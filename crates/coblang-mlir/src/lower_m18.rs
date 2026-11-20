//! Milestone M18 (`REPORT SECTION`, `INITIATE`, `GENERATE`, `TERMINATE`, `SUPPRESS`, and
//! `USE BEFORE REPORTING`) HIR-to-`cobol` MLIR lowering.

#![deny(clippy::indexing_slicing)]

use coblang_common::{DataItemId, DigitCount, Result, ScaleFactor, SourceSpan};
use coblang_hir::{
    DataCategory, ExceptionCheckMask, HirGroupIndicateMode, HirInternalCharset, HirM18Stmt,
    HirPlace, HirProgramFormatting, HirReportCodeSpec, HirReportColAlign, HirReportColSpec,
    HirReportControlSpec, HirReportElemPlan, HirReportFormatterKind, HirReportGroupPlan,
    HirReportGroupRole, HirReportLineSpec, HirReportNextGroupSpec, HirReportNodePlan,
    HirReportOccursSpec, HirReportPlan, HirReportPresentKind, HirReportSourceKind,
    HirReportSubGroupPlan, HirType,
};
use coblang_rt_core::{desc_codec::encode_field_spec, field_ops::RuntimeMoveSourceKind};
use melior::dialect::arith::CmpiPredicate;
use melior::dialect::{arith, llvm, scf};
use melior::ir::attribute::{IntegerAttribute, TypeAttribute};
use melior::ir::block::BlockLike;
use melior::ir::r#type::IntegerType;
use melior::ir::{Block, Location, Region, RegionLike, Type, Value};

use crate::lower_from_hir::{
    ModuleLowerState, ProgramLowerCtx, append_val, emit_const_slice, emit_decimal_expr, emit_i32,
    emit_i64, emit_rt_call, emit_spec_slice, eval_expr_as_i64, lower_condition,
    lower_move_source_args, lower_place_view,
};
use crate::lower_m14::emit_null_ptr;
use crate::ops::{CobolLoopOp, CobolPerformRangeOp, CobolYieldOp, RtDispatchTarget, llvm_ptr_type};
use crate::storage_init::make_runtime_field_spec;

/// Pass selector for walking a [`HirReportGroupPlan`] tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupWalkMode {
    /// Pass A: evaluate `PRESENT WHEN` and collect visible `LINE` specs for pagination.
    CollectLineSpecs,
    /// Pass B: evaluate `PRESENT WHEN`, update `LINE-COUNTER`, format items, and emit columns.
    RenderItems,
}

/// Enclosing `OCCURS` byte-offset and horizontal `STEP` accumulators threaded down a report subtree.
#[derive(Clone, Copy)]
struct SubtreeOccursEnv<'ctx, 'b> {
    /// Byte offset added to writable report-section `HirPlace` addresses (`0` outside `OCCURS`).
    byte_offset_i64: Value<'ctx, 'b>,
    /// Additional column offset added to `COLUMN` coordinates from `OCCURS ... STEP n` (`0` outside `STEP`).
    col_step_i32: Value<'ctx, 'b>,
    /// Active `OCCURS ... STEP n` slot width (`0` outside `STEP`).
    active_step_cols: u32,
    /// `i1` flag true when rendering occurrence index `> 0` of the innermost `OCCURS`.
    in_occurs_after_first: Value<'ctx, 'b>,
    /// Root `DataItemId` of the enclosing synthesized `01` report group (`u32::MAX` if none).
    report_root_id: u32,
}

/// Dispatches [`HirM18Stmt`] statements into `cobol` MLIR operations.
///
/// # Arguments
///
/// * `stmt` - Lowered M18 report statement.
/// * `block` - Target MLIR insertion block.
/// * `prog_ctx` - Active program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
pub(crate) fn lower_m18_stmt<'ctx, 'b, 'e: 'b>(
    stmt: &HirM18Stmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    match stmt {
        HirM18Stmt::Initiate { reports, span, .. } => {
            let loc = state.loc(*span)?;
            let site = format_site_string(*span, state);
            let (site_ptr, site_len) =
                emit_const_slice(site.as_bytes(), block, prog_ctx, state, loc)?;
            for (rid, _) in reports {
                if let Some(plan) = prog_ctx
                    .prog
                    .report_plans
                    .iter()
                    .find(|p| p.report_id == *rid)
                {
                    emit_register_report_schema(plan, block, prog_ctx, state, loc)?;
                    let rid_v = emit_i32(
                        i32::try_from(rid.as_u32()).unwrap_or(0),
                        block,
                        prog_ctx,
                        state,
                        loc,
                    );
                    let rc = emit_rt_call(
                        RtDispatchTarget::ReportInitiate,
                        &[rid_v, site_ptr, site_len],
                        block,
                        prog_ctx,
                        state,
                        loc,
                    )?;
                    let z0 = emit_i32(0, block, prog_ctx, state, loc);
                    let is_ok = append_val(
                        block,
                        arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, rc, z0, loc).into(),
                        z0,
                    );
                    let then_reg = Region::new();
                    let else_reg = Region::new();
                    let tb = then_reg.append_block(Block::new(&[]));
                    let one = emit_i32(1, &tb, prog_ctx, state, loc);
                    let zero = emit_i32(0, &tb, prog_ctx, state, loc);
                    store_i32_to_data_item(
                        plan.page_counter_item,
                        one,
                        *span,
                        &tb,
                        prog_ctx,
                        state,
                    )?;
                    store_i32_to_data_item(
                        plan.line_counter_item,
                        zero,
                        *span,
                        &tb,
                        prog_ctx,
                        state,
                    )?;
                    store_i32_to_data_item(
                        plan.print_switch_item,
                        zero,
                        *span,
                        &tb,
                        prog_ctx,
                        state,
                    )?;
                    for grp in &plan.groups {
                        if let Some((root_id, root_sz)) = grp.backing_root_item {
                            emit_clear_group_backing_to_spaces(
                                root_id, root_sz, *span, &tb, prog_ctx, state, loc,
                            )?;
                        }
                    }
                    emit_sync_named_sum_backing_items(plan, *span, &tb, prog_ctx, state)?;
                    let _ = tb.append_operation(scf::r#yield(&[], loc));
                    let eb = else_reg.append_block(Block::new(&[]));
                    let _ = eb.append_operation(scf::r#yield(&[], loc));
                    let _ = block.append_operation(scf::r#if(is_ok, &[], then_reg, else_reg, loc));
                }
            }
            Ok(())
        }
        HirM18Stmt::GenerateDetail {
            report_id,
            report_name,
            group_name,
            span,
            ..
        } => {
            let Some(plan) = prog_ctx
                .prog
                .report_plans
                .iter()
                .find(|p| p.report_id == *report_id)
            else {
                return Ok(());
            };
            let detail_idx = plan
                .groups
                .iter()
                .position(|g| {
                    g.role == HirReportGroupRole::Detail
                        && g.name.as_deref() == Some(group_name.as_str())
                })
                .and_then(|i| i32::try_from(i).ok())
                .unwrap_or(0);
            emit_generate_common(plan, report_name, detail_idx, *span, block, prog_ctx, state)
        }
        HirM18Stmt::GenerateSummary {
            report_id,
            report_name,
            span,
            ..
        } => {
            let Some(plan) = prog_ctx
                .prog
                .report_plans
                .iter()
                .find(|p| p.report_id == *report_id)
            else {
                return Ok(());
            };
            emit_generate_common(plan, report_name, -1, *span, block, prog_ctx, state)
        }
        HirM18Stmt::Terminate { reports, span, .. } => {
            let loc = state.loc(*span)?;
            let site = format_site_string(*span, state);
            let (site_ptr, site_len) =
                emit_const_slice(site.as_bytes(), block, prog_ctx, state, loc)?;
            for (rid, rep_name) in reports {
                let Some(plan) = prog_ctx
                    .prog
                    .report_plans
                    .iter()
                    .find(|p| p.report_id == *rid)
                else {
                    continue;
                };
                emit_sync_dynamic_env_and_controls(plan, *span, block, prog_ctx, state)?;
                let rid_v = emit_i32(
                    i32::try_from(rid.as_u32()).unwrap_or(0),
                    block,
                    prog_ctx,
                    state,
                    loc,
                );
                let (rn_ptr, rn_len) =
                    emit_const_slice(rep_name.as_bytes(), block, prog_ctx, state, loc)?;
                let rc = emit_rt_call(
                    RtDispatchTarget::ReportBeginTerminate,
                    &[rid_v, rn_ptr, rn_len, site_ptr, site_len],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
                let z0 = emit_i32(0, block, prog_ctx, state, loc);
                let ok_cond = append_val(
                    block,
                    arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, rc, z0, loc).into(),
                    z0,
                );
                let then_reg = Region::new();
                let else_reg = Region::new();
                let tb = then_reg.append_block(Block::new(&[]));
                emit_report_stepper_loop(plan, *span, &tb, prog_ctx, state)?;
                let _ = tb.append_operation(scf::r#yield(&[], loc));
                let eb = else_reg.append_block(Block::new(&[]));
                let _ = eb.append_operation(scf::r#yield(&[], loc));
                let _ = block.append_operation(scf::r#if(ok_cond, &[], then_reg, else_reg, loc));
            }
            Ok(())
        }
        HirM18Stmt::SuppressPrinting { span } => {
            let loc = state.loc(*span)?;
            let all_v = emit_i32(-1, block, prog_ctx, state, loc);
            let _ = emit_rt_call(
                RtDispatchTarget::ReportSuppress,
                &[all_v],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            Ok(())
        }
    }
}

/// Shared `GENERATE` emitter (`detail_idx >= 0` for detail, `-1` for summary).
fn emit_generate_common<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    report_name: &str,
    detail_idx: i32,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    emit_sync_dynamic_env_and_controls(plan, span, block, prog_ctx, state)?;
    let rid_v = emit_i32(
        i32::try_from(plan.report_id.as_u32()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let (rn_ptr, rn_len) = emit_const_slice(report_name.as_bytes(), block, prog_ctx, state, loc)?;
    let det_v = emit_i32(detail_idx, block, prog_ctx, state, loc);
    let site = format_site_string(span, state);
    let (site_ptr, site_len) = emit_const_slice(site.as_bytes(), block, prog_ctx, state, loc)?;
    let rc = emit_rt_call(
        RtDispatchTarget::ReportBeginGenerate,
        &[rid_v, rn_ptr, rn_len, det_v, site_ptr, site_len],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let z0 = emit_i32(0, block, prog_ctx, state, loc);
    let ok_cond = append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, rc, z0, loc).into(),
        z0,
    );
    let then_reg = Region::new();
    let else_reg = Region::new();
    let tb = then_reg.append_block(Block::new(&[]));
    emit_report_stepper_loop(plan, span, &tb, prog_ctx, state)?;
    let _ = tb.append_operation(scf::r#yield(&[], loc));
    let eb = else_reg.append_block(Block::new(&[]));
    let _ = eb.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(ok_cond, &[], then_reg, else_reg, loc));
    Ok(())
}

/// Registers static schema, groups, and `SUM` counter links for `plan` prior to `ReportInitiate`.
fn emit_register_report_schema<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let rid_v = emit_i32(
        i32::try_from(plan.report_id.as_u32()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let (rn_ptr, rn_len) =
        emit_const_slice(plan.report_name.as_bytes(), block, prog_ctx, state, loc)?;
    let (fn_ptr, fn_len) =
        emit_const_slice(plan.file_name.as_bytes(), block, prog_ctx, state, loc)?;
    let (as_ptr, as_len) =
        emit_const_slice(plan.assign_target.as_bytes(), block, prog_ctx, state, loc)?;

    let pl_v = emit_i64(
        plan.page_limits.line_limit.map_or(-1, i64::from),
        block,
        prog_ctx,
        state,
        loc,
    );
    let hd_v = emit_i64(
        plan.page_limits.heading.map_or(-1, i64::from),
        block,
        prog_ctx,
        state,
        loc,
    );
    let fd_v = emit_i64(
        plan.page_limits.first_detail.map_or(-1, i64::from),
        block,
        prog_ctx,
        state,
        loc,
    );
    let ld_v = emit_i64(
        plan.page_limits.last_detail.map_or(-1, i64::from),
        block,
        prog_ctx,
        state,
        loc,
    );
    let ft_v = emit_i64(
        plan.page_limits.footing.map_or(-1, i64::from),
        block,
        prog_ctx,
        state,
        loc,
    );
    let ctrl_cnt = emit_i32(
        i32::try_from(plan.controls.len()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let sum_cnt = emit_i32(
        i32::try_from(plan.sum_counters.len()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let grp_cnt = emit_i32(
        i32::try_from(plan.groups.len()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let _ = emit_rt_call(
        RtDispatchTarget::ReportInitSchema,
        &[
            rid_v, rn_ptr, rn_len, fn_ptr, fn_len, as_ptr, as_len, pl_v, hd_v, fd_v, ld_v, ft_v,
            ctrl_cnt, sum_cnt, grp_cnt,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    for (g_idx, grp) in plan.groups.iter().enumerate() {
        let g_v = emit_i32(
            i32::try_from(g_idx).unwrap_or(0),
            block,
            prog_ctx,
            state,
            loc,
        );
        let g_nm = grp.name.as_deref().unwrap_or("");
        let (gn_ptr, gn_len) = emit_const_slice(g_nm.as_bytes(), block, prog_ctx, state, loc)?;
        let (role_code, c_idx) = match grp.role {
            HirReportGroupRole::ReportHeading => (0_i32, 0_i32),
            HirReportGroupRole::PageHeading => (1, 0),
            HirReportGroupRole::ControlHeading(ci) => (2, i32::from(ci)),
            HirReportGroupRole::Detail => (3, 0),
            HirReportGroupRole::ControlFooting(ci) => (4, i32::from(ci)),
            HirReportGroupRole::PageFooting => (5, 0),
            HirReportGroupRole::ReportFooting => (6, 0),
        };
        let (ng_kind, ng_val) = match grp.next_group {
            None => (0_i32, 0_i32),
            Some(HirReportNextGroupSpec::Absolute(n)) => (1, i32::try_from(n).unwrap_or(0)),
            Some(HirReportNextGroupSpec::Relative(n)) => (2, i32::try_from(n).unwrap_or(0)),
            Some(HirReportNextGroupSpec::NextPage) => (3, 0),
        };
        let has_hook = grp
            .name
            .as_deref()
            .is_some_and(|nm| plan.hooks.iter().any(|h| h.group_name == nm));
        let role_v = emit_i32(role_code, block, prog_ctx, state, loc);
        let ci_v = emit_i32(c_idx, block, prog_ctx, state, loc);
        let ngk_v = emit_i32(ng_kind, block, prog_ctx, state, loc);
        let ngv_v = emit_i32(ng_val, block, prog_ctx, state, loc);
        let hk_v = emit_i32(i32::from(has_hook), block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::ReportConfigGroup,
            &[rid_v, g_v, gn_ptr, gn_len, role_v, ci_v, ngk_v, ngv_v, hk_v],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }

    for (s_idx, sc) in plan.sum_counters.iter().enumerate() {
        let s_v = emit_i32(
            i32::try_from(s_idx).unwrap_or(0),
            block,
            prog_ctx,
            state,
            loc,
        );
        let oc_v = emit_i32(i32::from(sc.owner_control_idx), block, prog_ctx, state, loc);
        let rc_v = emit_i32(i32::from(sc.reset_control_idx), block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::ReportConfigSum,
            &[rid_v, s_v, oc_v, rc_v],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        let (empty_p, zero_l) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
        for &sub_idx in &sc.rolling_counters {
            let sub_v = emit_i32(
                i32::try_from(sub_idx).unwrap_or(0),
                block,
                prog_ctx,
                state,
                loc,
            );
            let _ = emit_rt_call(
                RtDispatchTarget::ReportConfigSumLink,
                &[rid_v, s_v, sub_v, empty_p, zero_l],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        let neg1 = emit_i32(-1, block, prog_ctx, state, loc);
        for upon_nm in &sc.upon_details {
            let (up_p, up_l) = emit_const_slice(upon_nm.as_bytes(), block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::ReportConfigSumLink,
                &[rid_v, s_v, neg1, up_p, up_l],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
    }
    Ok(())
}

/// Pushes dynamic `CODE IS`, dynamic `COLUMNS`, and live control snapshots to `report_abi`.
fn emit_sync_dynamic_env_and_controls<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let rid_v = emit_i32(
        i32::try_from(plan.report_id.as_u32()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let (code_ptr, code_len) = match &plan.code_spec {
        HirReportCodeSpec::None => emit_const_slice(&[], block, prog_ctx, state, loc)?,
        HirReportCodeSpec::Literal(bytes) => emit_const_slice(bytes, block, prog_ctx, state, loc)?,
        HirReportCodeSpec::Place(pl) => {
            let v = lower_place_view(pl, block, prog_ctx, state)?;
            (v.ptr, v.len_i64)
        }
    };
    let col_lim_v = if let Some(cl_e) = &plan.page_limits.col_limit {
        eval_expr_as_i64(cl_e, block, prog_ctx, state)?
    } else {
        emit_i64(0, block, prog_ctx, state, loc)
    };
    let _ = emit_rt_call(
        RtDispatchTarget::ReportSetDynamicEnv,
        &[rid_v, code_ptr, code_len, col_lim_v],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    let null_p = emit_null_ptr(block, prog_ctx, state, loc);
    let zero_l = emit_i64(0, block, prog_ctx, state, loc);
    for (c_idx, ctrl) in plan.controls.iter().enumerate() {
        let ci_v = emit_i32(
            i32::try_from(c_idx).unwrap_or(0),
            block,
            prog_ctx,
            state,
            loc,
        );
        match ctrl {
            HirReportControlSpec::Final => {
                let _ = emit_rt_call(
                    RtDispatchTarget::ReportSnapshotControl,
                    &[rid_v, ci_v, null_p, zero_l],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            }
            HirReportControlSpec::Field { place, .. } => {
                let v = lower_place_view(place, block, prog_ctx, state)?;
                let _ = emit_rt_call(
                    RtDispatchTarget::ReportSnapshotControl,
                    &[rid_v, ci_v, v.ptr, v.len_i64],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            }
        }
    }
    Ok(())
}

/// Emits the `CobolLoopOp` stepper driving `CHECK_GATE`, `COLLECT_LINES`, `RUN_HOOK`, `RENDER_GROUP`, and `ACCUM_SUMS`.
fn emit_report_stepper_loop<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let i32_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 32).into();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let one_i64 = emit_i64(1, block, prog_ctx, state, loc);

    let grp_slot = alloc_stack_slot(block, state.ctx.raw(), ptr_ty, i32_ty, one_i64, loc);
    let pc_slot = alloc_stack_slot(block, state.ctx.raw(), ptr_ty, i32_ty, one_i64, loc);
    let lc_slot = alloc_stack_slot(block, state.ctx.raw(), ptr_ty, i32_ty, one_i64, loc);
    let action_slot = alloc_stack_slot(block, state.ctx.raw(), ptr_ty, i32_ty, one_i64, loc);

    let cond_region = Region::new();
    let cond_block = cond_region.append_block(Block::new(&[]));
    let rid_c = emit_i32(
        i32::try_from(plan.report_id.as_u32()).unwrap_or(0),
        &cond_block,
        prog_ctx,
        state,
        loc,
    );
    let act_v = emit_rt_call(
        RtDispatchTarget::ReportNextStep,
        &[rid_c, grp_slot, pc_slot, lc_slot],
        &cond_block,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = cond_block.append_operation(llvm::store(
        state.ctx.raw(),
        act_v,
        action_slot,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let cur_pc = load_i32_slot(&cond_block, state.ctx.raw(), pc_slot, i32_ty, loc);
    let cur_lc = load_i32_slot(&cond_block, state.ctx.raw(), lc_slot, i32_ty, loc);
    store_i32_to_data_item(
        plan.page_counter_item,
        cur_pc,
        span,
        &cond_block,
        prog_ctx,
        state,
    )?;
    store_i32_to_data_item(
        plan.line_counter_item,
        cur_lc,
        span,
        &cond_block,
        prog_ctx,
        state,
    )?;

    let z0 = emit_i32(0, &cond_block, prog_ctx, state, loc);
    let is_done = append_val(
        &cond_block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, act_v, z0, loc).into(),
        z0,
    );
    let yield_cond = CobolYieldOp {
        value: Some(is_done),
    }
    .build(state.ctx, loc)?;
    let _ = cond_block.append_operation(yield_cond);

    let body_region = Region::new();
    let body_block = body_region.append_block(Block::new(&[]));
    let act_b = load_i32_slot(&body_block, state.ctx.raw(), action_slot, i32_ty, loc);
    let grp_b = load_i32_slot(&body_block, state.ctx.raw(), grp_slot, i32_ty, loc);

    // Action 1: CHECK_GATE (+ inline COLLECT_LINES when visible)
    emit_action_branch(
        &body_block,
        act_b,
        1,
        state,
        prog_ctx,
        loc,
        |ab, pctx, st| {
            emit_sync_named_sum_backing_items(plan, span, ab, pctx, st)?;
            emit_check_gate_and_collect_dispatch(plan, grp_b, span, ab, pctx, st)
        },
    )?;

    // Action 3: RUN_HOOK (`USE BEFORE REPORTING`)
    emit_action_branch(
        &body_block,
        act_b,
        3,
        state,
        prog_ctx,
        loc,
        |ab, pctx, st| {
            emit_sync_named_sum_backing_items(plan, span, ab, pctx, st)?;
            emit_run_hook_dispatch(plan, grp_b, span, ab, pctx, st)
        },
    )?;

    // Action 4: RENDER_GROUP
    emit_action_branch(
        &body_block,
        act_b,
        4,
        state,
        prog_ctx,
        loc,
        |ab, pctx, st| {
            emit_sync_named_sum_backing_items(plan, span, ab, pctx, st)?;
            emit_render_group_dispatch(plan, grp_b, span, ab, pctx, st)
        },
    )?;

    // Action 5: ACCUM_SUMS
    emit_action_branch(
        &body_block,
        act_b,
        5,
        state,
        prog_ctx,
        loc,
        |ab, pctx, st| emit_accum_sums_dispatch(plan, grp_b, span, ab, pctx, st),
    )?;

    let yield_body = CobolYieldOp { value: None }.build(state.ctx, loc)?;
    let _ = body_block.append_operation(yield_body);

    let step_region = Region::new();
    let step_block = step_region.append_block(Block::new(&[]));
    let yield_step = CobolYieldOp { value: None }.build(state.ctx, loc)?;
    let _ = step_block.append_operation(yield_step);

    let loop_op = CobolLoopOp {
        loop_kind: "user_perform",
        test_pos: "before",
        cond_region,
        body_region,
        step_region,
    }
    .build(state.ctx, loc)?;
    let _ = block.append_operation(loop_op);
    Ok(())
}

/// Syncs every `SUM` counter that has a `backing_item` into its live Working-Storage `DataItemId`.
fn emit_sync_named_sum_backing_items<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let rid_v = emit_i32(
        i32::try_from(plan.report_id.as_u32()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    for (s_idx, sc) in plan.sum_counters.iter().enumerate() {
        let Some(item_id) = sc.backing_item else {
            continue;
        };
        let Some(item) = prog_ctx.prog.data_items.get(item_id.as_u32() as usize) else {
            continue;
        };
        let pl = HirPlace {
            item: item_id,
            static_offset: item.byte_offset,
            effective_ty: item.ty.clone(),
            subscripts: Vec::new(),
            refmod: None,
            check_mask: ExceptionCheckMask::NONE,
            span,
        };
        let v = lower_place_view(&pl, block, prog_ctx, state)?;
        let (sp_ptr, sp_len) = emit_spec_slice(&v.spec, block, prog_ctx, state, loc)?;
        let s_v = emit_i32(
            i32::try_from(s_idx).unwrap_or(0),
            block,
            prog_ctx,
            state,
            loc,
        );
        let _ = emit_rt_call(
            RtDispatchTarget::ReportSyncSumField,
            &[rid_v, s_v, v.ptr, v.len_i64, sp_ptr, sp_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Emits Action 1 (`CHECK_GATE` + inline `COLLECT_LINES` + `ReportFinishCollect`) per report group.
fn emit_check_gate_and_collect_dispatch<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    grp_idx_v: Value<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let rid_v = emit_i32(
        i32::try_from(plan.report_id.as_u32()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    for (g_idx, grp) in plan.groups.iter().enumerate() {
        emit_action_branch(
            block,
            grp_idx_v,
            i32::try_from(g_idx).unwrap_or(0),
            state,
            prog_ctx,
            loc,
            |gb, pctx, st| {
                let vis_i1 = eval_present_when(
                    plan.report_id.as_u32(),
                    &grp.present_when,
                    span,
                    gb,
                    pctx,
                    st,
                )?;
                let i32_ty: Type<'ctx> = IntegerType::new(st.ctx.raw(), 32).into();
                let vis_i32 = append_val(gb, arith::extui(vis_i1, i32_ty, loc).into(), grp_idx_v);
                let go = emit_rt_call(
                    RtDispatchTarget::ReportSubmitGate,
                    &[rid_v, vis_i32],
                    gb,
                    pctx,
                    st,
                    loc,
                )?;
                let z0 = emit_i32(0, gb, pctx, st, loc);
                let should_collect = append_val(
                    gb,
                    arith::cmpi(st.ctx.raw(), CmpiPredicate::Ne, go, z0, loc).into(),
                    z0,
                );
                let c_then = Region::new();
                let c_else = Region::new();
                let cb = c_then.append_block(Block::new(&[]));
                let i1_ty: Type<'ctx> = IntegerType::new(st.ctx.raw(), 1).into();
                let false_i1 = append_val(
                    &cb,
                    arith::constant(st.ctx.raw(), IntegerAttribute::new(i1_ty, 0).into(), loc)
                        .into(),
                    emit_i32(0, &cb, pctx, st, loc),
                );
                let env = SubtreeOccursEnv {
                    byte_offset_i64: emit_i64(0, &cb, pctx, st, loc),
                    col_step_i32: emit_i32(0, &cb, pctx, st, loc),
                    active_step_cols: 0,
                    in_occurs_after_first: false_i1,
                    report_root_id: grp
                        .backing_root_item
                        .map_or(u32::MAX, |(id, _)| id.as_u32()),
                };
                emit_walk_group(
                    plan,
                    grp,
                    GroupWalkMode::CollectLineSpecs,
                    env,
                    span,
                    &cb,
                    pctx,
                    st,
                )?;
                let _ = emit_rt_call(
                    RtDispatchTarget::ReportFinishCollect,
                    &[rid_v],
                    &cb,
                    pctx,
                    st,
                    loc,
                )?;
                let _ = cb.append_operation(scf::r#yield(&[], loc));
                let eb = c_else.append_block(Block::new(&[]));
                let _ = eb.append_operation(scf::r#yield(&[], loc));
                let _ = gb.append_operation(scf::r#if(should_collect, &[], c_then, c_else, loc));
                Ok(())
            },
        )?;
    }
    Ok(())
}

/// Emits Action 3 (`RUN_HOOK` via `CobolPerformRangeOp` + `ReportAfterHook`).
fn emit_run_hook_dispatch<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    grp_idx_v: Value<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let zero = emit_i32(0, block, prog_ctx, state, loc);
    store_i32_to_data_item(plan.print_switch_item, zero, span, block, prog_ctx, state)?;

    for (g_idx, grp) in plan.groups.iter().enumerate() {
        let Some(g_nm) = grp.name.as_deref() else {
            continue;
        };
        let matching_hooks: Vec<&coblang_hir::HirReportHookPlan> =
            plan.hooks.iter().filter(|h| h.group_name == g_nm).collect();
        if matching_hooks.is_empty() {
            continue;
        }
        let cond = append_val(
            block,
            arith::cmpi(
                state.ctx.raw(),
                CmpiPredicate::Eq,
                grp_idx_v,
                emit_i32(
                    i32::try_from(g_idx).unwrap_or(0),
                    block,
                    prog_ctx,
                    state,
                    loc,
                ),
                loc,
            )
            .into(),
            zero,
        );
        // Note: `CobolPerformRangeOp` must sit in a `CobolLoopOp` body or `scf.if` supported by `cfg_perform`!
        let then_reg = Region::new();
        let else_reg = Region::new();
        let tb = then_reg.append_block(Block::new(&[]));
        for hk in matching_hooks {
            let perf_op = CobolPerformRangeOp {
                start_proc: hk.start_proc.as_u32(),
                end_proc: hk.end_proc.as_u32(),
            }
            .build(state.ctx, loc)?;
            let _ = tb.append_operation(perf_op);
        }
        let _ = tb.append_operation(scf::r#yield(&[], loc));
        let eb = else_reg.append_block(Block::new(&[]));
        let _ = eb.append_operation(scf::r#yield(&[], loc));
        let _ = block.append_operation(scf::r#if(cond, &[], then_reg, else_reg, loc));
    }

    let rid_v = emit_i32(
        i32::try_from(plan.report_id.as_u32()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let new_pc = load_i32_from_data_item(plan.page_counter_item, span, block, prog_ctx, state)?;
    let new_ps = load_i32_from_data_item(plan.print_switch_item, span, block, prog_ctx, state)?;
    let _ = emit_rt_call(
        RtDispatchTarget::ReportAfterHook,
        &[rid_v, new_pc, new_ps],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Emits Action 4 (`RENDER_GROUP` + `ReportFinishRenderGroup` + space-reset of writable backing items).
fn emit_render_group_dispatch<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    grp_idx_v: Value<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let rid_v = emit_i32(
        i32::try_from(plan.report_id.as_u32()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    for (g_idx, grp) in plan.groups.iter().enumerate() {
        emit_action_branch(
            block,
            grp_idx_v,
            i32::try_from(g_idx).unwrap_or(0),
            state,
            prog_ctx,
            loc,
            |gb, pctx, st| {
                let i1_ty: Type<'ctx> = IntegerType::new(st.ctx.raw(), 1).into();
                let false_i1 = append_val(
                    gb,
                    arith::constant(st.ctx.raw(), IntegerAttribute::new(i1_ty, 0).into(), loc)
                        .into(),
                    emit_i32(0, gb, pctx, st, loc),
                );
                let env = SubtreeOccursEnv {
                    byte_offset_i64: emit_i64(0, gb, pctx, st, loc),
                    col_step_i32: emit_i32(0, gb, pctx, st, loc),
                    active_step_cols: 0,
                    in_occurs_after_first: false_i1,
                    report_root_id: grp
                        .backing_root_item
                        .map_or(u32::MAX, |(id, _)| id.as_u32()),
                };
                emit_walk_group(
                    plan,
                    grp,
                    GroupWalkMode::RenderItems,
                    env,
                    span,
                    gb,
                    pctx,
                    st,
                )?;
                let _ = emit_rt_call(
                    RtDispatchTarget::ReportFinishRenderGroup,
                    &[rid_v],
                    gb,
                    pctx,
                    st,
                    loc,
                )?;
                if let Some((root_id, root_sz)) = grp.backing_root_item {
                    emit_clear_group_backing_to_spaces(root_id, root_sz, span, gb, pctx, st, loc)?;
                    emit_sync_named_sum_backing_items(plan, span, gb, pctx, st)?;
                }
                Ok(())
            },
        )?;
    }
    Ok(())
}

/// Fills a writable `01` report group's backing buffer with active-charset `SPACE` bytes.
///
/// # Arguments
///
/// * `root_id` - Synthesized `01` report group [`DataItemId`].
/// * `root_sz` - Total byte size of the backing group.
/// * `span` - Source span for place lowering.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
fn emit_clear_group_backing_to_spaces<'ctx, 'b, 'e: 'b>(
    root_id: DataItemId,
    root_sz: u32,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    if let Some(root_item) = prog_ctx.prog.data_items.get(root_id.as_u32() as usize) {
        let pl = HirPlace {
            item: root_id,
            static_offset: root_item.byte_offset,
            effective_ty: HirType::Alphanumeric {
                byte_len: root_sz.max(1),
                justified: coblang_hir::JustifiedSpec::Left,
            },
            subscripts: Vec::new(),
            refmod: None,
            check_mask: ExceptionCheckMask::NONE,
            span,
        };
        let v = lower_place_view(&pl, block, prog_ctx, state)?;
        let sp_byte = match prog_ctx.prog.formatting.internal_charset {
            HirInternalCharset::Ascii => b' ',
            HirInternalCharset::Ebcdic => 0x40,
        };
        let spaces = vec![sp_byte; root_sz.max(1) as usize];
        let (s_ptr, s_len) = emit_const_slice(&spaces, block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::RawCopy,
            &[v.ptr, s_ptr, s_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Emits Action 5 (`ACCUM_SUMS` via `DecBegin` + `emit_decimal_expr` + `DecStore` + `ReportAddSumAscii`).
fn emit_accum_sums_dispatch<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    detail_idx_v: Value<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    if plan.sum_counters.is_empty() {
        return Ok(());
    }
    let loc = state.loc(span)?;
    let rid_v = emit_i32(
        i32::try_from(plan.report_id.as_u32()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i8_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 8).into();
    let scratch_len = emit_i64(48, block, prog_ctx, state, loc);
    let scratch_ptr = alloc_stack_slot(block, state.ctx.raw(), ptr_ty, i8_ty, scratch_len, loc);

    // Format each addend as `-9(26).9(10)` in canonical ASCII `.` form for `ReportAddSumAscii`.
    let dec_str_ty = build_signed_decimal_text_type();
    let ds_byte_len = i64::from(dec_str_ty.static_byte_size());
    let dec_spec = make_runtime_field_spec(dec_str_ty, HirProgramFormatting::default());
    let dec_spec_bytes = encode_field_spec(&dec_spec);
    let (ds_ptr, ds_len) = emit_const_slice(&dec_spec_bytes, block, prog_ctx, state, loc)?;
    let ds_len_v = emit_i64(ds_byte_len, block, prog_ctx, state, loc);
    let z0 = emit_i32(0, block, prog_ctx, state, loc);

    for (s_idx, sc) in plan.sum_counters.iter().enumerate() {
        let s_v = emit_i32(
            i32::try_from(s_idx).unwrap_or(0),
            block,
            prog_ctx,
            state,
            loc,
        );
        for addend in &sc.addend_exprs {
            let _ = emit_rt_call(RtDispatchTarget::DecBegin, &[], block, prog_ctx, state, loc)?;
            emit_decimal_expr(addend, block, prog_ctx, state)?;
            let _ = emit_rt_call(
                RtDispatchTarget::DecStore,
                &[scratch_ptr, ds_len_v, ds_ptr, ds_len, z0, z0, z0],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let _ = emit_rt_call(
                RtDispatchTarget::ReportAddSumAscii,
                &[rid_v, s_v, detail_idx_v, scratch_ptr, ds_len_v],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
    }
    emit_sync_named_sum_backing_items(plan, span, block, prog_ctx, state)?;
    Ok(())
}

/// Builds a `NumericEdited` `HirType` (`-9(26).9(10)`, 38 bytes) so `DecStore` writes exact `[-]dddd.ffff` ASCII!
fn build_signed_decimal_text_type() -> HirType {
    use coblang_hir::PicEditOp;
    let mut ops = Vec::with_capacity(38);
    ops.push(PicEditOp::FixedSignMinus);
    ops.extend(core::iter::repeat_n(PicEditOp::DigitSlot, 26));
    ops.push(PicEditOp::InsertDecimalPoint);
    ops.extend(core::iter::repeat_n(PicEditOp::DigitSlot, 10));
    HirType::NumericEdited {
        byte_len: 38,
        digits: DigitCount::try_new(36).unwrap_or(DigitCount::ONE),
        scale: ScaleFactor::try_new(10).unwrap_or(ScaleFactor::ZERO),
        sign_capability: coblang_hir::Signedness::Signed,
        blank_zero: coblang_hir::BlankZeroSpec::RetainZero,
        edit_ops: ops,
    }
}

/// Walks `grp` in either `CollectLineSpecs` or `RenderItems` mode.
#[expect(clippy::too_many_arguments, reason = "Recursive report group walker.")]
fn emit_walk_group<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    grp: &HirReportGroupPlan,
    mode: GroupWalkMode,
    env: SubtreeOccursEnv<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i32_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 32).into();
    let one_i64 = emit_i64(1, block, prog_ctx, state, loc);
    let has_open_line_slot = alloc_stack_slot(block, state.ctx.raw(), ptr_ty, i32_ty, one_i64, loc);
    let zero_i32 = emit_i32(0, block, prog_ctx, state, loc);
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        zero_i32,
        has_open_line_slot,
        loc,
        llvm::LoadStoreOptions::new(),
    ));

    if let Some(ls) = grp.line {
        emit_start_new_line(
            plan,
            ls,
            mode,
            has_open_line_slot,
            span,
            block,
            prog_ctx,
            state,
        )?;
    }

    for node in &grp.children {
        emit_walk_node(
            plan,
            node,
            mode,
            env,
            has_open_line_slot,
            span,
            block,
            prog_ctx,
            state,
        )?;
    }
    Ok(())
}

/// Walks a single [`HirReportNodePlan`] (`SubGroup` or `Elementary`), handling `PRESENT WHEN` and `OCCURS`.
#[expect(
    clippy::too_many_arguments,
    reason = "Recursive report subtree walker."
)]
fn emit_walk_node<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    node: &HirReportNodePlan,
    mode: GroupWalkMode,
    env: SubtreeOccursEnv<'ctx, 'b>,
    has_open_line_slot: Value<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    match node {
        HirReportNodePlan::SubGroup(sg) => emit_with_present_gate(
            plan,
            &sg.present_when,
            span,
            block,
            prog_ctx,
            state,
            |gb, pctx, st| {
                emit_with_optional_occurs(
                    sg.occurs.as_ref(),
                    env,
                    span,
                    gb,
                    pctx,
                    st,
                    |ob, sub_env, pctx2, st2| {
                        emit_walk_subgroup_body(
                            plan,
                            sg,
                            mode,
                            sub_env,
                            has_open_line_slot,
                            span,
                            ob,
                            pctx2,
                            st2,
                        )
                    },
                )
            },
        ),
        HirReportNodePlan::Elementary(el) => emit_with_present_gate(
            plan,
            &el.present_when,
            span,
            block,
            prog_ctx,
            state,
            |gb, pctx, st| {
                emit_with_optional_occurs(
                    el.occurs.as_ref(),
                    env,
                    span,
                    gb,
                    pctx,
                    st,
                    |ob, sub_env, pctx2, st2| {
                        emit_walk_elem_body(
                            plan,
                            el,
                            mode,
                            sub_env,
                            has_open_line_slot,
                            span,
                            ob,
                            pctx2,
                            st2,
                        )
                    },
                )
            },
        ),
    }
}

/// Emits the body of a `HirReportSubGroupPlan` for a single occurrence.
#[expect(
    clippy::too_many_arguments,
    reason = "Recursive report subgroup body emitter."
)]
fn emit_walk_subgroup_body<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    sg: &HirReportSubGroupPlan,
    mode: GroupWalkMode,
    env: SubtreeOccursEnv<'ctx, 'b>,
    has_open_line_slot: Value<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    if let Some(ls) = sg.line {
        emit_start_new_line(
            plan,
            ls,
            mode,
            has_open_line_slot,
            span,
            block,
            prog_ctx,
            state,
        )?;
    }
    for ch in &sg.children {
        emit_walk_node(
            plan,
            ch,
            mode,
            env,
            has_open_line_slot,
            span,
            block,
            prog_ctx,
            state,
        )?;
    }
    Ok(())
}

/// Emits a single elementary report item (including multi-column `COLUMN 1, 11, 21` lists).
#[expect(clippy::too_many_arguments, reason = "Elementary report item emitter.")]
fn emit_walk_elem_body<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    el: &HirReportElemPlan,
    mode: GroupWalkMode,
    env: SubtreeOccursEnv<'ctx, 'b>,
    has_open_line_slot: Value<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    if let Some(ls) = el.line {
        emit_start_new_line(
            plan,
            ls,
            mode,
            has_open_line_slot,
            span,
            block,
            prog_ctx,
            state,
        )?;
    } else if !el.columns.is_empty() || !el.sources.is_empty() {
        // Ensure at least an implicit `LINE PLUS 1` is opened if no ancestor opened a line!
        let i32_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 32).into();
        let opened = load_i32_slot(block, state.ctx.raw(), has_open_line_slot, i32_ty, loc);
        let z0 = emit_i32(0, block, prog_ctx, state, loc);
        let not_open = append_val(
            block,
            arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, opened, z0, loc).into(),
            z0,
        );
        let then_reg = Region::new();
        let else_reg = Region::new();
        let tb = then_reg.append_block(Block::new(&[]));
        emit_start_new_line(
            plan,
            HirReportLineSpec::Relative(1),
            mode,
            has_open_line_slot,
            span,
            &tb,
            prog_ctx,
            state,
        )?;
        let _ = tb.append_operation(scf::r#yield(&[], loc));
        let eb = else_reg.append_block(Block::new(&[]));
        let _ = eb.append_operation(scf::r#yield(&[], loc));
        let _ = block.append_operation(scf::r#if(not_open, &[], then_reg, else_reg, loc));
    }

    if mode == GroupWalkMode::CollectLineSpecs || el.columns.is_empty() || el.sources.is_empty() {
        return Ok(());
    }

    let rid_v = emit_i32(
        i32::try_from(plan.report_id.as_u32()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let gi_v = emit_i32(
        i32::from(el.group_indicate == HirGroupIndicateMode::GroupIndicate),
        block,
        prog_ctx,
        state,
        loc,
    );

    let count = el.columns.len().max(1);
    for col_i in 0..count {
        let Some(&col_spec) = el.columns.get(col_i).or_else(|| el.columns.first()) else {
            continue;
        };
        let Some(src_kind) = el.sources.get(col_i).or_else(|| el.sources.last()) else {
            continue;
        };
        let mut step_env = env;
        if count > 1
            && el.sources.len() == 1
            && matches!(src_kind, HirReportSourceKind::Place(pl) if is_inside_report_root(pl.item, env.report_root_id, &prog_ctx.prog.data_items))
        {
            // Multi-column writable report item (`COLUMN 1, 11, 21, 27 PIC 999` in `0021`/`0027`!).
            let elem_w = match &el.formatter {
                HirReportFormatterKind::Picture { byte_len, .. } => i64::from(*byte_len),
                HirReportFormatterKind::FixedAlpha { width, .. } => i64::from(*width),
                HirReportFormatterKind::RawBytes => 1,
            };
            let add_off = emit_i64(
                elem_w.saturating_mul(i64::try_from(col_i).unwrap_or(0)),
                block,
                prog_ctx,
                state,
                loc,
            );
            step_env.byte_offset_i64 = append_val(
                block,
                arith::addi(env.byte_offset_i64, add_off, loc).into(),
                env.byte_offset_i64,
            );
        }

        let (data_ptr, data_len) = format_report_source_to_slice(
            plan,
            src_kind,
            &el.formatter,
            step_env,
            span,
            block,
            prog_ctx,
            state,
        )?;

        let map_align = |a: HirReportColAlign| match a {
            HirReportColAlign::Left => 0_i32,
            HirReportColAlign::Center => 1,
            HirReportColAlign::Right => 2,
            HirReportColAlign::ExplicitLeft => 3,
        };
        let (col_kind, base_col, align_code) = match col_spec {
            HirReportColSpec::Absolute { col, align } => {
                (1_i32, i32::try_from(col).unwrap_or(1), map_align(align))
            }
            HirReportColSpec::Relative { offset, align } => {
                (2, i32::try_from(offset).unwrap_or(1), map_align(align))
            }
        };
        let ck_v = emit_i32(col_kind, block, prog_ctx, state, loc);
        let base_c_v = emit_i32(base_col, block, prog_ctx, state, loc);
        let eff_c_v = if col_kind == 1 {
            append_val(
                block,
                arith::addi(base_c_v, env.col_step_i32, loc).into(),
                base_c_v,
            )
        } else if el.occurs.is_some() {
            let one_c = emit_i32(1, block, prog_ctx, state, loc);
            append_val(
                block,
                arith::select(env.in_occurs_after_first, one_c, base_c_v, loc).into(),
                base_c_v,
            )
        } else {
            base_c_v
        };
        let is_lit_flag = if matches!(src_kind, HirReportSourceKind::LiteralBytes(_)) {
            0x4_i32
        } else {
            0_i32
        };
        let step_bits = i32::try_from(step_env.active_step_cols.min(0xFFFF)).unwrap_or(0) << 8;
        let al_v = emit_i32(
            align_code | is_lit_flag | step_bits,
            block,
            prog_ctx,
            state,
            loc,
        );
        let _ = emit_rt_call(
            RtDispatchTarget::ReportPutItem,
            &[rid_v, ck_v, eff_c_v, al_v, gi_v, data_ptr, data_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Formats a [`HirReportSourceKind`] through [`HirReportFormatterKind`] into `(ptr, len_i64)`.
#[expect(
    clippy::too_many_arguments,
    reason = "Formats any report item source into a stack buffer."
)]
fn format_report_source_to_slice<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    src: &HirReportSourceKind,
    formatter: &HirReportFormatterKind,
    env: SubtreeOccursEnv<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let loc = state.loc(span)?;
    let (dst_ty, dst_len_u32) = match formatter {
        HirReportFormatterKind::Picture { byte_len, ty } => (ty.clone(), (*byte_len).max(1)),
        HirReportFormatterKind::FixedAlpha {
            width, justified, ..
        } => (
            HirType::Alphanumeric {
                byte_len: (*width).max(1),
                justified: *justified,
            },
            (*width).max(1),
        ),
        HirReportFormatterKind::RawBytes => {
            if let HirReportSourceKind::LiteralBytes(bytes) = src {
                return emit_const_slice(bytes, block, prog_ctx, state, loc);
            }
            if let HirReportSourceKind::Place(pl) = src {
                let v = lower_place_view_with_report_offset(pl, env, block, prog_ctx, state, loc)?;
                return Ok((v.0, v.1));
            }
            (
                HirType::Alphanumeric {
                    byte_len: 16,
                    justified: coblang_hir::JustifiedSpec::Left,
                },
                16,
            )
        }
    };

    if let HirReportSourceKind::LiteralBytes(bytes) = src
        && matches!(formatter, HirReportFormatterKind::FixedAlpha { .. })
        && u32::try_from(bytes.len()).ok() == Some(dst_len_u32)
    {
        return emit_const_slice(bytes, block, prog_ctx, state, loc);
    }

    let ptr_ty = llvm_ptr_type(state.ctx);
    let i8_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 8).into();
    let dst_len_v = emit_i64(i64::from(dst_len_u32), block, prog_ctx, state, loc);
    let dst_ptr = alloc_stack_slot(block, state.ctx.raw(), ptr_ty, i8_ty, dst_len_v, loc);
    let dst_spec = make_runtime_field_spec(dst_ty.clone(), prog_ctx.prog.formatting);
    let dst_spec_bytes = encode_field_spec(&dst_spec);
    let (ds_ptr, ds_len) = emit_const_slice(&dst_spec_bytes, block, prog_ctx, state, loc)?;

    match src {
        HirReportSourceKind::LiteralBytes(bytes) => {
            let (s_ptr, s_len) = emit_const_slice(bytes, block, prog_ctx, state, loc)?;
            let (emp_p, zero_l) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
            let k = emit_i32(
                RuntimeMoveSourceKind::AlphanumericLiteral.to_abi_code(),
                block,
                prog_ctx,
                state,
                loc,
            );
            let _ = emit_rt_call(
                RtDispatchTarget::MoveDesc,
                &[
                    k, s_ptr, s_len, emp_p, zero_l, dst_ptr, dst_len_v, ds_ptr, ds_len,
                ],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirReportSourceKind::SumCounter(sum_idx) => {
            let rid_v = emit_i32(
                i32::try_from(plan.report_id.as_u32()).unwrap_or(0),
                block,
                prog_ctx,
                state,
                loc,
            );
            let s_v = emit_i32(
                i32::try_from(*sum_idx).unwrap_or(0),
                block,
                prog_ctx,
                state,
                loc,
            );
            let _ = emit_rt_call(
                RtDispatchTarget::ReportSyncSumField,
                &[rid_v, s_v, dst_ptr, dst_len_v, ds_ptr, ds_len],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirReportSourceKind::PageCounter => {
            emit_move_data_item_to_buf(
                plan.page_counter_item,
                dst_ptr,
                dst_len_v,
                ds_ptr,
                ds_len,
                span,
                block,
                prog_ctx,
                state,
            )?;
        }
        HirReportSourceKind::LineCounter => {
            emit_move_data_item_to_buf(
                plan.line_counter_item,
                dst_ptr,
                dst_len_v,
                ds_ptr,
                ds_len,
                span,
                block,
                prog_ctx,
                state,
            )?;
        }
        HirReportSourceKind::Place(pl) => {
            let (s_ptr, s_len, s_spec) =
                lower_place_view_with_report_offset(pl, env, block, prog_ctx, state, loc)?;
            let (ss_ptr, ss_len) = emit_spec_slice(&s_spec, block, prog_ctx, state, loc)?;
            let k = emit_i32(
                RuntimeMoveSourceKind::Field.to_abi_code(),
                block,
                prog_ctx,
                state,
                loc,
            );
            let _ = emit_rt_call(
                RtDispatchTarget::MoveDesc,
                &[
                    k, s_ptr, s_len, ss_ptr, ss_len, dst_ptr, dst_len_v, ds_ptr, ds_len,
                ],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirReportSourceKind::Expr(expr) => {
            if matches!(
                dst_ty.category(),
                DataCategory::Numeric | DataCategory::NumericEdited
            ) {
                let _ = emit_rt_call(RtDispatchTarget::DecBegin, &[], block, prog_ctx, state, loc)?;
                emit_decimal_expr(expr, block, prog_ctx, state)?;
                let z0 = emit_i32(0, block, prog_ctx, state, loc);
                let _ = emit_rt_call(
                    RtDispatchTarget::DecStore,
                    &[dst_ptr, dst_len_v, ds_ptr, ds_len, z0, z0, z0],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            } else {
                let (k, s_ptr, s_len, ss_ptr, ss_len) =
                    lower_move_source_args(expr, block, prog_ctx, state, loc)?;
                let _ = emit_rt_call(
                    RtDispatchTarget::MoveDesc,
                    &[
                        k, s_ptr, s_len, ss_ptr, ss_len, dst_ptr, dst_len_v, ds_ptr, ds_len,
                    ],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            }
        }
    }
    Ok((dst_ptr, dst_len_v))
}

/// Lowers `pl` and adds `env.byte_offset_i64` when `pl.item` belongs to the enclosing `01` report group.
fn lower_place_view_with_report_offset<'ctx, 'b, 'e: 'b>(
    pl: &HirPlace,
    env: SubtreeOccursEnv<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    coblang_rt_core::desc_codec::RuntimeFieldSpec,
)> {
    let v = lower_place_view(pl, block, prog_ctx, state)?;
    if is_inside_report_root(pl.item, env.report_root_id, &prog_ctx.prog.data_items) {
        let ptr_ty = llvm_ptr_type(state.ctx);
        let i8_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 8).into();
        let adj_ptr = append_val(
            block,
            llvm::get_element_ptr_dynamic(
                state.ctx.raw(),
                v.ptr,
                &[env.byte_offset_i64],
                i8_ty,
                ptr_ty,
                loc,
            )
            .into(),
            v.ptr,
        );
        Ok((adj_ptr, v.len_i64, v.spec))
    } else {
        Ok((v.ptr, v.len_i64, v.spec))
    }
}

/// Checks whether `item_id` is `root_id` or a descendant of `root_id` in `data_items`.
fn is_inside_report_root(
    item_id: DataItemId,
    root_id: u32,
    data_items: &[coblang_hir::HirDataItem],
) -> bool {
    if root_id == u32::MAX {
        return false;
    }
    let mut cur = Some(item_id);
    while let Some(id) = cur {
        if id.as_u32() == root_id {
            return true;
        }
        cur = data_items
            .get(id.as_u32() as usize)
            .and_then(|it| it.parent);
    }
    false
}

/// Starts a new print line (`ReportCollectLineSpec` in Pass A, `ReportBeginRenderLine` + `LINE-COUNTER` sync in Pass B).
#[expect(
    clippy::too_many_arguments,
    reason = "Starts a report print line in Pass A or Pass B."
)]
fn emit_start_new_line<'ctx, 'b, 'e: 'b>(
    plan: &HirReportPlan,
    ls: HirReportLineSpec,
    mode: GroupWalkMode,
    has_open_line_slot: Value<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let rid_v = emit_i32(
        i32::try_from(plan.report_id.as_u32()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let one = emit_i32(1, block, prog_ctx, state, loc);
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        one,
        has_open_line_slot,
        loc,
        llvm::LoadStoreOptions::new(),
    ));

    match mode {
        GroupWalkMode::CollectLineSpecs => {
            let (kind, val) = match ls {
                HirReportLineSpec::Absolute(n) => (1_i32, i32::try_from(n).unwrap_or(1)),
                HirReportLineSpec::Relative(d) => (2, i32::try_from(d).unwrap_or(1)),
                HirReportLineSpec::OnNextPage(n) => (3, i32::try_from(n).unwrap_or(1)),
            };
            let k_v = emit_i32(kind, block, prog_ctx, state, loc);
            let v_v = emit_i32(val, block, prog_ctx, state, loc);
            let _ = emit_rt_call(
                RtDispatchTarget::ReportCollectLineSpec,
                &[rid_v, k_v, v_v],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        GroupWalkMode::RenderItems => {
            let target_lc = emit_rt_call(
                RtDispatchTarget::ReportBeginRenderLine,
                &[rid_v],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            store_i32_to_data_item(
                plan.line_counter_item,
                target_lc,
                span,
                block,
                prog_ctx,
                state,
            )?;
        }
    }
    Ok(())
}

/// Wraps `body_fn` inside a `PRESENT WHEN` / `AFTER NEW` `scf.if` when `gate != Always`.
fn emit_with_present_gate<'ctx, 'b, 'e: 'b, F>(
    plan: &HirReportPlan,
    gate: &HirReportPresentKind,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    body_fn: F,
) -> Result<()>
where
    F: for<'sb> FnOnce(
        &'sb Block<'ctx>,
        &ProgramLowerCtx<'_, 'ctx, 'e>,
        &mut ModuleLowerState<'_, 'ctx>,
    ) -> Result<()>,
{
    if matches!(gate, HirReportPresentKind::Always) {
        return body_fn(block, prog_ctx, state);
    }
    let loc = state.loc(span)?;
    let cond_i1 = eval_present_when(plan.report_id.as_u32(), gate, span, block, prog_ctx, state)?;
    let then_reg = Region::new();
    let else_reg = Region::new();
    let tb = then_reg.append_block(Block::new(&[]));
    body_fn(&tb, prog_ctx, state)?;
    let _ = tb.append_operation(scf::r#yield(&[], loc));
    let eb = else_reg.append_block(Block::new(&[]));
    let _ = eb.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(cond_i1, &[], then_reg, else_reg, loc));
    Ok(())
}

/// Wraps `body_fn` inside a dynamic `CobolLoopOp` when `occurs.is_some()`, updating `VARYING`, `STEP`, and `occurs_byte_stride`.
fn emit_with_optional_occurs<'ctx, 'b, 'e: 'b, F>(
    occurs: Option<&HirReportOccursSpec>,
    env: SubtreeOccursEnv<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    body_fn: F,
) -> Result<()>
where
    F: for<'sb> FnOnce(
        &'sb Block<'ctx>,
        SubtreeOccursEnv<'ctx, 'sb>,
        &ProgramLowerCtx<'_, 'ctx, 'e>,
        &mut ModuleLowerState<'_, 'ctx>,
    ) -> Result<()>,
{
    let Some(occ) = occurs else {
        return body_fn(block, env, prog_ctx, state);
    };
    let loc = state.loc(span)?;
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i64_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 64).into();
    let i32_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 32).into();
    let one_i64 = emit_i64(1, block, prog_ctx, state, loc);
    let idx_slot = alloc_stack_slot(block, state.ctx.raw(), ptr_ty, i64_ty, one_i64, loc);
    let zero_i64 = emit_i64(0, block, prog_ctx, state, loc);
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        zero_i64,
        idx_slot,
        loc,
        llvm::LoadStoreOptions::new(),
    ));

    let max_v = emit_i64(i64::from(occ.max_count), block, prog_ctx, state, loc);
    let active_limit = if let Some(odo_e) = &occ.depending_on {
        let dyn_v = eval_expr_as_i64(odo_e, block, prog_ctx, state)?;
        let clamped_lo = append_val(block, arith::maxsi(dyn_v, zero_i64, loc).into(), dyn_v);
        append_val(
            block,
            arith::minsi(clamped_lo, max_v, loc).into(),
            clamped_lo,
        )
    } else {
        max_v
    };

    let cond_region = Region::new();
    let cb = cond_region.append_block(Block::new(&[]));
    let cur_i = load_i64_slot(&cb, state.ctx.raw(), idx_slot, i64_ty, loc);
    let is_done = append_val(
        &cb,
        arith::cmpi(
            state.ctx.raw(),
            CmpiPredicate::Sge,
            cur_i,
            active_limit,
            loc,
        )
        .into(),
        cur_i,
    );
    let _ = cb.append_operation(
        CobolYieldOp {
            value: Some(is_done),
        }
        .build(state.ctx, loc)?,
    );

    let body_region = Region::new();
    let bb = body_region.append_block(Block::new(&[]));
    let body_i = load_i64_slot(&bb, state.ctx.raw(), idx_slot, i64_ty, loc);

    if let Some((var_pl, from_v, by_v)) = &occ.varying_place {
        let from_c = emit_i64(*from_v, &bb, prog_ctx, state, loc);
        let by_c = emit_i64(*by_v, &bb, prog_ctx, state, loc);
        let prod = append_val(&bb, arith::muli(body_i, by_c, loc).into(), body_i);
        let var_val = append_val(&bb, arith::addi(from_c, prod, loc).into(), from_c);
        let var_i32 = append_val(&bb, arith::trunci(var_val, i32_ty, loc).into(), var_val);
        store_i32_to_data_item(var_pl.item, var_i32, span, &bb, prog_ctx, state)?;
    }

    let stride_c = emit_i64(i64::from(occ.occurs_byte_stride), &bb, prog_ctx, state, loc);
    let off_delta = append_val(&bb, arith::muli(body_i, stride_c, loc).into(), body_i);
    let next_byte_off = append_val(
        &bb,
        arith::addi(env.byte_offset_i64, off_delta, loc).into(),
        env.byte_offset_i64,
    );

    let step_c = emit_i64(
        i64::from(occ.step_cols.unwrap_or(0)),
        &bb,
        prog_ctx,
        state,
        loc,
    );
    let col_delta_64 = append_val(&bb, arith::muli(body_i, step_c, loc).into(), body_i);
    let col_delta_32 = append_val(
        &bb,
        arith::trunci(col_delta_64, i32_ty, loc).into(),
        col_delta_64,
    );
    let next_col_step = append_val(
        &bb,
        arith::addi(env.col_step_i32, col_delta_32, loc).into(),
        env.col_step_i32,
    );

    let zero_bb = emit_i64(0, &bb, prog_ctx, state, loc);
    let after_first = append_val(
        &bb,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Sgt, body_i, zero_bb, loc).into(),
        body_i,
    );
    let sub_env = SubtreeOccursEnv {
        byte_offset_i64: next_byte_off,
        col_step_i32: next_col_step,
        active_step_cols: occ.step_cols.unwrap_or(env.active_step_cols),
        in_occurs_after_first: after_first,
        report_root_id: env.report_root_id,
    };
    body_fn(&bb, sub_env, prog_ctx, state)?;
    let _ = bb.append_operation(CobolYieldOp { value: None }.build(state.ctx, loc)?);

    let step_region = Region::new();
    let sb = step_region.append_block(Block::new(&[]));
    let prev_i = load_i64_slot(&sb, state.ctx.raw(), idx_slot, i64_ty, loc);
    let step_one = emit_i64(1, &sb, prog_ctx, state, loc);
    let next_i = append_val(&sb, arith::addi(prev_i, step_one, loc).into(), prev_i);
    let _ = sb.append_operation(llvm::store(
        state.ctx.raw(),
        next_i,
        idx_slot,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let _ = sb.append_operation(CobolYieldOp { value: None }.build(state.ctx, loc)?);

    let loop_op = CobolLoopOp {
        loop_kind: "user_perform",
        test_pos: "before",
        cond_region,
        body_region,
        step_region,
    }
    .build(state.ctx, loc)?;
    let _ = block.append_operation(loop_op);
    Ok(())
}

/// Evaluates [`HirReportPresentKind`] to an `i1` SSA value.
fn eval_present_when<'ctx, 'b, 'e: 'b>(
    report_id: u32,
    present: &HirReportPresentKind,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let loc = state.loc(span)?;
    let i1_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 1).into();
    match present {
        HirReportPresentKind::Always => {
            let t = arith::constant(state.ctx.raw(), IntegerAttribute::new(i1_ty, 1).into(), loc);
            Ok(append_val(
                block,
                t.into(),
                emit_i32(1, block, prog_ctx, state, loc),
            ))
        }
        HirReportPresentKind::Condition(cond) => {
            lower_condition(cond, span, block, prog_ctx, state)
        }
        HirReportPresentKind::AfterNewControl { control_idx } => {
            emit_after_new_check_i1(report_id, *control_idx, block, prog_ctx, state, loc)
        }
        HirReportPresentKind::AnyAfterNew(list) => {
            let mut acc = append_val(
                block,
                arith::constant(state.ctx.raw(), IntegerAttribute::new(i1_ty, 0).into(), loc)
                    .into(),
                emit_i32(0, block, prog_ctx, state, loc),
            );
            for &ci in list {
                let term = emit_after_new_check_i1(report_id, ci, block, prog_ctx, state, loc)?;
                acc = append_val(block, arith::ori(acc, term, loc).into(), acc);
            }
            Ok(acc)
        }
        HirReportPresentKind::Not(inner) => {
            let base = eval_present_when(report_id, inner, span, block, prog_ctx, state)?;
            let one = append_val(
                block,
                arith::constant(state.ctx.raw(), IntegerAttribute::new(i1_ty, 1).into(), loc)
                    .into(),
                base,
            );
            Ok(append_val(block, arith::xori(base, one, loc).into(), base))
        }
    }
}

/// Calls `ReportCheckAfterNew(report_id, ctrl_idx)` and compares `!= 0` to `i1`.
fn emit_after_new_check_i1<'ctx, 'b, 'e: 'b>(
    report_id: u32,
    ctrl_idx: u16,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let rid_v = emit_i32(
        i32::try_from(report_id).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let ci_v = emit_i32(i32::from(ctrl_idx), block, prog_ctx, state, loc);
    let res = emit_rt_call(
        RtDispatchTarget::ReportCheckAfterNew,
        &[rid_v, ci_v],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let z0 = emit_i32(0, block, prog_ctx, state, loc);
    Ok(append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, res, z0, loc).into(),
        z0,
    ))
}

// ============================================================================
// Low-Level Stack Slot & Data-Item Load/Store Helpers
// ============================================================================

/// Emits `scf.if (code_v == expected)` executing `body_fn` inside the `then` region.
fn emit_action_branch<'ctx, 'b, 'e: 'b, F>(
    block: &'b Block<'ctx>,
    code_v: Value<'ctx, 'b>,
    expected: i32,
    state: &mut ModuleLowerState<'_, 'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    loc: Location<'ctx>,
    body_fn: F,
) -> Result<()>
where
    F: for<'sb> FnOnce(
        &'sb Block<'ctx>,
        &ProgramLowerCtx<'_, 'ctx, 'e>,
        &mut ModuleLowerState<'_, 'ctx>,
    ) -> Result<()>,
{
    let exp_v = emit_i32(expected, block, prog_ctx, state, loc);
    let cmp = append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, code_v, exp_v, loc).into(),
        exp_v,
    );
    let then_reg = Region::new();
    let else_reg = Region::new();
    let tb = then_reg.append_block(Block::new(&[]));
    body_fn(&tb, prog_ctx, state)?;
    let _ = tb.append_operation(scf::r#yield(&[], loc));
    let eb = else_reg.append_block(Block::new(&[]));
    let _ = eb.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(cmp, &[], then_reg, else_reg, loc));
    Ok(())
}

/// Allocates an `llvm.alloca` slot of `count` elements of `elem_ty`.
fn alloc_stack_slot<'ctx, 'b>(
    block: &'b Block<'ctx>,
    ctx: &'ctx melior::Context,
    ptr_ty: Type<'ctx>,
    elem_ty: Type<'ctx>,
    count: Value<'ctx, 'b>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    let op = llvm::alloca(
        ctx,
        count,
        ptr_ty,
        loc,
        llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(elem_ty))),
    );
    append_val(block, op.into(), count)
}

/// Loads an `i32` from stack slot `ptr`.
fn load_i32_slot<'ctx, 'b>(
    block: &'b Block<'ctx>,
    ctx: &'ctx melior::Context,
    ptr: Value<'ctx, 'b>,
    i32_ty: Type<'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    let op = llvm::load(ctx, ptr, i32_ty, loc, llvm::LoadStoreOptions::new());
    append_val(block, op.into(), ptr)
}

/// Loads an `i64` from stack slot `ptr`.
fn load_i64_slot<'ctx, 'b>(
    block: &'b Block<'ctx>,
    ctx: &'ctx melior::Context,
    ptr: Value<'ctx, 'b>,
    i64_ty: Type<'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    let op = llvm::load(ctx, ptr, i64_ty, loc, llvm::LoadStoreOptions::new());
    append_val(block, op.into(), ptr)
}

/// Stores a native `i32` SSA value into a 4-byte `BinaryInt` `DataItemId`.
fn store_i32_to_data_item<'ctx, 'b, 'e: 'b>(
    item_id: DataItemId,
    val_i32: Value<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let Some(item) = prog_ctx.prog.data_items.get(item_id.as_u32() as usize) else {
        return Ok(());
    };
    let loc = state.loc(span)?;
    let pl = HirPlace {
        item: item_id,
        static_offset: item.byte_offset,
        effective_ty: item.ty.clone(),
        subscripts: Vec::new(),
        refmod: None,
        check_mask: ExceptionCheckMask::NONE,
        span,
    };
    let v = lower_place_view(&pl, block, prog_ctx, state)?;
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        val_i32,
        v.ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    Ok(())
}

/// Loads a native `i32` SSA value from a 4-byte `BinaryInt` `DataItemId`.
fn load_i32_from_data_item<'ctx, 'b, 'e: 'b>(
    item_id: DataItemId,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let loc = state.loc(span)?;
    let Some(item) = prog_ctx.prog.data_items.get(item_id.as_u32() as usize) else {
        return Ok(emit_i32(0, block, prog_ctx, state, loc));
    };
    let pl = HirPlace {
        item: item_id,
        static_offset: item.byte_offset,
        effective_ty: item.ty.clone(),
        subscripts: Vec::new(),
        refmod: None,
        check_mask: ExceptionCheckMask::NONE,
        span,
    };
    let v = lower_place_view(&pl, block, prog_ctx, state)?;
    let i32_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 32).into();
    Ok(load_i32_slot(block, state.ctx.raw(), v.ptr, i32_ty, loc))
}

/// Formats `item_id` (`PAGE-COUNTER` / `LINE-COUNTER`) into `(dst_ptr, dst_len_v)` via `MoveDesc`.
#[expect(
    clippy::too_many_arguments,
    reason = "Internal special-register field formatter."
)]
fn emit_move_data_item_to_buf<'ctx, 'b, 'e: 'b>(
    item_id: DataItemId,
    dst_ptr: Value<'ctx, 'b>,
    dst_len_v: Value<'ctx, 'b>,
    ds_ptr: Value<'ctx, 'b>,
    ds_len: Value<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let Some(item) = prog_ctx.prog.data_items.get(item_id.as_u32() as usize) else {
        return Ok(());
    };
    let loc = state.loc(span)?;
    let pl = HirPlace {
        item: item_id,
        static_offset: item.byte_offset,
        effective_ty: item.ty.clone(),
        subscripts: Vec::new(),
        refmod: None,
        check_mask: ExceptionCheckMask::NONE,
        span,
    };
    let v = lower_place_view(&pl, block, prog_ctx, state)?;
    let (ss_ptr, ss_len) = emit_spec_slice(&v.spec, block, prog_ctx, state, loc)?;
    let k = emit_i32(
        RuntimeMoveSourceKind::Field.to_abi_code(),
        block,
        prog_ctx,
        state,
        loc,
    );
    let _ = emit_rt_call(
        RtDispatchTarget::MoveDesc,
        &[
            k, v.ptr, v.len_i64, ss_ptr, ss_len, dst_ptr, dst_len_v, ds_ptr, ds_len,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Formats `<basename>:<line>` (e.g. `"prog.cob:31"`) from `span` using `state.source_map`.
fn format_site_string(span: SourceSpan, state: &ModuleLowerState<'_, '_>) -> String {
    if let Ok(resolved) = state.source_map.resolve_span(span) {
        let base = std::path::Path::new(&resolved.file_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&resolved.file_path);
        format!("{base}:{}", resolved.line.get())
    } else {
        String::from("prog.cob:1")
    }
}
