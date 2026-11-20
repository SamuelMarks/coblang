//! Declarative `ReportState` state machine executing `INITIATE`, `GENERATE`, `TERMINATE`, and `SUPPRESS PRINTING`.

#![deny(clippy::indexing_slicing)]

use std::io::Write;

use coblang_common::Result;
use coblang_decimal::FixedDecimal;
use coblang_hir::{DataCategory, JustifiedSpec};
use coblang_pic::{MoveSource, execute_move, format_field_for_display};

use crate::descriptor::{
    FieldFormatter, ItemValueSource, PresentWhenSpec, ReportDesc, ReportFieldItem, ReportGroupDesc,
    ReportNode, SumAddend, SumUponFilter, report_err,
};
use crate::env::{FieldValue, ReportEnv, ReportHookContext};
use crate::types::{
    ColumnPositionSpec, ControlKind, GenerateTarget, GroupIndicatePolicy, LinePositionSpec,
    NextGroupSpec, PageLimits, RenderFormat, ReportGroupKind, ReportLifecycleState, ReportLine,
    ReportLineAdvancing, SumCounterId, render_lines_to_bytes, write_rendered_lines,
};

/// Tracks whether at least one `GENERATE` statement has been executed since `INITIATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GenerationProgress {
    /// No `GENERATE` has occurred since `INITIATE`.
    NoneYet,
    /// One or more `GENERATE` statements have executed.
    Generated,
}

/// Tracks whether a preceding group's `NEXT GROUP` clause requested a page advance before the next body group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeferredPageBreak {
    /// No deferred page break is pending.
    None,
    /// Advance to a new page before presenting the next body group.
    Pending,
}

/// Tracks whether `PAGE HEADING` has already been presented on the current page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageHeadingStatus {
    /// `PAGE HEADING` has not yet been presented on this page.
    NotYetPresented,
    /// `PAGE HEADING` has been processed for this page.
    Presented,
}

/// Tracks whether at least one physical line has been emitted on the current page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageContentStatus {
    /// Current page has zero emitted lines so far.
    EmptyPage,
    /// Current page has at least one emitted line.
    HasEmittedLine,
}

/// Carriage-control state across the initiated report stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmissionCursorState {
    /// No lines have been emitted anywhere in the report yet.
    BeforeFirstReportLine,
    /// At least one line has been emitted on the current page.
    SamePageActive,
    /// A page transition occurred after prior output; next emitted line starts a new page.
    AwaitingNewPageFirstLine,
}

/// Whether a line-target resolution attempt is testing placement on an active page or a newly advanced page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PagePlacementContext {
    /// Attempting to place lines onto the current page (`OnNextPage` triggers a page advance).
    CurrentPage,
    /// Placing lines immediately after advancing to a fresh page (`OnNextPage` resolves as `Absolute`).
    FreshPage,
}

/// Positioning policy for the first `LINE PLUS n` clause of a group when `LINE-COUNTER` is near top-of-page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirstRelativeLineRule {
    /// Advance `cur_lc + offset` normally (`>= 1`).
    StandardAdvance,
    /// Enforce a minimum `HEADING` floor (`max(cur_lc + offset, heading)`) when `cur_lc < heading`.
    HeadingFloor(u32),
    /// Snap the first relative body line directly onto `FIRST DETAIL` whenever `cur_lc < first_detail`.
    SnapToFirstDetail(u32),
}

/// Flattened print line collected from a visible `01` report group node walk.
#[derive(Debug, Clone)]
struct PendingPrintLine {
    /// Governing `LINE NUMBER IS ...` specification for this line.
    line_spec: LinePositionSpec,
    /// Visible `(column_spec, elementary_item)` pairs placed onto this line.
    items: Vec<(ColumnPositionSpec, ReportFieldItem)>,
}

/// Evaluates a [`PresentWhenSpec`] visibility predicate against `env`.
///
/// # Arguments
///
/// * `spec` - Declarative visibility predicate.
/// * `env` - Active [`ReportEnv`] data/condition provider.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if field decoding or condition lookup fails.
fn eval_present_when(spec: &PresentWhenSpec, env: &dyn ReportEnv) -> Result<bool> {
    match spec {
        PresentWhenSpec::Always => Ok(true),
        PresentWhenSpec::Condition(cid) => env.eval_condition(*cid),
        PresentWhenSpec::FieldNonZero(fid) => {
            Ok(!env.read_field(*fid)?.decode_decimal()?.is_zero())
        }
        PresentWhenSpec::FieldEquals { field, expected } => {
            Ok(env.read_field(*field)?.bytes == *expected)
        }
        PresentWhenSpec::Not(inner) => Ok(!eval_present_when(inner, env)?),
        PresentWhenSpec::All(children) => {
            for child in children {
                if !eval_present_when(child, env)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        PresentWhenSpec::Any(children) => {
            for child in children {
                if eval_present_when(child, env)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

/// Resolves candidate 1-based target line numbers for `lines` starting from `start_lc`.
///
/// Returns `None` if an `OnNextPage` is encountered in [`PagePlacementContext::CurrentPage`] or if an
/// `Absolute(n)` line is `<= cur_lc`.
///
/// # Arguments
///
/// * `lines` - Non-empty slice of [`PendingPrintLine`]s to position.
/// * `start_lc` - Current `LINE-COUNTER` value on the candidate page.
/// * `first_rel_rule` - Top-of-page rule applied when line `0` is `LinePositionSpec::Relative`.
/// * `placement` - [`PagePlacementContext`] distinguishing current-page fit tests from fresh-page placement.
#[must_use]
fn resolve_line_targets(
    lines: &[PendingPrintLine],
    start_lc: u32,
    first_rel_rule: FirstRelativeLineRule,
    placement: PagePlacementContext,
) -> Option<Vec<u32>> {
    let mut cur_lc = start_lc;
    let mut out = Vec::with_capacity(lines.len());
    for (idx, pl) in lines.iter().enumerate() {
        match pl.line_spec {
            LinePositionSpec::OnNextPage(line_num) => {
                if placement == PagePlacementContext::CurrentPage {
                    return None;
                }
                cur_lc = line_num;
                out.push(cur_lc);
            }
            LinePositionSpec::Absolute(line_num) => {
                if line_num <= cur_lc {
                    return None;
                }
                cur_lc = line_num;
                out.push(cur_lc);
            }
            LinePositionSpec::Relative(offset) => {
                if idx == 0 {
                    cur_lc = match first_rel_rule {
                        FirstRelativeLineRule::StandardAdvance => {
                            cur_lc.saturating_add(offset).max(1)
                        }
                        FirstRelativeLineRule::HeadingFloor(heading) => {
                            cur_lc.saturating_add(offset).max(heading)
                        }
                        FirstRelativeLineRule::SnapToFirstDetail(fd) => {
                            if cur_lc < fd {
                                fd
                            } else {
                                cur_lc.saturating_add(offset).max(1)
                            }
                        }
                    };
                } else {
                    cur_lc = cur_lc.saturating_add(offset).max(1);
                }
                out.push(cur_lc);
            }
        }
    }
    Some(out)
}

/// Pads or truncates `raw` to `width` bytes according to `justified` using `space_byte`.
///
/// # Arguments
///
/// * `raw` - Unformatted text bytes.
/// * `width` - Target fixed byte width (`>= 1`).
/// * `justified` - Left vs right justification policy.
/// * `space_byte` - Padding space byte (`0x20` or `0x40`).
#[must_use]
fn pad_or_truncate_alpha(
    raw: &[u8],
    width: usize,
    justified: JustifiedSpec,
    space_byte: u8,
) -> Vec<u8> {
    match justified {
        JustifiedSpec::Left => {
            if raw.len() >= width {
                raw.get(..width).unwrap_or(raw).to_vec()
            } else {
                let mut out = Vec::with_capacity(width);
                out.extend_from_slice(raw);
                out.resize(width, space_byte);
                out
            }
        }
        JustifiedSpec::Right => {
            if raw.len() >= width {
                let start = raw.len().saturating_sub(width);
                raw.get(start..).unwrap_or(raw).to_vec()
            } else {
                let pad = width.saturating_sub(raw.len());
                let mut out = vec![space_byte; pad];
                out.extend_from_slice(raw);
                out
            }
        }
    }
}

/// Pure-Rust ISO/ANSI COBOL Report Writer Control System (`RWCS`) state machine for a single `RD`.
#[derive(Debug, Clone)]
pub struct ReportState {
    /// Validated `RD` report descriptor.
    desc: ReportDesc,
    /// Current lifecycle state (`Inactive` vs `Active`).
    lifecycle: ReportLifecycleState,
    /// Tracks whether at least one `GENERATE` has executed since `INITIATE`.
    generation_progress: GenerationProgress,
    /// Current `PAGE-COUNTER` special register.
    page_counter: u32,
    /// Current `LINE-COUNTER` special register.
    line_counter: u32,
    /// Whether `SUPPRESS PRINTING` is active for the group currently in `USE BEFORE REPORTING`.
    suppress_current_group: bool,
    /// Deferred page-break flag set by `NEXT GROUP NEXT PAGE` or zone-overflowing `NEXT GROUP`.
    deferred_page_break: DeferredPageBreak,
    /// Whether `PAGE HEADING` has already been presented on the current page.
    page_heading_status: PageHeadingStatus,
    /// Whether any line has been emitted on the current page.
    page_content: PageContentStatus,
    /// Stream carriage-control cursor state across pages.
    emission_cursor: EmissionCursorState,
    /// Saved control-field byte snapshots from the previous `GENERATE` (`CONTROLS ARE` order).
    prior_controls: Vec<Vec<u8>>,
    /// Live `SUM` accumulator values indexed by `SumCounterId`.
    sum_values: Vec<FixedDecimal>,
    /// Per-group `GROUP INDICATE` unsuppressed flags (`true` after `INITIATE`, control break, or page advance).
    group_indicate_ready: Vec<bool>,
    /// Ordered log of all physical [`ReportLine`]s emitted during the active session.
    emitted_lines: Vec<ReportLine>,
}

impl ReportState {
    /// Validates `desc` and constructs a fresh `Inactive` [`ReportState`].
    ///
    /// # Arguments
    ///
    /// * `desc` - Declarative [`ReportDesc`] schema for the `RD`.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError::ReportEngine`] if `desc.validate()` fails.
    pub fn try_new(desc: ReportDesc) -> Result<Self> {
        desc.validate()?;
        let sum_len = desc.sum_counters.len();
        let grp_len = desc.groups.len();
        Ok(Self {
            desc,
            lifecycle: ReportLifecycleState::Inactive,
            generation_progress: GenerationProgress::NoneYet,
            page_counter: 0,
            line_counter: 0,
            suppress_current_group: false,
            deferred_page_break: DeferredPageBreak::None,
            page_heading_status: PageHeadingStatus::NotYetPresented,
            page_content: PageContentStatus::EmptyPage,
            emission_cursor: EmissionCursorState::BeforeFirstReportLine,
            prior_controls: Vec::new(),
            sum_values: vec![FixedDecimal::ZERO; sum_len],
            group_indicate_ready: vec![true; grp_len],
            emitted_lines: Vec::new(),
        })
    }

    /// Returns a reference to the underlying [`ReportDesc`].
    #[must_use]
    pub const fn descriptor(&self) -> &ReportDesc {
        &self.desc
    }

    /// Returns the current [`ReportLifecycleState`] (`Inactive` or `Active`).
    #[must_use]
    pub const fn lifecycle(&self) -> ReportLifecycleState {
        self.lifecycle
    }

    /// Returns the current `PAGE-COUNTER` special register value.
    #[must_use]
    pub const fn page_counter(&self) -> u32 {
        self.page_counter
    }

    /// Updates the `PAGE-COUNTER` special register value.
    ///
    /// # Arguments
    ///
    /// * `value` - New `PAGE-COUNTER` integer value.
    pub fn set_page_counter(&mut self, value: u32) {
        self.page_counter = value;
    }

    /// Returns the current `LINE-COUNTER` special register value.
    #[must_use]
    pub const fn line_counter(&self) -> u32 {
        self.line_counter
    }

    /// Reads the current exact [`FixedDecimal`] value of `SUM` accumulator `id`.
    ///
    /// # Arguments
    ///
    /// * `id` - Target [`SumCounterId`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError::ReportEngine`] if `id` is out of range.
    pub fn sum_value(&self, id: SumCounterId) -> Result<FixedDecimal> {
        let Some(&val) = self.sum_values.get(id.as_usize()) else {
            return Err(report_err(
                &self.desc.report_name,
                "out-of-range SumCounterId",
            ));
        };
        Ok(val)
    }

    /// Returns all [`ReportLine`]s emitted since the last `INITIATE` (or last [`Self::take_emitted_lines`]).
    #[must_use]
    pub fn emitted_lines(&self) -> &[ReportLine] {
        &self.emitted_lines
    }

    /// Drains and returns all buffered [`ReportLine`]s emitted so far.
    pub fn take_emitted_lines(&mut self) -> Vec<ReportLine> {
        core::mem::take(&mut self.emitted_lines)
    }

    /// Renders all buffered [`ReportLine`]s into a flat byte vector using `format`.
    ///
    /// # Arguments
    ///
    /// * `format` - Output serialization [`RenderFormat`].
    #[must_use]
    pub fn render_output(&self, format: RenderFormat) -> Vec<u8> {
        render_lines_to_bytes(&self.emitted_lines, format)
    }

    /// Writes all buffered [`ReportLine`]s formatted with `format` into `writer`.
    ///
    /// # Arguments
    ///
    /// * `format` - Output serialization [`RenderFormat`].
    /// * `writer` - Destination byte stream.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError::Io`] if writing or flushing `writer` fails.
    pub fn write_output(&self, format: RenderFormat, writer: &mut dyn Write) -> Result<()> {
        write_rendered_lines(&self.emitted_lines, format, writer)
    }

    /// Executes `SUPPRESS PRINTING` on an active report.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError::ReportEngine`] if the report is currently `Inactive`.
    pub fn suppress_printing(&mut self) -> Result<()> {
        if self.lifecycle != ReportLifecycleState::Active {
            return Err(report_err(
                &self.desc.report_name,
                "cannot execute SUPPRESS PRINTING when report is inactive",
            ));
        }
        self.suppress_current_group = true;
        Ok(())
    }

    /// Executes `INITIATE report-name`, initializing `PAGE-COUNTER = 1`, `LINE-COUNTER = 0`, and zeroing all `SUM` counters.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError::ReportEngine`] if the report is already `Active`.
    pub fn initiate(&mut self) -> Result<()> {
        if self.lifecycle == ReportLifecycleState::Active {
            return Err(report_err(
                &self.desc.report_name,
                "cannot INITIATE an already active report (EC-REPORT-ACTIVE)",
            ));
        }
        self.lifecycle = ReportLifecycleState::Active;
        self.generation_progress = GenerationProgress::NoneYet;
        self.page_counter = 1;
        self.line_counter = 0;
        self.suppress_current_group = false;
        self.deferred_page_break = DeferredPageBreak::None;
        self.page_heading_status = PageHeadingStatus::NotYetPresented;
        self.page_content = PageContentStatus::EmptyPage;
        self.emission_cursor = EmissionCursorState::BeforeFirstReportLine;
        self.prior_controls.clear();
        self.sum_values.fill(FixedDecimal::ZERO);
        self.group_indicate_ready.fill(true);
        self.emitted_lines.clear();
        Ok(())
    }

    /// Executes `GENERATE {detail-group | report-name}` against `env`.
    ///
    /// # Arguments
    ///
    /// * `target` - [`GenerateTarget`] (`Summary`, `DetailIndex`, or `DetailName`).
    /// * `env` - Mutable [`ReportEnv`] providing field values, conditions, and declarative hooks.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if the report is inactive, targets an invalid/non-`DETAIL`
    /// group, or encounters a field/codec/layout error.
    pub fn generate(&mut self, target: &GenerateTarget, env: &mut dyn ReportEnv) -> Result<()> {
        if self.lifecycle != ReportLifecycleState::Active {
            return Err(report_err(
                &self.desc.report_name,
                "cannot execute GENERATE on an inactive report (EC-REPORT-INACTIVE)",
            ));
        }

        let resolved_detail = self.resolve_generate_target(target)?;
        let current_controls = self.snapshot_controls(env)?;

        if self.generation_progress == GenerationProgress::NoneYet {
            self.generation_progress = GenerationProgress::Generated;
            self.prior_controls.clone_from(&current_controls);
            self.present_report_heading_if_present(env)?;
            for lvl_idx in 0..self.desc.controls.len() {
                self.present_control_headings_at(lvl_idx, env)?;
            }
            self.group_indicate_ready.fill(true);
        } else if let Some(break_idx) = self.detect_control_break(&current_controls) {
            let saved_prior = self.prior_controls.clone();
            self.restore_controls_in_env(&saved_prior, env)?;
            for lvl_idx in (break_idx..self.desc.controls.len()).rev() {
                self.present_control_footings_and_reset_at(lvl_idx, env)?;
            }
            self.restore_controls_in_env(&current_controls, env)?;
            self.prior_controls = current_controls;
            self.group_indicate_ready.fill(true);
            for lvl_idx in break_idx..self.desc.controls.len() {
                self.present_control_headings_at(lvl_idx, env)?;
            }
        }

        self.accumulate_generate_sums(resolved_detail.as_ref().map(|(i, _)| *i), env)?;

        if let Some((detail_idx, detail_group)) = resolved_detail {
            self.present_body_group(detail_idx, &detail_group, env)?;
        }

        Ok(())
    }

    /// Executes `TERMINATE report-name` against `env`.
    ///
    /// # Arguments
    ///
    /// * `env` - Mutable [`ReportEnv`] providing field values, conditions, and declarative hooks.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if the report is inactive or encounters a runtime hook/formatting error.
    pub fn terminate(&mut self, env: &mut dyn ReportEnv) -> Result<()> {
        if self.lifecycle != ReportLifecycleState::Active {
            return Err(report_err(
                &self.desc.report_name,
                "cannot execute TERMINATE on an inactive report (EC-REPORT-INACTIVE)",
            ));
        }

        if self.generation_progress == GenerationProgress::NoneYet {
            self.lifecycle = ReportLifecycleState::Inactive;
            return Ok(());
        }

        let caller_controls = self.snapshot_controls(env)?;
        let final_prior = self.prior_controls.clone();
        self.restore_controls_in_env(&final_prior, env)?;

        for lvl_idx in (0..self.desc.controls.len()).rev() {
            self.present_control_footings_and_reset_at(lvl_idx, env)?;
        }

        self.present_final_page_and_report_footing(env)?;
        self.restore_controls_in_env(&caller_controls, env)?;
        self.lifecycle = ReportLifecycleState::Inactive;
        Ok(())
    }

    /// Looks up a singleton group (`RH`, `PH`, `PF`, or `RF`) returning its index and cloned descriptor.
    ///
    /// # Arguments
    ///
    /// * `kind` - Target [`ReportGroupKind`] to find.
    #[must_use]
    fn find_group_by_kind(&self, kind: ReportGroupKind) -> Option<(usize, ReportGroupDesc)> {
        self.desc
            .groups
            .iter()
            .enumerate()
            .find(|(_, g)| g.kind == kind)
            .map(|(i, g)| (i, g.clone()))
    }

    /// Evaluates root `PRESENT WHEN`, runs `USE BEFORE REPORTING`, and collects visible [`PendingPrintLine`]s.
    ///
    /// Returns `Ok(None)` when the group is hidden by `PRESENT WHEN` or suppressed via `SUPPRESS PRINTING`.
    ///
    /// # Arguments
    ///
    /// * `group` - Report group candidate.
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if predicate evaluation or the declarative hook fails.
    fn prepare_group_lines(
        &mut self,
        group: &ReportGroupDesc,
        env: &mut dyn ReportEnv,
    ) -> Result<Option<Vec<PendingPrintLine>>> {
        if !eval_present_when(&group.present_when, env)? {
            return Ok(None);
        }
        if self.run_before_reporting_hook(group, env)? {
            return Ok(None);
        }
        let lines = collect_visible_lines(group, env)?;
        if lines.is_empty() {
            return Ok(None);
        }
        Ok(Some(lines))
    }

    /// Resolves a [`GenerateTarget`] into an optional `(group_index, ReportGroupDesc)` `DETAIL` pair.
    ///
    /// # Arguments
    ///
    /// * `target` - Target passed to [`Self::generate`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError::ReportEngine`] if the target index/name does not denote a `DETAIL` group.
    fn resolve_generate_target(
        &self,
        target: &GenerateTarget,
    ) -> Result<Option<(usize, ReportGroupDesc)>> {
        match target {
            GenerateTarget::Summary => Ok(None),
            GenerateTarget::DetailIndex(idx) => {
                let Some(grp) = self.desc.groups.get(*idx) else {
                    return Err(report_err(
                        &self.desc.report_name,
                        &format!("DETAIL group index {idx} is out of bounds"),
                    ));
                };
                if grp.kind != ReportGroupKind::Detail {
                    return Err(report_err(
                        &self.desc.report_name,
                        &format!(
                            "GENERATE DetailIndex({idx}) targets a {} group instead of DETAIL",
                            grp.kind.short_code()
                        ),
                    ));
                }
                Ok(Some((*idx, grp.clone())))
            }
            GenerateTarget::DetailName(name) => {
                for (idx, grp) in self.desc.groups.iter().enumerate() {
                    if grp.kind == ReportGroupKind::Detail
                        && grp
                            .name
                            .as_deref()
                            .is_some_and(|g_name| g_name.eq_ignore_ascii_case(name))
                    {
                        return Ok(Some((idx, grp.clone())));
                    }
                }
                Err(report_err(
                    &self.desc.report_name,
                    &format!("unknown DETAIL group `{name}` in GENERATE"),
                ))
            }
        }
    }

    /// Reads the current raw storage bytes of every entry in `self.desc.controls`.
    ///
    /// # Arguments
    ///
    /// * `env` - Active [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if reading a `DataField` control fails.
    fn snapshot_controls(&self, env: &dyn ReportEnv) -> Result<Vec<Vec<u8>>> {
        let mut snap = Vec::with_capacity(self.desc.controls.len());
        for ctrl in &self.desc.controls {
            match ctrl.kind {
                ControlKind::Final => snap.push(Vec::new()),
                ControlKind::DataField(fid) => snap.push(env.read_field(fid)?.bytes),
            }
        }
        Ok(snap)
    }

    /// Writes `snapshot` bytes back into all `DataField` controls in `env`.
    ///
    /// # Arguments
    ///
    /// * `snapshot` - Saved control byte vectors aligned with `self.desc.controls`.
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if `env.write_control_bytes` fails.
    fn restore_controls_in_env(&self, snapshot: &[Vec<u8>], env: &mut dyn ReportEnv) -> Result<()> {
        for (ctrl, bytes) in self.desc.controls.iter().zip(snapshot.iter()) {
            if let ControlKind::DataField(fid) = ctrl.kind {
                env.write_control_bytes(fid, bytes)?;
            }
        }
        Ok(())
    }

    /// Compares `current` control values against `self.prior_controls` from major to minor,
    /// returning the most major breaking control index if any `DataField` changed.
    ///
    /// # Arguments
    ///
    /// * `current` - Newly snapshotted control byte vectors.
    #[must_use]
    fn detect_control_break(&self, current: &[Vec<u8>]) -> Option<usize> {
        for (idx, ((ctrl, cur), prev)) in self
            .desc
            .controls
            .iter()
            .zip(current.iter())
            .zip(self.prior_controls.iter())
            .enumerate()
        {
            if matches!(ctrl.kind, ControlKind::DataField(_)) && cur != prev {
                return Some(idx);
            }
        }
        None
    }

    /// Augments `SUM` accumulators from `SourceField` addends matching `detail_idx` during `GENERATE`.
    ///
    /// # Arguments
    ///
    /// * `detail_idx` - Optional active `DETAIL` group index (`None` for summary `GENERATE report-name`).
    /// * `env` - Active [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if numeric field decoding or decimal addition fails.
    fn accumulate_generate_sums(
        &mut self,
        detail_idx: Option<usize>,
        env: &dyn ReportEnv,
    ) -> Result<()> {
        let active_detail_name = detail_idx
            .and_then(|idx| self.desc.groups.get(idx))
            .and_then(|g| g.name.as_deref());

        for (counter_desc, acc_slot) in self
            .desc
            .sum_counters
            .iter()
            .zip(self.sum_values.iter_mut())
        {
            let matches_upon = match &counter_desc.upon {
                SumUponFilter::AllGenerations => true,
                SumUponFilter::NamedDetails(allowed) => active_detail_name.is_some_and(|d_name| {
                    allowed.iter().any(|cand| cand.eq_ignore_ascii_case(d_name))
                }),
            };
            if !matches_upon {
                continue;
            }
            for addend in &counter_desc.addends {
                if let SumAddend::SourceField(fid) = addend {
                    let delta = env.read_field(*fid)?.decode_decimal()?;
                    let (updated, _) = acc_slot.checked_add(&delta)?;
                    *acc_slot = updated;
                }
            }
        }
        Ok(())
    }

    /// Presents all `CONTROL HEADING` groups bound to `lvl_idx` in declaration order.
    ///
    /// # Arguments
    ///
    /// * `lvl_idx` - Zero-based control hierarchy index.
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if presenting any matching `CH` group fails.
    fn present_control_headings_at(
        &mut self,
        lvl_idx: usize,
        env: &mut dyn ReportEnv,
    ) -> Result<()> {
        let matching: Vec<(usize, ReportGroupDesc)> = self
            .desc
            .groups
            .iter()
            .enumerate()
            .filter_map(|(i, g)| match g.kind {
                ReportGroupKind::ControlHeading(c) if c.as_usize() == lvl_idx => {
                    Some((i, g.clone()))
                }
                _ => None,
            })
            .collect();
        for (grp_idx, group) in matching {
            self.present_body_group(grp_idx, &group, env)?;
        }
        Ok(())
    }

    /// Presents all `CONTROL FOOTING` groups bound to `lvl_idx`, rolls subordinate `SUM` counters into
    /// higher-level counters, and resets all `SUM` counters whose `reset_level == lvl_idx`.
    ///
    /// # Arguments
    ///
    /// * `lvl_idx` - Zero-based control hierarchy index being closed.
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if presenting a `CF` group or rolling `SUM` addition fails.
    fn present_control_footings_and_reset_at(
        &mut self,
        lvl_idx: usize,
        env: &mut dyn ReportEnv,
    ) -> Result<()> {
        let matching: Vec<(usize, ReportGroupDesc)> = self
            .desc
            .groups
            .iter()
            .enumerate()
            .filter_map(|(i, g)| match g.kind {
                ReportGroupKind::ControlFooting(c) if c.as_usize() == lvl_idx => {
                    Some((i, g.clone()))
                }
                _ => None,
            })
            .collect();
        for (grp_idx, group) in matching {
            self.present_body_group(grp_idx, &group, env)?;
        }

        // Roll forward any subordinate SUM counters owned by `lvl_idx` into higher-level rolling counters
        // before resetting counters at `lvl_idx`.
        let snapshot_sums = self.sum_values.clone();
        for (counter_desc, acc_slot) in self
            .desc
            .sum_counters
            .iter()
            .zip(self.sum_values.iter_mut())
        {
            for addend in &counter_desc.addends {
                if let SumAddend::RollingCounter(sub_id) = addend
                    && self
                        .desc
                        .sum_counters
                        .get(sub_id.as_usize())
                        .is_some_and(|sub_desc| sub_desc.owner_control_level.as_usize() == lvl_idx)
                {
                    let sub_val = snapshot_sums
                        .get(sub_id.as_usize())
                        .copied()
                        .unwrap_or(FixedDecimal::ZERO);
                    let (updated, _) = acc_slot.checked_add(&sub_val)?;
                    *acc_slot = updated;
                }
            }
        }

        // Reset all SUM counters configured with `RESET ON lvl_idx`.
        for (counter_desc, acc_slot) in self
            .desc
            .sum_counters
            .iter()
            .zip(self.sum_values.iter_mut())
        {
            if counter_desc.reset_level.as_usize() == lvl_idx {
                *acc_slot = FixedDecimal::ZERO;
            }
        }
        Ok(())
    }

    /// Presents `REPORT HEADING` (`RH`) on Page 1 if defined on the `RD`.
    ///
    /// # Arguments
    ///
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if hook execution, line positioning, or formatting fails.
    fn present_report_heading_if_present(&mut self, env: &mut dyn ReportEnv) -> Result<()> {
        let Some((rh_idx, group)) = self.find_group_by_kind(ReportGroupKind::ReportHeading) else {
            return Ok(());
        };
        let Some(lines) = self.prepare_group_lines(&group, env)? else {
            return Ok(());
        };
        let rel_rule = self
            .desc
            .page_limits
            .map_or(FirstRelativeLineRule::StandardAdvance, |l| {
                FirstRelativeLineRule::HeadingFloor(l.heading())
            });
        let Some(targets) = resolve_line_targets(
            &lines,
            self.line_counter,
            rel_rule,
            PagePlacementContext::FreshPage,
        ) else {
            return Err(report_err(
                &self.desc.report_name,
                "REPORT HEADING lines are non-increasing",
            ));
        };
        if let Some(limits) = self.desc.page_limits
            && targets.last().copied().unwrap_or(0) > limits.page_limit()
        {
            return Err(report_err(
                &self.desc.report_name,
                "REPORT HEADING exceeds PAGE LIMIT",
            ));
        }
        self.emit_resolved_lines(rh_idx, group.kind, &lines, &targets, env)?;

        if let Some(ng) = group.next_group {
            match ng {
                NextGroupSpec::NextPage => {
                    self.start_fresh_page_without_footing();
                }
                NextGroupSpec::Absolute(target_lc) => {
                    if target_lc > self.line_counter {
                        self.line_counter = target_lc;
                    }
                }
                NextGroupSpec::Relative(delta) => {
                    self.line_counter = self.line_counter.saturating_add(delta);
                }
            }
        }
        Ok(())
    }

    /// Presents `PAGE HEADING` (`PH`) on the current page if defined and not yet presented on this page.
    ///
    /// # Arguments
    ///
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if hook execution or line formatting fails.
    fn present_page_heading_if_present(&mut self, env: &mut dyn ReportEnv) -> Result<()> {
        if self.page_heading_status == PageHeadingStatus::Presented {
            return Ok(());
        }
        self.page_heading_status = PageHeadingStatus::Presented;
        let Some((ph_idx, group)) = self.find_group_by_kind(ReportGroupKind::PageHeading) else {
            return Ok(());
        };
        let Some(lines) = self.prepare_group_lines(&group, env)? else {
            return Ok(());
        };
        let page_limit = self
            .desc
            .page_limits
            .map_or(u32::MAX, PageLimits::page_limit);
        let rel_rule = FirstRelativeLineRule::HeadingFloor(
            self.desc.page_limits.map_or(1, PageLimits::heading),
        );
        let targets = if let Some(t) = resolve_line_targets(
            &lines,
            self.line_counter,
            rel_rule,
            PagePlacementContext::CurrentPage,
        ) {
            t
        } else {
            self.start_fresh_page_without_footing();
            self.page_heading_status = PageHeadingStatus::Presented;
            let Some(fresh_t) = resolve_line_targets(
                &lines,
                self.line_counter,
                rel_rule,
                PagePlacementContext::FreshPage,
            ) else {
                return Err(report_err(
                    &self.desc.report_name,
                    "PAGE HEADING lines are non-increasing",
                ));
            };
            fresh_t
        };
        if targets.last().copied().unwrap_or(0) > page_limit {
            return Err(report_err(
                &self.desc.report_name,
                "PAGE HEADING exceeds PAGE LIMIT",
            ));
        }
        self.emit_resolved_lines(ph_idx, group.kind, &lines, &targets, env)
    }

    /// Presents `PAGE FOOTING` (`PF`) on the current page if defined on the `RD`.
    ///
    /// # Arguments
    ///
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if `PF` lines do not fit within `PAGE LIMIT` or formatting fails.
    fn present_page_footing_if_present(&mut self, env: &mut dyn ReportEnv) -> Result<()> {
        let Some((pf_idx, group)) = self.find_group_by_kind(ReportGroupKind::PageFooting) else {
            return Ok(());
        };
        let Some(lines) = self.prepare_group_lines(&group, env)? else {
            return Ok(());
        };
        let Some(targets) = resolve_line_targets(
            &lines,
            self.line_counter,
            FirstRelativeLineRule::StandardAdvance,
            PagePlacementContext::CurrentPage,
        ) else {
            return Err(report_err(
                &self.desc.report_name,
                "PAGE FOOTING lines overlap or precede current LINE-COUNTER",
            ));
        };
        let page_limit = self
            .desc
            .page_limits
            .map_or(u32::MAX, PageLimits::page_limit);
        if targets.last().copied().unwrap_or(0) > page_limit {
            return Err(report_err(
                &self.desc.report_name,
                "PAGE FOOTING exceeds PAGE LIMIT",
            ));
        }
        self.emit_resolved_lines(pf_idx, group.kind, &lines, &targets, env)
    }

    /// Presents a body group (`CH`, `DE`, or `CF`) at `grp_idx`, managing `USE BEFORE REPORTING`,
    /// `SUPPRESS PRINTING`, `PAGE LIMIT` overflow pagination, `GROUP INDICATE`, and `NEXT GROUP`.
    ///
    /// # Arguments
    ///
    /// * `grp_idx` - Index of the body group inside `self.desc.groups`.
    /// * `group` - Cloned [`ReportGroupDesc`] for `grp_idx`.
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if the body group cannot fit even on a fresh page or formatting fails.
    fn present_body_group(
        &mut self,
        grp_idx: usize,
        group: &ReportGroupDesc,
        env: &mut dyn ReportEnv,
    ) -> Result<()> {
        let Some(lines) = self.prepare_group_lines(group, env)? else {
            return Ok(());
        };

        if let Some(limits) = self.desc.page_limits {
            let zone_ceiling = match group.kind {
                ReportGroupKind::ControlFooting(_) => limits.footing(),
                _ => limits.last_detail(),
            };

            if self.deferred_page_break == DeferredPageBreak::Pending {
                self.advance_body_page(env)?;
            } else {
                self.present_page_heading_if_present(env)?;
            }

            let body_rule = FirstRelativeLineRule::SnapToFirstDetail(limits.first_detail());
            let fits_current = resolve_line_targets(
                &lines,
                self.line_counter,
                body_rule,
                PagePlacementContext::CurrentPage,
            )
            .filter(|t| t.last().copied().unwrap_or(0) <= zone_ceiling);

            let targets = if let Some(valid_targets) = fits_current {
                valid_targets
            } else {
                self.advance_body_page(env)?;
                let Some(fresh_targets) = resolve_line_targets(
                    &lines,
                    self.line_counter,
                    body_rule,
                    PagePlacementContext::FreshPage,
                )
                .filter(|t| t.last().copied().unwrap_or(0) <= zone_ceiling) else {
                    return Err(report_err(
                        &self.desc.report_name,
                        &format!(
                            "{} group exceeds PAGE LIMIT zone ceiling {zone_ceiling} even on a fresh page",
                            group.kind.short_code()
                        ),
                    ));
                };
                fresh_targets
            };

            self.emit_resolved_lines(grp_idx, group.kind, &lines, &targets, env)?;
            if group.kind == ReportGroupKind::Detail {
                self.mark_detail_indicated(grp_idx);
            }
            if let Some(ng) = group.next_group {
                self.apply_body_next_group(ng, limits.footing());
            }
        } else {
            let Some(targets) = resolve_line_targets(
                &lines,
                self.line_counter,
                FirstRelativeLineRule::StandardAdvance,
                PagePlacementContext::CurrentPage,
            ) else {
                return Err(report_err(
                    &self.desc.report_name,
                    "non-increasing absolute LINE NUMBER in unpaged report",
                ));
            };
            self.emit_resolved_lines(grp_idx, group.kind, &lines, &targets, env)?;
            if group.kind == ReportGroupKind::Detail {
                self.mark_detail_indicated(grp_idx);
            }
            match group.next_group {
                Some(NextGroupSpec::Absolute(target_lc)) => {
                    if target_lc > self.line_counter {
                        self.line_counter = target_lc;
                    }
                }
                Some(NextGroupSpec::Relative(delta)) => {
                    self.line_counter = self.line_counter.saturating_add(delta);
                }
                Some(NextGroupSpec::NextPage) | None => {}
            }
        }
        Ok(())
    }

    /// Clears `group_indicate_ready` for `grp_idx` after a `DETAIL` group has been presented.
    ///
    /// # Arguments
    ///
    /// * `grp_idx` - Index of the presented `DETAIL` group.
    fn mark_detail_indicated(&mut self, grp_idx: usize) {
        self.group_indicate_ready
            .iter_mut()
            .skip(grp_idx)
            .take(1)
            .for_each(|flag| *flag = false);
    }

    /// Applies `NEXT GROUP` after presenting a body group on a paged report.
    ///
    /// # Arguments
    ///
    /// * `ng` - Configured [`NextGroupSpec`].
    /// * `footing_limit` - `FOOTING` line boundary of the active `PageLimits`.
    fn apply_body_next_group(&mut self, ng: NextGroupSpec, footing_limit: u32) {
        match ng {
            NextGroupSpec::NextPage => {
                self.deferred_page_break = DeferredPageBreak::Pending;
            }
            NextGroupSpec::Absolute(target_lc) => {
                if target_lc > self.line_counter && target_lc <= footing_limit {
                    self.line_counter = target_lc;
                } else {
                    self.deferred_page_break = DeferredPageBreak::Pending;
                }
            }
            NextGroupSpec::Relative(delta) => {
                let candidate = self.line_counter.saturating_add(delta);
                if candidate <= footing_limit {
                    self.line_counter = candidate;
                } else {
                    self.deferred_page_break = DeferredPageBreak::Pending;
                }
            }
        }
    }

    /// Closes the current page with `PAGE FOOTING`, increments `PAGE-COUNTER`, resets `LINE-COUNTER` and
    /// `GROUP INDICATE` flags, and presents `PAGE HEADING` on the new page.
    ///
    /// # Arguments
    ///
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if `PF` or `PH` presentation fails.
    fn advance_body_page(&mut self, env: &mut dyn ReportEnv) -> Result<()> {
        if self.page_content == PageContentStatus::HasEmittedLine {
            self.present_page_footing_if_present(env)?;
        }
        self.start_fresh_page_without_footing();
        self.group_indicate_ready.fill(true);
        self.present_page_heading_if_present(env)
    }

    /// Increments `PAGE-COUNTER` and resets per-page state without emitting `PAGE FOOTING`.
    fn start_fresh_page_without_footing(&mut self) {
        self.page_counter = self.page_counter.saturating_add(1);
        self.line_counter = 0;
        self.deferred_page_break = DeferredPageBreak::None;
        self.page_heading_status = PageHeadingStatus::NotYetPresented;
        self.page_content = PageContentStatus::EmptyPage;
        if self.emission_cursor != EmissionCursorState::BeforeFirstReportLine {
            self.emission_cursor = EmissionCursorState::AwaitingNewPageFirstLine;
        }
    }

    /// Closes the active page with `PAGE FOOTING` if at least one line was emitted on it.
    ///
    /// # Arguments
    ///
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if `PF` presentation fails.
    fn flush_page_footing_if_page_nonempty(&mut self, env: &mut dyn ReportEnv) -> Result<()> {
        if self.page_content == PageContentStatus::HasEmittedLine {
            self.present_page_footing_if_present(env)?;
        }
        Ok(())
    }

    /// Presents final `PAGE FOOTING` and `REPORT FOOTING` during `TERMINATE`.
    ///
    /// # Arguments
    ///
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if hook execution or line formatting fails.
    fn present_final_page_and_report_footing(&mut self, env: &mut dyn ReportEnv) -> Result<()> {
        let Some((report_footing_idx, rf_group)) =
            self.find_group_by_kind(ReportGroupKind::ReportFooting)
        else {
            return self.flush_page_footing_if_page_nonempty(env);
        };

        if !eval_present_when(&rf_group.present_when, env)? {
            return self.flush_page_footing_if_page_nonempty(env);
        }

        let pf_first_abs_line = self
            .desc
            .groups
            .iter()
            .find(|g| g.kind == ReportGroupKind::PageFooting)
            .and_then(first_declared_line_spec)
            .and_then(|spec| match spec {
                LinePositionSpec::Absolute(n) => Some(n),
                LinePositionSpec::OnNextPage(_) | LinePositionSpec::Relative(_) => None,
            });

        let rf_first_spec = first_declared_line_spec(&rf_group);
        let rf_precedes_pf = match (rf_first_spec, pf_first_abs_line) {
            (Some(LinePositionSpec::Absolute(report_line)), Some(page_foot_line)) => {
                report_line > self.line_counter && report_line < page_foot_line
            }
            _ => false,
        };

        if rf_precedes_pf {
            self.present_report_footing_group(report_footing_idx, &rf_group, env)?;
            return self.flush_page_footing_if_page_nonempty(env);
        }

        self.flush_page_footing_if_page_nonempty(env)?;
        self.present_report_footing_group(report_footing_idx, &rf_group, env)
    }

    /// Presents `rf_group` either on the current page (if fitting) or on a standalone final page.
    ///
    /// # Arguments
    ///
    /// * `rf_idx` - Index of `REPORT FOOTING` in `self.desc.groups`.
    /// * `rf_group` - Cloned `REPORT FOOTING` descriptor whose root `PRESENT WHEN` is already `true`.
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if `RF` exceeds `PAGE LIMIT` even on a fresh page or formatting fails.
    fn present_report_footing_group(
        &mut self,
        rf_idx: usize,
        rf_group: &ReportGroupDesc,
        env: &mut dyn ReportEnv,
    ) -> Result<()> {
        let Some(lines) = self.prepare_group_lines(rf_group, env)? else {
            return Ok(());
        };

        if let Some(limits) = self.desc.page_limits {
            let fits_current = resolve_line_targets(
                &lines,
                self.line_counter,
                FirstRelativeLineRule::StandardAdvance,
                PagePlacementContext::CurrentPage,
            )
            .filter(|t| t.last().copied().unwrap_or(0) <= limits.page_limit());

            let targets = if let Some(valid) = fits_current {
                valid
            } else {
                if self.page_content == PageContentStatus::HasEmittedLine {
                    self.start_fresh_page_without_footing();
                }
                let Some(fresh_valid) = resolve_line_targets(
                    &lines,
                    self.line_counter,
                    FirstRelativeLineRule::HeadingFloor(limits.heading()),
                    PagePlacementContext::FreshPage,
                )
                .filter(|t| t.last().copied().unwrap_or(0) <= limits.page_limit()) else {
                    return Err(report_err(
                        &self.desc.report_name,
                        "REPORT FOOTING exceeds PAGE LIMIT even on a fresh page",
                    ));
                };
                fresh_valid
            };
            self.emit_resolved_lines(rf_idx, rf_group.kind, &lines, &targets, env)
        } else {
            let Some(targets) = resolve_line_targets(
                &lines,
                self.line_counter,
                FirstRelativeLineRule::StandardAdvance,
                PagePlacementContext::CurrentPage,
            ) else {
                return Err(report_err(
                    &self.desc.report_name,
                    "REPORT FOOTING absolute LINE NUMBER precedes current LINE-COUNTER",
                ));
            };
            self.emit_resolved_lines(rf_idx, rf_group.kind, &lines, &targets, env)
        }
    }

    /// Invokes `env.before_reporting(...)` for `group`, returning `Ok(true)` if `SUPPRESS PRINTING` was triggered.
    ///
    /// # Arguments
    ///
    /// * `group` - Report group about to be presented.
    /// * `env` - Mutable [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Propagates any [`coblang_common::CoblangError`] returned by `env.before_reporting`.
    fn run_before_reporting_hook(
        &mut self,
        group: &ReportGroupDesc,
        env: &mut dyn ReportEnv,
    ) -> Result<bool> {
        self.suppress_current_group = false;
        let mut ctx = ReportHookContext::new(
            &self.desc.report_name,
            group.name.as_deref(),
            group.kind,
            &mut self.page_counter,
            self.line_counter,
            &mut self.suppress_current_group,
            &self.sum_values,
        );
        env.before_reporting(&mut ctx)?;
        Ok(self.suppress_current_group)
    }

    /// Formats each [`PendingPrintLine`] at its resolved 1-based `target_line` and appends [`ReportLine`]s.
    ///
    /// # Arguments
    ///
    /// * `grp_idx` - Originating group index in `self.desc.groups`.
    /// * `kind` - Originating [`ReportGroupKind`].
    /// * `lines` - Visible print lines for this group.
    /// * `targets` - Resolved 1-based page line numbers aligned with `lines`.
    /// * `env` - Active [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if item formatting or field decoding fails.
    fn emit_resolved_lines(
        &mut self,
        grp_idx: usize,
        kind: ReportGroupKind,
        lines: &[PendingPrintLine],
        targets: &[u32],
        env: &dyn ReportEnv,
    ) -> Result<()> {
        let indicate_allowed = self
            .group_indicate_ready
            .get(grp_idx)
            .copied()
            .unwrap_or(true);

        for (pl, &target_line) in lines.iter().zip(targets.iter()) {
            let prev_lc = self.line_counter;
            self.line_counter = target_line;
            let content = self.render_single_line(pl, kind, indicate_allowed, env)?;

            let advancing = match self.emission_cursor {
                EmissionCursorState::BeforeFirstReportLine => ReportLineAdvancing::InitialLine {
                    skip_before: target_line.saturating_sub(1),
                },
                EmissionCursorState::AwaitingNewPageFirstLine => ReportLineAdvancing::NewPage {
                    skip_before: target_line.saturating_sub(1),
                },
                EmissionCursorState::SamePageActive => ReportLineAdvancing::SamePage {
                    lines_down: target_line.saturating_sub(prev_lc),
                },
            };

            self.page_content = PageContentStatus::HasEmittedLine;
            self.emission_cursor = EmissionCursorState::SamePageActive;
            self.emitted_lines.push(ReportLine {
                page_number: self.page_counter,
                line_number: target_line,
                advancing,
                group_kind: kind,
                code_prefix: self.desc.code_prefix.clone(),
                content,
            });
        }
        Ok(())
    }

    /// Formats all elementary items on `pl` into a horizontal line byte buffer.
    ///
    /// # Arguments
    ///
    /// * `pl` - [`PendingPrintLine`] containing visible elementary items.
    /// * `kind` - Enclosing [`ReportGroupKind`].
    /// * `indicate_allowed` - Whether `GROUP INDICATE` items should display on this group presentation.
    /// * `env` - Active [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if formatting an elementary item fails.
    fn render_single_line(
        &self,
        pl: &PendingPrintLine,
        kind: ReportGroupKind,
        indicate_allowed: bool,
        env: &dyn ReportEnv,
    ) -> Result<Vec<u8>> {
        let mut line_buf: Vec<u8> = Vec::new();
        let mut right_col_1based: u32 = 0;

        for (col_spec, item) in &pl.items {
            let start_col_1based = match *col_spec {
                ColumnPositionSpec::Absolute(col) => col,
                ColumnPositionSpec::Relative(offset) => {
                    right_col_1based.saturating_add(offset).max(1)
                }
            };

            let formatted = self.format_elementary_item(item, env)?;
            let item_len_u32 = u32::try_from(formatted.len()).unwrap_or(u32::MAX);
            let suppressed_by_indicate = kind == ReportGroupKind::Detail
                && item.group_indicate == GroupIndicatePolicy::GroupIndicate
                && !indicate_allowed;

            if formatted.is_empty() {
                right_col_1based = start_col_1based;
            } else {
                right_col_1based = start_col_1based
                    .saturating_add(item_len_u32)
                    .saturating_sub(1);
            }

            if suppressed_by_indicate {
                continue;
            }

            let start_idx = usize::try_from(start_col_1based.saturating_sub(1)).unwrap_or(0);
            let required_len = start_idx.saturating_add(formatted.len());
            if line_buf.len() < required_len {
                line_buf.resize(required_len, b' ');
            }
            for (dst, &src_byte) in line_buf.iter_mut().skip(start_idx).zip(formatted.iter()) {
                *dst = src_byte;
            }
        }
        Ok(line_buf)
    }

    /// Evaluates `item.source` and formats it according to `item.formatter`.
    ///
    /// # Arguments
    ///
    /// * `item` - Elementary [`ReportFieldItem`] to format.
    /// * `env` - Active [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if reading a field or running `PICTURE` formatting fails.
    fn format_elementary_item(
        &self,
        item: &ReportFieldItem,
        env: &dyn ReportEnv,
    ) -> Result<Vec<u8>> {
        let Some(source) = &item.source else {
            return Ok(match &item.formatter {
                FieldFormatter::RawBytes => Vec::new(),
                FieldFormatter::FixedWidthAlpha {
                    width, space_byte, ..
                } => vec![*space_byte; *width],
                FieldFormatter::Picture { byte_len, desc } => {
                    vec![desc.overpunch.space_byte(); *byte_len]
                }
            });
        };

        match &item.formatter {
            FieldFormatter::RawBytes => self.format_source_as_raw_bytes(source, env),
            FieldFormatter::FixedWidthAlpha {
                width,
                justified,
                space_byte,
            } => {
                let raw = self.format_source_as_raw_bytes(source, env)?;
                Ok(pad_or_truncate_alpha(&raw, *width, *justified, *space_byte))
            }
            FieldFormatter::Picture {
                byte_len,
                desc: dst_desc,
            } => {
                let mut dst = vec![dst_desc.overpunch.space_byte(); *byte_len];
                match source {
                    ItemValueSource::Literal(bytes) => {
                        execute_move(MoveSource::AlphanumericLiteral(bytes), &mut dst, dst_desc)?;
                    }
                    ItemValueSource::DecimalLiteral(dec) => {
                        execute_move(MoveSource::Decimal(*dec), &mut dst, dst_desc)?;
                    }
                    ItemValueSource::PageCounter => {
                        let dec = FixedDecimal::from_i64(i64::from(self.page_counter));
                        execute_move(MoveSource::Decimal(dec), &mut dst, dst_desc)?;
                    }
                    ItemValueSource::LineCounter => {
                        let dec = FixedDecimal::from_i64(i64::from(self.line_counter));
                        execute_move(MoveSource::Decimal(dec), &mut dst, dst_desc)?;
                    }
                    ItemValueSource::SumCounter(sum_id) => {
                        let dec = self
                            .sum_values
                            .get(sum_id.as_usize())
                            .copied()
                            .unwrap_or(FixedDecimal::ZERO);
                        execute_move(MoveSource::Decimal(dec), &mut dst, dst_desc)?;
                    }
                    ItemValueSource::SourceField(fid) => {
                        let fv = env.read_field(*fid)?;
                        if let Some(src_desc) = &fv.desc {
                            execute_move(
                                MoveSource::Field {
                                    bytes: &fv.bytes,
                                    desc: src_desc,
                                },
                                &mut dst,
                                dst_desc,
                            )?;
                        } else if matches!(
                            dst_desc.category(),
                            DataCategory::Numeric | DataCategory::NumericEdited
                        ) {
                            let dec = FieldValue::raw(&fv.bytes).decode_decimal()?;
                            execute_move(MoveSource::Decimal(dec), &mut dst, dst_desc)?;
                        } else {
                            execute_move(
                                MoveSource::AlphanumericLiteral(&fv.bytes),
                                &mut dst,
                                dst_desc,
                            )?;
                        }
                    }
                }
                Ok(dst)
            }
        }
    }

    /// Evaluates `source` into plain unedited display bytes.
    ///
    /// # Arguments
    ///
    /// * `source` - Elementary [`ItemValueSource`].
    /// * `env` - Active [`ReportEnv`].
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if reading or decoding a field fails.
    fn format_source_as_raw_bytes(
        &self,
        source: &ItemValueSource,
        env: &dyn ReportEnv,
    ) -> Result<Vec<u8>> {
        match source {
            ItemValueSource::Literal(bytes) => Ok(bytes.clone()),
            ItemValueSource::DecimalLiteral(dec) => Ok(dec.to_string().into_bytes()),
            ItemValueSource::PageCounter => Ok(self.page_counter.to_string().into_bytes()),
            ItemValueSource::LineCounter => Ok(self.line_counter.to_string().into_bytes()),
            ItemValueSource::SumCounter(sum_id) => {
                let dec = self
                    .sum_values
                    .get(sum_id.as_usize())
                    .copied()
                    .unwrap_or(FixedDecimal::ZERO);
                Ok(dec.to_string().into_bytes())
            }
            ItemValueSource::SourceField(fid) => {
                let fv = env.read_field(*fid)?;
                if let Some(src_desc) = &fv.desc {
                    format_field_for_display(&fv.bytes, src_desc)
                } else {
                    Ok(fv.bytes)
                }
            }
        }
    }
}

/// Walks `group` respecting `PRESENT WHEN` guards and collects visible [`PendingPrintLine`]s.
///
/// # Arguments
///
/// * `group` - Validated [`ReportGroupDesc`] whose root `present_when` already evaluated to `true`.
/// * `env` - Active [`ReportEnv`].
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if evaluating a child `PRESENT WHEN` predicate fails.
fn collect_visible_lines(
    group: &ReportGroupDesc,
    env: &dyn ReportEnv,
) -> Result<Vec<PendingPrintLine>> {
    let mut lines = Vec::new();
    if let Some(root_line) = group.line {
        lines.push(PendingPrintLine {
            line_spec: root_line,
            items: Vec::new(),
        });
    }
    collect_nodes_into_lines(&group.children, env, &mut lines)?;
    Ok(lines)
}

/// Recursively walks `nodes`, filtering by `PRESENT WHEN` and appending lines/items into `lines`.
///
/// # Arguments
///
/// * `nodes` - Child [`ReportNode`] slice.
/// * `env` - Active [`ReportEnv`].
/// * `lines` - Output [`PendingPrintLine`] accumulator.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if evaluating a `PRESENT WHEN` predicate fails.
fn collect_nodes_into_lines(
    nodes: &[ReportNode],
    env: &dyn ReportEnv,
    lines: &mut Vec<PendingPrintLine>,
) -> Result<()> {
    for node in nodes {
        match node {
            ReportNode::SubGroup(sub) => {
                if !eval_present_when(&sub.present_when, env)? {
                    continue;
                }
                if let Some(line_spec) = sub.line {
                    lines.push(PendingPrintLine {
                        line_spec,
                        items: Vec::new(),
                    });
                }
                collect_nodes_into_lines(&sub.children, env, lines)?;
            }
            ReportNode::Elementary(item) => {
                if !eval_present_when(&item.present_when, env)? {
                    continue;
                }
                if let Some(line_spec) = item.line {
                    lines.push(PendingPrintLine {
                        line_spec,
                        items: Vec::new(),
                    });
                }
                if let Some(col_spec) = item.column {
                    lines
                        .last_mut()
                        .into_iter()
                        .for_each(|active| active.items.push((col_spec, item.clone())));
                }
            }
        }
    }
    Ok(())
}

/// Returns the first declared [`LinePositionSpec`] in `group` (either on `group.line` or its first `LINE`-bearing descendant).
///
/// # Arguments
///
/// * `group` - [`ReportGroupDesc`] to inspect.
#[must_use]
fn first_declared_line_spec(group: &ReportGroupDesc) -> Option<LinePositionSpec> {
    if let Some(root_line) = group.line {
        return Some(root_line);
    }
    first_line_in_nodes(&group.children)
}

/// Searches `nodes` depth-first for the first declared [`LinePositionSpec`].
///
/// # Arguments
///
/// * `nodes` - [`ReportNode`] slice to inspect.
#[must_use]
fn first_line_in_nodes(nodes: &[ReportNode]) -> Option<LinePositionSpec> {
    for node in nodes {
        match node {
            ReportNode::SubGroup(sub) => {
                if let Some(l) = sub.line {
                    return Some(l);
                }
                if let Some(nested) = first_line_in_nodes(&sub.children) {
                    return Some(nested);
                }
            }
            ReportNode::Elementary(item) => {
                if let Some(l) = item.line {
                    return Some(l);
                }
            }
        }
    }
    None
}
