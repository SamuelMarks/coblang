//! Milestone M12 (~120 ISO/`GnuCOBOL`/`gcobol` intrinsics, `ALL` table-slice expansion,
//! `MODULE-*` call-stack tracking, `FUNCTION-ID` User-Defined Function invocation, and
//! function-result reference modification) HIR-to-`cobol` MLIR lowering.

use coblang_common::Result;
use coblang_hir::{
    DataCategory, HirCallArg, HirExpr, HirExtIntrinsic, HirM12Arg, HirM12Callee,
    HirM12FunctionCall, HirRefMod, HirSliceBound, HirTableSlice, HirType, Signedness,
};
use coblang_rt_core::field_ops::RuntimeMoveSourceKind;
use melior::dialect::arith::CmpiPredicate;
use melior::dialect::{arith, llvm, scf};
use melior::ir::attribute::TypeAttribute;
use melior::ir::block::BlockLike;
use melior::ir::r#type::IntegerType;
use melior::ir::{Block, Location, Region, RegionLike, Type, Value};

use crate::lower_from_hir::{
    ModuleLowerState, ProgramLowerCtx, append_val, emit_const_slice, emit_decimal_expr, emit_i32,
    emit_i64, emit_rt_call, emit_spec_slice, eval_expr_as_i64, lower_move_source_args,
    lower_place_view,
};
use crate::lower_m11::{emit_ptr_to_i64, lower_single_call_arg};
use crate::ops::{
    CallDispatchKind, CobolCallOp, CobolGetReturnCodeOp, CobolSetReturnCodeOp, ExceptionBranches,
    RtDispatchTarget, llvm_ptr_type,
};
use crate::storage_init::make_runtime_field_spec;

/// Emits `coblang_rt_m12_enter_module(mod_ptr, mod_len, src_ptr, src_len)` on unit entry so
/// `MODULE-ID`, `MODULE-NAME`, `MODULE-CALLER-ID`, and `MODULE-SOURCE` track active invocations.
///
/// # Arguments
///
/// * `block` - Entry block of `ProgramEntry`.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if constant-pool emission fails.
pub(crate) fn emit_m12_module_enter<'ctx, 'b, 'e: 'b>(
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let mod_orig = state
        .interner
        .resolve_original(prog_ctx.prog.name)
        .unwrap_or("MAIN");
    let resolved = state.source_map.resolve_span(prog_ctx.prog.span)?;
    let src_base = std::path::Path::new(&resolved.file_path)
        .file_name()
        .map_or(resolved.file_path.as_str(), |f| {
            f.to_str().unwrap_or("source.cob")
        });
    let (mp, ml) = emit_const_slice(mod_orig.as_bytes(), block, prog_ctx, state, loc)?;
    let (sp, sl) = emit_const_slice(src_base.as_bytes(), block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::M12EnterModule,
        &[mp, ml, sp, sl],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Lowers `HirM12FunctionCall` as a 5-value `MOVE` / comparison source tuple
/// `(src_kind_i32, src_ptr, src_len_i64, src_spec_ptr, src_spec_len_i64)`.
///
/// # Arguments
///
/// * `call` - Resolved M12 function call.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if argument or runtime dispatch lowering fails.
pub(crate) fn lower_m12_call_move_source<'ctx, 'b, 'e: 'b>(
    call: &HirM12FunctionCall,
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
    let (empty_ptr, zero_i64) = emit_const_slice(b"", block, prog_ctx, state, loc)?;

    if let HirM12Callee::UserDefined { external_name } = call.callee {
        let (raw_ptr, raw_len) =
            eval_udf_call_raw(external_name, call, block, prog_ctx, state, loc)?;
        if let Some(rm) = &call.refmod {
            let is_nat = matches!(call.return_ty, HirType::National { .. });
            let (sp, sl) = apply_m12_refmod(
                (raw_ptr, raw_len),
                is_nat,
                rm,
                call,
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let kind = if is_nat {
                RuntimeMoveSourceKind::NationalLiteral
            } else {
                RuntimeMoveSourceKind::AlphanumericLiteral
            };
            let kv = emit_i32(kind.to_abi_code(), block, prog_ctx, state, loc);
            return Ok((kv, sp, sl, empty_ptr, zero_i64));
        }
        let spec = make_runtime_field_spec(call.return_ty.clone(), prog_ctx.prog.formatting);
        let (spec_ptr, spec_len) = emit_spec_slice(&spec, block, prog_ctx, state, loc)?;
        let kv = emit_i32(
            RuntimeMoveSourceKind::Field.to_abi_code(),
            block,
            prog_ctx,
            state,
            loc,
        );
        return Ok((kv, raw_ptr, raw_len, spec_ptr, spec_len));
    }

    if matches!(
        call.return_ty.category(),
        DataCategory::Numeric | DataCategory::Index
    ) {
        emit_m12_call_decimal(call, block, prog_ctx, state, loc)?;
        let abi_code = intrinsic_abi_code(&call.callee);
        let (p, l) =
            emit_format_top_decimal(0, abi_code, &call.return_ty, block, prog_ctx, state, loc)?;
        let kv = emit_i32(
            RuntimeMoveSourceKind::DecimalAscii.to_abi_code(),
            block,
            prog_ctx,
            state,
            loc,
        );
        return Ok((kv, p, l, empty_ptr, zero_i64));
    }

    let (ptr, len_i64) = eval_intrinsic_bytes_with_refmod(call, block, prog_ctx, state, loc)?;
    let kind = if matches!(call.return_ty, HirType::National { .. }) {
        RuntimeMoveSourceKind::NationalLiteral
    } else {
        RuntimeMoveSourceKind::AlphanumericLiteral
    };
    let kv = emit_i32(kind.to_abi_code(), block, prog_ctx, state, loc);
    Ok((kv, ptr, len_i64, empty_ptr, zero_i64))
}

/// Evaluates `call` and pushes its exact `FixedDecimal` result onto `rt.decimal_stack_mut()`.
///
/// # Arguments
///
/// * `call` - Resolved numeric/integer M12 function call.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if argument lowering fails.
pub(crate) fn emit_m12_call_decimal<'ctx, 'b, 'e: 'b>(
    call: &HirM12FunctionCall,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    if let HirM12Callee::UserDefined { external_name } = call.callee {
        let (ret_ptr, ret_len) =
            eval_udf_call_raw(external_name, call, block, prog_ctx, state, loc)?;
        let spec = make_runtime_field_spec(call.return_ty.clone(), prog_ctx.prog.formatting);
        let (spec_ptr, spec_len) = emit_spec_slice(&spec, block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::DecPushField,
            &[ret_ptr, ret_len, spec_ptr, spec_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok(());
    }

    stage_intrinsic_frame(call, block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::M12EvalToDec,
        &[],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Pre-evaluated `(ptr, len, optional_field_spec)` tuple for `DISPLAY` staging of an `M12Call`.
pub(crate) type M12DisplayPreEval<'ctx, 'b> = (
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Option<(Value<'ctx, 'b>, Value<'ctx, 'b>)>,
);

/// Evaluates an `M12Call` prior to `coblang_rt_disp_begin` so nested `DISPLAY` statements inside UDFs
/// do not collide with the outer `DISPLAY` staging buffer.
///
/// # Arguments
///
/// * `call` - Target [`HirM12FunctionCall`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if evaluation lowering fails.
pub(crate) fn pre_eval_m12_for_display<'ctx, 'b, 'e: 'b>(
    call: &HirM12FunctionCall,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<M12DisplayPreEval<'ctx, 'b>> {
    if let HirM12Callee::UserDefined { external_name } = call.callee {
        let (raw_ptr, raw_len) =
            eval_udf_call_raw(external_name, call, block, prog_ctx, state, loc)?;
        if let Some(rm) = &call.refmod {
            let is_nat = matches!(call.return_ty, HirType::National { .. });
            let (sp, sl) = apply_m12_refmod(
                (raw_ptr, raw_len),
                is_nat,
                rm,
                call,
                block,
                prog_ctx,
                state,
                loc,
            )?;
            if is_nat {
                let spec =
                    make_runtime_field_spec(call.return_ty.clone(), prog_ctx.prog.formatting);
                let spec_pair = emit_spec_slice(&spec, block, prog_ctx, state, loc)?;
                return Ok((sp, sl, Some(spec_pair)));
            }
            return Ok((sp, sl, None));
        }
        let spec = make_runtime_field_spec(call.return_ty.clone(), prog_ctx.prog.formatting);
        let spec_pair = emit_spec_slice(&spec, block, prog_ctx, state, loc)?;
        return Ok((raw_ptr, raw_len, Some(spec_pair)));
    }

    if matches!(
        call.return_ty.category(),
        DataCategory::Numeric | DataCategory::Index
    ) {
        let _ = emit_rt_call(RtDispatchTarget::DecBegin, &[], block, prog_ctx, state, loc)?;
        emit_m12_call_decimal(call, block, prog_ctx, state, loc)?;
        let abi_code = intrinsic_abi_code(&call.callee);
        let (p, l) =
            emit_format_top_decimal(1, abi_code, &call.return_ty, block, prog_ctx, state, loc)?;
        return Ok((p, l, None));
    }

    let (ptr, len_i64) = eval_intrinsic_bytes_with_refmod(call, block, prog_ctx, state, loc)?;
    if matches!(call.return_ty, HirType::National { .. }) {
        let spec = make_runtime_field_spec(call.return_ty.clone(), prog_ctx.prog.formatting);
        let spec_pair = emit_spec_slice(&spec, block, prog_ctx, state, loc)?;
        return Ok((ptr, len_i64, Some(spec_pair)));
    }
    Ok((ptr, len_i64, None))
}

/// Evaluates an alphanumeric/national intrinsic and applies optional `(offset : [length])` reference modification.
///
/// # Arguments
///
/// * `call` - Lowered [`HirM12FunctionCall`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn eval_intrinsic_bytes_with_refmod<'ctx, 'b, 'e: 'b>(
    call: &HirM12FunctionCall,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    stage_intrinsic_frame(call, block, prog_ctx, state, loc)?;
    let (out_ptr_slot, out_len_slot) = alloc_ptr_len_slots(prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::M12EvalBytes,
        &[out_ptr_slot, out_len_slot],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();
    let p = append_val(
        block,
        llvm::load(
            raw,
            out_ptr_slot,
            ptr_ty,
            loc,
            llvm::LoadStoreOptions::new(),
        ),
        prog_ctx.fallback_val,
    );
    let l = append_val(
        block,
        llvm::load(
            raw,
            out_len_slot,
            i64_ty,
            loc,
            llvm::LoadStoreOptions::new(),
        ),
        prog_ctx.fallback_val,
    );
    if let Some(rm) = &call.refmod {
        let is_nat = matches!(call.return_ty, HirType::National { .. });
        return apply_m12_refmod((p, l), is_nat, rm, call, block, prog_ctx, state, loc);
    }
    Ok((p, l))
}

/// Applies `coblang_rt_m12_apply_refmod` to `(in_ptr, in_len)`.
///
/// # Arguments
///
/// * `in_slice` - Base `(ptr, len_i64)` byte slice.
/// * `is_national` - Whether `in_slice` uses UTF-16BE character units.
/// * `rm` - Reference-modification `(offset : [length])` spec.
/// * `call` - Enclosing [`HirM12FunctionCall`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
#[expect(
    clippy::too_many_arguments,
    reason = "Forwards lowered slice pair, encoding tag, refmod AST, and MLIR context."
)]
fn apply_m12_refmod<'ctx, 'b, 'e: 'b>(
    in_slice: (Value<'ctx, 'b>, Value<'ctx, 'b>),
    is_national: bool,
    rm: &HirRefMod,
    call: &HirM12FunctionCall,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let off_v = eval_expr_as_i64(&rm.offset, block, prog_ctx, state)?;
    let (len_v, has_len_v) = if let Some(le) = &rm.length {
        (
            eval_expr_as_i64(le, block, prog_ctx, state)?,
            emit_i32(1, block, prog_ctx, state, loc),
        )
    } else {
        (
            emit_i64(0, block, prog_ctx, state, loc),
            emit_i32(0, block, prog_ctx, state, loc),
        )
    };
    let is_nat_v = emit_i32(i32::from(is_national), block, prog_ctx, state, loc);
    let check_en = emit_i32(
        i32::from(prog_ctx.has_ec_tracking),
        block,
        prog_ctx,
        state,
        loc,
    );
    let is_fat = emit_i32(0, block, prog_ctx, state, loc);
    let loc_prefix = state
        .interner
        .resolve_original(call.location_sym)
        .unwrap_or("");
    let line_num = state
        .source_map
        .resolve_span(call.span)
        .map_or(1, |sl| sl.line.get());
    let full_loc = format!("{loc_prefix}; {line_num}");
    let (lp, ll) = emit_const_slice(full_loc.as_bytes(), block, prog_ctx, state, loc)?;
    let (out_ptr_slot, out_len_slot) = alloc_ptr_len_slots(prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::M12ApplyRefMod,
        &[
            in_slice.0,
            in_slice.1,
            is_nat_v,
            off_v,
            len_v,
            has_len_v,
            check_en,
            is_fat,
            lp,
            ll,
            out_ptr_slot,
            out_len_slot,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();
    let sp = append_val(
        block,
        llvm::load(
            raw,
            out_ptr_slot,
            ptr_ty,
            loc,
            llvm::LoadStoreOptions::new(),
        ),
        prog_ctx.fallback_val,
    );
    let sl = append_val(
        block,
        llvm::load(
            raw,
            out_len_slot,
            i64_ty,
            loc,
            llvm::LoadStoreOptions::new(),
        ),
        prog_ctx.fallback_val,
    );
    let two = emit_i32(2, block, prog_ctx, state, loc);
    let _ = two;
    Ok((sp, sl))
}

/// Stages `M12Begin`, `M12SetContext`, and all `HirM12Arg` items for `call`.
///
/// # Arguments
///
/// * `call` - Lowered [`HirM12FunctionCall`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn stage_intrinsic_frame<'ctx, 'b, 'e: 'b>(
    call: &HirM12FunctionCall,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let abi_code = intrinsic_abi_code(&call.callee);
    let code_v = emit_i32(abi_code, block, prog_ctx, state, loc);
    let comma_v = emit_i32(
        i32::from(prog_ctx.prog.formatting.decimal_char == b','),
        block,
        prog_ctx,
        state,
        loc,
    );
    let _ = emit_rt_call(
        RtDispatchTarget::M12Begin,
        &[code_v, comma_v],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    let mut curr_utf8 = [0_u8; 4];
    let curr_b = prog_ctx
        .prog
        .formatting
        .currency_symbol
        .encode_utf8(&mut curr_utf8)
        .as_bytes();
    let (cp, cl) = emit_const_slice(curr_b, block, prog_ctx, state, loc)?;
    let (coll_p, coll_l) = if let Some(alpha) = prog_ctx
        .prog
        .collating_sequence
        .and_then(|idx| prog_ctx.prog.alphabets.get(idx))
    {
        emit_const_slice(&alpha.ranks, block, prog_ctx, state, loc)?
    } else {
        let null_p = append_val(
            block,
            llvm::zero(llvm_ptr_type(state.ctx), loc),
            prog_ctx.fallback_val,
        );
        (null_p, emit_i64(0, block, prog_ctx, state, loc))
    };
    let loc_prefix = state
        .interner
        .resolve_original(call.location_sym)
        .unwrap_or("");
    let line_num = state
        .source_map
        .resolve_span(call.span)
        .map_or(1, |sl| sl.line.get());
    let full_loc = format!("{loc_prefix}; {line_num}");
    let (lp, ll) = emit_const_slice(full_loc.as_bytes(), block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::M12SetContext,
        &[cp, cl, coll_p, coll_l, lp, ll],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    for arg in &call.args {
        stage_single_m12_arg(arg, block, prog_ctx, state, loc)?;
    }
    Ok(())
}

/// Lowers one [`HirM12Arg`] onto the active M12 evaluation frame.
///
/// # Arguments
///
/// * `arg` - Lowered [`HirM12Arg`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn stage_single_m12_arg<'ctx, 'b, 'e: 'b>(
    arg: &HirM12Arg,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    match arg {
        HirM12Arg::Omitted => {
            let _ = emit_rt_call(
                RtDispatchTarget::M12PushOmittedArg,
                &[],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirM12Arg::Keyword(sym) => {
            let kw = state.interner.resolve_canonical(*sym).unwrap_or("");
            let (kp, kl) = emit_const_slice(kw.as_bytes(), block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::M12PushKeywordArg,
                &[kp, kl],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirM12Arg::AlgebraicProbe(spec) => {
            let d = emit_i32(i32::from(spec.digits), block, prog_ctx, state, loc);
            let s = emit_i32(i32::from(spec.scale), block, prog_ctx, state, loc);
            let sg = emit_i32(
                i32::from(spec.signedness == Signedness::Signed),
                block,
                prog_ctx,
                state,
                loc,
            );
            let _ = emit_rt_call(
                RtDispatchTarget::M12PushAlgProbe,
                &[d, s, sg],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirM12Arg::OdoLengthProbe {
            base_bytes,
            odo_place,
            elem_stride,
        } => {
            let odo_v =
                eval_expr_as_i64(&HirExpr::Place(odo_place.clone()), block, prog_ctx, state)?;
            let stride_v = emit_i64(i64::from(*elem_stride), block, prog_ctx, state, loc);
            let base_v = emit_i64(i64::from(*base_bytes), block, prog_ctx, state, loc);
            let prod = append_val(
                block,
                arith::muli(odo_v, stride_v, loc),
                prog_ctx.fallback_val,
            );
            let tot = append_val(block, arith::addi(base_v, prod, loc), prog_ctx.fallback_val);
            push_i64_onto_decimal_stack(tot, block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::M12PopDecArg,
                &[],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirM12Arg::RawStoragePlace(pl) => {
            let view = lower_place_view(pl, block, prog_ctx, state)?;
            let cat = if matches!(pl.effective_ty, HirType::Pointer | HirType::ProgramPointer) {
                3
            } else {
                bytes_cat_code(&pl.effective_ty)
            };
            let cv = emit_i32(cat, block, prog_ctx, state, loc);
            let _ = emit_rt_call(
                RtDispatchTarget::M12PushBytesArg,
                &[view.ptr, view.len_i64, cv],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirM12Arg::Expr(expr) => {
            stage_expr_m12_arg(expr, block, prog_ctx, state, loc)?;
        }
        HirM12Arg::TableSlice(slice) => {
            stage_table_slice_m12_arg(slice, block, prog_ctx, state, loc)?;
        }
    }
    Ok(())
}

/// Stages a scalar [`HirExpr`] argument (pointer, numeric, or byte string) onto the active M12 frame.
///
/// # Arguments
///
/// * `expr` - Argument [`HirExpr`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn stage_expr_m12_arg<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    if let HirExpr::AddressOf(pl) = expr {
        let p = crate::lower_m11::eval_address_of_place(pl, block, prog_ctx, state)?;
        let raw = state.ctx.raw();
        let ptr_ty = llvm_ptr_type(state.ctx);
        let one_64 = emit_i64(1, block, prog_ctx, state, loc);
        let slot = append_val(
            prog_ctx.entry_block,
            llvm::alloca(
                raw,
                one_64,
                ptr_ty,
                loc,
                llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(ptr_ty))),
            ),
            prog_ctx.fallback_val,
        );
        let _ = block.append_operation(llvm::store(
            raw,
            p,
            slot,
            loc,
            llvm::LoadStoreOptions::new(),
        ));
        let eight = emit_i64(8, block, prog_ctx, state, loc);
        let three = emit_i32(3, block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::M12PushBytesArg,
            &[slot, eight, three],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok(());
    }

    if is_numeric_m12_expr(expr) {
        emit_decimal_expr(expr, block, prog_ctx, state)?;
        let _ = emit_rt_call(
            RtDispatchTarget::M12PopDecArg,
            &[],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    } else {
        let (_, ptr, len_i64, _, _) = lower_move_source_args(expr, block, prog_ctx, state, loc)?;
        let cat = expr_bytes_cat_code(expr);
        let cv = emit_i32(cat, block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::M12PushBytesArg,
            &[ptr, len_i64, cv],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Expands a multi-dimensional `ALL` [`HirTableSlice`] via nested `scf.for` loops.
///
/// # Arguments
///
/// * `slice` - Lowered `ALL` [`HirTableSlice`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn stage_table_slice_m12_arg<'ctx, 'b, 'e: 'b>(
    slice: &HirTableSlice,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let is_num = matches!(
        slice.base_place.effective_ty.category(),
        DataCategory::Numeric | DataCategory::Index
    );
    let is_num_v = emit_i32(i32::from(is_num), block, prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::M12SeriesBegin,
        &[is_num_v],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    let mut unmod_place = slice.base_place.clone();
    let saved_refmod = unmod_place.refmod.take();
    let base_view = lower_place_view(&unmod_place, block, prog_ctx, state)?;
    let spec_pair = if is_num {
        Some(emit_spec_slice(
            &base_view.spec,
            block,
            prog_ctx,
            state,
            loc,
        )?)
    } else {
        None
    };

    emit_slice_dim_loop(
        0,
        &slice.varying_dims,
        (base_view.ptr, base_view.len_i64),
        (saved_refmod.as_ref(), &slice.base_place),
        spec_pair,
        block,
        prog_ctx,
        state,
        loc,
    )?;

    let _ = emit_rt_call(
        RtDispatchTarget::M12SeriesEnd,
        &[],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Recursively emits nested `scf.for` loops across `varying_dims[dim_idx..]`.
///
/// # Arguments
///
/// * `dim_idx` - Current varying dimension index.
/// * `varying_dims` - Slice of `(HirSliceBound, byte_stride)` dimensions.
/// * `cur_slice` - Current `(ptr, len_i64)` element slice.
/// * `refmod_ctx` - Optional element reference-modification spec and originating place.
/// * `spec_pair` - Optional numeric field descriptor `(spec_ptr, spec_len)`.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
#[expect(
    clippy::too_many_arguments,
    reason = "Recursive multi-dimensional `scf.for` emitter carrying base slice, refmod, and MLIR context."
)]
#[expect(
    clippy::too_many_lines,
    reason = "Complete recursive `ALL` table-slice `scf.for` loop and leaf refmod emitter."
)]
fn emit_slice_dim_loop<'ctx, 'b, 'e: 'b>(
    dim_idx: usize,
    varying_dims: &[(HirSliceBound, u32)],
    cur_slice: (Value<'ctx, 'b>, Value<'ctx, 'b>),
    refmod_ctx: (Option<&HirRefMod>, &coblang_hir::HirPlace),
    spec_pair: Option<(Value<'ctx, 'b>, Value<'ctx, 'b>)>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let Some((bound, stride)) = varying_dims.get(dim_idx) else {
        let (mut elem_ptr, mut elem_len) = cur_slice;
        if let (Some(rm), orig_place) = refmod_ctx {
            let base_v = crate::lower_from_hir::RefModBaseView {
                ptr: elem_ptr,
                byte_len: elem_len,
            };
            (elem_ptr, elem_len) = crate::lower_from_hir::lower_place_refmod(
                orig_place, rm, &base_v, block, prog_ctx, state, loc,
            )?;
        }
        if let Some((sp, sl)) = spec_pair {
            let _ = emit_rt_call(
                RtDispatchTarget::DecPushField,
                &[elem_ptr, elem_len, sp, sl],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let _ = emit_rt_call(
                RtDispatchTarget::M12PopDecArg,
                &[],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        } else {
            let cv = emit_i32(
                bytes_cat_code(&refmod_ctx.1.effective_ty),
                block,
                prog_ctx,
                state,
                loc,
            );
            let _ = emit_rt_call(
                RtDispatchTarget::M12PushBytesArg,
                &[elem_ptr, elem_len, cv],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        return Ok(());
    };

    let raw = state.ctx.raw();
    let idx_ty = Type::index(raw);
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();

    let count_i64 = match bound {
        HirSliceBound::StaticCount(c) => emit_i64(i64::from(*c), block, prog_ctx, state, loc),
        HirSliceBound::DynamicOdo {
            dep_place,
            max_occurs,
        } => {
            let raw_c =
                eval_expr_as_i64(&HirExpr::Place(dep_place.clone()), block, prog_ctx, state)?;
            let z = emit_i64(0, block, prog_ctx, state, loc);
            let mx = emit_i64(i64::from(*max_occurs), block, prog_ctx, state, loc);
            let ge0 = append_val(block, arith::maxsi(raw_c, z, loc), prog_ctx.fallback_val);
            append_val(block, arith::minsi(ge0, mx, loc), prog_ctx.fallback_val)
        }
    };

    let zero_idx = append_val(
        block,
        arith::index_cast(emit_i64(0, block, prog_ctx, state, loc), idx_ty, loc),
        prog_ctx.fallback_val,
    );
    let one_idx = append_val(
        block,
        arith::index_cast(emit_i64(1, block, prog_ctx, state, loc), idx_ty, loc),
        prog_ctx.fallback_val,
    );
    let upper_idx = append_val(
        block,
        arith::index_cast(count_i64, idx_ty, loc),
        prog_ctx.fallback_val,
    );

    let body_reg = Region::new();
    let body_blk = body_reg.append_block(Block::new(&[(idx_ty, loc)]));
    let iv = body_blk
        .argument(0)
        .map_or(prog_ctx.fallback_val, Into::into);
    let iv_i64 = append_val(
        &body_blk,
        arith::index_cast(iv, i64_ty, loc),
        prog_ctx.fallback_val,
    );
    let stride_v = emit_i64(i64::from(*stride), &body_blk, prog_ctx, state, loc);
    let byte_off = append_val(
        &body_blk,
        arith::muli(iv_i64, stride_v, loc),
        prog_ctx.fallback_val,
    );
    let next_ptr = append_val(
        &body_blk,
        llvm::get_element_ptr_dynamic(raw, cur_slice.0, &[byte_off], i8_ty, ptr_ty, loc),
        prog_ctx.fallback_val,
    );

    emit_slice_dim_loop(
        dim_idx + 1,
        varying_dims,
        (next_ptr, cur_slice.1),
        refmod_ctx,
        spec_pair,
        &body_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = body_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#for(zero_idx, upper_idx, one_idx, body_reg, loc));
    Ok(())
}

/// Invokes a compiled User-Defined Function (`__UDF_<EXT_UPPER>`), passing the `RETURNING` buffer
/// at call-frame slot `0` and user arguments at slots `1..=k`, and returns `(ret_ptr, ret_len_i64)`.
///
/// # Arguments
///
/// * `external_name` - Interned external UDF symbol.
/// * `call` - Enclosing [`HirM12FunctionCall`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
#[expect(
    clippy::too_many_lines,
    reason = "Complete UDF argument marshalling, in-module CobolCallOp, and dynamic C-ABI dispatch."
)]
fn eval_udf_call_raw<'ctx, 'b, 'e: 'b>(
    external_name: coblang_common::SymbolId,
    call: &HirM12FunctionCall,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();
    let i32_ty: Type<'ctx> = IntegerType::new(raw, 32).into();
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();

    let ret_bytes_u32 = call.return_ty.static_byte_size().max(1);
    let ret_len_v = emit_i64(i64::from(ret_bytes_u32), block, prog_ctx, state, loc);
    let ret_buf = append_val(
        prog_ctx.entry_block,
        llvm::alloca(
            raw,
            ret_len_v,
            ptr_ty,
            loc,
            llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i8_ty))),
        ),
        prog_ctx.fallback_val,
    );
    // Zero/space-initialize `ret_buf` before invocation.
    let init_byte: u8 = if matches!(
        call.return_ty.category(),
        DataCategory::Numeric | DataCategory::Index
    ) {
        b'0'
    } else {
        b' '
    };
    let init_vec = vec![init_byte; usize::try_from(ret_bytes_u32).unwrap_or(1)];
    let (init_p, init_l) = emit_const_slice(&init_vec, block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::RawCopy,
        &[ret_buf, init_p, init_l],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    let total_argc = 1_usize.saturating_add(call.args.len());
    let slots_v = emit_i64(
        i64::try_from(total_argc.saturating_mul(2)).unwrap_or(2),
        block,
        prog_ctx,
        state,
        loc,
    );
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

    // Slot 0: `RETURNING` buffer pointer + byte size.
    let ret_ptr_word = emit_ptr_to_i64(ret_buf, block, prog_ctx, state, loc);
    let idx0 = emit_i64(0, block, prog_ctx, state, loc);
    let idx1 = emit_i64(1, block, prog_ctx, state, loc);
    let s0 = append_val(
        block,
        llvm::get_element_ptr_dynamic(raw, arr_ptr, &[idx0], i64_ty, ptr_ty, loc),
        prog_ctx.fallback_val,
    );
    let s1 = append_val(
        block,
        llvm::get_element_ptr_dynamic(raw, arr_ptr, &[idx1], i64_ty, ptr_ty, loc),
        prog_ctx.fallback_val,
    );
    let _ = block.append_operation(llvm::store(
        raw,
        ret_ptr_word,
        s0,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let _ = block.append_operation(llvm::store(
        raw,
        ret_len_v,
        s1,
        loc,
        llvm::LoadStoreOptions::new(),
    ));

    let ext_orig = state.interner.resolve_original(external_name).unwrap_or("");
    let udf_key = format!("__UDF_{}", ext_orig.trim().to_ascii_uppercase());
    let formal_info = state.udf_param_types.get(&udf_key).cloned();

    // Slots 1..=k: user arguments.
    let _ = emit_rt_call(
        RtDispatchTarget::M13RecFrameBegin,
        &[],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    for (idx, m12_arg) in call.args.iter().enumerate() {
        let formal_ty_opt = formal_info
            .as_ref()
            .and_then(|(tys, fmt)| tys.get(idx).cloned().map(|t| (t, *fmt)));
        let (pw, mw) = if let (HirM12Arg::Expr(e), Some((formal_ty, formal_fmt))) =
            (m12_arg, formal_ty_opt)
            && !matches!(e, HirExpr::Place(_))
        {
            let f_bytes = i64::from(formal_ty.static_byte_size().max(1));
            let f_len_v = emit_i64(f_bytes, block, prog_ctx, state, loc);
            let tmp_buf = append_val(
                prog_ctx.entry_block,
                llvm::alloca(
                    raw,
                    f_len_v,
                    ptr_ty,
                    loc,
                    llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i8_ty))),
                ),
                prog_ctx.fallback_val,
            );
            let dst_spec = make_runtime_field_spec(formal_ty, formal_fmt);
            let (dsp, dsl) = emit_spec_slice(&dst_spec, block, prog_ctx, state, loc)?;
            if matches!(e, HirExpr::Binary { .. } | HirExpr::Neg(_)) {
                let _ = emit_rt_call(RtDispatchTarget::DecBegin, &[], block, prog_ctx, state, loc)?;
                emit_decimal_expr(e, block, prog_ctx, state)?;
                let z0 = emit_i32(0, block, prog_ctx, state, loc);
                let _ = emit_rt_call(
                    RtDispatchTarget::DecStore,
                    &[tmp_buf, f_len_v, dsp, dsl, z0, z0, z0],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            } else {
                let (sk, sp, sl, ssp, ssl) =
                    lower_move_source_args(e, block, prog_ctx, state, loc)?;
                let _ = emit_rt_call(
                    RtDispatchTarget::MoveDesc,
                    &[sk, sp, sl, ssp, ssl, tmp_buf, f_len_v, dsp, dsl],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            }
            (
                emit_ptr_to_i64(tmp_buf, block, prog_ctx, state, loc),
                f_len_v,
            )
        } else {
            let call_arg = match m12_arg {
                HirM12Arg::RawStoragePlace(pl) | HirM12Arg::Expr(HirExpr::Place(pl)) => {
                    HirCallArg::ByReference(pl.clone())
                }
                HirM12Arg::Expr(e) => HirCallArg::ByContent(e.clone()),
                HirM12Arg::Omitted
                | HirM12Arg::Keyword(_)
                | HirM12Arg::AlgebraicProbe(_)
                | HirM12Arg::OdoLengthProbe { .. }
                | HirM12Arg::TableSlice(_) => HirCallArg::Omitted,
            };
            lower_single_call_arg(&call_arg, block, prog_ctx, state, loc)?
        };
        let base_slot = i64::try_from((idx + 1).saturating_mul(2)).unwrap_or(2);
        let p_idx = emit_i64(base_slot, block, prog_ctx, state, loc);
        let m_idx = emit_i64(base_slot + 1, block, prog_ctx, state, loc);
        let p_slot = append_val(
            block,
            llvm::get_element_ptr_dynamic(raw, arr_ptr, &[p_idx], i64_ty, ptr_ty, loc),
            prog_ctx.fallback_val,
        );
        let m_slot = append_val(
            block,
            llvm::get_element_ptr_dynamic(raw, arr_ptr, &[m_idx], i64_ty, ptr_ty, loc),
            prog_ctx.fallback_val,
        );
        let _ = block.append_operation(llvm::store(
            raw,
            pw,
            p_slot,
            loc,
            llvm::LoadStoreOptions::new(),
        ));
        let _ = block.append_operation(llvm::store(
            raw,
            mw,
            m_slot,
            loc,
            llvm::LoadStoreOptions::new(),
        ));
    }

    let caller_orig = state
        .interner
        .resolve_original(prog_ctx.prog.name)
        .unwrap_or("MAIN");
    let mut caller_nul = Vec::with_capacity(caller_orig.len() + 1);
    caller_nul.extend_from_slice(caller_orig.as_bytes());
    caller_nul.push(0);
    let (caller_cstr, _) = emit_const_slice(&caller_nul, block, prog_ctx, state, loc)?;

    let ext_orig = state
        .interner
        .resolve_original(external_name)
        .unwrap_or("UDF");
    let ext_upper = ext_orig.to_ascii_uppercase();
    let udf_prog_key = format!("__UDF_{ext_upper}");
    let mut target_nul = Vec::with_capacity(ext_orig.len() + 1);
    target_nul.extend_from_slice(ext_orig.as_bytes());
    target_nul.push(0);
    let (target_cstr, _) = emit_const_slice(&target_nul, block, prog_ctx, state, loc)?;

    let saved_mod_rc = {
        let get_op = CobolGetReturnCodeOp.build(state.ctx, loc)?;
        append_val(block, get_op, prog_ctx.fallback_val)
    };

    let argc_v = emit_i64(
        i64::try_from(total_argc).unwrap_or(1),
        block,
        prog_ctx,
        state,
        loc,
    );
    if let Some(canon_sym) = state.in_module_programs.get(&udf_prog_key).cloned() {
        let saved_rc = emit_rt_call(
            RtDispatchTarget::PushCallFrame,
            &[caller_cstr, target_cstr, argc_v, arr_ptr],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        let call_op = CobolCallOp {
            dispatch: CallDispatchKind::StaticSymbol,
            callee: &canon_sym,
            args: &[],
            exception_branches: ExceptionBranches::None,
        }
        .build(state.ctx, loc)?;
        let _ = block.append_operation(call_op);
        let one_32 = emit_i32(1, block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::PopCallFrame,
            &[saved_rc, one_32],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    } else {
        let (ext_ptr, ext_len) =
            emit_const_slice(ext_orig.as_bytes(), block, prog_ctx, state, loc)?;
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
        let zero_32 = emit_i32(0, block, prog_ctx, state, loc);
        let _ = block.append_operation(llvm::store(
            raw,
            zero_32,
            rc_slot,
            loc,
            llvm::LoadStoreOptions::new(),
        ));
        let preserve_rc_flags = emit_i32(2, block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::CallInvoke,
            &[
                caller_cstr,
                ext_ptr,
                ext_len,
                preserve_rc_flags,
                argc_v,
                arr_ptr,
                rc_slot,
            ],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }

    let restore_op = CobolSetReturnCodeOp { code: saved_mod_rc }.build(state.ctx, loc)?;
    let _ = block.append_operation(restore_op);
    let _ = emit_rt_call(
        RtDispatchTarget::M13RecFrameRestore,
        &[],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = CmpiPredicate::Eq;

    Ok((ret_buf, ret_len_v))
}

/// Pushes a native `i64` SSA integer onto `rt.decimal_stack_mut()` using an 8-byte native binary descriptor.
///
/// # Arguments
///
/// * `val_i64` - Signed 64-bit integer SSA value.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn push_i64_onto_decimal_stack<'ctx, 'b, 'e: 'b>(
    val_i64: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();
    let one_64 = emit_i64(1, block, prog_ctx, state, loc);
    let slot = append_val(
        prog_ctx.entry_block,
        llvm::alloca(
            raw,
            one_64,
            ptr_ty,
            loc,
            llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i64_ty))),
        ),
        prog_ctx.fallback_val,
    );
    let _ = block.append_operation(llvm::store(
        raw,
        val_i64,
        slot,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let i64_hir_ty = HirType::BinaryInt {
        byte_width: 8,
        digits: coblang_common::DigitCount::try_new(18).unwrap_or(coblang_common::DigitCount::ONE),
        scale: coblang_common::ScaleFactor::ZERO,
        sign: Signedness::Signed,
        endian: coblang_hir::BinaryEndianness::Native,
        trunc: coblang_hir::TruncationPolicy::FullBinaryRange,
    };
    let spec = make_runtime_field_spec(i64_hir_ty, prog_ctx.prog.formatting);
    let (sp, sl) = emit_spec_slice(&spec, block, prog_ctx, state, loc)?;
    let eight = emit_i64(8, block, prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::DecPushField,
        &[slot, eight, sp, sl],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Calls `coblang_rt_m12_format_display_dec` to pop the top decimal and return `(ptr, len_i64)`.
///
/// # Arguments
///
/// * `mode` - Formatting mode (`0` = canonical `MOVE` ASCII, `1` = `DISPLAY` format).
/// * `abi_code` - Intrinsic ABI code (`0` for UDFs).
/// * `ret_ty` - Declared return [`HirType`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_format_top_decimal<'ctx, 'b, 'e: 'b>(
    mode: i32,
    abi_code: i32,
    ret_ty: &HirType,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let (digits, scale, signed) = match *ret_ty {
        HirType::ZonedDecimal {
            digits,
            scale,
            sign,
            ..
        } => (
            i32::from(digits.get()),
            i32::from(scale.get()),
            i32::from(sign != coblang_hir::NumericSignSpec::Unsigned),
        ),
        _ => (0, 0, 1),
    };
    let mode_v = emit_i32(mode, block, prog_ctx, state, loc);
    let code_v = emit_i32(abi_code, block, prog_ctx, state, loc);
    let dig_v = emit_i32(digits, block, prog_ctx, state, loc);
    let scl_v = emit_i32(scale, block, prog_ctx, state, loc);
    let sgn_v = emit_i32(signed, block, prog_ctx, state, loc);
    let comma_v = emit_i32(
        i32::from(prog_ctx.prog.formatting.decimal_char == b','),
        block,
        prog_ctx,
        state,
        loc,
    );
    let (out_ptr_slot, out_len_slot) = alloc_ptr_len_slots(prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::M12FormatDisplayDec,
        &[
            mode_v,
            code_v,
            dig_v,
            scl_v,
            sgn_v,
            comma_v,
            out_ptr_slot,
            out_len_slot,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();
    let p = append_val(
        block,
        llvm::load(
            raw,
            out_ptr_slot,
            ptr_ty,
            loc,
            llvm::LoadStoreOptions::new(),
        ),
        prog_ctx.fallback_val,
    );
    let l = append_val(
        block,
        llvm::load(
            raw,
            out_len_slot,
            i64_ty,
            loc,
            llvm::LoadStoreOptions::new(),
        ),
        prog_ctx.fallback_val,
    );
    Ok((p, l))
}

/// Allocates `(out_ptr_slot, out_len_slot)` in `prog_ctx.entry_block`.
///
/// # Arguments
///
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn alloc_ptr_len_slots<'ctx, 'e>(
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> (Value<'ctx, 'e>, Value<'ctx, 'e>) {
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();
    let one_64 = emit_i64(1, prog_ctx.entry_block, prog_ctx, state, loc);
    let p_slot = append_val(
        prog_ctx.entry_block,
        llvm::alloca(
            raw,
            one_64,
            ptr_ty,
            loc,
            llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(ptr_ty))),
        ),
        prog_ctx.fallback_val,
    );
    let l_slot = append_val(
        prog_ctx.entry_block,
        llvm::alloca(
            raw,
            one_64,
            ptr_ty,
            loc,
            llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i64_ty))),
        ),
        prog_ctx.fallback_val,
    );
    (p_slot, l_slot)
}

/// Maps `HirM12Callee` to its positive (`1..=116`) or negative (`-1..-5`) C-ABI integer code.
///
/// # Arguments
///
/// * `callee` - Target [`HirM12Callee`].
#[must_use]
const fn intrinsic_abi_code(callee: &HirM12Callee) -> i32 {
    match callee {
        HirM12Callee::Intrinsic { abi_code } => *abi_code as i32,
        HirM12Callee::ExtIntrinsic(HirExtIntrinsic::ContentLength) => -1,
        HirM12Callee::ExtIntrinsic(HirExtIntrinsic::NumericDecimalPoint) => -2,
        HirM12Callee::ExtIntrinsic(HirExtIntrinsic::NumericThousandsSeparator) => -3,
        HirM12Callee::ExtIntrinsic(HirExtIntrinsic::MonetaryDecimalPoint) => -4,
        HirM12Callee::ExtIntrinsic(HirExtIntrinsic::MonetaryThousandsSeparator)
        | HirM12Callee::UserDefined { .. } => -5,
    }
}

/// Returns `true` when `expr` evaluates to a numeric value for intrinsic argument staging.
///
/// # Arguments
///
/// * `expr` - Candidate [`HirExpr`].
#[must_use]
fn is_numeric_m12_expr(expr: &HirExpr) -> bool {
    matches!(
        expr,
        HirExpr::Neg(_)
            | HirExpr::Binary { .. }
            | HirExpr::LengthOf(_)
            | HirExpr::Const {
                value: coblang_hir::HirConstValue::Decimal { .. }
                    | coblang_hir::HirConstValue::Float(_)
                    | coblang_hir::HirConstValue::Figurative(coblang_hir::HirFigurative::Zero),
                ..
            }
    ) || matches!(
        expr,
        HirExpr::Place(pl)
            if matches!(pl.effective_ty.category(), DataCategory::Numeric | DataCategory::Index)
    ) || matches!(
        expr,
        HirExpr::M12Call(c)
            if matches!(c.return_ty.category(), DataCategory::Numeric | DataCategory::Index)
    ) || matches!(
        expr,
        HirExpr::M20(m)
            if matches!(
                m.as_ref(),
                coblang_hir::HirM20Expr::OoInlineInvoke(inv)
                    if matches!(inv.return_ty.category(), DataCategory::Numeric | DataCategory::Index)
            )
    )
}

/// Maps `HirType` to the `cat_code` expected by `coblang_rt_m12_push_bytes_arg`.
///
/// # Arguments
///
/// * `ty` - Element [`HirType`].
#[must_use]
const fn bytes_cat_code(ty: &HirType) -> i32 {
    match ty {
        HirType::National { .. } | HirType::NationalEdited { .. } => 1,
        HirType::Utf8 { .. } => 2,
        _ => 0,
    }
}

/// Infers `bytes_cat_code` for an arbitrary byte-producing `HirExpr`.
///
/// # Arguments
///
/// * `expr` - Byte-producing [`HirExpr`].
#[must_use]
fn expr_bytes_cat_code(expr: &HirExpr) -> i32 {
    match expr {
        HirExpr::Place(pl) => bytes_cat_code(&pl.effective_ty),
        HirExpr::M12Call(c) => bytes_cat_code(&c.return_ty),
        HirExpr::Const {
            value: coblang_hir::HirConstValue::NationalBytes(_),
            ..
        } => 1,
        _ => 0,
    }
}
