//! Character-indexed `INSPECT`, `STRING`, and `UNSTRING` execution over `NATIONAL` (`UTF-16LE`)
//! and `UTF-8` (`PIC U`) fields (`Milestone M17`).

#![deny(clippy::indexing_slicing)]

use coblang_common::Result;
use coblang_hir::HirType;
use coblang_pic::{
    InspectMatchKind, Justification, MoveFieldDesc, MoveSource, StringFlowOutcome,
    UnstringMatchMode, execute_move, pad_or_truncate_utf8_bytes_lossless,
};

use crate::m10_staging::{
    StagedInspectOperand, StagedReplaceClause, StagedStringDelim, StagedTallyClause,
};

/// Character-unit mode of a `STRING`, `UNSTRING`, or `INSPECT` target/source field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CharUnitMode {
    /// Single-byte ASCII or EBCDIC (`1` byte per character position).
    #[default]
    SingleByte,
    /// `NATIONAL` / `NATIONAL-EDITED` (`2` bytes per UTF-16LE code unit).
    NationalUtf16Le,
    /// `UTF-8` (`PIC U`, variable-width Unicode scalar positions).
    Utf8Scalars,
}

impl CharUnitMode {
    /// Classifies `desc_opt` into its [`CharUnitMode`].
    ///
    /// # Arguments
    ///
    /// * `desc_opt` - Optional [`MoveFieldDesc`] of the subject/source/destination.
    #[must_use]
    pub const fn from_desc(desc_opt: Option<&MoveFieldDesc>) -> Self {
        match desc_opt {
            Some(d) => match d.ty {
                HirType::National { .. } | HirType::NationalEdited { .. } => Self::NationalUtf16Le,
                HirType::Utf8 { .. } => Self::Utf8Scalars,
                _ => Self::SingleByte,
            },
            None => Self::SingleByte,
        }
    }
}

/// Decodes an operand byte slice into Unicode scalar `char` units under `mode`.
///
/// Handles UTF-16LE field bytes (`NationalUtf16Le`), UTF-16BE literals (`[0x00, b'A']` when heuristic
/// or explicit), and UTF-8/ASCII bytes.
///
/// # Arguments
///
/// * `raw` - Raw operand bytes.
/// * `mode` - Active [`CharUnitMode`].
#[must_use]
pub fn bytes_to_unit_chars(raw: &[u8], mode: CharUnitMode) -> Vec<char> {
    match mode {
        CharUnitMode::NationalUtf16Le => decode_utf16le_units_as_chars(raw),
        CharUnitMode::Utf8Scalars | CharUnitMode::SingleByte => {
            String::from_utf8_lossy(raw).chars().collect()
        }
    }
}

/// Decodes UTF-16LE byte pairs into `Vec<char>` (treating unpaired surrogates as `U+FFFD`).
///
/// # Arguments
///
/// * `bytes` - UTF-16LE byte slice.
#[must_use]
pub fn decode_utf16le_units_as_chars(bytes: &[u8]) -> Vec<char> {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    char::decode_utf16(units)
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

/// Encodes `chars` back into UTF-16LE bytes (`2` bytes per BMP unit).
///
/// # Arguments
///
/// * `chars` - Unicode scalar slice.
#[must_use]
pub fn encode_chars_to_utf16le(chars: &[char]) -> Vec<u8> {
    let mut out = Vec::with_capacity(chars.len() * 2);
    let mut buf = [0_u16; 2];
    for &ch in chars {
        for &u in ch.encode_utf16(&mut buf).iter() {
            out.extend_from_slice(&u.to_le_bytes());
        }
    }
    out
}

/// Encodes `chars` into a UTF-8 `Vec<u8>`.
///
/// # Arguments
///
/// * `chars` - Unicode scalar slice.
#[must_use]
pub fn encode_chars_to_utf8(chars: &[char]) -> Vec<u8> {
    let s: String = chars.iter().collect();
    s.into_bytes()
}

/// Materializes a [`StagedInspectOperand`] into `Vec<char>` under `mode`.
///
/// # Arguments
///
/// * `op` - Staged operand.
/// * `target_len` - Target character length (`1` for standalone pattern, or pattern len for `BY`/`TO`).
/// * `mode` - Active [`CharUnitMode`].
#[must_use]
fn materialize_operand_chars(
    op: &StagedInspectOperand,
    target_len: usize,
    mode: CharUnitMode,
) -> Vec<char> {
    match op {
        StagedInspectOperand::FigurativeByte(b) => {
            let ch = char::from(*b);
            vec![ch; target_len.max(1)]
        }
        StagedInspectOperand::Bytes(raw) => decode_operand_auto_chars(raw, mode),
    }
}

/// Decodes an operand byte vector (which may be UTF-16LE from a `PIC N` field, UTF-16BE from `N"..."`,
/// or UTF-8/ASCII) into `Vec<char>`.
///
/// # Arguments
///
/// * `raw` - Raw operand bytes.
/// * `mode` - Subject [`CharUnitMode`].
#[must_use]
pub fn decode_operand_auto_chars(raw: &[u8], mode: CharUnitMode) -> Vec<char> {
    if mode == CharUnitMode::NationalUtf16Le && raw.len().is_multiple_of(2) && !raw.is_empty() {
        // Distinguish UTF-16BE (`[0x00, ascii]`) vs UTF-16LE (`[ascii, 0x00]`) when ASCII BMP.
        let pairs = raw.as_chunks::<2>().0;
        let looks_be = pairs.iter().all(|p| p[0] == 0 && p[1] != 0);
        if looks_be {
            let units: Vec<u16> = pairs.iter().map(|p| u16::from_be_bytes(*p)).collect();
            return char::decode_utf16(units)
                .map(|r| r.unwrap_or('\u{FFFD}'))
                .collect();
        }
        return decode_utf16le_units_as_chars(raw);
    }
    String::from_utf8_lossy(raw).chars().collect()
}

/// Computes `[start..end)` character window bounds inside `subject` for `after` / `before`.
///
/// # Arguments
///
/// * `subject` - Subject character slice.
/// * `after` - Optional `AFTER INITIAL` operand.
/// * `before` - Optional `BEFORE INITIAL` operand.
/// * `mode` - Active [`CharUnitMode`].
#[must_use]
fn resolve_char_window(
    subject: &[char],
    after: Option<&StagedInspectOperand>,
    before: Option<&StagedInspectOperand>,
    mode: CharUnitMode,
) -> (usize, usize) {
    let mut start = 0;
    if let Some(af) = after {
        let pat = materialize_operand_chars(af, 1, mode);
        if pat.is_empty() {
            return (subject.len(), subject.len());
        }
        if let Some(pos) = find_char_subslice(subject, &pat) {
            start = pos + pat.len();
        } else {
            return (subject.len(), subject.len());
        }
    }
    let tail = subject.get(start..).unwrap_or(&[]);
    let mut end = subject.len();
    if let Some(bf) = before {
        let pat = materialize_operand_chars(bf, 1, mode);
        if !pat.is_empty()
            && let Some(rel) = find_char_subslice(tail, &pat)
        {
            end = start + rel;
        }
    }
    (start, end)
}

/// Finds the first 0-based index where `needle` occurs inside `haystack`.
///
/// # Arguments
///
/// * `haystack` - Searched character slice.
/// * `needle` - Non-empty pattern slice.
#[must_use]
fn find_char_subslice(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Executes staged `INSPECT` `TALLYING` and `REPLACING` clauses over `NATIONAL` or `UTF-8` `target`.
///
/// # Arguments
///
/// * `target` - Mutable target field bytes.
/// * `mode` - `NationalUtf16Le` or `Utf8Scalars`.
/// * `tallies` - Staged [`StagedTallyClause`] slice.
/// * `replaces` - Staged [`StagedReplaceClause`] slice.
/// * `deltas` - Mutable counter delta accumulator slice.
pub fn execute_unicode_inspect_tally_replace(
    target: &mut [u8],
    mode: CharUnitMode,
    tallies: &[StagedTallyClause],
    replaces: &[StagedReplaceClause],
    deltas: &mut [i64],
) {
    let mut chars = bytes_to_unit_chars(target, mode);
    if !tallies.is_empty() {
        tally_unicode_chars(&chars, mode, tallies, deltas);
    }
    if !replaces.is_empty() {
        replace_unicode_chars(&mut chars, mode, replaces);
        write_back_unicode_chars(target, &chars, mode);
    }
}

/// Executes `TALLYING` clauses over `chars` following ISO left-to-right scan semantics.
///
/// # Arguments
///
/// * `chars` - Subject character slice.
/// * `mode` - Active [`CharUnitMode`].
/// * `tallies` - Staged tally clauses.
/// * `deltas` - Output counter increments.
fn tally_unicode_chars(
    chars: &[char],
    mode: CharUnitMode,
    tallies: &[StagedTallyClause],
    deltas: &mut [i64],
) {
    let wins: Vec<(usize, usize)> = tallies
        .iter()
        .map(|c| resolve_char_window(chars, c.after.as_ref(), c.before.as_ref(), mode))
        .collect();
    let mut fired_first = vec![false; tallies.len()];
    let mut leading_open: Vec<bool> = tallies
        .iter()
        .map(|c| c.kind == InspectMatchKind::Leading)
        .collect();

    // Pre-pass for TRAILING clauses
    for (ci, clause) in tallies.iter().enumerate() {
        if clause.kind == InspectMatchKind::Trailing {
            let (ws, we) = wins.get(ci).copied().unwrap_or((0, 0));
            let pat = materialize_operand_chars(&clause.pattern, 1, mode);
            if !pat.is_empty() && we > ws {
                let mut cur_end = we;
                while cur_end >= ws + pat.len()
                    && chars.get(cur_end - pat.len()..cur_end) == Some(pat.as_slice())
                {
                    if let Some(d) = deltas.get_mut(clause.counter_slot) {
                        *d += 1;
                    }
                    cur_end -= pat.len();
                }
            }
        }
    }

    let mut pos = 0;
    while pos < chars.len() {
        let mut step = 1;
        for (ci, clause) in tallies.iter().enumerate() {
            let (ws, we) = wins.get(ci).copied().unwrap_or((0, 0));
            if pos < ws || pos >= we {
                continue;
            }
            match clause.kind {
                InspectMatchKind::Trailing => {}
                InspectMatchKind::Characters => {
                    if let Some(d) = deltas.get_mut(clause.counter_slot) {
                        *d += 1;
                    }
                    step = 1;
                    break;
                }
                InspectMatchKind::All | InspectMatchKind::First | InspectMatchKind::Leading => {
                    if clause.kind == InspectMatchKind::First
                        && fired_first.get(ci).copied().unwrap_or(false)
                    {
                        continue;
                    }
                    if clause.kind == InspectMatchKind::Leading
                        && !leading_open.get(ci).copied().unwrap_or(false)
                    {
                        continue;
                    }
                    let pat = materialize_operand_chars(&clause.pattern, 1, mode);
                    if !pat.is_empty()
                        && pos + pat.len() <= we
                        && chars.get(pos..pos + pat.len()) == Some(pat.as_slice())
                    {
                        if let Some(d) = deltas.get_mut(clause.counter_slot) {
                            *d += 1;
                        }
                        if clause.kind == InspectMatchKind::First
                            && let Some(ff) = fired_first.get_mut(ci)
                        {
                            *ff = true;
                        }
                        step = pat.len();
                        break;
                    } else if clause.kind == InspectMatchKind::Leading
                        && let Some(lo) = leading_open.get_mut(ci)
                    {
                        *lo = false;
                    }
                }
            }
        }
        pos += step;
    }
}

/// Executes `REPLACING` clauses in-place over `chars`.
///
/// # Arguments
///
/// * `chars` - Mutable subject character vector.
/// * `mode` - Active [`CharUnitMode`].
/// * `replaces` - Staged replace clauses.
fn replace_unicode_chars(chars: &mut [char], mode: CharUnitMode, replaces: &[StagedReplaceClause]) {
    let wins: Vec<(usize, usize)> = replaces
        .iter()
        .map(|c| resolve_char_window(chars, c.after.as_ref(), c.before.as_ref(), mode))
        .collect();
    let mut fired_first = vec![false; replaces.len()];
    let mut leading_open: Vec<bool> = replaces
        .iter()
        .map(|c| c.kind == InspectMatchKind::Leading)
        .collect();

    for (ci, clause) in replaces.iter().enumerate() {
        if clause.kind == InspectMatchKind::Trailing {
            let (ws, we) = wins.get(ci).copied().unwrap_or((0, 0));
            let pat = materialize_operand_chars(&clause.pattern, 1, mode);
            let rep = materialize_operand_chars(&clause.by, pat.len().max(1), mode);
            if !pat.is_empty() && rep.len() >= pat.len() && we > ws {
                let mut cur_end = we;
                while cur_end >= ws + pat.len()
                    && chars.get(cur_end - pat.len()..cur_end) == Some(pat.as_slice())
                {
                    if let (Some(dst_s), Some(rep_s)) = (
                        chars.get_mut(cur_end - pat.len()..cur_end),
                        rep.get(..pat.len()),
                    ) {
                        dst_s.copy_from_slice(rep_s);
                    }
                    cur_end -= pat.len();
                }
            }
        }
    }

    let mut pos = 0;
    while pos < chars.len() {
        let mut step = 1;
        for (ci, clause) in replaces.iter().enumerate() {
            let (ws, we) = wins.get(ci).copied().unwrap_or((0, 0));
            if pos < ws || pos >= we {
                continue;
            }
            match clause.kind {
                InspectMatchKind::Trailing => {}
                InspectMatchKind::Characters => {
                    let rep = materialize_operand_chars(&clause.by, 1, mode);
                    if let (Some(slot), Some(&ch)) = (chars.get_mut(pos), rep.first()) {
                        *slot = ch;
                    }
                    step = 1;
                    break;
                }
                InspectMatchKind::All | InspectMatchKind::First | InspectMatchKind::Leading => {
                    if clause.kind == InspectMatchKind::First
                        && fired_first.get(ci).copied().unwrap_or(false)
                    {
                        continue;
                    }
                    if clause.kind == InspectMatchKind::Leading
                        && !leading_open.get(ci).copied().unwrap_or(false)
                    {
                        continue;
                    }
                    let pat = materialize_operand_chars(&clause.pattern, 1, mode);
                    let rep = materialize_operand_chars(&clause.by, pat.len().max(1), mode);
                    if !pat.is_empty()
                        && rep.len() >= pat.len()
                        && pos + pat.len() <= we
                        && chars.get(pos..pos + pat.len()) == Some(pat.as_slice())
                    {
                        if let (Some(dst_s), Some(rep_s)) =
                            (chars.get_mut(pos..pos + pat.len()), rep.get(..pat.len()))
                        {
                            dst_s.copy_from_slice(rep_s);
                        }
                        if clause.kind == InspectMatchKind::First
                            && let Some(ff) = fired_first.get_mut(ci)
                        {
                            *ff = true;
                        }
                        step = pat.len();
                        break;
                    } else if clause.kind == InspectMatchKind::Leading
                        && let Some(lo) = leading_open.get_mut(ci)
                    {
                        *lo = false;
                    }
                }
            }
        }
        pos += step;
    }
}

/// Executes `INSPECT ... CONVERTING` over `NATIONAL` or `UTF-8` `target`.
///
/// # Arguments
///
/// * `target` - Mutable target field bytes.
/// * `mode` - `NationalUtf16Le` or `Utf8Scalars`.
/// * `from` - `FROM` character map operand.
/// * `to` - `TO` character map operand.
/// * `bounds` - Optional `(after, before)` window bounds.
pub fn execute_unicode_inspect_converting(
    target: &mut [u8],
    mode: CharUnitMode,
    from: &StagedInspectOperand,
    to: &StagedInspectOperand,
    bounds: (Option<&StagedInspectOperand>, Option<&StagedInspectOperand>),
) {
    let mut chars = bytes_to_unit_chars(target, mode);
    let (ws, we) = resolve_char_window(&chars, bounds.0, bounds.1, mode);
    let from_c = materialize_operand_chars(from, 1, mode);
    let to_c = materialize_operand_chars(to, from_c.len().max(1), mode);
    if let Some(win) = chars.get_mut(ws..we) {
        for ch in win {
            if let Some(idx) = from_c.iter().position(|&fc| fc == *ch)
                && let Some(&mapped) = to_c.get(idx)
            {
                *ch = mapped;
            }
        }
    }
    write_back_unicode_chars(target, &chars, mode);
}

/// Writes `chars` back into `target` according to `mode` (`UTF-16LE` or lossless `UTF-8` space-padded).
///
/// # Arguments
///
/// * `target` - Mutable target byte buffer.
/// * `chars` - Updated Unicode scalar slice.
/// * `mode` - `NationalUtf16Le` or `Utf8Scalars`.
fn write_back_unicode_chars(target: &mut [u8], chars: &[char], mode: CharUnitMode) {
    match mode {
        CharUnitMode::NationalUtf16Le => {
            let encoded = encode_chars_to_utf16le(chars);
            for pair in target.as_chunks_mut::<2>().0 {
                *pair = [b' ', 0x00];
            }
            let n = target.len().min(encoded.len());
            target
                .get_mut(..n)
                .unwrap_or(&mut [])
                .copy_from_slice(encoded.get(..n).unwrap_or(&[]));
        }
        CharUnitMode::Utf8Scalars | CharUnitMode::SingleByte => {
            let raw = encode_chars_to_utf8(chars);
            let padded =
                pad_or_truncate_utf8_bytes_lossless(&raw, target.len(), Justification::Left);
            target.copy_from_slice(&padded);
        }
    }
}

/// Executes character-indexed `STRING` into a `NATIONAL` or `UTF-8` destination.
///
/// Updates `*ptr_one_based` in character positions (`1..=capacity+1`).
///
/// # Arguments
///
/// * `pieces` - Staged `(raw_bytes, StagedStringDelim)` slice.
/// * `dst` - Mutable destination byte slice.
/// * `mode` - `NationalUtf16Le` or `Utf8Scalars`.
/// * `ptr_one_based` - In/out 1-based character pointer.
#[must_use]
pub fn execute_unicode_string(
    pieces: &[(Vec<u8>, StagedStringDelim)],
    dst: &mut [u8],
    mode: CharUnitMode,
    ptr_one_based: &mut i64,
) -> StringFlowOutcome {
    let mut dst_chars = bytes_to_unit_chars(dst, mode);
    let cap = i64::try_from(dst_chars.len()).unwrap_or(0);
    if *ptr_one_based < 1 || *ptr_one_based > cap {
        return StringFlowOutcome::Overflowed;
    }
    let mut overflow = StringFlowOutcome::Completed;
    'outer: for (src_raw, delim) in pieces {
        let src_c = decode_operand_auto_chars(src_raw, mode);
        let eff: &[char] = match delim {
            StagedStringDelim::Size => &src_c,
            StagedStringDelim::FigurativeByte(b) => {
                let dch = char::from(*b);
                let stop = src_c.iter().position(|&c| c == dch).unwrap_or(src_c.len());
                src_c.get(..stop).unwrap_or(&[])
            }
            StagedStringDelim::Pattern(pat_raw) => {
                let pat_c = decode_operand_auto_chars(pat_raw, mode);
                let stop = find_char_subslice(&src_c, &pat_c).unwrap_or(src_c.len());
                src_c.get(..stop).unwrap_or(&[])
            }
        };
        for &ch in eff {
            if *ptr_one_based > cap {
                overflow = StringFlowOutcome::Overflowed;
                break 'outer;
            }
            let idx = usize::try_from(*ptr_one_based - 1).unwrap_or(0);
            if let Some(slot) = dst_chars.get_mut(idx) {
                *slot = ch;
            }
            *ptr_one_based += 1;
        }
    }
    write_back_unicode_chars(dst, &dst_chars, mode);
    overflow
}

/// Advances one character-indexed `UNSTRING` step over `src_chars` (`NATIONAL` or `UTF-8`),
/// storing into `into_buf`, optional `delim_in`, and returning extracted character count.
///
/// # Arguments
///
/// * `src_chars` - Decoded Unicode scalar vector of the `UNSTRING` source.
/// * `delims` - `(UnstringMatchMode, decoded_pattern_chars)` delimiter rules.
/// * `ptr_one_based` - In/out 1-based character cursor.
/// * `into_pair` - `(into_buf, into_desc)` target slot.
/// * `delim_pair` - Optional `(delim_buf, delim_desc)` delimiter output slot.
///
/// # Errors
///
/// Returns an error if `execute_move` into `into_buf` or `delim_buf` fails.
pub fn step_unicode_unstring(
    src_chars: &[char],
    delims: &[(UnstringMatchMode, Vec<char>)],
    ptr_one_based: &mut i64,
    into_pair: (&mut [u8], &MoveFieldDesc),
    delim_pair: Option<(&mut [u8], &MoveFieldDesc)>,
) -> Result<i64> {
    let (into_buf, into_desc) = into_pair;
    let start = usize::try_from((*ptr_one_based - 1).max(0)).unwrap_or(src_chars.len());
    let tail = src_chars.get(start..).unwrap_or(&[]);

    if delims.is_empty() {
        let cap = match into_desc.ty {
            HirType::National { char_len, .. } | HirType::NationalEdited { char_len, .. } => {
                usize::try_from(char_len).unwrap_or(tail.len())
            }
            _ => into_buf.len(),
        };
        let take = tail.len().min(cap);
        let chunk = tail.get(..take).unwrap_or(&[]);
        let chunk_u8 = encode_chars_to_utf8(chunk);
        execute_move(MoveSource::Utf8Literal(&chunk_u8), into_buf, into_desc)?;
        if let Some((dbuf, ddesc)) = delim_pair {
            execute_move(MoveSource::Utf8Literal(b""), dbuf, ddesc)?;
        }
        let n_i64 = i64::try_from(take).unwrap_or(0);
        *ptr_one_based += n_i64;
        return Ok(n_i64);
    }

    // Find earliest leftmost delimiter match in `tail`
    let mut best: Option<(usize, UnstringMatchMode, Vec<char>)> = None;
    for (d_mode, pat) in delims {
        if !pat.is_empty()
            && let Some(rel) = find_char_subslice(tail, pat)
            && best.as_ref().is_none_or(|(b_rel, _, _)| rel < *b_rel)
        {
            best = Some((rel, *d_mode, pat.clone()));
        }
    }

    match best {
        None => {
            let chunk_u8 = encode_chars_to_utf8(tail);
            execute_move(MoveSource::Utf8Literal(&chunk_u8), into_buf, into_desc)?;
            if let Some((dbuf, ddesc)) = delim_pair {
                execute_move(MoveSource::Utf8Literal(b""), dbuf, ddesc)?;
            }
            let count = i64::try_from(tail.len()).unwrap_or(0);
            *ptr_one_based = i64::try_from(src_chars.len()).unwrap_or(0) + 1;
            Ok(count)
        }
        Some((rel, d_mode, pat)) => {
            let chunk = tail.get(..rel).unwrap_or(&[]);
            let chunk_u8 = encode_chars_to_utf8(chunk);
            execute_move(MoveSource::Utf8Literal(&chunk_u8), into_buf, into_desc)?;
            let mut consume = pat.len();
            if d_mode == UnstringMatchMode::AllRepeated {
                while tail.get(rel + consume..rel + consume + pat.len()) == Some(pat.as_slice()) {
                    consume += pat.len();
                }
            }
            if let Some((dbuf, ddesc)) = delim_pair {
                let pat_u8 = encode_chars_to_utf8(&pat);
                execute_move(MoveSource::Utf8Literal(&pat_u8), dbuf, ddesc)?;
            }
            let count = i64::try_from(chunk.len()).unwrap_or(0);
            *ptr_one_based += i64::try_from(rel + consume).unwrap_or(0);
            Ok(count)
        }
    }
}
