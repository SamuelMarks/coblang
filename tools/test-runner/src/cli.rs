//! Command-line interface for `coblang-test` (`run`, `run-one`, `summary`, `emit-lit`).

use std::io::Write;
use std::path::{Path, PathBuf};

use coblang_common::{CoblangError, DriverFlavor, Result, TestManifest, decode_manifest_bundle};

use crate::capabilities::HostCapabilities;
use crate::engine::{CompilerBackend, RunnerConfig, execute_suite_parallel};
use crate::ratchet::{
    decode_expectations, emit_lit_wrappers, encode_expectations, render_summary_json,
    render_summary_markdown, snapshot_expectations, verify_ratchet,
};

/// Line-feed character (`0x0A`).
const LF: char = '\u{000a}';

/// Parsed subcommand for the `coblang-test` runner CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerCommand {
    /// Print CLI usage and exit `0`.
    Help,
    /// Execute all manifests in one or more bundle files.
    Run {
        /// Bundle file paths to load and execute.
        bundles: Vec<PathBuf>,
        /// Optional expectation baseline file path to verify against when `ratchet` is `true`.
        baseline: Option<PathBuf>,
        /// Optional path where a fresh baseline snapshot should be written.
        write_baseline: Option<PathBuf>,
        /// Enforce ratchet (`FAIL`/`XPASS`/regression returns non-zero).
        ratchet: bool,
        /// Optional oracle compiler (`"cobc"` or `"gcobol"`).
        oracle: Option<String>,
        /// Worker thread count.
        jobs: usize,
    },
    /// Execute a single test by ID from a bundle (used by `llvm-lit`).
    RunOne {
        /// Bundle file path.
        bundle: PathBuf,
        /// Canonical test identifier to execute.
        test_id: String,
    },
    /// Execute bounds-checked or full-tree bundles and print the Markdown/JSON conformance scoreboard.
    Summary {
        /// Bundle file paths to execute and summarize.
        bundles: Vec<PathBuf>,
        /// Optional output Markdown file path.
        output: Option<PathBuf>,
        /// Optional output JSON file path.
        json_output: Option<PathBuf>,
        /// Worker thread count.
        jobs: usize,
    },
    /// Emit `llvm-lit` `.lit.test` wrapper files for a bundle.
    EmitLit {
        /// Input manifest bundle path.
        bundle: PathBuf,
        /// Destination directory for `.lit.test` files.
        out_dir: PathBuf,
    },
}

/// Parses `coblang-test` command-line arguments into a [`RunnerCommand`].
///
/// # Arguments
///
/// * `args` - CLI arguments excluding `argv[0]`.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] if subcommand or flag syntax is invalid.
pub fn parse_runner_cli(args: &[String]) -> Result<RunnerCommand> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(CoblangError::InvalidCli {
            message: String::from("missing subcommand (expected run|run-one|summary|emit-lit)"),
        });
    };
    match sub {
        "--help" | "-h" | "help" => Ok(RunnerCommand::Help),
        "run" => parse_run_subcommand(&args[1..]),
        "run-one" => parse_run_one_subcommand(&args[1..]),
        "summary" => parse_summary_subcommand(&args[1..]),
        "emit-lit" => parse_emit_lit_subcommand(&args[1..]),
        other => Err(CoblangError::InvalidCli {
            message: format!("unknown coblang-test subcommand `{other}`"),
        }),
    }
}

/// Parses `coblang-test run` options.
///
/// # Arguments
///
/// * `args` - Arguments following `run`.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] on missing values or empty bundle list.
fn parse_run_subcommand(args: &[String]) -> Result<RunnerCommand> {
    let mut bundles = Vec::new();
    let mut baseline = None;
    let mut write_baseline = None;
    let mut ratchet = false;
    let mut oracle = None;
    let mut jobs = 8_usize;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bundle" => {
                i += 1;
                bundles.push(PathBuf::from(req_arg(args, i, "--bundle")?));
            }
            "--bundle-dir" => {
                i += 1;
                let dir = PathBuf::from(req_arg(args, i, "--bundle-dir")?);
                collect_bundles_recursive(&dir, &mut bundles)?;
            }
            "--baseline" => {
                i += 1;
                baseline = Some(PathBuf::from(req_arg(args, i, "--baseline")?));
            }
            "--write-baseline" => {
                i += 1;
                write_baseline = Some(PathBuf::from(req_arg(args, i, "--write-baseline")?));
            }
            "--ratchet" => ratchet = true,
            "--oracle" => {
                i += 1;
                oracle = Some(req_arg(args, i, "--oracle")?);
            }
            "--jobs" | "-j" => {
                i += 1;
                let raw = req_arg(args, i, "--jobs")?;
                jobs = raw.parse::<usize>().map_err(|_| CoblangError::InvalidCli {
                    message: format!("invalid --jobs count `{raw}`"),
                })?;
            }
            flag => {
                return Err(CoblangError::InvalidCli {
                    message: format!("unknown run option `{flag}`"),
                });
            }
        }
        i += 1;
    }
    if bundles.is_empty() {
        return Err(CoblangError::InvalidCli {
            message: String::from("at least one --bundle is required"),
        });
    }
    Ok(RunnerCommand::Run {
        bundles,
        baseline,
        write_baseline,
        ratchet,
        oracle,
        jobs,
    })
}

/// Parses `coblang-test run-one --bundle <path> --id <id>`.
///
/// # Arguments
///
/// * `args` - Arguments following `run-one`.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] if `--bundle` or `--id` is missing.
fn parse_run_one_subcommand(args: &[String]) -> Result<RunnerCommand> {
    let mut bundle = None;
    let mut test_id = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bundle" => {
                i += 1;
                bundle = Some(PathBuf::from(req_arg(args, i, "--bundle")?));
            }
            "--id" => {
                i += 1;
                test_id = Some(req_arg(args, i, "--id")?);
            }
            flag => {
                return Err(CoblangError::InvalidCli {
                    message: format!("unknown run-one option `{flag}`"),
                });
            }
        }
        i += 1;
    }
    let (Some(bundle), Some(test_id)) = (bundle, test_id) else {
        return Err(CoblangError::InvalidCli {
            message: String::from("run-one requires both --bundle and --id"),
        });
    };
    Ok(RunnerCommand::RunOne { bundle, test_id })
}

/// Parses `coblang-test summary --bundle <path>... [--output <md>] [--jobs <n>]`.
///
/// # Arguments
///
/// * `args` - Arguments following `summary`.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] if no `--bundle` is supplied or options are malformed.
fn parse_summary_subcommand(args: &[String]) -> Result<RunnerCommand> {
    let mut bundles = Vec::new();
    let mut output = None;
    let mut json_output = None;
    let mut jobs = 8_usize;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bundle" => {
                i += 1;
                bundles.push(PathBuf::from(req_arg(args, i, "--bundle")?));
            }
            "--bundle-dir" => {
                i += 1;
                let dir = PathBuf::from(req_arg(args, i, "--bundle-dir")?);
                collect_bundles_recursive(&dir, &mut bundles)?;
            }
            "--output" => {
                i += 1;
                output = Some(PathBuf::from(req_arg(args, i, "--output")?));
            }
            "--json" => {
                i += 1;
                json_output = Some(PathBuf::from(req_arg(args, i, "--json")?));
            }
            "--jobs" | "-j" => {
                i += 1;
                let raw = req_arg(args, i, "--jobs")?;
                jobs = raw.parse::<usize>().map_err(|_| CoblangError::InvalidCli {
                    message: format!("invalid --jobs count `{raw}`"),
                })?;
            }
            flag => {
                return Err(CoblangError::InvalidCli {
                    message: format!("unknown summary option `{flag}`"),
                });
            }
        }
        i += 1;
    }
    if bundles.is_empty() {
        return Err(CoblangError::InvalidCli {
            message: String::from("summary requires at least one --bundle"),
        });
    }
    Ok(RunnerCommand::Summary {
        bundles,
        output,
        json_output,
        jobs,
    })
}

/// Recursively collects sorted `*.bundle` paths under `dir`.
///
/// # Arguments
///
/// * `dir` - Root directory to traverse.
/// * `out` - Destination path vector.
///
/// # Errors
///
/// Returns [`CoblangError::Io`] if reading `dir` fails.
pub fn collect_bundles_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        entries.push(entry?.path());
    }
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_bundles_recursive(&p, out)?;
        } else if p.extension().is_some_and(|e| e == "bundle") {
            out.push(p);
        }
    }
    Ok(())
}

/// Parses `coblang-test emit-lit --bundle <path> --out-dir <dir>`.
///
/// # Arguments
///
/// * `args` - Arguments following `emit-lit`.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] if `--bundle` or `--out-dir` is missing.
fn parse_emit_lit_subcommand(args: &[String]) -> Result<RunnerCommand> {
    let mut bundle = None;
    let mut out_dir = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bundle" => {
                i += 1;
                bundle = Some(PathBuf::from(req_arg(args, i, "--bundle")?));
            }
            "--out-dir" => {
                i += 1;
                out_dir = Some(PathBuf::from(req_arg(args, i, "--out-dir")?));
            }
            flag => {
                return Err(CoblangError::InvalidCli {
                    message: format!("unknown emit-lit option `{flag}`"),
                });
            }
        }
        i += 1;
    }
    let (Some(bundle), Some(out_dir)) = (bundle, out_dir) else {
        return Err(CoblangError::InvalidCli {
            message: String::from("emit-lit requires both --bundle and --out-dir"),
        });
    };
    Ok(RunnerCommand::EmitLit { bundle, out_dir })
}

/// Extracts a required option argument at `idx`.
///
/// # Arguments
///
/// * `args` - Full option slice.
/// * `idx` - Value index.
/// * `flag` - Option flag name.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] if `idx >= args.len()`.
fn req_arg(args: &[String], idx: usize, flag: &str) -> Result<String> {
    args.get(idx)
        .cloned()
        .ok_or_else(|| CoblangError::InvalidCli {
            message: format!("missing value after `{flag}`"),
        })
}

/// Resolves an optional `--oracle` string into a [`CompilerBackend`].
///
/// # Arguments
///
/// * `oracle` - Optional oracle name (`None`, `Some("cobc")`, or `Some("gcobol")`).
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] if `oracle` is unrecognized.
pub fn resolve_oracle_backend(oracle: Option<&str>) -> Result<CompilerBackend> {
    let Some(name) = oracle else {
        return Ok(CompilerBackend::InProcessScaffold);
    };
    match name {
        "cobc" => Ok(CompilerBackend::External {
            compiler_bin: PathBuf::from("cobc"),
            cobcrun_bin: Some(PathBuf::from("cobcrun")),
            flavor: DriverFlavor::Cobc,
            strict_diagnostic_text: false,
            compile_timeout_ms: 15_000,
        }),
        "gcobol" => Ok(CompilerBackend::External {
            compiler_bin: PathBuf::from("gcobol"),
            cobcrun_bin: None,
            flavor: DriverFlavor::Gcobol,
            strict_diagnostic_text: false,
            compile_timeout_ms: 15_000,
        }),
        other => Err(CoblangError::InvalidCli {
            message: format!("unsupported --oracle `{other}` (expected cobc or gcobol)"),
        }),
    }
}

/// Loads and concatenates [`TestManifest`] entries from multiple bundle files.
///
/// # Arguments
///
/// * `paths` - Bundle file paths to read and decode.
///
/// # Errors
///
/// Propagates I/O or manifest format errors.
fn load_bundles(paths: &[PathBuf]) -> Result<Vec<TestManifest>> {
    let mut all = Vec::new();
    for p in paths {
        let content = std::fs::read_to_string(p)?;
        all.extend(decode_manifest_bundle(&content)?);
    }
    Ok(all)
}

/// Executes a [`RunnerCommand`] writing output to `stdout`/`stderr` and returning an exit code.
///
/// # Arguments
///
/// * `args` - Command-line arguments excluding `argv[0]`.
/// * `sandbox_root` - Directory where temporary per-test sandboxes are created.
/// * `stdout` - Standard output stream.
/// * `stderr` - Standard error stream.
///
/// # Returns
///
/// Exit code (`0` on success, `1` on test/ratchet/runtime failure, `2` on CLI usage error).
pub fn run_cli<W1: Write, W2: Write>(
    args: &[String],
    sandbox_root: &Path,
    stdout: &mut W1,
    stderr: &mut W2,
) -> u8 {
    let cmd = match parse_runner_cli(args) {
        Ok(c) => c,
        Err(err) => {
            let _ = write!(stderr, "coblang-test: error: {err}{LF}");
            return 2;
        }
    };
    match dispatch_command(cmd, sandbox_root, stdout) {
        Ok(()) => 0,
        Err(err) => {
            let _ = write!(stderr, "coblang-test: {err}{LF}");
            1
        }
    }
}

/// Dispatches a parsed [`RunnerCommand`].
///
/// # Arguments
///
/// * `cmd` - Parsed runner command.
/// * `sandbox_root` - Sandbox base directory.
/// * `stdout` - Standard output stream.
///
/// # Errors
///
/// Propagates execution, I/O, manifest, or ratchet errors.
fn dispatch_command<W: Write>(
    cmd: RunnerCommand,
    sandbox_root: &Path,
    stdout: &mut W,
) -> Result<()> {
    match cmd {
        RunnerCommand::Help => {
            let _ = write!(
                stdout,
                "Usage: coblang-test <run|run-one|summary|emit-lit> [options]{LF}"
            );
            Ok(())
        }
        RunnerCommand::Run {
            bundles,
            baseline,
            write_baseline,
            ratchet,
            oracle,
            jobs,
        } => {
            let manifests = load_bundles(&bundles)?;
            let cfg = RunnerConfig {
                backend: resolve_oracle_backend(oracle.as_deref())?,
                sandbox_root: sandbox_root.to_path_buf(),
                preserve_on_failure: false,
                c_compiler: PathBuf::from("clang"),
                capabilities: HostCapabilities::detect_host(),
                jobs,
            };
            let records = execute_suite_parallel(&manifests, &cfg)?;
            if let Some(base_path) = &write_baseline {
                let snap = snapshot_expectations(&records);
                std::fs::write(base_path, encode_expectations(&snap))?;
            }
            if ratchet {
                let loaded = match &baseline {
                    Some(p) => Some(decode_expectations(&std::fs::read_to_string(p)?)?),
                    None => None,
                };
                verify_ratchet(&records, loaded.as_ref())?;
            }
            let _ = write!(stdout, "Executed {} test(s).{LF}", records.len());
            Ok(())
        }
        RunnerCommand::RunOne { bundle, test_id } => {
            let manifests = load_bundles(&[bundle])?;
            let selected: Vec<TestManifest> =
                manifests.into_iter().filter(|m| m.id == test_id).collect();
            if selected.is_empty() {
                return Err(CoblangError::RunnerExecution {
                    test_id,
                    message: String::from("test ID not found in bundle"),
                });
            }
            let cfg = RunnerConfig {
                backend: CompilerBackend::InProcessScaffold,
                sandbox_root: sandbox_root.to_path_buf(),
                preserve_on_failure: false,
                c_compiler: PathBuf::from("clang"),
                capabilities: HostCapabilities::detect_host(),
                jobs: 1,
            };
            let records = execute_suite_parallel(&selected, &cfg)?;
            verify_ratchet(&records, None)
        }
        RunnerCommand::Summary {
            bundles,
            output,
            json_output,
            jobs,
        } => {
            let manifests = load_bundles(&bundles)?;
            let cfg = RunnerConfig {
                backend: CompilerBackend::InProcessScaffold,
                sandbox_root: sandbox_root.to_path_buf(),
                preserve_on_failure: false,
                c_compiler: PathBuf::from("clang"),
                capabilities: HostCapabilities::detect_host(),
                jobs,
            };
            let records = execute_suite_parallel(&manifests, &cfg)?;
            let md = render_summary_markdown(&records);
            if let Some(path) = output {
                std::fs::write(path, &md)?;
            }
            if let Some(jpath) = json_output {
                std::fs::write(jpath, render_summary_json(&records))?;
            }
            let _ = write!(stdout, "{md}");
            Ok(())
        }
        RunnerCommand::EmitLit { bundle, out_dir } => {
            let manifests = load_bundles(core::slice::from_ref(&bundle))?;
            let count = emit_lit_wrappers(&manifests, &bundle, &out_dir)?;
            let _ = write!(stdout, "Emitted {count} llvm-lit test wrapper(s).{LF}");
            Ok(())
        }
    }
}
