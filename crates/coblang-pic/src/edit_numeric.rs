//! Full `PicEditOp` numeric-edited bytecode formatter (`Z`, `*`, `$$`, `++`, `--`, `CR`, `DB`, `,`, `.`).

use coblang_common::{DigitCount, Result, ScaleFactor};
use coblang_decimal::{
    FixedDecimal, HighOrderPolicy, RoundingMode, SignednessPolicy, SizeErrorStatus,
};
use coblang_hir::{BlankZeroSpec, PicEditOp, Signedness};

/// Configuration parameters for formatting a `FixedDecimal` through a numeric-edited `PICTURE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NumericEditConfig {
    /// Total numeric digit positions (`1..=38`).
    pub digits: DigitCount,
    /// Fractional / `P` scale factor (`-38..=38`).
    pub scale: ScaleFactor,
    /// Whether the `PICTURE` contains a sign symbol (`+`, `-`, `CR`, `DB`).
    pub sign_capability: Signedness,
    /// `BLANK WHEN ZERO` setting.
    pub blank_zero: BlankZeroSpec,
    /// Active decimal point character (`b'.'` normally, `b','` under `DECIMAL-POINT IS COMMA`).
    pub decimal_char: u8,
    /// Active grouping separator character (`b','` normally, `b'.'` under `DECIMAL-POINT IS COMMA`).
    pub group_sep_char: u8,
}

impl NumericEditConfig {
    /// Creates a standard `.`-decimal, `,`-separator [`NumericEditConfig`].
    ///
    /// # Arguments
    ///
    /// * `digits` - Total digit positions.
    /// * `scale` - Scale factor.
    /// * `sign_capability` - Signedness capability.
    /// * `blank_zero` - `BLANK WHEN ZERO` mode.
    #[must_use]
    pub const fn standard(
        digits: DigitCount,
        scale: ScaleFactor,
        sign_capability: Signedness,
        blank_zero: BlankZeroSpec,
    ) -> Self {
        Self {
            digits,
            scale,
            sign_capability,
            blank_zero,
            decimal_char: b'.',
            group_sep_char: b',',
        }
    }
}

/// Formats `value` through `edit_ops` according to ISO COBOL numeric-editing rules.
///
/// # Arguments
///
/// * `value` - Source `FixedDecimal`.
/// * `edit_ops` - Compiled [`PicEditOp`] bytecode from `coblang-sema`.
/// * `cfg` - [`NumericEditConfig`] specifying digit count, scale, sign capability, and separators.
/// * `mode` - Rounding mode for excess low-order fractional digits.
/// * `high_order` - High-order digit overflow vs modulo-truncation policy.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError::DecimalDomain`] only on internal scale bound violation.
pub fn format_numeric_edited(
    value: &FixedDecimal,
    edit_ops: &[PicEditOp],
    cfg: NumericEditConfig,
    mode: RoundingMode,
    high_order: HighOrderPolicy,
) -> Result<(Vec<u8>, SizeErrorStatus)> {
    let signed_pol = match cfg.sign_capability {
        Signedness::Signed => SignednessPolicy::Signed,
        Signedness::Unsigned => SignednessPolicy::Unsigned,
    };
    let (fitted, status) =
        value.fit_to_picture(cfg.digits, cfg.scale, signed_pol, mode, high_order)?;
    let display_len = compute_display_byte_len(edit_ops);

    if fitted.is_zero() && cfg.blank_zero == BlankZeroSpec::BlankWhenZero {
        return Ok((vec![b' '; display_len], status));
    }

    // ISO COBOL all-zero-suppression rule: if zero and no `DigitSlot` (`9`) exists anywhere,
    // fill with `' '` (or `'*'` except at `InsertDecimalPoint` when asterisk suppression is used).
    let has_hard_digit = edit_ops.contains(&PicEditOp::DigitSlot);
    if fitted.is_zero() && !has_hard_digit {
        return Ok((format_all_suppressed_zero(edit_ops, cfg), status));
    }

    let is_neg = fitted.is_negative();
    let digit_chars = extract_aligned_digits(&fitted, usize::from(cfg.digits.get()));
    let rendered = render_edited_ops(edit_ops, &digit_chars, is_neg, cfg);
    Ok((rendered, status))
}

/// Computes total output byte length of `edit_ops`.
///
/// # Arguments
///
/// * `edit_ops` - Compiled `PicEditOp` slice.
#[must_use]
fn compute_display_byte_len(edit_ops: &[PicEditOp]) -> usize {
    edit_ops
        .iter()
        .map(|op| match op {
            PicEditOp::ImpliedDecimal | PicEditOp::ScalingPosition => 0,
            PicEditOp::FixedCreditCr | PicEditOp::FixedDebitDb => 2,
            _ => 1,
        })
        .sum()
}

/// Renders an all-suppression `PICTURE` (`PIC ZZZ.ZZ`, `PIC ***.**`, `PIC $$$.$$`) when value is zero.
///
/// # Arguments
///
/// * `edit_ops` - Compiled `PicEditOp` slice.
/// * `cfg` - Numeric edit config.
#[must_use]
fn format_all_suppressed_zero(edit_ops: &[PicEditOp], cfg: NumericEditConfig) -> Vec<u8> {
    let is_asterisk = edit_ops.contains(&PicEditOp::ZeroSuppressAsterisk);
    let mut out = Vec::with_capacity(compute_display_byte_len(edit_ops));
    for &op in edit_ops {
        match op {
            PicEditOp::ImpliedDecimal | PicEditOp::ScalingPosition => {}
            PicEditOp::FixedCreditCr | PicEditOp::FixedDebitDb => {
                out.push(b' ');
                out.push(b' ');
            }
            PicEditOp::InsertDecimalPoint if is_asterisk => out.push(cfg.decimal_char),
            _ => out.push(if is_asterisk { b'*' } else { b' ' }),
        }
    }
    out
}

/// Extracts exact `digit_count` right-aligned ASCII digits `'0'..'9'` from `fitted`.
///
/// # Arguments
///
/// * `fitted` - Quantized `FixedDecimal`.
/// * `digit_count` - Declared `PICTURE` digit count.
#[must_use]
fn extract_aligned_digits(fitted: &FixedDecimal, digit_count: usize) -> Vec<u8> {
    let raw = fitted.mantissa().magnitude().to_decimal_digits();
    let mut buf = vec![b'0'; digit_count];
    let copy_len = raw.len().min(digit_count);
    let src_start = raw.len() - copy_len;
    let dst_start = digit_count - copy_len;
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

/// State machine executing `edit_ops` against `digit_chars`.
struct EditRenderState {
    /// Whether a significant digit (`1..9`) or `DigitSlot` or `.` has started unsuppressed output.
    started: bool,
    /// Fill character used during suppression (`b' '` or `b'*'`).
    suppress_fill: u8,
    /// Optional floating symbol (`'$'`, `'+'`, `'-'`, `' '`) waiting to be emitted right before the first significant position.
    pending_float_char: Option<u8>,
    /// Index of the most recent output byte written by a suppressed floating slot, so the float symbol can be placed there if `.` or `9` arrives.
    last_float_out_idx: Option<usize>,
    /// Whether the first anchor `Floating*` op has already been seen.
    seen_float_anchor: bool,
    /// Current index into `digit_chars`.
    digit_idx: usize,
}

/// Executes `edit_ops` over `digit_chars` and `is_neg` to produce the formatted display bytes.
///
/// # Arguments
///
/// * `edit_ops` - Compiled `PicEditOp` slice.
/// * `digit_chars` - Right-aligned ASCII digit characters `'0'..'9'`.
/// * `is_neg` - `true` when quantized value is `< 0`.
/// * `cfg` - Numeric edit config.
#[must_use]
fn render_edited_ops(
    edit_ops: &[PicEditOp],
    digit_chars: &[u8],
    is_neg: bool,
    cfg: NumericEditConfig,
) -> Vec<u8> {
    let fill = if edit_ops.contains(&PicEditOp::ZeroSuppressAsterisk) {
        b'*'
    } else {
        b' '
    };
    let pending_float_char = detect_float_char(edit_ops, is_neg);
    let mut st = EditRenderState {
        started: false,
        suppress_fill: fill,
        pending_float_char,
        last_float_out_idx: None,
        seen_float_anchor: false,
        digit_idx: 0,
    };

    let mut out = Vec::with_capacity(compute_display_byte_len(edit_ops));
    for &op in edit_ops {
        render_single_op(op, digit_chars, is_neg, cfg, &mut st, &mut out);
    }
    if let (Some(fl_ch), Some(pos)) = (st.pending_float_char.take(), st.last_float_out_idx) {
        out.get_mut(pos..=pos).unwrap_or(&mut []).fill(fl_ch);
    }
    out
}

/// Determines the floating insertion byte (`Some(b'$')`, `Some(b'+')`, `Some(b'-')`, `Some(b' ')`), if any.
///
/// # Arguments
///
/// * `edit_ops` - Compiled `PicEditOp` slice.
/// * `is_neg` - Whether quantized value is `< 0`.
#[must_use]
fn detect_float_char(edit_ops: &[PicEditOp], is_neg: bool) -> Option<u8> {
    for &op in edit_ops {
        match op {
            PicEditOp::FloatingSignPlus => return Some(if is_neg { b'-' } else { b'+' }),
            PicEditOp::FloatingSignMinus => return Some(if is_neg { b'-' } else { b' ' }),
            PicEditOp::FloatingCurrency(ch) => {
                return Some(if ch.is_ascii() { ch as u8 } else { b'$' });
            }
            _ => {}
        }
    }
    None
}

/// Flushes any pending floating sign/currency character into `last_float_out_idx` when significance begins.
///
/// # Arguments
///
/// * `st` - Mutable render state.
/// * `out` - Output byte buffer.
fn flush_float_on_start(st: &mut EditRenderState, out: &mut [u8]) {
    if !st.started {
        st.started = true;
        if let (Some(fl_ch), Some(pos)) = (st.pending_float_char.take(), st.last_float_out_idx) {
            out.get_mut(pos..=pos).unwrap_or(&mut []).fill(fl_ch);
        }
    }
}

/// Processes a single [`PicEditOp`] during numeric-edited formatting.
///
/// # Arguments
///
/// * `op` - Current bytecode instruction.
/// * `digit_chars` - Aligned ASCII digits.
/// * `is_neg` - Negative flag.
/// * `cfg` - Numeric edit config.
/// * `st` - Mutable [`EditRenderState`].
/// * `out` - Output display buffer.
fn render_single_op(
    op: PicEditOp,
    digit_chars: &[u8],
    is_neg: bool,
    cfg: NumericEditConfig,
    st: &mut EditRenderState,
    out: &mut Vec<u8>,
) {
    match op {
        PicEditOp::ImpliedDecimal => {
            st.started = true;
        }
        PicEditOp::DigitSlot => {
            let d = next_digit(digit_chars, &mut st.digit_idx);
            flush_float_on_start(st, out);
            out.push(d);
        }
        PicEditOp::ZeroSuppressSpace | PicEditOp::ZeroSuppressAsterisk => {
            let d = next_digit(digit_chars, &mut st.digit_idx);
            if st.started || d != b'0' {
                flush_float_on_start(st, out);
                out.push(d);
            } else {
                out.push(st.suppress_fill);
            }
        }
        PicEditOp::FloatingSignPlus
        | PicEditOp::FloatingSignMinus
        | PicEditOp::FloatingCurrency(_) => {
            if st.seen_float_anchor {
                let d = next_digit(digit_chars, &mut st.digit_idx);
                if st.started || d != b'0' {
                    flush_float_on_start(st, out);
                    out.push(d);
                } else {
                    st.last_float_out_idx = Some(out.len());
                    out.push(b' ');
                }
            } else {
                st.seen_float_anchor = true;
                st.last_float_out_idx = Some(out.len());
                out.push(b' ');
            }
        }
        PicEditOp::InsertDecimalPoint => {
            flush_float_on_start(st, out);
            out.push(cfg.decimal_char);
        }
        PicEditOp::InsertGroupSep => {
            if st.started {
                out.push(cfg.group_sep_char);
            } else {
                if st.seen_float_anchor {
                    st.last_float_out_idx = Some(out.len());
                }
                out.push(st.suppress_fill);
            }
        }
        PicEditOp::InsertBlank => {
            out.push(if st.started { b' ' } else { st.suppress_fill });
        }
        PicEditOp::InsertZero => {
            if st.started {
                out.push(b'0');
            } else {
                out.push(st.suppress_fill);
            }
        }
        PicEditOp::InsertSlash => out.push(b'/'),
        PicEditOp::FixedSignPlus => out.push(if is_neg { b'-' } else { b'+' }),
        PicEditOp::FixedSignMinus => out.push(if is_neg { b'-' } else { b' ' }),
        PicEditOp::FixedCreditCr => {
            out.extend_from_slice(if is_neg { b"CR" } else { b"  " });
        }
        PicEditOp::FixedDebitDb => {
            out.extend_from_slice(if is_neg { b"DB" } else { b"  " });
        }
        PicEditOp::FixedCurrency(ch) => {
            out.push(if ch.is_ascii() { ch as u8 } else { b'$' });
        }
        PicEditOp::ScalingPosition
        | PicEditOp::AlphaSlot
        | PicEditOp::AnyCharSlot
        | PicEditOp::BitSlot
        | PicEditOp::NationalSlot
        | PicEditOp::Utf8Slot => {}
    }
}

/// Fetches `digit_chars[digit_idx]` and advances `digit_idx`.
///
/// # Arguments
///
/// * `digit_chars` - Aligned ASCII digits.
/// * `digit_idx` - Mutable cursor.
#[must_use]
fn next_digit(digit_chars: &[u8], digit_idx: &mut usize) -> u8 {
    let d = digit_chars.get(*digit_idx).copied().unwrap_or(b'0');
    *digit_idx += 1;
    d
}
