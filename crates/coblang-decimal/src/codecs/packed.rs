//! `PACKED-DECIMAL` (`COMP-3`) and unsigned `COMP-6` BCD encoder, decoder, and `IS NUMERIC` validator.

use coblang_common::{CoblangError, DigitCount, Result, ScaleFactor};

use crate::fixed_decimal::{FixedDecimal, HighOrderPolicy, SignednessPolicy};
use crate::rounding::{RoundingMode, SizeErrorStatus};
use crate::wide_int::{DecimalSign, SignedInt256, Uint256};

/// Storage variant of a packed BCD field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackedKind {
    /// Signed `PACKED-DECIMAL` / `COMP-3` (`0x0C` positive/zero, `0x0D` negative).
    Comp3Signed,
    /// Unsigned `PACKED-DECIMAL` / `COMP-3` (`0x0F` unsigned sign nibble).
    Comp3Unsigned,
    /// Unsigned `COMP-6` BCD without a trailing sign nibble.
    Comp6Unsigned,
}

impl PackedKind {
    /// Returns `true` when this kind carries a sign (`Comp3Signed`).
    #[must_use]
    pub const fn is_signed(self) -> bool {
        matches!(self, Self::Comp3Signed)
    }
}

/// Dialect sign-nibble acceptance policy for `IS NUMERIC` on `COMP-3` items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackedSignPolicy {
    /// `GnuCOBOL` strict sign-nibble check (`0x0F` only for unsigned; `0x0C`/`0x0D` for signed).
    Gnu,
    /// IBM mainframe sign-nibble check (`0x0F` for unsigned; `0x0A..=0x0F` for signed).
    Ibm,
}

/// Physical storage descriptor for a `COMP-3` or `COMP-6` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PackedLayout {
    /// Total `PICTURE` decimal digits (`1..=38`).
    pub digits: DigitCount,
    /// Fractional / `P` scale factor (`-38..=38`).
    pub scale: ScaleFactor,
    /// `COMP-3` signed, `COMP-3` unsigned, or `COMP-6` unsigned.
    pub kind: PackedKind,
}

impl PackedLayout {
    /// Returns exact storage byte length: `(digits / 2) + 1` for `COMP-3`, `(digits + 1) / 2` for `COMP-6`.
    #[must_use]
    pub const fn byte_len(self) -> usize {
        let d = self.digits.get() as usize;
        match self.kind {
            PackedKind::Comp3Signed | PackedKind::Comp3Unsigned => (d / 2) + 1,
            PackedKind::Comp6Unsigned => d.div_ceil(2),
        }
    }
}

/// Encodes `value` into packed BCD bytes according to `layout`, `mode`, and `high_order`.
///
/// # Arguments
///
/// * `value` - Source [`FixedDecimal`].
/// * `layout` - Target [`PackedLayout`].
/// * `mode` - Fractional rounding mode.
/// * `high_order` - Overflow vs modulo-truncation policy.
///
/// # Errors
///
/// Returns [`CoblangError::DecimalDomain`] on internal scale bound violation.
pub fn encode_packed(
    value: &FixedDecimal,
    layout: PackedLayout,
    mode: RoundingMode,
    high_order: HighOrderPolicy,
) -> Result<(Vec<u8>, SizeErrorStatus)> {
    let signed_policy = if layout.kind.is_signed() {
        SignednessPolicy::Signed
    } else {
        SignednessPolicy::Unsigned
    };
    let (fitted, status) =
        value.fit_to_picture(layout.digits, layout.scale, signed_policy, mode, high_order)?;

    let digit_count = usize::from(layout.digits.get());
    let raw_ascii = fitted.mantissa().magnitude().to_decimal_digits();
    let total_nibbles = match layout.kind {
        PackedKind::Comp3Signed | PackedKind::Comp3Unsigned => layout.byte_len() * 2 - 1,
        PackedKind::Comp6Unsigned => layout.byte_len() * 2,
    };

    let mut nibbles = vec![0_u8; total_nibbles];
    let copy_len = raw_ascii.len().min(digit_count);
    let src_start = raw_ascii.len() - copy_len;
    let dst_start = total_nibbles - copy_len;
    for (slot, &src_b) in nibbles
        .get_mut(dst_start..)
        .unwrap_or(&mut [])
        .iter_mut()
        .zip(raw_ascii.get(src_start..).unwrap_or(&[]))
    {
        *slot = src_b - b'0';
    }

    match layout.kind {
        PackedKind::Comp3Signed => {
            let sign_nib = if fitted.is_negative() { 0x0D } else { 0x0C };
            nibbles.push(sign_nib);
        }
        PackedKind::Comp3Unsigned => {
            nibbles.push(0x0F);
        }
        PackedKind::Comp6Unsigned => {}
    }

    let mut out = Vec::with_capacity(layout.byte_len());
    let mut idx = 0;
    while idx + 1 < nibbles.len() {
        let hi = nibbles.get(idx).copied().unwrap_or(0);
        let lo = nibbles.get(idx + 1).copied().unwrap_or(0);
        out.push((hi << 4) | lo);
        idx += 2;
    }
    Ok((out, status))
}

/// Decodes packed BCD `bytes` into a [`FixedDecimal`] according to `layout`.
///
/// # Arguments
///
/// * `bytes` - Raw storage bytes of length `layout.byte_len()`.
/// * `layout` - [`PackedLayout`] descriptor.
///
/// # Errors
///
/// Returns [`CoblangError::DecimalDomain`] if `bytes.len() != layout.byte_len()` or nibbles are invalid BCD.
pub fn decode_packed(bytes: &[u8], layout: PackedLayout) -> Result<FixedDecimal> {
    let expected = layout.byte_len();
    if bytes.len() != expected {
        return Err(CoblangError::DecimalDomain {
            operation: "decode_packed",
            message: format!("expected {expected} packed bytes, found {}", bytes.len()),
        });
    }

    let mut ascii_digits = Vec::with_capacity(expected * 2);
    let is_neg = match layout.kind {
        PackedKind::Comp6Unsigned => {
            for &b in bytes {
                let hi = b >> 4;
                let lo = b & 0x0F;
                if hi > 9 || lo > 9 {
                    return Err(CoblangError::DecimalDomain {
                        operation: "decode_packed",
                        message: format!("invalid COMP-6 BCD byte 0x{b:02X}"),
                    });
                }
                ascii_digits.push(b'0' + hi);
                ascii_digits.push(b'0' + lo);
            }
            false
        }
        PackedKind::Comp3Signed | PackedKind::Comp3Unsigned => {
            let (&last_byte, prefix) = bytes.split_last().unwrap_or((&0x0F, &[]));
            for &b in prefix {
                let hi = b >> 4;
                let lo = b & 0x0F;
                if hi > 9 || lo > 9 {
                    return Err(CoblangError::DecimalDomain {
                        operation: "decode_packed",
                        message: format!("invalid COMP-3 BCD byte 0x{b:02X}"),
                    });
                }
                ascii_digits.push(b'0' + hi);
                ascii_digits.push(b'0' + lo);
            }
            let last_digit = last_byte >> 4;
            let sign_nib = last_byte & 0x0F;
            if last_digit > 9 || sign_nib < 0x0A {
                return Err(CoblangError::DecimalDomain {
                    operation: "decode_packed",
                    message: format!("invalid COMP-3 trailing byte 0x{last_byte:02X}"),
                });
            }
            ascii_digits.push(b'0' + last_digit);
            layout.kind.is_signed() && matches!(sign_nib, 0x0B | 0x0D)
        }
    };

    // Enforce declared PICTURE digit count by keeping only the rightmost `digits` nibbles
    // if an unused high pad nibble had a non-zero digit.
    let declared = usize::from(layout.digits.get());
    let slice_start = ascii_digits.len().saturating_sub(declared);
    let relevant = ascii_digits.get(slice_start..).unwrap_or(&ascii_digits);
    let mag = Uint256::from_decimal_digits(relevant)?;
    let sign = if is_neg && !mag.is_zero() {
        DecimalSign::Negative
    } else {
        DecimalSign::Positive
    };
    FixedDecimal::try_new(SignedInt256::from_parts(mag, sign), layout.scale.get())
}

/// Evaluates COBOL `IS NUMERIC` on a `COMP-3` / `COMP-6` byte slice under `policy`.
///
/// # Arguments
///
/// * `bytes` - Raw storage bytes.
/// * `layout` - [`PackedLayout`] descriptor.
/// * `policy` - Dialect sign-nibble acceptance policy (`Gnu` vs `Ibm`).
#[must_use]
pub fn is_valid_packed_bcd(bytes: &[u8], layout: PackedLayout, policy: PackedSignPolicy) -> bool {
    if bytes.len() != layout.byte_len() {
        return false;
    }
    let digits = layout.digits.get();
    match layout.kind {
        PackedKind::Comp6Unsigned => {
            if (digits & 1) == 1 && (bytes.first().copied().unwrap_or(0) >> 4) != 0 {
                return false;
            }
            bytes.iter().all(|&b| (b >> 4) <= 9 && (b & 0x0F) <= 9)
        }
        PackedKind::Comp3Unsigned | PackedKind::Comp3Signed => {
            if (digits & 1) == 0 && (bytes.first().copied().unwrap_or(0) >> 4) != 0 {
                return false;
            }
            let (&last_b, prefix) = bytes.split_last().unwrap_or((&0, &[]));
            if !prefix.iter().all(|&b| (b >> 4) <= 9 && (b & 0x0F) <= 9) {
                return false;
            }
            if (last_b >> 4) > 9 {
                return false;
            }
            let sign_nib = last_b & 0x0F;
            if layout.kind == PackedKind::Comp3Unsigned {
                sign_nib == 0x0F
            } else {
                match policy {
                    PackedSignPolicy::Gnu => matches!(sign_nib, 0x0C | 0x0D),
                    PackedSignPolicy::Ibm => (0x0A..=0x0F).contains(&sign_nib),
                }
            }
        }
    }
}
