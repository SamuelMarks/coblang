//! Audited C-ABI entrypoints (`libcoblang_rt`) for Milestone M18 Declarative `REPORT SECTION`
//! (`INITIATE`, `GENERATE`, `TERMINATE`, `SUPPRESS PRINTING`, `USE BEFORE REPORTING`, `SUM`, and pagination).

#![deny(clippy::indexing_slicing)]

use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;
use std::sync::Mutex;

use coblang_decimal::FixedDecimal;
use coblang_report::{PageLimits, ReportGroupKind, ReportLifecycleState};
use coblang_rt_core::field_ops::{RuntimeMoveSourceKind, runtime_execute_move};

use crate::fileio_abi::{implicit_close_open_files_with_warning, write_report_line_to_open_file};

/// Stepper action code: all pending steps for the current `GENERATE` / `TERMINATE` completed.
pub const REPORT_STEP_DONE: i32 = 0;
/// Stepper action code: evaluate group root `PRESENT WHEN` / `AFTER NEW` for `*out_group_idx`.
pub const REPORT_STEP_CHECK_GATE: i32 = 1;
/// Stepper action code: collect visible line specs (dry-run) for `*out_group_idx` to resolve pagination.
pub const REPORT_STEP_COLLECT_LINES: i32 = 2;
/// Stepper action code: run `USE BEFORE REPORTING` COBOL declarative for `*out_group_idx`.
pub const REPORT_STEP_RUN_HOOK: i32 = 3;
/// Stepper action code: render prepared lines & items for `*out_group_idx`.
pub const REPORT_STEP_RENDER_GROUP: i32 = 4;
/// Stepper action code: accumulate `SUM` addends for `*out_group_idx` (`-1` for summary `GENERATE`).
pub const REPORT_STEP_ACCUM_SUMS: i32 = 5;

///Configured `NEXT GROUP` rule on a report group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RtNextGroup {
    /// No `NEXT GROUP` clause.
    None,
    /// `NEXT GROUP IS n`.
    Absolute(u32),
    /// `NEXT GROUP IS PLUS n`.
    Relative(u32),
    /// `NEXT GROUP NEXT PAGE`.
    NextPage,
}

/// Configured group metadata inside [`ManagedReport`].
#[derive(Debug, Clone)]
struct RtGroupConfig {
    /// Uppercase group name (empty if unnamed).
    name: String,
    /// Group role (`RH`, `PH`, `CH(i)`, `DE`, `CF(i)`, `PF`, `RF`).
    kind: ReportGroupKind,
    /// `NEXT GROUP` rule.
    next_group: RtNextGroup,
    /// Whether a `USE BEFORE REPORTING` hook is bound to this group.
    has_hook: bool,
}

/// Configured `SUM` counter metadata inside [`ManagedReport`].
#[derive(Debug, Clone)]
struct RtSumConfig {
    /// Owning control level index.
    owner_ctrl: usize,
    /// `RESET ON` control level index.
    reset_ctrl: usize,
    /// Subordinate `SUM` counter indices rolled into this counter on control footing break.
    rolling_from: Vec<usize>,
    /// Optional uppercase `UPON` detail group filter names (empty = all details + summary).
    upon_details: Vec<String>,
}

/// Live control field binding (`prior_bytes`, `current_bytes`, and live COBOL storage pointer).
#[derive(Debug, Clone)]
struct RtControlSlot {
    /// Control value captured on the previous `GENERATE`.
    prior_bytes: Vec<u8>,
    /// Control value captured at the start of the current `GENERATE` / `TERMINATE`.
    current_bytes: Vec<u8>,
    /// Live COBOL storage address + length (`(0, 0)` for `FINAL`).
    live_addr: usize,
    /// Live COBOL storage byte length.
    live_len: usize,
}

/// Line positioning kind passed from MLIR during Pass A (`COLLECT_LINES`) and Pass B (`RENDER_GROUP`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RtLineSpec {
    /// `LINE n` (1-based absolute).
    Absolute(u32),
    /// `LINE PLUS n` (relative offset).
    Relative(u32),
    /// `LINE n NEXT PAGE`.
    OnNextPage(u32),
}

/// Pending line buffer constructed during `REPORT_STEP_RENDER_GROUP`.
#[derive(Debug, Clone)]
struct ActiveRenderLine {
    /// Target 1-based page line number resolved by Pass A.
    target_line: u32,
    /// Horizontal character buffer (1-based column 1 at index 0).
    buf: Vec<u8>,
    /// 1-based rightmost column written by the previous item on this line (`0` initially).
    right_col_1based: u32,
}

/// Queued internal state-machine phase inside [`ManagedReport`].
#[derive(Debug, Clone)]
enum ReportPhase {
    /// Restore `prior_bytes` into live COBOL control variables before `CF` hooks & rendering.
    UsePriorControls,
    /// Restore `current_bytes` into live COBOL control variables after `CF` sequence completes.
    UseCurrentControls,
    /// Roll and reset `SUM` counters at `ctrl_idx` after `CF(ctrl_idx)` completes.
    RollAndResetSums(usize),
    /// Begin presenting group `group_idx` (`is_overflow_retry` = already on a fresh page).
    BeginGroup {
        /// Group index in `groups`.
        group_idx: usize,
        /// `true` after an overflow page break has already been performed.
        on_fresh_page: bool,
    },
    /// Run `USE BEFORE REPORTING` hook for `group_idx` with pre-resolved `targets`.
    RunHook {
        /// Group index.
        group_idx: usize,
        /// Resolved 1-based line targets.
        targets: Vec<u32>,
    },
    /// Render `group_idx` onto `targets`.
    RenderPrepared {
        /// Group index.
        group_idx: usize,
        /// Resolved 1-based line targets.
        targets: Vec<u32>,
    },
    /// Increment `PAGE-COUNTER`, reset `LINE-COUNTER = 0`, and reset `GROUP INDICATE`.
    StartFreshPage,
    /// Accumulate `SUM` addends for `detail_group_idx` (`-1` for summary `GENERATE`).
    AccumulateSums(i32),
    /// Emit final `PF` / `RF` sequence during `TERMINATE`.
    FinalFootings,
    /// Pad final page to `PAGE LIMIT` if needed and mark report `Inactive`.
    FinishTerminate,
}

/// Bitmask of lifecycle, page-emission, and pagination state flags on [`ManagedReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReportFlags(u16);

impl ReportFlags {
    /// Whether at least one `GENERATE` has occurred since `INITIATE`.
    const HAS_GENERATED: u16 = 1 << 0;
    /// Whether any line has ever been written since `INITIATE`.
    const ANY_LINE_EMITTED: u16 = 1 << 1;
    /// Whether any body group (`CH`/`DE`/`CF`) has emitted a line on the current page.
    const BODY_EMITTED_ON_PAGE: u16 = 1 << 2;
    /// Whether `PAGE HEADING` has been presented on the current page.
    const PH_PRESENTED_ON_PAGE: u16 = 1 << 3;
    /// Whether `PAGE FOOTING` has started presenting on the current page.
    const PF_PRESENTED_ON_PAGE: u16 = 1 << 4;
    /// Deferred `NEXT GROUP NEXT PAGE` flag.
    const DEFERRED_NEXT_PAGE: u16 = 1 << 5;
    /// Suppress trailing page pad in `FinishTerminate` when `RF` followed `PF` on a fresh page.
    const SKIP_FINAL_PAD: u16 = 1 << 6;
    /// Suppress flag set by `SUPPRESS PRINTING` or `PRINT-SWITCH != 0`.
    const SUPPRESS_CURRENT_GROUP: u16 = 1 << 7;
    /// `true` when at the top of a fresh page for `PRESENT AFTER NEW PAGE`.
    const AFTER_NEW_PAGE: u16 = 1 << 8;
    /// Whether `pending_group` is already retrying on a fresh page.
    const PENDING_FRESH_PAGE: u16 = 1 << 9;

    /// Returns `true` when `bit` is set.
    ///
    /// # Arguments
    ///
    /// * `bit` - Single-bit flag mask to query.
    const fn has(self, bit: u16) -> bool {
        (self.0 & bit) != 0
    }

    /// Sets or clears `bit` according to `enabled`.
    ///
    /// # Arguments
    ///
    /// * `bit` - Single-bit flag mask to update.
    /// * `enabled` - New boolean value for `bit`.
    fn set_to(&mut self, bit: u16, enabled: bool) {
        if enabled {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }
}

/// Complete runtime state of a single `RD` report engine.
struct ManagedReport {
    /// Original COBOL report name (`"rp"`, `"RW-FS1-REPORT-1"`).
    report_name: String,
    /// Associated COBOL `FD` connector name (`"report-file"`).
    file_name: String,
    /// Associated `SELECT ... ASSIGN` external/literal label (`"PRINTOUT"`).
    assign_label: String,
    /// Optional validated [`PageLimits`].
    page_limits: Option<PageLimits>,
    /// Dynamic or static horizontal column limit (`0` = unbounded).
    active_col_limit: usize,
    /// Active `CODE IS` prefix bytes prepended to every emitted line.
    active_code_prefix: Vec<u8>,
    /// Lifecycle state (`Inactive` or `Active`).
    lifecycle: ReportLifecycleState,
    /// Live `PAGE-COUNTER` (`1` after `INITIATE`, `0` before).
    page_counter: u32,
    /// Live `LINE-COUNTER` (`0` after `INITIATE`).
    line_counter: u32,
    /// Last emitted physical line number on the current page (`0` when page is empty).
    emitted_line_on_page: u32,
    /// Packed boolean state flags.
    flags: ReportFlags,
    /// Control hierarchy slots (`0` = most major / `FINAL`).
    controls: Vec<RtControlSlot>,
    /// `SUM` counter configurations.
    sum_configs: Vec<RtSumConfig>,
    /// Exact accumulated `FixedDecimal` values per `SUM` counter.
    sum_values: Vec<FixedDecimal>,
    /// Configured report groups (`0..groups.len()`).
    groups: Vec<RtGroupConfig>,
    /// Per-group `GROUP INDICATE` ready flags (`true` on page start or control break).
    group_indicate_ready: Vec<bool>,
    /// Active `AFTER NEW` break flags (`true` for controls that broke on current `GENERATE`).
    after_new_controls: Vec<bool>,
    /// Pending stepper queue.
    queue: VecDeque<ReportPhase>,
    /// Active group index awaiting callback response from MLIR (`gate`, `collect`, `hook`, `render`).
    pending_group: usize,
    /// Collected `RtLineSpec`s during `REPORT_STEP_COLLECT_LINES`.
    collected_specs: Vec<RtLineSpec>,
    /// Resolved line targets for the group currently in `REPORT_STEP_RENDER_GROUP`.
    render_targets: Vec<u32>,
    /// Current line cursor (`0..render_targets.len()`) during `REPORT_STEP_RENDER_GROUP`.
    render_line_cursor: usize,
    /// Accumulated rendered lines for the current group.
    rendered_lines: Vec<ActiveRenderLine>,
}

impl ManagedReport {
    /// Creates an inactive, unconfigured [`ManagedReport`].
    fn new() -> Self {
        Self {
            report_name: String::new(),
            file_name: String::new(),
            assign_label: String::new(),
            page_limits: None,
            active_col_limit: 0,
            active_code_prefix: Vec::new(),
            lifecycle: ReportLifecycleState::Inactive,
            page_counter: 0,
            line_counter: 0,
            emitted_line_on_page: 0,
            flags: ReportFlags(ReportFlags::AFTER_NEW_PAGE),
            controls: Vec::new(),
            sum_configs: Vec::new(),
            sum_values: Vec::new(),
            groups: Vec::new(),
            group_indicate_ready: Vec::new(),
            after_new_controls: Vec::new(),
            queue: VecDeque::new(),
            pending_group: 0,
            collected_specs: Vec::new(),
            render_targets: Vec::new(),
            render_line_cursor: 0,
            rendered_lines: Vec::new(),
        }
    }

    /// Finds all group indices matching `kind` in declaration order (for `RH`, `PH`, `PF`, `RF`).
    ///
    /// # Arguments
    ///
    /// * `kind` - Target [`ReportGroupKind`] to match.
    fn find_all_groups(&self, kind: ReportGroupKind) -> Vec<usize> {
        self.groups
            .iter()
            .enumerate()
            .filter_map(|(i, g)| (g.kind == kind).then_some(i))
            .collect()
    }

    /// Writes a slice of `(target_line, content)` lines to the open `FD` file connector.
    ///
    /// # Arguments
    ///
    /// * `lines` - Prepared physical lines to flush.
    fn flush_lines_to_fd(&mut self, lines: &[ActiveRenderLine]) {
        for rl in lines {
            let mut content = rl.buf.clone();
            if self.active_col_limit > 0 && content.len() > self.active_col_limit {
                content.truncate(self.active_col_limit);
            }
            if !self.flags.has(ReportFlags::ANY_LINE_EMITTED) {
                let skip = rl.target_line.saturating_sub(1);
                for _ in 0..skip {
                    write_report_line_to_open_file(&self.file_name, &self.active_code_prefix, 2, 1);
                }
                let mut full = self.active_code_prefix.clone();
                full.extend_from_slice(&content);
                write_report_line_to_open_file(&self.file_name, &full, 2, 1);
                self.flags.set_to(ReportFlags::ANY_LINE_EMITTED, true);
            } else if self.emitted_line_on_page == 0 {
                // First line on a subsequent page: advance past remaining lines of prior page if paged.
                let skip = rl.target_line.saturating_sub(1);
                for _ in 0..skip {
                    write_report_line_to_open_file(&self.file_name, &self.active_code_prefix, 2, 1);
                }
                let mut full = self.active_code_prefix.clone();
                full.extend_from_slice(&content);
                write_report_line_to_open_file(&self.file_name, &full, 2, 1);
            } else {
                let down = rl.target_line.saturating_sub(self.emitted_line_on_page);
                let blank_between = down.saturating_sub(1);
                for _ in 0..blank_between {
                    write_report_line_to_open_file(&self.file_name, &self.active_code_prefix, 2, 1);
                }
                let mut full = self.active_code_prefix.clone();
                full.extend_from_slice(&content);
                write_report_line_to_open_file(&self.file_name, &full, 2, i64::from(down != 0));
            }
            self.emitted_line_on_page = rl.target_line;
            self.line_counter = rl.target_line;
        }
    }

    /// Pads remaining blank lines down to `page_limit` before advancing to a new page (or at report end).
    fn pad_page_to_limit(&mut self) {
        if let Some(lim) = self.page_limits
            && self.emitted_line_on_page > 0
            && self.emitted_line_on_page < lim.page_limit()
        {
            let rem = lim.page_limit() - self.emitted_line_on_page;
            for _ in 0..rem {
                write_report_line_to_open_file(&self.file_name, &self.active_code_prefix, 2, 1);
            }
            self.emitted_line_on_page = lim.page_limit();
        }
    }
}

/// Process-wide table of active and configured `RD` report instances.
static REPORT_TABLE: Mutex<BTreeMap<u32, ManagedReport>> = Mutex::new(BTreeMap::new());

/// Executes `f` with shared access to [`REPORT_TABLE`].
///
/// # Arguments
///
/// * `f` - Closure reading [`REPORT_TABLE`].
fn with_report_table<R>(f: impl FnOnce(&BTreeMap<u32, ManagedReport>) -> R) -> R {
    let guard = REPORT_TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&guard)
}

/// Executes `f` with exclusive access to [`REPORT_TABLE`].
///
/// # Arguments
///
/// * `f` - Closure mutating [`REPORT_TABLE`].
fn with_report_table_mut<R>(f: impl FnOnce(&mut BTreeMap<u32, ManagedReport>) -> R) -> R {
    let mut guard = REPORT_TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut guard)
}

/// Resets all report instances (called on run-unit teardown / tests).
pub fn reset_report_runtime_state() {
    with_report_table_mut(BTreeMap::clear);
}

/// Decodes a raw `(ptr, len)` UTF-8 byte slice safely.
///
/// # Arguments
///
/// * `ptr` - Raw byte pointer (`null` allowed when `len == 0`).
/// * `len` - Byte length of `ptr`.
///
/// # Safety
///
/// `ptr` must be valid for `len` bytes when `len > 0`.
#[expect(
    unsafe_code,
    reason = "Internal pointer-slice decoder for C-ABI strings."
)]
unsafe fn slice_from_raw<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: Caller guarantees `[ptr..ptr + len)` is readable.
        unsafe { core::slice::from_raw_parts(ptr, len) }
    }
}

/// Decodes a raw `(ptr, len)` slice into an owned UTF-8 [`String`].
///
/// # Arguments
///
/// * `ptr` - Raw byte pointer (`null` allowed when `len == 0`).
/// * `len` - Byte length of `ptr`.
///
/// # Safety
///
/// `ptr` must be valid for `len` bytes when `len > 0`.
#[expect(
    unsafe_code,
    reason = "Internal UTF-8 string helper wrapping `slice_from_raw`."
)]
unsafe fn lossy_string_from_raw(ptr: *const u8, len: usize) -> String {
    // SAFETY: Caller guarantees `[ptr..ptr + len)` is readable.
    let raw = unsafe { slice_from_raw(ptr, len) };
    String::from_utf8_lossy(raw).into_owned()
}

/// Initializes static schema metadata for `report_id` prior to `coblang_rt_report_initiate`.
///
/// # Arguments
///
/// * `report_id` - Zero-based report handle ID.
/// * `report_name_ptr` - Original report name UTF-8 pointer.
/// * `report_name_len` - Byte length of `report_name_ptr`.
/// * `file_name_ptr` - Associated `FD` file name UTF-8 pointer.
/// * `file_name_len` - Byte length of `file_name_ptr`.
/// * `assign_ptr` - Associated `ASSIGN` target label UTF-8 pointer.
/// * `assign_len` - Byte length of `assign_ptr`.
/// * `page_limit` - `PAGE LIMIT` (`<= 0` when unpaged).
/// * `heading` - `HEADING` line (`<= 0` when omitted).
/// * `first_detail` - `FIRST DETAIL` line (`<= 0` when omitted).
/// * `last_detail` - `LAST DETAIL` line (`<= 0` when omitted).
/// * `footing` - `FOOTING` line (`<= 0` when omitted).
/// * `control_count` - Number of control levels.
/// * `sum_count` - Number of `SUM` counters.
/// * `group_count` - Number of typed report groups.
///
/// # Safety
///
/// String pointers must be valid for their corresponding byte lengths.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_report_init_schema(
    report_id: u32,
    report_name_ptr: *const u8,
    report_name_len: usize,
    file_name_ptr: *const u8,
    file_name_len: usize,
    assign_ptr: *const u8,
    assign_len: usize,
    page_limit: i64,
    heading: i64,
    first_detail: i64,
    last_detail: i64,
    footing: i64,
    control_count: u32,
    sum_count: u32,
    group_count: u32,
) {
    // SAFETY: Caller passes valid static string slices from compiled MLIR constants.
    let rep_name = unsafe { lossy_string_from_raw(report_name_ptr, report_name_len) };
    // SAFETY: Caller passes valid static string slices from compiled MLIR constants.
    let f_name = unsafe { lossy_string_from_raw(file_name_ptr, file_name_len) };
    // SAFETY: Caller passes valid static string slices from compiled MLIR constants.
    let a_lbl = unsafe { lossy_string_from_raw(assign_ptr, assign_len) };

    with_report_table_mut(|tbl| {
        let rep = tbl.entry(report_id).or_insert_with(ManagedReport::new);
        if rep.lifecycle == ReportLifecycleState::Active {
            return;
        }
        rep.report_name = rep_name;
        rep.file_name = f_name;
        rep.assign_label = a_lbl;
        rep.page_limits = if page_limit > 0 {
            let pl = u32::try_from(page_limit).unwrap_or(66);
            let h = u32::try_from(heading).ok().filter(|&v| v > 0);
            let fd = u32::try_from(first_detail).ok().filter(|&v| v > 0);
            let ld = u32::try_from(last_detail).ok().filter(|&v| v > 0);
            let ft = u32::try_from(footing).ok().filter(|&v| v > 0);
            PageLimits::try_new(pl, h, fd, ld, ft).ok()
        } else {
            None
        };
        let c_cnt = usize::try_from(control_count).unwrap_or(0);
        let s_cnt = usize::try_from(sum_count).unwrap_or(0);
        let g_cnt = usize::try_from(group_count).unwrap_or(0);
        rep.controls = (0..c_cnt)
            .map(|_| RtControlSlot {
                prior_bytes: Vec::new(),
                current_bytes: Vec::new(),
                live_addr: 0,
                live_len: 0,
            })
            .collect();
        rep.after_new_controls = vec![false; c_cnt];
        rep.sum_configs = (0..s_cnt)
            .map(|_| RtSumConfig {
                owner_ctrl: 0,
                reset_ctrl: 0,
                rolling_from: Vec::new(),
                upon_details: Vec::new(),
            })
            .collect();
        rep.sum_values = vec![FixedDecimal::ZERO; s_cnt];
        rep.groups = (0..g_cnt)
            .map(|_| RtGroupConfig {
                name: String::new(),
                kind: ReportGroupKind::Detail,
                next_group: RtNextGroup::None,
                has_hook: false,
            })
            .collect();
        rep.group_indicate_ready = vec![true; g_cnt];
    });
}

/// Configures static metadata for a single report group `group_idx` on `report_id`.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `group_idx` - Zero-based group index.
/// * `name_ptr` - Group name UTF-8 pointer.
/// * `name_len` - Group name byte length.
/// * `role_code` - `0=RH, 1=PH, 2=CH, 3=DE, 4=CF, 5=PF, 6=RF`.
/// * `ctrl_idx` - Control index for `CH`/`CF`.
/// * `ng_kind` - `0=None, 1=Absolute, 2=Relative, 3=NextPage`.
/// * `ng_val` - Line/offset value for `NEXT GROUP`.
/// * `has_hook` - `1` if a `USE BEFORE REPORTING` declarative is bound to this group.
///
/// # Safety
///
/// `name_ptr` must be valid for `name_len` bytes.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_report_config_group(
    report_id: u32,
    group_idx: u32,
    name_ptr: *const u8,
    name_len: usize,
    role_code: u32,
    ctrl_idx: u32,
    ng_kind: u32,
    ng_val: u32,
    has_hook: i32,
) {
    // SAFETY: Caller passes valid static string bytes.
    let g_name = unsafe { lossy_string_from_raw(name_ptr, name_len) };
    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return;
        };
        if rep.lifecycle == ReportLifecycleState::Active {
            return;
        }
        let Some(slot) = usize::try_from(group_idx)
            .ok()
            .and_then(|i| rep.groups.get_mut(i))
        else {
            return;
        };
        let ci = coblang_report::ControlLevelIndex::new(u16::try_from(ctrl_idx).unwrap_or(0));
        slot.name = g_name;
        slot.kind = match role_code {
            0 => ReportGroupKind::ReportHeading,
            1 => ReportGroupKind::PageHeading,
            2 => ReportGroupKind::ControlHeading(ci),
            4 => ReportGroupKind::ControlFooting(ci),
            5 => ReportGroupKind::PageFooting,
            6 => ReportGroupKind::ReportFooting,
            _ => ReportGroupKind::Detail,
        };
        slot.next_group = match ng_kind {
            1 => RtNextGroup::Absolute(ng_val),
            2 => RtNextGroup::Relative(ng_val),
            3 => RtNextGroup::NextPage,
            _ => RtNextGroup::None,
        };
        slot.has_hook = has_hook != 0;
    });
}

/// Configures a `SUM` counter's control ownership, `RESET ON` level, rolling inputs, and `UPON` filters.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `sum_idx` - Zero-based `SUM` counter index.
/// * `owner_ctrl` - Owning `CONTROL FOOTING` level index.
/// * `reset_ctrl` - `RESET ON` control level index.
///
/// # Safety
///
/// Pure integer configuration; always safe to call.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_report_config_sum(
    report_id: u32,
    sum_idx: u32,
    owner_ctrl: u32,
    reset_ctrl: u32,
) {
    with_report_table_mut(|tbl| {
        if let Some(rep) = tbl.get_mut(&report_id)
            && rep.lifecycle == ReportLifecycleState::Inactive
            && let Ok(idx) = usize::try_from(sum_idx)
            && let Some(sc) = rep.sum_configs.get_mut(idx)
        {
            sc.owner_ctrl = usize::try_from(owner_ctrl).unwrap_or(0);
            sc.reset_ctrl = usize::try_from(reset_ctrl).unwrap_or(0);
        }
    });
}

/// Registers a rolling `sub_sum_idx -> sum_idx` link or `UPON detail_name` filter on `sum_idx`.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `sum_idx` - Target `SUM` counter index.
/// * `sub_sum_idx` - Subordinate `SUM` counter index (`< 0` when registering `upon_ptr`).
/// * `upon_ptr` - Optional uppercase `UPON` detail group name UTF-8 pointer.
/// * `upon_len` - Byte length of `upon_ptr`.
///
/// # Safety
///
/// `upon_ptr` must be valid for `upon_len` bytes when `upon_len > 0`.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_report_config_sum_link(
    report_id: u32,
    sum_idx: u32,
    sub_sum_idx: i32,
    upon_ptr: *const u8,
    upon_len: usize,
) {
    // SAFETY: Caller passes valid static string bytes when `upon_len > 0`.
    let upon_s = unsafe { lossy_string_from_raw(upon_ptr, upon_len) };
    with_report_table_mut(|tbl| {
        if let Some(rep) = tbl.get_mut(&report_id)
            && rep.lifecycle == ReportLifecycleState::Inactive
            && let Ok(idx) = usize::try_from(sum_idx)
            && let Some(sc) = rep.sum_configs.get_mut(idx)
        {
            if let Ok(sub_u) = usize::try_from(sub_sum_idx) {
                sc.rolling_from.push(sub_u);
            }
            if !upon_s.is_empty() {
                sc.upon_details.push(upon_s);
            }
        }
    });
}

/// Executes `INITIATE` on `report_id`, returning `0` on fresh activation or `1` if already active.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `site_ptr` - Source `<file>:<line>` UTF-8 pointer.
/// * `site_len` - Byte length of `site_ptr`.
///
/// # Safety
///
/// `site_ptr` must be valid for `site_len` bytes.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_report_initiate(
    report_id: u32,
    site_ptr: *const u8,
    site_len: usize,
) -> i32 {
    // SAFETY: Caller passes valid static site bytes.
    let site = unsafe { lossy_string_from_raw(site_ptr, site_len) };
    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return 1;
        };
        if rep.lifecycle == ReportLifecycleState::Active {
            let _ = writeln!(
                std::io::stderr(),
                "libcob: {site}: error: INITIATE {} was already done",
                rep.report_name
            );
            let _ = std::io::stderr().flush();
            return 1;
        }
        rep.lifecycle = ReportLifecycleState::Active;
        rep.page_counter = 1;
        rep.line_counter = 0;
        rep.emitted_line_on_page = 0;
        rep.flags = ReportFlags(ReportFlags::AFTER_NEW_PAGE);
        rep.after_new_controls.fill(false);
        rep.sum_values.fill(FixedDecimal::ZERO);
        rep.group_indicate_ready.fill(true);
        rep.queue.clear();
        0
    })
}

/// Updates dynamic `CODE IS` prefix and `COLUMNS` limit on `report_id` prior to `GENERATE`/`TERMINATE`.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `code_ptr` - `CODE` prefix byte pointer (`null` when no `CODE` clause).
/// * `code_len` - Byte length of `code_ptr`.
/// * `col_limit` - Horizontal column limit (`<= 0` when unbounded).
///
/// # Safety
///
/// `code_ptr` must be valid for `code_len` bytes when `code_len > 0`.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_report_set_dynamic_env(
    report_id: u32,
    code_ptr: *const u8,
    code_len: usize,
    col_limit: i64,
) {
    // SAFETY: Caller passes valid live buffer or literal pointer.
    let code = unsafe { slice_from_raw(code_ptr, code_len) }.to_vec();
    with_report_table_mut(|tbl| {
        if let Some(rep) = tbl.get_mut(&report_id) {
            rep.active_code_prefix = code;
            rep.active_col_limit = usize::try_from(col_limit).unwrap_or(0);
        }
    });
}

/// Snapshots a live control variable (`ctrl_idx`) before `GENERATE` / `TERMINATE`.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `ctrl_idx` - Zero-based control index.
/// * `live_ptr` - Live COBOL memory address of the control variable (`null` for `FINAL`).
/// * `live_len` - Byte length of the control variable (`0` for `FINAL`).
///
/// # Safety
///
/// `live_ptr` must be valid for read/write of `live_len` bytes during the statement when non-null.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_report_snapshot_control(
    report_id: u32,
    ctrl_idx: u32,
    live_ptr: *mut u8,
    live_len: usize,
) {
    // SAFETY: Caller passes live COBOL storage slice for control variable.
    let cur = unsafe { slice_from_raw(live_ptr.cast_const(), live_len) }.to_vec();
    with_report_table_mut(|tbl| {
        if let Some(rep) = tbl.get_mut(&report_id)
            && let Ok(idx) = usize::try_from(ctrl_idx)
            && let Some(slot) = rep.controls.get_mut(idx)
        {
            slot.current_bytes = cur;
            slot.live_addr = live_ptr.addr();
            slot.live_len = live_len;
        }
    });
}

/// Queues initial `RH` + `CH` groups or control-break `CF` -> `RollAndResetSums` -> `CH` phases on `rep`.
///
/// # Arguments
///
/// * `rep` - Active [`ManagedReport`] being prepared for `GENERATE`.
fn queue_generate_control_breaks(rep: &mut ManagedReport) {
    if !rep.flags.has(ReportFlags::HAS_GENERATED) {
        rep.flags.set_to(ReportFlags::HAS_GENERATED, true);
        for c in &mut rep.controls {
            c.prior_bytes.clone_from(&c.current_bytes);
        }
        rep.after_new_controls.fill(true);
        for rh_idx in rep.find_all_groups(ReportGroupKind::ReportHeading) {
            rep.queue.push_back(ReportPhase::BeginGroup {
                group_idx: rh_idx,
                on_fresh_page: false,
            });
        }
        for lvl in 0..rep.controls.len() {
            let ci = coblang_report::ControlLevelIndex::new(u16::try_from(lvl).unwrap_or(0));
            for (g_idx, g) in rep.groups.iter().enumerate() {
                if g.kind == ReportGroupKind::ControlHeading(ci) {
                    rep.queue.push_back(ReportPhase::BeginGroup {
                        group_idx: g_idx,
                        on_fresh_page: false,
                    });
                }
            }
        }
    } else if let Some(break_lvl) = rep
        .controls
        .iter()
        .position(|c| c.live_len > 0 && c.prior_bytes != c.current_bytes)
    {
        rep.queue.push_back(ReportPhase::UsePriorControls);
        for lvl in (break_lvl..rep.controls.len()).rev() {
            let ci = coblang_report::ControlLevelIndex::new(u16::try_from(lvl).unwrap_or(0));
            for (g_idx, g) in rep.groups.iter().enumerate() {
                if g.kind == ReportGroupKind::ControlFooting(ci) {
                    rep.queue.push_back(ReportPhase::BeginGroup {
                        group_idx: g_idx,
                        on_fresh_page: false,
                    });
                }
            }
            rep.queue.push_back(ReportPhase::RollAndResetSums(lvl));
        }
        rep.queue.push_back(ReportPhase::UseCurrentControls);
        for flag in rep.after_new_controls.iter_mut().skip(break_lvl) {
            *flag = true;
        }
        for lvl in break_lvl..rep.controls.len() {
            let ci = coblang_report::ControlLevelIndex::new(u16::try_from(lvl).unwrap_or(0));
            for (g_idx, g) in rep.groups.iter().enumerate() {
                if g.kind == ReportGroupKind::ControlHeading(ci) {
                    rep.queue.push_back(ReportPhase::BeginGroup {
                        group_idx: g_idx,
                        on_fresh_page: false,
                    });
                }
            }
        }
    }
}

/// Begins a `GENERATE` statement (`detail_group_idx >= 0` for detail, `-1` for summary `GENERATE report`).
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `report_name_ptr` - Report name UTF-8 pointer (used if report was never initialized).
/// * `report_name_len` - Report name byte length.
/// * `detail_group_idx` - Target `DETAIL` group index (`-1` for summary `GENERATE`).
/// * `site_ptr` - `<file>:<line>` UTF-8 pointer.
/// * `site_len` - `<file>:<line>` byte length.
///
/// # Safety
///
/// String pointers must be valid for their corresponding lengths.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_report_begin_generate(
    report_id: u32,
    report_name_ptr: *const u8,
    report_name_len: usize,
    detail_group_idx: i32,
    site_ptr: *const u8,
    site_len: usize,
) -> i32 {
    // SAFETY: Caller passes valid static string slices.
    let fallback_name = unsafe { lossy_string_from_raw(report_name_ptr, report_name_len) };
    // SAFETY: Caller passes valid static string slices.
    let site = unsafe { lossy_string_from_raw(site_ptr, site_len) };

    let inactive_name = with_report_table(|tbl| match tbl.get(&report_id) {
        Some(rep) if rep.lifecycle == ReportLifecycleState::Active => None,
        Some(rep) if !rep.report_name.is_empty() => Some(rep.report_name.clone()),
        _ => Some(fallback_name),
    });

    if let Some(rname) = inactive_name {
        let _ = writeln!(
            std::io::stderr(),
            "libcob: {site}: error: GENERATE {rname} but no INITIATE was done"
        );
        let _ = std::io::stderr().flush();
        if implicit_close_open_files_with_warning(&site) {
            std::process::exit(1);
        }
        return 1;
    }

    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return 1;
        };
        rep.queue.clear();
        rep.after_new_controls.fill(false);
        queue_generate_control_breaks(rep);

        rep.queue
            .push_back(ReportPhase::AccumulateSums(detail_group_idx));
        if let Ok(d_idx) = usize::try_from(detail_group_idx) {
            rep.queue.push_back(ReportPhase::BeginGroup {
                group_idx: d_idx,
                on_fresh_page: false,
            });
        }
        0
    })
}

/// Begins a `TERMINATE` statement on `report_id`.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `report_name_ptr` - Report name UTF-8 pointer.
/// * `report_name_len` - Report name byte length.
/// * `site_ptr` - `<file>:<line>` UTF-8 pointer.
/// * `site_len` - `<file>:<line>` byte length.
///
/// # Safety
///
/// String pointers must be valid for their corresponding lengths.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_report_begin_terminate(
    report_id: u32,
    report_name_ptr: *const u8,
    report_name_len: usize,
    site_ptr: *const u8,
    site_len: usize,
) -> i32 {
    // SAFETY: Caller passes valid static string slices.
    let fallback_name = unsafe { lossy_string_from_raw(report_name_ptr, report_name_len) };
    // SAFETY: Caller passes valid static string slices.
    let site = unsafe { lossy_string_from_raw(site_ptr, site_len) };

    let inactive_name = with_report_table(|tbl| match tbl.get(&report_id) {
        Some(rep) if rep.lifecycle == ReportLifecycleState::Active => None,
        Some(rep) if !rep.report_name.is_empty() => Some(rep.report_name.clone()),
        _ => Some(fallback_name),
    });

    if let Some(rname) = inactive_name {
        let _ = writeln!(
            std::io::stderr(),
            "libcob: {site}: error: TERMINATE {rname} but no INITIATE was done"
        );
        let _ = std::io::stderr().flush();
        if implicit_close_open_files_with_warning(&site) {
            std::process::exit(1);
        }
        return 1;
    }

    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return 1;
        };
        rep.queue.clear();
        if !rep.flags.has(ReportFlags::HAS_GENERATED) {
            rep.lifecycle = ReportLifecycleState::Inactive;
            return 0;
        }
        rep.queue.push_back(ReportPhase::UsePriorControls);
        for lvl in (0..rep.controls.len()).rev() {
            let ci = coblang_report::ControlLevelIndex::new(u16::try_from(lvl).unwrap_or(0));
            for (g_idx, g) in rep.groups.iter().enumerate() {
                if g.kind == ReportGroupKind::ControlFooting(ci) {
                    rep.queue.push_back(ReportPhase::BeginGroup {
                        group_idx: g_idx,
                        on_fresh_page: false,
                    });
                }
            }
            rep.queue.push_back(ReportPhase::RollAndResetSums(lvl));
        }
        rep.queue.push_back(ReportPhase::UseCurrentControls);
        rep.queue.push_back(ReportPhase::FinalFootings);
        rep.queue.push_back(ReportPhase::FinishTerminate);
        0
    })
}

/// Advances the report stepper to the next MLIR callback action, writing `*out_group_idx`,
/// `*out_page_counter`, and `*out_line_counter`.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `out_group_idx` - Output group index for the returned action.
/// * `out_page_counter` - Live `PAGE-COUNTER` output pointer.
/// * `out_line_counter` - Live `LINE-COUNTER` output pointer.
///
/// # Safety
///
/// Output pointers must be non-null writable `i32` addresses.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI stepper entrypoint for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_report_next_step(
    report_id: u32,
    out_group_idx: *mut i32,
    out_page_counter: *mut i32,
    out_line_counter: *mut i32,
) -> i32 {
    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return REPORT_STEP_DONE;
        };
        let code = advance_internal_phases(rep, out_group_idx);
        if !out_page_counter.is_null() {
            // SAFETY: Caller passes stack/register slot pointer.
            unsafe { *out_page_counter = i32::try_from(rep.page_counter).unwrap_or(i32::MAX) };
        }
        if !out_line_counter.is_null() {
            // SAFETY: Caller passes stack/register slot pointer.
            unsafe { *out_line_counter = i32::try_from(rep.line_counter).unwrap_or(i32::MAX) };
        }
        code
    })
}

/// Rolls subordinate `SUM` counters into parent counters at `lvl` and zeroes counters with `reset_ctrl >= lvl`.
///
/// # Arguments
///
/// * `rep` - Active [`ManagedReport`].
/// * `lvl` - Breaking control hierarchy level index.
fn roll_and_reset_sums(rep: &mut ManagedReport, lvl: usize) {
    for idx in 0..rep.sum_configs.len() {
        if rep
            .sum_configs
            .get(idx)
            .is_some_and(|sc| sc.owner_ctrl < lvl)
        {
            let subs = rep
                .sum_configs
                .get(idx)
                .map_or_default(|sc| sc.rolling_from.clone());
            for sub_i in subs {
                if rep
                    .sum_configs
                    .get(sub_i)
                    .is_some_and(|s| s.owner_ctrl == lvl)
                {
                    let delta = rep
                        .sum_values
                        .get(sub_i)
                        .copied()
                        .unwrap_or(FixedDecimal::ZERO);
                    if let Some(acc) = rep.sum_values.get_mut(idx)
                        && let Ok((next, _)) = acc.checked_add(&delta)
                    {
                        *acc = next;
                    }
                }
            }
        }
    }
    let is_field_ctrl = rep.controls.get(lvl).is_some_and(|c| c.live_addr != 0);
    for (idx, sc) in rep.sum_configs.iter().enumerate() {
        if (is_field_ctrl || sc.reset_ctrl > lvl)
            && sc.reset_ctrl >= lvl
            && let Some(val) = rep.sum_values.get_mut(idx)
        {
            *val = FixedDecimal::ZERO;
        }
    }
}

/// Queues `PageFooting` and `ReportFooting` groups at the end of `TERMINATE`.
///
/// # Arguments
///
/// * `rep` - Active [`ManagedReport`].
fn queue_final_footings(rep: &mut ManagedReport) {
    let footing_groups = if rep.emitted_line_on_page > 0 {
        rep.find_all_groups(ReportGroupKind::PageFooting)
    } else {
        Vec::new()
    };
    let report_footings = rep.find_all_groups(ReportGroupKind::ReportFooting);
    if footing_groups.is_empty() {
        for &rf_idx in report_footings.iter().rev() {
            rep.queue.push_front(ReportPhase::BeginGroup {
                group_idx: rf_idx,
                on_fresh_page: false,
            });
        }
    } else {
        if !report_footings.is_empty() {
            rep.flags.set_to(ReportFlags::SKIP_FINAL_PAD, true);
            for &rf_idx in report_footings.iter().rev() {
                rep.queue.push_front(ReportPhase::BeginGroup {
                    group_idx: rf_idx,
                    on_fresh_page: true,
                });
            }
            rep.queue.push_front(ReportPhase::StartFreshPage);
        }
        for &pf_idx in footing_groups.iter().rev() {
            rep.queue.push_front(ReportPhase::BeginGroup {
                group_idx: pf_idx,
                on_fresh_page: false,
            });
        }
    }
}

/// Processes non-interactive internal phases until reaching an MLIR callback action or `DONE`.
///
/// # Arguments
///
/// * `rep` - Active [`ManagedReport`].
/// * `out_group_idx` - Output pointer receiving the pending group or detail index.
#[expect(
    unsafe_code,
    reason = "Writes control snapshots and `*out_group_idx` to caller-owned addresses."
)]
fn advance_internal_phases(rep: &mut ManagedReport, out_group_idx: *mut i32) -> i32 {
    while let Some(phase) = rep.queue.pop_front() {
        match phase {
            ReportPhase::UsePriorControls => {
                for c in &rep.controls {
                    if c.live_addr != 0 && c.prior_bytes.len() == c.live_len {
                        let dst = core::ptr::with_exposed_provenance_mut::<u8>(c.live_addr);
                        // SAFETY: `live_addr` points to active Working-Storage / File-Section field during statement.
                        unsafe {
                            core::ptr::copy_nonoverlapping(c.prior_bytes.as_ptr(), dst, c.live_len);
                        };
                    }
                }
            }
            ReportPhase::UseCurrentControls => {
                for c in &mut rep.controls {
                    if c.live_addr != 0 && c.current_bytes.len() == c.live_len {
                        let dst = core::ptr::with_exposed_provenance_mut::<u8>(c.live_addr);
                        // SAFETY: `live_addr` points to active Working-Storage / File-Section field during statement.
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                c.current_bytes.as_ptr(),
                                dst,
                                c.live_len,
                            );
                        };
                    }
                    c.prior_bytes.clone_from(&c.current_bytes);
                }
                rep.group_indicate_ready.fill(true);
            }
            ReportPhase::RollAndResetSums(lvl) => roll_and_reset_sums(rep, lvl),
            ReportPhase::StartFreshPage => {
                rep.pad_page_to_limit();
                rep.page_counter = rep.page_counter.saturating_add(1);
                rep.line_counter = 0;
                rep.emitted_line_on_page = 0;
                rep.flags.set_to(ReportFlags::BODY_EMITTED_ON_PAGE, false);
                rep.flags.set_to(ReportFlags::PH_PRESENTED_ON_PAGE, false);
                rep.flags.set_to(ReportFlags::PF_PRESENTED_ON_PAGE, false);
                rep.flags.set_to(ReportFlags::DEFERRED_NEXT_PAGE, false);
                rep.flags.set_to(ReportFlags::AFTER_NEW_PAGE, true);
                rep.group_indicate_ready.fill(true);
            }
            ReportPhase::BeginGroup {
                group_idx,
                on_fresh_page,
            } => {
                rep.pending_group = group_idx;
                rep.flags
                    .set_to(ReportFlags::PENDING_FRESH_PAGE, on_fresh_page);
                rep.collected_specs.clear();
                rep.flags.set_to(ReportFlags::SUPPRESS_CURRENT_GROUP, false);
                if !out_group_idx.is_null() {
                    // SAFETY: Valid caller-owned `i32` slot.
                    unsafe { *out_group_idx = i32::try_from(group_idx).unwrap_or(0) };
                }
                return REPORT_STEP_CHECK_GATE;
            }
            ReportPhase::RunHook { group_idx, targets } => {
                rep.pending_group = group_idx;
                rep.render_targets = targets;
                rep.flags.set_to(ReportFlags::SUPPRESS_CURRENT_GROUP, false);
                if !out_group_idx.is_null() {
                    // SAFETY: Valid caller-owned `i32` slot.
                    unsafe { *out_group_idx = i32::try_from(group_idx).unwrap_or(0) };
                }
                return REPORT_STEP_RUN_HOOK;
            }
            ReportPhase::RenderPrepared { group_idx, targets } => {
                rep.pending_group = group_idx;
                rep.render_targets = targets;
                rep.render_line_cursor = 0;
                rep.rendered_lines.clear();
                if !out_group_idx.is_null() {
                    // SAFETY: Valid caller-owned `i32` slot.
                    unsafe { *out_group_idx = i32::try_from(group_idx).unwrap_or(0) };
                }
                return REPORT_STEP_RENDER_GROUP;
            }
            ReportPhase::AccumulateSums(detail_idx) => {
                rep.pending_group = usize::try_from(detail_idx).unwrap_or(usize::MAX);
                if !out_group_idx.is_null() {
                    // SAFETY: Valid caller-owned `i32` slot.
                    unsafe { *out_group_idx = detail_idx };
                }
                return REPORT_STEP_ACCUM_SUMS;
            }
            ReportPhase::FinalFootings => queue_final_footings(rep),
            ReportPhase::FinishTerminate => {
                if !rep.flags.has(ReportFlags::SKIP_FINAL_PAD) {
                    rep.pad_page_to_limit();
                }
                rep.lifecycle = ReportLifecycleState::Inactive;
            }
        }
    }
    REPORT_STEP_DONE
}

/// Queries `AFTER NEW` break status (`u16::MAX` = `PAGE`, `0..controls.len()` = control index).
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `ctrl_or_page` - Control index or `0xFFFF` (`u16::MAX`) for `PAGE`.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI predicate for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_report_check_after_new(report_id: u32, ctrl_or_page: u32) -> i32 {
    with_report_table(|tbl| {
        let Some(rep) = tbl.get(&report_id) else {
            return 0;
        };
        if ctrl_or_page >= u32::from(u16::MAX) {
            i32::from(rep.flags.has(ReportFlags::AFTER_NEW_PAGE) || rep.emitted_line_on_page == 0)
        } else {
            i32::from(
                usize::try_from(ctrl_or_page)
                    .ok()
                    .and_then(|i| rep.after_new_controls.get(i))
                    .copied()
                    .unwrap_or(false),
            )
        }
    })
}

/// Formats `SUM` counter `sum_idx` into a live COBOL field buffer (used both to sync named `SUM`
/// counters into Working-Storage before `PRESENT WHEN`/`USE BEFORE REPORTING` and to render `SUM` items).
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `sum_idx` - Zero-based `SUM` counter index.
/// * `dst_ptr` - Destination COBOL buffer pointer.
/// * `dst_len` - Destination byte length.
/// * `dst_spec_ptr` - Serialized destination `RuntimeFieldSpec` pointer.
/// * `dst_spec_len` - Byte length of `dst_spec_ptr`.
///
/// # Safety
///
/// `dst_ptr` must be writable for `dst_len` bytes and `dst_spec_ptr` readable for `dst_spec_len` bytes.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI formatter for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_report_sync_sum_field(
    report_id: u32,
    sum_idx: u32,
    dst_ptr: *mut u8,
    dst_len: usize,
    dst_spec_ptr: *const u8,
    dst_spec_len: usize,
) {
    let val = with_report_table(|tbl| {
        usize::try_from(sum_idx)
            .ok()
            .and_then(|i| tbl.get(&report_id).and_then(|rep| rep.sum_values.get(i)))
            .copied()
            .unwrap_or(FixedDecimal::ZERO)
    });
    if dst_ptr.is_null() || dst_len == 0 || dst_spec_ptr.is_null() || dst_spec_len == 0 {
        return;
    }
    let ascii = val.to_string();
    // SAFETY: Caller guarantees non-aliasing static `dst_spec_ptr` and writable `dst_ptr`.
    let dst_spec = unsafe { core::slice::from_raw_parts(dst_spec_ptr, dst_spec_len) };
    // SAFETY: Caller guarantees `[dst_ptr..dst_ptr + dst_len)` is writable.
    let dst_slice = unsafe { core::slice::from_raw_parts_mut(dst_ptr, dst_len) };
    let _ = runtime_execute_move(
        RuntimeMoveSourceKind::DecimalAscii,
        ascii.as_bytes(),
        &[],
        dst_slice,
        dst_spec,
    );
}

/// Reports the root `PRESENT WHEN` / `AFTER NEW` gate outcome for the current `pending_group`.
/// Returns `1` if MLIR should now run `COLLECT_LINES`, or `0` if the group is skipped.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `is_present` - Non-zero when the group is visible.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_report_submit_gate(report_id: u32, is_present: i32) -> i32 {
    if is_present == 0 {
        return 0;
    }
    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return 0;
        };
        rep.collected_specs.clear();
        1
    })
}

/// Appends a visible line positioning spec during Pass A (`COLLECT_LINES`).
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `line_kind` - `1=Absolute, 2=Relative, 3=OnNextPage`.
/// * `line_val` - Line number or relative offset.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_report_collect_line_spec(
    report_id: u32,
    line_kind: u32,
    line_val: u32,
) {
    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return;
        };
        let spec = match line_kind {
            1 => RtLineSpec::Absolute(line_val.max(1)),
            3 => RtLineSpec::OnNextPage(line_val.max(1)),
            _ => RtLineSpec::Relative(line_val),
        };
        rep.collected_specs.push(spec);
    });
}

/// Resolves pagination for a paged body group (`CH`/`DE`/`CF`).
///
/// # Arguments
///
/// * `rep` - Active [`ManagedReport`].
/// * `g_idx` - Group index being collected.
/// * `grp` - Cloned group configuration.
/// * `lim` - Validated [`PageLimits`].
fn finish_collect_paged_body(
    rep: &mut ManagedReport,
    g_idx: usize,
    grp: &RtGroupConfig,
    lim: PageLimits,
) {
    let fresh = rep.flags.has(ReportFlags::PENDING_FRESH_PAGE);
    if rep.flags.has(ReportFlags::DEFERRED_NEXT_PAGE) && !fresh {
        queue_overflow_break_and_retry(rep, g_idx);
        return;
    }
    if !rep.flags.has(ReportFlags::PH_PRESENTED_ON_PAGE) {
        let heading_groups = rep.find_all_groups(ReportGroupKind::PageHeading);
        if !heading_groups.is_empty() {
            rep.flags.set_to(ReportFlags::PH_PRESENTED_ON_PAGE, true);
            rep.queue.push_front(ReportPhase::BeginGroup {
                group_idx: g_idx,
                on_fresh_page: fresh,
            });
            for &ph_idx in heading_groups.iter().rev() {
                rep.queue.push_front(ReportPhase::BeginGroup {
                    group_idx: ph_idx,
                    on_fresh_page: true,
                });
            }
            return;
        }
    }

    let ceiling = lim.last_detail();
    let fit = resolve_targets_for_specs(
        &rep.collected_specs,
        rep.line_counter,
        Some(lim.first_detail()),
        None,
        fresh || !rep.flags.has(ReportFlags::BODY_EMITTED_ON_PAGE),
    )
    .filter(|t| t.last().copied().unwrap_or(0) <= ceiling);

    if let Some(targets) = fit {
        queue_hook_and_render(rep, g_idx, grp.has_hook, targets);
    } else if !fresh {
        queue_overflow_break_and_retry(rep, g_idx);
    }
}

/// Completes Pass A (`COLLECT_LINES`), resolving pagination (`PF` -> new page -> `PH` if overflowing)
/// and queueing `RunHook` + `RenderPrepared`.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI pagination resolver for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_report_finish_collect(report_id: u32) {
    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return;
        };
        let g_idx = rep.pending_group;
        let Some(grp) = rep.groups.get(g_idx).cloned() else {
            return;
        };
        if rep.collected_specs.is_empty() {
            if grp.has_hook {
                rep.queue.push_front(ReportPhase::RunHook {
                    group_idx: g_idx,
                    targets: Vec::new(),
                });
            }
            return;
        }

        let is_body = matches!(
            grp.kind,
            ReportGroupKind::ControlHeading(_)
                | ReportGroupKind::Detail
                | ReportGroupKind::ControlFooting(_)
        );

        if is_body && let Some(lim) = rep.page_limits {
            finish_collect_paged_body(rep, g_idx, &grp, lim);
            return;
        }

        if grp.kind == ReportGroupKind::ReportFooting
            && let Some(lim) = rep.page_limits
        {
            let fresh = rep.flags.has(ReportFlags::PENDING_FRESH_PAGE);
            let fit = resolve_targets_for_specs(
                &rep.collected_specs,
                rep.line_counter,
                None,
                Some(lim.heading()),
                fresh || rep.emitted_line_on_page == 0,
            )
            .filter(|t| t.last().copied().unwrap_or(0) <= lim.page_limit());
            if let Some(targets) = fit {
                queue_hook_and_render(rep, g_idx, grp.has_hook, targets);
            } else if !fresh && rep.emitted_line_on_page > 0 {
                rep.queue.push_front(ReportPhase::BeginGroup {
                    group_idx: g_idx,
                    on_fresh_page: true,
                });
                rep.queue.push_front(ReportPhase::StartFreshPage);
            }
            return;
        }

        queue_header_or_footer_group(rep, g_idx, &grp);
    });
}

/// Resolves and queues a `ReportHeading`, `PageHeading`, `PageFooting`, or unpaged `ReportFooting`.
///
/// # Arguments
///
/// * `rep` - Active [`ManagedReport`].
/// * `g_idx` - Group index being collected.
/// * `grp` - Group configuration.
fn queue_header_or_footer_group(rep: &mut ManagedReport, g_idx: usize, grp: &RtGroupConfig) {
    let heading_floor = if matches!(
        grp.kind,
        ReportGroupKind::ReportHeading | ReportGroupKind::PageHeading
    ) {
        rep.page_limits.map(PageLimits::heading)
    } else {
        None
    };
    let start_lc = if grp.kind == ReportGroupKind::PageFooting {
        let base = if !rep.flags.has(ReportFlags::PF_PRESENTED_ON_PAGE)
            && let Some(lim) = rep.page_limits
        {
            rep.line_counter.max(lim.last_detail())
        } else {
            rep.line_counter
        };
        rep.flags.set_to(ReportFlags::PF_PRESENTED_ON_PAGE, true);
        base
    } else {
        rep.line_counter
    };
    if grp.kind == ReportGroupKind::PageHeading {
        rep.flags.set_to(ReportFlags::PH_PRESENTED_ON_PAGE, true);
    }
    if let Some(targets) =
        resolve_targets_for_specs(&rep.collected_specs, start_lc, None, heading_floor, true)
    {
        queue_hook_and_render(rep, g_idx, grp.has_hook, targets);
    }
    if grp.kind == ReportGroupKind::ReportHeading
        && matches!(grp.next_group, RtNextGroup::NextPage)
        && rep.page_limits.is_some()
    {
        let popped_hook = if grp.has_hook {
            rep.queue.pop_front()
        } else {
            None
        };
        let popped_render = rep.queue.pop_front();
        rep.queue.push_front(ReportPhase::StartFreshPage);
        if let Some(r) = popped_render {
            rep.queue.push_front(r);
        }
        if let Some(h) = popped_hook {
            rep.queue.push_front(h);
        }
    }
}

/// Pushes `[BeginGroup(PF..), StartFreshPage, BeginGroup(PH..), BeginGroup(g_idx, fresh=true)]` onto the front of `rep.queue`.
///
/// # Arguments
///
/// * `rep` - Active [`ManagedReport`].
/// * `g_idx` - Overflowing group index to retry on the next page.
fn queue_overflow_break_and_retry(rep: &mut ManagedReport, g_idx: usize) {
    rep.queue.push_front(ReportPhase::BeginGroup {
        group_idx: g_idx,
        on_fresh_page: true,
    });
    let heading_groups = rep.find_all_groups(ReportGroupKind::PageHeading);
    if !heading_groups.is_empty() {
        for &ph_idx in heading_groups.iter().rev() {
            rep.queue.push_front(ReportPhase::BeginGroup {
                group_idx: ph_idx,
                on_fresh_page: true,
            });
        }
    }
    rep.queue.push_front(ReportPhase::StartFreshPage);
    if rep.emitted_line_on_page > 0 {
        let footing_groups = rep.find_all_groups(ReportGroupKind::PageFooting);
        for &pf_idx in footing_groups.iter().rev() {
            rep.queue.push_front(ReportPhase::BeginGroup {
                group_idx: pf_idx,
                on_fresh_page: false,
            });
        }
    }
}

/// Pushes `RenderPrepared` (preceded by `RunHook` when `has_hook` is true) onto the front of `rep.queue`.
///
/// # Arguments
///
/// * `rep` - Active [`ManagedReport`].
/// * `group_idx` - Group index to render.
/// * `has_hook` - Whether a `USE BEFORE REPORTING` declarative exists for `group_idx`.
/// * `targets` - Resolved 1-based physical line targets.
fn queue_hook_and_render(
    rep: &mut ManagedReport,
    group_idx: usize,
    has_hook: bool,
    targets: Vec<u32>,
) {
    rep.queue.push_front(ReportPhase::RenderPrepared {
        group_idx,
        targets: targets.clone(),
    });
    if has_hook {
        rep.queue
            .push_front(ReportPhase::RunHook { group_idx, targets });
    }
}

/// Resolves 1-based page line numbers for `specs` starting from `start_lc`.
///
/// # Arguments
///
/// * `specs` - Collected [`RtLineSpec`] sequence.
/// * `start_lc` - Starting `LINE-COUNTER` value.
/// * `first_detail_floor` - Optional `FIRST DETAIL` floor for body groups.
/// * `heading_floor` - Optional `HEADING` floor for header/report-footing groups.
/// * `is_fresh_page` - Whether resolution occurs at the top of a fresh page.
fn resolve_targets_for_specs(
    specs: &[RtLineSpec],
    start_lc: u32,
    first_detail_floor: Option<u32>,
    heading_floor: Option<u32>,
    is_fresh_page: bool,
) -> Option<Vec<u32>> {
    let mut out = Vec::with_capacity(specs.len());
    let mut lc = start_lc;
    for (idx, &spec) in specs.iter().enumerate() {
        let target = match spec {
            RtLineSpec::Absolute(n) => {
                if n <= lc {
                    return None;
                }
                n
            }
            RtLineSpec::OnNextPage(n) => {
                if !is_fresh_page && idx == 0 {
                    return None;
                }
                let eff = first_detail_floor.map_or(n, |fd| n.max(fd));
                if eff <= lc {
                    return None;
                }
                eff
            }
            RtLineSpec::Relative(delta) => {
                if idx == 0 {
                    if let Some(fd) = first_detail_floor {
                        if lc < fd {
                            fd.max(lc.saturating_add(delta))
                        } else {
                            lc.saturating_add(delta)
                        }
                    } else if let Some(hf) = heading_floor
                        && lc == 0
                    {
                        hf.saturating_add(delta.saturating_sub(1)).max(1)
                    } else {
                        lc.saturating_add(delta).max(1)
                    }
                } else {
                    lc.saturating_add(delta)
                }
            }
        };
        lc = target;
        out.push(target);
    }
    Some(out)
}

/// Marks `suppress_current_group = true` when `SUPPRESS PRINTING` executes inside `DECLARATIVES`.
///
/// # Arguments
///
/// * `report_id` - Active report handle ID (`u32::MAX` suppresses all currently active reports).
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI entrypoint for M18 `SUPPRESS PRINTING`."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_report_suppress(report_id: u32) {
    with_report_table_mut(|tbl| {
        if report_id == u32::MAX {
            for rep in tbl.values_mut() {
                if rep.lifecycle == ReportLifecycleState::Active {
                    rep.flags.set_to(ReportFlags::SUPPRESS_CURRENT_GROUP, true);
                }
            }
        } else if let Some(rep) = tbl.get_mut(&report_id) {
            rep.flags.set_to(ReportFlags::SUPPRESS_CURRENT_GROUP, true);
        }
    });
}

/// Syncs `PAGE-COUNTER` and `PRINT-SWITCH` after a `USE BEFORE REPORTING` declarative finishes.
/// If suppressed, drops the queued `RenderPrepared` step for `pending_group`.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `new_page_counter` - Updated `PAGE-COUNTER` read from COBOL storage.
/// * `print_switch` - Updated `PRINT-SWITCH` read from COBOL storage (`!= 0` suppresses group).
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI post-hook sync for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_report_after_hook(
    report_id: u32,
    new_page_counter: i32,
    print_switch: i32,
) {
    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return;
        };
        if let Ok(pc) = u32::try_from(new_page_counter) {
            rep.page_counter = pc;
        }
        if print_switch != 0 {
            rep.flags.set_to(ReportFlags::SUPPRESS_CURRENT_GROUP, true);
        }
        if rep.flags.has(ReportFlags::SUPPRESS_CURRENT_GROUP)
            && matches!(rep.queue.front(), Some(ReportPhase::RenderPrepared { .. }))
        {
            let _ = rep.queue.pop_front();
        }
    });
}

/// Starts rendering the next resolved print line during `REPORT_STEP_RENDER_GROUP`,
/// returning the 1-based `target_line` (so MLIR updates `LINE-COUNTER` BEFORE evaluating `SOURCE` items!).
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI line starter for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_report_begin_render_line(report_id: u32) -> i32 {
    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return 1;
        };
        let target = rep
            .render_targets
            .get(rep.render_line_cursor)
            .copied()
            .unwrap_or_else(|| rep.line_counter.saturating_add(1));
        rep.render_line_cursor = rep.render_line_cursor.saturating_add(1);
        rep.line_counter = target;
        rep.rendered_lines.push(ActiveRenderLine {
            target_line: target,
            buf: Vec::new(),
            right_col_1based: 0,
        });
        i32::try_from(target).unwrap_or(1)
    })
}

/// Places a formatted field slice onto the active print line during `REPORT_STEP_RENDER_GROUP`.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `col_kind` - `0=None (omit), 1=Absolute, 2=Relative`.
/// * `col_val` - 1-based column or relative offset.
/// * `align_code` - `0=Left, 1=Center, 2=Right`.
/// * `group_indicate` - `1` if `GROUP INDICATE` applies to this field.
/// * `data_ptr` - Formatted ASCII/EBCDIC byte slice pointer.
/// * `data_len` - Byte length of `data_ptr`.
///
/// # Safety
///
/// `data_ptr` must be valid for `data_len` bytes when `data_len > 0`.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI column placer for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_report_put_item(
    report_id: u32,
    col_kind: u32,
    col_val: u32,
    align_code: u32,
    group_indicate: i32,
    data_ptr: *const u8,
    data_len: usize,
) {
    if col_kind == 0 {
        return;
    }
    // SAFETY: Caller passes valid formatted byte buffer.
    let raw = unsafe { slice_from_raw(data_ptr, data_len) };
    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return;
        };
        let g_idx = rep.pending_group;
        let is_detail = rep
            .groups
            .get(g_idx)
            .is_some_and(|g| g.kind == ReportGroupKind::Detail);
        let indicate_ok = rep.group_indicate_ready.get(g_idx).copied().unwrap_or(true);
        let suppressed = is_detail && group_indicate != 0 && !indicate_ok;

        let Some(line) = rep.rendered_lines.last_mut() else {
            return;
        };

        let base_align = align_code & 0x3;
        let is_literal = (align_code & 0x4) != 0;
        let step_slot_cols = align_code >> 8;

        let (slice_to_write, start_col_1based) = compute_aligned_placement(
            raw,
            col_kind,
            col_val,
            base_align,
            is_literal,
            line.right_col_1based,
        );

        let w_u32 = u32::try_from(slice_to_write.len()).unwrap_or(0);
        let natural_right = if w_u32 == 0 {
            start_col_1based
        } else {
            start_col_1based.saturating_add(w_u32).saturating_sub(1)
        };
        line.right_col_1based = if step_slot_cols > 0 {
            natural_right.max(
                start_col_1based
                    .saturating_add(step_slot_cols)
                    .saturating_sub(1),
            )
        } else {
            natural_right
        };

        if suppressed || slice_to_write.is_empty() {
            return;
        }

        let start_idx = usize::try_from(start_col_1based.saturating_sub(1)).unwrap_or(0);
        let needed = start_idx.saturating_add(slice_to_write.len());
        if line.buf.len() < needed {
            line.buf.resize(needed, b' ');
        }
        for (dst, &b) in line
            .buf
            .iter_mut()
            .skip(start_idx)
            .zip(slice_to_write.iter())
        {
            *dst = b;
        }
    });
}

/// Computes `(trimmed_or_full_slice, 1_based_start_col)` for `LEFT`, `CENTER`, and `RIGHT` column alignment.
///
/// # Arguments
///
/// * `raw` - Formatted item bytes.
/// * `col_kind` - `1=Absolute, 2=Relative`.
/// * `col_val` - 1-based column or relative offset.
/// * `align_code` - `0=Default, 1=Center, 2=Right, 3=Left`.
/// * `is_literal` - Whether `raw` comes from a literal `VALUE` item.
/// * `prev_right_col` - Previous item's 1-based rightmost column on the line.
fn compute_aligned_placement(
    raw: &[u8],
    col_kind: u32,
    col_val: u32,
    align_code: u32,
    is_literal: bool,
    prev_right_col: u32,
) -> (&[u8], u32) {
    let anchor = if col_kind == 2 {
        prev_right_col.saturating_add(col_val).max(1)
    } else {
        col_val.max(1)
    };
    match align_code {
        1 => {
            if is_literal {
                let half = u32::try_from(raw.len().saturating_sub(1) / 2).unwrap_or(0);
                (
                    trim_trailing_ascii_spaces(raw),
                    anchor.saturating_sub(half).max(1),
                )
            } else {
                let trimmed = trim_ascii_spaces(raw);
                let half_usize = if trimmed.len() == raw.len() {
                    trimmed.len().saturating_sub(1) / 2
                } else {
                    trimmed.len() / 2
                };
                let half = u32::try_from(half_usize).unwrap_or(0);
                (trimmed, anchor.saturating_sub(half).max(1))
            }
        }
        2 => {
            let trimmed = trim_trailing_ascii_spaces(raw);
            let len_u32 = u32::try_from(trimmed.len()).unwrap_or(1).max(1);
            (
                trimmed,
                anchor.saturating_add(1).saturating_sub(len_u32).max(1),
            )
        }
        3 => {
            let first = raw.iter().position(|&b| b != b' ').unwrap_or(raw.len());
            (raw.get(first..).unwrap_or(&[]), anchor)
        }
        _ => (raw, anchor),
    }
}

/// Trims leading and trailing ASCII spaces (`0x20`).
///
/// # Arguments
///
/// * `bytes` - Input byte slice.
fn trim_ascii_spaces(bytes: &[u8]) -> &[u8] {
    let first = bytes.iter().position(|&b| b != b' ').unwrap_or(bytes.len());
    let tail = bytes.get(first..).unwrap_or(&[]);
    trim_trailing_ascii_spaces(tail)
}

/// Trims trailing ASCII spaces (`0x20`).
///
/// # Arguments
///
/// * `bytes` - Input byte slice.
fn trim_trailing_ascii_spaces(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
    bytes.get(..end).unwrap_or(&[])
}

/// Inspects `rep.queue` after a `ControlFooting` renders and returns `(suppress_all, suppress_next_page)`:
/// - `suppress_all`: true during `TERMINATE` (`FinishTerminate` queued), per ISO Report Writer rules.
/// - `suppress_next_page`: true when any enclosing `ControlFooting` remains in the current break.
///
/// # Arguments
///
/// * `rep` - Active [`ManagedReport`].
fn classify_cf_next_group_suppression(rep: &ManagedReport) -> (bool, bool) {
    if rep
        .queue
        .iter()
        .any(|p| matches!(p, ReportPhase::FinishTerminate))
    {
        return (true, true);
    }
    for phase in &rep.queue {
        match phase {
            ReportPhase::BeginGroup { group_idx, .. } => {
                if rep
                    .groups
                    .get(*group_idx)
                    .is_some_and(|g| matches!(g.kind, ReportGroupKind::ControlFooting(_)))
                {
                    return (false, true);
                }
            }
            ReportPhase::UseCurrentControls => return (false, false),
            _ => {}
        }
    }
    (false, false)
}

/// Applies `NEXT GROUP` advancement to `rep` after a body or `ReportHeading` group renders.
///
/// # Arguments
///
/// * `rep` - Active [`ManagedReport`].
/// * `grp` - Rendered group configuration.
/// * `is_body` - Whether `grp` is `CH`, `DE`, or `CF`.
/// * `eff_next_group` - Effective [`RtNextGroup`] after `CF` suppression rules.
fn apply_post_render_next_group(
    rep: &mut ManagedReport,
    grp: &RtGroupConfig,
    is_body: bool,
    eff_next_group: RtNextGroup,
) {
    if is_body {
        if let Some(lim) = rep.page_limits {
            match eff_next_group {
                RtNextGroup::None => {}
                RtNextGroup::NextPage => rep.flags.set_to(ReportFlags::DEFERRED_NEXT_PAGE, true),
                RtNextGroup::Absolute(n) => {
                    if n > rep.line_counter && n <= lim.footing() {
                        rep.line_counter = n;
                    } else {
                        rep.flags.set_to(ReportFlags::DEFERRED_NEXT_PAGE, true);
                    }
                }
                RtNextGroup::Relative(d) => {
                    let cand = rep.line_counter.saturating_add(d);
                    if cand <= lim.footing() {
                        rep.line_counter = cand;
                    } else {
                        rep.flags.set_to(ReportFlags::DEFERRED_NEXT_PAGE, true);
                    }
                }
            }
        } else {
            match eff_next_group {
                RtNextGroup::Absolute(n) if n > rep.line_counter => rep.line_counter = n,
                RtNextGroup::Relative(d) => {
                    rep.line_counter = rep.line_counter.saturating_add(d);
                }
                RtNextGroup::None | RtNextGroup::NextPage | RtNextGroup::Absolute(_) => {}
            }
        }
    } else if grp.kind == ReportGroupKind::ReportHeading {
        match grp.next_group {
            RtNextGroup::Absolute(n) if n > rep.line_counter => rep.line_counter = n,
            RtNextGroup::Relative(d) => rep.line_counter = rep.line_counter.saturating_add(d),
            _ => {}
        }
    }
}

/// Flushes all rendered lines of `pending_group` to the open `FD`, updates `GROUP INDICATE`,
/// and applies `NEXT GROUP`.
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI group committer for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_report_finish_render_group(report_id: u32) {
    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return;
        };
        let g_idx = rep.pending_group;
        let Some(grp) = rep.groups.get(g_idx).cloned() else {
            return;
        };
        let lines = std::mem::take(&mut rep.rendered_lines);
        let had_lines = !lines.is_empty();
        rep.flush_lines_to_fd(&lines);

        if grp.kind == ReportGroupKind::Detail {
            if let Some(flag) = rep.group_indicate_ready.get_mut(g_idx) {
                *flag = false;
            }
            if had_lines {
                rep.flags.set_to(ReportFlags::AFTER_NEW_PAGE, false);
            }
        }

        let is_body = matches!(
            grp.kind,
            ReportGroupKind::ControlHeading(_)
                | ReportGroupKind::Detail
                | ReportGroupKind::ControlFooting(_)
        );
        if is_body && had_lines {
            rep.flags.set_to(ReportFlags::BODY_EMITTED_ON_PAGE, true);
        }
        let (suppress_all, suppress_np) = if matches!(grp.kind, ReportGroupKind::ControlFooting(_))
        {
            classify_cf_next_group_suppression(rep)
        } else {
            (false, false)
        };
        let eff_next_group =
            if suppress_all || (suppress_np && matches!(grp.next_group, RtNextGroup::NextPage)) {
                RtNextGroup::None
            } else {
                grp.next_group
            };
        apply_post_render_next_group(rep, &grp, is_body, eff_next_group);
    });
}

/// Adds a decimal addend `(unscaled_hi, unscaled_lo, scale)` to `SUM` counter `sum_idx` when
/// `UPON` detail filtering matches the active `GENERATE` target (`detail_group_idx`).
///
/// # Arguments
///
/// * `report_id` - Report handle ID.
/// * `sum_idx` - Zero-based `SUM` counter index.
/// * `detail_group_idx` - Active `DETAIL` group index (`-1` for summary `GENERATE`).
/// * `val_str_ptr` - Decimal string representation pointer (e.g. from `m20_dec` or formatted numeric).
/// * `val_str_len` - Byte length of `val_str_ptr`.
///
/// # Safety
///
/// `val_str_ptr` must be valid for `val_str_len` bytes.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI `SUM` accumulator for M18 Report Writer."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_report_add_sum_ascii(
    report_id: u32,
    sum_idx: u32,
    detail_group_idx: i32,
    val_str_ptr: *const u8,
    val_str_len: usize,
) {
    // SAFETY: Caller passes valid numeric string bytes.
    let raw = unsafe { lossy_string_from_raw(val_str_ptr, val_str_len) };
    let Some(addend) = parse_decimal_bytes(raw.trim()) else {
        return;
    };
    with_report_table_mut(|tbl| {
        let Some(rep) = tbl.get_mut(&report_id) else {
            return;
        };
        let Ok(s_idx) = usize::try_from(sum_idx) else {
            return;
        };
        let allowed = if let Some(sc) = rep.sum_configs.get(s_idx) {
            if sc.upon_details.is_empty() {
                true
            } else if let Ok(d_idx) = usize::try_from(detail_group_idx)
                && let Some(dg) = rep.groups.get(d_idx)
            {
                sc.upon_details
                    .iter()
                    .any(|u| u.eq_ignore_ascii_case(&dg.name))
            } else {
                false
            }
        } else {
            false
        };
        if allowed
            && let Some(acc) = rep.sum_values.get_mut(s_idx)
            && let Ok((next, _)) = acc.checked_add(&addend)
        {
            *acc = next;
        }
    });
}

/// Parses a signed decimal string (`"123.45"`, `"-007"`, `"+12"`) into a [`FixedDecimal`].
///
/// # Arguments
///
/// * `s` - Trimmed ASCII numeric text.
fn parse_decimal_bytes(s: &str) -> Option<FixedDecimal> {
    if s.is_empty() {
        return Some(FixedDecimal::ZERO);
    }
    let (neg, rest) = if let Some(r) = s.strip_prefix('-') {
        (true, r.trim())
    } else if let Some(r) = s.strip_prefix('+') {
        (false, r.trim())
    } else {
        (false, s)
    };
    let (rest, trailing_neg) = if let Some(r) = rest.strip_suffix('-') {
        (r.trim(), true)
    } else if let Some(r) = rest.strip_suffix('+') {
        (r.trim(), false)
    } else {
        (rest, false)
    };
    let is_neg = neg || trailing_neg;
    let mut cleaned = String::with_capacity(rest.len() + 1);
    if is_neg {
        cleaned.push('-');
    }
    let mut saw_digit = false;
    for ch in rest.chars() {
        if ch.is_ascii_digit() {
            saw_digit = true;
            cleaned.push(ch);
        } else if ch == '.' {
            cleaned.push('.');
        }
    }
    if !saw_digit {
        return Some(FixedDecimal::ZERO);
    }
    FixedDecimal::parse_str(&cleaned).ok()
}
