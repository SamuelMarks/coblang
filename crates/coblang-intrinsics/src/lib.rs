//! Pure-Rust ISO/IEC 1989:2023, `GnuCOBOL`, and `gcobol` intrinsic function library (`Milestone M12`).

#![deny(clippy::indexing_slicing)]

use coblang_common::{CoblangError, DigitCount, Result, ScaleFactor};
use coblang_decimal::SignednessPolicy;

/// Calendar, Lilian date, and ISO 8601 formatted date/time intrinsics (`Milestone M12`).
pub mod datetime;
/// Mathematical, trigonometric, and algebraic limit intrinsics (`Milestone M12`).
pub mod math;
/// National (`UTF-16`), `UTF-8`, module reflection, exception, and `UUID4` intrinsics (`Milestone M12`).
pub mod meta_unicode;
/// Financial, statistical, and deterministic PRNG intrinsics (`Milestone M12`).
pub mod stats;
/// String, numeric parsing, base/bit/hex conversion, and collation intrinsics (`Milestone M12`).
pub mod strings;

/// Logical subsystem grouping of a COBOL intrinsic function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntrinsicCategory {
    /// Mathematical, trigonometric, and algebraic limit functions (`src/math.rs`).
    Math,
    /// Financial, statistical, and PRNG functions (`src/stats.rs`).
    Stats,
    /// Calendar, Lilian date, and ISO 8601 formatted date/time functions (`src/datetime.rs`).
    DateTime,
    /// String, numeric parsing, base/bit conversion, and collation functions (`src/strings.rs`).
    Strings,
    /// National (`UTF-16`), `UTF-8`, reflection, exception, and `UUID4` functions (`src/meta_unicode.rs`).
    MetaUnicode,
}

/// Canonical COBOL value category returned by an intrinsic function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntrinsicReturnKind {
    /// General fixed-point or floating-point numeric result (`FixedDecimal`).
    Numeric,
    /// Exact integer numeric result (`FixedDecimal` with scale `0`).
    Integer,
    /// Alphanumeric / SBCS / UTF-8 character byte sequence (`Vec<u8>` / `String`).
    Alphanumeric,
    /// `USAGE NATIONAL` (`UTF-16`) character sequence (`Vec<u8>`).
    National,
}

/// Strongly-typed numeric/numeric-edited target descriptor for `HIGHEST-ALGEBRAIC`,
/// `LOWEST-ALGEBRAIC`, and `SMALLEST-ALGEBRAIC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AlgebraicTargetSpec {
    /// Declared decimal digit capacity (`1..=38`).
    pub digits: DigitCount,
    /// Signed decimal scale factor (`-38..=38`).
    pub scale: ScaleFactor,
    /// Signed (`PIC S9...`) vs unsigned (`PIC 9...`) capability.
    pub signed: SignednessPolicy,
}

impl AlgebraicTargetSpec {
    /// Constructs a validated [`AlgebraicTargetSpec`].
    ///
    /// # Arguments
    ///
    /// * `digits` - Validated decimal digit count (`1..=38`).
    /// * `scale` - Validated scale factor (`-38..=38`).
    /// * `signed` - Signedness capability of the target item.
    #[must_use]
    pub const fn new(digits: DigitCount, scale: ScaleFactor, signed: SignednessPolicy) -> Self {
        Self {
            digits,
            scale,
            signed,
        }
    }
}

/// Constructs an ISO/IEC 1989:2023 `EC-ARGUMENT-FUNCTION` domain violation error.
///
/// # Arguments
///
/// * `function_name` - Canonical uppercase COBOL intrinsic name (e.g. `"ACOS"`, `"FACTORIAL"`).
/// * `offending_value` - Stringified representation of the invalid argument(s).
/// * `reason` - Explanation of the violated ISO/dialect domain constraint.
#[must_use]
pub fn ec_argument_function(
    function_name: &'static str,
    offending_value: impl Into<String>,
    reason: impl Into<String>,
) -> CoblangError {
    let val = offending_value.into();
    CoblangError::InvalidDomainValue {
        domain_type: "EC-ARGUMENT-FUNCTION",
        value: format!("{function_name}({val})"),
        reason: reason.into(),
    }
}

/// Strongly-typed identifier for every ISO 2023, `GnuCOBOL`, and `gcobol` intrinsic function.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntrinsicId {
    /// `FUNCTION ABS(x)`
    Abs = 1,
    /// `FUNCTION ACOS(x)`
    Acos = 2,
    /// `FUNCTION ASIN(x)`
    Asin = 3,
    /// `FUNCTION ATAN(x)`
    Atan = 4,
    /// `FUNCTION COS(x)`
    Cos = 5,
    /// `FUNCTION E`
    E = 6,
    /// `FUNCTION EXP(x)`
    Exp = 7,
    /// `FUNCTION EXP10(x)`
    Exp10 = 8,
    /// `FUNCTION FACTORIAL(n)`
    Factorial = 9,
    /// `FUNCTION FRACTION-PART(x)`
    FractionPart = 10,
    /// `FUNCTION HIGHEST-ALGEBRAIC(item)`
    HighestAlgebraic = 11,
    /// `FUNCTION INTEGER(x)`
    Integer = 12,
    /// `FUNCTION INTEGER-PART(x)`
    IntegerPart = 13,
    /// `FUNCTION LOG(x)`
    Log = 14,
    /// `FUNCTION LOG10(x)`
    Log10 = 15,
    /// `FUNCTION LOWEST-ALGEBRAIC(item)`
    LowestAlgebraic = 16,
    /// `FUNCTION MOD(a, b)`
    Mod = 17,
    /// `FUNCTION PI`
    Pi = 18,
    /// `FUNCTION REM(a, b)`
    Rem = 19,
    /// `FUNCTION SIGN(x)`
    Sign = 20,
    /// `FUNCTION SIN(x)`
    Sin = 21,
    /// `FUNCTION SMALLEST-ALGEBRAIC(item)`
    SmallestAlgebraic = 22,
    /// `FUNCTION SQRT(x)`
    Sqrt = 23,
    /// `FUNCTION TAN(x)`
    Tan = 24,
    /// `FUNCTION ANNUITY(rate, periods)`
    Annuity = 25,
    /// `FUNCTION PRESENT-VALUE(rate, amt...)`
    PresentValue = 26,
    /// `FUNCTION MAX(arg...)`
    Max = 27,
    /// `FUNCTION MIN(arg...)`
    Min = 28,
    /// `FUNCTION ORD-MAX(arg...)`
    OrdMax = 29,
    /// `FUNCTION ORD-MIN(arg...)`
    OrdMin = 30,
    /// `FUNCTION SUM(arg...)`
    Sum = 31,
    /// `FUNCTION MEAN(arg...)`
    Mean = 32,
    /// `FUNCTION MEDIAN(arg...)`
    Median = 33,
    /// `FUNCTION MIDRANGE(arg...)`
    Midrange = 34,
    /// `FUNCTION RANGE(arg...)`
    Range = 35,
    /// `FUNCTION VARIANCE(arg...)`
    Variance = 36,
    /// `FUNCTION STANDARD-DEVIATION(arg...)`
    StandardDeviation = 37,
    /// `FUNCTION RANDOM([seed])`
    Random = 38,
    /// `FUNCTION CURRENT-DATE`
    CurrentDate = 39,
    /// `FUNCTION WHEN-COMPILED`
    WhenCompiled = 40,
    /// `FUNCTION SECONDS-PAST-MIDNIGHT`
    SecondsPastMidnight = 41,
    /// `FUNCTION INTEGER-OF-DATE(yyyymmdd)`
    IntegerOfDate = 42,
    /// `FUNCTION DATE-OF-INTEGER(lilian)`
    DateOfInteger = 43,
    /// `FUNCTION INTEGER-OF-DAY(yyyyddd)`
    IntegerOfDay = 44,
    /// `FUNCTION DAY-OF-INTEGER(lilian)`
    DayOfInteger = 45,
    /// `FUNCTION DATE-TO-YYYYMMDD(yymmdd [, offset [, year]])`
    DateToYyyymmdd = 46,
    /// `FUNCTION DAY-TO-YYYYDDD(yyddd [, offset [, year]])`
    DayToYyyyddd = 47,
    /// `FUNCTION YEAR-TO-YYYY(yy [, offset [, year]])`
    YearToYyyy = 48,
    /// `FUNCTION COMBINED-DATETIME(lilian, seconds)`
    CombinedDatetime = 49,
    /// `FUNCTION FORMATTED-CURRENT-DATE(fmt)`
    FormattedCurrentDate = 50,
    /// `FUNCTION FORMATTED-DATE(fmt, lilian)`
    FormattedDate = 51,
    /// `FUNCTION FORMATTED-DATETIME(fmt, lilian, seconds [, offset [, sys_offset]])`
    FormattedDatetime = 52,
    /// `FUNCTION FORMATTED-TIME(fmt, seconds [, offset [, sys_offset]])`
    FormattedTime = 53,
    /// `FUNCTION INTEGER-OF-FORMATTED-DATE(fmt, str)`
    IntegerOfFormattedDate = 54,
    /// `FUNCTION SECONDS-FROM-FORMATTED-TIME(fmt, str)`
    SecondsFromFormattedTime = 55,
    /// `FUNCTION TEST-DATE-YYYYMMDD(yyyymmdd)`
    TestDateYyyymmdd = 56,
    /// `FUNCTION TEST-DAY-YYYYDDD(yyyyddd)`
    TestDayYyyyddd = 57,
    /// `FUNCTION TEST-FORMATTED-DATETIME(fmt, str)`
    TestFormattedDatetime = 58,
    /// `FUNCTION LOCALE-DATE(yyyymmdd [, locale])`
    LocaleDate = 59,
    /// `FUNCTION LOCALE-TIME(hhmmss [, locale])`
    LocaleTime = 60,
    /// `FUNCTION LOCALE-TIME-FROM-SECONDS(seconds [, locale])`
    LocaleTimeFromSeconds = 61,
    /// `FUNCTION BYTE-LENGTH(item)`
    ByteLength = 62,
    /// `FUNCTION LENGTH(item)`
    Length = 63,
    /// `FUNCTION LENGTH-AN(item)`
    LengthAn = 64,
    /// `FUNCTION CHAR(ordinal)`
    Char = 65,
    /// `FUNCTION ORD(ch)`
    Ord = 66,
    /// `FUNCTION CONCAT(arg...)` / `FUNCTION CONCATENATE(arg...)`
    Concatenate = 67,
    /// `FUNCTION REVERSE(str)`
    Reverse = 68,
    /// `FUNCTION UPPER-CASE(str)`
    UpperCase = 69,
    /// `FUNCTION LOWER-CASE(str)`
    LowerCase = 70,
    /// `FUNCTION TRIM(str [, LEADING|TRAILING])`
    Trim = 71,
    /// `FUNCTION SUBSTITUTE(str, from, to ...)`
    Substitute = 72,
    /// `FUNCTION SUBSTITUTE-CASE(str, from, to ...)`
    SubstituteCase = 73,
    /// `FUNCTION FIND-STRING(str, pat ...)`
    FindString = 74,
    /// `FUNCTION NUMVAL(str)`
    Numval = 75,
    /// `FUNCTION NUMVAL-C(str [, currency [, ANYCASE]])`
    NumvalC = 76,
    /// `FUNCTION NUMVAL-F(str)`
    NumvalF = 77,
    /// `FUNCTION TEST-NUMVAL(str)`
    TestNumval = 78,
    /// `FUNCTION TEST-NUMVAL-C(str [, currency [, ANYCASE]])`
    TestNumvalC = 79,
    /// `FUNCTION TEST-NUMVAL-F(str)`
    TestNumvalF = 80,
    /// `FUNCTION BASECONVERT(str, src_base, dst_base)`
    BaseConvert = 81,
    /// `FUNCTION BOOLEAN-OF-INTEGER(int, len)`
    BooleanOfInteger = 82,
    /// `FUNCTION INTEGER-OF-BOOLEAN(bool_str [, signed])`
    IntegerOfBoolean = 83,
    /// `FUNCTION BIT-OF(bytes)`
    BitOf = 84,
    /// `FUNCTION BIT-TO-CHAR(bit_str)`
    BitToChar = 85,
    /// `FUNCTION HEX-OF(bytes)`
    HexOf = 86,
    /// `FUNCTION HEX-TO-CHAR(hex_str)`
    HexToChar = 87,
    /// `FUNCTION CONVERT(src, from_spec, to_spec)`
    Convert = 88,
    /// `FUNCTION STANDARD-COMPARE(a, b [, coll [, level]])`
    StandardCompare = 89,
    /// `FUNCTION LOCALE-COMPARE(a, b [, locale])`
    LocaleCompare = 90,
    /// `FUNCTION STORED-CHAR-LENGTH(str)`
    StoredCharLength = 91,
    /// `FUNCTION CHAR-NATIONAL(ordinal)`
    CharNational = 92,
    /// `FUNCTION DISPLAY-OF(nat [, replacement])`
    DisplayOf = 93,
    /// `FUNCTION NATIONAL-OF(str [, replacement])`
    NationalOf = 94,
    /// `FUNCTION ULENGTH(str)`
    Ulength = 95,
    /// `FUNCTION UPOS(str, n)`
    Upos = 96,
    /// `FUNCTION USUBSTR(str, start, count)`
    Usubstr = 97,
    /// `FUNCTION USUPPLEMENTARY(str)`
    Usupplementary = 98,
    /// `FUNCTION UVALID(str)`
    Uvalid = 99,
    /// `FUNCTION UWIDTH(str, n)`
    Uwidth = 100,
    /// `FUNCTION CONTENT-OF(ptr)`
    ContentOf = 101,
    /// `FUNCTION CURRENCY-SYMBOL`
    CurrencySymbol = 102,
    /// `FUNCTION MODULE-CALLER-ID`
    ModuleCallerId = 103,
    /// `FUNCTION MODULE-DATE`
    ModuleDate = 104,
    /// `FUNCTION MODULE-FORMATTED-DATE`
    ModuleFormattedDate = 105,
    /// `FUNCTION MODULE-ID` / `FUNCTION MODULE-NAME`
    ModuleId = 106,
    /// `FUNCTION MODULE-PATH`
    ModulePath = 107,
    /// `FUNCTION MODULE-SOURCE`
    ModuleSource = 108,
    /// `FUNCTION MODULE-TIME`
    ModuleTime = 109,
    /// `FUNCTION EXCEPTION-FILE`
    ExceptionFile = 110,
    /// `FUNCTION EXCEPTION-FILE-N`
    ExceptionFileN = 111,
    /// `FUNCTION EXCEPTION-LOCATION`
    ExceptionLocation = 112,
    /// `FUNCTION EXCEPTION-LOCATION-N`
    ExceptionLocationN = 113,
    /// `FUNCTION EXCEPTION-STATEMENT`
    ExceptionStatement = 114,
    /// `FUNCTION EXCEPTION-STATUS`
    ExceptionStatus = 115,
    /// `FUNCTION UUID4`
    Uuid4 = 116,
}

/// Ordered metadata table entry pairing an [`IntrinsicId`] with its canonical name, arity, and return kind.
struct IntrinsicMetadata {
    /// Canonical intrinsic identifier.
    id: IntrinsicId,
    /// Canonical uppercase COBOL intrinsic name.
    name: &'static str,
    /// `(min_args, max_args)` arity bounds.
    arity: (usize, usize),
    /// Return value category.
    return_kind: IntrinsicReturnKind,
}

/// Master metadata table of all 116 intrinsics indexed by `(abi_code - 1)`.
const INTRINSIC_METADATA_TABLE: [IntrinsicMetadata; 116] = [
    IntrinsicMetadata {
        id: IntrinsicId::Abs,
        name: "ABS",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Acos,
        name: "ACOS",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Asin,
        name: "ASIN",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Atan,
        name: "ATAN",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Cos,
        name: "COS",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::E,
        name: "E",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Exp,
        name: "EXP",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Exp10,
        name: "EXP10",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Factorial,
        name: "FACTORIAL",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::FractionPart,
        name: "FRACTION-PART",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::HighestAlgebraic,
        name: "HIGHEST-ALGEBRAIC",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Integer,
        name: "INTEGER",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::IntegerPart,
        name: "INTEGER-PART",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Log,
        name: "LOG",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Log10,
        name: "LOG10",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::LowestAlgebraic,
        name: "LOWEST-ALGEBRAIC",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Mod,
        name: "MOD",
        arity: (2, 2),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Pi,
        name: "PI",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Rem,
        name: "REM",
        arity: (2, 2),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Sign,
        name: "SIGN",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Sin,
        name: "SIN",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::SmallestAlgebraic,
        name: "SMALLEST-ALGEBRAIC",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Sqrt,
        name: "SQRT",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Tan,
        name: "TAN",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Annuity,
        name: "ANNUITY",
        arity: (2, 2),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::PresentValue,
        name: "PRESENT-VALUE",
        arity: (2, usize::MAX),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Max,
        name: "MAX",
        arity: (1, usize::MAX),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Min,
        name: "MIN",
        arity: (1, usize::MAX),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::OrdMax,
        name: "ORD-MAX",
        arity: (1, usize::MAX),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::OrdMin,
        name: "ORD-MIN",
        arity: (1, usize::MAX),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Sum,
        name: "SUM",
        arity: (1, usize::MAX),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Mean,
        name: "MEAN",
        arity: (1, usize::MAX),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Median,
        name: "MEDIAN",
        arity: (1, usize::MAX),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Midrange,
        name: "MIDRANGE",
        arity: (1, usize::MAX),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Range,
        name: "RANGE",
        arity: (1, usize::MAX),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Variance,
        name: "VARIANCE",
        arity: (1, usize::MAX),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::StandardDeviation,
        name: "STANDARD-DEVIATION",
        arity: (1, usize::MAX),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Random,
        name: "RANDOM",
        arity: (0, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::CurrentDate,
        name: "CURRENT-DATE",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::WhenCompiled,
        name: "WHEN-COMPILED",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::SecondsPastMidnight,
        name: "SECONDS-PAST-MIDNIGHT",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::IntegerOfDate,
        name: "INTEGER-OF-DATE",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::DateOfInteger,
        name: "DATE-OF-INTEGER",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::IntegerOfDay,
        name: "INTEGER-OF-DAY",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::DayOfInteger,
        name: "DAY-OF-INTEGER",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::DateToYyyymmdd,
        name: "DATE-TO-YYYYMMDD",
        arity: (1, 3),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::DayToYyyyddd,
        name: "DAY-TO-YYYYDDD",
        arity: (1, 3),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::YearToYyyy,
        name: "YEAR-TO-YYYY",
        arity: (1, 3),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::CombinedDatetime,
        name: "COMBINED-DATETIME",
        arity: (2, 2),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::FormattedCurrentDate,
        name: "FORMATTED-CURRENT-DATE",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::FormattedDate,
        name: "FORMATTED-DATE",
        arity: (2, 2),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::FormattedDatetime,
        name: "FORMATTED-DATETIME",
        arity: (4, 5),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::FormattedTime,
        name: "FORMATTED-TIME",
        arity: (2, 4),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::IntegerOfFormattedDate,
        name: "INTEGER-OF-FORMATTED-DATE",
        arity: (2, 2),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::SecondsFromFormattedTime,
        name: "SECONDS-FROM-FORMATTED-TIME",
        arity: (2, 2),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::TestDateYyyymmdd,
        name: "TEST-DATE-YYYYMMDD",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::TestDayYyyyddd,
        name: "TEST-DAY-YYYYDDD",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::TestFormattedDatetime,
        name: "TEST-FORMATTED-DATETIME",
        arity: (2, 2),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::LocaleDate,
        name: "LOCALE-DATE",
        arity: (1, 2),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::LocaleTime,
        name: "LOCALE-TIME",
        arity: (1, 2),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::LocaleTimeFromSeconds,
        name: "LOCALE-TIME-FROM-SECONDS",
        arity: (1, 2),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ByteLength,
        name: "BYTE-LENGTH",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Length,
        name: "LENGTH",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::LengthAn,
        name: "LENGTH-AN",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Char,
        name: "CHAR",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Ord,
        name: "ORD",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Concatenate,
        name: "CONCATENATE",
        arity: (1, usize::MAX),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Reverse,
        name: "REVERSE",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::UpperCase,
        name: "UPPER-CASE",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::LowerCase,
        name: "LOWER-CASE",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Trim,
        name: "TRIM",
        arity: (1, 2),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Substitute,
        name: "SUBSTITUTE",
        arity: (3, usize::MAX),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::SubstituteCase,
        name: "SUBSTITUTE-CASE",
        arity: (3, usize::MAX),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::FindString,
        name: "FIND-STRING",
        arity: (2, 5),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Numval,
        name: "NUMVAL",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::NumvalC,
        name: "NUMVAL-C",
        arity: (1, 3),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::NumvalF,
        name: "NUMVAL-F",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Numeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::TestNumval,
        name: "TEST-NUMVAL",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::TestNumvalC,
        name: "TEST-NUMVAL-C",
        arity: (1, 3),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::TestNumvalF,
        name: "TEST-NUMVAL-F",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::BaseConvert,
        name: "BASECONVERT",
        arity: (3, 3),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::BooleanOfInteger,
        name: "BOOLEAN-OF-INTEGER",
        arity: (2, 2),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::IntegerOfBoolean,
        name: "INTEGER-OF-BOOLEAN",
        arity: (1, 2),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::BitOf,
        name: "BIT-OF",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::BitToChar,
        name: "BIT-TO-CHAR",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::HexOf,
        name: "HEX-OF",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::HexToChar,
        name: "HEX-TO-CHAR",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Convert,
        name: "CONVERT",
        arity: (3, 3),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::StandardCompare,
        name: "STANDARD-COMPARE",
        arity: (2, 4),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::LocaleCompare,
        name: "LOCALE-COMPARE",
        arity: (2, 3),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::StoredCharLength,
        name: "STORED-CHAR-LENGTH",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::CharNational,
        name: "CHAR-NATIONAL",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::National,
    },
    IntrinsicMetadata {
        id: IntrinsicId::DisplayOf,
        name: "DISPLAY-OF",
        arity: (1, 2),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::NationalOf,
        name: "NATIONAL-OF",
        arity: (1, 2),
        return_kind: IntrinsicReturnKind::National,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Ulength,
        name: "ULENGTH",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Upos,
        name: "UPOS",
        arity: (2, 2),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Usubstr,
        name: "USUBSTR",
        arity: (3, 3),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Usupplementary,
        name: "USUPPLEMENTARY",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Uvalid,
        name: "UVALID",
        arity: (1, 1),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Uwidth,
        name: "UWIDTH",
        arity: (2, 2),
        return_kind: IntrinsicReturnKind::Integer,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ContentOf,
        name: "CONTENT-OF",
        arity: (1, 2),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::CurrencySymbol,
        name: "CURRENCY-SYMBOL",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ModuleCallerId,
        name: "MODULE-CALLER-ID",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ModuleDate,
        name: "MODULE-DATE",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ModuleFormattedDate,
        name: "MODULE-FORMATTED-DATE",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ModuleId,
        name: "MODULE-ID",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ModulePath,
        name: "MODULE-PATH",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ModuleSource,
        name: "MODULE-SOURCE",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ModuleTime,
        name: "MODULE-TIME",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ExceptionFile,
        name: "EXCEPTION-FILE",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ExceptionFileN,
        name: "EXCEPTION-FILE-N",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::National,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ExceptionLocation,
        name: "EXCEPTION-LOCATION",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ExceptionLocationN,
        name: "EXCEPTION-LOCATION-N",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::National,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ExceptionStatement,
        name: "EXCEPTION-STATEMENT",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::ExceptionStatus,
        name: "EXCEPTION-STATUS",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
    IntrinsicMetadata {
        id: IntrinsicId::Uuid4,
        name: "UUID4",
        arity: (0, 0),
        return_kind: IntrinsicReturnKind::Alphanumeric,
    },
];

impl IntrinsicId {
    /// Complete ordered table of all 116 canonical [`IntrinsicId`] variants.
    pub const ALL: &'static [Self] = &[
        Self::Abs,
        Self::Acos,
        Self::Asin,
        Self::Atan,
        Self::Cos,
        Self::E,
        Self::Exp,
        Self::Exp10,
        Self::Factorial,
        Self::FractionPart,
        Self::HighestAlgebraic,
        Self::Integer,
        Self::IntegerPart,
        Self::Log,
        Self::Log10,
        Self::LowestAlgebraic,
        Self::Mod,
        Self::Pi,
        Self::Rem,
        Self::Sign,
        Self::Sin,
        Self::SmallestAlgebraic,
        Self::Sqrt,
        Self::Tan,
        Self::Annuity,
        Self::PresentValue,
        Self::Max,
        Self::Min,
        Self::OrdMax,
        Self::OrdMin,
        Self::Sum,
        Self::Mean,
        Self::Median,
        Self::Midrange,
        Self::Range,
        Self::Variance,
        Self::StandardDeviation,
        Self::Random,
        Self::CurrentDate,
        Self::WhenCompiled,
        Self::SecondsPastMidnight,
        Self::IntegerOfDate,
        Self::DateOfInteger,
        Self::IntegerOfDay,
        Self::DayOfInteger,
        Self::DateToYyyymmdd,
        Self::DayToYyyyddd,
        Self::YearToYyyy,
        Self::CombinedDatetime,
        Self::FormattedCurrentDate,
        Self::FormattedDate,
        Self::FormattedDatetime,
        Self::FormattedTime,
        Self::IntegerOfFormattedDate,
        Self::SecondsFromFormattedTime,
        Self::TestDateYyyymmdd,
        Self::TestDayYyyyddd,
        Self::TestFormattedDatetime,
        Self::LocaleDate,
        Self::LocaleTime,
        Self::LocaleTimeFromSeconds,
        Self::ByteLength,
        Self::Length,
        Self::LengthAn,
        Self::Char,
        Self::Ord,
        Self::Concatenate,
        Self::Reverse,
        Self::UpperCase,
        Self::LowerCase,
        Self::Trim,
        Self::Substitute,
        Self::SubstituteCase,
        Self::FindString,
        Self::Numval,
        Self::NumvalC,
        Self::NumvalF,
        Self::TestNumval,
        Self::TestNumvalC,
        Self::TestNumvalF,
        Self::BaseConvert,
        Self::BooleanOfInteger,
        Self::IntegerOfBoolean,
        Self::BitOf,
        Self::BitToChar,
        Self::HexOf,
        Self::HexToChar,
        Self::Convert,
        Self::StandardCompare,
        Self::LocaleCompare,
        Self::StoredCharLength,
        Self::CharNational,
        Self::DisplayOf,
        Self::NationalOf,
        Self::Ulength,
        Self::Upos,
        Self::Usubstr,
        Self::Usupplementary,
        Self::Uvalid,
        Self::Uwidth,
        Self::ContentOf,
        Self::CurrencySymbol,
        Self::ModuleCallerId,
        Self::ModuleDate,
        Self::ModuleFormattedDate,
        Self::ModuleId,
        Self::ModulePath,
        Self::ModuleSource,
        Self::ModuleTime,
        Self::ExceptionFile,
        Self::ExceptionFileN,
        Self::ExceptionLocation,
        Self::ExceptionLocationN,
        Self::ExceptionStatement,
        Self::ExceptionStatus,
        Self::Uuid4,
    ];

    /// Resolves a case-insensitive COBOL intrinsic function name (including `CONCAT` and `MODULE-NAME` aliases).
    ///
    /// # Arguments
    ///
    /// * `name` - Candidate COBOL intrinsic name.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `name` is not a recognized intrinsic function.
    pub fn from_cobol_name(name: &str) -> Result<Self> {
        let upper = name.trim().to_ascii_uppercase();
        if upper == "CONCAT" {
            return Ok(Self::Concatenate);
        }
        if upper == "MODULE-NAME" {
            return Ok(Self::ModuleId);
        }
        for entry in &INTRINSIC_METADATA_TABLE {
            if entry.name == upper {
                return Ok(entry.id);
            }
        }
        Err(CoblangError::InvalidDomainValue {
            domain_type: "IntrinsicId",
            value: String::from(name),
            reason: String::from("unknown COBOL intrinsic function name"),
        })
    }

    /// Decodes a stable `u16` ABI discriminant (`1..=116`) into [`IntrinsicId`].
    ///
    /// # Arguments
    ///
    /// * `code` - Raw 1-based intrinsic ABI discriminant.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `code` is outside `1..=116`.
    pub fn from_abi_code(code: u16) -> Result<Self> {
        let idx = usize::from(code).wrapping_sub(1);
        INTRINSIC_METADATA_TABLE
            .get(idx)
            .map(|m| m.id)
            .ok_or_else(|| CoblangError::InvalidDomainValue {
                domain_type: "IntrinsicId",
                value: code.to_string(),
                reason: String::from("intrinsic ABI code must be in 1..=116"),
            })
    }

    /// Encodes `self` into its stable `u16` ABI discriminant (`1..=116`).
    #[must_use]
    pub const fn to_abi_code(self) -> u16 {
        self as u16
    }

    /// Looks up the static [`IntrinsicMetadata`] record for `self`.
    #[must_use]
    fn metadata(self) -> &'static IntrinsicMetadata {
        /// Static fallback entry satisfying `clippy::indexing_slicing` without panicking.
        const FALLBACK: IntrinsicMetadata = IntrinsicMetadata {
            id: IntrinsicId::Abs,
            name: "ABS",
            arity: (1, 1),
            return_kind: IntrinsicReturnKind::Numeric,
        };
        let idx = usize::from(self as u16) - 1;
        INTRINSIC_METADATA_TABLE.get(idx).unwrap_or(&FALLBACK)
    }

    /// Returns the canonical uppercase ISO/GnuCOBOL name of this intrinsic.
    #[must_use]
    pub fn canonical_name(self) -> &'static str {
        self.metadata().name
    }

    /// Returns the logical [`IntrinsicCategory`] owning this intrinsic.
    #[must_use]
    pub const fn category(self) -> IntrinsicCategory {
        match self as u16 {
            1..=24 => IntrinsicCategory::Math,
            25..=38 => IntrinsicCategory::Stats,
            39..=61 => IntrinsicCategory::DateTime,
            62..=91 => IntrinsicCategory::Strings,
            _ => IntrinsicCategory::MetaUnicode,
        }
    }

    /// Returns `(min_args, max_args)` arity bounds for this intrinsic.
    #[must_use]
    pub fn arity_bounds(self) -> (usize, usize) {
        self.metadata().arity
    }

    /// Returns the canonical [`IntrinsicReturnKind`] of this intrinsic.
    #[must_use]
    pub fn return_kind(self) -> IntrinsicReturnKind {
        self.metadata().return_kind
    }
}
