//! Strongly-typed `(SourceCategory, DestCategory)` `MOVE` conversion matrix, `Justification`,
//! and encoding-aware padding, truncation, and transcoding (`ASCII`, `EBCDIC`, `UTF-16`, `UTF-8`).

use coblang_common::{CoblangError, Result, TextEncoding};
use coblang_decimal::charset::{
    Codepage, NationalJustify, UnmappedCharPolicy, decode_to_chars, move_national_utf16be,
    move_utf8_characters, transcode_bytes,
};
use coblang_hir::{DataCategory, JustifiedSpec};

use crate::edit_alpha::{format_alphanumeric, format_national};

/// Character alignment policy for padding and truncation during `MOVE` and `PICTURE` editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Justification {
    /// Standard left alignment (right-pad with encoding space, truncate excess right positions).
    Left,
    /// `JUSTIFIED RIGHT` alignment (left-pad with encoding space, truncate excess left positions).
    Right,
}

impl Justification {
    /// Converts a [`JustifiedSpec`] from HIR into [`Justification`].
    ///
    /// # Arguments
    ///
    /// * `spec` - Resolved HIR [`JustifiedSpec`].
    #[must_use]
    pub const fn from_hir(spec: JustifiedSpec) -> Self {
        match spec {
            JustifiedSpec::Left => Self::Left,
            JustifiedSpec::Right => Self::Right,
        }
    }

    /// Converts `self` into [`JustifiedSpec`].
    #[must_use]
    pub const fn to_hir(self) -> JustifiedSpec {
        match self {
            Self::Left => JustifiedSpec::Left,
            Self::Right => JustifiedSpec::Right,
        }
    }

    /// Converts `self` into [`NationalJustify`] for `UTF-16BE` national buffers.
    #[must_use]
    pub const fn to_national_justify(self) -> NationalJustify {
        match self {
            Self::Left => NationalJustify::Left,
            Self::Right => NationalJustify::Right,
        }
    }
}

/// Source operand category in the ISO COBOL `MOVE` conversion matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceCategory {
    /// Alphabetic (`PIC A`).
    Alphabetic,
    /// Alphanumeric (`PIC X` or string literal).
    Alphanumeric,
    /// Alphanumeric-edited (`PIC X/B/0`).
    AlphanumericEdited,
    /// Exact or floating numeric (`9`, `COMP*`, `FLOAT*`, numeric literal).
    Numeric,
    /// Numeric-edited (`PIC Z,ZZ9.99`, etc.).
    NumericEdited,
    /// National UTF-16 (`PIC N`).
    National,
    /// National-edited (`PIC N/B/0`).
    NationalEdited,
    /// UTF-8 (`PIC U`).
    Utf8,
    /// Alphanumeric group item.
    Group,
    /// Figurative constant (`SPACE`, `ZERO`, `HIGH-VALUE`, `LOW-VALUE`, `QUOTE`, `ALL`).
    FigurativeConstant,
}

/// Ordered array of all 10 [`SourceCategory`] variants.
pub const ALL_SOURCE_CATEGORIES: [SourceCategory; 10] = [
    SourceCategory::Alphabetic,
    SourceCategory::Alphanumeric,
    SourceCategory::AlphanumericEdited,
    SourceCategory::Numeric,
    SourceCategory::NumericEdited,
    SourceCategory::National,
    SourceCategory::NationalEdited,
    SourceCategory::Utf8,
    SourceCategory::Group,
    SourceCategory::FigurativeConstant,
];

impl SourceCategory {
    /// Maps a HIR [`DataCategory`] to its [`SourceCategory`], or `None` for non-character/non-numeric pointers/bits.
    ///
    /// # Arguments
    ///
    /// * `cat` - HIR [`DataCategory`].
    #[must_use]
    pub const fn from_data_category(cat: DataCategory) -> Option<Self> {
        match cat {
            DataCategory::Alphabetic => Some(Self::Alphabetic),
            DataCategory::Alphanumeric => Some(Self::Alphanumeric),
            DataCategory::AlphanumericEdited => Some(Self::AlphanumericEdited),
            DataCategory::Numeric => Some(Self::Numeric),
            DataCategory::NumericEdited => Some(Self::NumericEdited),
            DataCategory::National => Some(Self::National),
            DataCategory::NationalEdited => Some(Self::NationalEdited),
            DataCategory::Utf8 => Some(Self::Utf8),
            DataCategory::Group => Some(Self::Group),
            DataCategory::BooleanBit
            | DataCategory::Pointer
            | DataCategory::Index
            | DataCategory::ObjectReference => None,
        }
    }
}

/// Destination receiving category in the ISO COBOL `MOVE` conversion matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DestCategory {
    /// Alphabetic (`PIC A`).
    Alphabetic,
    /// Alphanumeric (`PIC X`).
    Alphanumeric,
    /// Alphanumeric-edited (`PIC X/B/0`).
    AlphanumericEdited,
    /// Exact or floating numeric (`9`, `COMP*`, `FLOAT*`).
    Numeric,
    /// Numeric-edited (`PIC Z,ZZ9.99`, etc.).
    NumericEdited,
    /// National UTF-16 (`PIC N`).
    National,
    /// National-edited (`PIC N/B/0`).
    NationalEdited,
    /// UTF-8 (`PIC U`).
    Utf8,
    /// Alphanumeric group item.
    Group,
}

/// Ordered array of all 9 [`DestCategory`] variants.
pub const ALL_DEST_CATEGORIES: [DestCategory; 9] = [
    DestCategory::Alphabetic,
    DestCategory::Alphanumeric,
    DestCategory::AlphanumericEdited,
    DestCategory::Numeric,
    DestCategory::NumericEdited,
    DestCategory::National,
    DestCategory::NationalEdited,
    DestCategory::Utf8,
    DestCategory::Group,
];

impl DestCategory {
    /// Maps a HIR [`DataCategory`] to its [`DestCategory`], or `None` for non-character/non-numeric pointers/bits.
    ///
    /// # Arguments
    ///
    /// * `cat` - HIR [`DataCategory`].
    #[must_use]
    pub const fn from_data_category(cat: DataCategory) -> Option<Self> {
        match cat {
            DataCategory::Alphabetic => Some(Self::Alphabetic),
            DataCategory::Alphanumeric => Some(Self::Alphanumeric),
            DataCategory::AlphanumericEdited => Some(Self::AlphanumericEdited),
            DataCategory::Numeric => Some(Self::Numeric),
            DataCategory::NumericEdited => Some(Self::NumericEdited),
            DataCategory::National => Some(Self::National),
            DataCategory::NationalEdited => Some(Self::NationalEdited),
            DataCategory::Utf8 => Some(Self::Utf8),
            DataCategory::Group => Some(Self::Group),
            DataCategory::BooleanBit
            | DataCategory::Pointer
            | DataCategory::Index
            | DataCategory::ObjectReference => None,
        }
    }
}

/// Semantic conversion pathway selected by [`classify_move`] for a `(SourceCategory, DestCategory)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MoveConversionKind {
    /// Raw alphanumeric group transfer (left-justified, padded with active encoding space).
    GroupRawPad,
    /// Character pad/truncation with destination [`Justification`].
    AlphaPadJustify,
    /// Character alignment followed by alphanumeric `PicEditOp` insertion formatting.
    AlphaEditFormat,
    /// Unsigned integer digit extraction from a numeric item into character slots.
    NumericToAlphaDigits,
    /// Numeric-edited or alphanumeric de-editing into `FixedDecimal` followed by numeric store.
    DeEditToNumeric,
    /// Exact decimal rescaling, rounding/truncation, and numeric/numeric-edited encoding.
    NumericScaleAndStore,
    /// Character/integer transcoding and `U+0020` alignment into `USAGE NATIONAL` (`UTF-16`).
    TranscodeToNational,
    /// Character/integer transcoding followed by `NATIONAL-EDITED` `PicEditOp` insertion formatting.
    TranscodeToNationalEdited,
    /// Character/integer transcoding into `USAGE UTF-8` with scalar-boundary truncation and `0x20` padding.
    TranscodeToUtf8,
    /// `NATIONAL` (`UTF-16`) decoding into target single-byte display codepage (`ASCII` / `EBCDIC`).
    TranscodeFromNationalToSbcs,
    /// Figurative constant expansion/tiling (`SPACE`, `ZERO`, `HIGH-VALUE`, `LOW-VALUE`, `QUOTE`, `ALL`).
    FigurativeFill,
}

/// Resolves the ISO/IEC 1989:2023 conversion pathway for `(src, dst)`.
///
/// # Arguments
///
/// * `src` - [`SourceCategory`] of the sending operand.
/// * `dst` - [`DestCategory`] of the receiving item.
///
/// # Errors
///
/// Returns [`CoblangError::PictureCodec`] when ISO COBOL forbids an elementary `MOVE` between `src` and `dst`.
pub fn classify_move(src: SourceCategory, dst: DestCategory) -> Result<MoveConversionKind> {
    if src == SourceCategory::FigurativeConstant {
        return Ok(MoveConversionKind::FigurativeFill);
    }

    match dst {
        DestCategory::Group => Ok(MoveConversionKind::GroupRawPad),
        DestCategory::National => Ok(MoveConversionKind::TranscodeToNational),
        DestCategory::NationalEdited => Ok(MoveConversionKind::TranscodeToNationalEdited),
        DestCategory::Utf8 => Ok(MoveConversionKind::TranscodeToUtf8),
        DestCategory::Alphabetic => match src {
            SourceCategory::Alphabetic
            | SourceCategory::Alphanumeric
            | SourceCategory::AlphanumericEdited
            | SourceCategory::Utf8
            | SourceCategory::Group => Ok(MoveConversionKind::AlphaPadJustify),
            SourceCategory::National | SourceCategory::NationalEdited => {
                Ok(MoveConversionKind::TranscodeFromNationalToSbcs)
            }
            _ => Err(illegal_move_err(src, dst)),
        },
        DestCategory::Alphanumeric => match src {
            SourceCategory::National | SourceCategory::NationalEdited => {
                Ok(MoveConversionKind::TranscodeFromNationalToSbcs)
            }
            SourceCategory::Numeric | SourceCategory::NumericEdited => {
                Ok(MoveConversionKind::NumericToAlphaDigits)
            }
            _ => Ok(MoveConversionKind::AlphaPadJustify),
        },
        DestCategory::AlphanumericEdited => match src {
            SourceCategory::Numeric | SourceCategory::NumericEdited => {
                Ok(MoveConversionKind::NumericToAlphaDigits)
            }
            _ => Ok(MoveConversionKind::AlphaEditFormat),
        },
        DestCategory::Numeric | DestCategory::NumericEdited => match src {
            SourceCategory::Numeric => Ok(MoveConversionKind::NumericScaleAndStore),
            SourceCategory::Alphanumeric
            | SourceCategory::NumericEdited
            | SourceCategory::Utf8
            | SourceCategory::Group => Ok(MoveConversionKind::DeEditToNumeric),
            _ => Err(illegal_move_err(src, dst)),
        },
    }
}

/// Constructs a [`CoblangError::PictureCodec`] describing an illegal `(src, dst)` `MOVE` pair.
///
/// # Arguments
///
/// * `src` - Sending [`SourceCategory`].
/// * `dst` - Receiving [`DestCategory`].
#[must_use]
fn illegal_move_err(src: SourceCategory, dst: DestCategory) -> CoblangError {
    CoblangError::PictureCodec {
        picture: String::from("MOVE"),
        message: format!("illegal elementary MOVE from {src:?} to {dst:?}"),
    }
}

/// Returns the single-byte space codepoint for `codepage` (`Some(0x20)` ASCII/UTF-8, `Some(0x40)` EBCDIC, `None` UTF-16).
///
/// # Arguments
///
/// * `codepage` - Target [`Codepage`].
#[must_use]
pub const fn encoding_space_byte(codepage: Codepage) -> Option<u8> {
    codepage.single_byte_space()
}

/// Pads or truncates a single-byte character buffer `src` to `byte_len` using `codepage`'s space byte (`0x20` or `0x40`).
///
/// # Arguments
///
/// * `src` - Source SBCS bytes already encoded in `codepage`.
/// * `byte_len` - Target storage byte length.
/// * `justification` - [`Justification::Left`] or [`Justification::Right`].
/// * `codepage` - Single-byte [`Codepage`] (`ASCII`, `ISO-8859-*`, `CP1252`, or `EBCDIC` `CP*`).
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] if `codepage` is multi-byte UTF-16 (`single_byte_space() == None`).
pub fn pad_or_truncate_sbcs(
    src: &[u8],
    byte_len: usize,
    justification: Justification,
    codepage: Codepage,
) -> Result<Vec<u8>> {
    let space_byte =
        codepage
            .single_byte_space()
            .ok_or_else(|| CoblangError::EncodingConversion {
                encoding: codepage.to_text_encoding(),
                byte_offset: 0,
                message: format!(
                    "{} is a 16-bit codepage without a single-byte space",
                    codepage.canonical_name()
                ),
            })?;
    Ok(format_alphanumeric(
        src,
        byte_len,
        justification.to_hir(),
        space_byte,
    ))
}

/// Pads or truncates `UTF-16BE` `NATIONAL` bytes to `char_len` 16-bit units (`[0x00, 0x20]` space).
///
/// # Arguments
///
/// * `src_utf16be` - Source `UTF-16BE` byte slice (must have even length).
/// * `char_len` - Destination 16-bit character unit count.
/// * `justification` - [`Justification::Left`] or [`Justification::Right`].
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] if `src_utf16be.len()` is odd.
pub fn pad_or_truncate_national_utf16be(
    src_utf16be: &[u8],
    char_len: usize,
    justification: Justification,
) -> Result<Vec<u8>> {
    move_national_utf16be(src_utf16be, char_len, justification.to_national_justify())
}

/// Pads or truncates `UTF-16LE` `NATIONAL` bytes to `char_len` 16-bit units (`[0x20, 0x00]` space).
///
/// # Arguments
///
/// * `src_utf16le` - Source `UTF-16LE` byte slice (must have even length).
/// * `char_len` - Destination 16-bit character unit count.
/// * `justification` - [`Justification::Left`] or [`Justification::Right`].
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] if `src_utf16le.len()` is odd.
pub fn pad_or_truncate_national_utf16le(
    src_utf16le: &[u8],
    char_len: usize,
    justification: Justification,
) -> Result<Vec<u8>> {
    if !src_utf16le.len().is_multiple_of(2) {
        return Err(CoblangError::EncodingConversion {
            encoding: TextEncoding::Codepage(String::from("UTF-16LE")),
            byte_offset: src_utf16le.len().saturating_sub(1),
            message: String::from("NATIONAL UTF-16LE byte slice has odd length"),
        });
    }
    Ok(format_national(
        src_utf16le,
        char_len,
        justification.to_hir(),
    ))
}

/// Pads or truncates `src_utf8` cleanly on Unicode scalar boundaries to `<= max_chars` and `<= byte_len`,
/// padding unused storage bytes with ASCII `0x20` according to `justification`.
///
/// # Arguments
///
/// * `src_utf8` - Valid UTF-8 source string.
/// * `max_chars` - Maximum Unicode scalar count (`PIC U(max_chars)`).
/// * `byte_len` - Fixed storage byte capacity.
/// * `justification` - [`Justification::Left`] or [`Justification::Right`].
#[must_use]
pub fn pad_or_truncate_utf8(
    src_utf8: &str,
    max_chars: usize,
    byte_len: usize,
    justification: Justification,
) -> Vec<u8> {
    match justification {
        Justification::Left => move_utf8_characters(src_utf8, max_chars, byte_len, byte_len),
        Justification::Right => {
            let mut kept_rev: Vec<char> = Vec::new();
            let mut used_bytes = 0_usize;
            for ch in src_utf8.chars().rev() {
                if kept_rev.len() >= max_chars {
                    break;
                }
                let ch_len = ch.len_utf8();
                if used_bytes.saturating_add(ch_len) > byte_len {
                    break;
                }
                kept_rev.push(ch);
                used_bytes = used_bytes.saturating_add(ch_len);
            }
            let pad_count = byte_len.saturating_sub(used_bytes);
            let mut out = vec![b' '; pad_count];
            let mut enc_buf = [0_u8; 4];
            for ch in kept_rev.into_iter().rev() {
                out.extend_from_slice(ch.encode_utf8(&mut enc_buf).as_bytes());
            }
            out
        }
    }
}

/// Pads or truncates raw bytes into a `USAGE UTF-8` field of `byte_len` bytes without splitting
/// valid multi-byte UTF-8 codepoints when `src` is valid UTF-8.
///
/// # Arguments
///
/// * `src` - Source byte slice.
/// * `byte_len` - Target `UTF-8` storage byte length.
/// * `justification` - [`Justification::Left`] or [`Justification::Right`].
#[must_use]
pub fn pad_or_truncate_utf8_bytes_lossless(
    src: &[u8],
    byte_len: usize,
    justification: Justification,
) -> Vec<u8> {
    if let Ok(valid_str) = core::str::from_utf8(src) {
        pad_or_truncate_utf8(valid_str, byte_len, byte_len, justification)
    } else {
        format_alphanumeric(src, byte_len, justification.to_hir(), b' ')
    }
}

/// Transcodes `src` from `src_codepage` into a `UTF-16LE` `NATIONAL` buffer of `char_len` 16-bit units.
///
/// # Arguments
///
/// * `src` - Source bytes encoded in `src_codepage`.
/// * `src_codepage` - Source [`Codepage`].
/// * `char_len` - Destination UTF-16 code unit capacity.
/// * `justification` - [`Justification::Left`] or [`Justification::Right`].
/// * `policy` - [`UnmappedCharPolicy`] for invalid source bytes.
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] if decoding `src` fails under `StrictError`.
pub fn transcode_to_national_utf16le(
    src: &[u8],
    src_codepage: Codepage,
    char_len: usize,
    justification: Justification,
    policy: UnmappedCharPolicy,
) -> Result<Vec<u8>> {
    let u16le = transcode_bytes(src, src_codepage, Codepage::Utf16Le, policy)?;
    Ok(format_national(&u16le, char_len, justification.to_hir()))
}

/// Transcodes `UTF-16LE` `NATIONAL` bytes into a single-byte `dst_codepage` buffer of `byte_len` bytes.
///
/// # Arguments
///
/// * `src_utf16le` - Source `UTF-16LE` bytes.
/// * `dst_codepage` - Target single-byte [`Codepage`] (`ASCII` / `EBCDIC` / `CP1252` / `ISO-8859-*`).
/// * `byte_len` - Target storage byte width.
/// * `justification` - [`Justification::Left`] or [`Justification::Right`].
/// * `policy` - [`UnmappedCharPolicy`] for unrepresentable codepoints or invalid UTF-16.
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] if transcoding fails or `dst_codepage` is UTF-16.
pub fn transcode_national_utf16le_to_sbcs(
    src_utf16le: &[u8],
    dst_codepage: Codepage,
    byte_len: usize,
    justification: Justification,
    policy: UnmappedCharPolicy,
) -> Result<Vec<u8>> {
    let sbcs = transcode_bytes(src_utf16le, Codepage::Utf16Le, dst_codepage, policy)?;
    pad_or_truncate_sbcs(&sbcs, byte_len, justification, dst_codepage)
}

/// Transcodes `src` from `src_codepage` into a `USAGE UTF-8` field bounded by `max_chars` and `byte_len`.
///
/// # Arguments
///
/// * `src` - Source bytes encoded in `src_codepage`.
/// * `src_codepage` - Source [`Codepage`].
/// * `max_chars` - Maximum Unicode scalar count (`PIC U(max_chars)`).
/// * `byte_len` - Fixed storage byte capacity.
/// * `justification` - [`Justification::Left`] or [`Justification::Right`].
/// * `policy` - [`UnmappedCharPolicy`] for invalid source bytes.
///
/// # Errors
///
/// Returns [`CoblangError::EncodingConversion`] if decoding `src` fails under `StrictError`.
pub fn transcode_to_utf8(
    src: &[u8],
    src_codepage: Codepage,
    max_chars: usize,
    byte_len: usize,
    justification: Justification,
    policy: UnmappedCharPolicy,
) -> Result<Vec<u8>> {
    let chars = decode_to_chars(src, src_codepage, policy)?;
    let utf8_str: String = chars.into_iter().collect();
    Ok(pad_or_truncate_utf8(
        &utf8_str,
        max_chars,
        byte_len,
        justification,
    ))
}
