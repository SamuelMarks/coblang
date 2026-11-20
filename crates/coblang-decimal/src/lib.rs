//! Pure-Rust exact 38/76-digit fixed-point decimal arithmetic, ISO 2023 rounding, and storage codecs.

/// Pure-Rust EBCDIC, ISO-8859, `CP1252`, `UTF-8`, and `UTF-16` codepage transcoding (`Milestone M17`).
pub mod charset;
/// Storage representation encoders, decoders, and `IS NUMERIC` validators (zoned, packed, binary, float).
pub mod codecs;
/// Scaled fixed-point decimal type (`FixedDecimal`) with fast-path `i128` and 256-bit escalation.
pub mod fixed_decimal;
/// All eight ISO/IEC 1989:2023 COBOL rounding modes and `SIZE ERROR` status outcomes.
pub mod rounding;
/// Zero-allocation 256-bit unsigned and signed integer arithmetic (`Uint256`, `SignedInt256`).
pub mod wide_int;

pub use charset::{
    ALL_CODEPAGES, ALL_EBCDIC_CODEPAGES, ALL_FIGURATIVE_CHARS, Codepage, EbcdicCodepage,
    FigurativeConstantChar, FigurativeUnit, NationalJustify, UnmappedCharPolicy,
    compare_alphanumeric_ebcdic, compare_national_utf16be, decode_to_chars, display_of,
    encode_from_chars, fill_figurative_bytes, move_national_utf16be, move_utf8_characters,
    national_of, transcode_bytes,
};
pub use codecs::{
    binary::{
        BinaryByteOrder, BinaryLayout, BinarySignMode, BinaryTruncMode, decode_binary,
        encode_binary,
    },
    float::{
        Decimal64Bid, Decimal128Bid, FloatBinary128, decode_comp1_ne, decode_comp2_ne,
        encode_comp1_ne, encode_comp2_ne, pack_dpd_declet, unpack_dpd_declet,
    },
    packed::{
        PackedKind, PackedLayout, PackedSignPolicy, decode_packed, encode_packed,
        is_valid_packed_bcd,
    },
    zoned::{
        BlankZeroMode, OverpunchDialect, SignPlacement, ZonedLayout, decode_zoned, encode_zoned,
        is_valid_zoned_numeric,
    },
};
pub use fixed_decimal::{FixedDecimal, HighOrderPolicy, SignednessPolicy};
pub use rounding::{RoundingMode, SizeErrorStatus, round_quotient_with_remainder};
pub use wide_int::{DecimalSign, SignedInt256, Uint256};
