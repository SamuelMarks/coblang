//! Safe subprocess execution with process-group isolation, timeouts, and C helper compilation.

use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use coblang_common::{CoblangError, CompileCSpec, Result};

/// Captured result of running a child subprocess inside an isolated process group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    /// Numeric exit code when the process exited normally.
    pub exit_code: Option<i32>,
    /// Unix signal number when terminated by signal (e.g., `SIGSEGV`, `SIGABRT`, `SIGKILL`).
    pub signal: Option<i32>,
    /// Whether the process exceeded its wall-clock timeout and its `PGID` was killed.
    pub timed_out: bool,
    /// Raw bytes written to `stdout`.
    pub stdout: Vec<u8>,
    /// Raw bytes written to `stderr`.
    pub stderr: Vec<u8>,
}

/// Specification for spawning a bounded subprocess via [`run_command_with_timeout`].
#[derive(Debug, Clone)]
pub struct CommandSpec<'a> {
    /// Executable path or program name in `PATH`.
    pub program: &'a Path,
    /// Command-line arguments passed to `program`.
    pub args: &'a [String],
    /// Working directory for the child process.
    pub cwd: &'a Path,
    /// Environment variable overrides exported to the child process.
    pub env: &'a [(String, String)],
    /// Bytes written to the child's standard input before closing `stdin`.
    pub stdin_bytes: &'a [u8],
    /// Maximum wall-clock runtime in milliseconds before process-group kill.
    pub timeout_ms: u64,
}

/// Executes a child command in its own Unix process group with bounded wall-clock timeout.
///
/// # Arguments
///
/// * `spec` - Subprocess launch and timeout parameters.
/// * `test_id` - Test identifier attached to [`CoblangError::RunnerExecution`] on spawn failure.
///
/// # Errors
///
/// Returns [`CoblangError::RunnerExecution`] if the child process cannot be spawned.
pub fn run_command_with_timeout(spec: &CommandSpec<'_>, test_id: &str) -> Result<ProcessOutput> {
    let stdin_cfg = if spec.stdin_bytes.is_empty() {
        Stdio::null()
    } else {
        Stdio::piped()
    };
    let mut cmd = Command::new(spec.program);
    cmd.args(spec.args)
        .current_dir(spec.cwd)
        .stdin(stdin_cfg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for (k, v) in spec.env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().map_err(|err| CoblangError::RunnerExecution {
        test_id: String::from(test_id),
        message: format!("failed to spawn `{}`: {err}", spec.program.display()),
    })?;

    let pgid = child.id();
    if let Some(mut stdin_pipe) = child.stdin.take() {
        let _ = stdin_pipe.write_all(spec.stdin_bytes);
    }

    let child_stdout = child.stdout.take();
    let child_stderr = child.stderr.take();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let waited = std::thread::scope(|scope| -> Result<_> {
        let out_ref = &mut stdout;
        let err_ref = &mut stderr;
        scope.spawn(move || {
            *out_ref = read_optional_pipe(child_stdout);
        });
        scope.spawn(move || {
            *err_ref = read_optional_pipe(child_stderr);
        });

        let deadline = Instant::now() + Duration::from_millis(spec.timeout_ms);
        let res = loop {
            if let Some(done) = child.try_wait()? {
                break (done, false);
            }
            if Instant::now() >= deadline {
                kill_process_group(pgid);
                let _ = child.kill();
                let done = child.wait()?;
                break (done, true);
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        Ok(res)
    });
    let (status, timed_out) = waited?;

    Ok(ProcessOutput {
        exit_code: status.code(),
        signal: status.signal(),
        timed_out,
        stdout,
        stderr,
    })
}

/// Reads an optional pipe stream until EOF into a byte vector.
///
/// # Arguments
///
/// * `pipe` - Optional child stdout or stderr pipe handle.
#[must_use]
pub fn read_optional_pipe<R: Read>(pipe: Option<R>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut reader) = pipe {
        let _ = reader.read_to_end(&mut buf);
    }
    buf
}

/// Sends `SIGKILL` to the entire Unix process group `-pgid` using `/bin/kill`.
///
/// # Arguments
///
/// * `pgid` - Process group leader PID created via `process_group(0)`.
fn kill_process_group(pgid: u32) {
    let target = format!("-{pgid}");
    let _ = Command::new("/bin/kill")
        .args(["-KILL", &target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Compiles companion C source files inside `sandbox` using the configured C compiler (`clang`/`cc`).
///
/// # Arguments
///
/// * `spec` - C compilation step specification.
/// * `sandbox` - Sandbox directory containing the `.c` sources.
/// * `c_compiler` - C compiler executable path or name (e.g., `"clang"`).
/// * `timeout_ms` - Timeout in milliseconds.
/// * `test_id` - Test identifier used for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::RunnerExecution`] if the C compiler process cannot be spawned.
pub fn compile_c_companion(
    spec: &CompileCSpec,
    sandbox: &Path,
    c_compiler: &Path,
    timeout_ms: u64,
    test_id: &str,
) -> Result<Option<String>> {
    let mut args = Vec::new();
    if spec.shared {
        args.push(String::from("-shared"));
        args.push(String::from("-fPIC"));
    } else {
        args.push(String::from("-c"));
    }
    for src in &spec.sources {
        args.push(src.clone());
    }
    args.push(String::from("-o"));
    args.push(spec.output.clone());

    let out = run_command_with_timeout(
        &CommandSpec {
            program: c_compiler,
            args: &args,
            cwd: sandbox,
            env: &[],
            stdin_bytes: &[],
            timeout_ms,
        },
        test_id,
    )?;

    if out.timed_out {
        return Ok(Some(String::from("companion C compilation timed out")));
    }
    if out.exit_code != Some(0) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Ok(Some(format!(
            "companion C compilation failed (exit {:?}): {}",
            out.exit_code,
            stderr.trim()
        )));
    }
    Ok(None)
}
