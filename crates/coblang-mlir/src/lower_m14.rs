//! Milestones M14 (`SEQUENTIAL`, `LINE SEQUENTIAL`, `RELATIVE`) and M15 (`INDEXED` ISAM)
//! HIR-to-`cobol` MLIR lowering, `LINAGE`/`RELATIVE KEY`/`DEPENDING ON` register sync,
//! and post-I/O `DECLARATIVES` dispatch.

use coblang_common::{DataItemId, FileConnectorId, Result, SourceSpan};
use coblang_hir::{
    ExceptionCheckMask, HirAdvancingSide, HirAssignSpec, HirCloseDisposition, HirConnectorSharing,
    HirDeclRef, HirExpr, HirFileCloseStmt, HirFileConnector, HirFileDeclSet,
    HirFileDeletePhysicalStmt, HirFileDeleteStmt, HirFileOpenStmt, HirFileReadStmt,
    HirFileRewriteStmt, HirFileStartStmt, HirFileUnlockStmt, HirFileWriteStmt,
    HirInlineClausePresence, HirLinageParam, HirOpenMode, HirOpenOption, HirPlace,
    HirReadLockPolicy, HirReadMode, HirSeqReadDir, HirStartMode, HirStartRelation, HirStmt,
    HirType, HirWriteAdvancing, HirWriteHandlerKind,
};
use coblang_rt_core::fileio_wire::{
    DECL_MASK_EXTEND, DECL_MASK_FILE, DECL_MASK_INPUT, DECL_MASK_IO, DECL_MASK_OUTPUT,
    FileConnectorWireSpec, POST_IO_CONTINUE, POST_IO_FATAL_ABORT, POST_IO_FIRE_AT_END,
    POST_IO_FIRE_AT_EOP, POST_IO_FIRE_INVALID_KEY, POST_IO_FIRE_NOT_AT_END,
    POST_IO_FIRE_NOT_AT_EOP, POST_IO_FIRE_NOT_INVALID_KEY, POST_IO_INVOKE_DECL_EXTEND,
    POST_IO_INVOKE_DECL_FILE, POST_IO_INVOKE_DECL_INPUT, POST_IO_INVOKE_DECL_IO,
    POST_IO_INVOKE_DECL_OUTPUT, WireAltKey, WireAssignKind, WireConnectorIdentity,
    encode_file_connector_spec,
};
use melior::dialect::arith::CmpiPredicate;
use melior::dialect::{arith, llvm, scf};
use melior::ir::block::BlockLike;
use melior::ir::{Block, Location, Region, RegionLike, Value};

use crate::lower_from_hir::{
    ModuleLowerState, ProgramLowerCtx, append_val, emit_const_slice, emit_i32, emit_i64,
    emit_rt_call, emit_size_error_branch, eval_expr_as_i64, lower_place_view, lower_stmt,
};
use crate::lower_m10::store_i64_to_place;
use crate::ops::{CobolPerformRangeOp, RtDispatchTarget, llvm_ptr_type};

/// Bit `0` of `inline_flags` (`INLINE_FLAG_HAS_ON` in `fileio_abi`).
pub(crate) const ABI_INLINE_HAS_ON: i32 = 1 << 0;
/// Bit `1` of `inline_flags` (`INLINE_FLAG_HAS_NOT_ON` in `fileio_abi`).
pub(crate) const ABI_INLINE_HAS_NOT_ON: i32 = 1 << 1;

/// Sync flag bit `0`: copy `coblang_rt_file_last_rec_len()` into `fc.depending_on_item`.
const SYNC_REC_LEN: u8 = 1 << 0;
/// Sync flag bit `1`: copy `coblang_rt_file_last_rel_key()` into `fc.relative_key_item`.
const SYNC_REL_KEY: u8 = 1 << 1;
/// Sync flag bit `2`: copy `coblang_rt_file_last_linage_counter()` into `linage.counter_item`.
const SYNC_LINAGE: u8 = 1 << 2;

/// Resolved SSA handles `(spec_ptr, spec_len, rec_ptr, rec_max_len, status_ptr)` for a connector.
pub(crate) struct ConnectorSsaHandles<'ctx, 'b> {
    /// Read-only `FileConnectorWireSpec` pointer.
    pub(crate) spec_ptr: Value<'ctx, 'b>,
    /// Byte length of `spec_ptr`.
    pub(crate) spec_len: Value<'ctx, 'b>,
    /// Mutable `FD` / `SD` record area base pointer.
    pub(crate) rec_ptr: Value<'ctx, 'b>,
    /// Maximum record byte size (`i64`).
    pub(crate) rec_max_len: Value<'ctx, 'b>,
    /// Optional 2-byte `FILE STATUS` pointer (null when undeclared).
    pub(crate) status_ptr: Value<'ctx, 'b>,
}

/// Lowers `HirStmt::FileOpen` (`OPEN {INPUT | OUTPUT | I-O | EXTEND} file ...`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirFileOpenStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if place or declarative lowering fails.
pub(crate) fn lower_file_open<'ctx, 'b, 'e: 'b>(
    stmt: &HirFileOpenStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(stmt.span)?;
    for entry in &stmt.entries {
        let _ = emit_single_file_open(
            entry.file,
            entry.mode,
            entry.option,
            &entry.decls,
            stmt.span,
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Emits one `OPEN` operation + `LINAGE-COUNTER` sync + post-I/O dispatch, returning the ABI `rc` (`i32`).
///
/// # Arguments
///
/// * `fc_id` - Target [`FileConnectorId`].
/// * `mode` - Requested [`HirOpenMode`].
/// * `option` - Entry [`HirOpenOption`].
/// * `decls` - Candidate `DECLARATIVES`.
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] on MLIR build failure.
#[expect(
    clippy::too_many_arguments,
    reason = "Shared primitive invoked by both `OPEN` and `SORT`/`MERGE` `USING`/`GIVING` loops."
)]
pub(crate) fn emit_single_file_open<'ctx, 'b, 'e: 'b>(
    fc_id: FileConnectorId,
    mode: HirOpenMode,
    option: HirOpenOption,
    decls: &HirFileDeclSet,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let Some(fc) = prog_ctx.prog.files.get(fc_id.as_u32() as usize) else {
        return Ok(emit_i32(POST_IO_CONTINUE, block, prog_ctx, state, loc));
    };
    let h = emit_connector_handles(fc, span, block, prog_ctx, state, loc)?;
    let (dyn_ptr, dyn_len) = emit_dynamic_assign_slice(fc, span, block, prog_ctx, state, loc)?;
    let (lin_body, lin_foot, lin_top, lin_bot) =
        eval_linage_operands(fc, span, block, prog_ctx, state, loc)?;

    let mode_v = emit_i32(encode_open_mode(mode), block, prog_ctx, state, loc);
    let opt_v = emit_i32(encode_open_option(option), block, prog_ctx, state, loc);
    let dmask_v = emit_i32(encode_decl_mask(decls), block, prog_ctx, state, loc);

    let rc = emit_rt_call(
        RtDispatchTarget::FileOpen,
        &[
            h.spec_ptr,
            h.spec_len,
            dyn_ptr,
            dyn_len,
            mode_v,
            opt_v,
            lin_body,
            lin_foot,
            lin_top,
            lin_bot,
            h.status_ptr,
            dmask_v,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    sync_connector_registers(fc, SYNC_LINAGE, span, block, prog_ctx, state, loc)?;
    emit_post_io_dispatch(
        rc,
        (h.spec_ptr, h.spec_len),
        (&[], &[], &[]),
        decls,
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(rc)
}

/// Lowers `HirStmt::FileClose` (`CLOSE file ...`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirFileCloseStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if handle or declarative lowering fails.
pub(crate) fn lower_file_close<'ctx, 'b, 'e: 'b>(
    stmt: &HirFileCloseStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(stmt.span)?;
    for entry in &stmt.entries {
        let _ = emit_single_file_close(
            entry.file,
            entry.disposition,
            &entry.decls,
            stmt.span,
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Emits one `CLOSE` operation + post-I/O dispatch, returning the ABI `rc` (`i32`).
///
/// # Arguments
///
/// * `fc_id` - Target [`FileConnectorId`].
/// * `disp` - Close disposition.
/// * `decls` - Candidate `DECLARATIVES`.
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] on MLIR build failure.
#[expect(
    clippy::too_many_arguments,
    reason = "Shared primitive invoked by both `CLOSE` and `SORT`/`MERGE` `USING`/`GIVING` loops."
)]
pub(crate) fn emit_single_file_close<'ctx, 'b, 'e: 'b>(
    fc_id: FileConnectorId,
    disp: HirCloseDisposition,
    decls: &HirFileDeclSet,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let Some(fc) = prog_ctx.prog.files.get(fc_id.as_u32() as usize) else {
        return Ok(emit_i32(POST_IO_CONTINUE, block, prog_ctx, state, loc));
    };
    let h = emit_connector_handles(fc, span, block, prog_ctx, state, loc)?;
    let disp_v = emit_i32(encode_close_disp(disp), block, prog_ctx, state, loc);
    let dmask_v = emit_i32(encode_decl_mask(decls), block, prog_ctx, state, loc);
    let rc = emit_rt_call(
        RtDispatchTarget::FileClose,
        &[h.spec_ptr, h.spec_len, disp_v, h.status_ptr, dmask_v],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    emit_post_io_dispatch(
        rc,
        (h.spec_ptr, h.spec_len),
        (&[], &[], &[]),
        decls,
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(rc)
}

/// Lowers `HirStmt::FileRead` (`READ file [NEXT | PREVIOUS] [INTO dst] [KEY IS key] ...`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirFileReadStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if place or handler lowering fails.
pub(crate) fn lower_file_read<'ctx, 'b, 'e: 'b>(
    stmt: &HirFileReadStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(stmt.span)?;
    let Some(fc) = prog_ctx.prog.files.get(stmt.file.as_u32() as usize) else {
        return Ok(());
    };
    let h = emit_connector_handles(fc, stmt.span, block, prog_ctx, state, loc)?;
    let null_ptr = emit_null_ptr(block, prog_ctx, state, loc);
    let z64 = emit_i64(0, block, prog_ctx, state, loc);

    let (read_kind, key_idx, key_ptr, key_len) = match &stmt.mode {
        HirReadMode::Sequential {
            direction: HirSeqReadDir::Next,
        } => (0_i32, 0_i32, null_ptr, z64),
        HirReadMode::Sequential {
            direction: HirSeqReadDir::Previous,
        } => (1_i32, 0_i32, null_ptr, z64),
        HirReadMode::Random {
            key_index,
            key_place,
        } => {
            if let Some(kp) = key_place {
                let pv = lower_place_view(kp, block, prog_ctx, state)?;
                (2_i32, i32::from(*key_index), pv.ptr, pv.len_i64)
            } else {
                (2_i32, i32::from(*key_index), null_ptr, z64)
            }
        }
    };

    let rel_key_in = eval_rel_key_i64(fc, stmt.span, block, prog_ctx, state, loc)?;
    let rk_v = emit_i32(read_kind, block, prog_ctx, state, loc);
    let ki_v = emit_i32(key_idx, block, prog_ctx, state, loc);
    let lk_v = emit_i32(
        encode_read_lock(stmt.lock_policy),
        block,
        prog_ctx,
        state,
        loc,
    );
    let inl_v = emit_i32(
        encode_inline_flags(stmt.inline_presence, !stmt.not_on_condition.is_empty()),
        block,
        prog_ctx,
        state,
        loc,
    );
    let dmask_v = emit_i32(encode_decl_mask(&stmt.decls), block, prog_ctx, state, loc);

    let rc = emit_rt_call(
        RtDispatchTarget::FileRead,
        &[
            h.spec_ptr,
            h.spec_len,
            h.rec_ptr,
            h.rec_max_len,
            rk_v,
            ki_v,
            key_ptr,
            key_len,
            rel_key_in,
            lk_v,
            inl_v,
            h.status_ptr,
            dmask_v,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    sync_connector_registers(
        fc,
        SYNC_REC_LEN | SYNC_REL_KEY,
        stmt.span,
        block,
        prog_ctx,
        state,
        loc,
    )?;
    emit_post_io_dispatch(
        rc,
        (h.spec_ptr, h.spec_len),
        (&stmt.into_moves, &stmt.on_condition, &stmt.not_on_condition),
        &stmt.decls,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Lowers `HirStmt::FileWrite` (`WRITE record [FROM src] [ADVANCING ...] ...`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirFileWriteStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if move, place, or handler lowering fails.
pub(crate) fn lower_file_write<'ctx, 'b, 'e: 'b>(
    stmt: &HirFileWriteStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    for mv in &stmt.pre_from_moves {
        lower_stmt(mv, block, prog_ctx, state)?;
    }
    let loc = state.loc(stmt.span)?;
    let Some(fc) = prog_ctx.prog.files.get(stmt.file.as_u32() as usize) else {
        return Ok(());
    };
    let h = emit_connector_handles(fc, stmt.span, block, prog_ctx, state, loc)?;
    if fc.linage.is_some() {
        let (lb, lf, lt, lbot) = eval_linage_operands(fc, stmt.span, block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::FileSetLinage,
            &[h.spec_ptr, h.spec_len, lb, lf, lt, lbot],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    let rec_pv = lower_place_view(&stmt.record_place, block, prog_ctx, state)?;
    let write_len = eval_active_write_len(fc, rec_pv.len_i64, stmt.span, block, prog_ctx, state)?;
    let (adv_kind, adv_cnt) = eval_write_advancing(&stmt.advancing, block, prog_ctx, state, loc)?;

    let rel_key_in = eval_rel_key_i64(fc, stmt.span, block, prog_ctx, state, loc)?;
    let ak_v = emit_i32(adv_kind, block, prog_ctx, state, loc);
    let hk_v = emit_i32(
        match stmt.handler_kind {
            HirWriteHandlerKind::None => 0,
            HirWriteHandlerKind::EndOfPage => 1,
            HirWriteHandlerKind::InvalidKey => 2,
        },
        block,
        prog_ctx,
        state,
        loc,
    );
    let has_on = if stmt.on_event.is_empty() {
        HirInlineClausePresence::Absent
    } else {
        HirInlineClausePresence::Present
    };
    let inl_v = emit_i32(
        encode_inline_flags(has_on, !stmt.not_on_event.is_empty()),
        block,
        prog_ctx,
        state,
        loc,
    );
    let dmask_v = emit_i32(encode_decl_mask(&stmt.decls), block, prog_ctx, state, loc);

    let rc = emit_rt_call(
        RtDispatchTarget::FileWrite,
        &[
            h.spec_ptr,
            h.spec_len,
            rec_pv.ptr,
            write_len,
            ak_v,
            adv_cnt,
            rel_key_in,
            hk_v,
            inl_v,
            h.status_ptr,
            dmask_v,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    sync_connector_registers(
        fc,
        SYNC_REL_KEY | SYNC_LINAGE,
        stmt.span,
        block,
        prog_ctx,
        state,
        loc,
    )?;
    emit_post_io_dispatch(
        rc,
        (h.spec_ptr, h.spec_len),
        (&[], &stmt.on_event, &stmt.not_on_event),
        &stmt.decls,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Lowers `HirStmt::FileRewrite` (`REWRITE record [FROM src] ...`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirFileRewriteStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if move, place, or handler lowering fails.
pub(crate) fn lower_file_rewrite<'ctx, 'b, 'e: 'b>(
    stmt: &HirFileRewriteStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    for mv in &stmt.pre_from_moves {
        lower_stmt(mv, block, prog_ctx, state)?;
    }
    let loc = state.loc(stmt.span)?;
    let Some(fc) = prog_ctx.prog.files.get(stmt.file.as_u32() as usize) else {
        return Ok(());
    };
    let h = emit_connector_handles(fc, stmt.span, block, prog_ctx, state, loc)?;
    let rec_pv = lower_place_view(&stmt.record_place, block, prog_ctx, state)?;
    let write_len = eval_active_write_len(fc, rec_pv.len_i64, stmt.span, block, prog_ctx, state)?;
    let rel_key_in = eval_rel_key_i64(fc, stmt.span, block, prog_ctx, state, loc)?;
    let inl_v = emit_i32(
        encode_inline_flags(stmt.inline_presence, !stmt.not_on_invalid_key.is_empty()),
        block,
        prog_ctx,
        state,
        loc,
    );
    let dmask_v = emit_i32(encode_decl_mask(&stmt.decls), block, prog_ctx, state, loc);

    let rc = emit_rt_call(
        RtDispatchTarget::FileRewrite,
        &[
            h.spec_ptr,
            h.spec_len,
            rec_pv.ptr,
            write_len,
            rel_key_in,
            inl_v,
            h.status_ptr,
            dmask_v,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    emit_post_io_dispatch(
        rc,
        (h.spec_ptr, h.spec_len),
        (&[], &stmt.on_invalid_key, &stmt.not_on_invalid_key),
        &stmt.decls,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Lowers `HirStmt::FileDelete` (`DELETE file [RECORD] ...`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirFileDeleteStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if place or handler lowering fails.
pub(crate) fn lower_file_delete<'ctx, 'b, 'e: 'b>(
    stmt: &HirFileDeleteStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(stmt.span)?;
    let Some(fc) = prog_ctx.prog.files.get(stmt.file.as_u32() as usize) else {
        return Ok(());
    };
    let h = emit_connector_handles(fc, stmt.span, block, prog_ctx, state, loc)?;
    let rel_key_in = eval_rel_key_i64(fc, stmt.span, block, prog_ctx, state, loc)?;
    let inl_v = emit_i32(
        encode_inline_flags(stmt.inline_presence, !stmt.not_on_invalid_key.is_empty()),
        block,
        prog_ctx,
        state,
        loc,
    );
    let dmask_v = emit_i32(encode_decl_mask(&stmt.decls), block, prog_ctx, state, loc);

    let rc = emit_rt_call(
        RtDispatchTarget::FileDelete,
        &[
            h.spec_ptr,
            h.spec_len,
            h.rec_ptr,
            h.rec_max_len,
            rel_key_in,
            inl_v,
            h.status_ptr,
            dmask_v,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    emit_post_io_dispatch(
        rc,
        (h.spec_ptr, h.spec_len),
        (&[], &stmt.on_invalid_key, &stmt.not_on_invalid_key),
        &stmt.decls,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Lowers `HirStmt::FileDeletePhysical` (`DELETE FILE f1 f2 ...`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirFileDeletePhysicalStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if place or handler lowering fails.
pub(crate) fn lower_file_delete_physical<'ctx, 'b, 'e: 'b>(
    stmt: &HirFileDeletePhysicalStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(stmt.span)?;
    let inl_flags = encode_inline_flags(stmt.inline_presence, !stmt.not_on_exception.is_empty());
    let inl_v = emit_i32(inl_flags, block, prog_ctx, state, loc);

    let mut any_failed = emit_i32(0, block, prog_ctx, state, loc);
    let one_i32 = emit_i32(1, block, prog_ctx, state, loc);
    let fk_code = emit_i32(POST_IO_FIRE_INVALID_KEY, block, prog_ctx, state, loc);

    for entry in &stmt.entries {
        let Some(fc) = prog_ctx.prog.files.get(entry.file.as_u32() as usize) else {
            continue;
        };
        let h = emit_connector_handles(fc, stmt.span, block, prog_ctx, state, loc)?;
        let (dyn_ptr, dyn_len) =
            emit_dynamic_assign_slice(fc, stmt.span, block, prog_ctx, state, loc)?;
        let dmask_v = emit_i32(encode_decl_mask(&entry.decls), block, prog_ctx, state, loc);

        let rc = emit_rt_call(
            RtDispatchTarget::FileDeletePhysical,
            &[
                h.spec_ptr,
                h.spec_len,
                dyn_ptr,
                dyn_len,
                inl_v,
                h.status_ptr,
                dmask_v,
            ],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        let is_exc = append_val(
            block,
            arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, rc, fk_code, loc),
            prog_ctx.fallback_val,
        );
        any_failed = append_val(
            block,
            arith::select(is_exc, one_i32, any_failed, loc),
            prog_ctx.fallback_val,
        );
        emit_post_io_dispatch(
            rc,
            (h.spec_ptr, h.spec_len),
            (&[], &[], &[]),
            &entry.decls,
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }

    if !stmt.on_exception.is_empty() || !stmt.not_on_exception.is_empty() {
        let z0 = emit_i32(0, block, prog_ctx, state, loc);
        let cond = append_val(
            block,
            arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, any_failed, z0, loc),
            prog_ctx.fallback_val,
        );
        emit_size_error_branch(
            cond,
            &stmt.on_exception,
            &stmt.not_on_exception,
            loc,
            block,
            prog_ctx,
            state,
        )?;
    }
    Ok(())
}

/// Lowers `HirStmt::FileStart` (`START file [FIRST | LAST | KEY IS rel_op key] ...`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirFileStartStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if key place or handler lowering fails.
pub(crate) fn lower_file_start<'ctx, 'b, 'e: 'b>(
    stmt: &HirFileStartStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(stmt.span)?;
    let Some(fc) = prog_ctx.prog.files.get(stmt.file.as_u32() as usize) else {
        return Ok(());
    };
    let h = emit_connector_handles(fc, stmt.span, block, prog_ctx, state, loc)?;
    let null_ptr = emit_null_ptr(block, prog_ctx, state, loc);
    let z64 = emit_i64(0, block, prog_ctx, state, loc);

    let (start_kind, key_idx, key_ptr, key_len) = match &stmt.mode {
        HirStartMode::First => (5_i32, 0_i32, null_ptr, z64),
        HirStartMode::Last => (6_i32, 0_i32, null_ptr, z64),
        HirStartMode::Relation {
            relation,
            key_index,
            key_place,
            key_length,
        } => {
            let sk = match relation {
                HirStartRelation::Equal => 0_i32,
                HirStartRelation::Greater => 1_i32,
                HirStartRelation::GreaterOrEqual => 2_i32,
                HirStartRelation::Less => 3_i32,
                HirStartRelation::LessOrEqual => 4_i32,
            };
            let (kp, mut kl) = if let Some(pl) = key_place {
                let pv = lower_place_view(pl, block, prog_ctx, state)?;
                (pv.ptr, pv.len_i64)
            } else {
                (null_ptr, z64)
            };
            if let Some(kl_expr) = key_length {
                kl = eval_expr_as_i64(kl_expr, block, prog_ctx, state)?;
            }
            (sk, i32::from(*key_index), kp, kl)
        }
    };

    let rel_key_in = eval_rel_key_i64(fc, stmt.span, block, prog_ctx, state, loc)?;
    let sk_v = emit_i32(start_kind, block, prog_ctx, state, loc);
    let ki_v = emit_i32(key_idx, block, prog_ctx, state, loc);
    let inl_v = emit_i32(
        encode_inline_flags(stmt.inline_presence, !stmt.not_on_invalid_key.is_empty()),
        block,
        prog_ctx,
        state,
        loc,
    );
    let dmask_v = emit_i32(encode_decl_mask(&stmt.decls), block, prog_ctx, state, loc);

    let rc = emit_rt_call(
        RtDispatchTarget::FileStart,
        &[
            h.spec_ptr,
            h.spec_len,
            h.rec_ptr,
            h.rec_max_len,
            sk_v,
            ki_v,
            key_ptr,
            key_len,
            rel_key_in,
            inl_v,
            h.status_ptr,
            dmask_v,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    emit_post_io_dispatch(
        rc,
        (h.spec_ptr, h.spec_len),
        (&[], &stmt.on_invalid_key, &stmt.not_on_invalid_key),
        &stmt.decls,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Lowers `HirStmt::FileUnlock` (`UNLOCK file`).
///
/// # Arguments
///
/// * `stmt` - Resolved [`HirFileUnlockStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if handle or declarative lowering fails.
pub(crate) fn lower_file_unlock<'ctx, 'b, 'e: 'b>(
    stmt: &HirFileUnlockStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(stmt.span)?;
    let Some(fc) = prog_ctx.prog.files.get(stmt.file.as_u32() as usize) else {
        return Ok(());
    };
    let h = emit_connector_handles(fc, stmt.span, block, prog_ctx, state, loc)?;
    let dmask_v = emit_i32(encode_decl_mask(&stmt.decls), block, prog_ctx, state, loc);
    let rc = emit_rt_call(
        RtDispatchTarget::FileUnlock,
        &[h.spec_ptr, h.spec_len, h.status_ptr, dmask_v],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    emit_post_io_dispatch(
        rc,
        (h.spec_ptr, h.spec_len),
        (&[], &[], &[]),
        &stmt.decls,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Emits `(spec_ptr, spec_len, rec_ptr, rec_max_len, status_ptr)` for `fc`.
///
/// # Arguments
///
/// * `fc` - Resolved [`HirFileConnector`].
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] on MLIR build failure.
pub(crate) fn emit_connector_handles<'ctx, 'b, 'e: 'b>(
    fc: &HirFileConnector,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<ConnectorSsaHandles<'ctx, 'b>> {
    let wire = build_connector_wire_spec(fc, prog_ctx, state);
    let wire_bytes = encode_file_connector_spec(&wire);
    let (spec_ptr, spec_len) = emit_const_slice(&wire_bytes, block, prog_ctx, state, loc)?;

    let null_ptr = emit_null_ptr(block, prog_ctx, state, loc);
    let rec_ptr = if let Some(&rec0_id) = fc.records.first()
        && let Some(rec0_pl) = simple_item_place(rec0_id, prog_ctx, span)
    {
        lower_place_view(&rec0_pl, block, prog_ctx, state)?.ptr
    } else {
        null_ptr
    };
    let rec_max_len = emit_i64(
        i64::from(fc.max_record_bytes.max(1)),
        block,
        prog_ctx,
        state,
        loc,
    );

    let status_ptr = if let Some(st_id) = fc.status_item
        && let Some(st_pl) = simple_item_place(st_id, prog_ctx, span)
    {
        lower_place_view(&st_pl, block, prog_ctx, state)?.ptr
    } else {
        null_ptr
    };

    Ok(ConnectorSsaHandles {
        spec_ptr,
        spec_len,
        rec_ptr,
        rec_max_len,
        status_ptr,
    })
}

/// Evaluates [`HirWriteAdvancing`] into `(adv_kind_i32, adv_count_i64)`.
///
/// # Arguments
///
/// * `advancing` - HIR advancing specification.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
fn eval_write_advancing<'ctx, 'b, 'e: 'b>(
    advancing: &HirWriteAdvancing,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(i32, Value<'ctx, 'b>)> {
    match advancing {
        HirWriteAdvancing::Default => Ok((0_i32, emit_i64(0, block, prog_ctx, state, loc))),
        HirWriteAdvancing::Lines { side, count } => {
            let k = match side {
                HirAdvancingSide::Before => 1_i32,
                HirAdvancingSide::After => 2_i32,
            };
            let c = eval_expr_as_i64(count, block, prog_ctx, state)?;
            Ok((k, c))
        }
        HirWriteAdvancing::Page { side } => {
            let k = match side {
                HirAdvancingSide::Before => 3_i32,
                HirAdvancingSide::After => 4_i32,
            };
            Ok((k, emit_i64(0, block, prog_ctx, state, loc)))
        }
    }
}

/// Evaluates the active write byte length for `fc` (`depending_on_item` if declared, else `default_len`).
///
/// # Arguments
///
/// * `fc` - Target [`HirFileConnector`].
/// * `default_len` - Record place static/ODO byte length (`i64`).
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if evaluating `depending_on_item` fails.
pub(crate) fn eval_active_write_len<'ctx, 'b, 'e: 'b>(
    fc: &HirFileConnector,
    default_len: Value<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Value<'ctx, 'b>> {
    if let Some(dep_id) = fc.depending_on_item
        && let Some(dep_pl) = simple_item_place(dep_id, prog_ctx, span)
    {
        eval_expr_as_i64(&HirExpr::Place(dep_pl), block, prog_ctx, state)
    } else {
        Ok(default_len)
    }
}

/// Builds a [`HirPlace`] referencing an unsubscripted `DataItemId` in `prog_ctx`.
///
/// # Arguments
///
/// * `id` - Target [`DataItemId`].
/// * `prog_ctx` - Program lowering context.
/// * `span` - Source span.
pub(crate) fn simple_item_place(
    id: DataItemId,
    prog_ctx: &ProgramLowerCtx<'_, '_, '_>,
    span: SourceSpan,
) -> Option<HirPlace> {
    let it = prog_ctx.prog.data_items.get(id.as_u32() as usize)?;
    Some(HirPlace {
        item: id,
        static_offset: it.byte_offset,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: it.ty.clone(),
        check_mask: ExceptionCheckMask::default(),
        span,
    })
}

/// Synchronizes staged `DEPENDING ON`, `RELATIVE KEY`, and `LINAGE-COUNTER` values into COBOL storage.
///
/// # Arguments
///
/// * `fc` - Target [`HirFileConnector`].
/// * `flags` - Bitmask of `SYNC_REC_LEN`, `SYNC_REL_KEY`, and `SYNC_LINAGE`.
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] on MLIR build failure.
pub(crate) fn sync_connector_registers<'ctx, 'b, 'e: 'b>(
    fc: &HirFileConnector,
    flags: u8,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    if (flags & SYNC_REC_LEN) != 0
        && let Some(dep_id) = fc.depending_on_item
        && let Some(dep_pl) = simple_item_place(dep_id, prog_ctx, span)
    {
        let rec_len = emit_rt_call(
            RtDispatchTarget::FileLastRecLen,
            &[],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        emit_cond_store_ge(rec_len, 0, &dep_pl, block, prog_ctx, state, loc)?;
    }

    if (flags & SYNC_REL_KEY) != 0
        && let Some(rk_id) = fc.relative_key_item
        && let Some(rk_pl) = simple_item_place(rk_id, prog_ctx, span)
    {
        let rk_val = emit_rt_call(
            RtDispatchTarget::FileLastRelKey,
            &[],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        emit_cond_store_ge(rk_val, 1, &rk_pl, block, prog_ctx, state, loc)?;
    }

    if (flags & SYNC_LINAGE) != 0
        && let Some(lin) = &fc.linage
        && let Some(lc_pl) = simple_item_place(lin.counter_item, prog_ctx, span)
    {
        let lc_val = emit_rt_call(
            RtDispatchTarget::FileLastLinageCounter,
            &[],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        emit_cond_store_ge(lc_val, 1, &lc_pl, block, prog_ctx, state, loc)?;
    }
    Ok(())
}

/// Emits `if val >= min_bound { store_i64_to_place(val, place) }`.
///
/// # Arguments
///
/// * `val` - Staged `i64` SSA value.
/// * `min_bound` - Minimum valid threshold (`0` or `1`).
/// * `place` - Destination COBOL [`HirPlace`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
fn emit_cond_store_ge<'ctx, 'b, 'e: 'b>(
    val: Value<'ctx, 'b>,
    min_bound: i64,
    place: &HirPlace,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let bound_v = emit_i64(min_bound, block, prog_ctx, state, loc);
    let cond = append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Sge, val, bound_v, loc),
        prog_ctx.fallback_val,
    );
    let then_reg = Region::new();
    let then_blk = then_reg.append_block(Block::new(&[]));
    store_i64_to_place(val, place, &then_blk, prog_ctx, state, loc)?;
    let _ = then_blk.append_operation(scf::r#yield(&[], loc));
    let else_reg = Region::new();
    let else_blk = else_reg.append_block(Block::new(&[]));
    let _ = else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(cond, &[], then_reg, else_reg, loc));
    Ok(())
}

/// Emits structured post-I/O control flow (`INTO` moves, inline `AT END`/`INVALID KEY`/`EOP`, `DECLARATIVES`, and fatal abort).
///
/// # Arguments
///
/// * `rc` - `POST_IO_*` `i32` return value.
/// * `spec_slice` - `(spec_ptr, spec_len)` for fatal abort reporting.
/// * `bodies` - `(into_moves, on_stmts, not_on_stmts)` statement slices.
/// * `decls` - Candidate 5-slot [`HirFileDeclSet`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if nested statement lowering fails.
#[expect(
    clippy::too_many_arguments,
    reason = "Unified post-I/O control-flow builder shared across all M14/M15/M16 file verbs."
)]
pub(crate) fn emit_post_io_dispatch<'ctx, 'b, 'e: 'b>(
    rc: Value<'ctx, 'b>,
    spec_slice: (Value<'ctx, 'b>, Value<'ctx, 'b>),
    bodies: (&[HirStmt], &[HirStmt], &[HirStmt]),
    decls: &HirFileDeclSet,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let (into_moves, on_stmts, not_on_stmts) = bodies;

    if !into_moves.is_empty() {
        let c0 = emit_eq_i32(rc, POST_IO_CONTINUE, block, prog_ctx, state, loc);
        let c2 = emit_eq_i32(rc, POST_IO_FIRE_NOT_AT_END, block, prog_ctx, state, loc);
        let c4 = emit_eq_i32(
            rc,
            POST_IO_FIRE_NOT_INVALID_KEY,
            block,
            prog_ctx,
            state,
            loc,
        );
        let c02 = append_val(block, arith::ori(c0, c2, loc), prog_ctx.fallback_val);
        let is_ok = append_val(block, arith::ori(c02, c4, loc), prog_ctx.fallback_val);
        emit_size_error_branch(is_ok, into_moves, &[], loc, block, prog_ctx, state)?;
    }

    if !on_stmts.is_empty() {
        let c1 = emit_eq_i32(rc, POST_IO_FIRE_AT_END, block, prog_ctx, state, loc);
        let c3 = emit_eq_i32(rc, POST_IO_FIRE_INVALID_KEY, block, prog_ctx, state, loc);
        let c5 = emit_eq_i32(rc, POST_IO_FIRE_AT_EOP, block, prog_ctx, state, loc);
        let c13 = append_val(block, arith::ori(c1, c3, loc), prog_ctx.fallback_val);
        let fire_on = append_val(block, arith::ori(c13, c5, loc), prog_ctx.fallback_val);
        emit_size_error_branch(fire_on, on_stmts, &[], loc, block, prog_ctx, state)?;
    }

    if !not_on_stmts.is_empty() {
        let c2 = emit_eq_i32(rc, POST_IO_FIRE_NOT_AT_END, block, prog_ctx, state, loc);
        let c4 = emit_eq_i32(
            rc,
            POST_IO_FIRE_NOT_INVALID_KEY,
            block,
            prog_ctx,
            state,
            loc,
        );
        let c6 = emit_eq_i32(rc, POST_IO_FIRE_NOT_AT_EOP, block, prog_ctx, state, loc);
        let c24 = append_val(block, arith::ori(c2, c4, loc), prog_ctx.fallback_val);
        let fire_not = append_val(block, arith::ori(c24, c6, loc), prog_ctx.fallback_val);
        emit_size_error_branch(fire_not, not_on_stmts, &[], loc, block, prog_ctx, state)?;
    }

    for (code, slot) in [
        (POST_IO_INVOKE_DECL_FILE, decls.on_file),
        (POST_IO_INVOKE_DECL_INPUT, decls.on_input),
        (POST_IO_INVOKE_DECL_OUTPUT, decls.on_output),
        (POST_IO_INVOKE_DECL_IO, decls.on_io),
        (POST_IO_INVOKE_DECL_EXTEND, decls.on_extend),
    ] {
        if let Some(dref) = slot {
            emit_declarative_invocation(rc, code, dref, block, prog_ctx, state, loc)?;
        }
    }

    let is_fatal = emit_eq_i32(rc, POST_IO_FATAL_ABORT, block, prog_ctx, state, loc);
    let fatal_then = Region::new();
    let fatal_blk = fatal_then.append_block(Block::new(&[]));
    let _ = emit_rt_call(
        RtDispatchTarget::FileFatalAbort,
        &[spec_slice.0, spec_slice.1],
        &fatal_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = fatal_blk.append_operation(scf::r#yield(&[], loc));
    let fatal_else = Region::new();
    let fatal_else_blk = fatal_else.append_block(Block::new(&[]));
    let _ = fatal_else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(is_fatal, &[], fatal_then, fatal_else, loc));
    Ok(())
}

/// Emits `if rc == code { if decl_enter() != 0 { perform_range(dref); decl_leave(); } }`.
///
/// # Arguments
///
/// * `rc` - Post-I/O `i32` status action.
/// * `code` - Expected `POST_IO_INVOKE_DECL_*` constant.
/// * `dref` - Target [`HirDeclRef`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
fn emit_declarative_invocation<'ctx, 'b, 'e: 'b>(
    rc: Value<'ctx, 'b>,
    code: i32,
    dref: HirDeclRef,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let cond = emit_eq_i32(rc, code, block, prog_ctx, state, loc);
    let outer_then = Region::new();
    let outer_blk = outer_then.append_block(Block::new(&[]));

    let u_val = emit_i32(
        i32::try_from(dref.owner_unit.as_u32()).unwrap_or(0),
        &outer_blk,
        prog_ctx,
        state,
        loc,
    );
    let p_val = emit_i32(
        i32::try_from(dref.proc_start.as_u32()).unwrap_or(0),
        &outer_blk,
        prog_ctx,
        state,
        loc,
    );
    let entered = emit_rt_call(
        RtDispatchTarget::DeclEnter,
        &[u_val, p_val],
        &outer_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let z0 = emit_i32(0, &outer_blk, prog_ctx, state, loc);
    let can_run = append_val(
        &outer_blk,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, entered, z0, loc),
        prog_ctx.fallback_val,
    );

    let inner_then = Region::new();
    let inner_blk = inner_then.append_block(Block::new(&[]));
    let target_prog_opt = if dref.owner_unit == prog_ctx.prog.id {
        None
    } else {
        state
            .program_symbols
            .get(&dref.owner_unit)
            .map(|(canon, _)| canon.clone())
    };
    let pfm_op = CobolPerformRangeOp {
        start_proc: dref.proc_start.as_u32(),
        end_proc: dref.proc_end.as_u32(),
    }
    .build_with_target_prog(target_prog_opt.as_deref(), state.ctx, loc)?;
    let _ = inner_blk.append_operation(pfm_op);
    let _ = emit_rt_call(
        RtDispatchTarget::DeclLeave,
        &[u_val, p_val],
        &inner_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = inner_blk.append_operation(scf::r#yield(&[], loc));

    let inner_else = Region::new();
    let inner_else_blk = inner_else.append_block(Block::new(&[]));
    let _ = inner_else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = outer_blk.append_operation(scf::r#if(can_run, &[], inner_then, inner_else, loc));
    let _ = outer_blk.append_operation(scf::r#yield(&[], loc));

    let outer_else = Region::new();
    let outer_else_blk = outer_else.append_block(Block::new(&[]));
    let _ = outer_else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(cond, &[], outer_then, outer_else, loc));
    Ok(())
}

/// Converts a [`HirFileConnector`] into its serializable [`FileConnectorWireSpec`].
///
/// # Arguments
///
/// * `fc` - Resolved HIR file connector.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn build_connector_wire_spec(
    fc: &HirFileConnector,
    prog_ctx: &ProgramLowerCtx<'_, '_, '_>,
    state: &ModuleLowerState<'_, '_>,
) -> FileConnectorWireSpec {
    let identity = match &fc.sharing {
        HirConnectorSharing::UnitOwned {
            owner_unit,
            owner_fc,
        } => WireConnectorIdentity::UnitOwned {
            owner_unit: owner_unit.as_u32(),
            owner_fc: owner_fc.as_u32(),
        },
        HirConnectorSharing::External { external_name } => {
            WireConnectorIdentity::External(external_name.clone())
        }
    };
    let file_name = state
        .interner
        .resolve_original(fc.name)
        .map_or_else(|_| String::from("FILE"), String::from);
    let assign = match &fc.assign {
        HirAssignSpec::Literal(s) => WireAssignKind::Literal(s.clone()),
        HirAssignSpec::ExternalEnv(s) => WireAssignKind::ExternalEnv(s.clone()),
        HirAssignSpec::DynamicItem(_) => WireAssignKind::DynamicBuffer,
        HirAssignSpec::EnvOrLiteral(s) => WireAssignKind::EnvOrLiteral(s.clone()),
        HirAssignSpec::DisplayDevice => WireAssignKind::DisplayDevice,
        HirAssignSpec::KeyboardDevice => WireAssignKind::KeyboardDevice,
    };
    let relative_key_digits = fc
        .relative_key_item
        .and_then(|id| prog_ctx.prog.data_items.get(id.as_u32() as usize))
        .map_or(0, |it| extract_decimal_digits(&it.ty));
    let collating_ranks = fc
        .collating_alphabet
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

    FileConnectorWireSpec {
        identity,
        file_name,
        organization: fc.organization,
        access_mode: fc.access_mode,
        optionality: fc.optionality,
        assign,
        record_format: fc.record_format,
        min_record_bytes: fc.min_record_bytes.max(1),
        max_record_bytes: fc.max_record_bytes.max(fc.min_record_bytes).max(1),
        relative_key_digits,
        lock_mode: fc.lock_mode,
        sharing_mode: fc.sharing_mode,
        padding_byte: fc.padding_byte,
        print_mode: fc.print_mode,
        collating_ranks,
        primary_key_segments: fc.primary_key_segments.clone(),
        alternate_keys: fc
            .alternate_keys
            .iter()
            .map(|ak| WireAltKey {
                segments: ak.segments.clone(),
                duplicates: ak.duplicates,
                suppression: ak.suppression.clone(),
            })
            .collect(),
    }
}

/// Extracts the declared decimal digit capacity (`1..=38`) of a `RELATIVE KEY` [`HirType`] (`0` if unbounded `COMP-5`).
///
/// # Arguments
///
/// * `ty` - `RELATIVE KEY` HIR type.
const fn extract_decimal_digits(ty: &HirType) -> u8 {
    match ty {
        HirType::ZonedDecimal { digits, .. } | HirType::PackedDecimal { digits, .. } => {
            digits.get()
        }
        HirType::BinaryInt {
            digits,
            trunc: coblang_hir::TruncationPolicy::TruncateToPic,
            ..
        } => digits.get(),
        _ => 0,
    }
}

/// Emits `(dyn_ptr, dyn_len)` for `HirAssignSpec::DynamicItem` (or `(null, 0)` otherwise).
///
/// # Arguments
///
/// * `fc` - Target connector.
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
pub(crate) fn emit_dynamic_assign_slice<'ctx, 'b, 'e: 'b>(
    fc: &HirFileConnector,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    if let HirAssignSpec::DynamicItem(did) = fc.assign
        && let Some(pl) = simple_item_place(did, prog_ctx, span)
    {
        let pv = lower_place_view(&pl, block, prog_ctx, state)?;
        Ok((pv.ptr, pv.len_i64))
    } else {
        Ok((
            emit_null_ptr(block, prog_ctx, state, loc),
            emit_i64(0, block, prog_ctx, state, loc),
        ))
    }
}

/// Evaluates `(body, footing, top, bottom)` `i64` operands for `LINAGE` at `OPEN`.
///
/// # Arguments
///
/// * `fc` - Target connector.
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
fn eval_linage_operands<'ctx, 'b, 'e: 'b>(
    fc: &HirFileConnector,
    span: SourceSpan,
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
    let z0 = emit_i64(0, block, prog_ctx, state, loc);
    let Some(lin) = &fc.linage else {
        return Ok((z0, z0, z0, z0));
    };
    let body = eval_one_linage_param(Some(lin.body), span, block, prog_ctx, state, loc)?;
    let foot = eval_one_linage_param(lin.footing, span, block, prog_ctx, state, loc)?;
    let top = eval_one_linage_param(lin.top, span, block, prog_ctx, state, loc)?;
    let bot = eval_one_linage_param(lin.bottom, span, block, prog_ctx, state, loc)?;
    Ok((body, foot, top, bot))
}

/// Evaluates an optional [`HirLinageParam`] to an `i64` SSA value (`0` if `None`).
///
/// # Arguments
///
/// * `param` - Optional `LINAGE` parameter.
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
fn eval_one_linage_param<'ctx, 'b, 'e: 'b>(
    param: Option<HirLinageParam>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    match param {
        None => Ok(emit_i64(0, block, prog_ctx, state, loc)),
        Some(HirLinageParam::Static(n)) => Ok(emit_i64(i64::from(n), block, prog_ctx, state, loc)),
        Some(HirLinageParam::Dynamic(did)) => {
            if let Some(pl) = simple_item_place(did, prog_ctx, span) {
                eval_expr_as_i64(&HirExpr::Place(pl), block, prog_ctx, state)
            } else {
                Ok(emit_i64(0, block, prog_ctx, state, loc))
            }
        }
    }
}

/// Evaluates `fc.relative_key_item` as an `i64` SSA value (`0` when absent).
///
/// # Arguments
///
/// * `fc` - Target connector.
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
pub(crate) fn eval_rel_key_i64<'ctx, 'b, 'e: 'b>(
    fc: &HirFileConnector,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    if let Some(rk_id) = fc.relative_key_item
        && let Some(pl) = simple_item_place(rk_id, prog_ctx, span)
    {
        eval_expr_as_i64(&HirExpr::Place(pl), block, prog_ctx, state)
    } else {
        Ok(emit_i64(0, block, prog_ctx, state, loc))
    }
}

/// Emits `llvm.mlir.zero : !llvm.ptr`.
///
/// # Arguments
///
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
pub(crate) fn emit_null_ptr<'ctx, 'b, 'e: 'b>(
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    append_val(
        block,
        llvm::zero(llvm_ptr_type(state.ctx), loc),
        prog_ctx.fallback_val,
    )
}

/// Emits `arith.cmpi eq, lhs, const_i32(rhs)`.
///
/// # Arguments
///
/// * `lhs` - `i32` value.
/// * `rhs` - `i32` constant.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - MLIR location.
pub(crate) fn emit_eq_i32<'ctx, 'b, 'e: 'b>(
    lhs: Value<'ctx, 'b>,
    rhs: i32,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    let rv = emit_i32(rhs, block, prog_ctx, state, loc);
    append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, lhs, rv, loc),
        prog_ctx.fallback_val,
    )
}

/// Encodes [`HirFileDeclSet`] into a 5-bit `DECL_MASK_*` integer.
///
/// # Arguments
///
/// * `decls` - Candidate declarative bindings.
pub(crate) const fn encode_decl_mask(decls: &HirFileDeclSet) -> i32 {
    let mut m = 0_i32;
    if decls.on_file.is_some() {
        m |= DECL_MASK_FILE;
    }
    if decls.on_input.is_some() {
        m |= DECL_MASK_INPUT;
    }
    if decls.on_output.is_some() {
        m |= DECL_MASK_OUTPUT;
    }
    if decls.on_io.is_some() {
        m |= DECL_MASK_IO;
    }
    if decls.on_extend.is_some() {
        m |= DECL_MASK_EXTEND;
    }
    m
}

/// Encodes `(has_on, has_not_on)` into `ABI_INLINE_*` bitmask.
///
/// # Arguments
///
/// * `on_presence` - Presence of `ON` branch.
/// * `has_not_on` - Whether `NOT ON` branch is non-empty.
const fn encode_inline_flags(on_presence: HirInlineClausePresence, has_not_on: bool) -> i32 {
    let mut f = match on_presence {
        HirInlineClausePresence::Absent => 0,
        HirInlineClausePresence::Present => ABI_INLINE_HAS_ON,
    };
    if has_not_on {
        f |= ABI_INLINE_HAS_NOT_ON;
    }
    f
}

/// Maps [`HirOpenMode`] to `0..=3`.
///
/// # Arguments
///
/// * `mode` - Open mode.
const fn encode_open_mode(mode: HirOpenMode) -> i32 {
    match mode {
        HirOpenMode::Input => 0,
        HirOpenMode::Output => 1,
        HirOpenMode::Io => 2,
        HirOpenMode::Extend => 3,
    }
}

/// Maps [`HirOpenOption`] to `0..=6`.
///
/// # Arguments
///
/// * `opt` - Open option.
const fn encode_open_option(opt: HirOpenOption) -> i32 {
    match opt {
        HirOpenOption::Standard => 0,
        HirOpenOption::WithNoRewind => 1,
        HirOpenOption::Reversed => 2,
        HirOpenOption::WithLock => 3,
        HirOpenOption::SharingAllOther => 4,
        HirOpenOption::SharingNoOther => 5,
        HirOpenOption::SharingReadOnly => 6,
    }
}

/// Maps [`HirCloseDisposition`] to `0..=4`.
///
/// # Arguments
///
/// * `disp` - Close disposition.
const fn encode_close_disp(disp: HirCloseDisposition) -> i32 {
    match disp {
        HirCloseDisposition::Standard => 0,
        HirCloseDisposition::WithLock => 1,
        HirCloseDisposition::WithNoRewind => 2,
        HirCloseDisposition::ReelUnit => 3,
        HirCloseDisposition::ReelUnitForRemoval => 4,
    }
}

/// Maps [`HirReadLockPolicy`] to `0..=4`.
///
/// # Arguments
///
/// * `pol` - Read lock policy.
const fn encode_read_lock(pol: HirReadLockPolicy) -> i32 {
    match pol {
        HirReadLockPolicy::Default => 0,
        HirReadLockPolicy::WithLock => 1,
        HirReadLockPolicy::WithKeptLock => 2,
        HirReadLockPolicy::WithNoLock => 3,
        HirReadLockPolicy::IgnoreLock => 4,
    }
}
