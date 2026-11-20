//! All eight ISO/IEC 1989:2023 COBOL rounding modes and `SIZE ERROR` status outcomes.

use core::cmp::Ordering;

use crate::wide_int::{DecimalSign, SignedInt256, Uint256};

/// All eight ISO/IEC 1989:2023 COBOL rounding modes for fixed-point quantization and division.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoundingMode {
    /// Truncate toward zero (default when `ROUNDED` is omitted).
    Truncation,
    /// Round to nearest; ties round away from zero (default `ROUNDED` without `MODE`).
    NearestAwayFromZero,
    /// Round to nearest; ties round to even least-significant digit (`NEAREST-EVEN`).
    NearestEven,
    /// Round to nearest; ties round toward zero (`NEAREST-TOWARD-ZERO`).
    NearestTowardZero,
    /// Prohibit non-zero discarded digits, signaling [`SizeErrorStatus::Overflowed`] (`PROHIBITED`).
    Prohibited,
    /// Directed rounding toward $+\infty$ (`TOWARD-GREATER`).
    TowardGreater,
    /// Directed rounding toward $-\infty$ (`TOWARD-LESSER`).
    TowardLesser,
    /// Directed rounding away from zero on any non-zero remainder (`AWAY-FROM-ZERO`).
    AwayFromZero,
}

/// Arithmetic or target-fit status used to drive `ON SIZE ERROR` / `NOT ON SIZE ERROR` and `EC-SIZE-*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeErrorStatus {
    /// Operation completed within target bounds with no prohibited truncation.
    Exact,
    /// Target high-order overflow, 256-bit overflow, or `PROHIBITED` non-zero truncation (`EC-SIZE-OVERFLOW` / `EC-SIZE-TRUNCATION`).
    Overflowed,
    /// Division by zero (`EC-SIZE-ZERO-DIVIDE`).
    DivisionByZero,
    /// Undefined exponentiation such as `0 ** 0` or negative base with fractional exponent (`EC-SIZE-EXPONENTIATION`).
    InvalidExponent,
}

impl SizeErrorStatus {
    /// Returns `true` when any size error condition occurred (`!= Self::Exact`).
    #[must_use]
    pub const fn is_error(self) -> bool {
        !matches!(self, Self::Exact)
    }
}

/// Applies `mode` to an unsigned `(quotient_mag, remainder_mag)` pair divided by `divisor_mag` (> 0).
///
/// # Arguments
///
/// * `quotient_mag` - Truncated unsigned quotient $|n| \div |d|$.
/// * `remainder_mag` - Unsigned remainder $|n| \bmod |d|$.
/// * `divisor_mag` - Strictly positive unsigned divisor $|d|$.
/// * `true_sign` - Mathematical sign of the unrounded rational $n / d$.
/// * `mode` - ISO COBOL [`RoundingMode`] to apply.
#[must_use]
pub fn round_quotient_with_remainder(
    quotient_mag: Uint256,
    remainder_mag: Uint256,
    divisor_mag: Uint256,
    true_sign: DecimalSign,
    mode: RoundingMode,
) -> (SignedInt256, SizeErrorStatus) {
    if remainder_mag.is_zero() {
        return (
            SignedInt256::from_parts(quotient_mag, true_sign),
            SizeErrorStatus::Exact,
        );
    }

    if mode == RoundingMode::Prohibited {
        return (
            SignedInt256::from_parts(quotient_mag, true_sign),
            SizeErrorStatus::Overflowed,
        );
    }

    let cmp_half = remainder_mag.cmp_double_to(divisor_mag);
    let increment = match mode {
        RoundingMode::Truncation | RoundingMode::Prohibited => false,
        RoundingMode::AwayFromZero => true,
        RoundingMode::TowardGreater => !true_sign.is_negative(),
        RoundingMode::TowardLesser => true_sign.is_negative(),
        RoundingMode::NearestAwayFromZero => {
            matches!(cmp_half, Ordering::Greater | Ordering::Equal)
        }
        RoundingMode::NearestTowardZero => cmp_half == Ordering::Greater,
        RoundingMode::NearestEven => {
            cmp_half == Ordering::Greater || (cmp_half == Ordering::Equal && quotient_mag.is_odd())
        }
    };

    if !increment {
        return (
            SignedInt256::from_parts(quotient_mag, true_sign),
            SizeErrorStatus::Exact,
        );
    }

    let Some(inc_mag) = quotient_mag.checked_add(Uint256::ONE) else {
        return (
            SignedInt256::from_parts(quotient_mag, true_sign),
            SizeErrorStatus::Overflowed,
        );
    };
    (
        SignedInt256::from_parts(inc_mag, true_sign),
        SizeErrorStatus::Exact,
    )
}
