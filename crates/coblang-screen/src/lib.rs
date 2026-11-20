//! Pure-Rust `SCREEN SECTION` TUI layout compiler, field attribute model, `AnsiVt100Backend`,
//! and deterministic `24x80` `VirtualGridBackend` (`Milestone M19`).

/// Safe [`TerminalBackend`] trait, [`VirtualGridBackend`], and [`AnsiVt100Backend`] TTY/PTY driver.
pub mod backend;
/// `SCREEN SECTION` `DISPLAY` and interactive `ACCEPT` engine updating `CRT STATUS` and `CURSOR`.
pub mod engine;
/// Hierarchical `SCREEN SECTION` coordinate/inheritance resolver and [`ScreenLayout`] model.
pub mod layout;
/// Validated screen coordinate, color, `CRT STATUS`, `CURSOR`, keystroke, and attribute domain types.
pub mod types;

pub use backend::{
    AnsiVt100Backend, ScreenCell, TerminalBackend, VirtualGridBackend, decode_next_key_event,
    format_ansi_sgr, translate_cell_char,
};
pub use engine::{
    AcceptSummary, EditInsertMode, FieldStorage, MapFieldStorage, ScreenEngine,
    format_field_for_screen_display,
};
pub use layout::{
    AtCompactSpec, ColSpec, LevelScreenEntry, LineSpec, LiteralTileMode, ResolvedScreenField,
    ResolvedScreenStep, ScreenAttributeSet, ScreenDataBinding, ScreenFieldPic, ScreenFieldSpec,
    ScreenLayout, ScreenNode,
};
pub use types::{
    CellStyle, ColorIntensity, CrtStatusCode, CursorFormat, CursorPosition, CursorVisibility,
    KeyEvent, PromptSpec, ScreenAttrFlags, ScreenCol, ScreenColor, ScreenDimensions,
    ScreenExceptionKind, ScreenRow,
};
