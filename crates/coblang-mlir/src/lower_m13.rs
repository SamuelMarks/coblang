//! Milestone M13 (`>>TURN` directives, `SET LAST EXCEPTION TO OFF`, `RAISE`, `RESUME`,
//! `USE [GLOBAL] AFTER EXCEPTION CONDITION` declarative dispatch, NIST CCVS-85 `DB`
//! `DEBUG-ITEM` / `USE FOR DEBUGGING` hooks, and recursive frame save/restore) HIR-to-`cobol` MLIR lowering.

use coblang_common::Result;
use coblang_hir::{
    HirCheckExceptionStmt, HirDebugCompileMode, HirDebugContents, HirDebugTraceStmt,
    HirEcDeclarativeRule, HirM13UnitSpec, HirRaiseStmt, HirRaiseTarget, HirRecursivity,
    HirResumeStmt, HirResumeTarget, HirSetTurnStmt, HirStorageSection, HirTurnMode, HirTurnTarget,
};
use melior::dialect::arith::CmpiPredicate;
use melior::dialect::{arith, llvm, scf};
use melior::ir::attribute::TypeAttribute;
use melior::ir::block::BlockLike;
use melior::ir::r#type::IntegerType;
use melior::ir::{Block, Location, Region, RegionLike, Type, Value};

use crate::cfg_perform::PERFORM_STACK_STORAGE_BYTES;
use crate::lower_from_hir::{
    ModuleLowerState, ProgramLowerCtx, append_val, emit_const_slice, emit_i32, emit_i64,
    emit_rt_call, emit_storage_ptr, eval_expr_as_i64, lower_place_view,
};
use crate::ops::{
    CobolFlowExitOp, CobolGotoOp, CobolPerformRangeOp, CobolStopRunOp, RtDispatchTarget,
    llvm_ptr_type,
};
use crate::storage_init::RootBackingKind;

/// Emits recursive-frame state snapshots (`LOCAL-STORAGE`, `LINKAGE` pointer slots, and `@__cobol_pfm_stk_*`)
/// when `prog.recursivity == HirRecursivity::Recursive` (`RECURSIVE` programs and `FUNCTION-ID` UDFs).
///
/// # Arguments
///
/// * `canon_sym` - Canonical mangled symbol of the enclosing unit.
/// * `block` - Entry block of `ProgramEntry`.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
pub(crate) fn emit_m13_recursive_prologue<'ctx, 'b, 'e: 'b>(
    canon_sym: &str,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    if prog_ctx.prog.recursivity != HirRecursivity::Recursive {
        return Ok(());
    }
    let _ = emit_rt_call(
        RtDispatchTarget::M13RecFrameBegin,
        &[],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    for root in &prog_ctx.storage.roots {
        let sec = prog_ctx
            .prog
            .data_items
            .get(root.root_id.as_u32() as usize)
            .map(|it| it.section);
        if root.backing == RootBackingKind::IndirectBasePointer {
            let ptr = emit_storage_ptr(&root.sym_name, 0, 8, block, prog_ctx, state, loc)?;
            let eight = emit_i64(8, block, prog_ctx, state, loc);
            let _ = emit_rt_call(
                RtDispatchTarget::M13RecFrameSave,
                &[ptr, eight],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            if sec == Some(HirStorageSection::LocalStorage) {
                let raw = state.ctx.raw();
                let ptr_ty = llvm_ptr_type(state.ctx);
                let i8_ty: Type<'ctx> = IntegerType::new(raw, 8).into();
                let sz_i64 = emit_i64(
                    i64::from(root.byte_size.max(1)),
                    block,
                    prog_ctx,
                    state,
                    loc,
                );
                let fresh_buf = append_val(
                    prog_ctx.entry_block,
                    llvm::alloca(
                        raw,
                        sz_i64,
                        ptr_ty,
                        loc,
                        llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i8_ty))),
                    ),
                    prog_ctx.fallback_val,
                );
                let _ = block.append_operation(llvm::store(
                    raw,
                    fresh_buf,
                    ptr,
                    loc,
                    llvm::LoadStoreOptions::new(),
                ));
                crate::lower_m11::apply_based_root_value_image(
                    fresh_buf, root, block, prog_ctx, state, loc,
                )?;
            }
        }
    }

    if !prog_ctx.prog.procedures.is_empty() {
        let (pfm_sym, pfm_bytes) = match prog_ctx.prog.perform_style {
            coblang_hir::HirPerformStyle::IsoStack => (
                format!("__cobol_pfm_stk_{canon_sym}"),
                PERFORM_STACK_STORAGE_BYTES,
            ),
            coblang_hir::HirPerformStyle::OsVsContinuation => {
                let n = u32::try_from(prog_ctx.prog.procedures.len())
                    .unwrap_or(1)
                    .max(1);
                (
                    format!("__cobol_pfm_slots_{canon_sym}"),
                    n.saturating_mul(4),
                )
            }
        };
        let pfm_ptr = emit_storage_ptr(&pfm_sym, 0, pfm_bytes, block, prog_ctx, state, loc)?;
        let pfm_len = emit_i64(i64::from(pfm_bytes), block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::M13RecFrameSave,
            &[pfm_ptr, pfm_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }

    Ok(())
}

/// Arms the current statement's ISO `"<unit>; <proc>; <line>"` location before an `ON SIZE ERROR`
/// arithmetic statement so `FUNCTION EXCEPTION-LOCATION` inside the `ON SIZE ERROR` handler sees it.
///
/// # Arguments
///
/// * `verb` - COBOL verb name (`"COMPUTE"`, `"DIVIDE"`, etc.).
/// * `span` - Statement source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if constant-slice emission fails.
pub(crate) fn emit_arm_stmt_location<'ctx, 'b, 'e: 'b>(
    verb: &str,
    span: coblang_common::SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let unit_orig = state
        .interner
        .resolve_original(prog_ctx.prog.name)
        .unwrap_or("MAIN");
    let line_no = state
        .source_map
        .resolve_span(span)
        .map_or(1, |r| r.line.get());
    let proc_lbl = prog_ctx.active_proc_label.as_deref().unwrap_or("");
    let iso_loc = format!("{unit_orig}; {proc_lbl}; {line_no}");
    let (lp, ll) = emit_const_slice(iso_loc.as_bytes(), block, prog_ctx, state, loc)?;
    let (vp, vl) = emit_const_slice(verb.as_bytes(), block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::M13SetStmtLoc,
        &[lp, ll, vp, vl],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Lowers `HirStmt::SetTurn` (`>>TURN ec-name ... CHECKING {ON [WITH LOCATION] | OFF}`).
///
/// # Arguments
///
/// * `spec` - `>>TURN` statement payload.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
pub(crate) fn lower_set_turn_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirSetTurnStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let mode_code = match spec.mode {
        HirTurnMode::Off => 0,
        HirTurnMode::On => 1,
        HirTurnMode::OnWithLocation => 2,
    };
    let mode_v = emit_i32(mode_code, block, prog_ctx, state, loc);
    match &spec.target {
        HirTurnTarget::All => {
            let zero = emit_i32(0, block, prog_ctx, state, loc);
            let _ = emit_rt_call(
                RtDispatchTarget::M13SetTurn,
                &[zero, mode_v],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        HirTurnTarget::Codes(codes) => {
            for &c in codes {
                let cv = emit_i32(i32::from(c), block, prog_ctx, state, loc);
                let _ = emit_rt_call(
                    RtDispatchTarget::M13SetTurn,
                    &[cv, mode_v],
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

/// Lowers `HirStmt::SetLastExceptionOff` (`SET LAST EXCEPTION TO OFF`).
///
/// # Arguments
///
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
pub(crate) fn lower_set_last_exception_off<'ctx, 'b, 'e: 'b>(
    span: coblang_common::SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let _ = emit_rt_call(
        RtDispatchTarget::M13LastExceptionOff,
        &[],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Lowers `HirStmt::Raise` (`RAISE [EXCEPTION ec-name]`).
///
/// # Arguments
///
/// * `spec` - `RAISE` statement payload.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
pub(crate) fn lower_raise_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirRaiseStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let abi_id = match spec.target {
        HirRaiseTarget::LastException => 0,
        HirRaiseTarget::EcCode(c) => i32::from(c),
    };
    let id_v = emit_i32(abi_id, block, prog_ctx, state, loc);
    let loc_txt = state
        .interner
        .resolve_original(spec.location_sym)
        .unwrap_or("");
    let stmt_txt = state
        .interner
        .resolve_original(spec.statement_sym)
        .unwrap_or("RAISE");
    let (lp, ll) = emit_const_slice(loc_txt.as_bytes(), block, prog_ctx, state, loc)?;
    let (sp, sl) = emit_const_slice(stmt_txt.as_bytes(), block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::M13Raise,
        &[id_v, lp, ll, sp, sl],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Lowers `HirStmt::Resume` (`RESUME [AT NEXT STATEMENT | AT proc-name]`), recording the target
/// and immediately exiting the enclosing declarative section via `cobol.flow_exit("section")`.
///
/// # Arguments
///
/// * `spec` - `RESUME` statement payload.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
pub(crate) fn lower_resume_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirResumeStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let (kind, pid) = match spec.target {
        HirResumeTarget::NextStatement => (0, -1),
        HirResumeTarget::Procedure(p) => (1, i32::try_from(p.as_u32()).unwrap_or(0)),
    };
    let kv = emit_i32(kind, block, prog_ctx, state, loc);
    let pv = emit_i32(pid, block, prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::M13SetResume,
        &[kv, pv],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let section_end = match prog_ctx.body_ctx {
        crate::lower_from_hir::BodyExecContext::Paragraph { section_end, .. } => section_end,
        crate::lower_from_hir::BodyExecContext::ProgramEntry
        | crate::lower_from_hir::BodyExecContext::EntryHelper => 0,
    };
    let exit_op = CobolFlowExitOp {
        exit_kind: "section",
        section_end,
    }
    .build(state.ctx, loc)?;
    let _ = block.append_operation(exit_op);
    Ok(())
}

/// Lowers `HirStmt::CheckException`, polling `coblang_rt_m13_take_pending_ec` and routing any
/// raised enabled `EC-*` condition to the highest-priority `USE [GLOBAL] AFTER EXCEPTION` declarative
/// (with `RESUME NEXT STATEMENT` / `RESUME AT proc-name` / fatal unhandled check).
///
/// # Arguments
///
/// * `spec` - Post-statement exception check payload.
/// * `m13_unit` - Optional unit-level M13 declarative & debug metadata.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
#[expect(
    clippy::too_many_lines,
    reason = "Complete post-statement pending-exception polling, declarative routing, and RESUME jump."
)]
pub(crate) fn lower_check_exception_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirCheckExceptionStmt,
    m13_unit: Option<&HirM13UnitSpec>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let raw = state.ctx.raw();
    let i32_ty: Type<'ctx> = IntegerType::new(raw, 32).into();

    let loc_txt = state
        .interner
        .resolve_original(spec.location_sym)
        .unwrap_or("");
    let stmt_txt = state
        .interner
        .resolve_original(spec.statement_sym)
        .unwrap_or("");
    let (lp, ll) = emit_const_slice(loc_txt.as_bytes(), block, prog_ctx, state, loc)?;
    let (sp, sl) = emit_const_slice(stmt_txt.as_bytes(), block, prog_ctx, state, loc)?;
    let ec_id = emit_rt_call(
        RtDispatchTarget::M13TakePendingEc,
        &[lp, ll, sp, sl],
        block,
        prog_ctx,
        state,
        loc,
    )?;

    let zero_32 = emit_i32(0, block, prog_ctx, state, loc);
    let has_ec = append_val(
        block,
        arith::cmpi(raw, CmpiPredicate::Sgt, ec_id, zero_32, loc),
        prog_ctx.fallback_val,
    );

    // Allocate a `resume_sel: i32` slot (`0` = continue, `-1` = fatal exit(1), `>= 1` = `pid + 1`).
    let ptr_ty = llvm_ptr_type(state.ctx);
    let one_64 = emit_i64(1, prog_ctx.entry_block, prog_ctx, state, loc);
    let resume_slot = append_val(
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
    let _ = block.append_operation(llvm::store(
        raw,
        zero_32,
        resume_slot,
        loc,
        llvm::LoadStoreOptions::new(),
    ));

    let then_reg = Region::new();
    let then_blk = then_reg.append_block(Block::new(&[]));
    let rules = m13_unit.map_or(&[][..], |u| u.ec_declaratives.as_slice());
    emit_declarative_rule_chain(
        0,
        rules,
        ec_id,
        resume_slot,
        &then_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = then_blk.append_operation(scf::r#yield(&[], loc));

    let else_reg = Region::new();
    let else_blk = else_reg.append_block(Block::new(&[]));
    let _ = else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(has_ec, &[], then_reg, else_reg, loc));

    // Check `resume_sel` outside `scf.if` so `CobolGotoOp` runs in the enclosing paragraph/entry block!
    let resume_val = append_val(
        block,
        llvm::load(raw, resume_slot, i32_ty, loc, llvm::LoadStoreOptions::new()),
        prog_ctx.fallback_val,
    );
    let is_fatal = append_val(
        block,
        arith::cmpi(raw, CmpiPredicate::Slt, resume_val, zero_32, loc),
        prog_ctx.fallback_val,
    );
    let fat_then = Region::new();
    let fat_blk = fat_then.append_block(Block::new(&[]));
    let one_exit = emit_i32(1, &fat_blk, prog_ctx, state, loc);
    let stop_op = CobolStopRunOp { status: one_exit }.build(state.ctx, loc)?;
    let _ = fat_blk.append_operation(stop_op);
    let _ = fat_blk.append_operation(scf::r#yield(&[], loc));
    let fat_else = Region::new();
    let fat_skip = fat_else.append_block(Block::new(&[]));
    let _ = fat_skip.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(is_fatal, &[], fat_then, fat_else, loc));

    // If `resume_val >= 1` (`RESUME AT proc-name`), jump via `CobolGotoOp`!
    let normal_count = prog_ctx.prog.fallthrough_limit_pid.map_or(
        u32::try_from(prog_ctx.prog.procedures.len()).unwrap_or(0),
        coblang_common::ProcedureId::as_u32,
    );
    if normal_count > 0 {
        let is_goto = append_val(
            block,
            arith::cmpi(raw, CmpiPredicate::Sgt, resume_val, zero_32, loc),
            prog_ctx.fallback_val,
        );
        let goto_then = Region::new();
        let goto_blk = goto_then.append_block(Block::new(&[]));
        let all_targets: Vec<i32> = (0..normal_count)
            .map(|p| i32::try_from(p).unwrap_or(0))
            .collect();
        let goto_op = CobolGotoOp {
            targets: &all_targets,
            alter_slot_sym: "",
            selector: Some(resume_val),
        }
        .build(state.ctx, loc)?;
        let _ = goto_blk.append_operation(goto_op);
        let _ = goto_blk.append_operation(scf::r#yield(&[], loc));
        let goto_else = Region::new();
        let goto_skip = goto_else.append_block(Block::new(&[]));
        let _ = goto_skip.append_operation(scf::r#yield(&[], loc));
        let _ = block.append_operation(scf::r#if(is_goto, &[], goto_then, goto_else, loc));
    }

    Ok(())
}

/// Recursively emits prioritized `scf.if` dispatch across `rules[idx..]`, falling back to
/// `coblang_rt_m13_check_fatal_unhandled` when no rule matches or declarative re-entrancy is active.
///
/// # Arguments
///
/// * `idx` - Current candidate rule index.
/// * `rules` - Ordered [`HirEcDeclarativeRule`] slice.
/// * `ec_id` - Raised `ExceptionCode::to_abi_id()` SSA value.
/// * `resume_slot` - Stack slot storing post-declarative `RESUME` disposition.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
#[expect(
    clippy::too_many_arguments,
    reason = "Recursive `scf.if` chain carrying rule index, `ec_id`, `resume_slot`, and MLIR context."
)]
#[expect(
    clippy::too_many_lines,
    reason = "Complete prioritized `scf.if` declarative chain with re-entrancy guard and unhandled fallback."
)]
fn emit_declarative_rule_chain<'ctx, 'b, 'e: 'b>(
    idx: usize,
    rules: &[HirEcDeclarativeRule],
    ec_id: Value<'ctx, 'b>,
    resume_slot: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let raw = state.ctx.raw();
    let i32_ty: Type<'ctx> = IntegerType::new(raw, 32).into();

    let Some(rule) = rules.get(idx) else {
        let zero_32 = emit_i32(0, block, prog_ctx, state, loc);
        let should_abort = emit_rt_call(
            RtDispatchTarget::M13CheckFatalUnhandled,
            &[ec_id, zero_32],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        let abort_cond = append_val(
            block,
            arith::cmpi(raw, CmpiPredicate::Ne, should_abort, zero_32, loc),
            prog_ctx.fallback_val,
        );
        let neg_one = emit_i32(-1, block, prog_ctx, state, loc);
        let sel_v = append_val(
            block,
            arith::select(abort_cond, neg_one, zero_32, loc),
            prog_ctx.fallback_val,
        );
        let _ = block.append_operation(llvm::store(
            raw,
            sel_v,
            resume_slot,
            loc,
            llvm::LoadStoreOptions::new(),
        ));
        return Ok(());
    };

    let matched = emit_rule_ec_match(&rule.ec_codes, ec_id, block, prog_ctx, state, loc);
    let m_then = Region::new();
    let m_blk = m_then.append_block(Block::new(&[]));

    let entered = emit_rt_call(
        RtDispatchTarget::M13EnterDecl,
        &[],
        &m_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let one_32 = emit_i32(1, &m_blk, prog_ctx, state, loc);
    let can_enter = append_val(
        &m_blk,
        arith::cmpi(raw, CmpiPredicate::Eq, entered, one_32, loc),
        prog_ctx.fallback_val,
    );

    let enter_then = Region::new();
    let enter_blk = enter_then.append_block(Block::new(&[]));
    let target_prog_opt = if rule.target.owner_unit == prog_ctx.prog.id {
        None
    } else {
        state
            .program_symbols
            .get(&rule.target.owner_unit)
            .map(|(c, _)| c.clone())
    };
    let pfm_op = CobolPerformRangeOp {
        start_proc: rule.target.proc_start.as_u32(),
        end_proc: rule.target.proc_end.as_u32(),
    }
    .build_with_target_prog(target_prog_opt.as_deref(), state.ctx, loc)?;
    let _ = enter_blk.append_operation(pfm_op);

    let fin_64 = emit_rt_call(
        RtDispatchTarget::M13FinishDecl,
        &[ec_id],
        &enter_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let fin_32 = append_val(
        &enter_blk,
        arith::trunci(fin_64, i32_ty, loc),
        prog_ctx.fallback_val,
    );
    let _ = enter_blk.append_operation(llvm::store(
        raw,
        fin_32,
        resume_slot,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let _ = enter_blk.append_operation(scf::r#yield(&[], loc));

    let enter_else = Region::new();
    let reentrant_blk = enter_else.append_block(Block::new(&[]));
    // Re-entrant exception inside declarative: check fatal unhandled directly.
    emit_declarative_rule_chain(
        rules.len(),
        rules,
        ec_id,
        resume_slot,
        &reentrant_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = reentrant_blk.append_operation(scf::r#yield(&[], loc));
    let _ = m_blk.append_operation(scf::r#if(can_enter, &[], enter_then, enter_else, loc));
    let _ = m_blk.append_operation(scf::r#yield(&[], loc));

    let m_else = Region::new();
    let next_rule_blk = m_else.append_block(Block::new(&[]));
    emit_declarative_rule_chain(
        idx + 1,
        rules,
        ec_id,
        resume_slot,
        &next_rule_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = next_rule_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(matched, &[], m_then, m_else, loc));
    Ok(())
}

/// Builds an `i1` predicate testing whether `ec_id` matches any code in `ec_codes` (`0` = `EC-ALL`).
///
/// # Arguments
///
/// * `ec_codes` - Candidate `ExceptionCode::to_abi_id()` codes (`0` = `EC-ALL`).
/// * `ec_id` - Raised `ExceptionCode::to_abi_id()` SSA value.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_rule_ec_match<'ctx, 'b, 'e: 'b>(
    ec_codes: &[u16],
    ec_id: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    let raw = state.ctx.raw();
    let i1_ty: Type<'ctx> = IntegerType::new(raw, 1).into();
    if ec_codes.contains(&0) {
        return append_val(
            block,
            arith::constant(
                raw,
                melior::ir::attribute::IntegerAttribute::new(i1_ty, 1).into(),
                loc,
            ),
            prog_ctx.fallback_val,
        );
    }
    let mut acc = append_val(
        block,
        arith::constant(
            raw,
            melior::ir::attribute::IntegerAttribute::new(i1_ty, 0).into(),
            loc,
        ),
        prog_ctx.fallback_val,
    );
    for &c in ec_codes {
        let cv = emit_i32(i32::from(c), block, prog_ctx, state, loc);
        let eq = append_val(
            block,
            arith::cmpi(raw, CmpiPredicate::Eq, ec_id, cv, loc),
            prog_ctx.fallback_val,
        );
        acc = append_val(block, arith::ori(acc, eq, loc), prog_ctx.fallback_val);
    }
    acc
}

/// Lowers `HirStmt::DebugTrace` (NIST CCVS-85 `DB` `DEBUG-ITEM` population & `USE FOR DEBUGGING` dispatch).
///
/// # Arguments
///
/// * `spec` - Debug trace payload.
/// * `m13_unit` - Optional unit-level M13 metadata.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if MLIR op construction fails.
#[expect(
    clippy::too_many_lines,
    reason = "Complete NIST CCVS-85 `DEBUG-ITEM` population and `USE FOR DEBUGGING` section invocation."
)]
pub(crate) fn lower_debug_trace_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirDebugTraceStmt,
    m13_unit: Option<&HirM13UnitSpec>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let Some(unit) = m13_unit else {
        return Ok(());
    };
    if unit.debugging_mode != HirDebugCompileMode::WithDebuggingMode {
        return Ok(());
    }
    let Some(dbg_item_id) = unit.debug_item else {
        return Ok(());
    };
    let Some(Some(dbg_loc)) = prog_ctx.storage.items.get(dbg_item_id.as_u32() as usize) else {
        return Ok(());
    };

    let loc = state.loc(spec.span)?;
    let raw = state.ctx.raw();
    let dbg_ptr = emit_storage_ptr(
        &dbg_loc.root_storage_sym,
        dbg_loc.base_byte_offset,
        dbg_loc.elem_byte_len.max(132),
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let dbg_len = emit_i64(
        i64::from(dbg_loc.elem_byte_len.max(132)),
        block,
        prog_ctx,
        state,
        loc,
    );

    let line_num = if spec.line_text.trim().is_empty() {
        state
            .source_map
            .resolve_span(spec.span)
            .map_or(0, |sl| i32::try_from(sl.line.get()).unwrap_or(0))
    } else {
        spec.line_text.trim().parse::<i32>().unwrap_or(0)
    };
    let line_v = emit_i32(line_num, block, prog_ctx, state, loc);
    let (np, nl) = emit_const_slice(spec.name_text.as_bytes(), block, prog_ctx, state, loc)?;

    let sub_cnt = emit_i32(
        i32::try_from(spec.subscripts.len().min(3)).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let s1 = if let Some(e) = spec.subscripts.first() {
        eval_expr_as_i64(e, block, prog_ctx, state)?
    } else {
        emit_i64(0, block, prog_ctx, state, loc)
    };
    let s2 = if let Some(e) = spec.subscripts.get(1) {
        eval_expr_as_i64(e, block, prog_ctx, state)?
    } else {
        emit_i64(0, block, prog_ctx, state, loc)
    };
    let s3 = if let Some(e) = spec.subscripts.get(2) {
        eval_expr_as_i64(e, block, prog_ctx, state)?
    } else {
        emit_i64(0, block, prog_ctx, state, loc)
    };

    let (cp, cl) = match &spec.contents {
        HirDebugContents::Spaces => emit_const_slice(b"", block, prog_ctx, state, loc)?,
        HirDebugContents::StaticText(txt) => {
            emit_const_slice(txt.as_bytes(), block, prog_ctx, state, loc)?
        }
        HirDebugContents::PlaceValue(pl) => {
            let v = lower_place_view(pl, block, prog_ctx, state)?;
            (v.ptr, v.len_i64)
        }
    };

    let should_fire = emit_rt_call(
        RtDispatchTarget::M13DebugPrepare,
        &[
            dbg_ptr, dbg_len, line_v, np, nl, sub_cnt, s1, s2, s3, cp, cl,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let one_32 = emit_i32(1, block, prog_ctx, state, loc);
    let cond = append_val(
        block,
        arith::cmpi(raw, CmpiPredicate::Eq, should_fire, one_32, loc),
        prog_ctx.fallback_val,
    );

    let then_reg = Region::new();
    let then_blk = then_reg.append_block(Block::new(&[]));
    let pfm_op = CobolPerformRangeOp {
        start_proc: spec.section.proc_start.as_u32(),
        end_proc: spec.section.proc_end.as_u32(),
    }
    .build(state.ctx, loc)?;
    let _ = then_blk.append_operation(pfm_op);
    let zero_32 = emit_i32(0, &then_blk, prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::M13FinishDecl,
        &[zero_32],
        &then_blk,
        prog_ctx,
        state,
        loc,
    )?;
    let _ = then_blk.append_operation(scf::r#yield(&[], loc));

    let else_reg = Region::new();
    let else_blk = else_reg.append_block(Block::new(&[]));
    let _ = else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(cond, &[], then_reg, else_reg, loc));

    Ok(())
}
