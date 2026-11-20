//! Pure-Rust ISO COBOL `STRING` and `UNSTRING` execution engines (`Milestone M10`).

use coblang_common::Result;
use coblang_decimal::OverpunchDialect;
use coblang_hir::{DataCategory, HirFigurative, HirType, NumericSignSpec};

use crate::move_engine::{MoveFieldDesc, MoveSource, execute_move};

/// Completion status of a `STRING` or `UNSTRING` statement (`ON OVERFLOW` vs `NOT ON OVERFLOW`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StringFlowOutcome {
    /// Statement completed without triggering `OVERFLOW`.
    Completed,
    /// `POINTER` started out of bounds or receiving capacity was exhausted before completion.
    Overflowed,
}

/// Delimiter rule governing a `STRING` source piece (`DELIMITED BY SIZE` vs pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringDelimiter<'a> {
    /// Transfer the entire source operand (`DELIMITED BY SIZE`).
    Size,
    /// Transfer bytes strictly preceding the first match of the pattern slice.
    Pattern(&'a [u8]),
    /// Transfer bytes strictly preceding the first occurrence of a 1-byte figurative constant.
    FigurativeByte(u8),
}

/// Resolved source operand and delimiter for [`execute_string`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StringSourcePiece<'a> {
    /// Plain character bytes of the source operand (embedded zoned signs pre-stripped).
    pub bytes: &'a [u8],
    /// Delimiter rule terminating transfer from `bytes`.
    pub delimiter: StringDelimiter<'a>,
}

/// Repetition policy on an `UNSTRING` delimiter (`Single` vs `ALL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnstringMatchMode {
    /// Match a single delimiter occurrence.
    Single,
    /// Collapse contiguous repeated occurrences (`DELIMITED BY ALL ...`).
    AllRepeated,
}

/// Delimiter pattern specification inside [`execute_unstring`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnstringDelimSpec<'a> {
    /// Single vs `ALL` repeated match mode.
    pub mode: UnstringMatchMode,
    /// Non-empty delimiter pattern bytes (figurative constants pre-expanded to 1 byte).
    pub pattern: &'a [u8],
}

/// Destination receiver slot for a single `INTO` target in [`execute_unstring`].
#[derive(Debug)]
pub struct UnstringIntoSlot<'a> {
    /// Backing storage slice of the `INTO` target field.
    pub into_buf: &'a mut [u8],
    /// Field descriptor of the `INTO` target.
    pub into_desc: &'a MoveFieldDesc,
    /// Optional `(storage_slice, descriptor)` for `DELIMITER IN`.
    pub delim_in: Option<(&'a mut [u8], &'a MoveFieldDesc)>,
    /// Optional character-count output for `COUNT IN`.
    pub count_out: Option<&'a mut i64>,
}

impl<'a> UnstringIntoSlot<'a> {
    /// Constructs an [`UnstringIntoSlot`] without `DELIMITER IN` or `COUNT IN`.
    ///
    /// # Arguments
    ///
    /// * `into_buf` - Mutable target storage slice.
    /// * `into_desc` - Target [`MoveFieldDesc`].
    #[must_use]
    pub const fn simple(into_buf: &'a mut [u8], into_desc: &'a MoveFieldDesc) -> Self {
        Self {
            into_buf,
            into_desc,
            delim_in: None,
            count_out: None,
        }
    }
}

/// Strips an embedded overpunched sign nibble from `raw` into plain digit characters if `desc`
/// describes a `ZonedDecimal` with `EmbeddedLeading` or `EmbeddedTrailing` sign.
///
/// # Arguments
///
/// * `raw` - Raw storage byte slice of the operand.
/// * `desc` - Optional [`MoveFieldDesc`] when the operand is a typed field.
#[must_use]
pub fn normalize_display_operand_bytes(raw: &[u8], desc: Option<&MoveFieldDesc>) -> Vec<u8> {
    let mut out = raw.to_vec();
    if let Some(d) = desc
        && let HirType::ZonedDecimal { sign, .. } = d.ty
    {
        match sign {
            NumericSignSpec::EmbeddedLeading => {
                let _ = out
                    .first_mut()
                    .map(|first| *first = strip_overpunch_digit(*first, d.overpunch).0);
            }
            NumericSignSpec::EmbeddedTrailing => {
                let _ = out
                    .last_mut()
                    .map(|last| *last = strip_overpunch_digit(*last, d.overpunch).0);
            }
            NumericSignSpec::Unsigned
            | NumericSignSpec::SeparateLeading
            | NumericSignSpec::SeparateTrailing => {}
        }
    }
    out
}

/// Decodes an overpunched zoned byte into `(plain_digit_byte, OverpunchSignTag)`.
///
/// # Arguments
///
/// * `byte` - Raw storage byte at the leading or trailing zoned sign position.
/// * `dialect` - Active [`OverpunchDialect`].
#[must_use]
pub(crate) const fn strip_overpunch_digit(
    byte: u8,
    dialect: OverpunchDialect,
) -> (u8, OverpunchSignTag) {
    match dialect {
        OverpunchDialect::AsciiGnu
        | OverpunchDialect::MicroFocus
        | OverpunchDialect::Cobol85Standard => match byte {
            0x70..=0x79 => (b'0' + (byte - 0x70), OverpunchSignTag::Negative),
            b'}' => (b'0', OverpunchSignTag::Negative),
            b'J'..=b'R' => (b'1' + (byte - b'J'), OverpunchSignTag::Negative),
            b'{' => (b'0', OverpunchSignTag::PositiveExplicit),
            b'A'..=b'I' => (b'1' + (byte - b'A'), OverpunchSignTag::PositiveExplicit),
            _ => (byte, OverpunchSignTag::Plain),
        },
        OverpunchDialect::EbcdicIbm => match byte {
            0xD0..=0xD9 => (0xF0 + (byte - 0xD0), OverpunchSignTag::Negative),
            0xC0..=0xC9 => (0xF0 + (byte - 0xC0), OverpunchSignTag::PositiveExplicit),
            _ => (byte, OverpunchSignTag::Plain),
        },
    }
}

/// Preserved operational sign classification of an embedded zoned byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverpunchSignTag {
    /// Unsigned/plain digit byte.
    Plain,
    /// Explicit positive overpunch (`'{'`/`'A'..'I'` or EBCDIC `0xC0..0xC9`).
    PositiveExplicit,
    /// Negative overpunch (`'p'..'y'`, `'}'`/`'J'..'R'`, or EBCDIC `0xD0..0xD9`).
    Negative,
}

/// Re-applies `tag` onto a plain digit byte `byte` under `dialect` when `byte` is still a valid digit.
///
/// # Arguments
///
/// * `byte` - Resulting byte at the zoned sign position after `INSPECT` modification.
/// * `tag` - Original [`OverpunchSignTag`] captured prior to `INSPECT`.
/// * `dialect` - Active [`OverpunchDialect`].
#[must_use]
pub(crate) const fn reapply_overpunch_digit(
    byte: u8,
    tag: OverpunchSignTag,
    dialect: OverpunchDialect,
) -> u8 {
    match (dialect, tag) {
        (_, OverpunchSignTag::Plain) => byte,
        (OverpunchDialect::EbcdicIbm, OverpunchSignTag::Negative) => match byte {
            0xF0..=0xF9 => 0xD0 + (byte - 0xF0),
            _ => byte,
        },
        (OverpunchDialect::EbcdicIbm, OverpunchSignTag::PositiveExplicit) => match byte {
            0xF0..=0xF9 => 0xC0 + (byte - 0xF0),
            _ => byte,
        },
        (OverpunchDialect::AsciiGnu | OverpunchDialect::MicroFocus, OverpunchSignTag::Negative) => {
            match byte {
                b'0'..=b'9' => 0x70 + (byte - b'0'),
                _ => byte,
            }
        }
        (OverpunchDialect::Cobol85Standard, OverpunchSignTag::Negative) => match byte {
            b'0' => b'}',
            b'1'..=b'9' => b'J' + (byte - b'1'),
            _ => byte,
        },
        (
            OverpunchDialect::AsciiGnu
            | OverpunchDialect::MicroFocus
            | OverpunchDialect::Cobol85Standard,
            OverpunchSignTag::PositiveExplicit,
        ) => match byte {
            b'0' => b'{',
            b'1'..=b'9' => b'A' + (byte - b'1'),
            _ => byte,
        },
    }
}

/// Executes an ISO COBOL `STRING` statement into `dst`, updating 1-based `pointer_one_based`.
///
/// Unwritten positions in `dst` are left untouched per ISO/IEC 1989:2023 `STRING` General Rule 7.
///
/// # Arguments
///
/// * `pieces` - Ordered source operands with their active delimiters.
/// * `dst` - Mutable destination byte slice (`INTO` target).
/// * `pointer_one_based` - 1-based write cursor (`1` when `WITH POINTER` is omitted).
#[must_use]
pub fn execute_string(
    pieces: &[StringSourcePiece<'_>],
    dst: &mut [u8],
    pointer_one_based: &mut i64,
) -> StringFlowOutcome {
    let dst_len = i64::try_from(dst.len()).unwrap_or(i64::MAX);
    if *pointer_one_based < 1 || *pointer_one_based > dst_len {
        return StringFlowOutcome::Overflowed;
    }

    for piece in pieces {
        let prefix = slice_before_delimiter(piece.bytes, piece.delimiter);
        for &ch in prefix {
            if *pointer_one_based > dst_len {
                return StringFlowOutcome::Overflowed;
            }
            let zero_idx = usize::try_from(*pointer_one_based - 1).unwrap_or(usize::MAX);
            let _ = dst.get_mut(zero_idx).map(|slot| *slot = ch);
            *pointer_one_based += 1;
        }
    }
    StringFlowOutcome::Completed
}

/// Returns the prefix of `bytes` preceding the first match of `delim`.
///
/// # Arguments
///
/// * `bytes` - Source operand bytes.
/// * `delim` - Active [`StringDelimiter`].
#[must_use]
fn slice_before_delimiter<'a>(bytes: &'a [u8], delim: StringDelimiter<'_>) -> &'a [u8] {
    match delim {
        StringDelimiter::Size => bytes,
        StringDelimiter::FigurativeByte(target_b) => {
            let cut = bytes
                .iter()
                .position(|&b| b == target_b)
                .unwrap_or(bytes.len());
            bytes.get(..cut).unwrap_or(bytes)
        }
        StringDelimiter::Pattern(pat) => {
            if pat.is_empty() {
                return bytes;
            }
            let cut = find_subslice(bytes, pat).unwrap_or(bytes.len());
            bytes.get(..cut).unwrap_or(bytes)
        }
    }
}

/// Finds the first byte index in `haystack` where `needle` (non-empty) starts.
///
/// # Arguments
///
/// * `haystack` - Searched byte slice.
/// * `needle` - Non-empty pattern slice.
#[must_use]
pub(crate) fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|win| win == needle)
}

/// Executes an ISO COBOL `UNSTRING` statement from `src` into `targets`.
///
/// # Arguments
///
/// * `src` - Normalized source character bytes.
/// * `delimiters` - Active `DELIMITED BY` specifications (empty when `DELIMITED BY` is omitted).
/// * `targets` - Ordered `INTO` target slots (`DELIMITER IN`, `COUNT IN`).
/// * `pointer_one_based` - 1-based scan cursor (`1` when `WITH POINTER` is omitted).
/// * `tally_delta` - Output count of receiving fields acted upon (to be added to `TALLYING IN`).
///
/// # Errors
///
/// Returns an error if moving an extracted substring into a receiving field fails codec bounds.
pub fn execute_unstring(
    src: &[u8],
    delimiters: &[UnstringDelimSpec<'_>],
    targets: &mut [UnstringIntoSlot<'_>],
    pointer_one_based: &mut i64,
    tally_delta: &mut i64,
) -> Result<StringFlowOutcome> {
    *tally_delta = 0;
    let src_len_i64 = i64::try_from(src.len()).unwrap_or(i64::MAX);
    if *pointer_one_based < 1 || *pointer_one_based > src_len_i64 {
        return Ok(StringFlowOutcome::Overflowed);
    }

    let mut pos = usize::try_from(*pointer_one_based - 1).unwrap_or(0);
    if delimiters.is_empty() {
        for slot in targets {
            if pos >= src.len() {
                break;
            }
            let recv_cap = slot.into_buf.len().max(1);
            let end = pos.saturating_add(recv_cap).min(src.len());
            let chunk = src.get(pos..end).unwrap_or(&[]);
            store_unstring_slot(slot, chunk, b"")?;
            pos = end;
            *tally_delta += 1;
        }
    } else {
        for slot in targets {
            if pos >= src.len() {
                break;
            }
            let (token, matched_delim, next_pos) = scan_next_unstring_token(src, pos, delimiters);
            store_unstring_slot(slot, token, matched_delim)?;
            pos = next_pos;
            *tally_delta += 1;
        }
    }

    *pointer_one_based = i64::try_from(pos).unwrap_or(0).saturating_add(1);
    if pos < src.len() {
        Ok(StringFlowOutcome::Overflowed)
    } else {
        Ok(StringFlowOutcome::Completed)
    }
}

/// Scans `src[start..]` for the earliest delimiter in `delimiters` (breaking ties by written order).
///
/// Returns `(token_slice, single_delimiter_slice, next_cursor_pos)`.
///
/// # Arguments
///
/// * `src` - Full source byte slice.
/// * `start` - 0-based start offset (`start < src.len()`).
/// * `delimiters` - Non-empty slice of [`UnstringDelimSpec`].
#[must_use]
fn scan_next_unstring_token<'a>(
    src: &'a [u8],
    start: usize,
    delimiters: &[UnstringDelimSpec<'_>],
) -> (&'a [u8], &'a [u8], usize) {
    let rem = src.get(start..).unwrap_or(&[]);
    let mut best: Option<(usize, usize, UnstringMatchMode)> = None;
    for spec in delimiters {
        if spec.pattern.is_empty() {
            continue;
        }
        if let Some(rel_idx) = find_subslice(rem, spec.pattern) {
            let replace = match best {
                None => true,
                Some((best_idx, _, _)) => rel_idx < best_idx,
            };
            if replace {
                best = Some((rel_idx, spec.pattern.len(), spec.mode));
            }
        }
    }

    let Some((rel_idx, pat_len, mode)) = best else {
        return (rem, b"", src.len());
    };

    let tok = rem.get(..rel_idx).unwrap_or(&[]);
    let single_delim = rem.get(rel_idx..rel_idx + pat_len).unwrap_or(&[]);
    let mut after_rel = rel_idx + pat_len;
    if mode == UnstringMatchMode::AllRepeated {
        while rem
            .get(after_rel..after_rel + pat_len)
            .is_some_and(|w| w == single_delim)
        {
            after_rel += pat_len;
        }
    }
    (tok, single_delim, start + after_rel)
}

/// Populates one [`UnstringIntoSlot`] (`INTO`, `DELIMITER IN`, and `COUNT IN`).
///
/// # Arguments
///
/// * `slot` - Destination slot.
/// * `token` - Extracted substring bytes.
/// * `delim` - Matched delimiter bytes (`b""` when end-of-string terminated the token).
fn store_unstring_slot(slot: &mut UnstringIntoSlot<'_>, token: &[u8], delim: &[u8]) -> Result<()> {
    let cat = slot.into_desc.category();
    let move_src =
        if token.is_empty() && matches!(cat, DataCategory::Numeric | DataCategory::NumericEdited) {
            MoveSource::Figurative(HirFigurative::Zero)
        } else {
            MoveSource::AlphanumericLiteral(token)
        };
    execute_move(move_src, slot.into_buf, slot.into_desc)?;

    if let Some((d_buf, d_desc)) = slot.delim_in.as_mut() {
        execute_move(MoveSource::AlphanumericLiteral(delim), d_buf, d_desc)?;
    }
    if let Some(cnt) = slot.count_out.as_mut() {
        **cnt = i64::try_from(token.len()).unwrap_or(0);
    }
    Ok(())
}
