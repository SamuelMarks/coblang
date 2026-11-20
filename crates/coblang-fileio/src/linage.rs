//! ISO COBOL `LINAGE` logical page layout parameters, `LINAGE-COUNTER` special register state,
//! `BEFORE`/`AFTER ADVANCING` line/page framing, and `AT END-OF-PAGE` (`EOP`) detection.

use coblang_common::{CoblangError, Result};

use crate::types::{ASCII_CR, ASCII_LF, AdvancingPhase, AdvancingSpec, PageOverflowOutcome};

/// Validated `LINAGE` logical page dimensions (`LINAGE IS body LINES [WITH FOOTING AT f] ...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinageParams {
    /// Number of lines in the logical page body (`>= 1`).
    body_lines: usize,
    /// Optional 1-based starting line of the footing area (`1 <= footing <= body_lines`).
    footing_at: Option<usize>,
    /// Blank margin lines emitted at the top of each logical page (`LINES AT TOP`).
    lines_at_top: usize,
    /// Blank margin lines emitted at the bottom of each logical page (`LINES AT BOTTOM`).
    lines_at_bottom: usize,
}

impl LinageParams {
    /// Validates and constructs [`LinageParams`] per ISO COBOL `LINAGE` rules (`EC-I-O-LINAGE`).
    ///
    /// Invariants enforced:
    /// - `body_lines >= 1`
    /// - When `footing_at` is `Some(f)`, `1 <= f <= body_lines`
    ///
    /// # Arguments
    ///
    /// * `body_lines` - Page body line count (`LINAGE IS n LINES`, must be `>= 1`).
    /// * `footing_at` - Optional footing start line (`WITH FOOTING AT f`, `1..=body_lines`).
    /// * `lines_at_top` - Top margin blank line count (`LINES AT TOP`).
    /// * `lines_at_bottom` - Bottom margin blank line count (`LINES AT BOTTOM`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `body_lines == 0` or `footing_at` lies
    /// outside `1..=body_lines`.
    pub fn try_new(
        body_lines: usize,
        footing_at: Option<usize>,
        lines_at_top: usize,
        lines_at_bottom: usize,
    ) -> Result<Self> {
        if body_lines == 0 {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "LinageParams",
                value: String::from("0"),
                reason: String::from("LINAGE body line count must be >= 1 (EC-I-O-LINAGE)"),
            });
        }
        if let Some(footing) = footing_at
            && (footing == 0 || footing > body_lines)
        {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "LinageParams",
                value: format!("footing={footing}, body={body_lines}"),
                reason: String::from(
                    "LINAGE FOOTING AT must satisfy 1 <= footing <= body_lines (EC-I-O-LINAGE)",
                ),
            });
        }
        Ok(Self {
            body_lines,
            footing_at,
            lines_at_top,
            lines_at_bottom,
        })
    }

    /// Returns the page body line count (`>= 1`).
    #[must_use]
    pub const fn body_lines(self) -> usize {
        self.body_lines
    }

    /// Returns the optional 1-based footing start line (`1..=body_lines`).
    #[must_use]
    pub const fn footing_at(self) -> Option<usize> {
        self.footing_at
    }

    /// Returns the `LINES AT TOP` margin count.
    #[must_use]
    pub const fn lines_at_top(self) -> usize {
        self.lines_at_top
    }

    /// Returns the `LINES AT BOTTOM` margin count.
    #[must_use]
    pub const fn lines_at_bottom(self) -> usize {
        self.lines_at_bottom
    }

    /// Returns the total logical page height (`lines_at_top + body_lines + lines_at_bottom`).
    #[must_use]
    pub const fn total_page_lines(self) -> usize {
        self.lines_at_top
            .saturating_add(self.body_lines)
            .saturating_add(self.lines_at_bottom)
    }

    /// Returns `true` if 1-based `line` lies inside the footing area (`f..=body_lines`).
    ///
    /// # Arguments
    ///
    /// * `line` - One-based body line number (`1..=body_lines`).
    #[must_use]
    pub const fn is_in_footing(self, line: usize) -> bool {
        match self.footing_at {
            Some(f) => line >= f,
            None => false,
        }
    }
}

/// Tracks whether the current `LINAGE` body line already holds unterminated printed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LineHeadState {
    /// Print head sits at column 1 of `linage_counter`.
    AtStartOfLine,
    /// Record text was emitted on `linage_counter` by an `AFTER ADVANCING` write without trailing `0x0A` yet.
    AfterPrintedText,
}

/// Output of framing a single `WRITE` statement through [`LinagePaginationState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinageWriteResult {
    /// Exact framed byte stream (newlines, carriage returns, and record payload) to emit.
    pub framed_bytes: Vec<u8>,
    /// Updated 1-based `LINAGE-COUNTER` value after completing the `WRITE`.
    pub linage_counter: usize,
    /// Whether `AT END-OF-PAGE` (`EOP`) was triggered by footing entry or logical page overflow.
    pub page_overflow: PageOverflowOutcome,
}

/// Stateful ISO COBOL `LINAGE` pagination engine maintaining `LINAGE-COUNTER` (`1..=body_lines`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinagePaginationState {
    /// Active page's latched [`LinageParams`].
    active_page: LinageParams,
    /// Pending [`LinageParams`] to latch on the next logical page transition (for data-name `LINAGE`).
    pending_next_page: LinageParams,
    /// Current 1-based `LINAGE-COUNTER` special register (`1` upon `OPEN`).
    linage_counter: usize,
    /// Print-head column state on the current line.
    head_state: LineHeadState,
}

impl LinagePaginationState {
    /// Initializes pagination state at `OPEN OUTPUT` or `OPEN EXTEND`, setting `LINAGE-COUNTER = 1`
    /// and returning any initial `LINES AT TOP` blank lines to emit for Page 1.
    ///
    /// # Arguments
    ///
    /// * `initial_params` - Validated [`LinageParams`] sampled at `OPEN` time.
    #[must_use]
    pub fn open_page(initial_params: LinageParams) -> (Self, Vec<u8>) {
        let top_bytes = vec![ASCII_LF; initial_params.lines_at_top()];
        let state = Self {
            active_page: initial_params,
            pending_next_page: initial_params,
            linage_counter: 1,
            head_state: LineHeadState::AtStartOfLine,
        };
        (state, top_bytes)
    }

    /// Returns the current 1-based `LINAGE-COUNTER` special register value.
    #[must_use]
    pub const fn linage_counter(&self) -> usize {
        self.linage_counter
    }

    /// Returns the [`LinageParams`] latched for the currently active page.
    #[must_use]
    pub const fn active_params(&self) -> LinageParams {
        self.active_page
    }

    /// Updates the dynamic `LINAGE` parameters that will take effect on the NEXT page boundary.
    ///
    /// Per ANSI X3.23-1985 (`SQ208M`), mutating `LINAGE` data-names mid-page does not alter the
    /// current page's body/footing/bottom dimensions; the new values are latched when advancing
    /// to the next logical page via overflow or `ADVANCING PAGE`.
    ///
    /// # Arguments
    ///
    /// * `next_params` - Newly sampled [`LinageParams`] for subsequent pages.
    pub const fn set_next_page_params(&mut self, next_params: LinageParams) {
        self.pending_next_page = next_params;
    }

    /// Frames a `WRITE` of `record` under `advancing`, updating `LINAGE-COUNTER` and computing `EOP`.
    ///
    /// Note: Under `LINAGE`, both [`AdvancingSpec::NoneRaw`] and [`AdvancingSpec::DefaultSingleLine`]
    /// follow standard COBOL-85 default `AFTER ADVANCING 1 LINE` semantics.
    ///
    /// # Arguments
    ///
    /// * `record` - Record payload bytes to print.
    /// * `advancing` - `BEFORE`/`AFTER ADVANCING` specification on the `WRITE` statement.
    #[must_use]
    pub fn write_record(&mut self, record: &[u8], advancing: AdvancingSpec) -> LinageWriteResult {
        match advancing {
            AdvancingSpec::NoneRaw | AdvancingSpec::DefaultSingleLine => {
                self.write_after_lines(record, 1)
            }
            AdvancingSpec::Lines {
                phase: AdvancingPhase::After,
                count,
            } => self.write_after_lines(record, count),
            AdvancingSpec::Lines {
                phase: AdvancingPhase::Before,
                count,
            } => self.write_before_lines(record, count),
            AdvancingSpec::Page {
                phase: AdvancingPhase::After,
            } => self.write_after_page(record),
            AdvancingSpec::Page {
                phase: AdvancingPhase::Before,
            } => self.write_before_page(record),
        }
    }

    /// Returns trailing newline bytes needed when closing the file so the final printed line is terminated.
    #[must_use]
    pub fn finish_on_close(&mut self) -> Vec<u8> {
        match self.head_state {
            LineHeadState::AtStartOfLine => Vec::new(),
            LineHeadState::AfterPrintedText => {
                self.head_state = LineHeadState::AtStartOfLine;
                vec![ASCII_LF]
            }
        }
    }

    /// Ejects from `self.linage_counter` of `self.active_page` to line 1 of `self.pending_next_page`.
    ///
    /// # Arguments
    ///
    /// * `out` - Output buffer receiving remaining body newlines, bottom margin, and next top margin.
    fn eject_to_next_page(&mut self, out: &mut Vec<u8>) {
        let remaining_body = self
            .active_page
            .body_lines()
            .saturating_sub(self.linage_counter);
        let total_lf = 1_usize
            .saturating_add(remaining_body)
            .saturating_add(self.active_page.lines_at_bottom())
            .saturating_add(self.pending_next_page.lines_at_top());
        out.resize(out.len().saturating_add(total_lf), ASCII_LF);
        self.active_page = self.pending_next_page;
        self.linage_counter = 1;
        self.head_state = LineHeadState::AtStartOfLine;
    }

    /// Handles `WRITE record AFTER ADVANCING count LINES`.
    ///
    /// # Arguments
    ///
    /// * `record` - Record bytes.
    /// * `count` - Number of lines to advance prior to printing `record`.
    fn write_after_lines(&mut self, record: &[u8], count: usize) -> LinageWriteResult {
        let mut out = Vec::new();
        let target_line = self.linage_counter.saturating_add(count);
        if target_line > self.active_page.body_lines() {
            self.eject_to_next_page(&mut out);
            out.extend_from_slice(record);
            self.head_state = LineHeadState::AfterPrintedText;
            LinageWriteResult {
                framed_bytes: out,
                linage_counter: self.linage_counter,
                page_overflow: PageOverflowOutcome::EndOfPageReached,
            }
        } else {
            if count == 0 {
                if self.head_state == LineHeadState::AfterPrintedText {
                    out.push(ASCII_CR);
                }
            } else {
                out.resize(out.len().saturating_add(count), ASCII_LF);
            }
            out.extend_from_slice(record);
            self.linage_counter = target_line;
            self.head_state = LineHeadState::AfterPrintedText;
            let eop = if self.active_page.is_in_footing(self.linage_counter) {
                PageOverflowOutcome::EndOfPageReached
            } else {
                PageOverflowOutcome::WithinPage
            };
            LinageWriteResult {
                framed_bytes: out,
                linage_counter: self.linage_counter,
                page_overflow: eop,
            }
        }
    }

    /// Handles `WRITE record BEFORE ADVANCING count LINES`.
    ///
    /// # Arguments
    ///
    /// * `record` - Record bytes.
    /// * `count` - Number of lines to advance after printing `record`.
    fn write_before_lines(&mut self, record: &[u8], count: usize) -> LinageWriteResult {
        let mut out = Vec::new();
        if self.head_state == LineHeadState::AfterPrintedText {
            out.push(ASCII_CR);
        }
        out.extend_from_slice(record);
        self.head_state = LineHeadState::AfterPrintedText;

        let target_line = self.linage_counter.saturating_add(count);
        if target_line > self.active_page.body_lines() {
            self.eject_to_next_page(&mut out);
            LinageWriteResult {
                framed_bytes: out,
                linage_counter: self.linage_counter,
                page_overflow: PageOverflowOutcome::EndOfPageReached,
            }
        } else {
            if count == 0 {
                out.push(ASCII_CR);
            } else {
                out.resize(out.len().saturating_add(count), ASCII_LF);
            }
            self.linage_counter = target_line;
            self.head_state = LineHeadState::AtStartOfLine;
            let eop = if self.active_page.is_in_footing(self.linage_counter) {
                PageOverflowOutcome::EndOfPageReached
            } else {
                PageOverflowOutcome::WithinPage
            };
            LinageWriteResult {
                framed_bytes: out,
                linage_counter: self.linage_counter,
                page_overflow: eop,
            }
        }
    }

    /// Handles `WRITE record AFTER ADVANCING PAGE`.
    ///
    /// # Arguments
    ///
    /// * `record` - Record bytes printed on line 1 of the new page.
    fn write_after_page(&mut self, record: &[u8]) -> LinageWriteResult {
        let mut out = Vec::new();
        self.eject_to_next_page(&mut out);
        out.extend_from_slice(record);
        self.head_state = LineHeadState::AfterPrintedText;
        let eop = if self.active_page.is_in_footing(1) {
            PageOverflowOutcome::EndOfPageReached
        } else {
            PageOverflowOutcome::WithinPage
        };
        LinageWriteResult {
            framed_bytes: out,
            linage_counter: self.linage_counter,
            page_overflow: eop,
        }
    }

    /// Handles `WRITE record BEFORE ADVANCING PAGE`.
    ///
    /// # Arguments
    ///
    /// * `record` - Record bytes printed on the current line before ejecting to the next page.
    fn write_before_page(&mut self, record: &[u8]) -> LinageWriteResult {
        let mut out = Vec::new();
        if self.head_state == LineHeadState::AfterPrintedText {
            out.push(ASCII_CR);
        }
        out.extend_from_slice(record);
        self.head_state = LineHeadState::AfterPrintedText;
        let crossed_footing = self.active_page.footing_at().is_some();
        self.eject_to_next_page(&mut out);
        let eop = if crossed_footing {
            PageOverflowOutcome::EndOfPageReached
        } else {
            PageOverflowOutcome::WithinPage
        };
        LinageWriteResult {
            framed_bytes: out,
            linage_counter: self.linage_counter,
            page_overflow: eop,
        }
    }
}
