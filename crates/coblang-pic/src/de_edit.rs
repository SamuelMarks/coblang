//! Numeric-edited and free-form alphanumeric de-editing into `FixedDecimal`.

use coblang_common::{CoblangError, Result};
use coblang_decimal::{DecimalSign, FixedDecimal, SignedInt256, Uint256};

/// De-edits a numeric-edited or alphanumeric display byte buffer back into an exact `FixedDecimal`.
///
/// Supports leading/trailing `+`/`-`, trailing `CR`/`DB` (case-insensitive), floating or fixed
/// currency symbols, `*` check-protection characters, grouping separators, and `B`/`/` spaces.
///
/// # Arguments
///
/// * `edited_bytes` - Raw display bytes of the edited or alphanumeric source field.
/// * `decimal_char` - Active decimal point byte (`b'.'` normally, `b','` under `DECIMAL-POINT IS COMMA`).
/// * `currency_symbol` - Active currency symbol (`'$'` by default).
///
/// # Errors
///
/// Returns [`CoblangError::PictureCodec`] if unparseable alphabetic characters appear outside `CR`/`DB` or digits exceed 76.
pub fn de_edit_to_decimal(
    edited_bytes: &[u8],
    decimal_char: u8,
    currency_symbol: char,
) -> Result<FixedDecimal> {
    let mut cur_buf = [0_u8; 4];
    let cur_bytes = currency_symbol.encode_utf8(&mut cur_buf).as_bytes();
    de_edit_to_decimal_with_currency(edited_bytes, decimal_char, cur_bytes)
}

/// De-edits a numeric-edited or alphanumeric display byte buffer using an arbitrary single- or
/// multi-byte `CURRENCY SIGN` string (such as `"$"`, `"EUR"`, `"CHF"`, or `"€"`).
///
/// # Arguments
///
/// * `edited_bytes` - Raw display bytes of the edited or alphanumeric source field.
/// * `decimal_char` - Active decimal point byte (`b'.'` normally, `b','` under `DECIMAL-POINT IS COMMA`).
/// * `currency_bytes` - Non-empty active currency string bytes.
///
/// # Errors
///
/// Returns [`CoblangError::PictureCodec`] if `currency_bytes` is empty, invalid characters appear, or digits exceed 76.
pub fn de_edit_to_decimal_with_currency(
    edited_bytes: &[u8],
    decimal_char: u8,
    currency_bytes: &[u8],
) -> Result<FixedDecimal> {
    if currency_bytes.is_empty() {
        return Err(CoblangError::PictureCodec {
            picture: String::from("de-edit"),
            message: String::from("currency sign byte string cannot be empty"),
        });
    }
    let group_sep = if decimal_char == b',' { b'.' } else { b',' };

    // Trim surrounding ASCII spaces and '*' check-protection padding.
    let mut slice = edited_bytes;
    while let Some((&first, rest)) = slice.split_first() {
        if first == b' ' || first == b'*' {
            slice = rest;
        } else {
            break;
        }
    }
    while let Some((&last, rest)) = slice.split_last() {
        if last == b' ' || last == b'*' {
            slice = rest;
        } else {
            break;
        }
    }

    let mut is_neg = false;
    if let [rest @ .., c0, c1] = slice
        && ([*c0, *c1].eq_ignore_ascii_case(b"CR") || [*c0, *c1].eq_ignore_ascii_case(b"DB"))
    {
        is_neg = true;
        slice = rest;
    }

    let mut digits = Vec::with_capacity(slice.len());
    let mut frac_count: Option<i16> = None;
    let cur_len = currency_bytes.len();
    let mut idx = 0_usize;
    while let Some(&b) = slice.get(idx) {
        if slice.get(idx..idx.saturating_add(cur_len)) == Some(currency_bytes) {
            idx = idx.saturating_add(cur_len);
            continue;
        }
        if b.is_ascii_digit() {
            digits.push(b);
            if let Some(fc) = &mut frac_count {
                *fc = fc.saturating_add(1);
            }
        } else if b == decimal_char {
            if frac_count.is_some() {
                return Err(CoblangError::PictureCodec {
                    picture: String::from("de-edit"),
                    message: String::from("multiple decimal points in de-edited field"),
                });
            }
            frac_count = Some(0);
        } else if b == b'-' {
            is_neg = true;
        } else if b == b'+' || b == b' ' || b == b'*' || b == group_sep || b == b'/' {
            if let Some(fc) = &mut frac_count
                && b == b'*'
            {
                digits.push(b'0');
                *fc = fc.saturating_add(1);
            }
        } else {
            return Err(CoblangError::PictureCodec {
                picture: String::from("de-edit"),
                message: format!("invalid byte 0x{b:02X} in numeric de-editing"),
            });
        }
        idx += 1;
    }

    if digits.is_empty() {
        return Ok(FixedDecimal::ZERO);
    }

    let mag = Uint256::from_decimal_digits(&digits).map_err(|e| CoblangError::PictureCodec {
        picture: String::from("de-edit"),
        message: e.to_string(),
    })?;
    let sign = if is_neg && !mag.is_zero() {
        DecimalSign::Negative
    } else {
        DecimalSign::Positive
    };
    let scale = frac_count.unwrap_or(0);
    FixedDecimal::try_new(SignedInt256::from_parts(mag, sign), scale).map_err(|e| {
        CoblangError::PictureCodec {
            picture: String::from("de-edit"),
            message: e.to_string(),
        }
    })
}
