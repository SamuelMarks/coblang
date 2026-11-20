//! Milestone M13 `>>TURN` directives, `RAISE`/`RESUME`, `DECLARATIVES` (`USE AFTER EXCEPTION`
//! & cross-declarative control-flow validation), and NIST CCVS-85 `DB` Debug Module (`USE FOR DEBUGGING`
//! + `DEBUG-ITEM`) semantic analyzer and HIR instrumenter for `coblang-sema`.

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{
    DataItemId, Dialect, FileConnectorId, ProcedureId, ProgramUnitId, Result, SourceSpan, SymbolId,
};
use coblang_diag::{Diagnostic, DiagnosticCode, WarningGroup};
use coblang_hir::{
    DynamicOdoPresence, HirDataItem, HirDebugCompileMode, HirDebugContents, HirDebugTraceStmt,
    HirDeclRef, HirEcDeclarativeRule, HirExpr, HirGlobalFlag, HirM13UnitSpec, HirParagraph,
    HirPlace, HirRaiseStmt, HirRaiseTarget, HirResumeStmt, HirResumeTarget, HirSetTurnStmt,
    HirStmt, HirTurnMode, HirTurnTarget, HirType, JustifiedSpec,
};
use coblang_pp::{TurnCheckingState, TurnDirectiveRecord};
use coblang_rt_core::exceptions::ExceptionCode;
use coblang_syntax::ast::{
    DataRef, DataRefModifier, DebugUseTarget, DebuggingMode, DeclarativeSection, Expr,
    GlobalVisibility, ProcedureDivision, QualifiedName, UseClause,
};

use crate::lower_hir::{UnitLowerCtx, resolve_place};
use crate::symbols::ResolvedSymbol;

/// Per-unit resolved `USE FOR DEBUGGING` rules used to instrument `HirProgram` statements & procedures.
#[derive(Debug, Clone)]
pub struct UnitDebugInstrumentationPlan {
    /// Whether `SOURCE-COMPUTER ... WITH DEBUGGING MODE` is active on this unit.
    pub debugging_mode: HirDebugCompileMode,
    /// `01 DEBUG-ITEM` root `DataItemId` in this unit's `data_items`.
    pub debug_item: Option<DataItemId>,
    /// Optional `USE FOR DEBUGGING ON ALL PROCEDURES` declarative target.
    pub all_procedures: Option<HirDeclRef>,
    /// Specific `USE FOR DEBUGGING ON proc-name` map (`ProcedureId -> (proc_name_str, HirDeclRef)`).
    pub proc_hooks: BTreeMap<ProcedureId, (String, HirDeclRef)>,
    /// Procedure display names indexed by `ProcedureId` (for `ALL PROCEDURES` and `ALTER`).
    pub proc_names: BTreeMap<ProcedureId, String>,
    /// Monitored `DataItemId -> (display_name, all_references, HirDeclRef)` map.
    pub data_hooks: BTreeMap<DataItemId, (String, MonitoredRefKind, HirDeclRef)>,
    /// Monitored `FileConnectorId -> HirDeclRef` map (`USE FOR DEBUGGING ON file-name`).
    pub file_hooks: Vec<(FileConnectorId, HirDeclRef)>,
    /// Detailed file debug hook metadata (`FileConnectorId -> (file_name, first_record_place, HirDeclRef)`).
    pub file_hook_info: BTreeMap<FileConnectorId, (String, Option<HirPlace>, HirDeclRef)>,
    /// `ProcedureId`s belonging to `USE FOR DEBUGGING` declarative sections (excluded from target & body instrumentation).
    pub debug_decl_pids: BTreeSet<u32>,
}

impl Default for UnitDebugInstrumentationPlan {
    /// Creates an empty [`UnitDebugInstrumentationPlan`] with `HirDebugCompileMode::Disabled`.
    fn default() -> Self {
        Self {
            debugging_mode: HirDebugCompileMode::Disabled,
            debug_item: None,
            all_procedures: None,
            proc_hooks: BTreeMap::new(),
            proc_names: BTreeMap::new(),
            data_hooks: BTreeMap::new(),
            file_hooks: Vec::new(),
            file_hook_info: BTreeMap::new(),
            debug_decl_pids: BTreeSet::new(),
        }
    }
}

/// Scope of data-item monitoring in `USE FOR DEBUGGING ON [ALL REFERENCES OF] identifier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MonitoredRefKind {
    /// `USE FOR DEBUGGING ON identifier` (write/modification hooks).
    WriteOnly,
    /// `USE FOR DEBUGGING ON ALL REFERENCES OF identifier` (read + write hooks).
    AllReferences,
}

/// Validates unit-scoped `>>TURN` directives (`syn_misc/0074_turn_directive`) and returns ordered
/// `(byte_offset, HirSetTurnStmt)` records lying inside `unit_span`.
///
/// # Arguments
///
/// * `turn_records` - Compilation-group [`TurnDirectiveRecord`] list from `coblang-pp`.
/// * `unit_span` - Enclosing unit [`SourceSpan`].
/// * `ctx` - Active [`UnitLowerCtx`].
pub(crate) fn collect_unit_turn_directives(
    turn_records: &[TurnDirectiveRecord],
    unit_span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<(u32, HirSetTurnStmt)> {
    let unit_start = unit_span.start.as_u32();
    let unit_end = unit_start.saturating_add(unit_span.len.as_u32());
    let mut out = Vec::new();

    for rec in turn_records {
        let off = rec.span.start.as_u32();
        if rec.span.file_id != unit_span.file_id || off < unit_start || off > unit_end {
            continue;
        }
        let mode = match rec.state {
            TurnCheckingState::Disabled => HirTurnMode::Off,
            TurnCheckingState::Enabled => HirTurnMode::On,
            TurnCheckingState::EnabledWithLocation => HirTurnMode::OnWithLocation,
        };
        if rec.exception_names.is_empty() {
            continue;
        }

        let mut saw_all = false;
        let mut allows_file_names = false;
        let mut codes = Vec::new();

        for raw_name in &rec.exception_names {
            let up = raw_name.trim().to_ascii_uppercase();
            if up == "EC-ALL" {
                saw_all = true;
                allows_file_names = false;
                continue;
            }
            if let Ok(ec) = ExceptionCode::from_cobol_name(&up) {
                allows_file_names = up == "EC-I-O" || up.starts_with("EC-I-O-");
                codes.push(ec.to_abi_id());
                continue;
            }
            if allows_file_names {
                let file_exists = ctx.interner.intern(&up).ok().and_then(|fsym| {
                    ctx.symtab
                        .resolve_symbol_silent(ctx.scope, fsym, &[])
                        .filter(|r| matches!(r, ResolvedSymbol::File(_)))
                });
                if file_exists.is_none() {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0004UndefinedSymbol,
                        Some(rec.span),
                        format!("file '{raw_name}' does not exist"),
                    ));
                }
            } else if up.starts_with("EC-") {
                ctx.sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::DialectExtension,
                    Some(rec.span),
                    format!("unrecognized exception condition '{raw_name}' ignored"),
                ));
            } else {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0004UndefinedSymbol,
                    Some(rec.span),
                    format!("invalid exception-name: {raw_name}"),
                ));
            }
        }

        let target = if saw_all {
            HirTurnTarget::All
        } else if !codes.is_empty() {
            HirTurnTarget::Codes(codes)
        } else {
            continue;
        };

        out.push((
            off,
            HirSetTurnStmt {
                target,
                mode,
                span: rec.span,
            },
        ));
    }

    out
}

/// Builds Milestone M13 `EC-*` exception declarative rules and NIST `DB` debug instrumentation plan for a unit.
///
/// # Arguments
///
/// * `unit_id` - Owning [`ProgramUnitId`].
/// * `debug_mode` - AST `SOURCE-COMPUTER ... WITH DEBUGGING MODE` setting.
/// * `proc_div` - Unit [`ProcedureDivision`].
/// * `normal_proc_cnt` - Starting `ProcedureId` index of declarative sections.
/// * `inherited_ec_decls` - Parent unit's `EC-*` declaratives (filtered to `GLOBAL`).
/// * `ctx` - Active [`UnitLowerCtx`].
#[expect(
    clippy::too_many_lines,
    reason = "Validates and compiles all EC-* exception and NIST DB debugging declaratives for a unit."
)]
pub(crate) fn build_m13_unit_rules(
    unit_id: ProgramUnitId,
    debug_mode: DebuggingMode,
    proc_div: &ProcedureDivision,
    normal_proc_cnt: u32,
    inherited_ec_decls: &[HirEcDeclarativeRule],
    ctx: &mut UnitLowerCtx<'_>,
) -> Result<(HirM13UnitSpec, UnitDebugInstrumentationPlan)> {
    let debugging_mode = match debug_mode {
        DebuggingMode::Disabled => HirDebugCompileMode::Disabled,
        DebuggingMode::Enabled => HirDebugCompileMode::WithDebuggingMode,
    };

    let dbg_sym = ctx.interner.intern("DEBUG-ITEM")?;
    let debug_item = match ctx.symtab.resolve_symbol_silent(ctx.scope, dbg_sym, &[]) {
        Some(ResolvedSymbol::DataItem(id)) => Some(id),
        _ => None,
    };

    let mut plan = UnitDebugInstrumentationPlan {
        debugging_mode,
        debug_item,
        ..UnitDebugInstrumentationPlan::default()
    };

    if let Some(sc) = ctx.symtab.scope(ctx.scope) {
        for p in &sc.procedures {
            let pname = ctx
                .interner
                .resolve_canonical(p.name)
                .unwrap_or("")
                .to_owned();
            plan.proc_names.insert(p.id, pname);
        }
    }

    let mut pre_pid = normal_proc_cnt;
    for decl in &proc_div.declaratives {
        let para_cnt = u32::try_from(decl.paragraphs.len()).unwrap_or(0);
        if matches!(decl.use_clause, UseClause::ForDebugging { .. }) {
            for p_off in 0..=para_cnt {
                let _ = plan.debug_decl_pids.insert(pre_pid.saturating_add(p_off));
            }
        }
        pre_pid = pre_pid.saturating_add(1).saturating_add(para_cnt);
    }

    let mut ec_declaratives = Vec::new();
    let mut seen_all_procs = false;
    let mut seen_debug_procs: BTreeSet<ProcedureId> = BTreeSet::new();
    let mut seen_debug_items: BTreeSet<DataItemId> = BTreeSet::new();
    let mut seen_debug_files: BTreeSet<FileConnectorId> = BTreeSet::new();
    let mut max_debug_content_bytes = 76_u32;

    let mut pid = normal_proc_cnt;
    for decl in &proc_div.declaratives {
        let para_cnt = u32::try_from(decl.paragraphs.len()).unwrap_or(0);
        let target = HirDeclRef {
            owner_unit: unit_id,
            proc_start: ProcedureId::new(pid),
            proc_end: ProcedureId::new(pid + para_cnt),
        };
        pid = pid.saturating_add(1).saturating_add(para_cnt);

        match &decl.use_clause {
            UseClause::AfterException { global, targets } => {
                collect_ec_declarative_rule(
                    decl,
                    *global,
                    targets,
                    target,
                    &mut ec_declaratives,
                    ctx,
                );
            }
            UseClause::ForDebugging { targets } => {
                if matches!(ctx.dialect.base_dialect, Dialect::Cobol85 | Dialect::Ibm) {
                    ctx.sink.emit(Diagnostic::warning(
                        DiagnosticCode::W0002Obsolete,
                        WarningGroup::Obsolete,
                        Some(decl.span),
                        String::from("USE FOR DEBUGGING is obsolete in COBOL 85"),
                    ));
                }
                for dbg_target in targets {
                    resolve_single_debug_target(
                        dbg_target,
                        decl.span,
                        target,
                        &mut seen_all_procs,
                        &mut seen_debug_procs,
                        &mut seen_debug_items,
                        &mut seen_debug_files,
                        &mut max_debug_content_bytes,
                        &mut plan,
                        ctx,
                    );
                }
            }
            UseClause::BeforeReporting { .. } => {}
        }
    }

    for inh in inherited_ec_decls {
        if inh.visibility == HirGlobalFlag::Global {
            ec_declaratives.push(inh.clone());
        }
    }

    if max_debug_content_bytes > 76
        && let Some(root_id) = debug_item
    {
        widen_debug_item_contents(root_id, max_debug_content_bytes, &mut ctx.layout.items);
    }

    let spec = HirM13UnitSpec {
        unit_id,
        debugging_mode,
        debug_item,
        ec_declaratives,
        file_debug_hooks: plan.file_hooks.clone(),
    };

    Ok((spec, plan))
}

/// Extracts `EC-*` / `EC-ALL` targets from a `USE AFTER EXCEPTION` declarative section.
///
/// # Arguments
///
/// * `decl` - Source [`DeclarativeSection`].
/// * `global` - AST `GLOBAL` visibility.
/// * `targets` - Target symbols.
/// * `target` - Procedure range [`HirDeclRef`].
/// * `out` - Output [`HirEcDeclarativeRule`] vector.
/// * `ctx` - Active [`UnitLowerCtx`].
fn collect_ec_declarative_rule(
    decl: &DeclarativeSection,
    global: GlobalVisibility,
    targets: &[SymbolId],
    target: HirDeclRef,
    out: &mut Vec<HirEcDeclarativeRule>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut ec_codes = Vec::new();
    for &tsym in targets {
        let canon = ctx.interner.resolve_canonical(tsym).unwrap_or("");
        if canon == "EC-ALL" {
            ec_codes.push(0);
        } else if let Ok(ec) = ExceptionCode::from_cobol_name(canon) {
            ec_codes.push(ec.to_abi_id());
        } else if canon.starts_with("EC-") {
            let orig = ctx.interner.resolve_original(tsym).unwrap_or(canon);
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0004UndefinedSymbol,
                Some(decl.span),
                format!("unknown exception condition '{orig}'"),
            ));
        }
    }
    if !ec_codes.is_empty() {
        let visibility = match global {
            GlobalVisibility::Local => HirGlobalFlag::Local,
            GlobalVisibility::Global => HirGlobalFlag::Global,
        };
        out.push(HirEcDeclarativeRule {
            target,
            visibility,
            ec_codes,
        });
    }
}

/// Validates and registers a single [`DebugUseTarget`] inside `USE FOR DEBUGGING ON ...`.
///
/// # Arguments
///
/// * `dbg_target` - Parsed [`DebugUseTarget`].
/// * `decl_span` - Enclosing declarative span.
/// * `target` - Declarative [`HirDeclRef`].
/// * `seen_all_procs` - Tracks duplicate `ALL PROCEDURES`.
/// * `seen_procs` - Tracks duplicate procedure targets.
/// * `seen_items` - Tracks duplicate data-item targets.
/// * `seen_files` - Tracks duplicate file targets.
/// * `max_content_bytes` - Maximum byte size among monitored items.
/// * `plan` - Output [`UnitDebugInstrumentationPlan`].
/// * `ctx` - Active [`UnitLowerCtx`].
#[expect(
    clippy::too_many_arguments,
    reason = "Tracks duplicate sets across procedures, data items, and file connectors."
)]
#[expect(
    clippy::too_many_lines,
    reason = "Resolves ALL PROCEDURES, procedure names, file connectors, and qualified data items."
)]
fn resolve_single_debug_target(
    dbg_target: &DebugUseTarget,
    decl_span: SourceSpan,
    target: HirDeclRef,
    seen_all_procs: &mut bool,
    seen_procs: &mut BTreeSet<ProcedureId>,
    seen_items: &mut BTreeSet<DataItemId>,
    seen_files: &mut BTreeSet<FileConnectorId>,
    max_content_bytes: &mut u32,
    plan: &mut UnitDebugInstrumentationPlan,
    ctx: &mut UnitLowerCtx<'_>,
) {
    match dbg_target {
        DebugUseTarget::AllProcedures(sp) => {
            if *seen_all_procs || !seen_procs.is_empty() {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0004UndefinedSymbol,
                    Some(*sp),
                    String::from("duplicate USE DEBUGGING ON ALL PROCEDURES"),
                ));
            }
            *seen_all_procs = true;
            plan.all_procedures = Some(target);
        }
        DebugUseTarget::AllReferencesOf(dref) => {
            if !validate_debug_dref_plain(dref, ctx) {
                return;
            }
            if let Some(ResolvedSymbol::DataItem(id)) = ctx.symtab.resolve_symbol(
                ctx.scope,
                dref.name.base,
                &dref.name.qualifiers,
                dref.name.span,
                ctx.interner,
                ctx.sink,
            ) {
                register_debug_data_item(
                    id,
                    dref,
                    MonitoredRefKind::AllReferences,
                    target,
                    seen_items,
                    max_content_bytes,
                    plan,
                    ctx,
                );
            }
        }
        DebugUseTarget::Target(dref) => {
            if !validate_debug_dref_plain(dref, ctx) {
                return;
            }
            let sec_qual = dref.name.qualifiers.first().copied();
            if dref.name.qualifiers.len() <= 1
                && let Some(sc) = ctx.symtab.scope(ctx.scope)
            {
                let proc_matches: Vec<ProcedureId> = sc
                    .procedures
                    .iter()
                    .filter(|p| {
                        p.name == dref.name.base && sec_qual.is_none_or(|sq| p.section == Some(sq))
                    })
                    .map(|p| p.id)
                    .collect();
                if proc_matches.len() == 1
                    && let Some(&pid) = proc_matches.first()
                {
                    if plan.debug_decl_pids.contains(&pid.as_u32()) {
                        let orig = ctx.interner.resolve_original(dref.name.base).unwrap_or("?");
                        ctx.sink.emit(Diagnostic::error(
                            DiagnosticCode::E0008TypeMismatch,
                            Some(dref.span),
                            format!("DEBUGGING target '{orig}' may not be in DECLARATIVES"),
                        ));
                        return;
                    }
                    if *seen_all_procs || !seen_procs.insert(pid) {
                        let orig = ctx.interner.resolve_original(dref.name.base).unwrap_or("?");
                        ctx.sink.emit(Diagnostic::error(
                            DiagnosticCode::E0004UndefinedSymbol,
                            Some(dref.span),
                            format!("duplicate DEBUGGING target: '{orig}'"),
                        ));
                        return;
                    }
                    let pname = ctx
                        .interner
                        .resolve_canonical(dref.name.base)
                        .unwrap_or("")
                        .to_owned();
                    plan.proc_hooks.insert(pid, (pname, target));
                    return;
                }
            }

            match ctx.symtab.resolve_symbol(
                ctx.scope,
                dref.name.base,
                &dref.name.qualifiers,
                dref.span.merge(decl_span),
                ctx.interner,
                ctx.sink,
            ) {
                Some(ResolvedSymbol::File(fid)) => {
                    if !seen_files.insert(fid) {
                        let orig = ctx.interner.resolve_original(dref.name.base).unwrap_or("?");
                        ctx.sink.emit(Diagnostic::error(
                            DiagnosticCode::E0004UndefinedSymbol,
                            Some(dref.span),
                            format!("duplicate DEBUGGING target: '{orig}'"),
                        ));
                        return;
                    }
                    let fname = ctx
                        .interner
                        .resolve_canonical(dref.name.base)
                        .unwrap_or("")
                        .to_owned();
                    let rec_place = ctx.files.get(fid.as_u32() as usize).and_then(|fc| {
                        *max_content_bytes = (*max_content_bytes).max(fc.max_record_bytes);
                        let rec_id = *fc.records.first()?;
                        let rec_item = ctx.layout.items.get(rec_id.as_u32() as usize)?;
                        Some(HirPlace {
                            item: rec_id,
                            static_offset: coblang_common::ByteOffset::ZERO,
                            subscripts: Vec::new(),
                            refmod: None,
                            effective_ty: rec_item.ty.clone(),
                            check_mask: coblang_hir::ExceptionCheckMask::NONE,
                            span: dref.span,
                        })
                    });
                    plan.file_hooks.push((fid, target));
                    plan.file_hook_info.insert(fid, (fname, rec_place, target));
                }
                Some(ResolvedSymbol::DataItem(id)) => {
                    register_debug_data_item(
                        id,
                        dref,
                        MonitoredRefKind::WriteOnly,
                        target,
                        seen_items,
                        max_content_bytes,
                        plan,
                        ctx,
                    );
                }
                _ => {}
            }
        }
    }
}

/// Emits errors if `dref` inside `USE FOR DEBUGGING ON` carries subscripts or reference modification (`syn_misc/0091`).
///
/// # Arguments
///
/// * `dref` - Candidate [`DataRef`].
/// * `ctx` - Active [`UnitLowerCtx`].
fn validate_debug_dref_plain(dref: &DataRef, ctx: &mut UnitLowerCtx<'_>) -> bool {
    let mut ok = true;
    if dref.refmod.is_some() {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(dref.span),
            String::from("DEBUGGING target may not be reference modified"),
        ));
        ok = false;
    }
    if !dref.subscripts.is_empty() {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(dref.span),
            String::from("DEBUGGING target may not be subscripted"),
        ));
        ok = false;
    }
    ok
}

/// Registers a monitored `DataItemId` (and checks for `DEBUG-ITEM` self-monitoring and duplicates).
///
/// # Arguments
///
/// * `id` - Resolved [`DataItemId`].
/// * `dref` - Source [`DataRef`].
/// * `mode` - `WriteOnly` vs `AllReferences`.
/// * `target` - Declarative [`HirDeclRef`].
/// * `seen_items` - Duplicate tracker.
/// * `max_content_bytes` - Maximum byte size tracker.
/// * `plan` - Output [`UnitDebugInstrumentationPlan`].
/// * `ctx` - Active [`UnitLowerCtx`].
#[expect(
    clippy::too_many_arguments,
    reason = "Registers monitored DataItemId and updates duplicate + DEBUG-CONTENTS width trackers."
)]
fn register_debug_data_item(
    id: DataItemId,
    dref: &DataRef,
    mode: MonitoredRefKind,
    target: HirDeclRef,
    seen_items: &mut BTreeSet<DataItemId>,
    max_content_bytes: &mut u32,
    plan: &mut UnitDebugInstrumentationPlan,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let canon = ctx
        .interner
        .resolve_canonical(dref.name.base)
        .unwrap_or("")
        .to_owned();
    let orig = ctx
        .interner
        .resolve_original(dref.name.base)
        .unwrap_or(&canon)
        .to_owned();
    if matches!(
        canon.as_str(),
        "DEBUG-ITEM"
            | "DEBUG-LINE"
            | "DEBUG-NAME"
            | "DEBUG-SUB-1"
            | "DEBUG-SUB-2"
            | "DEBUG-SUB-3"
            | "DEBUG-CONTENTS"
    ) {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(dref.span),
            format!("DEBUGGING target may not be '{orig}'"),
        ));
        return;
    }
    if !seen_items.insert(id) {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0004UndefinedSymbol,
            Some(dref.span),
            format!("duplicate DEBUGGING target: '{orig}'"),
        ));
        return;
    }
    if let Some(it) = ctx.layout.items.get(id.as_u32() as usize) {
        *max_content_bytes = (*max_content_bytes).max(it.ty.static_byte_size());
    }
    let mut full_name = canon;
    for &q in &dref.name.qualifiers {
        if let Ok(qc) = ctx.interner.resolve_canonical(q) {
            full_name.push_str(" OF ");
            full_name.push_str(qc);
        }
    }
    plan.data_hooks.insert(id, (full_name, mode, target));
}

/// Widens `02 DEBUG-CONTENTS` and `01 DEBUG-ITEM` when a monitored item exceeds 76 bytes.
///
/// # Arguments
///
/// * `root_id` - `01 DEBUG-ITEM` [`DataItemId`].
/// * `content_len` - New `DEBUG-CONTENTS` byte width (`> 76`).
/// * `items` - Unit `HirDataItem` arena.
fn widen_debug_item_contents(root_id: DataItemId, content_len: u32, items: &mut [HirDataItem]) {
    let last_child = items
        .get(root_id.as_u32() as usize)
        .and_then(|r| r.children.last().copied());
    if let Some(cid) = last_child
        && let Some(c_item) = items.get_mut(cid.as_u32() as usize)
    {
        c_item.ty = HirType::Alphanumeric {
            byte_len: content_len,
            justified: JustifiedSpec::Left,
        };
        c_item.total_byte_size = content_len;
    }
    let total = 56_u32.saturating_add(content_len);
    if let Some(root) = items.get_mut(root_id.as_u32() as usize) {
        root.ty = HirType::Group {
            static_byte_len: total,
            odo_presence: DynamicOdoPresence::StaticSize,
        };
        root.total_byte_size = total;
    }
}

/// Lowers `Stmt::Raise` (`RAISE [EXCEPTION ec-name | obj]`) into [`HirStmt::Raise`] + [`HirStmt::CheckException`].
///
/// # Arguments
///
/// * `target` - Optional AST [`Expr`] target (`None` = `RAISE` re-raising `LAST EXCEPTION`).
/// * `span` - Statement [`SourceSpan`].
/// * `out` - Output [`HirStmt`] vector.
/// * `ctx` - Active [`UnitLowerCtx`].
pub(crate) fn lower_raise_stmt(
    target: Option<&Expr>,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let hir_target = match target {
        None => HirRaiseTarget::LastException,
        Some(Expr::Data(dref))
            if dref.modifier == DataRefModifier::Plain
                && dref.name.qualifiers.is_empty()
                && dref.subscripts.is_empty()
                && dref.refmod.is_none() =>
        {
            let canon = ctx.interner.resolve_canonical(dref.name.base).unwrap_or("");
            if let Ok(ec) = ExceptionCode::from_cobol_name(canon) {
                HirRaiseTarget::EcCode(ec.to_abi_id())
            } else if canon.starts_with("EC-") {
                let orig = ctx
                    .interner
                    .resolve_original(dref.name.base)
                    .unwrap_or(canon);
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0004UndefinedSymbol,
                    Some(dref.span),
                    format!("unknown exception condition '{orig}' in RAISE"),
                ));
                HirRaiseTarget::EcCode(ExceptionCode::EcUser.to_abi_id())
            } else {
                let _ = resolve_place(dref, ctx);
                HirRaiseTarget::EcCode(ExceptionCode::EcOoException.to_abi_id())
            }
        }
        Some(other_expr) => {
            let _ = crate::lower_hir::lower_expr_with_cat(other_expr, ctx);
            HirRaiseTarget::EcCode(ExceptionCode::EcOoException.to_abi_id())
        }
    };

    let location_sym = ctx.current_location_prefix_sym();
    let statement_sym = ctx.builtins_get("RAISE");
    out.push(HirStmt::Raise(HirRaiseStmt {
        target: hir_target,
        location_sym,
        statement_sym,
        span,
    }));
    out.push(HirStmt::CheckException(
        coblang_hir::HirCheckExceptionStmt {
            location_sym,
            statement_sym,
            span,
        },
    ));
}

/// Declarative context of a `RESUME` statement.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeclarativeRegion {
    /// Outside `DECLARATIVES`.
    NormalBody,
    /// Inside `DECLARATIVES`.
    InDeclaratives,
}

/// Lowers `Stmt::Resume` (`RESUME [AT NEXT STATEMENT | AT proc-name]`) into [`HirStmt::Resume`].
///
/// # Arguments
///
/// * `target` - Optional AST [`QualifiedName`] (`None` = `NEXT STATEMENT`).
/// * `region` - Whether the `RESUME` statement appears inside `DECLARATIVES`.
/// * `normal_proc_cnt` - Boundary between normal procedures (`< normal_proc_cnt`) and declarative procedures.
/// * `span` - Statement [`SourceSpan`].
/// * `out` - Output [`HirStmt`] vector.
/// * `ctx` - Active [`UnitLowerCtx`].
pub(crate) fn lower_resume_stmt(
    target: Option<&QualifiedName>,
    region: DeclarativeRegion,
    normal_proc_cnt: u32,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    if region == DeclarativeRegion::NormalBody {
        if ctx.dialect.relaxed_syntax == coblang_diag::RelaxedSyntaxMode::Relaxed {
            ctx.sink.emit(Diagnostic::warning(
                DiagnosticCode::W0006GeneralWarning,
                WarningGroup::DialectExtension,
                Some(span),
                String::from("RESUME outside DECLARATIVES"),
            ));
        } else {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(span),
                String::from("RESUME may only be used inside DECLARATIVES"),
            ));
        }
    }
    let hir_target = match target {
        None => HirResumeTarget::NextStatement,
        Some(qname) => {
            if ctx.interner.resolve_canonical(qname.base).ok() == Some("NEXT") {
                HirResumeTarget::NextStatement
            } else {
                let sec_qual = qname.qualifiers.first().copied();
                if let Some(pid) = ctx.symtab.resolve_procedure(
                    ctx.scope,
                    qname.base,
                    sec_qual,
                    qname.span,
                    ctx.interner,
                    ctx.sink,
                ) {
                    if pid.as_u32() >= normal_proc_cnt {
                        let orig = ctx.interner.resolve_original(qname.base).unwrap_or("?");
                        ctx.sink.emit(Diagnostic::error(
                            DiagnosticCode::E0008TypeMismatch,
                            Some(qname.span),
                            format!("RESUME target '{orig}' must not be in DECLARATIVES"),
                        ));
                    }
                    HirResumeTarget::Procedure(pid)
                } else {
                    HirResumeTarget::NextStatement
                }
            }
        }
    };
    out.push(HirStmt::Resume(HirResumeStmt {
        target: hir_target,
        span,
    }));
}

/// Statement transfer kind referencing a procedure (`PERFORM` vs `GO TO` / `ALTER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcRefTransferKind {
    /// Out-of-line `PERFORM` (permitted to target non-debugging `DECLARATIVES`).
    Perform,
    /// `GO TO` or `ALTER` (forbidden from targeting `DECLARATIVES` from outside).
    GoToOrAlter,
}

/// Validates cross-boundary `PERFORM` / `GO TO` / `ALTER` procedure references against `DECLARATIVES`
/// (`syn_file/0017_declaratives_invalid_procedure_reference_1` and `0018_..._2`).
///
/// # Arguments
///
/// * `region` - Whether the referencing statement is inside `DECLARATIVES`.
/// * `transfer` - Whether the reference is `PERFORM` vs `GO TO`/`ALTER`.
/// * `target_pid` - Referenced [`ProcedureId`].
/// * `target_sym` - Referenced procedure symbol.
/// * `normal_proc_cnt` - Boundary between normal (`< normal_proc_cnt`) and declarative (`>= normal_proc_cnt`) procedures.
/// * `span` - Statement [`SourceSpan`].
/// * `ctx` - Active [`UnitLowerCtx`].
pub(crate) fn validate_declarative_proc_ref(
    region: DeclarativeRegion,
    transfer: ProcRefTransferKind,
    target_pid: ProcedureId,
    target_sym: SymbolId,
    normal_proc_cnt: u32,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let tid = target_pid.as_u32();
    let orig = ctx
        .interner
        .resolve_original(target_sym)
        .unwrap_or("?")
        .to_owned();
    if region == DeclarativeRegion::NormalBody
        && transfer == ProcRefTransferKind::GoToOrAlter
        && tid >= normal_proc_cnt
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            format!("invalid reference to '{orig}' (in DECLARATIVES)"),
        ));
    } else if region == DeclarativeRegion::InDeclaratives && tid < normal_proc_cnt {
        ctx.sink.emit(Diagnostic::warning(
            DiagnosticCode::W0006GeneralWarning,
            WarningGroup::DialectExtension,
            Some(span),
            format!("'{orig}' is not in DECLARATIVES"),
        ));
    }
}

/// Instruments a unit's `entry_statements` and `procedures` with NIST CCVS-85 `DB` `DebugTrace`
/// statements when `WITH DEBUGGING MODE` and `USE FOR DEBUGGING` hooks are present.
///
/// # Arguments
///
/// * `entry_stmts` - Mutable top-level `PROCEDURE DIVISION` entry statements.
/// * `procedures` - Mutable unit [`HirParagraph`] list.
/// * `normal_proc_cnt` - Exclusive upper bound of non-declarative `ProcedureId`s.
/// * `plan` - Resolved [`UnitDebugInstrumentationPlan`].
pub(crate) fn apply_nist_db_instrumentation(
    entry_stmts: &mut Vec<HirStmt>,
    procedures: &mut [HirParagraph],
    normal_proc_cnt: u32,
    plan: &UnitDebugInstrumentationPlan,
) {
    if plan.debugging_mode != HirDebugCompileMode::WithDebuggingMode {
        return;
    }
    let has_any_hook = plan.all_procedures.is_some()
        || !plan.proc_hooks.is_empty()
        || !plan.data_hooks.is_empty()
        || !plan.file_hook_info.is_empty();
    if !has_any_hook {
        return;
    }

    let entry_was_empty = entry_stmts.is_empty();
    *entry_stmts = instrument_stmt_block(std::mem::take(entry_stmts), plan);

    for (idx, para) in procedures.iter_mut().enumerate() {
        let pid_u32 = para.id.as_u32();
        if plan.debug_decl_pids.contains(&pid_u32) {
            continue;
        }
        let mut new_body = Vec::new();
        if pid_u32 >= normal_proc_cnt {
            if let Some((pname, section)) = lookup_proc_debug_hook(para.id, plan)
                && (plan.proc_hooks.contains_key(&para.id)
                    || para.kind == coblang_hir::HirProcKind::SectionHeader)
            {
                let first_span = para.statements.first().map_or(para.span, stmt_primary_span);
                new_body.push(HirStmt::DebugTrace(Box::new(HirDebugTraceStmt {
                    section,
                    line_text: String::new(),
                    name_text: pname,
                    subscripts: Vec::new(),
                    contents: HirDebugContents::StaticText(String::from("USE PROCEDURE")),
                    span: first_span,
                })));
            }
        } else if let Some((pname, section)) = lookup_proc_debug_hook(para.id, plan) {
            let contents = if idx == 0 && entry_was_empty {
                HirDebugContents::StaticText(String::from("START PROGRAM"))
            } else {
                HirDebugContents::StaticText(String::from("FALL THROUGH"))
            };
            let first_span = para.statements.first().map_or(para.span, stmt_primary_span);
            new_body.push(HirStmt::DebugTrace(Box::new(HirDebugTraceStmt {
                section,
                line_text: String::new(),
                name_text: pname,
                subscripts: Vec::new(),
                contents,
                span: first_span,
            })));
        }
        let instrumented = instrument_stmt_block(std::mem::take(&mut para.statements), plan);
        new_body.extend(instrumented);
        para.statements = new_body;
    }
}

/// Resolves the `(procedure_name, HirDeclRef)` debug hook for `pid` (`proc_hooks` or `all_procedures`).
///
/// # Arguments
///
/// * `pid` - Target [`ProcedureId`].
/// * `plan` - Active [`UnitDebugInstrumentationPlan`].
#[must_use]
fn lookup_proc_debug_hook(
    pid: ProcedureId,
    plan: &UnitDebugInstrumentationPlan,
) -> Option<(String, HirDeclRef)> {
    if let Some((nm, sec)) = plan.proc_hooks.get(&pid) {
        return Some((nm.clone(), *sec));
    }
    let sec = plan.all_procedures?;
    let nm = plan.proc_names.get(&pid)?.clone();
    Some((nm, sec))
}

/// Recursively instruments a statement block with `PERFORM LOOP`, `ALTER`, file, and identifier `DebugTrace` hooks.
///
/// # Arguments
///
/// * `stmts` - Input [`HirStmt`] vector.
/// * `plan` - Active [`UnitDebugInstrumentationPlan`].
#[expect(
    clippy::too_many_lines,
    reason = "Covers procedure, file I/O, PERFORM UNTIL, and deduplicated data hooks uniformly."
)]
fn instrument_stmt_block(stmts: Vec<HirStmt>, plan: &UnitDebugInstrumentationPlan) -> Vec<HirStmt> {
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        if let HirStmt::Sentence { body, span } = stmt {
            out.push(HirStmt::Sentence {
                body: instrument_stmt_block(body, plan),
                span,
            });
            continue;
        }

        let sp = stmt_primary_span(&stmt);
        let mut pre_reads = Vec::new();
        let mut post_writes = Vec::new();
        collect_stmt_data_hooks(&stmt, plan, &mut pre_reads, &mut post_writes);

        let mut seen_stmt_items: BTreeSet<DataItemId> = BTreeSet::new();
        let mut dedup_writes = Vec::with_capacity(post_writes.len());
        for (pl, dname, sec) in post_writes {
            if seen_stmt_items.insert(pl.item) {
                dedup_writes.push((pl, dname, sec));
            }
        }
        for (pl, dname, sec) in pre_reads {
            if seen_stmt_items.insert(pl.item) {
                out.push(make_place_debug_trace(pl, dname, sec, sp));
            }
        }

        match stmt {
            HirStmt::PerformRange {
                start_proc,
                end_proc,
                span,
            } => {
                if let Some((pname, sec)) = lookup_proc_debug_hook(start_proc, plan) {
                    out.push(HirStmt::DebugTrace(Box::new(HirDebugTraceStmt {
                        section: sec,
                        line_text: String::new(),
                        name_text: pname,
                        subscripts: Vec::new(),
                        contents: HirDebugContents::StaticText(String::from("PERFORM LOOP")),
                        span,
                    })));
                }
                out.push(HirStmt::PerformRange {
                    start_proc,
                    end_proc,
                    span,
                });
            }
            HirStmt::Alter { pairs, span } => {
                out.push(HirStmt::Alter {
                    pairs: pairs.clone(),
                    span,
                });
                for (paragraph, new_target) in pairs {
                    if let Some((pname, sec)) = lookup_proc_debug_hook(paragraph, plan) {
                        let tgt_name = plan
                            .proc_names
                            .get(&new_target)
                            .cloned()
                            .unwrap_or_default();
                        out.push(HirStmt::DebugTrace(Box::new(HirDebugTraceStmt {
                            section: sec,
                            line_text: String::new(),
                            name_text: pname,
                            subscripts: Vec::new(),
                            contents: HirDebugContents::StaticText(tgt_name),
                            span,
                        })));
                    }
                }
            }
            HirStmt::If {
                cond,
                then_body,
                else_body,
                span,
            } => {
                out.push(HirStmt::If {
                    cond,
                    then_body: instrument_stmt_block(then_body, plan),
                    else_body: instrument_stmt_block(else_body, plan),
                    span,
                });
            }
            HirStmt::Loop {
                kind,
                test_before,
                until,
                body,
                cycle_step,
                span,
            } => {
                let mut cond_hooks = Vec::new();
                if matches!(
                    kind,
                    coblang_hir::HirLoopKind::UserPerform
                        | coblang_hir::HirLoopKind::InternalAfterTier
                ) {
                    let mut seen_cond = BTreeSet::new();
                    collect_cond_perform_hooks(&until, plan, &mut seen_cond, &mut cond_hooks);
                }
                if test_before == coblang_hir::HirLoopTest::Before {
                    for (pl, dname, sec) in &cond_hooks {
                        out.push(make_place_debug_trace(
                            pl.clone(),
                            dname.clone(),
                            *sec,
                            span,
                        ));
                    }
                }
                let mut inst_step = instrument_stmt_block(cycle_step, plan);
                for (pl, dname, sec) in &cond_hooks {
                    inst_step.push(make_place_debug_trace(
                        pl.clone(),
                        dname.clone(),
                        *sec,
                        span,
                    ));
                }
                out.push(HirStmt::Loop {
                    kind,
                    test_before,
                    until,
                    body: instrument_stmt_block(body, plan),
                    cycle_step: inst_step,
                    span,
                });
            }
            HirStmt::FileOpen(spec) => {
                let span = spec.span;
                let files: Vec<FileConnectorId> = spec.entries.iter().map(|e| e.file).collect();
                out.push(HirStmt::FileOpen(spec));
                for fid in files {
                    if let Some((fname, _, sec)) = plan.file_hook_info.get(&fid) {
                        out.push(make_file_debug_trace(fname.clone(), *sec, None, span));
                    }
                }
            }
            HirStmt::FileClose(spec) => {
                let span = spec.span;
                let files: Vec<FileConnectorId> = spec.entries.iter().map(|e| e.file).collect();
                out.push(HirStmt::FileClose(spec));
                for fid in files {
                    if let Some((fname, _, sec)) = plan.file_hook_info.get(&fid) {
                        out.push(make_file_debug_trace(fname.clone(), *sec, None, span));
                    }
                }
            }
            HirStmt::FileRead(mut spec) => {
                spec.on_condition = instrument_stmt_block(spec.on_condition, plan);
                spec.not_on_condition = instrument_stmt_block(spec.not_on_condition, plan);
                spec.into_moves = instrument_stmt_block(spec.into_moves, plan);
                if let Some((fname, rec_pl, sec)) = plan.file_hook_info.get(&spec.file) {
                    spec.into_moves.push(make_file_debug_trace(
                        fname.clone(),
                        *sec,
                        rec_pl.clone(),
                        spec.span,
                    ));
                }
                out.push(HirStmt::FileRead(spec));
            }
            HirStmt::FileWrite(mut spec) => {
                let had_from = !spec.pre_from_moves.is_empty();
                spec.pre_from_moves = instrument_stmt_block(spec.pre_from_moves, plan);
                spec.on_event = instrument_stmt_block(spec.on_event, plan);
                spec.not_on_event = instrument_stmt_block(spec.not_on_event, plan);
                if !had_from
                    && let Some((nm, _, sec)) = plan.data_hooks.get(&spec.record_place.item)
                {
                    out.push(make_place_debug_trace(
                        spec.record_place.clone(),
                        nm.clone(),
                        *sec,
                        spec.span,
                    ));
                }
                out.push(HirStmt::FileWrite(spec));
            }
            HirStmt::FileRewrite(mut spec) => {
                let had_from = !spec.pre_from_moves.is_empty();
                spec.pre_from_moves = instrument_stmt_block(spec.pre_from_moves, plan);
                spec.on_invalid_key = instrument_stmt_block(spec.on_invalid_key, plan);
                spec.not_on_invalid_key = instrument_stmt_block(spec.not_on_invalid_key, plan);
                if !had_from
                    && let Some((nm, _, sec)) = plan.data_hooks.get(&spec.record_place.item)
                {
                    out.push(make_place_debug_trace(
                        spec.record_place.clone(),
                        nm.clone(),
                        *sec,
                        spec.span,
                    ));
                }
                out.push(HirStmt::FileRewrite(spec));
            }
            HirStmt::FileDelete(mut spec) => {
                spec.on_invalid_key = instrument_stmt_block(spec.on_invalid_key, plan);
                spec.not_on_invalid_key = instrument_stmt_block(spec.not_on_invalid_key, plan);
                if let Some((fname, _, sec)) = plan.file_hook_info.get(&spec.file) {
                    let tr = make_file_debug_trace(fname.clone(), *sec, None, spec.span);
                    if spec.inline_presence == coblang_hir::HirInlineClausePresence::Present
                        || !spec.not_on_invalid_key.is_empty()
                    {
                        spec.on_invalid_key.insert(0, tr.clone());
                        spec.not_on_invalid_key.insert(0, tr);
                        out.push(HirStmt::FileDelete(spec));
                    } else {
                        out.push(HirStmt::FileDelete(spec));
                        out.push(tr);
                    }
                } else {
                    out.push(HirStmt::FileDelete(spec));
                }
            }
            HirStmt::FileStart(mut spec) => {
                spec.on_invalid_key = instrument_stmt_block(spec.on_invalid_key, plan);
                spec.not_on_invalid_key = instrument_stmt_block(spec.not_on_invalid_key, plan);
                if let Some((fname, _, sec)) = plan.file_hook_info.get(&spec.file) {
                    let tr = make_file_debug_trace(fname.clone(), *sec, None, spec.span);
                    if spec.inline_presence == coblang_hir::HirInlineClausePresence::Present
                        || !spec.not_on_invalid_key.is_empty()
                    {
                        spec.on_invalid_key.insert(0, tr.clone());
                        spec.not_on_invalid_key.insert(0, tr);
                        out.push(HirStmt::FileStart(spec));
                    } else {
                        out.push(HirStmt::FileStart(spec));
                        out.push(tr);
                    }
                } else {
                    out.push(HirStmt::FileStart(spec));
                }
            }
            other => {
                out.push(other);
            }
        }

        for (pl, dname, sec) in dedup_writes {
            out.push(make_place_debug_trace(pl, dname, sec, sp));
        }
    }
    out
}

/// Constructs a [`HirStmt::DebugTrace`] for a monitored `FD` file connector (`OPEN`, `CLOSE`, `READ`, `DELETE`, `START`).
///
/// # Arguments
///
/// * `fname` - Canonical `DEBUG-NAME` of the file connector.
/// * `sec` - Declarative [`HirDeclRef`].
/// * `rec_place` - `Some(01_record)` on successful `READ`, `None` (`SPACES`) on `OPEN`/`CLOSE`/`DELETE`/`START`.
/// * `span` - Statement [`SourceSpan`].
#[must_use]
fn make_file_debug_trace(
    fname: String,
    sec: HirDeclRef,
    rec_place: Option<HirPlace>,
    span: SourceSpan,
) -> HirStmt {
    HirStmt::DebugTrace(Box::new(HirDebugTraceStmt {
        section: sec,
        line_text: String::new(),
        name_text: fname,
        subscripts: Vec::new(),
        contents: match rec_place {
            Some(pl) => HirDebugContents::PlaceValue(pl),
            None => HirDebugContents::Spaces,
        },
        span,
    }))
}

/// Collects all monitored [`HirPlace`]s (both `WriteOnly` and `AllReferences`, per ANSI XI-1.3.2(2)(b))
/// referenced inside a `PERFORM ... UNTIL` condition.
///
/// # Arguments
///
/// * `cond` - `UNTIL` [`coblang_hir::HirCondition`].
/// * `plan` - Active [`UnitDebugInstrumentationPlan`].
/// * `seen` - Deduplication set per `UNTIL` condition.
/// * `out` - Output `(HirPlace, name, section)` vector.
fn collect_cond_perform_hooks(
    cond: &coblang_hir::HirCondition,
    plan: &UnitDebugInstrumentationPlan,
    seen: &mut BTreeSet<DataItemId>,
    out: &mut Vec<(HirPlace, String, HirDeclRef)>,
) {
    match cond {
        coblang_hir::HirCondition::Compare { lhs, rhs, .. } => {
            collect_expr_perform_hooks(lhs, plan, seen, out);
            collect_expr_perform_hooks(rhs, plan, seen, out);
        }
        coblang_hir::HirCondition::Not(inner) => collect_cond_perform_hooks(inner, plan, seen, out),
        coblang_hir::HirCondition::And(a, b) | coblang_hir::HirCondition::Or(a, b) => {
            collect_cond_perform_hooks(a, plan, seen, out);
            collect_cond_perform_hooks(b, plan, seen, out);
        }
        _ => {}
    }
}

/// Helper scanning `expr` for any monitored `DataItemId` (`WriteOnly` or `AllReferences`) inside `PERFORM ... UNTIL`.
///
/// # Arguments
///
/// * `expr` - Candidate [`HirExpr`].
/// * `plan` - Active [`UnitDebugInstrumentationPlan`].
/// * `seen` - Deduplication set.
/// * `out` - Output `(HirPlace, name, section)` vector.
fn collect_expr_perform_hooks(
    expr: &HirExpr,
    plan: &UnitDebugInstrumentationPlan,
    seen: &mut BTreeSet<DataItemId>,
    out: &mut Vec<(HirPlace, String, HirDeclRef)>,
) {
    match expr {
        HirExpr::Place(pl) => {
            for (se, _) in &pl.subscripts {
                collect_expr_perform_hooks(se, plan, seen, out);
            }
            if let Some((nm, _, sec)) = plan.data_hooks.get(&pl.item)
                && seen.insert(pl.item)
            {
                out.push((pl.clone(), nm.clone(), *sec));
            }
        }
        HirExpr::Neg(inner) => collect_expr_perform_hooks(inner, plan, seen, out),
        HirExpr::Binary { lhs, rhs, .. } => {
            collect_expr_perform_hooks(lhs, plan, seen, out);
            collect_expr_perform_hooks(rhs, plan, seen, out);
        }
        _ => {}
    }
}

/// Constructs a [`HirStmt::DebugTrace`] for a monitored [`HirPlace`].
///
/// # Arguments
///
/// * `pl` - Accessed [`HirPlace`].
/// * `dname` - Monitored `DEBUG-NAME`.
/// * `sec` - Declarative [`HirDeclRef`].
/// * `span` - Enclosing statement [`SourceSpan`].
#[must_use]
fn make_place_debug_trace(
    pl: HirPlace,
    dname: String,
    sec: HirDeclRef,
    span: SourceSpan,
) -> HirStmt {
    let subs: Vec<HirExpr> = pl
        .subscripts
        .iter()
        .take(3)
        .map(|(e, _)| e.clone())
        .collect();
    HirStmt::DebugTrace(Box::new(HirDebugTraceStmt {
        section: sec,
        line_text: String::new(),
        name_text: dname,
        subscripts: subs,
        contents: HirDebugContents::PlaceValue(pl),
        span,
    }))
}

/// Mode of [`HirPlace`] access (`ReadAccess` vs `WriteAccess`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlaceAccessKind {
    /// Source read access.
    ReadAccess,
    /// Target write/modification access.
    WriteAccess,
}

/// Collects monitored read and write [`HirPlace`] accesses on a single top-level [`HirStmt`].
///
/// # Arguments
///
/// * `stmt` - Inspected [`HirStmt`].
/// * `plan` - Active [`UnitDebugInstrumentationPlan`].
/// * `reads` - Output `(HirPlace, name, section)` vector for `AllReferences` reads.
/// * `writes` - Output `(HirPlace, name, section)` vector for `WriteOnly` / `AllReferences` writes.
#[expect(
    clippy::too_many_lines,
    reason = "Exhaustive read/write place collector across all HIR statement variants."
)]
fn collect_stmt_data_hooks(
    stmt: &HirStmt,
    plan: &UnitDebugInstrumentationPlan,
    reads: &mut Vec<(HirPlace, String, HirDeclRef)>,
    writes: &mut Vec<(HirPlace, String, HirDeclRef)>,
) {
    if plan.data_hooks.is_empty() {
        return;
    }
    match stmt {
        HirStmt::Move { src, dst, .. } => {
            collect_expr_reads(src, plan, reads);
            check_place_hook(dst, PlaceAccessKind::WriteAccess, plan, reads, writes);
        }
        HirStmt::AssignArith { expr, targets, .. } => {
            collect_expr_reads(expr, plan, reads);
            for (dst, _) in targets {
                check_place_hook(dst, PlaceAccessKind::WriteAccess, plan, reads, writes);
            }
        }
        HirStmt::DivideRemainder(spec) => {
            collect_expr_reads(&spec.dividend, plan, reads);
            collect_expr_reads(&spec.divisor, plan, reads);
            check_place_hook(
                &spec.quotient.0,
                PlaceAccessKind::WriteAccess,
                plan,
                reads,
                writes,
            );
            check_place_hook(
                &spec.remainder,
                PlaceAccessKind::WriteAccess,
                plan,
                reads,
                writes,
            );
        }
        HirStmt::Display { operands, .. } => {
            for op in operands {
                collect_expr_reads(op, plan, reads);
            }
        }
        HirStmt::Accept(spec) => {
            if let coblang_hir::HirAcceptTarget::Place(pl) = &spec.target {
                check_place_hook(pl, PlaceAccessKind::WriteAccess, plan, reads, writes);
            }
        }
        HirStmt::If { cond, .. } => {
            collect_cond_reads(cond, plan, reads);
        }
        HirStmt::Jump {
            depending_on: Some(dep_expr),
            ..
        } => {
            collect_expr_reads(dep_expr, plan, reads);
        }
        HirStmt::Initialize { targets, .. } => {
            for dst in targets {
                check_place_hook(dst, PlaceAccessKind::WriteAccess, plan, reads, writes);
            }
        }
        HirStmt::Inspect(spec) => match &spec.action {
            coblang_hir::HirInspectAction::TallyAndReplace {
                counters, replaces, ..
            } => {
                check_place_hook(
                    &spec.target,
                    if replaces.is_empty() {
                        PlaceAccessKind::ReadAccess
                    } else {
                        PlaceAccessKind::WriteAccess
                    },
                    plan,
                    reads,
                    writes,
                );
                for c in counters {
                    check_place_hook(c, PlaceAccessKind::WriteAccess, plan, reads, writes);
                }
            }
            coblang_hir::HirInspectAction::Converting(_) => {
                check_place_hook(
                    &spec.target,
                    PlaceAccessKind::WriteAccess,
                    plan,
                    reads,
                    writes,
                );
            }
        },
        HirStmt::StringVerb(spec) => {
            for (src, _) in &spec.pieces {
                collect_expr_reads(src, plan, reads);
            }
            check_place_hook(
                &spec.into,
                PlaceAccessKind::WriteAccess,
                plan,
                reads,
                writes,
            );
            if let Some(ptr_pl) = &spec.pointer {
                check_place_hook(ptr_pl, PlaceAccessKind::WriteAccess, plan, reads, writes);
            }
        }
        HirStmt::Unstring(spec) => {
            check_place_hook(
                &spec.source,
                PlaceAccessKind::ReadAccess,
                plan,
                reads,
                writes,
            );
            for dst in &spec.targets {
                check_place_hook(&dst.into, PlaceAccessKind::WriteAccess, plan, reads, writes);
                if let Some(dl) = &dst.delimiter_in {
                    check_place_hook(dl, PlaceAccessKind::WriteAccess, plan, reads, writes);
                }
                if let Some(cnt) = &dst.count_in {
                    check_place_hook(cnt, PlaceAccessKind::WriteAccess, plan, reads, writes);
                }
            }
            if let Some(ptr_pl) = &spec.pointer {
                check_place_hook(ptr_pl, PlaceAccessKind::WriteAccess, plan, reads, writes);
            }
            if let Some(tl_pl) = &spec.tallying {
                check_place_hook(tl_pl, PlaceAccessKind::WriteAccess, plan, reads, writes);
            }
        }
        HirStmt::ReleaseRecord(spec) => {
            for mv in &spec.pre_from_moves {
                collect_stmt_data_hooks(mv, plan, reads, writes);
            }
            check_place_hook(
                &spec.record_place,
                PlaceAccessKind::WriteAccess,
                plan,
                reads,
                writes,
            );
        }
        HirStmt::ReturnRecord(spec) => {
            for mv in &spec.into_moves {
                collect_stmt_data_hooks(mv, plan, reads, writes);
            }
        }
        _ => {}
    }
}

/// Checks if `pl` matches a monitored `DataItemId` in `plan.data_hooks`.
///
/// # Arguments
///
/// * `pl` - Accessed [`HirPlace`].
/// * `access` - `ReadAccess` vs `WriteAccess`.
/// * `plan` - Active [`UnitDebugInstrumentationPlan`].
/// * `reads` - Read hook sink.
/// * `writes` - Write hook sink.
fn check_place_hook(
    pl: &HirPlace,
    access: PlaceAccessKind,
    plan: &UnitDebugInstrumentationPlan,
    reads: &mut Vec<(HirPlace, String, HirDeclRef)>,
    writes: &mut Vec<(HirPlace, String, HirDeclRef)>,
) {
    for (sub_e, _) in &pl.subscripts {
        collect_expr_reads(sub_e, plan, reads);
    }
    if let Some((nm, kind, sec)) = plan.data_hooks.get(&pl.item) {
        if access == PlaceAccessKind::WriteAccess {
            writes.push((pl.clone(), nm.clone(), *sec));
        } else if *kind == MonitoredRefKind::AllReferences {
            reads.push((pl.clone(), nm.clone(), *sec));
        }
    }
}

/// Scans an expression for `AllReferences` read hooks.
///
/// # Arguments
///
/// * `expr` - Inspected [`HirExpr`].
/// * `plan` - Active [`UnitDebugInstrumentationPlan`].
/// * `reads` - Read hook sink.
fn collect_expr_reads(
    expr: &HirExpr,
    plan: &UnitDebugInstrumentationPlan,
    reads: &mut Vec<(HirPlace, String, HirDeclRef)>,
) {
    let mut dummy_writes = Vec::new();
    match expr {
        HirExpr::Place(pl) => {
            check_place_hook(
                pl,
                PlaceAccessKind::ReadAccess,
                plan,
                reads,
                &mut dummy_writes,
            );
        }
        HirExpr::Neg(inner) => collect_expr_reads(inner, plan, reads),
        HirExpr::Binary { lhs, rhs, .. } => {
            collect_expr_reads(lhs, plan, reads);
            collect_expr_reads(rhs, plan, reads);
        }
        HirExpr::M12Call(call) => {
            for a in &call.args {
                if let coblang_hir::HirM12Arg::Expr(e) = a {
                    collect_expr_reads(e, plan, reads);
                } else if let coblang_hir::HirM12Arg::RawStoragePlace(pl) = a {
                    check_place_hook(
                        pl,
                        PlaceAccessKind::ReadAccess,
                        plan,
                        reads,
                        &mut dummy_writes,
                    );
                }
            }
        }
        _ => {}
    }
}

/// Scans a condition for `AllReferences` read hooks.
///
/// # Arguments
///
/// * `cond` - Inspected [`coblang_hir::HirCondition`].
/// * `plan` - Active [`UnitDebugInstrumentationPlan`].
/// * `reads` - Read hook sink.
fn collect_cond_reads(
    cond: &coblang_hir::HirCondition,
    plan: &UnitDebugInstrumentationPlan,
    reads: &mut Vec<(HirPlace, String, HirDeclRef)>,
) {
    match cond {
        coblang_hir::HirCondition::Compare { lhs, rhs, .. } => {
            collect_expr_reads(lhs, plan, reads);
            collect_expr_reads(rhs, plan, reads);
        }
        coblang_hir::HirCondition::Not(inner) => collect_cond_reads(inner, plan, reads),
        coblang_hir::HirCondition::And(a, b) | coblang_hir::HirCondition::Or(a, b) => {
            collect_cond_reads(a, plan, reads);
            collect_cond_reads(b, plan, reads);
        }
        _ => {}
    }
}

/// Extracts the primary [`SourceSpan`] of a [`HirStmt`] for `DEBUG-LINE` attribution.
///
/// # Arguments
///
/// * `stmt` - Target [`HirStmt`].
#[must_use]
fn stmt_primary_span(stmt: &HirStmt) -> SourceSpan {
    match stmt {
        HirStmt::Move { span, .. }
        | HirStmt::Initialize { span, .. }
        | HirStmt::AssignArith { span, .. }
        | HirStmt::Display { span, .. }
        | HirStmt::DisplayEnv { span, .. }
        | HirStmt::If { span, .. }
        | HirStmt::Sentence { span, .. }
        | HirStmt::PerformRange { span, .. }
        | HirStmt::Loop { span, .. }
        | HirStmt::Jump { span, .. }
        | HirStmt::Alter { span, .. }
        | HirStmt::FlowExit { span, .. }
        | HirStmt::SetSwitch { span, .. }
        | HirStmt::Cancel { span, .. }
        | HirStmt::Return { span, .. }
        | HirStmt::Free { span, .. }
        | HirStmt::SetTurn(HirSetTurnStmt { span, .. })
        | HirStmt::SetLastExceptionOff { span }
        | HirStmt::Raise(HirRaiseStmt { span, .. })
        | HirStmt::Resume(HirResumeStmt { span, .. })
        | HirStmt::CheckException(coblang_hir::HirCheckExceptionStmt { span, .. })
        | HirStmt::HighLevelOp { span, .. } => *span,
        HirStmt::DivideRemainder(s) => s.span,
        HirStmt::SortTable(s) => s.span,
        HirStmt::Call(s) => s.span,
        HirStmt::Accept(s) => s.span,
        HirStmt::StringVerb(s) => s.span,
        HirStmt::Unstring(s) => s.span,
        HirStmt::Inspect(s) => s.span,
        HirStmt::SetPointer(s) => s.span,
        HirStmt::Allocate(s) => s.span,
        HirStmt::FileOpen(s) => s.span,
        HirStmt::FileClose(s) => s.span,
        HirStmt::FileRead(s) => s.span,
        HirStmt::FileWrite(s) => s.span,
        HirStmt::FileRewrite(s) => s.span,
        HirStmt::FileDelete(s) => s.span,
        HirStmt::FileDeletePhysical(s) => s.span,
        HirStmt::FileStart(s) => s.span,
        HirStmt::FileUnlock(s) => s.span,
        HirStmt::SortFile(s) => s.span,
        HirStmt::MergeFile(s) => s.span,
        HirStmt::ReleaseRecord(s) => s.span,
        HirStmt::ReturnRecord(s) => s.span,
        HirStmt::DebugTrace(s) => s.span,
        HirStmt::M18(s) => s.span(),
        HirStmt::M19(s) => s.span(),
        HirStmt::M20(s) => s.span(),
    }
}
