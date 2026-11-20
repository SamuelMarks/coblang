//! Milestone M20 (`XML GENERATE`/`PARSE`, `JSON GENERATE`/`PARSE`, `XML-*`/`JSON-*` special registers,
//! and Object-Oriented COBOL `CLASS-ID` / `INVOKE` / inline `::`) HIR-to-`cobol` MLIR lowering.

use std::collections::BTreeSet;

use coblang_common::{DataItemId, DigitCount, LevelNumber, Result, ScaleFactor, SourceSpan};
use coblang_diag::SymbolInterner;
use coblang_dynload::{SymbolMangleStyle, mangle_program_symbol};
use coblang_hir::{
    BinaryEndianness, HirCallArg, HirClass, HirConstValue, HirDataItem, HirDecimalCommaMode,
    HirExpr, HirFigurative, HirGenericSuppressRule, HirJsonDetailMode, HirJsonGenerateStmt,
    HirJsonParseStmt, HirM12Arg, HirM12Callee, HirM12FunctionCall, HirM20Expr, HirM20Stmt,
    HirMarkupNameOverride, HirOoClassSpec, HirOoCompartment, HirOoInlineInvokeExpr,
    HirOoInvokeStmt, HirOoReceiver, HirPlace, HirProgram, HirRelOp, HirStmt, HirStorageSection,
    HirType, HirXmlAttrMode, HirXmlDeclMode, HirXmlGenerateStmt, HirXmlNodeType, HirXmlParseStmt,
    HirXmlStreamMode, JustifiedSpec, Signedness, TruncationPolicy,
};
use coblang_intrinsics::IntrinsicId;
use coblang_rt_core::{
    TrimDirection, desc_codec::encode_field_spec, field_ops::RuntimeMoveSourceKind,
};
use melior::dialect::arith::CmpiPredicate;
use melior::dialect::{arith, llvm, scf};
use melior::ir::attribute::IntegerAttribute;
use melior::ir::block::BlockLike;
use melior::ir::r#type::IntegerType;
use melior::ir::{Block, Location, Region, RegionLike, Type, Value};

use crate::lower_from_hir::{
    LoweredPlaceView, ModuleLowerState, ProgramLowerCtx, RefModBaseView, append_val,
    append_val_pair, emit_const_slice, emit_i32, emit_i64, emit_rt_call, emit_spec_slice,
    emit_storage_ptr, eval_expr_as_i64, lower_move_source_args, lower_place_refmod,
    lower_place_view, lower_stmt,
};
use crate::lower_m14::emit_null_ptr;
use crate::ops::{
    CobolLoopOp, CobolPerformRangeOp, CobolStorageOp, CobolTrimOp, CobolYieldOp, RtDispatchTarget,
    StorageKind,
};
use crate::storage_init::{RootInitStrategy, build_program_storage_plan, make_runtime_field_spec};

/// Guard global ensuring OO class & method registration executes once per module.
const OO_REG_GUARD_SYM: &str = "__coblang_m20_oo_guard";

/// Five-tuple `(kind_i32, data_ptr, data_len, spec_ptr, spec_len)` returned by move-source lowering.
pub(crate) type LoweredMoveSourceArgs<'ctx, 'b> = (
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
);

/// Resolves canonical M20 special register names to `coblang_rt_m20_reg_*` IDs (`0..=10`).
///
/// # Arguments
///
/// * `canon_name` - Uppercase canonical special register name.
fn m20_special_reg_id(canon_name: &str) -> Option<i32> {
    match canon_name {
        "XML-CODE" => Some(0),
        "XML-INFORMATION" => Some(1),
        "XML-EVENT" => Some(2),
        "XML-TEXT" => Some(3),
        "XML-NTEXT" => Some(4),
        "XML-NAMESPACE" => Some(5),
        "XML-NNAMESPACE" => Some(6),
        "XML-NAMESPACE-PREFIX" => Some(7),
        "XML-NNAMESPACE-PREFIX" => Some(8),
        "JSON-CODE" => Some(9),
        "JSON-STATUS" => Some(10),
        _ => None,
    }
}

/// Intercepts M20 `XML-*` and `JSON-*` special registers inside `lower_place_view`.
///
/// # Arguments
/// * `place` - Candidate [`HirPlace`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
/// Returns [`coblang_common::CoblangError`] if runtime dispatch or `RefMod` lowering fails.
pub(crate) fn try_lower_m20_special_reg_view<'ctx, 'b, 'e: 'b>(
    place: &HirPlace,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Option<LoweredPlaceView<'ctx, 'b>>> {
    let Some(item) = prog_ctx.prog.data_items.get(place.item.as_u32() as usize) else {
        return Ok(None);
    };
    if item.section != HirStorageSection::SpecialRegister {
        return Ok(None);
    }
    let Some(canon) = item
        .name
        .and_then(|s| state.interner.resolve_canonical(s).ok())
    else {
        return Ok(None);
    };
    let Some(reg_id) = m20_special_reg_id(canon) else {
        return Ok(None);
    };

    let rid_val = emit_i32(reg_id, block, prog_ctx, state, loc);
    let base_ptr = emit_rt_call(
        RtDispatchTarget::M20RegPtr,
        &[rid_val],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let base_len = emit_rt_call(
        RtDispatchTarget::M20RegLen,
        &[rid_val],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let (ptr, len_i64) = if let Some(rm) = &place.refmod {
        let base_view = RefModBaseView {
            ptr: base_ptr,
            byte_len: base_len,
        };
        lower_place_refmod(place, rm, &base_view, block, prog_ctx, state, loc)?
    } else {
        (base_ptr, base_len)
    };
    let spec = make_runtime_field_spec(place.effective_ty.clone(), prog_ctx.prog.formatting);
    Ok(Some(LoweredPlaceView {
        ptr,
        len_i64,
        spec,
        root_id: None,
    }))
}

/// Lowers `FUNCTION LENGTH` / `BYTE-LENGTH` / `NUMVAL` / `NUMVAL-C` onto `DecimalEvalStack` when encountered as `CallFunction`.
///
/// # Arguments
/// * `fname` - Canonical uppercase function name.
/// * `args` - Call argument expressions.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
/// Returns [`coblang_common::CoblangError`] if operand lowering fails.
pub(crate) fn try_emit_m20_intrinsic_decimal<'ctx, 'b, 'e: 'b>(
    fname: &str,
    args: &[HirExpr],
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<bool> {
    let Some(first) = args.first() else {
        return Ok(false);
    };
    match fname {
        "LENGTH" | "BYTE-LENGTH" => {
            let len_i64 = match first {
                HirExpr::Place(pl) => lower_place_view(pl, block, prog_ctx, state)?.len_i64,
                other => lower_move_source_args(other, block, prog_ctx, state, loc)?.2,
            };
            push_i64_to_decimal_stack(len_i64, block, prog_ctx, state, loc)?;
            Ok(true)
        }
        "NUMVAL" | "NUMVAL-C" => {
            let (ptr, len_i64) = eval_byte_slice_expr(first, block, prog_ctx, state, loc)?;
            let mode = emit_i32(i32::from(fname == "NUMVAL-C"), block, prog_ctx, state, loc);
            let _ = emit_rt_call(
                RtDispatchTarget::M20DecPushNumval,
                &[ptr, len_i64, mode],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Lowers [`HirM12FunctionCall`] (`LENGTH`, `BYTE-LENGTH`, `NUMVAL`, `NUMVAL-C`) onto `DecimalEvalStack`.
///
/// # Arguments
///
/// * `call` - Lowered [`HirM12FunctionCall`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if argument view or decimal push emission fails.
pub(crate) fn try_emit_m12_call_decimal<'ctx, 'b, 'e: 'b>(
    call: &HirM12FunctionCall,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<bool> {
    let HirM12Callee::Intrinsic { abi_code } = call.callee else {
        return Ok(false);
    };
    let Ok(id) = IntrinsicId::from_abi_code(abi_code) else {
        return Ok(false);
    };
    let Some(first) = call.args.first() else {
        return Ok(false);
    };
    match id {
        IntrinsicId::Length | IntrinsicId::ByteLength => {
            let len_i64 = match first {
                HirM12Arg::RawStoragePlace(pl) => {
                    lower_place_view(pl, block, prog_ctx, state)?.len_i64
                }
                HirM12Arg::Expr(e) => lower_move_source_args(e, block, prog_ctx, state, loc)?.2,
                HirM12Arg::OdoLengthProbe {
                    base_bytes,
                    odo_place,
                    elem_stride,
                } => {
                    let odo_v = eval_expr_as_i64(
                        &HirExpr::Place(odo_place.clone()),
                        block,
                        prog_ctx,
                        state,
                    )?;
                    let stride_v = emit_i64(i64::from(*elem_stride), block, prog_ctx, state, loc);
                    let base_v = emit_i64(i64::from(*base_bytes), block, prog_ctx, state, loc);
                    let prod = append_val(
                        block,
                        arith::muli(odo_v, stride_v, loc),
                        prog_ctx.fallback_val,
                    );
                    append_val(block, arith::addi(base_v, prod, loc), prog_ctx.fallback_val)
                }
                _ => return Ok(false),
            };
            push_i64_to_decimal_stack(len_i64, block, prog_ctx, state, loc)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Lowers [`HirM12FunctionCall`] (`TRIM`) inside `lower_move_source_args` / `DISPLAY`.
///
/// # Arguments
///
/// * `call` - Lowered [`HirM12FunctionCall`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if `cobol.trim` emission fails.
pub(crate) fn try_lower_m12_call_move_source<'ctx, 'b, 'e: 'b>(
    call: &HirM12FunctionCall,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Option<LoweredMoveSourceArgs<'ctx, 'b>>> {
    let HirM12Callee::Intrinsic { abi_code } = call.callee else {
        return Ok(None);
    };
    let Ok(IntrinsicId::Trim) = IntrinsicId::from_abi_code(abi_code) else {
        return Ok(None);
    };
    let Some(first) = call.args.first() else {
        return Ok(None);
    };
    let (s_ptr, s_len) = match first {
        HirM12Arg::Expr(e) => eval_byte_slice_expr(e, block, prog_ctx, state, loc)?,
        HirM12Arg::RawStoragePlace(pl) => {
            let pv = lower_place_view(pl, block, prog_ctx, state)?;
            (pv.ptr, pv.len_i64)
        }
        _ => return Ok(None),
    };
    let dir = match call.args.get(1) {
        Some(HirM12Arg::Keyword(sym)) => {
            match state.interner.resolve_canonical(*sym).unwrap_or("BOTH") {
                "LEADING" => TrimDirection::Leading,
                "TRAILING" => TrimDirection::Trailing,
                _ => TrimDirection::Both,
            }
        }
        _ => TrimDirection::Both,
    };
    let trim_op = CobolTrimOp {
        src_ptr: s_ptr,
        src_len: s_len,
        direction: dir,
    }
    .build(state.ctx, loc)?;
    let (t_ptr, t_len) = append_val_pair(block, trim_op, prog_ctx.fallback_val);
    let (empty_ptr, zero_i64) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
    let k = emit_i32(
        RuntimeMoveSourceKind::AlphanumericLiteral.to_abi_code(),
        block,
        prog_ctx,
        state,
        loc,
    );
    Ok(Some((k, t_ptr, t_len, empty_ptr, zero_i64)))
}

/// Emits `DISPLAY` staging for `HirM12FunctionCall` (`HEX-OF` or `TRIM`), returning `true` if handled.
///
/// # Arguments
///
/// * `call` - Lowered [`HirM12FunctionCall`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if staging runtime calls fail.
pub(crate) fn try_emit_m12_display_hex_or_trim<'ctx, 'b, 'e: 'b>(
    call: &HirM12FunctionCall,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<bool> {
    if let HirM12Callee::Intrinsic { abi_code } = call.callee
        && let Ok(IntrinsicId::HexOf) = IntrinsicId::from_abi_code(abi_code)
        && let Some(first) = call.args.first()
    {
        let (p, l) = match first {
            HirM12Arg::RawStoragePlace(pl) => {
                let pv = lower_place_view(pl, block, prog_ctx, state)?;
                (pv.ptr, pv.len_i64)
            }
            HirM12Arg::Expr(e) => eval_byte_slice_expr(e, block, prog_ctx, state, loc)?,
            _ => return Ok(false),
        };
        let _ = emit_rt_call(
            RtDispatchTarget::DispPushHex,
            &[p, l],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok(true);
    }
    if let Some((_, p, l, _, _)) =
        try_lower_m12_call_move_source(call, block, prog_ctx, state, loc)?
    {
        let _ = emit_rt_call(
            RtDispatchTarget::DispPushRaw,
            &[p, l],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok(true);
    }
    Ok(false)
}

/// Pushes an `i64` SSA integer onto `DecimalEvalStack` via `@__coblang_idx_scratch`.
///
/// # Arguments
///
/// * `val_i64` - `i64` SSA value to push.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if emitting `DecPushField` fails.
fn push_i64_to_decimal_stack<'ctx, 'b, 'e: 'b>(
    val_i64: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let scratch_ptr =
        emit_storage_ptr(&prog_ctx.idx_scratch_sym, 0, 8, block, prog_ctx, state, loc)?;
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        val_i64,
        scratch_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let spec = make_runtime_field_spec(i64_binary_hir_type(), prog_ctx.prog.formatting);
    let (sp_ptr, sp_len) = emit_spec_slice(&spec, block, prog_ctx, state, loc)?;
    let eight = emit_i64(8, block, prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::DecPushField,
        &[scratch_ptr, eight, sp_ptr, sp_len],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Constructs an 8-byte signed native `BinaryInt` [`HirType`].
fn i64_binary_hir_type() -> HirType {
    HirType::BinaryInt {
        byte_width: 8,
        digits: DigitCount::try_new(18).unwrap_or(DigitCount::ONE),
        scale: ScaleFactor::ZERO,
        sign: Signedness::Signed,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::FullBinaryRange,
    }
}

/// Dispatches [`HirStmt::M20`] statements into `cobol` MLIR operations.
///
/// # Arguments
/// * `stmt` - Lowered [`HirM20Stmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
/// Returns [`coblang_common::CoblangError`] if MLIR emission fails.
pub(crate) fn lower_m20_stmt<'ctx, 'b, 'e: 'b>(
    stmt: &HirM20Stmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    match stmt {
        HirM20Stmt::XmlGenerate(spec) => lower_xml_gen_stmt(spec, block, prog_ctx, state),
        HirM20Stmt::XmlParse(spec) => lower_xml_parse_stmt(spec, block, prog_ctx, state),
        HirM20Stmt::JsonGenerate(spec) => lower_json_gen_stmt(spec, block, prog_ctx, state),
        HirM20Stmt::JsonParse(spec) => lower_json_parse_stmt(spec, block, prog_ctx, state),
        HirM20Stmt::OoInvoke(spec) => lower_oo_invoke_stmt(spec, block, prog_ctx, state),
        HirM20Stmt::OoBindSelfStorage { based_root, span } => {
            lower_oo_bind_self_storage(*based_root, *span, block, prog_ctx, state)
        }
        HirM20Stmt::OoSetRef(b) => {
            let (dst, src, span) = b.as_ref();
            let loc = state.loc(*span)?;
            let dv = lower_place_view(dst, block, prog_ctx, state)?;
            let hv = eval_obj_or_null_i64(src, block, prog_ctx, state, loc)?;
            let _ = block.append_operation(llvm::store(
                state.ctx.raw(),
                hv,
                dv.ptr,
                loc,
                llvm::LoadStoreOptions::new(),
            ));
            Ok(())
        }
    }
}

/// Lowers `HirM20Stmt::XmlGenerate`.
///
/// # Arguments
///
/// * `spec` - Lowered [`HirXmlGenerateStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR emission fails.
fn lower_xml_gen_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirXmlGenerateStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let dst_view = lower_place_view(&spec.destination, block, prog_ctx, state)?;
    let src_view = lower_place_view(&spec.source, block, prog_ctx, state)?;

    let (ns_ptr, ns_len) =
        eval_opt_slice_expr(spec.namespace.as_ref(), block, prog_ctx, state, loc)?;
    let (pfx_ptr, pfx_len) =
        eval_opt_slice_expr(spec.namespace_prefix.as_ref(), block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::M20SetNs,
        &[ns_ptr, ns_len, pfx_ptr, pfx_len],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    emit_optional_count_staging(spec.count_in.as_ref(), block, prog_ctx, state, loc)?;
    emit_subtree_odo_pushes(spec.source.item, block, prog_ctx, state, loc)?;

    let plan_bytes = encode_xml_gen_plan(spec, prog_ctx.prog, state.interner);
    let (plan_ptr, plan_len) = emit_const_slice(&plan_bytes, block, prog_ctx, state, loc)?;
    let exc = emit_rt_call(
        RtDispatchTarget::M20XmlGen,
        &[
            dst_view.ptr,
            dst_view.len_i64,
            src_view.ptr,
            plan_ptr,
            plan_len,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    emit_m20_exception_branch(
        exc,
        &spec.on_exception,
        &spec.not_on_exception,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Lowers `HirM20Stmt::JsonGenerate`.
///
/// # Arguments
///
/// * `spec` - Lowered [`HirJsonGenerateStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR emission fails.
fn lower_json_gen_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirJsonGenerateStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let dst_view = lower_place_view(&spec.destination, block, prog_ctx, state)?;
    let src_view = lower_place_view(&spec.source, block, prog_ctx, state)?;

    emit_optional_count_staging(spec.count_in.as_ref(), block, prog_ctx, state, loc)?;
    emit_subtree_odo_pushes(spec.source.item, block, prog_ctx, state, loc)?;

    let plan_bytes = encode_json_gen_plan(spec, prog_ctx.prog, state.interner);
    let (plan_ptr, plan_len) = emit_const_slice(&plan_bytes, block, prog_ctx, state, loc)?;
    let exc = emit_rt_call(
        RtDispatchTarget::M20JsonGen,
        &[
            dst_view.ptr,
            dst_view.len_i64,
            src_view.ptr,
            plan_ptr,
            plan_len,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    emit_m20_exception_branch(
        exc,
        &spec.on_exception,
        &spec.not_on_exception,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Lowers `HirM20Stmt::JsonParse`.
///
/// # Arguments
///
/// * `spec` - Lowered [`HirJsonParseStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR emission fails.
fn lower_json_parse_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirJsonParseStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let (src_ptr, src_len) = eval_byte_slice_expr(&spec.source, block, prog_ctx, state, loc)?;
    let dst_view = lower_place_view(&spec.destination, block, prog_ctx, state)?;
    emit_subtree_odo_pushes(spec.destination.item, block, prog_ctx, state, loc)?;

    let plan_bytes = encode_json_parse_plan(spec, prog_ctx.prog, state.interner);
    let (plan_ptr, plan_len) = emit_const_slice(&plan_bytes, block, prog_ctx, state, loc)?;
    let exc = emit_rt_call(
        RtDispatchTarget::M20JsonParse,
        &[src_ptr, src_len, dst_view.ptr, plan_ptr, plan_len],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    emit_m20_exception_branch(
        exc,
        &spec.on_exception,
        &spec.not_on_exception,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Lowers `HirM20Stmt::XmlParse` with streaming push-parser continuation (`END-OF-INPUT` + `XML-CODE = 1`).
///
/// # Arguments
///
/// * `spec` - Lowered [`HirXmlParseStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR emission fails.
fn lower_xml_parse_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirXmlParseStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let (fname, line_no) = resolve_file_and_line(spec.span, state);
    let (f_ptr, f_len) = emit_const_slice(fname.as_bytes(), block, prog_ctx, state, loc)?;
    let line_val = emit_i32(line_no, block, prog_ctx, state, loc);
    let mode_val = emit_i32(
        match spec.stream_mode {
            HirXmlStreamMode::Xmlss => 0,
            HirXmlStreamMode::Compat => 1,
        },
        block,
        prog_ctx,
        state,
        loc,
    );
    let sid = emit_rt_call(
        RtDispatchTarget::M20XmlParseBegin,
        &[f_ptr, f_len, line_val, mode_val],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let (init_ptr, init_len) = eval_byte_slice_expr(&spec.source, block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::M20XmlParseFeed,
        &[sid, init_ptr, init_len],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    let cond_region = Region::new();
    let cond_blk = cond_region.append_block(Block::new(&[]));
    let ready = emit_rt_call(
        RtDispatchTarget::M20XmlParseNext,
        &[sid],
        &cond_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let zero_32 = emit_i32(0, &cond_blk, prog_ctx, state, loc);
    let done_cmp = append_val(
        &cond_blk,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, ready, zero_32, loc),
        prog_ctx.fallback_val,
    );
    let _ = cond_blk.append_operation(
        CobolYieldOp {
            value: Some(done_cmp),
        }
        .build(state.ctx, loc)?,
    );

    let body_region = build_xml_parse_body_region(spec, sid, prog_ctx, state, loc)?;
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

    let exc = emit_rt_call(
        RtDispatchTarget::M20XmlParseEnd,
        &[sid],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    emit_m20_exception_branch(
        exc,
        &spec.on_exception,
        &spec.not_on_exception,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Builds the loop body region performing `PROCESSING PROCEDURE` and feeding continuation chunks.
///
/// # Arguments
///
/// * `spec` - Lowered [`HirXmlParseStmt`].
/// * `sid` - Active XML parser session handle SSA value.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
fn build_xml_parse_body_region<'ctx, 'b, 'e: 'b>(
    spec: &HirXmlParseStmt,
    sid: Value<'ctx, 'b>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Region<'ctx>> {
    let body_region = Region::new();
    let body_blk = body_region.append_block(Block::new(&[]));
    let pr_op = CobolPerformRangeOp {
        start_proc: spec.proc_start.as_u32(),
        end_proc: spec.proc_end.as_u32(),
    }
    .build(state.ctx, loc)?;
    let _ = body_blk.append_operation(pr_op);
    let post = emit_rt_call(
        RtDispatchTarget::M20XmlParseAfter,
        &[sid],
        &body_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let two_32 = emit_i32(2, &body_blk, prog_ctx, state, loc);
    let need_more = append_val(
        &body_blk,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, post, two_32, loc),
        prog_ctx.fallback_val,
    );
    let feed_then = Region::new();
    let feed_blk = feed_then.append_block(Block::new(&[]));
    let (next_ptr, next_len) = eval_byte_slice_expr(&spec.source, &feed_blk, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::M20XmlParseFeed,
        &[sid, next_ptr, next_len],
        &feed_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = feed_blk.append_operation(scf::r#yield(&[], loc));
    let feed_else = Region::new();
    let else_blk = feed_else.append_block(Block::new(&[]));
    let _ = else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = body_blk.append_operation(scf::r#if(need_more, &[], feed_then, feed_else, loc));
    let _ = body_blk.append_operation(CobolYieldOp { value: None }.build(state.ctx, loc)?);
    Ok(body_region)
}

/// Resolves `(display_filename, 1-based_line)` from `span` for runtime XML warnings.
///
/// # Arguments
///
/// * `span` - Statement source span.
/// * `state` - Module lowering state.
fn resolve_file_and_line(span: SourceSpan, state: &ModuleLowerState<'_, '_>) -> (String, i32) {
    if let Ok(loc) = state.source_map.resolve_span(span) {
        let fname = std::path::Path::new(&loc.file_path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&loc.file_path)
            .to_owned();
        let line = i32::try_from(loc.line.get()).unwrap_or(1);
        (fname, line)
    } else {
        (String::from("prog.cob"), 1)
    }
}

/// Emits `scf.if (exc != 0)` for `ON EXCEPTION` / `NOT ON EXCEPTION` handlers.
///
/// # Arguments
///
/// * `exc_i32` - Runtime exception status (`0` = success).
/// * `on_exc` - `ON EXCEPTION` handler statements.
/// * `not_on_exc` - `NOT ON EXCEPTION` handler statements.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if lowering handler statements fails.
fn emit_m20_exception_branch<'ctx, 'b, 'e: 'b>(
    exc_i32: Value<'ctx, 'b>,
    on_exc: &[HirStmt],
    not_on_exc: &[HirStmt],
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    if on_exc.is_empty() && not_on_exc.is_empty() {
        return Ok(());
    }
    let zero_32 = emit_i32(0, block, prog_ctx, state, loc);
    let is_exc = append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, exc_i32, zero_32, loc),
        prog_ctx.fallback_val,
    );
    let then_reg = Region::new();
    let then_blk = then_reg.append_block(Block::new(&[]));
    for st in on_exc {
        lower_stmt(st, &then_blk, prog_ctx, state)?;
    }
    let _ = then_blk.append_operation(scf::r#yield(&[], loc));

    let else_reg = Region::new();
    let else_blk = else_reg.append_block(Block::new(&[]));
    for st in not_on_exc {
        lower_stmt(st, &else_blk, prog_ctx, state)?;
    }
    let _ = else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(is_exc, &[], then_reg, else_reg, loc));
    Ok(())
}

/// Evaluates an expression as `(ptr, byte_len_i64)` in memory.
///
/// # Arguments
///
/// * `expr` - HIR expression to materialize as a byte slice.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if expression lowering fails.
fn eval_byte_slice_expr<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    match expr {
        HirExpr::Place(pl) => {
            let pv = lower_place_view(pl, block, prog_ctx, state)?;
            Ok((pv.ptr, pv.len_i64))
        }
        HirExpr::Const {
            value: HirConstValue::Bytes(b),
            ..
        } => emit_const_slice(b, block, prog_ctx, state, loc),
        other => {
            let (_, ptr, len, _, _) = lower_move_source_args(other, block, prog_ctx, state, loc)?;
            Ok((ptr, len))
        }
    }
}

/// Evaluates an `Option<&HirExpr>` into `(ptr, len)` where `None` yields `(null, -1)`.
///
/// # Arguments
///
/// * `opt` - Optional HIR expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if expression lowering fails.
fn eval_opt_slice_expr<'ctx, 'b, 'e: 'b>(
    opt: Option<&HirExpr>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    if let Some(e) = opt {
        eval_byte_slice_expr(e, block, prog_ctx, state, loc)
    } else {
        let null_p = emit_null_ptr(block, prog_ctx, state, loc);
        let neg1 = emit_i64(-1, block, prog_ctx, state, loc);
        Ok((null_p, neg1))
    }
}

/// Stages optional `COUNT IN` target via `M20SetCountDst`.
///
/// # Arguments
///
/// * `count_in` - Optional `COUNT IN` target place.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if place or runtime call emission fails.
fn emit_optional_count_staging<'ctx, 'b, 'e: 'b>(
    count_in: Option<&HirPlace>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    if let Some(cnt_pl) = count_in {
        let cv = lower_place_view(cnt_pl, block, prog_ctx, state)?;
        let (sp_ptr, sp_len) = emit_spec_slice(&cv.spec, block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::M20SetCountDst,
            &[cv.ptr, cv.len_i64, sp_ptr, sp_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    } else {
        let null_p = emit_null_ptr(block, prog_ctx, state, loc);
        let z = emit_i64(0, block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::M20SetCountDst,
            &[null_p, z, null_p, z],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Evaluates and pushes all dynamic ODO counts referenced under `root_item` via `M20PushOdo`.
///
/// # Arguments
///
/// * `root_item` - Root group/record [`DataItemId`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if evaluating an ODO count expression fails.
fn emit_subtree_odo_pushes<'ctx, 'b, 'e: 'b>(
    root_item: DataItemId,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let mut odo_ids = BTreeSet::new();
    collect_odo_ids_rec(root_item, prog_ctx.prog, &mut odo_ids);
    for oid in odo_ids {
        let Some(odo_item) = prog_ctx.prog.data_items.get(oid.as_u32() as usize) else {
            continue;
        };
        let odo_pl = HirPlace {
            item: oid,
            subscripts: Vec::new(),
            refmod: None,
            static_offset: odo_item.byte_offset,
            effective_ty: odo_item.ty.clone(),
            check_mask: coblang_hir::ExceptionCheckMask::NONE,
            span: odo_item.span,
        };
        let cnt_val = eval_expr_as_i64(&HirExpr::Place(odo_pl), block, prog_ctx, state)?;
        let id_i32 = emit_i32(
            i32::try_from(oid.as_u32()).unwrap_or(0),
            block,
            prog_ctx,
            state,
            loc,
        );
        let _ = emit_rt_call(
            RtDispatchTarget::M20PushOdo,
            &[id_i32, cnt_val],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Collects `DataItemId`s referenced by dynamic `OCCURS DEPENDING ON` dimensions under `item_id`.
///
/// # Arguments
///
/// * `item_id` - Subtree root [`DataItemId`].
/// * `prog` - Enclosing [`HirProgram`].
/// * `out` - Output set of ODO target item IDs.
fn collect_odo_ids_rec(item_id: DataItemId, prog: &HirProgram, out: &mut BTreeSet<DataItemId>) {
    let Some(item) = prog.data_items.get(item_id.as_u32() as usize) else {
        return;
    };
    for dim in &item.dimensions {
        if let Some(dep) = dim.depending_on {
            let _ = out.insert(dep);
        }
    }
    for child in &prog.data_items {
        if child.parent == Some(item_id) {
            collect_odo_ids_rec(child.id, prog, out);
        }
    }
}

/// Encodes binary plan for `XML GENERATE`.
///
/// # Arguments
///
/// * `spec` - Lowered [`HirXmlGenerateStmt`].
/// * `prog` - Enclosing [`HirProgram`].
/// * `interner` - Symbol interner for original field names.
fn encode_xml_gen_plan(
    spec: &HirXmlGenerateStmt,
    prog: &HirProgram,
    interner: &SymbolInterner,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut flags = 0_u8;
    if spec.xml_decl == HirXmlDeclMode::Include {
        flags |= 1;
    }
    if spec.attr_mode == HirXmlAttrMode::WithAttributes {
        flags |= 2;
    }
    if spec.dpc_comma == HirDecimalCommaMode::Comma {
        flags |= 4;
    }
    out.push(flags);
    let src_total = prog
        .data_items
        .get(spec.source.item.as_u32() as usize)
        .map_or(1, |i| i.total_byte_size.max(1));
    push_u32(&mut out, src_total);
    encode_common_gen_rules(
        &spec.name_overrides,
        &spec.field_suppress,
        &spec.generic_suppress,
        &mut out,
    );
    push_u32(
        &mut out,
        u32::try_from(spec.type_overrides.len()).unwrap_or(0),
    );
    for &(fid, nt) in &spec.type_overrides {
        push_u32(&mut out, fid.as_u32());
        out.push(match nt {
            HirXmlNodeType::Attribute => 0,
            HirXmlNodeType::Element => 1,
            HirXmlNodeType::Content => 2,
        });
    }
    encode_schema_node(spec.source.item, 0, 0, true, prog, interner, &mut out);
    out
}

/// Encodes binary plan for `JSON GENERATE`.
///
/// # Arguments
///
/// * `spec` - Lowered [`HirJsonGenerateStmt`].
/// * `prog` - Enclosing [`HirProgram`].
/// * `interner` - Symbol interner for original field names.
fn encode_json_gen_plan(
    spec: &HirJsonGenerateStmt,
    prog: &HirProgram,
    interner: &SymbolInterner,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(u8::from(spec.dpc_comma == HirDecimalCommaMode::Comma));
    let src_total = prog
        .data_items
        .get(spec.source.item.as_u32() as usize)
        .map_or(1, |i| i.total_byte_size.max(1));
    push_u32(&mut out, src_total);
    encode_common_gen_rules(
        &spec.name_overrides,
        &spec.field_suppress,
        &spec.generic_suppress,
        &mut out,
    );
    push_u32(&mut out, u32::try_from(spec.bool_fields.len()).unwrap_or(0));
    for (fid, lit) in &spec.bool_fields {
        push_u32(&mut out, fid.as_u32());
        push_str16(&mut out, lit);
    }
    encode_schema_node(spec.source.item, 0, 0, true, prog, interner, &mut out);
    out
}

/// Encodes binary plan for `JSON PARSE`.
///
/// # Arguments
///
/// * `spec` - Lowered [`HirJsonParseStmt`].
/// * `prog` - Enclosing [`HirProgram`].
/// * `interner` - Symbol interner for original field names.
fn encode_json_parse_plan(
    spec: &HirJsonParseStmt,
    prog: &HirProgram,
    interner: &SymbolInterner,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(u8::from(spec.detail == HirJsonDetailMode::WithDetail));
    let dst_total = prog
        .data_items
        .get(spec.destination.item.as_u32() as usize)
        .map_or(1, |i| i.total_byte_size.max(1));
    push_u32(&mut out, dst_total);

    push_u32(
        &mut out,
        u32::try_from(spec.name_overrides.len()).unwrap_or(0),
    );
    for (fid, ov) in &spec.name_overrides {
        push_str16(&mut out, &item_orig_name(*fid, prog, interner));
        match ov {
            HirMarkupNameOverride::Omitted => {
                out.push(0);
                push_str16(&mut out, "");
            }
            HirMarkupNameOverride::Rename(s) => {
                out.push(1);
                push_str16(&mut out, s);
            }
        }
    }
    push_u32(
        &mut out,
        u32::try_from(spec.suppressed_fields.len()).unwrap_or(0),
    );
    for &fid in &spec.suppressed_fields {
        push_str16(&mut out, &item_orig_name(fid, prog, interner));
    }
    push_u32(&mut out, u32::try_from(spec.bool_fields.len()).unwrap_or(0));
    for (fid, t_lit, f_lit) in &spec.bool_fields {
        push_str16(&mut out, &item_orig_name(*fid, prog, interner));
        push_str16(&mut out, t_lit);
        push_str16(&mut out, f_lit);
    }
    encode_schema_node(spec.destination.item, 0, 0, true, prog, interner, &mut out);
    out
}

/// Encodes shared `(name_overrides, field_suppress, generic_suppress)` sections.
///
/// # Arguments
///
/// * `name_overrides` - Per-field rename or `OMITTED` tag rules.
/// * `field_suppress` - Per-field conditional suppression masks.
/// * `generic_suppress` - Category-wide generic `SUPPRESS` rules.
/// * `out` - Output binary plan buffer.
fn encode_common_gen_rules(
    name_overrides: &[(DataItemId, HirMarkupNameOverride)],
    field_suppress: &[(DataItemId, u8)],
    generic_suppress: &[HirGenericSuppressRule],
    out: &mut Vec<u8>,
) {
    push_u32(out, u32::try_from(name_overrides.len()).unwrap_or(0));
    for (fid, ov) in name_overrides {
        push_u32(out, fid.as_u32());
        match ov {
            HirMarkupNameOverride::Omitted => {
                out.push(0);
                push_str16(out, "");
            }
            HirMarkupNameOverride::Rename(s) => {
                out.push(1);
                push_str16(out, s);
            }
        }
    }
    push_u32(out, u32::try_from(field_suppress.len()).unwrap_or(0));
    for &(fid, mask) in field_suppress {
        push_u32(out, fid.as_u32());
        out.push(mask);
    }
    push_u32(out, u32::try_from(generic_suppress.len()).unwrap_or(0));
    for g in generic_suppress {
        out.push(g.category_code);
        out.push(g.placement_code);
        out.push(g.cond_mask);
    }
}

/// Encodes a recursive `SchemaNode` for `item_id` into `out`.
///
/// # Arguments
///
/// * `item_id` - Current [`DataItemId`].
/// * `parent_abs_off` - Enclosing parent item's absolute byte offset.
/// * `parent_dims_len` - Number of `OCCURS` dimensions inherited from ancestors.
/// * `is_root` - `true` when `item_id` is the root operand of `XML`/`JSON`.
/// * `prog` - Enclosing [`HirProgram`].
/// * `interner` - Symbol interner.
/// * `out` - Output binary plan buffer.
fn encode_schema_node(
    item_id: DataItemId,
    parent_abs_off: u32,
    parent_dims_len: usize,
    is_root: bool,
    prog: &HirProgram,
    interner: &SymbolInterner,
    out: &mut Vec<u8>,
) {
    let Some(item) = prog.data_items.get(item_id.as_u32() as usize) else {
        return;
    };
    let name = item_orig_name(item_id, prog, interner);
    let abs_off = item.byte_offset.as_u32();
    let rel_off = if is_root {
        0
    } else {
        abs_off.saturating_sub(parent_abs_off)
    };
    let cur_dims_len = item.dimensions.len();
    let own_dim = if !is_root && cur_dims_len > parent_dims_len {
        item.dimensions.last()
    } else {
        None
    };
    let (max_occurs, odo_i32, elem_len) = match own_dim {
        None => (0_u32, -1_i32, item.total_byte_size.max(1)),
        Some(d) => {
            let dep_i32 = d
                .depending_on
                .and_then(|oid| i32::try_from(oid.as_u32()).ok())
                .unwrap_or(-1);
            (d.max, dep_i32, d.element_byte_stride.max(1))
        }
    };

    push_u32(out, item_id.as_u32());
    push_str16(out, &name);
    push_u32(out, rel_off);
    push_u32(out, elem_len);
    push_u32(out, max_occurs);
    out.extend_from_slice(&odo_i32.to_le_bytes());

    let children: Vec<&HirDataItem> = prog
        .data_items
        .iter()
        .filter(|ch| {
            ch.parent == Some(item_id)
                && ch.redefines.is_none()
                && !matches!(
                    ch.level,
                    LevelNumber::Renames66 | LevelNumber::Condition88 | LevelNumber::Constant78
                )
        })
        .collect();

    if matches!(item.ty, HirType::Group { .. }) && !children.is_empty() {
        out.push(1);
        push_u32(out, u32::try_from(children.len()).unwrap_or(0));
        for ch in children {
            encode_schema_node(ch.id, abs_off, cur_dims_len, false, prog, interner, out);
        }
    } else {
        out.push(0);
        let cat_tag = classify_leaf_cat_tag(&item.ty);
        out.push(cat_tag);
        let spec = make_runtime_field_spec(item.ty.clone(), prog.formatting);
        let spec_bytes = encode_field_spec(&spec);
        push_u32(out, u32::try_from(spec_bytes.len()).unwrap_or(0));
        out.extend_from_slice(&spec_bytes);
    }
}

/// Maps elementary [`HirType`] to schema `cat_tag` (`0` = `Alpha`, `1` = `AlphaRight`, `2` = `Numeric`, `3` = `NumericEdited`).
///
/// # Arguments
///
/// * `ty` - Elementary [`HirType`] to classify.
const fn classify_leaf_cat_tag(ty: &HirType) -> u8 {
    match ty {
        HirType::Alphanumeric {
            justified: JustifiedSpec::Right,
            ..
        }
        | HirType::Alphabetic {
            justified: JustifiedSpec::Right,
            ..
        } => 1,
        HirType::ZonedDecimal { .. }
        | HirType::PackedDecimal { .. }
        | HirType::BinaryInt { .. }
        | HirType::FloatBinary { .. }
        | HirType::FloatDecimal { .. }
        | HirType::Index => 2,
        HirType::NumericEdited { .. } => 3,
        _ => 0,
    }
}

/// Resolves original field name (`"FILLER"` when unnamed).
///
/// # Arguments
///
/// * `item_id` - Target [`DataItemId`].
/// * `prog` - Enclosing [`HirProgram`].
/// * `interner` - Symbol interner.
fn item_orig_name(item_id: DataItemId, prog: &HirProgram, interner: &SymbolInterner) -> String {
    prog.data_items
        .get(item_id.as_u32() as usize)
        .and_then(|i| i.name)
        .and_then(|s| interner.resolve_original(s).ok())
        .filter(|s| !s.is_empty())
        .unwrap_or("FILLER")
        .to_owned()
}

/// Appends little-endian `u32`.
///
/// # Arguments
///
/// * `out` - Output byte buffer.
/// * `v` - `u32` value to append.
fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Appends `u16`-length-prefixed UTF-8 string.
///
/// # Arguments
///
/// * `out` - Output byte buffer.
/// * `s` - UTF-8 string slice to append.
fn push_str16(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    if let Some(sub) = bytes.get(..usize::from(len)) {
        out.extend_from_slice(sub);
    }
}

/// Binds an `OBJECT` method's synthesized `BasedStorage` `01 __OO_INSTANCE_SELF` root to `M20OoSelfDataPtr`.
///
/// # Arguments
///
/// * `based_root` - Synthesized `BASED` root [`DataItemId`].
/// * `span` - Method span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if emitting `M20OoSelfDataPtr` fails.
fn lower_oo_bind_self_storage<'ctx, 'b, 'e: 'b>(
    based_root: DataItemId,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let Some(Some(item_loc)) = prog_ctx.storage.items.get(based_root.as_u32() as usize) else {
        return Ok(());
    };
    let Some(root_entry) = prog_ctx
        .storage
        .roots
        .iter()
        .find(|r| r.root_id == item_loc.root_id)
    else {
        return Ok(());
    };
    let self_ptr = emit_rt_call(
        RtDispatchTarget::M20OoSelfDataPtr,
        &[],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let slot_ptr = emit_storage_ptr(&root_entry.sym_name, 0, 8, block, prog_ctx, state, loc)?;
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        self_ptr,
        slot_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    Ok(())
}

/// Lowers `HirM20Stmt::OoInvoke`.
///
/// # Arguments
///
/// * `spec` - Lowered [`HirOoInvokeStmt`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if lowering the OO invocation fails.
fn lower_oo_invoke_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirOoInvokeStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let ret_pair = if let Some(rpl) = &spec.returning {
        let rv = lower_place_view(rpl, block, prog_ctx, state)?;
        Some((rv.ptr, rv.len_i64))
    } else {
        None
    };
    emit_oo_invoke_common(
        (&spec.receiver, &spec.method_name, &spec.args),
        ret_pair,
        block,
        prog_ctx,
        state,
        loc,
    )
}

/// Shared OO invocation emitter for both `INVOKE` statements and inline `::` expressions.
///
/// # Arguments
///
/// * `call_target` - `(receiver, method_name, args)` invocation triple.
/// * `ret_pair` - Optional `(ret_ptr, ret_len)` return buffer.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if argument or dispatch emission fails.
fn emit_oo_invoke_common<'ctx, 'b, 'e: 'b>(
    call_target: (&HirOoReceiver, &HirExpr, &[HirCallArg]),
    ret_pair: Option<(Value<'ctx, 'b>, Value<'ctx, 'b>)>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let (receiver, method_name, args) = call_target;
    let (ret_ptr, ret_len) = match ret_pair {
        Some(pair) => pair,
        None => (
            emit_null_ptr(block, prog_ctx, state, loc),
            emit_i64(0, block, prog_ctx, state, loc),
        ),
    };
    let _ = emit_rt_call(
        RtDispatchTarget::M20OoBeginInvoke,
        &[ret_ptr, ret_len],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    for arg in args {
        emit_one_oo_arg(arg, block, prog_ctx, state, loc)?;
    }

    let (recv_kind, recv_handle, cls_bytes) = match receiver {
        HirOoReceiver::FactoryClass(ext) => (
            0_i32,
            emit_i64(0, block, prog_ctx, state, loc),
            ext.as_bytes(),
        ),
        HirOoReceiver::SelfRef => {
            let h = emit_rt_call(
                RtDispatchTarget::M20OoSelfHandle,
                &[],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            (1_i32, h, &[][..])
        }
        HirOoReceiver::SuperRef { parent_external } => {
            let h = emit_rt_call(
                RtDispatchTarget::M20OoSelfHandle,
                &[],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            (2_i32, h, parent_external.as_bytes())
        }
        HirOoReceiver::ObjectExpr(e) => {
            let h = eval_object_handle_expr(e, block, prog_ctx, state, loc)?;
            (1_i32, h, &[][..])
        }
    };
    let rk_val = emit_i32(recv_kind, block, prog_ctx, state, loc);
    let (c_ptr, c_len) = emit_const_slice(cls_bytes, block, prog_ctx, state, loc)?;
    let (m_ptr, m_len) = eval_byte_slice_expr(method_name, block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::M20OoInvoke,
        &[rk_val, recv_handle, c_ptr, c_len, m_ptr, m_len],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Evaluates an 8-byte `OBJECT REFERENCE` expression to an `i64` handle.
///
/// # Arguments
///
/// * `expr` - Object handle expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if expression lowering fails.
fn eval_object_handle_expr<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    match expr {
        HirExpr::Place(pl) => {
            let pv = lower_place_view(pl, block, prog_ctx, state)?;
            let i64_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 64).into();
            Ok(append_val(
                block,
                llvm::load(
                    state.ctx.raw(),
                    pv.ptr,
                    i64_ty,
                    loc,
                    llvm::LoadStoreOptions::new(),
                ),
                prog_ctx.fallback_val,
            ))
        }
        HirExpr::M20(m) => match m.as_ref() {
            HirM20Expr::OoSelf { .. } => emit_rt_call(
                RtDispatchTarget::M20OoSelfHandle,
                &[],
                block,
                prog_ctx,
                state,
                loc,
            ),
            HirM20Expr::OoInlineInvoke(inv) => {
                let (buf_ptr, _, _) = eval_inline_invoke_to_buf(inv, block, prog_ctx, state, loc)?;
                let i64_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 64).into();
                Ok(append_val(
                    block,
                    llvm::load(
                        state.ctx.raw(),
                        buf_ptr,
                        i64_ty,
                        loc,
                        llvm::LoadStoreOptions::new(),
                    ),
                    prog_ctx.fallback_val,
                ))
            }
        },
        other => eval_expr_as_i64(other, block, prog_ctx, state),
    }
}

/// Pushes one [`HirCallArg`] via `M20OoPushArg`.
///
/// # Arguments
///
/// * `arg` - Call argument specification.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if materializing or pushing the argument fails.
fn emit_one_oo_arg<'ctx, 'b, 'e: 'b>(
    arg: &HirCallArg,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    match arg {
        HirCallArg::Omitted => {
            let null_p = emit_null_ptr(block, prog_ctx, state, loc);
            let z = emit_i64(0, block, prog_ctx, state, loc);
            let _ = emit_rt_call(
                RtDispatchTarget::M20OoPushArg,
                &[null_p, z],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirCallArg::ByReference(pl) => {
            let pv = lower_place_view(pl, block, prog_ctx, state)?;
            let _ = emit_rt_call(
                RtDispatchTarget::M20OoPushArg,
                &[pv.ptr, pv.len_i64],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirCallArg::ByContent(e) | HirCallArg::ByValue(e) => {
            let (ptr, len) = materialize_temp_arg_copy(e, block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::M20OoPushArg,
                &[ptr, len],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
    }
    Ok(())
}

/// Copies a `BY CONTENT` / `BY VALUE` argument expression into a dedicated module scratch slot.
///
/// # Arguments
///
/// * `expr` - Argument expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if scratch allocation or evaluation fails.
fn materialize_temp_arg_copy<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    if let HirExpr::Const {
        value: HirConstValue::Bytes(b),
        ..
    } = expr
    {
        let sz = u32::try_from(b.len().max(1)).unwrap_or(u32::MAX);
        let sym = state.alloc_m20_scratch_global(sz, loc)?;
        let dst_ptr = emit_storage_ptr(&sym, 0, sz, block, prog_ctx, state, loc)?;
        let (src_ptr, src_len) = emit_const_slice(b, block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::RawCopy,
            &[dst_ptr, src_ptr, src_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok((dst_ptr, src_len));
    }
    let val = eval_expr_as_i64(expr, block, prog_ctx, state)?;
    let sym = state.alloc_m20_scratch_global(8, loc)?;
    let dst_ptr = emit_storage_ptr(&sym, 0, 8, block, prog_ctx, state, loc)?;
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        val,
        dst_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let eight = emit_i64(8, block, prog_ctx, state, loc);
    Ok((dst_ptr, eight))
}

/// Evaluates [`HirOoInlineInvokeExpr`] into a dedicated static return buffer `(ptr, len_i64, HirType)`.
///
/// # Arguments
///
/// * `inv` - Inline `::` invocation expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if scratch allocation or invocation fails.
pub(crate) fn eval_inline_invoke_to_buf<'ctx, 'b, 'e: 'b>(
    inv: &HirOoInlineInvokeExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>, HirType)> {
    let exact_sz = inv.return_ty.static_byte_size().max(1);
    let alloc_sz = exact_sz.max(8);
    let sym = state.alloc_m20_scratch_global(alloc_sz, loc)?;
    let buf_ptr = emit_storage_ptr(&sym, 0, alloc_sz, block, prog_ctx, state, loc)?;
    let ret_len = emit_i64(i64::from(exact_sz), block, prog_ctx, state, loc);
    emit_oo_invoke_common(
        (&inv.receiver, &inv.method_name, &inv.args),
        Some((buf_ptr, ret_len)),
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok((buf_ptr, ret_len, inv.return_ty.clone()))
}

/// Lowers [`HirM20Expr`] inside `lower_move_source_args`.
///
/// # Arguments
///
/// * `mexpr` - Lowered [`HirM20Expr`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if expression evaluation fails.
pub(crate) fn lower_m20_move_source<'ctx, 'b, 'e: 'b>(
    mexpr: &HirM20Expr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<LoweredMoveSourceArgs<'ctx, 'b>> {
    let (ptr, len_i64, ty) = match mexpr {
        HirM20Expr::OoSelf { .. } => {
            let h = emit_rt_call(
                RtDispatchTarget::M20OoSelfHandle,
                &[],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let sym = state.alloc_m20_scratch_global(8, loc)?;
            let bp = emit_storage_ptr(&sym, 0, 8, block, prog_ctx, state, loc)?;
            let _ = block.append_operation(llvm::store(
                state.ctx.raw(),
                h,
                bp,
                loc,
                llvm::LoadStoreOptions::new(),
            ));
            let eight = emit_i64(8, block, prog_ctx, state, loc);
            (bp, eight, HirType::ObjectRef { class_sym: None })
        }
        HirM20Expr::OoInlineInvoke(inv) => {
            eval_inline_invoke_to_buf(inv, block, prog_ctx, state, loc)?
        }
    };
    let spec = make_runtime_field_spec(ty, prog_ctx.prog.formatting);
    let (sp_ptr, sp_len) = emit_spec_slice(&spec, block, prog_ctx, state, loc)?;
    let kind = emit_i32(
        RuntimeMoveSourceKind::Field.to_abi_code(),
        block,
        prog_ctx,
        state,
        loc,
    );
    Ok((kind, ptr, len_i64, sp_ptr, sp_len))
}

/// Lowers [`HirM20Expr`] onto `DecimalEvalStack` inside `emit_decimal_expr`.
///
/// # Arguments
///
/// * `mexpr` - Lowered [`HirM20Expr`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if expression evaluation fails.
pub(crate) fn emit_m20_decimal_expr<'ctx, 'b, 'e: 'b>(
    mexpr: &HirM20Expr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    match mexpr {
        HirM20Expr::OoSelf { .. } => {
            let h = emit_rt_call(
                RtDispatchTarget::M20OoSelfHandle,
                &[],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            push_i64_to_decimal_stack(h, block, prog_ctx, state, loc)
        }
        HirM20Expr::OoInlineInvoke(inv) => {
            let (ptr, len_i64, ty) = eval_inline_invoke_to_buf(inv, block, prog_ctx, state, loc)?;
            let spec = make_runtime_field_spec(ty, prog_ctx.prog.formatting);
            let (sp_ptr, sp_len) = emit_spec_slice(&spec, block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::DecPushField,
                &[ptr, len_i64, sp_ptr, sp_len],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            Ok(())
        }
    }
}

/// Lowers [`HirM20Expr`] inside `emit_display_operand`.
///
/// # Arguments
///
/// * `mexpr` - Lowered [`HirM20Expr`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if expression evaluation fails.
pub(crate) fn emit_m20_display_operand<'ctx, 'b, 'e: 'b>(
    mexpr: &HirM20Expr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let (_, ptr, len_i64, sp_ptr, sp_len) =
        lower_m20_move_source(mexpr, block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::DispPushField,
        &[ptr, len_i64, sp_ptr, sp_len],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Prepared registration entry for one OO class and its compiled method logical names.
pub(crate) struct PreparedOoClassReg {
    /// Class runtime metadata.
    pub spec: HirOoClassSpec,
    /// Initial instance template bytes.
    pub init_bytes: Vec<u8>,
    /// `(compartment_code, method_name, logical_entry_name)` triples.
    pub methods: Vec<(i32, String, String)>,
}

/// Prepares `PreparedOoClassReg` records and logical method names for all `HirClass` units in `classes`.
///
/// # Arguments
///
/// * `classes` - Lowered [`HirClass`] units in the compilation group.
/// * `source_map` - Source map for diagnostics.
/// * `interner` - Symbol interner.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if computing initial template bytes fails.
pub(crate) fn prepare_oo_class_registrations(
    classes: &[HirClass],
    source_map: &coblang_diag::SourceMap,
    interner: &SymbolInterner,
) -> Result<Vec<PreparedOoClassReg>> {
    let mut out = Vec::with_capacity(classes.len());
    for (cls_idx, cls) in classes.iter().enumerate() {
        let Some(spec) = &cls.oo_spec else {
            continue;
        };
        let init_bytes = compute_oo_template_bytes(spec, cls.span, source_map, interner)?;
        let mut methods = Vec::with_capacity(spec.method_specs.len());
        for ms in &spec.method_specs {
            let comp = match ms.compartment {
                HirOoCompartment::Factory => 0,
                HirOoCompartment::Object => 1,
            };
            let logical = format!("__OO_M_{cls_idx}_{}", ms.method_index);
            let canon = mangle_program_symbol(&logical, SymbolMangleStyle::CobolCanonical);
            let addr_sym = format!("__func_addr:{canon}");
            methods.push((comp, ms.method_name.clone(), addr_sym));
        }
        out.push(PreparedOoClassReg {
            spec: spec.clone(),
            init_bytes,
            methods,
        });
    }
    Ok(out)
}

/// Computes initial `VALUE` bytes for an OO class's `instance_template_items`.
///
/// # Arguments
///
/// * `spec` - Lowered [`HirOoClassSpec`].
/// * `span` - Class definition span.
/// * `source_map` - Source map.
/// * `interner` - Symbol interner.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if building the storage template plan fails.
fn compute_oo_template_bytes(
    spec: &HirOoClassSpec,
    span: SourceSpan,
    source_map: &coblang_diag::SourceMap,
    interner: &SymbolInterner,
) -> Result<Vec<u8>> {
    let size = usize::try_from(spec.instance_byte_size.max(8)).unwrap_or(8);
    if spec.instance_template_items.is_empty() {
        return Ok(vec![0_u8; size]);
    }
    let dummy_prog = HirProgram {
        id: coblang_common::ProgramUnitId::new(0),
        scope: coblang_common::ScopeId::new(0),
        name: coblang_common::SymbolId::new(0),
        formatting: coblang_hir::HirProgramFormatting::default(),
        perform_style: coblang_hir::HirPerformStyle::IsoStack,
        odo_slide: coblang_hir::HirOdoSlideMode::FixedOffsets,
        recursivity: coblang_hir::HirRecursivity::NonRecursive,
        initial_state: coblang_hir::HirInitialState::RetainStorage,
        alphabets: Vec::new(),
        collating_sequence: None,
        custom_classes: Vec::new(),
        switches: Vec::new(),
        files: Vec::new(),
        data_items: spec.instance_template_items.clone(),
        conditions_88: Vec::new(),
        reports: Vec::new(),
        report_plans: Vec::new(),
        screen_plans: Vec::new(),
        screen_special_regs: coblang_hir::HirScreenSpecialRegs::default(),
        using_params: Vec::new(),
        returning_item: None,
        entry_statements: Vec::new(),
        procedures: Vec::new(),
        fallthrough_limit_pid: None,
        declaratives: Vec::new(),
        nested: Vec::new(),
        span,
    };
    let plan = build_program_storage_plan(&dummy_prog, "__oo_tmpl", source_map, interner)?;
    for root in plan.roots.into_iter().rev() {
        let is_ws = dummy_prog
            .data_items
            .get(root.root_id.as_u32() as usize)
            .is_some_and(|it| it.section == HirStorageSection::WorkingStorage);
        if is_ws
            && let RootInitStrategy::StaticImage(mut bytes) = root.init
            && !bytes.is_empty()
        {
            if bytes.len() < size {
                bytes.resize(size, b' ');
            }
            return Ok(bytes);
        }
    }
    Ok(vec![b' '; size])
}

/// Lowers `OBJECT REFERENCE` / `SELF` / `NULL` relational comparisons (`lhs <op> rhs`) into `i1`.
///
/// # Arguments
///
/// * `op` - Relational operator.
/// * `lhs` - Left operand expression.
/// * `rhs` - Right operand expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if handle evaluation fails.
pub(crate) fn try_lower_object_ref_compare<'ctx, 'b, 'e: 'b>(
    op: HirRelOp,
    lhs: &HirExpr,
    rhs: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Option<Value<'ctx, 'b>>> {
    if !is_obj_ref_expr(lhs) && !is_obj_ref_expr(rhs) {
        return Ok(None);
    }
    let lv = eval_obj_or_null_i64(lhs, block, prog_ctx, state, loc)?;
    let rv = eval_obj_or_null_i64(rhs, block, prog_ctx, state, loc)?;
    let pred = match op {
        HirRelOp::Eq => CmpiPredicate::Eq,
        HirRelOp::Ne => CmpiPredicate::Ne,
        HirRelOp::Lt => CmpiPredicate::Slt,
        HirRelOp::Le => CmpiPredicate::Sle,
        HirRelOp::Gt => CmpiPredicate::Sgt,
        HirRelOp::Ge => CmpiPredicate::Sge,
    };
    Ok(Some(append_val(
        block,
        arith::cmpi(state.ctx.raw(), pred, lv, rv, loc),
        prog_ctx.fallback_val,
    )))
}

/// Returns `true` when `e` produces an 8-byte `OBJECT REFERENCE` handle.
///
/// # Arguments
///
/// * `e` - Candidate HIR expression.
fn is_obj_ref_expr(e: &HirExpr) -> bool {
    match e {
        HirExpr::Place(pl) => matches!(pl.effective_ty, HirType::ObjectRef { .. }),
        HirExpr::M20(m) => match m.as_ref() {
            HirM20Expr::OoSelf { .. } => true,
            HirM20Expr::OoInlineInvoke(inv) => matches!(inv.return_ty, HirType::ObjectRef { .. }),
        },
        _ => false,
    }
}

/// Evaluates an `OBJECT REFERENCE` or `NULL` operand to an `i64` handle (`0` for `NULL`).
///
/// # Arguments
///
/// * `e` - Object reference or `NULL` expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if handle evaluation fails.
fn eval_obj_or_null_i64<'ctx, 'b, 'e: 'b>(
    e: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    if matches!(
        e,
        HirExpr::Const {
            value: HirConstValue::Figurative(HirFigurative::Null | HirFigurative::Zero),
            ..
        }
    ) {
        return Ok(emit_i64(0, block, prog_ctx, state, loc));
    }
    eval_object_handle_expr(e, block, prog_ctx, state, loc)
}

/// Emits the `@__coblang_m20_oo_guard` global when `oo_regs` is non-empty.
///
/// # Arguments
///
/// * `oo_regs` - Prepared OO class registration descriptors.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if `cobol.storage` op creation fails.
pub(crate) fn emit_oo_guard_global(
    oo_regs: &[PreparedOoClassReg],
    state: &mut ModuleLowerState<'_, '_>,
) -> Result<()> {
    if oo_regs.is_empty() {
        return Ok(());
    }
    let op = CobolStorageOp {
        sym_name: OO_REG_GUARD_SYM,
        storage_kind: StorageKind::Working,
        byte_size: 1,
        alignment: 1,
        init_bytes: &[0],
    }
    .build(state.ctx, state.ctx.unknown_location())?;
    let _ = state.module.body().append_operation(op);
    Ok(())
}

/// Emits one-time startup registration of all OO classes and methods at `ProgramEntry`.
///
/// # Arguments
///
/// * `oo_regs` - Prepared OO class registration descriptors.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if emitting registration calls fails.
pub(crate) fn emit_m20_oo_startup<'ctx, 'b, 'e: 'b>(
    oo_regs: &[PreparedOoClassReg],
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    if oo_regs.is_empty() {
        return Ok(());
    }
    let raw = state.ctx.raw();
    let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();
    let g_ptr = emit_storage_ptr(OO_REG_GUARD_SYM, 0, 1, block, prog_ctx, state, loc)?;
    let flag = append_val(
        block,
        llvm::load(raw, g_ptr, i8_ty, loc, llvm::LoadStoreOptions::new()),
        prog_ctx.fallback_val,
    );
    let zero_8 = append_val(
        block,
        arith::constant(raw, IntegerAttribute::new(i8_ty, 0).into(), loc),
        prog_ctx.fallback_val,
    );
    let need_init = append_val(
        block,
        arith::cmpi(raw, CmpiPredicate::Eq, flag, zero_8, loc),
        prog_ctx.fallback_val,
    );

    let then_reg = Region::new();
    let then_blk = then_reg.append_block(Block::new(&[]));
    let one_8 = append_val(
        &then_blk,
        arith::constant(raw, IntegerAttribute::new(i8_ty, 1).into(), loc),
        prog_ctx.fallback_val,
    );
    let _ = then_blk.append_operation(llvm::store(
        raw,
        one_8,
        g_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));

    for reg in oo_regs {
        let (c_ptr, c_len) = emit_const_slice(
            reg.spec.external_name.as_bytes(),
            &then_blk,
            prog_ctx,
            state,
            loc,
        )?;
        let parents_joined = reg.spec.parent_externals.join("\0");
        let (p_ptr, p_len) =
            emit_const_slice(parents_joined.as_bytes(), &then_blk, prog_ctx, state, loc)?;
        let (i_ptr, i_len) = emit_const_slice(&reg.init_bytes, &then_blk, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::M20OoRegClass,
            &[c_ptr, c_len, p_ptr, p_len, i_ptr, i_len],
            &then_blk,
            prog_ctx,
            state,
            loc,
        )?;
        for (comp, mname, addr_sym) in &reg.methods {
            let fn_ptr = emit_storage_ptr(addr_sym, 0, 1, &then_blk, prog_ctx, state, loc)?;
            let comp_v = emit_i32(*comp, &then_blk, prog_ctx, state, loc);
            let (m_ptr, m_len) =
                emit_const_slice(mname.as_bytes(), &then_blk, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::M20OoRegMethod,
                &[c_ptr, c_len, comp_v, m_ptr, m_len, fn_ptr],
                &then_blk,
                prog_ctx,
                state,
                loc,
            )?;
        }
    }
    let _ = then_blk.append_operation(scf::r#yield(&[], loc));
    let else_reg = Region::new();
    let else_blk = else_reg.append_block(Block::new(&[]));
    let _ = else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(need_init, &[], then_reg, else_reg, loc));
    Ok(())
}
