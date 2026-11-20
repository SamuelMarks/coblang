//! Zero-heap-allocation `PERFORM ... THRU`, sequential fallthrough, `GO TO` / `DEPENDING ON`,
//! `ALTER`, `EXIT PARAGRAPH` / `EXIT SECTION`, inline `PERFORM` loop, and `NEXT SENTENCE` CFG lowering.

use std::collections::{BTreeMap, BTreeSet};

use melior::dialect::arith::CmpiPredicate;
use melior::dialect::llvm::LoadStoreOptions;
use melior::dialect::{arith, cf, func, llvm};
use melior::ir::attribute::{
    FlatSymbolRefAttribute, IntegerAttribute, StringAttribute, TypeAttribute,
};
use melior::ir::block::BlockLike;
use melior::ir::operation::OperationLike;
use melior::ir::r#type::{FunctionType, IntegerType};
use melior::ir::{
    Attribute, Block, BlockRef, Identifier, Location, Module, OperationRef, Region, RegionLike,
    Type, Value,
};

use crate::lower_to_llvm::{
    BlockTermination, FnCursor, LlvmRewriteState, append_res0, emit_addressof, op_in,
    read_i32_array_attr, read_i64_attr, read_str_attr, remap_val, rewrite_cobol_block,
};
use crate::ops::{CobolOpKind, llvm_ptr_type};

/// Paragraph helper return code indicating normal completion / `EXIT PARAGRAPH`.
pub const ACTION_NORMAL_NEXT: i32 = 0;

/// Paragraph helper return code indicating `GOBACK` / active `EXIT PROGRAM` termination.
pub const ACTION_GOBACK: i32 = -1;

/// Base offset for `EXIT SECTION` return codes (`-2 - section_end_pid`).
pub const ACTION_EXIT_SECTION_BASE: i32 = -2;

/// Fixed capacity of each program's zero-heap-allocation [`PerformContinuationModel::PerformStack`] (`sp` + 256 frame entries).
pub const PERFORM_STACK_MAX_DEPTH: u32 = 256;

/// Byte size of `@__cobol_pfm_stk_<prog>` (`(1 + PERFORM_STACK_MAX_DEPTH) * 4`).
pub const PERFORM_STACK_STORAGE_BYTES: u32 = (1 + PERFORM_STACK_MAX_DEPTH) * 4;

/// Zero-heap-allocation continuation discipline backing out-of-line `PERFORM ... THRU` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PerformContinuationModel {
    /// Fixed-capacity `i32` [`PerformContinuationModel::PerformStack`] (`sp` at index `0`, `frames[1..=256]`) honoring `PerformReturnStyle::IsoStack`.
    PerformStack,
    /// Per-paragraph `i32` [`PerformContinuationModel::PerformContinuationSlot`] array honoring `PerformReturnStyle::OsVsContinuation`.
    PerformContinuationSlot,
}

/// Discovered paragraph count, entry-helper presence, and continuation model for one `cobol.program`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramPerformPlan {
    /// Canonical mangled program symbol (`<sym>`).
    pub prog_sym: String,
    /// Number of sequential paragraphs/sections (`__cobol_proc_<sym>_0 .. __cobol_proc_<sym>_<N-1>`).
    pub proc_count: u32,
    /// Whether `__cobol_proc_<sym>_entry` is defined in the module.
    pub has_entry_helper: EntryHelperState,
    /// Backing continuation storage symbol (`__cobol_pfm_stk_<sym>` or `__cobol_pfm_slots_<sym>`).
    pub storage_sym: String,
    /// Active [`PerformContinuationModel`] (`PerformStack` vs `PerformContinuationSlot`).
    pub model: PerformContinuationModel,
}

/// Presence of an extracted `__cobol_proc_<sym>_entry` helper function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryHelperState {
    /// No separate entry helper was emitted.
    Absent,
    /// `__cobol_proc_<sym>_entry` is present.
    Present,
}

/// Scans `module` for `__cobol_proc_<base>_*` helpers and `@__cobol_pfm_{stk,slots}_<base>` globals.
///
/// # Arguments
///
/// * `module` - Verified `cobol` MLIR module.
#[must_use]
pub fn collect_program_perform_plans(module: &Module<'_>) -> BTreeMap<String, ProgramPerformPlan> {
    let mut storage_syms = BTreeSet::new();
    let mut proc_indices: BTreeMap<String, u32> = BTreeMap::new();
    let mut entry_helpers = BTreeSet::new();

    let mut cur = module.body().first_operation();
    while let Some(op) = cur {
        let name_id = op.name();
        let op_name = name_id.as_string_ref().as_str().unwrap_or("");
        if op_name == CobolOpKind::Storage.op_name() {
            let sym = read_str_attr(op, "sym_name");
            let _ = storage_syms.insert(sym);
        } else if op_name == CobolOpKind::Program.op_name() {
            let sym = read_str_attr(op, "sym_name");
            if let Some(rest) = sym.strip_prefix("__cobol_proc_")
                && let Some((base, suffix)) = rest.rsplit_once('_')
            {
                if suffix == "entry" {
                    let _ = entry_helpers.insert(String::from(base));
                } else if let Ok(pid) = suffix.parse::<u32>() {
                    let count_slot = proc_indices.entry(String::from(base)).or_insert(0);
                    *count_slot = (*count_slot).max(pid + 1);
                }
            }
        }
        cur = op.next_in_block();
    }

    let mut plans = BTreeMap::new();
    for (base, proc_count) in proc_indices {
        let slots_sym = format!("__cobol_pfm_slots_{base}");
        let stk_sym = format!("__cobol_pfm_stk_{base}");
        let (storage_sym, model) = if storage_syms.contains(&slots_sym) {
            (slots_sym, PerformContinuationModel::PerformContinuationSlot)
        } else if storage_syms.contains(&stk_sym) {
            (stk_sym, PerformContinuationModel::PerformStack)
        } else {
            continue;
        };
        let has_entry_helper = if entry_helpers.contains(&base) {
            EntryHelperState::Present
        } else {
            EntryHelperState::Absent
        };
        plans.insert(
            base.clone(),
            ProgramPerformPlan {
                prog_sym: base,
                proc_count,
                has_entry_helper,
                storage_sym,
                model,
            },
        );
    }
    plans
}

/// Emits private `@__cobol_dispatch_<sym>(%start_pid: i32, %end_pid: i32) -> i32` functions for all `plans`.
///
/// # Arguments
///
/// * `plans` - Per-program [`ProgramPerformPlan`] map.
/// * `rw` - Module rewrite state.
pub fn emit_all_perform_dispatchers(
    plans: &BTreeMap<String, ProgramPerformPlan>,
    rw: &LlvmRewriteState<'_>,
) {
    for plan in plans.values() {
        emit_single_perform_dispatcher(plan, rw);
    }
}

/// Emits `@__cobol_dispatch_<sym>(%start_pid: i32, %end_pid: i32) -> i32` for one program.
///
/// `%end_pid >= 0` runs a bounded `PERFORM %start_pid ..= %end_pid` (returning `0` on range completion or
/// `-1` on `GOBACK` / end-of-program falloff). `%end_pid < 0` runs unbounded sequential fallthrough to end of program.
///
/// # Arguments
///
/// * `plan` - Program dispatcher plan.
/// * `rw` - Module rewrite state.
fn emit_single_perform_dispatcher<'ctx>(plan: &ProgramPerformPlan, rw: &LlvmRewriteState<'ctx>) {
    let raw = rw.ctx.raw();
    let loc = rw.ctx.unknown_location();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let ptr_ty = llvm_ptr_type(rw.ctx);
    let fn_ty = FunctionType::new(raw, &[i32_ty, i32_ty], &[i32_ty]);
    let region = Region::new();

    let entry_blk = region.append_block(Block::new(&[(i32_ty, loc), (i32_ty, loc)]));
    let loop_blk = region.append_block(Block::new(&[(i32_ty, loc)]));
    let after_call_blk = region.append_block(Block::new(&[(i32_ty, loc), (i32_ty, loc)]));
    let epilogue_blk = region.append_block(Block::new(&[(i32_ty, loc)]));
    let ret_ok_blk = region.append_block(Block::new(&[]));
    let ret_end_blk = region.append_block(Block::new(&[]));

    let zero_op = arith::constant(raw, IntegerAttribute::new(i32_ty, 0).into(), loc);
    let appended_zero = entry_blk.append_operation(zero_op);
    let seed_vals: Vec<Value<'ctx, '_>> = (0..appended_zero.result_count())
        .filter_map(|i| appended_zero.result(i).ok().map(Into::into))
        .collect();
    for zero_i32 in seed_vals {
        let start_arg = entry_blk.argument(0).map_or(zero_i32, Into::into);
        let end_arg = entry_blk.argument(1).map_or(zero_i32, Into::into);
        let null_ptr = append_res0(entry_blk, llvm::zero(ptr_ty, loc), zero_i32);
        let base_ptr = emit_addressof(rw.ctx, entry_blk, &plan.storage_sym, null_ptr, loc);

        emit_arm_continuation(
            plan,
            rw,
            entry_blk,
            (base_ptr, end_arg, zero_i32),
            loop_blk,
            start_arg,
            loc,
        );

        emit_dispatch_switch(
            plan,
            rw,
            &region,
            (loop_blk, after_call_blk, ret_end_blk),
            zero_i32,
            loc,
        );

        emit_action_router(
            rw,
            &region,
            (after_call_blk, loop_blk, epilogue_blk, ret_end_blk),
            zero_i32,
            loc,
        );

        emit_epilogue_checker(
            plan,
            rw,
            &region,
            (epilogue_blk, loop_blk, ret_ok_blk),
            (base_ptr, end_arg, zero_i32),
            loc,
        );

        let _ = ret_ok_blk.append_operation(func::r#return(&[zero_i32], loc));
        emit_unwind_and_return_goback(plan, rw, ret_end_blk, (base_ptr, end_arg, zero_i32), loc);
    }

    let disp_sym = format!("__cobol_dispatch_{}", plan.prog_sym);
    let priv_attr: [(Identifier<'_>, Attribute<'_>); 1] = [(
        Identifier::new(raw, "sym_visibility"),
        StringAttribute::new(raw, "private").into(),
    )];
    let fn_op = func::func(
        raw,
        StringAttribute::new(raw, &disp_sym),
        TypeAttribute::new(fn_ty.into()),
        region,
        &priv_attr,
        loc,
    );
    let _ = rw.out_module.body().append_operation(fn_op);
}

/// Emits an `i32` constant in `blk` and returns its SSA [`Value`].
///
/// # Arguments
///
/// * `raw` - Raw MLIR context.
/// * `blk` - Target block.
/// * `val` - Constant signed 32-bit value.
/// * `fallback` - Dominated fallback value.
/// * `loc` - Location.
fn emit_const_i32<'a, 'ctx>(
    raw: &'ctx melior::Context,
    blk: BlockRef<'ctx, 'a>,
    val: i32,
    fallback: Value<'ctx, 'a>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'a> {
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let op = arith::constant(
        raw,
        IntegerAttribute::new(i32_ty, i64::from(val)).into(),
        loc,
    );
    append_res0(blk, op, fallback)
}

/// Computes `!llvm.ptr` to `i32` element `idx_i32` inside `base_ptr`.
///
/// # Arguments
///
/// * `rw` - Rewrite state.
/// * `blk` - Target block.
/// * `base_ptr` - Base `!llvm.ptr`.
/// * `idx_i32` - Element index (`i32`).
/// * `loc` - Location.
fn gep_i32_slot<'a, 'ctx>(
    rw: &LlvmRewriteState<'ctx>,
    blk: BlockRef<'ctx, 'a>,
    base_ptr: Value<'ctx, 'a>,
    idx_i32: Value<'ctx, 'a>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'a> {
    let raw = rw.ctx.raw();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let ptr_ty = llvm_ptr_type(rw.ctx);
    append_res0(
        blk,
        llvm::get_element_ptr_dynamic(raw, base_ptr, &[idx_i32], i32_ty, ptr_ty, loc),
        base_ptr,
    )
}

/// Arms `PerformStack` or `PerformContinuationSlot` when `%end_arg >= 0` and jumps to `loop_blk(%start_arg)`.
///
/// # Arguments
///
/// * `plan` - Program dispatcher plan.
/// * `rw` - Rewrite state.
/// * `entry_blk` - Entry block.
/// * `vals` - `(base_ptr, end_arg, zero_i32)` values.
/// * `loop_blk` - Target dispatch loop header block.
/// * `start_arg` - Initial `%cur` value.
/// * `loc` - Location.
fn emit_arm_continuation<'a, 'ctx>(
    plan: &ProgramPerformPlan,
    rw: &LlvmRewriteState<'ctx>,
    entry_blk: BlockRef<'ctx, 'a>,
    vals: (Value<'ctx, 'a>, Value<'ctx, 'a>, Value<'ctx, 'a>),
    loop_blk: BlockRef<'ctx, 'a>,
    start_arg: Value<'ctx, 'a>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let (base_ptr, end_arg, zero_i32) = vals;
    let is_bounded = append_res0(
        entry_blk,
        arith::cmpi(raw, CmpiPredicate::Sge, end_arg, zero_i32, loc),
        zero_i32,
    );
    let one = emit_const_i32(raw, entry_blk, 1, zero_i32, loc);

    match plan.model {
        PerformContinuationModel::PerformStack => {
            let max_sp = emit_const_i32(
                raw,
                entry_blk,
                i32::try_from(PERFORM_STACK_MAX_DEPTH - 1).unwrap_or(255),
                zero_i32,
                loc,
            );
            let sp_old = append_res0(
                entry_blk,
                llvm::load(raw, base_ptr, i32_ty, loc, LoadStoreOptions::new()),
                zero_i32,
            );
            let clamped = append_res0(entry_blk, arith::minsi(sp_old, max_sp, loc), zero_i32);
            let slot_idx = append_res0(entry_blk, arith::addi(clamped, one, loc), zero_i32);
            let frame_ptr = gep_i32_slot(rw, entry_blk, base_ptr, slot_idx, loc);
            let prev_frame = append_res0(
                entry_blk,
                llvm::load(raw, frame_ptr, i32_ty, loc, LoadStoreOptions::new()),
                zero_i32,
            );
            let written_frame = append_res0(
                entry_blk,
                arith::select(is_bounded, end_arg, prev_frame, loc),
                zero_i32,
            );
            let _ = entry_blk.append_operation(llvm::store(
                raw,
                written_frame,
                frame_ptr,
                loc,
                LoadStoreOptions::new(),
            ));
            let sp_inc = append_res0(entry_blk, arith::addi(clamped, one, loc), zero_i32);
            let sp_new = append_res0(
                entry_blk,
                arith::select(is_bounded, sp_inc, sp_old, loc),
                zero_i32,
            );
            let _ = entry_blk.append_operation(llvm::store(
                raw,
                sp_new,
                base_ptr,
                loc,
                LoadStoreOptions::new(),
            ));
        }
        PerformContinuationModel::PerformContinuationSlot => {
            let max_idx = emit_const_i32(
                raw,
                entry_blk,
                i32::try_from(plan.proc_count.saturating_sub(1)).unwrap_or(0),
                zero_i32,
                loc,
            );
            let non_neg = append_res0(entry_blk, arith::maxsi(end_arg, zero_i32, loc), zero_i32);
            let safe_idx = append_res0(entry_blk, arith::minsi(non_neg, max_idx, loc), zero_i32);
            let slot_ptr = gep_i32_slot(rw, entry_blk, base_ptr, safe_idx, loc);
            let cur_cnt = append_res0(
                entry_blk,
                llvm::load(raw, slot_ptr, i32_ty, loc, LoadStoreOptions::new()),
                zero_i32,
            );
            let inc_cnt = append_res0(entry_blk, arith::addi(cur_cnt, one, loc), zero_i32);
            let new_cnt = append_res0(
                entry_blk,
                arith::select(is_bounded, inc_cnt, cur_cnt, loc),
                zero_i32,
            );
            let _ = entry_blk.append_operation(llvm::store(
                raw,
                new_cnt,
                slot_ptr,
                loc,
                LoadStoreOptions::new(),
            ));
        }
    }

    let _ = entry_blk.append_operation(cf::br(&loop_blk, &[start_arg], loc));
}

/// Emits the `%cur` bounds check and cascade calling `@__cobol_proc_<sym>_<pid>()`.
///
/// # Arguments
///
/// * `plan` - Program dispatcher plan.
/// * `rw` - Rewrite state.
/// * `region` - Enclosing dispatcher region.
/// * `blks` - `(loop_blk, after_call_blk, ret_end_blk)` tuple.
/// * `zero_i32` - Dominated zero value.
/// * `loc` - Location.
fn emit_dispatch_switch<'a, 'ctx>(
    plan: &ProgramPerformPlan,
    rw: &LlvmRewriteState<'ctx>,
    region: &'a Region<'ctx>,
    blks: (BlockRef<'ctx, 'a>, BlockRef<'ctx, 'a>, BlockRef<'ctx, 'a>),
    zero_i32: Value<'ctx, 'a>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let (loop_blk, after_call_blk, ret_end_blk) = blks;
    let cur_pid: Value<'ctx, 'a> = loop_blk.argument(0).map_or(zero_i32, Into::into);

    let mut check_blk = loop_blk;
    for pid in 0..plan.proc_count {
        let pid_val = emit_const_i32(
            raw,
            check_blk,
            i32::try_from(pid).unwrap_or(0),
            zero_i32,
            loc,
        );
        let is_match = append_res0(
            check_blk,
            arith::cmpi(raw, CmpiPredicate::Eq, cur_pid, pid_val, loc),
            zero_i32,
        );
        let call_blk = region.append_block(Block::new(&[]));
        let next_check = if pid + 1 < plan.proc_count {
            region.append_block(Block::new(&[]))
        } else {
            ret_end_blk
        };
        let _ = check_blk.append_operation(cf::cond_br(
            raw,
            is_match,
            &call_blk,
            &next_check,
            &[],
            &[],
            loc,
        ));
        let helper_sym = format!("__cobol_proc_{}_{pid}", plan.prog_sym);
        let act_val = append_res0(
            call_blk,
            func::call(
                raw,
                FlatSymbolRefAttribute::new(raw, &helper_sym),
                &[],
                &[i32_ty],
                loc,
            ),
            zero_i32,
        );
        let _ = call_blk.append_operation(cf::br(&after_call_blk, &[cur_pid, act_val], loc));
        check_blk = next_check;
    }
}

/// Routes paragraph return code `%act` (`0` normal, `-1` goback, `>= 1` goto, `<= -2` exit section).
///
/// # Arguments
///
/// * `rw` - Rewrite state.
/// * `region` - Enclosing dispatcher region.
/// * `blks` - `(after_call_blk, loop_blk, epilogue_blk, ret_end_blk)` tuple.
/// * `zero_i32` - Dominated zero value.
/// * `loc` - Location.
fn emit_action_router<'a, 'ctx>(
    rw: &LlvmRewriteState<'ctx>,
    region: &'a Region<'ctx>,
    blks: (
        BlockRef<'ctx, 'a>,
        BlockRef<'ctx, 'a>,
        BlockRef<'ctx, 'a>,
        BlockRef<'ctx, 'a>,
    ),
    zero_i32: Value<'ctx, 'a>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let (after_call_blk, loop_blk, epilogue_blk, ret_end_blk) = blks;
    let cur_pid: Value<'ctx, 'a> = after_call_blk.argument(0).map_or(zero_i32, Into::into);
    let act: Value<'ctx, 'a> = after_call_blk.argument(1).map_or(zero_i32, Into::into);

    let is_normal = append_res0(
        after_call_blk,
        arith::cmpi(raw, CmpiPredicate::Eq, act, zero_i32, loc),
        zero_i32,
    );
    let non_normal_blk = region.append_block(Block::new(&[]));
    let _ = after_call_blk.append_operation(cf::cond_br(
        raw,
        is_normal,
        &epilogue_blk,
        &non_normal_blk,
        &[cur_pid],
        &[],
        loc,
    ));

    let neg_one = emit_const_i32(raw, non_normal_blk, ACTION_GOBACK, zero_i32, loc);
    let is_goback = append_res0(
        non_normal_blk,
        arith::cmpi(raw, CmpiPredicate::Eq, act, neg_one, loc),
        zero_i32,
    );
    let goto_or_sec_blk = region.append_block(Block::new(&[]));
    let _ = non_normal_blk.append_operation(cf::cond_br(
        raw,
        is_goback,
        &ret_end_blk,
        &goto_or_sec_blk,
        &[],
        &[],
        loc,
    ));

    let one = emit_const_i32(raw, goto_or_sec_blk, 1, zero_i32, loc);
    let is_goto = append_res0(
        goto_or_sec_blk,
        arith::cmpi(raw, CmpiPredicate::Sge, act, one, loc),
        zero_i32,
    );
    let goto_target = append_res0(goto_or_sec_blk, arith::subi(act, one, loc), zero_i32);
    let neg_two = emit_const_i32(
        raw,
        goto_or_sec_blk,
        ACTION_EXIT_SECTION_BASE,
        zero_i32,
        loc,
    );
    let sec_end = append_res0(goto_or_sec_blk, arith::subi(neg_two, act, loc), zero_i32);
    let _ = goto_or_sec_blk.append_operation(cf::cond_br(
        raw,
        is_goto,
        &loop_blk,
        &epilogue_blk,
        &[goto_target],
        &[sec_end],
        loc,
    ));
}

/// Checks `PerformStack` / `PerformContinuationSlot` at paragraph epilogue `%done_pid`.
///
/// # Arguments
///
/// * `plan` - Program dispatcher plan.
/// * `rw` - Rewrite state.
/// * `region` - Enclosing dispatcher region.
/// * `blks` - `(epilogue_blk, loop_blk, ret_ok_blk)` tuple.
/// * `vals` - `(base_ptr, end_arg, zero_i32)` values.
/// * `loc` - Location.
fn emit_epilogue_checker<'a, 'ctx>(
    plan: &ProgramPerformPlan,
    rw: &LlvmRewriteState<'ctx>,
    region: &'a Region<'ctx>,
    blks: (BlockRef<'ctx, 'a>, BlockRef<'ctx, 'a>, BlockRef<'ctx, 'a>),
    vals: (Value<'ctx, 'a>, Value<'ctx, 'a>, Value<'ctx, 'a>),
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let (epilogue_blk, loop_blk, ret_ok_blk) = blks;
    let (base_ptr, end_arg, zero_i32) = vals;
    let done_pid: Value<'ctx, 'a> = epilogue_blk.argument(0).map_or(zero_i32, Into::into);
    let one = emit_const_i32(raw, epilogue_blk, 1, zero_i32, loc);
    let seq_next = append_res0(epilogue_blk, arith::addi(done_pid, one, loc), zero_i32);

    match plan.model {
        PerformContinuationModel::PerformStack => {
            let is_bounded = append_res0(
                epilogue_blk,
                arith::cmpi(raw, CmpiPredicate::Sge, end_arg, zero_i32, loc),
                zero_i32,
            );
            let sp_cur = append_res0(
                epilogue_blk,
                llvm::load(raw, base_ptr, i32_ty, loc, LoadStoreOptions::new()),
                zero_i32,
            );
            let has_sp = append_res0(
                epilogue_blk,
                arith::cmpi(raw, CmpiPredicate::Sgt, sp_cur, zero_i32, loc),
                zero_i32,
            );
            let active = append_res0(epilogue_blk, arith::andi(is_bounded, has_sp, loc), zero_i32);
            let safe_top_idx = append_res0(epilogue_blk, arith::maxsi(sp_cur, one, loc), zero_i32);
            let top_ptr = gep_i32_slot(rw, epilogue_blk, base_ptr, safe_top_idx, loc);
            let top_val = append_res0(
                epilogue_blk,
                llvm::load(raw, top_ptr, i32_ty, loc, LoadStoreOptions::new()),
                zero_i32,
            );
            let eq_end = append_res0(
                epilogue_blk,
                arith::cmpi(raw, CmpiPredicate::Eq, done_pid, top_val, loc),
                zero_i32,
            );
            let should_ret = append_res0(epilogue_blk, arith::andi(active, eq_end, loc), zero_i32);

            let pop_blk = region.append_block(Block::new(&[]));
            let _ = epilogue_blk.append_operation(cf::cond_br(
                raw,
                should_ret,
                &pop_blk,
                &loop_blk,
                &[],
                &[seq_next],
                loc,
            ));
            let sp_dec = append_res0(pop_blk, arith::subi(sp_cur, one, loc), zero_i32);
            let _ = pop_blk.append_operation(llvm::store(
                raw,
                sp_dec,
                base_ptr,
                loc,
                LoadStoreOptions::new(),
            ));
            let _ = pop_blk.append_operation(cf::br(&ret_ok_blk, &[], loc));
        }
        PerformContinuationModel::PerformContinuationSlot => {
            let slot_ptr = gep_i32_slot(rw, epilogue_blk, base_ptr, done_pid, loc);
            let cnt = append_res0(
                epilogue_blk,
                llvm::load(raw, slot_ptr, i32_ty, loc, LoadStoreOptions::new()),
                zero_i32,
            );
            let armed = append_res0(
                epilogue_blk,
                arith::cmpi(raw, CmpiPredicate::Sgt, cnt, zero_i32, loc),
                zero_i32,
            );
            let disarm_blk = region.append_block(Block::new(&[]));
            let _ = epilogue_blk.append_operation(cf::cond_br(
                raw,
                armed,
                &disarm_blk,
                &loop_blk,
                &[],
                &[seq_next],
                loc,
            ));
            let dec = append_res0(disarm_blk, arith::subi(cnt, one, loc), zero_i32);
            let _ = disarm_blk.append_operation(llvm::store(
                raw,
                dec,
                slot_ptr,
                loc,
                LoadStoreOptions::new(),
            ));
            let _ = disarm_blk.append_operation(cf::br(&ret_ok_blk, &[], loc));
        }
    }
}

/// Pops/disarms any active frame on `GOBACK` / end-of-program falloff and returns `ACTION_GOBACK` (`-1`).
///
/// # Arguments
///
/// * `plan` - Program dispatcher plan.
/// * `rw` - Rewrite state.
/// * `ret_end_blk` - Exit block for `GOBACK` / falloff.
/// * `vals` - `(base_ptr, end_arg, zero_i32)` values.
/// * `loc` - Location.
fn emit_unwind_and_return_goback<'a, 'ctx>(
    plan: &ProgramPerformPlan,
    rw: &LlvmRewriteState<'ctx>,
    ret_end_blk: BlockRef<'ctx, 'a>,
    vals: (Value<'ctx, 'a>, Value<'ctx, 'a>, Value<'ctx, 'a>),
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let (base_ptr, end_arg, zero_i32) = vals;
    let is_bounded = append_res0(
        ret_end_blk,
        arith::cmpi(raw, CmpiPredicate::Sge, end_arg, zero_i32, loc),
        zero_i32,
    );
    let one = emit_const_i32(raw, ret_end_blk, 1, zero_i32, loc);

    match plan.model {
        PerformContinuationModel::PerformStack => {
            let sp = append_res0(
                ret_end_blk,
                llvm::load(raw, base_ptr, i32_ty, loc, LoadStoreOptions::new()),
                zero_i32,
            );
            let dec = append_res0(ret_end_blk, arith::subi(sp, one, loc), zero_i32);
            let clamped = append_res0(ret_end_blk, arith::maxsi(dec, zero_i32, loc), zero_i32);
            let next_sp = append_res0(
                ret_end_blk,
                arith::select(is_bounded, clamped, sp, loc),
                zero_i32,
            );
            let _ = ret_end_blk.append_operation(llvm::store(
                raw,
                next_sp,
                base_ptr,
                loc,
                LoadStoreOptions::new(),
            ));
        }
        PerformContinuationModel::PerformContinuationSlot => {
            let max_idx = emit_const_i32(
                raw,
                ret_end_blk,
                i32::try_from(plan.proc_count.saturating_sub(1)).unwrap_or(0),
                zero_i32,
                loc,
            );
            let non_neg = append_res0(ret_end_blk, arith::maxsi(end_arg, zero_i32, loc), zero_i32);
            let safe_idx = append_res0(ret_end_blk, arith::minsi(non_neg, max_idx, loc), zero_i32);
            let slot_ptr = gep_i32_slot(rw, ret_end_blk, base_ptr, safe_idx, loc);
            let cnt = append_res0(
                ret_end_blk,
                llvm::load(raw, slot_ptr, i32_ty, loc, LoadStoreOptions::new()),
                zero_i32,
            );
            let dec = append_res0(ret_end_blk, arith::subi(cnt, one, loc), zero_i32);
            let clamped = append_res0(ret_end_blk, arith::maxsi(dec, zero_i32, loc), zero_i32);
            let next_cnt = append_res0(
                ret_end_blk,
                arith::select(is_bounded, clamped, cnt, loc),
                zero_i32,
            );
            let _ = ret_end_blk.append_operation(llvm::store(
                raw,
                next_cnt,
                slot_ptr,
                loc,
                LoadStoreOptions::new(),
            ));
        }
    }

    let neg_one = emit_const_i32(raw, ret_end_blk, ACTION_GOBACK, zero_i32, loc);
    let _ = ret_end_blk.append_operation(func::r#return(&[neg_one], loc));
}

/// Lowers `cobol.perform_range` via `@__cobol_dispatch_<prog>(start, end)`.
///
/// # Arguments
///
/// * `op` - `cobol.perform_range` operation.
/// * `cursor` - Active function cursor.
/// * `rw` - Rewrite state.
/// * `loc` - Source location.
pub fn lower_perform_range_cfg<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let target_override = read_str_attr(op, "target_prog");
    let lookup_sym = if target_override.is_empty() {
        cursor.prog_base_sym.as_str()
    } else {
        target_override.as_str()
    };
    let Some(plan) = rw.perform_plans.get(lookup_sym) else {
        return;
    };
    let raw_start = read_i64_attr(op, "start_proc");
    let raw_end = read_i64_attr(op, "end_proc");
    let disp_sym = format!("__cobol_dispatch_{}", plan.prog_sym);

    if raw_start == i64::from(u32::MAX) {
        lower_entry_helper_fallthrough(&plan.prog_sym, &disp_sym, cursor, rw, loc);
        return;
    }

    let raw = rw.ctx.raw();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let start_i32 = i32::try_from(raw_start).unwrap_or(0);
    let end_i32 = if raw_end == i64::from(u32::MAX) {
        -1
    } else {
        i32::try_from(raw_end).unwrap_or(0)
    };
    let s_val = emit_const_i32(raw, cursor.current_block, start_i32, cursor.zero_i32, loc);
    let e_val = emit_const_i32(raw, cursor.current_block, end_i32, cursor.zero_i32, loc);
    let ret_code = append_res0(
        cursor.current_block,
        func::call(
            raw,
            FlatSymbolRefAttribute::new(raw, &disp_sym),
            &[s_val, e_val],
            &[i32_ty],
            loc,
        ),
        cursor.zero_i32,
    );
    let did_terminate = append_res0(
        cursor.current_block,
        arith::cmpi(raw, CmpiPredicate::Ne, ret_code, cursor.zero_i32, loc),
        cursor.zero_i32,
    );
    let prop_blk = cursor.region.append_block(Block::new(&[]));
    let cont_blk = cursor.region.append_block(Block::new(&[]));
    let _ = cursor.current_block.append_operation(cf::cond_br(
        raw,
        did_terminate,
        &prop_blk,
        &cont_blk,
        &[],
        &[],
        loc,
    ));
    if let Some(slot) = cursor.para_goback_slot {
        let neg_one = emit_const_i32(raw, prop_blk, ACTION_GOBACK, cursor.zero_i32, loc);
        let _ = prop_blk.append_operation(llvm::store(
            raw,
            neg_one,
            slot,
            loc,
            LoadStoreOptions::new(),
        ));
    }
    let _ = prop_blk.append_operation(cf::br(&cursor.exit_block, &[], loc));
    cursor.current_block = cont_blk;
    cursor.termination = BlockTermination::Open;
}

/// Calls `@__cobol_proc_<prog_sym>_entry()` and routes fallthrough/goto into `@<disp_sym>`.
///
/// # Arguments
///
/// * `prog_sym` - Canonical program symbol.
/// * `disp_sym` - Program dispatcher symbol.
/// * `cursor` - Active function cursor.
/// * `rw` - Rewrite state.
/// * `loc` - Source location.
fn lower_entry_helper_fallthrough<'ctx>(
    prog_sym: &str,
    disp_sym: &str,
    cursor: &mut FnCursor<'_, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let entry_sym = format!("__cobol_proc_{prog_sym}_entry");
    let entry_act = append_res0(
        cursor.current_block,
        func::call(
            raw,
            FlatSymbolRefAttribute::new(raw, &entry_sym),
            &[],
            &[i32_ty],
            loc,
        ),
        cursor.zero_i32,
    );
    let neg_one = emit_const_i32(
        raw,
        cursor.current_block,
        ACTION_GOBACK,
        cursor.zero_i32,
        loc,
    );
    let is_goback = append_res0(
        cursor.current_block,
        arith::cmpi(raw, CmpiPredicate::Eq, entry_act, neg_one, loc),
        cursor.zero_i32,
    );
    let run_disp_blk = cursor.region.append_block(Block::new(&[]));
    let cont_blk = cursor.region.append_block(Block::new(&[]));
    let _ = cursor.current_block.append_operation(cf::cond_br(
        raw,
        is_goback,
        &cursor.exit_block,
        &run_disp_blk,
        &[],
        &[],
        loc,
    ));
    let one = emit_const_i32(raw, run_disp_blk, 1, cursor.zero_i32, loc);
    let is_goto = append_res0(
        run_disp_blk,
        arith::cmpi(raw, CmpiPredicate::Sge, entry_act, one, loc),
        cursor.zero_i32,
    );
    let goto_pid = append_res0(
        run_disp_blk,
        arith::subi(entry_act, one, loc),
        cursor.zero_i32,
    );
    let eff_start = append_res0(
        run_disp_blk,
        arith::select(is_goto, goto_pid, cursor.zero_i32, loc),
        cursor.zero_i32,
    );
    let unb = emit_const_i32(raw, run_disp_blk, -1, cursor.zero_i32, loc);
    let _ = run_disp_blk.append_operation(func::call(
        raw,
        FlatSymbolRefAttribute::new(raw, disp_sym),
        &[eff_start, unb],
        &[i32_ty],
        loc,
    ));
    let _ = run_disp_blk.append_operation(cf::br(&cont_blk, &[], loc));
    cursor.current_block = cont_blk;
    cursor.termination = BlockTermination::Open;
}

/// Lowers `cobol.goto` (unconditional, `ALTER`able, or `DEPENDING ON`).
///
/// # Arguments
///
/// * `op` - `cobol.goto` operation.
/// * `cursor` - Active function cursor.
/// * `rw` - Rewrite state.
/// * `loc` - Source location.
pub fn lower_goto_cfg<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let Some(slot) = cursor.para_goback_slot else {
        return;
    };
    let targets = read_i32_array_attr(op, "targets");
    if op.operand_count() >= 1 {
        let sel = op_in(op, 0, cursor, cursor.zero_i32);
        lower_goto_depending_on(sel, &targets, slot, cursor, rw, loc);
        return;
    }

    let raw = rw.ctx.raw();
    let i32_ty: Type<'_> = IntegerType::new(raw, 32).into();
    let alter_sym = read_str_attr(op, "alter_slot_sym");
    if !alter_sym.is_empty() {
        let ptr = emit_addressof(
            rw.ctx,
            cursor.current_block,
            &alter_sym,
            cursor.null_ptr,
            loc,
        );
        let loaded_pid = append_res0(
            cursor.current_block,
            llvm::load(raw, ptr, i32_ty, loc, LoadStoreOptions::new()),
            cursor.zero_i32,
        );
        let one = emit_const_i32(raw, cursor.current_block, 1, cursor.zero_i32, loc);
        let act_val = append_res0(
            cursor.current_block,
            arith::addi(loaded_pid, one, loc),
            cursor.zero_i32,
        );
        let _ = cursor.current_block.append_operation(llvm::store(
            raw,
            act_val,
            slot,
            loc,
            LoadStoreOptions::new(),
        ));
        let _ = cursor
            .current_block
            .append_operation(cf::br(&cursor.exit_block, &[], loc));
        cursor.termination = BlockTermination::Terminated;
        return;
    }

    if let Some(&first_pid) = targets.first() {
        let act_val = emit_const_i32(
            raw,
            cursor.current_block,
            first_pid.saturating_add(1),
            cursor.zero_i32,
            loc,
        );
        let _ = cursor.current_block.append_operation(llvm::store(
            raw,
            act_val,
            slot,
            loc,
            LoadStoreOptions::new(),
        ));
        let _ = cursor
            .current_block
            .append_operation(cf::br(&cursor.exit_block, &[], loc));
        cursor.termination = BlockTermination::Terminated;
    }
}

/// Emits the 1-based `GO TO t1 ... tn DEPENDING ON sel` cascade (falling through when `sel < 1` or `sel > n`).
///
/// # Arguments
///
/// * `sel` - 1-based `i32` selector SSA value.
/// * `targets` - Target paragraph IDs.
/// * `slot` - Paragraph return-action slot pointer.
/// * `cursor` - Active function cursor.
/// * `rw` - Rewrite state.
/// * `loc` - Source location.
fn lower_goto_depending_on<'a, 'ctx>(
    sel: Value<'ctx, 'a>,
    targets: &[i32],
    slot: Value<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let fall_blk = cursor.region.append_block(Block::new(&[]));
    let mut check_blk = cursor.current_block;
    for (idx, &target_pid) in targets.iter().enumerate() {
        let case_num = emit_const_i32(
            raw,
            check_blk,
            i32::try_from(idx + 1).unwrap_or(1),
            cursor.zero_i32,
            loc,
        );
        let matched = append_res0(
            check_blk,
            arith::cmpi(raw, CmpiPredicate::Eq, sel, case_num, loc),
            cursor.zero_i32,
        );
        let jump_blk = cursor.region.append_block(Block::new(&[]));
        let next_blk = if idx + 1 < targets.len() {
            cursor.region.append_block(Block::new(&[]))
        } else {
            fall_blk
        };
        let _ = check_blk.append_operation(cf::cond_br(
            raw,
            matched,
            &jump_blk,
            &next_blk,
            &[],
            &[],
            loc,
        ));
        let act_val = emit_const_i32(
            raw,
            jump_blk,
            target_pid.saturating_add(1),
            cursor.zero_i32,
            loc,
        );
        let _ = jump_blk.append_operation(llvm::store(
            raw,
            act_val,
            slot,
            loc,
            LoadStoreOptions::new(),
        ));
        let _ = jump_blk.append_operation(cf::br(&cursor.exit_block, &[], loc));
        check_blk = next_blk;
    }
    if targets.is_empty() {
        let _ = check_blk.append_operation(cf::br(&fall_blk, &[], loc));
    }
    cursor.current_block = fall_blk;
    cursor.termination = BlockTermination::Open;
}

/// Lowers `cobol.alter` by storing `new_target` into `@__cobol_alter_<prog>_<altered_para>`.
///
/// # Arguments
///
/// * `op` - `cobol.alter` operation.
/// * `cursor` - Active function cursor.
/// * `rw` - Rewrite state.
/// * `loc` - Source location.
pub fn lower_alter_cfg<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let altered = read_i64_attr(op, "altered_para");
    let new_target = i32::try_from(read_i64_attr(op, "new_target")).unwrap_or(0);
    let alter_sym = format!("__cobol_alter_{}_{altered}", cursor.prog_base_sym);
    if !rw.defined_globals.contains(&alter_sym) {
        return;
    }
    let raw = rw.ctx.raw();
    let ptr = emit_addressof(
        rw.ctx,
        cursor.current_block,
        &alter_sym,
        cursor.null_ptr,
        loc,
    );
    let val = emit_const_i32(raw, cursor.current_block, new_target, cursor.zero_i32, loc);
    let _ = cursor.current_block.append_operation(llvm::store(
        raw,
        val,
        ptr,
        loc,
        LoadStoreOptions::new(),
    ));
}

/// Lowers `cobol.loop` (`TEST BEFORE` / `TEST AFTER`, `EXIT PERFORM`, and `EXIT PERFORM CYCLE`).
///
/// # Arguments
///
/// * `op` - `cobol.loop` operation.
/// * `cursor` - Active function cursor.
/// * `rw` - Mutable rewrite state.
/// * `loc` - Source location.
pub fn lower_loop_cfg<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &mut LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let is_user_perform = read_str_attr(op, "loop_kind") == "user_perform";
    let is_before = read_str_attr(op, "test_pos") == "before";

    let cond_blk = cursor.region.append_block(Block::new(&[]));
    let body_blk = cursor.region.append_block(Block::new(&[]));
    let step_blk = cursor.region.append_block(Block::new(&[]));
    let after_blk = cursor.region.append_block(Block::new(&[]));

    let entry_target = if is_before { &cond_blk } else { &body_blk };
    let _ = cursor
        .current_block
        .append_operation(cf::br(entry_target, &[], loc));

    // Condition block: evaluate region 0 and branch to `after_blk` when `until` holds.
    cursor.current_block = cond_blk;
    cursor.termination = BlockTermination::Open;
    let until_val = rewrite_first_region_block(op, 0, cursor, rw)
        .map_or(cursor.zero_i32, |src_cblk| {
            extract_block_yield_val(src_cblk, cursor)
        });
    let on_false_blk = if is_before { &body_blk } else { &step_blk };
    let _ = cursor.current_block.append_operation(cf::cond_br(
        raw,
        until_val,
        &after_blk,
        on_false_blk,
        &[],
        &[],
        loc,
    ));

    // Body block: bind `perform_break_block` (if `user_perform`) and `perform_cycle_block`!
    let prev_break = cursor.perform_break_block;
    let prev_cycle = cursor.perform_cycle_block;
    if is_user_perform {
        cursor.perform_break_block = Some(after_blk);
    }
    let cycle_target = if is_before { step_blk } else { cond_blk };
    cursor.perform_cycle_block = Some(cycle_target);

    cursor.current_block = body_blk;
    cursor.termination = BlockTermination::Open;
    let _ = rewrite_first_region_block(op, 1, cursor, rw);
    if cursor.termination == BlockTermination::Open {
        let _ = cursor
            .current_block
            .append_operation(cf::br(&cycle_target, &[], loc));
    }

    cursor.perform_break_block = prev_break;
    cursor.perform_cycle_block = prev_cycle;

    // Step block: execute augmentation statements and loop back (`cond_blk` for Before, `body_blk` for After).
    cursor.current_block = step_blk;
    cursor.termination = BlockTermination::Open;
    let _ = rewrite_first_region_block(op, 2, cursor, rw);
    let back_target = if is_before { &cond_blk } else { &body_blk };
    let _ = cursor
        .current_block
        .append_operation(cf::br(back_target, &[], loc));

    cursor.current_block = after_blk;
    cursor.termination = BlockTermination::Open;
}

/// Rewrites `op.region(reg_idx).first_block()` when present, returning the source block.
///
/// # Arguments
///
/// * `op` - Enclosing region-bearing `cobol.*` operation.
/// * `reg_idx` - Region index (`0`, `1`, or `2`).
/// * `cursor` - Active function cursor.
/// * `rw` - Mutable rewrite state.
fn rewrite_first_region_block<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    reg_idx: usize,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &mut LlvmRewriteState<'ctx>,
) -> Option<BlockRef<'ctx, 'a>> {
    let reg = op.region(reg_idx).ok()?;
    let blk = reg.first_block()?;
    rewrite_cobol_block(blk, cursor, rw);
    Some(blk)
}

/// Scans `blk` for its trailing `cobol.yield` operation and remaps its yielded SSA operand.
///
/// # Arguments
///
/// * `blk` - Source condition region block.
/// * `cursor` - Active function cursor.
fn extract_block_yield_val<'a, 'ctx>(
    blk: BlockRef<'ctx, 'a>,
    cursor: &FnCursor<'a, 'ctx>,
) -> Value<'ctx, 'a> {
    let mut cur = blk.first_operation();
    let mut yielded = cursor.zero_i32;
    while let Some(op) = cur {
        if op.name().as_string_ref().as_str().unwrap_or("") == CobolOpKind::Yield.op_name() {
            yielded = op
                .operand(0)
                .map_or(cursor.zero_i32, |v| remap_val(cursor, v, cursor.zero_i32));
        }
        cur = op.next_in_block();
    }
    yielded
}

/// Lowers `cobol.sentence` by binding `cursor.sentence_exit_block` around its body region.
///
/// # Arguments
///
/// * `op` - `cobol.sentence` operation.
/// * `cursor` - Active function cursor.
/// * `rw` - Mutable rewrite state.
/// * `loc` - Source location.
pub fn lower_sentence_cfg<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &mut LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let sent_after = cursor.region.append_block(Block::new(&[]));
    let prev_sent = cursor.sentence_exit_block;
    cursor.sentence_exit_block = Some(sent_after);

    let _ = rewrite_first_region_block(op, 0, cursor, rw);
    if cursor.termination == BlockTermination::Open {
        let _ = cursor
            .current_block
            .append_operation(cf::br(&sent_after, &[], loc));
    }

    cursor.sentence_exit_block = prev_sent;
    cursor.current_block = sent_after;
    cursor.termination = BlockTermination::Open;
}

/// Lowers `cobol.flow_exit` (`paragraph`, `section`, `perform_break`, `perform_cycle`, `next_sentence`).
///
/// # Arguments
///
/// * `op` - `cobol.flow_exit` operation.
/// * `cursor` - Active function cursor.
/// * `rw` - Rewrite state.
/// * `loc` - Source location.
pub fn lower_flow_exit_cfg<'a, 'ctx>(
    op: OperationRef<'ctx, 'a>,
    cursor: &mut FnCursor<'a, 'ctx>,
    rw: &LlvmRewriteState<'ctx>,
    loc: Location<'ctx>,
) {
    let raw = rw.ctx.raw();
    let kind = read_str_attr(op, "exit_kind");
    match kind.as_str() {
        "perform_break" => {
            if let Some(target) = cursor.perform_break_block {
                let _ = cursor
                    .current_block
                    .append_operation(cf::br(&target, &[], loc));
                cursor.termination = BlockTermination::Terminated;
            }
        }
        "perform_cycle" => {
            if let Some(target) = cursor.perform_cycle_block {
                let _ = cursor
                    .current_block
                    .append_operation(cf::br(&target, &[], loc));
                cursor.termination = BlockTermination::Terminated;
            }
        }
        "next_sentence" => {
            let target = cursor.sentence_exit_block.unwrap_or(cursor.exit_block);
            let _ = cursor
                .current_block
                .append_operation(cf::br(&target, &[], loc));
            cursor.termination = BlockTermination::Terminated;
        }
        "section" => {
            if let Some(slot) = cursor.para_goback_slot {
                let sec_end = i32::try_from(read_i64_attr(op, "section_end")).unwrap_or(0);
                let code = ACTION_EXIT_SECTION_BASE.saturating_sub(sec_end);
                let val = emit_const_i32(raw, cursor.current_block, code, cursor.zero_i32, loc);
                let _ = cursor.current_block.append_operation(llvm::store(
                    raw,
                    val,
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
        _ => {
            // `"paragraph"` -> normal fallthrough (`0`) to paragraph epilogue!
            let _ = cursor
                .current_block
                .append_operation(cf::br(&cursor.exit_block, &[], loc));
            cursor.termination = BlockTermination::Terminated;
        }
    }
}
