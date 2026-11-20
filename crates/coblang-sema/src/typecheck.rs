//! Semantic validation for files, `MOVE` categories, arithmetic operands, subscripts, refmods, `SET`, and intrinsics.

use coblang_common::{Dialect, ProgramUnitId, ScopeId, SourceSpan, SymbolId};
use coblang_diag::{Diagnostic, DiagnosticCode, DiagnosticSink, DialectConfig, SymbolInterner};
use coblang_hir::{DataCategory, HirDataItem, HirFileConnector, HirType, JustifiedSpec};
use coblang_syntax::ast::{
    CorrespondingMode, DataDivision, DataRef, DataRefModifier, EnvironmentDivision, Expr,
    FigurativeValue, SetAction, SubscriptItem, ValueAtom,
};

use crate::layout::SolvedDataLayout;
use crate::m14_fileio::{FileResolveEnv, resolve_unit_file_connectors};
use crate::symbols::{ResolvedSymbol, SymbolTable};

/// Validates `FILE-CONTROL` (`SELECT`) <-> `FILE SECTION` (`FD`/`SD`) pairing and resolves [`HirFileConnector`]s.
///
/// # Arguments
///
/// * `env` - Parsed `ENVIRONMENT DIVISION`.
/// * `data` - Parsed `DATA DIVISION`.
/// * `layout` - Solved data layout with `FD`/`SD` bindings.
/// * `scope` - Owning lexical scope.
/// * `symtab` - Symbol table.
/// * `interner` - Symbol interner.
/// * `sink` - Diagnostic sink.
#[must_use]
pub fn check_and_resolve_files(
    env: &EnvironmentDivision,
    data: &DataDivision,
    layout: &SolvedDataLayout,
    scope: ScopeId,
    symtab: &SymbolTable,
    interner: &SymbolInterner,
    sink: &mut DiagnosticSink,
) -> Vec<HirFileConnector> {
    let default_dialect = DialectConfig::for_dialect(Dialect::Cobol2023);
    let mut items_scratch = layout.items.clone();
    let fenv = FileResolveEnv {
        unit_id: ProgramUnitId::new(0),
        scope,
        layout,
        dialect: &default_dialect,
        alphabets: &[],
        prog_collating_seq: None,
        symtab,
        interner,
    };
    resolve_unit_file_connectors(env, data, &fenv, &mut items_scratch, sink)
}

/// Validates subscript arity, static literal subscript bounds, and refmod bounds on a [`DataRef`].
///
/// # Arguments
///
/// * `dref` - AST data reference.
/// * `item` - Resolved [`HirDataItem`].
/// * `interner` - Symbol interner for literal parsing.
/// * `sink` - Diagnostic sink.
#[must_use]
pub fn validate_subscripts_and_refmod(
    dref: &DataRef,
    item: &HirDataItem,
    interner: &SymbolInterner,
    sink: &mut DiagnosticSink,
) -> HirType {
    let expected_dims = item.dimensions.len();
    let given_dims = dref.subscripts.len();

    if expected_dims == 0 && given_dims > 0 {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(dref.span),
            String::from("cannot subscript a non-table data item"),
        ));
    } else if expected_dims > 0 && given_dims > 0 && given_dims != expected_dims {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(dref.span),
            format!("table item requires {expected_dims} subscript(s), found {given_dims}"),
        ));
    }

    for (sub, dim) in dref.subscripts.iter().zip(&item.dimensions) {
        if let SubscriptItem::Expr(expr) = sub
            && let Some(const_idx) = extract_static_int(expr, interner)
            && (const_idx < 1 || (dim.max != u32::MAX && const_idx > i128::from(dim.max)))
        {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(expr.span()),
                format!(
                    "subscript `{const_idx}` is out of static bounds `1..={}`",
                    dim.max
                ),
            ));
        }
    }

    if let Some(rm) = &dref.refmod {
        return validate_refmod_slice(rm, item, interner, sink);
    }

    item.ty.clone()
}

/// Validates a `(offset : [length])` reference modification slice and computes its slice [`HirType`].
///
/// # Arguments
///
/// * `rm` - AST reference modification spec.
/// * `item` - Resolved [`HirDataItem`].
/// * `interner` - Symbol interner.
/// * `sink` - Diagnostic sink.
fn validate_refmod_slice(
    rm: &coblang_syntax::ast::RefModSpec,
    item: &HirDataItem,
    interner: &SymbolInterner,
    sink: &mut DiagnosticSink,
) -> HirType {
    let cat = item.ty.category();
    if matches!(
        cat,
        DataCategory::Pointer
            | DataCategory::Index
            | DataCategory::ObjectReference
            | DataCategory::BooleanBit
    ) || matches!(
        item.ty,
        HirType::FloatBinary { .. }
            | HirType::FloatDecimal { .. }
            | HirType::BinaryInt { .. }
            | HirType::PackedDecimal { .. }
    ) {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(rm.span),
            String::from("reference modification is only valid on display/character/group items"),
        ));
    }

    let max_units = match &item.ty {
        HirType::National { char_len, .. } | HirType::NationalEdited { char_len, .. } => {
            i128::from(*char_len)
        }
        _ => i128::from(item.ty.static_byte_size()),
    };

    let off_opt = extract_static_int(&rm.offset, interner);
    if let Some(off) = off_opt
        && (off < 1 || off > max_units)
    {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(rm.span),
            format!("reference-modification offset `{off}` out of bounds `1..={max_units}`"),
        ));
    }
    if let Some(len_expr) = &rm.length
        && let Some(len_val) = extract_static_int(len_expr, interner)
    {
        if len_val < 1 {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(rm.span),
                format!("reference-modification length `{len_val}` must be >= 1"),
            ));
        } else if let Some(off) = off_opt
            && off >= 1
            && off.saturating_add(len_val).saturating_sub(1) > max_units
        {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(rm.span),
                format!(
                    "reference-modification slice `({off}:{len_val})` exceeds item size `{max_units}`"
                ),
            ));
        }
        return make_slice_type(cat, u32::try_from(len_val.max(1)).unwrap_or(1));
    }

    make_slice_type(cat, u32::try_from(max_units.max(1)).unwrap_or(1))
}

/// Builds an `Alphanumeric` or `National` slice type of `units` length.
///
/// # Arguments
///
/// * `cat` - Base item category.
/// * `units` - Slice length in bytes or UTF-16 code units.
#[must_use]
const fn make_slice_type(cat: DataCategory, units: u32) -> HirType {
    if matches!(cat, DataCategory::National | DataCategory::NationalEdited) {
        HirType::National {
            char_len: units,
            justified: JustifiedSpec::Left,
        }
    } else if matches!(cat, DataCategory::Utf8) {
        HirType::Utf8 { byte_len: units }
    } else {
        HirType::Alphanumeric {
            byte_len: units,
            justified: JustifiedSpec::Left,
        }
    }
}

/// Validates ISO COBOL `MOVE` category compatibility (`syn_move.bundle`).
///
/// # Arguments
///
/// * `mode` - `Standard` vs `Corresponding`.
/// * `src_expr` - AST source expression (for figurative constant rules).
/// * `src_cat` - Resolved source [`DataCategory`].
/// * `dst_cat` - Resolved destination [`DataCategory`].
/// * `span` - Statement span.
/// * `sink` - Diagnostic sink.
pub fn validate_move_compatibility(
    mode: CorrespondingMode,
    src_expr: &Expr,
    src_cat: DataCategory,
    dst_cat: DataCategory,
    span: SourceSpan,
    sink: &mut DiagnosticSink,
) {
    if mode == CorrespondingMode::Corresponding {
        if src_cat != DataCategory::Group || dst_cat != DataCategory::Group {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0008TypeMismatch,
                Some(span),
                String::from("`MOVE CORRESPONDING` requires group operands"),
            ));
        }
        return;
    }

    if matches!(
        src_cat,
        DataCategory::Pointer | DataCategory::Index | DataCategory::ObjectReference
    ) || matches!(
        dst_cat,
        DataCategory::Pointer | DataCategory::Index | DataCategory::ObjectReference
    ) {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            String::from("pointer, index, and object-reference items require `SET`, not `MOVE`"),
        ));
        return;
    }

    // Group moves treat the group as alphanumeric bytes and allow any non-pointer partner!
    if src_cat == DataCategory::Group || dst_cat == DataCategory::Group {
        return;
    }

    // Check figurative constant restrictions!
    if let Expr::Literal {
        atom: ValueAtom::Figurative(fig),
        ..
    } = src_expr
    {
        match (fig, dst_cat) {
            (FigurativeValue::Space, DataCategory::Numeric | DataCategory::NumericEdited) => {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(span),
                    String::from("cannot `MOVE SPACE` to a numeric or numeric-edited item"),
                ));
            }
            (FigurativeValue::Zero, DataCategory::Alphabetic) => {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(span),
                    String::from("cannot `MOVE ZERO` to an alphabetic `PIC A` item"),
                ));
            }
            (FigurativeValue::Null, _) => {
                sink.emit(Diagnostic::error(
                    DiagnosticCode::E0008TypeMismatch,
                    Some(span),
                    String::from("`NULL` cannot be used in `MOVE` (use `SET ... TO NULL`)"),
                ));
            }
            _ => {}
        }
        return;
    }

    if crate::m17_charset::validate_m17_move_categories(src_cat, dst_cat, span, sink) {
        return;
    }

    let invalid = matches!(
        (src_cat, dst_cat),
        (
            DataCategory::Numeric | DataCategory::NumericEdited,
            DataCategory::Alphabetic
        ) | (
            DataCategory::Alphabetic,
            DataCategory::Numeric | DataCategory::NumericEdited
        )
    );
    if invalid {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            format!(
                "invalid MOVE statement: incompatible `MOVE` from `{src_cat:?}` to `{dst_cat:?}`"
            ),
        ));
    }
}

/// Validates arithmetic operand (`Numeric`) and destination (`Numeric` or `GIVING NumericEdited`) categories.
///
/// # Arguments
///
/// * `cat` - Operand or target [`DataCategory`].
/// * `allow_edited_giving` - Whether `NumericEdited` is permitted (`GIVING` / `COMPUTE` destination).
/// * `span` - Operand/target span.
/// * `sink` - Diagnostic sink.
pub fn validate_arith_category(
    cat: DataCategory,
    allow_edited_giving: EditedTargetPolicy,
    span: SourceSpan,
    sink: &mut DiagnosticSink,
) {
    let ok = cat == DataCategory::Numeric
        || (allow_edited_giving == EditedTargetPolicy::AllowNumericEdited
            && cat == DataCategory::NumericEdited);
    if !ok {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            format!("arithmetic operand/target must be numeric, found `{cat:?}`"),
        ));
    }
}

/// Whether `NumericEdited` is permitted on an arithmetic destination (`GIVING` / `COMPUTE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditedTargetPolicy {
    /// Strict numeric operand/in-place target (`ADD A TO B`).
    StrictNumeric,
    /// `GIVING` / `COMPUTE` target (`Numeric` or `NumericEdited`).
    AllowNumericEdited,
}

/// Validates a `SET` sub-operation (`syn_set.bundle`).
///
/// # Arguments
///
/// * `action` - Parsed [`SetAction`].
/// * `scope` - Active scope.
/// * `items` - Program data items.
/// * `symtab` - Symbol table.
/// * `interner` - Symbol interner.
/// * `sink` - Diagnostic sink.
pub fn validate_set_action(
    action: &SetAction,
    scope: ScopeId,
    items: &[HirDataItem],
    symtab: &SymbolTable,
    interner: &SymbolInterner,
    sink: &mut DiagnosticSink,
) {
    match action {
        SetAction::ConditionState { targets, .. } => {
            for t in targets {
                let res = symtab.resolve_symbol(
                    scope,
                    t.name.base,
                    &t.name.qualifiers,
                    t.span,
                    interner,
                    sink,
                );
                if let Some(sym_res) = res
                    && !matches!(
                        sym_res,
                        ResolvedSymbol::Condition88(_) | ResolvedSymbol::SwitchCondition { .. }
                    )
                {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0008TypeMismatch,
                        Some(t.span),
                        String::from(
                            "`SET ... TO TRUE/FALSE` requires a level-`88` condition-name",
                        ),
                    ));
                }
            }
        }
        SetAction::StepIndex { targets, .. } => {
            for t in targets {
                if t.modifier == DataRefModifier::AddressOf {
                    continue;
                }
                if let Some(ResolvedSymbol::DataItem(id)) = symtab.resolve_symbol(
                    scope,
                    t.name.base,
                    &t.name.qualifiers,
                    t.span,
                    interner,
                    sink,
                ) && let Some(it) = items.get(id.as_u32() as usize)
                    && !matches!(
                        it.ty.category(),
                        DataCategory::Index | DataCategory::Pointer | DataCategory::Numeric
                    )
                {
                    sink.emit(Diagnostic::error(
                        DiagnosticCode::E0008TypeMismatch,
                        Some(t.span),
                        String::from("`SET ... UP/DOWN BY` requires an index or pointer target"),
                    ));
                }
            }
        }
        SetAction::ToValue { targets, value } => {
            resolve_set_targets_and_rhs(
                targets,
                value,
                AddressOfTargetMode::DeferToPointerPass,
                scope,
                symtab,
                interner,
                sink,
            );
        }
        SetAction::ToEntry { targets, entry } => {
            resolve_set_targets_and_rhs(
                targets,
                entry,
                AddressOfTargetMode::ResolveImmediately,
                scope,
                symtab,
                interner,
                sink,
            );
        }
        SetAction::SwitchState { .. }
        | SetAction::LastExceptionOff
        | SetAction::Environment { .. }
        | SetAction::ScreenAttributes { .. } => {}
    }
}

/// Controls whether `ADDRESS OF` targets on `SET` are deferred to `m11_pointers` or resolved immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddressOfTargetMode {
    /// Defer `ADDRESS OF` targets to `m11_pointers` (`SET ADDRESS OF item TO ...`).
    DeferToPointerPass,
    /// Resolve all targets immediately (`SET prog-ptr TO ENTRY ...`).
    ResolveImmediately,
}

/// Resolves target and right-hand symbol references for `SetAction::ToValue` and `SetAction::ToEntry`.
///
/// # Arguments
///
/// * `targets` - Target [`DataRef`] slice.
/// * `rhs` - Right-hand [`Expr`].
/// * `addr_mode` - [`AddressOfTargetMode`] governing `ADDRESS OF` target resolution.
/// * `scope` - Current lexical scope.
/// * `symtab` - Symbol table.
/// * `interner` - Symbol interner.
/// * `sink` - Diagnostic sink.
fn resolve_set_targets_and_rhs(
    targets: &[DataRef],
    rhs: &Expr,
    addr_mode: AddressOfTargetMode,
    scope: ScopeId,
    symtab: &SymbolTable,
    interner: &SymbolInterner,
    sink: &mut DiagnosticSink,
) {
    for t in targets {
        if addr_mode == AddressOfTargetMode::DeferToPointerPass
            && t.modifier == DataRefModifier::AddressOf
        {
            continue;
        }
        let _ = symtab.resolve_symbol(
            scope,
            t.name.base,
            &t.name.qualifiers,
            t.span,
            interner,
            sink,
        );
    }
    if let Expr::Data(vref) = rhs {
        let _ = symtab.resolve_symbol(
            scope,
            vref.name.base,
            &vref.name.qualifiers,
            vref.span,
            interner,
            sink,
        );
    }
}

/// Validates known ISO / `GnuCOBOL` / `gcobol` intrinsic function name and argument count (`syn_functions.bundle`).
///
/// # Arguments
///
/// * `fn_sym` - Function name symbol.
/// * `arg_count` - Number of supplied arguments.
/// * `span` - Call span.
/// * `interner` - Symbol interner.
/// * `sink` - Diagnostic sink.
pub fn validate_intrinsic_arity(
    fn_sym: SymbolId,
    arg_count: usize,
    span: SourceSpan,
    interner: &SymbolInterner,
    sink: &mut DiagnosticSink,
) {
    let name = interner.resolve_canonical(fn_sym).unwrap_or("");
    let Some((min_args, max_args)) = intrinsic_arity_bounds(name) else {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0004UndefinedSymbol,
            Some(span),
            format!("unknown intrinsic function `{name}`"),
        ));
        return;
    };
    if arg_count < min_args || arg_count > max_args {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0008TypeMismatch,
            Some(span),
            format!(
                "`FUNCTION {name}` expects {min_args}..={max_args} argument(s), found {arg_count}"
            ),
        ));
    }
}

/// Returns `(min_args, max_args)` for all ~105 ISO / `GnuCOBOL` / `gcobol` intrinsics.
///
/// # Arguments
///
/// * `name` - Canonical uppercase intrinsic name.
#[must_use]
pub fn intrinsic_arity_bounds(name: &str) -> Option<(usize, usize)> {
    if is_nullary_intrinsic(name) {
        return Some((0, 0));
    }
    if is_unary_intrinsic(name) {
        return Some((1, 1));
    }
    match name {
        "RANDOM" => Some((0, 1)),
        "MOD"
        | "REM"
        | "ANNUITY"
        | "COMBINED-DATETIME"
        | "FORMATTED-DATE"
        | "INTEGER-OF-FORMATTED-DATE"
        | "SECONDS-FROM-FORMATTED-TIME"
        | "TEST-FORMATTED-DATETIME"
        | "BOOLEAN-OF-INTEGER"
        | "INTEGER-OF-BOOLEAN"
        | "UPOS"
        | "UWIDTH" => Some((2, 2)),
        "TRIM"
        | "DISPLAY-OF"
        | "NATIONAL-OF"
        | "LOCALE-DATE"
        | "LOCALE-TIME"
        | "LOCALE-TIME-FROM-SECONDS" => Some((1, 2)),
        "DATE-TO-YYYYMMDD" | "DAY-TO-YYYYDDD" | "YEAR-TO-YYYY" | "NUMVAL-C" | "TEST-NUMVAL-C" => {
            Some((1, 3))
        }
        "CONVERT" | "BASECONVERT" | "USUBSTR" => Some((3, 3)),
        "LOCALE-COMPARE" => Some((2, 3)),
        "STANDARD-COMPARE" | "FORMATTED-TIME" => Some((2, 4)),
        "FORMATTED-DATETIME" => Some((4, 5)),
        "FIND-STRING" => Some((2, 5)),
        "PRESENT-VALUE" => Some((2, usize::MAX)),
        "MAX" | "MIN" | "ORD-MAX" | "ORD-MIN" | "SUM" | "MEAN" | "MEDIAN" | "MIDRANGE"
        | "RANGE" | "VARIANCE" | "STANDARD-DEVIATION" | "CONCAT" | "CONCATENATE" => {
            Some((1, usize::MAX))
        }
        "SUBSTITUTE" | "SUBSTITUTE-CASE" => Some((3, usize::MAX)),
        _ => None,
    }
}

/// Returns `true` if `name` is a 0-argument intrinsic function.
///
/// # Arguments
///
/// * `name` - Canonical uppercase intrinsic name.
#[must_use]
fn is_nullary_intrinsic(name: &str) -> bool {
    matches!(
        name,
        "E" | "PI"
            | "CURRENT-DATE"
            | "WHEN-COMPILED"
            | "SECONDS-PAST-MIDNIGHT"
            | "MODULE-CALLER-ID"
            | "MODULE-DATE"
            | "MODULE-FORMATTED-DATE"
            | "MODULE-ID"
            | "MODULE-NAME"
            | "MODULE-PATH"
            | "MODULE-SOURCE"
            | "MODULE-TIME"
            | "EXCEPTION-FILE"
            | "EXCEPTION-FILE-N"
            | "EXCEPTION-LOCATION"
            | "EXCEPTION-LOCATION-N"
            | "EXCEPTION-STATEMENT"
            | "EXCEPTION-STATUS"
            | "CURRENCY-SYMBOL"
            | "UUID4"
    )
}

/// Returns `true` if `name` is a 1-argument intrinsic function.
///
/// # Arguments
///
/// * `name` - Canonical uppercase intrinsic name.
#[must_use]
fn is_unary_intrinsic(name: &str) -> bool {
    matches!(
        name,
        "ABS"
            | "ACOS"
            | "ASIN"
            | "ATAN"
            | "COS"
            | "EXP"
            | "EXP10"
            | "FACTORIAL"
            | "FRACTION-PART"
            | "HIGHEST-ALGEBRAIC"
            | "INTEGER"
            | "INTEGER-PART"
            | "LOG"
            | "LOG10"
            | "LOWEST-ALGEBRAIC"
            | "SIGN"
            | "SIN"
            | "SMALLEST-ALGEBRAIC"
            | "SQRT"
            | "TAN"
            | "INTEGER-OF-DATE"
            | "DATE-OF-INTEGER"
            | "INTEGER-OF-DAY"
            | "DAY-OF-INTEGER"
            | "BYTE-LENGTH"
            | "LENGTH"
            | "LENGTH-AN"
            | "CHAR"
            | "CHAR-NATIONAL"
            | "ORD"
            | "REVERSE"
            | "UPPER-CASE"
            | "LOWER-CASE"
            | "NUMVAL"
            | "NUMVAL-F"
            | "TEST-NUMVAL"
            | "TEST-NUMVAL-F"
            | "TEST-DATE-YYYYMMDD"
            | "TEST-DAY-YYYYDDD"
            | "BIT-OF"
            | "BIT-TO-CHAR"
            | "HEX-OF"
            | "HEX-TO-CHAR"
            | "STORED-CHAR-LENGTH"
            | "ULENGTH"
            | "USUPPLEMENTARY"
            | "UVALID"
            | "CONTENT-OF"
            | "FORMATTED-CURRENT-DATE"
    )
}

/// Evaluates simple compile-time integer literal or unary `+`/`-` integer expressions for static bounds checking.
///
/// # Arguments
///
/// * `expr` - Candidate AST expression.
/// * `interner` - Symbol interner.
#[must_use]
pub(crate) fn extract_static_int(expr: &Expr, interner: &SymbolInterner) -> Option<i128> {
    match expr {
        Expr::Literal {
            atom: ValueAtom::Integer(sym),
            ..
        } => interner.resolve_original(*sym).ok()?.parse::<i128>().ok(),
        Expr::Unary { op, operand, .. } => {
            let v = extract_static_int(operand, interner)?;
            match op {
                coblang_syntax::ast::UnaryOp::Plus => Some(v),
                coblang_syntax::ast::UnaryOp::Minus => Some(-v),
            }
        }
        _ => None,
    }
}
