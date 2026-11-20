//! Milestone M20 (`XML`/`JSON` & Object-Oriented COBOL) MLIR + LLVM dialect lowering test suite.

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

/// Compiles inline free-format COBOL source into `(SourceMap, SymbolInterner, ResolvedHirModule)`.
fn compile_free_to_hir(
    src: &str,
    dialect: Dialect,
) -> Result<(SourceMap, SymbolInterner, ResolvedHirModule)> {
    let dialect_cfg = DialectConfig::for_dialect(dialect);
    let resolver = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dialect_cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &resolver,
    };
    let mut sm = SourceMap::new();
    let mut interner = SymbolInterner::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 64);
    let unit = preprocess_source_file(
        "m20.cob",
        std::path::Path::new("."),
        src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let parsed = parse_preprocessed_unit(&unit, &dialect_cfg, &mut interner, &mut sink)?;
    let hir = lower_compilation_group(&parsed, &dialect_cfg, &mut interner, &mut sink)?;
    assert_eq!(
        sink.error_count(),
        0,
        "unexpected sema errors: {:?}",
        sink.diagnostics()
    );
    Ok((sm, interner, hir))
}

/// Verifies `XML GENERATE`, `XML PARSE`, `JSON GENERATE`, `JSON PARSE`, and M20 special registers lower to LLVM MLIR.
#[test]
fn m20_xml_and_json_verbs_lower_to_llvm_mlir() -> Result<()> {
    let src = r#"
IDENTIFICATION DIVISION.
PROGRAM-ID. M20-ML.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 N PIC 9 VALUE 2.
01 REC.
   05 FLAGGED PIC X VALUE 'Y'.
   05 ITEMS OCCURS 1 TO 3 DEPENDING ON N PIC X(4).
01 XML-BUF PIC X(128).
01 JSON-BUF PIC X(128).
01 CNT PIC 9(4) COMP-5.
01 NS-URI PIC X(24) VALUE 'http://example.com/ns'.
01 NS-PFX PIC X(4) VALUE 'ex'.
PROCEDURE DIVISION.
MAIN-PARA.
    XML GENERATE XML-BUF FROM REC COUNT IN CNT
        WITH XML-DECLARATION WITH ATTRIBUTES
        NAMESPACE IS NS-URI NAMESPACE-PREFIX IS NS-PFX
        NAME OF FLAGGED IS 'flag'
        TYPE OF FLAGGED IS ATTRIBUTE
        SUPPRESS EVERY NONNUMERIC ELEMENT WHEN SPACES
        ON EXCEPTION DISPLAY XML-CODE
        NOT ON EXCEPTION DISPLAY CNT
    END-XML.
    JSON GENERATE JSON-BUF FROM REC COUNT IN CNT
        NAME OF FLAGGED IS 'active'
        CONVERTING FLAGGED TO JSON BOOLEAN USING 'Y'
        SUPPRESS ITEMS WHEN SPACES
        ON EXCEPTION DISPLAY JSON-CODE
    END-JSON.
    JSON PARSE JSON-BUF INTO REC WITH DETAIL
        NAME OF FLAGGED IS 'active'
        CONVERTING FLAGGED FROM JSON BOOLEAN USING 'Y' AND 'N'
        ON EXCEPTION DISPLAY JSON-STATUS
    END-JSON.
    XML PARSE XML-BUF PROCESSING PROCEDURE HANDLE-XML
        ON EXCEPTION DISPLAY 'ERR'
    END-XML.
    STOP RUN.
HANDLE-XML.
    IF XML-EVENT = 'CONTENT-CHARACTERS'
        DISPLAY XML-TEXT(1:2) FUNCTION LENGTH(XML-TEXT)
    END-IF.
"#;
    let (sm, interner, hir) = compile_free_to_hir(src, Dialect::Cobol2023)?;
    let ctx = CobolMlirContext::new();
    let cobol_mod = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner)?;
    let llvm_mod = lower_cobol_to_llvm_mlir(
        &ctx,
        &cobol_mod,
        OptimizationLevel::O1,
        MainEntryMode::EmitMain,
    )?;
    let ir = llvm_mod.to_mlir_text();
    assert!(ir.contains("coblang_rt_m20_xml_gen"));
    assert!(ir.contains("coblang_rt_m20_json_gen"));
    assert!(ir.contains("coblang_rt_m20_json_parse"));
    assert!(ir.contains("coblang_rt_m20_xml_parse_begin"));
    Ok(())
}

/// Verifies OO COBOL `CLASS-ID`, `INHERITS`, `FACTORY`, `OBJECT`, `INVOKE`, `SELF`, `SUPER`, and inline `::` lower to LLVM MLIR.
#[test]
fn m20_oo_classes_invoke_and_inline_calls_lower_to_llvm_mlir() -> Result<()> {
    let src = r#"
IDENTIFICATION DIVISION.
CLASS-ID. BaseAccount AS "BaseAccount".
OBJECT.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 BAL PIC S9(6) COMP-5 VALUE 100.
PROCEDURE DIVISION.
METHOD-ID. getBal.
DATA DIVISION.
LINKAGE SECTION.
01 R PIC S9(6) COMP-5.
PROCEDURE DIVISION RETURNING R.
    MOVE BAL TO R.
END METHOD getBal.
END OBJECT.
END CLASS BaseAccount.

IDENTIFICATION DIVISION.
CLASS-ID. SubAccount AS "SubAccount" INHERITS FROM BaseAccount.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
REPOSITORY.
    CLASS BaseAccount AS "BaseAccount".
FACTORY.
PROCEDURE DIVISION.
METHOD-ID. makeOne.
DATA DIVISION.
LINKAGE SECTION.
01 OUT-REF USAGE OBJECT REFERENCE SELF.
PROCEDURE DIVISION RETURNING OUT-REF.
    INVOKE SELF "new" RETURNING OUT-REF.
END METHOD makeOne.
END FACTORY.
OBJECT.
PROCEDURE DIVISION.
METHOD-ID. addAndGet.
DATA DIVISION.
LINKAGE SECTION.
01 DELTA PIC S9(6) COMP-5.
01 R PIC S9(6) COMP-5.
PROCEDURE DIVISION USING BY VALUE DELTA RETURNING R.
    INVOKE SUPER "getBal" RETURNING R.
    ADD DELTA TO R.
END METHOD addAndGet.
END OBJECT.
END CLASS SubAccount.

IDENTIFICATION DIVISION.
PROGRAM-ID. OO-DRIVER.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
REPOSITORY.
    CLASS SubAccount AS "SubAccount".
DATA DIVISION.
WORKING-STORAGE SECTION.
01 ACC USAGE OBJECT REFERENCE SubAccount.
01 RES PIC S9(6) COMP-5.
PROCEDURE DIVISION.
    SET ACC TO SubAccount::"makeOne"().
    IF ACC NOT = NULL
        COMPUTE RES = ACC::"addAndGet"(25) + 1
        DISPLAY RES
    END-IF.
    STOP RUN.
END PROGRAM OO-DRIVER.
"#;
    let (sm, interner, hir) = compile_free_to_hir(src, Dialect::Cobol2023)?;
    let ctx = CobolMlirContext::new();
    let cobol_mod = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner)?;
    let llvm_mod = lower_cobol_to_llvm_mlir(
        &ctx,
        &cobol_mod,
        OptimizationLevel::O1,
        MainEntryMode::EmitMain,
    )?;
    let ir = llvm_mod.to_mlir_text();
    assert!(ir.contains("coblang_rt_m20_oo_reg_class"));
    assert!(ir.contains("coblang_rt_m20_oo_invoke"));
    Ok(())
}
