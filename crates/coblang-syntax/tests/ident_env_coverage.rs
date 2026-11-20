//! Exhaustive unit and branch-coverage tests for `IDENTIFICATION` and `ENVIRONMENT` division parsing.

#![deny(clippy::indexing_slicing)]

use std::path::Path;

use coblang_common::{
    ByteLength, ByteOffset, Dialect, ExpansionId, FileId, Result, SourceFormat, SourceSpan,
};
use coblang_diag::{
    DiagnosticSink, DialectConfig, RelaxedSyntaxMode, SourceMap, SymbolInterner, WarningPolicy,
};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, MarginZone, PreprocessConfig, preprocess_source_file,
};
use coblang_syntax::ast::{
    AccessMode, AlphabetKind, ArithmeticMode, AssignModifier, AssignTarget, CommonVisibility,
    CompilationGroup, CompilationUnit, ConfigurationSection, ConsoleMode, DebuggingMode,
    DecimalPointKind, DuplicatePolicy, EntryConvention, FileOrganization, InitialState,
    IntrinsicImportPolicy, LockModeSpec, MethodPropertyRole, ProgramAttributes, PrototypeKind,
    Recursivity, SameAreaKind, SelectOptionality, SpecialNamesParagraph,
};
use coblang_syntax::{Token, TokenKind, WordRole, parse_preprocessed_unit, parse_tokens};

/// Preprocesses, lexes, and parses a synthetic COBOL snippet into a [`CompilationGroup`].
///
/// # Arguments
///
/// * `src` - Synthetic COBOL source text.
/// * `dialect` - Active dialect configuration.
fn parse_snippet(
    src: &str,
    dialect: &DialectConfig,
) -> Result<(CompilationGroup, SymbolInterner, DiagnosticSink)> {
    let mem = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: dialect,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 200);
    let unit =
        preprocess_source_file("unit.cob", Path::new("."), src, &pp_cfg, &mut sm, &mut sink)?;
    let mut interner = SymbolInterner::new();
    let group = parse_preprocessed_unit(&unit, dialect, &mut interner, &mut sink)?;
    Ok((group, interner, sink))
}

/// Exercises `Default` implementations on `IDENTIFICATION` and `ENVIRONMENT` AST types.
#[test]
fn ast_defaults_coverage() {
    let def_attrs = ProgramAttributes::default();
    assert_eq!(def_attrs.common, CommonVisibility::PrivateToParent);
    assert_eq!(def_attrs.initial, InitialState::Resident);
    assert_eq!(def_attrs.recursive, Recursivity::NonRecursive);
    assert_eq!(def_attrs.prototype, PrototypeKind::Concrete);

    let def_sn = SpecialNamesParagraph::default();
    assert_eq!(def_sn.decimal_point, DecimalPointKind::Period);
    assert_eq!(def_sn.console_mode, ConsoleMode::StandardStream);

    let def_cfg = ConfigurationSection::default();
    assert_eq!(def_cfg.debugging_mode, DebuggingMode::Disabled);
}

/// Exercises direct `parse_tokens` with residual comment-paragraph tokens, separators, and trailing `Eof`.
#[test]
fn ast_defaults_and_direct_parse_tokens_entrypoint() -> Result<()> {
    let sp = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let dialect = DialectConfig::for_dialect(Dialect::Default);
    let mut interner = SymbolInterner::new();
    let id_sym = interner.intern("ID")?;
    let div_sym = interner.intern("DIVISION")?;
    let auth_sym = interner.intern("AUTHOR")?;
    let pid_sym = interner.intern("PROGRAM-ID")?;
    let toks = vec![
        Token {
            kind: TokenKind::Comma,
            span: sp,
        },
        Token {
            kind: TokenKind::Word {
                sym: id_sym,
                area: MarginZone::AreaA,
                role: WordRole::ReservedKeyword,
            },
            span: sp,
        },
        Token {
            kind: TokenKind::Word {
                sym: div_sym,
                area: MarginZone::AreaA,
                role: WordRole::ReservedKeyword,
            },
            span: sp,
        },
        Token {
            kind: TokenKind::Period,
            span: sp,
        },
        Token {
            kind: TokenKind::Word {
                sym: auth_sym,
                area: MarginZone::AreaA,
                role: WordRole::ReservedKeyword,
            },
            span: sp,
        },
        Token {
            kind: TokenKind::Period,
            span: sp,
        },
        Token {
            kind: TokenKind::Word {
                sym: pid_sym,
                area: MarginZone::AreaA,
                role: WordRole::ReservedKeyword,
            },
            span: sp,
        },
        Token {
            kind: TokenKind::Period,
            span: sp,
        },
        Token {
            kind: TokenKind::IntegerLit(interner.intern("100")?),
            span: sp,
        },
        Token {
            kind: TokenKind::Period,
            span: sp,
        },
        Token {
            kind: TokenKind::Eof,
            span: sp,
        },
    ];
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let grp = parse_tokens(&toks, sp, &dialect, &mut interner, &mut sink)?;
    assert_eq!(grp.units.len(), 1);
    Ok(())
}

/// Exercises direct `parse_tokens` slices without a trailing `TokenKind::Eof` token.
#[test]
fn direct_parse_tokens_without_trailing_eof() -> Result<()> {
    let sp = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let dialect = DialectConfig::for_dialect(Dialect::Default);
    let mut interner = SymbolInterner::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let mk_word = |sym| Token {
        kind: TokenKind::Word {
            sym,
            area: MarginZone::AreaB,
            role: WordRole::ReservedKeyword,
        },
        span: sp,
    };
    let pid_sym = interner.intern("PROGRAM-ID")?;
    let div_sym = interner.intern("DIVISION")?;
    let proc_sym = interner.intern("PROCEDURE")?;
    let iface_kw_sym = interner.intern("IF")?;
    let a_sym = interner.intern("A")?;
    let b_sym = interner.intern("B")?;
    let c_sym = interner.intern("C")?;
    let or_sym = interner.intern("OR")?;

    let no_eof_1 = vec![
        mk_word(pid_sym),
        Token {
            kind: TokenKind::Period,
            span: sp,
        },
        mk_word(a_sym),
        Token {
            kind: TokenKind::Period,
            span: sp,
        },
        Token {
            kind: TokenKind::Comma,
            span: sp,
        },
    ];
    let _ = parse_tokens(&no_eof_1, sp, &dialect, &mut interner, &mut sink)?;

    let no_eof_2 = vec![
        mk_word(proc_sym),
        mk_word(div_sym),
        Token {
            kind: TokenKind::Period,
            span: sp,
        },
        mk_word(iface_kw_sym),
        Token {
            kind: TokenKind::LParen,
            span: sp,
        },
        mk_word(a_sym),
    ];
    let _ = parse_tokens(&no_eof_2, sp, &dialect, &mut interner, &mut sink)?;

    let no_eof_3 = vec![
        mk_word(proc_sym),
        mk_word(div_sym),
        Token {
            kind: TokenKind::Period,
            span: sp,
        },
        mk_word(iface_kw_sym),
        mk_word(a_sym),
        Token {
            kind: TokenKind::Eq,
            span: sp,
        },
        mk_word(b_sym),
        mk_word(or_sym),
        mk_word(c_sym),
    ];
    let _ = parse_tokens(&no_eof_3, sp, &dialect, &mut interner, &mut sink)?;
    Ok(())
}

/// Exercises stray top-level non-unit tokens and EOF unit termination variants.
#[test]
fn stray_top_level_token_and_eof_unit_tail_variants() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Default);
    let _ = parse_snippet(")\n", &dialect)?;
    let cob23 = DialectConfig::for_dialect(Dialect::Cobol2023);
    for unit_tail in [
        "PROGRAM-ID. P-ID-NEST.\nID DIVISION.\nPROGRAM-ID. CHILD-ID.\nEND PROGRAM CHILD-ID.\nEND.",
        "FUNCTION-ID. F-LAST.\nPROCEDURE DIVISION.\nGOBACK.",
        "FUNCTION-ID. F-END-DOT.\nEND.",
        "CLASS-ID. C-LAST.\nFACTORY.\nPROCEDURE DIVISION.\nMETHOD-ID. M.\nEND METHOD M.\nEND FACTORY.",
        "CLASS-ID. C-NO-END-OBJ.\nOBJECT.\nPROCEDURE DIVISION.\nMETHOD-ID. M.",
        "CLASS-ID. C-END-DOT.\nOBJECT.\nPROCEDURE DIVISION.\nMETHOD-ID. M.\nEND.\nEND.\nEND.",
        "CLASS-ID. C-FAC-EOF.\nFACTORY",
        "INTERFACE-ID. I-EOF.\nPROCEDURE DIVISION.\nMETHOD-ID. M.\nEND METHOD.",
        "INTERFACE-ID. I-END-DOT.\nPROCEDURE DIVISION.\nEND.",
    ] {
        let _ = parse_snippet(unit_tail, &cob23)?;
    }
    Ok(())
}

/// Exercises multi-unit `PROGRAM-ID`, nested programs, and `FUNCTION-ID` compilation groups.
#[test]
fn multi_unit_program_and_function_parsing() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. OUTER-PROG AS \"outer_entry\" IS INITIAL RECURSIVE PROTOTYPE EXTRA-ATTR.\n",
        "OPTIONS.\n",
        "    ARITHMETIC IS STANDARD-DECIMAL.\n",
        "    DEFAULT ROUNDED MODE IS NEAREST-EVEN\n",
        "    ENTRY-CONVENTION IS STDCALL\n",
        "    UNKNOWN-OPT.\n",
        "PROCEDURE DIVISION.\n",
        "    STOP RUN.\n",
        "    IDENTIFICATION DIVISION.\n",
        "    PROGRAM-ID. CHILD-PROG IS COMMON PROGRAM.\n",
        "    END PROGRAM CHILD-PROG.\n",
        "    PROGRAM-ID. CHILD-TWO.\n",
        "    END PROGRAM WRONG-CHILD.\n",
        "END PROGRAM OUTER-PROG.\n",
        "\n",
        "IDENTIFICATION DIVISION.\n",
        "FUNCTION-ID. MY-FUNC AS Z\"my_func\" IS PROTOTYPE EXTRA.\n",
        "OPTIONS. ARITHMETIC IS STANDARD-BINARY ENTRY-CONVENTION IS COBOL.\n",
        "END FUNCTION WRONG-FUNC.\n",
    );
    let (grp, _interner, sink) = parse_snippet(src, &dialect)?;
    assert_eq!(grp.units.len(), 2);
    assert_eq!(sink.error_count(), 2);

    if let Some(CompilationUnit::Program(p)) = grp.units.first() {
        assert_eq!(
            p.identification.program_id.attributes.initial,
            InitialState::Initial
        );
        assert_eq!(
            p.identification.program_id.attributes.recursive,
            Recursivity::Recursive
        );
        assert_eq!(
            p.identification.program_id.attributes.prototype,
            PrototypeKind::Prototype
        );
        assert_eq!(
            p.identification.options.arithmetic,
            Some(ArithmeticMode::StandardDecimal)
        );
        assert_eq!(
            p.identification.options.entry_convention,
            Some(EntryConvention::Stdcall)
        );
        assert_eq!(p.nested_programs.len(), 2);
        assert_eq!(
            p.nested_programs
                .first()
                .map(|c| c.identification.program_id.attributes.common),
            Some(CommonVisibility::Common)
        );
    }
    Ok(())
}

/// Exercises multi-unit OO `CLASS-ID` (`FACTORY`, `OBJECT`, `PROPERTY` methods) and `INTERFACE-ID` parsing.
#[test]
fn multi_unit_class_and_interface_parsing() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "CLASS-ID. MY-CLS AS \"MyCls\" INHERITS FROM BASE-A BASE-B USING T-PARAM EXTRA.\n",
        "ENVIRONMENT DIVISION.\n",
        "FACTORY.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 FAC-COUNT PIC 9.\n",
        "PROCEDURE DIVISION.\n",
        "METHOD-ID. CREATE-INST AS \"create\" EXTRA.\n",
        "PROCEDURE DIVISION.\n",
        "    EXIT METHOD.\n",
        "END METHOD.\n",
        "END FACTORY.\n",
        "OBJECT.\n",
        "PROCEDURE DIVISION.\n",
        "METHOD-ID. GET PROPERTY PROP-V.\n",
        "END METHOD.\n",
        "METHOD-ID. SET PROPERTY PROP-V.\n",
        "END METHOD.\n",
        "END OBJECT.\n",
        "END CLASS MY-CLS.\n",
        "\n",
        "IDENTIFICATION DIVISION.\n",
        "INTERFACE-ID. MY-IFACE AS \"MyIface\" INHERITS BASE-IFACE EXTRA.\n",
        "PROCEDURE DIVISION.\n",
        "METHOD-ID. DO-OP.\n",
        "END METHOD.\n",
        "END INTERFACE MY-IFACE.\n",
    );
    let (grp, _interner, sink) = parse_snippet(src, &dialect)?;
    assert_eq!(grp.units.len(), 2);
    assert_eq!(sink.error_count(), 0);

    if let Some(CompilationUnit::Class(c)) = grp.units.first() {
        assert!(c.factory.is_some());
        assert!(c.object.is_some());
        let obj_methods = c.object.as_ref().map_or(0, |o| o.methods.len());
        assert_eq!(obj_methods, 2);
        assert_eq!(
            c.object
                .as_ref()
                .and_then(|o| o.methods.first())
                .map(|m| m.method_id.property_role),
            Some(MethodPropertyRole::GetProperty)
        );
        assert_eq!(
            c.object
                .as_ref()
                .and_then(|o| o.methods.get(1))
                .map(|m| m.method_id.property_role),
            Some(MethodPropertyRole::SetProperty)
        );
    }
    assert!(matches!(
        grp.units.get(1),
        Some(CompilationUnit::Interface(_))
    ));
    Ok(())
}

/// Exercises missing unit names, strict vs relaxed missing headers, and remaining `OPTIONS` modes.
#[test]
fn identification_missing_names_and_strict_header_diagnostics() -> Result<()> {
    let mut strict_85 = DialectConfig::for_dialect(Dialect::Cobol85);
    strict_85.relaxed_syntax = RelaxedSyntaxMode::Strict;
    let (_, _, s_strict) = parse_snippet(
        "PROCEDURE DIVISION.
STOP RUN.
",
        &strict_85,
    )?;
    assert!(s_strict.error_count() >= 2);

    let mut relaxed_gnu = DialectConfig::for_dialect(Dialect::Gnu);
    relaxed_gnu.relaxed_syntax = RelaxedSyntaxMode::Relaxed;
    let (g_rel, _, s_rel) = parse_snippet(
        "PROCEDURE DIVISION.
STOP RUN.
",
        &relaxed_gnu,
    )?;
    assert!(!s_rel.has_errors());
    assert_eq!(g_rel.units.len(), 1);

    let missing_ids_src = concat!(
        "PROGRAM-ID. + .
",
        "OPTIONS. ARITHMETIC IS STANDARD ENTRY-CONVENTION IS EXTERN.
",
        "END PROGRAM.
",
        "FUNCTION-ID. + .
",
        "OPTIONS. ARITHMETIC IS NATIVE.
",
        "END FUNCTION.
",
        "CLASS-ID. + .
",
        "OBJECT.
",
        "PROCEDURE DIVISION.
",
        "METHOD-ID. + .
",
        "END METHOD.
",
        "END OBJECT.
",
        "END CLASS.
",
        "INTERFACE-ID. + .
",
        "END INTERFACE.
",
    );
    let (g_miss, _, s_miss) = parse_snippet(missing_ids_src, &strict_85)?;
    assert_eq!(g_miss.units.len(), 4);
    assert!(s_miss.error_count() >= 5);
    Ok(())
}

/// Exercises `CONFIGURATION SECTION`, `SPECIAL-NAMES`, and `REPOSITORY` clauses and error recovery.
#[test]
fn environment_configuration_special_names_and_repository_coverage() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. ENV-CFG.\n",
        "ENVIRONMENT DIVISION.\n",
        "BOGUS-ENV-TOKEN.\n",
        "CONFIGURATION SECTION.\n",
        "SOURCE-COMPUTER. COMP-A EXTRA-TOK. WITH DEBUGGING MODE.\n",
        "OBJECT-COMPUTER. OBJ-A EXTRA-TOK. PROGRAM COLLATING SEQUENCE IS ALPHA-1 MEMORY 64.\n",
        "SPECIAL-NAMES.\n",
        "    DECIMAL-POINT IS\n",
        "    CURRENCY SIGN IS \"EUR\" WITH PICTURE SYMBOL \"E\"\n",
        "    CURRENCY SIGN IS \"$\"\n",
        "    ALPHABET ALPHA-1 IS NATIVE\n",
        "    ALPHABET ALPHA-2 IS STANDARD-1\n",
        "    ALPHABET ALPHA-2B IS ASCII\n",
        "    ALPHABET ALPHA-3 IS STANDARD-2\n",
        "    ALPHABET ALPHA-4 IS EBCDIC\n",
        "    ALPHABET ALPHA-5 IS \"A\" THRU \"Z\" 65 +\n",
        "    SYMBOLIC CHARACTERS SYM-A SYM-B ARE 65 66 IN ALPHA-1\n",
        "    CRT STATUS IS WS-CRT\n",
        "    CURSOR IS WS-CUR\n",
        "    CONSOLE IS CRT\n",
        "    SWITCH-0 IS SW0 ON STATUS IS SW0-ON OFF STATUS IS SW0-OFF\n",
        "    UPSI-0 ON STATUS IS U0-ON\n",
        "    SYSERR IS ERR-OUT\n",
        "    CLASS CLS-HEX IS \"0\" THRU \"9\" \"A\" THROUGH \"F\" IN ALPHA-2\n",
        "    123.\n",
        "REPOSITORY.\n",
        "    FUNCTION ABS COS INTRINSIC\n",
        "    FUNCTION SIN INTRINSIC\n",
        "    FUNCTION MY-UDF AS \"my_udf\"\n",
        "    CLASS CLS-A AS \"ClsA\" EXPANDS CLS-BASE\n",
        "    CLASS CLS-B\n",
        "    INTERFACE IFC-A AS \"IfcA\"\n",
        "    INTERFACE IFC-B\n",
        "    999.\n",
    );
    let (grp, _, sink) = parse_snippet(src, &dialect)?;
    assert_eq!(sink.error_count(), 1);
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let cfg = &p.environment.configuration;
    assert_eq!(cfg.debugging_mode, DebuggingMode::Enabled);
    assert!(cfg.source_computer.is_some());
    assert!(cfg.object_computer.is_some());
    assert!(cfg.program_collating_sequence.is_some());
    assert_eq!(cfg.special_names.currency_signs.len(), 2);
    assert_eq!(cfg.special_names.alphabets.len(), 6);
    assert!(matches!(
        cfg.special_names.alphabets.get(5).map(|a| &a.kind),
        Some(AlphabetKind::Custom(_))
    ));
    assert_eq!(cfg.special_names.symbolic_chars.len(), 1);
    assert_eq!(cfg.special_names.classes.len(), 1);
    assert_eq!(cfg.special_names.switches.len(), 2);
    assert_eq!(cfg.special_names.devices.len(), 1);
    assert_eq!(cfg.special_names.console_mode, ConsoleMode::ConsoleIsCrt);
    assert!(matches!(
        cfg.repository.intrinsics,
        IntrinsicImportPolicy::SpecificIntrinsics(_)
    ));
    assert_eq!(cfg.repository.functions.len(), 1);
    assert_eq!(cfg.repository.classes.len(), 2);
    assert_eq!(cfg.repository.interfaces.len(), 2);
    Ok(())
}

/// Exercises bare configuration paragraphs (without `CONFIGURATION SECTION`), `ALL INTRINSIC`, and incomplete clauses.
#[test]
fn environment_bare_configuration_and_all_intrinsic_coverage() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let src = concat!(
        "PROGRAM-ID. ENV-BARE.\n",
        "ENVIRONMENT DIVISION.\n",
        "OBJECT-COMPUTER. M1 COLLATING SEQUENCE IS A1.\n",
        "SPECIAL-NAMES. DECIMAL-POINT IS COMMA SYMBOLIC CHARACTER SC1 IS 10 CONSOLE IS STD ALPHABET .\n",
        "SPECIAL-NAMES. CLASS . CURRENCY SIGN IS . LONE-WORD .\n",
        "REPOSITORY. FUNCTION ALL INTRINSIC FUNCTION + CLASS . INTERFACE .\n",
    );
    let (grp, _, _) = parse_snippet(src, &dialect)?;
    if let Some(CompilationUnit::Program(p)) = grp.units.first() {
        assert_eq!(
            p.environment.configuration.repository.intrinsics,
            IntrinsicImportPolicy::AllIntrinsic
        );
    }
    Ok(())
}

/// Exercises `INPUT-OUTPUT SECTION` bare `SELECT` and `FILE-CONTROL` indexed file clauses.
#[test]
fn environment_input_output_select_and_io_control_coverage() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let src = concat!(
        "PROGRAM-ID. IO-IDX.\n",
        "ENVIRONMENT DIVISION.\n",
        "INPUT-OUTPUT SECTION.\n",
        "SELECT F-BARE ASSIGN TO \"bare.dat\" LINE SEQUENTIAL.\n",
        "FILE-CONTROL.\n",
        "    SELECT OPTIONAL F-IDX\n",
        "        ASSIGN TO DYNAMIC WS-P EXTERNAL DD1 DISK \"idx.dat\" DISPLAY KEYBOARD +\n",
        "        ORGANIZATION IS INDEXED\n",
        "        ACCESS MODE IS DYNAMIC\n",
        "        RECORD KEY IS RK-SPLIT = SEG-A IN REC-A SEG-B +\n",
        "        ALTERNATE RECORD KEY IS AK-1 WITH DUPLICATES\n",
        "        ALTERNATE RECORD KEY IS AK-2 DUPLICATES\n",
        "        ALTERNATE RECORD KEY IS AK-3\n",
        "        FILE STATUS IS FS-1 IN WS FS-2\n",
        "        LOCK MODE IS AUTOMATIC WITH LOCK ON MULTIPLE RECORDS\n",
        "        PADDING CHARACTER IS \" \"\n",
        "        COLLATING SEQUENCE IS ALPHA-1\n",
        "        RESERVE 2 AREAS.\n",
    );
    let (grp, _, _) = parse_snippet(src, &dialect)?;
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    let fc = &p.environment.input_output.file_control;
    assert_eq!(fc.len(), 2);
    if let Some(f_idx) = fc.get(1) {
        assert_eq!(f_idx.optionality, SelectOptionality::Optional);
        assert_eq!(f_idx.organization, FileOrganization::Indexed);
        assert_eq!(f_idx.access_mode, AccessMode::Dynamic);
        assert!(f_idx.assign_targets.iter().any(|t| matches!(
            t,
            AssignTarget::Name {
                modifier: AssignModifier::Dynamic,
                ..
            }
        )));
        assert!(f_idx.assign_targets.iter().any(|t| matches!(
            t,
            AssignTarget::Name {
                modifier: AssignModifier::External,
                ..
            }
        )));
        assert!(
            f_idx
                .assign_targets
                .iter()
                .any(|t| matches!(t, AssignTarget::Literal(_)))
        );
        assert!(
            f_idx
                .assign_targets
                .iter()
                .any(|t| matches!(t, AssignTarget::Device(_)))
        );
        assert_eq!(f_idx.alternate_keys.len(), 3);
        assert_eq!(
            f_idx.alternate_keys.first().map(|k| k.duplicates),
            Some(DuplicatePolicy::WithDuplicates)
        );
        assert_eq!(
            f_idx.alternate_keys.get(2).map(|k| k.duplicates),
            Some(DuplicatePolicy::Unique)
        );
        assert!(f_idx.secondary_status.is_some());
        assert_eq!(f_idx.lock_mode, Some(LockModeSpec::Automatic));
    }
    Ok(())
}

/// Exercises `FILE-CONTROL` relative/binary-sequential `SELECT` entries and `I-O-CONTROL` `SAME AREA` clauses.
#[test]
fn environment_file_control_relative_manual_and_io_control_coverage() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let src = concat!(
        "PROGRAM-ID. IO-REL-MAN.\n",
        "ENVIRONMENT DIVISION.\n",
        "INPUT-OUTPUT SECTION.\n",
        "FILE-CONTROL.\n",
        "    SELECT F-REL\n",
        "        ASSIGN TO WS-REL\n",
        "        ORGANIZATION IS RELATIVE\n",
        "        ACCESS MODE IS RANDOM\n",
        "        RELATIVE KEY IS REL-K OF WS\n",
        "        STATUS IS FS-3\n",
        "        LOCK MODE IS EXCLUSIVE.\n",
        "    SELECT F-MAN\n",
        "        ORGANIZATION IS RECORD BINARY SEQUENTIAL\n",
        "        ACCESS MODE IS SEQUENTIAL\n",
        "        RECORD KEY IS RK-SIMPLE\n",
        "        RECORD KEY IS\n",
        "        LOCK MODE IS MANUAL WITH ROLLBACK STATUS IS .\n",
        "    SELECT .\n",
        "I-O-CONTROL.\n",
        "    SAME AREA FOR F-BARE F-IDX.\n",
        "    SAME RECORD AREA FOR F-REL F-MAN 123.\n",
        "    SAME SORT AREA FOR S-1 S-2.\n",
        "    SAME SORT-MERGE AREA FOR S-3 S-4.\n",
        "    RERUN ON F-BARE.\n",
    );
    let (grp, _, sink) = parse_snippet(src, &dialect)?;
    assert!(sink.error_count() >= 1);
    let Some(CompilationUnit::Program(p)) = grp.units.first() else {
        return Ok(());
    };
    assert_eq!(p.environment.input_output.file_control.len(), 3);
    let sa = &p.environment.input_output.io_control.same_areas;
    assert_eq!(sa.len(), 4);
    assert_eq!(sa.first().map(|s| s.kind), Some(SameAreaKind::BufferArea));
    assert_eq!(sa.get(1).map(|s| s.kind), Some(SameAreaKind::RecordArea));
    assert_eq!(sa.get(2).map(|s| s.kind), Some(SameAreaKind::SortMergeArea));
    Ok(())
}

/// Exercises period-less division/paragraph boundaries, bare `SWITCH`/`UPSI`/device clauses, and nested programs after `ENVIRONMENT`.
#[test]
fn environment_periodless_boundaries_and_edge_cases_coverage() -> Result<()> {
    let dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let edge_cases = [
        "PROGRAM-ID. EDGE-PROG DATA DIVISION.\u{000a}END PROGRAM EDGE-PROG.\u{000a}",
        "OPTIONS. DATA ARITHMETIC IS NATIVE.\u{000a}PROGRAM-ID. OPT1.\u{000a}",
        "PROGRAM-ID. EOF-OPT.\u{000a}OPTIONS. ARITHMETIC IS NATIVE",
        "PROGRAM-ID. SC-NUM.\u{000a}ENVIRONMENT DIVISION.\u{000a}CONFIGURATION SECTION.\u{000a}SOURCE-COMPUTER. 123.\u{000a}OBJECT-COMPUTER. 123.\u{000a}FILE-CONTROL.\u{000a}SELECT F1 ASSIGN TO WS1 DATA DIVISION.\u{000a}",
        "PROGRAM-ID. SN-1.\u{000a}ENVIRONMENT DIVISION.\u{000a}SPECIAL-NAMES. CURRENCY SIGN IS .\u{000a}",
        "PROGRAM-ID. SN-2.\u{000a}ENVIRONMENT DIVISION.\u{000a}SPECIAL-NAMES. ALPHABET .\u{000a}",
        "PROGRAM-ID. SN-3.\u{000a}ENVIRONMENT DIVISION.\u{000a}SPECIAL-NAMES. SYMBOLIC CHARACTERS + .\u{000a}",
        "PROGRAM-ID. SN-4.\u{000a}ENVIRONMENT DIVISION.\u{000a}SPECIAL-NAMES. SYMBOLIC CHARACTERS S1 IS 65.\u{000a}",
        "PROGRAM-ID. SN-5.\u{000a}ENVIRONMENT DIVISION.\u{000a}SPECIAL-NAMES. CLASS C1 IS 'A'.\u{000a}",
        "PROGRAM-ID. SN-6.\u{000a}ENVIRONMENT DIVISION.\u{000a}SPECIAL-NAMES. CLASS C2 IS + .\u{000a}",
        "PROGRAM-ID. SN-7.\u{000a}ENVIRONMENT DIVISION.\u{000a}SPECIAL-NAMES. SWITCH-1 .\u{000a}",
        "PROGRAM-ID. SN-8.\u{000a}ENVIRONMENT DIVISION.\u{000a}SPECIAL-NAMES. UPSI-0 .\u{000a}",
        "PROGRAM-ID. SN-9.\u{000a}ENVIRONMENT DIVISION.\u{000a}SPECIAL-NAMES. LONE-DEV .\u{000a}",
        "PROGRAM-ID. RP-1.\u{000a}ENVIRONMENT DIVISION.\u{000a}REPOSITORY. FUNCTION ALL INTRINSIC FUNCTION SIN INTRINSIC.\u{000a}",
        "PROGRAM-ID. RP-2.\u{000a}ENVIRONMENT DIVISION.\u{000a}REPOSITORY. INTERFACE .\u{000a}",
        "PROGRAM-ID. IO-1.\u{000a}ENVIRONMENT DIVISION.\u{000a}INPUT-OUTPUT SECTION. BOGUS.\u{000a}",
        "PROGRAM-ID. IO-2.\u{000a}ENVIRONMENT DIVISION.\u{000a}FILE-CONTROL. SELECT F1 RECORD KEY IS + LOCK MODE IS MANUAL FILE STATUS IS FS1 OF + .\u{000a}",
        "PROGRAM-ID. IO-3.\u{000a}ENVIRONMENT DIVISION.\u{000a}FILE-CONTROL. SELECT F1 RECORD KEY IS K1 = S1.\u{000a}",
        "PROGRAM-ID. IO-4.\u{000a}ENVIRONMENT DIVISION.\u{000a}FILE-CONTROL. SELECT F1 FILE STATUS IS FS1 + .\u{000a}",
        "PROGRAM-ID. IO-5.\u{000a}ENVIRONMENT DIVISION.\u{000a}I-O-CONTROL. SAME AREA FOR F1. DATA .\u{000a}DATA DIVISION.\u{000a}",
        "PROGRAM-ID. IO-6.\u{000a}ENVIRONMENT DIVISION.\u{000a}I-O-CONTROL. RERUN",
        "PROGRAM-ID. NEST-ENV.\u{000a}ENVIRONMENT DIVISION.\u{000a}IDENTIFICATION DIVISION.\u{000a}PROGRAM-ID. SUB1.\u{000a}END PROGRAM SUB1.\u{000a}END PROGRAM NEST-ENV.\u{000a}",
        "PROGRAM-ID. SC-NOP.\u{000a}ENVIRONMENT DIVISION.\u{000a}SOURCE-COMPUTER. M1 OBJECT-COMPUTER. M2 DATA DIVISION.\u{000a}",
        "PROGRAM-ID. SC-EOF.\u{000a}ENVIRONMENT DIVISION.\u{000a}SOURCE-COMPUTER. M1",
        "PROGRAM-ID. SK-ACC.\u{000a}ENVIRONMENT DIVISION.\u{000a}FILE-CONTROL. SELECT F1 RECORD KEY IS K1 = S1 ACCESS RANDOM.\u{000a}",
        "CLASS-ID. C-INH-U INHERITS B1 USING P1.\u{000a}END CLASS C-INH-U.\u{000a}",
        "CLASS-ID. C-INH-END INHERITS B1.\u{000a}END CLASS C-INH-END.\u{000a}",
        "PROGRAM-ID. P-EOF-EC.\u{000a}OPTIONS. ENTRY-CONVENTION IS",
    ];
    for case_src in edge_cases {
        let _ = parse_snippet(case_src, &dialect)?;
    }
    Ok(())
}
