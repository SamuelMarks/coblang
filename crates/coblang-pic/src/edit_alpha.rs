//! Alphanumeric, alphabetic, alphanumeric-edited, and UTF-16LE national formatting.

use coblang_hir::{JustifiedSpec, PicEditOp};

/// Formats `src` into a fixed `byte_len` buffer with left or right justification and `pad_byte`.
///
/// # Arguments
///
/// * `src` - Source raw character bytes.
/// * `byte_len` - Destination storage byte length.
/// * `justified` - [`JustifiedSpec::Left`] (right-pad/right-truncate) or [`JustifiedSpec::Right`] (left-pad/left-truncate).
/// * `pad_byte` - Padding byte (`b' '` for ASCII, `0x40` for EBCDIC).
#[must_use]
pub fn format_alphanumeric(
    src: &[u8],
    byte_len: usize,
    justified: JustifiedSpec,
    pad_byte: u8,
) -> Vec<u8> {
    let mut out = vec![pad_byte; byte_len];
    if byte_len == 0 || src.is_empty() {
        return out;
    }
    match justified {
        JustifiedSpec::Left => {
            let copy_len = src.len().min(byte_len);
            out.get_mut(..copy_len)
                .unwrap_or(&mut [])
                .copy_from_slice(src.get(..copy_len).unwrap_or(&[]));
        }
        JustifiedSpec::Right => {
            if src.len() >= byte_len {
                let start = src.len() - byte_len;
                out.copy_from_slice(src.get(start..).unwrap_or(&[]));
            } else {
                let pad_prefix = byte_len - src.len();
                out.get_mut(pad_prefix..)
                    .unwrap_or(&mut [])
                    .copy_from_slice(src);
            }
        }
    }
    out
}

/// Formats `src` through an alphanumeric-edited `PicEditOp` program (`A`, `X`, `9`, `B`, `0`, `/`).
///
/// # Arguments
///
/// * `src` - Source unedited character bytes.
/// * `edit_ops` - Compiled [`PicEditOp`] sequence.
/// * `justified` - [`JustifiedSpec`] controlling source alignment across character slots.
/// * `blank_byte` - Space byte (`b' '` for ASCII).
#[must_use]
pub fn format_alphanumeric_edited(
    src: &[u8],
    edit_ops: &[PicEditOp],
    justified: JustifiedSpec,
    blank_byte: u8,
) -> Vec<u8> {
    let slot_count = edit_ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                PicEditOp::AlphaSlot | PicEditOp::AnyCharSlot | PicEditOp::DigitSlot
            )
        })
        .count();
    let aligned_chars = format_alphanumeric(src, slot_count, justified, blank_byte);

    let mut char_iter = aligned_chars.into_iter();
    let mut out = Vec::with_capacity(edit_ops.len());
    for &op in edit_ops {
        match op {
            PicEditOp::AlphaSlot | PicEditOp::AnyCharSlot | PicEditOp::DigitSlot => {
                out.push(char_iter.next().unwrap_or(blank_byte));
            }
            PicEditOp::InsertBlank => out.push(blank_byte),
            PicEditOp::InsertZero => out.push(b'0'),
            PicEditOp::InsertSlash => out.push(b'/'),
            _ => {}
        }
    }
    out
}

/// Converts an ASCII byte slice into UTF-16LE `USAGE NATIONAL` storage (`2 * char_len` bytes).
///
/// # Arguments
///
/// * `ascii` - Source ASCII bytes.
/// * `char_len` - Target UTF-16 code-unit count.
/// * `justified` - Left vs `JUSTIFIED RIGHT` alignment.
#[must_use]
pub fn ascii_to_national_utf16le(
    ascii: &[u8],
    char_len: usize,
    justified: JustifiedSpec,
) -> Vec<u8> {
    let mut raw_u16 = Vec::with_capacity(ascii.len() * 2);
    for &b in ascii {
        raw_u16.push(b);
        raw_u16.push(0);
    }
    format_national(&raw_u16, char_len, justified)
}

/// Formats UTF-16LE `src_utf16le` bytes into `char_len` UTF-16LE code units (`2 * char_len` bytes), padding with `U+0020`.
///
/// # Arguments
///
/// * `src_utf16le` - Source UTF-16LE byte slice (2 bytes per code unit).
/// * `char_len` - Destination UTF-16 code-unit count.
/// * `justified` - [`JustifiedSpec::Left`] or [`JustifiedSpec::Right`].
#[must_use]
pub fn format_national(src_utf16le: &[u8], char_len: usize, justified: JustifiedSpec) -> Vec<u8> {
    let total_bytes = char_len * 2;
    let mut out = Vec::with_capacity(total_bytes);
    for _ in 0..char_len {
        out.push(0x20);
        out.push(0x00);
    }
    let src_units = src_utf16le.len() / 2;
    if char_len == 0 || src_units == 0 {
        return out;
    }
    let usable_src_bytes = src_units * 2;
    let clean_src = src_utf16le.get(..usable_src_bytes).unwrap_or(&[]);

    match justified {
        JustifiedSpec::Left => {
            let copy_bytes = clean_src.len().min(total_bytes);
            out.get_mut(..copy_bytes)
                .unwrap_or(&mut [])
                .copy_from_slice(clean_src.get(..copy_bytes).unwrap_or(&[]));
        }
        JustifiedSpec::Right => {
            if clean_src.len() >= total_bytes {
                let start = clean_src.len() - total_bytes;
                out.copy_from_slice(clean_src.get(start..).unwrap_or(&[]));
            } else {
                let pad_bytes = total_bytes - clean_src.len();
                out.get_mut(pad_bytes..)
                    .unwrap_or(&mut [])
                    .copy_from_slice(clean_src);
            }
        }
    }
    out
}

/// Formats UTF-16LE `src_utf16le` through a `NATIONAL-EDITED` `PicEditOp` sequence (`N`, `B`, `0`, `/`).
///
/// # Arguments
///
/// * `src_utf16le` - Source UTF-16LE byte slice.
/// * `edit_ops` - Compiled [`PicEditOp`] sequence.
#[must_use]
pub fn format_national_edited(src_utf16le: &[u8], edit_ops: &[PicEditOp]) -> Vec<u8> {
    let slot_count = edit_ops
        .iter()
        .filter(|&&op| op == PicEditOp::NationalSlot)
        .count();
    let aligned = format_national(src_utf16le, slot_count, JustifiedSpec::Left);
    let mut unit_idx = 0_usize;
    let mut out = Vec::with_capacity(edit_ops.len() * 2);
    for &op in edit_ops {
        match op {
            PicEditOp::NationalSlot => {
                let lo = aligned.get(unit_idx * 2).copied().unwrap_or(0x20);
                let hi = aligned.get(unit_idx * 2 + 1).copied().unwrap_or(0x00);
                out.push(lo);
                out.push(hi);
                unit_idx += 1;
            }
            PicEditOp::InsertBlank => {
                out.push(0x20);
                out.push(0x00);
            }
            PicEditOp::InsertZero => {
                out.push(b'0');
                out.push(0x00);
            }
            PicEditOp::InsertSlash => {
                out.push(b'/');
                out.push(0x00);
            }
            _ => {}
        }
    }
    out
}
