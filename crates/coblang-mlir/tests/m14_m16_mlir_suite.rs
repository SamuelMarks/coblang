//! End-to-end HIR -> `cobol` MLIR -> verifier -> `llvm` MLIR lowering suite for
//! Milestones M14 (`SEQUENTIAL`, `LINE SEQUENTIAL`, `RELATIVE`), M15 (`INDEXED` ISAM),
//! and M16 (`SORT` / `MERGE`).

use coblang_common::{Dialect, OptimizationLevel, Result, SourceFormat};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap, SymbolInterner, WarningPolicy};
use coblang_hir::ResolvedHirModule;
use coblang_mlir::{
    CobolMlirContext, MainEntryMode, lower_cobol_to_llvm_mlir, lower_hir_to_cobol_mlir,
};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, preprocess_source_file,
};
use coblang_sema::lower_compilation_group;
use coblang_syntax::parse_preprocessed_unit;

/// Compiles COBOL `src` through Sema -> `cobol` MLIR -> Verifier -> `llvm` MLIR and returns textual LLVM-dialect IR.
fn lower_all_to_llvm_ir(src: &str, dialect: Dialect) -> Result<String> {
    let dcfg = DialectConfig::for_dialect(dialect);
    let resolver = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dcfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &resolver,
    };
    let mut sm = SourceMap::new();
    let mut interner = SymbolInterner::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 100);
    let unit = preprocess_source_file(
        "m14_mlir.cob",
        std::path::Path::new("."),
        src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let group = parse_preprocessed_unit(&unit, &dcfg, &mut interner, &mut sink)?;
    let resolved: ResolvedHirModule =
        lower_compilation_group(&group, &dcfg, &mut interner, &mut sink)?;

    let ctx = CobolMlirContext::new();
    let verified = lower_hir_to_cobol_mlir(&ctx, &resolved, &sm, &interner)?;
    let lowered = lower_cobol_to_llvm_mlir(
        &ctx,
        &verified,
        OptimizationLevel::O0,
        MainEntryMode::EmitMain,
    )?;
    Ok(lowered.as_module().as_operation().to_string())
}

/// Verifies full MLIR lowering & verification of `SEQUENTIAL`, `LINE SEQUENTIAL`, `RELATIVE`, `INDEXED`, `LINAGE`, and `DECLARATIVES`.
#[test]
fn lower_m14_m15_all_file_verbs_and_declaratives_to_llvm() -> Result<()> {
    let src = "
IDENTIFICATION DIVISION.
PROGRAM-ID. MLIR-M14.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SPECIAL-NAMES.
    ALPHABET CUSTOM-A IS 'Z' THRU 'A'.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT OPTIONAL F-PRT ASSIGN TO 'prt.txt'
        ORGANIZATION IS SEQUENTIAL
        FILE STATUS IS WS-FS.
    SELECT F-REL ASSIGN TO DYNAMIC WS-PATH
        ORGANIZATION IS RELATIVE
        ACCESS MODE IS DYNAMIC
        RELATIVE KEY IS WS-RK
        FILE STATUS IS WS-FS.
    SELECT F-IDX ASSIGN TO EXTERNAL DD_IDX
        ORGANIZATION IS INDEXED
        ACCESS MODE IS DYNAMIC
        RECORD KEY IS IR-PK
        ALTERNATE RECORD KEY IS IR-AK WITH DUPLICATES SUPPRESS WHEN ALL '*'
        COLLATING SEQUENCE IS CUSTOM-A
        FILE STATUS IS WS-FS.
DATA DIVISION.
FILE SECTION.
FD F-PRT IS GLOBAL
    RECORD IS VARYING IN SIZE FROM 4 TO 20 CHARACTERS DEPENDING ON WS-LEN
    LINAGE IS WS-BODY LINES WITH FOOTING AT 10 LINES AT TOP 1 LINES AT BOTTOM WS-BOT.
01 PRT-REC PIC X(20).
FD F-REL.
01 REL-REC PIC X(8).
FD F-IDX IS EXTERNAL.
01 IDX-REC.
   05 IR-PK PIC X(4).
   05 IR-AK PIC X(4).
WORKING-STORAGE SECTION.
01 WS-FS PIC XX.
01 WS-PATH PIC X(16) VALUE 'rel.dat'.
01 WS-RK PIC 9(4) VALUE 1.
01 WS-LEN PIC 99 VALUE 10.
01 WS-BODY PIC 99 VALUE 12.
01 WS-BOT PIC 99 VALUE 2.
01 WS-DST PIC X(20).
PROCEDURE DIVISION.
DECLARATIVES.
D-F SECTION.
    USE GLOBAL AFTER ERROR PROCEDURE ON F-PRT.
    CONTINUE.
D-IN SECTION.
    USE AFTER ERROR PROCEDURE INPUT.
    CONTINUE.
END DECLARATIVES.
MAIN-PARA.
    OPEN OUTPUT F-PRT I-O F-REL I-O F-IDX.
    WRITE PRT-REC FROM WS-DST AFTER ADVANCING 2 LINES AT END-OF-PAGE CONTINUE NOT AT END-OF-PAGE CONTINUE END-WRITE.
    WRITE PRT-REC BEFORE ADVANCING PAGE.
    WRITE REL-REC INVALID KEY CONTINUE NOT INVALID KEY CONTINUE END-WRITE.
    READ F-REL NEXT INTO WS-DST AT END CONTINUE NOT AT END CONTINUE END-READ.
    READ F-REL PREVIOUS AT END CONTINUE END-READ.
    READ F-REL INVALID KEY CONTINUE END-READ.
    REWRITE REL-REC FROM WS-DST INVALID KEY CONTINUE NOT INVALID KEY CONTINUE END-REWRITE.
    START F-IDX FIRST INVALID KEY CONTINUE END-START.
    START F-IDX LAST INVALID KEY CONTINUE END-START.
    START F-IDX KEY >= IR-PK WITH SIZE 2 INVALID KEY CONTINUE NOT INVALID KEY CONTINUE END-START.
    READ F-IDX KEY IS IR-AK INVALID KEY CONTINUE END-READ.
    DELETE F-IDX INVALID KEY CONTINUE NOT INVALID KEY CONTINUE END-DELETE.
    UNLOCK F-IDX.
    CLOSE F-PRT F-REL WITH LOCK F-IDX.
    DELETE FILE F-REL ON EXCEPTION CONTINUE NOT ON EXCEPTION CONTINUE END-DELETE.
    CALL 'MLIR-SUB'.
    STOP RUN.

IDENTIFICATION DIVISION.
PROGRAM-ID. MLIR-SUB.
PROCEDURE DIVISION.
    OPEN EXTEND F-PRT.
    CLOSE F-PRT.
    GOBACK.
END PROGRAM MLIR-SUB.
END PROGRAM MLIR-M14.
";
    let ir = lower_all_to_llvm_ir(src, Dialect::Default)?;
    assert!(ir.contains("coblang_rt_file_open"));
    assert!(ir.contains("coblang_rt_file_write"));
    assert!(ir.contains("coblang_rt_file_read"));
    assert!(ir.contains("coblang_rt_file_rewrite"));
    assert!(ir.contains("coblang_rt_file_delete"));
    assert!(ir.contains("coblang_rt_file_delete_physical"));
    assert!(ir.contains("coblang_rt_file_start"));
    assert!(ir.contains("coblang_rt_file_unlock"));
    assert!(ir.contains("coblang_rt_file_close"));
    assert!(ir.contains("coblang_rt_decl_enter"));
    Ok(())
}

/// Verifies full MLIR lowering & verification of `SORT` (`USING`/`INPUT PROCEDURE` -> `GIVING`/`OUTPUT PROCEDURE`), `MERGE`, `RELEASE`, `RETURN`, and `SORT-RETURN`.
#[test]
fn lower_m16_sort_and_merge_to_llvm() -> Result<()> {
    let src = "
IDENTIFICATION DIVISION.
PROGRAM-ID. MLIR-M16.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SPECIAL-NAMES.
    ALPHABET SRT-A IS 'Z' THRU 'A'.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-IN1 ASSIGN TO 'in1.dat'.
    SELECT F-IN2 ASSIGN TO 'in2.dat'.
    SELECT F-OUT1 ASSIGN TO 'out1.dat'.
    SELECT F-OUT2 ASSIGN TO 'out2.dat'.
    SELECT SD-S.
I-O-CONTROL.
    SAME RECORD AREA FOR SD-S F-OUT1.
DATA DIVISION.
FILE SECTION.
FD F-IN1 RECORD IS VARYING IN SIZE FROM 4 TO 12 CHARACTERS DEPENDING ON WS-LEN.
01 IN1-R PIC X(12).
FD F-IN2.
01 IN2-R PIC X(12).
FD F-OUT1 RECORD IS VARYING IN SIZE FROM 4 TO 12 CHARACTERS DEPENDING ON WS-LEN.
01 OUT1-R PIC X(12).
FD F-OUT2.
01 OUT2-R PIC X(12).
SD SD-S RECORD IS VARYING IN SIZE FROM 4 TO 12 CHARACTERS DEPENDING ON WS-LEN.
01 SD-R.
   05 SD-K PIC X(4).
   05 SD-B PIC X(8).
WORKING-STORAGE SECTION.
01 WS-LEN PIC 99 VALUE 8.
01 WS-BUF PIC X(12).
PROCEDURE DIVISION.
MAIN-S.
    SORT SD-S ON ASCENDING KEY SD-K
        WITH DUPLICATES IN ORDER
        COLLATING SEQUENCE IS SRT-A
        USING F-IN1 F-IN2
        GIVING F-OUT1 F-OUT2.
    SORT SD-S ON DESCENDING KEY SD-K
        INPUT PROCEDURE IS P-IN
        OUTPUT PROCEDURE IS P-OUT.
    MERGE SD-S ON ASCENDING KEY SD-K
        USING F-IN1 F-IN2
        OUTPUT PROCEDURE IS P-OUT.
    MERGE SD-S ON DESCENDING KEY SD-K
        USING F-IN1 F-IN2
        GIVING F-OUT2.
    IF SORT-RETURN = 0
        CONTINUE
    END-IF.
    STOP RUN.
P-IN.
    RELEASE SD-R FROM WS-BUF.
P-OUT.
    RETURN SD-S RECORD INTO WS-BUF AT END CONTINUE NOT AT END CONTINUE END-RETURN.
";
    let ir = lower_all_to_llvm_ir(src, Dialect::Default)?;
    assert!(ir.contains("coblang_rt_sort_begin"));
    assert!(ir.contains("coblang_rt_sort_begin_merge_stream"));
    assert!(ir.contains("coblang_rt_sort_release"));
    assert!(ir.contains("coblang_rt_sort_finish_input"));
    assert!(ir.contains("coblang_rt_sort_return"));
    assert!(ir.contains("coblang_rt_sort_end"));
    Ok(())
}

/// Covers remaining `OPEN`/`CLOSE`/`READ`/`START`/`ASSIGN` options, dynamic `LINAGE` footing/top, and mode declaratives in `lower_m14`.
#[test]
fn lower_m14_all_assign_open_close_lock_start_variants() -> Result<()> {
    let src = "\
IDENTIFICATION DIVISION.
PROGRAM-ID. MLIR-M14-ALL.
ENVIRONMENT DIVISION.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-ENV ASSIGN TO BARE-ENV-FILE.
    SELECT F-DSP ASSIGN TO DISPLAY.
    SELECT F-KBD ASSIGN TO KEYBOARD.
    SELECT F-IDX ASSIGN TO 'idx.dat'
        ORGANIZATION IS INDEXED
        ACCESS MODE IS DYNAMIC
        RECORD KEY IS IK.
DATA DIVISION.
FILE SECTION.
FD F-ENV
    LINAGE IS 20 LINES WITH FOOTING AT WS-FOOT LINES AT TOP WS-TOP LINES AT BOTTOM 1.
01 ENV-R PIC X(8).
FD F-DSP.
01 DSP-R PIC X(8).
FD F-KBD.
01 KBD-R PIC X(8).
FD F-IDX.
01 IDX-R.
   05 IK PIC X(4).
WORKING-STORAGE SECTION.
01 WS-FOOT PIC 99 VALUE 15.
01 WS-TOP PIC 99 VALUE 2.
PROCEDURE DIVISION.
DECLARATIVES.
D-OUT SECTION.
    USE AFTER ERROR PROCEDURE OUTPUT.
    CONTINUE.
D-IO SECTION.
    USE AFTER ERROR PROCEDURE I-O.
    CONTINUE.
D-EXT SECTION.
    USE AFTER ERROR PROCEDURE EXTEND.
    CONTINUE.
END DECLARATIVES.
P-ALL.
    OPEN INPUT F-KBD REVERSED
         INPUT F-ENV WITH NO REWIND
         OUTPUT F-DSP WITH LOCK
         I-O F-IDX SHARING WITH ALL OTHER.
    CLOSE F-KBD WITH NO REWIND
          F-ENV REEL
          F-DSP UNIT FOR REMOVAL.
    OPEN INPUT F-ENV SHARING WITH READ ONLY.
    CLOSE F-ENV.
    OPEN OUTPUT F-ENV SHARING WITH NO OTHER.
    WRITE ENV-R.
    WRITE ENV-R BEFORE ADVANCING 1 LINES.
    WRITE ENV-R AFTER ADVANCING PAGE.
    READ F-IDX WITH LOCK INVALID KEY CONTINUE END-READ.
    READ F-IDX WITH KEPT LOCK INVALID KEY CONTINUE END-READ.
    READ F-IDX WITH NO LOCK INVALID KEY CONTINUE END-READ.
    READ F-IDX IGNORING LOCK INVALID KEY CONTINUE END-READ.
    START F-IDX KEY = IK INVALID KEY CONTINUE END-START.
    START F-IDX KEY > IK INVALID KEY CONTINUE END-START.
    START F-IDX KEY < IK INVALID KEY CONTINUE END-START.
    START F-IDX KEY <= IK INVALID KEY CONTINUE END-START.
    CLOSE F-ENV F-IDX.
    STOP RUN.
";
    let ir = lower_all_to_llvm_ir(src, Dialect::Default)?;
    assert!(ir.contains("coblang_rt_file_open"));
    assert!(ir.contains("coblang_rt_file_start"));
    Ok(())
}
