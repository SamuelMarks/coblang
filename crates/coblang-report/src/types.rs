//! Strongly-typed domain primitives for `REPORT SECTION` zones, positioning, control levels, and output lines.

#![deny(clippy::indexing_slicing)]

use std::io::Write;

use coblang_common::{CoblangError, Result};

/// Strongly-typed handle for a Data Division field referenced by `CONTROLS`, `SOURCE`, `SUM`, or `PRESENT WHEN`.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldId(
    /// Zero-based field identifier.
    u32,
);

impl FieldId {
    /// Constructs a [`FieldId`] from a raw `u32` identifier.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based field identifier.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying `u32` identifier.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Strongly-typed handle for a `SUM` accumulator declared inside a `CONTROL FOOTING` report group.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SumCounterId(
    /// Zero-based `SUM` counter slot index inside `ReportDesc::sum_counters`.
    u32,
);

impl SumCounterId {
    /// Constructs a [`SumCounterId`] from a zero-based accumulator slot index.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based `SUM` accumulator index.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying `u32` accumulator index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Converts this identifier into a `usize` slice index.
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Strongly-typed handle for an external `PRESENT WHEN` boolean condition evaluated by the runtime environment.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConditionId(
    /// Zero-based condition identifier.
    u32,
);

impl ConditionId {
    /// Constructs a [`ConditionId`] from a raw `u32` identifier.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based condition identifier.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying `u32` identifier.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Zero-based index into an `RD` major-to-minor `CONTROLS ARE [FINAL] c1 c2 ...` hierarchy (`0` = most major).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ControlLevelIndex(
    /// Zero-based control hierarchy slot (`0` is most major, e.g. `FINAL`).
    u16,
);

impl ControlLevelIndex {
    /// Most major control hierarchy slot (`0`).
    pub const ZERO: Self = Self(0);

    /// Constructs a [`ControlLevelIndex`] from a zero-based `u16` index.
    ///
    /// # Arguments
    ///
    /// * `index` - Zero-based major-to-minor control hierarchy position.
    #[must_use]
    pub const fn new(index: u16) -> Self {
        Self(index)
    }

    /// Converts a `usize` hierarchy slot into a validated [`ControlLevelIndex`].
    ///
    /// # Arguments
    ///
    /// * `index` - Zero-based control hierarchy position.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `index` exceeds `u16::MAX`.
    pub fn try_from_usize(index: usize) -> Result<Self> {
        let converted = u16::try_from(index).map_err(|_| CoblangError::InvalidDomainValue {
            domain_type: "ControlLevelIndex",
            value: index.to_string(),
            reason: String::from("control hierarchy index exceeds u16::MAX"),
        })?;
        Ok(Self(converted))
    }

    /// Returns the underlying `u16` control index.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self.0
    }

    /// Returns the zero-based `usize` control index.
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Classification of an entry in an `RD` `CONTROLS ARE` hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlKind {
    /// `FINAL` report-wide control level (fires `CH FINAL` on first `GENERATE` and `CF FINAL` on `TERMINATE`).
    Final,
    /// Data-item control key bound to a [`FieldId`].
    DataField(FieldId),
}

/// Single control level declaration inside an `RD` `CONTROLS ARE [FINAL] c1 c2 ...` clause.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ControlDef {
    /// Canonical control level name (`"FINAL"` or the COBOL data-item identifier).
    pub name: String,
    /// Control break binding (`Final` vs `DataField`).
    pub kind: ControlKind,
}

impl ControlDef {
    /// Creates the top-level `FINAL` control definition.
    #[must_use]
    pub fn final_control() -> Self {
        Self {
            name: String::from("FINAL"),
            kind: ControlKind::Final,
        }
    }

    /// Creates a data-field control definition for `name` bound to `field_id`.
    ///
    /// # Arguments
    ///
    /// * `name` - Canonical COBOL data-item name.
    /// * `field_id` - Resolved [`FieldId`] read on each `GENERATE`.
    #[must_use]
    pub fn field(name: &str, field_id: FieldId) -> Self {
        Self {
            name: String::from(name),
            kind: ControlKind::DataField(field_id),
        }
    }
}

/// Strongly-typed ISO COBOL `TYPE IS ...` classification for a `01`-level report group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportGroupKind {
    /// `REPORT HEADING` (`RH`) — presented once at the start of the report on first `GENERATE`.
    ReportHeading,
    /// `PAGE HEADING` (`PH`) — presented at the top of each page.
    PageHeading,
    /// `CONTROL HEADING` (`CH`) — presented from major to minor when a control level starts.
    ControlHeading(ControlLevelIndex),
    /// `DETAIL` (`DE`) — presented on explicit `GENERATE detail-group-name`.
    Detail,
    /// `CONTROL FOOTING` (`CF`) — presented from minor to major when a control break or `TERMINATE` occurs.
    ControlFooting(ControlLevelIndex),
    /// `PAGE FOOTING` (`PF`) — presented at the bottom of a completed page.
    PageFooting,
    /// `REPORT FOOTING` (`RF`) — presented once at the end of the report during `TERMINATE`.
    ReportFooting,
}

impl ReportGroupKind {
    /// Returns `true` when this group is a body group (`CONTROL HEADING`, `DETAIL`, or `CONTROL FOOTING`).
    #[must_use]
    pub const fn is_body_group(self) -> bool {
        matches!(
            self,
            Self::ControlHeading(_) | Self::Detail | Self::ControlFooting(_)
        )
    }

    /// Returns the associated [`ControlLevelIndex`] for `CONTROL HEADING` or `CONTROL FOOTING` groups.
    #[must_use]
    pub const fn control_level(self) -> Option<ControlLevelIndex> {
        match self {
            Self::ControlHeading(idx) | Self::ControlFooting(idx) => Some(idx),
            Self::ReportHeading
            | Self::PageHeading
            | Self::Detail
            | Self::PageFooting
            | Self::ReportFooting => None,
        }
    }

    /// Returns the canonical two-character ISO COBOL abbreviation (`"RH"`, `"PH"`, `"CH"`, `"DE"`, `"CF"`, `"PF"`, `"RF"`).
    #[must_use]
    pub const fn short_code(self) -> &'static str {
        match self {
            Self::ReportHeading => "RH",
            Self::PageHeading => "PH",
            Self::ControlHeading(_) => "CH",
            Self::Detail => "DE",
            Self::ControlFooting(_) => "CF",
            Self::PageFooting => "PF",
            Self::ReportFooting => "RF",
        }
    }
}

/// Vertical line-positioning specification (`LINE NUMBER IS ...`) on a report group or elementary item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinePositionSpec {
    /// `LINE NUMBER IS n` — 1-based page line number on the current (or overflowed next) page.
    Absolute(u32),
    /// `LINE NUMBER IS n ON NEXT PAGE` — 1-based page line number after advancing to a new page.
    OnNextPage(u32),
    /// `LINE NUMBER IS PLUS n` — relative downward advance of `n` lines from `LINE-COUNTER`.
    Relative(u32),
}

impl LinePositionSpec {
    /// Constructs a validated 1-based [`Self::Absolute`] line specification (`line >= 1`).
    ///
    /// # Arguments
    ///
    /// * `line` - One-based page line number.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `line == 0`.
    pub fn try_absolute(line: u32) -> Result<Self> {
        if line == 0 {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "LinePositionSpec",
                value: String::from("0"),
                reason: String::from("absolute LINE NUMBER must be >= 1"),
            });
        }
        Ok(Self::Absolute(line))
    }

    /// Constructs a validated 1-based [`Self::OnNextPage`] line specification (`line >= 1`).
    ///
    /// # Arguments
    ///
    /// * `line` - One-based line number on the next page.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `line == 0`.
    pub fn try_on_next_page(line: u32) -> Result<Self> {
        if line == 0 {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "LinePositionSpec",
                value: String::from("0"),
                reason: String::from("ON NEXT PAGE line number must be >= 1"),
            });
        }
        Ok(Self::OnNextPage(line))
    }

    /// Constructs a [`Self::Relative`] (`LINE NUMBER IS PLUS offset`) specification.
    ///
    /// # Arguments
    ///
    /// * `offset` - Number of lines to advance downward (`0` denotes same-line overprint/continuation).
    #[must_use]
    pub const fn relative(offset: u32) -> Self {
        Self::Relative(offset)
    }
}

/// Horizontal column-positioning specification (`COLUMN NUMBER IS ...`) on an elementary printable item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnPositionSpec {
    /// `COLUMN NUMBER IS c` — 1-based character column position.
    Absolute(u32),
    /// `COLUMN NUMBER IS PLUS c` — relative offset added to the rightmost column written so far on the line.
    Relative(u32),
}

impl ColumnPositionSpec {
    /// Constructs a validated 1-based [`Self::Absolute`] column specification (`column >= 1`).
    ///
    /// # Arguments
    ///
    /// * `column` - One-based column coordinate.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `column == 0`.
    pub fn try_absolute(column: u32) -> Result<Self> {
        if column == 0 {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "ColumnPositionSpec",
                value: String::from("0"),
                reason: String::from("absolute COLUMN NUMBER must be >= 1"),
            });
        }
        Ok(Self::Absolute(column))
    }

    /// Constructs a [`Self::Relative`] (`COLUMN NUMBER IS PLUS offset`) specification.
    ///
    /// # Arguments
    ///
    /// * `offset` - Relative horizontal column step from the previous item's end column.
    #[must_use]
    pub const fn relative(offset: u32) -> Self {
        Self::Relative(offset)
    }
}

/// Post-presentation spacing specification (`NEXT GROUP IS ...`) on a `01` report group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NextGroupSpec {
    /// `NEXT GROUP IS n` — sets `LINE-COUNTER` to `n` (or forces a page advance before the next body group if passed).
    Absolute(u32),
    /// `NEXT GROUP IS PLUS n` — advances `LINE-COUNTER` by `n` lines (or forces next page if exceeding zone limit).
    Relative(u32),
    /// `NEXT GROUP IS NEXT PAGE` — forces a page advance before the next body group (or standalone `RH` cover page).
    NextPage,
}

impl NextGroupSpec {
    /// Constructs a validated [`Self::Absolute`] `NEXT GROUP` specification (`line >= 1`).
    ///
    /// # Arguments
    ///
    /// * `line` - One-based target `LINE-COUNTER` position.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `line == 0`.
    pub fn try_absolute(line: u32) -> Result<Self> {
        if line == 0 {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "NextGroupSpec",
                value: String::from("0"),
                reason: String::from("NEXT GROUP absolute line must be >= 1"),
            });
        }
        Ok(Self::Absolute(line))
    }

    /// Constructs a validated [`Self::Relative`] `NEXT GROUP PLUS` specification (`offset >= 1`).
    ///
    /// # Arguments
    ///
    /// * `offset` - Positive relative line advance (`>= 1`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `offset == 0`.
    pub fn try_relative(offset: u32) -> Result<Self> {
        if offset == 0 {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "NextGroupSpec",
                value: String::from("0"),
                reason: String::from("NEXT GROUP PLUS offset must be >= 1"),
            });
        }
        Ok(Self::Relative(offset))
    }

    /// Constructs [`Self::NextPage`] (`NEXT GROUP IS NEXT PAGE`).
    #[must_use]
    pub const fn next_page() -> Self {
        Self::NextPage
    }
}

/// Validated ISO/ANSI COBOL `PAGE LIMIT IS n LINES` vertical zone boundaries for an `RD`.
///
/// Enforces the invariant `1 <= heading <= first_detail <= last_detail <= footing <= page_limit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageLimits {
    /// Maximum lines per page (`PAGE LIMIT IS n LINES`).
    page_limit: u32,
    /// First line on which `REPORT HEADING` or `PAGE HEADING` may appear (`HEADING`).
    heading: u32,
    /// First line on which body groups (`CH`, `DE`, `CF`) may appear (`FIRST DETAIL`).
    first_detail: u32,
    /// Last line on which `CONTROL HEADING` or `DETAIL` groups may appear (`LAST DETAIL`).
    last_detail: u32,
    /// Last line on which `CONTROL FOOTING` groups may appear (`FOOTING`).
    footing: u32,
}

impl PageLimits {
    /// Resolves ANSI/ISO default zones and validates `1 <= heading <= first_detail <= last_detail <= footing <= page_limit`.
    ///
    /// # Arguments
    ///
    /// * `page_limit` - Total lines per page (`>= 1`).
    /// * `heading` - Optional `HEADING` line (defaults to `1`).
    /// * `first_detail` - Optional `FIRST DETAIL` line (defaults to resolved `heading`).
    /// * `last_detail` - Optional `LAST DETAIL` line (defaults to `footing` if specified, else `page_limit`).
    /// * `footing` - Optional `FOOTING` line (defaults to `last_detail` if specified, else `page_limit`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `page_limit == 0` or the zone ordering invariant is violated.
    pub fn try_new(
        page_limit: u32,
        heading: Option<u32>,
        first_detail: Option<u32>,
        last_detail: Option<u32>,
        footing: Option<u32>,
    ) -> Result<Self> {
        if page_limit == 0 {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "PageLimits",
                value: String::from("0"),
                reason: String::from("PAGE LIMIT must be >= 1"),
            });
        }
        let h = heading.unwrap_or(1);
        let fd = first_detail.unwrap_or(h);
        let (ld, f) = match (last_detail, footing) {
            (Some(l), Some(ft)) => (l, ft),
            (Some(l), None) => (l, l),
            (None, Some(ft)) => (ft, ft),
            (None, None) => (page_limit, page_limit),
        };

        if h == 0 || h > fd || fd > ld || ld > f || f > page_limit {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "PageLimits",
                value: format!(
                    "limit={page_limit}, heading={h}, first_detail={fd}, last_detail={ld}, footing={f}"
                ),
                reason: String::from(
                    "expected 1 <= heading <= first_detail <= last_detail <= footing <= page_limit",
                ),
            });
        }

        Ok(Self {
            page_limit,
            heading: h,
            first_detail: fd,
            last_detail: ld,
            footing: f,
        })
    }

    /// Returns the maximum lines per page (`PAGE LIMIT`).
    #[must_use]
    pub const fn page_limit(self) -> u32 {
        self.page_limit
    }

    /// Returns the resolved `HEADING` line number.
    #[must_use]
    pub const fn heading(self) -> u32 {
        self.heading
    }

    /// Returns the resolved `FIRST DETAIL` line number.
    #[must_use]
    pub const fn first_detail(self) -> u32 {
        self.first_detail
    }

    /// Returns the resolved `LAST DETAIL` line number.
    #[must_use]
    pub const fn last_detail(self) -> u32 {
        self.last_detail
    }

    /// Returns the resolved `FOOTING` line number.
    #[must_use]
    pub const fn footing(self) -> u32 {
        self.footing
    }
}

/// Lifecycle state of a `ReportState` (`Inactive` vs `Active`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportLifecycleState {
    /// Report has not been initiated or has already been terminated.
    Inactive,
    /// Report is currently active between `INITIATE` and `TERMINATE`.
    Active,
}

/// `GROUP INDICATE` repetition-suppression mode for an elementary `DETAIL` item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupIndicatePolicy {
    /// Always render the elementary item whenever its `DETAIL` group is presented.
    AlwaysDisplay,
    /// Render once after `INITIATE`, any control break, or any page break; replace with spaces on repeats.
    GroupIndicate,
}

/// Target selector for a COBOL `GENERATE` statement.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GenerateTarget {
    /// Summary reporting (`GENERATE report-name`): processes control breaks and unconditional `SUM`s without printing a `DETAIL` group.
    Summary,
    /// Detail reporting (`GENERATE detail-group`) targeting a zero-based index in `ReportDesc::groups`.
    DetailIndex(usize),
    /// Detail reporting (`GENERATE detail-group-name`) matched case-insensitively against `01` `DETAIL` group names.
    DetailName(String),
}

/// Vertical carriage-control transition preceding an emitted [`ReportLine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportLineAdvancing {
    /// Very first emitted line of the initiated report (`skip_before = line_number - 1`).
    InitialLine {
        /// Number of blank lines between top-of-page line `1` and this line.
        skip_before: u32,
    },
    /// Subsequent emitted line on the same page (`lines_down = line_number - previous_line_number`).
    SamePage {
        /// Vertical downward delta (`0` for same-line overprint/continuation, `>= 1` for normal advance).
        lines_down: u32,
    },
    /// First emitted line on a new page (`skip_before = line_number - 1`).
    NewPage {
        /// Number of blank lines between top-of-page line `1` and this line on the new page.
        skip_before: u32,
    },
}

/// Structured physical print line produced by `ReportState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportLine {
    /// One-based `PAGE-COUNTER` value when this line was emitted.
    pub page_number: u32,
    /// One-based `LINE-COUNTER` value of this line on `page_number`.
    pub line_number: u32,
    /// Vertical carriage-control transition from the prior emitted line.
    pub advancing: ReportLineAdvancing,
    /// Originating [`ReportGroupKind`] (`RH`, `PH`, `CH`, `DE`, `CF`, `PF`, or `RF`).
    pub group_kind: ReportGroupKind,
    /// Optional `RD` `CODE` clause prefix prepended to physical file records.
    pub code_prefix: Vec<u8>,
    /// Formatted horizontal column bytes (excluding `code_prefix`).
    pub content: Vec<u8>,
}

impl ReportLine {
    /// Assembles the physical record (`code_prefix` + `content`), optionally space-padded or truncated to `fixed_width`.
    ///
    /// # Arguments
    ///
    /// * `fixed_width` - Optional fixed record byte length (when `None`, trailing content width is preserved).
    #[must_use]
    pub fn full_record_bytes(&self, fixed_width: Option<usize>) -> Vec<u8> {
        let total = self.code_prefix.len().saturating_add(self.content.len());
        let mut out = Vec::with_capacity(fixed_width.unwrap_or(total));
        out.extend_from_slice(&self.code_prefix);
        out.extend_from_slice(&self.content);
        if let Some(width) = fixed_width {
            if out.len() > width {
                out.truncate(width);
            } else if out.len() < width {
                out.resize(width, b' ');
            }
        }
        out
    }
}

/// Page separator byte convention for [`RenderFormat::LineSequential`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageBreakStyle {
    /// Emit an ASCII form-feed (`0x0C`) byte at the start of each [`ReportLineAdvancing::NewPage`].
    FormFeed,
    /// Separate pages purely by line-feeds without emitting `0x0C`.
    BlankLinesOnly,
}

/// Serialization format converting structured [`ReportLine`] slices into flat output bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderFormat {
    /// Expands vertical spacing (`skip_before`, `lines_down`) into blank lines for `LINE SEQUENTIAL` / console streams.
    LineSequential {
        /// Whether page transitions emit an ASCII form-feed (`0x0C`).
        page_break: PageBreakStyle,
    },
    /// Emits one LF-terminated (`0x0A`) record per presented [`ReportLine`], optionally padded/truncated to `fixed_width`.
    PresentedRecords {
        /// Optional fixed record length in bytes.
        fixed_width: Option<usize>,
    },
}

/// Renders `lines` into an in-memory byte vector according to `format`.
///
/// # Arguments
///
/// * `lines` - Emitted [`ReportLine`] slice.
/// * `format` - Target [`RenderFormat`] policy.
#[must_use]
pub fn render_lines_to_bytes(lines: &[ReportLine], format: RenderFormat) -> Vec<u8> {
    let mut out = Vec::new();
    match format {
        RenderFormat::LineSequential { page_break } => {
            for line in lines {
                match line.advancing {
                    ReportLineAdvancing::InitialLine { skip_before } => {
                        append_blank_lines(&mut out, skip_before, &line.code_prefix);
                    }
                    ReportLineAdvancing::SamePage { lines_down } => {
                        if lines_down == 0 {
                            out.push(0x0D);
                        } else {
                            out.push(0x0A);
                            append_blank_lines(
                                &mut out,
                                lines_down.saturating_sub(1),
                                &line.code_prefix,
                            );
                        }
                    }
                    ReportLineAdvancing::NewPage { skip_before } => {
                        out.push(0x0A);
                        if page_break == PageBreakStyle::FormFeed {
                            out.push(0x0C);
                        }
                        append_blank_lines(&mut out, skip_before, &line.code_prefix);
                    }
                }
                out.extend_from_slice(&line.code_prefix);
                out.extend_from_slice(&line.content);
            }
            if !lines.is_empty() {
                out.push(0x0A);
            }
        }
        RenderFormat::PresentedRecords { fixed_width } => {
            for line in lines {
                let rec = line.full_record_bytes(fixed_width);
                out.extend_from_slice(&rec);
                out.push(0x0A);
            }
        }
    }
    out
}

/// Appends `count` blank LF-terminated (`0x0A`) lines (each prefixed by `code_prefix` if non-empty) to `out`.
///
/// # Arguments
///
/// * `out` - Destination byte buffer.
/// * `count` - Number of blank spacing lines to emit.
/// * `code_prefix` - Optional `RD` `CODE` literal bytes.
fn append_blank_lines(out: &mut Vec<u8>, count: u32, code_prefix: &[u8]) {
    for _ in 0..count {
        out.extend_from_slice(code_prefix);
        out.push(0x0A);
    }
}

/// Writes `lines` formatted via `format` into `writer` and flushes the stream.
///
/// # Arguments
///
/// * `lines` - Emitted [`ReportLine`] slice.
/// * `format` - Target [`RenderFormat`] policy.
/// * `writer` - Destination byte stream.
///
/// # Errors
///
/// Returns [`CoblangError::Io`] if writing or flushing `writer` fails.
pub fn write_rendered_lines(
    lines: &[ReportLine],
    format: RenderFormat,
    writer: &mut dyn Write,
) -> Result<()> {
    let bytes = render_lines_to_bytes(lines, format);
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}
