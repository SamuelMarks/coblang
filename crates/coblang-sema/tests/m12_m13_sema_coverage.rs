//! Comprehensive semantic analysis and HIR lowering tests for Milestone M12 (`intrinsics.rs`)
//! and Milestone M13 (`m13_exceptions.rs`) in `coblang-sema`.

use std::path::Path;

use coblang_common::{Dialect, Result, SourceFormat};
use coblang_diag::{
    DiagnosticSink, DialectConfig, RelaxedSyntaxMode, SourceMap, SymbolInterner, WarningPolicy,
};
use coblang_hir::ResolvedHirModule;
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, preprocess_source_file,
};
use coblang_sema::lower_compilation_group;
use coblang_syntax::parse_preprocessed_unit;

/// Runs full preprocessing, parsing, and semantic lowering with configurable `DialectConfig`,
/// returning `(DiagnosticSink, Option<ResolvedHirModule>)`.
fn lower_with_config(
    src: &str,
    cfg: DialectConfig,
) -> Result<(DiagnosticSink, Option<ResolvedHirModule>)> {
    let mem = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut pol = WarningPolicy::new();
    pol.apply_cli_flag("-Wall")?;
    let mut sink = DiagnosticSink::new(pol, 500);
    let unit = preprocess_source_file(
        "m12_m13_cov.cob",
        Path::new("."),
        src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let mut interner = SymbolInterner::new();
    let parsed = parse_preprocessed_unit(&unit, &cfg, &mut interner, &mut sink)?;
    let res = lower_compilation_group(&parsed, &cfg, &mut interner, &mut sink);
    Ok((sink, res.ok()))
}

/// Asserts that `src` compiles cleanly to `ResolvedHirModule` with zero semantic errors.
fn assert_clean_hir(src: &str, dialect: Dialect) -> Result<ResolvedHirModule> {
    let (sink, hir_opt) = lower_with_config(src, DialectConfig::for_dialect(dialect))?;
    assert_eq!(
        sink.error_count(),
        0,
        "unexpected errors: {:?}",
        sink.diagnostics()
    );
    hir_opt.ok_or_else(|| coblang_common::CoblangError::HirLowering {
        construct: "lower_compilation_group",
        location: String::from("test.cob:1:1"),
        message: String::from("expected Ok(ResolvedHirModule)"),
    })
}

/// Covers M12 `FUNCTION-ID` group/currency/alias/recursive signatures, `REPOSITORY` imports,
/// algebraic probes, `ODO` length probes, `ALL` slices, `MAX`/`MIN` categories, `CHAR`/`ORD` alphabets,
/// `FORMATTED-*` validation modes, and reference modification.
#[test]
fn m12_positive_udf_repository_slices_probes_and_datetime_formats() -> Result<()> {
    let src = r#"
IDENTIFICATION DIVISION.
FUNCTION-ID. GRP-UDF AS "ext-grp-udf".
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SPECIAL-NAMES.
    CURRENCY SIGN IS "EUR" WITH PICTURE SYMBOL "E".
DATA DIVISION.
LINKAGE SECTION.
01 LK-IN PIC 9(4).
01 LK-GRP.
   05 LK-A PIC X(4).
   88 LK-COND VALUE "ABCD".
   05 LK-B REDEFINES LK-A PIC 9(4).
   05 LK-ARR OCCURS 2 TIMES PIC E99.99.
01 LK-NEXT PIC X(2).
PROCEDURE DIVISION USING LK-IN RETURNING LK-GRP.
    MOVE "ABCD" TO LK-A.
    GOBACK.
END FUNCTION GRP-UDF.

IDENTIFICATION DIVISION.
FUNCTION-ID. REC-UDF.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
REPOSITORY.
    FUNCTION REC-UDF
    FUNCTION ALL INTRINSIC.
DATA DIVISION.
LINKAGE SECTION.
01 LK-N PIC S9(4) COMP-5.
01 LK-R PIC S9(8) COMP-5.
PROCEDURE DIVISION USING LK-N RETURNING LK-R.
    IF LK-N <= 1
        MOVE 1 TO LK-R
    ELSE
        COMPUTE LK-R = LK-N * FUNCTION REC-UDF(LK-N - 1)
    END-IF.
    GOBACK.
END FUNCTION REC-UDF.

IDENTIFICATION DIVISION.
PROGRAM-ID. M12-POS-MAIN.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SPECIAL-NAMES.
    ALPHABET MY-EBCDIC IS EBCDIC.
REPOSITORY.
    PROGRAM SUB-P1 AS "sub_p1"
    FUNCTION GRP-UDF AS "ext-grp-udf"
    FUNCTION REC-UDF
    FUNCTION EXT-ONLY-UDF AS "ext_only_udf"
    FUNCTION SIN PI E CURRENT-DATE TRIM NUMVAL-C CONVERT FIND-STRING INTRINSIC.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 WS-FMT-CONST CONSTANT AS "YYYY-MM-DD".
01 WS-NUM-CONST CONSTANT AS 42.
01 WS-FMT-VAR PIC X(10) VALUE "YYYY-MM-DD".
01 WS-STR PIC X(16) VALUE "  hello  ".
01 WS-NAT PIC N(6).
01 WS-PTR USAGE POINTER.
01 WS-CNT PIC 99 VALUE 2.
01 WS-ODO-ALPHA.
   05 WS-ODO-PFX PIC X(2).
   05 WS-ODO-EL OCCURS 1 TO 4 TIMES DEPENDING ON WS-CNT PIC X(3).
01 WS-ODO-NAT.
   05 WS-ONAT-EL OCCURS 1 TO 3 TIMES DEPENDING ON WS-CNT PIC N(2).
01 WS-MAT-WRAP.
   05 WS-ROW OCCURS 2 TIMES.
      10 WS-CELL OCCURS 3 TIMES PIC S9(4) COMP-5.
      10 WS-STR-CELL OCCURS 2 TIMES PIC X(4).
      10 WS-NAT-CELL OCCURS 2 TIMES PIC N(2).
01 WS-Z-S PIC S9(4)V99.
01 WS-Z-U PIC 9(4).
01 WS-PKD PIC S9(5)V99 COMP-3.
01 WS-B1 USAGE BINARY-CHAR SIGNED.
01 WS-B2 USAGE BINARY-SHORT UNSIGNED.
01 WS-B4 USAGE BINARY-LONG SIGNED.
01 WS-B8 USAGE BINARY-DOUBLE UNSIGNED.
01 WS-BCMP PIC S9(4) COMP.
01 WS-NED PIC +999.99.
01 WS-FB32 USAGE COMP-1.
01 WS-FB64 USAGE COMP-2.
01 WS-FB128 USAGE FLOAT-EXTENDED.
01 WS-FD64 USAGE FLOAT-DECIMAL-16.
01 WS-FD128 USAGE FLOAT-DECIMAL-34.
PROCEDURE DIVISION.
    DISPLAY GRP-UDF(10) REC-UDF(4) EXT-ONLY-UDF(OMITTED, "abc")(1:3).
    DISPLAY PI E SIN(0.5) CURRENT-DATE(1:8) TRIM(WS-STR LEADING) TRIM(WS-STR TRAILING).
    DISPLAY NUMVAL-C("$12.34" OMITTED ANYCASE) CONVERT("41" HEX ALPHANUMERIC) FIND-STRING("abc" "b" ANYCASE LAST).
    DISPLAY FUNCTION CONTENT-LENGTH(WS-PTR) FUNCTION MONETARY-DECIMAL-POINT
            FUNCTION MONETARY-THOUSANDS-SEPARATOR FUNCTION NUMERIC-DECIMAL-POINT
            FUNCTION NUMERIC-THOUSANDS-SEPARATOR.
    DISPLAY FUNCTION CHAR(66, MY-EBCDIC) FUNCTION ORD("A", MY-EBCDIC).
    DISPLAY FUNCTION HIGHEST-ALGEBRAIC(WS-Z-S) FUNCTION LOWEST-ALGEBRAIC(WS-Z-U)
            FUNCTION SMALLEST-ALGEBRAIC(WS-PKD) FUNCTION HIGHEST-ALGEBRAIC(WS-B1)
            FUNCTION HIGHEST-ALGEBRAIC(WS-B2) FUNCTION HIGHEST-ALGEBRAIC(WS-B4)
            FUNCTION HIGHEST-ALGEBRAIC(WS-B8) FUNCTION HIGHEST-ALGEBRAIC(WS-BCMP)
            FUNCTION LOWEST-ALGEBRAIC(WS-NED) FUNCTION HIGHEST-ALGEBRAIC(WS-FB32)
            FUNCTION LOWEST-ALGEBRAIC(WS-FB64) FUNCTION SMALLEST-ALGEBRAIC(WS-FB128)
            FUNCTION HIGHEST-ALGEBRAIC(WS-FD64) FUNCTION LOWEST-ALGEBRAIC(WS-FD128).
    DISPLAY FUNCTION LENGTH(WS-ODO-ALPHA) FUNCTION BYTE-LENGTH(WS-ODO-ALPHA)
            FUNCTION LENGTH(WS-ODO-NAT) FUNCTION LENGTH("lit") FUNCTION LENGTH(WS-FMT-CONST)
            FUNCTION LENGTH(WS-STR PHYSICAL) FUNCTION BYTE-LENGTH(WS-STR LOCALE).
    DISPLAY FUNCTION SUM(WS-CELL(1, ALL)) FUNCTION MAX(WS-CELL(ALL, ALL))
            FUNCTION ORD-MAX(WS-STR-CELL(1, ALL)(1:2)) FUNCTION MAX(WS-ODO-EL(ALL))
            FUNCTION MAX(WS-NAT-CELL(1, ALL)).
    DISPLAY FUNCTION MAX("a" "b") FUNCTION MIN(N"A" N"B") FUNCTION MAX(1.5 2.5)
            FUNCTION MIN(ZERO 1) FUNCTION MAX(WS-STR "z") FUNCTION MIN(WS-Z-S 0).
    DISPLAY FUNCTION NATIONAL-OF("ABCD")(2:2) FUNCTION WHEN-COMPILED(1:8)
            FUNCTION MODULE-DATE(1:4) FUNCTION MODULE-TIME(1:4)
            FUNCTION UUID4(1:8) FUNCTION EXCEPTION-STATUS(1:5) FUNCTION ABS(WS-Z-S).
    DISPLAY FUNCTION BIT-OF(WS-STR) FUNCTION HEX-OF(WS-STR) FUNCTION STORED-CHAR-LENGTH(WS-STR)
            FUNCTION DISPLAY-OF(WS-NAT) FUNCTION ULENGTH(WS-STR).
    DISPLAY FUNCTION FORMATTED-DATE(WS-FMT-CONST, 1)
            FUNCTION FORMATTED-DATE(WS-FMT-VAR, 1)
            FUNCTION INTEGER-OF-FORMATTED-DATE("YYYY-MM-DDThh:mm:ss+hh:mm", "2026-01-01T00:00:00+00:00")
            FUNCTION FORMATTED-CURRENT-DATE("YYYY-MM-DDThh:mm:ssZ", SYSTEM-OFFSET)
            FUNCTION FORMATTED-TIME("hh:mm:ss+hh:mm", 100, SYSTEM-OFFSET)
            FUNCTION FORMATTED-TIME("hh:mm:ss+hh:mm", 100, 60)
            FUNCTION FORMATTED-TIME("hh:mm:ssZ", 100)
            FUNCTION SECONDS-FROM-FORMATTED-TIME("hh:mm:ss", "01:02:03")
            FUNCTION FORMATTED-DATETIME("YYYY-MM-DDThh:mm:ss+hh:mm", 1, 100, SYSTEM-OFFSET)
            FUNCTION FORMATTED-DATETIME("YYYY-MM-DDThh:mm:ss+hh:mm", 1, 100, 60)
            FUNCTION FORMATTED-DATETIME("YYYY-MM-DDThh:mm:ssZ", 1, 100)
            FUNCTION TEST-FORMATTED-DATETIME("hh:mm:ss", "12:00:00").
    STOP RUN.
END PROGRAM M12-POS-MAIN.
"#;
    let mut cfg = DialectConfig::for_dialect(Dialect::Gnu);
    cfg.relaxed_syntax = RelaxedSyntaxMode::Relaxed;
    let (sink, hir_opt) = lower_with_config(src, cfg)?;
    assert_eq!(
        sink.error_count(),
        0,
        "unexpected errors: {:?}",
        sink.diagnostics()
    );
    assert!(hir_opt.is_some());
    Ok(())
}

/// Covers M12 negative diagnostics across `FUNCTION-ID` headers, `REPOSITORY` collisions,
/// disabled/unimported intrinsics, invalid arity/categories, `ALL` errors, `DECIMAL-POINT IS COMMA`
/// format checks, and invalid reference modifications.
#[test]
fn m12_negative_udf_repository_and_intrinsic_diagnostics() -> Result<()> {
    let udf_errs = r#"
IDENTIFICATION DIVISION.
FUNCTION-ID. F-NO-RET.
DATA DIVISION.
LINKAGE SECTION.
01 LK-X PIC 9.
PROCEDURE DIVISION.
    GOBACK.
END FUNCTION F-NO-RET.

IDENTIFICATION DIVISION.
FUNCTION-ID. F-OMIT-RET.
DATA DIVISION.
LINKAGE SECTION.
01 LK-X PIC 9.
PROCEDURE DIVISION RETURNING OMITTED.
    GOBACK.
END FUNCTION F-OMIT-RET.

IDENTIFICATION DIVISION.
FUNCTION-ID. F-WS-RET.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 WS-RET PIC 9.
LINKAGE SECTION.
01 LK-DUP PIC 9.
PROCEDURE DIVISION USING WS-RET RETURNING WS-RET.
    GOBACK.
END FUNCTION F-WS-RET.

IDENTIFICATION DIVISION.
FUNCTION-ID. F-BAD-LK.
DATA DIVISION.
LINKAGE SECTION.
01 LK-BASE PIC X(4).
01 LK-BAD REDEFINES LK-BASE OCCURS 2 TIMES PIC X ANY LENGTH.
PROCEDURE DIVISION RETURNING LK-BAD.
    GOBACK.
END FUNCTION F-BAD-LK.

IDENTIFICATION DIVISION.
FUNCTION-ID. F-LVL-RET.
DATA DIVISION.
LINKAGE SECTION.
01 LK-G.
   05 LK-SUB PIC 9.
PROCEDURE DIVISION RETURNING LK-SUB.
    GOBACK.
END FUNCTION F-LVL-RET.
"#;
    let (sink1, _) = lower_with_config(udf_errs, DialectConfig::for_dialect(Dialect::Gnu))?;
    assert!(sink1.error_count() >= 6);

    let repo_and_call_errs = r#"
IDENTIFICATION DIVISION.
FUNCTION-ID. GOOD-UDF.
DATA DIVISION.
LINKAGE SECTION.
01 LK-A PIC 9.
01 LK-R PIC X(4).
PROCEDURE DIVISION USING LK-A RETURNING LK-R.
    GOBACK.
END FUNCTION GOOD-UDF.

IDENTIFICATION DIVISION.
PROGRAM-ID. M12-NEG-MAIN.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SPECIAL-NAMES.
    ALPHABET ALPH-COLLIDE IS STANDARD-1
    CLASS CLS-COLLIDE IS "A" THRU "Z"
    SYMBOLIC CHARACTERS SYM-COLLIDE ARE 66
    DECIMAL-POINT IS COMMA.
REPOSITORY.
    PROGRAM P-DUP AS "p_ext_1"
    PROGRAM P-DUP AS "p_ext_1"
    PROGRAM P-DUP AS "p_ext_2"
    PROGRAM P-OTHER AS "p_ext_1"
    FUNCTION ABS
    FUNCTION ALPH-COLLIDE
    FUNCTION CLS-COLLIDE
    FUNCTION SYM-COLLIDE
    FUNCTION UDF-COLLIDE AS "p_ext_1"
    FUNCTION UDF-CONFLICT AS "u_ext_1"
    FUNCTION UDF-CONFLICT AS "u_ext_2"
    FUNCTION MISSING-STRICT-UDF
    FUNCTION GOOD-UDF
    FUNCTION NO-SUCH-INTRINSIC INTRINSIC.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 WS-NUM-CONST CONSTANT AS 99.
01 WS-X PIC X(8).
01 WS-N PIC 9(4).
01 WS-GRP.
   05 WS-ARR OCCURS 3 TIMES PIC X(4).
PROCEDURE DIVISION.
    DISPLAY MISSING-STRICT-UDF(1).
    DISPLAY FUNCTION COMPLETELY-UNKNOWN(1).
    DISPLAY SIN(1).
    DISPLAY GOOD-UDF(1, 2, 3).
    DISPLAY GOOD-UDF(ALL).
    DISPLAY FUNCTION ABS().
    DISPLAY FUNCTION SUBSTITUTE("ab" "a").
    DISPLAY FUNCTION ABS(ALL).
    DISPLAY FUNCTION LENGTH(123).
    DISPLAY FUNCTION LENGTH(WS-X, WS-N).
    DISPLAY FUNCTION UPPER-CASE(WS-ARR(ALL)).
    DISPLAY FUNCTION SUM(WS-ARR(1, ALL)).
    DISPLAY FUNCTION SUM(NO-SUCH-TAB(ALL)).
    DISPLAY FUNCTION SUM(WS-NUM-CONST(ALL)).
    DISPLAY FUNCTION CONTENT-OF(WS-X).
    DISPLAY FUNCTION HIGHEST-ALGEBRAIC(WS-X).
    DISPLAY FUNCTION HIGHEST-ALGEBRAIC(123).
    DISPLAY FUNCTION ABS(WS-X) FUNCTION CHAR(WS-X) FUNCTION FORMATTED-DATE("YYYYMMDD"; WS-X)
            FUNCTION FORMATTED-TIME("hhmmss"; WS-X) FUNCTION FORMATTED-DATETIME("YYYYMMDDThhmmss"; WS-X; 1)
            FUNCTION LOCALE-TIME-FROM-SECONDS(WS-X) FUNCTION BASECONVERT("10"; WS-X; 2).
    DISPLAY FUNCTION FORMATTED-DATE(WS-N; 1) FUNCTION FORMATTED-DATE(WS-NUM-CONST; 1)
            FUNCTION FORMATTED-DATE(UNDEF-FMT; 1)
            FUNCTION FORMATTED-TIME("hh:mm:ss.ss"; 1)
            FUNCTION FORMATTED-DATE("BAD-FORMAT"; 1).
    DISPLAY FUNCTION ABS(1)(1:1) FUNCTION CHAR(66)(1:1) FUNCTION CHAR-NATIONAL(66)(1:1)
            FUNCTION CURRENT-DATE(0:2) FUNCTION CURRENT-DATE(25:2)
            FUNCTION CURRENT-DATE(1:0) FUNCTION CURRENT-DATE(20:5).
    STOP RUN.
END PROGRAM M12-NEG-MAIN.
"#;
    let mut strict_cfg = DialectConfig::for_dialect(Dialect::Gnu);
    strict_cfg.relaxed_syntax = RelaxedSyntaxMode::Strict;
    let (sink2, _) = lower_with_config(repo_and_call_errs, strict_cfg)?;
    assert!(sink2.error_count() >= 25);

    // Disabled intrinsic via DialectConfig (`UUID4` disabled).
    let disabled_src = r#"
IDENTIFICATION DIVISION.
PROGRAM-ID. M12-DIS.
PROCEDURE DIVISION.
    DISPLAY FUNCTION UUID4().
    STOP RUN.
"#;
    let mut dis_cfg = DialectConfig::for_dialect(Dialect::Cobol85);
    dis_cfg.disabled_intrinsics.insert(String::from("UUID4"));
    let (sink3, _) = lower_with_config(disabled_src, dis_cfg)?;
    assert!(sink3.error_count() >= 1);
    Ok(())
}

/// Covers M13 `>>TURN`, `RAISE`/`RESUME`, `GLOBAL` `DECLARATIVES`, and full NIST `DB` instrumentation
/// across file I/O (`OPEN`, `CLOSE`, `READ`, `WRITE`, `REWRITE`, `DELETE`, `START`), `SORT`/`MERGE`,
/// `STRING`/`UNSTRING`/`INSPECT`, `DIVIDE`, `PERFORM UNTIL`, `ALTER`, `GO TO DEPENDING`, and `>76`-byte widening.
#[test]
fn m13_positive_declaratives_and_full_nist_db_instrumentation() -> Result<()> {
    // First test `WITH DEBUGGING MODE` with zero hooks (early return path).
    let no_hooks_src = r#"
IDENTIFICATION DIVISION.
PROGRAM-ID. DB-NO-HOOKS.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SOURCE-COMPUTER. TEST-CPU WITH DEBUGGING MODE.
PROCEDURE DIVISION.
    DISPLAY "OK".
    STOP RUN.
"#;
    let _ = assert_clean_hir(no_hooks_src, Dialect::Gnu)?;

    // Full NIST `DB` + `GLOBAL` `EC-*` declaratives + nested unit + wide `DEBUG-CONTENTS` + all verb hooks.
    let full_db_src = r#"
>>TURN EC-ALL CHECKING OFF
IDENTIFICATION DIVISION.
PROGRAM-ID. DB-FULL.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SOURCE-COMPUTER. TEST-CPU WITH DEBUGGING MODE.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-MON ASSIGN TO "fmon.dat" ORGANIZATION IS INDEXED ACCESS MODE IS DYNAMIC RECORD KEY IS F-KEY FILE STATUS IS WS-ST.
    SELECT F-PLAIN ASSIGN TO "fplain.dat" ORGANIZATION IS INDEXED ACCESS MODE IS DYNAMIC RECORD KEY IS FP-KEY FILE STATUS IS WS-ST.
    SELECT SD-WORK ASSIGN TO "sort.tmp".
DATA DIVISION.
FILE SECTION.
FD F-MON.
01 F-REC.
   05 F-KEY PIC X(4).
   05 F-BODY PIC X(100).
FD F-PLAIN.
01 FP-REC.
   05 FP-KEY PIC X(4).
SD SD-WORK.
01 SD-REC PIC X(20).
WORKING-STORAGE SECTION.
01 WS-ST PIC XX.
01 WS-GRP.
   05 WS-R PIC 9(4) VALUE 1.
   05 WS-W PIC 9(4) VALUE 2.
01 WS-WIDE PIC X(120).
01 WS-STR PIC X(20) VALUE "A,B".
01 WS-DEL PIC X(2).
01 WS-CNT PIC 99.
01 WS-PTRN PIC 999 VALUE 1.
01 WS-TAL PIC 99.
01 WS-TAB-WRAP.
   05 WS-TAB OCCURS 3 TIMES PIC X(4).
01 WS-PTR USAGE POINTER.
>>TURN EC-I-O F-MON CHECKING ON
>>TURN EC-SIZE-ZERO-DIVIDE CHECKING ON WITH LOCATION
PROCEDURE DIVISION.
DECLARATIVES.
EC-GLOB-SEC SECTION.
    USE GLOBAL AFTER STANDARD EXCEPTION PROCEDURE ON EC-USER-EXCEPTION EC-ALL.
EC-GLOB-P.
    RESUME AT NEXT STATEMENT.
EC-LOC-SEC SECTION.
    USE AFTER STANDARD EXCEPTION PROCEDURE ON EC-SIZE-ZERO-DIVIDE.
EC-LOC-P.
    RESUME AT REC-TARGET.
DBG-ALL-SEC SECTION.
    USE FOR DEBUGGING ON ALL PROCEDURES.
DBG-ALL-P.
    DISPLAY DEBUG-ITEM.
DBG-SPEC-SEC SECTION.
    USE FOR DEBUGGING ON F-MON F-REC WS-WIDE WS-W OF WS-GRP ALL REFERENCES OF WS-R OF WS-GRP.
DBG-SPEC-P.
    DISPLAY DEBUG-NAME.
END DECLARATIVES.
MAIN-SEC SECTION.
P-START.
    OPEN I-O F-MON F-PLAIN.
    READ F-MON NEXT AT END CONTINUE NOT AT END CONTINUE END-READ.
    READ F-PLAIN NEXT AT END CONTINUE END-READ.
    WRITE F-REC.
    WRITE F-REC INVALID KEY CONTINUE NOT INVALID KEY CONTINUE END-WRITE.
    WRITE FP-REC.
    REWRITE F-REC.
    REWRITE F-REC INVALID KEY CONTINUE NOT INVALID KEY CONTINUE END-REWRITE.
    REWRITE FP-REC.
    START F-MON KEY IS >= F-KEY.
    START F-MON KEY IS >= F-KEY INVALID KEY CONTINUE NOT INVALID KEY CONTINUE END-START.
    START F-PLAIN KEY IS >= FP-KEY.
    DELETE F-MON RECORD.
    DELETE F-MON RECORD INVALID KEY CONTINUE NOT INVALID KEY CONTINUE END-DELETE.
    DELETE F-PLAIN RECORD.
    UNLOCK F-MON.
    CLOSE F-MON F-PLAIN.
    DELETE FILE F-PLAIN.
    DIVIDE WS-R INTO WS-W GIVING WS-W REMAINDER WS-TAL.
    ACCEPT WS-W FROM DATE.
    IF NOT (WS-R = 1 AND WS-R = 2 OR -WS-R = 0)
        INITIALIZE WS-W
    END-IF.
    INSPECT WS-STR TALLYING WS-W FOR ALL "A".
    INSPECT WS-WIDE REPLACING ALL "A" BY "B".
    INSPECT WS-WIDE CONVERTING "A" TO "B".
    STRING WS-R DELIMITED BY SIZE INTO WS-WIDE WITH POINTER WS-PTRN.
    UNSTRING WS-STR DELIMITED BY "," INTO WS-WIDE DELIMITER IN WS-DEL COUNT IN WS-CNT
        WITH POINTER WS-PTRN TALLYING IN WS-TAL.
    DISPLAY FUNCTION ABS(WS-R) FUNCTION LENGTH(WS-R).
    PERFORM P-SUB.
    PERFORM UNTIL NOT (WS-W = 1 AND (WS-R + 1 = 2 OR -WS-W = 0))
        ADD 1 TO WS-W
    END-PERFORM.
    PERFORM WITH TEST AFTER UNTIL WS-TAB(WS-R) = "X"
        MOVE "X" TO WS-TAB(1)
    END-PERFORM.
    SORT WS-TAB ASCENDING KEY WS-TAB.
    SET WS-PTR TO ADDRESS OF WS-WIDE.
    ALLOCATE 16 CHARACTERS INITIALIZED RETURNING WS-PTR.
    FREE WS-PTR.
    CANCEL "SUB".
    SORT SD-WORK ON ASCENDING KEY SD-REC INPUT PROCEDURE IS SORT-IN OUTPUT PROCEDURE IS SORT-OUT.
    ALTER P-ALT TO PROCEED TO REC-TARGET.
    GO TO P-SUB REC-TARGET DEPENDING ON WS-R.
P-ALT.
    GO TO P-SUB.
P-SUB.
    RAISE EXCEPTION EC-USER-EXCEPTION.
    RAISE.
REC-TARGET.
    STOP RUN.
SORT-IN SECTION.
SI-P.
    RELEASE SD-REC FROM WS-STR.
SORT-OUT SECTION.
SO-P.
    RETURN SD-WORK AT END CONTINUE END-RETURN.

IDENTIFICATION DIVISION.
PROGRAM-ID. NESTED-CHILD.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 WS-NEST-V USAGE OBJECT REFERENCE.
PROCEDURE DIVISION.
    RAISE WS-NEST-V.
    STOP RUN.
END PROGRAM NESTED-CHILD.
END PROGRAM DB-FULL.
"#;
    let _ = assert_clean_hir(full_db_src, Dialect::Gnu)?;
    Ok(())
}

/// Covers M13 negative diagnostics (`>>TURN` bad files/names, duplicate/subscripted/refmodded/`DEBUG-ITEM`
/// debug targets, `RESUME` outside/into `DECLARATIVES` in Strict & Relaxed modes, and cross-declarative `PERFORM`).
#[test]
fn m13_negative_turn_declarative_and_debug_diagnostics() -> Result<()> {
    let neg_src = r#"
>>TURN EC-CUSTOM-UNKNOWN CHECKING ON
>>TURN NOT-AN-EC-NAME CHECKING ON
>>TURN EC-I-O MISSING-FILE CHECKING OFF
IDENTIFICATION DIVISION.
PROGRAM-ID. M13-NEG.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SOURCE-COMPUTER. TEST-CPU WITH DEBUGGING MODE.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-DUP ASSIGN TO "fdup.dat".
DATA DIVISION.
FILE SECTION.
FD F-DUP.
01 F-DUP-REC PIC X(10).
WORKING-STORAGE SECTION.
01 WS-GRP.
   05 WS-ITEM PIC X(10).
   05 WS-ARR OCCURS 3 TIMES PIC X(4).
PROCEDURE DIVISION.
DECLARATIVES.
EC-BAD-SEC SECTION.
    USE AFTER STANDARD EXCEPTION PROCEDURE ON EC-NOT-REAL.
EC-BAD-P.
    PERFORM NORM-P.
    RESUME AT EC-BAD-P.
    RESUME AT NO-SUCH-PROC.
DBG-1 SECTION.
    USE FOR DEBUGGING ON NORM-P NORM-P DBG-1 F-DUP F-DUP WS-ITEM WS-ITEM
        DEBUG-ITEM WS-ITEM(1:2) WS-ARR(1) ALL PROCEDURES.
DBG-1-P.
    CONTINUE.
DBG-2 SECTION.
    USE FOR DEBUGGING ON ALL PROCEDURES.
DBG-2-P.
    CONTINUE.
END DECLARATIVES.
MAIN-SEC SECTION.
NORM-P.
    PERFORM EC-BAD-P.
    RAISE EXCEPTION EC-FAKE-COND.
    RAISE 123.
    RESUME.
    STOP RUN.
END PROGRAM M13-NEG.
"#;
    // Run in Strict mode (RESUME outside DECLARATIVES = error) and COBOL-85 Relaxed mode (obsolete warning + RESUME warning).
    let mut strict_cfg = DialectConfig::for_dialect(Dialect::Gnu);
    strict_cfg.relaxed_syntax = RelaxedSyntaxMode::Strict;
    let (s_strict, _) = lower_with_config(neg_src, strict_cfg)?;
    assert!(s_strict.error_count() >= 12);

    let mut cob85_relaxed = DialectConfig::for_dialect(Dialect::Cobol85);
    cob85_relaxed.relaxed_syntax = RelaxedSyntaxMode::Relaxed;
    let (s_rel, _) = lower_with_config(neg_src, cob85_relaxed)?;
    assert!(s_rel.error_count() >= 10);
    Ok(())
}

/// Covers remaining `intrinsics.rs` and `m13_exceptions.rs` branches including `ExplicitKeyword` UDF calls,
/// duplicate `REPOSITORY` entries, `ALL INTRINSIC` bare non-zero-arg lookups, `FORMATTED-*` offset combinations,
/// `MERGE`/`REPORT`/`SCREEN`/`XML` debug instrumentation spans, and synthetic AST fallbacks.
#[test]
fn m12_m13_remaining_edge_and_synthetic_ast_coverage() -> Result<()> {
    // 1. Positive edge cases: `PROGRAM` without `AS`, duplicate `FUNCTION` in `REPOSITORY`,
    //    `ExplicitKeyword` UDF call without `REPOSITORY`, static group `LENGTH`, `MAX(1 + 2, 3)`,
    //    `NATIONAL-OF("ABCD")(2)` omitted refmod length, `RETURN ... INTO` + `CONTENT-OF(monitored-ptr)`,
    //    `MERGE`, `REPORT`, `SCREEN`, `XML GENERATE` under `WITH DEBUGGING MODE` + `USE FOR DEBUGGING ON ALL PROCEDURES`.
    let pos_edges = r#"
IDENTIFICATION DIVISION.
FUNCTION-ID. DIRECT-UDF.
DATA DIVISION.
LINKAGE SECTION.
01 LK-X PIC 9(4).
01 LK-Y PIC X(8).
PROCEDURE DIVISION USING LK-X RETURNING LK-Y.
    MOVE "RESULT" TO LK-Y.
    GOBACK.
END FUNCTION DIRECT-UDF.

IDENTIFICATION DIVISION.
PROGRAM-ID. EDGE-POS.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SOURCE-COMPUTER. TEST-CPU WITH DEBUGGING MODE.
REPOSITORY.
    PROGRAM BARE-PROG
    FUNCTION DIRECT-UDF
    FUNCTION DIRECT-UDF.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-A ASSIGN TO "fa.dat".
    SELECT F-B ASSIGN TO "fb.dat".
    SELECT F-OUT ASSIGN TO "fout.dat".
    SELECT F-RPT ASSIGN TO "frpt.txt".
    SELECT SD-M ASSIGN TO "sdm.tmp".
DATA DIVISION.
FILE SECTION.
FD F-A.
01 FA-REC PIC X(10).
FD F-B.
01 FB-REC PIC X(10).
FD F-OUT.
01 FOUT-REC PIC X(10).
FD F-RPT REPORT IS RPT-1.
SD SD-M.
01 SDM-REC PIC X(10).
WORKING-STORAGE SECTION.
01 WS-STATIC-GRP.
   05 WS-SG-1 PIC X(4).
   05 WS-SG-2 PIC X(4).
01 WS-PTR USAGE POINTER.
01 WS-INTO PIC X(10).
01 WS-XML PIC X(64).
01 WS-SCR-VAL PIC X(4) VALUE "ABCD".
REPORT SECTION.
RD RPT-1 PAGE LIMIT IS 60 LINES.
01 RPT-LINE TYPE IS DETAIL LINE IS PLUS 1.
   05 COLUMN NUMBER IS 1 PIC X(4) SOURCE IS WS-SCR-VAL.
SCREEN SECTION.
01 SCR-1.
   05 LINE 1 COLUMN 1 VALUE "HI" PIC X(4) USING WS-SCR-VAL.
PROCEDURE DIVISION.
DECLARATIVES.
IO-SEC SECTION.
    USE AFTER STANDARD ERROR PROCEDURE ON INPUT.
IO-P.
    CONTINUE.
RPT-SEC SECTION.
    USE BEFORE REPORTING RPT-LINE.
RPT-P.
    SUPPRESS PRINTING.
DBG-ALL SECTION.
    USE FOR DEBUGGING ON ALL PROCEDURES ALL REFERENCES OF WS-PTR WS-INTO.
DBG-P.
    CONTINUE.
END DECLARATIVES.
MAIN-SEC SECTION.
P1.
    DISPLAY FUNCTION LENGTH(WS-STATIC-GRP) FUNCTION MAX(1 + 2, 3)
            FUNCTION NATIONAL-OF("ABCD")(2:)
            FUNCTION FORMATTED-TIME("hh:mm:ss+hh:mm", 100)
            FUNCTION FORMATTED-DATETIME("YYYY-MM-DDThh:mm:ss+hh:mm", 1, 100)
            FUNCTION CONTENT-OF(WS-PTR, 2).
    INITIATE RPT-1.
    GENERATE RPT-LINE.
    TERMINATE RPT-1.
    DISPLAY SCR-1.
    XML GENERATE WS-XML FROM WS-SCR-VAL.
    MERGE SD-M ON ASCENDING KEY SDM-REC USING F-A F-B OUTPUT PROCEDURE IS M-OUT.
    STOP RUN.
M-OUT SECTION.
MO-P.
    RETURN SD-M INTO WS-INTO AT END CONTINUE END-RETURN.
END PROGRAM EDGE-POS.

IDENTIFICATION DIVISION.
PROGRAM-ID. EXPLICIT-CALLER.
PROCEDURE DIVISION.
    DISPLAY FUNCTION DIRECT-UDF(5).
    STOP RUN.
END PROGRAM EXPLICIT-CALLER.
"#;
    let _ = assert_clean_hir(pos_edges, Dialect::Gnu)?;

    // 2. Negative edge cases: `FUNCTION-ID` with no `PROCEDURE DIVISION` (`span.is_none()`),
    //    `RETURNING` undefined symbol, `ALL INTRINSIC` bare 1-arg intrinsic as data ref,
    //    invalid `FORMATTED-TIME(..., SYSTEM-OFFSET)`, `FORMATTED-DATETIME(..., SYSTEM-OFFSET)`,
    //    `SECONDS-FROM-FORMATTED-TIME`, `TEST-FORMATTED-DATETIME`, `FORMATTED-DATE(123, 1)`.
    let neg_edges = r#"
IDENTIFICATION DIVISION.
FUNCTION-ID. F-UNDEF-RET.
DATA DIVISION.
LINKAGE SECTION.
01 LK-X PIC 9.
PROCEDURE DIVISION RETURNING NO-SUCH-RET.
    GOBACK.
END FUNCTION F-UNDEF-RET.

IDENTIFICATION DIVISION.
PROGRAM-ID. EDGE-NEG.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
REPOSITORY.
    FUNCTION ALL INTRINSIC.
PROCEDURE DIVISION.
    DISPLAY ABS UNDEF-SYM.
    DISPLAY FUNCTION FORMATTED-TIME("YYYY-MM-DD", 100, SYSTEM-OFFSET)
            FUNCTION FORMATTED-DATETIME("YYYY-MM-DD", 1, 100, SYSTEM-OFFSET)
            FUNCTION SECONDS-FROM-FORMATTED-TIME("YYYY-MM-DD", "2026-01-01")
            FUNCTION TEST-FORMATTED-DATETIME("BAD-FMT", "x")
            FUNCTION FORMATTED-DATE(123, 1).
    STOP RUN.
END PROGRAM EDGE-NEG.
"#;
    let (s_neg, _) = lower_with_config(neg_edges, DialectConfig::for_dialect(Dialect::Gnu))?;
    assert!(s_neg.error_count() >= 6);

    // 3. Synthetic AST edge cases (`RETURNING OMITTED`, `OCCURS UNBOUNDED` in group UDF,
    //    uninterned `SymbolId` with `AllWildcard`, `AllWildcard` as 1st arg of `FORMATTED-DATE`,
    //    and empty `TurnDirectiveRecord`).
    let base_src = r#"
IDENTIFICATION DIVISION.
FUNCTION-ID. SYN-F.
DATA DIVISION.
LINKAGE SECTION.
01 LK-GRP.
   05 LK-SUB OCCURS 2 TIMES PIC X(2).
PROCEDURE DIVISION RETURNING LK-GRP.
    GOBACK.
END FUNCTION SYN-F.

IDENTIFICATION DIVISION.
PROGRAM-ID. SYN-P.
PROCEDURE DIVISION.
    DISPLAY FUNCTION FORMATTED-DATE("YYYYMMDD", 1).
    STOP RUN.
END PROGRAM SYN-P.
"#;
    let cfg = DialectConfig::for_dialect(Dialect::Gnu);
    let mem = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 500);
    let unit = preprocess_source_file(
        "syn.cob",
        Path::new("."),
        base_src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let mut interner = SymbolInterner::new();
    let mut grp = parse_preprocessed_unit(&unit, &cfg, &mut interner, &mut sink)?;

    let sp = grp.span;
    let omitted_sym = interner.intern("OMITTED")?;
    // Clone function unit to test both `OCCURS UNBOUNDED` and `RETURNING OMITTED` and `procedure.span = None`.
    if let coblang_syntax::ast::CompilationUnit::Function(f_unb) = &mut grp.units[0] {
        if let Some(occ) = &mut f_unb.data.linkage[1].occurs {
            occ.max = coblang_syntax::ast::OccursUpperBound::Unbounded;
        }
        let mut f_omit = f_unb.clone();
        f_omit.function_id.name = interner.intern("SYN-F-OMIT")?;
        f_omit.procedure.header.returning = Some(omitted_sym);
        let mut f_nospan = f_unb.clone();
        f_nospan.function_id.name = interner.intern("SYN-F-NOSPAN")?;
        f_nospan.procedure.header.returning = None;
        f_nospan.procedure.span = None;
        grp.units
            .push(coblang_syntax::ast::CompilationUnit::Function(f_omit));
        grp.units
            .push(coblang_syntax::ast::CompilationUnit::Function(f_nospan));
    }
    // Inject empty `TurnDirectiveRecord` + `AllWildcard` on `FORMATTED-DATE` and uninterned `SymbolId(99999)`.
    grp.turn_directives.push(coblang_pp::TurnDirectiveRecord {
        exception_names: Vec::new(),
        state: coblang_pp::TurnCheckingState::Enabled,
        span: sp,
    });
    if let coblang_syntax::ast::CompilationUnit::Program(prog) = &mut grp.units[1]
        && let Some(sent) = prog.procedure.initial_sentences.first_mut()
        && let Some(coblang_syntax::ast::Stmt::Display { operands, .. }) =
            sent.statements.first_mut()
    {
        operands.push(coblang_syntax::ast::Expr::FunctionCall {
            name: coblang_common::SymbolId::new(99_999),
            syntax: coblang_syntax::ast::FunctionCallSyntax::ExplicitKeyword,
            args: vec![coblang_syntax::ast::SubscriptItem::AllWildcard(sp)],
            refmod: None,
            span: sp,
        });
        operands.push(coblang_syntax::ast::Expr::FunctionCall {
            name: interner.intern("FORMATTED-DATE")?,
            syntax: coblang_syntax::ast::FunctionCallSyntax::ExplicitKeyword,
            args: vec![
                coblang_syntax::ast::SubscriptItem::AllWildcard(sp),
                coblang_syntax::ast::SubscriptItem::AllWildcard(sp),
            ],
            refmod: None,
            span: sp,
        });
    }
    let _ = lower_compilation_group(&grp, &cfg, &mut interner, &mut sink);
    Ok(())
}
