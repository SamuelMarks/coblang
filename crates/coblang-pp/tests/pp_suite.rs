//! Exhaustive unit and branch-coverage test suite for `coblang-pp`.

#![deny(clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::path::Path;

use coblang_common::{Dialect, ExpansionId, Result, SourceFormat};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap, WarningPolicy};
use coblang_pp::{
    CdfEnv, CdfValue, CopybookResolver, DebugLinePolicy, FilesystemCopybookResolver,
    HeaderVisibility, InMemoryCopybookResolver, LineIndicator, ListingOptions, ListingSymbols,
    ListingWidth, MarginZone, MessageVisibility, PpToken, PpTokenKind, PreprocessConfig,
    PreprocessLimits, SpacingHint, TurnCheckingState, apply_replace_statements,
    apply_replace_statements_with_limit, apply_replacement_rules, emit_preprocessed_source,
    normalize_file, parse_replacing_clauses, preprocess_source_file,
    preprocess_source_file_with_limits, render_compiler_listing, tokenize_normalized_lines,
};

/// Exercises Fixed, Free, and Auto reference formats, tabs, debug lines, and continuations.
#[test]
fn format_normalizer_fixed_free_auto_and_continuations() -> Result<()> {
    let cfg = DialectConfig::for_dialect(Dialect::Default);
    let mut pol = WarningPolicy::new();
    pol.apply_cli_flag("-Wextra")?;

    let fixed_src = concat!(
        "000100 IDENTIFICATION DIVISION.                                         PROGID\u{000d}\u{000a}",
        "ABC100* Comment asterisk\u{000a}",
        "000200/ Comment slash\u{000a}",
        "000300$SET NOOP\u{000a}",
        r#"000400D      DISPLAY "DEBUG"."#,
        "\u{000a}",
        "000\u{000a}",
        "000500!      BAD-IND.\u{000a}",
        r#"000600       MOVE "HELLO"#,
        "\u{000a}",
        r#"000700-           "WORLD" TO WS-A *> inline comment"#,
        "\u{000a}",
        "000800       DIS\u{000a}",
        "000900-          PLAY\u{0009}WS-A.\u{000a}",
    );
    let mut sm = SourceMap::new();
    let fid = sm.add_file(String::from("fixed.cob"), String::from(fixed_src))?;
    let file = sm.get_file(fid)?;

    let mut sink_strip = DiagnosticSink::new(pol.clone(), 50);
    let lines_strip = normalize_file(
        file,
        SourceFormat::Fixed,
        &cfg,
        DebugLinePolicy::StripDebugLines,
        ExpansionId::ROOT,
        &mut sink_strip,
    );
    assert!(sink_strip.warning_count() >= 1);
    assert_eq!(sink_strip.error_count(), 1);
    assert_eq!(lines_strip.first().map(|l| l.zone), Some(MarginZone::AreaA));
    assert!(
        lines_strip
            .iter()
            .any(|l| l.code.contains("DISPLAY") && l.code.contains("WS-A"))
    );
    assert!(lines_strip.iter().any(|l| l.code.contains("WORLD")));

    let mut sink_dbg = DiagnosticSink::new(pol.clone(), 50);
    let lines_dbg = normalize_file(
        file,
        SourceFormat::Auto,
        &cfg,
        DebugLinePolicy::CompileDebugLines,
        ExpansionId::ROOT,
        &mut sink_dbg,
    );
    assert!(
        lines_dbg
            .iter()
            .any(|l| l.indicator == LineIndicator::DebugD && l.code.contains("DEBUG"))
    );

    let bad_cont_src = concat!(
        "000100-      ORPHAN.\u{000a}",
        "000200       MOVE 'UNCLOSED\u{000a}",
        "000300-      NOQUOTE' TO X.\u{000a}",
    );
    let fid_bad = sm.add_file(String::from("badc.cob"), String::from(bad_cont_src))?;
    let mut sink_bad = DiagnosticSink::new(pol, 50);
    let _ = normalize_file(
        sm.get_file(fid_bad)?,
        SourceFormat::Fixed,
        &cfg,
        DebugLinePolicy::StripDebugLines,
        ExpansionId::ROOT,
        &mut sink_bad,
    );
    assert_eq!(sink_bad.error_count(), 2);
    Ok(())
}

/// Exercises Free format, Auto format heuristics, and inline `>>SOURCE` / `$SET SOURCEFORMAT` switches.
#[test]
fn format_free_and_inline_directive_switching_coverage() -> Result<()> {
    let cfg = DialectConfig::for_dialect(Dialect::Default);
    let mut sm = SourceMap::new();

    for auto_free_sample in [
        "*> leading free comment\u{000a}PROGRAM-ID. A.\u{000a}",
        ">>DEFINE X AS 1\u{000a}IDENTIFICATION DIVISION.\u{000a}",
        "IDENTIFICATION DIVISION.\u{000a}",
        "ID DIVISION.\u{000a}",
        "PROGRAM-ID. P.\u{000a}",
        "FUNCTION-ID. F.\u{000a}",
        "DATA DIVISION.\u{000a}",
        "PROCEDURE DIVISION.\u{000a}",
        "\u{000a}",
    ] {
        let id = sm.add_file(String::from("auto.cob"), String::from(auto_free_sample))?;
        let mut sink = DiagnosticSink::new(WarningPolicy::new(), 10);
        let _ = normalize_file(
            sm.get_file(id)?,
            SourceFormat::Auto,
            &cfg,
            DebugLinePolicy::StripDebugLines,
            ExpansionId::ROOT,
            &mut sink,
        );
    }

    let switch_src = concat!(
        r#"000100$SET SOURCEFORMAT"FREE""#,
        "\u{000a}",
        "*> full free comment\u{000a}",
        r#"MOVE "PART1"-"#,
        "\u{000a}",
        r#"  "PART2" TO A"#,
        "\u{000a}",
        "MOVE 'P3'-\u{000a}",
        "  'P4' TO B\u{000a}",
        "COMPUTE C = 10 - 3\u{000a}",
        ">>SOURCE FORMAT IS FIXED\u{000a}",
        r#"000200$SET SOURCEFORMAT"FIXED""#,
        "\u{000a}",
        "000300       >>SOURCE FORMAT IS FREE\u{000a}",
        "STOP RUN.\u{000a}",
    );
    let fid = sm.add_file(String::from("sw.cob"), String::from(switch_src))?;
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 10);
    let lines = normalize_file(
        sm.get_file(fid)?,
        SourceFormat::Fixed,
        &cfg,
        DebugLinePolicy::StripDebugLines,
        ExpansionId::ROOT,
        &mut sink,
    );
    assert!(lines.iter().any(|l| l.code.contains("PART1PART2")));
    assert!(lines.iter().any(|l| l.code.contains("P3P4")));
    Ok(())
}

/// Exercises `pseudo_text.rs` tokenization, literal prefixes, quotes, and `REPLACING` rules.
#[test]
fn pseudo_text_tokenizer_and_replacing_engine_coverage() -> Result<()> {
    let cfg = DialectConfig::for_dialect(Dialect::Default);
    let mut sm = SourceMap::new();
    let src = concat!(
        r#"MOVE X"41" N'AB' NX"0041" "A""B" 'C''D' TO ARR(1:2), SEMI;"#,
        "\u{000a}",
        "COMPUTE V = 3.14.\u{000a}",
        r#"DISPLAY "UNTERMINATED"#,
        "\u{000a}",
    );
    let fid = sm.add_file(String::from("tok.cob"), String::from(src))?;
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 20);
    let norm = normalize_file(
        sm.get_file(fid)?,
        SourceFormat::Free,
        &cfg,
        DebugLinePolicy::StripDebugLines,
        ExpansionId::ROOT,
        &mut sink,
    );
    let toks = tokenize_normalized_lines(&norm, &mut sink);
    assert_eq!(sink.error_count(), 1);
    assert!(
        toks.iter()
            .any(|t| t.kind == PpTokenKind::LParen && t.spacing == SpacingHint::Tight)
    );
    assert!(toks.iter().any(|t| t.kind == PpTokenKind::Colon));
    assert!(toks.iter().any(|t| t.kind == PpTokenKind::RParen));
    assert!(toks.iter().any(|t| t.kind == PpTokenKind::SeparatorPunct));
    assert!(toks.iter().any(|t| t.text == "3.14"));

    let clause_src = concat!(
        "==TEST-VAR== BY ==FIRST-VAR== ",
        "==TEST-VAR== BY ==SECOND-VAR== ",
        "LEADING ==OLD-== BY ==NEW-== ",
        "TRAILING ==-IN== BY ==-OUT== ",
        "A OF REC BY B OF REC ",
        "==A, B== BY ==X Y==\u{000a}",
    );
    let cfid = sm.add_file(String::from("cl.cob"), String::from(clause_src))?;
    let mut csink = DiagnosticSink::new(WarningPolicy::new(), 20);
    let cnorm = normalize_file(
        sm.get_file(cfid)?,
        SourceFormat::Free,
        &cfg,
        DebugLinePolicy::StripDebugLines,
        ExpansionId::ROOT,
        &mut csink,
    );
    let ctoks = tokenize_normalized_lines(&cnorm, &mut csink);
    let base_span =
        ctoks
            .first()
            .map(|t| t.span)
            .ok_or_else(|| coblang_common::CoblangError::InvalidCli {
                message: String::from("empty"),
            })?;
    let rules = parse_replacing_clauses(&ctoks, base_span, &mut csink);
    assert_eq!(csink.error_count(), 0);
    assert_eq!(rules.len(), 6);
    assert_eq!(apply_replacement_rules(&ctoks, &[]).len(), ctoks.len());

    let body_src = r#"MOVE TEST-VAR, OLD-ITEM, DATA-IN, A OF REC, A, B TO "LIT"."#;
    let bfid = sm.add_file(String::from("b.cob"), format!("{body_src}\u{000a}"))?;
    let bnorm = normalize_file(
        sm.get_file(bfid)?,
        SourceFormat::Free,
        &cfg,
        DebugLinePolicy::StripDebugLines,
        ExpansionId::ROOT,
        &mut csink,
    );
    let btoks = tokenize_normalized_lines(&bnorm, &mut csink);
    let out_toks = apply_replacement_rules(&btoks, &rules);
    let texts: Vec<&str> = out_toks.iter().map(|t| t.text.as_str()).collect();
    assert!(texts.contains(&"FIRST-VAR"));
    assert!(!texts.contains(&"SECOND-VAR"));
    assert!(texts.contains(&"NEW-ITEM"));
    assert!(texts.contains(&"DATA-OUT"));
    Ok(())
}

/// Exercises error paths in `parse_replacing_clauses`.
#[test]
fn pseudo_text_replacing_error_branches() -> Result<()> {
    let cfg = DialectConfig::for_dialect(Dialect::Default);
    let mut sm = SourceMap::new();
    for bad_clause in [
        "==UNCLOSED",
        "FOO",
        "FOO BY",
        "FOO NOTBY BAR",
        "FOO BY ==UNCLOSED",
        "==== BY ==X==",
        "A IN",
    ] {
        let efid = sm.add_file(String::from("e.cob"), format!("{bad_clause}\u{000a}"))?;
        let mut esink = DiagnosticSink::new(WarningPolicy::new(), 20);
        let enorm = normalize_file(
            sm.get_file(efid)?,
            SourceFormat::Free,
            &cfg,
            DebugLinePolicy::StripDebugLines,
            ExpansionId::ROOT,
            &mut esink,
        );
        let etoks = tokenize_normalized_lines(&enorm, &mut esink);
        let sp = etoks.first().map(|t| t.span).ok_or_else(|| {
            coblang_common::CoblangError::InvalidCli {
                message: String::from("empty"),
            }
        })?;
        let _ = parse_replacing_clauses(&etoks, sp, &mut esink);
        assert!(esink.error_count() >= 1);
    }
    Ok(())
}

/// Exercises `COPY` resolution (`FilesystemCopybookResolver` + `InMemoryCopybookResolver`), nesting, cycles, and `REPLACE`.
#[test]
fn copy_and_replace_engine_full_branch_coverage() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-pp-cpy-{}", std::process::id()));
    let sub_dir = tmp.join("MYLIB");
    let inc_dir = tmp.join("inc");
    let env_dir = tmp.join("envc");
    std::fs::create_dir_all(&sub_dir)?;
    std::fs::create_dir_all(&inc_dir)?;
    std::fs::create_dir_all(&env_dir)?;
    std::fs::write(sub_dir.join("subbook.inc"), "01 LIB-VAR PIC X.\u{000a}")?;
    std::fs::write(inc_dir.join("plain.custom"), "01 EXT-VAR PIC 9.\u{000a}")?;
    std::fs::write(env_dir.join("fromenv.cpy"), "01 ENV-VAR PIC 9.\u{000a}")?;

    let mut lib_map = BTreeMap::new();
    lib_map.insert(String::from("MAPPED"), sub_dir.clone());
    let fs_res = FilesystemCopybookResolver {
        include_dirs: vec![inc_dir],
        extra_extensions: vec![String::from("custom"), String::from(".cpy")],
        cob_copy_dir: Some(env_dir.to_string_lossy().to_string()),
        library_dirs: lib_map,
    };
    assert!(
        fs_res
            .resolve_copybook("subbook", Some("MYLIB"), &tmp)
            .is_ok()
    );
    assert!(
        fs_res
            .resolve_copybook("subbook", Some("MAPPED"), &tmp)
            .is_ok()
    );
    assert!(fs_res.resolve_copybook("plain", None, &tmp).is_ok());
    assert!(fs_res.resolve_copybook("fromenv", None, &tmp).is_ok());
    assert!(fs_res.resolve_copybook("nope", None, &tmp).is_err());
    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Exercises recursive `COPY`, `SUPPRESS`, `REPLACE` / `REPLACE ALSO` / `REPLACE LAST OFF`, and error paths.
#[test]
fn copy_and_replace_in_memory_coverage() -> Result<()> {
    let mut mem = InMemoryCopybookResolver::new();
    mem.insert(
        None,
        "OUTER",
        "outer.cpy",
        "COPY INNER SUPPRESS PRINTING.\u{000a}",
    );
    mem.insert(
        Some("LIB1"),
        "INNER_LIB",
        "inner_lib.cpy",
        "01 L PIC X.\u{000a}",
    );
    mem.insert(None, "INNER", "inner.cpy", "01 ORIG-NAME PIC X.\u{000a}");
    mem.insert(None, "SELF", "self.cpy", "COPY SELF.\u{000a}");

    let dialect_cfg = DialectConfig::for_dialect(Dialect::Gnu);
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dialect_cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };

    let main_src = concat!(
        "COPY OUTER REPLACING ==ORIG-NAME== BY ==STEP-ONE==.\u{000a}",
        "COPY INNER_LIB IN LIB1 SUPPRESS.\u{000a}",
        "REPLACE ==STEP-ONE== BY ==STEP-TWO==.\u{000a}",
        "MOVE STEP-ONE TO X.\u{000a}",
        "REPLACE ALSO ==STEP-TWO== BY ==FINAL-NAME==.\u{000a}",
        "MOVE STEP-ONE TO Y.\u{000a}",
        "REPLACE LAST OFF.\u{000a}",
        "MOVE STEP-ONE TO Z.\u{000a}",
        "REPLACE OFF.\u{000a}",
    );
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let unit = preprocess_source_file(
        "main.cob",
        Path::new("."),
        main_src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    assert!(!sink.has_errors());
    let emitted = emit_preprocessed_source("main.cob", &unit.tokens);
    assert!(emitted.contains("FINAL-NAME"));
    assert!(emitted.contains("STEP-TWO"));

    for err_prog in [
        "COPY.\u{000a}",
        "COPY A IN.\u{000a}",
        "COPY INNER BOGUS.\u{000a}",
        "COPY MISSING_BOOK.\u{000a}",
        "COPY SELF.\u{000a}",
        "COPY UNTERMINATED\u{000a}",
        "REPLACE ==A== BY ==B==\u{000a}",
    ] {
        let mut esm = SourceMap::new();
        let mut esink = DiagnosticSink::new(WarningPolicy::new(), 50);
        let _ = preprocess_source_file(
            "e.cob",
            Path::new("."),
            err_prog,
            &pp_cfg,
            &mut esm,
            &mut esink,
        )?;
        assert!(esink.has_errors());
    }
    let mut empty_sm = SourceMap::new();
    let mut empty_sink = DiagnosticSink::new(WarningPolicy::new(), 10);
    assert_eq!(
        apply_replace_statements(&[], &mut empty_sm, &mut empty_sink)?.len(),
        0
    );
    Ok(())
}

/// Exercises ISO CDF directives (`>>DEFINE`, `>>IF`/`>>ELIF`/`>>ELSE`, `>>EVALUATE`, `>>TURN`, `>>PUSH`/`>>POP`).
#[test]
fn cdf_directives_and_listing_generator_coverage() -> Result<()> {
    let mem = InMemoryCopybookResolver::new();
    let dialect_cfg = DialectConfig::for_dialect(Dialect::Cobol2023);
    let cli_defs = vec![(String::from("CLI_PARAM"), CdfValue::Integer(42))];
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dialect_cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &cli_defs,
        copybook_resolver: &mem,
    };

    let cdf_prog = concat!(
        "*CBL NOOPT\u{000a}*CONTROL SOURCE\u{000a}",
        ">>DEFINE CONSTANT CLI_PARAM AS PARAMETER\u{000a}>>DEFINE UNSET_PARAM AS PARAMETER\u{000a}",
        ">>DEFINE FLAG_A\u{000a}>>DEFINE STR_V AS \u{0022}HELLO\u{0022}\u{000a}",
        ">>DEFINE NUM_V AS 10\u{000a}>>PUSH\u{000a}>>DEFINE NUM_V AS 99 OVERRIDE\u{000a}>>POP\u{000a}",
        ">>DEFINE FLAG_OFF AS OFF\u{000a}>>DISPLAY \u{0022}compile message\u{0022}\u{000a}",
        ">>TURN EC-SIZE-ZERO-DIVIDE CHECKING ON WITH LOCATION\u{000a}",
        ">>TURN EC-BOUND-SUBSCRIPT CHECKING ON\u{000a}>>TURN EC-ALL CHECKING OFF\u{000a}",
        ">>IMP INCLUDE \u{0022}a.h\u{0022}\u{000a}>>CALL-CONVENTION C\u{000a}",
        ">>IF DEFINED(FLAG_A) AND CLI_PARAM = 42 AND STR_V = \u{0022}HELLO\u{0022} AND STR_V <> \u{0022}NO\u{0022}\u{000a}",
        "  01 KEPT-ONE PIC 9.\u{000a}",
        "  >>IF FALSE OR 1 + 2 * 3 - 1 / 1 >= 6 AND 2 / 0 = 0 AND 1 < 2 AND 2 > 1 AND 2 <= 2\u{000a}",
        "    01 KEPT-NESTED PIC 9.\u{000a}  >>END-IF\u{000a}",
        ">>ELIF TRUE\u{000a}  01 SKIPPED-ELIF PIC 9.\u{000a}>>ELSE\u{000a}",
        "  01 SKIPPED-ELSE PIC 9.\u{000a}",
        ">>END-IF\u{000a}",
        ">>IF NOT Flag_A IS DEFINED OR Flag_Off IS NOT DEFINED\u{000a}",
        "  >>IF DEFINED FLAG_OFF\u{000a}",
        "    >>IF TRUE\u{000a}",
        "    >>ELIF TRUE\u{000a}",
        "    >>ELSE\u{000a}",
        "    >>END-IF\u{000a}",
        "    >>DEFINE NEVER_RUN AS 1\u{000a}",
        r#"  >>ELIF "A" < "B" AND "B" > "A" AND "A" <= "A" AND "B" >= "B""#,
        "\u{000a}",
        "    01 KEPT-ELIF PIC 9.\u{000a}",
        "  >>ELSE\u{000a}",
        "  >>END-IF\u{000a}",
        ">>END-IF\u{000a}",
        ">>IF FALSE\u{000a}",
        ">>ELSE\u{000a}",
        "  01 KEPT-ELSE PIC 9.\u{000a}",
        ">>END-IF\u{000a}",
        ">>EVALUATE NUM_V\u{000a}",
        ">>WHEN 5\u{000a}",
        "  01 BAD-WHEN PIC 9.\u{000a}",
        ">>WHEN 10\u{000a}",
        "  01 KEPT-WHEN PIC 9.\u{000a}",
        ">>WHEN OTHER\u{000a}",
        "  01 SKIP-OTHER PIC 9.\u{000a}",
        ">>END-EVALUATE\u{000a}",
        ">>EVALUATE TRUE\u{000a}",
        ">>WHEN 0\u{000a}",
        ">>WHEN OTHER\u{000a}",
        "  01 KEPT-OTHER PIC 9.\u{000a}",
        ">>END-EVALUATE\u{000a}",
    );

    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let unit = preprocess_source_file(
        "cdf.cob",
        Path::new("."),
        cdf_prog,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    assert!(!sink.has_errors());
    assert_eq!(unit.cdf_env.get("CLI_PARAM"), Some(&CdfValue::Integer(42)));
    assert_eq!(unit.cdf_env.get("NUM_V"), Some(&CdfValue::Integer(10)));
    assert_eq!(unit.cdf_env.turn_records.len(), 3);
    assert_eq!(
        unit.cdf_env.turn_records.first().map(|r| r.state),
        Some(TurnCheckingState::EnabledWithLocation)
    );
    assert_eq!(
        unit.cdf_env.display_messages,
        vec![String::from("compile message")]
    );

    let pp_out = emit_preprocessed_source("cdf.cob", &unit.tokens);
    assert!(pp_out.contains("KEPT-ONE"));
    assert!(pp_out.contains("KEPT-ELIF"));
    assert!(pp_out.contains("KEPT-ELSE"));
    assert!(pp_out.contains("KEPT-WHEN"));
    assert!(pp_out.contains("KEPT-OTHER"));
    Ok(())
}

/// Exercises `-t` / `-T` compiler listing generation across narrow/wide, pagination, and visibility flags.
#[test]
fn listing_generator_coverage() -> Result<()> {
    let mem = InMemoryCopybookResolver::new();
    let dialect_cfg = DialectConfig::for_dialect(Dialect::Default);
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Fixed,
        dialect_config: &dialect_cfg,
        debug_policy: DebugLinePolicy::CompileDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let src = concat!(
        "000100 IDENTIFICATION DIVISION.\u{000a}",
        "000200* Comment\u{000a}",
        "000300/ Page\u{000a}",
        "000400D      DISPLAY \u{0022}D\u{0022}.\u{000a}",
        "000500$SET X\u{000a}",
        "000600       MOVE 1\u{000a}",
        "000700-          TO A.\u{000a}",
    );
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 10);
    let unit = preprocess_source_file("l.cob", Path::new("."), src, &pp_cfg, &mut sm, &mut sink)?;

    let lst_default = render_compiler_listing(
        "l.cob",
        &unit.root_lines,
        &unit.tokens,
        &sink,
        ListingOptions {
            width: ListingWidth::Narrow80,
            lines_per_page: 3,
            symbols: ListingSymbols::IncludeSymbols,
            header: HeaderVisibility::ShowHeader,
            messages: MessageVisibility::ShowMessages,
        },
    );
    assert!(lst_default.contains("Page 0002"));
    assert!(lst_default.contains("SYMBOL CROSS-REFERENCE"));

    let lst_minimal = render_compiler_listing(
        "l.cob",
        &unit.root_lines,
        &unit.tokens,
        &sink,
        ListingOptions {
            width: ListingWidth::Wide120,
            lines_per_page: 2,
            symbols: ListingSymbols::OmitSymbols,
            header: HeaderVisibility::HideHeader,
            messages: MessageVisibility::HideMessages,
        },
    );
    assert!(!lst_minimal.contains("Page 0001"));
    assert_eq!(
        emit_preprocessed_source("empty.cob", &[]),
        "#line 1 \u{0022}empty.cob\u{0022}\u{000a}"
    );
    Ok(())
}

/// Exercises all CDF error diagnostics (`>>END-IF`, `>>ELIF`, `>>ELSE`, `>>WHEN`, `>>POP`, `>>DEFINE`, `>>TURN`, unknown).
#[test]
fn cdf_error_branches_coverage() -> Result<()> {
    let mem = InMemoryCopybookResolver::new();
    let dialect_cfg = DialectConfig::for_dialect(Dialect::Cobol2023);
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dialect_cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };

    let bad_cdf = concat!(
        ">>ELIF TRUE\u{000a}",
        ">>ELSE\u{000a}",
        ">>END-IF\u{000a}",
        ">>WHEN 1\u{000a}",
        ">>END-EVALUATE\u{000a}",
        ">>POP\u{000a}",
        ">>DEFINE\u{000a}",
        ">>TURN EC-ALL ON\u{000a}",
        ">>BOGUS-DIR\u{000a}",
        ">>IF\u{000a}",
    );
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let _ = preprocess_source_file(
        "bad_cdf.cob",
        Path::new("."),
        bad_cdf,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    assert_eq!(sink.error_count(), 11);
    let _ = CdfEnv::new();
    Ok(())
}

/// Exercises remaining edge branches (`OF`, `SourceMap` caps, invalid UTF-8 copybook, CDF/format/replace edges).
#[test]
fn remaining_edge_and_capacity_branches_coverage() -> Result<()> {
    let mut mem = InMemoryCopybookResolver::new();
    mem.insert(Some("LIB1"), "B1", "b1.cpy", "01 V1 PIC X.\u{000a}");
    mem.insert(None, "NEST", "nest.cpy", "COPY B1 OF LIB1.\u{000a}");
    let dialect_cfg = DialectConfig::for_dialect(Dialect::Cobol2023);
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dialect_cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };

    // Empty source file + pagination disabled (`lines_per_page: 0`) + manual empty ReplacementRule.
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let empty_u =
        preprocess_source_file("mt.cob", Path::new("."), "", &pp_cfg, &mut sm, &mut sink)?;
    let _ = render_compiler_listing(
        "mt.cob",
        &empty_u.root_lines,
        &[],
        &sink,
        ListingOptions {
            lines_per_page: 0,
            ..ListingOptions::default()
        },
    );

    // Edge cases across format, pseudo_text, copy (`OF`), replace (`BAD`, `LAST BAD`, `A BAD`), and CDF.
    let edge_src = concat!(
        ">>SOURCE FORMAT IS FREE\u{000a}",
        ">>SOURCE BOGUS\u{000a}",
        "SET SOURCEFORMAT\u{0022}BOGUS\u{0022}\u{000a}",
        "*CONTROL\u{000a}*CBL\u{000a}",
        "MOVE \u{0022}Q1\u{0022}-\u{000a}NOQUOTE-\u{000a}NEXT.\u{000a}",
        ">>SOURCE FORMAT IS FIXED\u{000a}",
        "000100       MOVE \u{0022}CLOSED\u{0022} TO\u{000a}",
        "000200-          TARGET.\u{000a}",
        ">>SOURCE FORMAT IS FREE\u{000a}",
        "COMPUTE V = .5 + A=B + X\u{000a}",
        "COPY B1 OF LIB1.\u{000a}",
        "REPLACE BAD.\u{000a}",
        "REPLACE LAST BAD.\u{000a}",
        "REPLACE A BAD.\u{000a}",
        "REPLACE ==A OF REC== BY ==Z==.\u{000a}",
        ">>DEFINE D1\u{000a}>>DEFINE D1 AS 2\u{000a}>>DEFINE UNIT_ONLY\u{000a}>>DEFINE OFF_V AS OFF\u{000a}",
        ">>IF FALSE\u{000a}>>ELIF FALSE\u{000a}>>END-IF\u{000a}",
        ">>IF FALSE\u{000a}>>EVALUATE 1\u{000a}>>WHEN 1\u{000a}>>END-EVALUATE\u{000a}>>END-IF\u{000a}",
        ">>IF TRUE OR FALSE\u{000a}>>END-IF\u{000a}",
        ">>IF FALSE AND TRUE\u{000a}>>ELIF TRUE = 1\u{000a}>>END-IF\u{000a}",
        ">>IF UNIT_ONLY AND 1 <> 2 AND 'S1' != 'S2' AND \u{0022}TXT\u{0022} = 0 AND 0 = \u{0022}TXT\u{0022} AND OFF_V = 0 AND UNDEF_X = 0\u{000a}>>END-IF\u{000a}",
        "MOVE A\u{000a}",
    );
    let u2 = preprocess_source_file(
        "ed.cob",
        Path::new("."),
        edge_src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let _ = render_compiler_listing(
        "ed.cob",
        &u2.root_lines,
        &[],
        &sink,
        ListingOptions {
            lines_per_page: 0,
            ..ListingOptions::default()
        },
    );
    for rk in [
        coblang_pp::ReplacementPatternKind::LeadingWordPrefix,
        coblang_pp::ReplacementPatternKind::TrailingWordSuffix,
    ] {
        let _ = apply_replacement_rules(
            &u2.tokens,
            &[coblang_pp::ReplacementRule {
                kind: rk,
                pattern: Vec::new(),
                replacement: Vec::new(),
            }],
        );
    }
    Ok(())
}

/// Exercises `SourceMap` capacity error propagation across root files, `COPY`, and `REPLACE`.
#[test]
fn sourcemap_capacity_and_utf8_error_coverage() {
    let mut mem = InMemoryCopybookResolver::new();
    mem.insert(Some("LIB1"), "B1", "b1.cpy", "01 V1 PIC X.\u{000a}");
    mem.insert(None, "NEST", "nest.cpy", "COPY B1 OF LIB1.\u{000a}");
    let dialect_cfg = DialectConfig::for_dialect(Dialect::Cobol2023);
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dialect_cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);

    // SourceMap capacity error propagation in lib.rs, copy.rs, and replace.rs.
    let mut sm_0_files = SourceMap::with_limits(0, 1000, 10);
    assert!(
        preprocess_source_file(
            "x.cob",
            Path::new("."),
            "MOVE 1.",
            &pp_cfg,
            &mut sm_0_files,
            &mut sink
        )
        .is_err()
    );

    let mut sm_0_exp = SourceMap::with_limits(10, 1000, 0);
    assert!(
        preprocess_source_file(
            "x.cob",
            Path::new("."),
            "COPY NEST.",
            &pp_cfg,
            &mut sm_0_exp,
            &mut sink
        )
        .is_err()
    );

    let mut sm_1_file = SourceMap::with_limits(1, 1000, 10);
    assert!(
        preprocess_source_file(
            "x.cob",
            Path::new("."),
            "COPY NEST.",
            &pp_cfg,
            &mut sm_1_file,
            &mut sink
        )
        .is_err()
    );

    let mut sm_2_files = SourceMap::with_limits(2, 1000, 10);
    assert!(
        preprocess_source_file(
            "x.cob",
            Path::new("."),
            "COPY NEST.",
            &pp_cfg,
            &mut sm_2_files,
            &mut sink
        )
        .is_err()
    );

    let mut sm_rep_0_exp = SourceMap::with_limits(10, 1000, 0);
    assert!(
        preprocess_source_file(
            "x.cob",
            Path::new("."),
            "REPLACE ==A== BY ==B==.",
            &pp_cfg,
            &mut sm_rep_0_exp,
            &mut sink
        )
        .is_err()
    );
}

/// Exercises invalid UTF-8 copybooks, canonical self-cycles, and empty `expand_copy_tokens` input.
#[test]
fn filesystem_utf8_and_self_cycle_coverage() -> Result<()> {
    // Invalid UTF-8 copybook file triggers `fs::read_to_string` error in `FilesystemCopybookResolver`.
    let tmp = std::env::temp_dir().join(format!("coblang-pp-utf8-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    std::fs::write(tmp.join("badutf8.cpy"), [0xFF_u8, 0xFE_u8])?;
    let fs_res = FilesystemCopybookResolver::default();
    assert!(fs_res.resolve_copybook("badutf8", None, &tmp).is_err());

    // Canonical path cycle check (`./self.cpy`) and `expand_copy_tokens` wrapper.
    std::fs::write(tmp.join("self.cpy"), "       COPY \"./self.cpy\".\u{000a}")?;
    let cfg = DialectConfig::for_dialect(Dialect::Default);
    let fs_pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &fs_res,
    };
    let mut sm_cyc = SourceMap::new();
    let mut sink_cyc = DiagnosticSink::new(WarningPolicy::new(), 10);
    let _ = preprocess_source_file(
        "self.cpy",
        &tmp,
        "COPY \"./self.cpy\".",
        &fs_pp_cfg,
        &mut sm_cyc,
        &mut sink_cyc,
    )?;
    assert!(sink_cyc.has_errors());

    let mut cdf_env = CdfEnv::new();
    let mut stack = Vec::new();
    let empty_expanded = coblang_pp::copy::expand_copy_tokens(
        &[],
        &tmp,
        &fs_pp_cfg,
        &mut cdf_env,
        &mut sm_cyc,
        &mut sink_cyc,
        &mut stack,
    )?;
    assert_eq!(empty_expanded.len(), 0);

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Exercises `PreprocessLimits` copy-depth / token caps and empty `ReplacementRule::pattern` safeguards.
#[test]
fn preprocess_limits_and_empty_rule_safeguard_coverage() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-pp-limits-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    std::fs::write(tmp.join("leaf.cpy"), "MOVE A TO B.\u{000a}")?;
    let fs_res = FilesystemCopybookResolver::default();
    let cfg = DialectConfig::for_dialect(Dialect::Default);
    let fs_pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &fs_res,
    };

    let mut sm_lim = SourceMap::new();
    let mut sink_lim = DiagnosticSink::new(WarningPolicy::new(), 10);
    assert!(
        preprocess_source_file_with_limits(
            "root.cob",
            &tmp,
            "COPY \"leaf.cpy\".",
            &fs_pp_cfg,
            PreprocessLimits::new(0, 100),
            &mut sm_lim,
            &mut sink_lim,
        )
        .is_err()
    );
    assert!(
        preprocess_source_file_with_limits(
            "root2.cob",
            &tmp,
            "IDENTIFICATION DIVISION. PROGRAM-ID. P.",
            &fs_pp_cfg,
            PreprocessLimits::new(8, 2),
            &mut sm_lim,
            &mut sink_lim,
        )
        .is_err()
    );

    let span = coblang_common::SourceSpan::new(
        coblang_common::FileId::new(0),
        coblang_common::ByteOffset::ZERO,
        coblang_common::ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let sample_tok = PpToken {
        text: String::from("X"),
        kind: PpTokenKind::Word,
        span,
        zone: MarginZone::AreaB,
        spacing: SpacingHint::Space,
    };
    assert!(
        apply_replace_statements_with_limit(
            &[sample_tok.clone(), sample_tok.clone()],
            1,
            &mut sm_lim,
            &mut sink_lim,
        )
        .is_err()
    );
    assert!(
        preprocess_source_file_with_limits(
            "rep_boom.cob",
            &tmp,
            "REPLACE ==A== BY ==B C D E F G H I J K==. A A A.",
            &fs_pp_cfg,
            PreprocessLimits::new(8, 22),
            &mut sm_lim,
            &mut sink_lim,
        )
        .is_err()
    );

    // Empty `ReplacementRule::pattern` safeguard returns input unchanged without looping.
    let empty_rule = coblang_pp::ReplacementRule {
        kind: coblang_pp::ReplacementPatternKind::FullSequence,
        pattern: Vec::new(),
        replacement: vec![sample_tok.clone()],
    };
    assert_eq!(
        apply_replacement_rules(core::slice::from_ref(&sample_tok), &[empty_rule]).len(),
        1
    );

    // Comma separator between REPLACING clauses (`PpTokenKind::SeparatorPunct` in `parse_replacing_clauses`).
    let comma_rep = preprocess_source_file(
        "comma_rep.cob",
        &tmp,
        "       REPLACE ==X1== BY ==Y1==, ==X2== BY ==Y2==.\n       X1 X2.\n",
        &fs_pp_cfg,
        &mut sm_lim,
        &mut sink_lim,
    )?;
    assert!(comma_rep.tokens.iter().any(|t| t.text == "Y2"));

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Exercises `*CONTROL`/`*CBL`, free-format `/` & `*>` comments, and `AUTHOR.` unclosed quotes.
#[test]
fn m8_cbl_control_free_comments_and_author_quote_coverage() -> Result<()> {
    let mem = InMemoryCopybookResolver::new();
    let cfg = DialectConfig::for_dialect(Dialect::Default);
    let free_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 10);
    let cbl_free = concat!(
        ".\n",
        "/ page eject in free format\n",
        "   *> indented free comment\n",
        "       *CONTROL SOURCE\n",
        "       *CONTROL\n",
        "       *CBL LIST\n",
        "       *CBL\n",
        "       PROGRAM-ID. CBLP.\n",
    );
    let cbl_unit = preprocess_source_file(
        "cbl.cob",
        Path::new("."),
        cbl_free,
        &free_cfg,
        &mut sm,
        &mut sink,
    )?;
    assert!(cbl_unit.tokens.iter().any(|t| t.text == "CBLP"));

    let fixed_cfg = PreprocessConfig {
        initial_format: SourceFormat::Fixed,
        dialect_config: &cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sink_id = DiagnosticSink::new(WarningPolicy::new(), 10);
    let id_src = concat!(
        "       IDENTIFICATION DIVISION.\n",
        "       PROGRAM-ID. IDP.\n",
        "       AUTHOR.\n",
        "           O'Brien unclosed quote in comment paragraph.\n",
        "       DATA DIVISION.\n",
    );
    let _ = preprocess_source_file(
        "id.cob",
        Path::new("."),
        id_src,
        &fixed_cfg,
        &mut sm,
        &mut sink_id,
    )?;
    assert!(!sink_id.has_errors());
    Ok(())
}

/// Exercises M9 `$SET BOUND`/`NOBOUND`/`NOSSRANGE` in free and fixed formats, continuations across `$SET`, and inactive `>>IF` blocks.
#[test]
fn m9_vendor_set_bound_free_and_continuation_coverage() -> Result<()> {
    let mem = InMemoryCopybookResolver::new();
    let cfg = DialectConfig::for_dialect(Dialect::Gnu);
    let free_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 10);
    let free_src = concat!(
        "$SET BOUND\n",
        "$SET NOBOUND\n",
        "$SET SSRANGE\n",
        "$SET NOSSRANGE\n",
        "$SET OTHER\n",
        "$OTHER\n",
        ">>IF 0\n",
        "$SET BOUND\n",
        ">>END-IF\n",
        "MOVE \"AB\"-\n",
        "$SET BOUND\n",
        "\"CD\" TO X.\n",
    );
    let u_free = preprocess_source_file(
        "set_free.cob",
        Path::new("."),
        free_src,
        &free_cfg,
        &mut sm,
        &mut sink,
    )?;
    assert!(u_free.cdf_env.turn_records.len() >= 3);

    let fixed_cfg = PreprocessConfig {
        initial_format: SourceFormat::Fixed,
        dialect_config: &cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let fixed_src = concat!(
        "000100       MOVE \"AB\n",
        "000200$SET BOUND\n",
        "000300-          \"CD\" TO X.\n",
    );
    let u_fixed = preprocess_source_file(
        "set_fixed.cob",
        Path::new("."),
        fixed_src,
        &fixed_cfg,
        &mut sm,
        &mut sink,
    )?;
    assert_eq!(u_fixed.cdf_env.turn_records.len(), 1);

    let fid = sm.add_file(String::from("raw_dlr.cob"), String::from("$SET BOUND\n"))?;
    let fref = sm.get_file(fid)?;
    let raw_lines = normalize_file(
        fref,
        SourceFormat::Free,
        &cfg,
        DebugLinePolicy::StripDebugLines,
        ExpansionId::ROOT,
        &mut sink,
    );
    let toks = tokenize_normalized_lines(&raw_lines, &mut sink);
    assert_eq!(toks.len(), 0);
    Ok(())
}
