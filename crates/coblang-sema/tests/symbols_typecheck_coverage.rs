//! Clean-room unit, branch, and end-to-end coverage tests for `symbols.rs` and `typecheck.rs`.

#![deny(clippy::indexing_slicing)]

use std::path::Path;

use coblang_common::{
    ByteLength, ByteOffset, DataItemId, Dialect, DigitCount, ExpansionId, FileConnectorId, FileId,
    LevelNumber, ProcedureId, Result, ScaleFactor, ScopeId, SourceFormat, SourceSpan, SymbolId,
};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap, SymbolInterner, WarningPolicy};
use coblang_hir::{
    BinaryEndianness, BlankZeroSpec, Comp6Mode, DataCategory, FloatBinaryWidth, FloatDecimalWidth,
    HirConstValue, HirDataItem, HirFigurative, HirOccursDim, HirStorageSection, HirTruthValue,
    HirType, JustifiedSpec, NumericSignSpec, PicEditOp, ResolvedHirModule, Signedness,
    TruncationPolicy,
};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, preprocess_source_file,
};
use coblang_sema::typecheck::{
    EditedTargetPolicy, intrinsic_arity_bounds, validate_arith_category, validate_intrinsic_arity,
    validate_move_compatibility, validate_set_action, validate_subscripts_and_refmod,
};
use coblang_sema::{
    ProcedureEntry, ResolvedSymbol, SymbolCandidate, SymbolTable, lower_compilation_group,
};
use coblang_syntax::ast::{
    CorrespondingMode, DataRef, DataRefModifier, Expr, FigurativeValue, GlobalVisibility,
    QualifiedName, RefModSpec, SetAction, StepDirection, SubscriptItem, TruthValue, UnaryOp,
    ValueAtom,
};
use coblang_syntax::parse_preprocessed_unit;

/// Returns a synthetic [`SourceSpan`] for semantic unit tests.
fn span() -> SourceSpan {
    SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::new(4),
        ExpansionId::ROOT,
    )
}

/// Compiles clean-room free-format COBOL source `src` under `dialect` and returns `(ResolvedHirModule, DiagnosticSink)`.
///
/// # Arguments
///
/// * `src` - Clean-room COBOL source text.
/// * `dialect` - Target COBOL dialect.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError`] if preprocessing, parsing, or semantic lowering fails fatally.
fn compile_cobol(
    src: &str,
    dialect: Dialect,
) -> Result<(Option<ResolvedHirModule>, DiagnosticSink)> {
    let dcfg = DialectConfig::for_dialect(dialect);
    let mem = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dcfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm = SourceMap::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 200);
    let unit = preprocess_source_file(
        "sema_test.cob",
        Path::new("."),
        src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let mut interner = SymbolInterner::new();
    let group = parse_preprocessed_unit(&unit, &dcfg, &mut interner, &mut sink)?;
    let hir = lower_compilation_group(&group, &dcfg, &mut interner, &mut sink).ok();
    Ok((hir, sink))
}

/// Builds a synthetic [`HirDataItem`] with `ty` and `dimensions` for direct typecheck tests.
///
/// # Arguments
///
/// * `id_raw` - Raw data item index.
/// * `ty` - Resolved HIR type.
/// * `dimensions` - Table dimensions.
fn make_item(id_raw: u32, ty: HirType, dimensions: Vec<HirOccursDim>) -> HirDataItem {
    let total_byte_size = ty.static_byte_size();
    HirDataItem {
        id: DataItemId::new(id_raw),
        scope: ScopeId::new(0),
        name: None,
        level: LevelNumber::Independent77,
        parent: None,
        children: Vec::new(),
        section: HirStorageSection::WorkingStorage,
        ty,
        byte_offset: ByteOffset::ZERO,
        total_byte_size,
        dimensions,
        redefines: None,
        initial_value: None,
        span: span(),
    }
}

/// Builds a simple [`DataRef`] with optional subscripts, refmod, and modifier.
///
/// # Arguments
///
/// * `base` - Base identifier symbol.
/// * `subscripts` - Subscript items.
/// * `refmod` - Optional reference-modification slice.
/// * `modifier` - Normal vs `ADDRESS OF`.
fn make_dref(
    base: SymbolId,
    subscripts: Vec<SubscriptItem>,
    refmod: Option<RefModSpec>,
    modifier: DataRefModifier,
) -> DataRef {
    DataRef {
        name: QualifiedName {
            base,
            qualifiers: Vec::new(),
            span: span(),
        },
        subscripts,
        refmod,
        modifier,
        span: span(),
    }
}

/// Tests `SymbolTable` scope creation, invalid scope lookups, `GLOBAL` propagation, and qualifier matching.
#[test]
fn symbol_table_scopes_qualifiers_and_global_resolution() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let mut symtab = SymbolTable::default();
    let parent = symtab.create_scope(None);
    let child = symtab.create_scope(Some(parent));
    let invalid_scope = ScopeId::new(999);

    assert!(symtab.scope(invalid_scope).is_none());
    assert!(symtab.scope_mut(invalid_scope).is_none());

    let s_item = interner.intern("FLD")?;
    let s_grp1 = interner.intern("GRP1")?;
    let s_grp2 = interner.intern("GRP2")?;
    let s_top = interner.intern("TOP")?;
    let s_local_only = interner.intern("LOC-ONLY")?;

    // Inserting into an invalid scope is a safe no-op.
    symtab.insert_symbol(
        invalid_scope,
        s_item,
        SymbolCandidate {
            target: ResolvedSymbol::Constant(HirConstValue::Figurative(HirFigurative::Zero)),
            qualifiers: Vec::new(),
            visibility: GlobalVisibility::Global,
        },
    );

    // Register local-only and global symbols in parent.
    symtab.insert_symbol(
        parent,
        s_local_only,
        SymbolCandidate {
            target: ResolvedSymbol::DataItem(DataItemId::new(1)),
            qualifiers: Vec::new(),
            visibility: GlobalVisibility::Local,
        },
    );
    symtab.insert_symbol(
        parent,
        s_item,
        SymbolCandidate {
            target: ResolvedSymbol::DataItem(DataItemId::new(2)),
            qualifiers: vec![s_grp1, s_top],
            visibility: GlobalVisibility::Global,
        },
    );

    // Inject special registers into parent and verify lookup.
    let mut reg_items = Vec::new();
    symtab.inject_special_registers(parent, &mut reg_items, &mut interner)?;
    assert!(reg_items.len() >= 22);

    // Child cannot see parent's Local symbol, but CAN see parent's Global symbol with partial or exact qualifiers.
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    assert!(
        symtab
            .resolve_symbol(child, s_local_only, &[], span(), &interner, &mut sink)
            .is_none()
    );
    assert!(sink.has_errors());

    let mut sink_ok = DiagnosticSink::new(WarningPolicy::new(), 50);
    let r_partial = symtab.resolve_symbol(child, s_item, &[s_top], span(), &interner, &mut sink_ok);
    assert_eq!(
        r_partial,
        Some(ResolvedSymbol::DataItem(DataItemId::new(2)))
    );
    let r_exact = symtab.resolve_symbol(
        child,
        s_item,
        &[s_grp1, s_top],
        span(),
        &interner,
        &mut sink_ok,
    );
    assert_eq!(r_exact, Some(ResolvedSymbol::DataItem(DataItemId::new(2))));
    assert!(!sink_ok.has_errors());

    // Non-matching qualifier on child candidate falls back to parent Global match.
    symtab.insert_symbol(
        child,
        s_item,
        SymbolCandidate {
            target: ResolvedSymbol::DataItem(DataItemId::new(3)),
            qualifiers: vec![s_grp2],
            visibility: GlobalVisibility::Local,
        },
    );
    let r_fallback =
        symtab.resolve_symbol(child, s_item, &[s_grp1], span(), &interner, &mut sink_ok);
    assert_eq!(
        r_fallback,
        Some(ResolvedSymbol::DataItem(DataItemId::new(2)))
    );
    assert!(!sink_ok.has_errors());

    Ok(())
}

/// Tests `SymbolTable` exact vs partial ambiguity branches, invalid `ScopeId` walk, and bogus `SymbolId` labels.
#[test]
fn symbol_table_ambiguity_and_bogus_symbol_branches() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let mut symtab = SymbolTable::default();
    let sc = symtab.create_scope(Some(ScopeId::new(777)));

    let s_dup = interner.intern("DUP")?;
    let s_g1 = interner.intern("G1")?;
    let s_g2 = interner.intern("G2")?;
    let bogus_sym = SymbolId::new(50_000);

    symtab.insert_symbol(
        sc,
        s_dup,
        SymbolCandidate {
            target: ResolvedSymbol::File(FileConnectorId::new(0)),
            qualifiers: vec![s_g1],
            visibility: GlobalVisibility::Local,
        },
    );
    symtab.insert_symbol(
        sc,
        s_dup,
        SymbolCandidate {
            target: ResolvedSymbol::File(FileConnectorId::new(1)),
            qualifiers: vec![s_g1, s_g2],
            visibility: GlobalVisibility::Local,
        },
    );
    let mut sink1 = DiagnosticSink::new(WarningPolicy::new(), 50);
    let exact_win = symtab.resolve_symbol(sc, s_dup, &[s_g1], span(), &interner, &mut sink1);
    assert_eq!(
        exact_win,
        Some(ResolvedSymbol::File(FileConnectorId::new(0)))
    );
    assert!(!sink1.has_errors());

    let mut sink2 = DiagnosticSink::new(WarningPolicy::new(), 50);
    let amb_partial = symtab.resolve_symbol(sc, s_dup, &[], span(), &interner, &mut sink2);
    assert_eq!(
        amb_partial,
        Some(ResolvedSymbol::File(FileConnectorId::new(0)))
    );
    assert!(sink2.has_errors());

    for cond_idx in [0_usize, 1] {
        symtab.insert_symbol(
            sc,
            bogus_sym,
            SymbolCandidate {
                target: ResolvedSymbol::Condition88(cond_idx),
                qualifiers: Vec::new(),
                visibility: GlobalVisibility::Local,
            },
        );
    }
    let mut sink3 = DiagnosticSink::new(WarningPolicy::new(), 50);
    let amb_exact = symtab.resolve_symbol(sc, bogus_sym, &[], span(), &interner, &mut sink3);
    assert_eq!(amb_exact, Some(ResolvedSymbol::Condition88(0)));
    assert!(sink3.has_errors());

    let mut sink4 = DiagnosticSink::new(WarningPolicy::new(), 50);
    assert!(
        symtab
            .resolve_symbol(
                sc,
                SymbolId::new(50_001),
                &[],
                span(),
                &interner,
                &mut sink4
            )
            .is_none()
    );
    assert!(sink4.has_errors());

    Ok(())
}

/// Tests `SymbolTable::resolve_procedure` across invalid scopes, qualified sections, undefined, and ambiguous labels.
#[test]
fn symbol_table_procedure_resolution_branches() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let mut symtab = SymbolTable::default();
    let sc = symtab.create_scope(None);
    let bogus_sym = SymbolId::new(50_000);

    let p_para = interner.intern("P-WORK")?;
    let p_sec1 = interner.intern("SEC-A")?;
    let p_sec2 = interner.intern("SEC-B")?;
    if let Some(scope_mut) = symtab.scope_mut(sc) {
        for (raw_id, name, section) in [
            (0_u32, p_para, Some(p_sec1)),
            (1, p_para, Some(p_sec2)),
            (2, bogus_sym, None),
            (3, bogus_sym, None),
        ] {
            scope_mut.procedures.push(ProcedureEntry {
                id: ProcedureId::new(raw_id),
                name,
                section,
            });
        }
    }
    let mut psink = DiagnosticSink::new(WarningPolicy::new(), 50);
    assert!(
        symtab
            .resolve_procedure(
                ScopeId::new(888),
                p_para,
                None,
                span(),
                &interner,
                &mut psink
            )
            .is_none()
    );
    assert_eq!(
        symtab.resolve_procedure(sc, p_para, Some(p_sec2), span(), &interner, &mut psink),
        Some(ProcedureId::new(1))
    );
    assert!(!psink.has_errors());

    assert_eq!(
        symtab.resolve_procedure(sc, p_para, None, span(), &interner, &mut psink),
        Some(ProcedureId::new(0))
    );
    assert_eq!(
        symtab.resolve_procedure(sc, bogus_sym, None, span(), &interner, &mut psink),
        Some(ProcedureId::new(2))
    );
    assert!(
        symtab
            .resolve_procedure(sc, p_sec1, None, span(), &interner, &mut psink)
            .is_none()
    );
    assert!(
        symtab
            .resolve_procedure(
                sc,
                SymbolId::new(50_002),
                None,
                span(),
                &interner,
                &mut psink
            )
            .is_none()
    );

    Ok(())
}

/// Tests `validate_subscripts_and_refmod` across scalar items and fixed-bound table subscripts.
#[test]
fn subscript_arity_and_fixed_bounds_branches() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let base = interner.intern("ARR")?;
    let s_six = interner.intern("6")?;
    let s_bad_int = interner.intern("NOTANINT")?;
    let mk_int = |sym: SymbolId| Expr::Literal {
        atom: ValueAtom::Integer(sym),
        span: span(),
    };

    let scalar = make_item(
        0,
        HirType::Alphanumeric {
            byte_len: 10,
            justified: JustifiedSpec::Left,
        },
        Vec::new(),
    );
    let table_fixed = make_item(
        1,
        HirType::National {
            char_len: 5,
            justified: JustifiedSpec::Left,
        },
        vec![HirOccursDim {
            min: 1,
            max: 4,
            element_byte_stride: 10,
            depending_on: None,
            keys: Vec::new(),
            indexed_by: Vec::new(),
        }],
    );

    let wildcard_dref = make_dref(
        base,
        vec![SubscriptItem::AllWildcard(span())],
        None,
        DataRefModifier::Plain,
    );
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 100);
    let _ = validate_subscripts_and_refmod(&wildcard_dref, &scalar, &interner, &mut sink);
    assert!(sink.has_errors());

    let mut ok_sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let _ = validate_subscripts_and_refmod(&wildcard_dref, &table_fixed, &interner, &mut ok_sink);
    assert!(!ok_sink.has_errors());

    for sub_expr in [
        mk_int(s_six),
        mk_int(s_bad_int),
        mk_int(SymbolId::new(60_000)),
        Expr::Literal {
            atom: ValueAtom::Figurative(FigurativeValue::Zero),
            span: span(),
        },
    ] {
        let _ = validate_subscripts_and_refmod(
            &make_dref(
                base,
                vec![SubscriptItem::Expr(sub_expr)],
                None,
                DataRefModifier::Plain,
            ),
            &table_fixed,
            &interner,
            &mut sink,
        );
    }

    Ok(())
}

/// Tests `validate_subscripts_and_refmod` across unbounded table dimensions and unary literal subscripts.
#[test]
fn subscript_unbounded_and_unary_bounds_branches() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let base = interner.intern("ARR")?;
    let s_two = interner.intern("2")?;
    let s_six = interner.intern("6")?;
    let mk_int = |sym: SymbolId| Expr::Literal {
        atom: ValueAtom::Integer(sym),
        span: span(),
    };

    let table_unbounded = make_item(
        2,
        HirType::NationalEdited {
            char_len: 6,
            edit_ops: vec![PicEditOp::NationalSlot],
        },
        vec![
            HirOccursDim {
                min: 1,
                max: u32::MAX,
                element_byte_stride: 36,
                depending_on: None,
                keys: Vec::new(),
                indexed_by: Vec::new(),
            },
            HirOccursDim {
                min: 1,
                max: 3,
                element_byte_stride: 12,
                depending_on: None,
                keys: Vec::new(),
                indexed_by: Vec::new(),
            },
        ],
    );

    let plain_dref = |subs| make_dref(base, subs, None, DataRefModifier::Plain);
    let mut ok_sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let _ = validate_subscripts_and_refmod(
        &plain_dref(Vec::new()),
        &table_unbounded,
        &interner,
        &mut ok_sink,
    );
    assert!(!ok_sink.has_errors());

    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 100);
    let _ = validate_subscripts_and_refmod(
        &plain_dref(vec![SubscriptItem::Expr(mk_int(s_two))]),
        &table_unbounded,
        &interner,
        &mut sink,
    );

    let plus_six = Expr::Unary {
        op: UnaryOp::Plus,
        operand: Box::new(mk_int(s_six)),
        span: span(),
    };
    let minus_two = Expr::Unary {
        op: UnaryOp::Minus,
        operand: Box::new(mk_int(s_two)),
        span: span(),
    };
    let _ = validate_subscripts_and_refmod(
        &plain_dref(vec![
            SubscriptItem::Expr(plus_six),
            SubscriptItem::Expr(minus_two),
        ]),
        &table_unbounded,
        &interner,
        &mut sink,
    );
    assert!(sink.has_errors());

    Ok(())
}

/// Tests `validate_refmod_slice` across `Alphanumeric`, `National`, and `NationalEdited` static/dynamic bounds.
#[test]
fn refmod_bounds_and_slice_types() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let base = interner.intern("STR")?;
    let s_zero = interner.intern("0")?;
    let s_two = interner.intern("2")?;
    let s_six = interner.intern("6")?;
    let mk_int = |sym: SymbolId| Expr::Literal {
        atom: ValueAtom::Integer(sym),
        span: span(),
    };

    let alpha = make_item(
        0,
        HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        },
        Vec::new(),
    );
    let nat = make_item(
        1,
        HirType::National {
            char_len: 4,
            justified: JustifiedSpec::Left,
        },
        Vec::new(),
    );
    let nat_ed = make_item(
        2,
        HirType::NationalEdited {
            char_len: 4,
            edit_ops: vec![PicEditOp::NationalSlot],
        },
        Vec::new(),
    );

    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 100);
    let dyn_expr = Expr::Data(make_dref(base, Vec::new(), None, DataRefModifier::Plain));
    for (off, len) in [
        (mk_int(s_two), None),
        (mk_int(s_two), Some(mk_int(s_two))),
        (mk_int(s_zero), Some(mk_int(s_two))),
        (mk_int(s_six), None),
        (mk_int(s_two), Some(mk_int(s_zero))),
        (mk_int(s_two), Some(mk_int(s_six))),
        (dyn_expr.clone(), Some(mk_int(s_two))),
        (mk_int(s_two), Some(dyn_expr)),
    ] {
        let rm = RefModSpec {
            offset: Box::new(off),
            length: len.map(Box::new),
            span: span(),
        };
        for item in [&alpha, &nat, &nat_ed] {
            let _ = validate_subscripts_and_refmod(
                &make_dref(base, Vec::new(), Some(rm.clone()), DataRefModifier::Plain),
                item,
                &interner,
                &mut sink,
            );
        }
    }

    Ok(())
}

/// Tests `validate_refmod_slice` rejection across all non-character computational/pointer/bit `HirType` variants.
#[test]
fn refmod_rejects_computational_and_handle_types() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let base = interner.intern("X")?;
    let s_one = interner.intern("1")?;
    let d1 = DigitCount::ONE;
    let sc0 = ScaleFactor::ZERO;

    let forbidden_types = [
        HirType::Pointer,
        HirType::ProgramPointer,
        HirType::Index,
        HirType::ObjectRef { class_sym: None },
        HirType::Bit { bit_len: 8 },
        HirType::FloatBinary {
            width: FloatBinaryWidth::Bits64,
        },
        HirType::FloatDecimal {
            width: FloatDecimalWidth::Decimal64,
        },
        HirType::BinaryInt {
            byte_width: 4,
            digits: d1,
            scale: sc0,
            sign: Signedness::Signed,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
        },
        HirType::PackedDecimal {
            digits: d1,
            scale: sc0,
            sign: Signedness::Signed,
            unsigned_comp6: Comp6Mode::StandardComp3,
        },
    ];

    let rm = RefModSpec {
        offset: Box::new(Expr::Literal {
            atom: ValueAtom::Integer(s_one),
            span: span(),
        }),
        length: None,
        span: span(),
    };
    for ty in forbidden_types {
        let item = make_item(0, ty, Vec::new());
        let mut sink = DiagnosticSink::new(WarningPolicy::new(), 10);
        let _ = validate_subscripts_and_refmod(
            &make_dref(base, Vec::new(), Some(rm.clone()), DataRefModifier::Plain),
            &item,
            &interner,
            &mut sink,
        );
        assert!(sink.has_errors());
    }

    // ZonedDecimal is a display item and allows reference modification.
    let zoned = make_item(
        1,
        HirType::ZonedDecimal {
            digits: d1,
            scale: sc0,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        },
        Vec::new(),
    );
    let mut ok_sink = DiagnosticSink::new(WarningPolicy::new(), 10);
    let _ = validate_subscripts_and_refmod(
        &make_dref(base, Vec::new(), Some(rm), DataRefModifier::Plain),
        &zoned,
        &interner,
        &mut ok_sink,
    );
    assert!(!ok_sink.has_errors());

    Ok(())
}

/// Tests `validate_move_compatibility` across `CORRESPONDING`, group, pointer, index, and object-reference categories.
#[test]
fn move_corresponding_group_and_handle_category_matrix() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let sym_num = interner.intern("N1")?;
    let non_fig = Expr::Data(make_dref(sym_num, Vec::new(), None, DataRefModifier::Plain));

    for (mode, src_c, dst_c, expect_err) in [
        (
            CorrespondingMode::Corresponding,
            DataCategory::Group,
            DataCategory::Group,
            false,
        ),
        (
            CorrespondingMode::Corresponding,
            DataCategory::Alphanumeric,
            DataCategory::Group,
            true,
        ),
        (
            CorrespondingMode::Corresponding,
            DataCategory::Group,
            DataCategory::Alphanumeric,
            true,
        ),
        (
            CorrespondingMode::Standard,
            DataCategory::Pointer,
            DataCategory::Alphanumeric,
            true,
        ),
        (
            CorrespondingMode::Standard,
            DataCategory::Alphanumeric,
            DataCategory::Index,
            true,
        ),
        (
            CorrespondingMode::Standard,
            DataCategory::ObjectReference,
            DataCategory::Group,
            true,
        ),
        (
            CorrespondingMode::Standard,
            DataCategory::Group,
            DataCategory::Numeric,
            false,
        ),
        (
            CorrespondingMode::Standard,
            DataCategory::Numeric,
            DataCategory::Group,
            false,
        ),
    ] {
        let mut s = DiagnosticSink::new(WarningPolicy::new(), 10);
        validate_move_compatibility(mode, &non_fig, src_c, dst_c, span(), &mut s);
        assert_eq!(
            s.has_errors(),
            expect_err,
            "case {mode:?} {src_c:?} -> {dst_c:?}"
        );
    }

    Ok(())
}

/// Tests `validate_move_compatibility` across figurative constants (`SPACE`, `ZERO`, `NULL`, and `HIGH-VALUE`).
#[test]
fn move_figurative_category_matrix() {
    let fig_expr = |f: FigurativeValue| Expr::Literal {
        atom: ValueAtom::Figurative(f),
        span: span(),
    };

    for (fig, src_c, dst_c, expect_err) in [
        (
            FigurativeValue::Space,
            DataCategory::Alphanumeric,
            DataCategory::Numeric,
            true,
        ),
        (
            FigurativeValue::Space,
            DataCategory::Alphanumeric,
            DataCategory::NumericEdited,
            true,
        ),
        (
            FigurativeValue::Space,
            DataCategory::Alphanumeric,
            DataCategory::Alphabetic,
            false,
        ),
        (
            FigurativeValue::Zero,
            DataCategory::Numeric,
            DataCategory::Alphabetic,
            true,
        ),
        (
            FigurativeValue::Zero,
            DataCategory::Numeric,
            DataCategory::Numeric,
            false,
        ),
        (
            FigurativeValue::Null,
            DataCategory::Alphanumeric,
            DataCategory::Alphanumeric,
            true,
        ),
        (
            FigurativeValue::HighValue,
            DataCategory::Alphanumeric,
            DataCategory::Alphanumeric,
            false,
        ),
    ] {
        let mut s = DiagnosticSink::new(WarningPolicy::new(), 10);
        validate_move_compatibility(
            CorrespondingMode::Standard,
            &fig_expr(fig),
            src_c,
            dst_c,
            span(),
            &mut s,
        );
        assert_eq!(s.has_errors(), expect_err, "case {src_c:?} -> {dst_c:?}");
    }
}

/// Tests elementary `validate_move_compatibility` and `validate_arith_category` policies.
#[test]
fn move_elementary_and_arith_category_matrix() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let sym_num = interner.intern("N1")?;
    let non_fig = Expr::Data(make_dref(sym_num, Vec::new(), None, DataRefModifier::Plain));

    for (src_c, dst_c, expect_err) in [
        (DataCategory::Numeric, DataCategory::Alphabetic, true),
        (DataCategory::NumericEdited, DataCategory::Alphabetic, true),
        (DataCategory::Alphabetic, DataCategory::Numeric, true),
        (DataCategory::Alphabetic, DataCategory::NumericEdited, true),
        (DataCategory::Numeric, DataCategory::NumericEdited, false),
    ] {
        let mut s = DiagnosticSink::new(WarningPolicy::new(), 10);
        validate_move_compatibility(
            CorrespondingMode::Standard,
            &non_fig,
            src_c,
            dst_c,
            span(),
            &mut s,
        );
        assert_eq!(s.has_errors(), expect_err, "case {src_c:?} -> {dst_c:?}");
    }

    for (cat, pol, exp_err) in [
        (
            DataCategory::Numeric,
            EditedTargetPolicy::StrictNumeric,
            false,
        ),
        (
            DataCategory::NumericEdited,
            EditedTargetPolicy::StrictNumeric,
            true,
        ),
        (
            DataCategory::NumericEdited,
            EditedTargetPolicy::AllowNumericEdited,
            false,
        ),
        (
            DataCategory::Alphanumeric,
            EditedTargetPolicy::AllowNumericEdited,
            true,
        ),
    ] {
        let mut s = DiagnosticSink::new(WarningPolicy::new(), 10);
        validate_arith_category(cat, pol, span(), &mut s);
        assert_eq!(s.has_errors(), exp_err);
    }

    Ok(())
}

/// Tests `validate_set_action` across `ConditionState`, `StepIndex`, `ToValue`, and `SwitchState`.
#[test]
fn set_action_validation_branches() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let sym_num = interner.intern("N1")?;
    let sym_alp = interner.intern("A1")?;
    let sym_c88 = interner.intern("C88")?;
    let sym_sw = interner.intern("SW1")?;
    let sym_oob = interner.intern("OOB")?;
    let sym_undef = interner.intern("NOPE")?;

    let non_fig = Expr::Data(make_dref(sym_num, Vec::new(), None, DataRefModifier::Plain));
    let fig_expr = |f: FigurativeValue| Expr::Literal {
        atom: ValueAtom::Figurative(f),
        span: span(),
    };

    let mut symtab = SymbolTable::default();
    let sc = symtab.create_scope(None);
    for (sym, target) in [
        (sym_num, ResolvedSymbol::DataItem(DataItemId::new(0))),
        (sym_alp, ResolvedSymbol::DataItem(DataItemId::new(1))),
        (sym_oob, ResolvedSymbol::DataItem(DataItemId::new(99))),
        (sym_c88, ResolvedSymbol::Condition88(0)),
        (
            sym_sw,
            ResolvedSymbol::SwitchCondition {
                switch_index: 0,
                expected: HirTruthValue::True,
            },
        ),
    ] {
        symtab.insert_symbol(
            sc,
            sym,
            SymbolCandidate {
                target,
                qualifiers: Vec::new(),
                visibility: GlobalVisibility::Local,
            },
        );
    }
    let items = vec![
        make_item(0, HirType::Index, Vec::new()),
        make_item(
            1,
            HirType::Alphanumeric {
                byte_len: 2,
                justified: JustifiedSpec::Left,
            },
            Vec::new(),
        ),
    ];
    let mut set_sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    for action in [
        SetAction::ConditionState {
            targets: vec![
                make_dref(sym_c88, Vec::new(), None, DataRefModifier::Plain),
                make_dref(sym_sw, Vec::new(), None, DataRefModifier::Plain),
                make_dref(sym_num, Vec::new(), None, DataRefModifier::Plain),
                make_dref(sym_undef, Vec::new(), None, DataRefModifier::Plain),
            ],
            state: TruthValue::True,
        },
        SetAction::StepIndex {
            targets: vec![
                make_dref(sym_num, Vec::new(), None, DataRefModifier::Plain),
                make_dref(sym_alp, Vec::new(), None, DataRefModifier::Plain),
                make_dref(sym_oob, Vec::new(), None, DataRefModifier::Plain),
                make_dref(sym_c88, Vec::new(), None, DataRefModifier::Plain),
                make_dref(sym_undef, Vec::new(), None, DataRefModifier::Plain),
            ],
            direction: StepDirection::Up,
            amount: fig_expr(FigurativeValue::Zero),
        },
        SetAction::ToValue {
            targets: vec![
                make_dref(sym_undef, Vec::new(), None, DataRefModifier::AddressOf),
                make_dref(sym_num, Vec::new(), None, DataRefModifier::Plain),
            ],
            value: non_fig,
        },
        SetAction::ToValue {
            targets: Vec::new(),
            value: fig_expr(FigurativeValue::Null),
        },
        SetAction::SwitchState {
            switch_name: sym_sw,
            state: TruthValue::True,
        },
    ] {
        validate_set_action(&action, sc, &items, &symtab, &interner, &mut set_sink);
    }

    Ok(())
}

/// Tests `intrinsic_arity_bounds` and `validate_intrinsic_arity` across every arity class and unknown names.
#[test]
fn intrinsic_function_arity_catalog() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let sample_bounds = [
        ("PI", Some((0, 0))),
        ("ABS", Some((1, 1))),
        ("RANDOM", Some((0, 1))),
        ("MOD", Some((2, 2))),
        ("TRIM", Some((1, 2))),
        ("YEAR-TO-YYYY", Some((1, 3))),
        ("CONVERT", Some((3, 3))),
        ("LOCALE-COMPARE", Some((2, 3))),
        ("FORMATTED-TIME", Some((2, 4))),
        ("FORMATTED-DATETIME", Some((4, 5))),
        ("FIND-STRING", Some((2, 5))),
        ("PRESENT-VALUE", Some((2, usize::MAX))),
        ("MAX", Some((1, usize::MAX))),
        ("SUBSTITUTE", Some((3, usize::MAX))),
        ("NO-SUCH-FUNC", None),
    ];
    for (name, expected) in sample_bounds {
        assert_eq!(intrinsic_arity_bounds(name), expected, "intrinsic `{name}`");
    }

    let sym_trim = interner.intern("TRIM")?;
    let sym_unknown = interner.intern("NOT-AN-INTRINSIC")?;

    let mut s_ok = DiagnosticSink::new(WarningPolicy::new(), 10);
    validate_intrinsic_arity(sym_trim, 1, span(), &interner, &mut s_ok);
    validate_intrinsic_arity(sym_trim, 2, span(), &interner, &mut s_ok);
    assert!(!s_ok.has_errors());

    for (sym, cnt) in [
        (sym_trim, 0),
        (sym_trim, 3),
        (sym_unknown, 1),
        (SymbolId::new(70_000), 0),
    ] {
        let mut s_err = DiagnosticSink::new(WarningPolicy::new(), 10);
        validate_intrinsic_arity(sym, cnt, span(), &interner, &mut s_err);
        assert!(s_err.has_errors());
    }

    Ok(())
}

/// Tests `FILE-CONTROL` and `FD` validation diagnostics and clean file connector resolution end-to-end.
#[test]
fn end_to_end_file_control_and_fd_diagnostics() -> Result<()> {
    let valid_files = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. FILE-OK.
ENVIRONMENT DIVISION.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-IDX ASSIGN TO "idx.dat"
        ORGANIZATION IS INDEXED
        ACCESS MODE IS DYNAMIC
        RECORD KEY IS IDX-KEY
        FILE STATUS IS WS-STAT.
    SELECT F-REL ASSIGN TO "rel.dat"
        ORGANIZATION IS RELATIVE
        ACCESS MODE IS RANDOM
        RELATIVE KEY IS WS-REL-KEY.
DATA DIVISION.
FILE SECTION.
FD F-IDX.
01 IDX-REC.
   05 IDX-KEY PIC X(4).
FD F-REL.
01 REL-REC PIC X(10).
WORKING-STORAGE SECTION.
01 WS-STAT PIC XX.
01 WS-REL-KEY PIC 9(4).
PROCEDURE DIVISION.
    STOP RUN.
"#;
    let (hir_ok, sink_ok) = compile_cobol(valid_files, Dialect::Cobol2023)?;
    assert!(
        !sink_ok.has_errors(),
        "unexpected errors: {:?}",
        sink_ok.diagnostics()
    );
    assert_eq!(hir_ok.map(|h| h.0.programs.len()), Some(1));

    let invalid_files = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. FILE-ERR.
ENVIRONMENT DIVISION.
INPUT-OUTPUT SECTION.
FILE-CONTROL.
    SELECT F-DUP ASSIGN TO "a.dat".
    SELECT F-DUP
        ORGANIZATION IS SEQUENTIAL
        ORGANIZATION IS INDEXED
        FILE STATUS IS WS-BAD-STAT.
    SELECT F-REL-BAD ASSIGN TO "r.dat"
        ORGANIZATION IS RELATIVE
        ACCESS MODE IS DYNAMIC.
    SELECT F-BAD-KEY ASSIGN TO "k.dat"
        ORGANIZATION IS INDEXED
        RECORD KEY IS UNDEFINED-KEY.
DATA DIVISION.
FILE SECTION.
FD F-ORPHAN.
01 ORPHAN-REC PIC X(4).
WORKING-STORAGE SECTION.
01 WS-BAD-STAT PIC X(5).
PROCEDURE DIVISION.
    STOP RUN.
"#;
    let (hir_err, sink_err) = compile_cobol(invalid_files, Dialect::Cobol2023)?;
    assert!(hir_err.is_none());
    assert!(sink_err.error_count() >= 5);

    Ok(())
}

/// Tests `check_and_resolve_files` when `FILE STATUS` resolves to an out-of-range `DataItemId`.
#[test]
fn check_and_resolve_files_out_of_range_status_id() -> Result<()> {
    let mut interner = SymbolInterner::new();
    let f_sym = interner.intern("F-SYN")?;
    let st_sym = interner.intern("WS-OUT-OF-RANGE")?;
    let cond_sym = interner.intern("COND-88-NOT-ITEM")?;
    let mut symtab = SymbolTable::default();
    let sc = symtab.create_scope(None);
    symtab.insert_symbol(
        sc,
        st_sym,
        SymbolCandidate {
            target: ResolvedSymbol::DataItem(DataItemId::new(999)),
            qualifiers: Vec::new(),
            visibility: GlobalVisibility::Local,
        },
    );
    symtab.insert_symbol(
        sc,
        cond_sym,
        SymbolCandidate {
            target: ResolvedSymbol::Condition88(0),
            qualifiers: Vec::new(),
            visibility: GlobalVisibility::Local,
        },
    );
    let mut env = coblang_syntax::ast::EnvironmentDivision::default();
    env.input_output
        .file_control
        .push(coblang_syntax::ast::FileControlEntry {
            file_name: f_sym,
            optionality: coblang_syntax::ast::SelectOptionality::Required,
            assign_targets: vec![coblang_syntax::ast::AssignTarget::Literal(f_sym)],
            organization: coblang_syntax::ast::FileOrganization::Sequential,
            access_mode: coblang_syntax::ast::AccessMode::Sequential,
            record_key: None,
            alternate_keys: Vec::new(),
            relative_key: Some(vec![cond_sym]),
            file_status: Some(vec![st_sym]),
            secondary_status: None,
            lock_mode: None,
            sharing_mode: None,
            padding_char: None,
            collating_sequence: None,
            organization_clause_count: 1,
            span: span(),
        });
    let mut layout = coblang_sema::layout::SolvedDataLayout::default();
    layout.fd_bindings = vec![(f_sym, FileConnectorId::new(0), Vec::new(), 0, span())];
    let mut syn_sink = DiagnosticSink::new(WarningPolicy::new(), 20);
    let conns = coblang_sema::typecheck::check_and_resolve_files(
        &env,
        &coblang_syntax::ast::DataDivision::default(),
        &layout,
        sc,
        &symtab,
        &interner,
        &mut syn_sink,
    );
    assert_eq!(conns.len(), 1);
    assert!(!syn_sink.has_errors());

    Ok(())
}

/// Tests end-to-end nested programs (`GLOBAL`, `COMMON`), `SPECIAL-NAMES`, `CORRESPONDING`, `SET`, and intrinsics.
#[test]
fn end_to_end_nested_programs_and_procedure_typechecking() -> Result<()> {
    let src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. OUTER-PROG.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
SPECIAL-NAMES.
    SWITCH-1 ON STATUS IS SW-ON OFF STATUS IS SW-OFF.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 SHARED-GRP IS GLOBAL.
   05 SHARED-NUM PIC 9(4) VALUE 10.
01 DST-GRP.
   05 SHARED-NUM PIC Z(3)9.
01 TBL-WRAP.
   05 TBL-ITEM PIC X(6) OCCURS 3 TIMES INDEXED BY IDX-1.
01 FLAG-ITEM PIC X VALUE "N".
   88 FLAG-YES VALUE "Y".
PROCEDURE DIVISION.
SEC-MAIN SECTION.
PARA-1.
    MOVE CORRESPONDING SHARED-GRP TO DST-GRP
    SET FLAG-YES TO TRUE
    SET IDX-1 TO 1
    SET IDX-1 UP BY 1
    MOVE FUNCTION UPPER-CASE(TBL-ITEM(2)(1:3)) TO TBL-ITEM(1)
    CALL "HELPER-PROG"
    STOP RUN.
IDENTIFICATION DIVISION.
PROGRAM-ID. HELPER-PROG IS COMMON.
PROCEDURE DIVISION.
    ADD 1 TO SHARED-NUM
    EXIT PROGRAM.
END PROGRAM HELPER-PROG.
END PROGRAM OUTER-PROG.
"#;
    let (hir, sink) = compile_cobol(src, Dialect::Cobol2023)?;
    assert!(
        !sink.has_errors(),
        "unexpected errors: {:?}",
        sink.diagnostics()
    );
    assert_eq!(
        hir.and_then(|h| h.0.programs.first().map(|p| p.nested.len())),
        Some(1)
    );
    Ok(())
}
