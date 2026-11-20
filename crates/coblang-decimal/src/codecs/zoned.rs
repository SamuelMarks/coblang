//! Zoned decimal (`USAGE DISPLAY`) encoder, decoder, and `IS NUMERIC` validator.

use coblang_common::{CoblangError, DigitCount, Result, ScaleFactor};

use crate::fixed_decimal::{FixedDecimal, HighOrderPolicy, SignednessPolicy};
use crate::rounding::{RoundingMode, SizeErrorStatus};
use crate::wide_int::{DecimalSign, SignedInt256, Uint256};

/// Overpunch sign encoding convention for `USAGE DISPLAY` zoned decimal items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverpunchDialect {
    /// `GnuCOBOL` / `gcobol` `-fsign=ascii`: `'0'..'9'` (`0x30..0x39`) positive, `'p'..'y'` (`0x70..0x79`) negative.
    AsciiGnu,
    /// COBOL-85 / `GnuCOBOL` `-fsign=ebcdic` on ASCII: `'{'`,`'A'..'I'` positive, `'}'`,`'J'..'R'` negative.
    Cobol85Standard,
    /// Native IBM EBCDIC (`-finternal-ebcdic`): `0xF0..0xF9` unsigned, `0xC0..0xC9` positive, `0xD0..0xD9` negative.
    EbcdicIbm,
    /// `MicroFocus` ASCII display sign convention (`'0'..'9'` positive, `'p'..'y'` negative; decodes `{`/`}`/`A..R` as well).
    MicroFocus,
}

impl OverpunchDialect {
    /// Returns the space byte (`0x20` ASCII or `0x40` EBCDIC) for this dialect.
    #[must_use]
    pub const fn space_byte(self) -> u8 {
        match self {
            Self::EbcdicIbm => 0x40,
            Self::AsciiGnu | Self::Cobol85Standard | Self::MicroFocus => b' ',
        }
    }

    /// Returns the unsigned zero digit byte (`0x30` ASCII or `0xF0` EBCDIC).
    #[must_use]
    pub const fn zero_digit_byte(self) -> u8 {
        match self {
            Self::EbcdicIbm => 0xF0,
            Self::AsciiGnu | Self::Cobol85Standard | Self::MicroFocus => b'0',
        }
    }

    /// Returns `(plus_byte, minus_byte)` for `SIGN IS SEPARATE`.
    #[must_use]
    pub const fn separate_sign_bytes(self) -> (u8, u8) {
        match self {
            Self::EbcdicIbm => (0x4E, 0x60),
            Self::AsciiGnu | Self::Cobol85Standard | Self::MicroFocus => (b'+', b'-'),
        }
    }
}

/// Sign storage placement of a `USAGE DISPLAY` zoned decimal field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignPlacement {
    /// Unsigned (`PIC 9...`).
    Unsigned,
    /// Sign overpunched into the last byte (default `PIC S9...`).
    EmbeddedTrailing,
    /// Sign overpunched into the first byte (`SIGN IS LEADING`).
    EmbeddedLeading,
    /// Separate trailing `'+'` / `'-'` byte (`SIGN IS TRAILING SEPARATE`).
    SeparateTrailing,
    /// Separate leading `'+'` / `'-'` byte (`SIGN IS LEADING SEPARATE`).
    SeparateLeading,
}

impl SignPlacement {
    /// Returns `true` when this placement stores a sign (`!= Self::Unsigned`).
    #[must_use]
    pub const fn is_signed(self) -> bool {
        !matches!(self, Self::Unsigned)
    }

    /// Returns `true` when this placement uses an extra sign byte (`Separate*`).
    #[must_use]
    pub const fn has_separate_sign_byte(self) -> bool {
        matches!(self, Self::SeparateTrailing | Self::SeparateLeading)
    }
}

/// Zero-blanking policy (`BLANK WHEN ZERO`) on a zoned decimal field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlankZeroMode {
    /// Normal zoned zero representation.
    RetainZero,
    /// Fill entire field with spaces when quantized value is zero.
    BlankWhenZero,
}

/// Complete physical storage layout descriptor of a zoned decimal (`USAGE DISPLAY`) field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ZonedLayout {
    /// Total `PICTURE` digit positions (`1..=38`).
    pub digits: DigitCount,
    /// Fractional / `P` scale factor (`-38..=38`).
    pub scale: ScaleFactor,
    /// Sign placement (`Unsigned`, `Embedded*`, `Separate*`).
    pub sign: SignPlacement,
    /// Overpunch encoding convention.
    pub dialect: OverpunchDialect,
    /// `BLANK WHEN ZERO` policy.
    pub blank_zero: BlankZeroMode,
}

impl ZonedLayout {
    /// Returns exact storage byte length (`digits` or `digits + 1` for separate sign).
    #[must_use]
    pub const fn byte_len(self) -> usize {
        let base = self.digits.get() as usize;
        if self.sign.has_separate_sign_byte() {
            base + 1
        } else {
            base
        }
    }
}

/// Encodes `value` into a zoned decimal byte buffer according to `layout`, `mode`, and `high_order`.
///
/// # Arguments
///
/// * `value` - Source [`FixedDecimal`].
/// * `layout` - Target [`ZonedLayout`] descriptor.
/// * `mode` - Rounding mode for fractional truncation.
/// * `high_order` - High-order digit overflow vs modulo-truncation policy.
///
/// # Errors
///
/// Returns [`CoblangError::DecimalDomain`] on internal scale bound violation.
pub fn encode_zoned(
    value: &FixedDecimal,
    layout: ZonedLayout,
    mode: RoundingMode,
    high_order: HighOrderPolicy,
) -> Result<(Vec<u8>, SizeErrorStatus)> {
    let signed_policy = if layout.sign.is_signed() {
        SignednessPolicy::Signed
    } else {
        SignednessPolicy::Unsigned
    };
    let (fitted, status) =
        value.fit_to_picture(layout.digits, layout.scale, signed_policy, mode, high_order)?;

    let total_len = layout.byte_len();
    if fitted.is_zero() && layout.blank_zero == BlankZeroMode::BlankWhenZero {
        return Ok((vec![layout.dialect.space_byte(); total_len], status));
    }

    let digit_count = usize::from(layout.digits.get());
    let raw_ascii = fitted.mantissa().magnitude().to_decimal_digits();
    let zero_base = layout.dialect.zero_digit_byte();
    let copy_len = raw_ascii.len().min(digit_count);
    let src_start = raw_ascii.len() - copy_len;
    let dst_start = digit_count - copy_len;
    let is_neg = fitted.is_negative();

    let mut digit_buf: Vec<u8> = (0..digit_count)
        .map(|idx| {
            let d = if idx < dst_start {
                0
            } else {
                raw_ascii
                    .get(src_start + (idx - dst_start))
                    .copied()
                    .unwrap_or(b'0')
                    - b'0'
            };
            if (layout.sign == SignPlacement::EmbeddedLeading && idx == 0)
                || (layout.sign == SignPlacement::EmbeddedTrailing && idx + 1 == digit_count)
            {
                encode_overpunch_byte(d, is_neg, layout.dialect)
            } else {
                zero_base + d
            }
        })
        .collect();

    let (plus_ch, minus_ch) = layout.dialect.separate_sign_bytes();
    let sep_ch = if is_neg { minus_ch } else { plus_ch };

    let encoded = match layout.sign {
        SignPlacement::Unsigned
        | SignPlacement::EmbeddedLeading
        | SignPlacement::EmbeddedTrailing => digit_buf,
        SignPlacement::SeparateLeading => {
            let mut out = Vec::with_capacity(total_len);
            out.push(sep_ch);
            out.extend_from_slice(&digit_buf);
            out
        }
        SignPlacement::SeparateTrailing => {
            digit_buf.push(sep_ch);
            digit_buf
        }
    };
    Ok((encoded, status))
}

/// Decodes a zoned decimal byte slice into a [`FixedDecimal`] according to `layout`.
///
/// # Arguments
///
/// * `bytes` - Raw storage bytes of length `layout.byte_len()`.
/// * `layout` - [`ZonedLayout`] descriptor.
///
/// # Errors
///
/// Returns [`CoblangError::DecimalDomain`] if `bytes.len() != layout.byte_len()` or any byte is invalid.
pub fn decode_zoned(bytes: &[u8], layout: ZonedLayout) -> Result<FixedDecimal> {
    let expected = layout.byte_len();
    if bytes.len() != expected {
        return Err(CoblangError::DecimalDomain {
            operation: "decode_zoned",
            message: format!("expected {expected} zoned bytes, found {}", bytes.len()),
        });
    }

    let sp = layout.dialect.space_byte();
    if layout.blank_zero == BlankZeroMode::BlankWhenZero && bytes.iter().all(|&b| b == sp) {
        return FixedDecimal::try_new(SignedInt256::ZERO, layout.scale.get());
    }

    let (is_neg, ascii_digits) = extract_zoned_digits_and_sign(bytes, layout)?;
    let mag = Uint256::from_decimal_digits(&ascii_digits)?;
    let sign = if !layout.sign.is_signed() || mag.is_zero() {
        DecimalSign::Positive
    } else if is_neg {
        DecimalSign::Negative
    } else {
        DecimalSign::Positive
    };
    FixedDecimal::try_new(SignedInt256::from_parts(mag, sign), layout.scale.get())
}

/// Evaluates COBOL `IS NUMERIC` class condition on a zoned decimal `bytes` slice.
///
/// # Arguments
///
/// * `bytes` - Raw field storage bytes.
/// * `layout` - [`ZonedLayout`] descriptor.
#[must_use]
pub fn is_valid_zoned_numeric(bytes: &[u8], layout: ZonedLayout) -> bool {
    if bytes.len() != layout.byte_len() {
        return false;
    }
    match layout.sign {
        SignPlacement::Unsigned => bytes
            .iter()
            .all(|&b| decode_plain_digit(b, layout.dialect).is_some()),
        SignPlacement::SeparateLeading => {
            let (&sign_b, rest) = bytes.split_first().unwrap_or((&0, &[]));
            is_valid_separate_sign(sign_b, layout.dialect)
                && rest
                    .iter()
                    .all(|&b| decode_plain_digit(b, layout.dialect).is_some())
        }
        SignPlacement::SeparateTrailing => {
            let (&sign_b, rest) = bytes.split_last().unwrap_or((&0, &[]));
            is_valid_separate_sign(sign_b, layout.dialect)
                && rest
                    .iter()
                    .all(|&b| decode_plain_digit(b, layout.dialect).is_some())
        }
        SignPlacement::EmbeddedLeading => {
            let (&sign_b, rest) = bytes.split_first().unwrap_or((&0, &[]));
            decode_overpunch_byte(sign_b, layout.dialect).is_some()
                && rest
                    .iter()
                    .all(|&b| decode_plain_digit(b, layout.dialect).is_some())
        }
        SignPlacement::EmbeddedTrailing => {
            let (&sign_b, rest) = bytes.split_last().unwrap_or((&0, &[]));
            decode_overpunch_byte(sign_b, layout.dialect).is_some()
                && rest
                    .iter()
                    .all(|&b| decode_plain_digit(b, layout.dialect).is_some())
        }
    }
}

/// Extracts `(is_negative, ascii_digits)` from `bytes` (guaranteed length `layout.byte_len() >= 1`).
///
/// # Arguments
///
/// * `bytes` - Validated-length byte slice.
/// * `layout` - Zoned layout descriptor.
fn extract_zoned_digits_and_sign(bytes: &[u8], layout: ZonedLayout) -> Result<(bool, Vec<u8>)> {
    let mut ascii_digits = Vec::with_capacity(usize::from(layout.digits.get()));
    match layout.sign {
        SignPlacement::Unsigned => {
            for &b in bytes {
                ascii_digits.push(b'0' + decode_plain_or_space_digit(b, layout.dialect)?);
            }
            Ok((false, ascii_digits))
        }
        SignPlacement::SeparateLeading => {
            let (&sign_b, rest) = bytes.split_first().unwrap_or((&b'+', &[]));
            let neg = decode_separate_sign(sign_b, layout.dialect)?;
            for &b in rest {
                ascii_digits.push(b'0' + decode_plain_or_space_digit(b, layout.dialect)?);
            }
            Ok((neg, ascii_digits))
        }
        SignPlacement::SeparateTrailing => {
            let (&sign_b, rest) = bytes.split_last().unwrap_or((&b'+', &[]));
            let neg = decode_separate_sign(sign_b, layout.dialect)?;
            for &b in rest {
                ascii_digits.push(b'0' + decode_plain_or_space_digit(b, layout.dialect)?);
            }
            Ok((neg, ascii_digits))
        }
        SignPlacement::EmbeddedLeading => {
            let (&sign_b, rest) = bytes.split_first().unwrap_or((&b'0', &[]));
            let (d0, neg) = decode_overpunch_or_err(sign_b, layout.dialect)?;
            ascii_digits.push(b'0' + d0);
            for &b in rest {
                ascii_digits.push(b'0' + decode_plain_or_space_digit(b, layout.dialect)?);
            }
            Ok((neg, ascii_digits))
        }
        SignPlacement::EmbeddedTrailing => {
            let (&sign_b, rest) = bytes.split_last().unwrap_or((&b'0', &[]));
            for &b in rest {
                ascii_digits.push(b'0' + decode_plain_or_space_digit(b, layout.dialect)?);
            }
            let (dn, neg) = decode_overpunch_or_err(sign_b, layout.dialect)?;
            ascii_digits.push(b'0' + dn);
            Ok((neg, ascii_digits))
        }
    }
}

/// Encodes digit `d` (`0..=9`) with sign `is_neg` under `dialect`.
///
/// # Arguments
///
/// * `digit` - Decimal digit in `0..=9`.
/// * `is_neg` - `true` when value is strictly negative (`< 0`).
/// * `dialect` - Overpunch dialect.
#[must_use]
const fn encode_overpunch_byte(digit: u8, is_neg: bool, dialect: OverpunchDialect) -> u8 {
    match dialect {
        OverpunchDialect::AsciiGnu | OverpunchDialect::MicroFocus => {
            if is_neg {
                0x70 + digit
            } else {
                b'0' + digit
            }
        }
        OverpunchDialect::Cobol85Standard => {
            if is_neg {
                if digit == 0 { b'}' } else { b'J' + (digit - 1) }
            } else if digit == 0 {
                b'{'
            } else {
                b'A' + (digit - 1)
            }
        }
        OverpunchDialect::EbcdicIbm => {
            if is_neg {
                0xD0 + digit
            } else {
                0xC0 + digit
            }
        }
    }
}

/// Decodes an overpunched sign byte into `Some((digit, is_negative))` or `None`.
///
/// # Arguments
///
/// * `byte` - Overpunched byte.
/// * `dialect` - Overpunch dialect.
#[must_use]
const fn decode_overpunch_byte(byte: u8, dialect: OverpunchDialect) -> Option<(u8, bool)> {
    match dialect {
        OverpunchDialect::EbcdicIbm => match byte {
            0xC0..=0xC9 => Some((byte - 0xC0, false)),
            0xF0..=0xF9 => Some((byte - 0xF0, false)),
            0xD0..=0xD9 => Some((byte - 0xD0, true)),
            _ => None,
        },
        OverpunchDialect::AsciiGnu
        | OverpunchDialect::Cobol85Standard
        | OverpunchDialect::MicroFocus => match byte {
            b'0'..=b'9' => Some((byte - b'0', false)),
            b'{' => Some((0, false)),
            b'A'..=b'I' => Some((byte - b'A' + 1, false)),
            b'p'..=b'y' => Some((byte - b'p', true)),
            b'}' => Some((0, true)),
            b'J'..=b'R' => Some((byte - b'J' + 1, true)),
            _ => None,
        },
    }
}

/// Wraps [`decode_overpunch_byte`] into a `Result`, treating space as `(0, false)`.
///
/// # Arguments
///
/// * `byte` - Raw overpunch byte.
/// * `dialect` - Overpunch dialect.
fn decode_overpunch_or_err(byte: u8, dialect: OverpunchDialect) -> Result<(u8, bool)> {
    if byte == dialect.space_byte() {
        return Ok((0, false));
    }
    decode_overpunch_byte(byte, dialect).ok_or_else(|| CoblangError::DecimalDomain {
        operation: "decode_zoned",
        message: format!("invalid overpunch sign byte 0x{byte:02X}"),
    })
}

/// Decodes a strict unsigned digit byte (`0..=9`).
///
/// # Arguments
///
/// * `byte` - Raw digit byte.
/// * `dialect` - Overpunch dialect.
#[must_use]
const fn decode_plain_digit(byte: u8, dialect: OverpunchDialect) -> Option<u8> {
    match dialect {
        OverpunchDialect::EbcdicIbm => {
            if byte.wrapping_sub(0xF0) <= 9 {
                Some(byte - 0xF0)
            } else {
                None
            }
        }
        OverpunchDialect::AsciiGnu
        | OverpunchDialect::Cobol85Standard
        | OverpunchDialect::MicroFocus => {
            if byte.is_ascii_digit() {
                Some(byte - b'0')
            } else {
                None
            }
        }
    }
}

/// Decodes a digit byte in arithmetic `decode_zoned`, treating a leading space as `0`.
///
/// # Arguments
///
/// * `byte` - Raw digit byte.
/// * `dialect` - Overpunch dialect.
fn decode_plain_or_space_digit(byte: u8, dialect: OverpunchDialect) -> Result<u8> {
    if byte == dialect.space_byte() {
        return Ok(0);
    }
    decode_plain_digit(byte, dialect).ok_or_else(|| CoblangError::DecimalDomain {
        operation: "decode_zoned",
        message: format!("invalid zoned digit byte 0x{byte:02X}"),
    })
}

/// Checks whether `byte` is `'+'` or `'-'` in `dialect`.
///
/// # Arguments
///
/// * `byte` - Sign character byte.
/// * `dialect` - Overpunch dialect.
#[must_use]
const fn is_valid_separate_sign(byte: u8, dialect: OverpunchDialect) -> bool {
    let (plus, minus) = dialect.separate_sign_bytes();
    byte == plus || byte == minus
}

/// Decodes a separate sign byte into `is_negative` (`true` for `'-'`, `false` for `'+'` or space).
///
/// # Arguments
///
/// * `byte` - Separate sign byte.
/// * `dialect` - Overpunch dialect.
fn decode_separate_sign(byte: u8, dialect: OverpunchDialect) -> Result<bool> {
    let (plus, minus) = dialect.separate_sign_bytes();
    if byte == minus {
        Ok(true)
    } else if byte == plus || byte == dialect.space_byte() {
        Ok(false)
    } else {
        Err(CoblangError::DecimalDomain {
            operation: "decode_zoned",
            message: format!("invalid separate sign byte 0x{byte:02X}"),
        })
    }
}
