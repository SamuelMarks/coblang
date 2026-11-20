//! Comprehensive tests for `coblang-common` error types, domain enums, and manifest bundle codec.

use std::error::Error as StdError;

use coblang_common::{
    ByteLength, ByteOffset, Capability, CoblangError, CobolExitCode, ColumnNumber, CompilationMode,
    CompileCSpec, CompileCobolSpec, DataItemId, DiagnosticSeverity, Dialect, DigitCount,
    ExpansionId, ExpectedDiagnostic, FeatureTag, FileConnectorId, FileId, FileStatusCode,
    FileStatusKind, LevelNumber, LineNumber, NistModule, OptimizationLevel, OutputMatcher,
    ProcedureId, ProgramUnitId, Provenance, ReportId, ResolvedLocation, ResourceLimitKind, Result,
    RunBinarySpec, ScaleFactor, ScopeId, SourceFormat, SourceSpan, SuiteId, SymbolId, TestManifest,
    TestOutcome, TestStep, TextEncoding, WorkspaceFile, decode_manifest_bundle,
    encode_manifest_bundle, hex_decode, hex_decode_utf8, hex_encode,
};

/// Exercises harness/frontend `Display` and `Error` implementations of [`CoblangError`].
#[test]
fn all_error_variants_format_cleanly() -> Result<()> {
    let io_err: CoblangError = std::io::Error::other("disk full").into();
    assert!(io_err.to_string().contains("disk full"));
    assert!(io_err.source().is_some());
    assert_ne!(format!("{io_err:?}"), "");

    let frontend_errors = [
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
        CoblangError::ConfigLoad {
            path: String::from("default.conf"),
            line: 12,
            message: String::from("unknown key"),
        },
        CoblangError::EncodingConversion {
            encoding: TextEncoding::Ebcdic,
            byte_offset: 4,
            message: String::from("invalid byte"),
        },
        CoblangError::PreprocessError {
            file: String::from("p.cob"),
            line: 1,
            column: 7,
            message: String::from("bad continuation"),
        },
        CoblangError::LexError {
            file: String::from("p.cob"),
            line: 2,
            column: 12,
            message: String::from("unterminated string"),
        },
        CoblangError::ParseError {
            file: String::from("p.cob"),
            line: 3,
            column: 8,
            expected: String::from("DIVISION"),
            found: String::from("EOF"),
        },
        CoblangError::SemanticError {
            diagnostic_count: 2,
            first_message: String::from("undefined identifier X"),
        },
        CoblangError::InvalidDomainValue {
            domain_type: "LevelNumber",
            value: String::from("50"),
            reason: String::from("out of range"),
        },
        CoblangError::HirLowering {
            construct: "MOVE CORRESPONDING",
            location: String::from("p.cob:10:12"),
            message: String::from("not a group"),
        },
    ];
    for err in &frontend_errors {
        assert_ne!(err.to_string(), "");
        assert!(err.source().is_none());
    }

    assert_eq!(coblang_test_runner::runner_smoke_check()?, 78);
    assert_eq!(coblang_test_importer::importer_smoke_check("abc")?, 3);
    assert!(coblang_test_importer::importer_smoke_check("").is_err());
    Ok(())
}

/// Exercises MLIR, codegen, and runtime `Display`/`Error` variants of [`CoblangError`].
#[test]
fn backend_and_runtime_error_variants_format_cleanly() {
    let backend_errors = [
        CoblangError::MlirVerification {
            stage: "cobol-dialect",
            ir_snippet: String::from("cobol.move"),
            message: String::from("missing operand"),
        },
        CoblangError::MlirPassExecution {
            pass_pipeline: String::from("lower-to-llvm"),
            message: String::from("pass failed"),
        },
        CoblangError::CodegenTarget {
            triple: String::from("unknown-triple"),
            message: String::from("no target"),
        },
        CoblangError::LinkerInvocation {
            linker_bin: String::from("clang"),
            exit_code: Some(1),
            stderr: String::from("undefined symbol"),
        },
        CoblangError::DecimalDomain {
            operation: "DIVIDE",
            message: String::from("division by zero"),
        },
        CoblangError::PictureCodec {
            picture: String::from("9(40)"),
            message: String::from("too wide"),
        },
        CoblangError::RuntimeFileIo {
            file_name: String::from("INFILE"),
            status: FileStatusKind::FileNotFound35.code(),
            message: String::from("missing file"),
        },
        CoblangError::IsamStorage {
            path: String::from("idx.dat"),
            page_id: 3,
            message: String::from("bad crc"),
        },
        CoblangError::SortMerge {
            message: String::from("nested sort active"),
        },
        CoblangError::ReportEngine {
            report_name: String::from("RPT1"),
            message: String::from("not initiated"),
        },
        CoblangError::ScreenBackend {
            message: String::from("pty closed"),
        },
        CoblangError::XmlJsonProcess {
            kind: "XML",
            offset: 9,
            message: String::from("unclosed tag"),
        },
        CoblangError::DynamicModuleLoad {
            module_name: String::from("SUBPROG"),
            searched_paths: String::from("."),
            message: String::from("not found"),
        },
        CoblangError::AbiNullOrInvalidPointer {
            symbol: "cob_move",
            param_name: "src",
        },
        CoblangError::UnimplementedFeature {
            feature: FeatureTag::FeatureOo,
            construct: String::from("CLASS-ID"),
            location: String::from("oo.cob:1:8"),
        },
    ];
    for err in &backend_errors {
        assert_ne!(err.to_string(), "");
        assert!(err.source().is_none());
    }
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
        if let Some(slot) = c.get_mut(idx) {
            *slot = bad_val;
        }
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
        if let Some(slot) = c.get_mut(idx) {
            *slot = bad_val;
        }
        let line = c.join("\u{0009}");
        let doc = format!("{hdr}\u{000a}TEST\u{0009}61\u{000a}{line}\u{000a}END_TEST");
        assert!(decode_manifest_bundle(&doc).is_err());
    }
}

/// Exercises every constructor, accessor, and merge branch in `ids` and `span`.
#[test]
fn strong_typed_ids_and_spans_roundtrip() -> Result<()> {
    assert_eq!(FileId::new(4).as_u32(), 4);
    assert!(ExpansionId::ROOT.is_root());
    assert!(!ExpansionId::new(2).is_root());
    assert_eq!(ExpansionId::new(2).as_u32(), 2);
    assert_eq!(SymbolId::new(9).as_u32(), 9);
    assert_eq!(ScopeId::new(3).as_u32(), 3);
    assert_eq!(ProgramUnitId::new(1).as_u32(), 1);
    assert_eq!(DataItemId::new(7).as_u32(), 7);
    assert_eq!(ProcedureId::new(5).as_u32(), 5);
    assert_eq!(FileConnectorId::new(6).as_u32(), 6);
    assert_eq!(ReportId::new(8).as_u32(), 8);

    assert_eq!(ByteOffset::ZERO.as_u32(), 0);
    assert_eq!(ByteLength::ZERO.as_u32(), 0);
    let advanced = ByteOffset::new(10).checked_advance(ByteLength::new(5))?;
    assert_eq!(advanced.as_u32(), 15);
    assert_eq!(advanced.checked_sub(ByteLength::new(5))?.as_u32(), 10);
    assert!(
        ByteOffset::new(u32::MAX)
            .checked_advance(ByteLength::new(1))
            .is_err()
    );
    assert!(ByteOffset::new(3).checked_sub(ByteLength::new(4)).is_err());

    let len_sum = ByteLength::new(7).checked_add(ByteLength::new(8))?;
    assert_eq!(len_sum.as_u32(), 15);
    assert_eq!(len_sum.checked_sub(ByteLength::new(8))?.as_u32(), 7);
    assert!(
        ByteLength::new(u32::MAX)
            .checked_add(ByteLength::new(1))
            .is_err()
    );
    assert!(ByteLength::new(2).checked_sub(ByteLength::new(3)).is_err());

    assert_eq!(LineNumber::ONE.get(), 1);
    assert_eq!(LineNumber::from_zero_based(0).get(), 1);
    assert_eq!(LineNumber::from_zero_based(4).get(), 5);
    assert_eq!(LineNumber::try_from_one_based(42)?.get(), 42);
    assert!(LineNumber::try_from_one_based(0).is_err());

    assert_eq!(ColumnNumber::ONE.get(), 1);
    assert_eq!(ColumnNumber::from_zero_based(0).get(), 1);
    assert_eq!(ColumnNumber::from_zero_based(9).get(), 10);
    assert_eq!(ColumnNumber::try_from_one_based(12)?.get(), 12);
    assert!(ColumnNumber::try_from_one_based(0).is_err());

    let sp_a = SourceSpan::new(
        FileId::new(0),
        ByteOffset::new(10),
        ByteLength::new(4),
        ExpansionId::ROOT,
    );
    let sp_b = SourceSpan::new(
        FileId::new(0),
        ByteOffset::new(20),
        ByteLength::new(5),
        ExpansionId::ROOT,
    );
    let sp_inner = SourceSpan::new(
        FileId::new(0),
        ByteOffset::new(11),
        ByteLength::new(1),
        ExpansionId::ROOT,
    );
    let sp_other_file = SourceSpan::new(
        FileId::new(1),
        ByteOffset::new(0),
        ByteLength::new(3),
        ExpansionId::ROOT,
    );
    assert_eq!(sp_a.merge(sp_b).start.as_u32(), 10);
    assert_eq!(sp_a.merge(sp_b).len.as_u32(), 15);
    assert_eq!(sp_b.merge(sp_a).start.as_u32(), 10);
    assert_eq!(sp_b.merge(sp_a).len.as_u32(), 15);
    assert_eq!(sp_a.merge(sp_inner).len.as_u32(), 4);
    assert_eq!(sp_a.merge(sp_other_file), sp_a);

    let loc = ResolvedLocation {
        file_path: String::from("prog.cob"),
        line: LineNumber::try_from_one_based(7)?,
        column: ColumnNumber::try_from_one_based(8)?,
    };
    assert_eq!(loc.to_header_prefix(), "prog.cob:7:8");
    Ok(())
}

/// Exercises every constructor, accessor, and validation branch in `bounds`.
#[test]
fn strong_typed_bounds_and_file_status_roundtrip() -> Result<()> {
    assert_eq!(LevelNumber::parse(1)?.as_u8(), 1);
    assert_eq!(LevelNumber::parse(49)?.as_u8(), 49);
    assert_eq!(LevelNumber::parse(66)?.as_u8(), 66);
    assert_eq!(LevelNumber::parse(77)?.as_u8(), 77);
    assert_eq!(LevelNumber::parse(78)?.as_u8(), 78);
    assert_eq!(LevelNumber::parse(88)?.as_u8(), 88);
    assert!(LevelNumber::parse(0).is_err());
    assert!(LevelNumber::parse(50).is_err());
    assert!(LevelNumber::parse(99).is_err());

    assert_eq!(DigitCount::ONE.get(), 1);
    let d18 = DigitCount::try_new(18)?.validate_for_dialect(DigitCount::MAX_COBOL85_DIGITS)?;
    assert_eq!(d18.get(), 18);
    let d38 = DigitCount::try_new(38)?.validate_for_dialect(DigitCount::MAX_ISO_DIGITS)?;
    assert_eq!(d38.get(), 38);
    assert!(
        d38.validate_for_dialect(DigitCount::MAX_COBOL85_DIGITS)
            .is_err()
    );
    assert!(DigitCount::try_new(0).is_err());
    assert!(DigitCount::try_new(39).is_err());

    assert_eq!(ScaleFactor::ZERO.get(), 0);
    assert_eq!(ScaleFactor::try_new(-5)?.get(), -5);
    assert_eq!(ScaleFactor::try_new(12)?.get(), 12);
    assert!(ScaleFactor::try_new(-39).is_err());
    assert!(ScaleFactor::try_new(39).is_err());

    assert_eq!(CobolExitCode::SUCCESS.as_u8(), 0);
    assert_eq!(CobolExitCode::ERROR.as_u8(), 1);
    assert_eq!(CobolExitCode::UNIMPLEMENTED.as_u8(), 78);
    assert_eq!(CobolExitCode::new(42).as_u8(), 42);

    assert_eq!(FileStatusKind::ALL.len(), 28);
    for &kind in FileStatusKind::ALL {
        let code = FileStatusCode::from_kind(kind);
        assert_eq!(kind.code(), code);
        assert_eq!(code.kind(), Some(kind));
        assert_eq!(FileStatusCode::try_from_ascii(*code.as_bytes())?, code);
        assert_eq!(code.as_str().len(), 2);
    }
    let custom_status = FileStatusCode::try_from_ascii(*b"99")?;
    assert_eq!(custom_status.kind(), None);
    assert!(FileStatusKind::Success00.code().is_success());
    assert!(!FileStatusKind::FileNotFound35.code().is_success());
    assert!(FileStatusCode::try_from_ascii(*b" 0").is_err());
    assert!(FileStatusCode::try_from_ascii(*b"0 ").is_err());

    assert_eq!(ResourceLimitKind::ALL.len(), 6);
    for &rk in ResourceLimitKind::ALL {
        let err = CoblangError::ResourceLimitExceeded {
            resource: rk,
            limit: 10,
            observed: 11,
        };
        assert!(err.to_string().contains(rk.as_str()));
    }

    Ok(())
}
