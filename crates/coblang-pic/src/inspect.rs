//! Pure-Rust ISO COBOL `INSPECT` (`TALLYING`, `REPLACING`, `CONVERTING`) and `TRANSFORM` engine (`Milestone M10`).

use coblang_decimal::OverpunchDialect;
use coblang_hir::{HirType, NumericSignSpec};

use crate::move_engine::MoveFieldDesc;
use crate::string_verbs::{
    OverpunchSignTag, find_subslice, reapply_overpunch_digit, strip_overpunch_digit,
};

/// Match mode for an `INSPECT` `TALLYING` or `REPLACING` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InspectMatchKind {
    /// `CHARACTERS` (matches 1 character position at a time).
    Characters,
    /// `ALL pattern`.
    All,
    /// `LEADING pattern` (contiguous occurrences starting at `win_start`).
    Leading,
    /// `FIRST pattern` (at most once within the clause window).
    First,
    /// `TRAILING pattern` (contiguous occurrences ending at `win_end`).
    Trailing,
}

/// Pattern or replacement operand supplied to `INSPECT` / `TRANSFORM`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectOperand<'a> {
    /// Explicit character slice (field or string literal).
    Bytes(&'a [u8]),
    /// Single-byte figurative constant (`SPACE`, `ZERO`, `LOW-VALUE`, `HIGH-VALUE`, `QUOTE`, `NULL`).
    FigurativeByte(u8),
}

impl InspectOperand<'_> {
    /// Materializes `self` to match `target_len` when `self` is a figurative constant,
    /// or returns its literal bytes directly (`1` byte when `target_len == 0`).
    ///
    /// # Arguments
    ///
    /// * `target_len` - Desired byte width (`1` for standalone pattern matching, or `pat.len()` for `BY`/`TO`).
    #[must_use]
    pub fn materialize_for_len(self, target_len: usize) -> Vec<u8> {
        match self {
            Self::Bytes(slice) => slice.to_vec(),
            Self::FigurativeByte(b) => vec![b; target_len.max(1)],
        }
    }
}

/// Optional `BEFORE INITIAL` / `AFTER INITIAL` window specification on an `INSPECT` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InspectWindowSpec<'a> {
    /// Optional `AFTER INITIAL` pattern operand.
    pub after: Option<InspectOperand<'a>>,
    /// Optional `BEFORE INITIAL` pattern operand.
    pub before: Option<InspectOperand<'a>>,
}

/// Single `TALLYING` sub-clause inside [`execute_inspect_tallying`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InspectTallyClause<'a> {
    /// Index into the caller's `deltas` accumulator slice.
    pub counter_slot: usize,
    /// Match mode (`Characters`, `All`, `Leading`, `First`, `Trailing`).
    pub kind: InspectMatchKind,
    /// Pattern operand (ignored when `kind == InspectMatchKind::Characters`).
    pub pattern: InspectOperand<'a>,
    /// `BEFORE` / `AFTER` window bounds.
    pub window: InspectWindowSpec<'a>,
}

/// Single `REPLACING` sub-clause inside [`execute_inspect_replacing`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InspectReplaceClause<'a> {
    /// Match mode (`Characters`, `All`, `Leading`, `First`, `Trailing`).
    pub kind: InspectMatchKind,
    /// Pattern operand (ignored when `kind == InspectMatchKind::Characters`).
    pub pattern: InspectOperand<'a>,
    /// Replacement operand (`BY ...`).
    pub by: InspectOperand<'a>,
    /// `BEFORE` / `AFTER` window bounds.
    pub window: InspectWindowSpec<'a>,
}

/// `CONVERTING from TO to [BEFORE ...] [AFTER ...]` specification (also used for `TRANSFORM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InspectConvertSpec<'a> {
    /// Source character alphabet (`FROM`).
    pub from: InspectOperand<'a>,
    /// Replacement character alphabet (`TO`).
    pub to: InspectOperand<'a>,
    /// `BEFORE` / `AFTER` window bounds.
    pub window: InspectWindowSpec<'a>,
}

/// Precomputed active window and `LEADING`/`TRAILING` boundaries for one `INSPECT` clause.
#[derive(Debug, Clone)]
struct PreparedClause {
    /// Inclusive window start byte index.
    win_start: usize,
    /// Exclusive window end byte index.
    win_end: usize,
    /// Clause match mode.
    kind: InspectMatchKind,
    /// Materialized pattern bytes (empty for `Characters`).
    pat: Vec<u8>,
    /// Exclusive end of contiguous `LEADING` occurrences starting at `win_start`.
    lead_end: usize,
    /// Inclusive start of contiguous `TRAILING` occurrences ending at `win_end`.
    trail_start: usize,
    /// Whether a `FIRST` clause has already fired.
    first_used: bool,
}

/// Preserved embedded-sign location, dialect, and tag on a `ZonedDecimal` `INSPECT` target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZonedSignRestore {
    /// Target has no embedded overpunch sign.
    None,
    /// Leading byte carries an embedded overpunch sign.
    Leading(OverpunchDialect, OverpunchSignTag),
    /// Trailing byte carries an embedded overpunch sign.
    Trailing(OverpunchDialect, OverpunchSignTag),
}

/// Strips any embedded zoned overpunch sign on `buf` in-place, returning its [`ZonedSignRestore`] handle.
///
/// # Arguments
///
/// * `buf` - Mutable target field bytes.
/// * `desc` - Optional target [`MoveFieldDesc`].
fn detach_target_zoned_sign(buf: &mut [u8], desc: Option<&MoveFieldDesc>) -> ZonedSignRestore {
    let Some(d) = desc else {
        return ZonedSignRestore::None;
    };
    let HirType::ZonedDecimal { sign, .. } = d.ty else {
        return ZonedSignRestore::None;
    };
    match sign {
        NumericSignSpec::EmbeddedLeading => buf.first_mut().map_or(ZonedSignRestore::None, |b| {
            let (plain, tag) = strip_overpunch_digit(*b, d.overpunch);
            *b = plain;
            ZonedSignRestore::Leading(d.overpunch, tag)
        }),
        NumericSignSpec::EmbeddedTrailing => buf.last_mut().map_or(ZonedSignRestore::None, |b| {
            let (plain, tag) = strip_overpunch_digit(*b, d.overpunch);
            *b = plain;
            ZonedSignRestore::Trailing(d.overpunch, tag)
        }),
        NumericSignSpec::Unsigned
        | NumericSignSpec::SeparateLeading
        | NumericSignSpec::SeparateTrailing => ZonedSignRestore::None,
    }
}

/// Restores a previously detached [`ZonedSignRestore`] onto `buf`.
///
/// # Arguments
///
/// * `buf` - Mutable target field bytes.
/// * `restore` - Saved [`ZonedSignRestore`] state.
fn restore_target_zoned_sign(buf: &mut [u8], restore: ZonedSignRestore) {
    match restore {
        ZonedSignRestore::None => {}
        ZonedSignRestore::Leading(dialect, tag) => {
            let _ = buf
                .first_mut()
                .map(|first| *first = reapply_overpunch_digit(*first, tag, dialect));
        }
        ZonedSignRestore::Trailing(dialect, tag) => {
            let _ = buf
                .last_mut()
                .map(|last| *last = reapply_overpunch_digit(*last, tag, dialect));
        }
    }
}

/// Computes `(win_start, win_end)` for `window` over `buf` per ISO/IEC 1989:2023 `BEFORE`/`AFTER` rules.
///
/// # Arguments
///
/// * `buf` - Normalized inspected byte slice.
/// * `window` - Clause [`InspectWindowSpec`].
#[must_use]
fn resolve_window(buf: &[u8], window: InspectWindowSpec<'_>) -> (usize, usize) {
    let mut win_start = 0;
    if let Some(after_op) = window.after {
        let after_pat = after_op.materialize_for_len(1);
        if after_pat.is_empty() {
            return (0, 0);
        }
        let Some(idx) = find_subslice(buf, &after_pat) else {
            return (0, 0);
        };
        win_start = idx + after_pat.len();
    }

    let mut win_end = buf.len();
    if let Some(before_op) = window.before {
        let before_pat = before_op.materialize_for_len(1);
        if !before_pat.is_empty() {
            let tail = buf.get(win_start..).unwrap_or(&[]);
            if let Some(rel_idx) = find_subslice(tail, &before_pat) {
                win_end = win_start + rel_idx;
            }
        }
    }
    (win_start, win_end)
}

/// Precomputes window and `LEADING`/`TRAILING` span boundaries for a single clause.
///
/// # Arguments
///
/// * `buf` - Normalized inspected byte slice.
/// * `kind` - Clause [`InspectMatchKind`].
/// * `pattern` - Pattern [`InspectOperand`].
/// * `window` - Clause [`InspectWindowSpec`].
#[must_use]
fn prepare_clause(
    buf: &[u8],
    kind: InspectMatchKind,
    pattern: InspectOperand<'_>,
    window: InspectWindowSpec<'_>,
) -> PreparedClause {
    let (win_start, win_end) = resolve_window(buf, window);
    let pat = if kind == InspectMatchKind::Characters {
        Vec::new()
    } else {
        pattern.materialize_for_len(1)
    };

    let mut lead_end = win_start;
    let mut trail_start = win_end;
    if !pat.is_empty() && win_start < win_end {
        let win_slice = buf.get(win_start..win_end).unwrap_or(&[]);
        if kind == InspectMatchKind::Leading {
            let mut off = 0;
            while win_slice
                .get(off..off + pat.len())
                .is_some_and(|w| w == pat)
            {
                off += pat.len();
            }
            lead_end = win_start + off;
        } else if kind == InspectMatchKind::Trailing {
            let mut end_off = win_slice.len();
            while end_off >= pat.len()
                && win_slice
                    .get(end_off - pat.len()..end_off)
                    .is_some_and(|w| w == pat)
            {
                end_off -= pat.len();
            }
            trail_start = win_start + end_off;
        }
    }

    PreparedClause {
        win_start,
        win_end,
        kind,
        pat,
        lead_end,
        trail_start,
        first_used: false,
    }
}

/// Tests whether `prep` matches `buf` at `pos`, returning `Some(consumed_byte_len)` on match.
///
/// # Arguments
///
/// * `prep` - Mutable [`PreparedClause`] state.
/// * `buf` - Normalized inspected byte slice.
/// * `pos` - Current 0-based scan cursor.
#[must_use]
fn try_match_prepared(prep: &mut PreparedClause, buf: &[u8], pos: usize) -> Option<usize> {
    if pos < prep.win_start || pos >= prep.win_end {
        return None;
    }
    match prep.kind {
        InspectMatchKind::Characters => Some(1),
        InspectMatchKind::All => {
            let plen = prep.pat.len();
            if plen == 0 || pos + plen > prep.win_end {
                return None;
            }
            buf.get(pos..pos + plen)
                .filter(|w| *w == prep.pat.as_slice())
                .map(|_| plen)
        }
        InspectMatchKind::First => {
            let plen = prep.pat.len();
            if prep.first_used || plen == 0 || pos + plen > prep.win_end {
                return None;
            }
            if buf.get(pos..pos + plen) == Some(prep.pat.as_slice()) {
                prep.first_used = true;
                Some(plen)
            } else {
                None
            }
        }
        InspectMatchKind::Leading => {
            let plen = prep.pat.len();
            if plen == 0
                || pos + plen > prep.lead_end
                || !(pos - prep.win_start).is_multiple_of(plen)
            {
                return None;
            }
            Some(plen)
        }
        InspectMatchKind::Trailing => {
            let plen = prep.pat.len();
            if plen == 0
                || pos < prep.trail_start
                || pos + plen > prep.win_end
                || !(prep.win_end - pos).is_multiple_of(plen)
            {
                return None;
            }
            Some(plen)
        }
    }
}

/// Executes an `INSPECT ... TALLYING ...` pass on `target`, accumulating match counts into `deltas`.
///
/// Does not mutate `target` externally (any embedded zoned sign is normalized on a scratch copy).
///
/// # Arguments
///
/// * `target` - Inspected field bytes.
/// * `target_desc` - Optional [`MoveFieldDesc`] for embedded zoned sign stripping.
/// * `clauses` - Ordered [`InspectTallyClause`] rules.
/// * `deltas` - Mutable counter increments indexed by `clause.counter_slot`.
pub fn execute_inspect_tallying(
    target: &[u8],
    target_desc: Option<&MoveFieldDesc>,
    clauses: &[InspectTallyClause<'_>],
    deltas: &mut [i64],
) {
    let mut work = target.to_vec();
    let _ = detach_target_zoned_sign(&mut work, target_desc);
    let mut preps: Vec<(usize, PreparedClause)> = clauses
        .iter()
        .map(|c| {
            (
                c.counter_slot,
                prepare_clause(&work, c.kind, c.pattern, c.window),
            )
        })
        .collect();

    let mut pos = 0;
    while pos < work.len() {
        let mut step = 1;
        for (slot_idx, prep) in &mut preps {
            if let Some(len) = try_match_prepared(prep, &work, pos) {
                let _ = deltas.get_mut(*slot_idx).map(|d| *d += 1);
                step = len;
                break;
            }
        }
        pos += step;
    }
}

/// Executes an `INSPECT ... REPLACING ...` pass in-place on `target`.
///
/// # Arguments
///
/// * `target` - Mutable inspected field bytes.
/// * `target_desc` - Optional [`MoveFieldDesc`] for embedded zoned sign detach/restore.
/// * `clauses` - Ordered [`InspectReplaceClause`] rules.
pub fn execute_inspect_replacing(
    target: &mut [u8],
    target_desc: Option<&MoveFieldDesc>,
    clauses: &[InspectReplaceClause<'_>],
) {
    let sign_restore = detach_target_zoned_sign(target, target_desc);
    let snapshot = target.to_vec();
    let mut preps: Vec<(PreparedClause, Vec<u8>)> = clauses
        .iter()
        .map(|c| {
            let prep = prepare_clause(&snapshot, c.kind, c.pattern, c.window);
            let rep_len = if c.kind == InspectMatchKind::Characters {
                1
            } else {
                prep.pat.len().max(1)
            };
            let mut rep = c.by.materialize_for_len(rep_len);
            rep.resize(rep_len, b' ');
            (prep, rep)
        })
        .collect();

    let mut pos = 0;
    while pos < snapshot.len() {
        let mut step = 1;
        for (prep, rep) in &mut preps {
            if let Some(len) = try_match_prepared(prep, &snapshot, pos) {
                let _ = target
                    .get_mut(pos..pos + len)
                    .zip(rep.get(..len))
                    .map(|(dst_seg, src_seg)| dst_seg.copy_from_slice(src_seg));
                step = len;
                break;
            }
        }
        pos += step;
    }
    restore_target_zoned_sign(target, sign_restore);
}

/// Executes an `INSPECT ... CONVERTING ...` or `TRANSFORM` pass in-place on `target`.
///
/// Each character inside the active window is translated at most once using the first matching
/// byte in `spec.from` (ISO/IEC 1989:2023 `INSPECT` Format 4 single-translation rule).
///
/// # Arguments
///
/// * `target` - Mutable inspected field bytes.
/// * `target_desc` - Optional [`MoveFieldDesc`] for embedded zoned sign detach/restore.
/// * `spec` - [`InspectConvertSpec`] conversion specification.
pub fn execute_inspect_converting(
    target: &mut [u8],
    target_desc: Option<&MoveFieldDesc>,
    spec: InspectConvertSpec<'_>,
) {
    let sign_restore = detach_target_zoned_sign(target, target_desc);
    let (win_start, win_end) = resolve_window(target, spec.window);
    let from_bytes = spec.from.materialize_for_len(1);
    let to_bytes = spec.to.materialize_for_len(from_bytes.len());

    let mut map = [None; 256];
    for (&f_b, &t_b) in from_bytes.iter().zip(to_bytes.iter()) {
        let _ = map.get_mut(usize::from(f_b)).map(|entry| {
            if entry.is_none() {
                *entry = Some(t_b);
            }
        });
    }

    for ch in target.get_mut(win_start..win_end).unwrap_or(&mut []) {
        if let Some(mapped) = map.get(usize::from(*ch)).copied().flatten() {
            *ch = mapped;
        }
    }
    restore_target_zoned_sign(target, sign_restore);
}

/// Executes a combined ISO COBOL Format 3 `INSPECT ... TALLYING ... REPLACING ...` statement.
///
/// Per ISO/IEC 1989:2023 `INSPECT` General Rules, `TALLYING` executes completely against the
/// initial contents of `target` before `REPLACING` mutates `target` in-place.
///
/// # Arguments
///
/// * `target` - Mutable inspected field bytes.
/// * `target_desc` - Optional [`MoveFieldDesc`] for embedded zoned sign detach/restore.
/// * `tally_clauses` - Ordered [`InspectTallyClause`] rules.
/// * `deltas` - Mutable counter increments indexed by `clause.counter_slot`.
/// * `replace_clauses` - Ordered [`InspectReplaceClause`] rules.
pub fn execute_inspect_tallying_and_replacing(
    target: &mut [u8],
    target_desc: Option<&MoveFieldDesc>,
    tally_clauses: &[InspectTallyClause<'_>],
    deltas: &mut [i64],
    replace_clauses: &[InspectReplaceClause<'_>],
) {
    execute_inspect_tallying(target, target_desc, tally_clauses, deltas);
    execute_inspect_replacing(target, target_desc, replace_clauses);
}

/// Executes a legacy COBOL `TRANSFORM target CHARACTERS FROM from TO to` statement in-place.
///
/// Equivalent to `INSPECT target CONVERTING from TO to` across the entire item.
///
/// # Arguments
///
/// * `target` - Mutable target field bytes.
/// * `target_desc` - Optional [`MoveFieldDesc`] for embedded zoned sign detach/restore.
/// * `from` - Source character alphabet (`FROM`).
/// * `to` - Replacement character alphabet (`TO`).
pub fn execute_transform(
    target: &mut [u8],
    target_desc: Option<&MoveFieldDesc>,
    from: InspectOperand<'_>,
    to: InspectOperand<'_>,
) {
    execute_inspect_converting(
        target,
        target_desc,
        InspectConvertSpec {
            from,
            to,
            window: InspectWindowSpec::default(),
        },
    );
}
