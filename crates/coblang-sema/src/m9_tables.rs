//! Milestone M9 table operations: scoped `EC-BOUND-*` masks, `SEARCH` / `SEARCH ALL` desugaring,
//! and in-memory `SORT table` semantic lowering.

use coblang_common::{DataItemId, ScaleFactor, SourceSpan, SymbolId};
use coblang_diag::{BoundCheckToggle, Diagnostic, DiagnosticCode, InitialBoundsChecks};
use coblang_hir::{
    ExceptionCheckMask, HirBinOp, HirCondition, HirConstValue, HirDataItem, HirDecimalSign,
    HirExpr, HirFlowExitKind, HirLoopKind, HirLoopTest, HirOccursDim, HirPlace, HirRelOp,
    HirRounding, HirSortDirection, HirSortTableKey, HirSortTableSpec, HirStmt, HirTableKey,
};
use coblang_pp::{TurnCheckingState, TurnDirectiveRecord};
use coblang_syntax::ast::{
    AbbrevRhs, Condition, DataRef, Expr, KeyOrder, NegationMode, RelOp, SearchMode, Stmt,
    SubscriptItem,
};

use crate::lower_hir::{
    UnitLowerCtx, lower_condition, lower_expr_with_cat, lower_stmt_slice, resolve_place,
    simple_place_for_item,
};
use crate::m8_flow::{alloc_i64_counter_place, const_int_expr};
use crate::special_names_hir::ebcdic_collation_ranks;
use crate::symbols::ResolvedSymbol;

/// Computes the active [`ExceptionCheckMask`] at `span` from CLI defaults and preceding `>>TURN` / `$SET` directives.
///
/// # Arguments
///
/// * `span` - Reference [`SourceSpan`] being evaluated.
/// * `base` - Initial CLI bounds-checking toggles (`-debug`, `-fec=`, `-fno-ec=`).
/// * `directives` - Ordered `>>TURN` and `$SET BOUND`/`SSRANGE` records in the compilation group.
#[must_use]
pub(crate) fn compute_active_check_mask(
    span: SourceSpan,
    base: InitialBoundsChecks,
    directives: &[TurnDirectiveRecord],
) -> ExceptionCheckMask {
    let mut mask = ExceptionCheckMask::NONE;
    if base.subscript == BoundCheckToggle::EnabledFatal {
        mask = mask
            .union(ExceptionCheckMask::BOUND_SUBSCRIPT)
            .union(ExceptionCheckMask::BOUND_FATAL);
    }
    if base.odo == BoundCheckToggle::EnabledFatal {
        mask = mask
            .union(ExceptionCheckMask::BOUND_ODO)
            .union(ExceptionCheckMask::BOUND_FATAL);
    }
    if base.refmod == BoundCheckToggle::EnabledFatal {
        mask = mask
            .union(ExceptionCheckMask::BOUND_REF_MOD)
            .union(ExceptionCheckMask::BOUND_FATAL);
    }

    for rec in directives {
        if rec.span.file_id != span.file_id || rec.span.start > span.start {
            continue;
        }
        for raw_ec in &rec.exception_names {
            let bits = classify_turn_exception_bits(raw_ec);
            if bits == ExceptionCheckMask::NONE {
                continue;
            }
            match rec.state {
                TurnCheckingState::Disabled => {
                    mask = mask.without(bits);
                    let any_bound = ExceptionCheckMask::BOUND_SUBSCRIPT
                        .union(ExceptionCheckMask::BOUND_ODO)
                        .union(ExceptionCheckMask::BOUND_REF_MOD);
                    if (mask.bits() & any_bound.bits()) == 0 {
                        mask = mask.without(ExceptionCheckMask::BOUND_FATAL);
                    }
                }
                TurnCheckingState::Enabled => {
                    mask = mask.union(bits.without(ExceptionCheckMask::BOUND_FATAL));
                }
                TurnCheckingState::EnabledWithLocation => {
                    mask = mask.union(bits).union(ExceptionCheckMask::BOUND_FATAL);
                }
            }
        }
    }
    mask
}

/// Maps an uppercase `>>TURN` / `$SET` exception name to its [`ExceptionCheckMask`] bits.
///
/// # Arguments
///
/// * `raw_ec` - Exception identifier string.
#[must_use]
fn classify_turn_exception_bits(raw_ec: &str) -> ExceptionCheckMask {
    let all_bounds = ExceptionCheckMask::BOUND_SUBSCRIPT
        .union(ExceptionCheckMask::BOUND_ODO)
        .union(ExceptionCheckMask::BOUND_REF_MOD);
    match raw_ec.trim().to_ascii_uppercase().as_str() {
        "EC-ALL" | "EC-BOUND" => all_bounds,
        "EC-BOUND-FATAL" => all_bounds.union(ExceptionCheckMask::BOUND_FATAL),
        "EC-BOUND-SUBSCRIPT" => ExceptionCheckMask::BOUND_SUBSCRIPT,
        "EC-BOUND-ODO" => ExceptionCheckMask::BOUND_ODO,
        "EC-BOUND-REF-MOD" => ExceptionCheckMask::BOUND_REF_MOD,
        _ => ExceptionCheckMask::NONE,
    }
}

/// Bundled AST inputs for [`lower_search_stmt`].
pub(crate) struct SearchStmtParts<'a> {
    /// Serial `SEARCH` vs binary `SEARCH ALL`.
    pub(crate) mode: SearchMode,
    /// Searched table reference.
    pub(crate) table: &'a DataRef,
    /// Optional `VARYING` index/identifier.
    pub(crate) varying: Option<&'a DataRef>,
    /// Optional `AT END` handler statements.
    pub(crate) at_end: &'a [Stmt],
    /// `(when_condition, when_body)` arms.
    pub(crate) when_arms: &'a [(Condition, Vec<Stmt>)],
    /// Statement span.
    pub(crate) span: SourceSpan,
}

/// Desugars serial `SEARCH` and binary `SEARCH ALL` into canonical `HirStmt::Loop` + `HirStmt::If` IR.
///
/// # Arguments
///
/// * `parts` - [`SearchStmtParts`] AST components.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_search_stmt(
    parts: &SearchStmtParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(ResolvedTableTarget {
        table_item,
        outer_subs,
        own_dim,
    }) = resolve_table_target(parts.table, ctx)
    else {
        return;
    };
    let varying_pl = parts.varying.and_then(|v| resolve_place(v, ctx));
    let primary_idx = if let Some(vp) = &varying_pl
        && own_dim.indexed_by.contains(&vp.item)
    {
        vp.clone()
    } else if let Some(idx_item) = own_dim
        .indexed_by
        .first()
        .and_then(|&idx_id| ctx.layout.items.get(idx_id.as_u32() as usize))
    {
        simple_place_for_item(idx_item, parts.span)
    } else if let Some(vp) = &varying_pl {
        vp.clone()
    } else {
        alloc_i64_counter_place(parts.span, ctx)
    };

    let count_expr = build_table_count_expr(&own_dim, parts.span, ctx);
    let matched_pl = alloc_i64_counter_place(parts.span, ctx);
    out.push(HirStmt::Move {
        src: const_int_expr("0", parts.span, ctx),
        dst: matched_pl.clone(),
        span: parts.span,
    });

    match parts.mode {
        SearchMode::Serial => emit_serial_search_loop(
            parts,
            &primary_idx,
            varying_pl.as_ref(),
            count_expr,
            &matched_pl,
            out,
            ctx,
        ),
        SearchMode::BinaryAll => emit_binary_search_all(
            parts,
            &table_item,
            &outer_subs,
            &own_dim,
            &primary_idx,
            count_expr,
            &matched_pl,
            out,
            ctx,
        ),
    }

    if !parts.at_end.is_empty() {
        let mut end_stmts = Vec::new();
        lower_stmt_slice(parts.at_end, &mut end_stmts, ctx);
        out.push(HirStmt::If {
            cond: HirCondition::Compare {
                op: HirRelOp::Eq,
                lhs: HirExpr::Place(matched_pl),
                rhs: const_int_expr("0", parts.span, ctx),
            },
            then_body: end_stmts,
            else_body: Vec::new(),
            span: parts.span,
        });
    }
}

/// Emits the serial `SEARCH` loop (`primary_idx >= 1` guard + `WHEN` cascade + index increment).
///
/// # Arguments
///
/// * `parts` - [`SearchStmtParts`] AST components.
/// * `primary_idx` - Primary `INDEXED BY` place.
/// * `varying_pl` - Optional explicit `VARYING` place.
/// * `count_expr` - Active table occurrence count expression.
/// * `matched_pl` - `0`/`1` match-found flag place.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn emit_serial_search_loop(
    parts: &SearchStmtParts<'_>,
    primary_idx: &HirPlace,
    varying_pl: Option<&HirPlace>,
    count_expr: HirExpr,
    matched_pl: &HirPlace,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let one = const_int_expr("1", parts.span, ctx);
    let mut step_stmts = vec![make_incr_stmt(primary_idx, &one, parts.span)];
    if let Some(vp) = varying_pl
        && vp.item != primary_idx.item
    {
        step_stmts.push(make_incr_stmt(vp, &one, parts.span));
    }

    let mut cascade = step_stmts;
    for (cond_ast, body_ast) in parts.when_arms.iter().rev() {
        let cond_hir = lower_condition(cond_ast, ctx);
        let mut hit_body = vec![HirStmt::Move {
            src: one.clone(),
            dst: matched_pl.clone(),
            span: parts.span,
        }];
        lower_stmt_slice(body_ast, &mut hit_body, ctx);
        hit_body.push(HirStmt::FlowExit {
            kind: HirFlowExitKind::ExitPerformBreak,
            span: parts.span,
        });
        cascade = vec![HirStmt::If {
            cond: cond_hir,
            then_body: hit_body,
            else_body: cascade,
            span: parts.span,
        }];
    }

    let search_loop = HirStmt::Loop {
        kind: HirLoopKind::UserPerform,
        test_before: HirLoopTest::Before,
        until: HirCondition::Compare {
            op: HirRelOp::Gt,
            lhs: HirExpr::Place(primary_idx.clone()),
            rhs: count_expr,
        },
        body: cascade,
        cycle_step: Vec::new(),
        span: parts.span,
    };

    out.push(HirStmt::If {
        cond: HirCondition::Compare {
            op: HirRelOp::Ge,
            lhs: HirExpr::Place(primary_idx.clone()),
            rhs: one,
        },
        then_body: vec![search_loop],
        else_body: Vec::new(),
        span: parts.span,
    });
}

/// Emits binary `SEARCH ALL` over `own_dim.keys` (falling back to `1..=count` scan when keys are unconstrained).
///
/// # Arguments
///
/// * `parts` - [`SearchStmtParts`] AST components.
/// * `table_item` - Resolved table [`HirDataItem`].
/// * `outer_subs` - Outer dimension `(expr, stride)` pairs.
/// * `own_dim` - Own [`HirOccursDim`] of the searched table.
/// * `primary_idx` - Primary `INDEXED BY` place.
/// * `count_expr` - Active table occurrence count expression.
/// * `matched_pl` - `0`/`1` match-found flag place.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
#[expect(
    clippy::too_many_arguments,
    reason = "internal helper threading lowered places and table metadata cleanly without wrapper churn"
)]
fn emit_binary_search_all(
    parts: &SearchStmtParts<'_>,
    table_item: &HirDataItem,
    outer_subs: &[(HirExpr, u32)],
    own_dim: &HirOccursDim,
    primary_idx: &HirPlace,
    count_expr: HirExpr,
    matched_pl: &HirPlace,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some((when_cond_ast, when_body_ast)) = parts.when_arms.first() else {
        return;
    };
    let key_targets = match_search_all_keys(&own_dim.keys, when_cond_ast, ctx);
    if key_targets.is_empty() {
        out.push(HirStmt::Move {
            src: const_int_expr("1", parts.span, ctx),
            dst: primary_idx.clone(),
            span: parts.span,
        });
        emit_serial_search_loop(parts, primary_idx, None, count_expr, matched_pl, out, ctx);
        return;
    }

    let one = const_int_expr("1", parts.span, ctx);
    let two = const_u32_expr(2, parts.span, ctx);
    let low_pl = alloc_i64_counter_place(parts.span, ctx);
    let high_pl = alloc_i64_counter_place(parts.span, ctx);
    out.push(HirStmt::Move {
        src: one.clone(),
        dst: low_pl.clone(),
        span: parts.span,
    });
    out.push(HirStmt::Move {
        src: count_expr,
        dst: high_pl.clone(),
        span: parts.span,
    });

    let full_cond = lower_condition(when_cond_ast, ctx);
    let mut hit_body = vec![HirStmt::Move {
        src: one.clone(),
        dst: matched_pl.clone(),
        span: parts.span,
    }];
    lower_stmt_slice(when_body_ast, &mut hit_body, ctx);
    hit_body.push(HirStmt::FlowExit {
        kind: HirFlowExitKind::ExitPerformBreak,
        span: parts.span,
    });

    let adjust_bounds = build_binary_key_cascade(
        &key_targets,
        table_item,
        outer_subs,
        own_dim.element_byte_stride,
        primary_idx,
        (&low_pl, &high_pl),
        parts.span,
        ctx,
    );

    let mid_sum = HirExpr::Binary {
        op: HirBinOp::Add,
        lhs: Box::new(HirExpr::Place(low_pl.clone())),
        rhs: Box::new(HirExpr::Place(high_pl.clone())),
        span: parts.span,
    };
    let calc_mid = HirStmt::AssignArith {
        expr: HirExpr::Binary {
            op: HirBinOp::Div,
            lhs: Box::new(mid_sum),
            rhs: Box::new(two),
            span: parts.span,
        },
        targets: vec![(primary_idx.clone(), HirRounding::Truncate)],
        on_size_error: Vec::new(),
        not_on_size_error: Vec::new(),
        span: parts.span,
    };
    let check_if = HirStmt::If {
        cond: full_cond,
        then_body: hit_body,
        else_body: adjust_bounds,
        span: parts.span,
    };

    out.push(HirStmt::Loop {
        kind: HirLoopKind::UserPerform,
        test_before: HirLoopTest::Before,
        until: HirCondition::Compare {
            op: HirRelOp::Gt,
            lhs: HirExpr::Place(low_pl),
            rhs: HirExpr::Place(high_pl),
        },
        body: vec![calc_mid, check_if],
        cycle_step: Vec::new(),
        span: parts.span,
    });
}

/// Builds the lexicographic `low = idx + 1` / `high = idx - 1` cascade for `SEARCH ALL`.
///
/// # Arguments
///
/// * `key_targets` - Matched `(HirTableKey, target_expr)` prefix.
/// * `table_item` - Searched table [`HirDataItem`].
/// * `outer_subs` - Outer dimension `(expr, stride)` pairs.
/// * `elem_stride` - Byte stride of the searched dimension.
/// * `primary_idx` - Current midpoint index place.
/// * `bounds` - `(low_pl, high_pl)` binary search bound places.
/// * `span` - Statement span.
/// * `ctx` - Unit lowering context.
#[expect(
    clippy::too_many_arguments,
    reason = "internal helper constructing binary-search key cascade cleanly"
)]
fn build_binary_key_cascade(
    key_targets: &[(HirTableKey, HirExpr)],
    table_item: &HirDataItem,
    outer_subs: &[(HirExpr, u32)],
    elem_stride: u32,
    primary_idx: &HirPlace,
    bounds: (&HirPlace, &HirPlace),
    span: SourceSpan,
    ctx: &UnitLowerCtx<'_>,
) -> Vec<HirStmt> {
    let (low_pl, high_pl) = bounds;
    let one = const_int_expr("1", span, ctx);
    let go_right = vec![HirStmt::AssignArith {
        expr: HirExpr::Binary {
            op: HirBinOp::Add,
            lhs: Box::new(HirExpr::Place(primary_idx.clone())),
            rhs: Box::new(one.clone()),
            span,
        },
        targets: vec![(low_pl.clone(), HirRounding::Truncate)],
        on_size_error: Vec::new(),
        not_on_size_error: Vec::new(),
        span,
    }];
    let go_left = vec![HirStmt::AssignArith {
        expr: HirExpr::Binary {
            op: HirBinOp::Sub,
            lhs: Box::new(HirExpr::Place(primary_idx.clone())),
            rhs: Box::new(one),
            span,
        },
        targets: vec![(high_pl.clone(), HirRounding::Truncate)],
        on_size_error: Vec::new(),
        not_on_size_error: Vec::new(),
        span,
    }];

    let mut tie_branch = go_left.clone();
    for (tk, target_expr) in key_targets.iter().rev() {
        let key_item = ctx
            .layout
            .items
            .get(tk.key_item.as_u32() as usize)
            .unwrap_or(table_item);
        let mut subs = outer_subs.to_vec();
        subs.push((HirExpr::Place(primary_idx.clone()), elem_stride));
        let key_place = HirPlace {
            item: key_item.id,
            static_offset: key_item.byte_offset,
            subscripts: subs,
            refmod: None,
            effective_ty: key_item.ty.clone(),
            check_mask: ExceptionCheckMask::NONE,
            span,
        };
        let (right_op, left_op) = match tk.direction {
            HirSortDirection::Ascending => (HirRelOp::Lt, HirRelOp::Gt),
            HirSortDirection::Descending => (HirRelOp::Gt, HirRelOp::Lt),
        };
        let inner_gt = HirStmt::If {
            cond: HirCondition::Compare {
                op: left_op,
                lhs: HirExpr::Place(key_place.clone()),
                rhs: target_expr.clone(),
            },
            then_body: go_left.clone(),
            else_body: tie_branch,
            span,
        };
        tie_branch = vec![HirStmt::If {
            cond: HirCondition::Compare {
                op: right_op,
                lhs: HirExpr::Place(key_place),
                rhs: target_expr.clone(),
            },
            then_body: go_right.clone(),
            else_body: vec![inner_gt],
            span,
        }];
    }
    tie_branch
}

/// Extracts leading `(HirTableKey, target_hir_expr)` equality constraints from a `SEARCH ALL` `WHEN` condition.
///
/// # Arguments
///
/// * `declared_keys` - `OCCURS` `ASCENDING`/`DESCENDING KEY` list.
/// * `cond` - AST `WHEN` condition.
/// * `ctx` - Unit lowering context.
fn match_search_all_keys(
    declared_keys: &[HirTableKey],
    cond: &Condition,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<(HirTableKey, HirExpr)> {
    let mut eq_pairs = Vec::new();
    collect_search_eq_conjuncts(declared_keys, cond, &mut eq_pairs, ctx);
    let mut matched = Vec::new();
    for tk in declared_keys {
        if let Some((_, rhs)) = eq_pairs.iter().find(|(kid, _)| *kid == tk.key_item) {
            matched.push((*tk, rhs.clone()));
        } else {
            break;
        }
    }
    matched
}

/// Recursively collects `(key_item_id, target_expr)` pairs from `AND`-connected `WHEN` equality conjuncts.
///
/// # Arguments
///
/// * `declared_keys` - Declared `OCCURS` keys on the searched table.
/// * `cond` - AST condition node.
/// * `out` - Output `(DataItemId, HirExpr)` list.
/// * `ctx` - Unit lowering context.
fn collect_search_eq_conjuncts(
    declared_keys: &[HirTableKey],
    cond: &Condition,
    out: &mut Vec<(DataItemId, HirExpr)>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    match cond {
        Condition::And { lhs, rhs, .. } => {
            collect_search_eq_conjuncts(declared_keys, lhs, out, ctx);
            collect_search_eq_conjuncts(declared_keys, rhs, out, ctx);
        }
        Condition::Relation {
            subject,
            op: RelOp::Eq,
            rhs:
                AbbrevRhs::Leaf {
                    negated: NegationMode::Positive,
                    op: None | Some(RelOp::Eq),
                    object,
                    ..
                },
            ..
        } => {
            let lhs_id = try_resolve_data_item_silent(subject, ctx);
            let rhs_id = try_resolve_data_item_silent(object, ctx);
            if let Some(kid) = lhs_id
                && declared_keys.iter().any(|tk| tk.key_item == kid)
            {
                let (rh, _) = lower_expr_with_cat(object, ctx);
                out.push((kid, rh));
            } else if let Some(kid) = rhs_id
                && declared_keys.iter().any(|tk| tk.key_item == kid)
            {
                let (lh, _) = lower_expr_with_cat(subject, ctx);
                out.push((kid, lh));
            }
        }
        Condition::ConditionRef(dref) => {
            if let Some(c88) = resolve_symbol_silent(dref.name.base, &dref.name.qualifiers, ctx)
                .and_then(|sym| match sym {
                    ResolvedSymbol::Condition88(idx) => ctx.layout.conditions_88.get(idx),
                    _ => None,
                })
                && let [(lo, None)] = c88.ranges.as_slice()
            {
                out.push((
                    c88.parent_item,
                    HirExpr::Const {
                        value: lo.clone(),
                        span: dref.span,
                    },
                ));
            }
        }
        _ => {}
    }
}

/// Quietly checks whether `expr` is `Expr::Data(dref)` resolving to a [`DataItemId`].
///
/// # Arguments
///
/// * `expr` - Candidate AST expression.
/// * `ctx` - Unit lowering context.
#[must_use]
fn try_resolve_data_item_silent(expr: &Expr, ctx: &UnitLowerCtx<'_>) -> Option<DataItemId> {
    let Expr::Data(dref) = expr else {
        return None;
    };
    match resolve_symbol_silent(dref.name.base, &dref.name.qualifiers, ctx)? {
        ResolvedSymbol::DataItem(id) => Some(id),
        _ => None,
    }
}

/// Quietly looks up `(base, qualifiers)` in `ctx.symtab` without emitting diagnostics.
///
/// # Arguments
///
/// * `base` - Base identifier symbol.
/// * `qualifiers` - Ancestor qualifier symbols.
/// * `ctx` - Unit lowering context.
#[must_use]
fn resolve_symbol_silent(
    base: SymbolId,
    qualifiers: &[SymbolId],
    ctx: &UnitLowerCtx<'_>,
) -> Option<ResolvedSymbol> {
    let mut cur = Some(ctx.scope);
    while let Some(sc) = cur.and_then(|s| ctx.symtab.scope(s)) {
        if let Some(cands) = sc.by_name.get(&base) {
            for cand in cands {
                if qualifiers_subsequence(qualifiers, &cand.qualifiers) {
                    return Some(cand.target.clone());
                }
            }
        }
        cur = sc.parent_scope;
    }
    None
}

/// Checks whether `given` qualifiers appear in order inside `declared` qualifiers.
///
/// # Arguments
///
/// * `given` - User-written qualifiers.
/// * `declared` - Item's ancestor qualifier chain.
#[must_use]
fn qualifiers_subsequence(given: &[SymbolId], declared: &[SymbolId]) -> bool {
    let mut rem = declared;
    for q in given {
        let Some(pos) = rem.iter().position(|d| d == q) else {
            return false;
        };
        rem = rem.get(pos + 1..).unwrap_or(&[]);
    }
    true
}

/// Lowers `Stmt::SortTable` into `HirStmt::SortTable`.
///
/// # Arguments
///
/// * `table` - Sorted table reference.
/// * `keys` - Explicit `(KeyOrder, Vec<DataRef>)` clauses.
/// * `collating_sequence` - Optional `[COLLATING] SEQUENCE` symbol.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_sort_table_stmt(
    table: &DataRef,
    keys: &[(KeyOrder, Vec<DataRef>)],
    collating_sequence: Option<SymbolId>,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(ResolvedTableTarget {
        table_item,
        outer_subs,
        own_dim,
    }) = resolve_table_target(table, ctx)
    else {
        return;
    };
    let stride = own_dim.element_byte_stride.max(1);
    let active_mask =
        compute_active_check_mask(span, ctx.dialect.bounds_checks, ctx.turn_directives);
    let table_place = HirPlace {
        item: table_item.id,
        static_offset: table_item.byte_offset,
        subscripts: outer_subs,
        refmod: None,
        effective_ty: table_item.ty.clone(),
        check_mask: active_mask,
        span: table.span,
    };

    let sort_keys = build_sort_table_keys(&table_item, &own_dim, stride, keys, span, ctx);
    let collating_ranks = resolve_sort_collating_ranks(collating_sequence, ctx);

    out.push(HirStmt::SortTable(Box::new(HirSortTableSpec {
        table: table_place,
        count_odo: own_dim.depending_on,
        max_count: own_dim.max,
        stride,
        keys: sort_keys,
        collating_ranks,
        span,
    })));
}

/// Builds the ordered [`HirSortTableKey`] vector for `SORT table`.
///
/// # Arguments
///
/// * `table_item` - Resolved table [`HirDataItem`].
/// * `own_dim` - Own [`HirOccursDim`] of the sorted table.
/// * `stride` - Element byte stride.
/// * `stmt_keys` - Explicit AST key clauses on `SORT`.
/// * `span` - Statement span.
/// * `ctx` - Unit lowering context.
fn build_sort_table_keys(
    table_item: &HirDataItem,
    own_dim: &HirOccursDim,
    stride: u32,
    stmt_keys: &[(KeyOrder, Vec<DataRef>)],
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<HirSortTableKey> {
    let base_off = table_item.byte_offset.as_u32();
    let whole_elem = |direction: HirSortDirection| HirSortTableKey {
        direction,
        rel_offset: 0,
        byte_len: stride,
        ty: table_item.ty.clone(),
    };
    let item_to_key = |direction: HirSortDirection, key_id: DataItemId| {
        let (rel_offset, byte_len, ty) = ctx.layout.items.get(key_id.as_u32() as usize).map_or(
            (0, stride, table_item.ty.clone()),
            |kit| {
                (
                    kit.byte_offset.as_u32().saturating_sub(base_off),
                    kit.ty.static_byte_size().max(1),
                    kit.ty.clone(),
                )
            },
        );
        HirSortTableKey {
            direction,
            rel_offset,
            byte_len,
            ty,
        }
    };

    if stmt_keys.is_empty() {
        if own_dim.keys.is_empty() {
            return vec![whole_elem(HirSortDirection::Ascending)];
        }
        return own_dim
            .keys
            .iter()
            .map(|tk| item_to_key(tk.direction, tk.key_item))
            .collect();
    }

    let mut out_keys = Vec::new();
    for (order, key_refs) in stmt_keys {
        let direction = match order {
            KeyOrder::Ascending => HirSortDirection::Ascending,
            KeyOrder::Descending => HirSortDirection::Descending,
        };
        if key_refs.is_empty() {
            if own_dim.keys.is_empty() {
                out_keys.push(whole_elem(direction));
            } else {
                for tk in &own_dim.keys {
                    out_keys.push(item_to_key(direction, tk.key_item));
                }
            }
            continue;
        }
        for kref in key_refs {
            if let Some(ResolvedSymbol::DataItem(kid)) = ctx.symtab.resolve_symbol(
                ctx.scope,
                kref.name.base,
                &kref.name.qualifiers,
                kref.span,
                ctx.interner,
                ctx.sink,
            ) {
                out_keys.push(item_to_key(direction, kid));
            } else {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(span),
                    String::from("`SORT` key must name a data item subordinate to the table"),
                ));
            }
        }
    }
    out_keys
}

/// Resolves the optional 256-byte collation rank table for `SORT table`.
///
/// # Arguments
///
/// * `explicit_seq` - Explicit `[COLLATING] SEQUENCE` symbol on `SORT`, if any.
/// * `ctx` - Unit lowering context.
#[must_use]
fn resolve_sort_collating_ranks(
    explicit_seq: Option<SymbolId>,
    ctx: &UnitLowerCtx<'_>,
) -> Option<Vec<u8>> {
    if let Some(cs_sym) = explicit_seq {
        if let Some(alpha) = ctx.alphabets.iter().find(|a| a.name == cs_sym) {
            return Some(alpha.ranks.clone());
        }
        let canon = ctx.interner.resolve_canonical(cs_sym).unwrap_or("");
        return match canon {
            "EBCDIC" => Some(ebcdic_collation_ranks()),
            _ => None,
        };
    }
    ctx.prog_collating_seq
        .and_then(|idx| ctx.alphabets.get(idx))
        .map(|a| a.ranks.clone())
}

/// Resolved table item, outer dimension subscripts, and own `OCCURS` descriptor.
struct ResolvedTableTarget {
    /// Resolved table [`HirDataItem`].
    table_item: HirDataItem,
    /// Lowered `(expr, byte_stride)` pairs for enclosing outer dimensions.
    outer_subs: Vec<(HirExpr, u32)>,
    /// Own [`HirOccursDim`] of the searched or sorted table.
    own_dim: HirOccursDim,
}

/// Resolves a table [`DataRef`] for `SEARCH` / `SORT`, returning [`ResolvedTableTarget`].
///
/// # Arguments
///
/// * `dref` - AST table reference.
/// * `ctx` - Unit lowering context.
fn resolve_table_target(dref: &DataRef, ctx: &mut UnitLowerCtx<'_>) -> Option<ResolvedTableTarget> {
    let res = ctx.symtab.resolve_symbol(
        ctx.scope,
        dref.name.base,
        &dref.name.qualifiers,
        dref.span,
        ctx.interner,
        ctx.sink,
    )?;
    let ResolvedSymbol::DataItem(item_id) = res else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(dref.span),
            String::from("`SEARCH`/`SORT` target must be a table data item"),
        ));
        return None;
    };
    let table_item = ctx.layout.items.get(item_id.as_u32() as usize)?.clone();
    let Some(own_dim) = table_item.dimensions.last().cloned() else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(dref.span),
            String::from("`SEARCH`/`SORT` target must have an `OCCURS` clause"),
        ));
        return None;
    };
    let outer_dims = table_item.dimensions.len().saturating_sub(1);
    let mut outer_subs = Vec::with_capacity(outer_dims);
    for (i, sub) in dref.subscripts.iter().take(outer_dims).enumerate() {
        let stride = table_item
            .dimensions
            .get(i)
            .map_or(1, |d| d.element_byte_stride);
        let expr = match sub {
            SubscriptItem::Expr(e) => lower_expr_with_cat(e, ctx).0,
            SubscriptItem::AllWildcard(sp) => const_int_expr("1", *sp, ctx),
        };
        outer_subs.push((expr, stride));
    }
    Some(ResolvedTableTarget {
        table_item,
        outer_subs,
        own_dim,
    })
}

/// Builds the active element count [`HirExpr`] (`depending_on` place or static `max` constant).
///
/// # Arguments
///
/// * `own_dim` - Table [`HirOccursDim`].
/// * `span` - Statement span.
/// * `ctx` - Unit lowering context.
fn build_table_count_expr(
    own_dim: &HirOccursDim,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirExpr {
    if let Some(odo_item) = own_dim
        .depending_on
        .and_then(|odo_id| ctx.layout.items.get(odo_id.as_u32() as usize))
    {
        HirExpr::Place(simple_place_for_item(odo_item, span))
    } else {
        const_u32_expr(own_dim.max, span, ctx)
    }
}

/// Interns `val` and returns a decimal integer [`HirExpr::Const`].
///
/// # Arguments
///
/// * `val` - Unsigned integer literal value.
/// * `span` - Source span.
/// * `ctx` - Unit lowering context.
fn const_u32_expr(val: u32, span: SourceSpan, ctx: &mut UnitLowerCtx<'_>) -> HirExpr {
    let fallback = ctx.builtins_get("1");
    let text = ctx.interner.intern(&val.to_string()).unwrap_or(fallback);
    HirExpr::Const {
        value: HirConstValue::Decimal {
            text,
            scale: ScaleFactor::ZERO,
            sign: HirDecimalSign::UnsignedOrEmbedded,
        },
        span,
    }
}

/// Builds `target = target + step` as an unrounded [`HirStmt::AssignArith`].
///
/// # Arguments
///
/// * `target` - Target counter/index place.
/// * `step` - Step expression (`1`).
/// * `span` - Statement span.
#[must_use]
fn make_incr_stmt(target: &HirPlace, step: &HirExpr, span: SourceSpan) -> HirStmt {
    HirStmt::AssignArith {
        expr: HirExpr::Binary {
            op: HirBinOp::Add,
            lhs: Box::new(HirExpr::Place(target.clone())),
            rhs: Box::new(step.clone()),
            span,
        },
        targets: vec![(target.clone(), HirRounding::Truncate)],
        on_size_error: Vec::new(),
        not_on_size_error: Vec::new(),
        span,
    }
}
