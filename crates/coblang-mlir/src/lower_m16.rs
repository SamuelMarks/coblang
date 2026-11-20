//! Milestone M16 (`SORT`, `MERGE`, `RELEASE`, `RETURN`, and `SORT-RETURN`) HIR-to-`cobol` MLIR lowering.

use coblang_common::{DataItemId, FileConnectorId, Result, SourceSpan};
use coblang_hir::{
    HirCloseDisposition, HirExpr, HirFileConnector, HirMergeFileStmt, HirOpenMode, HirOpenOption,
    HirRecordFormat, HirReleaseStmt, HirReturnStmt, HirSortDuplicates, HirSortFileStmt,
    HirSortInput, HirSortKeyDesc, HirSortOutput, HirSortTransferFile,
};
use coblang_rt_core::{
    desc_codec::{DecodedTableSortPlan, RuntimeTableSortKey, encode_table_sort_plan},
    fileio_wire::POST_IO_CONTINUE,
};
use melior::dialect::arith::CmpiPredicate;
use melior::dialect::{arith, llvm, scf};
use melior::ir::block::BlockLike;
use melior::ir::r#type::IntegerType;
use melior::ir::{Block, Location, Region, RegionLike, Value};

use crate::lower_from_hir::{
    ModuleLowerState, ProgramLowerCtx, append_val, emit_const_slice, emit_i32, emit_i64,
    emit_rt_call, emit_storage_ptr, eval_expr_as_i64, lower_place_view, lower_stmt,
};
use crate::lower_m10::store_i64_to_place;
use crate::lower_m14::{
    ABI_INLINE_HAS_ON, emit_connector_handles, emit_eq_i32, emit_null_ptr, emit_post_io_dispatch,
    emit_single_file_close, emit_single_file_open, encode_decl_mask, eval_active_write_len,
    eval_rel_key_i64, simple_item_place, sync_connector_registers,
};
use crate::ops::{CobolLoopOp, CobolPerformRangeOp, CobolYieldOp, RtDispatchTarget};
use crate::storage_init::make_runtime_field_spec;

/// Bit flag on `coblang_rt_sort_begin`: `MERGE` session (`1 << 0`).
const ABI_SORT_FLAG_MERGE: i32 = 1 << 0;
/// Bit flag on `coblang_rt_sort_begin`: `WITH DUPLICATES IN ORDER` (`1 << 1`).
const ABI_SORT_FLAG_DUPLICATES: i32 = 1 << 1;

///Lowers `HirStmt::SortFile` (`SORT sd ON ... {USING | INPUT PROCEDURE} {GIVING | OUTPUT PROCEDURE}`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirSortFileStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] on MLIR build failure.
pub(crate) fn lower_sort_file<'ctx, 'b, 'e: 'b>(
    stmt: &HirSortFileStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(stmt.span)?;
    let Some(sd_fc) = prog_ctx.prog.files.get(stmt.sd_file.as_u32() as usize) else {
        return Ok(());
    };
    let flags = match stmt.duplicates {
        HirSortDuplicates::Arbitrary => 0,
        HirSortDuplicates::InOrder => ABI_SORT_FLAG_DUPLICATES,
    };
    emit_sort_or_merge_begin(
        sd_fc,
        &stmt.keys,
        stmt.collating_alphabet,
        flags,
        stmt.sort_return_item,
        stmt.span,
        block,
        prog_ctx,
        state,
        loc,
    )?;

    match &stmt.input {
        HirSortInput::Procedure { start, end } => {
            if state.m13_units.get(&prog_ctx.prog.id).is_some_and(|u| {
                u.debugging_mode == coblang_hir::HirDebugCompileMode::WithDebuggingMode
            }) {
                let mode_v = emit_i32(3, block, prog_ctx, state, loc);
                let _ = emit_rt_call(
                    RtDispatchTarget::M13DebugSetSuppress,
                    &[mode_v],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            }
            let op = CobolPerformRangeOp {
                start_proc: start.as_u32(),
                end_proc: end.as_u32(),
            }
            .build(state.ctx, loc)?;
            let _ = block.append_operation(op);
        }
        HirSortInput::Using(files) => {
            for tf in files {
                emit_using_pump(
                    tf,
                    stmt.sort_return_item,
                    stmt.span,
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            }
        }
    }

    emit_finish_and_output_phase(
        stmt.sd_file,
        &stmt.output,
        stmt.sort_return_item,
        4,
        stmt.span,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Lowers `HirStmt::MergeFile` (`MERGE sd ON ... USING f1 f2 ... {GIVING | OUTPUT PROCEDURE}`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirMergeFileStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] on MLIR build failure.
pub(crate) fn lower_merge_file<'ctx, 'b, 'e: 'b>(
    stmt: &HirMergeFileStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(stmt.span)?;
    let Some(sd_fc) = prog_ctx.prog.files.get(stmt.sd_file.as_u32() as usize) else {
        return Ok(());
    };
    emit_sort_or_merge_begin(
        sd_fc,
        &stmt.keys,
        stmt.collating_alphabet,
        ABI_SORT_FLAG_MERGE | ABI_SORT_FLAG_DUPLICATES,
        stmt.sort_return_item,
        stmt.span,
        block,
        prog_ctx,
        state,
        loc,
    )?;

    for tf in &stmt.using_files {
        let _ = emit_rt_call(
            RtDispatchTarget::SortBeginMergeStream,
            &[],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        emit_using_pump(
            tf,
            stmt.sort_return_item,
            stmt.span,
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }

    emit_finish_and_output_phase(
        stmt.sd_file,
        &stmt.output,
        stmt.sort_return_item,
        5,
        stmt.span,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Lowers `HirStmt::ReleaseRecord` (`RELEASE sd-rec [FROM src]`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirReleaseStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] on MLIR build failure.
pub(crate) fn lower_release_record<'ctx, 'b, 'e: 'b>(
    stmt: &HirReleaseStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    for mv in &stmt.pre_from_moves {
        lower_stmt(mv, block, prog_ctx, state)?;
    }
    let loc = state.loc(stmt.span)?;
    let Some(sd_fc) = prog_ctx.prog.files.get(stmt.sd_file.as_u32() as usize) else {
        return Ok(());
    };
    let rec_pv = lower_place_view(&stmt.record_place, block, prog_ctx, state)?;
    let rec_len = eval_active_write_len(sd_fc, rec_pv.len_i64, stmt.span, block, prog_ctx, state)?;
    let sr_val = eval_sort_return_i64(
        stmt.sort_return_item,
        stmt.span,
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let rc = emit_rt_call(
        RtDispatchTarget::SortRelease,
        &[rec_pv.ptr, rec_len, sr_val],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    sync_sort_return_if_nonzero(
        rc,
        stmt.sort_return_item,
        stmt.span,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Lowers `HirStmt::ReturnRecord` (`RETURN sd-file RECORD [INTO dst] AT END ...`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirReturnStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] on MLIR build failure.
pub(crate) fn lower_return_record<'ctx, 'b, 'e: 'b>(
    stmt: &HirReturnStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(stmt.span)?;
    let Some(sd_fc) = prog_ctx.prog.files.get(stmt.sd_file.as_u32() as usize) else {
        return Ok(());
    };
    let sd_h = emit_connector_handles(sd_fc, stmt.span, block, prog_ctx, state, loc)?;
    let pad_v = emit_i32(i32::from(sd_fc.padding_byte), block, prog_ctx, state, loc);
    let sr_val = eval_sort_return_i64(
        stmt.sort_return_item,
        stmt.span,
        block,
        prog_ctx,
        state,
        loc,
    )?;

    let ret_len = emit_rt_call(
        RtDispatchTarget::SortReturn,
        &[sd_h.rec_ptr, sd_h.rec_max_len, pad_v, sr_val],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    let z64 = emit_i64(0, block, prog_ctx, state, loc);
    let is_record = append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Sge, ret_len, z64, loc),
        prog_ctx.fallback_val,
    );

    let ok_reg = Region::new();
    let ok_blk = ok_reg.append_block(Block::new(&[]));
    if let Some(dep_id) = sd_fc.depending_on_item
        && let Some(dep_pl) = simple_item_place(dep_id, prog_ctx, stmt.span)
    {
        store_i64_to_place(ret_len, &dep_pl, &ok_blk, prog_ctx, state, loc)?;
    }
    for mv in &stmt.into_moves {
        lower_stmt(mv, &ok_blk, prog_ctx, state)?;
    }
    for st in &stmt.not_at_end {
        lower_stmt(st, &ok_blk, prog_ctx, state)?;
    }
    let _ = ok_blk.append_operation(scf::r#yield(&[], loc));

    let end_reg = Region::new();
    let end_blk = end_reg.append_block(Block::new(&[]));
    let neg_two = emit_i64(-2, &end_blk, prog_ctx, state, loc);
    let is_aborted = append_val(
        &end_blk,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, ret_len, neg_two, loc),
        prog_ctx.fallback_val,
    );
    if let Some(sr_id) = stmt.sort_return_item
        && let Some(sr_pl) = simple_item_place(sr_id, prog_ctx, stmt.span)
    {
        let ab_then = Region::new();
        let ab_blk = ab_then.append_block(Block::new(&[]));
        let c16 = emit_i64(16, &ab_blk, prog_ctx, state, loc);
        store_i64_to_place(c16, &sr_pl, &ab_blk, prog_ctx, state, loc)?;
        let _ = ab_blk.append_operation(scf::r#yield(&[], loc));
        let ab_else = Region::new();
        let ab_else_blk = ab_else.append_block(Block::new(&[]));
        let _ = ab_else_blk.append_operation(scf::r#yield(&[], loc));
        let _ = end_blk.append_operation(scf::r#if(is_aborted, &[], ab_then, ab_else, loc));
    }
    for st in &stmt.at_end {
        lower_stmt(st, &end_blk, prog_ctx, state)?;
    }
    let _ = end_blk.append_operation(scf::r#yield(&[], loc));

    let _ = block.append_operation(scf::r#if(is_record, &[], ok_reg, end_reg, loc));
    Ok(())
}

/// Serializes the `SD` sort/merge plan, calls `coblang_rt_sort_begin`, and initializes `SORT-RETURN`.
///
/// # Arguments
///
/// * `sd_fc` - `SD` file connector.
/// * `keys` - Multi-key specifications.
/// * `collating_alphabet` - Optional alphabet index.
/// * `flags` - `ABI_SORT_FLAG_*` bitmask.
/// * `sort_return_item` - Optional `SORT-RETURN` item ID.
/// * `span` - Statement span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
#[expect(
    clippy::too_many_arguments,
    reason = "Shared session initializer for both `SORT` and `MERGE`."
)]
fn emit_sort_or_merge_begin<'ctx, 'b, 'e: 'b>(
    sd_fc: &HirFileConnector,
    keys: &[HirSortKeyDesc],
    collating_alphabet: Option<usize>,
    flags: i32,
    sort_return_item: Option<DataItemId>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let collating_ranks = collating_alphabet
        .and_then(|idx| prog_ctx.prog.alphabets.get(idx))
        .map(|alpha| {
            let mut arr = [0_u8; 256];
            for (i, slot) in arr.iter_mut().enumerate() {
                *slot = alpha
                    .ranks
                    .get(i)
                    .copied()
                    .unwrap_or_else(|| u8::try_from(i).unwrap_or(0));
            }
            arr
        });
    let rt_keys: Vec<RuntimeTableSortKey> = keys
        .iter()
        .map(|k| RuntimeTableSortKey {
            direction: k.direction,
            rel_offset: k.offset,
            byte_len: k.length,
            spec: make_runtime_field_spec(k.ty.clone(), prog_ctx.prog.formatting),
        })
        .collect();
    let plan = DecodedTableSortPlan {
        stride: sd_fc.max_record_bytes.max(1),
        collating_ranks,
        keys: rt_keys,
    };
    let plan_bytes = encode_table_sort_plan(&plan);
    let (plan_ptr, plan_len) = emit_const_slice(&plan_bytes, block, prog_ctx, state, loc)?;
    let min_v = emit_i64(
        i64::from(sd_fc.min_record_bytes.max(1)),
        block,
        prog_ctx,
        state,
        loc,
    );
    let max_v = emit_i64(
        i64::from(sd_fc.max_record_bytes.max(sd_fc.min_record_bytes).max(1)),
        block,
        prog_ctx,
        state,
        loc,
    );
    let flags_v = emit_i32(flags, block, prog_ctx, state, loc);
    let init_rc = emit_rt_call(
        RtDispatchTarget::SortBegin,
        &[plan_ptr, plan_len, min_v, max_v, flags_v],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    if let Some(sr_id) = sort_return_item
        && let Some(sr_pl) = simple_item_place(sr_id, prog_ctx, span)
    {
        let i64_ty = IntegerType::new(state.ctx.raw(), 64).into();
        let rc64 = append_val(
            block,
            arith::extsi(init_rc, i64_ty, loc),
            prog_ctx.fallback_val,
        );
        store_i64_to_place(rc64, &sr_pl, block, prog_ctx, state, loc)?;
    }
    Ok(())
}

/// Emits `OPEN INPUT -> CobolLoopOp(READ -> SortRelease) -> CLOSE` for one `USING` file.
///
/// # Arguments
///
/// * `tf` - `USING` transfer file + declaratives.
/// * `sort_return_item` - Optional `SORT-RETURN` item ID.
/// * `span` - Statement span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
fn emit_using_pump<'ctx, 'b, 'e: 'b>(
    tf: &HirSortTransferFile,
    sort_return_item: Option<DataItemId>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let open_rc = emit_single_file_open(
        tf.file,
        HirOpenMode::Input,
        HirOpenOption::Standard,
        &tf.decls,
        span,
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let opened_ok = emit_eq_i32(open_rc, POST_IO_CONTINUE, block, prog_ctx, state, loc);

    let pump_then = Region::new();
    let pump_blk = pump_then.append_block(Block::new(&[]));
    if let Some(fc) = prog_ctx.prog.files.get(tf.file.as_u32() as usize) {
        let cond_region = build_using_cond_region(fc, tf, span, prog_ctx, state, loc)?;

        let body_region = Region::new();
        let body_blk = body_region.append_block(Block::new(&[]));
        let bh = emit_connector_handles(fc, span, &body_blk, prog_ctx, state, loc)?;
        let actual_len = emit_rt_call(
            RtDispatchTarget::FileLastRecLen,
            &[],
            &body_blk,
            prog_ctx,
            state,
            loc,
        )?;
        let sr = eval_sort_return_i64(sort_return_item, span, &body_blk, prog_ctx, state, loc)?;
        let rel_rc = emit_rt_call(
            RtDispatchTarget::SortRelease,
            &[bh.rec_ptr, actual_len, sr],
            &body_blk,
            prog_ctx,
            state,
            loc,
        )?;
        sync_sort_return_if_nonzero(
            rel_rc,
            sort_return_item,
            span,
            &body_blk,
            prog_ctx,
            state,
            loc,
        )?;
        let _ = body_blk.append_operation(CobolYieldOp { value: None }.build(state.ctx, loc)?);

        let step_region = Region::new();
        let step_blk = step_region.append_block(Block::new(&[]));
        let _ = step_blk.append_operation(CobolYieldOp { value: None }.build(state.ctx, loc)?);

        let loop_op = CobolLoopOp {
            loop_kind: "after_tier",
            test_pos: "before",
            cond_region,
            body_region,
            step_region,
        }
        .build(state.ctx, loc)?;
        let _ = pump_blk.append_operation(loop_op);

        let _ = emit_single_file_close(
            tf.file,
            HirCloseDisposition::Standard,
            &tf.decls,
            span,
            &pump_blk,
            prog_ctx,
            state,
            loc,
        )?;
    }
    let _ = pump_blk.append_operation(scf::r#yield(&[], loc));

    let pump_else = Region::new();
    let pump_else_blk = pump_else.append_block(Block::new(&[]));
    let _ = pump_else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(opened_ok, &[], pump_then, pump_else, loc));
    Ok(())
}

/// Builds the `READ NEXT` condition region for [`emit_using_pump`].
///
/// # Arguments
///
/// * `fc` - Input file connector.
/// * `tf` - Transfer file spec.
/// * `span` - Statement span.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
fn build_using_cond_region<'ctx>(
    fc: &HirFileConnector,
    tf: &HirSortTransferFile,
    span: SourceSpan,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, '_>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Region<'ctx>> {
    let cond_region = Region::new();
    let cond_blk = cond_region.append_block(Block::new(&[]));
    let h = emit_connector_handles(fc, span, &cond_blk, prog_ctx, state, loc)?;
    let null_ptr = emit_null_ptr(&cond_blk, prog_ctx, state, loc);
    let z0 = emit_i32(0, &cond_blk, prog_ctx, state, loc);
    let z64 = emit_i64(0, &cond_blk, prog_ctx, state, loc);
    let inl_on = emit_i32(ABI_INLINE_HAS_ON, &cond_blk, prog_ctx, state, loc);
    let dmask = emit_i32(encode_decl_mask(&tf.decls), &cond_blk, prog_ctx, state, loc);

    let read_rc = emit_rt_call(
        RtDispatchTarget::FileRead,
        &[
            h.spec_ptr,
            h.spec_len,
            h.rec_ptr,
            h.rec_max_len,
            z0,
            z0,
            null_ptr,
            z64,
            z64,
            z0,
            inl_on,
            h.status_ptr,
            dmask,
        ],
        &cond_blk,
        prog_ctx,
        state,
        loc,
    )?;
    sync_connector_registers(fc, 0x03, span, &cond_blk, prog_ctx, state, loc)?;
    emit_post_io_dispatch(
        read_rc,
        (h.spec_ptr, h.spec_len),
        (&[], &[], &[]),
        &tf.decls,
        &cond_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let cont_v = emit_i32(POST_IO_CONTINUE, &cond_blk, prog_ctx, state, loc);
    let is_done = append_val(
        &cond_blk,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, read_rc, cont_v, loc),
        prog_ctx.fallback_val,
    );
    let _ = cond_blk.append_operation(
        CobolYieldOp {
            value: Some(is_done),
        }
        .build(state.ctx, loc)?,
    );
    Ok(cond_region)
}

/// Finalizes input (`coblang_rt_sort_finish_input`), runs `GIVING` or `OUTPUT PROCEDURE`, and ends the session.
///
/// # Arguments
///
/// * `sd_file` - `SD` connector ID.
/// * `output` - `GIVING` files or `OUTPUT PROCEDURE`.
/// * `sort_return_item` - Optional `SORT-RETURN` item ID.
/// * `out_dbg_mode` - NIST `DB` entry mode (`4` = `"SORT OUTPUT"`, `5` = `"MERGE OUTPUT"`).
/// * `span` - Statement span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
#[expect(
    clippy::too_many_arguments,
    reason = "Shared output + teardown phase for both `SORT` and `MERGE`."
)]
fn emit_finish_and_output_phase<'ctx, 'b, 'e: 'b>(
    sd_file: FileConnectorId,
    output: &HirSortOutput,
    sort_return_item: Option<DataItemId>,
    out_dbg_mode: i32,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let sr = eval_sort_return_i64(sort_return_item, span, block, prog_ctx, state, loc)?;
    let fin_rc = emit_rt_call(
        RtDispatchTarget::SortFinishInput,
        &[sr],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    sync_sort_return_if_nonzero(fin_rc, sort_return_item, span, block, prog_ctx, state, loc)?;

    let fin_ok = emit_eq_i32(fin_rc, 0, block, prog_ctx, state, loc);
    let out_then = Region::new();
    let out_blk = out_then.append_block(Block::new(&[]));

    match output {
        HirSortOutput::Procedure { start, end } => {
            if state.m13_units.get(&prog_ctx.prog.id).is_some_and(|u| {
                u.debugging_mode == coblang_hir::HirDebugCompileMode::WithDebuggingMode
            }) {
                let mode_v = emit_i32(out_dbg_mode, &out_blk, prog_ctx, state, loc);
                let _ = emit_rt_call(
                    RtDispatchTarget::M13DebugSetSuppress,
                    &[mode_v],
                    &out_blk,
                    prog_ctx,
                    state,
                    loc,
                )?;
            }
            let op = CobolPerformRangeOp {
                start_proc: start.as_u32(),
                end_proc: end.as_u32(),
            }
            .build(state.ctx, loc)?;
            let _ = out_blk.append_operation(op);
        }
        HirSortOutput::Giving(files) => {
            emit_giving_fanout(
                sd_file,
                files,
                sort_return_item,
                span,
                &out_blk,
                prog_ctx,
                state,
                loc,
            )?;
        }
    }
    let _ = out_blk.append_operation(scf::r#yield(&[], loc));

    let out_else = Region::new();
    let out_else_blk = out_else.append_block(Block::new(&[]));
    let _ = out_else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(fin_ok, &[], out_then, out_else, loc));

    let end_rc = emit_rt_call(RtDispatchTarget::SortEnd, &[], block, prog_ctx, state, loc)?;
    sync_sort_return_if_nonzero(end_rc, sort_return_item, span, block, prog_ctx, state, loc)
}

/// Opens all `GIVING` files, fans out every sorted record from `coblang_rt_sort_return` to each file, and closes them.
///
/// # Arguments
///
/// * `sd_file` - `SD` connector ID.
/// * `giving_files` - Target `GIVING` files.
/// * `sort_return_item` - Optional `SORT-RETURN` item ID.
/// * `span` - Statement span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
#[expect(
    clippy::too_many_arguments,
    reason = "Multi-file GIVING fanout emitter."
)]
#[expect(
    clippy::too_many_lines,
    reason = "Complete multi-file SORT/MERGE GIVING fanout loop with NIST DB debug traces."
)]
fn emit_giving_fanout<'ctx, 'b, 'e: 'b>(
    sd_file: FileConnectorId,
    giving_files: &[HirSortTransferFile],
    sort_return_item: Option<DataItemId>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let Some(sd_fc) = prog_ctx.prog.files.get(sd_file.as_u32() as usize) else {
        return Ok(());
    };
    for gf in giving_files {
        let _ = emit_single_file_open(
            gf.file,
            HirOpenMode::Output,
            HirOpenOption::Standard,
            &gf.decls,
            span,
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }

    let i64_ty = IntegerType::new(state.ctx.raw(), 64).into();
    let cond_region = Region::new();
    let cond_blk = cond_region.append_block(Block::new(&[]));
    let sd_h = emit_connector_handles(sd_fc, span, &cond_blk, prog_ctx, state, loc)?;
    let pad_v = emit_i32(
        i32::from(sd_fc.padding_byte),
        &cond_blk,
        prog_ctx,
        state,
        loc,
    );
    let sr = eval_sort_return_i64(sort_return_item, span, &cond_blk, prog_ctx, state, loc)?;
    let ret_len = emit_rt_call(
        RtDispatchTarget::SortReturn,
        &[sd_h.rec_ptr, sd_h.rec_max_len, pad_v, sr],
        &cond_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let scratch_ptr = emit_storage_ptr(
        &prog_ctx.idx_scratch_sym,
        0,
        8,
        &cond_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = cond_blk.append_operation(llvm::store(
        state.ctx.raw(),
        ret_len,
        scratch_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let z64 = emit_i64(0, &cond_blk, prog_ctx, state, loc);
    let is_done = append_val(
        &cond_blk,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Slt, ret_len, z64, loc),
        prog_ctx.fallback_val,
    );
    let _ = cond_blk.append_operation(
        CobolYieldOp {
            value: Some(is_done),
        }
        .build(state.ctx, loc)?,
    );

    let body_region = Region::new();
    let body_blk = body_region.append_block(Block::new(&[]));
    let b_scratch = emit_storage_ptr(
        &prog_ctx.idx_scratch_sym,
        0,
        8,
        &body_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let cur_len = append_val(
        &body_blk,
        llvm::load(
            state.ctx.raw(),
            b_scratch,
            i64_ty,
            loc,
            llvm::LoadStoreOptions::new(),
        ),
        prog_ctx.fallback_val,
    );
    let body_sd_handles = emit_connector_handles(sd_fc, span, &body_blk, prog_ctx, state, loc)?;

    for gf in giving_files {
        let Some(out_fc) = prog_ctx.prog.files.get(gf.file.as_u32() as usize) else {
            continue;
        };
        emit_single_giving_write(
            out_fc,
            gf,
            body_sd_handles.rec_ptr,
            cur_len,
            span,
            &body_blk,
            prog_ctx,
            state,
            loc,
        )?;
    }
    let _ = body_blk.append_operation(CobolYieldOp { value: None }.build(state.ctx, loc)?);

    let step_region = Region::new();
    let step_blk = step_region.append_block(Block::new(&[]));
    let _ = step_blk.append_operation(CobolYieldOp { value: None }.build(state.ctx, loc)?);

    let loop_op = CobolLoopOp {
        loop_kind: "after_tier",
        test_pos: "before",
        cond_region,
        body_region,
        step_region,
    }
    .build(state.ctx, loc)?;
    let _ = block.append_operation(loop_op);

    for gf in giving_files {
        let _ = emit_single_file_close(
            gf.file,
            HirCloseDisposition::Standard,
            &gf.decls,
            span,
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Copies a sorted record into `out_fc`'s record buffer and emits `FileWrite` for one `GIVING` target.
///
/// # Arguments
///
/// * `out_fc` - Output `GIVING` file connector.
/// * `gf` - Transfer file spec.
/// * `sd_rec_ptr` - `SD` record buffer pointer.
/// * `cur_len` - Actual returned record length SSA `i64`.
/// * `span` - Statement span.
/// * `body_blk` - Loop body block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
#[expect(
    clippy::too_many_arguments,
    reason = "Single GIVING file write emitter."
)]
#[expect(
    clippy::too_many_lines,
    reason = "Complete GIVING record copy, FileWrite, and NIST DB output trace."
)]
fn emit_single_giving_write<'ctx, 'b, 'e: 'b>(
    out_fc: &HirFileConnector,
    gf: &HirSortTransferFile,
    sd_rec_ptr: Value<'ctx, 'b>,
    cur_len: Value<'ctx, 'b>,
    span: SourceSpan,
    body_blk: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let gh = emit_connector_handles(out_fc, span, body_blk, prog_ctx, state, loc)?;
    let max_u32 = out_fc.max_record_bytes.max(1);
    let min_u32 = out_fc.min_record_bytes.max(1);
    let max_val = emit_i64(i64::from(max_u32), body_blk, prog_ctx, state, loc);
    let min_val = emit_i64(i64::from(min_u32), body_blk, prog_ctx, state, loc);

    let write_len = match out_fc.record_format {
        HirRecordFormat::Fixed => max_val,
        HirRecordFormat::Varying => {
            let lo = append_val(
                body_blk,
                arith::maxsi(cur_len, min_val, loc),
                prog_ctx.fallback_val,
            );
            append_val(
                body_blk,
                arith::minsi(lo, max_val, loc),
                prog_ctx.fallback_val,
            )
        }
    };
    if let Some(dep_id) = out_fc.depending_on_item
        && let Some(dep_pl) = simple_item_place(dep_id, prog_ctx, span)
    {
        store_i64_to_place(write_len, &dep_pl, body_blk, prog_ctx, state, loc)?;
    }

    let copy_n = append_val(
        body_blk,
        arith::minsi(cur_len, max_val, loc),
        prog_ctx.fallback_val,
    );
    let _ = emit_rt_call(
        RtDispatchTarget::RawCopy,
        &[gh.rec_ptr, sd_rec_ptr, copy_n],
        body_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let tail_len = append_val(
        body_blk,
        arith::subi(max_val, copy_n, loc),
        prog_ctx.fallback_val,
    );
    let i8_ty = IntegerType::new(state.ctx.raw(), 8).into();
    let tail_ptr = append_val(
        body_blk,
        llvm::get_element_ptr_dynamic(
            state.ctx.raw(),
            gh.rec_ptr,
            &[copy_n],
            i8_ty,
            crate::ops::llvm_ptr_type(state.ctx),
            loc,
        ),
        prog_ctx.fallback_val,
    );
    let pad_bytes = vec![out_fc.padding_byte; max_u32 as usize];
    let (pad_ptr, _) = emit_const_slice(&pad_bytes, body_blk, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::RawCopy,
        &[tail_ptr, pad_ptr, tail_len],
        body_blk,
        prog_ctx,
        state,
        loc,
    )?;

    let rel_key_in = eval_rel_key_i64(out_fc, span, body_blk, prog_ctx, state, loc)?;
    let z0 = emit_i32(0, body_blk, prog_ctx, state, loc);
    let z64_b = emit_i64(0, body_blk, prog_ctx, state, loc);
    let dmask = emit_i32(encode_decl_mask(&gf.decls), body_blk, prog_ctx, state, loc);
    let w_rc = emit_rt_call(
        RtDispatchTarget::FileWrite,
        &[
            gh.spec_ptr,
            gh.spec_len,
            gh.rec_ptr,
            write_len,
            z0,
            z64_b,
            rel_key_in,
            z0,
            z0,
            gh.status_ptr,
            dmask,
        ],
        body_blk,
        prog_ctx,
        state,
        loc,
    )?;
    sync_connector_registers(out_fc, 0x06, span, body_blk, prog_ctx, state, loc)?;
    emit_post_io_dispatch(
        w_rc,
        (gh.spec_ptr, gh.spec_len),
        (&[], &[], &[]),
        &gf.decls,
        body_blk,
        prog_ctx,
        state,
        loc,
    )
}

/// Evaluates `SORT-RETURN` as an `i64` (`0` when undeclared).
///
/// # Arguments
///
/// * `sr_opt` - Optional `SORT-RETURN` `DataItemId`.
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
fn eval_sort_return_i64<'ctx, 'b, 'e: 'b>(
    sr_opt: Option<DataItemId>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    if let Some(sr_id) = sr_opt
        && let Some(sr_pl) = simple_item_place(sr_id, prog_ctx, span)
    {
        eval_expr_as_i64(&HirExpr::Place(sr_pl), block, prog_ctx, state)
    } else {
        Ok(emit_i64(0, block, prog_ctx, state, loc))
    }
}

/// Stores `16` into `SORT-RETURN` whenever `rc != 0`.
///
/// # Arguments
///
/// * `rc` - `i32` status returned by `coblang_rt_sort_*`.
/// * `sr_opt` - Optional `SORT-RETURN` `DataItemId`.
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
fn sync_sort_return_if_nonzero<'ctx, 'b, 'e: 'b>(
    rc: Value<'ctx, 'b>,
    sr_opt: Option<DataItemId>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let Some(sr_id) = sr_opt else {
        return Ok(());
    };
    let Some(sr_pl) = simple_item_place(sr_id, prog_ctx, span) else {
        return Ok(());
    };
    let z0 = emit_i32(0, block, prog_ctx, state, loc);
    let failed = append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, rc, z0, loc),
        prog_ctx.fallback_val,
    );
    let then_reg = Region::new();
    let then_blk = then_reg.append_block(Block::new(&[]));
    let c16 = emit_i64(16, &then_blk, prog_ctx, state, loc);
    store_i64_to_place(c16, &sr_pl, &then_blk, prog_ctx, state, loc)?;
    let _ = then_blk.append_operation(scf::r#yield(&[], loc));
    let else_reg = Region::new();
    let else_blk = else_reg.append_block(Block::new(&[]));
    let _ = else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(failed, &[], then_reg, else_reg, loc));
    Ok(())
}
