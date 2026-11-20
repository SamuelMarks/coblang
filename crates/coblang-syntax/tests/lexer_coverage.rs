//! Exhaustive unit and branch-coverage tests for `coblang-syntax` lexical analysis and crate entrypoints.

#![deny(clippy::indexing_slicing)]

use std::path::Path;

use coblang_common::{
    ByteLength, ByteOffset, CoblangError, Dialect, ExpansionId, FileId, Result, SourceFormat,
    SourceSpan,
};
use coblang_diag::{
    DiagnosticSink, DialectConfig, FeatureDisposition, RelaxedSyntaxMode, SourceMap,
    SymbolInterner, WarningPolicy,
};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, MarginZone, PpToken, PpTokenKind, PreprocessConfig,
    PreprocessedUnit, SpacingHint, preprocess_source_file,
};
use coblang_syntax::{
    LiteralSign, ParserLimits, StringPrefix, Token, TokenKind, WordRole, lex_tokens,
    parse_preprocessed_unit, parse_preprocessed_unit_with_limits,
};

/// Preprocesses and lexes a synthetic COBOL snippet under the given format and dialect configuration.
///
/// # Arguments
///
/// * `src` - Synthetic COBOL source text.
/// * `fmt` - Initial COBOL reference format (`Free` or `Fixed`).
/// * `dialect` - Active dialect configuration.
///
/// # Errors
///
/// Returns [`CoblangError`] if preprocessing, warning configuration, or lexical interning fails.
fn lex_snippet(
    src: &str,
    fmt: SourceFormat,
    dialect: &DialectConfig,
) -> Result<(Vec<Token>, SymbolInterner, DiagnosticSink)> {
    let mem = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: fmt,
        dialect_config: dialect,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut pol = WarningPolicy::new();
    pol.apply_cli_flag("-Wall")?;
    pol.apply_cli_flag("-Wextra")?;
    let mut sink = DiagnosticSink::new(pol, 200);
    let unit =
        preprocess_source_file("test.cob", Path::new("."), src, &pp_cfg, &mut sm, &mut sink)?;
    let eof_span = SourceSpan::new(
        unit.root_file_id,
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let mut interner = SymbolInterner::new();
    let tokens = lex_tokens(&unit.tokens, eof_span, dialect, &mut interner, &mut sink)?;
    Ok((tokens, interner, sink))
}

/// Builds a synthetic [`PpToken`] for targeted lexical edge-case verification.
///
/// # Arguments
///
/// * `text` - Raw token text.
/// * `kind` - Preprocessor token classification.
/// * `zone` - Source margin zone.
/// * `spacing` - Preceding whitespace hint.
fn mk_pp(text: &str, kind: PpTokenKind, zone: MarginZone, spacing: SpacingHint) -> PpToken {
    PpToken {
        text: String::from(text),
        kind,
        span: SourceSpan::new(
            FileId::new(0),
            ByteOffset::ZERO,
            ByteLength::new(u32::try_from(text.len()).unwrap_or(0)),
            ExpansionId::ROOT,
        ),
        zone,
        spacing,
    }
}

/// Exercises `parse_preprocessed_unit` and `parse_preprocessed_unit_with_limits` with empty and non-empty streams.
#[test]
fn crate_entrypoints_empty_and_non_empty_preprocessed_units() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Default);
    let mem = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dialect,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };

    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let empty_unit: PreprocessedUnit =
        preprocess_source_file("empty.cob", Path::new("."), "", &pp_cfg, &mut sm, &mut sink)?;
    let mut interner = SymbolInterner::new();
    let grp_empty = parse_preprocessed_unit(&empty_unit, &dialect, &mut interner, &mut sink)?;
    assert_eq!(grp_empty.units.len(), 0);

    let prog_src = "IDENTIFICATION DIVISION.
PROGRAM-ID. HELLO.
PROCEDURE DIVISION.
STOP RUN.
";
    let unit = preprocess_source_file(
        "ok.cob",
        Path::new("."),
        prog_src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let grp = parse_preprocessed_unit(&unit, &dialect, &mut interner, &mut sink)?;
    assert_eq!(grp.units.len(), 1);

    let mut tiny_interner = SymbolInterner::with_limit(0);
    let err = parse_preprocessed_unit_with_limits(
        &unit,
        &dialect,
        ParserLimits::default(),
        &mut tiny_interner,
        &mut sink,
    );
    assert!(matches!(
        err,
        Err(CoblangError::InvalidDomainValue {
            domain_type: "SymbolId",
            ..
        })
    ));

    let step_err = parse_preprocessed_unit_with_limits(
        &unit,
        &dialect,
        ParserLimits::new(0, 16),
        &mut interner,
        &mut sink,
    );
    assert!(matches!(
        step_err,
        Err(CoblangError::ResourceLimitExceeded { .. })
    ));
    Ok(())
}

/// Exercises punctuation, `::` vs `: :`, commas/semicolons, exponentiation, and concatenation operators.
#[test]
fn lexer_punctuation_and_arithmetic_operators() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Gnu);
    let src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. OP-TEST.\n",
        "PROCEDURE DIVISION.\n",
        "COMPUTE MY-VAR = (1 + 2 - 3 * 4 / 5 ** 2) & OBJ::METH(1 : 2), A : B;\n",
    );
    let (tokens, _interner, sink) = lex_snippet(src, SourceFormat::Free, &dialect)?;
    assert!(!sink.has_errors());
    assert!(tokens.iter().any(|t| t.kind == TokenKind::DoubleColon));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Colon));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Comma));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Semicolon));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::StarStar));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Ampersand));
    Ok(())
}

/// Exercises relational operators (`<=`, `>=`, `<>`, `!=`, `<`, `>`, `=`) and word reservation roles.
#[test]
fn lexer_relational_operators_and_word_roles() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Gnu);
    let src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. REL-TEST.\n",
        "PROCEDURE DIVISION.\n",
        "IF A <= B AND B >= C AND C <> D AND D != E AND E < F AND F > G AND G = H\n",
        "   CONTINUE\n",
        "END-IF.\n",
    );
    let (tokens, _interner, sink) = lex_snippet(src, SourceFormat::Free, &dialect)?;
    assert!(!sink.has_errors());
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Le));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Ge));
    assert!(tokens.iter().any(|t| t.kind == TokenKind::Ne));
    assert!(tokens.iter().any(|t| matches!(
        t.kind,
        TokenKind::Word {
            role: WordRole::ReservedKeyword,
            ..
        }
    )));
    assert!(tokens.iter().any(|t| matches!(
        t.kind,
        TokenKind::Word {
            role: WordRole::ContextSoftWord,
            ..
        }
    )));
    Ok(())
}

/// Exercises stray `==`, non-tight colons, trailing hyphens, invalid characters, and empty words.
#[test]
fn lexer_stray_pseudo_colons_and_invalid_words() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Gnu);
    let eof_span = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let raw_pps = vec![
        mk_pp(
            "==",
            PpTokenKind::PseudoDelimiter,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ":",
            PpTokenKind::Colon,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ":",
            PpTokenKind::Colon,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ":",
            PpTokenKind::Colon,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "BAD-",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "$SYS_1#A+B",
            PpTokenKind::Word,
            MarginZone::AreaA,
            SpacingHint::Newline,
        ),
        mk_pp(
            "%",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "!é",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp("", PpTokenKind::Word, MarginZone::AreaB, SpacingHint::Space),
    ];
    let mut pol = WarningPolicy::new();
    pol.apply_cli_flag("-Wall")?;
    let mut diag = DiagnosticSink::new(pol, 50);
    let mut int2 = SymbolInterner::new();
    let out = lex_tokens(&raw_pps, eof_span, &dialect, &mut int2, &mut diag)?;
    assert!(diag.error_count() >= 4);
    assert!(out.last().is_some_and(|t| t.kind == TokenKind::Eof));
    Ok(())
}

/// Exercises `PICTURE` mode scanning, `IS`, separators, `.` errors, EOF errors, and `PICTURE SYMBOL`.
#[test]
fn lexer_picture_mode_and_error_branches() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Default);
    let src = concat!(
        "IDENTIFICATION DIVISION.
",
        "PROGRAM-ID. PIC-TEST.
",
        "ENVIRONMENT DIVISION.
",
        "CONFIGURATION SECTION.
",
        "SPECIAL-NAMES.
",
        r#"    CURRENCY SIGN IS "EUR" WITH PICTURE SYMBOL "E".
"#,
        "DATA DIVISION.
",
        "WORKING-STORAGE SECTION.
",
        "01 A PIC IS S9(5)V99.
",
        "01 B PICTURE $,$$9.99.
",
        "01 C PIC 999,999, VALUE 0.
",
        "01 D PIC 9,.
",
        "01 E PIC .
",
    );
    let (tokens, interner, sink) = lex_snippet(src, SourceFormat::Free, &dialect)?;
    assert_eq!(sink.error_count(), 1);
    let pics: Vec<&str> = tokens
        .iter()
        .filter_map(|t| match t.kind {
            TokenKind::PictureString(sym) => interner.resolve_original(sym).ok(),
            _ => None,
        })
        .collect();
    assert!(pics.contains(&"S9(5)V99"));
    assert!(pics.contains(&"$,$$9.99"));
    assert!(pics.contains(&"999,999"));
    assert!(pics.contains(&"9"));

    // EOF while still in LexerMode::Picture (`01 Z PIC IS`, `01 Z PIC 9,`, and `01 Z PIC 99` at EOF), plus `:` at EOF.
    let (_, _, sink_eof1) = lex_snippet("01 Z PIC IS", SourceFormat::Free, &dialect)?;
    assert_eq!(sink_eof1.error_count(), 1);

    let (_, _, sink_eof2) = lex_snippet("01 Z PIC 9,", SourceFormat::Free, &dialect)?;
    assert_eq!(sink_eof2.error_count(), 0);

    let (_, _, sink_eof3) = lex_snippet("01 Z PIC 99", SourceFormat::Free, &dialect)?;
    assert_eq!(sink_eof3.error_count(), 0);

    let (_, _, sink_colon_eof) = lex_snippet("MOVE S(1 :", SourceFormat::Free, &dialect)?;
    assert_eq!(sink_colon_eof.error_count(), 0);
    Ok(())
}

/// Exercises valid comma-decimal, signed decimal, scientific float, HP octal, and `END PROGRAM` reset.
#[test]
fn lexer_comma_decimal_and_signed_float_literals() -> Result<()> {
    let mut dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    dialect.max_pic_digits = 6;
    let src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. NUM-TEST.\n",
        "ENVIRONMENT DIVISION.\n",
        "CONFIGURATION SECTION.\n",
        "SPECIAL-NAMES.\n",
        "    DECIMAL-POINT IS COMMA.\n",
        "PROCEDURE DIVISION.\n",
        "000-MAIN.\n",
        "    MOVE 12,34 TO A.\n",
        "    MOVE +5,6 TO A.\n",
        "    MOVE -0,75 TO A.\n",
        "    MOVE .5 TO A.\n",
        "    MOVE -.25 TO A.\n",
        "    MOVE 1.5E+2 TO A.\n",
        "    MOVE -2.0e-3 TO A.\n",
        "    MOVE 3E4 TO A.\n",
        "    MOVE %77 TO A.\n",
        "END PROGRAM NUM-TEST.\n",
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. AFTER-END.\n",
        "ENVIRONMENT DIVISION.\n",
        "SPECIAL-NAMES.\n",
        "    DECIMAL-POINT COMMA.\n",
    );
    let (tokens, _interner, _sink) = lex_snippet(src, SourceFormat::Free, &dialect)?;
    assert!(tokens.iter().any(|t| matches!(
        t.kind,
        TokenKind::DecimalLit {
            sign: LiteralSign::Positive,
            ..
        }
    )));
    assert!(tokens.iter().any(|t| matches!(
        t.kind,
        TokenKind::DecimalLit {
            sign: LiteralSign::Negative,
            ..
        }
    )));
    assert!(
        tokens
            .iter()
            .any(|t| matches!(t.kind, TokenKind::FloatLit(_)))
    );
    Ok(())
}

/// Exercises malformed HP octal, digit-limit overflow, multi-dot, exponent-error, and digit-letter literals.
#[test]
fn lexer_malformed_numeric_float_octal_and_digit_limits() -> Result<()> {
    let mut dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    dialect.max_pic_digits = 6;
    let src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. NUM-ERR-TEST.\n",
        "ENVIRONMENT DIVISION.\n",
        "CONFIGURATION SECTION.\n",
        "SPECIAL-NAMES.\n",
        "    DECIMAL-POINT IS COMMA.\n",
        "PROCEDURE DIVISION.\n",
        "    MOVE %89 TO A.\n",
        "    MOVE 1234567 TO A.\n",
        "    MOVE 1234.567 TO A.\n",
        "    MOVE 1.2.3 TO A.\n",
        "    MOVE 1.2.3E2 TO A.\n",
        "    MOVE 1.5E12345 TO A.\n",
        "    MOVE 1.5E1.2 TO A.\n",
        "    MOVE 1.5E TO A.\n",
        "    MOVE 1E+ TO A.\n",
        "    MOVE 1EA TO A.\n",
        "    MOVE 123ABC TO A.\n",
    );
    let (_tokens, _interner, sink) = lex_snippet(src, SourceFormat::Free, &dialect)?;
    assert!(sink.error_count() >= 8);
    Ok(())
}

/// Exercises `DECIMAL-POINT [IS] COMMA` lookahead edge cases, trailing tokens, and EOF boundaries.
#[test]
fn lexer_decimal_point_clause_lookahead_edges() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let eof_span = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let pps = vec![
        mk_pp(
            "DECIMAL-POINT",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ".",
            PpTokenKind::Period,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "DECIMAL-POINT",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "IS",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ".",
            PpTokenKind::Period,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "DECIMAL-POINT",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "IS",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "DECIMAL-POINT",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "COMMA",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "END",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ".",
            PpTokenKind::Period,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "DECIMAL-POINT",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "PERIOD",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "12",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
    ];
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let mut interner = SymbolInterner::new();
    let _ = lex_tokens(&pps, eof_span, &dialect, &mut interner, &mut sink)?;
    assert!(!sink.has_errors());
    Ok(())
}

/// Exercises `DECIMAL-POINT` and `DECIMAL-POINT IS` truncated at EOF.
#[test]
fn lexer_decimal_point_clause_eof_truncation_edges() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let eof_span = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let mut interner = SymbolInterner::new();

    let pps_dp_eof = vec![mk_pp(
        "DECIMAL-POINT",
        PpTokenKind::Word,
        MarginZone::AreaB,
        SpacingHint::Space,
    )];
    let _ = lex_tokens(&pps_dp_eof, eof_span, &dialect, &mut interner, &mut sink)?;

    let pps_dp_is_eof = vec![
        mk_pp(
            "DECIMAL-POINT",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "IS",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
    ];
    let _ = lex_tokens(&pps_dp_is_eof, eof_span, &dialect, &mut interner, &mut sink)?;
    assert!(!sink.has_errors());
    Ok(())
}

/// Exercises comma-decimal non-fusion when separator punctuation, spacing, or token kind prevents fusion.
#[test]
fn lexer_comma_decimal_spacing_and_delimiter_non_fusion() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let eof_span = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let pps = vec![
        mk_pp(
            "DECIMAL-POINT",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "COMMA",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "12",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ";",
            PpTokenKind::SeparatorPunct,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "34",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "12",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ",",
            PpTokenKind::SeparatorPunct,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "34",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "12",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ",",
            PpTokenKind::SeparatorPunct,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "34",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "12",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ",",
            PpTokenKind::SeparatorPunct,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "(",
            PpTokenKind::LParen,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
    ];
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let mut interner = SymbolInterner::new();
    let toks = lex_tokens(&pps, eof_span, &dialect, &mut interner, &mut sink)?;
    assert!(!sink.has_errors());
    assert!(toks.iter().any(|t| t.kind == TokenKind::Semicolon));
    assert!(toks.iter().any(|t| t.kind == TokenKind::Comma));
    Ok(())
}

/// Exercises comma-decimal non-digit operands and embedded sign/dot/operator word segment scanning.
#[test]
fn lexer_comma_decimal_non_digit_and_operator_segments() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let eof_span = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let pps = vec![
        mk_pp(
            "DECIMAL-POINT",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "COMMA",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "+",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ",",
            PpTokenKind::SeparatorPunct,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "34",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "AB",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ",",
            PpTokenKind::SeparatorPunct,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "34",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "12",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            ",",
            PpTokenKind::SeparatorPunct,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            "AB",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Tight,
        ),
        mk_pp(
            ".A",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "123-+",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "1+2",
            PpTokenKind::Word,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
    ];
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let mut interner = SymbolInterner::new();
    let toks = lex_tokens(&pps, eof_span, &dialect, &mut interner, &mut sink)?;
    assert!(toks.iter().any(|t| t.kind == TokenKind::Plus));
    Ok(())
}

/// Exercises all 10 string literal prefixes, quote escaping, dialect checks, and malformed hex/bit/UTF-8 literals.
#[test]
fn lexer_all_ten_string_prefixes_and_error_cases() -> Result<()> {
    let cob23 = DialectConfig::for_dialect(Dialect::Cobol2023);
    let valid_src = concat!(
        r#"MOVE "A""B" 'C''D' "#,
        r#"X"09afAF" H"ABC" Z"HI" B"01000001" B"1010" "#,
        "BX\"F\" N\"AB\" NX\"0041\" U\"UTF\" UX\"41C3A9\" TO R.\n",
    );
    let (toks, _, sink_ok) = lex_snippet(valid_src, SourceFormat::Free, &cob23)?;
    assert!(!sink_ok.has_errors());
    for expected_prefix in [
        StringPrefix::Plain,
        StringPrefix::HexX,
        StringPrefix::HexH,
        StringPrefix::NullTermZ,
        StringPrefix::BitB,
        StringPrefix::BitHexBX,
        StringPrefix::NationalN,
        StringPrefix::NationalHexNX,
        StringPrefix::Utf8U,
        StringPrefix::Utf8HexUX,
    ] {
        assert!(toks.iter().any(|t| matches!(
            t.kind,
            TokenKind::StringLit { prefix, .. } if prefix == expected_prefix
        )));
    }

    // Malformed hex, national hex, UTF-8 hex, binary, unterminated, empty, and length-exceeded literals.
    let long_85 = "A".repeat(161);
    let cob85 = DialectConfig::for_dialect(Dialect::Cobol85);
    let bad_85_src = format!(
        "MOVE \"\" \"{long_85}\" Z\"X\" X\"\" X\"4G\" X\"ABC\" NX\"\" NX\"004G\" NX\"00\" UX\"FFFE\" B\"\" B\"012\" TO R.\n"
    );
    let (_, _, sink_85) = lex_snippet(&bad_85_src, SourceFormat::Free, &cob85)?;
    assert!(sink_85.error_count() >= 11);

    let long_23 = "B".repeat(8192);
    let eof_span = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let raw_str_pps = vec![
        mk_pp(
            &format!(r#""{long_23}""#),
            PpTokenKind::StringLiteral,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            r#""UNTERM"#,
            PpTokenKind::StringLiteral,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            r#""A"B"#,
            PpTokenKind::StringLiteral,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            r#"""#,
            PpTokenKind::StringLiteral,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
        mk_pp(
            "",
            PpTokenKind::StringLiteral,
            MarginZone::AreaB,
            SpacingHint::Space,
        ),
    ];
    let mut s_str = DiagnosticSink::new(WarningPolicy::new(), 50);
    let mut i_str = SymbolInterner::new();
    let _ = lex_tokens(&raw_str_pps, eof_span, &cob23, &mut i_str, &mut s_str)?;
    assert_eq!(s_str.error_count(), 5);
    Ok(())
}

/// Exercises `IDENTIFICATION DIVISION` comment paragraphs, dialect dispositions, and word-length checks.
#[test]
fn lexer_comment_paragraphs_dispositions_and_word_limits() -> Result<()> {
    let fixed_comment_src = concat!(
        "000100 ID DIVISION.\n",
        "000200 AUTHOR. JANE DOE.\n",
        "000300     STILL IN COMMENT PARAGRAPH 123 \"STR\" (X).\n",
        "000400 NONHEADER-AREA-A-WORD.\n",
        "000500 INSTALLATION. HQ.\n",
        "000600 DATE-WRITTEN. TODAY.\n",
        "000700 DATE-COMPILED. TODAY.\n",
        "000800 SECURITY. PUBLIC.\n",
        "000900 REMARKS. NONE.\n",
        "001000 DATA DIVISION.\n",
        "001100 WORKING-STORAGE SECTION.\n",
        "001200 01 AUTHOR PIC X.\n",
        "001300 PROCEDURE DIVISION.\n",
        "001400 SECURITY.\n",
        "001500     STOP RUN.\n",
        "001600 END DECLARATIVES.\n",
    );

    // Test across Cobol85 (Obsolete), Cobol2014 Strict (Unconformable), Cobol2014 Relaxed (Warning), Cobol2023/Gnu (Ok).
    let d85 = DialectConfig::for_dialect(Dialect::Cobol85);
    let (_, _, s85) = lex_snippet(fixed_comment_src, SourceFormat::Fixed, &d85)?;
    assert!(s85.warning_count() >= 1);

    let mut d14_strict = DialectConfig::for_dialect(Dialect::Cobol2014);
    d14_strict.relaxed_syntax = RelaxedSyntaxMode::Strict;
    d14_strict.max_word_length = 8;
    let (_, _, sink_strict) = lex_snippet(fixed_comment_src, SourceFormat::Fixed, &d14_strict)?;
    assert!(sink_strict.has_errors());

    let mut d14_rel = DialectConfig::for_dialect(Dialect::Cobol2014);
    d14_rel.relaxed_syntax = RelaxedSyntaxMode::Relaxed;
    d14_rel.max_word_length = 8;
    let (_, _, sink_relaxed) = lex_snippet(fixed_comment_src, SourceFormat::Fixed, &d14_rel)?;
    assert!(sink_relaxed.warning_count() >= 1);

    let d_gnu = DialectConfig::for_dialect(Dialect::Gnu);
    let (_, _, s_gnu) = lex_snippet(fixed_comment_src, SourceFormat::Fixed, &d_gnu)?;
    assert!(!s_gnu.has_errors());

    // Free-format comment paragraph exit on newline vs inline space, plus all 6 FeatureDispositions.
    let free_src = "IDENTIFICATION DIVISION.\nAUTHOR. INLINE WORDS.\nNONHEADER ON NEWLINE\nAUTHOR INSPACE\nAUTHOR\nPROGRAM-ID. AUTHOR.\n";
    let (_, _, s_free) = lex_snippet(free_src, SourceFormat::Free, &d_gnu)?;
    assert!(!s_free.has_errors());
    let (_, _, s_auth_eof) = lex_snippet(
        "IDENTIFICATION DIVISION.\nAUTHOR",
        SourceFormat::Free,
        &d_gnu,
    )?;
    assert!(!s_auth_eof.has_errors());

    for disp in [
        FeatureDisposition::Ok,
        FeatureDisposition::Ignore,
        FeatureDisposition::Warning,
        FeatureDisposition::Archaic,
        FeatureDisposition::Obsolete,
        FeatureDisposition::Error,
        FeatureDisposition::Unconformable,
    ] {
        let mut custom = DialectConfig::for_dialect(Dialect::Default);
        custom
            .feature_rules
            .insert(String::from("hp-octal-literals"), disp);
        let (_, _, sc) = lex_snippet("MOVE %10 TO X.\n", SourceFormat::Free, &custom)?;
        if matches!(
            disp,
            FeatureDisposition::Error | FeatureDisposition::Unconformable
        ) {
            assert!(sc.has_errors());
        }
    }
    Ok(())
}
