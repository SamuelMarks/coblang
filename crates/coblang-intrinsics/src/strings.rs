//! String, numeric parsing (`NUMVAL*`), base/bit/hex/format conversion, and collation intrinsics (`Milestone M12`).

use core::cmp::Ordering;

use coblang_common::Result;
use coblang_decimal::{DecimalSign, FixedDecimal, RoundingMode, SignedInt256, Uint256};
use coblang_pic::DecimalPointKind;

use crate::ec_argument_function;

/// Maximum bit width accepted by `BOOLEAN-OF-INTEGER` and `INTEGER-OF-BOOLEAN` (`256` bits).
pub const MAX_BOOLEAN_BIT_WIDTH: usize = 256;

/// Character-counted item descriptor for `FUNCTION LENGTH`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasuredItem<'a> {
    /// Single-byte alphanumeric/alphabetic item (`1` character per byte).
    Alphanumeric(&'a [u8]),
    /// `USAGE NATIONAL` UTF-16 item (`1` national character unit per `2` bytes).
    NationalUtf16(&'a [u8]),
    /// `UTF-8` string item counted by Unicode scalar values.
    Utf8(&'a str),
}

/// Active single-byte collating sequence for `CHAR`, `ORD`, and `STANDARD-COMPARE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollatingTable<'a> {
    /// Native ASCII / ISO-646 identity ordering (`0x00..=0xFF` -> ordinals `1..=256`).
    NativeAscii,
    /// Custom 256-entry alphabet mapping `0`-based ordinal index to its character byte.
    Custom(&'a [u8; 256]),
}

/// Trim side selector for `FUNCTION TRIM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrimDirection {
    /// Strip both leading and trailing pad characters (default).
    Both,
    /// Strip leading pad characters only (`LEADING`).
    Leading,
    /// Strip trailing pad characters only (`TRAILING`).
    Trailing,
}

/// Case-matching policy for `FIND-STRING`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaseSensitivity {
    /// Exact byte/character match.
    Sensitive,
    /// Case-insensitive match (`INSENSITIVE`).
    Insensitive,
}

/// Match-selection mode for ISO 2023 `FUNCTION FIND-STRING`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FindStringMode {
    /// First non-overlapping substring occurrence (`FIRST` / default).
    First,
    /// Last substring occurrence (`LAST`).
    Last,
    /// Specific `1`-based non-overlapping occurrence count (`>= 1`).
    Occurrence(usize),
    /// First position matching any single character in pattern (`ANY`).
    AnyChar,
    /// Count of all non-overlapping substring occurrences (`ALL`).
    AllCount,
}

/// Strongly-typed configuration for `FUNCTION FIND-STRING`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FindStringSpec {
    /// Substring / character match mode (`FIRST`, `LAST`, `ANY`, `ALL`, or `n`-th occurrence).
    pub mode: FindStringMode,
    /// Case-matching mode (`Sensitive` vs `Insensitive`).
    pub case_mode: CaseSensitivity,
    /// `AFTER` 1-based byte offset (`0` searches from the start of the subject).
    pub after_pos: usize,
}

impl FindStringSpec {
    /// Constructs a default [`FindStringSpec`] (`First`, `Sensitive`, `after_pos = 0`).
    #[must_use]
    pub const fn first_sensitive() -> Self {
        Self {
            mode: FindStringMode::First,
            case_mode: CaseSensitivity::Sensitive,
            after_pos: 0,
        }
    }
}

/// Case policy for currency and `CR`/`DB` matching in `NUMVAL-C` and `TEST-NUMVAL-C`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CurrencyCasePolicy {
    /// Exact case match required.
    ExactCase,
    /// Case-insensitive currency and `CR`/`DB` match (`ANYCASE`).
    AnyCase,
}

/// Validated radix (`2..=36`) for ISO 2023 `FUNCTION BASECONVERT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NumericBase(u8);

impl NumericBase {
    /// Validates that `radix` lies in `2..=36`.
    ///
    /// # Arguments
    ///
    /// * `radix` - Candidate integer base (`2..=36`).
    ///
    /// # Errors
    ///
    /// Returns `EC-ARGUMENT-FUNCTION` if `radix < 2` or `radix > 36`.
    pub fn try_new(radix: u8) -> Result<Self> {
        if !(2..=36).contains(&radix) {
            return Err(ec_argument_function(
                "BASECONVERT",
                radix.to_string(),
                "numeric base must lie in 2..=36",
            ));
        }
        Ok(Self(radix))
    }

    /// Returns the underlying integer radix (`2..=36`).
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Supported data representation formats for ISO 2023 `FUNCTION CONVERT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConvertFormat {
    /// Raw alphanumeric / byte representation (`ANUM` / `BYTE`).
    Alphanumeric,
    /// Uppercase hexadecimal text (`HEX` / `HEXADECIMAL`).
    HexUpper,
    /// Lowercase hexadecimal text (`HEX-LOWER`).
    HexLower,
    /// RFC 4648 `Base64` text (`BASE64`).
    Base64,
    /// ASCII `'0'`/`'1'` bit-character representation (`BIT`).
    BitString,
}

/// Operation selector for `FUNCTION CONVERT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertSpec<'a> {
    /// ISO 2023 representation conversion between `source` and `target` formats.
    Format {
        /// Input encoding format of argument-1.
        source: ConvertFormat,
        /// Desired output encoding format.
        target: ConvertFormat,
    },
    /// 1-to-1 character translation (`from_chars.len() == to_chars.len()`).
    Alphabet {
        /// Source character set.
        from_chars: &'a [u8],
        /// Replacement character set aligned by index.
        to_chars: &'a [u8],
    },
}

/// Canonical 1-character ordering outcome returned by `STANDARD-COMPARE` and `LOCALE-COMPARE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareIndicator {
    /// Left operand precedes right operand (`"<"`).
    Less,
    /// Operands compare equal (`"="`).
    Equal,
    /// Left operand follows right operand (`">"`).
    Greater,
}

impl CompareIndicator {
    /// Returns the single ASCII byte (`b'<'`, `b'='`, or `b'>'`) representing `self`.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Less => b'<',
            Self::Equal => b'=',
            Self::Greater => b'>',
        }
    }

    /// Returns the 1-character `&'static str` (`"<"`, `"="`, or `">"`) representing `self`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Less => "<",
            Self::Equal => "=",
            Self::Greater => ">",
        }
    }
}

/// Evaluates `FUNCTION BYTE-LENGTH(bytes)` (physical storage byte count).
///
/// # Arguments
///
/// * `bytes` - Raw field byte slice.
#[must_use]
pub const fn str_byte_length(bytes: &[u8]) -> usize {
    bytes.len()
}

/// Evaluates `FUNCTION LENGTH(item)` (logical character count).
///
/// # Arguments
///
/// * `item` - Categorized [`MeasuredItem`].
#[must_use]
pub fn str_length(item: MeasuredItem<'_>) -> usize {
    match item {
        MeasuredItem::Alphanumeric(b) => b.len(),
        MeasuredItem::NationalUtf16(b) => b.len() / 2,
        MeasuredItem::Utf8(s) => s.chars().count(),
    }
}

/// Evaluates `GnuCOBOL` `FUNCTION LENGTH-AN(bytes)` (alphanumeric byte length).
///
/// # Arguments
///
/// * `bytes` - Raw field byte slice.
#[must_use]
pub const fn str_length_an(bytes: &[u8]) -> usize {
    bytes.len()
}

/// Evaluates `FUNCTION CHAR(ordinal)` (`1`-based ordinal `1..=256` -> single character byte).
///
/// # Arguments
///
/// * `ordinal` - `1`-based character position in `collation` (`1..=256`).
/// * `collation` - Active [`CollatingTable`].
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `ordinal` lies outside `1..=256`.
pub fn str_char(ordinal: i64, collation: CollatingTable<'_>) -> Result<u8> {
    if !(1..=256).contains(&ordinal) {
        return Err(ec_argument_function(
            "CHAR",
            ordinal.to_string(),
            "ordinal position must lie in 1..=256",
        ));
    }
    let idx = usize::try_from(ordinal - 1).unwrap_or(0);
    Ok(match collation {
        CollatingTable::NativeAscii => u8::try_from(idx).unwrap_or(0),
        CollatingTable::Custom(tbl) => tbl.get(idx).copied().unwrap_or(0),
    })
}

/// Evaluates `FUNCTION ORD(ch_bytes)` (single character -> `1`-based ordinal `1..=256`).
///
/// # Arguments
///
/// * `ch_bytes` - Single-byte character operand (`len == 1`).
/// * `collation` - Active [`CollatingTable`].
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `ch_bytes.len() != 1` or the byte is absent from `collation`.
pub fn str_ord(ch_bytes: &[u8], collation: CollatingTable<'_>) -> Result<u16> {
    let [byte] = ch_bytes else {
        return Err(ec_argument_function(
            "ORD",
            format!("len={}", ch_bytes.len()),
            "ORD requires a 1-character argument",
        ));
    };
    match collation {
        CollatingTable::NativeAscii => Ok(u16::from(*byte) + 1),
        CollatingTable::Custom(tbl) => {
            for (i, &entry) in tbl.iter().enumerate() {
                if entry == *byte {
                    return Ok(u16::try_from(i + 1).unwrap_or(1));
                }
            }
            Err(ec_argument_function(
                "ORD",
                byte.to_string(),
                "character not present in custom collating table",
            ))
        }
    }
}

/// Evaluates `FUNCTION CONCAT(pieces)` (alias of `CONCATENATE`).
///
/// # Arguments
///
/// * `pieces` - Ordered byte slices to concatenate.
#[must_use]
pub fn str_concat(pieces: &[&[u8]]) -> Vec<u8> {
    str_concatenate(pieces)
}

/// Evaluates `FUNCTION CONCATENATE(pieces)`.
///
/// # Arguments
///
/// * `pieces` - Ordered byte slices to concatenate.
#[must_use]
pub fn str_concatenate(pieces: &[&[u8]]) -> Vec<u8> {
    let cap: usize = pieces.iter().map(|p| p.len()).sum();
    let mut out = Vec::with_capacity(cap);
    for piece in pieces {
        out.extend_from_slice(piece);
    }
    out
}

/// Evaluates `FUNCTION REVERSE(input)` (reversing UTF-8 scalar order when valid UTF-8, else bytewise).
///
/// # Arguments
///
/// * `input` - Source character bytes.
#[must_use]
pub fn str_reverse(input: &[u8]) -> Vec<u8> {
    if let Ok(s) = core::str::from_utf8(input) {
        return s.chars().rev().collect::<String>().into_bytes();
    }
    input.iter().copied().rev().collect()
}

/// Evaluates `FUNCTION UPPER-CASE(input)` (Unicode uppercase when valid UTF-8, else ASCII uppercase).
///
/// # Arguments
///
/// * `input` - Source character bytes.
#[must_use]
pub fn str_upper_case(input: &[u8]) -> Vec<u8> {
    if let Ok(s) = core::str::from_utf8(input) {
        return s.to_uppercase().into_bytes();
    }
    input.to_ascii_uppercase()
}

/// Evaluates `FUNCTION LOWER-CASE(input)` (Unicode lowercase when valid UTF-8, else ASCII lowercase).
///
/// # Arguments
///
/// * `input` - Source character bytes.
#[must_use]
pub fn str_lower_case(input: &[u8]) -> Vec<u8> {
    if let Ok(s) = core::str::from_utf8(input) {
        return s.to_lowercase().into_bytes();
    }
    input.to_ascii_lowercase()
}

/// Evaluates `FUNCTION TRIM(input, direction, pad_byte)`.
///
/// # Arguments
///
/// * `input` - Source byte slice.
/// * `direction` - [`TrimDirection`] (`Both`, `Leading`, or `Trailing`).
/// * `pad_byte` - Optional trim character (defaults to ASCII space `0x20`).
#[must_use]
pub fn str_trim(input: &[u8], direction: TrimDirection, pad_byte: Option<u8>) -> Vec<u8> {
    let pad = pad_byte.unwrap_or(b' ');
    let start = match direction {
        TrimDirection::Both | TrimDirection::Leading => {
            input.iter().take_while(|&&b| b == pad).count()
        }
        TrimDirection::Trailing => 0,
    };
    let after_lead = input.get(start..).unwrap_or(&[]);
    let keep_len = match direction {
        TrimDirection::Both | TrimDirection::Trailing => {
            let suffix_count = after_lead.iter().rev().take_while(|&&b| b == pad).count();
            after_lead.len() - suffix_count
        }
        TrimDirection::Leading => after_lead.len(),
    };
    after_lead.get(..keep_len).unwrap_or(&[]).to_vec()
}

/// Evaluates `FUNCTION SUBSTITUTE(subject, pairs)` (single-pass simultaneous left-to-right replacement).
///
/// # Arguments
///
/// * `subject` - Input byte slice.
/// * `pairs` - Non-empty `(search, replacement)` byte-slice pairs.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `pairs` is empty or any `search` pattern is empty.
pub fn str_substitute(subject: &[u8], pairs: &[(&[u8], &[u8])]) -> Result<Vec<u8>> {
    substitute_impl("SUBSTITUTE", subject, pairs, CaseSensitivity::Sensitive)
}

/// Evaluates `FUNCTION SUBSTITUTE-CASE(subject, pairs)` (case-insensitive simultaneous replacement).
///
/// # Arguments
///
/// * `subject` - Input byte slice.
/// * `pairs` - Non-empty `(search, replacement)` byte-slice pairs.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `pairs` is empty or any `search` pattern is empty.
pub fn str_substitute_case(subject: &[u8], pairs: &[(&[u8], &[u8])]) -> Result<Vec<u8>> {
    substitute_impl(
        "SUBSTITUTE-CASE",
        subject,
        pairs,
        CaseSensitivity::Insensitive,
    )
}

/// Evaluates ISO 2023 `FUNCTION FIND-STRING(subject, pattern, spec)`.
///
/// # Arguments
///
/// * `subject` - Haystack byte slice.
/// * `pattern` - Non-empty needle byte slice.
/// * `spec` - [`FindStringSpec`] (`FIRST`, `LAST`, `Occurrence`, `ANY`, `ALL`, `INSENSITIVE`, `AFTER`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `pattern` is empty, `after_pos > subject.len()`, or `Occurrence(0)`.
pub fn str_find_string(subject: &[u8], pattern: &[u8], spec: FindStringSpec) -> Result<usize> {
    if pattern.is_empty() {
        return Err(ec_argument_function(
            "FIND-STRING",
            "empty-pattern",
            "search pattern must not be empty",
        ));
    }
    if spec.after_pos > subject.len() {
        return Err(ec_argument_function(
            "FIND-STRING",
            spec.after_pos.to_string(),
            "AFTER position exceeds subject length",
        ));
    }
    if spec.mode == FindStringMode::Occurrence(0) {
        return Err(ec_argument_function(
            "FIND-STRING",
            "0",
            "occurrence index must be >= 1",
        ));
    }
    Ok(find_string_checked(subject, pattern, spec))
}

/// Evaluates `FUNCTION TEST-NUMVAL(input, dec_point)`.
///
/// Returns `0` when valid, or the `1`-based character index (`1..=input.len() + 1`) of the first flaw.
///
/// # Arguments
///
/// * `input` - Candidate numeric string bytes.
/// * `dec_point` - Active [`DecimalPointKind`] (`Period` vs `Comma`).
#[must_use]
pub fn str_test_numval(input: &[u8], dec_point: DecimalPointKind) -> usize {
    match scan_numval_plain(input, dec_point) {
        Ok(_) => 0,
        Err(bad_pos) => bad_pos,
    }
}

/// Evaluates `FUNCTION NUMVAL(input, dec_point)`.
///
/// # Arguments
///
/// * `input` - Candidate numeric string bytes.
/// * `dec_point` - Active [`DecimalPointKind`] (`Period` vs `Comma`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `str_test_numval(input, dec_point) != 0`.
pub fn str_numval(input: &[u8], dec_point: DecimalPointKind) -> Result<FixedDecimal> {
    scan_numval_plain(input, dec_point).map_err(|pos| {
        ec_argument_function(
            "NUMVAL",
            String::from_utf8_lossy(input).into_owned(),
            format!("invalid numeric character at position {pos}"),
        )
    })
}

/// Evaluates `FUNCTION TEST-NUMVAL-C(input, currency, case_policy, dec_point)`.
///
/// Returns `0` when valid, or the `1`-based offending position (`1..=input.len() + 1`).
///
/// # Arguments
///
/// * `input` - Candidate monetary numeric string bytes.
/// * `currency` - Optional currency string (defaults to `b"$"`).
/// * `case_policy` - [`CurrencyCasePolicy`] (`ExactCase` vs `AnyCase`).
/// * `dec_point` - Active [`DecimalPointKind`].
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `currency` itself is empty or contains forbidden numeric characters.
pub fn str_test_numval_c(
    input: &[u8],
    currency: Option<&[u8]>,
    case_policy: CurrencyCasePolicy,
    dec_point: DecimalPointKind,
) -> Result<usize> {
    let cs = validate_currency_arg("TEST-NUMVAL-C", currency)?;
    Ok(
        match scan_numval_currency(input, cs, case_policy, dec_point) {
            Ok(_) => 0,
            Err(bad_pos) => bad_pos,
        },
    )
}

/// Evaluates `FUNCTION NUMVAL-C(input, currency, case_policy, dec_point)`.
///
/// # Arguments
///
/// * `input` - Candidate monetary numeric string bytes.
/// * `currency` - Optional currency string (defaults to `b"$"`).
/// * `case_policy` - [`CurrencyCasePolicy`] (`ExactCase` vs `AnyCase`).
/// * `dec_point` - Active [`DecimalPointKind`].
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `currency` or `input` is invalid.
pub fn str_numval_c(
    input: &[u8],
    currency: Option<&[u8]>,
    case_policy: CurrencyCasePolicy,
    dec_point: DecimalPointKind,
) -> Result<FixedDecimal> {
    let cs = validate_currency_arg("NUMVAL-C", currency)?;
    scan_numval_currency(input, cs, case_policy, dec_point).map_err(|pos| {
        ec_argument_function(
            "NUMVAL-C",
            String::from_utf8_lossy(input).into_owned(),
            format!("invalid monetary character at position {pos}"),
        )
    })
}

/// Evaluates `FUNCTION TEST-NUMVAL-F(input, dec_point)`.
///
/// Returns `0` when valid, or the `1`-based offending position (`1..=input.len() + 1`).
///
/// # Arguments
///
/// * `input` - Candidate floating-point numeric string bytes.
/// * `dec_point` - Active [`DecimalPointKind`].
#[must_use]
pub fn str_test_numval_f(input: &[u8], dec_point: DecimalPointKind) -> usize {
    match scan_numval_float(input, dec_point) {
        Ok(_) => 0,
        Err(bad_pos) => bad_pos,
    }
}

/// Evaluates `FUNCTION NUMVAL-F(input, dec_point)`.
///
/// # Arguments
///
/// * `input` - Candidate floating-point numeric string bytes.
/// * `dec_point` - Active [`DecimalPointKind`].
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `input` is syntactically invalid or overflows `FixedDecimal`.
pub fn str_numval_f(input: &[u8], dec_point: DecimalPointKind) -> Result<FixedDecimal> {
    scan_numval_float(input, dec_point).map_err(|pos| {
        ec_argument_function(
            "NUMVAL-F",
            String::from_utf8_lossy(input).into_owned(),
            format!("invalid floating-point syntax or range at position {pos}"),
        )
    })
}

/// Evaluates ISO 2023 `FUNCTION BASECONVERT(input, src_base, dst_base)` across bases `2..=36`.
///
/// # Arguments
///
/// * `input` - Integer numeral string in `src_base` (optional leading `+`/`-` and surrounding spaces).
/// * `src_base` - Validated source [`NumericBase`] (`2..=36`).
/// * `dst_base` - Validated destination [`NumericBase`] (`2..=36`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `input` is empty, contains out-of-radix digits, or overflows 256 bits.
pub fn str_baseconvert(
    input: &[u8],
    src_base: NumericBase,
    dst_base: NumericBase,
) -> Result<Vec<u8>> {
    let trimmed = str_trim(input, TrimDirection::Both, Some(b' '));
    let (neg, digits) = match trimmed.split_first() {
        None => {
            return Err(ec_argument_function(
                "BASECONVERT",
                "",
                "input numeral must not be empty",
            ));
        }
        Some((&b'-', rest)) => (true, rest),
        Some((&b'+', rest)) => (false, rest),
        Some(_) => (false, trimmed.as_slice()),
    };
    if digits.is_empty() {
        return Err(ec_argument_function(
            "BASECONVERT",
            String::from_utf8_lossy(input).into_owned(),
            "missing digits after sign",
        ));
    }
    let radix_u64 = u64::from(src_base.get());
    let mut value = Uint256::ZERO;
    for &b in digits {
        let Some(d) = decode_base36_digit(b) else {
            return Err(ec_argument_function(
                "BASECONVERT",
                char::from(b).to_string(),
                "invalid alphanumeric radix digit",
            ));
        };
        if d >= src_base.get() {
            return Err(ec_argument_function(
                "BASECONVERT",
                char::from(b).to_string(),
                "digit exceeds source base",
            ));
        }
        let Some(scaled) = value.checked_mul_u64(radix_u64) else {
            return Err(ec_argument_function(
                "BASECONVERT",
                String::from_utf8_lossy(input).into_owned(),
                "value exceeds 256-bit BASECONVERT capacity",
            ));
        };
        let Some(added) = scaled.checked_add(Uint256::from_u64(u64::from(d))) else {
            return Err(ec_argument_function(
                "BASECONVERT",
                String::from_utf8_lossy(input).into_owned(),
                "value exceeds 256-bit BASECONVERT capacity",
            ));
        };
        value = added;
    }
    Ok(encode_uint256_in_base(value, neg, dst_base.get()))
}

/// Evaluates ISO 2023 `FUNCTION BOOLEAN-OF-INTEGER(value, bit_length)`.
///
/// # Arguments
///
/// * `value` - Exact non-negative integer operand.
/// * `bit_length` - Target bit-string width (`1..=256`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `value` is negative/fractional or `bit_length` lies outside `1..=256`.
pub fn str_boolean_of_integer(value: &FixedDecimal, bit_length: usize) -> Result<Vec<u8>> {
    if !(1..=MAX_BOOLEAN_BIT_WIDTH).contains(&bit_length) {
        return Err(ec_argument_function(
            "BOOLEAN-OF-INTEGER",
            bit_length.to_string(),
            "bit length must lie in 1..=256",
        ));
    }
    if value.is_negative() {
        return Err(ec_argument_function(
            "BOOLEAN-OF-INTEGER",
            value.to_string(),
            "integer operand must be non-negative",
        ));
    }
    let (unscaled, status) = value
        .rescale(0, RoundingMode::Prohibited)
        .unwrap_or((*value, coblang_decimal::SizeErrorStatus::Overflowed));
    if status.is_error() {
        return Err(ec_argument_function(
            "BOOLEAN-OF-INTEGER",
            value.to_string(),
            "operand must be an exact integer",
        ));
    }
    let mut mag = unscaled.mantissa().magnitude();
    let two = Uint256::from_u64(2);
    let mut out = vec![b'0'; bit_length];
    for slot in out.iter_mut().rev() {
        let (q, rem) = mag
            .checked_div_rem(two)
            .unwrap_or((Uint256::ZERO, Uint256::ZERO));
        if !rem.is_zero() {
            *slot = b'1';
        }
        mag = q;
    }
    Ok(out)
}

/// Evaluates ISO 2023 `FUNCTION INTEGER-OF-BOOLEAN(bit_chars)`.
///
/// # Arguments
///
/// * `bit_chars` - Non-empty `'0'`/`'1'` bit-character sequence (`1..=256` bits).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `bit_chars` is empty, exceeds `256` bits, or contains non-bit bytes.
pub fn str_integer_of_boolean(bit_chars: &[u8]) -> Result<FixedDecimal> {
    if bit_chars.is_empty() || bit_chars.len() > MAX_BOOLEAN_BIT_WIDTH {
        return Err(ec_argument_function(
            "INTEGER-OF-BOOLEAN",
            format!("len={}", bit_chars.len()),
            "boolean bit string length must lie in 1..=256",
        ));
    }
    let mut acc = Uint256::ZERO;
    for &b in bit_chars {
        let bit = match b {
            b'0' => 0_u64,
            b'1' => 1_u64,
            _ => {
                return Err(ec_argument_function(
                    "INTEGER-OF-BOOLEAN",
                    char::from(b).to_string(),
                    "boolean string may only contain '0' and '1'",
                ));
            }
        };
        let shifted = acc.checked_mul_u64(2).unwrap_or(Uint256::MAX);
        acc = shifted
            .checked_add(Uint256::from_u64(bit))
            .unwrap_or(Uint256::MAX);
    }
    Ok(
        FixedDecimal::try_new(SignedInt256::from_parts(acc, DecimalSign::Positive), 0)
            .unwrap_or(FixedDecimal::ZERO),
    )
}

/// Evaluates `FUNCTION BIT-OF(bytes)` (8 `'0'`/`'1'` characters per input byte, MSB first).
///
/// # Arguments
///
/// * `bytes` - Raw byte slice.
#[must_use]
pub fn str_bit_of(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 8);
    for &byte in bytes {
        for shift in (0..8_u8).rev() {
            out.push(if (byte >> shift) & 1 == 1 { b'1' } else { b'0' });
        }
    }
    out
}

/// Evaluates `FUNCTION BIT-TO-CHAR(bit_chars)` (packing 8 `'0'`/`'1'` characters per output byte).
///
/// # Arguments
///
/// * `bit_chars` - Non-empty bit-character slice whose length is a multiple of `8`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `bit_chars` is empty, not a multiple of `8`, or contains non-bits.
pub fn str_bit_to_char(bit_chars: &[u8]) -> Result<Vec<u8>> {
    if bit_chars.is_empty() || !bit_chars.len().is_multiple_of(8) {
        return Err(ec_argument_function(
            "BIT-TO-CHAR",
            format!("len={}", bit_chars.len()),
            "bit string length must be a non-zero multiple of 8",
        ));
    }
    let mut out = Vec::with_capacity(bit_chars.len() / 8);
    for chunk in bit_chars.as_chunks::<8>().0 {
        let mut byte = 0_u8;
        for &b in chunk {
            let bit = match b {
                b'0' => 0_u8,
                b'1' => 1_u8,
                _ => {
                    return Err(ec_argument_function(
                        "BIT-TO-CHAR",
                        char::from(b).to_string(),
                        "bit string may only contain '0' and '1'",
                    ));
                }
            };
            byte = (byte << 1) | bit;
        }
        out.push(byte);
    }
    Ok(out)
}

/// Evaluates `FUNCTION HEX-OF(bytes)` (2 uppercase ASCII hex characters per input byte).
///
/// # Arguments
///
/// * `bytes` - Raw byte slice.
#[must_use]
pub fn str_hex_of(bytes: &[u8]) -> Vec<u8> {
    encode_hex_bytes(bytes, true)
}

/// Evaluates `FUNCTION HEX-TO-CHAR(hex_chars)` (decoding pairs of ASCII hex digits into raw bytes).
///
/// # Arguments
///
/// * `hex_chars` - Non-empty even-length hexadecimal character slice.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `hex_chars` is empty, has odd length, or contains non-hex characters.
pub fn str_hex_to_char(hex_chars: &[u8]) -> Result<Vec<u8>> {
    if hex_chars.is_empty() || !hex_chars.len().is_multiple_of(2) {
        return Err(ec_argument_function(
            "HEX-TO-CHAR",
            format!("len={}", hex_chars.len()),
            "hex string length must be a non-zero multiple of 2",
        ));
    }
    let mut out = Vec::with_capacity(hex_chars.len() / 2);
    for &[hi_b, lo_b] in hex_chars.as_chunks::<2>().0 {
        let (Some(hi), Some(lo)) = (decode_hex_nibble(hi_b), decode_hex_nibble(lo_b)) else {
            return Err(ec_argument_function(
                "HEX-TO-CHAR",
                String::from_utf8_lossy(&[hi_b, lo_b]).into_owned(),
                "invalid hexadecimal nibble",
            ));
        };
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

/// Evaluates ISO 2023 `FUNCTION CONVERT(input, spec)` (format conversion or alphabet translation).
///
/// # Arguments
///
/// * `input` - Source byte slice.
/// * `spec` - [`ConvertSpec`] (`Format { source, target }` or `Alphabet { from_chars, to_chars }`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `input` fails `source` decoding or `from_chars.len() != to_chars.len()`.
pub fn str_convert(input: &[u8], spec: ConvertSpec<'_>) -> Result<Vec<u8>> {
    match spec {
        ConvertSpec::Alphabet {
            from_chars,
            to_chars,
        } => {
            if from_chars.len() != to_chars.len() {
                return Err(ec_argument_function(
                    "CONVERT",
                    format!("{} != {}", from_chars.len(), to_chars.len()),
                    "source and target alphabets must have equal length",
                ));
            }
            let mut out = input.to_vec();
            for slot in &mut out {
                for (i, &src_c) in from_chars.iter().enumerate() {
                    if *slot == src_c {
                        *slot = to_chars.get(i).copied().unwrap_or(*slot);
                        break;
                    }
                }
            }
            Ok(out)
        }
        ConvertSpec::Format { source, target } => {
            let raw = match source {
                ConvertFormat::Alphanumeric => input.to_vec(),
                ConvertFormat::HexUpper | ConvertFormat::HexLower => str_hex_to_char(input)?,
                ConvertFormat::BitString => str_bit_to_char(input)?,
                ConvertFormat::Base64 => decode_base64(input)?,
            };
            Ok(match target {
                ConvertFormat::Alphanumeric => raw,
                ConvertFormat::HexUpper => encode_hex_bytes(&raw, true),
                ConvertFormat::HexLower => encode_hex_bytes(&raw, false),
                ConvertFormat::BitString => str_bit_of(&raw),
                ConvertFormat::Base64 => encode_base64(&raw),
            })
        }
    }
}

/// Evaluates ISO 2023 `FUNCTION STANDARD-COMPARE(lhs, rhs, collation, cultural_level)`.
///
/// # Arguments
///
/// * `lhs` - Left operand bytes.
/// * `rhs` - Right operand bytes.
/// * `collation` - Active [`CollatingTable`].
/// * `cultural_level` - Optional ISO 14651 comparison level (`1..=4`, defaults to `4`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `cultural_level` is outside `1..=4`.
pub fn str_standard_compare(
    lhs: &[u8],
    rhs: &[u8],
    collation: CollatingTable<'_>,
    cultural_level: Option<u8>,
) -> Result<CompareIndicator> {
    let level = cultural_level.unwrap_or(4);
    if !(1..=4).contains(&level) {
        return Err(ec_argument_function(
            "STANDARD-COMPARE",
            level.to_string(),
            "comparison level must lie in 1..=4",
        ));
    }
    let max_len = lhs.len().max(rhs.len());
    for i in 0..max_len {
        let a = lhs.get(i).copied().unwrap_or(b' ');
        let b = rhs.get(i).copied().unwrap_or(b' ');
        let wa = standard_level_key(a, level, collation);
        let wb = standard_level_key(b, level, collation);
        match wa.cmp(&wb) {
            Ordering::Less => return Ok(CompareIndicator::Less),
            Ordering::Greater => return Ok(CompareIndicator::Greater),
            Ordering::Equal => {}
        }
    }
    Ok(CompareIndicator::Equal)
}

/// Evaluates `FUNCTION LOCALE-COMPARE(lhs, rhs, locale)`.
///
/// # Arguments
///
/// * `lhs` - Left operand bytes.
/// * `rhs` - Right operand bytes.
/// * `locale` - Optional locale identifier (`"C"`/`"POSIX"` uses exact bytewise order, else cultural).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if an explicit `locale` string is empty/whitespace-only.
pub fn str_locale_compare(
    lhs: &[u8],
    rhs: &[u8],
    locale: Option<&str>,
) -> Result<CompareIndicator> {
    let is_posix = if let Some(raw) = locale {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ec_argument_function(
                "LOCALE-COMPARE",
                raw,
                "explicit locale name must not be empty",
            ));
        }
        trimmed.eq_ignore_ascii_case("C") || trimmed.eq_ignore_ascii_case("POSIX")
    } else {
        false
    };
    if is_posix {
        return str_standard_compare(lhs, rhs, CollatingTable::NativeAscii, Some(4));
    }
    // Cultural multi-pass: primary case-folded, tertiary exact case tie-break.
    let p1 = str_standard_compare(lhs, rhs, CollatingTable::NativeAscii, Some(1))?;
    if p1 != CompareIndicator::Equal {
        return Ok(p1);
    }
    str_standard_compare(lhs, rhs, CollatingTable::NativeAscii, Some(3))
}

/// Evaluates `GnuCOBOL` `FUNCTION STORED-CHAR-LENGTH(bytes)` (length excluding trailing spaces).
///
/// # Arguments
///
/// * `bytes` - Raw alphanumeric byte slice.
#[must_use]
pub fn str_stored_char_length(bytes: &[u8]) -> usize {
    let trailing_spaces = bytes.iter().rev().take_while(|&&b| b == b' ').count();
    bytes.len() - trailing_spaces
}

/// Simultaneous single-pass replacement engine backing `SUBSTITUTE` and `SUBSTITUTE-CASE`.
///
/// # Arguments
///
/// * `fn_name` - Intrinsic name for diagnostics.
/// * `subject` - Input byte slice.
/// * `pairs` - Non-empty `(search, replacement)` pairs.
/// * `case_mode` - [`CaseSensitivity`] mode.
fn substitute_impl(
    fn_name: &'static str,
    subject: &[u8],
    pairs: &[(&[u8], &[u8])],
    case_mode: CaseSensitivity,
) -> Result<Vec<u8>> {
    if pairs.is_empty() {
        return Err(ec_argument_function(
            fn_name,
            "",
            "at least one search/replacement pair is required",
        ));
    }
    for &(pat, _) in pairs {
        if pat.is_empty() {
            return Err(ec_argument_function(
                fn_name,
                "",
                "search pattern in SUBSTITUTE must not be empty",
            ));
        }
    }
    let mut out = Vec::with_capacity(subject.len());
    let mut pos = 0_usize;
    while pos < subject.len() {
        let rem = subject.get(pos..).unwrap_or(&[]);
        let mut matched: Option<(usize, &[u8])> = None;
        for &(pat, rep) in pairs {
            if let Some(prefix) = rem.get(..pat.len())
                && slice_eq_with_case(prefix, pat, case_mode)
            {
                matched = Some((pat.len(), rep));
                break;
            }
        }
        if let Some((adv, rep)) = matched {
            out.extend_from_slice(rep);
            pos += adv;
        } else {
            out.push(subject.get(pos).copied().unwrap_or(0));
            pos += 1;
        }
    }
    Ok(out)
}

/// Executes `FIND-STRING` after arguments have been validated.
///
/// # Arguments
///
/// * `subject` - Haystack bytes.
/// * `pattern` - Non-empty needle bytes.
/// * `spec` - Validated [`FindStringSpec`].
#[must_use]
fn find_string_checked(subject: &[u8], pattern: &[u8], spec: FindStringSpec) -> usize {
    let start = spec.after_pos;
    match spec.mode {
        FindStringMode::AnyChar => {
            for idx in start..subject.len() {
                let b = subject.get(idx).copied().unwrap_or(0);
                if pattern
                    .iter()
                    .any(|&p| byte_eq_with_case(b, p, spec.case_mode))
                {
                    return idx + 1;
                }
            }
            0
        }
        FindStringMode::Last => {
            if pattern.len() > subject.len().saturating_sub(start) {
                return 0;
            }
            let max_i = subject.len() - pattern.len();
            for idx in (start..=max_i).rev() {
                let win = subject.get(idx..idx + pattern.len()).unwrap_or(&[]);
                if slice_eq_with_case(win, pattern, spec.case_mode) {
                    return idx + 1;
                }
            }
            0
        }
        FindStringMode::First | FindStringMode::Occurrence(_) | FindStringMode::AllCount => {
            let target_occ = match spec.mode {
                FindStringMode::First => Some(1_usize),
                FindStringMode::Occurrence(n) => Some(n),
                _ => None,
            };
            let mut count = 0_usize;
            let mut pos = start;
            while pos + pattern.len() <= subject.len() {
                let win = subject.get(pos..pos + pattern.len()).unwrap_or(&[]);
                if slice_eq_with_case(win, pattern, spec.case_mode) {
                    count += 1;
                    if target_occ == Some(count) {
                        return pos + 1;
                    }
                    pos += pattern.len();
                } else {
                    pos += 1;
                }
            }
            if spec.mode == FindStringMode::AllCount {
                count
            } else {
                0
            }
        }
    }
}

/// Compares two equal-length byte slices under `case_mode`.
///
/// # Arguments
///
/// * `a` - First slice.
/// * `b` - Second slice.
/// * `case_mode` - [`CaseSensitivity`] mode.
#[must_use]
fn slice_eq_with_case(a: &[u8], b: &[u8], case_mode: CaseSensitivity) -> bool {
    match case_mode {
        CaseSensitivity::Sensitive => a == b,
        CaseSensitivity::Insensitive => a.eq_ignore_ascii_case(b),
    }
}

/// Compares two single bytes under `case_mode`.
///
/// # Arguments
///
/// * `a` - First byte.
/// * `b` - Second byte.
/// * `case_mode` - [`CaseSensitivity`] mode.
#[must_use]
const fn byte_eq_with_case(a: u8, b: u8, case_mode: CaseSensitivity) -> bool {
    match case_mode {
        CaseSensitivity::Sensitive => a == b,
        CaseSensitivity::Insensitive => a.eq_ignore_ascii_case(&b),
    }
}

/// Validates and trims the optional `currency` parameter of `NUMVAL-C` / `TEST-NUMVAL-C`.
///
/// # Arguments
///
/// * `fn_name` - Intrinsic name for diagnostics.
/// * `currency` - Optional raw currency bytes.
fn validate_currency_arg<'a>(
    fn_name: &'static str,
    currency: Option<&'a [u8]>,
) -> Result<&'a [u8]> {
    let Some(raw) = currency else {
        return Ok(b"$");
    };
    let start = raw.iter().take_while(|&&b| b == b' ').count();
    let tail = raw.get(start..).unwrap_or(&[]);
    let end_spaces = tail.iter().rev().take_while(|&&b| b == b' ').count();
    let cs = tail.get(..tail.len() - end_spaces).unwrap_or(&[]);
    if cs.is_empty()
        || cs
            .iter()
            .any(|&b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b',' | b' '))
    {
        return Err(ec_argument_function(
            fn_name,
            String::from_utf8_lossy(raw).into_owned(),
            "currency string must be non-empty and free of digits, signs, separators, and spaces",
        ));
    }
    Ok(cs)
}

/// Scans an ISO COBOL `NUMVAL` string, returning `Ok(FixedDecimal)` or `Err(1_based_error_pos)`.
///
/// # Arguments
///
/// * `input` - Raw input bytes.
/// * `dec_point` - Active [`DecimalPointKind`].
fn scan_numval_plain(
    input: &[u8],
    dec_point: DecimalPointKind,
) -> core::result::Result<FixedDecimal, usize> {
    let dec_b = decimal_sep_byte(dec_point);
    let mut pos = skip_ascii_spaces(input, 0);
    if pos == input.len() {
        return Err(input.len() + 1);
    }
    let mut negative = false;
    let mut has_lead_sign = false;
    let first_b = input.get(pos).copied().unwrap_or(0);
    if first_b == b'+' || first_b == b'-' {
        has_lead_sign = true;
        negative = first_b == b'-';
        pos = skip_ascii_spaces(input, pos + 1);
    }
    let (digits, frac_count, next_pos) = scan_mantissa_digits(input, pos, dec_b, None)?;
    if digits.len() > 38 {
        return Err(input.len());
    }
    pos = skip_ascii_spaces(input, next_pos);
    if !has_lead_sign && pos < input.len() {
        let (trail_neg, after_sign) = scan_trailing_sign(input, pos)?;
        negative = trail_neg;
        pos = skip_ascii_spaces(input, after_sign);
    }
    if pos < input.len() {
        return Err(pos + 1);
    }
    let scale = i16::try_from(frac_count).unwrap_or(0);
    build_parsed_decimal_with_scale(&digits, scale, negative, input.len())
}

/// Best-effort `NUMVAL` / `NUMVAL-C` fallback extractor invoked after `EC-ARGUMENT-FUNCTION`
/// is recorded (extracting valid digits, first decimal separator, sign/`CR`/`DB`, and low-order 38 digits).
///
/// # Arguments
///
/// * `input` - Raw input bytes.
/// * `dec_point` - Active [`DecimalPointKind`].
#[must_use]
pub fn str_numval_salvage(input: &[u8], dec_point: DecimalPointKind) -> FixedDecimal {
    let dec_b = decimal_sep_byte(dec_point);
    let mut digits = Vec::with_capacity(38);
    let mut seen_dec = false;
    let mut frac_count = 0_usize;
    let mut negative = false;
    let mut has_sign = false;
    let mut idx = 0_usize;
    while let Some(&b) = input.get(idx) {
        if b.is_ascii_digit() {
            digits.push(b);
            if seen_dec {
                frac_count += 1;
            }
            idx += 1;
        } else if b == dec_b && !seen_dec {
            seen_dec = true;
            idx += 1;
        } else if (b == b'+' || b == b'-') && !has_sign {
            has_sign = true;
            negative = b == b'-';
            idx += 1;
        } else if !digits.is_empty()
            && let Some(two) = input.get(idx..idx + 2)
            && (two.eq_ignore_ascii_case(b"CR") || two.eq_ignore_ascii_case(b"DB"))
        {
            negative = true;
            break;
        } else {
            idx += 1;
        }
    }
    if digits.is_empty() {
        return FixedDecimal::ZERO;
    }
    let slice = if digits.len() > 38 {
        digits.get(digits.len() - 38..).unwrap_or(&digits)
    } else {
        digits.as_slice()
    };
    let eff_frac = frac_count.min(slice.len());
    let scale = i16::try_from(eff_frac).unwrap_or(0);
    build_parsed_decimal_with_scale(slice, scale, negative, input.len())
        .unwrap_or(FixedDecimal::ZERO)
}

/// Scans an ISO COBOL `NUMVAL-C` string, returning `Ok(FixedDecimal)` or `Err(1_based_error_pos)`.
///
/// # Arguments
///
/// * `input` - Raw input bytes.
/// * `cs` - Validated non-empty currency symbol bytes.
/// * `case_policy` - [`CurrencyCasePolicy`].
/// * `dec_point` - Active [`DecimalPointKind`].
fn scan_numval_currency(
    input: &[u8],
    cs: &[u8],
    case_policy: CurrencyCasePolicy,
    dec_point: DecimalPointKind,
) -> core::result::Result<FixedDecimal, usize> {
    let dec_b = decimal_sep_byte(dec_point);
    let grp_b = if dec_b == b'.' { b',' } else { b'.' };
    let case_mode = match case_policy {
        CurrencyCasePolicy::ExactCase => CaseSensitivity::Sensitive,
        CurrencyCasePolicy::AnyCase => CaseSensitivity::Insensitive,
    };
    let mut pos = skip_ascii_spaces(input, 0);
    if pos == input.len() {
        return Err(input.len() + 1);
    }
    let mut negative = false;
    let mut has_lead_sign = false;
    let first_b = input.get(pos).copied().unwrap_or(0);
    if first_b == b'+' || first_b == b'-' {
        has_lead_sign = true;
        negative = first_b == b'-';
        pos = skip_ascii_spaces(input, pos + 1);
    }
    if let Some(prefix) = input.get(pos..pos + cs.len())
        && slice_eq_with_case(prefix, cs, case_mode)
    {
        pos = skip_ascii_spaces(input, pos + cs.len());
        let sb = input.get(pos).copied().unwrap_or(0);
        if !has_lead_sign && (sb == b'+' || sb == b'-') {
            has_lead_sign = true;
            negative = sb == b'-';
            pos = skip_ascii_spaces(input, pos + 1);
        }
    }
    let (digits, frac_count, next_pos) = scan_mantissa_digits(input, pos, dec_b, Some(grp_b))?;
    if digits.len() > 38 {
        return Err(input.len());
    }
    pos = skip_ascii_spaces(input, next_pos);
    if !has_lead_sign && pos < input.len() {
        let (trail_neg, after_sign) = scan_trailing_sign(input, pos)?;
        negative = trail_neg;
        pos = skip_ascii_spaces(input, after_sign);
    }
    if pos < input.len() {
        return Err(pos + 1);
    }
    let scale = i16::try_from(frac_count).unwrap_or(0);
    build_parsed_decimal_with_scale(&digits, scale, negative, input.len())
}

/// Scans an ISO COBOL `NUMVAL-F` string, returning `Ok(FixedDecimal)` or `Err(1_based_error_pos)`.
///
/// # Arguments
///
/// * `input` - Raw input bytes.
/// * `dec_point` - Active [`DecimalPointKind`].
fn scan_numval_float(
    input: &[u8],
    dec_point: DecimalPointKind,
) -> core::result::Result<FixedDecimal, usize> {
    let dec_b = decimal_sep_byte(dec_point);
    let mut pos = skip_ascii_spaces(input, 0);
    if pos == input.len() {
        return Err(input.len() + 1);
    }
    let mut negative = false;
    let mut has_lead_sign = false;
    let first_b = input.get(pos).copied().unwrap_or(0);
    if first_b == b'+' || first_b == b'-' {
        has_lead_sign = true;
        negative = first_b == b'-';
        pos = skip_ascii_spaces(input, pos + 1);
    }
    let (digits, frac_count, next_pos) = scan_mantissa_digits(input, pos, dec_b, None)?;
    pos = skip_ascii_spaces(input, next_pos);
    if !has_lead_sign
        && let Some(&ts) = input.get(pos)
        && (ts == b'+' || ts == b'-')
    {
        negative = ts == b'-';
        pos = skip_ascii_spaces(input, pos + 1);
    }
    let mut exp_val = 0_i32;
    if let Some(&e_byte) = input.get(pos)
        && e_byte.eq_ignore_ascii_case(&b'E')
    {
        pos = skip_ascii_spaces(input, pos + 1);
        let Some(&sb) = input.get(pos) else {
            return Err(pos + 1);
        };
        if sb != b'+' && sb != b'-' {
            return Err(pos + 1);
        }
        let exp_neg = sb == b'-';
        pos = skip_ascii_spaces(input, pos + 1);
        let exp_start = pos;
        while let Some(&db) = input.get(pos) {
            if !db.is_ascii_digit() {
                break;
            }
            if pos - exp_start >= 4 {
                return Err(pos + 1);
            }
            exp_val = exp_val * 10 + i32::from(db - b'0');
            pos += 1;
        }
        if pos == exp_start {
            return Err(pos + 1);
        }
        if exp_neg {
            exp_val = -exp_val;
        }
        pos = skip_ascii_spaces(input, pos);
    }
    if pos < input.len() {
        return Err(pos + 1);
    }
    let adj_scale_i32 = i32::try_from(frac_count).unwrap_or(i32::MAX) - exp_val;
    if !(-i32::from(FixedDecimal::MAX_INTERMEDIATE_SCALE)
        ..=i32::from(FixedDecimal::MAX_INTERMEDIATE_SCALE))
        .contains(&adj_scale_i32)
    {
        return Err(input.len());
    }
    let adj_scale = i16::try_from(adj_scale_i32).unwrap_or(0);
    build_parsed_decimal_with_scale(&digits, adj_scale, negative, input.len())
}

/// Scans `[int_digits (with optional grp_sep)] [dec_sep frac_digits]` starting at `start`.
///
/// # Arguments
///
/// * `input` - Input byte slice.
/// * `start` - 0-based cursor index.
/// * `dec_b` - Active decimal separator byte (`b'.'` or `b','`).
/// * `grp_b` - Optional grouping separator byte.
fn scan_mantissa_digits(
    input: &[u8],
    start: usize,
    dec_b: u8,
    grp_b: Option<u8>,
) -> core::result::Result<(Vec<u8>, usize, usize), usize> {
    let mut digits = Vec::with_capacity(16);
    let mut pos = start;
    while let Some(&b) = input.get(pos) {
        if b.is_ascii_digit() {
            digits.push(b);
            pos += 1;
        } else if grp_b == Some(b) {
            if digits.is_empty() {
                return Err(pos + 1);
            }
            let Some(&next_b) = input.get(pos + 1) else {
                return Err(pos + 2);
            };
            if !next_b.is_ascii_digit() {
                return Err(pos + 2);
            }
            pos += 1;
        } else {
            break;
        }
    }
    let mut frac_count = 0_usize;
    if input.get(pos).copied() == Some(dec_b) {
        pos += 1;
        while let Some(&b) = input.get(pos) {
            if b.is_ascii_digit() {
                digits.push(b);
                frac_count += 1;
                pos += 1;
            } else {
                break;
            }
        }
    }
    if digits.is_empty() {
        return Err(pos + 1);
    }
    Ok((digits, frac_count, pos))
}

/// Scans a trailing `+`, `-`, `CR`, or `DB` sign starting at `pos` (case-insensitive per ISO COBOL).
///
/// # Arguments
///
/// * `input` - Input byte slice.
/// * `pos` - 0-based index of the first trailing non-space byte.
fn scan_trailing_sign(input: &[u8], pos: usize) -> core::result::Result<(bool, usize), usize> {
    let b0 = input.get(pos).copied().unwrap_or(0);
    if b0 == b'+' {
        return Ok((false, pos + 1));
    }
    if b0 == b'-' {
        return Ok((true, pos + 1));
    }
    if byte_eq_with_case(b0, b'C', CaseSensitivity::Insensitive) {
        let Some(&b1) = input.get(pos + 1) else {
            return Err(pos + 2);
        };
        if byte_eq_with_case(b1, b'R', CaseSensitivity::Insensitive) {
            return Ok((true, pos + 2));
        }
        return Err(pos + 2);
    }
    if byte_eq_with_case(b0, b'D', CaseSensitivity::Insensitive) {
        let Some(&b1) = input.get(pos + 1) else {
            return Err(pos + 2);
        };
        if byte_eq_with_case(b1, b'B', CaseSensitivity::Insensitive) {
            return Ok((true, pos + 2));
        }
        return Err(pos + 2);
    }
    Err(pos + 1)
}

/// Converts collected ASCII digits and signed `scale` (`-76..=76`) into a [`FixedDecimal`].
///
/// # Arguments
///
/// * `digits` - Collected ASCII `'0'..'9'` digits.
/// * `scale` - Validated scale in `-76..=76`.
/// * `negative` - Sign flag.
/// * `err_pos` - Fallback 1-based error position on 256-bit overflow.
fn build_parsed_decimal_with_scale(
    digits: &[u8],
    scale: i16,
    negative: bool,
    err_pos: usize,
) -> core::result::Result<FixedDecimal, usize> {
    let Ok(mag) = Uint256::from_decimal_digits(digits) else {
        return Err(err_pos);
    };
    let sign = if mag.is_zero() {
        DecimalSign::UnsignedZero
    } else if negative {
        DecimalSign::Negative
    } else {
        DecimalSign::Positive
    };
    Ok(
        FixedDecimal::try_new(SignedInt256::from_parts(mag, sign), scale)
            .unwrap_or(FixedDecimal::ZERO),
    )
}

/// Advances `pos` past contiguous ASCII spaces (`0x20`).
///
/// # Arguments
///
/// * `input` - Input byte slice.
/// * `pos` - Starting cursor index.
#[must_use]
fn skip_ascii_spaces(input: &[u8], mut pos: usize) -> usize {
    while input.get(pos).copied() == Some(b' ') {
        pos += 1;
    }
    pos
}

/// Returns the decimal separator byte (`b'.'` or `b','`) for `dec_point`.
///
/// # Arguments
///
/// * `dec_point` - Active [`DecimalPointKind`].
#[must_use]
const fn decimal_sep_byte(dec_point: DecimalPointKind) -> u8 {
    match dec_point {
        DecimalPointKind::Period => b'.',
        DecimalPointKind::Comma => b',',
    }
}

/// Decodes a single alphanumeric base-36 digit (`'0'..'9'`, `'A'..'Z'`, `'a'..'z'`).
///
/// # Arguments
///
/// * `byte` - Candidate ASCII character byte.
#[must_use]
const fn decode_base36_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'Z' => Some(byte - b'A' + 10),
        b'a'..=b'z' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Encodes `value` into uppercase base-`radix` ASCII digits (`2..=36`) with optional `-` prefix.
///
/// # Arguments
///
/// * `value` - Unsigned 256-bit magnitude.
/// * `negative` - Whether to prepend `'-'` when `value > 0`.
/// * `radix` - Validated base (`2..=36`).
#[must_use]
fn encode_uint256_in_base(mut value: Uint256, negative: bool, radix: u8) -> Vec<u8> {
    if value.is_zero() {
        return vec![b'0'];
    }
    let rad = Uint256::from_u64(u64::from(radix));
    let mut rev_digits = Vec::with_capacity(64);
    while !value.is_zero() {
        let (q, r) = value
            .checked_div_rem(rad)
            .unwrap_or((Uint256::ZERO, Uint256::ZERO));
        let rem_u8 = u8::try_from(r.to_u128().unwrap_or(0)).unwrap_or(0);
        let ch = if rem_u8 < 10 {
            b'0' + rem_u8
        } else {
            b'A' + (rem_u8 - 10)
        };
        rev_digits.push(ch);
        value = q;
    }
    if negative {
        rev_digits.push(b'-');
    }
    rev_digits.reverse();
    rev_digits
}

/// Decodes an ASCII hex digit into its 4-bit nibble (`0..=15`).
///
/// # Arguments
///
/// * `byte` - Candidate ASCII hex byte.
#[must_use]
const fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// Encodes `bytes` into 2-char hexadecimal ASCII (`upper` selects `A-F` vs `a-f`).
///
/// # Arguments
///
/// * `bytes` - Raw byte slice.
/// * `upper` - Uppercase hex selector.
#[must_use]
fn encode_hex_bytes(bytes: &[u8], upper: bool) -> Vec<u8> {
    let alpha_base = if upper { b'A' } else { b'a' };
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for &b in bytes {
        for nib in [b >> 4, b & 0x0F] {
            out.push(if nib < 10 {
                b'0' + nib
            } else {
                alpha_base + (nib - 10)
            });
        }
    }
    out
}

/// Encodes `bytes` into standard RFC 4648 `Base64` with `'='` padding.
///
/// # Arguments
///
/// * `bytes` - Raw input bytes.
#[must_use]
fn encode_base64(bytes: &[u8]) -> Vec<u8> {
    /// Standard 64-character RFC 4648 `Base64` encoding alphabet.
    const B64_ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk.first().copied().unwrap_or(0);
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let triple = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        let i0 = usize::try_from((triple >> 18) & 0x3F).unwrap_or(0);
        let i1 = usize::try_from((triple >> 12) & 0x3F).unwrap_or(0);
        let i2 = usize::try_from((triple >> 6) & 0x3F).unwrap_or(0);
        let i3 = usize::try_from(triple & 0x3F).unwrap_or(0);
        out.push(B64_ALPHABET.get(i0).copied().unwrap_or(b'A'));
        out.push(B64_ALPHABET.get(i1).copied().unwrap_or(b'A'));
        out.push(if chunk.len() >= 2 {
            B64_ALPHABET.get(i2).copied().unwrap_or(b'A')
        } else {
            b'='
        });
        out.push(if chunk.len() == 3 {
            B64_ALPHABET.get(i3).copied().unwrap_or(b'A')
        } else {
            b'='
        });
    }
    out
}

/// Decodes standard RFC 4648 `Base64` bytes into raw bytes.
///
/// # Arguments
///
/// * `b64` - `Base64`-encoded ASCII slice.
fn decode_base64(b64: &[u8]) -> Result<Vec<u8>> {
    if !b64.len().is_multiple_of(4) {
        return Err(ec_argument_function(
            "CONVERT",
            format!("len={}", b64.len()),
            "Base64 input length must be a multiple of 4",
        ));
    }
    let mut out = Vec::with_capacity((b64.len() / 4) * 3);
    let quads = b64.as_chunks::<4>().0;
    let total_quads = quads.len();
    for (q_idx, &quad) in quads.iter().enumerate() {
        let [c0, c1, c2, c3] = quad;
        let (Some(v0), Some(v1)) = (decode_b64_sextet(c0), decode_b64_sextet(c1)) else {
            return Err(ec_argument_function(
                "CONVERT",
                String::from_utf8_lossy(&quad).into_owned(),
                "invalid Base64 character",
            ));
        };
        let is_last = q_idx + 1 == total_quads;
        if c2 == b'=' {
            if !is_last || c3 != b'=' {
                return Err(ec_argument_function(
                    "CONVERT",
                    String::from_utf8_lossy(&quad).into_owned(),
                    "malformed Base64 '=' padding",
                ));
            }
            out.push((v0 << 2) | (v1 >> 4));
        } else if c3 == b'=' {
            let Some(v2) = decode_b64_sextet(c2) else {
                return Err(ec_argument_function(
                    "CONVERT",
                    String::from_utf8_lossy(&quad).into_owned(),
                    "invalid Base64 character",
                ));
            };
            if !is_last {
                return Err(ec_argument_function(
                    "CONVERT",
                    String::from_utf8_lossy(&quad).into_owned(),
                    "Base64 padding only permitted in final quartet",
                ));
            }
            out.push((v0 << 2) | (v1 >> 4));
            out.push((v1 << 4) | (v2 >> 2));
        } else {
            let (Some(v2), Some(v3)) = (decode_b64_sextet(c2), decode_b64_sextet(c3)) else {
                return Err(ec_argument_function(
                    "CONVERT",
                    String::from_utf8_lossy(&quad).into_owned(),
                    "invalid Base64 character",
                ));
            };
            out.push((v0 << 2) | (v1 >> 4));
            out.push((v1 << 4) | (v2 >> 2));
            out.push((v2 << 6) | v3);
        }
    }
    Ok(out)
}

/// Maps a single RFC 4648 `Base64` non-padding character to `0..=63`.
///
/// # Arguments
///
/// * `ch` - Candidate ASCII byte.
#[must_use]
const fn decode_b64_sextet(ch: u8) -> Option<u8> {
    match ch {
        b'A'..=b'Z' => Some(ch - b'A'),
        b'a'..=b'z' => Some(ch - b'a' + 26),
        b'0'..=b'9' => Some(ch - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Looks up the primary rank (`0..=255` or `u16::MAX` if absent) of `byte` in `collation`.
///
/// # Arguments
///
/// * `byte` - Single character byte.
/// * `collation` - Active [`CollatingTable`].
#[must_use]
fn collation_rank_of(byte: u8, collation: CollatingTable<'_>) -> u16 {
    match collation {
        CollatingTable::NativeAscii => u16::from(byte),
        CollatingTable::Custom(tbl) => tbl
            .iter()
            .position(|&b| b == byte)
            .and_then(|idx| u16::try_from(idx).ok())
            .unwrap_or(u16::MAX),
    }
}

/// Computes the comparison weight tuple `(primary, case_tiebreak)` of `byte` at ISO level `1..=4`.
///
/// # Arguments
///
/// * `byte` - Single character byte.
/// * `level` - Validated level (`1..=4`).
/// * `collation` - Active [`CollatingTable`].
#[must_use]
fn standard_level_key(byte: u8, level: u8, collation: CollatingTable<'_>) -> (u16, u8) {
    match level {
        1 | 2 => {
            let folded = byte.to_ascii_lowercase();
            (collation_rank_of(folded, collation), 0)
        }
        3 => {
            let folded = byte.to_ascii_lowercase();
            let is_upper = u8::from(byte.is_ascii_uppercase());
            (collation_rank_of(folded, collation), is_upper)
        }
        _ => (collation_rank_of(byte, collation), 0),
    }
}
