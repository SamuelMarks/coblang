//! Lowering of Milestone M11 pointers (`USAGE POINTER` / `PROGRAM-POINTER`), `ADDRESS OF`,
//! `BASED` / `LINKAGE` indirect base pointers, `SET` pointer/address/entry operations,
//! `ALLOCATE`, and `FREE` from HIR to `cobol` MLIR.

use coblang_common::{
    ByteLength, ByteOffset, DataItemId, DigitCount, FeatureTag, Result, ScaleFactor, SourceSpan,
};
use coblang_hir::{
    BinaryEndianness, DataCategory, ExceptionCheckMask, HirAllocateInit, HirAllocateSize,
    HirAllocateSpec, HirCallArg, HirCallReturning, HirCallSpec, HirConstValue, HirExpr,
    HirFigurative, HirFreeTarget, HirInitialState, HirParamSource, HirPassingMode, HirPlace,
    HirPointerTarget, HirPointerValue, HirProgram, HirRelOp, HirSetPointerSpec, HirStepDir,
    HirStorageSection, HirType, Signedness, TruncationPolicy,
};
use coblang_rt_core::field_ops::RuntimeMoveSourceKind;
use melior::dialect::arith::CmpiPredicate;
use melior::dialect::{arith, llvm, scf};
use melior::ir::attribute::{IntegerAttribute, TypeAttribute};
use melior::ir::block::BlockLike;
use melior::ir::operation::OperationBuilder;
use melior::ir::r#type::{IntegerType, Type};
use melior::ir::{Block, Location, Region, RegionLike, Value};

use crate::lower_from_hir::{
    ModuleLowerState, ProgramLowerCtx, append_val, const_to_display_bytes, emit_const_slice,
    emit_i32, emit_i64, emit_rt_call, emit_spec_slice, emit_storage_ptr, eval_expr_as_i64,
    expr_span, lower_intrinsic_as_move_source, lower_place_view, lower_place_view_allow_null,
    lower_stmt, unimpl,
};
use crate::ops::{
    CallDispatchKind, CobolCallOp, CobolGetReturnCodeOp, CobolMoveOp, CobolSetReturnCodeOp,
    CobolStorageOp, ExceptionBranches, MoveEncodingKind, RtDispatchTarget, StorageKind,
    llvm_ptr_type,
};
use crate::storage_init::{
    ResolvedItemStorage, RootBackingKind, RootInitStrategy, RootStorageRecord,
    make_runtime_field_spec,
};

/// Null-pointer check policy when resolving an [`RootBackingKind::IndirectBasePointer`] item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum NullPtrPolicy {
    /// Emit `@coblang_rt_check_based` before dereference (`EC-DATA-PTR-NULL`).
    CheckNonNull,
    /// Allow `NULL` base pointer when computing `ADDRESS OF item` (ISO 2023 `NULL` propagation).
    AllowNullForAddressOf,
}

/// Computes the unsubscripted element `(1, 1, ...)` base pointer for `item_loc`, loading indirect
/// `BASED` / `LINKAGE` / `EXTERNAL` pointers and optionally checking `EC-DATA-PTR-NULL`.
///
/// Returns `(element_base_ptr, opt_loaded_root_base_ptr)`.
///
/// # Arguments
///
/// * `item_loc` - Resolved storage layout of the accessed item.
/// * `place` - Accessed [`HirPlace`] (`item` + `span`).
/// * `policy` - [`NullPtrPolicy`] governing `BASED` `NULL` trapping vs `ADDRESS OF` passthrough.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
pub(crate) fn emit_item_base_ptr<'ctx, 'b, 'e: 'b>(
    item_loc: &ResolvedItemStorage,
    place: &HirPlace,
    policy: NullPtrPolicy,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Option<Value<'ctx, 'b>>)> {
    let root_backing = prog_ctx
        .storage
        .roots
        .get(item_loc.root_idx)
        .map_or(RootBackingKind::DirectBuffer, |r| r.backing);

    if matches!(
        root_backing,
        RootBackingKind::DirectBuffer | RootBackingKind::InheritedGlobalBuffer
    ) {
        let ptr = emit_storage_ptr(
            &item_loc.root_storage_sym,
            item_loc.base_byte_offset,
            item_loc.elem_byte_len,
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok((ptr, None));
    }

    let slot_ptr = emit_storage_ptr(
        &item_loc.root_storage_sym,
        0,
        8,
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let ptr_ty = llvm_ptr_type(state.ctx);
    let raw_base = append_val(
        block,
        llvm::load(
            state.ctx.raw(),
            slot_ptr,
            ptr_ty,
            loc,
            llvm::LoadStoreOptions::new(),
        ),
        prog_ctx.fallback_val,
    );

    if root_backing == RootBackingKind::IndirectBasePointer && policy == NullPtrPolicy::CheckNonNull
    {
        emit_null_based_guard(
            raw_base,
            item_loc.root_id,
            place,
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }

    let eff_ptr = if item_loc.base_byte_offset == 0 {
        raw_base
    } else {
        let off_v = emit_i64(
            i64::from(item_loc.base_byte_offset),
            block,
            prog_ctx,
            state,
            loc,
        );
        let i8_ty = IntegerType::new(state.ctx.raw(), 8).into();
        let gep =
            llvm::get_element_ptr_dynamic(state.ctx.raw(), raw_base, &[off_v], i8_ty, ptr_ty, loc);
        append_val(block, gep, prog_ctx.fallback_val)
    };
    Ok((eff_ptr, Some(raw_base)))
}

/// Emits `@coblang_rt_check_based` with `file:line`, root name, and accessed item name.
///
/// # Arguments
///
/// * `raw_base` - Loaded `!llvm.ptr` base pointer.
/// * `root_id` - Owning `01`/`77` root [`DataItemId`].
/// * `place` - Accessed [`HirPlace`] (`item` + `span`).
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_null_based_guard<'ctx, 'b, 'e: 'b>(
    raw_base: Value<'ctx, 'b>,
    root_id: DataItemId,
    place: &HirPlace,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let r = state.source_map.resolve_span(place.span)?;
    let fname = std::path::Path::new(&r.file_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&r.file_path);
    let site_text = format!("{fname}:{}", r.line.get());
    let raw_root_name = resolve_item_original_name(root_id, prog_ctx, state);
    let is_using_param = prog_ctx
        .prog
        .using_params
        .iter()
        .any(|up| up.source == HirParamSource::Using && up.item == root_id);
    let root_name = if is_using_param {
        format!("L:{raw_root_name}")
    } else {
        raw_root_name
    };
    let item_name = if place.item == root_id {
        String::new()
    } else {
        resolve_item_original_name(place.item, prog_ctx, state)
    };
    let (lp, ll) = emit_const_slice(site_text.as_bytes(), block, prog_ctx, state, loc)?;
    let (rp, rl) = emit_const_slice(root_name.as_bytes(), block, prog_ctx, state, loc)?;
    let (ip, il) = emit_const_slice(item_name.as_bytes(), block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::CheckBased,
        &[raw_base, lp, ll, rp, rl, ip, il],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Resolves the original source spelling of `item_id` (or `"FILLER"` when unnamed).
///
/// # Arguments
///
/// * `item_id` - Target [`DataItemId`].
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn resolve_item_original_name(
    item_id: DataItemId,
    prog_ctx: &ProgramLowerCtx<'_, '_, '_>,
    state: &ModuleLowerState<'_, '_>,
) -> String {
    prog_ctx
        .prog
        .data_items
        .get(item_id.as_u32() as usize)
        .and_then(|it| it.name)
        .and_then(|sym| state.interner.resolve_original(sym).ok())
        .unwrap_or("FILLER")
        .to_owned()
}

/// Emits an `llvm.ptrtoint` operation converting `!llvm.ptr` `ptr_val` into `i64`.
///
/// # Arguments
///
/// * `ptr_val` - Pointer SSA operand.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
pub(crate) fn emit_ptr_to_i64<'ctx, 'b, 'e: 'b>(
    ptr_val: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    let raw = state.ctx.raw();
    let i64_ty = IntegerType::new(raw, 64).into();
    let op = OperationBuilder::new("llvm.ptrtoint", loc)
        .add_operands(&[ptr_val])
        .add_results(&[i64_ty])
        .build()
        .unwrap_or(llvm::unreachable(loc));
    append_val(block, op, prog_ctx.fallback_val)
}

/// Propagates `NULL` for `ADDRESS OF sub-item` when `raw_base` is `NULL` (ISO 2023 14.7.2).
///
/// # Arguments
///
/// * `raw_base` - Unoffset base pointer loaded from `@__cobol_ws_<prog>_<root>`.
/// * `computed_ptr` - Computed element/field pointer.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
pub(crate) fn select_null_if_base_null<'ctx, 'b, 'e: 'b>(
    raw_base: Value<'ctx, 'b>,
    computed_ptr: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let base_int = emit_ptr_to_i64(raw_base, block, prog_ctx, state, loc);
    let z64 = emit_i64(0, block, prog_ctx, state, loc);
    let is_null = append_val(
        block,
        arith::cmpi(raw, CmpiPredicate::Eq, base_int, z64, loc),
        prog_ctx.fallback_val,
    );
    let null_ptr = append_val(block, llvm::zero(ptr_ty, loc), prog_ctx.fallback_val);
    append_val(
        block,
        arith::select(is_null, null_ptr, computed_ptr, loc),
        prog_ctx.fallback_val,
    )
}

/// Evaluates `ADDRESS OF place` into an `!llvm.ptr` SSA value without triggering `CheckBased`.
///
/// # Arguments
///
/// * `place` - Target [`HirPlace`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if place view lowering fails.
pub(crate) fn eval_address_of_place<'ctx, 'b, 'e: 'b>(
    place: &HirPlace,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Value<'ctx, 'b>> {
    Ok(lower_place_view_allow_null(place, block, prog_ctx, state)?.ptr)
}

/// Returns `true` when `lhs <op> rhs` compares pointers, `ADDRESS OF`, or `NULL`.
///
/// # Arguments
///
/// * `lhs` - Left-hand expression.
/// * `rhs` - Right-hand expression.
#[must_use]
pub(crate) fn is_pointer_compare_pair(lhs: &HirExpr, rhs: &HirExpr) -> bool {
    let is_ptr_expr = |e: &HirExpr| match e {
        HirExpr::AddressOf(_) => true,
        HirExpr::Place(pl) => matches!(pl.effective_ty, HirType::Pointer | HirType::ProgramPointer),
        _ => false,
    };
    let is_null_lit = |e: &HirExpr| {
        matches!(
            e,
            HirExpr::Const {
                value: HirConstValue::Figurative(HirFigurative::Null),
                ..
            }
        )
    };
    is_ptr_expr(lhs) || is_ptr_expr(rhs) || (is_null_lit(lhs) && is_null_lit(rhs))
}

/// Lowers pointer relational comparison `lhs <op> rhs` into `i1`.
///
/// # Arguments
///
/// * `op` - Relational operator.
/// * `lhs` - Left-hand expression.
/// * `rhs` - Right-hand expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if operand lowering fails.
pub(crate) fn lower_pointer_compare_cond<'ctx, 'b, 'e: 'b>(
    op: HirRelOp,
    lhs: &HirExpr,
    rhs: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let lv = eval_ptr_expr_as_i64(lhs, block, prog_ctx, state, loc)?;
    let rv = eval_ptr_expr_as_i64(rhs, block, prog_ctx, state, loc)?;
    let pred = match op {
        HirRelOp::Eq => CmpiPredicate::Eq,
        HirRelOp::Ne => CmpiPredicate::Ne,
        HirRelOp::Lt => CmpiPredicate::Ult,
        HirRelOp::Le => CmpiPredicate::Ule,
        HirRelOp::Gt => CmpiPredicate::Ugt,
        HirRelOp::Ge => CmpiPredicate::Uge,
    };
    Ok(append_val(
        block,
        arith::cmpi(state.ctx.raw(), pred, lv, rv, loc),
        prog_ctx.fallback_val,
    ))
}

/// Evaluates a pointer/`ADDRESS OF`/`NULL` expression as a 64-bit unsigned address (`i64`).
///
/// # Arguments
///
/// * `expr` - Pointer expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn eval_ptr_expr_as_i64<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let raw = state.ctx.raw();
    let i64_ty = IntegerType::new(raw, 64).into();
    match expr {
        HirExpr::AddressOf(pl) => {
            let p = eval_address_of_place(pl, block, prog_ctx, state)?;
            Ok(emit_ptr_to_i64(p, block, prog_ctx, state, loc))
        }
        HirExpr::Place(pl)
            if matches!(pl.effective_ty, HirType::Pointer | HirType::ProgramPointer) =>
        {
            let pv = lower_place_view(pl, block, prog_ctx, state)?;
            Ok(append_val(
                block,
                llvm::load(raw, pv.ptr, i64_ty, loc, llvm::LoadStoreOptions::new()),
                prog_ctx.fallback_val,
            ))
        }
        HirExpr::Const {
            value:
                HirConstValue::Figurative(
                    HirFigurative::Null | HirFigurative::Zero | HirFigurative::LowValue,
                ),
            ..
        } => Ok(emit_i64(0, block, prog_ctx, state, loc)),
        other => eval_expr_as_i64(other, block, prog_ctx, state),
    }
}

/// Lowers `HirStmt::SetPointer` (`SET ptr / ADDRESS OF item TO ...` and `UP/DOWN BY`).
///
/// # Arguments
///
/// * `spec` - Resolved [`HirSetPointerSpec`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if source or target lowering fails.
pub(crate) fn lower_set_pointer_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirSetPointerSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let ptr_ty = llvm_ptr_type(state.ctx);
    match &spec.value {
        HirPointerValue::StepBytes { direction, amount } => {
            let amt_v = eval_expr_as_i64(amount, block, prog_ctx, state)?;
            let signed_delta = match direction {
                HirStepDir::Up => amt_v,
                HirStepDir::Down => {
                    let z64 = emit_i64(0, block, prog_ctx, state, loc);
                    append_val(block, arith::subi(z64, amt_v, loc), prog_ctx.fallback_val)
                }
            };
            let i8_ty = IntegerType::new(state.ctx.raw(), 8).into();
            for tgt in &spec.targets {
                let slot_ptr = resolve_target_slot_ptr(tgt, block, prog_ctx, state, loc)?;
                let cur_ptr = append_val(
                    block,
                    llvm::load(
                        state.ctx.raw(),
                        slot_ptr,
                        ptr_ty,
                        loc,
                        llvm::LoadStoreOptions::new(),
                    ),
                    prog_ctx.fallback_val,
                );
                let next_ptr = append_val(
                    block,
                    llvm::get_element_ptr_dynamic(
                        state.ctx.raw(),
                        cur_ptr,
                        &[signed_delta],
                        i8_ty,
                        ptr_ty,
                        loc,
                    ),
                    prog_ctx.fallback_val,
                );
                store_ptr_to_slot(next_ptr, slot_ptr, block, state, loc);
            }
            Ok(())
        }
        other_val => {
            let rhs_ptr = eval_pointer_rhs(other_val, spec.span, block, prog_ctx, state, loc)?;
            for tgt in &spec.targets {
                let slot_ptr = resolve_target_slot_ptr(tgt, block, prog_ctx, state, loc)?;
                store_ptr_to_slot(rhs_ptr, slot_ptr, block, state, loc);
            }
            Ok(())
        }
    }
}

/// Evaluates a non-stepping [`HirPointerValue`] (`Null`, `AddressOf`, `PointerPlace`, `Entry`) into `!llvm.ptr`.
///
/// # Arguments
///
/// * `val` - Pointer value source.
/// * `span` - Enclosing statement span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn eval_pointer_rhs<'ctx, 'b, 'e: 'b>(
    val: &HirPointerValue,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let ptr_ty = llvm_ptr_type(state.ctx);
    match val {
        HirPointerValue::Null | HirPointerValue::StepBytes { .. } => Ok(append_val(
            block,
            llvm::zero(ptr_ty, loc),
            prog_ctx.fallback_val,
        )),
        HirPointerValue::AddressOf(pl) => eval_address_of_place(pl, block, prog_ctx, state),
        HirPointerValue::PointerPlace(pl) => {
            let pv = lower_place_view(pl, block, prog_ctx, state)?;
            Ok(append_val(
                block,
                llvm::load(
                    state.ctx.raw(),
                    pv.ptr,
                    ptr_ty,
                    loc,
                    llvm::LoadStoreOptions::new(),
                ),
                prog_ctx.fallback_val,
            ))
        }
        HirPointerValue::Entry(entry_expr) => {
            let (np, nl) = lower_raw_bytes_expr(entry_expr, span, block, prog_ctx, state, loc)?;
            emit_rt_call(
                RtDispatchTarget::ResolveEntry,
                &[np, nl],
                block,
                prog_ctx,
                state,
                loc,
            )
        }
    }
}

/// Resolves the 8-byte `!llvm.ptr` storage slot address of a [`HirPointerTarget`].
///
/// # Arguments
///
/// * `target` - Pointer place or `BaseAddressOf` root target.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn resolve_target_slot_ptr<'ctx, 'b, 'e: 'b>(
    target: &HirPointerTarget,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    match target {
        HirPointerTarget::PointerPlace(pl) => Ok(lower_place_view(pl, block, prog_ctx, state)?.ptr),
        HirPointerTarget::BaseAddressOf(root_id) => {
            let root = find_storage_root(*root_id, prog_ctx, state, prog_ctx.prog.span)?;
            emit_storage_ptr(&root.sym_name, 0, 8, block, prog_ctx, state, loc)
        }
    }
}

/// Stores an `!llvm.ptr` value `val` into `slot_ptr`.
///
/// # Arguments
///
/// * `val` - Pointer SSA value to store.
/// * `slot_ptr` - Destination 8-byte slot pointer.
/// * `block` - Target insertion block.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn store_ptr_to_slot<'ctx, 'b>(
    val: Value<'ctx, 'b>,
    slot_ptr: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) {
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        val,
        slot_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
}

/// Finds the [`RootStorageRecord`] for `root_id`.
///
/// # Arguments
///
/// * `root_id` - Primary `01`/`77` [`DataItemId`].
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `span` - Diagnostic source span.
fn find_storage_root<'p>(
    root_id: DataItemId,
    prog_ctx: &'p ProgramLowerCtx<'_, '_, '_>,
    state: &ModuleLowerState<'_, '_>,
    span: SourceSpan,
) -> Result<&'p RootStorageRecord> {
    prog_ctx
        .storage
        .roots
        .iter()
        .find(|r| r.root_id == root_id)
        .ok_or_else(|| {
            unimpl(
                FeatureTag::FeatureAllocateBased,
                "unresolved BASED/LINKAGE root storage",
                state.source_map,
                span,
            )
        })
}

/// Lowers `HirStmt::Allocate` (ISO 2023 Rules 6–9 + `INITIALIZED TO expr`).
///
/// # Arguments
///
/// * `spec` - Resolved [`HirAllocateSpec`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if allocation operand lowering fails.
pub(crate) fn lower_allocate_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirAllocateSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let def_fill = i32::from(prog_ctx.prog.formatting.default_byte.unwrap_or(0));
    let alloc_ptr = match &spec.size {
        HirAllocateSize::CharCount(sz_expr) => {
            let sz_v = eval_expr_as_i64(sz_expr, block, prog_ctx, state)?;
            lower_char_count_alloc(sz_v, spec, def_fill, block, prog_ctx, state, loc)?
        }
        HirAllocateSize::BasedItem(root_id) => {
            lower_based_item_alloc(*root_id, spec, def_fill, block, prog_ctx, state, loc)?
        }
    };
    if let Some(ret_pl) = &spec.returning {
        let ret_pv = lower_place_view(ret_pl, block, prog_ctx, state)?;
        store_ptr_to_slot(alloc_ptr, ret_pv.ptr, block, state, loc);
    }
    Ok(())
}

/// Lowers `ALLOCATE expr CHARACTERS [INITIALIZED [TO expr]]` into `!llvm.ptr`.
///
/// # Arguments
///
/// * `sz_v` - Requested byte count (`i64`).
/// * `spec` - Enclosing [`HirAllocateSpec`] (`init` + `span`).
/// * `def_fill` - Active `default_byte` (`0..=255`).
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_char_count_alloc<'ctx, 'b, 'e: 'b>(
    sz_v: Value<'ctx, 'b>,
    spec: &HirAllocateSpec,
    def_fill: i32,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    match &spec.init {
        HirAllocateInit::Uninitialized | HirAllocateInit::InitializedDefault => {
            let fill_v = emit_i32(def_fill, block, prog_ctx, state, loc);
            emit_rt_call(
                RtDispatchTarget::Alloc,
                &[sz_v, fill_v],
                block,
                prog_ctx,
                state,
                loc,
            )
        }
        HirAllocateInit::InitializedTo(fill_expr) => {
            if let HirExpr::Const {
                value: HirConstValue::Figurative(fig),
                ..
            } = fill_expr
            {
                let (pat_bytes, pad_code) = match fig {
                    HirFigurative::Zero => (&b"0"[..], i32::from(b'0')),
                    HirFigurative::Space => (&b" "[..], i32::from(b' ')),
                    HirFigurative::HighValue => (&[0xFF][..], 0xFF),
                    HirFigurative::LowValue | HirFigurative::Null => (&[0x00][..], 0x00),
                    HirFigurative::Quote => (&b"\""[..], i32::from(b'"')),
                    HirFigurative::All(p) => (p.as_slice(), -1),
                };
                let (pp, pl) = emit_const_slice(pat_bytes, block, prog_ctx, state, loc)?;
                let pad_v = emit_i32(pad_code, block, prog_ctx, state, loc);
                return emit_rt_call(
                    RtDispatchTarget::AllocFillPattern,
                    &[sz_v, pp, pl, pad_v],
                    block,
                    prog_ctx,
                    state,
                    loc,
                );
            }
            let (pp, pl) = lower_raw_bytes_expr(fill_expr, spec.span, block, prog_ctx, state, loc)?;
            let space_pad = emit_i32(i32::from(b' '), block, prog_ctx, state, loc);
            emit_rt_call(
                RtDispatchTarget::AllocFillPattern,
                &[sz_v, pp, pl, space_pad],
                block,
                prog_ctx,
                state,
                loc,
            )
        }
    }
}

/// Lowers `ALLOCATE based-item [INITIALIZED]` (ISO 2023 Rules 7 & 9), storing the new pointer into
/// `ADDRESS OF based-item` and returning `!llvm.ptr`.
///
/// # Arguments
///
/// * `root_id` - `01`/`77` `BASED` root [`DataItemId`].
/// * `spec` - Enclosing [`HirAllocateSpec`] (`init` + `span`).
/// * `def_fill` - Active `default_byte` (`0..=255`).
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_based_item_alloc<'ctx, 'b, 'e: 'b>(
    root_id: DataItemId,
    spec: &HirAllocateSpec,
    def_fill: i32,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let root = find_storage_root(root_id, prog_ctx, state, spec.span)?.clone();
    let sz_v = emit_i64(i64::from(root.byte_size), block, prog_ctx, state, loc);
    let fill_v = emit_i32(def_fill, block, prog_ctx, state, loc);
    let raw_ptr = emit_rt_call(
        RtDispatchTarget::Alloc,
        &[sz_v, fill_v],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    match &spec.init {
        HirAllocateInit::Uninitialized => {
            if def_fill != 0 {
                zero_pointer_leaves(
                    raw_ptr,
                    &root.pointer_zero_offsets,
                    block,
                    prog_ctx,
                    state,
                    loc,
                );
            }
        }
        HirAllocateInit::InitializedDefault | HirAllocateInit::InitializedTo(_) => {
            apply_based_root_value_image(raw_ptr, &root, block, prog_ctx, state, loc)?;
        }
    }

    let slot_ptr = emit_storage_ptr(&root.sym_name, 0, 8, block, prog_ctx, state, loc)?;
    store_ptr_to_slot(raw_ptr, slot_ptr, block, state, loc);
    Ok(raw_ptr)
}

/// Copies a `BASED` root's compile-time `VALUE`/default image (`StaticImage` or `DeferredBss`) into `raw_ptr`.
///
/// # Arguments
///
/// * `raw_ptr` - Newly allocated heap pointer.
/// * `root` - [`RootStorageRecord`] of the `BASED` item.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
pub(crate) fn apply_based_root_value_image<'ctx, 'b, 'e: 'b>(
    raw_ptr: Value<'ctx, 'b>,
    root: &RootStorageRecord,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    match &root.init {
        RootInitStrategy::StaticImage(img_bytes) => {
            let (img_ptr, _) = emit_const_slice(img_bytes, block, prog_ctx, state, loc)?;
            let mv_op = CobolMoveOp {
                src: img_ptr,
                dst_view: raw_ptr,
                encoding: MoveEncodingKind::RawBytesCopyPad,
                src_byte_len: root.byte_size,
                dst_byte_len: root.byte_size,
            }
            .build(state.ctx, loc)?;
            let _ = block.append_operation(mv_op);
            Ok(())
        }
        RootInitStrategy::DeferredBss {
            byte_size,
            plan_bytes,
        } => {
            let len_64 = emit_i64(i64::from(*byte_size), block, prog_ctx, state, loc);
            let (plan_ptr, plan_len) = emit_const_slice(plan_bytes, block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::InitBegin,
                &[],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let _ = emit_rt_call(
                RtDispatchTarget::InitExec,
                &[raw_ptr, len_64, plan_ptr, plan_len],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            Ok(())
        }
    }
}

/// Stores `0_i64` (`NULL`) at each 8-byte pointer leaf offset inside `raw_ptr` (ISO 2023 Rule 9).
///
/// # Arguments
///
/// * `raw_ptr` - Allocated `BASED` root pointer.
/// * `offsets` - Byte offsets of 8-byte pointer leaves.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn zero_pointer_leaves<'ctx, 'b, 'e: 'b>(
    raw_ptr: Value<'ctx, 'b>,
    offsets: &[u32],
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) {
    let raw = state.ctx.raw();
    let i8_ty = IntegerType::new(raw, 8).into();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let z64 = emit_i64(0, block, prog_ctx, state, loc);
    for &off in offsets {
        let leaf_ptr = if off == 0 {
            raw_ptr
        } else {
            let off_v = emit_i64(i64::from(off), block, prog_ctx, state, loc);
            append_val(
                block,
                llvm::get_element_ptr_dynamic(raw, raw_ptr, &[off_v], i8_ty, ptr_ty, loc),
                prog_ctx.fallback_val,
            )
        };
        let _ = block.append_operation(llvm::store(
            raw,
            z64,
            leaf_ptr,
            loc,
            llvm::LoadStoreOptions::new(),
        ));
    }
}

/// Lowers `HirStmt::Free` (`FREE based-item | ADDRESS OF based-item | ptr`), releasing heap memory
/// and resetting each target pointer slot to `NULL`.
///
/// # Arguments
///
/// * `targets` - Ordered [`HirFreeTarget`] slice.
/// * `span` - Statement span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if target slot lowering fails.
pub(crate) fn lower_free_stmt<'ctx, 'b, 'e: 'b>(
    targets: &[HirFreeTarget],
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let ptr_ty = llvm_ptr_type(state.ctx);
    let null_ptr = append_val(block, llvm::zero(ptr_ty, loc), prog_ctx.fallback_val);
    for tgt in targets {
        let slot_ptr = match tgt {
            HirFreeTarget::BasedItem(root_id) => {
                let root = find_storage_root(*root_id, prog_ctx, state, span)?;
                emit_storage_ptr(&root.sym_name, 0, 8, block, prog_ctx, state, loc)?
            }
            HirFreeTarget::PointerPlace(pl) => lower_place_view(pl, block, prog_ctx, state)?.ptr,
        };
        let cur_ptr = append_val(
            block,
            llvm::load(
                state.ctx.raw(),
                slot_ptr,
                ptr_ty,
                loc,
                llvm::LoadStoreOptions::new(),
            ),
            prog_ctx.fallback_val,
        );
        let _ = emit_rt_call(
            RtDispatchTarget::Free,
            &[cur_ptr],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        store_ptr_to_slot(null_ptr, slot_ptr, block, state, loc);
    }
    Ok(())
}

/// Lowers a character/alphanumeric expression (`Const`, `Place`, `FUNCTION TRIM`) into `(ptr, len_i64)`.
///
/// # Arguments
///
/// * `expr` - Character expression.
/// * `span` - Fallback diagnostic span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_raw_bytes_expr<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    match expr {
        HirExpr::Const { value, .. } => {
            let bytes = const_to_display_bytes(value, state.interner);
            emit_const_slice(&bytes, block, prog_ctx, state, loc)
        }
        HirExpr::Place(pl) => {
            let pv = lower_place_view(pl, block, prog_ctx, state)?;
            Ok((pv.ptr, pv.len_i64))
        }
        HirExpr::CallFunction {
            name,
            args,
            span: fn_sp,
        } => {
            let (_, p, l, _, _) =
                lower_intrinsic_as_move_source(*name, args, *fn_sp, block, prog_ctx, state, loc)?;
            Ok((p, l))
        }
        HirExpr::M12Call(c) => {
            if let Some((_, p, l, _, _)) =
                crate::lower_m20::try_lower_m12_call_move_source(c, block, prog_ctx, state, loc)?
            {
                return Ok((p, l));
            }
            let (_, p, l, _, _) =
                crate::lower_m12::lower_m12_call_move_source(c, block, prog_ctx, state, loc)?;
            Ok((p, l))
        }
        other => Err(unimpl(
            FeatureTag::FeatureAllocateBased,
            format!("character expression `{other:?}`"),
            state.source_map,
            expr_span(other).merge(span),
        )),
    }
}

/// Binds every [`RootBackingKind::ExternalPointer`] root slot in `roots` at `ProgramEntry` via
/// `@coblang_rt_resolve_external`.
///
/// # Arguments
///
/// * `roots` - Program [`RootStorageRecord`] slice.
/// * `block` - Program entry block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
pub(crate) fn emit_external_roots_startup<'ctx, 'b, 'e: 'b>(
    roots: &[RootStorageRecord],
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    for root in roots {
        let Some(ext_name) = &root.external_name else {
            continue;
        };
        let (name_ptr, name_len) =
            emit_const_slice(ext_name.as_bytes(), block, prog_ctx, state, loc)?;
        let (init_ptr, init_len) =
            emit_const_slice(root.init.static_image_slice(), block, prog_ctx, state, loc)?;
        let sz_val = emit_i64(
            i64::from(root.byte_size.max(1)),
            block,
            prog_ctx,
            state,
            loc,
        );
        let ext_ptr = emit_rt_call(
            RtDispatchTarget::ResolveExternal,
            &[name_ptr, name_len, sz_val, init_ptr, init_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        let slot_ptr = emit_storage_ptr(&root.sym_name, 0, 8, block, prog_ctx, state, loc)?;
        let _ = block.append_operation(llvm::store(
            state.ctx.raw(),
            ext_ptr,
            slot_ptr,
            loc,
            llvm::LoadStoreOptions::new(),
        ));
    }
    Ok(())
}

/// Emits the 1-byte `@__cobol_cancel_pending_<canon_sym>` global flag for `CANCEL` state reset.
///
/// # Arguments
///
/// * `canon_sym` - Canonical program symbol.
/// * `loc` - Source location.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if `cobol.storage` construction fails.
pub(crate) fn emit_cancel_pending_global(
    canon_sym: &str,
    loc: Location<'_>,
    state: &mut ModuleLowerState<'_, '_>,
) -> Result<()> {
    let sym = format!("__cobol_cancel_pending_{canon_sym}");
    let op = CobolStorageOp {
        sym_name: &sym,
        storage_kind: StorageKind::Working,
        byte_size: 1,
        alignment: 1,
        init_bytes: &[0],
    }
    .build(state.ctx, loc)?;
    let _ = state.module.body().append_operation(op);
    Ok(())
}

/// Emits Milestone M11 program entry startup: `LOCAL-STORAGE` / `IS INITIAL` / `CANCEL` state reset,
/// `NUMBER-OF-CALL-PARAMETERS` sync, and `PROCEDURE DIVISION USING / CHAINING` parameter binding.
///
/// # Arguments
///
/// * `block` - Program entry block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
pub(crate) fn emit_m11_program_entry_prologue<'ctx, 'b, 'e: 'b>(
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    emit_storage_reinit_and_cancel_check(block, prog_ctx, state, loc)?;
    emit_narg_special_register_sync(block, prog_ctx, state, loc)?;
    emit_formal_params_prologue(block, prog_ctx, state, loc)
}

/// Reinitializes `LOCAL-STORAGE` (and `IS INITIAL` `WORKING-STORAGE`) on every entry and checks
/// `@__cobol_cancel_pending_<canon_sym>` to restore `WORKING-STORAGE` after `CANCEL`.
///
/// # Arguments
///
/// * `block` - Entry block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_storage_reinit_and_cancel_check<'ctx, 'b, 'e: 'b>(
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let is_initial = prog_ctx.prog.initial_state == HirInitialState::ResetInitial;
    let mut cancel_ws_roots = Vec::new();
    for root in &prog_ctx.storage.roots {
        if root.backing != RootBackingKind::DirectBuffer {
            continue;
        }
        let sec = prog_ctx
            .prog
            .data_items
            .get(root.root_id.as_u32() as usize)
            .map(|it| it.section);
        if sec == Some(HirStorageSection::LocalStorage) || is_initial {
            let r_ptr = emit_storage_ptr(
                &root.sym_name,
                0,
                root.byte_size.max(1),
                block,
                prog_ctx,
                state,
                loc,
            )?;
            apply_based_root_value_image(r_ptr, root, block, prog_ctx, state, loc)?;
        } else if sec == Some(HirStorageSection::WorkingStorage) {
            cancel_ws_roots.push(root);
        }
    }
    if cancel_ws_roots.is_empty() {
        return Ok(());
    }
    let raw = state.ctx.raw();
    let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();
    let flag_sym = format!("__cobol_cancel_pending_{}", prog_ctx.canon_sym);
    let flag_ptr = emit_storage_ptr(&flag_sym, 0, 1, block, prog_ctx, state, loc)?;
    let flag_val = append_val(
        block,
        llvm::load(raw, flag_ptr, i8_ty, loc, llvm::LoadStoreOptions::new()),
        prog_ctx.fallback_val,
    );
    let z_i8 = append_val(
        block,
        arith::constant(raw, IntegerAttribute::new(i8_ty, 0).into(), loc),
        prog_ctx.fallback_val,
    );
    let is_cancelled = append_val(
        block,
        arith::cmpi(raw, CmpiPredicate::Ne, flag_val, z_i8, loc),
        prog_ctx.fallback_val,
    );
    let then_reg = Region::new();
    let then_blk = then_reg.append_block(Block::new(&[]));
    let _ = then_blk.append_operation(llvm::store(
        raw,
        z_i8,
        flag_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    for root in cancel_ws_roots {
        let r_ptr = emit_storage_ptr(
            &root.sym_name,
            0,
            root.byte_size.max(1),
            &then_blk,
            prog_ctx,
            state,
            loc,
        )?;
        apply_based_root_value_image(r_ptr, root, &then_blk, prog_ctx, state, loc)?;
    }
    let _ = then_blk.append_operation(scf::r#yield(&[], loc));
    let else_reg = Region::new();
    let else_blk = else_reg.append_block(Block::new(&[]));
    let _ = else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(is_cancelled, &[], then_reg, else_reg, loc));
    Ok(())
}

/// Populates the `NUMBER-OF-CALL-PARAMETERS` special register at program entry when declared.
///
/// # Arguments
///
/// * `block` - Entry block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_narg_special_register_sync<'ctx, 'b, 'e: 'b>(
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let narg_loc = prog_ctx
        .prog
        .data_items
        .iter()
        .find_map(|it| {
            if it.section == HirStorageSection::SpecialRegister
                && it
                    .name
                    .and_then(|s| state.interner.resolve_canonical(s).ok())
                    == Some("NUMBER-OF-CALL-PARAMETERS")
            {
                Some(it.id)
            } else {
                None
            }
        })
        .and_then(|id| prog_ctx.storage.items.get(id.as_u32() as usize)?.as_ref());
    let Some(item_loc) = narg_loc else {
        return Ok(());
    };
    let reg_ptr = emit_storage_ptr(
        &item_loc.root_storage_sym,
        item_loc.base_byte_offset,
        4,
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let narg_i32 = emit_rt_call(
        RtDispatchTarget::CurrentNarg,
        &[],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        narg_i32,
        reg_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    Ok(())
}

/// Binds all `PROCEDURE DIVISION USING` and `CHAINING` formal parameters at program entry.
///
/// # Arguments
///
/// * `block` - Entry block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
#[expect(
    clippy::too_many_lines,
    reason = "Complete USING, RETURNING, and CHAINING formal parameter binding prologue."
)]
fn emit_formal_params_prologue<'ctx, 'b, 'e: 'b>(
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();
    let null_ptr = append_val(block, llvm::zero(ptr_ty, loc), prog_ctx.fallback_val);
    let mut using_idx = usize::from(prog_ctx.is_udf_unit);
    let mut chain_idx = 1_usize;

    if prog_ctx.is_udf_unit
        && let Some(ret_id) = prog_ctx.prog.returning_item
        && let Some(Some(ret_loc)) = prog_ctx.storage.items.get(ret_id.as_u32() as usize)
    {
        let zero_idx = emit_i64(0, block, prog_ctx, state, loc);
        let ret_ptr = emit_rt_call(
            RtDispatchTarget::GetCallParam,
            &[zero_idx, null_ptr],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        let slot_ptr =
            emit_storage_ptr(&ret_loc.root_storage_sym, 0, 8, block, prog_ctx, state, loc)?;
        let _ = block.append_operation(llvm::store(
            raw,
            ret_ptr,
            slot_ptr,
            loc,
            llvm::LoadStoreOptions::new(),
        ));
        if let Some(root_rec) = prog_ctx.storage.roots.get(ret_loc.root_idx) {
            let init_img = root_rec.init.static_image_slice();
            if init_img.is_empty() {
                let fill_byte: u8 = if matches!(
                    ret_loc.field_spec.desc.ty.category(),
                    DataCategory::Alphanumeric
                        | DataCategory::Alphabetic
                        | DataCategory::AlphanumericEdited
                        | DataCategory::NumericEdited
                ) {
                    b' '
                } else {
                    0
                };
                let fill_len = usize::try_from(root_rec.byte_size.max(1)).unwrap_or(1);
                let fill_vec = vec![fill_byte; fill_len];
                let (fp, fl) = emit_const_slice(&fill_vec, block, prog_ctx, state, loc)?;
                emit_raw_memcpy(ret_ptr, fp, fl, block, prog_ctx, state, loc)?;
            } else {
                let (ip, il) = emit_const_slice(init_img, block, prog_ctx, state, loc)?;
                emit_raw_memcpy(ret_ptr, ip, il, block, prog_ctx, state, loc)?;
            }
        }
    }

    for up in &prog_ctx.prog.using_params {
        let Some(Some(item_loc)) = prog_ctx.storage.items.get(up.item.as_u32() as usize) else {
            continue;
        };
        match up.source {
            HirParamSource::Using => {
                let idx_v = emit_i64(
                    i64::try_from(using_idx).unwrap_or(0),
                    block,
                    prog_ctx,
                    state,
                    loc,
                );
                using_idx += 1;
                let arg_ptr = emit_rt_call(
                    RtDispatchTarget::GetCallParam,
                    &[idx_v, null_ptr],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
                let slot_ptr = emit_storage_ptr(
                    &item_loc.root_storage_sym,
                    0,
                    8,
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
                let bound_ptr = if up.mode == HirPassingMode::ByValue {
                    materialize_by_value_formal(arg_ptr, item_loc, block, prog_ctx, state, loc)
                } else {
                    arg_ptr
                };
                let _ = block.append_operation(llvm::store(
                    raw,
                    bound_ptr,
                    slot_ptr,
                    loc,
                    llvm::LoadStoreOptions::new(),
                ));
            }
            HirParamSource::Chaining => {
                let cap = i64::from(item_loc.elem_byte_len.max(256));
                let cap_v = emit_i64(cap, block, prog_ctx, state, loc);
                let idx_v = emit_i64(
                    i64::try_from(chain_idx).unwrap_or(1),
                    block,
                    prog_ctx,
                    state,
                    loc,
                );
                chain_idx += 1;
                let buf_ptr = append_val(
                    prog_ctx.entry_block,
                    llvm::alloca(
                        raw,
                        cap_v,
                        ptr_ty,
                        loc,
                        llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i8_ty))),
                    ),
                    prog_ctx.fallback_val,
                );
                let got_len = emit_rt_call(
                    RtDispatchTarget::CopyCliArg,
                    &[idx_v, buf_ptr, cap_v],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
                let ch_place = HirPlace {
                    item: up.item,
                    static_offset: ByteOffset::new(item_loc.base_byte_offset),
                    subscripts: Vec::new(),
                    refmod: None,
                    effective_ty: item_loc.field_spec.desc.ty.clone(),
                    check_mask: ExceptionCheckMask::NONE,
                    span: up.span,
                };
                emit_chaining_move_if_present(
                    &ch_place, buf_ptr, got_len, block, prog_ctx, state, loc,
                )?;
            }
        }
    }
    Ok(())
}

/// Allocates a local stack buffer for a `PROCEDURE DIVISION USING BY VALUE` formal parameter and
/// stores the incoming register word into it (converting endianness for big-endian `COMP` items).
///
/// # Arguments
///
/// * `arg_ptr` - Incoming register value carried as `!llvm.ptr`.
/// * `item_loc` - Resolved `LINKAGE` item layout.
/// * `block` - Entry block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn materialize_by_value_formal<'ctx, 'b, 'e: 'b>(
    arg_ptr: Value<'ctx, 'b>,
    item_loc: &ResolvedItemStorage,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();
    let alloc_sz = emit_i64(
        i64::from(item_loc.elem_byte_len.max(8)),
        block,
        prog_ctx,
        state,
        loc,
    );
    let local_buf = append_val(
        prog_ctx.entry_block,
        llvm::alloca(
            raw,
            alloc_sz,
            ptr_ty,
            loc,
            llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i8_ty))),
        ),
        prog_ctx.fallback_val,
    );
    let word64 = emit_ptr_to_i64(arg_ptr, block, prog_ctx, state, loc);
    if let HirType::BinaryInt {
        byte_width,
        endian: BinaryEndianness::BigEndian,
        ..
    } = item_loc.field_spec.desc.ty
    {
        for idx in 0..byte_width {
            let shift_bits = i64::from((byte_width - 1 - idx) * 8);
            let sh_v = emit_i64(shift_bits, block, prog_ctx, state, loc);
            let shifted = append_val(
                block,
                arith::shrui(word64, sh_v, loc),
                prog_ctx.fallback_val,
            );
            let b_val = append_val(
                block,
                arith::trunci(shifted, i8_ty, loc),
                prog_ctx.fallback_val,
            );
            let off_v = emit_i64(i64::from(idx), block, prog_ctx, state, loc);
            let dst_b = append_val(
                block,
                llvm::get_element_ptr_dynamic(raw, local_buf, &[off_v], i8_ty, ptr_ty, loc),
                prog_ctx.fallback_val,
            );
            let _ = block.append_operation(llvm::store(
                raw,
                b_val,
                dst_b,
                loc,
                llvm::LoadStoreOptions::new(),
            ));
        }
    } else {
        let _ = block.append_operation(llvm::store(
            raw,
            word64,
            local_buf,
            loc,
            llvm::LoadStoreOptions::new(),
        ));
    }
    local_buf
}

/// Conditionally executes an alphanumeric `MOVE` of `buf_ptr[..got_len]` into `place` when `got_len >= 0`.
///
/// # Arguments
///
/// * `place` - Target `CHAINING` [`HirPlace`].
/// * `buf_ptr` - Copied CLI argument buffer.
/// * `got_len` - Copied byte count (`-1` when omitted).
/// * `block` - Entry block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_chaining_move_if_present<'ctx, 'b, 'e: 'b>(
    place: &HirPlace,
    buf_ptr: Value<'ctx, 'b>,
    got_len: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let z64 = emit_i64(0, block, prog_ctx, state, loc);
    let has_arg = append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Sge, got_len, z64, loc),
        prog_ctx.fallback_val,
    );
    let then_reg = Region::new();
    let then_blk = then_reg.append_block(Block::new(&[]));
    let dst_view = lower_place_view(place, &then_blk, prog_ctx, state)?;
    let (empty_ptr, empty_len) = emit_const_slice(&[], &then_blk, prog_ctx, state, loc)?;
    let kind_v = emit_i32(
        RuntimeMoveSourceKind::AlphanumericLiteral.to_abi_code(),
        &then_blk,
        prog_ctx,
        state,
        loc,
    );
    let (dst_spec_ptr, dst_spec_len) =
        emit_spec_slice(&dst_view.spec, &then_blk, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::MoveDesc,
        &[
            kind_v,
            buf_ptr,
            got_len,
            empty_ptr,
            empty_len,
            dst_view.ptr,
            dst_view.len_i64,
            dst_spec_ptr,
            dst_spec_len,
        ],
        &then_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = then_blk.append_operation(scf::r#yield(&[], loc));
    let else_reg = Region::new();
    let else_blk = else_reg.append_block(Block::new(&[]));
    let _ = else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(has_arg, &[], then_reg, else_reg, loc));
    Ok(())
}

/// Builds an unsubscripted [`HirPlace`] for `item` from `prog.data_items`.
///
/// # Arguments
///
/// * `item` - Target [`DataItemId`].
/// * `span` - Source span.
/// * `prog` - Enclosing [`HirProgram`].
fn make_direct_item_place(
    item: DataItemId,
    span: SourceSpan,
    prog: &HirProgram,
) -> Option<HirPlace> {
    let di = prog.data_items.get(item.as_u32() as usize)?;
    Some(HirPlace {
        item,
        static_offset: di.byte_offset,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: di.ty.clone(),
        check_mask: ExceptionCheckMask::NONE,
        span,
    })
}

/// Copies `prog.returning_item` (when declared on `PROCEDURE DIVISION RETURNING`) into `RETURN-CODE`
/// prior to `GOBACK`, `EXIT PROGRAM`, or program fallthrough.
///
/// # Arguments
///
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if expression evaluation fails.
pub(crate) fn emit_returning_item_sync<'ctx, 'b, 'e: 'b>(
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let Some(ret_id) = prog_ctx.prog.returning_item else {
        return Ok(());
    };
    let Some(ret_place) = make_direct_item_place(ret_id, prog_ctx.prog.span, prog_ctx.prog) else {
        return Ok(());
    };
    let ret_expr = HirExpr::Place(ret_place);
    let v64 = eval_expr_as_i64(&ret_expr, block, prog_ctx, state)?;
    let i32_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 32).into();
    let cval = append_val(
        block,
        arith::trunci(v64, i32_ty, loc),
        prog_ctx.fallback_val,
    );
    let set_op = CobolSetReturnCodeOp { code: cval }.build(state.ctx, loc)?;
    let _ = block.append_operation(set_op);
    Ok(())
}

/// Lowers `IF param IS OMITTED` by testing whether the `LINKAGE` root's indirect base-pointer slot is `NULL`.
///
/// # Arguments
///
/// * `item` - Tested `LINKAGE` `DataItemId`.
/// * `span` - Condition span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if `item` has no indirect base-pointer slot.
pub(crate) fn lower_m11_omitted_condition<'ctx, 'b, 'e: 'b>(
    item: DataItemId,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let loc = state.loc(span)?;
    let Some(Some(item_loc)) = prog_ctx.storage.items.get(item.as_u32() as usize) else {
        return Err(unimpl(
            FeatureTag::FeatureCallSubprog,
            "unresolved IS OMITTED item",
            state.source_map,
            span,
        ));
    };
    let slot_ptr = emit_storage_ptr(
        &item_loc.root_storage_sym,
        0,
        8,
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let ptr_ty = llvm_ptr_type(state.ctx);
    let base_ptr = append_val(
        block,
        llvm::load(
            state.ctx.raw(),
            slot_ptr,
            ptr_ty,
            loc,
            llvm::LoadStoreOptions::new(),
        ),
        prog_ctx.fallback_val,
    );
    let addr = emit_ptr_to_i64(base_ptr, block, prog_ctx, state, loc);
    let z64 = emit_i64(0, block, prog_ctx, state, loc);
    Ok(append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, addr, z64, loc),
        prog_ctx.fallback_val,
    ))
}

/// Lowers `HirStmt::Cancel` (`CANCEL target ...`) into `@coblang_rt_cancel_by_slice`.
///
/// # Arguments
///
/// * `targets` - Cancelled module expressions.
/// * `span` - Statement span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if target operand lowering fails.
pub(crate) fn lower_m11_cancel_stmt<'ctx, 'b, 'e: 'b>(
    targets: &[HirExpr],
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    for tgt in targets {
        let (ptr, len) = match tgt {
            HirExpr::Const { value, .. } => {
                let bytes = const_to_display_bytes(value, state.interner);
                emit_const_slice(&bytes, block, prog_ctx, state, loc)?
            }
            HirExpr::Place(pl) => {
                let v = lower_place_view(pl, block, prog_ctx, state)?;
                (v.ptr, v.len_i64)
            }
            other => {
                return Err(unimpl(
                    FeatureTag::FeatureCallSubprog,
                    "unsupported CANCEL operand",
                    state.source_map,
                    expr_span(other),
                ));
            }
        };
        let _ = emit_rt_call(
            RtDispatchTarget::CancelBySlice,
            &[ptr, len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Lowers `HirStmt::Call` (`HirCallSpec`) supporting static/dynamic/pointer callees, `BY REFERENCE`,
/// `BY CONTENT`, `BY VALUE`, `OMITTED`, `RETURNING`, and `ON EXCEPTION` / `NOT ON EXCEPTION`.
///
/// # Arguments
///
/// * `spec` - Resolved [`HirCallSpec`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if argument or handler lowering fails.
pub(crate) fn lower_m11_call_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirCallSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let caller_orig = state
        .interner
        .resolve_original(prog_ctx.prog.name)
        .unwrap_or("MAIN");
    let mut caller_nul = Vec::with_capacity(caller_orig.len() + 1);
    caller_nul.extend_from_slice(caller_orig.as_bytes());
    caller_nul.push(0);
    let (caller_cstr, _) = emit_const_slice(&caller_nul, block, prog_ctx, state, loc)?;

    let (argc_v, meta_ptr) = pack_call_arguments(&spec.args, block, prog_ctx, state, loc)?;
    let preserve_rc = i32::from(matches!(spec.returning, HirCallReturning::Omitted));
    let has_on_exc = i32::from(!spec.on_exception.is_empty());

    // Static in-module COBOL subprogram fast path (preserves nested `__nested__` resolution).
    if let Some((canon_sym, target_orig)) = resolve_in_module_callee(&spec.callee, prog_ctx, state)
    {
        return lower_static_in_module_call(
            spec,
            (&canon_sym, &target_orig),
            (caller_cstr, argc_v, meta_ptr, preserve_rc),
            block,
            prog_ctx,
            state,
            loc,
        );
    }

    let (target_ptr, mode_i64) =
        lower_dynamic_call_target(&spec.callee, block, prog_ctx, state, loc)?;
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i32_ty: Type<'ctx> = IntegerType::new(raw, 32).into();
    let one_64 = emit_i64(1, block, prog_ctx, state, loc);
    let rc_slot = append_val(
        prog_ctx.entry_block,
        llvm::alloca(
            raw,
            one_64,
            ptr_ty,
            loc,
            llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i32_ty))),
        ),
        prog_ctx.fallback_val,
    );
    let z32 = emit_i32(0, block, prog_ctx, state, loc);
    let _ = block.append_operation(llvm::store(
        raw,
        z32,
        rc_slot,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let flags_v = emit_i32(has_on_exc | (preserve_rc << 1), block, prog_ctx, state, loc);
    let status = emit_rt_call(
        RtDispatchTarget::CallInvoke,
        &[
            caller_cstr,
            target_ptr,
            mode_i64,
            flags_v,
            argc_v,
            meta_ptr,
            rc_slot,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let callee_rc = append_val(
        block,
        llvm::load(raw, rc_slot, i32_ty, loc, llvm::LoadStoreOptions::new()),
        prog_ctx.fallback_val,
    );
    emit_post_call_branches(spec, status, callee_rc, block, prog_ctx, state, loc)
}

/// Lowers an in-module static COBOL subprogram `CALL` wrapped with `PushCallFrame` / `PopCallFrame`.
///
/// # Arguments
///
/// * `spec` - [`HirCallSpec`].
/// * `target_pair` - `(canonical_symbol, original_program_name)` of callee.
/// * `frame_ops` - `(caller_cstr, argc_v, meta_ptr, preserve_rc)` operands.
/// * `block` - Target block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_static_in_module_call<'ctx, 'b, 'e: 'b>(
    spec: &HirCallSpec,
    target_pair: (&str, &str),
    frame_ops: (Value<'ctx, 'b>, Value<'ctx, 'b>, Value<'ctx, 'b>, i32),
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let (canon_sym, target_orig) = target_pair;
    let (caller_cstr, argc_v, meta_ptr, preserve_rc) = frame_ops;
    let saved_mod_rc = if matches!(spec.returning, HirCallReturning::Omitted) {
        let get_op = CobolGetReturnCodeOp.build(state.ctx, loc)?;
        Some(append_val(block, get_op, prog_ctx.fallback_val))
    } else {
        None
    };
    let mut target_nul = Vec::with_capacity(target_orig.len() + 1);
    target_nul.extend_from_slice(target_orig.as_bytes());
    target_nul.push(0);
    let (target_cstr, _) = emit_const_slice(&target_nul, block, prog_ctx, state, loc)?;
    let saved_rc = emit_rt_call(
        RtDispatchTarget::PushCallFrame,
        &[caller_cstr, target_cstr, argc_v, meta_ptr],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let call_op = CobolCallOp {
        dispatch: CallDispatchKind::StaticSymbol,
        callee: canon_sym,
        args: &[],
        exception_branches: ExceptionBranches::None,
    }
    .build(state.ctx, loc)?;
    let _ = block.append_operation(call_op);
    let pres_v = emit_i32(preserve_rc, block, prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::PopCallFrame,
        &[saved_rc, pres_v],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let get_after = CobolGetReturnCodeOp.build(state.ctx, loc)?;
    let callee_rc = append_val(block, get_after, prog_ctx.fallback_val);
    if let Some(prev_rc) = saved_mod_rc {
        let restore_op = CobolSetReturnCodeOp { code: prev_rc }.build(state.ctx, loc)?;
        let _ = block.append_operation(restore_op);
    } else {
        store_call_returning(&spec.returning, callee_rc, block, prog_ctx, state, loc)?;
    }
    for st in &spec.not_on_exception {
        lower_stmt(st, block, prog_ctx, state)?;
    }
    Ok(())
}

/// Checks whether `callee` is a string literal naming an in-module COBOL subprogram.
///
/// # Arguments
///
/// * `callee` - Callee [`HirExpr`].
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn resolve_in_module_callee(
    callee: &HirExpr,
    prog_ctx: &ProgramLowerCtx<'_, '_, '_>,
    state: &ModuleLowerState<'_, '_>,
) -> Option<(String, String)> {
    let HirExpr::Const {
        value: HirConstValue::Bytes(raw_b),
        ..
    } = callee
    else {
        return None;
    };
    let orig = String::from_utf8_lossy(raw_b).trim().to_owned();
    let upper = orig.to_ascii_uppercase();
    prog_ctx
        .local_children
        .get(&upper)
        .or_else(|| state.in_module_programs.get(&upper))
        .map(|canon| (canon.clone(), orig))
}

/// Lowers a dynamic literal, alphanumeric identifier, or `PROGRAM-POINTER` callee into `(target_ptr, mode_i64)`.
///
/// # Arguments
///
/// * `callee` - Callee [`HirExpr`].
/// * `block` - Target block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_dynamic_call_target<'ctx, 'b, 'e: 'b>(
    callee: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    match callee {
        HirExpr::Const { value, .. } => {
            let bytes = const_to_display_bytes(value, state.interner);
            emit_const_slice(&bytes, block, prog_ctx, state, loc)
        }
        HirExpr::Place(pl) => {
            let v = lower_place_view(pl, block, prog_ctx, state)?;
            if matches!(v.spec.desc.ty, HirType::ProgramPointer | HirType::Pointer) {
                let raw = state.ctx.raw();
                let ptr_ty = llvm_ptr_type(state.ctx);
                let code_ptr = append_val(
                    block,
                    llvm::load(raw, v.ptr, ptr_ty, loc, llvm::LoadStoreOptions::new()),
                    prog_ctx.fallback_val,
                );
                let neg1 = emit_i64(-1, block, prog_ctx, state, loc);
                Ok((code_ptr, neg1))
            } else {
                Ok((v.ptr, v.len_i64))
            }
        }
        other => Err(unimpl(
            FeatureTag::FeatureCallSubprog,
            "unsupported CALL target expression",
            state.source_map,
            expr_span(other),
        )),
    }
}

/// Packs `args` into a stack `[2 * argc x i64]` array `[ptr_0, meta_0, ...]`, returning `(argc_i64, array_ptr)`.
///
/// # Arguments
///
/// * `args` - Ordered [`HirCallArg`] slice.
/// * `block` - Target block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn pack_call_arguments<'ctx, 'b, 'e: 'b>(
    args: &[HirCallArg],
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();
    if args.is_empty() {
        let z = emit_i64(0, block, prog_ctx, state, loc);
        let null_p = append_val(block, llvm::zero(ptr_ty, loc), prog_ctx.fallback_val);
        return Ok((z, null_p));
    }
    let argc_i64 = i64::try_from(args.len()).unwrap_or(0);
    let slots_v = emit_i64(argc_i64.saturating_mul(2), block, prog_ctx, state, loc);
    let arr_ptr = append_val(
        prog_ctx.entry_block,
        llvm::alloca(
            raw,
            slots_v,
            ptr_ty,
            loc,
            llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i64_ty))),
        ),
        prog_ctx.fallback_val,
    );
    for (idx, arg) in args.iter().enumerate() {
        let (pw, mw) = lower_single_call_arg(arg, block, prog_ctx, state, loc)?;
        let base_slot = i64::try_from(idx.saturating_mul(2)).unwrap_or(0);
        let idx0 = emit_i64(base_slot, block, prog_ctx, state, loc);
        let idx1 = emit_i64(base_slot + 1, block, prog_ctx, state, loc);
        let p0 = append_val(
            block,
            llvm::get_element_ptr_dynamic(raw, arr_ptr, &[idx0], i64_ty, ptr_ty, loc),
            prog_ctx.fallback_val,
        );
        let p1 = append_val(
            block,
            llvm::get_element_ptr_dynamic(raw, arr_ptr, &[idx1], i64_ty, ptr_ty, loc),
            prog_ctx.fallback_val,
        );
        let _ =
            block.append_operation(llvm::store(raw, pw, p0, loc, llvm::LoadStoreOptions::new()));
        let _ =
            block.append_operation(llvm::store(raw, mw, p1, loc, llvm::LoadStoreOptions::new()));
    }
    let argc_v = emit_i64(argc_i64, block, prog_ctx, state, loc);
    Ok((argc_v, arr_ptr))
}

/// Lowers a single [`HirCallArg`] into `(ptr_word_i64, meta_word_i64)`.
///
/// # Arguments
///
/// * `arg` - Single [`HirCallArg`].
/// * `block` - Target block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
pub(crate) fn lower_single_call_arg<'ctx, 'b, 'e: 'b>(
    arg: &HirCallArg,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    match arg {
        HirCallArg::Omitted => {
            let z = emit_i64(0, block, prog_ctx, state, loc);
            Ok((z, z))
        }
        HirCallArg::ByReference(pl) => {
            let v = lower_place_view_allow_null(pl, block, prog_ctx, state)?;
            let pw = emit_ptr_to_i64(v.ptr, block, prog_ctx, state, loc);
            let is_be = matches!(
                v.spec.desc.ty,
                HirType::BinaryInt {
                    endian: BinaryEndianness::BigEndian,
                    ..
                }
            );
            let mw = if is_be {
                let flag_hi = emit_i64(1_i64 << 56, block, prog_ctx, state, loc);
                append_val(
                    block,
                    arith::ori(v.len_i64, flag_hi, loc),
                    prog_ctx.fallback_val,
                )
            } else {
                v.len_i64
            };
            Ok((pw, mw))
        }
        HirCallArg::ByContent(expr) => lower_by_content_call_arg(expr, block, prog_ctx, state, loc),
        HirCallArg::ByValue(expr) => lower_by_value_call_arg(expr, block, prog_ctx, state, loc),
    }
}

/// Lowers a `BY CONTENT` call argument expression into `(temp_buf_ptr_word, byte_len)`.
///
/// # Arguments
///
/// * `expr` - `BY CONTENT` [`HirExpr`].
/// * `block` - Target block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
#[expect(
    clippy::too_many_lines,
    reason = "Complete BY CONTENT marshalling across places, M12 calls, literals, and arithmetic."
)]
fn lower_by_content_call_arg<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();
    let i32_ty: Type<'ctx> = IntegerType::new(raw, 32).into();
    match expr {
        HirExpr::Place(pl) => {
            let v = lower_place_view(pl, block, prog_ctx, state)?;
            let one64 = emit_i64(1, block, prog_ctx, state, loc);
            let alloc_len = append_val(
                block,
                arith::addi(v.len_i64, one64, loc),
                prog_ctx.fallback_val,
            );
            let tmp = append_val(
                block,
                llvm::alloca(
                    raw,
                    alloc_len,
                    ptr_ty,
                    loc,
                    llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i8_ty))),
                ),
                prog_ctx.fallback_val,
            );
            emit_memcpy_with_trailing_nul(tmp, v.ptr, v.len_i64, block, prog_ctx, state, loc)?;
            Ok((emit_ptr_to_i64(tmp, block, prog_ctx, state, loc), v.len_i64))
        }
        HirExpr::Const {
            value: HirConstValue::Bytes(raw_b),
            ..
        } => {
            let mut nul_b = Vec::with_capacity(raw_b.len() + 1);
            nul_b.extend_from_slice(raw_b);
            nul_b.push(0);
            let (c_ptr, c_len) = emit_const_slice(&nul_b, block, prog_ctx, state, loc)?;
            let tmp = append_val(
                prog_ctx.entry_block,
                llvm::alloca(
                    raw,
                    c_len,
                    ptr_ty,
                    loc,
                    llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i8_ty))),
                ),
                prog_ctx.fallback_val,
            );
            emit_raw_memcpy(tmp, c_ptr, c_len, block, prog_ctx, state, loc)?;
            let logical_len = emit_i64(
                i64::try_from(raw_b.len()).unwrap_or(0),
                block,
                prog_ctx,
                state,
                loc,
            );
            Ok((
                emit_ptr_to_i64(tmp, block, prog_ctx, state, loc),
                logical_len,
            ))
        }
        HirExpr::Const {
            value: HirConstValue::Figurative(HirFigurative::Null),
            ..
        } => {
            let z = emit_i64(0, block, prog_ctx, state, loc);
            Ok((z, z))
        }
        HirExpr::M12Call(c)
            if !matches!(
                c.return_ty.category(),
                DataCategory::Numeric | DataCategory::Index
            ) =>
        {
            let (_, ptr, len_i64, _, _) =
                crate::lower_m12::lower_m12_call_move_source(c, block, prog_ctx, state, loc)?;
            let one64 = emit_i64(1, block, prog_ctx, state, loc);
            let alloc_len = append_val(
                block,
                arith::addi(len_i64, one64, loc),
                prog_ctx.fallback_val,
            );
            let tmp = append_val(
                block,
                llvm::alloca(
                    raw,
                    alloc_len,
                    ptr_ty,
                    loc,
                    llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i8_ty))),
                ),
                prog_ctx.fallback_val,
            );
            emit_memcpy_with_trailing_nul(tmp, ptr, len_i64, block, prog_ctx, state, loc)?;
            Ok((emit_ptr_to_i64(tmp, block, prog_ctx, state, loc), len_i64))
        }
        other => {
            let val64 = eval_expr_as_i64(other, block, prog_ctx, state)?;
            let val32 = append_val(
                block,
                arith::trunci(val64, i32_ty, loc),
                prog_ctx.fallback_val,
            );
            let four64 = emit_i64(4, block, prog_ctx, state, loc);
            let tmp = append_val(
                prog_ctx.entry_block,
                llvm::alloca(
                    raw,
                    four64,
                    ptr_ty,
                    loc,
                    llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i8_ty))),
                ),
                prog_ctx.fallback_val,
            );
            let _ = block.append_operation(llvm::store(
                raw,
                val32,
                tmp,
                loc,
                llvm::LoadStoreOptions::new(),
            ));
            Ok((emit_ptr_to_i64(tmp, block, prog_ctx, state, loc), four64))
        }
    }
}

/// Lowers a `BY VALUE` call argument expression into `(word_i64, byte_width_i64)`.
///
/// # Arguments
///
/// * `expr` - `BY VALUE` [`HirExpr`].
/// * `block` - Target block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_by_value_call_arg<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();
    match expr {
        HirExpr::AddressOf(place) => {
            let v = lower_place_view_allow_null(place, block, prog_ctx, state)?;
            let eight = emit_i64(8, block, prog_ctx, state, loc);
            Ok((emit_ptr_to_i64(v.ptr, block, prog_ctx, state, loc), eight))
        }
        HirExpr::Place(pl) => {
            let v = lower_place_view(pl, block, prog_ctx, state)?;
            if matches!(v.spec.desc.ty, HirType::Pointer | HirType::ProgramPointer) {
                let p_val = append_val(
                    block,
                    llvm::load(raw, v.ptr, ptr_ty, loc, llvm::LoadStoreOptions::new()),
                    prog_ctx.fallback_val,
                );
                let eight = emit_i64(8, block, prog_ctx, state, loc);
                return Ok((emit_ptr_to_i64(p_val, block, prog_ctx, state, loc), eight));
            }
            if matches!(
                v.spec.desc.ty.category(),
                DataCategory::Numeric | DataCategory::NumericEdited | DataCategory::Index
            ) {
                let n64 = eval_expr_as_i64(expr, block, prog_ctx, state)?;
                return Ok((n64, v.len_i64));
            }
            // Alphanumeric `BY VALUE` (up to 8 bytes packed little-endian into register word).
            let eight = emit_i64(8, block, prog_ctx, state, loc);
            let slot = append_val(
                prog_ctx.entry_block,
                llvm::alloca(
                    raw,
                    eight,
                    ptr_ty,
                    loc,
                    llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i64_ty))),
                ),
                prog_ctx.fallback_val,
            );
            let z64 = emit_i64(0, block, prog_ctx, state, loc);
            let _ = block.append_operation(llvm::store(
                raw,
                z64,
                slot,
                loc,
                llvm::LoadStoreOptions::new(),
            ));
            let is_lt8 = append_val(
                block,
                arith::cmpi(raw, CmpiPredicate::Ult, v.len_i64, eight, loc),
                prog_ctx.fallback_val,
            );
            let copy_n = append_val(
                block,
                arith::select(is_lt8, v.len_i64, eight, loc),
                prog_ctx.fallback_val,
            );
            let _ = i8_ty;
            emit_raw_memcpy(slot, v.ptr, copy_n, block, prog_ctx, state, loc)?;
            let packed = append_val(
                block,
                llvm::load(raw, slot, i64_ty, loc, llvm::LoadStoreOptions::new()),
                prog_ctx.fallback_val,
            );
            Ok((packed, v.len_i64))
        }
        other => {
            let n64 = eval_expr_as_i64(other, block, prog_ctx, state)?;
            let four = emit_i64(4, block, prog_ctx, state, loc);
            Ok((n64, four))
        }
    }
}

/// Emits `CobolMoveOp` (`RawBytesCopyPad`) copying `len` raw bytes from `src` to `dst`.
///
/// # Arguments
///
/// * `dst` - Destination `!llvm.ptr`.
/// * `src` - Source `!llvm.ptr`.
/// * `len` - Byte length `i64`.
/// * `block` - Target block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_raw_memcpy<'ctx, 'b, 'e: 'b>(
    dst: Value<'ctx, 'b>,
    src: Value<'ctx, 'b>,
    len: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let _ = emit_rt_call(
        RtDispatchTarget::RawCopy,
        &[dst, src, len],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Copies `len` bytes from `src` to `dst` and writes a trailing `0_u8` NUL byte at `dst[len]`.
///
/// # Arguments
///
/// * `dst` - Destination `!llvm.ptr` of capacity `>= len + 1`.
/// * `src` - Source `!llvm.ptr`.
/// * `len` - Byte count `i64`.
/// * `block` - Target block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_memcpy_with_trailing_nul<'ctx, 'b, 'e: 'b>(
    dst: Value<'ctx, 'b>,
    src: Value<'ctx, 'b>,
    len: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();
    emit_raw_memcpy(dst, src, len, block, prog_ctx, state, loc)?;
    let end_p = append_val(
        block,
        llvm::get_element_ptr_dynamic(raw, dst, &[len], i8_ty, ptr_ty, loc),
        prog_ctx.fallback_val,
    );
    let z8 = append_val(
        block,
        arith::constant(raw, IntegerAttribute::new(i8_ty, 0).into(), loc),
        prog_ctx.fallback_val,
    );
    let _ = block.append_operation(llvm::store(
        raw,
        z8,
        end_p,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    Ok(())
}

/// Stores `callee_rc` (`i32`) into `dst` when `returning` is [`HirCallReturning::Place`].
///
/// # Arguments
///
/// * `returning` - `CALL RETURNING` disposition.
/// * `callee_rc` - Returned `i32` SSA value.
/// * `block` - Target block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn store_call_returning<'ctx, 'b, 'e: 'b>(
    returning: &HirCallReturning,
    callee_rc: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    if matches!(returning, HirCallReturning::Omitted) {
        return Ok(());
    }
    let set_op = CobolSetReturnCodeOp { code: callee_rc }.build(state.ctx, loc)?;
    let _ = block.append_operation(set_op);
    let HirCallReturning::Place(dst) = returning else {
        return Ok(());
    };
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i32_ty: Type<'ctx> = IntegerType::new(raw, 32).into();
    let one64 = emit_i64(1, block, prog_ctx, state, loc);
    let four64 = emit_i64(4, block, prog_ctx, state, loc);
    let src_slot = append_val(
        prog_ctx.entry_block,
        llvm::alloca(
            raw,
            one64,
            ptr_ty,
            loc,
            llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i32_ty))),
        ),
        prog_ctx.fallback_val,
    );
    let _ = block.append_operation(llvm::store(
        raw,
        callee_rc,
        src_slot,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let src_spec = make_runtime_field_spec(
        HirType::BinaryInt {
            byte_width: 4,
            digits: DigitCount::try_new(9).unwrap_or(DigitCount::ONE),
            scale: ScaleFactor::ZERO,
            sign: Signedness::Signed,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
        },
        prog_ctx.prog.formatting,
    );
    let dst_view = lower_place_view(dst, block, prog_ctx, state)?;
    let (src_sp_p, src_sp_l) = emit_spec_slice(&src_spec, block, prog_ctx, state, loc)?;
    let (dst_sp_p, dst_sp_l) = emit_spec_slice(&dst_view.spec, block, prog_ctx, state, loc)?;
    let kind_v = emit_i32(
        RuntimeMoveSourceKind::Field.to_abi_code(),
        block,
        prog_ctx,
        state,
        loc,
    );
    let _ = emit_rt_call(
        RtDispatchTarget::MoveDesc,
        &[
            kind_v,
            src_slot,
            four64,
            src_sp_p,
            src_sp_l,
            dst_view.ptr,
            dst_view.len_i64,
            dst_sp_p,
            dst_sp_l,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = (ByteLength::ZERO, ByteOffset::ZERO);
    Ok(())
}

/// Emits post-`CallInvoke` `RETURNING` writeback and `ON EXCEPTION` / `NOT ON EXCEPTION` regions.
///
/// # Arguments
///
/// * `spec` - [`HirCallSpec`].
/// * `status` - `i32` status from `coblang_rt_call_invoke` (`0` = ok, `< 0` = exception).
/// * `callee_rc` - `i32` return value loaded from `rc_slot`.
/// * `block` - Target block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_post_call_branches<'ctx, 'b, 'e: 'b>(
    spec: &HirCallSpec,
    status: Value<'ctx, 'b>,
    callee_rc: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    if spec.on_exception.is_empty() && spec.not_on_exception.is_empty() {
        return store_call_returning(&spec.returning, callee_rc, block, prog_ctx, state, loc);
    }
    let z32 = emit_i32(0, block, prog_ctx, state, loc);
    let is_exc = append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, status, z32, loc),
        prog_ctx.fallback_val,
    );
    let exc_reg = Region::new();
    let exc_blk = exc_reg.append_block(Block::new(&[]));
    for st in &spec.on_exception {
        lower_stmt(st, &exc_blk, prog_ctx, state)?;
    }
    let _ = exc_blk.append_operation(scf::r#yield(&[], loc));

    let ok_reg = Region::new();
    let ok_blk = ok_reg.append_block(Block::new(&[]));
    store_call_returning(&spec.returning, callee_rc, &ok_blk, prog_ctx, state, loc)?;
    for st in &spec.not_on_exception {
        lower_stmt(st, &ok_blk, prog_ctx, state)?;
    }
    let _ = ok_blk.append_operation(scf::r#yield(&[], loc));

    let _ = block.append_operation(scf::r#if(is_exc, &[], exc_reg, ok_reg, loc));
    Ok(())
}
