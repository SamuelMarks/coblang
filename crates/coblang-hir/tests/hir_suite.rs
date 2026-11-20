//! Exhaustive unit and branch coverage suite for `coblang-hir`.

use coblang_common::{DigitCount, Result, ScaleFactor, SymbolId};
use coblang_hir::{
    BinaryEndianness, BlankZeroSpec, Comp6Mode, DataCategory, DynamicOdoPresence,
    ExceptionCheckMask, FloatBinaryWidth, FloatDecimalWidth, HirType, JustifiedSpec,
    NumericSignSpec, PicEditOp, Signedness, TruncationPolicy,
};

/// Exercises alphabetic, alphanumeric, alphanumeric-edited, and zoned-decimal [`HirType`] variants.
#[test]
fn hir_type_alpha_and_zoned_categories_and_sizes() -> Result<()> {
    let d5 = DigitCount::try_new(5)?;
    let s0 = ScaleFactor::ZERO;

    let cases = [
        (
            HirType::Alphabetic {
                byte_len: 3,
                justified: JustifiedSpec::Left,
            },
            DataCategory::Alphabetic,
            3,
        ),
        (
            HirType::Alphanumeric {
                byte_len: 8,
                justified: JustifiedSpec::Right,
            },
            DataCategory::Alphanumeric,
            8,
        ),
        (
            HirType::AlphanumericEdited {
                byte_len: 4,
                edit_ops: vec![PicEditOp::AnyCharSlot],
            },
            DataCategory::AlphanumericEdited,
            4,
        ),
        (
            HirType::ZonedDecimal {
                digits: d5,
                scale: s0,
                sign: NumericSignSpec::Unsigned,
                blank_zero: BlankZeroSpec::RetainZero,
            },
            DataCategory::Numeric,
            5,
        ),
        (
            HirType::ZonedDecimal {
                digits: d5,
                scale: s0,
                sign: NumericSignSpec::EmbeddedLeading,
                blank_zero: BlankZeroSpec::RetainZero,
            },
            DataCategory::Numeric,
            5,
        ),
        (
            HirType::ZonedDecimal {
                digits: d5,
                scale: s0,
                sign: NumericSignSpec::EmbeddedTrailing,
                blank_zero: BlankZeroSpec::BlankWhenZero,
            },
            DataCategory::Numeric,
            5,
        ),
        (
            HirType::ZonedDecimal {
                digits: d5,
                scale: s0,
                sign: NumericSignSpec::SeparateLeading,
                blank_zero: BlankZeroSpec::RetainZero,
            },
            DataCategory::Numeric,
            6,
        ),
        (
            HirType::ZonedDecimal {
                digits: d5,
                scale: s0,
                sign: NumericSignSpec::SeparateTrailing,
                blank_zero: BlankZeroSpec::RetainZero,
            },
            DataCategory::Numeric,
            6,
        ),
    ];

    for (ty, expected_cat, expected_bytes) in cases {
        assert_eq!(ty.category(), expected_cat);
        assert_eq!(ty.static_byte_size(), expected_bytes);
    }
    Ok(())
}

/// Exercises packed-decimal, binary-integer, and numeric-edited [`HirType`] variants.
#[test]
fn hir_type_packed_binary_and_edited_categories_and_sizes() -> Result<()> {
    let d5 = DigitCount::try_new(5)?;
    let d4 = DigitCount::try_new(4)?;
    let s0 = ScaleFactor::ZERO;

    let cases = [
        (
            HirType::PackedDecimal {
                digits: d5,
                scale: s0,
                sign: Signedness::Signed,
                unsigned_comp6: Comp6Mode::StandardComp3,
            },
            DataCategory::Numeric,
            3,
        ),
        (
            HirType::PackedDecimal {
                digits: d4,
                scale: s0,
                sign: Signedness::Unsigned,
                unsigned_comp6: Comp6Mode::UnsignedComp6,
            },
            DataCategory::Numeric,
            2,
        ),
        (
            HirType::BinaryInt {
                byte_width: 4,
                digits: d5,
                scale: s0,
                sign: Signedness::Signed,
                endian: BinaryEndianness::BigEndian,
                trunc: TruncationPolicy::TruncateToPic,
            },
            DataCategory::Numeric,
            4,
        ),
        (
            HirType::NumericEdited {
                byte_len: 7,
                digits: d5,
                scale: s0,
                sign_capability: Signedness::Signed,
                edit_ops: vec![PicEditOp::DigitSlot],
                blank_zero: BlankZeroSpec::BlankWhenZero,
            },
            DataCategory::NumericEdited,
            7,
        ),
    ];

    for (ty, expected_cat, expected_bytes) in cases {
        assert_eq!(ty.category(), expected_cat);
        assert_eq!(ty.static_byte_size(), expected_bytes);
    }
    Ok(())
}

/// Exercises floating-point, Unicode, bit, pointer, index, OO, and group [`HirType`] variants.
#[test]
fn hir_type_non_decimal_categories_and_static_sizes() {
    let cases = [
        (
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits32,
            },
            DataCategory::Numeric,
            4,
        ),
        (
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits64,
            },
            DataCategory::Numeric,
            8,
        ),
        (
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits128,
            },
            DataCategory::Numeric,
            16,
        ),
        (
            HirType::FloatDecimal {
                width: FloatDecimalWidth::Decimal64,
            },
            DataCategory::Numeric,
            8,
        ),
        (
            HirType::FloatDecimal {
                width: FloatDecimalWidth::Decimal128,
            },
            DataCategory::Numeric,
            16,
        ),
        (
            HirType::National {
                char_len: 3,
                justified: JustifiedSpec::Left,
            },
            DataCategory::National,
            6,
        ),
        (
            HirType::NationalEdited {
                char_len: 4,
                edit_ops: vec![PicEditOp::NationalSlot],
            },
            DataCategory::NationalEdited,
            8,
        ),
        (HirType::Utf8 { byte_len: 12 }, DataCategory::Utf8, 12),
        (HirType::Bit { bit_len: 9 }, DataCategory::BooleanBit, 2),
        (HirType::Pointer, DataCategory::Pointer, 8),
        (HirType::ProgramPointer, DataCategory::Pointer, 8),
        (HirType::Index, DataCategory::Index, 8),
        (
            HirType::ObjectRef {
                class_sym: Some(SymbolId::new(1)),
            },
            DataCategory::ObjectReference,
            8,
        ),
        (
            HirType::Group {
                static_byte_len: 20,
                odo_presence: DynamicOdoPresence::StaticSize,
            },
            DataCategory::Group,
            20,
        ),
    ];

    for (ty, expected_cat, expected_bytes) in cases {
        assert_eq!(ty.category(), expected_cat);
        assert_eq!(ty.static_byte_size(), expected_bytes);
    }
}

/// Exercises [`ExceptionCheckMask`] bit constructors, union, and flag queries.
#[test]
fn exception_check_mask_operations() {
    let empty = ExceptionCheckMask::NONE;
    assert_eq!(empty.bits(), 0);
    let combined = ExceptionCheckMask::BOUND_SUBSCRIPT
        .union(ExceptionCheckMask::BOUND_ODO)
        .union(ExceptionCheckMask::BOUND_REF_MOD)
        .union(ExceptionCheckMask::SIZE_ERROR)
        .union(ExceptionCheckMask::BOUND_FATAL);
    assert_eq!(ExceptionCheckMask::from_bits(combined.bits()), combined);
    assert!(combined.contains(ExceptionCheckMask::BOUND_SUBSCRIPT));
    let cleared = combined.without(ExceptionCheckMask::BOUND_FATAL);
    assert!(!cleared.contains(ExceptionCheckMask::BOUND_FATAL));
    assert!(!empty.contains(ExceptionCheckMask::SIZE_ERROR));
}
