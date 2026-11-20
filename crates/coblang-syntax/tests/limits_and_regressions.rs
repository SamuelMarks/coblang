//! Required OOM / infinite-loop regression tests and `ParserLimits` / `SymbolInterner` safeguard tests.

#![deny(clippy::indexing_slicing)]

use std::path::Path;
use std::time::{Duration, Instant};

use coblang_common::{
    ByteLength, ByteOffset, CoblangError, Dialect, ExpansionId, ResourceLimitKind, Result,
    SourceFormat, SourceSpan,
};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap, SymbolInterner, WarningPolicy};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, PreprocessedUnit,
    preprocess_source_file,
};
use coblang_syntax::ast::{CompilationGroup, CompilationUnit};
use coblang_syntax::{
    ParserLimits, lex_tokens, parse_preprocessed_unit_with_limits, parse_tokens_with_limits,
};

/// Preprocesses a synthetic COBOL snippet into a [`PreprocessedUnit`] for fast repeated lex/parse checks.
///
/// # Arguments
///
/// * `src` - Synthetic COBOL source text.
///
/// # Errors
///
/// Returns [`CoblangError`] if preprocessing fails.
fn make_pp_unit(src: &str) -> Result<PreprocessedUnit> {
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
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 200);
    preprocess_source_file("reg.cob", Path::new("."), src, &pp_cfg, &mut sm, &mut sink)
}

/// Parses `src` while asserting `<10ms` termination and returning `(CompilationGroup, DiagnosticSink)`.
///
/// # Arguments
///
/// * `src` - Synthetic COBOL source text.
///
/// # Errors
///
/// Returns [`CoblangError`] if preprocessing or parsing fails with an internal resource/domain error.
fn assert_terminates_under_10ms(src: &str) -> Result<(CompilationGroup, DiagnosticSink)> {
    let unit = make_pp_unit(src)?;
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let mut interner = SymbolInterner::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 200);
    let t0 = Instant::now();
    let group = parse_preprocessed_unit_with_limits(
        &unit,
        &dialect,
        ParserLimits::default(),
        &mut interner,
        &mut sink,
    )?;
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_millis(10),
        "expected <10ms parse time, observed {elapsed:?}"
    );
    Ok((group, sink))
}

/// Verifies all required OOM / non-advancing cursor regression cases terminate in <10ms with bounded AST size.
#[test]
fn oom_and_cursor_progress_regression_cases() -> Result<()> {
    // 1. Non-identifier `-` operator in MOVE target list (`MOVE LOW-VALUES TO 'A'-'B'.`).
    let (g1, s1) = assert_terminates_under_10ms(
        "PROGRAM-ID. R1.\nPROCEDURE DIVISION.\nMOVE LOW-VALUES TO 'A'-'B'.\n",
    )?;
    assert!(s1.has_errors());
    if let Some(CompilationUnit::Program(p)) = g1.units.first() {
        assert!(p.procedure.initial_sentences.len() <= 2);
    }

    // 2. MS-COBOL screen position tuples in DISPLAY and ACCEPT.
    let (g2, _s2) = assert_terminates_under_10ms(
        "PROGRAM-ID. R2.\nPROCEDURE DIVISION.\nDISPLAY (LIN, COL - 3) FIELD-A.\n",
    )?;
    assert_eq!(g2.units.len(), 1);

    let (g3, s3) =
        assert_terminates_under_10ms("PROGRAM-ID. R3.\nPROCEDURE DIVISION.\nACCEPT ( , 10) A.\n")?;
    assert_eq!(g3.units.len(), 1);
    let _ = s3.error_count();

    // 3. Unclosed `(` before `.` in subscripts, refmod, inline invoke, and FUNCTION call.
    for unclosed in [
        "PROGRAM-ID. U1.\nPROCEDURE DIVISION.\nMOVE T(1 .\n",
        "PROGRAM-ID. U2.\nPROCEDURE DIVISION.\nMOVE T(1: .\n",
        "PROGRAM-ID. U3.\nPROCEDURE DIVISION.\nMOVE O::M(1 .\n",
        "PROGRAM-ID. U4.\nPROCEDURE DIVISION.\nMOVE FUNCTION F(1 .\n",
    ] {
        let (gu, su) = assert_terminates_under_10ms(unclosed)?;
        assert!(su.has_errors());
        if let Some(CompilationUnit::Program(p)) = gu.units.first() {
            assert!(p.procedure.initial_sentences.len() <= 2);
        }
    }

    // 4. Stray `)` in expressions and statements.
    for stray_rparen in [
        "PROGRAM-ID. S1.\nPROCEDURE DIVISION.\nADD 1 ) TO X.\n",
        "PROGRAM-ID. S2.\nPROCEDURE DIVISION.\nDISPLAY ) .\n",
    ] {
        let (gs, ss) = assert_terminates_under_10ms(stray_rparen)?;
        assert!(ss.has_errors());
        if let Some(CompilationUnit::Program(p)) = gs.units.first() {
            assert!(p.procedure.initial_sentences.len() <= 2);
        }
    }

    // 5. Data entry `01 X IS.` before period.
    let (gd, _sd) = assert_terminates_under_10ms(
        "PROGRAM-ID. D1.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 X IS.\n",
    )?;
    if let Some(CompilationUnit::Program(p)) = gd.units.first() {
        assert_eq!(p.data.working_storage.len(), 1);
    }
    Ok(())
}

/// Asserts that parsing `src` under `limits` fails with `CoblangError::ResourceLimitExceeded` of `expected_kind`.
///
/// # Arguments
///
/// * `src` - Synthetic COBOL source text.
/// * `limits` - Explicit parser step/recursion limits.
/// * `expected_kind` - Expected resource limit category (`ParseSteps` or `ParseRecursionDepth`).
///
/// # Errors
///
/// Returns [`CoblangError`] if preprocessing fails.
fn assert_trips_parser_limit(
    src: &str,
    limits: ParserLimits,
    expected_kind: ResourceLimitKind,
) -> Result<()> {
    let unit = make_pp_unit(src)?;
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let mut interner = SymbolInterner::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let res =
        parse_preprocessed_unit_with_limits(&unit, &dialect, limits, &mut interner, &mut sink);
    assert!(
        matches!(
            res,
            Err(CoblangError::ResourceLimitExceeded { resource, .. }) if resource == expected_kind
        ),
        "expected {expected_kind:?} for `{src}` under {limits:?}, got {res:?}"
    );
    Ok(())
}

/// Exercises `ParseSteps` and `ParseRecursionDepth` across expressions, tuples, conditions, statements, and nested units.
#[test]
fn parser_limits_steps_and_expr_cond_stmt_recursion_depth() -> Result<()> {
    assert_trips_parser_limit(
        "PROGRAM-ID. P1.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 A PIC X VALUE \"1\".\n",
        ParserLimits::new(2, 256),
        ResourceLimitKind::ParseSteps,
    )?;
    assert_trips_parser_limit(
        "PROGRAM-ID. P2.\nPROCEDURE DIVISION.\nMOVE A TO B.\n",
        ParserLimits::new(2, 256),
        ResourceLimitKind::ParseSteps,
    )?;

    assert_trips_parser_limit(
        "PROGRAM-ID. D1.\nPROCEDURE DIVISION.\nCOMPUTE X = ((((1)))).\n",
        ParserLimits::new(10_000, 2),
        ResourceLimitKind::ParseRecursionDepth,
    )?;
    assert_trips_parser_limit(
        "PROGRAM-ID. D1B.\nPROCEDURE DIVISION.\nDISPLAY (1, (((2)))) .\n",
        ParserLimits::new(10_000, 2),
        ResourceLimitKind::ParseRecursionDepth,
    )?;
    assert_trips_parser_limit(
        "PROGRAM-ID. D1C.\nPROCEDURE DIVISION.\nCOMPUTE X = (1, (((2)))).\n",
        ParserLimits::new(10_000, 2),
        ResourceLimitKind::ParseRecursionDepth,
    )?;

    assert_trips_parser_limit(
        "PROGRAM-ID. D2.\nPROCEDURE DIVISION.\nIF NOT NOT NOT TRUE CONTINUE END-IF.\n",
        ParserLimits::new(10_000, 2),
        ResourceLimitKind::ParseRecursionDepth,
    )?;
    assert_trips_parser_limit(
        "PROGRAM-ID. D3.\nPROCEDURE DIVISION.\nIF (((A = B))) CONTINUE END-IF.\n",
        ParserLimits::new(10_000, 2),
        ResourceLimitKind::ParseRecursionDepth,
    )?;

    assert_trips_parser_limit(
        "PROGRAM-ID. D4.\nPROCEDURE DIVISION.\nIF TRUE DISPLAY 1 END-IF.\n",
        ParserLimits::new(10_000, 1),
        ResourceLimitKind::ParseRecursionDepth,
    )?;
    assert_trips_parser_limit(
        "PROGRAM-ID. OUTER.\nPROGRAM-ID. INNER1.\nPROGRAM-ID. INNER2.\nEND PROGRAM INNER2.\nEND PROGRAM INNER1.\nEND PROGRAM OUTER.\n",
        ParserLimits::new(10_000, 1),
        ResourceLimitKind::ParseRecursionDepth,
    )?;
    Ok(())
}

/// Exercises `ParseRecursionDepth` inside subscripts, reference modifications, inline invokes, `FUNCTION` calls, and `ACCEPT`.
#[test]
fn parser_limits_subscript_refmod_invoke_and_call_recursion_depth() -> Result<()> {
    for paren_case in [
        "PROGRAM-ID. P.\nPROCEDURE DIVISION.\nMOVE T((((1)))) TO X.\n",
        "PROGRAM-ID. P.\nPROCEDURE DIVISION.\nMOVE T(1, (((2)))) TO X.\n",
        "PROGRAM-ID. P.\nPROCEDURE DIVISION.\nMOVE T(1 : (((2)))) TO X.\n",
        "PROGRAM-ID. P.\nPROCEDURE DIVISION.\nMOVE T(1)((((2))) : 1) TO X.\n",
        "PROGRAM-ID. P.\nPROCEDURE DIVISION.\nMOVE T(1)(1 : (((2)))) TO X.\n",
        "PROGRAM-ID. P.\nPROCEDURE DIVISION.\nCOMPUTE X = O::M((((1)))).\n",
        "PROGRAM-ID. P.\nPROCEDURE DIVISION.\nCOMPUTE X = FUNCTION F((((1)))).\n",
        "PROGRAM-ID. P.\nPROCEDURE DIVISION.\nACCEPT ((((1)))) X.\n",
    ] {
        assert_trips_parser_limit(
            paren_case,
            ParserLimits::new(10_000, 3),
            ResourceLimitKind::ParseRecursionDepth,
        )?;
    }
    Ok(())
}

/// Runs lexer (`0..8`) and parser (`exact_cap..=exact_cap + 12`) `SymbolInterner::with_limit` exhaustion over `samples`.
///
/// # Arguments
///
/// * `samples` - Synthetic COBOL snippets exercising lexer and parser symbol interning.
///
/// # Errors
///
/// Returns [`CoblangError`] if baseline un-capped preprocessing or lexing fails.
fn run_interner_capacity_sweep(samples: &[&str]) -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    for src in samples {
        let unit = make_pp_unit(src)?;
        let eof_span = SourceSpan::new(
            unit.root_file_id,
            ByteOffset::ZERO,
            ByteLength::ZERO,
            ExpansionId::ROOT,
        );
        for cap in 0..8 {
            let mut int_lex = SymbolInterner::with_limit(cap);
            let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
            let _ = lex_tokens(&unit.tokens, eof_span, &dialect, &mut int_lex, &mut sink);
        }
        let mut full_int = SymbolInterner::new();
        let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
        let toks = lex_tokens(&unit.tokens, eof_span, &dialect, &mut full_int, &mut sink)?;
        let exact_cap = u32::try_from(full_int.len()).unwrap_or(u32::MAX);
        for extra in 0..=12 {
            let cap = exact_cap.saturating_add(extra);
            let mut capped_int = SymbolInterner::with_limit(cap);
            let toks_capped =
                lex_tokens(&unit.tokens, eof_span, &dialect, &mut capped_int, &mut sink)?;
            let _ = parse_tokens_with_limits(
                &toks_capped,
                eof_span,
                &dialect,
                ParserLimits::default(),
                &mut capped_int,
                &mut sink,
            );
        }
        let _ = toks.len();
    }
    Ok(())
}

/// Exercises `SymbolInterner::with_limit` exhaustion across lexical modes and compilation-unit identification headers.
#[test]
fn symbol_interner_capacity_exhaustion_lexer_and_headers() -> Result<()> {
    let samples = [
        "01 X PIC IS 9(5).\n",
        "ENVIRONMENT DIVISION. SPECIAL-NAMES. DECIMAL-POINT IS COMMA. PROCEDURE DIVISION. MOVE 12,34 TO X.\n",
        "MOVE %77 TO X.\n",
        "MOVE 1.5E+2 TO X.\n",
        "PROCEDURE DIVISION.\nSTOP RUN.\n",
        "PROGRAM-ID. .\n",
        "FUNCTION-ID. .\n",
        "CLASS-ID. .\n",
        "INTERFACE-ID. .\n",
        "CLASS-ID. C.\nOBJECT.\nPROCEDURE DIVISION.\nMETHOD-ID. .\n",
    ];
    run_interner_capacity_sweep(&samples)
}

/// Exercises `SymbolInterner::with_limit` exhaustion across environment, data, procedure verbs, and fallback expressions.
#[test]
fn symbol_interner_capacity_exhaustion_divisions_and_verbs() -> Result<()> {
    let samples = [
        concat!(
            "PROGRAM-ID. P.\n",
            "DATA DIVISION.\n",
            "WORKING-STORAGE SECTION.\n",
            "01 V1 VALUE + 9.\n",
            "01 V2 VALUE 1 THRU + 2.\n",
            "88 C88 VALUE 1 WHEN SET TO FALSE IS + 3.\n",
            "01 C1 CONSTANT AS + 6.\n",
        ),
        concat!(
            "PROGRAM-ID. P.\n",
            "ENVIRONMENT DIVISION.\n",
            "INPUT-OUTPUT SECTION.\n",
            "SELECT .\n",
            "FILE-CONTROL.\n",
            "SELECT .\n",
            "DATA DIVISION.\n",
            "FILE SECTION.\n",
            "FD .\n",
            "REPORT SECTION.\n",
            "RD .\n",
            "RD R1.\n",
            "01 G1 VALUE IS + 4.\n",
            "SCREEN SECTION.\n",
            "01 S1 VALUE IS + 5.\n",
        ),
        concat!(
            "PROGRAM-ID. P.\n",
            "PROCEDURE DIVISION.\n",
            "ACCEPT A FROM DATE YYYYMMDD.\n",
            "ENTRY .\n",
            "GENERATE .\n",
            "MERGE .\n",
            "READ .\n",
            "RETURN .\n",
            "START .\n",
            "STOP RUN WITH NORMAL STATUS.\n",
            "STOP RUN WITH ERROR STATUS.\n",
            "UNLOCK .\n",
        ),
        concat!(
            "PROGRAM-ID. P.\n",
            "PROCEDURE DIVISION.\n",
            "COMPUTE X = ) + O:: + FUNCTION .\n",
            "IF 1 CONTINUE END-IF.\n",
        ),
    ];
    run_interner_capacity_sweep(&samples)
}

/// Runs `max_depth` (`0..=8`) and `max_steps` (`1..=500`) sweeps over a single preprocessed COBOL program.
///
/// # Arguments
///
/// * `src` - Synthetic COBOL program source text.
///
/// # Errors
///
/// Returns [`CoblangError`] if preprocessing or lexing fails.
fn sweep_program_steps_and_depths(src: &str) -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let unit = make_pp_unit(src)?;
    let eof_span = SourceSpan::new(
        unit.root_file_id,
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let mut interner = SymbolInterner::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 500);
    let toks = lex_tokens(&unit.tokens, eof_span, &dialect, &mut interner, &mut sink)?;

    for depth in 0..=8 {
        let mut d_sink = DiagnosticSink::new(WarningPolicy::new(), 500);
        let _ = parse_tokens_with_limits(
            &toks,
            eof_span,
            &dialect,
            ParserLimits::new(100_000, depth),
            &mut interner,
            &mut d_sink,
        );
    }

    for steps in 1..=500 {
        let mut s_sink = DiagnosticSink::new(WarningPolicy::new(), 500);
        let res = parse_tokens_with_limits(
            &toks,
            eof_span,
            &dialect,
            ParserLimits::new(steps, 256),
            &mut interner,
            &mut s_sink,
        );
        if res.is_ok() {
            break;
        }
    }
    Ok(())
}

/// Runs step and recursion-depth sweeps across all Data Division sections, declaratives, expressions, and conditions.
#[test]
fn exhaustive_step_and_depth_sweep_data_and_exprs() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. SWEEP-DATA.\n",
        "DATA DIVISION.\n",
        "01 BARE-ITEM PIC X.\n",
        "FILE SECTION.\n",
        "FD F1.\n",
        "01 F1-REC PIC X.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 W1 PIC X VALUE \"A\".\n",
        "01 W2 CONSTANT AS 1.\n",
        "LOCAL-STORAGE SECTION.\n",
        "01 L1 PIC 9.\n",
        "LINKAGE SECTION.\n",
        "01 K1 PIC X.\n",
        "REPORT SECTION.\n",
        "RD R1.\n",
        "01 RG1 LINE 1 VALUE \"H\".\n",
        "SCREEN SECTION.\n",
        "01 SC1 LINE 1 VALUE \"S\".\n",
        "PROCEDURE DIVISION.\n",
        "DECLARATIVES.\n",
        "SEC-D SECTION.\n",
        "    USE AFTER ERROR PROCEDURE ON F1.\n",
        "    MOVE W1 TO F1-REC.\n",
        "PARA-D.\n",
        "    MOVE W1 TO F1-REC.\n",
        "END DECLARATIVES.\n",
        "    MOVE W1 TO L1.\n",
        "PARA-TOP.\n",
        "    MOVE W1 TO K1.\n",
        "SEC-MAIN SECTION.\n",
        "    MOVE W1 TO L1.\n",
        "PARA-SEC.\n",
        "    COMPUTE L1 = (1, (((2)))) + +W1 - -L1 * 2 ** 3 & \"X\" + 1.\n",
        "    MOVE T(1, 2)(3 : 4) TO F1-REC.\n",
        "    MOVE FUNCTION UPPER-CASE(W1)(1 : 1) TO F1-REC.\n",
        "    MOVE OBJ::M(1) TO L1.\n",
        "    IF NOT (W1 = \"A\" OR \"B\" AND NOT > \"Z\") AND L1 = 1 OR K1 = \"Y\"\n",
        "        MOVE 1 TO L1\n",
        "    ELSE\n",
        "        MOVE 2 TO L1\n",
        "    END-IF.\n",
    );
    sweep_program_steps_and_depths(src)
}

/// Runs step and recursion-depth sweeps across `ACCEPT` through `INVOKE` Procedure Division verbs.
#[test]
fn exhaustive_step_and_depth_sweep_verbs_part_one() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. SWEEP-VERBS-1.\n",
        "PROCEDURE DIVISION.\n",
        "    ACCEPT (1) A FROM CONSOLE NOT ON EXCEPTION MOVE 1 TO X ON EXCEPTION MOVE 2 TO X END-ACCEPT.\n",
        "    ADD 1 TO A ROUNDED NOT ON SIZE ERROR MOVE 1 TO X ON SIZE ERROR MOVE 2 TO X END-ADD.\n",
        "    ADD 1 TO B GIVING C END-ADD.\n",
        "    ALLOCATE 16 CHARACTERS RETURNING PTR.\n",
        "    ALTER P1 TO PROCEED TO P2.\n",
        "    CALL \"S\" USING A RETURNING R NOT ON EXCEPTION MOVE 1 TO X ON EXCEPTION MOVE 2 TO X END-CALL.\n",
        "    CANCEL \"S\".\n",
        "    COMPUTE A ROUNDED = 1 + 2 NOT ON SIZE ERROR MOVE 1 TO X ON SIZE ERROR MOVE 2 TO X END-COMPUTE.\n",
        "    CONTINUE AFTER 1 SECONDS.\n",
        "    DELETE F1 NOT INVALID KEY MOVE 1 TO X INVALID KEY MOVE 2 TO X END-DELETE.\n",
        "    DISPLAY AT 1 A NOT ON EXCEPTION MOVE 1 TO X ON EXCEPTION MOVE 2 TO X END-DISPLAY.\n",
        "    DIVIDE A INTO B END-DIVIDE.\n",
        "    DIVIDE A BY B GIVING C REMAINDER D NOT ON SIZE ERROR MOVE 1 TO X ON SIZE ERROR MOVE 2 TO X END-DIVIDE.\n",
        "    EVALUATE TRUE ALSO X\n",
        "        WHEN A = B ALSO 1 THRU 2 MOVE 1 TO X\n",
        "        WHEN NOT (C = D) ALSO TRUE MOVE 2 TO X\n",
        "        WHEN OTHER MOVE 3 TO X\n",
        "    END-EVALUATE.\n",
        "    EXHIBIT A.\n",
        "    FREE PTR.\n",
        "    GO TO P1 DEPENDING ON X.\n",
        "    GOBACK RETURNING 0.\n",
        "    INITIALIZE A REPLACING NUMERIC DATA BY 0.\n",
        "    INSPECT A TALLYING C FOR ALL \"X\" BEFORE \"Y\" REPLACING ALL \"A\" BY \"B\" AFTER \"Z\" CONVERTING \"a\" TO \"A\" BEFORE \"Q\".\n",
        "    INVOKE OBJ \"M\" USING A RETURNING R END-INVOKE.\n",
    );
    sweep_program_steps_and_depths(src)
}

/// Runs step and recursion-depth sweeps across `JSON` through `XML` Procedure Division verbs.
#[test]
fn exhaustive_step_and_depth_sweep_verbs_part_two() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. SWEEP-VERBS-2.\n",
        "PROCEDURE DIVISION.\n",
        "    JSON GENERATE D FROM S COUNT IN C NOT ON EXCEPTION MOVE 1 TO X ON EXCEPTION MOVE 2 TO X END-JSON.\n",
        "    JSON PARSE S INTO D NOT ON EXCEPTION MOVE 1 TO X ON EXCEPTION MOVE 2 TO X END-JSON.\n",
        "    MERGE SD1 ON ASCENDING KEY K1 USING F1 OUTPUT PROCEDURE IS P1 THRU P2.\n",
        "    MULTIPLY A BY B END-MULTIPLY.\n",
        "    MULTIPLY A BY B GIVING C NOT ON SIZE ERROR MOVE 1 TO X ON SIZE ERROR MOVE 2 TO X END-MULTIPLY.\n",
        "    PERFORM UNTIL A = B MOVE 1 TO X END-PERFORM.\n",
        "    PERFORM VARYING I FROM 1 BY 1 UNTIL I > 2 AFTER J FROM 1 BY 1 UNTIL J > 2 MOVE 1 TO X END-PERFORM.\n",
        "    PERFORM X TIMES MOVE 1 TO X END-PERFORM.\n",
        "    PERFORM P1 THRU P2 X TIMES.\n",
        "    RAISE EXCEPTION X.\n",
        "    READ F1 INTO R KEY IS K1 NOT AT END MOVE 1 TO X AT END MOVE 2 TO X END-READ.\n",
        "    RELEASE REC FROM X.\n",
        "    RESUME AT P1.\n",
        "    RETURN SD1 INTO R NOT AT END MOVE 1 TO X AT END MOVE 2 TO X END-RETURN.\n",
        "    REWRITE REC FROM X NOT INVALID KEY MOVE 1 TO X INVALID KEY MOVE 2 TO X END-REWRITE.\n",
        "    SEARCH TBL VARYING IX AT END MOVE 1 TO X WHEN A = B MOVE 2 TO X END-SEARCH.\n",
        "    SET IX UP BY 1 IX DOWN BY 1 PTR TO X.\n",
        "    SORT SD1 ON ASCENDING KEY K1 INPUT PROCEDURE IS P1 OUTPUT PROCEDURE IS P2.\n",
        "    START F1 KEY IS = K1 NOT INVALID KEY MOVE 1 TO X INVALID KEY MOVE 2 TO X END-START.\n",
        "    STOP RUN RETURNING 0.\n",
        "    STOP RUN WITH NORMAL STATUS 0.\n",
        "    STOP \"H\".\n",
        "    STRING A DELIMITED BY \",\" INTO D WITH POINTER P NOT ON OVERFLOW MOVE 1 TO X ON OVERFLOW MOVE 2 TO X END-STRING.\n",
        "    SUBTRACT A FROM B END-SUBTRACT.\n",
        "    SUBTRACT A FROM B GIVING C NOT ON SIZE ERROR MOVE 1 TO X ON SIZE ERROR MOVE 2 TO X END-SUBTRACT.\n",
        "    TRANSFORM A FROM \"a\" TO \"A\".\n",
        "    UNSTRING S DELIMITED BY \",\" INTO D DELIMITER IN DL COUNT IN CN WITH POINTER P TALLYING IN TL NOT ON OVERFLOW MOVE 1 TO X ON OVERFLOW MOVE 2 TO X END-UNSTRING.\n",
        "    VALIDATE A.\n",
        "    WRITE REC FROM X AFTER ADVANCING 1 LINES NOT AT EOP MOVE 1 TO X AT EOP MOVE 2 TO X END-WRITE.\n",
        "    XML GENERATE D FROM S COUNT IN C NOT ON EXCEPTION MOVE 1 TO X ON EXCEPTION MOVE 2 TO X END-XML.\n",
        "    XML PARSE S PROCESSING PROCEDURE IS P1 THRU P2 NOT ON EXCEPTION MOVE 1 TO X ON EXCEPTION MOVE 2 TO X END-XML.\n",
    );
    sweep_program_steps_and_depths(src)
}

/// Runs step and recursion-depth sweeps across `FUNCTION-ID`, `CLASS-ID`, `FACTORY`, `OBJECT`, `METHOD-ID`, and `INTERFACE-ID`.
#[test]
fn exhaustive_step_and_depth_sweep_oo_and_functions() -> Result<()> {
    let src = concat!(
        "FUNCTION-ID. SWEEP-FN.\n",
        "ENVIRONMENT DIVISION.\n",
        "INPUT-OUTPUT SECTION.\n",
        "SELECT F1 ASSIGN TO \"f1\".\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 X PIC 9.\n",
        "PROCEDURE DIVISION.\n",
        "    GOBACK.\n",
        "END FUNCTION SWEEP-FN.\n",
        "CLASS-ID. SWEEP-CLS.\n",
        "ENVIRONMENT DIVISION.\n",
        "FACTORY.\n",
        "ENVIRONMENT DIVISION.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 FX PIC 9.\n",
        "PROCEDURE DIVISION.\n",
        "METHOD-ID. FM.\n",
        "ENVIRONMENT DIVISION.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 MX PIC 9.\n",
        "PROCEDURE DIVISION.\n",
        "    EXIT METHOD.\n",
        "END METHOD.\n",
        "END FACTORY.\n",
        "OBJECT.\n",
        "PROCEDURE DIVISION.\n",
        "METHOD-ID. OM.\n",
        "PROCEDURE DIVISION.\n",
        "    EXIT METHOD.\n",
        "END METHOD.\n",
        "END OBJECT.\n",
        "END CLASS SWEEP-CLS.\n",
        "INTERFACE-ID. SWEEP-IF.\n",
        "ENVIRONMENT DIVISION.\n",
        "PROCEDURE DIVISION.\n",
        "METHOD-ID. IM.\n",
        "PROCEDURE DIVISION.\n",
        "    EXIT METHOD.\n",
        "END METHOD.\n",
        "END INTERFACE SWEEP-IF.\n",
    );
    sweep_program_steps_and_depths(src)
}
