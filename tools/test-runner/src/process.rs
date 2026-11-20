//! Safe subprocess execution with process-group isolation, timeouts, and C helper compilation.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use coblang_common::{CoblangError, CompileCSpec, Result};
use coblang_scaffold_driver::open_raw_pty_pair;

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
    /// When `true`, attaches child `stdin`/`stdout` to a POSIX PTY (`Capability::CursesPty`).
    pub use_pty: bool,
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
    if spec.use_pty {
        return run_pty_command_with_timeout(spec, test_id);
    }
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

/// Default hard ceiling on captured `stdout` or `stderr` bytes per subprocess (`4 MiB`).
pub const DEFAULT_MAX_PIPE_BYTES: usize = 4 * 1024 * 1024;

/// Reads an optional pipe stream up to [`DEFAULT_MAX_PIPE_BYTES`], draining any excess bytes.
///
/// # Arguments
///
/// * `pipe` - Optional child stdout or stderr pipe handle.
#[must_use]
pub fn read_optional_pipe<R: Read>(pipe: Option<R>) -> Vec<u8> {
    read_optional_pipe_bounded(pipe, DEFAULT_MAX_PIPE_BYTES).0
}

/// Reads an optional pipe stream up to `max_bytes`, draining and discarding overflow so the child
/// process never blocks on a full pipe while runner memory stays strictly bounded.
///
/// # Arguments
///
/// * `pipe` - Optional child stdout or stderr pipe handle.
/// * `max_bytes` - Maximum byte count retained in the returned `Vec<u8>`.
#[must_use]
pub fn read_optional_pipe_bounded<R: Read>(pipe: Option<R>, max_bytes: usize) -> (Vec<u8>, bool) {
    let mut buf = Vec::new();
    let Some(mut reader) = pipe else {
        return (buf, false);
    };
    let cap_plus_one = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let _ = reader.by_ref().take(cap_plus_one).read_to_end(&mut buf);
    if buf.len() <= max_bytes {
        return (buf, false);
    }
    buf.truncate(max_bytes);
    let mut sink = std::io::sink();
    let _ = std::io::copy(&mut reader, &mut sink);
    (buf, true)
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
        #[cfg(target_os = "macos")]
        args.push(String::from("-Wl,-undefined,dynamic_lookup"));
    } else {
        args.push(String::from("-c"));
    }
    args.push(String::from("-I"));
    args.push(String::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/coblang-rt-abi/include"
    )));
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
            use_pty: false,
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

/// Active row prompt substrings mapped to VT100 escape sequences on `Capability::CursesPty`.
const ROW_PROMPT_RULES: &[(&str, &[u8])] = &[
    ("Function key 01", b"\x1bOP"),
    ("Function key 02", b"\x1bOQ"),
    ("Function key 03", b"\x1bOR"),
    ("Function key 04", b"\x1bOS"),
    ("Function key 05", b"\x1b[15~"),
    ("Prev-page key", b"\x1b[5~"),
    ("Next-page key", b"\x1b[6~"),
];

/// Full-screen prompt substrings mapped to VT100 operator replies on `Capability::CursesPty`.
const SCREEN_PROMPT_RULES: &[(&str, &[u8])] = &[
    ("TIMEOUT TEST", b"ABCDEFGHY"),
    ("enter 'AB'", b"AB\r"),
    ("'E' (fifth column)", b"\x1b[C\x1b[C\r"),
    (
        "positioned at the last",
        b"\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\r",
    ),
    ("'C' (third column)", b"\x1b[D\x1b[D\r"),
    ("Type 'Y'", b"Y"),
    ("ANSWER Y OR N", b"Y\r"),
];

/// Minimal VT100 parser state for headless `Capability::CursesPty` screen inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VtParseState {
    /// Normal ground text mode.
    Ground,
    /// Saw `ESC` (`0x1B`).
    Esc,
    /// Inside `ESC [` CSI parameter sequence.
    Csi {
        /// Up to two numeric CSI parameters (`row`, `col`).
        params: [u16; 2],
        /// Active parameter index (`0` or `1`).
        idx: usize,
    },
}

/// Headless `50x80` VT100 screen tracker driving automated `Capability::CursesPty` operator replies.
#[derive(Debug, Clone)]
pub struct Vt100Screen {
    /// `50` rows of `80` ASCII cells.
    grid: [[u8; 80]; 50],
    /// Current 0-based cursor row (`0..50`).
    row: usize,
    /// Current 0-based cursor column (`0..80`).
    col: usize,
    /// Whether terminal bell (`0x07`) was emitted since the last operator reply.
    saw_bell: bool,
    /// Active escape-sequence parser state.
    state: VtParseState,
}

impl Default for Vt100Screen {
    /// Returns a blank `50x80` [`Vt100Screen`] positioned at `(0, 0)`.
    fn default() -> Self {
        Self::new()
    }
}

impl Vt100Screen {
    /// Creates a blank `50x80` [`Vt100Screen`] positioned at `(0, 0)`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            grid: [[b' '; 80]; 50],
            row: 0,
            col: 0,
            saw_bell: false,
            state: VtParseState::Ground,
        }
    }

    /// Feeds raw ANSI/VT100 terminal output bytes emitted by the PTY child into the virtual grid.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Raw PTY master output slice.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.step_byte(b);
        }
    }

    /// Advances the VT100 state machine by one byte `b`.
    ///
    /// # Arguments
    ///
    /// * `b` - Single output byte from the PTY stream.
    fn step_byte(&mut self, b: u8) {
        match self.state {
            VtParseState::Ground => match b {
                0x1b => self.state = VtParseState::Esc,
                0x07 => self.saw_bell = true,
                b'\r' => self.col = 0,
                b'\n' => self.row = (self.row + 1).min(49),
                0x20..=0x7e => {
                    if let Some(cell) = self
                        .grid
                        .get_mut(self.row)
                        .and_then(|r| r.get_mut(self.col))
                    {
                        *cell = b;
                    }
                    self.col = (self.col + 1).min(79);
                }
                _ => {}
            },
            VtParseState::Esc => {
                self.state = if b == b'[' {
                    VtParseState::Csi {
                        params: [0, 0],
                        idx: 0,
                    }
                } else {
                    VtParseState::Ground
                };
            }
            VtParseState::Csi {
                mut params,
                mut idx,
            } => match b {
                b'0'..=b'9' => {
                    if let Some(slot) = params.get_mut(idx) {
                        *slot = slot.saturating_mul(10).saturating_add(u16::from(b - b'0'));
                    }
                    self.state = VtParseState::Csi { params, idx };
                }
                b';' => {
                    idx = (idx + 1).min(1);
                    self.state = VtParseState::Csi { params, idx };
                }
                b'H' => {
                    self.row = usize::from(params[0].max(1) - 1).min(49);
                    self.col = usize::from(params[1].max(1) - 1).min(79);
                    self.state = VtParseState::Ground;
                }
                b'J' => {
                    self.grid = [[b' '; 80]; 50];
                    self.state = VtParseState::Ground;
                }
                0x40..=0x7e => self.state = VtParseState::Ground,
                _ => {}
            },
        }
    }

    /// Selects the next automated keystroke sequence when the PTY child is waiting for input, or
    /// returns `None` once when an `ACCEPT ... WITH TIMEOUT` prompt must first expire naturally.
    ///
    /// # Arguments
    ///
    /// * `did_timeout_pause` - Tracks whether the one-shot timeout pause has already been observed.
    #[must_use]
    pub fn next_operator_input(&mut self, did_timeout_pause: &mut bool) -> Option<&'static [u8]> {
        let row_text = self
            .grid
            .get(self.row)
            .map(|r| String::from_utf8_lossy(r).into_owned())
            .unwrap_or_default();
        let mut full_text = String::with_capacity(50 * 81);
        for r in &self.grid {
            full_text.push_str(&String::from_utf8_lossy(r));
            full_text.push('\n');
        }
        if full_text.contains("TIMEOUT TEST") && !*did_timeout_pause {
            *did_timeout_pause = true;
            return None;
        }
        for &(needle, reply) in ROW_PROMPT_RULES {
            if row_text.contains(needle) {
                return Some(reply);
            }
        }
        for &(needle, reply) in SCREEN_PROMPT_RULES {
            if full_text.contains(needle) {
                return Some(reply);
            }
        }
        if self.saw_bell {
            self.saw_bell = false;
            return Some(b"YYYYYYYYYY\r");
        }
        Some(b"\r")
    }
}

/// Drains all currently available non-blocking bytes from `master` into `vt`, returning `true` if
/// at least one byte was read.
///
/// # Arguments
///
/// * `master` - Non-blocking master PTY [`File`] (or regular file in unit tests).
/// * `vt` - Mutable [`Vt100Screen`] state.
pub fn drain_nonblocking_pty(master: &mut File, vt: &mut Vt100Screen) -> bool {
    let mut buf = [0_u8; 4096];
    let mut read_any = false;
    while let Ok(n) = master.read(&mut buf) {
        let Some(slice) = buf.get(..n).filter(|s| !s.is_empty()) else {
            break;
        };
        vt.feed(slice);
        read_any = true;
    }
    read_any
}

/// Spawns `spec` attached to a POSIX PTY (`25x80`) and drives interactive VT100 prompts until exit
/// or wall-clock timeout.
///
/// # Arguments
///
/// * `spec` - Subprocess launch and timeout parameters.
/// * `test_id` - Test identifier attached to [`CoblangError::RunnerExecution`] on failure.
///
/// # Errors
///
/// Returns [`CoblangError::RunnerExecution`] if PTY allocation or child spawning fails.
fn run_pty_command_with_timeout(spec: &CommandSpec<'_>, test_id: &str) -> Result<ProcessOutput> {
    let (mut master, slave_in) = open_raw_pty_pair(25, 80, test_id)?;
    let slave_out = slave_in
        .try_clone()
        .map_err(|err| CoblangError::RunnerExecution {
            test_id: String::from(test_id),
            message: format!("failed to dup slave PTY: {err}"),
        })?;

    let mut cmd = Command::new(spec.program);
    cmd.args(spec.args)
        .current_dir(spec.cwd)
        .stdin(Stdio::from(slave_in))
        .stdout(Stdio::from(slave_out))
        .stderr(Stdio::piped())
        .env("TERM", "xterm-256color")
        .env("COBLANG_FAST_SLEEP", "1")
        .process_group(0);
    for (k, v) in spec.env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().map_err(|err| CoblangError::RunnerExecution {
        test_id: String::from(test_id),
        message: format!(
            "failed to spawn PTY child `{}`: {err}",
            spec.program.display()
        ),
    })?;

    let pgid = child.id();
    if !spec.stdin_bytes.is_empty() {
        let _ = master.write_all(spec.stdin_bytes);
    }

    let child_stderr = child.stderr.take();
    let mut stderr = Vec::new();
    let waited = std::thread::scope(|scope| -> Result<_> {
        let err_ref = &mut stderr;
        scope.spawn(move || {
            *err_ref = read_optional_pipe(child_stderr);
        });
        poll_pty_child_until_done(&mut child, &mut master, pgid, spec.timeout_ms)
    });
    let (status, timed_out) = waited?;

    Ok(ProcessOutput {
        exit_code: status.code(),
        signal: status.signal(),
        timed_out,
        stdout: Vec::new(),
        stderr,
    })
}

/// Polls `child` and `master` until exit or `timeout_ms`, driving `Vt100Screen` operator replies.
///
/// # Arguments
///
/// * `child` - Spawned child process handle.
/// * `master` - Non-blocking master PTY file.
/// * `pgid` - Child process-group ID for timeout cleanup.
/// * `timeout_ms` - Maximum wall-clock runtime in milliseconds.
fn poll_pty_child_until_done(
    child: &mut std::process::Child,
    master: &mut File,
    pgid: u32,
    timeout_ms: u64,
) -> Result<(std::process::ExitStatus, bool)> {
    let mut vt = Vt100Screen::new();
    let mut did_timeout_pause = false;
    let mut idle_wait = Duration::from_millis(1500);
    let mut last_activity = Instant::now();
    let deadline = last_activity + Duration::from_millis(timeout_ms);

    loop {
        if drain_nonblocking_pty(master, &mut vt) {
            idle_wait = Duration::from_millis(25);
            last_activity = Instant::now();
        }
        if let Some(done) = child.try_wait()? {
            let _ = drain_nonblocking_pty(master, &mut vt);
            return Ok((done, false));
        }
        let now = Instant::now();
        if now >= deadline {
            kill_process_group(pgid);
            let _ = child.kill();
            let done = child.wait()?;
            return Ok((done, true));
        }
        if now.duration_since(last_activity) >= idle_wait {
            if let Some(reply) = vt.next_operator_input(&mut did_timeout_pause) {
                let _ = master.write_all(reply);
                idle_wait = Duration::from_millis(25);
            } else {
                idle_wait = Duration::from_millis(1100);
            }
            last_activity = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
