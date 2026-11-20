//! Clean-room unit, branch, and end-to-end coverage tests for `coblang_sema::picture`.

#![deny(clippy::indexing_slicing)]

use std::path::Path;

use coblang_common::{
    ByteLength, ByteOffset, Dialect, ExpansionId, FileId, Result, SourceFormat, SourceSpan,
    SymbolId,
};
use coblang_diag::{
    BinaryByteOrder, BinarySizePolicy, DiagnosticSink, DialectConfig, SourceMap, SymbolInterner,
    TruncationMode, WarningPolicy,
};
use coblang_hir::{
    BinaryEndianness, BlankZeroSpec, Comp6Mode, FloatBinaryWidth, FloatDecimalWidth, HirType,
    JustifiedSpec, NumericSignSpec, PicEditOp, ResolvedHirModule, Signedness, TruncationPolicy,
};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, preprocess_source_file,
};
use coblang_sema::{
    ElementaryClauses, ParsedPicture, PictureContext, PictureFlavor, compile_elementary_type,
    lower_compilation_group, parse_picture_string,
};
use coblang_syntax::ast::{
    BlankWhenZeroMode, DecimalPointKind, JustificationMode, SignClause, SignPlacement,
    SignSeparation, UsageKind,
};
use coblang_syntax::parse_preprocessed_unit;

/// Returns a synthetic zero-offset [`SourceSpan`] for unit tests.
fn dummy_span() -> SourceSpan {
    SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::new(4),
        ExpansionId::ROOT,
    )
}

/// Parses `raw` with [`PictureContext::default`] under `Cobol2023` and returns `(ParsedPicture, DiagnosticSink)`.
///
/// # Arguments
///
/// * `raw` - Raw `PICTURE` string to expand and classify.
fn parse_pic_2023(raw: &str) -> (ParsedPicture, DiagnosticSink) {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 100);
    let parsed = parse_picture_string(
        raw,
        dummy_span(),
        PictureContext::default(),
        &dialect,
        &mut sink,
    );
    (parsed, sink)
}

/// Compiles [`ElementaryClauses`] under `dialect` and returns `(HirType, DiagnosticSink)`.
///
/// # Arguments
///
/// * `clauses` - Bundled elementary Data Division clauses.
/// * `dialect` - Active dialect configuration.
fn compile_elem(
    clauses: &ElementaryClauses<'_>,
    dialect: &DialectConfig,
) -> (HirType, DiagnosticSink) {
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 100);
    let ty = compile_elementary_type(clauses, dialect, &mut sink);
    (ty, sink)
}

/// Compiles a synthetic free-format COBOL snippet end-to-end through `lower_compilation_group`.
///
/// # Arguments
///
/// * `src` - Clean-room COBOL source text.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if preprocessing, parsing, or semantic lowering fails.
fn compile_e2e_cobol(src: &str) -> Result<ResolvedHirModule> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let mem = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dialect,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 100);
    let unit = preprocess_source_file(
        "pic_e2e.cob",
        Path::new("."),
        src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let mut interner = SymbolInterner::new();
    let group = parse_preprocessed_unit(&unit, &dialect, &mut interner, &mut sink)?;
    lower_compilation_group(&group, &dialect, &mut interner, &mut sink)
}

/// Tests repetition expansion, malformed factors, raw length caps, and `MAX_EXPANDED_PICTURE_LEN = 65_535`.
#[test]
fn repetition_expansion_and_max_length_safeguards() {
    let (ok_pic, ok_sink) = parse_pic_2023("S9(4)V9(2)X(0)");
    assert!(ok_sink.has_errors());
    assert_eq!(ok_pic.digit_positions, 6);

    let (one_rep, one_sink) = parse_pic_2023("X(1)");
    assert!(!one_sink.has_errors());
    assert_eq!(one_rep.display_len, 1);

    for bad_rep in ["(5)9", "X(", "X(12", "X()", "X(0)", "X(65536)", "X(ABC)"] {
        let (_p, s) = parse_pic_2023(bad_rep);
        assert!(s.has_errors(), "expected error for `{bad_rep}`");
    }

    // Overflow via repetition with remaining > 0 (`65531 + 9 > 65535`).
    let (ov1, s_ov1) = parse_pic_2023("X(65530)X(10)");
    assert!(s_ov1.has_errors());
    assert_eq!(ov1.display_len, 65_535);

    // Overflow via repetition with remaining == 0 (`65535 + 4 > 65535`).
    let (ov2, s_ov2) = parse_pic_2023("X(65534)X(5)");
    assert!(s_ov2.has_errors());
    assert_eq!(ov2.display_len, 65_535);

    // Overflow via literal char after hitting 65,535 via `X(65535)X(5)`.
    let (ov3, s_ov3) = parse_pic_2023("X(65535)X(5)");
    assert!(s_ov3.has_errors());
    assert_eq!(ov3.display_len, 65_535);

    // Overflow via >65,535 literal characters (also >255 raw length).
    let huge_literal = "X".repeat(65_536);
    let (ov4, s_ov4) = parse_pic_2023(&huge_literal);
    assert!(s_ov4.has_errors());
    assert_eq!(ov4.display_len, 65_535);

    // Raw character-string length cap in COBOL-85 (50) vs COBOL-2023 (255).
    let d85 = DialectConfig::for_dialect(Dialect::Cobol85);
    let mut s85_ok = DiagnosticSink::new(WarningPolicy::new(), 20);
    let _ = parse_picture_string(
        &"X".repeat(50),
        dummy_span(),
        PictureContext::default(),
        &d85,
        &mut s85_ok,
    );
    assert!(!s85_ok.has_errors());

    let mut s85_err = DiagnosticSink::new(WarningPolicy::new(), 20);
    let _ = parse_picture_string(
        &"X".repeat(51),
        dummy_span(),
        PictureContext::default(),
        &d85,
        &mut s85_err,
    );
    assert!(s85_err.has_errors());

    let (_p255, s255_ok) = parse_pic_2023(&"X".repeat(255));
    assert!(!s255_ok.has_errors());
    let (_p256, s256_err) = parse_pic_2023(&"X".repeat(256));
    assert!(s256_err.has_errors());
}

/// Tests alphabetic, alphanumeric, alphanumeric-edited, national, national-edited, UTF-8, and bit `PictureFlavor`s.
#[test]
fn picture_non_numeric_and_unicode_flavors() {
    let valid_cases = [
        (
            "AAA",
            PictureFlavor::Alphabetic,
            3,
            0,
            0,
            Signedness::Unsigned,
        ),
        (
            "AB0/A",
            PictureFlavor::AlphanumericEdited,
            5,
            0,
            0,
            Signedness::Unsigned,
        ),
        (
            "A9",
            PictureFlavor::Alphanumeric,
            2,
            1,
            0,
            Signedness::Unsigned,
        ),
        (
            "XX",
            PictureFlavor::Alphanumeric,
            2,
            0,
            0,
            Signedness::Unsigned,
        ),
        (
            "XB0/X",
            PictureFlavor::AlphanumericEdited,
            5,
            0,
            0,
            Signedness::Unsigned,
        ),
        ("NN", PictureFlavor::National, 2, 0, 0, Signedness::Unsigned),
        (
            "NB0/N",
            PictureFlavor::NationalEdited,
            5,
            0,
            0,
            Signedness::Unsigned,
        ),
        ("UUU", PictureFlavor::Utf8, 3, 0, 0, Signedness::Unsigned),
        ("1111", PictureFlavor::Bit, 4, 0, 0, Signedness::Unsigned),
    ];

    for (raw, flavor, disp, digits, scale, sign) in valid_cases {
        let (p, sink) = parse_pic_2023(raw);
        assert!(
            !sink.has_errors(),
            "unexpected error on `{raw}`: {:?}",
            sink.diagnostics()
        );
        assert_eq!(p.kind, flavor);
        assert_eq!(p.display_len, disp);
        assert_eq!(p.digit_positions, digits);
        assert_eq!(p.scale, scale);
        assert_eq!(p.signed, sign);
    }
}

/// Tests numeric, `P`-scaled, numeric-edited `PictureFlavor`s, and invalid symbol mixtures.
#[test]
fn picture_numeric_scaling_and_invalid_combinations() {
    let valid_cases = [
        ("S99V9", PictureFlavor::Numeric, 3, 3, 1, Signedness::Signed),
        (
            "PP99",
            PictureFlavor::Numeric,
            2,
            2,
            4,
            Signedness::Unsigned,
        ),
        (
            "VPP99",
            PictureFlavor::Numeric,
            2,
            2,
            4,
            Signedness::Unsigned,
        ),
        (
            "99PP",
            PictureFlavor::Numeric,
            2,
            2,
            -2,
            Signedness::Unsigned,
        ),
        (
            "ZZ9.99",
            PictureFlavor::NumericEdited,
            6,
            5,
            2,
            Signedness::Unsigned,
        ),
        (
            "**9,999",
            PictureFlavor::NumericEdited,
            7,
            6,
            0,
            Signedness::Unsigned,
        ),
    ];

    for (raw, flavor, disp, digits, scale, sign) in valid_cases {
        let (p, sink) = parse_pic_2023(raw);
        assert!(
            !sink.has_errors(),
            "unexpected error on `{raw}`: {:?}",
            sink.diagnostics()
        );
        assert_eq!(p.kind, flavor);
        assert_eq!(p.display_len, disp);
        assert_eq!(p.digit_positions, digits);
        assert_eq!(p.scale, scale);
        assert_eq!(p.signed, sign);
    }

    let invalid_cases = [
        "", "S", "V", "P", "B0/", "...", "+", "$", "CR", "9S9", "9V9V9", "9.9.9", "9V9.9", "V9P",
        "SX", "XV9", "X$", "PX", "9PX", "NX", "N9", "SZ9", "S9.9", "9Q9", "9C", "9CZ", "9D", "9DZ",
    ];
    for bad in invalid_cases {
        let (_p, sink) = parse_pic_2023(bad);
        assert!(sink.has_errors(), "expected error on `{bad}`");
    }
}

/// Tests editing signs (`+`, `-`, `CR`, `DB`), currency symbols, and `DECIMAL-POINT IS COMMA`.
#[test]
fn editing_signs_currency_and_comma_context() {
    let (cr_ok, s_cr) = parse_pic_2023("99cr");
    assert!(!s_cr.has_errors());
    assert_eq!(cr_ok.signed, Signedness::Signed);
    assert!(cr_ok.ops.contains(&PicEditOp::FixedCreditCr));

    let (db_ok, s_db) = parse_pic_2023("99DB");
    assert!(!s_db.has_errors());
    assert!(db_ok.ops.contains(&PicEditOp::FixedDebitDb));

    for bad_sign in ["CR99", "DB99", "+99CR", "-99DB", "9+9", "9-9"] {
        let (_p, s) = parse_pic_2023(bad_sign);
        assert!(s.has_errors(), "expected sign error on `{bad_sign}`");
    }

    for good_sign in ["+99", "99+", "-99", "99-", "+++9", "---9", "$99", "$$$9"] {
        let (_p, s) = parse_pic_2023(good_sign);
        assert!(!s.has_errors(), "unexpected error on `{good_sign}`");
    }

    // European comma decimal-point with custom currency symbol `'e'` -> `'E'`.
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let comma_ctx = PictureContext {
        decimal_point: DecimalPointKind::Comma,
        currency_symbol: 'e',
    };
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 20);
    let parsed = parse_picture_string("eee.ee9,99", dummy_span(), comma_ctx, &dialect, &mut sink);
    assert!(!sink.has_errors());
    assert_eq!(parsed.kind, PictureFlavor::NumericEdited);
    assert_eq!(parsed.scale, 2);
    assert_eq!(parsed.digit_positions, 7);
    assert!(parsed.ops.contains(&PicEditOp::FloatingCurrency('E')));
    assert!(parsed.ops.contains(&PicEditOp::InsertGroupSep));
    assert!(parsed.ops.contains(&PicEditOp::InsertDecimalPoint));
}

/// Tests all pictureless `USAGE` representations and picture-forbidding `USAGE` diagnostics.
#[test]
fn pictureless_usages_and_forbidden_picture_combinations() {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let base = ElementaryClauses {
        raw_pic: None,
        entry_span: dummy_span(),
        pic_ctx: PictureContext::default(),
        usage: None,
        sign_clause: None,
        justified: JustificationMode::StandardLeft,
        blank_zero: BlankWhenZeroMode::Normal,
    };

    // Missing both PICTURE and USAGE -> error.
    let (_ty_none, s_none) = compile_elem(&base, &dialect);
    assert!(s_none.has_errors());

    // Pictureless-valid usages (also forbid accompanying PICTURE).
    let pictureless_usages = [
        UsageKind::FloatShort,
        UsageKind::FloatLong,
        UsageKind::FloatExtended,
        UsageKind::FloatDecimal16,
        UsageKind::FloatDecimal34,
        UsageKind::Index,
        UsageKind::Pointer,
        UsageKind::ProgramPointer,
        UsageKind::FunctionPointer,
        UsageKind::ObjectReference(None),
        UsageKind::ObjectReference(Some(SymbolId::new(1))),
        UsageKind::BinaryCharSigned,
        UsageKind::BinaryCharUnsigned,
        UsageKind::BinaryShortSigned,
        UsageKind::BinaryShortUnsigned,
        UsageKind::BinaryLongSigned,
        UsageKind::BinaryLongUnsigned,
        UsageKind::BinaryDoubleSigned,
        UsageKind::BinaryDoubleUnsigned,
        UsageKind::BinaryCLongSigned,
        UsageKind::BinaryCLongUnsigned,
    ];

    for u in &pictureless_usages {
        let ok_clauses = ElementaryClauses {
            usage: Some(u),
            ..base
        };
        let (ty, s_ok) = compile_elem(&ok_clauses, &dialect);
        assert!(
            !s_ok.has_errors(),
            "unexpected error for pictureless `{u:?}`"
        );
        if matches!(u, UsageKind::FloatShort) {
            assert_eq!(
                ty,
                HirType::FloatBinary {
                    width: FloatBinaryWidth::Bits32
                }
            );
        } else if matches!(u, UsageKind::FloatDecimal34) {
            assert_eq!(
                ty,
                HirType::FloatDecimal {
                    width: FloatDecimalWidth::Decimal128
                }
            );
        }

        // Pairing any of these with a PICTURE must emit E0006InvalidPicture.
        let bad_clauses = ElementaryClauses {
            raw_pic: Some(("9", dummy_span())),
            usage: Some(u),
            ..base
        };
        let (_ty_bad, s_bad) = compile_elem(&bad_clauses, &dialect);
        assert!(
            s_bad.has_errors(),
            "expected error when pairing PIC with `{u:?}`"
        );
    }

    // Usages that require a PICTURE when raw_pic is None.
    for needs_pic in [
        UsageKind::Display,
        UsageKind::Binary,
        UsageKind::PackedDecimal,
    ] {
        let c = ElementaryClauses {
            usage: Some(&needs_pic),
            ..base
        };
        let (_t, s) = compile_elem(&c, &dialect);
        assert!(s.has_errors());
    }
}

/// Helper constructing [`ElementaryClauses`] with default `PictureContext`.
///
/// # Arguments
///
/// * `pic` - Raw `PICTURE` string.
/// * `span` - Source span.
/// * `usage` - Optional [`UsageKind`] reference.
/// * `sign_clause` - Optional [`SignClause`].
/// * `justified` - [`JustificationMode`].
/// * `blank_zero` - [`BlankWhenZeroMode`].
fn mk_clauses<'a>(
    pic: &'a str,
    span: SourceSpan,
    usage: Option<&'a UsageKind>,
    sign_clause: Option<SignClause>,
    justified: JustificationMode,
    blank_zero: BlankWhenZeroMode,
) -> ElementaryClauses<'a> {
    ElementaryClauses {
        raw_pic: Some((pic, span)),
        entry_span: span,
        pic_ctx: PictureContext::default(),
        usage,
        sign_clause,
        justified,
        blank_zero,
    }
}

/// Tests `resolve_alphanumeric_with_usage` across `COMP-X`, `COMP-N`, `NATIONAL`, `UTF-8`, and `DISPLAY`.
#[test]
fn combine_alphanumeric_picture_with_usages_and_justification() {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let span = dummy_span();
    let mk = |pic: &'static str,
              usage: Option<&'static UsageKind>,
              sign_clause: Option<SignClause>,
              justified: JustificationMode,
              blank_zero: BlankWhenZeroMode| {
        mk_clauses(pic, span, usage, sign_clause, justified, blank_zero)
    };

    let (tx_cx, s1) = compile_elem(
        &mk(
            "X(4)",
            Some(&UsageKind::CompX),
            None,
            JustificationMode::StandardLeft,
            BlankWhenZeroMode::Normal,
        ),
        &dialect,
    );
    assert!(!s1.has_errors());
    assert!(matches!(
        tx_cx,
        HirType::BinaryInt {
            byte_width: 4,
            endian: BinaryEndianness::BigEndian,
            ..
        }
    ));

    let (tx_cn, s2) = compile_elem(
        &mk(
            "X(2)",
            Some(&UsageKind::CompN),
            None,
            JustificationMode::StandardLeft,
            BlankWhenZeroMode::Normal,
        ),
        &dialect,
    );
    assert!(!s2.has_errors());
    assert!(matches!(
        tx_cn,
        HirType::BinaryInt {
            byte_width: 2,
            endian: BinaryEndianness::Native,
            ..
        }
    ));

    let (tx_nat, s3) = compile_elem(
        &mk(
            "X(3)",
            Some(&UsageKind::National),
            None,
            JustificationMode::JustifiedRight,
            BlankWhenZeroMode::Normal,
        ),
        &dialect,
    );
    assert!(!s3.has_errors());
    assert_eq!(
        tx_nat,
        HirType::National {
            char_len: 3,
            justified: JustifiedSpec::Right
        }
    );

    let (tx_utf8, s4) = compile_elem(
        &mk(
            "X(5)",
            Some(&UsageKind::Utf8),
            None,
            JustificationMode::StandardLeft,
            BlankWhenZeroMode::Normal,
        ),
        &dialect,
    );
    assert!(!s4.has_errors());
    assert_eq!(tx_utf8, HirType::Utf8 { byte_len: 5 });

    let (tx_disp, s5) = compile_elem(
        &mk(
            "X(5)",
            Some(&UsageKind::Display),
            None,
            JustificationMode::JustifiedRight,
            BlankWhenZeroMode::Normal,
        ),
        &dialect,
    );
    assert!(!s5.has_errors());
    assert_eq!(
        tx_disp,
        HirType::Alphanumeric {
            byte_len: 5,
            justified: JustifiedSpec::Right
        }
    );
}

/// Tests `JUSTIFIED RIGHT` diagnostics on numeric/edited items and `NumericEdited` `USAGE` checks.
#[test]
fn combine_picture_justified_restrictions_and_numeric_edited_usages() {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let span = dummy_span();
    let mk = |pic: &'static str,
              usage: Option<&'static UsageKind>,
              sign_clause: Option<SignClause>,
              justified: JustificationMode,
              blank_zero: BlankWhenZeroMode| {
        mk_clauses(pic, span, usage, sign_clause, justified, blank_zero)
    };

    let (_t, sj_num) = compile_elem(
        &mk(
            "99",
            None,
            None,
            JustificationMode::JustifiedRight,
            BlankWhenZeroMode::Normal,
        ),
        &dialect,
    );
    assert!(sj_num.has_errors());
    let (_t, sj_ed) = compile_elem(
        &mk(
            "Z9",
            Some(&UsageKind::National),
            None,
            JustificationMode::JustifiedRight,
            BlankWhenZeroMode::BlankWhenZero,
        ),
        &dialect,
    );
    assert!(sj_ed.has_errors());
    let (_t, sed_disp) = compile_elem(
        &mk(
            "Z9",
            Some(&UsageKind::Display),
            None,
            JustificationMode::StandardLeft,
            BlankWhenZeroMode::Normal,
        ),
        &dialect,
    );
    assert!(!sed_disp.has_errors());
    let (_t, sed_bin) = compile_elem(
        &mk(
            "Z9",
            Some(&UsageKind::Binary),
            None,
            JustificationMode::StandardLeft,
            BlankWhenZeroMode::Normal,
        ),
        &dialect,
    );
    assert!(sed_bin.has_errors());
}

/// Tests `PACKED-DECIMAL` (`COMP-3`) and unsigned `COMP-6` sign validation rules.
#[test]
fn combine_picture_packed_decimal_and_comp6_clauses() {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let span = dummy_span();
    let mk = |pic: &'static str,
              usage: Option<&'static UsageKind>,
              sign_clause: Option<SignClause>,
              justified: JustificationMode,
              blank_zero: BlankWhenZeroMode| {
        mk_clauses(pic, span, usage, sign_clause, justified, blank_zero)
    };

    let (tp, sp) = compile_elem(
        &mk(
            "S9(5)",
            Some(&UsageKind::PackedDecimal),
            None,
            JustificationMode::StandardLeft,
            BlankWhenZeroMode::Normal,
        ),
        &dialect,
    );
    assert!(!sp.has_errors());
    assert!(matches!(
        tp,
        HirType::PackedDecimal {
            unsigned_comp6: Comp6Mode::StandardComp3,
            ..
        }
    ));

    let (tc6_ok, sc6_ok) = compile_elem(
        &mk(
            "9(4)",
            Some(&UsageKind::Comp6),
            None,
            JustificationMode::StandardLeft,
            BlankWhenZeroMode::Normal,
        ),
        &dialect,
    );
    assert!(!sc6_ok.has_errors());
    assert!(matches!(
        tc6_ok,
        HirType::PackedDecimal {
            unsigned_comp6: Comp6Mode::UnsignedComp6,
            ..
        }
    ));

    let (_tc6_err, sc6_err) = compile_elem(
        &mk(
            "S9(4)",
            Some(&UsageKind::Comp6),
            None,
            JustificationMode::StandardLeft,
            BlankWhenZeroMode::Normal,
        ),
        &dialect,
    );
    assert!(sc6_err.has_errors());
}

/// Tests all four `SignClause` placement and separation combinations on zoned decimal items.
#[test]
fn zoned_sign_clause_placements_and_separations() {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let span = dummy_span();

    let sign_variants = [
        (
            SignPlacement::Trailing,
            SignSeparation::Embedded,
            NumericSignSpec::EmbeddedTrailing,
        ),
        (
            SignPlacement::Leading,
            SignSeparation::Embedded,
            NumericSignSpec::EmbeddedLeading,
        ),
        (
            SignPlacement::Trailing,
            SignSeparation::SeparateCharacter,
            NumericSignSpec::SeparateTrailing,
        ),
        (
            SignPlacement::Leading,
            SignSeparation::SeparateCharacter,
            NumericSignSpec::SeparateLeading,
        ),
    ];
    for (placement, separation, expected) in sign_variants {
        let clauses = ElementaryClauses {
            raw_pic: Some(("S9(3)", span)),
            entry_span: span,
            pic_ctx: PictureContext::default(),
            usage: None,
            sign_clause: Some(SignClause {
                placement,
                separation,
            }),
            justified: JustificationMode::StandardLeft,
            blank_zero: BlankWhenZeroMode::Normal,
        };
        let (ty, sink) = compile_elem(&clauses, &dialect);
        assert!(!sink.has_errors());
        assert!(matches!(ty, HirType::ZonedDecimal { sign, .. } if sign == expected));
    }
}

/// Tests `SIGN` on unsigned items, `BLANK WHEN ZERO` on signed items, and `clamp_digits` limits.
#[test]
fn zoned_blank_zero_restrictions_and_digit_clamping() {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let span = dummy_span();

    // Unsigned with SIGN clause -> error; Signed without SIGN clause + BLANK WHEN ZERO -> error.
    let (_t1, s_uns_sign) = compile_elem(
        &ElementaryClauses {
            raw_pic: Some(("9(3)", span)),
            entry_span: span,
            pic_ctx: PictureContext::default(),
            usage: None,
            sign_clause: Some(SignClause {
                placement: SignPlacement::Leading,
                separation: SignSeparation::Embedded,
            }),
            justified: JustificationMode::StandardLeft,
            blank_zero: BlankWhenZeroMode::BlankWhenZero,
        },
        &dialect,
    );
    assert!(s_uns_sign.has_errors());

    let (t_sgn_bwz, s_sgn_bwz) = compile_elem(
        &ElementaryClauses {
            raw_pic: Some(("S9(3)", span)),
            entry_span: span,
            pic_ctx: PictureContext::default(),
            usage: None,
            sign_clause: None,
            justified: JustificationMode::StandardLeft,
            blank_zero: BlankWhenZeroMode::BlankWhenZero,
        },
        &dialect,
    );
    assert!(s_sgn_bwz.has_errors());
    assert!(matches!(
        t_sgn_bwz,
        HirType::ZonedDecimal {
            sign: NumericSignSpec::EmbeddedTrailing,
            blank_zero: BlankZeroSpec::BlankWhenZero,
            ..
        }
    ));

    // Digit clamping (>18 in COBOL-85, >38 in COBOL-2023).
    let d85 = DialectConfig::for_dialect(Dialect::Cobol85);
    let (_t85, s85) = compile_elem(
        &ElementaryClauses {
            raw_pic: Some(("9(19)", span)),
            entry_span: span,
            pic_ctx: PictureContext::default(),
            usage: None,
            sign_clause: None,
            justified: JustificationMode::StandardLeft,
            blank_zero: BlankWhenZeroMode::Normal,
        },
        &d85,
    );
    assert!(s85.has_errors());

    let (_t39, s39) = compile_elem(
        &ElementaryClauses {
            raw_pic: Some(("9(40)", span)),
            entry_span: span,
            pic_ctx: PictureContext::default(),
            usage: None,
            sign_clause: None,
            justified: JustificationMode::StandardLeft,
            blank_zero: BlankWhenZeroMode::Normal,
        },
        &dialect,
    );
    assert!(s39.has_errors());
}

/// Tests `BinaryInt` endianness (`BigEndian` vs `Native`) and truncation modes (`TruncateToPic` vs `NoTrunc`).
#[test]
fn binary_endianness_and_truncation_modes() {
    let span = dummy_span();
    let mut dialect = DialectConfig::for_dialect(Dialect::Cobol2023);

    dialect.binary_byteorder = BinaryByteOrder::BigEndian;
    dialect.binary_truncate = TruncationMode::TruncateToPic;
    for (u, exp_end, exp_trunc) in [
        (
            UsageKind::Binary,
            BinaryEndianness::BigEndian,
            TruncationPolicy::TruncateToPic,
        ),
        (
            UsageKind::Comp5,
            BinaryEndianness::Native,
            TruncationPolicy::FullBinaryRange,
        ),
        (
            UsageKind::CompX,
            BinaryEndianness::BigEndian,
            TruncationPolicy::FullBinaryRange,
        ),
        (
            UsageKind::CompN,
            BinaryEndianness::Native,
            TruncationPolicy::FullBinaryRange,
        ),
    ] {
        let (ty, sink) = compile_elem(
            &ElementaryClauses {
                raw_pic: Some(("9(4)", span)),
                entry_span: span,
                pic_ctx: PictureContext::default(),
                usage: Some(&u),
                sign_clause: None,
                justified: JustificationMode::StandardLeft,
                blank_zero: BlankWhenZeroMode::Normal,
            },
            &dialect,
        );
        assert!(!sink.has_errors());
        assert!(matches!(
            ty,
            HirType::BinaryInt { endian, trunc, .. } if endian == exp_end && trunc == exp_trunc
        ));
    }

    dialect.binary_byteorder = BinaryByteOrder::Native;
    dialect.binary_truncate = TruncationMode::NoTrunc;
    let (ty_nt, s_nt) = compile_elem(
        &ElementaryClauses {
            raw_pic: Some(("9(4)", span)),
            entry_span: span,
            pic_ctx: PictureContext::default(),
            usage: Some(&UsageKind::Binary),
            sign_clause: None,
            justified: JustificationMode::StandardLeft,
            blank_zero: BlankWhenZeroMode::Normal,
        },
        &dialect,
    );
    assert!(!s_nt.has_errors());
    assert!(matches!(
        ty_nt,
        HirType::BinaryInt {
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
            ..
        }
    ));
}

/// Tests `BinaryInt` sizing policies (`2-4-8`, `1-2-4-8`, `1--8`) across all digit buckets.
#[test]
fn binary_size_policies_across_digit_buckets() {
    let span = dummy_span();
    let mut dialect = DialectConfig::for_dialect(Dialect::Cobol2023);

    let policy_matrix = [
        (
            BinarySizePolicy::TwoFourEight,
            &[(2_u8, 2_u8), (6, 4), (12, 8), (20, 16)][..],
        ),
        (
            BinarySizePolicy::OneTwoFourEight,
            &[(2, 1), (4, 2), (6, 4), (12, 8), (20, 16)][..],
        ),
        (
            BinarySizePolicy::OneToEight,
            &[
                (1, 1),
                (3, 2),
                (5, 3),
                (8, 4),
                (10, 5),
                (13, 6),
                (15, 7),
                (17, 8),
                (20, 16),
            ][..],
        ),
    ];

    for (policy, cases) in policy_matrix {
        dialect.binary_size = policy;
        for &(digits, expected_bytes) in cases {
            let pic = format!("9({digits})");
            let (ty, sink) = compile_elem(
                &ElementaryClauses {
                    raw_pic: Some((&pic, span)),
                    entry_span: span,
                    pic_ctx: PictureContext::default(),
                    usage: Some(&UsageKind::Binary),
                    sign_clause: None,
                    justified: JustificationMode::StandardLeft,
                    blank_zero: BlankWhenZeroMode::Normal,
                },
                &dialect,
            );
            assert!(!sink.has_errors());
            assert!(
                matches!(ty, HirType::BinaryInt { byte_width, .. } if byte_width == expected_bytes),
                "policy {policy:?} digits {digits}: got {ty:?}"
            );
        }
    }
}

/// Exercises end-to-end COBOL compilation of `SPECIAL-NAMES` picture context and diverse elementary items.
#[test]
fn end_to_end_cobol_picture_and_usage_lowering() -> Result<()> {
    let src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. PIC-E2E.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SPECIAL-NAMES.
    DECIMAL-POINT IS COMMA
    CURRENCY SIGN IS "EUR" WITH PICTURE SYMBOL "E".
DATA DIVISION.
WORKING-STORAGE SECTION.
01 A-ALPHA PIC A(4) JUSTIFIED RIGHT.
01 A-EDITED PIC X(2)B0/X(2).
01 N-NAT PIC N(3).
01 N-ED PIC NB/0N.
01 U-STR PIC U(4).
01 B-BIT PIC 1(8) USAGE BIT.
01 C-PRICE PIC EEE.EE9,99CR BLANK WHEN ZERO.
01 Z-LEAD PIC S9(4)V9(2) SIGN IS LEADING SEPARATE CHARACTER.
01 P-PACK PIC S9(5)V99 USAGE PACKED-DECIMAL.
01 F-LONG USAGE COMP-2.
PROCEDURE DIVISION.
    STOP RUN.
"#;
    let resolved = compile_e2e_cobol(src)?;
    assert_eq!(resolved.0.programs.len(), 1);
    if let Some(prog) = resolved.0.programs.first() {
        assert!(prog.data_items.len() >= 10);
    }
    Ok(())
}
