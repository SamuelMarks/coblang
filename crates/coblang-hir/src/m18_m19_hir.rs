//! Strongly-typed Milestone M18 (`REPORT SECTION`) and Milestone M19 (`SCREEN SECTION` & TUI) HIR nodes.

#![deny(clippy::indexing_slicing)]

use coblang_common::{DataItemId, FileConnectorId, ProcedureId, ReportId, SourceSpan};

use crate::{HirCondition, HirExpr, HirPlace, HirStmt, HirType, JustifiedSpec};

// ============================================================================
// Milestone M18: Declarative REPORT SECTION HIR
// ============================================================================

/// Control hierarchy entry declared in `RD ... CONTROLS ARE ...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirReportControlSpec {
    /// Top-most `FINAL` control break level.
    Final,
    /// Data-driven control break field (`Place` snapshot compared on `GENERATE`).
    Field {
        /// Canonical COBOL control field name.
        name: String,
        /// Addressable Data Division storage place.
        place: HirPlace,
    },
}

/// Vertical `LINE NUMBER IS ...` positioning specification in HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirReportLineSpec {
    /// Absolute 1-based page line (`LINE n`).
    Absolute(u32),
    /// Relative line offset (`LINE PLUS n` / `LINE + n`).
    Relative(u32),
    /// Advance to next page and place on 1-based line (`LINE n ON NEXT PAGE` / `LINE NEXT PAGE`).
    OnNextPage(u32),
}

/// Post-presentation `NEXT GROUP IS ...` spacing specification in HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirReportNextGroupSpec {
    /// Advance `LINE-COUNTER` to absolute 1-based line (`NEXT GROUP n`).
    Absolute(u32),
    /// Advance `LINE-COUNTER` by relative offset (`NEXT GROUP PLUS n`).
    Relative(u32),
    /// Force page break before next body group (`NEXT GROUP NEXT PAGE`).
    NextPage,
}

/// Horizontal column alignment mode (`LEFT`, `CENTER`, `RIGHT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HirReportColAlign {
    /// Standard left-aligned start column (`COLUMN n`).
    #[default]
    Left,
    /// Explicit `COLUMN LEFT` trimming leading spaces (`COLUMN LEFT n` / `COLUMN LEFT PLUS n`).
    ExplicitLeft,
    /// Center field around 1-based column `n` (`COLUMN CENTER n`).
    Center,
    /// Right-align last character at 1-based column `n` (`COLUMN RIGHT n`).
    Right,
}

/// Horizontal `COLUMN NUMBER IS ...` positioning specification in HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirReportColSpec {
    /// Absolute 1-based column with alignment (`COLUMN [LEFT|CENTER|RIGHT] n`).
    Absolute {
        /// 1-based column coordinate (`>= 1`).
        col: u32,
        /// Horizontal alignment relative to `col`.
        align: HirReportColAlign,
    },
    /// Relative spacing from the previous column end (`COLUMN [LEFT|CENTER|RIGHT] PLUS n`).
    Relative {
        /// Relative column delta (`0` = immediately adjacent).
        offset: u32,
        /// Horizontal alignment relative to computed column.
        align: HirReportColAlign,
    },
}

/// Value origin (`VALUE`, `SOURCE`, `SUM`, `PAGE-COUNTER`, `LINE-COUNTER`) for an elementary report item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirReportSourceKind {
    /// Constant literal bytes (`VALUE "..."`).
    LiteralBytes(Vec<u8>),
    /// Live Data Division storage place (`SOURCE id`).
    Place(HirPlace),
    /// General arithmetic/intrinsic expression (`SOURCE (a + b)`).
    Expr(HirExpr),
    /// `CONTROL FOOTING` `SUM` accumulator index (`0..sum_counters.len()`).
    SumCounter(u32),
    /// Per-report `PAGE-COUNTER` special register.
    PageCounter,
    /// Per-report `LINE-COUNTER` special register.
    LineCounter,
}

/// Declarative `[NOT] PRESENT WHEN / AFTER NEW` visibility guard in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirReportPresentKind {
    /// Always visible.
    Always,
    /// Arbitrary COBOL boolean condition (`PRESENT WHEN cond`).
    Condition(Box<HirCondition>),
    /// Visible after a break on control index `0..controls.len()` or initial page break (`u16::MAX` for `PAGE`).
    AfterNewControl {
        /// Control index in `HirReportPlan::controls`, or `u16::MAX` for `AFTER NEW PAGE`.
        control_idx: u16,
    },
    /// Disjunctive `AFTER [NEW] c1 OR c2 OR PAGE` (`u16::MAX` = `PAGE`).
    AnyAfterNew(Vec<u16>),
    /// Logical negation (`NOT PRESENT WHEN ...` / `ABSENT WHEN ...`).
    Not(Box<Self>),
}

/// Elementary report field output formatter in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirReportFormatterKind {
    /// Raw unpadded bytes.
    RawBytes,
    /// Fixed-width alphanumeric column (`width`, `justified`, `space_byte`).
    FixedAlpha {
        /// Column byte width (`>= 1`).
        width: u32,
        /// Left vs right justification.
        justified: JustifiedSpec,
        /// Padding byte (`0x20` ASCII, `0x40` EBCDIC).
        space_byte: u8,
    },
    /// Compiled `PICTURE` formatting (`MoveFieldDesc` derived from `ty` + program formatting).
    Picture {
        /// Output byte width (`>= 1`).
        byte_len: u32,
        /// Compiled `PICTURE` [`HirType`].
        ty: HirType,
    },
}

/// Lowered `SUM ... [UPON ...] [RESET ON ...]` counter plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirReportSumSpec {
    /// 0-based counter slot index.
    pub counter_idx: u32,
    /// Optional COBOL sum-counter name.
    pub name: Option<String>,
    /// Optional synthesized Working-Storage [`DataItemId`] mirroring this sum counter for `DECLARATIVES`.
    pub backing_item: Option<DataItemId>,
    /// Control hierarchy index owning this counter.
    pub owner_control_idx: u16,
    /// Control hierarchy index whose break resets this counter.
    pub reset_control_idx: u16,
    /// Non-rolling expression/field addends summed on `GENERATE`.
    pub addend_exprs: Vec<HirExpr>,
    /// Subordinate `counter_idx` slots rolled into this counter upon subordinate control footing.
    pub rolling_counters: Vec<u32>,
    /// Optional uppercase `DETAIL` group names filtering `addend_exprs` accumulation.
    pub upon_details: Vec<String>,
}

/// `USE [GLOBAL] BEFORE REPORTING <group>` declarative hook binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirReportHookPlan {
    /// Target uppercase `01` report group name.
    pub group_name: String,
    /// Inclusive starting [`ProcedureId`] of the declarative section.
    pub start_proc: ProcedureId,
    /// Inclusive ending [`ProcedureId`] of the declarative section.
    pub end_proc: ProcedureId,
}

/// `01`-level report group role in HIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirReportGroupRole {
    /// `REPORT HEADING` (`RH`).
    ReportHeading,
    /// `PAGE HEADING` (`PH`).
    PageHeading,
    /// `CONTROL HEADING` (`CH`) for 0-based control index.
    ControlHeading(u16),
    /// `DETAIL` (`DE`).
    Detail,
    /// `CONTROL FOOTING` (`CF`) for 0-based control index.
    ControlFooting(u16),
    /// `PAGE FOOTING` (`PF`).
    PageFooting,
    /// `REPORT FOOTING` (`RF`).
    ReportFooting,
}

/// `GROUP INDICATE` policy on an elementary report item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HirGroupIndicateMode {
    /// Print on every `DETAIL` presentation.
    #[default]
    AlwaysDisplay,
    /// Print on first detail after control/page break, then blank until next break (`GROUP INDICATE`).
    GroupIndicate,
}

/// Repeating `OCCURS` expansion metadata on a report node (`OCCURS n [TO m DEPENDING ON dep] [STEP s]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirReportOccursSpec {
    /// Maximum static occurrence count (`>= 1`).
    pub max_count: u32,
    /// Optional dynamic `DEPENDING ON` count expression.
    pub depending_on: Option<HirExpr>,
    /// Optional horizontal `STEP` column delta between occurrences.
    pub step_cols: Option<u32>,
    /// Optional `VARYING` index Working-Storage place updated `1..=count` during rendering.
    pub varying_place: Option<(HirPlace, i64, i64)>,
    /// Static byte stride in the synthesized `01` report-group storage buffer (`0` if unbacked).
    pub occurs_byte_stride: u32,
}

/// Elementary leaf report item in [`HirReportGroupPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirReportElemPlan {
    /// Optional COBOL item name.
    pub name: Option<String>,
    /// Optional vertical `LINE` transition starting at this item.
    pub line: Option<HirReportLineSpec>,
    /// Optional horizontal `COLUMN` placement (one or multiple columns when `COLUMN c1, c2, ...`).
    pub columns: Vec<HirReportColSpec>,
    /// Value source(s) (`1` normally, or `N` matching multi-column `VALUE` / `SOURCE` lists).
    pub sources: Vec<HirReportSourceKind>,
    /// Output formatter.
    pub formatter: HirReportFormatterKind,
    /// `GROUP INDICATE` suppression mode.
    pub group_indicate: HirGroupIndicateMode,
    /// `PRESENT WHEN` / `AFTER NEW` visibility guard.
    pub present_when: HirReportPresentKind,
    /// Optional `OCCURS` repetition clause.
    pub occurs: Option<HirReportOccursSpec>,
    /// Optional writable report-section backing place cleared to `SPACES` after group presentation.
    pub clear_place_after_present: Option<HirPlace>,
}

/// Intermediate hierarchical sub-group in [`HirReportGroupPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirReportSubGroupPlan {
    /// Optional COBOL group name.
    pub name: Option<String>,
    /// Optional vertical `LINE` transition starting at this sub-group.
    pub line: Option<HirReportLineSpec>,
    /// Optional default starting `COLUMN` inherited by first child.
    pub column: Option<HirReportColSpec>,
    /// Visibility guard on the entire sub-group.
    pub present_when: HirReportPresentKind,
    /// Optional `OCCURS` repetition clause.
    pub occurs: Option<HirReportOccursSpec>,
    /// Child nodes.
    pub children: Vec<HirReportNodePlan>,
}

/// Hierarchical node inside a [`HirReportGroupPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirReportNodePlan {
    /// Sub-group node.
    SubGroup(HirReportSubGroupPlan),
    /// Elementary leaf item.
    Elementary(HirReportElemPlan),
}

/// Lowered `01`-level report group plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirReportGroupPlan {
    /// Optional uppercase `01`-level group name.
    pub name: Option<String>,
    /// Group `TYPE IS` role.
    pub role: HirReportGroupRole,
    /// Optional `LINE` clause on `01`.
    pub line: Option<HirReportLineSpec>,
    /// Optional `NEXT GROUP` clause on `01`.
    pub next_group: Option<HirReportNextGroupSpec>,
    /// Visibility guard on `01`.
    pub present_when: HirReportPresentKind,
    /// Optional synthesized `01` Working-Storage `(root_item_id, byte_len)` cleared after presentation when writable items exist.
    pub backing_root_item: Option<(DataItemId, u32)>,
    /// Ordered child nodes.
    pub children: Vec<HirReportNodePlan>,
}

/// `RD` `PAGE LIMIT` parameters in HIR.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HirReportPageLimits {
    /// Optional vertical page line limit (`PAGE LIMIT n LINES`).
    pub line_limit: Option<u32>,
    /// Optional horizontal page column limit (`m COLUMNS`, static or dynamic!).
    pub col_limit: Option<HirExpr>,
    /// Optional `HEADING h`.
    pub heading: Option<u32>,
    /// Optional `FIRST DETAIL fd`.
    pub first_detail: Option<u32>,
    /// Optional `LAST DETAIL ld`.
    pub last_detail: Option<u32>,
    /// Optional `FOOTING ft`.
    pub footing: Option<u32>,
}

/// `RD ... CODE IS ...` prefix specification in HIR.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HirReportCodeSpec {
    /// No `CODE` prefix.
    #[default]
    None,
    /// Static literal prefix bytes.
    Literal(Vec<u8>),
    /// Live Data Division field prefix (`CODE IS MYCODE`).
    Place(HirPlace),
}

/// Complete lowered runtime plan for one `RD` report descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirReportPlan {
    /// Report handle ID (`0..reports.len()`).
    pub report_id: ReportId,
    /// Original COBOL report name (`"RWCS-Report"`, `"rp"`, etc.).
    pub report_name: String,
    /// Associated `FD` [`FileConnectorId`] in `HirProgram::files`.
    pub file_connector: FileConnectorId,
    /// Associated `FD` COBOL file name (`"REPORT-FILE"`).
    pub file_name: String,
    /// Associated `SELECT ... ASSIGN TO` literal or external variable name.
    pub assign_target: String,
    /// Whether `ASSIGN TO EXTERNAL` / env resolution applies (`true` unless quoted literal path).
    pub assign_is_external: bool,
    /// Optional dynamic `ASSIGN TO data-name` storage place.
    pub assign_place: Option<HirPlace>,
    /// Whether `ASSIGN TO ... LINE ADVANCING` / `RECORD DELIMITER` carriage-control applies.
    pub has_line_advancing: bool,
    /// Optional `CODE IS` prefix.
    pub code_spec: HirReportCodeSpec,
    /// `PAGE LIMIT` parameters.
    pub page_limits: HirReportPageLimits,
    /// Ordered `CONTROLS` hierarchy (`0` = major / `FINAL`, `last` = minor).
    pub controls: Vec<HirReportControlSpec>,
    /// Ordered `SUM` counter descriptors.
    pub sum_counters: Vec<HirReportSumSpec>,
    /// Ordered `01` report group descriptors.
    pub groups: Vec<HirReportGroupPlan>,
    /// Bound `USE BEFORE REPORTING` declarative hooks.
    pub hooks: Vec<HirReportHookPlan>,
    /// Synthesized `PAGE-COUNTER` Working-Storage [`DataItemId`].
    pub page_counter_item: DataItemId,
    /// Synthesized `LINE-COUNTER` Working-Storage [`DataItemId`].
    pub line_counter_item: DataItemId,
    /// Synthesized `PRINT-SWITCH` Working-Storage [`DataItemId`].
    pub print_switch_item: DataItemId,
}

/// Lowered Milestone M18 Report Writer statement in [`HirStmt::M18`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirM18Stmt {
    /// `INITIATE report-1 ...`
    Initiate {
        /// Target `(ReportId, original_report_name)` pairs.
        reports: Vec<(ReportId, String)>,
        /// `<file>:<line>` diagnostic label for duplicate-initiate runtime check.
        site_label: String,
        /// Source span.
        span: SourceSpan,
    },
    /// `GENERATE detail-group-name`
    GenerateDetail {
        /// Owning [`ReportId`].
        report_id: ReportId,
        /// Original owning report name for runtime diagnostics.
        report_name: String,
        /// Uppercase `01` `DETAIL` group name.
        group_name: String,
        /// `<file>:<line>` diagnostic label.
        site_label: String,
        /// Source span.
        span: SourceSpan,
    },
    /// `GENERATE report-name` (summary reporting).
    GenerateSummary {
        /// Target [`ReportId`].
        report_id: ReportId,
        /// Original report name for runtime diagnostics.
        report_name: String,
        /// `<file>:<line>` diagnostic label.
        site_label: String,
        /// Source span.
        span: SourceSpan,
    },
    /// `TERMINATE report-1 ...`
    Terminate {
        /// Target `(ReportId, original_report_name)` pairs.
        reports: Vec<(ReportId, String)>,
        /// `<file>:<line>` diagnostic label.
        site_label: String,
        /// Source span.
        span: SourceSpan,
    },
    /// `SUPPRESS [PRINTING]` inside a `USE BEFORE REPORTING` declarative.
    SuppressPrinting {
        /// Source span.
        span: SourceSpan,
    },
}

impl HirM18Stmt {
    /// Returns the source span of this M18 statement.
    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        match self {
            Self::Initiate { span, .. }
            | Self::GenerateDetail { span, .. }
            | Self::GenerateSummary { span, .. }
            | Self::Terminate { span, .. }
            | Self::SuppressPrinting { span } => *span,
        }
    }
}

// ============================================================================
// Milestone M19: SCREEN SECTION & Extended TUI DISPLAY/ACCEPT HIR
// ============================================================================

/// Screen row/column coordinate specification in HIR.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HirScreenCoordSpec {
    /// Omitted (inherits group/cursor coordinate; `Column` defaults to `+ 0` adjacent after prior field).
    #[default]
    Omitted,
    /// Static 1-based coordinate (`LINE 5`, `COL 10`; `0` allowed in ad-hoc `LINE 0`).
    Absolute(u16),
    /// Relative positive offset (`LINE PLUS n` / `COL + n`).
    Plus(u16),
    /// Relative negative offset (`LINE MINUS n` / `COL - n`).
    Minus(u16),
    /// Dynamic runtime coordinate expression (`LINE var`, `COL var + 1`).
    Dynamic(HirExpr),
    /// Dynamic relative positive offset (`LINE + var`).
    DynamicPlus(HirExpr),
    /// Dynamic relative negative offset (`LINE - var`).
    DynamicMinus(HirExpr),
}

/// Screen foreground/background color specification (`0..=7`) in HIR.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HirScreenColorSpec {
    /// Inherited / terminal default color.
    #[default]
    Default,
    /// Static color code (`0..=7`).
    Static(u8),
    /// Dynamic runtime color expression (`FOREGROUND-COLOR fg-var`).
    Dynamic(HirExpr),
}

/// Data binding on an elementary `SCREEN SECTION` item in HIR.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HirScreenBindingPlan {
    /// Pure positioning/blanking/erasing/bell item with no payload.
    #[default]
    None,
    /// Static `VALUE "..."` literal bytes.
    StaticValue(Vec<u8>),
    /// Tiled `VALUE ALL "x"` pattern bytes (`byte_width` display columns).
    FigurativeAll {
        /// Pattern bytes to tile.
        pattern: Vec<u8>,
        /// Total display column width.
        width: u16,
    },
    /// Output-only `FROM expr` binding.
    FromExpr {
        /// Source expression (`DataRef`, literal, or intrinsic).
        src: HirExpr,
        /// Display column width (`>= 1`).
        width: u16,
        /// Screen `PICTURE` [`HirType`].
        screen_ty: HirType,
    },
    /// Input-only `TO place` binding.
    ToPlace {
        /// Destination Working-Storage place.
        dst: HirPlace,
        /// Display column width (`>= 1`).
        width: u16,
        /// Screen `PICTURE` [`HirType`].
        screen_ty: HirType,
    },
    /// Separate `FROM src` + `TO dst` binding.
    FromAndTo {
        /// Output source expression.
        src: HirExpr,
        /// Input destination place.
        dst: HirPlace,
        /// Display column width (`>= 1`).
        width: u16,
        /// Screen `PICTURE` [`HirType`].
        screen_ty: HirType,
    },
    /// Bidirectional `USING target` binding.
    UsingPlace {
        /// Working-Storage place read on `DISPLAY`/`ACCEPT` init and written on `ACCEPT` commit.
        target: HirPlace,
        /// Display column width (`>= 1`).
        width: u16,
        /// Screen `PICTURE` [`HirType`].
        screen_ty: HirType,
    },
}

/// Prompt fill specification on a screen item or ad-hoc `ACCEPT`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HirScreenPromptSpec {
    /// No prompt fill.
    #[default]
    Disabled,
    /// Static prompt fill byte (`'_'` by default).
    StaticByte(u8),
    /// Dynamic single-byte prompt variable (`PROMPT CHARACTER IS prompt-var`).
    Dynamic(HirPlace),
}

/// Elementary `SCREEN SECTION` leaf node in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirScreenFieldPlan {
    /// Optional COBOL screen item name.
    pub name: Option<String>,
    /// Row specification.
    pub line: HirScreenCoordSpec,
    /// Column specification.
    pub col: HirScreenCoordSpec,
    /// Raw `coblang_screen::ScreenAttrFlags` bitmask declared directly on this item.
    pub attr_bits: u32,
    /// Foreground color specification.
    pub fg_color: HirScreenColorSpec,
    /// Background color specification.
    pub bg_color: HirScreenColorSpec,
    /// Prompt character specification.
    pub prompt: HirScreenPromptSpec,
    /// Optional explicit `SIZE expr` override.
    pub size_expr: Option<HirExpr>,
    /// Data binding.
    pub binding: HirScreenBindingPlan,
}

/// Hierarchical `SCREEN SECTION` group node in HIR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirScreenGroupPlan {
    /// Optional COBOL screen group name.
    pub name: Option<String>,
    /// Group starting row specification.
    pub line: HirScreenCoordSpec,
    /// Group starting column specification.
    pub col: HirScreenCoordSpec,
    /// Raw `coblang_screen::ScreenAttrFlags` bitmask inherited by descendants.
    pub attr_bits: u32,
    /// Foreground color inherited by descendants.
    pub fg_color: HirScreenColorSpec,
    /// Background color inherited by descendants.
    pub bg_color: HirScreenColorSpec,
    /// Child nodes.
    pub children: Vec<HirScreenNodePlan>,
}

/// Node in a lowered [`HirScreenPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirScreenNodePlan {
    /// Group screen item (`01`..`48` with children).
    Group(Box<HirScreenGroupPlan>),
    /// Elementary screen item (`01`..`49` leaf).
    Field(Box<HirScreenFieldPlan>),
}

/// Addressable `SCREEN SECTION` target registered in [`crate::HirProgram::screen_plans`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirScreenPlan {
    /// 0-based screen handle ID (`0..screen_plans.len()`).
    pub screen_id: u32,
    /// Uppercase COBOL screen item name.
    pub name: String,
    /// Subtree rooted at this addressable `SCREEN SECTION` item.
    pub root: HirScreenNodePlan,
}

/// Program-level `SPECIAL-NAMES` `CURSOR IS` and `CRT STATUS IS` (plus implicit `COB-CRT-STATUS`) bindings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HirScreenSpecialRegs {
    /// Optional `SPECIAL-NAMES. CURSOR IS ...` place (`4` or `6` byte/digit item).
    pub cursor_place: Option<HirPlace>,
    /// Optional `SPECIAL-NAMES. CRT STATUS IS ...` place (`4`-byte numeric or alphanumeric item).
    pub crt_status_place: Option<HirPlace>,
    /// Implicit `GnuCOBOL` `COB-CRT-STATUS` (`PIC 9(4)`) place when referenced in `PROCEDURE DIVISION`.
    pub cob_crt_status_place: Option<HirPlace>,
}

/// Statement-level positioning and attribute overrides on screen/extended `DISPLAY` and `ACCEPT`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HirScreenOverrides {
    /// Combined 4-digit (`RRCC`) or 6-digit (`RRRCCC`) `AT expr` coordinate.
    pub at_combined: Option<(HirExpr, u8)>,
    /// Explicit `LINE` coordinate (`Absolute`, `Plus`, `Minus`, or `Dynamic`).
    pub line: HirScreenCoordSpec,
    /// Explicit `COLUMN` coordinate (`Absolute`, `Plus`, `Minus`, or `Dynamic`).
    pub col: HirScreenCoordSpec,
    /// Attribute flags (`HIGHLIGHT`, `LOWLIGHT`, `REVERSE-VIDEO`, `BLINK`, `UNDERLINE`, `BLANK SCREEN`, `ERASE`, `AUTO`, `SECURE`, `REQUIRED`, `FULL`, `UPDATE`, `BELL`, etc.).
    pub attr_bits: u32,
    /// Explicit `NO UPDATE` / `DEFAULT` clear flag overriding `UPDATE`.
    pub force_no_update: bool,
    /// Foreground color override.
    pub fg_color: HirScreenColorSpec,
    /// Background color override.
    pub bg_color: HirScreenColorSpec,
    /// Optional `SIZE expr` display/accept width override.
    pub size_expr: Option<HirExpr>,
    /// Prompt character specification.
    pub prompt: HirScreenPromptSpec,
    /// Optional `TIMEOUT` / `TIME-OUT` duration expression on `ACCEPT`.
    pub timeout_expr: Option<HirExpr>,
    /// Milliseconds per unit of `timeout_expr` (`1000` for seconds, `10` for `BEFORE TIME` hundredths).
    pub timeout_scale_ms: u32,
    /// Optional mutable inline `ACCEPT ... CURSOR var` place (1-based field column read & written back).
    pub cursor_offset_place: Option<HirPlace>,
    /// Optional read-only inline `ACCEPT ... CURSOR expr` 1-based field column expression.
    pub cursor_offset_expr: Option<HirExpr>,
    /// Optional `SCROLL UP/DOWN [n LINES]` signed line count (`>0` up, `<0` down).
    pub scroll_lines: Option<HirExpr>,
}

/// Single operand in a TUI `DISPLAY` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirScreenDisplayItem {
    /// `DISPLAY screen-name [AT ...]`
    NamedScreen {
        /// Target `screen_id` in `HirProgram::screen_plans`.
        screen_id: u32,
        /// Optional `AT` / `LINE` / `COLUMN` / `WITH` overrides.
        overrides: HirScreenOverrides,
    },
    /// Extended `DISPLAY expr AT ... WITH ...` (or `DISPLAY ERASE` / `DISPLAY BLANK SCREEN` when `expr` is `None`).
    ExtendedField {
        /// Optional value expression (`None` for bare `DISPLAY BLANK SCREEN` / `DISPLAY ERASE`).
        expr: Option<HirExpr>,
        /// Positioning, color, erase, and attribute overrides.
        overrides: HirScreenOverrides,
    },
}

/// Lowered Milestone M19 TUI statement in [`HirStmt::M19`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirM19Stmt {
    /// Screen or extended CRT `DISPLAY`.
    DisplayScreen {
        /// Ordered screen/field items to render onto the TUI grid.
        items: Vec<HirScreenDisplayItem>,
        /// `ON EXCEPTION` handler statements.
        on_exception: Vec<HirStmt>,
        /// `NOT ON EXCEPTION` handler statements.
        not_on_exception: Vec<HirStmt>,
        /// Source span.
        span: SourceSpan,
    },
    /// Screen or extended CRT `ACCEPT`.
    AcceptScreen {
        /// Target `SCREEN SECTION` `screen_id` (`None` for ad-hoc field `ACCEPT`).
        screen_id: Option<u32>,
        /// Target Working-Storage `HirPlace` for ad-hoc `ACCEPT field AT ... WITH ...` (`None` for `ACCEPT OMITTED` or `ACCEPT screen-name`).
        adhoc_place: Option<HirPlace>,
        /// Positioning, attribute, prompt, size, and timeout overrides.
        overrides: Box<HirScreenOverrides>,
        /// `ON EXCEPTION` / `ON ESCAPE` handler statements.
        on_exception: Vec<HirStmt>,
        /// `NOT ON EXCEPTION` / `NOT ON ESCAPE` handler statements.
        not_on_exception: Vec<HirStmt>,
        /// Source span.
        span: SourceSpan,
    },
    /// `SET screen-name ATTRIBUTE attr ON/OFF ...`
    SetScreenAttr {
        /// Target `screen_id`.
        screen_id: u32,
        /// `ScreenAttrFlags` bits to turn `ON`.
        on_bits: u32,
        /// `ScreenAttrFlags` bits to turn `OFF`.
        off_bits: u32,
        /// Source span.
        span: SourceSpan,
    },
}

impl HirM19Stmt {
    /// Returns the source span of this M19 statement.
    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        match self {
            Self::DisplayScreen { span, .. }
            | Self::AcceptScreen { span, .. }
            | Self::SetScreenAttr { span, .. } => *span,
        }
    }
}
