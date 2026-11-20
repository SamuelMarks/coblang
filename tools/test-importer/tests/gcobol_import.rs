//! Unit and full-tree integration tests for Phase 2 (`gcobol` `DejaGnu` importer & taxonomy).

use std::path::{Path, PathBuf};

use coblang_common::{
    Capability, CompilationMode, CompileCobolSpec, Dialect, FeatureTag, OptimizationLevel,
    Provenance, Result, SourceFormat, SuiteId, TestManifest, TestOutcome, TextEncoding,
    encode_manifest_bundle,
};
use coblang_test_importer::gcobol::{
    DgAction, DgOptPin, apply_gcobol_options, import_cobol_dg_file, import_gcobol_tree,
    parse_dejagnu_directives, resolve_dg_line_spec, tokenize_tcl_words,
};
use coblang_test_importer::taxonomy::{
    infer_feature_tags, link_cross_suite_equivalents, map_dialect_tag, normalize_equivalence_key,
};
use coblang_test_importer::{import_and_write_all_ported_suites, importer_smoke_check};
use coblang_test_runner::capabilities::{HostCapabilities, IconvProbePolicy};
use coblang_test_runner::engine::{
    CompilerBackend, RunnerConfig, SandboxRetention, execute_suite_parallel,
};

/// Exercises `taxonomy.rs` tag inference and cross-suite equivalence linking.
#[test]
fn taxonomy_and_equivalence_coverage() {
    let empty_tags = infer_feature_tags(
        "",
        CompilationMode::BuildExecutable,
        Dialect::Default,
        &TextEncoding::Ascii,
    );
    assert_eq!(empty_tags, vec![FeatureTag::FeaturePerformEvalIf]);

    let pre_tags = infer_feature_tags(
        "COMP",
        CompilationMode::PreprocessOnly,
        Dialect::Ibm,
        &TextEncoding::Ebcdic,
    );
    assert!(pre_tags.contains(&FeatureTag::StagePreprocess));
    assert!(pre_tags.contains(&FeatureTag::FeatureEbcdic));
    assert!(pre_tags.contains(&FeatureTag::TypeBinaryComp5));

    let sema_kitchen_sink = r"COPY A. PIC X PIC 9 PIC Z COMP-5 COMP-3 COMP-1 POINTER NATIONAL 9(37)
88 FLG REDEFINES OCCURS (1:2) ADD MOVE STRING PERFORM SEARCH CALL GLOBAL
ALLOCATE DECLARATIVES FUNCTION FUNCTION-ID SEQUENTIAL RELATIVE INDEXED
MERGE REPORT SECTION SCREEN SECTION XML GENERATE CLASS-ID CBL_OPEN";
    let all_inferred = infer_feature_tags(
        sema_kitchen_sink,
        CompilationMode::SyntaxOnly,
        Dialect::Gnu,
        &TextEncoding::Ascii,
    );
    assert!(all_inferred.len() >= 30);

    for d in [
        Dialect::Cobol85,
        Dialect::Cobol2002,
        Dialect::Cobol2014,
        Dialect::Cobol2023,
        Dialect::Ibm,
        Dialect::MicroFocus,
        Dialect::Gnu,
    ] {
        assert!(map_dialect_tag(d).is_some());
    }
    assert_eq!(map_dialect_tag(Dialect::Bs2000), None);

    assert_eq!(
        normalize_equivalence_key("ADD_CORRESPONDING (2)"),
        "add_corresponding_2"
    );
    assert_eq!(normalize_equivalence_key("---"), "");

    let mk = |id: &str, suite: SuiteId, orig: &str| TestManifest {
        id: String::from(id),
        suite,
        provenance: Provenance {
            upstream_repo: String::from("r"),
            upstream_commit: String::from("c"),
            source_file: String::from("f"),
            source_line: 1,
            original_test_name: String::from(orig),
            spdx_license: String::from("GPL-3.0-or-later"),
        },
        files: Vec::new(),
        steps: Vec::new(),
        torture_optimizations: Vec::new(),
        required_capabilities: Vec::new(),
        xfail_reason: None,
        skip_reason: None,
        feature_tags: Vec::new(),
        equivalent_tests: Vec::new(),
    };
    let mut items = vec![
        mk("gcobol/group2/ADD_CORR", SuiteId::Gcobol, "ADD_CORR"),
        mk("gnucobol/run/001", SuiteId::GnuCobol, "ADD CORR"),
        mk("gcobol/short", SuiteId::Gcobol, "ab"),
        mk("gcobol/solo", SuiteId::Gcobol, "UNIQUE_SOLO_NAME"),
    ];
    link_cross_suite_equivalents(&mut items);
    let equivs: Vec<Vec<String>> = items.into_iter().map(|m| m.equivalent_tests).collect();
    assert_eq!(
        equivs,
        vec![
            vec![String::from("gnucobol/run/001")],
            vec![String::from("gcobol/group2/ADD_CORR")],
            Vec::<String>::new(),
            Vec::<String>::new(),
        ]
    );
}

/// Exercises every `DejaGnu` Tcl tokenizer, line resolver, directive, and flag-mapping branch.
#[test]
fn dejagnu_parser_and_flag_mapper_coverage() -> Result<()> {
    assert_eq!(importer_smoke_check("abc")?, 3);
    assert!(importer_smoke_check("").is_err());

    let backslash = '\u{005c}';
    let escaped_tcl = format!(
        r#"dg-error "msg{backslash}n{backslash}r{backslash}t{backslash}"" {{ target {{ *-*-* }} }} .-1  "#
    );
    let words = tokenize_tcl_words(&escaped_tcl, "t.cob", 1)?;
    assert_eq!(words.first().map(String::as_str), Some("dg-error"));
    assert_eq!(
        tokenize_tcl_words("dg-do run", "t.cob", 1)?,
        vec![String::from("dg-do"), String::from("run")]
    );
    assert!(tokenize_tcl_words(&format!(r#""unclosed{backslash}"#), "t.cob", 1).is_err());
    assert!(tokenize_tcl_words(r#"dg-error "unclosed"#, "t.cob", 1).is_err());
    assert!(tokenize_tcl_words("dg-error {unclosed", "t.cob", 1).is_err());

    assert_eq!(resolve_dg_line_spec(None, 5), Some(5));
    assert_eq!(resolve_dg_line_spec(Some("."), 5), Some(5));
    assert_eq!(resolve_dg_line_spec(Some(""), 5), Some(5));
    assert_eq!(resolve_dg_line_spec(Some("0"), 5), None);
    assert_eq!(resolve_dg_line_spec(Some(".+3"), 5), Some(8));
    assert_eq!(resolve_dg_line_spec(Some(".-2"), 5), Some(3));
    assert_eq!(resolve_dg_line_spec(Some(".+bad"), 5), None);
    assert_eq!(resolve_dg_line_spec(Some(".-bad"), 5), None);
    assert_eq!(resolve_dg_line_spec(Some("19"), 5), Some(19));

    exercise_all_dg_directives()?;
    exercise_gcobol_flag_options();
    Ok(())
}

/// Tests all `dg-*` directive variants and error cases in `parse_dejagnu_directives`.
fn exercise_all_dg_directives() -> Result<()> {
    for (kw, act) in [
        ("preprocess", DgAction::Preprocess),
        ("compile", DgAction::Compile),
        ("assemble", DgAction::Assemble),
        ("link", DgAction::Link),
        ("run", DgAction::Run),
    ] {
        let p = parse_dejagnu_directives(&format!("*> {{ dg-do {kw} }}"), "a.cob")?;
        assert_eq!(p.action, act);
    }
    assert!(parse_dejagnu_directives("*> { dg-do }", "a.cob").is_err());
    assert!(parse_dejagnu_directives("*> { dg-do invalid }", "a.cob").is_err());
    assert!(parse_dejagnu_directives(r#"*> { dg-options "unclosed }"#, "a.cob").is_err());

    let src = r#"*> { not-dg ignored }
*> {   }
*> { dg-do run { target *-*-* } }
*> { dg-options "-O2 -dialect ibm" }
*> { dg-additional-options "-Wno-foo" }
*> { dg-options }
*> { dg-output {hello} }
*> { dg-output }
*> { dg-output-file "g.out" }
*> { dg-output-file }
*> { dg-warning "w" "" { target *-*-* } .+1 }
*> { dg-message "m" }
*> { dg-bogus "b" }
*> { dg-bogus }
*> { dg-excess-errors "all" }
*> { dg-prune-output "prune" }
*> { dg-prune-output }
*> { dg-additional-sources "aux1.cob aux2.cob" }
*> { dg-additional-sources }
*> { dg-set-target-env-var TZ "UTC0" }
*> { dg-set-target-env-var ONLY_KEY }
*> { dg-require-iconv "CP1140" }
*> { dg-require-iconv }
*> { dg-require-effective-target int128 }
*> { dg-require-effective-target other }
*> { dg-xfail-if "reason" { *-*-* } }
*> { dg-skip-if "" { *-*-* } }
*> { dg-skip-if "explicit_skip" { *-*-* } }
*> { dg-timeout-factor 2.0 }
*> { dg-timeout-factor 3 }
*> { dg-timeout-factor bad }
*> { dg-timeout-factor }
*> { dg-unknown-custom "x" }
{ dg-begin-multiline-output "" }
*> { dg-error "ignored_inside_multiline" }
{ dg-end-multiline-output "" }
"#;
    let d = parse_dejagnu_directives(src, "dir/full.cob")?;
    assert_eq!(d.opt_pin, DgOptPin::Explicit);
    assert!(d.allow_excess_errors);
    assert_eq!(d.timeout_factor, 3);
    assert_eq!(d.additional_sources.len(), 2);
    assert_eq!(d.capabilities.len(), 2);
    assert_eq!(d.diagnostics.len(), 2);
    assert_eq!(d.skip_reason.as_deref(), Some("explicit_skip"));
    Ok(())
}

/// Tests `apply_gcobol_options` across two-token, attached, trailing, and unrecognized options.
fn exercise_gcobol_flag_options() {
    let mut spec = CompileCobolSpec {
        mode: CompilationMode::SyntaxOnly,
        format: SourceFormat::Auto,
        dialect: Dialect::Default,
        optimization: OptimizationLevel::O0,
        encoding: TextEncoding::Ascii,
        sources: Vec::new(),
        include_dirs: Vec::new(),
        copy_exts: Vec::new(),
        library_dirs: Vec::new(),
        libraries: Vec::new(),
        raw_flags: Vec::new(),
        output: String::new(),
        expected_success: true,
        expected_diagnostics: Vec::new(),
        bogus_patterns: Vec::new(),
        allow_excess_errors: false,
        prune_patterns: Vec::new(),
        env: Vec::new(),
    };
    apply_gcobol_options(
        &mut spec,
        &[
            String::from("-ffixed-form -ffree-form -finternal-ebcdic -dialect mf -dialect bad"),
            String::from(
                "-I inc1 -Iinc2 -copyext .cpy -fdefaultbyte 64 -fexec-charset=CP037 -O3 -Wall",
            ),
            String::from("-fdefaultbyte"),
            String::from("-dialect"),
            String::from("-I"),
            String::from("-copyext"),
        ],
    );
    assert_eq!(spec.format, SourceFormat::Free);
    assert_eq!(spec.dialect, Dialect::MicroFocus);
    assert_eq!(spec.optimization, OptimizationLevel::O3);
    assert_eq!(spec.encoding, TextEncoding::Codepage(String::from("CP037")));
    assert_eq!(
        spec.include_dirs,
        vec![String::from("inc1"), String::from("inc2")]
    );
    assert_eq!(spec.copy_exts, vec![String::from(".cpy")]);
    assert!(spec.raw_flags.contains(&String::from("-fdefaultbyte=64")));
    assert!(spec.raw_flags.contains(&String::from("-fdefaultbyte")));
}

/// Imports synthetic multi-source / preprocess / assemble / link `.cob` files and the full 543-test `gcobol` cache!
#[test]
fn import_full_gcobol_cache_and_synthetic_modes() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-gcob-imp-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    std::fs::write(tmp.join("aux.cob"), "IDENTIFICATION DIVISION.\u{000a}")?;
    for (name, body) in [
        (
            "pre.cob",
            r#"*> { dg-do preprocess }
*> { dg-additional-sources "aux.cob missing.cob" }
"#,
        ),
        ("asm.cob", "*> { dg-do assemble }\u{000a}"),
        ("lnk.cob", "*> { dg-do link }\u{000a}"),
        (
            "run_plain.cob",
            "*> { dg-do run }\u{000a}IDENTIFICATION DIVISION.\u{000a}",
        ),
        (
            "run_err.cob",
            r#"*> { dg-do run }
*> { dg-error "compile fails before run" }
"#,
        ),
    ] {
        let p = tmp.join(name);
        std::fs::write(&p, body)?;
        let m = import_cobol_dg_file(&p, &tmp, "gcobol/syn", name)?;
        assert_eq!(m.suite, SuiteId::Gcobol);
    }
    std::fs::remove_dir_all(&tmp)?;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cache = root.join(".upstream-cache/gcobol");
    let bundles = import_gcobol_tree(&cache)?;

    assert_eq!(bundles.top_level.len(), 8);
    assert_eq!(bundles.group1.len(), 16);
    assert!(bundles.group2.len() >= 500);
    assert_eq!(bundles.libgcobol.len(), 6);

    let tmp_out = std::env::temp_dir().join(format!("coblang-gcob-out-{}", std::process::id()));
    let out_dir = tmp_out.join("gcobol");
    std::fs::create_dir_all(&out_dir)?;
    std::fs::write(
        out_dir.join("top_level.bundle"),
        encode_manifest_bundle(&bundles.top_level),
    )?;
    std::fs::write(
        out_dir.join("group1.bundle"),
        encode_manifest_bundle(&bundles.group1),
    )?;
    std::fs::write(
        out_dir.join("group2.bundle"),
        encode_manifest_bundle(&bundles.group2),
    )?;
    std::fs::write(
        out_dir.join("libgcobol.bundle"),
        encode_manifest_bundle(&bundles.libgcobol),
    )?;

    let mut all = Vec::new();
    all.extend(bundles.top_level);
    all.extend(bundles.group1);
    all.extend(bundles.group2);
    all.extend(bundles.libgcobol);
    let cfg = RunnerConfig {
        backend: CompilerBackend::InProcessScaffold,
        sandbox_root: tmp_out.join("dryrun-sbx"),
        sandbox_retention: SandboxRetention::CleanAlways,
        c_compiler: PathBuf::from("clang"),
        capabilities: HostCapabilities::from_explicit(
            &[Capability::PosixTools],
            IconvProbePolicy::ProbeSystemIconv,
        ),
        jobs: 8,
    };
    let records = execute_suite_parallel(&all, &cfg)?;
    assert_eq!(records.len(), all.len());
    for r in &records {
        assert!(
            matches!(r.outcome, TestOutcome::XFail | TestOutcome::Skip),
            "unexpected outcome {:?} on {}: {}",
            r.outcome,
            r.id,
            r.detail
        );
    }

    let full_summary =
        import_and_write_all_ported_suites(&root.join(".upstream-cache"), &tmp_out.join("ported"))?;
    let _ = std::fs::remove_dir_all(&tmp_out);
    assert_eq!(full_summary.total_manifests, 2525);
    assert!(full_summary.linked_equivalents > 0);
    Ok(())
}
