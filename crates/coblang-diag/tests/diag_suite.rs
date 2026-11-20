//! Exhaustive unit and branch-coverage tests for `coblang-diag`.

#![deny(clippy::indexing_slicing)]

use coblang_common::{
    ByteLength, ByteOffset, CoblangError, DiagnosticSeverity, Dialect, ExpansionId, FileId, Result,
    SourceSpan, SymbolId,
};
use coblang_diag::{
    AssignClauseDefault, BinaryByteOrder, BinarySizePolicy, Diagnostic, DiagnosticCode,
    DiagnosticSink, DialectConfig, DisplayFormatMode, ExpansionKind, FeatureDisposition,
    OdoComplexity, PerformReturnStyle, RedefinesChainPolicy, RelaxedSyntaxMode, SourceMap,
    StickyLinkageMode, SymbolInterner, TruncationMode, WarningGroup, WarningPolicy,
};

/// Exercises case-insensitive interning, original spelling lookup, capacity limits, and errors.
#[test]
fn symbol_interner_full_branch_coverage() -> Result<()> {
    let mut interner = SymbolInterner::default();
    assert!(interner.is_empty());
    assert_eq!(interner.len(), 0);
    assert!(interner.lookup("Ws-Count").is_none());

    let id1 = interner.intern("Ws-Count")?;
    let id2 = interner.intern("WS-COUNT")?;
    assert_eq!(id1, id2);
    assert!(!interner.is_empty());
    assert_eq!(interner.len(), 1);
    assert_eq!(interner.lookup("ws-count"), Some(id1));
    assert_eq!(interner.resolve_canonical(id1)?, "WS-COUNT");
    assert_eq!(interner.resolve_original(id1)?, "Ws-Count");

    let bogus = SymbolId::new(99);
    assert!(interner.resolve_canonical(bogus).is_err());
    assert!(interner.resolve_original(bogus).is_err());

    let mut tiny = SymbolInterner::with_limit(1);
    let _ = tiny.intern("FIRST")?;
    assert!(tiny.intern("SECOND").is_err());
    Ok(())
}

/// Exercises `SourceMap` line/column lookup (LF & CRLF), expansion unwinding, and table limits.
#[test]
fn source_map_and_expansion_chain_branch_coverage() -> Result<()> {
    let mut sm = SourceMap::default();
    let main_id = sm.add_file(
        String::from("main.cob"),
        String::from("000100 IDENTIFICATION DIVISION.\u{000d}\u{000a}000200 COPY SUB.\u{000a}"),
    )?;
    let cpy_id = sm.add_file(
        String::from("sub.cpy"),
        String::from("       01 BAD-ITEM PIC 9(99).\u{000a}"),
    )?;

    let main_file = sm.get_file(main_id)?;
    assert_eq!(main_file.id(), main_id);
    assert_eq!(main_file.path(), "main.cob");
    assert!(main_file.content().starts_with("000100"));
    assert_eq!(main_file.line_count(), 3);

    let (l1, c1) = main_file.resolve_line_col(ByteOffset::ZERO)?;
    assert_eq!((l1.get(), c1.get()), (1, 1));
    let (l2, c8) = main_file.resolve_line_col(ByteOffset::new(40))?;
    assert_eq!((l2.get(), c8.get()), (2, 8));
    assert!(main_file.resolve_line_col(ByteOffset::new(9999)).is_err());
    assert!(sm.get_file(FileId::new(99)).is_err());

    let call_span = SourceSpan::new(
        main_id,
        ByteOffset::new(40),
        ByteLength::new(8),
        ExpansionId::ROOT,
    );
    let exp1 = sm.record_expansion(ExpansionId::ROOT, call_span, ExpansionKind::Copybook)?;
    let exp2 = sm.record_expansion(exp1, call_span, ExpansionKind::ReplaceStatement)?;
    let exp3 = sm.record_expansion(exp2, call_span, ExpansionKind::CdfDefine)?;
    let exp4 = sm.record_expansion(exp3, call_span, ExpansionKind::CdfConditional)?;

    let inner_span = SourceSpan::new(cpy_id, ByteOffset::new(10), ByteLength::new(8), exp4);
    let chain = sm.unwind_expansion_chain(inner_span)?;
    assert_eq!(chain.len(), 4);
    for (_, kind) in &chain {
        assert_ne!(kind.diagnostic_note_text(), "");
    }

    let bad_exp_span = SourceSpan::new(
        cpy_id,
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::new(99),
    );
    assert!(sm.unwind_expansion_chain(bad_exp_span).is_err());

    let mut capped = SourceMap::with_limits(1, 5, 1);
    assert!(
        capped
            .add_file(String::from("too_big.cob"), String::from("123456"))
            .is_err()
    );
    let f0 = capped.add_file(String::from("ok.cob"), String::from("123"))?;
    assert!(
        capped
            .add_file(String::from("second.cob"), String::from("1"))
            .is_err()
    );
    let sp0 = SourceSpan::new(f0, ByteOffset::ZERO, ByteLength::ZERO, ExpansionId::ROOT);
    let _ = capped.record_expansion(ExpansionId::ROOT, sp0, ExpansionKind::Copybook)?;
    assert!(
        capped
            .record_expansion(ExpansionId::ROOT, sp0, ExpansionKind::Copybook)
            .is_err()
    );
    Ok(())
}

/// Exercises all `WarningGroup` names and `WarningPolicy` CLI transitions.
#[test]
fn warning_policy_cli_state_machine_coverage() -> Result<()> {
    for &grp in WarningGroup::ALL {
        let name = grp.flag_name();
        assert_eq!(WarningGroup::from_flag_name(name), Some(grp));
    }
    assert_eq!(
        WarningGroup::from_flag_name("truncation"),
        Some(WarningGroup::Truncate)
    );
    assert!(WarningGroup::from_flag_name("nonexistent").is_none());

    let mut pol = WarningPolicy::default();
    assert_eq!(
        pol.classify(WarningGroup::Obsolete),
        Some(DiagnosticSeverity::Warning)
    );
    assert_eq!(pol.classify(WarningGroup::Archaic), None);

    pol.apply_cli_flag("-Wall")?;
    assert_eq!(
        pol.classify(WarningGroup::Truncate),
        Some(DiagnosticSeverity::Warning)
    );

    pol.apply_cli_flag("-Wextra")?;
    assert_eq!(
        pol.classify(WarningGroup::Unused),
        Some(DiagnosticSeverity::Warning)
    );

    pol.apply_cli_flag("-Werror=unused")?;
    assert_eq!(
        pol.classify(WarningGroup::Unused),
        Some(DiagnosticSeverity::Error)
    );
    // Re-applying -Wall / -Wextra preserves explicit -Werror=unused promotion.
    pol.apply_cli_flag("-W")?;
    assert_eq!(
        pol.classify(WarningGroup::Unused),
        Some(DiagnosticSeverity::Error)
    );

    pol.apply_cli_flag("-Wno-error=unused")?;
    assert_eq!(
        pol.classify(WarningGroup::Unused),
        Some(DiagnosticSeverity::Warning)
    );
    // -Wno-error=<grp> on a non-promoted group leaves it unchanged.
    pol.apply_cli_flag("-Wno-error=unused")?;
    assert_eq!(
        pol.classify(WarningGroup::Unused),
        Some(DiagnosticSeverity::Warning)
    );

    pol.apply_cli_flag("-Werror=archaic")?;
    pol.apply_cli_flag("-Werror")?;
    assert_eq!(
        pol.classify(WarningGroup::Obsolete),
        Some(DiagnosticSeverity::Error)
    );

    pol.apply_cli_flag("-Wno-error")?;
    assert_eq!(
        pol.classify(WarningGroup::Archaic),
        Some(DiagnosticSeverity::Warning)
    );
    assert_eq!(
        pol.classify(WarningGroup::Obsolete),
        Some(DiagnosticSeverity::Warning)
    );

    pol.apply_cli_flag("-Wno-obsolete")?;
    assert_eq!(pol.classify(WarningGroup::Obsolete), None);
    pol.apply_cli_flag("-Wobsolete")?;
    assert_eq!(
        pol.classify(WarningGroup::Obsolete),
        Some(DiagnosticSeverity::Warning)
    );

    pol.apply_cli_flag("-w")?;
    assert_eq!(pol.classify(WarningGroup::Obsolete), None);
    pol.apply_cli_flag("-Wno-all")?;
    assert_eq!(pol.classify(WarningGroup::Unused), None);
    pol.apply_cli_flag("-Wfatal-errors")?;

    assert!(pol.apply_cli_flag("-Wunknown-group").is_err());
    assert!(pol.apply_cli_flag("-Wno-unknown-group").is_err());
    assert!(pol.apply_cli_flag("-Werror=unknown-group").is_err());
    assert!(pol.apply_cli_flag("-Wno-error=unknown-group").is_err());
    assert!(pol.apply_cli_flag("--not-a-warning").is_err());
    Ok(())
}

/// Exercises `GnuCOBOL`/`gcobol` warning-flag aliases and built-in `.conf` filename mappings.
#[test]
fn warning_and_builtin_conf_aliases_coverage() {
    for (alias, grp) in [
        ("unfinished", WarningGroup::DialectExtension),
        ("typing", WarningGroup::StrictTyping),
        ("segment", WarningGroup::Archaic),
        ("possible-overlap", WarningGroup::Redefinition),
        ("linkage", WarningGroup::ImplicitDefine),
        ("goto-section", WarningGroup::Unreachable),
    ] {
        assert_eq!(WarningGroup::from_flag_name(alias), Some(grp));
    }
    for (fname, expected) in [
        ("default.conf", Some(Dialect::Default)),
        ("cobol85.conf", Some(Dialect::Cobol85)),
        ("cobol2002.conf", Some(Dialect::Cobol2002)),
        ("cobol2014.conf", Some(Dialect::Cobol2014)),
        ("cobol2023.conf", Some(Dialect::Cobol2023)),
        ("gnu.conf", Some(Dialect::Gnu)),
        ("ibm.conf", Some(Dialect::Ibm)),
        ("mf.conf", Some(Dialect::MicroFocus)),
        ("bs2000.conf", Some(Dialect::Bs2000)),
        ("acu.conf", Some(Dialect::Acu)),
        ("xopen.conf", Some(Dialect::XOpen)),
        ("custom.conf", None),
    ] {
        assert_eq!(DialectConfig::builtin_from_conf_filename(fname), expected);
    }
}

/// Exercises `DiagnosticCode`, `DiagnosticSink`, error caps, notes, and GCC/`cobc` rendering.
#[test]
fn diagnostic_sink_and_emitter_branch_coverage() -> Result<()> {
    let all_codes = [
        DiagnosticCode::E0001Preprocessor,
        DiagnosticCode::E0002Lexical,
        DiagnosticCode::E0003Syntax,
        DiagnosticCode::E0004UndefinedSymbol,
        DiagnosticCode::E0005AmbiguousSymbol,
        DiagnosticCode::E0006InvalidPicture,
        DiagnosticCode::E0007InvalidLayout,
        DiagnosticCode::E0008TypeMismatch,
        DiagnosticCode::E0009DialectUnconformable,
        DiagnosticCode::W0001Archaic,
        DiagnosticCode::W0002Obsolete,
        DiagnosticCode::W0003Redefinition,
        DiagnosticCode::W0004Truncate,
        DiagnosticCode::W0005SourceLayout,
        DiagnosticCode::W0006GeneralWarning,
    ];
    for c in all_codes {
        assert_eq!(c.as_str().len(), 5);
    }

    let mut sm = SourceMap::new();
    let f_main = sm.add_file(String::from("prog.cob"), String::from("COPY C.\u{000a}"))?;
    let f_cpy = sm.add_file(String::from("c.cpy"), String::from("BAD.\u{000a}"))?;
    let copy_site = SourceSpan::new(
        f_main,
        ByteOffset::ZERO,
        ByteLength::new(7),
        ExpansionId::ROOT,
    );
    let exp = sm.record_expansion(ExpansionId::ROOT, copy_site, ExpansionKind::Copybook)?;
    let cpy_span = SourceSpan::new(f_cpy, ByteOffset::ZERO, ByteLength::new(3), exp);

    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 1);
    assert!(!sink.has_errors());

    // Suppressed warning (Archaic is off by default) is dropped.
    sink.emit(Diagnostic::warning(
        DiagnosticCode::W0001Archaic,
        WarningGroup::Archaic,
        Some(copy_site),
        String::from("archaic construct"),
    ));
    assert_eq!(sink.warning_count(), 0);

    // Enabled warning (Obsolete is on by default) is retained.
    sink.emit(
        Diagnostic::warning(
            DiagnosticCode::W0002Obsolete,
            WarningGroup::Obsolete,
            None,
            String::from("AUTHOR is obsolete"),
        )
        .with_note(None, String::from("remove AUTHOR paragraph")),
    );
    assert_eq!(sink.warning_count(), 1);

    // Direct Note-severity diagnostic is also supported.
    sink.emit(Diagnostic {
        severity: DiagnosticSeverity::Note,
        code: DiagnosticCode::W0006GeneralWarning,
        primary_span: None,
        message: String::from("standalone note"),
        notes: Vec::new(),
        warning_group: None,
    });

    // First hard error is recorded; second hard error hits max_errors=1 cap.
    sink.emit(
        Diagnostic::error(
            DiagnosticCode::E0003Syntax,
            Some(cpy_span),
            String::from("unexpected token BAD"),
        )
        .with_note(Some(copy_site), String::from("while parsing copybook")),
    );
    sink.emit(Diagnostic::error(
        DiagnosticCode::E0003Syntax,
        None,
        String::from("dropped error"),
    ));
    assert!(sink.has_errors());
    assert_eq!(sink.error_count(), 1);
    assert_eq!(sink.diagnostics().len(), 3);

    let rendered = sink.render_all(&sm)?;
    assert!(rendered.contains("coblang: warning: AUTHOR is obsolete"));
    assert!(rendered.contains("c.cpy:1:1: error: unexpected token BAD"));
    assert!(rendered.contains("prog.cob:1:1: note: in COPY included from here"));
    assert!(rendered.contains("prog.cob:1:1: note: while parsing copybook"));
    Ok(())
}

/// Exercises span-resolution error propagation paths in `DiagnosticSink::render_all`.
#[test]
fn diagnostic_sink_span_error_propagation_coverage() -> Result<()> {
    let mut sm = SourceMap::new();
    let f_main = sm.add_file(String::from("prog.cob"), String::from("COPY C.\u{000a}"))?;

    // Propagate span error on invalid primary/expansion/note spans.
    let bad_file_span = SourceSpan::new(
        FileId::new(88),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let mut bad_sink = DiagnosticSink::new(WarningPolicy::new(), 5);
    bad_sink.emit(Diagnostic::error(
        DiagnosticCode::E0001Preprocessor,
        Some(bad_file_span),
        String::from("e"),
    ));
    assert!(bad_sink.render_all(&sm).is_err());

    let bad_exp_span = SourceSpan::new(
        f_main,
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::new(77),
    );
    let mut bad_exp_sink = DiagnosticSink::new(WarningPolicy::new(), 5);
    bad_exp_sink.emit(Diagnostic::error(
        DiagnosticCode::E0001Preprocessor,
        Some(bad_exp_span),
        String::from("e"),
    ));
    assert!(bad_exp_sink.render_all(&sm).is_err());

    let mut bad_note_sink = DiagnosticSink::new(WarningPolicy::new(), 5);
    bad_note_sink.emit(
        Diagnostic::error(DiagnosticCode::E0001Preprocessor, None, String::from("e"))
            .with_note(Some(bad_file_span), String::from("bad note")),
    );
    assert!(bad_note_sink.render_all(&sm).is_err());
    Ok(())
}

/// Exercises all 11 `Dialect` presets and `.conf` key/value/include parsing & error branches.
#[test]
fn dialect_config_presets_and_conf_parser_coverage() -> Result<()> {
    for d in [
        Dialect::Default,
        Dialect::Cobol85,
        Dialect::Cobol2002,
        Dialect::Cobol2014,
        Dialect::Cobol2023,
        Dialect::Gnu,
        Dialect::Ibm,
        Dialect::MicroFocus,
        Dialect::Bs2000,
        Dialect::Acu,
        Dialect::XOpen,
    ] {
        let cfg = DialectConfig::for_dialect(d);
        assert_eq!(cfg.base_dialect, d);
        assert!(cfg.reserved_words.contains("MOVE"));
    }

    let mut cfg = DialectConfig::for_dialect(Dialect::Cobol85);
    let conf_doc = r#"
# Top comment
include "base.conf"
includeif "missing_opt.conf"
includeif	'extra.conf'
include if-exists "missing_opt.conf"
include	'extra.conf'
name: "custom-iso#profile" # inline comment after quotes
tab-width: 4
text-column: 80
word-length: 61
pic-length: 38
reserved: MY-VERB*
reserved: ALIAS=MOVE
reserved: *
not-reserved: ALTER
binary-byteorder: native
binary-byteorder: big-endian
binary-size: 1-2-4-8
binary-size: 2-4-8
binary-size: 1--8
binary-truncate: no
binary-truncate: yes
pretty-display: no
pretty-display: yes
complex-odo: yes
complex-odo: no
indirect-redefines: yes
indirect-redefines: no
larger-redefines: warning
relax-syntax-checks: yes
relax-syntax-checks: no
sticky-linkage: yes
sticky-linkage: no
perform-osvs: yes
perform-osvs: no
assign-clause: external
assign-clause: mf
assign-clause: dynamic
author-paragraph: obsolete
memory-size-clause: archaic
goto-statement-without-name: unconformable
debugging-line: skip
eject-statement: ok
stop-literal-statement: error
"#;
    cfg.load_conf_text(conf_doc, "main.conf", &mut |inc| match inc {
        "base.conf" => Ok(String::from("numeric-literal-length: 36\u{000a}")),
        "extra.conf" => Ok(String::from("# empty include\u{000a}")),
        other => Err(CoblangError::ConfigLoad {
            path: String::from(other),
            line: 1,
            message: String::from("missing include"),
        }),
    })?;

    assert_eq!(cfg.profile_name, "custom-iso#profile");
    assert_eq!(cfg.tab_width, 4);
    assert_eq!(cfg.text_column, 80);
    assert_eq!(cfg.max_word_length, 61);
    assert_eq!(cfg.max_pic_digits, 38);
    assert!(cfg.reserved_words.contains("MY-VERB"));
    assert!(cfg.reserved_words.contains("ALIAS"));
    assert!(!cfg.reserved_words.contains("ALTER"));
    assert_eq!(cfg.binary_byteorder, BinaryByteOrder::BigEndian);
    assert_eq!(cfg.binary_size, BinarySizePolicy::OneToEight);
    assert_eq!(cfg.binary_truncate, TruncationMode::TruncateToPic);
    assert_eq!(cfg.pretty_display, DisplayFormatMode::GnuPretty);
    assert_eq!(cfg.complex_odo, OdoComplexity::SimpleIsoOnly);
    assert_eq!(cfg.indirect_redefines, RedefinesChainPolicy::DirectOnly);
    assert_eq!(cfg.larger_redefines, FeatureDisposition::Warning);
    assert_eq!(cfg.relaxed_syntax, RelaxedSyntaxMode::Strict);
    assert_eq!(cfg.sticky_linkage, StickyLinkageMode::Disabled);
    assert_eq!(cfg.perform_style, PerformReturnStyle::IsoStack);
    assert_eq!(cfg.assign_clause, AssignClauseDefault::Dynamic);
    assert_eq!(
        cfg.feature_rules.get("author-paragraph"),
        Some(&FeatureDisposition::Obsolete)
    );
    Ok(())
}

/// Exercises all error branches in `DialectConfig::load_conf_text` and `apply_conf_key_value`.
#[test]
fn dialect_config_error_branches_coverage() {
    let mut cfg = DialectConfig::for_dialect(Dialect::Default);

    // Cycle detection & empty include & missinginclude & malformed line.
    assert!(
        cfg.load_conf_text(r#"include "self.conf""#, "self.conf", &mut |_| {
            Ok(String::from(r#"include "self.conf""#))
        })
        .is_err()
    );
    assert!(
        cfg.load_conf_text(r#"include """#, "bad.conf", &mut |_| Ok(String::new()))
            .is_err()
    );
    assert!(
        cfg.load_conf_text(r#"include "missing.conf""#, "bad.conf", &mut |_| {
            Err(CoblangError::ConfigLoad {
                path: String::from("missing.conf"),
                line: 1,
                message: String::from("not found"),
            })
        })
        .is_err()
    );
    assert!(
        cfg.load_conf_text("not_a_key_value_line", "bad.conf", &mut |_| {
            Ok(String::new())
        })
        .is_err()
    );
    assert!(
        cfg.load_conf_text("tab-width: 0", "bad_val.conf", &mut |_| Ok(String::new()))
            .is_err()
    );
    assert!(
        cfg.load_conf_text(r#"include "child_err.conf""#, "p.conf", &mut |_| {
            Ok(String::from("tab-width: 0"))
        })
        .is_err()
    );

    let invalid_pairs = [
        ("tab-width", "0"),
        ("tab-width", "abc"),
        ("text-column", "8"),
        ("text-column", "abc"),
        ("word-length", "0"),
        ("word-length", "abc"),
        ("pic-length", "0"),
        ("pic-length", "abc"),
        ("binary-byteorder", "middle-endian"),
        ("binary-size", "3-6-9"),
        ("binary-truncate", "maybe"),
        ("pretty-display", "maybe"),
        ("display-sign", "maybe"),
        ("complex-odo", "maybe"),
        ("indirect-redefines", "maybe"),
        ("larger-redefines", "maybe"),
        ("relax-syntax-checks", "maybe"),
        ("sticky-linkage", "maybe"),
        ("perform-osvs", "maybe"),
        ("assign-clause", "weird"),
        ("some-feature", "not-a-disposition"),
    ];
    for (k, v) in invalid_pairs {
        assert!(cfg.apply_conf_key_value(k, v, "<test>", 1).is_err());
    }
    assert!(
        cfg.apply_conf_key_value("sign-ebcdic", "yes", "<test>", 1)
            .is_ok()
    );
    assert!(
        cfg.apply_conf_key_value("display-sign", "ascii", "<test>", 1)
            .is_ok()
    );

    let large_sink = DiagnosticSink::new(WarningPolicy::new(), 100);
    assert!(!large_sink.has_errors());

    let mut capped_sink = DiagnosticSink::with_limits(WarningPolicy::new(), 10, 1);
    capped_sink.emit(Diagnostic::error(
        DiagnosticCode::E0001Preprocessor,
        None,
        String::from("n1"),
    ));
    capped_sink.emit(Diagnostic::error(
        DiagnosticCode::E0001Preprocessor,
        None,
        String::from("n2"),
    ));
    assert_eq!(capped_sink.diagnostics().len(), 1);
}

/// Exercises M9 `InitialBoundsChecks::apply_ec_cli_flag`, `odoslide`, and `default-colseq` config rules.
#[test]
fn m9_bounds_odoslide_and_colseq_conf_coverage() -> Result<()> {
    use coblang_diag::{BoundCheckToggle, DefaultColSeqMode, InitialBoundsChecks, OdoSlidePolicy};

    let mut bc = InitialBoundsChecks::default();
    bc.apply_ec_cli_flag(BoundCheckToggle::EnabledFatal, "bound-subscript");
    assert_eq!(bc.subscript, BoundCheckToggle::EnabledFatal);
    bc.apply_ec_cli_flag(BoundCheckToggle::EnabledFatal, "bound-odo");
    assert_eq!(bc.odo, BoundCheckToggle::EnabledFatal);
    bc.apply_ec_cli_flag(BoundCheckToggle::EnabledFatal, "bound-ref-mod");
    assert_eq!(bc.refmod, BoundCheckToggle::EnabledFatal);
    bc.apply_ec_cli_flag(BoundCheckToggle::Disabled, "all");
    assert_eq!(bc.subscript, BoundCheckToggle::Disabled);
    assert_eq!(bc.odo, BoundCheckToggle::Disabled);
    assert_eq!(bc.refmod, BoundCheckToggle::Disabled);
    bc.apply_ec_cli_flag(BoundCheckToggle::EnabledFatal, "ec-unknown-xyz");
    assert_eq!(bc.subscript, BoundCheckToggle::Disabled);

    let mut cfg = DialectConfig::for_dialect(Dialect::Gnu);
    cfg.apply_conf_key_value("odoslide", "yes", "<test>", 1)?;
    assert_eq!(cfg.odo_slide, OdoSlidePolicy::SlideVariablyLocated);
    cfg.apply_conf_key_value("odoslide", "no", "<test>", 1)?;
    assert_eq!(cfg.odo_slide, OdoSlidePolicy::FixedMaxOffsets);
    assert!(
        cfg.apply_conf_key_value("odoslide", "maybe", "<test>", 1)
            .is_err()
    );

    cfg.apply_conf_key_value("default-colseq", "ebcdic", "<test>", 1)?;
    assert_eq!(cfg.default_colseq, DefaultColSeqMode::Ebcdic);
    cfg.apply_conf_key_value("default-colseq", "ascii", "<test>", 1)?;
    assert_eq!(cfg.default_colseq, DefaultColSeqMode::AsciiNative);
    assert!(
        cfg.apply_conf_key_value("default-colseq", "bogus", "<test>", 1)
            .is_err()
    );

    cfg.apply_conf_key_value("defaultbyte", "none", "<test>", 1)?;
    assert_eq!(cfg.default_byte, None);
    cfg.apply_conf_key_value("defaultbyte", "0x41", "<test>", 1)?;
    assert_eq!(cfg.default_byte, Some(0x41));
    cfg.apply_conf_key_value("defaultbyte", "66", "<test>", 1)?;
    assert_eq!(cfg.default_byte, Some(66));
    cfg.apply_conf_key_value("defaultbyte", "\"C\"", "<test>", 1)?;
    assert_eq!(cfg.default_byte, Some(b'C'));
    cfg.apply_conf_key_value("defaultbyte", "'D'", "<test>", 1)?;
    assert_eq!(cfg.default_byte, Some(b'D'));
    for invalid_db in ["0xZZ", "\"AB\"", "unquoted_word"] {
        assert!(
            cfg.apply_conf_key_value("defaultbyte", invalid_db, "<test>", 1)
                .is_err()
        );
    }
    Ok(())
}
