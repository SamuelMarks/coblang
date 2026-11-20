//! Pure-Rust runtime execution engine for `MOVE`, `INITIALIZE`, 38/76-digit `FixedDecimal` arithmetic,
//! relational comparisons, `IS NUMERIC`/`ALPHABETIC` class checks, and `DISPLAY`/`HEX-OF` staging.

use core::cmp::Ordering;
use std::io::Write;

use coblang_common::{CoblangError, DigitCount, Result, ScaleFactor};
use coblang_decimal::{
    Codepage, FixedDecimal, HighOrderPolicy, OverpunchDialect, RoundingMode, SignednessPolicy,
    SizeErrorStatus, UnmappedCharPolicy, transcode_bytes,
};
use coblang_hir::{
    DataCategory, HirFigurative, HirSortDirection, HirType, NumericSignSpec, Signedness,
};
use coblang_pic::{
    InitTargetField, InitializeSpec, MoveFieldDesc, MoveSource, check_field_is_numeric,
    decode_numeric_field, execute_initialize, execute_move, format_field_for_display,
    is_zoned_negative_zero, store_decimal_to_field,
};

use crate::LineAdvancing;
use crate::desc_codec::{
    DecodedTableSortPlan, FillerMembership, RedefinesMembership, RuntimeTableSortKey,
    decode_field_spec, decode_init_plan, decode_table_sort_plan,
};

/// Source operand kind passed across the runtime `MOVE` / `INITIALIZE` / `COMPARE` boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeMoveSourceKind {
    /// Stored COBOL field (`bytes` + `RuntimeFieldSpec`).
    Field,
    /// Raw alphanumeric literal bytes.
    AlphanumericLiteral,
    /// Figurative `ZERO`.
    FigZero,
    /// Figurative `SPACE`.
    FigSpace,
    /// Figurative `HIGH-VALUE`.
    FigHigh,
    /// Figurative `LOW-VALUE`.
    FigLow,
    /// Figurative `QUOTE`.
    FigQuote,
    /// Figurative `NULL`.
    FigNull,
    /// Figurative `ALL <pattern>` (`bytes` holds pattern).
    FigAll,
    /// Exact ASCII decimal constant (`[+|-]digits[.digits]`).
    DecimalAscii,
    /// `NATIONAL` (`N"..."` / `NX"..."`) UTF-16BE literal bytes.
    NationalLiteral,
    /// `UTF-8` (`U"..."` / `UX"..."`) UTF-8 literal bytes.
    Utf8Literal,
}

impl RuntimeMoveSourceKind {
    /// Decodes a C-ABI source kind integer (`0..=11`).
    ///
    /// # Arguments
    ///
    /// * `code` - Raw ABI integer code.
    #[must_use]
    pub const fn from_abi_code(code: i32) -> Self {
        match code {
            0 => Self::Field,
            2 => Self::FigZero,
            3 => Self::FigSpace,
            4 => Self::FigHigh,
            5 => Self::FigLow,
            6 => Self::FigQuote,
            7 => Self::FigNull,
            8 => Self::FigAll,
            9 => Self::DecimalAscii,
            10 => Self::NationalLiteral,
            11 => Self::Utf8Literal,
            _ => Self::AlphanumericLiteral,
        }
    }

    /// Encodes this source kind into its C-ABI integer representation.
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::Field => 0,
            Self::AlphanumericLiteral => 1,
            Self::FigZero => 2,
            Self::FigSpace => 3,
            Self::FigHigh => 4,
            Self::FigLow => 5,
            Self::FigQuote => 6,
            Self::FigNull => 7,
            Self::FigAll => 8,
            Self::DecimalAscii => 9,
            Self::NationalLiteral => 10,
            Self::Utf8Literal => 11,
        }
    }
}

/// Binary arithmetic operator evaluated on [`DecimalEvalStack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecimalArithOp {
    /// `lhs + rhs`.
    Add,
    /// `lhs - rhs`.
    Sub,
    /// `lhs * rhs`.
    Mul,
    /// `lhs / rhs` at 38 fractional digits.
    Div,
    /// `lhs ** rhs` at 38 fractional digits.
    Pow,
}

impl DecimalArithOp {
    /// Decodes a C-ABI binary arithmetic opcode (`0..=4`).
    ///
    /// # Arguments
    ///
    /// * `code` - Raw opcode integer.
    #[must_use]
    pub const fn from_abi_code(code: i32) -> Self {
        match code {
            1 => Self::Sub,
            2 => Self::Mul,
            3 => Self::Div,
            4 => Self::Pow,
            _ => Self::Add,
        }
    }

    /// Encodes this binary arithmetic operator into its C-ABI opcode.
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::Add => 0,
            Self::Sub => 1,
            Self::Mul => 2,
            Self::Div => 3,
            Self::Pow => 4,
        }
    }
}

/// Relational operator for runtime decimal and alphanumeric comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelCmpOp {
    /// `=`.
    Eq,
    /// `<>`.
    Ne,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
}

impl RelCmpOp {
    /// Decodes a C-ABI relational operator code (`0..=5`).
    ///
    /// # Arguments
    ///
    /// * `code` - Raw relational opcode.
    #[must_use]
    pub const fn from_abi_code(code: i32) -> Self {
        match code {
            1 => Self::Ne,
            2 => Self::Lt,
            3 => Self::Le,
            4 => Self::Gt,
            5 => Self::Ge,
            _ => Self::Eq,
        }
    }

    /// Encodes this relational operator into its C-ABI integer code.
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::Eq => 0,
            Self::Ne => 1,
            Self::Lt => 2,
            Self::Le => 3,
            Self::Gt => 4,
            Self::Ge => 5,
        }
    }

    /// Evaluates `ord` against this relational operator.
    ///
    /// # Arguments
    ///
    /// * `ord` - [`Ordering`] of `lhs` vs `rhs`.
    #[must_use]
    pub const fn matches_ordering(self, ord: Ordering) -> bool {
        match self {
            Self::Eq => matches!(ord, Ordering::Equal),
            Self::Ne => !matches!(ord, Ordering::Equal),
            Self::Lt => matches!(ord, Ordering::Less),
            Self::Le => matches!(ord, Ordering::Less | Ordering::Equal),
            Self::Gt => matches!(ord, Ordering::Greater),
            Self::Ge => matches!(ord, Ordering::Greater | Ordering::Equal),
        }
    }
}

/// Built-in COBOL class condition kind (`IS NUMERIC` / `IS ALPHABETIC*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClassTestKind {
    /// `IS NUMERIC`.
    Numeric,
    /// `IS ALPHABETIC` (`'A'..='Z'`, `'a'..='z'`, `' '`).
    Alphabetic,
    /// `IS ALPHABETIC-LOWER` (`'a'..='z'`, `' '`).
    AlphabeticLower,
    /// `IS ALPHABETIC-UPPER` (`'A'..='Z'`, `' '`).
    AlphabeticUpper,
}

impl ClassTestKind {
    /// Decodes a C-ABI class condition code (`0..=3`).
    ///
    /// # Arguments
    ///
    /// * `code` - Raw class opcode.
    #[must_use]
    pub const fn from_abi_code(code: i32) -> Self {
        match code {
            1 => Self::Alphabetic,
            2 => Self::AlphabeticLower,
            3 => Self::AlphabeticUpper,
            _ => Self::Numeric,
        }
    }

    /// Encodes this class condition into its C-ABI integer code.
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::Numeric => 0,
            Self::Alphabetic => 1,
            Self::AlphabeticLower => 2,
            Self::AlphabeticUpper => 3,
        }
    }
}

/// Controls whether storing an unrounded `0` into a signed `ZonedDecimal` preserves existing `-0` (`'p'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NegZeroPolicy {
    /// Normalize zero results to `+0` (`'0'` / `'{'` / `'+'`).
    NormalizePositiveZero,
    /// Preserve `-0` (`'p'`) when target already held `-0` and unrounded result is `0`.
    PreserveExistingNegZero,
}

impl NegZeroPolicy {
    /// Decodes a C-ABI negative-zero policy (`0` -> `NormalizePositiveZero`, non-zero -> `PreserveExistingNegZero`).
    ///
    /// # Arguments
    ///
    /// * `code` - Raw ABI policy integer.
    #[must_use]
    pub const fn from_abi_code(code: i32) -> Self {
        if code == 0 {
            Self::NormalizePositiveZero
        } else {
            Self::PreserveExistingNegZero
        }
    }

    /// Encodes this negative-zero policy into its C-ABI integer code.
    #[must_use]
    pub const fn to_abi_code(self) -> i32 {
        match self {
            Self::NormalizePositiveZero => 0,
            Self::PreserveExistingNegZero => 1,
        }
    }
}

/// Decodes a C-ABI rounding mode integer (`0..=7`) into [`RoundingMode`].
///
/// # Arguments
///
/// * `code` - Raw ABI rounding integer.
#[must_use]
pub const fn rounding_from_abi_code(code: i32) -> RoundingMode {
    match code {
        1 => RoundingMode::NearestAwayFromZero,
        2 => RoundingMode::NearestEven,
        3 => RoundingMode::NearestTowardZero,
        4 => RoundingMode::Prohibited,
        5 => RoundingMode::TowardGreater,
        6 => RoundingMode::TowardLesser,
        7 => RoundingMode::AwayFromZero,
        _ => RoundingMode::Truncation,
    }
}

/// Encodes a [`RoundingMode`] into its C-ABI integer code (`0..=7`).
///
/// # Arguments
///
/// * `mode` - Rounding mode to encode.
#[must_use]
pub const fn rounding_to_abi_code(mode: RoundingMode) -> i32 {
    match mode {
        RoundingMode::Truncation => 0,
        RoundingMode::NearestAwayFromZero => 1,
        RoundingMode::NearestEven => 2,
        RoundingMode::NearestTowardZero => 3,
        RoundingMode::Prohibited => 4,
        RoundingMode::TowardGreater => 5,
        RoundingMode::TowardLesser => 6,
        RoundingMode::AwayFromZero => 7,
    }
}

/// Executes an overlap-safe category-aware `MOVE` from `src_bytes` into `dst_bytes`.
///
/// Note: callers at the FFI boundary snapshot `src_bytes` into an owned buffer before
/// creating `&mut dst_bytes`, ensuring aliasing `REDEFINES` / `RefMod` moves are 100% sound.
///
/// # Arguments
///
/// * `src_kind` - [`RuntimeMoveSourceKind`] tag.
/// * `src_bytes` - Snapshot of source operand bytes.
/// * `src_spec_bytes` - Serialized source [`MoveFieldDesc`] when `src_kind == Field`.
/// * `dst_bytes` - Mutable destination storage slice.
/// * `dst_spec_bytes` - Serialized destination [`MoveFieldDesc`].
///
/// # Errors
///
/// Returns [`CoblangError`] if descriptor decoding or `MOVE` conversion fails.
pub fn runtime_execute_move(
    src_kind: RuntimeMoveSourceKind,
    src_bytes: &[u8],
    src_spec_bytes: &[u8],
    dst_bytes: &mut [u8],
    dst_spec_bytes: &[u8],
) -> Result<()> {
    let dst_spec = decode_field_spec(dst_spec_bytes)?;
    match src_kind {
        RuntimeMoveSourceKind::Field => {
            let s_spec = decode_field_spec(src_spec_bytes)?;
            execute_move(
                MoveSource::Field {
                    bytes: src_bytes,
                    desc: &s_spec.desc,
                },
                dst_bytes,
                &dst_spec.desc,
            )
        }
        RuntimeMoveSourceKind::AlphanumericLiteral => execute_move(
            MoveSource::AlphanumericLiteral(src_bytes),
            dst_bytes,
            &dst_spec.desc,
        ),
        RuntimeMoveSourceKind::FigZero => execute_move(
            MoveSource::Figurative(HirFigurative::Zero),
            dst_bytes,
            &dst_spec.desc,
        ),
        RuntimeMoveSourceKind::FigSpace => execute_move(
            MoveSource::Figurative(HirFigurative::Space),
            dst_bytes,
            &dst_spec.desc,
        ),
        RuntimeMoveSourceKind::FigHigh => execute_move(
            MoveSource::Figurative(HirFigurative::HighValue),
            dst_bytes,
            &dst_spec.desc,
        ),
        RuntimeMoveSourceKind::FigLow => execute_move(
            MoveSource::Figurative(HirFigurative::LowValue),
            dst_bytes,
            &dst_spec.desc,
        ),
        RuntimeMoveSourceKind::FigQuote => execute_move(
            MoveSource::Figurative(HirFigurative::Quote),
            dst_bytes,
            &dst_spec.desc,
        ),
        RuntimeMoveSourceKind::FigNull => execute_move(
            MoveSource::Figurative(HirFigurative::Null),
            dst_bytes,
            &dst_spec.desc,
        ),
        RuntimeMoveSourceKind::FigAll => execute_move(
            MoveSource::Figurative(HirFigurative::All(src_bytes.to_vec())),
            dst_bytes,
            &dst_spec.desc,
        ),
        RuntimeMoveSourceKind::DecimalAscii => {
            let text =
                core::str::from_utf8(src_bytes).map_err(|_| CoblangError::DecimalDomain {
                    operation: "runtime_execute_move(DecimalAscii)",
                    message: String::from("non-UTF8 decimal ASCII literal"),
                })?;
            let dec = FixedDecimal::parse_str(text)?;
            execute_move(MoveSource::Decimal(dec), dst_bytes, &dst_spec.desc)
        }
        RuntimeMoveSourceKind::NationalLiteral => {
            let u16_le = swap_utf16be_to_le(src_bytes);
            execute_move(
                MoveSource::NationalLiteralUtf16Le(&u16_le),
                dst_bytes,
                &dst_spec.desc,
            )
        }
        RuntimeMoveSourceKind::Utf8Literal => execute_move(
            MoveSource::Utf8Literal(src_bytes),
            dst_bytes,
            &dst_spec.desc,
        ),
    }
}

/// Converts big-endian UTF-16BE byte pairs (`N"..."` / `NX"..."`) into little-endian UTF-16LE bytes.
///
/// # Arguments
///
/// * `be_bytes` - UTF-16BE byte slice.
#[must_use]
pub fn swap_utf16be_to_le(be_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(be_bytes.len());
    for pair in be_bytes.as_chunks::<2>().0 {
        out.push(pair[1]);
        out.push(pair[0]);
    }
    out
}

/// Borrowed alphanumeric/integer comparison operand bundling source kind, payload bytes, and optional field spec.
pub struct AlphaCmpOperand<'a> {
    /// Source representation kind.
    pub kind: RuntimeMoveSourceKind,
    /// Backing storage or literal bytes.
    pub bytes: &'a [u8],
    /// Optional serialized `RuntimeFieldSpec` (`&[]` when non-field).
    pub spec_bytes: &'a [u8],
}

/// Evaluates ISO COBOL non-numeric / group / figurative / integer-vs-alphanumeric comparison,
/// coercing integer numeric operands to unsigned `USAGE DISPLAY` digits (`14.8.30.1.2`),
/// supporting `NATIONAL` (`UTF-16`) / `UTF-8` cross-charset comparison, and optionally
/// ordering single-byte operands via `collation_ranks`.
///
/// # Arguments
///
/// * `lhs` - Left [`AlphaCmpOperand`].
/// * `rhs` - Right [`AlphaCmpOperand`].
/// * `rel` - Relational operator.
/// * `collation_ranks` - Optional 256-byte ordinal weight table (`None` uses native byte order).
#[must_use]
pub fn runtime_compare_alphanumeric_ex(
    lhs: &AlphaCmpOperand<'_>,
    rhs: &AlphaCmpOperand<'_>,
    rel: RelCmpOp,
    collation_ranks: Option<&[u8; 256]>,
) -> bool {
    if is_unicode_cmp_operand(lhs) || is_unicode_cmp_operand(rhs) {
        let ord = compare_unicode_operands(lhs, rhs);
        return rel.matches_ordering(ord);
    }
    let space_byte = if is_ebcdic_field_operand(lhs) || is_ebcdic_field_operand(rhs) {
        0x40
    } else {
        b' '
    };
    let l_conv = coerce_integer_operand_for_alpha_cmp(lhs);
    let r_conv = coerce_integer_operand_for_alpha_cmp(rhs);
    let lb_eff = l_conv.as_deref().unwrap_or(lhs.bytes);
    let rb_eff = r_conv.as_deref().unwrap_or(rhs.bytes);
    let len = lb_eff.len().max(rb_eff.len()).max(1);
    let mut idx = 0;
    let mut ord = Ordering::Equal;
    while idx < len {
        let lb = alpha_virtual_byte(lhs.kind, lb_eff, idx, space_byte);
        let rb = alpha_virtual_byte(rhs.kind, rb_eff, idx, space_byte);
        let (lw, rw) = if let Some(ranks) = collation_ranks {
            (
                ranks.get(usize::from(lb)).copied().unwrap_or(lb),
                ranks.get(usize::from(rb)).copied().unwrap_or(rb),
            )
        } else {
            (lb, rb)
        };
        if lw != rw {
            ord = lw.cmp(&rw);
            break;
        }
        idx += 1;
    }
    rel.matches_ordering(ord)
}

/// Returns `true` when `op` is a `NATIONAL` or `UTF-8` field or literal.
///
/// # Arguments
///
/// * `op` - Inspected [`AlphaCmpOperand`].
#[must_use]
fn is_unicode_cmp_operand(op: &AlphaCmpOperand<'_>) -> bool {
    match op.kind {
        RuntimeMoveSourceKind::NationalLiteral | RuntimeMoveSourceKind::Utf8Literal => true,
        RuntimeMoveSourceKind::Field if !op.spec_bytes.is_empty() => {
            decode_field_spec(op.spec_bytes).is_ok_and(|s| {
                matches!(
                    s.desc.ty,
                    HirType::National { .. }
                        | HirType::NationalEdited { .. }
                        | HirType::Utf8 { .. }
                )
            })
        }
        _ => false,
    }
}

/// Returns `true` when `op` is a `Field` backed by native EBCDIC storage (`OverpunchDialect::EbcdicIbm`).
///
/// # Arguments
///
/// * `op` - Inspected [`AlphaCmpOperand`].
#[must_use]
fn is_ebcdic_field_operand(op: &AlphaCmpOperand<'_>) -> bool {
    op.kind == RuntimeMoveSourceKind::Field
        && !op.spec_bytes.is_empty()
        && decode_field_spec(op.spec_bytes)
            .is_ok_and(|s| s.desc.overpunch == OverpunchDialect::EbcdicIbm)
}

/// Compares two [`AlphaCmpOperand`]s by Unicode scalar sequence (`U+0020` space-padded).
///
/// # Arguments
///
/// * `lhs` - Left operand.
/// * `rhs` - Right operand.
#[must_use]
fn compare_unicode_operands(lhs: &AlphaCmpOperand<'_>, rhs: &AlphaCmpOperand<'_>) -> Ordering {
    let l_chars = decode_operand_unicode_chars(lhs);
    let r_chars = decode_operand_unicode_chars(rhs);
    let len = l_chars.len().max(r_chars.len()).max(1);
    for idx in 0..len {
        let lc = unicode_virtual_char(lhs.kind, &l_chars, idx);
        let rc = unicode_virtual_char(rhs.kind, &r_chars, idx);
        if lc != rc {
            return lc.cmp(&rc);
        }
    }
    Ordering::Equal
}

/// Decodes `op` into its Unicode `char` sequence for cross-charset comparison.
///
/// # Arguments
///
/// * `op` - Comparison operand.
#[must_use]
fn decode_operand_unicode_chars(op: &AlphaCmpOperand<'_>) -> Vec<char> {
    if let Some(int_digs) = coerce_integer_operand_for_alpha_cmp(op) {
        return int_digs.into_iter().map(char::from).collect();
    }
    match op.kind {
        RuntimeMoveSourceKind::NationalLiteral => {
            let u16_le = swap_utf16be_to_le(op.bytes);
            let utf8 = transcode_bytes(
                &u16_le,
                Codepage::Utf16Le,
                Codepage::Utf8,
                UnmappedCharPolicy::Substitute('\u{FFFD}'),
            )
            .unwrap_or_default();
            String::from_utf8_lossy(&utf8).chars().collect()
        }
        RuntimeMoveSourceKind::Utf8Literal => String::from_utf8_lossy(op.bytes).chars().collect(),
        RuntimeMoveSourceKind::Field if !op.spec_bytes.is_empty() => {
            if let Ok(spec) = decode_field_spec(op.spec_bytes) {
                match spec.desc.ty {
                    HirType::National { .. } | HirType::NationalEdited { .. } => {
                        let utf8 = transcode_bytes(
                            op.bytes,
                            Codepage::Utf16Le,
                            Codepage::Utf8,
                            UnmappedCharPolicy::Substitute('\u{FFFD}'),
                        )
                        .unwrap_or_default();
                        return String::from_utf8_lossy(&utf8).chars().collect();
                    }
                    HirType::Utf8 { .. } => {
                        return String::from_utf8_lossy(op.bytes).chars().collect();
                    }
                    _ if spec.desc.overpunch == OverpunchDialect::EbcdicIbm => {
                        let utf8 = transcode_bytes(
                            op.bytes,
                            Codepage::Cp1140,
                            Codepage::Utf8,
                            UnmappedCharPolicy::Substitute('\u{FFFD}'),
                        )
                        .unwrap_or_default();
                        return String::from_utf8_lossy(&utf8).chars().collect();
                    }
                    _ => {}
                }
            }
            op.bytes.iter().map(|&b| char::from(b)).collect()
        }
        _ => op.bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

/// Returns the Unicode scalar at `idx` for `kind` (`' '` past end of plain strings).
///
/// # Arguments
///
/// * `kind` - Operand kind.
/// * `chars` - Decoded scalar vector.
/// * `idx` - 0-based character index.
#[must_use]
fn unicode_virtual_char(kind: RuntimeMoveSourceKind, chars: &[char], idx: usize) -> char {
    match kind {
        RuntimeMoveSourceKind::FigZero => '0',
        RuntimeMoveSourceKind::FigSpace => ' ',
        RuntimeMoveSourceKind::FigHigh => char::MAX,
        RuntimeMoveSourceKind::FigLow | RuntimeMoveSourceKind::FigNull => '\0',
        RuntimeMoveSourceKind::FigQuote => '"',
        RuntimeMoveSourceKind::FigAll => {
            if chars.is_empty() {
                ' '
            } else {
                chars.get(idx % chars.len()).copied().unwrap_or(' ')
            }
        }
        _ => chars.get(idx).copied().unwrap_or(' '),
    }
}

/// Converts an integer numeric `Field` or signed `DecimalAscii` literal into unsigned ASCII digits for alphanumeric comparison.
///
/// # Arguments
///
/// * `op` - Operand to inspect and convert if integer numeric.
#[must_use]
fn coerce_integer_operand_for_alpha_cmp(op: &AlphaCmpOperand<'_>) -> Option<Vec<u8>> {
    match op.kind {
        RuntimeMoveSourceKind::DecimalAscii => {
            let stripped = op
                .bytes
                .strip_prefix(b"+")
                .or_else(|| op.bytes.strip_prefix(b"-"))
                .unwrap_or(op.bytes);
            Some(stripped.to_vec())
        }
        RuntimeMoveSourceKind::Field if !op.spec_bytes.is_empty() => {
            let spec = decode_field_spec(op.spec_bytes).ok()?;
            let digits = match &spec.desc.ty {
                HirType::ZonedDecimal { sign, .. } => {
                    return strip_zoned_sign_for_alpha_cmp(op.bytes, *sign);
                }
                HirType::PackedDecimal { digits, .. } | HirType::BinaryInt { digits, .. } => {
                    digits.get()
                }
                HirType::Index => 10,
                _ => return None,
            };
            let val = decode_numeric_field(op.bytes, &spec.desc).ok()?;
            let (unscaled, _) = val.abs().rescale(0, RoundingMode::Truncation).ok()?;
            let mag_str = unscaled.to_string();
            let width = usize::from(digits.max(1));
            let mut out = Vec::with_capacity(width);
            if mag_str.len() >= width {
                let start = mag_str.len() - width;
                out.extend_from_slice(mag_str.as_bytes().get(start..).unwrap_or(b"0"));
            } else {
                out.extend(core::iter::repeat_n(b'0', width - mag_str.len()));
                out.extend_from_slice(mag_str.as_bytes());
            }
            Some(out)
        }
        _ => None,
    }
}

/// Normalizes a `ZonedDecimal` field's operational sign for ISO 14.8.30.1.2 alphanumeric comparison,
/// returning `None` when unsigned (raw storage bytes are already unsigned `USAGE DISPLAY` characters).
///
/// # Arguments
///
/// * `bytes` - Stored zoned decimal bytes.
/// * `sign` - Declared [`NumericSignSpec`].
#[must_use]
fn strip_zoned_sign_for_alpha_cmp(bytes: &[u8], sign: NumericSignSpec) -> Option<Vec<u8>> {
    match sign {
        NumericSignSpec::Unsigned => None,
        NumericSignSpec::SeparateLeading => bytes.get(1..).map(<[u8]>::to_vec),
        NumericSignSpec::SeparateTrailing => bytes
            .get(..bytes.len().saturating_sub(1))
            .map(<[u8]>::to_vec),
        NumericSignSpec::EmbeddedLeading => {
            let mut out = bytes.to_vec();
            if let Some(first) = out.first_mut() {
                *first = strip_ascii_overpunch_byte(*first);
            }
            Some(out)
        }
        NumericSignSpec::EmbeddedTrailing => {
            let mut out = bytes.to_vec();
            if let Some(last) = out.last_mut() {
                *last = strip_ascii_overpunch_byte(*last);
            }
            Some(out)
        }
    }
}

/// Strips ASCII/EBCDIC-style overpunch sign encoding from a single zoned byte into `'0'..='9'`,
/// returning `b` unchanged if `b` is not an overpunched digit.
///
/// # Arguments
///
/// * `b` - Candidate sign-bearing byte.
#[must_use]
const fn strip_ascii_overpunch_byte(b: u8) -> u8 {
    match b {
        b'{' | b'}' | b'p' => b'0',
        b'A'..=b'I' => b'1' + (b - b'A'),
        b'J'..=b'R' => b'1' + (b - b'J'),
        b'q'..=b'y' => b'1' + (b - b'q'),
        _ => b,
    }
}

/// Evaluates ISO COBOL non-numeric / group / figurative comparison with space-padded shorter operand.
///
/// # Arguments
///
/// * `lhs` - Left `(kind, bytes)` operand.
/// * `rhs` - Right `(kind, bytes)` operand.
/// * `rel` - Relational operator.
#[must_use]
pub fn runtime_compare_alphanumeric(
    lhs: (RuntimeMoveSourceKind, &[u8]),
    rhs: (RuntimeMoveSourceKind, &[u8]),
    rel: RelCmpOp,
) -> bool {
    runtime_compare_alphanumeric_ex(
        &AlphaCmpOperand {
            kind: lhs.0,
            bytes: lhs.1,
            spec_bytes: &[],
        },
        &AlphaCmpOperand {
            kind: rhs.0,
            bytes: rhs.1,
            spec_bytes: &[],
        },
        rel,
        None,
    )
}

/// Returns the byte at `idx` of a string/field or tiled figurative operand (`space_byte` past end of plain strings).
///
/// # Arguments
///
/// * `kind` - Operand kind.
/// * `bytes` - Backing bytes.
/// * `idx` - Byte index.
/// * `space_byte` - Active space byte (`0x20` ASCII, `0x40` EBCDIC).
#[must_use]
fn alpha_virtual_byte(kind: RuntimeMoveSourceKind, bytes: &[u8], idx: usize, space_byte: u8) -> u8 {
    match kind {
        RuntimeMoveSourceKind::FigZero => {
            if space_byte == 0x40 {
                0xF0
            } else {
                b'0'
            }
        }
        RuntimeMoveSourceKind::FigSpace => space_byte,
        RuntimeMoveSourceKind::FigHigh => 0xFF,
        RuntimeMoveSourceKind::FigLow | RuntimeMoveSourceKind::FigNull => 0x00,
        RuntimeMoveSourceKind::FigQuote => {
            if space_byte == 0x40 {
                0x7F
            } else {
                b'"'
            }
        }
        RuntimeMoveSourceKind::FigAll => {
            if bytes.is_empty() {
                space_byte
            } else {
                bytes.get(idx % bytes.len()).copied().unwrap_or(space_byte)
            }
        }
        RuntimeMoveSourceKind::Field
        | RuntimeMoveSourceKind::AlphanumericLiteral
        | RuntimeMoveSourceKind::DecimalAscii
        | RuntimeMoveSourceKind::NationalLiteral
        | RuntimeMoveSourceKind::Utf8Literal => bytes.get(idx).copied().unwrap_or(space_byte),
    }
}

/// Computes the 0-based byte offset of 1-based UTF-8 character index `one_based_char` inside `bytes`.
///
/// # Arguments
///
/// * `bytes` - UTF-8 byte slice.
/// * `one_based_char` - 1-based Unicode scalar index (`<= 1` returns `0`).
#[must_use]
pub fn utf8_refmod_byte_offset(bytes: &[u8], one_based_char: i64) -> i64 {
    if one_based_char <= 1 {
        return 0;
    }
    let target = usize::try_from(one_based_char - 1).unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(bytes);
    let off = text
        .char_indices()
        .nth(target)
        .map_or(bytes.len(), |(b_idx, _)| b_idx)
        .min(bytes.len());
    i64::try_from(off).unwrap_or(0)
}

/// Computes the byte length of `char_count` UTF-8 scalars starting at `byte_off` inside `bytes`
/// (`char_count < 0` returns remaining bytes `bytes.len() - byte_off`).
///
/// # Arguments
///
/// * `bytes` - UTF-8 byte slice.
/// * `byte_off` - 0-based starting byte offset.
/// * `char_count` - Requested scalar count (`< 0` selects rest of field).
#[must_use]
pub fn utf8_refmod_byte_length(bytes: &[u8], byte_off: i64, char_count: i64) -> i64 {
    let start = usize::try_from(byte_off.max(0))
        .unwrap_or(bytes.len())
        .min(bytes.len());
    let tail = bytes.get(start..).unwrap_or(&[]);
    if char_count < 0 {
        return i64::try_from(tail.len()).unwrap_or(0);
    }
    let n_chars = usize::try_from(char_count).unwrap_or(0);
    if n_chars == 0 {
        return 0;
    }
    let text = String::from_utf8_lossy(tail);
    let len = text
        .char_indices()
        .nth(n_chars)
        .map_or(tail.len(), |(b_idx, _)| b_idx)
        .min(tail.len());
    i64::try_from(len).unwrap_or(0)
}

/// Evaluates `IS NUMERIC`, `IS ALPHABETIC`, `IS ALPHABETIC-LOWER`, or `IS ALPHABETIC-UPPER`.
///
/// # Arguments
///
/// * `bytes` - Stored field bytes.
/// * `spec_bytes` - Serialized [`crate::desc_codec::RuntimeFieldSpec`].
/// * `kind` - Class condition kind.
///
/// # Errors
///
/// Returns [`CoblangError::PictureCodec`] if `spec_bytes` is malformed.
pub fn runtime_check_class(bytes: &[u8], spec_bytes: &[u8], kind: ClassTestKind) -> Result<bool> {
    let spec = decode_field_spec(spec_bytes)?;
    if kind == ClassTestKind::Numeric {
        return Ok(check_field_is_numeric(
            bytes,
            &spec.desc,
            spec.packed_sign_rule,
        ));
    }
    if matches!(
        spec.desc.ty,
        HirType::National { .. } | HirType::NationalEdited { .. }
    ) {
        let utf8 = transcode_bytes(
            bytes,
            Codepage::Utf16Le,
            Codepage::Utf8,
            UnmappedCharPolicy::Substitute('?'),
        )?;
        return Ok(match kind {
            ClassTestKind::Numeric => false,
            ClassTestKind::Alphabetic => utf8.iter().all(|&b| b == b' ' || b.is_ascii_alphabetic()),
            ClassTestKind::AlphabeticLower => {
                utf8.iter().all(|&b| b == b' ' || b.is_ascii_lowercase())
            }
            ClassTestKind::AlphabeticUpper => {
                utf8.iter().all(|&b| b == b' ' || b.is_ascii_uppercase())
            }
        });
    }
    if spec.desc.overpunch == OverpunchDialect::EbcdicIbm {
        let asc = transcode_bytes(
            bytes,
            Codepage::Cp1140,
            Codepage::Iso8859_1,
            UnmappedCharPolicy::Substitute('?'),
        )?;
        return Ok(match kind {
            ClassTestKind::Numeric => false,
            ClassTestKind::Alphabetic => asc.iter().all(|&b| b == b' ' || b.is_ascii_alphabetic()),
            ClassTestKind::AlphabeticLower => {
                asc.iter().all(|&b| b == b' ' || b.is_ascii_lowercase())
            }
            ClassTestKind::AlphabeticUpper => {
                asc.iter().all(|&b| b == b' ' || b.is_ascii_uppercase())
            }
        });
    }
    let sp = spec.desc.overpunch.space_byte();
    Ok(match kind {
        ClassTestKind::Numeric => false,
        ClassTestKind::Alphabetic => bytes.iter().all(|&b| b == sp || b.is_ascii_alphabetic()),
        ClassTestKind::AlphabeticLower => bytes.iter().all(|&b| b == sp || b.is_ascii_lowercase()),
        ClassTestKind::AlphabeticUpper => bytes.iter().all(|&b| b == sp || b.is_ascii_uppercase()),
    })
}

/// Evaluates a `SPECIAL-NAMES. CLASS` condition by testing every byte in `bytes` against a 32-byte (`256`-bit) bitmap.
///
/// # Arguments
///
/// * `bytes` - Subject byte slice.
/// * `bitmap` - 32-byte (`256`-bit) character membership bitmap.
#[must_use]
pub fn runtime_check_custom_class(bytes: &[u8], bitmap: &[u8; 32]) -> bool {
    bytes.iter().all(|&b| {
        let idx = usize::from(b >> 3);
        let bit = b & 7;
        bitmap
            .get(idx)
            .is_some_and(|&mask| ((mask >> bit) & 1) == 1)
    })
}

/// Outcome of [`DecimalEvalStack::divide_remainder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DivRemOutcome {
    /// Updated quotient bytes (`None` when quotient must remain untouched on `SIZE ERROR`).
    pub quotient_bytes: Option<Vec<u8>>,
    /// Updated remainder bytes (`None` when remainder must remain untouched on `SIZE ERROR`).
    pub remainder_bytes: Option<Vec<u8>>,
    /// Resulting size-error status (`Exact`, `Overflowed`, or `DivisionByZero`).
    pub status: SizeErrorStatus,
}

/// Exact 38/76-digit RPN evaluation stack for `ADD`, `SUBTRACT`, `MULTIPLY`, `DIVIDE`, `COMPUTE`, and numeric `IF`.
#[derive(Debug, Default)]
pub struct DecimalEvalStack {
    /// Stack of exact intermediate [`FixedDecimal`] operands.
    values: Vec<FixedDecimal>,
    /// Sticky size-error status latched across expression evaluation (`DivisionByZero`, `Overflowed`, `InvalidExponent`).
    sticky_status: Option<SizeErrorStatus>,
}

impl DecimalEvalStack {
    /// Creates an empty [`DecimalEvalStack`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            values: Vec::new(),
            sticky_status: None,
        }
    }

    /// Resets operands and clears sticky error state before an expression or arithmetic statement.
    pub fn begin(&mut self) {
        self.values.clear();
        self.sticky_status = None;
    }

    /// Decodes a stored COBOL field and pushes its [`FixedDecimal`] value onto the stack.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Stored field bytes.
    /// * `spec_bytes` - Serialized [`crate::desc_codec::RuntimeFieldSpec`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if descriptor decoding or numeric field decoding fails.
    pub fn push_field(&mut self, bytes: &[u8], spec_bytes: &[u8]) -> Result<()> {
        let spec = decode_field_spec(spec_bytes)?;
        let val = decode_numeric_field(bytes, &spec.desc)?;
        self.values.push(val);
        Ok(())
    }

    /// Parses an exact ASCII decimal literal and pushes its [`FixedDecimal`] value onto the stack.
    ///
    /// # Arguments
    ///
    /// * `ascii` - UTF-8 decimal string (`[+|-]digits[.digits]`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `ascii` is not valid UTF-8 or decimal syntax.
    pub fn push_ascii(&mut self, ascii: &[u8]) -> Result<()> {
        let s = core::str::from_utf8(ascii).map_err(|_| CoblangError::DecimalDomain {
            operation: "DecimalEvalStack::push_ascii",
            message: String::from("non-UTF8 decimal constant"),
        })?;
        let val = FixedDecimal::parse_str(s)?;
        self.values.push(val);
        Ok(())
    }

    /// Pushes a pre-computed [`FixedDecimal`] value directly onto the evaluation stack.
    ///
    /// # Arguments
    ///
    /// * `val` - Exact [`FixedDecimal`] operand or intrinsic result.
    pub fn push_decimal(&mut self, val: FixedDecimal) {
        self.values.push(val);
    }

    /// Pops and returns the top [`FixedDecimal`] from the evaluation stack.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if the evaluation stack is empty.
    pub fn pop_decimal(&mut self) -> Result<FixedDecimal> {
        self.pop_one("m12_arg")
    }

    /// Negates the top [`FixedDecimal`] on the stack (`UnaryMinus`).
    pub fn negate_top(&mut self) {
        if let Some(top) = self.values.last_mut() {
            *top = top.negated();
        }
    }

    /// Pops `(lhs, rhs)`, evaluates `op`, latches any size error, and pushes the result.
    ///
    /// # Arguments
    ///
    /// * `op` - Binary arithmetic operator.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] on stack underflow or scale bound error.
    pub fn apply_binop(&mut self, op: DecimalArithOp) -> Result<()> {
        let rhs = self.pop_one("rhs")?;
        let lhs = self.pop_one("lhs")?;
        let (res, st) = match op {
            DecimalArithOp::Add => lhs.checked_add(&rhs),
            DecimalArithOp::Sub => lhs.checked_sub(&rhs),
            DecimalArithOp::Mul => lhs.checked_mul(&rhs),
            DecimalArithOp::Div => lhs.checked_div(
                &rhs,
                FixedDecimal::DEFAULT_DIV_SCALE,
                RoundingMode::Truncation,
            ),
            DecimalArithOp::Pow => lhs.checked_pow(
                &rhs,
                FixedDecimal::DEFAULT_DIV_SCALE,
                RoundingMode::NearestAwayFromZero,
            ),
        }
        .unwrap_or((FixedDecimal::ZERO, SizeErrorStatus::Overflowed));
        if st.is_error() && self.sticky_status.is_none() {
            self.sticky_status = Some(st);
        }
        self.values.push(res);
        Ok(())
    }

    /// Peeks the top [`FixedDecimal`] and stores it into `dst_bytes` with `mode`, `high_order`, and `neg_zero`.
    ///
    /// Leaves the value on the stack so multi-target `GIVING A ROUNDED, B` can store repeatedly.
    ///
    /// # Arguments
    ///
    /// * `dst_bytes` - Destination field bytes.
    /// * `dst_spec_bytes` - Serialized destination [`crate::desc_codec::RuntimeFieldSpec`].
    /// * `mode` - Rounding mode.
    /// * `high_order` - `FlagOverflow` (when `ON SIZE ERROR` present) vs `ModuloTruncate`.
    /// * `neg_zero` - Negative-zero preservation policy for unrounded in-place `ZonedDecimal` `ADD`/`SUBTRACT`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] on empty stack or descriptor error.
    pub fn store_top(
        &self,
        dst_bytes: &mut [u8],
        dst_spec_bytes: &[u8],
        mode: RoundingMode,
        high_order: HighOrderPolicy,
        neg_zero: NegZeroPolicy,
    ) -> Result<SizeErrorStatus> {
        if let Some(err_st) = self.sticky_status {
            return Ok(err_st);
        }
        let val = self
            .values
            .last()
            .ok_or_else(|| CoblangError::DecimalDomain {
                operation: "DecimalEvalStack::store_top",
                message: String::from("empty decimal evaluation stack"),
            })?;
        let spec = decode_field_spec(dst_spec_bytes)?;
        if neg_zero == NegZeroPolicy::PreserveExistingNegZero
            && mode == RoundingMode::Truncation
            && val.is_zero()
            && is_zoned_negative_zero(dst_bytes, &spec.desc)
        {
            return Ok(SizeErrorStatus::Exact);
        }
        store_decimal_to_field(val, dst_bytes, &spec.desc, mode, high_order)
    }

    /// Pops `(dividend, divisor)` and computes ISO COBOL `DIVIDE ... GIVING q [ROUNDED] REMAINDER r`.
    ///
    /// Returns `(new_q_bytes, new_r_bytes, status)` where `None` indicates the target must remain untouched.
    ///
    /// # Arguments
    ///
    /// * `q_init` - Current quotient `(initial_bytes, spec_bytes, rounding_mode)`.
    /// * `r_init` - Current remainder `(initial_bytes, spec_bytes)`.
    /// * `high_order` - `FlagOverflow` vs `ModuloTruncate`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] on stack underflow or descriptor error.
    pub fn divide_remainder(
        &mut self,
        q_init: (&[u8], &[u8], RoundingMode),
        r_init: (&[u8], &[u8]),
        high_order: HighOrderPolicy,
    ) -> Result<DivRemOutcome> {
        let divisor = self.pop_one("divisor")?;
        let dividend = self.pop_one("dividend")?;
        if let Some(err_st) = self.sticky_status {
            return Ok(DivRemOutcome {
                quotient_bytes: None,
                remainder_bytes: None,
                status: err_st,
            });
        }
        let (q_cur, q_spec_bytes, q_mode) = q_init;
        let (r_cur, r_spec_bytes) = r_init;
        let q_spec = decode_field_spec(q_spec_bytes)?;
        let r_spec = decode_field_spec(r_spec_bytes)?;
        let (q_digits, q_scale, q_signed) = extract_quotient_capacity(&q_spec.desc)?;

        let (q_val, rem_val, div_status) = dividend
            .checked_div_rem_cobol(&divisor, (q_digits, q_scale, q_signed), q_mode)
            .unwrap_or((
                FixedDecimal::ZERO,
                FixedDecimal::ZERO,
                SizeErrorStatus::Overflowed,
            ));
        if div_status.is_error() {
            if high_order == HighOrderPolicy::ModuloTruncate
                && div_status == SizeErrorStatus::Overflowed
            {
                let mut q_buf = q_cur.to_vec();
                let _ = store_decimal_to_field(
                    &q_val,
                    &mut q_buf,
                    &q_spec.desc,
                    q_mode,
                    HighOrderPolicy::ModuloTruncate,
                )?;
                return Ok(DivRemOutcome {
                    quotient_bytes: Some(q_buf),
                    remainder_bytes: None,
                    status: div_status,
                });
            }
            return Ok(DivRemOutcome {
                quotient_bytes: None,
                remainder_bytes: None,
                status: div_status,
            });
        }

        let mut q_buf = q_cur.to_vec();
        let q_store_st =
            store_decimal_to_field(&q_val, &mut q_buf, &q_spec.desc, q_mode, high_order)?;
        if q_store_st.is_error() {
            return Ok(DivRemOutcome {
                quotient_bytes: None,
                remainder_bytes: None,
                status: q_store_st,
            });
        }
        let mut r_buf = r_cur.to_vec();
        let r_store_st = store_decimal_to_field(
            &rem_val,
            &mut r_buf,
            &r_spec.desc,
            RoundingMode::Truncation,
            high_order,
        )?;
        if r_store_st.is_error() {
            return Ok(DivRemOutcome {
                quotient_bytes: Some(q_buf),
                remainder_bytes: None,
                status: r_store_st,
            });
        }
        Ok(DivRemOutcome {
            quotient_bytes: Some(q_buf),
            remainder_bytes: Some(r_buf),
            status: SizeErrorStatus::Exact,
        })
    }

    /// Pops `(lhs, rhs)` and evaluates `lhs <rel> rhs` algebraically.
    ///
    /// # Arguments
    ///
    /// * `rel` - Relational operator.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] on stack underflow.
    pub fn compare_top_two(&mut self, rel: RelCmpOp) -> Result<bool> {
        let rhs = self.pop_one("rhs")?;
        let lhs = self.pop_one("lhs")?;
        Ok(rel.matches_ordering(lhs.cmp_numeric(&rhs)))
    }

    /// Pops one value or returns [`CoblangError::DecimalDomain`].
    ///
    /// # Arguments
    ///
    /// * `label` - Operand label for diagnostics.
    fn pop_one(&mut self, label: &'static str) -> Result<FixedDecimal> {
        self.values
            .pop()
            .ok_or_else(|| CoblangError::DecimalDomain {
                operation: label,
                message: String::from("decimal evaluation stack underflow"),
            })
    }
}

/// Extracts `(DigitCount, ScaleFactor, SignednessPolicy)` for `DIVIDE ... REMAINDER` quotient calculation.
///
/// # Arguments
///
/// * `desc` - Quotient field descriptor.
fn extract_quotient_capacity(
    desc: &MoveFieldDesc,
) -> Result<(DigitCount, ScaleFactor, SignednessPolicy)> {
    match &desc.ty {
        HirType::ZonedDecimal {
            digits,
            scale,
            sign,
            ..
        } => Ok((
            *digits,
            *scale,
            if matches!(sign, NumericSignSpec::Unsigned) {
                SignednessPolicy::Unsigned
            } else {
                SignednessPolicy::Signed
            },
        )),
        HirType::PackedDecimal {
            digits,
            scale,
            sign,
            ..
        }
        | HirType::NumericEdited {
            digits,
            scale,
            sign_capability: sign,
            ..
        } => Ok((
            *digits,
            *scale,
            if *sign == Signedness::Signed {
                SignednessPolicy::Signed
            } else {
                SignednessPolicy::Unsigned
            },
        )),
        HirType::BinaryInt {
            byte_width,
            digits,
            scale,
            sign,
            trunc,
            ..
        } => {
            let eff_digits = match trunc {
                coblang_hir::TruncationPolicy::TruncateToPic => *digits,
                coblang_hir::TruncationPolicy::FullBinaryRange => {
                    let hw = match *byte_width {
                        1 => 3,
                        2 => 5,
                        3 => 8,
                        4 => 10,
                        5..=7 => 18,
                        8 => 20,
                        _ => 38,
                    };
                    DigitCount::try_new(hw)?
                }
            };
            Ok((
                eff_digits,
                *scale,
                if *sign == Signedness::Signed {
                    SignednessPolicy::Signed
                } else {
                    SignednessPolicy::Unsigned
                },
            ))
        }
        _ => DigitCount::try_new(38).map(|dc| (dc, ScaleFactor::ZERO, SignednessPolicy::Signed)),
    }
}

/// Staging buffer accumulating formatted `DISPLAY` items without stack `alloca`s.
#[derive(Debug, Default)]
pub struct DisplayStagingBuffer {
    /// Accumulated output bytes for the active `DISPLAY` statement.
    buf: Vec<u8>,
}

impl DisplayStagingBuffer {
    /// Creates an empty [`DisplayStagingBuffer`].
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Clears staged output bytes.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Drains and returns all staged bytes, leaving the staging buffer empty.
    #[must_use]
    pub fn take_bytes(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.buf)
    }

    /// Appends raw literal or alphanumeric slice bytes verbatim.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Raw bytes to append.
    pub fn push_raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Formats a typed COBOL field (`Zoned`, `Packed`, `Binary`, `Edited`, `Pointer`, `Float`) and appends it.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Stored field bytes.
    /// * `spec_bytes` - Serialized [`crate::desc_codec::RuntimeFieldSpec`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if descriptor or numeric decoding fails.
    pub fn push_field(&mut self, bytes: &[u8], spec_bytes: &[u8]) -> Result<()> {
        let spec = decode_field_spec(spec_bytes)?;
        let formatted = format_field_for_display(bytes, &spec.desc)?;
        self.buf.extend_from_slice(&formatted);
        Ok(())
    }

    /// Appends uppercase hexadecimal nibble pairs (`FUNCTION HEX-OF(x)`) for each byte in `bytes`.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Raw byte slice to hex-encode.
    pub fn push_hex_of(&mut self, bytes: &[u8]) {
        /// Uppercase hex alphabet.
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        self.buf.reserve(bytes.len() * 2);
        for &b in bytes {
            let hi = usize::from(b >> 4);
            let lo = usize::from(b & 0x0F);
            self.buf.push(HEX.get(hi).copied().unwrap_or(b'0'));
            self.buf.push(HEX.get(lo).copied().unwrap_or(b'0'));
        }
    }

    /// Writes staged bytes + optional newline to `writer`, flushes `writer`, and clears the staging buffer.
    ///
    /// # Arguments
    ///
    /// * `advancing` - Whether to emit trailing LF (`0x0A`).
    /// * `writer` - Target output stream.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::Io`] if writing or flushing fails.
    pub fn flush_to(&mut self, advancing: LineAdvancing, writer: &mut dyn Write) -> Result<()> {
        if !self.buf.is_empty() {
            writer.write_all(&self.buf).map_err(CoblangError::from)?;
        }
        if advancing == LineAdvancing::AdvanceLine {
            writer.write_all(&[0x0A]).map_err(CoblangError::from)?;
        }
        self.buf.clear();
        writer.flush().map_err(CoblangError::from)
    }
}

/// Validated owned source value inside [`StagedInitReplacement`].
#[derive(Debug, Clone)]
pub enum OwnedReplacementSource {
    /// Stored COBOL field (`bytes` + validated `MoveFieldDesc`).
    Field {
        /// Snapshot of stored field bytes.
        bytes: Vec<u8>,
        /// Decoded field descriptor.
        desc: MoveFieldDesc,
    },
    /// Raw alphanumeric literal bytes.
    AlphanumericLiteral(Vec<u8>),
    /// `NATIONAL` UTF-16LE replacement literal bytes.
    NationalLiteralUtf16Le(Vec<u8>),
    /// `UTF-8` replacement literal bytes.
    Utf8Literal(Vec<u8>),
    /// Figurative constant (`ZERO`, `SPACE`, `HIGH-VALUE`, `LOW-VALUE`, `QUOTE`, `NULL`, `ALL`).
    Figurative(HirFigurative),
    /// Parsed `FixedDecimal` constant.
    Decimal(FixedDecimal),
}

impl OwnedReplacementSource {
    /// Borrows this owned replacement source as a [`MoveSource`].
    #[must_use]
    pub fn as_move_source(&self) -> MoveSource<'_> {
        match self {
            Self::Field { bytes, desc } => MoveSource::Field { bytes, desc },
            Self::AlphanumericLiteral(b) => MoveSource::AlphanumericLiteral(b),
            Self::NationalLiteralUtf16Le(b) => MoveSource::NationalLiteralUtf16Le(b),
            Self::Utf8Literal(b) => MoveSource::Utf8Literal(b),
            Self::Figurative(fig) => MoveSource::Figurative(fig.clone()),
            Self::Decimal(dec) => MoveSource::Decimal(*dec),
        }
    }
}

/// Owned `INITIALIZE ... REPLACING` staging entry.
#[derive(Debug, Clone)]
pub struct StagedInitReplacement {
    /// Target [`DataCategory`].
    pub category: DataCategory,
    /// Validated source operand.
    pub source: OwnedReplacementSource,
}

/// Staging state for `INITIALIZE` `REPLACING` clauses + plan execution.
#[derive(Debug, Default)]
pub struct InitStagingState {
    /// Staged `REPLACING` rules for the next `execute_plan` call.
    replacements: Vec<StagedInitReplacement>,
}

impl InitStagingState {
    /// Creates an empty [`InitStagingState`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            replacements: Vec::new(),
        }
    }

    /// Clears staged `REPLACING` entries.
    pub fn clear(&mut self) {
        self.replacements.clear();
    }

    /// Appends a `REPLACING <category> BY <source>` rule.
    ///
    /// # Arguments
    ///
    /// * `category` - Target [`DataCategory`].
    /// * `kind` - Source operand kind.
    /// * `payload` - Snapshot of source operand bytes.
    /// * `spec_bytes` - Serialized [`crate::desc_codec::RuntimeFieldSpec`] when `kind == Field`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if `kind == Field` and `spec_bytes` is malformed, or `DecimalAscii` is invalid.
    pub fn push_replacement(
        &mut self,
        category: DataCategory,
        kind: RuntimeMoveSourceKind,
        payload: Vec<u8>,
        spec_bytes: &[u8],
    ) -> Result<()> {
        let source = match kind {
            RuntimeMoveSourceKind::Field => OwnedReplacementSource::Field {
                bytes: payload,
                desc: decode_field_spec(spec_bytes)?.desc,
            },
            RuntimeMoveSourceKind::AlphanumericLiteral => {
                OwnedReplacementSource::AlphanumericLiteral(payload)
            }
            RuntimeMoveSourceKind::FigZero => {
                OwnedReplacementSource::Figurative(HirFigurative::Zero)
            }
            RuntimeMoveSourceKind::FigSpace => {
                OwnedReplacementSource::Figurative(HirFigurative::Space)
            }
            RuntimeMoveSourceKind::FigHigh => {
                OwnedReplacementSource::Figurative(HirFigurative::HighValue)
            }
            RuntimeMoveSourceKind::FigLow => {
                OwnedReplacementSource::Figurative(HirFigurative::LowValue)
            }
            RuntimeMoveSourceKind::FigQuote => {
                OwnedReplacementSource::Figurative(HirFigurative::Quote)
            }
            RuntimeMoveSourceKind::FigNull => {
                OwnedReplacementSource::Figurative(HirFigurative::Null)
            }
            RuntimeMoveSourceKind::FigAll => {
                OwnedReplacementSource::Figurative(HirFigurative::All(payload))
            }
            RuntimeMoveSourceKind::DecimalAscii => {
                let text =
                    core::str::from_utf8(&payload).map_err(|_| CoblangError::DecimalDomain {
                        operation: "INITIALIZE REPLACING",
                        message: String::from("non-UTF8 decimal ASCII replacement"),
                    })?;
                OwnedReplacementSource::Decimal(FixedDecimal::parse_str(text)?)
            }
            RuntimeMoveSourceKind::NationalLiteral => {
                OwnedReplacementSource::NationalLiteralUtf16Le(swap_utf16be_to_le(&payload))
            }
            RuntimeMoveSourceKind::Utf8Literal => OwnedReplacementSource::Utf8Literal(payload),
        };
        self.replacements
            .push(StagedInitReplacement { category, source });
        Ok(())
    }

    /// Decodes `plan_bytes`, expands flattened `OCCURS` repetitions, and executes `INITIALIZE` on `buf`.
    ///
    /// # Arguments
    ///
    /// * `buf` - Mutable target storage slice starting at the initialized item's address.
    /// * `plan_bytes` - Serialized [`crate::desc_codec::DecodedInitPlan`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if `plan_bytes` is malformed or any target slice lies out of bounds.
    pub fn execute_plan(&mut self, buf: &mut [u8], plan_bytes: &[u8]) -> Result<()> {
        let plan = decode_init_plan(plan_bytes)?;
        let borrowed_reps: Vec<(DataCategory, MoveSource<'_>)> = self
            .replacements
            .iter()
            .map(|rep| (rep.category, rep.source.as_move_source()))
            .collect();

        let spec = InitializeSpec {
            filler_policy: plan.filler_policy,
            to_value_mask: plan.to_value_mask,
            replacements: &borrowed_reps,
            default_mode: plan.default_mode,
        };

        let mut res = Ok(());
        for entry in &plan.entries {
            if let Err(e) = apply_init_entry_streaming(buf, entry, &spec) {
                res = Err(e);
                break;
            }
        }
        self.replacements.clear();
        res
    }
}

/// Applies one [`crate::desc_codec::RuntimeInitEntry`] across all `entry.dims` occurrences with zero heap allocation
/// (computing the element pattern ONCE and `copy_from_slice`ing across repeated occurrences!).
///
/// # Arguments
///
/// * `buf` - Mutable target buffer.
/// * `entry` - Decoded elementary initialization entry.
/// * `spec` - Active [`InitializeSpec`].
fn apply_init_entry_streaming(
    buf: &mut [u8],
    entry: &crate::desc_codec::RuntimeInitEntry,
    spec: &InitializeSpec<'_>,
) -> Result<()> {
    let elem_len = entry.byte_len as usize;
    let first_off = entry.offset as usize;
    let first_end = first_off.saturating_add(elem_len);
    if first_end > buf.len() {
        return Err(CoblangError::PictureCodec {
            picture: String::from("INITIALIZE"),
            message: format!(
                "INITIALIZE target slice {first_off}..{first_end} exceeds buffer length {}",
                buf.len()
            ),
        });
    }
    let is_filler = entry.filler == FillerMembership::FillerField;
    let is_redefined = entry.redefines == RedefinesMembership::RedefinesAlias;
    if is_redefined || (is_filler && spec.filler_policy == coblang_pic::FillerPolicy::SkipFiller) {
        return Ok(());
    }

    // Initialize the first element in place using `execute_initialize`, then if `entry.dims` repeats
    // AND the element was actually modified (not a skipped pointer/index), memcpy that exact `elem_len`
    // pattern across all remaining occurrences in `O(1)` per occurrence!
    let before_first = buf.get(first_off..first_end).map_or_default(<[u8]>::to_vec);
    let single = [InitTargetField {
        offset: first_off,
        byte_len: elem_len,
        desc: entry.desc.clone(),
        is_filler,
        is_redefined,
        value_bytes: entry.value_bytes.as_deref(),
    }];
    execute_initialize(buf, &single, spec)?;

    if entry.dims.is_empty() {
        return Ok(());
    }
    let pattern = buf.get(first_off..first_end).map_or_default(<[u8]>::to_vec);
    // Skip untouched categories (e.g. `Pointer`/`Index` under `StandardCobol` without `REPLACING`):
    if pattern == before_first
        && matches!(
            entry.desc.category(),
            DataCategory::Pointer | DataCategory::Index | DataCategory::ObjectReference
        )
        && spec.default_mode == coblang_pic::DefaultInitMode::StandardCobol
    {
        return Ok(());
    }
    splat_pattern_across_dims(buf, first_off, &entry.dims, &pattern, true)
}

/// Copies `pattern` across every multi-dimensional occurrence in `dims` (skipping the already-written `(0,0,...)` origin).
///
/// # Arguments
///
/// * `buf` - Mutable target buffer.
/// * `base_off` - Current dimension byte offset.
/// * `dims` - Remaining `(count, stride)` pairs.
/// * `pattern` - Pre-formatted elementary byte pattern.
/// * `is_origin` - `true` when all outer indices so far are `0`.
fn splat_pattern_across_dims(
    buf: &mut [u8],
    base_off: usize,
    dims: &[(u32, u32)],
    pattern: &[u8],
    is_origin: bool,
) -> Result<()> {
    if let Some((&(cnt, stride), rest)) = dims.split_first() {
        let n = cnt.max(1) as usize;
        let st = stride as usize;
        for i in 0..n {
            let off = base_off.saturating_add(i.saturating_mul(st));
            splat_pattern_across_dims(buf, off, rest, pattern, is_origin && i == 0)?;
        }
        Ok(())
    } else if is_origin {
        Ok(())
    } else {
        let total_len = buf.len();
        let end = base_off.saturating_add(pattern.len());
        let dst = buf
            .get_mut(base_off..end)
            .ok_or_else(|| CoblangError::PictureCodec {
                picture: String::from("INITIALIZE"),
                message: format!(
                    "INITIALIZE table element {base_off}..{end} exceeds buffer length {total_len}"
                ),
            })?;
        dst.copy_from_slice(pattern);
        Ok(())
    }
}

/// Severity disposition when a runtime `EC-BOUND-*` check detects an out-of-bounds value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeBoundFailureMode {
    /// Non-fatal ISO `>>TURN` check: record `EC-BOUND-*` in run-unit state and continue execution.
    RecordOnly,
    /// Fatal `-debug` / `$SET BOUND` / `$SET SSRANGE` check: emit stderr diagnostic and terminate with exit `1`.
    FatalAbort,
}

/// Result of evaluating a runtime `EC-BOUND-*` check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoundCheckDisposition {
    /// Tested value satisfied all declared bounds.
    Pass,
    /// Out-of-bounds value recorded non-fatally under [`RuntimeBoundFailureMode::RecordOnly`].
    RecordedNonFatal(&'static str),
    /// Out-of-bounds value formatted to `err_out` under [`RuntimeBoundFailureMode::FatalAbort`].
    FatalAbortExitOne(&'static str),
}

/// Optional explicit length operand for runtime `(offset : [length])` reference-modification checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefModLengthArg {
    /// `(offset :)` — length defaults to remaining units through the end of the item.
    Omitted,
    /// `(offset : length)` — explicit 1-based length operand.
    Explicit(i64),
}

/// Formats and classifies a detected runtime `EC-BOUND-*` violation.
///
/// # Arguments
///
/// * `ec_name` - Canonical ISO `EC-BOUND-*` name.
/// * `diag_line` - Formatted diagnostic line (including trailing newline).
/// * `mode` - Non-fatal record vs fatal abort policy.
/// * `err_out` - Diagnostic writer used when `mode == RuntimeBoundFailureMode::FatalAbort`.
fn report_bound_violation(
    ec_name: &'static str,
    diag_line: &str,
    mode: RuntimeBoundFailureMode,
    err_out: &mut dyn Write,
) -> BoundCheckDisposition {
    match mode {
        RuntimeBoundFailureMode::RecordOnly => BoundCheckDisposition::RecordedNonFatal(ec_name),
        RuntimeBoundFailureMode::FatalAbort => {
            let _ = err_out.write_all(diag_line.as_bytes());
            let _ = err_out.flush();
            BoundCheckDisposition::FatalAbortExitOne(ec_name)
        }
    }
}

/// Validates an `OCCURS min TO max TIMES DEPENDING ON odo` object value against `min..=max` (`EC-BOUND-ODO`).
///
/// # Arguments
///
/// * `odo_val` - Current runtime integer value of the `DEPENDING ON` item.
/// * `min_val` - Minimum declared occurrence count (`>= 0`).
/// * `max_val` - Maximum declared occurrence count (`>= min_val`).
/// * `mode` - Non-fatal record vs fatal abort policy.
/// * `loc_str` - Source `file:line` location prefix.
/// * `err_out` - Diagnostic stream for fatal aborts.
pub fn evaluate_odo_bound(
    odo_val: i64,
    min_val: i64,
    max_val: i64,
    mode: RuntimeBoundFailureMode,
    loc_str: &str,
    err_out: &mut dyn Write,
) -> BoundCheckDisposition {
    if odo_val >= min_val && odo_val <= max_val {
        BoundCheckDisposition::Pass
    } else {
        let line = format!(
            "{loc_str}: error: OCCURS DEPENDING ON value {odo_val} out of bounds {min_val}..={max_val} (EC-BOUND-ODO)\n"
        );
        report_bound_violation("EC-BOUND-ODO", &line, mode, err_out)
    }
}

/// Validates a 1-based table subscript or index occurrence number against `1..=max_val` (`EC-BOUND-SUBSCRIPT`).
///
/// # Arguments
///
/// * `idx_val` - Evaluated 1-based subscript value.
/// * `max_val` - Active upper occurrence bound (`dim.max` or active `DEPENDING ON` count).
/// * `mode` - Non-fatal record vs fatal abort policy.
/// * `loc_str` - Source `file:line` location prefix.
/// * `err_out` - Diagnostic stream for fatal aborts.
pub fn evaluate_subscript_bound(
    idx_val: i64,
    max_val: i64,
    mode: RuntimeBoundFailureMode,
    loc_str: &str,
    err_out: &mut dyn Write,
) -> BoundCheckDisposition {
    if idx_val >= 1 && idx_val <= max_val {
        BoundCheckDisposition::Pass
    } else {
        let (file_line, item_name) = loc_str.split_once('\t').unwrap_or((loc_str, "?"));
        let line = format!(
            "libcob: {file_line}: error: subscript of '{item_name}' out of bounds: {idx_val}\nnote: maximum subscript for '{item_name}': {max_val}\n"
        );
        report_bound_violation("EC-BOUND-SUBSCRIPT", &line, mode, err_out)
    }
}

/// Validates a 1-based `(offset : [length])` reference modification slice against `base_len` (`EC-BOUND-REF-MOD`).
///
/// # Arguments
///
/// * `base_len` - Active item size in character units (`>= 0`).
/// * `offset` - 1-based start offset.
/// * `len_arg` - Optional explicit length operand.
/// * `mode` - Non-fatal record vs fatal abort policy.
/// * `loc_str` - Source `file:line` location prefix.
/// * `err_out` - Diagnostic stream for fatal aborts.
pub fn evaluate_refmod_bound(
    base_len: i64,
    offset: i64,
    len_arg: RefModLengthArg,
    mode: RuntimeBoundFailureMode,
    loc_str: &str,
    err_out: &mut dyn Write,
) -> BoundCheckDisposition {
    let offset_bad = offset < 1 || offset > base_len;
    let len_bad = match len_arg {
        RefModLengthArg::Omitted => false,
        RefModLengthArg::Explicit(len) => {
            len < 1 || i128::from(offset) - 1 + i128::from(len) > i128::from(base_len)
        }
    };
    if !offset_bad && !len_bad {
        BoundCheckDisposition::Pass
    } else {
        let line = format!(
            "{loc_str}: error: reference modification out of bounds for length {base_len} (EC-BOUND-REF-MOD)\n"
        );
        report_bound_violation("EC-BOUND-REF-MOD", &line, mode, err_out)
    }
}

/// Validates that a `BASED` or `LINKAGE` base pointer is non-null before dereferencing (`EC-DATA-PTR-NULL`).
///
/// # Arguments
///
/// * `addr` - Base pointer address cast to `usize` (`0` = `NULL`).
/// * `loc_str` - Source `file:line` location text.
/// * `root_name` - Original name of the `01`/`77` `BASED` or `LINKAGE` root item.
/// * `item_name` - Original name of the accessed item (`""` or equal to `root_name` when accessing root directly).
/// * `err_out` - Diagnostic writer receiving the fatal error line.
pub fn evaluate_based_pointer(
    addr: usize,
    loc_str: &str,
    root_name: &str,
    item_name: &str,
    err_out: &mut dyn Write,
) -> BoundCheckDisposition {
    if addr != 0 {
        return BoundCheckDisposition::Pass;
    }
    let line = if let Some(lk_root) = root_name.strip_prefix("L:") {
        if item_name.is_empty() || item_name == lk_root {
            format!("libcob: {loc_str}: error: LINKAGE item '{lk_root}' not passed by caller\n")
        } else {
            format!(
                "libcob: {loc_str}: error: LINKAGE item '{lk_root}' (accessed by '{item_name}') not passed by caller\n"
            )
        }
    } else if item_name.is_empty() || item_name == root_name {
        format!("libcob: {loc_str}: error: BASED/LINKAGE item '{root_name}' has NULL address\n")
    } else {
        format!(
            "libcob: {loc_str}: error: BASED/LINKAGE item '{root_name}' (accessed by '{item_name}') has NULL address\n"
        )
    };
    report_bound_violation(
        "EC-DATA-PTR-NULL",
        &line,
        RuntimeBoundFailureMode::FatalAbort,
        err_out,
    )
}

/// Sorts `count` contiguous elements of an in-memory COBOL table in `buf` stably according to `plan_bytes`.
///
/// # Arguments
///
/// * `buf` - Mutable table storage starting at element `1`.
/// * `count` - Active occurrence count (`0..=max`).
/// * `plan_bytes` - Serialized [`DecodedTableSortPlan`].
///
/// # Errors
///
/// Returns [`CoblangError`] if `plan_bytes` is malformed or `count * stride > buf.len()`.
pub fn runtime_sort_table(buf: &mut [u8], count: usize, plan_bytes: &[u8]) -> Result<()> {
    let plan: DecodedTableSortPlan = decode_table_sort_plan(plan_bytes)?;
    let stride = plan.stride.max(1) as usize;
    let required = count.saturating_mul(stride);
    if required > buf.len() {
        return Err(CoblangError::SortMerge {
            message: format!(
                "table SORT range 0..{required} exceeds buffer length {}",
                buf.len()
            ),
        });
    }
    if count <= 1 || plan.keys.is_empty() {
        return Ok(());
    }

    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by(|&a_idx, &b_idx| {
        let a_elem = buf.get(a_idx * stride..(a_idx + 1) * stride).unwrap_or(&[]);
        let b_elem = buf.get(b_idx * stride..(b_idx + 1) * stride).unwrap_or(&[]);
        compare_table_elements(a_elem, b_elem, &plan.keys, plan.collating_ranks.as_ref())
    });

    let mut sorted = Vec::with_capacity(required);
    for idx in order {
        sorted.extend_from_slice(buf.get(idx * stride..(idx + 1) * stride).unwrap_or(&[]));
    }
    for (dst_b, src_b) in buf.iter_mut().zip(sorted) {
        *dst_b = src_b;
    }
    Ok(())
}

/// Compares two table element slices across `keys` in lexicographic precedence order.
///
/// # Arguments
///
/// * `a_elem` - Left element byte slice.
/// * `b_elem` - Right element byte slice.
/// * `keys` - Ordered [`RuntimeTableSortKey`] slice.
/// * `collating_ranks` - Optional 256-byte ordinal collation rank table.
fn compare_table_elements(
    a_elem: &[u8],
    b_elem: &[u8],
    keys: &[RuntimeTableSortKey],
    collating_ranks: Option<&[u8; 256]>,
) -> Ordering {
    for key in keys {
        let start = key.rel_offset as usize;
        let end = start.saturating_add(key.byte_len as usize);
        let a_slice = a_elem.get(start..end).unwrap_or(&[]);
        let b_slice = b_elem.get(start..end).unwrap_or(&[]);
        let raw_ord = if matches!(
            key.spec.desc.category(),
            DataCategory::Numeric | DataCategory::Index
        ) {
            let av = decode_numeric_field(a_slice, &key.spec.desc).unwrap_or(FixedDecimal::ZERO);
            let bv = decode_numeric_field(b_slice, &key.spec.desc).unwrap_or(FixedDecimal::ZERO);
            av.cmp_numeric(&bv)
        } else {
            compare_equal_len_bytes(a_slice, b_slice, collating_ranks)
        };
        let directed = match key.direction {
            HirSortDirection::Ascending => raw_ord,
            HirSortDirection::Descending => raw_ord.reverse(),
        };
        if directed != Ordering::Equal {
            return directed;
        }
    }
    Ordering::Equal
}

/// Compares two equal-length alphanumeric/group slices using optional `collating_ranks`.
///
/// # Arguments
///
/// * `lhs` - Left key byte slice.
/// * `rhs` - Right key byte slice.
/// * `collating_ranks` - Optional 256-byte ordinal weight table.
fn compare_equal_len_bytes(
    lhs: &[u8],
    rhs: &[u8],
    collating_ranks: Option<&[u8; 256]>,
) -> Ordering {
    for (&lb, &rb) in lhs.iter().zip(rhs.iter()) {
        let (lw, rw) = if let Some(ranks) = collating_ranks {
            (
                ranks.get(usize::from(lb)).copied().unwrap_or(lb),
                ranks.get(usize::from(rb)).copied().unwrap_or(rb),
            )
        } else {
            (lb, rb)
        };
        if lw != rw {
            return lw.cmp(&rw);
        }
    }
    Ordering::Equal
}
