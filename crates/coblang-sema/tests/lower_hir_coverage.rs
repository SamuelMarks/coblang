//! Exhaustive unit and integration tests for `coblang_sema::lower_hir`.

use std::path::Path;

use coblang_common::{
    ByteLength, ByteOffset, Dialect, ExpansionId, FileId, Result, SourceFormat, SourceSpan,
    SymbolId,
};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap, SymbolInterner, WarningPolicy};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, preprocess_source_file,
};
use coblang_sema::lower_compilation_group;
use coblang_syntax::ast::{
    CallArg, CompilationGroup, CompilationUnit, CorrespondingMode, DivideOrientation, Expr,
    FunctionCallSyntax, PassingMode, RoundedTarget, RoundingSpec, Sentence, SizeErrorHandlers,
    Stmt, SubscriptItem, ValueAtom,
};
use coblang_syntax::parse_preprocessed_unit;

/// Dummy span helper for synthetic AST edge cases.
fn span() -> SourceSpan {
    SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::new(1),
        ExpansionId::ROOT,
    )
}

/// Preprocesses and parses `src` into `(CompilationGroup, SymbolInterner, DiagnosticSink)`.
///
/// # Arguments
///
/// * `src` - Free-format COBOL source text.
/// * `dialect` - Target COBOL dialect.
///
/// # Errors
///
/// Returns an error if preprocessing or parsing exceeds resource limits.
fn parse_only(
    src: &str,
    dialect: Dialect,
) -> Result<(CompilationGroup, SymbolInterner, DiagnosticSink)> {
    let dcfg = DialectConfig::for_dialect(dialect);
    let mem = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dcfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut pol = WarningPolicy::new();
    pol.apply_cli_flag("-Wall")?;
    let mut sink = DiagnosticSink::new(pol, 500);
    let unit = preprocess_source_file(
        "lower_test.cob",
        Path::new("."),
        src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let mut interner = SymbolInterner::new();
    let grp = parse_preprocessed_unit(&unit, &dcfg, &mut interner, &mut sink)?;
    Ok((grp, interner, sink))
}

/// Preprocesses, parses, and lowers `src` into `(Option<ResolvedHirModule>, DiagnosticSink)`.
///
/// # Arguments
///
/// * `src` - Free-format COBOL source text.
/// * `dialect` - Target COBOL dialect.
///
/// # Errors
///
/// Returns an error only if preprocessing or parsing exceeds configured resource limits.
fn lower_cobol(
    src: &str,
    dialect: Dialect,
) -> Result<(Option<coblang_hir::ResolvedHirModule>, DiagnosticSink)> {
    let dcfg = DialectConfig::for_dialect(dialect);
    let (grp, mut interner, mut sink) = parse_only(src, dialect)?;
    match lower_compilation_group(&grp, &dcfg, &mut interner, &mut sink) {
        Ok(hir) => Ok((Some(hir), sink)),
        Err(coblang_common::CoblangError::SemanticError { .. }) => Ok((None, sink)),
        Err(other) => Err(other),
    }
}

/// Covers `PROGRAM-ID`, `REPORT SECTION`, `DECLARATIVES`, `SPECIAL-NAMES`, sections, and nested programs.
#[test]
fn program_special_names_declaratives_and_nested_programs() -> Result<()> {
    let src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. MAIN-UNIT.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SPECIAL-NAMES.
    SYSERR IS MY-ERR-DEV
    ALPHABET MY-ALPHA IS STANDARD-1
    SYMBOLIC CHARACTERS SYM-A SYM-BIG ARE 66 999
    CLASS CLS-DIGITS IS "0" THRU "9"
    CURRENCY SIGN IS "EUR" WITH PICTURE SYMBOL "E"
    CONSOLE IS CRT
    SWITCH-0 IS SW-ZERO ON STATUS IS SW-ON OFF STATUS IS SW-OFF.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-MAIN ASSIGN TO "main.dat".
DATA DIVISION.
FILE SECTION.
FD F-MAIN REPORT IS REP-ONE.
01 F-MAIN-REC PIC X(10).
WORKING-STORAGE SECTION.
01 WS-AMT PIC E99 VALUE 12.
REPORT SECTION.
RD REP-ONE PAGE LIMIT IS 10.
01 TYPE DETAIL LINE 1.
   05 COL 1 PIC X(4) SOURCE F-MAIN-REC.
PROCEDURE DIVISION.
DECLARATIVES.
DECL-SEC SECTION.
    USE AFTER STANDARD ERROR PROCEDURE ON F-MAIN.
    DISPLAY "DECL-INIT".
DECL-PARA.
    DISPLAY "DECL-PARA".
END DECLARATIVES.
    DISPLAY "ENTRY" SYM-A SYM-BIG.
TOP-PARA.
    DISPLAY "TOP".
MAIN-SEC SECTION.
    DISPLAY "SEC-INIT".
SEC-PARA.
    STOP RUN.

IDENTIFICATION DIVISION.
PROGRAM-ID. SUB-COMMON IS COMMON.
PROCEDURE DIVISION.
    GOBACK.
END PROGRAM SUB-COMMON.

IDENTIFICATION DIVISION.
PROGRAM-ID. SUB-LOCAL.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SPECIAL-NAMES.
    CURRENCY SIGN IS "$".
PROCEDURE DIVISION.
    GOBACK RETURNING 0.
END PROGRAM SUB-LOCAL.
END PROGRAM MAIN-UNIT.
"#;
    let (hir_opt, sink) = lower_cobol(src, Dialect::Cobol2023)?;
    assert!(!sink.has_errors());
    let hir = hir_opt.ok_or_else(|| coblang_common::CoblangError::InvalidCli {
        message: String::from("expected resolved HIR"),
    })?;
    assert_eq!(hir.0.programs.len(), 1);
    assert_eq!(hir.0.programs.first().map_or(0, |p| p.nested.len()), 2);
    Ok(())
}

/// Covers `FUNCTION-ID`, `CLASS-ID` (`FACTORY` & `OBJECT`), and `INTERFACE-ID` HIR lowering.
#[test]
fn function_class_and_interface_units_lowering() -> Result<()> {
    let src = r"IDENTIFICATION DIVISION.
FUNCTION-ID. FN-DOUBLE.
DATA DIVISION.
LINKAGE SECTION.
01 LK-IN PIC 9(4).
01 LK-OUT PIC 9(4).
PROCEDURE DIVISION USING LK-IN RETURNING LK-OUT.
    COMPUTE LK-OUT = LK-IN * 2.
    GOBACK.
END FUNCTION FN-DOUBLE.

IDENTIFICATION DIVISION.
CLASS-ID. CLS-BASE.
END CLASS CLS-BASE.

IDENTIFICATION DIVISION.
CLASS-ID. CLS-SAMPLE INHERITS CLS-BASE.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
REPOSITORY.
    CLASS CLS-BASE.
FACTORY.
PROCEDURE DIVISION.
METHOD-ID. FAC-METH.
PROCEDURE DIVISION.
    GOBACK.
END METHOD FAC-METH.
END FACTORY.
OBJECT.
PROCEDURE DIVISION.
METHOD-ID. OBJ-METH.
PROCEDURE DIVISION.
    GOBACK.
END METHOD OBJ-METH.
END OBJECT.
END CLASS CLS-SAMPLE.

IDENTIFICATION DIVISION.
INTERFACE-ID. IFC-BASE.
END INTERFACE IFC-BASE.

IDENTIFICATION DIVISION.
INTERFACE-ID. IFC-SAMPLE INHERITS IFC-BASE.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
REPOSITORY.
    INTERFACE IFC-BASE.
PROCEDURE DIVISION.
METHOD-ID. IFC-METH.
PROCEDURE DIVISION.
    GOBACK.
END METHOD IFC-METH.
END INTERFACE IFC-SAMPLE.
";
    let (hir_opt, sink) = lower_cobol(src, Dialect::Cobol2023)?;
    assert!(!sink.has_errors());
    let hir = hir_opt.ok_or_else(|| coblang_common::CoblangError::InvalidCli {
        message: String::from("expected resolved HIR"),
    })?;
    assert_eq!(hir.0.functions.len(), 1);
    assert_eq!(hir.0.classes.len(), 4);
    Ok(())
}

/// Covers `SymbolInterner` capacity exhaustion across `BuiltinSymbols::new` and all unit kinds.
#[test]
fn interner_capacity_exhaustion_across_all_unit_kinds() -> Result<()> {
    let dcfg = DialectConfig::for_dialect(Dialect::Cobol2023);
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 20);

    // 1. Exhaustion inside `BuiltinSymbols::new` (limit < 52).
    let (grp_prog, _, _) = parse_only(
        "IDENTIFICATION DIVISION.
PROGRAM-ID. P.
PROCEDURE DIVISION.
STOP RUN.
",
        Dialect::Cobol2023,
    )?;
    assert!(
        lower_compilation_group(
            &grp_prog,
            &dcfg,
            &mut SymbolInterner::with_limit(4),
            &mut sink
        )
        .is_err()
    );

    // 2. Exhaustion inside `Program` (`solve_data_division` special registers need > 52 symbols).
    assert!(
        lower_compilation_group(
            &grp_prog,
            &dcfg,
            &mut SymbolInterner::with_limit(55),
            &mut sink
        )
        .is_err()
    );

    // 3. Exhaustion inside `Function`, `Class`, and `Interface` method lowering.
    let (grp_fn, _, _) = parse_only(
        "IDENTIFICATION DIVISION.
FUNCTION-ID. F.
PROCEDURE DIVISION.
GOBACK.
END FUNCTION F.
",
        Dialect::Cobol2023,
    )?;
    assert!(
        lower_compilation_group(
            &grp_fn,
            &dcfg,
            &mut SymbolInterner::with_limit(55),
            &mut sink
        )
        .is_err()
    );

    let (grp_cls, _, _) = parse_only(
        "IDENTIFICATION DIVISION.
CLASS-ID. C.
OBJECT.
PROCEDURE DIVISION.
METHOD-ID. M.
PROCEDURE DIVISION.
GOBACK.
END METHOD M.
END OBJECT.
END CLASS C.
",
        Dialect::Cobol2023,
    )?;
    assert!(
        lower_compilation_group(
            &grp_cls,
            &dcfg,
            &mut SymbolInterner::with_limit(55),
            &mut sink
        )
        .is_err()
    );

    let (grp_ifc, _, _) = parse_only(
        "IDENTIFICATION DIVISION.
INTERFACE-ID. I.
PROCEDURE DIVISION.
METHOD-ID. M.
PROCEDURE DIVISION.
GOBACK.
END METHOD M.
END INTERFACE I.
",
        Dialect::Cobol2023,
    )?;
    assert!(
        lower_compilation_group(
            &grp_ifc,
            &dcfg,
            &mut SymbolInterner::with_limit(55),
            &mut sink
        )
        .is_err()
    );
    Ok(())
}

/// Covers arithmetic (`ADD`, `SUBTRACT`, `MULTIPLY`, `DIVIDE`, `COMPUTE`) and `CORRESPONDING` desugaring.
#[test]
fn arithmetic_and_corresponding_desugaring_coverage() -> Result<()> {
    let src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. ARITH-LOWER.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 A PIC S9(4) VALUE 10.
01 B PIC S9(4) VALUE 20.
01 C PIC S9(4) VALUE 0.
01 ED PIC Z(4)9.
01 S1 PIC X(6) VALUE "HELLO ".
01 GRP-SRC.
   05 FILLER PIC X(2).
   05 NUM-F PIC 9(3).
   05 MIX-NUM PIC 9(2).
   05 STR-F PIC X(3).
   05 REDEF-F REDEFINES STR-F PIC X(3).
   05 REDEF-ONLY PIC X(3).
   05 DIM-ONLY PIC X(2).
   05 TBL-F OCCURS 2 TIMES INDEXED BY IDX-A PIC X(2).
   05 SUB-GRP.
      10 LEAF-X PIC X(2).
   05 MISMATCH-F PIC X(2).
   05 GRP-TO-EL.
      10 G-LEAF PIC X.
01 GRP-DST.
   05 FILLER PIC 9(2).
   05 NUM-F PIC 9(3).
   05 MIX-NUM PIC X(2).
   05 STR-F PIC X(3).
   05 REDEF-F REDEFINES STR-F PIC X(3).
   05 REDEF-ONLY REDEFINES STR-F PIC X(3).
   05 DIM-ONLY OCCURS 2 TIMES PIC X(2).
   05 TBL-F OCCURS 2 TIMES PIC X(2).
   05 SUB-GRP.
      10 LEAF-X PIC X(2).
   05 MISMATCH-F.
      10 INNER-M PIC X(2).
   05 GRP-TO-EL PIC X.
PROCEDURE DIVISION.
    ADD A B TO C ROUNDED, A
        ON SIZE ERROR DISPLAY "SE"
        NOT ON SIZE ERROR DISPLAY "OK"
    END-ADD.
    ADD A TO B GIVING C ROUNDED, ED.
    ADD A B GIVING C.
    ADD CORRESPONDING GRP-SRC TO GRP-DST ROUNDED.
    SUBTRACT A B FROM C.
    SUBTRACT A FROM B GIVING C ROUNDED.
    SUBTRACT CORRESPONDING GRP-SRC FROM GRP-DST.
    MULTIPLY A BY B.
    MULTIPLY A BY B GIVING C ROUNDED.
    DIVIDE A INTO B.
    DIVIDE A INTO B GIVING C REMAINDER A.
    DIVIDE A BY B GIVING ED REMAINDER B.
    COMPUTE C ROUNDED = +A + (-B) - (A * B / 2 ** 1) + (IDX-A + 1) + (1 + IDX-A).
    MOVE "HI" & S1(1:2) & S1(2:) & LEAF-X OF SUB-GRP OF GRP-SRC TO S1.
    MOVE CORRESPONDING GRP-SRC TO GRP-DST.
    SET IDX-A TO 1.
    STOP RUN.
"#;
    let (hir_opt, sink) = lower_cobol(src, Dialect::Cobol2023)?;
    assert!(!sink.has_errors());
    assert!(hir_opt.is_some());
    Ok(())
}

/// Covers negative/error paths in `MOVE`, `MOVE CORRESPONDING`, `ADD CORRESPONDING`, and unresolved places.
#[test]
fn move_and_arith_corresponding_error_branches() -> Result<()> {
    let err_src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. CORR-ERR.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 ELEM-A PIC 9(3).
01 ELEM-B PIC 9(3).
   88 IS-ZERO VALUE 0.
01 GRP-OK.
   05 ELEM-A PIC 9(3).
PROCEDURE DIVISION.
    MOVE 123 TO UNDEF-DST, IS-ZERO.
    MOVE CORRESPONDING "LIT" TO GRP-OK.
    MOVE CORRESPONDING IS-ZERO TO GRP-OK.
    MOVE CORRESPONDING UNDEF-SRC TO GRP-OK.
    MOVE CORRESPONDING ELEM-A TO ELEM-B, UNDEF-DST, GRP-OK.
    ADD CORRESPONDING UNDEF-SRC TO GRP-OK.
    ADD CORRESPONDING GRP-OK TO UNDEF-DST.
    ADD 1 TO UNDEF-DST.
    COMPUTE UNDEF-DST = UNDEF-EXPR-VAR.
    DIVIDE 2 INTO 4 GIVING ELEM-A REMAINDER UNDEF-REM.
    STOP RUN.
"#;
    let (hir_opt, sink) = lower_cobol(err_src, Dialect::Cobol2023)?;
    assert!(hir_opt.is_none());
    assert!(sink.error_count() >= 8);
    Ok(())
}

/// Covers conditions (`88`, switches, class/sign/omitted checks, abbreviated relations) and control flow.
#[test]
fn conditions_and_control_flow_lowering_coverage() -> Result<()> {
    let src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. COND-FLOW.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SPECIAL-NAMES.
    SWITCH-1 IS SW1-MNEM ON STATUS IS SW1-ON
    CLASS CLS-ALPHA IS "A" THRU "Z".
DATA DIVISION.
WORKING-STORAGE SECTION.
78 K-TEN VALUE 10.
78 K-TXT VALUE "AB".
78 K-FLT VALUE 1.5E1.
01 NUM-X PIC S9(3) VALUE 5.
   88 X-SINGLE VALUE 5.
   88 X-MULTI VALUE 1, 10 THRU 20.
01 STR-X PIC X(4) VALUE "ABCD".
01 TBL-G.
   05 TBL-EL OCCURS 3 TIMES PIC 9(2).
LINKAGE SECTION.
01 LK-OPT PIC X(2).
PROCEDURE DIVISION USING OPTIONAL LK-OPT.
P-START.
    IF TRUE AND NOT FALSE OR X-SINGLE OR X-MULTI OR SW1-ON
        DISPLAY "B1" WITH NO ADVANCING
    ELSE
        DISPLAY "B2"
    END-IF.
    IF LK-OPT IS OMITTED OR LK-OPT IS NOT OMITTED
        DISPLAY K-TEN K-TXT K-FLT
    END-IF.
    IF NUM-X IS POSITIVE OR NUM-X IS NEGATIVE OR NUM-X IS NOT ZERO
        DISPLAY ADDRESS OF STR-X LENGTH OF STR-X
    END-IF.
    IF STR-X IS NUMERIC OR STR-X IS ALPHABETIC
        OR STR-X IS ALPHABETIC-LOWER OR STR-X IS ALPHABETIC-UPPER
        OR STR-X IS KANJI OR STR-X IS NOT CLS-ALPHA
        DISPLAY N"AB" NX"0041" U"CD" UX"42" 1.25 2.5E2 ZERO SPACES
    END-IF.
    IF NUM-X = 1 OR > 2 AND NOT < 3 OR <= 4 OR >= 5 OR <> 6 OR NOT X-SINGLE
        PERFORM P-MID
        PERFORM P-MID THRU P-END
    END-IF.
    PERFORM DISPLAY "ONCE" END-PERFORM.
    PERFORM 2 TIMES DISPLAY "T" END-PERFORM.
    PERFORM WITH TEST BEFORE UNTIL NUM-X > 9 DISPLAY "U1" END-PERFORM.
    PERFORM WITH TEST AFTER UNTIL NUM-X = 0 DISPLAY "U2" END-PERFORM.
    PERFORM WITH TEST BEFORE VARYING NUM-X FROM 1 BY 1 UNTIL NUM-X > 3 DISPLAY "V1" END-PERFORM.
    PERFORM WITH TEST AFTER VARYING NUM-X FROM 1 BY 1 UNTIL NUM-X > 2 DISPLAY "V2" END-PERFORM.
    GO TO P-MID P-END DEPENDING ON NUM-X.
P-MID.
    CALL "EXT-SUB" USING BY REFERENCE STR-X OMITTED RETURNING NUM-X
        ON EXCEPTION DISPLAY "EX"
        NOT ON EXCEPTION DISPLAY "OK"
    END-CALL.
    CALL "EXT-NO-RET".
    GO TO P-END.
P-END.
    STOP "DONE".
    STOP RUN RETURNING 0.
"#;
    let (hir_opt, sink) = lower_cobol(src, Dialect::Gnu)?;
    assert_eq!(sink.diagnostics(), &[]);
    assert!(hir_opt.is_some());
    Ok(())
}

/// Covers invalid conditions, unresolved `PERFORM` / `GO TO` / `OMITTED`, and intrinsics.
#[test]
fn invalid_conditions_and_intrinsics_coverage() -> Result<()> {
    let ok_intrinsics = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. INTR-OK.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
REPOSITORY.
    FUNCTION ALL INTRINSIC.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 S-OUT PIC X(20).
01 N-OUT PIC N(10).
01 NUM-OUT PIC 9(4).
01 TBL-NUM.
   05 ITEM-N OCCURS 3 TIMES PIC 9(2).
PROCEDURE DIVISION.
    MOVE FUNCTION TRIM(" HI ") TO S-OUT.
    MOVE FUNCTION NATIONAL-OF("AB") TO N-OUT.
    COMPUTE NUM-OUT = FUNCTION SUM(ITEM-N(ALL)) + FUNCTION LENGTH("ABC") + FUNCTION ORD-MAX(ALL).
    STOP RUN.
"#;
    let (hir_ok, sink_ok) = lower_cobol(ok_intrinsics, Dialect::Cobol2023)?;
    assert!(!sink_ok.has_errors());
    assert!(hir_ok.is_some());

    let bad_conds = r"IDENTIFICATION DIVISION.
PROGRAM-ID. COND-BAD.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 PLAIN-VAR PIC 9(4).
01 OTHER-VAR PIC 9(4).
PROCEDURE DIVISION.
    IF PLAIN-VAR = 1 OR OTHER-VAR OR NO-SUCH-ABBREV
        CONTINUE
    END-IF.
    IF PLAIN-VAR OR NO-SUCH-COND OR NO-SUCH-OMIT IS OMITTED
        PERFORM NO-SUCH-P1
        PERFORM PLAIN-PARA THRU NO-SUCH-P2
        GO TO NO-SUCH-P3
    END-IF.
PLAIN-PARA.
    STOP RUN.
";
    let (hir_bad, sink_bad) = lower_cobol(bad_conds, Dialect::Cobol2023)?;
    assert!(hir_bad.is_none());
    assert!(sink_bad.error_count() >= 5);
    Ok(())
}

/// Covers all checked high-level verbs (`ACCEPT`, `ALLOCATE`, `ALTER`, `CANCEL`, `EVALUATE`,
/// `EXHIBIT`, `FREE`, `INITIALIZE`, `INSPECT`, `READ`, `REWRITE`, `SEARCH`, `STRING`, `UNSTRING`,
/// `VALIDATE`, `WRITE`).
#[test]
fn checked_high_level_verbs_lowering_coverage() -> Result<()> {
    let src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. HIGH-CHECKED.
ENVIRONMENT DIVISION.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-IO ASSIGN TO "io.dat"
        ORGANIZATION IS INDEXED
        ACCESS MODE IS DYNAMIC
        RECORD KEY IS IO-KEY.
DATA DIVISION.
FILE SECTION.
FD F-IO.
01 IO-REC.
   05 IO-KEY PIC X(4).
   05 IO-BODY PIC X(6).
WORKING-STORAGE SECTION.
01 WS-BUF PIC X(10).
01 WS-CNT PIC 9(4).
01 WS-PTR USAGE POINTER.
01 WS-BASED PIC X(8) BASED.
01 TBL-BOX.
   05 TBL-ROW OCCURS 4 TIMES INDEXED BY ROW-IDX PIC X(4).
PROCEDURE DIVISION.
P-ONE.
    ACCEPT WS-BUF ON EXCEPTION DISPLAY "E" END-ACCEPT.
    ALLOCATE 16 CHARACTERS RETURNING WS-PTR.
    ALLOCATE WS-BASED.
    ALTER P-ONE TO PROCEED TO P-TWO.
    CANCEL "MOD-A".
    EVALUATE WS-CNT ALSO TRUE
        WHEN ANY ALSO WS-CNT = 0
            DISPLAY "ANY"
        WHEN 1 THRU 5 ALSO 1 = 1
            DISPLAY "RNG"
        WHEN 6 ALSO ANY
            DISPLAY "SIX"
        WHEN OTHER
            DISPLAY "OTH"
    END-EVALUATE.
    EXHIBIT NAMED WS-BUF.
    FREE WS-PTR.
    INITIALIZE WS-BUF REPLACING ALPHANUMERIC DATA BY "X".
    INSPECT WS-BUF TALLYING WS-CNT FOR CHARACTERS WS-CNT FOR ALL "A".
    INSPECT WS-BUF REPLACING CHARACTERS BY "B" ALL "B" BY "C".
    INSPECT WS-BUF CONVERTING "ABC" TO "XYZ".
    READ F-IO.
    READ F-IO INTO WS-BUF KEY IS IO-KEY INVALID KEY DISPLAY "IK" END-READ.
    WRITE IO-REC.
    WRITE IO-REC FROM WS-BUF.
    REWRITE IO-REC.
    REWRITE IO-REC FROM WS-BUF.
    SEARCH TBL-ROW VARYING ROW-IDX AT END DISPLAY "END" WHEN TBL-ROW(ROW-IDX) = "ABCD" DISPLAY "HIT" END-SEARCH.
    SEARCH TBL-ROW WHEN TBL-ROW(ROW-IDX) = "WXYZ" CONTINUE END-SEARCH.
    STRING "A" DELIMITED BY SIZE INTO WS-BUF WITH POINTER WS-CNT END-STRING.
    STRING "B" DELIMITED BY SIZE INTO WS-BUF END-STRING.
    UNSTRING WS-BUF DELIMITED BY " " INTO IO-KEY WITH POINTER WS-CNT TALLYING IN WS-CNT END-UNSTRING.
    UNSTRING WS-BUF DELIMITED BY " " INTO IO-KEY END-UNSTRING.
    VALIDATE WS-BUF.
P-TWO.
    STOP RUN.
"#;
    let (hir_opt, sink) = lower_cobol(src, Dialect::Gnu)?;
    assert!(!sink.has_errors());
    assert!(hir_opt.is_some());
    Ok(())
}

/// Covers all 28 pure tag-stamped M8–M20 high-level verbs (`CLOSE` through `XML PARSE`).
#[test]
fn pure_tag_high_level_verbs_lowering_coverage() -> Result<()> {
    let src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. HIGH-PURE.
ENVIRONMENT DIVISION.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-DAT ASSIGN TO "d.dat".
    SELECT F-SRT ASSIGN TO "s.dat".
DATA DIVISION.
FILE SECTION.
FD F-DAT REPORT IS R-TEST.
01 DAT-REC PIC X(8).
SD F-SRT.
01 SRT-REC.
   05 SRT-KEY PIC X(4).
WORKING-STORAGE SECTION.
01 WS-DOC PIC X(32).
01 WS-OBJ USAGE OBJECT REFERENCE.
01 WS-TBL.
   05 WS-EL OCCURS 4 TIMES ASCENDING KEY IS WS-EL PIC X(2).
REPORT SECTION.
RD R-TEST PAGE LIMIT IS 10.
01 R-DTL TYPE DETAIL LINE 1.
   05 COL 1 PIC X(4) SOURCE WS-DOC.
PROCEDURE DIVISION.
DECLARATIVES.
REP-DECL SECTION.
    USE BEFORE REPORTING R-DTL.
    SUPPRESS PRINTING.
END DECLARATIVES.
MAIN-BODY SECTION.
P-MAIN.
    OPEN INPUT F-DAT.
    START F-DAT.
    DELETE F-DAT.
    UNLOCK F-DAT.
    CLOSE F-DAT.
    COMMIT.
    ROLLBACK.
    CONTINUE.
    ENTRY "ALT-ENTRY".
    INITIATE R-TEST.
    GENERATE R-DTL.
    TERMINATE R-TEST.
    INVOKE WS-OBJ "PING".
    JSON GENERATE WS-DOC FROM WS-TBL.
    JSON PARSE WS-DOC INTO WS-TBL.
    XML GENERATE WS-DOC FROM WS-TBL.
    XML PARSE WS-DOC PROCESSING PROCEDURE P-XML.
    SORT F-SRT ON ASCENDING KEY SRT-KEY USING F-DAT GIVING F-DAT.
    MERGE F-SRT ON ASCENDING KEY SRT-KEY USING F-DAT F-DAT GIVING F-DAT.
    SORT WS-EL.
    RELEASE SRT-REC.
    RETURN F-SRT.
    TRANSFORM WS-DOC CHARACTERS FROM "A" TO "B".
    RAISE.
    RESUME.
    IF 1 = 2 NEXT SENTENCE END-IF.
    EXIT PARAGRAPH.
P-XML.
    STOP RUN.
"#;
    let (hir_opt, sink) = lower_cobol(src, Dialect::Gnu)?;
    assert!(!sink.has_errors());
    assert!(hir_opt.is_some());
    Ok(())
}

/// Covers synthetic `ValueAtom::Identifier`, `InlineInvoke`, and uninterned bare repository calls.
#[test]
fn synthetic_inline_invoke_and_identifier_atom_branches() -> Result<()> {
    let base_src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. SYN-LOWER.
DATA DIVISION.
WORKING-STORAGE SECTION.
78 K-NUM VALUE 1.
78 K-STR VALUE "A".
01 V-NUM PIC 9 VALUE 0.
PROCEDURE DIVISION.
    STOP RUN.
"#;
    let dcfg = DialectConfig::for_dialect(Dialect::Cobol2023);
    let (mut grp, mut interner, mut sink) = parse_only(base_src, Dialect::Cobol2023)?;
    let k_num = interner.intern("K-NUM")?;
    let k_str = interner.intern("K-STR")?;
    let v_num = interner.intern("V-NUM")?;
    let meth_sym = interner.intern("DO-IT")?;

    let inline_inv = Expr::InlineInvoke {
        receiver: Box::new(Expr::Literal {
            atom: ValueAtom::Identifier(k_str),
            span: span(),
        }),
        method: meth_sym,
        args: vec![
            CallArg::Value {
                mode: PassingMode::ByReference,
                expr: Expr::Literal {
                    atom: ValueAtom::Identifier(k_num),
                    span: span(),
                },
            },
            CallArg::Omitted(span()),
        ],
        span: span(),
    };
    let bogus_fn = Expr::FunctionCall {
        name: SymbolId::new(999_999),
        syntax: FunctionCallSyntax::BareRepository,
        args: vec![SubscriptItem::Expr(Expr::Literal {
            atom: ValueAtom::Identifier(v_num),
            span: span(),
        })],
        refmod: None,
        span: span(),
    };
    if let Some(CompilationUnit::Program(prog)) = grp.units.first_mut() {
        prog.procedure.initial_sentences.push(Sentence {
            statements: vec![Stmt::Display {
                operands: vec![inline_inv, bogus_fn],
                upon: None,
                advancing: coblang_syntax::ast::DisplayAdvancing::AdvanceLine,
                screen_ext: None,
                handlers: coblang_syntax::ast::EventHandlers::default(),
                span: span(),
            }],
            span: span(),
        });
    }
    let res = lower_compilation_group(&grp, &dcfg, &mut interner, &mut sink)?;
    assert_eq!(res.0.programs.len(), 1);
    Ok(())
}

/// Covers synthetic empty arithmetic `operands` and `GIVING` statements with `rhs: None`.
#[test]
fn synthetic_empty_arith_and_missing_rhs_branches() -> Result<()> {
    let base_src = "IDENTIFICATION DIVISION.\nPROGRAM-ID. SYN-AR.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 V-NUM PIC 9 VALUE 0.\nPROCEDURE DIVISION.\nSTOP RUN.\n";
    let dcfg = DialectConfig::for_dialect(Dialect::Cobol2023);
    let (mut grp, mut interner, mut sink) = parse_only(base_src, Dialect::Cobol2023)?;
    let v_num = interner.intern("V-NUM")?;
    let v_ref = coblang_syntax::ast::DataRef {
        name: coblang_syntax::ast::QualifiedName {
            base: v_num,
            qualifiers: Vec::new(),
            span: span(),
        },
        subscripts: Vec::new(),
        refmod: None,
        modifier: coblang_syntax::ast::DataRefModifier::Plain,
        span: span(),
    };
    let rt = RoundedTarget {
        target: v_ref.clone(),
        rounding: RoundingSpec::Unrounded,
    };
    if let Some(CompilationUnit::Program(prog)) = grp.units.first_mut() {
        prog.procedure.initial_sentences.push(Sentence {
            statements: vec![
                Stmt::Add {
                    mode: CorrespondingMode::Standard,
                    operands: Vec::new(),
                    to_giving_mid: None,
                    to_targets: vec![rt.clone()],
                    giving_targets: Vec::new(),
                    size_error: SizeErrorHandlers::default(),
                    span: span(),
                },
                Stmt::Add {
                    mode: CorrespondingMode::Corresponding,
                    operands: Vec::new(),
                    to_giving_mid: None,
                    to_targets: vec![rt.clone()],
                    giving_targets: Vec::new(),
                    size_error: SizeErrorHandlers::default(),
                    span: span(),
                },
                Stmt::Subtract {
                    mode: CorrespondingMode::Standard,
                    subtrahends: Vec::new(),
                    from_giving_minuend: None,
                    from_targets: Vec::new(),
                    giving_targets: vec![rt.clone()],
                    size_error: SizeErrorHandlers::default(),
                    span: span(),
                },
                Stmt::Multiply {
                    lhs: Expr::Data(v_ref.clone()),
                    rhs: None,
                    by_targets: Vec::new(),
                    giving_targets: vec![rt.clone()],
                    size_error: SizeErrorHandlers::default(),
                    span: span(),
                },
                Stmt::Divide {
                    orientation: DivideOrientation::Into,
                    lhs: Expr::Data(v_ref),
                    rhs: None,
                    into_targets: Vec::new(),
                    giving_targets: vec![rt],
                    remainder: None,
                    size_error: SizeErrorHandlers::default(),
                    span: span(),
                },
            ],
            span: span(),
        });
    }
    let res = lower_compilation_group(&grp, &dcfg, &mut interner, &mut sink)?;
    assert_eq!(res.0.programs.len(), 1);
    Ok(())
}

/// Covers default `INITIALIZE`, `INITIALIZE WITH FILLER` / `TO VALUE`, and `EXIT` variants (`PlainEndpoint`, `Program`, `Function`).
#[test]
fn initialize_and_exit_variants_lowering_coverage() -> Result<()> {
    let src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. INIT-EXIT-TEST.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SPECIAL-NAMES.
    SYSOUT IS MY-DEV.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 WS-A PIC X(4) VALUE "ABCD".
PROCEDURE DIVISION.
P-MAIN.
    INITIALIZE WS-A.
    INITIALIZE WS-A WITH FILLER.
    INITIALIZE WS-A ALL TO VALUE.
    DISPLAY WS-A UPON SYSERR.
    DISPLAY WS-A UPON MY-DEV.
    DISPLAY WS-A UPON ENVIRONMENT-NAME.
    DISPLAY WS-A UPON ENVIRONMENT-VALUE.
    DISPLAY WS-A UPON ARGUMENT-NUMBER.
    DISPLAY WS-A UPON COMMAND-LINE.
    EXIT.
    EXIT PROGRAM.
END PROGRAM INIT-EXIT-TEST.
IDENTIFICATION DIVISION.
FUNCTION-ID. FN-EXIT-TEST.
DATA DIVISION.
LINKAGE SECTION.
01 RET-VAL PIC 9.
PROCEDURE DIVISION RETURNING RET-VAL.
    MOVE 1 TO RET-VAL.
    EXIT FUNCTION.
END FUNCTION FN-EXIT-TEST.
"#;
    let (hir_opt, sink) = lower_cobol(src, Dialect::Cobol2023)?;
    assert!(!sink.has_errors(), "unexpected: {:?}", sink.diagnostics());
    assert!(hir_opt.is_some());
    Ok(())
}

/// Verifies `DisplaySignMode::Ebcdic`, out-of-line `PERFORM ... UNTIL`, in-place `DIVIDE ... REMAINDER`,
/// all `INITIALIZE REPLACING` categories, and all 8 ISO 2023 `ROUNDED MODE IS` keywords.
#[test]
fn ebcdic_sign_rounded_modes_and_init_categories_lowering_coverage() -> Result<()> {
    let extra_src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. INIT-ROUND-DIV.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 WS-GRP.
   05 F-ALPHA PIC A(2).
   05 F-AN PIC X(2).
   05 F-NUM PIC 9(4).
   05 F-REM PIC 9(4).
PROCEDURE DIVISION.
P-ENTRY.
    PERFORM P-BODY UNTIL F-NUM > 10.
    DIVIDE 2 INTO F-NUM REMAINDER F-REM.
    ADD 1 TO F-NUM ROUNDED MODE IS TRUNCATION.
    ADD 1 TO F-NUM ROUNDED MODE IS NEAREST-EVEN.
    ADD 1 TO F-NUM ROUNDED MODE IS NEAREST-TOWARD-ZERO.
    ADD 1 TO F-NUM ROUNDED MODE IS PROHIBITED.
    ADD 1 TO F-NUM ROUNDED MODE IS TOWARD-GREATER.
    ADD 1 TO F-NUM ROUNDED MODE IS TOWARD-LESSER.
    ADD 1 TO F-NUM ROUNDED MODE IS AWAY-FROM-ZERO.
    ADD 1 TO F-NUM ROUNDED MODE IS NEAREST-AWAY-FROM-ZERO.
    INITIALIZE WS-GRP REPLACING
        ALPHABETIC BY "AA"
        ALPHANUMERIC BY "XX"
        ALPHANUMERIC-EDITED BY "XE"
        NUMERIC BY 1
        NUMERIC-EDITED BY 2
        NATIONAL BY "NN"
        NATIONAL-EDITED BY "NE"
        UTF-8 BY "UU"
        BOOLEAN BY 0
        DATA-POINTER BY 0
        INDEX BY 1
        OBJECT-REFERENCE BY 0
        UNKNOWN-CATEGORY BY 0.
    STOP RUN.
P-BODY.
    ADD 1 TO F-NUM.
"#;
    let mut ebcdic_sign_cfg = DialectConfig::for_dialect(Dialect::Gnu);
    ebcdic_sign_cfg.apply_conf_key_value("sign-ebcdic", "yes", "<test>", 1)?;
    let mut sm = SourceMap::new();
    let mut interner = SymbolInterner::new();
    let mut sink2 = DiagnosticSink::new(WarningPolicy::new(), 100);
    let resolver = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &ebcdic_sign_cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &resolver,
    };
    let unit = preprocess_source_file(
        "extra.cob",
        Path::new("."),
        extra_src,
        &pp_cfg,
        &mut sm,
        &mut sink2,
    )?;
    let grp2 = parse_preprocessed_unit(&unit, &ebcdic_sign_cfg, &mut interner, &mut sink2)?;
    let lowered2 = lower_compilation_group(&grp2, &ebcdic_sign_cfg, &mut interner, &mut sink2)?;
    assert_eq!(lowered2.0.programs.len(), 1);
    let (bad_div, bad_sink) = lower_cobol(
        "IDENTIFICATION DIVISION.\nPROGRAM-ID. BD.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 R PIC 9.\nPROCEDURE DIVISION.\n    DIVIDE 2 INTO UNDEF-Q REMAINDER R.\n",
        Dialect::Gnu,
    )?;
    assert!(bad_div.is_none() && bad_sink.has_errors());
    Ok(())
}

/// Exercises M8 `SPECIAL-NAMES` (`ALPHABET`, `CLASS`, `SWITCH`), `EVALUATE`, `PERFORM VARYING ... AFTER`, `SET`, `ALTER`, and `NEXT SENTENCE`.
#[test]
fn m8_special_names_and_control_flow_desugaring_coverage() -> Result<()> {
    let m8_ok = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M8OK.\n",
        "ENVIRONMENT DIVISION.\n",
        "CONFIGURATION SECTION.\n",
        "OBJECT-COMPUTER. M1 PROGRAM COLLATING SEQUENCE IS CUSTOM-A.\n",
        "SPECIAL-NAMES.\n",
        "    SWITCH-1 IS SW1 ON STATUS IS SW1-ON OFF STATUS IS SW1-OFF\n",
        "    SWITCH-A IS SWA ON STATUS IS SWA-ON\n",
        "    UPSI-0 IS UP0\n",
        "    VENDOR-SW IS VSW ON STATUS IS VSW-ON\n",
        "    ALPHABET NAT-A IS NATIVE\n",
        "    ALPHABET S1-A IS STANDARD-1\n",
        "    ALPHABET S2-A IS STANDARD-2\n",
        "    ALPHABET EB-A IS EBCDIC\n",
        "    ALPHABET CUSTOM-A IS 'Z' THRU 'A', '0' THRU '9', SPACE, ZERO, QUOTE,\n",
        "        LOW-VALUE, HIGH-VALUE, 65, RAWWORD, 'a' ALSO 'A' ALSO 'a'\n",
        "    ALPHABET TRUNC-THRU IS 'A' THRU\n",
        "    ALPHABET TRUNC-ALSO IS 'A' ALSO\n",
        "    CLASS C-FWD IS 'A' THRU 'F', 'X'\n",
        "    CLASS C-REV IS 'F' THRU 'A'\n",
        "    CLASS C-AFWD IS 'Z' THRU 'A' IN CUSTOM-A\n",
        "    CLASS C-AREV IS 'A' THRU 'Z' IN CUSTOM-A.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 I PIC 99 COMP-5.\n",
        "01 J PIC 99 COMP-5.\n",
        "01 S1 PIC X(4) VALUE 'ABCD'.\n",
        "01 S2 PIC X(4) VALUE 'WXYZ'.\n",
        "01 PTR USAGE POINTER.\n",
        "01 B1 PIC X BASED.\n",
        "01 GRP.\n",
        "   05 ARR PIC X OCCURS 3 INDEXED BY IDX.\n",
        "      88 ARR-OK VALUE 'Y'.\n",
        "01 FLAG PIC X VALUE 'N'.\n",
        "   88 IS-YES VALUE 'Y' WHEN SET TO FALSE IS 'N'.\n",
        "PROCEDURE DIVISION.\n",
        "S-MAIN SECTION.\n",
        "P-START.\n",
        "    CONTINUE AFTER 1 SECONDS.\n",
        "    SET SW1 TO ON. SET SW1 TO OFF.\n",
        "    IF SW1-ON DISPLAY 'ON' END-IF. IF SW1-OFF DISPLAY 'OFF' END-IF.\n",
        "    SET IS-YES TO TRUE. SET IS-YES TO FALSE. SET ARR-OK(1) TO TRUE.\n",
        "    SET ADDRESS OF B1 TO PTR. SET ADDRESS OF B1 TO ADDRESS OF S1.\n",
        "    SET PTR TO NULL. SET PTR UP BY 1.\n",
        "    SET IDX TO 1. SET IDX UP BY 1. SET IDX DOWN BY 1.\n",
        "    PERFORM VARYING I FROM 1 BY 1 UNTIL I > 2\n",
        "            AFTER J FROM 1 BY 1 UNTIL J > 2\n",
        "        DISPLAY I J\n",
        "    END-PERFORM.\n",
        "    PERFORM WITH TEST AFTER VARYING I FROM 1 BY 1 UNTIL I >= 2\n",
        "            AFTER J FROM 1 BY 1 UNTIL J >= 2\n",
        "        DISPLAY I J\n",
        "    END-PERFORM.\n",
        "    EVALUATE IS-YES\n",
        "        WHEN TRUE NEXT SENTENCE\n",
        "        WHEN FALSE DISPLAY 'F'\n",
        "    END-EVALUATE.\n",
        "    EVALUATE (I + J) ALSO (S1 & S2)\n",
        "        WHEN 2 THRU 10 ALSO ANY DISPLAY 'M'\n",
        "        WHEN OTHER DISPLAY 'O'\n",
        "    END-EVALUATE.\n",
        "    EVALUATE I = 1\n",
        "        WHEN TRUE DISPLAY 'T'\n",
        "        WHEN FALSE DISPLAY 'F'\n",
        "        WHEN IS-YES DISPLAY 'Y'\n",
        "    END-EVALUATE.\n",
        "    EVALUATE FALSE\n",
        "        WHEN IS-YES DISPLAY 'NY'\n",
        "    END-EVALUATE.\n",
        "    EVALUATE TRUE\n",
        "        WHEN 1 DISPLAY 'NO1'\n",
        "        WHEN 2 THRU 3 DISPLAY 'NO2'\n",
        "    END-EVALUATE.\n",
        "    EVALUATE FLAG\n",
        "        WHEN 'A'\n",
        "        WHEN 'B' DISPLAY 'AB'\n",
        "        WHEN S1 DISPLAY 'S'\n",
        "        WHEN IS-YES DISPLAY 'Y'\n",
        "        WHEN NOT IS-YES DISPLAY 'N'\n",
        "    END-EVALUATE.\n",
        "    SEARCH ARR AT END NEXT SENTENCE WHEN ARR(IDX) = 'Y' CONTINUE END-SEARCH.\n",
        "    ADD 1 TO I ON SIZE ERROR NEXT SENTENCE END-ADD.\n",
        "    CALL 'SUB1' ON EXCEPTION NEXT SENTENCE END-CALL.\n",
        "P-ALT.\n",
        "    GO TO P-END.\n",
        "P-END.\n",
        "    ALTER P-ALT TO PROCEED TO P-END.\n",
        "    STOP RUN.\n",
    );
    let (ok_mod, ok_sink) = lower_cobol(m8_ok, Dialect::Gnu)?;
    assert!(
        ok_mod.is_some() && !ok_sink.has_errors(),
        "unexpected diagnostics: {:?}",
        ok_sink.diagnostics()
    );
    Ok(())
}

/// Exercises identity `PROGRAM COLLATING SEQUENCE` and negative error-recovery paths in `m8_flow.rs`.
#[test]
fn m8_identity_collation_and_error_recovery_coverage() -> Result<()> {
    let id_coll = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. IDCOLL.\n",
        "ENVIRONMENT DIVISION.\n",
        "CONFIGURATION SECTION.\n",
        "OBJECT-COMPUTER. M1 PROGRAM COLLATING SEQUENCE IS NAT-A.\n",
        "SPECIAL-NAMES. ALPHABET NAT-A IS NATIVE.\n",
        "PROCEDURE DIVISION.\n",
        "    STOP RUN.\n",
    );
    let (id_mod, _) = lower_cobol(id_coll, Dialect::Gnu)?;
    assert!(
        id_mod
            .and_then(|m| m.0.programs.first().and_then(|p| p.collating_sequence))
            .is_none()
    );

    let m8_bad = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M8BAD.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 X PIC 9.\n",
        "   88 NO-F VALUE 1.\n",
        "01 T PIC X OCCURS 2.\n",
        "   88 T-OK VALUE 'Y'.\n",
        "01 PTR USAGE POINTER.\n",
        "PROCEDURE DIVISION.\n",
        "P1.\n",
        "    SET UNDEF-SW TO ON.\n",
        "    SET X TO TRUE.\n",
        "    SET NO-F TO FALSE.\n",
        "    SET T-OK(ALL) TO TRUE.\n",
        "    SET PTR TO 1.\n",
        "    SET UNDEF-IDX TO 1.\n",
        "    SET UNDEF-IDX UP BY 1.\n",
        "    PERFORM VARYING UNDEF-V FROM 1 BY 1 UNTIL 1 = 1\n",
        "        CONTINUE\n",
        "    END-PERFORM.\n",
        "    ALTER UNDEF-P TO PROCEED TO P1.\n",
    );
    let (bad_mod, bad_s) = lower_cobol(m8_bad, Dialect::Gnu)?;
    assert!(bad_mod.is_none() && bad_s.has_errors());
    Ok(())
}

/// Exercises M9 `EC-BOUND-*` masks, `DefaultColSeqMode::Ebcdic`, `SEARCH` / `SEARCH ALL`, and `SORT table` lowering.
#[test]
fn m9_tables_search_sort_turns_and_colseq_coverage() -> Result<()> {
    use coblang_diag::{BoundCheckToggle, DefaultColSeqMode};

    let mut dcfg = DialectConfig::for_dialect(Dialect::Gnu);
    dcfg.default_colseq = DefaultColSeqMode::Ebcdic;
    dcfg.bounds_checks.subscript = BoundCheckToggle::EnabledFatal;
    dcfg.bounds_checks.odo = BoundCheckToggle::EnabledFatal;
    dcfg.bounds_checks.refmod = BoundCheckToggle::EnabledFatal;

    let m9_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M9-OK.\n",
        "ENVIRONMENT DIVISION.\n",
        "CONFIGURATION SECTION.\n",
        "SPECIAL-NAMES.\n",
        "    ALPHABET MY-ALPHA IS EBCDIC.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 N PIC 9 VALUE 3.\n",
        "01 V-CNT PIC 9 VALUE 1.\n",
        "01 TARGET-A PIC X(2) VALUE 'BB'.\n",
        "01 TARGET-B PIC 9(2) VALUE 10.\n",
        "01 G1.\n",
        "   05 T-IDX OCCURS 1 TO 4 TIMES DEPENDING ON N\n",
        "      ASCENDING KEY IS KA DESCENDING KEY IS KB INDEXED BY IDX1.\n",
        "      10 KA PIC X(2).\n",
        "         88 KA-AA VALUE 'AA'.\n",
        "         88 KA-RNG VALUE 'AA' THRU 'ZZ'.\n",
        "      10 KB PIC 9(2).\n",
        "01 G2.\n",
        "   05 KA PIC X(2) VALUE 'X '.\n",
        "   05 T-PLAIN OCCURS 3 TIMES PIC X(2).\n",
        "01 G-2D.\n",
        "   05 OUTER-ROW OCCURS 2 TIMES.\n",
        "      10 INNER-CELL OCCURS 3 TIMES ASCENDING KEY IS IC-K INDEXED BY IDX2.\n",
        "         15 IC-K PIC X(2).\n",
        "PROCEDURE DIVISION.\n",
        "P-MAIN.\n",
        ">>TURN EC-ALL CHECKING OFF\n",
        ">>TURN EC-BOUND-SUBSCRIPT CHECKING ON\n",
        ">>TURN EC-BOUND-ODO CHECKING ON WITH LOCATION\n",
        ">>TURN EC-BOUND-SUBSCRIPT CHECKING OFF\n",
        ">>TURN EC-BOUND-REF-MOD CHECKING ON\n",
        ">>TURN EC-UNKNOWN-XYZ CHECKING ON\n",
        "$SET BOUND\n",
        "$SET NOBOUND\n",
        "    MOVE T-PLAIN(1)(1:1) TO TARGET-A.\n",
        "    SEARCH T-IDX VARYING IDX1 AT END CONTINUE WHEN KA OF G1(IDX1) = 'AA' CONTINUE END-SEARCH.\n",
        "    SEARCH T-IDX VARYING V-CNT WHEN KA OF G1(IDX1) = 'AA' CONTINUE END-SEARCH.\n",
        "    SEARCH T-IDX WHEN KA OF G1(IDX1) = 'AA' CONTINUE END-SEARCH.\n",
        "    SEARCH T-PLAIN VARYING V-CNT WHEN T-PLAIN(V-CNT) = 'AA' CONTINUE END-SEARCH.\n",
        "    SEARCH T-PLAIN WHEN T-PLAIN(1) = 'AA' CONTINUE END-SEARCH.\n",
        "    SEARCH ALL T-IDX AT END CONTINUE\n",
        "        WHEN KA OF G1(IDX1) = TARGET-A AND TARGET-B = KB(IDX1) CONTINUE END-SEARCH.\n",
        "    SEARCH ALL T-IDX WHEN KA-AA CONTINUE END-SEARCH.\n",
        "    SEARCH ALL T-IDX WHEN KA-RNG AND NOT KA-AA AND 1 = TARGET-B AND KA OF G2 = 'X ' CONTINUE END-SEARCH.\n",
        "    SEARCH ALL INNER-CELL(1) WHEN IC-K(1, IDX2) = 'AA' CONTINUE END-SEARCH.\n",
        "    SORT T-IDX.\n",
        "    SORT T-PLAIN.\n",
        "    SORT T-IDX ON ASCENDING KEY.\n",
        "    SORT T-PLAIN ON DESCENDING KEY.\n",
        "    SORT T-IDX ON ASCENDING KEY KA OF G1 DESCENDING KEY KB SEQUENCE IS MY-ALPHA.\n",
        "    SORT T-PLAIN ON ASCENDING KEY T-PLAIN SEQUENCE IS EBCDIC.\n",
        "    SORT T-PLAIN SEQUENCE IS NATIVE.\n",
        "    STOP RUN.\n",
    );
    let (grp, mut interner, mut sink) = parse_only(m9_src, Dialect::Gnu)?;
    let lowered = lower_compilation_group(&grp, &dcfg, &mut interner, &mut sink)?;
    assert_eq!(lowered.0.programs.len(), 1);
    Ok(())
}

/// Exercises synthetic M9 AST edge cases and negative `SEARCH` / `SORT` table diagnostics.
#[test]
fn m9_synthetic_ast_and_negative_diagnostics_coverage() -> Result<()> {
    use coblang_pp::{TurnCheckingState, TurnDirectiveRecord};
    use coblang_syntax::ast::{DataRef, DataRefModifier, QualifiedName, SearchMode, TruthValue};

    let syn_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M9-SYN.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 G-2D.\n",
        "   05 OUTER-ROW OCCURS 2 TIMES.\n",
        "      10 INNER-CELL OCCURS 3 TIMES ASCENDING KEY IS IC-K INDEXED BY IDX2.\n",
        "         15 IC-K PIC X(2).\n",
        "PROCEDURE DIVISION.\n",
        "P-MAIN.\n",
        "    CONTINUE.\n",
    );
    let (mut grp, mut interner, mut sink) = parse_only(syn_src, Dialect::Gnu)?;
    grp.turn_directives.push(TurnDirectiveRecord {
        exception_names: vec![String::from("EC-BOUND")],
        state: TurnCheckingState::Enabled,
        span: SourceSpan::new(
            FileId::new(99),
            ByteOffset::ZERO,
            ByteLength::new(1),
            ExpansionId::ROOT,
        ),
    });
    let sym_inner = interner.intern("INNER-CELL")?;
    let mk_dref = |base, subscripts| DataRef {
        modifier: DataRefModifier::Plain,
        name: QualifiedName {
            base,
            qualifiers: Vec::new(),
            span: span(),
        },
        subscripts,
        refmod: None,
        span: span(),
    };
    if let Some(CompilationUnit::Program(p)) = grp.units.first_mut()
        && let Some(para) = p.procedure.paragraphs.first_mut()
        && let Some(sent) = para.sentences.first_mut()
    {
        sent.statements.push(Stmt::Search {
            mode: SearchMode::BinaryAll,
            table: mk_dref(
                sym_inner,
                vec![SubscriptItem::Expr(Expr::Literal {
                    atom: ValueAtom::Integer(interner.intern("1")?),
                    span: span(),
                })],
            ),
            varying: None,
            at_end: Vec::new(),
            when_arms: Vec::new(),
            span: span(),
        });
        sent.statements.push(Stmt::SortTable {
            table: mk_dref(sym_inner, vec![SubscriptItem::AllWildcard(span())]),
            keys: Vec::new(),
            duplicates_in_order: TruthValue::False,
            collating_sequence: None,
            span: span(),
        });
    }
    let dcfg = DialectConfig::for_dialect(Dialect::Gnu);
    let lowered = lower_compilation_group(&grp, &dcfg, &mut interner, &mut sink)?;
    assert_eq!(lowered.0.programs.len(), 1);

    let m9_bad = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M9-BAD.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "78 K78 VALUE 1.\n",
        "01 SCALAR-X PIC X(2).\n",
        "01 TBL-OK OCCURS 2 TIMES ASCENDING KEY IS TBL-OK INDEXED BY IDX-OK PIC X(2).\n",
        "PROCEDURE DIVISION.\n",
        "PARA-1.\n",
        "    SEARCH NO-SUCH-TBL WHEN 1 = 1 CONTINUE END-SEARCH.\n",
        "    SEARCH K78 WHEN 1 = 1 CONTINUE END-SEARCH.\n",
        "    SEARCH SCALAR-X WHEN 1 = 1 CONTINUE END-SEARCH.\n",
        "    SEARCH ALL TBL-OK WHEN PARA-1 AND K78 AND K78 = 1 CONTINUE END-SEARCH.\n",
        "    SORT NO-SUCH-TBL.\n",
        "    SORT TBL-OK ON ASCENDING KEY PARA-1.\n",
        "    STOP RUN.\n",
    );
    let (bad_m9, bad_m9_sink) = lower_cobol(m9_bad, Dialect::Gnu)?;
    assert!(bad_m9.is_none() && bad_m9_sink.error_count() >= 5);
    Ok(())
}

/// Exercises M10 `ACCEPT`, `STRING`, `UNSTRING`, `INSPECT`, and `TRANSFORM` valid lowering across all sources, qualifiers, bounds, alphabets, and pointer types.
#[test]
fn m10_strings_accept_inspect_transform_lowering_coverage() -> Result<()> {
    let m10_ok = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M10-OK.\n",
        "ENVIRONMENT DIVISION.\n",
        "CONFIGURATION SECTION.\n",
        "SPECIAL-NAMES.\n",
        "    SYSIN IS MY-CON\n",
        "    ALPHABET ALPHA-EB IS EBCDIC.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 G.\n",
        "   05 S1 PIC X(10) VALUE 'AABBCC'.\n",
        "   05 S2 PIC X(10) OCCURS 2.\n",
        "01 DST PIC X(12).\n",
        "01 DLM PIC X(2).\n",
        "01 P-Z PIC 9(2) VALUE 1.\n",
        "01 P-Z38 PIC 9(38) VALUE 1.\n",
        "01 P-P PIC 9(3) COMP-3 VALUE 1.\n",
        "01 P-BT PIC 9(3) BINARY VALUE 1.\n",
        "01 P-C5 PIC S9(9) COMP-5 VALUE 1.\n",
        "01 P-U128 PIC 9(38) COMP-5 VALUE 1.\n",
        "01 P-IDX USAGE INDEX.\n",
        "PROCEDURE DIVISION.\n",
        "P-MAIN.\n",
        "    ACCEPT OMITTED.\n",
        "    ACCEPT OMITTED FROM CRT.\n",
        "    ACCEPT DST FROM CRT.\n",
        "    ACCEPT S1 OF G FROM MY-CON.\n",
        "    ACCEPT S2(1) FROM SYSIN.\n",
        "    ACCEPT DST(1:4) FROM CUSTOM-DEV.\n",
        "    ACCEPT DST FROM DATE.\n",
        "    ACCEPT DST FROM DATE YYYYMMDD.\n",
        "    ACCEPT DST FROM DAY.\n",
        "    ACCEPT DST FROM DAY YYYYDDD.\n",
        "    ACCEPT DST FROM DAY-OF-WEEK.\n",
        "    ACCEPT DST FROM TIME.\n",
        "    ACCEPT DST FROM MICROSECOND-TIME.\n",
        "    ACCEPT DST FROM ARGUMENT-NUMBER.\n",
        "    ACCEPT DST FROM ARGUMENT-VALUE ON EXCEPTION CONTINUE NOT ON EXCEPTION CONTINUE END-ACCEPT.\n",
        "    ACCEPT DST FROM COMMAND-LINE.\n",
        "    ACCEPT DST FROM ENVIRONMENT-NAME.\n",
        "    ACCEPT DST FROM ENVIRONMENT-VALUE.\n",
        "    STRING S1 DELIMITED BY SIZE S2(1) DELIMITED BY ',' INTO DST WITH POINTER P-Z ON OVERFLOW CONTINUE END-STRING.\n",
        "    STRING 'A' DELIMITED BY SIZE INTO DST(1:4) WITH POINTER P-Z38.\n",
        "    STRING 'A' DELIMITED BY SIZE INTO DST WITH POINTER P-P.\n",
        "    STRING 'A' DELIMITED BY SIZE INTO DST WITH POINTER P-BT.\n",
        "    STRING 'A' DELIMITED BY SIZE INTO DST WITH POINTER P-C5.\n",
        "    STRING 'A' DELIMITED BY SIZE INTO DST WITH POINTER P-U128.\n",
        "    STRING 'A' DELIMITED BY SIZE INTO DST WITH POINTER P-IDX.\n",
        "    UNSTRING S1 DELIMITED BY ',' OR ALL '-' INTO DST DELIMITER IN DLM COUNT IN P-Z S2(1) WITH POINTER P-C5 TALLYING IN P-P ON OVERFLOW CONTINUE END-UNSTRING.\n",
        "    UNSTRING S1(1:4) INTO DST.\n",
        "    INSPECT DST TALLYING P-Z FOR CHARACTERS BEFORE INITIAL 'Z' AFTER INITIAL 'A'\n",
        "        P-Z FOR ALL 'B' P-C5 FOR LEADING 'C' P-C5 FOR FIRST 'D' P-C5 FOR TRAILING 'E'\n",
        "        REPLACING CHARACTERS BY 'X' BEFORE 'Z' ALL 'A' BY 'B' LEADING 'B' BY 'C' FIRST 'C' BY 'D' TRAILING 'D' BY 'E'.\n",
        "    INSPECT DST REPLACING ALL 'A' BY S1 OF G.\n",
        "    INSPECT DST REPLACING ALL 'A' BY S2(1).\n",
        "    INSPECT DST REPLACING ALL 'A' BY DST(1:1).\n",
        "    INSPECT DST CONVERTING ALPHA-EB TO ALPHA-EB AFTER 'A' BEFORE 'Z'.\n",
        "    TRANSFORM DST CHARACTERS FROM ALPHA-EB TO 'abc'.\n",
        "    STOP RUN.\n",
    );
    let (gnu_mod, gnu_sink) = lower_cobol(m10_ok, Dialect::Gnu)?;
    assert!(
        gnu_mod.is_some() && !gnu_sink.has_errors(),
        "{:?}",
        gnu_sink.diagnostics()
    );

    let acu_time = "IDENTIFICATION DIVISION.\nPROGRAM-ID. ACUT.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 T PIC X(12).\nPROCEDURE DIVISION.\n    ACCEPT T FROM TIME.\n    STOP RUN.\n";
    let (acu_mod, acu_sink) = lower_cobol(acu_time, Dialect::Acu)?;
    assert!(acu_mod.is_some() && !acu_sink.has_errors());
    Ok(())
}

/// Exercises M10 negative semantic diagnostics and synthetic AST branches in `m10_strings.rs`.
#[test]
fn m10_strings_negative_diagnostics_and_synthetic_coverage() -> Result<()> {
    use coblang_syntax::ast::{
        AcceptSource, DataRef, DataRefModifier, EventHandlers, QualifiedName,
    };

    let m10_bad = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M10-BAD.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 S PIC X(20).\n",
        "01 C5 PIC S9(4) COMP-5.\n",
        "01 TINY PIC 9 VALUE 1.\n",
        "01 FRAC PIC 9V9 VALUE 0.\n",
        "01 FRAC-PKD PIC 9V9 COMP-3 VALUE 0.\n",
        "01 FRAC-BIN PIC 9V9 COMP-5 VALUE 0.\n",
        "PROCEDURE DIVISION.\n",
        "P-MAIN.\n",
        "    ACCEPT UNDEF-ACC FROM TIME.\n",
        "    ACCEPT ADDRESS OF S FROM TIME.\n",
        "    STRING C5 DELIMITED BY C5 INTO C5 WITH POINTER TINY.\n",
        "    STRING 'A' DELIMITED BY SIZE INTO S WITH POINTER TINY.\n",
        "    STRING 'A' DELIMITED BY SIZE INTO UNDEF-INTO WITH POINTER UNDEF-PTR.\n",
        "    STRING 'A' DELIMITED BY SIZE INTO S WITH POINTER FRAC.\n",
        "    STRING 'A' DELIMITED BY SIZE INTO S WITH POINTER FRAC-PKD.\n",
        "    STRING 'A' DELIMITED BY SIZE INTO S WITH POINTER FRAC-BIN.\n",
        "    STRING 'A' DELIMITED BY SIZE INTO S WITH POINTER S.\n",
        "    UNSTRING C5 DELIMITED BY C5 INTO UNDEF-INTO DELIMITER IN UNDEF-DLM COUNT IN UNDEF-CNT WITH POINTER UNDEF-PTR TALLYING IN UNDEF-TAL.\n",
        "    UNSTRING UNDEF-SRC INTO S COUNT IN FRAC WITH POINTER TINY TALLYING IN S.\n",
        "    INSPECT UNDEF-INSP TALLYING UNDEF-CNT FOR ALL C5.\n",
        "    INSPECT C5 REPLACING ALL ADDRESS OF S BY 'X'.\n",
        "    TRANSFORM UNDEF-TR CHARACTERS FROM 'a' TO 'b'.\n",
        "    TRANSFORM C5 CHARACTERS FROM 'a' TO 'b'.\n",
        "    STOP RUN.\n",
    );
    let (bad_mod, bad_sink) = lower_cobol(m10_bad, Dialect::Gnu)?;
    assert!(bad_mod.is_none() && bad_sink.error_count() >= 8);

    // Synthetic `AcceptSource::ProcessInfo(unknown_sym)` -> `None` branch in `match_accept_source_keyword`.
    let syn_src = "IDENTIFICATION DIVISION.\nPROGRAM-ID. M10-SYN.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 S PIC X(4).\nPROCEDURE DIVISION.\n    STOP RUN.\n";
    let (mut grp, mut interner, mut sink) = parse_only(syn_src, Dialect::Gnu)?;
    let sym_s = interner.intern("S")?;
    let sym_unk = interner.intern("TOTALLY-UNKNOWN-ACCEPT-SRC")?;
    if let Some(CompilationUnit::Program(prog)) = grp.units.first_mut() {
        prog.procedure.initial_sentences.push(Sentence {
            statements: vec![Stmt::Accept {
                target: DataRef {
                    modifier: DataRefModifier::Plain,
                    name: QualifiedName {
                        base: sym_s,
                        qualifiers: Vec::new(),
                        span: span(),
                    },
                    subscripts: Vec::new(),
                    refmod: None,
                    span: span(),
                },
                source: AcceptSource::ProcessInfo(sym_unk),
                screen_ext: None,
                handlers: EventHandlers::default(),
                span: span(),
            }],
            span: span(),
        });
    }
    let dcfg = DialectConfig::for_dialect(Dialect::Gnu);
    let lowered = lower_compilation_group(&grp, &dcfg, &mut interner, &mut sink)?;
    assert_eq!(lowered.0.programs.len(), 1);
    Ok(())
}

/// Covers M11 valid `USING`/`CHAINING`/`RETURNING`, `SET` pointer/address/entry/step/object-ref,
/// `ALLOCATE`, `FREE`, and multi-level nested `IS GLOBAL` storage inheritance.
#[test]
fn m11_pointers_allocate_free_and_nested_global_valid_coverage() -> Result<()> {
    let valid_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. OUTER-G.\n",
        "ENVIRONMENT DIVISION.\n",
        "INPUT-OUTPUT SECTION.\n",
        "FILE-CONTROL.\n",
        "    SELECT F-GLOB ASSIGN TO 'fglob.dat'.\n",
        "DATA DIVISION.\n",
        "FILE SECTION.\n",
        "FD F-GLOB IS GLOBAL.\n",
        "01 F-REC PIC X(4).\n",
        "WORKING-STORAGE SECTION.\n",
        "01 G-SHADOW PIC X(4) IS GLOBAL VALUE 'ABCD'.\n",
        "01 G-EXT PIC X(4) IS EXTERNAL IS GLOBAL.\n",
        "01 G-COMPLEX IS GLOBAL.\n",
        "   05 G-CNT PIC 99 COMP VALUE 2.\n",
        "   05 G-FLAG PIC X VALUE 'Y'.\n",
        "      88 G-YES VALUE 'Y'.\n",
        "   05 G-TBL OCCURS 1 TO 4 DEPENDING ON G-CNT ASCENDING KEY IS G-KEY INDEXED BY G-IDX.\n",
        "      10 G-KEY PIC X(2).\n",
        "01 G-RED REDEFINES G-COMPLEX PIC X(11) IS GLOBAL.\n",
        "01 LOCAL-ONLY PIC X(2) VALUE 'LO'.\n",
        "01 PTR-A USAGE POINTER.\n",
        "01 PTR-B USAGE POINTER.\n",
        "01 PROG-P USAGE PROGRAM-POINTER.\n",
        "01 OBJ-A USAGE OBJECT REFERENCE.\n",
        "01 B-REC BASED.\n",
        "   05 B-FLD PIC X(4).\n",
        "77 B-77 PIC X(4) BASED.\n",
        "LINKAGE SECTION.\n",
        "01 L-REF PIC X(4).\n",
        "01 L-CNT PIC X(4).\n",
        "01 L-VAL PIC S9(4) COMP-5.\n",
        "77 L-77 PIC X(4).\n",
        "01 L-CHAIN PIC X(4).\n",
        "01 L-RET PIC S9(4) COMP-5.\n",
        "PROCEDURE DIVISION USING BY REFERENCE L-REF BY CONTENT OPTIONAL L-CNT BY VALUE L-VAL CHAINING L-CHAIN RETURNING L-RET.\n",
        "P-ONE.\n",
        "    SET PTR-A TO NULL.\n",
        "    SET PTR-B TO PTR-A.\n",
        "    SET PTR-A TO ADDRESS OF LOCAL-ONLY.\n",
        "    SET ADDRESS OF B-REC TO PTR-A.\n",
        "    SET ADDRESS OF L-77 TO PTR-B.\n",
        "    SET PROG-P TO ENTRY 'SUB1'.\n",
        "    SET PTR-A UP BY 1.\n",
        "    SET PTR-A DOWN BY 1.\n",
        "    SET ADDRESS OF B-REC UP BY 2.\n",
        "    SET ADDRESS OF B-REC DOWN BY 2.\n",
        "    SET OBJ-A TO OBJ-A.\n",
        "    SET LOCAL-ONLY TO OBJ-A.\n",
        "    ALLOCATE 16 CHARACTERS RETURNING PTR-A.\n",
        "    ALLOCATE 16 CHARACTERS INITIALIZED RETURNING PTR-A.\n",
        "    ALLOCATE 16 CHARACTERS INITIALIZED TO 'Z' RETURNING PTR-A.\n",
        "    ALLOCATE 16 CHARACTERS INITIALIZED TO ZERO RETURNING PTR-A.\n",
        "    ALLOCATE B-REC.\n",
        "    ALLOCATE B-77 INITIALIZED.\n",
        "    FREE PTR-A, B-REC, ADDRESS OF B-77.\n",
        "    STOP RUN.\n",
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. MID-G.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 G-SHADOW PIC X(2) IS GLOBAL VALUE 'XY'.\n",
        "PROCEDURE DIVISION.\n",
        "    IF G-YES DISPLAY G-SHADOW END-IF.\n",
        "    GOBACK.\n",
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. INNER-G.\n",
        "PROCEDURE DIVISION.\n",
        "    IF G-YES DISPLAY G-SHADOW G-EXT G-RED END-IF.\n",
        "    GOBACK.\n",
        "END PROGRAM INNER-G.\n",
        "END PROGRAM MID-G.\n",
        "END PROGRAM OUTER-G.\n",
    );
    let (mod_opt, sink) = lower_cobol(valid_src, Dialect::Gnu)?;
    assert!(
        !sink.has_errors(),
        "unexpected errors: {:?}",
        sink.diagnostics()
    );
    assert!(mod_opt.is_some());
    Ok(())
}

/// Covers all M11 negative diagnostic branches across `SET`, `ALLOCATE`, `FREE`, and `PROCEDURE USING`.
#[test]
fn m11_pointers_allocate_free_diagnostic_branches_coverage() -> Result<()> {
    let bad_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M11-BAD.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 WS-PLAIN PIC X(4).\n",
        "   88 WS-COND VALUE 'ABCD'.\n",
        "01 WS-NUM PIC 9(4) VALUE 10.\n",
        "01 PTR-1 USAGE POINTER.\n",
        "01 OBJ-1 USAGE OBJECT REFERENCE.\n",
        "01 B-REC BASED.\n",
        "   05 B-SUB PIC X(2).\n",
        "PROCEDURE DIVISION USING UNDEF-U CHAINING UNDEF-CH RETURNING P-PARA.\n",
        "P-PARA.\n",
        "    SET ADDRESS OF P-PARA TO NULL.\n",
        "    SET ADDRESS OF WS-COND TO NULL.\n",
        "    SET WS-COND TO 1.\n",
        "    SET P-PARA TO 1.\n",
        "    SET ADDRESS OF UNDEF-ADDR TO NULL.\n",
        "    SET ADDRESS OF WS-PLAIN TO NULL.\n",
        "    SET ADDRESS OF B-SUB TO NULL.\n",
        "    SET WS-PLAIN TO NULL.\n",
        "    SET UNDEF-PTR TO NULL.\n",
        "    SET PTR-1 TO 123.\n",
        "    SET ADDRESS OF WS-PLAIN TO ENTRY 'S1'.\n",
        "    SET PTR-1, WS-PLAIN TO ENTRY 'S1'.\n",
        "    SET ADDRESS OF WS-PLAIN UP BY 1.\n",
        "    SET ADDRESS OF B-REC, OBJ-1 TO NULL.\n",
        "    SET P-PARA TO OBJ-1.\n",
        "    ALLOCATE 8 CHARACTERS.\n",
        "    ALLOCATE 'BAD' CHARACTERS RETURNING PTR-1.\n",
        "    ALLOCATE 8 CHARACTERS RETURNING WS-PLAIN.\n",
        "    ALLOCATE 8 CHARACTERS RETURNING UNDEF-RET.\n",
        "    ALLOCATE 8 CHARACTERS INITIALIZED TO WS-NUM RETURNING PTR-1.\n",
        "    ALLOCATE 123.\n",
        "    ALLOCATE UNDEF-BASED.\n",
        "    ALLOCATE WS-PLAIN.\n",
        "    ALLOCATE B-SUB.\n",
        "    FREE UNDEF-FREE.\n",
        "    FREE ADDRESS OF WS-PLAIN.\n",
        "    FREE WS-PLAIN.\n",
        "    STOP RUN.\n",
    );
    let (bad_mod, bad_sink) = lower_cobol(bad_src, Dialect::Gnu)?;
    assert!(bad_mod.is_none() && bad_sink.error_count() >= 15);
    Ok(())
}
