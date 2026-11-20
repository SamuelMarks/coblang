//! Semantic validation and canonical HIR lowering for Milestone M10 (`STRING`, `UNSTRING`,
//! `INSPECT`, `TRANSFORM`, `ACCEPT`, and `DISPLAY ... UPON ENVIRONMENT-*`).

use coblang_common::{Dialect, SourceSpan};
use coblang_diag::{Diagnostic, DiagnosticCode};
use coblang_hir::{
    HirAcceptSourceKind, HirAcceptSpec, HirAcceptTarget, HirConstValue, HirExpr, HirInspectAction,
    HirInspectConvertSpec, HirInspectQualifier, HirInspectReplaceClause, HirInspectSpec,
    HirInspectTallyClause, HirInspectWindow, HirPlace, HirStmt, HirStringDelim, HirStringSpec,
    HirType, HirUnstringMode, HirUnstringSpec, HirUnstringTargetSpec, Signedness, TruncationPolicy,
};
use coblang_syntax::ast::{
    AcceptSource, BoundSide, DataRef, DataRefModifier, DuplicateMatchMode, EventHandlers, Expr,
    InspectBound, InspectOperation, InspectQualifier, StringSourceGroup, UnstringDelimiter,
    UnstringTarget,
};

use crate::lower_hir::{
    UnitLowerCtx, lower_event_handlers, lower_expr_with_cat, push_high_op, resolve_place,
};

/// Borrowed AST components of an `UNSTRING` statement.
pub(crate) struct UnstringStmtParts<'a> {
    /// Source character item.
    pub(crate) source: &'a DataRef,
    /// `DELIMITED BY` list.
    pub(crate) delimiters: &'a [UnstringDelimiter],
    /// `INTO` target triplets.
    pub(crate) targets: &'a [UnstringTarget],
    /// Optional `WITH POINTER` target.
    pub(crate) pointer: Option<&'a DataRef>,
    /// Optional `TALLYING IN` target.
    pub(crate) tallying: Option<&'a DataRef>,
    /// `ON OVERFLOW` / `NOT ON OVERFLOW` handlers.
    pub(crate) handlers: &'a EventHandlers,
    /// Statement span.
    pub(crate) span: SourceSpan,
}

/// Validates and lowers an `ACCEPT` statement into [`HirStmt::Accept`] (or `HighLevelOp("ACCEPT")` for M19 `SCREEN`).
///
/// # Arguments
///
/// * `target` - Receiving `DataRef` (or bare `OMITTED`).
/// * `source` - AST [`AcceptSource`].
/// * `handlers` - `ON EXCEPTION` / `NOT ON EXCEPTION` handlers.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_accept_stmt(
    target: &DataRef,
    source: &AcceptSource,
    handlers: &EventHandlers,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let is_omitted = target.modifier == DataRefModifier::Plain
        && target.name.qualifiers.is_empty()
        && target.subscripts.is_empty()
        && target.refmod.is_none()
        && ctx
            .interner
            .resolve_canonical(target.name.base)
            .unwrap_or("")
            == "OMITTED";

    let hir_target = if is_omitted {
        HirAcceptTarget::Omitted
    } else {
        let Some(pl) = resolve_place(target, ctx) else {
            let _ = lower_event_handlers(handlers, ctx);
            return;
        };
        HirAcceptTarget::Place(pl)
    };

    let (on_exception, not_on_exception) = lower_event_handlers(handlers, ctx);
    let Some(src_kind) = resolve_accept_source(source, is_omitted, ctx) else {
        push_high_op("ACCEPT-SCREEN", span, out, ctx);
        return;
    };

    out.push(HirStmt::Accept(Box::new(HirAcceptSpec {
        target: hir_target,
        source: src_kind,
        on_exception,
        not_on_exception,
        span,
    })));
}

/// Maps AST [`AcceptSource`] to [`HirAcceptSourceKind`], returning `None` for M19 `SCREEN` forms.
///
/// # Arguments
///
/// * `source` - AST `ACCEPT` source clause.
/// * `is_omitted` - Whether the `ACCEPT` target is `OMITTED`.
/// * `ctx` - Unit lowering context.
#[must_use]
fn resolve_accept_source(
    source: &AcceptSource,
    is_omitted: bool,
    ctx: &UnitLowerCtx<'_>,
) -> Option<HirAcceptSourceKind> {
    let is_acu = ctx.dialect.base_dialect == Dialect::Acu;
    match source {
        AcceptSource::Screen => is_omitted.then_some(HirAcceptSourceKind::ConsoleLine),
        AcceptSource::InputDevice(None) => Some(HirAcceptSourceKind::ConsoleLine),
        AcceptSource::InputDevice(Some(sym)) => {
            let resolved_sym = ctx
                .device_map
                .iter()
                .find(|d| d.mnemonic == *sym)
                .map_or(*sym, |d| d.system_device);
            let name = ctx.interner.resolve_canonical(resolved_sym).unwrap_or("");
            match_accept_source_keyword(name, is_acu).or(Some(HirAcceptSourceKind::ConsoleLine))
        }
        AcceptSource::SystemClock(sym) | AcceptSource::ProcessInfo(sym) => {
            let name = ctx.interner.resolve_canonical(*sym).unwrap_or("");
            match_accept_source_keyword(name, is_acu)
        }
    }
}

/// Maps a canonical `ACCEPT ... FROM <keyword>` string to [`HirAcceptSourceKind`].
///
/// # Arguments
///
/// * `kw` - Uppercase canonical source keyword.
/// * `is_acu` - Whether `-std=acu` microsecond `TIME` widening is active.
#[must_use]
fn match_accept_source_keyword(kw: &str, is_acu: bool) -> Option<HirAcceptSourceKind> {
    match kw {
        "DATE" => Some(HirAcceptSourceKind::DateYymmdd),
        "DATE YYYYMMDD" => Some(HirAcceptSourceKind::DateYyyymmdd),
        "DAY" => Some(HirAcceptSourceKind::DayYyddd),
        "DAY YYYYDDD" => Some(HirAcceptSourceKind::DayYyyyddd),
        "DAY-OF-WEEK" => Some(HirAcceptSourceKind::DayOfWeek),
        "TIME" => Some(if is_acu {
            HirAcceptSourceKind::TimeAcu12
        } else {
            HirAcceptSourceKind::TimeStandard8
        }),
        "MICROSECOND-TIME" => Some(HirAcceptSourceKind::MicrosecondTime12),
        "ARGUMENT-NUMBER" => Some(HirAcceptSourceKind::ArgumentNumber),
        "ARGUMENT-VALUE" => Some(HirAcceptSourceKind::ArgumentValue),
        "COMMAND-LINE" => Some(HirAcceptSourceKind::CommandLine),
        "ENVIRONMENT-NAME" => Some(HirAcceptSourceKind::EnvironmentName),
        "ENVIRONMENT-VALUE" => Some(HirAcceptSourceKind::EnvironmentValue),
        "SYSIN" | "STDIN" | "CONSOLE" | "KEYBOARD" => Some(HirAcceptSourceKind::ConsoleLine),
        _ => None,
    }
}

/// Validates and lowers a `STRING` statement into [`HirStmt::StringVerb`].
///
/// # Arguments
///
/// * `groups` - Source groups with `DELIMITED BY` specifications.
/// * `into` - Destination `INTO` reference.
/// * `pointer` - Optional `WITH POINTER` reference.
/// * `handlers` - `ON OVERFLOW` / `NOT ON OVERFLOW` handlers.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_string_stmt(
    groups: &[StringSourceGroup],
    into: &DataRef,
    pointer: Option<&DataRef>,
    handlers: &EventHandlers,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut pieces = Vec::new();
    for g in groups {
        let delim = match &g.delimiter {
            None => HirStringDelim::Size,
            Some(d_ast) => {
                let d_hir = lower_expr_with_cat(d_ast, ctx).0;
                check_display_usage_expr(&d_hir, "STRING", ctx);
                HirStringDelim::Pattern(d_hir)
            }
        };
        for s_ast in &g.sources {
            let s_hir = lower_expr_with_cat(s_ast, ctx).0;
            check_display_usage_expr(&s_hir, "STRING", ctx);
            pieces.push((s_hir, delim.clone()));
        }
    }

    let into_opt = resolve_place(into, ctx);
    if let Some(ref into_pl) = into_opt {
        check_display_usage_type(&into_pl.effective_ty, "STRING", into.span, ctx);
    }
    let static_into_len = into_opt.as_ref().and_then(|pl| {
        into.refmod.is_none().then(|| {
            crate::m17_charset::refmod_max_positions(
                &pl.effective_ty,
                pl.effective_ty.static_byte_size(),
            )
            .max(1)
        })
    });

    let ptr_pl = pointer.and_then(|p_ref| {
        let pl = resolve_place(p_ref, ctx)?;
        validate_int_counter_or_pointer(&pl, static_into_len, "POINTER", p_ref.span, ctx);
        Some(pl)
    });
    let (on_overflow, not_on_overflow) = lower_event_handlers(handlers, ctx);

    if let Some(into_pl) = into_opt {
        out.push(HirStmt::StringVerb(Box::new(HirStringSpec {
            pieces,
            into: into_pl,
            pointer: ptr_pl,
            on_overflow,
            not_on_overflow,
            span,
        })));
    }
}

/// Validates and lowers an `UNSTRING` statement into [`HirStmt::Unstring`].
///
/// # Arguments
///
/// * `parts` - Borrowed [`UnstringStmtParts`].
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_unstring_stmt(
    parts: &UnstringStmtParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let src_opt = resolve_place(parts.source, ctx);
    if let Some(ref src_pl) = src_opt {
        check_display_usage_type(&src_pl.effective_ty, "UNSTRING", parts.source.span, ctx);
    }
    let static_src_len = src_opt.as_ref().and_then(|pl| {
        parts.source.refmod.is_none().then(|| {
            crate::m17_charset::refmod_max_positions(
                &pl.effective_ty,
                pl.effective_ty.static_byte_size(),
            )
            .max(1)
        })
    });

    let mut delimiters = Vec::with_capacity(parts.delimiters.len());
    for d in parts.delimiters {
        let mode = match d.all_occurrences {
            DuplicateMatchMode::Single => HirUnstringMode::Single,
            DuplicateMatchMode::AllRepeated => HirUnstringMode::AllRepeated,
        };
        let pat_hir = lower_expr_with_cat(&d.pattern, ctx).0;
        check_display_usage_expr(&pat_hir, "UNSTRING", ctx);
        delimiters.push((mode, pat_hir));
    }

    let mut targets = Vec::with_capacity(parts.targets.len());
    for t in parts.targets {
        let into_opt = resolve_place(&t.into, ctx);
        let delim_in = t
            .delimiter_in
            .as_ref()
            .and_then(|dr| resolve_place(dr, ctx));
        let count_in = t.count_in.as_ref().and_then(|cr| {
            let pl = resolve_place(cr, ctx)?;
            validate_int_counter_or_pointer(&pl, None, "COUNT", cr.span, ctx);
            Some(pl)
        });
        if let Some(into) = into_opt {
            targets.push(HirUnstringTargetSpec {
                into,
                delimiter_in: delim_in,
                count_in,
            });
        }
    }

    let pointer = parts.pointer.and_then(|pr| {
        let pl = resolve_place(pr, ctx)?;
        validate_int_counter_or_pointer(&pl, static_src_len, "POINTER", pr.span, ctx);
        Some(pl)
    });
    let tallying = parts.tallying.and_then(|tr| {
        let pl = resolve_place(tr, ctx)?;
        validate_int_counter_or_pointer(&pl, None, "TALLYING", tr.span, ctx);
        Some(pl)
    });
    let (on_overflow, not_on_overflow) = lower_event_handlers(parts.handlers, ctx);

    if let Some(source) = src_opt {
        out.push(HirStmt::Unstring(Box::new(HirUnstringSpec {
            source,
            delimiters,
            targets,
            pointer,
            tallying,
            on_overflow,
            not_on_overflow,
            span: parts.span,
        })));
    }
}

/// Validates and lowers an `INSPECT` statement into one or more [`HirStmt::Inspect`] nodes.
///
/// # Arguments
///
/// * `target` - Inspected field reference.
/// * `operations` - Ordered `TALLYING`, `REPLACING`, and `CONVERTING` operations.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_inspect_stmt(
    target: &DataRef,
    operations: &[InspectOperation],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let target_opt = resolve_place(target, ctx);
    if let Some(ref tpl) = target_opt {
        check_display_usage_type(&tpl.effective_ty, "INSPECT", target.span, ctx);
    }

    let mut counters: Vec<HirPlace> = Vec::new();
    let mut tallies: Vec<HirInspectTallyClause> = Vec::new();
    let mut replaces: Vec<HirInspectReplaceClause> = Vec::new();
    let mut converts: Vec<(HirExpr, HirExpr, HirInspectWindow)> = Vec::new();

    for op in operations {
        match op {
            InspectOperation::Tallying {
                counter,
                qualifier,
                pattern,
                bounds,
            } => {
                let pat_hir = pattern.as_ref().map(|p| lower_inspect_operand(p, ctx));
                let window = lower_inspect_bounds(bounds, ctx);
                if let Some(c_pl) = resolve_place(counter, ctx) {
                    let c_idx = counters
                        .iter()
                        .position(|existing| existing == &c_pl)
                        .unwrap_or_else(|| {
                            let idx = counters.len();
                            counters.push(c_pl);
                            idx
                        });
                    tallies.push(HirInspectTallyClause {
                        counter_idx: c_idx,
                        qualifier: map_inspect_qualifier(*qualifier),
                        pattern: pat_hir,
                        window,
                    });
                }
            }
            InspectOperation::Replacing {
                qualifier,
                pattern,
                by,
                bounds,
            } => {
                let pat_hir = pattern.as_ref().map(|p| lower_inspect_operand(p, ctx));
                let by_hir = lower_inspect_operand(by, ctx);
                let window = lower_inspect_bounds(bounds, ctx);
                replaces.push(HirInspectReplaceClause {
                    qualifier: map_inspect_qualifier(*qualifier),
                    pattern: pat_hir,
                    by: by_hir,
                    window,
                });
            }
            InspectOperation::Converting { from, to, bounds } => {
                let from_hir = lower_inspect_operand(from, ctx);
                let to_hir = lower_inspect_operand(to, ctx);
                let window = lower_inspect_bounds(bounds, ctx);
                converts.push((from_hir, to_hir, window));
            }
        }
    }

    let Some(target_pl) = target_opt else {
        return;
    };
    if !tallies.is_empty() || !replaces.is_empty() {
        out.push(HirStmt::Inspect(Box::new(HirInspectSpec {
            target: target_pl.clone(),
            action: HirInspectAction::TallyAndReplace {
                counters,
                tallies,
                replaces,
            },
            span,
        })));
    }
    for (from, to, window) in converts {
        out.push(HirStmt::Inspect(Box::new(HirInspectSpec {
            target: target_pl.clone(),
            action: HirInspectAction::Converting(Box::new(HirInspectConvertSpec {
                from,
                to,
                window,
            })),
            span,
        })));
    }
}

/// Validates and lowers `TRANSFORM target CHARACTERS FROM from TO to` into [`HirStmt::Inspect`].
///
/// # Arguments
///
/// * `target` - Transformed field reference.
/// * `from` - Source alphabet expression.
/// * `to` - Replacement alphabet expression.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_transform_stmt(
    target: &DataRef,
    from: &Expr,
    to: &Expr,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let target_opt = resolve_place(target, ctx);
    if let Some(ref tpl) = target_opt {
        check_display_usage_type(&tpl.effective_ty, "TRANSFORM", target.span, ctx);
    }
    let from_hir = lower_inspect_operand(from, ctx);
    let to_hir = lower_inspect_operand(to, ctx);
    if let Some(target_pl) = target_opt {
        out.push(HirStmt::Inspect(Box::new(HirInspectSpec {
            target: target_pl,
            action: HirInspectAction::Converting(Box::new(HirInspectConvertSpec {
                from: from_hir,
                to: to_hir,
                window: HirInspectWindow::default(),
            })),
            span,
        })));
    }
}

/// Lowers an `INSPECT`/`TRANSFORM` operand, resolving bare `SPECIAL-NAMES. ALPHABET` names to byte literals.
///
/// # Arguments
///
/// * `expr` - AST operand expression.
/// * `ctx` - Unit lowering context.
fn lower_inspect_operand(expr: &Expr, ctx: &mut UnitLowerCtx<'_>) -> HirExpr {
    if let Expr::Data(dref) = expr
        && dref.modifier == DataRefModifier::Plain
        && dref.name.qualifiers.is_empty()
        && dref.subscripts.is_empty()
        && dref.refmod.is_none()
        && let Some(alpha) = ctx.alphabets.iter().find(|a| a.name == dref.name.base)
    {
        let mut chars = vec![0_u8; 256];
        for (byte_val, &rank) in alpha.ranks.iter().enumerate() {
            let _ = chars
                .get_mut(usize::from(rank))
                .map(|slot| *slot = u8::try_from(byte_val).unwrap_or(0));
        }
        return HirExpr::Const {
            value: HirConstValue::Bytes(chars),
            span: dref.span,
        };
    }
    let hir = lower_expr_with_cat(expr, ctx).0;
    check_display_usage_expr(&hir, "INSPECT", ctx);
    hir
}

/// Lowers `BEFORE`/`AFTER` bounds on an `INSPECT` clause into [`HirInspectWindow`].
///
/// # Arguments
///
/// * `bounds` - AST [`InspectBound`] slice.
/// * `ctx` - Unit lowering context.
fn lower_inspect_bounds(bounds: &[InspectBound], ctx: &mut UnitLowerCtx<'_>) -> HirInspectWindow {
    let mut win = HirInspectWindow::default();
    for b in bounds {
        let pat = lower_inspect_operand(&b.pattern, ctx);
        match b.side {
            BoundSide::After => win.after = Some(pat),
            BoundSide::Before => win.before = Some(pat),
        }
    }
    win
}

/// Maps AST [`InspectQualifier`] to [`HirInspectQualifier`].
///
/// # Arguments
///
/// * `q` - AST qualifier.
#[must_use]
const fn map_inspect_qualifier(q: InspectQualifier) -> HirInspectQualifier {
    match q {
        InspectQualifier::Characters => HirInspectQualifier::Characters,
        InspectQualifier::All => HirInspectQualifier::All,
        InspectQualifier::Leading => HirInspectQualifier::Leading,
        InspectQualifier::First => HirInspectQualifier::First,
        InspectQualifier::Trailing => HirInspectQualifier::Trailing,
    }
}

/// Emits a semantic error when `expr` is a direct `Place` whose `USAGE` is neither `DISPLAY` nor `NATIONAL`/`UTF-8`.
///
/// # Arguments
///
/// * `expr` - Lowered [`HirExpr`].
/// * `verb` - Enclosing verb name (`"STRING"`, `"UNSTRING"`, `"INSPECT"`).
/// * `ctx` - Unit lowering context.
fn check_display_usage_expr(expr: &HirExpr, verb: &'static str, ctx: &mut UnitLowerCtx<'_>) {
    if let HirExpr::Place(pl) = expr {
        check_display_usage_type(&pl.effective_ty, verb, pl.span, ctx);
    }
}

/// Emits `E0008TypeMismatch` if `ty` is non-display (`BINARY`, `PACKED-DECIMAL`, `FLOAT`, `POINTER`, `INDEX`, `BIT`).
///
/// # Arguments
///
/// * `ty` - Inspected [`HirType`].
/// * `verb` - Enclosing verb name.
/// * `span` - Operand span.
/// * `ctx` - Unit lowering context.
fn check_display_usage_type(
    ty: &HirType,
    verb: &'static str,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let is_valid_display = matches!(
        ty,
        HirType::Alphabetic { .. }
            | HirType::Alphanumeric { .. }
            | HirType::AlphanumericEdited { .. }
            | HirType::ZonedDecimal { .. }
            | HirType::NumericEdited { .. }
            | HirType::National { .. }
            | HirType::NationalEdited { .. }
            | HirType::Utf8 { .. }
            | HirType::Group { .. }
    );
    if !is_valid_display {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            format!("{verb} item must be USAGE DISPLAY or NATIONAL"),
        ));
    }
}

/// Validates that `pl` is an elementary unscaled integer (and optionally large enough for `size + 1` in `POINTER`).
///
/// # Arguments
///
/// * `pl` - Resolved [`HirPlace`].
/// * `target_size_opt` - Optional static character size of `INTO`/source (`POINTER` requires capacity `>= size + 1`).
/// * `clause_label` - `"POINTER"`, `"COUNT"`, or `"TALLYING"`.
/// * `span` - Clause span.
/// * `ctx` - Unit lowering context.
fn validate_int_counter_or_pointer(
    pl: &HirPlace,
    target_size_opt: Option<u32>,
    clause_label: &'static str,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let max_val_opt: Option<u128> = match &pl.effective_ty {
        HirType::ZonedDecimal { digits, scale, .. }
        | HirType::PackedDecimal { digits, scale, .. }
            if scale.get() == 0 =>
        {
            Some(pow10_minus_one(digits.get()))
        }
        HirType::BinaryInt {
            byte_width,
            digits,
            scale,
            sign,
            trunc,
            ..
        } if scale.get() == 0 => Some(match trunc {
            TruncationPolicy::TruncateToPic => pow10_minus_one(digits.get()),
            TruncationPolicy::FullBinaryRange => {
                let bits = u32::from(*byte_width) * 8 - u32::from(*sign == Signedness::Signed);
                if bits >= 127 {
                    u128::MAX
                } else {
                    (1_u128 << bits) - 1
                }
            }
        }),
        HirType::Index => Some(u128::from(u64::MAX)),
        _ => None,
    };

    let Some(max_val) = max_val_opt else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            format!("{clause_label} item must be an elementary unscaled integer"),
        ));
        return;
    };

    if let Some(sz) = target_size_opt {
        let required = u128::from(sz) + 1;
        if max_val < required {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(span),
                format!("{clause_label} item is too small to contain {required}"),
            ));
        }
    }
}

/// Computes $10^d - 1$ (`u128::MAX` when $d \ge 38$).
///
/// # Arguments
///
/// * `digits` - Decimal digit count (`1..=38`).
#[must_use]
const fn pow10_minus_one(digits: u8) -> u128 {
    if digits >= 38 {
        return u128::MAX;
    }
    10_u128.pow(digits as u32) - 1
}
