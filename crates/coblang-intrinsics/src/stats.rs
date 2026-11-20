//! Financial, statistical, variadic/`ALL`-slice, and deterministic PRNG intrinsics (`Milestone M12`).

use core::cmp::Ordering;

use coblang_common::Result;
use coblang_decimal::{
    DecimalSign, FixedDecimal, RoundingMode, SignedInt256, SizeErrorStatus, Uint256,
};

use crate::ec_argument_function;

/// Fractional digit precision for statistical/financial real results (`36` digits).
const STATS_REAL_SCALE: i16 = 36;

/// Fractional digit precision for `FUNCTION RANDOM` pseudo-random outputs in `[0, 1)` (`18` digits).
const RANDOM_OUTPUT_SCALE: i16 = 18;

/// Modulo ceiling (`10^18`) mapping a 64-bit PRNG word into `[0, 1)` at scale `18`.
const RANDOM_MANTISSA_MODULUS: u64 = 1_000_000_000_000_000_000;

/// Numeric argument item supporting scalar operands and `TABLE(ALL)` slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumericSeriesArg<'a> {
    /// Single scalar numeric operand.
    Scalar(FixedDecimal),
    /// Expanded `ALL`-subscripted table slice in subscript order.
    TableAll(&'a [FixedDecimal]),
}

/// Alphanumeric argument item supporting scalar byte slices and `TABLE(ALL)` slices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaSeriesArg<'a> {
    /// Single scalar alphanumeric operand.
    Scalar(&'a [u8]),
    /// Expanded `ALL`-subscripted alphanumeric table slice in subscript order.
    TableAll(&'a [&'a [u8]]),
}

/// Seed policy for `FUNCTION RANDOM([seed])`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RandomSeed<'a> {
    /// Continue stepping the current deterministic PRNG stream (`FUNCTION RANDOM`).
    Unseeded,
    /// Re-seed the PRNG stream deterministically before drawing (`FUNCTION RANDOM(seed)`).
    Seeded(&'a FixedDecimal),
}

/// Deterministic run-unit PRNG state backing `FUNCTION RANDOM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RandomGenerator {
    /// Internal 64-bit `SplitMix64` state word.
    state: u64,
}

impl Default for RandomGenerator {
    /// Constructs a [`RandomGenerator`] initialized with the canonical run-unit default seed (`0`).
    fn default() -> Self {
        Self::new()
    }
}

impl RandomGenerator {
    /// Constructs a [`RandomGenerator`] initialized to the canonical run-unit initial seed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: mix_seed_to_state(0),
        }
    }

    /// Resets the generator deterministically from `seed_u64`.
    ///
    /// # Arguments
    ///
    /// * `seed_u64` - Validated non-negative integer seed.
    pub const fn reseed(&mut self, seed_u64: u64) {
        self.state = mix_seed_to_state(seed_u64);
    }

    /// Advances the `SplitMix64` state by one step and returns the mixed 64-bit output word.
    const fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Flattens mixed scalar and `ALL` table-slice [`NumericSeriesArg`] items into a contiguous `Vec`.
///
/// # Arguments
///
/// * `args` - Ordered slice of scalar and `ALL`-table numeric arguments.
#[must_use]
pub fn expand_numeric_series(args: &[NumericSeriesArg<'_>]) -> Vec<FixedDecimal> {
    let mut out = Vec::with_capacity(args.len());
    for item in args {
        match item {
            NumericSeriesArg::Scalar(v) => out.push(*v),
            NumericSeriesArg::TableAll(slice) => out.extend_from_slice(slice),
        }
    }
    out
}

/// Flattens mixed scalar and `ALL` table-slice [`AlphaSeriesArg`] items into a contiguous `Vec`.
///
/// # Arguments
///
/// * `args` - Ordered slice of scalar and `ALL`-table alphanumeric arguments.
#[must_use]
pub fn expand_alpha_series<'a>(args: &[AlphaSeriesArg<'a>]) -> Vec<&'a [u8]> {
    let mut out = Vec::with_capacity(args.len());
    for item in args {
        match item {
            AlphaSeriesArg::Scalar(b) => out.push(*b),
            AlphaSeriesArg::TableAll(slice) => out.extend_from_slice(slice),
        }
    }
    out
}

/// Evaluates `FUNCTION ANNUITY(interest_rate, periods)`.
///
/// Computes `1 / periods` when `interest_rate == 0`, and `r / (1 - (1 + r)^(-periods))` when `r > 0`.
///
/// # Arguments
///
/// * `interest_rate` - Non-negative periodic interest rate (`>= 0`).
/// * `periods` - Positive integer period count (`>= 1`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `interest_rate < 0` or `periods` is not a positive integer.
pub fn stats_annuity(interest_rate: &FixedDecimal, periods: &FixedDecimal) -> Result<FixedDecimal> {
    if interest_rate.is_negative() {
        return Err(ec_argument_function(
            "ANNUITY",
            format!("{interest_rate}, {periods}"),
            "interest rate must be >= 0",
        ));
    }
    let Some(n_i32) = periods.to_exact_i32() else {
        return Err(ec_argument_function(
            "ANNUITY",
            format!("{interest_rate}, {periods}"),
            "period count must be a positive integer",
        ));
    };
    if n_i32 < 1 {
        return Err(ec_argument_function(
            "ANNUITY",
            format!("{interest_rate}, {periods}"),
            "period count must be >= 1",
        ));
    }
    if interest_rate.is_zero() {
        let one = FixedDecimal::from_i64(1);
        let n_dec = FixedDecimal::from_i64(i64::from(n_i32));
        return Ok(one
            .checked_div(&n_dec, STATS_REAL_SCALE, RoundingMode::NearestAwayFromZero)
            .map_or(FixedDecimal::ZERO, |(q, _)| q));
    }
    let one = FixedDecimal::from_i64(1);
    let base = one.checked_add(interest_rate).map_or(one, |(s, _)| s);
    let mut exp_u = u32::try_from(n_i32).unwrap_or(1);
    // Since `interest_rate > 0`, `q = 1 / (1 + r)` is strictly in `(0, 1)` and `q^n` is in `[0, 1)`.
    let mut cur = one
        .checked_div(&base, 38, RoundingMode::NearestAwayFromZero)
        .map_or(FixedDecimal::ZERO, |(q, _)| q);
    let mut q_pow = one;
    while exp_u > 0 {
        if exp_u & 1 == 1 {
            q_pow = q_pow
                .checked_mul(&cur)
                .map_or(q_pow, |(p, _)| p)
                .rescale(38, RoundingMode::NearestAwayFromZero)
                .map_or(q_pow, |(v, _)| v);
        }
        exp_u >>= 1;
        if exp_u > 0 {
            cur = cur
                .checked_mul(&cur)
                .map_or(cur, |(p, _)| p)
                .rescale(38, RoundingMode::NearestAwayFromZero)
                .map_or(cur, |(v, _)| v);
        }
    }
    let denom = one
        .checked_sub(&q_pow)
        .map_or(FixedDecimal::ZERO, |(d, _)| d);
    if denom.is_zero() {
        let n_dec = FixedDecimal::from_i64(i64::from(n_i32));
        return Ok(one
            .checked_div(&n_dec, STATS_REAL_SCALE, RoundingMode::NearestAwayFromZero)
            .map_or(FixedDecimal::ZERO, |(q, _)| q));
    }
    Ok(interest_rate
        .checked_div(&denom, STATS_REAL_SCALE, RoundingMode::NearestAwayFromZero)
        .map_or(FixedDecimal::ZERO, |(ans, _)| ans))
}

/// Evaluates `FUNCTION PRESENT-VALUE(discount_rate, amounts)` (`sum_{k=1..m} amounts[k] / (1 + r)^k`).
///
/// # Arguments
///
/// * `discount_rate` - Periodic discount rate strictly greater than `-1`.
/// * `amounts` - Non-empty cash-flow series (`a_1 .. a_m`).
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `amounts` is empty, `discount_rate <= -1`, or the sum overflows.
pub fn stats_present_value(
    discount_rate: &FixedDecimal,
    amounts: &[FixedDecimal],
) -> Result<FixedDecimal> {
    if amounts.is_empty() {
        return Err(ec_argument_function(
            "PRESENT-VALUE",
            discount_rate.to_string(),
            "at least one cash-flow amount is required",
        ));
    }
    let neg_one = FixedDecimal::from_i64(-1);
    if discount_rate.cmp_numeric(&neg_one).is_le() {
        return Err(ec_argument_function(
            "PRESENT-VALUE",
            discount_rate.to_string(),
            "discount rate must be strictly greater than -1",
        ));
    }
    if discount_rate.is_zero() {
        return stats_sum(amounts);
    }
    let base = 1.0 + discount_rate.to_f64();
    let mut denom = 1.0_f64;
    let mut total = 0.0_f64;
    for amt in amounts {
        denom *= base;
        total += amt.to_f64() / denom;
    }
    quantize_checked_f64("PRESENT-VALUE", total)
}

/// Evaluates `FUNCTION MAX` over a numeric slice, returning the leftmost maximum value.
///
/// # Arguments
///
/// * `values` - Non-empty numeric series.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `values` is empty.
pub fn stats_max(values: &[FixedDecimal]) -> Result<FixedDecimal> {
    let idx = stats_ord_max(values)?;
    Ok(values.get(idx - 1).copied().unwrap_or(FixedDecimal::ZERO))
}

/// Evaluates `FUNCTION MAX` over mixed scalar and `ALL` table-slice numeric arguments.
///
/// # Arguments
///
/// * `args` - Variadic/table-slice numeric arguments.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if the expanded series is empty.
pub fn stats_max_series(args: &[NumericSeriesArg<'_>]) -> Result<FixedDecimal> {
    let flat = expand_numeric_series(args);
    stats_max(&flat)
}

/// Evaluates `FUNCTION MIN` over a numeric slice, returning the leftmost minimum value.
///
/// # Arguments
///
/// * `values` - Non-empty numeric series.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `values` is empty.
pub fn stats_min(values: &[FixedDecimal]) -> Result<FixedDecimal> {
    let idx = stats_ord_min(values)?;
    Ok(values.get(idx - 1).copied().unwrap_or(FixedDecimal::ZERO))
}

/// Evaluates `FUNCTION MIN` over mixed scalar and `ALL` table-slice numeric arguments.
///
/// # Arguments
///
/// * `args` - Variadic/table-slice numeric arguments.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if the expanded series is empty.
pub fn stats_min_series(args: &[NumericSeriesArg<'_>]) -> Result<FixedDecimal> {
    let flat = expand_numeric_series(args);
    stats_min(&flat)
}

/// Evaluates `FUNCTION ORD-MAX` over a numeric slice, returning the 1-based index of the leftmost maximum.
///
/// # Arguments
///
/// * `values` - Non-empty numeric series.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `values` is empty.
pub fn stats_ord_max(values: &[FixedDecimal]) -> Result<usize> {
    let Some((first, rest)) = values.split_first() else {
        return Err(ec_argument_function(
            "ORD-MAX",
            "",
            "at least one argument is required",
        ));
    };
    let mut best = first;
    let mut best_ord = 1_usize;
    for (offset, candidate) in rest.iter().enumerate() {
        if candidate.cmp_numeric(best).is_gt() {
            best = candidate;
            best_ord = offset + 2;
        }
    }
    Ok(best_ord)
}

/// Evaluates `FUNCTION ORD-MAX` over mixed scalar and `ALL` table-slice numeric arguments.
///
/// # Arguments
///
/// * `args` - Variadic/table-slice numeric arguments.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if the expanded series is empty.
pub fn stats_ord_max_series(args: &[NumericSeriesArg<'_>]) -> Result<usize> {
    let flat = expand_numeric_series(args);
    stats_ord_max(&flat)
}

/// Evaluates `FUNCTION ORD-MIN` over a numeric slice, returning the 1-based index of the leftmost minimum.
///
/// # Arguments
///
/// * `values` - Non-empty numeric series.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `values` is empty.
pub fn stats_ord_min(values: &[FixedDecimal]) -> Result<usize> {
    let Some((first, rest)) = values.split_first() else {
        return Err(ec_argument_function(
            "ORD-MIN",
            "",
            "at least one argument is required",
        ));
    };
    let mut best = first;
    let mut best_ord = 1_usize;
    for (offset, candidate) in rest.iter().enumerate() {
        if candidate.cmp_numeric(best).is_lt() {
            best = candidate;
            best_ord = offset + 2;
        }
    }
    Ok(best_ord)
}

/// Evaluates `FUNCTION ORD-MIN` over mixed scalar and `ALL` table-slice numeric arguments.
///
/// # Arguments
///
/// * `args` - Variadic/table-slice numeric arguments.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if the expanded series is empty.
pub fn stats_ord_min_series(args: &[NumericSeriesArg<'_>]) -> Result<usize> {
    let flat = expand_numeric_series(args);
    stats_ord_min(&flat)
}

/// Evaluates `FUNCTION MAX` over alphanumeric operands using ISO space-padded comparison.
///
/// # Arguments
///
/// * `args` - Scalar and `ALL`-slice alphanumeric operands.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if the expanded series is empty.
pub fn stats_max_alpha<'a>(args: &[AlphaSeriesArg<'a>]) -> Result<&'a [u8]> {
    let flat = expand_alpha_series(args);
    let idx = stats_ord_max_alpha_slice(&flat)?;
    Ok(flat.get(idx - 1).copied().unwrap_or(b""))
}

/// Evaluates `FUNCTION MIN` over alphanumeric operands using ISO space-padded comparison.
///
/// # Arguments
///
/// * `args` - Scalar and `ALL`-slice alphanumeric operands.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if the expanded series is empty.
pub fn stats_min_alpha<'a>(args: &[AlphaSeriesArg<'a>]) -> Result<&'a [u8]> {
    let flat = expand_alpha_series(args);
    let idx = stats_ord_min_alpha_slice(&flat)?;
    Ok(flat.get(idx - 1).copied().unwrap_or(b""))
}

/// Evaluates `FUNCTION ORD-MAX` over alphanumeric operands using ISO space-padded comparison.
///
/// # Arguments
///
/// * `args` - Scalar and `ALL`-slice alphanumeric operands.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if the expanded series is empty.
pub fn stats_ord_max_alpha(args: &[AlphaSeriesArg<'_>]) -> Result<usize> {
    let flat = expand_alpha_series(args);
    stats_ord_max_alpha_slice(&flat)
}

/// Evaluates `FUNCTION ORD-MIN` over alphanumeric operands using ISO space-padded comparison.
///
/// # Arguments
///
/// * `args` - Scalar and `ALL`-slice alphanumeric operands.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if the expanded series is empty.
pub fn stats_ord_min_alpha(args: &[AlphaSeriesArg<'_>]) -> Result<usize> {
    let flat = expand_alpha_series(args);
    stats_ord_min_alpha_slice(&flat)
}

/// Evaluates `FUNCTION SUM` over a numeric slice using exact 256-bit decimal addition.
///
/// # Arguments
///
/// * `values` - Non-empty numeric series.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `values` is empty or the accumulated sum overflows 76 digits.
pub fn stats_sum(values: &[FixedDecimal]) -> Result<FixedDecimal> {
    let Some((first, rest)) = values.split_first() else {
        return Err(ec_argument_function(
            "SUM",
            "",
            "at least one argument is required",
        ));
    };
    let mut acc = *first;
    for item in rest {
        let (next, status) = acc
            .checked_add(item)
            .unwrap_or((FixedDecimal::ZERO, SizeErrorStatus::Overflowed));
        if status.is_error() {
            return Err(ec_argument_function(
                "SUM",
                item.to_string(),
                "accumulated decimal sum exceeds 76-digit capacity",
            ));
        }
        acc = next;
    }
    Ok(acc)
}

/// Evaluates `FUNCTION SUM` over mixed scalar and `ALL` table-slice numeric arguments.
///
/// # Arguments
///
/// * `args` - Variadic/table-slice numeric arguments.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if the expanded series is empty or overflows.
pub fn stats_sum_series(args: &[NumericSeriesArg<'_>]) -> Result<FixedDecimal> {
    let flat = expand_numeric_series(args);
    stats_sum(&flat)
}

/// Evaluates `FUNCTION MEAN` (`SUM / N`).
///
/// # Arguments
///
/// * `values` - Non-empty numeric series.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `values` is empty or `SUM` overflows.
pub fn stats_mean(values: &[FixedDecimal]) -> Result<FixedDecimal> {
    let total = stats_sum(values)?;
    let count_u64 = u64::try_from(values.len()).unwrap_or(u64::MAX);
    let count_dec = FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::from_u64(count_u64), DecimalSign::Positive),
        0,
    )
    .unwrap_or(FixedDecimal::from_i64(1));
    Ok(total
        .checked_div(
            &count_dec,
            STATS_REAL_SCALE,
            RoundingMode::NearestAwayFromZero,
        )
        .map_or(FixedDecimal::ZERO, |(q, _)| q))
}

/// Evaluates `FUNCTION MEDIAN` over a numeric slice.
///
/// # Arguments
///
/// * `values` - Non-empty numeric series.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `values` is empty.
pub fn stats_median(values: &[FixedDecimal]) -> Result<FixedDecimal> {
    if values.is_empty() {
        return Err(ec_argument_function(
            "MEDIAN",
            "",
            "at least one argument is required",
        ));
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(FixedDecimal::cmp_numeric);
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        return Ok(sorted.get(mid).copied().unwrap_or(FixedDecimal::ZERO));
    }
    let lo = sorted.get(mid - 1).copied().unwrap_or(FixedDecimal::ZERO);
    let hi = sorted.get(mid).copied().unwrap_or(FixedDecimal::ZERO);
    average_pair(&lo, &hi, "MEDIAN")
}

/// Evaluates `FUNCTION MIDRANGE` (`(MIN + MAX) / 2`).
///
/// # Arguments
///
/// * `values` - Non-empty numeric series.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `values` is empty or `MIN + MAX` overflows.
pub fn stats_midrange(values: &[FixedDecimal]) -> Result<FixedDecimal> {
    if values.is_empty() {
        return Err(ec_argument_function(
            "MIDRANGE",
            "",
            "at least one argument is required",
        ));
    }
    let min_val = stats_min(values).unwrap_or(FixedDecimal::ZERO);
    let max_val = stats_max(values).unwrap_or(FixedDecimal::ZERO);
    average_pair(&min_val, &max_val, "MIDRANGE")
}

/// Evaluates `FUNCTION RANGE` (`MAX - MIN >= 0`).
///
/// # Arguments
///
/// * `values` - Non-empty numeric series.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `values` is empty or `MAX - MIN` overflows.
pub fn stats_range(values: &[FixedDecimal]) -> Result<FixedDecimal> {
    if values.is_empty() {
        return Err(ec_argument_function(
            "RANGE",
            "",
            "at least one argument is required",
        ));
    }
    let min_val = stats_min(values).unwrap_or(FixedDecimal::ZERO);
    let max_val = stats_max(values).unwrap_or(FixedDecimal::ZERO);
    let (diff, status) = max_val
        .checked_sub(&min_val)
        .unwrap_or((FixedDecimal::ZERO, SizeErrorStatus::Overflowed));
    if status.is_error() {
        return Err(ec_argument_function(
            "RANGE",
            format!("{max_val} - {min_val}"),
            "range difference exceeds 76-digit decimal capacity",
        ));
    }
    Ok(diff)
}

/// Evaluates `FUNCTION VARIANCE` (ISO COBOL population variance `(1 / N) * sum((x - mean)^2)`).
///
/// # Arguments
///
/// * `values` - Non-empty numeric series.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `values` is empty or squared deviations overflow.
pub fn stats_variance(values: &[FixedDecimal]) -> Result<FixedDecimal> {
    if values.is_empty() {
        return Err(ec_argument_function(
            "VARIANCE",
            "",
            "at least one argument is required",
        ));
    }
    if values.len() == 1 {
        return Ok(FixedDecimal::ZERO);
    }
    let count_u64 = u64::try_from(values.len()).unwrap_or(1);
    let count_dec = FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::from_u64(count_u64), DecimalSign::Positive),
        0,
    )
    .unwrap_or(FixedDecimal::from_i64(1));
    let max_abs = values
        .iter()
        .map(|v| v.to_f64().abs())
        .fold(0.0_f64, f64::max);
    if max_abs < 1.0e10 {
        let work_s: i16 = if max_abs < 100.0 { 36 } else { 26 };
        let out_s: i16 = if max_abs < 1.0e8 {
            STATS_REAL_SCALE
        } else {
            18
        };
        let total = values.iter().fold(FixedDecimal::ZERO, |acc, v| {
            acc.checked_add(v).map_or(acc, |(s, _)| s)
        });
        let mean_w = total
            .checked_div(&count_dec, work_s, RoundingMode::NearestAwayFromZero)
            .map_or(FixedDecimal::ZERO, |(m, _)| m);
        let mut sq_acc = FixedDecimal::ZERO;
        for v in values {
            let diff = v
                .checked_sub(&mean_w)
                .map_or(FixedDecimal::ZERO, |(d, _)| d)
                .rescale(work_s, RoundingMode::NearestAwayFromZero)
                .map_or(FixedDecimal::ZERO, |(d, _)| d);
            let sq = diff
                .checked_mul(&diff)
                .map_or(FixedDecimal::ZERO, |(p, _)| p)
                .rescale(work_s, RoundingMode::NearestAwayFromZero)
                .map_or(FixedDecimal::ZERO, |(r, _)| r);
            sq_acc = sq_acc.checked_add(&sq).map_or(sq_acc, |(s, _)| s);
        }
        return Ok(sq_acc
            .checked_div(&count_dec, out_s, RoundingMode::NearestAwayFromZero)
            .map_or(FixedDecimal::ZERO, |(var_val, _)| var_val));
    }
    let n = f64::from(u32::try_from(values.len()).unwrap_or(u32::MAX));
    let mean = values.iter().map(FixedDecimal::to_f64).sum::<f64>() / n;
    let sq_sum = values
        .iter()
        .map(|v| {
            let d = v.to_f64() - mean;
            d * d
        })
        .sum::<f64>();
    quantize_checked_f64("VARIANCE", sq_sum / n)
}

/// Evaluates `FUNCTION STANDARD-DEVIATION` (`sqrt(VARIANCE)`).
///
/// # Arguments
///
/// * `values` - Non-empty numeric series.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if `values` is empty or `VARIANCE` overflows.
pub fn stats_standard_deviation(values: &[FixedDecimal]) -> Result<FixedDecimal> {
    if values.is_empty() {
        return Err(ec_argument_function(
            "STANDARD-DEVIATION",
            "",
            "at least one argument is required",
        ));
    }
    let var = stats_variance(values)?;
    if var.is_zero() {
        return Ok(FixedDecimal::ZERO);
    }
    crate::math::math_sqrt(&var)
}

/// Evaluates `FUNCTION RANDOM([seed])` on `rng`, returning a deterministic value in `[0, 1)`.
///
/// # Arguments
///
/// * `rng` - Mutable [`RandomGenerator`] state for the active run unit.
/// * `seed` - [`RandomSeed::Unseeded`] or [`RandomSeed::Seeded`] non-negative integer seed.
///
/// # Errors
///
/// Returns `EC-ARGUMENT-FUNCTION` if a supplied `seed` is negative, fractional, or exceeds `u64::MAX`.
pub fn stats_random(rng: &mut RandomGenerator, seed: RandomSeed<'_>) -> Result<FixedDecimal> {
    if let RandomSeed::Seeded(seed_dec) = seed {
        let seed_u64 = extract_non_negative_u64_seed(seed_dec)?;
        rng.reseed(seed_u64);
    }
    let raw = rng.next_u64() % RANDOM_MANTISSA_MODULUS;
    Ok(FixedDecimal::try_new(
        SignedInt256::from_parts(Uint256::from_u64(raw), DecimalSign::Positive),
        RANDOM_OUTPUT_SCALE,
    )
    .unwrap_or(FixedDecimal::ZERO))
}

/// Mixes a user `u64` seed into a non-degenerate initial `SplitMix64` state.
///
/// # Arguments
///
/// * `seed` - User-provided non-negative seed.
#[must_use]
const fn mix_seed_to_state(seed: u64) -> u64 {
    seed ^ 0xD1B5_4A32_D192_ED03
}

/// Validates and extracts a non-negative integer `u64` seed from `seed_dec`.
///
/// # Arguments
///
/// * `seed_dec` - Candidate `RANDOM` seed operand.
fn extract_non_negative_u64_seed(seed_dec: &FixedDecimal) -> Result<u64> {
    if seed_dec.is_negative() {
        return Err(ec_argument_function(
            "RANDOM",
            seed_dec.to_string(),
            "RANDOM seed must be >= 0",
        ));
    }
    let (unscaled, status) = seed_dec
        .rescale(0, RoundingMode::Prohibited)
        .unwrap_or((*seed_dec, SizeErrorStatus::Overflowed));
    if status.is_error() {
        return Err(ec_argument_function(
            "RANDOM",
            seed_dec.to_string(),
            "RANDOM seed must be an exact integer",
        ));
    }
    let Some(v128) = unscaled.mantissa().to_i128() else {
        return Err(ec_argument_function(
            "RANDOM",
            seed_dec.to_string(),
            "RANDOM seed exceeds u64::MAX",
        ));
    };
    u64::try_from(v128).map_err(|_| {
        ec_argument_function(
            "RANDOM",
            seed_dec.to_string(),
            "RANDOM seed exceeds u64::MAX",
        )
    })
}

/// Computes `(a + b) / 2` using exact 256-bit arithmetic.
///
/// # Arguments
///
/// * `a` - Left operand.
/// * `b` - Right operand.
/// * `fn_name` - Intrinsic name for overflow diagnostics.
fn average_pair(a: &FixedDecimal, b: &FixedDecimal, fn_name: &'static str) -> Result<FixedDecimal> {
    let (sum, status) = a
        .checked_add(b)
        .unwrap_or((FixedDecimal::ZERO, SizeErrorStatus::Overflowed));
    if status.is_error() {
        return Err(ec_argument_function(
            fn_name,
            format!("{a}, {b}"),
            "intermediate sum exceeds 76-digit decimal capacity",
        ));
    }
    let two = FixedDecimal::from_i64(2);
    let div_scale = sum.scale().saturating_add(1).clamp(1, STATS_REAL_SCALE);
    Ok(sum
        .checked_div(&two, div_scale, RoundingMode::NearestAwayFromZero)
        .map_or(FixedDecimal::ZERO, |(q, _)| q))
}

/// Quantizes `val` into a `FixedDecimal`, raising `EC-ARGUMENT-FUNCTION` if non-finite or `>= 10^57`.
///
/// # Arguments
///
/// * `fn_name` - Calling intrinsic name.
/// * `val` - Computed `f64` value.
fn quantize_checked_f64(fn_name: &'static str, val: f64) -> Result<FixedDecimal> {
    if !val.is_finite() || val.abs() >= 1.0e57 {
        return Err(ec_argument_function(
            fn_name,
            val.to_string(),
            "result overflows numeric decimal capacity",
        ));
    }
    let scale = if val.abs() < 1.0e38 {
        STATS_REAL_SCALE
    } else {
        18
    };
    Ok(
        FixedDecimal::from_f64(val, scale, RoundingMode::NearestAwayFromZero)
            .map_or(FixedDecimal::ZERO, |(d, _)| d),
    )
}

/// Compares two COBOL alphanumeric slices with right-hand space (`0x20`) padding.
///
/// # Arguments
///
/// * `lhs` - Left alphanumeric byte slice.
/// * `rhs` - Right alphanumeric byte slice.
#[must_use]
fn cmp_alpha_space_padded(lhs: &[u8], rhs: &[u8]) -> Ordering {
    let max_len = lhs.len().max(rhs.len());
    for i in 0..max_len {
        let a = lhs.get(i).copied().unwrap_or(b' ');
        let b = rhs.get(i).copied().unwrap_or(b' ');
        let ord = a.cmp(&b);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// Finds the 1-based index of the leftmost maximum in `values` under space-padded comparison.
///
/// # Arguments
///
/// * `values` - Flattened alphanumeric operand slice.
fn stats_ord_max_alpha_slice(values: &[&[u8]]) -> Result<usize> {
    let Some((first, rest)) = values.split_first() else {
        return Err(ec_argument_function(
            "ORD-MAX",
            "",
            "at least one argument is required",
        ));
    };
    let mut best = *first;
    let mut best_ord = 1_usize;
    for (offset, &candidate) in rest.iter().enumerate() {
        if cmp_alpha_space_padded(candidate, best) == Ordering::Greater {
            best = candidate;
            best_ord = offset + 2;
        }
    }
    Ok(best_ord)
}

/// Finds the 1-based index of the leftmost minimum in `values` under space-padded comparison.
///
/// # Arguments
///
/// * `values` - Flattened alphanumeric operand slice.
fn stats_ord_min_alpha_slice(values: &[&[u8]]) -> Result<usize> {
    let Some((first, rest)) = values.split_first() else {
        return Err(ec_argument_function(
            "ORD-MIN",
            "",
            "at least one argument is required",
        ));
    };
    let mut best = *first;
    let mut best_ord = 1_usize;
    for (offset, &candidate) in rest.iter().enumerate() {
        if cmp_alpha_space_padded(candidate, best) == Ordering::Less {
            best = candidate;
            best_ord = offset + 2;
        }
    }
    Ok(best_ord)
}
