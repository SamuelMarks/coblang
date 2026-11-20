//! MLIR lowering integration tests for Milestone M12 (`lower_m12.rs`: intrinsics, `ALL` slices,
//! `FUNCTION-ID` UDFs, algebraic/ODO probes, OO/unicode/pointer staging) and Milestone M13
//! (`lower_m13.rs`: `>>TURN`, `RAISE`/`RESUME`, `DECLARATIVES`, recursive frames, and NIST `DB`).

use std::path::Path;
use std::sync::Mutex;

/// Serializes MLIR context creation across integration tests.
static MLIR_LOCK: Mutex<()> = Mutex::new(());

use coblang_common::{Dialect, OptimizationLevel, Result, SourceFormat};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap, SymbolInterner, WarningPolicy};
use coblang_hir::{
    HirDebugCompileMode, HirDebugContents, HirDebugTraceStmt, HirDeclRef, HirStmt,
    ResolvedHirModule,
};
use coblang_mlir::{
    CobolMlirContext, MainEntryMode, lower_cobol_to_llvm_mlir, lower_hir_to_cobol_mlir,
};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, preprocess_source_file,
};
use coblang_sema::lower_compilation_group;
use coblang_syntax::parse_preprocessed_unit;

/// Lowers `hir` to verified COBOL MLIR (`lower_m12` / `lower_m13` target).
fn lower_full(hir: &ResolvedHirModule, sm: &SourceMap, interner: &SymbolInterner) -> Result<()> {
    let ctx = CobolMlirContext::new();
    let cob_mod = lower_hir_to_cobol_mlir(&ctx, hir, sm, interner)?;
    assert!(!cob_mod.as_module().as_operation().to_string().is_empty());
    Ok(())
}

/// Compiles free-format COBOL source through preprocessing, parsing, and semantic lowering into `ResolvedHirModule`.
fn compile_free_to_hir(
    src: &str,
    dialect: Dialect,
) -> Result<(SourceMap, SymbolInterner, ResolvedHirModule)> {
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
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 200);
    let unit = preprocess_source_file(
        "m12_m13_mlir.cob",
        Path::new("."),
        src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let mut interner = SymbolInterner::new();
    let group = parse_preprocessed_unit(&unit, &dcfg, &mut interner, &mut sink)?;
    let hir = lower_compilation_group(&group, &dcfg, &mut interner, &mut sink)?;
    assert_eq!(
        sink.error_count(),
        0,
        "unexpected errors: {:?}",
        sink.diagnostics()
    );
    Ok((sm, interner, hir))
}

/// Exercises every M12 MLIR lowering path (`MOVE` & `DISPLAY` & `COMPUTE` for UDFs, National UDFs,
/// refmods with/without length, `OBJECT-COMPUTER` collating sequence, `ALL` slices, `ODO` length probes,
/// `AlgebraicProbe`, `AddressOf`, `Pointer`, all 5 `ExtIntrinsic`s, and OO `M20` inline invokes).
#[test]
fn m12_intrinsics_all_slices_udf_probes_and_oo_lower_to_llvm_mlir() -> Result<()> {
    let _g = MLIR_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let oo_src = r#"
IDENTIFICATION DIVISION.
CLASS-ID. NUM-CLS.
FACTORY.
PROCEDURE DIVISION.
IDENTIFICATION DIVISION.
METHOD-ID. GET-NUM.
DATA DIVISION.
LINKAGE SECTION.
01 LK-OUT PIC S9(4) COMP-5.
PROCEDURE DIVISION RETURNING LK-OUT.
    MOVE 7 TO LK-OUT.
    GOBACK.
END METHOD GET-NUM.
IDENTIFICATION DIVISION.
METHOD-ID. GET-STR.
DATA DIVISION.
LINKAGE SECTION.
01 LK-S PIC X(4).
PROCEDURE DIVISION RETURNING LK-S.
    MOVE "ABCD" TO LK-S.
    GOBACK.
END METHOD GET-STR.
END FACTORY.
OBJECT.
PROCEDURE DIVISION.
IDENTIFICATION DIVISION.
METHOD-ID. INST-M.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
REPOSITORY.
    CLASS NUM-CLS.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 WS-NAT PIC N(2).
01 WS-U8 PIC U(2).
PROCEDURE DIVISION.
    DISPLAY FUNCTION ABS(NUM-CLS::"GET-NUM"())
            FUNCTION CONCATENATE(WS-NAT, WS-U8, N"AB", NUM-CLS::"GET-STR"()).
    GOBACK.
END METHOD INST-M.
END OBJECT.
END CLASS NUM-CLS.
"#;
    let (sm_oo, int_oo, hir_oo) = compile_free_to_hir(oo_src, Dialect::Cobol2023)?;
    {
        let ctx = CobolMlirContext::new();
        let mut cob_oo = lower_hir_to_cobol_mlir(&ctx, &hir_oo, &sm_oo, &int_oo)?;
        let _ = lower_cobol_to_llvm_mlir(
            &ctx,
            &mut cob_oo,
            OptimizationLevel::O0,
            MainEntryMode::EmitMain,
        )?;
    }

    let src = r#"
IDENTIFICATION DIVISION.
FUNCTION-ID. FACT-UDF.
DATA DIVISION.
LOCAL-STORAGE SECTION.
01 LS-TMP PIC S9(8) COMP-5 VALUE 1.
LINKAGE SECTION.
01 LK-N PIC S9(8) COMP-5.
01 LK-RET PIC S9(8) COMP-5.
PROCEDURE DIVISION USING LK-N RETURNING LK-RET.
P-FACT.
    IF LK-N <= 1
        MOVE 1 TO LK-RET
    ELSE
        COMPUTE LK-RET = LK-N * FUNCTION FACT-UDF(LK-N - 1)
    END-IF.
    GOBACK.
END FUNCTION FACT-UDF.

IDENTIFICATION DIVISION.
FUNCTION-ID. NAT-UDF.
DATA DIVISION.
LINKAGE SECTION.
01 LK-IN PIC X(2).
01 LK-NAT PIC N(4).
PROCEDURE DIVISION USING LK-IN RETURNING LK-NAT.
    MOVE FUNCTION NATIONAL-OF(LK-IN) TO LK-NAT.
    GOBACK.
END FUNCTION NAT-UDF.

IDENTIFICATION DIVISION.
PROGRAM-ID. M12-MAIN.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
OBJECT-COMPUTER. TEST-CPU PROGRAM COLLATING SEQUENCE IS MY-EBCDIC.
SPECIAL-NAMES.
    ALPHABET MY-EBCDIC IS EBCDIC.
REPOSITORY.
    FUNCTION FACT-UDF
    FUNCTION NAT-UDF
    FUNCTION EXT-UDF AS "ext_udf"
    FUNCTION ALL INTRINSIC.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 WS-TAB-GRP.
   05 WS-TAB OCCURS 3 TIMES PIC S9(4) COMP-5.
   05 WS-STR-TAB OCCURS 2 TIMES PIC X(6).
01 WS-CNT PIC 99 VALUE 2.
01 WS-ODO-GRP.
   05 WS-ODO-ITEM OCCURS 1 TO 4 TIMES DEPENDING ON WS-CNT PIC X(3).
01 WS-OUT PIC X(16).
01 WS-NAT PIC N(4).
01 WS-UTF8 PIC U(4).
01 WS-PTR USAGE POINTER.
01 WS-NUM PIC S9(6)V99.
01 WS-PKD PIC S9(5) COMP-3.
01 WS-FLT USAGE COMP-2.
PROCEDURE DIVISION.
    MOVE EXT-UDF("hello") TO WS-OUT.
    MOVE EXT-UDF("hello")(2:3) TO WS-OUT.
    MOVE EXT-UDF(OMITTED) TO WS-OUT.
    MOVE NAT-UDF("AB")(1:1) TO WS-NAT.
    MOVE UPPER-CASE("abcdef")(2:) TO WS-OUT.
    MOVE NATIONAL-OF("AB") TO WS-NAT.
    MOVE ABS(WS-NUM) TO WS-NUM.
    COMPUTE WS-NUM = FACT-UDF(5) + FACT-UDF(WS-TAB(1)) + SUM(WS-TAB(ALL))
                     + ORD-MAX(WS-ODO-ITEM(ALL)) + FUNCTION BYTE-LENGTH(WS-ODO-GRP).
    DISPLAY EXT-UDF("hi") EXT-UDF("hi")(1:1) NAT-UDF("AB")(1:1) NATIONAL-OF("AB").
    DISPLAY MAX(WS-STR-TAB(ALL)(1:3)) CONTENT-OF(WS-PTR, 4) CONTENT-OF(ADDRESS OF WS-OUT, 2)
            FUNCTION CONTENT-LENGTH(WS-PTR)
            FUNCTION NUMERIC-DECIMAL-POINT FUNCTION NUMERIC-THOUSANDS-SEPARATOR
            FUNCTION MONETARY-DECIMAL-POINT FUNCTION MONETARY-THOUSANDS-SEPARATOR.
    DISPLAY RANDOM(OMITTED) YEAR-TO-YYYY(26, OMITTED, 2020) TRIM(" hi " LEADING)
            HIGHEST-ALGEBRAIC(WS-PKD) HIGHEST-ALGEBRAIC(WS-FLT)
            FUNCTION BYTE-LENGTH(WS-ODO-GRP PHYSICAL)
            ABS(-WS-NUM) ABS(SIN(0.5))
            CONCATENATE(NATIONAL-OF("A"), "b").
    CALL "ext_sub" USING BY CONTENT CONCATENATE("ab" "cd").
    STOP RUN.
END PROGRAM M12-MAIN.
"#;
    let (sm, interner, mut hir) = compile_free_to_hir(src, Dialect::Gnu)?;
    lower_full(&hir, &sm, &interner)?;
    // Exercise non-ZonedDecimal `emit_format_top_decimal` arm (`_ => (0, 0, 1)`).
    if let Some(prog) = hir.0.programs.first_mut() {
        let zoned_opt = prog
            .data_items
            .iter()
            .find(|d| matches!(d.ty, coblang_hir::HirType::ZonedDecimal { .. }))
            .map(|d| d.ty.clone());
        let mut set_zoned = false;
        let mut set_index = false;
        for st in &mut prog.entry_statements {
            if let HirStmt::Display { operands, .. } = st {
                for op in operands {
                    if let coblang_hir::HirExpr::M12Call(c) = op
                        && c.return_ty.category() == coblang_hir::DataCategory::Numeric
                    {
                        if !set_zoned && let Some(zt) = zoned_opt.clone() {
                            c.return_ty = zt;
                            set_zoned = true;
                        } else if !set_index {
                            c.return_ty = coblang_hir::HirType::Index;
                            set_index = true;
                        }
                    }
                }
            }
        }
    }
    lower_full(&hir, &sm, &interner)?;
    Ok(())
}

/// Exercises every M13 MLIR lowering path (`RECURSIVE` program with `PERFORM` stack save/restore,
/// `>>TURN` `ON` / `ON WITH LOCATION` / `EC-I-O file`, explicit & bare `RAISE`, `RESUME` `NEXT STATEMENT`
/// & `procedure`, nested `GLOBAL` `DECLARATIVES`, 3D subscripted NIST `DB` traces, and synthetic `DebugTrace` guards).
#[test]
fn m13_turn_raise_resume_declaratives_recursive_and_debug_lower_to_llvm_mlir() -> Result<()> {
    let _g = MLIR_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let src = r#"
IDENTIFICATION DIVISION.
PROGRAM-ID. REC-WITH-PFM IS RECURSIVE.
DATA DIVISION.
LOCAL-STORAGE SECTION.
01 LS-COUNTER PIC 9(4) VALUE 1.
PROCEDURE DIVISION.
    PERFORM P-STEP.
    GOBACK.
P-STEP.
    ADD 1 TO LS-COUNTER.
END PROGRAM REC-WITH-PFM.

IDENTIFICATION DIVISION.
PROGRAM-ID. M13-MAIN.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SOURCE-COMPUTER. TEST-BOX WITH DEBUGGING MODE.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-1 ASSIGN TO "f1.dat" FILE STATUS IS WS-ST.
DATA DIVISION.
FILE SECTION.
FD F-1.
01 F1-REC PIC X(8).
WORKING-STORAGE SECTION.
01 WS-ST PIC XX.
01 WS-IDX PIC 99 VALUE 4.
01 WS-WRAP.
   05 WS-L1 OCCURS 2 TIMES.
      10 WS-L2 OCCURS 2 TIMES.
         15 WS-L3 OCCURS 2 TIMES PIC X(2).
>>TURN EC-ALL CHECKING OFF
>>TURN EC-I-O F-1 CHECKING ON
>>TURN EC-SIZE CHECKING ON
>>TURN EC-ALL CHECKING ON WITH LOCATION
PROCEDURE DIVISION.
DECLARATIVES.
EC-USER-SEC SECTION.
    USE GLOBAL AFTER STANDARD EXCEPTION PROCEDURE ON EC-USER-EXCEPTION EC-SIZE-ZERO-DIVIDE.
EC-USER-PARA.
    RESUME AT NEXT STATEMENT.
EC-ALL-SEC SECTION.
    USE AFTER STANDARD EXCEPTION PROCEDURE ON EC-ALL.
EC-ALL-PARA.
    RESUME AT REC-PARA.
DBG-SEC SECTION.
    USE FOR DEBUGGING ON MAIN-PARA F-1 ALL REFERENCES OF WS-L3.
DBG-PARA.
    DISPLAY DEBUG-ITEM.
END DECLARATIVES.
MAIN-SEC SECTION.
    RAISE EXCEPTION EC-USER-EXCEPTION.
MAIN-PARA.
    OPEN OUTPUT F-1.
    CLOSE F-1.
    SET LAST EXCEPTION TO OFF.
    MOVE "OK" TO WS-L3(1, 1, 1).
    RAISE EXCEPTION EC-USER-EXCEPTION.
    RAISE.
    GO TO REC-PARA.
REC-PARA.
    STOP RUN.

IDENTIFICATION DIVISION.
PROGRAM-ID. CHILD-PROG.
PROCEDURE DIVISION.
    RAISE EXCEPTION EC-USER-EXCEPTION.
    STOP RUN.
END PROGRAM CHILD-PROG.
END PROGRAM M13-MAIN.
"#;
    let (sm, interner, mut hir) = compile_free_to_hir(src, Dialect::Gnu)?;

    // Inject synthetic `DebugTrace` variants into `REC-WITH-PFM` (no `m13_unit` debug mode) and `M13-MAIN`
    // (empty `line_text` + `HirDebugContents::Spaces`) to exercise every guard in `lower_debug_trace_stmt`.
    let sp = hir.0.programs[0].span;
    let dummy_sec = HirDeclRef {
        owner_unit: hir.0.programs[1].id,
        proc_start: coblang_common::ProcedureId::new(0),
        proc_end: coblang_common::ProcedureId::new(0),
    };
    hir.0.programs[0]
        .entry_statements
        .push(HirStmt::DebugTrace(Box::new(HirDebugTraceStmt {
            line_text: String::new(),
            name_text: String::from("NO-DBG"),
            subscripts: Vec::new(),
            contents: HirDebugContents::Spaces,
            section: dummy_sec,
            span: sp,
        })));
    hir.0.programs[0]
        .entry_statements
        .push(HirStmt::Resume(coblang_hir::HirResumeStmt {
            target: coblang_hir::HirResumeTarget::NextStatement,
            span: sp,
        }));
    hir.0.programs[1]
        .entry_statements
        .push(HirStmt::DebugTrace(Box::new(HirDebugTraceStmt {
            line_text: String::new(),
            name_text: String::from("EMPTY-LINE"),
            subscripts: Vec::new(),
            contents: HirDebugContents::Spaces,
            section: dummy_sec,
            span: sp,
        })));
    hir.0.programs[1]
        .entry_statements
        .push(HirStmt::DebugTrace(Box::new(HirDebugTraceStmt {
            line_text: String::from("42"),
            name_text: String::from("EXPLICIT-LINE"),
            subscripts: Vec::new(),
            contents: HirDebugContents::Spaces,
            section: dummy_sec,
            span: sp,
        })));

    lower_full(&hir, &sm, &interner)?;

    // Exercise `m13_unit == None`, `debug_item == None`, and out-of-bounds `debug_item` guards on `lower_debug_trace_stmt`.
    let saved_units = std::mem::take(&mut hir.0.m13_units);
    lower_full(&hir, &sm, &interner)?;

    hir.0.m13_units = saved_units;
    for u in &mut hir.0.m13_units {
        if u.unit_id == hir.0.programs[1].id {
            u.debugging_mode = HirDebugCompileMode::WithDebuggingMode;
            u.debug_item = Some(coblang_common::DataItemId::new(99_999));
        }
    }
    lower_full(&hir, &sm, &interner)?;

    for u in &mut hir.0.m13_units {
        if u.unit_id == hir.0.programs[1].id {
            u.debug_item = None;
        }
    }
    lower_full(&hir, &sm, &interner)?;
    Ok(())
}
