//! `SCREEN SECTION` execution engine orchestrating `DISPLAY`, interactive `ACCEPT`, `PICTURE` codecs,
//! `CRT STATUS`, and `CURSOR` special registers over any [`TerminalBackend`].

use std::collections::BTreeMap;

use coblang_common::{CoblangError, DataItemId, Result};
use coblang_decimal::{FixedDecimal, HighOrderPolicy, RoundingMode};
use coblang_hir::{DataCategory, HirType, JustifiedSpec};
use coblang_pic::{
    Justification, MoveFieldDesc, MoveSource, PictureProgramKind, de_edit_to_decimal,
    decode_numeric_field, execute_move, format_field_for_display,
};

use crate::backend::TerminalBackend;
use crate::layout::{
    LiteralTileMode, ResolvedScreenField, ResolvedScreenStep, ScreenAttributeSet,
    ScreenDataBinding, ScreenFieldPic, ScreenLayout,
};
use crate::types::{
    CellStyle, CrtStatusCode, CursorFormat, CursorPosition, CursorVisibility, KeyEvent,
    ScreenAttrFlags, ScreenCol, ScreenExceptionKind,
};

/// Backing storage interface mapping [`DataItemId`] handles to COBOL Working/Local/Linkage byte slices.
pub trait FieldStorage {
    /// Borrows the raw storage bytes of `id`.
    ///
    /// # Arguments
    ///
    /// * `id` - Bound COBOL [`DataItemId`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ScreenBackend`] if `id` is not registered in storage.
    fn read_field(&self, id: DataItemId) -> Result<&[u8]>;

    /// Mutably borrows the raw storage bytes of `id`.
    ///
    /// # Arguments
    ///
    /// * `id` - Bound COBOL [`DataItemId`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ScreenBackend`] if `id` is not registered in storage.
    fn write_field_mut(&mut self, id: DataItemId) -> Result<&mut [u8]>;
}

/// Deterministic `BTreeMap`-backed [`FieldStorage`] for standalone tests and runtime staging.
#[derive(Debug, Clone, Default)]
pub struct MapFieldStorage {
    /// Map from [`DataItemId`] to raw field storage bytes.
    items: BTreeMap<DataItemId, Vec<u8>>,
}

impl MapFieldStorage {
    /// Creates an empty [`MapFieldStorage`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: BTreeMap::new(),
        }
    }

    /// Registers or replaces the backing byte buffer for `id`.
    ///
    /// # Arguments
    ///
    /// * `id` - COBOL [`DataItemId`].
    /// * `bytes` - Initial raw storage bytes.
    pub fn insert(&mut self, id: DataItemId, bytes: Vec<u8>) {
        self.items.insert(id, bytes);
    }

    /// Borrows the stored byte slice of `id`, if present.
    ///
    /// # Arguments
    ///
    /// * `id` - COBOL [`DataItemId`].
    #[must_use]
    pub fn get(&self, id: DataItemId) -> Option<&[u8]> {
        self.items.get(&id).map(Vec::as_slice)
    }
}

impl FieldStorage for MapFieldStorage {
    /// Borrows the raw storage bytes mapped to `id`.
    ///
    /// # Arguments
    ///
    /// * `id` - Bound COBOL [`DataItemId`].
    fn read_field(&self, id: DataItemId) -> Result<&[u8]> {
        self.items
            .get(&id)
            .map(Vec::as_slice)
            .ok_or_else(|| CoblangError::ScreenBackend {
                message: format!("unbound screen source DataItemId({})", id.as_u32()),
            })
    }

    /// Mutably borrows the raw storage bytes mapped to `id`.
    ///
    /// # Arguments
    ///
    /// * `id` - Bound COBOL [`DataItemId`].
    fn write_field_mut(&mut self, id: DataItemId) -> Result<&mut [u8]> {
        self.items
            .get_mut(&id)
            .map(Vec::as_mut_slice)
            .ok_or_else(|| CoblangError::ScreenBackend {
                message: format!("unbound screen target DataItemId({})", id.as_u32()),
            })
    }
}

/// Interactive character-entry mode toggled by the `Insert` key during `ACCEPT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EditInsertMode {
    /// Default COBOL screen overwrite mode.
    #[default]
    Overwrite,
    /// Shift trailing characters rightward when typing.
    Insert,
}

impl EditInsertMode {
    /// Toggles between [`Self::Overwrite`] and [`Self::Insert`].
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Overwrite => Self::Insert,
            Self::Insert => Self::Overwrite,
        }
    }

    /// Returns the corresponding [`CursorVisibility`] indicator (`Normal` vs `High`).
    #[must_use]
    pub const fn cursor_visibility(self) -> CursorVisibility {
        match self {
            Self::Overwrite => CursorVisibility::Normal,
            Self::Insert => CursorVisibility::High,
        }
    }
}

/// Scope controlling whether `BLANK SCREEN`, `BLANK LINE`, `ERASE EOL`, and `ERASE EOS` execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectiveMode {
    /// `DISPLAY`: honor all clear, erase, and bell directives.
    HonorAll,
    /// `ACCEPT`: ignore `BLANK` and `ERASE` directives per ISO COBOL / `GnuCOBOL` screen rules, honoring `BELL`.
    IgnoreBlankAndErase,
}

/// Completion summary returned by [`ScreenEngine::accept_layout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptSummary {
    /// Resulting 4-digit `CRT STATUS` code.
    pub crt_status: CrtStatusCode,
    /// Final 1-based cursor coordinate when `ACCEPT` terminated.
    pub final_cursor: CursorPosition,
    /// Terminating [`KeyEvent`] (`Enter`, `Escape`, `Function(n)`, `Eof`, etc.).
    pub terminating_key: KeyEvent,
    /// Any `EC-SCREEN-*` runtime conditions encountered during layout or validation.
    pub exceptions: Vec<ScreenExceptionKind>,
}

/// Stateful `SCREEN SECTION` runtime controller tracking `CRT STATUS`, `CURSOR`, and exceptions.
#[derive(Debug, Clone)]
pub struct ScreenEngine {
    /// Most recent `CRT STATUS` code (`0000` initially).
    crt_status: CrtStatusCode,
    /// Current `CURSOR` special register coordinate (`(1, 1)` initially).
    cursor: CursorPosition,
    /// Optional explicit starting cursor requested via `SPECIAL-NAMES. CURSOR IS ...` prior to `ACCEPT`.
    requested_accept_cursor: Option<CursorPosition>,
    /// Most recently recorded `EC-SCREEN-*` condition, if any.
    last_exception: Option<ScreenExceptionKind>,
}

impl Default for ScreenEngine {
    /// Creates a fresh [`ScreenEngine`] initialized to `CRT STATUS = 0000` and `CURSOR = (1, 1)`.
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenEngine {
    /// Constructs a new [`ScreenEngine`] with `CRT STATUS = 0000` and `CURSOR = (1, 1)`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            crt_status: CrtStatusCode::NORMAL_COMPLETION,
            cursor: CursorPosition::HOME,
            requested_accept_cursor: None,
            last_exception: None,
        }
    }

    /// Returns the most recent [`CrtStatusCode`].
    #[must_use]
    pub const fn crt_status(&self) -> CrtStatusCode {
        self.crt_status
    }

    /// Returns the current [`CursorPosition`].
    #[must_use]
    pub const fn cursor(&self) -> CursorPosition {
        self.cursor
    }

    /// Returns the most recent [`ScreenExceptionKind`], if any.
    #[must_use]
    pub const fn last_exception(&self) -> Option<ScreenExceptionKind> {
        self.last_exception
    }

    /// Clears any recorded [`ScreenExceptionKind`].
    pub fn clear_last_exception(&mut self) {
        self.last_exception = None;
    }

    /// Sets an explicit initial `ACCEPT` cursor position decoded from a COBOL `CURSOR` special register.
    ///
    /// # Arguments
    ///
    /// * `pos` - Optional 1-based [`CursorPosition`] (`None` uses the first editable field).
    pub fn set_requested_accept_cursor(&mut self, pos: Option<CursorPosition>) {
        self.requested_accept_cursor = pos;
    }

    /// Loads `requested_accept_cursor` by decoding `register_buf` with `format`.
    ///
    /// # Arguments
    ///
    /// * `format` - [`CursorFormat::FourDigitLlCc`] or [`CursorFormat::SixDigitLllCcc`].
    /// * `register_buf` - Raw bytes of the COBOL `CURSOR` special register.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ScreenBackend`] if `register_buf` is shorter than `format` requires.
    pub fn load_cursor_register(
        &mut self,
        format: CursorFormat,
        register_buf: &[u8],
    ) -> Result<()> {
        let decoded = format.decode(register_buf)?;
        self.requested_accept_cursor = decoded;
        if let Some(pos) = decoded {
            self.cursor = pos;
        }
        Ok(())
    }

    /// Synchronizes `self.cursor` back into a COBOL `CURSOR` special register buffer.
    ///
    /// # Arguments
    ///
    /// * `format` - [`CursorFormat::FourDigitLlCc`] or [`CursorFormat::SixDigitLllCcc`].
    /// * `register_buf` - Mutable COBOL `CURSOR` special register slice.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ScreenBackend`] if `register_buf` is shorter than `format` requires.
    pub fn store_cursor_register(
        &self,
        format: CursorFormat,
        register_buf: &mut [u8],
    ) -> Result<()> {
        format.encode(self.cursor, register_buf)
    }

    /// Synchronizes `self.crt_status` into a COBOL `CRT STATUS` special register buffer (`3` or `>= 4` bytes).
    ///
    /// # Arguments
    ///
    /// * `register_buf` - Mutable COBOL `CRT STATUS` special register slice.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ScreenBackend`] if `register_buf.len() < 3`.
    pub fn store_crt_status_register(&self, register_buf: &mut [u8]) -> Result<()> {
        self.crt_status.write_to_register(register_buf)
    }

    /// Executes `DISPLAY screen-name` (or `DISPLAY item AT ...`) against `backend`, reading bound
    /// `FROM`/`USING` values from `storage` and updating `CRT STATUS` and `CURSOR`.
    ///
    /// # Arguments
    ///
    /// * `layout` - Compiled [`ScreenLayout`] to render.
    /// * `storage` - Backing COBOL [`FieldStorage`].
    /// * `backend` - Target [`TerminalBackend`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if field decoding/formatting fails or `backend` reports an I/O error.
    pub fn display_layout(
        &mut self,
        layout: &ScreenLayout,
        storage: &dyn FieldStorage,
        backend: &mut dyn TerminalBackend,
    ) -> Result<CursorPosition> {
        self.record_layout_exceptions(layout);
        for step in layout.steps() {
            match step {
                ResolvedScreenStep::Directive { pos, flags, style } => {
                    execute_directive_step(*pos, *flags, *style, DirectiveMode::HonorAll, backend)?;
                }
                ResolvedScreenStep::Field(field) => {
                    let rendered = format_field_for_screen_display(field, storage)?;
                    let _ = backend.write_span(field.pos, &rendered, field.attrs.cell_style())?;
                }
            }
        }
        backend.flush()?;
        self.cursor = backend.cursor();
        self.crt_status = CrtStatusCode::NORMAL_COMPLETION;
        Ok(self.cursor)
    }

    /// Executes interactive `ACCEPT screen-name` (or `ACCEPT item AT ...`) against `backend`,
    /// honoring `PROMPT`, `AUTO`, `SECURE`, `REQUIRED`, `FULL`, `ZERO-FILL`, `UPDATE`, `INITIAL`,
    /// multi-field navigation, and updating `storage`, `CRT STATUS`, and `CURSOR`.
    ///
    /// # Arguments
    ///
    /// * `layout` - Compiled [`ScreenLayout`] to accept.
    /// * `storage` - Mutable COBOL [`FieldStorage`].
    /// * `backend` - Interactive or scripted [`TerminalBackend`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if storage access, `MOVE` write-back, or terminal I/O fails.
    pub fn accept_layout(
        &mut self,
        layout: &ScreenLayout,
        storage: &mut dyn FieldStorage,
        backend: &mut dyn TerminalBackend,
    ) -> Result<AcceptSummary> {
        let mut exceptions = layout.exceptions().to_vec();
        self.record_layout_exceptions(layout);

        let mut editors = prepare_accept_editors(layout, storage, backend)?;
        if editors.is_empty() {
            backend.flush()?;
            let key = backend.read_key()?;
            let status = key.to_crt_status();
            self.crt_status = status;
            self.cursor = backend.cursor();
            return Ok(AcceptSummary {
                crt_status: status,
                final_cursor: self.cursor,
                terminating_key: key,
                exceptions,
            });
        }

        let mut focus = AcceptFocusState::new(&editors, self.requested_accept_cursor);
        backend.set_cursor_visibility(focus.insert_mode.cursor_visibility())?;

        let exit = loop {
            let cur_pos = editor_cursor_pos(&editors, focus.field_idx, focus.char_offset);
            backend.set_cursor(cur_pos)?;
            backend.flush()?;

            let key = backend.read_key()?;
            if let Some(done) = dispatch_accept_key(
                key,
                &mut editors,
                &mut focus,
                storage,
                backend,
                &mut exceptions,
            )? {
                break done;
            }
        };

        let final_pos = editor_cursor_pos(&editors, focus.field_idx, focus.char_offset);
        backend.set_cursor(final_pos)?;
        backend.set_cursor_visibility(CursorVisibility::Normal)?;
        backend.flush()?;

        if let Some(&last_exc) = exceptions.last() {
            self.last_exception = Some(last_exc);
        }
        self.crt_status = exit.status;
        self.cursor = backend.cursor();
        Ok(AcceptSummary {
            crt_status: exit.status,
            final_cursor: self.cursor,
            terminating_key: exit.key,
            exceptions,
        })
    }

    /// Records the last exception from `layout.exceptions()` onto `self.last_exception` if non-empty.
    ///
    /// # Arguments
    ///
    /// * `layout` - Compiled [`ScreenLayout`].
    fn record_layout_exceptions(&mut self, layout: &ScreenLayout) {
        if let Some(&last) = layout.exceptions().last() {
            self.last_exception = Some(last);
        }
    }
}

/// Active focus and insert-mode state during an interactive `ACCEPT` loop.
struct AcceptFocusState {
    /// Currently focused editable field index.
    field_idx: usize,
    /// Intra-field character offset.
    char_offset: usize,
    /// Active [`EditInsertMode`].
    insert_mode: EditInsertMode,
}

impl AcceptFocusState {
    /// Initializes focus state from `editors` and an optional requested [`CursorPosition`].
    ///
    /// # Arguments
    ///
    /// * `editors` - Editable field buffers.
    /// * `requested` - Optional requested [`CursorPosition`].
    fn new(editors: &[ActiveFieldEditor<'_>], requested: Option<CursorPosition>) -> Self {
        let (field_idx, char_offset) = resolve_initial_focus(editors, requested);
        Self {
            field_idx,
            char_offset,
            insert_mode: EditInsertMode::Overwrite,
        }
    }
}

/// Terminal status and key produced when the `ACCEPT` keystroke loop finishes.
struct AcceptLoopExit {
    /// Final [`CrtStatusCode`].
    status: CrtStatusCode,
    /// Terminating [`KeyEvent`].
    key: KeyEvent,
}

/// Renders initial `ACCEPT` frame (ignoring `BLANK`/`ERASE` directives) and initializes editable buffers.
///
/// # Arguments
///
/// * `layout` - Compiled [`ScreenLayout`].
/// * `storage` - Backing COBOL [`FieldStorage`].
/// * `backend` - Target [`TerminalBackend`].
fn prepare_accept_editors<'a>(
    layout: &'a ScreenLayout,
    storage: &dyn FieldStorage,
    backend: &mut dyn TerminalBackend,
) -> Result<Vec<ActiveFieldEditor<'a>>> {
    let mut editors: Vec<ActiveFieldEditor<'a>> = Vec::new();
    for step in layout.steps() {
        match step {
            ResolvedScreenStep::Directive { pos, flags, style } => {
                execute_directive_step(
                    *pos,
                    *flags,
                    *style,
                    DirectiveMode::IgnoreBlankAndErase,
                    backend,
                )?;
            }
            ResolvedScreenStep::Field(field) => {
                if let Some((target_id, pic)) = extract_editable_binding(&field.binding) {
                    let initial_chars = load_initial_edit_chars(field, storage)?;
                    let ed = ActiveFieldEditor {
                        field,
                        target_id,
                        pic,
                        chars: initial_chars,
                    };
                    render_editor_cells(&ed, backend)?;
                    editors.push(ed);
                } else {
                    let rendered = format_field_for_screen_display(field, storage)?;
                    let _ = backend.write_span(field.pos, &rendered, field.attrs.cell_style())?;
                }
            }
        }
    }
    Ok(editors)
}

/// Dispatches a single [`KeyEvent`] during `ACCEPT`, returning `Ok(Some(AcceptLoopExit))` when `ACCEPT` terminates.
///
/// # Arguments
///
/// * `key` - Incoming [`KeyEvent`].
/// * `editors` - Mutable editable field buffers.
/// * `focus` - Mutable [`AcceptFocusState`].
/// * `storage` - Mutable COBOL [`FieldStorage`].
/// * `backend` - Target [`TerminalBackend`].
/// * `exceptions` - Mutable `EC-SCREEN-*` accumulator.
fn dispatch_accept_key(
    key: KeyEvent,
    editors: &mut [ActiveFieldEditor<'_>],
    focus: &mut AcceptFocusState,
    storage: &mut dyn FieldStorage,
    backend: &mut dyn TerminalBackend,
    exceptions: &mut Vec<ScreenExceptionKind>,
) -> Result<Option<AcceptLoopExit>> {
    match key {
        KeyEvent::Insert => {
            focus.insert_mode = focus.insert_mode.toggled();
            backend.set_cursor_visibility(focus.insert_mode.cursor_visibility())?;
            Ok(None)
        }
        KeyEvent::Home
        | KeyEvent::End
        | KeyEvent::Left
        | KeyEvent::Right
        | KeyEvent::AltLeft
        | KeyEvent::AltRight => {
            apply_motion_key(key, editors, focus);
            Ok(None)
        }
        KeyEvent::Backspace | KeyEvent::Delete | KeyEvent::AltDelete => {
            apply_erasure_key(key, editors, focus, backend)?;
            Ok(None)
        }
        KeyEvent::Char(ch) => {
            handle_printable_char(ch, editors, focus, storage, backend, exceptions)
        }
        KeyEvent::Tab | KeyEvent::Down => {
            if focus.field_idx + 1 < editors.len() {
                focus.field_idx += 1;
                focus.char_offset = 0;
                Ok(None)
            } else {
                try_validate_and_finish(key, editors, focus, storage, backend, exceptions)
            }
        }
        KeyEvent::BackTab | KeyEvent::Up => {
            if focus.field_idx > 0 {
                focus.field_idx -= 1;
                focus.char_offset = 0;
                Ok(None)
            } else {
                try_validate_and_finish(key, editors, focus, storage, backend, exceptions)
            }
        }
        KeyEvent::Enter => try_validate_and_finish(
            KeyEvent::Enter,
            editors,
            focus,
            storage,
            backend,
            exceptions,
        ),
        KeyEvent::PageUp | KeyEvent::PageDown | KeyEvent::Function(_) => {
            commit_all_editors(editors, storage, exceptions)?;
            Ok(Some(AcceptLoopExit {
                status: key.to_crt_status(),
                key,
            }))
        }
        KeyEvent::Escape | KeyEvent::Timeout => Ok(Some(AcceptLoopExit {
            status: key.to_crt_status(),
            key,
        })),
        KeyEvent::Eof => {
            if first_invalid_editor(editors).is_some() {
                exceptions.push(ScreenExceptionKind::ImplementorCondition);
                Ok(Some(AcceptLoopExit {
                    status: CrtStatusCode::SCREEN_ERROR,
                    key: KeyEvent::Eof,
                }))
            } else {
                commit_all_editors(editors, storage, exceptions)?;
                Ok(Some(AcceptLoopExit {
                    status: CrtStatusCode::INPUT_EOF,
                    key: KeyEvent::Eof,
                }))
            }
        }
    }
}

/// Applies cursor-movement keys (`Home`, `End`, `Left`, `Right`, `AltLeft`, `AltRight`) to `focus`.
///
/// # Arguments
///
/// * `key` - Motion [`KeyEvent`].
/// * `editors` - Slice of editable field buffers.
/// * `focus` - Mutable [`AcceptFocusState`].
fn apply_motion_key(
    key: KeyEvent,
    editors: &[ActiveFieldEditor<'_>],
    focus: &mut AcceptFocusState,
) {
    match key {
        KeyEvent::Home => {
            focus.char_offset = 0;
        }
        KeyEvent::End => {
            focus.char_offset = editor_end_offset(editors, focus.field_idx);
        }
        KeyEvent::Left => {
            if focus.char_offset > 0 {
                focus.char_offset -= 1;
            } else if focus.field_idx > 0 {
                focus.field_idx -= 1;
                focus.char_offset = editor_last_cell_offset(editors, focus.field_idx);
            }
        }
        KeyEvent::Right => {
            let max_off = editor_last_cell_offset(editors, focus.field_idx);
            if focus.char_offset < max_off {
                focus.char_offset += 1;
            } else if focus.field_idx + 1 < editors.len() {
                focus.field_idx += 1;
                focus.char_offset = 0;
            }
        }
        KeyEvent::AltLeft => {
            focus.char_offset = editor_word_left(editors, focus.field_idx, focus.char_offset);
        }
        _ => {
            focus.char_offset = editor_word_right(editors, focus.field_idx, focus.char_offset);
        }
    }
}

/// Applies `Backspace`, `Delete`, or `AltDelete` to `editors[focus.field_idx]`.
///
/// # Arguments
///
/// * `key` - Erasure [`KeyEvent`].
/// * `editors` - Mutable editable field buffers.
/// * `focus` - Mutable [`AcceptFocusState`].
/// * `backend` - Target [`TerminalBackend`].
fn apply_erasure_key(
    key: KeyEvent,
    editors: &mut [ActiveFieldEditor<'_>],
    focus: &mut AcceptFocusState,
    backend: &mut dyn TerminalBackend,
) -> Result<()> {
    match key {
        KeyEvent::Backspace => {
            if focus.char_offset > 0 {
                focus.char_offset -= 1;
                for ed in editors.iter_mut().skip(focus.field_idx).take(1) {
                    delete_char_at(&mut ed.chars, focus.char_offset);
                    render_editor_cells(ed, backend)?;
                }
            }
        }
        KeyEvent::Delete => {
            for ed in editors.iter_mut().skip(focus.field_idx).take(1) {
                delete_char_at(&mut ed.chars, focus.char_offset);
                render_editor_cells(ed, backend)?;
            }
        }
        _ => {
            for ed in editors.iter_mut().skip(focus.field_idx).take(1) {
                for slot in ed.chars.iter_mut().skip(focus.char_offset) {
                    *slot = ' ';
                }
                render_editor_cells(ed, backend)?;
            }
        }
    }
    Ok(())
}

/// Handles a printable character `ch`, advancing `focus` and triggering `AUTO` skip/completion when enabled.
///
/// # Arguments
///
/// * `ch` - Printable character typed.
/// * `editors` - Mutable editable field buffers.
/// * `focus` - Mutable [`AcceptFocusState`].
/// * `storage` - Mutable COBOL [`FieldStorage`].
/// * `backend` - Target [`TerminalBackend`].
/// * `exceptions` - Mutable `EC-SCREEN-*` accumulator.
fn handle_printable_char(
    ch: char,
    editors: &mut [ActiveFieldEditor<'_>],
    focus: &mut AcceptFocusState,
    storage: &mut dyn FieldStorage,
    backend: &mut dyn TerminalBackend,
    exceptions: &mut Vec<ScreenExceptionKind>,
) -> Result<Option<AcceptLoopExit>> {
    let (auto_mode, max_off) = apply_char_input(
        editors,
        focus.field_idx,
        focus.char_offset,
        ch,
        focus.insert_mode,
        backend,
    )?;
    if focus.char_offset < max_off {
        focus.char_offset += 1;
        return Ok(None);
    }
    if auto_mode == AutoAdvancePolicy::Manual {
        return Ok(None);
    }
    if !is_editor_valid(editors, focus.field_idx) {
        backend.ring_bell()?;
        return Ok(None);
    }
    if focus.field_idx + 1 < editors.len() {
        focus.field_idx += 1;
        focus.char_offset = 0;
        return Ok(None);
    }
    try_validate_and_finish(
        KeyEvent::Enter,
        editors,
        focus,
        storage,
        backend,
        exceptions,
    )
}

/// Validates all editable fields, either ringing the bell and focusing the first invalid field
/// or committing all buffers and terminating with `exit_key`.
///
/// # Arguments
///
/// * `exit_key` - Terminating [`KeyEvent`].
/// * `editors` - Editable field buffers.
/// * `focus` - Mutable [`AcceptFocusState`].
/// * `storage` - Mutable COBOL [`FieldStorage`].
/// * `backend` - Target [`TerminalBackend`].
/// * `exceptions` - Mutable `EC-SCREEN-*` accumulator.
fn try_validate_and_finish(
    exit_key: KeyEvent,
    editors: &[ActiveFieldEditor<'_>],
    focus: &mut AcceptFocusState,
    storage: &mut dyn FieldStorage,
    backend: &mut dyn TerminalBackend,
    exceptions: &mut Vec<ScreenExceptionKind>,
) -> Result<Option<AcceptLoopExit>> {
    if let Some(invalid_idx) = first_invalid_editor(editors) {
        focus.field_idx = invalid_idx;
        focus.char_offset = 0;
        backend.ring_bell()?;
        Ok(None)
    } else {
        commit_all_editors(editors, storage, exceptions)?;
        Ok(Some(AcceptLoopExit {
            status: exit_key.to_crt_status(),
            key: exit_key,
        }))
    }
}

/// Whether `AUTO` skip/termination is enabled on an editable field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoAdvancePolicy {
    /// Manual Enter/Tab required at end of field.
    Manual,
    /// `AUTO` enabled: advance or complete immediately when last cell is typed.
    AutoEnabled,
}

/// Mutable in-flight edit buffer for a single editable `TO` or `USING` screen field.
struct ActiveFieldEditor<'a> {
    /// Coordinate-resolved screen field metadata.
    field: &'a ResolvedScreenField,
    /// Bound destination COBOL [`DataItemId`].
    target_id: DataItemId,
    /// `PICTURE` and storage descriptor of the editable field.
    pic: &'a ScreenFieldPic,
    /// Fixed-length character cell buffer (`len == field.width`).
    chars: Vec<char>,
}

/// Extracts `(target_id, pic)` when `binding` is editable (`To` or `Using`).
///
/// # Arguments
///
/// * `binding` - [`ScreenDataBinding`] to inspect.
const fn extract_editable_binding(
    binding: &ScreenDataBinding,
) -> Option<(DataItemId, &ScreenFieldPic)> {
    match binding {
        ScreenDataBinding::To { target_id, pic, .. }
        | ScreenDataBinding::Using { target_id, pic, .. } => Some((*target_id, pic)),
        ScreenDataBinding::ActionOnly
        | ScreenDataBinding::Literal { .. }
        | ScreenDataBinding::From { .. } => None,
    }
}

/// Executes a single directive step against `backend` according to `mode`.
///
/// # Arguments
///
/// * `pos` - Directive [`CursorPosition`].
/// * `flags` - Directive [`ScreenAttrFlags`].
/// * `style` - Fill [`CellStyle`].
/// * `mode` - [`DirectiveMode`] (`HonorAll` vs `IgnoreBlankAndErase`).
/// * `backend` - Target [`TerminalBackend`].
fn execute_directive_step(
    pos: CursorPosition,
    flags: ScreenAttrFlags,
    style: CellStyle,
    mode: DirectiveMode,
    backend: &mut dyn TerminalBackend,
) -> Result<()> {
    if mode == DirectiveMode::HonorAll {
        if flags.contains(ScreenAttrFlags::BLANK_SCREEN) {
            backend.clear_screen(style)?;
        }
        if flags.contains(ScreenAttrFlags::BLANK_LINE) {
            backend.clear_line(pos.row, style)?;
        }
        if flags.contains(ScreenAttrFlags::ERASE_EOS) {
            backend.erase_to_end_of_screen(pos, style)?;
        }
        if flags.contains(ScreenAttrFlags::ERASE_EOL) {
            backend.erase_to_end_of_line(pos, style)?;
        }
    }
    if flags.contains(ScreenAttrFlags::BELL) {
        backend.ring_bell()?;
    }
    Ok(())
}

/// Formats `field` into an exact `field.width`-character string for `DISPLAY` (or non-editable background during `ACCEPT`).
///
/// # Arguments
///
/// * `field` - Coordinate-resolved [`ResolvedScreenField`].
/// * `storage` - Backing COBOL [`FieldStorage`].
///
/// # Errors
///
/// Returns [`CoblangError`] if `storage` lookup or `PICTURE` numeric decoding fails.
pub fn format_field_for_screen_display(
    field: &ResolvedScreenField,
    storage: &dyn FieldStorage,
) -> Result<String> {
    let width = field.width as usize;
    match &field.binding {
        ScreenDataBinding::ActionOnly => Ok(String::new()),
        ScreenDataBinding::Literal { bytes, tile, .. } => {
            Ok(format_literal_cells(bytes, *tile, width, field.attrs.flags))
        }
        ScreenDataBinding::From { source_id, pic, .. }
        | ScreenDataBinding::Using {
            target_id: source_id,
            pic,
            ..
        } => {
            let raw = storage.read_field(*source_id)?;
            format_bound_bytes_for_screen(raw, pic, field.attrs, width)
        }
        ScreenDataBinding::To { target_id, pic, .. } => {
            if field.attrs.flags.contains(ScreenAttrFlags::UPDATE)
                && !field.attrs.flags.contains(ScreenAttrFlags::INITIAL)
            {
                let raw = storage.read_field(*target_id)?;
                format_bound_bytes_for_screen(raw, pic, field.attrs, width)
            } else {
                let fill = field.attrs.effective_prompt().map_or(' ', |p| p.character);
                Ok(core::iter::repeat_n(fill, width).collect())
            }
        }
    }
}

/// Sanitizes control characters (`< 0x20` or `0x7F`, including `LOW-VALUES` `0x00`) to ASCII space `' '`.
///
/// # Arguments
///
/// * `ch` - Decoded Unicode scalar.
const fn sanitize_screen_char(ch: char) -> char {
    if (ch as u32) < 0x20 || ch == '\u{007F}' {
        ' '
    } else {
        ch
    }
}

/// Left- or right-aligns `chars` into `width` cells, truncating excess characters or padding with `' '`.
///
/// # Arguments
///
/// * `chars` - Source character slice.
/// * `width` - Target cell count.
/// * `flags` - Active [`ScreenAttrFlags`] (`JUSTIFIED_RIGHT`).
fn align_chars(chars: &[char], width: usize, flags: ScreenAttrFlags) -> Vec<char> {
    let mut out = Vec::with_capacity(width);
    if flags.contains(ScreenAttrFlags::JUSTIFIED_RIGHT) {
        if chars.len() >= width {
            let skip_n = chars.len() - width;
            out.extend(chars.iter().skip(skip_n).copied());
        } else {
            let pad = width - chars.len();
            out.extend(core::iter::repeat_n(' ', pad));
            out.extend(chars.iter().copied());
        }
    } else {
        out.extend(chars.iter().take(width).copied());
        if out.len() < width {
            let pad = width - out.len();
            out.extend(core::iter::repeat_n(' ', pad));
        }
    }
    out
}

/// Masks non-space characters with `'*'` when [`ScreenAttrFlags::SECURE`] is enabled.
///
/// # Arguments
///
/// * `chars` - Aligned cell characters.
/// * `flags` - Active [`ScreenAttrFlags`].
fn mask_if_secure(chars: &[char], flags: ScreenAttrFlags) -> String {
    let mut out = String::with_capacity(chars.len());
    let secure = flags.contains(ScreenAttrFlags::SECURE);
    for &ch in chars {
        if secure && ch != ' ' {
            out.push('*');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Returns `true` when `chars` represents a formatted numeric zero (`'0'`, separators, signs, spaces, and no `'1'..='9'`).
///
/// # Arguments
///
/// * `chars` - Formatted character slice.
fn is_displayed_zero(chars: &[char]) -> bool {
    let mut saw_zero = false;
    for &ch in chars {
        match ch {
            '0' => saw_zero = true,
            ' ' | '+' | '-' | '.' | ',' | '$' => {}
            _ => return false,
        }
    }
    saw_zero
}

/// Formats a literal byte slice into `width` cells (`Single` vs `RepeatAll` tiling).
///
/// # Arguments
///
/// * `bytes` - Raw literal bytes.
/// * `tile` - [`LiteralTileMode`].
/// * `width` - Target cell count.
/// * `flags` - Active [`ScreenAttrFlags`] (`JUSTIFIED_RIGHT`, `SECURE`).
fn format_literal_cells(
    bytes: &[u8],
    tile: LiteralTileMode,
    width: usize,
    flags: ScreenAttrFlags,
) -> String {
    let clean_chars: Vec<char> = String::from_utf8_lossy(bytes)
        .chars()
        .map(sanitize_screen_char)
        .collect();
    let aligned: Vec<char> = match tile {
        LiteralTileMode::Single => align_chars(&clean_chars, width, flags),
        LiteralTileMode::RepeatAll => {
            if clean_chars.is_empty() {
                vec![' '; width]
            } else {
                let pat_len = clean_chars.len();
                let mut tiled = Vec::with_capacity(width);
                for ch in clean_chars.iter().copied().cycle().take(width) {
                    let _ = pat_len;
                    tiled.push(ch);
                }
                tiled
            }
        }
    };
    mask_if_secure(&aligned, flags)
}

/// Formats raw COBOL storage bytes according to `pic`, `attrs`, and `width`.
///
/// # Arguments
///
/// * `raw_bytes` - Source COBOL field bytes.
/// * `pic` - [`ScreenFieldPic`] descriptor.
/// * `attrs` - [`ScreenAttributeSet`].
/// * `width` - Visible screen cell count.
fn format_bound_bytes_for_screen(
    raw_bytes: &[u8],
    pic: &ScreenFieldPic,
    attrs: ScreenAttributeSet,
    width: usize,
) -> Result<String> {
    let raw_formatted = if let Some(prog) = pic.edit_program() {
        match prog.kind() {
            PictureProgramKind::NumericEdited(_) => {
                let dec = decode_numeric_field(raw_bytes, pic.move_desc())?;
                prog.format_numeric(
                    &dec,
                    RoundingMode::Truncation,
                    HighOrderPolicy::ModuloTruncate,
                )?
                .0
            }
            PictureProgramKind::AlphanumericEdited
            | PictureProgramKind::NationalEdited
            | PictureProgramKind::Utf8Edited => {
                let just = if attrs.flags.contains(ScreenAttrFlags::JUSTIFIED_RIGHT) {
                    Justification::Right
                } else {
                    Justification::Left
                };
                prog.format_alphanumeric(raw_bytes, just, b' ')
            }
        }
    } else {
        format_field_for_display(raw_bytes, pic.move_desc())?
    };

    let mut chars: Vec<char> = String::from_utf8_lossy(&raw_formatted)
        .chars()
        .map(sanitize_screen_char)
        .collect();

    if attrs.flags.contains(ScreenAttrFlags::BLANK_WHEN_ZERO) && is_displayed_zero(&chars) {
        chars.fill(' ');
    } else if attrs.flags.contains(ScreenAttrFlags::ZERO_FILL) {
        for ch in &mut chars {
            if *ch == ' ' {
                *ch = '0';
            }
        }
    }

    let aligned = align_chars(&chars, width, attrs.flags);
    Ok(mask_if_secure(&aligned, attrs.flags))
}

/// Loads initial editable `char` cells for a `TO` or `USING` field at the start of `ACCEPT`.
///
/// # Arguments
///
/// * `field` - Editable [`ResolvedScreenField`].
/// * `storage` - Backing COBOL [`FieldStorage`].
fn load_initial_edit_chars(
    field: &ResolvedScreenField,
    storage: &dyn FieldStorage,
) -> Result<Vec<char>> {
    let width = field.width as usize;
    if field.attrs.flags.contains(ScreenAttrFlags::INITIAL) {
        return Ok(vec![' '; width]);
    }
    let preload = match &field.binding {
        ScreenDataBinding::Using { target_id, pic, .. } => Some((*target_id, pic)),
        ScreenDataBinding::To { target_id, pic, .. }
            if field.attrs.flags.contains(ScreenAttrFlags::UPDATE) =>
        {
            Some((*target_id, pic))
        }
        ScreenDataBinding::ActionOnly
        | ScreenDataBinding::Literal { .. }
        | ScreenDataBinding::From { .. }
        | ScreenDataBinding::To { .. } => None,
    };
    let Some((item_id, pic)) = preload else {
        return Ok(vec![' '; width]);
    };
    let raw = storage.read_field(item_id)?;
    let unmasked_attrs = ScreenAttributeSet {
        flags: field.attrs.flags.without(ScreenAttrFlags::SECURE),
        ..field.attrs
    };
    let formatted = format_bound_bytes_for_screen(raw, pic, unmasked_attrs, width)?;
    Ok(formatted.chars().collect())
}

/// Renders an [`ActiveFieldEditor`] onto `backend`, applying `SECURE` masking and `PROMPT` filler in blank cells.
///
/// # Arguments
///
/// * `editor` - Active editable field state.
/// * `backend` - Target [`TerminalBackend`].
fn render_editor_cells(
    editor: &ActiveFieldEditor<'_>,
    backend: &mut dyn TerminalBackend,
) -> Result<()> {
    let flags = editor.field.attrs.flags;
    let prompt_ch = editor.field.attrs.effective_prompt().map(|p| p.character);
    let mut rendered = String::with_capacity(editor.chars.len());
    for &ch in &editor.chars {
        let out_ch = if ch == ' ' {
            prompt_ch.unwrap_or(' ')
        } else if flags.contains(ScreenAttrFlags::SECURE) {
            '*'
        } else {
            ch
        };
        rendered.push(out_ch);
    }
    let _ = backend.write_span(editor.field.pos, &rendered, editor.field.attrs.cell_style())?;
    Ok(())
}

/// Resolves the initial `(field_idx, char_offset)` for `ACCEPT` given an optional `CURSOR` coordinate.
///
/// # Arguments
///
/// * `editors` - Non-empty slice of editable field buffers.
/// * `requested` - Optional requested [`CursorPosition`].
fn resolve_initial_focus(
    editors: &[ActiveFieldEditor<'_>],
    requested: Option<CursorPosition>,
) -> (usize, usize) {
    let Some(pos) = requested else {
        return (0, 0);
    };
    for (idx, ed) in editors.iter().enumerate() {
        let start_col = ed.field.pos.col.get();
        let end_col = start_col.saturating_add(ed.field.width);
        if pos.row == ed.field.pos.row && pos.col.get() >= start_col && pos.col.get() < end_col {
            let offset = (pos.col.get() - start_col) as usize;
            return (idx, offset);
        }
    }
    (0, 0)
}

/// Computes the 1-based screen [`CursorPosition`] for `(field_idx, char_offset)`.
///
/// # Arguments
///
/// * `editors` - Slice of active field editors.
/// * `field_idx` - Active field index.
/// * `char_offset` - Intra-field character offset.
fn editor_cursor_pos(
    editors: &[ActiveFieldEditor<'_>],
    field_idx: usize,
    char_offset: usize,
) -> CursorPosition {
    let mut out = CursorPosition::HOME;
    for ed in editors.iter().skip(field_idx).take(1) {
        let delta = u16::try_from(char_offset).unwrap_or(0);
        let col = ed.field.pos.col.advance_clamped(delta, ScreenCol::MAX);
        out = CursorPosition::new(ed.field.pos.row, col);
    }
    out
}

/// Returns the maximum valid intra-field character index (`width - 1`) for `field_idx`.
///
/// # Arguments
///
/// * `editors` - Slice of active field editors.
/// * `field_idx` - Active field index.
fn editor_last_cell_offset(editors: &[ActiveFieldEditor<'_>], field_idx: usize) -> usize {
    let mut last = 0_usize;
    for ed in editors.iter().skip(field_idx).take(1) {
        last = ed.chars.len().saturating_sub(1);
    }
    last
}

/// Computes the `End` key cursor offset (immediately after the last non-space character, clamped to `width - 1`).
///
/// # Arguments
///
/// * `editors` - Slice of active field editors.
/// * `field_idx` - Active field index.
fn editor_end_offset(editors: &[ActiveFieldEditor<'_>], field_idx: usize) -> usize {
    let mut target = 0_usize;
    for ed in editors.iter().skip(field_idx).take(1) {
        let max_off = ed.chars.len().saturating_sub(1);
        let after_non_space = ed
            .chars
            .iter()
            .rposition(|&c| c != ' ')
            .map_or(0, |idx| idx.saturating_add(1));
        target = after_non_space.min(max_off);
    }
    target
}

/// Computes the `Alt+Left` previous-word offset within `field_idx`.
///
/// # Arguments
///
/// * `editors` - Slice of active field editors.
/// * `field_idx` - Active field index.
/// * `char_offset` - Current character offset.
fn editor_word_left(
    editors: &[ActiveFieldEditor<'_>],
    field_idx: usize,
    char_offset: usize,
) -> usize {
    let mut idx = char_offset;
    for ed in editors.iter().skip(field_idx).take(1) {
        while idx > 0 && ed.chars.get(idx - 1).copied() == Some(' ') {
            idx -= 1;
        }
        while idx > 0 && ed.chars.get(idx - 1).copied() != Some(' ') {
            idx -= 1;
        }
    }
    idx
}

/// Computes the `Alt+Right` next-word offset within `field_idx`.
///
/// # Arguments
///
/// * `editors` - Slice of active field editors.
/// * `field_idx` - Active field index.
/// * `char_offset` - Current character offset.
fn editor_word_right(
    editors: &[ActiveFieldEditor<'_>],
    field_idx: usize,
    char_offset: usize,
) -> usize {
    let mut idx = char_offset;
    for ed in editors.iter().skip(field_idx).take(1) {
        let max_off = ed.chars.len().saturating_sub(1);
        while idx < max_off && ed.chars.get(idx).copied() != Some(' ') {
            idx += 1;
        }
        while idx < max_off && ed.chars.get(idx).copied() == Some(' ') {
            idx += 1;
        }
    }
    idx
}

/// Deletes the character at `offset`, shifting trailing characters left by 1 and padding with `' '`.
///
/// # Arguments
///
/// * `chars` - Mutable field character buffer.
/// * `offset` - Index of character to delete.
fn delete_char_at(chars: &mut [char], offset: usize) {
    let tail: Vec<char> = chars
        .iter()
        .skip(offset.saturating_add(1))
        .copied()
        .collect();
    for (slot, next_ch) in chars
        .iter_mut()
        .skip(offset)
        .zip(tail.into_iter().chain(core::iter::once(' ')))
    {
        *slot = next_ch;
    }
}

/// Inserts or overwrites `ch` at `char_offset` inside `editors[field_idx]` and redraws the field.
///
/// # Arguments
///
/// * `editors` - Mutable slice of active field editors.
/// * `field_idx` - Active field index.
/// * `char_offset` - Intra-field character index.
/// * `ch` - Printable character typed.
/// * `insert_mode` - [`EditInsertMode::Overwrite`] vs [`EditInsertMode::Insert`].
/// * `backend` - Target [`TerminalBackend`].
fn apply_char_input(
    editors: &mut [ActiveFieldEditor<'_>],
    field_idx: usize,
    char_offset: usize,
    ch: char,
    insert_mode: EditInsertMode,
    backend: &mut dyn TerminalBackend,
) -> Result<(AutoAdvancePolicy, usize)> {
    let clean = sanitize_screen_char(ch);
    let mut auto_mode = AutoAdvancePolicy::Manual;
    let mut max_off = 0_usize;
    for ed in editors.iter_mut().skip(field_idx).take(1) {
        max_off = ed.chars.len().saturating_sub(1);
        if insert_mode == EditInsertMode::Insert {
            let shifted: Vec<char> = core::iter::once(clean)
                .chain(ed.chars.iter().skip(char_offset).copied())
                .collect();
            for (slot, val) in ed.chars.iter_mut().skip(char_offset).zip(shifted) {
                *slot = val;
            }
        } else {
            for slot in ed.chars.iter_mut().skip(char_offset).take(1) {
                *slot = clean;
            }
        }
        if ed.field.attrs.flags.contains(ScreenAttrFlags::AUTO) {
            auto_mode = AutoAdvancePolicy::AutoEnabled;
        }
        render_editor_cells(ed, backend)?;
    }
    Ok((auto_mode, max_off))
}

/// Validates [`ScreenAttrFlags::REQUIRED`] and [`ScreenAttrFlags::FULL`] constraints on `editors[field_idx]`.
///
/// # Arguments
///
/// * `editors` - Slice of active field editors.
/// * `field_idx` - Editor index to validate.
fn is_editor_valid(editors: &[ActiveFieldEditor<'_>], field_idx: usize) -> bool {
    let mut valid = true;
    for ed in editors.iter().skip(field_idx).take(1) {
        valid = validate_editor_buffer(ed);
    }
    valid
}

/// Returns the index of the first editable field failing `REQUIRED` or `FULL` validation, if any.
///
/// # Arguments
///
/// * `editors` - Slice of active field editors.
fn first_invalid_editor(editors: &[ActiveFieldEditor<'_>]) -> Option<usize> {
    editors.iter().position(|ed| !validate_editor_buffer(ed))
}

/// Checks `REQUIRED` (non-empty and non-zero for numeric) and `FULL` (either all empty or all filled) on `ed`.
///
/// # Arguments
///
/// * `ed` - Active editable field buffer.
fn validate_editor_buffer(ed: &ActiveFieldEditor<'_>) -> bool {
    let flags = ed.field.attrs.flags;
    let non_space_count = ed.chars.iter().filter(|&&c| c != ' ').count();
    let is_numeric = matches!(
        ed.pic.move_desc().category(),
        DataCategory::Numeric | DataCategory::NumericEdited
    );
    if flags.contains(ScreenAttrFlags::REQUIRED) {
        if non_space_count == 0 {
            return false;
        }
        if is_numeric && is_displayed_zero(&ed.chars) {
            return false;
        }
    }
    !(flags.contains(ScreenAttrFlags::FULL)
        && non_space_count > 0
        && non_space_count < ed.chars.len())
}

/// Commits all editable field buffers back into `storage` using COBOL `MOVE` / numeric de-edit semantics.
///
/// # Arguments
///
/// * `editors` - Slice of active field editors.
/// * `storage` - Mutable COBOL [`FieldStorage`].
/// * `exceptions` - Mutable `EC-SCREEN-*` accumulator.
fn commit_all_editors(
    editors: &[ActiveFieldEditor<'_>],
    storage: &mut dyn FieldStorage,
    exceptions: &mut Vec<ScreenExceptionKind>,
) -> Result<()> {
    for ed in editors {
        commit_single_editor(ed, storage, exceptions)?;
    }
    Ok(())
}

/// Writes a single [`ActiveFieldEditor`] buffer into its bound `TO` or `USING` target item in `storage`.
///
/// # Arguments
///
/// * `ed` - Active editable field buffer.
/// * `storage` - Mutable COBOL [`FieldStorage`].
/// * `exceptions` - Mutable `EC-SCREEN-*` accumulator.
fn commit_single_editor(
    ed: &ActiveFieldEditor<'_>,
    storage: &mut dyn FieldStorage,
    exceptions: &mut Vec<ScreenExceptionKind>,
) -> Result<()> {
    let target_id = ed.target_id;
    let pic = ed.pic;
    let flags = ed.field.attrs.flags;
    let raw_str: String = ed.chars.iter().copied().collect();

    let dst_bytes = storage.write_field_mut(target_id)?;
    let cat = pic.move_desc().category();
    if matches!(cat, DataCategory::Numeric | DataCategory::NumericEdited) {
        let trimmed = raw_str.trim();
        let dec = if trimmed.is_empty() {
            FixedDecimal::ZERO
        } else if let Ok(val) = de_edit_to_decimal(
            trimmed.as_bytes(),
            pic.move_desc().decimal_char,
            pic.move_desc().currency_symbol,
        ) {
            val
        } else {
            exceptions.push(ScreenExceptionKind::ImplementorCondition);
            FixedDecimal::ZERO
        };
        execute_move(MoveSource::Decimal(dec), dst_bytes, pic.move_desc())
    } else {
        let effective_desc = apply_right_justification_override(pic.move_desc(), flags);
        let src_slice = if flags.contains(ScreenAttrFlags::JUSTIFIED_RIGHT) {
            raw_str.trim_end_matches(' ').as_bytes()
        } else {
            raw_str.as_bytes()
        };
        execute_move(
            MoveSource::AlphanumericLiteral(src_slice),
            dst_bytes,
            &effective_desc,
        )?;
        if flags.contains(ScreenAttrFlags::ZERO_FILL) {
            for b in dst_bytes.iter_mut() {
                if *b == b' ' {
                    *b = b'0';
                }
            }
        }
        Ok(())
    }
}

/// Returns a copy of `desc` overriding alphanumeric justification when [`ScreenAttrFlags::JUSTIFIED_RIGHT`] is active.
///
/// # Arguments
///
/// * `desc` - Base [`MoveFieldDesc`].
/// * `flags` - Active [`ScreenAttrFlags`].
fn apply_right_justification_override(
    desc: &MoveFieldDesc,
    flags: ScreenAttrFlags,
) -> MoveFieldDesc {
    let mut out = desc.clone();
    if flags.contains(ScreenAttrFlags::JUSTIFIED_RIGHT)
        && let HirType::Alphanumeric { byte_len, .. } = out.ty
    {
        out.ty = HirType::Alphanumeric {
            byte_len,
            justified: JustifiedSpec::Right,
        };
    }
    out
}
