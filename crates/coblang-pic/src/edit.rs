//! Strongly-typed `PictureProgram` bytecode VM unifying alphanumeric-edited, numeric-edited
//! (including `DecimalPointKind::Comma` and multi-byte `CURRENCY SIGN`), national-edited,
//! UTF-8-edited formatting, and numeric de-editing.

use coblang_common::{CoblangError, DigitCount, Result, ScaleFactor};
use coblang_decimal::{
    FixedDecimal, HighOrderPolicy, RoundingMode, SignednessPolicy, SizeErrorStatus, Uint256,
};
use coblang_hir::{BlankZeroSpec, PicEditOp, Signedness};

use crate::de_edit::de_edit_to_decimal_with_currency;
use crate::edit_alpha::format_alphanumeric_edited;
use crate::move_matrix::{
    Justification, pad_or_truncate_national_utf16be, pad_or_truncate_national_utf16le,
    pad_or_truncate_utf8,
};

/// `SPECIAL-NAMES` decimal-point convention (`DECIMAL-POINT IS COMMA` vs standard period).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecimalPointKind {
    /// Standard period decimal point (`'.'`), comma grouping separator (`','`).
    Period,
    /// `DECIMAL-POINT IS COMMA` (`','` decimal point, `'.'` grouping separator).
    Comma,
}

impl DecimalPointKind {
    /// Returns the active decimal separator ASCII byte (`b'.'` or `b','`).
    #[must_use]
    pub const fn decimal_byte(self) -> u8 {
        match self {
            Self::Period => b'.',
            Self::Comma => b',',
        }
    }

    /// Returns the active digit-grouping separator ASCII byte (`b','` or `b'.'`).
    #[must_use]
    pub const fn group_sep_byte(self) -> u8 {
        match self {
            Self::Period => b',',
            Self::Comma => b'.',
        }
    }

    /// Classifies a raw decimal separator byte (`b','` -> [`Self::Comma`], otherwise [`Self::Period`]).
    ///
    /// # Arguments
    ///
    /// * `byte` - Decimal point ASCII byte.
    #[must_use]
    pub const fn from_decimal_byte(byte: u8) -> Self {
        if byte == b',' {
            Self::Comma
        } else {
            Self::Period
        }
    }
}

/// Validated `CURRENCY SIGN` specification supporting single- and multi-byte currency strings
/// (`"$"`, `"EUR"`, `"CHF"`, `"€"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CurrencySignSpec {
    /// `PICTURE` symbol character representing this currency sign (`'$'` by default).
    picture_symbol: char,
    /// Non-empty formatted display string emitted at fixed or floating currency positions.
    currency_str: String,
}

impl CurrencySignSpec {
    /// Constructs the default ISO COBOL `'$'` -> `"$"` currency specification.
    #[must_use]
    pub fn standard_dollar() -> Self {
        Self {
            picture_symbol: '$',
            currency_str: String::from("$"),
        }
    }

    /// Constructs a validated [`CurrencySignSpec`].
    ///
    /// # Arguments
    ///
    /// * `picture_symbol` - Single character used in `PICTURE` strings (e.g. `'$'`).
    /// * `currency_str` - Non-empty display currency string (e.g. `"EUR"`, `"€"`, `"$"`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::PictureCodec`] if `currency_str` is empty.
    pub fn try_new(picture_symbol: char, currency_str: &str) -> Result<Self> {
        if currency_str.is_empty() {
            return Err(CoblangError::PictureCodec {
                picture: String::from("CURRENCY SIGN"),
                message: String::from("currency sign display string cannot be empty"),
            });
        }
        Ok(Self {
            picture_symbol,
            currency_str: String::from(currency_str),
        })
    }

    /// Returns the `PICTURE` symbol character.
    #[must_use]
    pub const fn picture_symbol(&self) -> char {
        self.picture_symbol
    }

    /// Returns the non-empty formatted currency string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.currency_str
    }

    /// Returns the UTF-8 / display bytes of the formatted currency string.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.currency_str.as_bytes()
    }
}

/// Numeric capacity, scale, sign, and zero-blanking metadata for [`PictureProgramKind::NumericEdited`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NumericPictureMeta {
    /// Total numeric digit positions (`1..=38`).
    pub digits: DigitCount,
    /// Decimal scale factor (`-38..=38`).
    pub scale: ScaleFactor,
    /// Whether the picture contains a sign symbol (`+`, `-`, `CR`, `DB`).
    pub sign_capability: Signedness,
    /// `BLANK WHEN ZERO` policy.
    pub blank_zero: BlankZeroSpec,
}

/// Editing category executed by a [`PictureProgram`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PictureProgramKind {
    /// Alphanumeric-edited (`A`, `X`, `9`, `B`, `0`, `/`).
    AlphanumericEdited,
    /// Numeric-edited (`Z`, `*`, `$$`, `++`, `--`, `+`, `-`, `CR`, `DB`, `,`, `.`, `B`, `0`, `/`).
    NumericEdited(NumericPictureMeta),
    /// National-edited UTF-16 (`N`, `B`, `0`, `/`).
    NationalEdited,
    /// UTF-8-edited (`U`, `B`, `0`, `/`).
    Utf8Edited,
}

/// Compiled, strongly-typed `PICTURE` bytecode program and virtual machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureProgram {
    /// Non-empty sequence of elementary `PICTURE` opcodes.
    ops: Vec<PicEditOp>,
    /// Program editing category and numeric metadata.
    kind: PictureProgramKind,
    /// Decimal/grouping separator mode (`Period` vs `Comma`).
    decimal_point: DecimalPointKind,
    /// Single- or multi-byte currency specification.
    currency: CurrencySignSpec,
}

impl PictureProgram {
    /// Compiles an alphanumeric-edited [`PictureProgram`].
    ///
    /// # Arguments
    ///
    /// * `ops` - Non-empty `PicEditOp` bytecode vector.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::PictureCodec`] if `ops` is empty.
    pub fn try_alphanumeric(ops: Vec<PicEditOp>) -> Result<Self> {
        validate_non_empty_ops(&ops)?;
        Ok(Self {
            ops,
            kind: PictureProgramKind::AlphanumericEdited,
            decimal_point: DecimalPointKind::Period,
            currency: CurrencySignSpec::standard_dollar(),
        })
    }

    /// Compiles a numeric-edited [`PictureProgram`].
    ///
    /// # Arguments
    ///
    /// * `ops` - Non-empty `PicEditOp` bytecode vector.
    /// * `meta` - Numeric digit count, scale, sign capability, and `BLANK WHEN ZERO` policy.
    /// * `decimal_point` - [`DecimalPointKind::Period`] or [`DecimalPointKind::Comma`].
    /// * `currency` - [`CurrencySignSpec`] (single- or multi-byte `CURRENCY SIGN`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::PictureCodec`] if `ops` is empty.
    pub fn try_numeric(
        ops: Vec<PicEditOp>,
        meta: NumericPictureMeta,
        decimal_point: DecimalPointKind,
        currency: CurrencySignSpec,
    ) -> Result<Self> {
        validate_non_empty_ops(&ops)?;
        Ok(Self {
            ops,
            kind: PictureProgramKind::NumericEdited(meta),
            decimal_point,
            currency,
        })
    }

    /// Compiles a `NATIONAL-EDITED` (`UTF-16`) [`PictureProgram`].
    ///
    /// # Arguments
    ///
    /// * `ops` - Non-empty `PicEditOp` bytecode vector.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::PictureCodec`] if `ops` is empty.
    pub fn try_national(ops: Vec<PicEditOp>) -> Result<Self> {
        validate_non_empty_ops(&ops)?;
        Ok(Self {
            ops,
            kind: PictureProgramKind::NationalEdited,
            decimal_point: DecimalPointKind::Period,
            currency: CurrencySignSpec::standard_dollar(),
        })
    }

    /// Compiles a `UTF-8` edited [`PictureProgram`].
    ///
    /// # Arguments
    ///
    /// * `ops` - Non-empty `PicEditOp` bytecode vector.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::PictureCodec`] if `ops` is empty.
    pub fn try_utf8(ops: Vec<PicEditOp>) -> Result<Self> {
        validate_non_empty_ops(&ops)?;
        Ok(Self {
            ops,
            kind: PictureProgramKind::Utf8Edited,
            decimal_point: DecimalPointKind::Period,
            currency: CurrencySignSpec::standard_dollar(),
        })
    }

    /// Returns the compiled `PicEditOp` slice.
    #[must_use]
    pub fn ops(&self) -> &[PicEditOp] {
        &self.ops
    }

    /// Returns the [`PictureProgramKind`] of this program.
    #[must_use]
    pub const fn kind(&self) -> PictureProgramKind {
        self.kind
    }

    /// Returns the [`DecimalPointKind`] of this program.
    #[must_use]
    pub const fn decimal_point(&self) -> DecimalPointKind {
        self.decimal_point
    }

    /// Returns the [`CurrencySignSpec`] of this program.
    #[must_use]
    pub const fn currency(&self) -> &CurrencySignSpec {
        &self.currency
    }

    /// Computes the exact formatted output byte length of this [`PictureProgram`] (assuming single-byte UTF-8 scalars for `Utf8Edited`).
    #[must_use]
    pub fn display_byte_len(&self) -> usize {
        match self.kind {
            PictureProgramKind::AlphanumericEdited => self
                .ops
                .iter()
                .filter(|op| {
                    matches!(
                        op,
                        PicEditOp::AlphaSlot
                            | PicEditOp::AnyCharSlot
                            | PicEditOp::DigitSlot
                            | PicEditOp::InsertBlank
                            | PicEditOp::InsertZero
                            | PicEditOp::InsertSlash
                    )
                })
                .count(),
            PictureProgramKind::NationalEdited => self
                .ops
                .iter()
                .filter(|op| {
                    matches!(
                        op,
                        PicEditOp::NationalSlot
                            | PicEditOp::InsertBlank
                            | PicEditOp::InsertZero
                            | PicEditOp::InsertSlash
                    )
                })
                .count()
                .saturating_mul(2),
            PictureProgramKind::Utf8Edited => self
                .ops
                .iter()
                .filter(|op| {
                    matches!(
                        op,
                        PicEditOp::Utf8Slot
                            | PicEditOp::AlphaSlot
                            | PicEditOp::AnyCharSlot
                            | PicEditOp::DigitSlot
                            | PicEditOp::InsertBlank
                            | PicEditOp::InsertZero
                            | PicEditOp::InsertSlash
                    )
                })
                .count(),
            PictureProgramKind::NumericEdited(_) => {
                let cur_len = self.currency.as_bytes().len();
                let mut seen_floating_cur = false;
                let mut total = 0_usize;
                for op in &self.ops {
                    total += match op {
                        PicEditOp::FixedCreditCr | PicEditOp::FixedDebitDb => 2,
                        PicEditOp::FixedCurrency(_) => cur_len,
                        PicEditOp::FloatingCurrency(_) if !seen_floating_cur => {
                            seen_floating_cur = true;
                            cur_len
                        }
                        PicEditOp::DigitSlot
                        | PicEditOp::ZeroSuppressSpace
                        | PicEditOp::ZeroSuppressAsterisk
                        | PicEditOp::FloatingSignPlus
                        | PicEditOp::FloatingSignMinus
                        | PicEditOp::FloatingCurrency(_)
                        | PicEditOp::InsertDecimalPoint
                        | PicEditOp::InsertGroupSep
                        | PicEditOp::InsertBlank
                        | PicEditOp::InsertZero
                        | PicEditOp::InsertSlash
                        | PicEditOp::FixedSignPlus
                        | PicEditOp::FixedSignMinus => 1,
                        _ => 0,
                    };
                }
                total
            }
        }
    }

    /// Executes alphanumeric-edited formatting (`A`, `X`, `9` slots with `B`, `0`, `/` insertions).
    ///
    /// # Arguments
    ///
    /// * `src` - Source single-byte character slice.
    /// * `justification` - [`Justification::Left`] or [`Justification::Right`].
    /// * `space_byte` - Active space byte (`0x20` ASCII, `0x40` EBCDIC).
    #[must_use]
    pub fn format_alphanumeric(
        &self,
        src: &[u8],
        justification: Justification,
        space_byte: u8,
    ) -> Vec<u8> {
        format_alphanumeric_edited(src, &self.ops, justification.to_hir(), space_byte)
    }

    /// Executes numeric-edited formatting (`Z`, `*`, `$$`, `++`, `--`, `+`, `-`, `CR`, `DB`, `,`, `.`,
    /// multi-byte `CURRENCY SIGN`, `BLANK WHEN ZERO`).
    ///
    /// # Arguments
    ///
    /// * `value` - Exact source [`FixedDecimal`].
    /// * `rounding` - Fractional [`RoundingMode`].
    /// * `high_order` - High-order overflow [`HighOrderPolicy`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::PictureCodec`] if `self.kind()` is not [`PictureProgramKind::NumericEdited`],
    /// or a decimal rescaling error if scale bounds are violated.
    pub fn format_numeric(
        &self,
        value: &FixedDecimal,
        rounding: RoundingMode,
        high_order: HighOrderPolicy,
    ) -> Result<(Vec<u8>, SizeErrorStatus)> {
        let PictureProgramKind::NumericEdited(meta) = self.kind else {
            return Err(CoblangError::PictureCodec {
                picture: String::from("PictureProgram::format_numeric"),
                message: format!("expected NumericEdited program, found {:?}", self.kind),
            });
        };
        execute_numeric_vm(
            value,
            &self.ops,
            meta,
            self.decimal_point,
            self.currency.as_bytes(),
            rounding,
            high_order,
            self.display_byte_len(),
        )
    }

    /// Executes `NATIONAL-EDITED` formatting into `UTF-16LE` bytes (`[ch, 0x00]`).
    ///
    /// # Arguments
    ///
    /// * `src_utf16le` - Source `UTF-16LE` byte slice (must have even length).
    /// * `justification` - [`Justification::Left`] or [`Justification::Right`] applied to `N` slots.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::EncodingConversion`] if `src_utf16le.len()` is odd.
    pub fn format_national_utf16le(
        &self,
        src_utf16le: &[u8],
        justification: Justification,
    ) -> Result<Vec<u8>> {
        let slot_count = self
            .ops
            .iter()
            .filter(|op| matches!(op, PicEditOp::NationalSlot))
            .count();
        let aligned = pad_or_truncate_national_utf16le(src_utf16le, slot_count, justification)?;
        let (pairs, _) = aligned.as_chunks::<2>();
        let mut pair_iter = pairs.iter();
        let mut out = Vec::with_capacity(self.display_byte_len());
        for op in &self.ops {
            match op {
                PicEditOp::NationalSlot => {
                    let unit = pair_iter.next().copied().unwrap_or([b' ', 0x00]);
                    out.extend_from_slice(&unit);
                }
                PicEditOp::InsertBlank => out.extend_from_slice(&[b' ', 0x00]),
                PicEditOp::InsertZero => out.extend_from_slice(&[b'0', 0x00]),
                PicEditOp::InsertSlash => out.extend_from_slice(&[b'/', 0x00]),
                _ => {}
            }
        }
        Ok(out)
    }

    /// Executes `NATIONAL-EDITED` formatting into `UTF-16BE` bytes (`[0x00, ch]`).
    ///
    /// # Arguments
    ///
    /// * `src_utf16be` - Source `UTF-16BE` byte slice (must have even length).
    /// * `justification` - [`Justification::Left`] or [`Justification::Right`] applied to `N` slots.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::EncodingConversion`] if `src_utf16be.len()` is odd.
    pub fn format_national_utf16be(
        &self,
        src_utf16be: &[u8],
        justification: Justification,
    ) -> Result<Vec<u8>> {
        let slot_count = self
            .ops
            .iter()
            .filter(|op| matches!(op, PicEditOp::NationalSlot))
            .count();
        let aligned = pad_or_truncate_national_utf16be(src_utf16be, slot_count, justification)?;
        let (pairs, _) = aligned.as_chunks::<2>();
        let mut pair_iter = pairs.iter();
        let mut out = Vec::with_capacity(self.display_byte_len());
        for op in &self.ops {
            match op {
                PicEditOp::NationalSlot => {
                    let unit = pair_iter.next().copied().unwrap_or([0x00, b' ']);
                    out.extend_from_slice(&unit);
                }
                PicEditOp::InsertBlank => out.extend_from_slice(&[0x00, b' ']),
                PicEditOp::InsertZero => out.extend_from_slice(&[0x00, b'0']),
                PicEditOp::InsertSlash => out.extend_from_slice(&[0x00, b'/']),
                _ => {}
            }
        }
        Ok(out)
    }

    /// Executes `UTF-8` edited formatting (`U` / character slots with `B`, `0`, `/` insertions),
    /// truncating cleanly on Unicode scalar boundaries and right-padding to `byte_len`.
    ///
    /// # Arguments
    ///
    /// * `src_utf8` - Source valid UTF-8 string.
    /// * `byte_len` - Target `UTF-8` storage byte capacity.
    /// * `justification` - [`Justification::Left`] or [`Justification::Right`] applied to character slots.
    #[must_use]
    pub fn format_utf8(
        &self,
        src_utf8: &str,
        byte_len: usize,
        justification: Justification,
    ) -> Vec<u8> {
        let slot_count = self
            .ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    PicEditOp::Utf8Slot
                        | PicEditOp::AlphaSlot
                        | PicEditOp::AnyCharSlot
                        | PicEditOp::DigitSlot
                )
            })
            .count();
        let src_chars: Vec<char> = src_utf8.chars().collect();
        let mut aligned_chars = Vec::with_capacity(slot_count);
        match justification {
            Justification::Left => {
                aligned_chars.extend(src_chars.iter().copied().take(slot_count));
                while aligned_chars.len() < slot_count {
                    aligned_chars.push(' ');
                }
            }
            Justification::Right => {
                if src_chars.len() >= slot_count {
                    let skip = src_chars.len() - slot_count;
                    aligned_chars.extend(src_chars.into_iter().skip(skip));
                } else {
                    let pad = slot_count - src_chars.len();
                    aligned_chars.extend(core::iter::repeat_n(' ', pad));
                    aligned_chars.extend(src_chars);
                }
            }
        }

        let mut char_iter = aligned_chars.into_iter();
        let mut edited = String::with_capacity(self.ops.len());
        for op in &self.ops {
            match op {
                PicEditOp::Utf8Slot
                | PicEditOp::AlphaSlot
                | PicEditOp::AnyCharSlot
                | PicEditOp::DigitSlot => {
                    edited.push(char_iter.next().unwrap_or(' '));
                }
                PicEditOp::InsertBlank => edited.push(' '),
                PicEditOp::InsertZero => edited.push('0'),
                PicEditOp::InsertSlash => edited.push('/'),
                _ => {}
            }
        }
        pad_or_truncate_utf8(&edited, byte_len, byte_len, Justification::Left)
    }

    /// De-edits a formatted display byte buffer back into an exact [`FixedDecimal`] using this
    /// program's [`DecimalPointKind`] and single- or multi-byte [`CurrencySignSpec`].
    ///
    /// # Arguments
    ///
    /// * `edited_bytes` - Raw display bytes of the edited field.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::PictureCodec`] if `edited_bytes` contains invalid characters or exceeds 76 digits.
    pub fn de_edit(&self, edited_bytes: &[u8]) -> Result<FixedDecimal> {
        de_edit_to_decimal_with_currency(
            edited_bytes,
            self.decimal_point.decimal_byte(),
            self.currency.as_bytes(),
        )
    }
}

/// Validates that a `PicEditOp` slice is non-empty.
///
/// # Arguments
///
/// * `ops` - Candidate editing bytecode slice.
fn validate_non_empty_ops(ops: &[PicEditOp]) -> Result<()> {
    if ops.is_empty() {
        return Err(CoblangError::PictureCodec {
            picture: String::from("PictureProgram"),
            message: String::from("PICTURE bytecode program cannot be empty"),
        });
    }
    Ok(())
}

/// Mutable cursor state during [`execute_numeric_vm`].
struct VmNumericState<'a> {
    /// Emitted output byte buffer.
    out: Vec<u8>,
    /// Index into `digit_chars`.
    digit_idx: usize,
    /// `true` once a non-zero digit or decimal point has established significance.
    started: bool,
    /// Replacement byte for suppressed leading positions (`b' '` or `b'*'`).
    suppress_fill: u8,
    /// `true` once the first `FloatingSign*` or `FloatingCurrency` anchor has been encountered.
    seen_floating: bool,
    /// Pending floating sign/currency byte slice (`b"+"`, `b"-"`, `b"EUR"`, etc.) to emit before first significant digit.
    pending_float: Option<&'a [u8]>,
    /// Whether `value` is negative and `sign_capability == Signed`.
    is_neg: bool,
    /// Active decimal point byte.
    decimal_byte: u8,
    /// Active grouping separator byte.
    group_sep_byte: u8,
    /// Active currency string bytes (len >= 1).
    currency_bytes: &'a [u8],
}

/// Executes the numeric-edited `PictureProgram` VM with multi-byte `CURRENCY SIGN` support.
///
/// # Arguments
///
/// * `value` - Source [`FixedDecimal`].
/// * `ops` - Non-empty `PicEditOp` slice.
/// * `meta` - Numeric metadata (`digits`, `scale`, `sign_capability`, `blank_zero`).
/// * `decimal_point` - Active [`DecimalPointKind`].
/// * `currency_bytes` - Active currency bytes (`len >= 1`).
/// * `rounding` - Fractional [`RoundingMode`].
/// * `high_order` - High-order [`HighOrderPolicy`].
/// * `total_byte_len` - Precomputed exact output byte length.
#[allow(clippy::too_many_arguments)]
fn execute_numeric_vm(
    value: &FixedDecimal,
    ops: &[PicEditOp],
    meta: NumericPictureMeta,
    decimal_point: DecimalPointKind,
    currency_bytes: &[u8],
    rounding: RoundingMode,
    high_order: HighOrderPolicy,
    total_byte_len: usize,
) -> Result<(Vec<u8>, SizeErrorStatus)> {
    let signed_pol = match meta.sign_capability {
        Signedness::Signed => SignednessPolicy::Signed,
        Signedness::Unsigned => SignednessPolicy::Unsigned,
    };
    let (fitted, status) =
        value.fit_to_picture(meta.digits, meta.scale, signed_pol, rounding, high_order)?;

    if meta.blank_zero == BlankZeroSpec::BlankWhenZero && fitted.is_zero() {
        return Ok((vec![b' '; total_byte_len], status));
    }

    let is_neg = fitted.is_negative();
    let digit_chars = extract_aligned_digits(
        fitted.mantissa().magnitude(),
        usize::from(meta.digits.get()),
    );
    let has_asterisk = ops
        .iter()
        .any(|op| matches!(op, PicEditOp::ZeroSuppressAsterisk));
    let suppress_fill = if has_asterisk { b'*' } else { b' ' };
    let has_explicit_digit = ops.iter().any(|op| matches!(op, PicEditOp::DigitSlot));

    if fitted.is_zero() && !has_explicit_digit {
        let mut blanked = Vec::with_capacity(total_byte_len);
        let mut seen_fl_cur = false;
        for op in ops {
            match op {
                PicEditOp::InsertDecimalPoint if has_asterisk => {
                    blanked.push(decimal_point.decimal_byte());
                }
                PicEditOp::FixedCreditCr | PicEditOp::FixedDebitDb => {
                    blanked.extend_from_slice(b"  ");
                }
                PicEditOp::FixedCurrency(_) => {
                    blanked.extend(core::iter::repeat_n(suppress_fill, currency_bytes.len()));
                }
                PicEditOp::FloatingCurrency(_) if !seen_fl_cur => {
                    seen_fl_cur = true;
                    blanked.extend(core::iter::repeat_n(suppress_fill, currency_bytes.len()));
                }
                PicEditOp::DigitSlot
                | PicEditOp::ZeroSuppressSpace
                | PicEditOp::ZeroSuppressAsterisk
                | PicEditOp::FloatingSignPlus
                | PicEditOp::FloatingSignMinus
                | PicEditOp::FloatingCurrency(_)
                | PicEditOp::InsertDecimalPoint
                | PicEditOp::InsertGroupSep
                | PicEditOp::InsertBlank
                | PicEditOp::InsertZero
                | PicEditOp::InsertSlash
                | PicEditOp::FixedSignPlus
                | PicEditOp::FixedSignMinus => blanked.push(suppress_fill),
                _ => {}
            }
        }
        return Ok((blanked, status));
    }

    let mut st = VmNumericState {
        out: Vec::with_capacity(total_byte_len),
        digit_idx: 0,
        started: false,
        suppress_fill,
        seen_floating: false,
        pending_float: None,
        is_neg,
        decimal_byte: decimal_point.decimal_byte(),
        group_sep_byte: decimal_point.group_sep_byte(),
        currency_bytes,
    };

    for op in ops {
        step_vm_op(*op, &digit_chars, &mut st);
    }
    if let Some(fl) = st.pending_float.take() {
        st.out.extend_from_slice(fl);
    }
    Ok((st.out, status))
}

/// Executes one [`PicEditOp`] step inside [`execute_numeric_vm`].
///
/// # Arguments
///
/// * `op` - Current [`PicEditOp`].
/// * `digit_chars` - Aligned ASCII digits.
/// * `st` - Mutable [`VmNumericState`].
fn step_vm_op(op: PicEditOp, digit_chars: &[u8], st: &mut VmNumericState<'_>) {
    match op {
        PicEditOp::DigitSlot => {
            let d = next_vm_digit(digit_chars, &mut st.digit_idx);
            flush_pending_float(st);
            st.started = true;
            st.out.push(d);
        }
        PicEditOp::ZeroSuppressSpace | PicEditOp::ZeroSuppressAsterisk => {
            let d = next_vm_digit(digit_chars, &mut st.digit_idx);
            if st.started || d != b'0' {
                flush_pending_float(st);
                st.started = true;
                st.out.push(d);
            } else {
                st.out.push(st.suppress_fill);
            }
        }
        PicEditOp::FloatingSignPlus => {
            let sym: &[u8] = if st.is_neg { b"-" } else { b"+" };
            step_floating(sym, digit_chars, st);
        }
        PicEditOp::FloatingSignMinus => {
            let sym: &[u8] = if st.is_neg { b"-" } else { b" " };
            step_floating(sym, digit_chars, st);
        }
        PicEditOp::FloatingCurrency(_) => {
            let sym = st.currency_bytes;
            step_floating(sym, digit_chars, st);
        }
        PicEditOp::InsertDecimalPoint => {
            flush_pending_float(st);
            st.started = true;
            st.out.push(st.decimal_byte);
        }
        PicEditOp::InsertGroupSep => {
            st.out.push(if st.started {
                st.group_sep_byte
            } else {
                st.suppress_fill
            });
        }
        PicEditOp::InsertBlank => {
            st.out
                .push(if st.started { b' ' } else { st.suppress_fill });
        }
        PicEditOp::InsertZero => {
            st.out
                .push(if st.started { b'0' } else { st.suppress_fill });
        }
        PicEditOp::InsertSlash => {
            st.out
                .push(if st.started { b'/' } else { st.suppress_fill });
        }
        PicEditOp::FixedSignPlus => st.out.push(if st.is_neg { b'-' } else { b'+' }),
        PicEditOp::FixedSignMinus => st.out.push(if st.is_neg { b'-' } else { b' ' }),
        PicEditOp::FixedCreditCr => st
            .out
            .extend_from_slice(if st.is_neg { b"CR" } else { b"  " }),
        PicEditOp::FixedDebitDb => st
            .out
            .extend_from_slice(if st.is_neg { b"DB" } else { b"  " }),
        PicEditOp::FixedCurrency(_) => st.out.extend_from_slice(st.currency_bytes),
        _ => {}
    }
}

/// Processes a floating sign or multi-byte currency position.
///
/// # Arguments
///
/// * `sym` - Floating symbol byte slice (`b"+"`, `b"-"`, `b" "`, or `currency_bytes`).
/// * `digit_chars` - Aligned ASCII digits.
/// * `st` - Mutable [`VmNumericState`].
fn step_floating<'a>(sym: &'a [u8], digit_chars: &[u8], st: &mut VmNumericState<'a>) {
    if !st.seen_floating {
        st.seen_floating = true;
        st.pending_float = Some(sym);
        return;
    }
    let d = next_vm_digit(digit_chars, &mut st.digit_idx);
    if st.started || d != b'0' {
        flush_pending_float(st);
        st.started = true;
        st.out.push(d);
    } else {
        st.out.push(st.suppress_fill);
    }
}

/// Emits `st.pending_float` into `st.out` once significance begins.
///
/// # Arguments
///
/// * `st` - Mutable [`VmNumericState`].
fn flush_pending_float(st: &mut VmNumericState<'_>) {
    if let Some(fl) = st.pending_float.take() {
        st.out.extend_from_slice(fl);
    }
}

/// Extracts `digits` right-aligned ASCII decimal characters from `mag`.
///
/// # Arguments
///
/// * `mag` - Fitted unsigned magnitude.
/// * `digits` - Target `PICTURE` digit count.
#[must_use]
fn extract_aligned_digits(mag: Uint256, digits: usize) -> Vec<u8> {
    let raw = mag.to_decimal_digits();
    let mut buf = vec![b'0'; digits];
    let copy_len = raw.len().min(digits);
    let src_start = raw.len() - copy_len;
    let dst_start = digits - copy_len;
    for (slot, &b) in buf
        .get_mut(dst_start..)
        .unwrap_or(&mut [])
        .iter_mut()
        .zip(raw.get(src_start..).unwrap_or(&[]))
    {
        *slot = b;
    }
    buf
}

/// Reads the next digit from `digit_chars`, defaulting to `b'0'` past the end.
///
/// # Arguments
///
/// * `digit_chars` - Aligned ASCII digits.
/// * `digit_idx` - Mutable cursor.
#[must_use]
fn next_vm_digit(digit_chars: &[u8], digit_idx: &mut usize) -> u8 {
    let d = digit_chars.get(*digit_idx).copied().unwrap_or(b'0');
    *digit_idx += 1;
    d
}
