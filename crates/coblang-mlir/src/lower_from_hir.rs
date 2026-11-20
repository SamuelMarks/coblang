//! HIR-to-`cobol` MLIR 20 dialect lowering pass.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{
    CoblangError, DataItemId, DigitCount, FeatureTag, LevelNumber, ProgramUnitId, Result,
    ScaleFactor, SourceSpan,
};
use coblang_diag::{SourceMap, SymbolInterner};
use coblang_dynload::{SymbolMangleStyle, mangle_program_symbol};
use coblang_hir::{
    BinaryEndianness, DataCategory, DynamicOdoPresence, ExceptionCheckMask, HirAdvancing, HirBinOp,
    HirCondition, HirConstValue, HirDataItem, HirDisplayDevice, HirDivideRemainderSpec, HirExpr,
    HirFigurative, HirFlowExitKind, HirInitFiller, HirInitValueMode, HirInitialState, HirLoopKind,
    HirLoopTest, HirOccursDim, HirOdoSlideMode, HirPerformStyle, HirPlace, HirProgram,
    HirRecursivity, HirRelOp, HirReturnMode, HirRounding, HirSortTableSpec, HirStmt,
    HirStorageSection, HirTruthValue, HirType, ResolvedHirModule, Signedness, TruncationPolicy,
};
use coblang_rt_core::{
    LineAdvancing, OutputDevice, TrimDirection,
    desc_codec::{encode_data_category, encode_field_spec},
    field_ops::{
        ClassTestKind, DecimalArithOp, NegZeroPolicy, RelCmpOp, RuntimeMoveSourceKind,
        rounding_to_abi_code,
    },
};
use melior::dialect::arith::CmpiPredicate;
use melior::dialect::{arith, llvm, scf};
use melior::ir::attribute::{IntegerAttribute, TypeAttribute};
use melior::ir::block::BlockLike;
use melior::ir::r#type::IntegerType;
use melior::ir::{
    Block, Location, Module, Operation, OperationRef, Region, RegionLike, Type, Value,
};

use crate::cfg_perform::PERFORM_STACK_STORAGE_BYTES;
use crate::context::CobolMlirContext;
use crate::ops::{
    BoundsCheckMode, CobolAlterOp, CobolFieldViewOp, CobolFlowExitOp, CobolGetReturnCodeOp,
    CobolGobackOp, CobolGotoOp, CobolInitOp, CobolLoopOp, CobolMoveOp, CobolPerformRangeOp,
    CobolProgramOp, CobolRefModOp, CobolRtDispatchOp, CobolSentenceOp, CobolSetReturnCodeOp,
    CobolStopRunOp, CobolStorageOp, CobolSubscriptOp, CobolTrimOp, CobolYieldOp,
    EntryConventionMode, FieldAttrDesc, GobackMode, InitPolicy, InitialStateMode, MoveEncodingKind,
    RecursivityMode, RtDispatchTarget, StorageKind, llvm_ptr_type,
};
use crate::opt_fastpath::{
    FastAlphaSource, FastIntArithPlan, FastIntBinOp, FastIntBoundRule, FastIntExpr, FastMovePlan,
    try_classify_fast_int_arith, try_classify_fast_int_cmp, try_classify_fast_int_move,
    try_classify_fast_move,
};
use crate::storage_init::{
    ProgramStoragePlan, RootBackingKind, RootInitStrategy, build_program_storage_plan_with_owners,
    build_serialized_init_plan, build_serialized_table_sort_plan, format_hir_decimal_ascii,
    make_runtime_field_spec, validate_supported_type,
};
use crate::verifier::{VerifiedCobolMlirModule, verify_cobol_module};

/// Reserved C/POSIX/runtime symbol names that must never receive an unprefixed C alias.
const RESERVED_C_SYMBOLS: &[&str] = &[
    "main",
    "exit",
    "_exit",
    "abort",
    "stat",
    "open",
    "read",
    "write",
    "close",
    "free",
    "malloc",
    "calloc",
    "realloc",
    "printf",
    "puts",
    "system",
    "errno",
    "time",
    "localtime",
];

/// Shared module-level lowering state across all flattened programs in a compilation group.
pub(crate) struct ModuleLowerState<'a, 'ctx> {
    /// Owning MLIR context.
    pub(crate) ctx: &'ctx CobolMlirContext,
    /// Source map for exact file/line/column locations.
    pub(crate) source_map: &'a SourceMap,
    /// Symbol interner.
    pub(crate) interner: &'a SymbolInterner,
    /// Target `cobol` MLIR module being populated.
    pub(crate) module: Module<'ctx>,
    /// Case-folded uppercase top-level program name -> canonical mangled symbol.
    pub(crate) in_module_programs: BTreeMap<String, String>,
    /// Pre-assigned unique `(canon_sym, raw_c_alias)` per `ProgramUnitId`.
    pub(crate) program_symbols: BTreeMap<ProgramUnitId, (String, String)>,
    /// Deduplicated `StaticConst` byte blobs (`payload -> (sym_name, actual_byte_len)`).
    const_pool: BTreeMap<Vec<u8>, (String, u32)>,
    /// Monotonic counter for read-only literal/descriptor globals.
    const_counter: u32,
    /// Prepared OO COBOL class & method registration entries.
    pub(crate) oo_regs: Vec<crate::lower_m20::PreparedOoClassReg>,
    /// Monotonic counter for writable M20 scratch globals.
    m20_scratch_counter: u32,
    /// Per-unit Milestone M13 exception & NIST `DB` debug metadata indexed by `ProgramUnitId`.
    pub(crate) m13_units: BTreeMap<ProgramUnitId, &'a coblang_hir::HirM13UnitSpec>,
    /// Set of `ProgramUnitId`s representing `FUNCTION-ID` User-Defined Functions.
    pub(crate) udf_unit_ids: BTreeSet<ProgramUnitId>,
    /// Formal parameter types + formatting profile for in-module `__UDF_<EXT>` units.
    pub(crate) udf_param_types: BTreeMap<String, (Vec<HirType>, coblang_hir::HirProgramFormatting)>,
}

/// Dominated entry-block SSA constant and `CobolFieldViewOp` deduplication pool.
#[derive(Default)]
struct EntryValuePool<'ctx, 'b> {
    /// Deduplicated `i8` constants (`val -> Value`).
    i8_consts: BTreeMap<u8, Value<'ctx, 'b>>,
    /// Deduplicated `i32` constants (`val -> Value`).
    i32_consts: BTreeMap<i32, Value<'ctx, 'b>>,
    /// Deduplicated `i64` constants (`val -> Value`).
    i64_consts: BTreeMap<i64, Value<'ctx, 'b>>,
    /// Deduplicated `(storage_sym, offset, byte_len) -> !llvm.ptr` field views.
    field_views: BTreeMap<(String, u32, u32), Value<'ctx, 'b>>,
}

/// Execution context of a lowered `cobol.program` body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyExecContext {
    /// Top-level program entry (`GobackMode::ExitProgram`).
    ProgramEntry,
    /// Pre-paragraph `__cobol_proc_<sym>_entry` helper (`GobackMode::ParaFallthrough`, non-`ALTER`able).
    EntryHelper,
    /// Named paragraph/section `__cobol_proc_<sym>_<pid>` helper (`GobackMode::ParaFallthrough`).
    Paragraph {
        /// Paragraph `ProcedureId`.
        pid: u32,
        /// Inclusive last paragraph ID of the enclosing `SECTION` (or `pid` when unsectioned).
        section_end: u32,
    },
}

/// Per-program lowering context shared across the entry body and paragraph helper bodies.
pub(crate) struct ProgramLowerCtx<'p, 'ctx, 'b> {
    /// Source HIR program.
    pub(crate) prog: &'p HirProgram,
    /// Owning `RETURN-CODE` storage symbol (`__coblang_rc_<root_idx>`).
    rc_slot_sym: String,
    /// Shared 8-byte native `i64` subscript scratch slot (`__coblang_idx_scratch`).
    pub(crate) idx_scratch_sym: String,
    /// `DataItemId` of this scope's synthesized `RETURN-CODE` special register.
    return_code_item_id: Option<DataItemId>,
    /// Solved Working-Storage / Local-Storage plan.
    pub(crate) storage: &'p ProgramStoragePlan,
    /// Direct contained child uppercase program name -> assigned canonical symbol.
    pub(crate) local_children: &'p BTreeMap<String, String>,
    /// Canonical mangled symbol of the enclosing `HirProgram`.
    pub(crate) canon_sym: String,
    /// Execution context of the active `cobol.program` body.
    pub(crate) body_ctx: BodyExecContext,
    /// `true` when lowering a `FUNCTION-ID` User-Defined Function body.
    pub(crate) is_udf_unit: bool,
    /// `true` when the unit contains `>>TURN` directives or `EC-*` declaratives.
    pub(crate) has_ec_tracking: bool,
    /// `true` when the unit declares at least one `USE AFTER EXCEPTION` `EC-*` declarative.
    pub(crate) has_ec_declaratives: bool,
    /// Enclosing paragraph/section label for ISO `EXCEPTION-LOCATION` formatting.
    pub(crate) active_proc_label: Option<String>,
    /// Map of `ALTER`able paragraph IDs -> initial target ID (`-1` when bare `GO TO.`).
    alterable_procs: &'p BTreeMap<u32, i32>,
    /// Function entry block dominating all nested `scf.if` regions.
    pub(crate) entry_block: &'b Block<'ctx>,
    /// Anchor operation at index 0 of `entry_block` for inserting hoisted constants/views.
    seed_anchor: OperationRef<'ctx, 'b>,
    /// Dominated `i32` zero constant in the active function's entry block.
    pub(crate) fallback_val: Value<'ctx, 'b>,
    /// Deduplicated entry-block SSA constants and field views.
    entry_pool: RefCell<EntryValuePool<'ctx, 'b>>,
}

/// Lowered SSA view `(ptr, byte_len, spec, root_id)` for an addressable [`HirPlace`].
pub(crate) struct LoweredPlaceView<'ctx, 'b> {
    /// `!llvm.ptr` pointing to the first byte of the resolved element/slice.
    pub(crate) ptr: Value<'ctx, 'b>,
    /// `i64` byte length of the resolved element/slice.
    pub(crate) len_i64: Value<'ctx, 'b>,
    /// Effective runtime field descriptor.
    pub(crate) spec: coblang_rt_core::desc_codec::RuntimeFieldSpec,
    /// Root `01`/`77` storage ID (`None` for `RETURN-CODE` special register).
    pub(crate) root_id: Option<DataItemId>,
}

/// Formats a [`SourceSpan`] as `file:line:col` for error diagnostics.
///
/// # Arguments
///
/// * `map` - Source map.
/// * `span` - Span to format.
fn format_span_loc(map: &SourceMap, span: SourceSpan) -> String {
    match map.resolve_span(span) {
        Ok(loc) => format!("{}:{}:{}", loc.file_path, loc.line.get(), loc.column.get()),
        Err(_) => format!(
            "file#{}:offset#{}",
            span.file_id.as_u32(),
            span.start.as_u32()
        ),
    }
}

/// Builds a [`CoblangError::UnimplementedFeature`] with resolved source location.
///
/// # Arguments
///
/// * `feature` - Taxonomy [`FeatureTag`] blocking lowering.
/// * `construct` - Human-readable construct description.
/// * `map` - Source map.
/// * `span` - Construct source span.
pub(crate) fn unimpl(
    feature: FeatureTag,
    construct: impl Into<String>,
    map: &SourceMap,
    span: SourceSpan,
) -> CoblangError {
    CoblangError::UnimplementedFeature {
        feature,
        construct: construct.into(),
        location: format_span_loc(map, span),
    }
}

/// Appends `op` to `block` and returns its first result (or `fallback` if zero results).
///
/// # Arguments
///
/// * `block` - Target MLIR block.
/// * `op` - Newly constructed operation.
/// * `fallback` - Dominated fallback SSA value.
pub(crate) fn append_val<'ctx, 'b>(
    block: &'b Block<'ctx>,
    op: Operation<'ctx>,
    fallback: Value<'ctx, 'b>,
) -> Value<'ctx, 'b> {
    block
        .append_operation(op)
        .result(0)
        .map_or(fallback, Into::into)
}

/// Appends a 2-result `op` to `block` and returns `(res0, res1)`.
///
/// # Arguments
///
/// * `block` - Target MLIR block.
/// * `op` - Two-result operation (`cobol.refmod` or `cobol.trim`).
/// * `fallback` - Dominated fallback SSA value.
pub(crate) fn append_val_pair<'ctx, 'b>(
    block: &'b Block<'ctx>,
    op: Operation<'ctx>,
    fallback: Value<'ctx, 'b>,
) -> (Value<'ctx, 'b>, Value<'ctx, 'b>) {
    let op_ref = block.append_operation(op);
    let r0 = op_ref.result(0).map_or(fallback, Into::into);
    let r1 = op_ref.result(1).map_or(fallback, Into::into);
    (r0, r1)
}

/// Lowers a validated [`ResolvedHirModule`] into a [`VerifiedCobolMlirModule`].
///
/// # Arguments
///
/// * `ctx` - Initialized [`CobolMlirContext`].
/// * `hir` - Semantic-checked [`ResolvedHirModule`].
/// * `source_map` - [`SourceMap`] for location attachment.
/// * `interner` - [`SymbolInterner`] for identifier decoding.
///
/// # Errors
///
/// Returns [`CoblangError::UnimplementedFeature`] for constructs scheduled for Milestones M8–M20,
/// [`CoblangError::HirLowering`] if the module has zero programs, or [`CoblangError::MlirVerification`]
/// if dialect verification fails.
pub fn lower_hir_to_cobol_mlir<'ctx>(
    ctx: &'ctx CobolMlirContext,
    hir: &ResolvedHirModule,
    source_map: &SourceMap,
    interner: &SymbolInterner,
) -> Result<VerifiedCobolMlirModule<'ctx>> {
    if hir.0.programs.is_empty() && hir.0.classes.is_empty() && hir.0.functions.is_empty() {
        return Err(CoblangError::HirLowering {
            construct: "HirModule",
            location: String::from("<module>"),
            message: String::from("compilation group contains zero PROGRAM-ID units"),
        });
    }

    let mut flattened: Vec<(&HirProgram, String)> = Vec::new();
    let mut sym_tables = ProgramSymbolTables {
        in_module_programs: BTreeMap::new(),
        program_symbols: BTreeMap::new(),
        used_syms: BTreeSet::new(),
    };
    let mut emitted_rc_roots = Vec::new();
    let mut udf_unit_ids = BTreeSet::new();

    for (root_idx, root_prog) in hir.0.programs.iter().enumerate() {
        let rc_sym = format!("__coblang_rc_{root_idx}");
        emitted_rc_roots.push((rc_sym.clone(), root_prog.span));
        collect_program_tree(
            root_prog,
            &rc_sym,
            interner,
            &mut sym_tables,
            &mut flattened,
        );
    }
    for (cls_idx, cls) in hir.0.classes.iter().enumerate() {
        let rc_sym = format!("__coblang_rc_cls_{cls_idx}");
        emitted_rc_roots.push((rc_sym.clone(), cls.span));
        for (meth_idx, meth_prog) in cls.methods.iter().enumerate() {
            let logical = format!("__OO_M_{cls_idx}_{meth_idx}");
            let canon = mangle_program_symbol(&logical, SymbolMangleStyle::CobolCanonical);
            let _ = sym_tables.used_syms.insert(canon.clone());
            sym_tables.in_module_programs.insert(logical, canon.clone());
            sym_tables
                .program_symbols
                .insert(meth_prog.id, (canon, String::new()));
            flattened.push((meth_prog, rc_sym.clone()));
        }
    }
    let mut udf_param_types = BTreeMap::new();
    register_udf_units_m12(
        &hir.0.functions,
        interner,
        &mut sym_tables,
        &mut emitted_rc_roots,
        &mut udf_unit_ids,
        &mut udf_param_types,
        &mut flattened,
    );
    let oo_regs =
        crate::lower_m20::prepare_oo_class_registrations(&hir.0.classes, source_map, interner)?;
    let m13_units: BTreeMap<ProgramUnitId, &coblang_hir::HirM13UnitSpec> =
        hir.0.m13_units.iter().map(|u| (u.unit_id, u)).collect();

    let has_oo = !oo_regs.is_empty();
    let mut state = ModuleLowerState {
        ctx,
        source_map,
        interner,
        module: Module::new(ctx.unknown_location()),
        in_module_programs: sym_tables.in_module_programs,
        program_symbols: sym_tables.program_symbols,
        const_pool: BTreeMap::new(),
        const_counter: 0,
        oo_regs,
        m20_scratch_counter: 0,
        m13_units,
        udf_unit_ids,
        udf_param_types,
    };
    emit_module_shared_globals(has_oo, emitted_rc_roots, &mut state)?;

    for (prog, rc_sym) in flattened {
        lower_single_program(prog, &rc_sym, &mut state)?;
    }

    verify_cobol_module(state.module)
}

/// Emits module-wide OO startup guard, index scratch, and `RETURN-CODE` storage ops.
///
/// # Arguments
///
/// * `has_oo` - Whether the compilation group defines any `CLASS-ID` units.
/// * `emitted_rc_roots` - Root `(rc_sym, span)` list requiring 4-byte `RETURN-CODE` slots.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`CoblangError`] if emitting any module-level `cobol.storage` op fails.
fn emit_module_shared_globals<'ctx>(
    has_oo: bool,
    emitted_rc_roots: Vec<(String, SourceSpan)>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    if has_oo {
        let tmp = std::mem::take(&mut state.oo_regs);
        let res = crate::lower_m20::emit_oo_guard_global(&tmp, state);
        state.oo_regs = tmp;
        res?;
    }
    let idx_scratch_op = CobolStorageOp {
        sym_name: "__coblang_idx_scratch",
        storage_kind: StorageKind::Working,
        byte_size: 8,
        alignment: 8,
        init_bytes: &[0; 8],
    }
    .build(state.ctx, state.ctx.unknown_location())?;
    let _ = state.module.body().append_operation(idx_scratch_op);

    for (rc_sym, span) in emitted_rc_roots {
        let loc = state.loc(span)?;
        let rc_op = CobolStorageOp {
            sym_name: &rc_sym,
            storage_kind: StorageKind::Working,
            byte_size: 4,
            alignment: 4,
            init_bytes: &[0, 0, 0, 0],
        }
        .build(state.ctx, loc)?;
        let _ = state.module.body().append_operation(rc_op);
    }
    Ok(())
}

/// Accumulated symbol deduplication and lookup tables while flattening a [`HirProgram`] tree.
struct ProgramSymbolTables {
    /// Case-folded uppercase program name -> canonical mangled symbol.
    in_module_programs: BTreeMap<String, String>,
    /// Per-`ProgramUnitId` `(canon_sym, raw_c_alias)` assignment.
    program_symbols: BTreeMap<ProgramUnitId, (String, String)>,
    /// Set of already-allocated linker symbols in this module.
    used_syms: BTreeSet<String>,
}

/// Recursively collects `prog` and its nested subprograms with unique symbols and shared root `rc_slot_sym`.
///
/// # Arguments
///
/// * `prog` - Program node to visit.
/// * `rc_sym` - Root `RETURN-CODE` storage symbol inherited by contained programs.
/// * `interner` - Symbol interner.
/// * `tables` - Mutable [`ProgramSymbolTables`] tracking allocated symbols.
/// * `out` - Flattened `(program, rc_sym)` list.
fn collect_program_tree<'a>(
    prog: &'a HirProgram,
    rc_sym: &str,
    interner: &SymbolInterner,
    tables: &mut ProgramSymbolTables,
    out: &mut Vec<(&'a HirProgram, String)>,
) {
    let orig = interner.resolve_original(prog.name).unwrap_or("PROG");
    let base_canon = mangle_program_symbol(orig, SymbolMangleStyle::CobolCanonical);
    let canon = if tables.used_syms.insert(base_canon.clone()) {
        base_canon
    } else {
        let unique = format!("{base_canon}_u{}", prog.id.as_u32());
        let _ = tables.used_syms.insert(unique.clone());
        unique
    };
    let candidate_alias = compute_raw_c_alias(orig);
    let raw_alias =
        if !candidate_alias.is_empty() && tables.used_syms.insert(candidate_alias.clone()) {
            candidate_alias
        } else {
            String::new()
        };

    tables
        .in_module_programs
        .entry(orig.trim().to_ascii_uppercase())
        .or_insert_with(|| canon.clone());
    tables.program_symbols.insert(prog.id, (canon, raw_alias));
    out.push((prog, String::from(rc_sym)));
    for child in &prog.nested {
        collect_program_tree(child, rc_sym, interner, tables, out);
    }
}

impl<'ctx> ModuleLowerState<'_, 'ctx> {
    /// Resolves `span` to an MLIR [`Location`].
    ///
    /// # Arguments
    ///
    /// * `span` - Source span.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if `span` cannot be resolved in `source_map`.
    pub(crate) fn loc(&self, span: SourceSpan) -> Result<Location<'ctx>> {
        self.ctx.span_to_location(self.source_map, span)
    }

    /// Deduplicates and emits a read-only `cobol.storage` constant byte buffer, returning `(sym_name, actual_byte_len)`.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Payload bytes (`""` allocates a 1-byte placeholder with returned len `0`).
    /// * `loc` - MLIR source location.
    fn intern_const_bytes(&mut self, bytes: &[u8], loc: Location<'ctx>) -> Result<(String, u32)> {
        if let Some(existing) = self.const_pool.get(bytes) {
            return Ok(existing.clone());
        }
        let id = self.const_counter;
        self.const_counter = self.const_counter.saturating_add(1);
        let sym = format!("__coblang_const_{id}");
        let actual_len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let backing: &[u8] = if bytes.is_empty() { &[0] } else { bytes };
        let backing_len = u32::try_from(backing.len()).unwrap_or(1);
        let op = CobolStorageOp {
            sym_name: &sym,
            storage_kind: StorageKind::StaticConst,
            byte_size: backing_len,
            alignment: 1,
            init_bytes: backing,
        }
        .build(self.ctx, loc)?;
        let _ = self.module.body().append_operation(op);
        self.const_pool
            .insert(bytes.to_vec(), (sym.clone(), actual_len));
        Ok((sym, actual_len))
    }

    /// Serializes and deduplicates `spec` into a `StaticConst` global, returning `(sym_name, byte_len)`.
    ///
    /// # Arguments
    ///
    /// * `spec` - Runtime field specification.
    /// * `loc` - MLIR source location.
    fn intern_field_spec(
        &mut self,
        spec: &coblang_rt_core::desc_codec::RuntimeFieldSpec,
        loc: Location<'ctx>,
    ) -> Result<(String, u32)> {
        let encoded = encode_field_spec(spec);
        self.intern_const_bytes(&encoded, loc)
    }

    /// Allocates a writable `Working` global slot of `byte_size` bytes for M20 inline returns/scratch.
    ///
    /// # Arguments
    ///
    /// * `byte_size` - Requested byte capacity for the scratch global.
    /// * `loc` - MLIR source location for the `cobol.storage` op.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError`] if constructing the `cobol.storage` operation fails.
    pub(crate) fn alloc_m20_scratch_global(
        &mut self,
        byte_size: u32,
        loc: Location<'ctx>,
    ) -> Result<String> {
        let id = self.m20_scratch_counter;
        self.m20_scratch_counter = self.m20_scratch_counter.saturating_add(1);
        let sym = format!("__coblang_m20_scratch_{id}");
        let sz = byte_size.max(8);
        let zeros = vec![0_u8; usize::try_from(sz).unwrap_or(8)];
        let op = CobolStorageOp {
            sym_name: &sym,
            storage_kind: StorageKind::Working,
            byte_size: sz,
            alignment: 8,
            init_bytes: &zeros,
        }
        .build(self.ctx, loc)?;
        let _ = self.module.body().append_operation(op);
        Ok(sym)
    }
}

/// Computes a non-colliding raw C alias for `orig_name` (or `""` if reserved/invalid).
///
/// # Arguments
///
/// * `orig_name` - Original `PROGRAM-ID` spelling.
fn compute_raw_c_alias(orig_name: &str) -> String {
    let alias = mangle_program_symbol(orig_name, SymbolMangleStyle::RawC);
    if RESERVED_C_SYMBOLS
        .iter()
        .any(|&reserved| reserved.eq_ignore_ascii_case(&alias))
    {
        String::new()
    } else {
        alias
    }
}

/// Deferred `.bss` startup initialization descriptor for Working-Storage roots exceeding 64 KiB.
struct DeferredBssRoot {
    /// Global storage symbol name.
    sym_name: String,
    /// Declared root byte size.
    byte_size: u32,
    /// Pre-serialized `INITIALIZE TO VALUE` binary plan.
    plan_bytes: Vec<u8>,
}

/// Emits `CobolStorageOp` globals for `storage` and returns `(optional_guard_sym, deferred_roots)`.
///
/// # Arguments
///
/// * `storage` - Solved program Working-Storage plan.
/// * `canon_sym` - Mangled program symbol name.
/// * `prog_loc` - Program MLIR location.
/// * `state` - Module-wide lowering state.
fn emit_program_storage_globals(
    storage: &ProgramStoragePlan,
    canon_sym: &str,
    prog_loc: Location<'_>,
    state: &mut ModuleLowerState<'_, '_>,
) -> Result<(Option<String>, Vec<DeferredBssRoot>)> {
    let mut deferred_roots: Vec<DeferredBssRoot> = Vec::new();
    for root in &storage.roots {
        if root.backing == RootBackingKind::InheritedGlobalBuffer {
            continue;
        }
        if matches!(
            root.backing,
            RootBackingKind::IndirectBasePointer | RootBackingKind::ExternalPointer
        ) {
            let op = CobolStorageOp {
                sym_name: &root.sym_name,
                storage_kind: StorageKind::Working,
                byte_size: 8,
                alignment: 8,
                init_bytes: &[0; 8],
            }
            .build(state.ctx, prog_loc)?;
            let _ = state.module.body().append_operation(op);
            continue;
        }
        let init_slice: &[u8] = match &root.init {
            RootInitStrategy::StaticImage(bytes) => bytes.as_slice(),
            RootInitStrategy::DeferredBss {
                byte_size,
                plan_bytes,
            } => {
                deferred_roots.push(DeferredBssRoot {
                    sym_name: root.sym_name.clone(),
                    byte_size: *byte_size,
                    plan_bytes: plan_bytes.clone(),
                });
                &[0]
            }
        };
        let op = CobolStorageOp {
            sym_name: &root.sym_name,
            storage_kind: StorageKind::Working,
            byte_size: root.byte_size.max(1),
            alignment: root.alignment,
            init_bytes: init_slice,
        }
        .build(state.ctx, prog_loc)?;
        let _ = state.module.body().append_operation(op);
    }

    let bss_guard_sym = if deferred_roots.is_empty() {
        None
    } else {
        let gsym = format!("__cobol_bss_guard_{canon_sym}");
        let g_op = CobolStorageOp {
            sym_name: &gsym,
            storage_kind: StorageKind::Working,
            byte_size: 1,
            alignment: 1,
            init_bytes: &[0],
        }
        .build(state.ctx, prog_loc)?;
        let _ = state.module.body().append_operation(g_op);
        Some(gsym)
    };
    Ok((bss_guard_sym, deferred_roots))
}

/// Lowers one `HirProgram` (storage globals, main entry body, and paragraph helper ops).
///
/// # Arguments
///
/// * `prog` - Resolved HIR program.
/// * `rc_slot_sym` - Owning `RETURN-CODE` storage symbol.
/// * `state` - Module-wide lowering state.
fn lower_single_program(
    prog: &HirProgram,
    rc_slot_sym: &str,
    state: &mut ModuleLowerState<'_, '_>,
) -> Result<()> {
    let (canon_sym, raw_alias) = state
        .program_symbols
        .get(&prog.id)
        .cloned()
        .unwrap_or_default();

    let storage = build_program_storage_plan_with_owners(
        prog,
        &canon_sym,
        &state.program_symbols,
        state.source_map,
        state.interner,
    )?;
    let prog_loc = state.loc(prog.span)?;
    let (bss_guard_sym, deferred_roots) =
        emit_program_storage_globals(&storage, &canon_sym, prog_loc, state)?;

    let return_code_item_id = prog.data_items.iter().find_map(|item| {
        if item.section == HirStorageSection::SpecialRegister
            && item
                .name
                .and_then(|s| state.interner.resolve_canonical(s).ok())
                == Some("RETURN-CODE")
        {
            Some(item.id)
        } else {
            None
        }
    });

    let mut local_children = BTreeMap::new();
    for child in &prog.nested {
        let child_orig = state
            .interner
            .resolve_original(child.name)
            .unwrap_or("PROG");
        let (c_sym, _) = state
            .program_symbols
            .get(&child.id)
            .cloned()
            .unwrap_or_default();
        local_children.insert(child_orig.trim().to_ascii_uppercase(), c_sym);
    }

    let alterable_procs = collect_alterable_procs(prog);
    emit_perform_and_alter_globals(prog, &canon_sym, &alterable_procs, prog_loc, state)?;

    let body_env = ProgramBodySharedEnv {
        prog,
        canon_sym: &canon_sym,
        return_code_item_id,
        storage: &storage,
        local_children: &local_children,
        alterable_procs: &alterable_procs,
    };

    emit_program_entry_and_paragraphs(
        (&canon_sym, &raw_alias, rc_slot_sym),
        bss_guard_sym
            .as_deref()
            .map(|g| (g, deferred_roots.as_slice())),
        &body_env,
        state,
    )
}

/// Emits the main program entry `CobolProgramOp`, optional `__cobol_proc_<sym>_entry`, and all paragraph helpers.
///
/// # Arguments
///
/// * `syms` - `(canon_sym, raw_alias, rc_slot_sym)` tuple.
/// * `bss_startup` - Optional `(guard_sym, deferred_roots)` for `.bss` tables `> 64 KiB`.
/// * `body_env` - Shared [`ProgramBodySharedEnv`].
/// * `state` - Module lowering state.
fn emit_program_entry_and_paragraphs(
    syms: (&str, &str, &str),
    bss_startup: Option<(&str, &[DeferredBssRoot])>,
    body_env: &ProgramBodySharedEnv<'_>,
    state: &mut ModuleLowerState<'_, '_>,
) -> Result<()> {
    let (canon_sym, raw_alias, rc_slot_sym) = syms;
    let prog = body_env.prog;

    let normal_proc_limit = prog.fallthrough_limit_pid.map_or_else(
        || u32::try_from(prog.procedures.len()).unwrap_or(u32::MAX),
        coblang_common::ProcedureId::as_u32,
    );

    if normal_proc_limit == 0 {
        emit_program_or_para_body(
            &ProgramBodySpec {
                syms: (canon_sym, raw_alias, rc_slot_sym),
                stmts: &prog.entry_statements,
                trailing_range: None,
                body_ctx: BodyExecContext::ProgramEntry,
                bss_startup,
                span: prog.span,
            },
            body_env,
            state,
        )?;
    } else if prog.entry_statements.is_empty() {
        let end_bound = if prog.fallthrough_limit_pid.is_some() {
            normal_proc_limit - 1
        } else {
            u32::MAX
        };
        emit_program_or_para_body(
            &ProgramBodySpec {
                syms: (canon_sym, raw_alias, rc_slot_sym),
                stmts: &[],
                trailing_range: Some((0, end_bound)),
                body_ctx: BodyExecContext::ProgramEntry,
                bss_startup,
                span: prog.span,
            },
            body_env,
            state,
        )?;
    } else {
        let last_pid = normal_proc_limit - 1;
        emit_program_or_para_body(
            &ProgramBodySpec {
                syms: (canon_sym, raw_alias, rc_slot_sym),
                stmts: &[],
                trailing_range: Some((u32::MAX, last_pid)),
                body_ctx: BodyExecContext::ProgramEntry,
                bss_startup,
                span: prog.span,
            },
            body_env,
            state,
        )?;
        let entry_helper_sym = format!("__cobol_proc_{canon_sym}_entry");
        emit_program_or_para_body(
            &ProgramBodySpec {
                syms: (&entry_helper_sym, "", rc_slot_sym),
                stmts: &prog.entry_statements,
                trailing_range: None,
                body_ctx: BodyExecContext::EntryHelper,
                bss_startup: None,
                span: prog.span,
            },
            body_env,
            state,
        )?;
    }

    for para in &prog.procedures {
        let helper_sym = format!("__cobol_proc_{canon_sym}_{}", para.id.as_u32());
        emit_program_or_para_body(
            &ProgramBodySpec {
                syms: (&helper_sym, "", rc_slot_sym),
                stmts: &para.statements,
                trailing_range: None,
                body_ctx: BodyExecContext::Paragraph {
                    pid: para.id.as_u32(),
                    section_end: para.section_end.as_u32(),
                },
                bss_startup: None,
                span: para.span,
            },
            body_env,
            state,
        )?;
    }
    Ok(())
}

/// Per-body specification passed to [`emit_program_or_para_body`].
struct ProgramBodySpec<'a> {
    /// `(sym_name, raw_c_alias, rc_slot_sym)` tuple.
    syms: (&'a str, &'a str, &'a str),
    /// Statements to lower inside the entry block.
    stmts: &'a [HirStmt],
    /// Optional `(start_proc, end_proc)` sequential fallthrough range.
    trailing_range: Option<(u32, u32)>,
    /// Execution context (`ProgramEntry`, `EntryHelper`, or `Paragraph`).
    body_ctx: BodyExecContext,
    /// Optional `(guard_sym, deferred_roots)` for `.bss` tables `> 64 KiB`.
    bss_startup: Option<(&'a str, &'a [DeferredBssRoot])>,
    /// Originating source span.
    span: SourceSpan,
}

/// Shared program-level references passed to [`emit_program_or_para_body`].
struct ProgramBodySharedEnv<'a> {
    /// Enclosing HIR program.
    prog: &'a HirProgram,
    /// Canonical mangled program symbol.
    canon_sym: &'a str,
    /// `DataItemId` of `RETURN-CODE` special register.
    return_code_item_id: Option<DataItemId>,
    /// Solved Working-Storage plan.
    storage: &'a ProgramStoragePlan,
    /// Direct nested child programs.
    local_children: &'a BTreeMap<String, String>,
    /// `ALTER`able paragraph IDs -> initial target ID.
    alterable_procs: &'a BTreeMap<u32, i32>,
}

/// Scans `prog` for paragraphs modified by `ALTER` or containing a bare `GO TO.` statement.
///
/// # Arguments
///
/// * `prog` - Resolved HIR program.
fn collect_alterable_procs(prog: &HirProgram) -> BTreeMap<u32, i32> {
    let mut alter_targets = BTreeSet::new();
    scan_stmts_for_alter(&prog.entry_statements, &mut alter_targets);
    for para in &prog.procedures {
        scan_stmts_for_alter(&para.statements, &mut alter_targets);
    }

    let mut map = BTreeMap::new();
    for para in &prog.procedures {
        let pid = para.id.as_u32();
        let has_bare_goto = para.statements.iter().any(|s| {
            matches!(
                s,
                HirStmt::Jump {
                    targets,
                    depending_on: None,
                    ..
                } if targets.is_empty()
            )
        });
        if alter_targets.contains(&pid) || has_bare_goto {
            let init_target = para
                .statements
                .iter()
                .find_map(|s| match s {
                    HirStmt::Jump {
                        targets,
                        depending_on: None,
                        ..
                    } => targets.first().and_then(|t| i32::try_from(t.as_u32()).ok()),
                    _ => None,
                })
                .unwrap_or(-1);
            map.insert(pid, init_target);
        }
    }
    map
}

/// Recursively collects `from_proc` IDs referenced in any [`HirStmt::Alter`] inside `stmts`.
///
/// # Arguments
///
/// * `stmts` - Statement slice to inspect.
/// * `out` - Set of alterable paragraph IDs.
fn scan_stmts_for_alter(stmts: &[HirStmt], out: &mut BTreeSet<u32>) {
    for st in stmts {
        match st {
            HirStmt::Alter { pairs, .. } => {
                for (from_p, _) in pairs {
                    let _ = out.insert(from_p.as_u32());
                }
            }
            HirStmt::If {
                then_body,
                else_body,
                ..
            } => {
                scan_stmts_for_alter(then_body, out);
                scan_stmts_for_alter(else_body, out);
            }
            HirStmt::Loop {
                body, cycle_step, ..
            } => {
                scan_stmts_for_alter(body, out);
                scan_stmts_for_alter(cycle_step, out);
            }
            HirStmt::Sentence { body, .. } => scan_stmts_for_alter(body, out),
            HirStmt::AssignArith {
                on_size_error,
                not_on_size_error,
                ..
            } => {
                scan_stmts_for_alter(on_size_error, out);
                scan_stmts_for_alter(not_on_size_error, out);
            }
            HirStmt::DivideRemainder(spec) => {
                scan_stmts_for_alter(&spec.on_size_error, out);
                scan_stmts_for_alter(&spec.not_on_size_error, out);
            }
            HirStmt::Accept(spec) => {
                scan_stmts_for_alter(&spec.on_exception, out);
                scan_stmts_for_alter(&spec.not_on_exception, out);
            }
            HirStmt::StringVerb(spec) => {
                scan_stmts_for_alter(&spec.on_overflow, out);
                scan_stmts_for_alter(&spec.not_on_overflow, out);
            }
            HirStmt::Unstring(spec) => {
                scan_stmts_for_alter(&spec.on_overflow, out);
                scan_stmts_for_alter(&spec.not_on_overflow, out);
            }
            HirStmt::Call(spec) => {
                scan_stmts_for_alter(&spec.on_exception, out);
                scan_stmts_for_alter(&spec.not_on_exception, out);
            }
            _ => {}
        }
    }
}

/// Emits zero-heap-allocation `PerformStack` / `PerformContinuationSlot` and `ALTER` globals for `prog`.
///
/// # Arguments
///
/// * `prog` - Enclosing HIR program.
/// * `canon_sym` - Mangled program symbol.
/// * `alterable_procs` - Discovered `ALTER`able paragraph map.
/// * `prog_loc` - Program source location.
/// * `state` - Module lowering state.
fn emit_perform_and_alter_globals(
    prog: &HirProgram,
    canon_sym: &str,
    alterable_procs: &BTreeMap<u32, i32>,
    prog_loc: Location<'_>,
    state: &mut ModuleLowerState<'_, '_>,
) -> Result<()> {
    crate::lower_m11::emit_cancel_pending_global(canon_sym, prog_loc, state)?;
    if !prog.procedures.is_empty() {
        let (pfm_sym, pfm_bytes) = match prog.perform_style {
            HirPerformStyle::IsoStack => (
                format!("__cobol_pfm_stk_{canon_sym}"),
                PERFORM_STACK_STORAGE_BYTES,
            ),
            HirPerformStyle::OsVsContinuation => {
                let n = u32::try_from(prog.procedures.len()).unwrap_or(1).max(1);
                (
                    format!("__cobol_pfm_slots_{canon_sym}"),
                    n.saturating_mul(4),
                )
            }
        };
        let pfm_op = CobolStorageOp {
            sym_name: &pfm_sym,
            storage_kind: StorageKind::Working,
            byte_size: pfm_bytes,
            alignment: 4,
            init_bytes: &[0],
        }
        .build(state.ctx, prog_loc)?;
        let _ = state.module.body().append_operation(pfm_op);
    }

    for (&pid, &init_target) in alterable_procs {
        let asym = format!("__cobol_alter_{canon_sym}_{pid}");
        let init_bytes = init_target.to_ne_bytes();
        let a_op = CobolStorageOp {
            sym_name: &asym,
            storage_kind: StorageKind::Working,
            byte_size: 4,
            alignment: 4,
            init_bytes: &init_bytes,
        }
        .build(state.ctx, prog_loc)?;
        let _ = state.module.body().append_operation(a_op);
    }
    Ok(())
}

/// Emits one `CobolProgramOp` (either the main program entry or a paragraph helper).
///
/// # Arguments
///
/// * `spec` - Per-body [`ProgramBodySpec`].
/// * `env` - Shared [`ProgramBodySharedEnv`].
/// * `state` - Module lowering state.
fn emit_program_or_para_body(
    spec: &ProgramBodySpec<'_>,
    env: &ProgramBodySharedEnv<'_>,
    state: &mut ModuleLowerState<'_, '_>,
) -> Result<()> {
    let (sym_name, raw_c_alias, rc_slot_sym) = spec.syms;
    let loc = state.loc(spec.span)?;
    let region = Region::new();
    let entry_block = region.append_block(Block::new(&[]));

    let i32_ty = IntegerType::new(state.ctx.raw(), 32).into();
    let seed_op = arith::constant(
        state.ctx.raw(),
        IntegerAttribute::new(i32_ty, 0).into(),
        loc,
    );
    let appended_seed = entry_block.append_operation(seed_op);
    for seed_val in (0..1).filter_map(|i| appended_seed.result(i).ok().map(Into::into)) {
        let mut init_pool = EntryValuePool::default();
        init_pool.i32_consts.insert(0, seed_val);
        let is_udf_unit = state.udf_unit_ids.contains(&env.prog.id);
        let has_ec_declaratives = state
            .m13_units
            .get(&env.prog.id)
            .is_some_and(|u| !u.ec_declaratives.is_empty());
        let has_ec_tracking = has_ec_declaratives
            || env
                .prog
                .entry_statements
                .iter()
                .chain(env.prog.procedures.iter().flat_map(|p| p.statements.iter()))
                .any(|s| matches!(s, HirStmt::SetTurn(_) | HirStmt::CheckException(_)));
        let active_proc_label = resolve_active_proc_label(spec.body_ctx, env.prog, state.interner);
        let prog_ctx = ProgramLowerCtx {
            prog: env.prog,
            rc_slot_sym: String::from(rc_slot_sym),
            idx_scratch_sym: String::from("__coblang_idx_scratch"),
            return_code_item_id: env.return_code_item_id,
            storage: env.storage,
            local_children: env.local_children,
            canon_sym: String::from(env.canon_sym),
            body_ctx: spec.body_ctx,
            is_udf_unit,
            has_ec_tracking,
            has_ec_declaratives,
            active_proc_label,
            alterable_procs: env.alterable_procs,
            entry_block: &entry_block,
            seed_anchor: appended_seed,
            fallback_val: seed_val,
            entry_pool: RefCell::new(init_pool),
        };
        emit_body_startup_prologues(spec, env, &entry_block, &prog_ctx, state, loc)?;
        for stmt in spec.stmts {
            lower_stmt(stmt, &entry_block, &prog_ctx, state)?;
        }
        if let Some((sp, ep)) = spec.trailing_range {
            let pr_op = CobolPerformRangeOp {
                start_proc: sp,
                end_proc: ep,
            }
            .build(state.ctx, loc)?;
            let _ = entry_block.append_operation(pr_op);
        }
        if spec.body_ctx == BodyExecContext::ProgramEntry && !prog_ctx.is_udf_unit {
            crate::lower_m11::emit_returning_item_sync(&entry_block, &prog_ctx, state, loc)?;
        }
    }

    let end_mode = match spec.body_ctx {
        BodyExecContext::ProgramEntry => GobackMode::ExitProgram,
        BodyExecContext::EntryHelper | BodyExecContext::Paragraph { .. } => {
            GobackMode::ParaFallthrough
        }
    };
    let fallthrough = CobolGobackOp { mode: end_mode }.build(state.ctx, loc)?;
    let _ = entry_block.append_operation(fallthrough);

    let initial_state = match env.prog.initial_state {
        HirInitialState::RetainStorage => InitialStateMode::Retained,
        HirInitialState::ResetInitial => InitialStateMode::Initial,
    };
    let recursivity = match env.prog.recursivity {
        HirRecursivity::NonRecursive => RecursivityMode::NonRecursive,
        HirRecursivity::Recursive => RecursivityMode::Recursive,
    };
    let prog_op = CobolProgramOp {
        sym_name: String::from(sym_name),
        raw_c_alias: String::from(raw_c_alias),
        rc_slot_sym: String::from(rc_slot_sym),
        initial_state,
        recursivity,
        entry_convention: EntryConventionMode::Cobol,
        body: region,
    }
    .build(state.ctx, loc)?;
    let _ = state.module.body().append_operation(prog_op);
    Ok(())
}

/// Emits once-per-run-unit startup `InitExec` calls for `.bss` roots exceeding 64 KiB.
///
/// # Arguments
///
/// * `guard_sym` - Global guard byte symbol preventing duplicate re-init on re-entry.
/// * `roots` - Deferred `.bss` roots to initialize.
/// * `block` - Target entry block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_deferred_bss_startup<'ctx, 'b, 'e: 'b>(
    guard_sym: &str,
    roots: &[DeferredBssRoot],
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let i8_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 8).into();
    let guard_ptr = emit_storage_ptr(guard_sym, 0, 1, block, prog_ctx, state, loc)?;
    let cur_flag = append_val(
        block,
        llvm::load(
            state.ctx.raw(),
            guard_ptr,
            i8_ty,
            loc,
            llvm::LoadStoreOptions::new(),
        ),
        prog_ctx.fallback_val,
    );
    let zero_8 = emit_i8(0, block, prog_ctx, state, loc);
    let need_init = append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, cur_flag, zero_8, loc),
        prog_ctx.fallback_val,
    );

    let then_reg = Region::new();
    let then_blk = then_reg.append_block(Block::new(&[]));
    let one_8 = emit_i8(1, &then_blk, prog_ctx, state, loc);
    let _ = then_blk.append_operation(llvm::store(
        state.ctx.raw(),
        one_8,
        guard_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    for root in roots {
        let root_ptr = emit_storage_ptr(
            &root.sym_name,
            0,
            root.byte_size,
            &then_blk,
            prog_ctx,
            state,
            loc,
        )?;
        let len_64 = emit_i64(i64::from(root.byte_size), &then_blk, prog_ctx, state, loc);
        let (plan_ptr, plan_len) =
            emit_const_slice(&root.plan_bytes, &then_blk, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::InitBegin,
            &[],
            &then_blk,
            prog_ctx,
            state,
            loc,
        )?;
        let _ = emit_rt_call(
            RtDispatchTarget::InitExec,
            &[root_ptr, len_64, plan_ptr, plan_len],
            &then_blk,
            prog_ctx,
            state,
            loc,
        )?;
    }
    let _ = then_blk.append_operation(scf::r#yield(&[], loc));

    let else_reg = Region::new();
    let else_blk = else_reg.append_block(Block::new(&[]));
    let _ = else_blk.append_operation(scf::r#yield(&[], loc));
    let _ = block.append_operation(scf::r#if(need_init, &[], then_reg, else_reg, loc));
    Ok(())
}

/// Inserts a hoisted constant/field-view `op` right after `prog_ctx.seed_anchor` in `entry_block`.
///
/// # Arguments
///
/// * `op` - Newly constructed MLIR operation.
/// * `prog_ctx` - Program lowering context owning `entry_block`.
fn insert_hoisted_val<'ctx, 'b>(
    op: Operation<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'b>,
) -> Value<'ctx, 'b> {
    prog_ctx
        .entry_block
        .insert_operation_after(prog_ctx.seed_anchor, op)
        .result(0)
        .map_or(prog_ctx.fallback_val, Into::into)
}

/// Emits (or reuses a dominated entry-block) `i32` integer constant.
///
/// # Arguments
///
/// * `val` - Constant `i32` value.
/// * `_block` - Current insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
pub(crate) fn emit_i32<'ctx, 'b, 'e: 'b>(
    val: i32,
    _block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    if let Some(&cached) = prog_ctx.entry_pool.borrow().i32_consts.get(&val) {
        return cached;
    }
    let ty = IntegerType::new(state.ctx.raw(), 32).into();
    let op = arith::constant(
        state.ctx.raw(),
        IntegerAttribute::new(ty, i64::from(val)).into(),
        loc,
    );
    let v = insert_hoisted_val(op, prog_ctx);
    let _ = prog_ctx.entry_pool.borrow_mut().i32_consts.insert(val, v);
    v
}

/// Emits (or reuses a dominated entry-block) `i64` integer constant.
///
/// # Arguments
///
/// * `val` - Constant `i64` value.
/// * `_block` - Current insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
pub(crate) fn emit_i64<'ctx, 'b, 'e: 'b>(
    val: i64,
    _block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    if let Some(&cached) = prog_ctx.entry_pool.borrow().i64_consts.get(&val) {
        return cached;
    }
    let ty = IntegerType::new(state.ctx.raw(), 64).into();
    let op = arith::constant(state.ctx.raw(), IntegerAttribute::new(ty, val).into(), loc);
    let v = insert_hoisted_val(op, prog_ctx);
    let _ = prog_ctx.entry_pool.borrow_mut().i64_consts.insert(val, v);
    v
}

/// Emits (or reuses a dominated entry-block) `i8` integer constant.
///
/// # Arguments
///
/// * `val` - Constant `u8` byte value.
/// * `_block` - Current insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_i8<'ctx, 'b, 'e: 'b>(
    val: u8,
    _block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    if let Some(&cached) = prog_ctx.entry_pool.borrow().i8_consts.get(&val) {
        return cached;
    }
    let ty = IntegerType::new(state.ctx.raw(), 8).into();
    let op = arith::constant(
        state.ctx.raw(),
        IntegerAttribute::new(ty, i64::from(val)).into(),
        loc,
    );
    let v = insert_hoisted_val(op, prog_ctx);
    let _ = prog_ctx.entry_pool.borrow_mut().i8_consts.insert(val, v);
    v
}

/// Emits (or reuses a dominated entry-block) `!llvm.ptr` pointing to byte `offset` of `storage_sym`.
///
/// # Arguments
///
/// * `storage_sym` - Global storage symbol name.
/// * `offset` - Static byte offset inside `storage_sym`.
/// * `byte_len` - Field byte length.
/// * `_block` - Current insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`CoblangError`] if `cobol.field_view` operation construction fails.
pub(crate) fn emit_storage_ptr<'ctx, 'b, 'e: 'b>(
    storage_sym: &str,
    offset: u32,
    byte_len: u32,
    _block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let norm_len = byte_len.max(1);
    let key = (String::from(storage_sym), offset, norm_len);
    if let Some(&cached) = prog_ctx.entry_pool.borrow().field_views.get(&key) {
        return Ok(cached);
    }
    let fv = CobolFieldViewOp {
        storage_sym,
        byte_offset: offset,
        byte_len: norm_len,
        attr: FieldAttrDesc {
            kind_tag: 0,
            digits: 0,
            scale: 0,
        },
    }
    .build(state.ctx, loc)?;
    let v = insert_hoisted_val(fv, prog_ctx);
    let _ = prog_ctx.entry_pool.borrow_mut().field_views.insert(key, v);
    Ok(v)
}

/// Emits `(ptr: !llvm.ptr, len_i64: i64)` for a deduplicated read-only byte slice.
///
/// # Arguments
///
/// * `bytes` - Raw byte slice to intern.
/// * `block` - Current insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`CoblangError`] if constant storage or field-view emission fails.
pub(crate) fn emit_const_slice<'ctx, 'b, 'e: 'b>(
    bytes: &[u8],
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let (sym, actual_len) = state.intern_const_bytes(bytes, loc)?;
    let ptr = emit_storage_ptr(&sym, 0, actual_len.max(1), block, prog_ctx, state, loc)?;
    let len_val = emit_i64(i64::from(actual_len), block, prog_ctx, state, loc);
    Ok((ptr, len_val))
}

/// Emits `(spec_ptr: !llvm.ptr, spec_len_i64: i64)` for `spec`.
///
/// # Arguments
///
/// * `spec` - Runtime field descriptor to serialize and intern.
/// * `block` - Current insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`CoblangError`] if descriptor interning or field-view emission fails.
pub(crate) fn emit_spec_slice<'ctx, 'b, 'e: 'b>(
    spec: &coblang_rt_core::desc_codec::RuntimeFieldSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let (sym, len) = state.intern_field_spec(spec, loc)?;
    let ptr = emit_storage_ptr(&sym, 0, len.max(1), block, prog_ctx, state, loc)?;
    let len_val = emit_i64(i64::from(len), block, prog_ctx, state, loc);
    Ok((ptr, len_val))
}

/// Emits a `cobol.rt_dispatch` operation and returns its result (or `fallback_val` if void).
///
/// # Arguments
///
/// * `target` - Runtime dispatch ABI target.
/// * `operands` - SSA argument values.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`CoblangError`] if `cobol.rt_dispatch` construction fails.
pub(crate) fn emit_rt_call<'ctx, 'b, 'e: 'b>(
    target: RtDispatchTarget,
    operands: &[Value<'ctx, 'b>],
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let op = CobolRtDispatchOp { target, operands }.build(state.ctx, loc)?;
    Ok(append_val(block, op, prog_ctx.fallback_val))
}

/// Extracts the source [`SourceSpan`] of any [`HirExpr`].
///
/// # Arguments
///
/// * `expr` - HIR expression node.
pub(crate) fn expr_span(expr: &HirExpr) -> SourceSpan {
    match expr {
        HirExpr::Place(p) | HirExpr::AddressOf(p) | HirExpr::LengthOf(p) => p.span,
        HirExpr::Const { span, .. }
        | HirExpr::Binary { span, .. }
        | HirExpr::CallFunction { span, .. } => *span,
        HirExpr::Neg(inner) => expr_span(inner),
        HirExpr::M12Call(c) => c.span,
        HirExpr::M20(m) => match m.as_ref() {
            coblang_hir::HirM20Expr::OoSelf { span } => *span,
            coblang_hir::HirM20Expr::OoInlineInvoke(e) => e.span,
        },
    }
}

/// Resolves any addressable [`HirPlace`] (including `RETURN-CODE`, subscripts, and `RefMod`) into [`LoweredPlaceView`].
///
/// # Arguments
///
/// * `place` - Resolved HIR place expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`CoblangError`] if subscript, refmod, or storage view lowering fails.
pub(crate) fn lower_place_view<'ctx, 'b, 'e: 'b>(
    place: &HirPlace,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<LoweredPlaceView<'ctx, 'b>> {
    lower_place_view_with_policy(
        place,
        crate::lower_m11::NullPtrPolicy::CheckNonNull,
        block,
        prog_ctx,
        state,
    )
}

/// Lowers `place` into a [`LoweredPlaceView`] allowing `NULL` base pointers (propagating `NULL` for `ADDRESS OF`).
///
/// # Arguments
///
/// * `place` - Resolved HIR place expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`CoblangError`] if subscript, refmod, or storage view lowering fails.
pub(crate) fn lower_place_view_allow_null<'ctx, 'b, 'e: 'b>(
    place: &HirPlace,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<LoweredPlaceView<'ctx, 'b>> {
    lower_place_view_with_policy(
        place,
        crate::lower_m11::NullPtrPolicy::AllowNullForAddressOf,
        block,
        prog_ctx,
        state,
    )
}

/// Lowers `place` into a [`LoweredPlaceView`] under the specified [`crate::lower_m11::NullPtrPolicy`].
///
/// # Arguments
///
/// * `place` - Resolved HIR place expression.
/// * `policy` - Null-pointer check policy for `BASED` / `LINKAGE` roots.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_place_view_with_policy<'ctx, 'b, 'e: 'b>(
    place: &HirPlace,
    policy: crate::lower_m11::NullPtrPolicy,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<LoweredPlaceView<'ctx, 'b>> {
    let loc = state.loc(place.span)?;
    let _ = validate_supported_type(&place.effective_ty, state.source_map, place.span)?;

    if Some(place.item) == prog_ctx.return_code_item_id {
        return lower_return_code_place_view(block, prog_ctx, state, loc);
    }
    if let Some(m20_view) =
        crate::lower_m20::try_lower_m20_special_reg_view(place, block, prog_ctx, state, loc)?
    {
        return Ok(m20_view);
    }

    let Some(Some(item_loc)) = prog_ctx.storage.items.get(place.item.as_u32() as usize) else {
        return Err(unimpl(
            FeatureTag::FeatureMoveInit,
            "unresolved storage item",
            state.source_map,
            place.span,
        ));
    };

    let hir_item = prog_ctx.prog.data_items.get(place.item.as_u32() as usize);
    let (mut ptr, opt_raw_base) =
        crate::lower_m11::emit_item_base_ptr(item_loc, place, policy, block, prog_ctx, state, loc)?;

    if prog_ctx.prog.odo_slide == HirOdoSlideMode::SlideOffsets
        && let Some(slide_delta) = compute_slide_offset_delta(
            hir_item,
            place.check_mask,
            place.span,
            block,
            prog_ctx,
            state,
        )?
    {
        let raw = state.ctx.raw();
        let i8_ty = IntegerType::new(raw, 8).into();
        let ptr_ty = llvm_ptr_type(state.ctx);
        let gep = llvm::get_element_ptr_dynamic(raw, ptr, &[slide_delta], i8_ty, ptr_ty, loc);
        ptr = append_val(block, gep, prog_ctx.fallback_val);
    }

    if !place.subscripts.is_empty() {
        ptr = lower_place_subscripts(place, hir_item, ptr, block, prog_ctx, state, loc)?;
    }

    let (base_item_len_val, mut len_i64) = compute_place_item_lengths(
        place,
        hir_item,
        item_loc.elem_byte_len,
        block,
        prog_ctx,
        state,
        loc,
    )?;

    if let Some(rm) = &place.refmod {
        let base_view = RefModBaseView {
            ptr,
            byte_len: base_item_len_val,
        };
        let (sliced_ptr, sliced_len) =
            lower_place_refmod(place, rm, &base_view, block, prog_ctx, state, loc)?;
        ptr = sliced_ptr;
        len_i64 = sliced_len;
    }

    if policy == crate::lower_m11::NullPtrPolicy::AllowNullForAddressOf
        && let Some(raw_base) = opt_raw_base
    {
        ptr =
            crate::lower_m11::select_null_if_base_null(raw_base, ptr, block, prog_ctx, state, loc);
    }

    Ok(LoweredPlaceView {
        ptr,
        len_i64,
        spec: make_runtime_field_spec(place.effective_ty.clone(), prog_ctx.prog.formatting),
        root_id: Some(item_loc.root_id),
    })
}

/// Builds a [`LoweredPlaceView`] pointing to the 4-byte `RETURN-CODE` special-register slot.
///
/// # Arguments
///
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_return_code_place_view<'ctx, 'b, 'e: 'b>(
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<LoweredPlaceView<'ctx, 'b>> {
    let rc_ty = HirType::BinaryInt {
        byte_width: 4,
        digits: DigitCount::try_new(9)?,
        scale: ScaleFactor::ZERO,
        sign: Signedness::Signed,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::FullBinaryRange,
    };
    let ptr = emit_storage_ptr(&prog_ctx.rc_slot_sym, 0, 4, block, prog_ctx, state, loc)?;
    let len_i64 = emit_i64(4, block, prog_ctx, state, loc);
    Ok(LoweredPlaceView {
        ptr,
        len_i64,
        spec: make_runtime_field_spec(rc_ty, prog_ctx.prog.formatting),
        root_id: None,
    })
}

/// Base storage pointer and active byte length passed into [`lower_place_refmod`].
pub(crate) struct RefModBaseView<'ctx, 'b> {
    /// Base storage pointer (`!llvm.ptr`).
    pub(crate) ptr: Value<'ctx, 'b>,
    /// Active un-sliced byte length (`i64`).
    pub(crate) byte_len: Value<'ctx, 'b>,
}

/// Lowers `place.refmod` into an optional `EC-BOUND-REF-MOD` check followed by `cobol.refmod`.
///
/// # Arguments
///
/// * `place` - Enclosing [`HirPlace`].
/// * `rm` - [`coblang_hir::HirRefMod`] slice specification.
/// * `base_view` - Un-sliced [`RefModBaseView`] (`ptr` + `byte_len`).
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
pub(crate) fn lower_place_refmod<'ctx, 'b, 'e: 'b>(
    place: &HirPlace,
    rm: &coblang_hir::HirRefMod,
    base_view: &RefModBaseView<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let off_val = eval_expr_as_i64(&rm.offset, block, prog_ctx, state)?;
    let opt_len_val = if let Some(le) = &rm.length {
        Some(eval_expr_as_i64(le, block, prog_ctx, state)?)
    } else {
        None
    };
    let base_ty = prog_ctx
        .prog
        .data_items
        .get(place.item.as_u32() as usize)
        .map_or(&place.effective_ty, |it| &it.ty);
    if place.check_mask.contains(ExceptionCheckMask::BOUND_REF_MOD) {
        let site_label = format_span_file_line(place.span, state.source_map)?;
        let (loc_ptr, loc_len) =
            emit_const_slice(site_label.as_bytes(), block, prog_ctx, state, loc)?;
        let is_fatal = emit_i32(
            i32::from(place.check_mask.contains(ExceptionCheckMask::BOUND_FATAL)),
            block,
            prog_ctx,
            state,
            loc,
        );
        let (has_len_val, len_arg_val) = if let Some(lv) = opt_len_val {
            (emit_i32(1, block, prog_ctx, state, loc), lv)
        } else {
            (
                emit_i32(0, block, prog_ctx, state, loc),
                emit_i64(0, block, prog_ctx, state, loc),
            )
        };
        let max_units = crate::lower_m17::compute_refmod_max_units(
            base_ty,
            base_view.byte_len,
            block,
            prog_ctx,
            state,
            loc,
        );
        let _ = emit_rt_call(
            RtDispatchTarget::CheckRefMod,
            &[
                max_units,
                off_val,
                has_len_val,
                len_arg_val,
                is_fatal,
                loc_ptr,
                loc_len,
            ],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    let bounds_mode = if place.check_mask.contains(ExceptionCheckMask::BOUND_REF_MOD) {
        BoundsCheckMode::Checked
    } else {
        BoundsCheckMode::Unchecked
    };
    let rm_op = CobolRefModOp {
        view_ptr: base_view.ptr,
        base_len: base_view.byte_len,
        one_based_offset: off_val,
        opt_char_len: opt_len_val,
        char_width: crate::lower_m17::char_width_for_hir_type(base_ty),
        bounds_mode,
    }
    .build(state.ctx, loc)?;
    Ok(append_val_pair(block, rm_op, prog_ctx.fallback_val))
}

/// Formats `span` into a `file:line` diagnostic location string.
///
/// # Arguments
///
/// * `span` - Source span.
/// * `source_map` - Compilation source map.
fn format_span_file_line(span: SourceSpan, source_map: &SourceMap) -> Result<String> {
    let r = source_map.resolve_span(span)?;
    Ok(format!("{}:{}", r.file_path, r.line.get()))
}

/// Evaluates a scalar [`DataItemId`] (such as an `OCCURS DEPENDING ON` object) as an `i64` SSA value.
///
/// # Arguments
///
/// * `item_id` - Target scalar [`DataItemId`].
/// * `span` - Access site span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn eval_scalar_item_as_i64<'ctx, 'b, 'e: 'b>(
    item_id: DataItemId,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let loc = state.loc(span)?;
    let Some(odo_item) = prog_ctx.prog.data_items.get(item_id.as_u32() as usize) else {
        return Ok(emit_i64(0, block, prog_ctx, state, loc));
    };
    let odo_place = HirPlace {
        item: odo_item.id,
        static_offset: odo_item.byte_offset,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: odo_item.ty.clone(),
        check_mask: ExceptionCheckMask::NONE,
        span,
    };
    eval_expr_as_i64(&HirExpr::Place(odo_place), block, prog_ctx, state)
}

/// Evaluates `dim.depending_on` if present, emitting `coblang_rt_check_odo` when `BOUND_ODO` is active.
///
/// # Arguments
///
/// * `dim` - [`HirOccursDim`] descriptor.
/// * `check_mask` - Active [`ExceptionCheckMask`].
/// * `span` - Access site span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn eval_odo_dim_checked<'ctx, 'b, 'e: 'b>(
    dim: &HirOccursDim,
    check_mask: ExceptionCheckMask,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Option<Value<'ctx, 'b>>> {
    let Some(odo_id) = dim.depending_on else {
        return Ok(None);
    };
    let loc = state.loc(span)?;
    let odo_val = eval_scalar_item_as_i64(odo_id, span, block, prog_ctx, state)?;
    if check_mask.contains(ExceptionCheckMask::BOUND_ODO) {
        let site_text = format_span_file_line(span, state.source_map)?;
        let (loc_ptr, loc_len) =
            emit_const_slice(site_text.as_bytes(), block, prog_ctx, state, loc)?;
        let min_v = emit_i64(i64::from(dim.min), block, prog_ctx, state, loc);
        let max_v = emit_i64(i64::from(dim.max), block, prog_ctx, state, loc);
        let is_fatal = emit_i32(
            i32::from(check_mask.contains(ExceptionCheckMask::BOUND_FATAL)),
            block,
            prog_ctx,
            state,
            loc,
        );
        let _ = emit_rt_call(
            RtDispatchTarget::CheckOdo,
            &[odo_val, min_v, max_v, is_fatal, loc_ptr, loc_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(Some(odo_val))
}

/// Clamps `val` into `[0, max_u32]` as an `i64` SSA value so dynamic length calculations never underflow or overflow.
///
/// # Arguments
///
/// * `val` - Raw `i64` count value.
/// * `max_u32` - Upper bound.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn clamp_count_i64<'ctx, 'b, 'e: 'b>(
    val: Value<'ctx, 'b>,
    max_u32: u32,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    let raw = state.ctx.raw();
    let z64 = emit_i64(0, block, prog_ctx, state, loc);
    let max64 = emit_i64(i64::from(max_u32), block, prog_ctx, state, loc);
    let is_neg = append_val(
        block,
        arith::cmpi(raw, CmpiPredicate::Slt, val, z64, loc),
        prog_ctx.fallback_val,
    );
    let non_neg = append_val(
        block,
        arith::select(is_neg, z64, val, loc),
        prog_ctx.fallback_val,
    );
    let is_over = append_val(
        block,
        arith::cmpi(raw, CmpiPredicate::Sgt, non_neg, max64, loc),
        prog_ctx.fallback_val,
    );
    append_val(
        block,
        arith::select(is_over, max64, non_neg, loc),
        prog_ctx.fallback_val,
    )
}

/// Lowers `place.subscripts`, emitting `EC-BOUND-ODO` / `EC-BOUND-SUBSCRIPT` checks and dynamic `ODOSLIDE` strides when active.
///
/// # Arguments
///
/// * `place` - Subscripted [`HirPlace`].
/// * `hir_item` - Resolved [`HirDataItem`] if available.
/// * `base_ptr` - Base storage pointer before subscripting.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_place_subscripts<'ctx, 'b, 'e: 'b>(
    place: &HirPlace,
    hir_item: Option<&HirDataItem>,
    base_ptr: Value<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let mut idx_vals = Vec::with_capacity(place.subscripts.len());
    let mut strides = Vec::with_capacity(place.subscripts.len());
    let mut last_odo_val: Option<Value<'ctx, 'b>> = None;
    let check_sub = place
        .check_mask
        .contains(ExceptionCheckMask::BOUND_SUBSCRIPT);
    let dyn_ec_sub = prog_ctx.has_ec_tracking;
    let loc_slice_opt = if check_sub || dyn_ec_sub {
        let base_label = format_span_file_line(place.span, state.source_map)?;
        let item_orig = hir_item
            .and_then(|it| it.name)
            .and_then(|sym| state.interner.resolve_original(sym).ok())
            .unwrap_or("?");
        let site_label = format!("{base_label}\t{item_orig}");
        Some(emit_const_slice(
            site_label.as_bytes(),
            block,
            prog_ctx,
            state,
            loc,
        )?)
    } else {
        None
    };
    let is_fatal = emit_i32(
        if dyn_ec_sub {
            if prog_ctx.has_ec_declaratives { -1 } else { -2 }
        } else {
            i32::from(place.check_mask.contains(ExceptionCheckMask::BOUND_FATAL))
        },
        block,
        prog_ctx,
        state,
        loc,
    );

    for (dim_idx, (sub_expr, stride)) in place.subscripts.iter().enumerate() {
        let mut idx_val = eval_expr_as_i64(sub_expr, block, prog_ctx, state)?;
        if let Some(dim) = hir_item.and_then(|it| it.dimensions.get(dim_idx)) {
            let odo_opt =
                eval_odo_dim_checked(dim, place.check_mask, place.span, block, prog_ctx, state)?;
            if odo_opt.is_some() {
                last_odo_val = odo_opt;
            }
            if let Some((loc_ptr, loc_len)) = loc_slice_opt {
                idx_val = emit_checked_subscript_dim(
                    (idx_val, dim.max, odo_opt),
                    (is_fatal, loc_ptr, loc_len, place.span),
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            }
        }
        idx_vals.push(idx_val);
        strides.push(i32::try_from(*stride).unwrap_or(i32::MAX));
    }

    if prog_ctx.prog.odo_slide == HirOdoSlideMode::SlideOffsets
        && let Some(slid_ptr) = try_lower_slide_subscripts(
            hir_item, &idx_vals, base_ptr, place.span, block, prog_ctx, state,
        )?
    {
        return Ok(slid_ptr);
    }

    let bounds_mode = if check_sub {
        BoundsCheckMode::Checked
    } else {
        BoundsCheckMode::Unchecked
    };
    let sub_op = CobolSubscriptOp {
        base_ptr,
        indices: &idx_vals,
        strides: &strides,
        odo_limit: last_odo_val,
        bounds_mode,
    }
    .build(state.ctx, loc)?;
    Ok(append_val(block, sub_op, prog_ctx.fallback_val))
}

/// Lowers dynamic `ODOSLIDE` subscript strides when an ancestor dimension owns a dynamic ODO group.
///
/// # Arguments
///
/// * `item_opt` - Subscripted [`HirDataItem`], if resolved.
/// * `idx_vals` - Evaluated 1-based `i64` subscript values.
/// * `base_ptr` - Base storage pointer.
/// * `span` - Access site span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn try_lower_slide_subscripts<'ctx, 'b, 'e: 'b>(
    item_opt: Option<&HirDataItem>,
    idx_vals: &[Value<'ctx, 'b>],
    base_ptr: Value<'ctx, 'b>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Option<Value<'ctx, 'b>>> {
    let dim_owners =
        item_opt.map_or_default(|it| collect_dimension_owner_items(it, &prog_ctx.prog.data_items));
    if !dim_owners.iter().any(|owner| {
        matches!(
            owner.ty,
            HirType::Group {
                odo_presence: DynamicOdoPresence::HasDynamicOdo,
                ..
            }
        )
    }) {
        return Ok(None);
    }
    let loc = state.loc(span)?;
    let raw = state.ctx.raw();
    let i8_ty = IntegerType::new(raw, 8).into();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let one64 = emit_i64(1, block, prog_ctx, state, loc);
    let mut cur_ptr = base_ptr;
    for (idx_val, owner) in idx_vals.iter().copied().zip(dim_owners.iter().copied()) {
        let dyn_stride = compute_dynamic_elem_len(
            owner,
            ExceptionCheckMask::NONE,
            span,
            block,
            prog_ctx,
            state,
        )?;
        let zero_based = append_val(
            block,
            arith::subi(idx_val, one64, loc),
            prog_ctx.fallback_val,
        );
        let byte_delta = append_val(
            block,
            arith::muli(zero_based, dyn_stride, loc),
            prog_ctx.fallback_val,
        );
        let gep = llvm::get_element_ptr_dynamic(raw, cur_ptr, &[byte_delta], i8_ty, ptr_ty, loc);
        cur_ptr = append_val(block, gep, prog_ctx.fallback_val);
    }
    Ok(Some(cur_ptr))
}

/// Collects the ordered [`HirDataItem`] owners for each dimension inherited by `item` (outermost to innermost).
///
/// # Arguments
///
/// * `item` - Subscripted [`HirDataItem`].
/// * `all_items` - Program data item table.
fn collect_dimension_owner_items<'a>(
    item: &'a HirDataItem,
    all_items: &'a [HirDataItem],
) -> Vec<&'a HirDataItem> {
    let mut chain = Vec::new();
    let mut cur_opt = Some(item);
    for _ in 0..all_items.len() {
        let Some(cur) = cur_opt else {
            break;
        };
        chain.push(cur);
        cur_opt = cur
            .parent
            .and_then(|pid| all_items.get(pid.as_u32() as usize));
    }
    chain.reverse();
    let mut owners = Vec::with_capacity(item.dimensions.len());
    let mut prev_depth = 0_usize;
    for anc in chain {
        if anc.dimensions.len() > prev_depth {
            for _ in prev_depth..anc.dimensions.len() {
                owners.push(anc);
            }
            prev_depth = anc.dimensions.len();
        }
    }
    owners
}

/// Collects direct primary (non-`REDEFINES`, non-`66`/`78`/`88`) children of `group_id`.
///
/// # Arguments
///
/// * `group_id` - Parent group [`DataItemId`].
/// * `all_items` - Program data item table.
fn direct_primary_children(group_id: DataItemId, all_items: &[HirDataItem]) -> Vec<&HirDataItem> {
    let start_idx = (group_id.as_u32() as usize).saturating_add(1);
    let mut out = Vec::new();
    for cand in all_items.get(start_idx..).unwrap_or(&[]) {
        if matches!(
            cand.level,
            LevelNumber::Condition88 | LevelNumber::Constant78 | LevelNumber::Renames66
        ) {
            continue;
        }
        if cand.parent == Some(group_id) {
            if cand.redefines.is_none() {
                out.push(cand);
            }
        } else if matches!(
            cand.level,
            LevelNumber::Hierarchy(1) | LevelNumber::Independent77
        ) {
            break;
        }
    }
    out
}

/// Computes the active runtime byte length of a single occurrence of `item` (handling nested ODO and `odoslide`).
///
/// # Arguments
///
/// * `item` - Target [`HirDataItem`].
/// * `check_mask` - Active [`ExceptionCheckMask`].
/// * `span` - Access site span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn compute_dynamic_elem_len<'ctx, 'b, 'e: 'b>(
    item: &HirDataItem,
    check_mask: ExceptionCheckMask,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let loc = state.loc(span)?;
    if !matches!(
        item.ty,
        HirType::Group {
            odo_presence: DynamicOdoPresence::HasDynamicOdo,
            ..
        }
    ) {
        return Ok(emit_i64(
            i64::from(item.ty.static_byte_size().max(1)),
            block,
            prog_ctx,
            state,
            loc,
        ));
    }
    let children = direct_primary_children(item.id, &prog_ctx.prog.data_items);
    let Some(last_child) = children.last().copied() else {
        return Ok(emit_i64(
            i64::from(item.ty.static_byte_size().max(1)),
            block,
            prog_ctx,
            state,
            loc,
        ));
    };

    if prog_ctx.prog.odo_slide == HirOdoSlideMode::SlideOffsets {
        let mut acc = emit_i64(0, block, prog_ctx, state, loc);
        let mut prev_static_end = item.byte_offset.as_u32();
        for child in children {
            let slack = child.byte_offset.as_u32().saturating_sub(prev_static_end);
            if slack > 0 {
                let sv = emit_i64(i64::from(slack), block, prog_ctx, state, loc);
                acc = append_val(block, arith::addi(acc, sv, loc), prog_ctx.fallback_val);
            }
            let c_bytes = compute_child_active_span(
                child,
                item.dimensions.len(),
                check_mask,
                span,
                block,
                prog_ctx,
                state,
            )?;
            acc = append_val(block, arith::addi(acc, c_bytes, loc), prog_ctx.fallback_val);
            prev_static_end = child
                .byte_offset
                .as_u32()
                .saturating_add(child.total_byte_size);
        }
        return Ok(acc);
    }

    let prefix = i64::from(
        last_child
            .byte_offset
            .as_u32()
            .saturating_sub(item.byte_offset.as_u32()),
    );
    let tail_bytes = compute_child_active_span(
        last_child,
        item.dimensions.len(),
        check_mask,
        span,
        block,
        prog_ctx,
        state,
    )?;
    if prefix == 0 {
        Ok(tail_bytes)
    } else {
        let pv = emit_i64(prefix, block, prog_ctx, state, loc);
        Ok(append_val(
            block,
            arith::addi(pv, tail_bytes, loc),
            prog_ctx.fallback_val,
        ))
    }
}

/// Computes the active byte span contributed by direct child `child` inside a parent of depth `parent_dim_depth`.
///
/// # Arguments
///
/// * `child` - Direct child [`HirDataItem`].
/// * `parent_dim_depth` - Dimension count inherited from parent.
/// * `check_mask` - Active [`ExceptionCheckMask`].
/// * `span` - Access site span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn compute_child_active_span<'ctx, 'b, 'e: 'b>(
    child: &HirDataItem,
    parent_dim_depth: usize,
    check_mask: ExceptionCheckMask,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let loc = state.loc(span)?;
    let elem_len = compute_dynamic_elem_len(child, check_mask, span, block, prog_ctx, state)?;
    if let Some(own_dim) = child
        .dimensions
        .last()
        .filter(|_| child.dimensions.len() > parent_dim_depth)
    {
        let cnt_val = if let Some(raw_odo) =
            eval_odo_dim_checked(own_dim, check_mask, span, block, prog_ctx, state)?
        {
            clamp_count_i64(raw_odo, own_dim.max, block, prog_ctx, state, loc)
        } else {
            emit_i64(i64::from(own_dim.max), block, prog_ctx, state, loc)
        };
        if prog_ctx.prog.odo_slide == HirOdoSlideMode::FixedOffsets
            && matches!(
                child.ty,
                HirType::Group {
                    odo_presence: DynamicOdoPresence::HasDynamicOdo,
                    ..
                }
            )
        {
            let raw = state.ctx.raw();
            let z64 = emit_i64(0, block, prog_ctx, state, loc);
            let one64 = emit_i64(1, block, prog_ctx, state, loc);
            let stride64 = emit_i64(
                i64::from(own_dim.element_byte_stride),
                block,
                prog_ctx,
                state,
                loc,
            );
            let is_zero = append_val(
                block,
                arith::cmpi(raw, CmpiPredicate::Sle, cnt_val, z64, loc),
                prog_ctx.fallback_val,
            );
            let cnt_minus_one = append_val(
                block,
                arith::subi(cnt_val, one64, loc),
                prog_ctx.fallback_val,
            );
            let prev_bytes = append_val(
                block,
                arith::muli(cnt_minus_one, stride64, loc),
                prog_ctx.fallback_val,
            );
            let non_zero_span = append_val(
                block,
                arith::addi(prev_bytes, elem_len, loc),
                prog_ctx.fallback_val,
            );
            return Ok(append_val(
                block,
                arith::select(is_zero, z64, non_zero_span, loc),
                prog_ctx.fallback_val,
            ));
        }
        return Ok(append_val(
            block,
            arith::muli(cnt_val, elem_len, loc),
            prog_ctx.fallback_val,
        ));
    }
    Ok(elem_len)
}

/// Computes the signed byte delta (`<= 0`) caused by preceding `OCCURS DEPENDING ON` siblings under `ODOSLIDE`.
///
/// # Arguments
///
/// * `item_opt` - Target [`HirDataItem`], if resolved.
/// * `check_mask` - Active [`ExceptionCheckMask`].
/// * `span` - Access site span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn compute_slide_offset_delta<'ctx, 'b, 'e: 'b>(
    item_opt: Option<&HirDataItem>,
    check_mask: ExceptionCheckMask,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Option<Value<'ctx, 'b>>> {
    let loc = state.loc(span)?;
    let mut delta_opt: Option<Value<'ctx, 'b>> = None;
    let mut cur_opt = item_opt;
    for _ in 0..prog_ctx.prog.data_items.len() {
        let Some((cur, parent_id, parent_item)) = cur_opt.and_then(|c| {
            c.parent.and_then(|pid| {
                prog_ctx
                    .prog
                    .data_items
                    .get(pid.as_u32() as usize)
                    .map(|p| (c, pid, p))
            })
        }) else {
            break;
        };
        for sib in direct_primary_children(parent_id, &prog_ctx.prog.data_items) {
            if sib.id.as_u32() >= cur.id.as_u32() {
                break;
            }
            let sib_has_odo = matches!(
                sib.ty,
                HirType::Group {
                    odo_presence: DynamicOdoPresence::HasDynamicOdo,
                    ..
                }
            ) || sib
                .dimensions
                .get(parent_item.dimensions.len()..)
                .unwrap_or(&[])
                .iter()
                .any(|d| d.depending_on.is_some());
            if sib_has_odo {
                let active_bytes = compute_child_active_span(
                    sib,
                    parent_item.dimensions.len(),
                    check_mask,
                    span,
                    block,
                    prog_ctx,
                    state,
                )?;
                let max_bytes =
                    emit_i64(i64::from(sib.total_byte_size), block, prog_ctx, state, loc);
                let diff = append_val(
                    block,
                    arith::subi(active_bytes, max_bytes, loc),
                    prog_ctx.fallback_val,
                );
                delta_opt = Some(if let Some(prev) = delta_opt {
                    append_val(block, arith::addi(prev, diff, loc), prog_ctx.fallback_val)
                } else {
                    diff
                });
            }
        }
        cur_opt = Some(parent_item);
    }
    Ok(delta_opt)
}

/// Dispatches lowering for a single [`HirStmt`].
///
/// # Arguments
///
/// * `stmt` - Statement node to lower.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
pub(crate) fn lower_stmt<'ctx, 'b, 'e: 'b>(
    stmt: &HirStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    match stmt {
        HirStmt::Move { src, dst, span } => lower_move(src, dst, *span, block, prog_ctx, state),
        HirStmt::AssignArith {
            targets: t,
            expr: e,
            on_size_error: se,
            not_on_size_error: nse,
            span: sp,
        } => lower_assign_arith((t, e, *sp), (se, nse), block, prog_ctx, state),
        HirStmt::DivideRemainder(spec) => lower_divide_remainder(spec, block, prog_ctx, state),
        HirStmt::Initialize {
            targets: t,
            filler: f,
            value_mode: vm,
            replacements: reps,
            span: sp,
        } => lower_initialize((t, *f, *vm, *sp), reps, block, prog_ctx, state),
        HirStmt::Display {
            operands,
            device,
            advancing,
            span,
        } => lower_display(operands, *device, *advancing, *span, block, prog_ctx, state),
        HirStmt::If {
            cond,
            then_body,
            else_body,
            span,
        } => lower_if(cond, then_body, else_body, *span, block, prog_ctx, state),
        HirStmt::Sentence { body, span } => {
            lower_sentence_stmt(body, *span, block, prog_ctx, state)
        }
        HirStmt::PerformRange {
            start_proc: s,
            end_proc: e,
            span,
        } => lower_perform_range(s.as_u32(), e.as_u32(), *span, block, prog_ctx, state),
        HirStmt::Loop {
            kind: k,
            test_before: tb,
            until: u,
            body: b,
            cycle_step: cs,
            span: sp,
        } => lower_loop_stmt((*k, *tb, *sp), u, (b, cs), block, prog_ctx, state),
        HirStmt::FlowExit { kind, span } => {
            lower_flow_exit_stmt(*kind, *span, block, prog_ctx, state)
        }
        HirStmt::Alter { pairs, span } => lower_alter_stmt(pairs, *span, block, state),
        HirStmt::SetSwitch {
            switch_index: idx,
            state: sw,
            span,
        } => lower_set_switch_stmt(*idx, *sw, *span, block, prog_ctx, state),
        HirStmt::Call(spec) => crate::lower_m11::lower_m11_call_stmt(spec, block, prog_ctx, state),
        HirStmt::Cancel { targets, span } => {
            crate::lower_m11::lower_m11_cancel_stmt(targets, *span, block, prog_ctx, state)
        }
        HirStmt::Return { mode, code, span } => {
            lower_return(*mode, code.as_ref(), *span, block, prog_ctx, state)
        }
        HirStmt::Jump {
            targets: t,
            depending_on: dep,
            span: sp,
        } => lower_jump_stmt((t, dep.as_ref(), *sp), block, prog_ctx, state),
        HirStmt::SortTable(spec) => lower_sort_table_stmt(spec, block, prog_ctx, state),
        _ => lower_m10_or_m11_stmt(stmt, block, prog_ctx, state),
    }
}

/// Dispatches M10 string/environment verbs and M11 pointer/allocation verbs, delegating M13–M20 onward.
///
/// # Arguments
///
/// * `stmt` - HIR statement variant.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`CoblangError`] if lowering the statement fails.
fn lower_m10_or_m11_stmt<'ctx, 'b, 'e: 'b>(
    stmt: &HirStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    use crate::lower_m10::{
        lower_accept_stmt, lower_display_env_stmt, lower_inspect_stmt, lower_string_stmt,
        lower_unstring_stmt,
    };
    match stmt {
        HirStmt::DisplayEnv {
            operands: ops,
            target: tgt,
            span: sp,
        } => lower_display_env_stmt(ops, *tgt, *sp, block, prog_ctx, state),
        HirStmt::Accept(spec) => lower_accept_stmt(spec, block, prog_ctx, state),
        HirStmt::StringVerb(spec) => lower_string_stmt(spec, block, prog_ctx, state),
        HirStmt::Unstring(spec) => lower_unstring_stmt(spec, block, prog_ctx, state),
        HirStmt::Inspect(spec) => lower_inspect_stmt(spec, block, prog_ctx, state),
        HirStmt::SetPointer(spec) => {
            crate::lower_m11::lower_set_pointer_stmt(spec, block, prog_ctx, state)
        }
        HirStmt::Allocate(spec) => {
            crate::lower_m11::lower_allocate_stmt(spec, block, prog_ctx, state)
        }
        HirStmt::Free { targets, span } => {
            crate::lower_m11::lower_free_stmt(targets, *span, block, prog_ctx, state)
        }
        _ => lower_file_or_milestone_stmt(stmt, block, prog_ctx, state),
    }
}

/// Dispatches M13–M20 file I/O, sort/merge, report, screen, XML/JSON/OO, and exception statements.
///
/// # Arguments
///
/// * `stmt` - HIR statement variant.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_file_or_milestone_stmt<'ctx, 'b, 'e: 'b>(
    stmt: &HirStmt,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    match stmt {
        HirStmt::FileOpen(spec) => {
            let loc = state.loc(spec.span)?;
            crate::lower_m13::emit_arm_stmt_location(
                "OPEN", spec.span, block, prog_ctx, state, loc,
            )?;
            crate::lower_m14::lower_file_open(spec, block, prog_ctx, state)
        }
        HirStmt::FileClose(spec) => {
            let loc = state.loc(spec.span)?;
            crate::lower_m13::emit_arm_stmt_location(
                "CLOSE", spec.span, block, prog_ctx, state, loc,
            )?;
            crate::lower_m14::lower_file_close(spec, block, prog_ctx, state)
        }
        HirStmt::FileRead(spec) => {
            let loc = state.loc(spec.span)?;
            crate::lower_m13::emit_arm_stmt_location(
                "READ", spec.span, block, prog_ctx, state, loc,
            )?;
            crate::lower_m14::lower_file_read(spec, block, prog_ctx, state)
        }
        HirStmt::FileWrite(spec) => {
            let loc = state.loc(spec.span)?;
            crate::lower_m13::emit_arm_stmt_location(
                "WRITE", spec.span, block, prog_ctx, state, loc,
            )?;
            crate::lower_m14::lower_file_write(spec, block, prog_ctx, state)
        }
        HirStmt::FileRewrite(spec) => {
            let loc = state.loc(spec.span)?;
            crate::lower_m13::emit_arm_stmt_location(
                "REWRITE", spec.span, block, prog_ctx, state, loc,
            )?;
            crate::lower_m14::lower_file_rewrite(spec, block, prog_ctx, state)
        }
        HirStmt::FileDelete(spec) => {
            let loc = state.loc(spec.span)?;
            crate::lower_m13::emit_arm_stmt_location(
                "DELETE", spec.span, block, prog_ctx, state, loc,
            )?;
            crate::lower_m14::lower_file_delete(spec, block, prog_ctx, state)
        }
        HirStmt::FileDeletePhysical(spec) => {
            let loc = state.loc(spec.span)?;
            crate::lower_m13::emit_arm_stmt_location(
                "DELETE", spec.span, block, prog_ctx, state, loc,
            )?;
            crate::lower_m14::lower_file_delete_physical(spec, block, prog_ctx, state)
        }
        HirStmt::FileStart(spec) => {
            let loc = state.loc(spec.span)?;
            crate::lower_m13::emit_arm_stmt_location(
                "START", spec.span, block, prog_ctx, state, loc,
            )?;
            crate::lower_m14::lower_file_start(spec, block, prog_ctx, state)
        }
        HirStmt::FileUnlock(spec) => {
            let loc = state.loc(spec.span)?;
            crate::lower_m13::emit_arm_stmt_location(
                "UNLOCK", spec.span, block, prog_ctx, state, loc,
            )?;
            crate::lower_m14::lower_file_unlock(spec, block, prog_ctx, state)
        }
        HirStmt::SortFile(spec) => crate::lower_m16::lower_sort_file(spec, block, prog_ctx, state),
        HirStmt::MergeFile(spec) => {
            crate::lower_m16::lower_merge_file(spec, block, prog_ctx, state)
        }
        HirStmt::ReleaseRecord(spec) => {
            crate::lower_m16::lower_release_record(spec, block, prog_ctx, state)
        }
        HirStmt::ReturnRecord(spec) => {
            crate::lower_m16::lower_return_record(spec, block, prog_ctx, state)
        }
        HirStmt::M18(m18) => crate::lower_m18::lower_m18_stmt(m18, block, prog_ctx, state),
        HirStmt::M19(m19) => crate::lower_m19::lower_m19_stmt(m19, block, prog_ctx, state),
        HirStmt::M20(m20) => crate::lower_m20::lower_m20_stmt(m20, block, prog_ctx, state),
        HirStmt::SetTurn(spec) => {
            crate::lower_m13::lower_set_turn_stmt(spec, block, prog_ctx, state)
        }
        HirStmt::SetLastExceptionOff { span } => {
            crate::lower_m13::lower_set_last_exception_off(*span, block, prog_ctx, state)
        }
        HirStmt::CheckException(spec) => {
            let m13_unit = state.m13_units.get(&prog_ctx.prog.id).copied();
            crate::lower_m13::lower_check_exception_stmt(spec, m13_unit, block, prog_ctx, state)
        }
        HirStmt::DebugTrace(spec) => {
            let m13_unit = state.m13_units.get(&prog_ctx.prog.id).copied();
            crate::lower_m13::lower_debug_trace_stmt(spec, m13_unit, block, prog_ctx, state)
        }
        HirStmt::Raise(r) => crate::lower_m13::lower_raise_stmt(r, block, prog_ctx, state),
        HirStmt::Resume(r) => crate::lower_m13::lower_resume_stmt(r, block, prog_ctx, state),
        HirStmt::HighLevelOp { verb, span, .. } => lower_unimpl_high_op(*verb, *span, state),
        _ => Ok(()),
    }
}

/// Lowers `HirStmt::SortTable` into `cobol.rt_dispatch` calling `@coblang_rt_sort_table`.
///
/// # Arguments
///
/// * `spec` - Resolved [`HirSortTableSpec`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_sort_table_stmt<'ctx, 'b, 'e: 'b>(
    spec: &HirSortTableSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let pv = lower_place_view(&spec.table, block, prog_ctx, state)?;
    let hir_item = prog_ctx
        .prog
        .data_items
        .get(spec.table.item.as_u32() as usize);
    if let Some(own_dim) = hir_item.and_then(|it| it.dimensions.last()) {
        let _ = eval_odo_dim_checked(
            own_dim,
            spec.table.check_mask,
            spec.span,
            block,
            prog_ctx,
            state,
        )?;
    }
    let count_val = if let Some(odo_id) = spec.count_odo {
        let raw_odo = eval_scalar_item_as_i64(odo_id, spec.span, block, prog_ctx, state)?;
        clamp_count_i64(raw_odo, spec.max_count, block, prog_ctx, state, loc)
    } else {
        emit_i64(i64::from(spec.max_count), block, prog_ctx, state, loc)
    };
    let buf_cap = i64::from(spec.max_count).saturating_mul(i64::from(spec.stride.max(1)));
    let buf_cap_val = emit_i64(buf_cap, block, prog_ctx, state, loc);
    let plan_bytes = build_serialized_table_sort_plan(spec, prog_ctx.prog);
    let (plan_ptr, plan_len) = emit_const_slice(&plan_bytes, block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::SortTable,
        &[pv.ptr, buf_cap_val, count_val, plan_ptr, plan_len],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Returns [`CoblangError::UnimplementedFeature`] for an unlowered [`HirStmt::HighLevelOp`].
///
/// # Arguments
///
/// * `verb` - Interned verb symbol.
/// * `span` - Statement source span.
/// * `state` - Module lowering state.
fn lower_unimpl_high_op(
    verb: coblang_common::SymbolId,
    span: SourceSpan,
    state: &ModuleLowerState<'_, '_>,
) -> Result<()> {
    let vname = state.interner.resolve_canonical(verb).unwrap_or("HIGH-OP");
    Err(unimpl(
        classify_high_op_tag(vname),
        format!("verb `{vname}`"),
        state.source_map,
        span,
    ))
}

/// Emits a `CobolPerformRangeOp` invoking paragraph indices `start_proc..=end_proc`.
///
/// # Arguments
///
/// * `start_proc` - First procedure index in range.
/// * `end_proc` - Inclusive last procedure index in range.
/// * `span` - Source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_perform_range<'ctx, 'b, 'e: 'b>(
    start_proc: u32,
    end_proc: u32,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    if state
        .m13_units
        .get(&prog_ctx.prog.id)
        .is_some_and(|u| u.debugging_mode == coblang_hir::HirDebugCompileMode::WithDebuggingMode)
    {
        let one_32 = emit_i32(1, block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::M13DebugSetSuppress,
            &[one_32],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    let op = CobolPerformRangeOp {
        start_proc,
        end_proc,
    }
    .build(state.ctx, loc)?;
    let _ = block.append_operation(op);
    Ok(())
}

/// Lowers structured `PERFORM` loops (`TIMES`, `UNTIL`, `VARYING ... AFTER ...`) into `CobolLoopOp`.
///
/// # Arguments
///
/// * `modes` - `(HirLoopKind, HirLoopTest, SourceSpan)` loop tier, test placement, and span.
/// * `until` - Termination condition.
/// * `slices` - `(body, cycle_step)` statement slices.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_loop_stmt<'ctx, 'b, 'e: 'b>(
    modes: (HirLoopKind, HirLoopTest, SourceSpan),
    until: &HirCondition,
    slices: (&[HirStmt], &[HirStmt]),
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let (kind, test_before, span) = modes;
    let (body, cycle_step) = slices;
    let loc = state.loc(span)?;

    let cond_region = Region::new();
    let cond_blk = cond_region.append_block(Block::new(&[]));
    let cond_val = lower_condition(until, span, &cond_blk, prog_ctx, state)?;
    let _ = cond_blk.append_operation(
        CobolYieldOp {
            value: Some(cond_val),
        }
        .build(state.ctx, loc)?,
    );

    let body_region = Region::new();
    let body_blk = body_region.append_block(Block::new(&[]));
    for st in body {
        lower_stmt(st, &body_blk, prog_ctx, state)?;
    }
    let _ = body_blk.append_operation(CobolYieldOp { value: None }.build(state.ctx, loc)?);

    let step_region = Region::new();
    let step_blk = step_region.append_block(Block::new(&[]));
    for st in cycle_step {
        lower_stmt(st, &step_blk, prog_ctx, state)?;
    }
    let _ = step_blk.append_operation(CobolYieldOp { value: None }.build(state.ctx, loc)?);

    let loop_kind = match kind {
        HirLoopKind::UserPerform => "user_perform",
        HirLoopKind::InternalAfterTier => "after_tier",
    };
    let test_pos = match test_before {
        HirLoopTest::Before => "before",
        HirLoopTest::After => "after",
    };
    let op = CobolLoopOp {
        loop_kind,
        test_pos,
        cond_region,
        body_region,
        step_region,
    }
    .build(state.ctx, loc)?;
    let _ = block.append_operation(op);
    Ok(())
}

/// Lowers `HirStmt::Sentence` into a `CobolSentenceOp` anchoring `NEXT SENTENCE` jumps.
///
/// # Arguments
///
/// * `body` - Statements inside the period-terminated sentence.
/// * `span` - Sentence span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_sentence_stmt<'ctx, 'b, 'e: 'b>(
    body: &[HirStmt],
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let body_region = Region::new();
    let body_blk = body_region.append_block(Block::new(&[]));
    for st in body {
        lower_stmt(st, &body_blk, prog_ctx, state)?;
    }
    let _ = body_blk.append_operation(CobolYieldOp { value: None }.build(state.ctx, loc)?);
    let op = CobolSentenceOp { body_region }.build(state.ctx, loc)?;
    let _ = block.append_operation(op);
    Ok(())
}

/// Lowers `EXIT PARAGRAPH`, `EXIT SECTION`, `EXIT PERFORM [CYCLE]`, and `NEXT SENTENCE` into `CobolFlowExitOp`.
///
/// # Arguments
///
/// * `kind` - [`HirFlowExitKind`] variant.
/// * `span` - Statement span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_flow_exit_stmt<'ctx>(
    kind: HirFlowExitKind,
    span: SourceSpan,
    block: &Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, '_>,
    state: &ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let exit_kind = match kind {
        HirFlowExitKind::ExitParagraph => "paragraph",
        HirFlowExitKind::ExitSection => "section",
        HirFlowExitKind::ExitPerformBreak => "perform_break",
        HirFlowExitKind::ExitPerformCycle => "perform_cycle",
        HirFlowExitKind::NextSentence => "next_sentence",
    };
    let section_end = match prog_ctx.body_ctx {
        BodyExecContext::Paragraph { section_end, .. } => section_end,
        BodyExecContext::ProgramEntry | BodyExecContext::EntryHelper => 0,
    };
    let op = CobolFlowExitOp {
        exit_kind,
        section_end,
    }
    .build(state.ctx, loc)?;
    let _ = block.append_operation(op);
    Ok(())
}

/// Lowers `ALTER` `(from_proc, to_proc)` pairs into `CobolAlterOp` operations.
///
/// # Arguments
///
/// * `pairs` - `(from_proc, to_proc)` pairs.
/// * `span` - Statement span.
/// * `block` - Target insertion block.
/// * `state` - Module lowering state.
fn lower_alter_stmt<'ctx>(
    pairs: &[(coblang_common::ProcedureId, coblang_common::ProcedureId)],
    span: SourceSpan,
    block: &Block<'ctx>,
    state: &ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    for (p1, p2) in pairs {
        let op = CobolAlterOp {
            altered_para: p1.as_u32(),
            new_target: p2.as_u32(),
        }
        .build(state.ctx, loc)?;
        let _ = block.append_operation(op);
    }
    Ok(())
}

/// Lowers `SET switch-mnemonic TO {ON | OFF}` via `coblang_rt_set_switch`.
///
/// # Arguments
///
/// * `switch_index` - Zero-based switch index (`0..=36`).
/// * `sw_state` - Desired truth state.
/// * `span` - Statement span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_set_switch_stmt<'ctx, 'b, 'e: 'b>(
    switch_index: u8,
    sw_state: HirTruthValue,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let idx_v = emit_i32(i32::from(switch_index), block, prog_ctx, state, loc);
    let st_v = emit_i32(
        match sw_state {
            HirTruthValue::True => 1,
            HirTruthValue::False => 0,
        },
        block,
        prog_ctx,
        state,
        loc,
    );
    let _ = emit_rt_call(
        RtDispatchTarget::SetSwitch,
        &[idx_v, st_v],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Lowers static `GO TO`, `GO TO ... DEPENDING ON`, and `ALTER`able `GO TO` into `CobolGotoOp`.
///
/// # Arguments
///
/// * `jump` - `(targets, depending_on, span)` tuple.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_jump_stmt<'ctx, 'b, 'e: 'b>(
    jump: (&[coblang_common::ProcedureId], Option<&HirExpr>, SourceSpan),
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let (targets, depending_on, span) = jump;
    let loc = state.loc(span)?;
    let target_ids: Vec<i32> = targets
        .iter()
        .map(|t| i32::try_from(t.as_u32()).unwrap_or(0))
        .collect();
    let selector = if let Some(dep_expr) = depending_on {
        let sel_64 = eval_expr_as_i64(dep_expr, block, prog_ctx, state)?;
        let i32_ty = IntegerType::new(state.ctx.raw(), 32).into();
        Some(append_val(
            block,
            arith::trunci(sel_64, i32_ty, loc),
            prog_ctx.fallback_val,
        ))
    } else {
        None
    };
    let alter_sym = if depending_on.is_none()
        && let BodyExecContext::Paragraph { pid, .. } = prog_ctx.body_ctx
        && prog_ctx.alterable_procs.contains_key(&pid)
    {
        format!("__cobol_alter_{}_{pid}", prog_ctx.canon_sym)
    } else {
        String::new()
    };
    if state
        .m13_units
        .get(&prog_ctx.prog.id)
        .is_some_and(|u| u.debugging_mode == coblang_hir::HirDebugCompileMode::WithDebuggingMode)
    {
        let two_32 = emit_i32(2, block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::M13DebugSetSuppress,
            &[two_32],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    let op = CobolGotoOp {
        targets: &target_ids,
        alter_slot_sym: &alter_sym,
        selector,
    }
    .build(state.ctx, loc)?;
    let _ = block.append_operation(op);
    Ok(())
}

/// Maps an unlowered `HighLevelOp` verb name to its primary [`FeatureTag`].
///
/// # Arguments
///
/// * `verb` - Canonical uppercase COBOL verb spelling.
fn classify_high_op_tag(verb: &str) -> FeatureTag {
    match verb {
        "ACCEPT-SCREEN" => FeatureTag::FeatureScreen,
        "SEARCH" => FeatureTag::FeatureSearchSortTable,
        "SORT" | "MERGE" | "RELEASE" | "RETURN" => FeatureTag::FeatureSortMerge,
        "OPEN" | "CLOSE" | "READ" | "WRITE" | "REWRITE" | "DELETE" | "START" | "UNLOCK"
        | "COMMIT" | "ROLLBACK" => FeatureTag::FeatureFileSeq,
        "INITIATE" | "GENERATE" | "TERMINATE" | "SUPPRESS" => FeatureTag::FeatureReportWriter,
        "ALLOCATE" | "FREE" => FeatureTag::FeatureAllocateBased,
        "INVOKE" => FeatureTag::FeatureOo,
        "XML" | "JSON" => FeatureTag::FeatureXmlJson,
        "RAISE" | "RESUME" => FeatureTag::FeatureDeclarativesExceptions,
        "CANCEL" | "ENTRY" => FeatureTag::FeatureCallSubprog,
        _ => FeatureTag::FeaturePerformEvalIf,
    }
}

/// Outcome of attempting inline `memset` / `memcpy` fast-path emission for `HirStmt::Move`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum FastMoveOutcome {
    /// Fast-path MLIR ops (`cobol.init` or `cobol.move`) were emitted directly.
    Emitted,
    /// Move requires general runtime conversion (`@coblang_rt_move`).
    FallbackToRuntime,
}

/// Attempts inline `memset` / `memcpy` fast-path emission for `HirStmt::Move`.
///
/// # Arguments
///
/// * `src` - Source expression.
/// * `dst` - Destination place.
/// * `dst_view` - Resolved destination view.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn try_lower_fast_move<'ctx, 'b, 'e: 'b>(
    src: &HirExpr,
    dst: &HirPlace,
    dst_view: &LoweredPlaceView<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<FastMoveOutcome> {
    let src_root = match src {
        HirExpr::Place(sp) => prog_ctx
            .storage
            .items
            .get(sp.item.as_u32() as usize)
            .and_then(|o| o.as_ref().map(|r| r.root_id)),
        _ => None,
    };
    let Some(dst_rid) = dst_view.root_id else {
        return Ok(FastMoveOutcome::FallbackToRuntime);
    };
    let Some(fast) = try_classify_fast_move(src, dst, src_root, dst_rid, prog_ctx.prog.formatting)
    else {
        return Ok(FastMoveOutcome::FallbackToRuntime);
    };
    match fast {
        FastMovePlan::FigurativeFill { dst_len, fill_byte } => {
            let op = CobolInitOp {
                dst_view: dst_view.ptr,
                policy: InitPolicy::DefaultCategory,
                byte_len: dst_len,
                fill_byte,
            }
            .build(state.ctx, loc)?;
            let _ = block.append_operation(op);
            Ok(FastMoveOutcome::Emitted)
        }
        FastMovePlan::DirectCopyPad {
            source,
            src_len,
            dst_len,
            ..
        } => {
            let src_ptr = match source {
                FastAlphaSource::Literal(bytes) => {
                    emit_const_slice(bytes, block, prog_ctx, state, loc)?.0
                }
                FastAlphaSource::Place(sp) => lower_place_view(sp, block, prog_ctx, state)?.ptr,
            };
            let op = CobolMoveOp {
                src: src_ptr,
                dst_view: dst_view.ptr,
                encoding: MoveEncodingKind::RawBytesCopyPad,
                src_byte_len: src_len,
                dst_byte_len: dst_len,
            }
            .build(state.ctx, loc)?;
            let _ = block.append_operation(op);
            Ok(FastMoveOutcome::Emitted)
        }
    }
}

/// Lowers `HirStmt::Move` via `opt_fastpath::try_classify_fast_move` or `RtDispatchTarget::MoveDesc`.
///
/// # Arguments
///
/// * `src` - Source expression.
/// * `dst` - Destination place.
/// * `span` - Statement source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_move<'ctx, 'b, 'e: 'b>(
    src: &HirExpr,
    dst: &HirPlace,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    if let Some(int_plan) = try_classify_fast_int_move(dst, src, state.interner) {
        return emit_fast_int_arith(&int_plan, (&[], &[]), span, block, prog_ctx, state);
    }
    let loc = state.loc(span)?;
    let dst_view = lower_place_view(dst, block, prog_ctx, state)?;
    if try_lower_fast_move(src, dst, &dst_view, block, prog_ctx, state, loc)?
        == FastMoveOutcome::Emitted
    {
        return Ok(());
    }

    if matches!(
        src,
        HirExpr::Neg(_) | HirExpr::Binary { .. } | HirExpr::LengthOf(_)
    ) {
        let _ = emit_rt_call(RtDispatchTarget::DecBegin, &[], block, prog_ctx, state, loc)?;
        emit_decimal_expr(src, block, prog_ctx, state)?;
        let (ds_ptr, ds_len) = emit_spec_slice(&dst_view.spec, block, prog_ctx, state, loc)?;
        let z0 = emit_i32(0, block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::DecStore,
            &[dst_view.ptr, dst_view.len_i64, ds_ptr, ds_len, z0, z0, z0],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok(());
    }

    if try_lower_unsigned_int_literal_to_alpha_move(src, &dst_view, block, prog_ctx, state, loc)?
        == FastMoveOutcome::Emitted
    {
        return Ok(());
    }

    let (kind_v, payload_ptr, payload_len, spec_ptr, spec_len) =
        lower_move_source_args(src, block, prog_ctx, state, loc)?;
    let (dst_spec_ptr, dst_spec_len) =
        emit_spec_slice(&dst_view.spec, block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::MoveDesc,
        &[
            kind_v,
            payload_ptr,
            payload_len,
            spec_ptr,
            spec_len,
            dst_view.ptr,
            dst_view.len_i64,
            dst_spec_ptr,
            dst_spec_len,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Lowers `MOVE <unsigned-integer-literal> TO <alpha/edited/group>` preserving leading zeros per ISO COBOL.
///
/// # Arguments
///
/// * `src` - Source expression.
/// * `dst_view` - Resolved destination view.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn try_lower_unsigned_int_literal_to_alpha_move<'ctx, 'b, 'e: 'b>(
    src: &HirExpr,
    dst_view: &LoweredPlaceView<'ctx, 'b>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<FastMoveOutcome> {
    if let HirExpr::Const {
        value:
            HirConstValue::Decimal {
                text,
                scale,
                sign: coblang_hir::HirDecimalSign::UnsignedOrEmbedded,
            },
        ..
    } = src
        && scale.get() == 0
        && matches!(
            dst_view.spec.desc.category(),
            DataCategory::Alphabetic
                | DataCategory::Alphanumeric
                | DataCategory::AlphanumericEdited
                | DataCategory::Group
        )
    {
        let trimmed = state.interner.resolve_original(*text).unwrap_or("").trim();
        if !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()) {
            let (payload_ptr, payload_len) =
                emit_const_slice(trimmed.as_bytes(), block, prog_ctx, state, loc)?;
            let (empty_ptr, zero_i64) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
            let kind_v = emit_i32(
                RuntimeMoveSourceKind::AlphanumericLiteral.to_abi_code(),
                block,
                prog_ctx,
                state,
                loc,
            );
            let (dst_spec_ptr, dst_spec_len) =
                emit_spec_slice(&dst_view.spec, block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::MoveDesc,
                &[
                    kind_v,
                    payload_ptr,
                    payload_len,
                    empty_ptr,
                    zero_i64,
                    dst_view.ptr,
                    dst_view.len_i64,
                    dst_spec_ptr,
                    dst_spec_len,
                ],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            return Ok(FastMoveOutcome::Emitted);
        }
    }
    Ok(FastMoveOutcome::FallbackToRuntime)
}

/// Encodes any non-arithmetic `HirExpr` into `(kind_i32, ptr, len_i64, spec_ptr, spec_len_i64)` for `MoveDesc` / `InitPushRep`.
///
/// # Arguments
///
/// * `src` - Source expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
pub(crate) fn lower_move_source_args<'ctx, 'b, 'e: 'b>(
    src: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
)> {
    let (empty_ptr, zero_i64) = emit_const_slice(&[], block, prog_ctx, state, loc)?;
    match src {
        HirExpr::Place(pl) => {
            let v = lower_place_view(pl, block, prog_ctx, state)?;
            let (sp_ptr, sp_len) = emit_spec_slice(&v.spec, block, prog_ctx, state, loc)?;
            let k = emit_i32(
                RuntimeMoveSourceKind::Field.to_abi_code(),
                block,
                prog_ctx,
                state,
                loc,
            );
            Ok((k, v.ptr, v.len_i64, sp_ptr, sp_len))
        }
        HirExpr::Const { value, .. } => {
            lower_const_move_source_args(value, (empty_ptr, zero_i64), block, prog_ctx, state, loc)
        }
        HirExpr::CallFunction { name, args, span } => {
            lower_intrinsic_as_move_source(*name, args, *span, block, prog_ctx, state, loc)
        }
        HirExpr::M12Call(c) => {
            if let Some(res) =
                crate::lower_m20::try_lower_m12_call_move_source(c, block, prog_ctx, state, loc)?
            {
                return Ok(res);
            }
            crate::lower_m12::lower_m12_call_move_source(c, block, prog_ctx, state, loc)
        }
        HirExpr::M20(m) => crate::lower_m20::lower_m20_move_source(m, block, prog_ctx, state, loc),
        other => Err(unimpl(
            FeatureTag::FeatureMoveInit,
            format!("MOVE source `{other:?}`"),
            state.source_map,
            expr_span(other),
        )),
    }
}

/// Lowers supported string intrinsics (`TRIM`) when used as `MOVE` / `COMPARE` operands.
///
/// # Arguments
///
/// * `name` - Interned intrinsic function name.
/// * `args` - Call arguments.
/// * `span` - Call source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`CoblangError::UnimplementedFeature`] for unsupported intrinsics.
pub(crate) fn lower_intrinsic_as_move_source<'ctx, 'b, 'e: 'b>(
    name: coblang_common::SymbolId,
    args: &[HirExpr],
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
)> {
    let fname = state.interner.resolve_canonical(name).unwrap_or("");
    if fname == "TRIM"
        && let Some(first) = args.first()
    {
        let dir = parse_trim_dir(args.get(1), state.interner);
        let (_, s_ptr, s_len, _, _) = lower_move_source_args(first, block, prog_ctx, state, loc)?;
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
        return Ok((k, t_ptr, t_len, empty_ptr, zero_i64));
    }
    Err(unimpl(
        FeatureTag::FeatureIntrinsics,
        format!("FUNCTION {fname}"),
        state.source_map,
        span,
    ))
}

/// Extracts [`TrimDirection`] from an optional second `FUNCTION TRIM` argument.
///
/// # Arguments
///
/// * `arg` - Optional second argument (`LEADING` / `TRAILING`).
/// * `interner` - Symbol interner.
fn parse_trim_dir(arg: Option<&HirExpr>, interner: &SymbolInterner) -> TrimDirection {
    let Some(HirExpr::Const {
        value: HirConstValue::Bytes(b),
        ..
    }) = arg
    else {
        let _ = interner;
        return TrimDirection::Both;
    };
    if b.eq_ignore_ascii_case(b"LEADING") {
        TrimDirection::Leading
    } else if b.eq_ignore_ascii_case(b"TRAILING") {
        TrimDirection::Trailing
    } else {
        TrimDirection::Both
    }
}

/// Lowers `HirStmt::AssignArith` via `opt_fastpath` (inline `i128`) or `DecimalEvalStack` (38/76-digit exact).
///
/// # Arguments
///
/// * `arith_op` - `(targets, expr, span)` tuple.
/// * `se_handlers` - `(on_size_error, not_on_size_error)` statement slices.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_assign_arith<'ctx, 'b, 'e: 'b>(
    arith_op: (&[(HirPlace, HirRounding)], &HirExpr, SourceSpan),
    se_handlers: (&[HirStmt], &[HirStmt]),
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let (targets, expr, span) = arith_op;
    if !prog_ctx.has_ec_tracking
        && let Some(fast_plan) = try_classify_fast_int_arith(targets, expr, state.interner)
    {
        return emit_fast_int_arith(&fast_plan, se_handlers, span, block, prog_ctx, state);
    }

    let (on_se, not_se) = se_handlers;
    let has_se = !on_se.is_empty() || !not_se.is_empty();
    let loc = state.loc(span)?;
    if has_se || prog_ctx.has_ec_tracking {
        crate::lower_m13::emit_arm_stmt_location("COMPUTE", span, block, prog_ctx, state, loc)?;
    }

    let _ = emit_rt_call(RtDispatchTarget::DecBegin, &[], block, prog_ctx, state, loc)?;
    emit_decimal_expr(expr, block, prog_ctx, state)?;

    let i1_ty = IntegerType::new(state.ctx.raw(), 1).into();
    let mut any_se = append_val(
        block,
        arith::constant(state.ctx.raw(), IntegerAttribute::new(i1_ty, 0).into(), loc),
        prog_ctx.fallback_val,
    );
    let zero_i32 = emit_i32(0, block, prog_ctx, state, loc);
    let flag_ov = emit_i32(i32::from(has_se), block, prog_ctx, state, loc);

    for (dst_pl, rnd) in targets {
        let dv = lower_place_view(dst_pl, block, prog_ctx, state)?;
        let (ds_ptr, ds_len) = emit_spec_slice(&dv.spec, block, prog_ctx, state, loc)?;
        let rnd_v = emit_i32(
            rounding_to_abi_code(map_hir_rounding(*rnd)),
            block,
            prog_ctx,
            state,
            loc,
        );
        let preserve_nz = is_inplace_unrounded_add_sub(dst_pl, *rnd, expr);
        let nz_v = emit_i32(
            if preserve_nz {
                NegZeroPolicy::PreserveExistingNegZero.to_abi_code()
            } else {
                NegZeroPolicy::NormalizePositiveZero.to_abi_code()
            },
            block,
            prog_ctx,
            state,
            loc,
        );
        let st = emit_rt_call(
            RtDispatchTarget::DecStore,
            &[dv.ptr, dv.len_i64, ds_ptr, ds_len, rnd_v, flag_ov, nz_v],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        let is_err = append_val(
            block,
            arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, st, zero_i32, loc),
            prog_ctx.fallback_val,
        );
        any_se = append_val(
            block,
            arith::ori(any_se, is_err, loc),
            prog_ctx.fallback_val,
        );
    }

    if has_se {
        emit_size_error_branch(any_se, on_se, not_se, loc, block, prog_ctx, state)?;
    }
    Ok(())
}

/// Emits inline `i128` fast-path arithmetic + optional `ON SIZE ERROR` guard for [`FastIntArithPlan`].
///
/// # Arguments
///
/// * `plan` - Classified inline `i128` arithmetic plan.
/// * `se_handlers` - `(on_size_error, not_on_size_error)` statement slices.
/// * `span` - Statement source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn emit_fast_int_arith<'ctx, 'b, 'e: 'b>(
    plan: &FastIntArithPlan<'_>,
    se_handlers: (&[HirStmt], &[HirStmt]),
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let (on_se, not_se) = se_handlers;
    let has_se = !on_se.is_empty() || !not_se.is_empty();
    let loc = state.loc(span)?;
    let (raw_val, div_zero) = eval_fast_i128(&plan.expr, block, prog_ctx, state, loc)?;
    let i128_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 128).into();
    let zero_128 = emit_i128_const(0, i128_ty, block, prog_ctx, state, loc);

    // Unsigned COBOL targets take `|raw_val|` before storing or checking capacity!
    let signed_adjusted = if plan.dst_sign == Signedness::Unsigned {
        let is_neg = append_val(
            block,
            arith::cmpi(state.ctx.raw(), CmpiPredicate::Slt, raw_val, zero_128, loc),
            prog_ctx.fallback_val,
        );
        let neg_val = append_val(
            block,
            arith::subi(zero_128, raw_val, loc),
            prog_ctx.fallback_val,
        );
        append_val(
            block,
            arith::select(is_neg, neg_val, raw_val, loc),
            prog_ctx.fallback_val,
        )
    } else {
        raw_val
    };

    let (stored_128, bound_err) = check_fast_int_bounds(
        signed_adjusted,
        plan.bound_rule,
        i128_ty,
        block,
        prog_ctx,
        state,
        loc,
    );

    let dst_view = lower_place_view(plan.dst_place, block, prog_ctx, state)?;
    let dst_bits = u32::from(plan.dst_byte_width) * 8;
    let dst_int_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), dst_bits).into();

    if has_se {
        let se_cond = append_val(
            block,
            arith::ori(div_zero, bound_err, loc),
            prog_ctx.fallback_val,
        );
        let then_reg = Region::new();
        let then_blk = then_reg.append_block(Block::new(&[]));
        for st in on_se {
            lower_stmt(st, &then_blk, prog_ctx, state)?;
        }
        let _ = then_blk.append_operation(scf::r#yield(&[], loc));

        let else_reg = Region::new();
        let else_blk = else_reg.append_block(Block::new(&[]));
        let trunc_v = append_val(
            &else_blk,
            arith::trunci(stored_128, dst_int_ty, loc),
            prog_ctx.fallback_val,
        );
        let _ = else_blk.append_operation(llvm::store(
            state.ctx.raw(),
            trunc_v,
            dst_view.ptr,
            loc,
            llvm::LoadStoreOptions::new(),
        ));
        for st in not_se {
            lower_stmt(st, &else_blk, prog_ctx, state)?;
        }
        let _ = else_blk.append_operation(scf::r#yield(&[], loc));
        let _ = block.append_operation(scf::r#if(se_cond, &[], then_reg, else_reg, loc));
    } else {
        let then_reg = Region::new();
        let then_blk = then_reg.append_block(Block::new(&[]));
        let _ = then_blk.append_operation(scf::r#yield(&[], loc));

        let else_reg = Region::new();
        let else_blk = else_reg.append_block(Block::new(&[]));
        let trunc_v = append_val(
            &else_blk,
            arith::trunci(stored_128, dst_int_ty, loc),
            prog_ctx.fallback_val,
        );
        let _ = else_blk.append_operation(llvm::store(
            state.ctx.raw(),
            trunc_v,
            dst_view.ptr,
            loc,
            llvm::LoadStoreOptions::new(),
        ));
        let _ = else_blk.append_operation(scf::r#yield(&[], loc));
        let _ = block.append_operation(scf::r#if(div_zero, &[], then_reg, else_reg, loc));
    }
    Ok(())
}

/// Computes `(wrapped_value_i128, overflow_i1)` for [`FastIntBoundRule`].
///
/// # Arguments
///
/// * `val_128` - Unbounded `i128` arithmetic result.
/// * `rule` - Target overflow/truncation bound rule.
/// * `i128_ty` - MLIR `i128` integer type.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn check_fast_int_bounds<'ctx, 'b, 'e: 'b>(
    val_128: Value<'ctx, 'b>,
    rule: FastIntBoundRule,
    i128_ty: Type<'ctx>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> (Value<'ctx, 'b>, Value<'ctx, 'b>) {
    match rule {
        FastIntBoundRule::FullBinary { min_val, max_val } => {
            let min_c = emit_i128_const(min_val, i128_ty, block, prog_ctx, state, loc);
            let max_c = emit_i128_const(max_val, i128_ty, block, prog_ctx, state, loc);
            let lt = append_val(
                block,
                arith::cmpi(state.ctx.raw(), CmpiPredicate::Slt, val_128, min_c, loc),
                prog_ctx.fallback_val,
            );
            let gt = append_val(
                block,
                arith::cmpi(state.ctx.raw(), CmpiPredicate::Sgt, val_128, max_c, loc),
                prog_ctx.fallback_val,
            );
            (
                val_128,
                append_val(block, arith::ori(lt, gt, loc), prog_ctx.fallback_val),
            )
        }
        FastIntBoundRule::PicModulo { modulus } => {
            let mod_c = emit_i128_const(modulus, i128_ty, block, prog_ctx, state, loc);
            let neg_mod_c = emit_i128_const(-modulus, i128_ty, block, prog_ctx, state, loc);
            let ge_hi = append_val(
                block,
                arith::cmpi(state.ctx.raw(), CmpiPredicate::Sge, val_128, mod_c, loc),
                prog_ctx.fallback_val,
            );
            let le_lo = append_val(
                block,
                arith::cmpi(state.ctx.raw(), CmpiPredicate::Sle, val_128, neg_mod_c, loc),
                prog_ctx.fallback_val,
            );
            let ov = append_val(block, arith::ori(ge_hi, le_lo, loc), prog_ctx.fallback_val);
            let wrapped = append_val(
                block,
                arith::remsi(val_128, mod_c, loc),
                prog_ctx.fallback_val,
            );
            (wrapped, ov)
        }
    }
}

/// Evaluates a [`FastIntExpr`] into `(value_i128, div_by_zero_i1)`.
///
/// # Arguments
///
/// * `expr` - Classified inline `i128` expression node.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn eval_fast_i128<'ctx, 'b, 'e: 'b>(
    expr: &FastIntExpr<'_>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let i128_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 128).into();
    let i1_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 1).into();
    let false_i1 = append_val(
        block,
        arith::constant(state.ctx.raw(), IntegerAttribute::new(i1_ty, 0).into(), loc),
        prog_ctx.fallback_val,
    );
    match expr {
        FastIntExpr::Const(n) => Ok((
            emit_i128_const(*n, i128_ty, block, prog_ctx, state, loc),
            false_i1,
        )),
        FastIntExpr::LoadBinary {
            place,
            byte_width,
            sign,
        } => {
            let pv = lower_place_view(place, block, prog_ctx, state)?;
            let bits = u32::from(*byte_width) * 8;
            let mem_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), bits).into();
            let loaded = append_val(
                block,
                llvm::load(
                    state.ctx.raw(),
                    pv.ptr,
                    mem_ty,
                    loc,
                    llvm::LoadStoreOptions::new(),
                ),
                prog_ctx.fallback_val,
            );
            let ext = match sign {
                Signedness::Signed => arith::extsi(loaded, i128_ty, loc),
                Signedness::Unsigned => arith::extui(loaded, i128_ty, loc),
            };
            Ok((append_val(block, ext, prog_ctx.fallback_val), false_i1))
        }
        FastIntExpr::Neg(inner) => {
            let (v, dz) = eval_fast_i128(inner, block, prog_ctx, state, loc)?;
            let z = emit_i128_const(0, i128_ty, block, prog_ctx, state, loc);
            Ok((
                append_val(block, arith::subi(z, v, loc), prog_ctx.fallback_val),
                dz,
            ))
        }
        FastIntExpr::Binary { op, lhs, rhs } => {
            let (lv, l_dz) = eval_fast_i128(lhs, block, prog_ctx, state, loc)?;
            let (rv, r_dz) = eval_fast_i128(rhs, block, prog_ctx, state, loc)?;
            let dz = append_val(block, arith::ori(l_dz, r_dz, loc), prog_ctx.fallback_val);
            match op {
                FastIntBinOp::Add => Ok((
                    append_val(block, arith::addi(lv, rv, loc), prog_ctx.fallback_val),
                    dz,
                )),
                FastIntBinOp::Sub => Ok((
                    append_val(block, arith::subi(lv, rv, loc), prog_ctx.fallback_val),
                    dz,
                )),
                FastIntBinOp::Mul => Ok((
                    append_val(block, arith::muli(lv, rv, loc), prog_ctx.fallback_val),
                    dz,
                )),
                FastIntBinOp::Div => {
                    let z = emit_i128_const(0, i128_ty, block, prog_ctx, state, loc);
                    let one = emit_i128_const(1, i128_ty, block, prog_ctx, state, loc);
                    let is_zero = append_val(
                        block,
                        arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, rv, z, loc),
                        prog_ctx.fallback_val,
                    );
                    let safe_den = append_val(
                        block,
                        arith::select(is_zero, one, rv, loc),
                        prog_ctx.fallback_val,
                    );
                    let q = append_val(
                        block,
                        arith::divsi(lv, safe_den, loc),
                        prog_ctx.fallback_val,
                    );
                    let all_dz =
                        append_val(block, arith::ori(dz, is_zero, loc), prog_ctx.fallback_val);
                    Ok((q, all_dz))
                }
            }
        }
    }
}

/// Emits an exact `i128` constant in `block` from two `i64` halves (`lo`, `hi`).
///
/// # Arguments
///
/// * `val` - Constant `i128` value.
/// * `i128_ty` - MLIR `i128` integer type.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_i128_const<'ctx, 'b, 'e: 'b>(
    val: i128,
    i128_ty: Type<'ctx>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Value<'ctx, 'b> {
    if let Ok(fits_i64) = i64::try_from(val) {
        if fits_i64 >= 0 {
            let op = arith::constant(
                state.ctx.raw(),
                IntegerAttribute::new(i128_ty, fits_i64).into(),
                loc,
            );
            return append_val(block, op, prog_ctx.fallback_val);
        }
        let i64_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 64).into();
        let c64 = append_val(
            block,
            arith::constant(
                state.ctx.raw(),
                IntegerAttribute::new(i64_ty, fits_i64).into(),
                loc,
            ),
            prog_ctx.fallback_val,
        );
        return append_val(
            block,
            arith::extsi(c64, i128_ty, loc),
            prog_ctx.fallback_val,
        );
    }
    let raw_bytes = val.to_le_bytes();
    let low_bits = raw_bytes
        .get(..8)
        .and_then(|s| <[u8; 8]>::try_from(s).ok())
        .map_or(0, i64::from_le_bytes);
    let high_bits = raw_bytes
        .get(8..)
        .and_then(|s| <[u8; 8]>::try_from(s).ok())
        .map_or(0, i64::from_le_bytes);
    let i64_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 64).into();
    let low_v64 = append_val(
        block,
        arith::constant(
            state.ctx.raw(),
            IntegerAttribute::new(i64_ty, low_bits).into(),
            loc,
        ),
        prog_ctx.fallback_val,
    );
    let high_v64 = append_val(
        block,
        arith::constant(
            state.ctx.raw(),
            IntegerAttribute::new(i64_ty, high_bits).into(),
            loc,
        ),
        prog_ctx.fallback_val,
    );
    let low_ext = append_val(
        block,
        arith::extui(low_v64, i128_ty, loc),
        prog_ctx.fallback_val,
    );
    let high_ext = append_val(
        block,
        arith::extui(high_v64, i128_ty, loc),
        prog_ctx.fallback_val,
    );
    let c64 = append_val(
        block,
        arith::constant(
            state.ctx.raw(),
            IntegerAttribute::new(i128_ty, 64).into(),
            loc,
        ),
        prog_ctx.fallback_val,
    );
    let high_shifted = append_val(
        block,
        arith::shli(high_ext, c64, loc),
        prog_ctx.fallback_val,
    );
    append_val(
        block,
        arith::ori(high_shifted, low_ext, loc),
        prog_ctx.fallback_val,
    )
}

/// Returns `true` when `expr` is an unrounded in-place `ADD rhs TO dst` or `SUBTRACT rhs FROM dst`.
///
/// # Arguments
///
/// * `dst` - Target receiving place.
/// * `rnd` - Target rounding mode.
/// * `expr` - Assigned binary expression.
fn is_inplace_unrounded_add_sub(dst: &HirPlace, rnd: HirRounding, expr: &HirExpr) -> bool {
    if rnd != HirRounding::Truncate || !matches!(dst.effective_ty, HirType::ZonedDecimal { .. }) {
        return false;
    }
    if let HirExpr::Binary { op, lhs, .. } = expr
        && matches!(op, HirBinOp::Add | HirBinOp::Sub)
        && let HirExpr::Place(lp) = lhs.as_ref()
    {
        return (lp.item, &lp.subscripts, &lp.refmod) == (dst.item, &dst.subscripts, &dst.refmod);
    }
    false
}

/// Maps [`HirRounding`] to `coblang_decimal::RoundingMode`.
///
/// # Arguments
///
/// * `r` - HIR rounding mode.
const fn map_hir_rounding(r: HirRounding) -> coblang_decimal::RoundingMode {
    match r {
        HirRounding::Truncate => coblang_decimal::RoundingMode::Truncation,
        HirRounding::NearestAwayFromZero => coblang_decimal::RoundingMode::NearestAwayFromZero,
        HirRounding::NearestEven => coblang_decimal::RoundingMode::NearestEven,
        HirRounding::NearestTowardZero => coblang_decimal::RoundingMode::NearestTowardZero,
        HirRounding::Prohibited => coblang_decimal::RoundingMode::Prohibited,
        HirRounding::TowardGreater => coblang_decimal::RoundingMode::TowardGreater,
        HirRounding::TowardLesser => coblang_decimal::RoundingMode::TowardLesser,
        HirRounding::AwayFromZero => coblang_decimal::RoundingMode::AwayFromZero,
    }
}

/// Emits an `scf.if` branch for `ON SIZE ERROR` / `NOT ON SIZE ERROR` (or `OVERFLOW` / `EXCEPTION`).
///
/// # Arguments
///
/// * `cond_i1` - Boolean `i1` size-error condition.
/// * `on_se` - Statements executed when size error occurred.
/// * `not_se` - Statements executed when no size error occurred.
/// * `loc` - Source location.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`CoblangError`] if lowering any nested handler statement fails.
pub(crate) fn emit_size_error_branch<'ctx, 'b, 'e: 'b>(
    cond_i1: Value<'ctx, 'b>,
    on_se: &[HirStmt],
    not_se: &[HirStmt],
    loc: Location<'ctx>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let then_reg = Region::new();
    let then_blk = then_reg.append_block(Block::new(&[]));
    for st in on_se {
        lower_stmt(st, &then_blk, prog_ctx, state)?;
    }
    let _ = then_blk.append_operation(scf::r#yield(&[], loc));

    let else_reg = Region::new();
    let else_blk = else_reg.append_block(Block::new(&[]));
    for st in not_se {
        lower_stmt(st, &else_blk, prog_ctx, state)?;
    }
    let _ = else_blk.append_operation(scf::r#yield(&[], loc));

    let _ = block.append_operation(scf::r#if(cond_i1, &[], then_reg, else_reg, loc));
    Ok(())
}

/// Pushes `LENGTH OF place` onto the runtime `DecimalEvalStack`.
///
/// # Arguments
///
/// * `pl` - Target place whose byte length is queried.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_length_of_decimal<'ctx, 'b, 'e: 'b>(
    pl: &HirPlace,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let pv = lower_place_view(pl, block, prog_ctx, state)?;
    let scratch_ptr =
        emit_storage_ptr(&prog_ctx.idx_scratch_sym, 0, 8, block, prog_ctx, state, loc)?;
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        pv.len_i64,
        scratch_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let spec = make_runtime_field_spec(
        HirType::BinaryInt {
            byte_width: 8,
            digits: DigitCount::try_new(18).unwrap_or(DigitCount::ONE),
            scale: ScaleFactor::ZERO,
            sign: Signedness::Signed,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
        },
        prog_ctx.prog.formatting,
    );
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

/// Pushes a compile-time [`HirConstValue`] onto the runtime `DecimalEvalStack`.
///
/// # Arguments
///
/// * `value` - Constant literal value.
/// * `span` - Literal source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_decimal_const<'ctx, 'b, 'e: 'b>(
    value: &HirConstValue,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let ascii = crate::lower_m17::const_to_decimal_ascii_m17(
        value,
        prog_ctx.prog.formatting,
        span,
        state.source_map,
        state.interner,
    )?;
    let (p, l) = emit_const_slice(ascii.as_bytes(), block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::DecPushAscii,
        &[p, l],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Pushes an arbitrary numeric [`HirExpr`] onto the runtime `DecimalEvalStack`.
///
/// # Arguments
///
/// * `expr` - Numeric expression node.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
pub(crate) fn emit_decimal_expr<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(expr_span(expr))?;
    match expr {
        HirExpr::Place(pl) => {
            let pv = lower_place_view(pl, block, prog_ctx, state)?;
            let (sp_ptr, sp_len) = emit_spec_slice(&pv.spec, block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::DecPushField,
                &[pv.ptr, pv.len_i64, sp_ptr, sp_len],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            Ok(())
        }
        HirExpr::Const { value, span } => {
            emit_decimal_const(value, *span, block, prog_ctx, state, loc)
        }
        HirExpr::Neg(operand) => {
            emit_decimal_expr(operand, block, prog_ctx, state)?;
            let _ = emit_rt_call(RtDispatchTarget::DecNeg, &[], block, prog_ctx, state, loc)?;
            Ok(())
        }
        HirExpr::Binary { op, lhs, rhs, span } => {
            let dop = match op {
                HirBinOp::Add => DecimalArithOp::Add,
                HirBinOp::Sub => DecimalArithOp::Sub,
                HirBinOp::Mul => DecimalArithOp::Mul,
                HirBinOp::Div => DecimalArithOp::Div,
                HirBinOp::Pow => DecimalArithOp::Pow,
                HirBinOp::Concat => {
                    return Err(unimpl(
                        FeatureTag::FeatureStringInspect,
                        "string concatenation `&`",
                        state.source_map,
                        *span,
                    ));
                }
            };
            emit_decimal_expr(lhs, block, prog_ctx, state)?;
            emit_decimal_expr(rhs, block, prog_ctx, state)?;
            let op_v = emit_i32(dop.to_abi_code(), block, prog_ctx, state, loc);
            let _ = emit_rt_call(
                RtDispatchTarget::DecBinop,
                &[op_v],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            Ok(())
        }
        HirExpr::CallFunction { name, args, span } => {
            let fname = state
                .interner
                .resolve_canonical(*name)
                .unwrap_or("INTRINSIC");
            if crate::lower_m20::try_emit_m20_intrinsic_decimal(
                fname, args, block, prog_ctx, state, loc,
            )? {
                return Ok(());
            }
            Err(unimpl(
                FeatureTag::FeatureIntrinsics,
                format!("FUNCTION {fname}"),
                state.source_map,
                *span,
            ))
        }
        HirExpr::M12Call(c) => {
            if crate::lower_m20::try_emit_m12_call_decimal(c, block, prog_ctx, state, loc)? {
                return Ok(());
            }
            crate::lower_m12::emit_m12_call_decimal(c, block, prog_ctx, state, loc)
        }
        HirExpr::M20(m) => crate::lower_m20::emit_m20_decimal_expr(m, block, prog_ctx, state, loc),
        HirExpr::LengthOf(pl) => emit_length_of_decimal(pl, block, prog_ctx, state, loc),
        HirExpr::AddressOf(pl) => Err(unimpl(
            FeatureTag::TypePointer,
            "ADDRESS OF expression",
            state.source_map,
            pl.span,
        )),
    }
}

/// Evaluates `expr` as a signed `i64` SSA value (for subscripts, `RefMod` offsets/lengths, and exit codes).
///
/// # Arguments
///
/// * `expr` - Integer-valued expression node.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
///
/// # Errors
///
/// Returns [`CoblangError`] if operand lowering fails.
pub(crate) fn eval_expr_as_i64<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let loc = state.loc(expr_span(expr))?;
    if let HirExpr::LengthOf(pl) = expr {
        return Ok(lower_place_view(pl, block, prog_ctx, state)?.len_i64);
    }
    let i64_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 64).into();
    if let HirExpr::Const {
        value: HirConstValue::Decimal { text, scale, sign },
        ..
    } = expr
        && scale.get() == 0
    {
        let trimmed = state.interner.resolve_original(*text).unwrap_or("").trim();
        let is_neg = *sign == coblang_hir::HirDecimalSign::Negative || trimmed.starts_with('-');
        if let Ok(mag) = trimmed.trim_start_matches(['+', '-']).parse::<i64>() {
            let v = if is_neg { -mag } else { mag };
            return Ok(emit_i64(v, block, prog_ctx, state, loc));
        }
    }
    if let HirExpr::Place(pl) = expr
        && pl.refmod.is_none()
        && pl.effective_ty == HirType::Index
    {
        let pv = lower_place_view(pl, block, prog_ctx, state)?;
        return Ok(append_val(
            block,
            llvm::load(
                state.ctx.raw(),
                pv.ptr,
                i64_ty,
                loc,
                llvm::LoadStoreOptions::new(),
            ),
            prog_ctx.fallback_val,
        ));
    }
    if let HirExpr::Place(pl) = expr
        && pl.refmod.is_none()
        && let HirType::BinaryInt {
            byte_width: bw @ (1 | 2 | 4 | 8),
            scale,
            sign,
            endian: BinaryEndianness::Native,
            ..
        } = &pl.effective_ty
        && scale.get() == 0
    {
        let pv = lower_place_view(pl, block, prog_ctx, state)?;
        let mem_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), u32::from(*bw) * 8).into();
        let loaded = append_val(
            block,
            llvm::load(
                state.ctx.raw(),
                pv.ptr,
                mem_ty,
                loc,
                llvm::LoadStoreOptions::new(),
            ),
            prog_ctx.fallback_val,
        );
        if *bw == 8 {
            return Ok(loaded);
        }
        let ext = match sign {
            Signedness::Signed => arith::extsi(loaded, i64_ty, loc),
            Signedness::Unsigned => arith::extui(loaded, i64_ty, loc),
        };
        return Ok(append_val(block, ext, prog_ctx.fallback_val));
    }

    eval_expr_as_i64_slow(expr, block, prog_ctx, state, loc)
}

/// Evaluates non-fast-path `HirExpr` operands into an `i64` stack slot via `MoveDesc` or `DecimalEvalStack`.
///
/// # Arguments
///
/// * `expr` - HIR expression to evaluate.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn eval_expr_as_i64_slow<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let i64_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 64).into();
    let scratch_ty = HirType::BinaryInt {
        byte_width: 8,
        digits: DigitCount::try_new(18).unwrap_or(DigitCount::ONE),
        scale: ScaleFactor::ZERO,
        sign: Signedness::Signed,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::FullBinaryRange,
    };
    let scratch_spec = make_runtime_field_spec(scratch_ty, prog_ctx.prog.formatting);
    let (sp_ptr, sp_len) = emit_spec_slice(&scratch_spec, block, prog_ctx, state, loc)?;
    let one_i64 = emit_i64(1, block, prog_ctx, state, loc);
    let ptr_ty = llvm_ptr_type(state.ctx);
    let scratch_ptr = append_val(
        block,
        llvm::alloca(
            state.ctx.raw(),
            one_i64,
            ptr_ty,
            loc,
            llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i64_ty))),
        ),
        one_i64,
    );
    let eight = emit_i64(8, block, prog_ctx, state, loc);
    let z0 = emit_i32(0, block, prog_ctx, state, loc);

    if let HirExpr::Place(pl) = expr {
        let pv = lower_place_view(pl, block, prog_ctx, state)?;
        let (src_spec_ptr, src_spec_len) = emit_spec_slice(&pv.spec, block, prog_ctx, state, loc)?;
        let kind_v = emit_i32(
            RuntimeMoveSourceKind::Field.to_abi_code(),
            block,
            prog_ctx,
            state,
            loc,
        );
        let _ = emit_rt_call(
            RtDispatchTarget::MoveDesc,
            &[
                kind_v,
                pv.ptr,
                pv.len_i64,
                src_spec_ptr,
                src_spec_len,
                scratch_ptr,
                eight,
                sp_ptr,
                sp_len,
            ],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok(append_val(
            block,
            llvm::load(
                state.ctx.raw(),
                scratch_ptr,
                i64_ty,
                loc,
                llvm::LoadStoreOptions::new(),
            ),
            prog_ctx.fallback_val,
        ));
    }

    // General expression fallback: evaluate on `DecimalEvalStack` and store into `scratch_ptr` (`i64`).
    let _ = emit_rt_call(RtDispatchTarget::DecBegin, &[], block, prog_ctx, state, loc)?;
    emit_decimal_expr(expr, block, prog_ctx, state)?;
    let _ = emit_rt_call(
        RtDispatchTarget::DecStore,
        &[scratch_ptr, eight, sp_ptr, sp_len, z0, z0, z0],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(append_val(
        block,
        llvm::load(
            state.ctx.raw(),
            scratch_ptr,
            i64_ty,
            loc,
            llvm::LoadStoreOptions::new(),
        ),
        prog_ctx.fallback_val,
    ))
}

/// Lowers `HirStmt::DivideRemainder`.
///
/// # Arguments
///
/// * `spec` - `DIVIDE ... REMAINDER` specification.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_divide_remainder<'ctx, 'b, 'e: 'b>(
    spec: &HirDivideRemainderSpec,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(spec.span)?;
    let has_se = !spec.on_size_error.is_empty() || !spec.not_on_size_error.is_empty();
    let _ = emit_rt_call(RtDispatchTarget::DecBegin, &[], block, prog_ctx, state, loc)?;
    emit_decimal_expr(&spec.dividend, block, prog_ctx, state)?;
    emit_decimal_expr(&spec.divisor, block, prog_ctx, state)?;

    let qv = lower_place_view(&spec.quotient.0, block, prog_ctx, state)?;
    let rv = lower_place_view(&spec.remainder, block, prog_ctx, state)?;
    let (qs_ptr, qs_len) = emit_spec_slice(&qv.spec, block, prog_ctx, state, loc)?;
    let (rs_ptr, rs_len) = emit_spec_slice(&rv.spec, block, prog_ctx, state, loc)?;
    let q_rnd = emit_i32(
        rounding_to_abi_code(map_hir_rounding(spec.quotient.1)),
        block,
        prog_ctx,
        state,
        loc,
    );
    let flag_ov = emit_i32(i32::from(has_se), block, prog_ctx, state, loc);

    let st = emit_rt_call(
        RtDispatchTarget::DecDivRem,
        &[
            qv.ptr, qv.len_i64, qs_ptr, qs_len, q_rnd, rv.ptr, rv.len_i64, rs_ptr, rs_len, flag_ov,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    if has_se {
        let z0 = emit_i32(0, block, prog_ctx, state, loc);
        let cond = append_val(
            block,
            arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, st, z0, loc),
            prog_ctx.fallback_val,
        );
        emit_size_error_branch(
            cond,
            &spec.on_size_error,
            &spec.not_on_size_error,
            loc,
            block,
            prog_ctx,
            state,
        )?;
    }
    Ok(())
}

/// Lowers `HirStmt::Initialize` across all target places and `REPLACING` rules.
///
/// # Arguments
///
/// * `init_spec` - `(targets, filler, value_mode, span)` tuple.
/// * `replacements` - `(DataCategory, replacement_expr)` rules.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_initialize<'ctx, 'b, 'e: 'b>(
    init_spec: (&[HirPlace], HirInitFiller, HirInitValueMode, SourceSpan),
    replacements: &[(DataCategory, HirExpr)],
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let (targets, filler, value_mode, span) = init_spec;
    let loc = state.loc(span)?;
    for target in targets {
        let tv = lower_place_view(target, block, prog_ctx, state)?;
        let plan_bytes =
            build_serialized_init_plan(target, filler, value_mode, prog_ctx.prog, state.interner)?;
        let (plan_ptr, plan_len) = emit_const_slice(&plan_bytes, block, prog_ctx, state, loc)?;

        let _ = emit_rt_call(
            RtDispatchTarget::InitBegin,
            &[],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        for (cat, rep_expr) in replacements {
            let cat_v = emit_i8(encode_data_category(*cat), block, prog_ctx, state, loc);
            let (kind_v, p, l, sp_p, sp_l) =
                lower_move_source_args(rep_expr, block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::InitPushRep,
                &[cat_v, kind_v, p, l, sp_p, sp_l],
                block,
                prog_ctx,
                state,
                loc,
            )?;
        }
        let _ = emit_rt_call(
            RtDispatchTarget::InitExec,
            &[tv.ptr, tv.len_i64, plan_ptr, plan_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Lowers `HirStmt::Display` via staged `DispBegin` / `DispPush*` / `DispFlush` ops.
///
/// # Arguments
///
/// * `operands` - Expressions to display in left-to-right order.
/// * `device` - Target output device (`Stdout` / `Stderr`).
/// * `advancing` - Line-advancing mode.
/// * `span` - Statement source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_display<'ctx, 'b, 'e: 'b>(
    operands: &[HirExpr],
    device: HirDisplayDevice,
    advancing: HirAdvancing,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let mut pre_m12 = Vec::new();
    collect_pre_eval_m12_display(operands, &mut pre_m12, block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::DispBegin,
        &[],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let mut m12_idx = 0_usize;
    for op_expr in operands {
        emit_display_operand_with_m12(
            op_expr,
            &pre_m12,
            &mut m12_idx,
            block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    let dev_code = match device {
        HirDisplayDevice::Stdout => OutputDevice::Stdout.to_abi_code(),
        HirDisplayDevice::Stderr => OutputDevice::Stderr.to_abi_code(),
    };
    let adv_code = match advancing {
        HirAdvancing::AdvanceLine => LineAdvancing::AdvanceLine.to_abi_code(),
        HirAdvancing::NoAdvancing => LineAdvancing::NoAdvancing.to_abi_code(),
    };
    let dv = emit_i32(dev_code, block, prog_ctx, state, loc);
    let av = emit_i32(adv_code, block, prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::DispFlush,
        &[dv, av],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Pre-evaluated `(ptr, len, optional (spec_ptr, spec_len))` tuple for an `M12Call` inside `DISPLAY`.
type PreEvalM12Display<'ctx, 'b> = (
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Option<(Value<'ctx, 'b>, Value<'ctx, 'b>)>,
);

/// Pre-evaluates all non-M20 `HirExpr::M12Call` subexpressions in `operands` prior to `DispBegin`.
///
/// # Arguments
///
/// * `operands` - `DISPLAY` operand slice.
/// * `out` - Output vector of pre-evaluated tuples.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn collect_pre_eval_m12_display<'ctx, 'b, 'e: 'b>(
    operands: &[HirExpr],
    out: &mut Vec<Option<PreEvalM12Display<'ctx, 'b>>>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    for expr in operands {
        collect_pre_eval_m12_expr(expr, out, block, prog_ctx, state, loc)?;
    }
    Ok(())
}

/// Recursively pre-evaluates `M12Call` nodes inside `expr` (`Concat` or top-level `M12Call`).
///
/// # Arguments
///
/// * `expr` - Operand expression.
/// * `out` - Output vector of pre-evaluated tuples.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn collect_pre_eval_m12_expr<'ctx, 'b, 'e: 'b>(
    expr: &HirExpr,
    out: &mut Vec<Option<PreEvalM12Display<'ctx, 'b>>>,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    match expr {
        HirExpr::Binary {
            op: HirBinOp::Concat,
            lhs,
            rhs,
            ..
        } => {
            collect_pre_eval_m12_expr(lhs, out, block, prog_ctx, state, loc)?;
            collect_pre_eval_m12_expr(rhs, out, block, prog_ctx, state, loc)
        }
        HirExpr::M12Call(c) => {
            let skip_pre = matches!(
                c.callee,
                coblang_hir::HirM12Callee::Intrinsic { abi_code }
                    if abi_code == coblang_intrinsics::IntrinsicId::HexOf.to_abi_code()
                        || abi_code == coblang_intrinsics::IntrinsicId::Trim.to_abi_code()
                        || abi_code == coblang_intrinsics::IntrinsicId::ContentOf.to_abi_code()
                        || abi_code == coblang_intrinsics::IntrinsicId::DisplayOf.to_abi_code()
                        || abi_code == coblang_intrinsics::IntrinsicId::NationalOf.to_abi_code()
            ) && c.refmod.is_none();
            if skip_pre {
                out.push(None);
            } else {
                let eval =
                    crate::lower_m12::pre_eval_m12_for_display(c, block, prog_ctx, state, loc)?;
                out.push(Some(eval));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Emits one `DISPLAY` operand consuming pre-evaluated `M12Call` slots in `pre_m12`.
///
/// # Arguments
///
/// * `op_expr` - Operand expression.
/// * `pre_m12` - Pre-evaluated `M12Call` buffer slice.
/// * `m12_idx` - Mutable cursor into `pre_m12`.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_display_operand_with_m12<'ctx, 'b, 'e: 'b>(
    op_expr: &HirExpr,
    pre_m12: &[Option<PreEvalM12Display<'ctx, 'b>>],
    m12_idx: &mut usize,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    match op_expr {
        HirExpr::Binary {
            op: HirBinOp::Concat,
            lhs,
            rhs,
            ..
        } => {
            emit_display_operand_with_m12(lhs, pre_m12, m12_idx, block, prog_ctx, state, loc)?;
            emit_display_operand_with_m12(rhs, pre_m12, m12_idx, block, prog_ctx, state, loc)
        }
        HirExpr::M12Call(_) => {
            let slot = pre_m12.get(*m12_idx).copied().flatten();
            *m12_idx += 1;
            if let Some((ptr, len_i64, opt_spec)) = slot {
                if let Some((sp_ptr, sp_len)) = opt_spec {
                    let _ = emit_rt_call(
                        RtDispatchTarget::DispPushField,
                        &[ptr, len_i64, sp_ptr, sp_len],
                        block,
                        prog_ctx,
                        state,
                        loc,
                    )?;
                } else {
                    let _ = emit_rt_call(
                        RtDispatchTarget::DispPushRaw,
                        &[ptr, len_i64],
                        block,
                        prog_ctx,
                        state,
                        loc,
                    )?;
                }
                Ok(())
            } else {
                emit_display_operand(op_expr, block, prog_ctx, state, loc)
            }
        }
        _ => emit_display_operand(op_expr, block, prog_ctx, state, loc),
    }
}

/// Appends one `DISPLAY` operand (`Place`, `Const`, `Concat`, `FUNCTION TRIM`, `FUNCTION HEX-OF`, or expression).
///
/// # Arguments
///
/// * `op_expr` - Operand expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`CoblangError`] if operand lowering fails.
pub(crate) fn emit_display_operand<'ctx, 'b, 'e: 'b>(
    op_expr: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    match op_expr {
        HirExpr::Binary {
            op: HirBinOp::Concat,
            lhs,
            rhs,
            ..
        } => {
            emit_display_operand(lhs, block, prog_ctx, state, loc)?;
            emit_display_operand(rhs, block, prog_ctx, state, loc)
        }
        HirExpr::Place(pl) => {
            let pv = lower_place_view(pl, block, prog_ctx, state)?;
            if crate::lower_m17::requires_formatted_display_push(
                &pl.effective_ty,
                prog_ctx.prog.formatting,
            ) {
                let (sp_ptr, sp_len) = emit_spec_slice(&pv.spec, block, prog_ctx, state, loc)?;
                let _ = emit_rt_call(
                    RtDispatchTarget::DispPushField,
                    &[pv.ptr, pv.len_i64, sp_ptr, sp_len],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            } else {
                let _ = emit_rt_call(
                    RtDispatchTarget::DispPushRaw,
                    &[pv.ptr, pv.len_i64],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            }
            Ok(())
        }
        HirExpr::Const { value, .. } => {
            let literal_bytes = crate::lower_m17::const_to_display_bytes_m17(
                value,
                prog_ctx.prog.formatting,
                state.interner,
            );
            let (p, l) = emit_const_slice(&literal_bytes, block, prog_ctx, state, loc)?;
            let _ = emit_rt_call(
                RtDispatchTarget::DispPushRaw,
                &[p, l],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            Ok(())
        }
        HirExpr::AddressOf(pl) => emit_display_address_of(pl, block, prog_ctx, state, loc),
        HirExpr::CallFunction { name, args, span } => {
            emit_display_call_fn(*name, args, *span, block, prog_ctx, state, loc)
        }
        HirExpr::M12Call(c) => {
            if c.refmod.is_none()
                && crate::lower_m20::try_emit_m12_display_hex_or_trim(
                    c, block, prog_ctx, state, loc,
                )?
            {
                return Ok(());
            }
            let (ptr, len_i64, opt_spec) =
                crate::lower_m12::pre_eval_m12_for_display(c, block, prog_ctx, state, loc)?;
            if let Some((sp_ptr, sp_len)) = opt_spec {
                let _ = emit_rt_call(
                    RtDispatchTarget::DispPushField,
                    &[ptr, len_i64, sp_ptr, sp_len],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            } else {
                let _ = emit_rt_call(
                    RtDispatchTarget::DispPushRaw,
                    &[ptr, len_i64],
                    block,
                    prog_ctx,
                    state,
                    loc,
                )?;
            }
            Ok(())
        }
        HirExpr::M20(m) => {
            crate::lower_m20::emit_m20_display_operand(m, block, prog_ctx, state, loc)
        }
        other => emit_display_scalar_expr(other, block, prog_ctx, state, loc),
    }
}

/// Evaluates an arithmetic/scalar `DISPLAY` operand via `idx_scratch_sym` and pushes it to display staging.
///
/// # Arguments
///
/// * `other` - Scalar HIR expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_display_scalar_expr<'ctx, 'b, 'e: 'b>(
    other: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let v64 = eval_expr_as_i64(other, block, prog_ctx, state)?;
    let scratch_ptr =
        emit_storage_ptr(&prog_ctx.idx_scratch_sym, 0, 8, block, prog_ctx, state, loc)?;
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        v64,
        scratch_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let spec = make_runtime_field_spec(
        HirType::BinaryInt {
            byte_width: 8,
            digits: DigitCount::try_new(18).unwrap_or(DigitCount::ONE),
            scale: ScaleFactor::ZERO,
            sign: Signedness::Signed,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::TruncateToPic,
        },
        prog_ctx.prog.formatting,
    );
    let (sp_p, sp_l) = emit_spec_slice(&spec, block, prog_ctx, state, loc)?;
    let eight = emit_i64(8, block, prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::DispPushField,
        &[scratch_ptr, eight, sp_p, sp_l],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Emits `DISPLAY ADDRESS OF place` formatted as an 8-byte `USAGE POINTER`.
///
/// # Arguments
///
/// * `pl` - Target [`HirPlace`].
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_display_address_of<'ctx, 'b, 'e: 'b>(
    pl: &HirPlace,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let p = crate::lower_m11::eval_address_of_place(pl, block, prog_ctx, state)?;
    let scratch_ptr =
        emit_storage_ptr(&prog_ctx.idx_scratch_sym, 0, 8, block, prog_ctx, state, loc)?;
    let _ = block.append_operation(llvm::store(
        state.ctx.raw(),
        p,
        scratch_ptr,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let spec = make_runtime_field_spec(HirType::Pointer, prog_ctx.prog.formatting);
    let (sp_p, sp_l) = emit_spec_slice(&spec, block, prog_ctx, state, loc)?;
    let eight = emit_i64(8, block, prog_ctx, state, loc);
    let _ = emit_rt_call(
        RtDispatchTarget::DispPushField,
        &[scratch_ptr, eight, sp_p, sp_l],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Emits `DISPLAY` staging ops for `FUNCTION HEX-OF` or `FUNCTION TRIM`.
///
/// # Arguments
///
/// * `name` - Interned function identifier.
/// * `args` - Call arguments.
/// * `span` - Call source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn emit_display_call_fn<'ctx, 'b, 'e: 'b>(
    name: coblang_common::SymbolId,
    args: &[HirExpr],
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    let fname = state.interner.resolve_canonical(name).unwrap_or("");
    if fname == "HEX-OF"
        && let Some(first) = args.first()
    {
        let (_, p, l, _, _) = lower_move_source_args(first, block, prog_ctx, state, loc)?;
        let _ = emit_rt_call(
            RtDispatchTarget::DispPushHex,
            &[p, l],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok(());
    }
    let (_, p, l, _, _) =
        lower_intrinsic_as_move_source(name, args, span, block, prog_ctx, state, loc)?;
    let _ = emit_rt_call(
        RtDispatchTarget::DispPushRaw,
        &[p, l],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(())
}

/// Formats a compile-time [`HirConstValue`] into raw `DISPLAY` bytes.
///
/// # Arguments
///
/// * `value` - Constant HIR literal value.
/// * `interner` - Symbol interner.
pub(crate) fn const_to_display_bytes(value: &HirConstValue, interner: &SymbolInterner) -> Vec<u8> {
    match value {
        HirConstValue::Bytes(b) | HirConstValue::NationalBytes(b) | HirConstValue::Utf8Bytes(b) => {
            b.clone()
        }
        HirConstValue::Figurative(fig) => match fig {
            HirFigurative::Zero => vec![b'0'],
            HirFigurative::Space => vec![b' '],
            HirFigurative::HighValue => vec![0xFF],
            HirFigurative::LowValue | HirFigurative::Null => vec![0x00],
            HirFigurative::Quote => vec![b'"'],
            HirFigurative::All(p) => p.clone(),
        },
        HirConstValue::Decimal { text, scale, sign } => {
            let raw = interner.resolve_original(*text).unwrap_or("0");
            format_hir_decimal_ascii(raw, *scale, *sign).into_bytes()
        }
        HirConstValue::Float(sym) => interner
            .resolve_original(*sym)
            .unwrap_or("0")
            .as_bytes()
            .to_vec(),
    }
}

/// Lowers an [`HirCondition`] into a boolean `i1` SSA value.
///
/// # Arguments
///
/// * `cond` - Condition AST/HIR node.
/// * `span` - Enclosing statement span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
pub(crate) fn lower_condition<'ctx, 'b, 'e: 'b>(
    cond: &HirCondition,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let loc = state.loc(span)?;
    let i1_ty = IntegerType::new(state.ctx.raw(), 1).into();
    match cond {
        HirCondition::BoolConst(tv) => {
            let b = i64::from(*tv == HirTruthValue::True);
            Ok(append_val(
                block,
                arith::constant(state.ctx.raw(), IntegerAttribute::new(i1_ty, b).into(), loc),
                prog_ctx.fallback_val,
            ))
        }
        HirCondition::Not(inner) => {
            let v = lower_condition(inner, span, block, prog_ctx, state)?;
            let one = append_val(
                block,
                arith::constant(state.ctx.raw(), IntegerAttribute::new(i1_ty, 1).into(), loc),
                prog_ctx.fallback_val,
            );
            Ok(append_val(
                block,
                arith::xori(v, one, loc),
                prog_ctx.fallback_val,
            ))
        }
        HirCondition::And(a, b) => {
            let av = lower_condition(a, span, block, prog_ctx, state)?;
            let bv = lower_condition(b, span, block, prog_ctx, state)?;
            Ok(append_val(
                block,
                arith::andi(av, bv, loc),
                prog_ctx.fallback_val,
            ))
        }
        HirCondition::Or(a, b) => {
            let av = lower_condition(a, span, block, prog_ctx, state)?;
            let bv = lower_condition(b, span, block, prog_ctx, state)?;
            Ok(append_val(
                block,
                arith::ori(av, bv, loc),
                prog_ctx.fallback_val,
            ))
        }
        HirCondition::ClassCheck { expr, class_name } => {
            lower_class_check(expr, *class_name, span, block, prog_ctx, state, loc)
        }
        HirCondition::Compare { op, lhs, rhs } => {
            lower_compare_cond(*op, lhs, rhs, block, prog_ctx, state, loc)
        }
        HirCondition::Switch {
            switch_index,
            expected,
        } => {
            let idx_v = emit_i32(i32::from(*switch_index), block, prog_ctx, state, loc);
            let cur_v = emit_rt_call(
                RtDispatchTarget::GetSwitch,
                &[idx_v],
                block,
                prog_ctx,
                state,
                loc,
            )?;
            let want = emit_i32(
                match expected {
                    HirTruthValue::True => 1,
                    HirTruthValue::False => 0,
                },
                block,
                prog_ctx,
                state,
                loc,
            );
            Ok(append_val(
                block,
                arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, cur_v, want, loc),
                prog_ctx.fallback_val,
            ))
        }
        HirCondition::Omitted { item } => {
            crate::lower_m11::lower_m11_omitted_condition(*item, span, block, prog_ctx, state)
        }
    }
}

/// Lowers `IS NUMERIC` / `IS ALPHABETIC*` / custom `SPECIAL-NAMES. CLASS` into `i1`.
///
/// # Arguments
///
/// * `subject` - Tested subject expression.
/// * `class_name` - Interned class name symbol.
/// * `span` - Enclosing condition span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_class_check<'ctx, 'b, 'e: 'b>(
    subject: &HirExpr,
    class_name: coblang_common::SymbolId,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let HirExpr::Place(pl) = subject else {
        return Err(unimpl(
            FeatureTag::FeaturePerformEvalIf,
            "non-place CLASS subject",
            state.source_map,
            span,
        ));
    };
    let pv = lower_place_view(pl, block, prog_ctx, state)?;
    let z0 = emit_i32(0, block, prog_ctx, state, loc);

    let cname = state.interner.resolve_canonical(class_name).unwrap_or("");
    let builtin_kind = match cname {
        "NUMERIC" => Some(ClassTestKind::Numeric),
        "ALPHABETIC" => Some(ClassTestKind::Alphabetic),
        "ALPHABETIC-LOWER" => Some(ClassTestKind::AlphabeticLower),
        "ALPHABETIC-UPPER" => Some(ClassTestKind::AlphabeticUpper),
        _ => None,
    };
    if let Some(ckind) = builtin_kind {
        let (sp_ptr, sp_len) = emit_spec_slice(&pv.spec, block, prog_ctx, state, loc)?;
        let ck_v = emit_i32(ckind.to_abi_code(), block, prog_ctx, state, loc);
        let res = emit_rt_call(
            RtDispatchTarget::ClassCheck,
            &[pv.ptr, pv.len_i64, sp_ptr, sp_len, ck_v],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok(append_val(
            block,
            arith::cmpi(state.ctx.raw(), CmpiPredicate::Ne, res, z0, loc),
            prog_ctx.fallback_val,
        ));
    }

    if let Some(custom_cls) = prog_ctx
        .prog
        .custom_classes
        .iter()
        .find(|c| c.name == class_name)
    {
        let (bm_ptr, _) = emit_const_slice(&custom_cls.bitmap, block, prog_ctx, state, loc)?;
        let res = emit_rt_call(
            RtDispatchTarget::ClassCustom,
            &[pv.ptr, pv.len_i64, bm_ptr],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok(append_val(
            block,
            arith::cmpi(
                state.ctx.raw(),
                CmpiPredicate::Eq,
                res,
                emit_i32(1, block, prog_ctx, state, loc),
                loc,
            ),
            prog_ctx.fallback_val,
        ));
    }

    Err(unimpl(
        FeatureTag::FeaturePerformEvalIf,
        format!("custom CLASS `{cname}`"),
        state.source_map,
        span,
    ))
}

/// Lowers numeric or alphanumeric relational comparison `lhs <op> rhs` into `i1`.
///
/// # Arguments
///
/// * `op` - Relational comparison operator.
/// * `lhs` - Left-hand expression.
/// * `rhs` - Right-hand expression.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
fn lower_compare_cond<'ctx, 'b, 'e: 'b>(
    op: HirRelOp,
    lhs: &HirExpr,
    rhs: &HirExpr,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    if crate::lower_m11::is_pointer_compare_pair(lhs, rhs) {
        return crate::lower_m11::lower_pointer_compare_cond(
            op, lhs, rhs, block, prog_ctx, state, loc,
        );
    }
    if let Some(obj_cmp) =
        crate::lower_m20::try_lower_object_ref_compare(op, lhs, rhs, block, prog_ctx, state, loc)?
    {
        return Ok(obj_cmp);
    }
    let rel_code = match op {
        HirRelOp::Eq => RelCmpOp::Eq,
        HirRelOp::Ne => RelCmpOp::Ne,
        HirRelOp::Lt => RelCmpOp::Lt,
        HirRelOp::Le => RelCmpOp::Le,
        HirRelOp::Gt => RelCmpOp::Gt,
        HirRelOp::Ge => RelCmpOp::Ge,
    }
    .to_abi_code();
    let rel_v = emit_i32(rel_code, block, prog_ctx, state, loc);

    if is_numeric_cmp_pair(lhs, rhs) {
        if let Some((fl, fr)) = try_classify_fast_int_cmp(lhs, rhs, state.interner) {
            let (lv, _) = eval_fast_i128(&fl, block, prog_ctx, state, loc)?;
            let (rv, _) = eval_fast_i128(&fr, block, prog_ctx, state, loc)?;
            let pred = match op {
                HirRelOp::Eq => CmpiPredicate::Eq,
                HirRelOp::Ne => CmpiPredicate::Ne,
                HirRelOp::Lt => CmpiPredicate::Slt,
                HirRelOp::Le => CmpiPredicate::Sle,
                HirRelOp::Gt => CmpiPredicate::Sgt,
                HirRelOp::Ge => CmpiPredicate::Sge,
            };
            return Ok(append_val(
                block,
                arith::cmpi(state.ctx.raw(), pred, lv, rv, loc),
                prog_ctx.fallback_val,
            ));
        }
        let _ = emit_rt_call(RtDispatchTarget::DecBegin, &[], block, prog_ctx, state, loc)?;
        emit_decimal_expr(lhs, block, prog_ctx, state)?;
        emit_decimal_expr(rhs, block, prog_ctx, state)?;
        let res = emit_rt_call(
            RtDispatchTarget::DecCmp,
            &[rel_v],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        return Ok(append_val(
            block,
            arith::cmpi(
                state.ctx.raw(),
                CmpiPredicate::Eq,
                res,
                emit_i32(1, block, prog_ctx, state, loc),
                loc,
            ),
            prog_ctx.fallback_val,
        ));
    }

    let (lk, lp, ll, lsp, lsl) = lower_move_source_args(lhs, block, prog_ctx, state, loc)?;
    let (rk, rp, rl, rsp, rsl) = lower_move_source_args(rhs, block, prog_ctx, state, loc)?;
    let (coll_ptr, coll_len) = if let Some(alpha) = prog_ctx
        .prog
        .collating_sequence
        .and_then(|idx| prog_ctx.prog.alphabets.get(idx))
    {
        emit_const_slice(&alpha.ranks, block, prog_ctx, state, loc)?
    } else {
        let null_p = append_val(
            block,
            llvm::zero(llvm_ptr_type(state.ctx), loc),
            prog_ctx.fallback_val,
        );
        (null_p, emit_i64(0, block, prog_ctx, state, loc))
    };
    let res = emit_rt_call(
        RtDispatchTarget::CmpAlphaEx,
        &[
            lk, lp, ll, lsp, lsl, rk, rp, rl, rsp, rsl, rel_v, coll_ptr, coll_len,
        ],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    let one = emit_i32(1, block, prog_ctx, state, loc);
    Ok(append_val(
        block,
        arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, res, one, loc),
        prog_ctx.fallback_val,
    ))
}

/// Returns `true` when `lhs` and `rhs` form an ISO COBOL algebraic numeric comparison.
///
/// # Arguments
///
/// * `lhs` - Left-hand operand expression.
/// * `rhs` - Right-hand operand expression.
fn is_numeric_cmp_pair(lhs: &HirExpr, rhs: &HirExpr) -> bool {
    let lc = classify_cmp_operand(lhs);
    let rc = classify_cmp_operand(rhs);
    matches!(
        (lc, rc),
        (CmpClass::Numeric, CmpClass::Numeric | CmpClass::FigZero)
            | (CmpClass::FigZero, CmpClass::Numeric)
    )
}

/// Operand category for relational comparison dispatch.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CmpClass {
    /// Strict numeric operand.
    Numeric,
    /// `ZERO` figurative constant (adapts to numeric or alphanumeric).
    FigZero,
    /// Non-numeric (`Alphanumeric`, `Group`, `Edited`, `Pointer`, etc.).
    NonNumeric,
}

/// Classifies `expr` for numeric vs alphanumeric relational comparison.
///
/// # Arguments
///
/// * `expr` - Operand expression.
fn classify_cmp_operand(expr: &HirExpr) -> CmpClass {
    match expr {
        HirExpr::Place(pl) => match pl.effective_ty.category() {
            DataCategory::Numeric | DataCategory::Index => CmpClass::Numeric,
            _ => CmpClass::NonNumeric,
        },
        HirExpr::Const { value, .. } => match value {
            HirConstValue::Decimal { .. } | HirConstValue::Float(_) => CmpClass::Numeric,
            HirConstValue::Figurative(HirFigurative::Zero) => CmpClass::FigZero,
            _ => CmpClass::NonNumeric,
        },
        HirExpr::Neg(_) | HirExpr::Binary { .. } => CmpClass::Numeric,
        HirExpr::M12Call(c) => match c.return_ty.category() {
            DataCategory::Numeric | DataCategory::Index => CmpClass::Numeric,
            _ => CmpClass::NonNumeric,
        },
        HirExpr::M20(m) => match m.as_ref() {
            coblang_hir::HirM20Expr::OoInlineInvoke(inv) => match inv.return_ty.category() {
                DataCategory::Numeric | DataCategory::Index => CmpClass::Numeric,
                _ => CmpClass::NonNumeric,
            },
            coblang_hir::HirM20Expr::OoSelf { .. } => CmpClass::NonNumeric,
        },
        _ => CmpClass::NonNumeric,
    }
}

/// Lowers `HirStmt::If` into a structured `scf.if` operation.
///
/// # Arguments
///
/// * `cond` - Branch condition.
/// * `then_body` - Statements for `TRUE` branch.
/// * `else_body` - Statements for `FALSE` branch.
/// * `span` - Statement source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_if<'ctx, 'b, 'e: 'b>(
    cond: &HirCondition,
    then_body: &[HirStmt],
    else_body: &[HirStmt],
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let cond_val = lower_condition(cond, span, block, prog_ctx, state)?;
    let loc = state.loc(span)?;

    let then_region = Region::new();
    let then_blk = then_region.append_block(Block::new(&[]));
    for st in then_body {
        lower_stmt(st, &then_blk, prog_ctx, state)?;
    }
    let _ = then_blk.append_operation(scf::r#yield(&[], loc));

    let else_region = Region::new();
    let else_blk = else_region.append_block(Block::new(&[]));
    for st in else_body {
        lower_stmt(st, &else_blk, prog_ctx, state)?;
    }
    let _ = else_blk.append_operation(scf::r#yield(&[], loc));

    let _ = block.append_operation(scf::r#if(cond_val, &[], then_region, else_region, loc));
    Ok(())
}

/// Lowers `STOP RUN`, `GOBACK`, and `EXIT PROGRAM`.
///
/// # Arguments
///
/// * `mode` - Return/termination flavor.
/// * `code` - Optional explicit return-code expression.
/// * `span` - Statement source span.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
fn lower_return<'ctx, 'b, 'e: 'b>(
    mode: HirReturnMode,
    code: Option<&HirExpr>,
    span: SourceSpan,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
) -> Result<()> {
    let loc = state.loc(span)?;
    let i32_ty: Type<'ctx> = IntegerType::new(state.ctx.raw(), 32).into();
    let _ = TypeAttribute::new(i32_ty);
    match mode {
        HirReturnMode::StopRun => {
            let status_val = if let Some(expr) = code {
                let v64 = eval_expr_as_i64(expr, block, prog_ctx, state)?;
                append_val(
                    block,
                    arith::trunci(v64, i32_ty, loc),
                    prog_ctx.fallback_val,
                )
            } else {
                let get_op = CobolGetReturnCodeOp.build(state.ctx, loc)?;
                append_val(block, get_op, prog_ctx.fallback_val)
            };
            let op = CobolStopRunOp { status: status_val }.build(state.ctx, loc)?;
            let _ = block.append_operation(op);
            Ok(())
        }
        HirReturnMode::Goback | HirReturnMode::ExitProgram => {
            if let Some(expr) = code {
                let v64 = eval_expr_as_i64(expr, block, prog_ctx, state)?;
                let cval = append_val(
                    block,
                    arith::trunci(v64, i32_ty, loc),
                    prog_ctx.fallback_val,
                );
                let set_op = CobolSetReturnCodeOp { code: cval }.build(state.ctx, loc)?;
                let _ = block.append_operation(set_op);
            } else if !prog_ctx.is_udf_unit {
                crate::lower_m11::emit_returning_item_sync(block, prog_ctx, state, loc)?;
            }
            let gb_mode = match mode {
                HirReturnMode::ExitProgram => GobackMode::ExitProgram,
                HirReturnMode::Goback | HirReturnMode::StopRun => GobackMode::Goback,
            };
            let op = CobolGobackOp { mode: gb_mode }.build(state.ctx, loc)?;
            let _ = block.append_operation(op);
            Ok(())
        }
    }
}

/// Registers `FUNCTION-ID` User-Defined Function units into module symbol and parameter tables.
///
/// # Arguments
///
/// * `functions` - Lowered HIR `FUNCTION-ID` units.
/// * `interner` - Symbol interner for external name lookup.
/// * `sym_tables` - Mutable symbol deduplication tables.
/// * `emitted_rc_roots` - Root `RETURN-CODE` storage slot list.
/// * `udf_unit_ids` - Set of UDF `ProgramUnitId`s.
/// * `udf_param_types` - Output map of `__UDF_<EXT>` formal parameter types and formatting.
/// * `flattened` - Flattened `(program, rc_sym)` emission queue.
fn register_udf_units_m12<'a>(
    functions: &'a [coblang_hir::HirFunction],
    interner: &SymbolInterner,
    sym_tables: &mut ProgramSymbolTables,
    emitted_rc_roots: &mut Vec<(String, SourceSpan)>,
    udf_unit_ids: &mut BTreeSet<ProgramUnitId>,
    udf_param_types: &mut BTreeMap<String, (Vec<HirType>, coblang_hir::HirProgramFormatting)>,
    flattened: &mut Vec<(&'a HirProgram, String)>,
) {
    for (udf_idx, udf) in functions.iter().enumerate() {
        let rc_sym = format!("__coblang_rc_udf_{udf_idx}");
        emitted_rc_roots.push((rc_sym.clone(), udf.body.span));
        let ext_orig = interner
            .resolve_original(udf.external_name)
            .unwrap_or("UDF");
        let ext_upper = ext_orig.trim().to_ascii_uppercase();
        let udf_key = format!("__UDF_{ext_upper}");
        let param_tys: Vec<HirType> = udf
            .body
            .using_params
            .iter()
            .filter_map(|up| udf.body.data_items.get(up.item.as_u32() as usize))
            .map(|di| di.ty.clone())
            .collect();
        let _ = udf_param_types.insert(udf_key.clone(), (param_tys, udf.body.formatting));
        let base_canon = mangle_program_symbol(ext_orig, SymbolMangleStyle::CobolCanonical);
        let canon = if sym_tables.used_syms.insert(base_canon.clone()) {
            base_canon
        } else {
            let fallback_canon = mangle_program_symbol(&udf_key, SymbolMangleStyle::CobolCanonical);
            let _ = sym_tables.used_syms.insert(fallback_canon.clone());
            fallback_canon
        };
        let candidate_alias = compute_raw_c_alias(ext_orig);
        let raw_alias = if !candidate_alias.is_empty()
            && sym_tables.used_syms.insert(candidate_alias.clone())
        {
            candidate_alias
        } else {
            String::new()
        };
        sym_tables.in_module_programs.insert(udf_key, canon.clone());
        sym_tables
            .program_symbols
            .insert(udf.body.id, (canon, raw_alias));
        let _ = udf_unit_ids.insert(udf.body.id);
        flattened.push((&udf.body, rc_sym));
    }
}

/// Resolves the ISO `EXCEPTION-LOCATION` paragraph or `para OF section` label for `body_ctx`.
///
/// # Arguments
///
/// * `body_ctx` - Active body execution context.
/// * `prog` - Enclosing HIR program.
/// * `interner` - Symbol interner.
fn resolve_active_proc_label(
    body_ctx: BodyExecContext,
    prog: &HirProgram,
    interner: &SymbolInterner,
) -> Option<String> {
    match body_ctx {
        BodyExecContext::Paragraph { pid, .. } => prog.procedures.get(pid as usize).and_then(|p| {
            let pname = interner.resolve_original(p.name).ok()?;
            if p.kind == coblang_hir::HirProcKind::SectionHeader {
                return Some(pname.to_owned());
            }
            let enclosing_sec = prog
                .procedures
                .get(..pid as usize)
                .and_then(|prefix| {
                    prefix.iter().rev().find(|cand| {
                        cand.kind == coblang_hir::HirProcKind::SectionHeader
                            && cand.section_end.as_u32() >= pid
                    })
                })
                .and_then(|sec| interner.resolve_original(sec.name).ok());
            Some(match enclosing_sec {
                Some(sname) => format!("{pname} OF {sname}"),
                None => pname.to_owned(),
            })
        }),
        BodyExecContext::ProgramEntry | BodyExecContext::EntryHelper => None,
    }
}

/// Computes `(base_item_len_val, effective_len_i64)` for a `HirPlace`, accounting for `ANY LENGTH` and dynamic `ODO`.
///
/// # Arguments
///
/// * `place` - Resolved HIR place expression.
/// * `hir_item` - Optional underlying [`HirDataItem`].
/// * `static_elem_len` - Static element byte length from storage layout.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`CoblangError`] if dynamic ODO or `ANY LENGTH` size emission fails.
fn compute_place_item_lengths<'ctx, 'b, 'e: 'b>(
    place: &HirPlace,
    hir_item: Option<&HirDataItem>,
    static_elem_len: u32,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(Value<'ctx, 'b>, Value<'ctx, 'b>)> {
    let any_len_slot = prog_ctx
        .prog
        .using_params
        .iter()
        .filter(|p| p.source == coblang_hir::HirParamSource::Using)
        .enumerate()
        .find(|(_, p)| {
            p.item == place.item && p.length_mode == coblang_hir::HirParamLengthMode::AnyLength
        })
        .map(|(ord, _)| ord + usize::from(prog_ctx.is_udf_unit));

    let base_item_len_val = if let Some(slot_idx) = any_len_slot {
        load_any_length_param_size(slot_idx, block, prog_ctx, state, loc)?
    } else if let Some(it) = hir_item.filter(|it| {
        matches!(
            it.ty,
            HirType::Group {
                odo_presence: DynamicOdoPresence::HasDynamicOdo,
                ..
            }
        )
    }) {
        compute_dynamic_elem_len(it, place.check_mask, place.span, block, prog_ctx, state)?
    } else {
        emit_i64(
            i64::from(static_elem_len.max(1)),
            block,
            prog_ctx,
            state,
            loc,
        )
    };

    let len_i64 = if any_len_slot.is_some()
        || matches!(
            place.effective_ty,
            HirType::Group {
                odo_presence: DynamicOdoPresence::HasDynamicOdo,
                ..
            }
        ) {
        base_item_len_val
    } else {
        emit_i64(
            i64::from(place.effective_ty.static_byte_size().max(1)),
            block,
            prog_ctx,
            state,
            loc,
        )
    };
    Ok((base_item_len_val, len_i64))
}

/// Emits program-entry startup prologues (M11, M12, M13, M20, deferred `.bss`) or paragraph debug reset.
///
/// # Arguments
///
/// * `spec` - Per-body [`ProgramBodySpec`].
/// * `env` - Shared [`ProgramBodySharedEnv`].
/// * `entry_block` - Body entry block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Body source location.
///
/// # Errors
///
/// Returns [`CoblangError`] if emitting any startup prologue call fails.
fn emit_body_startup_prologues<'ctx, 'b, 'e: 'b>(
    spec: &ProgramBodySpec<'_>,
    env: &ProgramBodySharedEnv<'_>,
    entry_block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<()> {
    if spec.body_ctx == BodyExecContext::ProgramEntry {
        crate::lower_m12::emit_m12_module_enter(entry_block, prog_ctx, state, loc)?;
        crate::lower_m13::emit_m13_recursive_prologue(
            env.canon_sym,
            entry_block,
            prog_ctx,
            state,
            loc,
        )?;
        crate::lower_m11::emit_external_roots_startup(
            &env.storage.roots,
            entry_block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    if let Some((guard_sym, roots)) = spec.bss_startup {
        emit_deferred_bss_startup(guard_sym, roots, entry_block, prog_ctx, state, loc)?;
    }
    if spec.body_ctx == BodyExecContext::ProgramEntry {
        crate::lower_m11::emit_m11_program_entry_prologue(entry_block, prog_ctx, state, loc)?;
        let oo_regs = std::mem::take(&mut state.oo_regs);
        let res =
            crate::lower_m20::emit_m20_oo_startup(&oo_regs, entry_block, prog_ctx, state, loc);
        state.oo_regs = oo_regs;
        res?;
    }
    if let BodyExecContext::Paragraph { pid, .. } = spec.body_ctx
        && state.m13_units.get(&env.prog.id).is_some_and(|u| {
            u.debugging_mode == coblang_hir::HirDebugCompileMode::WithDebuggingMode
        })
        && let Some(p) = env.prog.procedures.get(pid as usize)
        && !matches!(p.statements.first(), Some(HirStmt::DebugTrace(_)))
        && !(p.kind == coblang_hir::HirProcKind::SectionHeader && p.statements.is_empty())
    {
        let zero_32 = emit_i32(0, entry_block, prog_ctx, state, loc);
        let _ = emit_rt_call(
            RtDispatchTarget::M13DebugSetSuppress,
            &[zero_32],
            entry_block,
            prog_ctx,
            state,
            loc,
        )?;
    }
    Ok(())
}

/// Queries the caller-supplied runtime byte size of an `ANY LENGTH` `LINKAGE` parameter slot.
///
/// # Arguments
///
/// * `slot_idx` - Zero-based parameter slot index in `GetCallParam`.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`CoblangError`] if emitting the `GetCallParam` runtime call fails.
fn load_any_length_param_size<'ctx, 'b, 'e: 'b>(
    slot_idx: usize,
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let raw = state.ctx.raw();
    let ptr_ty = llvm_ptr_type(state.ctx);
    let i64_ty: Type<'ctx> = IntegerType::new(raw, 64).into();
    let one_e = emit_i64(1, prog_ctx.entry_block, prog_ctx, state, loc);
    let sz_slot = append_val(
        prog_ctx.entry_block,
        llvm::alloca(
            raw,
            one_e,
            ptr_ty,
            loc,
            llvm::AllocaOptions::new().elem_type(Some(TypeAttribute::new(i64_ty))),
        ),
        prog_ctx.fallback_val,
    );
    let z64 = emit_i64(0, block, prog_ctx, state, loc);
    let _ = block.append_operation(llvm::store(
        raw,
        z64,
        sz_slot,
        loc,
        llvm::LoadStoreOptions::new(),
    ));
    let idx_v = emit_i64(
        i64::try_from(slot_idx).unwrap_or(0),
        block,
        prog_ctx,
        state,
        loc,
    );
    let _ = emit_rt_call(
        RtDispatchTarget::GetCallParam,
        &[idx_v, sz_slot],
        block,
        prog_ctx,
        state,
        loc,
    )?;
    Ok(append_val(
        block,
        llvm::load(raw, sz_slot, i64_ty, loc, llvm::LoadStoreOptions::new()),
        prog_ctx.fallback_val,
    ))
}

/// Emits `CheckSub` bounds validation and non-fatal `EC-BOUND-SUBSCRIPT` clamping for one dimension.
///
/// # Arguments
///
/// * `dim_bounds` - `(idx_val, static_max, odo_opt)` tuple for the subscript dimension.
/// * `diag_site` - `(is_fatal, loc_ptr, loc_len, span)` diagnostic metadata.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`CoblangError`] if emitting `CheckSub` or exception location arming fails.
fn emit_checked_subscript_dim<'ctx, 'b, 'e: 'b>(
    dim_bounds: (Value<'ctx, 'b>, u32, Option<Value<'ctx, 'b>>),
    diag_site: (
        Value<'ctx, 'b>,
        Value<'ctx, 'b>,
        Value<'ctx, 'b>,
        SourceSpan,
    ),
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<Value<'ctx, 'b>> {
    let (mut idx_val, static_max, odo_opt) = dim_bounds;
    let (is_fatal, loc_ptr, loc_len, span) = diag_site;
    let active_max_opt = if let Some(ov) = odo_opt {
        Some(ov)
    } else if static_max != u32::MAX {
        Some(emit_i64(i64::from(static_max), block, prog_ctx, state, loc))
    } else {
        None
    };
    if let Some(max_val) = active_max_opt {
        if prog_ctx.has_ec_tracking {
            crate::lower_m13::emit_arm_stmt_location("", span, block, prog_ctx, state, loc)?;
        }
        let sub_rc = emit_rt_call(
            RtDispatchTarget::CheckSub,
            &[idx_val, max_val, is_fatal, loc_ptr, loc_len],
            block,
            prog_ctx,
            state,
            loc,
        )?;
        if prog_ctx.has_ec_tracking {
            let two_32 = emit_i32(2, block, prog_ctx, state, loc);
            let is_clamped = append_val(
                block,
                arith::cmpi(state.ctx.raw(), CmpiPredicate::Eq, sub_rc, two_32, loc),
                prog_ctx.fallback_val,
            );
            let one_64 = emit_i64(1, block, prog_ctx, state, loc);
            idx_val = append_val(
                block,
                arith::select(is_clamped, one_64, idx_val, loc),
                prog_ctx.fallback_val,
            );
        }
    }
    Ok(idx_val)
}

/// Lowers a [`HirConstValue`] literal or figurative constant into `MoveDesc` source SSA arguments.
///
/// # Arguments
///
/// * `value` - HIR constant literal or figurative constant.
/// * `empty_spec` - `(empty_ptr, zero_i64)` sentinel slice for literal sources without field specs.
/// * `block` - Target insertion block.
/// * `prog_ctx` - Program lowering context.
/// * `state` - Module lowering state.
/// * `loc` - Source location.
///
/// # Errors
///
/// Returns [`CoblangError`] if interning constant literal bytes or parsing floating literals fails.
fn lower_const_move_source_args<'ctx, 'b, 'e: 'b>(
    value: &HirConstValue,
    empty_spec: (Value<'ctx, 'b>, Value<'ctx, 'b>),
    block: &'b Block<'ctx>,
    prog_ctx: &ProgramLowerCtx<'_, 'ctx, 'e>,
    state: &mut ModuleLowerState<'_, 'ctx>,
    loc: Location<'ctx>,
) -> Result<(
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
    Value<'ctx, 'b>,
)> {
    let (empty_ptr, zero_i64) = empty_spec;
    match value {
        HirConstValue::Bytes(bytes) => {
            let (p, l) = emit_const_slice(bytes, block, prog_ctx, state, loc)?;
            let k = emit_i32(
                RuntimeMoveSourceKind::AlphanumericLiteral.to_abi_code(),
                block,
                prog_ctx,
                state,
                loc,
            );
            Ok((k, p, l, empty_ptr, zero_i64))
        }
        HirConstValue::NationalBytes(be_bytes) => crate::lower_m17::emit_m17_const_move_args(
            be_bytes,
            RuntimeMoveSourceKind::NationalLiteral,
            block,
            prog_ctx,
            state,
            loc,
        ),
        HirConstValue::Utf8Bytes(u8_bytes) => crate::lower_m17::emit_m17_const_move_args(
            u8_bytes,
            RuntimeMoveSourceKind::Utf8Literal,
            block,
            prog_ctx,
            state,
            loc,
        ),
        HirConstValue::Figurative(fig) => {
            let (rk, pat): (RuntimeMoveSourceKind, &[u8]) = match fig {
                HirFigurative::Zero => (RuntimeMoveSourceKind::FigZero, &[]),
                HirFigurative::Space => (RuntimeMoveSourceKind::FigSpace, &[]),
                HirFigurative::HighValue => (RuntimeMoveSourceKind::FigHigh, &[]),
                HirFigurative::LowValue => (RuntimeMoveSourceKind::FigLow, &[]),
                HirFigurative::Quote => (RuntimeMoveSourceKind::FigQuote, &[]),
                HirFigurative::Null => (RuntimeMoveSourceKind::FigNull, &[]),
                HirFigurative::All(p) => (RuntimeMoveSourceKind::FigAll, p.as_slice()),
            };
            let (p, l) = emit_const_slice(pat, block, prog_ctx, state, loc)?;
            let k = emit_i32(rk.to_abi_code(), block, prog_ctx, state, loc);
            Ok((k, p, l, empty_ptr, zero_i64))
        }
        HirConstValue::Decimal { text, scale, sign } => {
            let raw = state.interner.resolve_original(*text).unwrap_or("0");
            let ascii = format_hir_decimal_ascii(raw, *scale, *sign);
            let (p, l) = emit_const_slice(ascii.as_bytes(), block, prog_ctx, state, loc)?;
            let k = emit_i32(
                RuntimeMoveSourceKind::DecimalAscii.to_abi_code(),
                block,
                prog_ctx,
                state,
                loc,
            );
            Ok((k, p, l, empty_ptr, zero_i64))
        }
        HirConstValue::Float(sym) => {
            let raw = state.interner.resolve_original(*sym).unwrap_or("0");
            let dec = crate::storage_init::parse_scientific_or_decimal(raw)?;
            let ascii = dec.to_string();
            let (p, l) = emit_const_slice(ascii.as_bytes(), block, prog_ctx, state, loc)?;
            let k = emit_i32(
                RuntimeMoveSourceKind::DecimalAscii.to_abi_code(),
                block,
                prog_ctx,
                state,
                loc,
            );
            Ok((k, p, l, empty_ptr, zero_i64))
        }
    }
}
