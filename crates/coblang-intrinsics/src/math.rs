//! Mathematical, trigonometric, logarithmic, and algebraic limit intrinsics (`Milestone M12`).

use coblang_common::Result;
use coblang_decimal::{
    DecimalSign, FixedDecimal, RoundingMode, SignedInt256, SignednessPolicy, SizeErrorStatus,
    Uint256,
};

use crate::{AlgebraicTargetSpec, ec_argument_function};

/// Fractional precision used for IEEE 754 `f64`-backed transcendental intrinsics (`18` digits).
const TRANSCENDENTAL_SCALE: i16 = 18;

/// Maximum `n` whose exact factorial `n!` fits in 75 decimal digits (`56! < 10^75 < 2^256`).
pub const MAX_EXACT_FACTORIAL_ARG: u8 = 56;

/// Upper bound on `x` for `FUNCTION EXP(x)` before `exp(x)` overflows `FixedDecimal` at scale `18`.
const MAX_EXP_ARGUMENT_F64: f64 = 130.0;

/// Upper bound on `x` for `FUNCTION EXP10(x)` before `10^x` overflows `FixedDecimal` at scale `18`.
const MAX_EXP10_ARGUMENT_F64: f64 = 57.0;

/// Evaluates `FUNCTION ABS(arg)` (`|arg|`).
///
/// # Arguments
///
/// * `arg` - Signed or unsigned decimal operand.
#[must_use]
pub const fn math_abs(arg: &FixedDecimal) -> FixedDecimal {
    arg.abs()
}

/// Evaluates `FUNCTION ACOS(arg)` (`acos(arg)` in radians, `[0, pi]`).
///
/// # Arguments
///
/// * `arg` - Cosine value required to satisfy `-1 <= arg <= 1`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` via [`coblang_common::CoblangError::InvalidDomainValue`] if `|arg| > 1`.
pub fn math_acos(arg: &FixedDecimal) -> Result<FixedDecimal> {
    let one = FixedDecimal::from_i64(1);
    let neg_one = FixedDecimal::from_i64(-1);
    if arg.cmp_numeric(&neg_one).is_lt() || arg.cmp_numeric(&one).is_gt() {
        return Err(ec_argument_function(
            "ACOS",
            arg.to_string(),
            "argument must lie in [-1, +1]",
        ));
    }
    let asin_w = eval_asin_work(arg);
    Ok(fp_round_out(&fp_sub(&const_half_pi_38(), &asin_w)))
}

/// Evaluates `FUNCTION ASIN(arg)` (`asin(arg)` in radians, `[-pi/2, +pi/2]`).
///
/// # Arguments
///
/// * `arg` - Sine value required to satisfy `-1 <= arg <= 1`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` via [`coblang_common::CoblangError::InvalidDomainValue`] if `|arg| > 1`.
pub fn math_asin(arg: &FixedDecimal) -> Result<FixedDecimal> {
    let one = FixedDecimal::from_i64(1);
    let neg_one = FixedDecimal::from_i64(-1);
    if arg.cmp_numeric(&neg_one).is_lt() || arg.cmp_numeric(&one).is_gt() {
        return Err(ec_argument_function(
            "ASIN",
            arg.to_string(),
            "argument must lie in [-1, +1]",
        ));
    }
    Ok(fp_round_out(&eval_asin_work(arg)))
}

/// Evaluates `FUNCTION ATAN(arg)` (`atan(arg)` in radians, `(-pi/2, +pi/2)`).
///
/// # Arguments
///
/// * `arg` - Real decimal tangent value.
#[must_use]
pub fn math_atan(arg: &FixedDecimal) -> FixedDecimal {
    fp_round_out(&eval_atan_work(arg))
}

/// Evaluates `FUNCTION COS(arg)` (`cos(arg)` for radian angle `arg`).
///
/// # Arguments
///
/// * `arg` - Angle in radians.
#[must_use]
pub fn math_cos(arg: &FixedDecimal) -> FixedDecimal {
    let (_, c) = eval_sin_cos_work(arg);
    fp_round_out(&c)
}

/// Evaluates `FUNCTION E` (Euler's constant `e` to 36 fractional digits).
#[must_use]
pub fn math_e() -> FixedDecimal {
    // 2.718281828459045235360287471352662498 (36 fractional digits)
    let mag = Uint256::from_decimal_digits(b"2718281828459045235360287471352662498")
        .unwrap_or(Uint256::ZERO);
    FixedDecimal::try_new(SignedInt256::from_parts(mag, DecimalSign::Positive), 36)
        .unwrap_or(FixedDecimal::ZERO)
}

/// Evaluates `FUNCTION EXP(arg)` (`exp(arg)`).
///
/// # Arguments
///
/// * `arg` - Exponent value.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `exp(arg)` overflows `FixedDecimal` precision (`arg > 130`).
pub fn math_exp(arg: &FixedDecimal) -> Result<FixedDecimal> {
    let x = arg.to_f64();
    if x > MAX_EXP_ARGUMENT_F64 {
        return Err(ec_argument_function(
            "EXP",
            arg.to_string(),
            "exponent overflows decimal range",
        ));
    }
    Ok(eval_exp_high_prec(arg))
}

/// Evaluates `FUNCTION EXP10(arg)` (`10^arg`).
///
/// Uses exact power-of-10 decimal scaling when `arg` is an integer in `-76..=57`.
///
/// # Arguments
///
/// * `arg` - Base-10 exponent value.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `10^arg` overflows `FixedDecimal` precision (`arg > 57`).
pub fn math_exp10(arg: &FixedDecimal) -> Result<FixedDecimal> {
    let x = arg.to_f64();
    if x > MAX_EXP10_ARGUMENT_F64 {
        return Err(ec_argument_function(
            "EXP10",
            arg.to_string(),
            "base-10 exponent overflows decimal range",
        ));
    }
    if let Some(int_exp) = arg.to_exact_i32()
        && (-76..=57).contains(&int_exp)
    {
        if int_exp >= 0 {
            let pow_u8 = u8::try_from(int_exp).unwrap_or(0);
            let mag = Uint256::pow10(pow_u8).unwrap_or(Uint256::ONE);
            return Ok(FixedDecimal::try_new(
                SignedInt256::from_parts(mag, DecimalSign::Positive),
                0,
            )
            .unwrap_or(FixedDecimal::ZERO));
        }
        let neg_scale = i16::try_from(-int_exp).unwrap_or(0);
        return Ok(
            FixedDecimal::try_new(SignedInt256::ONE, neg_scale).unwrap_or(FixedDecimal::ZERO)
        );
    }
    let scaled_arg = fp_mul(arg, &const_ln10_38(), 38);
    Ok(eval_exp_high_prec(&scaled_arg))
}

/// Evaluates `FUNCTION FACTORIAL(arg)` (`n!` for integer `0 <= n <= 56`).
///
/// # Arguments
///
/// * `arg` - Non-negative integer operand in `0..=56`.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `arg` is negative, has a non-zero fractional part, or exceeds `56`.
pub fn math_factorial(arg: &FixedDecimal) -> Result<FixedDecimal> {
    let Some(n_i32) = arg.to_exact_i32() else {
        return Err(ec_argument_function(
            "FACTORIAL",
            arg.to_string(),
            "argument must be an exact integer in 0..=56",
        ));
    };
    let Ok(n_u8) = u8::try_from(n_i32) else {
        return Err(ec_argument_function(
            "FACTORIAL",
            arg.to_string(),
            "argument must be non-negative and fit in 0..=56",
        ));
    };
    if n_u8 > MAX_EXACT_FACTORIAL_ARG {
        return Err(ec_argument_function(
            "FACTORIAL",
            arg.to_string(),
            "factorial exceeds 75-digit exact decimal capacity",
        ));
    }
    let mut acc = Uint256::ONE;
    for k in 2..=u64::from(n_u8) {
        acc = acc.checked_mul_u64(k).unwrap_or(Uint256::MAX);
    }
    Ok(
        FixedDecimal::try_new(SignedInt256::from_parts(acc, DecimalSign::Positive), 0)
            .unwrap_or(FixedDecimal::ZERO),
    )
}

/// Evaluates `FUNCTION FRACTION-PART(arg)` (`arg - INTEGER-PART(arg)`).
///
/// # Arguments
///
/// * `arg` - Signed or unsigned decimal operand.
#[must_use]
pub fn math_fraction_part(arg: &FixedDecimal) -> FixedDecimal {
    if arg.scale() <= 0 {
        return FixedDecimal::ZERO;
    }
    let int_part = math_integer_part(arg);
    arg.checked_sub(&int_part)
        .map_or(FixedDecimal::ZERO, |(diff, _)| diff)
}

/// Evaluates `FUNCTION HIGHEST-ALGEBRAIC(spec)` (`(10^d - 1) * 10^-s`).
///
/// # Arguments
///
/// * `spec` - Target [`AlgebraicTargetSpec`] (`digits`, `scale`, `signed`).
#[must_use]
pub fn math_highest_algebraic(spec: AlgebraicTargetSpec) -> FixedDecimal {
    let pow = Uint256::pow10(spec.digits.get()).unwrap_or(Uint256::ONE);
    let nines = pow.checked_sub(Uint256::ONE).unwrap_or(Uint256::ZERO);
    FixedDecimal::try_new(
        SignedInt256::from_parts(nines, DecimalSign::Positive),
        spec.scale.get(),
    )
    .unwrap_or(FixedDecimal::ZERO)
}

/// Evaluates `FUNCTION INTEGER(arg)` (greatest integer `floor(arg) <= arg`).
///
/// # Arguments
///
/// * `arg` - Signed or unsigned decimal operand.
#[must_use]
pub fn math_integer(arg: &FixedDecimal) -> FixedDecimal {
    if arg.scale() <= 0 {
        let (rescaled, status) = arg
            .rescale(0, RoundingMode::Truncation)
            .unwrap_or((*arg, SizeErrorStatus::Exact));
        return if status.is_error() { *arg } else { rescaled };
    }
    arg.rescale(0, RoundingMode::TowardLesser)
        .map_or(FixedDecimal::ZERO, |(v, _)| v)
}

/// Evaluates `FUNCTION INTEGER-PART(arg)` (integer portion truncated toward zero).
///
/// # Arguments
///
/// * `arg` - Signed or unsigned decimal operand.
#[must_use]
pub fn math_integer_part(arg: &FixedDecimal) -> FixedDecimal {
    if arg.scale() <= 0 {
        let (rescaled, status) = arg
            .rescale(0, RoundingMode::Truncation)
            .unwrap_or((*arg, SizeErrorStatus::Exact));
        return if status.is_error() { *arg } else { rescaled };
    }
    arg.rescale(0, RoundingMode::Truncation)
        .map_or(FixedDecimal::ZERO, |(v, _)| v)
}

/// Evaluates `FUNCTION LOG(arg)` (`ln(arg)` for `arg > 0`).
///
/// # Arguments
///
/// * `arg` - Strictly positive decimal operand.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `arg <= 0`.
pub fn math_log(arg: &FixedDecimal) -> Result<FixedDecimal> {
    if arg.is_zero() || arg.is_negative() {
        return Err(ec_argument_function(
            "LOG",
            arg.to_string(),
            "argument must be strictly positive (> 0)",
        ));
    }
    Ok(fp_round_out(&eval_ln_work(arg)))
}

/// Evaluates `FUNCTION LOG10(arg)` (`log10(arg)` for `arg > 0`).
///
/// # Arguments
///
/// * `arg` - Strictly positive decimal operand.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `arg <= 0`.
pub fn math_log10(arg: &FixedDecimal) -> Result<FixedDecimal> {
    if arg.is_zero() || arg.is_negative() {
        return Err(ec_argument_function(
            "LOG10",
            arg.to_string(),
            "argument must be strictly positive (> 0)",
        ));
    }
    let ln_x = eval_ln_work(arg);
    let q = fp_div(&ln_x, &const_ln10_38(), 38);
    Ok(fp_round_out(&q))
}

/// Evaluates `FUNCTION LOWEST-ALGEBRAIC(spec)` (`0` when unsigned, `-(10^d - 1) * 10^-s` when signed).
///
/// # Arguments
///
/// * `spec` - Target [`AlgebraicTargetSpec`] (`digits`, `scale`, `signed`).
#[must_use]
pub fn math_lowest_algebraic(spec: AlgebraicTargetSpec) -> FixedDecimal {
    match spec.signed {
        SignednessPolicy::Unsigned => FixedDecimal::ZERO,
        SignednessPolicy::Signed => math_highest_algebraic(spec).negate(),
    }
}

/// Evaluates `FUNCTION MOD(dividend, divisor)` (`dividend - divisor * floor(dividend / divisor)`).
///
/// Per ISO COBOL, when non-zero, the result carries the sign of `divisor`.
///
/// # Arguments
///
/// * `dividend` - Numerator operand.
/// * `divisor` - Non-zero modulus operand.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` when `divisor == 0` or the intermediate quotient overflows 76 digits.
pub fn math_mod(dividend: &FixedDecimal, divisor: &FixedDecimal) -> Result<FixedDecimal> {
    if divisor.is_zero() {
        return Err(ec_argument_function(
            "MOD",
            format!("{dividend}, {divisor}"),
            "modulus divisor must not be zero",
        ));
    }
    eval_remainder_with_mode("MOD", dividend, divisor, RoundingMode::TowardLesser)
}

/// Evaluates `FUNCTION PI` (`pi` to 36 fractional digits).
#[must_use]
pub fn math_pi() -> FixedDecimal {
    // 3.141592653589793238462643383279502884 (36 fractional digits)
    let mag = Uint256::from_decimal_digits(b"3141592653589793238462643383279502884")
        .unwrap_or(Uint256::ZERO);
    FixedDecimal::try_new(SignedInt256::from_parts(mag, DecimalSign::Positive), 36)
        .unwrap_or(FixedDecimal::ZERO)
}

/// Evaluates `FUNCTION REM(dividend, divisor)` (`dividend - divisor * INTEGER-PART(dividend / divisor)`).
///
/// Per ISO COBOL, when non-zero, the result carries the sign of `dividend`.
///
/// # Arguments
///
/// * `dividend` - Numerator operand.
/// * `divisor` - Non-zero divisor operand.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` when `divisor == 0` or the intermediate quotient overflows 76 digits.
pub fn math_rem(dividend: &FixedDecimal, divisor: &FixedDecimal) -> Result<FixedDecimal> {
    if divisor.is_zero() {
        return Err(ec_argument_function(
            "REM",
            format!("{dividend}, {divisor}"),
            "divisor must not be zero",
        ));
    }
    eval_remainder_with_mode("REM", dividend, divisor, RoundingMode::Truncation)
}

/// Evaluates `FUNCTION SIGN(arg)` (`-1`, `0`, or `+1`).
///
/// # Arguments
///
/// * `arg` - Signed or unsigned decimal operand.
#[must_use]
pub const fn math_sign(arg: &FixedDecimal) -> FixedDecimal {
    if arg.is_zero() {
        FixedDecimal::ZERO
    } else if arg.is_negative() {
        FixedDecimal::from_i64(-1)
    } else {
        FixedDecimal::from_i64(1)
    }
}

/// Evaluates `FUNCTION SIN(arg)` (`sin(arg)` for radian angle `arg`).
///
/// # Arguments
///
/// * `arg` - Angle in radians.
#[must_use]
pub fn math_sin(arg: &FixedDecimal) -> FixedDecimal {
    let (s, _) = eval_sin_cos_work(arg);
    fp_round_out(&s)
}

/// Evaluates ISO 2023 `FUNCTION SMALLEST-ALGEBRAIC(spec)` (`+1 * 10^-s`, smallest positive quantum).
///
/// # Arguments
///
/// * `spec` - Target [`AlgebraicTargetSpec`] (`digits`, `scale`, `signed`).
#[must_use]
pub fn math_smallest_algebraic(spec: AlgebraicTargetSpec) -> FixedDecimal {
    FixedDecimal::try_new(SignedInt256::ONE, spec.scale.get()).unwrap_or(FixedDecimal::ZERO)
}

/// Evaluates `FUNCTION SQRT(arg)` (`sqrt(arg)` for `arg >= 0`).
///
/// # Arguments
///
/// * `arg` - Non-negative decimal operand.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `arg < 0`.
pub fn math_sqrt(arg: &FixedDecimal) -> Result<FixedDecimal> {
    if arg.is_negative() {
        return Err(ec_argument_function(
            "SQRT",
            arg.to_string(),
            "square root of negative argument is undefined",
        ));
    }
    if arg.is_zero() {
        return Ok(FixedDecimal::ZERO);
    }
    let xf = arg.to_f64();
    let (work_s, out_s) = if xf < 1.0e20 {
        let root_est = xf.sqrt();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "log10 of finite f64 < 1e10 lies in -324..=10."
        )]
        let int_digs = if root_est < 1.0 {
            0_i16
        } else {
            i16::try_from(root_est.log10().floor() as i32 + 1).unwrap_or(1)
        };
        let ws = (38_i16 - int_digs).clamp(26, 37);
        let os = (ws - 1).min(36);
        (ws, os)
    } else {
        (TRANSCENDENTAL_SCALE + 2, TRANSCENDENTAL_SCALE)
    };
    let w = eval_sqrt_work(arg, work_s);
    Ok(w.rescale(out_s, RoundingMode::NearestAwayFromZero)
        .map_or(w, |(r, _)| r))
}

/// Evaluates `FUNCTION TAN(arg)` (`tan(arg)` for radian angle `arg`).
///
/// # Arguments
///
/// * `arg` - Angle in radians.
#[must_use]
pub fn math_tan(arg: &FixedDecimal) -> FixedDecimal {
    let (s, c) = eval_sin_cos_work(arg);
    let out_s = if (s.to_f64() / c.to_f64()).abs() < 1.0e35 {
        36
    } else {
        TRANSCENDENTAL_SCALE
    };
    fp_div(&s, &c, out_s)
}

/// Computes `dividend - divisor * round_int(dividend / divisor, q_mode)` using exact 256-bit arithmetic.
///
/// # Arguments
///
/// * `fn_name` - Intrinsic name (`"MOD"` or `"REM"`) for `EC-ARGUMENT-FUNCTION` reporting.
/// * `dividend` - Numerator operand.
/// * `divisor` - Non-zero denominator operand.
/// * `q_mode` - Integer quotient rounding mode (`TowardLesser` for `MOD`, `Truncation` for `REM`).
fn eval_remainder_with_mode(
    fn_name: &'static str,
    dividend: &FixedDecimal,
    divisor: &FixedDecimal,
    q_mode: RoundingMode,
) -> Result<FixedDecimal> {
    let (q_int, q_status) = dividend
        .checked_div(divisor, 0, q_mode)
        .unwrap_or((FixedDecimal::ZERO, SizeErrorStatus::Overflowed));
    if q_status.is_error() {
        return Err(ec_argument_function(
            fn_name,
            format!("{dividend}, {divisor}"),
            "intermediate integer quotient exceeds 76-digit decimal range",
        ));
    }
    let (prod, _) = divisor
        .checked_mul(&q_int)
        .unwrap_or((FixedDecimal::ZERO, SizeErrorStatus::Exact));
    let (rem, _) = dividend
        .checked_sub(&prod)
        .unwrap_or((FixedDecimal::ZERO, SizeErrorStatus::Exact));
    Ok(rem)
}

/// Rounds a 38-fractional-digit working value to 36 fractional digits.
///
/// # Arguments
///
/// * `v` - Working precision [`FixedDecimal`].
#[must_use]
fn fp_round_out(v: &FixedDecimal) -> FixedDecimal {
    v.rescale(36, RoundingMode::NearestAwayFromZero)
        .map_or(*v, |(r, _)| r)
}

/// Adds `a + b` at fixed-point working precision.
///
/// # Arguments
///
/// * `a` - Left summand.
/// * `b` - Right summand.
#[must_use]
fn fp_add(a: &FixedDecimal, b: &FixedDecimal) -> FixedDecimal {
    a.checked_add(b).map_or(FixedDecimal::ZERO, |(s, _)| s)
}

/// Subtracts `a - b` at fixed-point working precision.
///
/// # Arguments
///
/// * `a` - Minuend.
/// * `b` - Subtrahend.
#[must_use]
fn fp_sub(a: &FixedDecimal, b: &FixedDecimal) -> FixedDecimal {
    a.checked_sub(b).map_or(FixedDecimal::ZERO, |(d, _)| d)
}

/// Multiplies `a * b` and rounds to `scale` fractional digits, rescaling down on 76-digit overflow.
///
/// # Arguments
///
/// * `a` - Left factor.
/// * `b` - Right factor.
/// * `scale` - Target fractional digit count.
#[must_use]
fn fp_mul(a: &FixedDecimal, b: &FixedDecimal, scale: i16) -> FixedDecimal {
    if let Ok((prod, st)) = a.checked_mul(b)
        && !st.is_error()
    {
        return prod
            .rescale(scale, RoundingMode::NearestAwayFromZero)
            .map_or(prod, |(r, _)| r);
    }
    let a28 = a
        .rescale(28, RoundingMode::NearestAwayFromZero)
        .map_or(*a, |(r, _)| r);
    let b28 = b
        .rescale(28, RoundingMode::NearestAwayFromZero)
        .map_or(*b, |(r, _)| r);
    let (prod, _) = a28
        .checked_mul(&b28)
        .unwrap_or((FixedDecimal::ZERO, SizeErrorStatus::Exact));
    prod.rescale(scale, RoundingMode::NearestAwayFromZero)
        .map_or(prod, |(r, _)| r)
}

/// Divides `a / b` rounded to `scale` fractional digits.
///
/// # Arguments
///
/// * `a` - Dividend.
/// * `b` - Non-zero divisor.
/// * `scale` - Target fractional digit count.
#[must_use]
fn fp_div(a: &FixedDecimal, b: &FixedDecimal, scale: i16) -> FixedDecimal {
    if let Ok((q, st)) = a.checked_div(b, scale, RoundingMode::NearestAwayFromZero)
        && !st.is_error()
    {
        return q;
    }
    let b24 = b
        .rescale(24, RoundingMode::NearestAwayFromZero)
        .map_or(*b, |(r, _)| r);
    let out_s = if a.to_f64().abs() < 1.0e14 {
        scale
    } else {
        scale.min(TRANSCENDENTAL_SCALE)
    };
    a.checked_div(&b24, out_s, RoundingMode::NearestAwayFromZero)
        .map_or(FixedDecimal::ZERO, |(q, _)| q)
}

/// Returns `pi / 2` to 38 fractional digits.
#[must_use]
fn const_half_pi_38() -> FixedDecimal {
    FixedDecimal::parse_str("1.57079632679489661923132169163975144210")
        .unwrap_or(FixedDecimal::ZERO)
}

/// Returns `2 * pi` to 38 fractional digits.
#[must_use]
fn const_two_pi_38() -> FixedDecimal {
    FixedDecimal::parse_str("6.28318530717958647692528676655900576839")
        .unwrap_or(FixedDecimal::ZERO)
}

/// Returns `ln(2)` to 38 fractional digits.
#[must_use]
fn const_ln2_38() -> FixedDecimal {
    FixedDecimal::parse_str("0.69314718055994530941723212145817656808")
        .unwrap_or(FixedDecimal::ZERO)
}

/// Returns `ln(10)` to 38 fractional digits.
#[must_use]
fn const_ln10_38() -> FixedDecimal {
    FixedDecimal::parse_str("2.30258509299404568401799145468436420760")
        .unwrap_or(FixedDecimal::ZERO)
}

/// Computes `sqrt(x)` (`x > 0`) to `work_scale` fractional digits via 8-step Newton-Raphson.
///
/// # Arguments
///
/// * `x` - Strictly positive operand.
/// * `work_scale` - Working fractional digit precision (`<= 38`).
#[must_use]
fn eval_sqrt_work(x: &FixedDecimal, work_scale: i16) -> FixedDecimal {
    let seed_f = x.to_f64().sqrt().max(1.0e-35);
    let mut y = FixedDecimal::from_f64(seed_f, work_scale, RoundingMode::NearestAwayFromZero)
        .map_or(FixedDecimal::from_i64(1), |(d, _)| d);
    let two = FixedDecimal::from_i64(2);
    for _ in 0..8 {
        let q = fp_div(x, &y, work_scale);
        y = fp_div(&fp_add(&y, &q), &two, work_scale);
    }
    y
}

/// Evaluates `exp(arg)` to 36 fractional digits (or 18 fractional digits when `arg > 80`).
///
/// # Arguments
///
/// * `arg` - Real exponent (`<= 130`).
#[must_use]
fn eval_exp_high_prec(arg: &FixedDecimal) -> FixedDecimal {
    let xf = arg.to_f64();
    if xf < -88.0 {
        return FixedDecimal::ZERO;
    }
    let ln2 = const_ln2_38();
    let k_dec = arg
        .checked_div(&ln2, 0, RoundingMode::NearestAwayFromZero)
        .map_or(FixedDecimal::ZERO, |(q, _)| q);
    let k_i64 = k_dec.to_string().parse::<i64>().unwrap_or(0);
    let k_ln2 = fp_mul(&k_dec, &ln2, 38);
    let r = arg
        .checked_sub(&k_ln2)
        .map_or(FixedDecimal::ZERO, |(d, _)| d)
        .rescale(38, RoundingMode::NearestAwayFromZero)
        .map_or(FixedDecimal::ZERO, |(v, _)| v);

    // Halve 4 times so `|u| <= ln(2) / 32 < 0.022`.
    let sixteen = FixedDecimal::from_i64(16);
    let u = fp_div(&r, &sixteen, 38);
    let mut sum = FixedDecimal::from_i64(1);
    let mut term = FixedDecimal::from_i64(1);
    for n in 1_i64..=19 {
        term = fp_div(&fp_mul(&term, &u, 38), &FixedDecimal::from_i64(n), 38);
        sum = fp_add(&sum, &term);
    }
    for _ in 0..4 {
        sum = fp_mul(&sum, &sum, 38);
    }

    let out_s = if k_i64 <= 110 {
        36
    } else {
        TRANSCENDENTAL_SCALE
    };
    let work_s = out_s + 2;
    let mut scaled = sum
        .rescale(work_s, RoundingMode::NearestAwayFromZero)
        .map_or(sum, |(v, _)| v);
    let two = FixedDecimal::from_i64(2);
    if k_i64 > 0 {
        let mut rem = u32::try_from(k_i64).unwrap_or(0);
        while rem > 0 {
            let step = rem.min(60);
            let factor = FixedDecimal::try_new(
                SignedInt256::from_parts(Uint256::from_u64(1_u64 << step), DecimalSign::Positive),
                0,
            )
            .unwrap_or(two);
            scaled = scaled
                .checked_mul(&factor)
                .map_or(scaled, |(p, _)| p)
                .rescale(work_s, RoundingMode::NearestAwayFromZero)
                .map_or(scaled, |(v, _)| v);
            rem -= step;
        }
    } else if k_i64 < 0 {
        let mut rem = u32::try_from(-k_i64).unwrap_or(0);
        while rem > 0 {
            let step = rem.min(60);
            let factor = FixedDecimal::try_new(
                SignedInt256::from_parts(Uint256::from_u64(1_u64 << step), DecimalSign::Positive),
                0,
            )
            .unwrap_or(two);
            scaled = fp_div(&scaled, &factor, work_s);
            rem -= step;
        }
    }
    scaled
        .rescale(out_s, RoundingMode::NearestAwayFromZero)
        .map_or(scaled, |(v, _)| v)
}

/// Evaluates `ln(arg)` (`arg > 0`) to 38 fractional digits via decimal/binary reduction + `atanh` series.
///
/// # Arguments
///
/// * `arg` - Strictly positive operand.
#[must_use]
fn eval_ln_work(arg: &FixedDecimal) -> FixedDecimal {
    let digits_str = arg.mantissa().magnitude().to_string();
    let d_len = i16::try_from(digits_str.len().max(1)).unwrap_or(1);
    let norm_scale = (d_len - 1).clamp(0, 75);
    let mut m = FixedDecimal::try_new(
        SignedInt256::from_parts(arg.mantissa().magnitude(), DecimalSign::Positive),
        norm_scale,
    )
    .unwrap_or(FixedDecimal::from_i64(1))
    .rescale(38, RoundingMode::NearestAwayFromZero)
    .unwrap_or((FixedDecimal::from_i64(1), SizeErrorStatus::Exact))
    .0;
    let pow10_diff = i64::from(norm_scale) - i64::from(arg.scale());

    let threshold =
        FixedDecimal::parse_str("1.414213562373095").unwrap_or(FixedDecimal::from_i64(2));
    let two = FixedDecimal::from_i64(2);
    let mut k2 = 0_i64;
    while m.cmp_numeric(&threshold).is_gt() {
        m = fp_div(&m, &two, 38);
        k2 += 1;
    }

    let one = FixedDecimal::from_i64(1);
    let z = fp_div(&fp_sub(&m, &one), &fp_add(&m, &one), 38);
    let z2 = fp_mul(&z, &z, 38);
    let mut term = z;
    let mut series = z;
    for denom in (3_i64..=57).step_by(2) {
        term = fp_mul(&term, &z2, 38);
        let contrib = fp_div(&term, &FixedDecimal::from_i64(denom), 38);
        series = fp_add(&series, &contrib);
    }
    let ln_m = fp_mul(&series, &two, 38);
    let k2_part = fp_mul(&FixedDecimal::from_i64(k2), &const_ln2_38(), 38);
    let p10_part = fp_mul(&FixedDecimal::from_i64(pow10_diff), &const_ln10_38(), 38);
    fp_add(&fp_add(&ln_m, &k2_part), &p10_part)
}

/// Evaluates `(sin(arg), cos(arg))` to 38 fractional digits via `2*pi` reduction and 6-fold half-angle Taylor.
///
/// # Arguments
///
/// * `arg` - Angle in radians.
#[must_use]
fn eval_sin_cos_work(arg: &FixedDecimal) -> (FixedDecimal, FixedDecimal) {
    if arg.is_zero() {
        return (FixedDecimal::ZERO, FixedDecimal::from_i64(1));
    }
    let two_pi = const_two_pi_38();
    let q = arg
        .checked_div(&two_pi, 0, RoundingMode::NearestAwayFromZero)
        .map_or(FixedDecimal::ZERO, |(v, _)| v);
    let q_prod = fp_mul(&q, &two_pi, 38);
    let rem = fp_sub(arg, &q_prod)
        .rescale(38, RoundingMode::NearestAwayFromZero)
        .map_or(FixedDecimal::ZERO, |(v, _)| v);

    let sixty_four = FixedDecimal::from_i64(64);
    let u = fp_div(&rem, &sixty_four, 38);
    let neg_u2 = fp_mul(&u, &u, 38).negate();

    let mut s_sum = u;
    let mut s_term = u;
    for k in 1_i64..=11 {
        let d = (2 * k) * (2 * k + 1);
        s_term = fp_div(
            &fp_mul(&s_term, &neg_u2, 38),
            &FixedDecimal::from_i64(d),
            38,
        );
        s_sum = fp_add(&s_sum, &s_term);
    }

    let mut c_sum = FixedDecimal::from_i64(1);
    let mut c_term = FixedDecimal::from_i64(1);
    for k in 1_i64..=11 {
        let d = (2 * k - 1) * (2 * k);
        c_term = fp_div(
            &fp_mul(&c_term, &neg_u2, 38),
            &FixedDecimal::from_i64(d),
            38,
        );
        c_sum = fp_add(&c_sum, &c_term);
    }

    let two = FixedDecimal::from_i64(2);
    let mut s = s_sum;
    let mut c = c_sum;
    for _ in 0..6 {
        let next_s = fp_mul(&two, &fp_mul(&s, &c, 38), 38);
        let next_c = fp_sub(&fp_mul(&c, &c, 38), &fp_mul(&s, &s, 38));
        s = next_s;
        c = next_c;
    }
    (s, c)
}

/// Evaluates `atan(arg)` to 38 fractional digits via reciprocal + 5-fold half-angle reduction.
///
/// # Arguments
///
/// * `arg` - Real tangent operand.
#[must_use]
fn eval_atan_work(arg: &FixedDecimal) -> FixedDecimal {
    if arg.is_zero() {
        return FixedDecimal::ZERO;
    }
    let neg = arg.is_negative();
    let ax = arg.abs();
    let one = FixedDecimal::from_i64(1);
    let (mut u, inverted) = if ax.cmp_numeric(&one).is_gt() {
        (fp_div(&one, &ax, 38), true)
    } else {
        (
            ax.rescale(38, RoundingMode::NearestAwayFromZero)
                .map_or(ax, |(v, _)| v),
            false,
        )
    };

    for _ in 0..5 {
        let u2 = fp_mul(&u, &u, 38);
        let hyp = eval_sqrt_work(&fp_add(&one, &u2), 38);
        u = fp_div(&u, &fp_add(&one, &hyp), 38);
    }

    let neg_u2 = fp_mul(&u, &u, 38).negate();
    let mut term = u;
    let mut sum = u;
    for denom in (3_i64..=31).step_by(2) {
        term = fp_mul(&term, &neg_u2, 38);
        let contrib = fp_div(&term, &FixedDecimal::from_i64(denom), 38);
        sum = fp_add(&sum, &contrib);
    }
    let mut res = fp_mul(&sum, &FixedDecimal::from_i64(32), 38);
    if inverted {
        res = fp_sub(&const_half_pi_38(), &res);
    }
    if neg { res.negate() } else { res }
}

/// Evaluates `asin(arg)` (`|arg| <= 1`) to 38 fractional digits.
///
/// # Arguments
///
/// * `arg` - Sine value in `[-1, +1]`.
#[must_use]
fn eval_asin_work(arg: &FixedDecimal) -> FixedDecimal {
    if arg.is_zero() {
        return FixedDecimal::ZERO;
    }
    let one = FixedDecimal::from_i64(1);
    if arg.abs().cmp_numeric(&one).is_eq() {
        let hp = const_half_pi_38();
        return if arg.is_negative() { hp.negate() } else { hp };
    }
    let x_w = arg
        .rescale(38, RoundingMode::NearestAwayFromZero)
        .map_or(*arg, |(v, _)| v);
    let one_minus_x2 = fp_sub(&one, &fp_mul(&x_w, &x_w, 38));
    let denom = eval_sqrt_work(&one_minus_x2, 38);
    let ratio = fp_div(&x_w, &denom, 38);
    eval_atan_work(&ratio)
}
