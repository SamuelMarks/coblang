//! Phase 1.3 canary self-tests and full unit/integration coverage for `coblang-test-runner`.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use coblang_common::{
    Capability, CompilationMode, CompileCSpec, CompileCobolSpec, DiagnosticSeverity, Dialect,
    DriverFlavor, ExpectedDiagnostic, FeatureTag, OptimizationLevel, OutputMatcher, Provenance,
    Result, RunBinarySpec, SourceFormat, SuiteId, TestManifest, TestOutcome, TestStep,
    TextEncoding, WorkspaceFile, encode_manifest_bundle,
};
use coblang_test_runner::capabilities::{
    HostCapabilities, IconvProbePolicy, TargetEndianness, insert_endian_capability,
    probe_iconv_with_binary,
};
use coblang_test_runner::cli::{RunnerCommand, parse_runner_cli, resolve_oracle_backend, run_cli};
use coblang_test_runner::engine::{
    CompilerBackend, RunnerConfig, SandboxRetention, TestExecutionRecord, build_driver_cli_args,
    execute_single_test, execute_suite_parallel,
};
use coblang_test_runner::matcher::{
    DiagnosticMessagePolicy, ExcessErrorPolicy, compile_regex, normalize_lines,
    parse_compiler_diagnostics, parse_single_diagnostic_line, verify_ccvs85_report,
    verify_compile_diagnostics, verify_output,
};
use coblang_test_runner::process::{
    CommandSpec, compile_c_companion, read_optional_pipe, run_command_with_timeout,
};
use coblang_test_runner::ratchet::{
    OutcomeCounts, decode_expectations, emit_lit_wrappers, encode_expectations,
    render_summary_json, render_summary_markdown, snapshot_expectations, verify_ratchet,
};

/// Creates a default [`Provenance`] record for synthetic canary manifests.
fn canary_prov(name: &str) -> Provenance {
    Provenance {
        upstream_repo: String::from("coblang-canary"),
        upstream_commit: String::from("HEAD"),
        source_file: format!("tests/canary/{name}.cob"),
        source_line: 1,
        original_test_name: String::from(name),
        spdx_license: String::from("Apache-2.0 WITH LLVM-exception"),
    }
}

/// Builds a baseline [`CompileCobolSpec`] for canary tests.
fn base_cobol_spec(mode: CompilationMode, src: &str, out: &str) -> CompileCobolSpec {
    CompileCobolSpec {
        mode,
        format: SourceFormat::Free,
        dialect: Dialect::Default,
        optimization: OptimizationLevel::O0,
        encoding: TextEncoding::Ascii,
        sources: vec![String::from(src)],
        include_dirs: Vec::new(),
        copy_exts: Vec::new(),
        library_dirs: Vec::new(),
        libraries: Vec::new(),
        raw_flags: Vec::new(),
        output: String::from(out),
        expected_success: true,
        expected_diagnostics: Vec::new(),
        bogus_patterns: Vec::new(),
        allow_excess_errors: false,
        prune_patterns: Vec::new(),
        env: Vec::new(),
    }
}

/// Tests diagnostic parsing, `dg-bogus`, `dg-prune-output`, and excess-error rules.
#[test]
fn diagnostic_matcher_coverage() -> Result<()> {
    assert!(compile_regex("[").is_err());
    assert_eq!(
        parse_single_diagnostic_line("plain text without colon"),
        None
    );

    let raw = r"Banner line without colon
prog.cob:12:5: error: invalid PIC
prog.cob:18: warning: truncated
cobol1: sorry, unimplemented: feature X
prog.cob:20: fatal error: stop
a:b:notnum: note: info
a:999999999999:5: note: overflow_ln
a:999999999999: note: overflow_ln2
pruned line: error: ignored
";
    let parsed = parse_compiler_diagnostics(raw, &[String::from("^pruned line")])?;
    assert_eq!(parsed.len(), 7);
    assert_eq!(parsed.first().map(|d| d.file.as_str()), Some("prog.cob"));
    assert_eq!(parsed.first().and_then(|d| d.line), Some(12));
    assert_eq!(parsed.get(1).and_then(|d| d.line), Some(18));
    assert_eq!(parsed.get(2).map(|d| d.line), Some(None));
    assert!(parse_compiler_diagnostics(raw, &[String::from("[")]).is_err());

    verify_bogus_and_message_policies(raw)?;
    verify_diagnostic_mismatch_cases()?;
    verify_output_variants()
}

/// Exercises `dg-bogus`, `dg-prune-output`, and [`DiagnosticMessagePolicy`] combinations.
///
/// # Arguments
///
/// * `raw` - Sample multi-line compiler diagnostic output.
fn verify_bogus_and_message_policies(raw: &str) -> Result<()> {
    assert!(
        verify_compile_diagnostics(
            raw,
            &[],
            &[String::from("invalid PIC")],
            &[],
            ExcessErrorPolicy::AllowExcessErrors,
            DiagnosticMessagePolicy::VerifyMessagePattern,
        )?
        .is_some()
    );
    assert!(
        verify_compile_diagnostics(
            raw,
            &[],
            &[String::from("absent_bogus")],
            &[],
            ExcessErrorPolicy::AllowExcessErrors,
            DiagnosticMessagePolicy::VerifyMessagePattern,
        )?
        .is_none()
    );
    assert!(
        verify_compile_diagnostics(
            raw,
            &[],
            &[String::from("[")],
            &[],
            ExcessErrorPolicy::AllowExcessErrors,
            DiagnosticMessagePolicy::VerifyMessagePattern,
        )
        .is_err()
    );

    let exp = vec![ExpectedDiagnostic {
        file: String::from("prog.cob"),
        line: Some(12),
        severity: DiagnosticSeverity::Error,
        message_pattern: String::from("invalid PIC"),
    }];
    assert!(
        verify_compile_diagnostics(
            raw,
            &exp,
            &[],
            &[String::from("^pruned")],
            ExcessErrorPolicy::ForbidExcessErrors,
            DiagnosticMessagePolicy::VerifyMessagePattern,
        )?
        .is_some()
    );
    assert!(
        verify_compile_diagnostics(
            "prog.cob:12:5: error: invalid PIC\nprog.cob:18: warning: ok\n",
            &exp,
            &[],
            &[],
            ExcessErrorPolicy::ForbidExcessErrors,
            DiagnosticMessagePolicy::VerifyMessagePattern,
        )?
        .is_none()
    );
    assert!(
        verify_compile_diagnostics(
            "prog.cob:12:5: error: different wording\n",
            &exp,
            &[],
            &[],
            ExcessErrorPolicy::ForbidExcessErrors,
            DiagnosticMessagePolicy::IgnoreMessagePattern,
        )?
        .is_none()
    );
    assert!(
        verify_compile_diagnostics(
            "prog.cob:12:5: error: different wording\n",
            &exp,
            &[],
            &[],
            ExcessErrorPolicy::ForbidExcessErrors,
            DiagnosticMessagePolicy::VerifyMessagePattern,
        )?
        .is_some()
    );
    Ok(())
}

/// Tests file, line, empty-pattern, and malformed-regex diagnostic cases.
fn verify_diagnostic_mismatch_cases() -> Result<()> {
    let two_diags = vec![
        ExpectedDiagnostic {
            file: String::from("prog.cob"),
            line: Some(12),
            severity: DiagnosticSeverity::Error,
            message_pattern: String::new(),
        },
        ExpectedDiagnostic {
            file: String::from("prog.cob"),
            line: Some(16),
            severity: DiagnosticSeverity::Error,
            message_pattern: String::new(),
        },
        ExpectedDiagnostic {
            file: String::from("prog.cob"),
            line: None,
            severity: DiagnosticSeverity::Warning,
            message_pattern: String::new(),
        },
    ];
    assert!(
        verify_compile_diagnostics(
            "prog.cob:15: warning: second\nprog.cob:12: error: first\nprog.cob:16: error: third\n",
            &two_diags,
            &[],
            &[],
            ExcessErrorPolicy::ForbidExcessErrors,
            DiagnosticMessagePolicy::VerifyMessagePattern,
        )?
        .is_none()
    );

    for (file, line, pat) in [
        ("other.cob", Some(12), ""),
        ("prog.cob", Some(99), ""),
        ("", Some(99), ""),
    ] {
        let spec = vec![ExpectedDiagnostic {
            file: String::from(file),
            line,
            severity: DiagnosticSeverity::Error,
            message_pattern: String::from(pat),
        }];
        assert!(
            verify_compile_diagnostics(
                "prog.cob:12: error: x
",
                &spec,
                &[],
                &[],
                ExcessErrorPolicy::AllowExcessErrors,
                DiagnosticMessagePolicy::VerifyMessagePattern,
            )?
            .is_some()
        );
    }
    let bad_re = vec![ExpectedDiagnostic {
        file: String::new(),
        line: None,
        severity: DiagnosticSeverity::Error,
        message_pattern: String::from("["),
    }];
    assert!(
        verify_compile_diagnostics(
            "a:1: error: x",
            &bad_re,
            &[],
            &[],
            ExcessErrorPolicy::AllowExcessErrors,
            DiagnosticMessagePolicy::VerifyMessagePattern,
        )
        .is_err()
    );
    assert_eq!(
        ExcessErrorPolicy::from_allow_flag(true),
        ExcessErrorPolicy::AllowExcessErrors
    );
    assert_eq!(
        ExcessErrorPolicy::from_allow_flag(false),
        ExcessErrorPolicy::ForbidExcessErrors
    );
    Ok(())
}

/// Exercises all [`OutputMatcher`] arms including `Exact`, `Regex`, `NormalizedLines`, and `Ccvs85Report`.
fn verify_output_variants() -> Result<()> {
    assert_eq!(verify_output(&OutputMatcher::Ignore, b"anything")?, None);
    assert_eq!(
        verify_output(
            &OutputMatcher::Exact(String::from("hi\u{000a}")),
            b"hi
"
        )?,
        None
    );
    assert!(verify_output(&OutputMatcher::Exact(String::from("hi")), b"bye")?.is_some());
    assert_eq!(
        verify_output(&OutputMatcher::Regex(String::from("^ok [0-9]+$")), b"ok 42")?,
        None
    );
    assert!(verify_output(&OutputMatcher::Regex(String::from("^ok$")), b"no")?.is_some());
    assert!(verify_output(&OutputMatcher::Regex(String::from("[")), b"no").is_err());

    assert_eq!(
        normalize_lines(
            "a
b\u{0009}

"
        ),
        "a\u{000a}b"
    );
    assert_eq!(
        verify_output(
            &OutputMatcher::NormalizedLines(String::from("hello\u{000a}")),
            b"hello

"
        )?,
        None
    );
    assert!(verify_output(&OutputMatcher::NormalizedLines(String::from("a")), b"b")?.is_some());

    let ccvs_ok = "PASS NC101A.1
0001 TEST DELETED
INSPECT NC101A.2
";
    assert_eq!(verify_ccvs85_report(ccvs_ok, 1, 1, 1), None);
    assert_eq!(
        verify_output(
            &OutputMatcher::Ccvs85Report {
                expected_pass: 1,
                expected_deleted: 1,
                expected_inspect: 1,
            },
            ccvs_ok.as_bytes()
        )?,
        None
    );
    assert!(
        verify_ccvs85_report(
            "NC101A *FAIL*
",
            0,
            0,
            0
        )
        .is_some()
    );
    assert!(
        verify_ccvs85_report(
            "SUBTEST PASS
    *INSPECT* X
    DELETED ONLY
    ",
            9,
            0,
            1
        )
        .is_some()
    );
    assert!(verify_ccvs85_report(ccvs_ok, 1, 9, 1).is_some());
    assert!(verify_ccvs85_report(ccvs_ok, 1, 1, 9).is_some());

    let footer_ok = concat!(
        "  93 OF 93 TESTS WERE EXECUTED SUCCESSFULLY\n",
        "  1 OF 2 OTHER WERE X\n",
        "  1 OF 2 TESTS NOTWERE\n",
        "  NO TEST(S) FAILED\n",
        "   2 TEST(S) DELETED\n",
        "   1 TEST(S) REQUIRE VISUAL INSPECTION\n",
        "   3 TESTS REQUIRE VISUAL INSPECTION\n",
        "   1 TESTS OTHER VISUAL INSPECTION\n",
        "   1 TESTS REQUIRE NOTVISUAL INSPECTION\n",
        "   0 TEST(S) UNKNOWN\n",
        "  XX OF 10 TESTS WERE IGNORED\n",
        "  XX TESTS REQUIRE VISUAL INSPECTION\n",
        "  trailing line after footer\n"
    );
    assert_eq!(verify_ccvs85_report(footer_ok, 93, 2, 4), None);
    assert!(verify_ccvs85_report("   1 TEST(S) FAILED\n", 0, 0, 0).is_some());
    assert!(verify_ccvs85_report("  IX101A  FAIL*  WRITE-TEST\n", 0, 0, 0).is_some());
    Ok(())
}

/// Tests [`HostCapabilities`], endianness helper, `iconv` probing, and subprocess helpers.
#[test]
fn capabilities_and_subprocess_coverage() -> Result<()> {
    let mut set = BTreeSet::new();
    insert_endian_capability(&mut set, TargetEndianness::BigEndian);
    assert!(set.contains(&Capability::BigEndian));
    insert_endian_capability(&mut set, TargetEndianness::LittleEndian);
    assert!(set.contains(&Capability::LittleEndian));

    let host = HostCapabilities::detect_host();
    assert!(host.supports(&Capability::PosixTools));
    assert!(host.supports(&Capability::CursesPty));
    assert!(host.supports(&Capability::Isam));
    assert!(host.supports(&Capability::Xml));
    assert!(host.supports(&Capability::Json));
    assert!(host.supports(&Capability::IconvCodepage(String::from("UTF-8"))));
    assert!(!host.supports(&Capability::IconvCodepage(String::from(
        "NO_SUCH_CHARSET_999"
    ))));
    assert!(!probe_iconv_with_binary(
        Path::new("/nonexistent/iconv-bin"),
        "UTF-8"
    ));

    let explicit =
        HostCapabilities::from_explicit(&[Capability::Xml], IconvProbePolicy::ExplicitOnly);
    assert!(explicit.supports(&Capability::Xml));
    assert!(!explicit.supports(&Capability::IconvCodepage(String::from("UTF-8"))));
    assert_eq!(
        explicit.first_missing(&[Capability::Xml, Capability::Isam]),
        Some(&Capability::Isam)
    );
    assert_eq!(explicit.first_missing(&[Capability::Xml]), None);

    assert_eq!(read_optional_pipe::<&[u8]>(None), Vec::<u8>::new());
    assert_eq!(read_optional_pipe(Some(&b"abc"[..])), b"abc");

    let tmp = std::env::temp_dir().join(format!("coblang-proc-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    assert!(
        run_command_with_timeout(
            &CommandSpec {
                program: Path::new("/nonexistent/coblang-no-bin"),
                args: &[],
                cwd: &tmp,
                env: &[],
                stdin_bytes: &[],
                timeout_ms: 500,
                use_pty: false,
            },
            "bad-spawn"
        )
        .is_err()
    );

    std::fs::write(
        tmp.join("ok.c"),
        "int cob_helper(void) { return 42; }\u{000a}",
    )?;
    std::fs::write(tmp.join("bad.c"), "syntax error here;\u{000a}")?;
    let cc_obj = CompileCSpec {
        sources: vec![String::from("ok.c")],
        shared: false,
        output: String::from("ok.o"),
    };
    assert_eq!(
        compile_c_companion(&cc_obj, &tmp, Path::new("clang"), 10_000, "c1")?,
        None
    );
    let cc_so = CompileCSpec {
        sources: vec![String::from("ok.c")],
        shared: true,
        output: String::from("libok.dylib"),
    };
    assert_eq!(
        compile_c_companion(&cc_so, &tmp, Path::new("clang"), 10_000, "c2")?,
        None
    );
    let cc_bad = CompileCSpec {
        sources: vec![String::from("bad.c")],
        shared: false,
        output: String::from("bad.o"),
    };
    assert!(compile_c_companion(&cc_bad, &tmp, Path::new("clang"), 10_000, "c3")?.is_some());

    let slow_cc = tmp.join("slow_cc.sh");
    std::fs::write(&slow_cc, "#!/bin/sh\u{000a}sleep 5\u{000a}")?;
    std::fs::set_permissions(&slow_cc, std::fs::Permissions::from_mode(0o755))?;
    assert!(compile_c_companion(&cc_obj, &tmp, &slow_cc, 40, "c4")?.is_some());

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Builds a mock compiler shell script used to exercise every canary scenario deterministically.
fn write_mock_compiler(dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let comp = dir.join("mock-coblang.sh");
    let script = r#"#!/bin/sh
MODE=exe; OUT=a.out; SRC=
while [ $# -gt 0 ]; do
  case "$1" in
    -fsyntax-only) MODE=syntax ;;
    -E|-S|-emit-llvm|-c|-m|-shared) MODE=other ;;
    -o) shift; OUT="$1" ;;
    -I|-L|-l|-ext|-copyext|-dialect) shift ;;
    -*) ;;
    *) if [ -z "$SRC" ]; then SRC="$1"; fi ;;
  esac
  shift
done
if grep -q TRIGGER_TIMEOUT_COMPILE "$SRC" 2>/dev/null; then sleep 5; exit 0; fi
if grep -q TRIGGER_CRASH_COMPILE "$SRC" 2>/dev/null; then kill -ABRT $$; fi
if grep -q TRIGGER_UNIMPL "$SRC" 2>/dev/null; then echo UNIMPLEMENTED >&2; exit 78; fi
if grep -q TRIGGER_SILENT_FAIL "$SRC" 2>/dev/null; then exit 1; fi
if grep -q TRIGGER_SYNTAX_ERR "$SRC" 2>/dev/null; then echo "$SRC:2:5: error: invalid verb" >&2; exit 1; fi
if [ "$MODE" = syntax ] || [ "$MODE" = other ]; then touch "$OUT"; exit 0; fi
if grep -q TRIGGER_RUNTIME_LOOP "$SRC" 2>/dev/null; then
  printf '%s\n' '#!/bin/sh' 'sleep 5' > "$OUT"
elif grep -q TRIGGER_RUNTIME_CRASH "$SRC" 2>/dev/null; then
  printf '%s\n' '#!/bin/sh' 'kill -ABRT $$' > "$OUT"
elif grep -q TRIGGER_FILE_ENV "$SRC" 2>/dev/null; then
  printf '%s\n' '#!/bin/sh' 'read INP' 'echo "GOT:$INP:$MY_VAR"' 'echo WRITTEN > "$DD_OUT"' > "$OUT"
else
  printf '%s\n' '#!/bin/sh' 'echo "HELLO CANARY"' > "$OUT"
fi
chmod +x "$OUT"
exit 0
"#;
    std::fs::write(&comp, script)?;
    std::fs::set_permissions(&comp, std::fs::Permissions::from_mode(0o755))?;

    let cobcrun = dir.join("mock-cobcrun.sh");
    let cr_script = r#"#!/bin/sh
echo "MOD:$1:$2"
"#;
    std::fs::write(&cobcrun, cr_script)?;
    std::fs::set_permissions(&cobcrun, std::fs::Permissions::from_mode(0o755))?;
    Ok((comp, cobcrun))
}

/// Exercises all Phase 1.3 canary scenarios against the mock compiler and real `cobc` oracle.
#[test]
fn canary_suite_end_to_end_and_real_cobc_oracle() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-canary-{}", std::process::id()));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    std::fs::create_dir_all(&tmp)?;
    let (mock_comp, mock_cobcrun) = write_mock_compiler(&tmp)?;

    let manifests = build_canary_manifests();
    let cfg = RunnerConfig {
        backend: CompilerBackend::External {
            compiler_bin: mock_comp,
            cobcrun_bin: Some(mock_cobcrun),
            flavor: DriverFlavor::Coblang,
            diagnostic_policy: DiagnosticMessagePolicy::VerifyMessagePattern,
            compile_timeout_ms: 1_500,
        },
        sandbox_root: tmp.join("sbx"),
        sandbox_retention: SandboxRetention::PreserveOnFailure,
        c_compiler: PathBuf::from("clang"),
        capabilities: HostCapabilities::detect_host(),
        jobs: 4,
    };

    assert_eq!(execute_suite_parallel(&[], &cfg)?, Vec::new());
    let records = execute_suite_parallel(&manifests, &cfg)?;
    let by_id: BTreeMap<String, TestOutcome> =
        records.iter().map(|r| (r.id.clone(), r.outcome)).collect();

    for (id, expected) in [
        ("canary/syntax-ok", TestOutcome::Pass),
        ("canary/syntax-err", TestOutcome::Pass),
        ("canary/env-file-io", TestOutcome::Pass),
        ("canary/c-companion", TestOutcome::Pass),
        ("canary/cobcrun-mod", TestOutcome::Pass),
        ("canary/skip-explicit", TestOutcome::Skip),
        ("canary/skip-cap", TestOutcome::Skip),
        ("canary/unimpl", TestOutcome::XFail),
        ("canary/xfail-expected", TestOutcome::XFail),
        ("canary/xpass-detected", TestOutcome::XPass),
        ("canary/runtime-timeout", TestOutcome::Fail),
        ("canary/runtime_crash", TestOutcome::Fail),
        ("canary/compile-timeout", TestOutcome::Fail),
        ("canary/compile-crash", TestOutcome::Fail),
    ] {
        assert_eq!(by_id.get(id), Some(&expected), "mismatch on {id}");
    }

    exercise_driver_cli_builder();
    exercise_scaffold_engine_branches(&tmp)?;
    exercise_external_mismatch_branches(&cfg)?;
    exercise_real_cobc_oracle(&tmp)?;
    write_committed_canary_bundle()?;

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Constructs the canary [`TestManifest`] suite covering all Phase 1.3 scenarios.
fn build_canary_manifests() -> Vec<TestManifest> {
    let mut err_spec = base_cobol_spec(CompilationMode::SyntaxOnly, "prog.cob", "bad.o");
    err_spec.expected_success = false;
    err_spec.expected_diagnostics.push(ExpectedDiagnostic {
        file: String::from("prog.cob"),
        line: Some(2),
        severity: DiagnosticSeverity::Error,
        message_pattern: String::from("invalid verb"),
    });

    let mut list = vec![
        simple_canary(
            "canary/syntax-ok",
            "OK\u{000a}",
            None,
            Vec::new(),
            None,
            vec![TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::SyntaxOnly,
                "prog.cob",
                "p.o",
            ))],
        ),
        simple_canary(
            "canary/syntax-err",
            "ID.\u{000a}TRIGGER_SYNTAX_ERR\u{000a}",
            None,
            Vec::new(),
            None,
            vec![TestStep::CompileCobol(err_spec)],
        ),
        build_env_file_io_canary(),
        build_c_companion_canary(),
        build_cobcrun_canary(),
    ];
    list.extend(build_outcome_and_hazard_canaries());
    list
}

/// Builds the `cobcrun` dynamic module canary.
fn build_cobcrun_canary() -> TestManifest {
    simple_canary(
        "canary/cobcrun-mod",
        "OK\u{000a}",
        None,
        Vec::new(),
        None,
        vec![
            TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::CompileModule,
                "prog.cob",
                "M.so",
            )),
            TestStep::RunBinary(RunBinarySpec {
                target: String::from("M"),
                via_cobcrun: true,
                args: vec![String::from("A1")],
                stdin: Vec::new(),
                env: Vec::new(),
                expected_exit_code: 0,
                stdout: OutputMatcher::Exact(String::from("MOD:M:A1\u{000a}")),
                stderr: OutputMatcher::Ignore,
                expected_files: Vec::new(),
                timeout_ms: 2_000,
            }),
        ],
    )
}

/// Builds skip, `XFAIL`, `XPASS`, timeout, and crash canaries.
fn build_outcome_and_hazard_canaries() -> Vec<TestManifest> {
    let mut explicit_skip = simple_canary(
        "canary/skip-explicit",
        "OK",
        None,
        Vec::new(),
        None,
        Vec::new(),
    );
    explicit_skip.skip_reason = Some(String::from("interactive TUI"));

    vec![
        explicit_skip,
        simple_canary(
            "canary/skip-cap",
            "OK",
            None,
            vec![Capability::IconvCodepage(String::from("NO_SUCH_CP_XYZ"))],
            None,
            Vec::new(),
        ),
        simple_canary(
            "canary/unimpl",
            "TRIGGER_UNIMPL\u{000a}",
            None,
            Vec::new(),
            None,
            vec![TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::BuildExecutable,
                "prog.cob",
                "a.out",
            ))],
        ),
        simple_canary(
            "canary/xfail-expected",
            "TRIGGER_SILENT_FAIL\u{000a}",
            Some("known bug"),
            Vec::new(),
            None,
            vec![TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::SyntaxOnly,
                "prog.cob",
                "p.o",
            ))],
        ),
        simple_canary(
            "canary/xpass-detected",
            "OK\u{000a}",
            Some("fixed!"),
            Vec::new(),
            None,
            vec![TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::SyntaxOnly,
                "prog.cob",
                "p.o",
            ))],
        ),
        build_run_hazard_canary("canary/runtime-timeout", "TRIGGER_RUNTIME_LOOP\u{000a}", 80),
        build_run_hazard_canary(
            "canary/runtime_crash",
            "TRIGGER_RUNTIME_CRASH\u{000a}",
            2_000,
        ),
        simple_canary(
            "canary/compile-timeout",
            "TRIGGER_TIMEOUT_COMPILE\u{000a}",
            Some("must FAIL"),
            Vec::new(),
            None,
            vec![TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::SyntaxOnly,
                "prog.cob",
                "p.o",
            ))],
        ),
        simple_canary(
            "canary/compile-crash",
            "TRIGGER_CRASH_COMPILE\u{000a}",
            Some("must FAIL"),
            Vec::new(),
            None,
            vec![TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::SyntaxOnly,
                "prog.cob",
                "p.o",
            ))],
        ),
    ]
}

/// Helper building a concise single-file canary [`TestManifest`].
fn simple_canary(
    id: &str,
    src_text: &str,
    xfail: Option<&str>,
    caps: Vec<Capability>,
    torture: Option<Vec<OptimizationLevel>>,
    steps: Vec<TestStep>,
) -> TestManifest {
    TestManifest {
        id: String::from(id),
        suite: SuiteId::Canary,
        provenance: canary_prov(id),
        files: vec![WorkspaceFile {
            relative_path: String::from("prog.cob"),
            contents: src_text.as_bytes().to_vec(),
        }],
        steps,
        torture_optimizations: torture.unwrap_or_default(),
        required_capabilities: caps,
        xfail_reason: xfail.map(String::from),
        skip_reason: None,
        feature_tags: vec![FeatureTag::StageParseSema],
        equivalent_tests: Vec::new(),
    }
}

/// Builds a multi-step canary exercising nested workspace dirs, `WriteFile`, `stdin`, `env`, torture opts, and output files.
fn build_env_file_io_canary() -> TestManifest {
    let mut m = simple_canary(
        "canary/env-file-io",
        "TRIGGER_FILE_ENV\u{000a}",
        None,
        Vec::new(),
        Some(vec![OptimizationLevel::O0, OptimizationLevel::O2]),
        vec![
            TestStep::WriteFile(WorkspaceFile {
                relative_path: String::from("sub/extra.txt"),
                contents: b"seed".to_vec(),
            }),
            TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::BuildExecutable,
                "prog.cob",
                "app.bin",
            )),
            TestStep::RunBinary(RunBinarySpec {
                target: String::from("./app.bin"),
                via_cobcrun: false,
                args: Vec::new(),
                stdin: "STDIN_VAL\u{000a}".as_bytes().to_vec(),
                env: vec![
                    (String::from("MY_VAR"), String::from("ENV_VAL")),
                    (String::from("DD_OUT"), String::from("produced.txt")),
                ],
                expected_exit_code: 0,
                stdout: OutputMatcher::Regex(String::from("GOT:STDIN_VAL:ENV_VAL")),
                stderr: OutputMatcher::Exact(String::new()),
                expected_files: vec![(
                    String::from("produced.txt"),
                    OutputMatcher::NormalizedLines(String::from("WRITTEN")),
                )],
                timeout_ms: 2_000,
            }),
        ],
    );
    m.files.push(WorkspaceFile {
        relative_path: String::from("inc/copy.cpy"),
        contents: b"*> copybook".to_vec(),
    });
    m
}

/// Builds a canary compiling a companion `.c` helper with `clang` before compiling COBOL.
fn build_c_companion_canary() -> TestManifest {
    let mut m = simple_canary(
        "canary/c-companion",
        "OK\u{000a}",
        None,
        vec![Capability::CCompiler],
        None,
        vec![
            TestStep::CompileC(CompileCSpec {
                sources: vec![String::from("helper.c")],
                shared: false,
                output: String::from("helper.o"),
            }),
            TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::BuildExecutable,
                "prog.cob",
                "app.bin",
            )),
            TestStep::RunBinary(RunBinarySpec {
                target: String::from("./app.bin"),
                via_cobcrun: false,
                args: Vec::new(),
                stdin: Vec::new(),
                env: Vec::new(),
                expected_exit_code: 0,
                stdout: OutputMatcher::Exact(String::from("HELLO CANARY\u{000a}")),
                stderr: OutputMatcher::Ignore,
                expected_files: Vec::new(),
                timeout_ms: 2_000,
            }),
        ],
    );
    m.files.push(WorkspaceFile {
        relative_path: String::from("helper.c"),
        contents: "int c_helper(void) { return 7; }\u{000a}"
            .as_bytes()
            .to_vec(),
    });
    m
}

/// Builds a canary whose compiled executable either loops past timeout or aborts with a signal.
fn build_run_hazard_canary(id: &str, trigger: &str, timeout_ms: u64) -> TestManifest {
    simple_canary(
        id,
        trigger,
        Some("hazards must still FAIL"),
        Vec::new(),
        None,
        vec![
            TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::BuildExecutable,
                "prog.cob",
                "haz.bin",
            )),
            TestStep::RunBinary(RunBinarySpec {
                target: String::from("./haz.bin"),
                via_cobcrun: false,
                args: Vec::new(),
                stdin: Vec::new(),
                env: Vec::new(),
                expected_exit_code: 0,
                stdout: OutputMatcher::Ignore,
                stderr: OutputMatcher::Ignore,
                expected_files: Vec::new(),
                timeout_ms,
            }),
        ],
    )
}

/// Exercises `build_driver_cli_args` across all `DriverFlavor`, `CompilationMode`, `Dialect`, and `TextEncoding` variants.
fn exercise_driver_cli_builder() {
    for flavor in [
        DriverFlavor::Gcobol,
        DriverFlavor::Cobc,
        DriverFlavor::Coblang,
    ] {
        for mode in [
            CompilationMode::PreprocessOnly,
            CompilationMode::SyntaxOnly,
            CompilationMode::EmitAssembly,
            CompilationMode::EmitLlvmIr,
            CompilationMode::CompileObject,
            CompilationMode::CompileModule,
            CompilationMode::BuildExecutable,
        ] {
            for dialect in [
                Dialect::Default,
                Dialect::Ibm,
                Dialect::MicroFocus,
                Dialect::Gnu,
                Dialect::Cobol2023,
                Dialect::Cobol85,
            ] {
                let mut spec = base_cobol_spec(mode, "a.cob", "out");
                spec.dialect = dialect;
                spec.format = match mode {
                    CompilationMode::SyntaxOnly => SourceFormat::Fixed,
                    CompilationMode::PreprocessOnly => SourceFormat::Auto,
                    _ => SourceFormat::Free,
                };
                spec.encoding = match mode {
                    CompilationMode::EmitAssembly => TextEncoding::Ebcdic,
                    CompilationMode::EmitLlvmIr => TextEncoding::Codepage(String::from("CP1140")),
                    _ => TextEncoding::Ascii,
                };
                spec.include_dirs = vec![String::from("inc")];
                spec.copy_exts = vec![String::from(".cpy")];
                spec.library_dirs = vec![String::from("lib")];
                spec.libraries = vec![String::from("m")];
                if mode == CompilationMode::CompileObject {
                    spec.output.clear();
                }
                let built = build_driver_cli_args(flavor, &spec, OptimizationLevel::Os);
                assert_ne!(built, Vec::<String>::new());
            }
        }
    }
}

/// Exercises `InProcessScaffold` execution paths (`XFAIL`, missing file `FAIL`, and `cobcrun` `XFAIL`).
fn exercise_scaffold_engine_branches(tmp: &Path) -> Result<()> {
    let scaf_cfg = RunnerConfig {
        backend: CompilerBackend::InProcessScaffold,
        sandbox_root: tmp.join("sbx"),
        sandbox_retention: SandboxRetention::CleanAlways,
        c_compiler: PathBuf::from("clang"),
        capabilities: HostCapabilities::detect_host(),
        jobs: 1,
    };
    let pre_existing = scaf_cfg
        .sandbox_root
        .join(format!("sbx-{}-0-canary_pre", std::process::id()));
    std::fs::create_dir_all(&pre_existing)?;

    let r_unimpl = execute_single_test(
        &simple_canary(
            "canary/pre",
            "OK",
            None,
            Vec::new(),
            None,
            vec![TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::SyntaxOnly,
                "prog.cob",
                "a.o",
            ))],
        ),
        &scaf_cfg,
        0,
    )?;
    assert_eq!(r_unimpl.outcome, TestOutcome::XFail);

    let r_missing = execute_single_test(
        &simple_canary(
            "canary/missing-src",
            "OK",
            None,
            Vec::new(),
            None,
            vec![TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::SyntaxOnly,
                "nope.cob",
                "a.o",
            ))],
        ),
        &scaf_cfg,
        1,
    )?;
    assert_eq!(r_missing.outcome, TestOutcome::Fail);

    let r_cobcrun = execute_single_test(
        &simple_canary(
            "canary/scaf-cobcrun",
            "OK",
            None,
            Vec::new(),
            None,
            vec![TestStep::RunBinary(RunBinarySpec {
                target: String::from("MOD"),
                via_cobcrun: true,
                args: Vec::new(),
                stdin: Vec::new(),
                env: Vec::new(),
                expected_exit_code: 0,
                stdout: OutputMatcher::Ignore,
                stderr: OutputMatcher::Ignore,
                expected_files: Vec::new(),
                timeout_ms: 1_000,
            })],
        ),
        &scaf_cfg,
        2,
    )?;
    assert_eq!(r_cobcrun.outcome, TestOutcome::XFail);
    Ok(())
}

/// Covers failed C step and compile diagnostic miss on external backend.
fn exercise_compile_mismatch_branches(ext_cfg: &RunnerConfig) -> Result<()> {
    let bad_c = simple_canary(
        "canary/bad-c",
        "OK",
        None,
        Vec::new(),
        None,
        vec![TestStep::CompileC(CompileCSpec {
            sources: vec![String::from("missing.c")],
            shared: false,
            output: String::from("m.o"),
        })],
    );
    assert_eq!(
        execute_single_test(&bad_c, ext_cfg, 10)?.outcome,
        TestOutcome::Fail
    );

    let mut diag_spec = base_cobol_spec(CompilationMode::SyntaxOnly, "prog.cob", "a.o");
    diag_spec.expected_diagnostics.push(ExpectedDiagnostic {
        file: String::from("prog.cob"),
        line: Some(1),
        severity: DiagnosticSeverity::Warning,
        message_pattern: String::from("never"),
    });
    let diag_miss = simple_canary(
        "canary/diag-miss",
        "OK",
        None,
        Vec::new(),
        None,
        vec![TestStep::CompileCobol(diag_spec)],
    );
    assert_eq!(
        execute_single_test(&diag_miss, ext_cfg, 11)?.outcome,
        TestOutcome::Fail
    );
    Ok(())
}

/// Covers runtime exit, stdout, stderr, and expected-file mismatches.
fn exercise_external_mismatch_branches(ext_cfg: &RunnerConfig) -> Result<()> {
    exercise_compile_mismatch_branches(ext_cfg)?;

    for (slot, code, so, se, files) in [
        (
            12,
            99,
            OutputMatcher::Ignore,
            OutputMatcher::Ignore,
            Vec::new(),
        ),
        (
            13,
            0,
            OutputMatcher::Exact(String::from("NOPE")),
            OutputMatcher::Ignore,
            Vec::new(),
        ),
        (
            14,
            0,
            OutputMatcher::Ignore,
            OutputMatcher::Exact(String::from("NOPE")),
            Vec::new(),
        ),
        (
            15,
            0,
            OutputMatcher::Ignore,
            OutputMatcher::Ignore,
            vec![(String::from("missing.txt"), OutputMatcher::Ignore)],
        ),
        (
            16,
            0,
            OutputMatcher::Ignore,
            OutputMatcher::Ignore,
            vec![(
                String::from("prog.cob"),
                OutputMatcher::Exact(String::from("WRONG")),
            )],
        ),
    ] {
        let m = simple_canary(
            "canary/mismatch",
            "OK\u{000a}",
            None,
            Vec::new(),
            None,
            vec![
                TestStep::CompileCobol(base_cobol_spec(
                    CompilationMode::BuildExecutable,
                    "prog.cob",
                    "a.out",
                )),
                TestStep::RunBinary(RunBinarySpec {
                    target: String::from("./a.out"),
                    via_cobcrun: false,
                    args: Vec::new(),
                    stdin: Vec::new(),
                    env: Vec::new(),
                    expected_exit_code: code,
                    stdout: so,
                    stderr: se,
                    expected_files: files,
                    timeout_ms: 1_000,
                }),
            ],
        );
        assert_eq!(
            execute_single_test(&m, ext_cfg, slot)?.outcome,
            TestOutcome::Fail
        );
    }

    exercise_cobcrun_path_fallback(ext_cfg)
}

/// Verifies `via_cobcrun` falls back to invoking `"cobcrun"` from `PATH` when `cobcrun_bin` is `None`.
fn exercise_cobcrun_path_fallback(ext_cfg: &RunnerConfig) -> Result<()> {
    let mut no_cr_cfg = ext_cfg.clone();
    if let CompilerBackend::External { cobcrun_bin, .. } = &mut no_cr_cfg.backend {
        *cobcrun_bin = None;
    }
    let cr_default = simple_canary(
        "canary/cr-path",
        "OK",
        None,
        Vec::new(),
        None,
        vec![TestStep::RunBinary(RunBinarySpec {
            target: String::from("--version"),
            via_cobcrun: true,
            args: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            expected_exit_code: 0,
            stdout: OutputMatcher::Regex(String::from("cobcrun")),
            stderr: OutputMatcher::Ignore,
            expected_files: Vec::new(),
            timeout_ms: 2_000,
        })],
    );
    assert_eq!(
        execute_single_test(&cr_default, &no_cr_cfg, 17)?.outcome,
        TestOutcome::Pass
    );
    Ok(())
}

/// Validates a real free-form COBOL program against the host `cobc` 3.2.0 oracle!
fn exercise_real_cobc_oracle(tmp: &Path) -> Result<()> {
    let cobol_hello = concat!(
        "IDENTIFICATION DIVISION.
",
        "PROGRAM-ID. HELLO-ORACLE.
",
        "PROCEDURE DIVISION.
",
        "    DISPLAY 'HELLO FROM COBC ORACLE'.
",
        "    STOP RUN.
"
    );
    let m = simple_canary(
        "canary/oracle-hello",
        cobol_hello,
        None,
        Vec::new(),
        None,
        vec![
            TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::BuildExecutable,
                "prog.cob",
                "hello_oracle",
            )),
            TestStep::RunBinary(RunBinarySpec {
                target: String::from("./hello_oracle"),
                via_cobcrun: false,
                args: Vec::new(),
                stdin: Vec::new(),
                env: Vec::new(),
                expected_exit_code: 0,
                stdout: OutputMatcher::Exact(String::from("HELLO FROM COBC ORACLE\u{000a}")),
                stderr: OutputMatcher::Ignore,
                expected_files: Vec::new(),
                timeout_ms: 10_000,
            }),
        ],
    );
    let cfg = RunnerConfig {
        backend: resolve_oracle_backend(Some("cobc"))?,
        sandbox_root: tmp.join("oracle-sbx"),
        sandbox_retention: SandboxRetention::CleanAlways,
        c_compiler: PathBuf::from("clang"),
        capabilities: HostCapabilities::detect_host(),
        jobs: 1,
    };
    let rec = execute_single_test(&m, &cfg, 0)?;
    assert_eq!(
        rec.outcome,
        TestOutcome::Pass,
        "oracle detail: {}",
        rec.detail
    );
    Ok(())
}

/// Materializes `tests/canary/canary.bundle` in the workspace for CLI and `llvm-lit` usage.
fn write_committed_canary_bundle() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let canary_dir = root.join("tests/canary");
    std::fs::create_dir_all(&canary_dir)?;
    let mut skip_case = simple_canary(
        "canary/scaffold-skip",
        "IDENTIFICATION DIVISION.\u{000a}",
        None,
        Vec::new(),
        None,
        Vec::new(),
    );
    skip_case.skip_reason = Some(String::from("canary skip verification"));
    let manifests = vec![
        simple_canary(
            "canary/scaffold-syntax",
            "IDENTIFICATION DIVISION.\u{000a}PROGRAM-ID. C1.\u{000a}",
            None,
            Vec::new(),
            None,
            vec![TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::SyntaxOnly,
                "prog.cob",
                "c1.o",
            ))],
        ),
        skip_case,
    ];
    std::fs::write(
        canary_dir.join("canary.bundle"),
        encode_manifest_bundle(&manifests),
    )?;
    Ok(())
}

/// Tests `ratchet.rs`, `cli.rs`, `emit_lit_wrappers`, and the `coblang-test` binary.
#[test]
fn ratchet_cli_lit_and_binary_coverage() -> Result<()> {
    let mut counts = OutcomeCounts::default();
    for o in [
        TestOutcome::Pass,
        TestOutcome::XFail,
        TestOutcome::Fail,
        TestOutcome::XPass,
        TestOutcome::Skip,
    ] {
        counts.record(o);
    }
    assert_eq!(counts.total, 5);

    let recs = vec![
        TestExecutionRecord {
            id: String::from("t/pass"),
            suite: SuiteId::Canary,
            outcome: TestOutcome::Pass,
            detail: String::new(),
            feature_tags: vec![FeatureTag::StageParseSema],
        },
        TestExecutionRecord {
            id: String::from("t/xfail"),
            suite: SuiteId::Gcobol,
            outcome: TestOutcome::XFail,
            detail: String::from("unimpl"),
            feature_tags: vec![FeatureTag::FeatureArith],
        },
    ];
    let snap = snapshot_expectations(&recs);
    let encoded = encode_expectations(&snap);
    let decoded = decode_expectations(&format!("{encoded}\u{000a}"))?;
    assert_eq!(decoded, snap);
    assert!(decode_expectations("malformed_only_one_col").is_err());
    assert!(decode_expectations("id\u{0009}BAD\u{0009}note").is_err());

    let mut with_new = recs.clone();
    with_new.push(TestExecutionRecord {
        id: String::from("t/brand_new"),
        suite: SuiteId::Canary,
        outcome: TestOutcome::Pass,
        detail: String::new(),
        feature_tags: Vec::new(),
    });
    verify_ratchet(&with_new, Some(&snap))?;
    let md = render_summary_markdown(&recs);
    assert!(md.contains("`canary`"));
    assert!(md.contains("`feature:arith`"));
    let js = render_summary_json(&recs);
    assert!(js.contains(r#""canary":{"total":1"#));

    for (id, suite, outcome) in [
        ("t/fail", SuiteId::Canary, TestOutcome::Fail),
        ("t/xpass", SuiteId::Canary, TestOutcome::XPass),
        ("t/pass", SuiteId::Canary, TestOutcome::XFail),
        ("t/xfail", SuiteId::Gcobol, TestOutcome::Pass),
    ] {
        let bad = TestExecutionRecord {
            id: String::from(id),
            suite,
            outcome,
            detail: String::from("delta"),
            feature_tags: Vec::new(),
        };
        assert!(verify_ratchet(&[bad], Some(&snap)).is_err());
    }

    exercise_cli_parser_errors()?;
    exercise_cli_dispatch_and_binary()
}

/// Exercises invalid option combinations in `parse_runner_cli` and `resolve_oracle_backend`.
fn exercise_cli_parser_errors() -> Result<()> {
    let s = |xs: &[&str]| -> Vec<String> { xs.iter().map(|x| String::from(*x)).collect() };
    assert_eq!(parse_runner_cli(&s(&["--help"]))?, RunnerCommand::Help);
    assert!(matches!(
        resolve_oracle_backend(Some("gcobol"))?,
        CompilerBackend::External { .. }
    ));
    assert!(resolve_oracle_backend(Some("bad")).is_err());

    for invalid in [
        &[][..],
        &["nope"][..],
        &["run"][..],
        &["run", "--bundle"][..],
        &["run", "--bundle-dir"][..],
        &["run", "--baseline"][..],
        &["run", "--write-baseline"][..],
        &["run", "--oracle"][..],
        &["run", "--filter"][..],
        &["run", "--tag"][..],
        &["run", "--tag", "bad:tag", "--bundle", "a"][..],
        &["run", "--jobs"][..],
        &["run", "--jobs", "x", "--bundle", "a"][..],
        &["run", "--bad"][..],
        &["run-one"][..],
        &["run-one", "--bundle"][..],
        &["run-one", "--bundle", "a", "--id"][..],
        &["run-one", "--bundle", "a", "--bad"][..],
        &["summary"][..],
        &["summary", "--bundle"][..],
        &["summary", "--bundle-dir"][..],
        &["summary", "--output"][..],
        &["summary", "--json"][..],
        &["summary", "--filter"][..],
        &["summary", "--tag"][..],
        &["summary", "--tag", "bad:tag", "--bundle", "a"][..],
        &["summary", "--jobs"][..],
        &["summary", "--jobs", "x", "--bundle", "a"][..],
        &["summary", "--bad"][..],
        &["emit-lit"][..],
        &["emit-lit", "--bundle"][..],
        &["emit-lit", "--bundle", "a", "--out-dir"][..],
        &["emit-lit", "--bad"][..],
    ] {
        assert!(parse_runner_cli(&s(invalid)).is_err());
    }
    Ok(())
}

/// Writes a 2-manifest fixture bundle inside `tmp` for CLI subcommand testing.
fn build_cli_fixture_bundle(tmp: &Path) -> Result<(PathBuf, Vec<TestManifest>)> {
    let bundle = tmp.join("b.bundle");
    let mut skip_m = simple_canary("flat_skip", "OK", Some("xf"), Vec::new(), None, Vec::new());
    skip_m.skip_reason = Some(String::from("sk"));
    let mut tagged = simple_canary(
        "sub/t1",
        "OK",
        None,
        Vec::new(),
        None,
        vec![TestStep::CompileCobol(base_cobol_spec(
            CompilationMode::SyntaxOnly,
            "prog.cob",
            "a.o",
        ))],
    );
    tagged.feature_tags = vec![FeatureTag::StageParseSema];
    let mut untagged = simple_canary(
        "sub/t2",
        "OK",
        None,
        Vec::new(),
        None,
        vec![TestStep::CompileCobol(base_cobol_spec(
            CompilationMode::SyntaxOnly,
            "prog.cob",
            "b.o",
        ))],
    );
    untagged.feature_tags = vec![FeatureTag::FeatureArith];
    let manifests = vec![tagged, untagged, skip_m];
    std::fs::write(&bundle, encode_manifest_bundle(&manifests))?;
    Ok((bundle, manifests))
}

/// Exercises `run_cli` subcommands (`run`, `run-one`, `summary`, `emit-lit`) and `CARGO_BIN_EXE_coblang-test`.
fn exercise_cli_dispatch_and_binary() -> Result<()> {
    let s = |xs: &[&str]| -> Vec<String> { xs.iter().map(|x| String::from(*x)).collect() };
    let tmp = std::env::temp_dir().join(format!("coblang-cli-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let (bundle, manifests) = build_cli_fixture_bundle(&tmp)?;

    let base_file = tmp.join("base.exp");
    let sum_file = tmp.join("sum.md");
    let json_file = tmp.join("sum.json");
    let lit_dir = tmp.join("lit");
    std::fs::create_dir_all(&lit_dir)?;
    std::fs::write(tmp.join("notes.txt"), "non-bundle\u{000a}")?;
    let tmp_str = tmp.to_string_lossy().to_string();
    let b_str = bundle.to_string_lossy().to_string();
    let base_str = base_file.to_string_lossy().to_string();
    let sum_str = sum_file.to_string_lossy().to_string();
    let json_str = json_file.to_string_lossy().to_string();
    let lit_str = lit_dir.to_string_lossy().to_string();

    let mut out = Vec::new();
    let mut err = Vec::new();
    for (args, code) in [
        (s(&["help"]), 0_u8),
        (s(&[]), 2),
        (
            s(&[
                "run",
                "--bundle-dir",
                &tmp_str,
                "--write-baseline",
                &base_str,
                "-j",
                "2",
            ]),
            0,
        ),
        (
            s(&[
                "run",
                "--bundle",
                &b_str,
                "--baseline",
                &base_str,
                "--ratchet",
            ]),
            0,
        ),
        (s(&["run", "--bundle", &b_str, "--ratchet"]), 0),
        (s(&["run", "--bundle", &b_str, "--oracle", "bad"]), 1),
        (s(&["run-one", "--bundle", &b_str, "--id", "sub/t1"]), 0),
        (s(&["run-one", "--bundle", &b_str, "--id", "missing"]), 1),
        (
            s(&[
                "summary",
                "--bundle-dir",
                &tmp_str,
                "--output",
                &sum_str,
                "--json",
                &json_str,
                "-j",
                "2",
            ]),
            0,
        ),
        (
            s(&[
                "summary",
                "--bundle",
                &b_str,
                "--filter",
                "sub/",
                "--tag",
                "stage:parse-sema",
            ]),
            0,
        ),
        (
            s(&[
                "run",
                "--bundle",
                &b_str,
                "--filter",
                "sub/",
                "--tag",
                "stage:parse-sema",
            ]),
            0,
        ),
        (
            s(&["emit-lit", "--bundle", &b_str, "--out-dir", &lit_str]),
            0,
        ),
    ] {
        assert_eq!(run_cli(&args, &tmp, &mut out, &mut err), code);
    }
    assert_eq!(emit_lit_wrappers(&manifests, &bundle, &lit_dir)?, 3);

    let bin_out = Command::new(env!("CARGO_BIN_EXE_coblang-test"))
        .arg("--help")
        .output()?;
    assert!(bin_out.status.success());

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Writes an executable shell script at `dir.join(name)` and returns its path.
fn write_exec_script(dir: &Path, name: &str, body: &str) -> Result<PathBuf> {
    let path = dir.join(name);
    std::fs::write(&path, body)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(path)
}

/// Builds synthetic `UNIMPL`, standalone `./a.out`, and `via_cobcrun` manifests for `CoblangCompiled` testing.
fn build_compiled_backend_manifests() -> (TestManifest, TestManifest, TestManifest) {
    let unimpl_m = simple_canary(
        "comp/unimpl",
        "UNIMPL",
        None,
        Vec::new(),
        None,
        vec![TestStep::CompileCobol(base_cobol_spec(
            CompilationMode::SyntaxOnly,
            "prog.cob",
            "a.o",
        ))],
    );
    let exe_m = simple_canary(
        "comp/exe",
        "STANDALONE",
        None,
        Vec::new(),
        None,
        vec![
            TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::BuildExecutable,
                "prog.cob",
                "a.out",
            )),
            TestStep::RunBinary(RunBinarySpec {
                target: String::from("./a.out"),
                via_cobcrun: false,
                args: Vec::new(),
                stdin: Vec::new(),
                env: Vec::new(),
                expected_exit_code: 0,
                stdout: OutputMatcher::Exact(String::from("EXE_OK")),
                stderr: OutputMatcher::Ignore,
                expected_files: Vec::new(),
                timeout_ms: 2_000,
            }),
        ],
    );
    let mod_m = simple_canary(
        "comp/mod",
        "OK",
        None,
        Vec::new(),
        None,
        vec![
            TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::CompileModule,
                "prog.cob",
                "MOD.so",
            )),
            TestStep::RunBinary(RunBinarySpec {
                target: String::from("MOD"),
                via_cobcrun: true,
                args: Vec::new(),
                stdin: Vec::new(),
                env: Vec::new(),
                expected_exit_code: 0,
                stdout: OutputMatcher::Exact(String::from("MOD_OK")),
                stderr: OutputMatcher::Ignore,
                expected_files: Vec::new(),
                timeout_ms: 2_000,
            }),
        ],
    );
    (unimpl_m, exe_m, mod_m)
}

/// Exercises `CompilerBackend::CoblangCompiled` across compile, standalone `./a.out`, and `via_cobcrun` module execution.
#[test]
fn coblang_compiled_backend_coverage() -> Result<()> {
    assert_eq!(coblang_test_runner::runner_smoke_check()?, 78);
    let tmp = std::env::temp_dir().join(format!("coblang-comp-be-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;

    let mock_comp = write_exec_script(
        &tmp,
        "mock_coblang.sh",
        "#!/bin/sh\u{000a}if grep -q UNIMPL prog.cob; then exit 78; fi\u{000a}if [ -f prog.cob ] && grep -q STANDALONE prog.cob; then printf '#!/bin/sh\\nprintf \"EXE_OK\"\\nexit 0\\n' > a.out; chmod +x a.out; fi\u{000a}exit 0\u{000a}",
    )?;
    let mock_cobcrun_78 = write_exec_script(
        &tmp,
        "mock_cobcrun_78.sh",
        "#!/bin/sh\u{000a}echo unimpl >&2; exit 78\u{000a}",
    )?;
    let mock_cobcrun_0 = write_exec_script(
        &tmp,
        "mock_cobcrun_0.sh",
        "#!/bin/sh\u{000a}printf 'MOD_OK'\u{000a}exit 0\u{000a}",
    )?;

    let (unimpl_m, exe_m, mod_m) = build_compiled_backend_manifests();

    let mk_cfg = |cobcrun_bin| RunnerConfig {
        backend: CompilerBackend::CoblangCompiled {
            compiler_bin: mock_comp.clone(),
            cobcrun_bin,
            compile_timeout_ms: 5_000,
        },
        sandbox_root: tmp.clone(),
        sandbox_retention: SandboxRetention::CleanAlways,
        c_compiler: PathBuf::from("clang"),
        capabilities: HostCapabilities::detect_host(),
        jobs: 1,
    };
    let cfg_78 = mk_cfg(mock_cobcrun_78);
    assert_eq!(
        execute_single_test(&unimpl_m, &cfg_78, 0)?.outcome,
        TestOutcome::XFail
    );
    assert_eq!(
        execute_single_test(&mod_m, &cfg_78, 1)?.outcome,
        TestOutcome::XFail
    );

    let cfg_0 = mk_cfg(mock_cobcrun_0);
    assert_eq!(
        execute_single_test(&exe_m, &cfg_0, 2)?.outcome,
        TestOutcome::Pass
    );
    assert_eq!(
        execute_single_test(&mod_m, &cfg_0, 3)?.outcome,
        TestOutcome::Pass
    );

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Exercises `read_optional_pipe_bounded` truncation and `InProcessCompiler` `XFail` paths.
#[test]
fn inprocess_backend_missing_bin_and_bounded_pipe_coverage() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-inproc-be-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;

    // Exercise `read_optional_pipe_bounded` truncation and draining when output exceeds `max_bytes`.
    let (capped, truncated) =
        coblang_test_runner::process::read_optional_pipe_bounded(Some(&b"0123456789ABCDEF"[..]), 4);
    assert!(truncated);
    assert_eq!(capped, b"0123");
    assert_eq!(
        coblang_test_runner::process::read_optional_pipe_bounded::<&[u8]>(None, 4),
        (Vec::new(), false)
    );
    assert!(
        compile_c_companion(
            &CompileCSpec {
                sources: vec![String::from("missing.c")],
                output: String::from("missing.o"),
                shared: false,
            },
            &tmp,
            Path::new("/nonexistent/coblang_missing_cc"),
            1_000,
            "comp/missing_cc",
        )
        .is_err()
    );

    // Exercise `InProcessCompiler` soft assertion mismatch -> `XFail` and missing target binary in `RunBinary`.
    let inproc_cfg = RunnerConfig {
        backend: CompilerBackend::InProcessCompiler,
        sandbox_root: tmp.clone(),
        sandbox_retention: SandboxRetention::CleanAlways,
        c_compiler: PathBuf::from("clang"),
        capabilities: HostCapabilities::detect_host(),
        jobs: 1,
    };
    let missing_bin_m = simple_canary(
        "comp/missing_bin",
        "       IDENTIFICATION DIVISION.\u{000a}       PROGRAM-ID. MB.\u{000a}",
        None,
        Vec::new(),
        None,
        vec![
            TestStep::CompileCobol(base_cobol_spec(
                CompilationMode::SyntaxOnly,
                "prog.cob",
                "mb.o",
            )),
            TestStep::RunBinary(RunBinarySpec {
                target: String::from("./does_not_exist"),
                via_cobcrun: false,
                args: Vec::new(),
                stdin: Vec::new(),
                env: Vec::new(),
                expected_exit_code: 0,
                stdout: OutputMatcher::Ignore,
                stderr: OutputMatcher::Ignore,
                expected_files: Vec::new(),
                timeout_ms: 1_000,
            }),
        ],
    );
    assert_eq!(
        execute_single_test(&missing_bin_m, &inproc_cfg, 3)?.outcome,
        TestOutcome::XFail
    );
    let xfail_annot_m = simple_canary(
        "comp/xfail_annot",
        "       IDENTIFICATION DIVISION.\u{000a}       PROGRAM-ID. XF.\u{000a}       PROCEDURE DIVISION.\u{000a}       P1.\u{000a}           BOGUSVERB X.\u{000a}",
        Some("known"),
        Vec::new(),
        None,
        vec![TestStep::CompileCobol(base_cobol_spec(
            CompilationMode::SyntaxOnly,
            "prog.cob",
            "xf.o",
        ))],
    );
    assert_eq!(
        execute_single_test(&xfail_annot_m, &inproc_cfg, 4)?.outcome,
        TestOutcome::XFail
    );

    let cobcrun_inproc_m = simple_canary(
        "comp/inproc_cobcrun",
        "       IDENTIFICATION DIVISION.\u{000a}       PROGRAM-ID. IC.\u{000a}       PROCEDURE DIVISION.\u{000a}           STOP RUN.\u{000a}",
        None,
        Vec::new(),
        None,
        vec![TestStep::RunBinary(RunBinarySpec {
            target: String::from("NO_SUCH_MOD"),
            via_cobcrun: true,
            args: Vec::new(),
            stdin: Vec::new(),
            env: Vec::new(),
            expected_exit_code: 0,
            stdout: OutputMatcher::Ignore,
            stderr: OutputMatcher::Ignore,
            expected_files: Vec::new(),
            timeout_ms: 2_000,
        })],
    );
    let _ = execute_single_test(&cobcrun_inproc_m, &inproc_cfg, 5);

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Verifies [`coblang_test_runner::engine::locate_cobcrun_shim`] across missing, parent, and direct shim paths.
#[test]
fn locate_cobcrun_shim_paths_coverage() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-shim-loc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let shim_sub = tmp.join("deps");
    std::fs::create_dir_all(&shim_sub)?;

    assert_eq!(
        coblang_test_runner::engine::locate_cobcrun_shim(None),
        PathBuf::from("cobcrun-shim")
    );
    assert_eq!(
        coblang_test_runner::engine::locate_cobcrun_shim(Some(Path::new("/"))),
        PathBuf::from("cobcrun-shim")
    );
    assert_eq!(
        coblang_test_runner::engine::locate_cobcrun_shim(Some(Path::new("/single_level"))),
        PathBuf::from("cobcrun-shim")
    );
    assert_eq!(
        coblang_test_runner::engine::locate_cobcrun_shim(Some(&shim_sub.join("runner"))),
        PathBuf::from("cobcrun-shim")
    );
    let up_shim = tmp.join("cobcrun-shim");
    std::fs::write(&up_shim, b"")?;
    assert_eq!(
        coblang_test_runner::engine::locate_cobcrun_shim(Some(&shim_sub.join("runner"))),
        up_shim
    );
    let direct_shim = shim_sub.join("cobcrun-shim");
    std::fs::write(&direct_shim, b"")?;
    assert_eq!(
        coblang_test_runner::engine::locate_cobcrun_shim(Some(&shim_sub.join("runner"))),
        direct_shim
    );

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}
