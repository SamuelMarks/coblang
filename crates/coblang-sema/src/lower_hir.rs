//! AST-to-HIR lowering pass (desugaring `CORRESPONDING`, `88` conditions, abbreviated relations, and `78` constants).

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{
    CoblangError, DataItemId, ProcedureId, ProgramUnitId, ReportId, Result, ScaleFactor, ScopeId,
    SourceSpan, SymbolId,
};
use coblang_diag::{
    Diagnostic, DiagnosticCode, DiagnosticSink, DialectConfig, OdoSlidePolicy, PerformReturnStyle,
    SymbolInterner,
};
use coblang_hir::{
    DataCategory, ExceptionCheckMask, HirAdvancing, HirAlphabetTable, HirBinOp, HirCondition,
    HirConstValue, HirDataItem, HirDecimalSign, HirDisplayDevice, HirEnvTargetKind, HirExpr,
    HirFigurative, HirFileConnector, HirFlowExitKind, HirFunction, HirInitFiller, HirInitValueMode,
    HirInitialState, HirModule, HirOdoSlideMode, HirParagraph, HirPerformStyle, HirPlace,
    HirProcKind, HirProgram, HirProgramFormatting, HirRecursivity, HirRefMod, HirRelOp,
    HirReturnMode, HirRounding, HirStmt, HirSwitchSpec, HirTruthValue, ResolvedHirModule,
};
use coblang_syntax::ast::{
    AbbrevLogicOp, AbbrevRhs, BinaryOp, CallArg, ClassCheckKind, CommonVisibility,
    CompilationGroup, CompilationUnit, Condition, CorrespondingMode, DataDivision, DataRef,
    DataRefModifier, DecimalPointKind, DeviceMnemonicDecl, DisplayAdvancing, DivideOrientation,
    EnvironmentDivision, EventHandlers, ExitKind, Expr, FigurativeValue, GlobalVisibility,
    InitialState, InitializeFillerMode, InitializeValueMode, NegationMode, PerformControl,
    ProcedureDivision, ProgramAttributes, ProgramUnit, QualifiedName, Recursivity, RelOp,
    RoundedTarget, RoundingSpec, SignCheckKind, SizeErrorHandlers, Stmt, StopKind, SubscriptItem,
    TruthValue, UnaryOp, ValueAtom,
};

use crate::layout::{SolvedDataLayout, solve_data_division_with_preload};
use crate::m8_flow::{
    build_parent_place_with_subscripts, emit_flow_exit, emit_perform_control_loop,
    lower_alter_stmt, lower_evaluate_stmt, lower_sentence_list, lower_set_actions,
    resolve_perform_proc_bounds,
};
use crate::picture::PictureContext;
use crate::symbols::{ProcedureEntry, ResolvedSymbol, SymbolCandidate, SymbolTable};
use crate::typecheck::{
    EditedTargetPolicy, validate_arith_category, validate_move_compatibility,
    validate_subscripts_and_refmod,
};

/// Canonical names pre-interned once per compilation group for HIR synthesis.
const BUILTIN_SYMBOL_NAMES: &[&str] = &[
    "0",
    "1",
    "ACCEPT",
    "ADD",
    "CALL",
    "COMPUTE",
    "DISPLAY",
    "DIVIDE",
    "MOVE",
    "MULTIPLY",
    "SET",
    "SUBTRACT",
    "NUMERIC",
    "ALPHABETIC",
    "ALPHABETIC-LOWER",
    "ALPHABETIC-UPPER",
    "KANJI",
    "SET",
    "ACCEPT-SCREEN",
    "ALLOCATE",
    "ALTER",
    "CANCEL",
    "CLOSE",
    "COMMIT",
    "CONTINUE",
    "DELETE",
    "ENTRY",
    "EVALUATE",
    "EXHIBIT",
    "EXIT",
    "FREE",
    "GENERATE",
    "INITIALIZE",
    "INITIATE",
    "INSPECT",
    "INVOKE",
    "JSON-GENERATE",
    "JSON-PARSE",
    "MERGE",
    "NEXT-SENTENCE",
    "OPEN",
    "RAISE",
    "READ",
    "RELEASE",
    "RESUME",
    "RETURN",
    "REWRITE",
    "ROLLBACK",
    "SEARCH",
    "SORT-FILE",
    "SORT-TABLE",
    "START",
    "STRING",
    "SUPPRESS",
    "TERMINATE",
    "TRANSFORM",
    "UNLOCK",
    "UNSTRING",
    "VALIDATE",
    "WRITE",
    "XML-GENERATE",
    "XML-PARSE",
];

/// Pre-interned symbol handles used during infallible statement/expression lowering.
pub(crate) struct BuiltinSymbols {
    /// Fallback symbol for `"0"`.
    zero: SymbolId,
    /// Lookup table from canonical name to [`SymbolId`].
    by_name: BTreeMap<&'static str, SymbolId>,
}

impl BuiltinSymbols {
    /// Interns all required builtin names once into `interner`.
    ///
    /// # Arguments
    ///
    /// * `interner` - Mutable [`SymbolInterner`].
    fn new(interner: &mut SymbolInterner) -> Result<Self> {
        let mut by_name = BTreeMap::new();
        let mut zero = SymbolId::new(0);
        for &name in BUILTIN_SYMBOL_NAMES {
            let sym = interner.intern(name)?;
            if name == "0" {
                zero = sym;
            }
            by_name.insert(name, sym);
        }
        Ok(Self { zero, by_name })
    }

    /// Looks up a pre-interned [`SymbolId`] by static canonical key.
    ///
    /// # Arguments
    ///
    /// * `name` - Static canonical symbol key from [`BUILTIN_SYMBOL_NAMES`].
    #[must_use]
    fn get(&self, name: &'static str) -> SymbolId {
        self.by_name.get(name).copied().unwrap_or(self.zero)
    }
}

/// Distinguishes `FUNCTION-ID` units from `PROGRAM-ID` / `METHOD-ID` units in `lower_unit_parts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActiveUnitRole {
    /// `PROGRAM-ID` or OO `METHOD-ID`.
    ProgramOrMethod,
    /// `FUNCTION-ID` User-Defined Function.
    FunctionUnit,
}

/// Shared group-wide lowering environment passed across compilation units.
pub(crate) struct GroupLowerEnv<'a> {
    /// Monotonic counter for [`ProgramUnitId`].
    pub(crate) next_unit_id: u32,
    /// Active dialect configuration.
    pub(crate) dialect: &'a DialectConfig,
    /// Scoped `>>TURN` and `$SET BOUND`/`SSRANGE` directives across the compilation group.
    pub(crate) turn_directives: &'a [coblang_pp::TurnDirectiveRecord],
    /// Scoped `$SET DPC-IN-DATA` directives across the compilation group.
    pub(crate) dpc_directives: &'a [coblang_pp::DpcInDataRecord],
    /// Pre-scanned group-wide OO classes, interfaces, and functions.
    pub(crate) oo_catalog: crate::m20_xml_json_oo::OoGroupCatalog,
    /// Pre-validated `REPOSITORY` bindings when lowering an OO `METHOD-ID`.
    pub(crate) active_oo_repo: Option<crate::m20_xml_json_oo::OoRepoBindings>,
    /// Enclosing class/compartment metadata when lowering an OO `METHOD-ID`.
    pub(crate) active_oo_method: Option<crate::m20_xml_json_oo::OoActiveMethodEnv>,
    /// Pre-collected `FUNCTION-ID` signatures across the compilation group (`Milestone M12`).
    pub(crate) udf_sigs: BTreeMap<String, crate::intrinsics::UdfUnitSignature>,
    /// Pre-collected uppercase `PROGRAM-ID` external names across the compilation group.
    pub(crate) prog_keys: BTreeSet<String>,
    /// Active compilation unit kind (`ProgramOrMethod` vs `FunctionUnit`).
    pub(crate) unit_role: ActiveUnitRole,
    /// Per-unit `EC-*` exception declaratives indexed by `ProgramUnitId` for nested `GLOBAL` inheritance.
    pub(crate) ec_decl_by_unit: BTreeMap<ProgramUnitId, Vec<coblang_hir::HirEcDeclarativeRule>>,
    /// Per-unit M18 report catalogs indexed by `ProgramUnitId` for nested `IS GLOBAL` inheritance.
    pub(crate) report_cat_by_unit: BTreeMap<ProgramUnitId, crate::m18_report::UnitReportCatalog>,
    /// Accumulated Milestone M13 exception & NIST `DB` debug specifications.
    pub(crate) m13_units: Vec<coblang_hir::HirM13UnitSpec>,
    /// Multi-scope symbol table.
    pub(crate) symtab: SymbolTable,
    /// Pre-interned builtin symbols.
    pub(crate) builtins: BuiltinSymbols,
    /// Shared symbol interner.
    pub(crate) interner: &'a mut SymbolInterner,
    /// Compiler diagnostic sink.
    pub(crate) sink: &'a mut DiagnosticSink,
}

/// AST parts for a single program-like body (`PROGRAM-ID`, `FUNCTION-ID`, or `METHOD-ID`).
pub(crate) struct UnitAstSlice<'a> {
    /// Unit name symbol.
    pub(crate) name: SymbolId,
    /// `PROGRAM-ID` lifecycle attributes (`RECURSIVE`, `INITIAL`).
    pub(crate) attrs: ProgramAttributes,
    /// Environment division.
    pub(crate) env: &'a EnvironmentDivision,
    /// Data division.
    pub(crate) data: &'a DataDivision,
    /// Procedure division.
    pub(crate) proc_div: &'a ProcedureDivision,
    /// Unit span.
    pub(crate) span: SourceSpan,
}

/// Runs semantic analysis and lowers a [`CompilationGroup`] into a [`ResolvedHirModule`].
///
/// # Arguments
///
/// * `group` - Parsed AST compilation group.
/// * `dialect` - Active [`DialectConfig`].
/// * `interner` - Shared [`SymbolInterner`].
/// * `sink` - Diagnostic sink.
///
/// # Errors
///
/// Returns [`CoblangError::SemanticError`] if any lexical, syntactic, or semantic hard errors were recorded in `sink`.
pub fn lower_compilation_group(
    group: &CompilationGroup,
    dialect: &DialectConfig,
    interner: &mut SymbolInterner,
    sink: &mut DiagnosticSink,
) -> Result<ResolvedHirModule> {
    let builtins = BuiltinSymbols::new(interner)?;
    let oo_catalog = crate::m20_xml_json_oo::OoGroupCatalog::build(group, interner, sink);
    let udf_sigs = crate::intrinsics::collect_group_udf_signatures(group, dialect, interner, sink)?;
    let mut prog_keys = BTreeSet::new();
    for unit in &group.units {
        if let CompilationUnit::Program(pr) = unit {
            let pid = &pr.identification.program_id;
            let key = match pid.external_alias {
                Some(al) => interner
                    .resolve_original(al)
                    .unwrap_or("")
                    .to_ascii_uppercase(),
                None => interner
                    .resolve_canonical(pid.name)
                    .unwrap_or("")
                    .to_owned(),
            };
            let _ = prog_keys.insert(key);
        }
    }
    let mut env = GroupLowerEnv {
        next_unit_id: 0,
        dialect,
        turn_directives: &group.turn_directives,
        dpc_directives: &group.dpc_in_data_directives,
        oo_catalog,
        active_oo_repo: None,
        active_oo_method: None,
        udf_sigs,
        prog_keys,
        unit_role: ActiveUnitRole::ProgramOrMethod,
        ec_decl_by_unit: BTreeMap::new(),
        report_cat_by_unit: BTreeMap::new(),
        m13_units: Vec::new(),
        symtab: SymbolTable::default(),
        builtins,
        interner,
        sink,
    };
    let mut module = HirModule::default();

    for unit in &group.units {
        lower_top_unit(unit, &mut module, &mut env)?;
    }
    module.m13_units = env.m13_units;

    if env.sink.has_errors() {
        let first_message = env
            .sink
            .diagnostics()
            .iter()
            .find(|d| d.severity == coblang_common::DiagnosticSeverity::Error)
            .map_or_else(String::new, |d| d.message.clone());
        return Err(CoblangError::SemanticError {
            diagnostic_count: env.sink.error_count(),
            first_message,
        });
    }

    Ok(ResolvedHirModule(module))
}

/// Lowers one top-level [`CompilationUnit`] into `module`.
///
/// # Arguments
///
/// * `unit` - Top-level AST compilation unit.
/// * `module` - Output [`HirModule`].
/// * `env` - Group lowering environment.
fn lower_top_unit(
    unit: &CompilationUnit,
    module: &mut HirModule,
    env: &mut GroupLowerEnv<'_>,
) -> Result<()> {
    match unit {
        CompilationUnit::Program(prog) => {
            if prog.identification.program_id.attributes.prototype
                == coblang_syntax::ast::PrototypeKind::Concrete
            {
                lower_program_tree_into(prog, module, env)?;
            } else {
                let parts = UnitAstSlice {
                    name: prog.identification.program_id.name,
                    attrs: prog.identification.program_id.attributes,
                    env: &prog.environment,
                    data: &prog.data,
                    proc_div: &prog.procedure,
                    span: prog.span,
                };
                let _ = lower_unit_parts(&parts, None, env)?;
            }
        }
        CompilationUnit::Function(func) => {
            let parts = UnitAstSlice {
                name: func.function_id.name,
                attrs: ProgramAttributes {
                    common: CommonVisibility::PrivateToParent,
                    initial: InitialState::Resident,
                    recursive: Recursivity::Recursive,
                    prototype: func.function_id.prototype,
                },
                env: &func.environment,
                data: &func.data,
                proc_div: &func.procedure,
                span: func.span,
            };
            let ext_name = if let Some(alias) = func.function_id.external_alias {
                alias
            } else {
                let canon = env
                    .interner
                    .resolve_canonical(func.function_id.name)
                    .unwrap_or("")
                    .to_owned();
                env.interner.intern(&canon)?
            };
            env.unit_role = ActiveUnitRole::FunctionUnit;
            let body_res = lower_unit_parts(&parts, None, env);
            env.unit_role = ActiveUnitRole::ProgramOrMethod;
            let body = body_res?;
            if func.function_id.prototype == coblang_syntax::ast::PrototypeKind::Concrete {
                module.functions.push(HirFunction {
                    name: func.function_id.name,
                    external_name: ext_name,
                    body,
                });
            }
        }
        CompilationUnit::Class(cls) => {
            crate::m20_xml_json_oo::lower_class_unit(cls, module, env)?;
        }
        CompilationUnit::Interface(iface) => {
            crate::m20_xml_json_oo::lower_interface_unit(iface, module, env)?;
        }
    }
    Ok(())
}

/// Iteratively lowers a root `ProgramUnit` and all nested subprograms into `module.programs`.
///
/// # Arguments
///
/// * `root` - Top-level AST program unit.
/// * `module` - Output [`HirModule`].
/// * `env` - Group lowering environment.
fn lower_program_tree_into(
    root: &ProgramUnit,
    module: &mut HirModule,
    env: &mut GroupLowerEnv<'_>,
) -> Result<()> {
    let mut stack: Vec<(&ProgramUnit, Option<(usize, ScopeId)>)> = vec![(root, None)];
    let mut nodes: Vec<(HirProgram, Option<usize>)> = Vec::new();

    while let Some((cur, parent_info)) = stack.pop() {
        let parent_hir = parent_info.and_then(|(pi, _)| nodes.get(pi).map(|(h, _)| h));
        let is_common = cur.identification.program_id.attributes.common == CommonVisibility::Common;
        let parts = UnitAstSlice {
            name: cur.identification.program_id.name,
            attrs: cur.identification.program_id.attributes,
            env: &cur.environment,
            data: &cur.data,
            proc_div: &cur.procedure,
            span: cur.span,
        };
        let hir = lower_unit_parts(&parts, parent_hir, env)?;
        let my_scope = hir.scope;
        if let Some((_, p_scope)) = parent_info {
            let child_name = hir.name;
            env.symtab.scope_mut(p_scope).into_iter().for_each(|sc| {
                sc.child_programs.push((child_name, my_scope, is_common));
            });
        }
        let my_idx = nodes.len();
        nodes.push((hir, parent_info.map(|(pi, _)| pi)));
        for child in cur.nested_programs.iter().rev() {
            stack.push((child, Some((my_idx, my_scope))));
        }
    }

    let mut attached: BTreeMap<usize, Vec<HirProgram>> = BTreeMap::new();
    for (idx, (mut hir, parent_idx)) in nodes.into_iter().enumerate().rev() {
        if let Some(mut kids) = attached.remove(&idx) {
            kids.reverse();
            hir.nested = kids;
        }
        if let Some(pi) = parent_idx {
            attached.entry(pi).or_default().push(hir);
        } else {
            module.programs.push(hir);
        }
    }
    Ok(())
}

/// Core unit lowerer shared across `PROGRAM-ID`, `FUNCTION-ID`, and `METHOD-ID`.
///
/// Creates the lexical [`ScopeId`] and preloaded `IS GLOBAL` [`SolvedDataLayout`] for a unit.
///
/// # Arguments
///
/// * `parent_hir` - Optional enclosing parent [`HirProgram`].
/// * `symtab` - Shared [`SymbolTable`].
fn create_unit_scope_and_preload(
    parent_hir: Option<&HirProgram>,
    symtab: &mut SymbolTable,
) -> (ScopeId, SolvedDataLayout) {
    let (scope, mut preloaded) = if let Some(p_hir) = parent_hir {
        let (inh_scope, pre) =
            crate::m11_pointers::prepare_inherited_global_context(p_hir, p_hir.scope, symtab);
        (symtab.create_scope(Some(inh_scope)), pre)
    } else {
        (symtab.create_scope(None), SolvedDataLayout::default())
    };
    for it in &mut preloaded.items {
        it.scope = scope;
    }
    (scope, preloaded)
}

/// Lowers a single unit's divisions into a [`HirProgram`].
///
/// # Arguments
///
/// * `parts` - [`UnitAstSlice`] containing divisions.
/// * `parent_hir` - Optional enclosing parent [`HirProgram`] for nested `IS GLOBAL` inheritance.
/// * `env` - Group lowering environment.
pub(crate) fn lower_unit_parts(
    parts: &UnitAstSlice<'_>,
    parent_hir: Option<&HirProgram>,
    env: &mut GroupLowerEnv<'_>,
) -> Result<HirProgram> {
    let unit_id = ProgramUnitId::new(env.next_unit_id);
    env.next_unit_id = env.next_unit_id.saturating_add(1);
    let (scope, preloaded) = create_unit_scope_and_preload(parent_hir, &mut env.symtab);

    let sn_tables = crate::special_names_hir::build_special_names_tables_with_dialect(
        parts.env,
        env.dialect,
        env.interner,
    );
    register_special_names(
        parts.env,
        &sn_tables.alphabets,
        scope,
        &mut env.symtab,
        env.interner,
    );
    let pic_ctx = extract_picture_context(parts.env, env.interner);
    let mut layout = solve_data_division_with_preload(
        preloaded,
        parts.data,
        scope,
        pic_ctx,
        env.dialect,
        &mut env.symtab,
        env.interner,
        env.sink,
    )?;
    let mut items_for_same = layout.items.clone();
    let mut files = {
        let fenv = crate::m14_fileio::FileResolveEnv {
            unit_id,
            scope,
            layout: &layout,
            dialect: env.dialect,
            alphabets: &sn_tables.alphabets,
            prog_collating_seq: sn_tables.collating_sequence,
            symtab: &env.symtab,
            interner: env.interner,
        };
        crate::m14_fileio::resolve_unit_file_connectors(
            parts.env,
            parts.data,
            &fenv,
            &mut items_for_same,
            env.sink,
        )
    };
    layout.items = items_for_same;
    let section_ends = register_procedures(parts.proc_div, scope, &mut env.symtab);
    let normal_proc_cnt = u32::try_from(
        parts.proc_div.paragraphs.len()
            + parts
                .proc_div
                .sections
                .iter()
                .map(|s| 1 + s.paragraphs.len())
                .sum::<usize>(),
    )
    .unwrap_or(0);
    let fallthrough_limit_pid =
        (!parts.proc_div.declaratives.is_empty()).then_some(ProcedureId::new(normal_proc_cnt));
    let inherited_decls: &[coblang_hir::HirDeclarativeRule] =
        parent_hir.map_or(&[], |p| &p.declaratives);
    let declaratives = crate::m14_fileio::build_unit_declarative_rules(
        &parts.proc_div.declaratives,
        normal_proc_cnt,
        inherited_decls,
        &files,
        (unit_id, scope, &env.symtab, env.interner, env.sink),
    );

    let reports: Vec<(ReportId, SymbolId)> = parts
        .data
        .report_section
        .iter()
        .enumerate()
        .map(|(i, rd)| (ReportId::new(u32::try_from(i).unwrap_or(0)), rd.report_name))
        .collect();

    let unit_oo_repo = match &env.active_oo_repo {
        Some(r) => r.clone(),
        None => crate::m20_xml_json_oo::validate_and_bind_repository(
            parts.env,
            &env.oo_catalog,
            env.interner,
            env.sink,
        ),
    };
    let is_function_unit = env.unit_role == ActiveUnitRole::FunctionUnit;
    let repo_scope = crate::intrinsics::build_unit_repository_scope(
        &parts.env.configuration.repository,
        parts.name,
        is_function_unit,
        &parts.env.configuration.special_names,
        &env.udf_sigs,
        &env.prog_keys,
        env.dialect,
        env.interner,
        env.sink,
    )?;
    let inherited_report_cat = parent_hir.and_then(|p| env.report_cat_by_unit.get(&p.id));
    let report_catalog = crate::m18_report::build_unit_report_catalog(
        parts.data,
        parts.proc_div,
        normal_proc_cnt,
        inherited_report_cat,
        env.interner,
        env.sink,
    );
    let screen_catalog =
        crate::m19_screen::build_unit_screen_catalog(parts.env, parts.data, env.interner);

    let (report_plans, screen_plans, screen_special_regs, entry_statements, procedures) =
        lower_unit_body_with_ctx(
            parts,
            parent_hir,
            (unit_id, scope, normal_proc_cnt, pic_ctx),
            (&mut layout, &mut files, &declaratives, &section_ends),
            (&sn_tables, &repo_scope, &unit_oo_repo),
            (report_catalog, &screen_catalog),
            env,
        )?;

    Ok(finalize_hir_program(
        parts,
        (unit_id, scope, pic_ctx, fallthrough_limit_pid),
        sn_tables,
        layout,
        files,
        (
            reports,
            report_plans,
            screen_plans,
            screen_special_regs,
            entry_statements,
            procedures,
            declaratives,
        ),
        env,
    ))
}

/// Tuple bundling unit body output artifacts (`report_plans`, `screen_plans`, `screen_special_regs`, `entry_statements`, `procedures`).
type LoweredUnitBodyArtifacts = (
    Vec<coblang_hir::HirReportPlan>,
    Vec<coblang_hir::HirScreenPlan>,
    Option<coblang_hir::HirScreenSpecialRegs>,
    Vec<HirStmt>,
    Vec<HirParagraph>,
);

/// Lowers `REPORT`, `SCREEN`, `M13` exception rules, and `PROCEDURE DIVISION` inside [`UnitLowerCtx`].
///
/// # Arguments
///
/// * `parts` - Unit AST divisions.
/// * `parent_hir` - Optional enclosing parent program.
/// * `ids_and_pic` - `(unit_id, scope, normal_proc_cnt, pic_ctx)` tuple.
/// * `data_and_procs` - Mutable layout/files plus declaratives and section bounds.
/// * `tables_and_repos` - `SPECIAL-NAMES`, intrinsic repository, and OO repository bindings.
/// * `catalogs` - `(report_catalog, screen_catalog)` tuple.
/// * `env` - Group lowering environment.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if `M13` exception rule interning fails.
fn lower_unit_body_with_ctx(
    parts: &UnitAstSlice<'_>,
    parent_hir: Option<&HirProgram>,
    ids_and_pic: (ProgramUnitId, ScopeId, u32, PictureContext),
    data_and_procs: (
        &mut SolvedDataLayout,
        &mut [HirFileConnector],
        &[coblang_hir::HirDeclarativeRule],
        &BTreeMap<SymbolId, ProcedureId>,
    ),
    tables_and_repos: (
        &crate::special_names_hir::SpecialNamesTables,
        &crate::intrinsics::UnitRepositoryScope,
        &crate::m20_xml_json_oo::UnitOoRepository,
    ),
    catalogs: (
        crate::m18_report::UnitReportCatalog,
        &crate::m19_screen::UnitScreenCatalog,
    ),
    env: &mut GroupLowerEnv<'_>,
) -> Result<LoweredUnitBodyArtifacts> {
    let (unit_id, scope, normal_proc_cnt, pic_ctx) = ids_and_pic;
    let (layout, files, declaratives, section_ends) = data_and_procs;
    let (sn_tables, repo_scope, unit_oo_repo) = tables_and_repos;
    let (report_catalog, screen_catalog) = catalogs;
    let unit_orig_name = env
        .interner
        .resolve_original(parts.name)
        .unwrap_or("prog")
        .to_owned();
    let inherited_ec_decls = parent_hir
        .and_then(|p| env.ec_decl_by_unit.get(&p.id).cloned())
        .unwrap_or_default();
    let is_function_unit = env.unit_role == ActiveUnitRole::FunctionUnit;
    let (r_plans, s_plans, s_regs, mut es, mut ps, spec, dplan) = {
        let mut lctx = UnitLowerCtx {
            scope,
            unit_name: parts.name,
            unit_orig_name,
            is_function_unit,
            layout,
            files,
            declaratives,
            symtab: &env.symtab,
            builtins: &env.builtins,
            dialect: env.dialect,
            turn_directives: env.turn_directives,
            dpc_directives: env.dpc_directives,
            pic_ctx,
            decimal_point: pic_ctx.decimal_point,
            udf_sigs: &env.udf_sigs,
            repo_scope,
            oo_catalog: &env.oo_catalog,
            oo_repo: unit_oo_repo,
            oo_method: env.active_oo_method.as_ref(),
            report_catalog: &report_catalog,
            screen_catalog,
            alphabets: &sn_tables.alphabets,
            prog_collating_seq: sn_tables.collating_sequence,
            switches: &sn_tables.switches,
            device_map: &parts.env.configuration.special_names.devices,
            section_ends,
            normal_proc_cnt,
            decl_region: crate::m13_exceptions::DeclarativeRegion::NormalBody,
            active_proc_label: String::new(),
            unit_turns: Vec::new(),
            turn_cursor: 0,
            stmt_ec_policy: StmtExceptionCheckPolicy::None,
            interner: env.interner,
            sink: env.sink,
        };
        let r_plans =
            crate::m18_report::lower_unit_report_plans(parts.data, &report_catalog, &mut lctx);
        let (s_plans, s_regs) = crate::m19_screen::lower_unit_screen_plans(
            parts.env,
            parts.data,
            pic_ctx,
            screen_catalog,
            &mut lctx,
        );
        lctx.unit_turns = crate::m13_exceptions::collect_unit_turn_directives(
            env.turn_directives,
            parts.span,
            &mut lctx,
        );
        let (spec, dplan) = crate::m13_exceptions::build_m13_unit_rules(
            unit_id,
            parts.env.configuration.debugging_mode,
            parts.proc_div,
            normal_proc_cnt,
            &inherited_ec_decls,
            &mut lctx,
        )?;
        let any_cli_ec = env.dialect.bounds_checks.subscript
            != coblang_diag::BoundCheckToggle::Disabled
            || env.dialect.bounds_checks.refmod != coblang_diag::BoundCheckToggle::Disabled
            || env.dialect.bounds_checks.odo != coblang_diag::BoundCheckToggle::Disabled;
        if !spec.ec_declaratives.is_empty() || !lctx.unit_turns.is_empty() || any_cli_ec {
            lctx.stmt_ec_policy = StmtExceptionCheckPolicy::EmitChecks;
        }
        let (es, ps) = lower_procedure_division(parts.proc_div, &mut lctx);
        (r_plans, s_plans, s_regs, es, ps, spec, dplan)
    };
    crate::m13_exceptions::apply_nist_db_instrumentation(&mut es, &mut ps, normal_proc_cnt, &dplan);
    env.ec_decl_by_unit
        .insert(unit_id, spec.ec_declaratives.clone());
    env.report_cat_by_unit.insert(unit_id, report_catalog);
    env.m13_units.push(spec);
    Ok((r_plans, s_plans, s_regs, es, ps))
}

/// Tuple bundling lowered body/declarative/report/screen collections for [`finalize_hir_program`].
type FinalizeProgramCollections = (
    Vec<(ReportId, SymbolId)>,
    Vec<coblang_hir::HirReportPlan>,
    Vec<coblang_hir::HirScreenPlan>,
    Option<coblang_hir::HirScreenSpecialRegs>,
    Vec<HirStmt>,
    Vec<HirParagraph>,
    Vec<coblang_hir::HirDeclarativeRule>,
);

/// Assembles a completed [`HirProgram`] from solved divisions, procedures, and dialect flags.
///
/// # Arguments
///
/// * `parts` - Unit AST divisions.
/// * `meta` - `(unit_id, scope, pic_ctx, fallthrough_limit_pid)` tuple.
/// * `sn_tables` - Lowered `SPECIAL-NAMES` tables.
/// * `layout` - Solved `DATA DIVISION` layout.
/// * `files` - Resolved file connectors.
/// * `collections` - Lowered reports, screens, entry statements, procedures, and declaratives.
/// * `env` - Group lowering environment.
fn finalize_hir_program(
    parts: &UnitAstSlice<'_>,
    meta: (ProgramUnitId, ScopeId, PictureContext, Option<ProcedureId>),
    sn_tables: crate::special_names_hir::SpecialNamesTables,
    layout: SolvedDataLayout,
    files: Vec<HirFileConnector>,
    collections: FinalizeProgramCollections,
    env: &mut GroupLowerEnv<'_>,
) -> HirProgram {
    let (unit_id, scope, pic_ctx, fallthrough_limit_pid) = meta;
    let (
        reports,
        report_plans,
        screen_plans,
        screen_special_regs,
        entry_statements,
        procedures,
        declaratives,
    ) = collections;
    let formatting = build_program_formatting(pic_ctx, env.dialect);
    let perform_style = match env.dialect.perform_style {
        PerformReturnStyle::IsoStack => HirPerformStyle::IsoStack,
        PerformReturnStyle::OsVsContinuation => HirPerformStyle::OsVsContinuation,
    };
    let odo_slide = match env.dialect.odo_slide {
        OdoSlidePolicy::FixedMaxOffsets => HirOdoSlideMode::FixedOffsets,
        OdoSlidePolicy::SlideVariablyLocated => HirOdoSlideMode::SlideOffsets,
    };
    let recursivity = match parts.attrs.recursive {
        Recursivity::NonRecursive => HirRecursivity::NonRecursive,
        Recursivity::Recursive => HirRecursivity::Recursive,
    };
    let initial_state = match parts.attrs.initial {
        InitialState::Resident => HirInitialState::RetainStorage,
        InitialState::Initial => HirInitialState::ResetInitial,
    };
    let (using_params, returning_item) = resolve_proc_header_params(
        &parts.proc_div.header,
        &parts.data.linkage,
        parts.span,
        scope,
        &env.symtab,
        env.interner,
        env.sink,
    );
    HirProgram {
        id: unit_id,
        scope,
        name: parts.name,
        formatting,
        perform_style,
        odo_slide,
        recursivity,
        initial_state,
        alphabets: sn_tables.alphabets,
        collating_sequence: sn_tables.collating_sequence,
        custom_classes: sn_tables.custom_classes,
        switches: sn_tables.switches,
        files,
        data_items: layout.items,
        conditions_88: layout.conditions_88,
        reports,
        report_plans,
        screen_plans,
        screen_special_regs,
        using_params,
        returning_item,
        entry_statements,
        procedures,
        fallthrough_limit_pid,
        declaratives,
        nested: Vec::new(),
        span: parts.span,
    }
}

/// Resolves `PROCEDURE DIVISION USING / CHAINING / RETURNING` header symbols into [`coblang_hir::HirUsingParam`]s.
///
/// # Arguments
///
/// * `header` - Parsed [`coblang_syntax::ast::ProcedureHeader`].
/// * `linkage` - Parsed `LINKAGE SECTION` entries.
/// * `prog_span` - Enclosing program span.
/// * `scope` - Program lexical scope.
/// * `symtab` - Symbol table.
/// * `interner` - Symbol interner.
/// * `sink` - Diagnostic sink.
fn resolve_proc_header_params(
    header: &coblang_syntax::ast::ProcedureHeader,
    linkage: &[coblang_syntax::ast::DataEntry],
    prog_span: SourceSpan,
    scope: ScopeId,
    symtab: &SymbolTable,
    interner: &SymbolInterner,
    sink: &mut DiagnosticSink,
) -> (Vec<coblang_hir::HirUsingParam>, Option<DataItemId>) {
    use coblang_hir::{
        HirParamLengthMode, HirParamOptionality, HirParamSource, HirPassingMode, HirUsingParam,
    };
    use coblang_syntax::ast::{DataEntryName, LengthVariability, ParamOptionality, PassingMode};

    let any_len_syms: BTreeSet<SymbolId> = linkage
        .iter()
        .filter(|e| e.flags.length_mode == LengthVariability::AnyLength)
        .filter_map(|e| match e.name {
            DataEntryName::Named(s) => Some(s),
            DataEntryName::Filler => None,
        })
        .collect();

    let mut using_out =
        Vec::with_capacity(header.using_params.len() + header.chaining_params.len());
    for up in &header.using_params {
        if let Some(ResolvedSymbol::DataItem(item)) =
            symtab.resolve_symbol(scope, up.name, &[], up.span, interner, sink)
        {
            let mode = match up.mode {
                PassingMode::ByReference => HirPassingMode::ByReference,
                PassingMode::ByContent => HirPassingMode::ByContent,
                PassingMode::ByValue => HirPassingMode::ByValue,
            };
            let optionality = match up.optionality {
                ParamOptionality::Required => HirParamOptionality::Required,
                ParamOptionality::Optional => HirParamOptionality::Optional,
            };
            let length_mode = if any_len_syms.contains(&up.name) {
                HirParamLengthMode::AnyLength
            } else {
                HirParamLengthMode::Fixed
            };
            using_out.push(HirUsingParam {
                source: HirParamSource::Using,
                mode,
                optionality,
                length_mode,
                item,
                span: up.span,
            });
        }
    }
    for &ch_sym in &header.chaining_params {
        if let Some(ResolvedSymbol::DataItem(item)) =
            symtab.resolve_symbol(scope, ch_sym, &[], prog_span, interner, sink)
        {
            using_out.push(HirUsingParam {
                source: HirParamSource::Chaining,
                mode: HirPassingMode::ByReference,
                optionality: HirParamOptionality::Required,
                length_mode: HirParamLengthMode::Fixed,
                item,
                span: prog_span,
            });
        }
    }
    let ret_item = header.returning.and_then(|rsym| {
        if interner.resolve_canonical(rsym).ok() == Some("OMITTED") {
            return None;
        }
        match symtab.resolve_symbol(scope, rsym, &[], prog_span, interner, sink) {
            Some(ResolvedSymbol::DataItem(id)) => Some(id),
            _ => None,
        }
    });
    (using_out, ret_item)
}

/// Derives [`HirProgramFormatting`] from `SPECIAL-NAMES` `PictureContext` and `DialectConfig`.
///
/// # Arguments
///
/// * `pic_ctx` - Active `SPECIAL-NAMES` picture context.
/// * `dialect` - Active dialect configuration.
#[must_use]
fn build_program_formatting(
    pic_ctx: PictureContext,
    dialect: &DialectConfig,
) -> HirProgramFormatting {
    crate::m17_charset::build_m17_program_formatting(pic_ctx, dialect)
}

/// Lowers all sentences, paragraphs, sections, and declaratives inside `proc_div`.
///
/// # Arguments
///
/// * `proc_div` - AST procedure division.
/// * `lctx` - Unit lowering context.
fn lower_procedure_division(
    proc_div: &ProcedureDivision,
    lctx: &mut UnitLowerCtx<'_>,
) -> (Vec<HirStmt>, Vec<HirParagraph>) {
    lctx.decl_region = crate::m13_exceptions::DeclarativeRegion::NormalBody;
    lctx.active_proc_label.clear();
    let mut entry_statements = Vec::new();
    let proc_start_off = proc_div.span.map_or(0, |s| s.start.as_u32());
    lctx.flush_turns_up_to(proc_start_off, &mut entry_statements);
    entry_statements.extend(lower_sentence_list(&proc_div.initial_sentences, lctx));
    let mut procedures = Vec::new();
    let mut proc_idx = 0_u32;

    for para in &proc_div.paragraphs {
        let pid = ProcedureId::new(proc_idx);
        proc_idx += 1;
        lctx.active_proc_label = lctx
            .interner
            .resolve_original(para.name)
            .unwrap_or("")
            .to_owned();
        let mut statements = Vec::new();
        lctx.emit_lexical_turn_sync(para.span.start.as_u32(), &mut statements);
        statements.extend(lower_sentence_list(&para.sentences, lctx));
        procedures.push(HirParagraph {
            id: pid,
            name: para.name,
            kind: HirProcKind::Paragraph,
            section_end: pid,
            statements,
            span: para.span,
        });
    }

    for sec in &proc_div.sections {
        let sec_pid = ProcedureId::new(proc_idx);
        let para_count = u32::try_from(sec.paragraphs.len()).unwrap_or(0);
        let sec_last_pid = ProcedureId::new(proc_idx + para_count);
        proc_idx += 1;
        let sec_label = lctx
            .interner
            .resolve_original(sec.name)
            .unwrap_or("")
            .to_owned();
        lctx.active_proc_label.clone_from(&sec_label);
        let mut sec_stmts = Vec::new();
        lctx.emit_lexical_turn_sync(sec.span.start.as_u32(), &mut sec_stmts);
        sec_stmts.extend(lower_sentence_list(&sec.initial_sentences, lctx));
        procedures.push(HirParagraph {
            id: sec_pid,
            name: sec.name,
            kind: HirProcKind::SectionHeader,
            section_end: sec_last_pid,
            statements: sec_stmts,
            span: sec.span,
        });
        for para in &sec.paragraphs {
            let pid = ProcedureId::new(proc_idx);
            proc_idx += 1;
            let p_orig = lctx.interner.resolve_original(para.name).unwrap_or("");
            lctx.active_proc_label = format!("{p_orig} OF {sec_label}");
            let mut statements = Vec::new();
            lctx.emit_lexical_turn_sync(para.span.start.as_u32(), &mut statements);
            statements.extend(lower_sentence_list(&para.sentences, lctx));
            procedures.push(HirParagraph {
                id: pid,
                name: para.name,
                kind: HirProcKind::Paragraph,
                section_end: sec_last_pid,
                statements,
                span: para.span,
            });
        }
    }

    lower_declarative_sections(&proc_div.declaratives, &mut proc_idx, &mut procedures, lctx);

    (entry_statements, procedures)
}

/// Lowers `DECLARATIVES` sections and their subordinate paragraphs into [`HirParagraph`] entries.
///
/// # Arguments
///
/// * `declaratives` - Parsed `DECLARATIVES` sections.
/// * `proc_idx` - Monotonic `ProcedureId` counter.
/// * `procedures` - Output [`HirParagraph`] accumulator.
/// * `lctx` - Unit lowering context.
fn lower_declarative_sections(
    declaratives: &[coblang_syntax::ast::DeclarativeSection],
    proc_idx: &mut u32,
    procedures: &mut Vec<HirParagraph>,
    lctx: &mut UnitLowerCtx<'_>,
) {
    lctx.decl_region = crate::m13_exceptions::DeclarativeRegion::InDeclaratives;
    for decl in declaratives {
        let decl_pid = ProcedureId::new(*proc_idx);
        *proc_idx += 1;
        let decl_label = lctx
            .interner
            .resolve_original(decl.name)
            .unwrap_or("")
            .to_owned();
        lctx.active_proc_label.clone_from(&decl_label);
        let mut decl_stmts = Vec::new();
        lctx.emit_lexical_turn_sync(decl.span.start.as_u32(), &mut decl_stmts);
        decl_stmts.extend(lower_sentence_list(&decl.initial_sentences, lctx));
        let para_count = u32::try_from(decl.paragraphs.len()).unwrap_or(0);
        let decl_last_pid = ProcedureId::new(decl_pid.as_u32() + para_count);
        procedures.push(HirParagraph {
            id: decl_pid,
            name: decl.name,
            kind: HirProcKind::SectionHeader,
            section_end: decl_last_pid,
            statements: decl_stmts,
            span: decl.span,
        });
        for para in &decl.paragraphs {
            let pid = ProcedureId::new(*proc_idx);
            *proc_idx += 1;
            let p_orig = lctx.interner.resolve_original(para.name).unwrap_or("");
            lctx.active_proc_label = format!("{p_orig} OF {decl_label}");
            let mut statements = Vec::new();
            lctx.emit_lexical_turn_sync(para.span.start.as_u32(), &mut statements);
            statements.extend(lower_sentence_list(&para.sentences, lctx));
            procedures.push(HirParagraph {
                id: pid,
                name: para.name,
                kind: HirProcKind::Paragraph,
                section_end: decl_last_pid,
                statements,
                span: para.span,
            });
        }
    }
}

/// Registers `SPECIAL-NAMES` alphabets, classes, symbolic characters, switches, and devices in `symtab`.
///
/// # Arguments
///
/// * `env` - AST environment division.
/// * `alphabets` - Compiled `SPECIAL-NAMES` alphabet tables.
/// * `scope` - Owning scope.
/// * `symtab` - Symbol table.
/// * `interner` - Symbol interner.
fn register_special_names(
    env: &EnvironmentDivision,
    alphabets: &[HirAlphabetTable],
    scope: ScopeId,
    symtab: &mut SymbolTable,
    interner: &SymbolInterner,
) {
    let sn = &env.configuration.special_names;
    symtab.scope_mut(scope).into_iter().for_each(|sc| {
        for cls in &sn.classes {
            sc.custom_classes.insert(cls.name);
        }
        for alpha in &sn.alphabets {
            sc.alphabets.insert(alpha.name);
        }
        for dev in &sn.devices {
            sc.device_mnemonics.insert(dev.mnemonic);
        }
    });
    for sw in &sn.switches {
        let sys = interner.resolve_canonical(sw.system_name).unwrap_or("");
        let switch_index = crate::special_names_hir::parse_switch_index(sys);
        for (opt_sym, expected) in [
            (sw.on_condition, HirTruthValue::True),
            (sw.off_condition, HirTruthValue::False),
            (sw.mnemonic, HirTruthValue::True),
        ] {
            if let Some(cond_sym) = opt_sym {
                symtab.insert_symbol(
                    scope,
                    cond_sym,
                    SymbolCandidate {
                        target: ResolvedSymbol::SwitchCondition {
                            switch_index,
                            expected,
                        },
                        qualifiers: Vec::new(),
                        visibility: GlobalVisibility::Local,
                    },
                );
            }
        }
    }
    for sym_ch in &sn.symbolic_chars {
        for (nm, ord_sym) in sym_ch.names.iter().zip(&sym_ch.ordinals) {
            let ord_one_based = interner
                .resolve_original(*ord_sym)
                .ok()
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(1);
            let ord_u8 = crate::m17_charset::resolve_symbolic_char_byte(
                ord_one_based,
                sym_ch.alphabet,
                alphabets,
            );
            symtab.insert_symbol(
                scope,
                *nm,
                SymbolCandidate {
                    target: ResolvedSymbol::Constant(HirConstValue::Bytes(vec![ord_u8])),
                    qualifiers: Vec::new(),
                    visibility: GlobalVisibility::Local,
                },
            );
        }
    }
}

/// Extracts [`PictureContext`] (`DECIMAL-POINT` & `CURRENCY`) from `SPECIAL-NAMES`.
///
/// # Arguments
///
/// * `env` - AST environment division.
/// * `interner` - Symbol interner.
#[must_use]
fn extract_picture_context(env: &EnvironmentDivision, interner: &SymbolInterner) -> PictureContext {
    let sn = &env.configuration.special_names;
    let currency_symbol = sn
        .currency_signs
        .first()
        .and_then(|cs| cs.picture_symbol.or(Some(cs.sign_literal)))
        .and_then(|sym| interner.resolve_original(sym).ok())
        .and_then(|s| s.chars().next())
        .unwrap_or('$');
    PictureContext {
        decimal_point: sn.decimal_point,
        currency_symbol,
    }
}

/// Registers all paragraphs and sections in `proc_div` into `symtab` in exact sequential `ProcedureId` order,
/// returning a parallel `Vec<ProcedureId>` mapping each `ProcedureId` to its `PERFORM`-expanded end procedure
/// (`section_end` when naming a `SECTION`, or `id` itself when naming a `Paragraph`).
///
/// # Arguments
///
/// * `proc_div` - AST procedure division.
/// * `scope` - Owning scope.
/// * `symtab` - Symbol table.
fn register_procedures(
    proc_div: &ProcedureDivision,
    scope: ScopeId,
    symtab: &mut SymbolTable,
) -> Vec<ProcedureId> {
    let mut section_ends = Vec::new();
    symtab.scope_mut(scope).into_iter().for_each(|sc| {
        let mut pid = 0_u32;
        for para in &proc_div.paragraphs {
            let id = ProcedureId::new(pid);
            sc.procedures.push(ProcedureEntry {
                id,
                name: para.name,
                section: None,
            });
            section_ends.push(id);
            pid += 1;
        }
        for sec in &proc_div.sections {
            let sec_id = ProcedureId::new(pid);
            let para_cnt = u32::try_from(sec.paragraphs.len()).unwrap_or(0);
            let sec_end = ProcedureId::new(pid + para_cnt);
            sc.procedures.push(ProcedureEntry {
                id: sec_id,
                name: sec.name,
                section: None,
            });
            section_ends.push(sec_end);
            pid += 1;
            for para in &sec.paragraphs {
                let id = ProcedureId::new(pid);
                sc.procedures.push(ProcedureEntry {
                    id,
                    name: para.name,
                    section: Some(sec.name),
                });
                section_ends.push(id);
                pid += 1;
            }
        }
        for decl in &proc_div.declaratives {
            let decl_id = ProcedureId::new(pid);
            let para_cnt = u32::try_from(decl.paragraphs.len()).unwrap_or(0);
            let decl_end = ProcedureId::new(pid + para_cnt);
            sc.procedures.push(ProcedureEntry {
                id: decl_id,
                name: decl.name,
                section: None,
            });
            section_ends.push(decl_end);
            pid += 1;
            for para in &decl.paragraphs {
                let id = ProcedureId::new(pid);
                sc.procedures.push(ProcedureEntry {
                    id,
                    name: para.name,
                    section: Some(decl.name),
                });
                section_ends.push(id);
                pid += 1;
            }
        }
    });
    section_ends
}

/// Controls whether `lower_one_stmt` appends post-statement `HirStmt::CheckException` nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StmtExceptionCheckPolicy {
    /// Unit has no `EC-*` declaratives, `>>TURN` directives, or CLI `-fec=` flags.
    None,
    /// Emit `CheckException` after elementary statements capable of raising `EC-*`.
    EmitChecks,
}

/// Shared unit-lowering context.
pub(crate) struct UnitLowerCtx<'a> {
    /// Active program scope.
    pub(crate) scope: ScopeId,
    /// Unit `PROGRAM-ID` / `FUNCTION-ID` / `METHOD-ID` symbol.
    pub(crate) unit_name: SymbolId,
    /// Original spelling of `unit_name` for `EXCEPTION-LOCATION`.
    pub(crate) unit_orig_name: String,
    /// `true` when lowering a `FUNCTION-ID` unit.
    pub(crate) is_function_unit: bool,
    /// Solved Data Division layout (mutable to allow compiler-synthesized temporaries).
    pub(crate) layout: &'a mut SolvedDataLayout,
    /// Resolved unit file connectors (mutable for `WRITE ... ADVANCING` print-mode promotion).
    pub(crate) files: &'a mut [HirFileConnector],
    /// Active local and inherited `GLOBAL` file `DECLARATIVES` rules.
    pub(crate) declaratives: &'a [coblang_hir::HirDeclarativeRule],
    /// Symbol table.
    pub(crate) symtab: &'a SymbolTable,
    /// Pre-interned builtin symbols.
    builtins: &'a BuiltinSymbols,
    /// Active dialect configuration.
    pub(crate) dialect: &'a DialectConfig,
    /// Scoped `>>TURN` and `$SET BOUND`/`SSRANGE` directives.
    pub(crate) turn_directives: &'a [coblang_pp::TurnDirectiveRecord],
    /// Scoped `$SET DPC-IN-DATA` directives.
    pub(crate) dpc_directives: &'a [coblang_pp::DpcInDataRecord],
    /// Active `SPECIAL-NAMES` picture context.
    pub(crate) pic_ctx: PictureContext,
    /// Active `SPECIAL-NAMES. DECIMAL-POINT` mode.
    pub(crate) decimal_point: DecimalPointKind,
    /// Group-wide `FUNCTION-ID` signature table (`Milestone M12`).
    pub(crate) udf_sigs: &'a BTreeMap<String, crate::intrinsics::UdfUnitSignature>,
    /// Unit `REPOSITORY` intrinsic/UDF resolution scope (`Milestone M12`).
    pub(crate) repo_scope: &'a crate::intrinsics::UnitRepositoryScope,
    /// Group-wide OO classes, interfaces, and functions catalog.
    pub(crate) oo_catalog: &'a crate::m20_xml_json_oo::OoGroupCatalog,
    /// Unit `REPOSITORY` class/interface bindings.
    pub(crate) oo_repo: &'a crate::m20_xml_json_oo::OoRepoBindings,
    /// Enclosing `METHOD-ID` context if inside an OO method.
    pub(crate) oo_method: Option<&'a crate::m20_xml_json_oo::OoActiveMethodEnv>,
    /// Unit M18 `REPORT SECTION` catalog.
    pub(crate) report_catalog: &'a crate::m18_report::UnitReportCatalog,
    /// Unit M19 `SCREEN SECTION` catalog.
    pub(crate) screen_catalog: &'a crate::m19_screen::UnitScreenCatalog,
    /// Compiled `SPECIAL-NAMES. ALPHABET` tables.
    pub(crate) alphabets: &'a [HirAlphabetTable],
    /// Optional `PROGRAM COLLATING SEQUENCE` index into `alphabets`.
    pub(crate) prog_collating_seq: Option<usize>,
    /// Compiled `SPECIAL-NAMES` switches.
    pub(crate) switches: &'a [HirSwitchSpec],
    /// Declared `SPECIAL-NAMES` device mnemonic aliases.
    pub(crate) device_map: &'a [DeviceMnemonicDecl],
    /// `PERFORM`-expanded end `ProcedureId` per registered procedure.
    pub(crate) section_ends: &'a [ProcedureId],
    /// Exclusive upper bound of non-declarative `ProcedureId`s (`Milestone M13`).
    pub(crate) normal_proc_cnt: u32,
    /// Whether currently lowering `DECLARATIVES` vs normal procedure body (`Milestone M13`).
    pub(crate) decl_region: crate::m13_exceptions::DeclarativeRegion,
    /// Active paragraph/section label (`""`, `"PARA"`, or `"PARA OF SEC"`) for `EXCEPTION-LOCATION`.
    pub(crate) active_proc_label: String,
    /// Unit-scoped `(byte_offset, HirSetTurnStmt)` directives in source order (`Milestone M13`).
    pub(crate) unit_turns: Vec<(u32, coblang_hir::HirSetTurnStmt)>,
    /// Next unemitted index into `unit_turns`.
    pub(crate) turn_cursor: usize,
    /// Whether to emit post-statement `HirStmt::CheckException` (`Milestone M13`).
    pub(crate) stmt_ec_policy: StmtExceptionCheckPolicy,
    /// Symbol interner (mutable for dynamic table count literal interning).
    pub(crate) interner: &'a mut SymbolInterner,
    /// Diagnostic sink.
    pub(crate) sink: &'a mut DiagnosticSink,
}

impl UnitLowerCtx<'_> {
    /// Looks up a pre-interned builtin [`SymbolId`] by static canonical key.
    ///
    /// # Arguments
    ///
    /// * `name` - Static canonical symbol key.
    #[must_use]
    pub(crate) fn builtins_get(&self, name: &'static str) -> SymbolId {
        self.builtins.get(name)
    }

    /// Interns `"<unit_orig_name>; <active_proc_label>"` for `EXCEPTION-LOCATION` formatting.
    pub(crate) fn current_location_prefix_sym(&mut self) -> SymbolId {
        let prefix = format!("{}; {}", self.unit_orig_name, self.active_proc_label);
        self.interner
            .intern(&prefix)
            .unwrap_or_else(|_| self.builtins.get("0"))
    }

    /// Emits any pending `unit_turns` with `byte_offset <= limit_offset` into `out`.
    ///
    /// # Arguments
    ///
    /// * `limit_offset` - Inclusive source byte offset.
    /// * `out` - Output [`HirStmt`] vector.
    pub(crate) fn flush_turns_up_to(&mut self, limit_offset: u32, out: &mut Vec<HirStmt>) {
        while let Some((off, stmt)) = self.unit_turns.get(self.turn_cursor) {
            if *off <= limit_offset {
                out.push(HirStmt::SetTurn(stmt.clone()));
                self.turn_cursor += 1;
            } else {
                break;
            }
        }
    }

    /// Re-synchronizes lexical `>>TURN` state at paragraph/section entry when multiple `>>TURN`
    /// directives exist in the unit (so out-of-order `PERFORM` paragraphs observe lexical `>>TURN` state).
    ///
    /// # Arguments
    ///
    /// * `entry_offset` - Paragraph/section header byte offset.
    /// * `out` - Output [`HirStmt`] vector.
    pub(crate) fn emit_lexical_turn_sync(&mut self, entry_offset: u32, out: &mut Vec<HirStmt>) {
        self.flush_turns_up_to(entry_offset, out);
        if self.unit_turns.len() <= 1 {
            return;
        }
        for (off, turn_stmt) in &self.unit_turns {
            if *off <= entry_offset {
                out.push(HirStmt::SetTurn(turn_stmt.clone()));
            } else {
                break;
            }
        }
    }
}

/// Borrowed AST components of an `ADD` statement.
struct AddParts<'a> {
    /// Standard vs `CORRESPONDING`.
    mode: CorrespondingMode,
    /// Summand expressions.
    operands: &'a [Expr],
    /// Optional `TO b GIVING` middle operand.
    to_giving_mid: Option<&'a Expr>,
    /// In-place `TO` targets.
    to_targets: &'a [RoundedTarget],
    /// `GIVING` targets.
    giving_targets: &'a [RoundedTarget],
    /// Size error handlers.
    size_error: &'a SizeErrorHandlers,
    /// Statement span.
    span: SourceSpan,
}

/// Borrowed AST components of a `SUBTRACT` statement.
struct SubtractParts<'a> {
    /// Standard vs `CORRESPONDING`.
    mode: CorrespondingMode,
    /// Subtrahend expressions.
    subtrahends: &'a [Expr],
    /// Optional `FROM b GIVING` minuend operand.
    from_giving_minuend: Option<&'a Expr>,
    /// In-place `FROM` targets.
    from_targets: &'a [RoundedTarget],
    /// `GIVING` targets.
    giving_targets: &'a [RoundedTarget],
    /// Size error handlers.
    size_error: &'a SizeErrorHandlers,
    /// Statement span.
    span: SourceSpan,
}

/// Borrowed AST components of a `MULTIPLY` statement.
struct MultiplyParts<'a> {
    /// Multiplicand expression.
    lhs: &'a Expr,
    /// Optional `BY b GIVING` multiplier expression.
    rhs: Option<&'a Expr>,
    /// In-place `BY` targets.
    by_targets: &'a [RoundedTarget],
    /// `GIVING` targets.
    giving_targets: &'a [RoundedTarget],
    /// Size error handlers.
    size_error: &'a SizeErrorHandlers,
    /// Statement span.
    span: SourceSpan,
}

/// Borrowed AST components of a `DIVIDE` statement.
struct DivideParts<'a> {
    /// `INTO` vs `BY` orientation.
    orientation: DivideOrientation,
    /// First operand expression.
    lhs: &'a Expr,
    /// Optional second operand expression (`GIVING` forms).
    rhs: Option<&'a Expr>,
    /// In-place `INTO` targets.
    into_targets: &'a [RoundedTarget],
    /// `GIVING` targets.
    giving_targets: &'a [RoundedTarget],
    /// Optional `REMAINDER` destination.
    remainder: Option<&'a DataRef>,
    /// Size error handlers.
    size_error: &'a SizeErrorHandlers,
    /// Statement span.
    span: SourceSpan,
}

/// Lowers a slice of AST [`Stmt`]s into `out`.
///
/// # Arguments
///
/// * `stmts` - AST statement list.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_stmt_slice(stmts: &[Stmt], out: &mut Vec<HirStmt>, ctx: &mut UnitLowerCtx<'_>) {
    for stmt in stmts {
        lower_one_stmt(stmt, out, ctx);
    }
}

/// Exhaustively lowers and typechecks a single AST [`Stmt`], appending desugared [`HirStmt`]s to `out`.
///
/// # Arguments
///
/// * `stmt` - AST statement.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_one_stmt(stmt: &Stmt, out: &mut Vec<HirStmt>, ctx: &mut UnitLowerCtx<'_>) {
    let stmt_sp = stmt.span();
    ctx.flush_turns_up_to(stmt_sp.start.as_u32(), out);
    lower_arith_or_move_partition(stmt, out, ctx);
    lower_flow_or_display_partition(stmt, out, ctx);
    lower_checked_high_partition_a(stmt, out, ctx);
    lower_checked_high_partition_b(stmt, out, ctx);
    lower_pure_high_partition(stmt, out, ctx);
    if ctx.stmt_ec_policy == StmtExceptionCheckPolicy::EmitChecks
        && let Some(verb_key) = stmt_ec_check_verb_key(stmt)
    {
        let location_sym = ctx.current_location_prefix_sym();
        let statement_sym = ctx.builtins_get(verb_key);
        out.push(HirStmt::CheckException(
            coblang_hir::HirCheckExceptionStmt {
                location_sym,
                statement_sym,
                span: stmt_sp,
            },
        ));
    }
}

/// Returns the canonical verb key when `stmt` is an elementary action capable of raising a runtime `EC-*`.
///
/// # Arguments
///
/// * `stmt` - AST [`Stmt`].
#[must_use]
fn stmt_ec_check_verb_key(stmt: &Stmt) -> Option<&'static str> {
    match stmt {
        Stmt::Move { .. } => Some("MOVE"),
        Stmt::Compute { size_error, .. }
            if size_error.on_size_error.is_empty() && size_error.not_on_size_error.is_empty() =>
        {
            Some("COMPUTE")
        }
        Stmt::Add { size_error, .. }
            if size_error.on_size_error.is_empty() && size_error.not_on_size_error.is_empty() =>
        {
            Some("ADD")
        }
        Stmt::Subtract { size_error, .. }
            if size_error.on_size_error.is_empty() && size_error.not_on_size_error.is_empty() =>
        {
            Some("SUBTRACT")
        }
        Stmt::Multiply { size_error, .. }
            if size_error.on_size_error.is_empty() && size_error.not_on_size_error.is_empty() =>
        {
            Some("MULTIPLY")
        }
        Stmt::Divide { size_error, .. }
            if size_error.on_size_error.is_empty() && size_error.not_on_size_error.is_empty() =>
        {
            Some("DIVIDE")
        }
        Stmt::Display { .. } => Some("DISPLAY"),
        Stmt::Accept { .. } => Some("ACCEPT"),
        Stmt::Call { handlers, .. }
            if handlers.on_event.is_empty() && handlers.not_on_event.is_empty() =>
        {
            Some("CALL")
        }
        Stmt::StringVerb { handlers, .. }
            if handlers.on_event.is_empty() && handlers.not_on_event.is_empty() =>
        {
            Some("STRING")
        }
        Stmt::Unstring { handlers, .. }
            if handlers.on_event.is_empty() && handlers.not_on_event.is_empty() =>
        {
            Some("UNSTRING")
        }
        Stmt::Inspect { .. } => Some("INSPECT"),
        Stmt::Set { .. } => Some("SET"),
        Stmt::Allocate { .. } => Some("ALLOCATE"),
        Stmt::Free { .. } => Some("FREE"),
        _ => None,
    }
}

/// Lowers arithmetic (`ADD`, `SUBTRACT`, `MULTIPLY`, `DIVIDE`, `COMPUTE`), `MOVE`, and `SET` statements.
///
/// # Arguments
///
/// * `stmt` - AST statement.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_arith_or_move_partition(stmt: &Stmt, out: &mut Vec<HirStmt>, ctx: &mut UnitLowerCtx<'_>) {
    match stmt {
        Stmt::Add {
            mode,
            operands,
            to_giving_mid,
            to_targets,
            giving_targets,
            size_error,
            span,
        } => lower_add_stmt(
            &AddParts {
                mode: *mode,
                operands,
                to_giving_mid: to_giving_mid.as_ref(),
                to_targets,
                giving_targets,
                size_error,
                span: *span,
            },
            out,
            ctx,
        ),
        Stmt::Subtract {
            mode,
            subtrahends,
            from_giving_minuend,
            from_targets,
            giving_targets,
            size_error,
            span,
        } => lower_subtract_stmt(
            &SubtractParts {
                mode: *mode,
                subtrahends,
                from_giving_minuend: from_giving_minuend.as_ref(),
                from_targets,
                giving_targets,
                size_error,
                span: *span,
            },
            out,
            ctx,
        ),
        Stmt::Multiply {
            lhs,
            rhs,
            by_targets,
            giving_targets,
            size_error,
            span,
        } => lower_multiply_stmt(
            &MultiplyParts {
                lhs,
                rhs: rhs.as_ref(),
                by_targets,
                giving_targets,
                size_error,
                span: *span,
            },
            out,
            ctx,
        ),
        Stmt::Divide {
            orientation,
            lhs,
            rhs,
            into_targets,
            giving_targets,
            remainder,
            size_error,
            span,
        } => lower_divide_stmt(
            &DivideParts {
                orientation: *orientation,
                lhs,
                rhs: rhs.as_ref(),
                into_targets,
                giving_targets,
                remainder: remainder.as_ref(),
                size_error,
                span: *span,
            },
            out,
            ctx,
        ),
        Stmt::Compute {
            targets,
            expr,
            size_error,
            span,
        } => lower_compute_stmt(targets, expr, size_error, *span, out, ctx),
        Stmt::Move {
            mode,
            source,
            targets,
            span,
        } => lower_move_stmt(*mode, source, targets, *span, out, ctx),
        Stmt::Set { actions, span } => lower_set_stmt(actions, *span, out, ctx),
        _ => {}
    }
}

/// Lowers `DISPLAY`, `IF`, `PERFORM`, `GO TO`, `CALL`, `GOBACK`, and `STOP` statements.
///
/// # Arguments
///
/// * `stmt` - AST statement.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_flow_or_display_partition(
    stmt: &Stmt,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    match stmt {
        Stmt::Display {
            operands,
            upon,
            advancing,
            screen_ext,
            handlers,
            span,
        } => {
            let p_ctx = ctx.pic_ctx;
            let s_cat = ctx.screen_catalog;
            if !crate::m19_screen::try_lower_m19_display_stmt(
                operands,
                *upon,
                *advancing,
                screen_ext.as_deref(),
                handlers,
                *span,
                p_ctx,
                s_cat,
                out,
                ctx,
            ) {
                lower_display_stmt(operands, *upon, *advancing, handlers, *span, out, ctx);
            }
        }
        Stmt::If {
            condition,
            then_body,
            else_body,
            span,
        } => lower_if_stmt(condition, then_body, else_body, *span, out, ctx),
        Stmt::PerformOutofLine {
            start,
            thru,
            control,
            span,
        } => lower_perform_out_of_line(start, thru.as_ref(), control, *span, out, ctx),
        Stmt::PerformInline {
            control,
            body,
            span,
        } => lower_perform_inline(control, body, *span, out, ctx),
        Stmt::GoTo {
            targets,
            depending_on,
            span,
        } => lower_goto_stmt(targets, depending_on.as_ref(), *span, out, ctx),
        Stmt::Call {
            callee,
            args,
            returning,
            handlers,
            span,
        } => lower_call_stmt((callee, args, returning), handlers, *span, out, ctx),
        Stmt::Goback { returning, span } => lower_goback_stmt(returning.as_ref(), *span, out, ctx),
        Stmt::Stop { kind, span } => lower_stop_stmt(kind, *span, out, ctx),
        _ => {}
    }
}

/// Lowers reference-validated high-level verbs (`ACCEPT` through `INSPECT`).
///
/// # Arguments
///
/// * `stmt` - AST statement.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_checked_high_partition_a(stmt: &Stmt, out: &mut Vec<HirStmt>, ctx: &mut UnitLowerCtx<'_>) {
    match stmt {
        Stmt::Accept {
            target,
            source,
            screen_ext,
            handlers,
            span,
        } => {
            let p_ctx = ctx.pic_ctx;
            let s_cat = ctx.screen_catalog;
            if !crate::m19_screen::try_lower_m19_accept_stmt(
                target,
                source,
                screen_ext.as_deref(),
                handlers,
                *span,
                p_ctx,
                s_cat,
                out,
                ctx,
            ) {
                crate::m10_strings::lower_accept_stmt(target, source, handlers, *span, out, ctx);
            }
        }
        Stmt::AcceptOmitted(spec) => {
            let p_ctx = ctx.pic_ctx;
            crate::m19_screen::lower_m19_accept_omitted(spec, p_ctx, out, ctx);
        }
        Stmt::Allocate {
            target,
            unit,
            initialized,
            returning,
            span,
        } => {
            crate::m11_pointers::lower_allocate_stmt(
                target,
                *unit,
                initialized,
                returning.as_ref(),
                *span,
                out,
                ctx,
            );
        }
        Stmt::Alter { pairs, span } => {
            lower_alter_stmt(pairs, *span, out, ctx);
        }
        Stmt::Cancel { targets, span } => {
            crate::m11_pointers::lower_cancel_stmt(targets, *span, out, ctx);
        }
        Stmt::Evaluate {
            subjects,
            branches,
            when_other,
            span,
        } => {
            lower_evaluate_stmt(subjects, branches, when_other, *span, out, ctx);
        }
        Stmt::Exhibit { operands, span, .. } => {
            check_expr_list(operands, ctx);
            push_high_op("EXHIBIT", *span, out, ctx);
        }
        Stmt::Free { targets, span } => {
            crate::m11_pointers::lower_free_stmt(targets, *span, out, ctx);
        }
        Stmt::Initialize {
            targets,
            filler,
            value_mode,
            replacing,
            span,
        } => {
            lower_initialize_stmt(targets, *filler, *value_mode, replacing, *span, out, ctx);
        }
        Stmt::Inspect {
            target,
            operations,
            span,
        } => {
            crate::m10_strings::lower_inspect_stmt(target, operations, *span, out, ctx);
        }
        _ => {}
    }
}

/// Lowers reference-validated I/O, search, and string high-level verbs (`READ` through `WRITE`).
///
/// # Arguments
///
/// * `stmt` - AST statement.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_checked_high_partition_b(stmt: &Stmt, out: &mut Vec<HirStmt>, ctx: &mut UnitLowerCtx<'_>) {
    match stmt {
        Stmt::Read {
            file,
            direction,
            into,
            key,
            lock_mode,
            handlers,
            span,
        } => crate::m14_fileio::lower_read_stmt(
            &crate::m14_fileio::ReadStmtParts {
                file: *file,
                direction: *direction,
                into: into.as_ref(),
                key: key.as_ref(),
                lock_mode: *lock_mode,
                handlers,
                span: *span,
            },
            out,
            ctx,
        ),
        Stmt::Rewrite {
            record,
            target_mode,
            from,
            handlers,
            span,
        } => crate::m14_fileio::lower_rewrite_stmt(
            record,
            *target_mode,
            from.as_ref(),
            handlers,
            *span,
            out,
            ctx,
        ),
        Stmt::Search {
            mode,
            table,
            varying,
            at_end,
            when_arms,
            span,
        } => crate::m9_tables::lower_search_stmt(
            &crate::m9_tables::SearchStmtParts {
                mode: *mode,
                table,
                varying: varying.as_ref(),
                at_end,
                when_arms,
                span: *span,
            },
            out,
            ctx,
        ),
        Stmt::StringVerb {
            groups,
            into,
            pointer,
            handlers,
            span,
        } => {
            crate::m10_strings::lower_string_stmt(
                groups,
                into,
                pointer.as_ref(),
                handlers,
                *span,
                out,
                ctx,
            );
        }
        _ => lower_checked_high_partition_c(stmt, out, ctx),
    }
}

/// Lowers `UNSTRING`, `VALIDATE`, and `WRITE` statements.
///
/// # Arguments
///
/// * `stmt` - AST statement.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_checked_high_partition_c(stmt: &Stmt, out: &mut Vec<HirStmt>, ctx: &mut UnitLowerCtx<'_>) {
    match stmt {
        Stmt::Unstring {
            source,
            delimiters,
            targets,
            pointer,
            tallying,
            handlers,
            span,
        } => {
            crate::m10_strings::lower_unstring_stmt(
                &crate::m10_strings::UnstringStmtParts {
                    source,
                    delimiters,
                    targets,
                    pointer: pointer.as_ref(),
                    tallying: tallying.as_ref(),
                    handlers,
                    span: *span,
                },
                out,
                ctx,
            );
        }
        Stmt::Validate { targets, span } => {
            check_place_list(targets, ctx);
            push_high_op("VALIDATE", *span, out, ctx);
        }
        Stmt::Write {
            record,
            target_mode,
            from,
            advancing,
            handler_kind,
            handlers,
            span,
        } => crate::m14_fileio::lower_write_stmt(
            &crate::m14_fileio::WriteStmtParts {
                record,
                target_mode: *target_mode,
                from: from.as_ref(),
                advancing: advancing.as_ref(),
                handler_kind: *handler_kind,
                handlers,
                span: *span,
            },
            out,
            ctx,
        ),
        _ => {}
    }
}

/// Lowers pure tag-stamped M8–M20 high-level verbs (`CLOSE` through `XML-PARSE`).
///
/// # Arguments
///
/// * `stmt` - AST statement.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_pure_high_partition(stmt: &Stmt, out: &mut Vec<HirStmt>, ctx: &mut UnitLowerCtx<'_>) {
    match stmt {
        Stmt::Close {
            files,
            dispositions,
            span,
        } => crate::m14_fileio::lower_close_stmt(files, dispositions, *span, out, ctx),
        Stmt::Commit { span } => push_high_op("COMMIT", *span, out, ctx),
        Stmt::Continue {
            after_seconds: Some(sec_expr),
            span,
        } => {
            let _ = lower_expr_with_cat(sec_expr, ctx);
            push_high_op("CONTINUE", *span, out, ctx);
        }
        Stmt::Delete {
            files,
            kind,
            handlers,
            span,
        } => crate::m14_fileio::lower_delete_stmt(files, *kind, handlers, *span, out, ctx),
        Stmt::Entry { span, .. } => push_high_op("ENTRY", *span, out, ctx),
        Stmt::Exit { kind, span } => lower_exit_stmt(kind, *span, out, ctx),
        Stmt::Generate { target, span } => {
            let r_cat = ctx.report_catalog;
            crate::m18_report::lower_generate_stmt(*target, *span, r_cat, out, ctx);
        }
        Stmt::Initiate { reports, span } => {
            let r_cat = ctx.report_catalog;
            crate::m18_report::lower_initiate_stmt(reports, *span, r_cat, out, ctx);
        }
        Stmt::Invoke {
            receiver,
            method,
            args,
            returning,
            span,
        } => crate::m20_xml_json_oo::lower_invoke_stmt(
            receiver,
            method,
            args,
            returning.as_ref(),
            *span,
            out,
            ctx,
        ),
        Stmt::JsonGenerate {
            destination,
            source,
            count_in,
            name_mappings,
            suppress_clauses,
            converting_clauses,
            handlers,
            span,
        } => crate::m20_xml_json_oo::lower_json_generate_stmt(
            &crate::m20_xml_json_oo::JsonGenParts {
                destination,
                source,
                count_in: count_in.as_ref(),
                name_mappings,
                suppress_clauses,
                converting_clauses,
                handlers,
                span: *span,
            },
            out,
            ctx,
        ),
        _ => lower_pure_high_partition_b(stmt, out, ctx),
    }
}

/// Lowers `EXIT` variants (`EXIT PROGRAM`, `EXIT FUNCTION`, `EXIT METHOD`, `EXIT PARAGRAPH/SECTION/PERFORM`).
///
/// # Arguments
///
/// * `kind` - Parsed [`ExitKind`].
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_exit_stmt(
    kind: &ExitKind,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    match kind {
        ExitKind::PlainEndpoint => {}
        ExitKind::Program(ret_opt) => out.push(HirStmt::Return {
            mode: HirReturnMode::ExitProgram,
            code: ret_opt.as_ref().map(|r| lower_expr_with_cat(r, ctx).0),
            span,
        }),
        ExitKind::Function | ExitKind::Method => out.push(HirStmt::Return {
            mode: HirReturnMode::Goback,
            code: None,
            span,
        }),
        ExitKind::Paragraph => emit_flow_exit(HirFlowExitKind::ExitParagraph, span, out),
        ExitKind::Section => emit_flow_exit(HirFlowExitKind::ExitSection, span, out),
        ExitKind::PerformBreak => emit_flow_exit(HirFlowExitKind::ExitPerformBreak, span, out),
        ExitKind::PerformCycle => emit_flow_exit(HirFlowExitKind::ExitPerformCycle, span, out),
    }
}

/// Lowers `JSON-PARSE` through `SORT-TABLE` high-level verbs.
///
/// # Arguments
///
/// * `stmt` - AST statement.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_pure_high_partition_b(stmt: &Stmt, out: &mut Vec<HirStmt>, ctx: &mut UnitLowerCtx<'_>) {
    match stmt {
        Stmt::JsonParse {
            source,
            destination,
            detail,
            name_mappings,
            suppress_clauses,
            converting_clauses,
            handlers,
            span,
        } => crate::m20_xml_json_oo::lower_json_parse_stmt(
            &crate::m20_xml_json_oo::JsonParseParts {
                source,
                destination,
                detail: *detail,
                name_mappings,
                suppress_clauses,
                converting_clauses,
                handlers,
                span: *span,
            },
            out,
            ctx,
        ),
        Stmt::Merge {
            sd_file,
            keys,
            collating_sequence,
            using_files,
            giving_files,
            output_proc,
            span,
        } => crate::m14_fileio::lower_merge_file_stmt(
            &crate::m14_fileio::MergeFileParts {
                sd_file: *sd_file,
                keys,
                collating_sequence: *collating_sequence,
                using_files,
                giving_files,
                output_proc: output_proc.as_ref(),
                span: *span,
            },
            out,
            ctx,
        ),
        Stmt::NextSentence { span } => emit_flow_exit(HirFlowExitKind::NextSentence, *span, out),
        Stmt::Open {
            entries,
            options,
            span,
        } => crate::m14_fileio::lower_open_stmt(entries, options, *span, out, ctx),
        Stmt::Raise { target, span } => {
            crate::m13_exceptions::lower_raise_stmt(target.as_ref(), *span, out, ctx);
        }
        Stmt::Release { record, from, span } => {
            crate::m14_fileio::lower_release_stmt(record, from.as_ref(), *span, out, ctx);
        }
        Stmt::Resume { target, span } => {
            crate::m13_exceptions::lower_resume_stmt(
                target.as_ref(),
                ctx.decl_region,
                ctx.normal_proc_cnt,
                *span,
                out,
                ctx,
            );
        }
        Stmt::Return {
            sd_file,
            into,
            handlers,
            span,
        } => {
            crate::m14_fileio::lower_return_stmt(
                *sd_file,
                into.as_ref(),
                handlers,
                *span,
                out,
                ctx,
            );
        }
        Stmt::Rollback { span } => push_high_op("ROLLBACK", *span, out, ctx),
        _ => lower_pure_high_partition_c(stmt, out, ctx),
    }
}

/// Lowers `SORT` through `XML-PARSE` high-level verbs.
///
/// # Arguments
///
/// * `stmt` - AST statement.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_pure_high_partition_c(stmt: &Stmt, out: &mut Vec<HirStmt>, ctx: &mut UnitLowerCtx<'_>) {
    match stmt {
        Stmt::SortFile {
            sd_file,
            keys,
            duplicates_in_order,
            collating_sequence,
            using_files,
            input_proc,
            giving_files,
            output_proc,
            span,
        } => crate::m14_fileio::lower_sort_file_stmt(
            &crate::m14_fileio::SortFileParts {
                sd_file: *sd_file,
                keys,
                duplicates_in_order: *duplicates_in_order,
                collating_sequence: *collating_sequence,
                using_files,
                input_proc: input_proc.as_ref(),
                giving_files,
                output_proc: output_proc.as_ref(),
                span: *span,
            },
            out,
            ctx,
        ),
        Stmt::SortTable {
            table,
            keys,
            collating_sequence,
            span,
            ..
        } => crate::m9_tables::lower_sort_table_stmt(
            table,
            keys,
            *collating_sequence,
            *span,
            out,
            ctx,
        ),
        Stmt::Start {
            file,
            key_rel,
            position,
            key_length,
            handlers,
            span,
        } => crate::m14_fileio::lower_start_stmt(
            &crate::m14_fileio::StartStmtParts {
                file: *file,
                key_rel: key_rel.as_ref(),
                position: *position,
                key_length: key_length.as_ref(),
                handlers,
                span: *span,
            },
            out,
            ctx,
        ),
        Stmt::Suppress { span } => {
            crate::m18_report::lower_suppress_stmt(*span, out, ctx);
        }
        Stmt::Terminate { reports, span } => {
            let r_cat = ctx.report_catalog;
            crate::m18_report::lower_terminate_stmt(reports, *span, r_cat, out, ctx);
        }
        Stmt::Transform {
            target,
            from,
            to,
            span,
        } => crate::m10_strings::lower_transform_stmt(target, from, to, *span, out, ctx),
        Stmt::Unlock { file, span } => {
            crate::m14_fileio::lower_unlock_stmt(*file, *span, out, ctx);
        }
        Stmt::XmlGenerate {
            destination,
            source,
            count_in,
            encoding,
            xml_declaration,
            attributes,
            namespace,
            namespace_prefix,
            name_mappings,
            type_mappings,
            suppress_clauses,
            handlers,
            span,
        } => crate::m20_xml_json_oo::lower_xml_generate_stmt(
            &crate::m20_xml_json_oo::XmlGenParts {
                destination,
                source,
                count_in: count_in.as_ref(),
                encoding: encoding.as_deref(),
                xml_declaration: *xml_declaration,
                attributes: *attributes,
                namespace: namespace.as_deref(),
                namespace_prefix: namespace_prefix.as_deref(),
                name_mappings,
                type_mappings,
                suppress_clauses,
                handlers,
                span: *span,
            },
            out,
            ctx,
        ),
        Stmt::XmlParse {
            source,
            encoding,
            validating,
            processing_proc,
            handlers,
            span,
            ..
        } => crate::m20_xml_json_oo::lower_xml_parse_stmt(
            &crate::m20_xml_json_oo::XmlParseParts {
                source,
                encoding: encoding.as_ref(),
                validating: validating.as_ref(),
                processing_proc,
                handlers,
                span: *span,
            },
            out,
            ctx,
        ),
        _ => {}
    }
}

/// Appends a [`HirStmt::HighLevelOp`] with pre-interned `tag`.
///
/// # Arguments
///
/// * `tag` - Canonical builtin verb key.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn push_high_op(
    tag: &'static str,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &UnitLowerCtx<'_>,
) {
    out.push(HirStmt::HighLevelOp {
        verb: ctx.builtins.get(tag),
        span,
    });
}

/// Lowers `MOVE` and `MOVE CORRESPONDING` statements.
///
/// # Arguments
///
/// * `mode` - `Standard` vs `Corresponding`.
/// * `source` - Source expression.
/// * `targets` - Destination references.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_move_stmt(
    mode: CorrespondingMode,
    source: &Expr,
    targets: &[DataRef],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    if mode == CorrespondingMode::Corresponding {
        lower_move_corresponding(source, targets, span, out, ctx);
        return;
    }
    let (src_hir, src_cat) = lower_expr_with_cat(source, ctx);
    for dst_ref in targets {
        if let Some(dst_place) = resolve_place(dst_ref, ctx) {
            let dst_cat = dst_place.effective_ty.category();
            validate_move_compatibility(mode, source, src_cat, dst_cat, span, ctx.sink);
            out.push(HirStmt::Move {
                src: src_hir.clone(),
                dst: dst_place,
                span,
            });
        }
    }
}

/// Lowers `DISPLAY` statements.
///
/// # Arguments
///
/// * `operands` - Displayed operand expressions.
/// * `upon` - Optional `UPON` device/mnemonic symbol.
/// * `advancing` - Line advancing mode.
/// * `handlers` - Exception handlers.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_display_stmt(
    operands: &[Expr],
    upon: Option<SymbolId>,
    advancing: DisplayAdvancing,
    handlers: &EventHandlers,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let ops: Vec<HirExpr> = operands
        .iter()
        .map(|op| {
            let (e, cat) = lower_expr_with_cat(op, ctx);
            if cat == DataCategory::ObjectReference {
                crate::m20_xml_json_oo::emit_invalid_display_object_ref(op, ctx);
            }
            e
        })
        .collect();
    let _ = lower_event_handlers(handlers, ctx);
    let mut device = HirDisplayDevice::Stdout;
    if let Some(dev_sym) = upon {
        let resolved_sym = ctx
            .device_map
            .iter()
            .find(|d| d.mnemonic == dev_sym)
            .map_or(dev_sym, |d| d.system_device);
        let dev_name = ctx.interner.resolve_canonical(resolved_sym).unwrap_or("");
        if let Some(env_target) = match dev_name {
            "ENVIRONMENT-NAME" => Some(HirEnvTargetKind::EnvironmentName),
            "ENVIRONMENT-VALUE" => Some(HirEnvTargetKind::EnvironmentValue),
            "ARGUMENT-NUMBER" => Some(HirEnvTargetKind::ArgumentNumber),
            "COMMAND-LINE" => Some(HirEnvTargetKind::CommandLine),
            _ => None,
        } {
            out.push(HirStmt::DisplayEnv {
                operands: ops,
                target: env_target,
                span,
            });
            return;
        }
        if matches!(dev_name, "SYSERR" | "STDERR" | "SYSPUNCH") {
            device = HirDisplayDevice::Stderr;
        }
    }
    out.push(HirStmt::Display {
        operands: ops,
        device,
        advancing: match advancing {
            DisplayAdvancing::AdvanceLine => HirAdvancing::AdvanceLine,
            DisplayAdvancing::NoAdvancing => HirAdvancing::NoAdvancing,
        },
        span,
    });
}

/// Lowers `IF ... THEN ... ELSE ...` statements.
///
/// # Arguments
///
/// * `condition` - Guard condition.
/// * `then_body` - True branch statements.
/// * `else_body` - False branch statements.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_if_stmt(
    condition: &Condition,
    then_body: &[Stmt],
    else_body: &[Stmt],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let cond = lower_condition(condition, ctx);
    let mut tb = Vec::new();
    let mut eb = Vec::new();
    lower_stmt_slice(then_body, &mut tb, ctx);
    lower_stmt_slice(else_body, &mut eb, ctx);
    out.push(HirStmt::If {
        cond,
        then_body: tb,
        else_body: eb,
        span,
    });
}

/// Lowers out-of-line `PERFORM p1 [THRU p2]` statements.
///
/// # Arguments
///
/// * `start` - Start procedure reference.
/// * `thru` - Optional end procedure reference.
/// * `control` - Loop control specification.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_perform_out_of_line(
    start: &QualifiedName,
    thru: Option<&QualifiedName>,
    control: &PerformControl,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    if let Some((sp_id, ep_id)) = resolve_perform_proc_bounds(start, thru, ctx) {
        let range_stmt = HirStmt::PerformRange {
            start_proc: sp_id,
            end_proc: ep_id,
            span,
        };
        emit_perform_control_loop(control, vec![range_stmt], span, out, ctx);
    } else {
        emit_perform_control_loop(control, Vec::new(), span, out, ctx);
    }
}

/// Lowers inline `PERFORM ... END-PERFORM` loops.
///
/// # Arguments
///
/// * `control` - Loop control specification.
/// * `body` - Loop body statements.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_perform_inline(
    control: &PerformControl,
    body: &[Stmt],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut b = Vec::new();
    lower_stmt_slice(body, &mut b, ctx);
    emit_perform_control_loop(control, b, span, out, ctx);
}

/// Lowers `GO TO` and `GO TO ... DEPENDING ON` statements.
///
/// # Arguments
///
/// * `targets` - Target procedure references.
/// * `depending_on` - Optional dispatch index expression.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_goto_stmt(
    targets: &[QualifiedName],
    depending_on: Option<&Expr>,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let mut pids = Vec::with_capacity(targets.len());
    for t in targets {
        if let Some(pid) = ctx.symtab.resolve_procedure(
            ctx.scope,
            t.base,
            t.qualifiers.first().copied(),
            t.span,
            ctx.interner,
            ctx.sink,
        ) {
            crate::m13_exceptions::validate_declarative_proc_ref(
                ctx.decl_region,
                crate::m13_exceptions::ProcRefTransferKind::GoToOrAlter,
                pid,
                t.base,
                ctx.normal_proc_cnt,
                t.span,
                ctx,
            );
            pids.push(pid);
        }
    }
    let dep = depending_on.map(|d| lower_expr_with_cat(d, ctx).0);
    out.push(HirStmt::Jump {
        targets: pids,
        depending_on: dep,
        span,
    });
}

/// Lowers `CALL` statements.
///
/// # Arguments
///
/// * `call_sig` - `(callee, args, returning)` tuple.
/// * `handlers` - `ON EXCEPTION` / `NOT ON EXCEPTION` handlers.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_call_stmt(
    call_sig: (&Expr, &[CallArg], &coblang_syntax::ast::CallReturning),
    handlers: &EventHandlers,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    crate::m11_pointers::lower_m11_call_stmt(call_sig, handlers, span, out, ctx);
}

/// Lowers `GOBACK` statements.
///
/// # Arguments
///
/// * `returning` - Optional returning expression.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_goback_stmt(
    returning: Option<&Expr>,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let code = returning.map(|r| lower_expr_with_cat(r, ctx).0);
    out.push(HirStmt::Return {
        mode: HirReturnMode::Goback,
        code,
        span,
    });
}

/// Lowers `STOP` statements.
///
/// # Arguments
///
/// * `kind` - `STOP RUN` vs `STOP literal`.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_stop_stmt(
    kind: &StopKind,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let code = match kind {
        StopKind::Run(Some(e)) | StopKind::Literal(e) => Some(lower_expr_with_cat(e, ctx).0),
        StopKind::Run(None) => None,
    };
    out.push(HirStmt::Return {
        mode: HirReturnMode::StopRun,
        code,
        span,
    });
}

/// Validates and lowers `SET` statements.
///
/// # Arguments
///
/// * `actions` - `SET` sub-operations.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_set_stmt(
    actions: &[coblang_syntax::ast::SetAction],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    lower_set_actions(actions, span, out, ctx);
}

/// Lowers a `COMPUTE` statement.
///
/// # Arguments
///
/// * `targets` - Rounded destination list.
/// * `expr` - Right-hand arithmetic expression.
/// * `size_error` - Size error handlers.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_compute_stmt(
    targets: &[RoundedTarget],
    expr: &Expr,
    size_error: &SizeErrorHandlers,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let (e_hir, _) = lower_expr_with_cat(expr, ctx);
    let tlist = resolve_giving_targets(targets, ctx);
    let (on_se, not_se) = lower_size_error(size_error, ctx);
    out.push(HirStmt::AssignArith {
        targets: tlist,
        expr: e_hir,
        on_size_error: on_se,
        not_on_size_error: not_se,
        span,
    });
}

/// Lowers an `ADD` statement.
///
/// # Arguments
///
/// * `parts` - Borrowed `ADD` AST parts.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_add_stmt(parts: &AddParts<'_>, out: &mut Vec<HirStmt>, ctx: &mut UnitLowerCtx<'_>) {
    if parts.mode == CorrespondingMode::Corresponding {
        lower_arith_corresponding(
            parts.operands,
            parts.to_targets,
            HirBinOp::Add,
            parts.span,
            out,
            ctx,
        );
        return;
    }
    let sum_expr = fold_arith_operands(parts.operands, HirBinOp::Add, parts.span, ctx);
    if parts.giving_targets.is_empty() {
        emit_inplace_arith_targets(
            parts.to_targets,
            &sum_expr,
            HirBinOp::Add,
            parts.size_error,
            parts.span,
            out,
            ctx,
        );
    } else {
        let total = if let Some(mid) = parts.to_giving_mid {
            let (m_hir, m_cat) = lower_expr_with_cat(mid, ctx);
            validate_arith_category(
                m_cat,
                EditedTargetPolicy::StrictNumeric,
                mid.span(),
                ctx.sink,
            );
            HirExpr::Binary {
                op: HirBinOp::Add,
                lhs: Box::new(sum_expr),
                rhs: Box::new(m_hir),
                span: parts.span,
            }
        } else {
            sum_expr
        };
        let tlist = resolve_giving_targets(parts.giving_targets, ctx);
        let (on_se, not_se) = lower_size_error(parts.size_error, ctx);
        out.push(HirStmt::AssignArith {
            targets: tlist,
            expr: total,
            on_size_error: on_se,
            not_on_size_error: not_se,
            span: parts.span,
        });
    }
}

/// Lowers a `SUBTRACT` statement.
///
/// # Arguments
///
/// * `parts` - Borrowed `SUBTRACT` AST parts.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_subtract_stmt(
    parts: &SubtractParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    if parts.mode == CorrespondingMode::Corresponding {
        lower_arith_corresponding(
            parts.subtrahends,
            parts.from_targets,
            HirBinOp::Sub,
            parts.span,
            out,
            ctx,
        );
        return;
    }
    let sub_sum = fold_arith_operands(parts.subtrahends, HirBinOp::Add, parts.span, ctx);
    if parts.giving_targets.is_empty() {
        emit_inplace_arith_targets(
            parts.from_targets,
            &sub_sum,
            HirBinOp::Sub,
            parts.size_error,
            parts.span,
            out,
            ctx,
        );
    } else if let Some(minuend) = parts.from_giving_minuend {
        let (m_hir, m_cat) = lower_expr_with_cat(minuend, ctx);
        validate_arith_category(
            m_cat,
            EditedTargetPolicy::StrictNumeric,
            minuend.span(),
            ctx.sink,
        );
        let tlist = resolve_giving_targets(parts.giving_targets, ctx);
        let (on_se, not_se) = lower_size_error(parts.size_error, ctx);
        out.push(HirStmt::AssignArith {
            targets: tlist,
            expr: HirExpr::Binary {
                op: HirBinOp::Sub,
                lhs: Box::new(m_hir),
                rhs: Box::new(sub_sum),
                span: parts.span,
            },
            on_size_error: on_se,
            not_on_size_error: not_se,
            span: parts.span,
        });
    }
}

/// Lowers a `MULTIPLY` statement.
///
/// # Arguments
///
/// * `parts` - Borrowed `MULTIPLY` AST parts.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_multiply_stmt(
    parts: &MultiplyParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let (l_hir, l_cat) = lower_expr_with_cat(parts.lhs, ctx);
    validate_arith_category(
        l_cat,
        EditedTargetPolicy::StrictNumeric,
        parts.lhs.span(),
        ctx.sink,
    );
    if parts.giving_targets.is_empty() {
        emit_inplace_arith_targets(
            parts.by_targets,
            &l_hir,
            HirBinOp::Mul,
            parts.size_error,
            parts.span,
            out,
            ctx,
        );
    } else if let Some(r_ast) = parts.rhs {
        let (r_hir, r_cat) = lower_expr_with_cat(r_ast, ctx);
        validate_arith_category(
            r_cat,
            EditedTargetPolicy::StrictNumeric,
            r_ast.span(),
            ctx.sink,
        );
        let tlist = resolve_giving_targets(parts.giving_targets, ctx);
        let (on_se, not_se) = lower_size_error(parts.size_error, ctx);
        out.push(HirStmt::AssignArith {
            targets: tlist,
            expr: HirExpr::Binary {
                op: HirBinOp::Mul,
                lhs: Box::new(l_hir),
                rhs: Box::new(r_hir),
                span: parts.span,
            },
            on_size_error: on_se,
            not_on_size_error: not_se,
            span: parts.span,
        });
    }
}

/// Lowers a `DIVIDE` statement.
///
/// # Arguments
///
/// * `parts` - Borrowed `DIVIDE` AST parts.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_divide_stmt(parts: &DivideParts<'_>, out: &mut Vec<HirStmt>, ctx: &mut UnitLowerCtx<'_>) {
    let (l_hir, l_cat) = lower_expr_with_cat(parts.lhs, ctx);
    validate_arith_category(
        l_cat,
        EditedTargetPolicy::StrictNumeric,
        parts.lhs.span(),
        ctx.sink,
    );
    let rem_place = parts.remainder.and_then(|rem_ref| {
        let rem_pl = resolve_place(rem_ref, ctx)?;
        validate_arith_category(
            rem_pl.effective_ty.category(),
            EditedTargetPolicy::AllowNumericEdited,
            rem_ref.span,
            ctx.sink,
        );
        Some(rem_pl)
    });
    if parts.giving_targets.is_empty() {
        if let (Some(rem_pl), [only_into]) = (rem_place, parts.into_targets)
            && let Some(q_pl) = resolve_place(&only_into.target, ctx)
        {
            validate_arith_category(
                q_pl.effective_ty.category(),
                EditedTargetPolicy::StrictNumeric,
                only_into.target.span,
                ctx.sink,
            );
            let (on_se, not_se) = lower_size_error(parts.size_error, ctx);
            out.push(HirStmt::DivideRemainder(Box::new(
                coblang_hir::HirDivideRemainderSpec {
                    dividend: HirExpr::Place(q_pl.clone()),
                    divisor: l_hir,
                    quotient: (q_pl, map_rounding(only_into.rounding, ctx.interner)),
                    remainder: rem_pl,
                    on_size_error: on_se,
                    not_on_size_error: not_se,
                    span: parts.span,
                },
            )));
            return;
        }
        emit_inplace_arith_targets(
            parts.into_targets,
            &l_hir,
            HirBinOp::Div,
            parts.size_error,
            parts.span,
            out,
            ctx,
        );
    } else if let Some(r_ast) = parts.rhs {
        let (r_hir, r_cat) = lower_expr_with_cat(r_ast, ctx);
        validate_arith_category(
            r_cat,
            EditedTargetPolicy::StrictNumeric,
            r_ast.span(),
            ctx.sink,
        );
        let (dividend, divisor) = match parts.orientation {
            DivideOrientation::By => (l_hir, r_hir),
            DivideOrientation::Into => (r_hir, l_hir),
        };
        let tlist = resolve_giving_targets(parts.giving_targets, ctx);
        let (on_se, not_se) = lower_size_error(parts.size_error, ctx);
        if let (Some(rem_pl), [(q_pl, q_rnd)]) = (rem_place, tlist.as_slice()) {
            out.push(HirStmt::DivideRemainder(Box::new(
                coblang_hir::HirDivideRemainderSpec {
                    dividend,
                    divisor,
                    quotient: (q_pl.clone(), *q_rnd),
                    remainder: rem_pl,
                    on_size_error: on_se,
                    not_on_size_error: not_se,
                    span: parts.span,
                },
            )));
            return;
        }
        out.push(HirStmt::AssignArith {
            targets: tlist,
            expr: HirExpr::Binary {
                op: HirBinOp::Div,
                lhs: Box::new(dividend),
                rhs: Box::new(divisor),
                span: parts.span,
            },
            on_size_error: on_se,
            not_on_size_error: not_se,
            span: parts.span,
        });
    }
}

/// Emits in-place `dst = dst op rhs_expr` [`HirStmt::AssignArith`] statements for each target in `targets`.
///
/// # Arguments
///
/// * `targets` - In-place destination targets.
/// * `rhs_expr` - Right-hand operand expression.
/// * `op` - Binary operator (`Add`, `Sub`, `Mul`, `Div`).
/// * `size_error` - Size error handlers.
/// * `span` - Statement span.
/// * `out` - Output statement accumulator.
/// * `ctx` - Unit lowering context.
fn emit_inplace_arith_targets(
    targets: &[RoundedTarget],
    rhs_expr: &HirExpr,
    op: HirBinOp,
    size_error: &SizeErrorHandlers,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let has_se = !size_error.on_size_error.is_empty() || !size_error.not_on_size_error.is_empty();
    let mut resolved_targets = Vec::with_capacity(targets.len());
    for rt in targets {
        if let Some(pl) = resolve_place(&rt.target, ctx) {
            validate_arith_category(
                pl.effective_ty.category(),
                EditedTargetPolicy::StrictNumeric,
                rt.target.span,
                ctx.sink,
            );
            resolved_targets.push((pl, map_rounding(rt.rounding, ctx.interner)));
        }
    }
    if resolved_targets.len() > 1 && has_se {
        let latch_pl = crate::m8_flow::alloc_i64_counter_place(span, ctx);
        let zero_expr = crate::m8_flow::const_int_expr("0", span, ctx);
        let one_expr = crate::m8_flow::const_int_expr("1", span, ctx);
        out.push(HirStmt::Move {
            src: zero_expr.clone(),
            dst: latch_pl.clone(),
            span,
        });
        for (pl, rnd) in resolved_targets {
            let combined = HirExpr::Binary {
                op,
                lhs: Box::new(HirExpr::Place(pl.clone())),
                rhs: Box::new(rhs_expr.clone()),
                span,
            };
            out.push(HirStmt::AssignArith {
                targets: vec![(pl, rnd)],
                expr: combined,
                on_size_error: vec![HirStmt::Move {
                    src: one_expr.clone(),
                    dst: latch_pl.clone(),
                    span,
                }],
                not_on_size_error: Vec::new(),
                span,
            });
        }
        let (on_se, not_se) = lower_size_error(size_error, ctx);
        out.push(HirStmt::If {
            cond: HirCondition::Compare {
                op: HirRelOp::Ne,
                lhs: HirExpr::Place(latch_pl),
                rhs: zero_expr,
            },
            then_body: on_se,
            else_body: not_se,
            span,
        });
        return;
    }
    for (pl, rnd) in resolved_targets {
        let combined = HirExpr::Binary {
            op,
            lhs: Box::new(HirExpr::Place(pl.clone())),
            rhs: Box::new(rhs_expr.clone()),
            span,
        };
        let (on_se, not_se) = lower_size_error(size_error, ctx);
        out.push(HirStmt::AssignArith {
            targets: vec![(pl, rnd)],
            expr: combined,
            on_size_error: on_se,
            not_on_size_error: not_se,
            span,
        });
    }
}

/// Validates a slice of expressions (`CANCEL`, `EXHIBIT`).
///
/// # Arguments
///
/// * `exprs` - Expression list.
/// * `ctx` - Unit lowering context.
fn check_expr_list(exprs: &[Expr], ctx: &mut UnitLowerCtx<'_>) {
    for e in exprs {
        let _ = lower_expr_with_cat(e, ctx);
    }
}

/// Validates a slice of data places (`FREE`, `VALIDATE`).
///
/// # Arguments
///
/// * `places` - Data reference list.
/// * `ctx` - Unit lowering context.
fn check_place_list(places: &[DataRef], ctx: &mut UnitLowerCtx<'_>) {
    for p in places {
        let _ = resolve_place(p, ctx);
    }
}

/// Lowers an `INITIALIZE` statement (including `WITH FILLER`, `TO VALUE`, and `REPLACING`) into [`HirStmt::Initialize`].
///
/// # Arguments
///
/// * `targets` - Initialized data references.
/// * `filler` - AST filler policy.
/// * `value_mode` - AST `TO VALUE` policy.
/// * `replacing` - Category replacement `(cat_sym, expr)` pairs.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_initialize_stmt(
    targets: &[DataRef],
    filler: InitializeFillerMode,
    value_mode: InitializeValueMode,
    replacing: &[(SymbolId, Expr)],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let resolved_targets: Vec<HirPlace> = targets
        .iter()
        .filter_map(|t| resolve_place(t, ctx))
        .collect();
    let mut lowered_reps = Vec::with_capacity(replacing.len());
    for (cat_sym, rep_expr) in replacing {
        let (e_hir, _) = lower_expr_with_cat(rep_expr, ctx);
        let cat_name = ctx.interner.resolve_canonical(*cat_sym).unwrap_or("");
        if let Some(cat) = parse_init_category_name(cat_name) {
            lowered_reps.push((cat, e_hir));
        }
    }
    out.push(HirStmt::Initialize {
        targets: resolved_targets,
        filler: match filler {
            InitializeFillerMode::ExcludeFiller => HirInitFiller::ExcludeFiller,
            InitializeFillerMode::IncludeFiller => HirInitFiller::IncludeFiller,
        },
        value_mode: match value_mode {
            InitializeValueMode::CategoryDefault => HirInitValueMode::DefaultCategory,
            InitializeValueMode::DefinedValue => HirInitValueMode::RestoreValue,
        },
        replacements: lowered_reps,
        span,
    });
}

/// Maps an `INITIALIZE ... REPLACING <category>` keyword into [`DataCategory`].
///
/// # Arguments
///
/// * `name` - Canonical uppercase category keyword.
#[must_use]
fn parse_init_category_name(name: &str) -> Option<DataCategory> {
    match name {
        "ALPHABETIC" => Some(DataCategory::Alphabetic),
        "ALPHANUMERIC" => Some(DataCategory::Alphanumeric),
        "ALPHANUMERIC-EDITED" => Some(DataCategory::AlphanumericEdited),
        "NUMERIC" => Some(DataCategory::Numeric),
        "NUMERIC-EDITED" => Some(DataCategory::NumericEdited),
        "NATIONAL" => Some(DataCategory::National),
        "NATIONAL-EDITED" => Some(DataCategory::NationalEdited),
        "UTF-8" => Some(DataCategory::Utf8),
        "BOOLEAN" | "BIT" => Some(DataCategory::BooleanBit),
        "DATA-POINTER" | "PROGRAM-POINTER" | "FUNCTION-POINTER" => Some(DataCategory::Pointer),
        "INDEX" => Some(DataCategory::Index),
        "OBJECT-REFERENCE" => Some(DataCategory::ObjectReference),
        _ => None,
    }
}

/// Resolves a `DataRef` directly to its `HirDataItem` in `ctx.layout.items` when valid.
///
/// # Arguments
///
/// * `dref` - AST data reference.
/// * `ctx` - Unit lowering context.
fn lookup_layout_item(dref: &DataRef, ctx: &mut UnitLowerCtx<'_>) -> Option<HirDataItem> {
    ctx.symtab
        .resolve_symbol(
            ctx.scope,
            dref.name.base,
            &dref.name.qualifiers,
            dref.span,
            ctx.interner,
            ctx.sink,
        )
        .and_then(|res| match res {
            ResolvedSymbol::DataItem(id) => ctx.layout.items.get(id.as_u32() as usize).cloned(),
            _ => None,
        })
}

/// Desugars `MOVE CORRESPONDING src TO dst ...` into elementary `HirStmt::Move` pairs!
///
/// # Arguments
///
/// * `source` - Source group expression.
/// * `targets` - Destination group references.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_move_corresponding(
    source: &Expr,
    targets: &[DataRef],
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Expr::Data(src_ref) = source else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            String::from("`MOVE CORRESPONDING` source must be a group data item"),
        ));
        return;
    };
    let Some(src_grp) = lookup_layout_item(src_ref, ctx) else {
        return;
    };
    let src_id = src_grp.id;
    if src_grp.ty.category() != DataCategory::Group {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(src_ref.span),
            String::from("`MOVE CORRESPONDING` source must be a group item"),
        ));
    }

    for dst_ref in targets {
        let Some(dst_grp) = lookup_layout_item(dst_ref, ctx) else {
            continue;
        };
        let dst_id = dst_grp.id;
        if dst_grp.ty.category() != DataCategory::Group {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(dst_ref.span),
                String::from("`MOVE CORRESPONDING` destination must be a group item"),
            ));
            continue;
        }
        emit_matching_children_moves(src_id, dst_id, span, out, ctx.layout);
    }
}

/// Recursively emits `HirStmt::Move` for matching named subordinate children of `src_grp_id` and `dst_grp_id`.
///
/// # Arguments
///
/// * `src_grp_id` - Source group `DataItemId`.
/// * `dst_grp_id` - Destination group `DataItemId`.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `layout` - Solved Data Division layout.
fn emit_matching_children_moves(
    src_grp_id: DataItemId,
    dst_grp_id: DataItemId,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    layout: &SolvedDataLayout,
) {
    layout
        .items
        .get(src_grp_id.as_u32() as usize)
        .zip(layout.items.get(dst_grp_id.as_u32() as usize))
        .into_iter()
        .for_each(|(sg, dg)| {
            for sc in sg
                .children
                .iter()
                .filter_map(|&id| layout.items.get(id.as_u32() as usize))
            {
                let Some(s_name) = sc.name else {
                    continue;
                };
                if sc.redefines.is_some() || !sc.dimensions.is_empty() {
                    continue;
                }
                for dc in dg
                    .children
                    .iter()
                    .filter_map(|&id| layout.items.get(id.as_u32() as usize))
                {
                    if dc.name != Some(s_name)
                        || dc.redefines.is_some()
                        || !dc.dimensions.is_empty()
                    {
                        continue;
                    }
                    if sc.ty.category() == DataCategory::Group
                        && dc.ty.category() == DataCategory::Group
                    {
                        emit_matching_children_moves(sc.id, dc.id, span, out, layout);
                    } else if sc.ty.category() != DataCategory::Group
                        && dc.ty.category() != DataCategory::Group
                    {
                        out.push(HirStmt::Move {
                            src: HirExpr::Place(simple_place_for_item(sc, span)),
                            dst: simple_place_for_item(dc, span),
                            span,
                        });
                    }
                }
            }
        });
}

/// Desugars `ADD/SUBTRACT CORRESPONDING` into elementary `HirStmt::AssignArith` operations!
///
/// # Arguments
///
/// * `sources` - Source group operand list.
/// * `targets` - Destination rounded group targets.
/// * `op` - `Add` or `Sub`.
/// * `span` - Statement span.
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
fn lower_arith_corresponding(
    sources: &[Expr],
    targets: &[RoundedTarget],
    op: HirBinOp,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some(Expr::Data(src_ref)) = sources.first() else {
        return;
    };
    let Some(sg) = lookup_layout_item(src_ref, ctx) else {
        return;
    };
    let src_children = sg.children.clone();
    for rt in targets {
        let Some(dg) = lookup_layout_item(&rt.target, ctx) else {
            continue;
        };
        let dst_children = dg.children.clone();
        for sc in src_children
            .iter()
            .filter_map(|&id| ctx.layout.items.get(id.as_u32() as usize))
        {
            if sc.ty.category() != DataCategory::Numeric {
                continue;
            }
            for dc in dst_children
                .iter()
                .filter_map(|&id| ctx.layout.items.get(id.as_u32() as usize))
            {
                if dc.name.is_some()
                    && dc.name == sc.name
                    && dc.ty.category() == DataCategory::Numeric
                {
                    let sp_pl = simple_place_for_item(sc, span);
                    let dp_pl = simple_place_for_item(dc, span);
                    out.push(HirStmt::AssignArith {
                        targets: vec![(dp_pl.clone(), map_rounding(rt.rounding, ctx.interner))],
                        expr: HirExpr::Binary {
                            op,
                            lhs: Box::new(HirExpr::Place(dp_pl)),
                            rhs: Box::new(HirExpr::Place(sp_pl)),
                            span,
                        },
                        on_size_error: Vec::new(),
                        not_on_size_error: Vec::new(),
                        span,
                    });
                }
            }
        }
    }
}

/// Builds a direct unsubscripted [`HirPlace`] for an elementary [`HirDataItem`].
///
/// # Arguments
///
/// * `item` - Resolved data item.
/// * `span` - Reference span.
#[must_use]
pub(crate) fn simple_place_for_item(item: &HirDataItem, span: SourceSpan) -> HirPlace {
    HirPlace {
        item: item.id,
        static_offset: item.byte_offset,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: item.ty.clone(),
        check_mask: ExceptionCheckMask::NONE,
        span,
    }
}

/// Lowers and typechecks an AST [`Expr`], returning `(HirExpr, DataCategory)`.
///
/// # Arguments
///
/// * `expr` - AST expression.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_expr_with_cat(
    expr: &Expr,
    ctx: &mut UnitLowerCtx<'_>,
) -> (HirExpr, DataCategory) {
    match expr {
        Expr::Literal { atom, span } => lower_literal_expr(atom, *span, ctx),
        Expr::Data(dref) => lower_data_expr(dref, ctx),
        Expr::Unary { op, operand, .. } => {
            let (inner, cat) = lower_expr_with_cat(operand, ctx);
            validate_arith_category(
                cat,
                EditedTargetPolicy::StrictNumeric,
                operand.span(),
                ctx.sink,
            );
            match op {
                UnaryOp::Plus => (inner, DataCategory::Numeric),
                UnaryOp::Minus => (HirExpr::Neg(Box::new(inner)), DataCategory::Numeric),
            }
        }
        Expr::Binary {
            op, lhs, rhs, span, ..
        } => lower_binary_expr(*op, lhs, rhs, *span, ctx),
        Expr::FunctionCall {
            name,
            syntax,
            args,
            refmod,
            span,
        } => crate::intrinsics::lower_m12_function_call(
            *name,
            *syntax,
            args,
            refmod.as_ref(),
            *span,
            ctx,
        ),
        Expr::InlineInvoke {
            receiver,
            method,
            args,
            span,
        } => crate::m20_xml_json_oo::lower_inline_invoke_expr(receiver, *method, args, *span, ctx),
    }
}

/// Lowers an AST literal [`ValueAtom`] into `(HirExpr, DataCategory)`.
///
/// # Arguments
///
/// * `atom` - Literal atom.
/// * `span` - Literal span.
/// * `ctx` - Unit lowering context.
fn lower_literal_expr(
    atom: &ValueAtom,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> (HirExpr, DataCategory) {
    let cat = match atom {
        ValueAtom::Integer(_)
        | ValueAtom::Decimal { .. }
        | ValueAtom::Float(_)
        | ValueAtom::Figurative(FigurativeValue::Zero) => DataCategory::Numeric,
        ValueAtom::String { prefix, .. } => match prefix {
            coblang_syntax::StringPrefix::NationalN
            | coblang_syntax::StringPrefix::NationalHexNX => DataCategory::National,
            coblang_syntax::StringPrefix::Utf8U | coblang_syntax::StringPrefix::Utf8HexUX => {
                DataCategory::Utf8
            }
            _ => DataCategory::Alphanumeric,
        },
        ValueAtom::Figurative(_) => DataCategory::Alphanumeric,
        ValueAtom::Identifier(sym) => {
            if let Some(ResolvedSymbol::Constant(c)) =
                ctx.symtab
                    .resolve_symbol(ctx.scope, *sym, &[], span, ctx.interner, ctx.sink)
            {
                let c_cat = crate::m17_charset::const_value_category(&c);
                return (HirExpr::Const { value: c, span }, c_cat);
            }
            DataCategory::Alphanumeric
        }
    };
    let value = crate::m17_charset::atom_to_hir_const_m17(atom, ctx.scope, ctx.symtab, ctx.dialect)
        .unwrap_or(HirConstValue::Figurative(HirFigurative::Zero));
    (HirExpr::Const { value, span }, cat)
}

/// Lowers an `Expr::Data` reference (constant `78` vs addressable `HirPlace`).
///
/// # Arguments
///
/// * `dref` - AST data reference.
/// * `ctx` - Unit lowering context.
fn lower_data_expr(dref: &DataRef, ctx: &mut UnitLowerCtx<'_>) -> (HirExpr, DataCategory) {
    if let Some(oo_pair) = crate::m20_xml_json_oo::try_lower_self_or_super_data(dref, ctx) {
        return oo_pair;
    }
    if let Some(ResolvedSymbol::Constant(c)) = lookup_constant_silent(dref, ctx) {
        let cat = crate::m17_charset::const_value_category(&c);
        return (
            HirExpr::Const {
                value: c,
                span: dref.span,
            },
            cat,
        );
    }
    if dref.name.qualifiers.is_empty()
        && dref.subscripts.is_empty()
        && dref.modifier == DataRefModifier::Plain
        && !has_data_symbol_silent(dref.name.base, ctx)
        && crate::intrinsics::is_bare_repository_function(dref.name.base, ctx)
    {
        return crate::intrinsics::lower_m12_function_call(
            dref.name.base,
            coblang_syntax::ast::FunctionCallSyntax::BareRepository,
            &[],
            dref.refmod.as_ref(),
            dref.span,
            ctx,
        );
    }
    if let Some(pl) = resolve_place(dref, ctx) {
        match dref.modifier {
            DataRefModifier::AddressOf => (HirExpr::AddressOf(pl), DataCategory::Pointer),
            DataRefModifier::LengthOf => (HirExpr::LengthOf(pl), DataCategory::Numeric),
            DataRefModifier::Plain => {
                let mut cat = pl.effective_ty.category();
                if cat == DataCategory::NumericEdited
                    && ctx
                        .layout
                        .report_entry_items
                        .values()
                        .any(|&id| id == pl.item)
                {
                    cat = DataCategory::Numeric;
                }
                (HirExpr::Place(pl), cat)
            }
        }
    } else {
        (
            HirExpr::Const {
                value: HirConstValue::Decimal {
                    text: ctx.builtins.get("0"),
                    scale: ScaleFactor::ZERO,
                    sign: HirDecimalSign::UnsignedOrEmbedded,
                },
                span: dref.span,
            },
            DataCategory::Numeric,
        )
    }
}

/// Lowers a binary arithmetic or `&` concatenation expression.
///
/// # Arguments
///
/// * `op` - AST binary operator.
/// * `lhs` - Left operand.
/// * `rhs` - Right operand.
/// * `span` - Expression span.
/// * `ctx` - Unit lowering context.
fn lower_binary_expr(
    op: BinaryOp,
    lhs: &Expr,
    rhs: &Expr,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> (HirExpr, DataCategory) {
    let (l_hir, l_cat) = lower_expr_with_cat(lhs, ctx);
    let (r_hir, r_cat) = lower_expr_with_cat(rhs, ctx);
    let hir_op = match op {
        BinaryOp::Add => HirBinOp::Add,
        BinaryOp::Sub => HirBinOp::Sub,
        BinaryOp::Mul => HirBinOp::Mul,
        BinaryOp::Div => HirBinOp::Div,
        BinaryOp::Pow => HirBinOp::Pow,
        BinaryOp::Concat => HirBinOp::Concat,
    };
    if hir_op == HirBinOp::Concat {
        if let (
            HirExpr::Const {
                value: HirConstValue::Bytes(lb),
                ..
            },
            HirExpr::Const {
                value: HirConstValue::Bytes(rb),
                ..
            },
        ) = (&l_hir, &r_hir)
        {
            let mut merged = Vec::with_capacity(lb.len() + rb.len());
            merged.extend_from_slice(lb);
            merged.extend_from_slice(rb);
            return (
                HirExpr::Const {
                    value: HirConstValue::Bytes(merged),
                    span,
                },
                DataCategory::Alphanumeric,
            );
        }
        return (
            HirExpr::Binary {
                op: hir_op,
                lhs: Box::new(l_hir),
                rhs: Box::new(r_hir),
                span,
            },
            DataCategory::Alphanumeric,
        );
    }
    if l_cat != DataCategory::Index {
        validate_arith_category(
            l_cat,
            EditedTargetPolicy::StrictNumeric,
            lhs.span(),
            ctx.sink,
        );
    }
    if r_cat != DataCategory::Index {
        validate_arith_category(
            r_cat,
            EditedTargetPolicy::StrictNumeric,
            rhs.span(),
            ctx.sink,
        );
    }
    (
        HirExpr::Binary {
            op: hir_op,
            lhs: Box::new(l_hir),
            rhs: Box::new(r_hir),
            span,
        },
        DataCategory::Numeric,
    )
}

/// Resolves an AST [`DataRef`] into a [`HirPlace`], validating subscripts & reference modification.
///
/// # Arguments
///
/// * `dref` - AST data reference.
/// * `ctx` - Unit lowering context.
pub(crate) fn resolve_place(dref: &DataRef, ctx: &mut UnitLowerCtx<'_>) -> Option<HirPlace> {
    let res = ctx.symtab.resolve_symbol(
        ctx.scope,
        dref.name.base,
        &dref.name.qualifiers,
        dref.span,
        ctx.interner,
        ctx.sink,
    )?;
    let ResolvedSymbol::DataItem(item_id) = res else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(dref.span),
            String::from("expected addressable data item, found condition-name or file"),
        ));
        return None;
    };
    ctx.layout
        .items
        .get(item_id.as_u32() as usize)
        .cloned()
        .map(|item| {
            let effective_ty = validate_subscripts_and_refmod(dref, &item, ctx.interner, ctx.sink);
            let subscripts = dref
                .subscripts
                .iter()
                .enumerate()
                .map(|(i, sub)| {
                    let stride = item.dimensions.get(i).map_or(1, |d| d.element_byte_stride);
                    let expr = match sub {
                        SubscriptItem::Expr(e) => lower_expr_with_cat(e, ctx).0,
                        SubscriptItem::AllWildcard(sp) => HirExpr::Const {
                            value: HirConstValue::Decimal {
                                text: ctx.builtins.get("1"),
                                scale: ScaleFactor::ZERO,
                                sign: HirDecimalSign::UnsignedOrEmbedded,
                            },
                            span: *sp,
                        },
                    };
                    (expr, stride)
                })
                .collect();

            let refmod = dref.refmod.as_ref().map(|rm| HirRefMod {
                offset: Box::new(lower_expr_with_cat(&rm.offset, ctx).0),
                length: rm
                    .length
                    .as_ref()
                    .map(|l| Box::new(lower_expr_with_cat(l, ctx).0)),
            });

            let check_mask = crate::m9_tables::compute_active_check_mask(
                dref.span,
                ctx.dialect.bounds_checks,
                ctx.turn_directives,
            );
            HirPlace {
                item: item_id,
                static_offset: item.byte_offset,
                subscripts,
                refmod,
                effective_ty,
                check_mask,
                span: dref.span,
            }
        })
}

/// Lowers and desugars an AST [`Condition`] (including `88` condition-names and abbreviated relations!).
///
/// # Arguments
///
/// * `cond` - AST condition.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_condition(cond: &Condition, ctx: &mut UnitLowerCtx<'_>) -> HirCondition {
    match cond {
        Condition::BooleanConst { value, .. } => HirCondition::BoolConst(match value {
            TruthValue::True => HirTruthValue::True,
            TruthValue::False => HirTruthValue::False,
        }),
        Condition::Not { inner, .. } => HirCondition::Not(Box::new(lower_condition(inner, ctx))),
        Condition::And { lhs, rhs, .. } => HirCondition::And(
            Box::new(lower_condition(lhs, ctx)),
            Box::new(lower_condition(rhs, ctx)),
        ),
        Condition::Or { lhs, rhs, .. } => HirCondition::Or(
            Box::new(lower_condition(lhs, ctx)),
            Box::new(lower_condition(rhs, ctx)),
        ),
        Condition::ConditionRef(dref) => expand_condition_ref(dref, ctx),
        Condition::OmittedCheck {
            target, polarity, ..
        } => {
            let item = resolve_place(target, ctx).map_or(DataItemId::new(0), |pl| pl.item);
            apply_polarity(HirCondition::Omitted { item }, *polarity)
        }
        Condition::SignCheck {
            subject,
            polarity,
            sign,
            span,
        } => {
            let (sub_h, sub_cat) = lower_expr_with_cat(subject, ctx);
            validate_arith_category(sub_cat, EditedTargetPolicy::StrictNumeric, *span, ctx.sink);
            let zero_expr = HirExpr::Const {
                value: HirConstValue::Decimal {
                    text: ctx.builtins.get("0"),
                    scale: ScaleFactor::ZERO,
                    sign: HirDecimalSign::UnsignedOrEmbedded,
                },
                span: *span,
            };
            let op = match sign {
                SignCheckKind::Positive => HirRelOp::Gt,
                SignCheckKind::Negative => HirRelOp::Lt,
                SignCheckKind::Zero => HirRelOp::Eq,
            };
            apply_polarity(
                HirCondition::Compare {
                    op,
                    lhs: sub_h,
                    rhs: zero_expr,
                },
                *polarity,
            )
        }
        Condition::ClassCheck {
            subject,
            polarity,
            class,
            ..
        } => {
            let (sub_h, _) = lower_expr_with_cat(subject, ctx);
            let cls_sym = match class {
                ClassCheckKind::Numeric => ctx.builtins.get("NUMERIC"),
                ClassCheckKind::Alphabetic => ctx.builtins.get("ALPHABETIC"),
                ClassCheckKind::AlphabeticLower => ctx.builtins.get("ALPHABETIC-LOWER"),
                ClassCheckKind::AlphabeticUpper => ctx.builtins.get("ALPHABETIC-UPPER"),
                ClassCheckKind::Kanji => ctx.builtins.get("KANJI"),
                ClassCheckKind::Custom(sym) => *sym,
            };
            apply_polarity(
                HirCondition::ClassCheck {
                    expr: sub_h,
                    class_name: cls_sym,
                },
                *polarity,
            )
        }
        Condition::Relation {
            subject, op, rhs, ..
        } => {
            let (sub_h, _) = lower_expr_with_cat(subject, ctx);
            let mut active_op = map_rel_op(*op);
            expand_abbrev_rhs(&sub_h, &mut active_op, rhs, ctx)
        }
    }
}

/// Recursively expands an ANSI-85 abbreviated relation [`AbbrevRhs`] (disambiguating `88` condition-names!).
///
/// # Arguments
///
/// * `subject` - Inherited left-hand subject expression.
/// * `inherited_op` - Mutable active relational operator.
/// * `rhs` - Abbreviated right-hand tree.
/// * `ctx` - Unit lowering context.
fn expand_abbrev_rhs(
    subject: &HirExpr,
    inherited_op: &mut HirRelOp,
    rhs: &AbbrevRhs,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirCondition {
    match rhs {
        AbbrevRhs::Leaf {
            negated,
            op,
            object,
            ..
        } => {
            if op.is_none()
                && let Expr::Data(dref) = object
                && is_condition_symbol_silent(dref, ctx)
            {
                let c88 = expand_condition_ref(dref, ctx);
                return apply_polarity(c88, *negated);
            }
            if let Some(explicit_op) = op {
                *inherited_op = map_rel_op(*explicit_op);
            }
            let (obj_h, _) = lower_expr_with_cat(object, ctx);
            let cmp = HirCondition::Compare {
                op: *inherited_op,
                lhs: subject.clone(),
                rhs: obj_h,
            };
            apply_polarity(cmp, *negated)
        }
        AbbrevRhs::Binary { logic, lhs, rhs } => {
            let l = expand_abbrev_rhs(subject, inherited_op, lhs, ctx);
            let r = expand_abbrev_rhs(subject, inherited_op, rhs, ctx);
            match logic {
                AbbrevLogicOp::And => HirCondition::And(Box::new(l), Box::new(r)),
                AbbrevLogicOp::Or => HirCondition::Or(Box::new(l), Box::new(r)),
            }
        }
    }
}

/// Expands a level-`88` or `SWITCH` condition-name reference into a [`HirCondition`]!
///
/// # Arguments
///
/// * `dref` - Condition-name reference.
/// * `ctx` - Unit lowering context.
pub(crate) fn expand_condition_ref(dref: &DataRef, ctx: &mut UnitLowerCtx<'_>) -> HirCondition {
    let Some(res) = ctx.symtab.resolve_symbol(
        ctx.scope,
        dref.name.base,
        &dref.name.qualifiers,
        dref.span,
        ctx.interner,
        ctx.sink,
    ) else {
        return HirCondition::BoolConst(HirTruthValue::False);
    };
    match res {
        ResolvedSymbol::Condition88(idx) => {
            let (parent_id, ranges) = ctx
                .layout
                .conditions_88
                .get(idx)
                .map_or((DataItemId::new(0), Vec::new()), |c88| {
                    (c88.parent_item, c88.ranges.clone())
                });
            let parent_pl = build_parent_place_with_subscripts(parent_id, dref, ctx);
            let parent_expr = HirExpr::Place(parent_pl);
            let mut combined: Option<HirCondition> = None;
            for (lo, hi_opt) in &ranges {
                let lo_e = HirExpr::Const {
                    value: lo.clone(),
                    span: dref.span,
                };
                let pred = if let Some(hi) = hi_opt {
                    let hi_e = HirExpr::Const {
                        value: hi.clone(),
                        span: dref.span,
                    };
                    HirCondition::And(
                        Box::new(HirCondition::Compare {
                            op: HirRelOp::Ge,
                            lhs: parent_expr.clone(),
                            rhs: lo_e,
                        }),
                        Box::new(HirCondition::Compare {
                            op: HirRelOp::Le,
                            lhs: parent_expr.clone(),
                            rhs: hi_e,
                        }),
                    )
                } else {
                    HirCondition::Compare {
                        op: HirRelOp::Eq,
                        lhs: parent_expr.clone(),
                        rhs: lo_e,
                    }
                };
                combined = Some(match combined {
                    None => pred,
                    Some(acc_cond) => HirCondition::Or(Box::new(acc_cond), Box::new(pred)),
                });
            }
            combined.unwrap_or(HirCondition::BoolConst(HirTruthValue::False))
        }
        ResolvedSymbol::SwitchCondition {
            switch_index,
            expected,
        } => HirCondition::Switch {
            switch_index,
            expected,
        },
        _ => {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(dref.span),
                String::from(
                    "data item cannot be used directly as a boolean condition without an operator",
                ),
            ));
            HirCondition::BoolConst(HirTruthValue::False)
        }
    }
}

/// Checks without emitting diagnostics if `dref` resolves to a `78` / `01 CONSTANT`.
///
/// # Arguments
///
/// * `dref` - Candidate data reference.
/// * `ctx` - Unit lowering context.
#[must_use]
pub(crate) fn lookup_constant_silent(
    dref: &DataRef,
    ctx: &UnitLowerCtx<'_>,
) -> Option<ResolvedSymbol> {
    if !dref.name.qualifiers.is_empty() || !dref.subscripts.is_empty() || dref.refmod.is_some() {
        return None;
    }
    let mut cur = Some(ctx.scope);
    while let Some(sc) = cur.and_then(|s| ctx.symtab.scope(s)) {
        if let Some(cand) = sc
            .by_name
            .get(&dref.name.base)
            .and_then(|list| list.first())
            && matches!(cand.target, ResolvedSymbol::Constant(_))
        {
            return Some(cand.target.clone());
        }
        cur = sc.parent_scope;
    }
    None
}

/// Checks without emitting diagnostics if `sym` resolves to any `DataItem`, `ConditionName`, or `Constant` in scope.
///
/// # Arguments
///
/// * `sym` - Candidate identifier symbol.
/// * `ctx` - Unit lowering context.
#[must_use]
fn has_data_symbol_silent(sym: SymbolId, ctx: &UnitLowerCtx<'_>) -> bool {
    let mut cur = Some(ctx.scope);
    while let Some(sc) = cur.and_then(|s| ctx.symtab.scope(s)) {
        if let Some(list) = sc.by_name.get(&sym)
            && list.iter().any(|c| {
                matches!(
                    c.target,
                    ResolvedSymbol::DataItem(_)
                        | ResolvedSymbol::Condition88 { .. }
                        | ResolvedSymbol::Constant(_)
                )
            })
        {
            return true;
        }
        cur = sc.parent_scope;
    }
    false
}

/// Checks without emitting diagnostics if `dref` resolves to a level-`88` or switch condition-name.
///
/// # Arguments
///
/// * `dref` - Candidate reference.
/// * `ctx` - Unit lowering context.
#[must_use]
pub(crate) fn is_condition_symbol_silent(dref: &DataRef, ctx: &UnitLowerCtx<'_>) -> bool {
    let mut cur = Some(ctx.scope);
    while let Some(sc) = cur.and_then(|s| ctx.symtab.scope(s)) {
        if let Some(list) = sc.by_name.get(&dref.name.base)
            && list.iter().any(|c| {
                matches!(
                    c.target,
                    ResolvedSymbol::Condition88(_) | ResolvedSymbol::SwitchCondition { .. }
                )
            })
        {
            return true;
        }
        cur = sc.parent_scope;
    }
    false
}

/// Folds 1+ arithmetic operand expressions with `op`.
///
/// # Arguments
///
/// * `operands` - AST operand slice.
/// * `op` - Binary operator.
/// * `span` - Statement span.
/// * `ctx` - Unit lowering context.
fn fold_arith_operands(
    operands: &[Expr],
    op: HirBinOp,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> HirExpr {
    let mut acc: Option<HirExpr> = None;
    for e in operands {
        let (h, cat) = lower_expr_with_cat(e, ctx);
        validate_arith_category(cat, EditedTargetPolicy::StrictNumeric, e.span(), ctx.sink);
        acc = Some(match acc {
            None => h,
            Some(prev) => HirExpr::Binary {
                op,
                lhs: Box::new(prev),
                rhs: Box::new(h),
                span,
            },
        });
    }
    acc.unwrap_or(HirExpr::Const {
        value: HirConstValue::Decimal {
            text: ctx.builtins.get("0"),
            scale: ScaleFactor::ZERO,
            sign: HirDecimalSign::UnsignedOrEmbedded,
        },
        span,
    })
}

/// Resolves `GIVING` / `COMPUTE` rounded destinations (allowing `NumericEdited`).
///
/// # Arguments
///
/// * `targets` - AST rounded targets.
/// * `ctx` - Unit lowering context.
fn resolve_giving_targets(
    targets: &[RoundedTarget],
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<(HirPlace, HirRounding)> {
    let mut out = Vec::with_capacity(targets.len());
    for rt in targets {
        if let Some(pl) = resolve_place(&rt.target, ctx) {
            validate_arith_category(
                pl.effective_ty.category(),
                EditedTargetPolicy::AllowNumericEdited,
                rt.target.span,
                ctx.sink,
            );
            out.push((pl, map_rounding(rt.rounding, ctx.interner)));
        }
    }
    out
}

/// Lowers `SizeErrorHandlers` into `(on_size_error, not_on_size_error)`.
///
/// # Arguments
///
/// * `se` - AST size error handlers.
/// * `ctx` - Unit lowering context.
fn lower_size_error(
    se: &SizeErrorHandlers,
    ctx: &mut UnitLowerCtx<'_>,
) -> (Vec<HirStmt>, Vec<HirStmt>) {
    let mut a = Vec::new();
    let mut b = Vec::new();
    lower_stmt_slice(&se.on_size_error, &mut a, ctx);
    if let Some(first) = se.on_size_error.first()
        && a.is_empty()
    {
        a.push(HirStmt::Sentence {
            body: Vec::new(),
            span: first.span(),
        });
    }
    lower_stmt_slice(&se.not_on_size_error, &mut b, ctx);
    if let Some(first) = se.not_on_size_error.first()
        && b.is_empty()
    {
        b.push(HirStmt::Sentence {
            body: Vec::new(),
            span: first.span(),
        });
    }
    (a, b)
}

/// Lowers `EventHandlers` (`ON EXCEPTION`, `AT END`, `INVALID KEY`, `ON OVERFLOW`).
///
/// # Arguments
///
/// * `eh` - AST event handlers.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_event_handlers(
    eh: &EventHandlers,
    ctx: &mut UnitLowerCtx<'_>,
) -> (Vec<HirStmt>, Vec<HirStmt>) {
    let mut a = Vec::new();
    let mut b = Vec::new();
    lower_stmt_slice(&eh.on_event, &mut a, ctx);
    if let Some(first) = eh.on_event.first()
        && a.is_empty()
    {
        a.push(HirStmt::Sentence {
            body: Vec::new(),
            span: first.span(),
        });
    }
    lower_stmt_slice(&eh.not_on_event, &mut b, ctx);
    if let Some(first) = eh.not_on_event.first()
        && b.is_empty()
    {
        b.push(HirStmt::Sentence {
            body: Vec::new(),
            span: first.span(),
        });
    }
    (a, b)
}

/// Maps AST [`RoundingSpec`] to [`HirRounding`] across all eight ISO/IEC 1989:2023 modes.
///
/// # Arguments
///
/// * `r` - AST rounding spec.
/// * `interner` - Symbol interner for resolving explicit `MODE IS ...` keywords.
#[must_use]
fn map_rounding(r: RoundingSpec, interner: &SymbolInterner) -> HirRounding {
    match r {
        RoundingSpec::Unrounded => HirRounding::Truncate,
        RoundingSpec::Rounded(None) => HirRounding::NearestAwayFromZero,
        RoundingSpec::Rounded(Some(sym)) => match interner.resolve_canonical(sym).unwrap_or("") {
            "TRUNCATION" => HirRounding::Truncate,
            "NEAREST-EVEN" => HirRounding::NearestEven,
            "NEAREST-TOWARD-ZERO" => HirRounding::NearestTowardZero,
            "PROHIBITED" => HirRounding::Prohibited,
            "TOWARD-GREATER" => HirRounding::TowardGreater,
            "TOWARD-LESSER" => HirRounding::TowardLesser,
            "AWAY-FROM-ZERO" => HirRounding::AwayFromZero,
            _ => HirRounding::NearestAwayFromZero,
        },
    }
}

/// Maps AST [`RelOp`] to [`HirRelOp`].
///
/// # Arguments
///
/// * `op` - AST relational operator.
#[must_use]
const fn map_rel_op(op: RelOp) -> HirRelOp {
    match op {
        RelOp::Eq => HirRelOp::Eq,
        RelOp::Ne => HirRelOp::Ne,
        RelOp::Lt => HirRelOp::Lt,
        RelOp::Le => HirRelOp::Le,
        RelOp::Gt => HirRelOp::Gt,
        RelOp::Ge => HirRelOp::Ge,
    }
}

/// Applies `NegationMode` (`NOT`) to `cond`.
///
/// # Arguments
///
/// * `cond` - Base condition.
/// * `polarity` - `Positive` vs `Negated`.
#[must_use]
pub(crate) fn apply_polarity(cond: HirCondition, polarity: NegationMode) -> HirCondition {
    match polarity {
        NegationMode::Positive => cond,
        NegationMode::Negated => HirCondition::Not(Box::new(cond)),
    }
}
