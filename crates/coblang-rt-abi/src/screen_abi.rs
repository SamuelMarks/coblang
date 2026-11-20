//! Audited C-ABI entrypoints (`libcoblang_rt`) for Milestone M19 `SCREEN SECTION` TUI,
//! extended `DISPLAY`/`ACCEPT`, `SET ... ATTRIBUTE`, `CBL_GC_WINDOW`, `CBL_GC_SET_SCR_SIZE`, and `CBL_GET_CSR_POS`.

#![deny(clippy::indexing_slicing)]

use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;
use std::sync::{Mutex, OnceLock};

use coblang_common::Result;
use coblang_hir::DataCategory;
use coblang_rt_core::{
    desc_codec::decode_field_spec,
    field_ops::{RuntimeMoveSourceKind, runtime_execute_move},
};
use coblang_screen::{
    AtCompactSpec, CellStyle, ColSpec, CrtStatusCode, CursorFormat, CursorPosition,
    CursorVisibility, KeyEvent, LineSpec, ScreenAttrFlags, ScreenCol, ScreenColor,
    ScreenDimensions, ScreenRow, TerminalBackend,
};

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "Audited POSIX `isatty` and `poll` FFI declarations for raw TTY key polling."
)]
unsafe extern "C" {
    /// Returns non-zero if `fd` refers to an open terminal device.
    ///
    /// # Arguments
    ///
    /// * `fd` - File descriptor to query.
    fn isatty(fd: core::ffi::c_int) -> core::ffi::c_int;
    /// Waits for events on a set of file descriptors.
    ///
    /// # Arguments
    ///
    /// * `fds` - Pointer to `PollFd` array.
    /// * `nfds` - Number of `PollFd` entries.
    /// * `timeout` - Timeout in milliseconds (`-1` blocks indefinitely).
    fn poll(
        fds: *mut PollFd,
        nfds: core::ffi::c_uint,
        timeout: core::ffi::c_int,
    ) -> core::ffi::c_int;
    /// Reads up to `count` bytes from `fd` into `buf`.
    ///
    /// # Arguments
    ///
    /// * `fd` - Open file descriptor.
    /// * `buf` - Destination buffer pointer.
    /// * `count` - Maximum byte count to read.
    fn read(fd: core::ffi::c_int, buf: *mut core::ffi::c_void, count: usize) -> isize;
}

/// POSIX `struct pollfd` layout (`fd`, `events`, `revents`).
#[repr(C)]
struct PollFd {
    /// File descriptor (`0` for `STDIN_FILENO`).
    fd: core::ffi::c_int,
    /// Requested event mask (`POLLIN = 0x0001`).
    events: core::ffi::c_short,
    /// Returned event mask.
    revents: core::ffi::c_short,
}

/// POSIX `POLLIN` bit (`0x0001`).
const POLLIN_MASK: core::ffi::c_short = 0x0001;

/// Dynamic coordinate mode encoded across the C-ABI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RtCoord {
    /// Omitted coordinate.
    Omitted,
    /// `LINE 0` / `COLUMN 0`.
    Zero,
    /// 1-based absolute row or column.
    Absolute(u16),
    /// `+ n` relative offset.
    Plus(u16),
    /// `- n` relative offset.
    Minus(u16),
}

impl RtCoord {
    /// Decodes `(kind, val)` (`0=Omitted, 1=Absolute/Zero, 2=Plus, 3=Minus`).
    ///
    /// # Arguments
    ///
    /// * `kind` - Coordinate mode discriminant (`0..=3`).
    /// * `val` - Coordinate value or relative delta.
    fn decode(kind: u32, val: i64) -> Self {
        match kind {
            1 => {
                if val <= 0 {
                    Self::Zero
                } else {
                    Self::Absolute(u16::try_from(val).unwrap_or(999).min(999))
                }
            }
            2 => Self::Plus(u16::try_from(val.max(0)).unwrap_or(0)),
            3 => Self::Minus(u16::try_from(val.max(0)).unwrap_or(0)),
            _ => Self::Omitted,
        }
    }

    /// Converts to [`LineSpec`].
    fn to_line_spec(self) -> LineSpec {
        match self {
            Self::Omitted => LineSpec::Omitted,
            Self::Zero => LineSpec::Zero,
            Self::Absolute(r) => ScreenRow::try_new(r).map_or(LineSpec::Zero, LineSpec::Absolute),
            Self::Plus(d) => LineSpec::RelativePlus(d),
            Self::Minus(d) => LineSpec::RelativeMinus(d),
        }
    }

    /// Converts to [`ColSpec`].
    fn to_col_spec(self) -> ColSpec {
        match self {
            Self::Omitted => ColSpec::Omitted,
            Self::Zero => ColSpec::Zero,
            Self::Absolute(c) => ScreenCol::try_new(c).map_or(ColSpec::Zero, ColSpec::Absolute),
            Self::Plus(d) => ColSpec::RelativePlus(d),
            Self::Minus(d) => ColSpec::RelativeMinus(d),
        }
    }
}

/// Live data binding for a runtime screen item assembled before `DISPLAY` or `ACCEPT`.
#[derive(Debug, Clone)]
enum RtItemBinding {
    /// Pure directive (`BLANK SCREEN`, `BELL`, etc.) without text cells.
    ActionOnly,
    /// Read-only display bytes (`VALUE`, `FROM`, or `DISPLAY expr`).
    DisplayOnly {
        /// Formatted display bytes.
        bytes: Vec<u8>,
        /// Whether `VALUE ALL` tiling applies.
        repeat_all: bool,
        /// Whether raw `LOW-VALUES` no-advance cursor positioning applies (`0008`/`0009`).
        low_values_noop: bool,
    },
    /// Editable field (`TO` or `USING` or `FROM ... TO`).
    Editable {
        /// Initial display bytes (from `USING`, `FROM`, or spaces).
        init_bytes: Vec<u8>,
        /// Whether edit buffer pre-loads `init_bytes` (`USING` without `NO UPDATE`, or `UPDATE`).
        preload_edit: bool,
        /// Destination COBOL storage address.
        dst_addr: usize,
        /// Destination COBOL byte length.
        dst_len: usize,
        /// Serialized `RuntimeFieldSpec` bytes for committing edited text into `dst_addr`.
        dst_spec: Vec<u8>,
    },
}

/// Flattened runtime screen item staged for `DISPLAY` or `ACCEPT`.
#[derive(Debug, Clone)]
struct StagedScreenItem {
    /// Optional owning `screen_id` (honors `SET screen_id ATTRIBUTE ...`).
    screen_id: Option<u32>,
    /// Optional compact `AT` `(val, digits)` override.
    at_compact: Option<(u32, u8)>,
    /// Vertical coordinate spec.
    line: LineSpec,
    /// Horizontal coordinate spec.
    col: ColSpec,
    /// Whether raw `LINE` or `COLUMN` exceeded active screen dimensions prior to `u8` clamping.
    raw_coord_overflow: bool,
    /// Attribute bitmask (`ScreenAttrFlags`).
    attr_bits: u32,
    /// Foreground color (`< 0` = default, `0..=15` = CGA/ANSI color).
    fg_color: i32,
    /// Background color (`< 0` = default, `0..=15` = CGA/ANSI color).
    bg_color: i32,
    /// Visible cell width (`0` for `ActionOnly`).
    width: u16,
    /// Optional `PROMPT` fill byte (`None` when disabled).
    prompt_byte: Option<u8>,
    /// Scroll signed line count (`> 0` up, `< 0` down).
    scroll_lines: i32,
    /// Data binding.
    binding: RtItemBinding,
}

/// Active `CBL_GC_WINDOW` window descriptor for `0069_system_function_cbl_gc_window`.
#[derive(Debug, Clone)]
struct ManagedGcWindow {
    /// 1-based assigned window number (`1..=20`).
    number: i16,
    /// 1-based top row.
    start_line: i16,
    /// 1-based left column.
    start_col: i16,
    /// Height in rows.
    lines: i16,
    /// Width in columns.
    cols: i16,
    /// Foreground color.
    fg_color: i16,
    /// Background color.
    bg_color: i16,
    /// Visibility flags (`0` = visible, `1` = hidden).
    flags: i16,
}

/// Thread-local TUI runtime state across `DISPLAY`, `ACCEPT`, `SET ATTRIBUTE`, and `CBL_GC_WINDOW`.
struct ScreenRuntimeState {
    /// Active terminal dimensions (`24x80` default).
    dims: ScreenDimensions,
    /// Persistent virtual cursor position across consecutive `DISPLAY` / `ACCEPT` calls.
    cursor: CursorPosition,
    /// Per-`screen_id` `(on_bits, off_bits)` mutations from `SET ... ATTRIBUTE`.
    attr_overrides: BTreeMap<u32, (u32, u32)>,
    /// Staged items for the in-progress `DISPLAY` or `ACCEPT` statement.
    staged: Vec<StagedScreenItem>,
    /// Buffered lookahead bytes on `STDIN_FILENO`.
    stdin_lookahead: VecDeque<u8>,
    /// Deterministic injected `KeyEvent` queue for headless unit tests.
    injected_keys: VecDeque<KeyEvent>,
    /// Captured output buffer for headless unit tests when `capture_headless` is enabled.
    captured_ansi: Vec<u8>,
    /// Managed `CBL_GC_WINDOW` z-ordered stack (index `0` = bottom, `last` = top).
    windows: Vec<ManagedGcWindow>,
    /// Next monotonic window number (`1..`).
    next_win_num: i16,
}

impl ScreenRuntimeState {
    /// Constructs a fresh `24x80` [`ScreenRuntimeState`].
    fn new() -> Self {
        Self {
            dims: ScreenDimensions::STANDARD_24X80,
            cursor: CursorPosition::HOME,
            attr_overrides: BTreeMap::new(),
            staged: Vec::new(),
            stdin_lookahead: VecDeque::new(),
            injected_keys: VecDeque::new(),
            captured_ansi: Vec::new(),
            windows: Vec::new(),
            next_win_num: 1,
        }
    }
}

/// Process-wide screen engine state.
static SCREEN_STATE: OnceLock<Mutex<ScreenRuntimeState>> = OnceLock::new();

/// Executes `f` with shared access to [`SCREEN_STATE`].
///
/// # Arguments
///
/// * `f` - Closure inspecting [`ScreenRuntimeState`].
fn with_screen_state<R>(f: impl FnOnce(&ScreenRuntimeState) -> R) -> R {
    let lock = SCREEN_STATE.get_or_init(|| Mutex::new(ScreenRuntimeState::new()));
    let guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&guard)
}

/// Executes `f` with exclusive access to [`SCREEN_STATE`].
///
/// # Arguments
///
/// * `f` - Closure mutating [`ScreenRuntimeState`].
fn with_screen_state_mut<R>(f: impl FnOnce(&mut ScreenRuntimeState) -> R) -> R {
    let lock = SCREEN_STATE.get_or_init(|| Mutex::new(ScreenRuntimeState::new()));
    let mut guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut guard)
}

/// Resets screen state (called on teardown / unit tests).
pub fn reset_screen_runtime_state() {
    with_screen_state_mut(|st| *st = ScreenRuntimeState::new());
}

/// Queues synthetic [`KeyEvent`]s for deterministic non-PTY unit tests.
///
/// # Arguments
///
/// * `keys` - Ordered keystrokes to feed to subsequent `ACCEPT` calls.
pub fn push_test_screen_keys(keys: impl IntoIterator<Item = KeyEvent>) {
    with_screen_state_mut(|st| st.injected_keys.extend(keys));
}

/// Live ANSI VT100 terminal backend writing directly to stdout (plus test capture buffer).
struct LiveAnsiBackend<'a> {
    /// Screen dimensions.
    dims: ScreenDimensions,
    /// Current cursor position.
    cursor: CursorPosition,
    /// Current cursor visibility.
    visibility: CursorVisibility,
    /// Reference to test capture buffer.
    capture: &'a mut Vec<u8>,
    /// Whether stdout is connected to a real TTY (suppresses stdout noise when non-TTY in unit tests).
    write_stdout: bool,
}

impl LiveAnsiBackend<'_> {
    /// Writes raw ANSI bytes to `capture` and (when on a TTY) `std::io::stdout()`.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Raw VT100 escape/character bytes to emit.
    fn emit_bytes(&mut self, bytes: &[u8]) {
        self.capture.extend_from_slice(bytes);
        if self.write_stdout {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(bytes);
        }
    }
}

impl TerminalBackend for LiveAnsiBackend<'_> {
    /// Returns the configured [`ScreenDimensions`].
    fn dimensions(&self) -> ScreenDimensions {
        self.dims
    }

    /// Returns the active [`CursorPosition`].
    fn cursor(&self) -> CursorPosition {
        self.cursor
    }

    /// Returns the active [`CursorVisibility`].
    fn cursor_visibility(&self) -> CursorVisibility {
        self.visibility
    }

    /// Moves the VT100 cursor to `pos` (`CSI row;col H`).
    ///
    /// # Arguments
    ///
    /// * `pos` - Target 1-based [`CursorPosition`].
    fn set_cursor(&mut self, pos: CursorPosition) -> Result<()> {
        let clamped = pos.clamp_to(self.dims);
        let seq = format!("\x1b[{};{}H", clamped.row.get(), clamped.col.get());
        self.emit_bytes(seq.as_bytes());
        self.cursor = clamped;
        Ok(())
    }

    /// Toggles DECCTCEM cursor visibility (`CSI ?25l` / `CSI ?25h`).
    ///
    /// # Arguments
    ///
    /// * `visibility` - Desired [`CursorVisibility`].
    fn set_cursor_visibility(&mut self, visibility: CursorVisibility) -> Result<()> {
        self.visibility = visibility;
        let seq = match visibility {
            CursorVisibility::Hidden => "\x1b[?25l",
            CursorVisibility::Normal | CursorVisibility::High => "\x1b[?25h",
        };
        self.emit_bytes(seq.as_bytes());
        Ok(())
    }

    /// Clears the entire screen with `style` and homes the cursor (`CSI 2J CSI 1;1H`).
    ///
    /// # Arguments
    ///
    /// * `style` - Background/attribute [`CellStyle`] for cleared cells.
    fn clear_screen(&mut self, style: CellStyle) -> Result<()> {
        let sgr = format_sgr(style);
        let seq = format!("{sgr}\x1b[2J\x1b[1;1H");
        self.emit_bytes(seq.as_bytes());
        self.cursor = CursorPosition::HOME;
        Ok(())
    }

    /// Clears all columns of `row` with `style` (`CSI 2K`).
    ///
    /// # Arguments
    ///
    /// * `row` - Target 1-based [`ScreenRow`].
    /// * `style` - Background/attribute [`CellStyle`].
    fn clear_line(&mut self, row: ScreenRow, style: CellStyle) -> Result<()> {
        let clamped_row = row.clamp_max(self.dims.rows);
        let sgr = format_sgr(style);
        let seq = format!("{sgr}\x1b[{};1H\x1b[2K", clamped_row.get());
        self.emit_bytes(seq.as_bytes());
        self.cursor = CursorPosition::new(clamped_row, ScreenCol::ONE);
        Ok(())
    }

    /// Erases from `pos` through the end of `pos.row` (`CSI 0K`).
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting [`CursorPosition`].
    /// * `style` - Background/attribute [`CellStyle`].
    fn erase_to_end_of_line(&mut self, pos: CursorPosition, style: CellStyle) -> Result<()> {
        let clamped = pos.clamp_to(self.dims);
        let sgr = format_sgr(style);
        let seq = format!(
            "{sgr}\x1b[{};{}H\x1b[0K",
            clamped.row.get(),
            clamped.col.get()
        );
        self.emit_bytes(seq.as_bytes());
        self.cursor = clamped;
        Ok(())
    }

    /// Erases from `pos` through the bottom-right corner (`CSI 0J`).
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting [`CursorPosition`].
    /// * `style` - Background/attribute [`CellStyle`].
    fn erase_to_end_of_screen(&mut self, pos: CursorPosition, style: CellStyle) -> Result<()> {
        let clamped = pos.clamp_to(self.dims);
        let sgr = format_sgr(style);
        let seq = format!(
            "{sgr}\x1b[{};{}H\x1b[0J",
            clamped.row.get(),
            clamped.col.get()
        );
        self.emit_bytes(seq.as_bytes());
        self.cursor = clamped;
        Ok(())
    }

    /// Emits ASCII `BEL` (`0x07`).
    fn ring_bell(&mut self) -> Result<()> {
        self.emit_bytes(&[0x07]);
        Ok(())
    }

    /// Writes `text` at `pos` with `style`, returning the resulting [`CursorPosition`].
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting [`CursorPosition`].
    /// * `text` - Printable text span.
    /// * `style` - Cell color/attribute [`CellStyle`].
    fn write_span(
        &mut self,
        pos: CursorPosition,
        text: &str,
        style: CellStyle,
    ) -> Result<CursorPosition> {
        let clamped = pos.clamp_to(self.dims);
        let cols_usize = self.dims.cols.get() as usize;
        let avail = cols_usize.saturating_sub(clamped.col.zero_based_index());
        let sgr = format_sgr(style);
        let mut out = format!("\x1b[{};{}H{sgr}", clamped.row.get(), clamped.col.get());
        let mut written = 0_usize;
        for ch in text.chars().take(avail) {
            out.push(if ch < ' ' { ' ' } else { ch });
            written = written.saturating_add(1);
        }
        out.push_str("\x1b[0m");
        self.emit_bytes(out.as_bytes());
        let advance = u16::try_from(written).unwrap_or(self.dims.cols.get());
        let next_col = clamped.col.advance_clamped(advance, self.dims.cols);
        let next_pos = CursorPosition::new(clamped.row, next_col);
        self.cursor = next_pos;
        Ok(next_pos)
    }

    /// Flushes `stdout` when connected to a TTY.
    fn flush(&mut self) -> Result<()> {
        if self.write_stdout {
            let _ = std::io::stdout().flush();
        }
        Ok(())
    }

    /// Unused direct key reader (`read_live_key_event` handles raw TTY input).
    fn read_key(&mut self) -> Result<KeyEvent> {
        Ok(KeyEvent::Eof)
    }
}

/// Formats ANSI SGR parameters for `style`.
///
/// # Arguments
///
/// * `style` - Cell color/attribute [`CellStyle`].
fn format_sgr(style: CellStyle) -> String {
    let mut codes = String::from("\x1b[0");
    if style.attrs.contains(ScreenAttrFlags::HIGHLIGHT) {
        codes.push_str(";1");
    } else if style.attrs.contains(ScreenAttrFlags::LOWLIGHT) {
        codes.push_str(";2");
    }
    if style.attrs.contains(ScreenAttrFlags::UNDERLINE) {
        codes.push_str(";4");
    }
    if style.attrs.contains(ScreenAttrFlags::BLINK) {
        codes.push_str(";5");
    }
    if style.attrs.contains(ScreenAttrFlags::REVERSE_VIDEO) {
        codes.push_str(";7");
    }
    if style.attrs.contains(ScreenAttrFlags::OVERLINE) {
        codes.push_str(";53");
    }
    if let Some(fg) = style.fg {
        let c = fg.ansi_fg_sgr();
        codes.push(';');
        codes.push_str(&c.to_string());
    }
    if let Some(bg) = style.bg {
        let c = bg.ansi_bg_sgr();
        codes.push(';');
        codes.push_str(&c.to_string());
    }
    codes.push('m');
    codes
}

/// Returns `true` if `fd` is a live POSIX TTY.
///
/// # Arguments
///
/// * `fd` - File descriptor (`0` stdin, `1` stdout).
#[expect(
    unsafe_code,
    reason = "Audited `isatty` call on standard file descriptor."
)]
fn fd_is_tty(fd: i32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `isatty` is safe for any integer file descriptor.
        (unsafe { isatty(fd) }) != 0
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        false
    }
}

/// Polls `STDIN_FILENO` (`0`) for up to `timeout_ms` (`-1` = block indefinitely) and reads 1 byte.
///
/// # Arguments
///
/// * `lookahead` - Buffered lookahead bytes from prior CRLF/escape probes.
/// * `timeout_ms` - Timeout in milliseconds (`-1` = infinite).
#[expect(
    unsafe_code,
    reason = "Audited POSIX `poll` + `read` on `STDIN_FILENO` (`0`)."
)]
fn poll_stdin_byte(lookahead: &mut VecDeque<u8>, timeout_ms: i32) -> Option<Result<u8>> {
    if let Some(b) = lookahead.pop_front() {
        return Some(Ok(b));
    }
    #[cfg(unix)]
    {
        let mut pfd = PollFd {
            fd: 0,
            events: POLLIN_MASK,
            revents: 0,
        };
        // SAFETY: `pfd` is a valid single-element stack struct.
        let rc = unsafe { poll(&raw mut pfd, 1, timeout_ms) };
        if rc == 0 {
            return None; // Timed out!
        }
        if rc < 0 {
            return Some(Err(coblang_common::CoblangError::ScreenBackend {
                message: String::from("poll(stdin) failed"),
            }));
        }
        let mut byte = 0_u8;
        // SAFETY: `byte` is a valid 1-byte stack destination.
        let n = unsafe { read(0, (&raw mut byte).cast(), 1) };
        if n <= 0 {
            return Some(Err(coblang_common::CoblangError::ScreenBackend {
                message: String::from("read(stdin) EOF"),
            }));
        }
        Some(Ok(byte))
    }
    #[cfg(not(unix))]
    {
        let _ = timeout_ms;
        None
    }
}

/// Reads a single [`KeyEvent`] from `injected_keys` or `STDIN_FILENO` honoring `timeout_ms`.
///
/// # Arguments
///
/// * `injected` - Queue of synthetic test [`KeyEvent`]s.
/// * `lookahead` - Buffered raw stdin lookahead bytes.
/// * `timeout_ms` - Optional timeout in milliseconds.
fn read_live_key_event(
    injected: &mut VecDeque<KeyEvent>,
    lookahead: &mut VecDeque<u8>,
    timeout_ms: Option<u32>,
) -> KeyEvent {
    if let Some(k) = injected.pop_front() {
        return k;
    }
    let poll_ms = timeout_ms.map_or(-1, |ms| i32::try_from(ms).unwrap_or(i32::MAX));
    let first = match poll_stdin_byte(lookahead, poll_ms) {
        None => return KeyEvent::Timeout,
        Some(Err(_)) => return KeyEvent::Eof,
        Some(Ok(b)) => b,
    };
    match first {
        0x0D => {
            // Non-blocking 0ms check for optional LF (CRLF) so single CR on a PTY never blocks.
            if let Some(Ok(next)) = poll_stdin_byte(lookahead, 0)
                && next != 0x0A
            {
                lookahead.push_back(next);
            }
            KeyEvent::Enter
        }
        0x0A => KeyEvent::Enter,
        0x09 => KeyEvent::Tab,
        0x08 | 0x7F => KeyEvent::Backspace,
        0x04 => KeyEvent::Eof,
        0x1B => decode_live_escape(lookahead),
        b if b < 0x20 => KeyEvent::Char(' '),
        b => KeyEvent::Char(char::from(b)),
    }
}

/// Disambiguates bare `ESC` (`25ms` poll window) from VT100 CSI / SS3 / Alt+key sequences.
///
/// # Arguments
///
/// * `lookahead` - Buffered raw stdin lookahead bytes.
fn decode_live_escape(lookahead: &mut VecDeque<u8>) -> KeyEvent {
    let Some(Ok(second)) = poll_stdin_byte(lookahead, 25) else {
        return KeyEvent::Escape;
    };
    match second {
        b'[' => decode_live_csi(lookahead),
        b'O' => {
            let Some(Ok(third)) = poll_stdin_byte(lookahead, 25) else {
                return KeyEvent::Escape;
            };
            match third {
                b'P'..=b'S' => KeyEvent::Function(third - b'P' + 1),
                b'H' => KeyEvent::Home,
                b'F' => KeyEvent::End,
                b'A' => KeyEvent::Up,
                b'B' => KeyEvent::Down,
                b'C' => KeyEvent::Right,
                b'D' => KeyEvent::Left,
                _ => KeyEvent::Escape,
            }
        }
        b'b' | b'B' => KeyEvent::AltLeft,
        b'f' | b'F' => KeyEvent::AltRight,
        b'd' | b'D' | 0x7F => KeyEvent::AltDelete,
        _ => KeyEvent::Escape,
    }
}

/// Decodes `ESC [ ...` CSI escape sequences from `STDIN_FILENO`.
///
/// # Arguments
///
/// * `lookahead` - Buffered raw stdin lookahead bytes.
fn decode_live_csi(lookahead: &mut VecDeque<u8>) -> KeyEvent {
    let Some(Ok(head)) = poll_stdin_byte(lookahead, 25) else {
        return KeyEvent::Escape;
    };
    match head {
        b'A' => KeyEvent::Up,
        b'B' => KeyEvent::Down,
        b'C' => KeyEvent::Right,
        b'D' => KeyEvent::Left,
        b'H' => KeyEvent::Home,
        b'F' => KeyEvent::End,
        b'Z' => KeyEvent::BackTab,
        b'[' => {
            let Some(Ok(f)) = poll_stdin_byte(lookahead, 25) else {
                return KeyEvent::Escape;
            };
            if (b'A'..=b'E').contains(&f) {
                KeyEvent::Function(f - b'A' + 1)
            } else {
                KeyEvent::Escape
            }
        }
        d if d.is_ascii_digit() => {
            let mut p1 = u16::from(d - b'0');
            let mut p2: Option<u16> = None;
            let final_b = loop {
                let Some(Ok(b)) = poll_stdin_byte(lookahead, 25) else {
                    return KeyEvent::Escape;
                };
                if b.is_ascii_digit() {
                    let dig = u16::from(b - b'0');
                    if let Some(m) = p2.as_mut() {
                        *m = m.saturating_mul(10).saturating_add(dig);
                    } else {
                        p1 = p1.saturating_mul(10).saturating_add(dig);
                    }
                } else if b == b';' {
                    p2 = Some(0);
                } else {
                    break b;
                }
            };
            let is_alt = p2 == Some(3);
            match final_b {
                b'C' if is_alt => KeyEvent::AltRight,
                b'D' if is_alt => KeyEvent::AltLeft,
                b'~' => match p1 {
                    1 | 7 => KeyEvent::Home,
                    2 => KeyEvent::Insert,
                    3 if is_alt => KeyEvent::AltDelete,
                    3 => KeyEvent::Delete,
                    4 | 8 => KeyEvent::End,
                    5 => KeyEvent::PageUp,
                    6 => KeyEvent::PageDown,
                    11..=15 => KeyEvent::Function(u8::try_from(p1 - 10).unwrap_or(1)),
                    17..=21 => KeyEvent::Function(u8::try_from(p1 - 11).unwrap_or(6)),
                    23..=24 => KeyEvent::Function(u8::try_from(p1 - 12).unwrap_or(11)),
                    _ => KeyEvent::Escape,
                },
                _ => KeyEvent::Escape,
            }
        }
        _ => KeyEvent::Escape,
    }
}

// ============================================================================
// C-ABI Staging & Execution Entrypoints Called from `lower_m19.rs`
// ============================================================================

/// Clears the item staging list prior to a `DISPLAY` or `ACCEPT` screen statement.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M19 Screen Engine."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_screen_begin_stmt() {
    with_screen_state_mut(|st| st.staged.clear());
}

/// Applies `SET <screen_id> ATTRIBUTE on_bits / off_bits` persistent overrides.
///
/// # Arguments
///
/// * `screen_id` - Target screen ID.
/// * `on_bits` - `ScreenAttrFlags` bits turned `ON`.
/// * `off_bits` - `ScreenAttrFlags` bits turned `OFF`.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M19 `SET ... ATTRIBUTE`."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_screen_set_attr(screen_id: u32, on_bits: u32, off_bits: u32) {
    with_screen_state_mut(|st| {
        let entry = st.attr_overrides.entry(screen_id).or_insert((0, 0));
        entry.0 = (entry.0 | on_bits) & !off_bits;
        entry.1 = (entry.1 | off_bits) & !on_bits;
    });
}

/// Stages one read-only or directive screen item (`DISPLAY`, `VALUE`, `FROM`, or `BLANK`/`BELL`).
///
/// # Arguments
///
/// * `screen_id` - Owning `screen_id` (`< 0` for ad-hoc field).
/// * `at_val` - Compact `AT` integer (`< 0` when omitted).
/// * `at_digits` - `4` or `6` when `at_val >= 0`.
/// * `line_kind` - `0=Omitted, 1=Abs, 2=Plus, 3=Minus`.
/// * `line_val` - Row coordinate or offset.
/// * `col_kind` - `0=Omitted, 1=Abs, 2=Plus, 3=Minus`.
/// * `col_val` - Column coordinate or offset.
/// * `attr_bits` - `ScreenAttrFlags` bitmask.
/// * `fg_color` - Foreground color (`< 0` default, `0..=15` color).
/// * `bg_color` - Background color (`< 0` default, `0..=15` color).
/// * `scroll_lines` - Signed scroll lines (`> 0` up, `< 0` down, `0` none).
/// * `width` - Display width (`0` for pure directive).
/// * `mode_flags` - Bit `0`: `repeat_all` (`VALUE ALL`); Bit `1`: `is_figurative_low_value`.
/// * `data_ptr` - Display byte pointer (`null` for pure directive).
/// * `data_len` - Display byte length.
///
/// # Safety
///
/// `data_ptr` must be valid for `data_len` bytes when `data_len > 0`.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI item stager for M19 Screen Engine."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_screen_stage_display_item(
    screen_id: i32,
    at_val: i64,
    at_digits: u32,
    line_kind: u32,
    line_val: i64,
    col_kind: u32,
    col_val: i64,
    attr_bits: u32,
    fg_color: i32,
    bg_color: i32,
    scroll_lines: i32,
    width: u32,
    mode_flags: u32,
    data_ptr: *const u8,
    data_len: usize,
) {
    let bytes = if data_ptr.is_null() || data_len == 0 {
        Vec::new()
    } else {
        // SAFETY: Caller guarantees `[data_ptr..data_ptr + data_len)` is readable when `data_len > 0`.
        let s = unsafe { core::slice::from_raw_parts(data_ptr, data_len) };
        s.to_vec()
    };
    let binding = if width == 0 && bytes.is_empty() {
        RtItemBinding::ActionOnly
    } else {
        let repeat_all = (mode_flags & 1) != 0;
        let low_values_noop =
            (mode_flags & 2) != 0 || (!bytes.is_empty() && bytes.iter().all(|&b| b == 0));
        RtItemBinding::DisplayOnly {
            bytes,
            repeat_all,
            low_values_noop,
        }
    };
    let eff_w = if width > 0 {
        u16::try_from(width).unwrap_or(80)
    } else {
        u16::try_from(data_len).unwrap_or(0)
    };
    push_staged_item(
        StagedItemParams {
            screen_id,
            at_val,
            at_digits,
            line_kind,
            line_val,
            col_kind,
            col_val,
            attr_bits,
            fg_color,
            bg_color,
            prompt_byte: -1,
            scroll_lines,
            width: eff_w,
        },
        binding,
    );
}

/// Stages one editable screen field (`TO`, `USING`, `FROM ... TO`, or ad-hoc `ACCEPT target`).
///
/// # Arguments
///
/// * `screen_id` - Owning `screen_id` (`< 0` for ad-hoc `ACCEPT`).
/// * `at_val` - Compact `AT` integer (`< 0` when omitted).
/// * `at_digits` - `4` or `6`.
/// * `line_kind` - `0=Omitted, 1=Abs, 2=Plus, 3=Minus`.
/// * `line_val` - Row coordinate or offset.
/// * `col_kind` - `0=Omitted, 1=Abs, 2=Plus, 3=Minus`.
/// * `col_val` - Column coordinate or offset.
/// * `attr_bits` - `ScreenAttrFlags` bitmask.
/// * `force_no_update` - Non-zero when `NO UPDATE` was specified.
/// * `fg_color` - Foreground color (`< 0` default).
/// * `bg_color` - Background color (`< 0` default).
/// * `prompt_byte` - Prompt ASCII byte (`< 0` when `PROMPT` disabled).
/// * `width` - Editable cell width (`>= 1`).
/// * `preload_edit` - Non-zero when initial value pre-populates the edit buffer (`USING` or `UPDATE`).
/// * `init_ptr` - Initial display byte pointer (`null` for blank).
/// * `init_len` - Initial display byte length.
/// * `dst_ptr` - Destination COBOL field pointer.
/// * `dst_len` - Destination COBOL field byte length.
/// * `dst_spec_ptr` - Serialized `RuntimeFieldSpec` byte slice pointer.
/// * `dst_spec_len` - Byte length of `dst_spec_ptr`.
///
/// # Safety
///
/// Non-null pointers must remain valid through the immediately following `execute_display` / `execute_accept` call.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI editable field stager for M19 Screen Engine."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_screen_stage_edit_item(
    screen_id: i32,
    at_val: i64,
    at_digits: u32,
    line_kind: u32,
    line_val: i64,
    col_kind: u32,
    col_val: i64,
    attr_bits: u32,
    force_no_update: i32,
    fg_color: i32,
    bg_color: i32,
    prompt_byte: i32,
    width: u32,
    preload_edit: i32,
    init_ptr: *const u8,
    init_len: usize,
    dst_ptr: *mut u8,
    dst_len: usize,
    dst_spec_ptr: *const u8,
    dst_spec_len: usize,
) {
    let init_bytes = if init_ptr.is_null() || init_len == 0 {
        Vec::new()
    } else {
        // SAFETY: Caller passes valid initial buffer when `init_len > 0`.
        let s = unsafe { core::slice::from_raw_parts(init_ptr, init_len) };
        s.to_vec()
    };
    let dst_spec = if dst_spec_ptr.is_null() || dst_spec_len == 0 {
        Vec::new()
    } else {
        // SAFETY: Caller passes valid static `RuntimeFieldSpec` bytes when `dst_spec_len > 0`.
        let s = unsafe { core::slice::from_raw_parts(dst_spec_ptr, dst_spec_len) };
        s.to_vec()
    };
    let eff_w = u16::try_from(width.max(1)).unwrap_or(1);
    push_staged_item(
        StagedItemParams {
            screen_id,
            at_val,
            at_digits,
            line_kind,
            line_val,
            col_kind,
            col_val,
            attr_bits,
            fg_color,
            bg_color,
            prompt_byte,
            scroll_lines: 0,
            width: eff_w,
        },
        RtItemBinding::Editable {
            init_bytes,
            preload_edit: preload_edit != 0 && force_no_update == 0,
            dst_addr: dst_ptr.addr(),
            dst_len,
            dst_spec,
        },
    );
}

/// Geometry and attribute parameters passed to [`push_staged_item`].
#[derive(Clone, Copy)]
struct StagedItemParams {
    /// Owning `screen_id` (`< 0` for ad-hoc items).
    screen_id: i32,
    /// Compact `AT` coordinate integer (`< 0` when omitted).
    at_val: i64,
    /// `AT` digit count (`4` or `6`).
    at_digits: u32,
    /// `LINE` coordinate kind (`0..=3`).
    line_kind: u32,
    /// `LINE` coordinate value.
    line_val: i64,
    /// `COL` coordinate kind (`0..=3`).
    col_kind: u32,
    /// `COL` coordinate value.
    col_val: i64,
    /// `ScreenAttrFlags` bitmask.
    attr_bits: u32,
    /// Foreground color code.
    fg_color: i32,
    /// Background color code.
    bg_color: i32,
    /// Prompt ASCII byte (`< 0` when disabled).
    prompt_byte: i32,
    /// Signed scroll line count.
    scroll_lines: i32,
    /// Display/edit cell width.
    width: u16,
}

/// Internal helper pushing a [`StagedScreenItem`] onto `SCREEN_STATE.staged`.
///
/// # Arguments
///
/// * `params` - Geometry and attribute configuration.
/// * `binding` - Display-only, editable, or action-only payload.
fn push_staged_item(params: StagedItemParams, binding: RtItemBinding) {
    let sid = u32::try_from(params.screen_id).ok();
    let at_compact = u32::try_from(params.at_val)
        .ok()
        .map(|v| (v, u8::try_from(params.at_digits).unwrap_or(4)));
    let prompt = u8::try_from(params.prompt_byte).ok();
    with_screen_state_mut(|st| {
        let raw_ov = (params.line_kind == 1 && params.line_val > i64::from(st.dims.rows.get()))
            || (params.col_kind == 1 && params.col_val > i64::from(st.dims.cols.get()));
        st.staged.push(StagedScreenItem {
            screen_id: sid,
            at_compact,
            line: RtCoord::decode(params.line_kind, params.line_val).to_line_spec(),
            col: RtCoord::decode(params.col_kind, params.col_val).to_col_spec(),
            raw_coord_overflow: raw_ov,
            attr_bits: params.attr_bits,
            fg_color: params.fg_color,
            bg_color: params.bg_color,
            width: params.width,
            prompt_byte: prompt,
            scroll_lines: params.scroll_lines,
            binding,
        });
    });
}

/// Renders a single [`StagedScreenItem`] onto `backend` during `DISPLAY`, returning `true` on coordinate overflow.
///
/// # Arguments
///
/// * `item` - Staged screen item to render.
/// * `overrides` - Active `SET ... ATTRIBUTE` map.
/// * `dims` - Virtual terminal dimensions.
/// * `cur` - Mutable cursor position.
/// * `at_origin` - Mutable flag tracking initial home position.
/// * `backend` - Output VT100 backend.
fn render_one_display_item(
    item: &StagedScreenItem,
    overrides: &BTreeMap<u32, (u32, u32)>,
    dims: ScreenDimensions,
    cur: &mut CursorPosition,
    at_origin: &mut bool,
    backend: &mut LiveAnsiBackend<'_>,
) -> bool {
    let mut overflow = item.raw_coord_overflow;
    if let LineSpec::Absolute(r) = item.line
        && r > dims.rows
    {
        overflow = true;
    }
    if let ColSpec::Absolute(c) = item.col
        && c > dims.cols
    {
        overflow = true;
    }
    if let Some((at_v, at_d)) = item.at_compact {
        let (raw_r, raw_c) = if at_d >= 6 {
            (at_v / 1000, at_v % 1000)
        } else {
            (at_v / 100, at_v % 100)
        };
        if raw_r > u32::from(dims.rows.get()) || raw_c > u32::from(dims.cols.get()) {
            overflow = true;
        }
    }
    let (pos, style, flags) =
        resolve_item_geometry_and_style(item, overrides, dims, cur, at_origin);
    if item.scroll_lines != 0 {
        emit_scroll(backend, item.scroll_lines);
    }
    if flags.contains(ScreenAttrFlags::BLANK_SCREEN) {
        let _ = backend.clear_screen(style);
    }
    if flags.contains(ScreenAttrFlags::BLANK_LINE) {
        let _ = backend.clear_line(pos.row, style);
    }
    if flags.contains(ScreenAttrFlags::ERASE_EOS) {
        let _ = backend.erase_to_end_of_screen(pos, style);
    }
    if flags.contains(ScreenAttrFlags::ERASE_EOL) {
        let _ = backend.erase_to_end_of_line(pos, style);
    }
    if flags.contains(ScreenAttrFlags::BELL) {
        let _ = backend.ring_bell();
    }
    match &item.binding {
        RtItemBinding::ActionOnly => {}
        RtItemBinding::DisplayOnly {
            bytes,
            repeat_all,
            low_values_noop,
        } => {
            if *low_values_noop {
                let _ = backend.set_cursor(pos);
                *cur = pos;
            } else {
                let text = render_display_text(bytes, *repeat_all, item.width as usize, flags);
                if let Ok(end_p) = backend.write_span(pos, &text, style) {
                    *cur = end_p;
                }
            }
        }
        RtItemBinding::Editable { init_bytes, .. } => {
            let text = if flags.contains(ScreenAttrFlags::SECURE) {
                "*".repeat(item.width as usize)
            } else {
                render_display_text(init_bytes, false, item.width as usize, flags)
            };
            if let Ok(end_p) = backend.write_span(pos, &text, style) {
                *cur = end_p;
            }
        }
    }
    overflow
}

/// Executes all staged items as a `DISPLAY` statement, updating `CRT STATUS` on coordinate overflow.
/// Returns `1` on coordinate overflow or `0` (`OK`).
///
/// # Arguments
///
/// * `_cursor_reg_ptr` - Reserved `SPECIAL-NAMES. CURSOR` storage pointer (unused on `DISPLAY`).
/// * `_cursor_reg_len` - Reserved byte length of `_cursor_reg_ptr`.
/// * `crt_status_ptr` - Optional `SPECIAL-NAMES. CRT STATUS` storage pointer (`null` when none).
/// * `crt_status_len` - Byte length of `crt_status_ptr` (`3` or `4`).
/// * `cob_crt_ptr` - Implicit `COB-CRT-STATUS` (`PIC 9(4)`) storage pointer.
///
/// # Safety
///
/// Non-null register pointers must be writable for their corresponding byte lengths.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI `DISPLAY` executor for M19 Screen Engine."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_screen_execute_display(
    _cursor_reg_ptr: *mut u8,
    _cursor_reg_len: usize,
    crt_status_ptr: *mut u8,
    crt_status_len: usize,
    cob_crt_ptr: *mut u8,
) -> i32 {
    let write_tty = fd_is_tty(1) || fd_is_tty(0);
    with_screen_state_mut(|st| {
        let items = std::mem::take(&mut st.staged);
        let overrides = st.attr_overrides.clone();
        let dims = st.dims;
        let mut cur = st.cursor;
        let mut at_origin = cur == CursorPosition::HOME;
        let mut backend = LiveAnsiBackend {
            dims,
            cursor: cur,
            visibility: CursorVisibility::Normal,
            capture: &mut st.captured_ansi,
            write_stdout: write_tty,
        };

        let mut coord_overflow = false;
        for item in &items {
            if render_one_display_item(
                item,
                &overrides,
                dims,
                &mut cur,
                &mut at_origin,
                &mut backend,
            ) {
                coord_overflow = true;
            }
        }
        let _ = backend.flush();
        st.cursor = cur;
        if coord_overflow {
            // SAFETY: Register pointers validated by caller; `null_mut()` skips `CURSOR` update.
            unsafe {
                write_special_registers(
                    CrtStatusCode::SCREEN_ERROR,
                    cur,
                    core::ptr::null_mut(),
                    0,
                    crt_status_ptr,
                    crt_status_len,
                    cob_crt_ptr,
                );
            }
        }
        i32::from(coord_overflow)
    })
}

/// Renders static prompts and builds [`LiveFieldEditor`] entries for [`coblang_rt_screen_execute_accept`].
///
/// # Arguments
///
/// * `items` - Staged screen items for this `ACCEPT`.
/// * `overrides` - Active `SET ... ATTRIBUTE` map.
/// * `dims` - Virtual terminal dimensions.
/// * `cur` - Mutable cursor position.
/// * `backend` - Output VT100 backend.
fn stage_accept_editors(
    items: &[StagedScreenItem],
    overrides: &BTreeMap<u32, (u32, u32)>,
    dims: ScreenDimensions,
    cur: &mut CursorPosition,
    backend: &mut LiveAnsiBackend<'_>,
) -> Vec<LiveFieldEditor> {
    let mut at_origin = *cur == CursorPosition::HOME;
    let mut editors = Vec::new();
    for item in items {
        let (pos, style, flags) =
            resolve_item_geometry_and_style(item, overrides, dims, cur, &mut at_origin);
        if flags.contains(ScreenAttrFlags::BELL) {
            let _ = backend.ring_bell();
        }
        match &item.binding {
            RtItemBinding::ActionOnly => {}
            RtItemBinding::DisplayOnly {
                bytes,
                repeat_all,
                low_values_noop,
            } => {
                if !*low_values_noop {
                    let text = render_display_text(bytes, *repeat_all, item.width as usize, flags);
                    if let Ok(end_p) = backend.write_span(pos, &text, style) {
                        *cur = end_p;
                    }
                }
            }
            RtItemBinding::Editable {
                init_bytes,
                preload_edit,
                dst_addr,
                dst_len,
                dst_spec,
            } => {
                let w = item.width.max(1) as usize;
                let mut chars = vec![' '; w];
                if *preload_edit {
                    for (dst_c, &b) in chars.iter_mut().zip(init_bytes.iter()) {
                        *dst_c = if (0x20..0x7F).contains(&b) {
                            char::from(b)
                        } else {
                            ' '
                        };
                    }
                }
                let ed = LiveFieldEditor {
                    pos,
                    style,
                    flags,
                    prompt_byte: item.prompt_byte,
                    width: item.width.max(1),
                    chars,
                    dst_addr: *dst_addr,
                    dst_len: *dst_len,
                    dst_spec: dst_spec.clone(),
                };
                draw_live_editor(&ed, backend);
                let end_col = pos.col.advance_clamped(ed.width, dims.cols);
                *cur = CursorPosition::new(pos.row, end_col);
                editors.push(ed);
            }
        }
    }
    let _ = backend.flush();
    editors
}

/// Executes all staged items as an interactive `ACCEPT` statement, returning `1` on exception
/// (`Escape`, `F1`..`F64`, `PageUp`/`PageDown`, `Timeout`, `Eof`) or `0` on normal completion.
///
/// # Arguments
///
/// * `timeout_ms` - Timeout in milliseconds (`<= 0` when no `TIMEOUT` clause).
/// * `initial_field_cursor` - Initial 1-based inline `CURSOR` offset (`<= 0` when omitted).
/// * `out_field_cursor` - Output 1-based inline `CURSOR` offset (`null` when omitted).
/// * `cursor_reg_ptr` - Optional `SPECIAL-NAMES. CURSOR` storage pointer.
/// * `cursor_reg_len` - Byte length of `cursor_reg_ptr` (`4` or `6`).
/// * `crt_status_ptr` - Optional `SPECIAL-NAMES. CRT STATUS` storage pointer.
/// * `crt_status_len` - Byte length of `crt_status_ptr` (`3` or `4`).
/// * `cob_crt_ptr` - Implicit `COB-CRT-STATUS` (`PIC 9(4)`) storage pointer.
///
/// # Safety
///
/// Non-null output and register pointers must be writable for their declared widths.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI `ACCEPT` executor for M19 Screen Engine."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_screen_execute_accept(
    timeout_ms: i64,
    initial_field_cursor: i64,
    out_field_cursor: *mut i64,
    cursor_reg_ptr: *mut u8,
    cursor_reg_len: usize,
    crt_status_ptr: *mut u8,
    crt_status_len: usize,
    cob_crt_ptr: *mut u8,
) -> i32 {
    let write_tty = fd_is_tty(1) || fd_is_tty(0);
    let eff_timeout = u32::try_from(timeout_ms).ok().filter(|&ms| ms > 0);

    with_screen_state_mut(|st| {
        let items = std::mem::take(&mut st.staged);
        let overrides = st.attr_overrides.clone();
        let dims = st.dims;
        let mut cur = st.cursor;

        let mut editors = {
            let mut backend = LiveAnsiBackend {
                dims,
                cursor: cur,
                visibility: CursorVisibility::Normal,
                capture: &mut st.captured_ansi,
                write_stdout: write_tty,
            };
            stage_accept_editors(&items, &overrides, dims, &mut cur, &mut backend)
        };

        // `ACCEPT OMITTED` or screen with 0 editable fields: wait for a single keystroke!
        if editors.is_empty() {
            let key =
                read_live_key_event(&mut st.injected_keys, &mut st.stdin_lookahead, eff_timeout);
            let status = map_key_to_gnucobol_crt_status(key);
            // SAFETY: Validated register pointers.
            unsafe {
                write_special_registers(
                    status,
                    cur,
                    cursor_reg_ptr,
                    cursor_reg_len,
                    crt_status_ptr,
                    crt_status_len,
                    cob_crt_ptr,
                );
            }
            return i32::from(status.get() != 0);
        }

        // Determine initial `(field_idx, char_offset)` from inline `CURSOR` or `SPECIAL-NAMES. CURSOR`.
        let (mut field_idx, mut char_offset) = resolve_initial_focus(
            &editors,
            initial_field_cursor,
            cursor_reg_ptr,
            cursor_reg_len,
        );
        let mut insert_on = false;

        let final_status = loop {
            let cur_ed_pos = editor_cursor_pos(&editors, field_idx, char_offset, dims);
            {
                let mut backend = LiveAnsiBackend {
                    dims,
                    cursor: cur_ed_pos,
                    visibility: CursorVisibility::Normal,
                    capture: &mut st.captured_ansi,
                    write_stdout: write_tty,
                };
                let _ = backend.set_cursor(cur_ed_pos);
                let _ = backend.flush();
            }

            let key =
                read_live_key_event(&mut st.injected_keys, &mut st.stdin_lookahead, eff_timeout);
            let mut backend = LiveAnsiBackend {
                dims,
                cursor: cur_ed_pos,
                visibility: CursorVisibility::Normal,
                capture: &mut st.captured_ansi,
                write_stdout: write_tty,
            };
            if let Some(done_status) = step_live_editor_key(
                key,
                &mut editors,
                &mut field_idx,
                &mut char_offset,
                &mut insert_on,
                &mut backend,
            ) {
                let _ = backend.set_cursor_visibility(CursorVisibility::Normal);
                let _ = backend.flush();
                break done_status;
            }
            let _ = backend.flush();
        };

        let final_pos = editor_cursor_pos(&editors, field_idx, char_offset, dims);
        st.cursor = final_pos;
        if !out_field_cursor.is_null() {
            // SAFETY: Validated `i64` output slot.
            unsafe {
                *out_field_cursor = i64::try_from(char_offset.saturating_add(1)).unwrap_or(1);
            };
        }
        // SAFETY: Validated register pointers.
        unsafe {
            write_special_registers(
                final_status,
                final_pos,
                cursor_reg_ptr,
                cursor_reg_len,
                crt_status_ptr,
                crt_status_len,
                cob_crt_ptr,
            );
        }
        i32::from(final_status.get() != 0)
    })
}

/// Mutable in-flight editable field during `coblang_rt_screen_execute_accept`.
struct LiveFieldEditor {
    /// 1-based starting cell coordinate.
    pos: CursorPosition,
    /// Visual style.
    style: CellStyle,
    /// Attribute flags.
    flags: ScreenAttrFlags,
    /// Optional prompt character byte.
    prompt_byte: Option<u8>,
    /// Visible width (`>= 1`).
    width: u16,
    /// Editable character cells (`len == width`).
    chars: Vec<char>,
    /// Destination COBOL memory address.
    dst_addr: usize,
    /// Destination byte length.
    dst_len: usize,
    /// Serialized `RuntimeFieldSpec` bytes.
    dst_spec: Vec<u8>,
}

impl LiveFieldEditor {
    /// Returns `true` if the target field is numeric or numeric-edited.
    fn is_numeric_target(&self) -> bool {
        decode_field_spec(&self.dst_spec).is_ok_and(|spec| {
            matches!(
                spec.desc.ty.category(),
                DataCategory::Numeric | DataCategory::NumericEdited
            )
        })
    }

    /// Validates `REQUIRED` (at least one non-space/non-zero) and `FULL` (all cells filled or all blank).
    fn satisfies_constraints(&self) -> bool {
        let non_blank_cnt = self.chars.iter().filter(|&&c| c != ' ').count();
        if self.flags.contains(ScreenAttrFlags::REQUIRED) {
            if non_blank_cnt == 0 {
                return false;
            }
            if self.is_numeric_target()
                && self
                    .chars
                    .iter()
                    .all(|&c| matches!(c, ' ' | '0' | '+' | '-' | '.'))
            {
                return false;
            }
        }
        !(self.flags.contains(ScreenAttrFlags::FULL)
            && !self.is_numeric_target()
            && non_blank_cnt > 0
            && non_blank_cnt < self.chars.len())
    }

    /// Commits `self.chars` into `(self.dst_addr, self.dst_len)` via `runtime_execute_move`.
    #[expect(
        unsafe_code,
        reason = "Writes committed field value into caller's COBOL storage."
    )]
    fn commit_to_cobol(&self) {
        if self.dst_addr == 0 || self.dst_len == 0 || self.dst_spec.is_empty() {
            return;
        }
        let dst_ptr = core::ptr::with_exposed_provenance_mut::<u8>(self.dst_addr);
        // SAFETY: `self.dst_addr` and `self.dst_len` reference valid COBOL field storage for this `ACCEPT`.
        let dst_slice = unsafe { core::slice::from_raw_parts_mut(dst_ptr, self.dst_len) };
        if self.is_numeric_target() {
            let text: String = self.chars.iter().collect();
            let trimmed = text.trim();
            let neg = trimmed.starts_with('-') || trimmed.ends_with('-');
            let mut cleaned = String::with_capacity(trimmed.len() + 1);
            if neg {
                cleaned.push('-');
            }
            let mut saw_digit = false;
            let mut saw_dot = false;
            for ch in trimmed.chars() {
                if ch == '.' && !saw_dot {
                    saw_dot = true;
                    cleaned.push('.');
                } else if ch.is_ascii_digit() {
                    saw_digit = true;
                    cleaned.push(ch);
                }
            }
            if !saw_digit {
                cleaned.push('0');
            }
            let _ = runtime_execute_move(
                RuntimeMoveSourceKind::DecimalAscii,
                cleaned.as_bytes(),
                &[],
                dst_slice,
                &self.dst_spec,
            );
        } else {
            let mut raw_bytes: Vec<u8> = self.chars.iter().map(|&c| c as u8).collect();
            if self.flags.contains(ScreenAttrFlags::ZERO_FILL) {
                for b in &mut raw_bytes {
                    if *b == b' ' {
                        *b = b'0';
                    }
                }
            }
            let _ = runtime_execute_move(
                RuntimeMoveSourceKind::AlphanumericLiteral,
                &raw_bytes,
                &[],
                dst_slice,
                &self.dst_spec,
            );
        }
    }
}

/// Renders an editable field (`SECURE` `*`, `NO-ECHO`, or `PROMPT` fill) onto `backend`.
///
/// # Arguments
///
/// * `ed` - Active [`LiveFieldEditor`] state.
/// * `backend` - Output VT100 backend.
fn draw_live_editor(ed: &LiveFieldEditor, backend: &mut LiveAnsiBackend<'_>) {
    let prompt_ch = ed
        .prompt_byte
        .or_else(|| ed.flags.contains(ScreenAttrFlags::PROMPT).then_some(b'_'))
        .map(char::from);
    let mut rendered = String::with_capacity(ed.chars.len());
    for &c in &ed.chars {
        if ed.flags.contains(ScreenAttrFlags::SECURE) {
            rendered.push(if c == ' ' {
                prompt_ch.unwrap_or(' ')
            } else {
                '*'
            });
        } else if c == ' '
            && let Some(p) = prompt_ch
        {
            rendered.push(p);
        } else {
            rendered.push(c);
        }
    }
    let _ = backend.write_span(ed.pos, &rendered, ed.style);
}

/// Handles horizontal cursor navigation (`Home`, `End`, `Left`, `Right`, `AltLeft`, `AltRight`) inside [`step_live_editor_key`].
///
/// # Arguments
///
/// * `key` - Navigation [`KeyEvent`].
/// * `editors` - Active field editors.
/// * `field_idx` - Mutable focused editor index.
/// * `char_offset` - Mutable character offset within `editors[*field_idx]`.
fn step_editor_horizontal_nav(
    key: KeyEvent,
    editors: &[LiveFieldEditor],
    field_idx: &mut usize,
    char_offset: &mut usize,
) {
    match key {
        KeyEvent::Home => {
            if let Some(ed) = editors.get(*field_idx) {
                let first_non_space = ed.chars.iter().position(|&c| c != ' ').unwrap_or(0);
                *char_offset = if *char_offset == 0 && first_non_space > 0 {
                    first_non_space
                } else {
                    0
                };
            }
        }
        KeyEvent::End => {
            if let Some(ed) = editors.get(*field_idx) {
                let max_cell = ed.chars.len().saturating_sub(1);
                let after_last = ed
                    .chars
                    .iter()
                    .rposition(|&c| c != ' ')
                    .map_or(0, |p| (p + 1).min(max_cell));
                *char_offset = if *char_offset == after_last {
                    max_cell
                } else {
                    after_last
                };
            }
        }
        KeyEvent::Left => {
            if *char_offset > 0 {
                *char_offset -= 1;
            } else if *field_idx > 0 {
                *field_idx -= 1;
                *char_offset = editors
                    .get(*field_idx)
                    .map_or(0, |e| e.chars.len().saturating_sub(1));
            }
        }
        KeyEvent::Right => {
            let max_c = editors
                .get(*field_idx)
                .map_or(0, |e| e.chars.len().saturating_sub(1));
            if *char_offset < max_c {
                *char_offset += 1;
            } else if *field_idx + 1 < editors.len() {
                *field_idx += 1;
                *char_offset = 0;
            }
        }
        KeyEvent::AltLeft => {
            if let Some(ed) = editors.get(*field_idx) {
                let mut p = (*char_offset).min(ed.chars.len());
                while p > 0 && ed.chars.get(p - 1) == Some(&' ') {
                    p -= 1;
                }
                while p > 0 && ed.chars.get(p - 1) != Some(&' ') {
                    p -= 1;
                }
                *char_offset = p;
            }
        }
        KeyEvent::AltRight => {
            if let Some(ed) = editors.get(*field_idx) {
                let max_c = ed.chars.len().saturating_sub(1);
                let mut p = *char_offset;
                while p < max_c && ed.chars.get(p) != Some(&' ') {
                    p += 1;
                }
                while p < max_c && ed.chars.get(p) == Some(&' ') {
                    p += 1;
                }
                *char_offset = p.min(max_c);
            }
        }
        _ => {}
    }
}

/// Handles a printable `KeyEvent::Char(ch)` in [`step_live_editor_key`].
///
/// # Arguments
///
/// * `ch` - Typed character.
/// * `editors` - Mutable field editors.
/// * `field_idx` - Mutable focused editor index.
/// * `char_offset` - Mutable character offset within `editors[*field_idx]`.
/// * `insert_on` - Whether insert mode is active.
/// * `backend` - Output VT100 backend.
fn step_editor_char_input(
    ch: char,
    editors: &mut [LiveFieldEditor],
    field_idx: &mut usize,
    char_offset: &mut usize,
    insert_on: bool,
    backend: &mut LiveAnsiBackend<'_>,
) -> Option<CrtStatusCode> {
    let (is_auto, max_c) = {
        let ed = editors.get_mut(*field_idx)?;
        if ed.is_numeric_target() && !matches!(ch, '0'..='9' | ' ' | '+' | '-' | '.') {
            let _ = backend.ring_bell();
            return None;
        }
        let len = ed.chars.len();
        if insert_on {
            if ed.chars.last() == Some(&' ') && *char_offset < len {
                let _ = ed.chars.pop();
                ed.chars.insert(*char_offset, ch);
            } else {
                let _ = backend.ring_bell();
                return None;
            }
        } else if let Some(slot) = ed.chars.get_mut(*char_offset) {
            *slot = ch;
        }
        draw_live_editor(ed, backend);
        (
            ed.flags.contains(ScreenAttrFlags::AUTO),
            len.saturating_sub(1),
        )
    };
    if *char_offset < max_c {
        *char_offset += 1;
        return None;
    }
    if !is_auto {
        return None;
    }
    if editors
        .get(*field_idx)
        .is_some_and(|e| !e.satisfies_constraints())
    {
        let _ = backend.ring_bell();
        return None;
    }
    if *field_idx + 1 < editors.len() {
        *field_idx += 1;
        *char_offset = 0;
        return None;
    }
    finish_accept_if_valid(
        CrtStatusCode::NORMAL_COMPLETION,
        editors,
        field_idx,
        char_offset,
        backend,
    )
}

/// Handles `Backspace`, `Delete`, and `AltDelete` editing keys inside [`step_live_editor_key`].
///
/// # Arguments
///
/// * `key` - Deletion [`KeyEvent`].
/// * `editors` - Mutable field editors.
/// * `field_idx` - Focused editor index.
/// * `char_offset` - Mutable character offset within `editors[field_idx]`.
/// * `backend` - Output VT100 backend.
fn step_editor_delete_key(
    key: KeyEvent,
    editors: &mut [LiveFieldEditor],
    field_idx: usize,
    char_offset: &mut usize,
    backend: &mut LiveAnsiBackend<'_>,
) {
    match key {
        KeyEvent::Backspace => {
            if *char_offset > 0 {
                *char_offset -= 1;
                if let Some(ed) = editors.get_mut(field_idx) {
                    if *char_offset < ed.chars.len() {
                        let _ = ed.chars.remove(*char_offset);
                        ed.chars.push(' ');
                    }
                    draw_live_editor(ed, backend);
                }
            }
        }
        KeyEvent::Delete => {
            if let Some(ed) = editors.get_mut(field_idx) {
                if *char_offset < ed.chars.len() {
                    let _ = ed.chars.remove(*char_offset);
                    ed.chars.push(' ');
                }
                draw_live_editor(ed, backend);
            }
        }
        KeyEvent::AltDelete => {
            if let Some(ed) = editors.get_mut(field_idx) {
                for slot in ed.chars.iter_mut().skip(*char_offset) {
                    *slot = ' ';
                }
                draw_live_editor(ed, backend);
            }
        }
        _ => {}
    }
}

/// Processes one [`KeyEvent`] in the interactive `ACCEPT` loop, returning `Some(status)` when done.
///
/// # Arguments
///
/// * `key` - Decoded [`KeyEvent`].
/// * `editors` - Mutable field editors.
/// * `field_idx` - Mutable focused editor index.
/// * `char_offset` - Mutable character offset within `editors[*field_idx]`.
/// * `insert_on` - Mutable insert-mode toggle.
/// * `backend` - Output VT100 backend.
fn step_live_editor_key(
    key: KeyEvent,
    editors: &mut [LiveFieldEditor],
    field_idx: &mut usize,
    char_offset: &mut usize,
    insert_on: &mut bool,
    backend: &mut LiveAnsiBackend<'_>,
) -> Option<CrtStatusCode> {
    match key {
        KeyEvent::Insert => {
            *insert_on = !*insert_on;
            let vis = if *insert_on {
                CursorVisibility::High
            } else {
                CursorVisibility::Normal
            };
            let _ = backend.set_cursor_visibility(vis);
            None
        }
        KeyEvent::Home
        | KeyEvent::End
        | KeyEvent::Left
        | KeyEvent::Right
        | KeyEvent::AltLeft
        | KeyEvent::AltRight => {
            step_editor_horizontal_nav(key, editors, field_idx, char_offset);
            None
        }
        KeyEvent::Backspace | KeyEvent::Delete | KeyEvent::AltDelete => {
            step_editor_delete_key(key, editors, *field_idx, char_offset, backend);
            None
        }
        KeyEvent::Char(ch) => {
            step_editor_char_input(ch, editors, field_idx, char_offset, *insert_on, backend)
        }
        KeyEvent::Tab | KeyEvent::Down => {
            if *field_idx + 1 < editors.len() {
                *field_idx += 1;
                *char_offset = 0;
                None
            } else {
                finish_accept_if_valid(
                    map_key_to_gnucobol_crt_status(key),
                    editors,
                    field_idx,
                    char_offset,
                    backend,
                )
            }
        }
        KeyEvent::BackTab | KeyEvent::Up => {
            if *field_idx > 0 {
                *field_idx -= 1;
                *char_offset = 0;
                None
            } else {
                finish_accept_if_valid(
                    map_key_to_gnucobol_crt_status(key),
                    editors,
                    field_idx,
                    char_offset,
                    backend,
                )
            }
        }
        KeyEvent::Enter => finish_accept_if_valid(
            CrtStatusCode::NORMAL_COMPLETION,
            editors,
            field_idx,
            char_offset,
            backend,
        ),
        KeyEvent::PageUp | KeyEvent::PageDown | KeyEvent::Function(_) | KeyEvent::Timeout => {
            for ed in editors.iter() {
                ed.commit_to_cobol();
            }
            Some(map_key_to_gnucobol_crt_status(key))
        }
        KeyEvent::Escape => Some(CrtStatusCode::ESCAPE),
        KeyEvent::Eof => {
            for ed in editors.iter() {
                ed.commit_to_cobol();
            }
            Some(CrtStatusCode::NORMAL_COMPLETION)
        }
    }
}

/// Validates all editors on Enter / field-end; commits and returns `Some(status)` if valid, else rings bell.
///
/// # Arguments
///
/// * `status` - Completion [`CrtStatusCode`] to return when all constraints pass.
/// * `editors` - All active field editors.
/// * `field_idx` - Mutable focused editor index (redirected to failing editor on violation).
/// * `char_offset` - Mutable character offset.
/// * `backend` - Output VT100 backend.
fn finish_accept_if_valid(
    status: CrtStatusCode,
    editors: &[LiveFieldEditor],
    field_idx: &mut usize,
    char_offset: &mut usize,
    backend: &mut LiveAnsiBackend<'_>,
) -> Option<CrtStatusCode> {
    if let Some(bad_idx) = editors.iter().position(|e| !e.satisfies_constraints()) {
        *field_idx = bad_idx;
        *char_offset = 0;
        let _ = backend.ring_bell();
        None
    } else {
        for ed in editors {
            ed.commit_to_cobol();
        }
        Some(status)
    }
}

/// Maps [`KeyEvent`] to `GnuCOBOL` `COB-CRT-STATUS` codes (`8001` for `Timeout` per `screenio.cpy`!).
///
/// # Arguments
///
/// * `key` - Terminating [`KeyEvent`].
fn map_key_to_gnucobol_crt_status(key: KeyEvent) -> CrtStatusCode {
    match key {
        KeyEvent::Timeout => CrtStatusCode::try_new(8001).unwrap_or(CrtStatusCode::TIMEOUT),
        KeyEvent::Tab | KeyEvent::Down | KeyEvent::BackTab | KeyEvent::Up | KeyEvent::Enter => {
            CrtStatusCode::NORMAL_COMPLETION
        }
        other => other.to_crt_status(),
    }
}

/// Computes initial `(field_idx, char_offset)` from inline `CURSOR` (`0047`..`0050`) or `SPECIAL-NAMES. CURSOR` (`0055`, `0067`).
///
/// # Arguments
///
/// * `editors` - Staged field editors.
/// * `initial_field_cursor` - 1-based inline `CURSOR` offset (`<= 0` when omitted).
/// * `cursor_reg_ptr` - Optional `SPECIAL-NAMES. CURSOR` buffer pointer.
/// * `cursor_reg_len` - Byte length of `cursor_reg_ptr`.
#[expect(unsafe_code, reason = "Reads optional `SPECIAL-NAMES. CURSOR` buffer.")]
fn resolve_initial_focus(
    editors: &[LiveFieldEditor],
    initial_field_cursor: i64,
    cursor_reg_ptr: *const u8,
    cursor_reg_len: usize,
) -> (usize, usize) {
    if initial_field_cursor > 0
        && let Some(ed) = editors.first()
    {
        let req_1based = usize::try_from(initial_field_cursor).unwrap_or(1).max(1);
        let last_non_space_1based = ed
            .chars
            .iter()
            .rposition(|&c| c != ' ')
            .map_or(0, |p| p + 1);
        let eff_1based = req_1based
            .min(last_non_space_1based.max(1))
            .min(ed.chars.len().max(1));
        return (0, eff_1based.saturating_sub(1));
    }

    if !cursor_reg_ptr.is_null() && (cursor_reg_len == 4 || cursor_reg_len == 6) {
        // SAFETY: Validated non-null `cursor_reg_ptr` of length 4 or 6.
        let buf = unsafe { core::slice::from_raw_parts(cursor_reg_ptr, cursor_reg_len) };
        let fmt = if cursor_reg_len == 6 {
            CursorFormat::SixDigitLllCcc
        } else {
            CursorFormat::FourDigitLlCc
        };
        if let Ok(Some(target_pos)) = fmt.decode(buf) {
            for (idx, ed) in editors.iter().enumerate() {
                if ed.pos.row == target_pos.row {
                    let start_c = ed.pos.col.get();
                    let end_c = start_c.saturating_add(ed.width);
                    if target_pos.col.get() >= start_c && target_pos.col.get() < end_c {
                        return (idx, usize::from(target_pos.col.get() - start_c));
                    }
                }
            }
        }
    }
    (0, 0)
}

/// Returns the screen [`CursorPosition`] of `(field_idx, char_offset)`.
///
/// # Arguments
///
/// * `editors` - Staged field editors.
/// * `field_idx` - Focused editor index.
/// * `char_offset` - Character offset within `editors[field_idx]`.
/// * `dims` - Terminal dimensions.
fn editor_cursor_pos(
    editors: &[LiveFieldEditor],
    field_idx: usize,
    char_offset: usize,
    dims: ScreenDimensions,
) -> CursorPosition {
    let Some(ed) = editors.get(field_idx) else {
        return CursorPosition::HOME;
    };
    let delta = u16::try_from(char_offset).unwrap_or(0);
    let col = ed.pos.col.advance_clamped(delta, dims.cols);
    CursorPosition::new(ed.pos.row, col)
}

/// Resolves `(CursorPosition, CellStyle, ScreenAttrFlags)` for one [`StagedScreenItem`].
///
/// # Arguments
///
/// * `item` - Staged screen item.
/// * `overrides` - Active `SET ... ATTRIBUTE` overrides.
/// * `dims` - Terminal dimensions.
/// * `cur` - Mutable cursor position.
/// * `at_origin` - Mutable flag tracking initial home position.
fn resolve_item_geometry_and_style(
    item: &StagedScreenItem,
    overrides: &BTreeMap<u32, (u32, u32)>,
    dims: ScreenDimensions,
    cur: &mut CursorPosition,
    at_origin: &mut bool,
) -> (CursorPosition, CellStyle, ScreenAttrFlags) {
    let (mut eff_line, mut eff_col) = (item.line, item.col);
    if let Some((at_v, at_d)) = item.at_compact {
        let spec_opt = if at_d == 6 {
            AtCompactSpec::from_six_digit_lllccc(at_v).ok()
        } else {
            u16::try_from(at_v)
                .ok()
                .and_then(|v| AtCompactSpec::from_four_digit_llcc(v).ok())
        };
        if let Some(at_s) = spec_opt {
            eff_line = at_s.line;
            eff_col = at_s.col;
        }
    }

    let (row, line_explicit) = match eff_line {
        LineSpec::Omitted => (cur.row, false),
        LineSpec::Zero => {
            if *at_origin {
                (ScreenRow::ONE, true)
            } else {
                (cur.row.advance_clamped(1, dims.rows), true)
            }
        }
        LineSpec::Absolute(r) => (r.clamp_max(dims.rows), true),
        LineSpec::RelativePlus(d) => (cur.row.advance_clamped(d.max(1), dims.rows), true),
        LineSpec::RelativeMinus(d) => (cur.row.retreat_clamped(d.max(1)), true),
    };
    let col = match eff_col {
        ColSpec::Omitted => {
            if line_explicit {
                ScreenCol::ONE
            } else {
                cur.col
            }
        }
        ColSpec::Zero => {
            if eff_line == LineSpec::Omitted {
                cur.col
            } else {
                ScreenCol::ONE
            }
        }
        ColSpec::Absolute(c) => c.clamp_max(dims.cols),
        ColSpec::RelativePlus(d) => {
            if line_explicit {
                ScreenCol::ONE.advance_clamped(d.saturating_sub(1), dims.cols)
            } else {
                cur.col.advance_clamped(d.saturating_sub(1), dims.cols)
            }
        }
        ColSpec::RelativeMinus(d) => cur.col.retreat_clamped(d),
    };
    *at_origin = false;
    let pos = CursorPosition::new(row, col);
    *cur = pos;

    let mut bits = item.attr_bits;
    if let Some(sid) = item.screen_id
        && let Some(&(on_b, off_b)) = overrides.get(&sid)
    {
        bits = (bits | on_b) & !off_b;
    }
    if item.fg_color >= 8 {
        bits |= ScreenAttrFlags::HIGHLIGHT.bits();
    }
    let flags = ScreenAttrFlags::from_bits_retain(bits);
    let fg = u8::try_from(item.fg_color)
        .ok()
        .and_then(|c| ScreenColor::from_extended_code(c % 16).ok())
        .map(|(col_enum, _)| col_enum);
    let bg = u8::try_from(item.bg_color)
        .ok()
        .and_then(|c| ScreenColor::from_extended_code(c % 16).ok())
        .map(|(col_enum, _)| col_enum);
    let style = CellStyle {
        fg,
        bg,
        attrs: flags,
    };
    (pos, style, flags)
}

/// Renders display bytes into an exact `width`-character `String`.
///
/// # Arguments
///
/// * `bytes` - Raw COBOL source bytes.
/// * `repeat_all` - Whether to cycle `bytes` across `width` (`ALL "..."`).
/// * `width` - Target cell width.
/// * `flags` - Active [`ScreenAttrFlags`] (`BLANK_WHEN_ZERO`, etc.).
fn render_display_text(
    bytes: &[u8],
    repeat_all: bool,
    width: usize,
    flags: ScreenAttrFlags,
) -> String {
    let w = width.max(bytes.len());
    if w == 0 {
        return String::new();
    }
    let raw: Vec<u8> = if repeat_all && !bytes.is_empty() {
        bytes.iter().copied().cycle().take(w).collect()
    } else {
        let mut v = bytes.to_vec();
        v.resize(w, b' ');
        v.truncate(w);
        v
    };
    if flags.contains(ScreenAttrFlags::BLANK_WHEN_ZERO)
        && raw
            .iter()
            .all(|&b| matches!(b, b'0' | b' ' | b'+' | b'-' | b'.'))
    {
        return " ".repeat(w);
    }
    raw.into_iter()
        .map(|b| {
            if (0x20..0x7F).contains(&b) {
                char::from(b)
            } else {
                ' '
            }
        })
        .collect()
}

/// Emits ANSI scroll escape sequence (`CSI n S` up / `CSI n T` down).
///
/// # Arguments
///
/// * `backend` - Output VT100 backend.
/// * `lines` - Signed scroll line count (`> 0` up, `< 0` down).
fn emit_scroll(backend: &mut LiveAnsiBackend<'_>, lines: i32) {
    if lines > 0 {
        let seq = format!("\x1b[{lines}S");
        backend.emit_bytes(seq.as_bytes());
    } else if lines < 0 {
        let n = lines.unsigned_abs();
        let seq = format!("\x1b[{n}T");
        backend.emit_bytes(seq.as_bytes());
    }
}

/// Writes `status` and `pos` into `SPECIAL-NAMES. CURSOR`, `CRT STATUS`, and `COB-CRT-STATUS`.
///
/// # Arguments
///
/// * `status` - Resulting [`CrtStatusCode`].
/// * `pos` - Resulting [`CursorPosition`].
/// * `cursor_reg_ptr` - Optional `SPECIAL-NAMES. CURSOR` buffer pointer.
/// * `cursor_reg_len` - Byte length of `cursor_reg_ptr`.
/// * `crt_status_ptr` - Optional `SPECIAL-NAMES. CRT STATUS` buffer pointer.
/// * `crt_status_len` - Byte length of `crt_status_ptr`.
/// * `cob_crt_ptr` - Optional 4-byte `COB-CRT-STATUS` buffer pointer.
///
/// # Safety
///
/// Non-null pointers must be writable for their declared byte lengths.
#[expect(
    unsafe_code,
    reason = "Writes screen special registers into caller-owned COBOL storage."
)]
unsafe fn write_special_registers(
    status: CrtStatusCode,
    pos: CursorPosition,
    cursor_reg_ptr: *mut u8,
    cursor_reg_len: usize,
    crt_status_ptr: *mut u8,
    crt_status_len: usize,
    cob_crt_ptr: *mut u8,
) {
    if !cursor_reg_ptr.is_null() && (cursor_reg_len == 4 || cursor_reg_len == 6) {
        // SAFETY: Validated non-null `cursor_reg_ptr` with length 4 or 6.
        let slice = unsafe { core::slice::from_raw_parts_mut(cursor_reg_ptr, cursor_reg_len) };
        let fmt = if cursor_reg_len == 6 {
            CursorFormat::SixDigitLllCcc
        } else {
            CursorFormat::FourDigitLlCc
        };
        let _ = fmt.encode(pos, slice);
    }
    if !crt_status_ptr.is_null() && (crt_status_len == 3 || crt_status_len == 4) {
        // SAFETY: Validated non-null `crt_status_ptr` with length 3 or 4.
        let slice = unsafe { core::slice::from_raw_parts_mut(crt_status_ptr, crt_status_len) };
        let _ = status.write_to_register(slice);
    }
    if !cob_crt_ptr.is_null() {
        // SAFETY: `COB-CRT-STATUS` is always a 4-byte `PIC 9(4)` special register.
        let slice = unsafe { core::slice::from_raw_parts_mut(cob_crt_ptr, 4) };
        let _ = status.write_to_register(slice);
    }
}

// ============================================================================
// Built-in `CBL_GC_SET_SCR_SIZE`, `CBL_GET_CSR_POS`, & `CBL_GC_WINDOW` Subroutines
// ============================================================================

/// Resizes virtual screen dimensions (`CALL "CBL_GC_SET_SCR_SIZE" USING BY VALUE lines BY VALUE cols`).
///
/// # Arguments
///
/// * `lines` - Desired row count (`1..=999`).
/// * `cols` - Desired column count (`1..=999`).
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_GC_SET_SCR_SIZE` C-ABI symbol."
)]
#[unsafe(no_mangle)]
pub extern "C" fn CBL_GC_SET_SCR_SIZE(lines: i32, cols: i32) -> i32 {
    let r = u16::try_from(lines).unwrap_or(24).clamp(1, 999);
    let c = u16::try_from(cols).unwrap_or(80).clamp(1, 999);
    if let (Ok(sr), Ok(sc)) = (ScreenRow::try_new(r), ScreenCol::try_new(c)) {
        with_screen_state_mut(|st| st.dims = ScreenDimensions::new(sr, sc));
        0
    } else {
        -1
    }
}

/// Writes 0-based `(row_u8, col_u8)` cursor coordinates into `buf_ptr` (`CALL "CBL_GET_CSR_POS" USING buf`).
///
/// # Arguments
///
/// * `buf_ptr` - 2-byte `(row, col)` output buffer.
///
/// # Safety
///
/// Non-null `buf_ptr` must reference at least 2 writable bytes.
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_GET_CSR_POS` C-ABI symbol."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn CBL_GET_CSR_POS(buf_ptr: *mut u8) -> i32 {
    if buf_ptr.is_null() {
        return -1;
    }
    let pos = with_screen_state(|st| st.cursor);
    // SAFETY: Caller passes at least 2-byte `CURSOR-LOCN-BUFFER`.
    let out = unsafe { core::slice::from_raw_parts_mut(buf_ptr, 2) };
    if let [r_slot, c_slot] = out {
        *r_slot = u8::try_from(pos.row.zero_based_index()).unwrap_or(0);
        *c_slot = u8::try_from(pos.col.zero_based_index()).unwrap_or(0);
    }
    0
}

/// Populates the 360-byte `WL-WINDOW-LIST` output buffer for `CBL_GC_WINDOW` mode `"08"`.
///
/// # Arguments
///
/// * `windows` - Active managed windows in z-order (bottom to top).
/// * `list_buf` - 360-byte output buffer (`20 * 9 * 2` bytes).
fn write_gc_window_list(windows: &[ManagedGcWindow], list_buf: &mut [u8]) {
    list_buf.fill(0);
    for (depth_0, win) in windows.iter().rev().take(20).enumerate() {
        let base = depth_0 * 18;
        let depth_i16 = i16::try_from(depth_0 + 1).unwrap_or(1);
        let fields = [
            depth_i16,
            win.number,
            win.start_line,
            win.start_col,
            win.lines,
            win.cols,
            win.fg_color,
            win.bg_color,
            win.flags,
        ];
        for (fi, val) in fields.into_iter().enumerate() {
            let off = base + fi * 2;
            if let Some(dst) = list_buf.get_mut(off..off + 2) {
                dst.copy_from_slice(&val.to_ne_bytes());
            }
        }
    }
}

/// Executes modes `"01"`..`"07"` on `WP-WINDOW-PARMS` (`16` bytes) for [`CBL_GC_WINDOW`].
///
/// # Arguments
///
/// * `st` - Mutable [`ScreenRuntimeState`].
/// * `mode` - Two-character mode code (`"01"`..`"07"`).
/// * `parm_buf` - Mutable 16-byte `WP-WINDOW-PARMS` slice.
fn apply_gc_window_parm_op(st: &mut ScreenRuntimeState, mode: &str, parm_buf: &mut [u8]) -> i32 {
    let read_i16 = |buf: &[u8], idx: usize| -> i16 {
        let off = idx * 2;
        buf.get(off..off + 2)
            .and_then(|s| <[u8; 2]>::try_from(s).ok())
            .map_or(0, i16::from_ne_bytes)
    };
    let win_num = read_i16(parm_buf, 0);
    let s_line = read_i16(parm_buf, 1);
    let s_col = read_i16(parm_buf, 2);
    let w_lines = read_i16(parm_buf, 3);
    let w_cols = read_i16(parm_buf, 4);
    let fg = read_i16(parm_buf, 5);
    let bg = read_i16(parm_buf, 6);
    let fl = read_i16(parm_buf, 7);

    match mode {
        "01" => {
            let assigned = st.next_win_num;
            st.next_win_num = st.next_win_num.saturating_add(1);
            st.windows.push(ManagedGcWindow {
                number: assigned,
                start_line: s_line.max(1),
                start_col: s_col.max(1),
                lines: w_lines.max(1),
                cols: w_cols.max(1),
                fg_color: fg,
                bg_color: bg,
                flags: fl,
            });
            if let Some(num_slot) = parm_buf.get_mut(0..2) {
                num_slot.copy_from_slice(&assigned.to_ne_bytes());
            }
            let r = u16::try_from(s_line.max(1)).unwrap_or(1);
            let c = u16::try_from(s_col.max(1)).unwrap_or(1);
            if let (Ok(sr), Ok(sc)) = (ScreenRow::try_new(r), ScreenCol::try_new(c)) {
                st.cursor = CursorPosition::new(sr, sc);
            }
            0
        }
        "02" => st
            .windows
            .iter()
            .position(|w| w.number == win_num)
            .map_or(1, |pos| {
                let _ = st.windows.remove(pos);
                0
            }),
        "03" => st
            .windows
            .iter_mut()
            .find(|w| w.number == win_num)
            .map_or(1, |w| {
                w.flags = 1;
                0
            }),
        "04" => st
            .windows
            .iter_mut()
            .find(|w| w.number == win_num)
            .map_or(1, |w| {
                w.flags = 0;
                0
            }),
        "05" => st
            .windows
            .iter()
            .position(|w| w.number == win_num)
            .map_or(1, |pos| {
                let w = st.windows.remove(pos);
                st.windows.push(w);
                0
            }),
        "06" => st
            .windows
            .iter()
            .position(|w| w.number == win_num)
            .map_or(1, |pos| {
                let w = st.windows.remove(pos);
                st.windows.insert(0, w);
                0
            }),
        "07" => st
            .windows
            .iter_mut()
            .find(|w| w.number == win_num)
            .map_or(1, |w| {
                w.start_line = s_line.max(1);
                w.start_col = s_col.max(1);
                0
            }),
        _ => 1,
    }
}

/// Executes clean-room window management (`CALL "CBL_GC_WINDOW" USING mode_ptr parms_or_list_ptr`).
///
/// # Arguments
///
/// * `mode_ptr` - 2-byte ASCII operation code (`"01"` new, `"02"` delete, `"03"` hide, `"04"` show, `"05"` top, `"06"` bottom, `"07"` move, `"08"` list).
/// * `parms_ptr` - Pointer to `WP-WINDOW-PARMS` (16 bytes: 8 `i16`s) or `WL-WINDOW-LIST` (`20 * 18 = 360` bytes).
///
/// # Safety
///
/// Non-null `mode_ptr` and `parms_ptr` must point to valid `gcwindow` COBOL structures.
#[expect(
    unsafe_code,
    reason = "Exported clean-room `CBL_GC_WINDOW` C-ABI symbol."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn CBL_GC_WINDOW(mode_ptr: *const u8, parms_ptr: *mut u8) -> i32 {
    if mode_ptr.is_null() || parms_ptr.is_null() {
        return -1;
    }
    // SAFETY: `WP-WINDOW-MODE` is `PIC X(02)`.
    let mode_bytes = unsafe { core::slice::from_raw_parts(mode_ptr, 2) };
    let mode = core::str::from_utf8(mode_bytes).unwrap_or("00");

    with_screen_state_mut(|st| {
        if mode == "08" {
            // SAFETY: `WL-WINDOW-LIST` is `20 * 9 * 2 = 360` bytes.
            let list_buf = unsafe { core::slice::from_raw_parts_mut(parms_ptr, 360) };
            write_gc_window_list(&st.windows, list_buf);
            return 0;
        }
        // SAFETY: `WP-WINDOW-PARMS` is `8 * 2 = 16` bytes (`8` native `COMP-5` `i16`s).
        let parm_buf = unsafe { core::slice::from_raw_parts_mut(parms_ptr, 16) };
        apply_gc_window_parm_op(st, mode, parm_buf)
    })
}
