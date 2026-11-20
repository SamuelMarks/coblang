//! ISO COBOL Compiler Directing Facility (`>>DEFINE`, `>>IF`, `>>EVALUATE`, `>>TURN`, `*CBL`).

use std::collections::BTreeMap;

use coblang_common::SourceSpan;
use coblang_diag::{Diagnostic, DiagnosticCode, DiagnosticSink, DpcInDataMode};

use crate::format::NormalizedLine;

/// Double-quote character (`0x22`).
const DQ: char = '"';
/// Single-quote character (`0x27`).
const SQ: char = '\u{0027}';

/// Compile-time value bound to a `>>DEFINE` compilation variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdfValue {
    /// Explicitly undefined (`>>DEFINE name AS OFF`).
    Unset,
    /// Defined without an explicit value (`-DFOO` or `>>DEFINE FOO AS PARAMETER`).
    DefinedUnit,
    /// Signed integer constant (`>>DEFINE VER AS 2023`).
    Integer(i128),
    /// String literal constant (`>>DEFINE ENV AS "PROD"`).
    Text(String),
}

/// Exception-checking toggle recorded from `>>TURN ec-name CHECKING ON/OFF`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TurnCheckingState {
    /// Runtime checking disabled (`CHECKING OFF`).
    Disabled,
    /// Runtime checking enabled without location metadata (`CHECKING ON`).
    Enabled,
    /// Runtime checking enabled with source location (`CHECKING ON WITH LOCATION`).
    EnabledWithLocation,
}

/// Scoped `>>TURN` exception directive captured during preprocessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnDirectiveRecord {
    /// Canonical uppercase `EC-*` exception name(s).
    pub exception_names: Vec<String>,
    /// Requested checking state.
    pub state: TurnCheckingState,
    /// Directive span where `>>TURN` appeared.
    pub span: SourceSpan,
}

/// Scoped `$SET DPC-IN-DATA` directive captured during preprocessing (`Milestone M20`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpcInDataRecord {
    /// Requested `DPC-IN-DATA` mode (`None`, `Xml`, `Json`, or `All`).
    pub mode: DpcInDataMode,
    /// Directive span where `$SET DPC-IN-DATA` appeared.
    pub span: SourceSpan,
}

/// Mutable state for ISO CDF variables, `>>PUSH`/`>>POP` stacks, and `>>TURN` records.
#[derive(Debug, Clone, Default)]
pub struct CdfEnv {
    /// Active compilation variable table keyed by uppercase identifier.
    vars: BTreeMap<String, CdfValue>,
    /// Saved variable snapshots pushed by `>>PUSH`.
    saved_stack: Vec<BTreeMap<String, CdfValue>>,
    /// Ordered log of `>>TURN` directives encountered in active compilation branches.
    pub turn_records: Vec<TurnDirectiveRecord>,
    /// Ordered log of `$SET DPC-IN-DATA` directives encountered in active compilation branches.
    pub dpc_in_data_records: Vec<DpcInDataRecord>,
    /// Compile-time messages emitted by `>>DISPLAY`.
    pub display_messages: Vec<String>,
}

impl CdfEnv {
    /// Creates an empty [`CdfEnv`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Initializes a [`CdfEnv`] with pre-populated CLI `-D` definitions.
    ///
    /// # Arguments
    ///
    /// * `initial` - Slice of `(name, CdfValue)` pairs.
    #[must_use]
    pub fn with_defines(initial: &[(String, CdfValue)]) -> Self {
        let mut env = Self::new();
        for (k, v) in initial {
            env.vars.insert(k.to_ascii_uppercase(), v.clone());
        }
        env
    }

    /// Queries the current [`CdfValue`] of `name` (case-insensitive).
    ///
    /// # Arguments
    ///
    /// * `name` - Variable identifier to inspect.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&CdfValue> {
        self.vars
            .get(&name.to_ascii_uppercase())
            .filter(|v| **v != CdfValue::Unset)
    }

    /// Sets or updates `name` in the active compilation variable table.
    ///
    /// # Arguments
    ///
    /// * `name` - Variable identifier.
    /// * `val` - New [`CdfValue`].
    pub fn set(&mut self, name: &str, val: CdfValue) {
        self.vars.insert(name.to_ascii_uppercase(), val);
    }
}

/// Conditional frame on the `>>IF` / `>>EVALUATE` nesting stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CondFrame {
    /// `>>IF` block (`parent_active`, `branch_taken`, `currently_active`).
    If {
        /// Whether enclosing outer blocks are active.
        parent_active: BranchActivity,
        /// Whether a prior `>>IF` / `>>ELIF` arm already matched.
        matched_any: BranchMatchState,
        /// Whether lines in the current arm should be emitted.
        current_active: BranchActivity,
    },
    /// `>>EVALUATE` block (`parent_active`, `subject_int_or_true`, `matched_any`, `current_active`).
    Evaluate {
        /// Whether enclosing outer blocks are active.
        parent_active: BranchActivity,
        /// Evaluated integer/boolean subject (`1` for `TRUE`).
        subject: i128,
        /// Whether a prior `>>WHEN` arm already matched.
        matched_any: BranchMatchState,
        /// Whether lines in the current `>>WHEN` arm should be emitted.
        current_active: BranchActivity,
    },
}

/// Strongly-typed conditional branch emission state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchActivity {
    /// Emit lines inside this branch.
    Active,
    /// Elide lines inside this branch.
    Inactive,
}

/// Strongly-typed latch recording whether any arm of `>>IF` / `>>EVALUATE` has matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BranchMatchState {
    /// No arm has matched yet.
    Unmatched,
    /// An arm has already matched.
    Matched,
}

/// Evaluates CDF directives (`>>...`, `*CONTROL`, `*CBL`) and filters inactive conditional lines.
///
/// # Arguments
///
/// * `lines` - Normalized source lines.
/// * `env` - Mutable [`CdfEnv`].
/// * `sink` - [`DiagnosticSink`] receiving malformed directive diagnostics.
#[must_use]
pub fn process_cdf_lines(
    lines: &[NormalizedLine],
    env: &mut CdfEnv,
    sink: &mut DiagnosticSink,
) -> Vec<NormalizedLine> {
    let mut stack: Vec<CondFrame> = Vec::new();
    let mut out = Vec::with_capacity(lines.len());

    for line in lines {
        let trimmed = line.code.trim();
        let active = current_branch_active(&stack);

        if line.indicator == crate::format::LineIndicator::VendorDirectiveDollar {
            if active == BranchActivity::Active {
                handle_vendor_dollar_directive(trimmed, line.span, env);
            }
            continue;
        }
        if let Some(directive_body) = trimmed.strip_prefix(">>") {
            handle_chevron_directive(
                directive_body.trim(),
                line.span,
                active,
                &mut stack,
                env,
                sink,
            );
            continue;
        }
        if active == BranchActivity::Active && is_cbl_control_directive(trimmed) {
            continue;
        }
        if active == BranchActivity::Active {
            out.push(line.clone());
        }
    }

    if let Some(last_line) = lines.last()
        && !stack.is_empty()
    {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0001Preprocessor,
            Some(last_line.span),
            String::from("unterminated `>>IF` or `>>EVALUATE` conditional compilation block"),
        ));
    }
    out
}

/// Returns whether the innermost conditional compilation branch is active.
///
/// # Arguments
///
/// * `stack` - Active conditional frame stack.
#[must_use]
fn current_branch_active(stack: &[CondFrame]) -> BranchActivity {
    match stack.last().copied() {
        None => BranchActivity::Active,
        Some(CondFrame::If { current_active, .. } | CondFrame::Evaluate { current_active, .. }) => {
            current_active
        }
    }
}

/// Checks for IBM `*CONTROL` or `*CBL` compiler-option lines in code flow.
///
/// # Arguments
///
/// * `trimmed` - Trimmed code line.
#[must_use]
fn is_cbl_control_directive(trimmed: &str) -> bool {
    let upper = trimmed.to_ascii_uppercase();
    upper.starts_with("*CONTROL ")
        || upper == "*CONTROL"
        || upper.starts_with("*CBL ")
        || upper == "*CBL"
}

/// Processes Micro Focus / IBM `$SET ...` directives (`BOUND`, `NOBOUND`, `NO-BOUND`, `SSRANGE`, `NOSSRANGE`).
///
/// # Arguments
///
/// * `trimmed` - Trimmed directive text following `$`.
/// * `span` - Directive [`SourceSpan`].
/// * `env` - Mutable [`CdfEnv`] receiving synthesized [`TurnDirectiveRecord`]s.
fn handle_vendor_dollar_directive(trimmed: &str, span: SourceSpan, env: &mut CdfEnv) {
    let upper = trimmed.to_ascii_uppercase();
    let Some(rest) = upper.strip_prefix("SET") else {
        return;
    };
    let tokens: Vec<&str> = rest
        .split(|c: char| c.is_ascii_whitespace() || c == ',' || c == '=' || c == '(' || c == ')')
        .filter(|t| !t.is_empty())
        .collect();
    let mut idx = 0;
    while let Some(&tok) = tokens.get(idx) {
        match tok {
            "BOUND" | "SSRANGE" => env.turn_records.push(TurnDirectiveRecord {
                exception_names: vec![String::from("EC-BOUND-FATAL")],
                state: TurnCheckingState::EnabledWithLocation,
                span,
            }),
            "NOBOUND" | "NO-BOUND" | "NOSSRANGE" | "NO-SSRANGE" => {
                env.turn_records.push(TurnDirectiveRecord {
                    exception_names: vec![String::from("EC-BOUND-FATAL")],
                    state: TurnCheckingState::Disabled,
                    span,
                });
            }
            "DPC-IN-DATA" => {
                if let Some(&val_tok) = tokens.get(idx + 1)
                    && let Some(mode) = DpcInDataMode::parse_token(val_tok)
                {
                    env.dpc_in_data_records.push(DpcInDataRecord { mode, span });
                    idx += 1;
                }
            }
            "NO-DPC-IN-DATA" | "NODPC-IN-DATA" => {
                env.dpc_in_data_records.push(DpcInDataRecord {
                    mode: DpcInDataMode::None,
                    span,
                });
            }
            _ => {}
        }
        idx += 1;
    }
}

/// Dispatches a `>>...` CDF directive line.
///
/// # Arguments
///
/// * `body` - Text immediately following `>>`.
/// * `span` - Directive [`SourceSpan`].
/// * `active` - Current [`BranchActivity`].
/// * `stack` - Conditional nesting stack.
/// * `env` - Mutable [`CdfEnv`].
/// * `sink` - Diagnostic sink.
fn handle_chevron_directive(
    body: &str,
    span: SourceSpan,
    active: BranchActivity,
    stack: &mut Vec<CondFrame>,
    env: &mut CdfEnv,
    sink: &mut DiagnosticSink,
) {
    let mut parts = body.splitn(2, char::is_whitespace);
    let kw = parts.next().unwrap_or("").to_ascii_uppercase();
    let rest = parts.next().unwrap_or("").trim().trim_end_matches('.');

    match kw.as_str() {
        "IF" => handle_if(rest, span, active, stack, env, sink),
        "ELIF" | "ELSE-IF" => handle_elif(rest, span, stack, env, sink),
        "ELSE" => handle_else(span, stack, sink),
        "END-IF" => {
            if !matches!(stack.pop(), Some(CondFrame::If { .. })) {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0001Preprocessor,
                    Some(span),
                    String::from("unmatched `>>END-IF` directive"),
                ));
            }
        }
        "EVALUATE" => handle_evaluate(rest, active, stack, env),
        "WHEN" => handle_when(rest, span, stack, env, sink),
        "END-EVALUATE" => {
            if !matches!(stack.pop(), Some(CondFrame::Evaluate { .. })) {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0001Preprocessor,
                    Some(span),
                    String::from("unmatched `>>END-EVALUATE` directive"),
                ));
            }
        }
        _ if active == BranchActivity::Inactive => {}
        "DEFINE" => handle_define(rest, span, env, sink),
        "TURN" => handle_turn(rest, span, env, sink),
        "PUSH" => env.saved_stack.push(env.vars.clone()),
        "POP" => {
            if let Some(prev) = env.saved_stack.pop() {
                env.vars = prev;
            } else {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0001Preprocessor,
                    Some(span),
                    String::from("`>>POP` with empty `>>PUSH` stack"),
                ));
            }
        }
        "DISPLAY" => env
            .display_messages
            .push(String::from(rest.trim_matches(DQ).trim_matches(SQ))),
        "SET" | "CALL-CONVENTION" | "IMP" | "PAGE" | "LISTING" => {}
        other => {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0001Preprocessor,
                Some(span),
                format!("unknown compiler directive `>>{other}`"),
            ));
        }
    }
}

/// Pushes a new `>>IF` frame onto `stack`.
///
/// # Arguments
///
/// * `expr` - Conditional expression text.
/// * `span` - Directive [`SourceSpan`].
/// * `parent_active` - Enclosing [`BranchActivity`].
/// * `stack` - Conditional stack.
/// * `env` - Active [`CdfEnv`].
/// * `sink` - Diagnostic sink.
fn handle_if(
    expr: &str,
    span: SourceSpan,
    parent_active: BranchActivity,
    stack: &mut Vec<CondFrame>,
    env: &CdfEnv,
    sink: &mut DiagnosticSink,
) {
    if parent_active == BranchActivity::Inactive {
        stack.push(CondFrame::If {
            parent_active: BranchActivity::Inactive,
            matched_any: BranchMatchState::Matched,
            current_active: BranchActivity::Inactive,
        });
        return;
    }
    let cond = eval_cdf_bool_expr(expr, span, env, sink);
    stack.push(CondFrame::If {
        parent_active: BranchActivity::Active,
        matched_any: if cond {
            BranchMatchState::Matched
        } else {
            BranchMatchState::Unmatched
        },
        current_active: if cond {
            BranchActivity::Active
        } else {
            BranchActivity::Inactive
        },
    });
}

/// Updates the top `>>IF` frame for `>>ELIF`.
///
/// # Arguments
///
/// * `expr` - Conditional expression text.
/// * `span` - Directive [`SourceSpan`].
/// * `stack` - Conditional stack.
/// * `env` - Active [`CdfEnv`].
/// * `sink` - Diagnostic sink.
fn handle_elif(
    expr: &str,
    span: SourceSpan,
    stack: &mut [CondFrame],
    env: &CdfEnv,
    sink: &mut DiagnosticSink,
) {
    let Some(CondFrame::If {
        parent_active,
        matched_any,
        current_active,
    }) = stack.last_mut()
    else {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0001Preprocessor,
            Some(span),
            String::from("`>>ELIF` outside of `>>IF` block"),
        ));
        return;
    };
    if *parent_active == BranchActivity::Inactive || *matched_any == BranchMatchState::Matched {
        *current_active = BranchActivity::Inactive;
        return;
    }
    if eval_cdf_bool_expr(expr, span, env, sink) {
        *matched_any = BranchMatchState::Matched;
        *current_active = BranchActivity::Active;
    } else {
        *current_active = BranchActivity::Inactive;
    }
}

/// Updates the top `>>IF` frame for `>>ELSE`.
///
/// # Arguments
///
/// * `span` - Directive [`SourceSpan`].
/// * `stack` - Conditional stack.
/// * `sink` - Diagnostic sink.
fn handle_else(span: SourceSpan, stack: &mut [CondFrame], sink: &mut DiagnosticSink) {
    let Some(CondFrame::If {
        parent_active,
        matched_any,
        current_active,
    }) = stack.last_mut()
    else {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0001Preprocessor,
            Some(span),
            String::from("`>>ELSE` outside of `>>IF` block"),
        ));
        return;
    };
    if *parent_active == BranchActivity::Active && *matched_any == BranchMatchState::Unmatched {
        *matched_any = BranchMatchState::Matched;
        *current_active = BranchActivity::Active;
    } else {
        *current_active = BranchActivity::Inactive;
    }
}

/// Pushes a new `>>EVALUATE` frame onto `stack`.
///
/// # Arguments
///
/// * `rest` - Subject expression (`TRUE` or integer/variable).
/// * `parent_active` - Enclosing [`BranchActivity`].
/// * `stack` - Conditional stack.
/// * `env` - Active [`CdfEnv`].
fn handle_evaluate(
    rest: &str,
    parent_active: BranchActivity,
    stack: &mut Vec<CondFrame>,
    env: &CdfEnv,
) {
    let subj = if rest.eq_ignore_ascii_case("TRUE") {
        1
    } else {
        eval_cdf_int_atom(rest, env)
    };
    stack.push(CondFrame::Evaluate {
        parent_active,
        subject: subj,
        matched_any: BranchMatchState::Unmatched,
        current_active: BranchActivity::Inactive,
    });
}

/// Updates the top `>>EVALUATE` frame for `>>WHEN`.
///
/// # Arguments
///
/// * `rest` - `OTHER` or condition/value expression.
/// * `span` - Directive [`SourceSpan`].
/// * `stack` - Conditional stack.
/// * `env` - Active [`CdfEnv`].
/// * `sink` - Diagnostic sink.
fn handle_when(
    rest: &str,
    span: SourceSpan,
    stack: &mut [CondFrame],
    env: &CdfEnv,
    sink: &mut DiagnosticSink,
) {
    let Some(CondFrame::Evaluate {
        parent_active,
        subject,
        matched_any,
        current_active,
    }) = stack.last_mut()
    else {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0001Preprocessor,
            Some(span),
            String::from("`>>WHEN` outside of `>>EVALUATE` block"),
        ));
        return;
    };
    if *parent_active == BranchActivity::Inactive || *matched_any == BranchMatchState::Matched {
        *current_active = BranchActivity::Inactive;
        return;
    }
    let matches = if rest.eq_ignore_ascii_case("OTHER") {
        true
    } else {
        eval_cdf_int_atom(rest, env) == *subject
    };
    if matches {
        *matched_any = BranchMatchState::Matched;
        *current_active = BranchActivity::Active;
    } else {
        *current_active = BranchActivity::Inactive;
    }
}

/// Parses `>>DEFINE [CONSTANT] name [AS] {literal | PARAMETER | OFF} [OVERRIDE]`.
///
/// # Arguments
///
/// * `rest` - Tokens following `>>DEFINE`.
/// * `span` - Directive [`SourceSpan`].
/// * `env` - Mutable [`CdfEnv`].
/// * `sink` - Diagnostic sink.
fn handle_define(rest: &str, span: SourceSpan, env: &mut CdfEnv, sink: &mut DiagnosticSink) {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let mut pos = 0_usize;
    if words
        .get(pos)
        .is_some_and(|w| w.eq_ignore_ascii_case("CONSTANT"))
    {
        pos += 1;
    }
    let Some(&name) = words.get(pos) else {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0001Preprocessor,
            Some(span),
            String::from("missing variable name in `>>DEFINE`"),
        ));
        return;
    };
    pos += 1;
    if words.get(pos).is_some_and(|w| w.eq_ignore_ascii_case("AS")) {
        pos += 1;
    }

    let is_override = words
        .last()
        .is_some_and(|w| w.eq_ignore_ascii_case("OVERRIDE"));
    if env.get(name).is_some() && !is_override {
        // Note: `PARAMETER` preserves existing CLI `-D` value silently per ISO CDF rules.
        if words
            .get(pos)
            .is_some_and(|w| w.eq_ignore_ascii_case("PARAMETER"))
        {
            return;
        }
    }

    let val = match words.get(pos).copied() {
        None | Some("OVERRIDE") => CdfValue::DefinedUnit,
        Some(w) if w.eq_ignore_ascii_case("PARAMETER") => CdfValue::Unset,
        Some(w) if w.eq_ignore_ascii_case("OFF") => CdfValue::Unset,
        Some(w) => {
            if let Ok(int_val) = w.parse::<i128>() {
                CdfValue::Integer(int_val)
            } else {
                CdfValue::Text(String::from(w.trim_matches(DQ).trim_matches(SQ)))
            }
        }
    };
    env.set(name, val);
}

/// Parses `>>TURN ec-name ... CHECKING {ON [[WITH] LOCATION] | OFF}`.
///
/// # Arguments
///
/// * `rest` - Tokens following `>>TURN`.
/// * `span` - Directive [`SourceSpan`].
/// * `env` - Mutable [`CdfEnv`].
/// * `sink` - Diagnostic sink.
fn handle_turn(rest: &str, span: SourceSpan, env: &mut CdfEnv, sink: &mut DiagnosticSink) {
    let words: Vec<&str> = rest
        .split(|c: char| c.is_ascii_whitespace() || c == ',' || c == ';')
        .filter(|s| !s.is_empty())
        .collect();
    let Some(chk_pos) = words
        .iter()
        .position(|w| w.eq_ignore_ascii_case("CHECKING"))
    else {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0001Preprocessor,
            Some(span),
            String::from("expected `CHECKING` in `>>TURN` directive"),
        ));
        return;
    };
    let ec_names: Vec<String> = words
        .get(..chk_pos)
        .unwrap_or(&[])
        .iter()
        .map(|w| w.to_ascii_uppercase())
        .collect();
    let tail = words.get(chk_pos + 1..).unwrap_or(&[]);
    let state = if tail.first().is_some_and(|w| w.eq_ignore_ascii_case("OFF")) {
        TurnCheckingState::Disabled
    } else if tail.iter().any(|w| w.eq_ignore_ascii_case("LOCATION")) {
        TurnCheckingState::EnabledWithLocation
    } else {
        TurnCheckingState::Enabled
    };
    env.turn_records.push(TurnDirectiveRecord {
        exception_names: ec_names,
        state,
        span,
    });
}

/// Evaluates a CDF boolean condition (`OR`, `AND`, `NOT`, `DEFINED(...)`, `IS [NOT] DEFINED`, comparisons).
///
/// # Arguments
///
/// * `expr` - Raw boolean expression string.
/// * `span` - Directive [`SourceSpan`].
/// * `env` - Active [`CdfEnv`].
/// * `sink` - Diagnostic sink.
#[must_use]
fn eval_cdf_bool_expr(
    expr: &str,
    span: SourceSpan,
    env: &CdfEnv,
    sink: &mut DiagnosticSink,
) -> bool {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0001Preprocessor,
            Some(span),
            String::from("empty conditional expression in CDF directive"),
        ));
        return false;
    }

    if let Some((lhs, rhs)) = split_top_kw(trimmed, " OR ") {
        return eval_cdf_bool_expr(lhs, span, env, sink)
            || eval_cdf_bool_expr(rhs, span, env, sink);
    }
    if let Some((lhs, rhs)) = split_top_kw(trimmed, " AND ") {
        return eval_cdf_bool_expr(lhs, span, env, sink)
            && eval_cdf_bool_expr(rhs, span, env, sink);
    }

    let upper = trimmed.to_ascii_uppercase();
    if let Some(inner) = upper.strip_prefix("NOT ") {
        let orig_inner = trimmed.get(4..).unwrap_or(inner);
        return !eval_cdf_bool_expr(orig_inner, span, env, sink);
    }
    if let Some(def_arg) = upper
        .strip_prefix("DEFINED(")
        .and_then(|s| s.strip_suffix(')'))
    {
        return env.get(def_arg.trim()).is_some();
    }
    if let Some(var) = upper
        .strip_suffix(" IS NOT DEFINED")
        .or_else(|| upper.strip_suffix(" NOT DEFINED"))
    {
        return env.get(var.trim()).is_none();
    }
    if let Some(var) = upper.strip_suffix(" IS DEFINED") {
        return env.get(var.trim()).is_some();
    }
    if let Some(var) = upper.strip_prefix("DEFINED ") {
        return env.get(var.trim()).is_some();
    }

    eval_cdf_comparison(trimmed, env)
}

/// Splits `s` on case-insensitive `kw_upper` outside quotes.
///
/// # Arguments
///
/// * `s` - Input expression.
/// * `kw_upper` - Uppercase keyword with surrounding spaces (`" OR "` / `" AND "`).
#[must_use]
fn split_top_kw<'a>(s: &'a str, kw_upper: &str) -> Option<(&'a str, &'a str)> {
    let upper = s.to_ascii_uppercase();
    let pos = upper.find(kw_upper)?;
    let (lhs, rest) = s.split_at(pos);
    let (_, rhs) = rest.split_at(kw_upper.len());
    Some((lhs, rhs))
}

/// Evaluates relational comparisons (`<>`, `!=`, `<=`, `>=`, `=`, `<`, `>`) or truthy atoms.
///
/// # Arguments
///
/// * `expr` - Atomic comparison string.
/// * `env` - Active [`CdfEnv`].
#[must_use]
fn eval_cdf_comparison(expr: &str, env: &CdfEnv) -> bool {
    for (op_str, pred) in [
        ("<>", RelKind::Ne),
        ("!=", RelKind::Ne),
        ("<=", RelKind::Le),
        (">=", RelKind::Ge),
        ("=", RelKind::Eq),
        ("<", RelKind::Lt),
        (">", RelKind::Gt),
    ] {
        if let Some((l_raw, r_raw)) = expr.split_once(op_str) {
            let lv = resolve_atom_text_or_int(l_raw.trim(), env);
            let rv = resolve_atom_text_or_int(r_raw.trim(), env);
            return compare_atoms(&lv, &rv, pred);
        }
    }
    eval_cdf_int_atom(expr, env) != 0
}

/// Relational operator tag for CDF comparisons.
#[derive(Debug, Clone, Copy)]
enum RelKind {
    /// `=`
    Eq,
    /// `<>` / `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

/// Evaluates an arithmetic CDF expression (`+`, `-`, `*`, `/`) or variable/integer atom.
///
/// # Arguments
///
/// * `raw` - Expression text.
/// * `env` - Active [`CdfEnv`].
#[must_use]
fn eval_cdf_int_atom(raw: &str, env: &CdfEnv) -> i128 {
    let t = raw.trim();
    if let Some((a, b)) = t.rsplit_once(" + ") {
        return eval_cdf_int_atom(a, env).saturating_add(eval_cdf_int_atom(b, env));
    }
    if let Some((a, b)) = t.rsplit_once(" - ") {
        return eval_cdf_int_atom(a, env).saturating_sub(eval_cdf_int_atom(b, env));
    }
    if let Some((a, b)) = t.rsplit_once(" * ") {
        return eval_cdf_int_atom(a, env).saturating_mul(eval_cdf_int_atom(b, env));
    }
    if let Some((a, b)) = t.rsplit_once(" / ") {
        let denom = eval_cdf_int_atom(b, env);
        return if denom == 0 {
            0
        } else {
            eval_cdf_int_atom(a, env) / denom
        };
    }
    if t.eq_ignore_ascii_case("TRUE") {
        return 1;
    }
    if t.eq_ignore_ascii_case("FALSE") {
        return 0;
    }
    if let Ok(n) = t.parse::<i128>() {
        return n;
    }
    match env.vars.get(&t.to_ascii_uppercase()) {
        Some(CdfValue::Integer(n)) => *n,
        Some(CdfValue::DefinedUnit | CdfValue::Text(_)) => 1,
        Some(CdfValue::Unset) | None => 0,
    }
}

/// Resolved scalar operand for CDF relational comparisons.
enum ResolvedAtom {
    /// Signed integer value.
    Int(i128),
    /// String value.
    Str(String),
}

/// Resolves an operand into either [`ResolvedAtom::Int`] or [`ResolvedAtom::Str`].
///
/// # Arguments
///
/// * `raw` - Operand text.
/// * `env` - Active [`CdfEnv`].
#[must_use]
fn resolve_atom_text_or_int(raw: &str, env: &CdfEnv) -> ResolvedAtom {
    let unquoted = raw
        .strip_prefix(DQ)
        .and_then(|s| s.strip_suffix(DQ))
        .or_else(|| raw.strip_prefix(SQ).and_then(|s| s.strip_suffix(SQ)));
    if let Some(lit) = unquoted {
        return ResolvedAtom::Str(String::from(lit));
    }
    if let Some(CdfValue::Text(s)) = env.get(raw) {
        return ResolvedAtom::Str(s.clone());
    }
    ResolvedAtom::Int(eval_cdf_int_atom(raw, env))
}

/// Applies `pred` between two [`ResolvedAtom`] operands.
///
/// # Arguments
///
/// * `lhs` - Left operand.
/// * `rhs` - Right operand.
/// * `pred` - Relational predicate.
#[must_use]
fn compare_atoms(lhs: &ResolvedAtom, rhs: &ResolvedAtom, pred: RelKind) -> bool {
    if let (ResolvedAtom::Str(a), ResolvedAtom::Str(b)) = (lhs, rhs) {
        return match pred {
            RelKind::Eq => a == b,
            RelKind::Ne => a != b,
            RelKind::Lt => a < b,
            RelKind::Le => a <= b,
            RelKind::Gt => a > b,
            RelKind::Ge => a >= b,
        };
    }
    let a = match lhs {
        ResolvedAtom::Int(n) => *n,
        ResolvedAtom::Str(_) => 0,
    };
    let b = match rhs {
        ResolvedAtom::Int(n) => *n,
        ResolvedAtom::Str(_) => 0,
    };
    match pred {
        RelKind::Eq => a == b,
        RelKind::Ne => a != b,
        RelKind::Lt => a < b,
        RelKind::Le => a <= b,
        RelKind::Gt => a > b,
        RelKind::Ge => a >= b,
    }
}
