//! Semantic validation, connector layout resolution, `DECLARATIVES` matching, and HIR lowering
//! for COBOL File I/O (`M14`), `INDEXED` ISAM (`M15`), and `SORT`/`MERGE` (`M16`).

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{
    DataItemId, FileConnectorId, LevelNumber, ProcedureId, ProgramUnitId, ScopeId, SourceSpan,
    SymbolId,
};
use coblang_diag::{
    AssignClauseDefault, Diagnostic, DiagnosticCode, DiagnosticSink, DialectConfig, SymbolInterner,
};
use coblang_hir::{
    DataCategory, DynamicOdoPresence, HirAdvancingSide, HirAlphabetTable, HirAltKeySpec,
    HirAltKeySuppression, HirAssignSpec, HirCloseDisposition, HirConnectorKind,
    HirConnectorSharing, HirDataItem, HirDeclRef, HirDeclarativeFileKey, HirDeclarativeRule,
    HirDeclarativeTrigger, HirDeletePhysicalEntry, HirDuplicateKeyPolicy, HirExpr,
    HirFileAccessMode, HirFileCloseEntry, HirFileCloseStmt, HirFileConnector, HirFileDeclSet,
    HirFileDeletePhysicalStmt, HirFileDeleteStmt, HirFileLockMode, HirFileOpenEntry,
    HirFileOpenStmt, HirFileOptionality, HirFileOrganization, HirFileReadStmt, HirFileRewriteStmt,
    HirFileSharingMode, HirFileStartStmt, HirFileUnlockStmt, HirFileWriteStmt, HirGlobalFlag,
    HirInlineClausePresence, HirKeySegment, HirLinageClause, HirLinageParam, HirMergeFileStmt,
    HirOpenMode, HirOpenOption, HirPlace, HirPrintAdvancingMode, HirReadLockPolicy, HirReadMode,
    HirRecordFormat, HirReleaseStmt, HirReturnStmt, HirSeqReadDir, HirSortDirection,
    HirSortDuplicates, HirSortFileStmt, HirSortInput, HirSortKeyDesc, HirSortOutput,
    HirSortTransferFile, HirStartMode, HirStartRelation, HirStmt, HirType, HirWriteAdvancing,
    HirWriteHandlerKind,
};
use coblang_syntax::ast::{
    AccessMode, AltKeySuppression, AssignModifier, AssignTarget, BoundSide, CloseDispositionSpec,
    CorrespondingMode, DataDivision, DataRef, DeclarativeSection, DeleteKind, DuplicatePolicy,
    EnvironmentDivision, EventHandlers, Expr, ExternalLinkage, FdOrSd, FdSdEntry, FileControlEntry,
    FileOrganization, GlobalVisibility, KeyOrder, LinageSpec, LockModeSpec, OpenMode,
    OpenOptionSpec, QualifiedName, ReadDirection, ReadLockSpec, RecordSizeClause, RelOp,
    SameAreaKind, SelectOptionality, SharingModeSpec, StartPositionSpec, Stmt, TruthValue,
    UseClause, WriteHandlerKind, WriteTargetMode,
};

use crate::layout::SolvedDataLayout;
use crate::lower_hir::{
    UnitLowerCtx, lower_event_handlers, lower_expr_with_cat, lower_move_stmt, lower_stmt_slice,
    resolve_place, simple_place_for_item,
};
use crate::m8_flow::{const_int_expr, resolve_perform_proc_bounds};
use crate::symbols::{ResolvedSymbol, SymbolTable};

/// Shared environment arguments for resolving `FILE-CONTROL` and `FD`/`SD` entries.
pub struct FileResolveEnv<'a> {
    /// Owning compilation unit handle.
    pub unit_id: ProgramUnitId,
    /// Active lexical scope.
    pub scope: ScopeId,
    /// Solved Data Division layout.
    pub layout: &'a SolvedDataLayout,
    /// Active dialect configuration.
    pub dialect: &'a DialectConfig,
    /// Declared `SPECIAL-NAMES` alphabets.
    pub alphabets: &'a [HirAlphabetTable],
    /// Optional `PROGRAM COLLATING SEQUENCE` index.
    pub prog_collating_seq: Option<usize>,
    /// Shared symbol table.
    pub symtab: &'a SymbolTable,
    /// Shared symbol interner.
    pub interner: &'a SymbolInterner,
}

/// Validates all `SELECT` + `FD`/`SD` declarations and resolves [`HirFileConnector`]s for `unit_id`.
///
/// # Arguments
///
/// * `env_div` - Parsed `ENVIRONMENT DIVISION`.
/// * `data_div` - Parsed `DATA DIVISION`.
/// * `fenv` - Shared file resolution lookup context.
/// * `items_mut` - Mutable program data-item arena (updated for `SAME RECORD AREA` aliasing).
/// * `sink` - Diagnostic sink.
pub fn resolve_unit_file_connectors(
    env_div: &EnvironmentDivision,
    data_div: &DataDivision,
    fenv: &FileResolveEnv<'_>,
    items_mut: &mut [HirDataItem],
    sink: &mut DiagnosticSink,
) -> Vec<HirFileConnector> {
    let mut connectors = fenv.layout.inherited_files.clone();
    let mut seen_selects = BTreeSet::new();

    for sel in &env_div.input_output.file_control {
        let fallback_idx = u32::try_from(connectors.len()).unwrap_or(0);
        let fc =
            resolve_single_connector(sel, data_div, fallback_idx, &mut seen_selects, fenv, sink);
        connectors.push(fc);
    }

    for fd in &data_div.file_section {
        if !seen_selects.contains(&fd.file_name) {
            let lbl = fenv
                .interner
                .resolve_original(fd.file_name)
                .unwrap_or("file");
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(fd.span),
                format!("`FD`/`SD` `{lbl}` has no corresponding `SELECT` entry in `FILE-CONTROL`"),
            ));
        }
    }

    apply_same_record_areas(
        &env_div.input_output.io_control.same_areas,
        &mut connectors,
        items_mut,
    );
    connectors
}

/// Resolves a single `SELECT` entry alongside its matching `FD`/`SD` entry.
///
/// # Arguments
///
/// * `sel` - AST `SELECT` entry.
/// * `data_div` - AST `DATA DIVISION`.
/// * `fallback_idx` - Fallback connector index when `FD`/`SD` is missing.
/// * `seen_selects` - Duplicate `SELECT` tracker.
/// * `fenv` - Shared file resolution lookup context.
/// * `sink` - Diagnostic sink.
fn resolve_single_connector(
    sel: &FileControlEntry,
    data_div: &DataDivision,
    fallback_idx: u32,
    seen_selects: &mut BTreeSet<SymbolId>,
    fenv: &FileResolveEnv<'_>,
    sink: &mut DiagnosticSink,
) -> HirFileConnector {
    let ast_fd = data_div
        .file_section
        .iter()
        .find(|fd| fd.file_name == sel.file_name);
    let kind = match ast_fd.map(|f| f.kind) {
        Some(FdOrSd::Sd) => HirConnectorKind::Sd,
        Some(FdOrSd::Fd) | None => HirConnectorKind::Fd,
    };
    let is_dup_select = validate_select_entry_rules(sel, kind, seen_selects, fenv, sink);
    let (fc_id, records, max_rec_from_01) =
        resolve_select_fd_binding(sel, is_dup_select, fallback_idx, fenv, sink);

    let sharing = resolve_connector_sharing(sel.file_name, fc_id, ast_fd, fenv);
    let (assign, is_printer_assign) = resolve_assign_spec(sel, fenv, sink);
    let has_reports = ast_fd.is_some_and(|fd| !fd.reports.is_empty());
    let is_pure_report_fd = has_reports && records.is_empty();
    let (mut record_format, mut min_record_bytes, mut max_record_bytes, depending_on_item) =
        resolve_record_framing(ast_fd, &records, max_rec_from_01, sel.span, fenv, sink);
    let organization = if is_pure_report_fd {
        HirFileOrganization::LineSequential
    } else {
        map_organization(sel.organization)
    };
    if is_pure_report_fd || (has_reports && organization == HirFileOrganization::LineSequential) {
        record_format = HirRecordFormat::Varying;
        min_record_bytes = 0;
        max_record_bytes = max_record_bytes.max(4096);
    }

    let (status_item, secondary_status_item) = resolve_status_items(sel, fenv, sink);
    let relative_key_item = sel
        .relative_key
        .as_ref()
        .and_then(|p| resolve_path_item(p, sel.span, fenv, sink));

    let (primary_split_name, primary_key_items, primary_key_segments) =
        resolve_primary_key(sel, fenv, sink);
    let alternate_keys = resolve_alternate_keys(sel, fenv, sink);
    let linage = ast_fd
        .and_then(|fd| fd.linage.as_ref())
        .and_then(|lin| resolve_linage_clause(fc_id, lin, fenv, sink));

    let print_mode = if is_printer_assign || linage.is_some() || has_reports {
        HirPrintAdvancingMode::LineAdvancing
    } else {
        HirPrintAdvancingMode::RawBinary
    };
    let padding_byte = resolve_padding_byte(sel.padding_char, fenv.interner);
    let collating_alphabet = resolve_collation_index(sel.collating_sequence, fenv);

    HirFileConnector {
        id: fc_id,
        name: sel.file_name,
        kind,
        sharing,
        record_area_group: fc_id,
        organization,
        access_mode: map_access_mode(sel.access_mode),
        optionality: match sel.optionality {
            SelectOptionality::Required => HirFileOptionality::Required,
            SelectOptionality::Optional => HirFileOptionality::Optional,
        },
        assign,
        record_format,
        min_record_bytes,
        max_record_bytes,
        depending_on_item,
        records,
        status_item,
        secondary_status_item,
        relative_key_item,
        primary_split_name,
        primary_key_items,
        primary_key_segments,
        alternate_keys,
        lock_mode: match sel.lock_mode {
            None => HirFileLockMode::Default,
            Some(LockModeSpec::Manual) => HirFileLockMode::Manual,
            Some(LockModeSpec::Automatic) => HirFileLockMode::Automatic,
            Some(LockModeSpec::Exclusive) => HirFileLockMode::Exclusive,
        },
        sharing_mode: sel.sharing_mode.map(|s| match s {
            SharingModeSpec::AllOther => HirFileSharingMode::AllOther,
            SharingModeSpec::NoOther => HirFileSharingMode::NoOther,
            SharingModeSpec::ReadOnly => HirFileSharingMode::ReadOnly,
        }),
        padding_byte,
        print_mode,
        linage,
        collating_alphabet,
        span: sel.span,
    }
}

/// Resolves the `FILE SECTION` `FD`/`SD` layout binding for a `SELECT` clause, or emits `E0007`.
///
/// # Arguments
///
/// * `sel` - `SELECT` file-control AST entry.
/// * `is_dup_select` - Whether `sel` was already flagged as a duplicate `SELECT`.
/// * `fallback_idx` - Fallback [`FileConnectorId`] index when `FD`/`SD` is absent.
/// * `fenv` - File resolution lookup context.
/// * `sink` - Diagnostic sink.
fn resolve_select_fd_binding(
    sel: &FileControlEntry,
    is_dup_select: bool,
    fallback_idx: u32,
    fenv: &FileResolveEnv<'_>,
    sink: &mut DiagnosticSink,
) -> (FileConnectorId, Vec<DataItemId>, u32) {
    let fd_binding = (!is_dup_select)
        .then(|| {
            fenv.layout
                .fd_bindings
                .iter()
                .find(|(fname, ..)| *fname == sel.file_name)
        })
        .flatten();
    if let Some((_, id, recs, max_b, _)) = fd_binding {
        (*id, recs.clone(), *max_b)
    } else {
        let lbl = fenv
            .interner
            .resolve_original(sel.file_name)
            .unwrap_or("file");
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(sel.span),
            format!("missing `FD` or `SD` entry in `FILE SECTION` for selected file `{lbl}`"),
        ));
        (FileConnectorId::new(fallback_idx), Vec::new(), 0)
    }
}

/// Emits semantic diagnostics for duplicate `SELECT`, duplicate `ORGANIZATION`, missing `ASSIGN`, or missing keys.
///
/// # Arguments
///
/// * `sel` - AST `SELECT` entry.
/// * `kind` - `FD` vs `SD` classification.
/// * `seen_selects` - Set of previously seen `SELECT` file symbols.
/// * `fenv` - Shared file resolution context.
/// * `sink` - Diagnostic sink.
fn validate_select_entry_rules(
    sel: &FileControlEntry,
    kind: HirConnectorKind,
    seen_selects: &mut BTreeSet<SymbolId>,
    fenv: &FileResolveEnv<'_>,
    sink: &mut DiagnosticSink,
) -> bool {
    let is_dup = !seen_selects.insert(sel.file_name);
    if is_dup {
        let lbl = fenv
            .interner
            .resolve_original(sel.file_name)
            .unwrap_or("file");
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(sel.span),
            format!("duplicate `SELECT` clause for file `{lbl}`"),
        ));
    }
    if sel.organization_clause_count > 1 {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(sel.span),
            String::from("duplicate `ORGANIZATION` clause in `SELECT` entry"),
        ));
    }
    if sel.assign_targets.is_empty() && kind == HirConnectorKind::Fd {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(sel.span),
            String::from("`SELECT` entry requires an `ASSIGN TO` clause"),
        ));
    }
    if sel.organization == FileOrganization::Indexed && sel.record_key.is_none() {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(sel.span),
            String::from("`ORGANIZATION IS INDEXED` requires a `RECORD KEY` clause"),
        ));
    }
    if sel.organization == FileOrganization::Relative
        && matches!(sel.access_mode, AccessMode::Random | AccessMode::Dynamic)
        && sel.relative_key.is_none()
    {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(sel.span),
            String::from("`RELATIVE` file with `RANDOM`/`DYNAMIC` access requires `RELATIVE KEY`"),
        ));
    }
    is_dup
}

/// Resolves and validates `FILE STATUS` (2-byte requirement) and optional secondary status items.
///
/// # Arguments
///
/// * `sel` - AST `SELECT` entry.
/// * `fenv` - Shared file resolution context.
/// * `sink` - Diagnostic sink.
fn resolve_status_items(
    sel: &FileControlEntry,
    fenv: &FileResolveEnv<'_>,
    sink: &mut DiagnosticSink,
) -> (Option<DataItemId>, Option<DataItemId>) {
    let status_item = sel
        .file_status
        .as_ref()
        .and_then(|p| resolve_path_item(p, sel.span, fenv, sink));
    if let Some(st_id) = status_item
        && let Some(st_it) = fenv.layout.items.get(st_id.as_u32() as usize)
        && st_it.total_byte_size != 2
    {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(sel.span),
            String::from("`FILE STATUS` data item must be 2 bytes in length"),
        ));
    }
    let secondary_status_item = sel
        .secondary_status
        .as_ref()
        .and_then(|p| resolve_path_item(p, sel.span, fenv, sink));
    (status_item, secondary_status_item)
}

/// Resolves [`HirConnectorSharing`] (`UnitOwned` vs `External`) for an `FD`/`SD`.
///
/// # Arguments
///
/// * `file_name` - Internal file symbol.
/// * `fc_id` - Assigned local [`FileConnectorId`].
/// * `ast_fd` - Optional matching [`FdSdEntry`].
/// * `fenv` - Shared file resolution context.
fn resolve_connector_sharing(
    file_name: SymbolId,
    fc_id: FileConnectorId,
    ast_fd: Option<&FdSdEntry>,
    fenv: &FileResolveEnv<'_>,
) -> HirConnectorSharing {
    if let Some(ExternalLinkage::External(ext_opt)) = ast_fd.map(|fd| &fd.external) {
        let ext_sym = ext_opt.unwrap_or(file_name);
        let ext_name = fenv
            .interner
            .resolve_original(ext_sym)
            .unwrap_or("EXTERNAL_FILE")
            .to_ascii_uppercase();
        HirConnectorSharing::External {
            external_name: ext_name,
        }
    } else {
        HirConnectorSharing::UnitOwned {
            owner_unit: fenv.unit_id,
            owner_fc: fc_id,
        }
    }
}

/// Resolves the `ASSIGN TO` target list into [`HirAssignSpec`] plus a printer hint flag.
///
/// # Arguments
///
/// * `sel` - AST `SELECT` entry.
/// * `fenv` - Shared file resolution context.
/// * `sink` - Diagnostic sink.
fn resolve_assign_spec(
    sel: &FileControlEntry,
    fenv: &FileResolveEnv<'_>,
    sink: &mut DiagnosticSink,
) -> (HirAssignSpec, bool) {
    let Some(first) = sel.assign_targets.first() else {
        let fallback = fenv
            .interner
            .resolve_original(sel.file_name)
            .unwrap_or("SORTWORK");
        return (HirAssignSpec::EnvOrLiteral(String::from(fallback)), false);
    };
    match first {
        AssignTarget::Literal(sym) => {
            let txt = fenv.interner.resolve_original(*sym).unwrap_or("");
            (HirAssignSpec::Literal(String::from(txt)), false)
        }
        AssignTarget::Device(sym) => {
            let canon = fenv.interner.resolve_canonical(*sym).unwrap_or("");
            if canon == "KEYBOARD" {
                (HirAssignSpec::KeyboardDevice, false)
            } else {
                (HirAssignSpec::DisplayDevice, true)
            }
        }
        AssignTarget::Name { sym, modifier } => {
            let is_printer = *modifier == AssignModifier::Printer;
            let orig = fenv.interner.resolve_original(*sym).unwrap_or("");
            match modifier {
                AssignModifier::External => (HirAssignSpec::ExternalEnv(String::from(orig)), false),
                AssignModifier::Dynamic => {
                    if let Some(did) = resolve_path_item(&[*sym], sel.span, fenv, sink) {
                        (HirAssignSpec::DynamicItem(did), false)
                    } else {
                        (HirAssignSpec::EnvOrLiteral(String::from(orig)), false)
                    }
                }
                AssignModifier::Printer => {
                    if matches!(
                        fenv.dialect.assign_clause,
                        AssignClauseDefault::Dynamic | AssignClauseDefault::MicroFocus
                    ) && let Some(ResolvedSymbol::DataItem(did)) =
                        fenv.symtab.resolve_symbol_silent(fenv.scope, *sym, &[])
                    {
                        (HirAssignSpec::DynamicItem(did), true)
                    } else {
                        (HirAssignSpec::EnvOrLiteral(String::from(orig)), true)
                    }
                }
                AssignModifier::Unspecified | AssignModifier::Disk => {
                    if matches!(
                        fenv.dialect.assign_clause,
                        AssignClauseDefault::Dynamic | AssignClauseDefault::MicroFocus
                    ) && let Some(ResolvedSymbol::DataItem(did)) =
                        fenv.symtab.resolve_symbol_silent(fenv.scope, *sym, &[])
                    {
                        (HirAssignSpec::DynamicItem(did), is_printer)
                    } else {
                        (HirAssignSpec::EnvOrLiteral(String::from(orig)), is_printer)
                    }
                }
            }
        }
    }
}

/// Computes `(record_format, min_bytes, max_bytes, depending_on)` for an `FD`/`SD`.
///
/// # Arguments
///
/// * `ast_fd` - Optional AST `FD`/`SD` entry.
/// * `records` - Associated `01` record `DataItemId`s.
/// * `max_from_01` - Maximum static byte size across `01` records.
/// * `span` - Diagnostic span.
/// * `fenv` - Shared file resolution context.
/// * `sink` - Diagnostic sink.
fn resolve_record_framing(
    ast_fd: Option<&FdSdEntry>,
    records: &[DataItemId],
    max_from_01: u32,
    span: SourceSpan,
    fenv: &FileResolveEnv<'_>,
    sink: &mut DiagnosticSink,
) -> (HirRecordFormat, u32, u32, Option<DataItemId>) {
    let mut min_01 = u32::MAX;
    let mut has_odo_01 = false;
    let mut min_odo_01 = u32::MAX;
    for &rid in records {
        if let Some(it) = fenv.layout.items.get(rid.as_u32() as usize) {
            min_01 = min_01.min(it.total_byte_size.max(1));
            if let Some((odo_min, _)) = compute_record_odo_min_bytes(it, &fenv.layout.items) {
                has_odo_01 = true;
                min_odo_01 = min_odo_01.min(odo_min.max(1));
            }
        }
    }
    let eff_max_01 = max_from_01.max(1);
    let eff_min_01 = if has_odo_01 {
        min_odo_01.min(min_01).min(eff_max_01)
    } else if min_01 == u32::MAX {
        eff_max_01
    } else {
        min_01
    };

    match ast_fd.and_then(|fd| fd.record_size.as_ref()) {
        Some(RecordSizeClause::Varying {
            min_bytes,
            max_bytes,
            depending_on,
        }) => {
            let dep_id = depending_on
                .as_ref()
                .and_then(|p| resolve_path_item(p, span, fenv, sink));
            let lo = min_bytes.unwrap_or(eff_min_01).max(1);
            let hi = max_bytes.unwrap_or(eff_max_01).max(lo).max(eff_max_01);
            (HirRecordFormat::Varying, lo, hi, dep_id)
        }
        Some(RecordSizeClause::Contains {
            min_bytes,
            max_bytes: Some(hi_decl),
        }) => {
            let lo = (*min_bytes).max(1);
            let hi = (*hi_decl).max(lo).max(eff_max_01);
            let fmt = if lo < hi || has_odo_01 {
                HirRecordFormat::Varying
            } else {
                HirRecordFormat::Fixed
            };
            (fmt, lo, hi, None)
        }
        Some(RecordSizeClause::Contains {
            min_bytes,
            max_bytes: None,
        }) => {
            let hi = (*min_bytes).max(eff_max_01);
            let fmt = if eff_min_01 < hi || has_odo_01 {
                HirRecordFormat::Varying
            } else {
                HirRecordFormat::Fixed
            };
            let lo = if fmt == HirRecordFormat::Varying {
                eff_min_01
            } else {
                hi
            };
            (fmt, lo, hi, None)
        }
        None => {
            let fmt = if eff_min_01 < eff_max_01 || has_odo_01 {
                HirRecordFormat::Varying
            } else {
                HirRecordFormat::Fixed
            };
            (fmt, eff_min_01, eff_max_01, None)
        }
    }
}

/// Computes `Some((min_bytes, max_bytes))` when a `01` record contains a subordinate `OCCURS DEPENDING ON`.
///
/// # Arguments
///
/// * `rec_item` - Root `01` [`HirDataItem`].
/// * `items` - Program data-item arena.
fn compute_record_odo_min_bytes(
    rec_item: &HirDataItem,
    items: &[HirDataItem],
) -> Option<(u32, u32)> {
    let mut stack = rec_item.children.clone();
    while let Some(cid) = stack.pop() {
        let Some(child) = items.get(cid.as_u32() as usize) else {
            continue;
        };
        if let Some(dim) = child.dimensions.last()
            && dim.depending_on.is_some()
        {
            let min_tail = dim.min.saturating_mul(dim.element_byte_stride);
            let min_total = record_relative_offset(child, items).saturating_add(min_tail);
            return Some((min_total, rec_item.total_byte_size));
        }
        stack.extend_from_slice(&child.children);
    }
    None
}

/// Computes the 0-based byte offset of `item` relative to its enclosing level-`01` record root.
///
/// # Arguments
///
/// * `item` - Resolved key segment [`HirDataItem`].
/// * `items` - Program data-item arena.
fn record_relative_offset(item: &HirDataItem, items: &[HirDataItem]) -> u32 {
    let mut cur = item;
    while let Some(pid) = cur.parent {
        let Some(parent_it) = items.get(pid.as_u32() as usize) else {
            break;
        };
        cur = parent_it;
    }
    item.byte_offset
        .as_u32()
        .saturating_sub(cur.byte_offset.as_u32())
}

/// Resolves primary `RECORD KEY` items and byte segments within the `FD` record area.
///
/// # Arguments
///
/// * `sel` - AST `SELECT` entry.
/// * `fenv` - Shared file resolution context.
/// * `sink` - Diagnostic sink.
fn resolve_primary_key(
    sel: &FileControlEntry,
    fenv: &FileResolveEnv<'_>,
    sink: &mut DiagnosticSink,
) -> (Option<SymbolId>, Vec<DataItemId>, Vec<HirKeySegment>) {
    let mut items = Vec::new();
    let mut segs = Vec::new();
    let Some(rk) = &sel.record_key else {
        return (None, items, segs);
    };
    for seg_path in &rk.segments {
        if let Some(kid) = resolve_path_item(seg_path, rk.span, fenv, sink) {
            items.push(kid);
            if let Some(kit) = fenv.layout.items.get(kid.as_u32() as usize) {
                segs.push(HirKeySegment {
                    offset: record_relative_offset(kit, &fenv.layout.items),
                    length: kit.total_byte_size.max(1),
                });
            }
        }
    }
    (rk.split_name, items, segs)
}

/// Resolves `ALTERNATE RECORD KEY` clauses into [`HirAltKeySpec`]s.
///
/// # Arguments
///
/// * `sel` - AST `SELECT` entry.
/// * `fenv` - Shared file resolution context.
/// * `sink` - Diagnostic sink.
fn resolve_alternate_keys(
    sel: &FileControlEntry,
    fenv: &FileResolveEnv<'_>,
    sink: &mut DiagnosticSink,
) -> Vec<HirAltKeySpec> {
    let mut out = Vec::with_capacity(sel.alternate_keys.len());
    for ak in &sel.alternate_keys {
        let mut key_items = Vec::new();
        let mut segments = Vec::new();
        for seg_path in &ak.segments {
            if let Some(kid) = resolve_path_item(seg_path, ak.span, fenv, sink) {
                key_items.push(kid);
                if let Some(kit) = fenv.layout.items.get(kid.as_u32() as usize) {
                    segments.push(HirKeySegment {
                        offset: record_relative_offset(kit, &fenv.layout.items),
                        length: kit.total_byte_size.max(1),
                    });
                }
            }
        }
        let total_len: u32 = segments.iter().map(|s| s.length).sum();
        let suppression = ak
            .suppression
            .map(|sup| resolve_suppression(sup, total_len.max(1), fenv.interner));
        out.push(HirAltKeySpec {
            split_name: ak.split_name,
            key_items,
            segments,
            duplicates: match ak.duplicates {
                DuplicatePolicy::Unique => HirDuplicateKeyPolicy::Unique,
                DuplicatePolicy::WithDuplicates => HirDuplicateKeyPolicy::WithDuplicates,
            },
            suppression,
        });
    }
    out
}

/// Resolves `SUPPRESS WHEN [ALL] lit` into [`HirAltKeySuppression`].
///
/// # Arguments
///
/// * `sup` - AST suppression clause.
/// * `key_len` - Total byte length of the alternate key.
/// * `interner` - Symbol interner.
fn resolve_suppression(
    sup: AltKeySuppression,
    key_len: u32,
    interner: &SymbolInterner,
) -> HirAltKeySuppression {
    match sup {
        AltKeySuppression::WhenAll(sym) => {
            let b = resolve_single_figurative_or_char(sym, interner);
            HirAltKeySuppression::WhenAllByte(b)
        }
        AltKeySuppression::WhenLiteral(sym) => {
            let canon = interner.resolve_canonical(sym).unwrap_or("");
            if let Some(fig_b) = figurative_fill_byte(canon) {
                HirAltKeySuppression::WhenAllByte(fig_b)
            } else {
                let raw = interner.resolve_original(sym).unwrap_or("");
                let mut padded = vec![b' '; key_len as usize];
                for (dst, &src) in padded.iter_mut().zip(raw.as_bytes()) {
                    *dst = src;
                }
                HirAltKeySuppression::WhenExactBytes(padded)
            }
        }
    }
}

/// Resolves `LINAGE` AST parameters and attaches the per-FD `LINAGE-COUNTER` register.
///
/// # Arguments
///
/// * `fc_id` - Target [`FileConnectorId`].
/// * `lin` - AST [`LinageSpec`].
/// * `fenv` - Shared file resolution context.
/// * `sink` - Diagnostic sink.
fn resolve_linage_clause(
    fc_id: FileConnectorId,
    lin: &LinageSpec,
    fenv: &FileResolveEnv<'_>,
    sink: &mut DiagnosticSink,
) -> Option<HirLinageClause> {
    let counter_item = fenv
        .layout
        .fd_linage_counters
        .iter()
        .find_map(|&(fid, cid)| (fid == fc_id).then_some(cid))?;
    let body = resolve_linage_param(&lin.body_lines, lin.span, fenv, sink)
        .unwrap_or(HirLinageParam::Static(60));
    let footing = lin
        .footing_at
        .as_ref()
        .and_then(|p| resolve_linage_param(p, lin.span, fenv, sink));
    let top = lin
        .lines_at_top
        .as_ref()
        .and_then(|p| resolve_linage_param(p, lin.span, fenv, sink));
    let bottom = lin
        .lines_at_bottom
        .as_ref()
        .and_then(|p| resolve_linage_param(p, lin.span, fenv, sink));
    Some(HirLinageClause {
        body,
        footing,
        top,
        bottom,
        counter_item,
    })
}

/// Resolves a `LINAGE` parameter (`Vec<SymbolId>`) as an integer literal or `DataItemId`.
///
/// # Arguments
///
/// * `syms` - Parsed integer token or qualified data-item path.
/// * `span` - Clause span.
/// * `fenv` - Shared file resolution context.
/// * `sink` - Diagnostic sink.
fn resolve_linage_param(
    syms: &[SymbolId],
    span: SourceSpan,
    fenv: &FileResolveEnv<'_>,
    sink: &mut DiagnosticSink,
) -> Option<HirLinageParam> {
    let &first = syms.first()?;
    if syms.len() == 1
        && let Ok(raw) = fenv.interner.resolve_original(first)
        && let Ok(n) = raw.parse::<u32>()
    {
        return Some(HirLinageParam::Static(n));
    }
    resolve_path_item(syms, span, fenv, sink).map(HirLinageParam::Dynamic)
}

/// Links `01` records of files sharing a `SAME RECORD AREA` clause onto the canonical group root,
/// rebasing their `byte_offset`s onto `r0.byte_offset` so all member records occupy identical bytes.
///
/// # Arguments
///
/// * `same_areas` - AST `SAME [RECORD] AREA` clauses.
/// * `connectors` - Resolved unit [`HirFileConnector`]s.
/// * `items_mut` - Mutable program data-item arena.
fn apply_same_record_areas(
    same_areas: &[coblang_syntax::ast::SameAreaClause],
    connectors: &mut [HirFileConnector],
    items_mut: &mut [HirDataItem],
) {
    for clause in same_areas {
        if clause.kind != SameAreaKind::RecordArea {
            continue;
        }
        let mut member_ids = Vec::new();
        for &fsym in &clause.files {
            if let Some(fid) = connectors.iter().find(|fc| fc.name == fsym).map(|fc| fc.id) {
                member_ids.push(fid);
            }
        }
        let Some(&canon_id) = member_ids.iter().min() else {
            continue;
        };
        let canon_rec0 = connectors
            .iter()
            .find(|fc| fc.id == canon_id)
            .and_then(|fc| fc.records.first().copied());
        let target_base = canon_rec0
            .and_then(|r0| items_mut.get(r0.as_u32() as usize))
            .map_or(0, |it| it.byte_offset.as_u32());
        for fc in connectors.iter_mut() {
            if member_ids.contains(&fc.id) {
                fc.record_area_group = canon_id;
                if let Some(r0) = canon_rec0 {
                    for &rid in &fc.records {
                        if rid != r0 {
                            rebase_record_subtree(rid, r0, target_base, items_mut);
                        }
                    }
                }
            }
        }
    }
}

/// Sets `rid.redefines = Some(r0)` and shifts `rid` plus all descendant items to start at `target_base`.
///
/// # Arguments
///
/// * `rid` - Aliased `01` record [`DataItemId`].
/// * `r0` - Canonical `01` record [`DataItemId`].
/// * `target_base` - Byte offset of `r0`.
/// * `items_mut` - Mutable program data-item arena.
fn rebase_record_subtree(
    rid: DataItemId,
    r0: DataItemId,
    target_base: u32,
    items_mut: &mut [HirDataItem],
) {
    let old_base = match items_mut.get_mut(rid.as_u32() as usize) {
        Some(rit) => {
            let ob = rit.byte_offset.as_u32();
            rit.redefines = Some(r0);
            ob
        }
        None => return,
    };
    let mut stack = vec![rid];
    while let Some(cid) = stack.pop() {
        let Some(it) = items_mut.get_mut(cid.as_u32() as usize) else {
            continue;
        };
        let rel = it.byte_offset.as_u32().saturating_sub(old_base);
        it.byte_offset = coblang_common::ByteOffset::new(target_base.saturating_add(rel));
        stack.extend_from_slice(&it.children);
    }
}

/// Resolves a `Vec<SymbolId>` qualified path to a [`DataItemId`].
///
/// # Arguments
///
/// * `path` - Leaf + ancestor qualifier symbols.
/// * `span` - Diagnostic span.
/// * `fenv` - Shared file resolution context.
/// * `sink` - Diagnostic sink.
fn resolve_path_item(
    path: &[SymbolId],
    span: SourceSpan,
    fenv: &FileResolveEnv<'_>,
    sink: &mut DiagnosticSink,
) -> Option<DataItemId> {
    let (&base, quals) = path.split_first()?;
    match fenv
        .symtab
        .resolve_symbol(fenv.scope, base, quals, span, fenv.interner, sink)?
    {
        ResolvedSymbol::DataItem(id) => Some(id),
        _ => None,
    }
}

/// Resolves an optional `COLLATING SEQUENCE` symbol to its index in `alphabets` (or `prog_collating_seq`).
///
/// # Arguments
///
/// * `col_sym` - Optional explicit alphabet symbol.
/// * `fenv` - Shared file resolution context.
fn resolve_collation_index(col_sym: Option<SymbolId>, fenv: &FileResolveEnv<'_>) -> Option<usize> {
    if let Some(sym) = col_sym {
        fenv.alphabets.iter().position(|a| a.name == sym)
    } else {
        fenv.prog_collating_seq
    }
}

/// Resolves `PADDING CHARACTER IS ...` to a single fill byte (`b' '` by default).
///
/// # Arguments
///
/// * `pad_sym` - Optional padding literal/figurative symbol.
/// * `interner` - Symbol interner.
fn resolve_padding_byte(pad_sym: Option<SymbolId>, interner: &SymbolInterner) -> u8 {
    pad_sym.map_or(b' ', |sym| resolve_single_figurative_or_char(sym, interner))
}

/// Resolves a figurative constant or 1-char literal symbol to its single ASCII byte.
///
/// # Arguments
///
/// * `sym` - Literal or figurative symbol.
/// * `interner` - Symbol interner.
fn resolve_single_figurative_or_char(sym: SymbolId, interner: &SymbolInterner) -> u8 {
    let canon = interner.resolve_canonical(sym).unwrap_or("");
    if let Some(b) = figurative_fill_byte(canon) {
        return b;
    }
    interner
        .resolve_original(sym)
        .ok()
        .and_then(|s| s.bytes().next())
        .unwrap_or(b' ')
}

/// Maps COBOL figurative constant canonical names to their single-byte ASCII value.
///
/// # Arguments
///
/// * `canon` - Uppercase canonical symbol string.
fn figurative_fill_byte(canon: &str) -> Option<u8> {
    match canon {
        "SPACE" | "SPACES" => Some(b' '),
        "ZERO" | "ZEROS" | "ZEROES" => Some(b'0'),
        "LOW-VALUE" | "LOW-VALUES" => Some(0x00),
        "HIGH-VALUE" | "HIGH-VALUES" => Some(0xFF),
        "QUOTE" | "QUOTES" => Some(b'"'),
        _ => None,
    }
}

/// Maps AST [`FileOrganization`] to [`HirFileOrganization`].
///
/// # Arguments
///
/// * `org` - AST organization.
const fn map_organization(org: FileOrganization) -> HirFileOrganization {
    match org {
        FileOrganization::Sequential => HirFileOrganization::Sequential,
        FileOrganization::LineSequential => HirFileOrganization::LineSequential,
        FileOrganization::Relative => HirFileOrganization::Relative,
        FileOrganization::Indexed => HirFileOrganization::Indexed,
    }
}

/// Maps AST [`AccessMode`] to [`HirFileAccessMode`].
///
/// # Arguments
///
/// * `acc` - AST access mode.
const fn map_access_mode(acc: AccessMode) -> HirFileAccessMode {
    match acc {
        AccessMode::Sequential => HirFileAccessMode::Sequential,
        AccessMode::Random => HirFileAccessMode::Random,
        AccessMode::Dynamic => HirFileAccessMode::Dynamic,
    }
}

/// Builds active [`HirDeclarativeRule`]s for a unit (local rules followed by inherited `GLOBAL` rules).
///
/// # Arguments
///
/// * `declaratives` - AST `DECLARATIVES` sections of the unit.
/// * `first_decl_pid` - Starting `ProcedureId` of the first declarative section.
/// * `inherited_rules` - `USE GLOBAL` rules inherited from enclosing programs.
/// * `files` - Resolved unit [`HirFileConnector`]s.
/// * `env_args` - `(unit_id, scope, symtab, interner, sink)`.
pub fn build_unit_declarative_rules(
    declaratives: &[DeclarativeSection],
    first_decl_pid: u32,
    inherited_rules: &[HirDeclarativeRule],
    files: &[HirFileConnector],
    env_args: (
        ProgramUnitId,
        ScopeId,
        &SymbolTable,
        &SymbolInterner,
        &mut DiagnosticSink,
    ),
) -> Vec<HirDeclarativeRule> {
    let (unit_id, scope, symtab, interner, sink) = env_args;
    let mut rules = Vec::new();
    let mut pid = first_decl_pid;

    for decl in declaratives {
        let para_cnt = u32::try_from(decl.paragraphs.len()).unwrap_or(0);
        let target = HirDeclRef {
            owner_unit: unit_id,
            proc_start: ProcedureId::new(pid),
            proc_end: ProcedureId::new(pid.saturating_add(para_cnt)),
        };
        pid = pid.saturating_add(1).saturating_add(para_cnt);

        let UseClause::AfterException { global, targets } = &decl.use_clause else {
            continue;
        };
        let visibility = match global {
            GlobalVisibility::Local => HirGlobalFlag::Local,
            GlobalVisibility::Global => HirGlobalFlag::Global,
        };
        if targets.len() == 1
            && let Some(&tsym) = targets.first()
            && let Some(mode) = parse_open_mode_word(interner.resolve_canonical(tsym).unwrap_or(""))
        {
            rules.push(HirDeclarativeRule {
                target,
                visibility,
                trigger: HirDeclarativeTrigger::OpenMode(mode),
            });
            continue;
        }

        let mut fkeys = Vec::new();
        for &tsym in targets {
            let canon = interner.resolve_canonical(tsym).unwrap_or("");
            if parse_open_mode_word(canon).is_some() || canon.starts_with("EC-") {
                continue;
            }
            if let Some(ResolvedSymbol::File(fid)) =
                symtab.resolve_symbol(scope, tsym, &[], decl.span, interner, sink)
                && let Some(fc) = files.get(fid.as_u32() as usize)
            {
                fkeys.push(connector_to_decl_key(fc));
            }
        }
        if !fkeys.is_empty() {
            rules.push(HirDeclarativeRule {
                target,
                visibility,
                trigger: HirDeclarativeTrigger::Files(fkeys),
            });
        }
    }

    for inh in inherited_rules {
        if inh.visibility == HirGlobalFlag::Global {
            rules.push(inh.clone());
        }
    }
    rules
}

/// Maps `"INPUT"`, `"OUTPUT"`, `"I-O"`, `"EXTEND"` to [`HirOpenMode`].
///
/// # Arguments
///
/// * `word` - Uppercase canonical word.
fn parse_open_mode_word(word: &str) -> Option<HirOpenMode> {
    match word {
        "INPUT" => Some(HirOpenMode::Input),
        "OUTPUT" => Some(HirOpenMode::Output),
        "I-O" => Some(HirOpenMode::Io),
        "EXTEND" => Some(HirOpenMode::Extend),
        _ => None,
    }
}

/// Converts a [`HirFileConnector`]'s sharing identity into [`HirDeclarativeFileKey`].
///
/// # Arguments
///
/// * `fc` - Target file connector.
fn connector_to_decl_key(fc: &HirFileConnector) -> HirDeclarativeFileKey {
    match &fc.sharing {
        HirConnectorSharing::UnitOwned {
            owner_unit,
            owner_fc,
        } => HirDeclarativeFileKey::UnitOwned(*owner_unit, *owner_fc),
        HirConnectorSharing::External { external_name } => {
            HirDeclarativeFileKey::External(external_name.clone())
        }
    }
}

/// Computes the 5-slot [`HirFileDeclSet`] matching `fc_id` in `ctx.declaratives`.
///
/// # Arguments
///
/// * `fc_id` - Target [`FileConnectorId`].
/// * `ctx` - Active unit lowering context.
pub(crate) fn resolve_decl_set_for_file(
    fc_id: FileConnectorId,
    ctx: &UnitLowerCtx<'_>,
) -> HirFileDeclSet {
    let mut set = HirFileDeclSet::default();
    let Some(fc) = ctx.files.get(fc_id.as_u32() as usize) else {
        return set;
    };
    let fkey = connector_to_decl_key(fc);
    for rule in ctx.declaratives {
        match &rule.trigger {
            HirDeclarativeTrigger::Files(keys) => {
                if set.on_file.is_none() && keys.contains(&fkey) {
                    set.on_file = Some(rule.target);
                }
            }
            HirDeclarativeTrigger::OpenMode(HirOpenMode::Input) => {
                if set.on_input.is_none() {
                    set.on_input = Some(rule.target);
                }
            }
            HirDeclarativeTrigger::OpenMode(HirOpenMode::Output) => {
                if set.on_output.is_none() {
                    set.on_output = Some(rule.target);
                }
            }
            HirDeclarativeTrigger::OpenMode(HirOpenMode::Io) => {
                if set.on_io.is_none() {
                    set.on_io = Some(rule.target);
                }
            }
            HirDeclarativeTrigger::OpenMode(HirOpenMode::Extend) => {
                if set.on_extend.is_none() {
                    set.on_extend = Some(rule.target);
                }
            }
        }
    }
    set
}

/// Collects all `DataItemId`s referenced by a `GLOBAL` [`HirFileConnector`] so nested programs inherit their storage.
///
/// # Arguments
///
/// * `fc` - Parent `GLOBAL` [`HirFileConnector`].
#[must_use]
pub fn collect_connector_referenced_items(fc: &HirFileConnector) -> Vec<DataItemId> {
    let mut ids = fc.records.clone();
    if let HirAssignSpec::DynamicItem(id) = fc.assign {
        ids.push(id);
    }
    if let Some(id) = fc.depending_on_item {
        ids.push(id);
    }
    if let Some(id) = fc.status_item {
        ids.push(id);
    }
    if let Some(id) = fc.secondary_status_item {
        ids.push(id);
    }
    if let Some(id) = fc.relative_key_item {
        ids.push(id);
    }
    ids.extend_from_slice(&fc.primary_key_items);
    for ak in &fc.alternate_keys {
        ids.extend_from_slice(&ak.key_items);
    }
    if let Some(lin) = &fc.linage {
        ids.push(lin.counter_item);
        for param in [Some(lin.body), lin.footing, lin.top, lin.bottom]
            .into_iter()
            .flatten()
        {
            if let HirLinageParam::Dynamic(id) = param {
                ids.push(id);
            }
        }
    }
    ids
}

/// Remaps all `DataItemId`s inside a parent `GLOBAL` [`HirFileConnector`] for a nested child program.
///
/// # Arguments
///
/// * `fc` - Parent `GLOBAL` [`HirFileConnector`].
/// * `new_id` - Child [`FileConnectorId`].
/// * `id_map` - Parent-to-child [`DataItemId`] mapping.
#[must_use]
pub fn remap_inherited_file_connector(
    fc: &HirFileConnector,
    new_id: FileConnectorId,
    id_map: &BTreeMap<DataItemId, DataItemId>,
) -> HirFileConnector {
    let map_opt = |opt: Option<DataItemId>| opt.and_then(|d| id_map.get(&d).copied());
    let map_lin = |p: HirLinageParam| match p {
        HirLinageParam::Static(n) => HirLinageParam::Static(n),
        HirLinageParam::Dynamic(d) => id_map
            .get(&d)
            .copied()
            .map_or(HirLinageParam::Static(1), HirLinageParam::Dynamic),
    };
    HirFileConnector {
        id: new_id,
        name: fc.name,
        kind: fc.kind,
        sharing: fc.sharing.clone(),
        record_area_group: new_id,
        organization: fc.organization,
        access_mode: fc.access_mode,
        optionality: fc.optionality,
        assign: match &fc.assign {
            HirAssignSpec::DynamicItem(d) => id_map.get(d).copied().map_or(
                HirAssignSpec::Literal(String::new()),
                HirAssignSpec::DynamicItem,
            ),
            other => other.clone(),
        },
        record_format: fc.record_format,
        min_record_bytes: fc.min_record_bytes,
        max_record_bytes: fc.max_record_bytes,
        depending_on_item: map_opt(fc.depending_on_item),
        records: fc
            .records
            .iter()
            .filter_map(|r| id_map.get(r).copied())
            .collect(),
        status_item: map_opt(fc.status_item),
        secondary_status_item: map_opt(fc.secondary_status_item),
        relative_key_item: map_opt(fc.relative_key_item),
        primary_split_name: fc.primary_split_name,
        primary_key_items: fc
            .primary_key_items
            .iter()
            .filter_map(|k| id_map.get(k).copied())
            .collect(),
        primary_key_segments: fc.primary_key_segments.clone(),
        alternate_keys: fc
            .alternate_keys
            .iter()
            .map(|ak| HirAltKeySpec {
                split_name: ak.split_name,
                key_items: ak
                    .key_items
                    .iter()
                    .filter_map(|k| id_map.get(k).copied())
                    .collect(),
                segments: ak.segments.clone(),
                duplicates: ak.duplicates,
                suppression: ak.suppression.clone(),
            })
            .collect(),
        lock_mode: fc.lock_mode,
        sharing_mode: fc.sharing_mode,
        padding_byte: fc.padding_byte,
        print_mode: fc.print_mode,
        linage: fc.linage.and_then(|l| {
            let cnt = id_map.get(&l.counter_item).copied()?;
            Some(HirLinageClause {
                body: map_lin(l.body),
                footing: l.footing.map(map_lin),
                top: l.top.map(map_lin),
                bottom: l.bottom.map(map_lin),
                counter_item: cnt,
            })
        }),
        collating_alphabet: fc.collating_alphabet,
        span: fc.span,
    }
}

/// Lowers `OPEN` into [`HirStmt::FileOpen`].
///
/// # Arguments
///
/// * `entries` - `(OpenMode, file_sym)` pairs.
/// * `options` - Per-entry [`OpenOptionSpec`]s.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_open_stmt(
    entries: &[(OpenMode, SymbolId)],
    options: &[OpenOptionSpec],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut hir_entries = Vec::with_capacity(entries.len());
    for (idx, &(mode, fsym)) in entries.iter().enumerate() {
        let Some(fc_id) = resolve_file_sym(fsym, span, ctx) else {
            continue;
        };
        let opt = options
            .get(idx)
            .copied()
            .unwrap_or(OpenOptionSpec::Standard);
        hir_entries.push(HirFileOpenEntry {
            file: fc_id,
            mode: match mode {
                OpenMode::Input => HirOpenMode::Input,
                OpenMode::Output => HirOpenMode::Output,
                OpenMode::Io => HirOpenMode::Io,
                OpenMode::Extend => HirOpenMode::Extend,
            },
            option: match opt {
                OpenOptionSpec::Standard => HirOpenOption::Standard,
                OpenOptionSpec::WithNoRewind => HirOpenOption::WithNoRewind,
                OpenOptionSpec::Reversed => HirOpenOption::Reversed,
                OpenOptionSpec::WithLock => HirOpenOption::WithLock,
                OpenOptionSpec::SharingAllOther => HirOpenOption::SharingAllOther,
                OpenOptionSpec::SharingNoOther => HirOpenOption::SharingNoOther,
                OpenOptionSpec::SharingReadOnly => HirOpenOption::SharingReadOnly,
            },
            decls: resolve_decl_set_for_file(fc_id, ctx),
        });
    }
    if !hir_entries.is_empty() {
        out.push(HirStmt::FileOpen(Box::new(HirFileOpenStmt {
            entries: hir_entries,
            span,
        })));
    }
}

/// Lowers `CLOSE` into [`HirStmt::FileClose`].
///
/// # Arguments
///
/// * `files` - Target file symbols.
/// * `dispositions` - Per-file [`CloseDispositionSpec`]s.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_close_stmt(
    files: &[SymbolId],
    dispositions: &[CloseDispositionSpec],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut entries = Vec::with_capacity(files.len());
    for (idx, &fsym) in files.iter().enumerate() {
        let Some(fc_id) = resolve_file_sym(fsym, span, ctx) else {
            continue;
        };
        let disp = dispositions
            .get(idx)
            .copied()
            .unwrap_or(CloseDispositionSpec::Standard);
        entries.push(HirFileCloseEntry {
            file: fc_id,
            disposition: match disp {
                CloseDispositionSpec::Standard => HirCloseDisposition::Standard,
                CloseDispositionSpec::WithLock => HirCloseDisposition::WithLock,
                CloseDispositionSpec::WithNoRewind => HirCloseDisposition::WithNoRewind,
                CloseDispositionSpec::ReelUnit => HirCloseDisposition::ReelUnit,
                CloseDispositionSpec::ReelUnitForRemoval => HirCloseDisposition::ReelUnitForRemoval,
            },
            decls: resolve_decl_set_for_file(fc_id, ctx),
        });
    }
    if !entries.is_empty() {
        out.push(HirStmt::FileClose(Box::new(HirFileCloseStmt {
            entries,
            span,
        })));
    }
}

/// Borrowed AST parts of a `READ` statement.
pub(crate) struct ReadStmtParts<'a> {
    /// File connector symbol.
    pub(crate) file: SymbolId,
    /// Parsed direction modifier.
    pub(crate) direction: ReadDirection,
    /// Optional `INTO` destination.
    pub(crate) into: Option<&'a DataRef>,
    /// Optional `KEY IS` data reference.
    pub(crate) key: Option<&'a DataRef>,
    /// Record lock modifier.
    pub(crate) lock_mode: ReadLockSpec,
    /// Conditional handlers.
    pub(crate) handlers: &'a EventHandlers,
    /// Statement span.
    pub(crate) span: SourceSpan,
}

/// Lowers `READ` into [`HirStmt::FileRead`].
///
/// # Arguments
///
/// * `parts` - Borrowed `READ` AST parts.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_read_stmt(
    parts: &ReadStmtParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(fc_id) = resolve_file_sym(parts.file, parts.span, ctx) else {
        let _ = lower_event_handlers(parts.handlers, ctx);
        return;
    };
    let acc_mode = ctx
        .files
        .get(fc_id.as_u32() as usize)
        .map_or(HirFileAccessMode::Sequential, |f| f.access_mode);

    let mode = match parts.direction {
        ReadDirection::Previous => HirReadMode::Sequential {
            direction: HirSeqReadDir::Previous,
        },
        ReadDirection::ExplicitNext => HirReadMode::Sequential {
            direction: HirSeqReadDir::Next,
        },
        ReadDirection::NextOrDefault => match acc_mode {
            HirFileAccessMode::Sequential => HirReadMode::Sequential {
                direction: HirSeqReadDir::Next,
            },
            HirFileAccessMode::Random | HirFileAccessMode::Dynamic => {
                resolve_random_read_key(fc_id, parts.key, ctx)
            }
        },
        ReadDirection::Random => resolve_random_read_key(fc_id, parts.key, ctx),
    };

    let into_moves = build_into_moves(fc_id, parts.into, parts.span, ctx);
    let inline_presence = if parts.handlers.on_event.is_empty() {
        HirInlineClausePresence::Absent
    } else {
        HirInlineClausePresence::Present
    };
    let (on_condition, not_on_condition) = lower_event_handlers(parts.handlers, ctx);
    let decls = resolve_decl_set_for_file(fc_id, ctx);

    out.push(HirStmt::FileRead(Box::new(HirFileReadStmt {
        file: fc_id,
        mode,
        lock_policy: match parts.lock_mode {
            ReadLockSpec::Default => HirReadLockPolicy::Default,
            ReadLockSpec::WithLock => HirReadLockPolicy::WithLock,
            ReadLockSpec::WithKeptLock => HirReadLockPolicy::WithKeptLock,
            ReadLockSpec::WithNoLock => HirReadLockPolicy::WithNoLock,
            ReadLockSpec::IgnoreLock => HirReadLockPolicy::IgnoreLock,
        },
        into_moves,
        inline_presence,
        on_condition,
        not_on_condition,
        decls,
        span: parts.span,
    })));
}

/// Resolves `KEY IS ...` (or default primary/relative key `0`) for a random `READ`.
///
/// # Arguments
///
/// * `fc_id` - Target [`FileConnectorId`].
/// * `key_ref` - Optional `KEY IS` data reference.
/// * `ctx` - Unit lowering context.
fn resolve_random_read_key(
    fc_id: FileConnectorId,
    key_ref: Option<&DataRef>,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirReadMode {
    let Some(kref) = key_ref else {
        return HirReadMode::Random {
            key_index: 0,
            key_place: None,
        };
    };
    if let Some(split_slot) = match_split_key_alias(fc_id, kref, ctx) {
        return HirReadMode::Random {
            key_index: split_slot,
            key_place: None,
        };
    }
    let kplace = resolve_place(kref, ctx);
    let key_index = kplace
        .as_ref()
        .map_or(0, |pl| match_indexed_key_slot(fc_id, pl, ctx));
    HirReadMode::Random {
        key_index,
        key_place: kplace,
    }
}

/// Checks if `kref` names a synthetic split-key alias (`0` for primary, `1..` for alternate).
///
/// # Arguments
///
/// * `fc_id` - Target [`FileConnectorId`].
/// * `kref` - Candidate `KEY IS` data reference.
/// * `ctx` - Unit lowering context.
fn match_split_key_alias(
    fc_id: FileConnectorId,
    kref: &DataRef,
    ctx: &UnitLowerCtx<'_>,
) -> Option<u16> {
    if !kref.subscripts.is_empty() || kref.refmod.is_some() {
        return None;
    }
    let fc = ctx.files.get(fc_id.as_u32() as usize)?;
    if fc.primary_split_name == Some(kref.name.base) {
        return Some(0);
    }
    for (idx, ak) in fc.alternate_keys.iter().enumerate() {
        if ak.split_name == Some(kref.name.base) {
            return u16::try_from(idx.saturating_add(1)).ok();
        }
    }
    None
}

/// Matches a key [`HirPlace`] to primary key (`0`) or 1-based alternate key (`1..=k`) by item ID or byte offset.
///
/// # Arguments
///
/// * `fc_id` - Target [`FileConnectorId`].
/// * `kplace` - Resolved key [`HirPlace`].
/// * `ctx` - Unit lowering context.
fn match_indexed_key_slot(
    fc_id: FileConnectorId,
    kplace: &HirPlace,
    ctx: &UnitLowerCtx<'_>,
) -> u16 {
    let Some(fc) = ctx.files.get(fc_id.as_u32() as usize) else {
        return 0;
    };
    if fc.primary_key_items.contains(&kplace.item) {
        return 0;
    }
    for (idx, ak) in fc.alternate_keys.iter().enumerate() {
        if ak.key_items.contains(&kplace.item) {
            return u16::try_from(idx.saturating_add(1)).unwrap_or(0);
        }
    }
    if let Some(kit) = ctx.layout.items.get(kplace.item.as_u32() as usize)
        && kit.section == coblang_hir::HirStorageSection::FileSection(fc_id)
    {
        let off = record_relative_offset(kit, &ctx.layout.items);
        if fc
            .primary_key_segments
            .first()
            .is_some_and(|s| s.offset == off)
        {
            return 0;
        }
        for (idx, ak) in fc.alternate_keys.iter().enumerate() {
            if ak.segments.first().is_some_and(|s| s.offset == off) {
                return u16::try_from(idx.saturating_add(1)).unwrap_or(0);
            }
        }
    }
    0
}

/// Borrowed AST parts of a `WRITE` statement.
pub(crate) struct WriteStmtParts<'a> {
    /// Record or file reference.
    pub(crate) record: &'a DataRef,
    /// Record-name vs `WRITE FILE file-name` mode.
    pub(crate) target_mode: WriteTargetMode,
    /// Optional `FROM` source expression.
    pub(crate) from: Option<&'a Expr>,
    /// Optional `(Before|After, expr)` advancing specification.
    pub(crate) advancing: Option<&'a (BoundSide, Expr)>,
    /// Conditional handler flavor.
    pub(crate) handler_kind: WriteHandlerKind,
    /// Conditional handlers.
    pub(crate) handlers: &'a EventHandlers,
    /// Statement span.
    pub(crate) span: SourceSpan,
}

/// Lowers `WRITE` into [`HirStmt::FileWrite`].
///
/// # Arguments
///
/// * `parts` - Borrowed `WRITE` AST parts.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_write_stmt(
    parts: &WriteStmtParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some((fc_id, rec_place, rec_dref_opt)) =
        resolve_write_target(parts.record, parts.target_mode, parts.span, ctx)
    else {
        if let Some(fexpr) = parts.from {
            let _ = lower_expr_with_cat(fexpr, ctx);
        }
        let _ = lower_event_handlers(parts.handlers, ctx);
        return;
    };

    let pre_from_moves = build_from_moves(parts.from, rec_dref_opt, &rec_place, parts.span, ctx);

    let advancing = if let Some((side, adv_expr)) = parts.advancing {
        if let Some(fc) = ctx.files.get_mut(fc_id.as_u32() as usize) {
            fc.print_mode = HirPrintAdvancingMode::LineAdvancing;
        }
        lower_write_advancing(*side, adv_expr, ctx)
    } else {
        HirWriteAdvancing::Default
    };

    let handler_kind = match parts.handler_kind {
        WriteHandlerKind::None => HirWriteHandlerKind::None,
        WriteHandlerKind::EndOfPage => HirWriteHandlerKind::EndOfPage,
        WriteHandlerKind::InvalidKey => HirWriteHandlerKind::InvalidKey,
    };
    let (on_event, not_on_event) = lower_event_handlers(parts.handlers, ctx);
    let decls = resolve_decl_set_for_file(fc_id, ctx);

    out.push(HirStmt::FileWrite(Box::new(HirFileWriteStmt {
        file: fc_id,
        record_place: rec_place,
        pre_from_moves,
        advancing,
        handler_kind,
        on_event,
        not_on_event,
        decls,
        span: parts.span,
    })));
}

/// Resolves `{BEFORE|AFTER} ADVANCING ...` into [`HirWriteAdvancing`], including `PAGE` and `SPECIAL-NAMES` mnemonics.
///
/// # Arguments
///
/// * `side` - `Before` vs `After`.
/// * `adv_expr` - Parsed advancing expression.
/// * `ctx` - Unit lowering context.
fn lower_write_advancing(
    side: BoundSide,
    adv_expr: &Expr,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirWriteAdvancing {
    let hir_side = match side {
        BoundSide::Before => HirAdvancingSide::Before,
        BoundSide::After => HirAdvancingSide::After,
    };
    if let Expr::Data(dref) = adv_expr
        && dref.name.qualifiers.is_empty()
        && dref.subscripts.is_empty()
        && dref.refmod.is_none()
    {
        let word = ctx.interner.resolve_canonical(dref.name.base).unwrap_or("");
        if word == "PAGE" {
            return HirWriteAdvancing::Page { side: hir_side };
        }
        if let Some(dev) = ctx.device_map.iter().find(|d| d.mnemonic == dref.name.base) {
            let sys = ctx
                .interner
                .resolve_canonical(dev.system_device)
                .unwrap_or("");
            if sys == "CSP" {
                return HirWriteAdvancing::Lines {
                    side: hir_side,
                    count: const_int_expr("0", adv_expr.span(), ctx),
                };
            }
            return HirWriteAdvancing::Page { side: hir_side };
        }
    }
    let (cnt_hir, _) = lower_expr_with_cat(adv_expr, ctx);
    HirWriteAdvancing::Lines {
        side: hir_side,
        count: cnt_hir,
    }
}

/// Lowers `REWRITE` into [`HirStmt::FileRewrite`].
///
/// # Arguments
///
/// * `record` - Record or file reference.
/// * `target_mode` - Record-name vs `REWRITE FILE file-name` mode.
/// * `from` - Optional `FROM` source expression.
/// * `handlers` - `INVALID KEY` handlers.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_rewrite_stmt(
    record: &DataRef,
    target_mode: WriteTargetMode,
    from: Option<&Expr>,
    handlers: &EventHandlers,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some((fc_id, rec_place, rec_dref_opt)) =
        resolve_write_target(record, target_mode, span, ctx)
    else {
        if let Some(fexpr) = from {
            let _ = lower_expr_with_cat(fexpr, ctx);
        }
        let _ = lower_event_handlers(handlers, ctx);
        return;
    };

    let pre_from_moves = build_from_moves(from, rec_dref_opt, &rec_place, span, ctx);
    let inline_presence = if handlers.on_event.is_empty() {
        HirInlineClausePresence::Absent
    } else {
        HirInlineClausePresence::Present
    };
    let (on_invalid_key, not_on_invalid_key) = lower_event_handlers(handlers, ctx);
    let decls = resolve_decl_set_for_file(fc_id, ctx);

    out.push(HirStmt::FileRewrite(Box::new(HirFileRewriteStmt {
        file: fc_id,
        record_place: rec_place,
        pre_from_moves,
        inline_presence,
        on_invalid_key,
        not_on_invalid_key,
        decls,
        span,
    })));
}

/// Lowers `DELETE` (`Record` vs `DELETE FILE`) into [`HirStmt::FileDelete`] or [`HirStmt::FileDeletePhysical`].
///
/// # Arguments
///
/// * `files` - Target file symbols.
/// * `kind` - `Record` vs `PhysicalFile`.
/// * `handlers` - `INVALID KEY` / `ON EXCEPTION` handlers.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_delete_stmt(
    files: &[SymbolId],
    kind: DeleteKind,
    handlers: &EventHandlers,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let inline_presence = if handlers.on_event.is_empty() {
        HirInlineClausePresence::Absent
    } else {
        HirInlineClausePresence::Present
    };
    let (on_ev, not_on_ev) = lower_event_handlers(handlers, ctx);

    match kind {
        DeleteKind::Record => {
            let Some(&fsym) = files.first() else {
                return;
            };
            let Some(fc_id) = resolve_file_sym(fsym, span, ctx) else {
                return;
            };
            let decls = resolve_decl_set_for_file(fc_id, ctx);
            out.push(HirStmt::FileDelete(Box::new(HirFileDeleteStmt {
                file: fc_id,
                inline_presence,
                on_invalid_key: on_ev,
                not_on_invalid_key: not_on_ev,
                decls,
                span,
            })));
        }
        DeleteKind::PhysicalFile => {
            let mut entries = Vec::with_capacity(files.len());
            for &fsym in files {
                if let Some(fc_id) = resolve_file_sym(fsym, span, ctx) {
                    entries.push(HirDeletePhysicalEntry {
                        file: fc_id,
                        decls: resolve_decl_set_for_file(fc_id, ctx),
                    });
                }
            }
            if !entries.is_empty() {
                out.push(HirStmt::FileDeletePhysical(Box::new(
                    HirFileDeletePhysicalStmt {
                        entries,
                        inline_presence,
                        on_exception: on_ev,
                        not_on_exception: not_on_ev,
                        span,
                    },
                )));
            }
        }
    }
}

/// Borrowed AST parts of a `START` statement.
pub(crate) struct StartStmtParts<'a> {
    /// Target file symbol.
    pub(crate) file: SymbolId,
    /// Optional `(RelOp, DataRef)` key relation.
    pub(crate) key_rel: Option<&'a (RelOp, DataRef)>,
    /// `KeyRelation`, `First`, or `Last`.
    pub(crate) position: StartPositionSpec,
    /// Optional `WITH SIZE/LENGTH` expression.
    pub(crate) key_length: Option<&'a Expr>,
    /// `INVALID KEY` handlers.
    pub(crate) handlers: &'a EventHandlers,
    /// Statement span.
    pub(crate) span: SourceSpan,
}

/// Lowers `START` into [`HirStmt::FileStart`].
///
/// # Arguments
///
/// * `parts` - Borrowed `START` AST parts.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_start_stmt(
    parts: &StartStmtParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(fc_id) = resolve_file_sym(parts.file, parts.span, ctx) else {
        if let Some(kl) = parts.key_length {
            let _ = lower_expr_with_cat(kl, ctx);
        }
        let _ = lower_event_handlers(parts.handlers, ctx);
        return;
    };

    let mode = match parts.position {
        StartPositionSpec::First => HirStartMode::First,
        StartPositionSpec::Last => HirStartMode::Last,
        StartPositionSpec::KeyRelation => {
            let klen_hir = parts.key_length.map(|e| lower_expr_with_cat(e, ctx).0);
            if let Some((op, kref)) = parts.key_rel {
                let relation = match op {
                    RelOp::Eq => HirStartRelation::Equal,
                    RelOp::Gt => HirStartRelation::Greater,
                    RelOp::Ge => HirStartRelation::GreaterOrEqual,
                    RelOp::Lt => HirStartRelation::Less,
                    RelOp::Le => HirStartRelation::LessOrEqual,
                    RelOp::Ne => {
                        ctx.sink.emit(Diagnostic::error(
                            DiagnosticCode::E0008TypeMismatch,
                            Some(parts.span),
                            String::from("`START ... KEY` does not allow `NOT EQUAL` relation"),
                        ));
                        HirStartRelation::Equal
                    }
                };
                let (key_index, kplace) =
                    if let Some(split_slot) = match_split_key_alias(fc_id, kref, ctx) {
                        (split_slot, None)
                    } else {
                        let kp = resolve_place(kref, ctx);
                        let ki = kp
                            .as_ref()
                            .map_or(0, |pl| match_indexed_key_slot(fc_id, pl, ctx));
                        (ki, kp)
                    };
                HirStartMode::Relation {
                    relation,
                    key_index,
                    key_place: kplace.map(Box::new),
                    key_length: klen_hir.map(Box::new),
                }
            } else {
                HirStartMode::Relation {
                    relation: HirStartRelation::Equal,
                    key_index: 0,
                    key_place: None,
                    key_length: klen_hir.map(Box::new),
                }
            }
        }
    };

    let inline_presence = if parts.handlers.on_event.is_empty() {
        HirInlineClausePresence::Absent
    } else {
        HirInlineClausePresence::Present
    };
    let (on_invalid_key, not_on_invalid_key) = lower_event_handlers(parts.handlers, ctx);
    let decls = resolve_decl_set_for_file(fc_id, ctx);

    out.push(HirStmt::FileStart(Box::new(HirFileStartStmt {
        file: fc_id,
        mode,
        inline_presence,
        on_invalid_key,
        not_on_invalid_key,
        decls,
        span: parts.span,
    })));
}

/// Lowers `UNLOCK` into [`HirStmt::FileUnlock`].
///
/// # Arguments
///
/// * `file` - Target file symbol.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_unlock_stmt(
    file: SymbolId,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(fc_id) = resolve_file_sym(file, span, ctx) else {
        return;
    };
    let decls = resolve_decl_set_for_file(fc_id, ctx);
    out.push(HirStmt::FileUnlock(Box::new(HirFileUnlockStmt {
        file: fc_id,
        decls,
        span,
    })));
}

/// Borrowed AST parts of a `SORT` file statement (`Milestone M16`).
pub(crate) struct SortFileParts<'a> {
    /// `SD` file symbol.
    pub(crate) sd_file: SymbolId,
    /// `(KeyOrder, Vec<DataRef>)` key clauses.
    pub(crate) keys: &'a [(KeyOrder, Vec<DataRef>)],
    /// `WITH DUPLICATES IN ORDER`.
    pub(crate) duplicates_in_order: TruthValue,
    /// Optional `COLLATING SEQUENCE` symbol.
    pub(crate) collating_sequence: Option<SymbolId>,
    /// `USING` files.
    pub(crate) using_files: &'a [SymbolId],
    /// Optional `INPUT PROCEDURE`.
    pub(crate) input_proc: Option<&'a (QualifiedName, Option<QualifiedName>)>,
    /// `GIVING` files.
    pub(crate) giving_files: &'a [SymbolId],
    /// Optional `OUTPUT PROCEDURE`.
    pub(crate) output_proc: Option<&'a (QualifiedName, Option<QualifiedName>)>,
    /// Statement span.
    pub(crate) span: SourceSpan,
}

/// Lowers `SORT` into [`HirStmt::SortFile`].
///
/// # Arguments
///
/// * `parts` - Borrowed `SORT` AST parts.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_sort_file_stmt(
    parts: &SortFileParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(sd_id) = resolve_file_sym(parts.sd_file, parts.span, ctx) else {
        return;
    };
    let keys = resolve_sort_keys(parts.keys, ctx);
    let collating_alphabet = resolve_stmt_collation(parts.collating_sequence, ctx);
    let Some(input) = resolve_sort_input(parts.using_files, parts.input_proc, parts.span, ctx)
    else {
        return;
    };
    let Some(output) = resolve_sort_output(parts.giving_files, parts.output_proc, parts.span, ctx)
    else {
        return;
    };
    let sort_return_item = lookup_sort_return_item(ctx);

    out.push(HirStmt::SortFile(Box::new(HirSortFileStmt {
        sd_file: sd_id,
        keys,
        duplicates: match parts.duplicates_in_order {
            TruthValue::True => HirSortDuplicates::InOrder,
            TruthValue::False => HirSortDuplicates::Arbitrary,
        },
        collating_alphabet,
        input,
        output,
        sort_return_item,
        span: parts.span,
    })));
}

/// Borrowed AST parts of a `MERGE` statement (`Milestone M16`).
pub(crate) struct MergeFileParts<'a> {
    /// `SD` file symbol.
    pub(crate) sd_file: SymbolId,
    /// `(KeyOrder, Vec<DataRef>)` key clauses.
    pub(crate) keys: &'a [(KeyOrder, Vec<DataRef>)],
    /// Optional `COLLATING SEQUENCE` symbol.
    pub(crate) collating_sequence: Option<SymbolId>,
    /// `USING` files.
    pub(crate) using_files: &'a [SymbolId],
    /// `GIVING` files.
    pub(crate) giving_files: &'a [SymbolId],
    /// Optional `OUTPUT PROCEDURE`.
    pub(crate) output_proc: Option<&'a (QualifiedName, Option<QualifiedName>)>,
    /// Statement span.
    pub(crate) span: SourceSpan,
}

/// Lowers `MERGE` into [`HirStmt::MergeFile`].
///
/// # Arguments
///
/// * `parts` - Borrowed `MERGE` AST parts.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_merge_file_stmt(
    parts: &MergeFileParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(sd_id) = resolve_file_sym(parts.sd_file, parts.span, ctx) else {
        return;
    };
    let keys = resolve_sort_keys(parts.keys, ctx);
    let collating_alphabet = resolve_stmt_collation(parts.collating_sequence, ctx);
    let mut using_files = Vec::with_capacity(parts.using_files.len());
    for &fsym in parts.using_files {
        if let Some(fid) = resolve_file_sym(fsym, parts.span, ctx) {
            using_files.push(HirSortTransferFile {
                file: fid,
                decls: resolve_decl_set_for_file(fid, ctx),
            });
        }
    }
    let Some(output) = resolve_sort_output(parts.giving_files, parts.output_proc, parts.span, ctx)
    else {
        return;
    };
    let sort_return_item = lookup_sort_return_item(ctx);

    out.push(HirStmt::MergeFile(Box::new(HirMergeFileStmt {
        sd_file: sd_id,
        keys,
        collating_alphabet,
        using_files,
        output,
        sort_return_item,
        span: parts.span,
    })));
}

/// Lowers `RELEASE sd-rec [FROM src]` into [`HirStmt::ReleaseRecord`].
///
/// # Arguments
///
/// * `record` - `SD` record reference.
/// * `from` - Optional `FROM` source expression.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_release_stmt(
    record: &DataRef,
    from: Option<&Expr>,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some((sd_id, rec_place, rec_dref_opt)) =
        resolve_write_target(record, WriteTargetMode::RecordName, span, ctx)
    else {
        if let Some(fexpr) = from {
            let _ = lower_expr_with_cat(fexpr, ctx);
        }
        return;
    };
    let pre_from_moves = build_from_moves(from, rec_dref_opt, &rec_place, span, ctx);
    let sort_return_item = lookup_sort_return_item(ctx);
    out.push(HirStmt::ReleaseRecord(Box::new(HirReleaseStmt {
        sd_file: sd_id,
        record_place: rec_place,
        pre_from_moves,
        sort_return_item,
        span,
    })));
}

/// Lowers `RETURN sd-file RECORD [INTO dst] AT END ...` into [`HirStmt::ReturnRecord`].
///
/// # Arguments
///
/// * `sd_file` - `SD` file symbol.
/// * `into` - Optional `INTO` destination reference.
/// * `handlers` - `AT END` / `NOT AT END` handlers.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_return_stmt(
    sd_file: SymbolId,
    into: Option<&DataRef>,
    handlers: &EventHandlers,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(sd_id) = resolve_file_sym(sd_file, span, ctx) else {
        let _ = lower_stmt_slice_to_vec(&handlers.on_event, ctx);
        let _ = lower_stmt_slice_to_vec(&handlers.not_on_event, ctx);
        return;
    };
    let into_moves = build_into_moves(sd_id, into, span, ctx);
    let (at_end, not_at_end) = lower_event_handlers(handlers, ctx);
    let sort_return_item = lookup_sort_return_item(ctx);
    out.push(HirStmt::ReturnRecord(Box::new(HirReturnStmt {
        sd_file: sd_id,
        into_moves,
        at_end,
        not_at_end,
        sort_return_item,
        span,
    })));
}

/// Resolves a file symbol to its [`FileConnectorId`].
///
/// # Arguments
///
/// * `fsym` - File connector symbol.
/// * `span` - Diagnostic span.
/// * `ctx` - Unit lowering context.
fn resolve_file_sym(
    fsym: SymbolId,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<FileConnectorId> {
    if let ResolvedSymbol::File(fid) =
        ctx.symtab
            .resolve_symbol(ctx.scope, fsym, &[], span, ctx.interner, ctx.sink)?
    {
        Some(fid)
    } else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            String::from("expected a file connector name"),
        ));
        None
    }
}

/// Resolves a `WRITE` / `REWRITE` / `RELEASE` target (`01` record or `WRITE FILE file-name`) to `(FileConnectorId, HirPlace, Option<&DataRef>)`.
///
/// # Arguments
///
/// * `target` - AST record or file reference.
/// * `mode` - `RecordName` vs `FileName`.
/// * `span` - Diagnostic span.
/// * `ctx` - Unit lowering context.
fn resolve_write_target<'a>(
    target: &'a DataRef,
    mode: WriteTargetMode,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<(FileConnectorId, HirPlace, Option<&'a DataRef>)> {
    if mode == WriteTargetMode::FileName {
        let fc_id = resolve_file_sym(target.name.base, span, ctx)?;
        let fc = ctx.files.get(fc_id.as_u32() as usize)?;
        let &first_rec_id = fc.records.first()?;
        let first_rec_item = ctx.layout.items.get(first_rec_id.as_u32() as usize)?;
        let mut pl = simple_place_for_item(first_rec_item, span);
        if fc.records.len() > 1 {
            pl.effective_ty = HirType::Group {
                static_byte_len: fc.max_record_bytes,
                odo_presence: DynamicOdoPresence::StaticSize,
            };
        }
        return Some((fc_id, pl, None));
    }

    let pl = resolve_place(target, ctx)?;
    let root_id = find_root_item(pl.item, &ctx.layout.items);
    let root_item = ctx.layout.items.get(root_id.as_u32() as usize)?;
    let fc_id = match root_item.section {
        coblang_hir::HirStorageSection::FileSection(fid) => fid,
        coblang_hir::HirStorageSection::InheritedGlobal { .. } => ctx
            .files
            .iter()
            .find_map(|f| f.records.contains(&root_id).then_some(f.id))?,
        _ => {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(span),
                String::from("`WRITE`/`REWRITE`/`RELEASE` subject must be a `FILE SECTION` record"),
            ));
            return None;
        }
    };
    if root_item.level != LevelNumber::Hierarchy(1) {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            String::from("`WRITE`/`REWRITE`/`RELEASE` subject must be a `01` record entry"),
        ));
    }
    Some((fc_id, pl, Some(target)))
}

/// Walks `parent` links to find the root `01`/`77` [`DataItemId`].
///
/// # Arguments
///
/// * `id` - Starting [`DataItemId`].
/// * `items` - Program data-item arena.
fn find_root_item(mut id: DataItemId, items: &[HirDataItem]) -> DataItemId {
    while let Some(parent) = items.get(id.as_u32() as usize).and_then(|it| it.parent) {
        id = parent;
    }
    id
}

/// Builds pre-write `MOVE src TO record` statements for `WRITE/REWRITE/RELEASE ... FROM src`.
///
/// # Arguments
///
/// * `from` - Optional `FROM` source expression.
/// * `rec_dref_opt` - Original `01` record `DataRef` (`None` when `WRITE FILE file-name` was used).
/// * `rec_place` - Resolved destination `01` record place.
/// * `span` - Statement span.
/// * `ctx` - Unit lowering context.
fn build_from_moves(
    from: Option<&Expr>,
    rec_dref_opt: Option<&DataRef>,
    rec_place: &HirPlace,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<HirStmt> {
    let mut moves = Vec::new();
    let Some(from_expr) = from else {
        return moves;
    };
    if let Some(rec_dref) = rec_dref_opt {
        lower_move_stmt(
            CorrespondingMode::Standard,
            from_expr,
            std::slice::from_ref(rec_dref),
            span,
            &mut moves,
            ctx,
        );
    } else {
        let (src_hir, _) = lower_expr_with_cat(from_expr, ctx);
        let mut dst_pl = rec_place.clone();
        if let HirExpr::Place(src_pl) = &src_hir {
            let src_sz = src_pl.effective_ty.static_byte_size();
            if src_sz > 0 {
                dst_pl.effective_ty = HirType::Group {
                    static_byte_len: src_sz,
                    odo_presence: DynamicOdoPresence::StaticSize,
                };
            }
        }
        moves.push(HirStmt::Move {
            src: src_hir,
            dst: dst_pl,
            span,
        });
    }
    moves
}

/// Builds post-read `MOVE fd_record TO dst` statements for `READ/RETURN ... INTO dst`.
///
/// # Arguments
///
/// * `fc_id` - Source [`FileConnectorId`].
/// * `into_opt` - Optional `INTO` destination `DataRef`.
/// * `span` - Statement span.
/// * `ctx` - Unit lowering context.
fn build_into_moves(
    fc_id: FileConnectorId,
    into_opt: Option<&DataRef>,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<HirStmt> {
    let mut moves = Vec::new();
    let Some(dst_ref) = into_opt else {
        return moves;
    };
    let Some(dst_place) = resolve_place(dst_ref, ctx) else {
        return moves;
    };
    let Some(fc) = ctx.files.get(fc_id.as_u32() as usize) else {
        return moves;
    };
    let Some(&first_rec_id) = fc.records.first() else {
        return moves;
    };
    let Some(first_rec_item) = ctx.layout.items.get(first_rec_id.as_u32() as usize) else {
        return moves;
    };
    let mut src_place = simple_place_for_item(first_rec_item, span);
    if fc.records.len() > 1 || src_place.effective_ty.category() == DataCategory::Group {
        let odo = match src_place.effective_ty {
            HirType::Group { odo_presence, .. } => odo_presence,
            _ => DynamicOdoPresence::StaticSize,
        };
        src_place.effective_ty = HirType::Group {
            static_byte_len: fc.max_record_bytes,
            odo_presence: odo,
        };
    }
    moves.push(HirStmt::Move {
        src: HirExpr::Place(src_place),
        dst: dst_place,
        span,
    });
    moves
}

/// Resolves `SORT`/`MERGE` `ASCENDING`/`DESCENDING KEY` fields into [`HirSortKeyDesc`]s.
///
/// # Arguments
///
/// * `keys` - AST key clauses.
/// * `ctx` - Unit lowering context.
fn resolve_sort_keys(
    keys: &[(KeyOrder, Vec<DataRef>)],
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<HirSortKeyDesc> {
    let mut out = Vec::new();
    for (order, krefs) in keys {
        let direction = match order {
            KeyOrder::Ascending => HirSortDirection::Ascending,
            KeyOrder::Descending => HirSortDirection::Descending,
        };
        for kref in krefs {
            if let Some(pl) = resolve_place(kref, ctx)
                && let Some(it) = ctx.layout.items.get(pl.item.as_u32() as usize)
            {
                out.push(HirSortKeyDesc {
                    direction,
                    offset: record_relative_offset(it, &ctx.layout.items),
                    length: pl.effective_ty.static_byte_size().max(1),
                    ty: pl.effective_ty,
                });
            }
        }
    }
    out
}

/// Resolves statement-level `[COLLATING] SEQUENCE` (falling back to `PROGRAM COLLATING SEQUENCE`).
///
/// # Arguments
///
/// * `col_sym` - Optional explicit alphabet symbol.
/// * `ctx` - Unit lowering context.
fn resolve_stmt_collation(col_sym: Option<SymbolId>, ctx: &UnitLowerCtx<'_>) -> Option<usize> {
    if let Some(sym) = col_sym {
        ctx.alphabets.iter().position(|a| a.name == sym)
    } else {
        ctx.prog_collating_seq
    }
}

/// Resolves `SORT` `USING` files or `INPUT PROCEDURE` into [`HirSortInput`].
///
/// # Arguments
///
/// * `using_files` - `USING` file symbols.
/// * `input_proc` - Optional `(start, thru)` procedure range.
/// * `span` - Statement span.
/// * `ctx` - Unit lowering context.
fn resolve_sort_input(
    using_files: &[SymbolId],
    input_proc: Option<&(QualifiedName, Option<QualifiedName>)>,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<HirSortInput> {
    if let Some((start_qn, thru_opt)) = input_proc {
        let (start, end) = resolve_perform_proc_bounds(start_qn, thru_opt.as_ref(), ctx)?;
        Some(HirSortInput::Procedure { start, end })
    } else {
        let mut files = Vec::with_capacity(using_files.len());
        for &fsym in using_files {
            if let Some(fid) = resolve_file_sym(fsym, span, ctx) {
                files.push(HirSortTransferFile {
                    file: fid,
                    decls: resolve_decl_set_for_file(fid, ctx),
                });
            }
        }
        Some(HirSortInput::Using(files))
    }
}

/// Resolves `SORT`/`MERGE` `GIVING` files or `OUTPUT PROCEDURE` into [`HirSortOutput`].
///
/// # Arguments
///
/// * `giving_files` - `GIVING` file symbols.
/// * `output_proc` - Optional `(start, thru)` procedure range.
/// * `span` - Statement span.
/// * `ctx` - Unit lowering context.
fn resolve_sort_output(
    giving_files: &[SymbolId],
    output_proc: Option<&(QualifiedName, Option<QualifiedName>)>,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<HirSortOutput> {
    if let Some((start_qn, thru_opt)) = output_proc {
        let (start, end) = resolve_perform_proc_bounds(start_qn, thru_opt.as_ref(), ctx)?;
        Some(HirSortOutput::Procedure { start, end })
    } else {
        let mut files = Vec::with_capacity(giving_files.len());
        for &fsym in giving_files {
            if let Some(fid) = resolve_file_sym(fsym, span, ctx) {
                files.push(HirSortTransferFile {
                    file: fid,
                    decls: resolve_decl_set_for_file(fid, ctx),
                });
            }
        }
        Some(HirSortOutput::Giving(files))
    }
}

/// Looks up the synthesized `SORT-RETURN` special-register [`DataItemId`] in `ctx.scope`.
///
/// # Arguments
///
/// * `ctx` - Unit lowering context.
fn lookup_sort_return_item(ctx: &UnitLowerCtx<'_>) -> Option<DataItemId> {
    let sr_sym = ctx.builtins_get("SORT-RETURN");
    match ctx.symtab.resolve_symbol_silent(ctx.scope, sr_sym, &[])? {
        ResolvedSymbol::DataItem(id) => Some(id),
        _ => None,
    }
}

/// Helper lowering a statement slice into a newly allocated `Vec<HirStmt>`.
///
/// # Arguments
///
/// * `stmts` - AST statements.
/// * `ctx` - Unit lowering context.
fn lower_stmt_slice_to_vec(stmts: &[Stmt], ctx: &mut UnitLowerCtx<'_>) -> Vec<HirStmt> {
    let mut v = Vec::new();
    lower_stmt_slice(stmts, &mut v, ctx);
    v
}
