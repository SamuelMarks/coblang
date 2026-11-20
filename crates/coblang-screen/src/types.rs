//! Validated coordinate, color, attribute, cursor, keystroke, and `CRT STATUS` domain types for `coblang-screen`.

use coblang_common::{CoblangError, Result};
use coblang_rt_core::exceptions::ExceptionCode;

/// Validated 1-based terminal screen row coordinate (`1..=999`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScreenRow(
    /// Underlying 1-based screen row in `1..=999`.
    u16,
);

impl ScreenRow {
    /// Topmost screen row (`1`).
    pub const ONE: Self = Self(1);

    /// Standard 24-line terminal height (`24`).
    pub const STANDARD_24: Self = Self(24);

    /// Maximum validated [`ScreenRow`] (`999`).
    pub const MAX: Self = Self(999);

    /// Maximum addressable 3-digit COBOL `CURSOR` row (`999`).
    pub const MAX_ROW: u16 = 999;

    /// Constructs a validated 1-based [`ScreenRow`] in `1..=999`.
    ///
    /// # Arguments
    ///
    /// * `raw` - One-based row coordinate (`0` and `> 999` are rejected).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw == 0` or `raw > 999`.
    pub fn try_new(raw: u16) -> Result<Self> {
        if (1..=Self::MAX_ROW).contains(&raw) {
            Ok(Self(raw))
        } else {
            Err(CoblangError::InvalidDomainValue {
                domain_type: "ScreenRow",
                value: raw.to_string(),
                reason: String::from("screen row must be in 1..=999"),
            })
        }
    }

    /// Returns the underlying 1-based row number (`1..=999`).
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// Returns the zero-based row index (`0..=998`).
    #[must_use]
    pub const fn zero_based_index(self) -> usize {
        (self.0 - 1) as usize
    }

    /// Clamps `self` to at most `max_row`.
    ///
    /// # Arguments
    ///
    /// * `max_row` - Upper row bound.
    #[must_use]
    pub const fn clamp_max(self, max_row: Self) -> Self {
        if self.0 > max_row.0 { max_row } else { self }
    }

    /// Advances `self` downward by `delta`, clamping to `max_row`.
    ///
    /// # Arguments
    ///
    /// * `delta` - Number of rows to advance downward.
    /// * `max_row` - Maximum terminal row bound.
    #[must_use]
    pub const fn advance_clamped(self, delta: u16, max_row: Self) -> Self {
        let sum = self.0.saturating_add(delta);
        if sum > max_row.0 { max_row } else { Self(sum) }
    }

    /// Moves `self` upward by `delta`, clamping at [`Self::ONE`].
    ///
    /// # Arguments
    ///
    /// * `delta` - Number of rows to move upward.
    #[must_use]
    pub const fn retreat_clamped(self, delta: u16) -> Self {
        let diff = self.0.saturating_sub(delta);
        if diff < 1 { Self::ONE } else { Self(diff) }
    }
}

/// Validated 1-based terminal screen column coordinate (`1..=999`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScreenCol(
    /// Underlying 1-based screen column in `1..=999`.
    u16,
);

impl ScreenCol {
    /// Leftmost screen column (`1`).
    pub const ONE: Self = Self(1);

    /// Standard 80-column terminal width (`80`).
    pub const STANDARD_80: Self = Self(80);

    /// Maximum validated [`ScreenCol`] (`999`).
    pub const MAX: Self = Self(999);

    /// Maximum addressable 3-digit COBOL `CURSOR` column (`999`).
    pub const MAX_COL: u16 = 999;

    /// Constructs a validated 1-based [`ScreenCol`] in `1..=999`.
    ///
    /// # Arguments
    ///
    /// * `raw` - One-based column coordinate (`0` and `> 999` are rejected).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw == 0` or `raw > 999`.
    pub fn try_new(raw: u16) -> Result<Self> {
        if (1..=Self::MAX_COL).contains(&raw) {
            Ok(Self(raw))
        } else {
            Err(CoblangError::InvalidDomainValue {
                domain_type: "ScreenCol",
                value: raw.to_string(),
                reason: String::from("screen column must be in 1..=999"),
            })
        }
    }

    /// Returns the underlying 1-based column number (`1..=999`).
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// Returns the zero-based column index (`0..=998`).
    #[must_use]
    pub const fn zero_based_index(self) -> usize {
        (self.0 - 1) as usize
    }

    /// Clamps `self` to at most `max_col`.
    ///
    /// # Arguments
    ///
    /// * `max_col` - Upper column bound.
    #[must_use]
    pub const fn clamp_max(self, max_col: Self) -> Self {
        if self.0 > max_col.0 { max_col } else { self }
    }

    /// Advances `self` rightward by `delta`, clamping to `max_col`.
    ///
    /// # Arguments
    ///
    /// * `delta` - Number of columns to advance rightward.
    /// * `max_col` - Maximum terminal column bound.
    #[must_use]
    pub const fn advance_clamped(self, delta: u16, max_col: Self) -> Self {
        let sum = self.0.saturating_add(delta);
        if sum > max_col.0 { max_col } else { Self(sum) }
    }

    /// Moves `self` leftward by `delta`, clamping at [`Self::ONE`].
    ///
    /// # Arguments
    ///
    /// * `delta` - Number of columns to move leftward.
    #[must_use]
    pub const fn retreat_clamped(self, delta: u16) -> Self {
        let diff = self.0.saturating_sub(delta);
        if diff < 1 { Self::ONE } else { Self(diff) }
    }
}

/// Validated terminal grid dimensions (`rows` x `cols`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenDimensions {
    /// Total visible terminal rows (`24` by default).
    pub rows: ScreenRow,
    /// Total visible terminal columns (`80` by default).
    pub cols: ScreenCol,
}

impl ScreenDimensions {
    /// Canonical IBM 3270 / VT100 `24x80` screen geometry.
    pub const STANDARD_24X80: Self = Self {
        rows: ScreenRow::STANDARD_24,
        cols: ScreenCol::STANDARD_80,
    };

    /// Constructs [`ScreenDimensions`] from validated row and column counts.
    ///
    /// # Arguments
    ///
    /// * `rows` - Terminal height (`1..=999`).
    /// * `cols` - Terminal width (`1..=999`).
    #[must_use]
    pub const fn new(rows: ScreenRow, cols: ScreenCol) -> Self {
        Self { rows, cols }
    }

    /// Resolves screen dimensions from `COB_SCREEN_LINES` / `LINES` and `COB_SCREEN_COLS` / `COLUMNS`,
    /// falling back to [`Self::STANDARD_24X80`] when unset or invalid.
    ///
    /// # Arguments
    ///
    /// * `env_lookup` - Callback resolving host environment variable values by name.
    #[must_use]
    pub fn from_env_lookup(env_lookup: &mut dyn FnMut(&str) -> Option<String>) -> Self {
        let rows = parse_dim_var(env_lookup, "COB_SCREEN_LINES", "LINES")
            .and_then(|n| ScreenRow::try_new(n).ok())
            .unwrap_or(ScreenRow::STANDARD_24);
        let cols = parse_dim_var(env_lookup, "COB_SCREEN_COLS", "COLUMNS")
            .and_then(|n| ScreenCol::try_new(n).ok())
            .unwrap_or(ScreenCol::STANDARD_80);
        Self { rows, cols }
    }

    /// Returns total cell count (`rows * cols`).
    #[must_use]
    pub const fn cell_count(self) -> usize {
        (self.rows.get() as usize) * (self.cols.get() as usize)
    }
}

/// Looks up a primary or fallback positive `u16` environment variable.
///
/// # Arguments
///
/// * `env_lookup` - Environment variable lookup function.
/// * `primary` - Primary `COB_SCREEN_*` variable name.
/// * `fallback` - Secondary standard POSIX `LINES`/`COLUMNS` variable name.
fn parse_dim_var(
    env_lookup: &mut dyn FnMut(&str) -> Option<String>,
    primary: &str,
    fallback: &str,
) -> Option<u16> {
    env_lookup(primary)
        .or_else(|| env_lookup(fallback))
        .and_then(|raw| raw.trim().parse::<u16>().ok())
}

/// Intensity tier when decoding extended 16-color (`0..=15`) COBOL color numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorIntensity {
    /// Standard palette intensity (`0..=7`).
    Standard,
    /// High-intensity / bright palette (`8..=15`).
    High,
}

/// ISO/IEC 1989:2023 & `GnuCOBOL` 8-color `SCREEN SECTION` color palette (`0..=7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenColor {
    /// Color `0`: Black.
    Black = 0,
    /// Color `1`: Blue.
    Blue = 1,
    /// Color `2`: Green.
    Green = 2,
    /// Color `3`: Cyan.
    Cyan = 3,
    /// Color `4`: Red.
    Red = 4,
    /// Color `5`: Magenta.
    Magenta = 5,
    /// Color `6`: Brown (Yellow when highlighted).
    Brown = 6,
    /// Color `7`: White (Light grey unless highlighted).
    White = 7,
}

impl ScreenColor {
    /// Decodes a standard COBOL color integer (`0..=7`) into [`ScreenColor`].
    ///
    /// # Arguments
    ///
    /// * `code` - COBOL color code (`0..=7`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `code > 7`.
    pub fn from_cobol_code(code: u8) -> Result<Self> {
        match code {
            0 => Ok(Self::Black),
            1 => Ok(Self::Blue),
            2 => Ok(Self::Green),
            3 => Ok(Self::Cyan),
            4 => Ok(Self::Red),
            5 => Ok(Self::Magenta),
            6 => Ok(Self::Brown),
            7 => Ok(Self::White),
            other => Err(CoblangError::InvalidDomainValue {
                domain_type: "ScreenColor",
                value: other.to_string(),
                reason: String::from("COBOL screen color must be in 0..=7"),
            }),
        }
    }

    /// Decodes an extended 16-color COBOL color integer (`0..=15`) into `(ScreenColor, ColorIntensity)`.
    ///
    /// # Arguments
    ///
    /// * `code` - Extended COBOL color code (`0..=15`, where `8..=15` set [`ColorIntensity::High`]).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `code > 15`.
    pub fn from_extended_code(code: u8) -> Result<(Self, ColorIntensity)> {
        if code > 15 {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "ScreenColor",
                value: code.to_string(),
                reason: String::from("extended COBOL screen color must be in 0..=15"),
            });
        }
        let intensity = if code >= 8 {
            ColorIntensity::High
        } else {
            ColorIntensity::Standard
        };
        let base = match code & 0x07 {
            0 => Self::Black,
            1 => Self::Blue,
            2 => Self::Green,
            3 => Self::Cyan,
            4 => Self::Red,
            5 => Self::Magenta,
            6 => Self::Brown,
            _ => Self::White,
        };
        Ok((base, intensity))
    }

    /// Parses a COBOL color constant name (`"BLACK"`, `"BLUE"`, `"YELLOW"`, etc.).
    ///
    /// # Arguments
    ///
    /// * `name` - Case-insensitive COBOL color keyword.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `name` is not a recognized color name.
    pub fn from_keyword(name: &str) -> Result<Self> {
        match name.trim().to_ascii_uppercase().as_str() {
            "BLACK" => Ok(Self::Black),
            "BLUE" => Ok(Self::Blue),
            "GREEN" => Ok(Self::Green),
            "CYAN" => Ok(Self::Cyan),
            "RED" => Ok(Self::Red),
            "MAGENTA" => Ok(Self::Magenta),
            "BROWN" | "YELLOW" => Ok(Self::Brown),
            "WHITE" => Ok(Self::White),
            _ => Err(CoblangError::InvalidDomainValue {
                domain_type: "ScreenColor",
                value: String::from(name),
                reason: String::from("unrecognized COBOL screen color keyword"),
            }),
        }
    }

    /// Returns the canonical COBOL integer code (`0..=7`).
    #[must_use]
    pub const fn to_cobol_code(self) -> u8 {
        self as u8
    }

    /// Returns the ANSI/ECMA-48 SGR foreground parameter (`30..=37`).
    ///
    /// Note that COBOL uses BGR bit ordering (`1 = Blue`, `4 = Red`), whereas ANSI SGR uses RGB
    /// (`31 = Red`, `34 = Blue`).
    #[must_use]
    pub const fn ansi_fg_sgr(self) -> u8 {
        match self {
            Self::Black => 30,
            Self::Red => 31,
            Self::Green => 32,
            Self::Brown => 33,
            Self::Blue => 34,
            Self::Magenta => 35,
            Self::Cyan => 36,
            Self::White => 37,
        }
    }

    /// Returns the ANSI/ECMA-48 SGR background parameter (`40..=47`).
    #[must_use]
    pub const fn ansi_bg_sgr(self) -> u8 {
        self.ansi_fg_sgr() + 10
    }
}

/// Validated 4-digit COBOL `CRT STATUS` code (`0000..=9999`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CrtStatusCode(
    /// Validated status integer in `0..=9999`.
    u16,
);

impl CrtStatusCode {
    /// `0000`: Normal `ACCEPT` completion (Enter / Return / `AUTO` skip completion).
    pub const NORMAL_COMPLETION: Self = Self(0);
    /// `0002`: Field truncated or validation warning.
    pub const FIELD_TRUNCATED: Self = Self(2);
    /// `2001`: `PageUp` key.
    pub const PAGE_UP: Self = Self(2001);
    /// `2002`: `PageDown` key.
    pub const PAGE_DOWN: Self = Self(2002);
    /// `2003`: `Up` arrow key.
    pub const UP_ARROW: Self = Self(2003);
    /// `2004`: `Down` arrow key.
    pub const DOWN_ARROW: Self = Self(2004);
    /// `2005`: `Escape` key abort.
    pub const ESCAPE: Self = Self(2005);
    /// `2006`: Print key.
    pub const PRINT_KEY: Self = Self(2006);
    /// `2007`: Forward `Tab` key when exiting single-field `ACCEPT`.
    pub const TAB_FORWARD: Self = Self(2007);
    /// `2008`: Reverse `Shift+Tab` (`BackTab`) key when exiting single-field `ACCEPT`.
    pub const BACK_TAB: Self = Self(2008);
    /// `2009`: `Left` arrow key.
    pub const LEFT_ARROW: Self = Self(2009);
    /// `2010`: `Right` arrow key.
    pub const RIGHT_ARROW: Self = Self(2010);
    /// `2011`: `Insert` key.
    pub const INSERT_KEY: Self = Self(2011);
    /// `2012`: `Delete` key.
    pub const DELETE_KEY: Self = Self(2012);
    /// `2013`: `Backspace` key.
    pub const BACKSPACE_KEY: Self = Self(2013);
    /// `2014`: `Home` key.
    pub const HOME_KEY: Self = Self(2014);
    /// `2015`: `End` key.
    pub const END_KEY: Self = Self(2015);
    /// `8000`: `ACCEPT ... TIME-OUT` expiration with no user input.
    pub const TIMEOUT: Self = Self(8000);
    /// `8001`: End-of-file (`EOF`) encountered on terminal/keystroke input stream.
    pub const INPUT_EOF: Self = Self(8001);
    /// `9000`: Fatal or unrecoverable screen condition (`REQUIRED`/`FULL` unsatisfied at EOF).
    pub const SCREEN_ERROR: Self = Self(9000);

    /// Maximum 4-digit `CRT STATUS` value (`9999`).
    pub const MAX_CODE: u16 = 9999;

    /// Constructs a validated [`CrtStatusCode`] in `0..=9999`.
    ///
    /// # Arguments
    ///
    /// * `raw` - Four-digit status code (`> 9999` is rejected).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw > 9999`.
    pub fn try_new(raw: u16) -> Result<Self> {
        if raw <= Self::MAX_CODE {
            Ok(Self(raw))
        } else {
            Err(CoblangError::InvalidDomainValue {
                domain_type: "CrtStatusCode",
                value: raw.to_string(),
                reason: String::from("CRT STATUS code must be in 0..=9999"),
            })
        }
    }

    /// Constructs the `CRT STATUS` code (`1001..=1064`) for function key `F1..=F64`.
    ///
    /// # Arguments
    ///
    /// * `key_number` - One-based function key index (`1..=64`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `key_number == 0` or `key_number > 64`.
    pub fn function_key(key_number: u8) -> Result<Self> {
        if (1..=64).contains(&key_number) {
            Ok(Self(1000 + u16::from(key_number)))
        } else {
            Err(CoblangError::InvalidDomainValue {
                domain_type: "CrtStatusCode::function_key",
                value: key_number.to_string(),
                reason: String::from("function key number must be in 1..=64"),
            })
        }
    }

    /// Returns the numeric `0..=9999` value of this status code.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// Formats this status code into 4 ASCII decimal digits (`b"0000"`..`b"9999"`).
    #[must_use]
    pub const fn to_four_ascii_digits(self) -> [u8; 4] {
        let v = self.0;
        let d0 = ((v / 1000) % 10) as u8 + b'0';
        let d1 = ((v / 100) % 10) as u8 + b'0';
        let d2 = ((v / 10) % 10) as u8 + b'0';
        let d3 = (v % 10) as u8 + b'0';
        [d0, d1, d2, d3]
    }

    /// Encodes this status code in X/Open 3-byte `CRT STATUS` format (`[key_1, key_2, 0]`).
    ///
    /// * `'0'` (`0x30`): Normal termination (`[b'0', b'0', 0]`).
    /// * `'1'` (`0x31`): Function key (`1001..=1064` -> `[b'1', fn_num, 0]`).
    /// * `'2'` (`0x32`): Special exception key (`2001..=2099` -> `[b'2', code - 2000, 0]`).
    /// * `'9'` (`0x39`): Timeout, EOF, or screen error (`[b'9', high_class, 0]`).
    #[must_use]
    pub const fn to_xopen_three_bytes(self) -> [u8; 3] {
        match self.0 {
            0..=999 => {
                let [rem, _] = (self.0 % 100).to_le_bytes();
                [b'0', rem.wrapping_add(b'0'), 0]
            }
            1001..=1064 => {
                let [fn_num, _] = (self.0 - 1000).to_le_bytes();
                [b'1', fn_num, 0]
            }
            2001..=2099 => {
                let [sp_num, _] = (self.0 - 2000).to_le_bytes();
                [b'2', sp_num, 0]
            }
            _ => {
                let [cls, _] = ((self.0 / 1000) % 10).to_le_bytes();
                [b'9', cls, 0]
            }
        }
    }

    /// Writes `self` into a COBOL `CRT STATUS` special register buffer (`4`-byte standard ASCII or `3`-byte X/Open).
    ///
    /// # Arguments
    ///
    /// * `register_buf` - Target `CRT STATUS` storage slice (`3` bytes for X/Open or `>= 4` bytes for `PIC 9(4)`/`PIC X(4)`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ScreenBackend`] if `register_buf.len() < 3`.
    pub fn write_to_register(self, register_buf: &mut [u8]) -> Result<()> {
        if register_buf.len() == 3 {
            let xopen = self.to_xopen_three_bytes();
            register_buf.copy_from_slice(&xopen);
            Ok(())
        } else if let Some(prefix) = register_buf.get_mut(..4) {
            let digits = self.to_four_ascii_digits();
            prefix.copy_from_slice(&digits);
            for slot in register_buf.iter_mut().skip(4) {
                *slot = b' ';
            }
            Ok(())
        } else {
            Err(CoblangError::ScreenBackend {
                message: format!(
                    "CRT STATUS register must be at least 3 bytes, got {}",
                    register_buf.len()
                ),
            })
        }
    }
}

/// Bitmask newtype encoding all `SCREEN SECTION` and extended `DISPLAY`/`ACCEPT` field attributes.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ScreenAttrFlags(
    /// Raw attribute bitmask.
    u32,
);

impl ScreenAttrFlags {
    /// Empty attribute set (`0`).
    pub const NONE: Self = Self(0);
    /// `BLANK SCREEN`: Clear the entire screen and home cursor before rendering.
    pub const BLANK_SCREEN: Self = Self(1 << 0);
    /// `BLANK LINE`: Clear the target row before rendering.
    pub const BLANK_LINE: Self = Self(1 << 1);
    /// `ERASE EOL`: Clear from starting column to end of line.
    pub const ERASE_EOL: Self = Self(1 << 2);
    /// `ERASE EOS`: Clear from starting position to end of screen.
    pub const ERASE_EOS: Self = Self(1 << 3);
    /// `BELL` / `BEEP`: Emit audible/terminal alert (`0x07`).
    pub const BELL: Self = Self(1 << 4);
    /// `HIGHLIGHT`: High-intensity / bold foreground rendering.
    pub const HIGHLIGHT: Self = Self(1 << 5);
    /// `LOWLIGHT`: Dim / half-bright foreground rendering.
    pub const LOWLIGHT: Self = Self(1 << 6);
    /// `REVERSE-VIDEO`: Swap foreground and background colors.
    pub const REVERSE_VIDEO: Self = Self(1 << 7);
    /// `UNDERLINE`: Underscore each character cell.
    pub const UNDERLINE: Self = Self(1 << 8);
    /// `BLINK`: Blinking character attribute.
    pub const BLINK: Self = Self(1 << 9);
    /// `LEFTLINE`: Left vertical cell border attribute.
    pub const LEFTLINE: Self = Self(1 << 10);
    /// `OVERLINE`: Top horizontal cell border attribute (`SGR 53`).
    pub const OVERLINE: Self = Self(1 << 11);
    /// `AUTO` / `AUTO-SKIP`: Automatically advance/complete when final character is typed.
    pub const AUTO: Self = Self(1 << 12);
    /// `SECURE` / `NO-ECHO`: Mask displayed/echoed input characters with `'*'`.
    pub const SECURE: Self = Self(1 << 13);
    /// `REQUIRED` / `EMPTY-CHECK`: Reject blank/zero-only submission during `ACCEPT`.
    pub const REQUIRED: Self = Self(1 << 14);
    /// `FULL` / `LENGTH-CHECK`: Require field to be either completely filled or completely empty.
    pub const FULL: Self = Self(1 << 15);
    /// `PROMPT`: Fill unoccupied editable character cells with a prompt character (`'_'` by default).
    pub const PROMPT: Self = Self(1 << 16);
    /// `ZERO-FILL`: Pad unused numeric/alphanumeric input positions with `'0'`.
    pub const ZERO_FILL: Self = Self(1 << 17);
    /// `BLANK WHEN ZERO`: Render numeric zero values as all spaces.
    pub const BLANK_WHEN_ZERO: Self = Self(1 << 18);
    /// `JUSTIFIED RIGHT`: Right-align alphanumeric values.
    pub const JUSTIFIED_RIGHT: Self = Self(1 << 19);
    /// `UPDATE`: Pre-populate `TO` / `ACCEPT` edit buffer with existing target field bytes.
    pub const UPDATE: Self = Self(1 << 20);
    /// `INITIAL`: Reset input state to initial `VALUE` / spaces on entry.
    pub const INITIAL: Self = Self(1 << 21);
    /// `GRAPHICS`: Translate DEC line-drawing character codes (`q`, `x`, `l`, `k`, `m`, `j`, `n`).
    pub const GRAPHICS: Self = Self(1 << 22);

    /// Mask of one-shot action directives (`BLANK SCREEN`, `BLANK LINE`, `ERASE EOL`, `ERASE EOS`, `BELL`)
    /// that execute once at group entry rather than duplicating onto every child item.
    pub const GROUP_ENTRY_DIRECTIVES: Self = Self(
        Self::BLANK_SCREEN.0
            | Self::BLANK_LINE.0
            | Self::ERASE_EOL.0
            | Self::ERASE_EOS.0
            | Self::BELL.0,
    );

    /// Mask of visual cell styling flags preserved inside a [`CellStyle`].
    pub const VISUAL_STYLE_MASK: Self = Self(
        Self::HIGHLIGHT.0
            | Self::LOWLIGHT.0
            | Self::REVERSE_VIDEO.0
            | Self::UNDERLINE.0
            | Self::BLINK.0
            | Self::LEFTLINE.0
            | Self::OVERLINE.0
            | Self::GRAPHICS.0,
    );

    /// Constructs an empty [`ScreenAttrFlags`] set.
    #[must_use]
    pub const fn empty() -> Self {
        Self::NONE
    }

    /// Constructs [`ScreenAttrFlags`] from raw bitmask bits.
    ///
    /// # Arguments
    ///
    /// * `bits` - Raw flag bitmask.
    #[must_use]
    pub const fn from_bits_retain(bits: u32) -> Self {
        Self(bits)
    }

    /// Returns the raw `u32` bitmask.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Returns `true` when no flags are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` when all bits in `other` are present in `self`.
    ///
    /// # Arguments
    ///
    /// * `other` - Flag mask to test.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns the union of `self` and `other`.
    ///
    /// # Arguments
    ///
    /// * `other` - Additional flags to include.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns `self` with all bits in `other` cleared.
    ///
    /// # Arguments
    ///
    /// * `other` - Flags to remove.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Inserts `other` into `self` in place.
    ///
    /// # Arguments
    ///
    /// * `other` - Flags to set.
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Removes `other` from `self` in place.
    ///
    /// # Arguments
    ///
    /// * `other` - Flags to clear.
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    /// Merges child flags over inherited parent flags, resolving mutually exclusive intensity flags
    /// (`HIGHLIGHT` vs `LOWLIGHT`).
    ///
    /// # Arguments
    ///
    /// * `child` - Explicitly declared child attribute flags.
    #[must_use]
    pub const fn inherit_into_child(self, child: Self) -> Self {
        let mut base = self.without(Self::GROUP_ENTRY_DIRECTIVES);
        if child.contains(Self::HIGHLIGHT) {
            base = base.without(Self::LOWLIGHT);
        }
        if child.contains(Self::LOWLIGHT) {
            base = base.without(Self::HIGHLIGHT);
        }
        base.union(child)
    }

    /// Parses a single uppercase COBOL `SCREEN SECTION` attribute keyword into [`ScreenAttrFlags`],
    /// returning `None` for unrecognized keywords.
    ///
    /// # Arguments
    ///
    /// * `keyword` - Uppercase or mixed-case COBOL keyword (e.g. `"HIGHLIGHT"`, `"REVERSE-VIDEO"`).
    #[must_use]
    pub fn from_keyword(keyword: &str) -> Option<Self> {
        match keyword.trim().to_ascii_uppercase().as_str() {
            "BELL" | "BEEP" => Some(Self::BELL),
            "HIGHLIGHT" => Some(Self::HIGHLIGHT),
            "LOWLIGHT" => Some(Self::LOWLIGHT),
            "REVERSE-VIDEO" | "REVERSE" => Some(Self::REVERSE_VIDEO),
            "UNDERLINE" => Some(Self::UNDERLINE),
            "BLINK" => Some(Self::BLINK),
            "LEFTLINE" => Some(Self::LEFTLINE),
            "OVERLINE" => Some(Self::OVERLINE),
            "AUTO" | "AUTO-SKIP" | "AUTOTERMINATE" => Some(Self::AUTO),
            "SECURE" | "NO-ECHO" => Some(Self::SECURE),
            "REQUIRED" | "EMPTY-CHECK" => Some(Self::REQUIRED),
            "FULL" | "LENGTH-CHECK" => Some(Self::FULL),
            "PROMPT" => Some(Self::PROMPT),
            "ZERO-FILL" => Some(Self::ZERO_FILL),
            "JUSTIFIED" | "JUST" => Some(Self::JUSTIFIED_RIGHT),
            "UPDATE" => Some(Self::UPDATE),
            "INITIAL" => Some(Self::INITIAL),
            "GRAPHICS" => Some(Self::GRAPHICS),
            _ => None,
        }
    }
}

/// Prompt fill specification for editable `SCREEN SECTION` fields (`PROMPT [CHARACTER IS ch]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PromptSpec {
    /// Visible filler character drawn in unedited positions (`'_'` by default).
    pub character: char,
}

impl Default for PromptSpec {
    /// Returns the standard underscore (`'_'`) prompt specification.
    fn default() -> Self {
        Self::UNDERSCORE
    }
}

impl PromptSpec {
    /// Default ISO COBOL / `GnuCOBOL` underscore prompt (`'_'`).
    pub const UNDERSCORE: Self = Self { character: '_' };

    /// Constructs a [`PromptSpec`] with a custom visible filler `character`.
    ///
    /// # Arguments
    ///
    /// * `character` - Visible prompt character (control characters `< ' '` normalize to `'_'`).
    #[must_use]
    pub const fn new(character: char) -> Self {
        if (character as u32) < 0x20 {
            Self::UNDERSCORE
        } else {
            Self { character }
        }
    }
}

/// Resolved visual styling for a single terminal character cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CellStyle {
    /// Optional foreground color (`None` uses terminal default foreground).
    pub fg: Option<ScreenColor>,
    /// Optional background color (`None` uses terminal default background).
    pub bg: Option<ScreenColor>,
    /// Visual attribute flags (`HIGHLIGHT`, `LOWLIGHT`, `REVERSE_VIDEO`, `UNDERLINE`, `BLINK`, `LEFTLINE`, `OVERLINE`, `GRAPHICS`).
    pub attrs: ScreenAttrFlags,
}

impl CellStyle {
    /// Default unstyled terminal cell appearance.
    pub const DEFAULT: Self = Self {
        fg: None,
        bg: None,
        attrs: ScreenAttrFlags::NONE,
    };

    /// Constructs a [`CellStyle`] masking `attrs` to [`ScreenAttrFlags::VISUAL_STYLE_MASK`].
    ///
    /// # Arguments
    ///
    /// * `fg` - Optional foreground color.
    /// * `bg` - Optional background color.
    /// * `attrs` - Attribute flags (only visual style bits are retained).
    #[must_use]
    pub const fn new(
        fg: Option<ScreenColor>,
        bg: Option<ScreenColor>,
        attrs: ScreenAttrFlags,
    ) -> Self {
        Self {
            fg,
            bg,
            attrs: ScreenAttrFlags::from_bits_retain(
                attrs.bits() & ScreenAttrFlags::VISUAL_STYLE_MASK.bits(),
            ),
        }
    }
}

/// Validated 1-based `(row, col)` cursor position on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CursorPosition {
    /// One-based screen row.
    pub row: ScreenRow,
    /// One-based screen column.
    pub col: ScreenCol,
}

impl Default for CursorPosition {
    /// Returns the top-left home cursor `(1, 1)`.
    fn default() -> Self {
        Self::HOME
    }
}

impl CursorPosition {
    /// Top-left home cursor `(row = 1, col = 1)`.
    pub const HOME: Self = Self {
        row: ScreenRow::ONE,
        col: ScreenCol::ONE,
    };

    /// Constructs a [`CursorPosition`] at `(row, col)`.
    ///
    /// # Arguments
    ///
    /// * `row` - One-based screen row.
    /// * `col` - One-based screen column.
    #[must_use]
    pub const fn new(row: ScreenRow, col: ScreenCol) -> Self {
        Self { row, col }
    }

    /// Constructs a validated [`CursorPosition`] from raw 1-based `(row, col)` integers.
    ///
    /// # Arguments
    ///
    /// * `row` - Raw 1-based row (`1..=999`).
    /// * `col` - Raw 1-based column (`1..=999`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if either `row` or `col` is outside `1..=999`.
    pub fn try_from_raw(row: u16, col: u16) -> Result<Self> {
        Ok(Self {
            row: ScreenRow::try_new(row)?,
            col: ScreenCol::try_new(col)?,
        })
    }

    /// Clamps `self` to stay within `dims`.
    ///
    /// # Arguments
    ///
    /// * `dims` - Active terminal grid bounds.
    #[must_use]
    pub const fn clamp_to(self, dims: ScreenDimensions) -> Self {
        Self {
            row: self.row.clamp_max(dims.rows),
            col: self.col.clamp_max(dims.cols),
        }
    }
}

/// Encoding layout for the `SPECIAL-NAMES. CURSOR IS ...` special register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorFormat {
    /// Four-digit `LLCC` format (`PIC 9(4)` / `PIC X(4)`: `row` in `01..=99`, `col` in `01..=99`).
    FourDigitLlCc,
    /// Six-digit `LLLCCC` format (`PIC 9(6)` / `PIC X(6)`: `row` in `001..=999`, `col` in `001..=999`).
    SixDigitLllCcc,
}

impl CursorFormat {
    /// Writes `pos` into `register_buf` using 4-digit (`LLCC`) or 6-digit (`LLLCCC`) ASCII decimal digits.
    ///
    /// Coordinates exceeding `99` in [`Self::FourDigitLlCc`] saturate at `99`.
    ///
    /// # Arguments
    ///
    /// * `pos` - Current 1-based [`CursorPosition`].
    /// * `register_buf` - Target `CURSOR` storage buffer (`4` bytes for `FourDigitLlCc`, `6` bytes for `SixDigitLllCcc`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ScreenBackend`] if `register_buf` is shorter than the required width.
    pub fn encode(self, pos: CursorPosition, register_buf: &mut [u8]) -> Result<()> {
        match self {
            Self::FourDigitLlCc => {
                let Some(out) = register_buf.get_mut(..4) else {
                    return Err(CoblangError::ScreenBackend {
                        message: format!(
                            "4-digit CURSOR register requires at least 4 bytes, got {}",
                            register_buf.len()
                        ),
                    });
                };
                let r = pos.row.get().min(99);
                let c = pos.col.get().min(99);
                let bytes = [
                    ((r / 10) % 10) as u8 + b'0',
                    (r % 10) as u8 + b'0',
                    ((c / 10) % 10) as u8 + b'0',
                    (c % 10) as u8 + b'0',
                ];
                out.copy_from_slice(&bytes);
                Ok(())
            }
            Self::SixDigitLllCcc => {
                let Some(out) = register_buf.get_mut(..6) else {
                    return Err(CoblangError::ScreenBackend {
                        message: format!(
                            "6-digit CURSOR register requires at least 6 bytes, got {}",
                            register_buf.len()
                        ),
                    });
                };
                let r = pos.row.get();
                let c = pos.col.get();
                let bytes = [
                    ((r / 100) % 10) as u8 + b'0',
                    ((r / 10) % 10) as u8 + b'0',
                    (r % 10) as u8 + b'0',
                    ((c / 100) % 10) as u8 + b'0',
                    ((c / 10) % 10) as u8 + b'0',
                    (c % 10) as u8 + b'0',
                ];
                out.copy_from_slice(&bytes);
                Ok(())
            }
        }
    }

    /// Decodes a 4-byte (`LLCC`) or 6-byte (`LLLCCC`) `CURSOR` register buffer into an optional [`CursorPosition`].
    ///
    /// Returns `Ok(None)` when either row or column is `0` or non-numeric (indicating default cursor placement).
    ///
    /// # Arguments
    ///
    /// * `register_buf` - Raw `CURSOR` special register bytes (`4` or `6` bytes).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ScreenBackend`] if `register_buf` is shorter than the required width.
    pub fn decode(self, register_buf: &[u8]) -> Result<Option<CursorPosition>> {
        let (row_slice, col_slice) = match self {
            Self::FourDigitLlCc => {
                let (Some(r), Some(c)) = (register_buf.get(..2), register_buf.get(2..4)) else {
                    return Err(CoblangError::ScreenBackend {
                        message: format!(
                            "4-digit CURSOR register requires at least 4 bytes, got {}",
                            register_buf.len()
                        ),
                    });
                };
                (r, c)
            }
            Self::SixDigitLllCcc => {
                let (Some(r), Some(c)) = (register_buf.get(..3), register_buf.get(3..6)) else {
                    return Err(CoblangError::ScreenBackend {
                        message: format!(
                            "6-digit CURSOR register requires at least 6 bytes, got {}",
                            register_buf.len()
                        ),
                    });
                };
                (r, c)
            }
        };
        let (Some(r), Some(c)) = (parse_ascii_digits(row_slice), parse_ascii_digits(col_slice))
        else {
            return Ok(None);
        };
        let pos = ScreenRow::try_new(r)
            .ok()
            .zip(ScreenCol::try_new(c).ok())
            .map(|(row, col)| CursorPosition::new(row, col));
        Ok(pos)
    }
}

/// Parses an ASCII digit slice (`b'0'..=b'9'`, treating space `' '` as leading `'0'`) into `u16`.
///
/// # Arguments
///
/// * `bytes` - Slice of ASCII digit/space bytes.
fn parse_ascii_digits(bytes: &[u8]) -> Option<u16> {
    let mut acc = 0_u16;
    for &b in bytes {
        let digit = match b {
            b'0'..=b'9' => u16::from(b - b'0'),
            b' ' => 0,
            _ => return None,
        };
        acc = acc.saturating_mul(10).saturating_add(digit);
    }
    Some(acc)
}

/// Terminal cursor visibility mode (`curs_set` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CursorVisibility {
    /// Cursor hidden (`0`).
    Hidden,
    /// Standard visible cursor (`1`).
    #[default]
    Normal,
    /// High-visibility block cursor (`2`).
    High,
}

/// Strongly-typed terminal keystroke or input-stream event consumed during `ACCEPT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyEvent {
    /// Printable Unicode character input.
    Char(char),
    /// `Enter` / `Return` (`CR` `0x0D` or `LF` `0x0A`).
    Enter,
    /// `Escape` key (`0x1B`).
    Escape,
    /// Forward `Tab` (`HT` `0x09`).
    Tab,
    /// Reverse `Shift+Tab` (`BackTab`).
    BackTab,
    /// `Backspace` (`0x08` or `0x7F`).
    Backspace,
    /// `Delete` key (`CSI 3 ~`).
    Delete,
    /// `Alt+Delete` (`clear to end of field`).
    AltDelete,
    /// `Insert` mode toggle key (`CSI 2 ~`).
    Insert,
    /// `Home` key (move to start of field).
    Home,
    /// `End` key (move after last non-space character in field).
    End,
    /// `Left` arrow key.
    Left,
    /// `Right` arrow key.
    Right,
    /// `Alt+Left` word-left key.
    AltLeft,
    /// `Alt+Right` word-right key.
    AltRight,
    /// `Up` arrow key.
    Up,
    /// `Down` arrow key.
    Down,
    /// `PageUp` key.
    PageUp,
    /// `PageDown` key.
    PageDown,
    /// Numbered function key `F1`..=`F64`.
    Function(u8),
    /// Input timeout expiration (`ACCEPT ... TIME-OUT`).
    Timeout,
    /// End-of-file on input stream.
    Eof,
}

impl KeyEvent {
    /// Maps a terminating [`KeyEvent`] to its corresponding [`CrtStatusCode`].
    #[must_use]
    pub fn to_crt_status(self) -> CrtStatusCode {
        match self {
            Self::Enter | Self::Char(_) => CrtStatusCode::NORMAL_COMPLETION,
            Self::Escape => CrtStatusCode::ESCAPE,
            Self::Tab => CrtStatusCode::TAB_FORWARD,
            Self::BackTab => CrtStatusCode::BACK_TAB,
            Self::Backspace => CrtStatusCode::BACKSPACE_KEY,
            Self::Delete | Self::AltDelete => CrtStatusCode::DELETE_KEY,
            Self::Insert => CrtStatusCode::INSERT_KEY,
            Self::Home => CrtStatusCode::HOME_KEY,
            Self::End => CrtStatusCode::END_KEY,
            Self::Left | Self::AltLeft => CrtStatusCode::LEFT_ARROW,
            Self::Right | Self::AltRight => CrtStatusCode::RIGHT_ARROW,
            Self::Up => CrtStatusCode::UP_ARROW,
            Self::Down => CrtStatusCode::DOWN_ARROW,
            Self::PageUp => CrtStatusCode::PAGE_UP,
            Self::PageDown => CrtStatusCode::PAGE_DOWN,
            Self::Function(n) => {
                CrtStatusCode::function_key(n).unwrap_or(CrtStatusCode::SCREEN_ERROR)
            }
            Self::Timeout => CrtStatusCode::TIMEOUT,
            Self::Eof => CrtStatusCode::INPUT_EOF,
        }
    }
}

/// ISO/IEC 1989:2023 `EC-SCREEN-*` runtime condition categories detected during layout resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenExceptionKind {
    /// `EC-SCREEN-LINE-NUMBER`: Resolved row exceeds terminal height.
    LineOutOfBounds {
        /// Requested 1-based row.
        requested_row: u16,
        /// Terminal row limit.
        max_row: u16,
    },
    /// `EC-SCREEN-STARTING-COLUMN`: Resolved starting column exceeds terminal width.
    StartingColumnOutOfBounds {
        /// Requested 1-based starting column.
        requested_col: u16,
        /// Terminal column limit.
        max_col: u16,
    },
    /// `EC-SCREEN-FIELD-OVERLAP`: Two elementary screen items overlap on the same row.
    FieldOverlap {
        /// Shared 1-based row where overlap occurred.
        row: ScreenRow,
        /// Overlapping 1-based column.
        col: ScreenCol,
    },
    /// `EC-SCREEN-IMP`: Implementor-defined screen condition (e.g. unsatisfied `REQUIRED`/`FULL` at EOF).
    ImplementorCondition,
}

impl ScreenExceptionKind {
    /// Maps this screen condition to its canonical `coblang_rt_core` [`ExceptionCode`].
    #[must_use]
    pub const fn exception_code(self) -> ExceptionCode {
        match self {
            Self::LineOutOfBounds { .. } => ExceptionCode::EcScreenLineNumber,
            Self::StartingColumnOutOfBounds { .. } => ExceptionCode::EcScreenStartingColumn,
            Self::FieldOverlap { .. } => ExceptionCode::EcScreenFieldOverlap,
            Self::ImplementorCondition => ExceptionCode::EcScreenImp,
        }
    }
}
