//! Milestone M12 intrinsic function typechecker, `REPOSITORY` resolver, `ALL` table-slice
//! expander, and User-Defined Function (`FUNCTION-ID`) signature table for `coblang-sema`.

use std::collections::{BTreeMap, BTreeSet};

use coblang_common::{
    ByteLength, ByteOffset, DigitCount, LevelNumber, Result, ScaleFactor, SourceSpan, SymbolId,
};
use coblang_diag::{
    Diagnostic, DiagnosticCode, DiagnosticSink, DialectConfig, RelaxedSyntaxMode, SymbolInterner,
    WarningGroup, WarningPolicy,
};
use coblang_hir::{
    BinaryEndianness, BlankZeroSpec, Comp6Mode, DataCategory, DynamicOdoPresence,
    ExceptionCheckMask, FloatBinaryWidth, FloatDecimalWidth, HirAlgebraicSpec, HirConstValue,
    HirDataItem, HirExpr, HirExtIntrinsic, HirFigurative, HirM12Arg, HirM12Callee,
    HirM12FunctionCall, HirPlace, HirRefMod, HirSliceBound, HirTableSlice, HirType, JustifiedSpec,
    NumericSignSpec, Signedness, TruncationPolicy,
};
use coblang_intrinsics::datetime::{
    ClockSnapshot, dt_formatted_current_date, dt_formatted_date, dt_formatted_datetime,
    dt_formatted_time,
};
use coblang_intrinsics::{IntrinsicId, IntrinsicReturnKind};
use coblang_syntax::ast::{
    CompilationGroup, CompilationUnit, DataEntryName, DataRef, DataRefModifier, DecimalPointKind,
    Expr, FunctionCallSyntax, FunctionUnit, IntrinsicImportPolicy, LengthVariability, RefModSpec,
    RepositoryParagraph, SubscriptItem, ValueAtom,
};

use crate::lower_hir::{UnitLowerCtx, lookup_constant_silent, lower_expr_with_cat, resolve_place};
use crate::picture::{ElementaryClauses, PictureContext, compile_elementary_type};
use crate::symbols::ResolvedSymbol;

/// Pre-collected signature of a `FUNCTION-ID` unit inside a [`CompilationGroup`].
#[derive(Debug, Clone)]
pub struct UdfUnitSignature {
    /// Internal `FUNCTION-ID` name symbol.
    pub internal_name: SymbolId,
    /// External linkage symbol (`AS "ext"` if present, else `internal_name`).
    pub external_name: SymbolId,
    /// Canonical uppercase external key used for cross-unit `REPOSITORY` binding.
    pub external_key: String,
    /// Resolved `RETURNING` [`HirType`].
    pub return_ty: HirType,
    /// Resolved `RETURNING` [`DataCategory`].
    pub return_cat: DataCategory,
    /// Expected formal parameter count (`PROCEDURE DIVISION USING ...`).
    pub param_count: usize,
    /// Source span of the `FUNCTION-ID` header.
    pub span: SourceSpan,
}

/// Active per-unit `REPOSITORY` function resolution table.
#[derive(Debug, Clone, Default)]
pub struct UnitRepositoryScope {
    /// Whether `FUNCTION ALL INTRINSIC` was declared in `REPOSITORY`.
    pub all_intrinsics_bare: bool,
    /// Specific intrinsic canonical names imported without `FUNCTION`.
    pub bare_intrinsics: BTreeSet<String>,
    /// User-defined functions imported in `REPOSITORY`, mapping internal `SymbolId` to `(external_sym, external_key, span)`.
    pub udf_imports: BTreeMap<SymbolId, (SymbolId, String, SourceSpan)>,
}

/// Pre-scans all top-level `FUNCTION-ID` units in `group` so forward-referenced UDF calls
/// (`PROGRAM-ID` preceding `FUNCTION-ID` in the same compilation group) know exact return types.
///
/// # Arguments
///
/// * `group` - Parsed [`CompilationGroup`].
/// * `dialect` - Active [`DialectConfig`].
/// * `interner` - Shared [`SymbolInterner`].
/// * `sink` - Compiler [`DiagnosticSink`].
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] only if symbol interning fails.
pub fn collect_group_udf_signatures(
    group: &CompilationGroup,
    dialect: &DialectConfig,
    interner: &mut SymbolInterner,
    sink: &mut DiagnosticSink,
) -> Result<BTreeMap<String, UdfUnitSignature>> {
    let mut table = BTreeMap::new();
    for unit in &group.units {
        let CompilationUnit::Function(func) = unit else {
            continue;
        };
        let sig = build_single_udf_signature(func, dialect, interner, sink);
        table.insert(sig.external_key.clone(), sig);
    }
    Ok(table)
}

/// Validates a `FUNCTION-ID` unit's `LINKAGE` / `USING` / `RETURNING` headers and extracts its [`UdfUnitSignature`].
///
/// # Arguments
///
/// * `func` - Parsed [`FunctionUnit`].
/// * `dialect` - Active [`DialectConfig`].
/// * `interner` - Shared [`SymbolInterner`].
/// * `sink` - Compiler [`DiagnosticSink`].
#[expect(
    clippy::too_many_lines,
    reason = "Complete ISO 2023 / GnuCOBOL `FUNCTION-ID` `RETURNING` & `LINKAGE` header validation."
)]
fn build_single_udf_signature(
    func: &FunctionUnit,
    dialect: &DialectConfig,
    interner: &mut SymbolInterner,
    sink: &mut DiagnosticSink,
) -> UdfUnitSignature {
    let internal_name = func.function_id.name;
    let external_name = func.function_id.external_alias.unwrap_or(internal_name);
    let external_key = match func.function_id.external_alias {
        Some(alias) => interner
            .resolve_original(alias)
            .unwrap_or("")
            .to_ascii_uppercase(),
        None => interner
            .resolve_canonical(internal_name)
            .unwrap_or("")
            .to_owned(),
    };

    let return_ty = HirType::Alphanumeric {
        byte_len: 32,
        justified: JustifiedSpec::Left,
    };
    let return_cat = DataCategory::Alphanumeric;

    let hdr_span = func.procedure.span.unwrap_or(func.function_id.span);
    let Some(ret_sym) = func.procedure.header.returning else {
        if func.procedure.span.is_some() {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(hdr_span),
                String::from("PROCEDURE DIVISION of a FUNCTION requires a RETURNING item"),
            ));
        }
        return UdfUnitSignature {
            internal_name,
            external_name,
            external_key,
            return_ty,
            return_cat,
            param_count: func.procedure.header.using_params.len(),
            span: func.function_id.span,
        };
    };

    let ret_canon = interner.resolve_canonical(ret_sym).unwrap_or("").to_owned();
    if ret_canon == "OMITTED" {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(hdr_span),
            String::from("RETURNING clause cannot be OMITTED for a FUNCTION"),
        ));
        return UdfUnitSignature {
            internal_name,
            external_name,
            external_key,
            return_ty,
            return_cat,
            param_count: func.procedure.header.using_params.len(),
            span: func.function_id.span,
        };
    }

    if func
        .procedure
        .header
        .using_params
        .iter()
        .any(|p| p.name == ret_sym)
    {
        let orig = interner
            .resolve_original(ret_sym)
            .unwrap_or(&ret_canon)
            .to_owned();
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(hdr_span),
            format!("'{orig}' USING item duplicates RETURNING item"),
        ));
    }

    let linkage_match = func
        .data
        .linkage
        .iter()
        .find(|e| e.name == DataEntryName::Named(ret_sym));
    let Some(ret_entry) = linkage_match else {
        let in_ws_or_ls = func
            .data
            .working_storage
            .iter()
            .chain(func.data.local_storage.iter())
            .any(|e| e.name == DataEntryName::Named(ret_sym));
        if in_ws_or_ls {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0007InvalidLayout,
                Some(hdr_span),
                String::from("RETURNING item must be defined in LINKAGE SECTION"),
            ));
        }
        return UdfUnitSignature {
            internal_name,
            external_name,
            external_key,
            return_ty,
            return_cat,
            param_count: func.procedure.header.using_params.len(),
            span: func.function_id.span,
        };
    };

    if !matches!(
        ret_entry.level,
        LevelNumber::Hierarchy(1) | LevelNumber::Independent77
    ) {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(ret_entry.span),
            String::from("RETURNING item must have level 01 or 77"),
        ));
    }
    if ret_entry.occurs.is_some() {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(ret_entry.span),
            String::from("RETURNING item should not have OCCURS"),
        ));
    }
    if ret_entry.redefines.is_some() {
        let orig = interner
            .resolve_original(ret_sym)
            .unwrap_or(&ret_canon)
            .to_owned();
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            Some(ret_entry.span),
            format!("'{orig}' REDEFINES field not allowed as RETURNING item"),
        ));
    }
    if ret_entry.flags.length_mode != LengthVariability::Fixed {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0007InvalidLayout,
            func.procedure.span.or(Some(ret_entry.span)),
            String::from("function RETURNING item may not be ANY LENGTH"),
        ));
    }

    let solved_ty = estimate_entry_hir_type(ret_entry, &func.data.linkage, func, dialect, interner);
    let solved_cat = solved_ty.category();

    UdfUnitSignature {
        internal_name,
        external_name,
        external_key,
        return_ty: solved_ty,
        return_cat: solved_cat,
        param_count: func.procedure.header.using_params.len(),
        span: func.function_id.span,
    }
}

/// Computes the [`HirType`] of a `LINKAGE SECTION` `RETURNING` entry (supporting elementary `PICTURE`/`USAGE` and `01` groups).
///
/// # Arguments
///
/// * `entry` - Target `RETURNING` `DataEntry`.
/// * `linkage` - Enclosing `LINKAGE SECTION` entries.
/// * `func` - Owning [`FunctionUnit`].
/// * `dialect` - Active [`DialectConfig`].
/// * `interner` - Shared [`SymbolInterner`].
fn estimate_entry_hir_type(
    entry: &coblang_syntax::ast::DataEntry,
    linkage: &[coblang_syntax::ast::DataEntry],
    func: &FunctionUnit,
    dialect: &DialectConfig,
    interner: &SymbolInterner,
) -> HirType {
    let sn = &func.environment.configuration.special_names;
    let currency_symbol = sn
        .currency_signs
        .first()
        .and_then(|cs| cs.picture_symbol.or(Some(cs.sign_literal)))
        .and_then(|sym| interner.resolve_original(sym).ok())
        .and_then(|s| s.chars().next())
        .unwrap_or('$');
    let pic_ctx = PictureContext {
        decimal_point: sn.decimal_point,
        currency_symbol,
    };
    let pic_pair = entry
        .picture
        .and_then(|(sym, sp)| interner.resolve_original(sym).ok().map(|raw| (raw, sp)));
    let clauses = ElementaryClauses {
        raw_pic: pic_pair,
        entry_span: entry.span,
        pic_ctx,
        usage: entry.usage.as_ref(),
        sign_clause: entry.sign,
        justified: entry.justified,
        blank_zero: entry.blank_when_zero,
    };
    let mut scratch = DiagnosticSink::new(WarningPolicy::new(), 10);
    if entry.picture.is_some() || entry.usage.is_some() {
        return compile_elementary_type(&clauses, dialect, &mut scratch);
    }
    let mut group_len = 0_u32;
    let mut inside = false;
    for item in linkage {
        if std::ptr::eq(item, entry) {
            inside = true;
            continue;
        }
        if inside {
            if matches!(
                item.level,
                LevelNumber::Hierarchy(1) | LevelNumber::Independent77 | LevelNumber::Constant78
            ) {
                break;
            }
            if item.level == LevelNumber::Condition88 || item.redefines.is_some() {
                continue;
            }
            let sub_pic = item
                .picture
                .and_then(|(sym, sp)| interner.resolve_original(sym).ok().map(|raw| (raw, sp)));
            let sub_clauses = ElementaryClauses {
                raw_pic: sub_pic,
                entry_span: item.span,
                pic_ctx,
                usage: item.usage.as_ref(),
                sign_clause: item.sign,
                justified: item.justified,
                blank_zero: item.blank_when_zero,
            };
            if item.picture.is_some() || item.usage.is_some() {
                let sub_ty = compile_elementary_type(&sub_clauses, dialect, &mut scratch);
                let mult = item.occurs.as_ref().map_or(1, |o| match o.max {
                    coblang_syntax::ast::OccursUpperBound::Bounded(n) => n,
                    coblang_syntax::ast::OccursUpperBound::Unbounded => 1,
                });
                group_len =
                    group_len.saturating_add(sub_ty.static_byte_size().saturating_mul(mult));
            }
        }
    }
    HirType::Group {
        static_byte_len: group_len.max(1),
        odo_presence: DynamicOdoPresence::StaticSize,
    }
}

/// Validates a unit's `REPOSITORY.` paragraph and builds its [`UnitRepositoryScope`].
///
/// # Arguments
///
/// * `repo` - Parsed [`RepositoryParagraph`].
/// * `current_unit_name` - Owning `PROGRAM-ID` or `FUNCTION-ID` symbol.
/// * `is_function_unit` - `true` when validating a `FUNCTION-ID` unit.
/// * `special_names` - Unit `SPECIAL-NAMES` paragraph (for alphabet/class name collision checks).
/// * `udf_sigs` - Pre-collected compilation-group UDF signatures.
/// * `prog_keys` - Pre-collected uppercase program external names across the compilation group.
/// * `dialect` - Active [`DialectConfig`].
/// * `interner` - Shared [`SymbolInterner`].
/// * `sink` - Compiler [`DiagnosticSink`].
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] only if symbol interning fails.
#[expect(
    clippy::too_many_arguments,
    reason = "Complete unit REPOSITORY context needed for ISO 2023 collision and dialect checks."
)]
#[expect(
    clippy::too_many_lines,
    reason = "Complete PROGRAM, FUNCTION, and INTRINSIC REPOSITORY validation."
)]
pub fn build_unit_repository_scope(
    repo: &RepositoryParagraph,
    current_unit_name: SymbolId,
    is_function_unit: bool,
    special_names: &coblang_syntax::ast::SpecialNamesParagraph,
    udf_sigs: &BTreeMap<String, UdfUnitSignature>,
    prog_keys: &BTreeSet<String>,
    dialect: &DialectConfig,
    interner: &mut SymbolInterner,
    sink: &mut DiagnosticSink,
) -> Result<UnitRepositoryScope> {
    let mut scope = UnitRepositoryScope::default();
    match &repo.intrinsics {
        IntrinsicImportPolicy::ExplicitOnly => {}
        IntrinsicImportPolicy::AllIntrinsic => {
            scope.all_intrinsics_bare = true;
        }
        IntrinsicImportPolicy::SpecificIntrinsics(_) => {
            for &(isym, isp) in &repo.intrinsic_spans {
                let canon = interner.resolve_canonical(isym).unwrap_or("");
                if lookup_intrinsic_or_ext(canon).is_none() || !dialect.is_intrinsic_enabled(canon)
                {
                    let orig = interner.resolve_original(isym).unwrap_or(canon);
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0004UndefinedSymbol,
                        Some(isp),
                        format!("FUNCTION '{orig}' unknown"),
                    ));
                } else {
                    scope.bare_intrinsics.insert(canon.to_owned());
                }
            }
        }
    }

    let mut special_syms: BTreeSet<SymbolId> = BTreeSet::new();
    for alph in &special_names.alphabets {
        let _ = special_syms.insert(alph.name);
    }
    for cls in &special_names.classes {
        let _ = special_syms.insert(cls.name);
    }
    for sym_ch in &special_names.symbolic_chars {
        for &s in &sym_ch.names {
            let _ = special_syms.insert(s);
        }
    }

    let mut seen_programs: BTreeMap<SymbolId, String> = BTreeMap::new();
    let mut seen_ext_names: BTreeMap<String, SymbolId> = BTreeMap::new();
    for &(psym, palias, psp) in &repo.programs {
        let ext_key = match palias {
            Some(al) => interner
                .resolve_original(al)
                .unwrap_or("")
                .to_ascii_uppercase(),
            None => interner.resolve_canonical(psym).unwrap_or("").to_owned(),
        };
        let orig = interner.resolve_original(psym).unwrap_or("");
        if let Some(prev_ext) = seen_programs.get(&psym) {
            if prev_ext == &ext_key {
                sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::Redefinition,
                    Some(psp),
                    format!("duplicate REPOSITORY entry for '{orig}'"),
                ));
            } else {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0004UndefinedSymbol,
                    Some(psp),
                    format!("redefinition of program name '{orig}'"),
                ));
            }
        } else if let Some(&other_sym) = seen_ext_names.get(&ext_key)
            && other_sym != psym
        {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0004UndefinedSymbol,
                Some(psp),
                format!("external repository name '{ext_key}' already in use"),
            ));
        } else {
            if psym != current_unit_name && !prog_keys.contains(&ext_key) {
                sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::CallParams,
                    Some(psp),
                    format!("no definition/prototype seen for PROGRAM '{orig}'"),
                ));
            }
            let _ = seen_programs.insert(psym, ext_key.clone());
            let _ = seen_ext_names.insert(ext_key, psym);
        }
    }

    for (idx, &(fsym, falias)) in repo.functions.iter().enumerate() {
        let fsp = repo
            .function_spans
            .get(idx)
            .copied()
            .unwrap_or_else(synthetic_zero_span);
        let canon = interner.resolve_canonical(fsym).unwrap_or("").to_owned();
        let orig = interner.resolve_original(fsym).unwrap_or(&canon).to_owned();

        if lookup_intrinsic_or_ext(&canon).is_some() && dialect.is_intrinsic_enabled(&canon) {
            let err_sp = repo.terminator_span.unwrap_or(fsp);
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0003Syntax,
                Some(err_sp),
                String::from(
                    "syntax error, unexpected ., expecting intrinsic function name or INTRINSIC",
                ),
            ));
            continue;
        }

        let ext_sym = match falias {
            Some(al) => al,
            None => interner.intern(&canon)?,
        };
        let ext_key = match falias {
            Some(al) => interner
                .resolve_original(al)
                .unwrap_or("")
                .to_ascii_uppercase(),
            None => canon.clone(),
        };

        if special_syms.contains(&fsym) || seen_programs.contains_key(&fsym) {
            if special_syms.contains(&fsym) && !udf_sigs.contains_key(&ext_key) {
                sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::CallParams,
                    Some(fsp),
                    format!("no definition/prototype seen for FUNCTION '{orig}'"),
                ));
            }
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0004UndefinedSymbol,
                Some(fsp),
                format!("redefinition of '{orig}' in REPOSITORY"),
            ));
            continue;
        }

        if let Some(&other_sym) = seen_ext_names.get(&ext_key)
            && other_sym != fsym
        {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0004UndefinedSymbol,
                Some(fsp),
                format!("external repository name '{ext_key}' already in use"),
            ));
            continue;
        }

        if is_function_unit && fsym == current_unit_name {
            sink.emit(Diagnostic::warning(
                DiagnosticCode::W0006GeneralWarning,
                WarningGroup::Redefinition,
                Some(fsp),
                String::from("prototype has same name as current function and will be ignored"),
            ));
            continue;
        }

        if let Some((_, prev_ext, _)) = scope.udf_imports.get(&fsym) {
            if prev_ext == &ext_key {
                sink.emit(Diagnostic::warning(
                    DiagnosticCode::W0006GeneralWarning,
                    WarningGroup::Redefinition,
                    Some(fsp),
                    format!("duplicate REPOSITORY entry for '{orig}'"),
                ));
            } else {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0004UndefinedSymbol,
                    Some(fsp),
                    format!("conflicting REPOSITORY declaration for FUNCTION '{orig}'"),
                ));
            }
            continue;
        }

        if !udf_sigs.contains_key(&ext_key) {
            sink.emit(Diagnostic::warning(
                DiagnosticCode::W0006GeneralWarning,
                WarningGroup::CallParams,
                Some(fsp),
                format!("no definition/prototype seen for FUNCTION '{orig}'"),
            ));
        }

        let _ = seen_ext_names.insert(ext_key.clone(), fsym);
        let _ = scope.udf_imports.insert(fsym, (ext_sym, ext_key, fsp));
    }

    if is_function_unit && !scope.udf_imports.contains_key(&current_unit_name) {
        let self_canon = interner
            .resolve_canonical(current_unit_name)
            .unwrap_or("")
            .to_owned();
        if let Some(sig) = udf_sigs.get(&self_canon) {
            let _ = scope.udf_imports.insert(
                current_unit_name,
                (sig.external_name, self_canon, synthetic_zero_span()),
            );
        }
    }

    Ok(scope)
}

/// Returns `true` when `sym` is imported via `REPOSITORY.` as either a User-Defined Function or a
/// zero-argument intrinsic (`PI`, `E`, `CURRENT-DATE`, `EXCEPTION-*`, `MODULE-*`, `RANDOM`, etc.).
///
/// # Arguments
///
/// * `sym` - Candidate bare identifier symbol.
/// * `ctx` - Active unit lowering context.
#[must_use]
pub(crate) fn is_bare_repository_function(sym: SymbolId, ctx: &UnitLowerCtx<'_>) -> bool {
    if ctx.repo_scope.udf_imports.contains_key(&sym) {
        return true;
    }
    let canon = ctx.interner.resolve_canonical(sym).unwrap_or("");
    if !(ctx.repo_scope.all_intrinsics_bare || ctx.repo_scope.bare_intrinsics.contains(canon)) {
        return false;
    }
    let Some(kind) = lookup_intrinsic_or_ext(canon) else {
        return false;
    };
    effective_intrinsic_arity(kind).0 == 0
}

/// Constructs a zero-length root [`SourceSpan`].
#[must_use]
fn synthetic_zero_span() -> SourceSpan {
    SourceSpan::new(
        coblang_common::FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        coblang_common::ExpansionId::ROOT,
    )
}

/// Classification of an intrinsic as standard (`IntrinsicId`) vs `GnuCOBOL` extension (`HirExtIntrinsic`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ResolvedIntrinsicKind {
    /// Standard `coblang_intrinsics::IntrinsicId`.
    Core(IntrinsicId),
    /// `GnuCOBOL` locale/pointer extension intrinsic.
    Ext(HirExtIntrinsic),
}

/// Resolves canonical uppercase `name` to [`ResolvedIntrinsicKind`] if known.
///
/// # Arguments
///
/// * `name` - Canonical uppercase function name.
#[must_use]
fn lookup_intrinsic_or_ext(name: &str) -> Option<ResolvedIntrinsicKind> {
    if let Ok(id) = IntrinsicId::from_cobol_name(name) {
        return Some(ResolvedIntrinsicKind::Core(id));
    }
    match name {
        "CONTENT-LENGTH" => Some(ResolvedIntrinsicKind::Ext(HirExtIntrinsic::ContentLength)),
        "MONETARY-DECIMAL-POINT" => Some(ResolvedIntrinsicKind::Ext(
            HirExtIntrinsic::MonetaryDecimalPoint,
        )),
        "MONETARY-THOUSANDS-SEPARATOR" => Some(ResolvedIntrinsicKind::Ext(
            HirExtIntrinsic::MonetaryThousandsSeparator,
        )),
        "NUMERIC-DECIMAL-POINT" => Some(ResolvedIntrinsicKind::Ext(
            HirExtIntrinsic::NumericDecimalPoint,
        )),
        "NUMERIC-THOUSANDS-SEPARATOR" => Some(ResolvedIntrinsicKind::Ext(
            HirExtIntrinsic::NumericThousandsSeparator,
        )),
        _ => None,
    }
}

/// Lowers an AST `Expr::FunctionCall` (`FUNCTION name(...)` or bare `REPOSITORY` call) into `(HirExpr, DataCategory)`.
///
/// # Arguments
///
/// * `name` - Function symbol ID.
/// * `syntax` - `ExplicitKeyword` vs `BareRepository`.
/// * `raw_args` - AST [`SubscriptItem`] argument list.
/// * `refmod` - Optional AST [`RefModSpec`].
/// * `span` - Full call expression [`SourceSpan`].
/// * `ctx` - Active [`UnitLowerCtx`].
pub(crate) fn lower_m12_function_call(
    name: SymbolId,
    syntax: FunctionCallSyntax,
    raw_args: &[SubscriptItem],
    refmod: Option<&RefModSpec>,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> (HirExpr, DataCategory) {
    let Ok(canon_ref) = ctx.interner.resolve_canonical(name) else {
        let fallback_args = raw_args
            .iter()
            .map(|a| match a {
                SubscriptItem::Expr(e) => lower_expr_with_cat(e, ctx).0,
                SubscriptItem::AllWildcard(sp) => HirExpr::Const {
                    value: HirConstValue::Figurative(HirFigurative::Zero),
                    span: *sp,
                },
            })
            .collect();
        return (
            HirExpr::CallFunction {
                name,
                args: fallback_args,
                span,
            },
            DataCategory::Numeric,
        );
    };
    let canon = canon_ref.to_owned();
    let orig = ctx
        .interner
        .resolve_original(name)
        .unwrap_or(&canon)
        .to_owned();

    if let Some(udf_res) = try_resolve_udf_target(name, &canon, syntax, ctx) {
        return finish_udf_call(udf_res, &orig, raw_args, refmod, span, ctx);
    }

    let Some(intr_kind) = lookup_intrinsic_or_ext(&canon) else {
        let msg = if ctx.repo_scope.udf_imports.contains_key(&name) {
            format!("no definition/prototype seen for FUNCTION '{orig}'")
        } else {
            format!("FUNCTION '{orig}' unknown")
        };
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0004UndefinedSymbol,
            Some(span),
            msg,
        ));
        return dummy_fn_result(span);
    };

    if !ctx.dialect.is_intrinsic_enabled(&canon) {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0004UndefinedSymbol,
            Some(span),
            format!("FUNCTION '{orig}' unknown"),
        ));
        return dummy_fn_result(span);
    }

    if syntax == FunctionCallSyntax::BareRepository
        && !ctx.repo_scope.all_intrinsics_bare
        && !ctx.repo_scope.bare_intrinsics.contains(&canon)
        && !ctx.dialect.is_bare_intrinsic_imported(&canon)
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0004UndefinedSymbol,
            Some(span),
            format!("'{orig}' is not defined"),
        ));
        return dummy_fn_result(span);
    }

    lower_resolved_intrinsic(intr_kind, &orig, raw_args, refmod, span, ctx)
}

/// Attempts to resolve `name` to a User-Defined Function [`UdfUnitSignature`].
///
/// # Arguments
///
/// * `name` - Called function symbol.
/// * `canon` - Canonical uppercase function spelling.
/// * `syntax` - `ExplicitKeyword` vs `BareRepository`.
/// * `ctx` - Active [`UnitLowerCtx`].
fn try_resolve_udf_target(
    name: SymbolId,
    canon: &str,
    syntax: FunctionCallSyntax,
    ctx: &UnitLowerCtx<'_>,
) -> Option<UdfUnitSignature> {
    if let Some(&(ext_sym, ref ext_key, _)) = ctx.repo_scope.udf_imports.get(&name) {
        if let Some(sig) = ctx.udf_sigs.get(ext_key) {
            return Some(sig.clone());
        }
        if ctx.dialect.relaxed_syntax == RelaxedSyntaxMode::Relaxed {
            return Some(UdfUnitSignature {
                internal_name: name,
                external_name: ext_sym,
                external_key: ext_key.clone(),
                return_ty: HirType::Alphanumeric {
                    byte_len: 64,
                    justified: JustifiedSpec::Left,
                },
                return_cat: DataCategory::Alphanumeric,
                param_count: usize::MAX,
                span: synthetic_zero_span(),
            });
        }
        return None;
    }

    if ctx.is_function_unit && ctx.unit_name == name {
        let key = ctx
            .udf_sigs
            .values()
            .find(|s| s.internal_name == name)
            .cloned();
        if key.is_some() {
            return key;
        }
    }

    if syntax == FunctionCallSyntax::ExplicitKeyword
        && (lookup_intrinsic_or_ext(canon).is_none() || !ctx.dialect.is_intrinsic_enabled(canon))
        && let Some(sig) = ctx.udf_sigs.get(canon)
    {
        return Some(sig.clone());
    }

    None
}

/// Validates and lowers a User-Defined Function call into `(HirExpr::M12Call, DataCategory)`.
///
/// # Arguments
///
/// * `sig` - Resolved [`UdfUnitSignature`].
/// * `orig_name` - Source spelling of the function name.
/// * `raw_args` - AST arguments.
/// * `refmod` - Optional AST [`RefModSpec`].
/// * `span` - Call expression span.
/// * `ctx` - Active [`UnitLowerCtx`].
fn finish_udf_call(
    sig: UdfUnitSignature,
    orig_name: &str,
    raw_args: &[SubscriptItem],
    refmod: Option<&RefModSpec>,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> (HirExpr, DataCategory) {
    if sig.param_count != usize::MAX && raw_args.len() > sig.param_count {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            format!(
                "FUNCTION '{orig_name}' has wrong number of arguments (expected at most {}, found {})",
                sig.param_count,
                raw_args.len()
            ),
        ));
    }

    let mut hir_args = Vec::with_capacity(raw_args.len());
    for item in raw_args {
        match item {
            SubscriptItem::AllWildcard(sp) => {
                ctx.sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(*sp),
                    String::from(
                        "ALL subscript is not permitted as a user-defined function argument",
                    ),
                ));
            }
            SubscriptItem::Expr(expr) => {
                if is_keyword_word_expr(expr, "OMITTED", ctx.interner) {
                    hir_args.push(HirM12Arg::Omitted);
                } else {
                    hir_args.push(HirM12Arg::Expr(lower_expr_with_cat(expr, ctx).0));
                }
            }
        }
    }

    let mut return_ty = sig.return_ty;
    let mut return_cat = sig.return_cat;
    let hir_refmod = validate_and_lower_fn_refmod(
        orig_name,
        refmod,
        true,
        Some(return_ty.static_byte_size()),
        &mut return_ty,
        &mut return_cat,
        ctx,
    );
    let location_sym = ctx.current_location_prefix_sym();

    (
        HirExpr::M12Call(Box::new(HirM12FunctionCall {
            callee: HirM12Callee::UserDefined {
                external_name: sig.external_name,
            },
            args: hir_args,
            refmod: hir_refmod,
            return_ty,
            location_sym,
            span,
        })),
        return_cat,
    )
}

/// Validates arity, argument categories, special keyword modifiers, format literals, `ALL` slices,
/// and reference modification for a resolved intrinsic call.
///
/// # Arguments
///
/// * `intr_kind` - Core vs extension intrinsic selector.
/// * `orig` - Original source spelling of the intrinsic name.
/// * `raw_args` - AST argument list.
/// * `refmod` - Optional AST [`RefModSpec`].
/// * `span` - Call [`SourceSpan`].
/// * `ctx` - Active [`UnitLowerCtx`].
fn lower_resolved_intrinsic(
    intr_kind: ResolvedIntrinsicKind,
    orig: &str,
    raw_args: &[SubscriptItem],
    refmod: Option<&RefModSpec>,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) -> (HirExpr, DataCategory) {
    let (min_a, max_a) = effective_intrinsic_arity(intr_kind);
    let argc = raw_args.len();
    let odd_pair_required = matches!(
        intr_kind,
        ResolvedIntrinsicKind::Core(IntrinsicId::Substitute | IntrinsicId::SubstituteCase)
    );
    if argc < min_a || argc > max_a || (odd_pair_required && argc.is_multiple_of(2)) {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            format!("FUNCTION '{orig}' has wrong number of arguments"),
        ));
        return dummy_fn_result(span);
    }

    let mut hir_args = Vec::with_capacity(argc);
    for (idx, item) in raw_args.iter().enumerate() {
        match item {
            SubscriptItem::AllWildcard(sp) => {
                hir_args.push(HirM12Arg::Expr(HirExpr::Const {
                    value: HirConstValue::Figurative(HirFigurative::Zero),
                    span: *sp,
                }));
            }
            SubscriptItem::Expr(expr) => {
                lower_single_intrinsic_arg(intr_kind, orig, idx, argc, expr, &mut hir_args, ctx);
            }
        }
    }

    validate_formatted_datetime_args(intr_kind, orig, raw_args, span, ctx);

    let (mut return_ty, mut return_cat, static_len_opt, allows_refmod) =
        compute_intrinsic_return_type(intr_kind, &hir_args);
    let hir_refmod = validate_and_lower_fn_refmod(
        orig,
        refmod,
        allows_refmod,
        static_len_opt,
        &mut return_ty,
        &mut return_cat,
        ctx,
    );
    let callee = match intr_kind {
        ResolvedIntrinsicKind::Core(id) => HirM12Callee::Intrinsic {
            abi_code: id.to_abi_code(),
        },
        ResolvedIntrinsicKind::Ext(ext) => HirM12Callee::ExtIntrinsic(ext),
    };
    let location_sym = ctx.current_location_prefix_sym();

    (
        HirExpr::M12Call(Box::new(HirM12FunctionCall {
            callee,
            args: hir_args,
            refmod: hir_refmod,
            return_ty,
            location_sym,
            span,
        })),
        return_cat,
    )
}

/// Returns `(min_args, max_args)` accounting for optional `GnuCOBOL`/ISO trailing keyword modifiers.
///
/// # Arguments
///
/// * `intr_kind` - Resolved intrinsic kind.
#[must_use]
fn effective_intrinsic_arity(intr_kind: ResolvedIntrinsicKind) -> (usize, usize) {
    match intr_kind {
        ResolvedIntrinsicKind::Core(
            IntrinsicId::FormattedCurrentDate
            | IntrinsicId::Length
            | IntrinsicId::ByteLength
            | IntrinsicId::Char
            | IntrinsicId::Ord,
        ) => (1, 2),
        ResolvedIntrinsicKind::Core(IntrinsicId::FormattedTime) => (2, 4),
        ResolvedIntrinsicKind::Core(IntrinsicId::FormattedDatetime) => (3, 5),
        ResolvedIntrinsicKind::Core(id) => id.arity_bounds(),
        ResolvedIntrinsicKind::Ext(HirExtIntrinsic::ContentLength) => (1, 1),
        ResolvedIntrinsicKind::Ext(
            HirExtIntrinsic::MonetaryDecimalPoint
            | HirExtIntrinsic::MonetaryThousandsSeparator
            | HirExtIntrinsic::NumericDecimalPoint
            | HirExtIntrinsic::NumericThousandsSeparator,
        ) => (0, 0),
    }
}

/// Lowers and validates a single positional argument `expr` at index `idx` of `intr_kind`.
///
/// # Arguments
///
/// * `intr_kind` - Target intrinsic.
/// * `orig_fn` - Display name of the intrinsic.
/// * `idx` - 0-based argument position.
/// * `argc` - Total argument count.
/// * `expr` - AST argument expression.
/// * `out` - Output [`HirM12Arg`] buffer.
/// * `ctx` - Active [`UnitLowerCtx`].
#[expect(
    clippy::too_many_lines,
    reason = "Dispatches OMITTED, keywords, algebraic probes, ALL slices, ODO probes, pointers, and scalars."
)]
fn lower_single_intrinsic_arg(
    intr_kind: ResolvedIntrinsicKind,
    orig_fn: &str,
    idx: usize,
    argc: usize,
    expr: &Expr,
    out: &mut Vec<HirM12Arg>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    if is_keyword_word_expr(expr, "OMITTED", ctx.interner) {
        out.push(HirM12Arg::Omitted);
        return;
    }

    if let Some(kw_sym) = try_extract_intrinsic_keyword(intr_kind, idx, argc, expr, ctx) {
        out.push(HirM12Arg::Keyword(kw_sym));
        return;
    }

    if matches!(
        intr_kind,
        ResolvedIntrinsicKind::Core(
            IntrinsicId::HighestAlgebraic
                | IntrinsicId::LowestAlgebraic
                | IntrinsicId::SmallestAlgebraic
        )
    ) {
        out.push(lower_algebraic_probe_arg(orig_fn, expr, ctx));
        return;
    }

    if matches!(
        intr_kind,
        ResolvedIntrinsicKind::Core(IntrinsicId::Length | IntrinsicId::ByteLength)
    ) {
        if idx == 1 {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(expr.span()),
                format!("FUNCTION '{orig_fn}' has invalid modifier argument"),
            ));
            return;
        }
        lower_length_target_arg(intr_kind, orig_fn, expr, out, ctx);
        return;
    }

    if let Expr::Data(dref) = expr
        && dref
            .subscripts
            .iter()
            .any(|s| matches!(s, SubscriptItem::AllWildcard(_)))
    {
        if !intrinsic_accepts_all_table(intr_kind) {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(dref.span),
                format!("FUNCTION '{orig_fn}' does not accept ALL table subscripts"),
            ));
            return;
        }
        if let Some(slice) = lower_all_table_slice(dref, intr_kind, orig_fn, ctx) {
            out.push(HirM12Arg::TableSlice(slice));
        }
        return;
    }

    if idx == 0
        && matches!(
            intr_kind,
            ResolvedIntrinsicKind::Core(
                IntrinsicId::BitOf
                    | IntrinsicId::HexOf
                    | IntrinsicId::ContentOf
                    | IntrinsicId::DisplayOf
                    | IntrinsicId::NationalOf
                    | IntrinsicId::StoredCharLength
                    | IntrinsicId::Ulength
                    | IntrinsicId::Upos
                    | IntrinsicId::Usubstr
                    | IntrinsicId::Usupplementary
                    | IntrinsicId::Uvalid
                    | IntrinsicId::Uwidth
            ) | ResolvedIntrinsicKind::Ext(HirExtIntrinsic::ContentLength)
        )
        && let Expr::Data(dref) = expr
        && dref.modifier == DataRefModifier::Plain
        && lookup_constant_silent(dref, ctx).is_none()
        && let Some(pl) = resolve_place(dref, ctx)
    {
        if matches!(
            intr_kind,
            ResolvedIntrinsicKind::Core(IntrinsicId::ContentOf)
                | ResolvedIntrinsicKind::Ext(HirExtIntrinsic::ContentLength)
        ) && pl.effective_ty.category() != DataCategory::Pointer
        {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(dref.span),
                format!("FUNCTION '{orig_fn}' requires a POINTER argument"),
            ));
        }
        out.push(HirM12Arg::RawStoragePlace(pl));
        return;
    }

    if idx == 1
        && matches!(
            intr_kind,
            ResolvedIntrinsicKind::Core(IntrinsicId::Char | IntrinsicId::Ord)
        )
        && let Expr::Data(dref) = expr
        && dref.name.qualifiers.is_empty()
        && dref.subscripts.is_empty()
        && dref.refmod.is_none()
        && let Some(alph) = ctx.alphabets.iter().find(|a| a.name == dref.name.base)
    {
        out.push(HirM12Arg::Expr(HirExpr::Const {
            value: HirConstValue::Bytes(alph.ranks.clone()),
            span: dref.span,
        }));
        return;
    }

    let (hir_e, cat) = lower_expr_with_cat(expr, ctx);
    validate_intrinsic_arg_category(intr_kind, orig_fn, idx, cat, expr.span(), ctx);
    out.push(HirM12Arg::Expr(hir_e));
}

/// Checks whether `expr` at `(intr_kind, idx)` is an intrinsic modifier keyword rather than a variable.
///
/// # Arguments
///
/// * `intr_kind` - Target intrinsic.
/// * `idx` - 0-based argument index.
/// * `argc` - Total argument count.
/// * `expr` - Candidate AST expression.
/// * `ctx` - Active [`UnitLowerCtx`].
fn try_extract_intrinsic_keyword(
    intr_kind: ResolvedIntrinsicKind,
    idx: usize,
    argc: usize,
    expr: &Expr,
    ctx: &UnitLowerCtx<'_>,
) -> Option<SymbolId> {
    let Expr::Data(dref) = expr else {
        return None;
    };
    if dref.modifier != DataRefModifier::Plain
        || !dref.name.qualifiers.is_empty()
        || !dref.subscripts.is_empty()
        || dref.refmod.is_some()
    {
        return None;
    }
    let word = ctx.interner.resolve_canonical(dref.name.base).ok()?;
    let matched = match intr_kind {
        ResolvedIntrinsicKind::Core(IntrinsicId::Trim) if idx == 1 => {
            matches!(word, "LEADING" | "TRAILING")
        }
        ResolvedIntrinsicKind::Core(IntrinsicId::FormattedCurrentDate) if idx == 1 => {
            word == "SYSTEM-OFFSET"
        }
        ResolvedIntrinsicKind::Core(IntrinsicId::FormattedTime) if idx == 2 => {
            word == "SYSTEM-OFFSET"
        }
        ResolvedIntrinsicKind::Core(IntrinsicId::FormattedDatetime) if idx == 3 => {
            word == "SYSTEM-OFFSET"
        }
        ResolvedIntrinsicKind::Core(IntrinsicId::Length | IntrinsicId::ByteLength) if idx == 1 => {
            matches!(word, "PHYSICAL" | "LOCALE")
        }
        ResolvedIntrinsicKind::Core(IntrinsicId::FindString) if idx >= 2 => {
            matches!(word, "ANYCASE" | "LAST")
        }
        ResolvedIntrinsicKind::Core(IntrinsicId::NumvalC | IntrinsicId::TestNumvalC)
            if idx >= 1 && idx <= argc =>
        {
            matches!(word, "ANYCASE" | "OMITTED")
        }
        ResolvedIntrinsicKind::Core(IntrinsicId::Convert) if idx >= 1 => matches!(
            word,
            "ANUM"
                | "ALPHANUMERIC"
                | "HEX"
                | "NAT"
                | "NATIONAL"
                | "BYTE"
                | "BIT"
                | "LOWER-CASE"
                | "UPPER-CASE"
        ),
        _ => word == "OMITTED",
    };
    matched.then_some(dref.name.base)
}

/// Lowers `HIGHEST-ALGEBRAIC` / `LOWEST-ALGEBRAIC` / `SMALLEST-ALGEBRAIC` target item probe.
///
/// # Arguments
///
/// * `orig_fn` - Display name of the intrinsic.
/// * `expr` - Target data-item AST expression.
/// * `ctx` - Active [`UnitLowerCtx`].
fn lower_algebraic_probe_arg(orig_fn: &str, expr: &Expr, ctx: &mut UnitLowerCtx<'_>) -> HirM12Arg {
    if let Expr::Data(dref) = expr
        && let Some(pl) = resolve_place(dref, ctx)
    {
        if let Some((digits, scale, signedness)) = extract_numeric_algebraic_spec(&pl.effective_ty)
        {
            return HirM12Arg::AlgebraicProbe(HirAlgebraicSpec {
                digits,
                scale,
                signedness,
            });
        }
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(dref.span),
            format!("FUNCTION '{orig_fn}' requires a numeric or numeric-edited identifier"),
        ));
    } else {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(expr.span()),
            format!("FUNCTION '{orig_fn}' requires a numeric data-item identifier"),
        ));
    }
    HirM12Arg::AlgebraicProbe(HirAlgebraicSpec {
        digits: 1,
        scale: 0,
        signedness: Signedness::Unsigned,
    })
}

/// Extracts `(digits, scale, signedness)` for algebraic range intrinsics (using `digits = 0` + encoded `scale` for floats).
///
/// # Arguments
///
/// * `ty` - Target [`HirType`].
#[must_use]
fn extract_numeric_algebraic_spec(ty: &HirType) -> Option<(u16, i16, Signedness)> {
    match ty {
        HirType::ZonedDecimal {
            digits,
            scale,
            sign,
            ..
        } => {
            let signedness = match sign {
                NumericSignSpec::Unsigned => Signedness::Unsigned,
                _ => Signedness::Signed,
            };
            Some((u16::from(digits.get()), scale.get(), signedness))
        }
        HirType::BinaryInt {
            byte_width,
            digits,
            scale,
            sign,
            trunc: TruncationPolicy::FullBinaryRange,
            ..
        } if scale.get() == 0
            && matches!(
                (*byte_width, digits.get()),
                (1, 3) | (2, 5) | (4, 10) | (8, 20)
            ) =>
        {
            Some((0, 10 + i16::from(*byte_width), *sign))
        }
        HirType::PackedDecimal {
            digits,
            scale,
            sign,
            ..
        }
        | HirType::BinaryInt {
            digits,
            scale,
            sign,
            ..
        } => Some((u16::from(digits.get()), scale.get(), *sign)),
        HirType::NumericEdited {
            digits,
            scale,
            sign_capability,
            ..
        } => Some((u16::from(digits.get()), scale.get(), *sign_capability)),
        HirType::FloatBinary { width } => {
            let code = match width {
                FloatBinaryWidth::Bits32 => 1,
                FloatBinaryWidth::Bits64 => 2,
                FloatBinaryWidth::Bits128 => 3,
            };
            Some((0, code, Signedness::Signed))
        }
        HirType::FloatDecimal { width } => {
            let digs = match width {
                FloatDecimalWidth::Decimal64 => 16,
                FloatDecimalWidth::Decimal128 => 34,
            };
            Some((digs, 0, Signedness::Signed))
        }
        _ => None,
    }
}

/// Lowers `FUNCTION LENGTH(x)` / `FUNCTION BYTE-LENGTH(x)` (including `ODO` variable-length groups).
///
/// # Arguments
///
/// * `intr_kind` - `Length` vs `ByteLength`.
/// * `orig_fn` - Display name of the intrinsic.
/// * `expr` - Measured argument AST expression.
/// * `out` - Output [`HirM12Arg`] list.
/// * `ctx` - Active [`UnitLowerCtx`].
fn lower_length_target_arg(
    intr_kind: ResolvedIntrinsicKind,
    orig_fn: &str,
    expr: &Expr,
    out: &mut Vec<HirM12Arg>,
    ctx: &mut UnitLowerCtx<'_>,
) {
    if let Expr::Literal { atom, span } = expr
        && matches!(
            atom,
            ValueAtom::Integer(_) | ValueAtom::Decimal { .. } | ValueAtom::Float(_)
        )
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(*span),
            format!("FUNCTION '{orig_fn}' argument may not be a numeric literal"),
        ));
        out.push(HirM12Arg::Expr(HirExpr::Const {
            value: HirConstValue::Bytes(vec![b' ']),
            span: *span,
        }));
        return;
    }

    if let Expr::Data(dref) = expr
        && dref.modifier == DataRefModifier::Plain
        && lookup_constant_silent(dref, ctx).is_none()
        && let Some(pl) = resolve_place(dref, ctx)
    {
        let is_char_len = matches!(intr_kind, ResolvedIntrinsicKind::Core(IntrinsicId::Length));
        if dref.refmod.is_none()
            && dref.subscripts.is_empty()
            && let Some(odo_probe) =
                try_build_odo_group_length_probe(pl.item, is_char_len, dref.span, ctx)
        {
            out.push(odo_probe);
            return;
        }
        out.push(HirM12Arg::RawStoragePlace(pl));
        return;
    }

    let (hir_e, _) = lower_expr_with_cat(expr, ctx);
    out.push(HirM12Arg::Expr(hir_e));
}

/// Builds [`HirM12Arg::OdoLengthProbe`] when `item_id` is a group containing a subordinate `OCCURS DEPENDING ON` table.
///
/// # Arguments
///
/// * `item_id` - Group [`coblang_common::DataItemId`].
/// * `char_units` - `true` for `FUNCTION LENGTH`, `false` for `FUNCTION BYTE-LENGTH`.
/// * `span` - Call site span.
/// * `ctx` - Active [`UnitLowerCtx`].
fn try_build_odo_group_length_probe(
    item_id: coblang_common::DataItemId,
    char_units: bool,
    span: SourceSpan,
    ctx: &UnitLowerCtx<'_>,
) -> Option<HirM12Arg> {
    let grp: &HirDataItem = ctx.layout.items.get(item_id.as_u32() as usize)?;
    if grp.children.is_empty() {
        return None;
    }
    for &cid in &grp.children {
        let child = ctx.layout.items.get(cid.as_u32() as usize)?;
        if let Some(first_dim) = child.dimensions.first()
            && let Some(dep_id) = first_dim.depending_on
        {
            let dep_item = ctx.layout.items.get(dep_id.as_u32() as usize)?;
            let prefix_bytes = child
                .byte_offset
                .as_u32()
                .saturating_sub(grp.byte_offset.as_u32());
            let mut stride = first_dim.element_byte_stride;
            let mut base = prefix_bytes;
            if char_units && matches!(child.ty.category(), DataCategory::National) {
                stride /= 2;
                base /= 2;
            }
            let odo_place = HirPlace {
                item: dep_id,
                static_offset: dep_item.byte_offset,
                subscripts: Vec::new(),
                refmod: None,
                effective_ty: dep_item.ty.clone(),
                check_mask: ExceptionCheckMask::NONE,
                span,
            };
            return Some(HirM12Arg::OdoLengthProbe {
                base_bytes: base,
                odo_place,
                elem_stride: stride,
            });
        }
    }
    None
}

/// Returns `true` if `intr_kind` accepts `ALL` table-slice arguments per ISO 2023.
///
/// # Arguments
///
/// * `intr_kind` - Target intrinsic.
#[must_use]
fn intrinsic_accepts_all_table(intr_kind: ResolvedIntrinsicKind) -> bool {
    matches!(
        intr_kind,
        ResolvedIntrinsicKind::Core(
            IntrinsicId::Max
                | IntrinsicId::Min
                | IntrinsicId::OrdMax
                | IntrinsicId::OrdMin
                | IntrinsicId::Sum
                | IntrinsicId::Mean
                | IntrinsicId::Median
                | IntrinsicId::Midrange
                | IntrinsicId::Range
                | IntrinsicId::Variance
                | IntrinsicId::StandardDeviation
                | IntrinsicId::PresentValue
        )
    )
}

/// Lowers an `ALL`-subscripted `DataRef` (`TAB(ALL)`, `TAB(1, ALL)`, `TAB(ALL, ALL)`) into a [`HirTableSlice`].
///
/// # Arguments
///
/// * `dref` - Table element `DataRef` containing one or more `SubscriptItem::AllWildcard` items.
/// * `intr_kind` - Enclosing intrinsic kind.
/// * `orig_fn` - Display name of the intrinsic.
/// * `ctx` - Active [`UnitLowerCtx`].
fn lower_all_table_slice(
    dref: &DataRef,
    intr_kind: ResolvedIntrinsicKind,
    orig_fn: &str,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<HirTableSlice> {
    let resolved = ctx.symtab.resolve_symbol(
        ctx.scope,
        dref.name.base,
        &dref.name.qualifiers,
        dref.name.span,
        ctx.interner,
        ctx.sink,
    )?;
    let ResolvedSymbol::DataItem(item_id) = resolved else {
        return None;
    };
    let item = ctx.layout.items.get(item_id.as_u32() as usize)?.clone();
    if item.dimensions.len() != dref.subscripts.len() {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(dref.span),
            format!(
                "table expects {} subscript(s), found {}",
                item.dimensions.len(),
                dref.subscripts.len()
            ),
        ));
        return None;
    }

    let mut fixed_subs = Vec::new();
    let mut varying_dims = Vec::new();
    for (sub_item, dim) in dref.subscripts.iter().zip(item.dimensions.iter()) {
        match sub_item {
            SubscriptItem::Expr(e) => {
                let sub_e = lower_expr_with_cat(e, ctx).0;
                fixed_subs.push((sub_e, dim.element_byte_stride));
            }
            SubscriptItem::AllWildcard(_) => {
                let bound = if let Some(dep_id) = dim.depending_on
                    && let Some(dep_item) = ctx.layout.items.get(dep_id.as_u32() as usize)
                {
                    HirSliceBound::DynamicOdo {
                        dep_place: HirPlace {
                            item: dep_id,
                            static_offset: dep_item.byte_offset,
                            subscripts: Vec::new(),
                            refmod: None,
                            effective_ty: dep_item.ty.clone(),
                            check_mask: ExceptionCheckMask::NONE,
                            span: dref.span,
                        },
                        max_occurs: dim.max,
                    }
                } else {
                    HirSliceBound::StaticCount(dim.max)
                };
                varying_dims.push((bound, dim.element_byte_stride));
            }
        }
    }

    let mut effective_ty = item.ty.clone();
    let hir_refmod = dref.refmod.as_ref().map(|rm| {
        let off = Box::new(lower_expr_with_cat(&rm.offset, ctx).0);
        let len = rm
            .length
            .as_ref()
            .map(|l| Box::new(lower_expr_with_cat(l, ctx).0));
        effective_ty = HirType::Alphanumeric {
            byte_len: item.ty.static_byte_size(),
            justified: JustifiedSpec::Left,
        };
        HirRefMod {
            offset: off,
            length: len,
        }
    });

    validate_intrinsic_arg_category(
        intr_kind,
        orig_fn,
        0,
        effective_ty.category(),
        dref.span,
        ctx,
    );

    Some(HirTableSlice {
        base_place: HirPlace {
            item: item_id,
            static_offset: item.byte_offset,
            subscripts: fixed_subs,
            refmod: hir_refmod,
            effective_ty,
            check_mask: ExceptionCheckMask::NONE,
            span: dref.span,
        },
        varying_dims,
    })
}

/// Validates positional argument category compatibility for `intr_kind` (`0012_intrinsic_functions_argument_type`).
///
/// # Arguments
///
/// * `intr_kind` - Target intrinsic.
/// * `orig_fn` - Display name of the intrinsic.
/// * `idx` - 0-based argument index.
/// * `cat` - Actual [`DataCategory`] of the argument.
/// * `span` - Argument [`SourceSpan`].
/// * `ctx` - Active [`UnitLowerCtx`].
fn validate_intrinsic_arg_category(
    intr_kind: ResolvedIntrinsicKind,
    orig_fn: &str,
    idx: usize,
    cat: DataCategory,
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let requires_numeric = match intr_kind {
        ResolvedIntrinsicKind::Core(
            IntrinsicId::Abs
            | IntrinsicId::Acos
            | IntrinsicId::Asin
            | IntrinsicId::Atan
            | IntrinsicId::Cos
            | IntrinsicId::Exp
            | IntrinsicId::Exp10
            | IntrinsicId::Factorial
            | IntrinsicId::FractionPart
            | IntrinsicId::Integer
            | IntrinsicId::IntegerPart
            | IntrinsicId::Log
            | IntrinsicId::Log10
            | IntrinsicId::Mod
            | IntrinsicId::Rem
            | IntrinsicId::Sign
            | IntrinsicId::Sin
            | IntrinsicId::Sqrt
            | IntrinsicId::Tan
            | IntrinsicId::Annuity
            | IntrinsicId::PresentValue
            | IntrinsicId::Sum
            | IntrinsicId::Mean
            | IntrinsicId::Median
            | IntrinsicId::Midrange
            | IntrinsicId::Range
            | IntrinsicId::Variance
            | IntrinsicId::StandardDeviation
            | IntrinsicId::Random
            | IntrinsicId::IntegerOfDate
            | IntrinsicId::DateOfInteger
            | IntrinsicId::IntegerOfDay
            | IntrinsicId::DayOfInteger
            | IntrinsicId::DateToYyyymmdd
            | IntrinsicId::DayToYyyyddd
            | IntrinsicId::YearToYyyy
            | IntrinsicId::CombinedDatetime
            | IntrinsicId::TestDateYyyymmdd
            | IntrinsicId::TestDayYyyyddd
            | IntrinsicId::CharNational
            | IntrinsicId::BooleanOfInteger,
        ) => true,
        ResolvedIntrinsicKind::Core(IntrinsicId::Char | IntrinsicId::LocaleTimeFromSeconds) => {
            idx == 0
        }
        ResolvedIntrinsicKind::Core(IntrinsicId::FormattedDate) => idx == 1,
        ResolvedIntrinsicKind::Core(
            IntrinsicId::FormattedTime | IntrinsicId::FormattedDatetime | IntrinsicId::BaseConvert,
        ) => idx >= 1,
        _ => false,
    };

    if requires_numeric && cat != DataCategory::Numeric && cat != DataCategory::Index {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            format!("FUNCTION '{orig_fn}' has invalid non-numeric argument"),
        ));
    }
}

/// Validates ISO 8601 date/time format arguments on `FORMATTED-*`, `INTEGER-OF-FORMATTED-DATE`,
/// `SECONDS-FROM-FORMATTED-TIME`, and `TEST-FORMATTED-DATETIME` (`syn_functions/0013`, `0014`, `run_functions/0119`).
///
/// # Arguments
///
/// * `intr_kind` - Target intrinsic.
/// * `orig_fn` - Display name of the intrinsic.
/// * `raw_args` - Raw AST arguments.
/// * `span` - Call span.
/// * `ctx` - Active [`UnitLowerCtx`].
#[expect(
    clippy::too_many_lines,
    reason = "Validates all 7 ISO 8601 formatted date/time intrinsics and variable-format diagnostics."
)]
fn validate_formatted_datetime_args(
    intr_kind: ResolvedIntrinsicKind,
    orig_fn: &str,
    raw_args: &[SubscriptItem],
    span: SourceSpan,
    ctx: &mut UnitLowerCtx<'_>,
) {
    let ResolvedIntrinsicKind::Core(id) = intr_kind else {
        return;
    };
    if !matches!(
        id,
        IntrinsicId::FormattedCurrentDate
            | IntrinsicId::FormattedDate
            | IntrinsicId::FormattedTime
            | IntrinsicId::FormattedDatetime
            | IntrinsicId::IntegerOfFormattedDate
            | IntrinsicId::SecondsFromFormattedTime
            | IntrinsicId::TestFormattedDatetime
    ) {
        return;
    }
    let Some(SubscriptItem::Expr(fmt_expr)) = raw_args.first() else {
        return;
    };
    let Some(fmt_str) = extract_compile_time_string(fmt_expr, ctx) else {
        let is_undef_data = matches!(
            fmt_expr,
            Expr::Data(dref)
                if ctx
                    .symtab
                    .resolve_symbol_silent(ctx.scope, dref.name.base, &dref.name.qualifiers)
                    .is_none()
        );
        let cat = lower_expr_with_cat(fmt_expr, ctx).1;
        let is_valid_str_var = is_undef_data
            || matches!(
                cat,
                DataCategory::Alphanumeric
                    | DataCategory::Alphabetic
                    | DataCategory::AlphanumericEdited
                    | DataCategory::Group
                    | DataCategory::National
            );
        if !is_valid_str_var {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(fmt_expr.span()),
                format!("FUNCTION '{orig_fn}' has invalid date/time format argument"),
            ));
            return;
        }
        ctx.sink.emit(Diagnostic::warning(
            DiagnosticCode::W0006GeneralWarning,
            WarningGroup::CallParams,
            Some(span),
            format!("FUNCTION '{orig_fn}' has format in variable"),
        ));
        return;
    };

    let wrong_dp = match ctx.decimal_point {
        DecimalPointKind::Period => fmt_str.contains(','),
        DecimalPointKind::Comma => fmt_str.contains('.'),
    };
    if wrong_dp {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            format!("FUNCTION '{orig_fn}' has invalid date/time format"),
        ));
        return;
    }

    let zero = coblang_decimal::FixedDecimal::ZERO;
    let valid = match id {
        IntrinsicId::FormattedDate => dt_formatted_date(&fmt_str, 1).is_ok(),
        IntrinsicId::IntegerOfFormattedDate => {
            dt_formatted_date(&fmt_str, 1).is_ok()
                || dt_formatted_datetime(&fmt_str, 1, &zero, None, None).is_ok()
                || dt_formatted_datetime(&fmt_str, 1, &zero, Some(0), None).is_ok()
        }
        IntrinsicId::FormattedCurrentDate => {
            fmt_str.contains('T')
                && dt_formatted_current_date(&fmt_str, ClockSnapshot::from_unix_utc(0, 0), None)
                    .is_ok()
        }
        IntrinsicId::FormattedTime => {
            let has_sys_off = raw_args.iter().skip(2).any(|s| {
                matches!(s, SubscriptItem::Expr(e) if is_keyword_word_expr(e, "SYSTEM-OFFSET", ctx.interner))
            });
            if has_sys_off {
                dt_formatted_time(&fmt_str, &zero, None, Some(0)).is_ok()
                    || dt_formatted_time(&fmt_str, &zero, Some(0), None).is_ok()
            } else {
                let has_exp = raw_args.len() >= 3;
                if has_exp {
                    dt_formatted_time(&fmt_str, &zero, Some(0), None).is_ok()
                } else {
                    dt_formatted_time(&fmt_str, &zero, None, None).is_ok()
                        || dt_formatted_time(&fmt_str, &zero, Some(0), None).is_ok()
                }
            }
        }
        IntrinsicId::SecondsFromFormattedTime => {
            dt_formatted_time(&fmt_str, &zero, None, None).is_ok()
                || dt_formatted_time(&fmt_str, &zero, Some(0), None).is_ok()
                || dt_formatted_datetime(&fmt_str, 1, &zero, None, None).is_ok()
                || dt_formatted_datetime(&fmt_str, 1, &zero, Some(0), None).is_ok()
        }
        IntrinsicId::FormattedDatetime => {
            let has_sys_off = raw_args.iter().skip(3).any(|s| {
                matches!(s, SubscriptItem::Expr(e) if is_keyword_word_expr(e, "SYSTEM-OFFSET", ctx.interner))
            });
            if has_sys_off {
                dt_formatted_datetime(&fmt_str, 1, &zero, None, Some(0)).is_ok()
                    || dt_formatted_datetime(&fmt_str, 1, &zero, Some(0), None).is_ok()
            } else {
                let has_exp = raw_args.len() >= 4;
                if has_exp {
                    dt_formatted_datetime(&fmt_str, 1, &zero, Some(0), None).is_ok()
                } else {
                    dt_formatted_datetime(&fmt_str, 1, &zero, None, None).is_ok()
                        || dt_formatted_datetime(&fmt_str, 1, &zero, Some(0), None).is_ok()
                }
            }
        }
        _ => {
            dt_formatted_date(&fmt_str, 1).is_ok()
                || dt_formatted_time(&fmt_str, &zero, None, None).is_ok()
                || dt_formatted_time(&fmt_str, &zero, Some(0), None).is_ok()
                || dt_formatted_datetime(&fmt_str, 1, &zero, None, None).is_ok()
                || dt_formatted_datetime(&fmt_str, 1, &zero, Some(0), None).is_ok()
        }
    };

    if !valid {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            format!("FUNCTION '{orig_fn}' has invalid date/time format"),
        ));
    }
}

/// Extracts a compile-time string literal or `01 CONSTANT` string from `expr`.
///
/// # Arguments
///
/// * `expr` - Candidate AST expression.
/// * `ctx` - Active [`UnitLowerCtx`].
#[must_use]
fn extract_compile_time_string(expr: &Expr, ctx: &UnitLowerCtx<'_>) -> Option<String> {
    match expr {
        Expr::Literal {
            atom: ValueAtom::String { bytes, .. },
            ..
        } => String::from_utf8(bytes.clone()).ok(),
        Expr::Data(dref) => match lookup_constant_silent(dref, ctx)? {
            ResolvedSymbol::Constant(HirConstValue::Bytes(b)) => String::from_utf8(b).ok(),
            _ => None,
        },
        _ => None,
    }
}

/// Computes `(return_ty, return_cat, static_char_len_opt, allows_refmod)` for `intr_kind`.
///
/// # Arguments
///
/// * `intr_kind` - Resolved intrinsic kind.
/// * `args` - Lowered [`HirM12Arg`] slice.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "Computes exact return HirType, static character length, and refmod eligibility across all intrinsics."
)]
fn compute_intrinsic_return_type(
    intr_kind: ResolvedIntrinsicKind,
    args: &[HirM12Arg],
) -> (HirType, DataCategory, Option<u32>, bool) {
    let int_ty = HirType::BinaryInt {
        byte_width: 8,
        digits: DigitCount::try_new(18).unwrap_or(DigitCount::ONE),
        scale: ScaleFactor::ZERO,
        sign: Signedness::Signed,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::FullBinaryRange,
    };
    let dec_ty = HirType::PackedDecimal {
        digits: DigitCount::try_new(36).unwrap_or(DigitCount::ONE),
        scale: ScaleFactor::try_new(18).unwrap_or(ScaleFactor::ZERO),
        sign: Signedness::Signed,
        unsigned_comp6: Comp6Mode::StandardComp3,
    };

    if intr_kind == ResolvedIntrinsicKind::Core(IntrinsicId::Abs)
        && let Some(HirM12Arg::Expr(HirExpr::Place(pl))) = args.first()
        && let Some((d, s, _)) = extract_numeric_algebraic_spec(&pl.effective_ty)
        && (1..=38).contains(&d)
    {
        let abs_ty = HirType::ZonedDecimal {
            digits: DigitCount::try_new(u8::try_from(d).unwrap_or(36)).unwrap_or(DigitCount::ONE),
            scale: ScaleFactor::try_new(s).unwrap_or(ScaleFactor::ZERO),
            sign: NumericSignSpec::EmbeddedTrailing,
            blank_zero: BlankZeroSpec::RetainZero,
        };
        return (abs_ty, DataCategory::Numeric, None, false);
    }

    match intr_kind {
        ResolvedIntrinsicKind::Ext(HirExtIntrinsic::ContentLength) => {
            (int_ty, DataCategory::Numeric, None, false)
        }
        ResolvedIntrinsicKind::Ext(_) => (
            HirType::Alphanumeric {
                byte_len: 1,
                justified: JustifiedSpec::Left,
            },
            DataCategory::Alphanumeric,
            Some(1),
            false,
        ),
        ResolvedIntrinsicKind::Core(IntrinsicId::Max | IntrinsicId::Min) => {
            let first_cat = first_series_arg_category(args);
            if matches!(
                first_cat,
                DataCategory::Alphanumeric | DataCategory::Alphabetic | DataCategory::National
            ) {
                let cat = if first_cat == DataCategory::National {
                    DataCategory::National
                } else {
                    DataCategory::Alphanumeric
                };
                (
                    HirType::Alphanumeric {
                        byte_len: 256,
                        justified: JustifiedSpec::Left,
                    },
                    cat,
                    None,
                    true,
                )
            } else {
                (dec_ty, DataCategory::Numeric, None, false)
            }
        }
        ResolvedIntrinsicKind::Core(id) => match id.return_kind() {
            IntrinsicReturnKind::Integer => (int_ty, DataCategory::Numeric, None, false),
            IntrinsicReturnKind::Numeric => (dec_ty, DataCategory::Numeric, None, false),
            IntrinsicReturnKind::National => {
                let allows_rm = id != IntrinsicId::CharNational;
                (
                    HirType::National {
                        char_len: 128,
                        justified: JustifiedSpec::Left,
                    },
                    DataCategory::National,
                    (id == IntrinsicId::CharNational).then_some(1),
                    allows_rm,
                )
            }
            IntrinsicReturnKind::Alphanumeric => {
                let static_len = match id {
                    IntrinsicId::Char => Some(1),
                    IntrinsicId::CurrentDate | IntrinsicId::WhenCompiled => Some(21),
                    IntrinsicId::ModuleDate => Some(8),
                    IntrinsicId::ModuleTime => Some(6),
                    IntrinsicId::Uuid4 => Some(36),
                    IntrinsicId::ExceptionStatus => Some(31),
                    _ => None,
                };
                let allows_rm = id != IntrinsicId::Char;
                let byte_len = static_len.unwrap_or(256);
                (
                    HirType::Alphanumeric {
                        byte_len,
                        justified: JustifiedSpec::Left,
                    },
                    DataCategory::Alphanumeric,
                    static_len,
                    allows_rm,
                )
            }
        },
    }
}

/// Determines the [`DataCategory`] of the first argument of `MAX` / `MIN`.
///
/// # Arguments
///
/// * `args` - Lowered [`HirM12Arg`] slice.
#[must_use]
fn first_series_arg_category(args: &[HirM12Arg]) -> DataCategory {
    match args.first() {
        Some(HirM12Arg::TableSlice(ts)) => ts.base_place.effective_ty.category(),
        Some(HirM12Arg::Expr(HirExpr::Place(pl))) => pl.effective_ty.category(),
        Some(HirM12Arg::Expr(HirExpr::Const { value, .. })) => match value {
            HirConstValue::Decimal { .. }
            | HirConstValue::Float(_)
            | HirConstValue::Figurative(HirFigurative::Zero) => DataCategory::Numeric,
            HirConstValue::NationalBytes(_) => DataCategory::National,
            _ => DataCategory::Alphanumeric,
        },
        _ => DataCategory::Numeric,
    }
}

/// Validates reference modification on a function result (`0011_intrinsic_functions_reference_modification`)
/// and lowers `refmod` into `Option<HirRefMod>`.
///
/// # Arguments
///
/// * `orig_fn` - Display name of the function.
/// * `refmod` - Optional AST [`RefModSpec`].
/// * `allows_refmod` - Whether the callee's return category allows reference modification.
/// * `static_max_len` - Known compile-time result length if fixed (`Some(21)` for `CURRENT-DATE`, etc.).
/// * `return_ty` - Mutable result [`HirType`].
/// * `return_cat` - Mutable result [`DataCategory`].
/// * `ctx` - Active [`UnitLowerCtx`].
fn validate_and_lower_fn_refmod(
    orig_fn: &str,
    refmod: Option<&RefModSpec>,
    allows_refmod: bool,
    static_max_len: Option<u32>,
    return_ty: &mut HirType,
    return_cat: &mut DataCategory,
    ctx: &mut UnitLowerCtx<'_>,
) -> Option<HirRefMod> {
    let rm = refmod?;
    if !allows_refmod
        || !matches!(
            *return_cat,
            DataCategory::Alphanumeric | DataCategory::Alphabetic | DataCategory::National
        )
    {
        ctx.sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(rm.span),
            format!("FUNCTION '{orig_fn}' may not be reference modified"),
        ));
        return None;
    }

    let static_off = crate::typecheck::extract_static_int(&rm.offset, ctx.interner);
    let static_len = rm
        .length
        .as_deref()
        .and_then(|l| crate::typecheck::extract_static_int(l, ctx.interner));

    if let Some(off) = static_off {
        if off < 1 {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(rm.span),
                format!("reference modification offset ({off}) must be >= 1"),
            ));
        } else if let Some(max_c) = static_max_len
            && off > i128::from(max_c)
        {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(rm.span),
                format!(
                    "reference modification offset ({off}) exceeds length ({max_c}) of FUNCTION '{orig_fn}'"
                ),
            ));
        }
    }
    if let Some(len) = static_len {
        if len < 1 {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(rm.span),
                format!("reference modification length ({len}) must be >= 1"),
            ));
        } else if let (Some(off), Some(max_c)) = (static_off, static_max_len)
            && off >= 1
            && off.saturating_add(len).saturating_sub(1) > i128::from(max_c)
        {
            ctx.sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(rm.span),
                format!(
                    "reference modification range ({off}:{len}) exceeds length ({max_c}) of FUNCTION '{orig_fn}'"
                ),
            ));
        }
    }

    let off_hir = Box::new(lower_expr_with_cat(&rm.offset, ctx).0);
    let len_hir = rm
        .length
        .as_ref()
        .map(|l| Box::new(lower_expr_with_cat(l, ctx).0));

    if let Some(len_i) = static_len.and_then(|v| u32::try_from(v).ok())
        && len_i >= 1
    {
        *return_ty = if *return_cat == DataCategory::National {
            HirType::National {
                char_len: len_i,
                justified: JustifiedSpec::Left,
            }
        } else {
            HirType::Alphanumeric {
                byte_len: len_i,
                justified: JustifiedSpec::Left,
            }
        };
    }

    Some(HirRefMod {
        offset: off_hir,
        length: len_hir,
    })
}

/// Checks whether `expr` is a bare single-word `DataRef` matching canonical uppercase `kw`.
///
/// # Arguments
///
/// * `expr` - Candidate AST expression.
/// * `kw` - Expected canonical uppercase keyword.
/// * `interner` - Shared [`SymbolInterner`].
#[must_use]
fn is_keyword_word_expr(expr: &Expr, kw: &str, interner: &SymbolInterner) -> bool {
    let Expr::Data(dref) = expr else {
        return false;
    };
    dref.modifier == DataRefModifier::Plain
        && dref.name.qualifiers.is_empty()
        && dref.subscripts.is_empty()
        && dref.refmod.is_none()
        && interner.resolve_canonical(dref.name.base).ok() == Some(kw)
}

/// Produces a harmless fallback numeric `0` expression after emitting a function diagnostic.
///
/// # Arguments
///
/// * `span` - Source span of the invalid function call.
#[must_use]
fn dummy_fn_result(span: SourceSpan) -> (HirExpr, DataCategory) {
    (
        HirExpr::Const {
            value: HirConstValue::Figurative(HirFigurative::Zero),
            span,
        },
        DataCategory::Numeric,
    )
}
