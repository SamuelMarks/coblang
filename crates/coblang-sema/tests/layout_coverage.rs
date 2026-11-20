//! Clean-room unit, branch, and end-to-end coverage tests for `coblang_sema::layout`.

#![deny(clippy::indexing_slicing)]

use std::path::Path;

use coblang_common::{
    ByteLength, ByteOffset, DataItemId, Dialect, ExpansionId, FileId, LevelNumber, Result,
    ScaleFactor, ScopeId, SourceFormat, SourceSpan, SymbolId,
};
use coblang_diag::{
    DiagnosticSink, DialectConfig, OdoComplexity, RedefinesChainPolicy, RelaxedSyntaxMode,
    SourceMap, SymbolInterner, WarningPolicy,
};
use coblang_hir::{HirConstValue, HirDecimalSign, HirFigurative, ResolvedHirModule};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, preprocess_source_file,
};
use coblang_sema::layout::{SolvedDataLayout, atom_to_hir_const, solve_data_division};
use coblang_sema::{
    PictureContext, ResolvedSymbol, SymbolCandidate, SymbolTable, lower_compilation_group,
};
use coblang_syntax::ast::{
    BlankWhenZeroMode, DataClauseCounts, DataDivision, DataEntry, DataEntryName, ExternalLinkage,
    FdOrSd, FdSdEntry, FigurativeValue, GlobalVisibility, JustificationMode, OccursClause,
    OccursUpperBound, RenamesRange, StorageLinkageFlags, SyncAlignment, UsageKind, ValueAtom,
    ValueClause, ValueRange,
};
use coblang_syntax::{LiteralSign, StringPrefix, parse_preprocessed_unit};

/// Returns a synthetic [`SourceSpan`] for layout unit tests.
fn span() -> SourceSpan {
    SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::new(4),
        ExpansionId::ROOT,
    )
}

/// Builds a baseline [`DataEntry`] with `level` and `name` for synthetic layout tests.
///
/// # Arguments
///
/// * `level` - Validated COBOL level number.
/// * `name` - Named symbol or `FILLER`.
fn blank_entry(level: LevelNumber, name: DataEntryName) -> DataEntry {
    DataEntry {
        level,
        name,
        redefines: None,
        renames: None,
        picture: None,
        usage: None,
        sign: None,
        occurs: None,
        sync: SyncAlignment::Unsynchronized,
        justified: JustificationMode::StandardLeft,
        blank_when_zero: BlankWhenZeroMode::Normal,
        value: None,
        flags: StorageLinkageFlags::default(),
        clause_counts: DataClauseCounts::default(),
        span: span(),
    }
}

/// Compiles clean-room free-format COBOL `src` under `dcfg` and returns `(Option<ResolvedHirModule>, DiagnosticSink)`.
///
/// # Arguments
///
/// * `src` - Clean-room COBOL source text.
/// * `dcfg` - Dialect configuration.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if preprocessing or parsing fails fatally.
fn compile_with_cfg(
    src: &str,
    dcfg: &DialectConfig,
) -> Result<(Option<ResolvedHirModule>, DiagnosticSink)> {
    let mem = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: dcfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut pol = WarningPolicy::new();
    pol.apply_cli_flag("-Wall")?;
    let mut sink = DiagnosticSink::new(pol, 250);
    let unit = preprocess_source_file(
        "layout_test.cob",
        Path::new("."),
        src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let mut interner = SymbolInterner::new();
    let group = parse_preprocessed_unit(&unit, dcfg, &mut interner, &mut sink)?;
    let hir = lower_compilation_group(&group, dcfg, &mut interner, &mut sink).ok();
    Ok((hir, sink))
}

/// Compiles clean-room free-format COBOL `src` under `dialect`.
///
/// # Arguments
///
/// * `src` - Clean-room COBOL source text.
/// * `dialect` - Target COBOL dialect.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if preprocessing or parsing fails fatally.
fn compile_cobol(
    src: &str,
    dialect: Dialect,
) -> Result<(Option<ResolvedHirModule>, DiagnosticSink)> {
    compile_with_cfg(src, &DialectConfig::for_dialect(dialect))
}

/// Solves a synthetic [`DataDivision`] directly via [`solve_data_division`].
///
/// # Arguments
///
/// * `data` - Synthetic AST data division.
/// * `dcfg` - Dialect configuration.
/// * `symtab` - Symbol table to populate.
/// * `scope` - Owning lexical scope.
/// * `interner` - Shared symbol interner.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if special-register interning fails.
fn run_solve(
    data: &DataDivision,
    dcfg: &DialectConfig,
    symtab: &mut SymbolTable,
    scope: ScopeId,
    interner: &mut SymbolInterner,
) -> Result<(SolvedDataLayout, DiagnosticSink)> {
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 250);
    let solved = solve_data_division(
        data,
        scope,
        PictureContext::default(),
        dcfg,
        symtab,
        interner,
        &mut sink,
    )?;
    Ok((solved, sink))
}

/// Tests all Data Division sections, irregular level hierarchies (`01 -> 05 -> 03`), `FILLER`, `GLOBAL`, and `SYNCHRONIZED`.
#[test]
fn sections_irregular_levels_filler_global_and_sync_alignment() -> Result<()> {
    let src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. LAYOUT-SECS.
ENVIRONMENT DIVISION.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-MAIN ASSIGN TO "main.dat".
DATA DIVISION.
FILE SECTION.
FD F-MAIN IS GLOBAL.
01 FD-REC-A.
   05 FD-KEY PIC X(3).
   05 FILLER.
      10 FD-SUB PIC 9(4).
01 FD-REC-B PIC X(12).
WORKING-STORAGE SECTION.
78 C-TEN VALUE 10.
01 C-STR CONSTANT IS GLOBAL AS "HI".
01 GRP-IRREG IS GLOBAL USAGE COMP-5.
   05 CHILD-A PIC 9(2).
   05 SUB-GRP.
      10 DEEP-A PIC 9(4).
   03 SIB-AFTER-05 PIC 9(8).
01 SYNC-REC.
   05 PAD-1 PIC X.
   05 B1 USAGE BINARY-CHAR SIGNED SYNCHRONIZED.
   05 PAD-2 PIC X.
   05 B2 USAGE BINARY-SHORT SIGNED SYNCHRONIZED LEFT.
   05 B2-ALIGNED USAGE BINARY-SHORT UNSIGNED SYNCHRONIZED RIGHT.
   05 PAD-3 PIC X.
   05 B4 USAGE BINARY-LONG SIGNED SYNCHRONIZED.
   05 PAD-4 PIC X.
   05 B8 USAGE BINARY-DOUBLE SIGNED SYNCHRONIZED.
   05 F4 USAGE COMP-1 SYNCHRONIZED.
   05 F8 USAGE COMP-2 SYNCHRONIZED.
   05 F16 USAGE FLOAT-EXTENDED SYNCHRONIZED.
   05 FD16 USAGE FLOAT-DECIMAL-16 SYNCHRONIZED.
   05 FD34 USAGE FLOAT-DECIMAL-34 SYNCHRONIZED.
   05 PTR-1 USAGE POINTER SYNCHRONIZED.
   05 PPTR-1 USAGE PROGRAM-POINTER SYNCHRONIZED.
   05 IDX-ITEM USAGE INDEX SYNCHRONIZED.
   05 OBJ-ITEM USAGE OBJECT REFERENCE SYNCHRONIZED.
   05 PAD-5 PIC X.
   05 NAT-1 PIC N(3) USAGE NATIONAL SYNCHRONIZED.
   05 NATED-1 PIC N/N USAGE NATIONAL SYNCHRONIZED.
   05 ALPHA-SYNC PIC X(3) SYNCHRONIZED.
LOCAL-STORAGE SECTION.
77 LS-SCALAR PIC S9(4) COMP-3 VALUE -12.
LINKAGE SECTION.
01 LK-CNT PIC 9(4) COMP-5.
01 LK-UNB.
   05 LK-ELEM PIC X(2) OCCURS 1 TO UNBOUNDED DEPENDING ON LK-CNT.
PROCEDURE DIVISION USING LK-CNT LK-UNB.
    STOP RUN.
"#;
    let (hir, sink) = compile_cobol(src, Dialect::Cobol2023)?;
    assert!(
        !sink.has_errors(),
        "unexpected errors: {:?}",
        sink.diagnostics()
    );
    assert!(hir.is_some());
    Ok(())
}

/// Tests `77` subordinate errors, orphan `05` in strict vs relaxed dialects, and group `PICTURE` errors.
#[test]
fn hierarchy_orphans_77_subordinates_and_group_picture_errors() -> Result<()> {
    let strict_err_src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. HIER-ERR.
DATA DIVISION.
WORKING-STORAGE SECTION.
05 ORPHAN-TOP PIC X(2).
77 BAD-77-A PIC X.
   88 COND-ON-77 VALUE "A".
05 ILLEGAL-CHILD PIC X.
77 BAD-77-B PIC X.
66 BAD-REN-AFTER-77 RENAMES BAD-77-B.
77 OK-77-BEFORE-78 PIC X.
78 CONST-AFTER-77 VALUE 1.
77 OK-77-BEFORE-01 PIC X.
01 GRP-WITH-PIC PIC X(4).
   88 GRP-COND VALUE "ABCD".
   05 GRP-CHILD PIC X(4).
77 TRAILING-77 PIC X.
PROCEDURE DIVISION.
    STOP RUN.
"#;
    let (_, sink_strict) = compile_cobol(strict_err_src, Dialect::Cobol85)?;
    assert!(sink_strict.error_count() >= 4);

    // Relaxed syntax permits top-level `05` without an enclosing `01`.
    let relaxed_src = r"IDENTIFICATION DIVISION.
PROGRAM-ID. HIER-RELAXED.
DATA DIVISION.
WORKING-STORAGE SECTION.
05 TOP-RELAXED PIC X(4).
PROCEDURE DIVISION.
    STOP RUN.
";
    let mut relaxed_cfg = DialectConfig::for_dialect(Dialect::Gnu);
    relaxed_cfg.relaxed_syntax = RelaxedSyntaxMode::Relaxed;
    let (_, sink_relaxed) = compile_with_cfg(relaxed_src, &relaxed_cfg)?;
    assert!(!sink_relaxed.has_errors());
    Ok(())
}

/// Tests `REDEFINES` valid chains, size warnings/errors, level mismatches, `OCCURS` targets, and `FILE SECTION` `01` rules.
#[test]
fn redefines_rules_across_dialects() -> Result<()> {
    let cob85_err = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. REDEF-85.
ENVIRONMENT DIVISION.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F1 ASSIGN TO "f1.dat".
DATA DIVISION.
FILE SECTION.
FD F1.
01 F1-REC PIC X(10).
01 F1-REDEF REDEFINES F1-REC PIC X(10).
   05 F1-A PIC X(4).
   05 F1-B REDEFINES F1-A PIC X(4).
WORKING-STORAGE SECTION.
01 TOP-BASE PIC X(4).
01 TOP-LARGER REDEFINES TOP-BASE PIC X(8).
01 GRP-REDEF.
   05 TARGET-OCC PIC X(2) OCCURS 3.
   05 BAD-OCC-REDEF REDEFINES TARGET-OCC PIC X(6).
   05 BASE-1 PIC X(4).
   05 REDEF-1 REDEFINES BASE-1 PIC X(2).
   05 REDEF-CHAIN REDEFINES REDEF-1 PIC X(2).
   05 REDEF-LARGER REDEFINES BASE-1 PIC X(6).
   05 REDEF-GRP-SMALL REDEFINES BASE-1.
      10 RGS-1 PIC X(2).
   05 REDEF-GRP-LARGE REDEFINES BASE-1.
      10 RGL-1 PIC X(8).
   05 LVL-5 PIC X(2).
   07 BAD-LVL REDEFINES LVL-5 PIC X(2).
   05 MISSING-REDEF REDEFINES NO-SUCH-ITEM PIC X(2).
PROCEDURE DIVISION.
    STOP RUN.
"#;
    let (_, sink85) = compile_cobol(cob85_err, Dialect::Cobol85)?;
    assert!(sink85.error_count() >= 6);
    assert!(sink85.warning_count() >= 1);

    // In Cobol2023 (or Gnu with IndirectAllowed), chained redefines and larger 05 redefines warn instead of error.
    let chain_ok = r"IDENTIFICATION DIVISION.
PROGRAM-ID. REDEF-2023.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 GRP.
   05 B1 PIC X(4).
   05 B2 REDEFINES B1 PIC X(6).
   05 B3 REDEFINES B2 PIC X(4).
PROCEDURE DIVISION.
    STOP RUN.
";
    let mut cfg = DialectConfig::for_dialect(Dialect::Cobol2023);
    cfg.indirect_redefines = RedefinesChainPolicy::IndirectAllowed;
    let (_, sink2023) = compile_with_cfg(chain_ok, &cfg)?;
    assert!(
        !sink2023.has_errors(),
        "unexpected: {:?}",
        sink2023.diagnostics()
    );
    assert!(sink2023.warning_count() >= 1);
    Ok(())
}

/// Tests `OCCURS` fixed, variable `DEPENDING ON` (ODO), `UNBOUNDED`, variably-located items, nested ODO, and `INDEXED BY`.
#[test]
fn occurs_and_odo_validation_across_dialects() -> Result<()> {
    let iso_odo_err = r"IDENTIFICATION DIVISION.
PROGRAM-ID. ODO-ERR.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 LVL1-OCC PIC X(2) OCCURS 3.
77 LVL77-OCC PIC X(2) OCCURS 2.
01 ODO-BAD-BOUNDS.
   05 CNT-OK PIC 9(2).
   05 CNT-DEC PIC 9V9.
   05 CNT-ALPHA PIC X(2).
   05 CNT-ARR PIC 9(2) OCCURS 2.
   05 Z-MAX PIC X OCCURS 0 TIMES.
   05 INV-MIN-MAX PIC X OCCURS 5 TO 3 TIMES DEPENDING ON CNT-OK.
   05 UNB-WS PIC X OCCURS 1 TO UNBOUNDED DEPENDING ON CNT-OK.
   05 UNB-NO-DEP PIC X OCCURS UNBOUNDED.
   05 ODO-GRP OCCURS 1 TO 4 TIMES DEPENDING ON CNT-DEC INDEXED BY IDX-DUP.
      10 NESTED-ODO PIC X OCCURS 1 TO 2 TIMES DEPENDING ON CNT-ALPHA INDEXED BY IDX-DUP.
   05 VAR-LOCATED-ELEM PIC X(2).
   05 REDEF-AFTER-ODO REDEFINES ODO-GRP PIC X(4).
01 ODO-SUB-TBL.
   05 SUB-DEP PIC X OCCURS 1 TO 3 TIMES DEPENDING ON CNT-ARR.
01 ODO-CHILD-PROP.
   05 INNER-WRAP.
      10 INNER-ODO PIC X OCCURS 1 TO 3 TIMES DEPENDING ON CNT-OK.
   05 VAR-AFTER-GROUP PIC X(2).
PROCEDURE DIVISION.
    STOP RUN.
";
    let (_, sink_iso) = compile_cobol(iso_odo_err, Dialect::Cobol85)?;
    assert!(sink_iso.error_count() >= 10);

    // Gnu with complex ODO allows level-01 OCCURS, nested ODO, variably-located items, and subscripted ODO objects.
    let gnu_odo_ok = r"IDENTIFICATION DIVISION.
PROGRAM-ID. ODO-GNU.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 TOP-TBL PIC X(2) OCCURS 2.
01 COUNTERS.
   05 C-P PIC 9(3) COMP-3.
   05 C-B PIC 9(4) COMP-5 OCCURS 2.
01 COMPLEX-REC.
   05 OUTER-G OCCURS 1 TO 3 TIMES DEPENDING ON C-P.
      10 INNER-G OCCURS 1 TO 2 TIMES DEPENDING ON C-B.
         15 LEAF PIC X.
   05 TRAILING-VAR PIC X(4).
PROCEDURE DIVISION.
    STOP RUN.
";
    let mut gnu_cfg = DialectConfig::for_dialect(Dialect::Gnu);
    gnu_cfg.complex_odo = OdoComplexity::ComplexAllowed;
    let (_, sink_gnu) = compile_with_cfg(gnu_odo_ok, &gnu_cfg)?;
    assert!(
        !sink_gnu.has_errors(),
        "unexpected: {:?}",
        sink_gnu.diagnostics()
    );
    Ok(())
}

/// Tests `66 RENAMES`, `78`/`01 CONSTANT`, `88` condition-names, and `VALUE` clause validation across categories and dialects.
#[test]
fn renames_constants_conditions_and_value_domains() -> Result<()> {
    let valid_src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. VAL-OK.
DATA DIVISION.
WORKING-STORAGE SECTION.
78 K-LOW VALUE 10.
78 K-HIGH VALUE 20.
01 REC-1.
   05 FLD-A PIC X(2) VALUE "AB".
   05 FLD-B PIC 9(2) VALUE ZERO.
   05 FLD-C PIC S9(2) VALUE -5.
   05 FLD-D PIC S9V9 VALUE -1.5.
   05 FLD-E PIC A(3) VALUE "AB ".
   05 FLD-F USAGE COMP-1 VALUE 0.
   05 FLD-G USAGE COMP-1 VALUE 1.5.
   05 FLD-H PIC Z9 VALUE " 1".
   05 GRP-COND-HOST VALUE "XY".
      88 GRP-IS-XY VALUE "XY" WHEN SET TO FALSE IS "NO".
      10 SUB-X PIC X.
      10 SUB-Y PIC X.
   05 FILLER PIC X VALUE SPACE.
      88 FILLER-SPACE VALUE "A" THRU "Z", SPACE.
   05 NUM-HOST PIC 99V9 VALUE 10.
      88 NUM-IN-RANGE VALUE K-LOW THRU K-HIGH, 1.5 THRU 2.5.
66 SPAN-AB RENAMES FLD-A THRU FLD-B.
66 SINGLE-A RENAMES FLD-A.
PROCEDURE DIVISION.
    STOP RUN.
"#;
    let (_, sink_ok) = compile_cobol(valid_src, Dialect::Cobol2023)?;
    assert!(
        !sink_ok.has_errors(),
        "unexpected: {:?}",
        sink_ok.diagnostics()
    );

    let err_src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. VAL-ERR.
ENVIRONMENT DIVISION.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-V ASSIGN TO "fv.dat".
DATA DIVISION.
FILE SECTION.
FD F-V.
01 FV-REC PIC X(2) VALUE "AA".
WORKING-STORAGE SECTION.
88 ORPHAN-88 VALUE "X".
78 BAD-CONST-PIC PIC X VALUE "A".
01 GRP.
   05 BASE-X PIC X(2).
   05 REDEF-VAL REDEFINES BASE-X PIC X(2) VALUE "BB".
   05 ODO-CNT PIC 9(2).
   05 ODO-VAL PIC X(2) OCCURS 1 TO 3 DEPENDING ON ODO-CNT VALUE "CC".
   05 BAD-THRU-ELEM PIC 99 VALUE 1 THRU 5.
   05 NUM-STR PIC 99 VALUE "AB".
   05 NUM-FIG PIC 99 VALUE SPACE.
   05 UNS-NEG-INT PIC 99 VALUE -1.
   05 UNS-NEG-DEC PIC 9V9 VALUE -1.2.
   05 UNS-PACK-NEG PIC 99 COMP-3 VALUE -1.
   05 UNS-BIN-NEG PIC 99 COMP-5 VALUE -1.
   05 SGN-PACK-OK PIC S99 COMP-3 VALUE -1.
   05 SGN-BIN-OK PIC S99 COMP-5 VALUE -1.
   05 FIT-INT-ERR PIC 99 VALUE 123.
   05 FIT-FRAC-ERR PIC 9V9 VALUE 1.23.
   05 ALPHA-NUM-ERR PIC X(2) VALUE 12.
   05 ALPHA-FLT-ERR PIC X(2) VALUE 1.5E2.
   05 PIC-A-DIG-ERR PIC A(2) VALUE "A1".
   05 TRUNC-WARN PIC X(2) VALUE "ABCD".
      88 REV-STR-RANGE VALUE "Z" THRU "A".
   05 ORD-HOST PIC 99.
      88 REV-INT-RANGE VALUE 50 THRU 10.
      88 BAD-88-CLAUSES PIC X VALUE 1.
66 BAD-REN-CLAUSES PIC X RENAMES BASE-X.
66 BAD-REN-LVL1 RENAMES GRP.
66 BAD-REN-ORDER RENAMES ODO-CNT THRU BASE-X.
PROCEDURE DIVISION.
    STOP RUN.
"#;
    let (_, sink_err) = compile_cobol(err_src, Dialect::Cobol85)?;
    assert!(sink_err.error_count() >= 18);
    assert!(sink_err.warning_count() >= 1);

    // IBM Enterprise strict mode emits a warning (not error) for VALUE on REDEFINES.
    let ibm_redef_val = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. IBM-RV.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 G.
   05 A PIC X(2).
   05 B REDEFINES A PIC X(2) VALUE "HI".
PROCEDURE DIVISION.
    STOP RUN.
"#;
    let mut ibm_cfg = DialectConfig::for_dialect(Dialect::Ibm);
    ibm_cfg.relaxed_syntax = RelaxedSyntaxMode::Strict;
    let (_, sink_ibm) = compile_with_cfg(ibm_redef_val, &ibm_cfg)?;
    assert!(!sink_ibm.has_errors());
    assert!(sink_ibm.warning_count() >= 1);
    Ok(())
}

/// Tests `atom_to_hir_const` across all literal, figurative, and constant/non-constant identifier variants.
#[test]
fn atom_to_hir_const_all_variants() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let sym_num = interner.intern("42")?;
    let sym_const = interner.intern("K-ONE")?;
    let sym_var = interner.intern("WS-VAR")?;
    let sym_missing = interner.intern("NO-SUCH")?;
    let s1 = ScaleFactor::try_new(1)?;

    let mut symtab = SymbolTable::default();
    let sc = symtab.create_scope(None);
    symtab.insert_symbol(
        sc,
        sym_const,
        SymbolCandidate {
            target: ResolvedSymbol::Constant(HirConstValue::Bytes(vec![b'K'])),
            qualifiers: Vec::new(),
            visibility: GlobalVisibility::Local,
        },
    );
    symtab.insert_symbol(
        sc,
        sym_var,
        SymbolCandidate {
            target: ResolvedSymbol::DataItem(DataItemId::new(0)),
            qualifiers: Vec::new(),
            visibility: GlobalVisibility::Local,
        },
    );

    assert_eq!(
        atom_to_hir_const(
            &ValueAtom::String {
                bytes: vec![b'A'],
                prefix: StringPrefix::Plain,
            },
            sc,
            &symtab,
        ),
        Some(HirConstValue::Bytes(vec![b'A']))
    );
    assert_eq!(
        atom_to_hir_const(&ValueAtom::Integer(sym_num), sc, &symtab),
        Some(HirConstValue::Decimal {
            text: sym_num,
            scale: ScaleFactor::ZERO,
            sign: HirDecimalSign::UnsignedOrEmbedded,
        })
    );
    for (lsign, expected_hsign) in [
        (LiteralSign::Unsigned, HirDecimalSign::UnsignedOrEmbedded),
        (LiteralSign::Positive, HirDecimalSign::Positive),
        (LiteralSign::Negative, HirDecimalSign::Negative),
    ] {
        let dec = ValueAtom::Decimal {
            digits: sym_num,
            scale: s1,
            sign: lsign,
        };
        let exp = HirConstValue::Decimal {
            text: sym_num,
            scale: s1,
            sign: expected_hsign,
        };
        assert_eq!(atom_to_hir_const(&dec, sc, &symtab), Some(exp));
    }
    assert_eq!(
        atom_to_hir_const(&ValueAtom::Float(sym_num), sc, &symtab),
        Some(HirConstValue::Float(sym_num))
    );

    for (fig, expected) in [
        (FigurativeValue::Zero, HirFigurative::Zero),
        (FigurativeValue::Space, HirFigurative::Space),
        (FigurativeValue::HighValue, HirFigurative::HighValue),
        (FigurativeValue::LowValue, HirFigurative::LowValue),
        (FigurativeValue::Quote, HirFigurative::Quote),
        (FigurativeValue::Null, HirFigurative::Null),
        (
            FigurativeValue::All(vec![b'*']),
            HirFigurative::All(vec![b'*']),
        ),
    ] {
        assert_eq!(
            atom_to_hir_const(&ValueAtom::Figurative(fig), sc, &symtab),
            Some(HirConstValue::Figurative(expected))
        );
    }

    assert_eq!(
        atom_to_hir_const(&ValueAtom::Identifier(sym_const), sc, &symtab),
        Some(HirConstValue::Bytes(vec![b'K']))
    );
    assert!(atom_to_hir_const(&ValueAtom::Identifier(sym_var), sc, &symtab).is_none());
    assert!(atom_to_hir_const(&ValueAtom::Identifier(sym_missing), sc, &symtab).is_none());
    assert!(
        atom_to_hir_const(
            &ValueAtom::Identifier(sym_const),
            ScopeId::new(999),
            &symtab
        )
        .is_none()
    );
    Ok(())
}

/// Tests synthetic AST edge cases in `solve_data_division`: duplicate clauses, `FD` records, and unbounded `LINKAGE` groups.
#[test]
fn synthetic_ast_edge_cases_duplicates_and_linkage() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let sym_file_desc = interner.intern("FD-SYN")?;
    let sym_record = interner.intern("SYN-REC")?;
    let sym_alpha = interner.intern("SYN-F1")?;
    let sym_beta = interner.intern("SYN-F2")?;
    let bogus = SymbolId::new(88_888);

    let mut dup_entry = blank_entry(LevelNumber::Hierarchy(5), DataEntryName::Named(sym_alpha));
    dup_entry.picture = Some((bogus, span()));
    dup_entry.clause_counts = DataClauseCounts {
        pic_count: 2,
        usage_count: 2,
        occurs_count: 2,
        value_count: 2,
        redefines_count: 2,
    };

    // Unbounded group in LINKAGE to exercise `own_dim.max == u32::MAX` in `close_stack_to_level`.
    let lk_grp = blank_entry(LevelNumber::Hierarchy(1), DataEntryName::Named(sym_record));
    let mut lk_unb_grp = blank_entry(LevelNumber::Hierarchy(5), DataEntryName::Filler);
    lk_unb_grp.occurs = Some(OccursClause {
        min: 1,
        max: OccursUpperBound::Unbounded,
        depending_on: Some(Vec::new()),
        keys: Vec::new(),
        indexed_by: vec![bogus, bogus],
        span: span(),
    });
    let mut lk_leaf = blank_entry(LevelNumber::Hierarchy(10), DataEntryName::Named(sym_beta));
    lk_leaf.usage = Some(UsageKind::Pointer);
    lk_leaf.redefines = Some(bogus);

    let mut data = DataDivision::default();
    data.file_section.push(FdSdEntry {
        kind: FdOrSd::Fd,
        file_name: sym_file_desc,
        external: ExternalLinkage::Internal,
        global: GlobalVisibility::Local,
        record_size: None,
        linage: None,
        reports: Vec::new(),
        report_spans: Vec::new(),
        label_records_span: None,
        data_records_span: None,
        record_clause_span: None,
        after_fd_span: span(),
        code_set: None,
        records: vec![
            blank_entry(LevelNumber::Hierarchy(1), DataEntryName::Named(sym_record)),
            dup_entry,
        ],
        span: span(),
    });
    data.linkage = vec![lk_grp, lk_unb_grp, lk_leaf];

    let mut symtab = SymbolTable::default();
    let sc = symtab.create_scope(None);
    let dcfg = DialectConfig::for_dialect(Dialect::Cobol2023);
    let (_, sink) = run_solve(&data, &dcfg, &mut symtab, sc, &mut interner)?;
    assert!(sink.error_count() >= 7);
    Ok(())
}

/// Tests synthetic AST edge cases in `solve_data_division`: malformed `66 RENAMES`, `78` constants, and `88` conditions.
#[test]
fn synthetic_ast_edge_cases_renames_constants_and_conditions() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let sym_alpha = interner.intern("SYN-F1")?;
    let s_k = interner.intern("SYN-K")?;
    let sym_renames = interner.intern("SYN-REN")?;
    let s_c88 = interner.intern("SYN-C88")?;
    let s_not_int = interner.intern("NOT-AN-INT")?;
    let bogus = SymbolId::new(88_888);

    let mut c78_filler = blank_entry(LevelNumber::Constant78, DataEntryName::Filler);
    c78_filler.occurs = Some(OccursClause {
        min: 1,
        max: OccursUpperBound::Bounded(2),
        depending_on: None,
        keys: Vec::new(),
        indexed_by: Vec::new(),
        span: span(),
    });
    let c78_no_val = blank_entry(LevelNumber::Constant78, DataEntryName::Named(s_k));
    let mut c78_empty_vals = blank_entry(LevelNumber::Constant78, DataEntryName::Named(s_k));
    c78_empty_vals.value = Some(ValueClause {
        values: Vec::new(),
        false_value: None,
        span: span(),
    });
    let mut c78_bad_ident = blank_entry(LevelNumber::Constant78, DataEntryName::Named(s_k));
    c78_bad_ident.value = Some(ValueClause {
        values: vec![ValueRange {
            low: ValueAtom::Identifier(bogus),
            high: None,
            span: span(),
        }],
        false_value: None,
        span: span(),
    });
    let mut c78_ok = blank_entry(LevelNumber::Constant78, DataEntryName::Named(s_k));
    c78_ok.value = Some(ValueClause {
        values: vec![ValueRange {
            low: ValueAtom::Integer(s_not_int),
            high: None,
            span: span(),
        }],
        false_value: None,
        span: span(),
    });

    let mut working_storage = vec![
        c78_filler,
        c78_no_val,
        c78_empty_vals,
        c78_bad_ident,
        c78_ok,
        build_host_for_88_and_66(sym_alpha, bogus, s_not_int),
        blank_entry(LevelNumber::Condition88, DataEntryName::Filler),
        blank_entry(LevelNumber::Condition88, DataEntryName::Named(s_c88)),
        build_weird_88_entry(s_c88, bogus, s_not_int),
    ];
    working_storage.extend(build_malformed_renames_entries(
        sym_renames,
        sym_alpha,
        s_k,
        bogus,
    ));
    let data = DataDivision {
        working_storage,
        ..DataDivision::default()
    };

    let mut symtab = SymbolTable::default();
    let sc = symtab.create_scope(None);
    let dcfg = DialectConfig::for_dialect(Dialect::Cobol2023);
    let (_, sink) = run_solve(&data, &dcfg, &mut symtab, sc, &mut interner)?;
    assert!(sink.error_count() >= 10);
    Ok(())
}

/// Builds a numeric elementary item with bogus `SymbolId` decimal/integer values for synthetic coverage.
///
/// # Arguments
///
/// * `name_sym` - Item symbol.
/// * `bogus` - Uninterned symbol ID.
/// * `not_int` - Non-numeric symbol ID.
fn build_host_for_88_and_66(name_sym: SymbolId, bogus: SymbolId, not_int: SymbolId) -> DataEntry {
    let mut e = blank_entry(LevelNumber::Hierarchy(1), DataEntryName::Named(name_sym));
    e.usage = Some(UsageKind::BinaryLongSigned);
    e.value = Some(ValueClause {
        values: vec![
            ValueRange {
                low: ValueAtom::Integer(bogus),
                high: None,
                span: span(),
            },
            ValueRange {
                low: ValueAtom::Decimal {
                    digits: bogus,
                    scale: ScaleFactor::ZERO,
                    sign: LiteralSign::Positive,
                },
                high: None,
                span: span(),
            },
            ValueRange {
                low: ValueAtom::Identifier(not_int),
                high: None,
                span: span(),
            },
        ],
        false_value: None,
        span: span(),
    });
    e
}

/// Builds a level-`88` entry with unparseable integer ranges and unresolvable identifier atoms.
///
/// # Arguments
///
/// * `cond_sym` - Condition-name symbol.
/// * `bogus` - Uninterned symbol ID.
/// * `not_int` - Non-integer symbol ID.
fn build_weird_88_entry(cond_sym: SymbolId, bogus: SymbolId, not_int: SymbolId) -> DataEntry {
    let mut e = blank_entry(LevelNumber::Condition88, DataEntryName::Named(cond_sym));
    e.usage = Some(UsageKind::Display);
    e.value = Some(ValueClause {
        values: vec![
            ValueRange {
                low: ValueAtom::Integer(not_int),
                high: Some(ValueAtom::Integer(bogus)),
                span: span(),
            },
            ValueRange {
                low: ValueAtom::Identifier(bogus),
                high: None,
                span: span(),
            },
        ],
        false_value: None,
        span: span(),
    });
    e
}

/// Builds synthetic `66 RENAMES` entries covering `Filler`, missing clause, empty paths, and non-data targets.
///
/// # Arguments
///
/// * `ren_sym` - `66` item name symbol.
/// * `item_sym` - Valid `01` data item symbol.
/// * `const_sym` - Constant symbol (`78`).
/// * `bogus` - Unresolved symbol ID.
fn build_malformed_renames_entries(
    ren_sym: SymbolId,
    item_sym: SymbolId,
    const_sym: SymbolId,
    bogus: SymbolId,
) -> Vec<DataEntry> {
    let ren_filler = blank_entry(LevelNumber::Renames66, DataEntryName::Filler);
    let ren_missing = blank_entry(LevelNumber::Renames66, DataEntryName::Named(ren_sym));

    let mut ren_empty_from = blank_entry(LevelNumber::Renames66, DataEntryName::Named(ren_sym));
    ren_empty_from.renames = Some(RenamesRange {
        from: Vec::new(),
        thru: None,
        span: span(),
    });

    let mut ren_const_from = blank_entry(LevelNumber::Renames66, DataEntryName::Named(ren_sym));
    ren_const_from.renames = Some(RenamesRange {
        from: vec![const_sym],
        thru: None,
        span: span(),
    });

    let mut ren_empty_thru = blank_entry(LevelNumber::Renames66, DataEntryName::Named(ren_sym));
    ren_empty_thru.renames = Some(RenamesRange {
        from: vec![item_sym],
        thru: Some(Vec::new()),
        span: span(),
    });

    let mut ren_const_thru = blank_entry(LevelNumber::Renames66, DataEntryName::Named(ren_sym));
    ren_const_thru.renames = Some(RenamesRange {
        from: vec![item_sym],
        thru: Some(vec![const_sym]),
        span: span(),
    });

    let mut ren_undef_from = blank_entry(LevelNumber::Renames66, DataEntryName::Named(ren_sym));
    ren_undef_from.renames = Some(RenamesRange {
        from: vec![bogus],
        thru: None,
        span: span(),
    });

    vec![
        ren_filler,
        ren_missing,
        ren_empty_from,
        ren_const_from,
        ren_empty_thru,
        ren_const_thru,
        ren_undef_from,
    ]
}

/// Covers remaining `REDEFINES` level/section mismatches, `66`/`78`/`88` clause combinations,
/// `Relaxed` `REDEFINES VALUE`, valid integer `THRU`, unresolved `ODO`, `COMP-1` negative `VALUE`,
/// and `SymbolInterner` capacity exhaustion during `solve_data_division`.
#[test]
fn layout_remaining_clause_and_interner_limit_branches() -> Result<()> {
    // 1. `SymbolInterner` capacity exhaustion in `solve_data_division`.
    let mut tiny_interner = SymbolInterner::with_limit(2);
    let mut tiny_symtab = SymbolTable::default();
    let tiny_scope = tiny_symtab.create_scope(None);
    let mut tiny_sink = DiagnosticSink::new(WarningPolicy::new(), 10);
    assert!(
        solve_data_division(
            &DataDivision::default(),
            tiny_scope,
            PictureContext::default(),
            &DialectConfig::for_dialect(Dialect::Default),
            &mut tiny_symtab,
            &mut tiny_interner,
            &mut tiny_sink,
        )
        .is_err()
    );

    // 2. Cross-section `01 REDEFINES`, mismatched sibling levels (`05` vs `03`), `66`/`78`/`88` forbidden clauses,
    //    valid integer `88` `THRU`, unresolved `DEPENDING ON`, and negative `COMP-1 VALUE`.
    let edge_src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. LAYOUT-EDGES.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 WS-SEC-TARGET PIC X(4).
01 GRP-MISMATCH.
   05 SUB-FIVE PIC X(2).
   03 SUB-THREE REDEFINES SUB-FIVE PIC X(2).
   05 NUM-OK PIC 99.
      88 COND-OK VALUE 10 THRU 20.
      88 COND-OCC OCCURS 2 VALUE 10.
      88 COND-RED REDEFINES NUM-OK VALUE 10.
   05 FLT-NEG COMP-1 VALUE -1.
   05 ODO-TBL OCCURS 1 TO 5 TIMES DEPENDING ON NO-SUCH-ODO PIC X.
   05 ODO-COND OCCURS 1 TO 5 TIMES DEPENDING ON COND-OK PIC X.
66 REN-USG RENAMES SUB-FIVE USAGE COMP.
66 REN-OCC RENAMES SUB-FIVE OCCURS 2.
66 REN-VAL RENAMES SUB-FIVE VALUE "AB".
78 K-OCC OCCURS 2 VALUE 1.
78 K-RED REDEFINES WS-SEC-TARGET VALUE 1.
LINKAGE SECTION.
01 LK-REDEF REDEFINES WS-SEC-TARGET PIC X(4).
PROCEDURE DIVISION.
    STOP RUN.
"#;
    let (_, sink_edges) = compile_cobol(edge_src, Dialect::Cobol2023)?;
    assert!(sink_edges.error_count() >= 9);

    // 3. `VALUE` under `REDEFINES` with `RelaxedSyntaxMode::Relaxed` produces no error.
    let mut gnu_relaxed = DialectConfig::for_dialect(Dialect::Gnu);
    gnu_relaxed.relaxed_syntax = RelaxedSyntaxMode::Relaxed;
    gnu_relaxed.apply_conf_key_value("binary-size", "1--8", "<test>", 1)?;
    let redef_relaxed_src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. REDEF-REL.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 R-GRP.
   05 R1 PIC X(2).
   05 R2 REDEFINES R1 PIC X(2) VALUE "OK".
01 B-GRP BASED.
   05 B-CHILD PIC X(2).
01 E-GRP EXTERNAL.
   05 E-CHILD PIC X(2).
01 C5-1B BINARY-CHAR UNSIGNED VALUE 200.
01 C5-2B BINARY-SHORT UNSIGNED VALUE 50000.
01 C5-3B PIC 9(5) COMP-5 VALUE 99999.
01 C5-6B PIC 9(12) COMP-5 VALUE 123456.
01 C5-16B PIC 9(35) COMP-5 VALUE 123456.
01 ODO-CNT PIC 9 VALUE 2.
01 KEY-GRP.
   05 KEY-TBL OCCURS 1 TO 4 TIMES DEPENDING ON ODO-CNT
      ASCENDING KEY IS K-A
      DESCENDING KEY IS K-B K-MISSING
      INDEXED BY K-IDX.
      10 K-SUB.
         15 K-A PIC X(2).
         15 K-B PIC 9(2).
PROCEDURE DIVISION.
    STOP RUN.
"#;
    let (_, sink_rel) = compile_with_cfg(redef_relaxed_src, &gnu_relaxed)?;
    assert!(!sink_rel.has_errors());
    Ok(())
}

/// Covers M11 IBM `COMP REDEFINES POINTER` 8-byte upgrade, `BASED`/`EXTERNAL` level checks, and `EXTERNAL AS` / `FILLER`.
#[test]
fn m11_ibm_pointer_redefines_based_and_external_layout_coverage() -> Result<()> {
    let ibm_src = concat!(
        "       IDENTIFICATION DIVISION.\n",
        "       PROGRAM-ID. IBM-PTR.\n",
        "       DATA DIVISION.\n",
        "       WORKING-STORAGE SECTION.\n",
        "       01 B4 PIC S9(9) COMP.\n",
        "       01 P1 REDEFINES B4 USAGE POINTER.\n",
        "       01 AX PIC X(8).\n",
        "       01 P3 REDEFINES AX USAGE POINTER.\n",
        "       01 P4 USAGE POINTER.\n",
        "       PROCEDURE DIVISION.\n",
        "           STOP RUN.\n",
    );
    let (ibm_mod_opt, ibm_sink) = compile_cobol(ibm_src, Dialect::Ibm)?;
    assert!(!ibm_sink.has_errors());
    assert!(ibm_mod_opt.is_some());

    let ibm_occ_src = concat!(
        "       IDENTIFICATION DIVISION.\n",
        "       PROGRAM-ID. IBM-OCC.\n",
        "       DATA DIVISION.\n",
        "       WORKING-STORAGE SECTION.\n",
        "       01 G-OCC.\n",
        "          05 B4-ARR PIC S9(9) COMP OCCURS 2.\n",
        "          05 P2 REDEFINES B4-ARR USAGE POINTER.\n",
        "       PROCEDURE DIVISION.\n",
        "           STOP RUN.\n",
    );
    let (_, ibm_occ_sink) = compile_cobol(ibm_occ_src, Dialect::Ibm)?;
    assert!(ibm_occ_sink.has_errors());

    let m11_ok_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M11-LAY-OK.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "77 B77 PIC X(4) BASED.\n",
        "77 E77 PIC X(4) EXTERNAL.\n",
        "01 E-ALIAS PIC X(4) EXTERNAL AS \"CUSTOM_SEG\".\n",
        "01 FILLER PIC X(4) EXTERNAL AS \"FILLER_SEG\".\n",
        "PROCEDURE DIVISION.\n",
        "    STOP RUN.\n",
    );
    let (_, ok_sink) = compile_cobol(m11_ok_src, Dialect::Gnu)?;
    assert!(!ok_sink.has_errors());

    let m11_bad_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M11-LAY-BAD.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 G1.\n",
        "   05 BAD-BASED PIC X BASED.\n",
        "   05 BAD-EXT PIC X EXTERNAL.\n",
        "01 FILLER PIC X(4) EXTERNAL.\n",
        "PROCEDURE DIVISION.\n",
        "    STOP RUN.\n",
    );
    let (_, bad_sink) = compile_cobol(m11_bad_src, Dialect::Gnu)?;
    assert_eq!(bad_sink.error_count(), 3);
    Ok(())
}
