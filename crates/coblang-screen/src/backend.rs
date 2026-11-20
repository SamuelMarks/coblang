//! Safe `TerminalBackend` trait, deterministic `VirtualGridBackend` (`24x80`), and `AnsiVt100Backend` TTY/PTY driver.

use std::collections::VecDeque;
use std::io::{Cursor, Read, Write};

use coblang_common::{CoblangError, Result};

use crate::types::{
    CellStyle, CursorPosition, CursorVisibility, KeyEvent, ScreenAttrFlags, ScreenCol,
    ScreenDimensions, ScreenRow,
};

/// Single rendered character cell inside a [`VirtualGridBackend`] frame buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenCell {
    /// Visible character stored in this grid cell (`' '` when blank).
    pub ch: char,
    /// Resolved visual color and attribute style of this cell.
    pub style: CellStyle,
}

impl Default for ScreenCell {
    /// Returns a blank ASCII space cell with [`CellStyle::DEFAULT`].
    fn default() -> Self {
        Self::BLANK
    }
}

impl ScreenCell {
    /// Blank default cell (`' '` with [`CellStyle::DEFAULT`]).
    pub const BLANK: Self = Self {
        ch: ' ',
        style: CellStyle::DEFAULT,
    };

    /// Constructs a [`ScreenCell`] containing `ch` styled with `style`.
    ///
    /// # Arguments
    ///
    /// * `ch` - Displayed character.
    /// * `style` - Cell color and attribute style.
    #[must_use]
    pub const fn new(ch: char, style: CellStyle) -> Self {
        Self { ch, style }
    }
}

/// Maps DEC Special Graphics line-drawing codes (`q`, `x`, `l`, `k`, `m`, `j`, `n`, `t`, `u`, `v`, `w`)
/// to Unicode box-drawing characters when [`ScreenAttrFlags::GRAPHICS`] is active.
///
/// # Arguments
///
/// * `ch` - Input character.
/// * `attrs` - Active [`ScreenAttrFlags`].
#[must_use]
pub const fn translate_cell_char(ch: char, attrs: ScreenAttrFlags) -> char {
    let clean = if (ch as u32) < 0x20 || ch == '\u{007F}' {
        ' '
    } else {
        ch
    };
    if attrs.contains(ScreenAttrFlags::GRAPHICS) {
        match clean {
            'q' => '─',
            'x' => '│',
            'l' => '┌',
            'k' => '┐',
            'm' => '└',
            'j' => '┘',
            'n' => '┼',
            't' => '├',
            'u' => '┤',
            'v' => '┴',
            'w' => '┬',
            other => other,
        }
    } else {
        clean
    }
}

/// Safe terminal rendering and keystroke input abstraction shared by [`AnsiVt100Backend`] and [`VirtualGridBackend`].
pub trait TerminalBackend {
    /// Returns the active `(rows, cols)` viewport geometry.
    fn dimensions(&self) -> ScreenDimensions;

    /// Returns the current 1-based `(row, col)` cursor position.
    fn cursor(&self) -> CursorPosition;

    /// Returns the current [`CursorVisibility`] state.
    fn cursor_visibility(&self) -> CursorVisibility;

    /// Moves the terminal cursor to `pos` (clamped to [`Self::dimensions`]).
    ///
    /// # Arguments
    ///
    /// * `pos` - Target 1-based [`CursorPosition`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] or [`CoblangError::ScreenBackend`] if terminal output fails.
    fn set_cursor(&mut self, pos: CursorPosition) -> Result<()>;

    /// Updates terminal cursor visibility (`Hidden`, `Normal`, `High`).
    ///
    /// # Arguments
    ///
    /// * `visibility` - Desired [`CursorVisibility`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] or [`CoblangError::ScreenBackend`] if terminal output fails.
    fn set_cursor_visibility(&mut self, visibility: CursorVisibility) -> Result<()>;

    /// Clears the entire screen using `style` and moves the cursor to `(1, 1)` (`BLANK SCREEN`).
    ///
    /// # Arguments
    ///
    /// * `style` - Fill [`CellStyle`] applied to cleared cells.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] or [`CoblangError::ScreenBackend`] if terminal output fails.
    fn clear_screen(&mut self, style: CellStyle) -> Result<()>;

    /// Clears all columns of `row` using `style` and positions the cursor at `(row, 1)` (`BLANK LINE`).
    ///
    /// # Arguments
    ///
    /// * `row` - Target 1-based [`ScreenRow`].
    /// * `style` - Fill [`CellStyle`] applied to cleared cells on `row`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] or [`CoblangError::ScreenBackend`] if terminal output fails.
    fn clear_line(&mut self, row: ScreenRow, style: CellStyle) -> Result<()>;

    /// Clears from `pos` through the end of `pos.row` using `style` (`ERASE EOL`).
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting 1-based [`CursorPosition`].
    /// * `style` - Fill [`CellStyle`] applied to erased cells.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] or [`CoblangError::ScreenBackend`] if terminal output fails.
    fn erase_to_end_of_line(&mut self, pos: CursorPosition, style: CellStyle) -> Result<()>;

    /// Clears from `pos` through the bottom-right corner of the screen using `style` (`ERASE EOS`).
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting 1-based [`CursorPosition`].
    /// * `style` - Fill [`CellStyle`] applied to erased cells.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] or [`CoblangError::ScreenBackend`] if terminal output fails.
    fn erase_to_end_of_screen(&mut self, pos: CursorPosition, style: CellStyle) -> Result<()>;

    /// Emits an audible or visual terminal bell (`BELL` / `BEEP`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] or [`CoblangError::ScreenBackend`] if terminal output fails.
    fn ring_bell(&mut self) -> Result<()>;

    /// Writes `text` starting at `pos` with `style`, clipping at the right margin of `pos.row`,
    /// and updates the cursor to the cell immediately following the written span (clamped to `cols`).
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting 1-based [`CursorPosition`].
    /// * `text` - Character sequence to render on `pos.row`.
    /// * `style` - [`CellStyle`] applied to every written cell.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] or [`CoblangError::ScreenBackend`] if terminal output fails.
    fn write_span(
        &mut self,
        pos: CursorPosition,
        text: &str,
        style: CellStyle,
    ) -> Result<CursorPosition>;

    /// Flushes any buffered terminal output bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] or [`CoblangError::ScreenBackend`] if flushing fails.
    fn flush(&mut self) -> Result<()>;

    /// Reads the next structured [`KeyEvent`] from the terminal or scripted input queue.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] or [`CoblangError::ScreenBackend`] if terminal input fails.
    fn read_key(&mut self) -> Result<KeyEvent>;
}

/// Deterministic in-memory `24x80` (or custom `rows x cols`) terminal frame buffer and keystroke queue.
#[derive(Debug, Clone)]
pub struct VirtualGridBackend {
    /// Grid dimensions (`24x80` by default).
    dims: ScreenDimensions,
    /// Row-major cell matrix of length `dims.cell_count()`.
    cells: Vec<ScreenCell>,
    /// Current 1-based cursor position.
    cursor: CursorPosition,
    /// Current cursor visibility mode.
    visibility: CursorVisibility,
    /// Total number of `BELL` / `BEEP` alerts emitted.
    bell_count: u32,
    /// Total number of `flush()` invocations performed.
    flush_count: u32,
    /// Scripted FIFO queue of upcoming [`KeyEvent`] inputs.
    keys: VecDeque<KeyEvent>,
}

impl Default for VirtualGridBackend {
    /// Constructs a blank `24x80` [`VirtualGridBackend`].
    fn default() -> Self {
        Self::new_24x80()
    }
}

impl VirtualGridBackend {
    /// Constructs a blank [`VirtualGridBackend`] with `dims`.
    ///
    /// # Arguments
    ///
    /// * `dims` - Grid geometry (`rows` x `cols`).
    #[must_use]
    pub fn new(dims: ScreenDimensions) -> Self {
        Self {
            dims,
            cells: vec![ScreenCell::BLANK; dims.cell_count()],
            cursor: CursorPosition::HOME,
            visibility: CursorVisibility::Normal,
            bell_count: 0,
            flush_count: 0,
            keys: VecDeque::new(),
        }
    }

    /// Constructs a standard `24x80` [`VirtualGridBackend`].
    #[must_use]
    pub fn new_24x80() -> Self {
        Self::new(ScreenDimensions::STANDARD_24X80)
    }

    /// Constructs a [`VirtualGridBackend`] honoring `COB_SCREEN_LINES`/`LINES` and `COB_SCREEN_COLS`/`COLUMNS`.
    ///
    /// # Arguments
    ///
    /// * `env_lookup` - Environment variable lookup function.
    #[must_use]
    pub fn from_env_lookup(env_lookup: &mut dyn FnMut(&str) -> Option<String>) -> Self {
        Self::new(ScreenDimensions::from_env_lookup(env_lookup))
    }

    /// Enqueues a single [`KeyEvent`] at the back of the input queue.
    ///
    /// # Arguments
    ///
    /// * `event` - Keystroke event to enqueue.
    pub fn push_key(&mut self, event: KeyEvent) {
        self.keys.push_back(event);
    }

    /// Enqueues a slice of [`KeyEvent`] items in order.
    ///
    /// # Arguments
    ///
    /// * `events` - Ordered keystroke events to enqueue.
    pub fn push_keys(&mut self, events: &[KeyEvent]) {
        for &ev in events {
            self.keys.push_back(ev);
        }
    }

    /// Enqueues every character of `text` as [`KeyEvent::Char`] followed by [`KeyEvent::Enter`].
    ///
    /// # Arguments
    ///
    /// * `text` - Input text typed prior to pressing Enter.
    pub fn push_text_with_enter(&mut self, text: &str) {
        for ch in text.chars() {
            self.keys.push_back(KeyEvent::Char(ch));
        }
        self.keys.push_back(KeyEvent::Enter);
    }

    /// Decodes VT100/ANSI keystrokes from `reader` until `EOF` and appends them to the input queue.
    ///
    /// # Arguments
    ///
    /// * `reader` - Input byte stream yielding ANSI keystrokes.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] if reading from `reader` fails.
    pub fn read_ansi_keys(&mut self, reader: &mut dyn Read) -> Result<()> {
        let mut lookahead = VecDeque::new();
        loop {
            let ev = decode_next_key_event(reader, &mut lookahead)?;
            if ev == KeyEvent::Eof {
                break;
            }
            self.keys.push_back(ev);
        }
        Ok(())
    }

    /// Decodes a raw VT100/ANSI byte slice into [`KeyEvent`] items and appends them to the input queue.
    ///
    /// # Arguments
    ///
    /// * `raw_bytes` - Raw terminal/PTY input byte sequence.
    pub fn push_ansi_bytes(&mut self, raw_bytes: &[u8]) {
        let mut cursor = Cursor::new(raw_bytes);
        let _ = self.read_ansi_keys(&mut cursor);
    }

    /// Returns the [`ScreenCell`] at 1-based `pos`.
    ///
    /// # Arguments
    ///
    /// * `pos` - One-based `(row, col)` cell coordinate.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ScreenBackend`] if `pos` lies outside [`Self::dimensions`].
    pub fn cell_at(&self, pos: CursorPosition) -> Result<ScreenCell> {
        if pos.row.get() > self.dims.rows.get() || pos.col.get() > self.dims.cols.get() {
            return Err(CoblangError::ScreenBackend {
                message: format!(
                    "cell coordinate ({}, {}) exceeds grid ({}, {})",
                    pos.row.get(),
                    pos.col.get(),
                    self.dims.rows.get(),
                    self.dims.cols.get()
                ),
            });
        }
        let cols_usize = self.dims.cols.get() as usize;
        let idx = pos
            .row
            .zero_based_index()
            .saturating_mul(cols_usize)
            .saturating_add(pos.col.zero_based_index());
        Ok(self.cells.get(idx).copied().unwrap_or(ScreenCell::BLANK))
    }

    /// Returns the full fixed-width string of `row` (`cols` characters).
    ///
    /// # Arguments
    ///
    /// * `row` - One-based [`ScreenRow`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ScreenBackend`] if `row` exceeds `self.dims.rows`.
    pub fn row_string(&self, row: ScreenRow) -> Result<String> {
        if row.get() > self.dims.rows.get() {
            return Err(CoblangError::ScreenBackend {
                message: format!(
                    "row {} exceeds grid height {}",
                    row.get(),
                    self.dims.rows.get()
                ),
            });
        }
        let cols_usize = self.dims.cols.get() as usize;
        let row_start = row.zero_based_index().saturating_mul(cols_usize);
        let mut out = String::with_capacity(cols_usize);
        for cell in self.cells.iter().skip(row_start).take(cols_usize) {
            out.push(cell.ch);
        }
        Ok(out)
    }

    /// Returns `row` with trailing ASCII spaces removed.
    ///
    /// # Arguments
    ///
    /// * `row` - One-based [`ScreenRow`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ScreenBackend`] if `row` exceeds `self.dims.rows`.
    pub fn row_trimmed_end(&self, row: ScreenRow) -> Result<String> {
        let full = self.row_string(row)?;
        Ok(String::from(full.trim_end_matches(' ')))
    }

    /// Renders the entire `rows x cols` grid as `rows` newline-separated lines.
    #[must_use]
    pub fn snapshot(&self) -> String {
        let cols_usize = self.dims.cols.get() as usize;
        let rows_usize = self.dims.rows.get() as usize;
        let mut out = String::with_capacity(self.dims.cell_count().saturating_add(rows_usize));
        for (r_idx, row_cells) in self.cells.chunks(cols_usize).enumerate() {
            if r_idx > 0 {
                out.push(char::from(0x0A));
            }
            for cell in row_cells {
                out.push(cell.ch);
            }
        }
        out
    }

    /// Returns the number of times `ring_bell()` has been called.
    #[must_use]
    pub const fn bell_count(&self) -> u32 {
        self.bell_count
    }

    /// Returns the number of times `flush()` has been called.
    #[must_use]
    pub const fn flush_count(&self) -> u32 {
        self.flush_count
    }

    /// Returns the number of queued [`KeyEvent`] items remaining.
    #[must_use]
    pub fn remaining_keys(&self) -> usize {
        self.keys.len()
    }
}

impl TerminalBackend for VirtualGridBackend {
    /// Returns the grid's configured [`ScreenDimensions`].
    fn dimensions(&self) -> ScreenDimensions {
        self.dims
    }

    /// Returns the current 1-based [`CursorPosition`].
    fn cursor(&self) -> CursorPosition {
        self.cursor
    }

    /// Returns the current [`CursorVisibility`] state.
    fn cursor_visibility(&self) -> CursorVisibility {
        self.visibility
    }

    /// Clamps and updates the cursor to `pos`.
    ///
    /// # Arguments
    ///
    /// * `pos` - Target 1-based [`CursorPosition`].
    fn set_cursor(&mut self, pos: CursorPosition) -> Result<()> {
        self.cursor = pos.clamp_to(self.dims);
        Ok(())
    }

    /// Updates cursor visibility to `visibility`.
    ///
    /// # Arguments
    ///
    /// * `visibility` - Target [`CursorVisibility`].
    fn set_cursor_visibility(&mut self, visibility: CursorVisibility) -> Result<()> {
        self.visibility = visibility;
        Ok(())
    }

    /// Fills all cells with `' '` in `style` and moves cursor to `(1, 1)`.
    ///
    /// # Arguments
    ///
    /// * `style` - Fill [`CellStyle`].
    fn clear_screen(&mut self, style: CellStyle) -> Result<()> {
        self.cells.fill(ScreenCell::new(' ', style));
        self.cursor = CursorPosition::HOME;
        Ok(())
    }

    /// Fills `row` with `' '` in `style` and moves cursor to `(row, 1)`.
    ///
    /// # Arguments
    ///
    /// * `row` - Target 1-based [`ScreenRow`].
    /// * `style` - Fill [`CellStyle`].
    fn clear_line(&mut self, row: ScreenRow, style: CellStyle) -> Result<()> {
        let clamped_row = row.clamp_max(self.dims.rows);
        let cols_usize = self.dims.cols.get() as usize;
        let start = clamped_row.zero_based_index().saturating_mul(cols_usize);
        let blank = ScreenCell::new(' ', style);
        for slot in self.cells.iter_mut().skip(start).take(cols_usize) {
            *slot = blank;
        }
        self.cursor = CursorPosition::new(clamped_row, ScreenCol::ONE);
        Ok(())
    }

    /// Clears from `pos` through the end of `pos.row` using `style`.
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting 1-based [`CursorPosition`].
    /// * `style` - Fill [`CellStyle`].
    fn erase_to_end_of_line(&mut self, pos: CursorPosition, style: CellStyle) -> Result<()> {
        let clamped = pos.clamp_to(self.dims);
        let cols_usize = self.dims.cols.get() as usize;
        let row_start = clamped.row.zero_based_index().saturating_mul(cols_usize);
        let col_idx = clamped.col.zero_based_index();
        let count = cols_usize.saturating_sub(col_idx);
        let blank = ScreenCell::new(' ', style);
        for slot in self
            .cells
            .iter_mut()
            .skip(row_start.saturating_add(col_idx))
            .take(count)
        {
            *slot = blank;
        }
        self.cursor = clamped;
        Ok(())
    }

    /// Clears from `pos` through the end of the virtual screen using `style`.
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting 1-based [`CursorPosition`].
    /// * `style` - Fill [`CellStyle`].
    fn erase_to_end_of_screen(&mut self, pos: CursorPosition, style: CellStyle) -> Result<()> {
        let clamped = pos.clamp_to(self.dims);
        let cols_usize = self.dims.cols.get() as usize;
        let start = clamped
            .row
            .zero_based_index()
            .saturating_mul(cols_usize)
            .saturating_add(clamped.col.zero_based_index());
        let blank = ScreenCell::new(' ', style);
        for slot in self.cells.iter_mut().skip(start) {
            *slot = blank;
        }
        self.cursor = clamped;
        Ok(())
    }

    /// Increments the recorded audible bell alert counter.
    fn ring_bell(&mut self) -> Result<()> {
        self.bell_count = self.bell_count.saturating_add(1);
        Ok(())
    }

    /// Writes `text` onto `pos.row` starting at `pos.col` styled with `style`.
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting 1-based [`CursorPosition`].
    /// * `text` - Characters to render.
    /// * `style` - Cell [`CellStyle`].
    fn write_span(
        &mut self,
        pos: CursorPosition,
        text: &str,
        style: CellStyle,
    ) -> Result<CursorPosition> {
        let clamped = pos.clamp_to(self.dims);
        let cols_usize = self.dims.cols.get() as usize;
        let row_start = clamped.row.zero_based_index().saturating_mul(cols_usize);
        let start_col_idx = clamped.col.zero_based_index();
        let avail = cols_usize.saturating_sub(start_col_idx);
        let mut written = 0_usize;
        for (slot, raw_ch) in self
            .cells
            .iter_mut()
            .skip(row_start.saturating_add(start_col_idx))
            .take(avail)
            .zip(text.chars())
        {
            let rendered = translate_cell_char(raw_ch, style.attrs);
            *slot = ScreenCell::new(rendered, style);
            written = written.saturating_add(1);
        }
        let advance = u16::try_from(written).unwrap_or(self.dims.cols.get());
        let next_col = clamped.col.advance_clamped(advance, self.dims.cols);
        let next_pos = CursorPosition::new(clamped.row, next_col);
        self.cursor = next_pos;
        Ok(next_pos)
    }

    /// Increments the recorded flush counter.
    fn flush(&mut self) -> Result<()> {
        self.flush_count = self.flush_count.saturating_add(1);
        Ok(())
    }

    /// Pops the next queued [`KeyEvent`], returning [`KeyEvent::Eof`] when empty.
    fn read_key(&mut self) -> Result<KeyEvent> {
        Ok(self.keys.pop_front().unwrap_or(KeyEvent::Eof))
    }
}

/// Formats an ECMA-48 / VT100 SGR escape sequence (`\x1b[0...m`) representing `style`.
///
/// # Arguments
///
/// * `style` - Visual [`CellStyle`] to encode.
#[must_use]
pub fn format_ansi_sgr(style: CellStyle) -> String {
    let mut seq = String::with_capacity(24);
    seq.push_str("\x1b[0");
    if style.attrs.contains(ScreenAttrFlags::HIGHLIGHT) {
        seq.push_str(";1");
    }
    if style.attrs.contains(ScreenAttrFlags::LOWLIGHT) {
        seq.push_str(";2");
    }
    if style.attrs.contains(ScreenAttrFlags::UNDERLINE) {
        seq.push_str(";4");
    }
    if style.attrs.contains(ScreenAttrFlags::BLINK) {
        seq.push_str(";5");
    }
    if style.attrs.contains(ScreenAttrFlags::REVERSE_VIDEO) {
        seq.push_str(";7");
    }
    if style.attrs.contains(ScreenAttrFlags::OVERLINE) {
        seq.push_str(";53");
    }
    if let Some(fg) = style.fg {
        seq.push(';');
        seq.push_str(&fg.ansi_fg_sgr().to_string());
    }
    if let Some(bg) = style.bg {
        seq.push(';');
        seq.push_str(&bg.ansi_bg_sgr().to_string());
    }
    seq.push('m');
    seq
}

/// Interactive VT100 / ECMA-48 ANSI terminal and PTY backend operating over safe `Read` and `Write` streams.
pub struct AnsiVt100Backend {
    /// Terminal viewport dimensions.
    dims: ScreenDimensions,
    /// Current 1-based cursor coordinate.
    cursor: CursorPosition,
    /// Current cursor visibility setting.
    visibility: CursorVisibility,
    /// Input byte stream (TTY, PTY slave, or piped stdin).
    reader: Box<dyn Read>,
    /// Output byte stream (TTY, PTY slave, or captured buffer).
    writer: Box<dyn Write>,
    /// Single-byte/escape lookahead buffer for CRLF and escape-sequence disambiguation.
    lookahead: VecDeque<u8>,
}

impl AnsiVt100Backend {
    /// Constructs an [`AnsiVt100Backend`] over arbitrary `Read` and `Write` streams.
    ///
    /// # Arguments
    ///
    /// * `dims` - Terminal viewport dimensions (`24x80` or custom).
    /// * `reader` - Input byte stream.
    /// * `writer` - Output byte stream.
    #[must_use]
    pub fn new(dims: ScreenDimensions, reader: Box<dyn Read>, writer: Box<dyn Write>) -> Self {
        Self {
            dims,
            cursor: CursorPosition::HOME,
            visibility: CursorVisibility::Normal,
            reader,
            writer,
            lookahead: VecDeque::new(),
        }
    }

    /// Constructs an [`AnsiVt100Backend`] bound to standard input (`std::io::stdin()`) and standard output (`std::io::stdout()`).
    ///
    /// # Arguments
    ///
    /// * `dims` - Terminal viewport dimensions.
    #[must_use]
    pub fn stdio(dims: ScreenDimensions) -> Self {
        Self::new(
            dims,
            Box::new(std::io::stdin()),
            Box::new(std::io::stdout()),
        )
    }

    /// Consumes `self` and returns the underlying `(reader, writer)` streams.
    #[must_use]
    pub fn into_streams(self) -> (Box<dyn Read>, Box<dyn Write>) {
        (self.reader, self.writer)
    }

    /// Writes raw bytes to `self.writer`.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Raw VT100/UTF-8 bytes to emit.
    fn write_raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes).map_err(CoblangError::from)
    }
}

impl TerminalBackend for AnsiVt100Backend {
    /// Returns the configured terminal [`ScreenDimensions`].
    fn dimensions(&self) -> ScreenDimensions {
        self.dims
    }

    /// Returns the current 1-based [`CursorPosition`].
    fn cursor(&self) -> CursorPosition {
        self.cursor
    }

    /// Returns the active [`CursorVisibility`] state.
    fn cursor_visibility(&self) -> CursorVisibility {
        self.visibility
    }

    /// Emits `CSI r ; c H` to move the VT100 cursor to `pos`.
    ///
    /// # Arguments
    ///
    /// * `pos` - Target 1-based [`CursorPosition`].
    fn set_cursor(&mut self, pos: CursorPosition) -> Result<()> {
        let clamped = pos.clamp_to(self.dims);
        let seq = format!("\x1b[{};{}H", clamped.row.get(), clamped.col.get());
        self.write_raw(seq.as_bytes())?;
        self.cursor = clamped;
        Ok(())
    }

    /// Emits DECTCEM (`CSI ? 25 l` / `CSI ? 25 h`) cursor visibility sequences.
    ///
    /// # Arguments
    ///
    /// * `visibility` - Target [`CursorVisibility`].
    fn set_cursor_visibility(&mut self, visibility: CursorVisibility) -> Result<()> {
        let seq = match visibility {
            CursorVisibility::Hidden => "\x1b[?25l",
            CursorVisibility::Normal => "\x1b[?25h\x1b[0 q",
            CursorVisibility::High => "\x1b[?25h\x1b[1 q",
        };
        self.write_raw(seq.as_bytes())?;
        self.visibility = visibility;
        Ok(())
    }

    /// Emits SGR `style` followed by `CSI 2 J CSI 1 ; 1 H`.
    ///
    /// # Arguments
    ///
    /// * `style` - Fill [`CellStyle`].
    fn clear_screen(&mut self, style: CellStyle) -> Result<()> {
        let sgr = format_ansi_sgr(style);
        let seq = format!("{sgr}\x1b[2J\x1b[1;1H");
        self.write_raw(seq.as_bytes())?;
        self.cursor = CursorPosition::HOME;
        Ok(())
    }

    /// Emits SGR `style` followed by `CSI r ; 1 H CSI 2 K`.
    ///
    /// # Arguments
    ///
    /// * `row` - Target 1-based [`ScreenRow`].
    /// * `style` - Fill [`CellStyle`].
    fn clear_line(&mut self, row: ScreenRow, style: CellStyle) -> Result<()> {
        let clamped_row = row.clamp_max(self.dims.rows);
        let sgr = format_ansi_sgr(style);
        let seq = format!("{sgr}\x1b[{};1H\x1b[2K", clamped_row.get());
        self.write_raw(seq.as_bytes())?;
        self.cursor = CursorPosition::new(clamped_row, ScreenCol::ONE);
        Ok(())
    }

    /// Emits SGR `style` followed by `CSI r ; c H CSI 0 K`.
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting 1-based [`CursorPosition`].
    /// * `style` - Fill [`CellStyle`].
    fn erase_to_end_of_line(&mut self, pos: CursorPosition, style: CellStyle) -> Result<()> {
        let clamped = pos.clamp_to(self.dims);
        let sgr = format_ansi_sgr(style);
        let seq = format!(
            "{sgr}\x1b[{};{}H\x1b[0K",
            clamped.row.get(),
            clamped.col.get()
        );
        self.write_raw(seq.as_bytes())?;
        self.cursor = clamped;
        Ok(())
    }

    /// Emits SGR `style` followed by `CSI r ; c H CSI 0 J`.
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting 1-based [`CursorPosition`].
    /// * `style` - Fill [`CellStyle`].
    fn erase_to_end_of_screen(&mut self, pos: CursorPosition, style: CellStyle) -> Result<()> {
        let clamped = pos.clamp_to(self.dims);
        let sgr = format_ansi_sgr(style);
        let seq = format!(
            "{sgr}\x1b[{};{}H\x1b[0J",
            clamped.row.get(),
            clamped.col.get()
        );
        self.write_raw(seq.as_bytes())?;
        self.cursor = clamped;
        Ok(())
    }

    /// Emits ASCII `BEL` (`0x07`).
    fn ring_bell(&mut self) -> Result<()> {
        self.write_raw(&[0x07])
    }

    /// Positions cursor at `pos`, emits SGR `style`, writes clipped `text`, and resets SGR.
    ///
    /// # Arguments
    ///
    /// * `pos` - Starting 1-based [`CursorPosition`].
    /// * `text` - Characters to emit.
    /// * `style` - Cell [`CellStyle`].
    fn write_span(
        &mut self,
        pos: CursorPosition,
        text: &str,
        style: CellStyle,
    ) -> Result<CursorPosition> {
        let clamped = pos.clamp_to(self.dims);
        let cols_usize = self.dims.cols.get() as usize;
        let avail = cols_usize.saturating_sub(clamped.col.zero_based_index());
        let sgr = format_ansi_sgr(style);
        let mut out = format!("\x1b[{};{}H{sgr}", clamped.row.get(), clamped.col.get());
        let mut written = 0_usize;
        for raw_ch in text.chars().take(avail) {
            out.push(translate_cell_char(raw_ch, style.attrs));
            written = written.saturating_add(1);
        }
        out.push_str("\x1b[0m");
        self.write_raw(out.as_bytes())?;
        let advance = u16::try_from(written).unwrap_or(self.dims.cols.get());
        let next_col = clamped.col.advance_clamped(advance, self.dims.cols);
        let next_pos = CursorPosition::new(clamped.row, next_col);
        self.cursor = next_pos;
        Ok(next_pos)
    }

    /// Flushes the underlying output writer stream.
    fn flush(&mut self) -> Result<()> {
        self.writer.flush().map_err(CoblangError::from)
    }

    /// Decodes the next [`KeyEvent`] from the underlying input stream.
    fn read_key(&mut self) -> Result<KeyEvent> {
        decode_next_key_event(&mut *self.reader, &mut self.lookahead)
    }
}

/// Reads a single byte from `lookahead` if non-empty, otherwise from `reader`.
///
/// # Arguments
///
/// * `reader` - Underlying input stream.
/// * `lookahead` - Buffered lookahead queue.
fn read_next_byte(reader: &mut dyn Read, lookahead: &mut VecDeque<u8>) -> Result<Option<u8>> {
    if let Some(b) = lookahead.pop_front() {
        return Ok(Some(b));
    }
    let mut single = [0_u8; 1];
    let n = reader.read(&mut single).map_err(CoblangError::from)?;
    if n == 0 {
        Ok(None)
    } else {
        Ok(single.first().copied())
    }
}

/// Decodes one VT100 / xterm / SS3 / UTF-8 [`KeyEvent`] from `reader` + `lookahead`.
///
/// # Arguments
///
/// * `reader` - Underlying input byte stream.
/// * `lookahead` - Buffered lookahead bytes.
///
/// # Errors
///
/// Returns [`CoblangError::Io`] if reading from `reader` fails.
pub fn decode_next_key_event(
    reader: &mut dyn Read,
    lookahead: &mut VecDeque<u8>,
) -> Result<KeyEvent> {
    let Some(first) = read_next_byte(reader, lookahead)? else {
        return Ok(KeyEvent::Eof);
    };
    match first {
        0x0D => {
            if let Some(next) = read_next_byte(reader, lookahead)?
                && next != 0x0A
            {
                lookahead.push_back(next);
            }
            Ok(KeyEvent::Enter)
        }
        0x0A => Ok(KeyEvent::Enter),
        0x09 => Ok(KeyEvent::Tab),
        0x08 | 0x7F => Ok(KeyEvent::Backspace),
        0x04 => Ok(KeyEvent::Eof),
        0x1B => decode_escape_key(reader, lookahead),
        b if b < 0x20 => Ok(KeyEvent::Char(' ')),
        b if b < 0x80 => Ok(KeyEvent::Char(char::from(b))),
        lead => decode_utf8_char_key(lead, reader, lookahead),
    }
}

/// Decodes a VT100/xterm escape sequence following an initial `ESC` (`0x1B`) byte.
///
/// # Arguments
///
/// * `reader` - Input byte stream.
/// * `lookahead` - Lookahead queue.
fn decode_escape_key(reader: &mut dyn Read, lookahead: &mut VecDeque<u8>) -> Result<KeyEvent> {
    let Some(second) = read_next_byte(reader, lookahead)? else {
        return Ok(KeyEvent::Escape);
    };
    match second {
        b'[' => decode_csi_key(reader, lookahead),
        b'O' => decode_ss3_key(reader, lookahead),
        b'b' | b'B' => Ok(KeyEvent::AltLeft),
        b'f' | b'F' => Ok(KeyEvent::AltRight),
        b'd' | b'D' | 0x7F => Ok(KeyEvent::AltDelete),
        _ => Ok(KeyEvent::Escape),
    }
}

/// Decodes a CSI (`ESC [`) keystroke sequence.
///
/// # Arguments
///
/// * `reader` - Input byte stream.
/// * `lookahead` - Lookahead queue.
fn decode_csi_key(reader: &mut dyn Read, lookahead: &mut VecDeque<u8>) -> Result<KeyEvent> {
    let Some(head) = read_next_byte(reader, lookahead)? else {
        return Ok(KeyEvent::Escape);
    };
    match head {
        b'A' => Ok(KeyEvent::Up),
        b'B' => Ok(KeyEvent::Down),
        b'C' => Ok(KeyEvent::Right),
        b'D' => Ok(KeyEvent::Left),
        b'H' => Ok(KeyEvent::Home),
        b'F' => Ok(KeyEvent::End),
        b'Z' => Ok(KeyEvent::BackTab),
        b'[' => {
            let Some(linux_fn) = read_next_byte(reader, lookahead)? else {
                return Ok(KeyEvent::Escape);
            };
            if (b'A'..=b'E').contains(&linux_fn) {
                Ok(KeyEvent::Function(linux_fn - b'A' + 1))
            } else {
                Ok(KeyEvent::Escape)
            }
        }
        d if d.is_ascii_digit() => decode_csi_numeric(d, reader, lookahead),
        _ => Ok(KeyEvent::Escape),
    }
}

/// Modifier state decoded from a parameterized CSI escape sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CsiModifier {
    /// No `Alt` modifier (`;3` absent).
    Plain,
    /// `Alt` modifier (`;3` present).
    Alt,
}

/// Decodes a parameterized CSI sequence (`ESC [ <p1> [; <p2>] <final>`).
///
/// # Arguments
///
/// * `first_digit` - Initial ASCII digit byte (`b'0'..=b'9'`).
/// * `reader` - Input byte stream.
/// * `lookahead` - Lookahead queue.
fn decode_csi_numeric(
    first_digit: u8,
    reader: &mut dyn Read,
    lookahead: &mut VecDeque<u8>,
) -> Result<KeyEvent> {
    let mut p1 = u16::from(first_digit - b'0');
    let mut p2: Option<u16> = None;
    let final_byte = loop {
        let Some(b) = read_next_byte(reader, lookahead)? else {
            return Ok(KeyEvent::Escape);
        };
        if b.is_ascii_digit() {
            let digit = u16::from(b - b'0');
            if let Some(mod_val) = p2.as_mut() {
                *mod_val = mod_val.saturating_mul(10).saturating_add(digit);
            } else {
                p1 = p1.saturating_mul(10).saturating_add(digit);
            }
        } else if b == b';' {
            p2 = Some(0);
        } else {
            break b;
        }
    };
    let modifier = if p2 == Some(3) {
        CsiModifier::Alt
    } else {
        CsiModifier::Plain
    };
    match final_byte {
        b'~' => Ok(map_csi_tilde_code(p1, modifier)),
        b'D' => Ok(if modifier == CsiModifier::Alt {
            KeyEvent::AltLeft
        } else {
            KeyEvent::Left
        }),
        b'C' => Ok(if modifier == CsiModifier::Alt {
            KeyEvent::AltRight
        } else {
            KeyEvent::Right
        }),
        b'A' => Ok(KeyEvent::Up),
        b'B' => Ok(KeyEvent::Down),
        b'H' => Ok(KeyEvent::Home),
        b'F' => Ok(KeyEvent::End),
        b'P' | b'Q' | b'R' | b'S' => Ok(KeyEvent::Function(final_byte - b'P' + 1)),
        _ => Ok(KeyEvent::Escape),
    }
}

/// Maps `CSI <p1> [; 3] ~` tilde keycodes to [`KeyEvent`].
///
/// # Arguments
///
/// * `p1` - Primary numeric CSI parameter.
/// * `modifier` - Decoded [`CsiModifier`].
fn map_csi_tilde_code(p1: u16, modifier: CsiModifier) -> KeyEvent {
    if p1 == 3 && modifier == CsiModifier::Alt {
        return KeyEvent::AltDelete;
    }
    match p1 {
        1 | 7 => KeyEvent::Home,
        2 => KeyEvent::Insert,
        3 => KeyEvent::Delete,
        4 | 8 => KeyEvent::End,
        5 => KeyEvent::PageUp,
        6 => KeyEvent::PageDown,
        11..=15 => KeyEvent::Function(u8::try_from(p1 - 10).unwrap_or(1)),
        17..=21 => KeyEvent::Function(u8::try_from(p1 - 11).unwrap_or(6)),
        23..=34 => KeyEvent::Function(u8::try_from(p1 - 12).unwrap_or(11)),
        _ => KeyEvent::Escape,
    }
}

/// Decodes an SS3 (`ESC O`) keystroke sequence (`F1`..`F4`, `Home`, `End`, arrows).
///
/// # Arguments
///
/// * `reader` - Input byte stream.
/// * `lookahead` - Lookahead queue.
fn decode_ss3_key(reader: &mut dyn Read, lookahead: &mut VecDeque<u8>) -> Result<KeyEvent> {
    let Some(code) = read_next_byte(reader, lookahead)? else {
        return Ok(KeyEvent::Escape);
    };
    match code {
        b'P' | b'Q' | b'R' | b'S' => Ok(KeyEvent::Function(code - b'P' + 1)),
        b'H' => Ok(KeyEvent::Home),
        b'F' => Ok(KeyEvent::End),
        b'A' => Ok(KeyEvent::Up),
        b'B' => Ok(KeyEvent::Down),
        b'C' => Ok(KeyEvent::Right),
        b'D' => Ok(KeyEvent::Left),
        _ => Ok(KeyEvent::Escape),
    }
}

/// Decodes a multi-byte UTF-8 character starting with `lead` (`>= 0x80`).
///
/// # Arguments
///
/// * `lead` - Leading UTF-8 byte (`>= 0x80`).
/// * `reader` - Input byte stream.
/// * `lookahead` - Lookahead queue.
fn decode_utf8_char_key(
    lead: u8,
    reader: &mut dyn Read,
    lookahead: &mut VecDeque<u8>,
) -> Result<KeyEvent> {
    let needed = match lead {
        0xC2..=0xDF => 1_usize,
        0xE0..=0xEF => 2_usize,
        0xF0..=0xF4 => 3_usize,
        _ => return Ok(KeyEvent::Char('\u{FFFD}')),
    };
    let mut buf = Vec::with_capacity(needed.saturating_add(1));
    buf.push(lead);
    for _ in 0..needed {
        let Some(cont) = read_next_byte(reader, lookahead)? else {
            return Ok(KeyEvent::Char('\u{FFFD}'));
        };
        if (cont & 0xC0) != 0x80 {
            lookahead.push_back(cont);
            return Ok(KeyEvent::Char('\u{FFFD}'));
        }
        buf.push(cont);
    }
    let ch = core::str::from_utf8(&buf)
        .ok()
        .and_then(|s| s.chars().next())
        .unwrap_or('\u{FFFD}');
    Ok(KeyEvent::Char(ch))
}
