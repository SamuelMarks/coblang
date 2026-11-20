//! Semantic validation and HIR lowering for Milestone M11 pointers, `SET` pointer/address/entry
//! operations, `ALLOCATE`, `FREE`, and nested `IS GLOBAL` storage inheritance.

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{DataItemId, LevelNumber, ScopeId, SourceSpan, SymbolId};
use coblang_diag::{Diagnostic, DiagnosticCode};
use coblang_hir::{
    DataCategory, HirAllocateInit, HirAllocateSize, HirAllocateSpec, HirCallArg, HirCallReturning,
    HirCallSpec, HirCondition88, HirConstValue, HirDataItem, HirExpr, HirFigurative, HirFreeTarget,
    HirOccursDim, HirPlace, HirPointerTarget, HirPointerValue, HirProgram, HirSetPointerSpec,
    HirStepDir, HirStmt, HirStorageSection, HirTableKey, HirType,
};
use coblang_syntax::ast::{
    AllocateInitClause, AllocateTargetUnit, CallArg, CallReturning, DataRef, DataRefModifier,
    EventHandlers, Expr, GlobalVisibility, PassingMode, StepDirection,
};

use crate::layout::SolvedDataLayout;
use crate::lower_hir::{UnitLowerCtx, lower_event_handlers, lower_expr_with_cat, resolve_place};
use crate::symbols::{ResolvedSymbol, SymbolCandidate, SymbolTable};

/// Resolves the primary `01`/`77` root [`DataItemId`] owning `id` (following `parent` and `redefines`).
///
/// # Arguments
///
/// * `id` - Starting [`DataItemId`].
/// * `ctx` - Unit lowering context.
#[must_use]
pub(crate) fn resolve_root_item_id(id: DataItemId, ctx: &UnitLowerCtx<'_>) -> DataItemId {
    primary_root_in_slice(id, &ctx.layout.items)
}

/// Resolves a `SET` pointer or `ADDRESS OF` target [`DataRef`] into a [`HirPointerTarget`].
///
/// # Arguments
///
/// * `target` - Target AST [`DataRef`].
/// * `ctx` - Unit lowering context.
pub(crate) fn resolve_pointer_set_target(
    target: &DataRef,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<HirPointerTarget> {
    if target.modifier == DataRefModifier::AddressOf {
        let id = ctx
            .symtab
            .resolve_symbol(
                ctx.scope,
                target.name.base,
                &target.name.qualifiers,
                target.span,
                ctx.interner,
                ctx.sink,
            )
            .and_then(|sym| match sym {
                ResolvedSymbol::DataItem(did) => Some(did),
                _ => None,
            })?;
        let is_valid_based_or_linkage_root = ctx
            .layout
            .items
            .get(id.as_u32() as usize)
            .is_some_and(|item| {
                matches!(
                    item.section,
                    HirStorageSection::BasedStorage | HirStorageSection::Linkage
                ) && matches!(
                    item.level,
                    LevelNumber::Hierarchy(1) | LevelNumber::Independent77
                )
            });
        if !is_valid_based_or_linkage_root {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(target.span),
                String::from(
                    "`SET ADDRESS OF` target must be a `01` or `77` `BASED` or `LINKAGE` item",
                ),
            ));
            return None;
        }
        return Some(HirPointerTarget::BaseAddressOf(resolve_root_item_id(
            id, ctx,
        )));
    }

    let pl = resolve_place(target, ctx)?;
    if matches!(pl.effective_ty, HirType::Pointer | HirType::ProgramPointer) {
        Some(HirPointerTarget::PointerPlace(pl))
    } else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(target.span),
            String::from("`SET` pointer operation requires a `POINTER` or `ADDRESS OF` target"),
        ));
        None
    }
}

/// Lowers `SET targets TO value` when any target or `value` involves pointers or `ADDRESS OF`.
///
/// # Arguments
///
/// * `targets` - Target [`DataRef`] slice.
/// * `val_hir` - Lowered right-hand [`HirExpr`].
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_set_pointer_to_value(
    targets: &[DataRef],
    val_hir: HirExpr,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let ptr_val = match val_hir {
        HirExpr::Const {
            value: HirConstValue::Figurative(HirFigurative::Null),
            ..
        } => HirPointerValue::Null,
        HirExpr::AddressOf(pl) => HirPointerValue::AddressOf(pl),
        HirExpr::Place(pl)
            if matches!(pl.effective_ty, HirType::Pointer | HirType::ProgramPointer) =>
        {
            HirPointerValue::PointerPlace(pl)
        }
        _ => {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(span),
                String::from(
                    "`SET` pointer source must be `NULL`, `ADDRESS OF`, or a `POINTER` item",
                ),
            ));
            return;
        }
    };

    let mut resolved = Vec::with_capacity(targets.len());
    for t in targets {
        if let Some(pt) = resolve_pointer_set_target(t, ctx) {
            resolved.push(pt);
        }
    }
    if !resolved.is_empty() {
        out.push(HirStmt::SetPointer(Box::new(HirSetPointerSpec {
            targets: resolved,
            value: ptr_val,
            span,
        })));
    }
}

/// Lowers `SET prog-ptr ... TO {ENTRY | ADDRESS OF PROGRAM | ADDRESS OF ENTRY} expr`.
///
/// # Arguments
///
/// * `targets` - Target [`DataRef`] slice.
/// * `entry` - Entry name AST expression.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_set_to_entry(
    targets: &[DataRef],
    entry: &Expr,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let (entry_hir, _) = lower_expr_with_cat(entry, ctx);
    let mut resolved = Vec::with_capacity(targets.len());
    for t in targets {
        if let Some(pt) = resolve_pointer_set_target(t, ctx) {
            resolved.push(pt);
        }
    }
    if !resolved.is_empty() {
        out.push(HirStmt::SetPointer(Box::new(HirSetPointerSpec {
            targets: resolved,
            value: HirPointerValue::Entry(entry_hir),
            span,
        })));
    }
}

/// Lowers `SET {ptr | ADDRESS OF item} {UP | DOWN} BY amount`.
///
/// # Arguments
///
/// * `target` - Single pointer or `ADDRESS OF` target [`DataRef`].
/// * `direction` - `Up` vs `Down`.
/// * `amt_hir` - Lowered byte offset [`HirExpr`].
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_set_pointer_step(
    target: &DataRef,
    direction: StepDirection,
    amt_hir: HirExpr,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(pt) = resolve_pointer_set_target(target, ctx) else {
        return;
    };
    let dir = match direction {
        StepDirection::Up => HirStepDir::Up,
        StepDirection::Down => HirStepDir::Down,
    };
    out.push(HirStmt::SetPointer(Box::new(HirSetPointerSpec {
        targets: vec![pt],
        value: HirPointerValue::StepBytes {
            direction: dir,
            amount: amt_hir,
        },
        span,
    })));
}

/// Validates and lowers `ALLOCATE {expr CHARACTERS | based-item} [INITIALIZED [TO expr]] [RETURNING ptr]`.
///
/// # Arguments
///
/// * `target` - Size expression or `BASED` item expression.
/// * `unit` - Whether `CHARACTERS` was explicitly written.
/// * `initialized` - Parsed [`AllocateInitClause`].
/// * `returning` - Optional `RETURNING` target [`DataRef`].
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_allocate_stmt(
    target: &Expr,
    unit: AllocateTargetUnit,
    initialized: &AllocateInitClause,
    returning: Option<&DataRef>,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let ret_place = returning.and_then(|r| validate_returning_ptr(r, ctx));
    if unit == AllocateTargetUnit::ExplicitCharacters && returning.is_none() {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            String::from("`ALLOCATE ... CHARACTERS` requires a `RETURNING` pointer phrase"),
        ));
    }

    let size_opt = match unit {
        AllocateTargetUnit::ExplicitCharacters => {
            let (sz_hir, sz_cat) = lower_expr_with_cat(target, ctx);
            if sz_cat == DataCategory::Numeric {
                Some(HirAllocateSize::CharCount(sz_hir))
            } else {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(span),
                    String::from(
                        "amount in `ALLOCATE ... CHARACTERS` must be a numeric expression",
                    ),
                ));
                None
            }
        }
        AllocateTargetUnit::BasedOrDefault => resolve_based_alloc_target(target, span, ctx),
    };

    let init_hir = match initialized {
        AllocateInitClause::Uninitialized => HirAllocateInit::Uninitialized,
        AllocateInitClause::InitializedDefault => HirAllocateInit::InitializedDefault,
        AllocateInitClause::InitializedTo(fill_expr) => {
            let (fill_hir, fill_cat) = lower_expr_with_cat(fill_expr, ctx);
            if matches!(size_opt, Some(HirAllocateSize::CharCount(_)))
                && fill_cat == DataCategory::Numeric
                && !is_figurative_expr(&fill_hir)
            {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(span),
                    String::from(
                        "`ALLOCATE ... CHARACTERS INITIALIZED TO` requires an alphanumeric or figurative value",
                    ),
                ));
            }
            HirAllocateInit::InitializedTo(fill_hir)
        }
    };

    if let Some(size) = size_opt {
        out.push(HirStmt::Allocate(Box::new(HirAllocateSpec {
            size,
            init: init_hir,
            returning: ret_place,
            span,
        })));
    }
}

/// Returns `true` when `expr` is a figurative constant (`ZERO`, `SPACE`, `LOW-VALUE`, etc.).
///
/// # Arguments
///
/// * `expr` - Lowered [`HirExpr`].
#[must_use]
const fn is_figurative_expr(expr: &HirExpr) -> bool {
    matches!(
        expr,
        HirExpr::Const {
            value: HirConstValue::Figurative(_),
            ..
        }
    )
}

/// Validates the optional `RETURNING` target of `ALLOCATE` as a `USAGE POINTER` item.
///
/// # Arguments
///
/// * `ret_ref` - `RETURNING` [`DataRef`].
/// * `ctx` - Unit lowering context.
fn validate_returning_ptr(ret_ref: &DataRef, ctx: &mut UnitLowerCtx<'_>) -> Option<HirPlace> {
    let pl = resolve_place(ret_ref, ctx)?;
    if pl.effective_ty == HirType::Pointer {
        Some(pl)
    } else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(ret_ref.span),
            String::from("`ALLOCATE ... RETURNING` target must be `USAGE POINTER`"),
        ));
        None
    }
}

/// Validates a non-`CHARACTERS` `ALLOCATE` target as a `01`/`77` `BASED` data item.
///
/// # Arguments
///
/// * `target` - Target [`Expr`].
/// * `span` - Enclosing statement span.
/// * `ctx` - Unit lowering context.
fn resolve_based_alloc_target(
    target: &Expr,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<HirAllocateSize> {
    let Expr::Data(dref) = target else {
        let _ = lower_expr_with_cat(target, ctx);
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            String::from("`ALLOCATE` without `CHARACTERS` requires a `BASED` data item"),
        ));
        return None;
    };
    let pl = resolve_place(dref, ctx)?;
    let is_based_root = ctx
        .layout
        .items
        .get(pl.item.as_u32() as usize)
        .is_some_and(|item| {
            item.section == HirStorageSection::BasedStorage
                && matches!(
                    item.level,
                    LevelNumber::Hierarchy(1) | LevelNumber::Independent77
                )
        });
    if !is_based_root {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(dref.span),
            String::from("target of `ALLOCATE` is not a `01`/`77` `BASED` data item"),
        ));
        return None;
    }
    Some(HirAllocateSize::BasedItem(resolve_root_item_id(
        pl.item, ctx,
    )))
}

/// Validates and lowers `FREE target ...` into [`HirStmt::Free`].
///
/// # Arguments
///
/// * `targets` - Target [`DataRef`] slice.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_free_stmt(
    targets: &[DataRef],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut lowered = Vec::with_capacity(targets.len());
    for t in targets {
        let Some((pl, item)) = resolve_place(t, ctx).and_then(|pl| {
            ctx.layout
                .items
                .get(pl.item.as_u32() as usize)
                .map(|it| (pl, it))
        }) else {
            continue;
        };
        let is_based_root = item.section == HirStorageSection::BasedStorage
            && matches!(
                item.level,
                LevelNumber::Hierarchy(1) | LevelNumber::Independent77
            );
        if t.modifier == DataRefModifier::AddressOf {
            if is_based_root {
                lowered.push(HirFreeTarget::BasedItem(resolve_root_item_id(pl.item, ctx)));
            } else {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(t.span),
                    String::from("`FREE ADDRESS OF` target must be a `01`/`77` `BASED` data item"),
                ));
            }
        } else if pl.effective_ty == HirType::Pointer {
            lowered.push(HirFreeTarget::PointerPlace(pl));
        } else if is_based_root {
            lowered.push(HirFreeTarget::BasedItem(resolve_root_item_id(pl.item, ctx)));
        } else {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(t.span),
                String::from("`FREE` target must be a `POINTER` or `01`/`77` `BASED` data item"),
            ));
        }
    }
    if !lowered.is_empty() {
        out.push(HirStmt::Free {
            targets: lowered,
            span,
        });
    }
}

/// Resolves the primary `01`/`77` root [`DataItemId`] owning `id` inside `items`.
///
/// # Arguments
///
/// * `id` - Starting [`DataItemId`].
/// * `items` - Program [`HirDataItem`] arena.
#[must_use]
fn primary_root_in_slice(id: DataItemId, items: &[HirDataItem]) -> DataItemId {
    let mut cur_id = id;
    while let Some(next_id) = items
        .get(cur_id.as_u32() as usize)
        .and_then(|cur| cur.parent.or(cur.redefines))
    {
        cur_id = next_id;
    }
    cur_id
}

/// Collects all `GlobalVisibility::Global` symbol bindings visible from `start_scope` (nearest scope first).
///
/// # Arguments
///
/// * `start_scope` - Immediate enclosing [`ScopeId`].
/// * `symtab` - Shared [`SymbolTable`].
#[must_use]
fn collect_visible_global_candidates(
    start_scope: ScopeId,
    symtab: &SymbolTable,
) -> Vec<(SymbolId, SymbolCandidate)> {
    let mut out = Vec::new();
    let mut seen_names = BTreeSet::new();
    let mut cur_opt = Some(start_scope);
    while let Some(sc) = cur_opt.and_then(|cur| symtab.scope(cur)) {
        let mut newly_bound = Vec::new();
        for (&sym, cands) in &sc.by_name {
            if seen_names.contains(&sym) {
                continue;
            }
            for cand in cands {
                if cand.visibility == GlobalVisibility::Global {
                    out.push((sym, cand.clone()));
                    newly_bound.push(sym);
                }
            }
        }
        seen_names.extend(newly_bound);
        cur_opt = sc.parent_scope;
    }
    out
}

/// Remaps an [`HirOccursDim`] slice using `id_map`.
///
/// # Arguments
///
/// * `dims` - Source table dimensions.
/// * `id_map` - Old-to-new [`DataItemId`] mapping.
#[must_use]
fn remap_inherited_dims(
    dims: &[HirOccursDim],
    id_map: &BTreeMap<DataItemId, DataItemId>,
) -> Vec<HirOccursDim> {
    dims.iter()
        .map(|d| HirOccursDim {
            min: d.min,
            max: d.max,
            element_byte_stride: d.element_byte_stride,
            depending_on: d
                .depending_on
                .map(|id| id_map.get(&id).copied().unwrap_or(id)),
            keys: d
                .keys
                .iter()
                .map(|k| HirTableKey {
                    direction: k.direction,
                    key_item: id_map.get(&k.key_item).copied().unwrap_or(k.key_item),
                })
                .collect(),
            indexed_by: d
                .indexed_by
                .iter()
                .map(|id| id_map.get(id).copied().unwrap_or(*id))
                .collect(),
        })
        .collect()
}

/// Builds an inherited synthetic parent [`ScopeId`] and preloaded [`SolvedDataLayout`] importing
/// all `IS GLOBAL` root subtrees from `parent_hir` for a nested child program.
///
/// # Arguments
///
/// * `parent_hir` - Lowered enclosing parent [`HirProgram`].
/// * `child_future_scope` - Placeholder [`ScopeId`] or parent scope tag for imported items.
/// * `symtab` - Shared [`SymbolTable`].
pub(crate) fn prepare_inherited_global_context(
    parent_hir: &HirProgram,
    child_future_scope: ScopeId,
    symtab: &mut SymbolTable,
) -> (ScopeId, SolvedDataLayout) {
    let global_cands = collect_visible_global_candidates(parent_hir.scope, symtab);
    let mut global_roots = BTreeSet::new();
    let mut global_files = BTreeSet::new();
    for (_, cand) in &global_cands {
        match cand.target {
            ResolvedSymbol::DataItem(did) => {
                let _ = global_roots.insert(primary_root_in_slice(did, &parent_hir.data_items));
            }
            ResolvedSymbol::File(fid) => {
                let _ = global_files.insert(fid);
                if let Some(fc) = parent_hir.files.get(fid.as_u32() as usize) {
                    for did in crate::m14_fileio::collect_connector_referenced_items(fc) {
                        let _ =
                            global_roots.insert(primary_root_in_slice(did, &parent_hir.data_items));
                    }
                }
            }
            _ => {}
        }
    }

    let mut id_map = BTreeMap::new();
    let mut next_idx = 0_u32;
    for item in &parent_hir.data_items {
        let root_id = primary_root_in_slice(item.id, &parent_hir.data_items);
        if global_roots.contains(&root_id) {
            let _ = id_map.insert(item.id, DataItemId::new(next_idx));
            next_idx = next_idx.saturating_add(1);
        }
    }

    let mut layout = SolvedDataLayout::default();
    for item in &parent_hir.data_items {
        let Some(&new_id) = id_map.get(&item.id) else {
            continue;
        };
        let orig_root = primary_root_in_slice(item.id, &parent_hir.data_items);
        let new_sec = match item.section {
            HirStorageSection::InheritedGlobal { .. }
            | HirStorageSection::ExternalWorkingStorage { .. } => item.section,
            _ => HirStorageSection::InheritedGlobal {
                owner_unit: parent_hir.id,
                owner_root: orig_root,
            },
        };
        layout.items.push(HirDataItem {
            id: new_id,
            scope: child_future_scope,
            name: item.name,
            level: item.level,
            parent: item.parent.and_then(|p| id_map.get(&p).copied()),
            children: item
                .children
                .iter()
                .filter_map(|c| id_map.get(c).copied())
                .collect(),
            section: new_sec,
            ty: item.ty.clone(),
            byte_offset: item.byte_offset,
            total_byte_size: item.total_byte_size,
            dimensions: remap_inherited_dims(&item.dimensions, &id_map),
            redefines: item.redefines.and_then(|r| id_map.get(&r).copied()),
            initial_value: item.initial_value.clone(),
            span: item.span,
        });
    }

    let c88_map = remap_inherited_conditions(parent_hir, &id_map, &mut layout);
    let fc_map = remap_inherited_files(parent_hir, global_files, &id_map, &mut layout);

    let inh_scope = symtab.create_scope(None);
    for (sym, cand) in global_cands {
        let mapped_target = match cand.target {
            ResolvedSymbol::DataItem(old_id) => {
                ResolvedSymbol::DataItem(id_map.get(&old_id).copied().unwrap_or(old_id))
            }
            ResolvedSymbol::Condition88(old_idx) => {
                ResolvedSymbol::Condition88(c88_map.get(&old_idx).copied().unwrap_or(old_idx))
            }
            ResolvedSymbol::File(old_fid) => {
                ResolvedSymbol::File(fc_map.get(&old_fid).copied().unwrap_or(old_fid))
            }
            other => other,
        };
        symtab.insert_symbol(
            inh_scope,
            sym,
            SymbolCandidate {
                target: mapped_target,
                qualifiers: cand.qualifiers,
                visibility: GlobalVisibility::Global,
            },
        );
    }

    (inh_scope, layout)
}

/// Remaps inherited `88`-level condition entries attached to `GLOBAL` data items.
///
/// # Arguments
///
/// * `parent_hir` - Enclosing parent [`HirProgram`].
/// * `id_map` - Parent-to-child [`DataItemId`] map.
/// * `layout` - Child [`SolvedDataLayout`] being populated.
fn remap_inherited_conditions(
    parent_hir: &HirProgram,
    id_map: &BTreeMap<DataItemId, DataItemId>,
    layout: &mut SolvedDataLayout,
) -> BTreeMap<usize, usize> {
    let mut c88_map = BTreeMap::new();
    for (old_c88, c88) in parent_hir.conditions_88.iter().enumerate() {
        if let Some(&new_parent) = id_map.get(&c88.parent_item) {
            let new_c88 = layout.conditions_88.len();
            layout.conditions_88.push(HirCondition88 {
                name: c88.name,
                parent_item: new_parent,
                ranges: c88.ranges.clone(),
                false_value: c88.false_value.clone(),
                span: c88.span,
            });
            let _ = c88_map.insert(old_c88, new_c88);
        }
    }
    c88_map
}

/// Remaps inherited `GLOBAL` file connectors into `layout.inherited_files` and `layout.fd_bindings`.
///
/// # Arguments
///
/// * `parent_hir` - Enclosing parent [`HirProgram`].
/// * `global_files` - Set of parent `GLOBAL` [`coblang_common::FileConnectorId`]s.
/// * `id_map` - Parent-to-child [`DataItemId`] map.
/// * `layout` - Child [`SolvedDataLayout`] being populated.
fn remap_inherited_files(
    parent_hir: &HirProgram,
    global_files: BTreeSet<coblang_common::FileConnectorId>,
    id_map: &BTreeMap<DataItemId, DataItemId>,
    layout: &mut SolvedDataLayout,
) -> BTreeMap<coblang_common::FileConnectorId, coblang_common::FileConnectorId> {
    let mut fc_map = BTreeMap::new();
    for old_fid in global_files {
        if let Some(fc) = parent_hir.files.get(old_fid.as_u32() as usize) {
            let new_fid = coblang_common::FileConnectorId::new(
                u32::try_from(layout.inherited_files.len()).unwrap_or(0),
            );
            let mapped_fc = crate::m14_fileio::remap_inherited_file_connector(fc, new_fid, id_map);
            layout.fd_bindings.push((
                mapped_fc.name,
                new_fid,
                mapped_fc.records.clone(),
                mapped_fc.max_record_bytes,
                mapped_fc.span,
            ));
            layout.inherited_files.push(mapped_fc);
            let _ = fc_map.insert(old_fid, new_fid);
        }
    }
    fc_map
}

/// Lowers `Stmt::Call` into a strongly-typed [`HirStmt::Call`] (`HirCallSpec`) preserving
/// `BY REFERENCE` / `BY CONTENT` / `BY VALUE` / `OMITTED`, `RETURNING`, and `ON EXCEPTION` handlers.
///
/// # Arguments
///
/// * `call_sig` - `(callee, args, returning)` tuple.
/// * `handlers` - `ON EXCEPTION` / `NOT ON EXCEPTION` handlers.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_m11_call_stmt(
    call_sig: (&Expr, &[CallArg], &CallReturning),
    handlers: &EventHandlers,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let (callee, args, returning) = call_sig;
    let (c_hir, _) = lower_expr_with_cat(callee, ctx);
    let lowered_args = args
        .iter()
        .map(|arg| match arg {
            CallArg::Omitted(_) => HirCallArg::Omitted,
            CallArg::Value { mode, expr, .. } => {
                let (e_hir, _) = lower_expr_with_cat(expr, ctx);
                match mode {
                    PassingMode::ByValue => HirCallArg::ByValue(e_hir),
                    PassingMode::ByContent => HirCallArg::ByContent(e_hir),
                    PassingMode::ByReference => match e_hir {
                        HirExpr::Place(pl) => HirCallArg::ByReference(pl),
                        other => HirCallArg::ByContent(other),
                    },
                }
            }
        })
        .collect();
    let ret = match returning {
        CallReturning::Implicit => HirCallReturning::ImplicitReturnCode,
        CallReturning::Omitted => HirCallReturning::Omitted,
        CallReturning::Target(r) => resolve_place(r, ctx).map_or(
            HirCallReturning::ImplicitReturnCode,
            HirCallReturning::Place,
        ),
    };
    let (on_exception, not_on_exception) = lower_event_handlers(handlers, ctx);
    out.push(HirStmt::Call(Box::new(HirCallSpec {
        callee: c_hir,
        args: lowered_args,
        returning: ret,
        on_exception,
        not_on_exception,
        span,
    })));
}

/// Lowers `Stmt::Cancel` into [`HirStmt::Cancel`].
///
/// # Arguments
///
/// * `targets` - Target module expressions.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_cancel_stmt(
    targets: &[Expr],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let lowered = targets
        .iter()
        .map(|t| lower_expr_with_cat(t, ctx).0)
        .collect();
    out.push(HirStmt::Cancel {
        targets: lowered,
        span,
    });
}
