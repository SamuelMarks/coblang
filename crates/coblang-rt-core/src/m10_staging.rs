//! Safe runtime staging state for multi-clause `STRING`, `UNSTRING`, and `INSPECT` statements (`Milestone M10`).

use coblang_common::Result;
use coblang_pic::{
    InspectConvertSpec, InspectOperand, InspectReplaceClause, InspectTallyClause,
    InspectWindowSpec, MoveFieldDesc, StringDelimiter, StringSourcePiece, UnstringDelimSpec,
    UnstringIntoSlot, execute_inspect_converting, execute_inspect_replacing,
    execute_inspect_tallying, execute_string, execute_unstring, normalize_display_operand_bytes,
};
pub use coblang_pic::{InspectMatchKind, StringFlowOutcome, UnstringMatchMode};

use crate::desc_codec::decode_field_spec;

/// Owned delimiter specification for a staged `STRING` source piece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedStringDelim {
    /// `DELIMITED BY SIZE` (`0`).
    Size,
    /// `DELIMITED BY <pattern>` (`1`).
    Pattern(Vec<u8>),
    /// `DELIMITED BY <figurative-byte>` (`2`).
    FigurativeByte(u8),
}

impl StagedStringDelim {
    /// Decodes `(delim_kind, raw_bytes, optional_spec)` into [`StagedStringDelim`].
    ///
    /// # Arguments
    ///
    /// * `delim_kind` - `0` `Size`, `1` `Pattern`, `2` `FigurativeByte`.
    /// * `raw` - Raw pattern bytes.
    /// * `spec_opt` - Optional serialized field spec.
    ///
    /// # Errors
    ///
    /// Returns an error if `spec_opt` is non-empty and malformed.
    pub fn from_raw(delim_kind: i32, raw: &[u8], spec_opt: Option<&[u8]>) -> Result<Self> {
        match delim_kind {
            1 => Ok(Self::Pattern(normalize_with_spec(raw, spec_opt)?)),
            2 => Ok(Self::FigurativeByte(raw.first().copied().unwrap_or(b' '))),
            _ => Ok(Self::Size),
        }
    }
}

/// Owned operand for staged `INSPECT` clauses (`Bytes` vs `FigurativeByte`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedInspectOperand {
    /// Explicit character bytes (`0`).
    Bytes(Vec<u8>),
    /// Single-byte figurative constant (`1`).
    FigurativeByte(u8),
}

impl Default for StagedInspectOperand {
    /// Returns an empty [`StagedInspectOperand::Bytes`] operand.
    fn default() -> Self {
        Self::Bytes(Vec::new())
    }
}

impl StagedInspectOperand {
    /// Borrows `self` as an [`InspectOperand`].
    #[must_use]
    pub fn as_ref(&self) -> InspectOperand<'_> {
        match self {
            Self::Bytes(v) => InspectOperand::Bytes(v),
            Self::FigurativeByte(b) => InspectOperand::FigurativeByte(*b),
        }
    }

    /// Constructs a [`StagedInspectOperand`] from ABI `(kind, raw_bytes, optional_spec)`.
    ///
    /// # Arguments
    ///
    /// * `kind` - `1` for `FigurativeByte`, else `Bytes`.
    /// * `raw` - Raw operand byte slice.
    /// * `spec_opt` - Optional serialized [`crate::desc_codec::RuntimeFieldSpec`] for zoned sign stripping.
    ///
    /// # Errors
    ///
    /// Returns an error if `spec_opt` is non-empty and malformed.
    pub fn from_raw(kind: i32, raw: &[u8], spec_opt: Option<&[u8]>) -> Result<Self> {
        if kind == 1 {
            return Ok(Self::FigurativeByte(raw.first().copied().unwrap_or(b' ')));
        }
        Ok(Self::Bytes(normalize_with_spec(raw, spec_opt)?))
    }
}

/// Owned `TALLYING` clause staged in [`M10StringInspectStaging`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedTallyClause {
    /// Index into `tally_deltas`.
    pub counter_slot: usize,
    /// Clause match mode.
    pub kind: InspectMatchKind,
    /// Pattern operand.
    pub pattern: StagedInspectOperand,
    /// Optional `AFTER INITIAL` operand.
    pub after: Option<StagedInspectOperand>,
    /// Optional `BEFORE INITIAL` operand.
    pub before: Option<StagedInspectOperand>,
}

/// Owned `REPLACING` clause staged in [`M10StringInspectStaging`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedReplaceClause {
    /// Clause match mode.
    pub kind: InspectMatchKind,
    /// Pattern operand.
    pub pattern: StagedInspectOperand,
    /// Replacement (`BY`) operand.
    pub by: StagedInspectOperand,
    /// Optional `AFTER INITIAL` operand.
    pub after: Option<StagedInspectOperand>,
    /// Optional `BEFORE INITIAL` operand.
    pub before: Option<StagedInspectOperand>,
}

/// Decodes an ABI integer match-mode code into [`InspectMatchKind`].
///
/// # Arguments
///
/// * `code` - `0` `Characters`, `1` `All`, `2` `Leading`, `3` `First`, `4` `Trailing`.
#[must_use]
pub const fn decode_inspect_match_kind(code: i32) -> InspectMatchKind {
    match code {
        1 => InspectMatchKind::All,
        2 => InspectMatchKind::Leading,
        3 => InspectMatchKind::First,
        4 => InspectMatchKind::Trailing,
        _ => InspectMatchKind::Characters,
    }
}

/// Encodes [`InspectMatchKind`] into its ABI integer code.
///
/// # Arguments
///
/// * `kind` - [`InspectMatchKind`] variant.
#[must_use]
pub const fn encode_inspect_match_kind(kind: InspectMatchKind) -> i32 {
    match kind {
        InspectMatchKind::Characters => 0,
        InspectMatchKind::All => 1,
        InspectMatchKind::Leading => 2,
        InspectMatchKind::First => 3,
        InspectMatchKind::Trailing => 4,
    }
}

/// Normalizes `raw` bytes using optional serialized [`crate::desc_codec::RuntimeFieldSpec`] `spec_opt`.
///
/// # Arguments
///
/// * `raw` - Raw field or literal bytes.
/// * `spec_opt` - Optional serialized field spec (`None` or empty for plain literals).
///
/// # Errors
///
/// Returns an error if `spec_opt` is non-empty and malformed.
pub fn normalize_with_spec(raw: &[u8], spec_opt: Option<&[u8]>) -> Result<Vec<u8>> {
    match spec_opt.filter(|s| !s.is_empty()) {
        None => Ok(raw.to_vec()),
        Some(sb) => {
            let spec = decode_field_spec(sb)?;
            Ok(normalize_display_operand_bytes(raw, Some(&spec.desc)))
        }
    }
}

/// Decodes an optional serialized [`MoveFieldDesc`] (`None` when `spec_opt` is `None` or empty).
///
/// # Arguments
///
/// * `spec_opt` - Optional serialized [`crate::desc_codec::RuntimeFieldSpec`].
///
/// # Errors
///
/// Returns an error if `spec_opt` is non-empty and malformed.
pub fn decode_optional_move_desc(spec_opt: Option<&[u8]>) -> Result<Option<MoveFieldDesc>> {
    match spec_opt.filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(sb) => Ok(Some(decode_field_spec(sb)?.desc)),
    }
}

/// Window boundary selector for [`M10StringInspectStaging::inspect_set_window`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InspectWindowEdge {
    /// `AFTER INITIAL` bound.
    After,
    /// `BEFORE INITIAL` bound.
    Before,
}

/// Operand register selector for [`M10StringInspectStaging::inspect_set_operand`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InspectRegSlot {
    /// Primary pattern / `FROM` operand register.
    PatternOrFrom,
    /// Replacement `BY` / `TO` operand register.
    ByOrTo,
}

/// Per-run-unit staging buffers for `STRING`, `UNSTRING`, and `INSPECT`.
#[derive(Debug, Default)]
pub struct M10StringInspectStaging {
    /// Active delimiter applied to subsequent `string_push_src` calls.
    active_str_delim: Option<StagedStringDelim>,
    /// Staged `(normalized_bytes, delimiter)` pieces for the active `STRING` statement.
    string_pieces: Vec<(Vec<u8>, StagedStringDelim)>,
    /// Final 1-based `POINTER` produced by the most recent `STRING` or `UNSTRING`.
    last_pointer_one_based: i64,
    /// Normalized source snapshot for the active `UNSTRING` statement.
    unstring_src: Vec<u8>,
    /// Active [`crate::m17_unicode_ops::CharUnitMode`] of `unstring_src`.
    unstring_mode: crate::m17_unicode_ops::CharUnitMode,
    /// Decoded Unicode scalar vector when `unstring_mode != SingleByte`.
    unstring_chars: Vec<char>,
    /// Staged `(mode, pattern)` delimiters for the active `UNSTRING` statement.
    unstring_delims: Vec<(UnstringMatchMode, Vec<u8>)>,
    /// Optional destination field spec staged for `STRING ... INTO` when target is `NATIONAL`/`UTF-8`.
    pending_str_dst_mode: crate::m17_unicode_ops::CharUnitMode,
    /// Serialized field spec staged for the next `UNSTRING` `DELIMITER IN` target.
    pending_delim_in_spec: Vec<u8>,
    /// Current 1-based cursor inside `unstring_src`.
    unstring_ptr_one_based: i64,
    /// Whether `unstring_begin` detected an initial out-of-range `POINTER`.
    unstring_initial_overflow: bool,
    /// Accumulated count of `INTO` targets acted upon in the active `UNSTRING`.
    unstring_tally_delta: i64,
    /// Staged pattern / `FROM` operand for `INSPECT`.
    insp_pat_reg: StagedInspectOperand,
    /// Staged `BY` / `TO` operand for `INSPECT`.
    insp_by_reg: StagedInspectOperand,
    /// Staged `AFTER INITIAL` bound for `INSPECT`.
    insp_after_reg: Option<StagedInspectOperand>,
    /// Staged `BEFORE INITIAL` bound for `INSPECT`.
    insp_before_reg: Option<StagedInspectOperand>,
    /// Staged `TALLYING` clauses for the active `INSPECT` statement.
    inspect_tallies: Vec<StagedTallyClause>,
    /// Staged `REPLACING` clauses for the active `INSPECT` statement.
    inspect_replaces: Vec<StagedReplaceClause>,
    /// Computed counter deltas from the most recent `inspect_exec`.
    inspect_deltas: Vec<i64>,
}

impl M10StringInspectStaging {
    /// Creates an empty [`M10StringInspectStaging`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            active_str_delim: None,
            string_pieces: Vec::new(),
            last_pointer_one_based: 1,
            unstring_src: Vec::new(),
            unstring_mode: crate::m17_unicode_ops::CharUnitMode::SingleByte,
            unstring_chars: Vec::new(),
            unstring_delims: Vec::new(),
            pending_str_dst_mode: crate::m17_unicode_ops::CharUnitMode::SingleByte,
            pending_delim_in_spec: Vec::new(),
            unstring_ptr_one_based: 1,
            unstring_initial_overflow: false,
            unstring_tally_delta: 0,
            insp_pat_reg: StagedInspectOperand::Bytes(Vec::new()),
            insp_by_reg: StagedInspectOperand::Bytes(Vec::new()),
            insp_after_reg: None,
            insp_before_reg: None,
            inspect_tallies: Vec::new(),
            inspect_replaces: Vec::new(),
            inspect_deltas: Vec::new(),
        }
    }

    /// Clears all staged M10 buffers.
    pub fn clear(&mut self) {
        self.active_str_delim = None;
        self.string_pieces.clear();
        self.last_pointer_one_based = 1;
        self.unstring_src.clear();
        self.unstring_mode = crate::m17_unicode_ops::CharUnitMode::SingleByte;
        self.unstring_chars.clear();
        self.unstring_delims.clear();
        self.pending_str_dst_mode = crate::m17_unicode_ops::CharUnitMode::SingleByte;
        self.pending_delim_in_spec.clear();
        self.unstring_ptr_one_based = 1;
        self.unstring_initial_overflow = false;
        self.unstring_tally_delta = 0;
        self.insp_after_reg = None;
        self.insp_before_reg = None;
        self.inspect_tallies.clear();
        self.inspect_replaces.clear();
        self.inspect_deltas.clear();
    }

    /// Returns the final 1-based `POINTER` value produced by the last `STRING` or `UNSTRING`.
    #[must_use]
    pub const fn last_pointer(&self) -> i64 {
        self.last_pointer_one_based
    }

    /// Returns the `TALLYING` increment produced by the last `UNSTRING`.
    #[must_use]
    pub const fn last_unstring_tally_delta(&self) -> i64 {
        self.unstring_tally_delta
    }

    /// Resets `STRING` piece staging before a new `STRING` statement.
    pub fn string_begin(&mut self) {
        self.active_str_delim = Some(StagedStringDelim::Size);
        self.pending_str_dst_mode = crate::m17_unicode_ops::CharUnitMode::SingleByte;
        self.string_pieces.clear();
    }

    /// Configures the `INTO` destination field descriptor for `STRING` (selecting character-indexed
    /// UTF-16LE or UTF-8 mode when `dst_spec` is `NATIONAL` or `UTF-8`).
    ///
    /// # Arguments
    ///
    /// * `dst_spec` - Optional serialized [`crate::desc_codec::RuntimeFieldSpec`] of `INTO`.
    ///
    /// # Errors
    ///
    /// Returns an error if `dst_spec` is non-empty and malformed.
    pub fn string_set_dst_spec(&mut self, dst_spec: Option<&[u8]>) -> Result<()> {
        let desc_opt = decode_optional_move_desc(dst_spec)?;
        self.pending_str_dst_mode =
            crate::m17_unicode_ops::CharUnitMode::from_desc(desc_opt.as_ref());
        Ok(())
    }

    /// Sets the active [`StagedStringDelim`] applied to subsequent [`Self::string_push_src`] calls.
    ///
    /// # Arguments
    ///
    /// * `delim` - Active delimiter specification.
    pub fn string_set_delim(&mut self, delim: StagedStringDelim) {
        self.active_str_delim = Some(delim);
    }

    /// Appends one source operand to the active `STRING` statement using the active delimiter.
    ///
    /// # Arguments
    ///
    /// * `src_bytes` - Raw source operand bytes.
    /// * `src_spec` - Optional serialized field spec of source (`None` for literals).
    ///
    /// # Errors
    ///
    /// Returns an error if `src_spec` is malformed.
    pub fn string_push_src(&mut self, src_bytes: &[u8], src_spec: Option<&[u8]>) -> Result<()> {
        let norm = normalize_with_spec(src_bytes, src_spec)?;
        let delim = self
            .active_str_delim
            .clone()
            .unwrap_or(StagedStringDelim::Size);
        self.string_pieces.push((norm, delim));
        Ok(())
    }

    /// Executes the staged `STRING` pieces into `dst` starting at `initial_ptr_one_based`.
    ///
    /// Stores the resulting 1-based pointer in [`Self::last_pointer`].
    ///
    /// # Arguments
    ///
    /// * `dst` - Mutable `INTO` destination byte slice.
    /// * `initial_ptr_one_based` - Initial 1-based write pointer (`1` when `WITH POINTER` is omitted).
    #[must_use]
    pub fn string_exec(&mut self, dst: &mut [u8], initial_ptr_one_based: i64) -> StringFlowOutcome {
        let mut ptr = initial_ptr_one_based;
        let mode = self.pending_str_dst_mode;
        self.pending_str_dst_mode = crate::m17_unicode_ops::CharUnitMode::SingleByte;
        if mode != crate::m17_unicode_ops::CharUnitMode::SingleByte {
            let outcome = crate::m17_unicode_ops::execute_unicode_string(
                &self.string_pieces,
                dst,
                mode,
                &mut ptr,
            );
            self.last_pointer_one_based = ptr;
            self.string_pieces.clear();
            return outcome;
        }
        let borrowed: Vec<StringSourcePiece<'_>> = self
            .string_pieces
            .iter()
            .map(|(b, d)| StringSourcePiece {
                bytes: b,
                delimiter: match d {
                    StagedStringDelim::Size => StringDelimiter::Size,
                    StagedStringDelim::Pattern(p) => StringDelimiter::Pattern(p),
                    StagedStringDelim::FigurativeByte(ch) => StringDelimiter::FigurativeByte(*ch),
                },
            })
            .collect();
        let outcome = execute_string(&borrowed, dst, &mut ptr);
        self.last_pointer_one_based = ptr;
        self.string_pieces.clear();
        outcome
    }

    /// Initializes `UNSTRING` staging with a normalized snapshot of `src` and initial 1-based `ptr_one_based`.
    ///
    /// # Arguments
    ///
    /// * `src_bytes` - Raw source field bytes.
    /// * `src_spec` - Optional serialized field spec of source.
    /// * `ptr_one_based` - Initial 1-based `POINTER` value (`1` when `WITH POINTER` is omitted).
    ///
    /// # Errors
    ///
    /// Returns an error if `src_spec` is malformed.
    pub fn unstring_begin(
        &mut self,
        src_bytes: &[u8],
        src_spec: Option<&[u8]>,
        ptr_one_based: i64,
    ) -> Result<()> {
        let desc_opt = decode_optional_move_desc(src_spec)?;
        self.unstring_mode = crate::m17_unicode_ops::CharUnitMode::from_desc(desc_opt.as_ref());
        self.unstring_src = normalize_with_spec(src_bytes, src_spec)?;
        self.unstring_delims.clear();
        self.pending_delim_in_spec.clear();
        self.unstring_tally_delta = 0;
        self.unstring_ptr_one_based = ptr_one_based;
        self.last_pointer_one_based = ptr_one_based;
        let len_i64 = if self.unstring_mode == crate::m17_unicode_ops::CharUnitMode::SingleByte {
            self.unstring_chars.clear();
            i64::try_from(self.unstring_src.len()).unwrap_or(i64::MAX)
        } else {
            self.unstring_chars =
                crate::m17_unicode_ops::bytes_to_unit_chars(&self.unstring_src, self.unstring_mode);
            i64::try_from(self.unstring_chars.len()).unwrap_or(i64::MAX)
        };
        self.unstring_initial_overflow = ptr_one_based < 1 || ptr_one_based > len_i64;
        Ok(())
    }

    /// Appends a delimiter specification (`Single` vs `AllRepeated`) to the active `UNSTRING`.
    ///
    /// # Arguments
    ///
    /// * `mode` - [`UnstringMatchMode`] (`Single` or `AllRepeated`).
    /// * `pat_bytes` - Raw delimiter pattern bytes.
    /// * `pat_spec` - Optional serialized field spec of pattern.
    ///
    /// # Errors
    ///
    /// Returns an error if `pat_spec` is malformed.
    pub fn unstring_push_delim(
        &mut self,
        mode: UnstringMatchMode,
        pat_bytes: &[u8],
        pat_spec: Option<&[u8]>,
    ) -> Result<()> {
        let norm = normalize_with_spec(pat_bytes, pat_spec)?;
        self.unstring_delims.push((mode, norm));
        Ok(())
    }

    /// Stages serialized field descriptor bytes for the next `DELIMITER IN` slot.
    ///
    /// # Arguments
    ///
    /// * `spec_bytes` - Serialized [`crate::desc_codec::RuntimeFieldSpec`] bytes.
    pub fn unstring_set_delim_in_spec(&mut self, spec_bytes: &[u8]) {
        self.pending_delim_in_spec.clear();
        self.pending_delim_in_spec.extend_from_slice(spec_bytes);
    }

    /// Advances the active `UNSTRING` into one receiving slot, returning `Ok(Some(char_count))` if acted upon,
    /// or `Ok(None)` when the source was already exhausted or initially out-of-bounds.
    ///
    /// # Arguments
    ///
    /// * `into_buf` - Mutable target storage slice.
    /// * `into_spec_bytes` - Serialized [`crate::desc_codec::RuntimeFieldSpec`] of `into_buf`.
    /// * `delim_buf_opt` - Optional `DELIMITER IN` storage buffer (paired with `pending_delim_in_spec`).
    ///
    /// # Errors
    ///
    /// Returns an error if any field descriptor is malformed or target conversion fails.
    pub fn unstring_step_into(
        &mut self,
        into_buf: &mut [u8],
        into_spec_bytes: &[u8],
        delim_buf_opt: Option<&mut [u8]>,
    ) -> Result<Option<i64>> {
        let src_len = if self.unstring_mode == crate::m17_unicode_ops::CharUnitMode::SingleByte {
            i64::try_from(self.unstring_src.len()).unwrap_or(i64::MAX)
        } else {
            i64::try_from(self.unstring_chars.len()).unwrap_or(i64::MAX)
        };
        if self.unstring_initial_overflow || self.unstring_ptr_one_based > src_len {
            self.pending_delim_in_spec.clear();
            return Ok(None);
        }
        let into_spec = decode_field_spec(into_spec_bytes)?;
        let mut decoded_delim = match delim_buf_opt {
            Some(dbuf) => Some((dbuf, decode_field_spec(&self.pending_delim_in_spec)?.desc)),
            None => None,
        };
        self.pending_delim_in_spec.clear();
        if self.unstring_mode != crate::m17_unicode_ops::CharUnitMode::SingleByte {
            let char_delims: Vec<(UnstringMatchMode, Vec<char>)> = self
                .unstring_delims
                .iter()
                .map(|(m, pat)| {
                    (
                        *m,
                        crate::m17_unicode_ops::decode_operand_auto_chars(pat, self.unstring_mode),
                    )
                })
                .collect();
            let delim_pair = decoded_delim
                .as_mut()
                .map(|(dbuf, ddesc)| (&mut **dbuf, &*ddesc));
            let cnt = crate::m17_unicode_ops::step_unicode_unstring(
                &self.unstring_chars,
                &char_delims,
                &mut self.unstring_ptr_one_based,
                (into_buf, &into_spec.desc),
                delim_pair,
            )?;
            self.unstring_tally_delta += 1;
            return Ok(Some(cnt));
        }
        let borrowed_delims: Vec<UnstringDelimSpec<'_>> = self
            .unstring_delims
            .iter()
            .map(|(mode, pat)| UnstringDelimSpec {
                mode: *mode,
                pattern: pat,
            })
            .collect();
        let mut count_val = 0_i64;
        let mut step_tally = 0_i64;
        let delim_pair = decoded_delim
            .as_mut()
            .map(|(dbuf, ddesc)| (&mut **dbuf, &*ddesc));
        let mut slots = [UnstringIntoSlot {
            into_buf,
            into_desc: &into_spec.desc,
            delim_in: delim_pair,
            count_out: Some(&mut count_val),
        }];
        let _ = execute_unstring(
            &self.unstring_src,
            &borrowed_delims,
            &mut slots,
            &mut self.unstring_ptr_one_based,
            &mut step_tally,
        )?;
        self.unstring_tally_delta += step_tally;
        Ok(Some(count_val))
    }

    /// Completes the active `UNSTRING`, updating [`Self::last_pointer`] and returning [`StringFlowOutcome`].
    #[must_use]
    pub fn unstring_finish(&mut self) -> StringFlowOutcome {
        let src_len = if self.unstring_mode == crate::m17_unicode_ops::CharUnitMode::SingleByte {
            i64::try_from(self.unstring_src.len()).unwrap_or(i64::MAX)
        } else {
            i64::try_from(self.unstring_chars.len()).unwrap_or(i64::MAX)
        };
        let outcome = if self.unstring_initial_overflow || self.unstring_ptr_one_based <= src_len {
            StringFlowOutcome::Overflowed
        } else {
            StringFlowOutcome::Completed
        };
        self.last_pointer_one_based = self.unstring_ptr_one_based;
        self.unstring_src.clear();
        self.unstring_chars.clear();
        self.unstring_mode = crate::m17_unicode_ops::CharUnitMode::SingleByte;
        self.unstring_delims.clear();
        outcome
    }

    /// Resets `INSPECT` staging buffers before a new `INSPECT` statement.
    ///
    /// # Arguments
    ///
    /// * `counter_count` - Number of distinct `TALLYING` counter slots in this statement.
    pub fn inspect_begin(&mut self, counter_count: usize) {
        self.insp_after_reg = None;
        self.insp_before_reg = None;
        self.inspect_tallies.clear();
        self.inspect_replaces.clear();
        self.inspect_deltas.clear();
        self.inspect_deltas.resize(counter_count, 0);
    }

    /// Clears staged `AFTER` / `BEFORE` window bounds prior to configuring the next sub-clause.
    pub fn inspect_clear_window(&mut self) {
        self.insp_after_reg = None;
        self.insp_before_reg = None;
    }

    /// Stages an `AFTER INITIAL` or `BEFORE INITIAL` window operand.
    ///
    /// # Arguments
    ///
    /// * `edge` - [`InspectWindowEdge::After`] or [`InspectWindowEdge::Before`].
    /// * `op` - Staged [`StagedInspectOperand`].
    pub fn inspect_set_window(&mut self, edge: InspectWindowEdge, op: StagedInspectOperand) {
        match edge {
            InspectWindowEdge::After => self.insp_after_reg = Some(op),
            InspectWindowEdge::Before => self.insp_before_reg = Some(op),
        }
    }

    /// Stages a pattern/`FROM` or `BY`/`TO` operand register.
    ///
    /// # Arguments
    ///
    /// * `slot` - [`InspectRegSlot::PatternOrFrom`] or [`InspectRegSlot::ByOrTo`].
    /// * `op` - Staged [`StagedInspectOperand`].
    pub fn inspect_set_operand(&mut self, slot: InspectRegSlot, op: StagedInspectOperand) {
        match slot {
            InspectRegSlot::PatternOrFrom => self.insp_pat_reg = op,
            InspectRegSlot::ByOrTo => self.insp_by_reg = op,
        }
    }

    /// Appends a `TALLYING` sub-clause using the currently staged pattern and window registers.
    ///
    /// # Arguments
    ///
    /// * `counter_slot` - Index into `inspect_deltas`.
    /// * `kind` - [`InspectMatchKind`].
    pub fn inspect_push_tally_from_regs(&mut self, counter_slot: usize, kind: InspectMatchKind) {
        self.inspect_tallies.push(StagedTallyClause {
            counter_slot,
            kind,
            pattern: self.insp_pat_reg.clone(),
            after: self.insp_after_reg.clone(),
            before: self.insp_before_reg.clone(),
        });
    }

    /// Appends a `REPLACING` sub-clause using the currently staged pattern, `BY`, and window registers.
    ///
    /// # Arguments
    ///
    /// * `kind` - [`InspectMatchKind`].
    pub fn inspect_push_replace_from_regs(&mut self, kind: InspectMatchKind) {
        self.inspect_replaces.push(StagedReplaceClause {
            kind,
            pattern: self.insp_pat_reg.clone(),
            by: self.insp_by_reg.clone(),
            after: self.insp_after_reg.clone(),
            before: self.insp_before_reg.clone(),
        });
    }

    /// Executes all staged `TALLYING` clauses followed by all staged `REPLACING` clauses on `target`.
    ///
    /// # Arguments
    ///
    /// * `target` - Mutable inspected field bytes.
    /// * `target_spec` - Optional serialized field spec of `target`.
    ///
    /// # Errors
    ///
    /// Returns an error if `target_spec` is non-empty and malformed.
    pub fn inspect_exec_tally_replace(
        &mut self,
        target: &mut [u8],
        target_spec: Option<&[u8]>,
    ) -> Result<()> {
        let desc_opt = decode_optional_move_desc(target_spec)?;
        let mode = crate::m17_unicode_ops::CharUnitMode::from_desc(desc_opt.as_ref());
        if mode != crate::m17_unicode_ops::CharUnitMode::SingleByte {
            crate::m17_unicode_ops::execute_unicode_inspect_tally_replace(
                target,
                mode,
                &self.inspect_tallies,
                &self.inspect_replaces,
                &mut self.inspect_deltas,
            );
            self.inspect_tallies.clear();
            self.inspect_replaces.clear();
            self.inspect_clear_window();
            return Ok(());
        }
        if !self.inspect_tallies.is_empty() {
            let t_refs: Vec<InspectTallyClause<'_>> = self
                .inspect_tallies
                .iter()
                .map(|c| InspectTallyClause {
                    counter_slot: c.counter_slot,
                    kind: c.kind,
                    pattern: c.pattern.as_ref(),
                    window: InspectWindowSpec {
                        after: c.after.as_ref().map(StagedInspectOperand::as_ref),
                        before: c.before.as_ref().map(StagedInspectOperand::as_ref),
                    },
                })
                .collect();
            execute_inspect_tallying(target, desc_opt.as_ref(), &t_refs, &mut self.inspect_deltas);
        }
        if !self.inspect_replaces.is_empty() {
            let r_refs: Vec<InspectReplaceClause<'_>> = self
                .inspect_replaces
                .iter()
                .map(|c| InspectReplaceClause {
                    kind: c.kind,
                    pattern: c.pattern.as_ref(),
                    by: c.by.as_ref(),
                    window: InspectWindowSpec {
                        after: c.after.as_ref().map(StagedInspectOperand::as_ref),
                        before: c.before.as_ref().map(StagedInspectOperand::as_ref),
                    },
                })
                .collect();
            execute_inspect_replacing(target, desc_opt.as_ref(), &r_refs);
        }
        self.inspect_tallies.clear();
        self.inspect_replaces.clear();
        Ok(())
    }

    /// Reads the accumulated `TALLYING` increment for `slot` (`0` if out of range).
    ///
    /// # Arguments
    ///
    /// * `slot` - Counter slot index.
    #[must_use]
    pub fn inspect_tally_delta(&self, slot: usize) -> i64 {
        self.inspect_deltas.get(slot).copied().unwrap_or(0)
    }

    /// Executes an `INSPECT ... CONVERTING ...` or `TRANSFORM` operation on `target` using staged registers.
    ///
    /// # Arguments
    ///
    /// * `target` - Mutable inspected field bytes.
    /// * `target_spec` - Optional serialized field spec of `target`.
    ///
    /// # Errors
    ///
    /// Returns an error if `target_spec` is non-empty and malformed.
    pub fn inspect_exec_converting_from_regs(
        &mut self,
        target: &mut [u8],
        target_spec: Option<&[u8]>,
    ) -> Result<()> {
        let desc_opt = decode_optional_move_desc(target_spec)?;
        let mode = crate::m17_unicode_ops::CharUnitMode::from_desc(desc_opt.as_ref());
        if mode != crate::m17_unicode_ops::CharUnitMode::SingleByte {
            crate::m17_unicode_ops::execute_unicode_inspect_converting(
                target,
                mode,
                &self.insp_pat_reg,
                &self.insp_by_reg,
                (self.insp_after_reg.as_ref(), self.insp_before_reg.as_ref()),
            );
            self.inspect_clear_window();
            return Ok(());
        }
        execute_inspect_converting(
            target,
            desc_opt.as_ref(),
            InspectConvertSpec {
                from: self.insp_pat_reg.as_ref(),
                to: self.insp_by_reg.as_ref(),
                window: InspectWindowSpec {
                    after: self
                        .insp_after_reg
                        .as_ref()
                        .map(StagedInspectOperand::as_ref),
                    before: self
                        .insp_before_reg
                        .as_ref()
                        .map(StagedInspectOperand::as_ref),
                },
            },
        );
        self.inspect_clear_window();
        Ok(())
    }
}
