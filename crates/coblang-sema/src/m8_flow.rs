//! Milestone M8 control-flow and `SET` / `EVALUATE` / `PERFORM` semantic desugaring helpers.

use coblang_common::{
    ByteOffset, DataItemId, DigitCount, LevelNumber, ProcedureId, ScaleFactor, SourceSpan,
};
use coblang_hir::{
    BinaryEndianness, Comp6Mode, DataCategory, ExceptionCheckMask, HirBinOp, HirCondition,
    HirConstValue, HirDataItem, HirDecimalSign, HirEnvTargetKind, HirExpr, HirFigurative,
    HirFlowExitKind, HirLoopKind, HirLoopTest, HirPlace, HirRelOp, HirRounding, HirStmt,
    HirStorageSection, HirTruthValue, HirType, JustifiedSpec, Signedness, TruncationPolicy,
};
use coblang_syntax::ast::{
    DataRef, DataRefModifier, EvaluateObject, EvaluateSubject, EvaluateWhenBranch, Expr,
    NegationMode, PerformControl, PerformTestPosition, PerformVaryingClause, QualifiedName,
    Sentence, SetAction, StepDirection, Stmt, SubscriptItem, TruthValue,
};

use crate::lower_hir::{
    UnitLowerCtx, apply_polarity, expand_condition_ref, is_condition_symbol_silent,
    lower_condition, lower_expr_with_cat, lower_stmt_slice, resolve_place,
};
use crate::symbols::ResolvedSymbol;
use crate::typecheck::{EditedTargetPolicy, validate_arith_category, validate_set_action};

/// Returns `true` if any statement inside `stmts` (stopping at nested sentence boundaries, which cannot occur)
/// contains an explicit `NEXT SENTENCE` transfer.
///
/// # Arguments
///
/// * `stmts` - AST statement slice of a period-terminated `Sentence`.
#[must_use]
pub(crate) fn stmts_contain_next_sentence(stmts: &[Stmt]) -> bool {
    stmts.iter().any(stmt_contains_next_sentence)
}

/// Recursively checks a single AST [`Stmt`] for any reachable `Stmt::NextSentence`.
///
/// # Arguments
///
/// * `stmt` - AST statement to inspect.
#[must_use]
fn stmt_contains_next_sentence(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::NextSentence { .. } => true,
        Stmt::If {
            then_body,
            else_body,
            ..
        } => stmts_contain_next_sentence(then_body) || stmts_contain_next_sentence(else_body),
        Stmt::PerformInline { body, .. } => stmts_contain_next_sentence(body),
        Stmt::Evaluate {
            branches,
            when_other,
            ..
        } => {
            branches
                .iter()
                .any(|b| stmts_contain_next_sentence(&b.body))
                || stmts_contain_next_sentence(when_other)
        }
        Stmt::Search {
            at_end, when_arms, ..
        } => {
            stmts_contain_next_sentence(at_end)
                || when_arms
                    .iter()
                    .any(|(_, b)| stmts_contain_next_sentence(b))
        }
        Stmt::Add { size_error, .. }
        | Stmt::Subtract { size_error, .. }
        | Stmt::Multiply { size_error, .. }
        | Stmt::Divide { size_error, .. }
        | Stmt::Compute { size_error, .. } => {
            stmts_contain_next_sentence(&size_error.on_size_error)
                || stmts_contain_next_sentence(&size_error.not_on_size_error)
        }
        Stmt::Accept { handlers, .. }
        | Stmt::Call { handlers, .. }
        | Stmt::Delete { handlers, .. }
        | Stmt::Display { handlers, .. }
        | Stmt::JsonGenerate { handlers, .. }
        | Stmt::JsonParse { handlers, .. }
        | Stmt::Read { handlers, .. }
        | Stmt::Return { handlers, .. }
        | Stmt::Rewrite { handlers, .. }
        | Stmt::Start { handlers, .. }
        | Stmt::StringVerb { handlers, .. }
        | Stmt::Unstring { handlers, .. }
        | Stmt::Write { handlers, .. }
        | Stmt::XmlGenerate { handlers, .. }
        | Stmt::XmlParse { handlers, .. } => {
            stmts_contain_next_sentence(&handlers.on_event)
                || stmts_contain_next_sentence(&handlers.not_on_event)
        }
        _ => false,
    }
}

/// Lowers a slice of AST [`Sentence`]s, wrapping sentences that use `NEXT SENTENCE` in [`HirStmt::Sentence`].
///
/// # Arguments
///
/// * `sentences` - AST sentence slice.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_sentence_list(
    sentences: &[Sentence],
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<HirStmt> {
    let mut out = Vec::new();
    for sent in sentences {
        if stmts_contain_next_sentence(&sent.statements) {
            let mut body = Vec::new();
            lower_stmt_slice(&sent.statements, &mut body, ctx);
            out.push(HirStmt::Sentence {
                body,
                span: sent.span,
            });
        } else {
            lower_stmt_slice(&sent.statements, &mut out, ctx);
        }
    }
    out
}

/// Allocates an unnamed compiler-internal `WORKING-STORAGE` temporary item and returns its [`HirPlace`].
///
/// # Arguments
///
/// * `ty` - Resolved [`HirType`] for the temporary item.
/// * `span` - Originating statement span.
/// * `ctx` - Unit lowering context.
#[must_use]
pub(crate) fn alloc_temp_place(
    ty: HirType,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirPlace {
    let id = DataItemId::new(u32::try_from(ctx.layout.items.len()).unwrap_or(0));
    let total_byte_size = ty.static_byte_size().max(1);
    ctx.layout.items.push(HirDataItem {
        id,
        scope: ctx.scope,
        name: None,
        level: LevelNumber::Independent77,
        parent: None,
        children: Vec::new(),
        section: HirStorageSection::WorkingStorage,
        ty: ty.clone(),
        byte_offset: ByteOffset::ZERO,
        total_byte_size,
        dimensions: Vec::new(),
        redefines: None,
        initial_value: None,
        span,
    });
    HirPlace {
        item: id,
        static_offset: ByteOffset::ZERO,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: ty,
        check_mask: ExceptionCheckMask::NONE,
        span,
    }
}

/// Synthesizes an 8-byte native signed integer temporary [`HirPlace`] in `WORKING-STORAGE`.
///
/// # Arguments
///
/// * `span` - Originating statement span.
/// * `ctx` - Unit lowering context.
#[must_use]
pub(crate) fn alloc_i64_counter_place(span: SourceSpan, ctx: &mut UnitLowerCtx<'_>) -> HirPlace {
    let digits = DigitCount::try_new(18).unwrap_or(DigitCount::ONE);
    alloc_temp_place(
        HirType::BinaryInt {
            byte_width: 8,
            digits,
            scale: ScaleFactor::ZERO,
            sign: Signedness::Signed,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
        },
        span,
        ctx,
    )
}

/// Builds a decimal integer constant [`HirExpr`] (`"0"` or `"1"`).
///
/// # Arguments
///
/// * `key` - Pre-interned integer key (`"0"` or `"1"`).
/// * `span` - Source span.
/// * `ctx` - Unit lowering context.
#[must_use]
pub(crate) fn const_int_expr(
    key: &'static str,
    span: SourceSpan,
    ctx: &UnitLowerCtx<'_>,
) -> HirExpr {
    HirExpr::Const {
        value: HirConstValue::Decimal {
            text: ctx.builtins_get(key),
            scale: ScaleFactor::ZERO,
            sign: HirDecimalSign::UnsignedOrEmbedded,
        },
        span,
    }
}

/// Lowers `PERFORM` (`Once`, `Times`, `Until`, and `Varying ... After ...`) around `core_body`.
///
/// # Arguments
///
/// * `control` - AST [`PerformControl`] specification.
/// * `core_body` - Lowered loop body statements.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn emit_perform_control_loop(
    control: &PerformControl,
    core_body: Vec<HirStmt>,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    match control {
        PerformControl::Once => {
            out.extend(core_body);
        }
        PerformControl::Times(count_ast) => {
            let (count_expr, cat) = lower_expr_with_cat(count_ast, ctx);
            validate_arith_category(
                cat,
                EditedTargetPolicy::StrictNumeric,
                count_ast.span(),
                ctx.sink,
            );
            let cnt_place = alloc_i64_counter_place(span, ctx);
            out.push(HirStmt::AssignArith {
                targets: vec![(cnt_place.clone(), HirRounding::Truncate)],
                expr: count_expr,
                on_size_error: Vec::new(),
                not_on_size_error: Vec::new(),
                span,
            });
            let zero = const_int_expr("0", span, ctx);
            let one = const_int_expr("1", span, ctx);
            let dec_step = HirStmt::AssignArith {
                targets: vec![(cnt_place.clone(), HirRounding::Truncate)],
                expr: HirExpr::Binary {
                    op: HirBinOp::Sub,
                    lhs: Box::new(HirExpr::Place(cnt_place.clone())),
                    rhs: Box::new(one),
                    span,
                },
                on_size_error: Vec::new(),
                not_on_size_error: Vec::new(),
                span,
            };
            out.push(HirStmt::Loop {
                kind: HirLoopKind::UserPerform,
                test_before: HirLoopTest::Before,
                until: HirCondition::Compare {
                    op: HirRelOp::Le,
                    lhs: HirExpr::Place(cnt_place),
                    rhs: zero,
                },
                body: core_body,
                cycle_step: vec![dec_step],
                span,
            });
        }
        PerformControl::Until {
            test_pos,
            condition,
        } => {
            let test_before = map_test_pos(*test_pos);
            let until = lower_condition(condition, ctx);
            out.push(HirStmt::Loop {
                kind: HirLoopKind::UserPerform,
                test_before,
                until,
                body: core_body,
                cycle_step: Vec::new(),
                span,
            });
        }
        PerformControl::Varying { test_pos, clauses } => {
            emit_perform_varying(*test_pos, clauses, core_body, span, out, ctx);
        }
    }
}

/// Maps AST [`PerformTestPosition`] to [`HirLoopTest`].
///
/// # Arguments
///
/// * `pos` - AST `Before` or `After` test position.
#[must_use]
const fn map_test_pos(pos: PerformTestPosition) -> HirLoopTest {
    match pos {
        PerformTestPosition::Before => HirLoopTest::Before,
        PerformTestPosition::After => HirLoopTest::After,
    }
}

/// Resolved counter, initial expression, step expression, and termination condition for one `VARYING`/`AFTER` tier.
struct LoweredVaryingTier {
    /// Validated counter place.
    counter: HirPlace,
    /// Initial `FROM` expression.
    from_expr: HirExpr,
    /// Step `BY` expression.
    by_expr: HirExpr,
    /// `UNTIL` termination condition.
    until: HirCondition,
}

/// Lowers 1..N `PERFORM VARYING ... AFTER ...` tiers per ANSI X3.23-1985 Figures 1 (`Before`) & 2 (`After`).
///
/// # Arguments
///
/// * `test_pos` - `Before` vs `After` loop test mode.
/// * `clauses` - Ordered `VARYING` + `AFTER` clauses.
/// * `core_body` - User loop body statements.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn emit_perform_varying(
    test_pos: PerformTestPosition,
    clauses: &[PerformVaryingClause],
    core_body: Vec<HirStmt>,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut tiers = Vec::with_capacity(clauses.len());
    for vc in clauses {
        let (from_expr, _) = lower_expr_with_cat(&vc.from, ctx);
        let (by_expr, _) = lower_expr_with_cat(&vc.by, ctx);
        let until = lower_condition(&vc.until, ctx);
        if let Some(counter) = resolve_place(&vc.counter, ctx) {
            tiers.push(LoweredVaryingTier {
                counter,
                from_expr,
                by_expr,
                until,
            });
        }
    }

    let Some((first, rest)) = tiers.split_first() else {
        out.extend(core_body);
        return;
    };

    match test_pos {
        PerformTestPosition::Before => {
            // ANSI-85 Figure 1 (`TEST BEFORE`): initialize ALL counters v_1..v_k in order up front.
            for tier in &tiers {
                out.push(make_counter_init_stmt(tier, span));
            }
            out.push(build_varying_before_tier(
                first,
                rest,
                HirLoopKind::UserPerform,
                core_body,
                span,
            ));
        }
        PerformTestPosition::After => {
            // ANSI-85 Figure 2 (`TEST AFTER`): initialize v_1 up front; each outer tier initializes v_{i+1} at top.
            out.push(make_counter_init_stmt(first, span));
            out.push(build_varying_after_tier(
                first,
                rest,
                HirLoopKind::UserPerform,
                core_body,
                span,
            ));
        }
    }
}

/// Builds recursive `TEST BEFORE` nested [`HirStmt::Loop`] for `(cur, rest)`.
///
/// # Arguments
///
/// * `cur` - Current `VARYING` or `AFTER` tier.
/// * `rest` - Remaining inner `AFTER` tiers.
/// * `kind` - `UserPerform` for outermost tier, `InternalAfterTier` for inner tiers.
/// * `core_body` - Innermost user statements.
/// * `span` - Statement span.
#[must_use]
fn build_varying_before_tier(
    cur: &LoweredVaryingTier,
    rest: &[LoweredVaryingTier],
    kind: HirLoopKind,
    core_body: Vec<HirStmt>,
    span: SourceSpan,
) -> HirStmt {
    let cycle_step = vec![make_counter_step_stmt(cur, span)];
    let body = if let Some((next_tier, tail)) = rest.split_first() {
        vec![
            build_varying_before_tier(
                next_tier,
                tail,
                HirLoopKind::InternalAfterTier,
                core_body,
                span,
            ),
            make_counter_init_stmt(next_tier, span),
        ]
    } else {
        core_body
    };
    HirStmt::Loop {
        kind,
        test_before: HirLoopTest::Before,
        until: cur.until.clone(),
        body,
        cycle_step,
        span,
    }
}

/// Builds recursive `TEST AFTER` nested [`HirStmt::Loop`] for `(cur, rest)`.
///
/// # Arguments
///
/// * `cur` - Current `VARYING` or `AFTER` tier.
/// * `rest` - Remaining inner `AFTER` tiers.
/// * `kind` - `UserPerform` for outermost tier, `InternalAfterTier` for inner tiers.
/// * `core_body` - Innermost user statements.
/// * `span` - Statement span.
#[must_use]
fn build_varying_after_tier(
    cur: &LoweredVaryingTier,
    rest: &[LoweredVaryingTier],
    kind: HirLoopKind,
    core_body: Vec<HirStmt>,
    span: SourceSpan,
) -> HirStmt {
    let cycle_step = vec![make_counter_step_stmt(cur, span)];
    let body = if let Some((next_tier, tail)) = rest.split_first() {
        vec![
            make_counter_init_stmt(next_tier, span),
            build_varying_after_tier(
                next_tier,
                tail,
                HirLoopKind::InternalAfterTier,
                core_body,
                span,
            ),
        ]
    } else {
        core_body
    };
    HirStmt::Loop {
        kind,
        test_before: HirLoopTest::After,
        until: cur.until.clone(),
        body,
        cycle_step,
        span,
    }
}

/// Creates `counter = from_expr` initialization [`HirStmt`] for a `VARYING` tier.
///
/// # Arguments
///
/// * `tier` - Lowered varying tier.
/// * `span` - Statement span.
#[must_use]
fn make_counter_init_stmt(tier: &LoweredVaryingTier, span: SourceSpan) -> HirStmt {
    HirStmt::AssignArith {
        targets: vec![(tier.counter.clone(), HirRounding::Truncate)],
        expr: tier.from_expr.clone(),
        on_size_error: Vec::new(),
        not_on_size_error: Vec::new(),
        span,
    }
}

/// Creates `counter = counter + by_expr` augmentation [`HirStmt`] for a `VARYING` tier.
///
/// # Arguments
///
/// * `tier` - Lowered varying tier.
/// * `span` - Statement span.
#[must_use]
fn make_counter_step_stmt(tier: &LoweredVaryingTier, span: SourceSpan) -> HirStmt {
    let dst = tier.counter.clone();
    let next_expr = HirExpr::Binary {
        op: HirBinOp::Add,
        lhs: Box::new(HirExpr::Place(dst.clone())),
        rhs: Box::new(tier.by_expr.clone()),
        span,
    };
    HirStmt::AssignArith {
        targets: vec![(dst, HirRounding::Truncate)],
        expr: next_expr,
        on_size_error: Vec::new(),
        not_on_size_error: Vec::new(),
        span,
    }
}

/// Prepared `EVALUATE` selection subject (either boolean truth value or comparable expression).
enum LoweredEvalSubject {
    /// Constant `EVALUATE TRUE` or `EVALUATE FALSE`.
    ConstBool(HirTruthValue),
    /// Runtime boolean condition stored in a 1-byte `0`/`1` flag place.
    RuntimeBool(HirPlace),
    /// Value expression (snapshotted into a temporary place if computed).
    Value(HirExpr),
}

/// Desugars multi-subject `EVALUATE ... ALSO ... WHEN ... THRU ... WHEN OTHER` into nested [`HirStmt::If`].
///
/// # Arguments
///
/// * `subjects` - AST selection subjects.
/// * `branches` - AST `WHEN` branches.
/// * `when_other` - AST `WHEN OTHER` fallback statements.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_evaluate_stmt(
    subjects: &[EvaluateSubject],
    branches: &[EvaluateWhenBranch],
    when_other: &[Stmt],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut lowered_subjects = Vec::with_capacity(subjects.len());
    for subj in subjects {
        lowered_subjects.push(prepare_evaluate_subject(subj, span, out, ctx));
    }

    let mut fallback = Vec::new();
    lower_stmt_slice(when_other, &mut fallback, ctx);

    for branch in branches.iter().rev() {
        let mut branch_cond: Option<HirCondition> = None;
        for tuple in &branch.alternatives {
            let tup_cond = build_when_tuple_condition(&lowered_subjects, tuple, span, ctx);
            branch_cond = Some(match branch_cond {
                None => tup_cond,
                Some(prev) => HirCondition::Or(Box::new(prev), Box::new(tup_cond)),
            });
        }
        let mut then_body = Vec::new();
        lower_stmt_slice(&branch.body, &mut then_body, ctx);
        let cond = branch_cond.unwrap_or(HirCondition::BoolConst(HirTruthValue::False));
        fallback = vec![HirStmt::If {
            cond,
            then_body,
            else_body: fallback,
            span,
        }];
    }

    out.extend(fallback);
}

/// Evaluates and snapshots a single `EVALUATE` selection subject.
///
/// # Arguments
///
/// * `subj` - AST selection subject.
/// * `span` - Enclosing `EVALUATE` span.
/// * `out` - Pre-`IF` setup statement accumulator.
/// * `ctx` - Unit lowering context.
fn prepare_evaluate_subject(
    subj: &EvaluateSubject,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) -> LoweredEvalSubject {
    match subj {
        EvaluateSubject::Condition(c) => {
            let hc = lower_condition(c, ctx);
            snapshot_bool_subject(hc, span, out, ctx)
        }
        EvaluateSubject::Expr(Expr::Data(dref)) if is_condition_symbol_silent(dref, ctx) => {
            let hc = expand_condition_ref(dref, ctx);
            snapshot_bool_subject(hc, span, out, ctx)
        }
        EvaluateSubject::Expr(expr) => {
            let (h_expr, cat) = lower_expr_with_cat(expr, ctx);
            if matches!(
                h_expr,
                HirExpr::Binary { .. }
                    | HirExpr::Neg(_)
                    | HirExpr::CallFunction { .. }
                    | HirExpr::M12Call(_)
            ) {
                let tmp_ty = if cat == DataCategory::Numeric {
                    let d38 = DigitCount::try_new(38).unwrap_or(DigitCount::ONE);
                    let s18 = ScaleFactor::try_new(18).unwrap_or(ScaleFactor::ZERO);
                    HirType::PackedDecimal {
                        digits: d38,
                        scale: s18,
                        sign: Signedness::Signed,
                        unsigned_comp6: Comp6Mode::StandardComp3,
                    }
                } else {
                    HirType::Alphanumeric {
                        byte_len: 256,
                        justified: JustifiedSpec::Left,
                    }
                };
                let tmp = alloc_temp_place(tmp_ty, span, ctx);
                if cat == DataCategory::Numeric {
                    out.push(HirStmt::AssignArith {
                        targets: vec![(tmp.clone(), HirRounding::Truncate)],
                        expr: h_expr,
                        on_size_error: Vec::new(),
                        not_on_size_error: Vec::new(),
                        span,
                    });
                } else {
                    out.push(HirStmt::Move {
                        src: h_expr,
                        dst: tmp.clone(),
                        span,
                    });
                }
                LoweredEvalSubject::Value(HirExpr::Place(tmp))
            } else {
                LoweredEvalSubject::Value(h_expr)
            }
        }
    }
}

/// Snapshots a boolean `EVALUATE` subject into `ConstBool` or a 1-byte `0`/`1` flag place.
///
/// # Arguments
///
/// * `hc` - Lowered boolean condition.
/// * `span` - Enclosing `EVALUATE` span.
/// * `out` - Pre-`IF` setup statement accumulator.
/// * `ctx` - Unit lowering context.
fn snapshot_bool_subject(
    hc: HirCondition,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) -> LoweredEvalSubject {
    if let HirCondition::BoolConst(tv) = hc {
        return LoweredEvalSubject::ConstBool(tv);
    }
    let d1 = DigitCount::ONE;
    let flag_pl = alloc_temp_place(
        HirType::BinaryInt {
            byte_width: 1,
            digits: d1,
            scale: ScaleFactor::ZERO,
            sign: Signedness::Unsigned,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
        },
        span,
        ctx,
    );
    let zero = const_int_expr("0", span, ctx);
    let one = const_int_expr("1", span, ctx);
    out.push(HirStmt::If {
        cond: hc,
        then_body: vec![HirStmt::AssignArith {
            targets: vec![(flag_pl.clone(), HirRounding::Truncate)],
            expr: one,
            on_size_error: Vec::new(),
            not_on_size_error: Vec::new(),
            span,
        }],
        else_body: vec![HirStmt::AssignArith {
            targets: vec![(flag_pl.clone(), HirRounding::Truncate)],
            expr: zero,
            on_size_error: Vec::new(),
            not_on_size_error: Vec::new(),
            span,
        }],
        span,
    });
    LoweredEvalSubject::RuntimeBool(flag_pl)
}

/// Builds the conjunction across all `(subject_j, object_j)` columns of one `WHEN` tuple.
///
/// # Arguments
///
/// * `subjects` - Prepared selection subjects.
/// * `tuple` - `WHEN` object tuple.
/// * `span` - Enclosing `EVALUATE` span.
/// * `ctx` - Unit lowering context.
fn build_when_tuple_condition(
    subjects: &[LoweredEvalSubject],
    tuple: &[EvaluateObject],
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirCondition {
    let mut acc: Option<HirCondition> = None;
    for (subj, obj) in subjects.iter().zip(tuple) {
        let col_cond = match_subject_against_object(subj, obj, span, ctx);
        acc = Some(match acc {
            None => col_cond,
            Some(prev) => HirCondition::And(Box::new(prev), Box::new(col_cond)),
        });
    }
    acc.unwrap_or(HirCondition::BoolConst(HirTruthValue::True))
}

/// Normalized `WHEN` selection object with `WHEN [NOT] cond-88` promoted to `Condition`.
enum NormalizedWhenObject<'a> {
    /// `WHEN ANY` wildcard.
    Any,
    /// Boolean condition (`TRUE`, `FALSE`, relational/class/sign, or level-`88` condition-name).
    Condition(Box<HirCondition>),
    /// Value or `low THRU high` range comparison.
    Range {
        /// Optional `NOT` prefix.
        negated: NegationMode,
        /// Lower bound (or exact value) expression.
        low: &'a Expr,
        /// Optional `THRU` upper bound expression.
        high: Option<&'a Expr>,
    },
}

/// Builds the comparison condition between a single `LoweredEvalSubject` and `EvaluateObject`.
///
/// # Arguments
///
/// * `subj` - Prepared selection subject.
/// * `obj` - AST `WHEN` object.
/// * `span` - Enclosing `EVALUATE` span.
/// * `ctx` - Unit lowering context.
fn match_subject_against_object(
    subj: &LoweredEvalSubject,
    obj: &EvaluateObject,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirCondition {
    let norm_obj = match obj {
        EvaluateObject::Any => NormalizedWhenObject::Any,
        EvaluateObject::Condition(c) => {
            NormalizedWhenObject::Condition(Box::new(lower_condition(c, ctx)))
        }
        EvaluateObject::Range {
            negated,
            low: Expr::Data(dref),
            high: None,
        } if is_condition_symbol_silent(dref, ctx) => {
            let c88 = expand_condition_ref(dref, ctx);
            NormalizedWhenObject::Condition(Box::new(apply_polarity(c88, *negated)))
        }
        EvaluateObject::Range { negated, low, high } => NormalizedWhenObject::Range {
            negated: *negated,
            low,
            high: high.as_ref(),
        },
    };

    match norm_obj {
        NormalizedWhenObject::Any => HirCondition::BoolConst(HirTruthValue::True),
        NormalizedWhenObject::Condition(oc) => match subj {
            LoweredEvalSubject::ConstBool(tv) => match tv {
                HirTruthValue::True => *oc,
                HirTruthValue::False => HirCondition::Not(oc),
            },
            LoweredEvalSubject::Value(_) => *oc,
            LoweredEvalSubject::RuntimeBool(flag_pl) => {
                let is_one = HirCondition::Compare {
                    op: HirRelOp::Eq,
                    lhs: HirExpr::Place(flag_pl.clone()),
                    rhs: const_int_expr("1", span, ctx),
                };
                let is_zero = HirCondition::Compare {
                    op: HirRelOp::Eq,
                    lhs: HirExpr::Place(flag_pl.clone()),
                    rhs: const_int_expr("0", span, ctx),
                };
                match *oc {
                    HirCondition::BoolConst(HirTruthValue::True) => is_one,
                    HirCondition::BoolConst(HirTruthValue::False) => is_zero,
                    other => HirCondition::Or(
                        Box::new(HirCondition::And(Box::new(is_one), Box::new(other.clone()))),
                        Box::new(HirCondition::And(
                            Box::new(is_zero),
                            Box::new(HirCondition::Not(Box::new(other))),
                        )),
                    ),
                }
            }
        },
        NormalizedWhenObject::Range { negated, low, high } => match subj {
            LoweredEvalSubject::Value(subj_expr) => {
                let (lo_h, _) = lower_expr_with_cat(low, ctx);
                let base_cmp = if let Some(hi_ast) = high {
                    let (hi_h, _) = lower_expr_with_cat(hi_ast, ctx);
                    HirCondition::And(
                        Box::new(HirCondition::Compare {
                            op: HirRelOp::Ge,
                            lhs: subj_expr.clone(),
                            rhs: lo_h,
                        }),
                        Box::new(HirCondition::Compare {
                            op: HirRelOp::Le,
                            lhs: subj_expr.clone(),
                            rhs: hi_h,
                        }),
                    )
                } else {
                    HirCondition::Compare {
                        op: HirRelOp::Eq,
                        lhs: subj_expr.clone(),
                        rhs: lo_h,
                    }
                };
                apply_polarity(base_cmp, negated)
            }
            LoweredEvalSubject::ConstBool(_) | LoweredEvalSubject::RuntimeBool(_) => {
                let _ = lower_expr_with_cat(low, ctx);
                if let Some(hi_ast) = high {
                    let _ = lower_expr_with_cat(hi_ast, ctx);
                }
                HirCondition::BoolConst(HirTruthValue::False)
            }
        },
    }
}

/// Lowers all sub-operations of a `SET` statement (`88` `TRUE`/`FALSE`, switches, and `INDEX` arithmetic).
///
/// # Arguments
///
/// * `actions` - AST `SET` sub-operations.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_set_actions(
    actions: &[SetAction],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    for act in actions {
        validate_set_action(
            act,
            ctx.scope,
            &ctx.layout.items,
            ctx.symtab,
            ctx.interner,
            ctx.sink,
        );
        lower_single_set_action(act, span, out, ctx);
    }
}

/// Lowers `SetAction::ToValue` (`SET idx/ptr/ADDRESS OF/obj TO value`).
///
/// # Arguments
///
/// * `targets` - Target [`DataRef`] slice.
/// * `value` - Right-hand [`Expr`].
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_set_to_value_action(
    targets: &[DataRef],
    value: &Expr,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let (val_hir, val_cat) = lower_expr_with_cat(value, ctx);
    let any_obj_target = targets.iter().any(|t| {
        t.modifier != DataRefModifier::AddressOf
            && ctx
                .symtab
                .resolve_symbol_silent(ctx.scope, t.name.base, &t.name.qualifiers)
                .and_then(|s| match s {
                    ResolvedSymbol::DataItem(id) => ctx.layout.items.get(id.as_u32() as usize),
                    _ => None,
                })
                .is_some_and(|it| matches!(it.ty, HirType::ObjectRef { .. }))
    });
    if val_cat == DataCategory::ObjectReference || any_obj_target {
        for t in targets {
            if let Some(dst_pl) = resolve_place(t, ctx) {
                out.push(HirStmt::M20(Box::new(coblang_hir::HirM20Stmt::OoSetRef(
                    Box::new((dst_pl, val_hir.clone(), span)),
                ))));
            }
        }
        return;
    }
    let is_ptr_rhs = val_cat == DataCategory::Pointer
        || matches!(
            val_hir,
            HirExpr::AddressOf(_)
                | HirExpr::Const {
                    value: HirConstValue::Figurative(HirFigurative::Null),
                    ..
                }
        );
    let any_ptr_target = targets.iter().any(|t| {
        t.modifier == DataRefModifier::AddressOf
            || ctx
                .symtab
                .resolve_symbol_silent(ctx.scope, t.name.base, &t.name.qualifiers)
                .and_then(|s| match s {
                    ResolvedSymbol::DataItem(id) => ctx.layout.items.get(id.as_u32() as usize),
                    _ => None,
                })
                .is_some_and(|it| matches!(it.ty, HirType::Pointer | HirType::ProgramPointer))
    });
    if is_ptr_rhs || any_ptr_target {
        crate::m11_pointers::lower_set_pointer_to_value(targets, val_hir, span, out, ctx);
        return;
    }
    for target in targets {
        if let Some(dst_pl) = resolve_place(target, ctx) {
            out.push(HirStmt::AssignArith {
                targets: vec![(dst_pl, HirRounding::Truncate)],
                expr: val_hir.clone(),
                on_size_error: Vec::new(),
                not_on_size_error: Vec::new(),
                span,
            });
        }
    }
}

/// Lowers one validated [`SetAction`] into [`HirStmt`]s (or `HighLevelOp("SET")` for M20 object refs).
///
/// # Arguments
///
/// * `act` - Single `SET` sub-operation.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_single_set_action(
    act: &SetAction,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    match act {
        SetAction::ConditionState { targets, state } => {
            for target in targets {
                lower_set_condition_88(target, *state, span, out, ctx);
            }
        }
        SetAction::SwitchState { switch_name, state } => {
            if let Some(sw) = ctx
                .switches
                .iter()
                .find(|s| s.mnemonic == Some(*switch_name))
            {
                let h_state = match state {
                    TruthValue::True => HirTruthValue::True,
                    TruthValue::False => HirTruthValue::False,
                };
                out.push(HirStmt::SetSwitch {
                    switch_index: sw.index,
                    state: h_state,
                    span,
                });
            }
        }
        SetAction::ToEntry { targets, entry } => {
            crate::m11_pointers::lower_set_to_entry(targets, entry, span, out, ctx);
        }
        SetAction::ToValue { targets, value } => {
            lower_set_to_value_action(targets, value, span, out, ctx);
        }
        SetAction::StepIndex {
            targets,
            direction,
            amount,
        } => {
            lower_set_step_index(targets, *direction, amount, span, out, ctx);
        }
        SetAction::LastExceptionOff => {
            out.push(HirStmt::SetLastExceptionOff { span });
        }
        SetAction::Environment { name, value } => {
            let (n_expr, _) = lower_expr_with_cat(name, ctx);
            let (v_expr, _) = lower_expr_with_cat(value, ctx);
            out.push(HirStmt::DisplayEnv {
                operands: vec![n_expr],
                target: HirEnvTargetKind::EnvironmentName,
                span,
            });
            out.push(HirStmt::DisplayEnv {
                operands: vec![v_expr],
                target: HirEnvTargetKind::EnvironmentValue,
                span,
            });
        }
        SetAction::ScreenAttributes { target, toggles } => {
            let screen_cat = ctx.screen_catalog.clone();
            crate::m19_screen::lower_m19_set_screen_attributes(
                target,
                toggles,
                span,
                &screen_cat,
                out,
                ctx,
            );
        }
    }
}

/// Lowers `SET target ... UP BY / DOWN BY amount` for index items and pointers.
///
/// # Arguments
///
/// * `targets` - Target index/pointer `DataRef`s.
/// * `direction` - `UP BY` or `DOWN BY` step direction.
/// * `amount` - Step delta expression.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_set_step_index(
    targets: &[DataRef],
    direction: StepDirection,
    amount: &Expr,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let (amt_hir, _) = lower_expr_with_cat(amount, ctx);
    let op = match direction {
        StepDirection::Up => HirBinOp::Add,
        StepDirection::Down => HirBinOp::Sub,
    };
    for target in targets {
        if target.modifier == DataRefModifier::AddressOf {
            crate::m11_pointers::lower_set_pointer_step(
                target,
                direction,
                amt_hir.clone(),
                span,
                out,
                ctx,
            );
            continue;
        }
        if let Some(dst_pl) = resolve_place(target, ctx) {
            if matches!(
                dst_pl.effective_ty,
                HirType::Pointer | HirType::ProgramPointer
            ) {
                crate::m11_pointers::lower_set_pointer_step(
                    target,
                    direction,
                    amt_hir.clone(),
                    span,
                    out,
                    ctx,
                );
                continue;
            }
            let expr = HirExpr::Binary {
                op,
                lhs: Box::new(HirExpr::Place(dst_pl.clone())),
                rhs: Box::new(amt_hir.clone()),
                span,
            };
            out.push(HirStmt::AssignArith {
                targets: vec![(dst_pl, HirRounding::Truncate)],
                expr,
                on_size_error: Vec::new(),
                not_on_size_error: Vec::new(),
                span,
            });
        }
    }
}

/// Lowers `SET cond-88 TO TRUE / FALSE` into a [`HirStmt::Move`] onto the parent conditional variable.
///
/// # Arguments
///
/// * `target` - Condition-88 `DataRef` (potentially carrying subscripts!).
/// * `state` - Desired `TRUE` or `FALSE` state.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_set_condition_88(
    target: &DataRef,
    state: TruthValue,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(ResolvedSymbol::Condition88(idx)) = ctx.symtab.resolve_symbol(
        ctx.scope,
        target.name.base,
        &target.name.qualifiers,
        target.span,
        ctx.interner,
        ctx.sink,
    ) else {
        return;
    };
    let Some((parent_id, chosen_val)) = ctx.layout.conditions_88.get(idx).and_then(|c88| {
        let val = match state {
            TruthValue::True => c88.ranges.first().map(|(lo, _)| lo.clone()),
            TruthValue::False => c88.false_value.clone(),
        }?;
        Some((c88.parent_item, val))
    }) else {
        return;
    };
    let dst_place = build_parent_place_with_subscripts(parent_id, target, ctx);
    out.push(HirStmt::Move {
        src: HirExpr::Const {
            value: chosen_val,
            span,
        },
        dst: dst_place,
        span,
    });
}

/// Constructs the [`HirPlace`] for a level-`88`'s parent item, inheriting any subscripts written on `cond_ref`.
///
/// # Arguments
///
/// * `parent_id` - Parent conditional variable [`DataItemId`].
/// * `cond_ref` - Level-`88` reference (possibly subscripted).
/// * `ctx` - Unit lowering context.
#[must_use]
pub(crate) fn build_parent_place_with_subscripts(
    parent_id: DataItemId,
    cond_ref: &DataRef,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirPlace {
    let (byte_offset, dimensions, ty) = ctx.layout.items.get(parent_id.as_u32() as usize).map_or(
        (
            ByteOffset::new(0),
            Vec::new(),
            HirType::Alphanumeric {
                byte_len: 1,
                justified: JustifiedSpec::Left,
            },
        ),
        |it| (it.byte_offset, it.dimensions.clone(), it.ty.clone()),
    );
    let subscripts = cond_ref
        .subscripts
        .iter()
        .enumerate()
        .map(|(i, sub)| {
            let stride = dimensions.get(i).map_or(1, |d| d.element_byte_stride);
            let expr = match sub {
                SubscriptItem::Expr(e) => lower_expr_with_cat(e, ctx).0,
                SubscriptItem::AllWildcard(sp) => const_int_expr("1", *sp, ctx),
            };
            (expr, stride)
        })
        .collect();
    HirPlace {
        item: parent_id,
        static_offset: byte_offset,
        subscripts,
        refmod: None,
        effective_ty: ty,
        check_mask: ExceptionCheckMask::NONE,
        span: cond_ref.span,
    }
}

/// Resolves an out-of-line `PERFORM` `(start_proc, end_proc)` pair, expanding `SECTION` targets to `section_end`.
///
/// # Arguments
///
/// * `start` - Start procedure name.
/// * `thru` - Optional `THRU` procedure name.
/// * `ctx` - Unit lowering context.
#[must_use]
pub(crate) fn resolve_perform_proc_bounds(
    start: &QualifiedName,
    thru: Option<&QualifiedName>,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<(ProcedureId, ProcedureId)> {
    let start_p = ctx.symtab.resolve_procedure(
        ctx.scope,
        start.base,
        start.qualifiers.first().copied(),
        start.span,
        ctx.interner,
        ctx.sink,
    )?;
    crate::m13_exceptions::validate_declarative_proc_ref(
        ctx.decl_region,
        crate::m13_exceptions::ProcRefTransferKind::Perform,
        start_p,
        start.base,
        ctx.normal_proc_cnt,
        start.span,
        ctx,
    );
    let raw_end = if let Some(t) = thru {
        let ep = ctx.symtab.resolve_procedure(
            ctx.scope,
            t.base,
            t.qualifiers.first().copied(),
            t.span,
            ctx.interner,
            ctx.sink,
        )?;
        crate::m13_exceptions::validate_declarative_proc_ref(
            ctx.decl_region,
            crate::m13_exceptions::ProcRefTransferKind::Perform,
            ep,
            t.base,
            ctx.normal_proc_cnt,
            t.span,
            ctx,
        );
        ep
    } else {
        start_p
    };
    let expanded_end = ctx
        .section_ends
        .get(raw_end.as_u32() as usize)
        .copied()
        .unwrap_or(raw_end);
    Some((start_p, expanded_end))
}

/// Resolves `ALTER` `(from_proc, to_proc)` pairs into [`ProcedureId`] tuples.
///
/// # Arguments
///
/// * `pairs` - AST `(from_proc, to_proc)` pairs.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_alter_stmt(
    pairs: &[(QualifiedName, QualifiedName)],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut resolved = Vec::with_capacity(pairs.len());
    for (p1, p2) in pairs {
        let r1 = ctx.symtab.resolve_procedure(
            ctx.scope,
            p1.base,
            p1.qualifiers.first().copied(),
            p1.span,
            ctx.interner,
            ctx.sink,
        );
        let r2 = ctx.symtab.resolve_procedure(
            ctx.scope,
            p2.base,
            p2.qualifiers.first().copied(),
            p2.span,
            ctx.interner,
            ctx.sink,
        );
        if let (Some(id1), Some(id2)) = (r1, r2) {
            crate::m13_exceptions::validate_declarative_proc_ref(
                ctx.decl_region,
                crate::m13_exceptions::ProcRefTransferKind::GoToOrAlter,
                id1,
                p1.base,
                ctx.normal_proc_cnt,
                p1.span,
                ctx,
            );
            crate::m13_exceptions::validate_declarative_proc_ref(
                ctx.decl_region,
                crate::m13_exceptions::ProcRefTransferKind::GoToOrAlter,
                id2,
                p2.base,
                ctx.normal_proc_cnt,
                p2.span,
                ctx,
            );
            resolved.push((id1, id2));
        }
    }
    out.push(HirStmt::Alter {
        pairs: resolved,
        span,
    });
}

/// Maps non-return `EXIT` variants (`Paragraph`, `Section`, `PerformBreak`, `PerformCycle`) to [`HirStmt::FlowExit`].
///
/// # Arguments
///
/// * `kind` - [`HirFlowExitKind`] variant.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
pub(crate) fn emit_flow_exit(kind: HirFlowExitKind, span: SourceSpan, out: &mut Vec<HirStmt>) {
    out.push(HirStmt::FlowExit { kind, span });
}
