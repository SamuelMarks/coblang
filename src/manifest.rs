//! Declarative test case manifest schema and deterministic line-oriented bundle codec.

use std::fmt::Write as _;

use crate::domain::{
    Capability, CompilationMode, DiagnosticSeverity, Dialect, FeatureTag, OptimizationLevel,
    SourceFormat, SuiteId, TextEncoding,
};
use crate::error::{CoblangError, Result};

/// Cryptographic and legal provenance metadata recorded on every imported test case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// Upstream repository URL or archive identifier.
    pub upstream_repo: String,
    /// Pinned upstream Git commit SHA-1 or archive SHA-256.
    pub upstream_commit: String,
    /// Relative path of the source file inside the upstream repository.
    pub source_file: String,
    /// One-based starting line number in the upstream source file.
    pub source_line: u32,
    /// Original upstream test name or filename stem.
    pub original_test_name: String,
    /// SPDX license expression governing the test input.
    pub spdx_license: String,
}

/// File materialized inside a test's isolated temporary workspace prior to execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceFile {
    /// Relative path inside the sandbox workspace (e.g., `"prog.cob"` or `"sub/copy.inc"`).
    pub relative_path: String,
    /// Raw byte contents written to `relative_path`.
    pub contents: Vec<u8>,
}

/// Compile-time diagnostic assertion normalized across `coblang`, `cobc`, and `gcobol`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedDiagnostic {
    /// Source filename expected to emit the diagnostic (empty matches any source file).
    pub file: String,
    /// Expected 1-based line number (`None` matches any line).
    pub line: Option<u32>,
    /// Expected diagnostic severity (`error`, `warning`, or `note`).
    pub severity: DiagnosticSeverity,
    /// Original regex or substring pattern from the upstream test (`dg-error` or `cobc` stderr).
    pub message_pattern: String,
}

/// Output verification rule applied to `stdout`, `stderr`, or generated workspace files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputMatcher {
    /// Output is unconstrained (`ignore` in Autotest).
    Ignore,
    /// Output must match the byte/UTF-8 string after optional CRLF-to-LF normalization.
    Exact(String),
    /// Output must match the given regular expression (`dg-output`).
    Regex(String),
    /// Output lines must match after trimming trailing spaces and normalizing `CRLF` to `LF`.
    NormalizedLines(String),
    /// Output is a NIST CCVS-85 report that must contain zero `*FAIL*` entries and match counts.
    Ccvs85Report {
        /// Expected count of `PASS` subtests reported by the program.
        expected_pass: u32,
        /// Expected count of `DELETED` subtests.
        expected_deleted: u32,
        /// Expected count of `INSPECT` subtests.
        expected_inspect: u32,
    },
}

impl OutputMatcher {
    /// Encodes this [`OutputMatcher`] into a tag and payload pair for manifest serialization.
    #[must_use]
    pub fn to_parts(&self) -> (&'static str, String) {
        match self {
            Self::Ignore => ("ignore", String::new()),
            Self::Exact(s) => ("exact", hex_encode(s.as_bytes())),
            Self::Regex(r) => ("regex", hex_encode(r.as_bytes())),
            Self::NormalizedLines(s) => ("norm", hex_encode(s.as_bytes())),
            Self::Ccvs85Report {
                expected_pass,
                expected_deleted,
                expected_inspect,
            } => (
                "ccvs85",
                format!("{expected_pass},{expected_deleted},{expected_inspect}"),
            ),
        }
    }

    /// Decodes an [`OutputMatcher`] from serialized `(kind, payload)` tokens.
    ///
    /// # Arguments
    ///
    /// * `kind` - Discriminator tag produced by [`Self::to_parts`].
    /// * `payload` - Hex-encoded string or comma-separated CCVS-85 counts.
    /// * `line` - Manifest line number used for error reporting.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ManifestFormat`] if `kind` or `payload` is invalid.
    pub fn from_parts(kind: &str, payload: &str, line: usize) -> Result<Self> {
        match kind {
            "ignore" => Ok(Self::Ignore),
            "exact" => Ok(Self::Exact(hex_decode_utf8(payload, line)?)),
            "regex" => Ok(Self::Regex(hex_decode_utf8(payload, line)?)),
            "norm" => Ok(Self::NormalizedLines(hex_decode_utf8(payload, line)?)),
            "ccvs85" => {
                let nums: Vec<&str> = payload.split(',').collect();
                if nums.len() != 3 {
                    return Err(CoblangError::ManifestFormat {
                        line,
                        message: format!("invalid ccvs85 payload `{payload}`"),
                    });
                }
                let expected_pass = parse_u32(nums[0], line)?;
                let expected_deleted = parse_u32(nums[1], line)?;
                let expected_inspect = parse_u32(nums[2], line)?;
                Ok(Self::Ccvs85Report {
                    expected_pass,
                    expected_deleted,
                    expected_inspect,
                })
            }
            _ => Err(CoblangError::ManifestFormat {
                line,
                message: format!("unknown output matcher kind `{kind}`"),
            }),
        }
    }
}

/// Specification for a COBOL compilation step inside a [`TestManifest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileCobolSpec {
    /// Requested compilation stage.
    pub mode: CompilationMode,
    /// Source reference format override.
    pub format: SourceFormat,
    /// COBOL standard/vendor dialect profile.
    pub dialect: Dialect,
    /// Base optimization level (overridden per iteration when torture testing is active).
    pub optimization: OptimizationLevel,
    /// Runtime character encoding setting.
    pub encoding: TextEncoding,
    /// Input COBOL source files (and any prebuilt `.o`/`.c` inputs) relative to workspace root.
    pub sources: Vec<String>,
    /// Copybook search directories (`-I`).
    pub include_dirs: Vec<String>,
    /// Extra copybook extensions (`-copyext`).
    pub copy_exts: Vec<String>,
    /// Library search directories (`-L`).
    pub library_dirs: Vec<String>,
    /// Linked library names (`-l`).
    pub libraries: Vec<String>,
    /// Preserved upstream compiler flags (`-Wno-*`, `-f*`, etc.).
    pub raw_flags: Vec<String>,
    /// Output artifact path relative to workspace root.
    pub output: String,
    /// Whether compilation is expected to succeed (`exit == 0`).
    pub expected_success: bool,
    /// Expected compile-time diagnostics (`dg-error`, `dg-warning`, `dg-message`, `cobc` stderr).
    pub expected_diagnostics: Vec<ExpectedDiagnostic>,
    /// Forbidden diagnostic regexes (`dg-bogus`).
    pub bogus_patterns: Vec<String>,
    /// Whether unmatched extra errors are tolerated (`dg-excess-errors`).
    pub allow_excess_errors: bool,
    /// Regex patterns pruned from compiler output before excess-error checks (`dg-prune-output`).
    pub prune_patterns: Vec<String>,
    /// Environment variables set during compilation.
    pub env: Vec<(String, String)>,
}

/// Specification for compiling companion C helper sources (`clang` / `cc`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileCSpec {
    /// Relative paths to `.c` source files in the workspace.
    pub sources: Vec<String>,
    /// Build a shared library (`.dylib` / `.so`) when `true`, or relocatable `.o` when `false`.
    pub shared: bool,
    /// Output artifact path relative to workspace root.
    pub output: String,
}

/// Specification for executing a compiled binary or `cobcrun` module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunBinarySpec {
    /// Relative path to executable, or module name when `via_cobcrun` is `true`.
    pub target: String,
    /// Invoke via the `cobcrun` dynamic module loader shim instead of direct execution.
    pub via_cobcrun: bool,
    /// Command-line arguments passed to the target program.
    pub args: Vec<String>,
    /// Bytes piped to the process's standard input.
    pub stdin: Vec<u8>,
    /// Environment variables exported for the child process.
    pub env: Vec<(String, String)>,
    /// Expected process exit code (`0` for normal `STOP RUN`).
    pub expected_exit_code: i32,
    /// Verification rule for `stdout`.
    pub stdout: OutputMatcher,
    /// Verification rule for `stderr`.
    pub stderr: OutputMatcher,
    /// Verification rules for files written by the program (`(relative_path, matcher)`).
    pub expected_files: Vec<(String, OutputMatcher)>,
    /// Maximum wall-clock runtime in milliseconds before process-group termination.
    pub timeout_ms: u64,
}

/// Ordered pipeline step within a [`TestManifest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestStep {
    /// Create or overwrite a file in the sandbox mid-pipeline.
    WriteFile(WorkspaceFile),
    /// Compile one or more companion C files using the host C compiler.
    CompileC(CompileCSpec),
    /// Invoke the COBOL compiler driver (`coblang` or oracle).
    CompileCobol(CompileCobolSpec),
    /// Execute a compiled program or `cobcrun` dynamic module and verify outputs.
    RunBinary(RunBinarySpec),
}

/// Complete, self-contained declarative specification of a single COBOL test case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestManifest {
    /// Globally unique canonical test identifier (e.g., `"gcobol/group1/check_88"`).
    pub id: String,
    /// Originating test suite namespace.
    pub suite: SuiteId,
    /// Upstream provenance and SPDX license record.
    pub provenance: Provenance,
    /// Initial workspace files materialized before step 0 runs.
    pub files: Vec<WorkspaceFile>,
    /// Ordered execution pipeline steps.
    pub steps: Vec<TestStep>,
    /// Optimization torture matrix (`-O0`..`-Os`); empty means run once using step defaults.
    pub torture_optimizations: Vec<OptimizationLevel>,
    /// Host/target capabilities required to run (missing capability -> `SKIP`).
    pub required_capabilities: Vec<Capability>,
    /// Static `XFAIL` reason from upstream (`AT_XFAIL_IF` / `dg-xfail-if`), if any.
    pub xfail_reason: Option<String>,
    /// Static `SKIP` reason from upstream (`AT_SKIP_IF` / `dg-skip-if` / interactive manual), if any.
    pub skip_reason: Option<String>,
    /// Compiler capability taxonomy tags assigned to this test.
    pub feature_tags: Vec<FeatureTag>,
    /// Cross-suite equivalent test identifiers for deduplicated triage.
    pub equivalent_tests: Vec<String>,
}

/// Tab field separator (`0x09`) used in manifest records.
const TAB: char = '\u{0009}';

/// Line-feed record terminator (`0x0A`) used in manifest bundles.
const LF: char = '\u{000a}';

/// Encodes a slice of [`TestManifest`] records into a deterministic UTF-8 bundle string.
///
/// # Arguments
///
/// * `manifests` - Slice of test manifests to serialize.
///
/// # Returns
///
/// Canonical line-oriented bundle text suitable for committing or caching on disk.
#[must_use]
pub fn encode_manifest_bundle(manifests: &[TestManifest]) -> String {
    let mut out = format!("COBLANG_MANIFEST_BUNDLE_V1{LF}");
    for m in manifests {
        let _ = writeln!(out, "TEST{TAB}{}", hex_encode(m.id.as_bytes()));
        let _ = writeln!(out, "SUITE{TAB}{}", m.suite.as_str());
        let _ = writeln!(
            out,
            "PROV{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}",
            hex_encode(m.provenance.upstream_repo.as_bytes()),
            hex_encode(m.provenance.upstream_commit.as_bytes()),
            hex_encode(m.provenance.source_file.as_bytes()),
            m.provenance.source_line,
            hex_encode(m.provenance.original_test_name.as_bytes()),
            hex_encode(m.provenance.spdx_license.as_bytes()),
        );
        if !m.torture_optimizations.is_empty() {
            let flags: Vec<&str> = m
                .torture_optimizations
                .iter()
                .map(|o| o.as_flag())
                .collect();
            let _ = writeln!(out, "TORTURE{TAB}{}", flags.join(","));
        }
        if !m.required_capabilities.is_empty() {
            let caps: Vec<String> = m
                .required_capabilities
                .iter()
                .map(Capability::to_token)
                .collect();
            let _ = writeln!(out, "CAPS{TAB}{}", caps.join(","));
        }
        if let Some(reason) = &m.xfail_reason {
            let _ = writeln!(out, "XFAIL{TAB}{}", hex_encode(reason.as_bytes()));
        }
        if let Some(reason) = &m.skip_reason {
            let _ = writeln!(out, "SKIP{TAB}{}", hex_encode(reason.as_bytes()));
        }
        if !m.feature_tags.is_empty() {
            let tags: Vec<&str> = m.feature_tags.iter().map(|t| t.as_str()).collect();
            let _ = writeln!(out, "TAGS{TAB}{}", tags.join(","));
        }
        if !m.equivalent_tests.is_empty() {
            let eqs: Vec<String> = m
                .equivalent_tests
                .iter()
                .map(|s| hex_encode(s.as_bytes()))
                .collect();
            let _ = writeln!(out, "EQUIV{TAB}{}", eqs.join(","));
        }
        for f in &m.files {
            let _ = writeln!(
                out,
                "FILE{TAB}{}{TAB}{}",
                hex_encode(f.relative_path.as_bytes()),
                hex_encode(&f.contents)
            );
        }
        for step in &m.steps {
            encode_step(&mut out, step);
        }
        let _ = writeln!(out, "END_TEST");
    }
    out
}

/// Serializes a single [`TestStep`] into the output buffer.
///
/// # Arguments
///
/// * `out` - Destination string buffer.
/// * `step` - Pipeline step to encode.
fn encode_step(out: &mut String, step: &TestStep) {
    match step {
        TestStep::WriteFile(wf) => {
            let _ = writeln!(
                out,
                "STEP_WRITE{TAB}{}{TAB}{}",
                hex_encode(wf.relative_path.as_bytes()),
                hex_encode(&wf.contents)
            );
        }
        TestStep::CompileC(cc) => {
            let _ = writeln!(
                out,
                "STEP_CC{TAB}{}{TAB}{}{TAB}{}",
                encode_str_list(&cc.sources),
                u8::from(cc.shared),
                hex_encode(cc.output.as_bytes())
            );
        }
        TestStep::CompileCobol(cb) => {
            let _ = writeln!(
                out,
                "STEP_COBOL{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}",
                cb.mode.as_str(),
                cb.format.as_str(),
                cb.dialect.as_str(),
                cb.optimization.as_flag(),
                hex_encode(cb.encoding.to_token().as_bytes()),
                encode_str_list(&cb.sources),
                encode_str_list(&cb.include_dirs),
                encode_str_list(&cb.copy_exts),
                encode_str_list(&cb.library_dirs),
                encode_str_list(&cb.libraries),
                encode_str_list(&cb.raw_flags),
                hex_encode(cb.output.as_bytes()),
                u8::from(cb.expected_success),
                encode_diagnostics(&cb.expected_diagnostics),
                encode_str_list(&cb.bogus_patterns),
                u8::from(cb.allow_excess_errors),
                encode_str_list(&cb.prune_patterns),
            );
            if !cb.env.is_empty() {
                let _ = writeln!(out, "STEP_COBOL_ENV{TAB}{}", encode_kv_list(&cb.env));
            }
        }
        TestStep::RunBinary(rb) => {
            let (stdout_kind, stdout_payload) = rb.stdout.to_parts();
            let (stderr_kind, stderr_payload) = rb.stderr.to_parts();
            let _ = writeln!(
                out,
                "STEP_RUN{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}{TAB}{}",
                hex_encode(rb.target.as_bytes()),
                u8::from(rb.via_cobcrun),
                encode_str_list(&rb.args),
                hex_encode(&rb.stdin),
                encode_kv_list(&rb.env),
                rb.expected_exit_code,
                stdout_kind,
                stdout_payload,
                stderr_kind,
                stderr_payload,
                rb.timeout_ms,
            );
            for (path, matcher) in &rb.expected_files {
                let (fk, fp) = matcher.to_parts();
                let _ = writeln!(
                    out,
                    "STEP_RUN_FILE{TAB}{}{TAB}{}{TAB}{}",
                    hex_encode(path.as_bytes()),
                    fk,
                    fp
                );
            }
        }
    }
}

/// Decodes a bundle text created by [`encode_manifest_bundle`] back into [`TestManifest`] items.
///
/// # Arguments
///
/// * `input` - UTF-8 bundle text to parse.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if the header, structure, or any field is malformed.
pub fn decode_manifest_bundle(input: &str) -> Result<Vec<TestManifest>> {
    let mut lines = input.lines().enumerate();
    let Some((_, header)) = lines.next() else {
        return Err(CoblangError::ManifestFormat {
            line: 1,
            message: String::from("empty manifest bundle"),
        });
    };
    if header != "COBLANG_MANIFEST_BUNDLE_V1" {
        return Err(CoblangError::ManifestFormat {
            line: 1,
            message: format!("invalid bundle header `{header}`"),
        });
    }

    let mut manifests = Vec::new();
    let mut current: Option<TestManifest> = None;

    for (idx, raw_line) in lines {
        let line_no = idx + 1;
        if raw_line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = raw_line.split(TAB).collect();
        let tag = cols[0];
        if tag == "TEST" {
            if current.is_some() {
                return Err(CoblangError::ManifestFormat {
                    line: line_no,
                    message: String::from("nested TEST without END_TEST"),
                });
            }
            require_cols(&cols, 2, line_no)?;
            current = Some(TestManifest {
                id: hex_decode_utf8(cols[1], line_no)?,
                suite: SuiteId::Canary,
                provenance: Provenance {
                    upstream_repo: String::new(),
                    upstream_commit: String::new(),
                    source_file: String::new(),
                    source_line: 1,
                    original_test_name: String::new(),
                    spdx_license: String::new(),
                },
                files: Vec::new(),
                steps: Vec::new(),
                torture_optimizations: Vec::new(),
                required_capabilities: Vec::new(),
                xfail_reason: None,
                skip_reason: None,
                feature_tags: Vec::new(),
                equivalent_tests: Vec::new(),
            });
            continue;
        }

        if tag == "END_TEST" {
            let Some(done) = current.take() else {
                return Err(CoblangError::ManifestFormat {
                    line: line_no,
                    message: String::from("END_TEST outside TEST block"),
                });
            };
            manifests.push(done);
            continue;
        }

        let Some(active) = current.as_mut() else {
            return Err(CoblangError::ManifestFormat {
                line: line_no,
                message: format!("unexpected record `{tag}` outside TEST block"),
            });
        };

        decode_record(active, &cols, line_no)?;
    }

    if current.is_some() {
        return Err(CoblangError::ManifestFormat {
            line: input.lines().count(),
            message: String::from("unterminated TEST block at end of bundle"),
        });
    }

    Ok(manifests)
}

/// Applies a single tab-delimited record inside a `TEST ... END_TEST` block.
///
/// # Arguments
///
/// * `m` - Mutable [`TestManifest`] currently being populated.
/// * `cols` - Tab-separated columns of the record line.
/// * `line` - One-based manifest line number for diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if column counts or field encodings are invalid.
fn decode_record(m: &mut TestManifest, cols: &[&str], line: usize) -> Result<()> {
    match cols[0] {
        "SUITE" => {
            require_cols(cols, 2, line)?;
            m.suite = SuiteId::parse_token(cols[1], line)?;
        }
        "PROV" => {
            require_cols(cols, 7, line)?;
            m.provenance = Provenance {
                upstream_repo: hex_decode_utf8(cols[1], line)?,
                upstream_commit: hex_decode_utf8(cols[2], line)?,
                source_file: hex_decode_utf8(cols[3], line)?,
                source_line: parse_u32(cols[4], line)?,
                original_test_name: hex_decode_utf8(cols[5], line)?,
                spdx_license: hex_decode_utf8(cols[6], line)?,
            };
        }
        "TORTURE" => {
            require_cols(cols, 2, line)?;
            m.torture_optimizations = cols[1]
                .split(',')
                .map(|t| OptimizationLevel::parse_flag(t, line))
                .collect::<Result<Vec<_>>>()?;
        }
        "CAPS" => {
            require_cols(cols, 2, line)?;
            m.required_capabilities = cols[1]
                .split(',')
                .map(|t| Capability::parse_token(t, line))
                .collect::<Result<Vec<_>>>()?;
        }
        "XFAIL" => {
            require_cols(cols, 2, line)?;
            m.xfail_reason = Some(hex_decode_utf8(cols[1], line)?);
        }
        "SKIP" => {
            require_cols(cols, 2, line)?;
            m.skip_reason = Some(hex_decode_utf8(cols[1], line)?);
        }
        "TAGS" => {
            require_cols(cols, 2, line)?;
            m.feature_tags = cols[1]
                .split(',')
                .map(|t| FeatureTag::parse_token(t, line))
                .collect::<Result<Vec<_>>>()?;
        }
        "EQUIV" => {
            require_cols(cols, 2, line)?;
            m.equivalent_tests = decode_str_list(cols[1], line)?;
        }
        "FILE" => {
            require_cols(cols, 3, line)?;
            m.files.push(WorkspaceFile {
                relative_path: hex_decode_utf8(cols[1], line)?,
                contents: hex_decode(cols[2], line)?,
            });
        }
        _ => decode_step_record(&mut m.steps, cols, line)?,
    }
    Ok(())
}

/// Decodes a pipeline step record (`STEP_*`) inside a manifest block.
///
/// # Arguments
///
/// * `steps` - Mutable step vector of the active [`TestManifest`].
/// * `cols` - Tab-separated columns of the step line.
/// * `line` - One-based manifest line number for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if the step tag or fields are malformed.
fn decode_step_record(steps: &mut Vec<TestStep>, cols: &[&str], line: usize) -> Result<()> {
    match cols[0] {
        "STEP_WRITE" => {
            require_cols(cols, 3, line)?;
            steps.push(TestStep::WriteFile(WorkspaceFile {
                relative_path: hex_decode_utf8(cols[1], line)?,
                contents: hex_decode(cols[2], line)?,
            }));
        }
        "STEP_CC" => {
            require_cols(cols, 4, line)?;
            steps.push(TestStep::CompileC(CompileCSpec {
                sources: decode_str_list(cols[1], line)?,
                shared: parse_bool01(cols[2], line)?,
                output: hex_decode_utf8(cols[3], line)?,
            }));
        }
        "STEP_COBOL" => {
            steps.push(TestStep::CompileCobol(decode_cobol_step(cols, line)?));
        }
        "STEP_COBOL_ENV" => {
            require_cols(cols, 2, line)?;
            let Some(TestStep::CompileCobol(cb)) = steps.last_mut() else {
                return Err(CoblangError::ManifestFormat {
                    line,
                    message: String::from("STEP_COBOL_ENV without preceding STEP_COBOL"),
                });
            };
            cb.env = decode_kv_list(cols[1], line)?;
        }
        "STEP_RUN" => {
            require_cols(cols, 12, line)?;
            let exit_code = cols[6]
                .parse::<i32>()
                .map_err(|_| CoblangError::ManifestFormat {
                    line,
                    message: format!("invalid exit code `{}`", cols[6]),
                })?;
            let timeout_ms = cols[11]
                .parse::<u64>()
                .map_err(|_| CoblangError::ManifestFormat {
                    line,
                    message: format!("invalid timeout `{}`", cols[11]),
                })?;
            steps.push(TestStep::RunBinary(RunBinarySpec {
                target: hex_decode_utf8(cols[1], line)?,
                via_cobcrun: parse_bool01(cols[2], line)?,
                args: decode_str_list(cols[3], line)?,
                stdin: hex_decode(cols[4], line)?,
                env: decode_kv_list(cols[5], line)?,
                expected_exit_code: exit_code,
                stdout: OutputMatcher::from_parts(cols[7], cols[8], line)?,
                stderr: OutputMatcher::from_parts(cols[9], cols[10], line)?,
                expected_files: Vec::new(),
                timeout_ms,
            }));
        }
        "STEP_RUN_FILE" => {
            require_cols(cols, 4, line)?;
            let path = hex_decode_utf8(cols[1], line)?;
            let matcher = OutputMatcher::from_parts(cols[2], cols[3], line)?;
            let Some(TestStep::RunBinary(rb)) = steps.last_mut() else {
                return Err(CoblangError::ManifestFormat {
                    line,
                    message: String::from("STEP_RUN_FILE without preceding STEP_RUN"),
                });
            };
            rb.expected_files.push((path, matcher));
        }
        other => {
            return Err(CoblangError::ManifestFormat {
                line,
                message: format!("unknown record tag `{other}`"),
            });
        }
    }
    Ok(())
}

/// Decodes a `STEP_COBOL` record into a [`CompileCobolSpec`].
///
/// # Arguments
///
/// * `cols` - Tab-separated columns of the `STEP_COBOL` line.
/// * `line` - One-based manifest line number for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if column count or any field is malformed.
fn decode_cobol_step(cols: &[&str], line: usize) -> Result<CompileCobolSpec> {
    require_cols(cols, 18, line)?;
    let dialect = Dialect::from_cli_name(cols[3]).map_err(|_| CoblangError::ManifestFormat {
        line,
        message: format!("invalid dialect `{}`", cols[3]),
    })?;
    let enc_tok = hex_decode_utf8(cols[5], line)?;
    Ok(CompileCobolSpec {
        mode: CompilationMode::parse_token(cols[1], line)?,
        format: SourceFormat::parse_token(cols[2], line)?,
        dialect,
        optimization: OptimizationLevel::parse_flag(cols[4], line)?,
        encoding: TextEncoding::parse_token(&enc_tok, line)?,
        sources: decode_str_list(cols[6], line)?,
        include_dirs: decode_str_list(cols[7], line)?,
        copy_exts: decode_str_list(cols[8], line)?,
        library_dirs: decode_str_list(cols[9], line)?,
        libraries: decode_str_list(cols[10], line)?,
        raw_flags: decode_str_list(cols[11], line)?,
        output: hex_decode_utf8(cols[12], line)?,
        expected_success: parse_bool01(cols[13], line)?,
        expected_diagnostics: decode_diagnostics(cols[14], line)?,
        bogus_patterns: decode_str_list(cols[15], line)?,
        allow_excess_errors: parse_bool01(cols[16], line)?,
        prune_patterns: decode_str_list(cols[17], line)?,
        env: Vec::new(),
    })
}

/// Validates that `cols` has exact length `expected`.
///
/// # Arguments
///
/// * `cols` - Parsed tab-separated columns.
/// * `expected` - Expected column count.
/// * `line` - Manifest line number used for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] when `cols.len() != expected`.
fn require_cols(cols: &[&str], expected: usize, line: usize) -> Result<()> {
    if cols.len() != expected {
        return Err(CoblangError::ManifestFormat {
            line,
            message: format!(
                "record `{}` expected {expected} columns, got {}",
                cols[0],
                cols.len()
            ),
        });
    }
    Ok(())
}

/// Parses an unsigned 32-bit integer field.
///
/// # Arguments
///
/// * `s` - Decimal string slice.
/// * `line` - Manifest line number used for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if `s` is not a valid `u32`.
fn parse_u32(s: &str, line: usize) -> Result<u32> {
    s.parse::<u32>().map_err(|_| CoblangError::ManifestFormat {
        line,
        message: format!("invalid u32 `{s}`"),
    })
}

/// Parses a `"0"` or `"1"` boolean field.
///
/// # Arguments
///
/// * `s` - `"0"` or `"1"` string slice.
/// * `line` - Manifest line number used for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if `s` is neither `"0"` nor `"1"`.
fn parse_bool01(s: &str, line: usize) -> Result<bool> {
    match s {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(CoblangError::ManifestFormat {
            line,
            message: format!("invalid boolean flag `{s}`"),
        }),
    }
}

/// Hex-encodes a slice of strings joined by commas (`"-" ` when empty).
///
/// # Arguments
///
/// * `items` - Strings to encode.
#[must_use]
fn encode_str_list(items: &[String]) -> String {
    if items.is_empty() {
        return String::from("-");
    }
    items
        .iter()
        .map(|s| hex_encode(s.as_bytes()))
        .collect::<Vec<_>>()
        .join(",")
}

/// Decodes a comma-separated hex string list produced by [`encode_str_list`].
///
/// # Arguments
///
/// * `raw` - Serialized string list token.
/// * `line` - Manifest line number used for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if hex or UTF-8 decoding fails.
fn decode_str_list(raw: &str, line: usize) -> Result<Vec<String>> {
    if raw == "-" {
        return Ok(Vec::new());
    }
    raw.split(',').map(|h| hex_decode_utf8(h, line)).collect()
}

/// Encodes `(key, value)` environment pairs into a comma-separated hex token.
///
/// # Arguments
///
/// * `pairs` - Key-value pairs to serialize.
#[must_use]
fn encode_kv_list(pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return String::from("-");
    }
    pairs
        .iter()
        .map(|(k, v)| format!("{}:{}", hex_encode(k.as_bytes()), hex_encode(v.as_bytes())))
        .collect::<Vec<_>>()
        .join(",")
}

/// Decodes `(key, value)` pairs serialized by [`encode_kv_list`].
///
/// # Arguments
///
/// * `raw` - Serialized key-value list token.
/// * `line` - Manifest line number used for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if any entry lacks `:` or contains invalid hex.
fn decode_kv_list(raw: &str, line: usize) -> Result<Vec<(String, String)>> {
    if raw == "-" {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for item in raw.split(',') {
        let Some((kh, vh)) = item.split_once(':') else {
            return Err(CoblangError::ManifestFormat {
                line,
                message: format!("invalid key:value entry `{item}`"),
            });
        };
        out.push((hex_decode_utf8(kh, line)?, hex_decode_utf8(vh, line)?));
    }
    Ok(out)
}

/// Encodes [`ExpectedDiagnostic`] assertions into a single compact field.
///
/// # Arguments
///
/// * `diags` - Slice of expected diagnostic assertions.
#[must_use]
fn encode_diagnostics(diags: &[ExpectedDiagnostic]) -> String {
    if diags.is_empty() {
        return String::from("-");
    }
    diags
        .iter()
        .map(|d| {
            let ln = d.line.unwrap_or(0);
            format!(
                "{}:{}:{}:{}",
                hex_encode(d.file.as_bytes()),
                ln,
                d.severity.as_str(),
                hex_encode(d.message_pattern.as_bytes())
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Decodes [`ExpectedDiagnostic`] assertions serialized by [`encode_diagnostics`].
///
/// # Arguments
///
/// * `raw` - Serialized diagnostic list field.
/// * `line` - Manifest line number used for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if any diagnostic entry is malformed.
fn decode_diagnostics(raw: &str, line: usize) -> Result<Vec<ExpectedDiagnostic>> {
    if raw == "-" {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in raw.split(',') {
        let parts: Vec<&str> = entry.split(':').collect();
        if parts.len() != 4 {
            return Err(CoblangError::ManifestFormat {
                line,
                message: format!("invalid diagnostic entry `{entry}`"),
            });
        }
        let ln = parse_u32(parts[1], line)?;
        out.push(ExpectedDiagnostic {
            file: hex_decode_utf8(parts[0], line)?,
            line: if ln == 0 { None } else { Some(ln) },
            severity: DiagnosticSeverity::parse_token(parts[2], line)?,
            message_pattern: hex_decode_utf8(parts[3], line)?,
        });
    }
    Ok(out)
}

/// Encodes raw bytes as lowercase hexadecimal (`"."` when empty so fields remain non-empty).
///
/// # Arguments
///
/// * `bytes` - Byte slice to hex-encode.
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::from(".");
    }
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Decodes lowercase hexadecimal produced by [`hex_encode`] back into raw bytes.
///
/// # Arguments
///
/// * `hex` - Hex string (`"."` represents empty byte slice).
/// * `line` - Manifest line number used for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if `hex` has odd length or non-hex digits.
pub fn hex_decode(hex: &str, line: usize) -> Result<Vec<u8>> {
    if hex == "." {
        return Ok(Vec::new());
    }
    if !hex.len().is_multiple_of(2) {
        return Err(CoblangError::ManifestFormat {
            line,
            message: format!("odd-length hex token `{hex}`"),
        });
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let raw = hex.as_bytes();
    let mut idx = 0;
    while idx < raw.len() {
        let hi = decode_nibble(raw[idx], line)?;
        let lo = decode_nibble(raw[idx + 1], line)?;
        out.push((hi << 4) | lo);
        idx += 2;
    }
    Ok(out)
}

/// Decodes a single hexadecimal ASCII character into its 4-bit value.
///
/// # Arguments
///
/// * `ch` - ASCII byte representing a hexadecimal digit.
/// * `line` - Manifest line number used for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if `ch` is not in `0-9`, `a-f`, or `A-F`.
const fn decode_nibble(ch: u8, line: usize) -> Result<u8> {
    match ch {
        b'0'..=b'9' => Ok(ch - b'0'),
        b'a'..=b'f' => Ok(ch - b'a' + 10),
        b'A'..=b'F' => Ok(ch - b'A' + 10),
        _ => Err(CoblangError::ManifestFormat {
            line,
            message: String::new(),
        }),
    }
}

/// Decodes a hex token produced by [`hex_encode`] into a validated UTF-8 [`String`].
///
/// # Arguments
///
/// * `hex` - Hexadecimal string slice (`"."` decodes to `""`).
/// * `line` - Manifest line number used for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if hex decoding or UTF-8 validation fails.
pub fn hex_decode_utf8(hex: &str, line: usize) -> Result<String> {
    let bytes = hex_decode(hex, line)?;
    String::from_utf8(bytes).map_err(|_| CoblangError::ManifestFormat {
        line,
        message: String::from("decoded bytes are not valid UTF-8"),
    })
}
