//! Compile-time Working-Storage / Local-Storage layout grouping, initial `VALUE` byte encoding,
//! `RuntimeFieldSpec` builders, and `INITIALIZE` plan construction for `coblang-mlir`.

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{CoblangError, DataItemId, FeatureTag, LevelNumber, ProgramUnitId, Result};
use coblang_decimal::{FixedDecimal, OverpunchDialect, PackedSignPolicy};
use coblang_diag::{SourceMap, SymbolInterner};
use coblang_hir::{
    DataCategory, HirConstValue, HirDataItem, HirDecimalSign, HirExternalCaseMode, HirInitFiller,
    HirInitValueMode, HirOccursDim, HirPlace, HirProgram, HirProgramFormatting, HirSignConvention,
    HirSortTableSpec, HirStorageSection, HirType,
};
use coblang_pic::{
    DefaultInitMode, FillerPolicy, InitCategoryMask, MoveFieldDesc, MoveSource,
    encode_zoned_negative_zero, execute_move,
};
use coblang_rt_core::desc_codec::{
    DecodedInitPlan, DecodedTableSortPlan, FillerMembership, RedefinesMembership, RuntimeFieldSpec,
    RuntimeInitEntry, RuntimeTableSortKey, encode_init_plan, encode_table_sort_plan,
};

/// Resolved storage location of a Data Division item inside its root `01` / `77` buffer.
#[derive(Debug, Clone)]
pub struct ResolvedItemStorage {
    /// Global `cobol.storage` symbol of the owning root `01` / `77` buffer.
    pub root_storage_sym: String,
    /// Root `DataItemId` (`REDEFINES` at `01`/`77` shares the primary root ID).
    pub root_id: DataItemId,
    /// Index into `ProgramStoragePlan::roots`.
    pub root_idx: usize,
    /// Static byte offset of element `(1, 1, ...)` from the start of `root_storage_sym`.
    pub base_byte_offset: u32,
    /// Static byte length of a single occurrence (`>= 1`).
    pub elem_byte_len: u32,
    /// Resolved `RuntimeFieldSpec` for this item.
    pub field_spec: RuntimeFieldSpec,
}

/// Threshold (`64 KiB`) above which root `01`/`77` buffers use `.bss` `zeroinitializer` + startup `InitExec`.
const MAX_INLINE_INIT_IMAGE_BYTES: u32 = 65_536;

/// Compile-time vs startup-`.bss` initialization strategy for a [`RootStorageRecord`].
#[derive(Debug, Clone)]
pub enum RootInitStrategy {
    /// Inline `.data` byte image (`<= 64 KiB`).
    StaticImage(Vec<u8>),
    /// Zero-initialized `.bss` array (`byte_size > 64 KiB`) initialized once at program entry via `plan_bytes`.
    DeferredBss {
        /// Full root storage byte size.
        byte_size: u32,
        /// Serialized `DecodedInitPlan` restoring all defaults and `VALUE` clauses.
        plan_bytes: Vec<u8>,
    },
}

impl RootInitStrategy {
    /// Returns the inline byte slice for [`Self::StaticImage`], or `&[]` for [`Self::DeferredBss`].
    #[must_use]
    pub fn static_image_slice(&self) -> &[u8] {
        match self {
            Self::StaticImage(bytes) => bytes.as_slice(),
            Self::DeferredBss { .. } => &[],
        }
    }
}

/// Backing representation of a [`RootStorageRecord`] in generated MLIR/LLVM storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RootBackingKind {
    /// Direct `[byte_size x i8]` storage buffer (`WORKING-STORAGE` / `LOCAL-STORAGE`).
    DirectBuffer,
    /// Inherited `IS GLOBAL` storage buffer already emitted by an enclosing ancestor program.
    InheritedGlobalBuffer,
    /// 8-byte `!llvm.ptr` dynamic base-pointer slot (`BASED` / `LINKAGE SECTION`), initially `NULL`.
    IndirectBasePointer,
    /// 8-byte `!llvm.ptr` slot bound at program entry to a run-unit `IS EXTERNAL` segment.
    ExternalPointer,
}

/// Root `01` / `77` storage allocation emitted as a `cobol.storage` global.
#[derive(Debug, Clone)]
pub struct RootStorageRecord {
    /// Global symbol name (`__cobol_ws_<prog>_<id>`).
    pub sym_name: String,
    /// Primary `01` / `77` `DataItemId`.
    pub root_id: DataItemId,
    /// Direct buffer, inherited global, or 8-byte pointer slot.
    pub backing: RootBackingKind,
    /// Canonical or explicit `AS` segment name when `backing == RootBackingKind::ExternalPointer`.
    pub external_name: Option<String>,
    /// Required byte alignment (`1`, `2`, `4`, `8`, or `16`).
    pub alignment: u32,
    /// Total byte size of this root allocation (`>= 1`).
    pub byte_size: u32,
    /// Byte offsets of non-`REDEFINES` 8-byte pointer leaves (zeroed on ISO 2023 Rule 9 `ALLOCATE based-var`).
    pub pointer_zero_offsets: Vec<u32>,
    /// Static byte image or deferred `.bss` startup plan.
    pub init: RootInitStrategy,
}

/// Complete lowered storage map for one [`HirProgram`].
#[derive(Debug, Default)]
pub struct ProgramStoragePlan {
    /// Root `01` / `77` `cobol.storage` allocations in declaration order.
    pub roots: Vec<RootStorageRecord>,
    /// Per-`DataItemId` resolved location (`index == item.id.as_u32() as usize`).
    pub items: Vec<Option<ResolvedItemStorage>>,
}

/// Builds a [`RuntimeFieldSpec`] for `ty` under `fmt`.
///
/// # Arguments
///
/// * `ty` - HIR storage type.
/// * `fmt` - Program-level formatting conventions.
#[must_use]
pub const fn make_runtime_field_spec(ty: HirType, fmt: HirProgramFormatting) -> RuntimeFieldSpec {
    let (overpunch, packed_sign_rule) = match fmt.sign_convention {
        HirSignConvention::AsciiGnu => (OverpunchDialect::AsciiGnu, PackedSignPolicy::Gnu),
        HirSignConvention::EbcdicOverpunch => {
            (OverpunchDialect::Cobol85Standard, PackedSignPolicy::Gnu)
        }
        HirSignConvention::IbmMainframe => {
            (OverpunchDialect::Cobol85Standard, PackedSignPolicy::Ibm)
        }
        HirSignConvention::EbcdicNative => (OverpunchDialect::EbcdicIbm, PackedSignPolicy::Ibm),
    };
    RuntimeFieldSpec {
        desc: MoveFieldDesc {
            ty,
            overpunch,
            decimal_char: fmt.decimal_char,
            currency_symbol: fmt.currency_symbol,
            display_style: fmt.display_style,
        },
        packed_sign_rule,
    }
}

/// Validates that `ty` is supported in Milestone M5–M7 and returns its alignment (`1..=16`).
///
/// # Arguments
///
/// * `ty` - HIR type to validate.
/// * `source_map` - Source map for diagnostics.
/// * `span` - Item source span.
///
/// # Errors
///
/// Returns [`CoblangError::UnimplementedFeature`] on M9/M17/M20 types (`ODO`, `National`, `Utf8`, `Bit`, `ObjectRef`).
pub fn validate_supported_type(
    ty: &HirType,
    source_map: &SourceMap,
    span: coblang_common::SourceSpan,
) -> Result<u32> {
    let loc_str = source_map.resolve_span(span).map_or_else(
        |_| String::from("<unknown>:1:1"),
        |r| format!("{}:{}:{}", r.file_path, r.line.get(), r.column.get()),
    );
    match ty {
        HirType::Alphabetic { .. }
        | HirType::Alphanumeric { .. }
        | HirType::AlphanumericEdited { .. }
        | HirType::ZonedDecimal { .. }
        | HirType::PackedDecimal { .. }
        | HirType::NumericEdited { .. }
        | HirType::Group { .. }
        | HirType::Utf8 { .. } => Ok(1),
        HirType::BinaryInt { byte_width, .. } => Ok(match *byte_width {
            1 => 1,
            2 => 2,
            3..=4 => 4,
            5..=8 => 8,
            _ => 16,
        }),
        HirType::FloatBinary { width } => Ok(match width {
            coblang_hir::FloatBinaryWidth::Bits32 => 4,
            coblang_hir::FloatBinaryWidth::Bits64 => 8,
            coblang_hir::FloatBinaryWidth::Bits128 => 16,
        }),
        HirType::FloatDecimal { width } => Ok(match width {
            coblang_hir::FloatDecimalWidth::Decimal64 => 8,
            coblang_hir::FloatDecimalWidth::Decimal128 => 16,
        }),
        HirType::Pointer | HirType::ProgramPointer | HirType::Index | HirType::ObjectRef { .. } => {
            Ok(8)
        }
        HirType::National { .. } | HirType::NationalEdited { .. } => Ok(2),
        HirType::Bit { .. } => Err(CoblangError::UnimplementedFeature {
            feature: FeatureTag::DialectIso2023,
            construct: String::from("USAGE BIT"),
            location: loc_str,
        }),
    }
}

/// Builds the complete [`ProgramStoragePlan`] for `prog`, computing all root `01`/`77` initial byte buffers.
///
/// # Arguments
///
/// * `prog` - Source [`HirProgram`].
/// * `prog_sym` - Mangled program symbol prefix.
/// * `source_map` - Source map for diagnostics.
/// * `interner` - Symbol interner for literal text resolution.
///
/// # Errors
///
/// Returns [`CoblangError::UnimplementedFeature`] on `EXTERNAL`, `BASED`, `LINKAGE`, `FILE`, or `ODO` items.
pub fn build_program_storage_plan(
    prog: &HirProgram,
    prog_sym: &str,
    source_map: &SourceMap,
    interner: &SymbolInterner,
) -> Result<ProgramStoragePlan> {
    build_program_storage_plan_with_owners(prog, prog_sym, &BTreeMap::new(), source_map, interner)
}

/// Builds the complete [`ProgramStoragePlan`] for `prog`, resolving enclosing `IS GLOBAL` ancestor symbols via `program_symbols`.
///
/// # Arguments
///
/// * `prog` - Source [`HirProgram`].
/// * `prog_sym` - Mangled program symbol prefix.
/// * `program_symbols` - Module-wide `(canon_sym, raw_alias)` map keyed by [`ProgramUnitId`].
/// * `source_map` - Source map for diagnostics.
/// * `interner` - Symbol interner for literal text resolution.
///
/// # Errors
///
/// Returns [`CoblangError::UnimplementedFeature`] on `FILE` or unimplemented `USAGE` items.
pub fn build_program_storage_plan_with_owners(
    prog: &HirProgram,
    prog_sym: &str,
    program_symbols: &BTreeMap<ProgramUnitId, (String, String)>,
    source_map: &SourceMap,
    interner: &SymbolInterner,
) -> Result<ProgramStoragePlan> {
    let mut plan = ProgramStoragePlan {
        roots: Vec::new(),
        items: vec![None; prog.data_items.len()],
    };
    let mut root_idx_by_id: BTreeMap<DataItemId, usize> = BTreeMap::new();

    for item in &prog.data_items {
        let is_backed_special = item.section == HirStorageSection::SpecialRegister
            && !matches!(
                item.name.and_then(|s| interner.resolve_canonical(s).ok()),
                Some(
                    "RETURN-CODE"
                        | "WHEN-COMPILED"
                        | "XML-CODE"
                        | "XML-INFORMATION"
                        | "XML-EVENT"
                        | "XML-TEXT"
                        | "XML-NTEXT"
                        | "XML-NAMESPACE"
                        | "XML-NNAMESPACE"
                        | "XML-NAMESPACE-PREFIX"
                        | "XML-NNAMESPACE-PREFIX"
                        | "JSON-CODE"
                        | "JSON-STATUS"
                )
            );
        if matches!(
            item.level,
            LevelNumber::Condition88 | LevelNumber::Constant78
        ) || (matches!(
            item.section,
            HirStorageSection::SpecialRegister | HirStorageSection::CompileTimeConstant
        ) && !is_backed_special)
        {
            continue;
        }
        let align = validate_supported_type(&item.ty, source_map, item.span)?;

        let root_id = resolve_primary_root_id(item, &prog.data_items);
        let root_sec = prog
            .data_items
            .get(root_id.as_u32() as usize)
            .map_or(item.section, |ri| ri.section);
        let root_sym = match root_sec {
            HirStorageSection::InheritedGlobal {
                owner_unit,
                owner_root,
            } => {
                let owner_sym = program_symbols
                    .get(&owner_unit)
                    .map_or(prog_sym, |(s, _)| s.as_str());
                format!("__cobol_ws_{owner_sym}_{}", owner_root.as_u32())
            }
            _ => format!("__cobol_ws_{prog_sym}_{}", root_id.as_u32()),
        };
        let elem_len = item.ty.static_byte_size().max(1);
        let item_off = item.byte_offset.as_u32();
        let fd_min_root_len = match root_sec {
            HirStorageSection::FileSection(fc_id) => prog
                .files
                .iter()
                .find(|f| f.id == fc_id)
                .map_or(1, |f| f.max_record_bytes.max(1)),
            _ => 1,
        };
        let req_root_len = item_off
            .saturating_add(item.total_byte_size.max(elem_len))
            .max(fd_min_root_len);

        let r_idx = upsert_storage_root(
            item,
            (root_id, root_sec, &root_sym),
            (align, req_root_len, item_off),
            prog,
            interner,
            &mut plan,
            &mut root_idx_by_id,
        )?;

        let _ = plan.items.get_mut(item.id.as_u32() as usize).map(|slot| {
            *slot = Some(ResolvedItemStorage {
                root_storage_sym: root_sym,
                root_id,
                root_idx: r_idx,
                base_byte_offset: item_off,
                elem_byte_len: elem_len,
                field_spec: make_runtime_field_spec(item.ty.clone(), prog.formatting),
            });
        });
    }

    populate_initial_root_images(&mut plan, prog, interner)?;
    Ok(plan)
}

/// Classifies `root_sec` into `(RootBackingKind, Option<external_name>)`.
///
/// # Arguments
///
/// * `root_sec` - Root [`HirStorageSection`].
/// * `files` - Program file connectors (used to detect `FD ... EXTERNAL`).
/// * `interner` - Symbol interner.
fn resolve_root_backing(
    root_sec: HirStorageSection,
    files: &[coblang_hir::HirFileConnector],
    interner: &SymbolInterner,
) -> Result<(RootBackingKind, Option<String>)> {
    match root_sec {
        HirStorageSection::BasedStorage | HirStorageSection::Linkage => {
            Ok((RootBackingKind::IndirectBasePointer, None))
        }
        HirStorageSection::InheritedGlobal { .. } => {
            Ok((RootBackingKind::InheritedGlobalBuffer, None))
        }
        HirStorageSection::ExternalWorkingStorage {
            external_name,
            case_mode,
        } => {
            let resolved = match case_mode {
                HirExternalCaseMode::CanonicalUpper => interner.resolve_canonical(external_name)?,
                HirExternalCaseMode::ExactAlias => interner.resolve_original(external_name)?,
            };
            Ok((
                RootBackingKind::ExternalPointer,
                Some(String::from(resolved)),
            ))
        }
        HirStorageSection::FileSection(fc_id) => {
            if let Some(coblang_hir::HirConnectorSharing::External { external_name }) =
                files.iter().find(|f| f.id == fc_id).map(|f| &f.sharing)
            {
                Ok((
                    RootBackingKind::ExternalPointer,
                    Some(format!("__FD_{external_name}")),
                ))
            } else {
                Ok((RootBackingKind::DirectBuffer, None))
            }
        }
        _ => Ok((RootBackingKind::DirectBuffer, None)),
    }
}

/// Inserts or expands the [`RootStorageRecord`] for `root_id`, returning its index in `plan.roots`.
///
/// # Arguments
///
/// * `item` - Current [`HirDataItem`].
/// * `root_meta` - `(root_id, root_sec, root_sym)` tuple.
/// * `sizing` - `(align, req_root_len, item_off)` tuple.
/// * `prog` - Source [`HirProgram`].
/// * `interner` - Symbol interner.
/// * `plan` - Mutable [`ProgramStoragePlan`].
/// * `root_idx_by_id` - Map from `DataItemId` to index in `plan.roots`.
fn upsert_storage_root(
    item: &HirDataItem,
    root_meta: (DataItemId, HirStorageSection, &str),
    sizing: (u32, u32, u32),
    prog: &HirProgram,
    interner: &SymbolInterner,
    plan: &mut ProgramStoragePlan,
    root_idx_by_id: &mut BTreeMap<DataItemId, usize>,
) -> Result<usize> {
    let (root_id, root_sec, root_sym) = root_meta;
    let (align, req_root_len, item_off) = sizing;
    if let Some(&r_idx) = root_idx_by_id.get(&root_id) {
        if let Some(existing) = plan.roots.get_mut(r_idx) {
            existing.alignment = existing.alignment.max(align);
            existing.byte_size = existing.byte_size.max(req_root_len.max(1));
            if matches!(item.ty, HirType::Pointer | HirType::ProgramPointer)
                && !is_item_under_redefines(item, None, &prog.data_items)
            {
                collect_pointer_leaf_offsets(
                    item_off,
                    &item.dimensions,
                    &mut existing.pointer_zero_offsets,
                );
            }
        }
        Ok(r_idx)
    } else {
        let r_idx = plan.roots.len();
        let _ = root_idx_by_id.insert(root_id, r_idx);
        let is_rec_local = root_sec == HirStorageSection::LocalStorage
            && prog.recursivity == coblang_hir::HirRecursivity::Recursive
            && prog.nested.is_empty();
        let (backing, external_name) = if is_rec_local {
            (RootBackingKind::IndirectBasePointer, None)
        } else {
            resolve_root_backing(root_sec, &prog.files, interner)?
        };
        let mut pointer_zero_offsets = Vec::new();
        if matches!(item.ty, HirType::Pointer | HirType::ProgramPointer) {
            collect_pointer_leaf_offsets(item_off, &item.dimensions, &mut pointer_zero_offsets);
        }
        plan.roots.push(RootStorageRecord {
            sym_name: String::from(root_sym),
            root_id,
            backing,
            external_name,
            alignment: align,
            byte_size: req_root_len.max(1),
            pointer_zero_offsets,
            init: RootInitStrategy::StaticImage(Vec::new()),
        });
        Ok(r_idx)
    }
}

/// Walks `parent` and root-level `redefines` chains to find the primary root `01` / `77` [`DataItemId`].
///
/// # Arguments
///
/// * `item` - Starting [`HirDataItem`].
/// * `all_items` - Program data items slice.
fn resolve_primary_root_id(item: &HirDataItem, all_items: &[HirDataItem]) -> DataItemId {
    let mut cur = item;
    for _ in 0..all_items.len() {
        if let Some(parent_item) = cur
            .parent
            .and_then(|pid| all_items.get(pid.as_u32() as usize))
        {
            cur = parent_item;
        } else if let Some(redef_item) = cur
            .redefines
            .and_then(|rid| all_items.get(rid.as_u32() as usize))
        {
            cur = redef_item;
        } else {
            break;
        }
    }
    cur.id
}

/// Returns `true` if `item` or any ancestor up to `stop_parent` (exclusive) bears `REDEFINES` or `66 RENAMES`.
///
/// # Arguments
///
/// * `item` - Candidate item.
/// * `stop_parent` - Optional ancestor boundary (`None` checks all the way to root).
/// * `all_items` - Program data items slice.
#[must_use]
pub fn is_item_under_redefines(
    item: &HirDataItem,
    stop_parent: Option<DataItemId>,
    all_items: &[HirDataItem],
) -> bool {
    if matches!(item.level, LevelNumber::Renames66) {
        return true;
    }
    let mut cur = item;
    let mut guard = 0_usize;
    while guard < all_items.len() {
        guard += 1;
        if Some(cur.id) == stop_parent {
            return false;
        }
        if cur.redefines.is_some() {
            return true;
        }
        let Some(pid) = cur.parent else {
            return false;
        };
        if Some(pid) == stop_parent {
            return false;
        }
        let Some(p_item) = all_items.get(pid.as_u32() as usize) else {
            return false;
        };
        cur = p_item;
    }
    false
}

/// Allocates the initial `StaticImage` or `DeferredBss` backing strategies for every root in `plan`.
///
/// # Arguments
///
/// * `plan` - Mutable storage plan.
/// * `prog` - Source HIR program.
/// * `interner` - Symbol interner for literal text resolution.
fn allocate_blank_root_images(
    plan: &mut ProgramStoragePlan,
    prog: &HirProgram,
    interner: &SymbolInterner,
) -> Result<()> {
    let ws_fill = prog.formatting.default_byte.unwrap_or(0);
    for root in &mut plan.roots {
        if root.backing == RootBackingKind::InheritedGlobalBuffer {
            continue;
        }
        if matches!(
            root.backing,
            RootBackingKind::DirectBuffer | RootBackingKind::IndirectBasePointer
        ) && root.byte_size > MAX_INLINE_INIT_IMAGE_BYTES
        {
            let plan_bytes = build_root_startup_init_plan(root.root_id, prog, interner)?;
            root.init = RootInitStrategy::DeferredBss {
                byte_size: root.byte_size,
                plan_bytes,
            };
        } else {
            let bg = match root.backing {
                RootBackingKind::DirectBuffer => ws_fill,
                RootBackingKind::InheritedGlobalBuffer
                | RootBackingKind::IndirectBasePointer
                | RootBackingKind::ExternalPointer => 0,
            };
            root.init = RootInitStrategy::StaticImage(vec![bg; root.byte_size as usize]);
        }
    }
    Ok(())
}

/// Populates compile-time initial byte images (or deferred startup `.bss` plans) for `plan.roots`.
///
/// # Arguments
///
/// * `plan` - Mutable storage plan.
/// * `prog` - Source HIR program.
/// * `interner` - Symbol interner for literal text resolution.
fn populate_initial_root_images(
    plan: &mut ProgramStoragePlan,
    prog: &HirProgram,
    interner: &SymbolInterner,
) -> Result<()> {
    allocate_blank_root_images(plan, prog, interner)?;

    // Pass 1: apply default category initialization to primary elementary items on StaticImage roots
    // (when `default_byte` is set on `DirectBuffer`, preserve `default_byte` except zeroing pointer/index leaves).
    for item in &prog.data_items {
        if matches!(
            item.level,
            LevelNumber::Condition88 | LevelNumber::Constant78 | LevelNumber::Renames66
        ) || matches!(item.ty, HirType::Group { .. })
            || is_item_under_redefines(item, None, &prog.data_items)
        {
            continue;
        }
        let Some(Some(loc)) = plan.items.get(item.id.as_u32() as usize) else {
            continue;
        };
        let Some(RootStorageRecord {
            backing,
            init: RootInitStrategy::StaticImage(init_bytes),
            ..
        }) = plan.roots.get_mut(loc.root_idx)
        else {
            continue;
        };
        if init_bytes.is_empty() {
            continue;
        }
        let preserve_default_byte = *backing == RootBackingKind::DirectBuffer
            && prog.formatting.default_byte.is_some()
            && !matches!(
                item.ty,
                HirType::Pointer | HirType::ProgramPointer | HirType::Index
            );
        if preserve_default_byte {
            continue;
        }
        let _ = apply_across_dims(
            init_bytes,
            loc.base_byte_offset as usize,
            loc.elem_byte_len as usize,
            &item.dimensions,
            |slice| {
                encode_default_category_bytes(slice, &loc.field_spec.desc);
                Ok(())
            },
        );
    }

    // Pass 2: apply explicit `VALUE` clauses in declaration order on StaticImage roots.
    for item in &prog.data_items {
        if matches!(
            item.level,
            LevelNumber::Condition88 | LevelNumber::Constant78 | LevelNumber::Renames66
        ) {
            continue;
        }
        let Some(val) = &item.initial_value else {
            continue;
        };
        let Some(Some(loc)) = plan.items.get(item.id.as_u32() as usize) else {
            continue;
        };
        let Some(RootStorageRecord {
            init: RootInitStrategy::StaticImage(init_bytes),
            ..
        }) = plan.roots.get_mut(loc.root_idx)
        else {
            continue;
        };
        if init_bytes.is_empty() {
            continue;
        }
        apply_across_dims(
            init_bytes,
            loc.base_byte_offset as usize,
            loc.elem_byte_len as usize,
            &item.dimensions,
            |slice| encode_const_into_slice(val, slice, &loc.field_spec.desc, interner),
        )?;
    }
    Ok(())
}

/// Builds a startup [`DecodedInitPlan`] (`IncludeFiller`, `RestoreValue`) for a large `.bss` root `01`/`77` item.
///
/// # Arguments
///
/// * `root_id` - Primary root `DataItemId`.
/// * `prog` - Enclosing [`HirProgram`].
/// * `interner` - Symbol interner.
fn build_root_startup_init_plan(
    root_id: DataItemId,
    prog: &HirProgram,
    interner: &SymbolInterner,
) -> Result<Vec<u8>> {
    let mut entries = Vec::new();
    let idx = root_id.as_u32() as usize;
    for base_item in prog.data_items.get(idx..=idx).unwrap_or(&[]) {
        collect_item_or_group_init_entries(
            base_item,
            prog,
            interner,
            ElementaryDimInclusion::IncludeSelfDimensions,
            &mut entries,
        )?;
    }
    Ok(encode_init_plan(&DecodedInitPlan {
        filler_policy: FillerPolicy::IncludeFiller,
        to_value_mask: InitCategoryMask::ALL,
        default_mode: DefaultInitMode::IncludePointersAndBit,
        entries,
    }))
}

/// Appends the byte offsets of every 8-byte pointer element across `dims` into `out`.
///
/// # Arguments
///
/// * `base_off` - Base byte offset of element `(1, 1, ...)`.
/// * `dims` - Outer-to-inner fixed/maximum `OCCURS` dimensions.
/// * `out` - Output vector receiving 8-byte pointer slot offsets.
fn collect_pointer_leaf_offsets(base_off: u32, dims: &[HirOccursDim], out: &mut Vec<u32>) {
    let Some((first, rest)) = dims.split_first() else {
        out.push(base_off);
        return;
    };
    let max_n = first.max.min(65_536);
    for idx in 0..max_n {
        let off = base_off.saturating_add(idx.saturating_mul(first.element_byte_stride));
        collect_pointer_leaf_offsets(off, rest, out);
    }
}

/// Invokes `writer` on every multi-dimensional element slice described by `dims`.
///
/// # Arguments
///
/// * `buf` - Root byte buffer.
/// * `base_off` - Starting byte offset.
/// * `elem_len` - Single-element byte length.
/// * `dims` - Dimension table.
/// * `writer` - Element encoder callback.
fn apply_across_dims<F>(
    buf: &mut [u8],
    base_off: usize,
    elem_len: usize,
    dims: &[HirOccursDim],
    mut writer: F,
) -> Result<()>
where
    F: FnMut(&mut [u8]) -> Result<()>,
{
    apply_across_dims_ref(buf, base_off, elem_len, dims, &mut writer)
}

/// Recursive helper borrowing `writer` across nested `OCCURS` dimensions.
///
/// # Arguments
///
/// * `buf` - Root byte buffer.
/// * `base_off` - Current byte offset.
/// * `elem_len` - Leaf byte length.
/// * `dims` - Remaining dimensions.
/// * `writer` - Borrowed element encoder callback.
fn apply_across_dims_ref<F>(
    buf: &mut [u8],
    base_off: usize,
    elem_len: usize,
    dims: &[HirOccursDim],
    writer: &mut F,
) -> Result<()>
where
    F: FnMut(&mut [u8]) -> Result<()>,
{
    if let Some((first, rest)) = dims.split_first() {
        let count = first.max.clamp(1, 65_536) as usize;
        let stride = first.element_byte_stride as usize;
        for idx in 0..count {
            let sub_off = base_off.saturating_add(idx.saturating_mul(stride));
            apply_across_dims_ref(buf, sub_off, elem_len, rest, writer)?;
        }
        Ok(())
    } else {
        let end = base_off.saturating_add(elem_len);
        let _ = buf.get_mut(base_off..end).map(&mut *writer).transpose()?;
        Ok(())
    }
}

/// Writes default Working-Storage bytes (`SPACE` for alpha/edited, `0` for numeric, `0x00` for pointer/index) into `slice`.
///
/// # Arguments
///
/// * `slice` - Target element storage slice.
/// * `desc` - Field descriptor.
fn encode_default_category_bytes(slice: &mut [u8], desc: &MoveFieldDesc) {
    if crate::lower_m17::encode_default_m17_category_bytes(slice, desc) {
        return;
    }
    match desc.category() {
        DataCategory::Alphabetic
        | DataCategory::Alphanumeric
        | DataCategory::AlphanumericEdited
        | DataCategory::Group => {
            slice.fill(desc.overpunch.space_byte());
        }
        DataCategory::Numeric | DataCategory::NumericEdited => {
            let _ = execute_move(MoveSource::Decimal(FixedDecimal::ZERO), slice, desc);
        }
        _ => {
            slice.fill(0);
        }
    }
}

/// Encodes an [`HirConstValue`] into `slice` according to `desc` (preserving `VALUE -0` on signed `ZonedDecimal`!).
///
/// # Arguments
///
/// * `val` - Constant `VALUE` payload.
/// * `slice` - Mutable target element bytes.
/// * `desc` - Field descriptor.
/// * `interner` - Symbol interner for literal text decoding.
///
/// # Errors
///
/// Returns [`CoblangError`] if decimal literal parsing or target encoding fails.
pub fn encode_const_into_slice(
    val: &HirConstValue,
    slice: &mut [u8],
    desc: &MoveFieldDesc,
    interner: &SymbolInterner,
) -> Result<()> {
    match val {
        HirConstValue::Bytes(bytes) => {
            execute_move(MoveSource::AlphanumericLiteral(bytes), slice, desc)
        }
        HirConstValue::NationalBytes(be_bytes) => {
            let le_bytes = coblang_rt_core::field_ops::swap_utf16be_to_le(be_bytes);
            execute_move(MoveSource::NationalLiteralUtf16Le(&le_bytes), slice, desc)
        }
        HirConstValue::Utf8Bytes(u8_bytes) => {
            execute_move(MoveSource::Utf8Literal(u8_bytes), slice, desc)
        }
        HirConstValue::Figurative(fig) => {
            execute_move(MoveSource::Figurative(fig.clone()), slice, desc)
        }
        HirConstValue::Decimal { text, scale, sign } => {
            let raw_txt = interner.resolve_original(*text).unwrap_or("0").trim();
            let stripped = raw_txt.trim_start_matches(['+', '-']);
            let is_explicit_neg = *sign == HirDecimalSign::Negative || raw_txt.starts_with('-');
            let is_all_zero = !stripped.is_empty() && stripped.bytes().all(|b| b == b'0');
            if is_all_zero && is_explicit_neg && matches!(desc.ty, HirType::ZonedDecimal { .. }) {
                encode_zoned_negative_zero(slice, desc);
                return Ok(());
            }
            let ascii = format_hir_decimal_ascii(raw_txt, *scale, *sign);
            let parsed_dec = FixedDecimal::parse_str(&ascii)?;
            execute_move(MoveSource::Decimal(parsed_dec), slice, desc)
        }
        HirConstValue::Float(sym) => {
            let f_txt = interner.resolve_original(*sym).unwrap_or("0").trim();
            let parsed_dec = parse_scientific_or_decimal(f_txt)?;
            execute_move(MoveSource::Decimal(parsed_dec), slice, desc)
        }
    }
}

/// Parses an ordinary or scientific (`1.25E+02`) literal string into a [`FixedDecimal`].
///
/// # Arguments
///
/// * `text` - Floating-point or decimal literal string.
///
/// # Errors
///
/// Returns [`CoblangError::DecimalDomain`] if `text` is invalid.
pub fn parse_scientific_or_decimal(text: &str) -> Result<FixedDecimal> {
    if let Some(e_pos) = text.find(['e', 'E']) {
        let (mant_str, exp_part) = text.split_at(e_pos);
        let exp_str = exp_part.get(1..).unwrap_or("0");
        let exp_val: i16 = exp_str.parse().unwrap_or(0);
        let base = FixedDecimal::parse_str(mant_str)?;
        let new_sc = base.scale().saturating_sub(exp_val).clamp(-38, 38);
        FixedDecimal::try_new(base.mantissa(), new_sc)
    } else {
        FixedDecimal::parse_str(text)
    }
}

/// Formats `(raw_text, scale, sign)` from [`HirConstValue::Decimal`] into a canonical ASCII decimal string.
///
/// # Arguments
///
/// * `raw_text` - Interned digit string (may carry leading `+` or `-` for `IntegerLit`).
/// * `scale` - Scale factor (`> 0` fractional digits, `< 0` trailing `P` zeros).
/// * `sign` - Explicit decimal sign qualifier.
#[must_use]
pub fn format_hir_decimal_ascii(
    raw_text: &str,
    scale: coblang_common::ScaleFactor,
    sign: HirDecimalSign,
) -> String {
    let trimmed = raw_text.trim();
    let is_neg = sign == HirDecimalSign::Negative || trimmed.starts_with('-');
    let digits = trimmed.trim_start_matches(['+', '-']);
    let clean_digits = if digits.is_empty() { "0" } else { digits };
    let prefix = if is_neg { "-" } else { "" };
    let sc = scale.get();
    if sc <= 0 {
        let zeros = usize::try_from(-sc).unwrap_or(0);
        let mut out = String::with_capacity(prefix.len() + clean_digits.len() + zeros);
        out.push_str(prefix);
        out.push_str(clean_digits);
        for _ in 0..zeros {
            out.push('0');
        }
        out
    } else {
        let frac = usize::try_from(sc).unwrap_or(0);
        if frac >= clean_digits.len() {
            let pad = frac - clean_digits.len();
            let mut out = String::with_capacity(prefix.len() + 2 + frac);
            out.push_str(prefix);
            out.push_str("0.");
            for _ in 0..pad {
                out.push('0');
            }
            out.push_str(clean_digits);
            out
        } else {
            let split = clean_digits.len() - frac;
            let (int_p, frac_p) = clean_digits.split_at(split);
            format!("{prefix}{int_p}.{frac_p}")
        }
    }
}

/// Builds a serialized [`DecodedInitPlan`] byte vector for `target` inside `prog`.
///
/// # Arguments
///
/// * `target` - Resolved target [`HirPlace`].
/// * `filler` - `WITH FILLER` inclusion policy.
/// * `value_mode` - `TO VALUE` restoration policy.
/// * `prog` - Enclosing [`HirProgram`].
/// * `interner` - Symbol interner for literal text decoding.
///
/// # Errors
///
/// Returns [`CoblangError`] if pre-encoding an item's `VALUE` fails.
pub fn build_serialized_init_plan(
    target: &HirPlace,
    filler: HirInitFiller,
    value_mode: HirInitValueMode,
    prog: &HirProgram,
    interner: &SymbolInterner,
) -> Result<Vec<u8>> {
    let filler_policy = match filler {
        HirInitFiller::ExcludeFiller => FillerPolicy::SkipFiller,
        HirInitFiller::IncludeFiller => FillerPolicy::IncludeFiller,
    };
    let to_value_mask = match value_mode {
        HirInitValueMode::DefaultCategory => InitCategoryMask::NONE,
        HirInitValueMode::RestoreValue => InitCategoryMask::ALL,
    };

    if target.refmod.is_some() {
        let spec = make_runtime_field_spec(target.effective_ty.clone(), prog.formatting);
        let entry = RuntimeInitEntry {
            offset: 0,
            dims: Vec::new(),
            byte_len: target.effective_ty.static_byte_size().max(1),
            desc: spec.desc,
            filler: FillerMembership::NamedField,
            redefines: RedefinesMembership::PrimaryStorage,
            value_bytes: None,
        };
        return Ok(encode_init_plan(&DecodedInitPlan {
            filler_policy: FillerPolicy::IncludeFiller,
            to_value_mask,
            default_mode: DefaultInitMode::StandardCobol,
            entries: vec![entry],
        }));
    }

    let mut entries = Vec::new();
    let idx = target.item.as_u32() as usize;
    for base_item in prog.data_items.get(idx..=idx).unwrap_or(&[]) {
        collect_item_or_group_init_entries(
            base_item,
            prog,
            interner,
            ElementaryDimInclusion::SingleElement,
            &mut entries,
        )?;
    }

    Ok(encode_init_plan(&DecodedInitPlan {
        filler_policy,
        to_value_mask,
        default_mode: DefaultInitMode::StandardCobol,
        entries,
    }))
}

/// Policy controlling whether `base_item.dimensions` are encoded into an elementary [`RuntimeInitEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ElementaryDimInclusion {
    /// Include `base_item.dimensions` (used for root `77` table startup initialization).
    IncludeSelfDimensions,
    /// Omit `base_item.dimensions` (used for `INITIALIZE` on an already-subscripted elementary item).
    SingleElement,
}

/// Appends [`RuntimeInitEntry`] records for `base_item` (either a `Group`'s elementary descendants or a single elementary item).
///
/// # Arguments
///
/// * `base_item` - Target root or `INITIALIZE` item.
/// * `prog` - Enclosing [`HirProgram`].
/// * `interner` - Symbol interner.
/// * `dim_inclusion` - [`ElementaryDimInclusion`] mode (`IncludeSelfDimensions` vs `SingleElement`).
/// * `entries` - Output entry accumulator.
fn collect_item_or_group_init_entries(
    base_item: &HirDataItem,
    prog: &HirProgram,
    interner: &SymbolInterner,
    dim_inclusion: ElementaryDimInclusion,
    entries: &mut Vec<RuntimeInitEntry>,
) -> Result<()> {
    if matches!(base_item.ty, HirType::Group { .. }) {
        return collect_group_init_entries(base_item, prog, interner, entries);
    }
    let desc = make_runtime_field_spec(base_item.ty.clone(), prog.formatting).desc;
    let len = base_item.ty.static_byte_size().max(1);
    let vb =
        encode_effective_init_value(base_item, &prog.data_items, len as usize, &desc, interner)?;
    let dims = if dim_inclusion == ElementaryDimInclusion::IncludeSelfDimensions {
        base_item
            .dimensions
            .iter()
            .map(|d| (d.max.max(1), d.element_byte_stride))
            .collect()
    } else {
        Vec::new()
    };
    entries.push(RuntimeInitEntry {
        offset: 0,
        dims,
        byte_len: len,
        desc,
        filler: FillerMembership::NamedField,
        redefines: RedefinesMembership::PrimaryStorage,
        value_bytes: vb,
    });
    Ok(())
}

/// Collects [`RuntimeInitEntry`] records for all elementary descendants of `group_item`.
///
/// # Arguments
///
/// * `group_item` - Target group [`HirDataItem`].
/// * `prog` - Enclosing [`HirProgram`].
/// * `interner` - Symbol interner.
/// * `entries` - Output entry accumulator.
fn collect_group_init_entries(
    group_item: &HirDataItem,
    prog: &HirProgram,
    interner: &SymbolInterner,
    entries: &mut Vec<RuntimeInitEntry>,
) -> Result<()> {
    let base_dim_depth = group_item.dimensions.len();
    let start_idx = (group_item.id.as_u32() as usize).saturating_add(1);
    let tail = prog.data_items.get(start_idx..).unwrap_or(&[]);
    let mut odo_objects = BTreeSet::new();
    for cand in tail {
        if matches!(
            cand.level,
            LevelNumber::Condition88 | LevelNumber::Constant78 | LevelNumber::Renames66
        ) {
            continue;
        }
        if !is_descendant_of(cand, group_item.id, &prog.data_items) {
            break;
        }
        for dim in cand.dimensions.get(base_dim_depth..).unwrap_or(&[]) {
            if let Some(odo_id) = dim.depending_on {
                let _ = odo_objects.insert(odo_id);
            }
        }
    }
    for cand in tail {
        if matches!(
            cand.level,
            LevelNumber::Condition88 | LevelNumber::Constant78 | LevelNumber::Renames66
        ) {
            continue;
        }
        if !is_descendant_of(cand, group_item.id, &prog.data_items) {
            break;
        }
        if matches!(cand.ty, HirType::Group { .. }) {
            continue;
        }
        let rel_off = cand
            .byte_offset
            .as_u32()
            .saturating_sub(group_item.byte_offset.as_u32());
        let elem_len = cand.ty.static_byte_size().max(1);
        let desc = make_runtime_field_spec(cand.ty.clone(), prog.formatting).desc;
        let vb = encode_effective_init_value(
            cand,
            &prog.data_items,
            elem_len as usize,
            &desc,
            interner,
        )?;
        let filler = if cand.name.is_none() {
            FillerMembership::FillerField
        } else {
            FillerMembership::NamedField
        };
        let redefines = if odo_objects.contains(&cand.id)
            || is_item_under_redefines(cand, Some(group_item.id), &prog.data_items)
        {
            RedefinesMembership::RedefinesAlias
        } else {
            RedefinesMembership::PrimaryStorage
        };
        let inner_dims = cand.dimensions.get(base_dim_depth..).unwrap_or(&[]);
        let dims = inner_dims
            .iter()
            .map(|d| (d.max.max(1), d.element_byte_stride))
            .collect();
        entries.push(RuntimeInitEntry {
            offset: rel_off,
            dims,
            byte_len: elem_len,
            desc,
            filler,
            redefines,
            value_bytes: vb,
        });
    }
    Ok(())
}

/// Returns `true` if `cand` has `ancestor_id` in its `parent` chain.
///
/// # Arguments
///
/// * `cand` - Candidate child item.
/// * `ancestor_id` - Target ancestor group ID.
/// * `all_items` - Program data items slice.
fn is_descendant_of(
    cand: &HirDataItem,
    ancestor_id: DataItemId,
    all_items: &[HirDataItem],
) -> bool {
    let mut cur = cand.parent;
    for _ in 0..all_items.len() {
        if cur == Some(ancestor_id) {
            return true;
        }
        let Some(p_item) = cur.and_then(|pid| all_items.get(pid.as_u32() as usize)) else {
            break;
        };
        cur = p_item.parent;
    }
    false
}

/// Pre-encodes an item's declared `VALUE` (or an enclosing group's `VALUE`) for `INITIALIZE ... TO VALUE`.
///
/// # Arguments
///
/// * `item` - Elementary item.
/// * `all_items` - Program data items slice (to check inherited group `VALUE`).
/// * `elem_len` - Elementary byte length.
/// * `desc` - Field descriptor.
/// * `interner` - Symbol interner for literal text resolution.
fn encode_effective_init_value(
    item: &HirDataItem,
    all_items: &[HirDataItem],
    elem_len: usize,
    desc: &MoveFieldDesc,
    interner: &SymbolInterner,
) -> Result<Option<Vec<u8>>> {
    if let Some(v) = &item.initial_value {
        let mut buf = vec![0_u8; elem_len];
        encode_const_into_slice(v, &mut buf, desc, interner)?;
        return Ok(Some(buf));
    }
    let mut cur_p = item.parent;
    for _ in 0..all_items.len() {
        let Some(p_item) = cur_p.and_then(|pid| all_items.get(pid.as_u32() as usize)) else {
            break;
        };
        if let Some(gv) = &p_item.initial_value {
            let p_len = p_item.ty.static_byte_size().max(1) as usize;
            let p_desc = MoveFieldDesc {
                ty: p_item.ty.clone(),
                ..desc.clone()
            };
            let mut p_buf = vec![desc.overpunch.space_byte(); p_len];
            encode_const_into_slice(gv, &mut p_buf, &p_desc, interner)?;
            let rel = (item
                .byte_offset
                .as_u32()
                .saturating_sub(p_item.byte_offset.as_u32())) as usize;
            return Ok(p_buf
                .get(rel..rel.saturating_add(elem_len))
                .map(<[u8]>::to_vec));
        }
        cur_p = p_item.parent;
    }
    Ok(None)
}

/// Serializes a [`DecodedTableSortPlan`] byte vector for an in-memory `HirStmt::SortTable` operation.
///
/// # Arguments
///
/// * `spec` - Resolved [`HirSortTableSpec`].
/// * `prog` - Enclosing [`HirProgram`].
#[must_use]
pub fn build_serialized_table_sort_plan(spec: &HirSortTableSpec, prog: &HirProgram) -> Vec<u8> {
    let collating_ranks = spec.collating_ranks.as_ref().map(|ranks| {
        let mut tbl = [0_u8; 256];
        for (i, slot) in tbl.iter_mut().enumerate() {
            *slot = ranks
                .get(i)
                .copied()
                .unwrap_or_else(|| u8::try_from(i).unwrap_or(0));
        }
        tbl
    });
    let keys = spec
        .keys
        .iter()
        .map(|tk| RuntimeTableSortKey {
            direction: tk.direction,
            rel_offset: tk.rel_offset,
            byte_len: tk.byte_len.max(1),
            spec: make_runtime_field_spec(tk.ty.clone(), prog.formatting),
        })
        .collect();
    encode_table_sort_plan(&DecodedTableSortPlan {
        stride: spec.stride.max(1),
        collating_ranks,
        keys,
    })
}
