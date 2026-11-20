//! Declarative `RD` report descriptor, hierarchical `01`..`49` group nodes, `SUM` rules, and schema validation.

#![deny(clippy::indexing_slicing)]

use std::collections::BTreeSet;

use coblang_common::{CoblangError, LevelNumber, Result};
use coblang_decimal::FixedDecimal;
use coblang_hir::JustifiedSpec;
use coblang_pic::MoveFieldDesc;

use crate::types::{
    ColumnPositionSpec, ConditionId, ControlDef, ControlKind, ControlLevelIndex, FieldId,
    GroupIndicatePolicy, LinePositionSpec, NextGroupSpec, PageLimits, ReportGroupKind,
    SumCounterId,
};

/// Addend term feeding a `CONTROL FOOTING` `SUM` accumulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SumAddend {
    /// Augments the accumulator by reading a numeric Data Division [`FieldId`] during `GENERATE`.
    SourceField(FieldId),
    /// Rolls a subordinate `CONTROL FOOTING` [`SumCounterId`] into this counter prior to resetting the subordinate counter.
    RollingCounter(SumCounterId),
}

/// `UPON` detail-group filter controlling when `GENERATE` augments a `SUM` accumulator.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SumUponFilter {
    /// Accumulate on every `GENERATE` execution (both detail and summary reporting).
    AllGenerations,
    /// Accumulate only when `GENERATE` targets one of the named `DETAIL` groups (ASCII case-insensitive).
    NamedDetails(Vec<String>),
}

/// Declarative specification of a `SUM ... [UPON ...] [RESET ON ...]` accumulator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SumCounterDesc {
    /// Unique slot identifier matching its position in [`ReportDesc::sum_counters`].
    pub id: SumCounterId,
    /// Optional COBOL sum-counter item name (for rolling totals or diagnostics).
    pub name: Option<String>,
    /// Control hierarchy level of the `CONTROL FOOTING` group that declares this counter.
    pub owner_control_level: ControlLevelIndex,
    /// Control hierarchy level whose completion resets this accumulator to zero (`<= owner_control_level`).
    pub reset_level: ControlLevelIndex,
    /// One or more [`SumAddend`] operands summed into this counter.
    pub addends: Vec<SumAddend>,
    /// `UPON` filter governing `SourceField` accumulation during `GENERATE`.
    pub upon: SumUponFilter,
}

/// Declarative `PRESENT WHEN` visibility predicate attached to a report group, sub-group, or elementary item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresentWhenSpec {
    /// Unconditionally visible.
    Always,
    /// Delegates boolean evaluation to `ReportEnv::eval_condition`.
    Condition(ConditionId),
    /// Visible when the numeric value of the specified field is non-zero.
    FieldNonZero(FieldId),
    /// Visible when the raw byte slice of `field` equals `expected`.
    FieldEquals {
        /// Target [`FieldId`] to inspect.
        field: FieldId,
        /// Expected byte sequence.
        expected: Vec<u8>,
    },
    /// Logical negation (`NOT`).
    Not(Box<Self>),
    /// Logical conjunction (`AND`; empty slice evaluates to `true`).
    All(Vec<Self>),
    /// Logical disjunction (`OR`; empty slice evaluates to `false`).
    Any(Vec<Self>),
}

/// Value origin (`VALUE`, `SOURCE`, `SUM`, `PAGE-COUNTER`, or `LINE-COUNTER`) for an elementary report item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemValueSource {
    /// Constant byte literal (`VALUE IS "..."`).
    Literal(Vec<u8>),
    /// Constant exact decimal literal (`VALUE IS 123.45`).
    DecimalLiteral(FixedDecimal),
    /// Live Data Division field (`SOURCE IS identifier`).
    SourceField(FieldId),
    /// `CONTROL FOOTING` `SUM` accumulator (`SUM ...`).
    SumCounter(SumCounterId),
    /// Per-report `PAGE-COUNTER` special register (`SOURCE IS PAGE-COUNTER`).
    PageCounter,
    /// Per-report `LINE-COUNTER` special register (`SOURCE IS LINE-COUNTER`).
    LineCounter,
}

/// Formatting rule converting an [`ItemValueSource`] into printable horizontal column bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldFormatter {
    /// Emits raw literal/field bytes verbatim (or plain ASCII decimal text for numeric/counter sources).
    RawBytes,
    /// Pads or truncates alphanumeric text to `width` bytes using `justified` and `space_byte`.
    FixedWidthAlpha {
        /// Fixed display width in bytes (`>= 1`).
        width: usize,
        /// Left vs `JUSTIFIED RIGHT` alignment.
        justified: JustifiedSpec,
        /// Padding byte (`0x20` for ASCII, `0x40` for EBCDIC).
        space_byte: u8,
    },
    /// Formats the source value through `coblang_pic::execute_move` into a `byte_len`-byte `PICTURE` buffer.
    Picture {
        /// Target formatted storage width in bytes (`>= 1`).
        byte_len: usize,
        /// Compiled `PICTURE` / `HirType` descriptor.
        desc: MoveFieldDesc,
    },
}

/// Elementary item inside a report group (`01`..`49` leaf entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportFieldItem {
    /// Optional COBOL data-name of this elementary report item.
    pub name: Option<String>,
    /// Optional `LINE NUMBER IS ...` vertical transition starting a new print line at this item.
    pub line: Option<LinePositionSpec>,
    /// Optional `COLUMN NUMBER IS ...` horizontal placement (`None` denotes an unprinted accumulator item).
    pub column: Option<ColumnPositionSpec>,
    /// Optional value source (`VALUE`, `SOURCE`, `SUM`, `PAGE-COUNTER`, `LINE-COUNTER`).
    pub source: Option<ItemValueSource>,
    /// Output formatter applied when rendering `source` into `column`.
    pub formatter: FieldFormatter,
    /// `GROUP INDICATE` suppression mode (`AlwaysDisplay` outside `DETAIL` groups).
    pub group_indicate: GroupIndicatePolicy,
    /// Optional `PRESENT WHEN` visibility guard on this elementary item.
    pub present_when: PresentWhenSpec,
}

/// Intermediate (`02`..`48`) sub-group node inside a `01` report group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportSubGroup {
    /// Optional COBOL group name.
    pub name: Option<String>,
    /// Optional `LINE NUMBER IS ...` starting a print line for child items.
    pub line: Option<LinePositionSpec>,
    /// Optional `PRESENT WHEN` guard suppressing the entire sub-group (and its `line` advance) when `false`.
    pub present_when: PresentWhenSpec,
    /// Ordered child sub-groups and elementary items.
    pub children: Vec<ReportNode>,
}

/// Hierarchical node inside a [`ReportGroupDesc`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportNode {
    /// Intermediate hierarchical group (`02`..`48`).
    SubGroup(ReportSubGroup),
    /// Leaf elementary item (`01`..`49`).
    Elementary(ReportFieldItem),
}

/// Complete `01`-level report group descriptor (`RH`, `PH`, `CH`, `DE`, `CF`, `PF`, or `RF`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportGroupDesc {
    /// Optional `01`-level group identifier (used by `GENERATE detail-name`, `UPON`, and `USE BEFORE REPORTING`).
    pub name: Option<String>,
    /// `TYPE IS ...` classification of this report group.
    pub kind: ReportGroupKind,
    /// Optional `LINE NUMBER IS ...` declared directly on the `01` entry.
    pub line: Option<LinePositionSpec>,
    /// Optional `NEXT GROUP IS ...` post-presentation spacing clause.
    pub next_group: Option<NextGroupSpec>,
    /// Optional `PRESENT WHEN` visibility guard on the entire `01` group.
    pub present_when: PresentWhenSpec,
    /// Ordered hierarchical child nodes.
    pub children: Vec<ReportNode>,
}

/// Flat `01`..`49` `REPORT SECTION` entry used by [`build_group_from_flat_entries`] to assemble a [`ReportGroupDesc`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatReportEntry {
    /// Validated COBOL [`LevelNumber`] (`01` for root, `02`..`49` for descendants).
    pub level: LevelNumber,
    /// Optional entry identifier (`None` for `FILLER` / unnamed entries).
    pub name: Option<String>,
    /// `TYPE IS ...` report group kind (required on level `01`, `None` on `02`..`49`).
    pub kind: Option<ReportGroupKind>,
    /// Optional `LINE NUMBER IS ...` clause.
    pub line: Option<LinePositionSpec>,
    /// Optional `NEXT GROUP IS ...` clause (`01` only).
    pub next_group: Option<NextGroupSpec>,
    /// Optional `COLUMN NUMBER IS ...` clause (elementary entries only).
    pub column: Option<ColumnPositionSpec>,
    /// Optional [`ItemValueSource`] (elementary entries only).
    pub source: Option<ItemValueSource>,
    /// Formatter for elementary entries.
    pub formatter: FieldFormatter,
    /// `GROUP INDICATE` policy for elementary entries.
    pub group_indicate: GroupIndicatePolicy,
    /// `PRESENT WHEN` guard for this entry.
    pub present_when: PresentWhenSpec,
}

/// Assembles a hierarchical [`ReportGroupDesc`] from a flat `01`..`49` entry sequence.
///
/// # Arguments
///
/// * `entries` - Contiguous `01`..`49` entries starting with a single `01` report group root.
/// * `report_name` - Owning `RD` name for diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::ReportEngine`] if `entries` is empty, does not begin at level `01` with a
/// [`ReportGroupKind`], uses non-hierarchy levels (`66`/`77`/`78`/`88`), or places elementary clauses on group nodes.
pub fn build_group_from_flat_entries(
    entries: &[FlatReportEntry],
    report_name: &str,
) -> Result<ReportGroupDesc> {
    let Some(root) = entries.first() else {
        return Err(report_err(
            report_name,
            "cannot build report group from empty entry slice",
        ));
    };
    if root.level != LevelNumber::Hierarchy(1) {
        return Err(report_err(
            report_name,
            &format!(
                "report group must start at level 01, found {:02}",
                root.level.as_u8()
            ),
        ));
    }
    let Some(kind) = root.kind else {
        return Err(report_err(
            report_name,
            "level 01 report group entry requires a TYPE IS classification",
        ));
    };

    let tail = entries.get(1..).unwrap_or(&[]);
    if tail.is_empty() {
        let mut children = Vec::new();
        if root.column.is_some()
            || root.source.is_some()
            || root.group_indicate == GroupIndicatePolicy::GroupIndicate
        {
            children.push(ReportNode::Elementary(ReportFieldItem {
                name: root.name.clone(),
                line: None,
                column: root.column,
                source: root.source.clone(),
                formatter: root.formatter.clone(),
                group_indicate: root.group_indicate,
                present_when: PresentWhenSpec::Always,
            }));
        }
        return Ok(ReportGroupDesc {
            name: root.name.clone(),
            kind,
            line: root.line,
            next_group: root.next_group,
            present_when: root.present_when.clone(),
            children,
        });
    }

    if root.column.is_some()
        || root.source.is_some()
        || root.group_indicate == GroupIndicatePolicy::GroupIndicate
    {
        return Err(report_err(
            report_name,
            "non-elementary 01 report group cannot specify COLUMN, SOURCE/VALUE/SUM, or GROUP INDICATE",
        ));
    }

    for entry in tail {
        let LevelNumber::Hierarchy(lvl) = entry.level else {
            return Err(report_err(
                report_name,
                &format!(
                    "invalid level {:02} inside REPORT SECTION group",
                    entry.level.as_u8()
                ),
            ));
        };
        if lvl <= 1 {
            return Err(report_err(
                report_name,
                "nested report entry must have hierarchy level 02..=49",
            ));
        }
        if entry.kind.is_some() || entry.next_group.is_some() {
            return Err(report_err(
                report_name,
                "TYPE IS and NEXT GROUP clauses are only permitted at level 01",
            ));
        }
    }

    let children = build_child_nodes(tail, report_name)?;
    Ok(ReportGroupDesc {
        name: root.name.clone(),
        kind,
        line: root.line,
        next_group: root.next_group,
        present_when: root.present_when.clone(),
        children,
    })
}

/// Recursively partitions a validated `02`..`49` slice into [`ReportNode`] trees.
///
/// # Arguments
///
/// * `slice` - Contiguous descendant entries with hierarchy levels strictly greater than their enclosing parent.
/// * `report_name` - Owning `RD` name for diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::ReportEngine`] if a sub-group specifies elementary-only clauses (`COLUMN`, `SOURCE`, `GROUP INDICATE`).
fn build_child_nodes(slice: &[FlatReportEntry], report_name: &str) -> Result<Vec<ReportNode>> {
    let mut nodes = Vec::new();
    let mut idx = 0_usize;
    while let Some(cur) = slice.get(idx) {
        let cur_lvl = cur.level.as_u8();
        let mut end = idx.saturating_add(1);
        while let Some(next_entry) = slice.get(end) {
            if next_entry.level.as_u8() > cur_lvl {
                end = end.saturating_add(1);
            } else {
                break;
            }
        }
        let descendants = slice.get(idx.saturating_add(1)..end).unwrap_or(&[]);
        if descendants.is_empty() {
            nodes.push(ReportNode::Elementary(ReportFieldItem {
                name: cur.name.clone(),
                line: cur.line,
                column: cur.column,
                source: cur.source.clone(),
                formatter: cur.formatter.clone(),
                group_indicate: cur.group_indicate,
                present_when: cur.present_when.clone(),
            }));
        } else {
            if cur.column.is_some()
                || cur.source.is_some()
                || cur.group_indicate == GroupIndicatePolicy::GroupIndicate
            {
                return Err(report_err(
                    report_name,
                    "group entry with subordinate items cannot specify COLUMN, SOURCE/VALUE/SUM, or GROUP INDICATE",
                ));
            }
            let sub_children = build_child_nodes(descendants, report_name)?;
            nodes.push(ReportNode::SubGroup(ReportSubGroup {
                name: cur.name.clone(),
                line: cur.line,
                present_when: cur.present_when.clone(),
                children: sub_children,
            }));
        }
        idx = end;
    }
    Ok(nodes)
}

/// Complete declarative `RD` (Report Description) schema consumed by `ReportState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportDesc {
    /// Canonical `RD` report identifier.
    pub report_name: String,
    /// Optional `CODE IS literal` prefix prepended to every physical report record.
    pub code_prefix: Vec<u8>,
    /// Optional `PAGE LIMIT` vertical zone boundaries (`None` denotes an unpaged continuous report).
    pub page_limits: Option<PageLimits>,
    /// Ordered major-to-minor `CONTROLS ARE [FINAL] c1 c2 ...` definitions.
    pub controls: Vec<ControlDef>,
    /// Ordered `01`-level report groups (`RH`, `PH`, `CH`, `DE`, `CF`, `PF`, `RF`).
    pub groups: Vec<ReportGroupDesc>,
    /// Declarative `SUM` accumulators indexed by [`SumCounterId`].
    pub sum_counters: Vec<SumCounterDesc>,
}

impl ReportDesc {
    /// Validates all ISO/ANSI COBOL structural invariants of this `RD` descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ReportEngine`] if control levels, page zones, group multiplicities,
    /// `NEXT GROUP`, `GROUP INDICATE`, `LINE`, or `SUM` invariants are violated.
    pub fn validate(&self) -> Result<()> {
        if self.report_name.trim().is_empty() {
            return Err(report_err("<unnamed>", "RD report_name must not be empty"));
        }
        self.validate_controls()?;
        self.validate_groups()?;
        self.validate_sum_counters()?;
        Ok(())
    }

    /// Validates `CONTROLS ARE` uniqueness, non-empty names, and `FINAL` placement at index `0`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ReportEngine`] if `FINAL` appears after index `0` or names are empty/duplicated.
    fn validate_controls(&self) -> Result<()> {
        let mut seen_names = BTreeSet::new();
        let mut seen_fields = BTreeSet::new();
        for (idx, ctrl) in self.controls.iter().enumerate() {
            let trimmed = ctrl.name.trim();
            if trimmed.is_empty() {
                return Err(report_err(
                    &self.report_name,
                    "control hierarchy entry name must not be empty",
                ));
            }
            if ctrl.kind == ControlKind::Final && idx != 0 {
                return Err(report_err(
                    &self.report_name,
                    "FINAL control must appear first (most major) in CONTROLS ARE",
                ));
            }
            let upper = trimmed.to_ascii_uppercase();
            if !seen_names.insert(upper) {
                return Err(report_err(
                    &self.report_name,
                    &format!("duplicate control name `{trimmed}` in CONTROLS ARE"),
                ));
            }
            if let ControlKind::DataField(fid) = ctrl.kind
                && !seen_fields.insert(fid)
            {
                return Err(report_err(
                    &self.report_name,
                    &format!(
                        "duplicate control FieldId({}) in CONTROLS ARE",
                        fid.as_u32()
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Validates all `01` report groups, singleton headings/footings, control references, and line specs.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ReportEngine`] if any group violates ISO COBOL `RD` rules.
    fn validate_groups(&self) -> Result<()> {
        let mut report_heading_count = 0_usize;
        let mut page_heading_count = 0_usize;
        let mut page_footing_count = 0_usize;
        let mut report_footing_count = 0_usize;
        let mut detail_names = BTreeSet::new();

        for group in &self.groups {
            match group.kind {
                ReportGroupKind::ReportHeading => {
                    report_heading_count = report_heading_count.saturating_add(1);
                }
                ReportGroupKind::PageHeading => {
                    page_heading_count = page_heading_count.saturating_add(1);
                }
                ReportGroupKind::PageFooting => {
                    page_footing_count = page_footing_count.saturating_add(1);
                }
                ReportGroupKind::ReportFooting => {
                    report_footing_count = report_footing_count.saturating_add(1);
                }
                ReportGroupKind::ControlHeading(lvl) | ReportGroupKind::ControlFooting(lvl) => {
                    if lvl.as_usize() >= self.controls.len() {
                        return Err(report_err(
                            &self.report_name,
                            &format!(
                                "{} references out-of-range ControlLevelIndex({})",
                                group.kind.short_code(),
                                lvl.as_u16()
                            ),
                        ));
                    }
                }
                ReportGroupKind::Detail => {
                    if let Some(name) = &group.name {
                        let upper = name.to_ascii_uppercase();
                        if !detail_names.insert(upper) {
                            return Err(report_err(
                                &self.report_name,
                                &format!("duplicate DETAIL group name `{name}`"),
                            ));
                        }
                    }
                }
            }

            if report_heading_count > 1
                || page_heading_count > 1
                || page_footing_count > 1
                || report_footing_count > 1
            {
                return Err(report_err(
                    &self.report_name,
                    "at most one RH, PH, PF, and RF group may be defined per RD",
                ));
            }

            if self.page_limits.is_none()
                && matches!(
                    group.kind,
                    ReportGroupKind::PageHeading | ReportGroupKind::PageFooting
                )
            {
                return Err(report_err(
                    &self.report_name,
                    "PAGE HEADING and PAGE FOOTING require PAGE LIMIT on the RD",
                ));
            }

            if let Some(ng) = group.next_group {
                self.validate_next_group_clause(group.kind, ng)?;
            }

            let mut line_state = LineWalkValidation {
                seen_first_line: false,
                has_active_line: group.line.is_some(),
            };
            if let Some(root_line) = group.line {
                self.validate_line_spec(group.kind, root_line, &mut line_state)?;
            }
            self.validate_nodes(&group.children, group.kind, &mut line_state)?;
        }
        Ok(())
    }

    /// Validates a `01`-level [`NextGroupSpec`] clause against group kind and `PAGE LIMIT` bounds.
    ///
    /// # Arguments
    ///
    /// * `kind` - Enclosing [`ReportGroupKind`].
    /// * `ng` - Declared [`NextGroupSpec`].
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ReportEngine`] if `NEXT GROUP` appears on `PH`/`PF`/`RF`, uses `NEXT PAGE`
    /// without `PAGE LIMIT`, or exceeds `PAGE LIMIT`.
    fn validate_next_group_clause(&self, kind: ReportGroupKind, ng: NextGroupSpec) -> Result<()> {
        if matches!(
            kind,
            ReportGroupKind::PageHeading
                | ReportGroupKind::PageFooting
                | ReportGroupKind::ReportFooting
        ) {
            return Err(report_err(
                &self.report_name,
                &format!("NEXT GROUP is not permitted on {} group", kind.short_code()),
            ));
        }
        if self.page_limits.is_none() && ng == NextGroupSpec::NextPage {
            return Err(report_err(
                &self.report_name,
                "NEXT GROUP NEXT PAGE requires PAGE LIMIT on the RD",
            ));
        }
        if let Some(limits) = self.page_limits
            && let NextGroupSpec::Absolute(target) = ng
            && target > limits.page_limit()
        {
            return Err(report_err(
                &self.report_name,
                "NEXT GROUP absolute line exceeds PAGE LIMIT",
            ));
        }
        Ok(())
    }

    /// Validates a single [`LinePositionSpec`] against page limits and first-line `ON NEXT PAGE` rules.
    ///
    /// # Arguments
    ///
    /// * `kind` - Enclosing [`ReportGroupKind`].
    /// * `spec` - Line positioning clause to validate.
    /// * `state` - Mutable walk state tracking whether a prior `LINE` clause already appeared in this group.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ReportEngine`] on zero lines, unpaged `ON NEXT PAGE`, non-first `ON NEXT PAGE`, or out-of-range page lines.
    fn validate_line_spec(
        &self,
        kind: ReportGroupKind,
        spec: LinePositionSpec,
        state: &mut LineWalkValidation,
    ) -> Result<()> {
        match spec {
            LinePositionSpec::Absolute(line) => {
                if line == 0 {
                    return Err(report_err(&self.report_name, "LINE NUMBER must be >= 1"));
                }
                if let Some(limits) = self.page_limits
                    && line > limits.page_limit()
                {
                    return Err(report_err(
                        &self.report_name,
                        &format!("LINE {line} exceeds PAGE LIMIT {}", limits.page_limit()),
                    ));
                }
            }
            LinePositionSpec::OnNextPage(line) => {
                if line == 0 {
                    return Err(report_err(
                        &self.report_name,
                        "ON NEXT PAGE line number must be >= 1",
                    ));
                }
                let Some(limits) = self.page_limits else {
                    return Err(report_err(
                        &self.report_name,
                        "LINE ... ON NEXT PAGE requires PAGE LIMIT on the RD",
                    ));
                };
                if line > limits.page_limit() {
                    return Err(report_err(
                        &self.report_name,
                        &format!(
                            "LINE {line} ON NEXT PAGE exceeds PAGE LIMIT {}",
                            limits.page_limit()
                        ),
                    ));
                }
                if state.seen_first_line {
                    return Err(report_err(
                        &self.report_name,
                        "ON NEXT PAGE may only appear on the first LINE clause of a report group",
                    ));
                }
                if matches!(
                    kind,
                    ReportGroupKind::PageHeading | ReportGroupKind::PageFooting
                ) {
                    return Err(report_err(
                        &self.report_name,
                        "ON NEXT PAGE is not permitted in PAGE HEADING or PAGE FOOTING",
                    ));
                }
            }
            LinePositionSpec::Relative(_) => {}
        }
        state.seen_first_line = true;
        state.has_active_line = true;
        Ok(())
    }

    /// Recursively validates child [`ReportNode`] items inside a `01` report group.
    ///
    /// # Arguments
    ///
    /// * `nodes` - Child node slice.
    /// * `kind` - Enclosing [`ReportGroupKind`].
    /// * `state` - Mutable line-tracking state.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ReportEngine`] if a printable item lacks an enclosing `LINE`, uses invalid `COLUMN 0`,
    /// uses `GROUP INDICATE` outside `DETAIL`, has zero-width formatting, or references an invalid `SumCounterId`.
    fn validate_nodes(
        &self,
        nodes: &[ReportNode],
        kind: ReportGroupKind,
        state: &mut LineWalkValidation,
    ) -> Result<()> {
        for node in nodes {
            match node {
                ReportNode::SubGroup(sub) => {
                    if let Some(l) = sub.line {
                        self.validate_line_spec(kind, l, state)?;
                    }
                    self.validate_nodes(&sub.children, kind, state)?;
                }
                ReportNode::Elementary(item) => {
                    if let Some(l) = item.line {
                        self.validate_line_spec(kind, l, state)?;
                    }
                    if item.group_indicate == GroupIndicatePolicy::GroupIndicate
                        && kind != ReportGroupKind::Detail
                    {
                        return Err(report_err(
                            &self.report_name,
                            "GROUP INDICATE is only permitted inside DETAIL report groups",
                        ));
                    }
                    if let Some(col) = item.column {
                        if !state.has_active_line {
                            return Err(report_err(
                                &self.report_name,
                                "printable COLUMN item must be governed by a preceding LINE clause",
                            ));
                        }
                        if matches!(col, ColumnPositionSpec::Absolute(0)) {
                            return Err(report_err(
                                &self.report_name,
                                "COLUMN NUMBER must be >= 1",
                            ));
                        }
                    }
                    match &item.formatter {
                        FieldFormatter::RawBytes => {}
                        FieldFormatter::FixedWidthAlpha { width, .. } => {
                            if *width == 0 {
                                return Err(report_err(
                                    &self.report_name,
                                    "FixedWidthAlpha width must be >= 1",
                                ));
                            }
                        }
                        FieldFormatter::Picture { byte_len, .. } => {
                            if *byte_len == 0 {
                                return Err(report_err(
                                    &self.report_name,
                                    "Picture formatter byte_len must be >= 1",
                                ));
                            }
                        }
                    }
                    if let Some(ItemValueSource::SumCounter(sum_id)) = item.source
                        && sum_id.as_usize() >= self.sum_counters.len()
                    {
                        return Err(report_err(
                            &self.report_name,
                            &format!(
                                "elementary item references undefined SumCounterId({})",
                                sum_id.as_u32()
                            ),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Validates all [`SumCounterDesc`] definitions (`id` alignment, `reset_level`, `addends`, and `UPON` detail targets).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ReportEngine`] if any `SUM` accumulator violates control-level or acyclic rolling rules.
    fn validate_sum_counters(&self) -> Result<()> {
        let known_details: BTreeSet<String> = self
            .groups
            .iter()
            .filter(|g| g.kind == ReportGroupKind::Detail)
            .filter_map(|g| g.name.as_ref().map(|s| s.to_ascii_uppercase()))
            .collect();

        for (expected_idx, sum) in self.sum_counters.iter().enumerate() {
            if sum.id.as_usize() != expected_idx {
                return Err(report_err(
                    &self.report_name,
                    &format!(
                        "SumCounterId({}) does not match slot index {expected_idx}",
                        sum.id.as_u32()
                    ),
                ));
            }
            if sum.owner_control_level.as_usize() >= self.controls.len() {
                return Err(report_err(
                    &self.report_name,
                    "SUM owner_control_level is out of range for CONTROLS ARE",
                ));
            }
            if sum.reset_level > sum.owner_control_level {
                return Err(report_err(
                    &self.report_name,
                    "SUM RESET ON control level cannot be more minor than its owning CONTROL FOOTING",
                ));
            }
            if sum.addends.is_empty() {
                return Err(report_err(
                    &self.report_name,
                    "SUM counter must declare at least one addend",
                ));
            }
            for addend in &sum.addends {
                if let SumAddend::RollingCounter(sub_id) = addend {
                    let Some(sub_desc) = self.sum_counters.get(sub_id.as_usize()) else {
                        return Err(report_err(
                            &self.report_name,
                            &format!(
                                "rolling SUM references undefined SumCounterId({})",
                                sub_id.as_u32()
                            ),
                        ));
                    };
                    if sub_desc.owner_control_level <= sum.owner_control_level {
                        return Err(report_err(
                            &self.report_name,
                            "rolling SUM must reference a strictly more minor CONTROL FOOTING counter",
                        ));
                    }
                }
            }
            if let SumUponFilter::NamedDetails(names) = &sum.upon {
                if names.is_empty() {
                    return Err(report_err(
                        &self.report_name,
                        "SUM UPON detail list must not be empty",
                    ));
                }
                for detail_name in names {
                    if !known_details.contains(&detail_name.to_ascii_uppercase()) {
                        return Err(report_err(
                            &self.report_name,
                            &format!("SUM UPON references unknown DETAIL group `{detail_name}`"),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Internal validation state while walking `LINE` clauses inside a single `01` report group.
#[derive(Debug, Clone, Copy)]
struct LineWalkValidation {
    /// Whether at least one `LINE` clause has already been encountered in the group.
    seen_first_line: bool,
    /// Whether printable `COLUMN` items currently have an active governing `LINE` clause.
    has_active_line: bool,
}

/// Constructs a [`CoblangError::ReportEngine`] error for `report_name` and `msg`.
///
/// # Arguments
///
/// * `report_name` - Owning `RD` name.
/// * `msg` - Detailed diagnostic message.
pub(crate) fn report_err(report_name: &str, msg: &str) -> CoblangError {
    CoblangError::ReportEngine {
        report_name: String::from(report_name),
        message: String::from(msg),
    }
}
