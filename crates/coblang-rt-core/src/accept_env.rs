//! Pure-Rust COBOL `ACCEPT` (`CONSOLE`, `DATE`, `DAY`, `TIME`, CLI args, environment) and `DISPLAY UPON ENVIRONMENT-*` engine (`Milestone M10`).

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use coblang_common::Result;
use coblang_decimal::{
    Codepage, FixedDecimal, OverpunchDialect, UnmappedCharPolicy, transcode_bytes,
};
use coblang_hir::{DataCategory, HirFigurative};
use coblang_pic::{MoveFieldDesc, MoveSource, execute_move};

use crate::desc_codec::decode_field_spec;

/// Outcome of an `ACCEPT` operation (`ON EXCEPTION` vs `NOT ON EXCEPTION`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcceptFlowOutcome {
    /// Source value was obtained and stored without exception (`0`).
    Succeeded,
    /// EOF on console, out-of-range CLI argument index, or unset environment variable (`1`).
    Exception,
}

impl AcceptFlowOutcome {
    /// Encodes `self` as `0` (`Succeeded`) or `1` (`Exception`) across the C-ABI boundary.
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::Succeeded => 0,
            Self::Exception => 1,
        }
    }
}

/// Strongly-typed `ACCEPT ... FROM ...` source selector across compiler/runtime boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeAcceptSource {
    /// `ACCEPT dst [FROM CONSOLE | FROM SYSIN]` (`0`).
    ConsoleLine,
    /// `ACCEPT dst FROM DATE` (`YYMMDD`, `1`).
    DateYymmdd,
    /// `ACCEPT dst FROM DATE YYYYMMDD` (`YYYYMMDD`, `2`).
    DateYyyymmdd,
    /// `ACCEPT dst FROM DAY` (`YYDDD`, `3`).
    DayYyddd,
    /// `ACCEPT dst FROM DAY YYYYDDD` (`YYYYDDD`, `4`).
    DayYyyyddd,
    /// `ACCEPT dst FROM DAY-OF-WEEK` (`1` = Monday .. `7` = Sunday, `5`).
    DayOfWeek,
    /// `ACCEPT dst FROM TIME` (`HHMMSShh`, `6`).
    TimeStandard8,
    /// `ACCEPT dst FROM TIME` under ACU dialect when target width > 8 (`HHMMSSuuuuuu`, `7`).
    TimeAcu12,
    /// `ACCEPT dst FROM MICROSECOND-TIME` (`HHMMSSuuuuuu`, `8`).
    MicrosecondTime12,
    /// `ACCEPT dst FROM ARGUMENT-NUMBER` (`9`).
    ArgumentNumber,
    /// `ACCEPT dst FROM ARGUMENT-VALUE` (`10`).
    ArgumentValue,
    /// `ACCEPT dst FROM COMMAND-LINE` (`11`).
    CommandLine,
    /// `ACCEPT dst FROM ENVIRONMENT-NAME` (`12`).
    EnvironmentName,
    /// `ACCEPT dst FROM ENVIRONMENT-VALUE` (`13`).
    EnvironmentValue,
}

impl RuntimeAcceptSource {
    /// Decodes a C-ABI integer selector into [`RuntimeAcceptSource`].
    ///
    /// # Arguments
    ///
    /// * `code` - Raw integer code emitted by MLIR lowering.
    #[must_use]
    pub const fn from_abi_code(code: i32) -> Self {
        match code {
            1 => Self::DateYymmdd,
            2 => Self::DateYyyymmdd,
            3 => Self::DayYyddd,
            4 => Self::DayYyyyddd,
            5 => Self::DayOfWeek,
            6 => Self::TimeStandard8,
            7 => Self::TimeAcu12,
            8 => Self::MicrosecondTime12,
            9 => Self::ArgumentNumber,
            10 => Self::ArgumentValue,
            11 => Self::CommandLine,
            12 => Self::EnvironmentName,
            13 => Self::EnvironmentValue,
            _ => Self::ConsoleLine,
        }
    }

    /// Encodes `self` into its C-ABI integer code.
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::ConsoleLine => 0,
            Self::DateYymmdd => 1,
            Self::DateYyyymmdd => 2,
            Self::DayYyddd => 3,
            Self::DayYyyyddd => 4,
            Self::DayOfWeek => 5,
            Self::TimeStandard8 => 6,
            Self::TimeAcu12 => 7,
            Self::MicrosecondTime12 => 8,
            Self::ArgumentNumber => 9,
            Self::ArgumentValue => 10,
            Self::CommandLine => 11,
            Self::EnvironmentName => 12,
            Self::EnvironmentValue => 13,
        }
    }
}

/// Target of a special `DISPLAY ... UPON ...` environment/argument register update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvDisplayTarget {
    /// `DISPLAY name UPON ENVIRONMENT-NAME` (`0`).
    EnvironmentName,
    /// `DISPLAY value UPON ENVIRONMENT-VALUE` (`1`).
    EnvironmentValue,
    /// `DISPLAY idx UPON ARGUMENT-NUMBER` (`2`).
    ArgumentNumber,
    /// `DISPLAY cmd UPON COMMAND-LINE` (`3`).
    CommandLine,
}

impl EnvDisplayTarget {
    /// Decodes a C-ABI integer code into [`EnvDisplayTarget`].
    ///
    /// # Arguments
    ///
    /// * `code` - Raw integer code emitted by MLIR lowering.
    #[must_use]
    pub const fn from_abi_code(code: i32) -> Self {
        match code {
            1 => Self::EnvironmentValue,
            2 => Self::ArgumentNumber,
            3 => Self::CommandLine,
            _ => Self::EnvironmentName,
        }
    }

    /// Encodes `self` into its C-ABI integer code.
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::EnvironmentName => 0,
            Self::EnvironmentValue => 1,
            Self::ArgumentNumber => 2,
            Self::CommandLine => 3,
        }
    }
}

/// Validated civil calendar date and wall-clock time to microsecond precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CobolDateTime {
    /// Four-digit Gregorian year (`1..=9999`).
    pub year: u16,
    /// Month of year (`1..=12`).
    pub month: u8,
    /// Day of month (`1..=31`).
    pub day: u8,
    /// Hour of day (`0..=23`).
    pub hour: u8,
    /// Minute of hour (`0..=59`).
    pub minute: u8,
    /// Second of minute (`0..=60`).
    pub second: u8,
    /// Microsecond within second (`0..=999_999`).
    pub microsecond: u32,
}

impl CobolDateTime {
    /// Returns `true` when `year` is a Gregorian leap year.
    ///
    /// # Arguments
    ///
    /// * `year` - Four-digit Gregorian year.
    #[must_use]
    pub const fn is_leap_year(year: u16) -> bool {
        (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
    }

    /// Returns the number of days in `(year, month)` (`0` if `month` is outside `1..=12`).
    ///
    /// # Arguments
    ///
    /// * `year` - Four-digit Gregorian year.
    /// * `month` - 1-based month (`1..=12`).
    #[must_use]
    pub const fn days_in_month(year: u16, month: u8) -> u8 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if Self::is_leap_year(year) {
                    29
                } else {
                    28
                }
            }
            _ => 0,
        }
    }

    /// Computes the 1-based ordinal day of year (`1..=366`).
    #[must_use]
    pub fn day_of_year(self) -> u16 {
        let mut doy = u16::from(self.day);
        for m in 1..self.month {
            doy += u16::from(Self::days_in_month(self.year, m));
        }
        doy
    }

    /// Computes ISO 8601 day of week (`1` = Monday .. `7` = Sunday).
    #[must_use]
    pub fn iso_day_of_week(self) -> u8 {
        let y = i64::from(self.year) - i64::from(self.month <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(self.month);
        let mp = m + if m > 2 { -3 } else { 9 };
        let doy = (153 * mp + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days_since_epoch = era * 146_097 + doe - 719_468;
        // 1970-01-01 was Thursday (ISO day 4 -> 0-based Monday index 3).
        let zero_mon = (days_since_epoch + 3).rem_euclid(7);
        u8::try_from(zero_mon).unwrap_or(0) + 1
    }

    /// Converts a Unix timestamp `(secs, micros)` into UTC [`CobolDateTime`].
    ///
    /// # Arguments
    ///
    /// * `unix_secs` - Seconds since `1970-01-01T00:00:00Z`.
    /// * `microsecond` - Sub-second microseconds (`0..=999_999`).
    #[must_use]
    pub fn from_unix_timestamp(unix_secs: i64, microsecond: u32) -> Self {
        let days = unix_secs.div_euclid(86_400);
        let rem_secs = unix_secs.rem_euclid(86_400);
        let hour = u8::try_from(rem_secs / 3600).unwrap_or(0);
        let minute = u8::try_from((rem_secs % 3600) / 60).unwrap_or(0);
        let second = u8::try_from(rem_secs % 60).unwrap_or(0);

        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let mut y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        if m <= 2 {
            y += 1;
        }
        Self {
            year: u16::try_from(y.clamp(1, 9999)).unwrap_or(1970),
            month: u8::try_from(m).unwrap_or(1),
            day: u8::try_from(d).unwrap_or(1),
            hour,
            minute,
            second,
            microsecond: microsecond.min(999_999),
        }
    }

    /// Queries the current host clock as [`CobolDateTime`].
    #[must_use]
    pub fn now_system() -> Self {
        let dur = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self::from_unix_timestamp(
            i64::try_from(dur.as_secs()).unwrap_or(0),
            dur.subsec_micros(),
        )
    }

    /// Formats `self` into the ASCII digit string requested by `src`.
    ///
    /// # Arguments
    ///
    /// * `src` - Date/day/time [`RuntimeAcceptSource`] variant.
    #[must_use]
    pub fn format_for_accept(self, src: RuntimeAcceptSource) -> String {
        match src {
            RuntimeAcceptSource::DateYymmdd => {
                format!("{:02}{:02}{:02}", self.year % 100, self.month, self.day)
            }
            RuntimeAcceptSource::DateYyyymmdd => {
                format!("{:04}{:02}{:02}", self.year, self.month, self.day)
            }
            RuntimeAcceptSource::DayYyddd => {
                format!("{:02}{:03}", self.year % 100, self.day_of_year())
            }
            RuntimeAcceptSource::DayYyyyddd => {
                format!("{:04}{:03}", self.year, self.day_of_year())
            }
            RuntimeAcceptSource::DayOfWeek => format!("{}", self.iso_day_of_week()),
            RuntimeAcceptSource::TimeStandard8 => format!(
                "{:02}{:02}{:02}{:02}",
                self.hour,
                self.minute,
                self.second,
                self.microsecond / 10_000
            ),
            RuntimeAcceptSource::TimeAcu12
            | RuntimeAcceptSource::MicrosecondTime12
            | RuntimeAcceptSource::ConsoleLine
            | RuntimeAcceptSource::ArgumentNumber
            | RuntimeAcceptSource::ArgumentValue
            | RuntimeAcceptSource::CommandLine
            | RuntimeAcceptSource::EnvironmentName
            | RuntimeAcceptSource::EnvironmentValue => format!(
                "{:02}{:02}{:02}{:06}",
                self.hour, self.minute, self.second, self.microsecond
            ),
        }
    }
}

/// Parses a `COB_CURRENT_DATE` / `GCOBOL_CURRENT_DATE` override string relative to `fallback`.
///
/// Supports compact (`"20150405184522"`, `"YYYYMMDDHHMMSS+0100"`) and delimited
/// (`"2015/04/05 18:45:22.123400000"`, `"2015-04-05T18:45:22Z"`) forms.
///
/// # Arguments
///
/// * `raw` - Raw environment variable string.
/// * `fallback` - Current clock [`CobolDateTime`] used to fill omitted time fields or `Y`/`M`/`D`/`H`/`S` placeholders.
#[must_use]
pub fn parse_current_date_override(raw: &str, fallback: CobolDateTime) -> Option<CobolDateTime> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Split optional trailing timezone (`Z`, `+HHMM`, `-HHMM`, `+HH:MM`, `-HH:MM`) after at least 8 date chars.
    let main_body = strip_valid_tz_suffix(trimmed)?;
    let fb_compact = format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}{:06}",
        fallback.year,
        fallback.month,
        fallback.day,
        fallback.hour,
        fallback.minute,
        fallback.second,
        fallback.microsecond
    );

    // Collect normalized digits (substituting `Y`/`M`/`D`/`H`/`N`/`S` placeholders from `fb_compact` while skipping valid separators).
    let mut digits = Vec::with_capacity(20);
    for ch in main_body.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch as u8);
        } else if matches!(ch.to_ascii_uppercase(), 'Y' | 'M' | 'D' | 'H' | 'N' | 'S') {
            let fb_b = fb_compact.as_bytes().get(digits.len()).copied()?;
            digits.push(fb_b);
        } else if matches!(ch, '/' | '-' | ':' | ' ' | 'T' | 't' | '.' | ',') {
            // Allowed ISO / GnuCOBOL date-time separator.
        } else {
            return None;
        }
    }

    if digits.len() < 8 {
        return None;
    }
    let year = parse_u16_digits(digits.get(0..4).unwrap_or(&[]));
    let month = u8::try_from(parse_u16_digits(digits.get(4..6).unwrap_or(&[]))).unwrap_or(0);
    let day = u8::try_from(parse_u16_digits(digits.get(6..8).unwrap_or(&[]))).unwrap_or(0);
    let max_day = CobolDateTime::days_in_month(year, month);
    if year == 0 || max_day == 0 || day == 0 || day > max_day {
        return None;
    }

    if digits.len() == 8 {
        return Some(CobolDateTime {
            year,
            month,
            day,
            hour: 0,
            minute: 0,
            second: 0,
            microsecond: 0,
        });
    }
    if digits.len() < 14 {
        return None;
    }
    let hour = u8::try_from(parse_u16_digits(digits.get(8..10).unwrap_or(&[]))).unwrap_or(99);
    let minute = u8::try_from(parse_u16_digits(digits.get(10..12).unwrap_or(&[]))).unwrap_or(99);
    let second = u8::try_from(parse_u16_digits(digits.get(12..14).unwrap_or(&[]))).unwrap_or(99);
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let frac = digits.get(14..).unwrap_or(&[]);
    if frac.len() > 9 {
        return None;
    }
    let mut micro: u32 = 0;
    for i in 0..6 {
        let d = frac.get(i).map_or(0, |b| b - b'0');
        micro = micro * 10 + u32::from(d);
    }
    Some(CobolDateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
        microsecond: micro,
    })
}

/// Validates and strips an optional trailing UTC/offset suffix (`Z`, `+HH`, `+HHMM`, `+HH:MM`) after the date portion.
///
/// # Arguments
///
/// * `s` - Trimmed timestamp string (`!s.is_empty()`).
fn strip_valid_tz_suffix(s: &str) -> Option<&str> {
    if let Some(prefix) = s.strip_suffix(['Z', 'z']) {
        return Some(prefix);
    }
    let tail = s.get(8..).unwrap_or("");
    let Some(rel_sign) = tail.rfind(['+', '-']) else {
        return Some(s);
    };
    let sign_idx = 8 + rel_sign;
    let tz_digits_raw = s.get(sign_idx + 1..).unwrap_or("");
    let tz_clean: Vec<u8> = match tz_digits_raw.len() {
        2 | 4 if tz_digits_raw.bytes().all(|b| b.is_ascii_digit()) => {
            tz_digits_raw.bytes().collect()
        }
        5 if tz_digits_raw.as_bytes().get(2) == Some(&b':') => {
            let mut v = Vec::with_capacity(4);
            for (i, b) in tz_digits_raw.bytes().enumerate() {
                if i != 2 {
                    if !b.is_ascii_digit() {
                        return None;
                    }
                    v.push(b);
                }
            }
            v
        }
        _ => return None,
    };
    let tz_h = parse_u16_digits(tz_clean.get(0..2).unwrap_or(&[]));
    let tz_m = if tz_clean.len() == 4 {
        parse_u16_digits(tz_clean.get(2..4).unwrap_or(&[]))
    } else {
        0
    };
    if tz_h > 23 || tz_m > 59 {
        return None;
    }
    s.get(..sign_idx)
}

/// Parses an ASCII digit slice of length `1..=4` into `u16`.
///
/// # Arguments
///
/// * `slice` - Slice of ASCII digit bytes (`b'0'..=b'9'`).
fn parse_u16_digits(slice: &[u8]) -> u16 {
    let mut acc: u16 = 0;
    for &b in slice {
        acc = acc * 10 + u16::from(b - b'0');
    }
    acc
}

/// Per-run-unit mutable state for `ACCEPT` / `DISPLAY UPON ENVIRONMENT-*` / CLI argument cursor.
#[derive(Debug, Default)]
pub struct AcceptEnvState {
    /// Active environment variable name selected by `DISPLAY ... UPON ENVIRONMENT-NAME`.
    pending_env_name: String,
    /// Safe in-process environment variable overlay updated by `DISPLAY ... UPON ENVIRONMENT-VALUE`.
    env_overrides: BTreeMap<String, String>,
    /// Next CLI argument index read by sequential `ACCEPT ... FROM ARGUMENT-VALUE` (`1` initially).
    arg_cursor: usize,
    /// Whether an invalid `COB_CURRENT_DATE` warning has already been emitted for the current value.
    warned_invalid_date: Option<String>,
}

impl AcceptEnvState {
    /// Creates an empty [`AcceptEnvState`] with `arg_cursor = 1`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending_env_name: String::new(),
            env_overrides: BTreeMap::new(),
            arg_cursor: 1,
            warned_invalid_date: None,
        }
    }

    /// Resets transient cursor and pending register state on `cob_init` / `cob_tidy`.
    pub fn reset(&mut self) {
        self.pending_env_name.clear();
        self.env_overrides.clear();
        self.arg_cursor = 1;
        self.warned_invalid_date = None;
    }

    /// Looks up `name` in the safe in-process overlay first, falling back to `env_lookup`.
    ///
    /// # Arguments
    ///
    /// * `name` - Environment variable name.
    /// * `env_lookup` - Host environment lookup callback.
    pub fn lookup_env_with(
        &self,
        name: &str,
        env_lookup: &mut dyn FnMut(&str) -> Option<String>,
    ) -> Option<String> {
        if let Some(v) = self.env_overrides.get(name) {
            return Some(v.clone());
        }
        env_lookup(name)
    }

    /// Executes `DISPLAY text UPON <target>` (`ENVIRONMENT-NAME`, `ENVIRONMENT-VALUE`, `ARGUMENT-NUMBER`, `COMMAND-LINE`).
    ///
    /// # Arguments
    ///
    /// * `target` - [`EnvDisplayTarget`] register selector.
    /// * `raw_bytes` - Formatted operand bytes.
    /// * `args` - Mutable run-unit CLI argument vector (for `COMMAND-LINE` override).
    pub fn apply_env_display(
        &mut self,
        target: EnvDisplayTarget,
        raw_bytes: &[u8],
        args: &mut Vec<String>,
    ) {
        let text = String::from_utf8_lossy(raw_bytes);
        let trimmed_end = text.trim_end_matches(' ');
        match target {
            EnvDisplayTarget::EnvironmentName => {
                self.pending_env_name = String::from(trimmed_end.trim_start_matches(' '));
            }
            EnvDisplayTarget::EnvironmentValue => {
                if !self.pending_env_name.is_empty() {
                    let _ = self
                        .env_overrides
                        .insert(self.pending_env_name.clone(), String::from(trimmed_end));
                }
            }
            EnvDisplayTarget::ArgumentNumber => {
                if let Ok(idx) = trimmed_end.trim().parse::<usize>() {
                    self.arg_cursor = idx;
                }
            }
            EnvDisplayTarget::CommandLine => {
                let prog0 = args.first().cloned().unwrap_or_default();
                args.clear();
                args.push(prog0);
                for part in trimmed_end.split_whitespace() {
                    args.push(String::from(part));
                }
                self.arg_cursor = 1;
            }
        }
    }

    /// Resolves the effective [`CobolDateTime`], honoring `COB_CURRENT_DATE` and `GCOBOL_CURRENT_DATE`.
    ///
    /// Emits `libcob: warning: COB_CURRENT_DATE '<val>' is invalid` at most once per distinct invalid value.
    ///
    /// # Arguments
    ///
    /// * `env_lookup` - Host environment lookup callback.
    /// * `stderr_out` - Warning stream writer.
    pub fn resolve_datetime(
        &mut self,
        env_lookup: &mut dyn FnMut(&str) -> Option<String>,
        stderr_out: &mut dyn Write,
    ) -> CobolDateTime {
        let now = CobolDateTime::now_system();
        let candidate = self
            .lookup_env_with("COB_CURRENT_DATE", env_lookup)
            .map(|v| ("COB_CURRENT_DATE", v))
            .or_else(|| {
                self.lookup_env_with("GCOBOL_CURRENT_DATE", env_lookup)
                    .map(|v| ("GCOBOL_CURRENT_DATE", v))
            });
        let Some((var_name, raw_val)) = candidate else {
            return now;
        };
        if let Some(parsed) = parse_current_date_override(&raw_val, now) {
            return parsed;
        }
        if var_name == "COB_CURRENT_DATE"
            && self.warned_invalid_date.as_deref() != Some(raw_val.as_str())
        {
            let _ = writeln!(
                stderr_out,
                "libcob: warning: COB_CURRENT_DATE '{raw_val}' is invalid"
            );
            self.warned_invalid_date = Some(raw_val);
        }
        now
    }
}

/// Destination receiver for [`execute_runtime_accept`].
pub enum AcceptTargetDest<'a> {
    /// `ACCEPT OMITTED` (reads/evaluates source and discards value).
    Omitted,
    /// Addressable COBOL receiving field `(storage_slice, serialized_field_spec)`.
    Field {
        /// Mutable target storage bytes.
        buf: &'a mut [u8],
        /// Encoded [`crate::desc_codec::RuntimeFieldSpec`] bytes.
        spec_bytes: &'a [u8],
    },
}

impl<'a> AcceptTargetDest<'a> {
    /// Constructs an [`AcceptTargetDest::Field`] target.
    ///
    /// # Arguments
    ///
    /// * `buf` - Mutable target storage bytes.
    /// * `spec_bytes` - Serialized [`crate::desc_codec::RuntimeFieldSpec`] bytes.
    #[must_use]
    pub const fn field(buf: &'a mut [u8], spec_bytes: &'a [u8]) -> Self {
        Self::Field { buf, spec_bytes }
    }
}

/// Executes a runtime `ACCEPT` operation from `source` into `dest`.
///
/// # Arguments
///
/// * `source` - [`RuntimeAcceptSource`] selector.
/// * `dest` - [`AcceptTargetDest`] (`Omitted` or `Field`).
/// * `state` - Mutable [`AcceptEnvState`].
/// * `args` - Run-unit CLI arguments (`args[0]` is program path).
/// * `stdin_in` - Buffered console input stream.
/// * `stderr_out` - Standard error stream for invalid `COB_CURRENT_DATE` warnings.
/// * `env_lookup` - Host environment lookup callback.
///
/// # Errors
///
/// Returns an error if `dest` carries malformed field descriptor metadata or numeric move conversion fails.
pub fn execute_runtime_accept(
    source: RuntimeAcceptSource,
    dest: AcceptTargetDest<'_>,
    state: &mut AcceptEnvState,
    args: &[String],
    stdin_in: &mut dyn BufRead,
    stderr_out: &mut dyn Write,
    env_lookup: &mut dyn FnMut(&str) -> Option<String>,
) -> Result<AcceptFlowOutcome> {
    match source {
        RuntimeAcceptSource::ConsoleLine => accept_console_line(dest, stdin_in),
        RuntimeAcceptSource::ArgumentNumber => {
            let user_argc = i64::try_from(args.len().saturating_sub(1)).unwrap_or(0);
            store_accept_decimal(dest, FixedDecimal::from_i64(user_argc))?;
            Ok(AcceptFlowOutcome::Succeeded)
        }
        RuntimeAcceptSource::ArgumentValue => {
            let Some(val) = args.get(state.arg_cursor) else {
                return Ok(AcceptFlowOutcome::Exception);
            };
            state.arg_cursor += 1;
            store_accept_text(dest, val.as_bytes())?;
            Ok(AcceptFlowOutcome::Succeeded)
        }
        RuntimeAcceptSource::CommandLine => {
            let joined = args.get(1..).unwrap_or(&[]).join(" ");
            store_accept_text(dest, joined.as_bytes())?;
            Ok(AcceptFlowOutcome::Succeeded)
        }
        RuntimeAcceptSource::EnvironmentName => {
            let name_copy = state.pending_env_name.clone();
            store_accept_text(dest, name_copy.as_bytes())?;
            Ok(AcceptFlowOutcome::Succeeded)
        }
        RuntimeAcceptSource::EnvironmentValue => {
            let key = state.pending_env_name.clone();
            let Some(val) = state.lookup_env_with(&key, env_lookup) else {
                return Ok(AcceptFlowOutcome::Exception);
            };
            store_accept_text(dest, val.as_bytes())?;
            Ok(AcceptFlowOutcome::Succeeded)
        }
        RuntimeAcceptSource::DateYymmdd
        | RuntimeAcceptSource::DateYyyymmdd
        | RuntimeAcceptSource::DayYyddd
        | RuntimeAcceptSource::DayYyyyddd
        | RuntimeAcceptSource::DayOfWeek
        | RuntimeAcceptSource::TimeStandard8
        | RuntimeAcceptSource::TimeAcu12
        | RuntimeAcceptSource::MicrosecondTime12 => {
            let dt = state.resolve_datetime(env_lookup, stderr_out);
            let effective_src = if source == RuntimeAcceptSource::TimeAcu12
                && match &dest {
                    AcceptTargetDest::Omitted => true,
                    AcceptTargetDest::Field { buf, .. } => buf.len() <= 8,
                } {
                RuntimeAcceptSource::TimeStandard8
            } else {
                source
            };
            let digits = dt.format_for_accept(effective_src);
            store_accept_digits_or_text(dest, &digits)?;
            Ok(AcceptFlowOutcome::Succeeded)
        }
    }
}

/// Reads a single line from `stdin_in`, stripping trailing CR/LF terminators, and moves it into `dest`.
///
/// # Arguments
///
/// * `dest` - [`AcceptTargetDest`].
/// * `stdin_in` - Buffered input stream.
fn accept_console_line(
    dest: AcceptTargetDest<'_>,
    stdin_in: &mut dyn BufRead,
) -> Result<AcceptFlowOutcome> {
    let mut line = String::new();
    let n = stdin_in.read_line(&mut line).unwrap_or(0);
    if n == 0 {
        store_accept_text(dest, b"")?;
        return Ok(AcceptFlowOutcome::Exception);
    }
    let clean = line.trim_end_matches([char::from(0x0D), char::from(0x0A)]);
    store_accept_text(dest, clean.as_bytes())?;
    Ok(AcceptFlowOutcome::Succeeded)
}

/// Stores an integer [`FixedDecimal`] into `dest` (`ARGUMENT-NUMBER`).
///
/// # Arguments
///
/// * `dest` - [`AcceptTargetDest`].
/// * `val` - Exact integer value.
fn store_accept_decimal(dest: AcceptTargetDest<'_>, val: FixedDecimal) -> Result<()> {
    match dest {
        AcceptTargetDest::Omitted => Ok(()),
        AcceptTargetDest::Field { buf, spec_bytes } => {
            let spec = decode_field_spec(spec_bytes)?;
            execute_move(MoveSource::Decimal(val), buf, &spec.desc)
        }
    }
}

/// Stores alphanumeric text `bytes` into `dest` (using `Zero` when `dest` is numeric and `bytes` is empty).
///
/// # Arguments
///
/// * `dest` - [`AcceptTargetDest`].
/// * `bytes` - Source text slice.
fn store_accept_text(dest: AcceptTargetDest<'_>, bytes: &[u8]) -> Result<()> {
    match dest {
        AcceptTargetDest::Omitted => Ok(()),
        AcceptTargetDest::Field { buf, spec_bytes } => {
            let spec = decode_field_spec(spec_bytes)?;
            move_bytes_to_desc(bytes, buf, &spec.desc)
        }
    }
}

/// Stores formatted `DATE`/`DAY`/`TIME` ASCII digits as an integer when `dest` is numeric/edited,
/// or as left-aligned alphanumeric characters when `dest` is alphanumeric/group.
///
/// # Arguments
///
/// * `dest` - [`AcceptTargetDest`].
/// * `digits` - Pure ASCII digit string (`1..=12` chars).
fn store_accept_digits_or_text(dest: AcceptTargetDest<'_>, digits: &str) -> Result<()> {
    match dest {
        AcceptTargetDest::Omitted => Ok(()),
        AcceptTargetDest::Field { buf, spec_bytes } => {
            let spec = decode_field_spec(spec_bytes)?;
            if matches!(
                spec.desc.category(),
                DataCategory::Numeric | DataCategory::NumericEdited
            ) {
                let n = digits.parse::<i64>().unwrap_or(0);
                execute_move(
                    MoveSource::Decimal(FixedDecimal::from_i64(n)),
                    buf,
                    &spec.desc,
                )
            } else {
                move_bytes_to_desc(digits.as_bytes(), buf, &spec.desc)
            }
        }
    }
}

/// Moves `bytes` (UTF-8/ASCII host text) into `(buf, desc)`, handling empty numeric input as `0`
/// and transcoding into `NATIONAL`, `UTF-8`, or `EBCDIC` storage when needed.
///
/// # Arguments
///
/// * `bytes` - Input text bytes.
/// * `buf` - Mutable target storage slice.
/// * `desc` - Target [`MoveFieldDesc`].
fn move_bytes_to_desc(bytes: &[u8], buf: &mut [u8], desc: &MoveFieldDesc) -> Result<()> {
    let trimmed = core::str::from_utf8(bytes).map_or(bytes, |s| s.trim().as_bytes());
    if matches!(
        desc.category(),
        DataCategory::Numeric | DataCategory::NumericEdited
    ) {
        let src = if trimmed.is_empty() {
            MoveSource::Figurative(HirFigurative::Zero)
        } else {
            MoveSource::AlphanumericLiteral(trimmed)
        };
        return execute_move(src, buf, desc);
    }
    if matches!(
        desc.category(),
        DataCategory::National | DataCategory::NationalEdited | DataCategory::Utf8
    ) {
        return execute_move(MoveSource::Utf8Literal(bytes), buf, desc);
    }
    if desc.overpunch == OverpunchDialect::EbcdicIbm {
        let ebc = transcode_bytes(
            bytes,
            Codepage::Iso8859_1,
            Codepage::Cp1140,
            UnmappedCharPolicy::Substitute('?'),
        )
        .unwrap_or_else(|_| bytes.to_vec());
        return execute_move(MoveSource::AlphanumericLiteral(&ebc), buf, desc);
    }
    execute_move(MoveSource::AlphanumericLiteral(bytes), buf, desc)
}
