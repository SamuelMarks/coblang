//! National (`UTF-16`), `UTF-8`, module reflection, `EXCEPTION-*`, and `UUID4` intrinsics (`Milestone M12`).

use coblang_common::Result;
use coblang_decimal::charset::{Codepage, UnmappedCharPolicy, transcode_bytes};

use crate::ec_argument_function;

/// Fixed 31-byte field width mandated by ISO COBOL for `EXCEPTION-STATUS` and `EXCEPTION-STATEMENT`.
pub const EXCEPTION_REGISTER_WIDTH: usize = 31;

/// Byte-order selector for `USAGE NATIONAL` (`UTF-16`) intrinsics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NationalByteOrder {
    /// Big-endian `UTF-16BE` (canonical `coblang` `PIC N` representation).
    BigEndian,
    /// Little-endian `UTF-16LE`.
    LittleEndian,
}

impl NationalByteOrder {
    /// Maps `self` to its corresponding `coblang-decimal` [`Codepage`].
    #[must_use]
    pub const fn to_codepage(self) -> Codepage {
        match self {
            Self::BigEndian => Codepage::Utf16Be,
            Self::LittleEndian => Codepage::Utf16Le,
        }
    }
}

/// Borrowed Unicode buffer input for ISO 2023 `ULENGTH`, `UPOS`, `USUBSTR`,
/// `USUPPLEMENTARY`, `UVALID`, and `UWIDTH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnicodeSlice<'a> {
    /// `UTF-8` encoded character bytes (`USAGE UTF-8` / `PIC U` / `PIC X`).
    Utf8(&'a [u8]),
    /// `UTF-16BE` encoded character bytes (`USAGE NATIONAL`).
    Utf16Be(&'a [u8]),
    /// `UTF-16LE` encoded character bytes.
    Utf16Le(&'a [u8]),
}

/// Compile-time and call-stack metadata backing `MODULE-*` intrinsics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleReflectionInfo {
    /// Active `PROGRAM-ID` / `FUNCTION-ID` (`MODULE-ID` / `MODULE-NAME`).
    pub module_id: String,
    /// Calling program/function identifier (`MODULE-CALLER-ID`, empty on root entry).
    pub caller_id: String,
    /// 8-character `"YYYYMMDD"` compilation date (`MODULE-DATE`).
    pub compile_date_yyyymmdd: [u8; 8],
    /// 6-character `"HHMMSS"` compilation time (`MODULE-TIME`).
    pub compile_time_hhmmss: [u8; 6],
    /// ISO 8601 formatted compilation timestamp (`MODULE-FORMATTED-DATE`).
    pub formatted_compile_date: String,
    /// Physical executable / shared-module path (`MODULE-PATH`).
    pub module_path: String,
    /// Compilation source file name (`MODULE-SOURCE`).
    pub module_source: String,
}

/// Active thread/run-unit exception snapshot backing `EXCEPTION-*` intrinsics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExceptionReflectionState {
    /// Active `EC-*` condition name (`None` when no exception is active).
    pub status_name: Option<String>,
    /// COBOL statement verb that raised the last exception (`None` when none).
    pub statement_verb: Option<String>,
    /// `"program; paragraph; line"` location string (`None` -> `" "`).
    pub location: Option<String>,
    /// 2-byte `FILE STATUS` (`*b"00"` when not an I/O exception).
    pub io_status: [u8; 2],
    /// File-connector name associated with the last I/O exception.
    pub file_name: String,
}

impl Default for ExceptionReflectionState {
    /// Returns the cleared ISO COBOL initial exception state (no active exception).
    fn default() -> Self {
        Self::cleared()
    }
}

impl ExceptionReflectionState {
    /// Constructs a cleared [`ExceptionReflectionState`] (`io_status = "00"`, no active exception).
    #[must_use]
    pub const fn cleared() -> Self {
        Self {
            status_name: None,
            statement_verb: None,
            location: None,
            io_status: *b"00",
            file_name: String::new(),
        }
    }
}

/// Pure-Rust 128-bit `xorshift128+` entropy generator backing `FUNCTION UUID4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Uuid4Generator {
    /// First non-zero 64-bit state word.
    s0: u64,
    /// Second 64-bit state word.
    s1: u64,
}

impl Uuid4Generator {
    /// Constructs a deterministic [`Uuid4Generator`] from two 64-bit seed words.
    ///
    /// # Arguments
    ///
    /// * `seed_lo` - Low 64-bit seed word.
    /// * `seed_hi` - High 64-bit seed word.
    #[must_use]
    pub const fn from_seeds(seed_lo: u64, seed_hi: u64) -> Self {
        let s0 = seed_lo ^ 0xA076_1D64_78BD_642F;
        let s1 = seed_hi ^ 0xE703_7ED1_A0B4_28DB;
        Self {
            s0: if s0 == 0 { 1 } else { s0 },
            s1,
        }
    }

    /// Constructs a [`Uuid4Generator`] seeded from system wall-clock nanoseconds and stack address entropy.
    #[must_use]
    pub fn from_system_clock() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_nanos();
        let lo = u64::try_from(nanos & u128::from(u64::MAX)).unwrap_or(1);
        let hi = u64::try_from(nanos >> 64).unwrap_or(0) ^ 0x517C_C1B7_2722_0A95;
        Self::from_seeds(lo, hi)
    }

    /// Produces the next 64-bit pseudo-random word.
    const fn next_u64(&mut self) -> u64 {
        let mut x = self.s0;
        let y = self.s1;
        self.s0 = y;
        x ^= x << 23;
        self.s1 = x ^ y ^ (x >> 17) ^ (y >> 26);
        self.s1.wrapping_add(y)
    }

    /// Fills 16 raw entropy bytes from two consecutive 64-bit steps.
    fn next_entropy_16(&mut self) -> [u8; 16] {
        let [a0, a1, a2, a3, a4, a5, a6, a7] = self.next_u64().to_le_bytes();
        let [b0, b1, b2, b3, b4, b5, b6, b7] = self.next_u64().to_le_bytes();
        [
            a0, a1, a2, a3, a4, a5, a6, a7, b0, b1, b2, b3, b4, b5, b6, b7,
        ]
    }
}

/// Decoded Unicode scalar span used internally by `ULENGTH`, `UPOS`, `USUBSTR`, `USUPPLEMENTARY`, and `UWIDTH`.
#[derive(Debug, Clone, Copy)]
struct ScalarSpan {
    /// Decoded Unicode scalar value.
    scalar: char,
    /// `1`-based encoding unit position (`byte` index for `UTF-8`, `u16` code-unit index for `UTF-16`).
    unit_pos_1based: usize,
    /// `0`-based physical byte offset in the input buffer.
    byte_offset: usize,
    /// Physical byte width of this scalar (`1..=4` for `UTF-8`, `2` or `4` for `UTF-16`).
    byte_len: usize,
}

/// Evaluates `FUNCTION CHAR-NATIONAL(ordinal, endian)` (`1`-based Unicode ordinal -> `UTF-16` bytes).
///
/// # Arguments
///
/// * `ordinal` - `1`-based Unicode code-point ordinal (`1..=1,114,112`, excluding surrogates).
/// * `endian` - Target [`NationalByteOrder`].
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `ordinal - 1` is negative, exceeds `U+10FFFF`, or is a surrogate.
pub fn meta_char_national(ordinal: i64, endian: NationalByteOrder) -> Result<Vec<u8>> {
    let Some(zero_based) = ordinal.checked_sub(1) else {
        return Err(ec_argument_function(
            "CHAR-NATIONAL",
            ordinal.to_string(),
            "ordinal underflow",
        ));
    };
    let Ok(scalar_code) = u32::try_from(zero_based) else {
        return Err(ec_argument_function(
            "CHAR-NATIONAL",
            ordinal.to_string(),
            "ordinal must lie in 1..=1,114,112",
        ));
    };
    let Some(ch) = char::from_u32(scalar_code) else {
        return Err(ec_argument_function(
            "CHAR-NATIONAL",
            ordinal.to_string(),
            "ordinal does not map to a valid Unicode scalar value",
        ));
    };
    let mut u16_buf = [0_u16; 2];
    let units = ch.encode_utf16(&mut u16_buf);
    let mut out = Vec::with_capacity(units.len() * 2);
    for &unit in units.iter() {
        let pair = match endian {
            NationalByteOrder::BigEndian => unit.to_be_bytes(),
            NationalByteOrder::LittleEndian => unit.to_le_bytes(),
        };
        out.extend_from_slice(&pair);
    }
    Ok(out)
}

/// Evaluates `FUNCTION DISPLAY-OF(national_bytes, endian, target_codepage, policy)`.
///
/// # Arguments
///
/// * `national_bytes` - Source `UTF-16` national character bytes.
/// * `endian` - Source [`NationalByteOrder`].
/// * `target_codepage` - Destination display [`Codepage`] (`Ascii`, `Utf8`, `Cp037`, `Cp1140`, etc.).
/// * `policy` - [`UnmappedCharPolicy`] (`StrictError` or `Substitute(ch)`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `national_bytes` is ill-formed or contains unmapped characters under `StrictError`.
pub fn meta_display_of(
    national_bytes: &[u8],
    endian: NationalByteOrder,
    target_codepage: Codepage,
    policy: UnmappedCharPolicy,
) -> Result<Vec<u8>> {
    transcode_bytes(
        national_bytes,
        endian.to_codepage(),
        target_codepage,
        policy,
    )
    .map_err(|err| {
        ec_argument_function(
            "DISPLAY-OF",
            format!("len={}", national_bytes.len()),
            err.to_string(),
        )
    })
}

/// Evaluates `FUNCTION NATIONAL-OF(display_bytes, source_codepage, endian, policy)`.
///
/// # Arguments
///
/// * `display_bytes` - Source alphanumeric/`UTF-8`/EBCDIC bytes.
/// * `source_codepage` - Source [`Codepage`].
/// * `endian` - Target [`NationalByteOrder`].
/// * `policy` - [`UnmappedCharPolicy`] (`StrictError` or `Substitute(ch)`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `display_bytes` contains invalid source bytes under `StrictError`.
pub fn meta_national_of(
    display_bytes: &[u8],
    source_codepage: Codepage,
    endian: NationalByteOrder,
    policy: UnmappedCharPolicy,
) -> Result<Vec<u8>> {
    transcode_bytes(display_bytes, source_codepage, endian.to_codepage(), policy).map_err(|err| {
        ec_argument_function(
            "NATIONAL-OF",
            format!("len={}", display_bytes.len()),
            err.to_string(),
        )
    })
}

/// Evaluates ISO 2023 `FUNCTION ULENGTH(input)` (number of Unicode scalar values).
///
/// # Arguments
///
/// * `input` - [`UnicodeSlice`] (`Utf8`, `Utf16Be`, or `Utf16Le`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `input` is not well-formed Unicode.
pub fn meta_ulength(input: UnicodeSlice<'_>) -> Result<usize> {
    let spans = scan_unicode_scalars(input).map_err(|pos| {
        ec_argument_function(
            "ULENGTH",
            format!("pos={pos}"),
            "ill-formed Unicode sequence",
        )
    })?;
    Ok(spans.len())
}

/// Evaluates ISO 2023 `FUNCTION UPOS(input, char_index)` (`1`-based unit offset of `char_index`-th scalar).
///
/// # Arguments
///
/// * `input` - [`UnicodeSlice`].
/// * `char_index` - `1`-based Unicode character index (`1..=ULENGTH(input)`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `input` is ill-formed or `char_index` lies outside `1..=ULENGTH`.
pub fn meta_upos(input: UnicodeSlice<'_>, char_index: usize) -> Result<usize> {
    let spans = scan_unicode_scalars(input).map_err(|pos| {
        ec_argument_function("UPOS", format!("pos={pos}"), "ill-formed Unicode sequence")
    })?;
    if char_index == 0 || char_index > spans.len() {
        return Err(ec_argument_function(
            "UPOS",
            char_index.to_string(),
            "character index must lie in 1..=ULENGTH",
        ));
    }
    Ok(spans
        .get(char_index - 1)
        .map_or(0, |span| span.unit_pos_1based))
}

/// Evaluates ISO 2023 `FUNCTION USUBSTR(input, start_char, char_count)`.
///
/// # Arguments
///
/// * `input` - [`UnicodeSlice`].
/// * `start_char` - `1`-based starting Unicode scalar index.
/// * `char_count` - Number of Unicode scalars to extract.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `input` is ill-formed or `(start_char, char_count)` exceeds `ULENGTH`.
pub fn meta_usubstr(
    input: UnicodeSlice<'_>,
    start_char: usize,
    char_count: usize,
) -> Result<Vec<u8>> {
    let spans = scan_unicode_scalars(input).map_err(|pos| {
        ec_argument_function(
            "USUBSTR",
            format!("pos={pos}"),
            "ill-formed Unicode sequence",
        )
    })?;
    if start_char == 0 {
        return Err(ec_argument_function(
            "USUBSTR",
            "0",
            "start character index must be >= 1",
        ));
    }
    if char_count == 0 {
        if start_char > spans.len() + 1 {
            return Err(ec_argument_function(
                "USUBSTR",
                format!("{start_char}, 0"),
                "start index exceeds ULENGTH + 1",
            ));
        }
        return Ok(Vec::new());
    }
    let end_char = start_char.saturating_add(char_count - 1);
    if end_char > spans.len() {
        return Err(ec_argument_function(
            "USUBSTR",
            format!("{start_char}, {char_count}"),
            "substring range exceeds ULENGTH",
        ));
    }
    let byte_start = spans.get(start_char - 1).map_or(0, |f| f.byte_offset);
    let byte_end = spans
        .get(end_char - 1)
        .map_or(0, |l| l.byte_offset + l.byte_len);
    let raw = match input {
        UnicodeSlice::Utf8(b) | UnicodeSlice::Utf16Be(b) | UnicodeSlice::Utf16Le(b) => b,
    };
    Ok(raw.get(byte_start..byte_end).unwrap_or(&[]).to_vec())
}

/// Evaluates ISO 2023 `FUNCTION USUPPLEMENTARY(input)` (`1`-based unit index of first supplementary scalar, or `0`).
///
/// # Arguments
///
/// * `input` - [`UnicodeSlice`].
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `input` is ill-formed Unicode.
pub fn meta_usupplementary(input: UnicodeSlice<'_>) -> Result<usize> {
    let spans = scan_unicode_scalars(input).map_err(|pos| {
        ec_argument_function(
            "USUPPLEMENTARY",
            format!("pos={pos}"),
            "ill-formed Unicode sequence",
        )
    })?;
    Ok(spans
        .iter()
        .find(|s| u32::from(s.scalar) > 0xFFFF)
        .map_or(0, |s| s.unit_pos_1based))
}

/// Evaluates ISO 2023 `FUNCTION UVALID(input)` (`0` when well-formed, else `1`-based offending unit index).
///
/// # Arguments
///
/// * `input` - [`UnicodeSlice`].
#[must_use]
pub fn meta_uvalid(input: UnicodeSlice<'_>) -> usize {
    match scan_unicode_scalars(input) {
        Ok(_) => 0,
        Err(bad_pos) => bad_pos,
    }
}

/// Evaluates ISO 2023 `FUNCTION UWIDTH(input, char_index)` (physical byte width of `char_index`-th scalar).
///
/// # Arguments
///
/// * `input` - [`UnicodeSlice`].
/// * `char_index` - `1`-based Unicode character index (`1..=ULENGTH`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `input` is ill-formed or `char_index` lies outside `1..=ULENGTH`.
pub fn meta_uwidth(input: UnicodeSlice<'_>, char_index: usize) -> Result<usize> {
    let spans = scan_unicode_scalars(input).map_err(|pos| {
        ec_argument_function(
            "UWIDTH",
            format!("pos={pos}"),
            "ill-formed Unicode sequence",
        )
    })?;
    if char_index == 0 || char_index > spans.len() {
        return Err(ec_argument_function(
            "UWIDTH",
            char_index.to_string(),
            "character index must lie in 1..=ULENGTH",
        ));
    }
    Ok(spans.get(char_index - 1).map_or(0, |span| span.byte_len))
}

/// Evaluates `GnuCOBOL` `FUNCTION CONTENT-OF(pointed_bytes, explicit_len)` safely without `unsafe`.
///
/// When `explicit_len` is `None`, extracts bytes up to the first NUL (`0x00`) terminator.
///
/// # Arguments
///
/// * `pointed_bytes` - Safe slice view resolved by the runtime (`None` represents a `NULL` pointer).
/// * `explicit_len` - Optional explicit byte count.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `pointed_bytes` is `None` or `explicit_len` exceeds buffer bounds.
pub fn meta_content_of(
    pointed_bytes: Option<&[u8]>,
    explicit_len: Option<usize>,
) -> Result<Vec<u8>> {
    let Some(buf) = pointed_bytes else {
        return Err(ec_argument_function(
            "CONTENT-OF",
            "NULL",
            "cannot dereference NULL pointer in CONTENT-OF",
        ));
    };
    if let Some(len) = explicit_len {
        let Some(sub) = buf.get(..len) else {
            return Err(ec_argument_function(
                "CONTENT-OF",
                len.to_string(),
                "requested length exceeds accessible buffer bounds",
            ));
        };
        return Ok(sub.to_vec());
    }
    let nul_pos = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Ok(buf.get(..nul_pos).unwrap_or(&[]).to_vec())
}

/// Evaluates `GnuCOBOL` / ISO `FUNCTION CURRENCY-SYMBOL(active_symbol)`.
///
/// # Arguments
///
/// * `active_symbol` - Configured `SPECIAL-NAMES. CURRENCY SIGN` string (empty defaults to `"$"`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `active_symbol` contains ASCII control characters.
pub fn meta_currency_symbol(active_symbol: &str) -> Result<String> {
    if active_symbol.is_empty() {
        return Ok(String::from("$"));
    }
    if active_symbol.bytes().any(|b| b < 0x20 || b == 0x7F) {
        return Err(ec_argument_function(
            "CURRENCY-SYMBOL",
            active_symbol,
            "currency symbol must not contain control characters",
        ));
    }
    Ok(String::from(active_symbol))
}

/// Evaluates `FUNCTION MODULE-CALLER-ID(info)`.
///
/// # Arguments
///
/// * `info` - Active [`ModuleReflectionInfo`].
#[must_use]
pub fn meta_module_caller_id(info: &ModuleReflectionInfo) -> String {
    info.caller_id.clone()
}

/// Evaluates `FUNCTION MODULE-DATE(info)` (8-byte `"YYYYMMDD"`).
///
/// # Arguments
///
/// * `info` - Active [`ModuleReflectionInfo`].
#[must_use]
pub const fn meta_module_date(info: &ModuleReflectionInfo) -> [u8; 8] {
    info.compile_date_yyyymmdd
}

/// Evaluates `FUNCTION MODULE-FORMATTED-DATE(info)`.
///
/// # Arguments
///
/// * `info` - Active [`ModuleReflectionInfo`].
#[must_use]
pub fn meta_module_formatted_date(info: &ModuleReflectionInfo) -> String {
    info.formatted_compile_date.clone()
}

/// Evaluates `FUNCTION MODULE-ID(info)`.
///
/// # Arguments
///
/// * `info` - Active [`ModuleReflectionInfo`].
#[must_use]
pub fn meta_module_id(info: &ModuleReflectionInfo) -> String {
    info.module_id.clone()
}

/// Evaluates `GnuCOBOL` `FUNCTION MODULE-NAME(info)` (alias of `MODULE-ID`).
///
/// # Arguments
///
/// * `info` - Active [`ModuleReflectionInfo`].
#[must_use]
pub fn meta_module_name(info: &ModuleReflectionInfo) -> String {
    meta_module_id(info)
}

/// Evaluates `FUNCTION MODULE-PATH(info)`.
///
/// # Arguments
///
/// * `info` - Active [`ModuleReflectionInfo`].
#[must_use]
pub fn meta_module_path(info: &ModuleReflectionInfo) -> String {
    info.module_path.clone()
}

/// Evaluates `FUNCTION MODULE-SOURCE(info)`.
///
/// # Arguments
///
/// * `info` - Active [`ModuleReflectionInfo`].
#[must_use]
pub fn meta_module_source(info: &ModuleReflectionInfo) -> String {
    info.module_source.clone()
}

/// Evaluates `FUNCTION MODULE-TIME(info)` (6-byte `"HHMMSS"`).
///
/// # Arguments
///
/// * `info` - Active [`ModuleReflectionInfo`].
#[must_use]
pub const fn meta_module_time(info: &ModuleReflectionInfo) -> [u8; 6] {
    info.compile_time_hhmmss
}

/// Evaluates `FUNCTION EXCEPTION-FILE(state)` (`"00"` when none, else `<2-char status><file-name>`).
///
/// # Arguments
///
/// * `state` - Active [`ExceptionReflectionState`].
#[must_use]
pub fn meta_exception_file(state: &ExceptionReflectionState) -> String {
    if state.io_status == *b"00" {
        return String::from("00");
    }
    let [s0, s1] = state.io_status;
    let mut out = String::with_capacity(2 + state.file_name.len());
    out.push(char::from(s0));
    out.push(char::from(s1));
    out.push_str(&state.file_name);
    out
}

/// Evaluates `FUNCTION EXCEPTION-FILE-N(state, endian)` (`UTF-16` encoding of `EXCEPTION-FILE`).
///
/// # Arguments
///
/// * `state` - Active [`ExceptionReflectionState`].
/// * `endian` - Target [`NationalByteOrder`].
#[must_use]
pub fn meta_exception_file_n(
    state: &ExceptionReflectionState,
    endian: NationalByteOrder,
) -> Vec<u8> {
    encode_str_to_utf16(&meta_exception_file(state), endian)
}

/// Evaluates `FUNCTION EXCEPTION-LOCATION(state)` (`" "` when none, else location string).
///
/// # Arguments
///
/// * `state` - Active [`ExceptionReflectionState`].
#[must_use]
pub fn meta_exception_location(state: &ExceptionReflectionState) -> String {
    match &state.location {
        Some(loc) if !loc.is_empty() => loc.clone(),
        _ => String::from(" "),
    }
}

/// Evaluates `FUNCTION EXCEPTION-LOCATION-N(state, endian)` (`UTF-16` encoding of `EXCEPTION-LOCATION`).
///
/// # Arguments
///
/// * `state` - Active [`ExceptionReflectionState`].
/// * `endian` - Target [`NationalByteOrder`].
#[must_use]
pub fn meta_exception_location_n(
    state: &ExceptionReflectionState,
    endian: NationalByteOrder,
) -> Vec<u8> {
    encode_str_to_utf16(&meta_exception_location(state), endian)
}

/// Evaluates `FUNCTION EXCEPTION-STATEMENT(state)` (31-byte uppercase space-padded verb name).
///
/// # Arguments
///
/// * `state` - Active [`ExceptionReflectionState`].
#[must_use]
pub fn meta_exception_statement(
    state: &ExceptionReflectionState,
) -> [u8; EXCEPTION_REGISTER_WIDTH] {
    format_31_byte_register(state.statement_verb.as_deref())
}

/// Evaluates `FUNCTION EXCEPTION-STATUS(state)` (31-byte uppercase space-padded `EC-*` condition name).
///
/// # Arguments
///
/// * `state` - Active [`ExceptionReflectionState`].
#[must_use]
pub fn meta_exception_status(state: &ExceptionReflectionState) -> [u8; EXCEPTION_REGISTER_WIDTH] {
    format_31_byte_register(state.status_name.as_deref())
}

/// Evaluates `FUNCTION UUID4(generator)` (36-byte RFC 9562 v4 lowercase string `"8-4-4-4-12"`).
///
/// # Arguments
///
/// * `generator` - Mutable [`Uuid4Generator`] state.
#[must_use]
pub fn meta_uuid4(generator: &mut Uuid4Generator) -> [u8; 36] {
    meta_uuid4_from_entropy(generator.next_entropy_16())
}

/// Formats 16 raw entropy bytes into a compliant 36-byte RFC 9562 Version 4 UUID (`"xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx"`).
///
/// # Arguments
///
/// * `entropy` - Raw 128-bit entropy buffer.
#[must_use]
pub const fn meta_uuid4_from_entropy(entropy: [u8; 16]) -> [u8; 36] {
    let [
        b0,
        b1,
        b2,
        b3,
        b4,
        b5,
        raw_b6,
        b7,
        raw_b8,
        b9,
        b10,
        b11,
        b12,
        b13,
        b14,
        b15,
    ] = entropy;
    let b6 = (raw_b6 & 0x0F) | 0x40;
    let b8 = (raw_b8 & 0x3F) | 0x80;
    let (h0, l0) = byte_to_lower_hex(b0);
    let (h1, l1) = byte_to_lower_hex(b1);
    let (h2, l2) = byte_to_lower_hex(b2);
    let (h3, l3) = byte_to_lower_hex(b3);
    let (h4, l4) = byte_to_lower_hex(b4);
    let (h5, l5) = byte_to_lower_hex(b5);
    let (h6, l6) = byte_to_lower_hex(b6);
    let (h7, l7) = byte_to_lower_hex(b7);
    let (h8, l8) = byte_to_lower_hex(b8);
    let (h9, l9) = byte_to_lower_hex(b9);
    let (h10, l10) = byte_to_lower_hex(b10);
    let (h11, l11) = byte_to_lower_hex(b11);
    let (h12, l12) = byte_to_lower_hex(b12);
    let (h13, l13) = byte_to_lower_hex(b13);
    let (h14, l14) = byte_to_lower_hex(b14);
    let (h15, l15) = byte_to_lower_hex(b15);
    [
        h0, l0, h1, l1, h2, l2, h3, l3, b'-', h4, l4, h5, l5, b'-', h6, l6, h7, l7, b'-', h8, l8,
        h9, l9, b'-', h10, l10, h11, l11, h12, l12, h13, l13, h14, l14, h15, l15,
    ]
}

/// Scans `input` into [`ScalarSpan`] items, or returns the `1`-based offending unit index on ill-formed Unicode.
///
/// # Arguments
///
/// * `input` - Candidate [`UnicodeSlice`].
fn scan_unicode_scalars(input: UnicodeSlice<'_>) -> core::result::Result<Vec<ScalarSpan>, usize> {
    match input {
        UnicodeSlice::Utf8(bytes) => match core::str::from_utf8(bytes) {
            Ok(valid) => Ok(valid
                .char_indices()
                .map(|(off, ch)| ScalarSpan {
                    scalar: ch,
                    unit_pos_1based: off + 1,
                    byte_offset: off,
                    byte_len: ch.len_utf8(),
                })
                .collect()),
            Err(err) => Err(err.valid_up_to() + 1),
        },
        UnicodeSlice::Utf16Be(bytes) => scan_utf16_scalars(bytes, NationalByteOrder::BigEndian),
        UnicodeSlice::Utf16Le(bytes) => scan_utf16_scalars(bytes, NationalByteOrder::LittleEndian),
    }
}

/// Scans `UTF-16BE` or `UTF-16LE` bytes into [`ScalarSpan`] items, returning `Err(1_based_unit_pos)` if ill-formed.
///
/// # Arguments
///
/// * `bytes` - Raw `UTF-16` byte slice.
/// * `endian` - [`NationalByteOrder`] byte order.
fn scan_utf16_scalars(
    bytes: &[u8],
    endian: NationalByteOrder,
) -> core::result::Result<Vec<ScalarSpan>, usize> {
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut pos = 0_usize;
    let mut unit_idx = 1_usize;
    while pos < bytes.len() {
        let Some(u0) = read_utf16_unit(bytes, pos, endian) else {
            return Err(unit_idx);
        };
        if (0xDC00..=0xDFFF).contains(&u0) {
            return Err(unit_idx);
        }
        if (0xD800..=0xDBFF).contains(&u0) {
            let Some(u1) = read_utf16_unit(bytes, pos + 2, endian) else {
                return Err(unit_idx);
            };
            if !(0xDC00..=0xDFFF).contains(&u1) {
                return Err(unit_idx);
            }
            let hi = u32::from(u0 - 0xD800) << 10;
            let lo = u32::from(u1 - 0xDC00);
            let scalar = char::from_u32(0x1_0000 + hi + lo).unwrap_or('\u{FFFD}');
            out.push(ScalarSpan {
                scalar,
                unit_pos_1based: unit_idx,
                byte_offset: pos,
                byte_len: 4,
            });
            pos += 4;
            unit_idx += 2;
        } else {
            let scalar = char::from_u32(u32::from(u0)).unwrap_or('\u{FFFD}');
            out.push(ScalarSpan {
                scalar,
                unit_pos_1based: unit_idx,
                byte_offset: pos,
                byte_len: 2,
            });
            pos += 2;
            unit_idx += 1;
        }
    }
    Ok(out)
}

/// Reads a single 16-bit code unit at `byte_offset` according to `endian`.
///
/// # Arguments
///
/// * `bytes` - Raw `UTF-16` byte slice.
/// * `byte_offset` - 0-based byte offset.
/// * `endian` - [`NationalByteOrder`].
#[must_use]
fn read_utf16_unit(bytes: &[u8], byte_offset: usize, endian: NationalByteOrder) -> Option<u16> {
    let b0 = *bytes.get(byte_offset)?;
    let b1 = *bytes.get(byte_offset + 1)?;
    Some(match endian {
        NationalByteOrder::BigEndian => u16::from_be_bytes([b0, b1]),
        NationalByteOrder::LittleEndian => u16::from_le_bytes([b0, b1]),
    })
}

/// Encodes `text` into `UTF-16BE` or `UTF-16LE` bytes.
///
/// # Arguments
///
/// * `text` - Valid `UTF-8` Rust `&str`.
/// * `endian` - Target [`NationalByteOrder`].
#[must_use]
fn encode_str_to_utf16(text: &str, endian: NationalByteOrder) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        let pair = match endian {
            NationalByteOrder::BigEndian => unit.to_be_bytes(),
            NationalByteOrder::LittleEndian => unit.to_le_bytes(),
        };
        out.extend_from_slice(&pair);
    }
    out
}

/// Formats an optional name into a 31-byte uppercase space-padded ASCII array.
///
/// # Arguments
///
/// * `name` - Optional condition or statement identifier.
#[must_use]
fn format_31_byte_register(name: Option<&str>) -> [u8; EXCEPTION_REGISTER_WIDTH] {
    let mut out = [b' '; EXCEPTION_REGISTER_WIDTH];
    if let Some(s) = name {
        for (slot, src) in out.iter_mut().zip(s.trim().bytes()) {
            *slot = src.to_ascii_uppercase();
        }
    }
    out
}

/// Encodes a byte into two lowercase ASCII hexadecimal characters.
///
/// # Arguments
///
/// * `byte` - Unsigned input byte.
#[must_use]
const fn byte_to_lower_hex(byte: u8) -> (u8, u8) {
    (
        nibble_to_lower_hex(byte >> 4),
        nibble_to_lower_hex(byte & 0x0F),
    )
}

/// Encodes a 4-bit nibble (`0..=15`) into lowercase ASCII hex (`'0'..'9'`, `'a'..'f'`).
///
/// # Arguments
///
/// * `nib` - Low 4-bit value.
#[must_use]
const fn nibble_to_lower_hex(nib: u8) -> u8 {
    if nib < 10 {
        b'0' + nib
    } else {
        b'a' + (nib - 10)
    }
}
