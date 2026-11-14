//! Parallel sandbox test execution engine supporting scaffold and external oracle backends.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use coblang_common::{
    CompilationMode, CompileCobolSpec, Dialect, DriverFlavor, FeatureTag, OptimizationLevel,
    OutputMatcher, Result, RunBinarySpec, SourceFormat, SuiteId, TestManifest, TestOutcome,
    TestStep, TextEncoding,
};
use coblang_scaffold_driver::{EXIT_UNIMPLEMENTED, execute_cobcrun, execute_driver};

use crate::capabilities::HostCapabilities;
use crate::matcher::{verify_compile_diagnostics, verify_output};
use crate::process::{CommandSpec, ProcessOutput, compile_c_companion, run_command_with_timeout};

/// Compiler backend used to execute COBOL compilation and `cobcrun` steps.
#[derive(Debug, Clone)]
pub enum CompilerBackend {
    /// Fast in-process zero-compiler scaffold driver (`coblang-scaffold-driver`).
    InProcessScaffold,
    /// External compiler executable (`coblang`, `cobc`, `gcobol`, or canary mock compiler).
    External {
        /// Path to the COBOL compiler binary.
        compiler_bin: PathBuf,
        /// Optional path to `cobcrun` binary for dynamic module steps.
        cobcrun_bin: Option<PathBuf>,
        /// CLI flag convention spoken by `compiler_bin`.
        flavor: DriverFlavor,
        /// Whether `ExpectedDiagnostic::message_pattern` regexes must match verbatim.
        strict_diagnostic_text: bool,
        /// Timeout in milliseconds per compiler invocation.
        compile_timeout_ms: u64,
    },
}

/// Configuration controlling sandbox creation, parallelism, capabilities, and backend selection.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Selected COBOL compiler backend.
    pub backend: CompilerBackend,
    /// Root directory under which per-test temporary sandboxes are created.
    pub sandbox_root: PathBuf,
    /// Keep sandbox directory on disk when a test ends in [`TestOutcome::Fail`].
    pub preserve_on_failure: bool,
    /// Host C compiler path/name used for [`TestStep::CompileC`] steps.
    pub c_compiler: PathBuf,
    /// Host/target capability set used to evaluate `required_capabilities`.
    pub capabilities: HostCapabilities,
    /// Number of parallel worker threads (`1` runs sequentially).
    pub jobs: usize,
}

/// Final execution result record for a single [`TestManifest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestExecutionRecord {
    /// Canonical test identifier.
    pub id: String,
    /// Suite namespace.
    pub suite: SuiteId,
    /// Final classified [`TestOutcome`].
    pub outcome: TestOutcome,
    /// Human-readable explanation when `outcome != Pass`.
    pub detail: String,
    /// Copy of taxonomy tags from the manifest for summary aggregation.
    pub feature_tags: Vec<FeatureTag>,
}

/// Internal outcome of running all pipeline steps prior to `XFAIL` classification.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RawStepDisposition {
    /// All compile and runtime steps satisfied their assertions.
    Passed,
    /// Driver returned deterministic `EXIT_UNIMPLEMENTED` (`78`).
    Unimplemented(String),
    /// Normal compile or runtime assertion mismatch (eligible for `XFAIL` if marked).
    AssertionFailed(String),
    /// Hard failure (signal crash or timeout) that MUST always report `FAIL` even if marked `XFAIL`.
    HardCrashOrTimeout(String),
}

/// Executes a slice of [`TestManifest`] items across `config.jobs` parallel worker threads.
///
/// # Arguments
///
/// * `manifests` - Slice of test manifests to execute.
/// * `config` - Runner configuration.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if sandbox directory setup or subprocess spawning fails.
pub fn execute_suite_parallel(
    manifests: &[TestManifest],
    config: &RunnerConfig,
) -> Result<Vec<TestExecutionRecord>> {
    std::fs::create_dir_all(&config.sandbox_root)?;
    if manifests.is_empty() {
        return Ok(Vec::new());
    }

    let worker_count = config.jobs.max(1).min(manifests.len());
    let next_idx = AtomicUsize::new(0);
    let mut buckets: Vec<Vec<(usize, Result<TestExecutionRecord>)>> =
        (0..worker_count).map(|_| Vec::new()).collect();

    std::thread::scope(|scope| {
        for bucket in &mut buckets {
            let idx_ref = &next_idx;
            scope.spawn(move || {
                loop {
                    let i = idx_ref.fetch_add(1, Ordering::Relaxed);
                    if i >= manifests.len() {
                        break;
                    }
                    bucket.push((i, execute_single_test(&manifests[i], config, i)));
                }
            });
        }
    });

    let mut indexed = Vec::with_capacity(manifests.len());
    for bucket in buckets {
        for (idx, res) in bucket {
            indexed.push((idx, res?));
        }
    }
    indexed.sort_by_key(|(idx, _)| *idx);
    Ok(indexed.into_iter().map(|(_, rec)| rec).collect())
}

/// Executes a single [`TestManifest`] inside its own isolated sandbox directory.
///
/// # Arguments
///
/// * `manifest` - Declarative test manifest to execute.
/// * `config` - Runner configuration.
/// * `slot` - Unique numeric index disambiguating sandbox folder names.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if filesystem sandbox operations or process spawning fail.
pub fn execute_single_test(
    manifest: &TestManifest,
    config: &RunnerConfig,
    slot: usize,
) -> Result<TestExecutionRecord> {
    if let Some(reason) = &manifest.skip_reason {
        return Ok(TestExecutionRecord {
            id: manifest.id.clone(),
            suite: manifest.suite,
            outcome: TestOutcome::Skip,
            detail: reason.clone(),
            feature_tags: manifest.feature_tags.clone(),
        });
    }
    if let Some(missing) = config
        .capabilities
        .first_missing(&manifest.required_capabilities)
    {
        return Ok(TestExecutionRecord {
            id: manifest.id.clone(),
            suite: manifest.suite,
            outcome: TestOutcome::Skip,
            detail: format!("missing capability `{}`", missing.to_token()),
            feature_tags: manifest.feature_tags.clone(),
        });
    }

    let safe_name: String = manifest
        .id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let sandbox = config
        .sandbox_root
        .join(format!("sbx-{}-{slot}-{safe_name}", std::process::id()));
    if sandbox.exists() {
        std::fs::remove_dir_all(&sandbox)?;
    }
    std::fs::create_dir_all(&sandbox)?;

    let disposition = run_torture_iterations(manifest, config, &sandbox)?;
    let (outcome, detail) = classify_disposition(disposition, manifest.xfail_reason.as_deref());

    if !(outcome == TestOutcome::Fail && config.preserve_on_failure) {
        let _ = std::fs::remove_dir_all(&sandbox);
    }

    Ok(TestExecutionRecord {
        id: manifest.id.clone(),
        suite: manifest.suite,
        outcome,
        detail,
        feature_tags: manifest.feature_tags.clone(),
    })
}

/// Maps [`RawStepDisposition`] and optional `xfail_reason` to final `(TestOutcome, detail)`.
///
/// # Arguments
///
/// * `disp` - Raw pipeline execution disposition.
/// * `xfail_reason` - Static `XFAIL` annotation on the test manifest, if present.
#[must_use]
fn classify_disposition(
    disp: RawStepDisposition,
    xfail_reason: Option<&str>,
) -> (TestOutcome, String) {
    match disp {
        RawStepDisposition::HardCrashOrTimeout(msg) => (TestOutcome::Fail, msg),
        RawStepDisposition::Unimplemented(msg) => (TestOutcome::XFail, msg),
        RawStepDisposition::Passed => {
            if let Some(reason) = xfail_reason {
                (TestOutcome::XPass, format!("unexpected pass ({reason})"))
            } else {
                (TestOutcome::Pass, String::new())
            }
        }
        RawStepDisposition::AssertionFailed(msg) => {
            if let Some(reason) = xfail_reason {
                (TestOutcome::XFail, format!("{reason}: {msg}"))
            } else {
                (TestOutcome::Fail, msg)
            }
        }
    }
}

/// Runs the manifest's step pipeline across all configured torture optimization levels.
///
/// # Arguments
///
/// * `manifest` - Test manifest to execute.
/// * `config` - Runner configuration.
/// * `sandbox` - Isolated sandbox directory.
///
/// # Errors
///
/// Propagates I/O, regex, or spawn errors.
fn run_torture_iterations(
    manifest: &TestManifest,
    config: &RunnerConfig,
    sandbox: &Path,
) -> Result<RawStepDisposition> {
    let levels: Vec<Option<OptimizationLevel>> = if manifest.torture_optimizations.is_empty() {
        vec![None]
    } else {
        manifest
            .torture_optimizations
            .iter()
            .copied()
            .map(Some)
            .collect()
    };

    for opt_override in levels {
        materialize_files(&manifest.files, sandbox)?;
        for step in &manifest.steps {
            let disp = execute_step(step, opt_override, manifest, config, sandbox)?;
            if disp != RawStepDisposition::Passed {
                return Ok(disp);
            }
        }
    }
    Ok(RawStepDisposition::Passed)
}

/// Writes workspace files into `sandbox`, creating parent subdirectories as needed.
///
/// # Arguments
///
/// * `files` - Slice of workspace files to materialize.
/// * `sandbox` - Sandbox root directory.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError::Io`] if writing fails.
fn materialize_files(files: &[coblang_common::WorkspaceFile], sandbox: &Path) -> Result<()> {
    for wf in files {
        if let Some((dir, _)) = wf.relative_path.rsplit_once('/') {
            std::fs::create_dir_all(sandbox.join(dir))?;
        }
        std::fs::write(sandbox.join(&wf.relative_path), &wf.contents)?;
    }
    Ok(())
}

/// Executes a single [`TestStep`] inside `sandbox`.
///
/// # Arguments
///
/// * `step` - Step to run.
/// * `opt_override` - Torture optimization override, if active.
/// * `manifest` - Owning test manifest.
/// * `config` - Runner configuration.
/// * `sandbox` - Sandbox root directory.
///
/// # Errors
///
/// Propagates I/O, regex, or spawn errors.
fn execute_step(
    step: &TestStep,
    opt_override: Option<OptimizationLevel>,
    manifest: &TestManifest,
    config: &RunnerConfig,
    sandbox: &Path,
) -> Result<RawStepDisposition> {
    match step {
        TestStep::WriteFile(wf) => {
            materialize_files(core::slice::from_ref(wf), sandbox)?;
            Ok(RawStepDisposition::Passed)
        }
        TestStep::CompileC(cc) => {
            if let Some(err) =
                compile_c_companion(cc, sandbox, &config.c_compiler, 15_000, &manifest.id)?
            {
                return Ok(RawStepDisposition::AssertionFailed(err));
            }
            Ok(RawStepDisposition::Passed)
        }
        TestStep::CompileCobol(cb) => {
            execute_cobol_compile(cb, opt_override, &manifest.id, &config.backend, sandbox)
        }
        TestStep::RunBinary(rb) => execute_run_binary(rb, &manifest.id, &config.backend, sandbox),
    }
}

/// Invokes the COBOL compiler backend and checks exit status and diagnostics.
///
/// # Arguments
///
/// * `spec` - COBOL compilation specification.
/// * `opt_override` - Optional torture optimization level override.
/// * `test_id` - Test identifier.
/// * `backend` - Selected compiler backend.
/// * `sandbox` - Sandbox directory.
///
/// # Errors
///
/// Propagates regex or subprocess launch errors.
fn execute_cobol_compile(
    spec: &CompileCobolSpec,
    opt_override: Option<OptimizationLevel>,
    test_id: &str,
    backend: &CompilerBackend,
    sandbox: &Path,
) -> Result<RawStepDisposition> {
    let opt = opt_override.unwrap_or(spec.optimization);
    match backend {
        CompilerBackend::InProcessScaffold => {
            let args = build_driver_cli_args(DriverFlavor::Coblang, spec, opt);
            let mut out = Vec::new();
            let mut err = Vec::new();
            let code = execute_driver(DriverFlavor::Coblang, &args, sandbox, &mut out, &mut err);
            if code == EXIT_UNIMPLEMENTED {
                return Ok(RawStepDisposition::Unimplemented(
                    String::from_utf8_lossy(&err).trim().to_string(),
                ));
            }
            Ok(RawStepDisposition::AssertionFailed(format!(
                "scaffold driver returned exit {code}: {}",
                String::from_utf8_lossy(&err).trim()
            )))
        }
        CompilerBackend::External {
            compiler_bin,
            flavor,
            strict_diagnostic_text,
            compile_timeout_ms,
            ..
        } => {
            let args = build_driver_cli_args(*flavor, spec, opt);
            let proc_out = run_command_with_timeout(
                &CommandSpec {
                    program: compiler_bin,
                    args: &args,
                    cwd: sandbox,
                    env: &spec.env,
                    stdin_bytes: &[],
                    timeout_ms: *compile_timeout_ms,
                },
                test_id,
            )?;
            evaluate_external_compile(spec, &proc_out, *strict_diagnostic_text)
        }
    }
}

/// Evaluates [`ProcessOutput`] from an external COBOL compiler against [`CompileCobolSpec`].
///
/// # Arguments
///
/// * `spec` - Expected compilation outcome and diagnostics.
/// * `proc_out` - Captured compiler process output.
/// * `strict_diag` - Whether diagnostic regexes must match verbatim.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError::InvalidRegex`] if any diagnostic regex is malformed.
fn evaluate_external_compile(
    spec: &CompileCobolSpec,
    proc_out: &ProcessOutput,
    strict_diag: bool,
) -> Result<RawStepDisposition> {
    if proc_out.timed_out {
        return Ok(RawStepDisposition::HardCrashOrTimeout(String::from(
            "compiler timed out",
        )));
    }
    if let Some(sig) = proc_out.signal {
        return Ok(RawStepDisposition::HardCrashOrTimeout(format!(
            "compiler crashed with signal {sig}"
        )));
    }
    if proc_out.exit_code == Some(i32::from(EXIT_UNIMPLEMENTED)) {
        return Ok(RawStepDisposition::Unimplemented(
            String::from_utf8_lossy(&proc_out.stderr).trim().to_string(),
        ));
    }

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&proc_out.stdout),
        String::from_utf8_lossy(&proc_out.stderr)
    );
    if let Some(diag_err) = verify_compile_diagnostics(
        &combined,
        &spec.expected_diagnostics,
        &spec.bogus_patterns,
        &spec.prune_patterns,
        spec.allow_excess_errors,
        strict_diag,
    )? {
        return Ok(RawStepDisposition::AssertionFailed(diag_err));
    }

    let succeeded = proc_out.exit_code == Some(0);
    if succeeded != spec.expected_success {
        return Ok(RawStepDisposition::AssertionFailed(format!(
            "expected compile success={}, got exit {:?}: {}",
            spec.expected_success,
            proc_out.exit_code,
            combined.trim()
        )));
    }
    Ok(RawStepDisposition::Passed)
}

/// Translates a [`CompileCobolSpec`] into concrete CLI flags for the target [`DriverFlavor`].
///
/// # Arguments
///
/// * `flavor` - Target compiler CLI flavor (`Coblang`, `Cobc`, or `Gcobol`).
/// * `spec` - Compilation specification.
/// * `opt` - Effective optimization level.
#[must_use]
pub fn build_driver_cli_args(
    flavor: DriverFlavor,
    spec: &CompileCobolSpec,
    opt: OptimizationLevel,
) -> Vec<String> {
    let mut args = Vec::new();
    match spec.mode {
        CompilationMode::PreprocessOnly => args.push(String::from("-E")),
        CompilationMode::SyntaxOnly => args.push(String::from("-fsyntax-only")),
        CompilationMode::EmitAssembly => args.push(String::from("-S")),
        CompilationMode::EmitLlvmIr => args.push(String::from("-emit-llvm")),
        CompilationMode::CompileObject => args.push(String::from("-c")),
        CompilationMode::CompileModule => {
            let flag = if flavor == DriverFlavor::Gcobol {
                "-shared"
            } else {
                "-m"
            };
            args.push(String::from(flag));
        }
        CompilationMode::BuildExecutable => {
            if flavor != DriverFlavor::Gcobol {
                args.push(String::from("-x"));
            }
        }
    }

    match spec.format {
        SourceFormat::Auto => {}
        SourceFormat::Fixed => {
            let flag = if flavor == DriverFlavor::Gcobol {
                "-ffixed-form"
            } else {
                "-fixed"
            };
            args.push(String::from(flag));
        }
        SourceFormat::Free => {
            let flag = if flavor == DriverFlavor::Gcobol {
                "-ffree-form"
            } else {
                "-free"
            };
            args.push(String::from(flag));
        }
    }

    append_dialect_and_encoding_args(&mut args, flavor, spec.dialect, &spec.encoding);
    args.push(String::from(opt.as_flag()));

    for dir in &spec.include_dirs {
        args.push(String::from("-I"));
        args.push(dir.clone());
    }
    for ext in &spec.copy_exts {
        let flag = if flavor == DriverFlavor::Gcobol {
            "-copyext"
        } else {
            "-ext"
        };
        args.push(String::from(flag));
        args.push(ext.clone());
    }
    for dir in &spec.library_dirs {
        args.push(String::from("-L"));
        args.push(dir.clone());
    }
    for lib in &spec.libraries {
        args.push(String::from("-l"));
        args.push(lib.clone());
    }
    args.extend(spec.raw_flags.iter().cloned());
    if !spec.output.is_empty() {
        args.push(String::from("-o"));
        args.push(spec.output.clone());
    }
    args.extend(spec.sources.iter().cloned());
    args
}

/// Appends dialect and character-encoding flags appropriate for `flavor`.
///
/// # Arguments
///
/// * `args` - Argument vector being constructed.
/// * `flavor` - Target compiler CLI flavor.
/// * `dialect` - Requested COBOL dialect.
/// * `encoding` - Requested text encoding.
fn append_dialect_and_encoding_args(
    args: &mut Vec<String>,
    flavor: DriverFlavor,
    dialect: Dialect,
    encoding: &TextEncoding,
) {
    if dialect != Dialect::Default {
        if flavor == DriverFlavor::Gcobol {
            let gcob_name = match dialect {
                Dialect::Ibm => Some("ibm"),
                Dialect::MicroFocus => Some("mf"),
                Dialect::Gnu => Some("gnu"),
                Dialect::Cobol2023 => Some("iso"),
                _ => None,
            };
            if let Some(d) = gcob_name {
                args.push(String::from("-dialect"));
                args.push(String::from(d));
            }
        } else if flavor == DriverFlavor::Cobc && dialect == Dialect::Gnu {
            args.push(String::from("-std=default"));
        } else {
            args.push(format!("-std={}", dialect.as_str()));
        }
    }
    match encoding {
        TextEncoding::Ascii => {}
        TextEncoding::Ebcdic => args.push(String::from("-finternal-ebcdic")),
        TextEncoding::Codepage(cp) => args.push(format!("-fexec-charset={cp}")),
    }
}

/// Executes a [`RunBinarySpec`] step and verifies exit code, stdout, stderr, and output files.
///
/// # Arguments
///
/// * `spec` - Runtime execution specification.
/// * `test_id` - Test identifier.
/// * `backend` - Active compiler backend (providing `cobcrun` when `via_cobcrun` is `true`).
/// * `sandbox` - Sandbox directory.
///
/// # Errors
///
/// Propagates subprocess or regex errors.
fn execute_run_binary(
    spec: &RunBinarySpec,
    test_id: &str,
    backend: &CompilerBackend,
    sandbox: &Path,
) -> Result<RawStepDisposition> {
    if spec.via_cobcrun
        && let CompilerBackend::InProcessScaffold = backend
    {
        let mut cr_args = vec![spec.target.clone()];
        cr_args.extend(spec.args.iter().cloned());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute_cobcrun(&cr_args, None, &mut out, &mut err);
        return Ok(RawStepDisposition::Unimplemented(format!(
            "cobcrun exit {code}: {}",
            String::from_utf8_lossy(&err).trim()
        )));
    }

    let (program, full_args) = resolve_run_command(spec, backend, sandbox);
    let proc_out = run_command_with_timeout(
        &CommandSpec {
            program: &program,
            args: &full_args,
            cwd: sandbox,
            env: &spec.env,
            stdin_bytes: &spec.stdin,
            timeout_ms: spec.timeout_ms,
        },
        test_id,
    )?;

    if proc_out.timed_out {
        return Ok(RawStepDisposition::HardCrashOrTimeout(format!(
            "program timed out after {}ms",
            spec.timeout_ms
        )));
    }
    if let Some(sig) = proc_out.signal {
        return Ok(RawStepDisposition::HardCrashOrTimeout(format!(
            "program crashed with signal {sig}"
        )));
    }
    if proc_out.exit_code != Some(spec.expected_exit_code) {
        return Ok(RawStepDisposition::AssertionFailed(format!(
            "expected exit code {}, got {:?}",
            spec.expected_exit_code, proc_out.exit_code
        )));
    }
    if let Some(mismatch) = verify_output(&spec.stdout, &proc_out.stdout)? {
        return Ok(RawStepDisposition::AssertionFailed(format!(
            "stdout: {mismatch}"
        )));
    }
    if let Some(mismatch) = verify_output(&spec.stderr, &proc_out.stderr)? {
        return Ok(RawStepDisposition::AssertionFailed(format!(
            "stderr: {mismatch}"
        )));
    }
    verify_expected_files(&spec.expected_files, sandbox)
}

/// Resolves executable path and arguments for direct binary vs `cobcrun` module execution.
///
/// # Arguments
///
/// * `spec` - Runtime execution specification.
/// * `backend` - Active compiler backend.
/// * `sandbox` - Sandbox directory.
#[must_use]
fn resolve_run_command(
    spec: &RunBinarySpec,
    backend: &CompilerBackend,
    sandbox: &Path,
) -> (PathBuf, Vec<String>) {
    if spec.via_cobcrun {
        let bin = match backend {
            CompilerBackend::External {
                cobcrun_bin: Some(p),
                ..
            } => p.clone(),
            _ => PathBuf::from("cobcrun"),
        };
        let mut args = vec![spec.target.clone()];
        args.extend(spec.args.iter().cloned());
        (bin, args)
    } else {
        (sandbox.join(&spec.target), spec.args.clone())
    }
}

/// Verifies all expected workspace output files declared on a [`RunBinarySpec`].
///
/// # Arguments
///
/// * `expected_files` - Pairs of `(relative_path, OutputMatcher)`.
/// * `sandbox` - Sandbox directory.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError::InvalidRegex`] if a file regex matcher is malformed.
fn verify_expected_files(
    expected_files: &[(String, OutputMatcher)],
    sandbox: &Path,
) -> Result<RawStepDisposition> {
    for (rel_path, matcher) in expected_files {
        let full = sandbox.join(rel_path);
        let Ok(bytes) = std::fs::read(&full) else {
            return Ok(RawStepDisposition::AssertionFailed(format!(
                "expected output file `{rel_path}` was not created"
            )));
        };
        if let Some(mismatch) = verify_output(matcher, &bytes)? {
            return Ok(RawStepDisposition::AssertionFailed(format!(
                "file `{rel_path}`: {mismatch}"
            )));
        }
    }
    Ok(RawStepDisposition::Passed)
}
