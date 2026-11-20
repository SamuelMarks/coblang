//! Minimal zero-compiler scaffold CLI driver and compatibility shims (`coblang`, `cobc`, `cobcrun`, `gcobol`).

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use coblang_common::{
    CoblangError, CompilationMode, Dialect, DriverFlavor, OptimizationLevel, Result, SourceFormat,
    TextEncoding,
};

/// Real incremental compiler pipeline runner (`-E` preprocessor, `-t`/`-T` listings, `-conf=`).
pub mod pipeline;
pub use coblang_dynload::open_raw_pty_pair;
pub use pipeline::{execute_cobcrun_driver, execute_compiler_driver};

/// Deterministic exit status (`78`) emitted when compilation or module loading is unimplemented.
pub const EXIT_UNIMPLEMENTED: u8 = 78;

/// Deterministic exit status (`2`) emitted on invalid CLI flag usage.
pub const EXIT_CLI_USAGE: u8 = 2;

/// Deterministic exit status (`1`) emitted when an input source file is missing on disk.
pub const EXIT_INPUT_ERROR: u8 = 1;

/// Line-feed character (`0x0A`) used when formatting diagnostics.
const LF: char = '\u{000a}';

/// Informational CLI query short-circuiting compilation (`--version` or `--help`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverQuery {
    /// Print scaffold version banner and exit `0`.
    Version,
    /// Print CLI usage summary and exit `0`.
    Help,
}

/// Strongly-typed representation of a parsed compiler CLI invocation (`coblang`, `cobc`, `gcobol`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDriverInvocation {
    /// Driver personality used to interpret defaults.
    pub flavor: DriverFlavor,
    /// Optional `--version` or `--help` short-circuit query.
    pub query: Option<DriverQuery>,
    /// Requested compilation stage.
    pub mode: CompilationMode,
    /// Source reference format override.
    pub format: SourceFormat,
    /// Standard or vendor dialect profile.
    pub dialect: Dialect,
    /// Optimization level.
    pub optimization: OptimizationLevel,
    /// Runtime character encoding setting.
    pub encoding: TextEncoding,
    /// Positional source/object input files.
    pub inputs: Vec<String>,
    /// Copybook include directories (`-I`).
    pub include_dirs: Vec<String>,
    /// Extra copybook file extensions (`-copyext` / `-ext`).
    pub copy_exts: Vec<String>,
    /// Linker search directories (`-L`).
    pub library_dirs: Vec<String>,
    /// Linker libraries (`-l`).
    pub libraries: Vec<String>,
    /// Preserved warning and feature flags (`-W*`, `-f*`, `-debug`, etc.).
    pub extra_flags: Vec<String>,
    /// Whether runtime exception checking was explicitly requested (`-debug` / `-fcobol-exceptions`).
    pub exceptions_enabled: bool,
    /// Output artifact path (`-o`).
    pub output_path: Option<String>,
}

/// Strongly-typed representation of a parsed `cobcrun` dynamic module invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCobcrunInvocation {
    /// Optional `--version` or `--help` short-circuit query.
    pub query: Option<DriverQuery>,
    /// Module search directories supplied via `-M` or `COB_LIBRARY_PATH`.
    pub module_dirs: Vec<String>,
    /// Name of the COBOL program module to load and execute.
    pub module_name: String,
    /// Positional runtime arguments forwarded to the COBOL module.
    pub program_args: Vec<String>,
}

/// Returns the deterministic exit code emitted when compilation is not yet implemented.
///
/// # Returns
///
/// [`EXIT_UNIMPLEMENTED`] (`78`).
#[must_use]
pub const fn scaffold_unimplemented_exit_code() -> u8 {
    EXIT_UNIMPLEMENTED
}

/// Parses compiler driver arguments according to the given [`DriverFlavor`].
///
/// # Arguments
///
/// * `flavor` - CLI personality (`Coblang`, `Cobc`, or `Gcobol`).
/// * `args` - Command-line arguments excluding `argv[0]`.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] when required flag values or input files are missing,
/// or when an invalid dialect or optimization flag is passed.
pub fn parse_driver_args(flavor: DriverFlavor, args: &[String]) -> Result<ParsedDriverInvocation> {
    let default_mode = if flavor == DriverFlavor::Cobc {
        CompilationMode::CompileModule
    } else {
        CompilationMode::BuildExecutable
    };
    let mut inv = ParsedDriverInvocation {
        flavor,
        query: None,
        mode: default_mode,
        format: SourceFormat::Auto,
        dialect: Dialect::Default,
        optimization: OptimizationLevel::O0,
        encoding: TextEncoding::Ascii,
        inputs: Vec::new(),
        include_dirs: Vec::new(),
        copy_exts: Vec::new(),
        library_dirs: Vec::new(),
        libraries: Vec::new(),
        extra_flags: Vec::new(),
        exceptions_enabled: false,
        output_path: None,
    };

    let mut idx = 0;
    while let Some(arg) = args.get(idx).map(String::as_str) {
        if matches!(arg, "--version" | "-V") {
            inv.query = Some(DriverQuery::Version);
            return Ok(inv);
        }
        if matches!(arg, "--help" | "-h") {
            inv.query = Some(DriverQuery::Help);
            return Ok(inv);
        }
        idx = apply_driver_token(&mut inv, arg, args, idx)?;
        idx += 1;
    }

    if inv.inputs.is_empty() {
        return Err(CoblangError::InvalidCli {
            message: String::from("no input files supplied"),
        });
    }

    Ok(inv)
}

/// Applies a single non-query CLI token (consuming an extra token when two-part) at `idx`.
///
/// # Arguments
///
/// * `inv` - Mutable invocation accumulator.
/// * `arg` - Current CLI token at `idx`.
/// * `args` - Full CLI argument slice.
/// * `idx` - Current index in `args`.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] if a required value is missing or invalid.
fn apply_driver_token(
    inv: &mut ParsedDriverInvocation,
    arg: &str,
    args: &[String],
    mut idx: usize,
) -> Result<usize> {
    match arg {
        "-E" => inv.mode = CompilationMode::PreprocessOnly,
        "-fsyntax-only" => inv.mode = CompilationMode::SyntaxOnly,
        "-S" => inv.mode = CompilationMode::EmitAssembly,
        "-emit-llvm" => inv.mode = CompilationMode::EmitLlvmIr,
        "-c" => inv.mode = CompilationMode::CompileObject,
        "-m" | "-shared" | "-b" => inv.mode = CompilationMode::CompileModule,
        "-x" => inv.mode = CompilationMode::BuildExecutable,
        "-free" | "-ffree-form" | "-F" => inv.format = SourceFormat::Free,
        "-fixed" | "-ffixed-form" => inv.format = SourceFormat::Fixed,
        "-finternal-ebcdic" => inv.encoding = TextEncoding::Ebcdic,
        "-debug" | "-d" | "-fcobol-exceptions" | "-ftraceall" => {
            inv.exceptions_enabled = true;
            inv.extra_flags.push(String::from(arg));
        }
        "-o" => {
            idx += 1;
            inv.output_path = Some(next_value(args, idx, "-o")?);
        }
        "-I" => {
            idx += 1;
            inv.include_dirs.push(next_value(args, idx, "-I")?);
        }
        "-L" => {
            idx += 1;
            inv.library_dirs.push(next_value(args, idx, "-L")?);
        }
        "-l" => {
            idx += 1;
            inv.libraries.push(next_value(args, idx, "-l")?);
        }
        "-copyext" | "-ext" => {
            idx += 1;
            inv.copy_exts.push(next_value(args, idx, arg)?);
        }
        "-dialect" | "-std" => {
            idx += 1;
            let val = next_value(args, idx, arg)?;
            inv.dialect = Dialect::from_cli_name(&val)?;
        }
        "-t" | "-T" => {
            idx += 1;
            let listing = next_value(args, idx, arg)?;
            inv.extra_flags.push(format!("{arg}={listing}"));
        }
        _ => parse_attached_or_positional(inv, arg)?,
    }
    Ok(idx)
}

/// Parses attached prefix flags (`-std=...`, `-I...`, `-O*`, `-W*`, `-f*`) or positional inputs.
///
/// # Arguments
///
/// * `inv` - Mutable invocation accumulator.
/// * `arg` - Single CLI token to classify.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] for unrecognized `-` flags or invalid attached values.
fn parse_attached_or_positional(inv: &mut ParsedDriverInvocation, arg: &str) -> Result<()> {
    if let Some(val) = arg.strip_prefix("-std=") {
        inv.dialect = Dialect::from_cli_name(val)?;
    } else if let Some(val) = arg.strip_prefix("-dialect=") {
        inv.dialect = Dialect::from_cli_name(val)?;
    } else if let Some(val) = arg.strip_prefix("-copyext=") {
        inv.copy_exts.push(String::from(val));
    } else if let Some(cp) = arg.strip_prefix("-fexec-charset=") {
        if cp.is_empty() {
            return Err(CoblangError::InvalidCli {
                message: String::from("empty charset in -fexec-charset="),
            });
        }
        inv.encoding = TextEncoding::Codepage(String::from(cp));
    } else if let Some(dir) = arg.strip_prefix("-I") {
        inv.include_dirs.push(String::from(dir));
    } else if let Some(dir) = arg.strip_prefix("-L") {
        inv.library_dirs.push(String::from(dir));
    } else if let Some(lib) = arg.strip_prefix("-l") {
        inv.libraries.push(String::from(lib));
    } else if let Some(out) = arg.strip_prefix("-o") {
        inv.output_path = Some(String::from(out));
    } else if arg.starts_with("-O") {
        inv.optimization =
            OptimizationLevel::parse_flag(arg, 0).map_err(|_| CoblangError::InvalidCli {
                message: format!("unsupported optimization flag `{arg}`"),
            })?;
    } else if matches!(arg, "--emit-mlir" | "--emit-mlir-llvm") {
        inv.mode = CompilationMode::EmitLlvmIr;
        inv.extra_flags.push(String::from(arg));
    } else if arg.starts_with("-W")
        || arg.starts_with("-f")
        || arg.starts_with("-D")
        || arg.starts_with("-g")
        || matches!(
            arg,
            "-w" | "-v" | "-q" | "-save-temps" | "-static" | "-rdynamic" | "-main"
        )
        || arg.starts_with("-conf=")
        || arg.starts_with("-cb_conf=")
        || arg.starts_with("--tlines=")
    {
        if arg.starts_with("-fec=") {
            inv.exceptions_enabled = true;
        }
        inv.extra_flags.push(String::from(arg));
    } else if arg.starts_with('-') {
        return Err(CoblangError::InvalidCli {
            message: format!("unrecognized option `{arg}`"),
        });
    } else {
        inv.inputs.push(String::from(arg));
    }
    Ok(())
}

/// Extracts the mandatory value token following a two-token CLI option.
///
/// # Arguments
///
/// * `args` - Full argument slice.
/// * `idx` - Index of the value token.
/// * `flag` - Option name used for error messages.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] if `idx >= args.len()`.
fn next_value(args: &[String], idx: usize, flag: &str) -> Result<String> {
    args.get(idx)
        .cloned()
        .ok_or_else(|| CoblangError::InvalidCli {
            message: format!("missing argument after `{flag}`"),
        })
}

/// Parses `cobcrun` dynamic module runner CLI arguments and `COB_LIBRARY_PATH`.
///
/// # Arguments
///
/// * `args` - Command-line arguments excluding `argv[0]`.
/// * `cob_library_path` - Optional value of the `COB_LIBRARY_PATH` environment variable.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidCli`] when `-M` lacks a directory, an unknown flag is given,
/// or no module name is supplied.
pub fn parse_cobcrun_args(
    args: &[String],
    cob_library_path: Option<&str>,
) -> Result<ParsedCobcrunInvocation> {
    let mut module_dirs = Vec::new();
    if let Some(env_paths) = cob_library_path {
        for entry in env_paths.split(':').filter(|s| !s.is_empty()) {
            module_dirs.push(String::from(entry));
        }
    }

    let mut idx = 0;
    while let Some(arg) = args.get(idx).map(String::as_str) {
        match arg {
            "--version" | "-V" => {
                return Ok(ParsedCobcrunInvocation {
                    query: Some(DriverQuery::Version),
                    module_dirs,
                    module_name: String::new(),
                    program_args: Vec::new(),
                });
            }
            "--help" | "-h" => {
                return Ok(ParsedCobcrunInvocation {
                    query: Some(DriverQuery::Help),
                    module_dirs,
                    module_name: String::new(),
                    program_args: Vec::new(),
                });
            }
            "-M" => {
                idx += 1;
                module_dirs.push(next_value(args, idx, "-M")?);
            }
            _ => {
                if let Some(dir) = arg.strip_prefix("-M") {
                    module_dirs.push(String::from(dir));
                } else if arg.starts_with('-') {
                    return Err(CoblangError::InvalidCli {
                        message: format!("unrecognized cobcrun option `{arg}`"),
                    });
                } else {
                    let module_name = String::from(arg);
                    let program_args = args.get(idx + 1..).unwrap_or(&[]).to_vec();
                    return Ok(ParsedCobcrunInvocation {
                        query: None,
                        module_dirs,
                        module_name,
                        program_args,
                    });
                }
            }
        }
        idx += 1;
    }

    Err(CoblangError::InvalidCli {
        message: String::from("no program module specified for cobcrun"),
    })
}

/// Executes a compiler driver invocation (`coblang`, `cobc`, or `gcobol`) in zero-compiler mode.
///
/// # Arguments
///
/// * `flavor` - CLI personality (`Coblang`, `Cobc`, or `Gcobol`).
/// * `args` - Command-line arguments excluding `argv[0]`.
/// * `cwd` - Working directory used to check whether input source files exist.
/// * `stdout` - Output stream receiving `--version` or `--help` banners.
/// * `stderr` - Error stream receiving CLI diagnostics or structured `UNIMPLEMENTED` messages.
///
/// # Returns
///
/// Process status code (`0` for queries, [`EXIT_CLI_USAGE`] for invalid flags,
/// [`EXIT_INPUT_ERROR`] for missing files, or [`EXIT_UNIMPLEMENTED`] when valid).
pub fn execute_driver<W1: Write, W2: Write>(
    flavor: DriverFlavor,
    args: &[String],
    cwd: &Path,
    stdout: &mut W1,
    stderr: &mut W2,
) -> u8 {
    let label = flavor_label(flavor);
    let parsed = match parse_driver_args(flavor, args) {
        Ok(inv) => inv,
        Err(err) => {
            let _ = write!(stderr, "{label}: error: {err}{LF}");
            return EXIT_CLI_USAGE;
        }
    };

    if let Some(query) = parsed.query {
        match query {
            DriverQuery::Version => {
                let _ = write!(
                    stdout,
                    "{label} (coblang scaffold driver) 0.1.0 [zero-compiler mode]{LF}"
                );
            }
            DriverQuery::Help => {
                let _ = write!(
                    stdout,
                    "Usage: {label} [-E|-fsyntax-only|-S|-emit-llvm|-c|-m|-x] [-std=<dialect>] [-free|-fixed] [-O0..-Os] [-I <dir>] [-o <out>] <inputs...>{LF}"
                );
            }
        }
        return 0;
    }

    for input in &parsed.inputs {
        let candidate = cwd.join(input);
        if !candidate.exists() {
            let _ = write!(stderr, "{label}: error: input file not found: {input}{LF}");
            return EXIT_INPUT_ERROR;
        }
    }

    let _ = write!(
        stderr,
        "coblang-scaffold [{label}]: UNIMPLEMENTED: mode={} dialect={} format={} opt={} encoding={} inputs={}{LF}",
        parsed.mode.as_str(),
        parsed.dialect.as_str(),
        parsed.format.as_str(),
        parsed.optimization.as_flag(),
        parsed.encoding.to_token(),
        parsed.inputs.join(","),
    );
    EXIT_UNIMPLEMENTED
}

/// Executes a `cobcrun` dynamic module invocation in zero-compiler scaffold mode.
///
/// # Arguments
///
/// * `args` - Command-line arguments excluding `argv[0]`.
/// * `cob_library_path` - Optional `COB_LIBRARY_PATH` environment variable value.
/// * `stdout` - Output stream receiving `--version` or `--help` banners.
/// * `stderr` - Error stream receiving CLI diagnostics or structured `UNIMPLEMENTED` messages.
///
/// # Returns
///
/// Process status code (`0`, [`EXIT_CLI_USAGE`], or [`EXIT_UNIMPLEMENTED`]).
pub fn execute_cobcrun<W1: Write, W2: Write>(
    args: &[String],
    cob_library_path: Option<&str>,
    stdout: &mut W1,
    stderr: &mut W2,
) -> u8 {
    let parsed = match parse_cobcrun_args(args, cob_library_path) {
        Ok(inv) => inv,
        Err(err) => {
            let _ = write!(stderr, "cobcrun: error: {err}{LF}");
            return EXIT_CLI_USAGE;
        }
    };

    if let Some(query) = parsed.query {
        match query {
            DriverQuery::Version => {
                let _ = write!(
                    stdout,
                    "cobcrun (coblang scaffold runtime) 0.1.0 [zero-compiler mode]{LF}"
                );
            }
            DriverQuery::Help => {
                let _ = write!(stdout, "Usage: cobcrun [-M <dir>] <PROGRAM> [args...]{LF}");
            }
        }
        return 0;
    }

    let _ = write!(
        stderr,
        "coblang-scaffold [cobcrun]: UNIMPLEMENTED: module={} dirs={} argc={}{LF}",
        parsed.module_name,
        parsed.module_dirs.len(),
        parsed.program_args.len()
    );
    EXIT_UNIMPLEMENTED
}

/// Returns the short binary display name for a [`DriverFlavor`].
///
/// # Arguments
///
/// * `flavor` - Driver personality enum value.
#[must_use]
pub const fn flavor_label(flavor: DriverFlavor) -> &'static str {
    match flavor {
        DriverFlavor::Coblang => "coblang",
        DriverFlavor::Cobc => "cobc",
        DriverFlavor::Cobcrun => "cobcrun",
        DriverFlavor::Gcobol => "gcobol",
    }
}

/// Process entry helper shared by the four binary shims (`coblang`, `cobc`, `cobcrun`, `gcobol`).
///
/// # Arguments
///
/// * `flavor` - CLI personality to execute using `std::env::args` and `std::env::current_dir`.
///
/// # Returns
///
/// Process [`ExitCode`] corresponding to the driver's numeric return code.
pub fn main_entry(flavor: DriverFlavor) -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env_vars: Vec<(String, String)> = std::env::vars().collect();
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let code = if flavor == DriverFlavor::Cobcrun {
        execute_cobcrun_driver(&args, &env_vars, Path::new("."), &mut stdout, &mut stderr)
    } else {
        execute_compiler_driver(
            flavor,
            &args,
            &env_vars,
            Path::new("."),
            &mut stdout,
            &mut stderr,
        )
    };
    ExitCode::from(code)
}
