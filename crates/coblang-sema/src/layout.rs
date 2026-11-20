//! Data Division level-hierarchy normalization, byte-exact layout, `REDEFINES`, `66 RENAMES`, `OCCURS`/`ODO`, and `VALUE` validation.

use coblang_common::{
    ByteOffset, DataItemId, Dialect, FileConnectorId, LevelNumber, Result, ScaleFactor, ScopeId,
    SourceSpan, SymbolId,
};
use coblang_diag::{
    Diagnostic, DiagnosticCode, DiagnosticSink, DialectConfig, OdoComplexity, RedefinesChainPolicy,
    RelaxedSyntaxMode, SymbolInterner, WarningGroup,
};
use coblang_hir::{
    BinaryEndianness, DataCategory, DynamicOdoPresence, HirCondition88, HirConstValue, HirDataItem,
    HirDecimalSign, HirExternalCaseMode, HirFigurative, HirOccursDim, HirSortDirection,
    HirStorageSection, HirTableKey, HirType, Signedness, TruncationPolicy,
};
use coblang_syntax::ast::{
    DataDivision, DataEntry, DataEntryName, ExternalLinkage, FigurativeValue, GlobalVisibility,
    KeyOrder, OccursClause, OccursUpperBound, ResidencyKind, SyncAlignment, UsageKind, ValueAtom,
    ValueClause,
};

use crate::picture::{ElementaryClauses, PictureContext, compile_elementary_type};
use crate::symbols::{Condition88List, ResolvedSymbol, SymbolCandidate, SymbolTable};

/// Output of Data Division layout analysis for a single program/function/method scope.
#[derive(Debug, Default)]
pub struct SolvedDataLayout {
    /// All resolved `HirDataItem`s in declaration + synthesized-index order.
    pub items: Vec<HirDataItem>,
    /// All resolved level-`88` condition predicates.
    pub conditions_88: Condition88List,
    /// File connector record bindings `(file_name_sym, FileConnectorId, Vec<DataItemId>, max_bytes, span)`.
    pub fd_bindings: Vec<(SymbolId, FileConnectorId, Vec<DataItemId>, u32, SourceSpan)>,
    /// Preloaded `IS GLOBAL` [`coblang_hir::HirFileConnector`]s inherited from enclosing programs.
    pub inherited_files: Vec<coblang_hir::HirFileConnector>,
    /// Synthesized per-FD `(FileConnectorId, DataItemId)` `LINAGE-COUNTER` special registers.
    pub fd_linage_counters: Vec<(FileConnectorId, DataItemId)>,
    /// Synthesized per-RD `(page_counter, line_counter, print_switch)` `DataItemId` triples.
    pub rd_special_regs: Vec<(DataItemId, DataItemId, DataItemId)>,
    /// Mapped `(rd_idx, group_entry_idx) -> DataItemId` for `REPORT SECTION` entries.
    pub report_entry_items: std::collections::BTreeMap<(usize, usize), DataItemId>,
    /// Data items carrying `IS SPECIAL-NAMES {CRT STATUS | CURSOR}`.
    pub special_names_role_items: Vec<(coblang_syntax::ast::SpecialNamesDataRole, DataItemId)>,
    /// Deferred `(table_item_id, dim_idx, occurs_clause)` records resolved after all items exist.
    pending_occurs: Vec<(DataItemId, usize, OccursClause)>,
}

/// Active ancestor stack frame during hierarchical `01`..`49` layout traversal.
#[derive(Debug, Clone)]
struct AncestorFrame {
    /// Numeric hierarchy level (`1`..=`49`).
    level: u8,
    /// `DataItemId` of the group item.
    item_id: DataItemId,
    /// Optional group name symbol (for qualification).
    name: Option<SymbolId>,
    /// Optional explicit/inherited `USAGE` on the group.
    usage: Option<UsageKind>,
    /// Inherited `GLOBAL` visibility.
    visibility: GlobalVisibility,
    /// Whether this group or an ancestor carries `REDEFINES`.
    under_redefines: bool,
    /// Whether this group or an ancestor carries `OCCURS DEPENDING ON`.
    under_odo: bool,
    /// Accumulated table dimensions above children.
    dims: Vec<HirOccursDim>,
    /// Starting byte offset of this group within its `01` record.
    start_offset: u32,
    /// Current high-water byte offset inside this group.
    cursor_offset: u32,
    /// Whether a subordinate variably-sized ODO child has already closed inside this `01` record.
    seen_variable_child: bool,
}

/// Solves all `DATA DIVISION` sections into `HirDataItem`s and populates `symtab` for `scope`.
///
/// # Arguments
///
/// * `data` - Parsed [`DataDivision`] AST.
/// * `scope` - Owning lexical scope ID.
/// * `pic_ctx` - Active [`PictureContext`] from `SPECIAL-NAMES`.
/// * `dialect` - Active [`DialectConfig`].
/// * `symtab` - Shared [`SymbolTable`].
/// * `interner` - Shared [`SymbolInterner`].
/// * `sink` - Diagnostic sink.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if symbol interning fails.
pub fn solve_data_division(
    data: &DataDivision,
    scope: ScopeId,
    pic_ctx: PictureContext,
    dialect: &DialectConfig,
    symtab: &mut SymbolTable,
    interner: &mut SymbolInterner,
    sink: &mut DiagnosticSink,
) -> Result<SolvedDataLayout> {
    solve_data_division_with_preload(
        SolvedDataLayout::default(),
        data,
        scope,
        pic_ctx,
        dialect,
        symtab,
        interner,
        sink,
    )
}

/// Solves all `DATA DIVISION` sections starting from an optional `preloaded` inherited-`GLOBAL` layout.
///
/// # Arguments
///
/// * `out` - Preloaded [`SolvedDataLayout`] containing any inherited `GLOBAL` items.
/// * `data` - Parsed [`DataDivision`] AST.
/// * `scope` - Owning lexical scope ID.
/// * `pic_ctx` - Active [`PictureContext`] from `SPECIAL-NAMES`.
/// * `dialect` - Active [`DialectConfig`].
/// * `symtab` - Shared [`SymbolTable`].
/// * `interner` - Shared [`SymbolInterner`].
/// * `sink` - Diagnostic sink.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if symbol interning fails.
#[expect(
    clippy::too_many_arguments,
    reason = "Core Data Division layout entry point threading preloaded layout + compiler tables."
)]
pub fn solve_data_division_with_preload(
    mut out: SolvedDataLayout,
    data: &DataDivision,
    scope: ScopeId,
    pic_ctx: PictureContext,
    dialect: &DialectConfig,
    symtab: &mut SymbolTable,
    interner: &mut SymbolInterner,
    sink: &mut DiagnosticSink,
) -> Result<SolvedDataLayout> {
    symtab.inject_special_registers(scope, &mut out.items, interner)?;
    symtab.inject_custom_registers(scope, &dialect.custom_registers, &mut out.items, interner)?;
    let has_linage_fd = data.file_section.iter().any(|fd| fd.linage.is_some());
    let lc_sym = interner.intern("LINAGE-COUNTER")?;
    if has_linage_fd && let Some(sc) = symtab.scope_mut(scope) {
        let _ = sc.by_name.remove(&lc_sym);
    }

    for fd in &data.file_section {
        let fc_id = FileConnectorId::new(u32::try_from(out.fd_bindings.len()).unwrap_or(0));
        symtab.insert_symbol(
            scope,
            fd.file_name,
            SymbolCandidate {
                target: ResolvedSymbol::File(fc_id),
                qualifiers: Vec::new(),
                visibility: fd.global,
            },
        );
        if fd.linage.is_some() {
            register_fd_linage_counter(fd, fc_id, lc_sym, scope, &mut out, symtab);
        }
        let start_item_len = out.items.len();
        let mut ctx = SectionLayoutCtx {
            scope,
            section: HirStorageSection::FileSection(fc_id),
            file_qual: Some(fd.file_name),
            inherited_global: fd.global,
            pic_ctx,
            dialect,
            symtab,
            interner,
            sink,
        };
        solve_entry_slice(&fd.records, &mut out, &mut ctx);
        let mut rec_ids = Vec::new();
        let mut max_rec = 0_u32;
        for item in out.items.get(start_item_len..).unwrap_or(&[]) {
            if item.level == LevelNumber::Hierarchy(1) {
                rec_ids.push(item.id);
                max_rec = max_rec.max(item.total_byte_size);
            }
        }
        if let Some(&first_rec_id) = rec_ids.first() {
            for &rid in rec_ids.iter().skip(1) {
                if let Some(rit) = out.items.get_mut(rid.as_u32() as usize) {
                    rit.redefines = Some(first_rec_id);
                }
            }
        }
        out.fd_bindings
            .push((fd.file_name, fc_id, rec_ids, max_rec, fd.span));
    }

    for (entries, sec) in [
        (&data.working_storage, HirStorageSection::WorkingStorage),
        (&data.local_storage, HirStorageSection::LocalStorage),
        (&data.linkage, HirStorageSection::Linkage),
    ] {
        let mut ctx = SectionLayoutCtx {
            scope,
            section: sec,
            file_qual: None,
            inherited_global: GlobalVisibility::Local,
            pic_ctx,
            dialect,
            symtab,
            interner,
            sink,
        };
        solve_entry_slice(entries, &mut out, &mut ctx);
    }

    crate::m18_report::solve_report_section_storage(
        &data.report_section,
        scope,
        pic_ctx,
        dialect,
        &mut out,
        symtab,
        interner,
        sink,
    )?;

    resolve_odo_targets(data, scope, &mut out, dialect, symtab, interner, sink);
    Ok(out)
}

/// Registers a per-`FD` `LINAGE-COUNTER OF <file>` 32-bit special register in `out` and `symtab`.
///
/// # Arguments
///
/// * `fd` - Source `FD` AST entry.
/// * `fc_id` - Assigned [`FileConnectorId`].
/// * `lc_sym` - Interned `"LINAGE-COUNTER"` symbol.
/// * `scope` - Unit [`ScopeId`].
/// * `out` - Mutable [`SolvedDataLayout`].
/// * `symtab` - Mutable [`SymbolTable`].
fn register_fd_linage_counter(
    fd: &coblang_syntax::ast::FdSdEntry,
    fc_id: FileConnectorId,
    lc_sym: SymbolId,
    scope: ScopeId,
    out: &mut SolvedDataLayout,
    symtab: &mut SymbolTable,
) {
    let lc_id = DataItemId::new(u32::try_from(out.items.len()).unwrap_or(0));
    out.items.push(HirDataItem {
        id: lc_id,
        scope,
        name: Some(lc_sym),
        level: LevelNumber::Independent77,
        parent: None,
        children: Vec::new(),
        section: HirStorageSection::SpecialRegister,
        ty: HirType::BinaryInt {
            byte_width: 4,
            digits: coblang_common::DigitCount::try_new(9)
                .unwrap_or(coblang_common::DigitCount::ONE),
            scale: ScaleFactor::ZERO,
            sign: Signedness::Signed,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
        },
        byte_offset: ByteOffset::ZERO,
        total_byte_size: 4,
        dimensions: Vec::new(),
        redefines: None,
        initial_value: None,
        span: fd.span,
    });
    symtab.insert_symbol(
        scope,
        lc_sym,
        SymbolCandidate {
            target: ResolvedSymbol::DataItem(lc_id),
            qualifiers: vec![fd.file_name],
            visibility: fd.global,
        },
    );
    out.fd_linage_counters.push((fc_id, lc_id));
}

/// Bundled context for laying out one storage section's `DataEntry` slice.
pub(crate) struct SectionLayoutCtx<'a> {
    /// Owning scope ID.
    pub(crate) scope: ScopeId,
    /// Active storage section.
    pub(crate) section: HirStorageSection,
    /// Optional enclosing `FD`/`SD` or `RD` qualifier.
    pub(crate) file_qual: Option<SymbolId>,
    /// Inherited `GLOBAL` visibility from `FD` / `RD`.
    pub(crate) inherited_global: GlobalVisibility,
    /// `DECIMAL-POINT` & `CURRENCY` context.
    pub(crate) pic_ctx: PictureContext,
    /// Active dialect configuration.
    pub(crate) dialect: &'a DialectConfig,
    /// Symbol table.
    pub(crate) symtab: &'a mut SymbolTable,
    /// Symbol interner.
    pub(crate) interner: &'a mut SymbolInterner,
    /// Diagnostic sink.
    pub(crate) sink: &'a mut DiagnosticSink,
}

/// Lays out a contiguous slice of `DataEntry` declarations within a single section.
///
/// # Arguments
///
/// * `entries` - Slice of AST entries.
/// * `out` - Output [`SolvedDataLayout`].
/// * `ctx` - Section layout context.
fn solve_entry_slice(
    entries: &[DataEntry],
    out: &mut SolvedDataLayout,
    ctx: &mut SectionLayoutCtx<'_>,
) {
    let _ = solve_entry_slice_with_ids(entries, out, ctx);
}

/// Lays out `entries` and returns the allocated `Option<DataItemId>` for each entry index.
///
/// # Arguments
///
/// * `entries` - Slice of AST entries.
/// * `out` - Output [`SolvedDataLayout`].
/// * `ctx` - Section layout context.
pub(crate) fn solve_entry_slice_with_ids(
    entries: &[DataEntry],
    out: &mut SolvedDataLayout,
    ctx: &mut SectionLayoutCtx<'_>,
) -> Vec<Option<DataItemId>> {
    let mut stack: Vec<AncestorFrame> = Vec::new();
    let mut current_01_root: Option<DataItemId> = None;
    let mut last_conditional_item: Option<(DataItemId, GlobalVisibility)> = None;
    let mut mapped_ids = Vec::with_capacity(entries.len());

    for (idx, entry) in entries.iter().enumerate() {
        check_duplicate_clauses(entry, ctx.sink);

        match entry.level {
            LevelNumber::Constant78 => {
                close_stack_to_level(&mut stack, 1, &mut out.items);
                handle_constant_78(entry, ctx);
                last_conditional_item = None;
                mapped_ids.push(None);
            }
            LevelNumber::Condition88 => {
                handle_condition_88(entry, last_conditional_item, &stack, out, ctx);
                mapped_ids.push(None);
            }
            LevelNumber::Renames66 => {
                close_stack_to_level(&mut stack, 2, &mut out.items);
                handle_renames_66(entry, current_01_root, out, ctx);
                last_conditional_item = None;
                mapped_ids.push(None);
            }
            LevelNumber::Independent77 => {
                close_stack_to_level(&mut stack, 1, &mut out.items);
                current_01_root = None;
                if next_non_88_is_subordinate(entries, idx) {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(entry.span),
                        String::from(
                            "level `77` item must be elementary and cannot have subordinate items",
                        ),
                    ));
                }
                let (id, vis) = instantiate_hierarchy_or_77(entry, 77, false, &mut stack, out, ctx);
                if let Some(sn_role) = entry.flags.special_names_role {
                    out.special_names_role_items.push((sn_role, id));
                }
                last_conditional_item = Some((id, vis));
                mapped_ids.push(Some(id));
            }
            LevelNumber::Hierarchy(lvl) => {
                if entry.flags.residency == ResidencyKind::CompileTimeConstant {
                    close_stack_to_level(&mut stack, 1, &mut out.items);
                    handle_constant_78(entry, ctx);
                    last_conditional_item = None;
                    mapped_ids.push(None);
                    continue;
                }
                close_stack_to_level(&mut stack, lvl, &mut out.items);
                if lvl > 1
                    && stack.is_empty()
                    && ctx.dialect.relaxed_syntax == RelaxedSyntaxMode::Strict
                {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0007InvalidLayout,
                        Some(entry.span),
                        format!("level `{lvl:02}` entry has no enclosing `01` record"),
                    ));
                }
                let is_group = has_subordinate_hierarchy_child(entries, idx, lvl);
                let (id, vis) =
                    instantiate_hierarchy_or_77(entry, lvl, is_group, &mut stack, out, ctx);
                if let Some(sn_role) = entry.flags.special_names_role {
                    out.special_names_role_items.push((sn_role, id));
                }
                if lvl == 1 {
                    current_01_root = Some(id);
                }
                last_conditional_item = Some((id, vis));
                mapped_ids.push(Some(id));
            }
        }
    }

    close_stack_to_level(&mut stack, 1, &mut out.items);
    mapped_ids
}

/// Checks `entry.clause_counts` for duplicate `PIC`, `USAGE`, `OCCURS`, `VALUE`, or `REDEFINES` clauses.
///
/// # Arguments
///
/// * `entry` - Parsed data entry.
/// * `sink` - Diagnostic sink.
fn check_duplicate_clauses(entry: &DataEntry, sink: &mut DiagnosticSink) {
    let counts = entry.clause_counts;
    let pic_sp = entry.picture.map_or(entry.span, |(_, sp)| sp);
    for (cnt, name, sp) in [
        (counts.pic_count, "PICTURE", pic_sp),
        (counts.usage_count, "USAGE", entry.span),
        (counts.occurs_count, "OCCURS", entry.span),
        (counts.value_count, "VALUE", entry.span),
        (counts.redefines_count, "REDEFINES", entry.span),
    ] {
        if cnt > 1 {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(sp),
                format!("duplicate `{name}` clause in data description entry"),
            ));
        }
    }
}

/// Instantiates a `01`..`49` or `77` [`HirDataItem`], computing offsets, `REDEFINES`, `OCCURS`, and `VALUE` checks.
///
/// # Arguments
///
/// * `entry` - AST data entry.
/// * `lvl` - Numeric level (`1`..=`49` or `77`).
/// * `is_group` - `true` when followed by subordinate hierarchy children.
/// * `stack` - Open ancestor group stack.
/// * `out` - Output [`SolvedDataLayout`].
/// * `ctx` - Section layout context.
fn instantiate_hierarchy_or_77(
    entry: &DataEntry,
    lvl: u8,
    is_group: bool,
    stack: &mut Vec<AncestorFrame>,
    out: &mut SolvedDataLayout,
    ctx: &mut SectionLayoutCtx<'_>,
) -> (DataItemId, GlobalVisibility) {
    let parent_frame = stack.last().cloned();
    let inherited_usage = entry
        .usage
        .clone()
        .or_else(|| parent_frame.as_ref().and_then(|f| f.usage.clone()));
    let visibility = if entry.flags.global == GlobalVisibility::Global
        || ctx.inherited_global == GlobalVisibility::Global
    {
        GlobalVisibility::Global
    } else {
        parent_frame
            .as_ref()
            .map_or(GlobalVisibility::Local, |f| f.visibility)
    };

    if let Some(pf) = &parent_frame
        && pf.seen_variable_child
        && entry.redefines.is_none()
        && ctx.dialect.complex_odo == OdoComplexity::SimpleIsoOnly
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("variably-located item after `OCCURS DEPENDING ON` is not permitted in active dialect"),
        ));
    }

    let (redef_target, base_offset) =
        resolve_redefines_and_offset(entry, lvl, parent_frame.as_ref(), &out.items, ctx);
    let under_redef =
        redef_target.is_some() || parent_frame.as_ref().is_some_and(|f| f.under_redefines);
    let has_own_odo = entry
        .occurs
        .as_ref()
        .is_some_and(|o| o.depending_on.is_some());
    let under_odo = has_own_odo || parent_frame.as_ref().is_some_and(|f| f.under_odo);

    validate_occurs_rules(entry, lvl, parent_frame.as_ref(), ctx);
    let ty = build_entry_hir_type(entry, is_group, has_own_odo, inherited_usage.as_ref(), ctx);
    let aligned_offset = apply_sync_alignment(base_offset, entry.sync, &ty);
    let (dims, occurs_mult) = compute_entry_dims(
        entry,
        ty.static_byte_size(),
        parent_frame.as_ref(),
        visibility,
        out,
        ctx,
    );

    let qualifiers = build_qualifiers(stack, ctx.file_qual);
    let item_id = record_data_item(
        entry,
        (parent_frame.as_ref().map(|f| f.item_id), visibility),
        &ty,
        (aligned_offset, occurs_mult, redef_target),
        (dims.clone(), qualifiers),
        out,
        ctx,
    );

    if let Some(val_clause) = &entry.value {
        validate_value_clause(val_clause, &ty, is_group, under_redef, under_odo, ctx);
    }

    if is_group {
        stack.push(AncestorFrame {
            level: lvl,
            item_id,
            name: match entry.name {
                DataEntryName::Named(s) => Some(s),
                DataEntryName::Filler => None,
            },
            usage: inherited_usage,
            visibility,
            under_redefines: under_redef,
            under_odo,
            dims,
            start_offset: aligned_offset,
            cursor_offset: aligned_offset,
            seen_variable_child: has_own_odo,
        });
    } else {
        maybe_upgrade_ibm_comp_redefined_by_pointer(
            redef_target,
            &ty,
            &mut out.items,
            ctx.dialect.base_dialect,
        );
        let total_sz = ty.static_byte_size().saturating_mul(occurs_mult);
        check_redefines_size(redef_target, lvl, total_sz, entry.span, &out.items, ctx);
        if let Some(top) = stack.last_mut() {
            let end_off = aligned_offset.saturating_add(total_sz);
            if redef_target.is_none() || end_off > top.cursor_offset {
                top.cursor_offset = top.cursor_offset.max(end_off);
            }
            if has_own_odo {
                top.seen_variable_child = true;
            }
        }
    }

    (item_id, visibility)
}

/// Promotes a 4-byte `BINARY`/`COMP` item to 8-byte native full-range when redefined by `POINTER` under `-dialect ibm`.
///
/// # Arguments
///
/// * `redef_target` - Optional redefined [`DataItemId`].
/// * `redefining_ty` - Type of the redefining item.
/// * `items` - Solved [`HirDataItem`] table.
/// * `dialect` - Active base [`Dialect`].
fn maybe_upgrade_ibm_comp_redefined_by_pointer(
    redef_target: Option<DataItemId>,
    redefining_ty: &HirType,
    items: &mut [HirDataItem],
    dialect: Dialect,
) {
    if dialect != Dialect::Ibm
        || !matches!(redefining_ty, HirType::Pointer | HirType::ProgramPointer)
    {
        return;
    }
    if let Some(target) = redef_target.and_then(|tid| items.get_mut(tid.as_u32() as usize))
        && target.dimensions.is_empty()
        && let HirType::BinaryInt {
            byte_width: 4,
            digits,
            scale,
            sign,
            ..
        } = target.ty
    {
        target.ty = HirType::BinaryInt {
            byte_width: 8,
            digits,
            scale,
            sign,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
        };
        target.total_byte_size = 8;
    }
}

/// Computes the initial [`HirType`] for a group or elementary [`DataEntry`].
///
/// # Arguments
///
/// * `entry` - AST entry.
/// * `is_group` - Whether `entry` has subordinate children.
/// * `has_own_odo` - Whether `entry` carries `OCCURS DEPENDING ON`.
/// * `inherited_usage` - Effective `USAGE` clause.
/// * `ctx` - Layout context.
fn build_entry_hir_type(
    entry: &DataEntry,
    is_group: bool,
    has_own_odo: bool,
    inherited_usage: Option<&UsageKind>,
    ctx: &mut SectionLayoutCtx<'_>,
) -> HirType {
    if is_group {
        if let Some((_, pic_sp)) = entry.picture {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(pic_sp),
                String::from("group item cannot have a `PICTURE` clause"),
            ));
        }
        HirType::Group {
            static_byte_len: 0,
            odo_presence: if has_own_odo {
                DynamicOdoPresence::HasDynamicOdo
            } else {
                DynamicOdoPresence::StaticSize
            },
        }
    } else {
        let raw_pic_owned = entry.picture.map(|(sym, sp)| {
            (
                ctx.interner.resolve_original(sym).unwrap_or("").to_owned(),
                sp,
            )
        });
        let raw_pic = raw_pic_owned.as_ref().map(|(s, sp)| (s.as_str(), *sp));
        let clauses = ElementaryClauses {
            raw_pic,
            entry_span: entry.span,
            pic_ctx: ctx.pic_ctx,
            usage: inherited_usage,
            sign_clause: entry.sign,
            justified: entry.justified,
            blank_zero: entry.blank_when_zero,
        };
        compile_elementary_type(&clauses, ctx.dialect, ctx.sink)
    }
}

/// Computes inherited + own `HirOccursDim` vector and static multiplier for `entry`.
///
/// # Arguments
///
/// * `entry` - AST entry.
/// * `elem_size` - Single-element byte size.
/// * `parent_frame` - Enclosing group frame, if any.
/// * `visibility` - Effective `GLOBAL` visibility.
/// * `out` - Output [`SolvedDataLayout`].
/// * `ctx` - Layout context.
fn compute_entry_dims(
    entry: &DataEntry,
    elem_size: u32,
    parent_frame: Option<&AncestorFrame>,
    visibility: GlobalVisibility,
    out: &mut SolvedDataLayout,
    ctx: &mut SectionLayoutCtx<'_>,
) -> (Vec<HirOccursDim>, u32) {
    let mut dims = parent_frame.map_or_else(Vec::new, |f| f.dims.clone());
    let mult = if let Some(occ) = &entry.occurs {
        let max_val = match occ.max {
            OccursUpperBound::Bounded(m) => m,
            OccursUpperBound::Unbounded => 1,
        };
        let indexed_by = register_indexed_by(&occ.indexed_by, occ.span, visibility, out, ctx);
        dims.push(HirOccursDim {
            min: occ.min,
            max: match occ.max {
                OccursUpperBound::Bounded(m) => m,
                OccursUpperBound::Unbounded => u32::MAX,
            },
            element_byte_stride: elem_size,
            depending_on: None,
            keys: Vec::new(),
            indexed_by,
        });
        max_val.max(1)
    } else {
        1
    };
    (dims, mult)
}

/// Appends a newly solved [`HirDataItem`] to `out.items` and registers its symbol in `ctx.symtab`.
///
/// # Arguments
///
/// * `entry` - AST entry.
/// * `scope_link` - `(parent_id, visibility)` pair.
/// * `ty` - Solved [`HirType`].
/// * `placement` - `(aligned_offset, occurs_mult, redef_target)`.
/// * `dims_and_quals` - `(dimensions, qualifiers)` pair.
/// * `out` - Output [`SolvedDataLayout`].
/// * `ctx` - Layout context.
fn record_data_item(
    entry: &DataEntry,
    scope_link: (Option<DataItemId>, GlobalVisibility),
    ty: &HirType,
    placement: (u32, u32, Option<DataItemId>),
    dims_and_quals: (Vec<HirOccursDim>, Vec<SymbolId>),
    out: &mut SolvedDataLayout,
    ctx: &mut SectionLayoutCtx<'_>,
) -> DataItemId {
    let (parent_id, visibility) = scope_link;
    let (aligned_offset, occurs_mult, redef_target) = placement;
    let (dims, qualifiers) = dims_and_quals;
    let item_id = DataItemId::new(u32::try_from(out.items.len()).unwrap_or(0));
    if let Some(occ) = &entry.occurs {
        let dim_idx = dims.len().saturating_sub(1);
        out.pending_occurs.push((item_id, dim_idx, occ.clone()));
    }
    let item_name = match entry.name {
        DataEntryName::Named(sym) => Some(sym),
        DataEntryName::Filler => None,
    };
    if let Some(sym) = item_name {
        ctx.symtab.insert_symbol(
            ctx.scope,
            sym,
            SymbolCandidate {
                target: ResolvedSymbol::DataItem(item_id),
                qualifiers,
                visibility,
            },
        );
    }
    if let Some(p_item) = parent_id.and_then(|pid| out.items.get_mut(pid.as_u32() as usize)) {
        p_item.children.push(item_id);
    }
    let initial_value = entry
        .value
        .as_ref()
        .and_then(|vc| vc.values.first())
        .and_then(|vr| {
            crate::m17_charset::atom_to_hir_const_m17(&vr.low, ctx.scope, ctx.symtab, ctx.dialect)
        });
    let parent_sec = parent_id
        .and_then(|pid| out.items.get(pid.as_u32() as usize))
        .map(|p| p.section);
    let lvl = entry.level.as_u8();
    if entry.flags.residency == ResidencyKind::Based && lvl != 1 && lvl != 77 {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("BASED only allowed at 01/77 level"),
        ));
    }
    let eff_section = if entry.flags.residency == ResidencyKind::Based
        || parent_sec == Some(HirStorageSection::BasedStorage)
    {
        HirStorageSection::BasedStorage
    } else if let ExternalLinkage::External(opt_alias) = entry.flags.external {
        if lvl != 1 && lvl != 77 {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(entry.span),
                String::from("EXTERNAL only allowed at 01/77 level"),
            ));
        }
        resolve_external_section(opt_alias, item_name, entry.span, ctx)
    } else if let Some(HirStorageSection::ExternalWorkingStorage {
        external_name,
        case_mode,
    }) = parent_sec
    {
        HirStorageSection::ExternalWorkingStorage {
            external_name,
            case_mode,
        }
    } else {
        ctx.section
    };
    out.items.push(HirDataItem {
        id: item_id,
        scope: ctx.scope,
        name: item_name,
        level: entry.level,
        parent: parent_id,
        children: Vec::new(),
        section: eff_section,
        ty: ty.clone(),
        byte_offset: ByteOffset::new(aligned_offset),
        total_byte_size: ty.static_byte_size().saturating_mul(occurs_mult),
        dimensions: dims,
        redefines: redef_target,
        initial_value,
        span: entry.span,
    });
    item_id
}

/// Resolves an `IS EXTERNAL [AS "alias"]` clause into [`HirStorageSection::ExternalWorkingStorage`],
/// diagnosing unnamed `FILLER` entries lacking an `AS` alias.
///
/// # Arguments
///
/// * `opt_alias` - Optional `AS "literal"` symbol.
/// * `item_name` - Declared item identifier (`None` for `FILLER`).
/// * `span` - Declaration span.
/// * `ctx` - Layout context.
fn resolve_external_section(
    opt_alias: Option<SymbolId>,
    item_name: Option<SymbolId>,
    span: SourceSpan,
    ctx: &mut SectionLayoutCtx<'_>,
) -> HirStorageSection {
    if let Some(alias_sym) = opt_alias {
        HirStorageSection::ExternalWorkingStorage {
            external_name: alias_sym,
            case_mode: HirExternalCaseMode::ExactAlias,
        }
    } else if let Some(name_sym) = item_name {
        HirStorageSection::ExternalWorkingStorage {
            external_name: name_sym,
            case_mode: HirExternalCaseMode::CanonicalUpper,
        }
    } else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(span),
            String::from("EXTERNAL FILLER item requires an explicit AS literal"),
        ));
        ctx.section
    }
}

/// Closes any open group frames on `stack` whose level is `>= target_level`, finalizing group sizes and strides.
///
/// # Arguments
///
/// * `stack` - Open ancestor stack.
/// * `target_level` - Incoming entry level (`1` closes everything).
/// * `items` - Program `HirDataItem` arena.
fn close_stack_to_level(
    stack: &mut Vec<AncestorFrame>,
    target_level: u8,
    items: &mut [HirDataItem],
) {
    while let Some(closed) = stack.pop_if(|f| f.level >= target_level) {
        let elem_len = closed
            .cursor_offset
            .saturating_sub(closed.start_offset)
            .max(1);
        let parent_dim_len = stack.last().map_or(0, |p| p.dims.len());
        let closed_idx = closed.item_id.as_u32() as usize;
        let mut group_had_own_occurs = false;
        items.get_mut(closed_idx).into_iter().for_each(|grp_item| {
            let mut mult = 1_u32;
            if grp_item.dimensions.len() > parent_dim_len {
                group_had_own_occurs = true;
                grp_item
                    .dimensions
                    .last_mut()
                    .into_iter()
                    .for_each(|own_dim| {
                        own_dim.element_byte_stride = elem_len;
                        if own_dim.max != u32::MAX {
                            mult = own_dim.max.max(1);
                        }
                    });
            }
            let total = elem_len.saturating_mul(mult);
            grp_item.ty = HirType::Group {
                static_byte_len: elem_len,
                odo_presence: if closed.seen_variable_child {
                    DynamicOdoPresence::HasDynamicOdo
                } else {
                    DynamicOdoPresence::StaticSize
                },
            };
            grp_item.total_byte_size = total;
            if let Some(parent_top) = stack.last_mut() {
                let end_off = closed.start_offset.saturating_add(total);
                if grp_item.redefines.is_none() || end_off > parent_top.cursor_offset {
                    parent_top.cursor_offset = parent_top.cursor_offset.max(end_off);
                }
                if closed.seen_variable_child {
                    parent_top.seen_variable_child = true;
                }
            }
        });
        if group_had_own_occurs {
            for desc in items.get_mut(closed_idx + 1..).unwrap_or(&mut []) {
                if let Some(inherited_dim) = desc.dimensions.get_mut(parent_dim_len) {
                    inherited_dim.element_byte_stride = elem_len;
                }
            }
        }
    }
}

/// Validates and resolves `REDEFINES target` to `(Option<DataItemId>, starting_byte_offset)`.
///
/// # Arguments
///
/// * `entry` - AST data entry.
/// * `lvl` - Numeric hierarchy level.
/// * `parent_frame` - Enclosing group frame.
/// * `items` - Solved items slice.
/// * `ctx` - Layout context.
fn resolve_redefines_and_offset(
    entry: &DataEntry,
    lvl: u8,
    parent_frame: Option<&AncestorFrame>,
    items: &[HirDataItem],
    ctx: &mut SectionLayoutCtx<'_>,
) -> (Option<DataItemId>, u32) {
    let default_off = parent_frame.map_or(0, |f| f.cursor_offset);
    let Some(target_sym) = entry.redefines else {
        return (None, default_off);
    };
    if matches!(ctx.section, HirStorageSection::FileSection(_)) && lvl == 1 {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("`REDEFINES` is not permitted at `01` level in `FILE SECTION`"),
        ));
    }

    let parent_id = parent_frame.map(|f| f.item_id);
    let expected_sec = parent_id
        .and_then(|pid| items.get(pid.as_u32() as usize))
        .map_or(ctx.section, |p| p.section);
    let sibling_opt = items.iter().rev().find(|it| {
        it.parent == parent_id && it.section == expected_sec && it.name == Some(target_sym)
    });

    let Some(target_item) = sibling_opt else {
        let label = ctx
            .interner
            .resolve_original(target_sym)
            .unwrap_or("target");
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            format!("`REDEFINES` target `{label}` not found at same level in current group"),
        ));
        return (None, default_off);
    };

    if target_item.level != entry.level {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("`REDEFINES` item and target must have identical level numbers"),
        ));
    }
    let own_parent_dims = parent_frame.map_or(0, |f| f.dims.len());
    if target_item.dimensions.len() > own_parent_dims {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("cannot `REDEFINES` an item with an `OCCURS` clause"),
        ));
    }
    if target_item.redefines.is_some()
        && ctx.dialect.indirect_redefines == RedefinesChainPolicy::DirectOnly
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from(
                "indirect `REDEFINES` of a redefining item is not permitted in active dialect",
            ),
        ));
    }

    let root_target = target_item.redefines.unwrap_or(target_item.id);
    (Some(root_target), target_item.byte_offset.as_u32())
}

/// Checks redefining item size against the redefined target size.
///
/// # Arguments
///
/// * `redef_target` - Optional target `DataItemId`.
/// * `lvl` - Numeric hierarchy level.
/// * `own_size` - Total size of redefining item.
/// * `span` - Entry span.
/// * `items` - Solved items slice.
/// * `ctx` - Layout context.
fn check_redefines_size(
    redef_target: Option<DataItemId>,
    lvl: u8,
    own_size: u32,
    span: SourceSpan,
    items: &[HirDataItem],
    ctx: &mut SectionLayoutCtx<'_>,
) {
    let Some(target) = redef_target.and_then(|tid| items.get(tid.as_u32() as usize)) else {
        return;
    };
    if own_size > target.total_byte_size {
        if lvl > 1 && ctx.dialect.base_dialect == Dialect::Cobol85 {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(span),
                format!(
                    "redefining item size ({own_size}) exceeds redefined item size ({})",
                    target.total_byte_size
                ),
            ));
        } else {
            ctx.sink.emit(Diagnostic::warning(
                DiagnosticCode::W0003Redefinition,
                WarningGroup::Redefinition,
                Some(span),
                format!(
                    "size of redefining item ({own_size}) is larger than redefined item ({})",
                    target.total_byte_size
                ),
            ));
        }
    }
}

/// Validates `OCCURS` placement (`01`/`77`/`66`/`88`, `0` count, `min > max`, `UNBOUNDED`, nested `ODO`).
///
/// # Arguments
///
/// * `entry` - AST data entry.
/// * `lvl` - Numeric level.
/// * `parent_frame` - Enclosing group frame.
/// * `ctx` - Layout context.
fn validate_occurs_rules(
    entry: &DataEntry,
    lvl: u8,
    parent_frame: Option<&AncestorFrame>,
    ctx: &mut SectionLayoutCtx<'_>,
) {
    let Some(occ) = &entry.occurs else {
        return;
    };
    if matches!(lvl, 1 | 77)
        && (lvl == 77
            || matches!(
                ctx.dialect.base_dialect,
                Dialect::Cobol85 | Dialect::Cobol2002 | Dialect::Cobol2014 | Dialect::Cobol2023
            ))
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(occ.span),
            format!("`OCCURS` clause is not permitted at level `{lvl:02}` in active dialect"),
        ));
    }

    match occ.max {
        OccursUpperBound::Bounded(max_n) => {
            if max_n == 0 || occ.min > max_n {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0007InvalidLayout,
                    Some(occ.span),
                    format!("invalid `OCCURS` bounds ({} TO {max_n})", occ.min),
                ));
            }
        }
        OccursUpperBound::Unbounded => {
            if occ.depending_on.is_none() || ctx.section != HirStorageSection::Linkage {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0007InvalidLayout,
                    Some(occ.span),
                    String::from(
                        "`OCCURS UNBOUNDED` requires `DEPENDING ON` inside `LINKAGE SECTION`",
                    ),
                ));
            }
            if matches!(
                ctx.dialect.base_dialect,
                Dialect::Cobol85 | Dialect::Cobol2002 | Dialect::Cobol2014
            ) {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0009DialectUnconformable,
                    Some(occ.span),
                    String::from("`OCCURS UNBOUNDED` does not conform to active dialect"),
                ));
            }
        }
    }

    if occ.depending_on.is_some()
        && parent_frame.is_some_and(|f| f.under_odo)
        && ctx.dialect.complex_odo == OdoComplexity::SimpleIsoOnly
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(occ.span),
            String::from("nested `OCCURS DEPENDING ON` is not permitted in active dialect"),
        ));
    }
}

/// Validates `66 name RENAMES from [THRU thru]` contiguous span rules.
///
/// # Arguments
///
/// * `entry` - AST `66` entry.
/// * `root_01` - Enclosing `01` record `DataItemId`.
/// * `out` - Output [`SolvedDataLayout`].
/// * `ctx` - Layout context.
fn handle_renames_66(
    entry: &DataEntry,
    root_01: Option<DataItemId>,
    out: &mut SolvedDataLayout,
    ctx: &mut SectionLayoutCtx<'_>,
) {
    if entry.picture.is_some()
        || entry.usage.is_some()
        || entry.occurs.is_some()
        || entry.value.is_some()
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from(
                "level `66 RENAMES` cannot specify `PICTURE`, `USAGE`, `OCCURS`, or `VALUE`",
            ),
        ));
    }
    let DataEntryName::Named(sym) = entry.name else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("level `66` entry requires a data-name before `RENAMES`"),
        ));
        return;
    };
    let Some(ren) = &entry.renames else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("level `66` entry requires a `RENAMES` clause"),
        ));
        return;
    };
    let Some((&from_base, from_quals)) = ren.from.split_first() else {
        return;
    };
    let from_resolved = ctx.symtab.resolve_symbol(
        ctx.scope,
        from_base,
        from_quals,
        ren.span,
        ctx.interner,
        ctx.sink,
    );
    let Some(from_item) = from_resolved.and_then(|res| match res {
        ResolvedSymbol::DataItem(from_id) => out.items.get(from_id.as_u32() as usize).cloned(),
        _ => None,
    }) else {
        return;
    };
    if matches!(
        from_item.level,
        LevelNumber::Hierarchy(1)
            | LevelNumber::Independent77
            | LevelNumber::Renames66
            | LevelNumber::Condition88
    ) {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(ren.span),
            String::from("invalid target level for `66 RENAMES`"),
        ));
    }

    let start_off = from_item.byte_offset.as_u32();
    let end_off = resolve_renames_thru_end(
        ren,
        start_off,
        start_off.saturating_add(from_item.total_byte_size),
        &out.items,
        ctx,
    );

    let span_len = end_off.saturating_sub(start_off).max(1);
    let item_id = DataItemId::new(u32::try_from(out.items.len()).unwrap_or(0));
    out.items.push(HirDataItem {
        id: item_id,
        scope: ctx.scope,
        name: Some(sym),
        level: LevelNumber::Renames66,
        parent: root_01,
        children: Vec::new(),
        section: ctx.section,
        ty: HirType::Group {
            static_byte_len: span_len,
            odo_presence: DynamicOdoPresence::StaticSize,
        },
        byte_offset: ByteOffset::new(start_off),
        total_byte_size: span_len,
        dimensions: Vec::new(),
        redefines: None,
        initial_value: None,
        span: entry.span,
    });
    ctx.symtab.insert_symbol(
        ctx.scope,
        sym,
        SymbolCandidate {
            target: ResolvedSymbol::DataItem(item_id),
            qualifiers: Vec::new(),
            visibility: ctx.inherited_global,
        },
    );
}

/// Computes the exclusive end byte offset of `66 RENAMES from [THRU thru]`.
///
/// # Arguments
///
/// * `ren` - `RENAMES` AST range.
/// * `start_off` - Start byte offset.
/// * `default_end` - End byte offset when `THRU` is omitted.
/// * `items` - Solved items slice.
/// * `ctx` - Layout context.
fn resolve_renames_thru_end(
    ren: &coblang_syntax::ast::RenamesRange,
    start_off: u32,
    default_end: u32,
    items: &[HirDataItem],
    ctx: &mut SectionLayoutCtx<'_>,
) -> u32 {
    let Some(thru_path) = &ren.thru else {
        return default_end;
    };
    let Some((&thru_base, thru_quals)) = thru_path.split_first() else {
        return default_end;
    };
    let Some(thru_item) = ctx
        .symtab
        .resolve_symbol(
            ctx.scope,
            thru_base,
            thru_quals,
            ren.span,
            ctx.interner,
            ctx.sink,
        )
        .and_then(|res| match res {
            ResolvedSymbol::DataItem(thru_id) => items.get(thru_id.as_u32() as usize),
            _ => None,
        })
    else {
        return default_end;
    };
    if thru_item.byte_offset.as_u32() <= start_off {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(ren.span),
            String::from("`RENAMES ... THRU` end item must follow start item"),
        ));
        default_end
    } else {
        thru_item
            .byte_offset
            .as_u32()
            .saturating_add(thru_item.total_byte_size)
    }
}

/// Validates and registers a level-`78` or `01 CONSTANT` compile-time constant.
///
/// # Arguments
///
/// * `entry` - AST constant entry.
/// * `ctx` - Layout context.
fn handle_constant_78(entry: &DataEntry, ctx: &mut SectionLayoutCtx<'_>) {
    if entry.picture.is_some() || entry.occurs.is_some() || entry.redefines.is_some() {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("compile-time constant cannot have `PICTURE`, `OCCURS`, or `REDEFINES`"),
        ));
    }
    let DataEntryName::Named(sym) = entry.name else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("constant entry requires an identifier"),
        ));
        return;
    };
    let Some(vc) = &entry.value else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("constant entry requires a `VALUE` / `AS` literal"),
        ));
        return;
    };
    if let Some(first) = vc.values.first()
        && let Some(cval) = crate::m17_charset::atom_to_hir_const_m17(
            &first.low,
            ctx.scope,
            ctx.symtab,
            ctx.dialect,
        )
    {
        ctx.symtab.insert_symbol(
            ctx.scope,
            sym,
            SymbolCandidate {
                target: ResolvedSymbol::Constant(cval),
                qualifiers: Vec::new(),
                visibility: entry.flags.global,
            },
        );
    }
}

/// Validates and registers a level-`88` condition-name entry.
///
/// # Arguments
///
/// * `entry` - AST `88` entry.
/// * `parent_cond_item` - Owning conditional variable `(DataItemId, GlobalVisibility)`.
/// * `stack` - Ancestor group stack.
/// * `out` - Output [`SolvedDataLayout`].
/// * `ctx` - Layout context.
fn handle_condition_88(
    entry: &DataEntry,
    parent_cond_item: Option<(DataItemId, GlobalVisibility)>,
    stack: &[AncestorFrame],
    out: &mut SolvedDataLayout,
    ctx: &mut SectionLayoutCtx<'_>,
) {
    if entry.picture.is_some()
        || entry.usage.is_some()
        || entry.occurs.is_some()
        || entry.redefines.is_some()
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("level `88` condition-name cannot have `PICTURE`, `USAGE`, `OCCURS`, or `REDEFINES`"),
        ));
    }
    let Some((parent_id, parent_vis, parent_ty)) = parent_cond_item.and_then(|(pid, pvis)| {
        out.items
            .get(pid.as_u32() as usize)
            .map(|it| (pid, pvis, it.ty.clone()))
    }) else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("level `88` condition-name has no preceding conditional variable"),
        ));
        return;
    };
    let DataEntryName::Named(sym) = entry.name else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("level `88` entry requires a condition-name"),
        ));
        return;
    };
    let Some(vc) = &entry.value else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(entry.span),
            String::from("level `88` condition-name requires a `VALUE` clause"),
        ));
        return;
    };

    let mut ranges = Vec::new();
    for vr in &vc.values {
        check_atom_domain(&vr.low, &parent_ty, vr.span, ctx);
        if let Some(hi) = &vr.high {
            check_atom_domain(hi, &parent_ty, vr.span, ctx);
            check_range_ordering(&vr.low, hi, vr.span, ctx);
        }
        if let Some(lo_c) =
            crate::m17_charset::atom_to_hir_const_m17(&vr.low, ctx.scope, ctx.symtab, ctx.dialect)
        {
            let hi_c = vr.high.as_ref().and_then(|h| {
                crate::m17_charset::atom_to_hir_const_m17(h, ctx.scope, ctx.symtab, ctx.dialect)
            });
            ranges.push((lo_c, hi_c));
        }
    }

    let false_value = vc.false_value.as_ref().and_then(|fv| {
        crate::m17_charset::atom_to_hir_const_m17(fv, ctx.scope, ctx.symtab, ctx.dialect)
    });
    let idx = out.conditions_88.len();
    out.conditions_88.push(HirCondition88 {
        name: sym,
        parent_item: parent_id,
        ranges,
        false_value,
        span: entry.span,
    });

    let mut quals = build_qualifiers(stack, ctx.file_qual);
    if let Some(p_name) = out
        .items
        .get(parent_id.as_u32() as usize)
        .and_then(|it| it.name)
        && quals.first() != Some(&p_name)
    {
        quals.insert(0, p_name);
    }
    ctx.symtab.insert_symbol(
        ctx.scope,
        sym,
        SymbolCandidate {
            target: ResolvedSymbol::Condition88(idx),
            qualifiers: quals,
            visibility: parent_vis,
        },
    );
}

/// Validates a non-`88` `VALUE` clause against item residency, `REDEFINES`, `ODO`, and `HirType`.
///
/// # Arguments
///
/// * `vc` - AST `VALUE` clause.
/// * `ty` - Solved [`HirType`].
/// * `is_group` - Whether item is a group.
/// * `under_redef` - Whether item is under `REDEFINES`.
/// * `under_odo` - Whether item is under `OCCURS DEPENDING ON`.
/// * `ctx` - Layout context.
fn validate_value_clause(
    vc: &ValueClause,
    ty: &HirType,
    is_group: bool,
    under_redef: bool,
    under_odo: bool,
    ctx: &mut SectionLayoutCtx<'_>,
) {
    if matches!(ctx.section, HirStorageSection::FileSection(_))
        && matches!(ctx.dialect.base_dialect, Dialect::Cobol85)
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(vc.span),
            String::from("`VALUE` clause is not permitted in `FILE SECTION` in COBOL-85"),
        ));
    }
    if under_redef && ctx.dialect.relaxed_syntax == RelaxedSyntaxMode::Strict {
        if matches!(
            ctx.dialect.base_dialect,
            Dialect::Cobol85 | Dialect::Cobol2002 | Dialect::Cobol2014 | Dialect::Cobol2023
        ) {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(vc.span),
                String::from("`VALUE` clause is not permitted on or under a `REDEFINES` item"),
            ));
        } else {
            ctx.sink.emit(Diagnostic::warning(
                DiagnosticCode::W0003Redefinition,
                WarningGroup::Redefinition,
                Some(vc.span),
                String::from("`VALUE` clause ignored on `REDEFINES` item"),
            ));
        }
    }
    if under_odo
        && matches!(
            ctx.dialect.base_dialect,
            Dialect::Cobol85 | Dialect::Cobol2014
        )
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(vc.span),
            String::from("`VALUE` clause is not permitted with `OCCURS DEPENDING ON`"),
        ));
    }
    if !is_group {
        for vr in &vc.values {
            if vr.high.is_some() {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(vr.span),
                    String::from(
                        "`VALUE ... THRU` range is only valid on level `88` condition-names",
                    ),
                ));
            }
            check_atom_domain(&vr.low, ty, vr.span, ctx);
        }
    }
}

/// Checks compatibility of a `ValueAtom` with `ty` (numeric vs string, sign, precision, length).
///
/// # Arguments
///
/// * `atom` - Literal/figurative value atom.
/// * `ty` - Target [`HirType`].
/// * `span` - Value span.
/// * `ctx` - Layout context.
fn check_atom_domain(
    atom: &ValueAtom,
    ty: &HirType,
    span: SourceSpan,
    ctx: &mut SectionLayoutCtx<'_>,
) {
    let cat = ty.category();
    match cat {
        DataCategory::Numeric => check_numeric_atom_domain(atom, ty, span, ctx),
        DataCategory::Alphabetic
        | DataCategory::Alphanumeric
        | DataCategory::AlphanumericEdited => check_alpha_atom_domain(cat, atom, ty, span, ctx),
        _ => {}
    }
}

/// Validates a `VALUE` atom assigned to a [`DataCategory::Numeric`] item.
///
/// # Arguments
///
/// * `atom` - Literal/figurative value atom.
/// * `ty` - Target numeric [`HirType`].
/// * `span` - Value span.
/// * `ctx` - Layout context.
fn check_numeric_atom_domain(
    atom: &ValueAtom,
    ty: &HirType,
    span: SourceSpan,
    ctx: &mut SectionLayoutCtx<'_>,
) {
    match atom {
        ValueAtom::String { .. } => {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(span),
                String::from("non-numeric literal in `VALUE` clause of numeric data item"),
            ));
        }
        ValueAtom::Figurative(fig) => {
            if !matches!(fig, FigurativeValue::Zero) {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(span),
                    String::from(
                        "invalid figurative constant for numeric data item (only `ZERO` allowed)",
                    ),
                ));
            }
        }
        ValueAtom::Integer(sym) => {
            let text = ctx
                .interner
                .resolve_original(*sym)
                .unwrap_or("0")
                .to_owned();
            if text.starts_with('-') && is_unsigned_numeric(ty) {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(span),
                    String::from("negative `VALUE` literal on unsigned numeric data item"),
                ));
            }
            check_numeric_value_fit(text.trim_start_matches(['+', '-']), 0, ty, span, ctx);
        }
        ValueAtom::Decimal {
            digits,
            scale,
            sign,
        } => {
            if *sign == coblang_syntax::LiteralSign::Negative && is_unsigned_numeric(ty) {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(span),
                    String::from("negative `VALUE` literal on unsigned numeric data item"),
                ));
            }
            let d_str = ctx
                .interner
                .resolve_original(*digits)
                .unwrap_or("0")
                .to_owned();
            check_numeric_value_fit(&d_str, scale.get(), ty, span, ctx);
        }
        _ => {}
    }
}

/// Validates a `VALUE` atom assigned to an alphabetic or alphanumeric item.
///
/// # Arguments
///
/// * `cat` - Target [`DataCategory`].
/// * `atom` - Literal/figurative value atom.
/// * `ty` - Target [`HirType`].
/// * `span` - Value span.
/// * `ctx` - Layout context.
fn check_alpha_atom_domain(
    cat: DataCategory,
    atom: &ValueAtom,
    ty: &HirType,
    span: SourceSpan,
    ctx: &mut SectionLayoutCtx<'_>,
) {
    match atom {
        ValueAtom::Integer(_) | ValueAtom::Decimal { .. } | ValueAtom::Float(_) => {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(span),
                String::from(
                    "numeric literal is not permitted in `VALUE` of alphanumeric/alphabetic item",
                ),
            ));
        }
        ValueAtom::String { bytes, .. } => {
            if cat == DataCategory::Alphabetic
                && !bytes.iter().all(|b| b.is_ascii_alphabetic() || *b == b' ')
            {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(span),
                    String::from("`PIC A` alphabetic `VALUE` may only contain letters and spaces"),
                ));
            }
            let cap = ty.static_byte_size() as usize;
            if bytes.len() > cap {
                ctx.sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0004Truncate,
                    WarningGroup::Truncate,
                    Some(span),
                    format!(
                        "`VALUE` literal length ({}) exceeds item size ({cap})",
                        bytes.len()
                    ),
                ));
            }
        }
        _ => {}
    }
}

/// Checks whether non-zero integer/fractional digits of a numeric `VALUE` fit inside `ty`.
///
/// # Arguments
///
/// * `all_digits` - Unscaled digit string.
/// * `lit_scale` - Literal scale factor.
/// * `ty` - Target [`HirType`].
/// * `span` - Value span.
/// * `ctx` - Layout context.
fn check_numeric_value_fit(
    all_digits: &str,
    lit_scale: i16,
    ty: &HirType,
    span: SourceSpan,
    ctx: &mut SectionLayoutCtx<'_>,
) {
    let (cap_digits, cap_scale) = match ty {
        HirType::ZonedDecimal { digits, scale, .. }
        | HirType::PackedDecimal { digits, scale, .. } => (i16::from(digits.get()), scale.get()),
        HirType::BinaryInt {
            byte_width,
            digits,
            scale,
            trunc,
            ..
        } => {
            let eff_digits = match trunc {
                TruncationPolicy::TruncateToPic => digits.get(),
                TruncationPolicy::FullBinaryRange => match *byte_width {
                    1 => 3,
                    2 => 5,
                    3 => 8,
                    4 => 10,
                    5..=7 => 18,
                    8 => 20,
                    _ => 38,
                },
            };
            (i16::from(eff_digits), scale.get())
        }
        _ => return,
    };
    let frac_len = usize::try_from(lit_scale.max(0)).unwrap_or(0);
    let split_idx = all_digits.len().saturating_sub(frac_len);
    let int_part = all_digits
        .get(..split_idx)
        .unwrap_or("")
        .trim_start_matches('0');
    let frac_part = all_digits
        .get(split_idx..)
        .unwrap_or("")
        .trim_end_matches('0');

    let allowed_int = usize::try_from((cap_digits - cap_scale).max(0)).unwrap_or(0);
    let allowed_frac = usize::try_from(cap_scale.max(0)).unwrap_or(0);
    if int_part.len() > allowed_int || frac_part.len() > allowed_frac {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            String::from(
                "numeric `VALUE` literal exceeds `PICTURE` integer or fractional capacity",
            ),
        ));
    }
}

/// Checks `low <= high` on `88` `VALUE low THRU high` ranges.
///
/// # Arguments
///
/// * `low` - Lower bound atom.
/// * `high` - Upper bound atom.
/// * `span` - Range span.
/// * `ctx` - Layout context.
fn check_range_ordering(
    low: &ValueAtom,
    high: &ValueAtom,
    span: SourceSpan,
    ctx: &mut SectionLayoutCtx<'_>,
) {
    match (low, high) {
        (ValueAtom::Integer(lo_s), ValueAtom::Integer(hi_s)) => {
            if let (Ok(l), Ok(h)) = (
                ctx.interner
                    .resolve_original(*lo_s)
                    .unwrap_or("0")
                    .parse::<i128>(),
                ctx.interner
                    .resolve_original(*hi_s)
                    .unwrap_or("0")
                    .parse::<i128>(),
            ) && l > h
            {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(span),
                    format!("`THRU` range lower bound ({l}) exceeds upper bound ({h})"),
                ));
            }
        }
        (ValueAtom::String { bytes: lb, .. }, ValueAtom::String { bytes: hb, .. }) if lb > hb => {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(span),
                String::from("`THRU` string range lower bound exceeds upper bound"),
            ));
        }
        _ => {}
    }
}

/// Resolves `OCCURS ... DEPENDING ON` objects and `ASCENDING`/`DESCENDING KEY`s after all items are registered.
///
/// # Arguments
///
/// * `_data` - AST data division (unused; `out.pending_occurs` carries exact `(item_id, dim_idx)` pairs).
/// * `scope` - Owning scope.
/// * `out` - Output [`SolvedDataLayout`].
/// * `dialect` - Active dialect config.
/// * `symtab` - Symbol table.
/// * `interner` - Symbol interner.
/// * `sink` - Diagnostic sink.
fn resolve_odo_targets(
    _data: &DataDivision,
    scope: ScopeId,
    out: &mut SolvedDataLayout,
    dialect: &DialectConfig,
    symtab: &SymbolTable,
    interner: &SymbolInterner,
    sink: &mut DiagnosticSink,
) {
    let pending = core::mem::take(&mut out.pending_occurs);
    for (table_id, dim_idx, occ) in pending {
        if let Some(path) = &occ.depending_on
            && let Some((&base, quals)) = path.split_first()
            && let Some(ResolvedSymbol::DataItem(odo_id)) =
                symtab.resolve_symbol(scope, base, quals, occ.span, interner, sink)
        {
            let is_int = out
                .items
                .get(odo_id.as_u32() as usize)
                .is_some_and(|odo_item| match &odo_item.ty {
                    HirType::ZonedDecimal { scale, .. }
                    | HirType::PackedDecimal { scale, .. }
                    | HirType::BinaryInt { scale, .. } => scale.get() == 0,
                    _ => false,
                });
            if !is_int {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(occ.span),
                    String::from("`DEPENDING ON` object must be an elementary integer data item"),
                ));
            }
            let is_table_odo = out
                .items
                .get(odo_id.as_u32() as usize)
                .is_some_and(|odo_item| !odo_item.dimensions.is_empty());
            if is_table_odo && dialect.complex_odo == OdoComplexity::SimpleIsoOnly {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0007InvalidLayout,
                    Some(occ.span),
                    String::from("`DEPENDING ON` object cannot itself be subscripted/table-bound"),
                ));
            }
            propagate_depending_on(&mut out.items, table_id, dim_idx, odo_id);
        }

        if !occ.keys.is_empty() {
            let resolved_keys = resolve_occurs_key_list(&out.items, table_id, &occ.keys);
            let _ = out
                .items
                .get_mut(table_id.as_u32() as usize)
                .and_then(|it| it.dimensions.get_mut(dim_idx))
                .map(|dim| dim.keys = resolved_keys);
        }
    }
}

/// Stamps `depending_on = Some(odo_id)` onto `table_id` and all subordinate descendants at `dim_idx`.
///
/// # Arguments
///
/// * `items` - Program data items slice.
/// * `table_id` - Root table item handle.
/// * `dim_idx` - Dimension index on `table_id`.
/// * `odo_id` - Resolved `DEPENDING ON` object handle.
fn propagate_depending_on(
    items: &mut [HirDataItem],
    table_id: DataItemId,
    dim_idx: usize,
    odo_id: DataItemId,
) {
    let mut work = vec![table_id];
    while let Some(cur_id) = work.pop() {
        let children = items
            .get_mut(cur_id.as_u32() as usize)
            .map_or_default(|item| {
                let _ = item
                    .dimensions
                    .get_mut(dim_idx)
                    .map(|d| d.depending_on = Some(odo_id));
                item.children.clone()
            });
        work.extend_from_slice(&children);
    }
}

/// Resolves `OCCURS` `ASCENDING`/`DESCENDING KEY` symbol paths within `table_id`'s subtree.
///
/// # Arguments
///
/// * `items` - Program data items slice.
/// * `table_id` - Owning table item handle.
/// * `specs` - AST key specifications on `OCCURS`.
#[must_use]
fn resolve_occurs_key_list(
    items: &[HirDataItem],
    table_id: DataItemId,
    specs: &[coblang_syntax::ast::OccursKeySpec],
) -> Vec<HirTableKey> {
    let mut subtree = Vec::new();
    let mut work = vec![table_id];
    while let Some(cid) = work.pop() {
        subtree.push(cid);
        let children = items
            .get(cid.as_u32() as usize)
            .map_or(&[][..], |it| it.children.as_slice());
        for &ch in children.iter().rev() {
            work.push(ch);
        }
    }
    let mut out_keys = Vec::new();
    for spec in specs {
        let direction = match spec.order {
            KeyOrder::Ascending => HirSortDirection::Ascending,
            KeyOrder::Descending => HirSortDirection::Descending,
        };
        for path in &spec.keys {
            if let Some(&matched_id) = path.first().and_then(|&leaf_sym| {
                subtree.iter().find(|id| {
                    items
                        .get(id.as_u32() as usize)
                        .is_some_and(|it| it.name == Some(leaf_sym))
                })
            }) {
                out_keys.push(HirTableKey {
                    direction,
                    key_item: matched_id,
                });
            }
        }
    }
    out_keys
}

/// Registers `INDEXED BY` index-names as implicit `HirType::Index` items in `out` and `ctx.symtab`.
///
/// # Arguments
///
/// * `names` - `INDEXED BY` symbol slice.
/// * `occ_span` - Span of the `OCCURS` clause.
/// * `visibility` - Effective `GLOBAL` visibility.
/// * `out` - Output [`SolvedDataLayout`].
/// * `ctx` - Layout context.
fn register_indexed_by(
    names: &[SymbolId],
    occ_span: SourceSpan,
    visibility: GlobalVisibility,
    out: &mut SolvedDataLayout,
    ctx: &mut SectionLayoutCtx<'_>,
) -> Vec<DataItemId> {
    let mut ids = Vec::with_capacity(names.len());
    for &idx_sym in names {
        let id = DataItemId::new(u32::try_from(out.items.len()).unwrap_or(0));
        ids.push(id);
        if ctx
            .symtab
            .scope(ctx.scope)
            .is_some_and(|s| s.by_name.contains_key(&idx_sym))
        {
            let lbl = ctx.interner.resolve_original(idx_sym).unwrap_or("index");
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(occ_span),
                format!("duplicate index-name `{lbl}`"),
            ));
        }
        out.items.push(HirDataItem {
            id,
            scope: ctx.scope,
            name: Some(idx_sym),
            level: LevelNumber::Independent77,
            parent: None,
            children: Vec::new(),
            section: HirStorageSection::WorkingStorage,
            ty: HirType::Index,
            byte_offset: ByteOffset::ZERO,
            total_byte_size: 8,
            dimensions: Vec::new(),
            redefines: None,
            initial_value: None,
            span: occ_span,
        });
        ctx.symtab.insert_symbol(
            ctx.scope,
            idx_sym,
            SymbolCandidate {
                target: ResolvedSymbol::DataItem(id),
                qualifiers: Vec::new(),
                visibility,
            },
        );
    }
    ids
}

/// Builds the ancestor qualifier chain (`[immediate_parent, ..., root_01, optional_fd]`) from `stack`.
///
/// # Arguments
///
/// * `stack` - Ancestor group stack.
/// * `file_qual` - Optional enclosing `FD` file name symbol.
#[must_use]
fn build_qualifiers(stack: &[AncestorFrame], file_qual: Option<SymbolId>) -> Vec<SymbolId> {
    let mut quals: Vec<SymbolId> = stack.iter().rev().filter_map(|f| f.name).collect();
    if let Some(fq) = file_qual {
        quals.push(fq);
    }
    quals
}

/// Converts a [`ValueAtom`] into [`HirConstValue`].
///
/// # Arguments
///
/// * `atom` - AST value atom.
/// * `scope` - Lookup scope for constant identifiers.
/// * `symtab` - Symbol table.
#[must_use]
pub fn atom_to_hir_const(
    atom: &ValueAtom,
    scope: ScopeId,
    symtab: &SymbolTable,
) -> Option<HirConstValue> {
    match atom {
        ValueAtom::String { bytes, .. } => Some(HirConstValue::Bytes(bytes.clone())),
        ValueAtom::Integer(sym) => Some(HirConstValue::Decimal {
            text: *sym,
            scale: ScaleFactor::ZERO,
            sign: HirDecimalSign::UnsignedOrEmbedded,
        }),
        ValueAtom::Decimal {
            digits,
            scale,
            sign,
        } => {
            let hsign = match sign {
                coblang_syntax::LiteralSign::Unsigned => HirDecimalSign::UnsignedOrEmbedded,
                coblang_syntax::LiteralSign::Positive => HirDecimalSign::Positive,
                coblang_syntax::LiteralSign::Negative => HirDecimalSign::Negative,
            };
            Some(HirConstValue::Decimal {
                text: *digits,
                scale: *scale,
                sign: hsign,
            })
        }
        ValueAtom::Float(sym) => Some(HirConstValue::Float(*sym)),
        ValueAtom::Figurative(fig) => {
            let hf = match fig {
                FigurativeValue::Zero => HirFigurative::Zero,
                FigurativeValue::Space => HirFigurative::Space,
                FigurativeValue::HighValue => HirFigurative::HighValue,
                FigurativeValue::LowValue => HirFigurative::LowValue,
                FigurativeValue::Quote => HirFigurative::Quote,
                FigurativeValue::Null => HirFigurative::Null,
                FigurativeValue::All(b) => HirFigurative::All(b.clone()),
            };
            Some(HirConstValue::Figurative(hf))
        }
        ValueAtom::Identifier(sym) => symtab
            .scope(scope)
            .and_then(|s| s.by_name.get(sym))
            .and_then(|list| list.first())
            .and_then(|cand| match &cand.target {
                ResolvedSymbol::Constant(c) => Some(c.clone()),
                _ => None,
            }),
    }
}

/// Returns `true` if the next non-`88` entry in `entries[idx + 1..]` has hierarchy level `> lvl`.
///
/// # Arguments
///
/// * `entries` - Section entry slice.
/// * `idx` - Current entry index.
/// * `lvl` - Current numeric level.
#[must_use]
fn has_subordinate_hierarchy_child(entries: &[DataEntry], idx: usize, lvl: u8) -> bool {
    let mut scan = idx + 1;
    while let Some(next) = entries.get(scan) {
        match next.level {
            LevelNumber::Condition88 => scan += 1,
            LevelNumber::Hierarchy(next_lvl) => return next_lvl > lvl,
            _ => return false,
        }
    }
    false
}

/// Returns `true` if `entries[idx]` (`77`) is illegally followed by a `02`..`49` or `66` entry.
///
/// # Arguments
///
/// * `entries` - Section entry slice.
/// * `idx` - Current `77` entry index.
#[must_use]
fn next_non_88_is_subordinate(entries: &[DataEntry], idx: usize) -> bool {
    let mut scan = idx + 1;
    while let Some(next) = entries.get(scan) {
        match next.level {
            LevelNumber::Condition88 => scan += 1,
            LevelNumber::Hierarchy(lvl) => return lvl > 1,
            LevelNumber::Renames66 => return true,
            _ => return false,
        }
    }
    false
}

/// Applies `SYNCHRONIZED` natural alignment padding to `offset` for binary/float/pointer items.
///
/// # Arguments
///
/// * `offset` - Unaligned byte offset.
/// * `sync` - `SYNCHRONIZED` mode.
/// * `ty` - Target [`HirType`].
#[must_use]
const fn apply_sync_alignment(offset: u32, sync: SyncAlignment, ty: &HirType) -> u32 {
    if matches!(sync, SyncAlignment::Unsynchronized) {
        return offset;
    }
    let align = match ty {
        HirType::BinaryInt { byte_width, .. } => match *byte_width {
            1 => 1,
            2 => 2,
            3..=4 => 4,
            _ => 8,
        },
        HirType::FloatBinary { .. }
        | HirType::FloatDecimal { .. }
        | HirType::Pointer
        | HirType::ProgramPointer
        | HirType::Index
        | HirType::ObjectRef { .. } => {
            let sz = ty.static_byte_size();
            if sz > 8 { 8 } else { sz }
        }
        HirType::National { .. } | HirType::NationalEdited { .. } => 2,
        _ => 1,
    };
    if align <= 1 {
        offset
    } else {
        let rem = offset % align;
        if rem == 0 {
            offset
        } else {
            offset.saturating_add(align - rem)
        }
    }
}

/// Returns `true` when `ty` is an unsigned numeric representation.
///
/// # Arguments
///
/// * `ty` - Target [`HirType`].
#[must_use]
const fn is_unsigned_numeric(ty: &HirType) -> bool {
    match ty {
        HirType::ZonedDecimal { sign, .. } => {
            matches!(sign, coblang_hir::NumericSignSpec::Unsigned)
        }
        HirType::PackedDecimal { sign, .. } | HirType::BinaryInt { sign, .. } => {
            matches!(sign, Signedness::Unsigned)
        }
        _ => false,
    }
}
