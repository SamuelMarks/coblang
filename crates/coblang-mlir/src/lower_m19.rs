//! Milestone M19 (`SCREEN SECTION`, extended `DISPLAY`/`ACCEPT`, and `SET ... ATTRIBUTE`)
//! HIR-to-`cobol` MLIR lowering.

#![deny(clippy::indexing_slicing)]

use coblang_common::{DigitCount, Result, ScaleFactor, SourceSpan};
use coblang_hir::{
    BlankZeroSpec, DataCategory, HirConstValue, HirExpr, HirFigurative, HirM19Stmt, HirPlace,
    HirScreenBindingPlan, HirScreenColorSpec, HirScreenCoordSpec, HirScreenDisplayItem,
    HirScreenFieldPlan, HirScreenGroupPlan, HirScreenNodePlan, HirScreenOverrides,
    HirScreenPromptSpec, HirScreenSpecialRegs, HirStmt, HirType, JustifiedSpec, NumericSignSpec,
    Signedness,
};
use coblang_rt_core::{desc_codec::encode_field_spec, field_ops::RuntimeMoveSourceKind};
use melior::dialect::arith::CmpiPredicate;
use melior::dialect::{arith, llvm, scf};
use melior::ir::attribute::TypeAttribute;
use melior::ir::block::BlockLike;
use melior::ir::r#type::IntegerType;
use melior::ir::{Block, Location, Region, RegionLike, Type, Value};

use crate::lower_from_hir::{
    ModuleLowerState, ProgramLowerCtx, append_val, emit_const_slice, emit_decimal_expr, emit_i32,
    emit_i64, emit_rt_call, emit_spec_slice, eval_expr_as_i64, lower_move_source_args,
    lower_place_view, lower_stmt,
};
use crate::lower_m14::emit_null_ptr;
use crate::ops::{RtDispatchTarget, llvm_ptr_type};
use crate::storage_init::make_runtime_field_spec;

/// Bit `20` in `ScreenAttrFlags` (`ATTR_UPDATE`).
const ATTR_UPDATE_BIT: u32 = 1 << 20;

/// Inherited group attributes & colors threaded down a `HirScreenNodePlan` hierarchy.
#[derive(Clone)]
struct ScreenInheritEnv {
    /// Owning `screen_id`.
    screen_id: i32,
    /// Inherited `ScreenAttrFlags` bitmask.
    attr_bits: u32,
    /// Inherited foreground color.
    fg_color: HirScreenColorSpec,
    /// Inherited background color.
    bg_color: HirScreenColorSpec,
    /// Whether root `overrides` apply to the very first positioned item.
    root_overrides: Option<HirScreenOverrides>,
}

/// Dispatches [`HirM19Stmt`] statements into `cobol` MLIR operations.
///
/// # Arguments
///
/// * `stmt` - Lowered M19 screen statement.
/// * `block` - Target MLIR insertion block.
/// * `prog_ctx` - Active program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
pub(crate) fn lower_m19_stmt<'ctx, 'b, 'e: 'b>(
    stmt: &HirM19Stmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    match stmt {
        HirM19Stmt::SetScreenAttr {
            screen_id,
            on_bits,
            off_bits,
            span,
        } => {
            let loc = state.loc(*span)?;
            let sid_v = emit_i32(
                i32::try_from(*screen_id).unwrap_or(0),
                block,
                prog_ctx,
                state,
                loc,
            );
            let on_v = emit_i32(*on_bits as i32, block, prog_ctx, state, loc);
            let off_v = emit_i32(*off_bits as i32, block, prog_ctx, state, loc);
            let _ = emit_rt_call(
                RtDispatchTarget::ScreenSetAttr,
                &[sid_v, on_v, off_v],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            Ok(())
        }
        HirM19Stmt::DisplayScreen {
            items,
            on_exception,
            not_on_exception,
            span,
        } => {
            let loc = state.loc(*span)?;
            let _ = emit_rt_call(
                RtDispatchTarget::ScreenBeginStmt,
                &[],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            for item in items {
                match item {
                    HirScreenDisplayItem::NamedScreen {
                        screen_id,
                        overrides,
                    } => {
                        if let Some(plan) = prog_ctx
                            .prog
                            .screen_plans
                            .iter()
                            .find(|p| p.screen_id == *screen_id)
                        {
                            let mut env = ScreenInheritEnv {
                                screen_id: i32::try_from(*screen_id).unwrap_or(0),
                                attr_bits: overrides.attr_bits,
                                fg_color: overrides.fg_color.clone(),
                                bg_color: overrides.bg_color.clone(),
                                root_overrides: Some(overrides.clone()),
                            };
                            emit_stage_screen_node(
                                &plan.root, false, &mut env, *span, block, prog_ctx, state,
                            )?;
                        }
                    }
                    HirScreenDisplayItem::ExtendedField { expr, overrides } => {
                        emit_stage_adhoc_display_item(
                            expr.as_ref(),
                            overrides,
                            *span,
                            block,
                            prog_ctx,
                            state,
                        )?;
                    }
                }
            }
            let (cur_p, cur_l, crt_p, crt_l, cob_p) = emit_special_reg_ptrs(
                &prog_ctx.prog.screen_special_regs,
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let exc = emit_rt_call(
                RtDispatchTarget::ScreenExecuteDisplay,
                &[cur_p, cur_l, crt_p, crt_l, cob_p],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            emit_exception_branches(
                exc,
                on_exception,
                not_on_exception,
                block,
                prog_ctx,
                state,
                loc,
            )
        }
        HirM19Stmt::AcceptScreen {
            screen_id,
            adhoc_place,
            overrides,
            on_exception,
            not_on_exception,
            span,
        } => {
            let loc = state.loc(*span)?;
            let _ = emit_rt_call(
                RtDispatchTarget::ScreenBeginStmt,
                &[],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            if let Some(sid) = screen_id {
                if let Some(plan) = prog_ctx
                    .prog
                    .screen_plans
                    .iter()
                    .find(|p| p.screen_id == *sid)
                {
                    let mut env = ScreenInheritEnv {
                        screen_id: i32::try_from(*sid).unwrap_or(0),
                        attr_bits: overrides.attr_bits,
                        fg_color: overrides.fg_color.clone(),
                        bg_color: overrides.bg_color.clone(),
                        root_overrides: Some((**overrides).clone()),
                    };
                    emit_stage_screen_node(
                        &plan.root, true, &mut env, *span, block, prog_ctx, state,
                    )?;
                }
            } else if let Some(pl) = adhoc_place {
                emit_stage_adhoc_accept_place(pl, overrides, *span, block, prog_ctx, state)?;
            } else if overrides.attr_bits != 0 {
                emit_stage_adhoc_display_item(None, overrides, *span, block, prog_ctx, state)?;
            }

            let timeout_ms_v = if let Some(te) = &overrides.timeout_expr {
                let raw = eval_expr_as_i64(te, block, prog_ctx, state)?;
                let scale = emit_i64(
                    i64::from(overrides.timeout_scale_ms.max(1)),
                    block,
                    prog_ctx,
                    state,
                    loc,
                );
                append_val(block, arith::muli(raw, scale, loc).into(), raw)
            } else {
                emit_i64(0, block, prog_ctx, state, loc)
            };

            let init_cur_v = if let Some(cp) = &overrides.cursor_offset_place {
                let ce = HirExpr::Place(cp.clone());
                eval_expr_as_i64(&ce, block, prog_ctx, state)?
            } else if let Some(ce) = &overrides.cursor_offset_expr {
                eval_expr_as_i64(ce, block, prog_ctx, state)?
            } else {
                emit_i64(0, block, prog_ctx, state, loc)
            };

            let ptr_ty = llvm_ptr_type(state.ctx);
            let i64_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 64).into();
            let one_i64 = emit_i64(1, block, prog_ctx, state, loc);
            let out_cur_slot = append_val(
                block,
                llvm::alloca(
                    state.ctx.raw(),
                    one_i64,
                    ptr_ty,
                    loc,
                    llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i64_ty))),
                )
                .into(),
                one_i64,
            );

            let (cur_p, cur_l, crt_p, crt_l, cob_p) = emit_special_reg_ptrs(
                &prog_ctx.prog.screen_special_regs,
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let exc = emit_rt_call(
                RtDispatchTarget::ScreenExecuteAccept,
                &[
                    timeout_ms_v,
                    init_cur_v,
                    out_cur_slot,
                    cur_p,
                    cur_l,
                    crt_p,
                    crt_l,
                    cob_p,
                ],
                block,
                prog_ctx,
                state,
                loc,
            )?;

            if let Some(cp) = &overrides.cursor_offset_place {
                emit_store_i64_slot_into_place(out_cur_slot, cp, block, prog_ctx, state, loc)?;
            }

            emit_exception_branches(
                exc,
                on_exception,
                not_on_exception,
                block,
                prog_ctx,
                state,
                loc,
            )
        }
    }
}

/// Recursively stages a `HirScreenNodePlan` (`Group` or `Field`) for `DISPLAY` or `ACCEPT`.
#[expect(
    clippy::too_many_arguments,
    reason = "Recursive screen hierarchy stager."
)]
fn emit_stage_screen_node<'ctx, 'b, 'e: 'b>(
    node: &HirScreenNodePlan,
    for_accept: bool,
    env: &mut ScreenInheritEnv,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    match node {
        HirScreenNodePlan::Group(grp) => {
            emit_stage_screen_group(grp, for_accept, env, span, block, prog_ctx, state, loc)
        }
        HirScreenNodePlan::Field(fld) => {
            emit_stage_screen_field(fld, for_accept, env, block, prog_ctx, state, loc)
        }
    }
}

/// Stages a `HirScreenGroupPlan`'s positioning/directive header and recurses into its children.
#[expect(clippy::too_many_arguments, reason = "Recursive screen group stager.")]
fn emit_stage_screen_group<'ctx, 'b, 'e: 'b>(
    grp: &HirScreenGroupPlan,
    for_accept: bool,
    env: &mut ScreenInheritEnv,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let root_ov = env.root_overrides.take();
    let eff_fg = merge_color(&grp.fg_color, &env.fg_color);
    let eff_bg = merge_color(&grp.bg_color, &env.bg_color);
    let grp_bits = env.attr_bits | grp.attr_bits;

    let eff_line = root_ov
        .as_ref()
        .filter(|ov| !matches!(ov.line, HirScreenCoordSpec::Omitted))
        .map_or(&grp.line, |ov| &ov.line);
    let eff_col = root_ov
        .as_ref()
        .filter(|ov| !matches!(ov.col, HirScreenCoordSpec::Omitted))
        .map_or(&grp.col, |ov| &ov.col);
    let at_pair = root_ov.as_ref().and_then(|ov| ov.at_combined.as_ref());

    if !matches!(eff_line, HirScreenCoordSpec::Omitted)
        || !matches!(eff_col, HirScreenCoordSpec::Omitted)
        || at_pair.is_some()
        || (grp.attr_bits & 0x1F) != 0
    {
        let sid_v = emit_i32(env.screen_id, block, prog_ctx, state, loc);
        let (at_v, at_d) = eval_at_pair(at_pair, block, prog_ctx, state, loc)?;
        let (lk, lv) = eval_coord(eff_line, block, prog_ctx, state, loc)?;
        let (ck, cv) = eval_coord(eff_col, block, prog_ctx, state, loc)?;
        let fg_v = eval_color(&eff_fg, block, prog_ctx, state, loc)?;
        let bg_v = eval_color(&eff_bg, block, prog_ctx, state, loc)?;
        let bits_v = emit_i32((grp_bits & 0x1F) as i32, block, prog_ctx, state, loc);
        let z0_32 = emit_i32(0, block, prog_ctx, state, loc);
        let (emp_p, z0_64) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::ScreenStageDisplayItem,
            &[
                sid_v, at_v, at_d, lk, lv, ck, cv, bits_v, fg_v, bg_v, z0_32, z0_32, z0_32, emp_p,
                z0_64,
            ],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }

    let child_inherited_bits = grp_bits & !0x1F;
    for ch in &grp.children {
        let mut child_env = ScreenInheritEnv {
            screen_id: env.screen_id,
            attr_bits: child_inherited_bits,
            fg_color: eff_fg.clone(),
            bg_color: eff_bg.clone(),
            root_overrides: None,
        };
        emit_stage_screen_node(ch, for_accept, &mut child_env, span, block, prog_ctx, state)?;
    }
    Ok(())
}

/// Stages a single elementary `HirScreenFieldPlan`.
fn emit_stage_screen_field<'ctx, 'b, 'e: 'b>(
    fld: &HirScreenFieldPlan,
    for_accept: bool,
    env: &mut ScreenInheritEnv,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let root_ov = env.root_overrides.take();
    let eff_fg = merge_color(&fld.fg_color, &env.fg_color);
    let eff_bg = merge_color(&fld.bg_color, &env.bg_color);
    let eff_bits = env.attr_bits | fld.attr_bits;

    let eff_line = root_ov
        .as_ref()
        .filter(|ov| !matches!(ov.line, HirScreenCoordSpec::Omitted))
        .map_or(&fld.line, |ov| &ov.line);
    let eff_col = root_ov
        .as_ref()
        .filter(|ov| !matches!(ov.col, HirScreenCoordSpec::Omitted))
        .map_or(&fld.col, |ov| &ov.col);
    let at_pair = root_ov.as_ref().and_then(|ov| ov.at_combined.as_ref());

    let sid_v = emit_i32(env.screen_id, block, prog_ctx, state, loc);
    let (at_v, at_d) = eval_at_pair(at_pair, block, prog_ctx, state, loc)?;
    let (lk, lv) = eval_coord(eff_line, block, prog_ctx, state, loc)?;
    let (ck, cv) = eval_coord(eff_col, block, prog_ctx, state, loc)?;
    let fg_v = eval_color(&eff_fg, block, prog_ctx, state, loc)?;
    let bg_v = eval_color(&eff_bg, block, prog_ctx, state, loc)?;
    let bits_v = emit_i32(eff_bits as i32, block, prog_ctx, state, loc);
    let z0_32 = emit_i32(0, block, prog_ctx, state, loc);

    match &fld.binding {
        HirScreenBindingPlan::None => {
            let (emp_p, z0_64) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::ScreenStageDisplayItem,
                &[
                    sid_v, at_v, at_d, lk, lv, ck, cv, bits_v, fg_v, bg_v, z0_32, z0_32, z0_32,
                    emp_p, z0_64,
                ],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirScreenBindingPlan::StaticValue(bytes) => {
            let w_v = eval_width_i32(
                fld.size_expr.as_ref(),
                u32::try_from(bytes.len()).unwrap_or(1),
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let (d_ptr, d_len) = emit_const_slice(bytes, block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::ScreenStageDisplayItem,
                &[
                    sid_v, at_v, at_d, lk, lv, ck, cv, bits_v, fg_v, bg_v, z0_32, w_v, z0_32,
                    d_ptr, d_len,
                ],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirScreenBindingPlan::FigurativeAll { pattern, width } => {
            let w_v = eval_width_i32(
                fld.size_expr.as_ref(),
                u32::from(*width),
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let repeat_flag = emit_i32(1, block, prog_ctx, state, loc);
            let (d_ptr, d_len) = emit_const_slice(pattern, block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::ScreenStageDisplayItem,
                &[
                    sid_v,
                    at_v,
                    at_d,
                    lk,
                    lv,
                    ck,
                    cv,
                    bits_v,
                    fg_v,
                    bg_v,
                    z0_32,
                    w_v,
                    repeat_flag,
                    d_ptr,
                    d_len,
                ],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirScreenBindingPlan::FromExpr {
            src,
            width,
            screen_ty,
        } => {
            let w_v = eval_width_i32(
                fld.size_expr.as_ref(),
                u32::from(*width),
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let (d_ptr, d_len) = format_expr_into_screen_ty(
                src,
                screen_ty,
                u32::from(*width),
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let _ = emit_rt_call(
                RtDispatchTarget::ScreenStageDisplayItem,
                &[
                    sid_v, at_v, at_d, lk, lv, ck, cv, bits_v, fg_v, bg_v, z0_32, w_v, z0_32,
                    d_ptr, d_len,
                ],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirScreenBindingPlan::ToPlace {
            dst,
            width,
            screen_ty,
        } => {
            let _ = screen_ty;
            let w_v = eval_width_i32(
                fld.size_expr.as_ref(),
                u32::from(*width),
                block,
                prog_ctx,
                state,
                loc,
            )?;
            if for_accept {
                let prompt_v = eval_prompt_i32(&fld.prompt, block, prog_ctx, state, loc)?;
                let dst_v = lower_place_view(dst, block, prog_ctx, state)?;
                let (sp_ptr, sp_len) = emit_spec_slice(&dst_v.spec, block, prog_ctx, state, loc)?;
                let (emp_p, z0_64) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
                let _ = emit_rt_call(
                    RtDispatchTarget::ScreenStageEditItem,
                    &[
                        sid_v,
                        at_v,
                        at_d,
                        lk,
                        lv,
                        ck,
                        cv,
                        bits_v,
                        z0_32,
                        fg_v,
                        bg_v,
                        prompt_v,
                        w_v,
                        z0_32,
                        emp_p,
                        z0_64,
                        dst_v.ptr,
                        dst_v.len_i64,
                        sp_ptr,
                        sp_len,
                    ],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            } else {
                let spaces = vec![b' '; usize::from((*width).max(1))];
                let (d_ptr, d_len) = emit_const_slice(&spaces, block, prog_ctx, state, loc)?;
                let _ = emit_rt_call(
                    RtDispatchTarget::ScreenStageDisplayItem,
                    &[
                        sid_v, at_v, at_d, lk, lv, ck, cv, bits_v, fg_v, bg_v, z0_32, w_v, z0_32,
                        d_ptr, d_len,
                    ],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            }
        }
        HirScreenBindingPlan::UsingPlace {
            target,
            width,
            screen_ty,
        } => {
            let w_v = eval_width_i32(
                fld.size_expr.as_ref(),
                u32::from(*width),
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let src_e = HirExpr::Place(target.clone());
            let (init_ptr, init_len) = format_expr_into_screen_ty(
                &src_e,
                screen_ty,
                u32::from(*width),
                block,
                prog_ctx,
                state,
                loc,
            )?;
            if for_accept {
                let prompt_v = eval_prompt_i32(&fld.prompt, block, prog_ctx, state, loc)?;
                let dst_v = lower_place_view(target, block, prog_ctx, state)?;
                let (sp_ptr, sp_len) = emit_spec_slice(&dst_v.spec, block, prog_ctx, state, loc)?;
                let one_32 = emit_i32(1, block, prog_ctx, state, loc);
                let no_upd = root_ov.as_ref().is_some_and(|ov| ov.force_no_update);
                let no_upd_v = emit_i32(i32::from(no_upd), block, prog_ctx, state, loc);
                let _ = emit_rt_call(
                    RtDispatchTarget::ScreenStageEditItem,
                    &[
                        sid_v,
                        at_v,
                        at_d,
                        lk,
                        lv,
                        ck,
                        cv,
                        bits_v,
                        no_upd_v,
                        fg_v,
                        bg_v,
                        prompt_v,
                        w_v,
                        one_32,
                        init_ptr,
                        init_len,
                        dst_v.ptr,
                        dst_v.len_i64,
                        sp_ptr,
                        sp_len,
                    ],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            } else {
                let _ = emit_rt_call(
                    RtDispatchTarget::ScreenStageDisplayItem,
                    &[
                        sid_v, at_v, at_d, lk, lv, ck, cv, bits_v, fg_v, bg_v, z0_32, w_v, z0_32,
                        init_ptr, init_len,
                    ],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            }
        }
        HirScreenBindingPlan::FromAndTo {
            src,
            dst,
            width,
            screen_ty,
        } => {
            let w_v = eval_width_i32(
                fld.size_expr.as_ref(),
                u32::from(*width),
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let (init_ptr, init_len) = format_expr_into_screen_ty(
                src,
                screen_ty,
                u32::from(*width),
                block,
                prog_ctx,
                state,
                loc,
            )?;
            if for_accept {
                let prompt_v = eval_prompt_i32(&fld.prompt, block, prog_ctx, state, loc)?;
                let dst_v = lower_place_view(dst, block, prog_ctx, state)?;
                let (sp_ptr, sp_len) = emit_spec_slice(&dst_v.spec, block, prog_ctx, state, loc)?;
                let has_upd = (eff_bits & ATTR_UPDATE_BIT) != 0
                    && !root_ov.as_ref().is_some_and(|ov| ov.force_no_update);
                let preload_v = emit_i32(i32::from(has_upd), block, prog_ctx, state, loc);
                let _ = emit_rt_call(
                    RtDispatchTarget::ScreenStageEditItem,
                    &[
                        sid_v,
                        at_v,
                        at_d,
                        lk,
                        lv,
                        ck,
                        cv,
                        bits_v,
                        z0_32,
                        fg_v,
                        bg_v,
                        prompt_v,
                        w_v,
                        preload_v,
                        init_ptr,
                        init_len,
                        dst_v.ptr,
                        dst_v.len_i64,
                        sp_ptr,
                        sp_len,
                    ],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            } else {
                let _ = emit_rt_call(
                    RtDispatchTarget::ScreenStageDisplayItem,
                    &[
                        sid_v, at_v, at_d, lk, lv, ck, cv, bits_v, fg_v, bg_v, z0_32, w_v, z0_32,
                        init_ptr, init_len,
                    ],
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

/// Stages an ad-hoc `DISPLAY <expr> AT/LINE/COL/WITH ...` item.
fn emit_stage_adhoc_display_item<'ctx, 'b, 'e: 'b>(
    expr: Option<&HirExpr>,
    ov: &HirScreenOverrides,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let neg1 = emit_i32(-1, block, prog_ctx, state, loc);
    let (at_v, at_d) = eval_at_pair(ov.at_combined.as_ref(), block, prog_ctx, state, loc)?;
    let (lk, lv) = eval_coord(&ov.line, block, prog_ctx, state, loc)?;
    let (ck, cv) = eval_coord(&ov.col, block, prog_ctx, state, loc)?;
    let bits_v = emit_i32(ov.attr_bits as i32, block, prog_ctx, state, loc);
    let fg_v = eval_color(&ov.fg_color, block, prog_ctx, state, loc)?;
    let bg_v = eval_color(&ov.bg_color, block, prog_ctx, state, loc)?;
    let scroll_v = if let Some(se) = &ov.scroll_lines {
        let s64 = eval_expr_as_i64(se, block, prog_ctx, state)?;
        let i32_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 32).into();
        append_val(block, arith::trunci(s64, i32_ty, loc).into(), neg1)
    } else {
        emit_i32(0, block, prog_ctx, state, loc)
    };

    let Some(e) = expr else {
        let z0_32 = emit_i32(0, block, prog_ctx, state, loc);
        let (emp_p, z0_64) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::ScreenStageDisplayItem,
            &[
                neg1, at_v, at_d, lk, lv, ck, cv, bits_v, fg_v, bg_v, scroll_v, z0_32, z0_32,
                emp_p, z0_64,
            ],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok(());
    };

    // Check figurative `LOW-VALUES` (`0008`/`0009`) and `ALL "x"` (`0011`..`0015`)!
    if let HirExpr::Const { value, .. } = e {
        match value {
            HirConstValue::Figurative(HirFigurative::LowValue) => {
                let w_v = eval_width_i32(ov.size_expr.as_ref(), 1, block, prog_ctx, state, loc)?;
                let mode_lv = emit_i32(2, block, prog_ctx, state, loc);
                let (d_ptr, d_len) = emit_const_slice(&[0x00], block, prog_ctx, state, loc)?;
                let _ = emit_rt_call(
                    RtDispatchTarget::ScreenStageDisplayItem,
                    &[
                        neg1, at_v, at_d, lk, lv, ck, cv, bits_v, fg_v, bg_v, scroll_v, w_v,
                        mode_lv, d_ptr, d_len,
                    ],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
                return Ok(());
            }
            HirConstValue::Figurative(HirFigurative::Space) => {
                // Bare `DISPLAY SPACES ...`: fills from cursor to end of line (`80`) unless `SIZE` is given (`0010`!).
                let w_v = eval_width_i32(ov.size_expr.as_ref(), 80, block, prog_ctx, state, loc)?;
                let mode_rep = emit_i32(1, block, prog_ctx, state, loc);
                let (d_ptr, d_len) = emit_const_slice(b" ", block, prog_ctx, state, loc)?;
                let _ = emit_rt_call(
                    RtDispatchTarget::ScreenStageDisplayItem,
                    &[
                        neg1, at_v, at_d, lk, lv, ck, cv, bits_v, fg_v, bg_v, scroll_v, w_v,
                        mode_rep, d_ptr, d_len,
                    ],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
                return Ok(());
            }
            HirConstValue::Figurative(HirFigurative::All(pat)) => {
                // `DISPLAY ALL "x" ...`: tiles across remaining line (`80`) unless `SIZE` is given (`0011`..`0015`!).
                let w_v = eval_width_i32(ov.size_expr.as_ref(), 80, block, prog_ctx, state, loc)?;
                let mode_rep = emit_i32(1, block, prog_ctx, state, loc);
                let non_empty = if pat.is_empty() {
                    b" ".as_slice()
                } else {
                    pat.as_slice()
                };
                let (d_ptr, d_len) = emit_const_slice(non_empty, block, prog_ctx, state, loc)?;
                let _ = emit_rt_call(
                    RtDispatchTarget::ScreenStageDisplayItem,
                    &[
                        neg1, at_v, at_d, lk, lv, ck, cv, bits_v, fg_v, bg_v, scroll_v, w_v,
                        mode_rep, d_ptr, d_len,
                    ],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
                return Ok(());
            }
            _ => {}
        }
    }

    let (d_ptr, d_len, default_w) = format_adhoc_display_expr(e, block, prog_ctx, state, loc)?;
    let w_v = eval_width_i32(
        ov.size_expr.as_ref(),
        default_w,
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let z0_32 = emit_i32(0, block, prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::ScreenStageDisplayItem,
        &[
            neg1, at_v, at_d, lk, lv, ck, cv, bits_v, fg_v, bg_v, scroll_v, w_v, z0_32, d_ptr,
            d_len,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Stages an ad-hoc `ACCEPT <place> AT/LINE/COL/WITH ...` editable item.
fn emit_stage_adhoc_accept_place<'ctx, 'b, 'e: 'b>(
    pl: &HirPlace,
    ov: &HirScreenOverrides,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let neg1 = emit_i32(-1, block, prog_ctx, state, loc);
    let (at_v, at_d) = eval_at_pair(ov.at_combined.as_ref(), block, prog_ctx, state, loc)?;
    let (lk, lv) = eval_coord(&ov.line, block, prog_ctx, state, loc)?;
    let (ck, cv) = eval_coord(&ov.col, block, prog_ctx, state, loc)?;
    let bits_v = emit_i32(ov.attr_bits as i32, block, prog_ctx, state, loc);
    let no_upd_v = emit_i32(i32::from(ov.force_no_update), block, prog_ctx, state, loc);
    let fg_v = eval_color(&ov.fg_color, block, prog_ctx, state, loc)?;
    let bg_v = eval_color(&ov.bg_color, block, prog_ctx, state, loc)?;
    let prompt_v = eval_prompt_i32(&ov.prompt, block, prog_ctx, state, loc)?;

    let field_w = pl.effective_ty.static_byte_size().max(1);
    let w_v = eval_width_i32(ov.size_expr.as_ref(), field_w, block, prog_ctx, state, loc)?;
    let has_upd = (ov.attr_bits & ATTR_UPDATE_BIT) != 0 && !ov.force_no_update;
    let preload_v = emit_i32(i32::from(has_upd), block, prog_ctx, state, loc);

    let dst_v = lower_place_view(pl, block, prog_ctx, state)?;
    let (sp_ptr, sp_len) = emit_spec_slice(&dst_v.spec, block, prog_ctx, state, loc)?;
    let src_e = HirExpr::Place(pl.clone());
    let (init_ptr, init_len) = format_expr_into_screen_ty(
        &src_e,
        &pl.effective_ty,
        field_w,
        block,
        prog_ctx,
        state,
        loc,
    )?;

    let _ = emit_rt_call(
        RtDispatchTarget::ScreenStageEditItem,
        &[
            neg1,
            at_v,
            at_d,
            lk,
            lv,
            ck,
            cv,
            bits_v,
            no_upd_v,
            fg_v,
            bg_v,
            prompt_v,
            w_v,
            preload_v,
            init_ptr,
            init_len,
            dst_v.ptr,
            dst_v.len_i64,
            sp_ptr,
            sp_len,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Formats `expr` through `screen_ty` (`byte_width` bytes) into a stack buffer `(ptr, len_i64)`.
fn format_expr_into_screen_ty<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    screen_ty: &HirType,
    byte_width: u32,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let w = byte_width.max(screen_ty.static_byte_size()).max(1);
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i8_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 8).into();
    let len_v = emit_i64(i64::from(w), block, prog_ctx, state, loc);
    let buf_ptr = append_val(
        block,
        llvm::alloca(
            state.ctx.raw(),
            len_v,
            ptr_ty,
            loc,
            llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i8_ty))),
        )
        .into(),
        len_v,
    );
    let spec = make_runtime_field_spec(screen_ty.clone(), prog_ctx.prog.formatting);
    let spec_bytes = encode_field_spec(&spec);
    let (ds_ptr, ds_len) = emit_const_slice(&spec_bytes, block, prog_ctx, state, loc)?;

    if matches!(
        screen_ty.category(),
        DataCategory::Numeric | DataCategory::NumericEdited
    ) && !matches!(expr, HirExpr::Place(_) | HirExpr::Const { .. })
    {
        let _ = emit_rt_call(RtDispatchTarget::DecBegin, &[], block, prog_ctx, state, loc)?;
        emit_decimal_expr(expr, block, prog_ctx, state)?;
        let z0 = emit_i32(0, block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::DecStore,
            &[buf_ptr, len_v, ds_ptr, ds_len, z0, z0, z0],
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
                k, s_ptr, s_len, ss_ptr, ss_len, buf_ptr, len_v, ds_ptr, ds_len,
            ],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok((buf_ptr, len_v))
}

/// Formats an arbitrary ad-hoc `DISPLAY` operand into `(ptr, len_i64, default_width)`.
fn format_adhoc_display_expr<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>, u32)> {
    match expr {
        HirExpr::Const {
            value: HirConstValue::Bytes(bytes),
            ..
        } => {
            let w = u32::try_from(bytes.len()).unwrap_or(1).max(1);
            let (p, l) = emit_const_slice(bytes, block, prog_ctx, state, loc)?;
            Ok((p, l, w))
        }
        HirExpr::Const {
            value: HirConstValue::Decimal { text, .. },
            ..
        } => {
            let s = state.interner.resolve_original(*text).unwrap_or("0");
            let w = u32::try_from(s.len()).unwrap_or(1).max(1);
            let (p, l) = emit_const_slice(s.as_bytes(), block, prog_ctx, state, loc)?;
            Ok((p, l, w))
        }
        HirExpr::Place(pl) => {
            if matches!(
                pl.effective_ty,
                HirType::Alphanumeric { .. }
                    | HirType::Alphabetic { .. }
                    | HirType::AlphanumericEdited { .. }
                    | HirType::Group { .. }
            ) {
                let v = lower_place_view(pl, block, prog_ctx, state)?;
                let w = pl.effective_ty.static_byte_size().max(1);
                Ok((v.ptr, v.len_i64, w))
            } else {
                let w = pl.effective_ty.static_byte_size().max(1);
                let disp_ty = match &pl.effective_ty {
                    HirType::BinaryInt {
                        digits,
                        scale,
                        sign,
                        ..
                    }
                    | HirType::PackedDecimal {
                        digits,
                        scale,
                        sign,
                        ..
                    } => HirType::ZonedDecimal {
                        digits: *digits,
                        scale: *scale,
                        sign: match sign {
                            Signedness::Unsigned => NumericSignSpec::Unsigned,
                            Signedness::Signed => NumericSignSpec::SeparateLeading,
                        },
                        blank_zero: BlankZeroSpec::RetainZero,
                    },
                    other => other.clone(),
                };
                let dw = disp_ty.static_byte_size().max(w);
                let (p, l) =
                    format_expr_into_screen_ty(expr, &disp_ty, dw, block, prog_ctx, state, loc)?;
                Ok((p, l, dw))
            }
        }
        _ => {
            let alpha_ty = HirType::Alphanumeric {
                byte_len: 32,
                justified: JustifiedSpec::Left,
            };
            let (p, l) =
                format_expr_into_screen_ty(expr, &alpha_ty, 32, block, prog_ctx, state, loc)?;
            Ok((p, l, 32))
        }
    }
}

/// Evaluates `HirScreenCoordSpec` into `(kind_i32, val_i64)`.
fn eval_coord<'ctx, 'b, 'e: 'b>(
    coord: &HirScreenCoordSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    match coord {
        HirScreenCoordSpec::Omitted => Ok((
            emit_i32(0, block, prog_ctx, state, loc),
            emit_i64(0, block, prog_ctx, state, loc),
        )),
        HirScreenCoordSpec::Absolute(n) => Ok((
            emit_i32(1, block, prog_ctx, state, loc),
            emit_i64(i64::from(*n), block, prog_ctx, state, loc),
        )),
        HirScreenCoordSpec::Plus(d) => Ok((
            emit_i32(2, block, prog_ctx, state, loc),
            emit_i64(i64::from(*d), block, prog_ctx, state, loc),
        )),
        HirScreenCoordSpec::Minus(d) => Ok((
            emit_i32(3, block, prog_ctx, state, loc),
            emit_i64(i64::from(*d), block, prog_ctx, state, loc),
        )),
        HirScreenCoordSpec::Dynamic(e) => {
            let v = eval_expr_as_i64(e, block, prog_ctx, state)?;
            Ok((emit_i32(1, block, prog_ctx, state, loc), v))
        }
        HirScreenCoordSpec::DynamicPlus(e) => {
            let v = eval_expr_as_i64(e, block, prog_ctx, state)?;
            Ok((emit_i32(2, block, prog_ctx, state, loc), v))
        }
        HirScreenCoordSpec::DynamicMinus(e) => {
            let v = eval_expr_as_i64(e, block, prog_ctx, state)?;
            Ok((emit_i32(3, block, prog_ctx, state, loc), v))
        }
    }
}

/// Evaluates optional `AT (expr, digits)` into `(at_val_i64, at_digits_i32)`.
fn eval_at_pair<'ctx, 'b, 'e: 'b>(
    at_opt: Option<&(HirExpr, u8)>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    if let Some((e, digits)) = at_opt {
        let v = eval_expr_as_i64(e, block, prog_ctx, state)?;
        let d = emit_i32(i32::from(*digits), block, prog_ctx, state, loc);
        Ok((v, d))
    } else {
        Ok((
            emit_i64(-1, block, prog_ctx, state, loc),
            emit_i32(4, block, prog_ctx, state, loc),
        ))
    }
}

/// Evaluates `HirScreenColorSpec` into an `i32` (`-1` when default, `0..=15` otherwise).
fn eval_color<'ctx, 'b, 'e: 'b>(
    col: &HirScreenColorSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    match col {
        HirScreenColorSpec::Default => Ok(emit_i32(-1, block, prog_ctx, state, loc)),
        HirScreenColorSpec::Static(c) => Ok(emit_i32(i32::from(*c), block, prog_ctx, state, loc)),
        HirScreenColorSpec::Dynamic(e) => {
            let v64 = eval_expr_as_i64(e, block, prog_ctx, state)?;
            let i32_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 32).into();
            Ok(append_val(
                block,
                arith::trunci(v64, i32_ty, loc).into(),
                v64,
            ))
        }
    }
}

/// Merges child color over inherited parent color.
fn merge_color(child: &HirScreenColorSpec, parent: &HirScreenColorSpec) -> HirScreenColorSpec {
    if matches!(child, HirScreenColorSpec::Default) {
        parent.clone()
    } else {
        child.clone()
    }
}

/// Evaluates optional `SIZE expr` override (`fallback_w` when omitted).
fn eval_width_i32<'ctx, 'b, 'e: 'b>(
    size_expr: Option<&HirExpr>,
    fallback_w: u32,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    if let Some(se) = size_expr {
        let v64 = eval_expr_as_i64(se, block, prog_ctx, state)?;
        let i32_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 32).into();
        Ok(append_val(
            block,
            arith::trunci(v64, i32_ty, loc).into(),
            v64,
        ))
    } else {
        Ok(emit_i32(
            i32::try_from(fallback_w.max(1)).unwrap_or(1),
            block,
            prog_ctx,
            state,
            loc,
        ))
    }
}

/// Evaluates `HirScreenPromptSpec` into `i32` (`-1` when disabled, `0..=255` byte when enabled).
fn eval_prompt_i32<'ctx, 'b, 'e: 'b>(
    prompt: &HirScreenPromptSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    match prompt {
        HirScreenPromptSpec::Disabled => Ok(emit_i32(-1, block, prog_ctx, state, loc)),
        HirScreenPromptSpec::StaticByte(b) => {
            Ok(emit_i32(i32::from(*b), block, prog_ctx, state, loc))
        }
        HirScreenPromptSpec::Dynamic(pl) => {
            let v = lower_place_view(pl, block, prog_ctx, state)?;
            let i8_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 8).into();
            let i32_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 32).into();
            let b8 = append_val(
                block,
                llvm::load(
                    state.ctx.raw(),
                    v.ptr,
                    i8_ty,
                    loc,
                    llvm::LoadStoreOptions::new(),
                )
                .into(),
                v.ptr,
            );
            Ok(append_val(block, arith::extui(b8, i32_ty, loc).into(), b8))
        }
    }
}

/// Resolves `(cursor_ptr, cursor_len, crt_ptr, crt_len, cob_crt_ptr)` for `ScreenExecute*`.
fn emit_special_reg_ptrs<'ctx, 'b, 'e: 'b>(
    regs: &HirScreenSpecialRegs,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
)> {
    let null_p = emit_null_ptr(block, prog_ctx, state, loc);
    let z0 = emit_i64(0, block, prog_ctx, state, loc);
    let (cur_p, cur_l) = if let Some(pl) = &regs.cursor_place {
        let v = lower_place_view(pl, block, prog_ctx, state)?;
        (v.ptr, v.len_i64)
    } else {
        (null_p, z0)
    };
    let (crt_p, crt_l) = if let Some(pl) = &regs.crt_status_place {
        let v = lower_place_view(pl, block, prog_ctx, state)?;
        (v.ptr, v.len_i64)
    } else {
        (null_p, z0)
    };
    let cob_p = if let Some(pl) = &regs.cob_crt_status_place {
        lower_place_view(pl, block, prog_ctx, state)?.ptr
    } else {
        null_p
    };
    Ok((cur_p, cur_l, crt_p, crt_l, cob_p))
}

/// Writes an `i64` stack slot value (`*slot_ptr`) back into `dst_place` via `DecBegin` + `DecStore`!
fn emit_store_i64_slot_into_place<'ctx, 'b, 'e: 'b>(
    slot_ptr: Value<'ctx, 'b>,
    dst_place: &HirPlace,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let bin64_ty = HirType::BinaryInt {
        byte_width: 8,
        digits: DigitCount::try_new(18).unwrap_or(DigitCount::ONE),
        scale: ScaleFactor::ZERO,
        sign: Signedness::Signed,
        endian: coblang_hir::BinaryEndianness::Native,
        trunc: coblang_hir::TruncationPolicy::FullBinaryRange,
    };
    let src_spec = make_runtime_field_spec(bin64_ty, prog_ctx.prog.formatting);
    let src_spec_bytes = encode_field_spec(&src_spec);
    let (ss_ptr, ss_len) = emit_const_slice(&src_spec_bytes, block, prog_ctx, state, loc)?;
    let eight_i64 = emit_i64(8, block, prog_ctx, state, loc);

    let dst_v = lower_place_view(dst_place, block, prog_ctx, state)?;
    let (ds_ptr, ds_len) = emit_spec_slice(&dst_v.spec, block, prog_ctx, state, loc)?;
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
            k,
            slot_ptr,
            eight_i64,
            ss_ptr,
            ss_len,
            dst_v.ptr,
            dst_v.len_i64,
            ds_ptr,
            ds_len,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Emits `scf.if (exc != 0)` for `ON EXCEPTION` / `NOT ON EXCEPTION` statement blocks.
fn emit_exception_branches<'ctx, 'b, 'e: 'b>(
    exc: Value<'ctx, 'b>,
    on_exception: &[HirStmt],
    not_on_exception: &[HirStmt],
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    if on_exception.is_empty() && not_on_exception.is_empty() {
        return Ok(());
    }
    let z0 = emit_i32(0, block, prog_ctx, state, loc);
    let cond = append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, exc, z0, loc).into(),
        z0,
    );
    let then_reg = Region::new();
    let else_reg = Region::new();
    let tb = then_reg.append_block(Block::new(&[]));
    for s in on_exception {
        lower_stmt(s, &tb, prog_ctx, state)?;
    }
    let _ = tb.append_operation(scf::r#yield(&[], loc));
    let eb = else_reg.append_block(Block::new(&[]));
    for s in not_on_exception {
        lower_stmt(s, &eb, prog_ctx, state)?;
    }
    let _ = eb.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(cond, &[], then_reg, else_reg, loc));
    Ok(())
}
