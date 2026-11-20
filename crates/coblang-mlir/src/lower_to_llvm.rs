//! Lowering pass from `VerifiedCobolMlirModule` to `LoweredLlvmMlirModule` (`llvm` MLIR 20 dialect).

use coblang_common::{CoblangError, OptimizationLevel, Result};
use melior::dialect::arith::CmpiPredicate;
use melior::dialect::llvm::attributes::{Linkage, linkage};
use melior::dialect::llvm::{AllocaOptions, LoadStoreOptions};
use melior::dialect::{arith, cf, func, llvm};
use melior::ir::attribute::{
    BoolAttribute, DenseElementsAttribute, DenseI32ArrayAttribute, FlatSymbolRefAttribute,
    IntegerAttribute, StringAttribute, TypeAttribute,
};
use melior::ir::block::BlockLike;
use melior::ir::operation::{OperationBuilder, OperationLike};
use melior::ir::r#type::{FunctionType, IntegerType, RankedTensorType};
use melior::ir::{
    Attribute, Block, BlockRef, Identifier, Location, Module, Operation, OperationRef, Region,
    RegionLike, Type, Value, ValueLike,
};
use melior::pass::{PassManager, conversion, transform};

use crate::cfg_perform::{
    ACTION_GOBACK, ProgramPerformPlan, collect_program_perform_plans, emit_all_perform_dispatchers,
    lower_alter_cfg, lower_flow_exit_cfg, lower_goto_cfg, lower_loop_cfg, lower_perform_range_cfg,
    lower_sentence_cfg,
};
use crate::context::CobolMlirContext;
use crate::ops::{CobolOpKind, decode_hex_bytes, llvm_ptr_type};
use crate::verifier::VerifiedCobolMlirModule;

/// Controls whether `@main(i32 %arg_count, !llvm.ptr %arg_values) -> i32` is synthesized in the lowered module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MainEntryMode {
    /// Emit C `@main` entrypoint initializing runtime, calling primary COBOL program, and tidying up (`-x`).
    EmitMain,
    /// Omit `@main` (`-c`, `-S`, `-m`, `-shared`).
    OmitMain,
}

/// Typestate-verified MLIR module lowered completely into the `llvm` dialect, ready for `mlirTranslateModuleToLLVMIR`.
#[derive(Debug)]
pub struct LoweredLlvmMlirModule<'ctx> {
    /// Verified `llvm`-dialect `melior` module.
    module: Module<'ctx>,
}

impl<'ctx> LoweredLlvmMlirModule<'ctx> {
    /// Borrows the lowered [`Module`].
    #[must_use]
    pub const fn as_module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// Formats the lowered `llvm` MLIR module as textual MLIR IR (`--emit-mlir-llvm`).
    #[must_use]
    pub fn to_mlir_text(&self) -> String {
        self.module.as_operation().to_string()
    }
}

/// Mutable state while lowering `cobol.*` operations into standard + `llvm` MLIR dialects.
pub struct LlvmRewriteState<'ctx> {
    /// Owning MLIR context.
    pub(crate) ctx: &'ctx CobolMlirContext,
    /// Output module receiving `llvm.mlir.global` and `func.func` operations.
    pub(crate) out_module: Module<'ctx>,
    /// Counter for dynamic-call module-name constant globals.
    dyn_name_counter: u32,
    /// Set of `cobol.storage` symbol names present in the verified input module.
    pub(crate) defined_globals: std::collections::BTreeSet<String>,
    /// Per-program `PERFORM ... THRU` / paragraph dispatcher plans.
    pub(crate) perform_plans: std::collections::BTreeMap<String, ProgramPerformPlan>,
}

/// Terminator emission state of the active LLVM basic block inside [`FnCursor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BlockTermination {
    /// Active block does not yet end with a control-flow terminator.
    Open,
    /// Active block already ends with a control-flow terminator (`cf.br` / `cf.cond_br`).
    Terminated,
}

/// Active per-function rewrite cursor tracking SSA value remapping and control-flow blocks.
pub struct FnCursor<'a, 'ctx> {
    /// Function body region owning all basic blocks.
    pub(crate) region: &'a Region<'ctx>,
    /// Dedicated function exit block (`load @rc_slot` + `func.return`).
    pub(crate) exit_block: BlockRef<'ctx, 'a>,
    /// Current insertion block.
    pub(crate) current_block: BlockRef<'ctx, 'a>,
    /// Whether `current_block` is still open or already ends with a control-flow terminator.
    pub(crate) termination: BlockTermination,
    /// Owning `RETURN-CODE` global symbol name.
    rc_slot_sym: String,
    /// Remapped `MlirValue.ptr -> lowered_ssa_val` table (`O(1)` lookup on large programs).
    ssa_values: std::collections::HashMap<usize, Value<'ctx, 'a>>,
    /// Fallback `i32` zero value in the entry block for infallible result unwrapping.
    pub(crate) zero_i32: Value<'ctx, 'a>,
    /// Fallback `!llvm.ptr` null value in the entry block for infallible result unwrapping.
    pub(crate) null_ptr: Value<'ctx, 'a>,
    /// Base program symbol prefix (`"cobol_entry_PROG"`) used by `cobol.perform_range`.
    pub(crate) prog_base_sym: String,
    /// Optional 4-byte `i32` stack slot (`0` fallthrough, `-1` `GOBACK`, `>=1` `GO TO`, `<=-2` `EXIT SECTION`).
    pub(crate) para_goback_slot: Option<Value<'ctx, 'a>>,
    /// Active break block for `EXIT PERFORM`.
    pub(crate) perform_break_block: Option<BlockRef<'ctx, 'a>>,
    /// Active latch/step block for `EXIT PERFORM CYCLE`.
    pub(crate) perform_cycle_block: Option<BlockRef<'ctx, 'a>>,
    /// Active sentence-end block for `NEXT SENTENCE`.
    pub(crate) sentence_exit_block: Option<BlockRef<'ctx, 'a>>,
}

/// Appends `op` to `block` and returns its first SSA result (or `fallback`).
///
/// # Arguments
///
/// * `block` - Target block.
/// * `op` - Operation to append.
/// * `fallback` - Dominated fallback value.
pub(crate) fn append_res0<'ctx, 'a>(
    block: BlockRef<'ctx, 'a>,
    op: Operation<'ctx>,
    fallback: Value<'ctx, 'a>,
) -> Value<'ctx, 'a> {
    block
        .append_operation(op)
        .result(0)
        .map_or(fallback, Into::into)
}

/// Records remapped SSA results of `op` into `cursor.ssa_values`.
///
/// # Arguments
///
/// * `op` - Source operation.
/// * `new_vals` - Lowered SSA values corresponding to `op`'s results.
/// * `cursor` - Active function cursor.
fn record_ssa_results<'ctx, 'a>(
    op: OperationRef<'ctx, 'a>,
    new_vals: &[Value<'ctx, 'a>],
    cursor: &mut FnCursor<'a, 'ctx>,
) {
    for (idx, old_r) in (0..op.result_count()).filter_map(|i| op.result(i).ok().map(|r| (i, r))) {
        let mapped = new_vals.get(idx).copied().unwrap_or(cursor.zero_i32);
        let raw_v: Value<'ctx, 'a> = old_r.into();
        let _ = cursor
            .ssa_values
            .insert(raw_v.to_raw().ptr as usize, mapped);
    }
}

/// Resolves operand `idx` of `op` through `cursor.ssa_values`.
///
/// # Arguments
///
/// * `op` - Source operation.
/// * `idx` - Zero-based operand index.
/// * `cursor` - Function cursor.
/// * `fallback` - Dominated fallback value.
pub(crate) fn op_in<'ctx, 'a>(
    op: OperationRef<'ctx, 'a>,
    idx: usize,
    cursor: &FnCursor<'a, 'ctx>,
    fallback: Value<'ctx, 'a>,
) -> Value<'ctx, 'a> {
    op.operand(idx)
        .map_or(fallback, |v| remap_val(cursor, v, fallback))
}

/// Lowers a [`VerifiedCobolMlirModule`] into a [`LoweredLlvmMlirModule`] at `opt_level`.
///
/// # Arguments
///
/// * `ctx` - Owning [`CobolMlirContext`].
/// * `verified` - Verified `cobol`-dialect module.
/// * `opt_level` - Optimization level (`O0`..`Os`).
/// * `main_mode` - Whether to emit `@main`.
///
/// # Errors
///
/// Returns [`CoblangError::MlirPassExecution`] if the MLIR pass pipeline fails, or
/// [`CoblangError::MlirVerification`] if post-lowering `llvm` verification fails.
pub fn lower_cobol_to_llvm_mlir<'ctx>(
    ctx: &'ctx CobolMlirContext,
    verified: &VerifiedCobolMlirModule<'ctx>,
    opt_level: OptimizationLevel,
    main_mode: MainEntryMode,
) -> Result<LoweredLlvmMlirModule<'ctx>> {
    let mut defined_globals = std::collections::BTreeSet::new();
    let mut scan = verified.as_module().body().first_operation();
    while let Some(op) = scan {
        if op.name().as_string_ref().as_str().unwrap_or("") == CobolOpKind::Storage.op_name() {
            defined_globals.insert(read_str_attr(op, "sym_name"));
        }
        scan = op.next_in_block();
    }
    let perform_plans = collect_program_perform_plans(verified.as_module());

    let mut rw = LlvmRewriteState {
        ctx,
        out_module: Module::new(ctx.unknown_location()),
        dyn_name_counter: 0,
        defined_globals,
        perform_plans,
    };

    emit_runtime_declarations(&rw);
    emit_all_perform_dispatchers(&rw.perform_plans, &rw);

    let mut primary_prog_sym: Option<String> = None;
    let mut cursor = verified.as_module().body().first_operation();
    while let Some(op) = cursor {
        let name_id = op.name();
        let name = name_id.as_string_ref().as_str().unwrap_or("");
        if name == CobolOpKind::Storage.op_name() {
            lower_storage_global(op, &rw)?;
        } else if name == CobolOpKind::Program.op_name() {
            let sym = lower_program_func(op, &mut rw);
            if primary_prog_sym.is_none() && !sym.starts_with("__cobol_proc_") {
                primary_prog_sym = Some(sym);
            }
        }
        cursor = op.next_in_block();
    }

    if main_mode == MainEntryMode::EmitMain
        && let Some(primary) = primary_prog_sym
    {
        emit_main_wrapper(&rw, &primary);
    }

    run_llvm_lowering_pipeline(ctx, &mut rw.out_module, opt_level)?;

    Ok(LoweredLlvmMlirModule {
        module: rw.out_module,
    })
}

/// Runs canonicalization/CSE/SCCP (when `opt_level != O0`) and converts `scf`/`arith`/`cf`/`func` to `llvm`.
///
/// # Arguments
///
/// * `ctx` - Owning [`CobolMlirContext`].
/// * `module` - Mutable MLIR module to transform in place.
/// * `opt_level` - Requested [`OptimizationLevel`].
///
/// # Errors
///
/// Returns [`CoblangError::MlirPassExecution`] if any MLIR conversion or canonicalization pass fails.
pub fn run_llvm_lowering_pipeline<'ctx>(
    ctx: &'ctx CobolMlirContext,
    module: &mut Module<'ctx>,
    opt_level: OptimizationLevel,
) -> Result<()> {
    let pm = PassManager::new(ctx.raw());
    pm.enable_verifier(true);

    if opt_level != OptimizationLevel::O0 {
        pm.add_pass(transform::create_canonicalizer());
        pm.add_pass(transform::create_cse());
        pm.add_pass(transform::create_sccp());
    }

    pm.add_pass(conversion::create_scf_to_control_flow());
    pm.add_pass(transform::create_canonicalizer());
    pm.nested_under("func.func")
        .add_pass(conversion::create_arith_to_llvm());
    pm.add_pass(conversion::create_control_flow_to_llvm());
    pm.add_pass(conversion::create_func_to_llvm());
    pm.add_pass(conversion::create_finalize_mem_ref_to_llvm());
    pm.add_pass(conversion::create_reconcile_unrealized_casts());

    pm.run(module)
        .map_err(|err| CoblangError::MlirPassExecution {
            pass_pipeline: format!("cobol-to-llvm({opt_level:?})"),
            message: err.to_string(),
        })
}

/// Builds an operation infallibly, falling back to `llvm.unreachable` if needed without panicking.
///
/// # Arguments
///
/// * `builder` - Configured [`OperationBuilder`].
/// * `loc` - Fallback location.
fn build_op<'c>(builder: OperationBuilder<'c>, loc: Location<'c>) -> Operation<'c> {
    builder.build().unwrap_or(llvm::unreachable(loc))
}

/// Declares external runtime ABI and `libc` `@exit` functions in `rw.out_module`.
///
/// # Arguments
///
/// * `rw` - Rewrite state.
fn emit_runtime_declarations(rw: &LlvmRewriteState<'_>) {
    let raw = rw.ctx.raw();
    let i8_ty: Type<'_> = IntegerType::new(raw, 8).into();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let i64_ty: Type<'_> = IntegerType::new(raw, 64).into();
    let ptr_ty = llvm_ptr_type(rw.ctx);

    let decls: &[(&str, &[Type<'_>], &[Type<'_>])] = &[
        ("cob_init", &[i32_ty, ptr_ty], &[]),
        ("cob_tidy", &[], &[]),
        ("coblang_rt_prepare_stop_run", &[i32_ty], &[i32_ty]),
        ("exit", &[i32_ty], &[]),
        ("coblang_rt_enter_call", &[], &[]),
        ("coblang_rt_leave_call", &[], &[]),
        ("coblang_rt_is_subprogram", &[], &[i32_ty]),
        (
            "coblang_rt_display",
            &[i32_ty, i32_ty, ptr_ty, ptr_ty, ptr_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_trim",
            &[ptr_ty, i64_ty, i32_ty, ptr_ty, ptr_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_call_dynamic",
            &[ptr_ty, i64_ty, ptr_ty],
            &[i32_ty],
        ),
    ];
    emit_decl_list(rw, decls);
    emit_part5_runtime_declarations(rw, (i8_ty, i32_ty, i64_ty, ptr_ty));
    emit_m10_runtime_declarations(rw, (i32_ty, i64_ty, ptr_ty));
    emit_m11_runtime_declarations(rw, (i32_ty, i64_ty, ptr_ty));
    emit_m12_m13_runtime_declarations(rw, (i32_ty, i64_ty, ptr_ty));
    emit_m14_m16_runtime_declarations(rw, (i32_ty, i64_ty, ptr_ty));
    emit_m18_m19_runtime_declarations(rw, (i32_ty, i64_ty, ptr_ty));
    emit_m20_runtime_declarations(rw, (i32_ty, i64_ty, ptr_ty));
}

/// Emits Milestones M18 (Report Writer) and M19 (`SCREEN SECTION`) runtime declarations.
///
/// # Arguments
///
/// * `rw` - Rewrite state.
/// * `tys` - `(i32, i64, !llvm.ptr)` MLIR types.
#[expect(
    clippy::too_many_lines,
    reason = "Flat declaration table for M18 and M19 C-ABI functions."
)]
fn emit_m18_m19_runtime_declarations<'c>(
    rw: &LlvmRewriteState<'c>,
    tys: (Type<'c>, Type<'c>, Type<'c>),
) {
    let (i32_ty, i64_ty, ptr_ty) = tys;
    let decls: &[(&str, &[Type<'_>], &[Type<'_>])] = &[
        (
            "coblang_rt_report_init_schema",
            &[
                i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty, ptr_ty, i64_ty, i64_ty, i64_ty, i64_ty,
                i64_ty, i64_ty, i32_ty, i32_ty, i32_ty,
            ],
            &[],
        ),
        (
            "coblang_rt_report_config_group",
            &[
                i32_ty, i32_ty, ptr_ty, i64_ty, i32_ty, i32_ty, i32_ty, i32_ty, i32_ty,
            ],
            &[],
        ),
        (
            "coblang_rt_report_config_sum",
            &[i32_ty, i32_ty, i32_ty, i32_ty],
            &[],
        ),
        (
            "coblang_rt_report_config_sum_link",
            &[i32_ty, i32_ty, i32_ty, ptr_ty, i64_ty],
            &[],
        ),
        (
            "coblang_rt_report_initiate",
            &[i32_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_report_set_dynamic_env",
            &[i32_ty, ptr_ty, i64_ty, i64_ty],
            &[],
        ),
        (
            "coblang_rt_report_snapshot_control",
            &[i32_ty, i32_ty, ptr_ty, i64_ty],
            &[],
        ),
        (
            "coblang_rt_report_begin_generate",
            &[i32_ty, ptr_ty, i64_ty, i32_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_report_begin_terminate",
            &[i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_report_next_step",
            &[i32_ty, ptr_ty, ptr_ty, ptr_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_report_check_after_new",
            &[i32_ty, i32_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_report_sync_sum_field",
            &[i32_ty, i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[],
        ),
        (
            "coblang_rt_report_submit_gate",
            &[i32_ty, i32_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_report_collect_line_spec",
            &[i32_ty, i32_ty, i32_ty],
            &[],
        ),
        ("coblang_rt_report_finish_collect", &[i32_ty], &[]),
        ("coblang_rt_report_suppress", &[i32_ty], &[]),
        (
            "coblang_rt_report_after_hook",
            &[i32_ty, i32_ty, i32_ty],
            &[],
        ),
        ("coblang_rt_report_begin_render_line", &[i32_ty], &[i32_ty]),
        (
            "coblang_rt_report_put_item",
            &[i32_ty, i32_ty, i32_ty, i32_ty, i32_ty, ptr_ty, i64_ty],
            &[],
        ),
        ("coblang_rt_report_finish_render_group", &[i32_ty], &[]),
        (
            "coblang_rt_report_add_sum_ascii",
            &[i32_ty, i32_ty, i32_ty, ptr_ty, i64_ty],
            &[],
        ),
        ("coblang_rt_screen_begin_stmt", &[], &[]),
        ("coblang_rt_screen_set_attr", &[i32_ty, i32_ty, i32_ty], &[]),
        (
            "coblang_rt_screen_stage_display_item",
            &[
                i32_ty, i64_ty, i32_ty, i32_ty, i64_ty, i32_ty, i64_ty, i32_ty, i32_ty, i32_ty,
                i32_ty, i32_ty, i32_ty, ptr_ty, i64_ty,
            ],
            &[],
        ),
        (
            "coblang_rt_screen_stage_edit_item",
            &[
                i32_ty, i64_ty, i32_ty, i32_ty, i64_ty, i32_ty, i64_ty, i32_ty, i32_ty, i32_ty,
                i32_ty, i32_ty, i32_ty, i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty, ptr_ty, i64_ty,
            ],
            &[],
        ),
        (
            "coblang_rt_screen_execute_display",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty, ptr_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_screen_execute_accept",
            &[
                i64_ty, i64_ty, ptr_ty, ptr_ty, i64_ty, ptr_ty, i64_ty, ptr_ty,
            ],
            &[i32_ty],
        ),
    ];
    emit_decl_list(rw, decls);
}

/// Emits Milestones M12 (intrinsics, `MODULE-*` stack) and M13 (`>>TURN`, `RAISE`/`RESUME`, `DEBUG-ITEM`, recursive frame) runtime declarations.
///
/// # Arguments
///
/// * `rw` - Rewrite state.
/// * `tys` - `(i32, i64, !llvm.ptr)` MLIR types.
fn emit_m12_m13_runtime_declarations<'c>(
    rw: &LlvmRewriteState<'c>,
    tys: (Type<'c>, Type<'c>, Type<'c>),
) {
    let (i32_ty, i64_ty, ptr_ty) = tys;
    let decls: &[(&str, &[Type<'_>], &[Type<'_>])] = &[
        (
            "coblang_rt_m12_enter_module",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_m12_leave_module", &[], &[i32_ty]),
        ("coblang_rt_m12_begin", &[i32_ty, i32_ty], &[i32_ty]),
        (
            "coblang_rt_m12_set_context",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_m12_pop_dec_arg", &[], &[i32_ty]),
        (
            "coblang_rt_m12_push_bytes_arg",
            &[ptr_ty, i64_ty, i32_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_m12_push_keyword_arg",
            &[ptr_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_m12_push_omitted_arg", &[], &[i32_ty]),
        (
            "coblang_rt_m12_push_alg_probe",
            &[i32_ty, i32_ty, i32_ty],
            &[i32_ty],
        ),
        ("coblang_rt_m12_series_begin", &[i32_ty], &[i32_ty]),
        ("coblang_rt_m12_series_end", &[], &[i32_ty]),
        ("coblang_rt_m12_eval_to_dec", &[], &[i32_ty]),
        ("coblang_rt_m12_eval_bytes", &[ptr_ty, ptr_ty], &[i32_ty]),
        (
            "coblang_rt_m12_format_display_dec",
            &[
                i32_ty, i32_ty, i32_ty, i32_ty, i32_ty, i32_ty, ptr_ty, ptr_ty,
            ],
            &[i32_ty],
        ),
        (
            "coblang_rt_m12_apply_refmod",
            &[
                ptr_ty, i64_ty, i32_ty, i64_ty, i64_ty, i32_ty, i32_ty, i32_ty, ptr_ty, i64_ty,
                ptr_ty, ptr_ty,
            ],
            &[i32_ty],
        ),
        ("coblang_rt_m13_set_turn", &[i32_ty, i32_ty], &[i32_ty]),
        ("coblang_rt_m13_last_exception_off", &[], &[i32_ty]),
        (
            "coblang_rt_m13_set_stmt_loc",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_m13_raise",
            &[i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_m13_take_pending_ec",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_m13_check_fatal_unhandled",
            &[i32_ty, i32_ty],
            &[i32_ty],
        ),
        ("coblang_rt_m13_enter_decl", &[], &[i32_ty]),
        ("coblang_rt_m13_set_resume", &[i32_ty, i32_ty], &[i32_ty]),
        ("coblang_rt_m13_finish_decl", &[i32_ty], &[i64_ty]),
        ("coblang_rt_m13_debug_set_suppress", &[i32_ty], &[i32_ty]),
        (
            "coblang_rt_m13_debug_prepare",
            &[
                ptr_ty, i64_ty, i32_ty, ptr_ty, i64_ty, i32_ty, i64_ty, i64_ty, i64_ty, ptr_ty,
                i64_ty,
            ],
            &[i32_ty],
        ),
        ("coblang_rt_m13_rec_frame_begin", &[], &[i32_ty]),
        (
            "coblang_rt_m13_rec_frame_save",
            &[ptr_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_m13_rec_frame_restore", &[], &[i32_ty]),
    ];
    emit_decl_list(rw, decls);
}

/// Emits Milestone M20 `XML`/`JSON`, special-register, and OO COBOL runtime declarations.
///
/// # Arguments
///
/// * `rw` - Rewrite state.
/// * `tys` - `(i32, i64, !llvm.ptr)` MLIR types.
fn emit_m20_runtime_declarations<'c>(
    rw: &LlvmRewriteState<'c>,
    tys: (Type<'c>, Type<'c>, Type<'c>),
) {
    let (i32_ty, i64_ty, ptr_ty) = tys;
    let decls: &[(&str, &[Type<'_>], &[Type<'_>])] = &[
        ("coblang_rt_m20_reg_ptr", &[i32_ty], &[ptr_ty]),
        ("coblang_rt_m20_reg_len", &[i32_ty], &[i64_ty]),
        (
            "coblang_rt_m20_set_ns",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[],
        ),
        (
            "coblang_rt_m20_set_count_dst",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[],
        ),
        ("coblang_rt_m20_push_odo", &[i32_ty, i64_ty], &[]),
        (
            "coblang_rt_m20_xml_gen",
            &[ptr_ty, i64_ty, ptr_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_m20_json_gen",
            &[ptr_ty, i64_ty, ptr_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_m20_json_parse",
            &[ptr_ty, i64_ty, ptr_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_m20_xml_parse_begin",
            &[ptr_ty, i64_ty, i32_ty, i32_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_m20_xml_parse_feed",
            &[i32_ty, ptr_ty, i64_ty],
            &[],
        ),
        ("coblang_rt_m20_xml_parse_next", &[i32_ty], &[i32_ty]),
        ("coblang_rt_m20_xml_parse_after", &[i32_ty], &[i32_ty]),
        ("coblang_rt_m20_xml_parse_end", &[i32_ty], &[i32_ty]),
        (
            "coblang_rt_m20_oo_reg_class",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[],
        ),
        (
            "coblang_rt_m20_oo_reg_method",
            &[ptr_ty, i64_ty, i32_ty, ptr_ty, i64_ty, ptr_ty],
            &[],
        ),
        ("coblang_rt_m20_oo_self_handle", &[], &[i64_ty]),
        ("coblang_rt_m20_oo_self_data_ptr", &[], &[ptr_ty]),
        ("coblang_rt_m20_oo_begin_invoke", &[ptr_ty, i64_ty], &[]),
        ("coblang_rt_m20_oo_push_arg", &[ptr_ty, i64_ty], &[]),
        (
            "coblang_rt_m20_oo_invoke",
            &[i32_ty, i64_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_m20_dec_push_numval",
            &[ptr_ty, i64_ty, i32_ty],
            &[i32_ty],
        ),
    ];
    emit_decl_list(rw, decls);
}

/// Emits Milestones M14–M16 File I/O, ISAM, `DECLARATIVES`, and `SORT`/`MERGE` runtime declarations.
///
/// # Arguments
///
/// * `rw` - Rewrite state.
/// * `tys` - `(i32, i64, !llvm.ptr)` MLIR types.
fn emit_m14_m16_runtime_declarations<'c>(
    rw: &LlvmRewriteState<'c>,
    tys: (Type<'c>, Type<'c>, Type<'c>),
) {
    let (i32_ty, i64_ty, ptr_ty) = tys;
    let decls: &[(&str, &[Type<'_>], &[Type<'_>])] = &[
        (
            "coblang_rt_file_open",
            &[
                ptr_ty, i64_ty, ptr_ty, i64_ty, i32_ty, i32_ty, i64_ty, i64_ty, i64_ty, i64_ty,
                ptr_ty, i32_ty,
            ],
            &[i32_ty],
        ),
        (
            "coblang_rt_file_close",
            &[ptr_ty, i64_ty, i32_ty, ptr_ty, i32_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_file_read",
            &[
                ptr_ty, i64_ty, ptr_ty, i64_ty, i32_ty, i32_ty, ptr_ty, i64_ty, i64_ty, i32_ty,
                i32_ty, ptr_ty, i32_ty,
            ],
            &[i32_ty],
        ),
        (
            "coblang_rt_file_write",
            &[
                ptr_ty, i64_ty, ptr_ty, i64_ty, i32_ty, i64_ty, i64_ty, i32_ty, i32_ty, ptr_ty,
                i32_ty,
            ],
            &[i32_ty],
        ),
        (
            "coblang_rt_file_rewrite",
            &[
                ptr_ty, i64_ty, ptr_ty, i64_ty, i64_ty, i32_ty, ptr_ty, i32_ty,
            ],
            &[i32_ty],
        ),
        (
            "coblang_rt_file_delete",
            &[
                ptr_ty, i64_ty, ptr_ty, i64_ty, i64_ty, i32_ty, ptr_ty, i32_ty,
            ],
            &[i32_ty],
        ),
        (
            "coblang_rt_file_delete_physical",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty, i32_ty, ptr_ty, i32_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_file_start",
            &[
                ptr_ty, i64_ty, ptr_ty, i64_ty, i32_ty, i32_ty, ptr_ty, i64_ty, i64_ty, i32_ty,
                ptr_ty, i32_ty,
            ],
            &[i32_ty],
        ),
        (
            "coblang_rt_file_unlock",
            &[ptr_ty, i64_ty, ptr_ty, i32_ty],
            &[i32_ty],
        ),
        ("coblang_rt_file_last_rec_len", &[], &[i64_ty]),
        ("coblang_rt_file_last_rel_key", &[], &[i64_ty]),
        ("coblang_rt_file_last_linage_counter", &[], &[i64_ty]),
        ("coblang_rt_file_fatal_abort", &[ptr_ty, i64_ty], &[i32_ty]),
        (
            "coblang_rt_file_set_linage",
            &[ptr_ty, i64_ty, i64_ty, i64_ty, i64_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_decl_enter", &[i32_ty, i32_ty], &[i32_ty]),
        ("coblang_rt_decl_leave", &[i32_ty, i32_ty], &[i32_ty]),
        (
            "coblang_rt_sort_begin",
            &[ptr_ty, i64_ty, i64_ty, i64_ty, i32_ty],
            &[i32_ty],
        ),
        ("coblang_rt_sort_begin_merge_stream", &[], &[i32_ty]),
        (
            "coblang_rt_sort_release",
            &[ptr_ty, i64_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_sort_finish_input", &[i64_ty], &[i32_ty]),
        (
            "coblang_rt_sort_return",
            &[ptr_ty, i64_ty, i32_ty, i64_ty],
            &[i64_ty],
        ),
        ("coblang_rt_sort_end", &[], &[i32_ty]),
    ];
    emit_decl_list(rw, decls);
}

/// Emits Milestone M11 (`ALLOCATE`, `FREE`, `BASED` guard, `SET ... TO ENTRY`) runtime declarations.
///
/// # Arguments
///
/// * `rw` - Rewrite state.
/// * `tys` - `(i32, i64, !llvm.ptr)` MLIR types.
fn emit_m11_runtime_declarations<'c>(
    rw: &LlvmRewriteState<'c>,
    tys: (Type<'c>, Type<'c>, Type<'c>),
) {
    let (i32_ty, i64_ty, ptr_ty) = tys;
    let decls: &[(&str, &[Type<'_>], &[Type<'_>])] = &[
        ("coblang_rt_alloc", &[i64_ty, i32_ty], &[ptr_ty]),
        (
            "coblang_rt_alloc_fill_pattern",
            &[i64_ty, ptr_ty, i64_ty, i32_ty],
            &[ptr_ty],
        ),
        ("coblang_rt_free", &[ptr_ty], &[i32_ty]),
        (
            "coblang_rt_check_based",
            &[ptr_ty, ptr_ty, i64_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_resolve_entry", &[ptr_ty, i64_ty], &[ptr_ty]),
        (
            "coblang_rt_resolve_external",
            &[ptr_ty, i64_ty, i64_ty, ptr_ty, i64_ty],
            &[ptr_ty],
        ),
        (
            "coblang_rt_call_invoke",
            &[ptr_ty, ptr_ty, i64_ty, i32_ty, i64_ty, ptr_ty, ptr_ty],
            &[i32_ty],
        ),
        ("coblang_rt_get_call_param", &[i64_ty, ptr_ty], &[ptr_ty]),
        ("coblang_rt_current_narg", &[], &[i32_ty]),
        (
            "coblang_rt_copy_cli_arg",
            &[i64_ty, ptr_ty, i64_ty],
            &[i64_ty],
        ),
        ("coblang_rt_cancel_by_slice", &[ptr_ty, i64_ty], &[]),
        (
            "coblang_rt_push_call_frame",
            &[ptr_ty, ptr_ty, i64_ty, ptr_ty],
            &[i32_ty],
        ),
        ("coblang_rt_pop_call_frame", &[i32_ty, i32_ty], &[i32_ty]),
        ("coblang_rt_raw_copy", &[ptr_ty, ptr_ty, i64_ty], &[]),
    ];
    emit_decl_list(rw, decls);
}

/// Emits Milestone M10 `STRING` / `UNSTRING` / `INSPECT` / `ACCEPT` / `DISPLAY UPON ENVIRONMENT-*` declarations.
///
/// # Arguments
///
/// * `rw` - Rewrite state.
/// * `tys` - `(i32, i64, !llvm.ptr)` MLIR types.
fn emit_m10_runtime_declarations<'c>(
    rw: &LlvmRewriteState<'c>,
    tys: (Type<'c>, Type<'c>, Type<'c>),
) {
    let (i32_ty, i64_ty, ptr_ty) = tys;
    let decls: &[(&str, &[Type<'_>], &[Type<'_>])] = &[
        ("coblang_rt_disp_flush_env", &[i32_ty], &[i32_ty]),
        (
            "coblang_rt_accept",
            &[i32_ty, i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_str_begin", &[], &[]),
        (
            "coblang_rt_str_set_delim",
            &[i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_str_push_src",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_str_exec", &[ptr_ty, i64_ty, i64_ty], &[i32_ty]),
        ("coblang_rt_str_last_ptr", &[], &[i64_ty]),
        (
            "coblang_rt_unstr_begin",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_unstr_push_delim",
            &[i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_unstr_set_delim_spec",
            &[ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_unstr_step",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i64_ty],
        ),
        ("coblang_rt_unstr_finish", &[], &[i32_ty]),
        ("coblang_rt_unstr_tally_delta", &[], &[i64_ty]),
        ("coblang_rt_insp_begin", &[i64_ty], &[]),
        ("coblang_rt_insp_clear_win", &[], &[]),
        (
            "coblang_rt_insp_set_win",
            &[i32_ty, i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_insp_set_op",
            &[i32_ty, i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_insp_push_tally", &[i64_ty, i32_ty], &[]),
        ("coblang_rt_insp_push_replace", &[i32_ty], &[]),
        (
            "coblang_rt_insp_exec",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_insp_get_delta", &[i64_ty], &[i64_ty]),
        (
            "coblang_rt_insp_convert",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_str_set_dst_spec", &[ptr_ty, i64_ty], &[i32_ty]),
        (
            "coblang_rt_utf8_refmod_off",
            &[ptr_ty, i64_ty, i64_ty],
            &[i64_ty],
        ),
        (
            "coblang_rt_utf8_refmod_len",
            &[ptr_ty, i64_ty, i64_ty, i64_ty],
            &[i64_ty],
        ),
    ];
    emit_decl_list(rw, decls);
}

/// Emits Part 5 runtime descriptor/decimal/initialize/display `func.func private` declarations.
///
/// # Arguments
///
/// * `rw` - Rewrite state.
/// * `tys` - `(i8, i32, i64, !llvm.ptr)` MLIR types.
fn emit_part5_runtime_declarations<'c>(
    rw: &LlvmRewriteState<'c>,
    tys: (Type<'c>, Type<'c>, Type<'c>, Type<'c>),
) {
    let (i8_ty, i32_ty, i64_ty, ptr_ty) = tys;
    let decls: &[(&str, &[Type<'_>], &[Type<'_>])] = &[
        (
            "coblang_rt_move_desc",
            &[
                i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty, ptr_ty, i64_ty, ptr_ty, i64_ty,
            ],
            &[i32_ty],
        ),
        ("coblang_rt_disp_begin", &[], &[]),
        ("coblang_rt_disp_push_raw", &[ptr_ty, i64_ty], &[i32_ty]),
        (
            "coblang_rt_disp_push_field",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_disp_push_hex", &[ptr_ty, i64_ty], &[i32_ty]),
        ("coblang_rt_disp_flush", &[i32_ty, i32_ty], &[i32_ty]),
        ("coblang_rt_dec_begin", &[], &[]),
        (
            "coblang_rt_dec_push_field",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        ("coblang_rt_dec_push_ascii", &[ptr_ty, i64_ty], &[i32_ty]),
        ("coblang_rt_dec_neg", &[], &[]),
        ("coblang_rt_dec_binop", &[i32_ty], &[i32_ty]),
        (
            "coblang_rt_dec_store",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty, i32_ty, i32_ty, i32_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_dec_div_rem",
            &[
                ptr_ty, i64_ty, ptr_ty, i64_ty, i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty, i32_ty,
            ],
            &[i32_ty],
        ),
        ("coblang_rt_dec_cmp", &[i32_ty], &[i32_ty]),
        (
            "coblang_rt_cmp_alpha",
            &[i32_ty, ptr_ty, i64_ty, i32_ty, ptr_ty, i64_ty, i32_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_cmp_alpha_ex",
            &[
                i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty, i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty,
                i32_ty, ptr_ty, i64_ty,
            ],
            &[i32_ty],
        ),
        (
            "coblang_rt_class_check",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty, i32_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_class_custom",
            &[ptr_ty, i64_ty, ptr_ty],
            &[i32_ty],
        ),
        ("coblang_rt_get_switch", &[i32_ty], &[i32_ty]),
        ("coblang_rt_set_switch", &[i32_ty, i32_ty], &[i32_ty]),
        ("coblang_rt_init_begin", &[], &[]),
        (
            "coblang_rt_init_push_rep",
            &[i8_ty, i32_ty, ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_init_exec",
            &[ptr_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_check_odo",
            &[i64_ty, i64_ty, i64_ty, i32_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_check_sub",
            &[i64_ty, i64_ty, i32_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_check_refmod",
            &[i64_ty, i64_ty, i32_ty, i64_ty, i32_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
        (
            "coblang_rt_sort_table",
            &[ptr_ty, i64_ty, i64_ty, ptr_ty, i64_ty],
            &[i32_ty],
        ),
    ];
    emit_decl_list(rw, decls);
}

/// Appends a slice of `func.func private` external declarations into `rw.out_module`.
///
/// # Arguments
///
/// * `rw` - Rewrite state.
/// * `decls` - `(symbol_name, param_types, result_types)` slice.
fn emit_decl_list<'c>(rw: &LlvmRewriteState<'c>, decls: &[(&str, &[Type<'c>], &[Type<'c>])]) {
    let raw = rw.ctx.raw();
    let loc = rw.ctx.unknown_location();
    let priv_attr = [(
        Identifier::new(raw, "sym_visibility"),
        StringAttribute::new(raw, "private").into(),
    )];
    for &(name, inputs, results) in decls {
        let fn_ty = FunctionType::new(raw, inputs, results);
        let op = func::func(
            raw,
            StringAttribute::new(raw, name),
            TypeAttribute::new(fn_ty.into()),
            Region::new(),
            &priv_attr,
            loc,
        );
        let _ = rw.out_module.body().append_operation(op);
    }
}

/// Reads a string attribute `key` from `op`, returning `""` if absent.
///
/// # Arguments
///
/// * `op` - Operation reference.
/// * `key` - Attribute name.
pub(crate) fn read_str_attr(op: OperationRef<'_, '_>, key: &str) -> String {
    let printed = op
        .attribute(key)
        .ok()
        .and_then(|a| StringAttribute::try_from(a).ok())
        .map_or_default(|s| Attribute::from(s).to_string());
    printed
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(&printed)
        .to_owned()
}

/// Reads a signed integer attribute `key` from `op` by parsing its canonical decimal prefix.
///
/// # Arguments
///
/// * `op` - Operation reference.
/// * `key` - Attribute name.
pub(crate) fn read_i64_attr(op: OperationRef<'_, '_>, key: &str) -> i64 {
    let text = op.attribute(key).ok().map_or_default(|a| a.to_string());
    let num_part = text.split(':').next().unwrap_or("0").trim();
    num_part.parse::<i64>().unwrap_or(0)
}

/// Parses a `DenseI32ArrayAttribute` (`array<i32: ...>`) into `Vec<i32>`.
///
/// # Arguments
///
/// * `op` - Operation reference.
/// * `key` - Attribute name.
pub(crate) fn read_i32_array_attr(op: OperationRef<'_, '_>, key: &str) -> Vec<i32> {
    let text = op.attribute(key).ok().map_or_default(|a| a.to_string());
    let Some(after_colon) = text.split(':').nth(1) else {
        return Vec::new();
    };
    after_colon
        .trim()
        .trim_end_matches('>')
        .split(',')
        .map(|part| part.trim().parse::<i32>().unwrap_or(0))
        .collect()
}

/// Mutability mode of an emitted `[N x i8]` LLVM global storage buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GlobalMutability {
    /// Mutable Working-Storage or Local-Storage buffer.
    Mutable,
    /// Read-only `StaticConst` literal / descriptor table (`constant` attribute attached).
    ReadOnly,
}

/// Emits an `llvm.mlir.global` byte array for a `cobol.storage` operation.
///
/// # Arguments
///
/// * `op` - Verified `cobol.storage` operation.
/// * `rw` - Rewrite state.
fn lower_storage_global(op: OperationRef<'_, '_>, rw: &LlvmRewriteState<'_>) -> Result<()> {
    let sym_name = read_str_attr(op, "sym_name");
    let storage_kind = read_str_attr(op, "storage_kind");
    let alignment = read_i64_attr(op, "alignment").max(1);
    let declared_size = u32::try_from(read_i64_attr(op, "byte_size").max(1)).unwrap_or(1);
    let init_hex = read_str_attr(op, "init_hex");
    let bytes = decode_hex_bytes(&init_hex)?;
    let mutability = if storage_kind == "static_const" {
        GlobalMutability::ReadOnly
    } else {
        GlobalMutability::Mutable
    };
    emit_raw_byte_global(
        rw,
        &sym_name,
        &bytes,
        declared_size,
        alignment,
        mutability,
        op.location(),
    );
    Ok(())
}

/// Emits an internal `llvm.mlir.global` `[N x i8]` initialized with `bytes` (or `zeroinitializer` when all-zero / `.bss`).
///
/// # Arguments
///
/// * `rw` - Rewrite state.
/// * `sym_name` - Global symbol name.
/// * `bytes` - Initializer bytes (`len >= 1`).
/// * `declared_size` - Declared byte size (`> bytes.len()` for `.bss` tables).
/// * `alignment` - Byte alignment.
/// * `mutability` - [`GlobalMutability`] (`Mutable` vs `ReadOnly`).
/// * `loc` - Source location.
fn emit_raw_byte_global<'c>(
    rw: &LlvmRewriteState<'c>,
    sym_name: &str,
    bytes: &[u8],
    declared_size: u32,
    alignment: i64,
    mutability: GlobalMutability,
    loc: Location<'c>,
) {
    let raw = rw.ctx.raw();
    let i8_ty: Type<'_> = IntegerType::new(raw, 8).into();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let i64_ty: Type<'_> = IntegerType::new(raw, 64).into();

    let count_u32 = u32::try_from(bytes.len())
        .unwrap_or(1)
        .max(declared_size)
        .max(1);
    let arr_ty = llvm::r#type::array(i8_ty, count_u32);
    let is_all_zero = (count_u32 as usize) > bytes.len() || bytes.iter().all(|&b| b == 0);

    let mut attrs = vec![
        (
            Identifier::new(raw, "sym_name"),
            StringAttribute::new(raw, sym_name).into(),
        ),
        (
            Identifier::new(raw, "global_type"),
            TypeAttribute::new(arr_ty).into(),
        ),
        (
            Identifier::new(raw, "linkage"),
            linkage(raw, Linkage::Internal),
        ),
        (
            Identifier::new(raw, "addr_space"),
            IntegerAttribute::new(i32_ty, 0).into(),
        ),
        (
            Identifier::new(raw, "alignment"),
            IntegerAttribute::new(i64_ty, alignment).into(),
        ),
    ];

    let init_reg = Region::new();
    if is_all_zero {
        let blk = init_reg.append_block(Block::new(&[]));
        let z_op = blk.append_operation(llvm::zero(arr_ty, loc));
        let z_val: Option<Value<'_, '_>> = z_op.result(0).ok().map(Into::into);
        let _ = blk.append_operation(llvm::r#return(z_val, loc));
    } else {
        let tensor_ty = RankedTensorType::new(&[u64::from(count_u32)], i8_ty, None);
        let elem_attrs: Vec<Attribute<'_>> = bytes
            .iter()
            .map(|&b| IntegerAttribute::new(i8_ty, i64::from(b.cast_signed())).into())
            .collect();
        attrs.extend(
            DenseElementsAttribute::new(tensor_ty.into(), &elem_attrs)
                .ok()
                .map(|dense| (Identifier::new(raw, "value"), dense.into())),
        );
    }
    if mutability == GlobalMutability::ReadOnly {
        attrs.push((Identifier::new(raw, "constant"), Attribute::unit(raw)));
    }

    let global_op = build_op(
        OperationBuilder::new("llvm.mlir.global", loc)
            .add_attributes(&attrs)
            .add_regions([init_reg]),
        loc,
    );
    let _ = rw.out_module.body().append_operation(global_op);
}

/// Emits `"llvm.mlir.addressof"` `@sym` into `block`, returning `!llvm.ptr`.
///
/// # Arguments
///
/// * `ctx` - Owning [`CobolMlirContext`].
/// * `block` - Target block.
/// * `sym` - Referenced global symbol.
/// * `fallback_ptr` - Fallback `!llvm.ptr` value.
/// * `loc` - Source location.
pub(crate) fn emit_addressof<'c, 'a>(
    ctx: &'c CobolMlirContext,
    block: BlockRef<'c, 'a>,
    sym: &str,
    fallback_ptr: Value<'c, 'a>,
    loc: Location<'c>,
) -> Value<'c, 'a> {
    let ptr_ty = llvm_ptr_type(ctx);
    let op = build_op(
        OperationBuilder::new("llvm.mlir.addressof", loc)
            .add_attributes(&[(
                Identifier::new(ctx.raw(), "global_name"),
                FlatSymbolRefAttribute::new(ctx.raw(), sym).into(),
            )])
            .add_results(&[ptr_ty]),
        loc,
    );
    append_res0(block, op, fallback_ptr)
}

/// Lowers a `cobol.program` operation into `func.func @<sym>() -> i32` (plus optional C alias) and returns `sym_name`.
///
/// # Arguments
///
/// * `prog_op` - Verified `cobol.program` operation.
/// * `rw` - Rewrite state.
fn lower_program_func<'ctx>(
    prog_op: OperationRef<'ctx, '_>,
    rw: &mut LlvmRewriteState<'ctx>,
) -> String {
    let raw = rw.ctx.raw();
    let loc = prog_op.location();
    let sym_name = read_str_attr(prog_op, "sym_name");
    let raw_c_alias = read_str_attr(prog_op, "raw_c_alias");
    let rc_slot_sym = read_str_attr(prog_op, "rc_slot_sym");
    let is_recursive = read_str_attr(prog_op, "recursivity") == "recursive";

    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let ptr_ty = llvm_ptr_type(rw.ctx);
    let fn_ty = FunctionType::new(raw, &[], &[i32_ty]);

    let fn_region = Region::new();
    let entry_block = fn_region.append_block(Block::new(&[]));
    let exit_block = fn_region.append_block(Block::new(&[]));

    let zero_op = arith::constant(raw, IntegerAttribute::new(i32_ty, 0).into(), loc);
    let appended_zero = entry_block.append_operation(zero_op);
    let seed_vals: Vec<Value<'ctx, '_>> = (0..appended_zero.result_count())
        .filter_map(|z_idx| appended_zero.result(z_idx).ok().map(Into::into))
        .collect();
    for zero_i32 in seed_vals {
        let null_ptr = append_res0(entry_block, llvm::zero(ptr_ty, loc), zero_i32);
        let prog_base_sym = if let Some(rest) = sym_name.strip_prefix("__cobol_proc_") {
            String::from(rest.rsplit_once('_').map_or(rest, |(b, _)| b))
        } else {
            sym_name.clone()
        };

        let para_goback_slot = setup_program_exit_block(
            rw,
            (entry_block, exit_block),
            (&sym_name, &rc_slot_sym, is_recursive),
            (zero_i32, null_ptr),
            loc,
        );

        let mut fn_cursor = FnCursor {
            region: &fn_region,
            exit_block,
            current_block: entry_block,
            termination: BlockTermination::Open,
            rc_slot_sym: rc_slot_sym.clone(),
            ssa_values: std::collections::HashMap::new(),
            zero_i32,
            null_ptr,
            prog_base_sym,
            para_goback_slot,
            perform_break_block: None,
            perform_cycle_block: None,
            sentence_exit_block: None,
        };

        for src_region in (0..prog_op.region_count()).filter_map(|i| prog_op.region(i).ok()) {
            let mut b_cur = src_region.first_block();
            while let Some(src_entry) = b_cur {
                rewrite_cobol_block(src_entry, &mut fn_cursor, rw);
                b_cur = src_entry.next_in_region();
            }
        }

        if fn_cursor.termination == BlockTermination::Open {
            let _ =
                fn_cursor
                    .current_block
                    .append_operation(cf::br(&fn_cursor.exit_block, &[], loc));
        }
    }

    let priv_attr = [(
        Identifier::new(raw, "sym_visibility"),
        StringAttribute::new(raw, "private").into(),
    )];
    let fn_attrs: &[(Identifier<'_>, Attribute<'_>)] = if sym_name.starts_with("__cobol_proc_") {
        &priv_attr
    } else {
        &[]
    };
    let fn_op = func::func(
        raw,
        StringAttribute::new(raw, &sym_name),
        TypeAttribute::new(fn_ty.into()),
        fn_region,
        fn_attrs,
        loc,
    );
    let _ = rw.out_module.body().append_operation(fn_op);

    if !raw_c_alias.is_empty() {
        emit_c_alias_wrapper(rw, &sym_name, &raw_c_alias, fn_ty, i32_ty, loc);
    }
    if let Some(encoded) = sym_name.strip_prefix("__coblang_prog_") {
        emit_cancel_reset_hook(rw, &sym_name, encoded, loc);
    }

    sym_name
}

/// Emits `@__coblang_cancel_<encoded>()` marking `@__cobol_cancel_pending_<sym_name> = 1` (and
/// resetting `@__cobol_bss_guard_<sym_name> = 0` when present) so the next `CALL` reinitializes Working-Storage.
///
/// # Arguments
///
/// * `rw` - Module rewrite state.
/// * `sym_name` - Canonical program symbol (`__coblang_prog_<encoded>`).
/// * `encoded` - Encoded suffix after `__coblang_prog_`.
/// * `loc` - Source location.
fn emit_cancel_reset_hook<'ctx>(
    rw: &LlvmRewriteState<'ctx>,
    sym_name: &str,
    encoded: &str,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let i8_ty: Type<'_> = IntegerType::new(raw, 8).into();
    let ptr_ty = llvm_ptr_type(rw.ctx);
    let cancel_sym = format!("__coblang_cancel_{encoded}");
    let region = Region::new();
    let blk = region.append_block(Block::new(&[]));
    let one_op = blk.append_operation(arith::constant(
        raw,
        IntegerAttribute::new(i8_ty, 1).into(),
        loc,
    ));
    for one_i8 in (0..1).filter_map(|i| one_op.result(i).ok().map(Into::into)) {
        let pend_sym = format!("__cobol_cancel_pending_{sym_name}");
        if rw.defined_globals.contains(&pend_sym) {
            let p_ptr = append_res0(
                blk,
                OperationBuilder::new("llvm.mlir.addressof", loc)
                    .add_attributes(&[(
                        Identifier::new(raw, "global_name"),
                        FlatSymbolRefAttribute::new(raw, &pend_sym).into(),
                    )])
                    .add_results(&[ptr_ty])
                    .build()
                    .unwrap_or(llvm::unreachable(loc)),
                one_i8,
            );
            let _ = blk.append_operation(llvm::store(
                raw,
                one_i8,
                p_ptr,
                loc,
                LoadStoreOptions::new(),
            ));
        }
        let bss_sym = format!("__cobol_bss_guard_{sym_name}");
        if rw.defined_globals.contains(&bss_sym) {
            let z_i8 = append_res0(
                blk,
                arith::constant(raw, IntegerAttribute::new(i8_ty, 0).into(), loc),
                one_i8,
            );
            let b_ptr = append_res0(
                blk,
                OperationBuilder::new("llvm.mlir.addressof", loc)
                    .add_attributes(&[(
                        Identifier::new(raw, "global_name"),
                        FlatSymbolRefAttribute::new(raw, &bss_sym).into(),
                    )])
                    .add_results(&[ptr_ty])
                    .build()
                    .unwrap_or(llvm::unreachable(loc)),
                one_i8,
            );
            let _ =
                blk.append_operation(llvm::store(raw, z_i8, b_ptr, loc, LoadStoreOptions::new()));
        }
    }
    let _ = blk.append_operation(func::r#return(&[], loc));
    let cancel_fn = func::func(
        raw,
        StringAttribute::new(raw, &cancel_sym),
        TypeAttribute::new(FunctionType::new(raw, &[], &[]).into()),
        region,
        &[],
        loc,
    );
    let _ = rw.out_module.body().append_operation(cancel_fn);
}

/// Populates `exit_block` for either a `__cobol_proc_*` paragraph helper or a main/subprogram entry.
///
/// # Arguments
///
/// * `rw` - Module rewrite state.
/// * `blocks` - `(entry_block, exit_block)` pair.
/// * `syms` - `(sym_name, rc_slot_sym)` pair.
/// * `seeds` - `(zero_i32, null_ptr)` dominated fallback values.
/// * `loc` - Source location.
fn setup_program_exit_block<'a, 'ctx>(
    rw: &LlvmRewriteState<'ctx>,
    blocks: (BlockRef<'ctx, 'a>, BlockRef<'ctx, 'a>),
    syms: (&str, &str, bool),
    seeds: (Value<'ctx, 'a>, Value<'ctx, 'a>),
    loc: Location<'ctx>,
) -> Option<Value<'ctx, 'a>> {
    let raw = rw.ctx.raw();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let ptr_ty = llvm_ptr_type(rw.ctx);
    let (entry_block, exit_block) = blocks;
    let (sym_name, rc_slot_sym, is_recursive) = syms;
    let (zero_i32, null_ptr) = seeds;

    if sym_name.starts_with("__cobol_proc_") {
        let one_64 = append_res0(
            entry_block,
            arith::constant(
                raw,
                IntegerAttribute::new(IntegerType::new(raw, 64).into(), 1).into(),
                loc,
            ),
            zero_i32,
        );
        let slot = append_res0(
            entry_block,
            llvm::alloca(
                raw,
                one_64,
                ptr_ty,
                loc,
                AllocaOptions::new().elem_type(Some(TypeAttribute::new(i32_ty))),
            ),
            null_ptr,
        );
        let _ = entry_block.append_operation(llvm::store(
            raw,
            zero_i32,
            slot,
            loc,
            LoadStoreOptions::new(),
        ));
        let exit_flag = append_res0(
            exit_block,
            llvm::load(raw, slot, i32_ty, loc, LoadStoreOptions::new()),
            zero_i32,
        );
        let _ = exit_block.append_operation(func::r#return(&[exit_flag], loc));
        Some(slot)
    } else {
        if is_recursive {
            let _ = exit_block.append_operation(func::call(
                raw,
                FlatSymbolRefAttribute::new(raw, "coblang_rt_m13_rec_frame_restore"),
                &[],
                &[i32_ty],
                loc,
            ));
        }
        let _ = exit_block.append_operation(func::call(
            raw,
            FlatSymbolRefAttribute::new(raw, "coblang_rt_m12_leave_module"),
            &[],
            &[i32_ty],
            loc,
        ));
        let rc_exit_ptr = emit_addressof(rw.ctx, exit_block, rc_slot_sym, null_ptr, loc);
        let exit_rc = append_res0(
            exit_block,
            llvm::load(raw, rc_exit_ptr, i32_ty, loc, LoadStoreOptions::new()),
            zero_i32,
        );
        let _ = exit_block.append_operation(func::r#return(&[exit_rc], loc));
        None
    }
}

/// Emits a forwarding C-alias `func.func` calling `@<sym_name>() -> i32`.
///
/// # Arguments
///
/// * `rw` - Rewrite state.
/// * `sym_name` - Canonical mangled target symbol.
/// * `raw_c_alias` - Unprefixed C alias name.
/// * `fn_ty` - `() -> i32` function type.
/// * `i32_ty` - `i32` MLIR type.
/// * `loc` - Source location.
fn emit_c_alias_wrapper<'ctx>(
    rw: &LlvmRewriteState<'ctx>,
    sym_name: &str,
    raw_c_alias: &str,
    fn_ty: FunctionType<'ctx>,
    i32_ty: Type<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let alias_region = Region::new();
    let alias_blk = alias_region.append_block(Block::new(&[]));
    let call_op = func::call(
        raw,
        FlatSymbolRefAttribute::new(raw, sym_name),
        &[],
        &[i32_ty],
        loc,
    );
    let appended = alias_blk.append_operation(call_op);
    let ret_vals: Vec<Value<'ctx, '_>> = (0..appended.result_count())
        .filter_map(|idx| appended.result(idx).ok().map(Into::into))
        .collect();
    let _ = alias_blk.append_operation(func::r#return(&ret_vals, loc));
    let alias_fn = func::func(
        raw,
        StringAttribute::new(raw, raw_c_alias),
        TypeAttribute::new(fn_ty.into()),
        alias_region,
        &[],
        loc,
    );
    let _ = rw.out_module.body().append_operation(alias_fn);
}

/// Lookups a remapped SSA [`Value`] in `cursor.ssa_values`.
///
/// # Arguments
///
/// * `cursor` - Active function cursor.
/// * `old` - Source operation operand value.
/// * `fallback` - Fallback value.
pub(crate) fn remap_val<'ctx, 'a>(
    cursor: &FnCursor<'a, 'ctx>,
    old: Value<'ctx, '_>,
    fallback: Value<'ctx, 'a>,
) -> Value<'ctx, 'a> {
    cursor
        .ssa_values
        .get(&(old.to_raw().ptr as usize))
        .copied()
        .unwrap_or(fallback)
}

/// Rewrites every operation inside `src_block` into `cursor.current_block`, stopping at block termination.
///
/// # Arguments
///
/// * `src_block` - Source block inside `cobol.program` or `scf.if`.
/// * `cursor` - Active function cursor.
/// * `rw` - Module rewrite state.
pub(crate) fn rewrite_cobol_block<'a, 'ctx>(
    src_block: BlockRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &mut LlvmRewriteState<'ctx>,
) {
    let mut cur = src_block.first_operation();
    while let Some(op) = cur {
        if cursor.termination == BlockTermination::Terminated {
            break;
        }
        rewrite_single_op(op, cursor, rw);
        cur = op.next_in_block();
    }
}

/// Rewrites a single operation from a `cobol.program` region into `func`/`arith`/`cf`/`llvm` ops.
///
/// # Arguments
///
/// * `op` - Source operation.
/// * `cursor` - Active function cursor.
/// * `rw` - Module rewrite state.
fn rewrite_single_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &mut LlvmRewriteState<'ctx>,
) {
    let name_id = op.name();
    let op_name = name_id.as_string_ref().as_str().unwrap_or("");
    if op_name == "scf.yield" {
        return;
    }
    let loc = op.location();

    if op_name == "scf.if" {
        rewrite_scf_if(op, cursor, rw, loc);
        return;
    }

    if let Some(cobol_kind) = CobolOpKind::from_op_name(op_name) {
        rewrite_cobol_dialect_op(cobol_kind, op, cursor, rw, loc);
        return;
    }

    clone_standard_op(op, op_name, cursor, rw, loc);
}

/// Rewrites an `scf.if` operation into `cf.cond_br` + `then`/`else`/`merge` basic blocks.
///
/// # Arguments
///
/// * `op` - `scf.if` operation reference.
/// * `cursor` - Active function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn rewrite_scf_if<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &mut LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let cond_val = op_in(op, 0, cursor, cursor.zero_i32);
    let then_blk = cursor.region.append_block(Block::new(&[]));
    let else_blk = cursor.region.append_block(Block::new(&[]));
    let merge_blk = cursor.region.append_block(Block::new(&[]));

    let _ = cursor.current_block.append_operation(cf::cond_br(
        rw.ctx.raw(),
        cond_val,
        &then_blk,
        &else_blk,
        &[],
        &[],
        loc,
    ));

    for (r_idx, reg) in (0..op.region_count()).filter_map(|i| op.region(i).ok().map(|r| (i, r))) {
        let target_blk = if r_idx == 0 { then_blk } else { else_blk };
        cursor.current_block = target_blk;
        cursor.termination = BlockTermination::Open;
        let mut b_cur = reg.first_block();
        while let Some(b_src) = b_cur {
            rewrite_cobol_block(b_src, cursor, rw);
            b_cur = b_src.next_in_region();
        }
        if cursor.termination == BlockTermination::Open {
            let _ = cursor
                .current_block
                .append_operation(cf::br(&merge_blk, &[], loc));
        }
    }

    cursor.current_block = merge_blk;
    cursor.termination = BlockTermination::Open;
}

/// Clones a single-result standard `arith.*` or `llvm.*` operation with remapped operands.
///
/// # Arguments
///
/// * `op` - Source operation.
/// * `op_name` - Operation name string.
/// * `cursor` - Active function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn clone_standard_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    op_name: &str,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let mut mapped_ops = Vec::with_capacity(op.operand_count());
    for idx in 0..op.operand_count() {
        mapped_ops.push(op_in(op, idx, cursor, cursor.zero_i32));
    }
    let res_tys: Vec<_> = (0..op.result_count())
        .filter_map(|r_idx| op.result(r_idx).ok().map(|r| r.r#type()))
        .collect();
    let mut attrs = Vec::new();
    for key in [
        "value",
        "predicate",
        "overflowFlags",
        "ordering",
        "alignment",
        "elem_type",
        "rawConstantIndices",
    ] {
        if let Ok(a) = op.attribute(key) {
            attrs.push((Identifier::new(rw.ctx.raw(), key), a));
        }
    }

    let cloned = build_op(
        OperationBuilder::new(op_name, loc)
            .add_operands(&mapped_ops)
            .add_attributes(&attrs)
            .add_results(&res_tys),
        loc,
    );
    if res_tys.is_empty() {
        let _ = cursor.current_block.append_operation(cloned);
    } else {
        let new_val = append_res0(cursor.current_block, cloned, cursor.zero_i32);
        record_ssa_results(op, &[new_val], cursor);
    }
}

/// Rewrites a `cobol.*` dialect operation into `func`/`arith`/`cf`/`llvm` operations.
///
/// # Arguments
///
/// * `kind` - Classified [`CobolOpKind`].
/// * `op` - Source operation reference.
/// * `cursor` - Active function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn rewrite_cobol_dialect_op<'a, 'ctx>(
    kind: CobolOpKind,
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &mut LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    match kind {
        CobolOpKind::FieldView => lower_field_view(op, cursor, rw, loc),
        CobolOpKind::Subscript => lower_subscript_op(op, cursor, rw, loc),
        CobolOpKind::RefMod => lower_refmod_op(op, cursor, rw, loc),
        CobolOpKind::PerformRange => lower_perform_range_cfg(op, cursor, rw, loc),
        CobolOpKind::Alter => lower_alter_cfg(op, cursor, rw, loc),
        CobolOpKind::Loop => lower_loop_cfg(op, cursor, rw, loc),
        CobolOpKind::Sentence => lower_sentence_cfg(op, cursor, rw, loc),
        CobolOpKind::FlowExit => lower_flow_exit_cfg(op, cursor, rw, loc),
        CobolOpKind::Goto => lower_goto_cfg(op, cursor, rw, loc),
        CobolOpKind::RtDispatch => lower_rt_dispatch_op(op, cursor, rw, loc),
        CobolOpKind::GetReturnCode => lower_get_rc(op, cursor, rw, loc),
        CobolOpKind::SetReturnCode => lower_set_rc(op, cursor, rw, loc),
        CobolOpKind::Init => lower_init_op(op, cursor, rw, loc),
        CobolOpKind::Move => lower_move_op(op, cursor, rw, loc),
        CobolOpKind::Arith => lower_arith_op(op, cursor, loc),
        CobolOpKind::Cmp => lower_cmp_op(op, cursor, rw, loc),
        CobolOpKind::Trim => lower_trim_op(op, cursor, rw, loc),
        CobolOpKind::Display => lower_display_op(op, cursor, rw, loc),
        CobolOpKind::Call => lower_call_op(op, cursor, rw, loc),
        CobolOpKind::StopRun => lower_stop_run_op(op, cursor, rw, loc),
        CobolOpKind::Goback => lower_goback_op(op, cursor, rw, loc),
        _ => {}
    }
}

/// Lowers `cobol.field_view` to `llvm.mlir.addressof` (+ `llvm.getelementptr` when `byte_offset > 0`).
///
/// # Arguments
///
/// * `op` - `cobol.field_view` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_field_view<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let storage_sym = read_str_attr(op, "storage_sym");
    if let Some(fn_sym) = storage_sym.strip_prefix("__func_addr:") {
        let i32_ty = IntegerType::new(rw.ctx.raw(), 32).into();
        let fn_ty = FunctionType::new(rw.ctx.raw(), &[], &[i32_ty]);
        let ptr_ty = llvm_ptr_type(rw.ctx);
        let fn_val = append_res0(
            cursor.current_block,
            func::constant(
                rw.ctx.raw(),
                FlatSymbolRefAttribute::new(rw.ctx.raw(), fn_sym),
                fn_ty,
                loc,
            ),
            cursor.null_ptr,
        );
        let cast_op = build_op(
            OperationBuilder::new("builtin.unrealized_conversion_cast", loc)
                .add_operands(&[fn_val])
                .add_results(&[ptr_ty]),
            loc,
        );
        let ptr_val = append_res0(cursor.current_block, cast_op, cursor.null_ptr);
        record_ssa_results(op, &[ptr_val], cursor);
        return;
    }
    let byte_offset = read_i64_attr(op, "byte_offset");
    let base_ptr = emit_addressof(
        rw.ctx,
        cursor.current_block,
        &storage_sym,
        cursor.null_ptr,
        loc,
    );
    let final_ptr = if byte_offset > 0 {
        let i8_ty = IntegerType::new(rw.ctx.raw(), 8).into();
        let ptr_ty = llvm_ptr_type(rw.ctx);
        let idx = i32::try_from(byte_offset).unwrap_or(0);
        let gep = llvm::get_element_ptr(
            rw.ctx.raw(),
            base_ptr,
            DenseI32ArrayAttribute::new(rw.ctx.raw(), &[idx]),
            i8_ty,
            ptr_ty,
            loc,
        );
        append_res0(cursor.current_block, gep, base_ptr)
    } else {
        base_ptr
    };
    record_ssa_results(op, &[final_ptr], cursor);
}

/// Lowers `cobol.get_return_code` to an `i32` load from `@<rc_slot_sym>`.
///
/// # Arguments
///
/// * `op` - `cobol.get_return_code` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_get_rc<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let i32_ty = IntegerType::new(rw.ctx.raw(), 32).into();
    let ptr = emit_addressof(
        rw.ctx,
        cursor.current_block,
        &cursor.rc_slot_sym,
        cursor.null_ptr,
        loc,
    );
    let loaded = append_res0(
        cursor.current_block,
        llvm::load(rw.ctx.raw(), ptr, i32_ty, loc, LoadStoreOptions::new()),
        cursor.zero_i32,
    );
    record_ssa_results(op, &[loaded], cursor);
}

/// Lowers `cobol.set_return_code` to an `i32` store into `@<rc_slot_sym>`.
///
/// # Arguments
///
/// * `op` - `cobol.set_return_code` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_set_rc<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let val = op_in(op, 0, cursor, cursor.zero_i32);
    let ptr = emit_addressof(
        rw.ctx,
        cursor.current_block,
        &cursor.rc_slot_sym,
        cursor.null_ptr,
        loc,
    );
    let _ = cursor.current_block.append_operation(llvm::store(
        rw.ctx.raw(),
        val,
        ptr,
        loc,
        LoadStoreOptions::new(),
    ));
}

/// Emits `llvm.intr.memset(dst, fill_byte, byte_len, isVolatile=false)`.
///
/// # Arguments
///
/// * `rw` - Module rewrite state.
/// * `block` - Target block.
/// * `dst_ptr` - Destination `!llvm.ptr`.
/// * `fill_i64` - Fill byte value (`0..=255`).
/// * `byte_len` - Byte length (`i64`).
/// * `fallback` - Fallback value.
/// * `loc` - Source location.
fn emit_memset<'a, 'ctx>(
    rw: &LlvmRewriteState<'ctx>,
    block: BlockRef<'ctx, 'a>,
    dst_ptr: Value<'ctx, 'a>,
    fill_i64: i64,
    byte_len: i64,
    fallback: Value<'ctx, 'a>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let i8_ty = IntegerType::new(raw, 8).into();
    let i64_ty = IntegerType::new(raw, 64).into();
    let fill_val = append_res0(
        block,
        arith::constant(
            raw,
            IntegerAttribute::new(i8_ty, fill_i64 & 0xFF).into(),
            loc,
        ),
        fallback,
    );
    let len_val = append_res0(
        block,
        arith::constant(raw, IntegerAttribute::new(i64_ty, byte_len).into(), loc),
        fallback,
    );
    let op = build_op(
        OperationBuilder::new("llvm.intr.memset", loc)
            .add_operands(&[dst_ptr, fill_val, len_val])
            .add_attributes(&[(
                Identifier::new(raw, "isVolatile"),
                BoolAttribute::new(raw, false).into(),
            )]),
        loc,
    );
    let _ = block.append_operation(op);
}

/// Lowers `cobol.init` to `llvm.intr.memset`.
///
/// # Arguments
///
/// * `op` - `cobol.init` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_init_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let dst_ptr = op_in(op, 0, cursor, cursor.null_ptr);
    let byte_len = read_i64_attr(op, "byte_len").max(1);
    let fill_val = read_i64_attr(op, "fill_byte") & 0xFF;
    emit_memset(
        rw,
        cursor.current_block,
        dst_ptr,
        fill_val,
        byte_len,
        cursor.zero_i32,
        loc,
    );
}

/// Lowers `cobol.move` (`i32_to_be`, `i32_to_ne`, or `bytes_copy_pad`).
///
/// # Arguments
///
/// * `op` - `cobol.move` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_move_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let encoding = read_str_attr(op, "encoding");
    let src_len = read_i64_attr(op, "src_byte_len");
    let dst_len = read_i64_attr(op, "dst_byte_len").max(1);
    let dst_ptr = op_in(op, 1, cursor, cursor.null_ptr);

    if encoding == "bytes_copy_pad" {
        let src_ptr = op_in(op, 0, cursor, cursor.null_ptr);
        emit_memset(
            rw,
            cursor.current_block,
            dst_ptr,
            i64::from(b' '),
            dst_len,
            cursor.zero_i32,
            loc,
        );
        let copy_len = src_len.min(dst_len);
        if copy_len > 0 {
            let i64_ty = IntegerType::new(raw, 64).into();
            let len_val = append_res0(
                cursor.current_block,
                arith::constant(raw, IntegerAttribute::new(i64_ty, copy_len).into(), loc),
                cursor.zero_i32,
            );
            let memcpy_op = build_op(
                OperationBuilder::new("llvm.intr.memcpy", loc)
                    .add_operands(&[dst_ptr, src_ptr, len_val])
                    .add_attributes(&[(
                        Identifier::new(raw, "isVolatile"),
                        BoolAttribute::new(raw, false).into(),
                    )]),
                loc,
            );
            let _ = cursor.current_block.append_operation(memcpy_op);
        }
        return;
    }

    let src_i32 = op_in(op, 0, cursor, cursor.zero_i32);
    let bits = u32::try_from(dst_len.saturating_mul(8)).unwrap_or(32);
    let target_int_ty: Type<'_> = IntegerType::new(raw, bits).into();

    let narrowed = if dst_len < 4 {
        append_res0(
            cursor.current_block,
            arith::trunci(src_i32, target_int_ty, loc),
            src_i32,
        )
    } else {
        src_i32
    };

    #[cfg(target_endian = "little")]
    let stored_val = if encoding == "i32_to_be" && dst_len > 1 {
        append_res0(
            cursor.current_block,
            llvm::intr_bswap(narrowed, target_int_ty, loc),
            narrowed,
        )
    } else {
        narrowed
    };
    #[cfg(not(target_endian = "little"))]
    let stored_val = narrowed;

    let _ = cursor.current_block.append_operation(llvm::store(
        raw,
        stored_val,
        dst_ptr,
        loc,
        LoadStoreOptions::new(),
    ));
}

/// Lowers `cobol.arith` to `arith.addi` / `arith.subi` / `arith.muli` / `arith.divsi`.
///
/// # Arguments
///
/// * `op` - `cobol.arith` operation.
/// * `cursor` - Function cursor.
/// * `loc` - Source location.
fn lower_arith_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    loc: Location<'ctx>,
) {
    let lhs = op_in(op, 0, cursor, cursor.zero_i32);
    let rhs = op_in(op, 1, cursor, cursor.zero_i32);
    let kind = read_str_attr(op, "arith_kind");
    let arith_op = match kind.as_str() {
        "sub" => arith::subi(lhs, rhs, loc),
        "mul" => arith::muli(lhs, rhs, loc),
        "div" => arith::divsi(lhs, rhs, loc),
        _ => arith::addi(lhs, rhs, loc),
    };
    let res = append_res0(cursor.current_block, arith_op, cursor.zero_i32);
    record_ssa_results(op, &[res], cursor);
}

/// Lowers `cobol.cmp` to `arith.cmpi`.
///
/// # Arguments
///
/// * `op` - `cobol.cmp` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_cmp_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let lhs = op_in(op, 0, cursor, cursor.zero_i32);
    let rhs = op_in(op, 1, cursor, cursor.zero_i32);
    let rel = read_str_attr(op, "rel_op");
    let pred = match rel.as_str() {
        "ne" => CmpiPredicate::Ne,
        "lt" => CmpiPredicate::Slt,
        "le" => CmpiPredicate::Sle,
        "gt" => CmpiPredicate::Sgt,
        "ge" => CmpiPredicate::Sge,
        _ => CmpiPredicate::Eq,
    };
    let cmp_op = arith::cmpi(rw.ctx.raw(), pred, lhs, rhs, loc);
    let res = append_res0(cursor.current_block, cmp_op, cursor.zero_i32);
    record_ssa_results(op, &[res], cursor);
}

/// Lowers `cobol.trim` into stack slots + `@coblang_rt_trim` call.
///
/// # Arguments
///
/// * `op` - `cobol.trim` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_trim_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let ptr_ty = llvm_ptr_type(rw.ctx);
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let i64_ty: Type<'_> = IntegerType::new(raw, 64).into();

    let src_ptr = op_in(op, 0, cursor, cursor.null_ptr);
    let src_len = op_in(op, 1, cursor, cursor.zero_i32);
    let dir = i32::try_from(read_i64_attr(op, "direction")).unwrap_or(0);

    let one_i64 = append_res0(
        cursor.current_block,
        arith::constant(raw, IntegerAttribute::new(i64_ty, 1).into(), loc),
        cursor.zero_i32,
    );
    let mode_val = append_res0(
        cursor.current_block,
        arith::constant(
            raw,
            IntegerAttribute::new(i32_ty, i64::from(dir)).into(),
            loc,
        ),
        cursor.zero_i32,
    );

    let out_ptr_slot = append_res0(
        cursor.current_block,
        llvm::alloca(
            raw,
            one_i64,
            ptr_ty,
            loc,
            AllocaOptions::new().elem_type(Some(TypeAttribute::new(ptr_ty))),
        ),
        cursor.null_ptr,
    );
    let out_len_slot = append_res0(
        cursor.current_block,
        llvm::alloca(
            raw,
            one_i64,
            ptr_ty,
            loc,
            AllocaOptions::new().elem_type(Some(TypeAttribute::new(i64_ty))),
        ),
        cursor.null_ptr,
    );

    let _ = cursor.current_block.append_operation(func::call(
        raw,
        FlatSymbolRefAttribute::new(raw, "coblang_rt_trim"),
        &[src_ptr, src_len, mode_val, out_ptr_slot, out_len_slot],
        &[i32_ty],
        loc,
    ));

    let trimmed_ptr = append_res0(
        cursor.current_block,
        llvm::load(raw, out_ptr_slot, ptr_ty, loc, LoadStoreOptions::new()),
        cursor.null_ptr,
    );
    let trimmed_len = append_res0(
        cursor.current_block,
        llvm::load(raw, out_len_slot, i64_ty, loc, LoadStoreOptions::new()),
        cursor.zero_i32,
    );
    record_ssa_results(op, &[trimmed_ptr, trimmed_len], cursor);
}

/// Allocates a typed stack array `llvm.alloca` of `elem_ty[count_val]`.
///
/// # Arguments
///
/// * `rw` - Module rewrite state.
/// * `cursor` - Function cursor.
/// * `count_val` - Element count SSA `i64`.
/// * `elem_ty` - Element MLIR [`Type`].
/// * `loc` - Source location.
fn emit_stack_array<'a, 'ctx>(
    rw: &LlvmRewriteState<'ctx>,
    cursor: &FnCursor<'a, 'ctx>,
    count_val: Value<'ctx, 'a>,
    elem_ty: Type<'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'a> {
    let ptr_ty = llvm_ptr_type(rw.ctx);
    append_res0(
        cursor.current_block,
        llvm::alloca(
            rw.ctx.raw(),
            count_val,
            ptr_ty,
            loc,
            AllocaOptions::new().elem_type(Some(TypeAttribute::new(elem_ty))),
        ),
        cursor.null_ptr,
    )
}

/// Stores `val` at `arr_base[idx]` with element type `elem_ty`.
///
/// # Arguments
///
/// * `rw` - Module rewrite state.
/// * `block` - Target block.
/// * `arr_base` - Array base `!llvm.ptr`.
/// * `idx` - Constant element index.
/// * `elem_ty` - Element MLIR [`Type`].
/// * `val` - Value to store.
/// * `loc` - Source location.
fn store_array_elem<'a, 'ctx>(
    rw: &LlvmRewriteState<'ctx>,
    block: BlockRef<'ctx, 'a>,
    arr_base: Value<'ctx, 'a>,
    idx: i32,
    elem_ty: Type<'ctx>,
    val: Value<'ctx, 'a>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let ptr_ty = llvm_ptr_type(rw.ctx);
    let idx_attr = DenseI32ArrayAttribute::new(raw, &[idx]);
    let slot = append_res0(
        block,
        llvm::get_element_ptr(raw, arr_base, idx_attr, elem_ty, ptr_ty, loc),
        arr_base,
    );
    let _ = block.append_operation(llvm::store(raw, val, slot, loc, LoadStoreOptions::new()));
}

/// Stack-allocated parallel arrays backing a `coblang_rt_display` invocation.
struct DisplayStackArrays<'ctx, 'a> {
    /// Stack array of operand `!llvm.ptr` pointers.
    ptrs: Value<'ctx, 'a>,
    /// Stack array of operand `i64` byte lengths.
    lens: Value<'ctx, 'a>,
    /// Stack array of operand `i8` display kind tags.
    kinds: Value<'ctx, 'a>,
    /// Stack array of operand `i16` decimal scales.
    scales: Value<'ctx, 'a>,
}

/// Populates element `idx` across all four `DisplayStackArrays` for `op`.
///
/// # Arguments
///
/// * `op` - Source `cobol.display` operation.
/// * `idx` - Zero-based operand item index.
/// * `meta` - Tuple `(kind_tag, scale_val)` for item `idx`.
/// * `arrays` - Destination [`DisplayStackArrays`].
/// * `cursor` - Active function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn populate_display_slot<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    idx: usize,
    meta: (i64, i64),
    arrays: &DisplayStackArrays<'ctx, 'a>,
    cursor: &FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let ptr_ty = llvm_ptr_type(rw.ctx);
    let i8_ty: Type<'_> = IntegerType::new(raw, 8).into();
    let i16_ty: Type<'_> = IntegerType::new(raw, 16).into();
    let i64_ty: Type<'_> = IntegerType::new(raw, 64).into();

    let p_val = op_in(op, idx * 2, cursor, cursor.null_ptr);
    let l_val = op_in(op, idx * 2 + 1, cursor, cursor.zero_i32);
    let idx_i32 = i32::try_from(idx).unwrap_or(0);

    let k_val = append_res0(
        cursor.current_block,
        arith::constant(raw, IntegerAttribute::new(i8_ty, meta.0).into(), loc),
        cursor.zero_i32,
    );
    let s_val = append_res0(
        cursor.current_block,
        arith::constant(raw, IntegerAttribute::new(i16_ty, meta.1).into(), loc),
        cursor.zero_i32,
    );

    let blk = cursor.current_block;
    store_array_elem(rw, blk, arrays.ptrs, idx_i32, ptr_ty, p_val, loc);
    store_array_elem(rw, blk, arrays.lens, idx_i32, i64_ty, l_val, loc);
    store_array_elem(rw, blk, arrays.kinds, idx_i32, i8_ty, k_val, loc);
    store_array_elem(rw, blk, arrays.scales, idx_i32, i16_ty, s_val, loc);
}

/// Lowers `cobol.display` into stack descriptor arrays + `@coblang_rt_display` call.
///
/// # Arguments
///
/// * `op` - `cobol.display` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_display_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let ptr_ty = llvm_ptr_type(rw.ctx);
    let i8_ty: Type<'_> = IntegerType::new(raw, 8).into();
    let i16_ty: Type<'_> = IntegerType::new(raw, 16).into();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let i64_ty: Type<'_> = IntegerType::new(raw, 64).into();

    let device = read_i64_attr(op, "device");
    let advancing = read_i64_attr(op, "advancing");
    let kinds = read_i32_array_attr(op, "item_kinds");
    let scales = read_i32_array_attr(op, "item_scales");
    let count = op.operand_count() / 2;
    let alloc_count = i64::try_from(count.max(1)).unwrap_or(1);

    let alloc_n = append_res0(
        cursor.current_block,
        arith::constant(raw, IntegerAttribute::new(i64_ty, alloc_count).into(), loc),
        cursor.zero_i32,
    );

    let arrays = DisplayStackArrays {
        ptrs: emit_stack_array(rw, cursor, alloc_n, ptr_ty, loc),
        lens: emit_stack_array(rw, cursor, alloc_n, i64_ty, loc),
        kinds: emit_stack_array(rw, cursor, alloc_n, i8_ty, loc),
        scales: emit_stack_array(rw, cursor, alloc_n, i16_ty, loc),
    };

    for idx in 0..count {
        let k_raw = i64::from(kinds.get(idx).copied().unwrap_or(0));
        let s_raw = i64::from(scales.get(idx).copied().unwrap_or(0));
        populate_display_slot(op, idx, (k_raw, s_raw), &arrays, cursor, rw, loc);
    }

    let dev_val = append_res0(
        cursor.current_block,
        arith::constant(raw, IntegerAttribute::new(i32_ty, device).into(), loc),
        cursor.zero_i32,
    );
    let adv_val = append_res0(
        cursor.current_block,
        arith::constant(raw, IntegerAttribute::new(i32_ty, advancing).into(), loc),
        cursor.zero_i32,
    );
    let cnt_val = append_res0(
        cursor.current_block,
        arith::constant(
            raw,
            IntegerAttribute::new(i64_ty, i64::try_from(count).unwrap_or(0)).into(),
            loc,
        ),
        cursor.zero_i32,
    );

    let _ = cursor.current_block.append_operation(func::call(
        raw,
        FlatSymbolRefAttribute::new(raw, "coblang_rt_display"),
        &[
            dev_val,
            adv_val,
            arrays.ptrs,
            arrays.lens,
            arrays.kinds,
            arrays.scales,
            cnt_val,
        ],
        &[i32_ty],
        loc,
    ));
}

/// Lowers `cobol.call` (static intra-module call or dynamic `coblang_rt_call_dynamic` call).
///
/// # Arguments
///
/// * `op` - `cobol.call` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_call_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &mut LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let i64_ty: Type<'_> = IntegerType::new(raw, 64).into();

    let dispatch = read_str_attr(op, "dispatch");
    let callee = read_str_attr(op, "callee");
    let rc_ptr = emit_addressof(
        rw.ctx,
        cursor.current_block,
        &cursor.rc_slot_sym,
        cursor.null_ptr,
        loc,
    );

    if dispatch == "static" {
        let _ = cursor.current_block.append_operation(func::call(
            raw,
            FlatSymbolRefAttribute::new(raw, "coblang_rt_enter_call"),
            &[],
            &[],
            loc,
        ));
        let callee_rc = append_res0(
            cursor.current_block,
            func::call(
                raw,
                FlatSymbolRefAttribute::new(raw, &callee),
                &[],
                &[i32_ty],
                loc,
            ),
            cursor.zero_i32,
        );
        let _ = cursor.current_block.append_operation(func::call(
            raw,
            FlatSymbolRefAttribute::new(raw, "coblang_rt_leave_call"),
            &[],
            &[],
            loc,
        ));
        let _ = cursor.current_block.append_operation(llvm::store(
            raw,
            callee_rc,
            rc_ptr,
            loc,
            LoadStoreOptions::new(),
        ));
    } else {
        let id = rw.dyn_name_counter;
        rw.dyn_name_counter = rw.dyn_name_counter.saturating_add(1);
        let name_sym = format!("__coblang_call_name_{id}");
        let name_bytes = callee.as_bytes();
        let backing = if name_bytes.is_empty() {
            b" "
        } else {
            name_bytes
        };
        emit_raw_byte_global(
            rw,
            &name_sym,
            backing,
            1,
            1,
            GlobalMutability::ReadOnly,
            loc,
        );

        let name_ptr = emit_addressof(
            rw.ctx,
            cursor.current_block,
            &name_sym,
            cursor.null_ptr,
            loc,
        );
        let name_len = append_res0(
            cursor.current_block,
            arith::constant(
                raw,
                IntegerAttribute::new(i64_ty, i64::try_from(name_bytes.len()).unwrap_or(0)).into(),
                loc,
            ),
            cursor.zero_i32,
        );
        let _ = cursor.current_block.append_operation(func::call(
            raw,
            FlatSymbolRefAttribute::new(raw, "coblang_rt_call_dynamic"),
            &[name_ptr, name_len, rc_ptr],
            &[i32_ty],
            loc,
        ));
    }
}

/// Lowers `cobol.stop_run` into `@coblang_rt_prepare_stop_run` + `@exit` + `cf.br ^exit`.
///
/// # Arguments
///
/// * `op` - `cobol.stop_run` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_stop_run_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let status = op_in(op, 0, cursor, cursor.zero_i32);

    let prepared = append_res0(
        cursor.current_block,
        func::call(
            raw,
            FlatSymbolRefAttribute::new(raw, "coblang_rt_prepare_stop_run"),
            &[status],
            &[i32_ty],
            loc,
        ),
        status,
    );
    let _ = cursor.current_block.append_operation(func::call(
        raw,
        FlatSymbolRefAttribute::new(raw, "exit"),
        &[prepared],
        &[],
        loc,
    ));
    let _ = cursor
        .current_block
        .append_operation(cf::br(&cursor.exit_block, &[], loc));
    cursor.termination = BlockTermination::Terminated;
}

/// Sign-extends `val` to `i64` if its integer bitwidth is smaller than 64 bits.
///
/// # Arguments
///
/// * `raw` - MLIR context.
/// * `val` - Integer SSA value (`i32` or `i64`).
/// * `i64_ty` - Target `i64` type.
/// * `block` - Current insertion block.
/// * `fallback` - Dominated `i64` fallback.
/// * `loc` - Source location.
fn coerce_to_i64<'a, 'ctx>(
    val: Value<'ctx, 'a>,
    i64_ty: Type<'ctx>,
    block: BlockRef<'ctx, 'a>,
    fallback: Value<'ctx, 'a>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'a> {
    if val.r#type() == i64_ty {
        val
    } else {
        append_res0(block, arith::extsi(val, i64_ty, loc), fallback)
    }
}

/// Lowers `cobol.subscript` to 1-based stride offset calculation + `llvm.getelementptr`.
///
/// # Arguments
///
/// * `op` - `cobol.subscript` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_subscript_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let i64_ty: Type<'_> = IntegerType::new(raw, 64).into();
    let i8_ty: Type<'_> = IntegerType::new(raw, 8).into();
    let ptr_ty = llvm_ptr_type(rw.ctx);

    let base_ptr = op_in(op, 0, cursor, cursor.null_ptr);
    let strides = read_i32_array_attr(op, "strides");
    let one_64 = append_res0(
        cursor.current_block,
        arith::constant(raw, IntegerAttribute::new(i64_ty, 1).into(), loc),
        cursor.zero_i32,
    );
    let mut total_off = append_res0(
        cursor.current_block,
        arith::constant(raw, IntegerAttribute::new(i64_ty, 0).into(), loc),
        cursor.zero_i32,
    );

    for (dim_idx, &stride_i32) in strides.iter().enumerate() {
        let raw_idx = op_in(op, dim_idx + 1, cursor, one_64);
        let idx_val = coerce_to_i64(raw_idx, i64_ty, cursor.current_block, one_64, loc);
        let zero_based = append_res0(
            cursor.current_block,
            arith::subi(idx_val, one_64, loc),
            one_64,
        );
        let stride_64 = append_res0(
            cursor.current_block,
            arith::constant(
                raw,
                IntegerAttribute::new(i64_ty, i64::from(stride_i32)).into(),
                loc,
            ),
            one_64,
        );
        let scaled = append_res0(
            cursor.current_block,
            arith::muli(zero_based, stride_64, loc),
            one_64,
        );
        total_off = append_res0(
            cursor.current_block,
            arith::addi(total_off, scaled, loc),
            one_64,
        );
    }

    let gep = llvm::get_element_ptr_dynamic(raw, base_ptr, &[total_off], i8_ty, ptr_ty, loc);
    let elem_ptr = append_res0(cursor.current_block, gep, base_ptr);
    record_ssa_results(op, &[elem_ptr], cursor);
}

/// Lowers `cobol.refmod` to `(ptr + max(0, off - 1), opt_len.unwrap_or(max(0, base_len - (off - 1))))`.
///
/// # Arguments
///
/// * `op` - `cobol.refmod` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_refmod_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let i64_ty: Type<'_> = IntegerType::new(raw, 64).into();
    let ptr_ty = llvm_ptr_type(rw.ctx);

    let z64 = append_res0(
        cursor.current_block,
        arith::constant(raw, IntegerAttribute::new(i64_ty, 0).into(), loc),
        cursor.zero_i32,
    );
    let one_64 = append_res0(
        cursor.current_block,
        arith::constant(raw, IntegerAttribute::new(i64_ty, 1).into(), loc),
        cursor.zero_i32,
    );

    let view_ptr = op_in(op, 0, cursor, cursor.null_ptr);
    let base_len_raw = op_in(op, 1, cursor, z64);
    let base_len = coerce_to_i64(base_len_raw, i64_ty, cursor.current_block, z64, loc);
    let one_based_raw = op_in(op, 2, cursor, one_64);
    let one_based = coerce_to_i64(one_based_raw, i64_ty, cursor.current_block, one_64, loc);
    let has_len = op.operand_count() >= 4;
    let char_width = read_str_attr(op, "char_width");

    if char_width == "2" {
        let opt_char_len = has_len.then(|| {
            let len_raw = op_in(op, 3, cursor, z64);
            coerce_to_i64(len_raw, i64_ty, cursor.current_block, z64, loc)
        });
        let (sliced_ptr, final_len) = crate::lower_m17::lower_utf16_refmod_llvm(
            raw,
            cursor.current_block,
            (view_ptr, base_len, one_based, opt_char_len),
            rw.ctx,
            z64,
            loc,
        );
        record_ssa_results(op, &[sliced_ptr, final_len], cursor);
        return;
    }

    if char_width == "utf8" {
        let opt_char_cnt = has_len.then(|| {
            let len_raw = op_in(op, 3, cursor, z64);
            coerce_to_i64(len_raw, i64_ty, cursor.current_block, z64, loc)
        });
        let (sliced_ptr, final_len) = lower_utf8_refmod_llvm(
            raw,
            cursor.current_block,
            (view_ptr, base_len, one_based, opt_char_cnt),
            ptr_ty,
            z64,
            loc,
        );
        record_ssa_results(op, &[sliced_ptr, final_len], cursor);
        return;
    }

    let req_len_opt = has_len.then(|| {
        let len_raw = op_in(op, 3, cursor, z64);
        coerce_to_i64(len_raw, i64_ty, cursor.current_block, z64, loc)
    });
    let (sliced_ptr, final_len) = clamp_byte_refmod_slice_llvm(
        raw,
        cursor.current_block,
        (view_ptr, base_len, one_based, req_len_opt),
        ptr_ty,
        z64,
        loc,
    );
    record_ssa_results(op, &[sliced_ptr, final_len], cursor);
}

/// Clamps 1-based byte-offset reference modification `(view_ptr, base_len, one_based, req_len_opt)`.
///
/// # Arguments
///
/// * `raw` - Raw Melior MLIR [`Context`].
/// * `block` - Target LLVM block.
/// * `slice_args` - `(view_ptr, base_len, one_based, req_len_opt)` operands.
/// * `ptr_ty` - Opaque `!llvm.ptr` type.
/// * `z64` - Fallback zero `i64` SSA value.
/// * `loc` - Source location.
fn clamp_byte_refmod_slice_llvm<'ctx, 'a>(
    raw: &'ctx melior::Context,
    block: BlockRef<'ctx, 'a>,
    slice_args: (
        Value<'ctx, 'a>,
        Value<'ctx, 'a>,
        Value<'ctx, 'a>,
        Option<Value<'ctx, 'a>>,
    ),
    ptr_ty: Type<'ctx>,
    z64: Value<'ctx, 'a>,
    loc: Location<'ctx>,
) -> (Value<'ctx, 'a>, Value<'ctx, 'a>) {
    let (view_ptr, base_len, one_based, req_len_opt) = slice_args;
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();
    let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();
    let one_64 = append_res0(
        block,
        arith::constant(raw, IntegerAttribute::new(i64_ty, 1).into(), loc),
        z64,
    );
    let raw_off = append_res0(block, arith::subi(one_based, one_64, loc), z64);
    let is_neg = append_res0(
        block,
        arith::cmpi(raw, CmpiPredicate::Slt, raw_off, z64, loc),
        z64,
    );
    let non_neg_off = append_res0(block, arith::select(is_neg, z64, raw_off, loc), z64);
    let over_base = append_res0(
        block,
        arith::cmpi(raw, CmpiPredicate::Sgt, non_neg_off, base_len, loc),
        z64,
    );
    let clamped_off = append_res0(
        block,
        arith::select(over_base, base_len, non_neg_off, loc),
        z64,
    );
    let gep = llvm::get_element_ptr_dynamic(raw, view_ptr, &[clamped_off], i8_ty, ptr_ty, loc);
    let sliced_ptr = append_res0(block, gep, view_ptr);

    let rem = append_res0(block, arith::subi(base_len, clamped_off, loc), z64);
    let rem_neg = append_res0(
        block,
        arith::cmpi(raw, CmpiPredicate::Slt, rem, z64, loc),
        z64,
    );
    let max_rem = append_res0(block, arith::select(rem_neg, z64, rem, loc), z64);
    let final_len = if let Some(req_len) = req_len_opt {
        let len_neg = append_res0(
            block,
            arith::cmpi(raw, CmpiPredicate::Slt, req_len, z64, loc),
            z64,
        );
        let non_neg_len = append_res0(block, arith::select(len_neg, z64, req_len, loc), z64);
        let len_over = append_res0(
            block,
            arith::cmpi(raw, CmpiPredicate::Sgt, non_neg_len, max_rem, loc),
            z64,
        );
        append_res0(
            block,
            arith::select(len_over, max_rem, non_neg_len, loc),
            z64,
        )
    } else {
        max_rem
    };
    (sliced_ptr, final_len)
}

/// Lowers `PIC U` (`UTF-8` character-indexed) reference modification to runtime C-ABI calls.
///
/// # Arguments
///
/// * `raw` - Raw Melior MLIR [`Context`].
/// * `block` - Target LLVM block.
/// * `slice_args` - `(view_ptr, base_len, one_based, opt_char_cnt)` operands.
/// * `ptr_ty` - Opaque `!llvm.ptr` type.
/// * `z64` - Fallback zero `i64` SSA value.
/// * `loc` - Source location.
fn lower_utf8_refmod_llvm<'ctx, 'a>(
    raw: &'ctx melior::Context,
    block: BlockRef<'ctx, 'a>,
    slice_args: (
        Value<'ctx, 'a>,
        Value<'ctx, 'a>,
        Value<'ctx, 'a>,
        Option<Value<'ctx, 'a>>,
    ),
    ptr_ty: Type<'ctx>,
    z64: Value<'ctx, 'a>,
    loc: Location<'ctx>,
) -> (Value<'ctx, 'a>, Value<'ctx, 'a>) {
    let (view_ptr, base_len, one_based, opt_char_cnt) = slice_args;
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();
    let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();
    let off_call = func::call(
        raw,
        FlatSymbolRefAttribute::new(raw, "coblang_rt_utf8_refmod_off"),
        &[view_ptr, base_len, one_based],
        &[i64_ty],
        loc,
    );
    let byte_off = append_res0(block, off_call, z64);
    let gep = llvm::get_element_ptr_dynamic(raw, view_ptr, &[byte_off], i8_ty, ptr_ty, loc);
    let sliced_ptr = append_res0(block, gep, view_ptr);
    let char_cnt = opt_char_cnt.unwrap_or_else(|| {
        append_res0(
            block,
            arith::constant(raw, IntegerAttribute::new(i64_ty, -1).into(), loc),
            z64,
        )
    });
    let len_call = func::call(
        raw,
        FlatSymbolRefAttribute::new(raw, "coblang_rt_utf8_refmod_len"),
        &[view_ptr, base_len, byte_off, char_cnt],
        &[i64_ty],
        loc,
    );
    let final_len = append_res0(block, len_call, z64);
    (sliced_ptr, final_len)
}

/// Lowers `cobol.rt_dispatch` directly to `func.call @<callee>(operands...)`, branching to `@exit(1)` on fatal bound traps.
///
/// # Arguments
///
/// * `op` - `cobol.rt_dispatch` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_rt_dispatch_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let callee = read_str_attr(op, "callee");
    let mut mapped_ops = Vec::with_capacity(op.operand_count());
    for idx in 0..op.operand_count() {
        mapped_ops.push(op_in(op, idx, cursor, cursor.zero_i32));
    }
    let res_tys: Vec<_> = (0..op.result_count())
        .filter_map(|r_idx| op.result(r_idx).ok().map(|r| r.r#type()))
        .collect();
    let call_op = func::call(
        raw,
        FlatSymbolRefAttribute::new(raw, &callee),
        &mapped_ops,
        &res_tys,
        loc,
    );
    if res_tys.is_empty() {
        let _ = cursor.current_block.append_operation(call_op);
    } else {
        let rv = append_res0(cursor.current_block, call_op, cursor.zero_i32);
        record_ssa_results(op, &[rv], cursor);
        if matches!(
            callee.as_str(),
            "coblang_rt_check_odo"
                | "coblang_rt_check_sub"
                | "coblang_rt_check_refmod"
                | "coblang_rt_check_based"
        ) {
            let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
            let one_i32 = append_res0(
                cursor.current_block,
                arith::constant(raw, IntegerAttribute::new(i32_ty, 1).into(), loc),
                cursor.zero_i32,
            );
            let is_fatal_exit = append_res0(
                cursor.current_block,
                arith::cmpi(raw, CmpiPredicate::Eq, rv, one_i32, loc),
                cursor.zero_i32,
            );
            let abort_blk = cursor.region.append_block(Block::new(&[]));
            let cont_blk = cursor.region.append_block(Block::new(&[]));
            let _ = cursor.current_block.append_operation(cf::cond_br(
                raw,
                is_fatal_exit,
                &abort_blk,
                &cont_blk,
                &[],
                &[],
                loc,
            ));
            let _ = abort_blk.append_operation(func::call(
                raw,
                FlatSymbolRefAttribute::new(raw, "exit"),
                &[one_i32],
                &[],
                loc,
            ));
            let _ = abort_blk.append_operation(cf::br(&cursor.exit_block, &[], loc));
            cursor.current_block = cont_blk;
        }
    }
}

/// Lowers `cobol.goback` (`para_fallthrough` normal `0`, `goback` unconditional `-1`, and `exit_program` subprogram return).
///
/// # Arguments
///
/// * `op` - `cobol.goback` operation.
/// * `cursor` - Function cursor.
/// * `rw` - Module rewrite state.
/// * `loc` - Source location.
fn lower_goback_op<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let mode = read_str_attr(op, "mode");
    if mode == "para_fallthrough" {
        let _ = cursor
            .current_block
            .append_operation(cf::br(&cursor.exit_block, &[], loc));
        cursor.termination = BlockTermination::Terminated;
    } else if mode == "exit_program" {
        let is_sub_i32 = append_res0(
            cursor.current_block,
            func::call(
                raw,
                FlatSymbolRefAttribute::new(raw, "coblang_rt_is_subprogram"),
                &[],
                &[i32_ty],
                loc,
            ),
            cursor.zero_i32,
        );
        let is_sub_i1 = append_res0(
            cursor.current_block,
            arith::cmpi(raw, CmpiPredicate::Ne, is_sub_i32, cursor.zero_i32, loc),
            cursor.zero_i32,
        );
        let ret_sub_blk = cursor.region.append_block(Block::new(&[]));
        let cont_blk = cursor.region.append_block(Block::new(&[]));
        let _ = cursor.current_block.append_operation(cf::cond_br(
            raw,
            is_sub_i1,
            &ret_sub_blk,
            &cont_blk,
            &[],
            &[],
            loc,
        ));
        if let Some(slot) = cursor.para_goback_slot {
            let neg_one = append_res0(
                ret_sub_blk,
                arith::constant(
                    raw,
                    IntegerAttribute::new(i32_ty, i64::from(ACTION_GOBACK)).into(),
                    loc,
                ),
                cursor.zero_i32,
            );
            let _ = ret_sub_blk.append_operation(llvm::store(
                raw,
                neg_one,
                slot,
                loc,
                LoadStoreOptions::new(),
            ));
        }
        let _ = ret_sub_blk.append_operation(cf::br(&cursor.exit_block, &[], loc));
        cursor.current_block = cont_blk;
        cursor.termination = BlockTermination::Open;
    } else {
        if let Some(slot) = cursor.para_goback_slot {
            let neg_one = append_res0(
                cursor.current_block,
                arith::constant(
                    raw,
                    IntegerAttribute::new(i32_ty, i64::from(ACTION_GOBACK)).into(),
                    loc,
                ),
                cursor.zero_i32,
            );
            let _ = cursor.current_block.append_operation(llvm::store(
                raw,
                neg_one,
                slot,
                loc,
                LoadStoreOptions::new(),
            ));
        }
        let _ = cursor
            .current_block
            .append_operation(cf::br(&cursor.exit_block, &[], loc));
        cursor.termination = BlockTermination::Terminated;
    }
}

/// Emits C `@main(i32 %arg_count, !llvm.ptr %arg_values) -> i32` calling `@cob_init`, `@<primary_sym>`, and `@cob_tidy`.
///
/// # Arguments
///
/// * `rw` - Module rewrite state.
/// * `primary_sym` - Canonical mangled entrypoint of the first top-level COBOL program.
fn emit_main_wrapper(rw: &LlvmRewriteState<'_>, primary_sym: &str) {
    let raw = rw.ctx.raw();
    let loc = rw.ctx.unknown_location();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let ptr_ty = llvm_ptr_type(rw.ctx);

    let main_ty = FunctionType::new(raw, &[i32_ty, ptr_ty], &[i32_ty]);
    let region = Region::new();
    let blk = region.append_block(Block::new(&[(i32_ty, loc), (ptr_ty, loc)]));

    let args_vec: Vec<Value<'_, '_>> = (0..blk.argument_count())
        .filter_map(|a_idx| blk.argument(a_idx).ok().map(Into::into))
        .collect();
    let _ = blk.append_operation(func::call(
        raw,
        FlatSymbolRefAttribute::new(raw, "cob_init"),
        &args_vec,
        &[],
        loc,
    ));
    let call_prog = blk.append_operation(func::call(
        raw,
        FlatSymbolRefAttribute::new(raw, primary_sym),
        &[],
        &[i32_ty],
        loc,
    ));
    let ret_vals: Vec<Value<'_, '_>> = (0..call_prog.result_count())
        .filter_map(|r_idx| call_prog.result(r_idx).ok().map(Into::into))
        .collect();
    let _ = blk.append_operation(func::call(
        raw,
        FlatSymbolRefAttribute::new(raw, "cob_tidy"),
        &[],
        &[],
        loc,
    ));
    let _ = blk.append_operation(func::r#return(&ret_vals, loc));

    let main_fn = func::func(
        raw,
        StringAttribute::new(raw, "main"),
        TypeAttribute::new(main_ty.into()),
        region,
        &[],
        loc,
    );
    let _ = rw.out_module.body().append_operation(main_fn);
}
