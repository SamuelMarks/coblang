//! Exhaustive unit and branch-coverage tests for `PROCEDURE DIVISION` headers, `DECLARATIVES`, and all 47 verbs.

#![deny(clippy::indexing_slicing)]

use std::path::Path;

use coblang_common::{Dialect, Result, SourceFormat};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap, SymbolInterner, WarningPolicy};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, preprocess_source_file,
};
use coblang_syntax::ast::{
    AcceptSource, CompilationGroup, CompilationUnit, DeleteKind, DisplayAdvancing,
    DivideOrientation, ExitKind, GlobalVisibility, InitializeFillerMode, InitializeValueMode,
    OpenMode, ParamOptionality, PassingMode, PerformControl, PerformTestPosition, ReadDirection,
    SearchMode, SetAction, Stmt, StopKind, TruthValue, UseClause,
};
use coblang_syntax::parse_preprocessed_unit;

/// Preprocesses, lexes, and parses a synthetic COBOL source program.
///
/// # Arguments
///
/// * `src` - Synthetic COBOL source text.
/// * `fmt` - Source reference format (`Free` or `Fixed`).
fn parse_stmt_prog(
    src: &str,
    fmt: SourceFormat,
) -> Result<(CompilationGroup, SymbolInterner, DiagnosticSink)> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let mem = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: fmt,
        dialect_config: &dialect,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 200);
    let unit =
        preprocess_source_file("stmt.cob", Path::new("."), src, &pp_cfg, &mut sm, &mut sink)?;
    let mut interner = SymbolInterner::new();
    let group = parse_preprocessed_unit(&unit, &dialect, &mut interner, &mut sink)?;
    Ok((group, interner, sink))
}

/// Exercises `PROCEDURE DIVISION` header (`USING`/`CHAINING`/`RETURNING`) and `DECLARATIVES` sections.
#[test]
fn procedure_header_and_declaratives_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. PROC-HDR.\n",
        "PROCEDURE DIVISION USING BY REFERENCE A BY VALUE B BY CONTENT OPTIONAL C +\n",
        "    CHAINING D E RETURNING F EXTRA-HDR.\n",
        "DECLARATIVES.\n",
        "+\n",
        "SEC-ERR SECTION 01.\n",
        "    USE GLOBAL AFTER STANDARD ERROR PROCEDURE ON INPUT F1 +\n",
        "    +\n",
        ".\n",
        "    CONTINUE.\n",
        "ERR-PARA.\n",
        "    EXIT.\n",
        "SEC-RPT SECTION.\n",
        "    USE BEFORE REPORTING GRP-1.\n",
        "    CONTINUE.\n",
        "SEC-DBG SECTION.\n",
        "    USE FOR DEBUGGING ON ALL PROCEDURES +\n",
        ".\n",
        "SEC-DBG2 SECTION.\n",
        "    USE DEBUGGING ON P1.\n",
        "END DECLARATIVES.\n",
        "END PROGRAM PROC-HDR.\n",
    );
    let (grp, _, _) = parse_stmt_prog(src, SourceFormat::Free)?;
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    assert_eq!(p.procedure.header.using_params.len(), 3);
    assert_eq!(
        p.procedure.header.using_params.get(1).map(|u| u.mode),
        Some(PassingMode::ByValue)
    );
    assert_eq!(
        p.procedure
            .header
            .using_params
            .get(2)
            .map(|u| (u.mode, u.optionality)),
        Some((PassingMode::ByContent, ParamOptionality::Optional))
    );
    assert_eq!(p.procedure.header.chaining_params.len(), 2);
    assert!(p.procedure.header.returning.is_some());
    assert_eq!(p.procedure.declaratives.len(), 4);
    assert!(matches!(
        p.procedure.declaratives.first().map(|d| &d.use_clause),
        Some(UseClause::AfterException {
            global: GlobalVisibility::Global,
            ..
        })
    ));
    assert!(matches!(
        p.procedure.declaratives.get(1).map(|d| &d.use_clause),
        Some(UseClause::BeforeReporting { .. })
    ));
    assert!(matches!(
        p.procedure.declaratives.get(2).map(|d| &d.use_clause),
        Some(UseClause::ForDebugging { .. })
    ));
    Ok(())
}

/// Exercises `PROCEDURE DIVISION` sections, paragraphs, `NEXT SENTENCE`, `COMMIT`, `ROLLBACK`, and `GIVING` headers.
#[test]
fn procedure_sections_paragraphs_and_giving_header_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. PROC-HDR.\n",
        "PROCEDURE DIVISION.\n",
        ". .\n",
        "+ .\n",
        "BOGUS-UNKNOWN-VERB ARG.\n",
        "0100.\n",
        "    NEXT SENTENCE.\n",
        "MAIN-SEC SECTION 49.\n",
        "    COMMIT.\n",
        "MAIN-PARA.\n",
        "    ROLLBACK.\n",
        "END PROGRAM PROC-HDR.\n",
        "PROGRAM-ID. GIVING-HDR.\n",
        "PROCEDURE DIVISION GIVING R.\n",
        "    STOP RUN.\n",
    );
    let (grp, _, sink) = parse_stmt_prog(src, SourceFormat::Free)?;
    assert!(sink.error_count() >= 2);
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    assert_eq!(p.procedure.paragraphs.len(), 1);
    assert_eq!(p.procedure.sections.len(), 1);
    Ok(())
}

/// Exercises `ACCEPT` and `DISPLAY` statement clauses and exception handlers.
#[test]
fn accept_and_display_verbs_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. ARITH-IO.\n",
        "PROCEDURE DIVISION.\n",
        "    ACCEPT A.\n",
        "    ACCEPT A FROM .\n",
        "    ACCEPT A FROM CONSOLE ON EXCEPTION CONTINUE NOT ON EXCEPTION CONTINUE END-ACCEPT.\n",
        "    ACCEPT A FROM CRT EXCEPTION CONTINUE NOT EXCEPTION CONTINUE END-ACCEPT.\n",
        "    ACCEPT A FROM DATE YYYYMMDD.\n",
        "    ACCEPT A FROM DAY YYYYDDD.\n",
        "    ACCEPT A FROM TIME.\n",
        "    ACCEPT A FROM ARGUMENT-VALUE.\n",
        "    DISPLAY A B AT 10 LINE NUMBER 2 COLUMN 5 COL 8 UPON SYSOUT WITH NO ADVANCING\n",
        "        ON EXCEPTION CONTINUE NOT ON EXCEPTION CONTINUE END-DISPLAY.\n",
        r#"    DISPLAY "HI" NO ADVANCING.
"#,
        r#"    DISPLAY "LN" WITH ADVANCING.
"#,
    );
    let (grp, _, _) = parse_stmt_prog(src, SourceFormat::Free)?;
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let stmts: Vec<&Stmt> = p
        .procedure
        .initial_sentences
        .iter()
        .flat_map(|s| &s.statements)
        .collect();
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Accept {
            source: AcceptSource::SystemClock(_),
            ..
        }
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Accept {
            source: AcceptSource::ProcessInfo(_),
            ..
        }
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Accept {
            source: AcceptSource::Screen,
            ..
        }
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Display {
            advancing: DisplayAdvancing::NoAdvancing,
            ..
        }
    )));
    Ok(())
}

/// Exercises `ADD`, `SUBTRACT`, `MULTIPLY`, `DIVIDE`, and `COMPUTE` verbs.
#[test]
fn arithmetic_and_compute_verbs_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. ARITH-VERBS.\n",
        "PROCEDURE DIVISION.\n",
        "    ADD 1 2 TO A ROUNDED B ROUNDED MODE IS NEAREST-EVEN\n",
        "        NOT ON SIZE ERROR CONTINUE ON SIZE ERROR CONTINUE END-ADD.\n",
        "    ADD A B TO C GIVING D SIZE ERROR CONTINUE NOT SIZE ERROR CONTINUE END-ADD.\n",
        "    ADD CORR G1 TO G2.\n",
        "    SUBTRACT 1 2 FROM A B.\n",
        "    SUBTRACT A FROM B GIVING C ON SIZE ERROR CONTINUE END-SUBTRACT.\n",
        "    SUBTRACT CORRESPONDING G1 FROM G2.\n",
        "    MULTIPLY A BY B C.\n",
        "    MULTIPLY A BY B GIVING C ON SIZE ERROR CONTINUE END-MULTIPLY.\n",
        "    MULTIPLY A B.\n",
        "    DIVIDE A INTO B C.\n",
        "    DIVIDE A INTO B GIVING C REMAINDER D.\n",
        "    DIVIDE A BY B GIVING C ON SIZE ERROR CONTINUE END-DIVIDE.\n",
        "    COMPUTE A ROUNDED = 1 + 2 ON SIZE ERROR CONTINUE END-COMPUTE.\n",
        "    COMPUTE A EQUAL 5.\n",
        "    COMPUTE A .\n",
    );
    let (grp, _, sink) = parse_stmt_prog(src, SourceFormat::Free)?;
    assert!(sink.error_count() >= 2);
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let stmts: Vec<&Stmt> = p
        .procedure
        .initial_sentences
        .iter()
        .flat_map(|s| &s.statements)
        .collect();
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Divide {
            orientation: DivideOrientation::By,
            ..
        }
    )));
    Ok(())
}

/// Exercises branching and loop control-flow verbs: `IF`, `EVALUATE`, `PERFORM`, `CONTINUE`, `ALTER`, and `GO TO`.
#[test]
fn control_flow_branch_and_perform_verbs_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. CTRL-VERBS.\n",
        "PROCEDURE DIVISION.\n",
        "    IF A = B THEN DISPLAY 1 ELSE DISPLAY 2 END-IF.\n",
        "    EVALUATE X ALSO TRUE ALSO A > B\n",
        "        WHEN 1 THRU 5 ALSO A = B ALSO FALSE\n",
        "        WHEN NOT 9 ALSO ANY ALSO TRUE\n",
        "            CONTINUE AFTER 1 SECONDS\n",
        "        WHEN OTHER\n",
        "            CONTINUE\n",
        "    END-EVALUATE.\n",
        "    EVALUATE FALSE WHEN FALSE CONTINUE END-EVALUATE.\n",
        "    PERFORM DISPLAY 1 END-PERFORM.\n",
        "    PERFORM 5 TIMES DISPLAY 1 END-PERFORM.\n",
        "    PERFORM 0100.\n",
        "    PERFORM 0100 TIMES.\n",
        "    PERFORM WS-N TIMES DISPLAY 1 END-PERFORM.\n",
        "    PERFORM P1 WS-N TIMES.\n",
        "    PERFORM WITH TEST AFTER FOREVER EXIT PERFORM END-PERFORM.\n",
        "    PERFORM WITH TEST BEFORE UNTIL A > 5 EXIT PERFORM CYCLE END-PERFORM.\n",
        "    PERFORM VARYING I FROM 1 BY 1 UNTIL I > 5 AFTER J FROM 1 BY 1 UNTIL J > 3 CONTINUE END-PERFORM.\n",
        "    PERFORM P1 THRU P2 UNTIL DONE-FLAG.\n",
        "    PERFORM .\n",
        "    ALTER P1 TO PROCEED TO P2.\n",
        "    GO TO P1 P2 DEPENDING ON IDX.\n",
        "    GO TO.\n",
    );
    let (grp, _, _) = parse_stmt_prog(src, SourceFormat::Free)?;
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let stmts: Vec<&Stmt> = p
        .procedure
        .initial_sentences
        .iter()
        .chain(p.procedure.paragraphs.iter().flat_map(|pa| &pa.sentences))
        .flat_map(|s| &s.statements)
        .collect();
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::PerformInline {
            control: PerformControl::Until {
                test_pos: PerformTestPosition::After,
                ..
            },
            ..
        }
    )));
    Ok(())
}

/// Exercises `EXIT`, `GOBACK`, `STOP`, `ENTRY`, `CALL`, `CANCEL`, `INVOKE`, `RAISE`, and `RESUME` verbs.
#[test]
fn exit_stop_call_invoke_and_exception_verbs_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. CALL-EXC-VERBS.\n",
        "PROCEDURE DIVISION.\n",
        "    EXIT.\n",
        "    EXIT PROGRAM RAISING EXCEPTION LAST EC-USER.\n",
        "    EXIT PARAGRAPH.\n",
        "    EXIT SECTION.\n",
        "    EXIT FUNCTION.\n",
        "    EXIT METHOD.\n",
        "    GOBACK.\n",
        "    GOBACK RETURNING 0.\n",
        "    GOBACK WITH NORMAL STATUS 0.\n",
        "    GOBACK WITH ERROR STATUS 1.\n",
        "    STOP RUN.\n",
        "    STOP RUN RETURNING 0.\n",
        "    STOP RUN WITH NORMAL STATUS 0.\n",
        "    STOP RUN WITH NORMAL STATUS.\n",
        "    STOP RUN WITH ERROR STATUS.\n",
        r#"    STOP "PAUSE".
"#,
        r#"    ENTRY "EP1" USING BY VALUE A.
"#,
        "    ENTRY .\n",
        r#"    CALL PROTOTYPE "SUB" USING BY REFERENCE A BY CONTENT "X" BY VALUE C OMITTED
"#,
        "        RETURNING R NOT ON EXCEPTION CONTINUE ON EXCEPTION CONTINUE END-CALL.\n",
        "    CALL WS-FN GIVING R OVERFLOW CONTINUE NOT OVERFLOW CONTINUE END-CALL.\n",
        r#"    CANCEL "SUB" WS-FN.
"#,
        r#"    INVOKE OBJ "m1" USING BY VALUE 1 RETURNING R END-INVOKE.
"#,
        "    RAISE EXCEPTION EC-ALL.\n",
        "    RAISE.\n",
        "    RESUME AT NEXT STATEMENT.\n",
        "    RESUME AT P1.\n",
        "    RESUME.\n",
    );
    let (grp, _, _) = parse_stmt_prog(src, SourceFormat::Free)?;
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let stmts: Vec<&Stmt> = p
        .procedure
        .initial_sentences
        .iter()
        .chain(p.procedure.paragraphs.iter().flat_map(|pa| &pa.sentences))
        .flat_map(|s| &s.statements)
        .collect();
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Exit {
            kind: ExitKind::Function,
            ..
        }
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Stop {
            kind: StopKind::Literal(_),
            ..
        }
    )));
    Ok(())
}

/// Exercises file I/O verbs: `OPEN`, `CLOSE`, `READ`, `WRITE`, `REWRITE`, `DELETE`, `START`, and `UNLOCK`.
#[test]
fn file_io_verbs_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. DATA-VERBS.\n",
        "PROCEDURE DIVISION.\n",
        "    OPEN INPUT F1 WITH NO REWIND OUTPUT F2 I-O F3 EXTEND F4 123.\n",
        "    CLOSE F1 WITH LOCK F2 REEL FOR REMOVAL 123.\n",
        "    READ F1 NEXT RECORD INTO R WITH NO LOCK NOT AT END CONTINUE AT END CONTINUE END-READ.\n",
        "    READ F1 + .\n",
        "    READ F1 PREVIOUS INTO R END CONTINUE NOT END CONTINUE END-READ.\n",
        "    READ F1 KEY IS K1 INVALID KEY CONTINUE NOT INVALID KEY CONTINUE END-READ.\n",
        "    READ .\n",
        "    WRITE REC1 FROM R BEFORE ADVANCING 2 LINES WITH NO LOCK +\n",
        "        NOT AT END-OF-PAGE CONTINUE AT END-OF-PAGE CONTINUE END-WRITE.\n",
        "    WRITE REC1 AFTER ADVANCING PAGE EOP CONTINUE NOT EOP CONTINUE END-WRITE.\n",
        "    REWRITE REC1 FROM R WITH NO LOCK NOT INVALID KEY CONTINUE INVALID KEY CONTINUE END-REWRITE.\n",
        "    DELETE F1 RECORD ON EXCEPTION CONTINUE NOT ON EXCEPTION CONTINUE END-DELETE.\n",
        "    DELETE FILE F1 F2.\n",
        "    START F1 KEY IS >= K1 INVALID KEY CONTINUE END-START.\n",
        "    START F1 KEY IS NOT < K1.\n",
        "    START F1 KEY IS NOT > K1.\n",
        "    START F1 KEY IS NOT = K1.\n",
        "    START .\n",
        "    UNLOCK F1 RECORD.\n",
        "    UNLOCK .\n",
    );
    let (grp, _, _) = parse_stmt_prog(src, SourceFormat::Free)?;
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let stmts: Vec<&Stmt> = p
        .procedure
        .initial_sentences
        .iter()
        .chain(p.procedure.paragraphs.iter().flat_map(|pa| &pa.sentences))
        .flat_map(|s| &s.statements)
        .collect();
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Open { entries, .. }
            if entries.len() == 4 && entries.iter().any(|(m, _)| *m == OpenMode::Extend)
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Read {
            direction: ReadDirection::Random,
            ..
        }
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Delete {
            kind: DeleteKind::PhysicalFile,
            ..
        }
    )));
    Ok(())
}

/// Exercises `SORT`, `MERGE`, `RELEASE`, `RETURN`, `ALLOCATE`, `FREE`, `MOVE`, `INITIALIZE`, and `SET` verbs.
#[test]
fn sort_merge_memory_and_data_manipulation_verbs_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. SORT-DATA-VERBS.\n",
        "PROCEDURE DIVISION.\n",
        "    SORT SD1 ON ASCENDING KEY K1 WITH DUPLICATES IN ORDER COLLATING SEQUENCE IS A1\n",
        "        INPUT PROCEDURE IS P1 THRU P2 OUTPUT PROCEDURE IS P3.\n",
        "    SORT SD1 DESCENDING K1 USING F1 GIVING F2 EXTRA-SORT.\n",
        "    SORT TBL ON ASCENDING KEY TK1 WITH DUPLICATES.\n",
        "    MERGE SD1 ON ASCENDING KEY K1 COLLATING SEQUENCE A1 USING F1 F2 GIVING F3 EXTRA-MRG.\n",
        "    MERGE SD1 ON DESCENDING KEY K1 USING F1 F2 OUTPUT PROCEDURE IS P1.\n",
        "    MERGE .\n",
        "    RELEASE SREC FROM R.\n",
        "    RETURN SD1 RECORD INTO R AT END CONTINUE NOT AT END CONTINUE END-RETURN.\n",
        "    RETURN .\n",
        "    ALLOCATE 32 CHARACTERS INITIALIZED RETURNING PTR END-ALLOCATE.\n",
        "    ALLOCATE BASED-REC.\n",
        "    FREE PTR1 PTR2.\n",
        "    MOVE CORR G1 TO G2.\n",
        "    MOVE 1 B.\n",
        r#"    INITIALIZE A B WITH FILLER ALL TO VALUE REPLACING ALPHANUMERIC DATA BY "X" DEFAULT +.
"#,
        r#"    SET IX1 TO 1 IX1 UP BY 1 IX2 DOWN BY 2 C88 TO TRUE C89 TO FALSE SW0 TO ON SW1 TO OFF FP TO ENTRY "s" +
"#,
        ".\n",
    );
    let (grp, _, sink) = parse_stmt_prog(src, SourceFormat::Free)?;
    assert!(sink.error_count() >= 1);
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let stmts: Vec<&Stmt> = p
        .procedure
        .initial_sentences
        .iter()
        .chain(p.procedure.paragraphs.iter().flat_map(|pa| &pa.sentences))
        .flat_map(|s| &s.statements)
        .collect();
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::SortTable {
            duplicates_in_order: TruthValue::True,
            ..
        }
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Initialize {
            filler: InitializeFillerMode::IncludeFiller,
            value_mode: InitializeValueMode::DefinedValue,
            ..
        }
    )));
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Set { actions, .. }
            if actions.iter().any(|a| matches!(a, SetAction::SwitchState { .. }))
    )));
    Ok(())
}

/// Exercises `SEARCH`, string (`INSPECT`/`STRING`/`UNSTRING`/`TRANSFORM`), report writer, `JSON`, and `XML` verbs.
#[test]
fn search_string_report_json_and_xml_verbs_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. STR-RPT-XML-VERBS.\n",
        "PROCEDURE DIVISION.\n",
        "    SEARCH TBL VARYING IX AT END CONTINUE WHEN K(IX) = 1 CONTINUE END-SEARCH.\n",
        "    SEARCH ALL TBL END CONTINUE WHEN K(IX) = 1 CONTINUE END-SEARCH.\n",
        r#"    INSPECT S TALLYING C1 FOR CHARACTERS BEFORE INITIAL "." ALL "A" AFTER "B" LEADING "C" TRAILING "D"
"#,
        r#"        REPLACING CHARACTERS BY "X" ALL "A" BY "B" FIRST "C" BY "D" LEADING "E" BY "F" TRAILING "G" BY "H"
"#,
        r#"        CONVERTING "a" TO "A" BEFORE "Z" EXTRA-INSP.
"#,
        r#"    STRING A DELIMITED BY SIZE B DELIMITED BY "," INTO DST WITH POINTER P
"#,
        "        ON OVERFLOW CONTINUE NOT ON OVERFLOW CONTINUE END-STRING.\n",
        "    STRING DELIMITED BY SIZE INTO DST.\n",
        r#"    UNSTRING S DELIMITED BY ALL " " OR "," INTO D1 DELIMITER IN DL1 COUNT IN CN1
"#,
        "        WITH POINTER P TALLYING IN TL ON OVERFLOW CONTINUE END-UNSTRING.\n",
        r#"    TRANSFORM A CHARACTERS FROM "a" TO "A".
"#,
        "    EXHIBIT NAMED CHANGED A.\n",
        "    VALIDATE A B.\n",
        "    INITIATE RPT-1.\n",
        "    GENERATE RPT-1.\n",
        "    GENERATE .\n",
        "    SUPPRESS PRINTING.\n",
        "    TERMINATE RPT-1.\n",
        "    JSON GENERATE DST FROM SRC COUNT IN CN EXTRA-J ON EXCEPTION CONTINUE END-JSON.\n",
        "    JSON PARSE SRC INTO DST EXTRA-JP ON EXCEPTION CONTINUE END-JSON.\n",
        "    XML GENERATE DST FROM SRC COUNT IN CN EXTRA-X ON EXCEPTION CONTINUE END-XML.\n",
        "    XML PARSE SRC EXTRA-XP PROCESSING PROCEDURE IS P1 THRU P2 ON EXCEPTION CONTINUE END-XML.\n",
    );
    let (grp, _, _) = parse_stmt_prog(src, SourceFormat::Free)?;
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let stmts: Vec<&Stmt> = p
        .procedure
        .initial_sentences
        .iter()
        .chain(p.procedure.paragraphs.iter().flat_map(|pa| &pa.sentences))
        .flat_map(|s| &s.statements)
        .collect();
    assert!(stmts.iter().any(|s| matches!(
        s,
        Stmt::Search {
            mode: SearchMode::BinaryAll,
            ..
        }
    )));
    Ok(())
}

/// Exercises all remaining clause boundaries, truncated statements, and header edges in `stmt.rs`.
#[test]
fn remaining_stmt_branch_coverage() -> Result<()> {
    let cases = [
        "PROGRAM-ID. S1.\u{000a}PROCEDURE DIVISION CHAINING C1 RETURNING R1.\u{000a}",
        "PROGRAM-ID. S2.\u{000a}PROCEDURE DIVISION CHAINING + .\u{000a}",
        "PROGRAM-ID. S3.\u{000a}PROCEDURE DIVISION USING U1 CHAINING C1.\u{000a}",
        "PROGRAM-ID. S4.\u{000a}PROCEDURE DIVISION.\u{000a}DECLARATIVES.",
        "PROGRAM-ID. S5.\u{000a}PROCEDURE DIVISION.\u{000a}DECLARATIVES.\u{000a}END PROGRAM S5.\u{000a}",
        "PROGRAM-ID. S5B.\u{000a}PROCEDURE DIVISION.\u{000a}DECLARATIVES.\u{000a}D1 SECTION.\u{000a}USE AFTER ERROR.\u{000a}END DECLARATIVES.\u{000a}",
        "PROGRAM-ID. S6.\u{000a}PROCEDURE DIVISION.\u{000a}END DECLARATIVES.\u{000a}",
        "PROGRAM-ID. S7.\u{000a}PROCEDURE DIVISION.\u{000a}SEC1 SECTION.\u{000a}P1.\u{000a}END-IF.\u{000a}END.\u{000a}",
        "PROGRAM-ID. S9.\u{000a}PROCEDURE DIVISION.\u{000a}EVALUATE X",
        "PROGRAM-ID. S10.\u{000a}PROCEDURE DIVISION.\u{000a}ADD 1 TO (A + B)",
        concat!(
            "PROGRAM-ID. S8.\u{000a}PROCEDURE DIVISION.\u{000a}",
            "    IF TRUE : .\u{000a}",
            "    IF A = B END-IF.\u{000a}",
            "    ACCEPT (1 .\u{000a}",
            "    ACCEPT X FROM DATE.\u{000a}",
            "    ADD 1.\u{000a}",
            "    ADD 1 TO X END-ADD.\u{000a}",
            "    ADD (A + B) TO C MOVE 1 TO D.\u{000a}",
            "    ADD 1 TO X NOT CONTINUE.\u{000a}",
            "    CALL 'F' USING X.\u{000a}",
            "    CALL 'F' USING X MOVE 1 TO Y.\u{000a}",
            "    CLOSE F1.\u{000a}",
            "    DELETE FILE F1 + .\u{000a}",
            "    DELETE + .\u{000a}",
            "    EVALUATE X WHEN 1 WHEN OTHER CONTINUE END-EVALUATE.\u{000a}",
            "    EVALUATE A NUMERIC WHEN TRUE CONTINUE END-EVALUATE.\u{000a}",
            "    EVALUATE (A = B) .\u{000a}",
            "    EXIT PROGRAM.\u{000a}",
            "    GOBACK WITH NORMAL STATUS.\u{000a}",
            "    INITIALIZE X REPLACING + .\u{000a}",
            "    INITIATE + .\u{000a}",
            "    TERMINATE + .\u{000a}",
            "    INVOKE O M.\u{000a}",
            "    INVOKE O M USING OMITTED.\u{000a}",
            "    INVOKE O M USING X ON EXCEPTION CONTINUE END-INVOKE.\u{000a}",
            "    MERGE SD1 ON ASCENDING KEY K1 USING F1 + .\u{000a}",
            "    MERGE SD1 ON ASCENDING KEY K1 USING F1 GIVING F2 + .\u{000a}",
            "    MERGE SD1 ON ASCENDING KEY K1 BOGUS.\u{000a}",
            "    OPEN INPUT F1.\u{000a}",
            "    PERFORM P1 TIMES.\u{000a}",
            "    RELEASE REC1.\u{000a}",
            "    REWRITE REC1.\u{000a}",
            "    SEARCH T AT END CONTINUE WHEN C1 CONTINUE END-SEARCH.\u{000a}",
            "    SEARCH T WHEN C1 CONTINUE END-SEARCH.\u{000a}",
            "    SET X.\u{000a}",
            "    SET TO ON.\u{000a}",
            "    SET TO OFF.\u{000a}",
            "    SET P1 TO ADDRESS OF ENTRY 'SUB1'.\u{000a}",
            "    SET P1 TO ADDRESS OF PROGRAM 'SUB1'.\u{000a}",
            "    ALLOCATE 16 CHARACTERS INITIALIZED TO 'Z' RETURNING P1.\u{000a}",
            "    SORT SD1 ON ASCENDING KEY K1 SEQUENCE IS ALPHA1 USING F1 GIVING F2.\u{000a}",
            "    SORT SD1 ON ASCENDING KEY K1 USING F1 + .\u{000a}",
            "    SORT SD1 ON ASCENDING KEY K1 GIVING F2 + .\u{000a}",
            "    SORT SD1 ON ASCENDING KEY K1 BOGUS.\u{000a}",
            "    STRING A.\u{000a}",
            "    SUBTRACT 1.\u{000a}",
            "    UNSTRING S INTO D1.\u{000a}",
            "    XML PARSE X.\u{000a}",
        ),
    ];
    for c in cases {
        let _ = parse_stmt_prog(c, SourceFormat::Free)?;
    }
    Ok(())
}
