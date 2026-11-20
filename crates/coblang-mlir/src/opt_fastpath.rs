//! MLIR Numeric & Move Fast-Path Classifier and Builder (`crates/coblang-mlir/src/opt_fastpath.rs`).
//!
//! Lowers fixed-length non-aliasing alphanumeric/group `MOVE`s directly to `llvm.intr.memcpy` + `llvm.intr.memset`,
//! and lowers native `COMP-5` / `<= 37`-digit fixed-scale `BINARY` `ADD` / `SUBTRACT` / `MULTIPLY` / `DIVIDE` / `COMPUTE`
//! to inline `i64` / `i128` `arith.*` operations + `@llvm.s*.with.overflow` checks when `ON SIZE ERROR` is attached.

use coblang_common::DataItemId;
use coblang_diag::SymbolInterner;
use coblang_hir::{
    BinaryEndianness, DynamicOdoPresence, HirBinOp, HirConstValue, HirDecimalSign, HirExpr,
    HirFigurative, HirPlace, HirProgramFormatting, HirRounding, HirType, JustifiedSpec, Signedness,
    TruncationPolicy,
};

/// Fast-path source payload for fixed-length alphanumeric/group `MOVE`.
#[derive(Debug, Clone)]
pub enum FastAlphaSource<'a> {
    /// Static literal byte slice.
    Literal(&'a [u8]),
    /// Non-aliasing static-length [`HirPlace`].
    Place(&'a HirPlace),
}

/// Classified fast-path `MOVE` execution plan.
#[derive(Debug, Clone)]
pub enum FastMovePlan<'a> {
    /// `llvm.intr.memcpy` of `min(src_len, dst_len)` + trailing `llvm.intr.memset` with `pad_byte`.
    DirectCopyPad {
        /// Source literal or place.
        source: FastAlphaSource<'a>,
        /// Static source byte length.
        src_len: u32,
        /// Static destination byte length (`>= 1`).
        dst_len: u32,
        /// Trailing space pad byte (`0x20` ASCII, `0x40` EBCDIC).
        pad_byte: u8,
    },
    /// Single `llvm.intr.memset` across `dst_len` bytes (`SPACE`, `ZERO`, `LOW-VALUE`, `HIGH-VALUE`, `QUOTE`).
    FigurativeFill {
        /// Static destination byte length (`>= 1`).
        dst_len: u32,
        /// Repeated fill byte.
        fill_byte: u8,
    },
}

/// Binary arithmetic operator inside a [`FastIntExpr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FastIntBinOp {
    /// Checked `i128` addition (`@llvm.sadd.with.overflow.i128` or `arith.addi`).
    Add,
    /// Checked `i128` subtraction (`@llvm.ssub.with.overflow.i128` or `arith.subi`).
    Sub,
    /// Checked `i128` multiplication (`@llvm.smul.with.overflow.i128` or `arith.muli`).
    Mul,
    /// Signed `i128` truncating division (`arith.divsi` guarded by `rhs != 0`).
    Div,
}

/// Verified fixed-scale integer expression tree lowered to inline `i64` / `i128` SSA operations.
#[derive(Debug, Clone)]
pub enum FastIntExpr<'a> {
    /// Immediate scaled `i128` constant.
    Const(i128),
    /// Native-endian `BinaryInt` load (`1`, `2`, `4`, or `8` bytes, signed or unsigned).
    LoadBinary {
        /// Source [`HirPlace`].
        place: &'a HirPlace,
        /// Storage byte width (`1`, `2`, `4`, or `8`).
        byte_width: u8,
        /// Signed vs unsigned integer extension.
        sign: Signedness,
    },
    /// Checked unary negation (`0 - inner`).
    Neg(Box<FastIntExpr<'a>>),
    /// Binary integer operation.
    Binary {
        /// Operator kind.
        op: FastIntBinOp,
        /// Left operand.
        lhs: Box<FastIntExpr<'a>>,
        /// Right operand.
        rhs: Box<FastIntExpr<'a>>,
    },
}

/// Storage truncation/overflow bound rule for a [`FastIntArithPlan`] destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastIntBoundRule {
    /// `COMP-5` / `-fnotrunc` hardware integer bounds (`[min_val..=max_val]`).
    FullBinary {
        /// Inclusive minimum representable integer (`0` when unsigned).
        min_val: i128,
        /// Inclusive maximum representable integer.
        max_val: i128,
    },
    /// `PIC 9(d)` decimal modulo / capacity bound (`10^digits`).
    PicModulo {
        /// Exact `10^digits` modulus (`>= 10`).
        modulus: i128,
    },
}

/// Verified inline `i64` / `i128` arithmetic lowering plan for `HirStmt::AssignArith`.
#[derive(Debug, Clone)]
pub struct FastIntArithPlan<'a> {
    /// Destination native `BinaryInt` place.
    pub dst_place: &'a HirPlace,
    /// Destination byte width (`1`, `2`, `4`, or `8`).
    pub dst_byte_width: u8,
    /// Destination signedness (`Unsigned` takes `|res|` before storing/checking).
    pub dst_sign: Signedness,
    /// Overflow/modulo rule.
    pub bound_rule: FastIntBoundRule,
    /// Verified integer expression tree.
    pub expr: FastIntExpr<'a>,
}

/// Attempts to classify `MOVE src TO dst` into a zero-runtime-call [`FastMovePlan`].
///
/// # Arguments
///
/// * `src` - Source HIR expression.
/// * `dst` - Destination HIR place.
/// * `src_root` - Optional root `01`/`77` [`DataItemId`] of `src` (when `src` is a place).
/// * `dst_root` - Root `01`/`77` [`DataItemId`] of `dst`.
/// * `fmt` - Program formatting conventions (determines ASCII `0x20` vs EBCDIC `0x40` space byte).
#[must_use]
pub fn try_classify_fast_move<'a>(
    src: &'a HirExpr,
    dst: &'a HirPlace,
    src_root: Option<DataItemId>,
    dst_root: DataItemId,
    fmt: HirProgramFormatting,
) -> Option<FastMovePlan<'a>> {
    if dst.refmod.is_some() || !is_left_justified_alpha_or_group(&dst.effective_ty) {
        return None;
    }
    let dst_len = dst.effective_ty.static_byte_size().max(1);
    let (space_byte, zero_byte, quote_byte) =
        crate::lower_m17::single_byte_figuratives_for_fmt(fmt);

    match src {
        HirExpr::Const { value, .. } => match value {
            HirConstValue::Bytes(bytes) => Some(FastMovePlan::DirectCopyPad {
                source: FastAlphaSource::Literal(bytes),
                src_len: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
                dst_len,
                pad_byte: space_byte,
            }),
            HirConstValue::Figurative(fig) => {
                let fill_byte = match fig {
                    HirFigurative::Space => Some(space_byte),
                    HirFigurative::Zero => Some(zero_byte),
                    HirFigurative::HighValue => Some(0xFF),
                    HirFigurative::LowValue | HirFigurative::Null => Some(0x00),
                    HirFigurative::Quote => Some(quote_byte),
                    HirFigurative::All(pat) => match pat.as_slice() {
                        [single] => Some(*single),
                        _ => None,
                    },
                }?;
                Some(FastMovePlan::FigurativeFill { dst_len, fill_byte })
            }
            _ => None,
        },
        HirExpr::Place(sp) => {
            if sp.refmod.is_some()
                || src_root.is_none()
                || src_root == Some(dst_root)
                || !is_any_alpha_or_group(&sp.effective_ty)
            {
                return None;
            }
            Some(FastMovePlan::DirectCopyPad {
                source: FastAlphaSource::Place(sp),
                src_len: sp.effective_ty.static_byte_size().max(1),
                dst_len,
                pad_byte: space_byte,
            })
        }
        _ => None,
    }
}

/// Returns `true` if `ty` is a left-justified `Alphanumeric`, `Alphabetic`, or static `Group`.
///
/// # Arguments
///
/// * `ty` - Candidate [`HirType`].
const fn is_left_justified_alpha_or_group(ty: &HirType) -> bool {
    matches!(
        ty,
        HirType::Alphanumeric {
            justified: JustifiedSpec::Left,
            ..
        } | HirType::Alphabetic {
            justified: JustifiedSpec::Left,
            ..
        } | HirType::Group {
            odo_presence: DynamicOdoPresence::StaticSize,
            ..
        }
    )
}

/// Returns `true` if `ty` is `Alphanumeric`, `Alphabetic` (either justification), `AlphanumericEdited`, or static `Group`.
///
/// # Arguments
///
/// * `ty` - Candidate [`HirType`].
const fn is_any_alpha_or_group(ty: &HirType) -> bool {
    matches!(
        ty,
        HirType::Alphanumeric { .. }
            | HirType::Alphabetic { .. }
            | HirType::AlphanumericEdited { .. }
            | HirType::Group {
                odo_presence: DynamicOdoPresence::StaticSize,
                ..
            }
    )
}

/// Attempts to classify a single-target unrounded `AssignArith` into an inline `i128` [`FastIntArithPlan`].
///
/// # Arguments
///
/// * `targets` - Target `(HirPlace, HirRounding)` list.
/// * `expr` - Right-hand arithmetic expression.
/// * `interner` - Symbol interner for literal text decoding.
#[must_use]
pub fn try_classify_fast_int_arith<'a>(
    targets: &'a [(HirPlace, HirRounding)],
    expr: &'a HirExpr,
    interner: &SymbolInterner,
) -> Option<FastIntArithPlan<'a>> {
    let [(dst_place, HirRounding::Truncate)] = targets else {
        return None;
    };
    try_classify_fast_int_move(dst_place, expr, interner)
}

/// Attempts to classify `MOVE src TO dst` (or single-target unrounded `AssignArith`) into an inline `i128` [`FastIntArithPlan`].
///
/// # Arguments
///
/// * `dst_place` - Destination [`HirPlace`].
/// * `expr` - Source or right-hand expression.
/// * `interner` - Symbol interner for literal text decoding.
#[must_use]
pub fn try_classify_fast_int_move<'a>(
    dst_place: &'a HirPlace,
    expr: &'a HirExpr,
    interner: &SymbolInterner,
) -> Option<FastIntArithPlan<'a>> {
    if dst_place.refmod.is_some() {
        return None;
    }
    let HirType::BinaryInt {
        byte_width,
        digits,
        scale,
        sign,
        endian: BinaryEndianness::Native,
        trunc,
    } = &dst_place.effective_ty
    else {
        return None;
    };
    if !matches!(*byte_width, 1 | 2 | 4 | 8) || digits.get() > 37 {
        return None;
    }
    let target_scale = scale.get();
    let bound_rule = match trunc {
        TruncationPolicy::FullBinaryRange => {
            let (min_val, max_val) = hardware_bounds(*byte_width, *sign).unwrap_or((0, 0));
            FastIntBoundRule::FullBinary { min_val, max_val }
        }
        TruncationPolicy::TruncateToPic => {
            let modulus = pow10_i128(digits.get()).unwrap_or(10);
            FastIntBoundRule::PicModulo { modulus }
        }
    };
    let fast_expr = lower_fast_int_expr(expr, target_scale, interner)?;
    Some(FastIntArithPlan {
        dst_place,
        dst_byte_width: *byte_width,
        dst_sign: *sign,
        bound_rule,
        expr: fast_expr,
    })
}

/// Attempts to classify a numeric relational comparison `lhs <op> rhs` into exact inline `i128` [`FastIntExpr`] operands.
///
/// Rejects trees containing `Div` so intermediate fractional quotients (`5 / 2 = 2.5`) preserve full decimal precision.
///
/// # Arguments
///
/// * `lhs` - Left numeric expression.
/// * `rhs` - Right numeric expression.
/// * `interner` - Symbol interner.
#[must_use]
pub fn try_classify_fast_int_cmp<'a>(
    lhs: &'a HirExpr,
    rhs: &'a HirExpr,
    interner: &SymbolInterner,
) -> Option<(FastIntExpr<'a>, FastIntExpr<'a>)> {
    let fl = lower_fast_int_expr(lhs, 0, interner)?;
    let fr = lower_fast_int_expr(rhs, 0, interner)?;
    if fast_expr_has_div(&fl) || fast_expr_has_div(&fr) {
        return None;
    }
    Some((fl, fr))
}

/// Returns `true` if `expr` contains any [`FastIntBinOp::Div`] node.
///
/// # Arguments
///
/// * `expr` - Candidate integer expression tree.
fn fast_expr_has_div(expr: &FastIntExpr<'_>) -> bool {
    match expr {
        FastIntExpr::Const(_) | FastIntExpr::LoadBinary { .. } => false,
        FastIntExpr::Neg(inner) => fast_expr_has_div(inner),
        FastIntExpr::Binary { op, lhs, rhs } => {
            *op == FastIntBinOp::Div || fast_expr_has_div(lhs) || fast_expr_has_div(rhs)
        }
    }
}

/// Recursively classifies `expr` at fixed scale `expected_scale` (`Mul`/`Div` require `expected_scale == 0`).
///
/// # Arguments
///
/// * `expr` - Candidate [`HirExpr`].
/// * `expected_scale` - Required fixed decimal scale (`0` for unscaled integers).
/// * `interner` - Symbol interner.
fn lower_fast_int_expr<'a>(
    expr: &'a HirExpr,
    expected_scale: i16,
    interner: &SymbolInterner,
) -> Option<FastIntExpr<'a>> {
    match expr {
        HirExpr::Place(pl) => {
            if pl.refmod.is_some() {
                return None;
            }
            let HirType::BinaryInt {
                byte_width,
                scale,
                sign,
                endian: BinaryEndianness::Native,
                ..
            } = &pl.effective_ty
            else {
                return None;
            };
            if !matches!(*byte_width, 1 | 2 | 4 | 8) || scale.get() != expected_scale {
                return None;
            }
            Some(FastIntExpr::LoadBinary {
                place: pl,
                byte_width: *byte_width,
                sign: *sign,
            })
        }
        HirExpr::Const { value, .. } => {
            let scaled = const_to_scaled_i128(value, expected_scale, interner)?;
            if scaled.unsigned_abs() > (i64::MAX as u128) + 1 {
                return None;
            }
            Some(FastIntExpr::Const(scaled))
        }
        HirExpr::Neg(operand) => {
            let inner = lower_fast_int_expr(operand, expected_scale, interner)?;
            Some(FastIntExpr::Neg(Box::new(inner)))
        }
        HirExpr::Binary { op, lhs, rhs, .. } => {
            let bin_op = match op {
                HirBinOp::Add => FastIntBinOp::Add,
                HirBinOp::Sub => FastIntBinOp::Sub,
                HirBinOp::Mul if expected_scale == 0 => FastIntBinOp::Mul,
                HirBinOp::Div if expected_scale == 0 => FastIntBinOp::Div,
                _ => return None,
            };
            let l = lower_fast_int_expr(lhs, expected_scale, interner)?;
            let r = lower_fast_int_expr(rhs, expected_scale, interner)?;
            if matches!(bin_op, FastIntBinOp::Mul | FastIntBinOp::Div)
                && (!is_63bit_leaf(&l) || !is_63bit_leaf(&r))
            {
                return None;
            }
            Some(FastIntExpr::Binary {
                op: bin_op,
                lhs: Box::new(l),
                rhs: Box::new(r),
            })
        }
        _ => None,
    }
}

/// Returns `true` if `e` is a leaf constant or `<= 63`-bit magnitude load (`<= 4` bytes or signed `8` bytes).
///
/// # Arguments
///
/// * `e` - Candidate integer subexpression.
fn is_63bit_leaf(e: &FastIntExpr<'_>) -> bool {
    match e {
        FastIntExpr::Const(_) => true,
        FastIntExpr::LoadBinary {
            byte_width, sign, ..
        } => *byte_width <= 4 || *sign == Signedness::Signed,
        FastIntExpr::Neg(_) | FastIntExpr::Binary { .. } => false,
    }
}

/// Converts a constant literal to an exact `i128` at `expected_scale` if representable without fractional loss.
///
/// # Arguments
///
/// * `val` - Constant literal value.
/// * `expected_scale` - Required scale (`value * 10^expected_scale`).
/// * `interner` - Symbol interner.
fn const_to_scaled_i128(
    val: &HirConstValue,
    expected_scale: i16,
    interner: &SymbolInterner,
) -> Option<i128> {
    match val {
        HirConstValue::Figurative(HirFigurative::Zero) => Some(0),
        HirConstValue::Decimal { text, scale, sign } => {
            let raw_txt = interner.resolve_original(*text).ok()?.trim();
            let is_neg = *sign == HirDecimalSign::Negative || raw_txt.starts_with('-');
            let clean = raw_txt.trim_start_matches(['+', '-']);
            let mag: i128 = clean.parse().ok()?;
            let signed_raw = if is_neg { -mag } else { mag };
            apply_scale_shift(signed_raw, scale.get(), expected_scale)
        }
        _ => None,
    }
}

/// Rescales `mantissa * 10^(-src_scale)` to `expected_scale` without loss, returning `None` if fractional bits would drop.
///
/// # Arguments
///
/// * `mantissa` - Unscaled integer mantissa.
/// * `src_scale` - Source scale.
/// * `expected_scale` - Target scale.
fn apply_scale_shift(mantissa: i128, src_scale: i16, expected_scale: i16) -> Option<i128> {
    let diff = i32::from(expected_scale) - i32::from(src_scale);
    match diff.cmp(&0) {
        core::cmp::Ordering::Equal => Some(mantissa),
        core::cmp::Ordering::Greater => {
            let shift = u8::try_from(diff).ok()?;
            let factor = pow10_i128(shift)?;
            mantissa.checked_mul(factor)
        }
        core::cmp::Ordering::Less => {
            let shift = u8::try_from(-diff).ok()?;
            let factor = pow10_i128(shift)?;
            if mantissa % factor != 0 {
                return None;
            }
            Some(mantissa / factor)
        }
    }
}

/// Returns `10^exp` as `i128` for `exp <= 37`.
///
/// # Arguments
///
/// * `exp` - Decimal exponent (`0..=37`).
#[must_use]
pub const fn pow10_i128(exp: u8) -> Option<i128> {
    if exp > 37 {
        None
    } else {
        Some(10_i128.pow(exp as u32))
    }
}

/// Returns `(min_val, max_val)` in `i128` for a `byte_width` (`1`, `2`, `4`, `8`) integer.
///
/// # Arguments
///
/// * `byte_width` - Storage width (`1`, `2`, `4`, or `8`).
/// * `sign` - Signed vs unsigned.
#[must_use]
pub const fn hardware_bounds(byte_width: u8, sign: Signedness) -> Option<(i128, i128)> {
    match (byte_width, sign) {
        (1, Signedness::Signed) => Some((i8::MIN as i128, i8::MAX as i128)),
        (1, Signedness::Unsigned) => Some((0, u8::MAX as i128)),
        (2, Signedness::Signed) => Some((i16::MIN as i128, i16::MAX as i128)),
        (2, Signedness::Unsigned) => Some((0, u16::MAX as i128)),
        (4, Signedness::Signed) => Some((i32::MIN as i128, i32::MAX as i128)),
        (4, Signedness::Unsigned) => Some((0, u32::MAX as i128)),
        (8, Signedness::Signed) => Some((i64::MIN as i128, i64::MAX as i128)),
        (8, Signedness::Unsigned) => Some((0, u64::MAX as i128)),
        _ => None,
    }
}
