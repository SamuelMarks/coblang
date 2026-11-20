//! 1..16-byte big-endian and native-endian `BINARY` / `COMP` / `COMP-4` / `COMP-5` / `COMP-X` codecs.

use coblang_common::{CoblangError, DigitCount, Result, ScaleFactor};

use crate::fixed_decimal::{FixedDecimal, HighOrderPolicy, SignednessPolicy};
use crate::rounding::{RoundingMode, SizeErrorStatus};
use crate::wide_int::{DecimalSign, SignedInt256};

/// Physical byte ordering of a binary integer COBOL item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryByteOrder {
    /// Big-endian network/mainframe byte order (`BINARY`, `COMP`, `COMP-4`, `COMP-X`).
    BigEndian,
    /// Host native byte order (`COMP-5`, `BINARY-CHAR/SHORT/LONG/DOUBLE`).
    NativeEndian,
}

impl BinaryByteOrder {
    /// Resolves [`Self::NativeEndian`] to `true` on little-endian hosts and `false` on big-endian hosts.
    #[must_use]
    pub const fn is_little_endian(self) -> bool {
        match self {
            Self::BigEndian => false,
            Self::NativeEndian => cfg!(target_endian = "little"),
        }
    }
}

/// Signedness of a binary integer COBOL item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinarySignMode {
    /// Two's-complement signed integer (`PIC S9...` or signed `BINARY-*`).
    Signed,
    /// Unsigned binary integer (`PIC 9...`, `COMP-X`, or `BINARY-* UNSIGNED`).
    Unsigned,
}

/// High-order range enforcement policy on a binary integer COBOL item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryTruncMode {
    /// Truncate/check against `PICTURE` decimal digit capacity `10^digits - 1`.
    TruncateToPic,
    /// Use full hardware bit width (`1..=16` bytes: `i8..i128` / `u8..u128`), as in `COMP-5` or `-fnotrunc`.
    FullBinaryRange,
}

/// Complete physical storage layout descriptor of a binary integer field (`1..=16` bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BinaryLayout {
    /// Physical storage width in bytes (`1..=16`).
    pub byte_width: u8,
    /// Declared `PICTURE` decimal digit count (`1..=38`).
    pub digits: DigitCount,
    /// Fractional / `P` scale factor (`-38..=38`).
    pub scale: ScaleFactor,
    /// Signed two's-complement vs unsigned storage.
    pub sign: BinarySignMode,
    /// Big-endian vs host native-endian byte order.
    pub byte_order: BinaryByteOrder,
    /// Decimal `PICTURE` truncation vs full hardware bit-width range.
    pub trunc: BinaryTruncMode,
}

impl BinaryLayout {
    /// Validates that `byte_width` lies in `1..=16`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DecimalDomain`] if `byte_width == 0` or `byte_width > 16`.
    pub fn validate(self) -> Result<Self> {
        if !(1..=16).contains(&self.byte_width) {
            return Err(CoblangError::DecimalDomain {
                operation: "BinaryLayout::validate",
                message: format!("binary byte_width {} outside 1..=16", self.byte_width),
            });
        }
        Ok(self)
    }
}

/// Encodes `value` into `1..=16` binary integer bytes according to `layout`, `mode`, and `high_order`.
///
/// # Arguments
///
/// * `value` - Source [`FixedDecimal`].
/// * `layout` - Target [`BinaryLayout`].
/// * `mode` - Rounding mode for fractional digits.
/// * `high_order` - Overflow vs wrap/modulo-truncation policy.
///
/// # Errors
///
/// Returns [`CoblangError::DecimalDomain`] if `layout.byte_width` lies outside `1..=16`.
pub fn encode_binary(
    value: &FixedDecimal,
    layout: BinaryLayout,
    mode: RoundingMode,
    high_order: HighOrderPolicy,
) -> Result<(Vec<u8>, SizeErrorStatus)> {
    let layout = layout.validate()?;
    let width = usize::from(layout.byte_width);
    let signed_policy = match layout.sign {
        BinarySignMode::Signed => SignednessPolicy::Signed,
        BinarySignMode::Unsigned => SignednessPolicy::Unsigned,
    };

    if layout.trunc == BinaryTruncMode::TruncateToPic {
        let (fitted, pic_status) =
            value.fit_to_picture(layout.digits, layout.scale, signed_policy, mode, high_order)?;
        if pic_status.is_error() && high_order == HighOrderPolicy::FlagOverflow {
            return Ok((vec![0_u8; width], pic_status));
        }
        return Ok(pack_int256_to_bytes(
            fitted.mantissa(),
            layout,
            high_order,
            pic_status,
        ));
    }

    let (rescaled, round_status) = value.rescale(layout.scale.get(), mode)?;
    if round_status.is_error() {
        return Ok((vec![0_u8; width], round_status));
    }
    let normalized = if layout.sign == BinarySignMode::Unsigned {
        rescaled.mantissa().abs()
    } else {
        rescaled.mantissa()
    };
    Ok(pack_int256_to_bytes(
        normalized,
        layout,
        high_order,
        SizeErrorStatus::Exact,
    ))
}

/// Decodes `1..=16` binary integer `bytes` into a [`FixedDecimal`] according to `layout`.
///
/// # Arguments
///
/// * `bytes` - Raw storage bytes of length `layout.byte_width`.
/// * `layout` - [`BinaryLayout`] descriptor.
///
/// # Errors
///
/// Returns [`CoblangError::DecimalDomain`] if `layout.byte_width` is invalid or `bytes.len() != layout.byte_width`.
pub fn decode_binary(bytes: &[u8], layout: BinaryLayout) -> Result<FixedDecimal> {
    let layout = layout.validate()?;
    let width = usize::from(layout.byte_width);
    if bytes.len() != width {
        return Err(CoblangError::DecimalDomain {
            operation: "decode_binary",
            message: format!("expected {width} binary bytes, found {}", bytes.len()),
        });
    }

    // Assemble big-endian 16-byte buffer with sign or zero extension.
    let is_le = layout.byte_order.is_little_endian();
    let msb = if is_le {
        bytes.last().copied().unwrap_or(0)
    } else {
        bytes.first().copied().unwrap_or(0)
    };
    let neg_ext = layout.sign == BinarySignMode::Signed && (msb & 0x80) != 0;
    let fill = if neg_ext { 0xFF_u8 } else { 0x00_u8 };

    let mut raw16 = [fill; 16];
    if is_le {
        raw16
            .get_mut(..width)
            .unwrap_or(&mut [])
            .copy_from_slice(bytes);
        let mant = match layout.sign {
            BinarySignMode::Signed => SignedInt256::from_i128(i128::from_le_bytes(raw16)),
            BinarySignMode::Unsigned => SignedInt256::from_u128(u128::from_le_bytes(raw16)),
        };
        FixedDecimal::try_new(mant, layout.scale.get())
    } else {
        let pad = 16 - width;
        raw16
            .get_mut(pad..)
            .unwrap_or(&mut [])
            .copy_from_slice(bytes);
        let mant = match layout.sign {
            BinarySignMode::Signed => SignedInt256::from_i128(i128::from_be_bytes(raw16)),
            BinarySignMode::Unsigned => SignedInt256::from_u128(u128::from_be_bytes(raw16)),
        };
        FixedDecimal::try_new(mant, layout.scale.get())
    }
}

/// Packs `mant` into `layout.byte_width` two's-complement / unsigned bytes, checking hardware bounds.
///
/// # Arguments
///
/// * `mant` - Scaled integer mantissa.
/// * `layout` - Validated binary layout (`1..=16` bytes).
/// * `high_order` - Overflow vs two's-complement wrap policy.
/// * `prior_status` - Upstream `PICTURE` fit status.
fn pack_int256_to_bytes(
    mant: SignedInt256,
    layout: BinaryLayout,
    high_order: HighOrderPolicy,
    prior_status: SizeErrorStatus,
) -> (Vec<u8>, SizeErrorStatus) {
    let width = usize::from(layout.byte_width);
    let bits = u32::from(layout.byte_width) * 8;

    let (low128_bits, fits_hw) = match layout.sign {
        BinarySignMode::Unsigned => {
            let max_u128 = if bits == 128 {
                u128::MAX
            } else {
                (1_u128 << bits) - 1
            };
            if let Some(v) = mant.to_u128()
                && v <= max_u128
            {
                (v, true)
            } else {
                let [l0, l1, _, _] = mant.magnitude().limbs();
                let raw = ((u128::from(l1)) << 64) | u128::from(l0);
                (raw, false)
            }
        }
        BinarySignMode::Signed => {
            let (min_i128, max_i128) = if bits == 128 {
                (i128::MIN, i128::MAX)
            } else {
                let half = 1_i128 << (bits - 1);
                (-half, half - 1)
            };
            if let Some(v) = mant.to_i128()
                && (min_i128..=max_i128).contains(&v)
            {
                (v.cast_unsigned(), true)
            } else {
                let [l0, l1, _, _] = mant.magnitude().limbs();
                let abs_low = ((u128::from(l1)) << 64) | u128::from(l0);
                let twos = if mant.sign() == DecimalSign::Negative {
                    abs_low.wrapping_neg()
                } else {
                    abs_low
                };
                (twos, false)
            }
        }
    };

    if !fits_hw && high_order == HighOrderPolicy::FlagOverflow {
        return (vec![0_u8; width], SizeErrorStatus::Overflowed);
    }

    let bytes = if layout.byte_order.is_little_endian() {
        low128_bits
            .to_le_bytes()
            .get(..width)
            .unwrap_or(&[])
            .to_vec()
    } else {
        let be = low128_bits.to_be_bytes();
        be.get(16 - width..).unwrap_or(&[]).to_vec()
    };
    (bytes, prior_status)
}
