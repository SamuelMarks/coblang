//! Semantic validation, storage synthesis, and HIR lowering for Milestone M18
//! (`REPORT SECTION` `RD`, `INITIATE`, `GENERATE`, `TERMINATE`, `SUPPRESS`, and `USE BEFORE REPORTING`).

#![deny(clippy::indexing_slicing)]

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{
    ByteOffset, DataItemId, Dialect, DigitCount, FileConnectorId, LevelNumber, ProcedureId,
    ReportId, Result, ScaleFactor, ScopeId, SourceSpan, SymbolId,
};
use coblang_diag::{
    Diagnostic, DiagnosticCode, DiagnosticSink, DialectConfig, InternalCharsetMode, SymbolInterner,
    WarningGroup, WarningPolicy,
};
use coblang_hir::{
    BinaryEndianness, DataCategory, HirCondition, HirDataItem, HirExpr, HirGroupIndicateMode,
    HirM18Stmt, HirPlace, HirReportCodeSpec, HirReportColAlign, HirReportColSpec,
    HirReportControlSpec, HirReportElemPlan, HirReportFormatterKind, HirReportGroupPlan,
    HirReportGroupRole, HirReportHookPlan, HirReportLineSpec, HirReportNextGroupSpec,
    HirReportNodePlan, HirReportOccursSpec, HirReportPageLimits, HirReportPlan,
    HirReportPresentKind, HirReportSourceKind, HirReportSubGroupPlan, HirReportSumSpec, HirStmt,
    HirStorageSection, HirType, JustifiedSpec, Signedness, TruncationPolicy,
};
use coblang_syntax::ast::{
    BlankWhenZeroMode, DataClauseCounts, DataDivision, DataEntry, DataEntryName, Expr,
    FigurativeValue, GlobalVisibility, JustificationMode, OccursClause, OccursUpperBound,
    ProcedureDivision, RdCodeClauseAst, RdColLimitAst, RdEntry, ReportAfterNewAtomAst,
    ReportColAlignAst, ReportColCoordAst, ReportControlAst, ReportGroupEntry, ReportGroupType,
    ReportLineClauseAst, ReportNextGroupAst, ReportPresentGateAst, ReportPresentPolarity,
    StorageLinkageFlags, SyncAlignment, UsageKind, UseClause, ValueAtom,
};

use crate::layout::{SectionLayoutCtx, SolvedDataLayout, solve_entry_slice_with_ids};
use crate::lower_hir::{
    UnitLowerCtx, lower_condition, lower_expr_with_cat, resolve_place, simple_place_for_item,
};
use crate::m13_exceptions::DeclarativeRegion;
use crate::picture::{ElementaryClauses, PictureContext, compile_elementary_type};
use crate::symbols::{ResolvedSymbol, SymbolCandidate, SymbolTable};

/// Pre-indexed report & group lookup table for a compilation unit.
#[derive(Debug, Clone, Default)]
pub struct UnitReportCatalog {
    /// Maps `RD` symbol -> `(ReportId, original_name, has_detail_group)`.
    pub rd_by_sym: BTreeMap<SymbolId, (ReportId, String, bool)>,
    /// Maps report group symbol -> `(ReportId, original_rd_name, uppercase_group_name, ReportGroupType)`.
    pub group_by_sym: BTreeMap<SymbolId, (ReportId, String, String, ReportGroupType)>,
    /// Inherited `IS GLOBAL` `RD` entries from enclosing programs.
    pub inherited_rds: BTreeMap<SymbolId, (ReportId, String, bool)>,
    /// Inherited `IS GLOBAL` report groups from enclosing programs.
    pub inherited_groups: BTreeMap<SymbolId, (ReportId, String, String, ReportGroupType)>,
    /// Validated `USE [GLOBAL] BEFORE REPORTING` hooks per `ReportId`.
    pub hooks_by_report: BTreeMap<ReportId, Vec<(HirReportHookPlan, GlobalVisibility)>>,
    /// Undefined report symbols already diagnosed once (`syn_reportwriter/0001`).
    pub seen_undef_reports: std::cell::RefCell<BTreeSet<SymbolId>>,
}

impl UnitReportCatalog {
    /// Looks up an `RD` by symbol (local first, then inherited `GLOBAL`).
    ///
    /// # Arguments
    ///
    /// * `sym` - `RD` identifier symbol.
    #[must_use]
    pub fn lookup_rd(&self, sym: SymbolId) -> Option<&(ReportId, String, bool)> {
        self.rd_by_sym
            .get(&sym)
            .or_else(|| self.inherited_rds.get(&sym))
    }

    /// Looks up a typed report group by symbol (local first, then inherited `GLOBAL`).
    ///
    /// # Arguments
    ///
    /// * `sym` - Report group identifier symbol.
    #[must_use]
    pub fn lookup_group(
        &self,
        sym: SymbolId,
    ) -> Option<&(ReportId, String, String, ReportGroupType)> {
        self.group_by_sym
            .get(&sym)
            .or_else(|| self.inherited_groups.get(&sym))
    }
}

/// Synthesizes per-`RD` `PAGE-COUNTER`, `LINE-COUNTER`, `PRINT-SWITCH`, `VARYING` indices,
/// and `01`..`49` `REPORT SECTION` backing storage in `SolvedDataLayout`.
///
/// # Arguments
///
/// * `report_section` - Parsed `RD` entries.
/// * `scope` - Unit lexical [`ScopeId`].
/// * `pic_ctx` - Active [`PictureContext`].
/// * `dialect` - Active [`DialectConfig`].
/// * `out` - Mutable [`SolvedDataLayout`].
/// * `symtab` - Mutable [`SymbolTable`].
/// * `interner` - Shared [`SymbolInterner`].
/// * `sink` - Diagnostic sink.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if symbol interning fails.
#[expect(
    clippy::too_many_arguments,
    reason = "Direct layout-phase extension sharing SolvedDataLayout and compiler tables."
)]
pub fn solve_report_section_storage(
    report_section: &[RdEntry],
    scope: ScopeId,
    pic_ctx: PictureContext,
    dialect: &DialectConfig,
    out: &mut SolvedDataLayout,
    symtab: &mut SymbolTable,
    interner: &mut SymbolInterner,
    sink: &mut DiagnosticSink,
) -> Result<()> {
    if report_section.is_empty() {
        return Ok(());
    }

    let pc_sym = interner.intern("PAGE-COUNTER")?;
    let lc_sym = interner.intern("LINE-COUNTER")?;
    let ps_sym = interner.intern("PRINT-SWITCH")?;
    if let Some(sc) = symtab.scope_mut(scope) {
        let _ = sc.by_name.remove(&pc_sym);
        let _ = sc.by_name.remove(&lc_sym);
        let _ = sc.by_name.remove(&ps_sym);
    }

    let s9_9 = HirType::BinaryInt {
        byte_width: 4,
        digits: DigitCount::try_new(9).unwrap_or(DigitCount::ONE),
        scale: ScaleFactor::ZERO,
        sign: Signedness::Signed,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::FullBinaryRange,
    };

    for (rd_idx, rd) in report_section.iter().enumerate() {
        let pc_id = alloc_rd_special_reg(
            scope,
            pc_sym,
            rd.report_name,
            rd.global,
            s9_9.clone(),
            rd.span,
            out,
            symtab,
        );
        let lc_id = alloc_rd_special_reg(
            scope,
            lc_sym,
            rd.report_name,
            rd.global,
            s9_9.clone(),
            rd.span,
            out,
            symtab,
        );
        let ps_id = alloc_rd_special_reg(
            scope,
            ps_sym,
            rd.report_name,
            rd.global,
            s9_9.clone(),
            rd.span,
            out,
            symtab,
        );
        out.rd_special_regs.push((pc_id, lc_id, ps_id));

        for g in &rd.groups {
            if let Some(occ) = &g.occurs
                && let Some((v_sym, _, _, v_sp)) = occ.varying
                && symtab.resolve_symbol_silent(scope, v_sym, &[]).is_none()
            {
                let v_id = DataItemId::new(u32::try_from(out.items.len()).unwrap_or(0));
                out.items.push(HirDataItem {
                    id: v_id,
                    scope,
                    name: Some(v_sym),
                    level: LevelNumber::Independent77,
                    parent: None,
                    children: Vec::new(),
                    section: HirStorageSection::WorkingStorage,
                    ty: s9_9.clone(),
                    byte_offset: ByteOffset::ZERO,
                    total_byte_size: 4,
                    dimensions: Vec::new(),
                    redefines: None,
                    initial_value: None,
                    span: v_sp,
                });
                symtab.insert_symbol(
                    scope,
                    v_sym,
                    SymbolCandidate {
                        target: ResolvedSymbol::DataItem(v_id),
                        qualifiers: Vec::new(),
                        visibility: rd.global,
                    },
                );
            }
        }

        let synth_entries = synthesize_rd_data_entries(&rd.groups, pic_ctx, dialect, interner)?;
        let mut dummy_sink = DiagnosticSink::new(WarningPolicy::default(), 0);
        let mut ctx = SectionLayoutCtx {
            scope,
            section: HirStorageSection::WorkingStorage,
            file_qual: Some(rd.report_name),
            inherited_global: rd.global,
            pic_ctx,
            dialect,
            symtab,
            interner,
            sink: &mut dummy_sink,
        };
        let ids = solve_entry_slice_with_ids(&synth_entries, out, &mut ctx);
        for (g_idx, opt_id) in ids.into_iter().enumerate() {
            if let Some(id) = opt_id {
                out.report_entry_items.insert((rd_idx, g_idx), id);
            }
        }
        let _ = &sink;
    }

    Ok(())
}

/// Allocates a single 4-byte binary special-register item qualified by `rd_sym`.
///
/// # Arguments
///
/// * `scope` - Unit lexical [`ScopeId`].
/// * `reg_sym` - Special-register [`SymbolId`] (`PAGE-COUNTER`, `LINE-COUNTER`, or `PRINT-SWITCH`).
/// * `rd_sym` - Qualifying `RD` report name [`SymbolId`].
/// * `vis` - Global visibility (`IS GLOBAL` or local) inherited from the `RD`.
/// * `ty` - Synthesized 4-byte binary [`HirType`].
/// * `span` - Source span of the enclosing `RD`.
/// * `out` - Mutable [`SolvedDataLayout`] receiving the allocated item.
/// * `symtab` - Mutable [`SymbolTable`] receiving the `RD`-qualified symbol binding.
#[expect(
    clippy::too_many_arguments,
    reason = "Internal special-register constructor."
)]
fn alloc_rd_special_reg(
    scope: ScopeId,
    reg_sym: SymbolId,
    rd_sym: SymbolId,
    vis: GlobalVisibility,
    ty: HirType,
    span: SourceSpan,
    out: &mut SolvedDataLayout,
    symtab: &mut SymbolTable,
) -> DataItemId {
    let id = DataItemId::new(u32::try_from(out.items.len()).unwrap_or(0));
    out.items.push(HirDataItem {
        id,
        scope,
        name: Some(reg_sym),
        level: LevelNumber::Independent77,
        parent: None,
        children: Vec::new(),
        section: HirStorageSection::WorkingStorage,
        ty,
        byte_offset: ByteOffset::ZERO,
        total_byte_size: 4,
        dimensions: Vec::new(),
        redefines: None,
        initial_value: None,
        span,
    });
    symtab.insert_symbol(
        scope,
        reg_sym,
        SymbolCandidate {
            target: ResolvedSymbol::DataItem(id),
            qualifiers: vec![rd_sym],
            visibility: vis,
        },
    );
    id
}

/// Converts `ReportGroupEntry` items into synthetic `DataEntry` declarations for layout calculation.
///
/// # Arguments
///
/// * `groups` - Report group entries under one `RD`.
/// * `pic_ctx` - Active [`PictureContext`].
/// * `dialect` - Active [`DialectConfig`].
/// * `interner` - Shared [`SymbolInterner`].
fn synthesize_rd_data_entries(
    groups: &[ReportGroupEntry],
    pic_ctx: PictureContext,
    dialect: &DialectConfig,
    interner: &mut SymbolInterner,
) -> Result<Vec<DataEntry>> {
    let mut out = Vec::with_capacity(groups.len());
    for (idx, g) in groups.iter().enumerate() {
        let lvl_num = match g.level {
            LevelNumber::Hierarchy(n) => n,
            _ => 1,
        };
        let has_child = groups.get(idx + 1).is_some_and(|nxt| match nxt.level {
            LevelNumber::Hierarchy(nl) => nl > lvl_num,
            _ => false,
        });
        let _ = (pic_ctx, dialect);
        let picture = if let Some(pic) = g.picture {
            Some(pic)
        } else if !has_child && g.sums.is_empty() {
            let fallback_len = estimate_value_len(&g.values, interner).max(1);
            let pic_sym = interner.intern(&format!("X({fallback_len})"))?;
            Some((pic_sym, g.span))
        } else {
            None
        };
        let occurs = if let Some(occ_ext) = &g.occurs {
            Some(occ_ext.base.clone())
        } else if g.columns.len() > 1 {
            let cnt = u32::try_from(g.columns.len()).unwrap_or(1);
            Some(OccursClause {
                min: cnt,
                max: OccursUpperBound::Bounded(cnt),
                depending_on: None,
                keys: Vec::new(),
                indexed_by: Vec::new(),
                span: g.span,
            })
        } else {
            None
        };
        out.push(DataEntry {
            level: g.level,
            name: g.name,
            redefines: None,
            renames: None,
            picture,
            usage: g.usage.clone(),
            sign: g.sign,
            justified: g.justified,
            blank_when_zero: g.blank_when_zero,
            sync: SyncAlignment::Unsynchronized,
            occurs,
            value: None,
            flags: StorageLinkageFlags::default(),
            clause_counts: DataClauseCounts::default(),
            span: g.span,
        });
    }
    Ok(out)
}

/// Estimates byte length from `VALUE` literals when `PIC` is omitted on a report literal item.
///
/// # Arguments
///
/// * `values` - Sequence of `(ValueAtom, SourceSpan)` literals declared on the report entry.
/// * `interner` - Shared [`SymbolInterner`] used to resolve numeric and identifier literal text.
fn estimate_value_len(values: &[(ValueAtom, SourceSpan)], interner: &SymbolInterner) -> u32 {
    let mut best = 1_u32;
    for (atom, _) in values {
        let n = match atom {
            ValueAtom::String { bytes, .. } => u32::try_from(bytes.len()).unwrap_or(1),
            ValueAtom::Integer(sym) | ValueAtom::Float(sym) => {
                u32::try_from(interner.resolve_original(*sym).map_or(1, str::len)).unwrap_or(1)
            }
            ValueAtom::Decimal { digits, .. } => {
                let a = interner.resolve_original(*digits).map_or(1, str::len);
                u32::try_from(a.saturating_add(1)).unwrap_or(1)
            }
            _ => 1,
        };
        best = best.max(n);
    }
    best
}

/// Builds [`UnitReportCatalog`] and validates `FD ... REPORT IS`, `RD`, and `USE BEFORE REPORTING` bindings.
///
/// # Arguments
///
/// * `data` - Unit `DATA DIVISION`.
/// * `proc_div` - Unit `PROCEDURE DIVISION`.
/// * `normal_proc_cnt` - Count of non-declarative procedures (base `ProcedureId` for declaratives).
/// * `inherited` - Optional parent [`UnitReportCatalog`] for `IS GLOBAL` inheritance.
/// * `interner` - Shared [`SymbolInterner`].
/// * `sink` - Diagnostic sink.
pub fn build_unit_report_catalog(
    data: &DataDivision,
    proc_div: &ProcedureDivision,
    normal_proc_cnt: u32,
    inherited: Option<&UnitReportCatalog>,
    interner: &SymbolInterner,
    sink: &mut DiagnosticSink,
) -> UnitReportCatalog {
    let mut cat = UnitReportCatalog::default();
    if let Some(parent) = inherited {
        cat.inherited_rds = parent.inherited_rds.clone();
        cat.inherited_groups = parent.inherited_groups.clone();
        for (&rid, hooks) in &parent.hooks_by_report {
            for (h, vis) in hooks {
                if *vis == GlobalVisibility::Global {
                    cat.hooks_by_report
                        .entry(rid)
                        .or_default()
                        .push((h.clone(), *vis));
                }
            }
        }
    }

    let mut fd_by_report: BTreeMap<SymbolId, SymbolId> = BTreeMap::new();
    let rd_set: BTreeSet<SymbolId> = data.report_section.iter().map(|r| r.report_name).collect();

    for fd in &data.file_section {
        for &(rep_sym, _) in &fd.report_spans {
            fd_by_report.insert(rep_sym, fd.file_name);
        }
    }

    for (idx, rd) in data.report_section.iter().enumerate() {
        let rid = ReportId::new(u32::try_from(idx).unwrap_or(0));
        let orig_rd = String::from(
            interner
                .resolve_original(rd.report_name)
                .unwrap_or("report"),
        );
        if !fd_by_report.contains_key(&rd.report_name) {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0004UndefinedSymbol,
                Some(rd.span),
                format!("`{orig_rd}` is not defined in any `FD`"),
            ));
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(rd.span),
                format!("report `{orig_rd}` is not referenced by any `FD ... REPORT IS` clause"),
            ));
        }
        let mut has_detail = false;
        collect_typed_groups_into_catalog(
            &rd.groups,
            rid,
            &orig_rd,
            rd.global,
            &mut has_detail,
            &mut cat,
            interner,
        );
        cat.rd_by_sym
            .insert(rd.report_name, (rid, orig_rd.clone(), has_detail));
        if rd.global == GlobalVisibility::Global {
            cat.inherited_rds
                .insert(rd.report_name, (rid, orig_rd, has_detail));
        }
    }

    for fd in &data.file_section {
        for &(rep_sym, rep_span) in &fd.report_spans {
            if !rd_set.contains(&rep_sym) {
                let lbl = interner.resolve_original(rep_sym).unwrap_or("report");
                let _ = cat.seen_undef_reports.borrow_mut().insert(rep_sym);
                if data.report_section.is_empty() {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0004UndefinedSymbol,
                        Some(rep_span),
                        format!("no `RD` entry defined for report `{lbl}`"),
                    ));
                }
            } else if let Some((_, orig_rd, false)) = cat.rd_by_sym.get(&rep_sym) {
                sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::Redefinition,
                    Some(rep_span),
                    format!("no `DETAIL` line defined in report `{orig_rd}`"),
                ));
            }
        }
    }

    let mut pid = normal_proc_cnt;
    for decl in &proc_div.declaratives {
        let para_cnt = u32::try_from(decl.paragraphs.len()).unwrap_or(0);
        let start_proc = ProcedureId::new(pid);
        let end_proc = ProcedureId::new(pid.saturating_add(para_cnt));
        pid = pid.saturating_add(1).saturating_add(para_cnt);

        if let UseClause::BeforeReporting {
            global,
            group,
            use_span,
        } = &decl.use_clause
        {
            if let Some((rid, _, grp_up, _)) = cat.lookup_group(*group).cloned() {
                cat.hooks_by_report.entry(rid).or_default().push((
                    HirReportHookPlan {
                        group_name: grp_up,
                        start_proc,
                        end_proc,
                    },
                    *global,
                ));
            } else {
                let lbl = interner.resolve_original(*group).unwrap_or("group");
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0004UndefinedSymbol,
                    Some(*use_span),
                    format!("`{lbl}` in `USE BEFORE REPORTING` is not a report group"),
                ));
            }
        }
    }

    cat
}

/// Indexes named typed report groups (`01` or typed `02` under untyped `01`) in `cat`.
fn collect_typed_groups_into_catalog(
    groups: &[ReportGroupEntry],
    rid: ReportId,
    orig_rd: &str,
    rd_global: GlobalVisibility,
    has_detail: &mut bool,
    cat: &mut UnitReportCatalog,
    interner: &SymbolInterner,
) {
    let mut enclosing_01_typed = false;
    for g in groups {
        let lvl = match g.level {
            LevelNumber::Hierarchy(n) => n,
            _ => 1,
        };
        if lvl == 1 {
            enclosing_01_typed = g.group_type.is_some();
        }
        let is_valid_typed_group = g.group_type.is_some() && (lvl == 1 || !enclosing_01_typed);
        if is_valid_typed_group && let Some(gt) = g.group_type {
            if gt == ReportGroupType::Detail {
                *has_detail = true;
            }
            if let DataEntryName::Named(sym) = g.name {
                let up = String::from(interner.resolve_canonical(sym).unwrap_or(""));
                let entry = (rid, String::from(orig_rd), up, gt);
                cat.group_by_sym.insert(sym, entry.clone());
                if rd_global == GlobalVisibility::Global {
                    cat.inherited_groups.insert(sym, entry);
                }
            }
        }
    }
}

/// Validates and lowers all `RD` entries in `data.report_section` into [`HirReportPlan`]s.
///
/// # Arguments
///
/// * `data` - Unit `DATA DIVISION`.
/// * `catalog` - Pre-built [`UnitReportCatalog`].
/// * `ctx` - Mutable [`UnitLowerCtx`].
pub(crate) fn lower_unit_report_plans(
    data: &DataDivision,
    catalog: &UnitReportCatalog,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<HirReportPlan> {
    if !data.report_section.is_empty() {
        validate_report_unit_fd_clauses(data, catalog, ctx);
    }
    let mut plans = Vec::with_capacity(data.report_section.len());
    for (rd_idx, rd) in data.report_section.iter().enumerate() {
        plans.push(lower_single_rd_plan(rd_idx, rd, data, catalog, ctx));
    }
    plans
}

/// Validates `FD` dialect clauses (`LABEL RECORDS`, `DATA RECORDS`, `RECORD` on `LINE SEQUENTIAL`, missing `01`) in Report Writer units (`syn_reportwriter/0006`).
fn validate_report_unit_fd_clauses(
    data: &DataDivision,
    catalog: &UnitReportCatalog,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let base = ctx.dialect.base_dialect;
    if !matches!(base, Dialect::MicroFocus | Dialect::Cobol2002) {
        return;
    }
    for fd in &data.file_section {
        if let Some(sp) = fd.label_records_span {
            if base == Dialect::MicroFocus {
                ctx.sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::Obsolete,
                    Some(sp),
                    String::from("`LABEL RECORDS` is obsolete in Micro Focus COBOL"),
                ));
            } else {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0003Syntax,
                    Some(sp),
                    String::from("`LABEL RECORDS` is deleted in COBOL 2002"),
                ));
            }
        }
        if let Some(sp) = fd.record_clause_span
            && base == Dialect::MicroFocus
            && ctx.files.iter().any(|fc| {
                fc.name == fd.file_name
                    && fc.organization == coblang_hir::HirFileOrganization::LineSequential
            })
        {
            ctx.sink.emit(Diagnostic::warning(
                DiagnosticCode::W0006GeneralWarning,
                WarningGroup::Redefinition,
                Some(sp),
                String::from("`RECORD` clause ignored for `LINE SEQUENTIAL`"),
            ));
        }
        if let Some(sp) = fd.data_records_span
            && base == Dialect::MicroFocus
        {
            ctx.sink.emit(Diagnostic::warning(
                DiagnosticCode::W0006GeneralWarning,
                WarningGroup::Obsolete,
                Some(sp),
                String::from("`DATA RECORDS` is obsolete in Micro Focus COBOL"),
            ));
        }
        if base == Dialect::Cobol2002
            && fd.records.is_empty()
            && !fd.reports.iter().any(|r| catalog.rd_by_sym.contains_key(r))
        {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(fd.after_fd_span),
                String::from("`RECORD` description missing or invalid"),
            ));
        }
    }
}

/// Lowers one [`RdEntry`] into a [`HirReportPlan`], checking all ISO / `GnuCOBOL` semantic rules.
fn lower_single_rd_plan(
    rd_idx: usize,
    rd: &RdEntry,
    data: &DataDivision,
    catalog: &UnitReportCatalog,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirReportPlan {
    let rid = ReportId::new(u32::try_from(rd_idx).unwrap_or(0));
    let report_name = String::from(
        ctx.interner
            .resolve_original(rd.report_name)
            .unwrap_or("report"),
    );

    validate_rd_page_limits(rd, ctx.sink);

    let (fc_id, file_name, assign_target, assign_is_external, assign_place) =
        resolve_rd_file_binding(rd, data, ctx);

    let code_spec = lower_rd_code_clause(rd.code_clause.as_ref(), ctx);
    let col_limit = lower_rd_col_limit(rd.col_limit.as_ref(), ctx);
    let page_limits = HirReportPageLimits {
        line_limit: rd.page_limit,
        col_limit,
        heading: rd.heading,
        first_detail: rd.first_detail,
        last_detail: rd.last_detail,
        footing: rd.footing,
    };

    let (controls, ctrl_lookup) = lower_rd_controls(rd, ctx);

    let (pc_id, lc_id, ps_id) = ctx.layout.rd_special_regs.get(rd_idx).copied().unwrap_or((
        DataItemId::new(0),
        DataItemId::new(0),
        DataItemId::new(0),
    ));

    let mut sum_counters = Vec::new();
    let mut sum_name_to_idx: BTreeMap<SymbolId, u32> = BTreeMap::new();
    let groups = lower_rd_groups(
        rd_idx,
        rd,
        &controls,
        &ctrl_lookup,
        &mut sum_counters,
        &mut sum_name_to_idx,
        ctx,
    );

    let hooks = catalog
        .hooks_by_report
        .get(&rid)
        .map(|list| list.iter().map(|(h, _)| h.clone()).collect())
        .unwrap_or_default();

    HirReportPlan {
        report_id: rid,
        report_name,
        file_connector: fc_id,
        file_name,
        assign_target,
        assign_is_external,
        assign_place,
        has_line_advancing: true,
        code_spec,
        page_limits,
        controls,
        sum_counters,
        groups,
        hooks,
        page_counter_item: pc_id,
        line_counter_item: lc_id,
        print_switch_item: ps_id,
    }
}

/// Validates `PAGE LIMIT`, `HEADING`, `FIRST DETAIL`, `LAST DETAIL`, and `FOOTING` ordering & bounds.
fn validate_rd_page_limits(rd: &RdEntry, sink: &mut DiagnosticSink) {
    let Some(limit) = rd.page_limit else {
        return;
    };
    if let Some(h) = rd.heading
        && (h == 0 || h > limit)
    {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            rd.heading_span.or(Some(rd.span)),
            format!("`HEADING {h}` exceeds `PAGE LIMIT {limit}`"),
        ));
    }
    if let Some(fd) = rd.first_detail {
        let bad_hi = fd == 0 || fd > limit;
        let bad_lo = rd.heading.is_some_and(|h| h <= limit && fd < h);
        if bad_hi || bad_lo {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                rd.first_detail_span.or(Some(rd.span)),
                format!("`FIRST DETAIL {fd}` conflicts with `HEADING` / `PAGE LIMIT {limit}`"),
            ));
        }
    }
    if let Some(ld) = rd.last_detail {
        let bad_hi = ld == 0 || ld > limit;
        let bad_fd = rd.first_detail.is_some_and(|fd| fd <= limit && ld < fd);
        let bad_h = rd.heading.is_some_and(|h| h <= limit && ld < h);
        if bad_hi || bad_fd || bad_h {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                rd.last_detail_span.or(Some(rd.span)),
                format!("`LAST DETAIL {ld}` conflicts with preceding page region bounds"),
            ));
        }
    }
    if let Some(ft) = rd.footing {
        let bad_hi = ft == 0 || ft > limit;
        let bad_ld = rd.last_detail.is_some_and(|ld| ld <= limit && ft < ld);
        let bad_fd = rd.first_detail.is_some_and(|fd| fd <= limit && ft < fd);
        let bad_h = rd.heading.is_some_and(|h| h <= limit && ft < h);
        if bad_hi || bad_ld || bad_fd || bad_h {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                rd.footing_span.or(Some(rd.span)),
                format!("`FOOTING {ft}` conflicts with preceding page region bounds"),
            ));
        }
    }
}

/// Resolves the `FD` file connector bound to `rd`.
fn resolve_rd_file_binding(
    rd: &RdEntry,
    data: &DataDivision,
    ctx: &UnitLowerCtx<'_>,
) -> (FileConnectorId, String, String, bool, Option<HirPlace>) {
    let fd_sym = data
        .file_section
        .iter()
        .find(|fd| fd.reports.contains(&rd.report_name))
        .map(|fd| fd.file_name);
    if let Some(f_sym) = fd_sym {
        let f_orig = String::from(ctx.interner.resolve_original(f_sym).unwrap_or("REPORT"));
        if let Some(fc) = ctx.files.iter().find(|c| c.name == f_sym) {
            let (target, is_ext, pl) = match &fc.assign {
                coblang_hir::HirAssignSpec::Literal(s) => (s.clone(), false, None),
                coblang_hir::HirAssignSpec::ExternalEnv(s)
                | coblang_hir::HirAssignSpec::EnvOrLiteral(s) => (s.clone(), true, None),
                coblang_hir::HirAssignSpec::DynamicItem(item_id) => {
                    let pl = ctx
                        .layout
                        .items
                        .get(item_id.as_u32() as usize)
                        .map(|it| simple_place_for_item(it, rd.span));
                    (String::new(), false, pl)
                }
                coblang_hir::HirAssignSpec::DisplayDevice
                | coblang_hir::HirAssignSpec::KeyboardDevice => (String::new(), false, None),
            };
            return (fc.id, f_orig, target, is_ext, pl);
        }
        return (FileConnectorId::new(0), f_orig, String::new(), true, None);
    }
    (
        FileConnectorId::new(0),
        String::from("REPORT"),
        String::new(),
        true,
        None,
    )
}

/// Lowers `RD ... CODE IS ...`.
fn lower_rd_code_clause(
    code: Option<&RdCodeClauseAst>,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirReportCodeSpec {
    match code {
        None => HirReportCodeSpec::None,
        Some(RdCodeClauseAst::Literal(sym, _)) => {
            let raw = ctx.interner.resolve_original(*sym).unwrap_or("");
            HirReportCodeSpec::Literal(encode_text_bytes(raw, ctx.dialect))
        }
        Some(RdCodeClauseAst::DataName(path, sp)) => {
            if let Some(dref) = path_to_dataref(path, *sp)
                && let Some(pl) = resolve_place(&dref, ctx)
            {
                HirReportCodeSpec::Place(pl)
            } else {
                HirReportCodeSpec::None
            }
        }
    }
}

/// Lowers `PAGE LIMIT ... c COLUMNS` / `LINE LIMIT c`.
fn lower_rd_col_limit(
    col_lim: Option<&RdColLimitAst>,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<HirExpr> {
    match col_lim {
        None => None,
        Some(RdColLimitAst::Literal(n, sp)) => {
            let sym = ctx
                .interner
                .intern(&n.to_string())
                .unwrap_or_else(|_| ctx.builtins_get("0"));
            Some(HirExpr::Const {
                value: coblang_hir::HirConstValue::Decimal {
                    text: sym,
                    scale: ScaleFactor::ZERO,
                    sign: coblang_hir::HirDecimalSign::UnsignedOrEmbedded,
                },
                span: *sp,
            })
        }
        Some(RdColLimitAst::DataName(path, sp)) => {
            let dref = path_to_dataref(path, *sp)?;
            let pl = resolve_place(&dref, ctx)?;
            Some(HirExpr::Place(pl))
        }
    }
}

/// Validates and lowers `RD ... CONTROLS ARE ...`.
fn lower_rd_controls(
    rd: &RdEntry,
    ctx: &mut UnitLowerCtx<'_>,
) -> (Vec<HirReportControlSpec>, BTreeMap<DataItemId, u16>) {
    let mut out = Vec::new();
    let mut lookup = BTreeMap::new();
    let mut seen_final = false;

    let has_implicit_final_ref = rd.groups.iter().any(|g| {
        matches!(g.type_control, Some(ReportControlAst::Final(_)))
            || g.sums
                .iter()
                .any(|s| matches!(s.reset_on, Some(ReportControlAst::Final(_))))
    });
    let explicit_final_first = matches!(rd.control_items.first(), Some(ReportControlAst::Final(_)));
    if !explicit_final_first && has_implicit_final_ref {
        out.push(HirReportControlSpec::Final);
    }

    for (idx, item) in rd.control_items.iter().enumerate() {
        match item {
            ReportControlAst::Final(sp) => {
                if idx > 0 || seen_final {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(*sp),
                        String::from(
                            "`FINAL` may only appear once as the first control in `CONTROLS`",
                        ),
                    ));
                } else {
                    seen_final = true;
                    out.push(HirReportControlSpec::Final);
                }
            }
            ReportControlAst::Field(path, sp) => {
                if let Some(dref) = path_to_dataref(path, *sp)
                    && let Some(pl) = resolve_place(&dref, ctx)
                {
                    let c_idx = u16::try_from(out.len()).unwrap_or(0);
                    let nm = path
                        .first()
                        .and_then(|s| ctx.interner.resolve_canonical(*s).ok())
                        .unwrap_or("CTRL")
                        .to_owned();
                    lookup.insert(pl.item, c_idx);
                    out.push(HirReportControlSpec::Field {
                        name: nm,
                        place: pl,
                    });
                }
            }
        }
    }

    // GnuCOBOL permits `PRESENT AFTER NEW <field>` even when `<field>` is omitted from `CONTROLS` (`syn_reportwriter/0010`).
    for g in &rd.groups {
        if let Some(ReportPresentGateAst::AfterNew { triggers, .. }) = &g.present_gate {
            for trig in triggers {
                if let ReportAfterNewAtomAst::Control(path, sp) = trig
                    && let Some(dref) = path_to_dataref(path, *sp)
                    && let Some(pl) = resolve_place(&dref, ctx)
                    && !lookup.contains_key(&pl.item)
                {
                    let c_idx = u16::try_from(out.len()).unwrap_or(0);
                    let nm = path
                        .first()
                        .and_then(|s| ctx.interner.resolve_canonical(*s).ok())
                        .unwrap_or("CTRL")
                        .to_owned();
                    lookup.insert(pl.item, c_idx);
                    out.push(HirReportControlSpec::Field {
                        name: nm,
                        place: pl,
                    });
                }
            }
        }
    }
    (out, lookup)
}

/// Validates and lowers all typed report groups under `rd`.
#[expect(
    clippy::too_many_arguments,
    reason = "Threads control table and sum-counter tables across group hierarchy lowering."
)]
fn lower_rd_groups(
    rd_idx: usize,
    rd: &RdEntry,
    controls: &[HirReportControlSpec],
    ctrl_lookup: &BTreeMap<DataItemId, u16>,
    sum_counters: &mut Vec<HirReportSumSpec>,
    sum_name_to_idx: &mut BTreeMap<SymbolId, u32>,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<HirReportGroupPlan> {
    let mut plans = Vec::new();
    let entries = &rd.groups;
    let mut i = 0;

    while let Some(top) = entries.get(i) {
        let top_lvl = entry_level_u8(top);
        let end = find_subtree_end(entries, i, top_lvl);

        if let Some(gt) = top.group_type {
            for (sub_idx, sub) in entries.get(i + 1..end).unwrap_or(&[]).iter().enumerate() {
                if sub.group_type.is_some() {
                    let _ = sub_idx;
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        sub.group_type_span.or(Some(sub.span)),
                        String::from("secondary `TYPE` clause is not permitted inside a typed `01` report group"),
                    ));
                }
            }
            if let Some(gp) = lower_typed_group_subtree(
                rd_idx,
                rd,
                i,
                end,
                gt,
                controls,
                ctrl_lookup,
                sum_counters,
                sum_name_to_idx,
                ctx,
            ) {
                plans.push(gp);
            }
        } else {
            // Untyped `01` container (COBOL 2002 super-group, e.g. `01 HEADING-LINE` -> `02 TYPE PH ...`).
            let mut j = i + 1;
            let mut found_typed_sub = false;
            while j < end {
                let Some(sub) = entries.get(j) else {
                    break;
                };
                let sub_lvl = entry_level_u8(sub);
                let sub_end = find_subtree_end(entries, j, sub_lvl);
                if let Some(gt) = sub.group_type {
                    found_typed_sub = true;
                    if let Some(gp) = lower_typed_group_subtree(
                        rd_idx,
                        rd,
                        j,
                        sub_end,
                        gt,
                        controls,
                        ctrl_lookup,
                        sum_counters,
                        sum_name_to_idx,
                        ctx,
                    ) {
                        plans.push(gp);
                    }
                }
                j = sub_end;
            }
            if !found_typed_sub {
                // Validate untyped `01` standalone items (`RW301M` / `syn_reportwriter/0006`).
                let _ = lower_typed_group_subtree(
                    rd_idx,
                    rd,
                    i,
                    end,
                    ReportGroupType::Detail,
                    controls,
                    ctrl_lookup,
                    sum_counters,
                    sum_name_to_idx,
                    ctx,
                );
            }
        }
        i = end;
    }

    plans
}

/// Validates and lowers a single typed report group subtree `entries[root_idx..end_idx]`.
#[expect(
    clippy::too_many_arguments,
    reason = "Threads RD metadata, control hierarchy, and sum counter accumulators."
)]
fn lower_typed_group_subtree(
    rd_idx: usize,
    rd: &RdEntry,
    root_idx: usize,
    end_idx: usize,
    gt: ReportGroupType,
    controls: &[HirReportControlSpec],
    ctrl_lookup: &BTreeMap<DataItemId, u16>,
    sum_counters: &mut Vec<HirReportSumSpec>,
    sum_name_to_idx: &mut BTreeMap<SymbolId, u32>,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<HirReportGroupPlan> {
    let entries = &rd.groups;
    let root = entries.get(root_idx)?;
    let role = resolve_group_role(gt, root, controls, ctrl_lookup, ctx)?;

    let mut line_tracker = GroupLineTracker {
        page_limit: rd.page_limit,
        role,
        seen_relative: false,
        last_group_abs: None,
        last_elem_abs: None,
        has_writable_elem: false,
    };

    let root_has_children = root_idx + 1 < end_idx;
    let root_line = validate_and_lower_entry_lines(
        root,
        !root_has_children,
        &mut line_tracker,
        ctx.dialect,
        ctx.sink,
    );
    let next_group = root.next_group.map(|ng| match ng {
        ReportNextGroupAst::Absolute { line, .. } => HirReportNextGroupSpec::Absolute(line),
        ReportNextGroupAst::Relative { offset, .. } => HirReportNextGroupSpec::Relative(offset),
        ReportNextGroupAst::NextPage { .. } => HirReportNextGroupSpec::NextPage,
    });
    let present_when = lower_present_gate(root.present_gate.as_ref(), controls, ctrl_lookup, ctx);

    let owner_ctrl_idx = match role {
        HirReportGroupRole::ControlFooting(ci) | HirReportGroupRole::ControlHeading(ci) => ci,
        _ => u16::try_from(controls.len().saturating_sub(1)).unwrap_or(0),
    };

    let mut first_on_line = root_line.is_some();
    let children = if root_has_children {
        lower_group_children(
            rd_idx,
            entries,
            root_idx + 1,
            end_idx,
            owner_ctrl_idx,
            controls,
            ctrl_lookup,
            &mut line_tracker,
            &mut first_on_line,
            sum_counters,
            sum_name_to_idx,
            ctx,
        )
    } else if is_printable_leaf(root) {
        let elem = lower_elem_entry(
            rd_idx,
            root_idx,
            root,
            None,
            owner_ctrl_idx,
            controls,
            ctrl_lookup,
            &mut line_tracker,
            &mut first_on_line,
            sum_counters,
            sum_name_to_idx,
            ctx,
        );
        vec![HirReportNodePlan::Elementary(elem)]
    } else {
        Vec::new()
    };

    let name = match root.name {
        DataEntryName::Named(sym) => Some(String::from(
            ctx.interner.resolve_canonical(sym).unwrap_or(""),
        )),
        DataEntryName::Filler => None,
    };

    let backing_root_item = if line_tracker.has_writable_elem {
        find_enclosing_01_item(rd_idx, entries, root_idx, ctx.layout)
    } else {
        None
    };

    Some(HirReportGroupPlan {
        name,
        role,
        line: root_line,
        next_group,
        present_when,
        backing_root_item,
        children,
    })
}

/// Finds the synthesized `01` `(DataItemId, total_byte_size)` enclosing `entry_idx`.
fn find_enclosing_01_item(
    rd_idx: usize,
    entries: &[ReportGroupEntry],
    entry_idx: usize,
    layout: &SolvedDataLayout,
) -> Option<(DataItemId, u32)> {
    let mut k = entry_idx;
    loop {
        if let Some(e) = entries.get(k)
            && entry_level_u8(e) == 1
        {
            let id = *layout.report_entry_items.get(&(rd_idx, k))?;
            let sz = layout.items.get(id.as_u32() as usize)?.total_byte_size;
            return Some((id, sz));
        }
        if k == 0 {
            return None;
        }
        k -= 1;
    }
}

/// Line-order & `PAGE LIMIT` state tracked within one report group.
struct GroupLineTracker {
    /// Optional `RD` `PAGE LIMIT`.
    page_limit: Option<u32>,
    /// Active group role.
    role: HirReportGroupRole,
    /// `true` once any relative `LINE PLUS n` clause has appeared in this group.
    seen_relative: bool,
    /// Previous group-level absolute `LINE n`.
    last_group_abs: Option<u32>,
    /// Previous elementary-level absolute `LINE n`.
    last_elem_abs: Option<u32>,
    /// Set to `true` if any elementary item reads/clears its own report-section storage.
    has_writable_elem: bool,
}

/// Validates `entry.line_clauses` against ISO / `GnuCOBOL` line ordering rules.
fn validate_and_lower_entry_lines(
    entry: &ReportGroupEntry,
    is_elementary: bool,
    tracker: &mut GroupLineTracker,
    dialect: &DialectConfig,
    sink: &mut DiagnosticSink,
) -> Option<HirReportLineSpec> {
    let mut resolved = None;
    for lc in &entry.line_clauses {
        match *lc {
            ReportLineClauseAst::Absolute { line, span } => {
                if line == 0 {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(span),
                        String::from("`LINE` integer must be > 0"),
                    ));
                } else if tracker.page_limit.is_none()
                    && !matches!(dialect.base_dialect, Dialect::Gnu | Dialect::MicroFocus)
                {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(span),
                        format!("`LINE {line}` requires `PAGE LIMIT` in `RD`"),
                    ));
                } else if let Some(lim) = tracker.page_limit
                    && line > lim
                {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(span),
                        format!("`LINE {line}` exceeds `PAGE LIMIT {lim}`"),
                    ));
                } else if tracker.seen_relative {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(span),
                        String::from(
                            "absolute `LINE` clause cannot follow relative `LINE PLUS` in same group",
                        ),
                    ));
                } else if is_elementary && tracker.last_elem_abs == Some(line) {
                    sink.emit(Diagnostic::warning(
                        DiagnosticCode::W0006GeneralWarning,
                        WarningGroup::Redefinition,
                        Some(span),
                        format!("duplicate `LINE {line}` ignored"),
                    ));
                    // Duplicate elementary `LINE n` stays on the same line (`None` = no new line break).
                } else if tracker.last_group_abs.is_some_and(|prev| line <= prev) {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(span),
                        format!("non-ascending `LINE {line}` in report group"),
                    ));
                } else {
                    tracker.last_group_abs = Some(line);
                    tracker.last_elem_abs = Some(line);
                    resolved = Some(HirReportLineSpec::Absolute(line));
                }
            }
            ReportLineClauseAst::Relative { offset, span } => {
                if offset == 0 {
                    if dialect.base_dialect == Dialect::MicroFocus {
                        sink.emit(Diagnostic::error(
                            DiagnosticCode::E0007InvalidLayout,
                            Some(span),
                            String::from("`LINE PLUS 0` is invalid; must be > 0"),
                        ));
                    } else {
                        sink.emit(Diagnostic::warning(
                            DiagnosticCode::W0006GeneralWarning,
                            WarningGroup::Redefinition,
                            Some(span),
                            String::from("`LINE PLUS 0` is not implemented"),
                        ));
                    }
                }
                tracker.seen_relative = true;
                tracker.last_elem_abs = None;
                resolved = Some(HirReportLineSpec::Relative(offset));
            }
            ReportLineClauseAst::OnNextPage { line, span } => {
                if tracker.page_limit.is_none() {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(span),
                        String::from("`NEXT PAGE` requires `PAGE LIMIT` in `RD`"),
                    ));
                } else if matches!(
                    tracker.role,
                    HirReportGroupRole::ReportHeading
                        | HirReportGroupRole::PageHeading
                        | HirReportGroupRole::PageFooting
                ) {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(span),
                        String::from(
                            "`ON NEXT PAGE` is not permitted on heading/page-footing groups",
                        ),
                    ));
                } else if let Some(lim) = tracker.page_limit
                    && (line == 0 || line > lim)
                {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(span),
                        format!("`LINE {line} NEXT PAGE` exceeds `PAGE LIMIT {lim}`"),
                    ));
                } else {
                    tracker.last_elem_abs = Some(line);
                    resolved = Some(HirReportLineSpec::OnNextPage(line));
                }
            }
        }
    }
    resolved
}

/// Recursively lowers subordinate nodes `entries[start..end]` inside a report group.
#[expect(
    clippy::too_many_arguments,
    reason = "Recursive AST-to-HIR report subtree walker."
)]
fn lower_group_children(
    rd_idx: usize,
    entries: &[ReportGroupEntry],
    start: usize,
    end: usize,
    owner_ctrl_idx: u16,
    controls: &[HirReportControlSpec],
    ctrl_lookup: &BTreeMap<DataItemId, u16>,
    line_tracker: &mut GroupLineTracker,
    first_on_line: &mut bool,
    sum_counters: &mut Vec<HirReportSumSpec>,
    sum_name_to_idx: &mut BTreeMap<SymbolId, u32>,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<HirReportNodePlan> {
    let mut nodes = Vec::new();
    let mut i = start;
    while i < end {
        let Some(entry) = entries.get(i) else {
            break;
        };
        let lvl = entry_level_u8(entry);
        let sub_end = find_subtree_end(entries, i, lvl).min(end);
        let has_children = i + 1 < sub_end;
        let line = validate_and_lower_entry_lines(
            entry,
            !has_children,
            line_tracker,
            ctx.dialect,
            ctx.sink,
        );
        if line.is_some() {
            *first_on_line = true;
        }
        if has_children {
            let col_vec = lower_entry_columns(entry, first_on_line, false, ctx);
            let present_when =
                lower_present_gate(entry.present_gate.as_ref(), controls, ctrl_lookup, ctx);
            let occurs = lower_report_occurs(rd_idx, i, entry, ctx);
            let sub_children = lower_group_children(
                rd_idx,
                entries,
                i + 1,
                sub_end,
                owner_ctrl_idx,
                controls,
                ctrl_lookup,
                line_tracker,
                first_on_line,
                sum_counters,
                sum_name_to_idx,
                ctx,
            );
            let name = match entry.name {
                DataEntryName::Named(s) => Some(String::from(
                    ctx.interner.resolve_canonical(s).unwrap_or(""),
                )),
                DataEntryName::Filler => None,
            };
            nodes.push(HirReportNodePlan::SubGroup(HirReportSubGroupPlan {
                name,
                line,
                column: col_vec.first().copied(),
                present_when,
                occurs,
                children: sub_children,
            }));
        } else {
            let elem = lower_elem_entry(
                rd_idx,
                i,
                entry,
                line,
                owner_ctrl_idx,
                controls,
                ctrl_lookup,
                line_tracker,
                first_on_line,
                sum_counters,
                sum_name_to_idx,
                ctx,
            );
            nodes.push(HirReportNodePlan::Elementary(elem));
        }
        i = sub_end;
    }
    nodes
}

/// Validates and lowers one elementary `ReportGroupEntry` into [`HirReportElemPlan`].
#[expect(
    clippy::too_many_arguments,
    reason = "Validates mutual-exclusion, SUM, COLUMN, and writable storage for one elementary entry."
)]
fn lower_elem_entry(
    rd_idx: usize,
    entry_idx: usize,
    entry: &ReportGroupEntry,
    line: Option<HirReportLineSpec>,
    owner_ctrl_idx: u16,
    controls: &[HirReportControlSpec],
    ctrl_lookup: &BTreeMap<DataItemId, u16>,
    line_tracker: &mut GroupLineTracker,
    first_on_line: &mut bool,
    sum_counters: &mut Vec<HirReportSumSpec>,
    sum_name_to_idx: &mut BTreeMap<SymbolId, u32>,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirReportElemPlan {
    let mut columns = lower_entry_columns(entry, first_on_line, true, ctx);
    if columns.is_empty()
        && entry.sums.is_empty()
        && ctx.dialect.base_dialect != Dialect::Cobol85
        && (line.is_some() || line_tracker.seen_relative || line_tracker.last_group_abs.is_some())
        && (!entry.sources.is_empty() || !entry.values.is_empty() || entry.picture.is_some())
    {
        columns.push(HirReportColSpec::Relative {
            offset: 1,
            align: HirReportColAlign::Left,
        });
        *first_on_line = false;
    }
    let item_id_opt = ctx
        .layout
        .report_entry_items
        .get(&(rd_idx, entry_idx))
        .copied();
    let item_ty_opt = item_id_opt
        .and_then(|id| ctx.layout.items.get(id.as_u32() as usize))
        .map(|it| it.ty.clone());

    validate_source_value_sum_exclusivity(entry, ctx.sink);

    let mut sources = Vec::new();
    let mut clear_place_after_present = None;

    if !entry.sums.is_empty() {
        let c_idx = lower_sum_clauses_for_elem(
            entry,
            item_id_opt,
            item_ty_opt.as_ref(),
            owner_ctrl_idx,
            controls,
            ctrl_lookup,
            sum_counters,
            sum_name_to_idx,
            ctx,
        );
        sources.push(HirReportSourceKind::SumCounter(c_idx));
    } else if !entry.sources.is_empty() {
        if entry.picture.is_none() {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(entry.span),
                String::from("`PICTURE` clause required for `SOURCE` report item"),
            ));
        }
        for (src_expr, _) in &entry.sources {
            sources.push(lower_report_source_kind(src_expr, ctx));
        }
    } else if !entry.values.is_empty() {
        let target_w = item_ty_opt
            .as_ref()
            .map_or(1, |t| t.static_byte_size().max(1));
        for (atom, _) in &entry.values {
            sources.push(HirReportSourceKind::LiteralBytes(encode_value_atom_bytes(
                atom,
                target_w,
                ctx.dialect,
                ctx.interner,
            )));
        }
    } else if let Some(own_id) = item_id_opt
        && (!columns.is_empty() || entry.picture.is_some())
        && let Some(own_it) = ctx.layout.items.get(own_id.as_u32() as usize)
    {
        // COBOL 2002 / GnuCOBOL writable report item (`MOVE 100 TO NUMS(1)`): reads own backing slot.
        let own_pl = simple_place_for_item(own_it, entry.span);
        line_tracker.has_writable_elem = true;
        clear_place_after_present = Some(own_pl.clone());
        sources.push(HirReportSourceKind::Place(own_pl));
    }

    let formatter = if clear_place_after_present.is_some() {
        let space_byte = match ctx.dialect.internal_charset {
            InternalCharsetMode::Ascii => b' ',
            InternalCharsetMode::Ebcdic => 0x40,
        };
        let w = item_ty_opt
            .as_ref()
            .map_or(1, |t| t.static_byte_size().max(1));
        HirReportFormatterKind::FixedAlpha {
            width: w,
            justified: JustifiedSpec::Left,
            space_byte,
        }
    } else {
        build_elem_formatter(
            entry,
            item_ty_opt.as_ref(),
            &sources,
            ctx.pic_ctx,
            ctx.dialect,
            ctx.interner,
        )
    };
    let group_indicate = if entry.group_indicate.is_some() {
        HirGroupIndicateMode::GroupIndicate
    } else {
        HirGroupIndicateMode::AlwaysDisplay
    };
    let present_when = lower_present_gate(entry.present_gate.as_ref(), controls, ctrl_lookup, ctx);
    let occurs = lower_report_occurs(rd_idx, entry_idx, entry, ctx);
    let name = match entry.name {
        DataEntryName::Named(s) => Some(String::from(
            ctx.interner.resolve_canonical(s).unwrap_or(""),
        )),
        DataEntryName::Filler => None,
    };

    HirReportElemPlan {
        name,
        line,
        columns,
        sources,
        formatter,
        group_indicate,
        present_when,
        occurs,
        clear_place_after_present,
    }
}

/// Checks `SOURCE` / `VALUE` / `SUM` mutual exclusion on a single report entry (`syn_reportwriter/0006`).
fn validate_source_value_sum_exclusivity(entry: &ReportGroupEntry, sink: &mut DiagnosticSink) {
    if !entry.sources.is_empty() && !entry.sums.is_empty() {
        let sp = entry.sums.first().map_or(entry.span, |s| s.span);
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(sp),
            String::from("report entry cannot specify both `SOURCE` and `SUM`"),
        ));
    }
    if !entry.sources.is_empty() && !entry.values.is_empty() {
        let sp = entry.sources.first().map_or(entry.span, |(_, s)| *s);
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(sp),
            String::from("report entry cannot specify both `VALUE` and `SOURCE`"),
        ));
    }
    if !entry.values.is_empty() && !entry.sums.is_empty() {
        let sp = entry.sums.first().map_or(entry.span, |s| s.span);
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(sp),
            String::from("report entry cannot specify both `VALUE` and `SUM`"),
        ));
    }
}

/// Validates and lowers `SUM` clauses on an elementary report item (`syn_reportwriter/0004`).
#[expect(
    clippy::too_many_arguments,
    reason = "Resolves numeric addends, rolling counters, UPON filters, and RESET ON controls."
)]
fn lower_sum_clauses_for_elem(
    entry: &ReportGroupEntry,
    item_id_opt: Option<DataItemId>,
    item_ty_opt: Option<&HirType>,
    owner_ctrl_idx: u16,
    controls: &[HirReportControlSpec],
    ctrl_lookup: &BTreeMap<DataItemId, u16>,
    sum_counters: &mut Vec<HirReportSumSpec>,
    sum_name_to_idx: &mut BTreeMap<SymbolId, u32>,
    ctx: &mut UnitLowerCtx<'_>,
) -> u32 {
    let counter_is_numeric = item_ty_opt.is_some_and(|t| {
        matches!(
            t.category(),
            DataCategory::Numeric | DataCategory::NumericEdited
        )
    });
    if entry.picture.is_none() {
        let sp = entry.sums.first().map_or(entry.span, |s| s.span);
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(sp),
            String::from("`SUM` target item requires a `PICTURE` clause"),
        ));
    } else if !counter_is_numeric {
        let sp = entry.sums.first().map_or(entry.span, |s| s.span);
        ctx.sink.emit(Diagnostic::warning(
            DiagnosticCode::W0006GeneralWarning,
            WarningGroup::Redefinition,
            Some(sp),
            String::from("non-numeric `PICTURE` clause for `SUM`"),
        ));
    }

    let mut addend_exprs = Vec::new();
    let mut rolling_counters = Vec::new();
    let mut upon_details = Vec::new();
    let mut reset_control_idx = owner_ctrl_idx;

    for sc in &entry.sums {
        for op_expr in &sc.operands {
            if let Expr::Data(dref) = op_expr
                && dref.qualifiers_and_subscripts_empty()
                && let Some(&sub_cidx) = sum_name_to_idx.get(&dref.name.base)
                && sum_counters
                    .get(sub_cidx as usize)
                    .is_some_and(|sub| sub.owner_control_idx > owner_ctrl_idx)
            {
                rolling_counters.push(sub_cidx);
                continue;
            }
            if let Expr::Data(dref) = op_expr {
                if let Some(pl) = resolve_place(dref, ctx) {
                    let op_cat = pl.effective_ty.category();
                    if !matches!(op_cat, DataCategory::Numeric | DataCategory::NumericEdited) {
                        ctx.sink.emit(Diagnostic::error(
                            DiagnosticCode::E0008TypeMismatch,
                            Some(dref.span),
                            String::from("`SUM` operand must be numeric"),
                        ));
                    }
                    addend_exprs.push(HirExpr::Place(pl));
                }
            } else {
                let (he, cat) = lower_expr_with_cat(op_expr, ctx);
                if !matches!(cat, DataCategory::Numeric | DataCategory::NumericEdited) {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0008TypeMismatch,
                        Some(op_expr.span()),
                        String::from("`SUM` operand must be numeric"),
                    ));
                }
                addend_exprs.push(he);
            }
        }
        for &(det_sym, _) in &sc.upon_details {
            let up = String::from(ctx.interner.resolve_canonical(det_sym).unwrap_or(""));
            if !up.is_empty() {
                upon_details.push(up);
            }
        }
        if let Some(rst) = &sc.reset_on
            && let Some(r_idx) = resolve_control_index(rst, controls, ctrl_lookup, ctx)
        {
            reset_control_idx = r_idx;
        }
    }

    let c_idx = u32::try_from(sum_counters.len()).unwrap_or(0);
    let name = match entry.name {
        DataEntryName::Named(sym) => {
            sum_name_to_idx.insert(sym, c_idx);
            Some(String::from(
                ctx.interner.resolve_canonical(sym).unwrap_or(""),
            ))
        }
        DataEntryName::Filler => None,
    };
    sum_counters.push(HirReportSumSpec {
        counter_idx: c_idx,
        name,
        backing_item: item_id_opt,
        owner_control_idx: owner_ctrl_idx,
        reset_control_idx,
        addend_exprs,
        rolling_counters,
        upon_details,
    });
    c_idx
}

/// Trait-like helper checking whether a `DataRef` is a bare unqualified, unsubscripted name.
trait BareDataRefExt {
    /// Returns `true` when `qualifiers`, `subscripts`, and `refmod` are all empty.
    fn qualifiers_and_subscripts_empty(&self) -> bool;
}

impl BareDataRefExt for coblang_syntax::ast::DataRef {
    fn qualifiers_and_subscripts_empty(&self) -> bool {
        self.name.qualifiers.is_empty() && self.subscripts.is_empty() && self.refmod.is_none()
    }
}

/// Lowers a `SOURCE IS expr` expression into [`HirReportSourceKind`].
fn lower_report_source_kind(expr: &Expr, ctx: &mut UnitLowerCtx<'_>) -> HirReportSourceKind {
    if let Expr::Data(dref) = expr {
        let base_up = ctx.interner.resolve_canonical(dref.name.base).unwrap_or("");
        if base_up == "PAGE-COUNTER" && dref.name.qualifiers.is_empty() {
            return HirReportSourceKind::PageCounter;
        }
        if base_up == "LINE-COUNTER" && dref.name.qualifiers.is_empty() {
            return HirReportSourceKind::LineCounter;
        }
        if let Some(pl) = resolve_place(dref, ctx) {
            return HirReportSourceKind::Place(pl);
        }
    }
    let (hir_e, _) = lower_expr_with_cat(expr, ctx);
    HirReportSourceKind::Expr(hir_e)
}

/// Validates and lowers `COLUMN` coordinates (`LEFT`/`CENTER`/`RIGHT`, `PLUS` on first column).
fn lower_entry_columns(
    entry: &ReportGroupEntry,
    first_on_line: &mut bool,
    is_printable: bool,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<HirReportColSpec> {
    let mut out = Vec::with_capacity(entry.columns.len());
    for coord in &entry.columns {
        match *coord {
            ReportColCoordAst::Absolute { col, align, span } => {
                if col == 0 {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(span),
                        String::from("`COLUMN` integer must be > 0"),
                    ));
                }
                let hir_align = match align {
                    ReportColAlignAst::LeftDefault => HirReportColAlign::Left,
                    ReportColAlignAst::ExplicitLeft => HirReportColAlign::ExplicitLeft,
                    ReportColAlignAst::Center => HirReportColAlign::Center,
                    ReportColAlignAst::Right => HirReportColAlign::Right,
                };
                out.push(HirReportColSpec::Absolute {
                    col: col.max(1),
                    align: hir_align,
                });
                if is_printable {
                    *first_on_line = false;
                }
            }
            ReportColCoordAst::Relative {
                offset,
                align,
                span,
            } => {
                if offset == 0 && ctx.dialect.base_dialect == Dialect::MicroFocus {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(span),
                        String::from("`COLUMN PLUS 0` is invalid; must be > 0"),
                    ));
                }
                let hir_align = match align {
                    ReportColAlignAst::LeftDefault => HirReportColAlign::Left,
                    ReportColAlignAst::ExplicitLeft => HirReportColAlign::ExplicitLeft,
                    ReportColAlignAst::Center => HirReportColAlign::Center,
                    ReportColAlignAst::Right => HirReportColAlign::Right,
                };
                if align != ReportColAlignAst::LeftDefault {
                    if ctx.dialect.base_dialect == Dialect::MicroFocus {
                        ctx.sink.emit(Diagnostic::warning(
                            DiagnosticCode::W0006GeneralWarning,
                            WarningGroup::Redefinition,
                            Some(span),
                            String::from(
                                "`PLUS` is not recommended with `LEFT`, `RIGHT`, or `CENTER`",
                            ),
                        ));
                    } else {
                        ctx.sink.emit(Diagnostic::error(
                            DiagnosticCode::E0007InvalidLayout,
                            Some(span),
                            String::from("`PLUS` is not allowed with `LEFT`, `RIGHT`, or `CENTER`"),
                        ));
                    }
                }
                if is_printable && *first_on_line {
                    ctx.sink.emit(Diagnostic::warning(
                        DiagnosticCode::W0006GeneralWarning,
                        WarningGroup::Redefinition,
                        Some(span),
                        String::from("`PLUS` is ignored on first field of line"),
                    ));
                    out.push(HirReportColSpec::Absolute {
                        col: 1,
                        align: hir_align,
                    });
                    *first_on_line = false;
                } else {
                    out.push(HirReportColSpec::Relative {
                        offset,
                        align: hir_align,
                    });
                    if is_printable {
                        *first_on_line = false;
                    }
                }
            }
        }
    }
    out
}

/// Lowers `[NOT] PRESENT WHEN / AFTER [NEW]` or `ABSENT WHEN / AFTER [NEW]`.
fn lower_present_gate(
    gate: Option<&ReportPresentGateAst>,
    controls: &[HirReportControlSpec],
    ctrl_lookup: &BTreeMap<DataItemId, u16>,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirReportPresentKind {
    let Some(g) = gate else {
        return HirReportPresentKind::Always;
    };
    match g {
        ReportPresentGateAst::WhenCondition {
            polarity,
            condition,
            ..
        } => {
            let hc: HirCondition = lower_condition(condition, ctx);
            let base = HirReportPresentKind::Condition(Box::new(hc));
            match polarity {
                ReportPresentPolarity::Present => base,
                ReportPresentPolarity::Absent => HirReportPresentKind::Not(Box::new(base)),
            }
        }
        ReportPresentGateAst::AfterNew {
            polarity, triggers, ..
        } => {
            let mut idxs = Vec::with_capacity(triggers.len());
            for trig in triggers {
                match trig {
                    ReportAfterNewAtomAst::Page(_) => idxs.push(u16::MAX),
                    ReportAfterNewAtomAst::Final(sp) => {
                        if let Some(ci) = resolve_control_index(
                            &ReportControlAst::Final(*sp),
                            controls,
                            ctrl_lookup,
                            ctx,
                        ) {
                            idxs.push(ci);
                        }
                    }
                    ReportAfterNewAtomAst::Control(path, sp) => {
                        if let Some(ci) = resolve_control_index(
                            &ReportControlAst::Field(path.clone(), *sp),
                            controls,
                            ctrl_lookup,
                            ctx,
                        ) {
                            idxs.push(ci);
                        }
                    }
                }
            }
            let base = if let [single] = idxs.as_slice() {
                HirReportPresentKind::AfterNewControl {
                    control_idx: *single,
                }
            } else {
                HirReportPresentKind::AnyAfterNew(idxs)
            };
            match polarity {
                ReportPresentPolarity::Present => base,
                ReportPresentPolarity::Absent => HirReportPresentKind::Not(Box::new(base)),
            }
        }
    }
}

/// Lowers `OCCURS ... [STEP s] [VARYING idx]` on a report group/elementary item.
fn lower_report_occurs(
    rd_idx: usize,
    entry_idx: usize,
    entry: &ReportGroupEntry,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<HirReportOccursSpec> {
    let occ = entry.occurs.as_ref()?;
    if entry.columns.len() > 1 {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("`OCCURS` and multiple `COLUMN`s is not allowed"),
        ));
    }
    let max_count = match occ.base.max {
        OccursUpperBound::Bounded(n) => n.max(1),
        OccursUpperBound::Unbounded => occ.base.min.max(1),
    };
    let depending_on = occ.base.depending_on.as_ref().and_then(|path| {
        let dref = path_to_dataref(path, occ.base.span)?;
        let pl = resolve_place(&dref, ctx)?;
        Some(HirExpr::Place(pl))
    });
    let varying_place = occ.varying.and_then(|(v_sym, from_v, by_v, v_sp)| {
        let dref = path_to_dataref(&[v_sym], v_sp)?;
        let pl = resolve_place(&dref, ctx)?;
        Some((pl, from_v, by_v))
    });
    let occurs_byte_stride = ctx
        .layout
        .report_entry_items
        .get(&(rd_idx, entry_idx))
        .and_then(|id| ctx.layout.items.get(id.as_u32() as usize))
        .and_then(|it| it.dimensions.last())
        .map_or(0, |d| d.element_byte_stride);
    Some(HirReportOccursSpec {
        max_count,
        depending_on,
        step_cols: occ.step,
        varying_place,
        occurs_byte_stride,
    })
}

/// Resolves `TYPE IS ...` + optional control name into [`HirReportGroupRole`].
fn resolve_group_role(
    gt: ReportGroupType,
    entry: &ReportGroupEntry,
    controls: &[HirReportControlSpec],
    ctrl_lookup: &BTreeMap<DataItemId, u16>,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<HirReportGroupRole> {
    match gt {
        ReportGroupType::ReportHeading => Some(HirReportGroupRole::ReportHeading),
        ReportGroupType::PageHeading => Some(HirReportGroupRole::PageHeading),
        ReportGroupType::Detail => Some(HirReportGroupRole::Detail),
        ReportGroupType::PageFooting => Some(HirReportGroupRole::PageFooting),
        ReportGroupType::ReportFooting => Some(HirReportGroupRole::ReportFooting),
        ReportGroupType::ControlHeading => {
            let ci = match &entry.type_control {
                Some(ctrl) => resolve_control_index(ctrl, controls, ctrl_lookup, ctx).unwrap_or(0),
                None => 0,
            };
            Some(HirReportGroupRole::ControlHeading(ci))
        }
        ReportGroupType::ControlFooting => {
            let ci = match &entry.type_control {
                Some(ctrl) => resolve_control_index(ctrl, controls, ctrl_lookup, ctx).unwrap_or(0),
                None => 0,
            };
            Some(HirReportGroupRole::ControlFooting(ci))
        }
    }
}

/// Resolves a `ReportControlAst` (`FINAL` or field path) to its 0-based index in `controls`.
fn resolve_control_index(
    ctrl: &ReportControlAst,
    controls: &[HirReportControlSpec],
    ctrl_lookup: &BTreeMap<DataItemId, u16>,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<u16> {
    match ctrl {
        ReportControlAst::Final(sp) => {
            if let Some(pos) = controls
                .iter()
                .position(|c| matches!(c, HirReportControlSpec::Final))
            {
                u16::try_from(pos).ok()
            } else {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0007InvalidLayout,
                    Some(*sp),
                    String::from("`FINAL` is not declared in `RD ... CONTROLS`"),
                ));
                None
            }
        }
        ReportControlAst::Field(path, sp) => {
            let resolved = path.first().and_then(|&base| {
                ctx.symtab
                    .resolve_symbol_silent(ctx.scope, base, path.get(1..).unwrap_or(&[]))
            });
            if let Some(ResolvedSymbol::DataItem(id)) = resolved
                && let Some(&idx) = ctrl_lookup.get(&id)
            {
                return Some(idx);
            }
            let lbl = path
                .first()
                .and_then(|s| ctx.interner.resolve_original(*s).ok())
                .unwrap_or("control");
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(*sp),
                format!("`{lbl}` is not declared in `RD ... CONTROLS`"),
            ));
            None
        }
    }
}

/// Builds the output [`HirReportFormatterKind`] for an elementary report entry.
fn build_elem_formatter(
    entry: &ReportGroupEntry,
    item_ty: Option<&HirType>,
    sources: &[HirReportSourceKind],
    pic_ctx: PictureContext,
    dialect: &DialectConfig,
    interner: &SymbolInterner,
) -> HirReportFormatterKind {
    let space_byte = match dialect.internal_charset {
        InternalCharsetMode::Ascii => b' ',
        InternalCharsetMode::Ebcdic => 0x40,
    };
    if let Some((pic_sym, pic_sp)) = entry.picture {
        let mut eff_ty = if !entry.sums.is_empty() {
            let raw = interner.resolve_original(pic_sym).unwrap_or("9");
            let disp = UsageKind::Display;
            let mut sink_tmp = DiagnosticSink::new(WarningPolicy::default(), 0);
            compile_elementary_type(
                &ElementaryClauses {
                    raw_pic: Some((raw, pic_sp)),
                    entry_span: pic_sp,
                    pic_ctx,
                    usage: Some(&disp),
                    sign_clause: entry.sign,
                    justified: entry.justified,
                    blank_zero: entry.blank_when_zero,
                },
                dialect,
                &mut sink_tmp,
            )
        } else if let Some(ty) = item_ty {
            ty.clone()
        } else {
            return HirReportFormatterKind::RawBytes;
        };
        if entry.blank_when_zero == BlankWhenZeroMode::BlankWhenZero
            && let HirType::ZonedDecimal { blank_zero, .. } = &mut eff_ty
        {
            *blank_zero = coblang_hir::BlankZeroSpec::BlankWhenZero;
        }
        return match &eff_ty {
            HirType::Alphanumeric {
                byte_len,
                justified,
            } => HirReportFormatterKind::FixedAlpha {
                width: (*byte_len).max(1),
                justified: *justified,
                space_byte,
            },
            _ => HirReportFormatterKind::Picture {
                byte_len: eff_ty.static_byte_size().max(1),
                ty: eff_ty,
            },
        };
    }
    if let Some(HirReportSourceKind::LiteralBytes(bytes)) = sources.first() {
        return HirReportFormatterKind::FixedAlpha {
            width: u32::try_from(bytes.len()).unwrap_or(1).max(1),
            justified: match entry.justified {
                JustificationMode::StandardLeft => JustifiedSpec::Left,
                JustificationMode::JustifiedRight => JustifiedSpec::Right,
            },
            space_byte,
        };
    }
    HirReportFormatterKind::RawBytes
}

/// Lowers `INITIATE report-1 ...` into [`HirStmt::M18`], validating `RD` existence (`syn_reportwriter/0001`, `0005`).
pub(crate) fn lower_initiate_stmt(
    reports: &[SymbolId],
    span: SourceSpan,
    catalog: &UnitReportCatalog,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut resolved = Vec::with_capacity(reports.len());
    for &rep_sym in reports {
        if let Some((rid, orig, _)) = catalog.lookup_rd(rep_sym) {
            resolved.push((*rid, orig.clone()));
        } else {
            let lbl = ctx.interner.resolve_original(rep_sym).unwrap_or("report");
            if ctx
                .symtab
                .resolve_symbol_silent(ctx.scope, rep_sym, &[])
                .is_none()
                && catalog.seen_undef_reports.borrow_mut().insert(rep_sym)
            {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0004UndefinedSymbol,
                    Some(span),
                    format!("`{lbl}` is not defined"),
                ));
            }
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0004UndefinedSymbol,
                Some(span),
                format!("`{lbl}` in `INITIATE` is not a valid report name"),
            ));
        }
    }
    if !resolved.is_empty() {
        out.push(HirStmt::M18(Box::new(HirM18Stmt::Initiate {
            reports: resolved,
            site_label: String::new(),
            span,
        })));
    }
}

/// Lowers `GENERATE target` into [`HirStmt::M18`], validating `DETAIL` or summary `RD` (`syn_reportwriter/0005`, `0009`).
pub(crate) fn lower_generate_stmt(
    target: SymbolId,
    span: SourceSpan,
    catalog: &UnitReportCatalog,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    if let Some((rid, orig_rd, grp_up, gt)) = catalog.lookup_group(target).cloned() {
        if gt != ReportGroupType::Detail {
            let lbl = ctx.interner.resolve_original(target).unwrap_or("group");
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(span),
                format!("`GENERATE` target `{lbl}` must be a `TYPE DETAIL` group"),
            ));
            return;
        }
        out.push(HirStmt::M18(Box::new(HirM18Stmt::GenerateDetail {
            report_id: rid,
            report_name: orig_rd,
            group_name: grp_up,
            site_label: String::new(),
            span,
        })));
        return;
    }

    if let Some((rid, orig_rd, has_detail)) = catalog.lookup_rd(target).cloned() {
        if !has_detail {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(span),
                format!(
                    "summary `GENERATE {orig_rd}` requires at least one `TYPE DETAIL` group in `RD`"
                ),
            ));
            return;
        }
        out.push(HirStmt::M18(Box::new(HirM18Stmt::GenerateSummary {
            report_id: rid,
            report_name: orig_rd,
            site_label: String::new(),
            span,
        })));
        return;
    }

    let lbl = ctx.interner.resolve_original(target).unwrap_or("target");
    ctx.sink.emit(Diagnostic::error(
        DiagnosticCode::E0004UndefinedSymbol,
        Some(span),
        format!("`{lbl}` in `GENERATE` is neither a `DETAIL` report group nor an `RD` report"),
    ));
}

/// Lowers `TERMINATE report-1 ...` into [`HirStmt::M18`], validating `RD` existence (`syn_reportwriter/0001`, `0005`).
pub(crate) fn lower_terminate_stmt(
    reports: &[SymbolId],
    span: SourceSpan,
    catalog: &UnitReportCatalog,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut resolved = Vec::with_capacity(reports.len());
    for &rep_sym in reports {
        if let Some((rid, orig, _)) = catalog.lookup_rd(rep_sym) {
            resolved.push((*rid, orig.clone()));
        } else {
            let lbl = ctx.interner.resolve_original(rep_sym).unwrap_or("report");
            if ctx
                .symtab
                .resolve_symbol_silent(ctx.scope, rep_sym, &[])
                .is_none()
                && catalog.seen_undef_reports.borrow_mut().insert(rep_sym)
            {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0004UndefinedSymbol,
                    Some(span),
                    format!("`{lbl}` is not defined"),
                ));
            }
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0004UndefinedSymbol,
                Some(span),
                format!("`{lbl}` in `TERMINATE` is not a valid report name"),
            ));
        }
    }
    if !resolved.is_empty() {
        out.push(HirStmt::M18(Box::new(HirM18Stmt::Terminate {
            reports: resolved,
            site_label: String::new(),
            span,
        })));
    }
}

/// Lowers `SUPPRESS [PRINTING]`, enforcing placement inside `DECLARATIVES` (`syn_reportwriter/0010`).
pub(crate) fn lower_suppress_stmt(
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    if ctx.decl_region == DeclarativeRegion::NormalBody {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0003Syntax,
            Some(span),
            String::from("`SUPPRESS PRINTING` is only permitted inside `DECLARATIVES`"),
        ));
        return;
    }
    out.push(HirStmt::M18(Box::new(HirM18Stmt::SuppressPrinting {
        span,
    })));
}

// ============================================================================
// Internal Helpers
// ============================================================================

/// Extracts numeric hierarchy level (`1`..=`49`) from a `ReportGroupEntry`.
const fn entry_level_u8(entry: &ReportGroupEntry) -> u8 {
    match entry.level {
        LevelNumber::Hierarchy(n) => n,
        _ => 1,
    }
}

/// Finds the exclusive end index of `entries[idx]`'s subordinate subtree (`level > parent_lvl`).
fn find_subtree_end(entries: &[ReportGroupEntry], idx: usize, parent_lvl: u8) -> usize {
    let mut cur = idx + 1;
    while let Some(nxt) = entries.get(cur) {
        if entry_level_u8(nxt) <= parent_lvl {
            break;
        }
        cur += 1;
    }
    cur
}

/// Returns `true` if `entry` has printable content (`COLUMN`, `PIC`, `VALUE`, `SOURCE`, or `SUM`).
fn is_printable_leaf(entry: &ReportGroupEntry) -> bool {
    !entry.columns.is_empty()
        || entry.picture.is_some()
        || !entry.values.is_empty()
        || !entry.sources.is_empty()
        || !entry.sums.is_empty()
}

/// Converts a symbol path `[base, qual...]` into a plain [`coblang_syntax::ast::DataRef`].
fn path_to_dataref(path: &[SymbolId], span: SourceSpan) -> Option<coblang_syntax::ast::DataRef> {
    let (&base, rest) = path.split_first()?;
    Some(coblang_syntax::ast::DataRef {
        modifier: coblang_syntax::ast::DataRefModifier::Plain,
        name: coblang_syntax::ast::QualifiedName {
            base,
            qualifiers: rest.to_vec(),
            span,
        },
        subscripts: Vec::new(),
        refmod: None,
        span,
    })
}

/// Encodes UTF-8 string text into active internal character-set bytes (ASCII vs EBCDIC).
fn encode_text_bytes(s: &str, dialect: &DialectConfig) -> Vec<u8> {
    match dialect.internal_charset {
        InternalCharsetMode::Ascii => s.as_bytes().to_vec(),
        InternalCharsetMode::Ebcdic => s
            .bytes()
            .map(|b| {
                *dialect
                    .ebcdic_tables
                    .ascii_to_ebcdic
                    .get(usize::from(b))
                    .unwrap_or(&0x40)
            })
            .collect(),
    }
}

/// Encodes a `ValueAtom` into report literal bytes.
fn encode_value_atom_bytes(
    atom: &ValueAtom,
    target_width: u32,
    dialect: &DialectConfig,
    interner: &SymbolInterner,
) -> Vec<u8> {
    let w = (target_width as usize).max(1);
    match atom {
        ValueAtom::String { bytes, .. } => match dialect.internal_charset {
            InternalCharsetMode::Ascii => bytes.clone(),
            InternalCharsetMode::Ebcdic => bytes
                .iter()
                .map(|&b| {
                    *dialect
                        .ebcdic_tables
                        .ascii_to_ebcdic
                        .get(usize::from(b))
                        .unwrap_or(&0x40)
                })
                .collect(),
        },
        ValueAtom::Integer(sym) | ValueAtom::Float(sym) => {
            let s = interner.resolve_original(*sym).unwrap_or("0");
            encode_text_bytes(s, dialect)
        }
        ValueAtom::Decimal { digits, scale, .. } => {
            let raw = interner.resolve_original(*digits).unwrap_or("0");
            let sc = usize::try_from(scale.get().max(0)).unwrap_or(0);
            let formatted = if sc > 0 && raw.len() > sc {
                let split = raw.len() - sc;
                format!(
                    "{}.{}",
                    &raw.get(..split).unwrap_or("0"),
                    &raw.get(split..).unwrap_or("0")
                )
            } else {
                String::from(raw)
            };
            encode_text_bytes(&formatted, dialect)
        }
        ValueAtom::Figurative(FigurativeValue::All(pat)) => {
            let enc = match dialect.internal_charset {
                InternalCharsetMode::Ascii => pat.clone(),
                InternalCharsetMode::Ebcdic => pat
                    .iter()
                    .map(|&b| {
                        *dialect
                            .ebcdic_tables
                            .ascii_to_ebcdic
                            .get(usize::from(b))
                            .unwrap_or(&0x40)
                    })
                    .collect(),
            };
            if enc.is_empty() {
                vec![b' '; w]
            } else {
                enc.iter().copied().cycle().take(w).collect()
            }
        }
        ValueAtom::Figurative(fig) => {
            let fill = match (fig, dialect.internal_charset) {
                (FigurativeValue::Space, InternalCharsetMode::Ascii) => b' ',
                (FigurativeValue::Space, InternalCharsetMode::Ebcdic) => 0x40,
                (FigurativeValue::Zero, InternalCharsetMode::Ascii) => b'0',
                (FigurativeValue::Zero, InternalCharsetMode::Ebcdic) => 0xF0,
                (FigurativeValue::Quote, InternalCharsetMode::Ascii) => b'"',
                (FigurativeValue::Quote, InternalCharsetMode::Ebcdic) => 0x7F,
                (FigurativeValue::LowValue, _) => 0x00,
                (FigurativeValue::HighValue, _) => 0xFF,
                (FigurativeValue::Null | FigurativeValue::All(_), _) => 0x00,
            };
            vec![fill; w]
        }
        ValueAtom::Identifier(sym) => {
            let s = interner.resolve_original(*sym).unwrap_or("");
            encode_text_bytes(s, dialect)
        }
    }
}
