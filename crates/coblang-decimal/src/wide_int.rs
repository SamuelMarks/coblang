//! Zero-allocation 256-bit unsigned (`Uint256`) and signed (`SignedInt256`) integer arithmetic.

use core::cmp::Ordering;
use core::fmt::{self, Write as _};

use coblang_common::{CoblangError, Result};

/// Canonical sign state of a [`SignedInt256`] or [`crate::FixedDecimal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecimalSign {
    /// Strictly positive (`> 0`).
    Positive,
    /// Strictly negative (`< 0`).
    Negative,
    /// Exact zero (`0`), preventing `-0` ambiguity in algebraic comparisons.
    UnsignedZero,
}

impl DecimalSign {
    /// Returns `true` when this sign is [`Self::Negative`].
    #[must_use]
    pub const fn is_negative(self) -> bool {
        matches!(self, Self::Negative)
    }

    /// Returns `true` when this sign is [`Self::Positive`].
    #[must_use]
    pub const fn is_positive(self) -> bool {
        matches!(self, Self::Positive)
    }

    /// Returns `true` when this sign is [`Self::UnsignedZero`].
    #[must_use]
    pub const fn is_zero(self) -> bool {
        matches!(self, Self::UnsignedZero)
    }

    /// Flips [`Self::Positive`] <-> [`Self::Negative`], preserving [`Self::UnsignedZero`].
    #[must_use]
    pub const fn negate(self) -> Self {
        match self {
            Self::Positive => Self::Negative,
            Self::Negative => Self::Positive,
            Self::UnsignedZero => Self::UnsignedZero,
        }
    }

    /// Computes the product sign of `self * rhs`.
    ///
    /// # Arguments
    ///
    /// * `rhs` - Right-hand operand sign.
    #[must_use]
    pub const fn mul_sign(self, rhs: Self) -> Self {
        if self.is_zero() || rhs.is_zero() {
            Self::UnsignedZero
        } else if self.is_negative() == rhs.is_negative() {
            Self::Positive
        } else {
            Self::Negative
        }
    }
}

/// Fixed-width 256-bit unsigned integer stored as four little-endian `u64` limbs (`[l0, l1, l2, l3]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Uint256([u64; 4]);

impl Uint256 {
    /// Additive identity (`0`).
    pub const ZERO: Self = Self([0, 0, 0, 0]);

    /// Multiplicative identity (`1`).
    pub const ONE: Self = Self([1, 0, 0, 0]);

    /// Maximum value (`2^256 - 1`).
    pub const MAX: Self = Self([u64::MAX, u64::MAX, u64::MAX, u64::MAX]);

    /// Maximum supported decimal power-of-10 exponent (`10^76 < 2^256`).
    pub const MAX_DECIMAL_DIGITS: u8 = 76;

    /// Largest power of 10 fitting in a single `u64` (`10^19`).
    const POW10_U64_CHUNK: u64 = 10_000_000_000_000_000_000;

    /// Constructs a [`Uint256`] from four little-endian `u64` limbs `[l0, l1, l2, l3]`.
    ///
    /// # Arguments
    ///
    /// * `limbs` - Little-endian 64-bit limb array.
    #[must_use]
    pub const fn from_limbs(limbs: [u64; 4]) -> Self {
        Self(limbs)
    }

    /// Returns the four little-endian `u64` limbs `[l0, l1, l2, l3]`.
    #[must_use]
    pub const fn limbs(self) -> [u64; 4] {
        self.0
    }

    /// Widens a `u64` into [`Uint256`].
    ///
    /// # Arguments
    ///
    /// * `val` - Unsigned 64-bit integer.
    #[must_use]
    pub const fn from_u64(val: u64) -> Self {
        Self([val, 0, 0, 0])
    }

    /// Widens a `u128` into [`Uint256`].
    ///
    /// # Arguments
    ///
    /// * `val` - Unsigned 128-bit integer.
    #[must_use]
    pub const fn from_u128(val: u128) -> Self {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Low 64-bit half of u128 fits in u64 by bitmask."
        )]
        let lo = val as u64;
        let hi = (val >> 64) as u64;
        Self([lo, hi, 0, 0])
    }

    /// Narrows `self` into `u128` if both upper limbs `l2` and `l3` are zero.
    #[must_use]
    pub const fn to_u128(self) -> Option<u128> {
        let [l0, l1, l2, l3] = self.0;
        if l2 == 0 && l3 == 0 {
            Some(((l1 as u128) << 64) | (l0 as u128))
        } else {
            None
        }
    }

    /// Returns `true` if all four limbs equal `0`.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        let [l0, l1, l2, l3] = self.0;
        (l0 | l1 | l2 | l3) == 0
    }

    /// Returns `true` if the least-significant bit is `1`.
    #[must_use]
    pub const fn is_odd(self) -> bool {
        let [l0, _, _, _] = self.0;
        (l0 & 1) == 1
    }

    /// Checked 256-bit unsigned addition returning `None` on carry out of bit 255.
    ///
    /// # Arguments
    ///
    /// * `rhs` - Addend.
    #[must_use]
    pub const fn checked_add(self, rhs: Self) -> Option<Self> {
        let [a0, a1, a2, a3] = self.0;
        let [b0, b1, b2, b3] = rhs.0;

        let s0 = (a0 as u128) + (b0 as u128);
        let (r0, c0) = split_u128(s0);
        let s1 = (a1 as u128) + (b1 as u128) + (c0 as u128);
        let (r1, c1) = split_u128(s1);
        let s2 = (a2 as u128) + (b2 as u128) + (c1 as u128);
        let (r2, c2) = split_u128(s2);
        let s3 = (a3 as u128) + (b3 as u128) + (c2 as u128);
        let (r3, c3) = split_u128(s3);

        if c3 == 0 {
            Some(Self([r0, r1, r2, r3]))
        } else {
            None
        }
    }

    /// Checked 256-bit unsigned subtraction returning `None` when `self < rhs`.
    ///
    /// # Arguments
    ///
    /// * `rhs` - Subtrahend.
    #[must_use]
    pub const fn checked_sub(self, rhs: Self) -> Option<Self> {
        let [a0, a1, a2, a3] = self.0;
        let [b0, b1, b2, b3] = rhs.0;

        let (r0, borrow0) = a0.overflowing_sub(b0);
        let (t1, sub_udf1) = a1.overflowing_sub(b1);
        let (r1, carry_udf1) = t1.overflowing_sub(u64_from_bool(borrow0));
        let (t2, sub_udf2) = a2.overflowing_sub(b2);
        let (r2, carry_udf2) = t2.overflowing_sub(u64_from_bool(sub_udf1 || carry_udf1));
        let (t3, sub_udf3) = a3.overflowing_sub(b3);
        let (r3, carry_udf3) = t3.overflowing_sub(u64_from_bool(sub_udf2 || carry_udf2));

        if sub_udf3 || carry_udf3 {
            None
        } else {
            Some(Self([r0, r1, r2, r3]))
        }
    }

    /// Checked multiplication by a scalar `u64` limb returning `None` on overflow.
    ///
    /// # Arguments
    ///
    /// * `rhs` - Scalar 64-bit multiplier.
    #[must_use]
    pub const fn checked_mul_u64(self, rhs: u64) -> Option<Self> {
        let [a0, a1, a2, a3] = self.0;
        let m = rhs as u128;

        let (r0, c0) = split_u128((a0 as u128) * m);
        let (r1, c1) = split_u128((a1 as u128) * m + (c0 as u128));
        let (r2, c2) = split_u128((a2 as u128) * m + (c1 as u128));
        let (r3, c3) = split_u128((a3 as u128) * m + (c2 as u128));

        if c3 == 0 {
            Some(Self([r0, r1, r2, r3]))
        } else {
            None
        }
    }

    /// Shifts limbs upward by `limb_shift` 64-bit positions, returning `None` if non-zero bits are lost.
    ///
    /// # Arguments
    ///
    /// * `limb_shift` - Number of 64-bit limbs to shift left (`0..=3`).
    #[must_use]
    const fn checked_shl_limbs(self, limb_shift: usize) -> Option<Self> {
        let [l0, l1, l2, l3] = self.0;
        match limb_shift {
            0 => Some(self),
            1 if l3 == 0 => Some(Self([0, l0, l1, l2])),
            2 if l2 == 0 && l3 == 0 => Some(Self([0, 0, l0, l1])),
            3 if l1 == 0 && l2 == 0 && l3 == 0 => Some(Self([0, 0, 0, l0])),
            _ => None,
        }
    }

    /// Checked full 256x256->256-bit unsigned multiplication returning `None` on overflow.
    ///
    /// # Arguments
    ///
    /// * `rhs` - Multiplier.
    #[must_use]
    pub fn checked_mul(self, rhs: Self) -> Option<Self> {
        if self.is_zero() || rhs.is_zero() {
            return Some(Self::ZERO);
        }
        let mut acc = Self::ZERO;
        for (shift, limb) in rhs.0.into_iter().enumerate() {
            if limb != 0 {
                let prod = self.checked_mul_u64(limb)?;
                let shifted = prod.checked_shl_limbs(shift)?;
                acc = acc.checked_add(shifted)?;
            }
        }
        Some(acc)
    }

    /// Divides `self` by a scalar `u64` divisor, returning `(quotient, remainder)` or `None` if `divisor == 0`.
    ///
    /// # Arguments
    ///
    /// * `divisor` - Non-zero 64-bit unsigned divisor.
    #[must_use]
    pub const fn div_rem_u64(self, divisor: u64) -> Option<(Self, u64)> {
        if divisor == 0 {
            return None;
        }
        let [l0, l1, l2, l3] = self.0;
        let d = divisor as u128;

        let q3 = (l3 as u128) / d;
        let rem3 = (l3 as u128) % d;
        let n2 = (rem3 << 64) | (l2 as u128);
        let q2 = n2 / d;
        let rem2 = n2 % d;
        let n1 = (rem2 << 64) | (l1 as u128);
        let q1 = n1 / d;
        let rem1 = n1 % d;
        let n0 = (rem1 << 64) | (l0 as u128);
        let q0 = n0 / d;
        let rem0 = n0 % d;

        #[expect(
            clippy::cast_possible_truncation,
            reason = "Each limb quotient and remainder mod u64 divisor fits in u64."
        )]
        Some((
            Self([q0 as u64, q1 as u64, q2 as u64, q3 as u64]),
            rem0 as u64,
        ))
    }

    /// Computes exact 256-bit `(quotient, remainder)` for `self / divisor`.
    ///
    /// # Arguments
    ///
    /// * `divisor` - Unsigned 256-bit divisor.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] when `divisor.is_zero()`.
    pub fn checked_div_rem(self, divisor: Self) -> Result<(Self, Self)> {
        if divisor.is_zero() {
            return Err(CoblangError::DecimalDomain {
                operation: "div_rem",
                message: String::from("division by zero"),
            });
        }
        if self < divisor {
            return Ok((Self::ZERO, self));
        }

        let top_bit = self.highest_bit_index();
        let mut quot = Self::ZERO;
        let mut rem = Self::ZERO;
        let mut bit = top_bit;
        loop {
            rem = rem.shl1_with_lsb(self.test_bit(bit));
            if rem >= divisor {
                rem = rem.checked_sub(divisor).unwrap_or(Self::ZERO);
                quot = quot.with_bit_set(bit);
            }
            if bit == 0 {
                break;
            }
            bit -= 1;
        }
        Ok((quot, rem))
    }

    /// Compares `2 * self` against `other` without overflowing bit 255.
    ///
    /// # Arguments
    ///
    /// * `other` - Divisor magnitude to compare `2 * self` against.
    #[must_use]
    pub fn cmp_double_to(self, other: Self) -> Ordering {
        if let Some(doubled) = self.checked_add(self) {
            doubled.cmp(&other)
        } else {
            Ordering::Greater
        }
    }

    /// Computes exact `10^exp` for `0 <= exp <= 76`.
    ///
    /// # Arguments
    ///
    /// * `exp` - Decimal exponent in `0..=76`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `exp > 76`.
    pub fn pow10(exp: u8) -> Result<Self> {
        if exp > Self::MAX_DECIMAL_DIGITS {
            return Err(CoblangError::DecimalDomain {
                operation: "pow10",
                message: format!("power-of-10 exponent {exp} exceeds 76-digit limit"),
            });
        }
        let chunks = exp / 19;
        let rem = u32::from(exp % 19);
        let mut acc = Self::from_u64(10_u64.pow(rem));
        for _ in 0..chunks {
            acc = acc
                .checked_mul_u64(Self::POW10_U64_CHUNK)
                .unwrap_or(Self::MAX);
        }
        Ok(acc)
    }

    /// Parses an ASCII digit slice (`'0'..='9'`) of up to 76 significant digits into [`Uint256`].
    ///
    /// # Arguments
    ///
    /// * `digits` - Non-empty ASCII decimal digit byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `digits` is empty, contains non-digits, or exceeds 76 significant digits.
    pub fn from_decimal_digits(digits: &[u8]) -> Result<Self> {
        if digits.is_empty() {
            return Err(CoblangError::DecimalDomain {
                operation: "from_decimal_digits",
                message: String::from("empty decimal digit string"),
            });
        }
        let mut sig_len = 0_usize;
        let mut acc = Self::ZERO;
        for &byte in digits {
            if !byte.is_ascii_digit() {
                return Err(CoblangError::DecimalDomain {
                    operation: "from_decimal_digits",
                    message: format!("invalid decimal digit byte 0x{byte:02X}"),
                });
            }
            let d = u64::from(byte - b'0');
            if !acc.is_zero() || d != 0 {
                sig_len += 1;
                if sig_len > usize::from(Self::MAX_DECIMAL_DIGITS) {
                    return Err(CoblangError::DecimalDomain {
                        operation: "from_decimal_digits",
                        message: format!(
                            "significant digit count exceeds {}",
                            Self::MAX_DECIMAL_DIGITS
                        ),
                    });
                }
            }
            acc = acc
                .checked_mul_u64(10)
                .and_then(|v| v.checked_add(Self::from_u64(d)))
                .unwrap_or(Self::MAX);
        }
        Ok(acc)
    }

    /// Formats `self` into canonical base-10 ASCII digit bytes (`"0"` for zero, no leading zeros otherwise).
    #[must_use]
    pub fn to_decimal_digits(self) -> Vec<u8> {
        if self.is_zero() {
            return vec![b'0'];
        }
        let mut rev_chunks: Vec<u64> = Vec::with_capacity(5);
        let mut cur = self;
        while !cur.is_zero() {
            let (next_q, rem) = cur
                .div_rem_u64(Self::POW10_U64_CHUNK)
                .unwrap_or((Self::ZERO, 0));
            rev_chunks.push(rem);
            cur = next_q;
        }
        let mut out = String::with_capacity(rev_chunks.len() * 19);
        for (idx, &chunk) in rev_chunks.iter().rev().enumerate() {
            if idx == 0 {
                let _ = write!(out, "{chunk}");
            } else {
                let _ = write!(out, "{chunk:019}");
            }
        }
        out.into_bytes()
    }

    /// Returns the zero-based index (`0..=255`) of the highest set bit of a non-zero `Uint256`.
    #[must_use]
    const fn highest_bit_index(self) -> u8 {
        let [l0, l1, l2, l3] = self.0;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "ilog2() on non-zero u64 is in 0..=63, so + 192 <= 255 fits in u8."
        )]
        if l3 != 0 {
            192 + l3.ilog2() as u8
        } else if l2 != 0 {
            128 + l2.ilog2() as u8
        } else if l1 != 0 {
            64 + l1.ilog2() as u8
        } else {
            l0.ilog2() as u8
        }
    }

    /// Tests whether bit `bit_idx` (`0..=255`) is `1`.
    ///
    /// # Arguments
    ///
    /// * `bit_idx` - Bit position (`0..=255`).
    #[must_use]
    const fn test_bit(self, bit_idx: u8) -> bool {
        let [l0, l1, l2, l3] = self.0;
        let shift = bit_idx & 63;
        let limb = match bit_idx >> 6 {
            0 => l0,
            1 => l1,
            2 => l2,
            _ => l3,
        };
        ((limb >> shift) & 1) == 1
    }

    /// Returns `self` with bit `bit_idx` (`0..=255`) set to `1`.
    ///
    /// # Arguments
    ///
    /// * `bit_idx` - Bit position (`0..=255`).
    #[must_use]
    const fn with_bit_set(self, bit_idx: u8) -> Self {
        let [l0, l1, l2, l3] = self.0;
        let mask = 1_u64 << (bit_idx & 63);
        match bit_idx >> 6 {
            0 => Self([l0 | mask, l1, l2, l3]),
            1 => Self([l0, l1 | mask, l2, l3]),
            2 => Self([l0, l1, l2 | mask, l3]),
            _ => Self([l0, l1, l2, l3 | mask]),
        }
    }

    /// Shifts `self` left by 1 bit and inserts `lsb` into bit 0.
    ///
    /// # Arguments
    ///
    /// * `lsb` - Bit inserted at bit 0.
    #[must_use]
    const fn shl1_with_lsb(self, lsb: bool) -> Self {
        let [l0, l1, l2, l3] = self.0;
        let r3 = (l3 << 1) | (l2 >> 63);
        let r2 = (l2 << 1) | (l1 >> 63);
        let r1 = (l1 << 1) | (l0 >> 63);
        let r0 = (l0 << 1) | u64_from_bool(lsb);
        Self([r0, r1, r2, r3])
    }
}

impl Ord for Uint256 {
    /// Compares `self` and `other` from most-significant limb `l3` down to `l0`.
    ///
    /// # Arguments
    ///
    /// * `other` - Right-hand unsigned 256-bit integer.
    fn cmp(&self, other: &Self) -> Ordering {
        let [a0, a1, a2, a3] = self.0;
        let [b0, b1, b2, b3] = other.0;
        a3.cmp(&b3)
            .then_with(|| a2.cmp(&b2))
            .then_with(|| a1.cmp(&b1))
            .then_with(|| a0.cmp(&b0))
    }
}

impl PartialOrd for Uint256 {
    /// Delegates partial comparison to [`Ord::cmp`].
    ///
    /// # Arguments
    ///
    /// * `other` - Right-hand unsigned 256-bit integer.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Uint256 {
    /// Formats `self` in canonical base-10 decimal notation.
    ///
    /// # Arguments
    ///
    /// * `f` - Destination formatter.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.to_decimal_digits();
        let s = core::str::from_utf8(&bytes).unwrap_or("0");
        f.write_str(s)
    }
}

/// Exact signed 256-bit magnitude + [`DecimalSign`] supporting 76-digit COBOL intermediate values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SignedInt256 {
    /// Unsigned 256-bit magnitude.
    magnitude: Uint256,
    /// Normalized sign (`UnsignedZero` iff `magnitude.is_zero()`).
    sign: DecimalSign,
}

impl SignedInt256 {
    /// Zero (`0`).
    pub const ZERO: Self = Self {
        magnitude: Uint256::ZERO,
        sign: DecimalSign::UnsignedZero,
    };

    /// Positive one (`+1`).
    pub const ONE: Self = Self {
        magnitude: Uint256::ONE,
        sign: DecimalSign::Positive,
    };

    /// Negative one (`-1`).
    pub const MINUS_ONE: Self = Self {
        magnitude: Uint256::ONE,
        sign: DecimalSign::Negative,
    };

    /// Constructs a normalized [`SignedInt256`] from `magnitude` and `sign` (`0` forces `UnsignedZero`).
    ///
    /// # Arguments
    ///
    /// * `magnitude` - Unsigned 256-bit magnitude.
    /// * `sign` - Requested sign when `magnitude` is non-zero.
    #[must_use]
    pub const fn from_parts(magnitude: Uint256, sign: DecimalSign) -> Self {
        if magnitude.is_zero() || matches!(sign, DecimalSign::UnsignedZero) {
            Self::ZERO
        } else {
            Self { magnitude, sign }
        }
    }

    /// Widens a signed `i64` into [`SignedInt256`].
    ///
    /// # Arguments
    ///
    /// * `val` - Signed 64-bit integer.
    #[must_use]
    pub const fn from_i64(val: i64) -> Self {
        if val == 0 {
            Self::ZERO
        } else if val < 0 {
            Self {
                magnitude: Uint256::from_u64(val.unsigned_abs()),
                sign: DecimalSign::Negative,
            }
        } else {
            Self {
                magnitude: Uint256::from_u64(val.unsigned_abs()),
                sign: DecimalSign::Positive,
            }
        }
    }

    /// Widens a signed `i128` into [`SignedInt256`].
    ///
    /// # Arguments
    ///
    /// * `val` - Signed 128-bit integer.
    #[must_use]
    pub const fn from_i128(val: i128) -> Self {
        if val == 0 {
            Self::ZERO
        } else if val < 0 {
            Self {
                magnitude: Uint256::from_u128(val.unsigned_abs()),
                sign: DecimalSign::Negative,
            }
        } else {
            Self {
                magnitude: Uint256::from_u128(val.unsigned_abs()),
                sign: DecimalSign::Positive,
            }
        }
    }

    /// Widens an unsigned `u128` into [`SignedInt256`].
    ///
    /// # Arguments
    ///
    /// * `val` - Unsigned 128-bit integer.
    #[must_use]
    pub const fn from_u128(val: u128) -> Self {
        if val == 0 {
            Self::ZERO
        } else {
            Self {
                magnitude: Uint256::from_u128(val),
                sign: DecimalSign::Positive,
            }
        }
    }

    /// Converts `self` to `i128` if it fits in `i128::MIN..=i128::MAX`.
    #[must_use]
    pub const fn to_i128(self) -> Option<i128> {
        let Some(mag128) = self.magnitude.to_u128() else {
            return None;
        };
        match self.sign {
            DecimalSign::UnsignedZero => Some(0),
            DecimalSign::Positive => {
                if mag128 <= (i128::MAX as u128) {
                    #[expect(
                        clippy::cast_possible_wrap,
                        reason = "Guarded by mag128 <= i128::MAX as u128."
                    )]
                    Some(mag128 as i128)
                } else {
                    None
                }
            }
            DecimalSign::Negative => {
                let min_abs = (i128::MAX as u128) + 1;
                if mag128 < min_abs {
                    #[expect(
                        clippy::cast_possible_wrap,
                        reason = "Guarded by mag128 <= i128::MAX as u128."
                    )]
                    Some(-(mag128 as i128))
                } else if mag128 == min_abs {
                    Some(i128::MIN)
                } else {
                    None
                }
            }
        }
    }

    /// Converts `self` to `u128` if `self >= 0` and fits in `u128`.
    #[must_use]
    pub const fn to_u128(self) -> Option<u128> {
        if self.sign.is_negative() {
            None
        } else {
            self.magnitude.to_u128()
        }
    }

    /// Returns the unsigned 256-bit magnitude.
    #[must_use]
    pub const fn magnitude(self) -> Uint256 {
        self.magnitude
    }

    /// Returns the normalized [`DecimalSign`].
    #[must_use]
    pub const fn sign(self) -> DecimalSign {
        self.sign
    }

    /// Returns `true` when `self == 0`.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.sign.is_zero()
    }

    /// Returns `true` when `self < 0`.
    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.sign.is_negative()
    }

    /// Returns `|self|` as a non-negative [`SignedInt256`].
    #[must_use]
    pub const fn abs(self) -> Self {
        if self.is_zero() {
            Self::ZERO
        } else {
            Self {
                magnitude: self.magnitude,
                sign: DecimalSign::Positive,
            }
        }
    }

    /// Returns `-self`.
    #[must_use]
    pub const fn negate(self) -> Self {
        Self {
            magnitude: self.magnitude,
            sign: self.sign.negate(),
        }
    }

    /// Checked signed 256-bit addition returning `None` on magnitude overflow.
    ///
    /// # Arguments
    ///
    /// * `rhs` - Signed addend.
    #[must_use]
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        if self.is_zero() {
            return Some(rhs);
        }
        if rhs.is_zero() {
            return Some(self);
        }
        if self.sign == rhs.sign {
            let sum = self.magnitude.checked_add(rhs.magnitude)?;
            Some(Self::from_parts(sum, self.sign))
        } else {
            match self.magnitude.cmp(&rhs.magnitude) {
                Ordering::Equal => Some(Self::ZERO),
                Ordering::Greater => {
                    let diff = self.magnitude.checked_sub(rhs.magnitude)?;
                    Some(Self::from_parts(diff, self.sign))
                }
                Ordering::Less => {
                    let diff = rhs.magnitude.checked_sub(self.magnitude)?;
                    Some(Self::from_parts(diff, rhs.sign))
                }
            }
        }
    }

    /// Checked signed 256-bit subtraction (`self - rhs`) returning `None` on magnitude overflow.
    ///
    /// # Arguments
    ///
    /// * `rhs` - Signed subtrahend.
    #[must_use]
    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.checked_add(rhs.negate())
    }

    /// Checked signed 256-bit multiplication returning `None` on magnitude overflow.
    ///
    /// # Arguments
    ///
    /// * `rhs` - Signed multiplier.
    #[must_use]
    pub fn checked_mul(self, rhs: Self) -> Option<Self> {
        let sign = self.sign.mul_sign(rhs.sign);
        if sign.is_zero() {
            return Some(Self::ZERO);
        }
        let prod = self.magnitude.checked_mul(rhs.magnitude)?;
        Some(Self::from_parts(prod, sign))
    }

    /// Checked multiplication by `10^exp` (`0 <= exp <= 76`) returning `None` on overflow.
    ///
    /// # Arguments
    ///
    /// * `exp` - Non-negative decimal scale shift (`0..=76`).
    #[must_use]
    pub fn checked_mul_pow10(self, exp: u8) -> Option<Self> {
        if self.is_zero() || exp == 0 {
            return Some(self);
        }
        let factor = Uint256::pow10(exp).ok()?;
        let prod = self.magnitude.checked_mul(factor)?;
        Some(Self::from_parts(prod, self.sign))
    }

    /// Computes truncating signed `(quotient, remainder)` satisfying `self == q * divisor + r`.
    ///
    /// # Arguments
    ///
    /// * `divisor` - Signed 256-bit divisor.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] when `divisor.is_zero()`.
    pub fn checked_div_rem(self, divisor: Self) -> Result<(Self, Self)> {
        let (q_mag, r_mag) = self.magnitude.checked_div_rem(divisor.magnitude)?;
        let q_sign = self.sign.mul_sign(divisor.sign);
        let r_sign = self.sign;
        Ok((
            Self::from_parts(q_mag, q_sign),
            Self::from_parts(r_mag, r_sign),
        ))
    }
}

impl Ord for SignedInt256 {
    /// Compares `self` and `other` algebraically across negative, zero, and positive values.
    ///
    /// # Arguments
    ///
    /// * `other` - Right-hand signed 256-bit integer.
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.sign, other.sign) {
            (DecimalSign::UnsignedZero, DecimalSign::UnsignedZero) => Ordering::Equal,
            (DecimalSign::Negative, DecimalSign::UnsignedZero | DecimalSign::Positive) => {
                Ordering::Less
            }
            (DecimalSign::UnsignedZero, DecimalSign::Positive) => Ordering::Less,
            (DecimalSign::UnsignedZero | DecimalSign::Positive, DecimalSign::Negative) => {
                Ordering::Greater
            }
            (DecimalSign::Positive, DecimalSign::UnsignedZero) => Ordering::Greater,
            (DecimalSign::Positive, DecimalSign::Positive) => self.magnitude.cmp(&other.magnitude),
            (DecimalSign::Negative, DecimalSign::Negative) => other.magnitude.cmp(&self.magnitude),
        }
    }
}

impl PartialOrd for SignedInt256 {
    /// Delegates partial comparison to [`Ord::cmp`].
    ///
    /// # Arguments
    ///
    /// * `other` - Right-hand signed 256-bit integer.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for SignedInt256 {
    /// Formats `self` with optional leading `'-'` and base-10 digits.
    ///
    /// # Arguments
    ///
    /// * `f` - Destination formatter.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_negative() {
            f.write_str("-")?;
        }
        fmt::Display::fmt(&self.magnitude, f)
    }
}

/// Splits a `u128` into `(low_u64, high_u64)`.
///
/// # Arguments
///
/// * `val` - Unsigned 128-bit value.
#[must_use]
const fn split_u128(val: u128) -> (u64, u64) {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Low 64 bits and >> 64 high 64 bits fit in u64."
    )]
    (val as u64, (val >> 64) as u64)
}

/// Converts a `bool` to `0_u64` or `1_u64` inside `const fn`.
///
/// # Arguments
///
/// * `flag` - Boolean bit.
#[must_use]
const fn u64_from_bool(flag: bool) -> u64 {
    if flag { 1 } else { 0 }
}
