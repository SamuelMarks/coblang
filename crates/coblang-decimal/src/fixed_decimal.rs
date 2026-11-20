//! Exact scaled fixed-point decimal (`FixedDecimal`) with `i128` fast path and 256-bit escalation.

use core::cmp::Ordering;
use core::fmt;

use coblang_common::{CoblangError, DigitCount, Result, ScaleFactor};

use crate::rounding::{RoundingMode, SizeErrorStatus, round_quotient_with_remainder};
use crate::wide_int::{DecimalSign, SignedInt256, Uint256};

/// Target high-order digit handling policy when fitting a [`FixedDecimal`] into a `PICTURE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HighOrderPolicy {
    /// Keep value unmutated or report [`SizeErrorStatus::Overflowed`] when high-order digits exceed `DigitCount`.
    FlagOverflow,
    /// Modulo-truncate high-order digits mod `10^digits` (standard COBOL `MOVE` / unguarded store).
    ModuloTruncate,
}

/// Target signedness policy when fitting a [`FixedDecimal`] into a destination `PICTURE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignednessPolicy {
    /// Unsigned target (`PIC 9...`): negative values drop their sign (`abs`) per ISO COBOL.
    Unsigned,
    /// Signed target (`PIC S9...`): preserves sign.
    Signed,
}

/// Scaled exact decimal number representing `mantissa * 10^-scale`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FixedDecimal {
    /// Signed 256-bit integer mantissa.
    mantissa: SignedInt256,
    /// Fractional digit count (positive = right of decimal point; negative = `P` high-order zeros).
    scale: i16,
}

impl FixedDecimal {
    /// Exact zero at scale `0`.
    pub const ZERO: Self = Self {
        mantissa: SignedInt256::ZERO,
        scale: 0,
    };

    /// Maximum supported intermediate absolute scale (`76`).
    pub const MAX_INTERMEDIATE_SCALE: i16 = 76;

    /// Default intermediate fractional precision for `COMPUTE` division (`38` digits).
    pub const DEFAULT_DIV_SCALE: i16 = 38;

    /// Fast-path `i128` magnitude ceiling ($10^{36}$, leaving headroom for addition/subtraction without overflow).
    const FAST_I128_LIMIT: i128 = 1_000_000_000_000_000_000_000_000_000_000_000_000;

    /// Constructs a [`FixedDecimal`] from `mantissa` and `scale`.
    ///
    /// # Arguments
    ///
    /// * `mantissa` - Signed 256-bit integer mantissa.
    /// * `scale` - Fractional scale in `-76..=76`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `scale.abs() > 76`.
    pub fn try_new(mantissa: SignedInt256, scale: i16) -> Result<Self> {
        if !(-Self::MAX_INTERMEDIATE_SCALE..=Self::MAX_INTERMEDIATE_SCALE).contains(&scale) {
            return Err(CoblangError::DecimalDomain {
                operation: "FixedDecimal::try_new",
                message: format!("scale {scale} outside -76..=76"),
            });
        }
        Ok(Self { mantissa, scale })
    }

    /// Constructs an unscaled integer [`FixedDecimal`] from `i64`.
    ///
    /// # Arguments
    ///
    /// * `val` - Signed 64-bit integer.
    #[must_use]
    pub const fn from_i64(val: i64) -> Self {
        Self {
            mantissa: SignedInt256::from_i64(val),
            scale: 0,
        }
    }

    /// Returns the arithmetically negated [`FixedDecimal`] (`0` remains `UnsignedZero`).
    #[must_use]
    pub const fn negated(&self) -> Self {
        Self {
            mantissa: self.mantissa.negate(),
            scale: self.scale,
        }
    }

    /// Constructs a [`FixedDecimal`] from `i128` and validated [`ScaleFactor`].
    ///
    /// # Arguments
    ///
    /// * `val` - Signed 128-bit mantissa.
    /// * `scale` - Validated COBOL [`ScaleFactor`].
    #[must_use]
    pub const fn from_i128_scaled(val: i128, scale: ScaleFactor) -> Self {
        Self {
            mantissa: SignedInt256::from_i128(val),
            scale: scale.get(),
        }
    }

    /// Parses an ASCII numeric literal (`[+|-][digits][.digits]`) into a [`FixedDecimal`].
    ///
    /// # Arguments
    ///
    /// * `text` - Trimmed or untrimmed decimal literal text.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `text` is empty, lacks digits, or exceeds 76 digits.
    pub fn parse_str(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        let (sign, body) = if let Some(rest) = trimmed.strip_prefix('-') {
            (DecimalSign::Negative, rest)
        } else if let Some(rest) = trimmed.strip_prefix('+') {
            (DecimalSign::Positive, rest)
        } else {
            (DecimalSign::Positive, trimmed)
        };

        let mut digits = Vec::with_capacity(body.len());
        let mut frac_digits: Option<i16> = None;
        for b in body.bytes() {
            if b == b'.' {
                if frac_digits.is_some() {
                    return Err(CoblangError::DecimalDomain {
                        operation: "FixedDecimal::parse_str",
                        message: String::from("multiple decimal points in numeric literal"),
                    });
                }
                frac_digits = Some(0);
            } else if b.is_ascii_digit() {
                digits.push(b);
                if let Some(fd) = &mut frac_digits {
                    *fd = fd.saturating_add(1);
                }
            } else {
                return Err(CoblangError::DecimalDomain {
                    operation: "FixedDecimal::parse_str",
                    message: format!("invalid character in decimal literal `{trimmed}`"),
                });
            }
        }

        let mag = Uint256::from_decimal_digits(&digits)?;
        let scale = frac_digits.unwrap_or(0);
        Self::try_new(SignedInt256::from_parts(mag, sign), scale)
    }

    /// Returns the underlying [`SignedInt256`] mantissa.
    #[must_use]
    pub const fn mantissa(&self) -> SignedInt256 {
        self.mantissa
    }

    /// Returns the fractional digit scale.
    #[must_use]
    pub const fn scale(&self) -> i16 {
        self.scale
    }

    /// Returns `true` when the numeric value equals zero.
    #[must_use]
    pub const fn is_zero(&self) -> bool {
        self.mantissa.is_zero()
    }

    /// Returns `true` when the numeric value is strictly negative (`< 0`).
    #[must_use]
    pub const fn is_negative(&self) -> bool {
        self.mantissa.is_negative()
    }

    /// Returns `-self` with identical scale.
    #[must_use]
    pub const fn negate(self) -> Self {
        Self {
            mantissa: self.mantissa.negate(),
            scale: self.scale,
        }
    }

    /// Returns `|self|` with identical scale.
    #[must_use]
    pub const fn abs(self) -> Self {
        Self {
            mantissa: self.mantissa.abs(),
            scale: self.scale,
        }
    }

    /// Rescales `self` to `target_scale` (`-76..=76`) using `mode`.
    ///
    /// # Arguments
    ///
    /// * `target_scale` - Desired fractional digit scale.
    /// * `mode` - Rounding mode when `target_scale < self.scale`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `target_scale` lies outside `-76..=76`.
    pub fn rescale(
        &self,
        target_scale: i16,
        mode: RoundingMode,
    ) -> Result<(Self, SizeErrorStatus)> {
        if !(-Self::MAX_INTERMEDIATE_SCALE..=Self::MAX_INTERMEDIATE_SCALE).contains(&target_scale) {
            return Err(CoblangError::DecimalDomain {
                operation: "FixedDecimal::rescale",
                message: format!("target scale {target_scale} outside -76..=76"),
            });
        }
        if target_scale == self.scale {
            return Ok((*self, SizeErrorStatus::Exact));
        }
        if target_scale > self.scale {
            let exp = u8::try_from(target_scale - self.scale).unwrap_or(u8::MAX);
            let Some(scaled) = self.mantissa.checked_mul_pow10(exp) else {
                return Ok((
                    Self {
                        mantissa: self.mantissa,
                        scale: target_scale,
                    },
                    SizeErrorStatus::Overflowed,
                ));
            };
            return Ok((
                Self {
                    mantissa: scaled,
                    scale: target_scale,
                },
                SizeErrorStatus::Exact,
            ));
        }

        let exp = u8::try_from(self.scale - target_scale).unwrap_or(u8::MAX);
        let Ok(divisor) = Uint256::pow10(exp) else {
            let zero_or_one = round_beyond_76(self.mantissa, mode);
            return Ok((
                Self {
                    mantissa: zero_or_one.0,
                    scale: target_scale,
                },
                zero_or_one.1,
            ));
        };
        let (q_mag, r_mag) = self.mantissa.magnitude().checked_div_rem(divisor)?;
        let (rounded, status) =
            round_quotient_with_remainder(q_mag, r_mag, divisor, self.mantissa.sign(), mode);
        Ok((
            Self {
                mantissa: rounded,
                scale: target_scale,
            },
            status,
        ))
    }

    /// Quantizes `self` to a destination `PICTURE` (`digits`, `scale`, `signed`) with `mode` and `high_order`.
    ///
    /// # Arguments
    ///
    /// * `digits` - Target `PICTURE` digit count (`1..=38`).
    /// * `scale` - Target `PICTURE` scale factor (`-38..=38`).
    /// * `signed` - Whether the destination retains negative signs.
    /// * `mode` - Rounding mode for excess low-order fractional digits.
    /// * `high_order` - Overflow vs modulo-truncation policy for high-order digits.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] only on internal scale bound violation.
    pub fn fit_to_picture(
        &self,
        digits: DigitCount,
        scale: ScaleFactor,
        signed: SignednessPolicy,
        mode: RoundingMode,
        high_order: HighOrderPolicy,
    ) -> Result<(Self, SizeErrorStatus)> {
        let (rescaled, round_status) = self.rescale(scale.get(), mode)?;
        if round_status.is_error() {
            return Ok((rescaled, round_status));
        }

        let limit = Uint256::pow10(digits.get())?;
        let mut mag = rescaled.mantissa.magnitude();
        let mut status = SizeErrorStatus::Exact;
        if mag >= limit {
            match high_order {
                HighOrderPolicy::FlagOverflow => {
                    status = SizeErrorStatus::Overflowed;
                }
                HighOrderPolicy::ModuloTruncate => {
                    let (_, rem) = mag.checked_div_rem(limit)?;
                    mag = rem;
                }
            }
        }

        let final_sign = match signed {
            SignednessPolicy::Unsigned => {
                if mag.is_zero() {
                    DecimalSign::UnsignedZero
                } else {
                    DecimalSign::Positive
                }
            }
            SignednessPolicy::Signed => rescaled.mantissa.sign(),
        };

        Ok((
            Self {
                mantissa: SignedInt256::from_parts(mag, final_sign),
                scale: scale.get(),
            },
            status,
        ))
    }

    /// Exact addition (`self + rhs`) with fast-path `i128` execution for `<= 36`-digit aligned operands.
    ///
    /// # Arguments
    ///
    /// * `rhs` - Addend.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if aligned scale exceeds `76`.
    pub fn checked_add(&self, rhs: &Self) -> Result<(Self, SizeErrorStatus)> {
        if self.scale == rhs.scale
            && let (Some(a), Some(b)) = (self.mantissa.to_i128(), rhs.mantissa.to_i128())
            && a.abs() <= Self::FAST_I128_LIMIT
            && b.abs() <= Self::FAST_I128_LIMIT
        {
            return Ok((
                Self {
                    mantissa: SignedInt256::from_i128(a + b),
                    scale: self.scale,
                },
                SizeErrorStatus::Exact,
            ));
        }

        let target_scale = self.scale.max(rhs.scale);
        let (lhs_aligned, st_l) = self.rescale(target_scale, RoundingMode::Truncation)?;
        let (rhs_aligned, st_r) = rhs.rescale(target_scale, RoundingMode::Truncation)?;
        if st_l.is_error() || st_r.is_error() {
            return Ok((*self, SizeErrorStatus::Overflowed));
        }
        let Some(sum) = lhs_aligned.mantissa.checked_add(rhs_aligned.mantissa) else {
            return Ok((*self, SizeErrorStatus::Overflowed));
        };
        Ok((
            Self {
                mantissa: sum,
                scale: target_scale,
            },
            SizeErrorStatus::Exact,
        ))
    }

    /// Exact subtraction (`self - rhs`).
    ///
    /// # Arguments
    ///
    /// * `rhs` - Subtrahend.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if aligned scale exceeds `76`.
    pub fn checked_sub(&self, rhs: &Self) -> Result<(Self, SizeErrorStatus)> {
        self.checked_add(&rhs.negate())
    }

    /// Exact multiplication (`self * rhs`), clamping excess intermediate scale > 76 via truncation if needed.
    ///
    /// # Arguments
    ///
    /// * `rhs` - Multiplier.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if combined scale underflows `-76` without representable shift.
    pub fn checked_mul(&self, rhs: &Self) -> Result<(Self, SizeErrorStatus)> {
        let Some(prod) = self.mantissa.checked_mul(rhs.mantissa) else {
            return Ok((*self, SizeErrorStatus::Overflowed));
        };
        let raw_scale = i32::from(self.scale) + i32::from(rhs.scale);
        if raw_scale > i32::from(Self::MAX_INTERMEDIATE_SCALE) {
            let excess = u8::try_from(raw_scale - i32::from(Self::MAX_INTERMEDIATE_SCALE))
                .unwrap_or(Uint256::MAX_DECIMAL_DIGITS);
            let div = Uint256::pow10(excess)?;
            let (q_mag, r_mag) = prod.magnitude().checked_div_rem(div)?;
            let (rounded, _) = round_quotient_with_remainder(
                q_mag,
                r_mag,
                div,
                prod.sign(),
                RoundingMode::NearestAwayFromZero,
            );
            return Ok((
                Self {
                    mantissa: rounded,
                    scale: Self::MAX_INTERMEDIATE_SCALE,
                },
                SizeErrorStatus::Exact,
            ));
        }
        if raw_scale < i32::from(-Self::MAX_INTERMEDIATE_SCALE) {
            let boost = u8::try_from(i32::from(-Self::MAX_INTERMEDIATE_SCALE) - raw_scale)
                .unwrap_or(Uint256::MAX_DECIMAL_DIGITS);
            let Some(boosted) = prod.checked_mul_pow10(boost) else {
                return Ok((*self, SizeErrorStatus::Overflowed));
            };
            return Ok((
                Self {
                    mantissa: boosted,
                    scale: -Self::MAX_INTERMEDIATE_SCALE,
                },
                SizeErrorStatus::Exact,
            ));
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Guarded by -76 <= raw_scale <= 76 above."
        )]
        Ok((
            Self {
                mantissa: prod,
                scale: raw_scale as i16,
            },
            SizeErrorStatus::Exact,
        ))
    }

    /// Divides `self` by `divisor` quantized to `target_scale` using `mode`.
    ///
    /// # Arguments
    ///
    /// * `divisor` - Denominator.
    /// * `target_scale` - Result fractional digit scale (`-76..=76`).
    /// * `mode` - Rounding mode.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `target_scale` lies outside `-76..=76`.
    pub fn checked_div(
        &self,
        divisor: &Self,
        target_scale: i16,
        mode: RoundingMode,
    ) -> Result<(Self, SizeErrorStatus)> {
        if !(-Self::MAX_INTERMEDIATE_SCALE..=Self::MAX_INTERMEDIATE_SCALE).contains(&target_scale) {
            return Err(CoblangError::DecimalDomain {
                operation: "FixedDecimal::checked_div",
                message: format!("target scale {target_scale} outside -76..=76"),
            });
        }
        if divisor.is_zero() {
            return Ok((Self::ZERO, SizeErrorStatus::DivisionByZero));
        }
        if self.is_zero() {
            return Ok((
                Self {
                    mantissa: SignedInt256::ZERO,
                    scale: target_scale,
                },
                SizeErrorStatus::Exact,
            ));
        }

        // Compute |self| / |divisor| at target_scale:
        // scaled_quotient = (|self.m| * 10^(target_scale - self.scale + divisor.scale)) / |divisor.m|
        let shift = i32::from(target_scale) - i32::from(self.scale) + i32::from(divisor.scale);
        let true_sign = self.mantissa.sign().mul_sign(divisor.mantissa.sign());

        let (num_mag, den_mag) = if shift >= 0 {
            let exp = u8::try_from(shift).unwrap_or(u8::MAX);
            let Some(scaled_num) = Uint256::pow10(exp)
                .ok()
                .and_then(|factor| self.mantissa.magnitude().checked_mul(factor))
            else {
                return Ok((*self, SizeErrorStatus::Overflowed));
            };
            (scaled_num, divisor.mantissa.magnitude())
        } else {
            let exp = u8::try_from(-shift).unwrap_or(u8::MAX);
            let Some(scaled_den) = Uint256::pow10(exp)
                .ok()
                .and_then(|factor| divisor.mantissa.magnitude().checked_mul(factor))
            else {
                let (m, st) =
                    round_beyond_76(SignedInt256::from_parts(Uint256::ONE, true_sign), mode);
                return Ok((
                    Self {
                        mantissa: m,
                        scale: target_scale,
                    },
                    st,
                ));
            };
            (self.mantissa.magnitude(), scaled_den)
        };

        let (q_mag, r_mag) = num_mag.checked_div_rem(den_mag)?;
        let (rounded, status) =
            round_quotient_with_remainder(q_mag, r_mag, den_mag, true_sign, mode);
        Ok((
            Self {
                mantissa: rounded,
                scale: target_scale,
            },
            status,
        ))
    }

    /// Computes ISO COBOL `DIVIDE self BY divisor GIVING q [ROUNDED] REMAINDER rem`,
    /// where `rem = self - (q * divisor)`.
    ///
    /// # Arguments
    ///
    /// * `divisor` - Denominator.
    /// * `quot_spec` - Target `(digits, scale, signed)` of the `GIVING` quotient item.
    /// * `mode` - Quotient rounding mode.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] on scale bound violation.
    pub fn checked_div_rem_cobol(
        &self,
        divisor: &Self,
        quot_spec: (DigitCount, ScaleFactor, SignednessPolicy),
        mode: RoundingMode,
    ) -> Result<(Self, Self, SizeErrorStatus)> {
        let (q_digits, q_scale, q_signed) = quot_spec;
        let (raw_q, div_status) = self.checked_div(divisor, q_scale.get(), mode)?;
        if div_status.is_error() {
            return Ok((Self::ZERO, Self::ZERO, div_status));
        }
        let (fitted_q, fit_status) = raw_q.fit_to_picture(
            q_digits,
            q_scale,
            q_signed,
            mode,
            HighOrderPolicy::FlagOverflow,
        )?;
        if fit_status.is_error() {
            return Ok((fitted_q, Self::ZERO, fit_status));
        }
        let (prod, prod_status) = fitted_q.checked_mul(divisor)?;
        if prod_status.is_error() {
            return Ok((fitted_q, Self::ZERO, prod_status));
        }
        let (rem, rem_status) = self.checked_sub(&prod)?;
        Ok((fitted_q, rem, rem_status))
    }

    /// Raises `self` to `exponent`, quantized to `target_scale` using `mode`.
    ///
    /// # Arguments
    ///
    /// * `exponent` - Signed integer or fractional exponent.
    /// * `target_scale` - Result fractional digit scale (`-76..=76`).
    /// * `mode` - Result rounding mode.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `target_scale` lies outside `-76..=76`.
    pub fn checked_pow(
        &self,
        exponent: &Self,
        target_scale: i16,
        mode: RoundingMode,
    ) -> Result<(Self, SizeErrorStatus)> {
        if self.is_zero() {
            if exponent.is_zero() {
                return Ok((Self::ZERO, SizeErrorStatus::InvalidExponent));
            }
            if exponent.is_negative() {
                return Ok((Self::ZERO, SizeErrorStatus::DivisionByZero));
            }
            return Ok((
                Self {
                    mantissa: SignedInt256::ZERO,
                    scale: target_scale,
                },
                SizeErrorStatus::Exact,
            ));
        }

        // Check whether exponent is an exact integer in -64..=64.
        if let Some(int_exp) = exponent.to_exact_i32()
            && (-64..=64).contains(&int_exp)
        {
            if int_exp == 0 {
                return Self::from_i64(1).rescale(target_scale, mode);
            }
            let abs_exp = int_exp.unsigned_abs();
            let mut acc = Self::from_i64(1);
            let mut base = *self;
            let mut n = abs_exp;
            while n > 0 {
                if (n & 1) == 1 {
                    let (next_acc, st) = acc.checked_mul(&base)?;
                    if st.is_error() {
                        return Ok((*self, SizeErrorStatus::Overflowed));
                    }
                    acc = next_acc;
                }
                n >>= 1;
                if n > 0 {
                    let (next_base, st) = base.checked_mul(&base)?;
                    if st.is_error() {
                        return Ok((*self, SizeErrorStatus::Overflowed));
                    }
                    base = next_base;
                }
            }
            if int_exp < 0 {
                return Self::from_i64(1).checked_div(&acc, target_scale, mode);
            }
            return acc.rescale(target_scale, mode);
        }

        // Fractional or large exponent: evaluate via IEEE 754 `f64::powf`.
        let base_f = self.to_f64();
        let exp_f = exponent.to_f64();
        if base_f < 0.0 {
            return Ok((Self::ZERO, SizeErrorStatus::InvalidExponent));
        }
        let result_f = base_f.powf(exp_f);
        if !result_f.is_finite() {
            return Ok((Self::ZERO, SizeErrorStatus::Overflowed));
        }
        Self::from_f64(result_f, target_scale, mode)
    }

    /// Converts an IEEE 754 `f64` into a [`FixedDecimal`] at `target_scale` using `mode`.
    ///
    /// # Arguments
    ///
    /// * `val` - Finite IEEE 754 `f64` value.
    /// * `target_scale` - Result fractional digit scale (`-76..=76`).
    /// * `mode` - Rounding mode.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `!val.is_finite()` or `target_scale` is out of bounds.
    pub fn from_f64(
        val: f64,
        target_scale: i16,
        mode: RoundingMode,
    ) -> Result<(Self, SizeErrorStatus)> {
        if !val.is_finite() {
            return Err(CoblangError::DecimalDomain {
                operation: "FixedDecimal::from_f64",
                message: format!("non-finite floating-point value `{val}`"),
            });
        }
        let formatted = format!("{val:.17e}");
        let (mant_part, exp_part) = formatted.split_once('e').unwrap_or((&formatted, "0"));
        let parsed_mant = Self::parse_str(mant_part)?;
        let exp10 = exp_part.parse::<i16>().unwrap_or(0);
        let combined_scale = parsed_mant.scale.saturating_sub(exp10);
        let clamped_scale =
            combined_scale.clamp(-Self::MAX_INTERMEDIATE_SCALE, Self::MAX_INTERMEDIATE_SCALE);
        let exact = Self {
            mantissa: parsed_mant.mantissa,
            scale: clamped_scale,
        };
        exact.rescale(target_scale, mode)
    }

    /// Approximates `self` as an IEEE 754 `f64`.
    #[must_use]
    pub fn to_f64(&self) -> f64 {
        if self.is_zero() {
            return 0.0;
        }
        let text = self.to_string();
        text.parse::<f64>().unwrap_or(0.0)
    }

    /// Converts `self` to an exact `i32` if its fractional part is zero and value fits in `i32`.
    #[must_use]
    pub fn to_exact_i32(&self) -> Option<i32> {
        let (unscaled, status) = self.rescale(0, RoundingMode::Prohibited).ok()?;
        if status.is_error() {
            return None;
        }
        let v128 = unscaled.mantissa.to_i128()?;
        i32::try_from(v128).ok()
    }

    /// Compares `self` and `other` algebraically across arbitrary scales (`0.5 == 0.50`, `-1 < 0`).
    ///
    /// # Arguments
    ///
    /// * `other` - Right-hand decimal operand.
    #[must_use]
    pub fn cmp_numeric(&self, other: &Self) -> Ordering {
        if self.is_zero() && other.is_zero() {
            return Ordering::Equal;
        }
        match (self.mantissa.sign(), other.mantissa.sign()) {
            (DecimalSign::Negative, DecimalSign::UnsignedZero | DecimalSign::Positive) => {
                return Ordering::Less;
            }
            (DecimalSign::UnsignedZero, DecimalSign::Positive) => return Ordering::Less,
            (DecimalSign::UnsignedZero | DecimalSign::Positive, DecimalSign::Negative) => {
                return Ordering::Greater;
            }
            (DecimalSign::Positive, DecimalSign::UnsignedZero) => return Ordering::Greater,
            _ => {}
        }

        if self.scale == other.scale {
            return self.mantissa.cmp(&other.mantissa);
        }
        let (high_scale, low_scale, swapped) = if self.scale > other.scale {
            (self, other, false)
        } else {
            (other, self, true)
        };
        let diff = high_scale.scale - low_scale.scale;
        let exp = u8::try_from(diff).unwrap_or(u8::MAX);
        let ord = if let Some(scaled_low) = low_scale.mantissa.checked_mul_pow10(exp) {
            high_scale.mantissa.cmp(&scaled_low)
        } else {
            // Divide `high_scale` down to `low_scale.scale` and inspect remainder!
            let (trunc_high, _) = high_scale
                .rescale(low_scale.scale, RoundingMode::TowardLesser)
                .unwrap_or((*high_scale, SizeErrorStatus::Exact));
            let base_cmp = trunc_high.mantissa.cmp(&low_scale.mantissa);
            if base_cmp == Ordering::Equal {
                Ordering::Greater
            } else {
                base_cmp
            }
        };
        if swapped { ord.reverse() } else { ord }
    }
}

impl fmt::Display for FixedDecimal {
    /// Formats `self` into decimal notation without scientific exponent.
    ///
    /// # Arguments
    ///
    /// * `f` - Destination formatter.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_zero() {
            if self.scale > 0 {
                f.write_str("0.")?;
                for _ in 0..self.scale {
                    f.write_str("0")?;
                }
                return Ok(());
            }
            return f.write_str("0");
        }
        if self.is_negative() {
            f.write_str("-")?;
        }
        let digits = self.mantissa.magnitude().to_decimal_digits();
        if self.scale <= 0 {
            let s = core::str::from_utf8(&digits).unwrap_or("0");
            f.write_str(s)?;
            for _ in 0..(-self.scale) {
                f.write_str("0")?;
            }
            return Ok(());
        }
        let frac = usize::try_from(self.scale).unwrap_or(0);
        if digits.len() <= frac {
            f.write_str("0.")?;
            for _ in 0..(frac - digits.len()) {
                f.write_str("0")?;
            }
            let s = core::str::from_utf8(&digits).unwrap_or("0");
            f.write_str(s)
        } else {
            let split = digits.len() - frac;
            let int_part = digits
                .get(..split)
                .and_then(|b| core::str::from_utf8(b).ok())
                .unwrap_or("0");
            let frac_part = digits
                .get(split..)
                .and_then(|b| core::str::from_utf8(b).ok())
                .unwrap_or("0");
            f.write_str(int_part)?;
            f.write_str(".")?;
            f.write_str(frac_part)
        }
    }
}

/// Evaluates `round_quotient_with_remainder` when the scale shift exceeds 76 digits (`quotient == 0`, `0 < rem < div / 2`).
///
/// # Arguments
///
/// * `nonzero_mant` - Non-zero signed mantissa being scaled down by $> 76$ digits.
/// * `mode` - Active [`RoundingMode`].
#[must_use]
fn round_beyond_76(
    nonzero_mant: SignedInt256,
    mode: RoundingMode,
) -> (SignedInt256, SizeErrorStatus) {
    if nonzero_mant.is_zero() {
        return (SignedInt256::ZERO, SizeErrorStatus::Exact);
    }
    round_quotient_with_remainder(
        Uint256::ZERO,
        Uint256::ONE,
        Uint256::from_u64(10),
        nonzero_mant.sign(),
        mode,
    )
}
