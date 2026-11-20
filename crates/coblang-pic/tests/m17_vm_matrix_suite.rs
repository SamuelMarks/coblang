//! Exhaustive unit and 100% branch-coverage test suite for `PictureProgram` (`edit.rs`),
//! `move_matrix.rs`, `WithFiller`/`ValueMode`/`ReplacingRule` (`initialize.rs`), Format 3
//! `INSPECT`/`TRANSFORM`, and M17 `NATIONAL`/`UTF-8`/`EBCDIC` `MOVE` pathways.

use coblang_common::{DigitCount, Result, ScaleFactor};
use coblang_decimal::charset::{Codepage, UnmappedCharPolicy};
use coblang_decimal::{
    DecimalSign, FixedDecimal, HighOrderPolicy, OverpunchDialect, RoundingMode, SignedInt256,
    SizeErrorStatus, Uint256,
};
use coblang_hir::{
    BlankZeroSpec, DataCategory, HirFigurative, HirType, JustifiedSpec, NumericSignSpec, PicEditOp,
    Signedness,
};
use coblang_pic::{
    ALL_DEST_CATEGORIES, ALL_SOURCE_CATEGORIES, CurrencySignSpec, DecimalPointKind,
    DefaultInitMode, DestCategory, FillerPolicy, InitCategoryMask, InitTargetField, InitializePlan,
    InspectMatchKind, InspectOperand, InspectReplaceClause, InspectTallyClause, InspectWindowSpec,
    Justification, MoveConversionKind, MoveFieldDesc, MoveSource, NumericPictureMeta,
    PictureProgram, PictureProgramKind, ReplacingRule, SourceCategory, ValueMode, WithFiller,
    classify_move, de_edit_to_decimal_with_currency, encoding_space_byte, execute_initialize_plan,
    execute_inspect_tallying_and_replacing, execute_move, execute_transform,
    pad_or_truncate_national_utf16be, pad_or_truncate_national_utf16le, pad_or_truncate_sbcs,
    pad_or_truncate_utf8, pad_or_truncate_utf8_bytes_lossless, transcode_national_utf16le_to_sbcs,
    transcode_to_national_utf16le, transcode_to_utf8,
};

/// Verifies `de_edit_to_decimal_with_currency`, `DecimalPointKind`, `CurrencySignSpec`, and `PictureProgram` constructors/accessors.
#[test]
fn picture_program_constructors_and_multibyte_currency() -> Result<()> {
    assert!(de_edit_to_decimal_with_currency(b"12", b'.', b"").is_err());
    assert_eq!(
        de_edit_to_decimal_with_currency(b"  EUR1,234.56CR ", b'.', b"EUR")?,
        FixedDecimal::parse_str("-1234.56")?
    );
    assert_eq!(
        de_edit_to_decimal_with_currency("  €1.234,50- ".as_bytes(), b',', "€".as_bytes())?,
        FixedDecimal::parse_str("-1234.50")?
    );

    assert_eq!(DecimalPointKind::Period.decimal_byte(), b'.');
    assert_eq!(DecimalPointKind::Period.group_sep_byte(), b',');
    assert_eq!(DecimalPointKind::Comma.decimal_byte(), b',');
    assert_eq!(DecimalPointKind::Comma.group_sep_byte(), b'.');
    assert_eq!(
        DecimalPointKind::from_decimal_byte(b','),
        DecimalPointKind::Comma
    );
    assert_eq!(
        DecimalPointKind::from_decimal_byte(b'.'),
        DecimalPointKind::Period
    );

    assert!(CurrencySignSpec::try_new('$', "").is_err());
    let dollar = CurrencySignSpec::standard_dollar();
    assert_eq!(dollar.picture_symbol(), '$');
    assert_eq!(dollar.as_str(), "$");
    assert_eq!(dollar.as_bytes(), b"$");
    let eur = CurrencySignSpec::try_new('$', "EUR")?;
    assert_eq!(eur.as_str(), "EUR");

    assert!(PictureProgram::try_alphanumeric(Vec::new()).is_err());
    assert!(PictureProgram::try_national(Vec::new()).is_err());
    assert!(PictureProgram::try_utf8(Vec::new()).is_err());
    let meta = NumericPictureMeta {
        digits: DigitCount::try_new(5)?,
        scale: ScaleFactor::try_new(2)?,
        sign_capability: Signedness::Signed,
        blank_zero: BlankZeroSpec::RetainZero,
    };
    assert!(
        PictureProgram::try_numeric(Vec::new(), meta, DecimalPointKind::Period, eur.clone())
            .is_err()
    );

    let alpha_prog = PictureProgram::try_alphanumeric(vec![
        PicEditOp::AnyCharSlot,
        PicEditOp::InsertSlash,
        PicEditOp::AnyCharSlot,
    ])?;
    assert_eq!(alpha_prog.ops().len(), 3);
    assert_eq!(alpha_prog.kind(), PictureProgramKind::AlphanumericEdited);
    assert_eq!(alpha_prog.decimal_point(), DecimalPointKind::Period);
    assert_eq!(alpha_prog.currency(), &dollar);
    assert_eq!(alpha_prog.display_byte_len(), 3);
    assert_eq!(
        alpha_prog.format_alphanumeric(b"AB", Justification::Left, b' '),
        b"A/B"
    );
    assert!(
        alpha_prog
            .format_numeric(
                &FixedDecimal::ZERO,
                RoundingMode::Truncation,
                HighOrderPolicy::ModuloTruncate,
            )
            .is_err()
    );

    picture_program_numeric_vm_and_editions(&eur, meta)
}

/// Verifies `PictureProgram::format_numeric`, `format_national_utf16*`, `format_utf8`, and `de_edit` across all opcodes and zero rules.
///
/// # Arguments
///
/// * `eur` - Multi-byte `"EUR"` [`CurrencySignSpec`].
/// * `meta` - 5-digit, scale-2 signed [`NumericPictureMeta`].
fn picture_program_numeric_vm_and_editions(
    eur: &CurrencySignSpec,
    meta: NumericPictureMeta,
) -> Result<()> {
    // Floating multi-byte EUR (`$$$9,99CR` under `Comma`).
    let eur_prog = PictureProgram::try_numeric(
        vec![
            PicEditOp::FloatingCurrency('$'),
            PicEditOp::FloatingCurrency('$'),
            PicEditOp::FloatingCurrency('$'),
            PicEditOp::DigitSlot,
            PicEditOp::InsertDecimalPoint,
            PicEditOp::DigitSlot,
            PicEditOp::DigitSlot,
            PicEditOp::FixedCreditCr,
            PicEditOp::ImpliedDecimal,
            PicEditOp::ScalingPosition,
        ],
        meta,
        DecimalPointKind::Comma,
        eur.clone(),
    )?;
    assert_eq!(eur_prog.display_byte_len(), 11);
    let (fmt_neg, st) = eur_prog.format_numeric(
        &FixedDecimal::parse_str("-5.45")?,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(
        (fmt_neg.as_slice(), st),
        (b"  EUR5,45CR".as_slice(), SizeErrorStatus::Exact)
    );
    assert_eq!(
        eur_prog.de_edit(&fmt_neg)?,
        FixedDecimal::parse_str("-5.45")?
    );

    let (fmt_zero, _) = eur_prog.format_numeric(
        &FixedDecimal::ZERO,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(fmt_zero, b"  EUR0,00  ");
    let (fmt_big, _) = eur_prog.format_numeric(
        &FixedDecimal::parse_str("125.45")?,
        RoundingMode::Truncation,
        HighOrderPolicy::ModuloTruncate,
    )?;
    assert_eq!(fmt_big, b"EUR125,45  ");

    // BLANK WHEN ZERO (both zero and non-zero) + Unsigned.
    let bwz_prog = PictureProgram::try_numeric(
        vec![PicEditOp::DigitSlot, PicEditOp::FixedCurrency('$')],
        NumericPictureMeta {
            digits: DigitCount::ONE,
            scale: ScaleFactor::ZERO,
            sign_capability: Signedness::Unsigned,
            blank_zero: BlankZeroSpec::BlankWhenZero,
        },
        DecimalPointKind::Period,
        eur.clone(),
    )?;
    assert_eq!(
        bwz_prog
            .format_numeric(
                &FixedDecimal::ZERO,
                RoundingMode::Truncation,
                HighOrderPolicy::ModuloTruncate
            )?
            .0,
        b"    "
    );
    assert_eq!(
        bwz_prog
            .format_numeric(
                &FixedDecimal::from_i64(-7),
                RoundingMode::Truncation,
                HighOrderPolicy::ModuloTruncate
            )?
            .0,
        b"7EUR"
    );

    // Rescale overflow error inside `format_numeric`: 76-digit integer upscaled to scale 38.
    let max_mag = Uint256::from_decimal_digits(&[b'9'; 76])?;
    let huge = FixedDecimal::try_new(SignedInt256::from_parts(max_mag, DecimalSign::Positive), 0)?;
    let up_prog = PictureProgram::try_numeric(
        vec![PicEditOp::DigitSlot],
        NumericPictureMeta {
            digits: DigitCount::ONE,
            scale: ScaleFactor::try_new(38)?,
            sign_capability: Signedness::Signed,
            blank_zero: BlankZeroSpec::RetainZero,
        },
        DecimalPointKind::Period,
        CurrencySignSpec::standard_dollar(),
    )?;
    assert_eq!(
        up_prog
            .format_numeric(
                &huge,
                RoundingMode::Truncation,
                HighOrderPolicy::FlagOverflow
            )?
            .1,
        SizeErrorStatus::Overflowed
    );

    picture_program_all_suppression_and_signs(eur)?;
    picture_program_national_and_utf8()
}

/// Exercises all-suppression zero (`Z`, `*`, `$$`, `CR`, `DB`, fixed currency) in `edit.rs`.
///
/// # Arguments
///
/// * `eur` - Multi-byte `"EUR"` [`CurrencySignSpec`].
fn picture_program_all_suppression_and_signs(eur: &CurrencySignSpec) -> Result<()> {
    let d3 = DigitCount::try_new(3)?;
    let sc1 = ScaleFactor::try_new(1)?;
    let meta_s = NumericPictureMeta {
        digits: d3,
        scale: sc1,
        sign_capability: Signedness::Signed,
        blank_zero: BlankZeroSpec::RetainZero,
    };

    // All-suppression zero WITHOUT asterisk (`Z` + `$$` + `FixedCurrency` + `.` + `CR` + `V`).
    let sup_space = PictureProgram::try_numeric(
        vec![
            PicEditOp::FixedCurrency('$'),
            PicEditOp::FloatingCurrency('$'),
            PicEditOp::FloatingCurrency('$'),
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::InsertDecimalPoint,
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::FixedCreditCr,
            PicEditOp::ImpliedDecimal,
        ],
        meta_s,
        DecimalPointKind::Period,
        eur.clone(),
    )?;
    assert_eq!(
        sup_space
            .format_numeric(
                &FixedDecimal::ZERO,
                RoundingMode::Truncation,
                HighOrderPolicy::ModuloTruncate
            )?
            .0,
        b"            "
    );

    // All-suppression zero WITH asterisk (`*` preserves decimal point!).
    let sup_ast = PictureProgram::try_numeric(
        vec![
            PicEditOp::ZeroSuppressAsterisk,
            PicEditOp::ZeroSuppressAsterisk,
            PicEditOp::InsertDecimalPoint,
            PicEditOp::ZeroSuppressAsterisk,
            PicEditOp::FixedDebitDb,
        ],
        meta_s,
        DecimalPointKind::Period,
        CurrencySignSpec::standard_dollar(),
    )?;
    assert_eq!(
        sup_ast
            .format_numeric(
                &FixedDecimal::ZERO,
                RoundingMode::Truncation,
                HighOrderPolicy::ModuloTruncate
            )?
            .0,
        b"**.*  "
    );
    picture_program_floating_and_fixed_signs()
}

/// Exercises floating `+`/`-`, end-of-program float flush, and all fixed sign/insertion ops in `edit.rs`.
fn picture_program_floating_and_fixed_signs() -> Result<()> {
    let m2 = NumericPictureMeta {
        digits: DigitCount::try_new(2)?,
        scale: ScaleFactor::ZERO,
        sign_capability: Signedness::Signed,
        blank_zero: BlankZeroSpec::RetainZero,
    };
    for (fl_op, v, expected) in [
        (PicEditOp::FloatingSignPlus, 5_i64, b" +5"),
        (PicEditOp::FloatingSignPlus, -5, b" -5"),
        (PicEditOp::FloatingSignMinus, 5, b"  5"),
        (PicEditOp::FloatingSignMinus, -5, b" -5"),
        (PicEditOp::FloatingSignPlus, 45, b"+45"),
    ] {
        let p = PictureProgram::try_numeric(
            vec![fl_op, fl_op, PicEditOp::DigitSlot],
            m2,
            DecimalPointKind::Period,
            CurrencySignSpec::standard_dollar(),
        )?;
        assert_eq!(
            p.format_numeric(
                &FixedDecimal::from_i64(v),
                RoundingMode::Truncation,
                HighOrderPolicy::ModuloTruncate
            )?
            .0,
            expected
        );
    }

    let flush_p = PictureProgram::try_numeric(
        vec![PicEditOp::FloatingSignPlus],
        NumericPictureMeta {
            digits: DigitCount::ONE,
            scale: ScaleFactor::ZERO,
            sign_capability: Signedness::Signed,
            blank_zero: BlankZeroSpec::RetainZero,
        },
        DecimalPointKind::Period,
        CurrencySignSpec::standard_dollar(),
    )?;
    assert_eq!(
        flush_p
            .format_numeric(
                &FixedDecimal::from_i64(4),
                RoundingMode::Truncation,
                HighOrderPolicy::ModuloTruncate
            )?
            .0,
        b"+"
    );

    let kitchen = PictureProgram::try_numeric(
        vec![
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::InsertGroupSep,
            PicEditOp::InsertBlank,
            PicEditOp::InsertZero,
            PicEditOp::InsertSlash,
            PicEditOp::ZeroSuppressAsterisk,
            PicEditOp::DigitSlot,
            PicEditOp::InsertGroupSep,
            PicEditOp::InsertBlank,
            PicEditOp::InsertZero,
            PicEditOp::InsertSlash,
            PicEditOp::FixedSignPlus,
            PicEditOp::FixedSignMinus,
            PicEditOp::FixedCreditCr,
            PicEditOp::FixedDebitDb,
            PicEditOp::AnyCharSlot,
        ],
        NumericPictureMeta {
            digits: DigitCount::try_new(3)?,
            scale: ScaleFactor::ZERO,
            sign_capability: Signedness::Signed,
            blank_zero: BlankZeroSpec::RetainZero,
        },
        DecimalPointKind::Period,
        CurrencySignSpec::standard_dollar(),
    )?;
    for sample in [5_i64, -5, 123] {
        let (rendered, _) = kitchen.format_numeric(
            &FixedDecimal::from_i64(sample),
            RoundingMode::Truncation,
            HighOrderPolicy::ModuloTruncate,
        )?;
        assert_eq!(rendered.len(), kitchen.display_byte_len());
    }
    Ok(())
}

/// Verifies `NationalEdited` (`UTF-16LE` & `UTF-16BE`) and `Utf8Edited` in `PictureProgram`.
fn picture_program_national_and_utf8() -> Result<()> {
    let nat_prog = PictureProgram::try_national(vec![
        PicEditOp::NationalSlot,
        PicEditOp::InsertBlank,
        PicEditOp::InsertZero,
        PicEditOp::InsertSlash,
        PicEditOp::NationalSlot,
        PicEditOp::ImpliedDecimal,
        PicEditOp::FixedCreditCr,
    ])?;
    assert_eq!(nat_prog.kind(), PictureProgramKind::NationalEdited);
    assert_eq!(nat_prog.display_byte_len(), 10);
    assert!(
        nat_prog
            .format_national_utf16le(&[0x41], Justification::Left)
            .is_err()
    );
    assert!(
        nat_prog
            .format_national_utf16be(&[0x00], Justification::Left)
            .is_err()
    );
    assert_eq!(
        nat_prog.format_national_utf16le(b"A\0B\0", Justification::Left)?,
        [b'A', 0, b' ', 0, b'0', 0, b'/', 0, b'B', 0]
    );
    assert_eq!(
        nat_prog.format_national_utf16be(&[0, b'A', 0, b'B'], Justification::Right)?,
        [0, b'A', 0, b' ', 0, b'0', 0, b'/', 0, b'B']
    );

    let utf8_prog = PictureProgram::try_utf8(vec![
        PicEditOp::Utf8Slot,
        PicEditOp::InsertBlank,
        PicEditOp::InsertZero,
        PicEditOp::InsertSlash,
        PicEditOp::Utf8Slot,
        PicEditOp::ImpliedDecimal,
    ])?;
    assert_eq!(utf8_prog.kind(), PictureProgramKind::Utf8Edited);
    assert_eq!(utf8_prog.display_byte_len(), 5);
    assert_eq!(
        utf8_prog.format_utf8("é", 8, Justification::Left),
        "é 0/   ".as_bytes()
    );
    assert_eq!(
        utf8_prog.format_utf8("é", 8, Justification::Right),
        "  0/é  ".as_bytes()
    );
    assert_eq!(
        utf8_prog.format_utf8("ABC", 6, Justification::Right),
        b"B 0/C "
    );
    Ok(())
}

/// Verifies `(SourceCategory, DestCategory)` `classify_move`, `Justification`, and padding helpers in `move_matrix.rs`.
#[test]
fn move_matrix_exhaustive_categories_and_padding() -> Result<()> {
    assert_eq!(
        Justification::from_hir(JustifiedSpec::Left),
        Justification::Left
    );
    assert_eq!(
        Justification::from_hir(JustifiedSpec::Right),
        Justification::Right
    );
    assert_eq!(Justification::Left.to_hir(), JustifiedSpec::Left);
    assert_eq!(Justification::Right.to_hir(), JustifiedSpec::Right);

    for cat in [
        DataCategory::Alphabetic,
        DataCategory::Alphanumeric,
        DataCategory::AlphanumericEdited,
        DataCategory::Numeric,
        DataCategory::NumericEdited,
        DataCategory::National,
        DataCategory::NationalEdited,
        DataCategory::Utf8,
        DataCategory::Group,
    ] {
        assert!(SourceCategory::from_data_category(cat).is_some());
        assert!(DestCategory::from_data_category(cat).is_some());
    }
    for non_move in [
        DataCategory::BooleanBit,
        DataCategory::Pointer,
        DataCategory::Index,
        DataCategory::ObjectReference,
    ] {
        assert!(SourceCategory::from_data_category(non_move).is_none());
        assert!(DestCategory::from_data_category(non_move).is_none());
    }

    let mut legal_count = 0_usize;
    let mut illegal_count = 0_usize;
    for src in ALL_SOURCE_CATEGORIES {
        for dst in ALL_DEST_CATEGORIES {
            match classify_move(src, dst) {
                Ok(kind) => {
                    legal_count += 1;
                    if src == SourceCategory::FigurativeConstant {
                        assert_eq!(kind, MoveConversionKind::FigurativeFill);
                    }
                }
                Err(_) => {
                    illegal_count += 1;
                }
            }
        }
    }
    assert_eq!(legal_count + illegal_count, 90);
    assert_eq!(illegal_count, 10);

    assert_eq!(encoding_space_byte(Codepage::Iso8859_1), Some(0x20));
    assert_eq!(encoding_space_byte(Codepage::Cp1140), Some(0x40));
    assert_eq!(encoding_space_byte(Codepage::Utf16Be), None);

    assert_eq!(
        pad_or_truncate_sbcs(&[0xC1], 3, Justification::Left, Codepage::Cp1140)?,
        [0xC1, 0x40, 0x40]
    );
    assert_eq!(
        pad_or_truncate_sbcs(b"A", 3, Justification::Right, Codepage::Iso8859_1)?,
        b"  A"
    );
    assert!(pad_or_truncate_sbcs(b"A", 2, Justification::Left, Codepage::Utf16Le).is_err());

    move_matrix_national_utf8_and_transcoding()
}

/// Verifies UTF-16 and UTF-8 padding helpers in `move_matrix.rs`.
fn move_matrix_national_utf8_and_transcoding() -> Result<()> {
    assert_eq!(
        pad_or_truncate_national_utf16be(&[0, b'A'], 2, Justification::Left)?,
        [0, b'A', 0, b' ']
    );
    assert_eq!(
        pad_or_truncate_national_utf16be(&[0, b'A'], 2, Justification::Right)?,
        [0, b' ', 0, b'A']
    );
    assert!(pad_or_truncate_national_utf16be(&[0], 1, Justification::Left).is_err());
    assert_eq!(
        pad_or_truncate_national_utf16le(b"A\0", 2, Justification::Right)?,
        b" \0A\0"
    );
    assert!(pad_or_truncate_national_utf16le(b"A", 1, Justification::Left).is_err());

    assert_eq!(
        pad_or_truncate_utf8("é€", 2, 4, Justification::Left),
        [0xC3, 0xA9, b' ', b' ']
    );
    assert_eq!(
        pad_or_truncate_utf8("€é", 2, 4, Justification::Right),
        [b' ', b' ', 0xC3, 0xA9]
    );
    assert_eq!(
        pad_or_truncate_utf8("ABCD", 2, 4, Justification::Right),
        b"  CD"
    );
    assert_eq!(
        pad_or_truncate_utf8_bytes_lossless("é€".as_bytes(), 4, Justification::Left),
        [0xC3, 0xA9, b' ', b' ']
    );
    assert_eq!(
        pad_or_truncate_utf8_bytes_lossless(&[0xFF, b'A'], 3, Justification::Left),
        [0xFF, b'A', b' ']
    );
    move_matrix_transcoding()
}

/// Verifies `transcode_*` `Ok` and `Err` paths in `move_matrix.rs`.
fn move_matrix_transcoding() -> Result<()> {
    assert_eq!(
        transcode_to_national_utf16le(
            &[0xC1],
            Codepage::Cp1140,
            2,
            Justification::Left,
            UnmappedCharPolicy::StrictError
        )?,
        b"A\0 \0"
    );
    assert!(
        transcode_to_national_utf16le(
            &[0xFF],
            Codepage::Utf8,
            2,
            Justification::Left,
            UnmappedCharPolicy::StrictError
        )
        .is_err()
    );

    assert_eq!(
        transcode_national_utf16le_to_sbcs(
            b"A\0",
            Codepage::Cp1140,
            2,
            Justification::Left,
            UnmappedCharPolicy::StrictError
        )?,
        [0xC1, 0x40]
    );
    assert!(
        transcode_national_utf16le_to_sbcs(
            &[0xAC, 0x20],
            Codepage::Iso8859_1,
            2,
            Justification::Left,
            UnmappedCharPolicy::StrictError
        )
        .is_err()
    );
    assert!(
        transcode_national_utf16le_to_sbcs(
            b"A\0",
            Codepage::Utf16Be,
            2,
            Justification::Left,
            UnmappedCharPolicy::Substitute('?')
        )
        .is_err()
    );

    assert_eq!(
        transcode_to_utf8(
            &[0xC1, 0xC2],
            Codepage::Cp1140,
            5,
            4,
            Justification::Left,
            UnmappedCharPolicy::StrictError
        )?,
        b"AB  "
    );
    assert!(
        transcode_to_utf8(
            &[0x41],
            Codepage::Utf16Le,
            2,
            4,
            Justification::Left,
            UnmappedCharPolicy::StrictError
        )
        .is_err()
    );
    Ok(())
}

/// Verifies `WithFiller`, `ValueMode`, `ReplacingRule`, `InitializePlan`, Format 3 `INSPECT`, `TRANSFORM`, and M17 `execute_move`.
#[test]
fn initialize_plan_inspect_format3_and_m17_moves() -> Result<()> {
    assert_eq!(
        WithFiller::ExcludeFiller.to_filler_policy(),
        FillerPolicy::SkipFiller
    );
    assert_eq!(
        WithFiller::IncludeFiller.to_filler_policy(),
        FillerPolicy::IncludeFiller
    );
    assert_eq!(
        WithFiller::from_filler_policy(FillerPolicy::SkipFiller),
        WithFiller::ExcludeFiller
    );
    assert_eq!(
        WithFiller::from_filler_policy(FillerPolicy::IncludeFiller),
        WithFiller::IncludeFiller
    );

    let alpha2 = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 2,
        justified: JustifiedSpec::Left,
    });
    let fields = [
        InitTargetField {
            offset: 0,
            byte_len: 2,
            desc: alpha2.clone(),
            is_filler: false,
            is_redefined: false,
            value_bytes: Some(b"HI"),
        },
        InitTargetField {
            offset: 2,
            byte_len: 2,
            desc: alpha2.clone(),
            is_filler: true,
            is_redefined: false,
            value_bytes: None,
        },
    ];
    let mut buf = *b"XXXX";
    let def_plan = InitializePlan::default();
    execute_initialize_plan(&mut buf, &fields, &def_plan)?;
    assert_eq!(&buf, b"  XX");

    let rules = [ReplacingRule {
        category: DataCategory::Alphanumeric,
        source: MoveSource::AlphanumericLiteral(b"ZZ"),
    }];
    let val_plan = InitializePlan {
        with_filler: WithFiller::IncludeFiller,
        value_mode: ValueMode::ToDefinedValue,
        value_categories: InitCategoryMask::ALL,
        replacing: &[],
        default_mode: DefaultInitMode::StandardCobol,
    };
    execute_initialize_plan(&mut buf, &fields, &val_plan)?;
    assert_eq!(&buf, b"HI  ");

    let rep_plan = InitializePlan {
        with_filler: WithFiller::IncludeFiller,
        value_mode: ValueMode::DefaultCategoryInit,
        value_categories: InitCategoryMask::NONE,
        replacing: &rules,
        default_mode: DefaultInitMode::StandardCobol,
    };
    execute_initialize_plan(&mut buf, &fields, &rep_plan)?;
    assert_eq!(&buf, b"ZZZZ");
    assert!(execute_initialize_plan(&mut [0_u8; 1], &fields, &rep_plan).is_err());

    // Combined Format 3 `INSPECT ... TALLYING ... REPLACING ...` + `TRANSFORM`.
    let mut insp = *b"AABBA";
    let mut deltas = [0_i64; 1];
    execute_inspect_tallying_and_replacing(
        &mut insp,
        None,
        &[InspectTallyClause {
            counter_slot: 0,
            kind: InspectMatchKind::All,
            pattern: InspectOperand::Bytes(b"A"),
            window: InspectWindowSpec::default(),
        }],
        &mut deltas,
        &[InspectReplaceClause {
            kind: InspectMatchKind::All,
            pattern: InspectOperand::Bytes(b"A"),
            by: InspectOperand::Bytes(b"X"),
            window: InspectWindowSpec::default(),
        }],
    );
    assert_eq!((deltas, &insp), ([3], b"XXBBX"));

    execute_transform(
        &mut insp,
        None,
        InspectOperand::Bytes(b"XB"),
        InspectOperand::Bytes(b"12"),
    );
    assert_eq!(&insp, b"11221");

    m17_move_engine_paths()
}

/// Exercises M17 `Utf8` and `National -> SBCS` `execute_move` branches in `move_engine.rs`.
fn m17_move_engine_paths() -> Result<()> {
    let utf8_4 = MoveFieldDesc::standard_gnu(HirType::Utf8 { byte_len: 4 });
    let nat2 = MoveFieldDesc::standard_gnu(HirType::National {
        char_len: 2,
        justified: JustifiedSpec::Left,
    });
    let mut ebc_alpha = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 2,
        justified: JustifiedSpec::Left,
    });
    ebc_alpha.overpunch = OverpunchDialect::EbcdicIbm;
    let ascii_alpha = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 2,
        justified: JustifiedSpec::Left,
    });
    let mut ebc_zoned = MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
        digits: DigitCount::try_new(2)?,
        scale: ScaleFactor::ZERO,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    ebc_zoned.overpunch = OverpunchDialect::EbcdicIbm;

    let mut u_buf = [0_u8; 4];
    execute_move(
        MoveSource::Figurative(HirFigurative::Zero),
        &mut u_buf,
        &utf8_4,
    )?;
    assert_eq!(&u_buf, b"0000");

    execute_move(
        MoveSource::Field {
            bytes: &[0xE9, 0x00, 0xAC, 0x20],
            desc: &nat2,
        },
        &mut u_buf,
        &utf8_4,
    )?;
    assert_eq!(&u_buf, &[0xC3, 0xA9, b' ', b' ']);
    assert!(
        execute_move(
            MoveSource::Field {
                bytes: &[0xE9],
                desc: &nat2,
            },
            &mut u_buf,
            &utf8_4,
        )
        .is_err()
    );

    execute_move(
        MoveSource::Field {
            bytes: &[0xC1, 0xC2],
            desc: &ebc_alpha,
        },
        &mut u_buf,
        &utf8_4,
    )?;
    assert_eq!(&u_buf, b"AB  ");
    execute_move(
        MoveSource::Field {
            bytes: &[0xF4, 0xF2],
            desc: &ebc_zoned,
        },
        &mut u_buf,
        &utf8_4,
    )?;
    assert_eq!(&u_buf, b"42  ");
    assert!(
        execute_move(
            MoveSource::Field {
                bytes: &[0x00],
                desc: &ebc_zoned,
            },
            &mut u_buf,
            &utf8_4,
        )
        .is_err()
    );

    let mut a2 = [0_u8; 2];
    execute_move(
        MoveSource::Field {
            bytes: b"A\0B\0",
            desc: &nat2,
        },
        &mut a2,
        &ascii_alpha,
    )?;
    assert_eq!(&a2, b"AB");
    execute_move(
        MoveSource::Field {
            bytes: b"A\0B\0",
            desc: &nat2,
        },
        &mut a2,
        &ebc_alpha,
    )?;
    assert_eq!(&a2, &[0xC1, 0xC2]);

    m17_move_to_national_paths(&utf8_4, &nat2, &ebc_alpha, &ebc_zoned)
}

/// Exercises M17 `Figurative`, `Utf8`, `EBCDIC`, and `Group` -> `National` branches in `move_engine.rs`.
fn m17_move_to_national_paths(
    utf8_4: &MoveFieldDesc,
    nat2: &MoveFieldDesc,
    ebc_alpha: &MoveFieldDesc,
    ebc_zoned: &MoveFieldDesc,
) -> Result<()> {
    let mut n4 = [0_u8; 4];
    for (fig, expected) in [
        (HirFigurative::Zero, [b'0', 0, b'0', 0]),
        (HirFigurative::Space, [b' ', 0, b' ', 0]),
        (HirFigurative::HighValue, [0xFF, 0xFF, 0xFF, 0xFF]),
        (HirFigurative::LowValue, [0, 0, 0, 0]),
        (HirFigurative::Null, [0, 0, 0, 0]),
        (HirFigurative::Quote, [b'"', 0, b'"', 0]),
        (HirFigurative::All(Vec::new()), [b' ', 0, b' ', 0]),
        (HirFigurative::All(b"Z".to_vec()), [b'Z', 0, b'Z', 0]),
        (HirFigurative::All(vec![0xFF]), [0xFF, 0, 0xFF, 0]),
    ] {
        execute_move(MoveSource::Figurative(fig), &mut n4, nat2)?;
        assert_eq!(n4, expected);
    }

    execute_move(
        MoveSource::Field {
            bytes: "é!".as_bytes(),
            desc: utf8_4,
        },
        &mut n4,
        nat2,
    )?;
    assert_eq!(n4, [0xE9, 0x00, b'!', 0x00]);
    assert!(
        execute_move(
            MoveSource::Field {
                bytes: &[0xFF, 0xFF, 0xFF, 0xFF],
                desc: utf8_4,
            },
            &mut n4,
            nat2,
        )
        .is_err()
    );

    execute_move(
        MoveSource::Field {
            bytes: &[0xC1, 0xC2],
            desc: ebc_alpha,
        },
        &mut n4,
        nat2,
    )?;
    assert_eq!(n4, [b'A', 0, b'B', 0]);

    execute_move(
        MoveSource::Field {
            bytes: &[0xF1, 0xF9],
            desc: ebc_zoned,
        },
        &mut n4,
        nat2,
    )?;
    assert_eq!(n4, [b'1', 0, b'9', 0]);
    assert!(
        execute_move(
            MoveSource::Field {
                bytes: &[0x00],
                desc: ebc_zoned,
            },
            &mut n4,
            nat2,
        )
        .is_err()
    );

    let ascii_alpha = MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
        byte_len: 2,
        justified: JustifiedSpec::Left,
    });
    execute_move(
        MoveSource::Field {
            bytes: b"HI",
            desc: &ascii_alpha,
        },
        &mut n4,
        nat2,
    )?;
    assert_eq!(n4, [b'H', 0, b'I', 0]);
    Ok(())
}
