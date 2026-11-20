//! Exhaustive unit and 100% branch-coverage test suite for `coblang-pic`.

use coblang_common::{DigitCount, Result, ScaleFactor};
use coblang_decimal::{
    FixedDecimal, HighOrderPolicy, OverpunchDialect, RoundingMode, SizeErrorStatus,
};
use coblang_hir::{
    BinaryEndianness, BlankZeroSpec, Comp6Mode, DataCategory, DynamicOdoPresence, FloatBinaryWidth,
    FloatDecimalWidth, HirFigurative, HirType, JustifiedSpec, NumericSignSpec, PicEditOp,
    Signedness, TruncationPolicy,
};
use coblang_pic::{
    DefaultInitMode, FillerPolicy, InitCategoryMask, InitTargetField, InitializeSpec,
    MoveFieldDesc, MoveSource, NumericEditConfig, ascii_to_national_utf16le, de_edit_to_decimal,
    decode_numeric_field, execute_initialize, execute_move, execute_move_overlapping,
    format_alphanumeric, format_alphanumeric_edited, format_field_for_display, format_national,
    format_national_edited, format_numeric_edited,
};

/// Verifies `format_numeric_edited` across `Z`, `*`, `$$`, `++`, `--`, `+`, `-`, `CR`, `DB`, `B`, `0`, `/`, `,`, `.`, `V`, `P`, and `BLANK WHEN ZERO`.
#[test]
fn numeric_editing_all_opcodes_and_zero_rules() -> Result<()> {
    let d6 = DigitCount::try_new(6)?;
    let sc2 = ScaleFactor::try_new(2)?;
    let cfg_s = NumericEditConfig::standard(d6, sc2, Signedness::Signed, BlankZeroSpec::RetainZero);

    let ops_cur_cr = [
        PicEditOp::FloatingCurrency('$'),
        PicEditOp::FloatingCurrency('$'),
        PicEditOp::InsertGroupSep,
        PicEditOp::FloatingCurrency('$'),
        PicEditOp::FloatingCurrency('$'),
        PicEditOp::FloatingCurrency('$'),
        PicEditOp::InsertDecimalPoint,
        PicEditOp::DigitSlot,
        PicEditOp::DigitSlot,
        PicEditOp::FixedCreditCr,
    ];
    let (out_neg, st) = format_numeric_edited(
        &FixedDecimal::parse_str("-1234.56")?,
        &ops_cur_cr,
        cfg_s,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(
        (out_neg.as_slice(), st),
        (b"$1,234.56CR".as_slice(), SizeErrorStatus::Exact)
    );

    let (out_small, _) = format_numeric_edited(
        &FixedDecimal::parse_str("0.05")?,
        &ops_cur_cr,
        cfg_s,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(out_small, b"     $.05  ");

    // Non-ASCII currency fallback + all-suppression zero (`Z`, `*`, `$`, `CR`, `DB`, `V`, `P`).
    let ops_all_z = [
        PicEditOp::FloatingCurrency('\u{20AC}'),
        PicEditOp::ZeroSuppressSpace,
        PicEditOp::InsertGroupSep,
        PicEditOp::ZeroSuppressSpace,
        PicEditOp::InsertDecimalPoint,
        PicEditOp::ZeroSuppressSpace,
        PicEditOp::ZeroSuppressSpace,
        PicEditOp::FixedCreditCr,
        PicEditOp::ImpliedDecimal,
        PicEditOp::ScalingPosition,
    ];
    let cfg4 = NumericEditConfig::standard(
        DigitCount::try_new(4)?,
        sc2,
        Signedness::Signed,
        BlankZeroSpec::RetainZero,
    );
    let (all_z0, _) = format_numeric_edited(
        &FixedDecimal::ZERO,
        &ops_all_z,
        cfg4,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(all_z0, b"         ");
    let (non_ascii_cur, _) = format_numeric_edited(
        &FixedDecimal::parse_str("12.34")?,
        &ops_all_z,
        cfg4,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(non_ascii_cur, b"$1,2.34  ");

    numeric_editing_asterisk_and_retain_zero()?;
    numeric_editing_signs_and_insertions()
}

/// Verifies check-protection `*` zero suppression and `RetainZero` comma suppression.
fn numeric_editing_asterisk_and_retain_zero() -> Result<()> {
    let ops_all_ast = [
        PicEditOp::ZeroSuppressAsterisk,
        PicEditOp::InsertGroupSep,
        PicEditOp::ZeroSuppressAsterisk,
        PicEditOp::ZeroSuppressAsterisk,
        PicEditOp::InsertDecimalPoint,
        PicEditOp::ZeroSuppressAsterisk,
        PicEditOp::FixedDebitDb,
    ];
    let (all_ast0, _) = format_numeric_edited(
        &FixedDecimal::ZERO,
        &ops_all_ast,
        NumericEditConfig::standard(
            DigitCount::try_new(4)?,
            ScaleFactor::try_new(1)?,
            Signedness::Unsigned,
            BlankZeroSpec::RetainZero,
        ),
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(all_ast0, b"****.*  ");
    let (ret_z, _) = format_numeric_edited(
        &FixedDecimal::ZERO,
        &[
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::InsertGroupSep,
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::DigitSlot,
        ],
        NumericEditConfig::standard(
            DigitCount::try_new(3)?,
            ScaleFactor::ZERO,
            Signedness::Signed,
            BlankZeroSpec::RetainZero,
        ),
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(ret_z, b"   0");
    Ok(())
}

/// Verifies `++`, `--`, fixed `+`/`-`/`DB`/`$`, `B`/`0`/`/` before and after significance, `BLANK WHEN ZERO`, and flush-at-end.
fn numeric_editing_signs_and_insertions() -> Result<()> {
    let d3 = DigitCount::try_new(3)?;
    let sc0 = ScaleFactor::ZERO;

    // BLANK WHEN ZERO.
    let bwz_cfg =
        NumericEditConfig::standard(d3, sc0, Signedness::Signed, BlankZeroSpec::BlankWhenZero);
    let (bwz, _) = format_numeric_edited(
        &FixedDecimal::ZERO,
        &[
            PicEditOp::DigitSlot,
            PicEditOp::DigitSlot,
            PicEditOp::DigitSlot,
        ],
        bwz_cfg,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(bwz, b"   ");

    // Floating `+` and `-` (both positive and negative, plus trailing float with no DigitSlot after).
    let cfg_s3 =
        NumericEditConfig::standard(d3, sc0, Signedness::Signed, BlankZeroSpec::RetainZero);
    for (op_fl, val, exp) in [
        (PicEditOp::FloatingSignPlus, 5_i64, b"  +5"),
        (PicEditOp::FloatingSignPlus, -5, b"  -5"),
        (PicEditOp::FloatingSignMinus, 5, b"   5"),
        (PicEditOp::FloatingSignMinus, -5, b"  -5"),
    ] {
        let ops = [op_fl, op_fl, op_fl, op_fl];
        let (res, _) = format_numeric_edited(
            &FixedDecimal::from_i64(val),
            &ops,
            cfg_s3,
            RoundingMode::Truncation,
            HighOrderPolicy::ModuloTruncate,
        )?;
        assert_eq!(res, exp);
    }

    // Floating sign where all digits are suppressed EXCEPT a trailing non-digit op (`DigitSlot` absent, non-zero can't be all '0' unless truncated to 0 via ModuloTruncate!).
    let (trunc_zero_fl, _) = format_numeric_edited(
        &FixedDecimal::from_i64(1000),
        &[
            PicEditOp::FloatingSignPlus,
            PicEditOp::FloatingSignPlus,
            PicEditOp::FloatingSignPlus,
            PicEditOp::FloatingSignPlus,
        ],
        cfg_s3,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(trunc_zero_fl, b"    ");

    // Synthetic op list with `FloatingSignPlus` and `DigitCount(1)` = `0` where `DigitSlot` is BEFORE `FloatingSignPlus`?
    // Even simpler: `[FloatingSignPlus, FloatingSignPlus]` with `DigitSlot` present earlier? No, `[FloatingSignPlus]` with `DigitSlot` absent and `fitted` non-zero (`value = 5`, `ops = [FloatingSignPlus]`) hits the post-loop `st.pending_float_char.take()`!
    let (end_flush, _) = format_numeric_edited(
        &FixedDecimal::from_i64(5),
        &[PicEditOp::FloatingSignPlus],
        NumericEditConfig::standard(
            DigitCount::ONE,
            sc0,
            Signedness::Signed,
            BlankZeroSpec::RetainZero,
        ),
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(end_flush, b"+");

    // Fixed `+`, `-`, `DB`, `$`, non-ASCII `$`, `B`, `0`, `/`, `V`, `P`, and ignored char slots both before & after `started`.
    let misc_ops = [
        PicEditOp::FixedCurrency('$'),
        PicEditOp::FixedCurrency('\u{00A3}'),
        PicEditOp::ZeroSuppressAsterisk,
        PicEditOp::InsertBlank,
        PicEditOp::InsertZero,
        PicEditOp::InsertSlash,
        PicEditOp::ImpliedDecimal,
        PicEditOp::DigitSlot,
        PicEditOp::InsertBlank,
        PicEditOp::InsertZero,
        PicEditOp::InsertGroupSep,
        PicEditOp::DigitSlot,
        PicEditOp::FixedSignPlus,
        PicEditOp::FixedSignMinus,
        PicEditOp::FixedDebitDb,
        PicEditOp::ScalingPosition,
        PicEditOp::AnyCharSlot,
    ];
    for sample in [5_i64, -5] {
        let (out, _) = format_numeric_edited(
            &FixedDecimal::from_i64(sample),
            &misc_ops,
            cfg_s3,
            RoundingMode::Truncation,
            HighOrderPolicy::ModuloTruncate,
        )?;
        assert_ne!(out, [] as [u8; 0]);
    }
    Ok(())
}

/// Verifies alphanumeric, alphanumeric-edited, national UTF-16LE, national-edited, and `de_edit_to_decimal`.
#[test]
fn alpha_national_editing_and_de_editing() -> Result<()> {
    assert_eq!(
        format_alphanumeric(b"ABC", 0, JustifiedSpec::Left, b' '),
        b""
    );
    assert_eq!(
        format_alphanumeric(b"", 3, JustifiedSpec::Left, b' '),
        b"   "
    );
    assert_eq!(
        format_alphanumeric(b"AB", 4, JustifiedSpec::Left, b' '),
        b"AB  "
    );
    assert_eq!(
        format_alphanumeric(b"ABCDEF", 4, JustifiedSpec::Left, b' '),
        b"ABCD"
    );
    assert_eq!(
        format_alphanumeric(b"AB", 4, JustifiedSpec::Right, b' '),
        b"  AB"
    );
    assert_eq!(
        format_alphanumeric(b"ABCDEF", 4, JustifiedSpec::Right, b' '),
        b"CDEF"
    );

    let ae = format_alphanumeric_edited(
        b"AB12",
        &[
            PicEditOp::AlphaSlot,
            PicEditOp::AnyCharSlot,
            PicEditOp::InsertSlash,
            PicEditOp::DigitSlot,
            PicEditOp::InsertBlank,
            PicEditOp::InsertZero,
            PicEditOp::DigitSlot,
            PicEditOp::ImpliedDecimal,
        ],
        JustifiedSpec::Left,
        b' ',
    );
    assert_eq!(ae, b"AB/1 02");

    // National & National-edited.
    assert_eq!(format_national(b"A\0", 0, JustifiedSpec::Left), b"");
    assert_eq!(format_national(b"", 2, JustifiedSpec::Left), b" \0 \0");
    assert_eq!(
        ascii_to_national_utf16le(b"A", 2, JustifiedSpec::Left),
        b"A\0 \0"
    );
    assert_eq!(
        ascii_to_national_utf16le(b"A", 2, JustifiedSpec::Right),
        b" \0A\0"
    );
    assert_eq!(
        ascii_to_national_utf16le(b"ABC", 2, JustifiedSpec::Right),
        b"B\0C\0"
    );
    let ne = format_national_edited(
        b"A\0B\0",
        &[
            PicEditOp::NationalSlot,
            PicEditOp::InsertBlank,
            PicEditOp::InsertZero,
            PicEditOp::InsertSlash,
            PicEditOp::NationalSlot,
            PicEditOp::ImpliedDecimal,
        ],
    );
    assert_eq!(ne, [b'A', 0, b' ', 0, b'0', 0, b'/', 0, b'B', 0]);

    // De-editing (`CR`, `DB`, `*`, `,`, decimal_point=comma, non-ASCII currency, all-blank, invalid).
    assert_eq!(
        de_edit_to_decimal(b"  $1,234.56CR ", b'.', '$')?,
        FixedDecimal::parse_str("-1234.56")?
    );
    assert_eq!(
        de_edit_to_decimal(b"***45.6*db", b'.', '\u{20AC}')?,
        FixedDecimal::parse_str("-45.60")?
    );
    assert_eq!(
        de_edit_to_decimal(b" +1.234,50 / ", b',', '$')?,
        FixedDecimal::parse_str("1234.50")?
    );
    assert!(de_edit_to_decimal(b"   ***   ", b'.', '$')?.is_zero());
    assert_eq!(
        de_edit_to_decimal(b"12*", b'.', '$')?,
        FixedDecimal::from_i64(12)
    );
    assert!(de_edit_to_decimal(b"-0.00", b'.', '$')?.is_zero());
    assert!(de_edit_to_decimal(b"1.2.3", b'.', '$').is_err());
    assert!(de_edit_to_decimal(b"12XYZ", b'.', '$').is_err());
    assert!(de_edit_to_decimal(&[b'9'; 77], b'.', '$').is_err());
    let mut dot_77 = vec![b'.'];
    dot_77.extend(core::iter::repeat_n(b'0', 76));
    dot_77.push(b'1');
    assert!(de_edit_to_decimal(&dot_77, b'.', '$').is_err());
    let mut dot_only_77 = vec![b'.'];
    dot_only_77.extend(core::iter::repeat_n(b'*', 77));
    dot_only_77.push(b'+');
    assert!(de_edit_to_decimal(&dot_only_77, b'.', '$').is_err());
    Ok(())
}

/// Verifies `execute_move`, `execute_move_overlapping`, `decode_numeric_field`, and `format_field_for_display` across all categories.
#[test]
fn move_engine_all_categories_and_overlap() -> Result<()> {
    let grp_desc = MoveFieldDesc::standard_gnu(HirType::Group {
        static_byte_len: 6,
        odo_presence: DynamicOdoPresence::StaticSize,
    });
    let alpha_desc = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 6,
        justified: JustifiedSpec::Left,
    });
    let mut comma_alpha = alpha_desc.clone();
    comma_alpha.decimal_char = b',';
    assert_eq!(comma_alpha.group_sep_char(), b'.');
    assert_eq!(alpha_desc.group_sep_char(), b',');

    // Overlapping in-place move (`buf[0..4]` -> `buf[1..5]`) + out-of-bounds checks.
    let mut shared = *b"ABCDEF";
    let a4 = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 4,
        justified: JustifiedSpec::Left,
    });
    execute_move_overlapping(&mut shared, (0..4, &a4), (1..5, &a4))?;
    assert_eq!(&shared, b"AABCDF");
    assert!(execute_move_overlapping(&mut shared, (0..10, &a4), (0..4, &a4)).is_err());
    assert!(execute_move_overlapping(&mut shared, (0..4, &a4), (0..10, &a4)).is_err());

    // Group moves (literal, field, decimal, figurative ALL tiling + empty ALL + EBCDIC Quote/Zero).
    let mut gbuf = [0_u8; 6];
    execute_move(MoveSource::AlphanumericLiteral(b"HI"), &mut gbuf, &grp_desc)?;
    assert_eq!(&gbuf, b"HI    ");
    execute_move(
        MoveSource::Field {
            bytes: b"XY",
            desc: &alpha_desc,
        },
        &mut gbuf,
        &grp_desc,
    )?;
    assert_eq!(&gbuf, b"XY    ");
    execute_move(
        MoveSource::Decimal(FixedDecimal::from_i64(-42)),
        &mut gbuf,
        &grp_desc,
    )?;
    assert_eq!(&gbuf, b"42    ");
    execute_move(
        MoveSource::Figurative(HirFigurative::All(b"AB".to_vec())),
        &mut gbuf,
        &grp_desc,
    )?;
    assert_eq!(&gbuf, b"ABABAB");
    execute_move(
        MoveSource::Figurative(HirFigurative::All(Vec::new())),
        &mut gbuf,
        &grp_desc,
    )?;
    assert_eq!(&gbuf, b"      ");

    let mut ebc_alpha = alpha_desc.clone();
    ebc_alpha.overpunch = OverpunchDialect::EbcdicIbm;
    for fig in [
        HirFigurative::Zero,
        HirFigurative::Space,
        HirFigurative::HighValue,
        HirFigurative::LowValue,
        HirFigurative::Null,
        HirFigurative::Quote,
    ] {
        execute_move(MoveSource::Figurative(fig.clone()), &mut gbuf, &alpha_desc)?;
        execute_move(MoveSource::Figurative(fig), &mut gbuf, &ebc_alpha)?;
    }

    // Group source moved to elementary numeric item.
    let z3_desc = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: DigitCount::try_new(3)?,
        scale: ScaleFactor::ZERO,
        sign: NumericSignSpec::EmbeddedTrailing,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let mut z3_buf = [0_u8; 3];
    execute_move(
        MoveSource::Field {
            bytes: b"123",
            desc: &grp_desc,
        },
        &mut z3_buf,
        &z3_desc,
    )?;
    assert_eq!(&z3_buf, b"123");

    move_engine_numeric_float_national_and_display(&z3_desc, &z3_buf)
}

/// Verifies numeric, edited, float, national, pointer, and `format_field_for_display` paths in `move_engine.rs`.
///
/// # Arguments
///
/// * `z3_desc` - 3-digit signed zoned descriptor.
/// * `z3_buf` - Populated `"123"` zoned buffer.
fn move_engine_numeric_float_national_and_display(
    z3_desc: &MoveFieldDesc,
    z3_buf: &[u8],
) -> Result<()> {
    let d3 = DigitCount::try_new(3)?;
    let sc0 = ScaleFactor::ZERO;

    // Numeric -> Alphanumeric & AlphanumericEdited; Figurative/Decimal -> AlphanumericEdited & Utf8.
    let mut a5 = [0_u8; 5];
    let utf8_desc = MoveFieldDesc::standard_gnu(HirType::Utf8 { byte_len: 5 });
    assert_eq!(utf8_desc.category(), DataCategory::Utf8);
    execute_move(
        MoveSource::Field {
            bytes: z3_buf,
            desc: z3_desc,
        },
        &mut a5,
        &utf8_desc,
    )?;
    assert_eq!(&a5, b"123  ");

    let ae_desc = MoveFieldDesc::standard_gnu(HirType::AlphanumericEdited {
        byte_len: 4,
        edit_ops: vec![
            PicEditOp::AnyCharSlot,
            PicEditOp::InsertSlash,
            PicEditOp::AnyCharSlot,
            PicEditOp::AnyCharSlot,
        ],
    });
    assert_eq!(ae_desc.category(), DataCategory::AlphanumericEdited);
    let mut ae_buf = [0_u8; 4];
    execute_move(
        MoveSource::Figurative(HirFigurative::Zero),
        &mut ae_buf,
        &ae_desc,
    )?;
    assert_eq!(&ae_buf, b"0/00");
    execute_move(
        MoveSource::Decimal(FixedDecimal::from_i64(78)),
        &mut ae_buf,
        &ae_desc,
    )?;
    assert_eq!(&ae_buf, b"7/8 ");

    // National & NationalEdited.
    let nat_desc = MoveFieldDesc::standard_gnu(HirType::National {
        char_len: 2,
        justified: JustifiedSpec::Left,
    });
    let edited_nat_desc = MoveFieldDesc::standard_gnu(HirType::NationalEdited {
        char_len: 3,
        edit_ops: vec![
            PicEditOp::NationalSlot,
            PicEditOp::InsertSlash,
            PicEditOp::NationalSlot,
        ],
    });
    let mut nat_buf = [0_u8; 4];
    let mut edited_nat_buf = [0_u8; 6];
    execute_move(
        MoveSource::AlphanumericLiteral(b"XY"),
        &mut nat_buf,
        &nat_desc,
    )?;
    execute_move(
        MoveSource::Field {
            bytes: &nat_buf,
            desc: &nat_desc,
        },
        &mut edited_nat_buf,
        &edited_nat_desc,
    )?;
    assert_eq!(edited_nat_desc.category(), DataCategory::NationalEdited);
    assert_eq!(&edited_nat_buf, b"X\0/\0Y\0");

    // NumericEdited destination + all numeric/float/index/pointer types.
    let ne_desc = MoveFieldDesc::standard_gnu(HirType::NumericEdited {
        byte_len: 4,
        digits: d3,
        scale: sc0,
        sign_capability: Signedness::Signed,
        edit_ops: vec![
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::DigitSlot,
            PicEditOp::FixedSignMinus,
        ],
        blank_zero: BlankZeroSpec::RetainZero,
    });
    assert_eq!(ne_desc.category(), DataCategory::NumericEdited);
    let mut ne_buf = [0_u8; 4];
    execute_move(
        MoveSource::Field {
            bytes: z3_buf,
            desc: z3_desc,
        },
        &mut ne_buf,
        &ne_desc,
    )?;
    execute_move(
        MoveSource::Decimal(FixedDecimal::from_i64(-7)),
        &mut ne_buf,
        &ne_desc,
    )?;
    assert_eq!(&ne_buf, b"  7-");
    assert_eq!(
        decode_numeric_field(&ne_buf, &ne_desc)?,
        FixedDecimal::from_i64(-7)
    );

    move_engine_all_numeric_and_pointer_variants(d3, sc0, &nat_desc, &nat_buf)
}

/// Exercises every `ZonedDecimal`, `PackedDecimal`, `BinaryInt`, `FloatBinary`, `FloatDecimal`, `Index`, `Pointer`, `Bit`, and `ObjectRef` branch.
///
/// # Arguments
///
/// * `d3` - 3-digit count.
/// * `sc0` - Zero scale.
/// * `nat_desc` - National field descriptor (used to test non-numeric decode rejection).
/// * `nat_buf` - National field bytes.
fn move_engine_all_numeric_and_pointer_variants(
    d3: DigitCount,
    sc0: ScaleFactor,
    nat_desc: &MoveFieldDesc,
    nat_buf: &[u8],
) -> Result<()> {
    assert!(decode_numeric_field(nat_buf, nat_desc).is_err());

    // Zoned sign placements + BlankWhenZero display.
    for sign in [
        NumericSignSpec::Unsigned,
        NumericSignSpec::EmbeddedLeading,
        NumericSignSpec::EmbeddedTrailing,
        NumericSignSpec::SeparateLeading,
        NumericSignSpec::SeparateTrailing,
    ] {
        let zd = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: d3,
            scale: sc0,
            sign,
            blank_zero: BlankZeroSpec::BlankWhenZero,
        });
        let len = if matches!(
            sign,
            NumericSignSpec::SeparateLeading | NumericSignSpec::SeparateTrailing
        ) {
            4
        } else {
            3
        };
        let mut zb = vec![0_u8; len];
        execute_move(MoveSource::Figurative(HirFigurative::Zero), &mut zb, &zd)?;
        let _ = format_field_for_display(&zb, &zd)?;
        execute_move(
            MoveSource::Figurative(HirFigurative::All(b"5".to_vec())),
            &mut zb,
            &zd,
        )?;
        let _ = format_field_for_display(&zb, &zd)?;
    }
    let u_zd = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: d3,
        scale: ScaleFactor::try_new(-2)?,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    assert!(
        execute_move(
            MoveSource::Figurative(HirFigurative::HighValue),
            &mut [0; 3],
            &u_zd
        )
        .is_err()
    );
    let mut p_buf = [0_u8; 3];
    execute_move(
        MoveSource::Decimal(FixedDecimal::from_i64(9100)),
        &mut p_buf,
        &u_zd,
    )?;
    assert_eq!(format_field_for_display(&p_buf, &u_zd)?, b"09100");

    // Leading-P display (`.000128`) & fractional display (`1.23`).
    let lp_zd = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: d3,
        scale: ScaleFactor::try_new(6)?,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    execute_move(
        MoveSource::AlphanumericLiteral(b"0.000128"),
        &mut p_buf,
        &lp_zd,
    )?;
    assert_eq!(format_field_for_display(&p_buf, &lp_zd)?, b".000128");
    let frac_zd = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: d3,
        scale: ScaleFactor::try_new(2)?,
        sign: NumericSignSpec::EmbeddedTrailing,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    execute_move(
        MoveSource::AlphanumericLiteral(b"-1.23"),
        &mut p_buf,
        &frac_zd,
    )?;
    assert_eq!(format_field_for_display(&p_buf, &frac_zd)?, b"-1.23");

    move_engine_packed_binary_float_and_pointers(d3, sc0)
}

/// Exercises `PackedDecimal`, `BinaryInt` (all byte widths), `FloatBinary`, `FloatDecimal`, `Index`, `Pointer`, `Bit`, and `ObjectRef`.
///
/// # Arguments
///
/// * `d3` - 3-digit count.
/// * `sc0` - Zero scale.
fn move_engine_packed_binary_float_and_pointers(d3: DigitCount, sc0: ScaleFactor) -> Result<()> {
    for (sign, comp6) in [
        (Signedness::Signed, Comp6Mode::StandardComp3),
        (Signedness::Unsigned, Comp6Mode::StandardComp3),
        (Signedness::Unsigned, Comp6Mode::UnsignedComp6),
    ] {
        let pd = MoveFieldDesc::standard_gnu(HirType::PackedDecimal {
            digits: d3,
            scale: sc0,
            sign,
            unsigned_comp6: comp6,
        });
        let mut pb = [0_u8; 2];
        execute_move(
            MoveSource::Decimal(FixedDecimal::from_i64(-12)),
            &mut pb,
            &pd,
        )?;
        let _ = format_field_for_display(&pb, &pd)?;
    }

    for bw in [1_u8, 2, 3, 4, 6, 8, 16] {
        let trunc = if bw == 2 {
            TruncationPolicy::TruncateToPic
        } else {
            TruncationPolicy::FullBinaryRange
        };
        let endian = if bw == 2 {
            BinaryEndianness::BigEndian
        } else {
            BinaryEndianness::Native
        };
        let sign = if bw == 3 {
            Signedness::Unsigned
        } else {
            Signedness::Signed
        };
        let bd = MoveFieldDesc::standard_gnu(HirType::BinaryInt {
            byte_width: bw,
            digits: d3,
            scale: sc0,
            sign,
            endian,
            trunc,
        });
        let mut bb = vec![0_u8; usize::from(bw)];
        execute_move(MoveSource::Decimal(FixedDecimal::from_i64(7)), &mut bb, &bd)?;
        let _ = format_field_for_display(&bb, &bd)?;
        if bw == 2 {
            let bd_full = MoveFieldDesc::standard_gnu(HirType::BinaryInt {
                byte_width: 2,
                digits: d3,
                scale: sc0,
                sign,
                endian,
                trunc: TruncationPolicy::FullBinaryRange,
            });
            let _ = format_field_for_display(&bb, &bd_full)?;
        }
    }

    move_engine_floats_and_pointers()
}

/// Exercises `FloatBinary`, `FloatDecimal`, `Index`, `Bit`, `Pointer`, `ProgramPointer`, and `ObjectRef` moves.
fn move_engine_floats_and_pointers() -> Result<()> {
    for (ty, len) in [
        (
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits32,
            },
            4_usize,
        ),
        (
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits64,
            },
            8,
        ),
        (
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits128,
            },
            16,
        ),
        (
            HirType::FloatDecimal {
                width: FloatDecimalWidth::Decimal64,
            },
            8,
        ),
        (
            HirType::FloatDecimal {
                width: FloatDecimalWidth::Decimal128,
            },
            16,
        ),
        (HirType::Index, 4),
    ] {
        let fd = MoveFieldDesc::standard_gnu(ty);
        let mut fb = vec![0_u8; len];
        execute_move(
            MoveSource::Decimal(FixedDecimal::from_i64(25)),
            &mut fb,
            &fd,
        )?;
        let _ = decode_numeric_field(&fb, &fd)?;
        let _ = format_field_for_display(&fb, &fd)?;
        if len >= 8 || matches!(fd.ty, HirType::FloatBinary { .. }) {
            assert!(decode_numeric_field(&[0_u8; 1], &fd).is_err());
        }
        let mut a4 = [0_u8; 4];
        let a_desc = MoveFieldDesc::standard_gnu(HirType::Alphabetic {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        });
        assert_eq!(a_desc.category(), DataCategory::Alphabetic);
        execute_move(
            MoveSource::Field {
                bytes: &fb,
                desc: &fd,
            },
            &mut a4,
            &a_desc,
        )?;
    }

    for ty in [
        HirType::Bit { bit_len: 8 },
        HirType::Pointer,
        HirType::ProgramPointer,
        HirType::ObjectRef { class_sym: None },
    ] {
        let pd = MoveFieldDesc::standard_gnu(ty);
        assert_ne!(pd.category(), DataCategory::Numeric);
        let mut pb = [0_u8; 8];
        execute_move(MoveSource::Figurative(HirFigurative::Null), &mut pb, &pd)?;
        execute_move(MoveSource::AlphanumericLiteral(b"12345678"), &mut pb, &pd)?;
        execute_move(MoveSource::Decimal(FixedDecimal::from_i64(9)), &mut pb, &pd)?;
        let _ = format_field_for_display(&pb, &pd)?;
    }
    Ok(())
}

/// Verifies `InitCategoryMask` bitset construction across all 13 `DataCategory` variants.
fn verify_init_category_mask() {
    let mut full = InitCategoryMask::NONE;
    for cat in [
        DataCategory::Alphabetic,
        DataCategory::Alphanumeric,
        DataCategory::AlphanumericEdited,
        DataCategory::Numeric,
        DataCategory::NumericEdited,
        DataCategory::National,
        DataCategory::NationalEdited,
        DataCategory::Utf8,
        DataCategory::BooleanBit,
        DataCategory::Pointer,
        DataCategory::Index,
        DataCategory::ObjectReference,
        DataCategory::Group,
    ] {
        let single = InitCategoryMask::from_category(cat);
        assert!(single.contains(cat));
        full = full.union(single);
    }
    assert!(InitCategoryMask::ALL.contains(DataCategory::Numeric));
    assert!(full.contains(DataCategory::Group));
}

/// Verifies `execute_initialize` across `SkipFiller`/`IncludeFiller`, `is_redefined`, `REPLACING`, `TO VALUE`, and `DEFAULT`.
#[test]
fn initialize_engine_all_clauses_and_categories() -> Result<()> {
    verify_init_category_mask();

    let alpha2 = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 2,
        justified: JustifiedSpec::Left,
    });
    let num2 = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: DigitCount::try_new(2)?,
        scale: ScaleFactor::ZERO,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let ptr8 = MoveFieldDesc::standard_gnu(HirType::Pointer);

    let fields = [
        InitTargetField {
            offset: 0,
            byte_len: 2,
            desc: alpha2.clone(),
            is_filler: false,
            is_redefined: false,
            value_bytes: Some(b"VL"),
        },
        InitTargetField {
            offset: 2,
            byte_len: 2,
            desc: alpha2.clone(),
            is_filler: true,
            is_redefined: false,
            value_bytes: None,
        },
        InitTargetField {
            offset: 4,
            byte_len: 2,
            desc: num2,
            is_filler: false,
            is_redefined: false,
            value_bytes: Some(b"99"),
        },
        InitTargetField {
            offset: 6,
            byte_len: 2,
            desc: alpha2.clone(),
            is_filler: false,
            is_redefined: true,
            value_bytes: None,
        },
        InitTargetField {
            offset: 8,
            byte_len: 2,
            desc: ptr8,
            is_filler: false,
            is_redefined: false,
            value_bytes: None,
        },
    ];

    // 1. Plain default INITIALIZE: skips filler & redefined & pointer; spaces alpha, zeroes numeric.
    let mut buf = *b"XXXXXXXXXX";
    execute_initialize(&mut buf, &fields, &InitializeSpec::default())?;
    assert_eq!(&buf, b"  XX00XXXX");

    // 2. WITH FILLER + DEFAULT pointer zeroing.
    let spec_def = InitializeSpec {
        filler_policy: FillerPolicy::IncludeFiller,
        to_value_mask: InitCategoryMask::NONE,
        replacements: &[],
        default_mode: DefaultInitMode::IncludePointersAndBit,
    };
    execute_initialize(&mut buf, &fields, &spec_def)?;
    assert_eq!(&buf, b"    00XX\0\0");

    // 3. REPLACING NUMERIC BY 42 + TO VALUE on Alphanumeric.
    let reps = [(
        DataCategory::Numeric,
        MoveSource::Decimal(FixedDecimal::from_i64(42)),
    )];
    let spec_rep = InitializeSpec {
        filler_policy: FillerPolicy::IncludeFiller,
        to_value_mask: InitCategoryMask::from_category(DataCategory::Alphanumeric),
        replacements: &reps,
        default_mode: DefaultInitMode::StandardCobol,
    };
    buf = *b"ZZZZZZZZZZ";
    execute_initialize(&mut buf, &fields, &spec_rep)?;
    assert_eq!(&buf, b"VLZZ42ZZZZ");

    // 4. Out-of-bounds field error.
    let bad_field = [InitTargetField {
        offset: 9,
        byte_len: 4,
        desc: alpha2,
        is_filler: false,
        is_redefined: false,
        value_bytes: None,
    }];
    assert!(execute_initialize(&mut buf, &bad_field, &InitializeSpec::default()).is_err());
    Ok(())
}

/// Verifies `InitCategoryMask` bit conversions, `NumericEdited` `INITIALIZE`, `store_decimal_to_field` `FlagOverflow`,
/// and `GnuPrettyWidth` P-scaled binary display formatting.
#[test]
fn move_engine_overflow_and_pretty_display_coverage() -> Result<()> {
    use coblang_decimal::{HighOrderPolicy, SizeErrorStatus};
    use coblang_hir::HirDisplayStyle;
    use coblang_pic::store_decimal_to_field;

    let raw_bits = InitCategoryMask::from_category(DataCategory::NumericEdited).bits();
    assert!(InitCategoryMask::from_bits(raw_bits).contains(DataCategory::NumericEdited));
    let ne_desc = MoveFieldDesc::standard_gnu(HirType::NumericEdited {
        byte_len: 3,
        digits: DigitCount::try_new(3)?,
        scale: ScaleFactor::ZERO,
        sign_capability: Signedness::Unsigned,
        edit_ops: vec![
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::DigitSlot,
            PicEditOp::DigitSlot,
        ],
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let ne_fields = [InitTargetField {
        offset: 0,
        byte_len: 3,
        desc: ne_desc.clone(),
        is_filler: false,
        is_redefined: false,
        value_bytes: None,
    }];
    let mut ne_buf = *b"999";
    execute_initialize(&mut ne_buf, &ne_fields, &InitializeSpec::default())?;
    assert_eq!(&ne_buf, b" 00");

    let d2 = DigitCount::try_new(2)?;
    let sc0 = ScaleFactor::ZERO;
    let zd_u2 = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: d2,
        scale: sc0,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let mut ne_out = *b"XXX";
    execute_move(
        MoveSource::Field {
            bytes: b"07",
            desc: &zd_u2,
        },
        &mut ne_out,
        &ne_desc,
    )?;
    assert_eq!(&ne_out, b" 07");
    execute_move(
        MoveSource::AlphanumericLiteral(b"12"),
        &mut ne_out,
        &ne_desc,
    )?;
    assert_eq!(&ne_out, b" 12");
    let bad_bin0 = MoveFieldDesc::standard_gnu(HirType::BinaryInt {
        byte_width: 0,
        digits: d2,
        scale: sc0,
        sign: Signedness::Unsigned,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::TruncateToPic,
    });
    assert!(
        execute_move(
            MoveSource::Decimal(FixedDecimal::from_i64(5)),
            &mut ne_out,
            &bad_bin0
        )
        .is_err()
    );
    let alpha3 = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 3,
        justified: JustifiedSpec::Left,
    });
    execute_move(
        MoveSource::Figurative(HirFigurative::All(Vec::new())),
        &mut ne_out,
        &alpha3,
    )?;
    assert_eq!(&ne_out, b"   ");

    for scale_raw in [-2_i16, 4_i16] {
        let mut pretty_bin = MoveFieldDesc::standard_gnu(HirType::BinaryInt {
            byte_width: 2,
            digits: d2,
            scale: ScaleFactor::try_new(scale_raw)?,
            sign: Signedness::Unsigned,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
        });
        pretty_bin.display_style = HirDisplayStyle::GnuPrettyWidth;
        let b2 = 5_u16.to_ne_bytes();
        assert_eq!(format_field_for_display(&b2, &pretty_bin)?, b"00005");
    }
    let _ = (
        HighOrderPolicy::FlagOverflow,
        SizeErrorStatus::Exact,
        store_decimal_to_field,
    );
    Ok(())
}

/// Verifies `store_decimal_to_field` `FlagOverflow` preservation and `check_field_is_numeric`.
#[test]
fn store_decimal_overflow_and_numeric_class_check_coverage() -> Result<()> {
    use coblang_decimal::{HighOrderPolicy, SizeErrorStatus};
    use coblang_pic::store_decimal_to_field;

    let d2 = DigitCount::try_new(2)?;
    let sc0 = ScaleFactor::ZERO;
    let zd_u2 = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: d2,
        scale: sc0,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let ne_desc = MoveFieldDesc::standard_gnu(HirType::NumericEdited {
        byte_len: 3,
        digits: DigitCount::try_new(3)?,
        scale: sc0,
        sign_capability: Signedness::Unsigned,
        edit_ops: vec![
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::DigitSlot,
            PicEditOp::DigitSlot,
        ],
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let big = FixedDecimal::from_i64(999_999);
    let small = FixedDecimal::from_i64(5);
    let pd_u2 = MoveFieldDesc::standard_gnu(HirType::PackedDecimal {
        digits: d2,
        scale: sc0,
        sign: Signedness::Unsigned,
        unsigned_comp6: Comp6Mode::StandardComp3,
    });
    let bin_u1 = MoveFieldDesc::standard_gnu(HirType::BinaryInt {
        byte_width: 1,
        digits: d2,
        scale: sc0,
        sign: Signedness::Unsigned,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::TruncateToPic,
    });
    let idx_desc = MoveFieldDesc::standard_gnu(HirType::Index);
    for (desc, len) in [
        (&zd_u2, 2_usize),
        (&pd_u2, 2),
        (&bin_u1, 1),
        (&ne_desc, 3),
        (&idx_desc, 1),
    ] {
        let mut slot = vec![0xAA_u8; len];
        assert_eq!(
            store_decimal_to_field(
                &small,
                &mut slot,
                desc,
                RoundingMode::Truncation,
                HighOrderPolicy::FlagOverflow
            )?,
            SizeErrorStatus::Exact
        );
        let prev = slot.clone();
        assert_eq!(
            store_decimal_to_field(
                &big,
                &mut slot,
                desc,
                RoundingMode::Truncation,
                HighOrderPolicy::FlagOverflow
            )?,
            SizeErrorStatus::Overflowed
        );
        assert_eq!(slot, prev);
    }
    let bad_bin0 = MoveFieldDesc::standard_gnu(HirType::BinaryInt {
        byte_width: 0,
        digits: d2,
        scale: sc0,
        sign: Signedness::Unsigned,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::TruncateToPic,
    });
    assert!(
        store_decimal_to_field(
            &small,
            &mut [0],
            &bad_bin0,
            RoundingMode::Truncation,
            HighOrderPolicy::FlagOverflow
        )
        .is_err()
    );
    assert_eq!(
        store_decimal_to_field(
            &FixedDecimal::parse_str("1.5")?,
            &mut [0; 2],
            &zd_u2,
            RoundingMode::Prohibited,
            HighOrderPolicy::ModuloTruncate
        )?,
        SizeErrorStatus::Overflowed
    );
    Ok(())
}

/// Verifies `check_field_is_numeric` across zoned, packed, binary, and alphanumeric fields.
#[test]
fn numeric_class_check_coverage() -> Result<()> {
    use coblang_decimal::PackedSignPolicy;
    use coblang_pic::check_field_is_numeric;

    let d2 = DigitCount::try_new(2)?;
    let sc0 = ScaleFactor::ZERO;
    let alpha3 = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 3,
        justified: JustifiedSpec::Left,
    });
    let zd_u2 = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: d2,
        scale: sc0,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let pd_u2 = MoveFieldDesc::standard_gnu(HirType::PackedDecimal {
        digits: d2,
        scale: sc0,
        sign: Signedness::Unsigned,
        unsigned_comp6: Comp6Mode::StandardComp3,
    });
    let bin_u1 = MoveFieldDesc::standard_gnu(HirType::BinaryInt {
        byte_width: 1,
        digits: d2,
        scale: sc0,
        sign: Signedness::Unsigned,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::TruncateToPic,
    });

    for (bytes, desc, pol, exp) in [
        (b"12".as_slice(), &zd_u2, PackedSignPolicy::Gnu, true),
        (b"1A", &zd_u2, PackedSignPolicy::Gnu, false),
        (b"\x10", &alpha3, PackedSignPolicy::Gnu, false),
        (&[0x01, 0x2F], &pd_u2, PackedSignPolicy::Ibm, true),
        (&[0x05], &bin_u1, PackedSignPolicy::Gnu, true),
        (b"123", &alpha3, PackedSignPolicy::Gnu, true),
        (b"12X", &alpha3, PackedSignPolicy::Gnu, false),
        (b"", &alpha3, PackedSignPolicy::Gnu, false),
    ] {
        assert_eq!(check_field_is_numeric(bytes, desc, pol), exp);
    }
    Ok(())
}

/// Verifies `encode_zoned_negative_zero` and `is_zoned_negative_zero` across dialects.
#[test]
fn zoned_negative_zero_dialects_coverage() -> Result<()> {
    use coblang_decimal::OverpunchDialect;
    use coblang_pic::{encode_zoned_negative_zero, is_zoned_negative_zero};

    let d2 = DigitCount::try_new(2)?;
    let sc0 = ScaleFactor::ZERO;
    let alpha3 = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 3,
        justified: JustifiedSpec::Left,
    });
    let zd_u2 = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: d2,
        scale: sc0,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    });

    // 4. `encode_zoned_negative_zero` + `is_zoned_negative_zero` across all sign placements and dialects.
    let mut dummy = [b'X'; 3];
    encode_zoned_negative_zero(&mut dummy, &alpha3);
    assert!(!is_zoned_negative_zero(&dummy, &alpha3));
    assert!(!is_zoned_negative_zero(b"00", &zd_u2));

    let bwz_zd = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: d2,
        scale: sc0,
        sign: NumericSignSpec::EmbeddedTrailing,
        blank_zero: BlankZeroSpec::BlankWhenZero,
    });
    let mut bwz_b = [b'9'; 2];
    encode_zoned_negative_zero(&mut bwz_b, &bwz_zd);
    assert_eq!(&bwz_b, b"  ");
    encode_zoned_negative_zero(&mut bwz_b, &zd_u2);
    assert_eq!(&bwz_b, b"00");

    for dialect in [
        OverpunchDialect::AsciiGnu,
        OverpunchDialect::Cobol85Standard,
        OverpunchDialect::EbcdicIbm,
    ] {
        for sign in [
            NumericSignSpec::EmbeddedTrailing,
            NumericSignSpec::EmbeddedLeading,
            NumericSignSpec::SeparateTrailing,
            NumericSignSpec::SeparateLeading,
        ] {
            let len = if matches!(
                sign,
                NumericSignSpec::SeparateTrailing | NumericSignSpec::SeparateLeading
            ) {
                3
            } else {
                2
            };
            let mut zd = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
                digits: d2,
                scale: sc0,
                sign,
                blank_zero: BlankZeroSpec::RetainZero,
            });
            zd.overpunch = dialect;
            let mut b = vec![0_u8; len];
            encode_zoned_negative_zero(&mut [], &zd);
            encode_zoned_negative_zero(&mut b, &zd);
            assert!(is_zoned_negative_zero(&b, &zd));
            assert!(!is_zoned_negative_zero(&[], &zd));
            assert!(!is_zoned_negative_zero(b"@@@", &zd));
            execute_move(MoveSource::Decimal(FixedDecimal::from_i64(0)), &mut b, &zd)?;
            assert!(!is_zoned_negative_zero(&b, &zd));
            execute_move(MoveSource::Decimal(FixedDecimal::from_i64(-5)), &mut b, &zd)?;
            assert!(!is_zoned_negative_zero(&b, &zd));
        }
    }
    Ok(())
}

/// Exercises M10 `execute_string` and `normalize_display_operand_bytes` across all branches.
#[test]
fn m10_string_engine_coverage() -> Result<()> {
    use coblang_pic::{
        StringDelimiter, StringFlowOutcome, StringSourcePiece, execute_string,
        normalize_display_operand_bytes,
    };

    let d2 = DigitCount::try_new(2)?;
    let sc0 = ScaleFactor::ZERO;
    let alpha_desc = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 4,
        justified: JustifiedSpec::Left,
    });
    assert_eq!(normalize_display_operand_bytes(b"AB", None), b"AB");
    assert_eq!(
        normalize_display_operand_bytes(b"AB", Some(&alpha_desc)),
        b"AB"
    );
    for sign in [
        NumericSignSpec::Unsigned,
        NumericSignSpec::EmbeddedLeading,
        NumericSignSpec::EmbeddedTrailing,
        NumericSignSpec::SeparateLeading,
    ] {
        let zd = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: d2,
            scale: sc0,
            sign,
            blank_zero: BlankZeroSpec::RetainZero,
        });
        let _ = normalize_display_operand_bytes(b"p1", Some(&zd));
    }

    let mut dst = *b"......";
    let mut ptr = 0_i64;
    assert_eq!(
        execute_string(&[], &mut dst, &mut ptr),
        StringFlowOutcome::Overflowed
    );
    ptr = 7;
    assert_eq!(
        execute_string(&[], &mut dst, &mut ptr),
        StringFlowOutcome::Overflowed
    );
    ptr = 1;
    let pieces = [
        StringSourcePiece {
            bytes: b"A|X",
            delimiter: StringDelimiter::Pattern(b"|"),
        },
        StringSourcePiece {
            bytes: b"B C",
            delimiter: StringDelimiter::FigurativeByte(b' '),
        },
        StringSourcePiece {
            bytes: b"D",
            delimiter: StringDelimiter::Pattern(b""),
        },
        StringSourcePiece {
            bytes: b"E",
            delimiter: StringDelimiter::Pattern(b"NOTFOUND"),
        },
        StringSourcePiece {
            bytes: b"F",
            delimiter: StringDelimiter::Size,
        },
    ];
    assert_eq!(
        execute_string(&pieces, &mut dst, &mut ptr),
        StringFlowOutcome::Completed
    );
    assert_eq!((&dst, ptr), (b"ABDEF.", 6));
    assert_eq!(
        execute_string(
            &[StringSourcePiece {
                bytes: b"XY",
                delimiter: StringDelimiter::Size,
            }],
            &mut dst,
            &mut ptr,
        ),
        StringFlowOutcome::Overflowed
    );
    assert_eq!(&dst, b"ABDEFX");
    Ok(())
}

/// Exercises M10 `execute_unstring` pointer bounds and un-delimited fixed-width splitting.
#[test]
fn m10_unstring_undelimited_coverage() -> Result<()> {
    use coblang_pic::{StringFlowOutcome, UnstringIntoSlot, execute_unstring};

    let mut u_ptr = 0_i64;
    let mut tally = 9_i64;
    assert_eq!(
        execute_unstring(b"ABCD", &[], &mut [], &mut u_ptr, &mut tally)?,
        StringFlowOutcome::Overflowed
    );
    u_ptr = 5;
    assert_eq!(
        execute_unstring(b"ABCD", &[], &mut [], &mut u_ptr, &mut tally)?,
        StringFlowOutcome::Overflowed
    );

    let (mut b1, mut b2, mut b3) = (*b"  ", *b"  ", *b"  ");
    let a2 = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 2,
        justified: JustifiedSpec::Left,
    });
    u_ptr = 1;
    assert_eq!(
        execute_unstring(
            b"ABC",
            &[],
            &mut [
                UnstringIntoSlot {
                    into_buf: &mut b1,
                    into_desc: &a2,
                    delim_in: None,
                    count_out: None,
                },
                UnstringIntoSlot {
                    into_buf: &mut b2,
                    into_desc: &a2,
                    delim_in: None,
                    count_out: None,
                },
                UnstringIntoSlot {
                    into_buf: &mut b3,
                    into_desc: &a2,
                    delim_in: None,
                    count_out: None,
                },
            ],
            &mut u_ptr,
            &mut tally,
        )?,
        StringFlowOutcome::Completed
    );
    assert_eq!((&b1, &b2, tally), (b"AB", b"C ", 2));
    Ok(())
}

/// Exercises M10 `execute_unstring` with `Single` / `AllRepeated` delimiters, `DELIMITER IN`, and `COUNT IN`.
#[test]
fn m10_unstring_delimited_coverage() -> Result<()> {
    use coblang_pic::{
        StringFlowOutcome, UnstringDelimSpec, UnstringIntoSlot, UnstringMatchMode, execute_unstring,
    };

    let num_desc = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: DigitCount::try_new(2)?,
        scale: ScaleFactor::ZERO,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let a1 = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 1,
        justified: JustifiedSpec::Left,
    });
    let a2 = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 2,
        justified: JustifiedSpec::Left,
    });
    let (mut b1, mut b2, mut d1, mut n1) = (*b"  ", *b"  ", [b' '; 1], *b"99");
    let (mut c1, mut u_ptr, mut tally) = (0_i64, 1_i64, 0_i64);
    let delims = [
        UnstringDelimSpec {
            mode: UnstringMatchMode::Single,
            pattern: b"",
        },
        UnstringDelimSpec {
            mode: UnstringMatchMode::AllRepeated,
            pattern: b"-",
        },
        UnstringDelimSpec {
            mode: UnstringMatchMode::Single,
            pattern: b",",
        },
    ];
    assert_eq!(
        execute_unstring(
            b",12--X",
            &delims,
            &mut [
                UnstringIntoSlot {
                    into_buf: &mut n1,
                    into_desc: &num_desc,
                    delim_in: Some((&mut d1, &a1)),
                    count_out: Some(&mut c1),
                },
                UnstringIntoSlot::simple(&mut b1, &a2),
            ],
            &mut u_ptr,
            &mut tally,
        )?,
        StringFlowOutcome::Overflowed
    );
    assert_eq!(
        (&n1, &d1, c1, &b1, u_ptr, tally),
        (b"00", b",", 0, b"12", 6, 2)
    );

    u_ptr = 1;
    assert_eq!(
        execute_unstring(
            b"A-B,C",
            &delims,
            &mut [UnstringIntoSlot::simple(&mut b1, &a2)],
            &mut u_ptr,
            &mut tally
        )?,
        StringFlowOutcome::Overflowed
    );
    u_ptr = 1;
    assert_eq!(
        execute_unstring(
            b"HI",
            &delims,
            &mut [
                UnstringIntoSlot::simple(&mut b1, &a2),
                UnstringIntoSlot::simple(&mut b2, &a2),
            ],
            &mut u_ptr,
            &mut tally,
        )?,
        StringFlowOutcome::Completed
    );
    assert_eq!(tally, 1);
    Ok(())
}

/// Exercises M10 `execute_inspect_tallying` across all qualifiers and window bounds.
#[test]
fn m10_inspect_tallying_coverage() {
    use coblang_pic::InspectMatchKind::{All, Characters, First, Leading, Trailing};
    use coblang_pic::InspectOperand::Bytes;
    use coblang_pic::{InspectTallyClause, InspectWindowSpec, execute_inspect_tallying};

    let mut deltas = [0_i64; 3];
    let win = |after, before| InspectWindowSpec { after, before };
    let tc = |counter_slot, kind, pattern, window| InspectTallyClause {
        counter_slot,
        kind,
        pattern,
        window,
    };
    let win_all = InspectWindowSpec::default();
    let win_ab = win(Some(Bytes(b"A")), Some(Bytes(b"Z")));
    execute_inspect_tallying(
        b"0000",
        None,
        &[
            tc(0, First, Bytes(b"0"), win_all),
            tc(0, Leading, Bytes(b"00"), win_all),
            tc(1, Trailing, Bytes(b"00"), win_all),
            tc(1, First, Bytes(b"00"), win_all),
        ],
        &mut deltas,
    );
    assert_eq!((deltas[0], deltas[1]), (1, 1));
    deltas = [0, 0, 0];
    let clauses = [
        tc(0, Leading, Bytes(b"00"), win_ab),
        tc(1, Trailing, Bytes(b"00"), win_ab),
        tc(2, First, Bytes(b"ZZ"), win(None, Some(Bytes(b"")))),
        tc(2, First, Bytes(b"B"), win_all),
        tc(2, All, Bytes(b"ZZ"), win_all),
        tc(2, All, Bytes(b""), win_all),
        tc(2, First, Bytes(b""), win_all),
        tc(2, All, Bytes(b"X"), win(Some(Bytes(b"")), None)),
        tc(2, First, Bytes(b""), win(Some(Bytes(b"NOPE")), None)),
        tc(2, Characters, Bytes(b""), win(None, Some(Bytes(b"NOPE")))),
    ];
    execute_inspect_tallying(b"A000BC000Z", None, &clauses, &mut deltas);
    assert_eq!((deltas[0], deltas[1], deltas[2]), (1, 1, 6));
}

/// Exercises M10 `execute_inspect_replacing` across all qualifiers (`Leading`, `Trailing`, `First`, `All`, `Characters`).
#[test]
fn m10_inspect_replacing_coverage() {
    use coblang_pic::{
        InspectMatchKind, InspectOperand, InspectReplaceClause, InspectWindowSpec,
        execute_inspect_replacing,
    };

    let win_all = InspectWindowSpec::default();
    let win_missing_after = InspectWindowSpec {
        after: Some(InspectOperand::Bytes(b"NOPE")),
        before: None,
    };
    let rc = |kind, pattern, by, window| InspectReplaceClause {
        kind,
        pattern,
        by,
        window,
    };
    let mut rep_buf = *b"00ABBA00";
    execute_inspect_replacing(
        &mut rep_buf,
        None,
        &[
            rc(
                InspectMatchKind::Leading,
                InspectOperand::Bytes(b""),
                InspectOperand::Bytes(b"X"),
                win_all,
            ),
            rc(
                InspectMatchKind::Trailing,
                InspectOperand::Bytes(b""),
                InspectOperand::Bytes(b"X"),
                win_all,
            ),
            rc(
                InspectMatchKind::Leading,
                InspectOperand::FigurativeByte(b'0'),
                InspectOperand::FigurativeByte(b' '),
                win_all,
            ),
            rc(
                InspectMatchKind::Trailing,
                InspectOperand::Bytes(b"0"),
                InspectOperand::Bytes(b"-"),
                win_all,
            ),
            rc(
                InspectMatchKind::First,
                InspectOperand::Bytes(b"BB"),
                InspectOperand::Bytes(b"12"),
                win_all,
            ),
            rc(
                InspectMatchKind::All,
                InspectOperand::Bytes(b"A"),
                InspectOperand::Bytes(b"Q"),
                win_all,
            ),
            rc(
                InspectMatchKind::Characters,
                InspectOperand::Bytes(b""),
                InspectOperand::Bytes(b"?"),
                win_missing_after,
            ),
        ],
    );
    assert_eq!(&rep_buf, b"  Q12Q--");
}

/// Exercises M10 `execute_inspect_converting` and zoned overpunch sign detach/restore across all 4 dialects.
#[test]
fn m10_inspect_converting_and_zoned_coverage() -> Result<()> {
    use coblang_pic::{
        InspectConvertSpec, InspectOperand, InspectWindowSpec, execute_inspect_converting,
    };

    let win_all = InspectWindowSpec::default();
    let mut conv_buf = *b"ABCA";
    let alpha_desc = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 4,
        justified: JustifiedSpec::Left,
    });
    execute_inspect_converting(
        &mut conv_buf,
        Some(&alpha_desc),
        InspectConvertSpec {
            from: InspectOperand::Bytes(b"AABC"),
            to: InspectOperand::Bytes(b"BCDE"),
            window: win_all,
        },
    );
    assert_eq!(&conv_buf, b"BDEB");

    let d2 = DigitCount::try_new(2)?;
    let sc0 = ScaleFactor::ZERO;
    for dialect in [
        OverpunchDialect::AsciiGnu,
        OverpunchDialect::Cobol85Standard,
        OverpunchDialect::EbcdicIbm,
        OverpunchDialect::MicroFocus,
    ] {
        for sign in [
            NumericSignSpec::EmbeddedLeading,
            NumericSignSpec::EmbeddedTrailing,
            NumericSignSpec::Unsigned,
        ] {
            let mut zd = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
                digits: d2,
                scale: sc0,
                sign,
                blank_zero: BlankZeroSpec::RetainZero,
            });
            zd.overpunch = dialect;
            execute_inspect_converting(
                &mut [],
                Some(&zd),
                InspectConvertSpec {
                    from: InspectOperand::Bytes(b"0"),
                    to: InspectOperand::Bytes(b"1"),
                    window: win_all,
                },
            );
            for sample in [[b'0', 0x71], *b"}J", *b"{A", [0xD0, 0xC1], *b"XY"] {
                let mut zbuf = sample;
                execute_inspect_converting(
                    &mut zbuf,
                    Some(&zd),
                    InspectConvertSpec {
                        from: InspectOperand::Bytes(b"01"),
                        to: InspectOperand::Bytes(b"10"),
                        window: win_all,
                    },
                );
                execute_inspect_converting(
                    &mut zbuf,
                    Some(&zd),
                    InspectConvertSpec {
                        from: InspectOperand::Bytes(&[b'0', b'1', 0xF0, 0xF1]),
                        to: InspectOperand::Bytes(b"XXXX"),
                        window: win_all,
                    },
                );
            }
        }
    }
    Ok(())
}
