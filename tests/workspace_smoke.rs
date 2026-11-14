//! Comprehensive tests for `coblang-common` error types, domain enums, and manifest bundle codec.

use std::error::Error as StdError;

use coblang_common::{
    Capability, CoblangError, CompilationMode, CompileCSpec, CompileCobolSpec, DiagnosticSeverity,
    Dialect, ExpectedDiagnostic, FeatureTag, NistModule, OptimizationLevel, OutputMatcher,
    Provenance, Result, RunBinarySpec, SourceFormat, SuiteId, TestManifest, TestOutcome, TestStep,
    TextEncoding, WorkspaceFile, decode_manifest_bundle, encode_manifest_bundle, hex_decode,
    hex_decode_utf8, hex_encode,
};

/// Exercises all variants and `Display`/`Error` implementations of [`CoblangError`].
#[test]
fn all_error_variants_format_cleanly() -> Result<()> {
    let io_err: CoblangError = std::io::Error::other("disk full").into();
    assert!(io_err.to_string().contains("disk full"));
    assert!(io_err.source().is_some());
    assert_ne!(format!("{io_err:?}"), "");

    let others = [
        CoblangError::InvalidCli {
            message: String::from("bad flag"),
        },
        CoblangError::ManifestFormat {
            line: 7,
            message: String::from("bad col"),
        },
        CoblangError::InvalidRegex {
            pattern: String::from("["),
            reason: String::from("unclosed"),
        },
        CoblangError::ImportParse {
            source_file: String::from("a.cob"),
            line: 3,
            message: String::from("oops"),
        },
        CoblangError::RunnerExecution {
            test_id: String::from("t1"),
            message: String::from("boom"),
        },
        CoblangError::RatchetViolation {
            summary: String::from("1 regression"),
        },
    ];
    for err in &others {
        assert_ne!(err.to_string(), "");
        assert!(err.source().is_none());
    }

    assert_eq!(coblang_test_runner::runner_smoke_check()?, 78);
    assert_eq!(coblang_test_importer::importer_smoke_check("abc")?, 3);
    assert!(coblang_test_importer::importer_smoke_check("").is_err());
    Ok(())
}

/// Exercises round-trip parsing and invalid-token branches for compiler domain enums.
#[test]
fn compiler_domain_enum_roundtrips() -> Result<()> {
    for m in [
        CompilationMode::PreprocessOnly,
        CompilationMode::SyntaxOnly,
        CompilationMode::EmitAssembly,
        CompilationMode::EmitLlvmIr,
        CompilationMode::CompileObject,
        CompilationMode::CompileModule,
        CompilationMode::BuildExecutable,
    ] {
        assert_eq!(CompilationMode::parse_token(m.as_str(), 1)?, m);
    }
    assert!(CompilationMode::parse_token("nope", 1).is_err());

    for f in [SourceFormat::Auto, SourceFormat::Fixed, SourceFormat::Free] {
        assert_eq!(SourceFormat::parse_token(f.as_str(), 1)?, f);
    }
    assert!(SourceFormat::parse_token("weird", 1).is_err());

    for d in [
        Dialect::Default,
        Dialect::Cobol85,
        Dialect::Cobol2002,
        Dialect::Cobol2014,
        Dialect::Cobol2023,
        Dialect::Ibm,
        Dialect::MicroFocus,
        Dialect::Gnu,
        Dialect::Bs2000,
        Dialect::Acu,
        Dialect::XOpen,
    ] {
        assert_eq!(Dialect::from_cli_name(d.as_str())?, d);
    }
    for alias in [
        "cobol-85",
        "iso",
        "ibm-strict",
        "mvs",
        "mvs-strict",
        "mf-strict",
        "cobol-gnu",
        "bs2000-strict",
        "acu-strict",
        "rm",
        "rm-strict",
    ] {
        assert!(Dialect::from_cli_name(alias).is_ok());
    }
    assert!(Dialect::from_cli_name("unknown-dialect").is_err());

    for o in [
        OptimizationLevel::O0,
        OptimizationLevel::O1,
        OptimizationLevel::O2,
        OptimizationLevel::O3,
        OptimizationLevel::Os,
    ] {
        assert_eq!(OptimizationLevel::parse_flag(o.as_flag(), 1)?, o);
    }
    assert_eq!(
        OptimizationLevel::parse_flag("-O", 1)?,
        OptimizationLevel::O1
    );
    assert_eq!(
        OptimizationLevel::parse_flag("-Ofast", 1)?,
        OptimizationLevel::O3
    );
    assert_eq!(
        OptimizationLevel::parse_flag("-Oz", 1)?,
        OptimizationLevel::Os
    );
    assert!(OptimizationLevel::parse_flag("-O99", 1).is_err());

    for enc in [
        TextEncoding::Ascii,
        TextEncoding::Ebcdic,
        TextEncoding::Codepage(String::from("CP1140")),
    ] {
        assert_eq!(TextEncoding::parse_token(&enc.to_token(), 1)?, enc);
    }
    assert!(TextEncoding::parse_token("codepage:", 1).is_err());
    assert!(TextEncoding::parse_token("utf32", 1).is_err());
    Ok(())
}

/// Exercises round-trip parsing and invalid-token branches for harness taxonomy enums.
#[test]
fn harness_taxonomy_enum_roundtrips() -> Result<()> {
    for s in [
        SuiteId::Canary,
        SuiteId::Gcobol,
        SuiteId::GnuCobol,
        SuiteId::Nist85,
    ] {
        assert_eq!(SuiteId::parse_token(s.as_str(), 1)?, s);
    }
    assert!(SuiteId::parse_token("other", 1).is_err());

    for sev in [
        DiagnosticSeverity::Error,
        DiagnosticSeverity::Warning,
        DiagnosticSeverity::Note,
    ] {
        assert_eq!(DiagnosticSeverity::parse_token(sev.as_str(), 1)?, sev);
    }
    assert!(DiagnosticSeverity::parse_token("fatal", 1).is_err());

    for out in [
        TestOutcome::Pass,
        TestOutcome::Fail,
        TestOutcome::XFail,
        TestOutcome::XPass,
        TestOutcome::Skip,
    ] {
        assert_eq!(TestOutcome::parse_token(out.as_str(), 1)?, out);
    }
    assert!(TestOutcome::parse_token("FLAKY", 1).is_err());

    for cap in [
        Capability::SharedLibraries,
        Capability::CCompiler,
        Capability::PosixTools,
        Capability::BigEndian,
        Capability::LittleEndian,
        Capability::Int128,
        Capability::Isam,
        Capability::Xml,
        Capability::Json,
        Capability::CursesPty,
        Capability::IconvCodepage(String::from("IBM1140")),
    ] {
        assert_eq!(Capability::parse_token(&cap.to_token(), 1)?, cap);
    }
    assert!(Capability::parse_token("iconv:", 1).is_err());
    assert!(Capability::parse_token("quantum", 1).is_err());

    for tag in FeatureTag::ALL {
        assert_eq!(FeatureTag::parse_token(tag.as_str(), 1)?, *tag);
    }
    assert!(FeatureTag::parse_token("feature:nonexistent", 1).is_err());

    for m in NistModule::ALL {
        assert_eq!(NistModule::from_code(m.code(), 1)?, *m);
    }
    assert!(NistModule::from_code("ZZ", 1).is_err());
    Ok(())
}

/// Exercises full manifest bundle round-trip serialization.
#[test]
fn manifest_bundle_roundtrip() -> Result<()> {
    assert_eq!(hex_encode(b""), ".");
    assert_eq!(hex_decode(".", 1)?, Vec::<u8>::new());
    assert_eq!(hex_decode("414a4f", 1)?, b"AJO");
    assert_eq!(hex_decode("414A4F", 1)?, b"AJO");
    assert!(hex_decode("abc", 1).is_err());
    assert!(hex_decode("zz", 1).is_err());
    assert!(hex_decode("0z", 1).is_err());
    assert!(hex_decode_utf8("ff", 1).is_err());

    for om in [
        OutputMatcher::Ignore,
        OutputMatcher::Exact(String::from("hello\u{000a}")),
        OutputMatcher::Regex(String::from("^ok$")),
        OutputMatcher::NormalizedLines(String::from("line1")),
        OutputMatcher::Ccvs85Report {
            expected_pass: 12,
            expected_deleted: 1,
            expected_inspect: 2,
        },
    ] {
        let (k, p) = om.to_parts();
        assert_eq!(OutputMatcher::from_parts(k, &p, 1)?, om);
    }
    for (k, p) in [
        ("ccvs85", "1,2"),
        ("ccvs85", "x,1,2"),
        ("ccvs85", "1,x,2"),
        ("ccvs85", "1,2,x"),
        ("exact", "zz"),
        ("regex", "zz"),
        ("norm", "zz"),
        ("bogus", ""),
    ] {
        assert!(OutputMatcher::from_parts(k, p, 1).is_err());
    }

    let rich = sample_rich_manifest();
    let minimal = sample_minimal_manifest();
    let encoded = encode_manifest_bundle(&[rich.clone(), minimal.clone()]);
    let with_blank_line = format!("{encoded}\u{000a}");
    let decoded = decode_manifest_bundle(&with_blank_line)?;
    assert_eq!(decoded, vec![rich, minimal]);
    Ok(())
}

/// Builds a feature-rich [`TestManifest`] exercising every optional field and step variant.
fn sample_rich_manifest() -> TestManifest {
    TestManifest {
        id: String::from("gcobol/group1/check_88"),
        suite: SuiteId::Gcobol,
        provenance: Provenance {
            upstream_repo: String::from("https://github.com/gcc-mirror/gcc"),
            upstream_commit: String::from("6e0de8bd5ebb9a2b9903cb7348ae015d9ec7d765"),
            source_file: String::from("gcc/testsuite/cobol.dg/group1/check_88.cob"),
            source_line: 1,
            original_test_name: String::from("check_88"),
            spdx_license: String::from("GPL-3.0-or-later"),
        },
        files: vec![WorkspaceFile {
            relative_path: String::from("check_88.cob"),
            contents: b"IDENTIFICATION DIVISION.".to_vec(),
        }],
        steps: vec![
            TestStep::WriteFile(WorkspaceFile {
                relative_path: String::from("input.dat"),
                contents: b"42".to_vec(),
            }),
            TestStep::CompileC(CompileCSpec {
                sources: vec![String::from("helper.c")],
                shared: false,
                output: String::from("helper.o"),
            }),
            TestStep::CompileCobol(CompileCobolSpec {
                mode: CompilationMode::BuildExecutable,
                format: SourceFormat::Free,
                dialect: Dialect::Cobol2023,
                optimization: OptimizationLevel::O2,
                encoding: TextEncoding::Ascii,
                sources: vec![String::from("check_88.cob"), String::from("helper.o")],
                include_dirs: vec![String::from("inc")],
                copy_exts: vec![String::from(".cpy")],
                library_dirs: vec![String::from("lib")],
                libraries: vec![String::from("m")],
                raw_flags: vec![String::from("-Wall")],
                output: String::from("a.out"),
                expected_success: true,
                expected_diagnostics: vec![
                    ExpectedDiagnostic {
                        file: String::from("check_88.cob"),
                        line: Some(4),
                        severity: DiagnosticSeverity::Warning,
                        message_pattern: String::from("warn"),
                    },
                    ExpectedDiagnostic {
                        file: String::new(),
                        line: None,
                        severity: DiagnosticSeverity::Note,
                        message_pattern: String::from("note"),
                    },
                ],
                bogus_patterns: vec![String::from("bogus")],
                allow_excess_errors: false,
                prune_patterns: vec![String::from("failed compiling")],
                env: vec![(String::from("COB_CONFIG_DIR"), String::from("/tmp"))],
            }),
            TestStep::RunBinary(RunBinarySpec {
                target: String::from("./a.out"),
                via_cobcrun: false,
                args: vec![String::from("--arg1")],
                stdin: b"hello".to_vec(),
                env: vec![(String::from("DD_OUT"), String::from("out.txt"))],
                expected_exit_code: 0,
                stdout: OutputMatcher::Exact(String::from("OK")),
                stderr: OutputMatcher::Ignore,
                expected_files: vec![(
                    String::from("out.txt"),
                    OutputMatcher::NormalizedLines(String::from("DONE")),
                )],
                timeout_ms: 5000,
            }),
        ],
        torture_optimizations: vec![OptimizationLevel::O0, OptimizationLevel::O2],
        required_capabilities: vec![Capability::CCompiler],
        xfail_reason: Some(String::from("scaffold")),
        skip_reason: Some(String::from("manual")),
        feature_tags: vec![FeatureTag::FeatureLevel88],
        equivalent_tests: vec![String::from("gnucobol/run_fundamental/0001")],
    }
}

/// Builds a minimal [`TestManifest`] exercising empty lists and `None` fields.
fn sample_minimal_manifest() -> TestManifest {
    TestManifest {
        id: String::from("canary/minimal"),
        suite: SuiteId::Canary,
        provenance: Provenance {
            upstream_repo: String::from("local"),
            upstream_commit: String::from("HEAD"),
            source_file: String::from("minimal.cob"),
            source_line: 1,
            original_test_name: String::from("minimal"),
            spdx_license: String::from("Apache-2.0 WITH LLVM-exception"),
        },
        files: Vec::new(),
        steps: vec![
            TestStep::CompileCobol(CompileCobolSpec {
                mode: CompilationMode::SyntaxOnly,
                format: SourceFormat::Fixed,
                dialect: Dialect::Default,
                optimization: OptimizationLevel::O0,
                encoding: TextEncoding::Ebcdic,
                sources: Vec::new(),
                include_dirs: Vec::new(),
                copy_exts: Vec::new(),
                library_dirs: Vec::new(),
                libraries: Vec::new(),
                raw_flags: Vec::new(),
                output: String::new(),
                expected_success: false,
                expected_diagnostics: Vec::new(),
                bogus_patterns: Vec::new(),
                allow_excess_errors: true,
                prune_patterns: Vec::new(),
                env: Vec::new(),
            }),
            TestStep::RunBinary(RunBinarySpec {
                target: String::from("PROG"),
                via_cobcrun: true,
                args: Vec::new(),
                stdin: Vec::new(),
                env: Vec::new(),
                expected_exit_code: 1,
                stdout: OutputMatcher::Ignore,
                stderr: OutputMatcher::Ignore,
                expected_files: Vec::new(),
                timeout_ms: 1000,
            }),
        ],
        torture_optimizations: Vec::new(),
        required_capabilities: Vec::new(),
        xfail_reason: None,
        skip_reason: None,
        feature_tags: Vec::new(),
        equivalent_tests: Vec::new(),
    }
}

/// Exercises every malformed-input branch in [`decode_manifest_bundle`].
#[test]
fn manifest_decoder_rejects_malformed_records() {
    let hdr = "COBLANG_MANIFEST_BUNDLE_V1";
    let base_cobol = "STEP_COBOL\u{0009}preprocess\u{0009}auto\u{0009}default\u{0009}-O0\u{0009}6173636969\u{0009}-\u{0009}-\u{0009}-\u{0009}-\u{0009}-\u{0009}-\u{0009}.\u{0009}1\u{0009}-\u{0009}-\u{0009}0\u{0009}-";
    let base_run = "STEP_RUN\u{0009}61\u{0009}0\u{0009}-\u{0009}.\u{0009}-\u{0009}0\u{0009}ignore\u{0009}\u{0009}ignore\u{0009}\u{0009}10";

    for structural in [
        String::new(),
        String::from("WRONG_HEADER"),
        format!("{hdr}\u{000a}END_TEST"),
        format!("{hdr}\u{000a}SUITE\u{0009}canary"),
        format!("{hdr}\u{000a}TEST\u{0009}61\u{000a}TEST\u{0009}62\u{000a}END_TEST"),
        format!("{hdr}\u{000a}TEST\u{0009}61"),
        format!("{hdr}\u{000a}TEST"),
        format!("{hdr}\u{000a}TEST\u{0009}zz\u{000a}END_TEST"),
    ] {
        assert!(decode_manifest_bundle(&structural).is_err());
    }

    let body_errors = [
        String::from("SUITE\u{0009}bad"),
        String::from("SUITE"),
        String::from("PROV\u{0009}61"),
        String::from("PROV\u{0009}zz\u{0009}61\u{0009}61\u{0009}1\u{0009}61\u{0009}61"),
        String::from("PROV\u{0009}61\u{0009}zz\u{0009}61\u{0009}1\u{0009}61\u{0009}61"),
        String::from("PROV\u{0009}61\u{0009}61\u{0009}zz\u{0009}1\u{0009}61\u{0009}61"),
        String::from("PROV\u{0009}61\u{0009}61\u{0009}61\u{0009}x\u{0009}61\u{0009}61"),
        String::from("PROV\u{0009}61\u{0009}61\u{0009}61\u{0009}1\u{0009}zz\u{0009}61"),
        String::from("PROV\u{0009}61\u{0009}61\u{0009}61\u{0009}1\u{0009}61\u{0009}zz"),
        String::from("TORTURE"),
        String::from("TORTURE\u{0009}-O99"),
        String::from("CAPS"),
        String::from("CAPS\u{0009}bad"),
        String::from("XFAIL"),
        String::from("XFAIL\u{0009}zz"),
        String::from("SKIP"),
        String::from("SKIP\u{0009}zz"),
        String::from("TAGS"),
        String::from("TAGS\u{0009}bad"),
        String::from("EQUIV"),
        String::from("EQUIV\u{0009}zz"),
        String::from("FILE\u{0009}61"),
        String::from("FILE\u{0009}zz\u{0009}61"),
        String::from("FILE\u{0009}61\u{0009}zz"),
        String::from("STEP_WRITE\u{0009}61"),
        String::from("STEP_WRITE\u{0009}zz\u{0009}61"),
        String::from("STEP_WRITE\u{0009}61\u{0009}zz"),
        String::from("STEP_CC\u{0009}61"),
        String::from("STEP_CC\u{0009}zz\u{0009}0\u{0009}61"),
        String::from("STEP_CC\u{0009}-\u{0009}9\u{0009}61"),
        String::from("STEP_CC\u{0009}-\u{0009}0\u{0009}zz"),
        String::from("STEP_COBOL\u{0009}short"),
        String::from("STEP_COBOL_ENV\u{0009}-"),
        String::from("STEP_COBOL_ENV"),
        format!("{base_cobol}\u{000a}STEP_COBOL_ENV\u{0009}nocolon"),
        format!("{base_cobol}\u{000a}STEP_COBOL_ENV\u{0009}zz:61"),
        format!("{base_cobol}\u{000a}STEP_COBOL_ENV\u{0009}61:zz"),
        String::from("STEP_RUN\u{0009}short"),
        String::from("STEP_RUN_FILE\u{0009}61\u{0009}ignore\u{0009}"),
        String::from("STEP_RUN_FILE\u{0009}61"),
        format!("{base_run}\u{000a}STEP_RUN_FILE\u{0009}zz\u{0009}ignore\u{0009}"),
        format!("{base_run}\u{000a}STEP_RUN_FILE\u{0009}61\u{0009}bad\u{0009}"),
        String::from("UNKNOWN_TAG"),
    ];
    for rec in &body_errors {
        let doc = format!("{hdr}\u{000a}TEST\u{0009}61\u{000a}{rec}\u{000a}END_TEST");
        assert!(decode_manifest_bundle(&doc).is_err(), "expected err: {rec}");
    }

    assert_step_column_mutations_fail(hdr, base_cobol, base_run);
}

/// Mutates each column of `STEP_COBOL` and `STEP_RUN` to verify all field error branches.
fn assert_step_column_mutations_fail(hdr: &str, base_cobol: &str, base_run: &str) {
    let cobol_cols: Vec<&str> = base_cobol.split('\u{0009}').collect();
    for (idx, bad_val) in [
        (1, "bad"),
        (2, "bad"),
        (3, "nope"),
        (4, "-O9"),
        (5, "zz"),
        (5, "626164"),
        (6, "zz"),
        (7, "zz"),
        (8, "zz"),
        (9, "zz"),
        (10, "zz"),
        (11, "zz"),
        (12, "zz"),
        (13, "9"),
        (14, "baddiag"),
        (14, "61:x:error:61"),
        (14, "zz:1:error:61"),
        (14, "61:1:bad:61"),
        (14, "61:1:error:zz"),
        (15, "zz"),
        (16, "9"),
        (17, "zz"),
    ] {
        let mut c = cobol_cols.clone();
        c[idx] = bad_val;
        let line = c.join("\u{0009}");
        let doc = format!("{hdr}\u{000a}TEST\u{0009}61\u{000a}{line}\u{000a}END_TEST");
        assert!(decode_manifest_bundle(&doc).is_err());
    }

    let run_cols: Vec<&str> = base_run.split('\u{0009}').collect();
    for (idx, bad_val) in [
        (1, "zz"),
        (2, "9"),
        (3, "zz"),
        (4, "zz"),
        (5, "bad"),
        (6, "x"),
        (7, "bad"),
        (9, "bad"),
        (11, "x"),
    ] {
        let mut c = run_cols.clone();
        c[idx] = bad_val;
        let line = c.join("\u{0009}");
        let doc = format!("{hdr}\u{000a}TEST\u{0009}61\u{000a}{line}\u{000a}END_TEST");
        assert!(decode_manifest_bundle(&doc).is_err());
    }
}
