//! Strongly-typed AST nodes for Milestone M18 (`REPORT SECTION`) and Milestone M19 (`SCREEN SECTION` & TUI).

#![deny(clippy::indexing_slicing)]

use coblang_common::{LevelNumber, SourceSpan, SymbolId};

use super::data_div::{
    BlankWhenZeroMode, DataEntryName, GlobalVisibility, JustificationMode, OccursClause,
    SignClause, UsageKind, ValueAtom,
};
use super::proc_div::{Condition, DataRef, DisplayAdvancing, EventHandlers, Expr};

/// Implicit `SPECIAL-NAMES` binding attached directly to a Data Division item (`IS SPECIAL-NAMES ...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpecialNamesDataRole {
    /// `IS SPECIAL-NAMES CRT STATUS`.
    CrtStatus,
    /// `IS SPECIAL-NAMES CURSOR`.
    Cursor,
}

/// Classification of a `TYPE IS ...` report group inside `REPORT SECTION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportGroupType {
    /// `REPORT HEADING` (`RH`).
    ReportHeading,
    /// `PAGE HEADING` (`PH`).
    PageHeading,
    /// `CONTROL HEADING` (`CH`).
    ControlHeading,
    /// `DETAIL` (`DE`).
    Detail,
    /// `CONTROL FOOTING` (`CF`).
    ControlFooting,
    /// `PAGE FOOTING` (`PF`).
    PageFooting,
    /// `REPORT FOOTING` (`RF`).
    ReportFooting,
}

/// Control hierarchy entry (`FINAL` or qualified data-name) in `CONTROLS`, `TYPE CH/CF`, or `RESET ON`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportControlAst {
    /// Top-level `FINAL` control break.
    Final(SourceSpan),
    /// Qualified Data Division control field path.
    Field(Vec<SymbolId>, SourceSpan),
}

/// `CODE IS` prefix specification on an `RD` report entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RdCodeClauseAst {
    /// String literal prefix (`CODE IS "Hi-Q: "`).
    Literal(SymbolId, SourceSpan),
    /// Qualified alphanumeric Data Division item (`CODE IS MYCODE`).
    DataName(Vec<SymbolId>, SourceSpan),
}

/// Horizontal page column limit (`PAGE LIMIT ... n COLUMNS` or `LINE LIMIT n`) on an `RD` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RdColLimitAst {
    /// Compile-time column limit literal.
    Literal(u32, SourceSpan),
    /// Dynamic Data Division column limit variable (`MAXCOL COLUMNS`).
    DataName(Vec<SymbolId>, SourceSpan),
}

/// Vertical `LINE NUMBER IS ...` positioning clause on a report group or elementary entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportLineClauseAst {
    /// `LINE [NUMBER IS] n` — 1-based absolute page line.
    Absolute {
        /// 1-based page line number.
        line: u32,
        /// Clause span.
        span: SourceSpan,
    },
    /// `LINE [NUMBER IS] {PLUS | +} n` — relative downward line offset.
    Relative {
        /// Relative line advance (`>= 0`).
        offset: u32,
        /// Clause span.
        span: SourceSpan,
    },
    /// `LINE [NUMBER IS] [n] [ON] NEXT PAGE` — advance to next page at line `n` (default `1`).
    OnNextPage {
        /// 1-based line on the new page.
        line: u32,
        /// Clause span.
        span: SourceSpan,
    },
}

impl ReportLineClauseAst {
    /// Returns the source span of this `LINE` clause.
    #[must_use]
    pub const fn span(self) -> SourceSpan {
        match self {
            Self::Absolute { span, .. }
            | Self::Relative { span, .. }
            | Self::OnNextPage { span, .. } => span,
        }
    }
}

/// `NEXT GROUP IS ...` spacing directive on a `01` report group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportNextGroupAst {
    /// `NEXT GROUP IS n` — absolute 1-based line number.
    Absolute {
        /// 1-based target line.
        line: u32,
        /// Clause span.
        span: SourceSpan,
    },
    /// `NEXT GROUP IS {PLUS | +} n` — relative line advance.
    Relative {
        /// Relative line offset.
        offset: u32,
        /// Clause span.
        span: SourceSpan,
    },
    /// `NEXT GROUP IS NEXT PAGE` — defer page advance before next body group.
    NextPage {
        /// Clause span.
        span: SourceSpan,
    },
}

/// Horizontal column alignment modifier (`COLUMN [LEFT | CENTER | RIGHT] ...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ReportColAlignAst {
    /// Standard left-anchored column (`COLUMN n` or `COLUMN LEFT n`).
    #[default]
    LeftDefault,
    /// Explicit `COLUMN LEFT n` keyword specified.
    ExplicitLeft,
    /// Centered around `COLUMN CENTER n`.
    Center,
    /// Right-anchored ending at `COLUMN RIGHT n`.
    Right,
}

/// Single column coordinate inside a `COLUMN` clause (`COLUMN 1` or `COLUMN 1, 11, 21`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportColCoordAst {
    /// 1-based absolute column position.
    Absolute {
        /// 1-based column number.
        col: u32,
        /// Alignment mode.
        align: ReportColAlignAst,
        /// Coordinate span.
        span: SourceSpan,
    },
    /// Relative `PLUS` / `+` column offset from previous printable end column.
    Relative {
        /// Relative column advance.
        offset: u32,
        /// Alignment mode (used to diagnose `LEFT/CENTER/RIGHT` + `PLUS`).
        align: ReportColAlignAst,
        /// Coordinate span.
        span: SourceSpan,
    },
}

/// `SUM operand ... [UPON detail ...] [RESET ON {FINAL | control}]` clause on a report item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportSumClauseAst {
    /// Summand expressions (Data Division fields, literals, arithmetic expressions, or subordinate `SUM` counter names).
    pub operands: Vec<Expr>,
    /// Optional `UPON detail-group ...` filter list.
    pub upon_details: Vec<(SymbolId, SourceSpan)>,
    /// Optional explicit `RESET ON` control level override.
    pub reset_on: Option<ReportControlAst>,
    /// Clause span.
    pub span: SourceSpan,
}

/// Target of `[PRESENT | ABSENT] AFTER [NEW] {PAGE | FINAL | control} [OR ...]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportAfterNewAtomAst {
    /// `PAGE` boundary trigger.
    Page(SourceSpan),
    /// `FINAL` control break trigger.
    Final(SourceSpan),
    /// Data Division control field break trigger.
    Control(Vec<SymbolId>, SourceSpan),
}

/// Polarity of `PRESENT` vs `ABSENT` / `NOT PRESENT` conditional presentation clauses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportPresentPolarity {
    /// Present item/group when trigger holds (`PRESENT`).
    Present,
    /// Suppress item/group when trigger holds (`ABSENT` / `NOT PRESENT`).
    Absent,
}

/// Conditional presentation gate (`PRESENT WHEN`, `ABSENT WHEN`, `PRESENT AFTER [NEW] ...`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportPresentGateAst {
    /// `[NOT] PRESENT WHEN condition` or `ABSENT WHEN condition`.
    WhenCondition {
        /// `Present` vs `Absent`.
        polarity: ReportPresentPolarity,
        /// Guard predicate evaluated at presentation time.
        condition: Box<Condition>,
        /// Clause span.
        span: SourceSpan,
    },
    /// `[NOT] PRESENT AFTER [NEW] atom [OR atom ...]` or `ABSENT AFTER [NEW] ...`.
    AfterNew {
        /// `Present` vs `Absent`.
        polarity: ReportPresentPolarity,
        /// Disjunctive list of `PAGE` / control-break triggers.
        triggers: Vec<ReportAfterNewAtomAst>,
        /// Clause span.
        span: SourceSpan,
    },
}

/// `OCCURS` extension fields specific to COBOL 2002 / `GnuCOBOL` `REPORT SECTION` entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportOccursExtAst {
    /// Base `OCCURS [min TO] max [DEPENDING ON ...]` specification.
    pub base: OccursClause,
    /// Optional horizontal `STEP n` column stride per occurrence.
    pub step: Option<u32>,
    /// Optional `VARYING idx [FROM start] [BY step]` loop variable `(sym, start, by, span)`.
    pub varying: Option<(SymbolId, i64, i64, SourceSpan)>,
}

/// Report group or elementary entry (`01`..`49`) inside an `RD` report description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportGroupEntry {
    /// Entry level number (`01`..`49`).
    pub level: LevelNumber,
    /// Optional group/field name.
    pub name: DataEntryName,
    /// Optional `TYPE IS ...` report group classification.
    pub group_type: Option<ReportGroupType>,
    /// Optional source span of the `TYPE IS ...` clause.
    pub group_type_span: Option<SourceSpan>,
    /// Optional control target on `TYPE IS CONTROL HEADING / FOOTING <control>`.
    pub type_control: Option<ReportControlAst>,
    /// `LINE NUMBER IS ...` clauses on this entry (multiple tracked for duplicate warnings).
    pub line_clauses: Vec<ReportLineClauseAst>,
    /// Optional `NEXT GROUP IS ...` clause.
    pub next_group: Option<ReportNextGroupAst>,
    /// `COLUMN` coordinate list (`1` for standard, `>1` for multi-column `COLUMN 1, 11, 21`).
    pub columns: Vec<ReportColCoordAst>,
    /// Optional `PICTURE` string + span.
    pub picture: Option<(SymbolId, SourceSpan)>,
    /// Optional explicit `USAGE` clause.
    pub usage: Option<UsageKind>,
    /// Optional `SIGN IS ...` clause.
    pub sign: Option<SignClause>,
    /// `JUSTIFIED` mode.
    pub justified: JustificationMode,
    /// `BLANK WHEN ZERO` mode.
    pub blank_when_zero: BlankWhenZeroMode,
    /// `GROUP INDICATE` presence (`Some(span)` if specified).
    pub group_indicate: Option<SourceSpan>,
    /// `SOURCE IS expr` clauses (tracked as `Vec` to detect duplicate/conflicting clauses).
    pub sources: Vec<(Expr, SourceSpan)>,
    /// `SUM ...` clauses (multiple `SUM` clauses on one counter are valid in ANSI-85/ISO!).
    pub sums: Vec<ReportSumClauseAst>,
    /// `VALUE IS ...` literal list (supports multi-value `VALUE "A" "B"`).
    pub values: Vec<(ValueAtom, SourceSpan)>,
    /// Optional `PRESENT WHEN` / `ABSENT WHEN` / `PRESENT AFTER` gate.
    pub present_gate: Option<ReportPresentGateAst>,
    /// Optional `OCCURS ... [STEP n] [VARYING ...]` repetition clause.
    pub occurs: Option<ReportOccursExtAst>,
    /// Entry source span.
    pub span: SourceSpan,
}

/// `RD report-name` Report Description entry in `REPORT SECTION`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdEntry {
    /// Report identifier.
    pub report_name: SymbolId,
    /// `IS GLOBAL` visibility.
    pub global: GlobalVisibility,
    /// Optional `CODE IS` line prefix literal or variable.
    pub code_clause: Option<RdCodeClauseAst>,
    /// Control break hierarchy (`FINAL` and qualified field paths) with spans.
    pub control_items: Vec<ReportControlAst>,
    /// Legacy control symbol paths preserved for convenience.
    pub controls: Vec<Vec<SymbolId>>,
    /// Optional `PAGE [LIMIT IS] n [LINES]` maximum.
    pub page_limit: Option<u32>,
    /// Optional source span of `PAGE LIMIT`.
    pub page_limit_span: Option<SourceSpan>,
    /// Optional `PAGE LIMIT ... c COLUMNS` / `LINE LIMIT c` horizontal width.
    pub col_limit: Option<RdColLimitAst>,
    /// Optional `HEADING` line number.
    pub heading: Option<u32>,
    /// Optional source span of `HEADING`.
    pub heading_span: Option<SourceSpan>,
    /// Optional `FIRST DETAIL` line number.
    pub first_detail: Option<u32>,
    /// Optional source span of `FIRST DETAIL`.
    pub first_detail_span: Option<SourceSpan>,
    /// Optional `LAST DETAIL` line number.
    pub last_detail: Option<u32>,
    /// Optional source span of `LAST DETAIL`.
    pub last_detail_span: Option<SourceSpan>,
    /// Optional `FOOTING` line number.
    pub footing: Option<u32>,
    /// Optional source span of `FOOTING`.
    pub footing_span: Option<SourceSpan>,
    /// Hierarchical report group/elementary entries (`01`..`49`).
    pub groups: Vec<ReportGroupEntry>,
    /// Source span of the `RD` header.
    pub span: SourceSpan,
}

// ============================================================================
// Milestone M19: `SCREEN SECTION` & Extended `DISPLAY` / `ACCEPT` / `SET` AST
// ============================================================================

/// Signed direction of a `LINE` or `COLUMN` screen coordinate (`Absolute`, `Plus`, `Minus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenCoordDirAst {
    /// Absolute coordinate (`LINE expr` / `COL expr`).
    Absolute,
    /// Forward relative offset (`LINE + expr` / `LINE PLUS expr`).
    Plus,
    /// Backward relative offset (`LINE - expr` / `LINE MINUS expr`).
    Minus,
}

/// Parsed `LINE` or `COLUMN` coordinate on a `SCREEN SECTION` or extended `DISPLAY`/`ACCEPT` item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenCoordAst {
    /// Coordinate direction (`Absolute`, `Plus`, or `Minus`).
    pub dir: ScreenCoordDirAst,
    /// Coordinate value expression (`None` when bare `LINE` / `COLUMN` defaults to `1`).
    pub expr: Option<Expr>,
    /// Whether an invalid inline signed literal (`+1` / `-1`) or non-integer decimal literal (`1.0`) was used.
    pub malformed_literal: Option<SourceSpan>,
    /// Clause source span.
    pub span: SourceSpan,
}

/// Tag identifying each distinct `SCREEN` / extended I/O clause for duplicate & conflict checking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenClauseAst {
    /// `LINE [NUMBER IS] [PLUS|MINUS|+|-] expr`.
    Line(ScreenCoordAst),
    /// `COLUMN`/`COL` `[NUMBER IS] [PLUS|MINUS|+|-] expr`.
    Column(ScreenCoordAst),
    /// Compact `AT expr` (`LLCC` / `LLLCCC`).
    At {
        /// Compact coordinate expression.
        expr: Expr,
        /// Span immediately following the `AT` operand (matches LR(1) diagnostics).
        post_span: SourceSpan,
        /// Clause span.
        span: SourceSpan,
    },
    /// MS-COBOL `( [line_expr] , [col_expr] )` coordinate pair (`-faccept-display-extensions`).
    MsPosition {
        /// Optional row expression.
        line: Option<Expr>,
        /// Optional column expression.
        col: Option<Expr>,
        /// Clause span.
        span: SourceSpan,
    },
    /// `PIC` / `PICTURE` clause.
    Picture {
        /// Raw picture string symbol.
        raw: SymbolId,
        /// Clause span.
        span: SourceSpan,
    },
    /// `VALUE [IS] atom` (or implicit string literal in `SCREEN SECTION`).
    Value {
        /// Literal or figurative value.
        atom: ValueAtom,
        /// Whether `VALUE` keyword was omitted (`05 "text"`).
        implicit_keyword: bool,
        /// Clause span.
        span: SourceSpan,
    },
    /// `FROM expr` output source binding.
    From {
        /// Source expression or identifier.
        expr: Expr,
        /// Clause span.
        span: SourceSpan,
    },
    /// `TO data-ref` input destination binding.
    To {
        /// Destination field.
        target: DataRef,
        /// Clause span.
        span: SourceSpan,
    },
    /// `USING data-ref` bidirectional binding.
    Using {
        /// Bound Working/Local/Linkage field.
        target: DataRef,
        /// Clause span.
        span: SourceSpan,
    },
    /// `USAGE [IS] usage`.
    Usage {
        /// Parsed usage kind.
        usage: UsageKind,
        /// Clause span.
        span: SourceSpan,
    },
    /// `SIGN [IS] {LEADING|TRAILING} [SEPARATE]`.
    Sign {
        /// Parsed sign specification.
        sign: SignClause,
        /// Clause span.
        span: SourceSpan,
    },
    /// `BLANK [WHEN] ZERO`.
    BlankWhenZero(SourceSpan),
    /// `JUSTIFIED` / `JUST` `[RIGHT]`.
    Justified(SourceSpan),
    /// `OCCURS n [TIMES]`.
    Occurs(OccursClause),
    /// `BLANK SCREEN`.
    BlankScreen(SourceSpan),
    /// `BLANK LINE`.
    BlankLine(SourceSpan),
    /// `ERASE [TO] EOL` / `ERASE LINE`.
    EraseEol(SourceSpan),
    /// `ERASE [TO] EOS` / `ERASE SCREEN` / bare `DISPLAY ... ERASE`.
    EraseEos(SourceSpan),
    /// `BELL` / `BEEP`.
    Bell(SourceSpan),
    /// `HIGHLIGHT`.
    Highlight(SourceSpan),
    /// `LOWLIGHT`.
    Lowlight(SourceSpan),
    /// `BLINK`.
    Blink(SourceSpan),
    /// `REVERSE-VIDEO` / `REVERSED`.
    ReverseVideo(SourceSpan),
    /// `UNDERLINE`.
    Underline(SourceSpan),
    /// `OVERLINE`.
    Overline(SourceSpan),
    /// `LEFTLINE`.
    Leftline(SourceSpan),
    /// `RIGHTLINE`.
    Rightline(SourceSpan),
    /// `GRID` (parsed with warning).
    Grid(SourceSpan),
    /// `AUTO` / `AUTO-SKIP` / `AUTOTERMINATE`.
    Auto(SourceSpan),
    /// `TAB` (conflicts with `AUTO`).
    Tab(SourceSpan),
    /// `SECURE` / `NO-ECHO`.
    Secure(SourceSpan),
    /// `REQUIRED` / `EMPTY-CHECK`.
    Required(SourceSpan),
    /// `FULL` / `LENGTH-CHECK`.
    Full(SourceSpan),
    /// `PROMPT [CHARACTER IS expr]`.
    Prompt {
        /// Optional single-character prompt expression.
        char_expr: Option<Expr>,
        /// Clause span.
        span: SourceSpan,
    },
    /// `UPDATE` / `DEFAULT` (`enabled = true`) or `NO UPDATE` (`enabled = false`).
    Update {
        /// Whether pre-fill update mode is enabled.
        enabled: bool,
        /// Clause span.
        span: SourceSpan,
    },
    /// `ZERO-FILL` (`NumericZeroFill` extension).
    ZeroFill(SourceSpan),
    /// `INITIAL` (treat field as initial focus or preserve on re-accept).
    Initial(SourceSpan),
    /// `SIZE [IS] expr` display/accept width override.
    Size {
        /// Width expression.
        expr: Expr,
        /// Clause span.
        span: SourceSpan,
    },
    /// `FOREGROUND-COLOR` / `FOREGROUND-COLOUR` `[IS] expr`.
    ForegroundColor {
        /// Color number (`0..=15`) expression.
        expr: Expr,
        /// Clause span.
        span: SourceSpan,
    },
    /// `BACKGROUND-COLOR` / `BACKGROUND-COLOUR` `[IS] expr`.
    BackgroundColor {
        /// Color number (`0..=15`) expression.
        expr: Expr,
        /// Clause span.
        span: SourceSpan,
    },
    /// `CONTROL [IS] expr` dynamic attribute string.
    Control {
        /// Control string expression.
        expr: Expr,
        /// Clause span.
        span: SourceSpan,
    },
    /// `COLOR [IS] expr` combined color attribute.
    Color {
        /// Combined color expression.
        expr: Expr,
        /// Clause span.
        span: SourceSpan,
    },
    /// `MODE IS BLOCK` clause on screen `DISPLAY` / `ACCEPT`.
    ModeIsBlock(SourceSpan),
    /// `WITH CONVERSION` clause on `ACCEPT` (ACUCOBOL compatibility).
    Conversion(SourceSpan),
    /// `SCROLL {UP | DOWN} [BY expr {LINE | LINES}]`.
    Scroll {
        /// `true` for `DOWN`, `false` for `UP`.
        down: bool,
        /// Optional line count (`None` = 1 line).
        lines: Option<Expr>,
        /// Clause span.
        span: SourceSpan,
    },
    /// `TIME-OUT` / `TIMEOUT` `[AFTER] expr` or `BEFORE TIME expr`.
    Timeout {
        /// Timeout duration expression.
        expr: Expr,
        /// Scale factor (`1000` for seconds, `10` for hundredths when `BEFORE TIME`).
        unit_ms: u32,
        /// Clause span.
        span: SourceSpan,
    },
    /// Inline `CURSOR [IS] expr` 1-based field offset on `ACCEPT`.
    CursorOffset {
        /// Mutable field cursor variable or literal expression.
        expr: Expr,
        /// Clause span.
        span: SourceSpan,
    },
}

impl ScreenClauseAst {
    /// Returns the source span of this screen clause.
    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        match self {
            Self::Line(c) | Self::Column(c) => c.span,
            Self::At { span, .. }
            | Self::MsPosition { span, .. }
            | Self::Picture { span, .. }
            | Self::Value { span, .. }
            | Self::From { span, .. }
            | Self::To { span, .. }
            | Self::Using { span, .. }
            | Self::Usage { span, .. }
            | Self::Sign { span, .. }
            | Self::Prompt { span, .. }
            | Self::Update { span, .. }
            | Self::Size { span, .. }
            | Self::ForegroundColor { span, .. }
            | Self::BackgroundColor { span, .. }
            | Self::Control { span, .. }
            | Self::Color { span, .. }
            | Self::Scroll { span, .. }
            | Self::Timeout { span, .. }
            | Self::CursorOffset { span, .. } => *span,
            Self::Occurs(o) => o.span,
            Self::BlankWhenZero(s)
            | Self::Justified(s)
            | Self::BlankScreen(s)
            | Self::BlankLine(s)
            | Self::EraseEol(s)
            | Self::EraseEos(s)
            | Self::Bell(s)
            | Self::Highlight(s)
            | Self::Lowlight(s)
            | Self::Blink(s)
            | Self::ReverseVideo(s)
            | Self::Underline(s)
            | Self::Overline(s)
            | Self::Leftline(s)
            | Self::Rightline(s)
            | Self::Grid(s)
            | Self::Auto(s)
            | Self::Tab(s)
            | Self::Secure(s)
            | Self::Required(s)
            | Self::Full(s)
            | Self::ZeroFill(s)
            | Self::Initial(s)
            | Self::ModeIsBlock(s)
            | Self::Conversion(s) => *s,
        }
    }
}

/// `SCREEN SECTION` hierarchy entry (`01`..`49`, plus invalid `88` captured for semantic rejection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenEntry {
    /// Entry level number.
    pub level: LevelNumber,
    /// Optional screen group/field name.
    pub name: DataEntryName,
    /// All clauses in source order (enabling exact duplicate, conflict, and vendor-rule checks).
    pub clauses: Vec<ScreenClauseAst>,
    /// Source span of the entire screen entry.
    pub span: SourceSpan,
}

/// Single operand together with its per-operand screen clauses inside an extended `DISPLAY`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenDisplayItemAst {
    /// Displayed operand (`None` for bare `DISPLAY ERASE` / `DISPLAY (1, 1) ERASE`).
    pub operand: Option<Expr>,
    /// Screen positioning/attribute clauses attached to this operand.
    pub clauses: Vec<ScreenClauseAst>,
    /// Optional per-operand `UPON device` override (`DISPLAY "a" UPON CRT, "c" UPON CRT-UNDER`).
    pub upon: Option<(SymbolId, SourceSpan)>,
    /// Item source span.
    pub span: SourceSpan,
}

/// Extended `DISPLAY` statement carrying screen positioning, attributes, or `UPON CRT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenDisplayStmtAst {
    /// Ordered display items.
    pub items: Vec<ScreenDisplayItemAst>,
    /// Statement-level `UPON` device if specified at the end.
    pub trailing_upon: Option<(SymbolId, SourceSpan)>,
    /// `WITH NO ADVANCING` mode and post-clause span.
    pub advancing: DisplayAdvancing,
    /// Post-`NO ADVANCING` span used for exact diagnostic line reporting.
    pub no_advancing_span: Option<SourceSpan>,
    /// Optional `ON EXCEPTION` / `NOT ON EXCEPTION` handlers.
    pub handlers: EventHandlers,
    /// Statement span.
    pub span: SourceSpan,
}

/// Target of an extended screen `ACCEPT` (`ACCEPT field ...`, `ACCEPT screen-name ...`, or `ACCEPT OMITTED ...`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenAcceptTargetAst {
    /// `ACCEPT data-or-screen-ref ...`.
    Item(DataRef),
    /// `ACCEPT OMITTED ...` single-key wait without destination storage.
    Omitted(SourceSpan),
}

/// Extended `ACCEPT` statement carrying screen positioning, attributes, `OMITTED`, or `FROM CRT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenAcceptStmtAst {
    /// Target field, screen item, or `OMITTED`.
    pub target: ScreenAcceptTargetAst,
    /// Screen positioning, attribute, timeout, and cursor clauses in source order.
    pub clauses: Vec<ScreenClauseAst>,
    /// Whether `FROM CRT` was explicitly specified.
    pub from_crt: bool,
    /// `ON EXCEPTION` / `ON ESCAPE` and `NOT ON EXCEPTION` / `NOT ON ESCAPE` handlers.
    pub handlers: EventHandlers,
    /// Statement span.
    pub span: SourceSpan,
}

/// Single attribute toggle inside `SET <screen-item> ATTRIBUTE <attr> {ON | OFF}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SetScreenAttrToggleAst {
    /// Attribute symbol (`BELL`, `BLINK`, `HIGHLIGHT`, `LOWLIGHT`, `REVERSE-VIDEO`, `UNDERLINE`, etc.).
    pub attr_name: SymbolId,
    /// `true` when turned `ON`, `false` when turned `OFF`.
    pub enabled: bool,
    /// Toggle span.
    pub span: SourceSpan,
}
