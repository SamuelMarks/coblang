//! Semantic validation and HIR lowering for Milestone M20 (`XML GENERATE`/`PARSE`, `JSON GENERATE`/`PARSE`, and Object-Oriented COBOL `CLASS-ID`/`INTERFACE-ID`/`INVOKE`).

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{DataItemId, Dialect, LevelNumber, Result, SourceSpan, SymbolId};
use coblang_diag::{
    Diagnostic, DiagnosticCode, DiagnosticSink, DialectConfig, DpcInDataMode, FeatureDisposition,
    SymbolInterner, WarningGroup, WarningPolicy, XmlParseMode,
};
use coblang_hir::{
    DataCategory, HirCallArg, HirClass, HirDataItem, HirDecimalCommaMode, HirExpr,
    HirGenericSuppressRule, HirJsonDetailMode, HirJsonGenerateStmt, HirJsonParseStmt, HirM20Expr,
    HirM20Stmt, HirMarkupNameOverride, HirModule, HirOoClassSpec, HirOoCompartment,
    HirOoInlineInvokeExpr, HirOoInvokeStmt, HirOoMethodSpec, HirOoReceiver, HirProgram, HirStmt,
    HirStorageSection, HirType, HirXmlAttrMode, HirXmlDeclMode, HirXmlGenerateStmt, HirXmlNodeType,
    HirXmlParseStmt, HirXmlStreamMode,
};
use coblang_syntax::ast::{
    BlankWhenZeroMode, CallArg, ClassIdClause, ClassModifierKind, ClassUnit, CommonVisibility,
    CompilationGroup, CompilationUnit, DataClauseCounts, DataDivision, DataEntry, DataEntryName,
    DataRef, DataRefModifier, DecimalPointKind, EnvironmentDivision, EventHandlers, Expr,
    ExternalLinkage, InitialState, InterfaceUnit, JsonBoolClause, JsonDetailPresence,
    JustificationMode, MarkupNameClause, MarkupNameTarget, MarkupSuppressClause,
    MarkupSuppressCond, MarkupSuppressGenericKind, MarkupSuppressNodeKind, MarkupSuppressSubject,
    MethodPropertyRole, MethodUnit, ParamOptionality, PassingMode, ProcedureDivision,
    ProgramAttributes, PrototypeKind, QualifiedName, Recursivity, ResidencyKind,
    StorageLinkageFlags, SyncAlignment, XmlAttrPresence, XmlDeclPresence, XmlNodeTypeClause,
    XmlTypeClause,
};

use crate::lower_hir::{
    GroupLowerEnv, UnitAstSlice, UnitLowerCtx, lower_event_handlers, lower_expr_with_cat,
    lower_unit_parts, resolve_place,
};
use crate::m8_flow::resolve_perform_proc_bounds;
use crate::picture::PictureContext;
use crate::symbols::ResolvedSymbol;

/// Returns `true` if Object-Oriented COBOL is supported under `dialect`.
///
/// # Arguments
/// * `dialect` - Active [`DialectConfig`].
#[must_use]
pub fn oo_supported_in_dialect(dialect: &DialectConfig) -> bool {
    if let Some(&disp) = dialect.feature_rules.get("oo-cobol") {
        return disp != FeatureDisposition::Unconformable;
    }
    !matches!(dialect.base_dialect, Dialect::Gnu | Dialect::Cobol85)
}

/// Pre-extracted signature of an OO `METHOD-ID` for compile-time `INVOKE` validation.
#[derive(Debug, Clone)]
pub struct OoMethodSig {
    /// `Factory` vs `Object` compartment.
    pub compartment: HirOoCompartment,
    /// `true` per `USING` parameter when marked `OPTIONAL`.
    pub param_optional: Vec<bool>,
    /// Whether `PROCEDURE DIVISION RETURNING` was declared.
    pub has_returning: bool,
    /// Approximate return [`DataCategory`] (`Alphanumeric` or `Numeric`).
    pub return_cat: DataCategory,
    /// Approximate return [`HirType`].
    pub return_ty: HirType,
}

/// Summary of a `CLASS-ID` or `INTERFACE-ID` across the compilation group.
#[derive(Debug, Clone)]
pub struct OoTypeInfo {
    /// Source identifier spelling.
    pub source_name: String,
    /// External linkage name (`AS "ext"` if present, else `source_name`).
    pub external_name: String,
    /// Ordered parent internal/external names from `INHERITS`.
    pub parent_names: Vec<String>,
    /// Per-compartment method table keyed by canonical uppercase method name.
    pub methods: BTreeMap<(HirOoCompartment, String), OoMethodSig>,
    /// Cloned `OBJECT. WORKING-STORAGE` AST entries for subclass field inheritance.
    pub object_ws_entries: Vec<DataEntry>,
}

/// Compilation-group-wide catalog of OO classes, interfaces, and functions.
#[derive(Debug, Clone, Default)]
pub struct OoGroupCatalog {
    /// Defined classes keyed by canonical uppercase external name.
    pub classes_by_ext: BTreeMap<String, OoTypeInfo>,
    /// Defined interfaces keyed by canonical uppercase external name.
    pub interfaces_by_ext: BTreeMap<String, OoTypeInfo>,
    /// Defined `FUNCTION-ID` external names mapped to original function spelling.
    pub functions_by_ext: BTreeMap<String, String>,
}

impl OoGroupCatalog {
    /// Builds the group-wide OO catalog and emits class-header / duplicate-ID diagnostics.
    ///
    /// # Arguments
    /// * `group` - Parsed [`CompilationGroup`].
    /// * `interner` - Symbol interner.
    /// * `sink` - Compiler diagnostic sink.
    #[must_use]
    pub fn build(
        group: &CompilationGroup,
        interner: &SymbolInterner,
        sink: &mut DiagnosticSink,
    ) -> Self {
        let mut cat = Self::default();
        let mut seen_class_ids = BTreeSet::new();
        let mut seen_iface_ids = BTreeSet::new();

        for unit in &group.units {
            match unit {
                CompilationUnit::Function(f) => {
                    let orig = sym_orig(f.function_id.name, interner);
                    let ext_sym = f.function_id.external_alias.unwrap_or(f.function_id.name);
                    let ext_key = sym_canon(ext_sym, interner);
                    cat.functions_by_ext.insert(ext_key, orig);
                }
                CompilationUnit::Class(cls) => {
                    validate_class_modifiers(&cls.class_id, sink);
                    let id_canon = sym_canon(cls.class_id.name, interner);
                    let orig = sym_orig(cls.class_id.name, interner);
                    if !seen_class_ids.insert(id_canon) {
                        sink.emit(Diagnostic::error(
                            DiagnosticCode::E0007InvalidLayout,
                            Some(cls.class_id.name_span),
                            format!("redefinition of class ID '{orig}'"),
                        ));
                    }
                    let info = extract_class_type_info(cls, interner);
                    cat.classes_by_ext
                        .insert(info.external_name.to_ascii_uppercase(), info);
                }
                CompilationUnit::Interface(iface) => {
                    let id_canon = sym_canon(iface.interface_id.name, interner);
                    let orig = sym_orig(iface.interface_id.name, interner);
                    if !seen_iface_ids.insert(id_canon) {
                        sink.emit(Diagnostic::error(
                            DiagnosticCode::E0007InvalidLayout,
                            Some(iface.interface_id.name_span),
                            format!("redefinition of interface ID '{orig}'"),
                        ));
                    }
                    let info = extract_interface_type_info(iface, interner);
                    cat.interfaces_by_ext
                        .insert(info.external_name.to_ascii_uppercase(), info);
                }
                CompilationUnit::Program(_) => {}
            }
        }
        cat
    }

    /// Looks up a method signature on `ext_class` (walking `INHERITS` ancestors).
    ///
    /// # Arguments
    /// * `ext_class` - External class or interface name.
    /// * `method_canon` - Uppercase method name.
    /// * `pref_comp` - Preferred compartment (`None` checks both `Object` and `Factory`).
    #[must_use]
    pub fn lookup_method(
        &self,
        ext_class: &str,
        method_canon: &str,
        pref_comp: Option<HirOoCompartment>,
    ) -> Option<&OoMethodSig> {
        let mut visited = BTreeSet::new();
        self.lookup_method_rec(ext_class, method_canon, pref_comp, &mut visited)
    }

    /// Recursive helper for [`Self::lookup_method`].
    ///
    /// # Arguments
    ///
    /// * `ext_class` - External class or interface name.
    /// * `method_canon` - Uppercase canonical method name.
    /// * `pref_comp` - Preferred compartment (`None` checks `Object` then `Factory`).
    /// * `visited` - Cycle-guard set of visited class names.
    fn lookup_method_rec<'a>(
        &'a self,
        ext_class: &str,
        method_canon: &str,
        pref_comp: Option<HirOoCompartment>,
        visited: &mut BTreeSet<String>,
    ) -> Option<&'a OoMethodSig> {
        let key = ext_class.to_ascii_uppercase();
        if !visited.insert(key.clone()) {
            return None;
        }
        let info = self
            .classes_by_ext
            .get(&key)
            .or_else(|| self.interfaces_by_ext.get(&key))?;
        let comps: &[HirOoCompartment] = match pref_comp {
            Some(HirOoCompartment::Factory) => &[HirOoCompartment::Factory],
            Some(HirOoCompartment::Object) => &[HirOoCompartment::Object],
            None => &[HirOoCompartment::Object, HirOoCompartment::Factory],
        };
        for &c in comps {
            if let Some(sig) = info.methods.get(&(c, method_canon.to_owned())) {
                return Some(sig);
            }
        }
        for parent in &info.parent_names {
            if let Some(sig) = self.lookup_method_rec(parent, method_canon, pref_comp, visited) {
                return Some(sig);
            }
        }
        None
    }

    /// Collects inherited + own `OBJECT` `WORKING-STORAGE` entries in base-to-derived order.
    ///
    /// # Arguments
    /// * `ext_class` - External class name.
    #[must_use]
    pub fn collect_object_ws_hierarchy(&self, ext_class: &str) -> Vec<DataEntry> {
        let mut out = Vec::new();
        let mut visited = BTreeSet::new();
        self.collect_ws_rec(ext_class, &mut visited, &mut out);
        out
    }

    /// Recursive helper for [`Self::collect_object_ws_hierarchy`].
    ///
    /// # Arguments
    ///
    /// * `ext_class` - External class name.
    /// * `visited` - Cycle-guard set of visited class names.
    /// * `out` - Output `WORKING-STORAGE` entries accumulator.
    fn collect_ws_rec(
        &self,
        ext_class: &str,
        visited: &mut BTreeSet<String>,
        out: &mut Vec<DataEntry>,
    ) {
        let key = ext_class.to_ascii_uppercase();
        if !visited.insert(key.clone()) {
            return;
        }
        if let Some(info) = self.classes_by_ext.get(&key) {
            for p in &info.parent_names {
                self.collect_ws_rec(p, visited, out);
            }
            out.extend(info.object_ws_entries.iter().cloned());
        }
    }
}

/// Binding recorded for an internal `REPOSITORY` symbol in the current compilation unit.
#[derive(Debug, Clone)]
pub enum OoRepoTarget {
    /// Valid `CLASS` binding to `external_name`.
    Class(String),
    /// Valid `INTERFACE` binding to `external_name`.
    Interface(String),
    /// Invalid `CLASS` / `INTERFACE` `REPOSITORY` entry that matched a `FUNCTION` (`syn_oo/0015`).
    WrongFunction(String),
}

/// Per-unit `REPOSITORY` class and interface bindings.
#[derive(Debug, Clone, Default)]
pub struct OoRepoBindings {
    /// Map from canonical internal symbol name to [`OoRepoTarget`].
    pub by_internal: BTreeMap<String, OoRepoTarget>,
}

/// Enclosing method context when lowering statements/expressions inside a `METHOD-ID`.
#[derive(Debug, Clone)]
pub struct OoActiveMethodEnv {
    /// Source class/interface name (`MyBaseClass`).
    pub source_class_name: String,
    /// External class/interface name (`MyClass`).
    pub external_class_name: String,
    /// `Factory` vs `Object` compartment.
    pub compartment: HirOoCompartment,
    /// Ordered external names of immediate parent classes/interfaces.
    pub parent_externals: Vec<String>,
    /// Total number of parents in `INHERITS` (`> 1` triggers `SUPER` ambiguity warning).
    pub parent_count: usize,
}

/// Validates a unit's `REPOSITORY` `CLASS` / `INTERFACE` entries and returns resolved bindings.
///
/// # Arguments
/// * `env_div` - Unit [`EnvironmentDivision`].
/// * `catalog` - Group-wide [`OoGroupCatalog`].
/// * `interner` - Symbol interner.
/// * `sink` - Diagnostic sink.
#[must_use]
pub fn validate_and_bind_repository(
    env_div: &EnvironmentDivision,
    catalog: &OoGroupCatalog,
    interner: &SymbolInterner,
    sink: &mut DiagnosticSink,
) -> OoRepoBindings {
    let repo = &env_div.configuration.repository;
    let mut bindings = OoRepoBindings::default();

    for (idx, &(internal_sym, alias_opt)) in repo.classes.iter().enumerate() {
        let sp = repo.class_spans.get(idx).copied();
        let internal_canon = sym_canon(internal_sym, interner);
        let ext_sym = alias_opt.unwrap_or(internal_sym);
        let ext_orig = sym_orig(ext_sym, interner);
        let ext_canon = ext_orig.to_ascii_uppercase();

        if let Some(fn_orig) = catalog.functions_by_ext.get(&ext_canon) {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                sp,
                format!("CLASS REPOSITORY entry for '{ext_orig}' does not match its definition"),
            ));
            bindings
                .by_internal
                .insert(internal_canon, OoRepoTarget::WrongFunction(fn_orig.clone()));
        } else {
            if !catalog.classes_by_ext.contains_key(&ext_canon) {
                sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::CallParams,
                    sp,
                    format!("no definition/prototype seen for CLASS '{ext_orig}'"),
                ));
            }
            bindings
                .by_internal
                .insert(internal_canon, OoRepoTarget::Class(ext_orig));
        }
    }

    for (idx, &(internal_sym, alias_opt)) in repo.interfaces.iter().enumerate() {
        let sp = repo.interface_spans.get(idx).copied();
        let internal_canon = sym_canon(internal_sym, interner);
        let ext_sym = alias_opt.unwrap_or(internal_sym);
        let ext_orig = sym_orig(ext_sym, interner);
        let ext_canon = ext_orig.to_ascii_uppercase();

        if let Some(fn_orig) = catalog.functions_by_ext.get(&ext_canon) {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                sp,
                format!(
                    "INTERFACE REPOSITORY entry for '{ext_orig}' does not match its definition"
                ),
            ));
            bindings
                .by_internal
                .insert(internal_canon, OoRepoTarget::WrongFunction(fn_orig.clone()));
        } else {
            if !catalog.interfaces_by_ext.contains_key(&ext_canon) {
                sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::CallParams,
                    sp,
                    format!("no definition/prototype seen for INTERFACE '{ext_orig}'"),
                ));
            }
            bindings
                .by_internal
                .insert(internal_canon, OoRepoTarget::Interface(ext_orig));
        }
    }

    bindings
}

/// Validates duplicate / conflicting `CLASS-ID` modifiers (`FINAL`, `ABSTRACT`, `PUBLIC`, `INTERNAL`, `PARTIAL`).
///
/// # Arguments
///
/// * `class_id` - Parsed [`ClassIdClause`].
/// * `sink` - Diagnostic sink.
fn validate_class_modifiers(class_id: &ClassIdClause, sink: &mut DiagnosticSink) {
    let mut seen_final = false;
    let mut seen_abstract = false;
    let mut seen_public = false;
    let mut seen_internal = false;
    let mut seen_partial = false;

    for &(kind, sp) in &class_id.modifiers {
        match kind {
            ClassModifierKind::Final => {
                if seen_final {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0003Syntax,
                        Some(sp),
                        String::from("duplicate FINAL clause"),
                    ));
                } else if seen_abstract {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0003Syntax,
                        Some(sp),
                        String::from("cannot specify both FINAL and ABSTRACT"),
                    ));
                }
                seen_final = true;
            }
            ClassModifierKind::Abstract => {
                if seen_abstract {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0003Syntax,
                        Some(sp),
                        String::from("duplicate ABSTRACT clause"),
                    ));
                } else if seen_final {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0003Syntax,
                        Some(sp),
                        String::from("cannot specify both FINAL and ABSTRACT"),
                    ));
                }
                seen_abstract = true;
            }
            ClassModifierKind::Public => {
                if seen_public {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0003Syntax,
                        Some(sp),
                        String::from("duplicate PUBLIC clause"),
                    ));
                } else if seen_internal {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0003Syntax,
                        Some(sp),
                        String::from("cannot specify both INTERNAL and PUBLIC"),
                    ));
                }
                seen_public = true;
            }
            ClassModifierKind::Internal => {
                if seen_internal {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0003Syntax,
                        Some(sp),
                        String::from("duplicate INTERNAL clause"),
                    ));
                } else if seen_public {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0003Syntax,
                        Some(sp),
                        String::from("cannot specify both INTERNAL and PUBLIC"),
                    ));
                }
                seen_internal = true;
            }
            ClassModifierKind::Partial => {
                if seen_partial {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0003Syntax,
                        Some(sp),
                        String::from("duplicate PARTIAL clause"),
                    ));
                }
                seen_partial = true;
            }
        }
    }
}

/// Extracts [`OoTypeInfo`] for a [`ClassUnit`].
///
/// # Arguments
///
/// * `cls` - Parsed [`ClassUnit`].
/// * `interner` - Symbol interner.
fn extract_class_type_info(cls: &ClassUnit, interner: &SymbolInterner) -> OoTypeInfo {
    let source_name = sym_orig(cls.class_id.name, interner);
    let ext_sym = cls.class_id.external_alias.unwrap_or(cls.class_id.name);
    let external_name = sym_orig(ext_sym, interner);
    let repo_map = raw_repo_ext_map(&cls.environment, interner);
    let parent_names = cls
        .class_id
        .inherits
        .iter()
        .map(|&p| {
            let pc = sym_canon(p, interner);
            repo_map
                .get(&pc)
                .cloned()
                .unwrap_or_else(|| sym_orig(p, interner))
        })
        .collect();

    let mut methods = BTreeMap::new();
    methods.insert(
        (HirOoCompartment::Factory, String::from("NEW")),
        OoMethodSig {
            compartment: HirOoCompartment::Factory,
            param_optional: Vec::new(),
            has_returning: true,
            return_cat: DataCategory::ObjectReference,
            return_ty: HirType::ObjectRef {
                class_sym: Some(cls.class_id.name),
            },
        },
    );
    if let Some(fac) = &cls.factory {
        for m in &fac.methods {
            let (mname, sig) = extract_method_sig(m, HirOoCompartment::Factory, interner);
            methods.insert((HirOoCompartment::Factory, mname), sig);
        }
    }
    let mut object_ws_entries = Vec::new();
    if let Some(obj) = &cls.object {
        object_ws_entries.clone_from(&obj.data.working_storage);
        for m in &obj.methods {
            let (mname, sig) = extract_method_sig(m, HirOoCompartment::Object, interner);
            methods.insert((HirOoCompartment::Object, mname), sig);
        }
    }
    OoTypeInfo {
        source_name,
        external_name,
        parent_names,
        methods,
        object_ws_entries,
    }
}

/// Extracts [`OoTypeInfo`] for an [`InterfaceUnit`].
///
/// # Arguments
///
/// * `iface` - Parsed [`InterfaceUnit`].
/// * `interner` - Symbol interner.
fn extract_interface_type_info(iface: &InterfaceUnit, interner: &SymbolInterner) -> OoTypeInfo {
    let source_name = sym_orig(iface.interface_id.name, interner);
    let ext_sym = iface
        .interface_id
        .external_alias
        .unwrap_or(iface.interface_id.name);
    let external_name = sym_orig(ext_sym, interner);
    let repo_map = raw_repo_ext_map(&iface.environment, interner);
    let parent_names = iface
        .interface_id
        .inherits
        .iter()
        .map(|&p| {
            let pc = sym_canon(p, interner);
            repo_map
                .get(&pc)
                .cloned()
                .unwrap_or_else(|| sym_orig(p, interner))
        })
        .collect();
    let mut methods = BTreeMap::new();
    for m in &iface.methods {
        let (mname, sig) = extract_method_sig(m, HirOoCompartment::Object, interner);
        methods.insert((HirOoCompartment::Object, mname), sig);
    }
    OoTypeInfo {
        source_name,
        external_name,
        parent_names,
        methods,
        object_ws_entries: Vec::new(),
    }
}

/// Extracts `(UPPERCASE_METHOD_NAME, OoMethodSig)` from an AST [`MethodUnit`].
///
/// # Arguments
///
/// * `m` - Parsed [`MethodUnit`].
/// * `compartment` - Owning `Factory` or `Object` compartment.
/// * `interner` - Symbol interner.
fn extract_method_sig(
    m: &MethodUnit,
    compartment: HirOoCompartment,
    interner: &SymbolInterner,
) -> (String, OoMethodSig) {
    let ext_sym = m.method_id.external_alias.unwrap_or(m.method_id.name);
    let mname = match m.method_id.property_role {
        MethodPropertyRole::StandardMethod => sym_canon(ext_sym, interner),
        MethodPropertyRole::GetProperty => format!("__GET_{}", sym_canon(ext_sym, interner)),
        MethodPropertyRole::SetProperty => format!("__SET_{}", sym_canon(ext_sym, interner)),
    };
    let param_optional = m
        .procedure
        .header
        .using_params
        .iter()
        .map(|p| p.optionality == ParamOptionality::Optional)
        .collect();
    let (has_returning, return_cat, return_ty) = match m.procedure.header.returning {
        Some(rsym) => {
            let (cat, ty) = infer_linkage_item_type(&m.data, rsym, interner);
            (true, cat, ty)
        }
        None => (
            false,
            DataCategory::Alphanumeric,
            HirType::Alphanumeric {
                byte_len: 1,
                justified: coblang_hir::JustifiedSpec::Left,
            },
        ),
    };
    (
        mname,
        OoMethodSig {
            compartment,
            param_optional,
            has_returning,
            return_cat,
            return_ty,
        },
    )
}

/// Infers `(DataCategory, HirType)` of a `RETURNING` item from AST `LINKAGE SECTION`.
///
/// # Arguments
///
/// * `data` - Method [`DataDivision`].
/// * `rsym` - `RETURNING` item [`SymbolId`].
/// * `interner` - Symbol interner.
fn infer_linkage_item_type(
    data: &DataDivision,
    rsym: SymbolId,
    interner: &SymbolInterner,
) -> (DataCategory, HirType) {
    for entry in &data.linkage {
        if matches!(entry.name, DataEntryName::Named(s) if s == rsym) {
            if let Some(coblang_syntax::ast::UsageKind::ObjectReference(cls)) = entry.usage {
                return (
                    DataCategory::ObjectReference,
                    HirType::ObjectRef { class_sym: cls },
                );
            }
            if let Some((pic_sym, pic_span)) = entry.picture {
                let raw_pic = sym_orig(pic_sym, interner);
                let clauses = crate::ElementaryClauses {
                    raw_pic: Some((&raw_pic, pic_span)),
                    entry_span: entry.span,
                    pic_ctx: PictureContext::default(),
                    usage: entry.usage.as_ref(),
                    sign_clause: entry.sign,
                    justified: entry.justified,
                    blank_zero: entry.blank_when_zero,
                };
                let mut dummy_sink = DiagnosticSink::new(WarningPolicy::default(), 16);
                let ty = crate::compile_elementary_type(
                    &clauses,
                    &DialectConfig::for_dialect(Dialect::Cobol2023),
                    &mut dummy_sink,
                );
                return (ty.category(), ty);
            }
            if entry.usage.is_some() {
                let clauses = crate::ElementaryClauses {
                    raw_pic: None,
                    entry_span: entry.span,
                    pic_ctx: PictureContext::default(),
                    usage: entry.usage.as_ref(),
                    sign_clause: entry.sign,
                    justified: entry.justified,
                    blank_zero: entry.blank_when_zero,
                };
                let mut dummy_sink = DiagnosticSink::new(WarningPolicy::default(), 16);
                let ty = crate::compile_elementary_type(
                    &clauses,
                    &DialectConfig::for_dialect(Dialect::Cobol2023),
                    &mut dummy_sink,
                );
                return (ty.category(), ty);
            }
        }
    }
    (
        DataCategory::Numeric,
        HirType::BinaryInt {
            byte_width: 8,
            digits: coblang_common::DigitCount::try_new(18)
                .unwrap_or(coblang_common::DigitCount::ONE),
            scale: coblang_common::ScaleFactor::ZERO,
            sign: coblang_hir::Signedness::Signed,
            endian: coblang_hir::BinaryEndianness::Native,
            trunc: coblang_hir::TruncationPolicy::FullBinaryRange,
        },
    )
}

/// Maps canonical internal `REPOSITORY` class/interface names to external names.
///
/// # Arguments
///
/// * `env_div` - Unit [`EnvironmentDivision`].
/// * `interner` - Symbol interner.
fn raw_repo_ext_map(
    env_div: &EnvironmentDivision,
    interner: &SymbolInterner,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let repo = &env_div.configuration.repository;
    for &(int_sym, alias_opt) in repo.classes.iter().chain(repo.interfaces.iter()) {
        let ext_sym = alias_opt.unwrap_or(int_sym);
        map.insert(sym_canon(int_sym, interner), sym_orig(ext_sym, interner));
    }
    map
}

/// Validates `INHERITS` parent list (`CLASS` or `INTERFACE`).
///
/// # Arguments
///
/// * `header_spec` - `(kind_kw, self_name, inherits, spans)` tuple for the `INHERITS` header.
/// * `repo` - Validated unit [`OoRepoBindings`].
/// * `catalog` - Group-wide [`OoGroupCatalog`].
/// * `interner` - Symbol interner.
/// * `sink` - Diagnostic sink.
fn validate_inherits_clause(
    header_spec: (&str, SymbolId, &[SymbolId], &[SourceSpan]),
    repo: &OoRepoBindings,
    catalog: &OoGroupCatalog,
    interner: &SymbolInterner,
    sink: &mut DiagnosticSink,
) {
    let (kind_kw, self_name, inherits, spans) = header_spec;
    let self_canon = sym_canon(self_name, interner);
    let mut seen = BTreeSet::new();
    for (idx, &parent_sym) in inherits.iter().enumerate() {
        let sp = spans.get(idx).copied();
        let p_orig = sym_orig(parent_sym, interner);
        let p_canon = p_orig.to_ascii_uppercase();
        if !seen.insert(p_canon.clone()) {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                sp,
                format!("duplicate parent {kind_kw} '{p_orig}'"),
            ));
            continue;
        }
        let in_repo = match repo.by_internal.get(&p_canon) {
            Some(OoRepoTarget::Class(_)) => kind_kw == "CLASS",
            Some(OoRepoTarget::Interface(_)) => kind_kw == "INTERFACE",
            Some(OoRepoTarget::WrongFunction(_)) | None => false,
        };
        let parent_ext = match repo.by_internal.get(&p_canon) {
            Some(OoRepoTarget::Class(e) | OoRepoTarget::Interface(e)) => e.as_str(),
            _ => p_orig.as_str(),
        };
        if p_canon == self_canon || has_inheritance_cycle(&self_canon, parent_ext, catalog) {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                sp,
                format!("cyclically inheriting {kind_kw} '{p_orig}'"),
            ));
        }
        if !in_repo {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0004UndefinedSymbol,
                sp,
                format!("inherited {kind_kw} '{p_orig}' not found in REPOSITORY paragraph"),
            ));
        }
    }
}

/// Detects whether `candidate_parent` reaches `self_canon` via `INHERITS`.
///
/// # Arguments
///
/// * `self_canon` - Uppercase canonical name of the defining class/interface.
/// * `candidate_parent` - Parent external name to start traversal from.
/// * `catalog` - Group-wide [`OoGroupCatalog`].
fn has_inheritance_cycle(
    self_canon: &str,
    candidate_parent: &str,
    catalog: &OoGroupCatalog,
) -> bool {
    let mut stack = vec![candidate_parent.to_ascii_uppercase()];
    let mut visited = BTreeSet::new();
    while let Some(cur) = stack.pop() {
        if cur == self_canon {
            return true;
        }
        if !visited.insert(cur.clone()) {
            continue;
        }
        if let Some(info) = catalog
            .classes_by_ext
            .get(&cur)
            .or_else(|| catalog.interfaces_by_ext.get(&cur))
        {
            for p in &info.parent_names {
                stack.push(p.to_ascii_uppercase());
            }
        }
    }
    false
}

/// Lowers an OO `CLASS-ID` unit into `module.classes` (`Milestone M20`).
///
/// # Arguments
/// * `cls` - AST [`ClassUnit`].
/// * `module` - Output [`HirModule`].
/// * `env` - Group lowering environment.
///
/// # Errors
/// Returns [`coblang_common::CoblangError`] if symbol interning fails.
pub(crate) fn lower_class_unit(
    cls: &ClassUnit,
    module: &mut HirModule,
    env: &mut GroupLowerEnv<'_>,
) -> Result<()> {
    let oo_enabled = oo_supported_in_dialect(env.dialect);
    if !oo_enabled {
        env.sink.emit(Diagnostic::error(
            DiagnosticCode::E0009DialectUnconformable,
            Some(cls.class_id.span),
            String::from("object-oriented COBOL is not supported"),
        ));
    }
    let repo =
        validate_and_bind_repository(&cls.environment, &env.oo_catalog, env.interner, env.sink);
    for _ in 0..2 {
        validate_inherits_clause(
            (
                "CLASS",
                cls.class_id.name,
                &cls.class_id.inherits,
                &cls.class_id.inherit_spans,
            ),
            &repo,
            &env.oo_catalog,
            env.interner,
            env.sink,
        );
    }

    let ext_sym = cls.class_id.external_alias.unwrap_or(cls.class_id.name);
    let external_name = sym_orig(ext_sym, env.interner);
    let parent_externals: Vec<String> = cls
        .class_id
        .inherits
        .iter()
        .map(
            |&p| match repo.by_internal.get(&sym_canon(p, env.interner)) {
                Some(OoRepoTarget::Class(e)) => e.clone(),
                _ => sym_orig(p, env.interner),
            },
        )
        .collect();

    let all_obj_ws = if oo_enabled {
        env.oo_catalog.collect_object_ws_hierarchy(&external_name)
    } else {
        Vec::new()
    };
    let (instance_byte_size, instance_template_items) =
        solve_instance_template(&all_obj_ws, cls.span, env)?;
    let (methods, method_specs) = lower_class_compartments(
        cls,
        oo_enabled,
        &external_name,
        &parent_externals,
        &all_obj_ws,
        &repo,
        env,
    )?;

    module.classes.push(HirClass {
        name: cls.class_id.name,
        inherits: cls.class_id.inherits.clone(),
        methods,
        oo_spec: oo_enabled.then_some(HirOoClassSpec {
            external_name,
            parent_externals,
            instance_byte_size,
            instance_template_items,
            method_specs,
        }),
        span: cls.span,
    });
    Ok(())
}

/// Lowers all `FACTORY` and `OBJECT` methods of `cls`.
///
/// # Arguments
///
/// * `cls` - AST [`ClassUnit`].
/// * `oo_enabled` - Whether OO COBOL is enabled in the active dialect.
/// * `external_name` - Class external linkage name.
/// * `parent_externals` - Ordered parent external names.
/// * `all_obj_ws` - Flattened inherited + own `OBJECT` `WORKING-STORAGE` entries.
/// * `repo` - Class-level [`OoRepoBindings`].
/// * `env` - Group lowering environment.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if method lowering fails.
fn lower_class_compartments(
    cls: &ClassUnit,
    oo_enabled: bool,
    external_name: &str,
    parent_externals: &[String],
    all_obj_ws: &[DataEntry],
    repo: &OoRepoBindings,
    env: &mut GroupLowerEnv<'_>,
) -> Result<(Vec<HirProgram>, Vec<HirOoMethodSpec>)> {
    let source_class_name = sym_orig(cls.class_id.name, env.interner);
    let mut methods = Vec::new();
    let mut method_specs = Vec::new();

    if let Some(fac) = &cls.factory {
        for m in &fac.methods {
            let m_env = OoActiveMethodEnv {
                source_class_name: source_class_name.clone(),
                external_class_name: external_name.to_owned(),
                compartment: HirOoCompartment::Factory,
                parent_externals: parent_externals.to_vec(),
                parent_count: cls.class_id.inherits.len(),
            };
            let mut prog = lower_one_oo_method(
                m,
                &cls.environment,
                oo_enabled.then_some(&fac.data.working_storage),
                None,
                repo,
                m_env,
                env,
            )?;
            let idx = methods.len();
            let ret_len = promote_oo_method_returning(&mut prog, oo_enabled);
            method_specs.push(HirOoMethodSpec {
                compartment: HirOoCompartment::Factory,
                method_name: resolve_method_ext_name(m, env.interner),
                method_index: idx,
                return_byte_len: ret_len,
            });
            methods.push(prog);
        }
    }

    if let Some(obj) = &cls.object {
        for m in &obj.methods {
            let m_env = OoActiveMethodEnv {
                source_class_name: source_class_name.clone(),
                external_class_name: external_name.to_owned(),
                compartment: HirOoCompartment::Object,
                parent_externals: parent_externals.to_vec(),
                parent_count: cls.class_id.inherits.len(),
            };
            let mut prog = lower_one_oo_method(
                m,
                &cls.environment,
                None,
                oo_enabled.then_some(all_obj_ws),
                repo,
                m_env,
                env,
            )?;
            let idx = methods.len();
            let ret_len = promote_oo_method_returning(&mut prog, oo_enabled);
            method_specs.push(HirOoMethodSpec {
                compartment: HirOoCompartment::Object,
                method_name: resolve_method_ext_name(m, env.interner),
                method_index: idx,
                return_byte_len: ret_len,
            });
            methods.push(prog);
        }
    }
    Ok((methods, method_specs))
}

/// Lowers an OO `INTERFACE-ID` unit into `module.classes` (`Milestone M20`).
///
/// # Arguments
/// * `iface` - AST [`InterfaceUnit`].
/// * `module` - Output [`HirModule`].
/// * `env` - Group lowering environment.
///
/// # Errors
/// Returns [`coblang_common::CoblangError`] if symbol interning fails.
pub(crate) fn lower_interface_unit(
    iface: &InterfaceUnit,
    module: &mut HirModule,
    env: &mut GroupLowerEnv<'_>,
) -> Result<()> {
    if !oo_supported_in_dialect(env.dialect) {
        env.sink.emit(Diagnostic::error(
            DiagnosticCode::E0009DialectUnconformable,
            Some(iface.interface_id.span),
            String::from("object-oriented COBOL (INTERFACE) is not supported"),
        ));
    }
    let repo =
        validate_and_bind_repository(&iface.environment, &env.oo_catalog, env.interner, env.sink);
    validate_inherits_clause(
        (
            "INTERFACE",
            iface.interface_id.name,
            &iface.interface_id.inherits,
            &iface.interface_id.inherit_spans,
        ),
        &repo,
        &env.oo_catalog,
        env.interner,
        env.sink,
    );

    let source_name = sym_orig(iface.interface_id.name, env.interner);
    let ext_sym = iface
        .interface_id
        .external_alias
        .unwrap_or(iface.interface_id.name);
    let ext_name = sym_orig(ext_sym, env.interner);

    let mut methods = Vec::new();
    for m in &iface.methods {
        let m_env = OoActiveMethodEnv {
            source_class_name: source_name.clone(),
            external_class_name: ext_name.clone(),
            compartment: HirOoCompartment::Object,
            parent_externals: Vec::new(),
            parent_count: iface.interface_id.inherits.len(),
        };
        let prog = lower_one_oo_method(m, &iface.environment, None, None, &repo, m_env, env)?;
        methods.push(prog);
    }
    module.classes.push(HirClass {
        name: iface.interface_id.name,
        inherits: iface.interface_id.inherits.clone(),
        methods,
        oo_spec: None,
        span: iface.span,
    });
    Ok(())
}

/// Solves a standalone layout for `OBJECT` `WORKING-STORAGE` to obtain instance byte size & initial `VALUE`s.
///
/// # Arguments
///
/// * `obj_ws` - Flattened `OBJECT` `WORKING-STORAGE` entries.
/// * `span` - Class span.
/// * `env` - Group lowering environment.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if template unit lowering fails.
fn solve_instance_template(
    obj_ws: &[DataEntry],
    span: SourceSpan,
    env: &mut GroupLowerEnv<'_>,
) -> Result<(u32, Vec<HirDataItem>)> {
    if obj_ws.is_empty() {
        return Ok((8, Vec::new()));
    }
    let mut dd = DataDivision::default();
    let self_sym = env.interner.intern("__OO_INSTANCE_TEMPLATE")?;
    dd.working_storage =
        wrap_entries_under_group(self_sym, ResidencyKind::AutomaticOrStatic, obj_ws, span);
    let empty_env = EnvironmentDivision::default();
    let empty_proc = ProcedureDivision::default();
    let parts = UnitAstSlice {
        name: self_sym,
        attrs: ProgramAttributes {
            common: CommonVisibility::PrivateToParent,
            initial: InitialState::Initial,
            recursive: Recursivity::NonRecursive,
            prototype: PrototypeKind::Concrete,
        },
        env: &empty_env,
        data: &dd,
        proc_div: &empty_proc,
        span,
    };
    let prog = lower_unit_parts(&parts, None, env)?;
    let total = prog
        .data_items
        .iter()
        .find(|i| i.name == Some(self_sym))
        .map_or(8, |i| i.total_byte_size.max(8));
    Ok((total, prog.data_items))
}

/// Wraps compartment `01`/`77` entries under a synthesized `01` group (`AutomaticOrStatic` or `Based`).
///
/// # Arguments
///
/// * `group_sym` - Synthesized `01` group [`SymbolId`].
/// * `residency` - [`ResidencyKind`] (`AutomaticOrStatic` or `Based`).
/// * `entries` - Subordinate `DATA DIVISION` entries.
/// * `span` - Synthetic group span.
fn wrap_entries_under_group(
    group_sym: SymbolId,
    residency: ResidencyKind,
    entries: &[DataEntry],
    span: SourceSpan,
) -> Vec<DataEntry> {
    let mut out = Vec::with_capacity(entries.len() + 1);
    out.push(DataEntry {
        level: LevelNumber::Hierarchy(1),
        name: DataEntryName::Named(group_sym),
        redefines: None,
        renames: None,
        picture: None,
        usage: None,
        sign: None,
        occurs: None,
        sync: SyncAlignment::Unsynchronized,
        justified: JustificationMode::StandardLeft,
        blank_when_zero: BlankWhenZeroMode::Normal,
        value: None,
        flags: StorageLinkageFlags {
            external: ExternalLinkage::Internal,
            global: coblang_syntax::ast::GlobalVisibility::Local,
            residency,
            length_mode: coblang_syntax::ast::LengthVariability::Fixed,
            type_role: coblang_syntax::ast::TypeRole::DataInstance,
            special_names_role: None,
        },
        clause_counts: DataClauseCounts::default(),
        span,
    });
    for e in entries {
        let mut cloned = e.clone();
        let raw_lvl = e.level.as_u8();
        if raw_lvl == 77 || raw_lvl == 1 {
            cloned.level = LevelNumber::Hierarchy(2);
        } else if (2..49).contains(&raw_lvl) {
            cloned.level = LevelNumber::Hierarchy(raw_lvl + 1);
        }
        out.push(cloned);
    }
    out
}

/// Lowers a single `METHOD-ID` inside a class or interface.
///
/// # Arguments
///
/// * `m` - AST [`MethodUnit`].
/// * `class_env` - Enclosing class/interface [`EnvironmentDivision`].
/// * `factory_ws` - Optional `FACTORY` `WORKING-STORAGE` entries.
/// * `object_ws` - Optional `OBJECT` `WORKING-STORAGE` entries.
/// * `class_repo` - Enclosing [`OoRepoBindings`].
/// * `m_env` - Active [`OoActiveMethodEnv`].
/// * `env` - Group lowering environment.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if symbol interning or unit lowering fails.
fn lower_one_oo_method(
    m: &MethodUnit,
    class_env: &EnvironmentDivision,
    factory_ws: Option<&[DataEntry]>,
    object_ws: Option<&[DataEntry]>,
    class_repo: &OoRepoBindings,
    m_env: OoActiveMethodEnv,
    env: &mut GroupLowerEnv<'_>,
) -> Result<HirProgram> {
    let mut merged_data = m.data.clone();
    let mut self_based_sym = None;

    if let Some(fws) = factory_ws
        && !fws.is_empty()
    {
        let mut ext_entries = Vec::with_capacity(fws.len());
        for item in fws {
            let mut cloned = item.clone();
            if matches!(item.level.as_u8(), 1 | 77)
                && let DataEntryName::Named(nm) = item.name
            {
                let ext_tag = format!(
                    "__OO_FAC_{}_{}",
                    m_env.external_class_name,
                    sym_canon(nm, env.interner)
                );
                let alias_sym = env.interner.intern(&ext_tag)?;
                cloned.flags.external = ExternalLinkage::External(Some(alias_sym));
            }
            ext_entries.push(cloned);
        }
        ext_entries.append(&mut merged_data.working_storage);
        merged_data.working_storage = ext_entries;
    }

    if let Some(ows) = object_ws
        && !ows.is_empty()
    {
        let bsym = env.interner.intern("__OO_INSTANCE_SELF")?;
        self_based_sym = Some(bsym);
        let mut wrapped = wrap_entries_under_group(bsym, ResidencyKind::Based, ows, m.span);
        wrapped.append(&mut merged_data.working_storage);
        merged_data.working_storage = wrapped;
    }

    let mut effective_repo = class_repo.clone();
    if !m.environment.configuration.repository.classes.is_empty()
        || !m.environment.configuration.repository.interfaces.is_empty()
    {
        let m_repo =
            validate_and_bind_repository(&m.environment, &env.oo_catalog, env.interner, env.sink);
        for (k, v) in m_repo.by_internal {
            let _ = effective_repo.by_internal.insert(k, v);
        }
    }

    let mut merged_env = m.environment.clone();
    if merged_env.configuration.repository.classes.is_empty()
        && merged_env.configuration.repository.interfaces.is_empty()
    {
        merged_env.configuration.repository = class_env.configuration.repository.clone();
    }

    let prev_repo = env.active_oo_repo.replace(effective_repo);
    let prev_method = env.active_oo_method.replace(m_env);

    let parts = UnitAstSlice {
        name: m.method_id.name,
        attrs: ProgramAttributes {
            common: CommonVisibility::PrivateToParent,
            initial: InitialState::Initial,
            recursive: Recursivity::Recursive,
            prototype: PrototypeKind::Concrete,
        },
        env: &merged_env,
        data: &merged_data,
        proc_div: &m.procedure,
        span: m.span,
    };
    let mut prog = lower_unit_parts(&parts, None, env)?;

    env.active_oo_repo = prev_repo;
    env.active_oo_method = prev_method;

    if let Some(ret_id) = prog.returning_item
        && let Some(ret_item) = prog.data_items.get(ret_id.as_u32() as usize)
        && ret_item.section != HirStorageSection::Linkage
    {
        let hdr_sp = m.procedure.span.unwrap_or(m.span);
        env.sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(hdr_sp),
            String::from("RETURNING item is not defined in LINKAGE SECTION"),
        ));
    }

    if let Some(bsym) = self_based_sym
        && let Some(root_item) = prog.data_items.iter().find(|i| i.name == Some(bsym))
    {
        prog.entry_statements.insert(
            0,
            HirStmt::M20(Box::new(HirM20Stmt::OoBindSelfStorage {
                based_root: root_item.id,
                span: m.span,
            })),
        );
    }

    Ok(prog)
}

/// Computes `RETURNING` byte size and binds `returning_item` as a trailing `LINKAGE` parameter on OO methods.
///
/// # Arguments
///
/// * `prog` - Lowered method [`HirProgram`].
/// * `oo_enabled` - Whether OO COBOL is enabled in the active dialect.
fn promote_oo_method_returning(prog: &mut HirProgram, oo_enabled: bool) -> u32 {
    let Some(ret_id) = prog.returning_item else {
        return 0;
    };
    let Some(item) = prog.data_items.get(ret_id.as_u32() as usize) else {
        return 0;
    };
    let len = item.total_byte_size;
    if oo_enabled && item.section == HirStorageSection::Linkage {
        prog.using_params.push(coblang_hir::HirUsingParam {
            source: coblang_hir::HirParamSource::Using,
            mode: coblang_hir::HirPassingMode::ByReference,
            optionality: coblang_hir::HirParamOptionality::Optional,
            length_mode: coblang_hir::HirParamLengthMode::Fixed,
            item: ret_id,
            span: prog.span,
        });
        prog.returning_item = None;
    }
    len
}

/// Resolves the external method name of `m`.
///
/// # Arguments
///
/// * `m` - AST [`MethodUnit`].
/// * `interner` - Symbol interner.
fn resolve_method_ext_name(m: &MethodUnit, interner: &SymbolInterner) -> String {
    let ext_sym = m.method_id.external_alias.unwrap_or(m.method_id.name);
    match m.method_id.property_role {
        MethodPropertyRole::StandardMethod => sym_orig(ext_sym, interner),
        MethodPropertyRole::GetProperty => format!("__GET_{}", sym_canon(ext_sym, interner)),
        MethodPropertyRole::SetProperty => format!("__SET_{}", sym_canon(ext_sym, interner)),
    }
}

/// Intercepts `SELF` and `SUPER` data references inside [`crate::lower_hir::lower_expr_with_cat`].
///
/// # Arguments
/// * `dref` - AST [`DataRef`].
/// * `ctx` - Unit lowering context.
pub(crate) fn try_lower_self_or_super_data(
    dref: &DataRef,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<(HirExpr, DataCategory)> {
    if !dref.name.qualifiers.is_empty()
        || !dref.subscripts.is_empty()
        || dref.refmod.is_some()
        || dref.modifier != DataRefModifier::Plain
    {
        return None;
    }
    let word = ctx.interner.resolve_canonical(dref.name.base).ok()?;
    if word != "SELF" && word != "SUPER" {
        return None;
    }
    if let Some(menv) = ctx.oo_method {
        if word == "SUPER" && menv.parent_count > 1 {
            ctx.sink.emit(Diagnostic::warning(
                DiagnosticCode::W0006GeneralWarning,
                WarningGroup::CallParams,
                Some(dref.span),
                String::from(
                    "use of SUPER in CLASS or INTERFACE with multiple inheritance may be ambiguous",
                ),
            ));
        }
    } else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0004UndefinedSymbol,
            Some(dref.span),
            format!("use of {word} outside of METHOD definition"),
        ));
    }
    Some((
        HirExpr::M20(Box::new(HirM20Expr::OoSelf { span: dref.span })),
        DataCategory::ObjectReference,
    ))
}

/// Emits an invalid `DISPLAY` operand error when an `OBJECT REFERENCE` or `SELF` is displayed.
///
/// # Arguments
/// * `op` - Displayed AST [`Expr`].
/// * `ctx` - Unit lowering context.
pub(crate) fn emit_invalid_display_object_ref(op: &Expr, ctx: &mut UnitLowerCtx<'_>) {
    let label = if let Some(menv) = ctx.oo_method {
        menv.source_class_name.clone()
    } else {
        String::from("OBJECT REFERENCE")
    };
    ctx.sink.emit(Diagnostic::error(
        DiagnosticCode::E0008TypeMismatch,
        Some(op.span()),
        format!("'{label}' is an invalid type for DISPLAY operand"),
    ));
}

/// Lowers `Stmt::Invoke` into [`HirStmt::M20`] (`HirM20Stmt::OoInvoke`).
///
/// # Arguments
/// * `receiver` - Receiver AST expression.
/// * `method` - Method name expression.
/// * `args` - `USING` arguments.
/// * `returning` - Optional `RETURNING` target.
/// * `span` - Statement span.
/// * `out` - Output HIR statements.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_invoke_stmt(
    receiver: &Expr,
    method: &Expr,
    args: &[CallArg],
    returning: Option<&DataRef>,
    span: SourceSpan,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let Some((hir_recv, target_class, pref_comp)) = resolve_oo_receiver(receiver, ctx) else {
        let _ = lower_expr_with_cat(method, ctx);
        for a in args {
            if let CallArg::Value { expr, .. } = a {
                let _ = lower_expr_with_cat(expr, ctx);
            }
        }
        if let Some(r) = returning {
            let _ = resolve_place(r, ctx);
        }
        return;
    };

    let (m_hir, _) = lower_expr_with_cat(method, ctx);
    let lowered_args = lower_oo_call_args(args, ctx);
    let ret_place = returning.and_then(|r| resolve_place(r, ctx));

    if let Some(cls_ext) = &target_class
        && let Some(m_canon) = extract_literal_method_canon(method, ctx.interner)
        && let Some(sig) = ctx.oo_catalog.lookup_method(cls_ext, &m_canon, pref_comp)
    {
        validate_oo_call_args(sig, args, span, ctx.sink);
        if sig.has_returning && returning.is_none() {
            ctx.sink.emit(Diagnostic::warning(
                DiagnosticCode::W0006GeneralWarning,
                WarningGroup::CallParams,
                Some(span),
                String::from("expecting a RETURNING item, but none provided"),
            ));
        }
    }

    out.push(HirStmt::M20(Box::new(HirM20Stmt::OoInvoke(Box::new(
        HirOoInvokeStmt {
            receiver: hir_recv,
            method_name: m_hir,
            args: lowered_args,
            returning: ret_place,
            span,
        },
    )))));
}

/// Lowers `Expr::InlineInvoke` (`receiver :: "method" (args...)`) into [`HirExpr::M20`].
///
/// # Arguments
/// * `receiver` - Receiver AST expression.
/// * `method` - Method [`SymbolId`].
/// * `args` - Invocation arguments.
/// * `span` - Expression span.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_inline_invoke_expr(
    receiver: &Expr,
    method: SymbolId,
    args: &[CallArg],
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> (HirExpr, DataCategory) {
    let default_ty = HirType::BinaryInt {
        byte_width: 8,
        digits: coblang_common::DigitCount::try_new(18).unwrap_or(coblang_common::DigitCount::ONE),
        scale: coblang_common::ScaleFactor::ZERO,
        sign: coblang_hir::Signedness::Signed,
        endian: coblang_hir::BinaryEndianness::Native,
        trunc: coblang_hir::TruncationPolicy::FullBinaryRange,
    };
    let Some((hir_recv, target_class, pref_comp)) = resolve_oo_receiver(receiver, ctx) else {
        let _ = lower_oo_call_args(args, ctx);
        return (
            HirExpr::M20(Box::new(HirM20Expr::OoSelf { span })),
            DataCategory::Numeric,
        );
    };

    let lowered_args = lower_oo_call_args(args, ctx);
    let m_orig = sym_orig(method, ctx.interner);
    let m_canon = m_orig.to_ascii_uppercase();
    let method_name = HirExpr::Const {
        value: coblang_hir::HirConstValue::Bytes(m_orig.into_bytes()),
        span,
    };

    let mut ret_cat = DataCategory::Numeric;
    let mut ret_ty = default_ty;
    if let Some(cls_ext) = &target_class
        && let Some(sig) = ctx.oo_catalog.lookup_method(cls_ext, &m_canon, pref_comp)
    {
        validate_oo_call_args(sig, args, span, ctx.sink);
        if sig.has_returning {
            ret_cat = sig.return_cat;
            ret_ty = sig.return_ty.clone();
        } else {
            ctx.sink.emit(Diagnostic::warning(
                DiagnosticCode::W0006GeneralWarning,
                WarningGroup::CallParams,
                Some(span),
                String::from("invalid inline invocation of a method with no RETURNING"),
            ));
        }
    }

    (
        HirExpr::M20(Box::new(HirM20Expr::OoInlineInvoke(Box::new(
            HirOoInlineInvokeExpr {
                receiver: hir_recv,
                method_name,
                args: lowered_args,
                return_ty: ret_ty,
                span,
            },
        )))),
        ret_cat,
    )
}

/// Resolved `(HirOoReceiver, optional_ext_class, optional_compartment)` triple.
type ResolvedOoReceiver = (HirOoReceiver, Option<String>, Option<HirOoCompartment>);

/// Resolves a plain `SELF`, `SUPER`, or `REPOSITORY` class identifier receiver if matched.
///
/// # Arguments
///
/// * `dref` - Unqualified plain [`DataRef`].
/// * `ctx` - Unit lowering context.
fn try_resolve_plain_oo_word(
    dref: &DataRef,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<Option<ResolvedOoReceiver>> {
    let word = sym_canon(dref.name.base, ctx.interner);
    if word == "SELF" {
        if let Some(menv) = ctx.oo_method {
            return Some(Some((
                HirOoReceiver::SelfRef,
                Some(menv.external_class_name.clone()),
                Some(menv.compartment),
            )));
        }
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0004UndefinedSymbol,
            Some(dref.span),
            String::from("use of SELF outside of METHOD definition"),
        ));
        let cls = ctx
            .oo_catalog
            .classes_by_ext
            .values()
            .next()
            .map(|c| c.external_name.clone());
        return Some(Some((HirOoReceiver::SelfRef, cls, None)));
    }
    if word == "SUPER" {
        let Some(menv) = ctx.oo_method else {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0004UndefinedSymbol,
                Some(dref.span),
                String::from("use of SUPER outside of METHOD definition"),
            ));
            return Some(None);
        };
        if menv.parent_count > 1 {
            ctx.sink.emit(Diagnostic::warning(
                DiagnosticCode::W0006GeneralWarning,
                WarningGroup::CallParams,
                Some(dref.span),
                String::from(
                    "use of SUPER in CLASS or INTERFACE with multiple inheritance may be ambiguous",
                ),
            ));
        }
        let p_ext = menv.parent_externals.first().cloned().unwrap_or_default();
        let target_cls = (!p_ext.is_empty()).then(|| p_ext.clone());
        return Some(Some((
            HirOoReceiver::SuperRef {
                parent_external: p_ext,
            },
            target_cls,
            Some(menv.compartment),
        )));
    }
    if ctx
        .symtab
        .resolve_symbol_silent(ctx.scope, dref.name.base, &[])
        .is_none()
    {
        if let Some(target) = ctx.oo_repo.by_internal.get(&word) {
            return match target {
                OoRepoTarget::WrongFunction(fn_orig) => {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0008TypeMismatch,
                        Some(dref.span),
                        format!("CLASS or INTERFACE expected (got FUNCTION '{fn_orig}')"),
                    ));
                    Some(None)
                }
                OoRepoTarget::Class(ext) | OoRepoTarget::Interface(ext) => Some(Some((
                    HirOoReceiver::FactoryClass(ext.clone()),
                    Some(ext.clone()),
                    Some(HirOoCompartment::Factory),
                ))),
            };
        }
        if let Some(info) = ctx.oo_catalog.classes_by_ext.get(&word) {
            return Some(Some((
                HirOoReceiver::FactoryClass(info.external_name.clone()),
                Some(info.external_name.clone()),
                Some(HirOoCompartment::Factory),
            )));
        }
    }
    None
}

/// Resolves an OO receiver expression into `(HirOoReceiver, optional_ext_class, optional_compartment)`.
///
/// # Arguments
///
/// * `receiver` - Receiver AST expression.
/// * `ctx` - Unit lowering context.
fn resolve_oo_receiver(receiver: &Expr, ctx: &mut UnitLowerCtx<'_>) -> Option<ResolvedOoReceiver> {
    if let Expr::Data(dref) = receiver
        && dref.name.qualifiers.is_empty()
        && dref.subscripts.is_empty()
        && dref.refmod.is_none()
        && dref.modifier == DataRefModifier::Plain
        && let Some(outcome) = try_resolve_plain_oo_word(dref, ctx)
    {
        return outcome;
    }

    let (r_hir, _) = lower_expr_with_cat(receiver, ctx);
    let mut target_cls = None;
    if let HirExpr::Place(pl) = &r_hir
        && let HirType::ObjectRef {
            class_sym: Some(csym),
        } = pl.effective_ty
    {
        let c_canon = sym_canon(csym, ctx.interner);
        if c_canon == "ACTIVE-CLASS" {
            target_cls = ctx.oo_method.map(|m| m.external_class_name.clone());
        } else if let Some(target) = ctx.oo_repo.by_internal.get(&c_canon) {
            match target {
                OoRepoTarget::WrongFunction(fn_orig) => {
                    ctx.sink.emit(Diagnostic::error(
                        DiagnosticCode::E0008TypeMismatch,
                        Some(receiver.span()),
                        format!("CLASS or INTERFACE expected (got FUNCTION '{fn_orig}')"),
                    ));
                    return None;
                }
                OoRepoTarget::Class(ext) | OoRepoTarget::Interface(ext) => {
                    target_cls = Some(ext.clone());
                }
            }
        } else {
            target_cls = Some(sym_orig(csym, ctx.interner));
        }
    }
    Some((HirOoReceiver::ObjectExpr(r_hir), target_cls, None))
}

/// Validates OO call arguments against [`OoMethodSig`].
///
/// # Arguments
///
/// * `sig` - Resolved target [`OoMethodSig`].
/// * `args` - Supplied call arguments.
/// * `call_span` - Invocation statement/expression span.
/// * `sink` - Diagnostic sink.
fn validate_oo_call_args(
    sig: &OoMethodSig,
    args: &[CallArg],
    call_span: SourceSpan,
    sink: &mut DiagnosticSink,
) {
    for (idx, &is_opt) in sig.param_optional.iter().enumerate() {
        if !is_opt {
            match args.get(idx) {
                None => sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::CallParams,
                    Some(call_span),
                    format!("argument #{} is not optional", idx + 1),
                )),
                Some(CallArg::Omitted(sp)) => sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::CallParams,
                    Some(*sp),
                    format!("argument #{} is not optional", idx + 1),
                )),
                Some(CallArg::Value { .. }) => {}
            }
        }
    }
    if args.len() > sig.param_optional.len() {
        sink.emit(Diagnostic::warning(
            DiagnosticCode::W0006GeneralWarning,
            WarningGroup::CallParams,
            Some(call_span),
            format!(
                "expecting up to {} arguments, but found {}",
                sig.param_optional.len(),
                args.len()
            ),
        ));
    }
}

/// Lowers `CallArg` slice into `Vec<HirCallArg>`.
///
/// # Arguments
///
/// * `args` - AST [`CallArg`] slice.
/// * `ctx` - Unit lowering context.
fn lower_oo_call_args(args: &[CallArg], ctx: &mut UnitLowerCtx<'_>) -> Vec<HirCallArg> {
    args.iter()
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
        .collect()
}

/// Extracts canonical uppercase method name if `expr` is a string or identifier literal.
///
/// # Arguments
///
/// * `expr` - Method name expression.
/// * `interner` - Symbol interner.
fn extract_literal_method_canon(expr: &Expr, interner: &SymbolInterner) -> Option<String> {
    match expr {
        Expr::Literal {
            atom: coblang_syntax::ast::ValueAtom::String { bytes, .. },
            ..
        } => Some(String::from_utf8_lossy(bytes).to_ascii_uppercase()),
        Expr::Literal {
            atom: coblang_syntax::ast::ValueAtom::Identifier(s),
            ..
        } => Some(sym_canon(*s, interner)),
        _ => None,
    }
}

/// Borrowed AST parts of `Stmt::XmlGenerate`.
pub(crate) struct XmlGenParts<'a> {
    /// Destination XML buffer reference.
    pub destination: &'a DataRef,
    /// Source record reference.
    pub source: &'a DataRef,
    /// Optional `COUNT IN` reference.
    pub count_in: Option<&'a DataRef>,
    /// Optional `ENCODING` expression.
    pub encoding: Option<&'a Expr>,
    /// `XML-DECLARATION` presence.
    pub xml_declaration: XmlDeclPresence,
    /// `ATTRIBUTES` presence.
    pub attributes: XmlAttrPresence,
    /// Optional `NAMESPACE` URI expression.
    pub namespace: Option<&'a Expr>,
    /// Optional `NAMESPACE-PREFIX` expression.
    pub namespace_prefix: Option<&'a Expr>,
    /// `NAME OF` clauses.
    pub name_mappings: &'a [MarkupNameClause],
    /// `TYPE OF` clauses.
    pub type_mappings: &'a [XmlTypeClause],
    /// `SUPPRESS` clauses.
    pub suppress_clauses: &'a [MarkupSuppressClause],
    /// Exception handlers.
    pub handlers: &'a EventHandlers,
    /// Statement span.
    pub span: SourceSpan,
}

/// Lowers `Stmt::XmlGenerate` into [`HirStmt::M20`] (`HirM20Stmt::XmlGenerate`).
///
/// # Arguments
///
/// * `parts` - Borrowed [`XmlGenParts`].
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_xml_generate_stmt(
    parts: &XmlGenParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let dst_opt = resolve_place(parts.destination, ctx);
    let src_opt = resolve_place(parts.source, ctx);
    let src_root = src_opt.as_ref().map(|p| p.item);
    let cnt_opt = parts.count_in.and_then(|c| resolve_place(c, ctx));
    if let Some(enc) = parts.encoding {
        let _ = lower_expr_with_cat(enc, ctx);
    }
    let ns_hir = parts.namespace.map(|e| lower_expr_with_cat(e, ctx).0);
    let pfx_hir = parts
        .namespace_prefix
        .map(|e| lower_expr_with_cat(e, ctx).0);
    let name_overrides = lower_name_mappings(parts.name_mappings, src_root, ctx);
    let type_overrides = lower_xml_type_mappings(parts.type_mappings, src_root, ctx);
    let (field_suppress, generic_suppress) =
        lower_suppress_clauses(parts.suppress_clauses, src_root, ctx);
    let (on_exception, not_on_exception) = lower_event_handlers(parts.handlers, ctx);

    let (Some(destination), Some(source)) = (dst_opt, src_opt) else {
        return;
    };
    let dpc_comma = active_dpc_mode(parts.span, true, ctx);
    out.push(HirStmt::M20(Box::new(HirM20Stmt::XmlGenerate(Box::new(
        HirXmlGenerateStmt {
            destination,
            source,
            count_in: cnt_opt,
            xml_decl: match parts.xml_declaration {
                XmlDeclPresence::Omit => HirXmlDeclMode::Omit,
                XmlDeclPresence::Include => HirXmlDeclMode::Include,
            },
            attr_mode: match parts.attributes {
                XmlAttrPresence::ElementsOnly => HirXmlAttrMode::ElementsOnly,
                XmlAttrPresence::WithAttributes => HirXmlAttrMode::WithAttributes,
            },
            dpc_comma,
            namespace: ns_hir,
            namespace_prefix: pfx_hir,
            name_overrides,
            type_overrides,
            field_suppress,
            generic_suppress,
            on_exception,
            not_on_exception,
            span: parts.span,
        },
    )))));
}

/// Borrowed AST parts of `Stmt::XmlParse`.
pub(crate) struct XmlParseParts<'a> {
    /// Input XML expression.
    pub source: &'a Expr,
    /// Optional `ENCODING` expression.
    pub encoding: Option<&'a Expr>,
    /// Optional `VALIDATING` expression.
    pub validating: Option<&'a Expr>,
    /// `PROCESSING PROCEDURE (start, thru)`.
    pub processing_proc: &'a (QualifiedName, Option<QualifiedName>),
    /// Exception handlers.
    pub handlers: &'a EventHandlers,
    /// Statement span.
    pub span: SourceSpan,
}

/// Lowers `Stmt::XmlParse` into [`HirStmt::M20`] (`HirM20Stmt::XmlParse`).
///
/// # Arguments
///
/// * `parts` - Borrowed [`XmlParseParts`].
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_xml_parse_stmt(
    parts: &XmlParseParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let (src_hir, _) = lower_expr_with_cat(parts.source, ctx);
    if let Some(enc) = parts.encoding {
        let _ = lower_expr_with_cat(enc, ctx);
    }
    if let Some(val) = parts.validating {
        let _ = lower_expr_with_cat(val, ctx);
    }
    let bounds = resolve_perform_proc_bounds(
        &parts.processing_proc.0,
        parts.processing_proc.1.as_ref(),
        ctx,
    );
    let (on_exception, not_on_exception) = lower_event_handlers(parts.handlers, ctx);
    let Some((proc_start, proc_end)) = bounds else {
        return;
    };
    let stream_mode = match ctx.dialect.xml_parse_mode {
        XmlParseMode::Xmlss => HirXmlStreamMode::Xmlss,
        XmlParseMode::Compat => HirXmlStreamMode::Compat,
    };
    out.push(HirStmt::M20(Box::new(HirM20Stmt::XmlParse(Box::new(
        HirXmlParseStmt {
            source: src_hir,
            stream_mode,
            proc_start,
            proc_end,
            on_exception,
            not_on_exception,
            span: parts.span,
        },
    )))));
}

/// Borrowed AST parts of `Stmt::JsonGenerate`.
pub(crate) struct JsonGenParts<'a> {
    /// Destination JSON buffer reference.
    pub destination: &'a DataRef,
    /// Source COBOL item reference.
    pub source: &'a DataRef,
    /// Optional `COUNT IN` reference.
    pub count_in: Option<&'a DataRef>,
    /// `NAME OF` clauses.
    pub name_mappings: &'a [MarkupNameClause],
    /// `SUPPRESS` clauses.
    pub suppress_clauses: &'a [MarkupSuppressClause],
    /// `CONVERTING` boolean clauses.
    pub converting_clauses: &'a [JsonBoolClause],
    /// Exception handlers.
    pub handlers: &'a EventHandlers,
    /// Statement span.
    pub span: SourceSpan,
}

/// Lowers `Stmt::JsonGenerate` into [`HirStmt::M20`] (`HirM20Stmt::JsonGenerate`).
///
/// # Arguments
///
/// * `parts` - Borrowed [`JsonGenParts`].
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_json_generate_stmt(
    parts: &JsonGenParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let dst_opt = resolve_place(parts.destination, ctx);
    let src_opt = resolve_place(parts.source, ctx);
    let src_root = src_opt.as_ref().map(|p| p.item);
    let cnt_opt = parts.count_in.and_then(|c| resolve_place(c, ctx));
    let name_overrides = lower_name_mappings(parts.name_mappings, src_root, ctx);
    let (field_suppress, generic_suppress) =
        lower_suppress_clauses(parts.suppress_clauses, src_root, ctx);
    let bool_fields = lower_json_gen_bools(parts.converting_clauses, src_root, ctx);
    let (on_exception, not_on_exception) = lower_event_handlers(parts.handlers, ctx);

    let (Some(destination), Some(source)) = (dst_opt, src_opt) else {
        return;
    };
    let dpc_comma = active_dpc_mode(parts.span, false, ctx);
    out.push(HirStmt::M20(Box::new(HirM20Stmt::JsonGenerate(Box::new(
        HirJsonGenerateStmt {
            destination,
            source,
            count_in: cnt_opt,
            dpc_comma,
            name_overrides,
            field_suppress,
            generic_suppress,
            bool_fields,
            on_exception,
            not_on_exception,
            span: parts.span,
        },
    )))));
}

/// Borrowed AST parts of `Stmt::JsonParse`.
pub(crate) struct JsonParseParts<'a> {
    /// Input JSON text expression.
    pub source: &'a Expr,
    /// Target COBOL record reference.
    pub destination: &'a DataRef,
    /// `WITH DETAIL` presence.
    pub detail: JsonDetailPresence,
    /// `NAME OF` clauses.
    pub name_mappings: &'a [MarkupNameClause],
    /// `SUPPRESS` clauses.
    pub suppress_clauses: &'a [MarkupSuppressClause],
    /// `CONVERTING` boolean clauses.
    pub converting_clauses: &'a [JsonBoolClause],
    /// Exception handlers.
    pub handlers: &'a EventHandlers,
    /// Statement span.
    pub span: SourceSpan,
}

/// Lowers `Stmt::JsonParse` into [`HirStmt::M20`] (`HirM20Stmt::JsonParse`).
///
/// # Arguments
///
/// * `parts` - Borrowed [`JsonParseParts`].
/// * `out` - Output [`HirStmt`] accumulator.
/// * `ctx` - Unit lowering context.
pub(crate) fn lower_json_parse_stmt(
    parts: &JsonParseParts<'_>,
    out: &mut Vec<HirStmt>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let (src_hir, _) = lower_expr_with_cat(parts.source, ctx);
    let dst_opt = resolve_place(parts.destination, ctx);
    let dst_root = dst_opt.as_ref().map(|p| p.item);
    let name_overrides = lower_name_mappings(parts.name_mappings, dst_root, ctx);
    let (field_suppress, _) = lower_suppress_clauses(parts.suppress_clauses, dst_root, ctx);
    let suppressed_fields = field_suppress.into_iter().map(|(id, _)| id).collect();
    let bool_fields = lower_json_parse_bools(parts.converting_clauses, dst_root, ctx);
    let (on_exception, not_on_exception) = lower_event_handlers(parts.handlers, ctx);

    let Some(destination) = dst_opt else {
        return;
    };
    out.push(HirStmt::M20(Box::new(HirM20Stmt::JsonParse(Box::new(
        HirJsonParseStmt {
            source: src_hir,
            destination,
            detail: match parts.detail {
                JsonDetailPresence::Standard => HirJsonDetailMode::Standard,
                JsonDetailPresence::WithDetail => HirJsonDetailMode::WithDetail,
            },
            name_overrides,
            suppressed_fields,
            bool_fields,
            on_exception,
            not_on_exception,
            span: parts.span,
        },
    )))));
}

/// Resolves a subclause `DataRef`, implicitly qualifying unqualified names under `root_item` if needed.
///
/// # Arguments
///
/// * `dref` - Subclause target [`DataRef`].
/// * `root_item` - Optional enclosing record [`DataItemId`].
/// * `ctx` - Unit lowering context.
fn resolve_markup_place(
    dref: &DataRef,
    root_item: Option<DataItemId>,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<coblang_hir::HirPlace> {
    if dref.name.qualifiers.is_empty()
        && let Some(rid) = root_item
        && let Some(root_sym) = ctx
            .layout
            .items
            .get(rid.as_u32() as usize)
            .and_then(|i| i.name)
        && ctx
            .symtab
            .resolve_symbol_silent(ctx.scope, dref.name.base, &[root_sym])
            .is_some()
    {
        let mut qref = dref.clone();
        qref.name.qualifiers.push(root_sym);
        return resolve_place(&qref, ctx);
    }
    resolve_place(dref, ctx)
}

/// Resolves `NAME OF` clauses into `(DataItemId, HirMarkupNameOverride)`.
///
/// # Arguments
///
/// * `clauses` - Parsed [`MarkupNameClause`] list.
/// * `root_item` - Optional enclosing record [`DataItemId`].
/// * `ctx` - Unit lowering context.
fn lower_name_mappings(
    clauses: &[MarkupNameClause],
    root_item: Option<DataItemId>,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<(DataItemId, HirMarkupNameOverride)> {
    let mut out = Vec::with_capacity(clauses.len());
    for c in clauses {
        if let Some(pl) = resolve_markup_place(&c.target, root_item, ctx) {
            let ov = match c.override_name {
                MarkupNameTarget::Omitted => HirMarkupNameOverride::Omitted,
                MarkupNameTarget::Literal(sym) => {
                    HirMarkupNameOverride::Rename(sym_orig(sym, ctx.interner))
                }
            };
            out.push((pl.item, ov));
        }
    }
    out
}

/// Resolves `TYPE OF` clauses into `(DataItemId, HirXmlNodeType)`.
///
/// # Arguments
///
/// * `clauses` - Parsed [`XmlTypeClause`] list.
/// * `root_item` - Optional enclosing record [`DataItemId`].
/// * `ctx` - Unit lowering context.
fn lower_xml_type_mappings(
    clauses: &[XmlTypeClause],
    root_item: Option<DataItemId>,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<(DataItemId, HirXmlNodeType)> {
    let mut out = Vec::with_capacity(clauses.len());
    for c in clauses {
        if let Some(pl) = resolve_markup_place(&c.target, root_item, ctx) {
            let nt = match c.node_type {
                XmlNodeTypeClause::Attribute => HirXmlNodeType::Attribute,
                XmlNodeTypeClause::Element => HirXmlNodeType::Element,
                XmlNodeTypeClause::Content => HirXmlNodeType::Content,
            };
            out.push((pl.item, nt));
        }
    }
    out
}

/// Resolves `SUPPRESS` clauses into field-targeted and generic rules.
///
/// # Arguments
///
/// * `clauses` - Parsed [`MarkupSuppressClause`] list.
/// * `root_item` - Optional enclosing record [`DataItemId`].
/// * `ctx` - Unit lowering context.
fn lower_suppress_clauses(
    clauses: &[MarkupSuppressClause],
    root_item: Option<DataItemId>,
    ctx: &mut UnitLowerCtx<'_>,
) -> (Vec<(DataItemId, u8)>, Vec<HirGenericSuppressRule>) {
    let mut field_out = Vec::new();
    let mut gen_out = Vec::new();
    for c in clauses {
        let mask = encode_suppress_conds(&c.conditions);
        match &c.subject {
            MarkupSuppressSubject::Field(dref) => {
                if let Some(pl) = resolve_markup_place(dref, root_item, ctx) {
                    field_out.push((pl.item, mask));
                }
            }
            MarkupSuppressSubject::Generic {
                category,
                node_kind,
            } => {
                let category_code = match category {
                    MarkupSuppressGenericKind::Any => 0,
                    MarkupSuppressGenericKind::Numeric => 1,
                    MarkupSuppressGenericKind::Nonnumeric => 2,
                };
                let placement_code = match node_kind {
                    MarkupSuppressNodeKind::Any => 0,
                    MarkupSuppressNodeKind::Element => 1,
                    MarkupSuppressNodeKind::Attribute => 2,
                    MarkupSuppressNodeKind::Content => 3,
                };
                gen_out.push(HirGenericSuppressRule {
                    category_code,
                    placement_code,
                    cond_mask: mask,
                });
            }
        }
    }
    (field_out, gen_out)
}

/// Encodes `MarkupSuppressCond` atoms into a bitmask (`0` = unconditional).
///
/// # Arguments
///
/// * `conds` - Figurative condition atoms (`ZERO`, `SPACE`, `LOW-VALUE`, `HIGH-VALUE`).
fn encode_suppress_conds(conds: &[MarkupSuppressCond]) -> u8 {
    let mut bits = 0_u8;
    for c in conds {
        match c {
            MarkupSuppressCond::Zero => bits |= 1,
            MarkupSuppressCond::Space => bits |= 2,
            MarkupSuppressCond::LowValue => bits |= 4,
            MarkupSuppressCond::HighValue => bits |= 8,
        }
    }
    bits
}

/// Resolves `CONVERTING ... TO BOOLEAN` clauses for `JSON GENERATE`.
///
/// # Arguments
///
/// * `clauses` - Parsed [`JsonBoolClause`] list.
/// * `root_item` - Optional enclosing record [`DataItemId`].
/// * `ctx` - Unit lowering context.
fn lower_json_gen_bools(
    clauses: &[JsonBoolClause],
    root_item: Option<DataItemId>,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<(DataItemId, String)> {
    let mut out = Vec::new();
    for c in clauses {
        if let Some(pl) = resolve_markup_place(&c.target, root_item, ctx) {
            let lit = resolve_bool_truth_lit(c.cond_sym, ctx).0;
            out.push((pl.item, lit));
        }
    }
    out
}

/// Resolves `CONVERTING ... FROM BOOLEAN` clauses for `JSON PARSE`.
///
/// # Arguments
///
/// * `clauses` - Parsed [`JsonBoolClause`] list.
/// * `root_item` - Optional enclosing record [`DataItemId`].
/// * `ctx` - Unit lowering context.
fn lower_json_parse_bools(
    clauses: &[JsonBoolClause],
    root_item: Option<DataItemId>,
    ctx: &mut UnitLowerCtx<'_>,
) -> Vec<(DataItemId, String, String)> {
    let mut out = Vec::new();
    for c in clauses {
        if let Some(pl) = resolve_markup_place(&c.target, root_item, ctx) {
            let (t_lit, def_f_lit) = resolve_bool_truth_lit(c.cond_sym, ctx);
            let f_lit = c
                .false_sym
                .map_or(def_f_lit, |fs| sym_orig(fs, ctx.interner));
            out.push((pl.item, t_lit, f_lit));
        }
    }
    out
}

/// Resolves `(true_literal, false_literal)` from an optional level-`88` condition or literal symbol.
///
/// # Arguments
///
/// * `cond_sym` - Optional level-`88` condition name or truth literal [`SymbolId`].
/// * `ctx` - Unit lowering context.
fn resolve_bool_truth_lit(
    cond_sym: Option<SymbolId>,
    ctx: &mut UnitLowerCtx<'_>,
) -> (String, String) {
    if let Some(sym) = cond_sym {
        if let Some(ResolvedSymbol::Condition88(idx)) =
            ctx.symtab.resolve_symbol_silent(ctx.scope, sym, &[])
            && let Some(c88) = ctx.layout.conditions_88.get(idx)
        {
            let t = c88.ranges.first().map_or_else(
                || String::from("1"),
                |(lo, _)| const_to_text(lo, ctx.interner),
            );
            let f = c88
                .false_value
                .as_ref()
                .map_or_else(|| String::from("0"), |fv| const_to_text(fv, ctx.interner));
            return (t, f);
        }
        return (sym_orig(sym, ctx.interner), String::from("0"));
    }
    (String::from("1"), String::from("0"))
}

/// Renders a [`coblang_hir::HirConstValue`] into a plain string for boolean truth matching.
///
/// # Arguments
///
/// * `val` - HIR constant value.
/// * `interner` - Symbol interner.
fn const_to_text(val: &coblang_hir::HirConstValue, interner: &SymbolInterner) -> String {
    match val {
        coblang_hir::HirConstValue::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        coblang_hir::HirConstValue::Decimal { text, .. } => sym_orig(*text, interner),
        coblang_hir::HirConstValue::Figurative(coblang_hir::HirFigurative::Zero) => {
            String::from("0")
        }
        coblang_hir::HirConstValue::Figurative(coblang_hir::HirFigurative::Space) => {
            String::from(" ")
        }
        _ => String::from("1"),
    }
}

/// Computes the effective [`HirDecimalCommaMode`] at `stmt_span` (`$SET DPC-IN-DATA` overrides `dialect.dpc_in_data`).
///
/// # Arguments
///
/// * `stmt_span` - Statement source span.
/// * `for_xml` - `true` for `XML GENERATE`, `false` for `JSON GENERATE`.
/// * `ctx` - Unit lowering context.
fn active_dpc_mode(
    stmt_span: SourceSpan,
    for_xml: bool,
    ctx: &UnitLowerCtx<'_>,
) -> HirDecimalCommaMode {
    if ctx.decimal_point != DecimalPointKind::Comma {
        return HirDecimalCommaMode::Period;
    }
    let mut mode: DpcInDataMode = ctx.dialect.dpc_in_data;
    for rec in ctx.dpc_directives {
        if rec.span.file_id == stmt_span.file_id && rec.span.start <= stmt_span.start {
            mode = rec.mode;
        }
    }
    let applies = if for_xml {
        mode.applies_to_xml()
    } else {
        mode.applies_to_json()
    };
    if applies {
        HirDecimalCommaMode::Comma
    } else {
        HirDecimalCommaMode::Period
    }
}

/// Resolves original spelling of `sym` in `interner`.
///
/// # Arguments
///
/// * `sym` - Interned [`SymbolId`].
/// * `interner` - Symbol interner.
fn sym_orig(sym: SymbolId, interner: &SymbolInterner) -> String {
    interner.resolve_original(sym).unwrap_or("").to_owned()
}

/// Resolves canonical uppercase spelling of `sym` in `interner`.
///
/// # Arguments
///
/// * `sym` - Interned [`SymbolId`].
/// * `interner` - Symbol interner.
fn sym_canon(sym: SymbolId, interner: &SymbolInterner) -> String {
    interner.resolve_canonical(sym).unwrap_or("").to_owned()
}
