//! `COMP-1` (`f32`), `COMP-2` (`f64`), `FLOAT-EXTENDED` (`binary128`), and `FLOAT-DECIMAL-16/34` (`BID`/`DPD`) codecs.

use coblang_common::{CoblangError, Result};

use crate::fixed_decimal::FixedDecimal;
use crate::rounding::{RoundingMode, SizeErrorStatus};
use crate::wide_int::{DecimalSign, SignedInt256, Uint256};

/// Encodes a [`FixedDecimal`] into 4-byte native-endian IEEE 754 `COMP-1` (`f32`).
///
/// # Arguments
///
/// * `val` - Source [`FixedDecimal`].
#[must_use]
pub fn encode_comp1_ne(val: &FixedDecimal) -> [u8; 4] {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "COMP-1 explicitly converts f64 approximation to IEEE 754 binary32."
    )]
    let f = val.to_f64() as f32;
    f.to_ne_bytes()
}

/// Decodes 4-byte native-endian IEEE 754 `COMP-1` (`f32`) into a [`FixedDecimal`].
///
/// # Arguments
///
/// * `bytes` - 4-byte `COMP-1` storage.
/// * `target_scale` - Destination fractional scale (`-76..=76`).
/// * `mode` - Rounding mode.
///
/// # Errors
///
/// Returns [`CoblangError::DecimalDomain`] if the float is `NaN`/`Inf` or `target_scale` is invalid.
pub fn decode_comp1_ne(
    bytes: [u8; 4],
    target_scale: i16,
    mode: RoundingMode,
) -> Result<(FixedDecimal, SizeErrorStatus)> {
    let f = f64::from(f32::from_ne_bytes(bytes));
    FixedDecimal::from_f64(f, target_scale, mode)
}

/// Encodes a [`FixedDecimal`] into 8-byte native-endian IEEE 754 `COMP-2` (`f64`).
///
/// # Arguments
///
/// * `val` - Source [`FixedDecimal`].
#[must_use]
pub fn encode_comp2_ne(val: &FixedDecimal) -> [u8; 8] {
    val.to_f64().to_ne_bytes()
}

/// Decodes 8-byte native-endian IEEE 754 `COMP-2` (`f64`) into a [`FixedDecimal`].
///
/// # Arguments
///
/// * `bytes` - 8-byte `COMP-2` storage.
/// * `target_scale` - Destination fractional scale (`-76..=76`).
/// * `mode` - Rounding mode.
///
/// # Errors
///
/// Returns [`CoblangError::DecimalDomain`] if the float is `NaN`/`Inf` or `target_scale` is invalid.
pub fn decode_comp2_ne(
    bytes: [u8; 8],
    target_scale: i16,
    mode: RoundingMode,
) -> Result<(FixedDecimal, SizeErrorStatus)> {
    let f = f64::from_ne_bytes(bytes);
    FixedDecimal::from_f64(f, target_scale, mode)
}

/// Pure-Rust IEEE 754 quadruple-precision (`binary128` / `FLOAT-EXTENDED`) 16-byte representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FloatBinary128([u8; 16]);

impl FloatBinary128 {
    /// IEEE 754 binary128 exponent bias (`16383`).
    const EXP_BIAS: i32 = 16_383;

    /// Wraps raw native-endian 16-byte `binary128` storage.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Native-endian 16-byte IEEE binary128 value.
    #[must_use]
    pub const fn from_ne_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns the raw native-endian 16-byte `binary128` representation.
    #[must_use]
    pub const fn to_ne_bytes(self) -> [u8; 16] {
        self.0
    }

    /// Encodes a finite `f64` losslessly into IEEE 754 `binary128`.
    ///
    /// # Arguments
    ///
    /// * `val` - Finite IEEE 754 `f64`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `!val.is_finite()`.
    pub fn from_f64(val: f64) -> Result<Self> {
        if !val.is_finite() {
            return Err(CoblangError::DecimalDomain {
                operation: "FloatBinary128::from_f64",
                message: format!("non-finite f64 `{val}`"),
            });
        }
        let bits64 = val.to_bits();
        let sign = u128::from(bits64 >> 63);
        let exp64 = ((bits64 >> 52) & 0x7FF) as i32;
        let frac64 = u128::from(bits64 & 0x000F_FFFF_FFFF_FFFF);

        if exp64 == 0 && frac64 == 0 {
            return Ok(Self((sign << 127).to_ne_bytes()));
        }

        let (exp128, frac128) = if exp64 == 0 {
            // Normalize subnormal f64 into normal binary128!
            let lz = frac64.leading_zeros().cast_signed() - (128 - 52);
            let shifted = (frac64 << (lz + 1)) & ((1_u128 << 52) - 1);
            let unbiased = -1022 - (lz + 1);
            let biased = u128::try_from(unbiased + Self::EXP_BIAS).unwrap_or(0);
            (biased, shifted << (112 - 52))
        } else {
            let unbiased = exp64 - 1023;
            let biased = u128::try_from(unbiased + Self::EXP_BIAS).unwrap_or(0);
            (biased, frac64 << (112 - 52))
        };

        let bits128 = (sign << 127) | (exp128 << 112) | frac128;
        Ok(Self(bits128.to_ne_bytes()))
    }

    /// Decodes `self` into an IEEE 754 `f64`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `self` encodes `NaN` or `Infinity`.
    pub fn to_f64(self) -> Result<f64> {
        let bits128 = u128::from_ne_bytes(self.0);
        let sign = (bits128 >> 127) != 0;
        let exp128 = ((bits128 >> 112) & 0x7FFF) as i32;
        let frac128 = bits128 & ((1_u128 << 112) - 1);

        if exp128 == 0x7FFF {
            return Err(CoblangError::DecimalDomain {
                operation: "FloatBinary128::to_f64",
                message: String::from("non-finite binary128 value"),
            });
        }
        if exp128 == 0 && frac128 == 0 {
            return Ok(if sign { -0.0 } else { 0.0 });
        }

        let unbiased = if exp128 == 0 {
            1 - Self::EXP_BIAS
        } else {
            exp128 - Self::EXP_BIAS
        };
        let mant = if exp128 == 0 {
            frac128
        } else {
            (1_u128 << 112) | frac128
        };
        #[expect(
            clippy::cast_precision_loss,
            reason = "Intentional narrowing from 113-bit binary128 significand to 53-bit f64."
        )]
        let mut val = (mant as f64) * 2.0_f64.powi(unbiased - 112);
        if sign {
            val = -val;
        }
        Ok(val)
    }
}

/// IEEE 754-2008 64-bit Decimal Floating-Point (`FLOAT-DECIMAL-16` / `decimal64` BID64).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Decimal64Bid([u8; 8]);

impl Decimal64Bid {
    /// IEEE 754-2008 `decimal64` exponent bias (`398`).
    const EXP_BIAS: i32 = 398;

    /// Maximum 16-digit BID64 coefficient ($10^{16} - 1$).
    const MAX_COEFF: u64 = 9_999_999_999_999_999;

    /// Wraps raw native-endian 8-byte BID64 storage.
    ///
    /// # Arguments
    ///
    /// * `bytes` - 8-byte native-endian BID64 representation.
    #[must_use]
    pub const fn from_ne_bytes(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    /// Returns raw native-endian 8-byte BID64 storage.
    #[must_use]
    pub const fn to_ne_bytes(self) -> [u8; 8] {
        self.0
    }

    /// Encodes a [`FixedDecimal`] into IEEE 754-2008 `decimal64` BID64 (rounding to 16 significant digits if needed).
    ///
    /// # Arguments
    ///
    /// * `val` - Source [`FixedDecimal`].
    /// * `mode` - Rounding mode when `val` has $> 16$ significant digits.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if the biased exponent overflows `0..=767`.
    pub fn from_fixed_decimal(val: &FixedDecimal, mode: RoundingMode) -> Result<Self> {
        if val.is_zero() {
            let biased = u64::try_from(Self::EXP_BIAS - i32::from(val.scale())).unwrap_or(398);
            return Ok(Self((biased << 53).to_ne_bytes()));
        }

        let mut cur = *val;
        while cur.mantissa().magnitude() > Uint256::from_u64(Self::MAX_COEFF) {
            let next_scale = cur.scale().saturating_sub(1);
            let (r, _) = cur.rescale(next_scale, mode)?;
            cur = r;
        }

        let coeff = cur
            .mantissa()
            .magnitude()
            .to_u128()
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(Self::MAX_COEFF);
        let biased_i32 = Self::EXP_BIAS - i32::from(cur.scale());
        let exp = u64::try_from(biased_i32).unwrap_or(0);
        let sign_bit = if cur.is_negative() { 1_u64 << 63 } else { 0 };
        let bits = if (coeff >> 53) == 0 {
            sign_bit | (exp << 53) | coeff
        } else {
            sign_bit | (0b11_u64 << 61) | (exp << 51) | (coeff & ((1_u64 << 51) - 1))
        };
        Ok(Self(bits.to_ne_bytes()))
    }

    /// Decodes `self` into an exact [`FixedDecimal`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `self` encodes `NaN`/`Infinity` or scale exceeds `76`.
    pub fn to_fixed_decimal(self) -> Result<FixedDecimal> {
        let bits = u64::from_ne_bytes(self.0);
        let is_neg = (bits >> 63) != 0;
        let top2 = (bits >> 61) & 0b11;
        if top2 == 0b11 && ((bits >> 59) & 0b11) == 0b11 {
            return Err(CoblangError::DecimalDomain {
                operation: "Decimal64Bid::to_fixed_decimal",
                message: String::from("non-finite BID64 value"),
            });
        }
        let (biased_exp, raw_coeff) = if top2 == 0b11 {
            let e = ((bits >> 51) & 0x3FF) as i32;
            let c = (0b100_u64 << 51) | (bits & ((1_u64 << 51) - 1));
            (e, c)
        } else {
            let e = ((bits >> 53) & 0x3FF) as i32;
            let c = bits & ((1_u64 << 53) - 1);
            (e, c)
        };
        let coeff = if raw_coeff > Self::MAX_COEFF {
            0
        } else {
            raw_coeff
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "398 - (0..=1023) lies in -625..=398, fitting in i16."
        )]
        let scale = (Self::EXP_BIAS - biased_exp) as i16;
        let sign = if is_neg {
            DecimalSign::Negative
        } else {
            DecimalSign::Positive
        };
        FixedDecimal::try_new(
            SignedInt256::from_parts(Uint256::from_u64(coeff), sign),
            scale,
        )
    }
}

/// IEEE 754-2008 128-bit Decimal Floating-Point (`FLOAT-DECIMAL-34` / `decimal128` BID128).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Decimal128Bid([u8; 16]);

impl Decimal128Bid {
    /// IEEE 754-2008 `decimal128` exponent bias (`6176`).
    const EXP_BIAS: i32 = 6176;

    /// Maximum 34-digit BID128 coefficient ($10^{34} - 1$).
    const MAX_COEFF: u128 = 9_999_999_999_999_999_999_999_999_999_999_999;

    /// Wraps raw native-endian 16-byte BID128 storage.
    ///
    /// # Arguments
    ///
    /// * `bytes` - 16-byte native-endian BID128 representation.
    #[must_use]
    pub const fn from_ne_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns raw native-endian 16-byte BID128 storage.
    #[must_use]
    pub const fn to_ne_bytes(self) -> [u8; 16] {
        self.0
    }

    /// Encodes a [`FixedDecimal`] into IEEE 754-2008 `decimal128` BID128 (rounding to 34 significant digits if needed).
    ///
    /// # Arguments
    ///
    /// * `val` - Source [`FixedDecimal`].
    /// * `mode` - Rounding mode when `val` has $> 34$ significant digits.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] on internal scale rescaling error.
    pub fn from_fixed_decimal(val: &FixedDecimal, mode: RoundingMode) -> Result<Self> {
        if val.is_zero() {
            let biased = u128::try_from(Self::EXP_BIAS - i32::from(val.scale())).unwrap_or(6176);
            return Ok(Self((biased << 113).to_ne_bytes()));
        }

        let mut cur = *val;
        while cur.mantissa().magnitude() > Uint256::from_u128(Self::MAX_COEFF) {
            let next_scale = cur.scale().saturating_sub(1);
            let (r, _) = cur.rescale(next_scale, mode)?;
            cur = r;
        }

        let coeff = cur
            .mantissa()
            .magnitude()
            .to_u128()
            .unwrap_or(Self::MAX_COEFF);
        let biased_i32 = Self::EXP_BIAS - i32::from(cur.scale());
        let exp = u128::try_from(biased_i32).unwrap_or(0);
        let sign_bit = if cur.is_negative() { 1_u128 << 127 } else { 0 };
        let bits = sign_bit | (exp << 113) | coeff;
        Ok(Self(bits.to_ne_bytes()))
    }

    /// Decodes `self` into an exact [`FixedDecimal`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `self` encodes `NaN`/`Infinity` or scale exceeds `76`.
    pub fn to_fixed_decimal(self) -> Result<FixedDecimal> {
        let bits = u128::from_ne_bytes(self.0);
        let is_neg = (bits >> 127) != 0;
        let top2 = (bits >> 125) & 0b11;
        if top2 == 0b11 && ((bits >> 123) & 0b11) == 0b11 {
            return Err(CoblangError::DecimalDomain {
                operation: "Decimal128Bid::to_fixed_decimal",
                message: String::from("non-finite BID128 value"),
            });
        }
        let (biased_exp, raw_coeff) = if top2 == 0b11 {
            let e = ((bits >> 111) & 0x3FFF) as i32;
            let c = (0b100_u128 << 111) | (bits & ((1_u128 << 111) - 1));
            (e, c)
        } else {
            let e = ((bits >> 113) & 0x3FFF) as i32;
            let c = bits & ((1_u128 << 113) - 1);
            (e, c)
        };
        let coeff = if raw_coeff > Self::MAX_COEFF {
            0
        } else {
            raw_coeff
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "6176 - (0..=16383) lies in -10207..=6176, fitting in i16."
        )]
        let scale = (Self::EXP_BIAS - biased_exp) as i16;
        let sign = if is_neg {
            DecimalSign::Negative
        } else {
            DecimalSign::Positive
        };
        FixedDecimal::try_new(
            SignedInt256::from_parts(Uint256::from_u128(coeff), sign),
            scale,
        )
    }
}

/// Splits a single BCD digit (`0..=9`) into its four `u16` bits `(b3, b2, b1, b0)`.
///
/// # Arguments
///
/// * `digit` - Decimal digit (`0..=9`).
#[must_use]
const fn bcd_bits(digit: u8) -> (u16, u16, u16, u16) {
    (
        (digit >> 3) as u16,
        ((digit >> 2) & 1) as u16,
        ((digit >> 1) & 1) as u16,
        (digit & 1) as u16,
    )
}

/// Packs three decimal digits `(d2, d1, d0)` (`0..=9`) into an IEEE 754-2008 10-bit Densely Packed Decimal (DPD) declet.
///
/// # Arguments
///
/// * `digits` - High-to-low decimal digits `[d2, d1, d0]`, each in `0..=9`.
///
/// # Errors
///
/// Returns [`CoblangError::DecimalDomain`] if any digit exceeds `9`.
pub fn pack_dpd_declet(digits: [u8; 3]) -> Result<u16> {
    let [d2, d1, d0] = digits;
    if d2 > 9 || d1 > 9 || d0 > 9 {
        return Err(CoblangError::DecimalDomain {
            operation: "pack_dpd_declet",
            message: format!("DPD digit out of 0..=9: [{d2}, {d1}, {d0}]"),
        });
    }
    let (d2_b3, d2_b2, d2_b1, d2_b0) = bcd_bits(d2);
    let (d1_b3, d1_b2, d1_b1, d1_b0) = bcd_bits(d1);
    let (d0_b3, d0_b2, d0_b1, d0_b0) = bcd_bits(d0);
    let low3_flag = 1_u16 << 3;
    let declet = match (d2_b3, d1_b3, d0_b3) {
        (0, 0, 0) => {
            (d2_b2 << 9)
                | (d2_b1 << 8)
                | (d2_b0 << 7)
                | (d1_b2 << 6)
                | (d1_b1 << 5)
                | (d1_b0 << 4)
                | (d0_b2 << 2)
                | (d0_b1 << 1)
                | d0_b0
        }
        (0, 0, 1) => {
            (d2_b2 << 9)
                | (d2_b1 << 8)
                | (d2_b0 << 7)
                | (d1_b2 << 6)
                | (d1_b1 << 5)
                | (d1_b0 << 4)
                | low3_flag
                | d0_b0
        }
        (0, 1, 0) => {
            (d2_b2 << 9)
                | (d2_b1 << 8)
                | (d2_b0 << 7)
                | (d0_b2 << 6)
                | (d0_b1 << 5)
                | (d1_b0 << 4)
                | low3_flag
                | 0b010
                | d0_b0
        }
        (1, 0, 0) => {
            (d0_b2 << 9)
                | (d0_b1 << 8)
                | (d2_b0 << 7)
                | (d1_b2 << 6)
                | (d1_b1 << 5)
                | (d1_b0 << 4)
                | low3_flag
                | 0b100
                | d0_b0
        }
        (0, 1, 1) => {
            (d2_b2 << 9)
                | (d2_b1 << 8)
                | (d2_b0 << 7)
                | (1 << 6)
                | (d1_b0 << 4)
                | low3_flag
                | 0b110
                | d0_b0
        }
        (1, 0, 1) => {
            (d1_b2 << 9)
                | (d1_b1 << 8)
                | (d2_b0 << 7)
                | (1 << 5)
                | (d1_b0 << 4)
                | low3_flag
                | 0b110
                | d0_b0
        }
        (1, 1, 0) => {
            (d0_b2 << 9) | (d0_b1 << 8) | (d2_b0 << 7) | (d1_b0 << 4) | low3_flag | 0b110 | d0_b0
        }
        _ => (d2_b0 << 7) | (0b11 << 5) | (d1_b0 << 4) | low3_flag | 0b110 | d0_b0,
    };
    Ok(declet)
}

/// Unpacks a 10-bit IEEE 754-2008 Densely Packed Decimal (DPD) declet (`0..=0x3FF`) into `[d2, d1, d0]`.
///
/// # Arguments
///
/// * `declet` - 10-bit DPD declet (`<= 0x3FF`).
///
/// # Errors
///
/// Returns [`CoblangError::DecimalDomain`] if `declet > 0x3FF`.
pub fn unpack_dpd_declet(declet: u16) -> Result<[u8; 3]> {
    if declet > 0x03FF {
        return Err(CoblangError::DecimalDomain {
            operation: "unpack_dpd_declet",
            message: format!("DPD declet 0x{declet:04X} exceeds 10 bits"),
        });
    }
    let (b9, b8, b7, b6, b5, b4, b3, b2, b1, b0) = (
        ((declet >> 9) & 1) as u8,
        ((declet >> 8) & 1) as u8,
        ((declet >> 7) & 1) as u8,
        ((declet >> 6) & 1) as u8,
        ((declet >> 5) & 1) as u8,
        ((declet >> 4) & 1) as u8,
        ((declet >> 3) & 1) as u8,
        ((declet >> 2) & 1) as u8,
        ((declet >> 1) & 1) as u8,
        (declet & 1) as u8,
    );

    if b3 == 0 {
        return Ok([
            (b9 << 2) | (b8 << 1) | b7,
            (b6 << 2) | (b5 << 1) | b4,
            (b2 << 2) | (b1 << 1) | b0,
        ]);
    }
    match (b2, b1) {
        (0, 0) => Ok([
            (b9 << 2) | (b8 << 1) | b7,
            (b6 << 2) | (b5 << 1) | b4,
            8 | b0,
        ]),
        (0, 1) => Ok([
            (b9 << 2) | (b8 << 1) | b7,
            8 | b4,
            (b6 << 2) | (b5 << 1) | b0,
        ]),
        (1, 0) => Ok([
            8 | b7,
            (b6 << 2) | (b5 << 1) | b4,
            (b9 << 2) | (b8 << 1) | b0,
        ]),
        _ => match (b6, b5) {
            (0, 0) => Ok([8 | b7, 8 | b4, (b9 << 2) | (b8 << 1) | b0]),
            (0, 1) => Ok([8 | b7, (b9 << 2) | (b8 << 1) | b4, 8 | b0]),
            (1, 0) => Ok([(b9 << 2) | (b8 << 1) | b7, 8 | b4, 8 | b0]),
            _ => Ok([8 | b7, 8 | b4, 8 | b0]),
        },
    }
}
