//! Compact binary serialization and deserialization for runtime `MoveFieldDesc` and `INITIALIZE` plans.

use coblang_common::{CoblangError, DigitCount, Result, ScaleFactor};
use coblang_decimal::{OverpunchDialect, PackedSignPolicy};
use coblang_hir::{
    BinaryEndianness, BlankZeroSpec, Comp6Mode, DataCategory, DynamicOdoPresence, FloatBinaryWidth,
    FloatDecimalWidth, HirDisplayStyle, HirSortDirection, HirType, JustifiedSpec, NumericSignSpec,
    PicEditOp, Signedness, TruncationPolicy,
};
use coblang_pic::{DefaultInitMode, FillerPolicy, InitCategoryMask, MoveFieldDesc};

/// Membership of an elementary field in `FILLER` vs named Data Division entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FillerMembership {
    /// Named Data Division item.
    NamedField,
    /// `FILLER` or unnamed Data Division item.
    FillerField,
}

/// Membership of an elementary field under a `REDEFINES` hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RedefinesMembership {
    /// Primary storage definition (participates in `INITIALIZE`).
    PrimaryStorage,
    /// Subordinate to or directly bearing a `REDEFINES` clause (excluded from `INITIALIZE`).
    RedefinesAlias,
}

/// Complete runtime field descriptor pairing [`MoveFieldDesc`] with packed `IS NUMERIC` dialect rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFieldSpec {
    /// `MOVE` / `DISPLAY` / `PICTURE` descriptor.
    pub desc: MoveFieldDesc,
    /// `COMP-3` sign-nibble `IS NUMERIC` rule (`Gnu` vs `Ibm`).
    pub packed_sign_rule: PackedSignPolicy,
}

/// Compact repeating descriptor entry inside a serialized `INITIALIZE` plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInitEntry {
    /// Relative byte offset of element `(1, 1, ...)` from the target `INITIALIZE` base pointer.
    pub offset: u32,
    /// Multi-dimensional `(occurrence_count, element_byte_stride)` pairs (`empty` for scalar leaf).
    pub dims: Vec<(u32, u32)>,
    /// Elementary item byte length (`>= 1`).
    pub byte_len: u32,
    /// Elementary field descriptor.
    pub desc: MoveFieldDesc,
    /// Whether this item is `FILLER`.
    pub filler: FillerMembership,
    /// Whether this item is under `REDEFINES`.
    pub redefines: RedefinesMembership,
    /// Optional pre-encoded Data Division `VALUE` bytes for `INITIALIZE ... TO VALUE`.
    pub value_bytes: Option<Vec<u8>>,
}

/// Decoded `INITIALIZE` plan header + flattened entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedInitPlan {
    /// Whether `FILLER` items participate.
    pub filler_policy: FillerPolicy,
    /// Categories restoring Data Division `VALUE` clauses.
    pub to_value_mask: InitCategoryMask,
    /// Pointer/bit default initialization policy.
    pub default_mode: DefaultInitMode,
    /// Repeating elementary field entries.
    pub entries: Vec<RuntimeInitEntry>,
}

/// Cursor over a compact binary descriptor slice.
struct ByteReader<'a> {
    /// Backing descriptor slice.
    buf: &'a [u8],
    /// Current read index.
    pos: usize,
}

impl<'a> ByteReader<'a> {
    /// Creates a new [`ByteReader`] at offset `0`.
    ///
    /// # Arguments
    ///
    /// * `buf` - Descriptor byte slice.
    const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Reads one byte or returns [`CoblangError::PictureCodec`].
    fn read_u8(&mut self) -> Result<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or_else(|| CoblangError::PictureCodec {
                picture: String::from("<runtime-desc>"),
                message: String::from("truncated runtime field descriptor"),
            })?;
        self.pos += 1;
        Ok(b)
    }

    /// Reads a little-endian `u16`.
    fn read_u16(&mut self) -> Result<u16> {
        let lo = self.read_u8()?;
        let hi = self.read_u8()?;
        Ok(u16::from_le_bytes([lo, hi]))
    }

    /// Reads a little-endian `i16`.
    fn read_i16(&mut self) -> Result<i16> {
        let lo = self.read_u8()?;
        let hi = self.read_u8()?;
        Ok(i16::from_le_bytes([lo, hi]))
    }

    /// Reads a little-endian `u32`.
    fn read_u32(&mut self) -> Result<u32> {
        let b0 = self.read_u8()?;
        let b1 = self.read_u8()?;
        let b2 = self.read_u8()?;
        let b3 = self.read_u8()?;
        Ok(u32::from_le_bytes([b0, b1, b2, b3]))
    }

    /// Reads a UTF-32 scalar `char`.
    fn read_char(&mut self) -> Result<char> {
        let raw = self.read_u32()?;
        char::from_u32(raw).ok_or_else(|| CoblangError::PictureCodec {
            picture: String::from("<runtime-desc>"),
            message: format!("invalid char scalar 0x{raw:X} in descriptor"),
        })
    }

    /// Reads an exact subslice of length `len`.
    ///
    /// # Arguments
    ///
    /// * `len` - Number of bytes to consume.
    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.pos.saturating_add(len);
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or_else(|| CoblangError::PictureCodec {
                picture: String::from("<runtime-desc>"),
                message: String::from("truncated byte payload in runtime descriptor"),
            })?;
        self.pos = end;
        Ok(slice)
    }
}

/// Serializes a [`RuntimeFieldSpec`] into a compact binary byte vector.
///
/// # Arguments
///
/// * `spec` - Field specification to encode.
#[must_use]
pub fn encode_field_spec(spec: &RuntimeFieldSpec) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    write_field_spec_into(&mut out, spec);
    out
}

/// Decodes a [`RuntimeFieldSpec`] from `bytes`.
///
/// # Arguments
///
/// * `bytes` - Compact descriptor bytes produced by [`encode_field_spec`].
///
/// # Errors
///
/// Returns [`CoblangError::PictureCodec`] or [`CoblangError::InvalidDomainValue`] if `bytes` is truncated or malformed.
pub fn decode_field_spec(bytes: &[u8]) -> Result<RuntimeFieldSpec> {
    let mut rdr = ByteReader::new(bytes);
    read_field_spec_from(&mut rdr)
}

/// Serializes a [`DecodedInitPlan`] into a compact binary byte vector.
///
/// # Arguments
///
/// * `plan` - Flattened `INITIALIZE` plan to encode.
#[must_use]
pub fn encode_init_plan(plan: &DecodedInitPlan) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + plan.entries.len() * 24);
    let flags = (match plan.filler_policy {
        FillerPolicy::SkipFiller => 0_u8,
        FillerPolicy::IncludeFiller => 1_u8,
    }) | (match plan.default_mode {
        DefaultInitMode::StandardCobol => 0_u8,
        DefaultInitMode::IncludePointersAndBit => 2_u8,
    });
    out.push(flags);
    out.extend_from_slice(&plan.to_value_mask.bits().to_le_bytes());
    let count_u32 = u32::try_from(plan.entries.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&count_u32.to_le_bytes());

    for entry in &plan.entries {
        out.extend_from_slice(&entry.offset.to_le_bytes());
        let dim_n = u8::try_from(entry.dims.len()).unwrap_or(u8::MAX);
        out.push(dim_n);
        for &(cnt, stride) in entry.dims.iter().take(usize::from(dim_n)) {
            out.extend_from_slice(&cnt.to_le_bytes());
            out.extend_from_slice(&stride.to_le_bytes());
        }
        out.extend_from_slice(&entry.byte_len.to_le_bytes());
        let e_flags = (match entry.filler {
            FillerMembership::NamedField => 0_u8,
            FillerMembership::FillerField => 1_u8,
        }) | (match entry.redefines {
            RedefinesMembership::PrimaryStorage => 0_u8,
            RedefinesMembership::RedefinesAlias => 2_u8,
        }) | if entry.value_bytes.is_some() {
            4_u8
        } else {
            0_u8
        };
        out.push(e_flags);
        write_field_spec_into(
            &mut out,
            &RuntimeFieldSpec {
                desc: entry.desc.clone(),
                packed_sign_rule: PackedSignPolicy::Gnu,
            },
        );
        if let Some(vb) = &entry.value_bytes {
            let vlen = u32::try_from(vb.len()).unwrap_or(0);
            out.extend_from_slice(&vlen.to_le_bytes());
            out.extend_from_slice(vb);
        }
    }
    out
}

/// Decodes a [`DecodedInitPlan`] from `bytes`.
///
/// # Arguments
///
/// * `bytes` - Serialized `INITIALIZE` plan bytes produced by [`encode_init_plan`].
///
/// # Errors
///
/// Returns [`CoblangError::PictureCodec`] if `bytes` is truncated or malformed.
pub fn decode_init_plan(bytes: &[u8]) -> Result<DecodedInitPlan> {
    let mut rdr = ByteReader::new(bytes);
    let flags = rdr.read_u8()?;
    let filler_policy = if (flags & 1) != 0 {
        FillerPolicy::IncludeFiller
    } else {
        FillerPolicy::SkipFiller
    };
    let default_mode = if (flags & 2) != 0 {
        DefaultInitMode::IncludePointersAndBit
    } else {
        DefaultInitMode::StandardCobol
    };
    let mask_bits = rdr.read_u16()?;
    let entry_count = rdr.read_u32()? as usize;
    let mut entries = Vec::with_capacity(entry_count.min(1024));
    for _ in 0..entry_count {
        let offset = rdr.read_u32()?;
        let dim_n = usize::from(rdr.read_u8()?);
        let mut dims = Vec::with_capacity(dim_n.min(16));
        for _ in 0..dim_n {
            let cnt = rdr.read_u32()?;
            let stride = rdr.read_u32()?;
            dims.push((cnt, stride));
        }
        let byte_len = rdr.read_u32()?;
        let e_flags = rdr.read_u8()?;
        let filler = if (e_flags & 1) != 0 {
            FillerMembership::FillerField
        } else {
            FillerMembership::NamedField
        };
        let redefines = if (e_flags & 2) != 0 {
            RedefinesMembership::RedefinesAlias
        } else {
            RedefinesMembership::PrimaryStorage
        };
        let fspec = read_field_spec_from(&mut rdr)?;
        let value_bytes = if (e_flags & 4) != 0 {
            let vlen = rdr.read_u32()? as usize;
            Some(rdr.read_bytes(vlen)?.to_vec())
        } else {
            None
        };
        entries.push(RuntimeInitEntry {
            offset,
            dims,
            byte_len,
            desc: fspec.desc,
            filler,
            redefines,
            value_bytes,
        });
    }
    Ok(DecodedInitPlan {
        filler_policy,
        to_value_mask: InitCategoryMask::from_bits(mask_bits),
        default_mode,
        entries,
    })
}

/// Appends a serialized [`RuntimeFieldSpec`] into `out`.
///
/// # Arguments
///
/// * `out` - Output byte buffer.
/// * `spec` - Field specification.
fn write_field_spec_into(out: &mut Vec<u8>, spec: &RuntimeFieldSpec) {
    let ov_bits = match spec.desc.overpunch {
        OverpunchDialect::AsciiGnu => 0_u8,
        OverpunchDialect::Cobol85Standard => 1_u8,
        OverpunchDialect::EbcdicIbm => 2_u8,
        OverpunchDialect::MicroFocus => 3_u8,
    };
    let ds_bit = match spec.desc.display_style {
        HirDisplayStyle::GnuPrettyWidth => 0_u8,
        HirDisplayStyle::IsoPicWidth => 4_u8,
    };
    let ps_bit = match spec.packed_sign_rule {
        PackedSignPolicy::Gnu => 0_u8,
        PackedSignPolicy::Ibm => 8_u8,
    };
    out.push(ov_bits | ds_bit | ps_bit);
    out.push(spec.desc.decimal_char);
    out.extend_from_slice(&(spec.desc.currency_symbol as u32).to_le_bytes());
    write_hir_type(out, &spec.desc.ty);
}

/// Reads a [`RuntimeFieldSpec`] from `rdr`.
///
/// # Arguments
///
/// * `rdr` - Active [`ByteReader`].
fn read_field_spec_from(rdr: &mut ByteReader<'_>) -> Result<RuntimeFieldSpec> {
    let hdr = rdr.read_u8()?;
    let overpunch = match hdr & 0x03 {
        0 => OverpunchDialect::AsciiGnu,
        1 => OverpunchDialect::Cobol85Standard,
        2 => OverpunchDialect::EbcdicIbm,
        _ => OverpunchDialect::MicroFocus,
    };
    let display_style = if (hdr & 4) != 0 {
        HirDisplayStyle::IsoPicWidth
    } else {
        HirDisplayStyle::GnuPrettyWidth
    };
    let packed_sign_rule = if (hdr & 8) != 0 {
        PackedSignPolicy::Ibm
    } else {
        PackedSignPolicy::Gnu
    };
    let decimal_char = rdr.read_u8()?;
    let currency_symbol = rdr.read_char()?;
    let ty = read_hir_type(rdr)?;
    Ok(RuntimeFieldSpec {
        desc: MoveFieldDesc {
            ty,
            overpunch,
            decimal_char,
            currency_symbol,
            display_style,
        },
        packed_sign_rule,
    })
}

/// Encodes `JustifiedSpec` as `0` (`Left`) or `1` (`Right`).
///
/// # Arguments
///
/// * `j` - Justification spec.
const fn encode_just(j: JustifiedSpec) -> u8 {
    match j {
        JustifiedSpec::Left => 0,
        JustifiedSpec::Right => 1,
    }
}

/// Decodes `JustifiedSpec` from `b`.
///
/// # Arguments
///
/// * `b` - Encoded byte.
const fn decode_just(b: u8) -> JustifiedSpec {
    if b == 0 {
        JustifiedSpec::Left
    } else {
        JustifiedSpec::Right
    }
}

/// Encodes `BlankZeroSpec` as `0` or `1`.
///
/// # Arguments
///
/// * `bz` - Blank-when-zero spec.
const fn encode_bz(bz: BlankZeroSpec) -> u8 {
    match bz {
        BlankZeroSpec::RetainZero => 0,
        BlankZeroSpec::BlankWhenZero => 1,
    }
}

/// Decodes `BlankZeroSpec` from `b`.
///
/// # Arguments
///
/// * `b` - Encoded byte.
const fn decode_bz(b: u8) -> BlankZeroSpec {
    if b == 0 {
        BlankZeroSpec::RetainZero
    } else {
        BlankZeroSpec::BlankWhenZero
    }
}

/// Encodes `Signedness` as `0` (`Unsigned`) or `1` (`Signed`).
///
/// # Arguments
///
/// * `s` - Signedness.
const fn encode_signed(s: Signedness) -> u8 {
    match s {
        Signedness::Unsigned => 0,
        Signedness::Signed => 1,
    }
}

/// Decodes `Signedness` from `b`.
///
/// # Arguments
///
/// * `b` - Encoded byte.
const fn decode_signed(b: u8) -> Signedness {
    if b == 0 {
        Signedness::Unsigned
    } else {
        Signedness::Signed
    }
}

/// Appends serialized [`HirType`] bytes to `out`.
///
/// # Arguments
///
/// * `out` - Output buffer.
/// * `ty` - HIR type to serialize.
fn write_hir_type(out: &mut Vec<u8>, ty: &HirType) {
    match ty {
        HirType::Alphabetic {
            byte_len,
            justified,
        } => {
            out.push(0);
            out.extend_from_slice(&byte_len.to_le_bytes());
            out.push(encode_just(*justified));
        }
        HirType::Alphanumeric {
            byte_len,
            justified,
        } => {
            out.push(1);
            out.extend_from_slice(&byte_len.to_le_bytes());
            out.push(encode_just(*justified));
        }
        HirType::AlphanumericEdited { byte_len, edit_ops } => {
            out.push(2);
            out.extend_from_slice(&byte_len.to_le_bytes());
            write_edit_ops(out, edit_ops);
        }
        HirType::ZonedDecimal { .. }
        | HirType::PackedDecimal { .. }
        | HirType::BinaryInt { .. }
        | HirType::NumericEdited { .. }
        | HirType::FloatBinary { .. }
        | HirType::FloatDecimal { .. } => write_numeric_hir_type(out, ty),
        HirType::Pointer => out.push(9),
        HirType::ProgramPointer => out.push(10),
        HirType::Index => out.push(11),
        HirType::Group {
            static_byte_len,
            odo_presence,
        } => {
            out.push(12);
            out.extend_from_slice(&static_byte_len.to_le_bytes());
            out.push(match odo_presence {
                DynamicOdoPresence::StaticSize => 0,
                DynamicOdoPresence::HasDynamicOdo => 1,
            });
        }
        HirType::National {
            char_len,
            justified,
        } => {
            out.push(13);
            out.extend_from_slice(&char_len.to_le_bytes());
            out.push(encode_just(*justified));
        }
        HirType::NationalEdited { char_len, edit_ops } => {
            out.push(14);
            out.extend_from_slice(&char_len.to_le_bytes());
            write_edit_ops(out, edit_ops);
        }
        HirType::Utf8 { byte_len } => {
            out.push(15);
            out.extend_from_slice(&byte_len.to_le_bytes());
        }
        HirType::Bit { bit_len } => {
            out.push(16);
            out.extend_from_slice(&bit_len.to_le_bytes());
        }
        HirType::ObjectRef { class_sym } => {
            out.push(17);
            let (present, raw) = match class_sym {
                Some(s) => (1_u8, s.as_u32()),
                None => (0_u8, 0_u32),
            };
            out.push(present);
            out.extend_from_slice(&raw.to_le_bytes());
        }
    }
}

/// Appends serialized numeric or numeric-edited [`HirType`] bytes (`tags 3..=8`) to `out`.
///
/// # Arguments
///
/// * `out` - Output buffer.
/// * `ty` - Numeric or numeric-edited [`HirType`].
fn write_numeric_hir_type(out: &mut Vec<u8>, ty: &HirType) {
    match ty {
        HirType::ZonedDecimal {
            digits,
            scale,
            sign,
            blank_zero,
        } => {
            out.push(3);
            out.push(digits.get());
            out.extend_from_slice(&scale.get().to_le_bytes());
            out.push(match sign {
                NumericSignSpec::Unsigned => 0,
                NumericSignSpec::EmbeddedTrailing => 1,
                NumericSignSpec::EmbeddedLeading => 2,
                NumericSignSpec::SeparateTrailing => 3,
                NumericSignSpec::SeparateLeading => 4,
            });
            out.push(encode_bz(*blank_zero));
        }
        HirType::PackedDecimal {
            digits,
            scale,
            sign,
            unsigned_comp6,
        } => {
            out.push(4);
            out.push(digits.get());
            out.extend_from_slice(&scale.get().to_le_bytes());
            out.push(encode_signed(*sign));
            out.push(match unsigned_comp6 {
                Comp6Mode::StandardComp3 => 0,
                Comp6Mode::UnsignedComp6 => 1,
            });
        }
        HirType::BinaryInt {
            byte_width,
            digits,
            scale,
            sign,
            endian,
            trunc,
        } => {
            out.push(5);
            out.push(*byte_width);
            out.push(digits.get());
            out.extend_from_slice(&scale.get().to_le_bytes());
            let flags = encode_signed(*sign)
                | (match endian {
                    BinaryEndianness::Native => 0,
                    BinaryEndianness::BigEndian => 2,
                })
                | (match trunc {
                    TruncationPolicy::TruncateToPic => 0,
                    TruncationPolicy::FullBinaryRange => 4,
                });
            out.push(flags);
        }
        HirType::NumericEdited {
            byte_len,
            digits,
            scale,
            sign_capability,
            edit_ops,
            blank_zero,
        } => {
            out.push(6);
            out.extend_from_slice(&byte_len.to_le_bytes());
            out.push(digits.get());
            out.extend_from_slice(&scale.get().to_le_bytes());
            out.push(encode_signed(*sign_capability));
            out.push(encode_bz(*blank_zero));
            write_edit_ops(out, edit_ops);
        }
        HirType::FloatBinary { width } => {
            out.push(7);
            out.push(match width {
                FloatBinaryWidth::Bits32 => 0,
                FloatBinaryWidth::Bits64 => 1,
                FloatBinaryWidth::Bits128 => 2,
            });
        }
        HirType::FloatDecimal {
            width: FloatDecimalWidth::Decimal64,
        } => {
            out.push(8);
            out.push(0);
        }
        _ => {
            out.push(8);
            out.push(1);
        }
    }
}

/// Reads a serialized [`HirType`] from `rdr`.
///
/// # Arguments
///
/// * `rdr` - Active [`ByteReader`].
fn read_hir_type(rdr: &mut ByteReader<'_>) -> Result<HirType> {
    let tag = rdr.read_u8()?;
    match tag {
        0 => Ok(HirType::Alphabetic {
            byte_len: rdr.read_u32()?,
            justified: decode_just(rdr.read_u8()?),
        }),
        1 => Ok(HirType::Alphanumeric {
            byte_len: rdr.read_u32()?,
            justified: decode_just(rdr.read_u8()?),
        }),
        2 => Ok(HirType::AlphanumericEdited {
            byte_len: rdr.read_u32()?,
            edit_ops: read_edit_ops(rdr)?,
        }),
        3..=8 => read_numeric_hir_type(tag, rdr),
        9 => Ok(HirType::Pointer),
        10 => Ok(HirType::ProgramPointer),
        11 => Ok(HirType::Index),
        12 => Ok(HirType::Group {
            static_byte_len: rdr.read_u32()?,
            odo_presence: if rdr.read_u8()? == 0 {
                DynamicOdoPresence::StaticSize
            } else {
                DynamicOdoPresence::HasDynamicOdo
            },
        }),
        13 => Ok(HirType::National {
            char_len: rdr.read_u32()?,
            justified: decode_just(rdr.read_u8()?),
        }),
        14 => Ok(HirType::NationalEdited {
            char_len: rdr.read_u32()?,
            edit_ops: read_edit_ops(rdr)?,
        }),
        15 => Ok(HirType::Utf8 {
            byte_len: rdr.read_u32()?,
        }),
        16 => Ok(HirType::Bit {
            bit_len: rdr.read_u32()?,
        }),
        17 => {
            let present = rdr.read_u8()?;
            let raw = rdr.read_u32()?;
            Ok(HirType::ObjectRef {
                class_sym: if present != 0 {
                    Some(coblang_common::SymbolId::new(raw))
                } else {
                    None
                },
            })
        }
        other => Err(CoblangError::PictureCodec {
            picture: String::from("<runtime-desc>"),
            message: format!("unknown HIR type tag {other} in runtime descriptor"),
        }),
    }
}

/// Reads a serialized numeric or numeric-edited [`HirType`] (`tag` in `3..=8`).
///
/// # Arguments
///
/// * `tag` - Verified numeric type tag (`3..=8`).
/// * `rdr` - Active [`ByteReader`].
fn read_numeric_hir_type(tag: u8, rdr: &mut ByteReader<'_>) -> Result<HirType> {
    match tag {
        3 => {
            let digits = DigitCount::try_new(rdr.read_u8()?)?;
            let scale = ScaleFactor::try_new(rdr.read_i16()?)?;
            let sign = match rdr.read_u8()? {
                0 => NumericSignSpec::Unsigned,
                1 => NumericSignSpec::EmbeddedTrailing,
                2 => NumericSignSpec::EmbeddedLeading,
                3 => NumericSignSpec::SeparateTrailing,
                _ => NumericSignSpec::SeparateLeading,
            };
            let blank_zero = decode_bz(rdr.read_u8()?);
            Ok(HirType::ZonedDecimal {
                digits,
                scale,
                sign,
                blank_zero,
            })
        }
        4 => {
            let digits = DigitCount::try_new(rdr.read_u8()?)?;
            let scale = ScaleFactor::try_new(rdr.read_i16()?)?;
            let sign = decode_signed(rdr.read_u8()?);
            let unsigned_comp6 = if rdr.read_u8()? == 0 {
                Comp6Mode::StandardComp3
            } else {
                Comp6Mode::UnsignedComp6
            };
            Ok(HirType::PackedDecimal {
                digits,
                scale,
                sign,
                unsigned_comp6,
            })
        }
        5 => {
            let byte_width = rdr.read_u8()?;
            let digits = DigitCount::try_new(rdr.read_u8()?)?;
            let scale = ScaleFactor::try_new(rdr.read_i16()?)?;
            let flags = rdr.read_u8()?;
            let sign = decode_signed(flags & 1);
            let endian = if (flags & 2) != 0 {
                BinaryEndianness::BigEndian
            } else {
                BinaryEndianness::Native
            };
            let trunc = if (flags & 4) != 0 {
                TruncationPolicy::FullBinaryRange
            } else {
                TruncationPolicy::TruncateToPic
            };
            Ok(HirType::BinaryInt {
                byte_width,
                digits,
                scale,
                sign,
                endian,
                trunc,
            })
        }
        6 => {
            let byte_len = rdr.read_u32()?;
            let digits = DigitCount::try_new(rdr.read_u8()?)?;
            let scale = ScaleFactor::try_new(rdr.read_i16()?)?;
            let sign_capability = decode_signed(rdr.read_u8()?);
            let blank_zero = decode_bz(rdr.read_u8()?);
            let edit_ops = read_edit_ops(rdr)?;
            Ok(HirType::NumericEdited {
                byte_len,
                digits,
                scale,
                sign_capability,
                edit_ops,
                blank_zero,
            })
        }
        7 => Ok(HirType::FloatBinary {
            width: match rdr.read_u8()? {
                0 => FloatBinaryWidth::Bits32,
                1 => FloatBinaryWidth::Bits64,
                _ => FloatBinaryWidth::Bits128,
            },
        }),
        _ => Ok(HirType::FloatDecimal {
            width: if rdr.read_u8()? == 0 {
                FloatDecimalWidth::Decimal64
            } else {
                FloatDecimalWidth::Decimal128
            },
        }),
    }
}

/// Appends a length-prefixed `PicEditOp` list to `out`.
///
/// # Arguments
///
/// * `out` - Output buffer.
/// * `ops` - `PicEditOp` slice.
fn write_edit_ops(out: &mut Vec<u8>, ops: &[PicEditOp]) {
    let n = u16::try_from(ops.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&n.to_le_bytes());
    for op in ops.iter().take(usize::from(n)) {
        match op {
            PicEditOp::AlphaSlot => out.push(0),
            PicEditOp::AnyCharSlot => out.push(1),
            PicEditOp::DigitSlot => out.push(2),
            PicEditOp::ImpliedDecimal => out.push(3),
            PicEditOp::ScalingPosition => out.push(4),
            PicEditOp::ZeroSuppressSpace => out.push(5),
            PicEditOp::ZeroSuppressAsterisk => out.push(6),
            PicEditOp::FloatingSignPlus => out.push(7),
            PicEditOp::FloatingSignMinus => out.push(8),
            PicEditOp::FloatingCurrency(ch) => {
                out.push(9);
                out.extend_from_slice(&(*ch as u32).to_le_bytes());
            }
            PicEditOp::FixedSignPlus => out.push(10),
            PicEditOp::FixedSignMinus => out.push(11),
            PicEditOp::FixedCreditCr => out.push(12),
            PicEditOp::FixedDebitDb => out.push(13),
            PicEditOp::FixedCurrency(ch) => {
                out.push(14);
                out.extend_from_slice(&(*ch as u32).to_le_bytes());
            }
            PicEditOp::InsertBlank => out.push(15),
            PicEditOp::InsertZero => out.push(16),
            PicEditOp::InsertSlash => out.push(17),
            PicEditOp::InsertGroupSep => out.push(18),
            PicEditOp::InsertDecimalPoint => out.push(19),
            PicEditOp::BitSlot => out.push(20),
            PicEditOp::NationalSlot => out.push(21),
            PicEditOp::Utf8Slot => out.push(22),
        }
    }
}

/// Reads a length-prefixed `PicEditOp` vector from `rdr`.
///
/// # Arguments
///
/// * `rdr` - Active [`ByteReader`].
fn read_edit_ops(rdr: &mut ByteReader<'_>) -> Result<Vec<PicEditOp>> {
    let n = usize::from(rdr.read_u16()?);
    let mut ops = Vec::with_capacity(n.min(256));
    for _ in 0..n {
        let op = match rdr.read_u8()? {
            0 => PicEditOp::AlphaSlot,
            1 => PicEditOp::AnyCharSlot,
            2 => PicEditOp::DigitSlot,
            3 => PicEditOp::ImpliedDecimal,
            4 => PicEditOp::ScalingPosition,
            5 => PicEditOp::ZeroSuppressSpace,
            6 => PicEditOp::ZeroSuppressAsterisk,
            7 => PicEditOp::FloatingSignPlus,
            8 => PicEditOp::FloatingSignMinus,
            9 => PicEditOp::FloatingCurrency(rdr.read_char()?),
            10 => PicEditOp::FixedSignPlus,
            11 => PicEditOp::FixedSignMinus,
            12 => PicEditOp::FixedCreditCr,
            13 => PicEditOp::FixedDebitDb,
            14 => PicEditOp::FixedCurrency(rdr.read_char()?),
            15 => PicEditOp::InsertBlank,
            16 => PicEditOp::InsertZero,
            17 => PicEditOp::InsertSlash,
            18 => PicEditOp::InsertGroupSep,
            19 => PicEditOp::InsertDecimalPoint,
            20 => PicEditOp::BitSlot,
            21 => PicEditOp::NationalSlot,
            22 => PicEditOp::Utf8Slot,
            other => {
                return Err(CoblangError::PictureCodec {
                    picture: String::from("<runtime-desc>"),
                    message: format!("unknown PicEditOp tag {other}"),
                });
            }
        };
        ops.push(op);
    }
    Ok(ops)
}

/// Encodes a [`DataCategory`] into its compact `u8` ABI code (`0..=12`).
///
/// # Arguments
///
/// * `cat` - Category to encode.
#[must_use]
pub const fn encode_data_category(cat: DataCategory) -> u8 {
    match cat {
        DataCategory::Alphabetic => 0,
        DataCategory::Alphanumeric => 1,
        DataCategory::AlphanumericEdited => 2,
        DataCategory::Numeric => 3,
        DataCategory::NumericEdited => 4,
        DataCategory::National => 5,
        DataCategory::NationalEdited => 6,
        DataCategory::Utf8 => 7,
        DataCategory::BooleanBit => 8,
        DataCategory::Pointer => 9,
        DataCategory::Index => 10,
        DataCategory::ObjectReference => 11,
        DataCategory::Group => 12,
    }
}

/// Decodes a [`DataCategory`] from its compact `u8` ABI code.
///
/// # Arguments
///
/// * `code` - Raw category ABI byte.
///
/// # Errors
///
/// Returns [`CoblangError::PictureCodec`] if `code > 12`.
pub fn decode_data_category(code: u8) -> Result<DataCategory> {
    match code {
        0 => Ok(DataCategory::Alphabetic),
        1 => Ok(DataCategory::Alphanumeric),
        2 => Ok(DataCategory::AlphanumericEdited),
        3 => Ok(DataCategory::Numeric),
        4 => Ok(DataCategory::NumericEdited),
        5 => Ok(DataCategory::National),
        6 => Ok(DataCategory::NationalEdited),
        7 => Ok(DataCategory::Utf8),
        8 => Ok(DataCategory::BooleanBit),
        9 => Ok(DataCategory::Pointer),
        10 => Ok(DataCategory::Index),
        11 => Ok(DataCategory::ObjectReference),
        12 => Ok(DataCategory::Group),
        other => Err(CoblangError::PictureCodec {
            picture: String::from("<init-category>"),
            message: format!("unknown DataCategory code {other}"),
        }),
    }
}

/// Single key descriptor inside a serialized [`DecodedTableSortPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeTableSortKey {
    /// `Ascending` vs `Descending` key order.
    pub direction: HirSortDirection,
    /// Relative byte offset of the key within each table element.
    pub rel_offset: u32,
    /// Key byte length (`>= 1`).
    pub byte_len: u32,
    /// Key field representation metadata.
    pub spec: RuntimeFieldSpec,
}

/// Decoded in-memory `SORT table` execution plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedTableSortPlan {
    /// Static byte stride of one table element (`>= 1`).
    pub stride: u32,
    /// Optional 256-byte ordinal collation rank table for non-numeric keys.
    pub collating_ranks: Option<[u8; 256]>,
    /// Ordered sort keys from highest to lowest precedence.
    pub keys: Vec<RuntimeTableSortKey>,
}

/// Encodes a [`DecodedTableSortPlan`] into a compact binary byte vector.
///
/// # Arguments
///
/// * `plan` - Table sort plan to serialize.
#[must_use]
pub fn encode_table_sort_plan(plan: &DecodedTableSortPlan) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + plan.keys.len() * 24);
    out.extend_from_slice(&plan.stride.to_le_bytes());
    if let Some(ranks) = &plan.collating_ranks {
        out.push(1);
        out.extend_from_slice(ranks);
    } else {
        out.push(0);
    }
    let key_cnt = u16::try_from(plan.keys.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&key_cnt.to_le_bytes());
    for key in plan.keys.iter().take(usize::from(key_cnt)) {
        let dir_byte = match key.direction {
            HirSortDirection::Ascending => 0_u8,
            HirSortDirection::Descending => 1_u8,
        };
        out.push(dir_byte);
        out.extend_from_slice(&key.rel_offset.to_le_bytes());
        out.extend_from_slice(&key.byte_len.to_le_bytes());
        write_field_spec_into(&mut out, &key.spec);
    }
    out
}

/// Decodes a [`DecodedTableSortPlan`] from its compact binary representation.
///
/// # Arguments
///
/// * `bytes` - Serialized table sort plan bytes.
///
/// # Errors
///
/// Returns [`CoblangError::PictureCodec`] if `bytes` is truncated or contains an invalid tag.
pub fn decode_table_sort_plan(bytes: &[u8]) -> Result<DecodedTableSortPlan> {
    let mut rdr = ByteReader::new(bytes);
    let stride = rdr.read_u32()?;
    let has_ranks = rdr.read_u8()?;
    let collating_ranks = match has_ranks {
        0 => None,
        1 => {
            let mut tbl = [0_u8; 256];
            for slot in &mut tbl {
                *slot = rdr.read_u8()?;
            }
            Some(tbl)
        }
        other => {
            return Err(CoblangError::PictureCodec {
                picture: String::from("<table-sort-plan>"),
                message: format!("invalid collating-ranks flag {other}"),
            });
        }
    };
    let key_cnt = usize::from(rdr.read_u16()?);
    let mut keys = Vec::with_capacity(key_cnt);
    for _ in 0..key_cnt {
        let dir_code = rdr.read_u8()?;
        let direction = match dir_code {
            0 => HirSortDirection::Ascending,
            1 => HirSortDirection::Descending,
            other => {
                return Err(CoblangError::PictureCodec {
                    picture: String::from("<table-sort-plan>"),
                    message: format!("invalid sort direction tag {other}"),
                });
            }
        };
        let rel_offset = rdr.read_u32()?;
        let byte_len = rdr.read_u32()?;
        let spec = read_field_spec_from(&mut rdr)?;
        keys.push(RuntimeTableSortKey {
            direction,
            rel_offset,
            byte_len,
            spec,
        });
    }
    Ok(DecodedTableSortPlan {
        stride,
        collating_ranks,
        keys,
    })
}
