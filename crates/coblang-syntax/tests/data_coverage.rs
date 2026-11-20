//! Exhaustive unit and branch-coverage tests for `DATA DIVISION` AST and parser (`parser/data.rs`).

#![deny(clippy::indexing_slicing)]

use std::path::Path;

use coblang_common::{Dialect, Result, SourceFormat};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap, SymbolInterner, WarningPolicy};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, preprocess_source_file,
};
use coblang_syntax::ast::{
    BlankWhenZeroMode, CompilationGroup, CompilationUnit, DataEntryName, ExternalLinkage, FdOrSd,
    GlobalVisibility, JustificationMode, LengthVariability, OccursUpperBound, RecordSizeClause,
    ReportGroupType, ResidencyKind, SignPlacement, SignSeparation, StorageLinkageFlags,
    SyncAlignment, TypeRole, UsageKind,
};
use coblang_syntax::parse_preprocessed_unit;

/// Preprocesses, lexes, and parses a synthetic COBOL snippet into a [`CompilationGroup`].
///
/// # Arguments
///
/// * `src` - Synthetic COBOL source text.
fn parse_data_src(src: &str) -> Result<(CompilationGroup, SymbolInterner, DiagnosticSink)> {
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
    let unit =
        preprocess_source_file("data.cob", Path::new("."), src, &pp_cfg, &mut sm, &mut sink)?;
    let mut interner = SymbolInterner::new();
    let group = parse_preprocessed_unit(&unit, &dialect, &mut interner, &mut sink)?;
    Ok((group, interner, sink))
}

/// Exercises `StorageLinkageFlags::default`, `FILE SECTION`, `FD`/`SD`, `RECORD`, `LINAGE`, `REPORTS`, and `CODE-SET`.
#[test]
fn file_section_fd_sd_linage_and_record_size_coverage() -> Result<()> {
    let def_flags = StorageLinkageFlags::default();
    assert_eq!(def_flags.external, ExternalLinkage::Internal);
    assert_eq!(def_flags.global, GlobalVisibility::Local);
    assert_eq!(def_flags.residency, ResidencyKind::AutomaticOrStatic);
    assert_eq!(def_flags.length_mode, LengthVariability::Fixed);
    assert_eq!(def_flags.type_role, TypeRole::DataInstance);

    let src = concat!(
        "PROGRAM-ID. FD-TEST.
",
        "DATA DIVISION.
",
        "BOGUS-DATA-TOKEN.
",
        "FILE SECTION.
",
        "BOGUS-IN-FILE-SECTION.
",
        "FD .
",
        r#"FD F-ONE IS EXTERNAL AS "ext_f1" IS GLOBAL
"#,
        "    RECORD CONTAINS 40 TO 80 CHARACTERS
",
        "    LINAGE IS 60 LINES WITH FOOTING AT 55 LINES AT TOP 3 LINES AT BOTTOM 2
",
        "    REPORTS ARE RPT-A RPT-B CODE-SET IS ALPHA-1
",
        "    BLOCK CONTAINS 0 RECORDS.
",
        "01 REC-ONE PIC X(80).
",
        "SD S-ONE EXTERNAL
",
        "    RECORD IS VARYING IN SIZE FROM 10 TO 100 BYTES DEPENDING ON WS-LEN
",
        "    REPORT IS RPT-C.
",
        "01 SORT-REC PIC X(100).
",
        "FD F-TWO RECORD VARYING 20 TO 60 LINAGE IS WS-BODY AT TOP WS-T AT BOTTOM WS-B STOP-HERE.
",
        "FD F-THREE RECORD CONTAINS 32 BYTES.
",
        "FD F-FOUR RECORD VARYING DEPENDING ON WS-LEN.
",
        "FD F-FIVE LINAGE IS 50 LINES LINES AT .
",
        "FD F-SIX REPORTS ARE .
",
        "FD F-SEVEN EXTERNAL
",
        "PROCEDURE DIVISION.
",
        "    STOP RUN.
",
    );
    let (grp, _, sink) = parse_data_src(src)?;
    assert!(sink.error_count() >= 3);
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let fs = &p.data.file_section;
    assert_eq!(fs.len(), 9);
    if let Some(f1) = fs.get(1) {
        assert_eq!(f1.kind, FdOrSd::Fd);
        assert_eq!(f1.global, GlobalVisibility::Global);
        assert!(matches!(f1.external, ExternalLinkage::External(Some(_))));
        assert!(matches!(
            f1.record_size,
            Some(RecordSizeClause::Contains {
                min_bytes: 40,
                max_bytes: Some(80)
            })
        ));
        assert!(f1.linage.is_some());
        assert_eq!(f1.reports.len(), 2);
        assert!(f1.code_set.is_some());
    }
    if let Some(s1) = fs.get(2) {
        assert_eq!(s1.kind, FdOrSd::Sd);
        assert!(matches!(
            s1.record_size,
            Some(RecordSizeClause::Varying {
                min_bytes: Some(10),
                max_bytes: Some(100),
                depending_on: Some(_)
            })
        ));
    }
    Ok(())
}

/// Exercises display, packed/computational, and floating-point `UsageKind` representations.
#[test]
fn data_entry_all_usages_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. USAGE-TEST.\n",
        "DATA DIVISION.\n",
        "01 IMPLICIT-WS-ITEM PIC 9.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 U01 USAGE IS DISPLAY.\n",
        "01 U02 DISPLAY-1.\n",
        "01 U03 NATIONAL.\n",
        "01 U04 UTF-8.\n",
        "01 U05 BINARY.\n",
        "01 U06 COMP.\n",
        "01 U07 COMPUTATIONAL-4.\n",
        "01 U08 COMP-5.\n",
        "01 U09 PACKED-DECIMAL.\n",
        "01 U10 COMP-3.\n",
        "01 U11 COMP-6.\n",
        "01 U12 COMP-X.\n",
        "01 U13 COMP-N.\n",
        "01 U14 COMP-1.\n",
        "01 U15 FLOAT-SHORT.\n",
        "01 U16 COMP-2.\n",
        "01 U17 FLOAT-LONG.\n",
        "01 U18 FLOAT-EXTENDED.\n",
        "01 U19 FLOAT-DECIMAL-16.\n",
        "01 U20 FLOAT-DECIMAL-34.\n",
    );
    let (grp, _, _) = parse_data_src(src)?;
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let ws = &p.data.working_storage;
    assert_eq!(ws.len(), 21);
    assert!(
        ws.iter()
            .any(|e| e.usage == Some(UsageKind::FloatDecimal34))
    );
    Ok(())
}

/// Exercises C/COBOL2023 sized binary, bit, index, pointer, object-reference, and invalid `USAGE` clauses.
#[test]
fn data_entry_sized_binary_pointer_and_object_usages_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. USAGE-TEST-2.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 U21 BINARY-CHAR SIGNED.\n",
        "01 U22 BINARY-CHAR UNSIGNED.\n",
        "01 U23 BINARY-SHORT SIGNED.\n",
        "01 U24 BINARY-SHORT UNSIGNED.\n",
        "01 U25 BINARY-LONG SIGNED.\n",
        "01 U26 BINARY-LONG UNSIGNED.\n",
        "01 U27 BINARY-DOUBLE SIGNED.\n",
        "01 U28 BINARY-DOUBLE UNSIGNED.\n",
        "01 U29 BINARY-C-LONG SIGNED.\n",
        "01 U30 BINARY-C-LONG UNSIGNED.\n",
        "01 U30A SIGNED-CHAR.\n",
        "01 U30B UNSIGNED-CHAR.\n",
        "01 U30C SIGNED-SHORT.\n",
        "01 U30D UNSIGNED-SHORT.\n",
        "01 U30E SIGNED-INT.\n",
        "01 U30F UNSIGNED-LONG.\n",
        "01 U31 BIT.\n",
        "01 U32 INDEX.\n",
        "01 U33 POINTER.\n",
        "01 U34 PROGRAM-POINTER.\n",
        "01 U35 FUNCTION-POINTER.\n",
        "01 U36 OBJECT REFERENCE MY-CLASS.\n",
        "01 U37 USAGE IS OBJECT REFERENCE.\n",
        "01 U37B OBJECT REFERENCE BASED.\n",
        "01 U38 USAGE IS BOGUS-USAGE.\n",
        "01 U39 USAGE IS .\n",
        "01 U40 PIC .\n",
        "01 U41 END FOOBAR.\n",
    );
    let (grp, _, _) = parse_data_src(src)?;
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let ws = &p.data.working_storage;
    assert_eq!(ws.len(), 28);
    assert!(
        ws.iter()
            .any(|e| e.usage == Some(UsageKind::BinaryCLongUnsigned))
    );
    assert!(
        ws.iter()
            .any(|e| matches!(e.usage, Some(UsageKind::ObjectReference(Some(_)))))
    );
    assert!(
        ws.iter()
            .any(|e| e.usage == Some(UsageKind::ObjectReference(None)))
    );
    Ok(())
}

/// Exercises `REDEFINES`, `RENAMES`, `SIGN`, `SYNC`, `JUST`, `BLANK`, `CONSTANT`, `TYPEDEF`, and `OCCURS`.
#[test]
fn data_entry_clauses_occurs_and_storage_flags_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. CLAUSES-TEST.\n",
        "DATA DIVISION.\n",
        "LOCAL-STORAGE SECTION.\n",
        "BOGUS-NO-LEVEL.\n",
        "50 BAD-LEVEL-FIFTY PIC X.\n",
        "01 FILLER PIC X.\n",
        "01 PIC 9.\n",
        "01 .\n",
        "01 (1).\n",
        "01 GRP-A.\n",
        "   05 FLD-1 PIC S9(4) SIGN IS LEADING SEPARATE CHARACTER SYNC LEFT.\n",
        "   05 FLD-2 PICTURE 99 SIGN IS TRAILING SYNC RIGHT.\n",
        "   05 FLD-3 PIC S99 LEADING SYNCHRONIZED.\n",
        "   05 FLD-4 PIC X(4) JUSTIFIED RIGHT BLANK WHEN ZERO.\n",
        "   05 FLD-5 PIC X(4) JUST.\n",
        "01 GRP-REDEF REDEFINES GRP-A PIC X(18).\n",
        "66 REN-1 RENAMES FLD-1.\n",
        "66 REN-2 RENAMES FLD-1 THRU FLD-4.\n",
        "78 CONST-78 VALUE 42.\n",
        "01 CONST-01 CONSTANT IS GLOBAL AS \"HI\".\n",
        "01 CONST-02 CONSTANT FROM + 10.\n",
        "01 CONST-03 CONSTANT.\n",
        "01 EXT-1 IS EXTERNAL AS \"ext_one\" PIC X.\n",
        "01 EXT-2 EXTERNAL GLOBAL BASED PIC X.\n",
        "01 T-DEF IS TYPEDEF STRONG PIC 9(4).\n",
        "01 T-INST TYPE TO T-DEF.\n",
        "01 T-EMPTY TYPE .\n",
        "01 T-SAME SAME AS FLD-1.\n",
        "01 DYN-STR DYNAMIC LENGTH PIC X.\n",
        "01 TBL-GRP.\n",
        "   05 TBL-FIXED OCCURS 5 TIMES ASCENDING KEY IS K-1 DESCENDING KEY IS K-2 INDEXED BY IX-1.\n",
        "   05 TBL-KEY-PIC OCCURS 3 ASCENDING KEY IS K-1 PIC X.\n",
        "   05 TBL-KEY-END OCCURS 3 ASCENDING KEY IS K-1.\n",
        "   05 TBL-KEY-BAD OCCURS 3 DESCENDING KEY IS K-2 +.\n",
        "   05 TBL-IDX-PIC OCCURS 3 INDEXED BY IX-2 PIC X.\n",
        "   05 TBL-IDX-DEP OCCURS 3 INDEXED BY IX-3 DEPENDING ON ODO-N.\n",
        "   05 TBL-IDX-BAD OCCURS 3 INDEXED BY IX-4 +.\n",
        "   05 TBL-BOUND OCCURS 1 TO 10 TIMES DEPENDING ON ODO-N.\n",
        "   05 TBL-UNB OCCURS 0 TO UNBOUNDED DEPENDING ON ODO-N.\n",
        "   05 TBL-DYN OCCURS DYNAMIC CAPACITY CAP-IX STOP-OCC.\n",
    );
    let (grp, _, sink) = parse_data_src(src)?;
    assert!(sink.error_count() >= 3);
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let ls = &p.data.local_storage;
    assert!(ls.iter().any(|e| e.sign.is_some_and(|s| {
        s.placement == SignPlacement::Leading && s.separation == SignSeparation::SeparateCharacter
    })));
    assert!(ls.iter().any(|e| e.sync == SyncAlignment::SyncLeft));
    assert!(ls.iter().any(|e| e.sync == SyncAlignment::SyncRight));
    assert!(ls.iter().any(|e| e.sync == SyncAlignment::SyncDefault));
    assert!(
        ls.iter()
            .any(|e| e.justified == JustificationMode::JustifiedRight)
    );
    assert!(
        ls.iter()
            .any(|e| e.blank_when_zero == BlankWhenZeroMode::BlankWhenZero)
    );
    assert!(ls.iter().any(|e| {
        e.occurs
            .as_ref()
            .is_some_and(|o| o.max == OccursUpperBound::Unbounded)
    }));
    Ok(())
}

/// Exercises `LINKAGE SECTION`, `ANY LENGTH`, `VALUE` / `ALL` / figurative constants, and level-88 clauses.
#[test]
fn data_entry_linkage_values_and_level_88_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. VALUES-TEST.\n",
        "DATA DIVISION.\n",
        "LOCAL-STORAGE SECTION.\n",
        "01 LS-ANCHOR PIC X.\n",
        "LINKAGE SECTION.\n",
        "01 LNK-ANY ANY LENGTH PIC X.\n",
        "01 VAL-ITEMS.\n",
        "   05 V-COMP VALUE 0 COMP-5.\n",
        "   05 V-STR VALUE \"A\".\n",
        "   05 V-DEC VALUE 12.34.\n",
        "   05 V-FLT VALUE 1.5E+2.\n",
        "   05 V-NEG-INT VALUE - 9.\n",
        "   05 V-POS-DEC VALUE + 1.25.\n",
        "   05 V-NEG-DEC VALUE - 1.25.\n",
        "   05 V-BAD-SIGN VALUE + \"X\".\n",
        "   05 V-ALL-S VALUE ALL \"*\".\n",
        "   05 V-ALL-Z VALUE ALL ZERO.\n",
        "   05 V-ALL-SP VALUE ALL SPACES.\n",
        "   05 V-ALL-Q VALUE ALL QUOTES.\n",
        "   05 V-ALL-HV VALUE ALL HIGH-VALUES.\n",
        "   05 V-ALL-LV VALUE ALL LOW-VALUES.\n",
        "   05 V-ALL-W VALUE ALL OTHER-SYM.\n",
        "   05 V-ALL-N VALUE ALL 9.\n",
        "   05 V-FIGS VALUES ARE ZERO SPACE HIGH-VALUE LOW-VALUE QUOTE NULL SYM-ID (1).\n",
        "   88 COND-88 VALUES ARE 1 THRU 5 10 THROUGH 20 WHEN SET TO FALSE IS 0.\n",
        "01 UNRECOGNIZED-CLAUSE-ITEM FOOBAR-CLAUSE.\n",
        "01 IS-BEFORE-PROC IS\n",
        "PROCEDURE DIVISION.\n",
        "    STOP RUN.\n",
    );
    let (grp, _, sink) = parse_data_src(src)?;
    assert!(sink.error_count() >= 1);
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    assert_eq!(
        p.data.linkage.first().map(|e| e.flags.length_mode),
        Some(LengthVariability::AnyLength)
    );
    Ok(())
}

/// Exercises `REPORT SECTION` (`RD`, page limits, controls, and all seven `ReportGroupType` variants).
#[test]
fn report_section_rd_and_group_types_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. RPT-ONLY.\n",
        "DATA DIVISION.\n",
        "REPORT SECTION.\n",
        "BOGUS-BEFORE-RD.\n",
        "RD .\n",
        "RD RPT-MAIN IS GLOBAL\n",
        "    CONTROLS ARE FINAL CTRL-A +\n",
        "    PAGE LIMIT IS 60 LINES\n",
        "    HEADING 1 FIRST DETAIL 5 LAST DETAIL 50 FOOTING 58 EXTRA-RD.\n",
        "BOGUS-IN-RD.\n",
        "01 FILLER TYPE IS RH LINE NUMBER IS PLUS 1.\n",
        "01 GRP-PH TYPE IS PH COLUMN NUMBER IS 5 PIC X(10) VALUE IS \"HDR\" EXTRA-GRP.\n",
        "01 TYPE IS CH CTRL-A COL 10 SOURCE IS WS-SRC.\n",
        "01 . \n",
        "01 (1).\n",
        "01 G-DE TYPE IS DE SUM WS-AMT SUM .\n",
        "01 G-CF TYPE IS CF CTRL-A.\n",
        "01 G-PF TYPE IS PF.\n",
        "01 G-RF TYPE IS RF.\n",
        "01 G-RH2 TYPE IS REPORT HEADING.\n",
        "01 G-RF2 TYPE IS REPORT FOOTING.\n",
        "01 G-PH2 TYPE IS PAGE HEADING.\n",
        "01 G-PF2 TYPE IS PAGE FOOTING.\n",
        "01 G-CH2 TYPE IS CONTROL HEADING FOR CTRL-A.\n",
        "01 G-CF2 TYPE IS CONTROL FOOTING FOR CTRL-A.\n",
        "01 G-UNK TYPE IS BOGUS-TYPE.\n",
        "01 G-MT TYPE IS .\n",
        "01 G-NOPIC PIC .\n",
        "SCREEN SECTION.\n",
        "01 SCR-AFTER-RPT BLANK SCREEN.\n",
    );
    let (grp, _, sink) = parse_data_src(src)?;
    assert!(sink.error_count() >= 3);
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let rds = &p.data.report_section;
    assert_eq!(rds.len(), 2);
    if let Some(rd) = rds.get(1) {
        assert_eq!(rd.global, GlobalVisibility::Global);
        assert_eq!(rd.page_limit, Some(60));
        assert_eq!(rd.heading, Some(1));
        assert_eq!(rd.first_detail, Some(5));
        assert_eq!(rd.last_detail, Some(50));
        assert_eq!(rd.footing, Some(58));
        for expected_gt in [
            ReportGroupType::ReportHeading,
            ReportGroupType::PageHeading,
            ReportGroupType::ControlHeading,
            ReportGroupType::Detail,
            ReportGroupType::ControlFooting,
            ReportGroupType::PageFooting,
            ReportGroupType::ReportFooting,
        ] {
            assert!(rd.groups.iter().any(|g| g.group_type == Some(expected_gt)));
        }
    }
    Ok(())
}

/// Exercises `SCREEN SECTION` item names, positioning, `FROM`/`TO`/`USING`, attributes, and error recovery.
#[test]
fn screen_section_full_coverage() -> Result<()> {
    let src = concat!(
        "PROGRAM-ID. SCR-ONLY.\n",
        "DATA DIVISION.\n",
        "SCREEN SECTION.\n",
        "BOGUS-IN-SCREEN.\n",
        "01 FILLER BLANK SCREEN.\n",
        "01 LINE 1 COLUMN 2 VALUE \"TITLE\".\n",
        "01 .\n",
        "01 (1).\n",
        "01 SCR-FLD PIC X(10) LINE NUMBER IS PLUS 2 COL 8\n",
        "    FROM 42 TO WS-DST USING WS-IO HIGHLIGHT BELL (1).\n",
        "01 SCR-PIC-MISSING PIC .\n",
    );
    let (grp, _, sink) = parse_data_src(src)?;
    assert!(sink.error_count() >= 2);
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let scr = &p.data.screen_section;
    assert_eq!(scr.len(), 6);
    assert!(
        scr.iter()
            .any(|s| matches!(s.name, DataEntryName::Named(_)))
    );
    assert!(scr.iter().any(|s| s.name == DataEntryName::Filler));
    Ok(())
}

/// Exercises `REPORT SECTION`, `SCREEN SECTION`, and `DATA DIVISION` exits on `EOF`, `PROCEDURE`, `FACTORY`, `OBJECT`, nested programs, and `END PROGRAM`.
#[test]
fn data_division_section_and_unit_boundary_exits() -> Result<()> {
    // 1. FILE SECTION and REPORT SECTION exiting on EOF, plus CONTROL without `+` before `.` and before `PAGE`.
    let fs_eof = "PROGRAM-ID. FS-EOF.\nDATA DIVISION.\nFILE SECTION.\nFD F-EOF.\n";
    let (g0, _, _) = parse_data_src(fs_eof)?;
    assert_eq!(g0.units.len(), 1);

    let rpt_eof = concat!(
        "PROGRAM-ID. RPT-EOF.\n",
        "DATA DIVISION.\n",
        "REPORT SECTION.\n",
        "RD R1 CONTROL IS CTRL-A PAGE LIMIT 50.\n",
        "RD R2 CONTROL IS CTRL-B.\n",
    );
    let (g1, _, _) = parse_data_src(rpt_eof)?;
    assert_eq!(g1.units.len(), 1);

    // 2. SCREEN SECTION followed by REPORT SECTION followed by PROCEDURE DIVISION.
    let scr_rpt_proc = concat!(
        "PROGRAM-ID. SCR-RPT-PROC.\n",
        "DATA DIVISION.\n",
        "SCREEN SECTION.\n",
        "01 S1 LINE 1 COL 1 VALUE \"X\".\n",
        "REPORT SECTION.\n",
        "RD R-PROC.\n",
        "01 G1 TYPE DE LINE 1 COL 1 PIC X.\n",
        "PROCEDURE DIVISION.\n",
        "    STOP RUN.\n",
    );
    let (g2, _, _) = parse_data_src(scr_rpt_proc)?;
    assert_eq!(g2.units.len(), 1);

    // 3. Data-only outer program containing a nested program and `END PROGRAM` without `PROCEDURE DIVISION`.
    let nested_and_end = concat!(
        "PROGRAM-ID. OUTER-DATA.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 W-OUTER PIC 9.\n",
        "PROGRAM-ID. INNER-DATA.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 W-INNER PIC 9.\n",
        "END PROGRAM INNER-DATA.\n",
        "END PROGRAM OUTER-DATA.\n",
    );
    let (g3, _, _) = parse_data_src(nested_and_end)?;
    assert_eq!(g3.units.len(), 1);

    // 4. OO `CLASS-ID` compartments with `DATA DIVISION` directly preceding `OBJECT.` and `FACTORY.`.
    let oo_boundaries = concat!(
        "CLASS-ID. CLS-DATA.\n",
        "FACTORY.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 F-VAR PIC 9.\n",
        "OBJECT.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 O-VAR PIC 9.\n",
        "END CLASS CLS-DATA.\n",
        "PROGRAM-ID. BAD-FAC.\n",
        "DATA DIVISION.\n",
        "FACTORY.\n",
    );
    let (g4, _, _) = parse_data_src(oo_boundaries)?;
    assert_ne!(g4.units.len(), 0);
    Ok(())
}
