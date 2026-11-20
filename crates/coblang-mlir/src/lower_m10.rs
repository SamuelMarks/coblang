//! Milestone M10 (`STRING`, `UNSTRING`, `INSPECT`, `TRANSFORM`, `ACCEPT`, and `DISPLAY UPON ENVIRONMENT-*`)
//! HIR-to-`cobol` MLIR lowering helpers.

use coblang_common::{DigitCount, FeatureTag, Result, ScaleFactor, SourceSpan};
use coblang_hir::{
    BinaryEndianness, HirAcceptSourceKind, HirAcceptSpec, HirAcceptTarget, HirConstValue,
    HirEnvTargetKind, HirExpr, HirInspectAction, HirInspectQualifier, HirInspectReplaceClause,
    HirInspectSpec, HirInspectTallyClause, HirInspectWindow, HirPlace, HirStringDelim,
    HirStringSpec, HirType, HirUnstringMode, HirUnstringSpec, HirUnstringTargetSpec, Signedness,
    TruncationPolicy,
};
use coblang_rt_core::{
    accept_env::{EnvDisplayTarget, RuntimeAcceptSource},
    field_ops::DecimalArithOp,
    m10_staging::{InspectMatchKind, encode_inspect_match_kind},
};
use melior::dialect::arith::CmpiPredicate;
use melior::dialect::{arith, llvm, scf};
use melior::ir::block::BlockLike;
use melior::ir::{Block, Location, Region, RegionLike, Value};

use crate::lower_from_hir::{
    ModuleLowerState, ProgramLowerCtx, append_val, const_to_display_bytes, emit_const_slice,
    emit_display_operand, emit_i32, emit_i64, emit_rt_call, emit_size_error_branch,
    emit_spec_slice, emit_storage_ptr, eval_expr_as_i64, expr_span, lower_intrinsic_as_move_source,
    lower_place_view, unimpl,
};
use crate::ops::{RtDispatchTarget, llvm_ptr_type};
use crate::storage_init::make_runtime_field_spec;

/// Lowers `HirStmt::DisplayEnv` via `DispBegin` -> `DispPush*` -> `DispFlushEnv`.
///
/// # Arguments
///
/// * `operands` - Displayed operand expressions.
/// * `target` - Environment or argument register selector.
/// * `span` - Statement source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if operand lowering fails.
pub(crate) fn lower_display_env_stmt<'ctx, 'b, 'e: 'b>(
    operands: &[HirExpr],
    target: HirEnvTargetKind,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let _ = emit_rt_call(
        RtDispatchTarget::DispBegin,
        &[],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    for op_expr in operands {
        emit_display_operand(op_expr, block, prog_ctx, state, loc)?;
    }
    let code = map_env_target(target).to_abi_code();
    let code_v = emit_i32(code, block, prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::DispFlushEnv,
        &[code_v],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Lowers `HirStmt::Accept` via `@coblang_rt_accept` + optional `ON EXCEPTION` `scf.if`.
///
/// # Arguments
///
/// * `spec` - Resolved [`HirAcceptSpec`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if place or exception-handler lowering fails.
pub(crate) fn lower_accept_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirAcceptSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let src_v = emit_i32(
        map_accept_source(spec.source).to_abi_code(),
        block,
        prog_ctx,
        state,
        loc,
    );
    let (omit_v, dst_ptr, dst_len, sp_ptr, sp_len) = match &spec.target {
        HirAcceptTarget::Omitted => {
            let (empty_p, zero_l) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
            let one = emit_i32(1, block, prog_ctx, state, loc);
            (one, empty_p, zero_l, empty_p, zero_l)
        }
        HirAcceptTarget::Place(pl) => {
            let pv = lower_place_view(pl, block, prog_ctx, state)?;
            let (s_ptr, s_len) = emit_spec_slice(&pv.spec, block, prog_ctx, state, loc)?;
            let zero = emit_i32(0, block, prog_ctx, state, loc);
            (zero, pv.ptr, pv.len_i64, s_ptr, s_len)
        }
    };
    let st = emit_rt_call(
        RtDispatchTarget::Accept,
        &[src_v, omit_v, dst_ptr, dst_len, sp_ptr, sp_len],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    if !spec.on_exception.is_empty() || !spec.not_on_exception.is_empty() {
        let zero_32 = emit_i32(0, block, prog_ctx, state, loc);
        let cond = append_val(
            block,
            arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, st, zero_32, loc),
            prog_ctx.fallback_val,
        );
        emit_size_error_branch(
            cond,
            &spec.on_exception,
            &spec.not_on_exception,
            loc,
            block,
            prog_ctx,
            state,
        )?;
    }
    Ok(())
}

/// Lowers `HirStmt::StringVerb` via `@coblang_rt_str_*`, `WITH POINTER`, and `ON OVERFLOW`.
///
/// # Arguments
///
/// * `spec` - Resolved [`HirStringSpec`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if piece, pointer, or handler lowering fails.
pub(crate) fn lower_string_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirStringSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let init_ptr = match &spec.pointer {
        Some(p_pl) => eval_expr_as_i64(&HirExpr::Place(p_pl.clone()), block, prog_ctx, state)?,
        None => emit_i64(1, block, prog_ctx, state, loc),
    };
    let _ = emit_rt_call(RtDispatchTarget::StrBegin, &[], block, prog_ctx, state, loc)?;
    for (src_expr, delim) in &spec.pieces {
        emit_string_delim(delim, block, prog_ctx, state, loc)?;
        let (src_ptr, src_len, spec_ptr, spec_len) =
            lower_char_operand_slices(src_expr, block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::StrPushSrc,
            &[src_ptr, src_len, spec_ptr, spec_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    let into_pv = lower_place_view(&spec.into, block, prog_ctx, state)?;
    if matches!(
        spec.into.effective_ty,
        HirType::National { .. } | HirType::NationalEdited { .. } | HirType::Utf8 { .. }
    ) {
        let (into_sp_p, into_sp_l) = emit_spec_slice(&into_pv.spec, block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::StrSetDstSpec,
            &[into_sp_p, into_sp_l],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    let st = emit_rt_call(
        RtDispatchTarget::StrExec,
        &[into_pv.ptr, into_pv.len_i64, init_ptr],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    if let Some(p_pl) = &spec.pointer {
        let last_ptr = emit_rt_call(
            RtDispatchTarget::StrLastPtr,
            &[],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        store_i64_to_place(last_ptr, p_pl, block, prog_ctx, state, loc)?;
    }
    if !spec.on_overflow.is_empty() || !spec.not_on_overflow.is_empty() {
        let zero_32 = emit_i32(0, block, prog_ctx, state, loc);
        let cond = append_val(
            block,
            arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, st, zero_32, loc),
            prog_ctx.fallback_val,
        );
        emit_size_error_branch(
            cond,
            &spec.on_overflow,
            &spec.not_on_overflow,
            loc,
            block,
            prog_ctx,
            state,
        )?;
    }
    Ok(())
}

/// Lowers `HirStmt::Unstring` via `@coblang_rt_unstr_*`, `COUNT IN`, `POINTER`, `TALLYING`, and `ON OVERFLOW`.
///
/// # Arguments
///
/// * `spec` - Resolved [`HirUnstringSpec`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if source, delimiter, target, or handler lowering fails.
pub(crate) fn lower_unstring_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirUnstringSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let init_ptr = match &spec.pointer {
        Some(p_pl) => eval_expr_as_i64(&HirExpr::Place(p_pl.clone()), block, prog_ctx, state)?,
        None => emit_i64(1, block, prog_ctx, state, loc),
    };
    let src_pv = lower_place_view(&spec.source, block, prog_ctx, state)?;
    let (src_sp_ptr, src_sp_len) = emit_spec_slice(&src_pv.spec, block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::UnstrBegin,
        &[src_pv.ptr, src_pv.len_i64, src_sp_ptr, src_sp_len, init_ptr],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    for (mode, pat_expr) in &spec.delimiters {
        let is_all = emit_i32(
            match mode {
                HirUnstringMode::Single => 0,
                HirUnstringMode::AllRepeated => 1,
            },
            block,
            prog_ctx,
            state,
            loc,
        );
        let (p_ptr, p_len, p_sp_ptr, p_sp_len) =
            lower_char_operand_slices(pat_expr, block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::UnstrPushDelim,
            &[is_all, p_ptr, p_len, p_sp_ptr, p_sp_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }

    for tgt in &spec.targets {
        lower_unstring_target_step(tgt, block, prog_ctx, state, loc)?;
    }

    let st = emit_rt_call(
        RtDispatchTarget::UnstrFinish,
        &[],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    if let Some(p_pl) = &spec.pointer {
        let last_ptr = emit_rt_call(
            RtDispatchTarget::StrLastPtr,
            &[],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        store_i64_to_place(last_ptr, p_pl, block, prog_ctx, state, loc)?;
    }
    if let Some(t_pl) = &spec.tallying {
        let delta = emit_rt_call(
            RtDispatchTarget::UnstrTallyDelta,
            &[],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        add_i64_delta_to_place(delta, t_pl, block, prog_ctx, state, loc)?;
    }
    if !spec.on_overflow.is_empty() || !spec.not_on_overflow.is_empty() {
        let zero_32 = emit_i32(0, block, prog_ctx, state, loc);
        let cond = append_val(
            block,
            arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, st, zero_32, loc),
            prog_ctx.fallback_val,
        );
        emit_size_error_branch(
            cond,
            &spec.on_overflow,
            &spec.not_on_overflow,
            loc,
            block,
            prog_ctx,
            state,
        )?;
    }
    Ok(())
}

/// Lowers one `INTO` target triplet inside `UNSTRING`.
///
/// # Arguments
///
/// * `tgt` - Target `INTO` / `DELIMITER IN` / `COUNT IN` triplet.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_unstring_target_step<'ctx, 'b, 'e: 'b>(
    tgt: &HirUnstringTargetSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let into_pv = lower_place_view(&tgt.into, block, prog_ctx, state)?;
    let (into_sp_ptr, into_sp_len) = emit_spec_slice(&into_pv.spec, block, prog_ctx, state, loc)?;
    let (delim_ptr, delim_len) = if let Some(delim_place) = &tgt.delimiter_in {
        let delim_view = lower_place_view(delim_place, block, prog_ctx, state)?;
        let (d_sp_ptr, d_sp_len) = emit_spec_slice(&delim_view.spec, block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::UnstrSetDelimSpec,
            &[d_sp_ptr, d_sp_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        (delim_view.ptr, delim_view.len_i64)
    } else {
        let null_p = append_val(
            block,
            llvm::zero(llvm_ptr_type(state.ctx), loc),
            prog_ctx.fallback_val,
        );
        let z64 = emit_i64(0, block, prog_ctx, state, loc);
        (null_p, z64)
    };
    let cnt_i64 = emit_rt_call(
        RtDispatchTarget::UnstrStep,
        &[
            into_pv.ptr,
            into_pv.len_i64,
            into_sp_ptr,
            into_sp_len,
            delim_ptr,
            delim_len,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    if let Some(c_pl) = &tgt.count_in {
        let z64 = emit_i64(0, block, prog_ctx, state, loc);
        let acted = append_val(
            block,
            arith::cmpi(state.ctx.raw(), CmpiPredicate::Sge, cnt_i64, z64, loc),
            prog_ctx.fallback_val,
        );
        let then_reg = Region::new();
        let then_blk = then_reg.append_block(Block::new(&[]));
        store_i64_to_place(cnt_i64, c_pl, &then_blk, prog_ctx, state, loc)?;
        let _ = then_blk.append_operation(scf::r#yield(&[], loc));
        let else_reg = Region::new();
        let else_blk = else_reg.append_block(Block::new(&[]));
        let _ = else_blk.append_operation(scf::r#yield(&[], loc));
        let _ = block.append_operation(scf::r#if(acted, &[], then_reg, else_reg, loc));
    }
    Ok(())
}

/// Lowers `HirStmt::Inspect` (`TALLYING`, `REPLACING`, `CONVERTING`, and `TRANSFORM`).
///
/// # Arguments
///
/// * `spec` - Resolved [`HirInspectSpec`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if target, operand, or counter lowering fails.
pub(crate) fn lower_inspect_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirInspectSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let t_pv = lower_place_view(&spec.target, block, prog_ctx, state)?;
    let (t_sp_ptr, t_sp_len) = emit_spec_slice(&t_pv.spec, block, prog_ctx, state, loc)?;

    match &spec.action {
        HirInspectAction::TallyAndReplace {
            counters,
            tallies,
            replaces,
        } => lower_inspect_tally_replace(
            (t_pv.ptr, t_pv.len_i64, t_sp_ptr, t_sp_len),
            (counters, tallies, replaces),
            block,
            prog_ctx,
            state,
            loc,
        ),
        HirInspectAction::Converting(conv) => {
            emit_inspect_window(&conv.window, block, prog_ctx, state, loc)?;
            emit_inspect_reg(0, &conv.from, block, prog_ctx, state, loc)?;
            emit_inspect_reg(1, &conv.to, block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::InspConvert,
                &[t_pv.ptr, t_pv.len_i64, t_sp_ptr, t_sp_len],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            Ok(())
        }
    }
}

/// Lowers Format 1/2/3 `INSPECT` `TALLYING` and/or `REPLACING` clauses.
///
/// # Arguments
///
/// * `target_view` - `(target_ptr, target_len, spec_ptr, spec_len)` SSA tuple.
/// * `clauses` - `(counters, tallies, replaces)` slices.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_inspect_tally_replace<'ctx, 'b, 'e: 'b>(
    target_view: (
        Value<'ctx, 'b>,
        Value<'ctx, 'b>,
        Value<'ctx, 'b>,
        Value<'ctx, 'b>,
    ),
    clauses: (
        &[HirPlace],
        &[HirInspectTallyClause],
        &[HirInspectReplaceClause],
    ),
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let (t_ptr, t_len, t_sp_ptr, t_sp_len) = target_view;
    let (counters, tallies, replaces) = clauses;
    let cnt_n = emit_i64(
        i64::try_from(counters.len()).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let _ = emit_rt_call(
        RtDispatchTarget::InspBegin,
        &[cnt_n],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    for tc in tallies {
        emit_inspect_window(&tc.window, block, prog_ctx, state, loc)?;
        if let Some(pat_expr) = &tc.pattern {
            emit_inspect_reg(0, pat_expr, block, prog_ctx, state, loc)?;
        }
        let slot_v = emit_i64(
            i64::try_from(tc.counter_idx).unwrap_or(0),
            block,
            prog_ctx,
            state,
            loc,
        );
        let kind_v = emit_i32(
            encode_inspect_match_kind(map_inspect_qual(tc.qualifier)),
            block,
            prog_ctx,
            state,
            loc,
        );
        let _ = emit_rt_call(
            RtDispatchTarget::InspPushTally,
            &[slot_v, kind_v],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    for rc in replaces {
        emit_inspect_window(&rc.window, block, prog_ctx, state, loc)?;
        if let Some(pat_expr) = &rc.pattern {
            emit_inspect_reg(0, pat_expr, block, prog_ctx, state, loc)?;
        }
        emit_inspect_reg(1, &rc.by, block, prog_ctx, state, loc)?;
        let kind_v = emit_i32(
            encode_inspect_match_kind(map_inspect_qual(rc.qualifier)),
            block,
            prog_ctx,
            state,
            loc,
        );
        let _ = emit_rt_call(
            RtDispatchTarget::InspPushReplace,
            &[kind_v],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    let _ = emit_rt_call(
        RtDispatchTarget::InspExec,
        &[t_ptr, t_len, t_sp_ptr, t_sp_len],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    for (slot_idx, c_pl) in counters.iter().enumerate() {
        let slot_v = emit_i64(
            i64::try_from(slot_idx).unwrap_or(0),
            block,
            prog_ctx,
            state,
            loc,
        );
        let delta = emit_rt_call(
            RtDispatchTarget::InspGetDelta,
            &[slot_v],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        add_i64_delta_to_place(delta, c_pl, block, prog_ctx, state, loc)?;
    }
    Ok(())
}

/// Emits `StrSetDelim` for a single `STRING` piece's [`HirStringDelim`].
///
/// # Arguments
///
/// * `delim` - Piece delimiter specification.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_string_delim<'ctx, 'b, 'e: 'b>(
    delim: &HirStringDelim,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let (empty_p, zero_l) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
    let (kind_v, p_ptr, p_len, sp_ptr, sp_len) = match delim {
        HirStringDelim::Size => (
            emit_i32(0, block, prog_ctx, state, loc),
            empty_p,
            zero_l,
            empty_p,
            zero_l,
        ),
        HirStringDelim::Pattern(pat_expr) => {
            if let HirExpr::Const {
                value: HirConstValue::Figurative(fig),
                ..
            } = pat_expr
                && let Some(b) = crate::lower_m17::figurative_single_byte_m17(
                    fig,
                    prog_ctx.prog.formatting,
                    false,
                )
            {
                let (bp, bl) = emit_const_slice(&[b], block, prog_ctx, state, loc)?;
                (
                    emit_i32(2, block, prog_ctx, state, loc),
                    bp,
                    bl,
                    empty_p,
                    zero_l,
                )
            } else {
                let (p, l, sp, sl) =
                    lower_char_operand_slices(pat_expr, block, prog_ctx, state, loc)?;
                (emit_i32(1, block, prog_ctx, state, loc), p, l, sp, sl)
            }
        }
    };
    let _ = emit_rt_call(
        RtDispatchTarget::StrSetDelim,
        &[kind_v, p_ptr, p_len, sp_ptr, sp_len],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Emits `InspClearWin` + optional `InspSetWin` calls for `window`.
///
/// # Arguments
///
/// * `window` - `AFTER`/`BEFORE` window specification.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_inspect_window<'ctx, 'b, 'e: 'b>(
    window: &HirInspectWindow,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let _ = emit_rt_call(
        RtDispatchTarget::InspClearWin,
        &[],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    if let Some(after_expr) = &window.after {
        let edge_v = emit_i32(0, block, prog_ctx, state, loc);
        let (ok, p, l, sp, sl) =
            lower_inspect_operand_args(after_expr, block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::InspSetWin,
            &[edge_v, ok, p, l, sp, sl],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    if let Some(before_expr) = &window.before {
        let edge_v = emit_i32(1, block, prog_ctx, state, loc);
        let (ok, p, l, sp, sl) =
            lower_inspect_operand_args(before_expr, block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::InspSetWin,
            &[edge_v, ok, p, l, sp, sl],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Emits `InspSetOp` for register `slot` (`0` = pattern/`FROM`, `1` = `BY`/`TO`).
///
/// # Arguments
///
/// * `slot` - Register selector (`0` or `1`).
/// * `expr` - Operand expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_inspect_reg<'ctx, 'b, 'e: 'b>(
    slot: i32,
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let slot_v = emit_i32(slot, block, prog_ctx, state, loc);
    let (ok, p, l, sp, sl) = lower_inspect_operand_args(expr, block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::InspSetOp,
        &[slot_v, ok, p, l, sp, sl],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Lowers an `INSPECT` operand into `(op_kind_i32, ptr, len_i64, spec_ptr, spec_len_i64)`.
///
/// # Arguments
///
/// * `expr` - Operand expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_inspect_operand_args<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
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
    if let HirExpr::Const {
        value: HirConstValue::Figurative(fig),
        ..
    } = expr
        && let Some(b) =
            crate::lower_m17::figurative_single_byte_m17(fig, prog_ctx.prog.formatting, false)
    {
        let (empty_p, zero_l) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
        let (bp, bl) = emit_const_slice(&[b], block, prog_ctx, state, loc)?;
        return Ok((
            emit_i32(1, block, prog_ctx, state, loc),
            bp,
            bl,
            empty_p,
            zero_l,
        ));
    }
    let (p, l, sp, sl) = lower_char_operand_slices(expr, block, prog_ctx, state, loc)?;
    Ok((emit_i32(0, block, prog_ctx, state, loc), p, l, sp, sl))
}

/// Lowers a character operand `expr` into `(ptr, len_i64, spec_ptr, spec_len_i64)`.
///
/// # Arguments
///
/// * `expr` - Character expression (`Place`, `Const`, or `FUNCTION TRIM`).
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_char_operand_slices<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
)> {
    let (empty_p, zero_l) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
    match expr {
        HirExpr::Place(pl) => {
            let pv = lower_place_view(pl, block, prog_ctx, state)?;
            let (sp, sl) = emit_spec_slice(&pv.spec, block, prog_ctx, state, loc)?;
            Ok((pv.ptr, pv.len_i64, sp, sl))
        }
        HirExpr::Const { value, .. } => {
            let bytes = const_to_display_bytes(value, state.interner);
            let (p, l) = emit_const_slice(&bytes, block, prog_ctx, state, loc)?;
            Ok((p, l, empty_p, zero_l))
        }
        HirExpr::CallFunction { name, args, span } => {
            let (_, p, l, _, _) =
                lower_intrinsic_as_move_source(*name, args, *span, block, prog_ctx, state, loc)?;
            Ok((p, l, empty_p, zero_l))
        }
        HirExpr::M12Call(c) => {
            if let Some((_, p, l, _, _)) =
                crate::lower_m20::try_lower_m12_call_move_source(c, block, prog_ctx, state, loc)?
            {
                return Ok((p, l, empty_p, zero_l));
            }
            let (_, p, l, sp, sl) =
                crate::lower_m12::lower_m12_call_move_source(c, block, prog_ctx, state, loc)?;
            Ok((p, l, sp, sl))
        }
        other => Err(unimpl(
            FeatureTag::FeatureStringInspect,
            format!("character operand `{other:?}`"),
            state.source_map,
            expr_span(other),
        )),
    }
}

/// Stores a signed `i64` SSA value into any integer [`HirPlace`] via `idx_scratch` + `DecimalEvalStack`.
///
/// # Arguments
///
/// * `val_i64` - `i64` value to store.
/// * `place` - Destination integer place (`Zoned`, `Packed`, `BinaryInt`, or `Index`).
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
pub(crate) fn store_i64_to_place<'ctx, 'b, 'e: 'b>(
    val_i64: Value<'ctx, 'b>,
    place: &HirPlace,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let pv = lower_place_view(place, block, prog_ctx, state)?;
    let (dst_sp_p, dst_sp_l) = emit_spec_slice(&pv.spec, block, prog_ctx, state, loc)?;
    let (scr_ptr, eight, scr_sp_p, scr_sp_l) =
        stage_i64_scratch(val_i64, block, prog_ctx, state, loc)?;
    let z0 = emit_i32(0, block, prog_ctx, state, loc);
    let _ = emit_rt_call(RtDispatchTarget::DecBegin, &[], block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::DecPushField,
        &[scr_ptr, eight, scr_sp_p, scr_sp_l],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = emit_rt_call(
        RtDispatchTarget::DecStore,
        &[pv.ptr, pv.len_i64, dst_sp_p, dst_sp_l, z0, z0, z0],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Adds a signed `i64` delta onto `place` (`place = place + delta`) via `DecimalEvalStack`.
///
/// # Arguments
///
/// * `delta_i64` - `i64` increment to add.
/// * `place` - Counter/tally integer place.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn add_i64_delta_to_place<'ctx, 'b, 'e: 'b>(
    delta_i64: Value<'ctx, 'b>,
    place: &HirPlace,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let pv = lower_place_view(place, block, prog_ctx, state)?;
    let (dst_sp_p, dst_sp_l) = emit_spec_slice(&pv.spec, block, prog_ctx, state, loc)?;
    let (scr_ptr, eight, scr_sp_p, scr_sp_l) =
        stage_i64_scratch(delta_i64, block, prog_ctx, state, loc)?;
    let add_op = emit_i32(
        DecimalArithOp::Add.to_abi_code(),
        block,
        prog_ctx,
        state,
        loc,
    );
    let z0 = emit_i32(0, block, prog_ctx, state, loc);
    let _ = emit_rt_call(RtDispatchTarget::DecBegin, &[], block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::DecPushField,
        &[pv.ptr, pv.len_i64, dst_sp_p, dst_sp_l],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = emit_rt_call(
        RtDispatchTarget::DecPushField,
        &[scr_ptr, eight, scr_sp_p, scr_sp_l],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = emit_rt_call(
        RtDispatchTarget::DecBinop,
        &[add_op],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = emit_rt_call(
        RtDispatchTarget::DecStore,
        &[pv.ptr, pv.len_i64, dst_sp_p, dst_sp_l, z0, z0, z0],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Writes `val_i64` into `prog_ctx.idx_scratch_sym` and returns `(scratch_ptr, 8_i64, spec_ptr, spec_len)`.
///
/// # Arguments
///
/// * `val_i64` - `i64` SSA value to write.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn stage_i64_scratch<'ctx, 'b, 'e: 'b>(
    val_i64: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
)> {
    let scr_ptr = emit_storage_ptr(&prog_ctx.idx_scratch_sym, 0, 8, block, prog_ctx, state, loc)?;
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        val_i64,
        scr_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let scr_ty = HirType::BinaryInt {
        byte_width: 8,
        digits: DigitCount::try_new(18).unwrap_or(DigitCount::ONE),
        scale: ScaleFactor::ZERO,
        sign: Signedness::Signed,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::FullBinaryRange,
    };
    let scr_spec = make_runtime_field_spec(scr_ty, prog_ctx.prog.formatting);
    let (sp_p, sp_l) = emit_spec_slice(&scr_spec, block, prog_ctx, state, loc)?;
    let eight = emit_i64(8, block, prog_ctx, state, loc);
    Ok((scr_ptr, eight, sp_p, sp_l))
}

/// Maps [`HirEnvTargetKind`] to [`EnvDisplayTarget`].
///
/// # Arguments
///
/// * `target` - HIR environment target kind.
const fn map_env_target(target: HirEnvTargetKind) -> EnvDisplayTarget {
    match target {
        HirEnvTargetKind::EnvironmentName => EnvDisplayTarget::EnvironmentName,
        HirEnvTargetKind::EnvironmentValue => EnvDisplayTarget::EnvironmentValue,
        HirEnvTargetKind::ArgumentNumber => EnvDisplayTarget::ArgumentNumber,
        HirEnvTargetKind::CommandLine => EnvDisplayTarget::CommandLine,
    }
}

/// Maps [`HirAcceptSourceKind`] to [`RuntimeAcceptSource`].
///
/// # Arguments
///
/// * `src` - HIR accept source kind.
const fn map_accept_source(src: HirAcceptSourceKind) -> RuntimeAcceptSource {
    match src {
        HirAcceptSourceKind::ConsoleLine => RuntimeAcceptSource::ConsoleLine,
        HirAcceptSourceKind::DateYymmdd => RuntimeAcceptSource::DateYymmdd,
        HirAcceptSourceKind::DateYyyymmdd => RuntimeAcceptSource::DateYyyymmdd,
        HirAcceptSourceKind::DayYyddd => RuntimeAcceptSource::DayYyddd,
        HirAcceptSourceKind::DayYyyyddd => RuntimeAcceptSource::DayYyyyddd,
        HirAcceptSourceKind::DayOfWeek => RuntimeAcceptSource::DayOfWeek,
        HirAcceptSourceKind::TimeStandard8 => RuntimeAcceptSource::TimeStandard8,
        HirAcceptSourceKind::TimeAcu12 => RuntimeAcceptSource::TimeAcu12,
        HirAcceptSourceKind::MicrosecondTime12 => RuntimeAcceptSource::MicrosecondTime12,
        HirAcceptSourceKind::ArgumentNumber => RuntimeAcceptSource::ArgumentNumber,
        HirAcceptSourceKind::ArgumentValue => RuntimeAcceptSource::ArgumentValue,
        HirAcceptSourceKind::CommandLine => RuntimeAcceptSource::CommandLine,
        HirAcceptSourceKind::EnvironmentName => RuntimeAcceptSource::EnvironmentName,
        HirAcceptSourceKind::EnvironmentValue => RuntimeAcceptSource::EnvironmentValue,
    }
}

/// Maps [`HirInspectQualifier`] to [`InspectMatchKind`].
///
/// # Arguments
///
/// * `qual` - HIR inspect qualifier.
const fn map_inspect_qual(qual: HirInspectQualifier) -> InspectMatchKind {
    match qual {
        HirInspectQualifier::Characters => InspectMatchKind::Characters,
        HirInspectQualifier::All => InspectMatchKind::All,
        HirInspectQualifier::Leading => InspectMatchKind::Leading,
        HirInspectQualifier::First => InspectMatchKind::First,
        HirInspectQualifier::Trailing => InspectMatchKind::Trailing,
    }
}
