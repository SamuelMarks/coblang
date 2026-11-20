//! Category-by-category ISO COBOL `MOVE` engine with overlap-safe execution and `ALL` tiling.

use core::ops::Range;

use coblang_common::{CoblangError, DigitCount, Result, ScaleFactor};
use coblang_decimal::charset::{Codepage, UnmappedCharPolicy, transcode_bytes};
use coblang_decimal::{
    BinaryByteOrder, BinaryLayout, BinarySignMode, BinaryTruncMode, BlankZeroMode, Decimal64Bid,
    Decimal128Bid, FixedDecimal, FloatBinary128, HighOrderPolicy, OverpunchDialect, PackedKind,
    PackedLayout, PackedSignPolicy, RoundingMode, SignPlacement, SizeErrorStatus, decode_binary,
    decode_comp1_ne, decode_comp2_ne, decode_packed, decode_zoned, encode_binary, encode_comp1_ne,
    encode_comp2_ne, encode_packed, encode_zoned, is_valid_packed_bcd, is_valid_zoned_numeric,
};
use coblang_hir::{
    BinaryEndianness, BlankZeroSpec, Comp6Mode, DataCategory, FloatBinaryWidth, FloatDecimalWidth,
    HirDisplayStyle, HirFigurative, HirType, JustifiedSpec, NumericSignSpec, Signedness,
    TruncationPolicy,
};

use crate::de_edit::de_edit_to_decimal;
use crate::edit_alpha::{
    ascii_to_national_utf16le, format_alphanumeric, format_alphanumeric_edited, format_national,
    format_national_edited,
};
use crate::edit_numeric::{NumericEditConfig, format_numeric_edited};
use crate::move_matrix::{Justification, pad_or_truncate_utf8_bytes_lossless};

/// Complete runtime/compile-time field descriptor driving [`execute_move`] and [`format_field_for_display`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveFieldDesc {
    /// Resolved HIR storage type (`Alphanumeric`, `ZonedDecimal`, `PackedDecimal`, `BinaryInt`, `NumericEdited`, `Group`, etc.).
    pub ty: HirType,
    /// Active zoned overpunch dialect (`AsciiGnu`, `Cobol85Standard`, `EbcdicIbm`, `MicroFocus`).
    pub overpunch: OverpunchDialect,
    /// Active decimal separator byte (`b'.'` or `b','`).
    pub decimal_char: u8,
    /// Active currency picture character (`'$'` by default).
    pub currency_symbol: char,
    /// Numeric `DISPLAY` digit-width style (`GnuPrettyWidth` vs `IsoPicWidth`).
    pub display_style: HirDisplayStyle,
}

impl MoveFieldDesc {
    /// Constructs a standard ASCII/`GnuCOBOL` `.`/`$` [`MoveFieldDesc`] for `ty`.
    ///
    /// # Arguments
    ///
    /// * `ty` - Resolved [`HirType`].
    #[must_use]
    pub const fn standard_gnu(ty: HirType) -> Self {
        Self {
            ty,
            overpunch: OverpunchDialect::AsciiGnu,
            decimal_char: b'.',
            currency_symbol: '$',
            display_style: HirDisplayStyle::GnuPrettyWidth,
        }
    }

    /// Returns the ISO COBOL [`DataCategory`] of this field.
    #[must_use]
    pub const fn category(&self) -> DataCategory {
        match &self.ty {
            HirType::Alphabetic { .. } => DataCategory::Alphabetic,
            HirType::Alphanumeric { .. } => DataCategory::Alphanumeric,
            HirType::AlphanumericEdited { .. } => DataCategory::AlphanumericEdited,
            HirType::ZonedDecimal { .. }
            | HirType::PackedDecimal { .. }
            | HirType::BinaryInt { .. }
            | HirType::FloatBinary { .. }
            | HirType::FloatDecimal { .. } => DataCategory::Numeric,
            HirType::NumericEdited { .. } => DataCategory::NumericEdited,
            HirType::National { .. } => DataCategory::National,
            HirType::NationalEdited { .. } => DataCategory::NationalEdited,
            HirType::Utf8 { .. } => DataCategory::Utf8,
            HirType::Bit { .. } => DataCategory::BooleanBit,
            HirType::Pointer | HirType::ProgramPointer => DataCategory::Pointer,
            HirType::Index => DataCategory::Index,
            HirType::ObjectRef { .. } => DataCategory::ObjectReference,
            HirType::Group { .. } => DataCategory::Group,
        }
    }

    /// Returns the grouping separator byte (`b','` when `decimal_char == b'.'`, else `b'.'`).
    #[must_use]
    pub const fn group_sep_char(&self) -> u8 {
        if self.decimal_char == b',' {
            b'.'
        } else {
            b','
        }
    }
}

/// Source value supplied to [`execute_move`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveSource<'a> {
    /// Stored COBOL field bytes paired with its [`MoveFieldDesc`].
    Field {
        /// Raw source storage bytes.
        bytes: &'a [u8],
        /// Source field descriptor.
        desc: &'a MoveFieldDesc,
    },
    /// Pre-evaluated exact `FixedDecimal` literal or arithmetic result.
    Decimal(FixedDecimal),
    /// Raw alphanumeric/string literal bytes.
    AlphanumericLiteral(&'a [u8]),
    /// `NATIONAL` (`N"..."` / `NX"..."`) literal pre-normalized to UTF-16LE bytes.
    NationalLiteralUtf16Le(&'a [u8]),
    /// `UTF-8` (`U"..."` / `UX"..."`) literal UTF-8 bytes.
    Utf8Literal(&'a [u8]),
    /// Figurative constant (`ZERO`, `SPACE`, `HIGH-VALUE`, `LOW-VALUE`, `QUOTE`, `NULL`, `ALL "pat"`).
    Figurative(HirFigurative),
}

/// Executes an overlap-safe `MOVE` within a shared storage buffer (such as `REDEFINES` or reference-modified `Group` slices).
///
/// Always snapshots `buf[src_range]` before writing `buf[dst_range]` when ranges overlap.
///
/// # Arguments
///
/// * `buf` - Backing Working/Local/Linkage storage byte slice.
/// * `src_spec` - Source `(byte_range, descriptor)`.
/// * `dst_spec` - Destination `(byte_range, descriptor)`.
///
/// # Errors
///
/// Returns [`CoblangError::PictureCodec`] or [`CoblangError::DecimalDomain`] if ranges are out of bounds or codecs fail.
pub fn execute_move_overlapping(
    buf: &mut [u8],
    src_spec: (Range<usize>, &MoveFieldDesc),
    dst_spec: (Range<usize>, &MoveFieldDesc),
) -> Result<()> {
    let (src_range, src_desc) = src_spec;
    let (dst_range, dst_desc) = dst_spec;
    let src_copy = buf
        .get(src_range)
        .ok_or_else(|| CoblangError::PictureCodec {
            picture: String::from("MOVE"),
            message: String::from("source slice range out of bounds"),
        })?
        .to_vec();
    let dst_slice = buf
        .get_mut(dst_range)
        .ok_or_else(|| CoblangError::PictureCodec {
            picture: String::from("MOVE"),
            message: String::from("destination slice range out of bounds"),
        })?;
    execute_move(
        MoveSource::Field {
            bytes: &src_copy,
            desc: src_desc,
        },
        dst_slice,
        dst_desc,
    )
}

/// Executes a full ISO COBOL category-aware `MOVE` into `dst_bytes`.
///
/// # Arguments
///
/// * `src` - [`MoveSource`] operand.
/// * `dst_bytes` - Mutable destination storage slice.
/// * `dst_desc` - Destination [`MoveFieldDesc`].
///
/// # Errors
///
/// Returns [`CoblangError`] if source numeric decoding or destination encoding fails.
pub fn execute_move(
    src: MoveSource<'_>,
    dst_bytes: &mut [u8],
    dst_desc: &MoveFieldDesc,
) -> Result<()> {
    if let HirType::Group { .. } = &dst_desc.ty {
        move_into_group(src, dst_bytes, dst_desc.overpunch.space_byte());
        return Ok(());
    }

    // Group source moved to an elementary item:
    // ISO COBOL treats a group source as an alphanumeric item!
    if let MoveSource::Field { bytes, desc } = src
        && desc.category() == DataCategory::Group
    {
        return execute_move(MoveSource::AlphanumericLiteral(bytes), dst_bytes, dst_desc);
    }

    match &dst_desc.ty {
        HirType::Alphabetic { justified, .. } | HirType::Alphanumeric { justified, .. } => {
            move_to_alphanumeric(src, dst_bytes, *justified, dst_desc)
        }
        HirType::Utf8 { .. } => move_to_utf8(src, dst_bytes, dst_desc),
        HirType::AlphanumericEdited { edit_ops, .. } => {
            let raw = extract_alpha_source_bytes(src, dst_bytes.len(), dst_desc)?;
            let formatted = format_alphanumeric_edited(
                &raw,
                edit_ops,
                JustifiedSpec::Left,
                dst_desc.overpunch.space_byte(),
            );
            write_exact_or_prefix(dst_bytes, &formatted, dst_desc.overpunch.space_byte());
            Ok(())
        }
        HirType::National {
            char_len,
            justified,
        } => move_to_national(src, dst_bytes, *char_len as usize, *justified, dst_desc),
        HirType::NationalEdited { edit_ops, .. } => {
            let raw_u16 = extract_national_source_bytes(src, edit_ops.len(), dst_desc)?;
            let formatted = format_national_edited(&raw_u16, edit_ops);
            write_exact_or_prefix(dst_bytes, &formatted, 0);
            Ok(())
        }
        HirType::NumericEdited { .. }
        | HirType::ZonedDecimal { .. }
        | HirType::PackedDecimal { .. }
        | HirType::BinaryInt { .. }
        | HirType::FloatBinary { .. }
        | HirType::FloatDecimal { .. }
        | HirType::Index => {
            let dec = extract_decimal_source(src)?;
            encode_decimal_into_numeric_field(&dec, dst_bytes, dst_desc)
        }
        HirType::Bit { .. }
        | HirType::Pointer
        | HirType::ProgramPointer
        | HirType::ObjectRef { .. }
        | HirType::Group { .. } => move_raw_or_zero(src, dst_bytes),
    }
}

/// Decodes a numeric COBOL field (`ZonedDecimal`, `PackedDecimal`, `BinaryInt`, `FloatBinary`, `FloatDecimal`, `Index`) into `FixedDecimal`.
///
/// # Arguments
///
/// * `bytes` - Raw storage bytes of the field.
/// * `desc` - [`MoveFieldDesc`] describing the field.
///
/// # Errors
///
/// Returns [`CoblangError`] if the underlying storage codec detects malformed bytes or lengths.
pub fn decode_numeric_field(bytes: &[u8], desc: &MoveFieldDesc) -> Result<FixedDecimal> {
    match &desc.ty {
        HirType::ZonedDecimal {
            digits,
            scale,
            sign,
            blank_zero,
        } => {
            let layout = build_zoned_layout(*digits, *scale, *sign, *blank_zero, desc.overpunch);
            decode_zoned(bytes, layout)
        }
        HirType::PackedDecimal {
            digits,
            scale,
            sign,
            unsigned_comp6,
        } => {
            let layout = build_packed_layout(*digits, *scale, *sign, *unsigned_comp6);
            decode_packed(bytes, layout)
        }
        HirType::BinaryInt {
            byte_width,
            digits,
            scale,
            sign,
            endian,
            trunc,
        } => {
            let layout = build_binary_layout(*byte_width, *digits, *scale, *sign, *endian, *trunc);
            decode_binary(bytes, layout)
        }
        HirType::FloatBinary { .. } | HirType::FloatDecimal { .. } => {
            decode_float_field(bytes, &desc.ty)
        }
        HirType::Index => {
            let layout = BinaryLayout {
                byte_width: u8::try_from(bytes.len().clamp(1, 16)).unwrap_or(4),
                digits: DigitCount::ONE,
                scale: ScaleFactor::ZERO,
                sign: BinarySignMode::Signed,
                byte_order: BinaryByteOrder::NativeEndian,
                trunc: BinaryTruncMode::FullBinaryRange,
            };
            decode_binary(bytes, layout)
        }
        HirType::NumericEdited { .. } | HirType::Alphanumeric { .. } => {
            if desc.overpunch == OverpunchDialect::EbcdicIbm {
                let asc = transcode_bytes(
                    bytes,
                    Codepage::Cp1140,
                    Codepage::Iso8859_1,
                    UnmappedCharPolicy::Substitute('?'),
                )?;
                de_edit_to_decimal(&asc, desc.decimal_char, desc.currency_symbol)
            } else {
                de_edit_to_decimal(bytes, desc.decimal_char, desc.currency_symbol)
            }
        }
        HirType::National { .. } | HirType::NationalEdited { .. } => {
            let asc = transcode_bytes(
                bytes,
                Codepage::Utf16Le,
                Codepage::Iso8859_1,
                UnmappedCharPolicy::Substitute('?'),
            )?;
            de_edit_to_decimal(&asc, desc.decimal_char, desc.currency_symbol)
        }
        HirType::Utf8 { .. } => de_edit_to_decimal(bytes, desc.decimal_char, desc.currency_symbol),
        other => Err(CoblangError::PictureCodec {
            picture: format!("{other:?}"),
            message: String::from("cannot decode non-numeric field as FixedDecimal"),
        }),
    }
}

/// Decodes a `FloatBinary` or `FloatDecimal` field into a [`FixedDecimal`].
///
/// # Arguments
///
/// * `bytes` - Raw IEEE-754 storage bytes.
/// * `ty` - Floating-point [`HirType`].
fn decode_float_field(bytes: &[u8], ty: &HirType) -> Result<FixedDecimal> {
    match ty {
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits32,
        } => {
            let arr: [u8; 4] = bytes.try_into().map_err(|_| CoblangError::DecimalDomain {
                operation: "decode_numeric_field(COMP-1)",
                message: format!("expected 4 bytes, found {}", bytes.len()),
            })?;
            Ok(decode_comp1_ne(arr, 18, RoundingMode::NearestEven)?.0)
        }
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits64,
        } => {
            let arr: [u8; 8] = bytes.try_into().map_err(|_| CoblangError::DecimalDomain {
                operation: "decode_numeric_field(COMP-2)",
                message: format!("expected 8 bytes, found {}", bytes.len()),
            })?;
            Ok(decode_comp2_ne(arr, 18, RoundingMode::NearestEven)?.0)
        }
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits128,
        } => {
            let arr: [u8; 16] = bytes.try_into().map_err(|_| CoblangError::DecimalDomain {
                operation: "decode_numeric_field(FLOAT-EXTENDED)",
                message: format!("expected 16 bytes, found {}", bytes.len()),
            })?;
            let f = FloatBinary128::from_ne_bytes(arr).to_f64()?;
            Ok(FixedDecimal::from_f64(f, 18, RoundingMode::NearestEven)?.0)
        }
        HirType::FloatDecimal {
            width: FloatDecimalWidth::Decimal64,
        } => {
            let arr: [u8; 8] = bytes.try_into().map_err(|_| CoblangError::DecimalDomain {
                operation: "decode_numeric_field(FLOAT-DECIMAL-16)",
                message: format!("expected 8 bytes, found {}", bytes.len()),
            })?;
            Decimal64Bid::from_ne_bytes(arr).to_fixed_decimal()
        }
        HirType::FloatDecimal {
            width: FloatDecimalWidth::Decimal128,
        } => {
            let arr: [u8; 16] = bytes.try_into().map_err(|_| CoblangError::DecimalDomain {
                operation: "decode_numeric_field(FLOAT-DECIMAL-34)",
                message: format!("expected 16 bytes, found {}", bytes.len()),
            })?;
            Decimal128Bid::from_ne_bytes(arr).to_fixed_decimal()
        }
        _ => unreachable!(),
    }
}

/// Formats a stored COBOL field into display output bytes for `DISPLAY`.
///
/// # Arguments
///
/// * `bytes` - Raw storage bytes of the field.
/// * `desc` - [`MoveFieldDesc`] of the field.
///
/// # Errors
///
/// Returns [`CoblangError`] if numeric decoding fails.
pub fn format_field_for_display(bytes: &[u8], desc: &MoveFieldDesc) -> Result<Vec<u8>> {
    match &desc.ty {
        HirType::ZonedDecimal {
            digits,
            scale,
            sign,
            blank_zero,
        } => {
            let sp = desc.overpunch.space_byte();
            if *blank_zero == BlankZeroSpec::BlankWhenZero && bytes.iter().all(|&b| b == sp) {
                return Ok(vec![b' '; bytes.len()]);
            }
            let val = decode_numeric_field(bytes, desc)?;
            let is_signed = !matches!(sign, NumericSignSpec::Unsigned);
            Ok(format_fixed_decimal_cobol_display(
                &val,
                digits.get(),
                scale.get(),
                is_signed,
                desc.decimal_char,
            ))
        }
        HirType::PackedDecimal {
            digits,
            scale,
            sign,
            ..
        } => {
            let val = decode_numeric_field(bytes, desc)?;
            Ok(format_fixed_decimal_cobol_display(
                &val,
                digits.get(),
                scale.get(),
                *sign == Signedness::Signed,
                desc.decimal_char,
            ))
        }
        HirType::BinaryInt {
            byte_width,
            digits,
            scale,
            sign,
            trunc,
            ..
        } => {
            let val = decode_numeric_field(bytes, desc)?;
            let (disp_digits, disp_scale) = match (desc.display_style, trunc) {
                (HirDisplayStyle::IsoPicWidth, _) | (_, TruncationPolicy::TruncateToPic) => {
                    (digits.get(), scale.get())
                }
                (HirDisplayStyle::GnuPrettyWidth, TruncationPolicy::FullBinaryRange) => {
                    let has_p_scaling = scale.get() < 0 || scale.get() > i16::from(digits.get());
                    let eff_scale = if has_p_scaling { 0 } else { scale.get() };
                    (hw_display_digits(*byte_width), eff_scale)
                }
            };
            Ok(format_fixed_decimal_cobol_display(
                &val,
                disp_digits,
                disp_scale,
                *sign == Signedness::Signed,
                desc.decimal_char,
            ))
        }
        HirType::FloatBinary { .. } | HirType::FloatDecimal { .. } => {
            let val = decode_numeric_field(bytes, desc)?;
            Ok(val.to_string().into_bytes())
        }
        HirType::Pointer | HirType::ProgramPointer => {
            let mut raw = [0_u8; 8];
            for (dst_b, &src_b) in raw.iter_mut().zip(bytes.iter()) {
                *dst_b = src_b;
            }
            let addr = u64::from_ne_bytes(raw);
            Ok(format!("0x{addr:016x}").into_bytes())
        }
        HirType::National { .. } | HirType::NationalEdited { .. } => transcode_bytes(
            bytes,
            Codepage::Utf16Le,
            Codepage::Utf8,
            UnmappedCharPolicy::Substitute('\u{FFFD}'),
        ),
        HirType::Utf8 { .. } => Ok(bytes.to_vec()),
        _ => {
            if desc.overpunch == OverpunchDialect::EbcdicIbm {
                transcode_bytes(
                    bytes,
                    Codepage::Cp1140,
                    Codepage::Iso8859_1,
                    UnmappedCharPolicy::Substitute('?'),
                )
            } else {
                Ok(bytes.to_vec())
            }
        }
    }
}

/// Returns standard `GnuCOBOL` hardware display digit width for `COMP-5` / `-fnotrunc` binary integers.
///
/// # Arguments
///
/// * `byte_width` - Storage byte width (`1..=16`).
#[must_use]
const fn hw_display_digits(byte_width: u8) -> u8 {
    match byte_width {
        1 => 3,
        2 => 5,
        3 => 8,
        4 => 10,
        5..=7 => 18,
        8 => 20,
        _ => 38,
    }
}

/// Formats `val` using `GnuCOBOL` / ISO COBOL `DISPLAY` conventions (`+000`, `-123`, `.000128`, `091000`).
///
/// # Arguments
///
/// * `val` - Decoded `FixedDecimal`.
/// * `pic_digits` - Declared or hardware display digit width.
/// * `scale` - Scale factor (`-38..=38`).
/// * `is_signed` - Whether a leading `'+'` / `'-'` sign is emitted.
/// * `dec_ch` - Decimal point byte (`b'.'` or `b','`).
#[must_use]
fn format_fixed_decimal_cobol_display(
    val: &FixedDecimal,
    pic_digits: u8,
    scale: i16,
    is_signed: bool,
    dec_ch: u8,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::from(pic_digits) + 4);
    if is_signed {
        out.push(if val.is_negative() { b'-' } else { b'+' });
    }
    let raw = val.mantissa().magnitude().to_decimal_digits();
    let d_len = usize::from(pic_digits);
    let mut aligned = vec![b'0'; d_len];
    let copy_len = raw.len().min(d_len);
    let src_start = raw.len() - copy_len;
    let dst_start = d_len - copy_len;
    for (slot, &b) in aligned
        .get_mut(dst_start..)
        .unwrap_or(&mut [])
        .iter_mut()
        .zip(raw.get(src_start..).unwrap_or(&[]))
    {
        *slot = b;
    }

    if scale <= 0 {
        out.extend_from_slice(&aligned);
        let trailing_zeros = usize::try_from(-scale).unwrap_or(0);
        out.extend(core::iter::repeat_n(b'0', trailing_zeros));
        return out;
    }

    let frac_len = usize::try_from(scale).unwrap_or(0);
    if frac_len >= d_len {
        out.push(dec_ch);
        out.extend(core::iter::repeat_n(b'0', frac_len - d_len));
        out.extend_from_slice(&aligned);
    } else {
        let split = d_len - frac_len;
        out.extend_from_slice(aligned.get(..split).unwrap_or(&[]));
        out.push(dec_ch);
        out.extend_from_slice(aligned.get(split..).unwrap_or(&[]));
    }
    out
}

/// Moves `src` into a `Group` destination using alphanumeric left-justified padding or figurative fill.
///
/// # Arguments
///
/// * `src` - Source operand.
/// * `dst_bytes` - Group byte slice.
/// * `space_byte` - Space pad byte.
fn move_into_group(src: MoveSource<'_>, dst_bytes: &mut [u8], space_byte: u8) {
    match src {
        MoveSource::Figurative(fig) => fill_figurative(dst_bytes, &fig, space_byte),
        MoveSource::AlphanumericLiteral(lit)
        | MoveSource::NationalLiteralUtf16Le(lit)
        | MoveSource::Utf8Literal(lit) => {
            let formatted =
                format_alphanumeric(lit, dst_bytes.len(), JustifiedSpec::Left, space_byte);
            dst_bytes.copy_from_slice(&formatted);
        }
        MoveSource::Field { bytes, .. } => {
            let formatted =
                format_alphanumeric(bytes, dst_bytes.len(), JustifiedSpec::Left, space_byte);
            dst_bytes.copy_from_slice(&formatted);
        }
        MoveSource::Decimal(dec) => {
            let text = dec.abs().to_string();
            let formatted = format_alphanumeric(
                text.as_bytes(),
                dst_bytes.len(),
                JustifiedSpec::Left,
                space_byte,
            );
            dst_bytes.copy_from_slice(&formatted);
        }
    }
}

/// Moves `src` into a `USAGE UTF-8` destination with Unicode scalar-boundary truncation and `0x20` padding.
///
/// # Arguments
///
/// * `src` - Source operand.
/// * `dst_bytes` - Destination byte slice.
/// * `dst_desc` - Destination descriptor.
fn move_to_utf8(src: MoveSource<'_>, dst_bytes: &mut [u8], dst_desc: &MoveFieldDesc) -> Result<()> {
    if let MoveSource::Figurative(fig) = src {
        fill_figurative(dst_bytes, &fig, b' ');
        return Ok(());
    }
    let raw = match src {
        MoveSource::NationalLiteralUtf16Le(u16_le) => transcode_bytes(
            u16_le,
            Codepage::Utf16Le,
            Codepage::Utf8,
            UnmappedCharPolicy::StrictError,
        )?,
        MoveSource::Utf8Literal(u8_lit) => u8_lit.to_vec(),
        MoveSource::Field { bytes, desc }
            if matches!(
                desc.category(),
                DataCategory::National | DataCategory::NationalEdited
            ) =>
        {
            transcode_bytes(
                bytes,
                Codepage::Utf16Le,
                Codepage::Utf8,
                UnmappedCharPolicy::StrictError,
            )?
        }
        MoveSource::Field { bytes, desc }
            if desc.overpunch == OverpunchDialect::EbcdicIbm
                && desc.category() != DataCategory::Numeric =>
        {
            transcode_bytes(
                bytes,
                Codepage::Cp1140,
                Codepage::Utf8,
                UnmappedCharPolicy::Substitute('\u{FFFD}'),
            )
            .unwrap_or_default()
        }
        _ => extract_alpha_source_bytes(src, dst_bytes.len(), dst_desc)?,
    };
    let formatted = pad_or_truncate_utf8_bytes_lossless(&raw, dst_bytes.len(), Justification::Left);
    dst_bytes.copy_from_slice(&formatted);
    Ok(())
}

/// Moves `src` into an `Alphabetic` or `Alphanumeric` destination.
///
/// # Arguments
///
/// * `src` - Source operand.
/// * `dst_bytes` - Destination byte slice.
/// * `justified` - Left or Right justification.
/// * `dst_desc` - Destination descriptor.
fn move_to_alphanumeric(
    src: MoveSource<'_>,
    dst_bytes: &mut [u8],
    justified: JustifiedSpec,
    dst_desc: &MoveFieldDesc,
) -> Result<()> {
    let sp = dst_desc.overpunch.space_byte();
    if let MoveSource::Figurative(fig) = src {
        fill_figurative(dst_bytes, &fig, sp);
        return Ok(());
    }
    let raw = extract_alpha_source_bytes(src, dst_bytes.len(), dst_desc)?;
    let formatted = format_alphanumeric(&raw, dst_bytes.len(), justified, sp);
    dst_bytes.copy_from_slice(&formatted);
    Ok(())
}

/// Extracts unedited alphanumeric byte representation from `src` (including integer-to-alphanumeric conversion).
///
/// # Arguments
///
/// * `src` - Source operand.
/// * `target_len` - Destination slot count (used for figurative tiling).
/// * `dst_desc` - Destination descriptor.
fn extract_alpha_source_bytes(
    src: MoveSource<'_>,
    target_len: usize,
    dst_desc: &MoveFieldDesc,
) -> Result<Vec<u8>> {
    let sp = dst_desc.overpunch.space_byte();
    match src {
        MoveSource::AlphanumericLiteral(lit) | MoveSource::Utf8Literal(lit) => Ok(lit.to_vec()),
        MoveSource::NationalLiteralUtf16Le(u16_le) => {
            let dst_cp = if dst_desc.overpunch == OverpunchDialect::EbcdicIbm {
                Codepage::Cp1140
            } else {
                Codepage::Iso8859_1
            };
            Ok(transcode_bytes(
                u16_le,
                Codepage::Utf16Le,
                dst_cp,
                UnmappedCharPolicy::Substitute('?'),
            )
            .unwrap_or_default())
        }
        MoveSource::Figurative(fig) => {
            let mut buf = vec![sp; target_len];
            fill_figurative(&mut buf, &fig, sp);
            Ok(buf)
        }
        MoveSource::Decimal(dec) => {
            let (int_dec, _) = dec.rescale(0, RoundingMode::Truncation)?;
            let mut digs = int_dec.mantissa().magnitude().to_decimal_digits();
            if dst_desc.overpunch == OverpunchDialect::EbcdicIbm {
                digs = transcode_bytes(
                    &digs,
                    Codepage::Iso8859_1,
                    Codepage::Cp1140,
                    UnmappedCharPolicy::Substitute('?'),
                )
                .unwrap_or(digs);
            }
            Ok(digs)
        }
        MoveSource::Field { bytes, desc } => match desc.category() {
            DataCategory::Numeric => {
                let dec = decode_numeric_field(bytes, desc)?;
                let (int_dec, _) = dec.rescale(0, RoundingMode::Truncation)?;
                let digits_width = numeric_pic_digits(&desc.ty);
                let raw_digits = int_dec.mantissa().magnitude().to_decimal_digits();
                let mut padded = vec![b'0'; digits_width];
                let copy_len = raw_digits.len().min(digits_width);
                let s_off = raw_digits.len() - copy_len;
                let d_off = digits_width - copy_len;
                for (slot, &b) in padded
                    .get_mut(d_off..)
                    .unwrap_or(&mut [])
                    .iter_mut()
                    .zip(raw_digits.get(s_off..).unwrap_or(&[]))
                {
                    *slot = b;
                }
                if dst_desc.overpunch == OverpunchDialect::EbcdicIbm {
                    padded = transcode_bytes(
                        &padded,
                        Codepage::Iso8859_1,
                        Codepage::Cp1140,
                        UnmappedCharPolicy::Substitute('?'),
                    )
                    .unwrap_or(padded);
                }
                Ok(padded)
            }
            DataCategory::National | DataCategory::NationalEdited => {
                let dst_cp = if dst_desc.overpunch == OverpunchDialect::EbcdicIbm {
                    Codepage::Cp1140
                } else {
                    Codepage::Iso8859_1
                };
                Ok(transcode_bytes(
                    bytes,
                    Codepage::Utf16Le,
                    dst_cp,
                    UnmappedCharPolicy::Substitute('?'),
                )
                .unwrap_or_default())
            }
            _ => Ok(bytes.to_vec()),
        },
    }
}

/// Moves `src` into a UTF-16LE `National` destination (`2 * char_len` bytes).
///
/// # Arguments
///
/// * `src` - Source operand.
/// * `dst_bytes` - Destination byte buffer.
/// * `char_len` - UTF-16 code unit count.
/// * `justified` - Alignment.
/// * `dst_desc` - Destination descriptor.
fn move_to_national(
    src: MoveSource<'_>,
    dst_bytes: &mut [u8],
    char_len: usize,
    justified: JustifiedSpec,
    dst_desc: &MoveFieldDesc,
) -> Result<()> {
    let raw_u16 = extract_national_source_bytes(src, char_len, dst_desc)?;
    let formatted = format_national(&raw_u16, char_len, justified);
    write_exact_or_prefix(dst_bytes, &formatted, 0);
    Ok(())
}

/// Extracts UTF-16LE code-unit bytes from `src` for `National` / `NationalEdited` destinations.
///
/// # Arguments
///
/// * `src` - Source operand.
/// * `char_len` - Target UTF-16 code unit count.
/// * `dst_desc` - Destination descriptor.
fn extract_national_source_bytes(
    src: MoveSource<'_>,
    char_len: usize,
    dst_desc: &MoveFieldDesc,
) -> Result<Vec<u8>> {
    if let MoveSource::Figurative(fig) = src {
        return Ok(fill_national_figurative(&fig, char_len));
    }
    if let MoveSource::NationalLiteralUtf16Le(u16_le) = src {
        return Ok(u16_le.to_vec());
    }
    if let MoveSource::Utf8Literal(u8_lit) = src {
        return transcode_bytes(
            u8_lit,
            Codepage::Utf8,
            Codepage::Utf16Le,
            UnmappedCharPolicy::StrictError,
        );
    }
    if let MoveSource::AlphanumericLiteral(lit) = src
        && dst_desc.overpunch == OverpunchDialect::EbcdicIbm
    {
        return Ok(transcode_bytes(
            lit,
            Codepage::Cp1140,
            Codepage::Utf16Le,
            UnmappedCharPolicy::Substitute('?'),
        )
        .unwrap_or_default());
    }
    if let MoveSource::Field { bytes, desc } = src {
        if matches!(
            desc.category(),
            DataCategory::National | DataCategory::NationalEdited
        ) {
            return Ok(bytes.to_vec());
        }
        if desc.category() == DataCategory::Utf8 {
            return transcode_bytes(
                bytes,
                Codepage::Utf8,
                Codepage::Utf16Le,
                UnmappedCharPolicy::StrictError,
            );
        }
        if desc.overpunch == OverpunchDialect::EbcdicIbm && desc.category() != DataCategory::Numeric
        {
            return Ok(transcode_bytes(
                bytes,
                Codepage::Cp1140,
                Codepage::Utf16Le,
                UnmappedCharPolicy::Substitute('?'),
            )
            .unwrap_or_default());
        }
    }
    let ascii = extract_alpha_source_bytes(src, char_len, dst_desc)?;
    Ok(ascii_to_national_utf16le(
        &ascii,
        ascii.len(),
        JustifiedSpec::Left,
    ))
}

/// Expands a [`HirFigurative`] constant into `char_len` UTF-16LE code units (`2 * char_len` bytes).
///
/// # Arguments
///
/// * `fig` - Figurative constant (`HIGH-VALUE` expands to `U+FFFF`).
/// * `char_len` - Target UTF-16 code unit count.
#[must_use]
fn fill_national_figurative(fig: &HirFigurative, char_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(char_len.saturating_mul(2));
    match fig {
        HirFigurative::Zero => {
            for _ in 0..char_len {
                out.extend_from_slice(&[b'0', 0x00]);
            }
        }
        HirFigurative::Space => {
            for _ in 0..char_len {
                out.extend_from_slice(&[b' ', 0x00]);
            }
        }
        HirFigurative::HighValue => {
            out.resize(char_len.saturating_mul(2), 0xFF);
        }
        HirFigurative::LowValue | HirFigurative::Null => {
            out.resize(char_len.saturating_mul(2), 0x00);
        }
        HirFigurative::Quote => {
            for _ in 0..char_len {
                out.extend_from_slice(&[b'"', 0x00]);
            }
        }
        HirFigurative::All(pat) => {
            let pat_units: Vec<[u8; 2]> = if let Ok(s) = core::str::from_utf8(pat) {
                let mut u = Vec::new();
                let mut buf = [0_u16; 2];
                for ch in s.chars() {
                    for &code_unit in ch.encode_utf16(&mut buf).iter() {
                        u.push(code_unit.to_le_bytes());
                    }
                }
                u
            } else {
                pat.iter().map(|&b| [b, 0x00]).collect()
            };
            if pat_units.is_empty() {
                for _ in 0..char_len {
                    out.extend_from_slice(&[b' ', 0x00]);
                }
            } else {
                for idx in 0..char_len {
                    let pair = pat_units
                        .get(idx % pat_units.len())
                        .copied()
                        .unwrap_or([b' ', 0x00]);
                    out.extend_from_slice(&pair);
                }
            }
        }
    }
    out
}

/// Evaluates `src` into a `FixedDecimal` for numeric or numeric-edited destinations.
///
/// # Arguments
///
/// * `src` - Source operand.
fn extract_decimal_source(src: MoveSource<'_>) -> Result<FixedDecimal> {
    match src {
        MoveSource::Decimal(dec) => Ok(dec),
        MoveSource::Figurative(
            HirFigurative::Zero
            | HirFigurative::Space
            | HirFigurative::LowValue
            | HirFigurative::Null,
        ) => Ok(FixedDecimal::ZERO),
        MoveSource::Figurative(HirFigurative::All(pat)) => de_edit_to_decimal(&pat, b'.', '$'),
        MoveSource::Figurative(_) => Err(CoblangError::PictureCodec {
            picture: String::from("MOVE"),
            message: String::from("HIGH-VALUE / QUOTE figurative cannot be moved to numeric item"),
        }),
        MoveSource::AlphanumericLiteral(lit) | MoveSource::Utf8Literal(lit) => {
            de_edit_to_decimal(lit, b'.', '$')
        }
        MoveSource::NationalLiteralUtf16Le(u16_le) => {
            let asc = transcode_bytes(
                u16_le,
                Codepage::Utf16Le,
                Codepage::Iso8859_1,
                UnmappedCharPolicy::Substitute('?'),
            )?;
            de_edit_to_decimal(&asc, b'.', '$')
        }
        MoveSource::Field { bytes, desc } => decode_numeric_field(bytes, desc),
    }
}

/// Stores `dec` into a numeric or numeric-edited destination with explicit `mode` and `high_order` policies.
///
/// When `high_order == HighOrderPolicy::FlagOverflow` and overflow/truncation-prohibited occurs,
/// `dst_bytes` is left completely untouched per ISO COBOL `ON SIZE ERROR` semantics.
///
/// # Arguments
///
/// * `dec` - Source `FixedDecimal` result.
/// * `dst_bytes` - Mutable destination storage slice.
/// * `dst_desc` - Destination [`MoveFieldDesc`].
/// * `mode` - ISO 2023 [`RoundingMode`].
/// * `high_order` - Overflow vs modulo-truncation policy.
///
/// # Errors
///
/// Returns [`CoblangError`] if floating-point/BCD encoding fails on invalid bounds.
pub fn store_decimal_to_field(
    dec: &FixedDecimal,
    dst_bytes: &mut [u8],
    dst_desc: &MoveFieldDesc,
    mode: RoundingMode,
    high_order: HighOrderPolicy,
) -> Result<SizeErrorStatus> {
    if matches!(
        dst_desc.ty,
        HirType::FloatBinary { .. } | HirType::FloatDecimal { .. }
    ) {
        return store_decimal_to_float_field(dec, dst_bytes, &dst_desc.ty, mode);
    }
    let ((mut enc, status), pad_byte) =
        encode_decimal_payload(dec, dst_bytes.len(), dst_desc, mode, high_order)?;
    if status.is_error() && high_order == HighOrderPolicy::FlagOverflow {
        return Ok(status);
    }
    if matches!(dst_desc.ty, HirType::NumericEdited { .. })
        && dst_desc.overpunch == OverpunchDialect::EbcdicIbm
    {
        enc = transcode_bytes(
            &enc,
            Codepage::Iso8859_1,
            Codepage::Cp1140,
            UnmappedCharPolicy::Substitute('?'),
        )
        .unwrap_or(enc);
    }
    write_exact_or_prefix(dst_bytes, &enc, pad_byte);
    Ok(status)
}

/// Encodes `dec` into `(encoded_bytes, SizeErrorStatus)` and trailing pad byte for fixed-point/edited `dst_desc`.
///
/// # Arguments
///
/// * `dec` - Source fixed-point decimal value to encode.
/// * `dst_len` - Destination storage slice length in bytes.
/// * `dst_desc` - Destination field descriptor and `HirType` layout.
/// * `mode` - Rounding mode applied when fractional digits are truncated.
/// * `high_order` - High-order overflow truncation/flag policy.
fn encode_decimal_payload(
    dec: &FixedDecimal,
    dst_len: usize,
    dst_desc: &MoveFieldDesc,
    mode: RoundingMode,
    high_order: HighOrderPolicy,
) -> Result<((Vec<u8>, SizeErrorStatus), u8)> {
    let (res, pad_byte) = match &dst_desc.ty {
        HirType::ZonedDecimal {
            digits,
            scale,
            sign,
            blank_zero,
        } => (
            encode_zoned(
                dec,
                build_zoned_layout(*digits, *scale, *sign, *blank_zero, dst_desc.overpunch),
                mode,
                high_order,
            ),
            dst_desc.overpunch.zero_digit_byte(),
        ),
        HirType::PackedDecimal {
            digits,
            scale,
            sign,
            unsigned_comp6,
        } => (
            encode_packed(
                dec,
                build_packed_layout(*digits, *scale, *sign, *unsigned_comp6),
                mode,
                high_order,
            ),
            0,
        ),
        HirType::BinaryInt {
            byte_width,
            digits,
            scale,
            sign,
            endian,
            trunc,
        } => (
            encode_binary(
                dec,
                build_binary_layout(*byte_width, *digits, *scale, *sign, *endian, *trunc),
                mode,
                high_order,
            ),
            0,
        ),
        HirType::NumericEdited {
            digits,
            scale,
            sign_capability,
            edit_ops,
            blank_zero,
            ..
        } => (
            format_numeric_edited(
                dec,
                edit_ops,
                NumericEditConfig {
                    digits: *digits,
                    scale: *scale,
                    sign_capability: *sign_capability,
                    blank_zero: *blank_zero,
                    decimal_char: dst_desc.decimal_char,
                    group_sep_char: dst_desc.group_sep_char(),
                },
                mode,
                high_order,
            ),
            dst_desc.overpunch.space_byte(),
        ),
        _ => (
            encode_binary(
                dec,
                BinaryLayout {
                    byte_width: u8::try_from(dst_len.clamp(1, 16)).unwrap_or(4),
                    digits: DigitCount::ONE,
                    scale: ScaleFactor::ZERO,
                    sign: BinarySignMode::Signed,
                    byte_order: BinaryByteOrder::NativeEndian,
                    trunc: BinaryTruncMode::FullBinaryRange,
                },
                mode,
                high_order,
            ),
            0,
        ),
    };
    Ok((res?, pad_byte))
}

/// Encodes `dec` into a `FloatBinary` (`COMP-1`/`COMP-2`/`FLOAT-EXTENDED`) or `FloatDecimal` (`DECIMAL64`/`DECIMAL128`) field.
///
/// # Arguments
///
/// * `dec` - Source `FixedDecimal`.
/// * `dst_bytes` - Destination storage slice.
/// * `ty` - Target float [`HirType`].
/// * `mode` - Active [`RoundingMode`] for IEEE 754 decimal encoding.
fn store_decimal_to_float_field(
    dec: &FixedDecimal,
    dst_bytes: &mut [u8],
    ty: &HirType,
    mode: RoundingMode,
) -> Result<SizeErrorStatus> {
    match ty {
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits32,
        } => write_exact_or_prefix(dst_bytes, &encode_comp1_ne(dec), 0),
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits64,
        } => write_exact_or_prefix(dst_bytes, &encode_comp2_ne(dec), 0),
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits128,
        } => {
            let b128 = FloatBinary128::from_f64(dec.to_f64())?;
            write_exact_or_prefix(dst_bytes, &b128.to_ne_bytes(), 0);
        }
        HirType::FloatDecimal {
            width: FloatDecimalWidth::Decimal64,
        } => {
            let b64 = Decimal64Bid::from_fixed_decimal(dec, mode)?;
            write_exact_or_prefix(dst_bytes, &b64.to_ne_bytes(), 0);
        }
        _ => {
            let b128 = Decimal128Bid::from_fixed_decimal(dec, mode)?;
            write_exact_or_prefix(dst_bytes, &b128.to_ne_bytes(), 0);
        }
    }
    Ok(SizeErrorStatus::Exact)
}

/// Encodes explicit `VALUE -0` into a signed `ZonedDecimal` buffer (`'p'`, `'}'`, `0xD0`, or `'-'`).
///
/// # Arguments
///
/// * `dst_bytes` - Mutable zoned storage buffer.
/// * `desc` - Field descriptor.
pub fn encode_zoned_negative_zero(dst_bytes: &mut [u8], desc: &MoveFieldDesc) {
    let HirType::ZonedDecimal {
        sign, blank_zero, ..
    } = &desc.ty
    else {
        return;
    };
    if *blank_zero == BlankZeroSpec::BlankWhenZero {
        dst_bytes.fill(desc.overpunch.space_byte());
        return;
    }
    let zero_ch = desc.overpunch.zero_digit_byte();
    dst_bytes.fill(zero_ch);
    let neg_zero_overpunch = match desc.overpunch {
        OverpunchDialect::AsciiGnu | OverpunchDialect::MicroFocus => 0x70,
        OverpunchDialect::Cobol85Standard => b'}',
        OverpunchDialect::EbcdicIbm => 0xD0,
    };
    let (_, minus_ch) = desc.overpunch.separate_sign_bytes();
    match sign {
        NumericSignSpec::Unsigned => {}
        NumericSignSpec::EmbeddedTrailing => {
            if let Some(last) = dst_bytes.last_mut() {
                *last = neg_zero_overpunch;
            }
        }
        NumericSignSpec::EmbeddedLeading => {
            if let Some(first) = dst_bytes.first_mut() {
                *first = neg_zero_overpunch;
            }
        }
        NumericSignSpec::SeparateTrailing => {
            if let Some(last) = dst_bytes.last_mut() {
                *last = minus_ch;
            }
        }
        NumericSignSpec::SeparateLeading => {
            if let Some(first) = dst_bytes.first_mut() {
                *first = minus_ch;
            }
        }
    }
}

/// Returns `true` if `bytes` is a signed `ZonedDecimal` currently holding negative zero (`-0`, e.g. `'p'`).
///
/// # Arguments
///
/// * `bytes` - Stored field bytes.
/// * `desc` - Field descriptor.
#[must_use]
pub fn is_zoned_negative_zero(bytes: &[u8], desc: &MoveFieldDesc) -> bool {
    let HirType::ZonedDecimal { sign, .. } = &desc.ty else {
        return false;
    };
    let check_byte = match sign {
        NumericSignSpec::Unsigned => return false,
        NumericSignSpec::EmbeddedTrailing | NumericSignSpec::SeparateTrailing => {
            let Some(&b) = bytes.last() else {
                return false;
            };
            b
        }
        NumericSignSpec::EmbeddedLeading | NumericSignSpec::SeparateLeading => {
            let Some(&b) = bytes.first() else {
                return false;
            };
            b
        }
    };
    let Ok(val) = decode_numeric_field(bytes, desc) else {
        return false;
    };
    if !val.is_zero() {
        return false;
    }
    let (_, minus_ch) = desc.overpunch.separate_sign_bytes();
    check_byte == minus_ch || matches!(check_byte, 0x70 | b'}' | 0xD0)
}

/// Evaluates ISO COBOL `IS NUMERIC` across `ZonedDecimal`, `PackedDecimal`, `BinaryInt`, `Float*`, and `Alphanumeric` / `Group`.
///
/// # Arguments
///
/// * `bytes` - Raw stored field bytes.
/// * `desc` - Field descriptor.
/// * `packed_sign` - Dialect `COMP-3` sign validation rule.
#[must_use]
pub fn check_field_is_numeric(
    bytes: &[u8],
    desc: &MoveFieldDesc,
    packed_sign: PackedSignPolicy,
) -> bool {
    match &desc.ty {
        HirType::ZonedDecimal {
            digits,
            scale,
            sign,
            blank_zero,
        } => {
            let layout = build_zoned_layout(*digits, *scale, *sign, *blank_zero, desc.overpunch);
            is_valid_zoned_numeric(bytes, layout)
        }
        HirType::PackedDecimal {
            digits,
            scale,
            sign,
            unsigned_comp6,
        } => {
            let layout = build_packed_layout(*digits, *scale, *sign, *unsigned_comp6);
            is_valid_packed_bcd(bytes, layout, packed_sign)
        }
        HirType::BinaryInt { .. } | HirType::FloatBinary { .. } | HirType::FloatDecimal { .. } => {
            true
        }
        _ => {
            let zero_ch = desc.overpunch.zero_digit_byte();
            !bytes.is_empty() && bytes.iter().all(|&b| b >= zero_ch && b <= zero_ch + 9)
        }
    }
}

/// Encodes `dec` into a numeric destination (`ZonedDecimal`, `PackedDecimal`, `BinaryInt`, `FloatBinary`, `FloatDecimal`, `Index`).
///
/// # Arguments
///
/// * `dec` - Source `FixedDecimal`.
/// * `dst_bytes` - Destination storage slice.
/// * `dst_desc` - Destination descriptor.
fn encode_decimal_into_numeric_field(
    dec: &FixedDecimal,
    dst_bytes: &mut [u8],
    dst_desc: &MoveFieldDesc,
) -> Result<()> {
    let _ = store_decimal_to_field(
        dec,
        dst_bytes,
        dst_desc,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    Ok(())
}

/// Fills `dst_bytes` with a [`HirFigurative`] constant, tiling `ALL "pattern"` across the entire destination!
///
/// # Arguments
///
/// * `dst_bytes` - Mutable destination slice.
/// * `fig` - Figurative constant.
/// * `space_byte` - Active space byte (`0x20` ASCII, `0x40` EBCDIC).
fn fill_figurative(dst_bytes: &mut [u8], fig: &HirFigurative, space_byte: u8) {
    match fig {
        HirFigurative::Zero => dst_bytes.fill(if space_byte == 0x40 { 0xF0 } else { b'0' }),
        HirFigurative::Space => dst_bytes.fill(space_byte),
        HirFigurative::HighValue => dst_bytes.fill(0xFF),
        HirFigurative::LowValue | HirFigurative::Null => dst_bytes.fill(0x00),
        HirFigurative::Quote => dst_bytes.fill(if space_byte == 0x40 { 0x7F } else { b'"' }),
        HirFigurative::All(pat) => {
            if pat.is_empty() {
                dst_bytes.fill(space_byte);
            } else {
                for (idx, slot) in dst_bytes.iter_mut().enumerate() {
                    *slot = pat.get(idx % pat.len()).copied().unwrap_or(space_byte);
                }
            }
        }
    }
}

/// Copies raw pointer/bit bytes or zeroes `dst_bytes` for `Pointer` / `Bit` / `ObjectRef`.
///
/// # Arguments
///
/// * `src` - Source operand.
/// * `dst_bytes` - Destination slice.
fn move_raw_or_zero(src: MoveSource<'_>, dst_bytes: &mut [u8]) -> Result<()> {
    match src {
        MoveSource::Field { bytes, .. }
        | MoveSource::AlphanumericLiteral(bytes)
        | MoveSource::NationalLiteralUtf16Le(bytes)
        | MoveSource::Utf8Literal(bytes) => {
            write_exact_or_prefix(dst_bytes, bytes, 0);
        }
        MoveSource::Figurative(fig) => fill_figurative(dst_bytes, &fig, 0),
        MoveSource::Decimal(dec) => {
            let (int_d, _) = dec.rescale(0, RoundingMode::Truncation)?;
            let val = int_d.mantissa().to_i128().unwrap_or(0);
            let ne = val.to_ne_bytes();
            write_exact_or_prefix(dst_bytes, &ne, 0);
        }
    }
    Ok(())
}

/// Writes `src` into `dst`, filling any excess trailing bytes with `pad`.
///
/// # Arguments
///
/// * `dst` - Destination byte slice.
/// * `src` - Encoded source bytes.
/// * `pad` - Trailing fill byte if `dst.len() > src.len()`.
fn write_exact_or_prefix(dst: &mut [u8], src: &[u8], pad: u8) {
    dst.fill(pad);
    let n = dst.len().min(src.len());
    dst.get_mut(..n)
        .unwrap_or(&mut [])
        .copy_from_slice(src.get(..n).unwrap_or(&[]));
}

/// Returns declared `PICTURE` digit count of a numeric [`HirType`].
///
/// # Arguments
///
/// * `ty` - Numeric `HirType`.
#[must_use]
const fn numeric_pic_digits(ty: &HirType) -> usize {
    match ty {
        HirType::ZonedDecimal { digits, .. }
        | HirType::PackedDecimal { digits, .. }
        | HirType::BinaryInt { digits, .. } => digits.get() as usize,
        _ => 18,
    }
}

/// Maps `HirType::ZonedDecimal` fields into a [`coblang_decimal::ZonedLayout`].
///
/// # Arguments
///
/// * `digits` - Digit count.
/// * `scale` - Scale factor.
/// * `sign` - HIR sign spec.
/// * `blank_zero` - `BLANK WHEN ZERO` spec.
/// * `dialect` - Overpunch dialect.
#[must_use]
const fn build_zoned_layout(
    digits: DigitCount,
    scale: ScaleFactor,
    sign: NumericSignSpec,
    blank_zero: BlankZeroSpec,
    dialect: OverpunchDialect,
) -> coblang_decimal::ZonedLayout {
    let placement = match sign {
        NumericSignSpec::Unsigned => SignPlacement::Unsigned,
        NumericSignSpec::EmbeddedLeading => SignPlacement::EmbeddedLeading,
        NumericSignSpec::EmbeddedTrailing => SignPlacement::EmbeddedTrailing,
        NumericSignSpec::SeparateLeading => SignPlacement::SeparateLeading,
        NumericSignSpec::SeparateTrailing => SignPlacement::SeparateTrailing,
    };
    let bz = match blank_zero {
        BlankZeroSpec::RetainZero => BlankZeroMode::RetainZero,
        BlankZeroSpec::BlankWhenZero => BlankZeroMode::BlankWhenZero,
    };
    coblang_decimal::ZonedLayout {
        digits,
        scale,
        sign: placement,
        dialect,
        blank_zero: bz,
    }
}

/// Maps `HirType::PackedDecimal` fields into a [`PackedLayout`].
///
/// # Arguments
///
/// * `digits` - Digit count.
/// * `scale` - Scale factor.
/// * `sign` - Signedness.
/// * `comp6` - `StandardComp3` vs `UnsignedComp6`.
#[must_use]
const fn build_packed_layout(
    digits: DigitCount,
    scale: ScaleFactor,
    sign: Signedness,
    comp6: Comp6Mode,
) -> PackedLayout {
    let kind = match (comp6, sign) {
        (Comp6Mode::UnsignedComp6, _) => PackedKind::Comp6Unsigned,
        (Comp6Mode::StandardComp3, Signedness::Signed) => PackedKind::Comp3Signed,
        (Comp6Mode::StandardComp3, Signedness::Unsigned) => PackedKind::Comp3Unsigned,
    };
    PackedLayout {
        digits,
        scale,
        kind,
    }
}

/// Maps `HirType::BinaryInt` fields into a [`BinaryLayout`].
///
/// # Arguments
///
/// * `byte_width` - Storage width in bytes (`1..=16`).
/// * `digits` - Declared `PICTURE` digits.
/// * `scale` - Scale factor.
/// * `sign` - Signedness.
/// * `endian` - Endianness.
/// * `trunc` - Truncation policy.
#[must_use]
const fn build_binary_layout(
    byte_width: u8,
    digits: DigitCount,
    scale: ScaleFactor,
    sign: Signedness,
    endian: BinaryEndianness,
    trunc: TruncationPolicy,
) -> BinaryLayout {
    BinaryLayout {
        byte_width,
        digits,
        scale,
        sign: match sign {
            Signedness::Signed => BinarySignMode::Signed,
            Signedness::Unsigned => BinarySignMode::Unsigned,
        },
        byte_order: match endian {
            BinaryEndianness::BigEndian => BinaryByteOrder::BigEndian,
            BinaryEndianness::Native => BinaryByteOrder::NativeEndian,
        },
        trunc: match trunc {
            TruncationPolicy::TruncateToPic => BinaryTruncMode::TruncateToPic,
            TruncationPolicy::FullBinaryRange => BinaryTruncMode::FullBinaryRange,
        },
    }
}
