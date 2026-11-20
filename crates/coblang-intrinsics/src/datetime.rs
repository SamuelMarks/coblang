//! Calendar, Lilian date (`1601-01-01 = 1`), and ISO 8601 formatted date/time intrinsics (`Milestone M12`).

use core::fmt::Write as _;

use coblang_common::Result;
use coblang_decimal::{
    DecimalSign, FixedDecimal, RoundingMode, SignedInt256, SizeErrorStatus, Uint256,
};

use crate::ec_argument_function;

/// Minimum valid ISO COBOL Lilian integer (`1601-01-01`).
pub const MIN_LILIAN_DAY: i64 = 1;

/// Maximum valid ISO COBOL Lilian integer (`9999-12-31`).
pub const MAX_LILIAN_DAY: i64 = 3_067_671;

/// Number of seconds in a standard 24-hour calendar day (`86,400`).
const SECONDS_PER_DAY: i64 = 86_400;

/// Maximum valid UTC offset magnitude in minutes (`23 * 60 + 59 = 1439`).
const MAX_UTC_OFFSET_MINUTES: i32 = 1_439;

/// Default sliding-century window offset for `YEAR-TO-YYYY`, `DATE-TO-YYYYMMDD`, and `DAY-TO-YYYYDDD`.
const DEFAULT_CENTURY_WINDOW: i64 = 50;

/// UTC offset state carried by [`ClockSnapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UtcOffsetSpec {
    /// Local offset unknown (`"00000"` in `CURRENT-DATE` positions `17..=21`).
    Unknown,
    /// Signed offset from UTC in minutes (`-1439..=+1439`).
    Minutes(i16),
}

/// Deterministic wall-clock snapshot used by `CURRENT-DATE`, `WHEN-COMPILED`,
/// `SECONDS-PAST-MIDNIGHT`, and `FORMATTED-CURRENT-DATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClockSnapshot {
    /// Gregorian year (`1601..=9999`).
    pub year: u16,
    /// Calendar month (`1..=12`).
    pub month: u8,
    /// Day of month (`1..=31`).
    pub day: u8,
    /// Hour of day (`0..=23`).
    pub hour: u8,
    /// Minute of hour (`0..=59`).
    pub minute: u8,
    /// Second of minute (`0..=60`).
    pub second: u8,
    /// Subsecond nanoseconds (`0..=999_999_999`).
    pub nanosecond: u32,
    /// UTC offset metadata.
    pub offset: UtcOffsetSpec,
}

impl ClockSnapshot {
    /// Constructs a UTC [`ClockSnapshot`] from UNIX epoch seconds (`1970-01-01T00:00:00Z = 0`) and nanoseconds.
    ///
    /// # Arguments
    ///
    /// * `unix_seconds` - Signed seconds since `1970-01-01T00:00:00Z`.
    /// * `subsec_nanos` - Subsecond nanoseconds clamped to `0..=999_999_999`.
    #[must_use]
    pub fn from_unix_utc(unix_seconds: i64, subsec_nanos: u32) -> Self {
        // 1970-01-01 is Lilian day 134,775 (`1601-01-01` = 1).
        let days_from_epoch = unix_seconds.div_euclid(SECONDS_PER_DAY);
        let secs_of_day = unix_seconds.rem_euclid(SECONDS_PER_DAY);
        let lilian = (days_from_epoch + 134_775).clamp(MIN_LILIAN_DAY, MAX_LILIAN_DAY);
        let (year, month, day) = ymd_from_lilian_u32(u32::try_from(lilian).unwrap_or(1));
        let hour = u8::try_from(secs_of_day / 3_600).unwrap_or(0);
        let minute = u8::try_from((secs_of_day % 3_600) / 60).unwrap_or(0);
        let second = u8::try_from(secs_of_day % 60).unwrap_or(0);
        Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
            nanosecond: subsec_nanos.min(999_999_999),
            offset: UtcOffsetSpec::Minutes(0),
        }
    }

    /// Captures the current system UTC clock as a [`ClockSnapshot`].
    #[must_use]
    pub fn now_system_utc() -> Self {
        let now = std::time::SystemTime::now();
        let dur = now
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO);
        let secs = i64::try_from(dur.as_secs()).unwrap_or(0);
        Self::from_unix_utc(secs, dur.subsec_nanos())
    }
}

/// Separator style of an ISO 8601 date or time format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum IsoDelimiterStyle {
    /// Basic compact format without `-` or `:` separators.
    Basic,
    /// Extended format with `-` and `:` separators.
    Extended,
}

/// Supported ISO/IEC 1989:2023 date format patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum IsoDatePattern {
    /// `"YYYYMMDD"`
    BasicCalendar,
    /// `"YYYY-MM-DD"`
    ExtendedCalendar,
    /// `"YYYYDDD"`
    BasicOrdinal,
    /// `"YYYY-DDD"`
    ExtendedOrdinal,
    /// `"YYYYWwwD"`
    BasicWeek,
    /// `"YYYY-Www-D"`
    ExtendedWeek,
}

impl IsoDatePattern {
    /// Returns the [`IsoDelimiterStyle`] of this date pattern.
    #[must_use]
    const fn style(self) -> IsoDelimiterStyle {
        match self {
            Self::BasicCalendar | Self::BasicOrdinal | Self::BasicWeek => IsoDelimiterStyle::Basic,
            Self::ExtendedCalendar | Self::ExtendedOrdinal | Self::ExtendedWeek => {
                IsoDelimiterStyle::Extended
            }
        }
    }

    /// Returns the exact character byte length produced by this date pattern.
    #[must_use]
    const fn encoded_len(self) -> usize {
        match self {
            Self::BasicOrdinal => 7,
            Self::BasicCalendar | Self::ExtendedOrdinal | Self::BasicWeek => 8,
            Self::ExtendedCalendar | Self::ExtendedWeek => 10,
        }
    }
}

/// Timezone suffix mode in an ISO 8601 time format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum IsoTzPattern {
    /// Local / unzoned time without suffix.
    Local,
    /// Literal `'Z'` UTC designator.
    UtcZ,
    /// Signed offset `"+hhmm"` (basic) or `"+hh:mm"` (extended).
    SignedOffset,
}

/// Parsed ISO/IEC 1989:2023 time format specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct IsoTimePattern {
    /// Basic (`"hhmmss"`) vs extended (`"hh:mm:ss"`).
    style: IsoDelimiterStyle,
    /// Optional `(decimal_sep, fractional_digit_count)` where count is `1..=9`.
    fraction: Option<(u8, u8)>,
    /// Timezone suffix mode.
    tz: IsoTzPattern,
}

impl IsoTimePattern {
    /// Returns the exact character byte length produced by this time pattern.
    #[must_use]
    const fn encoded_len(self) -> usize {
        let base = match self.style {
            IsoDelimiterStyle::Basic => 6,
            IsoDelimiterStyle::Extended => 8,
        };
        let frac = match self.fraction {
            None => 0,
            Some((_, n)) => 1 + (n as usize),
        };
        let tz = match (self.tz, self.style) {
            (IsoTzPattern::Local, _) => 0,
            (IsoTzPattern::UtcZ, _) => 1,
            (IsoTzPattern::SignedOffset, IsoDelimiterStyle::Basic) => 5,
            (IsoTzPattern::SignedOffset, IsoDelimiterStyle::Extended) => 6,
        };
        base + frac + tz
    }
}

/// Parsed ISO/IEC 1989:2023 date, time, or combined datetime format pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum IsoFormatSpec {
    /// Date-only format.
    Date(IsoDatePattern),
    /// Time-only format.
    Time(IsoTimePattern),
    /// Combined `<date>T<time>` format with matching delimiter style.
    DateTime(IsoDatePattern, IsoTimePattern),
}

/// Evaluates `FUNCTION CURRENT-DATE` (21-byte `"YYYYMMDDHHMMSShh+HHMM"`), honoring `COB_CURRENT_DATE`.
///
/// # Arguments
///
/// * `clock` - Base [`ClockSnapshot`] before applying any override.
/// * `cob_current_date` - Optional `COB_CURRENT_DATE` environment string override.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `cob_current_date` is malformed or encodes an invalid calendar date/time.
pub fn dt_current_date(clock: ClockSnapshot, cob_current_date: Option<&str>) -> Result<[u8; 21]> {
    let effective = apply_cob_current_date(clock, cob_current_date)?;
    validate_clock_snapshot(&effective, "CURRENT-DATE")?;
    Ok(format_21_byte_timestamp(&effective))
}

/// Evaluates `FUNCTION WHEN-COMPILED` (21-byte `"YYYYMMDDHHMMSShh+HHMM"`).
///
/// # Arguments
///
/// * `compiled_at` - Compile-time [`ClockSnapshot`] recorded for the compilation unit.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `compiled_at` falls outside valid ISO calendar/time bounds.
pub fn dt_when_compiled(compiled_at: ClockSnapshot) -> Result<[u8; 21]> {
    validate_clock_snapshot(&compiled_at, "WHEN-COMPILED")?;
    Ok(format_21_byte_timestamp(&compiled_at))
}

/// Evaluates `FUNCTION SECONDS-PAST-MIDNIGHT` (`0 <= s < 86401`, nanosecond scale `9`).
///
/// # Arguments
///
/// * `clock` - Active [`ClockSnapshot`].
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `clock` contains out-of-bounds time fields.
pub fn dt_seconds_past_midnight(clock: ClockSnapshot) -> Result<FixedDecimal> {
    validate_clock_snapshot(&clock, "SECONDS-PAST-MIDNIGHT")?;
    let whole_secs =
        u64::from(clock.hour) * 3_600 + u64::from(clock.minute) * 60 + u64::from(clock.second);
    let total_nanos = whole_secs * 1_000_000_000 + u64::from(clock.nanosecond);
    Ok(FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::from_u64(total_nanos), DecimalSign::Positive),
        9,
    )
    .unwrap_or(FixedDecimal::ZERO))
}

/// Evaluates `FUNCTION INTEGER-OF-DATE(yyyymmdd)` (`16010101 -> 1` .. `99991231 -> 3,067,671`).
///
/// # Arguments
///
/// * `yyyymmdd` - 8-digit Gregorian integer `YYYYMMDD`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `yyyymmdd` is not a valid Gregorian date in `1601..=9999`.
pub fn dt_integer_of_date(yyyymmdd: i64) -> Result<i64> {
    if dt_test_date_yyyymmdd(yyyymmdd) != 0 {
        return Err(ec_argument_function(
            "INTEGER-OF-DATE",
            yyyymmdd.to_string(),
            "argument must be a valid Gregorian YYYYMMDD in 16010101..=99991231",
        ));
    }
    let year = u16::try_from(yyyymmdd / 10_000).unwrap_or(1601);
    let month = u8::try_from((yyyymmdd / 100) % 100).unwrap_or(1);
    let day = u8::try_from(yyyymmdd % 100).unwrap_or(1);
    Ok(i64::from(lilian_from_ymd(year, month, day)))
}

/// Evaluates `FUNCTION DATE-OF-INTEGER(lilian)` (`1 -> 16010101` .. `3,067,671 -> 99991231`).
///
/// # Arguments
///
/// * `lilian` - Lilian day number in `1..=3,067,671`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `lilian` lies outside `1..=3,067,671`.
pub fn dt_date_of_integer(lilian: i64) -> Result<i64> {
    let lilian_u32 = validate_lilian("DATE-OF-INTEGER", lilian)?;
    let (year, month, day) = ymd_from_lilian_u32(lilian_u32);
    Ok(i64::from(year) * 10_000 + i64::from(month) * 100 + i64::from(day))
}

/// Evaluates `FUNCTION INTEGER-OF-DAY(yyyyddd)` (`1601001 -> 1` .. `9999365 -> 3,067,671`).
///
/// # Arguments
///
/// * `yyyyddd` - 7-digit Julian/ordinal integer `YYYYDDD`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `yyyyddd` is not a valid ordinal date in `1601..=9999`.
pub fn dt_integer_of_day(yyyyddd: i64) -> Result<i64> {
    if dt_test_day_yyyyddd(yyyyddd) != 0 {
        return Err(ec_argument_function(
            "INTEGER-OF-DAY",
            yyyyddd.to_string(),
            "argument must be a valid ordinal YYYYDDD in 1601001..=9999365",
        ));
    }
    let year = u16::try_from(yyyyddd / 1_000).unwrap_or(1601);
    let doy = u16::try_from(yyyyddd % 1_000).unwrap_or(1);
    Ok(i64::from(days_before_year(year) + u32::from(doy)))
}

/// Evaluates `FUNCTION DAY-OF-INTEGER(lilian)` (`1 -> 1601001` .. `3,067,671 -> 9999365`).
///
/// # Arguments
///
/// * `lilian` - Lilian day number in `1..=3,067,671`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `lilian` lies outside `1..=3,067,671`.
pub fn dt_day_of_integer(lilian: i64) -> Result<i64> {
    let lilian_u32 = validate_lilian("DAY-OF-INTEGER", lilian)?;
    let (year, doy) = ordinal_from_lilian_u32(lilian_u32);
    Ok(i64::from(year) * 1_000 + i64::from(doy))
}

/// Evaluates `FUNCTION YEAR-TO-YYYY(yy [, window_offset [, execution_year]])`.
///
/// # Arguments
///
/// * `yy` - 2-digit year in `0..=99`.
/// * `window_offset` - Sliding window upper offset (defaults to `50` when `None`).
/// * `execution_year` - Reference 4-digit year (`1601..=9999`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `yy` is outside `0..=99`, `execution_year` is outside
/// `1601..=9999`, or `execution_year + window_offset` lies outside `1700..=9999`.
pub fn dt_year_to_yyyy(yy: i64, window_offset: Option<i64>, execution_year: u16) -> Result<i64> {
    resolve_sliding_year("YEAR-TO-YYYY", yy, window_offset, execution_year)
}

/// Evaluates `FUNCTION DATE-TO-YYYYMMDD(yymmdd [, window_offset [, execution_year]])`.
///
/// # Arguments
///
/// * `yymmdd` - 6-digit date `YYMMDD` (`000101..=991231`).
/// * `window_offset` - Sliding window upper offset (defaults to `50` when `None`).
/// * `execution_year` - Reference 4-digit year (`1601..=9999`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `yymmdd` or the resulting 4-digit calendar date is invalid.
pub fn dt_date_to_yyyymmdd(
    yymmdd: i64,
    window_offset: Option<i64>,
    execution_year: u16,
) -> Result<i64> {
    if !(101..=991_231).contains(&yymmdd) {
        return Err(ec_argument_function(
            "DATE-TO-YYYYMMDD",
            yymmdd.to_string(),
            "argument must lie in 000101..=991231",
        ));
    }
    let yy = yymmdd / 10_000;
    let mmdd = yymmdd % 10_000;
    let yyyy = resolve_sliding_year("DATE-TO-YYYYMMDD", yy, window_offset, execution_year)?;
    let full = yyyy * 10_000 + mmdd;
    if dt_test_date_yyyymmdd(full) != 0 {
        return Err(ec_argument_function(
            "DATE-TO-YYYYMMDD",
            yymmdd.to_string(),
            "resolved YYYYMMDD is not a valid calendar date",
        ));
    }
    Ok(full)
}

/// Evaluates `FUNCTION DAY-TO-YYYYDDD(yyddd [, window_offset [, execution_year]])`.
///
/// # Arguments
///
/// * `yyddd` - 5-digit ordinal date `YYDDD` (`00001..=99366`).
/// * `window_offset` - Sliding window upper offset (defaults to `50` when `None`).
/// * `execution_year` - Reference 4-digit year (`1601..=9999`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `yyddd` or the resulting 4-digit ordinal date is invalid.
pub fn dt_day_to_yyyyddd(
    yyddd: i64,
    window_offset: Option<i64>,
    execution_year: u16,
) -> Result<i64> {
    if !(1..=99_366).contains(&yyddd) {
        return Err(ec_argument_function(
            "DAY-TO-YYYYDDD",
            yyddd.to_string(),
            "argument must lie in 00001..=99366",
        ));
    }
    let yy = yyddd / 1_000;
    let ddd = yyddd % 1_000;
    let yyyy = resolve_sliding_year("DAY-TO-YYYYDDD", yy, window_offset, execution_year)?;
    let full = yyyy * 1_000 + ddd;
    if dt_test_day_yyyyddd(full) != 0 {
        return Err(ec_argument_function(
            "DAY-TO-YYYYDDD",
            yyddd.to_string(),
            "resolved YYYYDDD is not a valid ordinal date",
        ));
    }
    Ok(full)
}

/// Evaluates `FUNCTION COMBINED-DATETIME(lilian, seconds)` (`lilian + seconds / 100,000`).
///
/// # Arguments
///
/// * `lilian` - Lilian date integer in `1..=3,067,671`.
/// * `seconds` - Seconds past midnight satisfying `0 <= seconds < 86,400`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `lilian` or `seconds` is out of bounds.
pub fn dt_combined_datetime(lilian: i64, seconds: &FixedDecimal) -> Result<FixedDecimal> {
    let _ = validate_lilian("COMBINED-DATETIME", lilian)?;
    validate_seconds_range("COMBINED-DATETIME", seconds)?;
    let shifted_scale = seconds.scale().saturating_add(5);
    if shifted_scale > FixedDecimal::MAX_INTERMEDIATE_SCALE {
        return Err(ec_argument_function(
            "COMBINED-DATETIME",
            seconds.to_string(),
            "seconds fractional precision exceeds decimal capacity",
        ));
    }
    let frac =
        FixedDecimal::try_new(seconds.mantissa(), shifted_scale).unwrap_or(FixedDecimal::ZERO);
    let base = FixedDecimal::from_i64(lilian);
    let (combined, _) = base
        .checked_add(&frac)
        .unwrap_or((FixedDecimal::ZERO, SizeErrorStatus::Exact));
    Ok(combined)
}

/// Evaluates `FUNCTION FORMATTED-CURRENT-DATE(format_str)` honoring `COB_CURRENT_DATE`.
///
/// # Arguments
///
/// * `format_str` - ISO 2023 date, time, or combined datetime format literal.
/// * `clock` - Base [`ClockSnapshot`].
/// * `cob_current_date` - Optional `COB_CURRENT_DATE` override.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `format_str` or the effective clock is invalid.
pub fn dt_formatted_current_date(
    format_str: &str,
    clock: ClockSnapshot,
    cob_current_date: Option<&str>,
) -> Result<String> {
    let effective = apply_cob_current_date(clock, cob_current_date)?;
    validate_clock_snapshot(&effective, "FORMATTED-CURRENT-DATE")?;
    let spec = parse_iso_format("FORMATTED-CURRENT-DATE", format_str)?;
    let lilian = i64::from(lilian_from_ymd(
        effective.year,
        effective.month,
        effective.day,
    ));
    let secs = dt_seconds_past_midnight(effective)?;
    let offset_min = match effective.offset {
        UtcOffsetSpec::Unknown => 0,
        UtcOffsetSpec::Minutes(m) => i32::from(m),
    };
    match spec {
        IsoFormatSpec::Date(dp) => Ok(render_iso_date(dp, u32::try_from(lilian).unwrap_or(1))),
        IsoFormatSpec::Time(tp) => {
            let (_, adj_secs, eff_off) = adjust_for_timezone(
                "FORMATTED-CURRENT-DATE",
                tp.tz,
                lilian,
                &secs,
                Some(offset_min),
                None,
            )?;
            Ok(render_iso_time(tp, &adj_secs, eff_off))
        }
        IsoFormatSpec::DateTime(dp, tp) => {
            let (adj_lilian, adj_secs, eff_off) = adjust_for_timezone(
                "FORMATTED-CURRENT-DATE",
                tp.tz,
                lilian,
                &secs,
                Some(offset_min),
                None,
            )?;
            let mut out = render_iso_date(dp, adj_lilian);
            out.push('T');
            out.push_str(&render_iso_time(tp, &adj_secs, eff_off));
            Ok(out)
        }
    }
}

/// Evaluates `FUNCTION FORMATTED-DATE(format_str, lilian)`.
///
/// # Arguments
///
/// * `format_str` - ISO 2023 date format pattern (`YYYYMMDD`, `YYYY-MM-DD`, `YYYYDDD`, `YYYY-DDD`, `YYYYWwwD`, `YYYY-Www-D`).
/// * `lilian` - Lilian date integer in `1..=3,067,671`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `format_str` is not a date-only format or `lilian` is out of bounds.
pub fn dt_formatted_date(format_str: &str, lilian: i64) -> Result<String> {
    let spec = parse_iso_format("FORMATTED-DATE", format_str)?;
    let IsoFormatSpec::Date(dp) = spec else {
        return Err(ec_argument_function(
            "FORMATTED-DATE",
            format_str,
            "format must be an ISO date pattern without time components",
        ));
    };
    let lilian_u32 = validate_lilian("FORMATTED-DATE", lilian)?;
    Ok(render_iso_date(dp, lilian_u32))
}

/// Evaluates `FUNCTION FORMATTED-TIME(format_str, seconds [, offset_minutes [, system_offset_minutes]])`.
///
/// # Arguments
///
/// * `format_str` - ISO 2023 time format pattern (`hhmmss`, `hh:mm:ss`, optional `.s..s`/`,s..s` and `Z`/`+hhmm`/`+hh:mm`).
/// * `seconds` - Seconds past midnight (`0 <= seconds < 86,400`).
/// * `offset_minutes` - Optional explicit UTC offset in minutes (`-1439..=+1439`).
/// * `system_offset_minutes` - Optional UTC conversion offset when `format_str` ends with `Z`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `format_str` is not a time-only format, `seconds` is invalid,
/// or required offset arguments are missing/out of range.
pub fn dt_formatted_time(
    format_str: &str,
    seconds: &FixedDecimal,
    offset_minutes: Option<i32>,
    system_offset_minutes: Option<i32>,
) -> Result<String> {
    let spec = parse_iso_format("FORMATTED-TIME", format_str)?;
    let IsoFormatSpec::Time(tp) = spec else {
        return Err(ec_argument_function(
            "FORMATTED-TIME",
            format_str,
            "format must be an ISO time pattern without date components",
        ));
    };
    validate_seconds_range("FORMATTED-TIME", seconds)?;
    let (_, adj_secs, eff_off) = adjust_for_timezone(
        "FORMATTED-TIME",
        tp.tz,
        2,
        seconds,
        offset_minutes,
        system_offset_minutes,
    )?;
    Ok(render_iso_time(tp, &adj_secs, eff_off))
}

/// Evaluates `FUNCTION FORMATTED-DATETIME(format_str, lilian, seconds [, offset_minutes [, system_offset_minutes]])`.
///
/// # Arguments
///
/// * `format_str` - ISO 2023 combined `<date>T<time>` format pattern.
/// * `lilian` - Lilian date integer in `1..=3,067,671`.
/// * `seconds` - Seconds past midnight (`0 <= seconds < 86,400`).
/// * `offset_minutes` - Optional UTC offset in minutes (`-1439..=+1439`).
/// * `system_offset_minutes` - Optional system offset when converting to `Z`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` on invalid format, date/time bounds, or rolled UTC date overflow.
pub fn dt_formatted_datetime(
    format_str: &str,
    lilian: i64,
    seconds: &FixedDecimal,
    offset_minutes: Option<i32>,
    system_offset_minutes: Option<i32>,
) -> Result<String> {
    let spec = parse_iso_format("FORMATTED-DATETIME", format_str)?;
    let IsoFormatSpec::DateTime(dp, tp) = spec else {
        return Err(ec_argument_function(
            "FORMATTED-DATETIME",
            format_str,
            "format must be a combined ISO <date>T<time> pattern",
        ));
    };
    let _ = validate_lilian("FORMATTED-DATETIME", lilian)?;
    validate_seconds_range("FORMATTED-DATETIME", seconds)?;
    let (adj_lilian, adj_secs, eff_off) = adjust_for_timezone(
        "FORMATTED-DATETIME",
        tp.tz,
        lilian,
        seconds,
        offset_minutes,
        system_offset_minutes,
    )?;
    let mut out = render_iso_date(dp, adj_lilian);
    out.push('T');
    out.push_str(&render_iso_time(tp, &adj_secs, eff_off));
    Ok(out)
}

/// Evaluates `FUNCTION INTEGER-OF-FORMATTED-DATE(format_str, formatted_value)`.
///
/// # Arguments
///
/// * `format_str` - ISO 2023 date or combined datetime format pattern.
/// * `formatted_value` - Formatted date or datetime string matching `format_str`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `format_str` has no date part or `formatted_value` does not conform.
pub fn dt_integer_of_formatted_date(format_str: &str, formatted_value: &str) -> Result<i64> {
    let spec = parse_iso_format("INTEGER-OF-FORMATTED-DATE", format_str)?;
    let dp = match spec {
        IsoFormatSpec::Date(d) | IsoFormatSpec::DateTime(d, _) => d,
        IsoFormatSpec::Time(_) => {
            return Err(ec_argument_function(
                "INTEGER-OF-FORMATTED-DATE",
                format_str,
                "format must include a date pattern",
            ));
        }
    };
    if check_formatted_against_spec(spec, formatted_value.as_bytes()) != 0 {
        return Err(ec_argument_function(
            "INTEGER-OF-FORMATTED-DATE",
            formatted_value,
            "value does not match ISO date/datetime format",
        ));
    }
    let date_bytes = formatted_value
        .as_bytes()
        .get(..dp.encoded_len())
        .unwrap_or(&[]);
    Ok(i64::from(parse_validated_iso_date(dp, date_bytes)))
}

/// Evaluates `FUNCTION SECONDS-FROM-FORMATTED-TIME(format_str, formatted_value)`.
///
/// # Arguments
///
/// * `format_str` - ISO 2023 time or combined datetime format pattern.
/// * `formatted_value` - Formatted time or datetime string matching `format_str`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `format_str` has no time part or `formatted_value` does not conform.
pub fn dt_seconds_from_formatted_time(
    format_str: &str,
    formatted_value: &str,
) -> Result<FixedDecimal> {
    let spec = parse_iso_format("SECONDS-FROM-FORMATTED-TIME", format_str)?;
    let (tp, start_idx) = match spec {
        IsoFormatSpec::Time(t) => (t, 0),
        IsoFormatSpec::DateTime(d, t) => (t, d.encoded_len() + 1),
        IsoFormatSpec::Date(_) => {
            return Err(ec_argument_function(
                "SECONDS-FROM-FORMATTED-TIME",
                format_str,
                "format must include a time pattern",
            ));
        }
    };
    if check_formatted_against_spec(spec, formatted_value.as_bytes()) != 0 {
        return Err(ec_argument_function(
            "SECONDS-FROM-FORMATTED-TIME",
            formatted_value,
            "value does not match ISO time/datetime format",
        ));
    }
    let time_bytes = formatted_value
        .as_bytes()
        .get(start_idx..start_idx + tp.encoded_len())
        .unwrap_or(&[]);
    Ok(parse_validated_iso_time_seconds(tp, time_bytes))
}

/// Evaluates `FUNCTION TEST-DATE-YYYYMMDD(candidate)`.
///
/// Returns `0` when valid, `1` for invalid year, `2` for invalid month, or `3` for invalid day.
///
/// # Arguments
///
/// * `candidate` - Integer candidate `YYYYMMDD`.
#[must_use]
pub fn dt_test_date_yyyymmdd(candidate: i64) -> u8 {
    if !(16_010_000..=99_999_999).contains(&candidate) {
        return 1;
    }
    let year = u16::try_from(candidate / 10_000).unwrap_or(0);
    let month = u8::try_from((candidate / 100) % 100).unwrap_or(0);
    if !(1..=12).contains(&month) {
        return 2;
    }
    let day = u8::try_from(candidate % 100).unwrap_or(0);
    let max_day = days_in_month(year, month);
    if day < 1 || day > max_day {
        return 3;
    }
    0
}

/// Evaluates `FUNCTION TEST-DAY-YYYYDDD(candidate)`.
///
/// Returns `0` when valid, `1` for invalid year, or `2` for invalid day-of-year.
///
/// # Arguments
///
/// * `candidate` - Integer candidate `YYYYDDD`.
#[must_use]
pub fn dt_test_day_yyyyddd(candidate: i64) -> u8 {
    if !(1_601_000..=9_999_999).contains(&candidate) {
        return 1;
    }
    let year = u16::try_from(candidate / 1_000).unwrap_or(0);
    let doy = u16::try_from(candidate % 1_000).unwrap_or(0);
    if doy < 1 || doy > days_in_year(year) {
        return 2;
    }
    0
}

/// Evaluates `FUNCTION TEST-FORMATTED-DATETIME(format_str, candidate_value)`.
///
/// Returns `0` when `candidate_value` strictly conforms to `format_str`, or the 1-based character
/// position of the first invalid character.
///
/// # Arguments
///
/// * `format_str` - ISO 2023 date, time, or combined datetime format pattern.
/// * `candidate_value` - Candidate formatted character string.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `format_str` itself is not a valid ISO format pattern.
pub fn dt_test_formatted_datetime(format_str: &str, candidate_value: &str) -> Result<usize> {
    let spec = parse_iso_format("TEST-FORMATTED-DATETIME", format_str)?;
    Ok(check_formatted_against_spec(
        spec,
        candidate_value.as_bytes(),
    ))
}

/// Evaluates `FUNCTION LOCALE-DATE(yyyymmdd_str [, locale])`.
///
/// # Arguments
///
/// * `yyyymmdd_str` - 8-character `"YYYYMMDD"` calendar date string.
/// * `locale` - Optional POSIX/ISO locale identifier (e.g. `"en_US"`, `"en_GB"`, `"de_DE"`, `"ja_JP"`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `yyyymmdd_str` is not a valid 8-digit calendar date in `1601..=9999`.
pub fn dt_locale_date(yyyymmdd_str: &str, locale: Option<&str>) -> Result<String> {
    let raw = yyyymmdd_str.trim();
    if raw.len() != 8 || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ec_argument_function(
            "LOCALE-DATE",
            yyyymmdd_str,
            "argument must be an 8-digit YYYYMMDD string",
        ));
    }
    let num = raw.parse::<i64>().unwrap_or(0);
    if dt_test_date_yyyymmdd(num) != 0 {
        return Err(ec_argument_function(
            "LOCALE-DATE",
            yyyymmdd_str,
            "argument is not a valid calendar date",
        ));
    }
    let yyyy = raw.get(0..4).unwrap_or("");
    let mm = raw.get(4..6).unwrap_or("");
    let dd = raw.get(6..8).unwrap_or("");
    let tag = normalize_locale_tag("LOCALE-DATE", locale)?;
    Ok(match tag.as_str() {
        "en_us" => format!("{mm}/{dd}/{yyyy}"),
        "en_gb" | "fr_fr" | "es_es" => format!("{dd}/{mm}/{yyyy}"),
        "de_de" => format!("{dd}.{mm}.{yyyy}"),
        "ja_jp" => format!("{yyyy}/{mm}/{dd}"),
        _ => format!("{yyyy}-{mm}-{dd}"),
    })
}

/// Evaluates `FUNCTION LOCALE-TIME(hhmmss_str [, locale])`.
///
/// # Arguments
///
/// * `hhmmss_str` - `"HHMMSS"` (6+ digit) time string (`00..=23`, `00..=59`, `00..=60`).
/// * `locale` - Optional POSIX/ISO locale identifier.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `hhmmss_str` does not begin with a valid 6-digit `HHMMSS` prefix.
pub fn dt_locale_time(hhmmss_str: &str, locale: Option<&str>) -> Result<String> {
    let trimmed = hhmmss_str.trim_end_matches(' ');
    let Some(prefix) = trimmed.get(0..6) else {
        return Err(ec_argument_function(
            "LOCALE-TIME",
            hhmmss_str,
            "argument must contain at least 6 HHMMSS digits",
        ));
    };
    if !prefix.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ec_argument_function(
            "LOCALE-TIME",
            hhmmss_str,
            "HHMMSS prefix must be numeric digits",
        ));
    }
    let hh = parse_2digits(prefix.as_bytes(), 0);
    let mm = parse_2digits(prefix.as_bytes(), 2);
    let ss = parse_2digits(prefix.as_bytes(), 4);
    if hh > 23 || mm > 59 || ss > 60 {
        return Err(ec_argument_function(
            "LOCALE-TIME",
            hhmmss_str,
            "HHMMSS values out of range",
        ));
    }
    let tag = normalize_locale_tag("LOCALE-TIME", locale)?;
    Ok(format_hms_for_locale(hh, mm, ss, &tag))
}

/// Evaluates `FUNCTION LOCALE-TIME-FROM-SECONDS(seconds [, locale])`.
///
/// # Arguments
///
/// * `seconds` - Seconds past midnight (`0 <= seconds < 86,400`).
/// * `locale` - Optional POSIX/ISO locale identifier.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `seconds` lies outside `[0, 86400)` or `locale` is invalid.
pub fn dt_locale_time_from_seconds(seconds: &FixedDecimal, locale: Option<&str>) -> Result<String> {
    validate_seconds_range("LOCALE-TIME-FROM-SECONDS", seconds)?;
    let whole = seconds
        .rescale(0, RoundingMode::Truncation)
        .map_or(0_i32, |(d, _)| d.to_exact_i32().unwrap_or(0));
    let hh = u8::try_from(whole / 3_600).unwrap_or(0);
    let mm = u8::try_from((whole % 3_600) / 60).unwrap_or(0);
    let ss = u8::try_from(whole % 60).unwrap_or(0);
    let tag = normalize_locale_tag("LOCALE-TIME-FROM-SECONDS", locale)?;
    Ok(format_hms_for_locale(hh, mm, ss, &tag))
}

/// Returns `true` when `year` is a Gregorian leap year.
///
/// # Arguments
///
/// * `year` - 4-digit Gregorian year.
#[must_use]
const fn is_leap_year(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// Returns the number of days (`28..=31`) in `(year, month)`.
///
/// # Arguments
///
/// * `year` - 4-digit Gregorian year.
/// * `month` - 1-based month (`1..=12`).
#[must_use]
const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 31,
    }
}

/// Returns `366` for leap years and `365` otherwise.
///
/// # Arguments
///
/// * `year` - 4-digit Gregorian year.
#[must_use]
const fn days_in_year(year: u16) -> u16 {
    if is_leap_year(year) { 366 } else { 365 }
}

/// Computes elapsed days between `1601-01-01` and `year-01-01` (`year >= 1601`).
///
/// # Arguments
///
/// * `year` - Gregorian year `>= 1601`.
#[must_use]
const fn days_before_year(year: u16) -> u32 {
    let y = (year - 1601) as u32;
    y * 365 + y / 4 - y / 100 + y / 400
}

/// Converts validated `(year, month, day)` in `1601..=9999` into a 1-based Lilian integer.
///
/// # Arguments
///
/// * `year` - Validated Gregorian year (`1601..=9999`).
/// * `month` - Validated month (`1..=12`).
/// * `day` - Validated day of month (`1..=31`).
#[must_use]
const fn lilian_from_ymd(year: u16, month: u8, day: u8) -> u32 {
    /// Cumulative non-leap day offsets preceding months `1..=12`.
    const CUM_MONTH_DAYS: [u16; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let idx = (month - 1) as usize;
    let base_doy = match idx {
        0 => CUM_MONTH_DAYS[0],
        1 => CUM_MONTH_DAYS[1],
        2 => CUM_MONTH_DAYS[2],
        3 => CUM_MONTH_DAYS[3],
        4 => CUM_MONTH_DAYS[4],
        5 => CUM_MONTH_DAYS[5],
        6 => CUM_MONTH_DAYS[6],
        7 => CUM_MONTH_DAYS[7],
        8 => CUM_MONTH_DAYS[8],
        9 => CUM_MONTH_DAYS[9],
        10 => CUM_MONTH_DAYS[10],
        _ => CUM_MONTH_DAYS[11],
    };
    let leap_extra = if month > 2 && is_leap_year(year) {
        1
    } else {
        0
    };
    days_before_year(year) + (base_doy + leap_extra + (day as u16)) as u32
}

/// Decomposes a validated Lilian integer (`1..=3,067,671`) into `(year, day_of_year)`.
///
/// # Arguments
///
/// * `lilian` - Validated Lilian day number.
#[must_use]
fn ordinal_from_lilian_u32(lilian: u32) -> (u16, u16) {
    let mut d = lilian - 1;
    let c400 = d / 146_097;
    d %= 146_097;
    let c100 = (d / 36_524).min(3);
    d -= c100 * 36_524;
    let c4 = d / 1_461;
    d %= 1_461;
    let c1 = (d / 365).min(3);
    d -= c1 * 365;
    let year_u32 = 1601 + c400 * 400 + c100 * 100 + c4 * 4 + c1;
    let year = u16::try_from(year_u32).unwrap_or(9999);
    let doy = u16::try_from(d + 1).unwrap_or(1);
    (year, doy)
}

/// Decomposes a validated Lilian integer (`1..=3,067,671`) into `(year, month, day)`.
///
/// # Arguments
///
/// * `lilian` - Validated Lilian day number.
#[must_use]
fn ymd_from_lilian_u32(lilian: u32) -> (u16, u8, u8) {
    let (year, mut rem_doy) = ordinal_from_lilian_u32(lilian);
    let mut month = 1_u8;
    while month < 12 {
        let dim = u16::from(days_in_month(year, month));
        if rem_doy <= dim {
            break;
        }
        rem_doy -= dim;
        month += 1;
    }
    let day = u8::try_from(rem_doy).unwrap_or(1);
    (year, month, day)
}

/// Returns the ISO 8601 weekday (`1 = Monday` .. `7 = Sunday`) of a Lilian day (`1601-01-01 = Monday`).
///
/// # Arguments
///
/// * `lilian` - Validated Lilian day number.
#[must_use]
const fn iso_weekday_of_lilian(lilian: u32) -> u8 {
    match (lilian - 1) % 7 {
        0 => 1,
        1 => 2,
        2 => 3,
        3 => 4,
        4 => 5,
        5 => 6,
        _ => 7,
    }
}

/// Returns the signed Lilian day number of Monday of ISO week 1 of `iso_year` (`1601..=10000`).
///
/// # Arguments
///
/// * `iso_year` - Gregorian/ISO week-year.
#[must_use]
fn iso_week1_monday(iso_year: u16) -> i32 {
    let jan4 = lilian_from_ymd(iso_year, 1, 4);
    let wd = i32::from(iso_weekday_of_lilian(jan4));
    i32::try_from(jan4).unwrap_or(0) - (wd - 1)
}

/// Returns the number of ISO weeks (`52` or `53`) in `iso_year` (`1601..=9999`).
///
/// # Arguments
///
/// * `iso_year` - ISO week-year (`1601..=9999`).
#[must_use]
fn iso_weeks_in_year(iso_year: u16) -> u8 {
    let start = iso_week1_monday(iso_year);
    let next = iso_week1_monday(iso_year + 1);
    u8::try_from((next - start) / 7).unwrap_or(52)
}

/// Decomposes a validated Lilian integer (`1..=3,067,671`) into ISO `(week_year, week, weekday)`.
///
/// # Arguments
///
/// * `lilian` - Validated Lilian day number.
#[must_use]
fn iso_week_date_from_lilian(lilian: u32) -> (u16, u8, u8) {
    let (cal_year, _, _) = ymd_from_lilian_u32(lilian);
    let l_i32 = i32::try_from(lilian).unwrap_or(1);
    let iso_year = if l_i32 < iso_week1_monday(cal_year) {
        cal_year - 1
    } else if cal_year < 9999 && l_i32 >= iso_week1_monday(cal_year + 1) {
        cal_year + 1
    } else {
        cal_year
    };
    let w1 = iso_week1_monday(iso_year);
    let week = u8::try_from(((l_i32 - w1) / 7) + 1).unwrap_or(1);
    let weekday = iso_weekday_of_lilian(lilian);
    (iso_year, week, weekday)
}

/// Validates that `lilian` lies in `1..=3,067,671` and converts it to `u32`.
///
/// # Arguments
///
/// * `fn_name` - Intrinsic name for diagnostics.
/// * `lilian` - Candidate Lilian integer.
fn validate_lilian(fn_name: &'static str, lilian: i64) -> Result<u32> {
    if !(MIN_LILIAN_DAY..=MAX_LILIAN_DAY).contains(&lilian) {
        return Err(ec_argument_function(
            fn_name,
            lilian.to_string(),
            "Lilian integer must lie in 1..=3,067,671",
        ));
    }
    Ok(u32::try_from(lilian).unwrap_or(1))
}

/// Validates that `seconds` satisfies `0 <= seconds < 86,400`.
///
/// # Arguments
///
/// * `fn_name` - Intrinsic name for diagnostics.
/// * `seconds` - Candidate seconds past midnight.
fn validate_seconds_range(fn_name: &'static str, seconds: &FixedDecimal) -> Result<()> {
    let max_sec = FixedDecimal::from_i64(SECONDS_PER_DAY);
    if seconds.is_negative() || seconds.cmp_numeric(&max_sec).is_ge() {
        return Err(ec_argument_function(
            fn_name,
            seconds.to_string(),
            "seconds must satisfy 0 <= seconds < 86,400",
        ));
    }
    Ok(())
}

/// Resolves a 2-digit `yy` into a 4-digit `YYYY` under the ISO COBOL sliding century window.
///
/// # Arguments
///
/// * `fn_name` - Calling intrinsic name.
/// * `yy` - 2-digit year (`0..=99`).
/// * `window_offset` - Optional offset added to `execution_year` (defaults to `50`).
/// * `execution_year` - 4-digit reference year (`1601..=9999`).
fn resolve_sliding_year(
    fn_name: &'static str,
    yy: i64,
    window_offset: Option<i64>,
    execution_year: u16,
) -> Result<i64> {
    if !(0..=99).contains(&yy) {
        return Err(ec_argument_function(
            fn_name,
            yy.to_string(),
            "2-digit year must lie in 0..=99",
        ));
    }
    if !(1601..=9999).contains(&execution_year) {
        return Err(ec_argument_function(
            fn_name,
            execution_year.to_string(),
            "execution year must lie in 1601..=9999",
        ));
    }
    let offset = window_offset.unwrap_or(DEFAULT_CENTURY_WINDOW);
    let max_year = i64::from(execution_year).saturating_add(offset);
    if !(1700..=9999).contains(&max_year) {
        return Err(ec_argument_function(
            fn_name,
            format!("{execution_year}+{offset}"),
            "sliding window ceiling must lie in 1700..=9999",
        ));
    }
    let century = max_year / 100;
    let rem = max_year % 100;
    if rem >= yy {
        Ok(century * 100 + yy)
    } else {
        Ok((century - 1) * 100 + yy)
    }
}

/// Validates calendar and time bounds of a [`ClockSnapshot`].
///
/// # Arguments
///
/// * `snap` - Candidate snapshot.
/// * `fn_name` - Calling intrinsic name.
fn validate_clock_snapshot(snap: &ClockSnapshot, fn_name: &'static str) -> Result<()> {
    let ymd = i64::from(snap.year) * 10_000 + i64::from(snap.month) * 100 + i64::from(snap.day);
    if dt_test_date_yyyymmdd(ymd) != 0
        || snap.hour > 23
        || snap.minute > 59
        || snap.second > 60
        || snap.nanosecond > 999_999_999
    {
        return Err(ec_argument_function(
            fn_name,
            ymd.to_string(),
            "clock snapshot contains out-of-range calendar or time fields",
        ));
    }
    if let UtcOffsetSpec::Minutes(m) = snap.offset
        && i32::from(m).abs() > MAX_UTC_OFFSET_MINUTES
    {
        return Err(ec_argument_function(
            fn_name,
            m.to_string(),
            "UTC offset minutes must lie in -1439..=+1439",
        ));
    }
    Ok(())
}

/// Formats a validated [`ClockSnapshot`] into the 21-byte `"YYYYMMDDHHMMSShh+HHMM"` buffer.
///
/// # Arguments
///
/// * `snap` - Validated snapshot.
#[must_use]
fn format_21_byte_timestamp(snap: &ClockSnapshot) -> [u8; 21] {
    let hundredths = u8::try_from(snap.nanosecond / 10_000_000).unwrap_or(0);
    let (y0, y1) = two_ascii_digits(u8::try_from(snap.year / 100).unwrap_or(0));
    let (y2, y3) = two_ascii_digits(u8::try_from(snap.year % 100).unwrap_or(0));
    let (mo0, mo1) = two_ascii_digits(snap.month);
    let (d0, d1) = two_ascii_digits(snap.day);
    let (h0, h1) = two_ascii_digits(snap.hour);
    let (mi0, mi1) = two_ascii_digits(snap.minute);
    let (s0, s1) = two_ascii_digits(snap.second);
    let (c0, c1) = two_ascii_digits(hundredths);
    let (tz0, tz1, tz2, tz3, tz4) = match snap.offset {
        UtcOffsetSpec::Unknown => (b'0', b'0', b'0', b'0', b'0'),
        UtcOffsetSpec::Minutes(m) => {
            let sign_byte = if m < 0 { b'-' } else { b'+' };
            let mag = m.unsigned_abs();
            let (oh0, oh1) = two_ascii_digits(u8::try_from(mag / 60).unwrap_or(0));
            let (om0, om1) = two_ascii_digits(u8::try_from(mag % 60).unwrap_or(0));
            (sign_byte, oh0, oh1, om0, om1)
        }
    };
    [
        y0, y1, y2, y3, mo0, mo1, d0, d1, h0, h1, mi0, mi1, s0, s1, c0, c1, tz0, tz1, tz2, tz3, tz4,
    ]
}

/// Applies an optional `COB_CURRENT_DATE` environment override onto `base`.
///
/// Supports compact `"YYYYMMDD[HHMMSS[hh[(+|-)HHMM]]]"` and delimited ISO
/// `"YYYY-MM-DD[T| ]HH:MM:SS[.hh][Z|(+|-)HH:MM]"` strings.
///
/// # Arguments
///
/// * `base` - Live/base [`ClockSnapshot`].
/// * `override_str` - Optional `COB_CURRENT_DATE` value.
fn apply_cob_current_date(
    base: ClockSnapshot,
    override_str: Option<&str>,
) -> Result<ClockSnapshot> {
    let Some(raw) = override_str else {
        return Ok(base);
    };
    let trimmed = raw.trim();
    let mut digits = Vec::with_capacity(20);
    let mut tz_sign: Option<u8> = None;
    let mut tz_digits = Vec::with_capacity(4);
    for &b in trimmed.as_bytes() {
        if tz_sign.is_some() {
            if b.is_ascii_digit() {
                tz_digits.push(b);
            } else if b != b':' {
                return Err(ec_argument_function(
                    "CURRENT-DATE",
                    trimmed,
                    "invalid character in COB_CURRENT_DATE timezone suffix",
                ));
            }
        } else if b.is_ascii_digit() {
            digits.push(b);
        } else if (b == b'+' || b == b'-') && digits.len() >= 14 {
            tz_sign = Some(b);
        } else if b.eq_ignore_ascii_case(&b'Z') {
            tz_sign = Some(b'+');
            tz_digits.extend_from_slice(b"0000");
        } else if !matches!(b, b'-' | b'/' | b':' | b'T' | b't' | b' ' | b'.' | b',') {
            return Err(ec_argument_function(
                "CURRENT-DATE",
                trimmed,
                "invalid character in COB_CURRENT_DATE",
            ));
        }
    }
    if digits.len() < 8 {
        return Err(ec_argument_function(
            "CURRENT-DATE",
            trimmed,
            "COB_CURRENT_DATE must supply at least YYYYMMDD",
        ));
    }
    let mut snap = base;
    snap.year = parse_4digits(&digits, 0);
    snap.month = parse_2digits(&digits, 4);
    snap.day = parse_2digits(&digits, 6);
    if digits.len() >= 14 {
        snap.hour = parse_2digits(&digits, 8);
        snap.minute = parse_2digits(&digits, 10);
        snap.second = parse_2digits(&digits, 12);
        snap.nanosecond = if digits.len() >= 16 {
            u32::from(parse_2digits(&digits, 14)) * 10_000_000
        } else {
            0
        };
    }
    if let Some(sign) = tz_sign {
        if tz_digits.len() != 4 {
            return Err(ec_argument_function(
                "CURRENT-DATE",
                trimmed,
                "COB_CURRENT_DATE timezone offset must have 4 HHMM digits",
            ));
        }
        let hh = i16::from(parse_2digits(&tz_digits, 0));
        let mm = i16::from(parse_2digits(&tz_digits, 2));
        if hh > 23 || mm > 59 {
            return Err(ec_argument_function(
                "CURRENT-DATE",
                trimmed,
                "COB_CURRENT_DATE timezone HHMM out of range",
            ));
        }
        let mins = hh * 60 + mm;
        snap.offset = UtcOffsetSpec::Minutes(if sign == b'-' { -mins } else { mins });
    }
    Ok(snap)
}

/// Parses an ISO/IEC 1989:2023 date, time, or combined datetime format pattern.
///
/// # Arguments
///
/// * `fn_name` - Calling intrinsic name.
/// * `fmt` - Candidate format pattern string.
fn parse_iso_format(fn_name: &'static str, fmt: &str) -> Result<IsoFormatSpec> {
    let trimmed = fmt.trim_end_matches(' ');
    if let Some(dp) = parse_iso_date_pattern(trimmed) {
        return Ok(IsoFormatSpec::Date(dp));
    }
    if let Some((date_part, time_part)) = trimmed.split_once('T')
        && let (Some(dp), Some(tp)) = (
            parse_iso_date_pattern(date_part),
            parse_iso_time_pattern(time_part),
        )
        && dp.style() == tp.style
    {
        return Ok(IsoFormatSpec::DateTime(dp, tp));
    }
    if let Some(tp) = parse_iso_time_pattern(trimmed) {
        return Ok(IsoFormatSpec::Time(tp));
    }
    Err(ec_argument_function(
        fn_name,
        fmt,
        "unrecognized ISO 8601 date/time format string",
    ))
}

/// Recognizes one of the six ISO 2023 date format patterns.
///
/// # Arguments
///
/// * `pat` - Candidate date pattern slice.
#[must_use]
fn parse_iso_date_pattern(pat: &str) -> Option<IsoDatePattern> {
    match pat {
        "YYYYMMDD" => Some(IsoDatePattern::BasicCalendar),
        "YYYY-MM-DD" => Some(IsoDatePattern::ExtendedCalendar),
        "YYYYDDD" => Some(IsoDatePattern::BasicOrdinal),
        "YYYY-DDD" => Some(IsoDatePattern::ExtendedOrdinal),
        "YYYYWwwD" => Some(IsoDatePattern::BasicWeek),
        "YYYY-Www-D" => Some(IsoDatePattern::ExtendedWeek),
        _ => None,
    }
}

/// Recognizes an ISO 2023 time format pattern (`hhmmss` / `hh:mm:ss` + optional fraction + optional tz).
///
/// # Arguments
///
/// * `pat` - Candidate time pattern slice.
#[must_use]
fn parse_iso_time_pattern(pat: &str) -> Option<IsoTimePattern> {
    let (style, mut rest) = if let Some(r) = pat.strip_prefix("hh:mm:ss") {
        (IsoDelimiterStyle::Extended, r)
    } else {
        let r = pat.strip_prefix("hhmmss")?;
        (IsoDelimiterStyle::Basic, r)
    };

    let fraction = if let Some(&sep) = rest.as_bytes().first()
        && (sep == b'.' || sep == b',')
    {
        let after_sep = rest.get(1..).unwrap_or("");
        let s_count = after_sep.bytes().take_while(|&b| b == b's').count();
        if !(1..=9).contains(&s_count) {
            return None;
        }
        rest = after_sep.get(s_count..).unwrap_or("");
        Some((sep, u8::try_from(s_count).unwrap_or(1)))
    } else {
        None
    };

    let tz = match (rest, style) {
        ("", _) => IsoTzPattern::Local,
        ("Z", _) => IsoTzPattern::UtcZ,
        ("+hhmm", IsoDelimiterStyle::Basic) | ("+hh:mm", IsoDelimiterStyle::Extended) => {
            IsoTzPattern::SignedOffset
        }
        _ => return None,
    };

    Some(IsoTimePattern {
        style,
        fraction,
        tz,
    })
}

/// Adjusts `(lilian, seconds)` for timezone formatting (`Local`, `SignedOffset`, or `UtcZ` conversion).
///
/// # Arguments
///
/// * `fn_name` - Calling intrinsic name.
/// * `tz` - Target [`IsoTzPattern`].
/// * `lilian` - Local Lilian day (`1..=3,067,671`).
/// * `seconds` - Local seconds past midnight (`0 <= seconds < 86,400`).
/// * `offset_minutes` - Optional explicit offset in minutes (`-1439..=+1439`).
/// * `system_offset_minutes` - Optional UTC conversion offset when `tz == UtcZ`.
fn adjust_for_timezone(
    fn_name: &'static str,
    tz: IsoTzPattern,
    lilian: i64,
    seconds: &FixedDecimal,
    offset_minutes: Option<i32>,
    system_offset_minutes: Option<i32>,
) -> Result<(u32, FixedDecimal, i32)> {
    if let Some(off) = offset_minutes
        && off.abs() > MAX_UTC_OFFSET_MINUTES
    {
        return Err(ec_argument_function(
            fn_name,
            off.to_string(),
            "UTC offset must lie in -1439..=+1439 minutes",
        ));
    }
    if let Some(sys_off) = system_offset_minutes
        && sys_off.abs() > MAX_UTC_OFFSET_MINUTES
    {
        return Err(ec_argument_function(
            fn_name,
            sys_off.to_string(),
            "system UTC offset must lie in -1439..=+1439 minutes",
        ));
    }

    match tz {
        IsoTzPattern::Local => Ok((u32::try_from(lilian).unwrap_or(1), *seconds, 0)),
        IsoTzPattern::SignedOffset => {
            let Some(off) = offset_minutes else {
                return Err(ec_argument_function(
                    fn_name,
                    "missing-offset",
                    "offset argument is required when format specifies +hhmm/+hh:mm",
                ));
            };
            Ok((u32::try_from(lilian).unwrap_or(1), *seconds, off))
        }
        IsoTzPattern::UtcZ => {
            let active_off = system_offset_minutes.or(offset_minutes).unwrap_or(0);
            if active_off == 0 {
                return Ok((u32::try_from(lilian).unwrap_or(1), *seconds, 0));
            }
            let shift_secs = FixedDecimal::from_i64(i64::from(active_off) * 60);
            let (mut utc_secs, _) = seconds
                .checked_sub(&shift_secs)
                .unwrap_or((*seconds, SizeErrorStatus::Exact));
            let day_sec = FixedDecimal::from_i64(SECONDS_PER_DAY);
            let mut utc_lilian = lilian;
            if utc_secs.is_negative() {
                utc_secs = utc_secs
                    .checked_add(&day_sec)
                    .map_or(FixedDecimal::ZERO, |(d, _)| d);
                utc_lilian -= 1;
            } else if utc_secs.cmp_numeric(&day_sec).is_ge() {
                utc_secs = utc_secs
                    .checked_sub(&day_sec)
                    .map_or(FixedDecimal::ZERO, |(d, _)| d);
                utc_lilian += 1;
            }
            let validated_day = validate_lilian(fn_name, utc_lilian)?;
            Ok((validated_day, utc_secs, 0))
        }
    }
}

/// Formats `lilian` (`1..=3,067,671`) according to `dp`.
///
/// # Arguments
///
/// * `dp` - Target [`IsoDatePattern`].
/// * `lilian` - Validated Lilian day number.
#[must_use]
fn render_iso_date(dp: IsoDatePattern, lilian: u32) -> String {
    match dp {
        IsoDatePattern::BasicCalendar => {
            let (y, m, d) = ymd_from_lilian_u32(lilian);
            format!("{y:04}{m:02}{d:02}")
        }
        IsoDatePattern::ExtendedCalendar => {
            let (y, m, d) = ymd_from_lilian_u32(lilian);
            format!("{y:04}-{m:02}-{d:02}")
        }
        IsoDatePattern::BasicOrdinal => {
            let (y, doy) = ordinal_from_lilian_u32(lilian);
            format!("{y:04}{doy:03}")
        }
        IsoDatePattern::ExtendedOrdinal => {
            let (y, doy) = ordinal_from_lilian_u32(lilian);
            format!("{y:04}-{doy:03}")
        }
        IsoDatePattern::BasicWeek => {
            let (wy, w, wd) = iso_week_date_from_lilian(lilian);
            format!("{wy:04}W{w:02}{wd}")
        }
        IsoDatePattern::ExtendedWeek => {
            let (wy, w, wd) = iso_week_date_from_lilian(lilian);
            format!("{wy:04}-W{w:02}-{wd}")
        }
    }
}

/// Formats `seconds` (`0 <= seconds < 86,400`) and `offset_minutes` according to `tp`.
///
/// # Arguments
///
/// * `tp` - Target [`IsoTimePattern`].
/// * `seconds` - Validated seconds past midnight.
/// * `offset_minutes` - Signed UTC offset in minutes (`-1439..=+1439`).
#[must_use]
fn render_iso_time(tp: IsoTimePattern, seconds: &FixedDecimal, offset_minutes: i32) -> String {
    let frac_digits = tp.fraction.map_or(0_i16, |(_, n)| i16::from(n));
    let (quantized, _) = seconds
        .rescale(frac_digits, RoundingMode::Truncation)
        .unwrap_or((*seconds, SizeErrorStatus::Exact));
    let scale_pow = 10_u128.pow(u32::try_from(frac_digits).unwrap_or(0));
    let raw_units = quantized
        .mantissa()
        .to_i128()
        .map_or(0_u128, i128::unsigned_abs);
    let whole_secs = raw_units / scale_pow;
    let frac_val = raw_units % scale_pow;
    let hh = whole_secs / 3_600;
    let mm = (whole_secs % 3_600) / 60;
    let ss = whole_secs % 60;

    let mut out = match tp.style {
        IsoDelimiterStyle::Basic => format!("{hh:02}{mm:02}{ss:02}"),
        IsoDelimiterStyle::Extended => format!("{hh:02}:{mm:02}:{ss:02}"),
    };
    if let Some((sep, width)) = tp.fraction {
        out.push(char::from(sep));
        let w = usize::from(width);
        let _ = write!(out, "{frac_val:0w$}");
    }
    match tp.tz {
        IsoTzPattern::Local => {}
        IsoTzPattern::UtcZ => out.push('Z'),
        IsoTzPattern::SignedOffset => {
            let sign = if offset_minutes < 0 { '-' } else { '+' };
            let mag = offset_minutes.unsigned_abs();
            let oh = mag / 60;
            let om = mag % 60;
            match tp.style {
                IsoDelimiterStyle::Basic => {
                    let _ = write!(out, "{sign}{oh:02}{om:02}");
                }
                IsoDelimiterStyle::Extended => {
                    let _ = write!(out, "{sign}{oh:02}:{om:02}");
                }
            }
        }
    }
    out
}

/// Validates `bytes` against `spec`, returning `0` when valid or the 1-based position of the first invalid byte.
///
/// # Arguments
///
/// * `spec` - Parsed [`IsoFormatSpec`].
/// * `bytes` - Candidate formatted byte slice.
#[must_use]
fn check_formatted_against_spec(spec: IsoFormatSpec, bytes: &[u8]) -> usize {
    let expected_len = match spec {
        IsoFormatSpec::Date(dp) => {
            let err = check_iso_date_prefix(dp, bytes);
            if err != 0 {
                return err;
            }
            dp.encoded_len()
        }
        IsoFormatSpec::Time(tp) => {
            let err = check_iso_time_at(tp, bytes, 0);
            if err != 0 {
                return err;
            }
            tp.encoded_len()
        }
        IsoFormatSpec::DateTime(dp, tp) => {
            let d_err = check_iso_date_prefix(dp, bytes);
            if d_err != 0 {
                return d_err;
            }
            let t_sep_idx = dp.encoded_len();
            let Some(&sep) = bytes.get(t_sep_idx) else {
                return t_sep_idx + 1;
            };
            if sep != b'T' {
                return t_sep_idx + 1;
            }
            let time_start = t_sep_idx + 1;
            let t_err = check_iso_time_at(tp, bytes, time_start);
            if t_err != 0 {
                return t_err;
            }
            time_start + tp.encoded_len()
        }
    };
    for (idx, &b) in bytes.iter().enumerate().skip(expected_len) {
        if b != b' ' {
            return idx + 1;
        }
    }
    0
}

/// Validates the date prefix of `bytes` against `dp`, returning `0` or the 1-based offending index.
///
/// # Arguments
///
/// * `dp` - Expected [`IsoDatePattern`].
/// * `bytes` - Input byte slice starting at offset `0`.
#[must_use]
fn check_iso_date_prefix(dp: IsoDatePattern, bytes: &[u8]) -> usize {
    let y_err = check_bounded_digits(bytes, 0, 4, 1601, 9999);
    if y_err != 0 {
        return y_err;
    }
    let year = parse_4digits(bytes, 0);
    match dp {
        IsoDatePattern::BasicCalendar => {
            let m_err = check_bounded_digits(bytes, 4, 2, 1, 12);
            if m_err != 0 {
                return m_err;
            }
            let month = parse_2digits(bytes, 4);
            check_bounded_digits(bytes, 6, 2, 1, u32::from(days_in_month(year, month)))
        }
        IsoDatePattern::ExtendedCalendar => {
            if bytes.get(4).copied() != Some(b'-') {
                return 5;
            }
            let m_err = check_bounded_digits(bytes, 5, 2, 1, 12);
            if m_err != 0 {
                return m_err;
            }
            if bytes.get(7).copied() != Some(b'-') {
                return 8;
            }
            let month = parse_2digits(bytes, 5);
            check_bounded_digits(bytes, 8, 2, 1, u32::from(days_in_month(year, month)))
        }
        IsoDatePattern::BasicOrdinal => {
            check_bounded_digits(bytes, 4, 3, 1, u32::from(days_in_year(year)))
        }
        IsoDatePattern::ExtendedOrdinal => {
            if bytes.get(4).copied() != Some(b'-') {
                return 5;
            }
            check_bounded_digits(bytes, 5, 3, 1, u32::from(days_in_year(year)))
        }
        IsoDatePattern::BasicWeek => check_iso_week_tail(bytes, year, 4, false),
        IsoDatePattern::ExtendedWeek => {
            if bytes.get(4).copied() != Some(b'-') {
                return 5;
            }
            check_iso_week_tail(bytes, year, 5, true)
        }
    }
}

/// Validates the `Www[-]D` tail of an ISO week-date string, including Lilian bounds `1..=3,067,671`.
///
/// # Arguments
///
/// * `bytes` - Input byte slice.
/// * `year` - Validated 4-digit ISO week-year (`1601..=9999`).
/// * `w_pos` - 0-based index of literal `'W'`.
/// * `has_dash` - `true` for `"Www-D"`, `false` for `"WwwD"`.
#[must_use]
fn check_iso_week_tail(bytes: &[u8], year: u16, w_pos: usize, has_dash: bool) -> usize {
    if bytes.get(w_pos).copied() != Some(b'W') {
        return w_pos + 1;
    }
    let max_w = u32::from(iso_weeks_in_year(year));
    let w_err = check_bounded_digits(bytes, w_pos + 1, 2, 1, max_w);
    if w_err != 0 {
        return w_err;
    }
    let d_pos = if has_dash {
        let dash_idx = w_pos + 3;
        if bytes.get(dash_idx).copied() != Some(b'-') {
            return dash_idx + 1;
        }
        w_pos + 4
    } else {
        w_pos + 3
    };
    let d_err = check_bounded_digits(bytes, d_pos, 1, 1, 7);
    if d_err != 0 {
        return d_err;
    }
    let week = i32::from(parse_2digits(bytes, w_pos + 1));
    let wd = i32::from(bytes.get(d_pos).copied().unwrap_or(b'1') - b'0');
    let lilian = i64::from(iso_week1_monday(year) + (week - 1) * 7 + (wd - 1));
    if !(MIN_LILIAN_DAY..=MAX_LILIAN_DAY).contains(&lilian) {
        return d_pos + 1;
    }
    0
}

/// Validates the time substring starting at `base_idx` against `tp`, returning `0` or 1-based error index.
///
/// # Arguments
///
/// * `tp` - Expected [`IsoTimePattern`].
/// * `bytes` - Full input byte slice.
/// * `base_idx` - 0-based starting index of the time portion.
#[must_use]
fn check_iso_time_at(tp: IsoTimePattern, bytes: &[u8], base_idx: usize) -> usize {
    let mut pos = base_idx;
    let h_err = check_bounded_digits(bytes, pos, 2, 0, 23);
    if h_err != 0 {
        return h_err;
    }
    pos += 2;
    if tp.style == IsoDelimiterStyle::Extended {
        if bytes.get(pos).copied() != Some(b':') {
            return pos + 1;
        }
        pos += 1;
    }
    let m_err = check_bounded_digits(bytes, pos, 2, 0, 59);
    if m_err != 0 {
        return m_err;
    }
    pos += 2;
    if tp.style == IsoDelimiterStyle::Extended {
        if bytes.get(pos).copied() != Some(b':') {
            return pos + 1;
        }
        pos += 1;
    }
    let s_err = check_bounded_digits(bytes, pos, 2, 0, 59);
    if s_err != 0 {
        return s_err;
    }
    pos += 2;

    if let Some((sep, frac_count)) = tp.fraction {
        if bytes.get(pos).copied() != Some(sep) {
            return pos + 1;
        }
        pos += 1;
        for _ in 0..frac_count {
            let Some(&b) = bytes.get(pos) else {
                return pos + 1;
            };
            if !b.is_ascii_digit() {
                return pos + 1;
            }
            pos += 1;
        }
    }

    match tp.tz {
        IsoTzPattern::Local => 0,
        IsoTzPattern::UtcZ => {
            if bytes.get(pos).copied() == Some(b'Z') {
                0
            } else {
                pos + 1
            }
        }
        IsoTzPattern::SignedOffset => check_tz_offset_at(tp.style, bytes, pos),
    }
}

/// Validates a `+hhmm` or `+hh:mm` timezone suffix starting at `pos` (`-00:00` rejected per ISO 8601).
///
/// # Arguments
///
/// * `style` - Basic vs extended delimiter style.
/// * `bytes` - Full input byte slice.
/// * `pos` - 0-based index of `+` or `-`.
#[must_use]
fn check_tz_offset_at(style: IsoDelimiterStyle, bytes: &[u8], pos: usize) -> usize {
    let Some(&sign) = bytes.get(pos) else {
        return pos + 1;
    };
    if sign != b'+' && sign != b'-' && sign != b'0' {
        return pos + 1;
    }
    let h_err = check_bounded_digits(bytes, pos + 1, 2, 0, 23);
    if h_err != 0 {
        return h_err;
    }
    let m_pos = if style == IsoDelimiterStyle::Extended {
        if bytes.get(pos + 3).copied() != Some(b':') {
            return pos + 4;
        }
        pos + 4
    } else {
        pos + 3
    };
    let m_err = check_bounded_digits(bytes, m_pos, 2, 0, 59);
    if m_err != 0 {
        return m_err;
    }
    let oh = parse_2digits(bytes, pos + 1);
    let om = parse_2digits(bytes, m_pos);
    if sign == b'0'
        && let Some(idx) = [pos + 1, pos + 2, m_pos, m_pos + 1]
            .into_iter()
            .find(|&idx| bytes.get(idx).copied() != Some(b'0'))
    {
        return idx + 1;
    }
    // ISO 8601 / ISO COBOL forbids `-0000` / `-00:00`.
    if sign == b'-' && oh == 0 && om == 0 {
        return m_pos + 2;
    }
    0
}

/// Checks `width` ASCII digits starting at `start` against `[min_val, max_val]` character-by-character,
/// pinpointing the exact 1-based position where no completion in `[min_val, max_val]` remains possible.
///
/// # Arguments
///
/// * `bytes` - Input byte slice.
/// * `start` - 0-based start offset.
/// * `width` - Expected digit width (`1..=4`).
/// * `min_val` - Minimum valid integer value.
/// * `max_val` - Maximum valid integer value.
#[must_use]
fn check_bounded_digits(
    bytes: &[u8],
    start: usize,
    width: usize,
    min_val: u32,
    max_val: u32,
) -> usize {
    let mut prefix = 0_u32;
    for i in 0..width {
        let idx = start + i;
        let Some(&b) = bytes.get(idx) else {
            return idx + 1;
        };
        if !b.is_ascii_digit() {
            return idx + 1;
        }
        prefix = prefix * 10 + u32::from(b - b'0');
        let rem_digits = u32::try_from(width - 1 - i).unwrap_or(0);
        let span = 10_u32.pow(rem_digits);
        let lowest_completion = prefix * span;
        let highest_completion = prefix * span + (span - 1);
        if highest_completion < min_val || lowest_completion > max_val {
            return idx + 1;
        }
    }
    0
}

/// Parses an already-validated ISO date slice into a 1-based Lilian day number.
///
/// # Arguments
///
/// * `dp` - Validated [`IsoDatePattern`].
/// * `date_bytes` - Validated date bytes.
#[must_use]
fn parse_validated_iso_date(dp: IsoDatePattern, date_bytes: &[u8]) -> u32 {
    let year = parse_4digits(date_bytes, 0);
    match dp {
        IsoDatePattern::BasicCalendar => {
            let m = parse_2digits(date_bytes, 4);
            let d = parse_2digits(date_bytes, 6);
            lilian_from_ymd(year, m, d)
        }
        IsoDatePattern::ExtendedCalendar => {
            let m = parse_2digits(date_bytes, 5);
            let d = parse_2digits(date_bytes, 8);
            lilian_from_ymd(year, m, d)
        }
        IsoDatePattern::BasicOrdinal => {
            let doy = parse_3digits(date_bytes, 4);
            days_before_year(year) + u32::from(doy)
        }
        IsoDatePattern::ExtendedOrdinal => {
            let doy = parse_3digits(date_bytes, 5);
            days_before_year(year) + u32::from(doy)
        }
        IsoDatePattern::BasicWeek => {
            let w = i32::from(parse_2digits(date_bytes, 5));
            let wd = i32::from(date_bytes.get(7).copied().unwrap_or(b'1') - b'0');
            u32::try_from(iso_week1_monday(year) + (w - 1) * 7 + (wd - 1)).unwrap_or(1)
        }
        IsoDatePattern::ExtendedWeek => {
            let w = i32::from(parse_2digits(date_bytes, 6));
            let wd = i32::from(date_bytes.get(9).copied().unwrap_or(b'1') - b'0');
            u32::try_from(iso_week1_monday(year) + (w - 1) * 7 + (wd - 1)).unwrap_or(1)
        }
    }
}

/// Parses an already-validated ISO time slice into exact `FixedDecimal` seconds past midnight.
///
/// # Arguments
///
/// * `tp` - Validated [`IsoTimePattern`].
/// * `time_bytes` - Validated time bytes.
#[must_use]
fn parse_validated_iso_time_seconds(tp: IsoTimePattern, time_bytes: &[u8]) -> FixedDecimal {
    let (hh, mm, ss, frac_start) = match tp.style {
        IsoDelimiterStyle::Basic => (
            u64::from(parse_2digits(time_bytes, 0)),
            u64::from(parse_2digits(time_bytes, 2)),
            u64::from(parse_2digits(time_bytes, 4)),
            6_usize,
        ),
        IsoDelimiterStyle::Extended => (
            u64::from(parse_2digits(time_bytes, 0)),
            u64::from(parse_2digits(time_bytes, 3)),
            u64::from(parse_2digits(time_bytes, 6)),
            8_usize,
        ),
    };
    let whole_secs = hh * 3_600 + mm * 60 + ss;
    let Some((_, frac_digits)) = tp.fraction else {
        return FixedDecimal::try_new(
            SignedInt256::from_parts(Uint256::from_u64(whole_secs), DecimalSign::Positive),
            0,
        )
        .unwrap_or(FixedDecimal::ZERO);
    };
    let mut mant = whole_secs;
    for i in 0..usize::from(frac_digits) {
        let d = u64::from(time_bytes.get(frac_start + 1 + i).copied().unwrap_or(b'0') - b'0');
        mant = mant * 10 + d;
    }
    FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::from_u64(mant), DecimalSign::Positive),
        i16::from(frac_digits),
    )
    .unwrap_or(FixedDecimal::ZERO)
}

/// Normalizes an optional locale tag, rejecting explicitly empty/whitespace-only locale strings.
///
/// # Arguments
///
/// * `fn_name` - Calling intrinsic name.
/// * `locale` - Optional locale parameter.
fn normalize_locale_tag(fn_name: &'static str, locale: Option<&str>) -> Result<String> {
    let Some(raw) = locale else {
        return Ok(String::from("iso"));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ec_argument_function(
            fn_name,
            raw,
            "explicit locale name must not be empty",
        ));
    }
    let base = trimmed
        .split(['.', '@'])
        .next()
        .unwrap_or(trimmed)
        .replace('-', "_")
        .to_ascii_lowercase();
    Ok(base)
}

/// Formats `(hh, mm, ss)` according to a normalized locale tag (`en_us` 12-hour vs ISO 24-hour).
///
/// # Arguments
///
/// * `hh` - Hour (`0..=23`).
/// * `mm` - Minute (`0..=59`).
/// * `ss` - Second (`0..=60`).
/// * `locale_tag` - Normalized lowercase locale tag.
#[must_use]
fn format_hms_for_locale(hh: u8, mm: u8, ss: u8, locale_tag: &str) -> String {
    if locale_tag == "en_us" {
        let (h12, mer) = match hh {
            0 => (12, "AM"),
            1..=11 => (hh, "AM"),
            12 => (12, "PM"),
            _ => (hh - 12, "PM"),
        };
        return format!("{h12:02}:{mm:02}:{ss:02} {mer}");
    }
    format!("{hh:02}:{mm:02}:{ss:02}")
}

/// Encodes a two-digit `0..=99` value into ASCII tens and ones bytes.
///
/// # Arguments
///
/// * `val` - Unsigned byte value in `0..=99`.
#[must_use]
const fn two_ascii_digits(val: u8) -> (u8, u8) {
    (b'0' + (val / 10) % 10, b'0' + val % 10)
}

/// Reads a 2-digit ASCII number at `offset`.
///
/// # Arguments
///
/// * `bytes` - Digit buffer.
/// * `offset` - Starting index.
#[must_use]
fn parse_2digits(bytes: &[u8], offset: usize) -> u8 {
    let d0 = bytes
        .get(offset)
        .copied()
        .unwrap_or(b'0')
        .wrapping_sub(b'0');
    let d1 = bytes
        .get(offset + 1)
        .copied()
        .unwrap_or(b'0')
        .wrapping_sub(b'0');
    d0.saturating_mul(10).saturating_add(d1)
}

/// Reads a 3-digit ASCII number at `offset`.
///
/// # Arguments
///
/// * `bytes` - Digit buffer.
/// * `offset` - Starting index.
#[must_use]
fn parse_3digits(bytes: &[u8], offset: usize) -> u16 {
    let d0 = u16::from(
        bytes
            .get(offset)
            .copied()
            .unwrap_or(b'0')
            .wrapping_sub(b'0'),
    );
    let d12 = u16::from(parse_2digits(bytes, offset + 1));
    d0 * 100 + d12
}

/// Reads a 4-digit ASCII number at `offset`.
///
/// # Arguments
///
/// * `bytes` - Digit buffer.
/// * `offset` - Starting index.
#[must_use]
fn parse_4digits(bytes: &[u8], offset: usize) -> u16 {
    let hi = u16::from(parse_2digits(bytes, offset));
    let lo = u16::from(parse_2digits(bytes, offset + 2));
    hi * 100 + lo
}
