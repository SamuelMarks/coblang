//! Exhaustive unit, integration, and 100% branch-coverage test suite for `coblang-screen`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Cursor, Read, Write};
use std::rc::Rc;

use coblang_common::{
    CoblangError, DataItemId, DigitCount, LevelNumber, Result, ScaleFactor, SymbolId,
};
use coblang_hir::{BlankZeroSpec, HirType, NumericSignSpec, PicEditOp, Signedness};
use coblang_pic::{
    CurrencySignSpec, DecimalPointKind, MoveFieldDesc, NumericPictureMeta, PictureProgram,
};
use coblang_rt_core::exceptions::ExceptionCode;
use coblang_screen::{
    AnsiVt100Backend, AtCompactSpec, CellStyle, ColSpec, ColorIntensity, CrtStatusCode,
    CursorFormat, CursorPosition, CursorVisibility, EditInsertMode, FieldStorage, KeyEvent,
    LevelScreenEntry, LineSpec, LiteralTileMode, MapFieldStorage, PromptSpec, ResolvedScreenField,
    ScreenAttrFlags, ScreenAttributeSet, ScreenCell, ScreenCol, ScreenColor, ScreenDataBinding,
    ScreenDimensions, ScreenEngine, ScreenExceptionKind, ScreenFieldPic, ScreenFieldSpec,
    ScreenLayout, ScreenNode, ScreenRow, TerminalBackend, VirtualGridBackend,
    decode_next_key_event, format_ansi_sgr, format_field_for_screen_display, translate_cell_char,
};

/// Shared byte buffer writer with configurable step-count I/O fault injection.
#[derive(Clone)]
struct StepWriter {
    /// Captured output bytes.
    buf: Rc<RefCell<Vec<u8>>>,
    /// Current operation counter (`write` + `flush`).
    calls: Rc<RefCell<usize>>,
    /// Optional 0-based call index at which to return an `io::Error`.
    fail_on_call: Option<usize>,
}

impl StepWriter {
    /// Creates an infallible [`StepWriter`] capturing output into an internal buffer.
    fn capture() -> (Self, Rc<RefCell<Vec<u8>>>) {
        let buf = Rc::new(RefCell::new(Vec::new()));
        let writer = Self {
            buf: Rc::clone(&buf),
            calls: Rc::new(RefCell::new(0)),
            fail_on_call: None,
        };
        (writer, buf)
    }

    /// Creates a [`StepWriter`] that fails on `call_idx`.
    fn failing_at(call_idx: usize) -> Self {
        Self {
            buf: Rc::new(RefCell::new(Vec::new())),
            calls: Rc::new(RefCell::new(0)),
            fail_on_call: Some(call_idx),
        }
    }

    /// Increments call counter and returns `Err` when `fail_on_call` matches.
    fn check_step(&self) -> io::Result<()> {
        let cur = *self.calls.borrow();
        *self.calls.borrow_mut() = cur.saturating_add(1);
        if self.fail_on_call == Some(cur) {
            Err(io::Error::other("injected writer fault"))
        } else {
            Ok(())
        }
    }
}

impl Write for StepWriter {
    /// Appends `data` unless `fail_on_call` triggers on this step.
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.check_step()?;
        self.buf.borrow_mut().extend_from_slice(data);
        Ok(data.len())
    }

    /// Flushes output unless `fail_on_call` triggers on this step.
    fn flush(&mut self) -> io::Result<()> {
        self.check_step()
    }
}

/// Byte stream reader with configurable step-count I/O fault injection.
struct StepReader {
    /// Queued input bytes.
    data: VecDeque<u8>,
    /// Current `read` call index.
    calls: usize,
    /// Optional 0-based `read` call index at which to fail.
    fail_on_call: Option<usize>,
}

impl StepReader {
    /// Creates a [`StepReader`] over `bytes` that fails on `fail_on_call` (`None` for never).
    fn new(bytes: &[u8], fail_on_call: Option<usize>) -> Self {
        Self {
            data: bytes.iter().copied().collect(),
            calls: 0,
            fail_on_call,
        }
    }
}

impl Read for StepReader {
    /// Reads at most one byte into `buf` unless `fail_on_call` triggers.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let cur = self.calls;
        self.calls = cur.saturating_add(1);
        if self.fail_on_call == Some(cur) {
            return Err(io::Error::other("injected reader fault"));
        }
        let Some(slot) = buf.first_mut() else {
            return Ok(0);
        };
        let Some(next) = self.data.pop_front() else {
            return Ok(0);
        };
        *slot = next;
        Ok(1)
    }
}

/// [`TerminalBackend`] wrapper injecting deterministic backend errors on call `fail_at`.
struct FaultyBackend {
    /// Underlying [`VirtualGridBackend`].
    inner: VirtualGridBackend,
    /// Current fallible call index.
    calls: usize,
    /// Call index that triggers [`CoblangError::ScreenBackend`].
    fail_at: usize,
}

impl FaultyBackend {
    /// Creates a [`FaultyBackend`] wrapping `inner` and failing on `fail_at`.
    fn new(inner: VirtualGridBackend, fail_at: usize) -> Self {
        Self {
            inner,
            calls: 0,
            fail_at,
        }
    }

    /// Checks whether the current backend operation should fail.
    fn step(&mut self) -> Result<()> {
        let cur = self.calls;
        self.calls = cur.saturating_add(1);
        if cur == self.fail_at {
            Err(CoblangError::ScreenBackend {
                message: format!("injected backend failure at step {cur}"),
            })
        } else {
            Ok(())
        }
    }
}

impl TerminalBackend for FaultyBackend {
    /// Delegates [`TerminalBackend::dimensions`] to `self.inner`.
    fn dimensions(&self) -> ScreenDimensions {
        self.inner.dimensions()
    }

    /// Delegates [`TerminalBackend::cursor`] to `self.inner`.
    fn cursor(&self) -> CursorPosition {
        self.inner.cursor()
    }

    /// Delegates [`TerminalBackend::cursor_visibility`] to `self.inner`.
    fn cursor_visibility(&self) -> CursorVisibility {
        self.inner.cursor_visibility()
    }

    /// Injects fault if scheduled, otherwise delegates [`TerminalBackend::set_cursor`].
    fn set_cursor(&mut self, pos: CursorPosition) -> Result<()> {
        self.step()?;
        self.inner.set_cursor(pos)
    }

    /// Injects fault if scheduled, otherwise delegates [`TerminalBackend::set_cursor_visibility`].
    fn set_cursor_visibility(&mut self, visibility: CursorVisibility) -> Result<()> {
        self.step()?;
        self.inner.set_cursor_visibility(visibility)
    }

    /// Injects fault if scheduled, otherwise delegates [`TerminalBackend::clear_screen`].
    fn clear_screen(&mut self, style: CellStyle) -> Result<()> {
        self.step()?;
        self.inner.clear_screen(style)
    }

    /// Injects fault if scheduled, otherwise delegates [`TerminalBackend::clear_line`].
    fn clear_line(&mut self, row: ScreenRow, style: CellStyle) -> Result<()> {
        self.step()?;
        self.inner.clear_line(row, style)
    }

    /// Injects fault if scheduled, otherwise delegates [`TerminalBackend::erase_to_end_of_line`].
    fn erase_to_end_of_line(&mut self, pos: CursorPosition, style: CellStyle) -> Result<()> {
        self.step()?;
        self.inner.erase_to_end_of_line(pos, style)
    }

    /// Injects fault if scheduled, otherwise delegates [`TerminalBackend::erase_to_end_of_screen`].
    fn erase_to_end_of_screen(&mut self, pos: CursorPosition, style: CellStyle) -> Result<()> {
        self.step()?;
        self.inner.erase_to_end_of_screen(pos, style)
    }

    /// Injects fault if scheduled, otherwise delegates [`TerminalBackend::ring_bell`].
    fn ring_bell(&mut self) -> Result<()> {
        self.step()?;
        self.inner.ring_bell()
    }

    /// Injects fault if scheduled, otherwise delegates [`TerminalBackend::write_span`].
    fn write_span(
        &mut self,
        pos: CursorPosition,
        text: &str,
        style: CellStyle,
    ) -> Result<CursorPosition> {
        self.step()?;
        self.inner.write_span(pos, text, style)
    }

    /// Injects fault if scheduled, otherwise delegates [`TerminalBackend::flush`].
    fn flush(&mut self) -> Result<()> {
        self.step()?;
        self.inner.flush()
    }

    /// Injects fault if scheduled, otherwise delegates [`TerminalBackend::read_key`].
    fn read_key(&mut self) -> Result<KeyEvent> {
        self.step()?;
        self.inner.read_key()
    }
}

/// [`FieldStorage`] wrapper that succeeds on `read_field` and fails on `write_field_mut` when configured.
struct ReadOnlyFaultStorage {
    /// Backing storage map.
    inner: MapFieldStorage,
}

impl FieldStorage for ReadOnlyFaultStorage {
    /// Reads field bytes from `self.inner`.
    fn read_field(&self, id: DataItemId) -> Result<&[u8]> {
        self.inner.read_field(id)
    }

    /// Always returns [`CoblangError::ScreenBackend`] to test write-back failure propagation.
    fn write_field_mut(&mut self, _id: DataItemId) -> Result<&mut [u8]> {
        Err(CoblangError::ScreenBackend {
            message: String::from("read-only fault storage"),
        })
    }
}

/// Verifies `ScreenRow`, `ScreenCol`, `ScreenDimensions`, and `ScreenColor` domain bounds and codecs.
#[test]
fn domain_coordinates_dimensions_and_colors() -> Result<()> {
    assert!(ScreenRow::try_new(0).is_err());
    assert!(ScreenRow::try_new(1000).is_err());
    let r5 = ScreenRow::try_new(5)?;
    let r10 = ScreenRow::try_new(10)?;
    assert_eq!(r5.get(), 5);
    assert_eq!(r5.zero_based_index(), 4);
    assert_eq!(r5.clamp_max(r10), r5);
    assert_eq!(r10.clamp_max(r5), r5);
    assert_eq!(r5.advance_clamped(3, r10).get(), 8);
    assert_eq!(r5.advance_clamped(20, r10), r10);
    assert_eq!(r5.retreat_clamped(2).get(), 3);
    assert_eq!(r5.retreat_clamped(9), ScreenRow::ONE);

    assert!(ScreenCol::try_new(0).is_err());
    assert!(ScreenCol::try_new(1000).is_err());
    let c4 = ScreenCol::try_new(4)?;
    let c12 = ScreenCol::try_new(12)?;
    assert_eq!(c4.get(), 4);
    assert_eq!(c4.zero_based_index(), 3);
    assert_eq!(c4.clamp_max(c12), c4);
    assert_eq!(c12.clamp_max(c4), c4);
    assert_eq!(c4.advance_clamped(5, c12).get(), 9);
    assert_eq!(c4.advance_clamped(25, c12), c12);
    assert_eq!(c4.retreat_clamped(2).get(), 2);
    assert_eq!(c4.retreat_clamped(9), ScreenCol::ONE);

    let custom_dims = ScreenDimensions::new(r10, c12);
    assert_eq!(custom_dims.cell_count(), 120);

    let dims_primary = ScreenDimensions::from_env_lookup(&mut |k| match k {
        "COB_SCREEN_LINES" => Some(String::from("43")),
        "COB_SCREEN_COLS" => Some(String::from("132")),
        _ => None,
    });
    assert_eq!(
        (dims_primary.rows.get(), dims_primary.cols.get()),
        (43, 132)
    );

    let dims_fallback = ScreenDimensions::from_env_lookup(&mut |k| match k {
        "LINES" => Some(String::from(" 25 ")),
        "COLUMNS" => Some(String::from("100")),
        _ => None,
    });
    assert_eq!(
        (dims_fallback.rows.get(), dims_fallback.cols.get()),
        (25, 100)
    );

    let dims_invalid = ScreenDimensions::from_env_lookup(&mut |k| match k {
        "COB_SCREEN_LINES" => Some(String::from("0")),
        "COB_SCREEN_COLS" => Some(String::from("not_a_number")),
        _ => None,
    });
    assert_eq!(dims_invalid, ScreenDimensions::STANDARD_24X80);
    assert_eq!(
        ScreenDimensions::from_env_lookup(&mut |_| None),
        ScreenDimensions::STANDARD_24X80
    );

    for code in 0_u8..=7 {
        let color = ScreenColor::from_cobol_code(code)?;
        assert_eq!(color.to_cobol_code(), code);
        assert!(color.ansi_fg_sgr() >= 30 && color.ansi_fg_sgr() <= 37);
        assert_eq!(color.ansi_bg_sgr(), color.ansi_fg_sgr() + 10);
    }
    assert!(ScreenColor::from_cobol_code(8).is_err());

    for ext in 0_u8..=15 {
        let (base, intensity) = ScreenColor::from_extended_code(ext)?;
        assert_eq!(base.to_cobol_code(), ext & 0x07);
        let exp_int = if ext < 8 {
            ColorIntensity::Standard
        } else {
            ColorIntensity::High
        };
        assert_eq!(intensity, exp_int);
    }
    assert!(ScreenColor::from_extended_code(16).is_err());

    for (kw, expected) in [
        ("black", ScreenColor::Black),
        ("BLUE", ScreenColor::Blue),
        ("Green", ScreenColor::Green),
        ("CYAN", ScreenColor::Cyan),
        ("RED", ScreenColor::Red),
        ("MAGENTA", ScreenColor::Magenta),
        ("BROWN", ScreenColor::Brown),
        ("YELLOW", ScreenColor::Brown),
        ("WHITE", ScreenColor::White),
    ] {
        assert_eq!(ScreenColor::from_keyword(kw)?, expected);
    }
    assert!(ScreenColor::from_keyword("CHARTREUSE").is_err());
    Ok(())
}

/// Verifies `CrtStatusCode`, `ScreenAttrFlags`, and `PromptSpec` operations.
#[test]
fn domain_crt_status_and_attribute_flags() -> Result<()> {
    assert!(CrtStatusCode::try_new(10_000).is_err());
    let st_ok = CrtStatusCode::try_new(2005)?;
    assert_eq!(st_ok, CrtStatusCode::ESCAPE);
    assert_eq!(st_ok.get(), 2005);
    assert_eq!(&st_ok.to_four_ascii_digits(), b"2005");
    assert!(CrtStatusCode::function_key(0).is_err());
    assert!(CrtStatusCode::function_key(65).is_err());
    let f12 = CrtStatusCode::function_key(12)?;
    assert_eq!(&f12.to_four_ascii_digits(), b"1012");

    assert_eq!(
        CrtStatusCode::NORMAL_COMPLETION.to_xopen_three_bytes(),
        [b'0', b'0', 0]
    );
    assert_eq!(f12.to_xopen_three_bytes(), [b'1', 12, 0]);
    assert_eq!(CrtStatusCode::ESCAPE.to_xopen_three_bytes(), [b'2', 5, 0]);
    assert_eq!(CrtStatusCode::TIMEOUT.to_xopen_three_bytes(), [b'9', 8, 0]);

    let mut short_reg = [0_u8; 2];
    assert!(f12.write_to_register(&mut short_reg).is_err());
    let mut xopen_reg = [0_u8; 3];
    f12.write_to_register(&mut xopen_reg)?;
    assert_eq!(xopen_reg, [b'1', 12, 0]);
    let mut std_reg = [0_u8; 4];
    f12.write_to_register(&mut std_reg)?;
    assert_eq!(&std_reg, b"1012");
    let mut wide_reg = [b'X'; 6];
    f12.write_to_register(&mut wide_reg)?;
    assert_eq!(&wide_reg, b"1012  ");

    let mut flags = ScreenAttrFlags::empty();
    assert!(flags.is_empty());
    flags.insert(ScreenAttrFlags::HIGHLIGHT);
    assert!(!flags.is_empty());
    assert!(flags.contains(ScreenAttrFlags::HIGHLIGHT));
    flags.remove(ScreenAttrFlags::HIGHLIGHT);
    assert!(flags.is_empty());
    let parent_hi = ScreenAttrFlags::HIGHLIGHT.union(ScreenAttrFlags::BLANK_SCREEN);
    let child_lo = parent_hi.inherit_into_child(ScreenAttrFlags::LOWLIGHT);
    assert!(child_lo.contains(ScreenAttrFlags::LOWLIGHT));
    assert!(!child_lo.contains(ScreenAttrFlags::HIGHLIGHT));
    assert!(!child_lo.contains(ScreenAttrFlags::BLANK_SCREEN));
    let child_hi = child_lo.inherit_into_child(ScreenAttrFlags::HIGHLIGHT);
    assert!(child_hi.contains(ScreenAttrFlags::HIGHLIGHT));
    assert!(!child_hi.contains(ScreenAttrFlags::LOWLIGHT));
    let child_keep = child_hi.inherit_into_child(ScreenAttrFlags::UNDERLINE);
    assert!(child_keep.contains(ScreenAttrFlags::HIGHLIGHT));
    assert!(child_keep.contains(ScreenAttrFlags::UNDERLINE));

    for kw in [
        "BELL",
        "BEEP",
        "HIGHLIGHT",
        "LOWLIGHT",
        "REVERSE-VIDEO",
        "REVERSE",
        "UNDERLINE",
        "BLINK",
        "LEFTLINE",
        "OVERLINE",
        "AUTO",
        "AUTO-SKIP",
        "AUTOTERMINATE",
        "SECURE",
        "NO-ECHO",
        "REQUIRED",
        "EMPTY-CHECK",
        "FULL",
        "LENGTH-CHECK",
        "PROMPT",
        "ZERO-FILL",
        "JUSTIFIED",
        "JUST",
        "UPDATE",
        "INITIAL",
        "GRAPHICS",
    ] {
        assert!(ScreenAttrFlags::from_keyword(kw).is_some());
    }
    assert!(ScreenAttrFlags::from_keyword("UNKNOWN-ATTR").is_none());

    assert_eq!(PromptSpec::default(), PromptSpec::UNDERSCORE);
    assert_eq!(PromptSpec::new('.').character, '.');
    assert_eq!(PromptSpec::new('\x00'), PromptSpec::UNDERSCORE);
    assert_eq!(CellStyle::default(), CellStyle::DEFAULT);
    Ok(())
}

/// Verifies `CursorPosition`, `CursorFormat`, `KeyEvent`, and `ScreenExceptionKind`.
#[test]
fn domain_cursor_codecs_key_events_and_exceptions() -> Result<()> {
    let r5 = ScreenRow::try_new(5)?;
    let c4 = ScreenCol::try_new(4)?;
    assert_eq!(CursorPosition::default(), CursorPosition::HOME);
    assert!(CursorPosition::try_from_raw(0, 1).is_err());
    assert!(CursorPosition::try_from_raw(1, 0).is_err());
    let big_pos = CursorPosition::try_from_raw(150, 200)?;
    assert_eq!(
        big_pos.clamp_to(ScreenDimensions::STANDARD_24X80),
        CursorPosition::try_from_raw(24, 80)?
    );

    let mut cur4 = [0_u8; 4];
    let mut cur6 = [0_u8; 6];
    assert!(
        CursorFormat::FourDigitLlCc
            .encode(big_pos, &mut [0_u8; 3])
            .is_err()
    );
    assert!(
        CursorFormat::SixDigitLllCcc
            .encode(big_pos, &mut [0_u8; 5])
            .is_err()
    );
    CursorFormat::FourDigitLlCc.encode(big_pos, &mut cur4)?;
    assert_eq!(&cur4, b"9999");
    let small_pos = CursorPosition::try_from_raw(7, 15)?;
    CursorFormat::FourDigitLlCc.encode(small_pos, &mut cur4)?;
    assert_eq!(&cur4, b"0715");
    CursorFormat::SixDigitLllCcc.encode(big_pos, &mut cur6)?;
    assert_eq!(&cur6, b"150200");

    assert!(CursorFormat::FourDigitLlCc.decode(b"12").is_err());
    assert!(CursorFormat::SixDigitLllCcc.decode(b"1234").is_err());
    assert_eq!(CursorFormat::FourDigitLlCc.decode(b"0015")?, None);
    assert_eq!(CursorFormat::FourDigitLlCc.decode(b"0700")?, None);
    assert_eq!(CursorFormat::FourDigitLlCc.decode(b"AB15")?, None);
    assert_eq!(CursorFormat::FourDigitLlCc.decode(b"07CD")?, None);
    assert_eq!(
        CursorFormat::FourDigitLlCc.decode(b" 715")?,
        Some(small_pos)
    );
    assert_eq!(
        CursorFormat::SixDigitLllCcc.decode(b"150200")?,
        Some(big_pos)
    );

    for (ev, code) in [
        (KeyEvent::Char('A'), CrtStatusCode::NORMAL_COMPLETION),
        (KeyEvent::Enter, CrtStatusCode::NORMAL_COMPLETION),
        (KeyEvent::Escape, CrtStatusCode::ESCAPE),
        (KeyEvent::Tab, CrtStatusCode::TAB_FORWARD),
        (KeyEvent::BackTab, CrtStatusCode::BACK_TAB),
        (KeyEvent::Backspace, CrtStatusCode::BACKSPACE_KEY),
        (KeyEvent::Delete, CrtStatusCode::DELETE_KEY),
        (KeyEvent::AltDelete, CrtStatusCode::DELETE_KEY),
        (KeyEvent::Insert, CrtStatusCode::INSERT_KEY),
        (KeyEvent::Home, CrtStatusCode::HOME_KEY),
        (KeyEvent::End, CrtStatusCode::END_KEY),
        (KeyEvent::Left, CrtStatusCode::LEFT_ARROW),
        (KeyEvent::AltLeft, CrtStatusCode::LEFT_ARROW),
        (KeyEvent::Right, CrtStatusCode::RIGHT_ARROW),
        (KeyEvent::AltRight, CrtStatusCode::RIGHT_ARROW),
        (KeyEvent::Up, CrtStatusCode::UP_ARROW),
        (KeyEvent::Down, CrtStatusCode::DOWN_ARROW),
        (KeyEvent::PageUp, CrtStatusCode::PAGE_UP),
        (KeyEvent::PageDown, CrtStatusCode::PAGE_DOWN),
        (KeyEvent::Function(3), CrtStatusCode::function_key(3)?),
        (KeyEvent::Function(99), CrtStatusCode::SCREEN_ERROR),
        (KeyEvent::Timeout, CrtStatusCode::TIMEOUT),
        (KeyEvent::Eof, CrtStatusCode::INPUT_EOF),
    ] {
        assert_eq!(ev.to_crt_status(), code);
    }

    assert_eq!(
        ScreenExceptionKind::LineOutOfBounds {
            requested_row: 30,
            max_row: 24
        }
        .exception_code(),
        ExceptionCode::EcScreenLineNumber
    );
    assert_eq!(
        ScreenExceptionKind::StartingColumnOutOfBounds {
            requested_col: 90,
            max_col: 80
        }
        .exception_code(),
        ExceptionCode::EcScreenStartingColumn
    );
    assert_eq!(
        ScreenExceptionKind::FieldOverlap { row: r5, col: c4 }.exception_code(),
        ExceptionCode::EcScreenFieldOverlap
    );
    assert_eq!(
        ScreenExceptionKind::ImplementorCondition.exception_code(),
        ExceptionCode::EcScreenImp
    );
    Ok(())
}

/// Verifies `AtCompactSpec`, `ScreenDataBinding`, and `ScreenAttributeSet` inheritance/override rules.
#[test]
fn layout_bindings_compact_specs_and_inheritance() -> Result<()> {
    assert!(AtCompactSpec::from_four_digit_llcc(10_000).is_err());
    let at_zero = AtCompactSpec::from_four_digit_llcc(0)?;
    assert_eq!((at_zero.line, at_zero.col), (LineSpec::Zero, ColSpec::Zero));
    let at_pos = AtCompactSpec::from_four_digit_llcc(512)?;
    assert_eq!(
        (at_pos.line, at_pos.col),
        (
            LineSpec::Absolute(ScreenRow::try_new(5)?),
            ColSpec::Absolute(ScreenCol::try_new(12)?)
        )
    );

    assert!(AtCompactSpec::from_six_digit_lllccc(1_000_000).is_err());
    let six_digit_zero = AtCompactSpec::from_six_digit_lllccc(0)?;
    assert_eq!(
        (six_digit_zero.line, six_digit_zero.col),
        (LineSpec::Zero, ColSpec::Zero)
    );
    let six_digit_pos = AtCompactSpec::from_six_digit_lllccc(24_080)?;
    assert_eq!(
        (six_digit_pos.line, six_digit_pos.col),
        (
            LineSpec::Absolute(ScreenRow::try_new(24)?),
            ColSpec::Absolute(ScreenCol::try_new(80)?)
        )
    );

    assert!(ScreenFieldPic::alphanumeric(0).is_err());
    assert!(ScreenFieldPic::alphanumeric(1000).is_err());
    let pic5 = ScreenFieldPic::alphanumeric(5)?;
    assert_eq!(pic5.display_width(), 5);
    assert!(pic5.edit_program().is_none());

    let b_act = ScreenDataBinding::ActionOnly;
    assert_eq!(b_act.effective_width(), 0);
    assert!(!b_act.is_editable());
    assert!(b_act.target_field_id().is_none());
    assert!(b_act.field_pic().is_none());

    let b_lit = ScreenDataBinding::Literal {
        bytes: b"HELLO".to_vec(),
        tile: LiteralTileMode::Single,
        explicit_size: None,
    };
    assert_eq!(b_lit.effective_width(), 5);
    assert!(b_lit.target_field_id().is_none());
    assert!(b_lit.field_pic().is_none());

    let b_lit_sized = ScreenDataBinding::Literal {
        bytes: b"-".to_vec(),
        tile: LiteralTileMode::RepeatAll,
        explicit_size: Some(10),
    };
    assert_eq!(b_lit_sized.effective_width(), 10);

    let id1 = DataItemId::new(1);
    let b_from = ScreenDataBinding::From {
        source_id: id1,
        pic: pic5.clone(),
        explicit_size: Some(7),
    };
    assert_eq!(b_from.effective_width(), 7);
    assert!(!b_from.is_editable());
    assert!(b_from.target_field_id().is_none());
    assert!(b_from.field_pic().is_some());

    let b_to = ScreenDataBinding::To {
        target_id: id1,
        pic: pic5,
        explicit_size: None,
    };
    assert_eq!(b_to.effective_width(), 5);
    assert!(b_to.is_editable());
    assert_eq!(b_to.target_field_id(), Some(id1));

    let parent_attrs = ScreenAttributeSet::new()
        .with_flags(ScreenAttrFlags::HIGHLIGHT.union(ScreenAttrFlags::BLANK_SCREEN))
        .with_fg_color(ScreenColor::Brown)
        .with_bg_color(ScreenColor::Blue);
    let child_override = ScreenAttributeSet::new()
        .with_flags(ScreenAttrFlags::LOWLIGHT)
        .with_fg_color(ScreenColor::White)
        .with_prompt(PromptSpec::new('.'));
    let merged = parent_attrs.inherit_into_child(&child_override);
    assert!(merged.flags.contains(ScreenAttrFlags::LOWLIGHT));
    assert!(!merged.flags.contains(ScreenAttrFlags::HIGHLIGHT));
    assert!(merged.flags.contains(ScreenAttrFlags::PROMPT));
    assert_eq!(merged.fg_color, Some(ScreenColor::White));
    assert_eq!(merged.bg_color, Some(ScreenColor::Blue));
    assert_eq!(merged.effective_prompt(), Some(PromptSpec::new('.')));
    assert_eq!(
        ScreenAttributeSet::new()
            .with_flags(ScreenAttrFlags::PROMPT)
            .effective_prompt(),
        Some(PromptSpec::UNDERSCORE)
    );
    assert_eq!(ScreenAttributeSet::new().effective_prompt(), None);
    Ok(())
}

/// Verifies `ScreenLayout::from_level_entries`, `BLANK SCREEN`/`BLANK LINE` resets, and `EC-SCREEN-*` detection.
#[test]
fn layout_level_hierarchies_and_ec_screen_detection() -> Result<()> {
    let pic5 = ScreenFieldPic::alphanumeric(5)?;
    let id1 = DataItemId::new(1);
    let lvl77 = LevelNumber::parse(77)?;
    assert!(
        ScreenLayout::from_level_entries(
            ScreenDimensions::STANDARD_24X80,
            &[LevelScreenEntry {
                level: lvl77,
                name: None,
                line: LineSpec::Omitted,
                col: ColSpec::Omitted,
                attrs: ScreenAttributeSet::new(),
                binding: ScreenDataBinding::ActionOnly,
            }]
        )
        .is_err()
    );

    let lvl01 = LevelNumber::parse(1)?;
    let lvl05 = LevelNumber::parse(5)?;
    let lvl10 = LevelNumber::parse(10)?;
    let entries = vec![
        LevelScreenEntry {
            level: lvl01,
            name: Some(SymbolId::new(0)),
            line: LineSpec::Zero,
            col: ColSpec::Zero,
            attrs: ScreenAttributeSet::new().with_flags(ScreenAttrFlags::BLANK_SCREEN),
            binding: ScreenDataBinding::ActionOnly,
        },
        LevelScreenEntry {
            level: lvl05,
            name: None,
            line: LineSpec::Omitted,
            col: ColSpec::Omitted,
            attrs: ScreenAttributeSet::new(),
            binding: ScreenDataBinding::ActionOnly,
        },
        LevelScreenEntry {
            level: lvl10,
            name: None,
            line: LineSpec::RelativePlus(2),
            col: ColSpec::RelativePlus(3),
            attrs: ScreenAttributeSet::new(),
            binding: ScreenDataBinding::Literal {
                bytes: b"HELLO".to_vec(),
                tile: LiteralTileMode::Single,
                explicit_size: None,
            },
        },
        LevelScreenEntry {
            level: lvl05,
            name: None,
            line: LineSpec::Zero,
            col: ColSpec::Omitted,
            attrs: ScreenAttributeSet::new(),
            binding: ScreenDataBinding::To {
                target_id: id1,
                pic: pic5.clone(),
                explicit_size: None,
            },
        },
        LevelScreenEntry {
            level: lvl01,
            name: None,
            line: LineSpec::RelativeMinus(1),
            col: ColSpec::RelativeMinus(2),
            attrs: ScreenAttributeSet::new().with_flags(ScreenAttrFlags::BLANK_LINE),
            binding: ScreenDataBinding::Using {
                target_id: id1,
                pic: pic5,
                explicit_size: None,
            },
        },
    ];
    let compiled = ScreenLayout::from_level_entries(ScreenDimensions::STANDARD_24X80, &entries)?;
    assert_eq!(compiled.dimensions(), ScreenDimensions::STANDARD_24X80);
    assert_eq!(compiled.fields().len(), 3);
    assert_eq!(compiled.editable_fields().len(), 2);
    assert_eq!(compiled.exceptions(), []);
    Ok(())
}

/// Verifies `EC-SCREEN-FIELD-OVERLAP`, `EC-SCREEN-LINE-NUMBER`, and `EC-SCREEN-STARTING-COLUMN` detection.
#[test]
fn layout_ec_screen_overlap_and_bounds() -> Result<()> {
    let small_dims = ScreenDimensions::new(ScreenRow::try_new(4)?, ScreenCol::try_new(10)?);
    let overlap_nodes = vec![
        ScreenNode::Elementary(ScreenFieldSpec {
            name: None,
            line: LineSpec::Omitted,
            col: ColSpec::Omitted,
            attrs: ScreenAttributeSet::new().with_flags(ScreenAttrFlags::BLANK_SCREEN),
            binding: ScreenDataBinding::Literal {
                bytes: b"ABCDE".to_vec(),
                tile: LiteralTileMode::Single,
                explicit_size: None,
            },
        }),
        ScreenNode::Elementary(ScreenFieldSpec {
            name: None,
            line: LineSpec::Absolute(ScreenRow::try_new(1)?),
            col: ColSpec::Absolute(ScreenCol::try_new(3)?),
            attrs: ScreenAttributeSet::new().with_flags(ScreenAttrFlags::BLANK_SCREEN),
            binding: ScreenDataBinding::Literal {
                bytes: b"XY".to_vec(),
                tile: LiteralTileMode::Single,
                explicit_size: None,
            },
        }),
        ScreenNode::Elementary(ScreenFieldSpec {
            name: None,
            line: LineSpec::Absolute(ScreenRow::try_new(1)?),
            col: ColSpec::Absolute(ScreenCol::try_new(3)?),
            attrs: ScreenAttributeSet::new(),
            binding: ScreenDataBinding::Literal {
                bytes: b"OVERLAP-WIDE".to_vec(),
                tile: LiteralTileMode::Single,
                explicit_size: None,
            },
        }),
        ScreenNode::Group {
            name: None,
            line: LineSpec::Absolute(ScreenRow::try_new(1)?),
            col: ColSpec::Absolute(ScreenCol::try_new(1)?),
            attrs: ScreenAttributeSet::new().with_flags(ScreenAttrFlags::BLANK_LINE),
            children: vec![ScreenNode::Elementary(ScreenFieldSpec {
                name: None,
                line: LineSpec::Absolute(ScreenRow::try_new(9)?),
                col: ColSpec::Absolute(ScreenCol::try_new(15)?),
                attrs: ScreenAttributeSet::new(),
                binding: ScreenDataBinding::ActionOnly,
            })],
        },
    ];
    let ov_layout = ScreenLayout::compile(small_dims, &overlap_nodes);
    assert!(
        ov_layout
            .exceptions()
            .contains(&ScreenExceptionKind::FieldOverlap {
                row: ScreenRow::try_new(1)?,
                col: ScreenCol::try_new(3)?,
            })
    );
    assert!(
        ov_layout
            .exceptions()
            .contains(&ScreenExceptionKind::LineOutOfBounds {
                requested_row: 9,
                max_row: 4,
            })
    );
    assert!(
        ov_layout
            .exceptions()
            .contains(&ScreenExceptionKind::StartingColumnOutOfBounds {
                requested_col: 15,
                max_col: 10,
            })
    );
    Ok(())
}

/// Verifies `VirtualGridBackend` frame buffer operations, SGR formatting, and DEC graphics translation.
#[test]
fn virtual_grid_backend_and_graphics_cells() -> Result<()> {
    assert_eq!(ScreenCell::default(), ScreenCell::BLANK);
    let gfx = ScreenAttrFlags::GRAPHICS;
    for (src, expected) in [
        ('\x00', ' '),
        ('\x7f', ' '),
        ('A', 'A'),
        ('q', '─'),
        ('x', '│'),
        ('l', '┌'),
        ('k', '┐'),
        ('m', '└'),
        ('j', '┘'),
        ('n', '┼'),
        ('t', '├'),
        ('u', '┤'),
        ('v', '┴'),
        ('w', '┬'),
        ('Z', 'Z'),
    ] {
        let flags = if src == 'A' {
            ScreenAttrFlags::NONE
        } else {
            gfx
        };
        assert_eq!(translate_cell_char(src, flags), expected);
    }

    let mut grid = VirtualGridBackend::default();
    assert_eq!(grid.dimensions(), ScreenDimensions::STANDARD_24X80);
    assert_eq!(grid.cursor(), CursorPosition::HOME);
    assert_eq!(grid.cursor_visibility(), CursorVisibility::Normal);
    grid.set_cursor_visibility(CursorVisibility::Hidden)?;
    assert_eq!(grid.cursor_visibility(), CursorVisibility::Hidden);
    assert_eq!(
        VirtualGridBackend::from_env_lookup(&mut |_| None).dimensions(),
        ScreenDimensions::STANDARD_24X80
    );

    let full_style = CellStyle::new(
        Some(ScreenColor::Green),
        Some(ScreenColor::Black),
        ScreenAttrFlags::HIGHLIGHT
            .union(ScreenAttrFlags::LOWLIGHT)
            .union(ScreenAttrFlags::UNDERLINE)
            .union(ScreenAttrFlags::BLINK)
            .union(ScreenAttrFlags::REVERSE_VIDEO)
            .union(ScreenAttrFlags::OVERLINE),
    );
    assert_eq!(format_ansi_sgr(full_style), "\x1b[0;1;2;4;5;7;53;32;40m");

    let r1 = ScreenRow::ONE;
    let r2 = ScreenRow::try_new(2)?;
    let _ = grid.write_span(CursorPosition::HOME, "HELLO WORLD", full_style)?;
    assert_eq!(grid.row_trimmed_end(r1)?, "HELLO WORLD");
    assert_eq!(grid.cell_at(CursorPosition::HOME)?.ch, 'H');
    assert!(grid.cell_at(CursorPosition::try_from_raw(25, 1)?).is_err());
    assert!(grid.cell_at(CursorPosition::try_from_raw(1, 81)?).is_err());
    assert!(grid.row_string(ScreenRow::try_new(25)?).is_err());
    assert!(grid.row_trimmed_end(ScreenRow::try_new(25)?).is_err());

    grid.erase_to_end_of_line(CursorPosition::try_from_raw(1, 6)?, CellStyle::DEFAULT)?;
    assert_eq!(grid.row_trimmed_end(r1)?, "HELLO");
    let _ = grid.write_span(
        CursorPosition::new(r2, ScreenCol::ONE),
        "ROW2",
        CellStyle::DEFAULT,
    )?;
    grid.clear_line(r2, CellStyle::DEFAULT)?;
    assert_eq!(grid.row_trimmed_end(r2)?, "");
    grid.erase_to_end_of_screen(CursorPosition::try_from_raw(1, 3)?, CellStyle::DEFAULT)?;
    assert_eq!(grid.row_trimmed_end(r1)?, "HE");
    assert_eq!(grid.snapshot().lines().count(), 24);
    grid.clear_screen(CellStyle::DEFAULT)?;
    assert_eq!(grid.row_trimmed_end(r1)?, "");
    grid.ring_bell()?;
    grid.flush()?;
    assert_eq!((grid.bell_count(), grid.flush_count()), (1, 1));

    grid.push_key(KeyEvent::Home);
    grid.push_keys(&[KeyEvent::End]);
    grid.push_text_with_enter("AB");
    assert_eq!(grid.remaining_keys(), 5);
    for exp in [
        KeyEvent::Home,
        KeyEvent::End,
        KeyEvent::Char('A'),
        KeyEvent::Char('B'),
        KeyEvent::Enter,
        KeyEvent::Eof,
    ] {
        assert_eq!(grid.read_key()?, exp);
    }
    Ok(())
}

/// Verifies VT100/CSI/SS3/UTF-8 keystroke decoding, `AnsiVt100Backend`, and stream I/O error propagation.
#[test]
fn ansi_vt100_backend_and_escape_sequence_decoder() -> Result<()> {
    let mut grid = VirtualGridBackend::new_24x80();
    let mut ansi_stream: Vec<u8> = Vec::new();
    ansi_stream.extend_from_slice(&[0x0D, 0x0A, 0x0D, b'X', 0x0A, 0x09, 0x08, 0x7F, 0x01]);
    ansi_stream
        .extend_from_slice(b"\x1bb\x1bf\x1bd\x1b?\x1b[A\x1b[B\x1b[C\x1b[D\x1b[H\x1b[F\x1b[Z\x1b[?");
    ansi_stream.extend_from_slice(
        b"\x1b[[A\x1b[[Z\x1b[1~\x1b[2~\x1b[3~\x1b[3;3~\x1b[4~\x1b[5~\x1b[6~\x1b[7~\x1b[8~\x1b[99~",
    );
    ansi_stream.extend_from_slice(b"\x1b[15~\x1b[17~\x1b[24~\x1b[1;3D\x1b[1;2D\x1b[1;3C\x1b[1;2C");
    ansi_stream.extend_from_slice(b"\x1b[1;2A\x1b[1;2B\x1b[1;2H\x1b[1;2F\x1b[1;2P\x1b[1;2X");
    ansi_stream
        .extend_from_slice(b"\x1bOP\x1bOQ\x1bOR\x1bOS\x1bOH\x1bOF\x1bOA\x1bOB\x1bOC\x1bOD\x1bOZ");
    ansi_stream.extend_from_slice("é€𝄞".as_bytes());
    ansi_stream.extend_from_slice(&[0xFF, 0xC3, 0x28, 0x04]);

    grid.push_ansi_bytes(&ansi_stream);
    for expected in [
        KeyEvent::Enter,
        KeyEvent::Enter,
        KeyEvent::Char('X'),
        KeyEvent::Enter,
        KeyEvent::Tab,
        KeyEvent::Backspace,
        KeyEvent::Backspace,
        KeyEvent::Char(' '),
        KeyEvent::AltLeft,
        KeyEvent::AltRight,
        KeyEvent::AltDelete,
        KeyEvent::Escape,
        KeyEvent::Up,
        KeyEvent::Down,
        KeyEvent::Right,
        KeyEvent::Left,
        KeyEvent::Home,
        KeyEvent::End,
        KeyEvent::BackTab,
        KeyEvent::Escape,
        KeyEvent::Function(1),
        KeyEvent::Escape,
        KeyEvent::Home,
        KeyEvent::Insert,
        KeyEvent::Delete,
        KeyEvent::AltDelete,
        KeyEvent::End,
        KeyEvent::PageUp,
        KeyEvent::PageDown,
        KeyEvent::Home,
        KeyEvent::End,
        KeyEvent::Escape,
        KeyEvent::Function(5),
        KeyEvent::Function(6),
        KeyEvent::Function(12),
        KeyEvent::AltLeft,
        KeyEvent::Left,
        KeyEvent::AltRight,
        KeyEvent::Right,
        KeyEvent::Up,
        KeyEvent::Down,
        KeyEvent::Home,
        KeyEvent::End,
        KeyEvent::Function(1),
        KeyEvent::Escape,
        KeyEvent::Function(1),
        KeyEvent::Function(2),
        KeyEvent::Function(3),
        KeyEvent::Function(4),
        KeyEvent::Home,
        KeyEvent::End,
        KeyEvent::Up,
        KeyEvent::Down,
        KeyEvent::Right,
        KeyEvent::Left,
        KeyEvent::Escape,
        KeyEvent::Char('é'),
        KeyEvent::Char('€'),
        KeyEvent::Char('𝄞'),
        KeyEvent::Char('\u{FFFD}'),
        KeyEvent::Char('\u{FFFD}'),
        KeyEvent::Char('('),
    ] {
        assert_eq!(grid.read_key()?, expected);
    }
    Ok(())
}

/// Verifies truncated escape/UTF-8 handling at EOF and `StepReader` I/O error propagation.
#[test]
fn ansi_truncated_and_reader_faults() -> Result<()> {
    for truncated in [
        &[0x0D_u8][..],
        &b"\x1b"[..],
        &b"\x1b["[..],
        &b"\x1b[["[..],
        &b"\x1b[12"[..],
        &b"\x1bO"[..],
        &[0xE2_u8, 0x82][..],
    ] {
        let _ = decode_next_key_event(&mut Cursor::new(truncated), &mut VecDeque::new())?;
    }
    for fail_step in 0..5 {
        let mut bad_reader = StepReader::new(&[0xE2, 0x82, 0xAC], Some(fail_step));
        let _ = VirtualGridBackend::new_24x80().read_ansi_keys(&mut bad_reader);
    }
    for (seq, max_steps) in [
        (&[0x0D_u8, 0x0A][..], 2_usize),
        (&b"\x1b[1;3~"[..], 6_usize),
        (&b"\x1b[[A"[..], 4_usize),
        (&b"\x1bOP"[..], 3_usize),
    ] {
        for fail_step in 0..max_steps {
            assert!(
                decode_next_key_event(
                    &mut StepReader::new(seq, Some(fail_step)),
                    &mut VecDeque::new()
                )
                .is_err()
            );
        }
    }
    Ok(())
}

/// Verifies `AnsiVt100Backend` escape emission, stdio constructor, and step-by-step writer errors.
#[test]
fn ansi_vt100_backend_io_and_faults() -> Result<()> {
    let stdio_backend = AnsiVt100Backend::stdio(ScreenDimensions::STANDARD_24X80);
    assert_eq!(stdio_backend.dimensions(), ScreenDimensions::STANDARD_24X80);
    let _ = stdio_backend.into_streams();

    let (writer, captured) = StepWriter::capture();
    let mut ansi = AnsiVt100Backend::new(
        ScreenDimensions::STANDARD_24X80,
        Box::new(Cursor::new(vec![b'Q', 0x0A])),
        Box::new(writer),
    );
    assert_eq!(
        (ansi.cursor(), ansi.cursor_visibility()),
        (CursorPosition::HOME, CursorVisibility::Normal)
    );
    ansi.set_cursor(CursorPosition::try_from_raw(3, 5)?)?;
    ansi.set_cursor_visibility(CursorVisibility::Hidden)?;
    ansi.set_cursor_visibility(CursorVisibility::High)?;
    ansi.set_cursor_visibility(CursorVisibility::Normal)?;
    ansi.clear_screen(CellStyle::DEFAULT)?;
    ansi.clear_line(ScreenRow::try_new(2)?, CellStyle::DEFAULT)?;
    ansi.erase_to_end_of_line(CursorPosition::HOME, CellStyle::DEFAULT)?;
    ansi.erase_to_end_of_screen(CursorPosition::HOME, CellStyle::DEFAULT)?;
    ansi.ring_bell()?;
    let _ = ansi.write_span(CursorPosition::HOME, "HI", CellStyle::DEFAULT)?;
    ansi.flush()?;
    assert_eq!(
        (ansi.read_key()?, ansi.read_key()?),
        (KeyEvent::Char('Q'), KeyEvent::Enter)
    );
    assert!(!captured.borrow().is_empty());

    for fail_idx in 0..9 {
        let mut f = AnsiVt100Backend::new(
            ScreenDimensions::STANDARD_24X80,
            Box::new(Cursor::new(Vec::new())),
            Box::new(StepWriter::failing_at(fail_idx)),
        );
        let res = f
            .set_cursor(CursorPosition::HOME)
            .and_then(|()| f.set_cursor_visibility(CursorVisibility::Hidden))
            .and_then(|()| f.clear_screen(CellStyle::DEFAULT))
            .and_then(|()| f.clear_line(ScreenRow::ONE, CellStyle::DEFAULT))
            .and_then(|()| f.erase_to_end_of_line(CursorPosition::HOME, CellStyle::DEFAULT))
            .and_then(|()| f.erase_to_end_of_screen(CursorPosition::HOME, CellStyle::DEFAULT))
            .and_then(|()| f.ring_bell())
            .and_then(|()| {
                f.write_span(CursorPosition::HOME, "X", CellStyle::DEFAULT)
                    .map(|_| ())
            })
            .and_then(|()| f.flush());
        assert!(res.is_err());
    }
    Ok(())
}

/// Builds a standard 4-digit unsigned zoned decimal [`HirType`].
fn zoned_u4_type() -> Result<HirType> {
    Ok(HirType::ZonedDecimal {
        digits: DigitCount::try_new(4)?,
        scale: ScaleFactor::try_new(0)?,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    })
}

/// Builds a `ScreenFieldSpec` with `Absolute` coordinates (`row`, `col`), `attrs`, and `binding`.
fn elem_at(
    row: u16,
    col: u16,
    attrs: ScreenAttributeSet,
    binding: ScreenDataBinding,
) -> Result<ScreenNode> {
    Ok(ScreenNode::Elementary(ScreenFieldSpec {
        name: None,
        line: LineSpec::Absolute(ScreenRow::try_new(row)?),
        col: ColSpec::Absolute(ScreenCol::try_new(col)?),
        attrs,
        binding,
    }))
}

/// Verifies unbound `MapFieldStorage` errors and `ActionOnly` formatting.
#[test]
fn storage_errors_and_action_only_display() -> Result<()> {
    let mut storage = MapFieldStorage::new();
    let id_missing = DataItemId::new(99);
    assert!(storage.get(id_missing).is_none());
    assert!(storage.read_field(id_missing).is_err());
    assert!(storage.write_field_mut(id_missing).is_err());

    let action_resolved = ResolvedScreenField {
        name: None,
        pos: CursorPosition::HOME,
        width: 0,
        attrs: ScreenAttributeSet::new(),
        binding: ScreenDataBinding::ActionOnly,
    };
    assert_eq!(
        format_field_for_screen_display(&action_resolved, &storage)?,
        ""
    );
    Ok(())
}

/// Verifies `ScreenEngine::display_layout` across `PICTURE` programs, `RepeatAll`, `SECURE`, and `JUSTIFIED RIGHT`.
#[test]
fn engine_display_picture_and_accept_blank_ignored() -> Result<()> {
    let mut storage = MapFieldStorage::new();
    let id_src = DataItemId::new(10);
    storage.insert(id_src, b"ALICE\x00".to_vec());

    let alpha_prog = PictureProgram::try_alphanumeric(vec![
        PicEditOp::AnyCharSlot,
        PicEditOp::AnyCharSlot,
        PicEditOp::InsertSlash,
        PicEditOp::AnyCharSlot,
    ])?;
    let alpha_ed_pic = ScreenFieldPic::try_new(
        4,
        MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 3,
            justified: coblang_hir::JustifiedSpec::Left,
        }),
        Some(alpha_prog),
    )?;

    let disp_nodes = vec![ScreenNode::Group {
        name: None,
        line: LineSpec::Absolute(ScreenRow::ONE),
        col: ColSpec::Absolute(ScreenCol::ONE),
        attrs: ScreenAttributeSet::new().with_flags(
            ScreenAttrFlags::BLANK_SCREEN
                .union(ScreenAttrFlags::BLANK_LINE)
                .union(ScreenAttrFlags::ERASE_EOS)
                .union(ScreenAttrFlags::ERASE_EOL)
                .union(ScreenAttrFlags::BELL),
        ),
        children: vec![
            elem_at(
                1,
                1,
                ScreenAttributeSet::new(),
                ScreenDataBinding::Literal {
                    bytes: b"=-".to_vec(),
                    tile: LiteralTileMode::RepeatAll,
                    explicit_size: Some(6),
                },
            )?,
            elem_at(
                1,
                8,
                ScreenAttributeSet::new(),
                ScreenDataBinding::Literal {
                    bytes: Vec::new(),
                    tile: LiteralTileMode::RepeatAll,
                    explicit_size: Some(3),
                },
            )?,
            elem_at(
                2,
                1,
                ScreenAttributeSet::new()
                    .with_flags(ScreenAttrFlags::JUSTIFIED_RIGHT.union(ScreenAttrFlags::SECURE)),
                ScreenDataBinding::From {
                    source_id: id_src,
                    pic: alpha_ed_pic.clone(),
                    explicit_size: Some(6),
                },
            )?,
            elem_at(
                2,
                10,
                ScreenAttributeSet::new().with_flags(ScreenAttrFlags::JUSTIFIED_RIGHT),
                ScreenDataBinding::From {
                    source_id: id_src,
                    pic: alpha_ed_pic,
                    explicit_size: Some(2),
                },
            )?,
        ],
    }];
    let mut engine = ScreenEngine::default();
    assert_eq!(
        (
            engine.crt_status(),
            engine.cursor(),
            engine.last_exception()
        ),
        (CrtStatusCode::NORMAL_COMPLETION, CursorPosition::HOME, None)
    );
    let mut grid = VirtualGridBackend::new_24x80();
    let _ = engine.display_layout(
        &ScreenLayout::compile(ScreenDimensions::STANDARD_24X80, &disp_nodes),
        &storage,
        &mut grid,
    )?;
    assert_eq!(grid.row_trimmed_end(ScreenRow::ONE)?, "=-=-=-");
    assert_eq!(grid.row_trimmed_end(ScreenRow::try_new(2)?)?, "  ***    /");
    assert_eq!(grid.bell_count(), 1);
    Ok(())
}

/// Verifies numeric edited `BLANK WHEN ZERO` and `ZERO-FILL` display formatting.
#[test]
fn engine_display_numeric_edited_blank_zero_and_zero_fill() -> Result<()> {
    let id_num = DataItemId::new(13);
    let mut storage = MapFieldStorage::new();
    storage.insert(id_num, b"0000".to_vec());
    let (d4, sc0) = (DigitCount::try_new(4)?, ScaleFactor::try_new(0)?);
    let num_prog = PictureProgram::try_numeric(
        vec![
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::ZeroSuppressSpace,
            PicEditOp::DigitSlot,
        ],
        NumericPictureMeta {
            digits: d4,
            scale: sc0,
            sign_capability: Signedness::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        },
        DecimalPointKind::Period,
        CurrencySignSpec::standard_dollar(),
    )?;
    let num_pic = ScreenFieldPic::try_new(
        4,
        MoveFieldDesc::standard_gnu(zoned_u4_type()?),
        Some(num_prog),
    )?;
    let nodes = vec![
        elem_at(
            3,
            1,
            ScreenAttributeSet::new().with_flags(ScreenAttrFlags::BLANK_WHEN_ZERO),
            ScreenDataBinding::From {
                source_id: id_num,
                pic: num_pic.clone(),
                explicit_size: None,
            },
        )?,
        elem_at(
            3,
            6,
            ScreenAttributeSet::new().with_flags(ScreenAttrFlags::ZERO_FILL),
            ScreenDataBinding::From {
                source_id: id_num,
                pic: num_pic,
                explicit_size: None,
            },
        )?,
    ];
    let mut grid = VirtualGridBackend::new_24x80();
    let _ = ScreenEngine::new().display_layout(
        &ScreenLayout::compile(ScreenDimensions::STANDARD_24X80, &nodes),
        &storage,
        &mut grid,
    )?;
    assert_eq!(grid.row_trimmed_end(ScreenRow::try_new(3)?)?, "     0000");
    Ok(())
}

/// Verifies `TO` display with `UPDATE` and `PROMPT` and confirms `BLANK SCREEN` is ignored during `ACCEPT`.
#[test]
fn engine_display_to_update_prompt_and_accept_blank_ignored() -> Result<()> {
    let id_to = DataItemId::new(11);
    let mut storage = MapFieldStorage::new();
    storage.insert(id_to, b"PRELOAD ".to_vec());
    let mut engine = ScreenEngine::new();
    let mut grid = VirtualGridBackend::new_24x80();
    let to_nodes = vec![
        elem_at(
            1,
            1,
            ScreenAttributeSet::new(),
            ScreenDataBinding::Literal {
                bytes: b"=-=-=-".to_vec(),
                tile: LiteralTileMode::Single,
                explicit_size: None,
            },
        )?,
        elem_at(
            4,
            1,
            ScreenAttributeSet::new().with_flags(ScreenAttrFlags::UPDATE),
            ScreenDataBinding::To {
                target_id: id_to,
                pic: ScreenFieldPic::alphanumeric(8)?,
                explicit_size: None,
            },
        )?,
        elem_at(
            4,
            10,
            ScreenAttributeSet::new().with_prompt(PromptSpec::UNDERSCORE),
            ScreenDataBinding::To {
                target_id: id_to,
                pic: ScreenFieldPic::alphanumeric(4)?,
                explicit_size: None,
            },
        )?,
    ];
    let _ = engine.display_layout(
        &ScreenLayout::compile(ScreenDimensions::STANDARD_24X80, &to_nodes),
        &storage,
        &mut grid,
    )?;
    assert_eq!(
        grid.row_trimmed_end(ScreenRow::try_new(4)?)?,
        "PRELOAD  ____"
    );

    grid.push_key(KeyEvent::Function(5));
    let ro_lay = ScreenLayout::compile(
        ScreenDimensions::STANDARD_24X80,
        &[elem_at(
            5,
            1,
            ScreenAttributeSet::new()
                .with_flags(ScreenAttrFlags::BLANK_SCREEN.union(ScreenAttrFlags::BELL)),
            ScreenDataBinding::Literal {
                bytes: b"PRESS KEY".to_vec(),
                tile: LiteralTileMode::Single,
                explicit_size: None,
            },
        )?],
    );
    assert_eq!(
        engine
            .accept_layout(&ro_lay, &mut storage, &mut grid)?
            .crt_status,
        CrtStatusCode::function_key(5)?
    );
    assert_eq!(grid.row_trimmed_end(ScreenRow::ONE)?, "=-=-=-");
    Ok(())
}

/// Constructs the 3-field `SCREEN SECTION` layout used to test multi-field `ACCEPT` navigation.
fn build_multifield_form(
    id_using: DataItemId,
    id_num: DataItemId,
    id_to: DataItemId,
) -> Result<ScreenLayout> {
    let raw_num_pic =
        ScreenFieldPic::try_new(4, MoveFieldDesc::standard_gnu(zoned_u4_type()?), None)?;
    let nodes = vec![
        elem_at(
            6,
            1,
            ScreenAttributeSet::new()
                .with_flags(
                    ScreenAttrFlags::AUTO
                        .union(ScreenAttrFlags::REQUIRED)
                        .union(ScreenAttrFlags::FULL)
                        .union(ScreenAttrFlags::SECURE),
                )
                .with_prompt(PromptSpec::UNDERSCORE),
            ScreenDataBinding::Using {
                target_id: id_using,
                pic: ScreenFieldPic::alphanumeric(4)?,
                explicit_size: None,
            },
        )?,
        elem_at(
            6,
            10,
            ScreenAttributeSet::new().with_flags(
                ScreenAttrFlags::REQUIRED
                    .union(ScreenAttrFlags::ZERO_FILL)
                    .union(ScreenAttrFlags::INITIAL),
            ),
            ScreenDataBinding::To {
                target_id: id_num,
                pic: raw_num_pic,
                explicit_size: None,
            },
        )?,
        elem_at(
            6,
            20,
            ScreenAttributeSet::new().with_flags(
                ScreenAttrFlags::UPDATE
                    .union(ScreenAttrFlags::JUSTIFIED_RIGHT)
                    .union(ScreenAttrFlags::AUTO),
            ),
            ScreenDataBinding::To {
                target_id: id_to,
                pic: ScreenFieldPic::alphanumeric(8)?,
                explicit_size: None,
            },
        )?,
    ];
    Ok(ScreenLayout::compile(
        ScreenDimensions::STANDARD_24X80,
        &nodes,
    ))
}

/// Verifies multi-field `ACCEPT` editing, `CURSOR` register focus, `Insert`, word/arrow motion,
/// `AUTO`, `REQUIRED`, `FULL`, `ZERO-FILL`, `UPDATE`, and `INITIAL`.
#[test]
fn engine_accept_multifield_navigation_and_validation() -> Result<()> {
    let mut storage = MapFieldStorage::new();
    let (id_to, id_using, id_num) = (
        DataItemId::new(11),
        DataItemId::new(12),
        DataItemId::new(13),
    );
    storage.insert(id_to, b"PRELOAD ".to_vec());
    storage.insert(id_using, b"HELLO   ".to_vec());
    storage.insert(id_num, b"0000".to_vec());

    let form_layout = build_multifield_form(id_using, id_num, id_to)?;
    let mut engine = ScreenEngine::new();
    let mut grid = VirtualGridBackend::new_24x80();
    engine.load_cursor_register(CursorFormat::FourDigitLlCc, b"0611")?;
    for k in [
        KeyEvent::Enter,
        KeyEvent::Char('0'),
        KeyEvent::Char('0'),
        KeyEvent::Char('0'),
        KeyEvent::Char('0'),
        KeyEvent::Enter,
        KeyEvent::AltDelete,
    ] {
        grid.push_key(k);
    }
    grid.push_keys(&[
        KeyEvent::Char('4'),
        KeyEvent::Char('2'),
        KeyEvent::BackTab,
        KeyEvent::AltDelete,
    ]);
    grid.push_keys(&[
        KeyEvent::Char('A'),
        KeyEvent::Char(' '),
        KeyEvent::Char('B'),
        KeyEvent::Home,
        KeyEvent::AltRight,
        KeyEvent::AltLeft,
        KeyEvent::End,
    ]);
    grid.push_keys(&[
        KeyEvent::Backspace,
        KeyEvent::Insert,
        KeyEvent::Home,
        KeyEvent::Char('Z'),
        KeyEvent::Insert,
        KeyEvent::Delete,
        KeyEvent::Tab,
        KeyEvent::Down,
        KeyEvent::Enter,
    ]);
    grid.push_keys(&[
        KeyEvent::AltDelete,
        KeyEvent::Char('P'),
        KeyEvent::Char('A'),
        KeyEvent::Char('S'),
        KeyEvent::Char('S'),
    ]);
    grid.push_keys(&[
        KeyEvent::End,
        KeyEvent::Right,
        KeyEvent::Right,
        KeyEvent::Left,
        KeyEvent::Up,
        KeyEvent::Tab,
        KeyEvent::Tab,
        KeyEvent::AltDelete,
    ]);
    for b in b"OK123456" {
        grid.push_key(KeyEvent::Char(char::from(*b)));
    }
    let summary = engine.accept_layout(&form_layout, &mut storage, &mut grid)?;
    assert_eq!(summary.crt_status, CrtStatusCode::NORMAL_COMPLETION);
    assert_eq!(storage.get(id_using), Some(&b"PASS    "[..]));
    assert_eq!(storage.get(id_num), Some(&b"0042"[..]));
    assert_eq!(storage.get(id_to), Some(&b"OK123456"[..]));

    let mut cur_out = [0_u8; 6];
    let mut st_out = [0_u8; 4];
    engine.store_cursor_register(CursorFormat::SixDigitLllCcc, &mut cur_out)?;
    engine.store_crt_status_register(&mut st_out)?;
    assert_eq!(&st_out, b"0000");
    assert_eq!(EditInsertMode::default(), EditInsertMode::Overwrite);
    Ok(())
}

/// Verifies single-field `ACCEPT` termination keys (`Escape`, `Tab`, `BackTab`, `PageUp`, `Eof`),
/// `AUTO` validation failure, and invalid numeric de-edit fallback.
#[test]
fn engine_accept_single_field_terminators_and_eof_validation() -> Result<()> {
    let mut storage = MapFieldStorage::new();
    let (id_to, id_using, id_num) = (
        DataItemId::new(11),
        DataItemId::new(12),
        DataItemId::new(13),
    );
    storage.insert(id_to, b"  ".to_vec());
    storage.insert(id_using, b"  ".to_vec());
    storage.insert(id_num, b"0000".to_vec());

    let raw_num_pic =
        ScreenFieldPic::try_new(4, MoveFieldDesc::standard_gnu(zoned_u4_type()?), None)?;
    let mut engine = ScreenEngine::new();
    let mut grid = VirtualGridBackend::new_24x80();
    engine.load_cursor_register(CursorFormat::FourDigitLlCc, b"0000")?;
    engine.set_requested_accept_cursor(Some(CursorPosition::try_from_raw(20, 20)?));

    let single_auto_req = ScreenLayout::compile(
        ScreenDimensions::STANDARD_24X80,
        &[elem_at(
            8,
            1,
            ScreenAttributeSet::new()
                .with_flags(ScreenAttrFlags::AUTO.union(ScreenAttrFlags::REQUIRED)),
            ScreenDataBinding::To {
                target_id: id_num,
                pic: raw_num_pic.clone(),
                explicit_size: None,
            },
        )?],
    );
    grid.push_keys(&[
        KeyEvent::Backspace,
        KeyEvent::Left,
        KeyEvent::Char(' '),
        KeyEvent::Char(' '),
        KeyEvent::Char(' '),
        KeyEvent::Char(' '),
        KeyEvent::Right,
        KeyEvent::Escape,
    ]);
    assert_eq!(
        engine
            .accept_layout(&single_auto_req, &mut storage, &mut grid)?
            .crt_status,
        CrtStatusCode::ESCAPE
    );
    assert_eq!(
        engine
            .accept_layout(&single_auto_req, &mut storage, &mut grid)?
            .crt_status,
        CrtStatusCode::SCREEN_ERROR
    );
    assert_eq!(
        engine.last_exception(),
        Some(ScreenExceptionKind::ImplementorCondition)
    );
    engine.clear_last_exception();
    assert!(engine.last_exception().is_none());
    Ok(())
}

/// Verifies `Tab`, `BackTab`, `PageUp`, `Eof`, malformed numeric de-edit, and `AUTO` re-validation across fields.
#[test]
fn engine_accept_optional_numeric_and_auto_recheck() -> Result<()> {
    let mut storage = MapFieldStorage::new();
    let (id_to, id_using, id_num) = (
        DataItemId::new(11),
        DataItemId::new(12),
        DataItemId::new(13),
    );
    storage.insert(id_to, b"  ".to_vec());
    storage.insert(id_using, b"  ".to_vec());
    storage.insert(id_num, b"0000".to_vec());
    let raw_num_pic =
        ScreenFieldPic::try_new(4, MoveFieldDesc::standard_gnu(zoned_u4_type()?), None)?;
    let mut engine = ScreenEngine::new();
    let mut grid = VirtualGridBackend::new_24x80();

    let single_opt_num = ScreenLayout::compile(
        ScreenDimensions::STANDARD_24X80,
        &[elem_at(
            9,
            1,
            ScreenAttributeSet::new(),
            ScreenDataBinding::To {
                target_id: id_num,
                pic: raw_num_pic,
                explicit_size: None,
            },
        )?],
    );
    grid.push_key(KeyEvent::Tab);
    assert_eq!(
        engine
            .accept_layout(&single_opt_num, &mut storage, &mut grid)?
            .crt_status,
        CrtStatusCode::TAB_FORWARD
    );
    assert_eq!(storage.get(id_num), Some(&b"0000"[..]));
    grid.push_key(KeyEvent::BackTab);
    assert_eq!(
        engine
            .accept_layout(&single_opt_num, &mut storage, &mut grid)?
            .crt_status,
        CrtStatusCode::BACK_TAB
    );
    grid.push_keys(&[KeyEvent::Char('A'), KeyEvent::Char('B'), KeyEvent::PageUp]);
    let bad_num_sum = engine.accept_layout(&single_opt_num, &mut storage, &mut grid)?;
    assert_eq!(bad_num_sum.crt_status, CrtStatusCode::PAGE_UP);
    assert!(
        bad_num_sum
            .exceptions
            .contains(&ScreenExceptionKind::ImplementorCondition)
    );
    assert_eq!(
        engine
            .accept_layout(&single_opt_num, &mut storage, &mut grid)?
            .crt_status,
        CrtStatusCode::INPUT_EOF
    );

    let two_field = ScreenLayout::compile(
        ScreenDimensions::STANDARD_24X80,
        &[
            elem_at(
                10,
                1,
                ScreenAttributeSet::new().with_flags(ScreenAttrFlags::REQUIRED),
                ScreenDataBinding::To {
                    target_id: id_using,
                    pic: ScreenFieldPic::alphanumeric(2)?,
                    explicit_size: None,
                },
            )?,
            elem_at(
                10,
                5,
                ScreenAttributeSet::new().with_flags(ScreenAttrFlags::AUTO),
                ScreenDataBinding::To {
                    target_id: id_to,
                    pic: ScreenFieldPic::alphanumeric(2)?,
                    explicit_size: None,
                },
            )?,
        ],
    );
    grid.push_keys(&[
        KeyEvent::Tab,
        KeyEvent::Char('X'),
        KeyEvent::Char('Y'),
        KeyEvent::Char('O'),
        KeyEvent::Char('K'),
        KeyEvent::Enter,
    ]);
    assert_eq!(
        engine
            .accept_layout(&two_field, &mut storage, &mut grid)?
            .crt_status,
        CrtStatusCode::NORMAL_COMPLETION
    );
    Ok(())
}

/// Exercises `display_layout` backend, unbound storage, and malformed numeric storage error branches.
#[test]
fn engine_display_and_codec_fault_injection() -> Result<()> {
    let id1 = DataItemId::new(1);
    let mut storage = MapFieldStorage::new();
    storage.insert(id1, b"ABCD".to_vec());

    let disp_layout = ScreenLayout::compile(
        ScreenDimensions::STANDARD_24X80,
        &[ScreenNode::Group {
            name: None,
            line: LineSpec::Absolute(ScreenRow::ONE),
            col: ColSpec::Absolute(ScreenCol::ONE),
            attrs: ScreenAttributeSet::new().with_flags(
                ScreenAttrFlags::BLANK_SCREEN
                    .union(ScreenAttrFlags::BLANK_LINE)
                    .union(ScreenAttrFlags::ERASE_EOS)
                    .union(ScreenAttrFlags::ERASE_EOL)
                    .union(ScreenAttrFlags::BELL),
            ),
            children: vec![elem_at(
                2,
                1,
                ScreenAttributeSet::new(),
                ScreenDataBinding::From {
                    source_id: id1,
                    pic: ScreenFieldPic::alphanumeric(4)?,
                    explicit_size: None,
                },
            )?],
        }],
    );

    for step in 0..7 {
        let mut faulty = FaultyBackend::new(VirtualGridBackend::new_24x80(), step);
        assert!(
            ScreenEngine::new()
                .display_layout(&disp_layout, &storage, &mut faulty)
                .is_err()
        );
    }

    let missing_id = DataItemId::new(404);
    for binding in [
        ScreenDataBinding::From {
            source_id: missing_id,
            pic: ScreenFieldPic::alphanumeric(4)?,
            explicit_size: None,
        },
        ScreenDataBinding::To {
            target_id: missing_id,
            pic: ScreenFieldPic::alphanumeric(4)?,
            explicit_size: None,
        },
    ] {
        let lay = ScreenLayout::compile(
            ScreenDimensions::STANDARD_24X80,
            &[elem_at(
                1,
                1,
                ScreenAttributeSet::new().with_flags(ScreenAttrFlags::UPDATE),
                binding,
            )?],
        );
        let mut g = VirtualGridBackend::new_24x80();
        assert!(
            ScreenEngine::new()
                .display_layout(&lay, &storage, &mut g)
                .is_err()
        );
        assert!(
            ScreenEngine::new()
                .accept_layout(&lay, &mut storage, &mut g)
                .is_err()
        );
    }
    Ok(())
}

/// Exercises malformed `PackedDecimal` storage error propagation during `display_layout`.
#[test]
fn engine_packed_codec_fault_injection() -> Result<()> {
    let mut storage = MapFieldStorage::new();
    let bad_num_id = DataItemId::new(77);
    storage.insert(bad_num_id, vec![0xFF, 0xFF]);
    let (d3, sc0) = (DigitCount::try_new(3)?, ScaleFactor::try_new(0)?);
    let packed_ty = HirType::PackedDecimal {
        digits: d3,
        scale: sc0,
        sign: Signedness::Signed,
        unsigned_comp6: coblang_hir::Comp6Mode::StandardComp3,
    };
    let num_prog = PictureProgram::try_numeric(
        vec![
            PicEditOp::DigitSlot,
            PicEditOp::DigitSlot,
            PicEditOp::DigitSlot,
        ],
        NumericPictureMeta {
            digits: d3,
            scale: sc0,
            sign_capability: Signedness::Signed,
            blank_zero: BlankZeroSpec::RetainZero,
        },
        DecimalPointKind::Period,
        CurrencySignSpec::standard_dollar(),
    )?;
    for edit_opt in [None, Some(num_prog)] {
        let bad_pic =
            ScreenFieldPic::try_new(3, MoveFieldDesc::standard_gnu(packed_ty.clone()), edit_opt)?;
        let bad_lay = ScreenLayout::compile(
            ScreenDimensions::STANDARD_24X80,
            &[elem_at(
                1,
                1,
                ScreenAttributeSet::new(),
                ScreenDataBinding::From {
                    source_id: bad_num_id,
                    pic: bad_pic,
                    explicit_size: None,
                },
            )?],
        );
        assert!(
            ScreenEngine::new()
                .display_layout(&bad_lay, &storage, &mut VirtualGridBackend::new_24x80())
                .is_err()
        );
    }
    Ok(())
}

/// Exercises `accept_layout` backend step failures, write-back storage errors, and out-of-bounds tracking.
#[test]
fn engine_accept_backend_and_writeback_fault_injection() -> Result<()> {
    let id1 = DataItemId::new(1);
    let mut storage = MapFieldStorage::new();
    storage.insert(id1, b"AB".to_vec());

    let ro_lay = ScreenLayout::compile(
        ScreenDimensions::STANDARD_24X80,
        &[elem_at(
            1,
            1,
            ScreenAttributeSet::new().with_flags(ScreenAttrFlags::BELL),
            ScreenDataBinding::Literal {
                bytes: b"LBL".to_vec(),
                tile: LiteralTileMode::Single,
                explicit_size: None,
            },
        )?],
    );
    for step in 0..4 {
        let mut inner = VirtualGridBackend::new_24x80();
        inner.push_key(KeyEvent::Enter);
        assert!(
            ScreenEngine::new()
                .accept_layout(&ro_lay, &mut storage, &mut FaultyBackend::new(inner, step))
                .is_err()
        );
    }

    let edit_lay = ScreenLayout::compile(
        ScreenDimensions::STANDARD_24X80,
        &[
            elem_at(
                1,
                1,
                ScreenAttributeSet::new(),
                ScreenDataBinding::Literal {
                    bytes: b"PROMPT:".to_vec(),
                    tile: LiteralTileMode::Single,
                    explicit_size: None,
                },
            )?,
            elem_at(
                1,
                9,
                ScreenAttributeSet::new()
                    .with_flags(ScreenAttrFlags::AUTO.union(ScreenAttrFlags::REQUIRED)),
                ScreenDataBinding::Using {
                    target_id: id1,
                    pic: ScreenFieldPic::alphanumeric(2)?,
                    explicit_size: None,
                },
            )?,
        ],
    );
    for step in 0..24 {
        let mut inner = VirtualGridBackend::new_24x80();
        inner.push_keys(&[
            KeyEvent::Insert,
            KeyEvent::Char('A'),
            KeyEvent::Backspace,
            KeyEvent::Delete,
            KeyEvent::AltDelete,
            KeyEvent::Enter,
        ]);
        inner.push_keys(&[
            KeyEvent::Char(' '),
            KeyEvent::Char(' '),
            KeyEvent::AltDelete,
            KeyEvent::Char('O'),
            KeyEvent::Char('K'),
        ]);
        let _ = ScreenEngine::new().accept_layout(
            &edit_lay,
            &mut storage,
            &mut FaultyBackend::new(inner, step),
        );
    }
    Ok(())
}

/// Exercises write-back storage errors and out-of-bounds layout exception recording.
#[test]
fn engine_writeback_and_oob_fault_injection() -> Result<()> {
    let id1 = DataItemId::new(1);
    let mut storage = MapFieldStorage::new();
    storage.insert(id1, b"AB".to_vec());

    let simple_to = ScreenLayout::single_field(
        ScreenDimensions::STANDARD_24X80,
        ScreenFieldSpec {
            name: None,
            line: LineSpec::Absolute(ScreenRow::ONE),
            col: ColSpec::Absolute(ScreenCol::ONE),
            attrs: ScreenAttributeSet::new(),
            binding: ScreenDataBinding::To {
                target_id: id1,
                pic: ScreenFieldPic::alphanumeric(2)?,
                explicit_size: None,
            },
        },
    );
    for term_key in [KeyEvent::Enter, KeyEvent::Function(1), KeyEvent::Eof] {
        let mut ro_store = ReadOnlyFaultStorage {
            inner: storage.clone(),
        };
        let mut g = VirtualGridBackend::new_24x80();
        g.push_keys(&[KeyEvent::Char('A'), KeyEvent::Char('B'), term_key]);
        assert!(
            ScreenEngine::new()
                .accept_layout(&simple_to, &mut ro_store, &mut g)
                .is_err()
        );
    }

    let oob_lay = ScreenLayout::compile(
        ScreenDimensions::STANDARD_24X80,
        &[elem_at(
            30,
            1,
            ScreenAttributeSet::new(),
            ScreenDataBinding::Literal {
                bytes: b"X".to_vec(),
                tile: LiteralTileMode::Single,
                explicit_size: None,
            },
        )?],
    );
    let mut eng = ScreenEngine::new();
    let _ = eng.display_layout(&oob_lay, &storage, &mut VirtualGridBackend::new_24x80())?;
    assert_eq!(
        eng.last_exception(),
        Some(ScreenExceptionKind::LineOutOfBounds {
            requested_row: 30,
            max_row: 24
        })
    );
    Ok(())
}

/// Covers remaining group column-only coordinate, directive flag combinations, `UPDATE`+`INITIAL` display,
/// `DEL` sanitization, left-padding, non-zero `BLANK WHEN ZERO`, word motion boundaries, standalone `FULL`,
/// alphanumeric `ZERO-FILL` commit, and non-alphanumeric `JUSTIFIED RIGHT` commit branches.
#[test]
fn branch_completion_edge_cases() -> Result<()> {
    let (id_alpha, id_nz) = (DataItemId::new(20), DataItemId::new(22));
    let mut storage = MapFieldStorage::new();
    storage.insert(id_alpha, b"A   ".to_vec());
    storage.insert(id_nz, b"0042".to_vec());

    let alpha_prog = PictureProgram::try_alphanumeric(vec![
        PicEditOp::AnyCharSlot,
        PicEditOp::InsertSlash,
        PicEditOp::AnyCharSlot,
    ])?;
    let alpha_ed_left = ScreenFieldPic::try_new(
        3,
        MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 2,
            justified: coblang_hir::JustifiedSpec::Left,
        }),
        Some(alpha_prog),
    )?;
    let nz_pic = ScreenFieldPic::try_new(4, MoveFieldDesc::standard_gnu(zoned_u4_type()?), None)?;

    let disp_nodes = vec![ScreenNode::Group {
        name: None,
        line: LineSpec::Omitted,
        col: ColSpec::Absolute(ScreenCol::try_new(2)?),
        attrs: ScreenAttributeSet::new().with_flags(ScreenAttrFlags::BLANK_LINE),
        children: vec![
            ScreenNode::Elementary(ScreenFieldSpec {
                name: None,
                line: LineSpec::Omitted,
                col: ColSpec::Omitted,
                attrs: ScreenAttributeSet::new().with_flags(ScreenAttrFlags::BELL),
                binding: ScreenDataBinding::Literal {
                    bytes: b"HI\x7f".to_vec(),
                    tile: LiteralTileMode::Single,
                    explicit_size: Some(6),
                },
            }),
            elem_at(
                2,
                1,
                ScreenAttributeSet::new(),
                ScreenDataBinding::From {
                    source_id: id_alpha,
                    pic: alpha_ed_left,
                    explicit_size: None,
                },
            )?,
            elem_at(
                2,
                6,
                ScreenAttributeSet::new().with_flags(ScreenAttrFlags::BLANK_WHEN_ZERO),
                ScreenDataBinding::From {
                    source_id: id_nz,
                    pic: nz_pic,
                    explicit_size: None,
                },
            )?,
            elem_at(
                3,
                1,
                ScreenAttributeSet::new()
                    .with_flags(ScreenAttrFlags::UPDATE.union(ScreenAttrFlags::INITIAL)),
                ScreenDataBinding::To {
                    target_id: id_alpha,
                    pic: ScreenFieldPic::alphanumeric(4)?,
                    explicit_size: None,
                },
            )?,
        ],
    }];
    let mut eng = ScreenEngine::new();
    let mut grid = VirtualGridBackend::new_24x80();
    let _ = eng.display_layout(
        &ScreenLayout::compile(ScreenDimensions::STANDARD_24X80, &disp_nodes),
        &storage,
        &mut grid,
    )?;
    assert_eq!(grid.row_trimmed_end(ScreenRow::try_new(2)?)?, "A/   0042");
    Ok(())
}

/// Covers word-motion boundaries, standalone `FULL`, alphanumeric `ZERO-FILL` commit, and alphabetic `JUSTIFIED RIGHT`.
#[test]
fn branch_completion_accept_edge_cases() -> Result<()> {
    let (id_alpha, id_grp) = (DataItemId::new(20), DataItemId::new(21));
    let mut storage = MapFieldStorage::new();
    storage.insert(id_alpha, b"A   ".to_vec());
    storage.insert(id_grp, b"GRP ".to_vec());
    let grp_pic = ScreenFieldPic::try_new(
        4,
        MoveFieldDesc::standard_gnu(HirType::Alphabetic {
            byte_len: 4,
            justified: coblang_hir::JustifiedSpec::Left,
        }),
        None,
    )?;
    let mut eng = ScreenEngine::new();
    let mut grid = VirtualGridBackend::new_24x80();

    let accept_nodes = vec![
        elem_at(
            5,
            10,
            ScreenAttributeSet::new().with_flags(ScreenAttrFlags::FULL),
            ScreenDataBinding::To {
                target_id: id_alpha,
                pic: ScreenFieldPic::alphanumeric(4)?,
                explicit_size: None,
            },
        )?,
        elem_at(
            5,
            20,
            ScreenAttributeSet::new().with_flags(ScreenAttrFlags::ZERO_FILL),
            ScreenDataBinding::To {
                target_id: id_alpha,
                pic: ScreenFieldPic::alphanumeric(4)?,
                explicit_size: None,
            },
        )?,
        elem_at(
            5,
            30,
            ScreenAttributeSet::new().with_flags(ScreenAttrFlags::JUSTIFIED_RIGHT),
            ScreenDataBinding::To {
                target_id: id_grp,
                pic: grp_pic,
                explicit_size: None,
            },
        )?,
    ];
    eng.set_requested_accept_cursor(Some(CursorPosition::try_from_raw(5, 3)?));
    grid.push_keys(&[
        KeyEvent::AltLeft,
        KeyEvent::Char(' '),
        KeyEvent::Char(' '),
        KeyEvent::AltLeft,
    ]);
    grid.push_keys(&[
        KeyEvent::Char('A'),
        KeyEvent::Char(' '),
        KeyEvent::Char('B'),
        KeyEvent::AltLeft,
        KeyEvent::Right,
        KeyEvent::Left,
    ]);
    grid.push_keys(&[
        KeyEvent::End,
        KeyEvent::AltRight,
        KeyEvent::Home,
        KeyEvent::AltDelete,
    ]);
    grid.push_keys(&[
        KeyEvent::Tab,
        KeyEvent::Char('A'),
        KeyEvent::Tab,
        KeyEvent::Char('G'),
        KeyEvent::Enter,
    ]);
    let sum = eng.accept_layout(
        &ScreenLayout::compile(ScreenDimensions::STANDARD_24X80, &accept_nodes),
        &mut storage,
        &mut grid,
    )?;
    assert_eq!(sum.crt_status, CrtStatusCode::NORMAL_COMPLETION);
    assert_eq!(storage.get(id_alpha), Some(&b"A000"[..]));
    Ok(())
}
