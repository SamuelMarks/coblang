//! Exhaustive unit, negative-verifier, and HIR-to-LLVM-MLIR coverage suite for `coblang-mlir`.

use coblang_common::{
    ByteLength, ByteOffset, CoblangError, Dialect, ExpansionId, FileId, OptimizationLevel, Result,
    SourceFormat, SourceSpan,
};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap, SymbolInterner, WarningPolicy};
use coblang_hir::ResolvedHirModule;
use coblang_mlir::ops::{
    AcceptSourceKind, ArithKind, BoundsCheckMode, CallDispatchKind, CharByteWidth,
    ClassConditionKind, CobolAcceptOp, CobolAlterOp, CobolArithOp, CobolCallOp, CobolCancelOp,
    CobolClassCheckOp, CobolCmpOp, CobolDisplayOp, CobolDivRemOp, CobolFieldViewOp, CobolFileOp,
    CobolGetReturnCodeOp, CobolGobackOp, CobolInitOp, CobolInspectOp, CobolMoveOp, CobolOpKind,
    CobolPerformRangeOp, CobolProgramOp, CobolRefModOp, CobolReportOp, CobolScreenOp,
    CobolSetReturnCodeOp, CobolSignCheckOp, CobolSortMergeOp, CobolStopRunOp, CobolStorageOp,
    CobolStringOp, CobolSubscriptOp, CobolTrimOp, CobolUnstringOp, CobolXmlJsonOp, CollatingSeqId,
    EntryConventionMode, ExceptionBranches, FieldAttrDesc, FileVerbKind, GobackMode, InitPolicy,
    InitialStateMode, MoveEncodingKind, RecursivityMode, RelOp, RoundingMode, SignConditionKind,
    SizeErrorBranch, StorageKind, decode_hex_bytes, encode_hex_bytes, finish_op_build,
    is_cobol_dialect_name, llvm_ptr_type,
};
use coblang_mlir::{
    CobolMlirContext, MainEntryMode, lower_cobol_to_llvm_mlir, lower_hir_to_cobol_mlir,
    verify_cobol_module,
};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, preprocess_source_file,
};
use coblang_rt_core::{LineAdvancing, OutputDevice, TrimDirection};
use coblang_sema::lower_compilation_group;
use coblang_syntax::parse_preprocessed_unit;
use melior::dialect::{arith, llvm};
use melior::ir::attribute::{IntegerAttribute, StringAttribute};
use melior::ir::block::BlockLike;
use melior::ir::operation::OperationBuilder;
use melior::ir::r#type::IntegerType;
use melior::ir::{Block, Identifier, Module, Region, RegionLike};

/// Compiles inline COBOL source text into `(SourceMap, SymbolInterner, ResolvedHirModule)`.
///
/// # Arguments
///
/// * `src` - COBOL source text.
/// * `dialect` - Base dialect preset.
fn compile_to_hir(
    src: &str,
    dialect: Dialect,
) -> Result<(SourceMap, SymbolInterner, ResolvedHirModule)> {
    let dcfg = DialectConfig::for_dialect(dialect);
    let resolver = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dcfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &resolver,
    };
    let mut sm = SourceMap::new();
    let mut interner = SymbolInterner::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let unit = preprocess_source_file(
        "test.cob",
        std::path::Path::new("."),
        src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let group = parse_preprocessed_unit(&unit, &dcfg, &mut interner, &mut sink)?;
    let hir = lower_compilation_group(&group, &dcfg, &mut interner, &mut sink)?;
    assert_eq!(sink.diagnostics().len(), 0);
    Ok((sm, interner, hir))
}

/// Verifies `CobolMlirContext::default`, `span_to_location` success/error, and hex codec helpers.
#[test]
fn context_locations_and_hex_helpers() -> Result<()> {
    let ctx = CobolMlirContext::default();
    let mut sm = SourceMap::new();
    let fid = sm.add_file(
        String::from("demo.cob"),
        String::from("IDENTIFICATION DIVISION.\u{000a}"),
    )?;
    let valid_span = SourceSpan::new(fid, ByteOffset::ZERO, ByteLength::new(5), ExpansionId::ROOT);
    let _loc = ctx.span_to_location(&sm, valid_span)?;

    let bad_span = SourceSpan::new(
        FileId::new(999),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    assert!(ctx.span_to_location(&sm, bad_span).is_err());

    assert_eq!(encode_hex_bytes(&[0x00, 0xAB, 0xFF]), "00ABFF");
    assert_eq!(decode_hex_bytes("00ABFF")?, vec![0x00, 0xAB, 0xFF]);
    assert!(matches!(
        decode_hex_bytes("ABC"),
        Err(CoblangError::MlirVerification { .. })
    ));
    assert!(matches!(
        decode_hex_bytes("GG"),
        Err(CoblangError::MlirVerification { .. })
    ));

    let infer_fail = finish_op_build(
        OperationBuilder::new("arith.addi", ctx.unknown_location()).enable_result_type_inference(),
        "arith.addi",
    );
    assert!(matches!(
        infer_fail,
        Err(CoblangError::MlirVerification { .. })
    ));
    Ok(())
}

/// Exercises all enum string mappings and all 31 `Cobol*Op` builders inside a verified `cobol.program`.
#[test]
fn all_thirty_one_cobol_op_builders_and_verifier_pass() -> Result<()> {
    let all_kinds = [
        CobolOpKind::Program,
        CobolOpKind::Storage,
        CobolOpKind::FieldView,
        CobolOpKind::Subscript,
        CobolOpKind::RefMod,
        CobolOpKind::Move,
        CobolOpKind::Init,
        CobolOpKind::Arith,
        CobolOpKind::DivRem,
        CobolOpKind::Cmp,
        CobolOpKind::ClassCheck,
        CobolOpKind::SignCheck,
        CobolOpKind::PerformRange,
        CobolOpKind::Alter,
        CobolOpKind::Call,
        CobolOpKind::Cancel,
        CobolOpKind::Display,
        CobolOpKind::Accept,
        CobolOpKind::StringVerb,
        CobolOpKind::UnstringVerb,
        CobolOpKind::Inspect,
        CobolOpKind::FileOp,
        CobolOpKind::SortMerge,
        CobolOpKind::Report,
        CobolOpKind::Screen,
        CobolOpKind::XmlJson,
        CobolOpKind::SetReturnCode,
        CobolOpKind::GetReturnCode,
        CobolOpKind::StopRun,
        CobolOpKind::Goback,
        CobolOpKind::Trim,
    ];
    for kind in all_kinds {
        assert!(is_cobol_dialect_name(kind.op_name()));
        assert_eq!(CobolOpKind::from_op_name(kind.op_name()), Some(kind));
    }
    assert_eq!(CobolOpKind::from_op_name("cobol.bogus"), None);
    assert!(!is_cobol_dialect_name("arith.addi"));

    check_enum_string_tags();
    build_and_verify_kitchen_sink_cobol_module()
}

/// Verifies `as_str` methods on every policy enum in `coblang_mlir::ops`.
fn check_enum_string_tags() {
    assert_eq!(InitialStateMode::Initial.as_str(), "initial");
    assert_eq!(RecursivityMode::Recursive.as_str(), "recursive");
    assert_eq!(EntryConventionMode::C.as_str(), "c");
    assert_eq!(EntryConventionMode::Static.as_str(), "static");
    assert_eq!(EntryConventionMode::Stdcall.as_str(), "stdcall");
    assert_eq!(StorageKind::Local.as_str(), "local");
    assert_eq!(StorageKind::External.as_str(), "external");
    assert_eq!(BoundsCheckMode::Unchecked.as_str(), "unchecked");
    assert_eq!(BoundsCheckMode::Checked.as_str(), "checked");
    assert_eq!(CharByteWidth::SingleByte.as_str(), "1");
    assert_eq!(CharByteWidth::Utf16TwoByte.as_str(), "2");
    assert_eq!(CharByteWidth::Utf8Variable.as_str(), "utf8");
    assert_eq!(InitPolicy::DefinedValue.as_str(), "defined_value");
    assert_eq!(ArithKind::Pow.as_str(), "pow");
    assert_eq!(
        RoundingMode::NearestAwayFromZero.as_str(),
        "nearest_away_from_zero"
    );
    assert_eq!(SizeErrorBranch::CheckOverflow.as_str(), "check");
    for (c, s) in [
        (ClassConditionKind::Numeric, "numeric"),
        (ClassConditionKind::Alphabetic, "alphabetic"),
        (ClassConditionKind::AlphabeticLower, "alphabetic_lower"),
        (ClassConditionKind::AlphabeticUpper, "alphabetic_upper"),
    ] {
        assert_eq!(c.as_str(), s);
    }
    for (sg, s) in [
        (SignConditionKind::Positive, "positive"),
        (SignConditionKind::Negative, "negative"),
        (SignConditionKind::Zero, "zero"),
    ] {
        assert_eq!(sg.as_str(), s);
    }
    assert_eq!(CallDispatchKind::DynamicValue.as_str(), "dynamic_val");
    assert_eq!(ExceptionBranches::Handled.as_str(), "handled");
    for (ac, s) in [
        (AcceptSourceKind::Console, "console"),
        (AcceptSourceKind::Date, "date"),
        (AcceptSourceKind::Day, "day"),
        (AcceptSourceKind::DayOfWeek, "day_of_week"),
        (AcceptSourceKind::Time, "time"),
        (AcceptSourceKind::CommandLine, "command_line"),
        (AcceptSourceKind::Environment, "environment"),
    ] {
        assert_eq!(ac.as_str(), s);
    }
    for (fv, s) in [
        (FileVerbKind::Open, "open"),
        (FileVerbKind::Close, "close"),
        (FileVerbKind::Read, "read"),
        (FileVerbKind::Write, "write"),
        (FileVerbKind::Rewrite, "rewrite"),
        (FileVerbKind::Delete, "delete"),
        (FileVerbKind::Start, "start"),
    ] {
        assert_eq!(fv.as_str(), s);
    }
}

/// Builds a synthetic `cobol` MLIR module exercising every single `Cobol*Op` builder and runs `verify_cobol_module`.
fn build_and_verify_kitchen_sink_cobol_module() -> Result<()> {
    let ctx = CobolMlirContext::new();
    let loc = ctx.unknown_location();
    let module = Module::new(loc);

    let st = CobolStorageOp {
        sym_name: "__ws_test",
        storage_kind: StorageKind::Working,
        byte_size: 8,
        alignment: 4,
        init_bytes: &[0; 8],
    }
    .build(&ctx, loc)?;
    let _ = module.body().append_operation(st);

    let region = Region::new();
    let blk = region.append_block(Block::new(&[]));
    let i32_ty = IntegerType::new(ctx.raw(), 32).into();
    let i64_ty = IntegerType::new(ctx.raw(), 64).into();
    let c0 = blk
        .append_operation(arith::constant(
            ctx.raw(),
            IntegerAttribute::new(i32_ty, 0).into(),
            loc,
        ))
        .result(0)
        .map_err(|_| CoblangError::MlirVerification {
            stage: "test",
            ir_snippet: String::new(),
            message: String::new(),
        })?
        .into();
    let c1_64 = blk
        .append_operation(arith::constant(
            ctx.raw(),
            IntegerAttribute::new(i64_ty, 1).into(),
            loc,
        ))
        .result(0)
        .map_err(|_| CoblangError::MlirVerification {
            stage: "test",
            ir_snippet: String::new(),
            message: String::new(),
        })?
        .into();

    let fv = blk.append_operation(
        CobolFieldViewOp {
            storage_sym: "__ws_test",
            byte_offset: 2,
            byte_len: 4,
            attr: FieldAttrDesc {
                kind_tag: 0,
                digits: 4,
                scale: 0,
            },
        }
        .build(&ctx, loc)?,
    );
    let ptr = fv
        .result(0)
        .map_err(|_| CoblangError::MlirVerification {
            stage: "test",
            ir_snippet: String::new(),
            message: String::new(),
        })?
        .into();

    append_remaining_ops_part1(&ctx, &blk, ptr, c0, c1_64, loc)?;
    append_remaining_ops_part2(&ctx, &blk, ptr, c0, c1_64, loc)?;

    let prog = CobolProgramOp {
        sym_name: String::from("__coblang_prog_SINK"),
        raw_c_alias: String::from("sink"),
        rc_slot_sym: String::from("__ws_test"),
        initial_state: InitialStateMode::Retained,
        recursivity: RecursivityMode::NonRecursive,
        entry_convention: EntryConventionMode::Cobol,
        body: region,
    }
    .build(&ctx, loc)?;
    let _ = module.body().append_operation(prog);

    let verified = verify_cobol_module(module)?;
    assert!(verified.to_mlir_text().contains("cobol.program"));
    let lowered = lower_cobol_to_llvm_mlir(
        &ctx,
        &verified,
        OptimizationLevel::O1,
        MainEntryMode::EmitMain,
    )?;
    assert!(lowered.to_mlir_text().contains("llvm.func @main"));
    Ok(())
}

/// Appends subscript, refmod, move, init, arith, `div_rem`, cmp, `class_check`, and `sign_check` ops.
///
/// # Arguments
///
/// * `ctx` - MLIR context.
/// * `blk` - Target block.
/// * `ptr` - Pointer SSA value.
/// * `c0` - `i32` zero SSA value.
/// * `c1_64` - `i64` one SSA value.
/// * `loc` - Source location.
fn append_remaining_ops_part1<'c, 'b>(
    ctx: &'c CobolMlirContext,
    blk: &'b Block<'c>,
    ptr: melior::ir::Value<'c, 'b>,
    c0: melior::ir::Value<'c, 'b>,
    c1_64: melior::ir::Value<'c, 'b>,
    loc: melior::ir::Location<'c>,
) -> Result<()> {
    let _ = blk.append_operation(
        CobolSubscriptOp {
            base_ptr: ptr,
            indices: &[c1_64],
            strides: &[4],
            odo_limit: Some(c1_64),
            bounds_mode: BoundsCheckMode::Checked,
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolSubscriptOp {
            base_ptr: ptr,
            indices: &[c0],
            strides: &[4],
            odo_limit: None,
            bounds_mode: BoundsCheckMode::Unchecked,
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolRefModOp {
            view_ptr: ptr,
            base_len: c1_64,
            one_based_offset: c1_64,
            opt_char_len: Some(c1_64),
            char_width: CharByteWidth::SingleByte,
            bounds_mode: BoundsCheckMode::Checked,
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolRefModOp {
            view_ptr: ptr,
            base_len: c1_64,
            one_based_offset: c1_64,
            opt_char_len: None,
            char_width: CharByteWidth::SingleByte,
            bounds_mode: BoundsCheckMode::Unchecked,
        }
        .build(ctx, loc)?,
    );
    for (encoding, dst_byte_len) in [
        (MoveEncodingKind::Int32ToNativeEndian, 4),
        (MoveEncodingKind::Int32ToNativeEndian, 2),
        (MoveEncodingKind::Int32ToBigEndian, 4),
        (MoveEncodingKind::Int32ToBigEndian, 1),
    ] {
        let _ = blk.append_operation(
            CobolMoveOp {
                src: c0,
                dst_view: ptr,
                encoding,
                src_byte_len: 0,
                dst_byte_len,
            }
            .build(ctx, loc)?,
        );
    }
    let _ = blk.append_operation(
        CobolInitOp {
            dst_view: ptr,
            policy: InitPolicy::DefaultCategory,
            byte_len: 4,
            fill_byte: 0,
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolDivRemOp {
            dividend: c0,
            divisor: c0,
            quotient_dst: ptr,
            remainder_dst: ptr,
            rounding: RoundingMode::Truncate,
            size_error: SizeErrorBranch::Ignore,
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolClassCheckOp {
            class_kind: ClassConditionKind::Numeric,
            view: ptr,
            byte_len: 4,
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolSignCheckOp {
            sign_kind: SignConditionKind::Positive,
            view: c0,
        }
        .build(ctx, loc)?,
    );
    Ok(())
}

/// Appends perform, alter, cancel, accept, string/unstring/inspect, file, sort, report, screen, and xml/json ops.
///
/// # Arguments
///
/// * `ctx` - MLIR context.
/// * `blk` - Target block.
/// * `ptr` - Pointer SSA value.
/// * `c0` - `i32` zero SSA value.
/// * `c1_64` - `i64` one SSA value.
/// * `loc` - Source location.
fn append_remaining_ops_part2<'c, 'b>(
    ctx: &'c CobolMlirContext,
    blk: &'b Block<'c>,
    ptr: melior::ir::Value<'c, 'b>,
    c0: melior::ir::Value<'c, 'b>,
    c1_64: melior::ir::Value<'c, 'b>,
    loc: melior::ir::Location<'c>,
) -> Result<()> {
    let _ = blk.append_operation(
        CobolPerformRangeOp {
            start_proc: 0,
            end_proc: 1,
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolAlterOp {
            altered_para: 0,
            new_target: 1,
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(CobolCancelOp { target: ptr }.build(ctx, loc)?);
    let _ = blk.append_operation(
        CobolAcceptOp {
            dst: ptr,
            source: AcceptSourceKind::Date,
            exception_branches: ExceptionBranches::None,
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(CobolStringOp { operands: &[ptr] }.build(ctx, loc)?);
    let _ = blk.append_operation(CobolUnstringOp { operands: &[ptr] }.build(ctx, loc)?);
    let _ = blk.append_operation(CobolInspectOp { operands: &[ptr] }.build(ctx, loc)?);
    let _ = blk.append_operation(
        CobolFileOp {
            verb: FileVerbKind::Open,
            connector_id: 0,
            operands: &[ptr],
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolSortMergeOp {
            connector_id: 0,
            operands: &[ptr],
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolReportOp {
            report_id: 0,
            action: "initiate",
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolScreenOp {
            action: "display",
            operands: &[ptr],
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolXmlJsonOp {
            action: "json_parse",
            operands: &[ptr],
        }
        .build(ctx, loc)?,
    );
    append_remaining_ops_part3(ctx, blk, ptr, c0, c1_64, loc)
}

/// Appends trim, display, return-code, arith, cmp, call, goback, and `stop_run` ops.
///
/// # Arguments
///
/// * `ctx` - MLIR context.
/// * `blk` - Target block.
/// * `ptr` - Pointer SSA value.
/// * `c0` - `i32` zero SSA value.
/// * `c1_64` - `i64` one SSA value.
/// * `loc` - Source location.
fn append_remaining_ops_part3<'c, 'b>(
    ctx: &'c CobolMlirContext,
    blk: &'b Block<'c>,
    ptr: melior::ir::Value<'c, 'b>,
    c0: melior::ir::Value<'c, 'b>,
    c1_64: melior::ir::Value<'c, 'b>,
    loc: melior::ir::Location<'c>,
) -> Result<()> {
    let _ = blk.append_operation(
        CobolTrimOp {
            src_ptr: ptr,
            src_len: c1_64,
            direction: TrimDirection::Both,
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolDisplayOp {
            ptr_len_pairs: &[ptr, c1_64],
            item_kinds: &[0],
            item_scales: &[0],
            device: OutputDevice::Stdout,
            advancing: LineAdvancing::AdvanceLine,
        }
        .build(ctx, loc)?,
    );
    assert_eq!(MoveEncodingKind::RawBytesCopyPad.as_str(), "bytes_copy_pad");
    assert_eq!(
        coblang_mlir::ops::RtDispatchTarget::CmpAlpha.symbol_name(),
        "coblang_rt_cmp_alpha"
    );
    let _ = blk.append_operation(CobolSetReturnCodeOp { code: c0 }.build(ctx, loc)?);
    let _ = blk.append_operation(CobolGetReturnCodeOp.build(ctx, loc)?);
    for kind in [
        ArithKind::Add,
        ArithKind::Sub,
        ArithKind::Mul,
        ArithKind::Div,
        ArithKind::Pow,
    ] {
        let _ = blk.append_operation(
            CobolArithOp {
                kind,
                lhs: c0,
                rhs: c0,
                rounding: RoundingMode::Truncate,
                size_error: SizeErrorBranch::Ignore,
            }
            .build(ctx, loc)?,
        );
    }
    for rel_op in [
        RelOp::Eq,
        RelOp::Ne,
        RelOp::Lt,
        RelOp::Le,
        RelOp::Gt,
        RelOp::Ge,
    ] {
        let _ = blk.append_operation(
            CobolCmpOp {
                rel_op,
                lhs: c0,
                rhs: c0,
                collating_seq: CollatingSeqId::default(),
            }
            .build(ctx, loc)?,
        );
    }
    let _ = blk.append_operation(
        CobolCallOp {
            dispatch: CallDispatchKind::DynamicLiteral,
            callee: "",
            args: &[],
            exception_branches: ExceptionBranches::None,
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolGobackOp {
            mode: GobackMode::Goback,
        }
        .build(ctx, loc)?,
    );
    let _ = blk.append_operation(CobolStopRunOp { status: c0 }.build(ctx, loc)?);
    Ok(())
}

/// Exercises every negative rejection branch in `verifier.rs`.
#[test]
fn verifier_rejects_malformed_cobol_and_builtin_ops() -> Result<()> {
    let ctx = CobolMlirContext::new();
    let loc = ctx.unknown_location();
    let ptr_ty = llvm_ptr_type(&ctx);
    let i64_ty = IntegerType::new(ctx.raw(), 64).into();

    // 1. Builtin MLIR verifier failure (`arith.addi` with 0 operands).
    let m_builtin = Module::new(loc);
    let bad_builtin = finish_op_build(OperationBuilder::new("arith.addi", loc), "arith.addi")?;
    let _ = m_builtin.body().append_operation(bad_builtin);
    assert!(verify_cobol_module(m_builtin).is_err());

    // 2. Unknown `cobol.*` op name.
    let m_unk = Module::new(loc);
    let unk_op = finish_op_build(OperationBuilder::new("cobol.no_such_op", loc), "unk")?;
    let _ = m_unk.body().append_operation(unk_op);
    assert!(verify_cobol_module(m_unk).is_err());

    // 3. Missing attribute on `cobol.goback`.
    let m_attr = Module::new(loc);
    let no_attr = finish_op_build(OperationBuilder::new("cobol.goback", loc), "goback")?;
    let _ = m_attr.body().append_operation(no_attr);
    assert!(verify_cobol_module(m_attr).is_err());

    // 4. Wrong operand count + wrong result count on `cobol.get_return_code`.
    let m_res = Module::new(loc);
    let bad_res = finish_op_build(
        OperationBuilder::new("cobol.get_return_code", loc),
        "get_rc",
    )?;
    let _ = m_res.body().append_operation(bad_res);
    assert!(verify_cobol_module(m_res).is_err());

    let m_ops = Module::new(loc);
    let c0_op = arith::constant(ctx.raw(), IntegerAttribute::new(i64_ty, 0).into(), loc);
    let c0_val = m_ops
        .body()
        .append_operation(c0_op)
        .result(0)
        .map_err(|_| CoblangError::MlirVerification {
            stage: "t",
            ir_snippet: String::new(),
            message: String::new(),
        })?
        .into();
    let bad_ops = finish_op_build(
        OperationBuilder::new("cobol.get_return_code", loc)
            .add_operands(&[c0_val])
            .add_results(&[i64_ty]),
        "get_rc",
    )?;
    let _ = m_ops.body().append_operation(bad_ops);
    assert!(verify_cobol_module(m_ops).is_err());

    verify_special_op_arity_errors(&ctx, ptr_ty, loc)
}

/// Exercises negative arity/region/hex checks on `subscript`, `refmod`, `display`, `program`, and `storage`.
///
/// # Arguments
///
/// * `ctx` - MLIR context.
/// * `ptr_ty` - `!llvm.ptr` type.
/// * `loc` - Unknown location.
fn verify_special_op_arity_errors<'c>(
    ctx: &'c CobolMlirContext,
    ptr_ty: melior::ir::Type<'c>,
    loc: melior::ir::Location<'c>,
) -> Result<()> {
    // `cobol.subscript` with 0 operands (< 2).
    let m_sub = Module::new(loc);
    let sub_bad = finish_op_build(
        OperationBuilder::new("cobol.subscript", loc).add_results(&[ptr_ty]),
        "sub",
    )?;
    let _ = m_sub.body().append_operation(sub_bad);
    assert!(verify_cobol_module(m_sub).is_err());

    // `cobol.refmod` with 0 operands (< 3) and 5 operands (> 4).
    let m_ref0 = Module::new(loc);
    let ref_bad0 = finish_op_build(OperationBuilder::new("cobol.refmod", loc), "refmod")?;
    let _ = m_ref0.body().append_operation(ref_bad0);
    assert!(verify_cobol_module(m_ref0).is_err());

    let m_ref5 = Module::new(loc);
    let z = m_ref5.body().append_operation(llvm::zero(ptr_ty, loc));
    let zv: melior::ir::Value<'_, '_> = z
        .result(0)
        .map_err(|_| CoblangError::MlirVerification {
            stage: "t",
            ir_snippet: String::new(),
            message: String::new(),
        })?
        .into();
    let ref_bad5 = finish_op_build(
        OperationBuilder::new("cobol.refmod", loc).add_operands(&[zv, zv, zv, zv, zv]),
        "refmod",
    )?;
    let _ = m_ref5.body().append_operation(ref_bad5);
    assert!(verify_cobol_module(m_ref5).is_err());

    // `cobol.display` with odd (1) operand count.
    let m_disp = Module::new(loc);
    let z2 = m_disp.body().append_operation(llvm::zero(ptr_ty, loc));
    let zv2: melior::ir::Value<'_, '_> = z2
        .result(0)
        .map_err(|_| CoblangError::MlirVerification {
            stage: "t",
            ir_snippet: String::new(),
            message: String::new(),
        })?
        .into();
    let disp_bad = finish_op_build(
        OperationBuilder::new("cobol.display", loc).add_operands(&[zv2]),
        "disp",
    )?;
    let _ = m_disp.body().append_operation(disp_bad);
    assert!(verify_cobol_module(m_disp).is_err());

    // `cobol.rt_dispatch` with 2 results (`result_count() > 1`).
    let m_rtd = Module::new(loc);
    let rtd_bad = finish_op_build(
        OperationBuilder::new("cobol.rt_dispatch", loc)
            .add_attributes(&[(
                Identifier::new(ctx.raw(), "target"),
                StringAttribute::new(ctx.raw(), "coblang_rt_disp_begin").into(),
            )])
            .add_results(&[ptr_ty, ptr_ty]),
        "rt_dispatch",
    )?;
    let _ = m_rtd.body().append_operation(rtd_bad);
    assert!(verify_cobol_module(m_rtd).is_err());

    verify_program_and_storage_errors(ctx, loc)
}

/// Exercises `cobol.program` missing/empty region/block and `cobol.storage` invalid `init_hex` errors.
///
/// # Arguments
///
/// * `ctx` - MLIR context.
/// * `loc` - Unknown location.
fn verify_program_and_storage_errors<'c>(
    ctx: &'c CobolMlirContext,
    loc: melior::ir::Location<'c>,
) -> Result<()> {
    let prog_attrs = || {
        [
            (
                Identifier::new(ctx.raw(), "sym_name"),
                StringAttribute::new(ctx.raw(), "p").into(),
            ),
            (
                Identifier::new(ctx.raw(), "raw_c_alias"),
                StringAttribute::new(ctx.raw(), "").into(),
            ),
            (
                Identifier::new(ctx.raw(), "rc_slot_sym"),
                StringAttribute::new(ctx.raw(), "rc").into(),
            ),
            (
                Identifier::new(ctx.raw(), "initial_state"),
                StringAttribute::new(ctx.raw(), "retained").into(),
            ),
            (
                Identifier::new(ctx.raw(), "recursivity"),
                StringAttribute::new(ctx.raw(), "non_recursive").into(),
            ),
            (
                Identifier::new(ctx.raw(), "entry_convention"),
                StringAttribute::new(ctx.raw(), "cobol").into(),
            ),
        ]
    };

    // Program with 0 regions.
    let m_p0 = Module::new(loc);
    let p0 = finish_op_build(
        OperationBuilder::new("cobol.program", loc).add_attributes(&prog_attrs()),
        "p0",
    )?;
    let _ = m_p0.body().append_operation(p0);
    assert!(verify_cobol_module(m_p0).is_err());

    // Program with 0 blocks in region.
    let m_p1 = Module::new(loc);
    let p1 = finish_op_build(
        OperationBuilder::new("cobol.program", loc)
            .add_attributes(&prog_attrs())
            .add_regions([Region::new()]),
        "p1",
    )?;
    let _ = m_p1.body().append_operation(p1);
    assert!(verify_cobol_module(m_p1).is_err());

    // Program with empty entry block.
    let m_p2 = Module::new(loc);
    let empty_reg = Region::new();
    let _ = empty_reg.append_block(Block::new(&[]));
    let p2 = finish_op_build(
        OperationBuilder::new("cobol.program", loc)
            .add_attributes(&prog_attrs())
            .add_regions([empty_reg]),
        "p2",
    )?;
    let _ = m_p2.body().append_operation(p2);
    assert!(verify_cobol_module(m_p2).is_err());

    // Storage with empty `init_hex` (`""`) and non-string `init_hex`.
    let m_st_empty = Module::new(loc);
    let st_empty = CobolStorageOp {
        sym_name: "s",
        storage_kind: StorageKind::Working,
        byte_size: 0,
        alignment: 1,
        init_bytes: &[],
    }
    .build(ctx, loc)?;
    let _ = m_st_empty.body().append_operation(st_empty);
    assert!(verify_cobol_module(m_st_empty).is_err());

    let i64_ty = IntegerType::new(ctx.raw(), 64).into();
    let m_st_int = Module::new(loc);
    let st_int = finish_op_build(
        OperationBuilder::new("cobol.storage", loc).add_attributes(&[
            (
                Identifier::new(ctx.raw(), "sym_name"),
                StringAttribute::new(ctx.raw(), "s").into(),
            ),
            (
                Identifier::new(ctx.raw(), "storage_kind"),
                StringAttribute::new(ctx.raw(), "working").into(),
            ),
            (
                Identifier::new(ctx.raw(), "byte_size"),
                IntegerAttribute::new(i64_ty, 1).into(),
            ),
            (
                Identifier::new(ctx.raw(), "alignment"),
                IntegerAttribute::new(i64_ty, 1).into(),
            ),
            (
                Identifier::new(ctx.raw(), "init_hex"),
                IntegerAttribute::new(i64_ty, 0).into(),
            ),
        ]),
        "st_int",
    )?;
    let _ = m_st_int.body().append_operation(st_int);
    assert!(verify_cobol_module(m_st_int).is_err());
    Ok(())
}

/// Exercises full HIR -> `cobol` MLIR -> `llvm` MLIR lowering across types, expressions, conditions, and opt levels.
#[test]
fn hir_to_llvm_mlir_comprehensive_features() -> Result<()> {
    let src = r#"
IDENTIFICATION DIVISION.
PROGRAM-ID. exit.
DATA DIVISION.
WORKING-STORAGE SECTION.
78 C-TEN VALUE 10.
01 WS-STR PIC X(8) VALUE "  HI  ".
01 WS-ZERO PIC X(4) VALUE ZEROS.
01 WS-SP PIC X(4) VALUE SPACES.
01 WS-HI PIC X(2) VALUE HIGH-VALUES.
01 WS-LO PIC X(2) VALUE LOW-VALUES.
01 WS-QT PIC X(2) VALUE QUOTES.
01 WS-AL PIC X(4) VALUE ALL "AB".
01 WS-B1 PIC S9(2) COMP VALUE -3.
01 WS-BZ PIC S9(2) COMP VALUE ZERO.
01 WS-U1 PIC 9(2) COMP-5 VALUE 4.
01 WS-B2 PIC S9(4) COMP VALUE -12.
01 WS-U2 PIC 9(4) COMP VALUE 12.
01 WS-B4 PIC S9(8) COMP-5 VALUE 100.
LOCAL-STORAGE SECTION.
01 LS-TMP PIC X(4) VALUE "".
PROCEDURE DIVISION.
P-ENTRY.
    INITIALIZE RETURN-CODE WS-STR WS-B2.
    ADD 1 TO WS-B2.
    MOVE "ABCD" TO WS-STR.
    MOVE WS-STR TO LS-TMP.
    MOVE "" TO LS-TMP.
    MOVE -5 TO WS-B1.
    MOVE RETURN-CODE TO WS-B1.
    MOVE WS-B1 TO WS-B2.
    MOVE WS-U1 TO WS-U2.
    MOVE WS-B2 TO WS-B4.
    COMPUTE WS-B4 = (WS-B4 + 10 - 2) * 3 / 2.
    COMPUTE RETURN-CODE = -WS-B1.
    IF TRUE
        DISPLAY RETURN-CODE
    ELSE
        GOBACK
    END-IF.
    IF FALSE
        GOBACK RETURNING 3
    ELSE
        DISPLAY +5
        DISPLAY 0.005
    END-IF.
    IF RETURN-CODE = 0 OR (WS-B4 <> 0 AND NOT WS-B4 < 0 AND WS-B4 <= 999 AND WS-B4 > 1 AND WS-B4 >= 1)
        DISPLAY "" WS-STR FUNCTION TRIM(WS-STR)
        DISPLAY WS-B2 UPON SYSERR WITH NO ADVANCING
        DISPLAY 0
        DISPLAY +0.00
        DISPLAY -0.00
        DISPLAY .05
        DISPLAY 123.45
        DISPLAY -0.05
        DISPLAY -5
        DISPLAY ZERO SPACE HIGH-VALUE LOW-VALUE QUOTE ALL "Z"
    END-IF.
    CALL "NESTED-ONE".
    CALL "EXT-MOD".
    EXIT PROGRAM.
    STOP RUN.

    IDENTIFICATION DIVISION.
    PROGRAM-ID. NESTED-ONE.
    PROCEDURE DIVISION.
        GOBACK.
    END PROGRAM NESTED-ONE.
END PROGRAM exit.

IDENTIFICATION DIVISION.
PROGRAM-ID. SIBLING-TWO.
PROCEDURE DIVISION.
    CALL "NESTED-ONE".
    STOP RUN 0.
    IDENTIFICATION DIVISION.
    PROGRAM-ID. NESTED-ONE.
    PROCEDURE DIVISION.
        EXIT PROGRAM.
    END PROGRAM NESTED-ONE.
END PROGRAM SIBLING-TWO.

IDENTIFICATION DIVISION.
PROGRAM-ID. SIBLING-TWO.
PROCEDURE DIVISION.
    STOP RUN.
END PROGRAM SIBLING-TWO.
"#;

    let (sm, interner, hir) = compile_to_hir(src, Dialect::Gnu)?;
    let ctx = CobolMlirContext::new();
    let verified = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner)?;
    for opt in [
        OptimizationLevel::O0,
        OptimizationLevel::O1,
        OptimizationLevel::O2,
        OptimizationLevel::O3,
        OptimizationLevel::Os,
    ] {
        let lowered = lower_cobol_to_llvm_mlir(&ctx, &verified, opt, MainEntryMode::OmitMain)?;
        assert!(lowered.to_mlir_text().contains("__coblang_prog_EXIT"));
    }
    Ok(())
}

/// Returns remaining unimplemented ISO 2023 Data Division COBOL test snippets (`USAGE BIT`).
fn unimplemented_data_snippets() -> &'static [&'static str] {
    &[
        "IDENTIFICATION DIVISION.\u{000a}PROGRAM-ID. P.\u{000a}DATA DIVISION.\u{000a}WORKING-STORAGE SECTION.\u{000a}01 BT PIC 1 USAGE BIT.\u{000a}PROCEDURE DIVISION.\u{000a}STOP RUN.",
    ]
}

/// Returns remaining unimplemented Procedure Division COBOL test snippets.
fn unimplemented_proc_snippets() -> &'static [&'static str] {
    &[]
}

/// Verifies that newly-implemented Part 5 & M8 Data Division, `MOVE`, `INITIALIZE`, `PICTURE`, arithmetic, and control-flow constructs lower cleanly through `llvm` MLIR!
#[test]
fn part5_data_move_init_and_arithmetic_lower_cleanly() -> Result<()> {
    let snippets: &[&str] = &[
        "IDENTIFICATION DIVISION.\u{000a}PROGRAM-ID. P.\u{000a}DATA DIVISION.\u{000a}WORKING-STORAGE SECTION.\u{000a}01 T PIC X OCCURS 3.\u{000a}01 A PIC X(4) VALUE 'ABCD'.\u{000a}01 B REDEFINES A PIC 9(4).\u{000a}01 G.\u{000a}  05 E PIC X(4) JUSTIFIED RIGHT VALUE 'HI'.\u{000a}  05 K PIC S9(5)V99 COMP-3 VALUE -12.34.\u{000a}  05 ED PIC Z,ZZ9.99.\u{000a}  05 FL COMP-2 VALUE 1.5.\u{000a}  05 PT USAGE POINTER.\u{000a}  05 B8 PIC S9(18) COMP-5 VALUE -9223372036854775808.\u{000a}PROCEDURE DIVISION.\u{000a}MAIN.\u{000a}  PERFORM WORK.\u{000a}  GOBACK.\u{000a}WORK.\u{000a}  INITIALIZE G.\u{000a}  MOVE K TO ED.\u{000a}  ADD 1.5 TO K ROUNDED ON SIZE ERROR DISPLAY 'ERR' NOT ON SIZE ERROR DISPLAY ED END-ADD.\u{000a}  DIVIDE 7 BY 2 GIVING B REMAINDER B8.\u{000a}  IF A(1:2) = 'AB' AND K IS NUMERIC DISPLAY FUNCTION HEX-OF(K) PT FL 1.5E2 (B8 + 1) END-IF.\u{000a}  MOVE FUNCTION TRIM(A) TO T(2).",
        "IDENTIFICATION DIVISION.\u{000a}PROGRAM-ID. P.\u{000a}DATA DIVISION.\u{000a}WORKING-STORAGE SECTION.\u{000a}01 I PIC 9 VALUE 1.\u{000a}PROCEDURE DIVISION.\u{000a}S1 SECTION.\u{000a}P1.\u{000a}  PERFORM UNTIL I > 2\u{000a}    ADD 1 TO I\u{000a}  END-PERFORM.\u{000a}  IF I = 3 NEXT SENTENCE END-IF.\u{000a}  GO TO P2.\u{000a}P2.\u{000a}  DISPLAY I.\u{000a}  EXIT PARAGRAPH.\u{000a}P3.\u{000a}  STOP RUN.",
    ];
    let ctx = CobolMlirContext::new();
    for &src in snippets {
        let (sm, interner, hir) = compile_to_hir(src, Dialect::Gnu)?;
        let cobol_mod = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner)?;
        let llvm_mod = lower_cobol_to_llvm_mlir(
            &ctx,
            &cobol_mod,
            OptimizationLevel::O1,
            MainEntryMode::EmitMain,
        )?;
        assert!(llvm_mod.to_mlir_text().contains("coblang_rt_"));
    }
    Ok(())
}

/// Verifies that every unimplemented M5–M20 construct returns `CoblangError::UnimplementedFeature` cleanly.
#[test]
fn unimplemented_hir_constructs_return_clean_feature_errors() -> Result<()> {
    let ctx = CobolMlirContext::new();
    for &src in unimplemented_data_snippets()
        .iter()
        .chain(unimplemented_proc_snippets())
    {
        let (sm, interner, hir) = compile_to_hir(src, Dialect::Gnu)?;
        let res = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner);
        assert!(
            matches!(res, Err(CoblangError::UnimplementedFeature { .. })),
            "expected UnimplementedFeature for:\u{000a}{src}\u{000a}got: {res:?}"
        );
    }
    Ok(())
}

/// Exercises synthetic HIR and `lower_to_llvm` edge branches (`empty HirModule`, `PassManager` failure, unterminated `cobol.program`, empty `DISPLAY`).
#[test]
fn synthetic_hir_and_llvm_lowering_edge_branches() -> Result<()> {
    let ctx = CobolMlirContext::new();
    let sm = SourceMap::new();
    let mut interner = SymbolInterner::new();

    // 1. Empty `HirModule` -> `CoblangError::HirLowering`.
    let empty_hir = ResolvedHirModule(coblang_hir::HirModule {
        programs: Vec::new(),
        functions: Vec::new(),
        classes: Vec::new(),
        m13_units: Vec::new(),
    });
    assert!(matches!(
        lower_hir_to_cobol_mlir(&ctx, &empty_hir, &sm, &interner),
        Err(CoblangError::HirLowering { .. })
    ));

    // 2. Storage-only `VerifiedCobolMlirModule` (`primary_prog_sym == None`) + unterminated `cobol.program` + empty `cobol.display`.
    let loc = ctx.unknown_location();
    let st_only = Module::new(loc);
    let _ = st_only.body().append_operation(melior::dialect::func::func(
        ctx.raw(),
        StringAttribute::new(ctx.raw(), "__ext_noop"),
        melior::ir::attribute::TypeAttribute::new(
            melior::ir::r#type::FunctionType::new(ctx.raw(), &[], &[]).into(),
        ),
        Region::new(),
        &[(
            Identifier::new(ctx.raw(), "sym_visibility"),
            StringAttribute::new(ctx.raw(), "private").into(),
        )],
        loc,
    ));
    let _ = st_only.body().append_operation(
        CobolStorageOp {
            sym_name: "__rc_edge",
            storage_kind: StorageKind::Working,
            byte_size: 4,
            alignment: 4,
            init_bytes: &[0, 0, 0, 0],
        }
        .build(&ctx, loc)?,
    );
    let verified_st = verify_cobol_module(st_only)?;
    let _ = lower_cobol_to_llvm_mlir(
        &ctx,
        &verified_st,
        OptimizationLevel::O0,
        MainEntryMode::EmitMain,
    )?;

    let m_unterm = Module::new(loc);
    let _ = m_unterm.body().append_operation(
        CobolStorageOp {
            sym_name: "__rc_u",
            storage_kind: StorageKind::Working,
            byte_size: 4,
            alignment: 4,
            init_bytes: &[0, 0, 0, 0],
        }
        .build(&ctx, loc)?,
    );
    let reg = Region::new();
    let blk = reg.append_block(Block::new(&[]));
    let _ = blk.append_operation(
        CobolDisplayOp {
            ptr_len_pairs: &[],
            item_kinds: &[],
            item_scales: &[],
            device: OutputDevice::Stdout,
            advancing: LineAdvancing::NoAdvancing,
        }
        .build(&ctx, loc)?,
    );
    let _ = blk.append_operation(
        CobolPerformRangeOp {
            start_proc: 99,
            end_proc: 99,
        }
        .build(&ctx, loc)?,
    );
    let _ = m_unterm.body().append_operation(
        CobolProgramOp {
            sym_name: String::from("__cobol_proc_U_0"),
            raw_c_alias: String::new(),
            rc_slot_sym: String::from("__rc_u"),
            initial_state: InitialStateMode::Retained,
            recursivity: RecursivityMode::NonRecursive,
            entry_convention: EntryConventionMode::Cobol,
            body: reg,
        }
        .build(&ctx, loc)?,
    );
    let verified_u = verify_cobol_module(m_unterm)?;
    let _ = lower_cobol_to_llvm_mlir(
        &ctx,
        &verified_u,
        OptimizationLevel::O0,
        MainEntryMode::OmitMain,
    )?;

    synthetic_hir_remaining_unimpl_edges(&ctx, &sm, &mut interner)
}

/// Verifies `run_llvm_lowering_pipeline` returns `CoblangError::MlirPassExecution` on invalid ops.
#[test]
fn llvm_lowering_pipeline_pass_failure_coverage() -> Result<()> {
    let ctx = CobolMlirContext::new();
    let loc = ctx.unknown_location();
    let mut bad_pm_mod = Module::new(loc);
    let broken = finish_op_build(OperationBuilder::new("arith.addi", loc), "arith.addi")?;
    let _ = bad_pm_mod.body().append_operation(broken);
    let pm_err = coblang_mlir::lower_to_llvm::run_llvm_lowering_pipeline(
        &ctx,
        &mut bad_pm_mod,
        OptimizationLevel::O0,
    );
    assert!(matches!(
        pm_err,
        Err(CoblangError::MlirPassExecution { .. })
    ));
    Ok(())
}

/// Wraps a single `HirProgram` in a `ResolvedHirModule`.
///
/// # Arguments
///
/// * `prog` - Program unit.
fn single_prog_mod(prog: coblang_hir::HirProgram) -> ResolvedHirModule {
    ResolvedHirModule(coblang_hir::HirModule {
        programs: vec![prog],
        functions: Vec::new(),
        classes: Vec::new(),
        m13_units: Vec::new(),
    })
}

/// Exercises synthetic HIR branches unreachable from surface syntax (`ReportId`, invalid span fallback, negative `ScaleFactor`, subscripted `HirPlace`, remaining `HighLevelOp` tags).
///
/// # Arguments
///
/// * `ctx` - MLIR context.
/// * `sm` - Empty `SourceMap` (triggering `format_span_loc` `Err` fallback).
/// * `interner` - Symbol interner.
fn synthetic_hir_remaining_unimpl_edges(
    ctx: &CobolMlirContext,
    sm: &SourceMap,
    interner: &mut SymbolInterner,
) -> Result<()> {
    use coblang_common::{ProgramUnitId, ReportId, ScaleFactor, ScopeId};
    use coblang_hir::{
        HirAdvancing, HirConstValue, HirDecimalSign, HirDisplayDevice, HirExpr, HirProgram, HirStmt,
    };

    let bad_span = SourceSpan::new(
        FileId::new(42),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let mut sm_real = SourceMap::new();
    let fid = sm_real.add_file(String::from("s.cob"), String::from("X\u{000a}"))?;
    let ok_span = SourceSpan::new(fid, ByteOffset::ZERO, ByteLength::new(1), ExpansionId::ROOT);

    let p_sym = interner.intern("SYNTH")?;
    let neg_num = interner.intern("50")?;
    let bad_prog = HirProgram {
        id: ProgramUnitId::new(0),
        scope: ScopeId::new(0),
        name: p_sym,
        formatting: coblang_hir::HirProgramFormatting::default(),
        perform_style: coblang_hir::HirPerformStyle::IsoStack,
        odo_slide: coblang_hir::HirOdoSlideMode::FixedOffsets,
        recursivity: coblang_hir::HirRecursivity::NonRecursive,
        initial_state: coblang_hir::HirInitialState::RetainStorage,
        alphabets: Vec::new(),
        collating_sequence: None,
        custom_classes: Vec::new(),
        switches: Vec::new(),
        files: Vec::new(),
        data_items: Vec::new(),
        conditions_88: Vec::new(),
        reports: Vec::new(),
        report_plans: Vec::new(),
        screen_plans: Vec::new(),
        screen_special_regs: coblang_hir::HirScreenSpecialRegs::default(),
        using_params: Vec::new(),
        returning_item: None,
        entry_statements: Vec::new(),
        procedures: Vec::new(),
        fallthrough_limit_pid: None,
        declaratives: Vec::new(),
        nested: Vec::new(),
        span: bad_span,
    };
    assert!(
        lower_hir_to_cobol_mlir(ctx, &single_prog_mod(bad_prog.clone()), sm, interner).is_err()
    );

    let mut base_prog = bad_prog;
    base_prog.span = ok_span;

    let mut p_rep = base_prog.clone();
    p_rep.reports.push((ReportId::new(0), p_sym));
    assert!(lower_hir_to_cobol_mlir(ctx, &single_prog_mod(p_rep), &sm_real, interner).is_ok());

    let zero_sym = interner.intern("0")?;
    let mut p_scale = base_prog.clone();
    p_scale.entry_statements = vec![
        HirStmt::Display {
            operands: vec![HirExpr::Neg(Box::new(HirExpr::Const {
                value: HirConstValue::Decimal {
                    text: neg_num,
                    scale: ScaleFactor::try_new(-2)?,
                    sign: HirDecimalSign::UnsignedOrEmbedded,
                },
                span: ok_span,
            }))],
            device: HirDisplayDevice::Stdout,
            advancing: HirAdvancing::AdvanceLine,
            span: ok_span,
        },
        HirStmt::Return {
            mode: coblang_hir::HirReturnMode::StopRun,
            code: Some(HirExpr::Const {
                value: HirConstValue::Decimal {
                    text: neg_num,
                    scale: ScaleFactor::ZERO,
                    sign: HirDecimalSign::Negative,
                },
                span: ok_span,
            }),
            span: ok_span,
        },
        HirStmt::Return {
            mode: coblang_hir::HirReturnMode::StopRun,
            code: Some(HirExpr::Const {
                value: HirConstValue::Decimal {
                    text: zero_sym,
                    scale: ScaleFactor::ZERO,
                    sign: HirDecimalSign::Negative,
                },
                span: ok_span,
            }),
            span: ok_span,
        },
    ];
    let _ = lower_hir_to_cobol_mlir(ctx, &single_prog_mod(p_scale), &sm_real, interner)?;

    synthetic_hir_data_item_edges(ctx, &sm_real, interner, &base_prog, ok_span)?;
    synthetic_hir_place_and_verb_edges(ctx, &sm_real, interner, &base_prog, (ok_span, bad_span))
}

/// Exercises synthetic `HirDataItem` branches (`FileSection`, `parent.is_some()`, `HasDynamicOdo`, non-string `Alphanumeric` init, non-int `BinaryInt` init).
///
/// # Arguments
///
/// * `ctx` - MLIR context.
/// * `sm` - Populated `SourceMap`.
/// * `interner` - Symbol interner.
/// * `base_prog` - Template `HirProgram`.
/// * `ok_span` - Valid source span.
fn synthetic_hir_data_item_edges(
    ctx: &CobolMlirContext,
    sm: &SourceMap,
    interner: &mut SymbolInterner,
    base_prog: &coblang_hir::HirProgram,
    ok_span: SourceSpan,
) -> Result<()> {
    use coblang_common::{DataItemId, DigitCount, LevelNumber, ScaleFactor, ScopeId};
    use coblang_hir::{
        BinaryEndianness, HirConstValue, HirDataItem, HirStorageSection, HirType, JustifiedSpec,
        Signedness, TruncationPolicy,
    };

    let flt_sym = interner.intern("1.5")?;
    let base_item = HirDataItem {
        id: DataItemId::new(0),
        scope: ScopeId::new(0),
        name: None,
        level: LevelNumber::parse(1)?,
        parent: None,
        children: Vec::new(),
        section: HirStorageSection::WorkingStorage,
        ty: HirType::Alphanumeric {
            byte_len: 2,
            justified: JustifiedSpec::Left,
        },
        byte_offset: ByteOffset::ZERO,
        total_byte_size: 2,
        dimensions: Vec::new(),
        redefines: None,
        initial_value: Some(HirConstValue::Float(flt_sym)),
        span: ok_span,
    };

    // Valid lowering with non-bytes `Alphanumeric` init and non-int `BinaryInt` init (`_ => None` in `eval_const_i64`).
    let mut bin_item = base_item.clone();
    bin_item.id = DataItemId::new(1);
    bin_item.ty = HirType::BinaryInt {
        byte_width: 4,
        digits: DigitCount::try_new(9)?,
        scale: ScaleFactor::ZERO,
        sign: Signedness::Signed,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::FullBinaryRange,
    };
    bin_item.initial_value = Some(HirConstValue::Bytes(vec![b'4', b'2']));
    let mut const_item = base_item.clone();
    const_item.id = DataItemId::new(2);
    const_item.section = HirStorageSection::CompileTimeConstant;
    let mut ok_prog = base_prog.clone();
    ok_prog.data_items = vec![base_item.clone(), bin_item.clone(), const_item];
    let _ = lower_hir_to_cobol_mlir(ctx, &single_prog_mod(ok_prog.clone()), sm, interner)?;

    // Malformed non-numeric `VALUE "X"` on `BinaryInt` -> `PictureCodec` error.
    let mut bad_val_item = bin_item;
    bad_val_item.id = DataItemId::new(0);
    bad_val_item.initial_value = Some(HirConstValue::Bytes(vec![b'X']));
    let mut bad_val_prog = base_prog.clone();
    bad_val_prog.data_items = vec![bad_val_item];
    assert!(lower_hir_to_cobol_mlir(ctx, &single_prog_mod(bad_val_prog), sm, interner).is_err());

    // `AssignArith` targeting unknown `DataItemId(99)` -> `UnimplementedFeature`.
    let z_sym = interner.intern("0")?;
    ok_prog.entry_statements = vec![coblang_hir::HirStmt::AssignArith {
        targets: vec![(
            coblang_hir::HirPlace {
                item: DataItemId::new(99),
                static_offset: ByteOffset::ZERO,
                subscripts: Vec::new(),
                refmod: None,
                effective_ty: base_item.ty.clone(),
                check_mask: coblang_hir::ExceptionCheckMask::default(),
                span: ok_span,
            },
            coblang_hir::HirRounding::Truncate,
        )],
        expr: coblang_hir::HirExpr::Const {
            value: HirConstValue::Decimal {
                text: z_sym,
                scale: ScaleFactor::ZERO,
                sign: coblang_hir::HirDecimalSign::UnsignedOrEmbedded,
            },
            span: ok_span,
        },
        on_size_error: Vec::new(),
        not_on_size_error: Vec::new(),
        span: ok_span,
    }];
    assert!(lower_hir_to_cobol_mlir(ctx, &single_prog_mod(ok_prog), sm, interner).is_err());

    assert_synthetic_sections(ctx, sm, interner, base_prog, &base_item);
    Ok(())
}

/// Asserts `FileSection` / `ExternalWorkingStorage` / `Linkage` / `BasedStorage` / `HasDynamicOdo` lowering outcomes.
///
/// # Arguments
///
/// * `ctx` - MLIR context.
/// * `sm` - Populated `SourceMap`.
/// * `interner` - Symbol interner.
/// * `base_prog` - Template `HirProgram`.
/// * `base_item` - Template `HirDataItem`.
fn assert_synthetic_sections(
    ctx: &CobolMlirContext,
    sm: &SourceMap,
    interner: &SymbolInterner,
    base_prog: &coblang_hir::HirProgram,
    base_item: &coblang_hir::HirDataItem,
) {
    for (sec, expect_ok) in [
        (
            coblang_hir::HirStorageSection::FileSection(coblang_common::FileConnectorId::new(0)),
            true,
        ),
        (
            coblang_hir::HirStorageSection::ExternalWorkingStorage {
                external_name: base_prog.name,
                case_mode: coblang_hir::HirExternalCaseMode::CanonicalUpper,
            },
            true,
        ),
        (
            coblang_hir::HirStorageSection::ExternalWorkingStorage {
                external_name: base_prog.name,
                case_mode: coblang_hir::HirExternalCaseMode::ExactAlias,
            },
            true,
        ),
        (
            coblang_hir::HirStorageSection::InheritedGlobal {
                owner_unit: base_prog.id,
                owner_root: base_item.id,
            },
            true,
        ),
        (coblang_hir::HirStorageSection::Linkage, true),
        (coblang_hir::HirStorageSection::BasedStorage, true),
    ] {
        let mut p = base_prog.clone();
        p.data_items = vec![coblang_hir::HirDataItem {
            section: sec,
            ..base_item.clone()
        }];
        assert_eq!(
            lower_hir_to_cobol_mlir(ctx, &single_prog_mod(p), sm, interner).is_ok(),
            expect_ok
        );
    }
    let mut odo_item = base_item.clone();
    odo_item.ty = coblang_hir::HirType::Group {
        static_byte_len: 4,
        odo_presence: coblang_hir::DynamicOdoPresence::HasDynamicOdo,
    };
    let mut p_odo = base_prog.clone();
    p_odo.data_items = vec![odo_item];
    assert!(lower_hir_to_cobol_mlir(ctx, &single_prog_mod(p_odo), sm, interner).is_ok());
}

/// Exercises remaining synthetic `HirPlace` / `HighLevelOp` error branches in `lower_from_hir.rs`.
///
/// # Arguments
///
/// * `ctx` - MLIR context.
/// * `sm` - Populated `SourceMap`.
/// * `interner` - Symbol interner.
/// * `base_prog` - Template `HirProgram`.
/// * `spans` - Tuple `(ok_span, bad_span)`.
fn synthetic_hir_place_and_verb_edges(
    ctx: &CobolMlirContext,
    sm: &SourceMap,
    interner: &mut SymbolInterner,
    base_prog: &coblang_hir::HirProgram,
    spans: (SourceSpan, SourceSpan),
) -> Result<()> {
    use coblang_common::{DataItemId, ScaleFactor};
    use coblang_hir::{
        ExceptionCheckMask, HirAdvancing, HirConstValue, HirDecimalSign, HirDisplayDevice, HirExpr,
        HirPlace, HirRounding, HirStmt, HirType,
    };

    let (ok_span, bad_span) = spans;
    let zero_sym = interner.intern("0")?;
    let trim_sym = interner.intern("TRIM")?;
    let zero_expr = HirExpr::Const {
        value: HirConstValue::Decimal {
            text: zero_sym,
            scale: ScaleFactor::ZERO,
            sign: HirDecimalSign::UnsignedOrEmbedded,
        },
        span: ok_span,
    };
    let unknown_place = HirPlace {
        item: DataItemId::new(99),
        static_offset: ByteOffset::ZERO,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: HirType::Pointer,
        check_mask: ExceptionCheckMask::default(),
        span: ok_span,
    };
    let sub_place = HirPlace {
        subscripts: vec![(zero_expr.clone(), 1)],
        span: bad_span,
        ..unknown_place.clone()
    };

    let mut stmts = vec![
        HirStmt::Move {
            src: zero_expr.clone(),
            dst: sub_place,
            span: ok_span,
        },
        HirStmt::Move {
            src: zero_expr.clone(),
            dst: unknown_place.clone(),
            span: ok_span,
        },
        HirStmt::Initialize {
            targets: vec![unknown_place.clone()],
            filler: coblang_hir::HirInitFiller::ExcludeFiller,
            value_mode: coblang_hir::HirInitValueMode::DefaultCategory,
            replacements: Vec::new(),
            span: ok_span,
        },
        HirStmt::AssignArith {
            targets: vec![(unknown_place.clone(), HirRounding::Truncate)],
            expr: zero_expr.clone(),
            on_size_error: Vec::new(),
            not_on_size_error: Vec::new(),
            span: ok_span,
        },
        HirStmt::Display {
            operands: vec![HirExpr::Neg(Box::new(HirExpr::Place(
                unknown_place.clone(),
            )))],
            device: HirDisplayDevice::Stdout,
            advancing: HirAdvancing::AdvanceLine,
            span: ok_span,
        },
        HirStmt::Display {
            operands: vec![HirExpr::CallFunction {
                name: trim_sym,
                args: Vec::new(),
                span: ok_span,
            }],
            device: HirDisplayDevice::Stdout,
            advancing: HirAdvancing::AdvanceLine,
            span: ok_span,
        },
        HirStmt::Return {
            mode: coblang_hir::HirReturnMode::StopRun,
            code: Some(HirExpr::Place(unknown_place)),
            span: ok_span,
        },
    ];
    for verb in [
        "OPEN", "SORT", "INITIATE", "SEARCH", "INVOKE", "XML", "CANCEL", "SET",
    ] {
        let vs = interner.intern(verb)?;
        stmts.push(HirStmt::HighLevelOp {
            verb: vs,
            span: bad_span,
        });
    }

    for st in stmts {
        let mut p = base_prog.clone();
        p.entry_statements = vec![st];
        assert!(lower_hir_to_cobol_mlir(ctx, &single_prog_mod(p), sm, interner).is_err());
    }
    Ok(())
}

/// Builds a native/big-endian `BinaryInt` `HirPlace` for `opt_fastpath` unit tests.
///
/// # Arguments
///
/// * `spec` - `(byte_width, digits, scale)` tuple.
/// * `flags` - `(sign, endian, trunc)` tuple.
/// * `sp` - Source span.
fn mk_fast_bin_place(
    spec: (u8, u8, i16),
    flags: (
        coblang_hir::Signedness,
        coblang_hir::BinaryEndianness,
        coblang_hir::TruncationPolicy,
    ),
    sp: SourceSpan,
) -> Result<coblang_hir::HirPlace> {
    use coblang_common::{DataItemId, DigitCount, ScaleFactor};
    let (bw, dig, sc) = spec;
    let (sign, endian, trunc) = flags;
    Ok(coblang_hir::HirPlace {
        item: DataItemId::new(0),
        static_offset: ByteOffset::ZERO,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: coblang_hir::HirType::BinaryInt {
            byte_width: bw,
            digits: DigitCount::try_new(dig)?,
            scale: ScaleFactor::try_new(sc)?,
            sign,
            endian,
            trunc,
        },
        check_mask: coblang_hir::ExceptionCheckMask::default(),
        span: sp,
    })
}

/// Exercises all `opt_fastpath::try_classify_fast_move` branches (`FigurativeFill`, `DirectCopyPad`, overlap, subscripts, refmod, `Justified::Right`).
#[test]
fn opt_fastpath_move_matrix_coverage() -> Result<()> {
    use coblang_common::{DataItemId, DigitCount, ScaleFactor};
    use coblang_hir::{
        BlankZeroSpec, DynamicOdoPresence, ExceptionCheckMask, HirConstValue, HirDecimalSign,
        HirExpr, HirPlace, HirProgramFormatting, HirType, JustifiedSpec, NumericSignSpec,
    };
    use coblang_mlir::opt_fastpath::{FastMovePlan, try_classify_fast_move};

    let sp = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::new(1),
        ExpansionId::ROOT,
    );
    let mut interner = SymbolInterner::new();
    let z_sym = interner.intern("0")?;
    let fmt = HirProgramFormatting::default();
    let (r0, r1) = (DataItemId::new(0), DataItemId::new(1));
    let mk_place = |ty: HirType| HirPlace {
        item: r0,
        static_offset: ByteOffset::ZERO,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: ty,
        check_mask: ExceptionCheckMask::default(),
        span: sp,
    };
    let z_expr = HirExpr::Const {
        value: HirConstValue::Decimal {
            text: z_sym,
            scale: ScaleFactor::ZERO,
            sign: HirDecimalSign::UnsignedOrEmbedded,
        },
        span: sp,
    };
    let dst_a4 = mk_place(HirType::Alphanumeric {
        byte_len: 4,
        justified: JustifiedSpec::Left,
    });
    let dst_sub = HirPlace {
        subscripts: vec![(z_expr.clone(), 1)],
        ..dst_a4.clone()
    };
    let dst_rm = HirPlace {
        refmod: Some(coblang_hir::HirRefMod {
            offset: Box::new(z_expr.clone()),
            length: None,
        }),
        ..dst_a4.clone()
    };
    let dst_right = mk_place(HirType::Alphanumeric {
        byte_len: 4,
        justified: JustifiedSpec::Right,
    });
    let dst_zlen = mk_place(HirType::Alphanumeric {
        byte_len: 0,
        justified: JustifiedSpec::Left,
    });
    let dst_num = mk_place(HirType::ZonedDecimal {
        digits: DigitCount::ONE,
        scale: ScaleFactor::ZERO,
        sign: NumericSignSpec::Unsigned,
        blank_zero: BlankZeroSpec::RetainZero,
    });
    let dst_grp = mk_place(HirType::Group {
        static_byte_len: 4,
        odo_presence: DynamicOdoPresence::StaticSize,
    });
    let lit_ab = HirExpr::Const {
        value: HirConstValue::Bytes(vec![b'A', b'B']),
        span: sp,
    };

    for bad_dst in [&dst_rm, &dst_right, &dst_num] {
        assert!(try_classify_fast_move(&lit_ab, bad_dst, None, r0, fmt).is_none());
    }
    for ok_dst in [&dst_sub, &dst_zlen, &dst_grp] {
        assert!(matches!(
            try_classify_fast_move(&lit_ab, ok_dst, None, r0, fmt),
            Some(FastMovePlan::DirectCopyPad { .. })
        ));
    }
    let src_pl = HirExpr::Place(dst_a4.clone());
    assert!(try_classify_fast_move(&src_pl, &dst_a4, Some(r0), r0, fmt).is_none());
    assert!(try_classify_fast_move(&src_pl, &dst_a4, Some(r1), r0, fmt).is_some());
    for bad_sp in [dst_rm, mk_place(HirType::Pointer)] {
        assert!(
            try_classify_fast_move(&HirExpr::Place(bad_sp), &dst_a4, Some(r1), r0, fmt).is_none()
        );
    }
    Ok(())
}

/// Exercises `opt_fastpath::try_classify_fast_move` figurative and non-literal source branches.
#[test]
fn opt_fastpath_move_figurative_coverage() -> Result<()> {
    use coblang_common::{DataItemId, ScaleFactor};
    use coblang_hir::{
        ExceptionCheckMask, HirConstValue, HirDecimalSign, HirExpr, HirFigurative, HirPlace,
        HirProgramFormatting, HirType, JustifiedSpec,
    };
    use coblang_mlir::opt_fastpath::{FastMovePlan, try_classify_fast_move};

    let sp = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::new(1),
        ExpansionId::ROOT,
    );
    let mut interner = SymbolInterner::new();
    let z_sym = interner.intern("0")?;
    let fmt = HirProgramFormatting::default();
    let r0 = DataItemId::new(0);
    let dst_a4 = HirPlace {
        item: r0,
        static_offset: ByteOffset::ZERO,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        },
        check_mask: ExceptionCheckMask::default(),
        span: sp,
    };
    let z_expr = HirExpr::Const {
        value: HirConstValue::Decimal {
            text: z_sym,
            scale: ScaleFactor::ZERO,
            sign: HirDecimalSign::UnsignedOrEmbedded,
        },
        span: sp,
    };
    for fig in [
        HirFigurative::Space,
        HirFigurative::Zero,
        HirFigurative::LowValue,
        HirFigurative::Null,
        HirFigurative::HighValue,
        HirFigurative::Quote,
        HirFigurative::All(vec![b'Z']),
    ] {
        let fe = HirExpr::Const {
            value: HirConstValue::Figurative(fig),
            span: sp,
        };
        assert!(matches!(
            try_classify_fast_move(&fe, &dst_a4, None, r0, fmt),
            Some(FastMovePlan::FigurativeFill { .. })
        ));
    }
    let multi = HirExpr::Const {
        value: HirConstValue::Figurative(HirFigurative::All(vec![b'A', b'B'])),
        span: sp,
    };
    for bad_src in [multi, z_expr.clone(), HirExpr::Neg(Box::new(z_expr))] {
        assert!(try_classify_fast_move(&bad_src, &dst_a4, None, r0, fmt).is_none());
    }
    Ok(())
}

/// Exercises `opt_fastpath` destination target rules, scaling, and `hardware_bounds`.
#[test]
fn opt_fastpath_int_targets_and_scaling_coverage() -> Result<()> {
    use coblang_common::ScaleFactor;
    use coblang_hir::{
        BinaryEndianness, HirConstValue, HirDecimalSign, HirExpr, HirPlace, HirRounding, HirType,
        Signedness, TruncationPolicy,
    };
    use coblang_mlir::opt_fastpath::try_classify_fast_int_arith;

    let sp = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::new(1),
        ExpansionId::ROOT,
    );
    let mut interner = SymbolInterner::new();
    let s_nat_full = (
        Signedness::Signed,
        BinaryEndianness::Native,
        TruncationPolicy::FullBinaryRange,
    );
    let s_nat_pic = (
        Signedness::Signed,
        BinaryEndianness::Native,
        TruncationPolicy::TruncateToPic,
    );
    let s_be_full = (
        Signedness::Signed,
        BinaryEndianness::BigEndian,
        TruncationPolicy::FullBinaryRange,
    );
    let s4 = mk_fast_bin_place((4, 9, 0), s_nat_full, sp)?;
    let s4_sc2 = mk_fast_bin_place((4, 9, 2), s_nat_pic, sp)?;
    let s4_sc38 = mk_fast_bin_place((4, 9, 38), s_nat_full, sp)?;
    let s4_neg37 = mk_fast_bin_place((4, 9, -37), s_nat_full, sp)?;

    let (sym_5, sym_500) = (interner.intern("5")?, interner.intern("500")?);
    let c5 = HirExpr::Const {
        value: HirConstValue::Decimal {
            text: sym_5,
            scale: ScaleFactor::ZERO,
            sign: HirDecimalSign::Negative,
        },
        span: sp,
    };
    let c5_exact = HirExpr::Const {
        value: HirConstValue::Decimal {
            text: sym_500,
            scale: ScaleFactor::try_new(2)?,
            sign: HirDecimalSign::UnsignedOrEmbedded,
        },
        span: sp,
    };
    assert!(try_classify_fast_int_arith(&[], &c5, &interner).is_none());
    assert!(
        try_classify_fast_int_arith(&[(s4.clone(), HirRounding::NearestEven)], &c5, &interner)
            .is_none()
    );
    let s4_rm = HirPlace {
        refmod: Some(coblang_hir::HirRefMod {
            offset: Box::new(c5.clone()),
            length: None,
        }),
        ..s4.clone()
    };
    for bad_dst in [
        s4_rm,
        mk_fast_bin_place((16, 35, 0), s_nat_full, sp)?,
        mk_fast_bin_place((4, 9, 0), s_be_full, sp)?,
        mk_fast_bin_place((8, 38, 0), s_nat_full, sp)?,
        HirPlace {
            effective_ty: HirType::Pointer,
            ..s4.clone()
        },
    ] {
        assert!(
            try_classify_fast_int_arith(&[(bad_dst, HirRounding::Truncate)], &c5, &interner)
                .is_none()
        );
    }
    let tgt_sc2 = [(s4_sc2.clone(), HirRounding::Truncate)];
    assert!(
        try_classify_fast_int_arith(&tgt_sc2, &HirExpr::Place(s4_sc2.clone()), &interner).is_some()
    );
    assert!(try_classify_fast_int_arith(&tgt_sc2, &c5, &interner).is_some());
    assert!(
        try_classify_fast_int_arith(&[(s4.clone(), HirRounding::Truncate)], &c5_exact, &interner)
            .is_some()
    );
    assert!(
        try_classify_fast_int_arith(
            &[(s4, HirRounding::Truncate)],
            &HirExpr::Place(s4_sc2),
            &interner
        )
        .is_none()
    );
    assert!(
        try_classify_fast_int_arith(&[(s4_sc38, HirRounding::Truncate)], &c5, &interner).is_none()
    );
    assert!(try_classify_fast_int_arith(&tgt_sc2, &HirExpr::Place(s4_neg37), &interner).is_none());
    Ok(())
}

/// Builds an unsigned `HirExpr::Const` decimal literal for `opt_fastpath` unit tests.
///
/// # Arguments
///
/// * `text` - Interned symbol ID.
/// * `scale` - Scale factor.
/// * `sp` - Source span.
fn mk_dec_expr(
    text: coblang_common::SymbolId,
    scale: coblang_common::ScaleFactor,
    sp: SourceSpan,
) -> coblang_hir::HirExpr {
    coblang_hir::HirExpr::Const {
        value: coblang_hir::HirConstValue::Decimal {
            text,
            scale,
            sign: coblang_hir::HirDecimalSign::UnsignedOrEmbedded,
        },
        span: sp,
    }
}

/// Exercises `opt_fastpath::lower_fast_int_expr` expression & 63-bit multiplication rules.
#[test]
fn opt_fastpath_int_expr_and_mul_coverage() -> Result<()> {
    use coblang_common::ScaleFactor;
    use coblang_hir::{
        BinaryEndianness, HirBinOp, HirConstValue, HirExpr, HirFigurative, HirPlace, HirRounding,
        HirType, Signedness, TruncationPolicy,
    };
    use coblang_mlir::opt_fastpath::try_classify_fast_int_arith;

    let sp = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::new(1),
        ExpansionId::ROOT,
    );
    let mut interner = SymbolInterner::new();
    let s_nat_full = (
        Signedness::Signed,
        BinaryEndianness::Native,
        TruncationPolicy::FullBinaryRange,
    );
    let s4 = mk_fast_bin_place((4, 9, 0), s_nat_full, sp)?;
    let s4_sc2 = mk_fast_bin_place((4, 9, 2), s_nat_full, sp)?;

    let (sym_5, sym_bad) = (interner.intern("5")?, interner.intern("bad")?);
    let sym_huge = interner.intern("99999999999999999999999999")?;
    let c5 = mk_dec_expr(sym_5, ScaleFactor::ZERO, sp);
    let s4_rm = HirPlace {
        refmod: Some(coblang_hir::HirRefMod {
            offset: Box::new(c5.clone()),
            length: None,
        }),
        ..s4.clone()
    };
    let tgt0 = [(s4.clone(), HirRounding::Truncate)];
    let tgt_sc2 = [(s4_sc2, HirRounding::Truncate)];

    let s16_p = mk_fast_bin_place((16, 35, 0), s_nat_full, sp)?;
    for bad_e in [
        HirExpr::Place(s4_rm),
        HirExpr::Place(s16_p),
        HirExpr::Place(HirPlace {
            effective_ty: HirType::Pointer,
            ..s4.clone()
        }),
        HirExpr::Const {
            value: HirConstValue::Figurative(HirFigurative::Space),
            span: sp,
        },
        mk_dec_expr(sym_5, ScaleFactor::try_new(2)?, sp),
        mk_dec_expr(sym_bad, ScaleFactor::ZERO, sp),
        mk_dec_expr(sym_huge, ScaleFactor::ZERO, sp),
        HirExpr::Neg(Box::new(HirExpr::LengthOf(s4.clone()))),
        HirExpr::Binary {
            op: HirBinOp::Pow,
            lhs: Box::new(c5.clone()),
            rhs: Box::new(c5.clone()),
            span: sp,
        },
        HirExpr::LengthOf(s4),
    ] {
        assert!(try_classify_fast_int_arith(&tgt0, &bad_e, &interner).is_none());
    }
    let div_e = HirExpr::Binary {
        op: HirBinOp::Div,
        lhs: Box::new(c5.clone()),
        rhs: Box::new(c5),
        span: sp,
    };
    assert!(try_classify_fast_int_arith(&tgt_sc2, &div_e, &interner).is_none());
    Ok(())
}

/// Exercises `opt_fastpath` 63-bit leaf multiplication safety rules.
#[test]
fn opt_fastpath_int_63bit_mul_coverage() -> Result<()> {
    use coblang_common::ScaleFactor;
    use coblang_hir::{
        BinaryEndianness, HirBinOp, HirConstValue, HirExpr, HirFigurative, HirRounding, Signedness,
        TruncationPolicy,
    };
    use coblang_mlir::opt_fastpath::try_classify_fast_int_arith;

    let sp = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::new(1),
        ExpansionId::ROOT,
    );
    let mut interner = SymbolInterner::new();
    let s_nat_full = (
        Signedness::Signed,
        BinaryEndianness::Native,
        TruncationPolicy::FullBinaryRange,
    );
    let u_nat_full = (
        Signedness::Unsigned,
        BinaryEndianness::Native,
        TruncationPolicy::FullBinaryRange,
    );
    let s4 = mk_fast_bin_place((4, 9, 0), s_nat_full, sp)?;
    let s4_sc2 = mk_fast_bin_place((4, 9, 2), s_nat_full, sp)?;
    let u8_p = mk_fast_bin_place((8, 18, 0), u_nat_full, sp)?;
    let s8_p = mk_fast_bin_place((8, 18, 0), s_nat_full, sp)?;
    let c5 = mk_dec_expr(interner.intern("5")?, ScaleFactor::ZERO, sp);
    let tgt0 = [(s4.clone(), HirRounding::Truncate)];
    let tgt_sc2 = [(s4_sc2, HirRounding::Truncate)];

    let add_c5 = HirExpr::Binary {
        op: HirBinOp::Add,
        lhs: Box::new(c5.clone()),
        rhs: Box::new(c5.clone()),
        span: sp,
    };
    for bad_lhs in [HirExpr::Place(u8_p), add_c5] {
        let m = HirExpr::Binary {
            op: HirBinOp::Mul,
            lhs: Box::new(bad_lhs),
            rhs: Box::new(c5.clone()),
            span: sp,
        };
        assert!(try_classify_fast_int_arith(&tgt0, &m, &interner).is_none());
    }
    let mul_c5 = HirExpr::Binary {
        op: HirBinOp::Mul,
        lhs: Box::new(c5.clone()),
        rhs: Box::new(c5),
        span: sp,
    };
    assert!(try_classify_fast_int_arith(&tgt_sc2, &mul_c5, &interner).is_none());
    let zero_c = HirExpr::Const {
        value: HirConstValue::Figurative(HirFigurative::Zero),
        span: sp,
    };
    let neg_mul = HirExpr::Binary {
        op: HirBinOp::Mul,
        lhs: Box::new(HirExpr::Place(s8_p.clone())),
        rhs: Box::new(HirExpr::Neg(Box::new(zero_c.clone()))),
        span: sp,
    };
    assert!(try_classify_fast_int_arith(&tgt0, &neg_mul, &interner).is_none());
    for ok_lhs in [s8_p, s4] {
        let m = HirExpr::Binary {
            op: HirBinOp::Mul,
            lhs: Box::new(HirExpr::Place(ok_lhs)),
            rhs: Box::new(zero_c.clone()),
            span: sp,
        };
        assert!(try_classify_fast_int_arith(&tgt0, &m, &interner).is_some());
    }
    Ok(())
}

/// Exercises `storage_init` formatting conventions, type alignments, scientific parsing, and `format_hir_decimal_ascii`.
#[test]
fn storage_init_helpers_and_scientific_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_hir::{
        BinaryEndianness, BlankZeroSpec, FloatBinaryWidth, FloatDecimalWidth, HirConstValue,
        HirDecimalSign, HirProgramFormatting, HirSignConvention, HirType, NumericSignSpec,
        Signedness, TruncationPolicy,
    };
    use coblang_mlir::storage_init::{
        encode_const_into_slice, format_hir_decimal_ascii, make_runtime_field_spec,
        parse_scientific_or_decimal, validate_supported_type,
    };

    let sm = SourceMap::new();
    let bad_sp = SourceSpan::new(
        FileId::new(9),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    for sc in [
        HirSignConvention::AsciiGnu,
        HirSignConvention::EbcdicOverpunch,
        HirSignConvention::IbmMainframe,
    ] {
        let fmt = HirProgramFormatting {
            sign_convention: sc,
            ..HirProgramFormatting::default()
        };
        let _ = make_runtime_field_spec(HirType::Pointer, fmt);
    }

    for bw in [1_u8, 2, 3, 6, 16] {
        let ty = HirType::BinaryInt {
            byte_width: bw,
            digits: DigitCount::ONE,
            scale: ScaleFactor::ZERO,
            sign: Signedness::Signed,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::FullBinaryRange,
        };
        assert!(validate_supported_type(&ty, &sm, bad_sp)? >= 1);
    }
    for fty in [
        FloatBinaryWidth::Bits32,
        FloatBinaryWidth::Bits64,
        FloatBinaryWidth::Bits128,
    ] {
        assert!(validate_supported_type(&HirType::FloatBinary { width: fty }, &sm, bad_sp)? >= 4);
    }
    for dty in [FloatDecimalWidth::Decimal64, FloatDecimalWidth::Decimal128] {
        assert!(validate_supported_type(&HirType::FloatDecimal { width: dty }, &sm, bad_sp)? >= 8);
    }

    assert!(parse_scientific_or_decimal("1.25E2").is_ok());
    for bad_s in ["badE2", "bad"] {
        assert!(parse_scientific_or_decimal(bad_s).is_err());
    }
    assert_eq!(
        format_hir_decimal_ascii("", ScaleFactor::ZERO, HirDecimalSign::UnsignedOrEmbedded),
        "0"
    );
    assert_eq!(
        format_hir_decimal_ascii("5", ScaleFactor::try_new(3)?, HirDecimalSign::Negative),
        "-0.005"
    );

    let mut interner = SymbolInterner::new();
    let (z0, empty_s) = (interner.intern("00")?, interner.intern("")?);
    let zd_ty = HirType::ZonedDecimal {
        digits: DigitCount::try_new(2)?,
        scale: ScaleFactor::ZERO,
        sign: NumericSignSpec::EmbeddedTrailing,
        blank_zero: BlankZeroSpec::RetainZero,
    };
    let zd_spec = make_runtime_field_spec(zd_ty, HirProgramFormatting::default());
    let mut slice = [0_u8; 2];
    let neg0 = HirConstValue::Decimal {
        text: z0,
        scale: ScaleFactor::ZERO,
        sign: HirDecimalSign::Negative,
    };
    encode_const_into_slice(&neg0, &mut slice, &zd_spec.desc, &interner)?;
    assert_eq!(&slice, b"0p");
    let ptr_spec = make_runtime_field_spec(HirType::Pointer, HirProgramFormatting::default());
    encode_const_into_slice(&neg0, &mut [0; 8], &ptr_spec.desc, &interner)?;
    for txt in [z0, empty_s] {
        let pos = HirConstValue::Decimal {
            text: txt,
            scale: ScaleFactor::ZERO,
            sign: HirDecimalSign::UnsignedOrEmbedded,
        };
        encode_const_into_slice(&pos, &mut slice, &zd_spec.desc, &interner)?;
    }
    Ok(())
}

/// Exercises `.bss` > 64 KiB, `INITIALIZE TO VALUE` with inherited group `VALUE`, `RefMod` init, and fast `i128` arithmetic.
#[test]
fn part5_full_pipeline_fast_int_bss_and_cond_coverage() -> Result<()> {
    let src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. P5FULL.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 BIG-BSS.
   05 BIG-HDR PIC X(5) VALUE "HELLO".
   05 BIG-PAD PIC X(100) OCCURS 700.
01 BIG-BSS-2.
   05 BIG-PAD-2 PIC X(100) OCCURS 700.
01 GRP-VAL VALUE "ABCD".
   05 GV-1 PIC X(2).
   05 GV-2 PIC X(2).
01 FAST-BINS.
   05 B1-S BINARY-CHAR SIGNED VALUE -2.
   05 B1-U BINARY-CHAR UNSIGNED VALUE 2.
   05 B2-S BINARY-SHORT SIGNED VALUE -10.
   05 B2-U BINARY-SHORT UNSIGNED VALUE 10.
   05 B4-TR PIC 9(4) BINARY VALUE 12.
   05 B8-S BINARY-DOUBLE SIGNED VALUE 100.
   05 B16-S PIC S9(30) COMP-5 VALUE 1.
   05 ZD-NEG PIC S9(3) VALUE -0.
   05 A-STR PIC X(6) VALUE " abCD ".
PROCEDURE DIVISION.
MAIN-P.
    ADD B1-S TO B2-S ON SIZE ERROR DISPLAY "SE" NOT ON SIZE ERROR DISPLAY "OK" END-ADD.
    SUBTRACT B1-U FROM B2-U.
    MULTIPLY 3 BY B4-TR ON SIZE ERROR CONTINUE END-MULTIPLY.
    DIVIDE 2 INTO B8-S ON SIZE ERROR CONTINUE NOT ON SIZE ERROR DISPLAY "OK" END-DIVIDE.
    DIVIDE 2 INTO B8-S.
    COMPUTE B8-S = - B2-S + (B1-S * 4) - (B2-U / 2).
    COMPUTE B8-S = B8-S + 1 ON SIZE ERROR DISPLAY "SE" NOT ON SIZE ERROR DISPLAY "OK" END-COMPUTE.
    ADD 0 TO ZD-NEG.
    COMPUTE ZD-NEG = - ZD-NEG + LENGTH OF A-STR + 1.5E1 - 2 ** 2.
    MOVE (B2-S + 1) TO ZD-NEG.
    MOVE 123 TO A-STR.
    MOVE ALL "Z" TO A-STR.
    MOVE ALL "XY" TO A-STR.
    MOVE FUNCTION TRIM(A-STR "LEADING") TO GV-1.
    MOVE FUNCTION TRIM(A-STR "TRAILING") TO GV-2.
    MOVE FUNCTION TRIM(A-STR "BOTH") TO GV-2.
    INITIALIZE GRP-VAL WITH FILLER ALL TO VALUE.
    INITIALIZE A-STR(1:2).
    INITIALIZE FAST-BINS REPLACING NUMERIC BY 1 ALPHANUMERIC BY "Q".
    INITIALIZE FAST-BINS REPLACING NUMERIC BY 1.
    IF A-STR IS ALPHABETIC OR A-STR IS ALPHABETIC-LOWER OR A-STR IS ALPHABETIC-UPPER
        DISPLAY "CLASS" UPON SYSERR WITH NO ADVANCING
    END-IF.
    IF ZD-NEG = ZERO AND ZERO <= ZD-NEG AND ZD-NEG <> 5 AND ZD-NEG < 9 AND ZD-NEG > -9 AND ZD-NEG >= -9
        DISPLAY ZERO SPACE HIGH-VALUE LOW-VALUE QUOTE ALL "!" 1.5E1 LENGTH OF GV-1 FUNCTION TRIM(GV-1)
    END-IF.
    STOP RUN RETURNING B2-S.
"#;
    let ctx = CobolMlirContext::new();
    let (sm, interner, hir) = compile_to_hir(src, Dialect::Gnu)?;
    let cobol_mod = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner)?;
    let llvm_mod = lower_cobol_to_llvm_mlir(
        &ctx,
        &cobol_mod,
        OptimizationLevel::O0,
        MainEntryMode::EmitMain,
    )?;
    assert!(llvm_mod.to_mlir_text().contains("__cobol_bss_guard_"));
    Ok(())
}

/// Exercises `COMP-5` inline `i128` fast-path arithmetic, all 8 rounding modes, `DIVIDE ... REMAINDER ... ON SIZE ERROR`, 2D `OCCURS`, and `JUSTIFIED RIGHT` moves.
#[test]
fn fast_int_comp5_rounding_and_cond_pipeline_coverage() -> Result<()> {
    let src = r#"IDENTIFICATION DIVISION.
PROGRAM-ID. COMP5FAST.
DATA DIVISION.
WORKING-STORAGE SECTION.
78 C78-FIVE VALUE 5.
01 COMP5-BINS.
   05 C1-S PIC S9(2) COMP-5 VALUE -2.
   05 C1-U PIC 9(2) COMP-5 VALUE 2.
   05 C2-S PIC S9(4) COMP-5 VALUE -0.
   05 C2-SC PIC S9(2)V9(2) COMP-5 VALUE 1.25.
   05 C4-S PIC S9(9) COMP-5 VALUE 100.
   05 C8-S PIC S9(18) COMP-5 VALUE 1000.
   05 ZD-A PIC S9(4) VALUE 10.
   05 ZD-B PIC S9(4) VALUE 20.
   05 JR-STR PIC X(4) JUSTIFIED RIGHT.
   05 A-STR PIC X(6) VALUE "ABCDEF".
01 NEST-GRP.
   05 FILLER PIC X VALUE "F".
   05 SUB-GRP.
      10 SUB-ITEM PIC X VALUE "S".
         88 IS-S VALUE "S".
   05 R-ORIG PIC X(2) VALUE "OR".
   05 R-ALIAS REDEFINES R-ORIG PIC X(2).
01 MAT-2D.
   05 M-ROW OCCURS 2.
      10 M-COL PIC X(2) OCCURS 2 VALUE "XY".
PROCEDURE DIVISION.
MAIN-P.
    ADD C1-S TO C2-S ON SIZE ERROR DISPLAY "SE" NOT ON SIZE ERROR CONTINUE END-ADD.
    SUBTRACT 5 FROM C1-U.
    COMPUTE C8-S = - C4-S + (C2-S * 3) - (C4-S / 2) ON SIZE ERROR DISPLAY "SE" NOT ON SIZE ERROR DISPLAY "OK" END-COMPUTE.
    COMPUTE ZD-A = - ZD-A.
    ADD 1 TO ZD-A ZD-B.
    ADD 1 TO ZD-A ROUNDED MODE IS NEAREST-EVEN.
    ADD 1 TO ZD-A ROUNDED MODE IS NEAREST-TOWARD-ZERO.
    ADD 1 TO ZD-A ROUNDED MODE IS PROHIBITED.
    ADD 1 TO ZD-A ROUNDED MODE IS TOWARD-GREATER.
    ADD 1 TO ZD-A ROUNDED MODE IS TOWARD-LESSER.
    ADD 1 TO ZD-A ROUNDED MODE IS AWAY-FROM-ZERO.
    DIVIDE 7 BY 2 GIVING ZD-A REMAINDER ZD-B ON SIZE ERROR DISPLAY "DSE" NOT ON SIZE ERROR CONTINUE END-DIVIDE.
    PERFORM DISPLAY C78-FIVE END-PERFORM.
    MOVE SPACES TO A-STR.
    MOVE A-STR(2:) TO JR-STR.
    MOVE ZERO TO JR-STR.
    MOVE SPACE TO JR-STR.
    MOVE HIGH-VALUE TO JR-STR.
    MOVE LOW-VALUE TO JR-STR.
    MOVE QUOTE TO JR-STR.
    MOVE ALL "K" TO JR-STR.
    MOVE -12 TO A-STR.
    MOVE 1.25 TO A-STR.
    MOVE 1.5E1 TO A-STR.
    INITIALIZE NEST-GRP.
    INITIALIZE MAT-2D.
    IF (C2-S + 1) > 0 AND FUNCTION TRIM(A-STR) = "AB"
        STOP RUN RETURNING C8-S
    END-IF.
    IF C1-U > 0
        STOP RUN RETURNING C1-U
    END-IF.
    IF C2-S > 0
        STOP RUN RETURNING C2-S
    END-IF.
    IF C2-SC > 0
        STOP RUN RETURNING C2-SC
    END-IF.
    IF ZD-B > 0
        STOP RUN RETURNING A-STR(1:1)
    END-IF.
    IF ZD-A = 999
        STOP RUN RETURNING 1.5
    END-IF.
    STOP RUN RETURNING 99999999999999999999999.
"#;
    let ctx = CobolMlirContext::new();
    let (sm, interner, hir) = compile_to_hir(src, Dialect::Gnu)?;
    let cobol_mod = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner)?;
    let _ = lower_cobol_to_llvm_mlir(
        &ctx,
        &cobol_mod,
        OptimizationLevel::O0,
        MainEntryMode::EmitMain,
    )?;
    Ok(())
}

/// Compiles a minimal 4-line program and returns `(SourceMap, SymbolInterner, HirProgram)`.
fn empty_hir_template() -> Result<(SourceMap, SymbolInterner, coblang_hir::HirProgram)> {
    let (sm, interner, hir) = compile_to_hir(
        "IDENTIFICATION DIVISION.\nPROGRAM-ID. T.\nPROCEDURE DIVISION.\n    STOP RUN.\n",
        Dialect::Gnu,
    )?;
    let prog = hir
        .0
        .programs
        .first()
        .cloned()
        .ok_or(CoblangError::HirLowering {
            construct: "test",
            location: String::new(),
            message: String::new(),
        })?;
    Ok((sm, interner, prog))
}

/// Exercises native-endian `PicModulo` fast-int bounds, 128-bit `emit_i128_const`, and synthetic HIR `storage_init` / `lower_from_hir` edges.
#[test]
fn synthetic_hir_128bit_cycles_and_unimpl_coverage() -> Result<()> {
    use coblang_common::{DataItemId, DigitCount, LevelNumber, ScaleFactor, ScopeId};
    use coblang_hir::{
        BinaryEndianness, ExceptionCheckMask, HirConstValue, HirDataItem, HirExpr, HirOccursDim,
        HirPlace, HirRounding, HirStmt, HirStorageSection, HirType, Signedness, TruncationPolicy,
    };

    let ctx = CobolMlirContext::new();
    let (sm, mut interner, base_prog) = empty_hir_template()?;
    let ok_sp = base_prog.span;
    let sr_sym = interner.intern("SORT-RETURN")?;
    let c0_sym = interner.intern("0")?;

    let b8_pic20 = HirType::BinaryInt {
        byte_width: 8,
        digits: DigitCount::try_new(20)?,
        scale: ScaleFactor::ZERO,
        sign: Signedness::Signed,
        endian: BinaryEndianness::Native,
        trunc: TruncationPolicy::TruncateToPic,
    };
    let dim10k = HirOccursDim {
        min: 10_000,
        max: 10_000,
        depending_on: None,
        element_byte_stride: 8,
        keys: Vec::new(),
        indexed_by: Vec::new(),
    };
    let item0 = HirDataItem {
        id: DataItemId::new(0),
        scope: ScopeId::new(0),
        name: None,
        level: LevelNumber::parse(77)?,
        parent: None,
        children: Vec::new(),
        section: HirStorageSection::WorkingStorage,
        ty: b8_pic20.clone(),
        byte_offset: ByteOffset::ZERO,
        total_byte_size: 80_000,
        dimensions: vec![dim10k],
        redefines: None,
        initial_value: None,
        span: ok_sp,
    };
    let sr_item = HirDataItem {
        id: DataItemId::new(1),
        name: Some(sr_sym),
        section: HirStorageSection::SpecialRegister,
        total_byte_size: 8,
        dimensions: Vec::new(),
        ..item0.clone()
    };
    let pl0 = HirPlace {
        item: DataItemId::new(0),
        static_offset: ByteOffset::ZERO,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: b8_pic20,
        check_mask: ExceptionCheckMask::default(),
        span: ok_sp,
    };
    let c0_val = HirConstValue::Decimal {
        text: c0_sym,
        scale: ScaleFactor::ZERO,
        sign: coblang_hir::HirDecimalSign::UnsignedOrEmbedded,
    };
    let mut p_ok = base_prog.clone();
    p_ok.data_items = vec![item0, sr_item];
    p_ok.entry_statements = vec![
        HirStmt::AssignArith {
            targets: vec![(pl0.clone(), HirRounding::Truncate)],
            expr: HirExpr::Const {
                value: c0_val,
                span: ok_sp,
            },
            on_size_error: Vec::new(),
            not_on_size_error: Vec::new(),
            span: ok_sp,
        },
        HirStmt::AssignArith {
            targets: vec![(pl0.clone(), HirRounding::NearestEven)],
            expr: HirExpr::Const {
                value: HirConstValue::Bytes(vec![b'7']),
                span: ok_sp,
            },
            on_size_error: Vec::new(),
            not_on_size_error: Vec::new(),
            span: ok_sp,
        },
        HirStmt::Move {
            src: HirExpr::Const {
                value: HirConstValue::Figurative(coblang_hir::HirFigurative::Null),
                span: ok_sp,
            },
            dst: pl0,
            span: ok_sp,
        },
    ];
    let _ = lower_hir_to_cobol_mlir(&ctx, &single_prog_mod(p_ok), &sm, &interner)?;
    Ok(())
}

/// Exercises `is_item_under_redefines` (`Renames66`, `stop_parent`, cycles, missing parent), `hardware_bounds`, and ODO `validate_item_flags`.
#[test]
fn storage_init_redefines_cycles_and_odo_coverage() -> Result<()> {
    use coblang_common::{DataItemId, LevelNumber, ScopeId};
    use coblang_hir::{HirDataItem, HirOccursDim, HirStorageSection, HirType, Signedness};
    use coblang_mlir::opt_fastpath::hardware_bounds;
    use coblang_mlir::storage_init::{build_program_storage_plan, is_item_under_redefines};

    for bw in [1_u8, 2, 4, 8] {
        assert!(hardware_bounds(bw, Signedness::Signed).is_some());
        assert!(hardware_bounds(bw, Signedness::Unsigned).is_some());
    }
    assert!(hardware_bounds(3, Signedness::Signed).is_none());

    let (sm, interner, hir) = compile_to_hir(
        "IDENTIFICATION DIVISION.\nPROGRAM-ID. T.\nPROCEDURE DIVISION.\n    STOP RUN.\n",
        Dialect::Gnu,
    )?;
    let base_prog = hir
        .0
        .programs
        .first()
        .cloned()
        .ok_or(CoblangError::HirLowering {
            construct: "test",
            location: String::new(),
            message: String::new(),
        })?;
    let bad_sp = SourceSpan::new(
        FileId::new(99),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    let item0 = HirDataItem {
        id: DataItemId::new(0),
        scope: ScopeId::new(0),
        name: None,
        level: LevelNumber::parse(1)?,
        parent: None,
        children: Vec::new(),
        section: HirStorageSection::WorkingStorage,
        ty: HirType::Pointer,
        byte_offset: ByteOffset::ZERO,
        total_byte_size: 8,
        dimensions: Vec::new(),
        redefines: None,
        initial_value: None,
        span: bad_sp,
    };
    let mut ren66 = item0.clone();
    ren66.level = LevelNumber::Renames66;
    assert!(is_item_under_redefines(
        &ren66,
        None,
        std::slice::from_ref(&ren66)
    ));
    let mut cyc = item0.clone();
    cyc.parent = Some(DataItemId::new(0));
    assert!(!is_item_under_redefines(
        &cyc,
        Some(DataItemId::new(0)),
        std::slice::from_ref(&cyc)
    ));
    assert!(!is_item_under_redefines(
        &cyc,
        None,
        std::slice::from_ref(&cyc)
    ));
    let mut bad_parent = item0.clone();
    bad_parent.parent = Some(DataItemId::new(99));
    assert!(!is_item_under_redefines(
        &bad_parent,
        None,
        std::slice::from_ref(&bad_parent)
    ));

    let mut odo_item = item0.clone();
    odo_item.dimensions = vec![HirOccursDim {
        min: 1,
        max: 5,
        depending_on: Some(DataItemId::new(0)),
        element_byte_stride: 8,
        keys: Vec::new(),
        indexed_by: Vec::new(),
    }];
    let mut p_odo = base_prog.clone();
    p_odo.data_items = vec![odo_item];
    assert!(build_program_storage_plan(&p_odo, "P", &sm, &interner).is_ok());

    let mut based_ok = item0;
    based_ok.section = HirStorageSection::BasedStorage;
    let mut p_based = base_prog;
    p_based.data_items = vec![based_ok];
    assert!(build_program_storage_plan(&p_based, "P", &sm, &interner).is_ok());
    Ok(())
}

/// Exercises `LevelNumber::Renames66` inside a `Group` during `build_program_storage_plan` and `build_serialized_init_plan`.
#[test]
fn storage_init_renames_in_group_coverage() -> Result<()> {
    use coblang_common::{DataItemId, LevelNumber, ScopeId};
    use coblang_hir::{HirDataItem, HirStorageSection, HirType};
    use coblang_mlir::storage_init::{build_program_storage_plan, build_serialized_init_plan};

    let (sm, interner, mut p_ren) = empty_hir_template()?;
    let sp = p_ren.span;
    let grp_root = HirDataItem {
        id: DataItemId::new(0),
        scope: ScopeId::new(0),
        name: None,
        level: LevelNumber::parse(1)?,
        parent: None,
        children: Vec::new(),
        section: HirStorageSection::WorkingStorage,
        ty: HirType::Group {
            static_byte_len: 8,
            odo_presence: coblang_hir::DynamicOdoPresence::StaticSize,
        },
        byte_offset: ByteOffset::ZERO,
        total_byte_size: 8,
        dimensions: Vec::new(),
        redefines: None,
        initial_value: None,
        span: sp,
    };
    let ren_in_grp = HirDataItem {
        id: DataItemId::new(1),
        level: LevelNumber::Renames66,
        parent: Some(DataItemId::new(0)),
        ty: HirType::Pointer,
        ..grp_root.clone()
    };
    p_ren.data_items = vec![grp_root, ren_in_grp];
    let _ = build_program_storage_plan(&p_ren, "P", &sm, &interner)?;
    let grp_pl = coblang_hir::HirPlace {
        item: DataItemId::new(0),
        static_offset: ByteOffset::ZERO,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: HirType::Pointer,
        check_mask: coblang_hir::ExceptionCheckMask::default(),
        span: sp,
    };
    let _ = build_serialized_init_plan(
        &grp_pl,
        coblang_hir::HirInitFiller::IncludeFiller,
        coblang_hir::HirInitValueMode::DefaultCategory,
        &p_ren,
        &interner,
    )?;
    Ok(())
}

/// Exercises remaining unimplemented `Move`, `AssignArith`, and `HirCondition` arms in `lower_from_hir.rs`.
#[test]
fn synthetic_hir_cond_and_expr_errors_coverage() -> Result<()> {
    use coblang_common::{DataItemId, LevelNumber, ScopeId};
    use coblang_hir::{
        ExceptionCheckMask, HirCondition, HirDataItem, HirExpr, HirPlace, HirStmt,
        HirStorageSection, HirType, JustifiedSpec,
    };

    let ctx = CobolMlirContext::new();
    let (sm, mut interner, base_prog) = empty_hir_template()?;
    let ok_sp = base_prog.span;
    let (trim_sym, cls_sym, num_sym) = (
        interner.intern("TRIM")?,
        interner.intern("CUSTOM-CLS")?,
        interner.intern("NUMERIC")?,
    );
    let hex_sym = interner.intern("HEX-OF")?;
    let a_ty = HirType::Alphanumeric {
        byte_len: 4,
        justified: JustifiedSpec::Right,
    };
    let a_item = HirDataItem {
        id: DataItemId::new(0),
        scope: ScopeId::new(0),
        name: None,
        level: LevelNumber::parse(1)?,
        parent: None,
        children: Vec::new(),
        section: HirStorageSection::WorkingStorage,
        ty: a_ty.clone(),
        byte_offset: ByteOffset::ZERO,
        total_byte_size: 4,
        dimensions: Vec::new(),
        redefines: None,
        initial_value: None,
        span: ok_sp,
    };
    let a_pl = HirPlace {
        item: DataItemId::new(0),
        static_offset: ByteOffset::ZERO,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: a_ty,
        check_mask: ExceptionCheckMask::default(),
        span: ok_sp,
    };
    let mk_if = |cond: HirCondition| HirStmt::If {
        cond,
        then_body: Vec::new(),
        else_body: Vec::new(),
        span: ok_sp,
    };
    let call_trim = HirExpr::CallFunction {
        name: trim_sym,
        args: Vec::new(),
        span: ok_sp,
    };
    let call_hex = HirExpr::CallFunction {
        name: hex_sym,
        args: Vec::new(),
        span: ok_sp,
    };
    for st in [
        HirStmt::Move {
            src: HirExpr::AddressOf(a_pl.clone()),
            dst: a_pl.clone(),
            span: ok_sp,
        },
        HirStmt::Move {
            src: call_trim,
            dst: a_pl.clone(),
            span: ok_sp,
        },
        HirStmt::Display {
            operands: vec![call_hex],
            device: coblang_hir::HirDisplayDevice::Stdout,
            advancing: coblang_hir::HirAdvancing::AdvanceLine,
            span: ok_sp,
        },
        mk_if(HirCondition::Omitted {
            item: DataItemId::new(999),
        }),
        mk_if(HirCondition::ClassCheck {
            expr: HirExpr::Place(a_pl.clone()),
            class_name: cls_sym,
        }),
        mk_if(HirCondition::ClassCheck {
            expr: HirExpr::AddressOf(a_pl),
            class_name: num_sym,
        }),
    ] {
        let mut p = base_prog.clone();
        p.data_items = vec![a_item.clone()];
        p.entry_statements = vec![st];
        assert!(lower_hir_to_cobol_mlir(&ctx, &single_prog_mod(p), &sm, &interner).is_err());
    }
    Ok(())
}

/// Exercises remaining unimplemented `Loop` (`WITH TEST AFTER`) and `AssignArith` (`CallFunction`, `Space`, `Concat`) arms.
#[test]
fn synthetic_hir_arith_and_loop_errors_coverage() -> Result<()> {
    use coblang_common::{DataItemId, LevelNumber, ScopeId};
    use coblang_hir::{
        ExceptionCheckMask, HirCondition, HirDataItem, HirExpr, HirPlace, HirRounding, HirStmt,
        HirStorageSection, HirType, JustifiedSpec,
    };

    let ctx = CobolMlirContext::new();
    let (sm, mut interner, base_prog) = empty_hir_template()?;
    let ok_sp = base_prog.span;
    let trim_sym = interner.intern("TRIM")?;
    let z_sym = interner.intern("0")?;
    let a_item = HirDataItem {
        id: DataItemId::new(0),
        scope: ScopeId::new(0),
        name: None,
        level: LevelNumber::parse(1)?,
        parent: None,
        children: Vec::new(),
        section: HirStorageSection::WorkingStorage,
        ty: HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Right,
        },
        byte_offset: ByteOffset::ZERO,
        total_byte_size: 4,
        dimensions: Vec::new(),
        redefines: None,
        initial_value: None,
        span: ok_sp,
    };
    let a_pl = HirPlace {
        item: DataItemId::new(0),
        static_offset: ByteOffset::ZERO,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: a_item.ty.clone(),
        check_mask: ExceptionCheckMask::default(),
        span: ok_sp,
    };
    let z_dec = mk_dec_expr(z_sym, coblang_common::ScaleFactor::ZERO, ok_sp);
    for expr in [
        HirExpr::CallFunction {
            name: trim_sym,
            args: Vec::new(),
            span: ok_sp,
        },
        HirExpr::Const {
            value: coblang_hir::HirConstValue::Figurative(coblang_hir::HirFigurative::Space),
            span: ok_sp,
        },
        HirExpr::Binary {
            op: coblang_hir::HirBinOp::Concat,
            lhs: Box::new(z_dec.clone()),
            rhs: Box::new(z_dec),
            span: ok_sp,
        },
    ] {
        let mut p = base_prog.clone();
        p.data_items = vec![a_item.clone()];
        p.entry_statements = vec![HirStmt::AssignArith {
            targets: vec![(a_pl.clone(), HirRounding::Truncate)],
            expr,
            on_size_error: Vec::new(),
            not_on_size_error: Vec::new(),
            span: ok_sp,
        }];
        assert!(lower_hir_to_cobol_mlir(&ctx, &single_prog_mod(p), &sm, &interner).is_err());
    }
    let empty_sym = interner.intern("")?;
    let empty_dec = mk_dec_expr(empty_sym, coblang_common::ScaleFactor::ZERO, ok_sp);
    let mut p_emp = base_prog.clone();
    p_emp.data_items = vec![a_item.clone()];
    p_emp.entry_statements = vec![HirStmt::Move {
        src: empty_dec,
        dst: a_pl.clone(),
        span: ok_sp,
    }];
    let _ = lower_hir_to_cobol_mlir(&ctx, &single_prog_mod(p_emp), &sm, &interner)?;
    let mut p_loop = base_prog;
    p_loop.data_items = vec![a_item];
    p_loop.entry_statements = vec![HirStmt::Loop {
        kind: coblang_hir::HirLoopKind::UserPerform,
        test_before: coblang_hir::HirLoopTest::After,
        until: HirCondition::BoolConst(coblang_hir::HirTruthValue::True),
        body: Vec::new(),
        cycle_step: Vec::new(),
        span: ok_sp,
    }];
    let verified = lower_hir_to_cobol_mlir(&ctx, &single_prog_mod(p_loop), &sm, &interner)?;
    let _ = lower_cobol_to_llvm_mlir(
        &ctx,
        &verified,
        OptimizationLevel::O0,
        MainEntryMode::OmitMain,
    )?;
    Ok(())
}

/// Exercises M8 MLIR & CFG lowering across `IsoStack`, `OsVsContinuation`, `ALTER`, `GO TO DEPENDING ON`,
/// `EXIT PERFORM [CYCLE]`, `EXIT SECTION`, custom `ALPHABET`/`CLASS`/`SWITCH`, and `fast_expr_has_div`.
#[test]
fn part6_m8_cfg_perform_osvs_alter_goto_and_special_names_coverage() -> Result<()> {
    let m8_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M8MLIR IS INITIAL RECURSIVE.\n",
        "ENVIRONMENT DIVISION.\n",
        "CONFIGURATION SECTION.\n",
        "OBJECT-COMPUTER. M1 PROGRAM COLLATING SEQUENCE IS REV-A.\n",
        "SPECIAL-NAMES.\n",
        "    SWITCH-1 IS SW1 ON STATUS IS SW1-ON OFF STATUS IS SW1-OFF\n",
        "    ALPHABET REV-A IS 'Z' THRU 'A'\n",
        "    CLASS MY-CLS IS 'A' THRU 'C'.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 I PIC S9(4) COMP-5 VALUE 2.\n",
        "01 J PIC S9(4) COMP-5 VALUE 1.\n",
        "01 K PIC 9(4) COMP-5 VALUE 2.\n",
        "01 P PIC 9(3) COMP-3 VALUE 7.\n",
        "01 S1 PIC X(2) VALUE 'AB'.\n",
        "01 S2 PIC X(2) VALUE 'AC'.\n",
        "PROCEDURE DIVISION.\n",
        "    IF I = 99 GO TO P-TWO END-IF.\n",
        "    IF I = 1 NEXT SENTENCE ELSE NEXT SENTENCE.\n",
        "    DISPLAY 'A' NEXT SENTENCE.\n",
        "    PERFORM 2 TIMES EXIT PERFORM END-PERFORM.\n",
        "    MOVE P TO RETURN-CODE.\n",
        "    SET SW1 TO ON. SET SW1 TO OFF.\n",
        "    IF (SW1-ON OR SW1-OFF) AND S1 < S2 AND S1 IS MY-CLS\n",
        "        DISPLAY 'OK'\n",
        "    END-IF.\n",
        "    IF (I / 2) = 1 AND 1 = (I / 2) AND ((I / 2) + 1) = 2 AND (1 + (I / 2)) = 2 AND -(I / 2) = -1\n",
        "        PERFORM S-WORK THRU P-TWO\n",
        "    END-IF.\n",
        "    PERFORM VARYING I FROM 1 BY 1 UNTIL I > 2\n",
        "            AFTER J FROM 1 BY 1 UNTIL J > 2\n",
        "        IF J = 1 EXIT PERFORM CYCLE END-IF\n",
        "        IF I = 2 EXIT PERFORM END-IF\n",
        "    END-PERFORM.\n",
        "    STOP RUN.\n",
        "S-WORK SECTION.\n",
        "P-UNALTERED-BARE.\n",
        "    GO TO.\n",
        "P-ALT.\n",
        "    MOVE 2 TO K.\n",
        "    GO TO P-TWO.\n",
        "P-ONE.\n",
        "    ALTER P-ALT TO PROCEED TO P-TWO.\n",
        "    GO TO P-ONE P-TWO DEPENDING ON K.\n",
        "P-TWO.\n",
        "    EXIT SECTION.\n",
    );

    let ctx = CobolMlirContext::new();
    for osvs in [false, true] {
        let mut dialect = DialectConfig::for_dialect(Dialect::Gnu);
        if osvs {
            dialect.apply_conf_key_value("perform-osvs", "yes", "cli", 1)?;
        }
        let resolver = InMemoryCopybookResolver::new();
        let pp_cfg = PreprocessConfig {
            initial_format: SourceFormat::Free,
            dialect_config: &dialect,
            debug_policy: DebugLinePolicy::StripDebugLines,
            initial_defines: &[],
            copybook_resolver: &resolver,
        };
        let mut sm = SourceMap::new();
        let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
        let unit = preprocess_source_file(
            "m8.cob",
            std::path::Path::new("."),
            m8_src,
            &pp_cfg,
            &mut sm,
            &mut sink,
        )?;
        let mut interner = SymbolInterner::new();
        let grp = parse_preprocessed_unit(&unit, &dialect, &mut interner, &mut sink)?;
        let hir = lower_compilation_group(&grp, &dialect, &mut interner, &mut sink)?;
        let verified = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner)?;
        let lowered = lower_cobol_to_llvm_mlir(
            &ctx,
            &verified,
            OptimizationLevel::O0,
            MainEntryMode::EmitMain,
        )?;
        assert!(lowered.to_mlir_text().contains("__cobol_dispatch_"));
    }

    synthetic_m8_cfg_defensive_branches(&ctx)?;
    Ok(())
}

/// Exercises defensive guard branches in `cfg_perform.rs` on synthetic `cobol.*` operations.
///
/// # Arguments
///
/// * `ctx` - Owning [`CobolMlirContext`].
fn synthetic_m8_cfg_defensive_branches(ctx: &CobolMlirContext) -> Result<()> {
    use melior::ir::r#type::IntegerType;

    let loc = ctx.unknown_location();
    let i32_ty = IntegerType::new(ctx.raw(), 32).into();
    let m = Module::new(loc);
    for (sym, sz) in [("__rc_top", 4_u32), ("__cobol_pfm_stk_TOP", 1028)] {
        let _ = m.body().append_operation(
            CobolStorageOp {
                sym_name: sym,
                storage_kind: StorageKind::Working,
                byte_size: sz,
                alignment: 4,
                init_bytes: &[0, 0, 0, 0],
            }
            .build(ctx, loc)?,
        );
    }

    let top_reg = build_synthetic_top_region(ctx, loc)?;
    let _ = m.body().append_operation(
        CobolProgramOp {
            sym_name: String::from("TOP"),
            raw_c_alias: String::new(),
            rc_slot_sym: String::from("__rc_top"),
            initial_state: InitialStateMode::Retained,
            recursivity: RecursivityMode::NonRecursive,
            entry_convention: EntryConventionMode::Cobol,
            body: top_reg,
        }
        .build(ctx, loc)?,
    );

    append_synthetic_m8_helper_programs(ctx, &m, loc, i32_ty)?;

    let verified = verify_cobol_module(m)?;
    let _ = lower_cobol_to_llvm_mlir(
        ctx,
        &verified,
        OptimizationLevel::O0,
        MainEntryMode::OmitMain,
    )?;
    Ok(())
}

/// Builds the synthetic top-level `cobol.program` body exercising defensive CFG paths.
///
/// # Arguments
///
/// * `ctx` - Owning [`CobolMlirContext`].
/// * `loc` - Source location.
fn build_synthetic_top_region<'c>(
    ctx: &'c CobolMlirContext,
    loc: melior::ir::Location<'c>,
) -> Result<Region<'c>> {
    use coblang_mlir::ops::{
        CobolAlterOp, CobolFlowExitOp, CobolGotoOp, CobolLoopOp, CobolPerformRangeOp,
        CobolSentenceOp, CobolYieldOp,
    };

    let top_reg = Region::new();
    let top_blk = top_reg.append_block(Block::new(&[]));
    let _ = top_blk.append_operation(
        CobolGotoOp {
            selector: None,
            targets: &[0],
            alter_slot_sym: "",
        }
        .build(ctx, loc)?,
    );
    let _ = top_blk.append_operation(
        CobolAlterOp {
            altered_para: 99,
            new_target: 0,
        }
        .build(ctx, loc)?,
    );
    for ek in ["perform_break", "perform_cycle", "section"] {
        let _ = top_blk.append_operation(
            CobolFlowExitOp {
                exit_kind: ek,
                section_end: 0,
            }
            .build(ctx, loc)?,
        );
    }
    let _ = top_blk.append_operation(
        CobolLoopOp {
            loop_kind: "after_tier",
            test_pos: "before",
            cond_region: Region::new(),
            body_region: Region::new(),
            step_region: Region::new(),
        }
        .build(ctx, loc)?,
    );
    let cr = Region::new();
    let cb = cr.append_block(Block::new(&[]));
    let _ = cb.append_operation(CobolYieldOp { value: None }.build(ctx, loc)?);
    let _ = top_blk.append_operation(
        CobolLoopOp {
            loop_kind: "user_perform",
            test_pos: "before",
            cond_region: cr,
            body_region: Region::new(),
            step_region: Region::new(),
        }
        .build(ctx, loc)?,
    );
    let _ = top_blk.append_operation(
        CobolSentenceOp {
            body_region: Region::new(),
        }
        .build(ctx, loc)?,
    );
    let _ = top_blk.append_operation(
        CobolPerformRangeOp {
            start_proc: u32::MAX,
            end_proc: 0,
        }
        .build(ctx, loc)?,
    );
    let _ = top_blk.append_operation(
        CobolGobackOp {
            mode: GobackMode::ExitProgram,
        }
        .build(ctx, loc)?,
    );
    Ok(top_reg)
}

/// Appends synthetic paragraph helper `cobol.program`s (`__cobol_proc_TOP_0`, `__cobol_proc_nounderscore`, `__cobol_proc_TOP_bad`).
///
/// # Arguments
///
/// * `ctx` - Owning [`CobolMlirContext`].
/// * `m` - Target module.
/// * `loc` - Source location.
/// * `i32_ty` - 32-bit integer type.
fn append_synthetic_m8_helper_programs<'c>(
    ctx: &'c CobolMlirContext,
    m: &Module<'c>,
    loc: melior::ir::Location<'c>,
    i32_ty: melior::ir::Type<'c>,
) -> Result<()> {
    use coblang_mlir::ops::CobolGotoOp;
    use melior::dialect::arith;
    use melior::ir::attribute::IntegerAttribute;

    let p0_reg = Region::new();
    let p0_blk = p0_reg.append_block(Block::new(&[]));
    let c0_op = p0_blk.append_operation(arith::constant(
        ctx.raw(),
        IntegerAttribute::new(i32_ty, 1).into(),
        loc,
    ));
    if let Ok(sel_v) = c0_op.result(0) {
        let _ = p0_blk.append_operation(
            CobolGotoOp {
                selector: Some(sel_v.into()),
                targets: &[],
                alter_slot_sym: "",
            }
            .build(ctx, loc)?,
        );
    }
    let _ = p0_blk.append_operation(
        CobolGotoOp {
            selector: None,
            targets: &[],
            alter_slot_sym: "",
        }
        .build(ctx, loc)?,
    );
    let _ = p0_blk.append_operation(
        CobolGobackOp {
            mode: GobackMode::ParaFallthrough,
        }
        .build(ctx, loc)?,
    );
    let _ = m.body().append_operation(
        CobolProgramOp {
            sym_name: String::from("__cobol_proc_TOP_0"),
            raw_c_alias: String::new(),
            rc_slot_sym: String::from("__rc_top"),
            initial_state: InitialStateMode::Retained,
            recursivity: RecursivityMode::NonRecursive,
            entry_convention: EntryConventionMode::Cobol,
            body: p0_reg,
        }
        .build(ctx, loc)?,
    );

    for bad_sym in ["__cobol_proc_nounderscore", "__cobol_proc_TOP_bad"] {
        let br = Region::new();
        let bb = br.append_block(Block::new(&[]));
        let _ = bb.append_operation(
            CobolGobackOp {
                mode: GobackMode::ExitProgram,
            }
            .build(ctx, loc)?,
        );
        let _ = m.body().append_operation(
            CobolProgramOp {
                sym_name: String::from(bad_sym),
                raw_c_alias: String::new(),
                rc_slot_sym: String::from("__rc_top"),
                initial_state: InitialStateMode::Retained,
                recursivity: RecursivityMode::NonRecursive,
                entry_convention: EntryConventionMode::Cobol,
                body: br,
            }
            .build(ctx, loc)?,
        );
    }
    Ok(())
}

/// Exercises M9 `FixedMaxOffsets` ODO groups, `INITIALIZE`, `EC-BOUND-*` runtime checks, and `SORT` lowering to LLVM MLIR.
#[test]
fn m9_fixed_odo_bounds_and_sort_table_mlir_coverage() -> Result<()> {
    use coblang_diag::BoundCheckToggle;
    use std::path::Path;

    let ctx = CobolMlirContext::new();
    let fixed_odo_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M9-FIXED.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 G-INIT.\n",
        "   05 N PIC 9 VALUE 2.\n",
        "   05 T1 PIC X(2) OCCURS 1 TO 3 TIMES DEPENDING ON N.\n",
        "01 G-FIRST-ODO.\n",
        "   05 T-FIRST PIC X(2) OCCURS 1 TO 3 TIMES DEPENDING ON N.\n",
        "01 G-NESTED.\n",
        "   05 OUTER-ODO OCCURS 1 TO 2 TIMES DEPENDING ON N.\n",
        "      10 INNER-ODO PIC X OCCURS 1 TO 2 TIMES DEPENDING ON N.\n",
        "01 FIXED-TBL PIC X(2) OCCURS 3 TIMES.\n",
        "01 OUT-BUF PIC X(10).\n",
        "PROCEDURE DIVISION.\n",
        "    INITIALIZE G-INIT.\n",
        "    MOVE G-INIT TO OUT-BUF.\n",
        "    MOVE G-FIRST-ODO(1:2) TO OUT-BUF.\n",
        "    MOVE G-NESTED(1:) TO OUT-BUF.\n",
        "    MOVE T1(1) TO FIXED-TBL(2).\n",
        ">>TURN EC-BOUND CHECKING ON\n",
        "    MOVE OUT-BUF(2:3) TO OUT-BUF.\n",
        "    SORT T1.\n",
        "    SORT FIXED-TBL.\n",
        "    STOP RUN.\n",
    );
    let mut d_fixed = DialectConfig::for_dialect(Dialect::Gnu);
    d_fixed.bounds_checks.subscript = BoundCheckToggle::EnabledFatal;
    d_fixed.bounds_checks.odo = BoundCheckToggle::EnabledFatal;
    d_fixed.bounds_checks.refmod = BoundCheckToggle::EnabledFatal;
    let mem = InMemoryCopybookResolver::new();
    let pp_fixed = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &d_fixed,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm1 = SourceMap::new();
    let mut sink1 = DiagnosticSink::new(WarningPolicy::new(), 50);
    let u1 = preprocess_source_file(
        "m9_fixed.cob",
        Path::new("."),
        fixed_odo_src,
        &pp_fixed,
        &mut sm1,
        &mut sink1,
    )?;
    let mut int1 = SymbolInterner::new();
    let grp1 = parse_preprocessed_unit(&u1, &d_fixed, &mut int1, &mut sink1)?;
    let hir1 = lower_compilation_group(&grp1, &d_fixed, &mut int1, &mut sink1)?;
    let cob1 = lower_hir_to_cobol_mlir(&ctx, &hir1, &sm1, &int1)?;
    let llvm1 =
        lower_cobol_to_llvm_mlir(&ctx, &cob1, OptimizationLevel::O0, MainEntryMode::OmitMain)?;
    let llvm1_txt = llvm1.to_mlir_text();
    assert!(llvm1_txt.contains("coblang_rt_check_odo"));
    assert!(llvm1_txt.contains("coblang_rt_check_sub"));
    assert!(llvm1_txt.contains("coblang_rt_check_refmod"));
    assert!(llvm1_txt.contains("coblang_rt_sort_table"));
    Ok(())
}

/// Exercises M9 `ODOSLIDE` (`SlideVariablyLocated`) dynamic sibling offsets and outer ODO group subscripting.
#[test]
fn m9_odoslide_mlir_coverage() -> Result<()> {
    use coblang_diag::OdoSlidePolicy;
    use std::path::Path;

    let ctx = CobolMlirContext::new();
    let slide_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M9-SLIDE.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "78 K78 VALUE 1.\n",
        "01 N1 PIC 9 VALUE 2.\n",
        "01 N2 PIC 9 VALUE 1.\n",
        "01 G-SLIDE.\n",
        "   05 HDR PIC X.\n",
        "      88 HDR-A VALUE 'A'.\n",
        "   05 HDR-RED REDEFINES HDR PIC X.\n",
        "   05 S-SYNC PIC 9(4) COMP SYNCHRONIZED.\n",
        "   05 ODO-A PIC X OCCURS 1 TO 3 TIMES DEPENDING ON N1.\n",
        "   05 FIXED-MID PIC X OCCURS 2 TIMES.\n",
        "   05 ODO-B PIC X OCCURS 1 TO 2 TIMES DEPENDING ON N2.\n",
        "   05 AFTER-GRP.\n",
        "      10 AFTER-LEAF PIC X(2).\n",
        "66 REN-HDR RENAMES HDR.\n",
        "01 G-OUTER-SLIDE.\n",
        "   05 ROW-SLIDE OCCURS 2 TIMES.\n",
        "      10 ROW-V PIC X OCCURS 1 TO 2 TIMES DEPENDING ON N1.\n",
        "      10 ROW-TAIL PIC X.\n",
        "01 PLAIN-TBL PIC X OCCURS 2 TIMES.\n",
        "PROCEDURE DIVISION.\n",
        "    MOVE 'XY' TO AFTER-LEAF.\n",
        "    DISPLAY G-SLIDE.\n",
        "    MOVE 'Z' TO ROW-TAIL(2).\n",
        "    MOVE 'Q' TO PLAIN-TBL(1).\n",
        "    STOP RUN.\n",
    );
    let mut d_slide = DialectConfig::for_dialect(Dialect::Gnu);
    d_slide.odo_slide = OdoSlidePolicy::SlideVariablyLocated;
    let mem = InMemoryCopybookResolver::new();
    let pp_slide = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &d_slide,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &mem,
    };
    let mut sm2 = SourceMap::new();
    let mut sink2 = DiagnosticSink::new(WarningPolicy::new(), 50);
    let u2 = preprocess_source_file(
        "m9_slide.cob",
        Path::new("."),
        slide_src,
        &pp_slide,
        &mut sm2,
        &mut sink2,
    )?;
    let mut int2 = SymbolInterner::new();
    let grp2 = parse_preprocessed_unit(&u2, &d_slide, &mut int2, &mut sink2)?;
    let hir2 = lower_compilation_group(&grp2, &d_slide, &mut int2, &mut sink2)?;
    let cob2 = lower_hir_to_cobol_mlir(&ctx, &hir2, &sm2, &int2)?;
    let _ = lower_cobol_to_llvm_mlir(&ctx, &cob2, OptimizationLevel::O0, MainEntryMode::OmitMain)?;
    Ok(())
}

/// Exercises `build_serialized_table_sort_plan` with short `collating_ranks` and scalar `SortTable` lowering.
#[test]
fn m9_synthetic_sort_plan_mlir_coverage() -> Result<()> {
    use coblang_common::DataItemId;
    use coblang_hir::{
        ExceptionCheckMask, HirPlace, HirSortDirection, HirSortTableKey, HirSortTableSpec, HirStmt,
        HirType, JustifiedSpec,
    };
    use coblang_mlir::storage_init::build_serialized_table_sort_plan;

    let ctx = CobolMlirContext::new();
    let (sm, int, hir_res) = compile_to_hir(
        "IDENTIFICATION DIVISION.\nPROGRAM-ID. M9-SP.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 X PIC X VALUE 'A'.\nPROCEDURE DIVISION.\n    STOP RUN.\n",
        Dialect::Gnu,
    )?;
    let sp = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::new(1),
        ExpansionId::ROOT,
    );
    let alpha1 = HirType::Alphanumeric {
        byte_len: 1,
        justified: JustifiedSpec::Left,
    };
    let mut syn_hir = hir_res.0;
    if let Some(prog) = syn_hir.programs.first_mut() {
        let short_spec = HirSortTableSpec {
            table: HirPlace {
                item: DataItemId::new(0),
                static_offset: ByteOffset::ZERO,
                subscripts: Vec::new(),
                refmod: None,
                effective_ty: alpha1.clone(),
                check_mask: ExceptionCheckMask::NONE,
                span: sp,
            },
            count_odo: Some(DataItemId::new(999)),
            max_count: 2,
            stride: 1,
            keys: vec![HirSortTableKey {
                direction: HirSortDirection::Ascending,
                rel_offset: 0,
                byte_len: 1,
                ty: alpha1,
            }],
            collating_ranks: Some(vec![1, 0]),
            span: sp,
        };
        assert!(build_serialized_table_sort_plan(&short_spec, prog).len() > 256);
        prog.entry_statements
            .push(HirStmt::SortTable(Box::new(short_spec)));
    }
    let cob = lower_hir_to_cobol_mlir(&ctx, &ResolvedHirModule(syn_hir), &sm, &int)?;
    let _ = lower_cobol_to_llvm_mlir(&ctx, &cob, OptimizationLevel::O0, MainEntryMode::OmitMain)?;
    Ok(())
}

/// Exercises synthetic M9 HIR edge branches (unmapped spans, childless ODO, unbounded tables, broken `SlideOffsets` parent).
#[test]
fn m9_synthetic_hir_edges_mlir_coverage() -> Result<()> {
    use coblang_common::{DataItemId, ScaleFactor};
    use coblang_hir::{
        DynamicOdoPresence, ExceptionCheckMask, HirAdvancing, HirDisplayDevice, HirOccursDim,
        HirOdoSlideMode, HirPlace, HirRefMod, HirStmt, HirStorageSection, HirType, JustifiedSpec,
    };

    let ctx = CobolMlirContext::new();
    let (sm2, mut int2, hir_res) = compile_to_hir(
        "IDENTIFICATION DIVISION.\nPROGRAM-ID. M9-SYN.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 X PIC X VALUE 'A'.\nPROCEDURE DIVISION.\n    STOP RUN.\n",
        Dialect::Gnu,
    )?;
    let unmapped_span = SourceSpan::new(
        FileId::new(0),
        ByteOffset::ZERO,
        ByteLength::new(1),
        ExpansionId::ROOT,
    );
    let alpha1 = HirType::Alphanumeric {
        byte_len: 1,
        justified: JustifiedSpec::Left,
    };
    let mut syn_hir = hir_res.0;
    if let Some(prog) = syn_hir.programs.first_mut()
        && let Some(template) = prog.data_items.first().cloned()
    {
        prog.odo_slide = HirOdoSlideMode::SlideOffsets;
        let one_expr = coblang_hir::HirExpr::Const {
            value: coblang_hir::HirConstValue::Decimal {
                text: int2.intern("1")?,
                scale: ScaleFactor::ZERO,
                sign: coblang_hir::HirDecimalSign::UnsignedOrEmbedded,
            },
            span: unmapped_span,
        };
        let empty_odo_id = DataItemId::new(u32::try_from(prog.data_items.len()).unwrap_or(0));
        let mut empty_odo = template.clone();
        empty_odo.id = empty_odo_id;
        empty_odo.section = HirStorageSection::WorkingStorage;
        empty_odo.ty = HirType::Group {
            static_byte_len: 4,
            odo_presence: DynamicOdoPresence::HasDynamicOdo,
        };
        prog.data_items.push(empty_odo);

        let unb_id = DataItemId::new(u32::try_from(prog.data_items.len()).unwrap_or(0));
        let mut unb_item = template;
        unb_item.id = unb_id;
        unb_item.section = HirStorageSection::WorkingStorage;
        unb_item.parent = None;
        unb_item.dimensions = vec![HirOccursDim {
            min: 1,
            max: u32::MAX,
            depending_on: None,
            indexed_by: Vec::new(),
            keys: Vec::new(),
            element_byte_stride: 1,
        }];
        prog.data_items.push(unb_item);

        prog.entry_statements.push(HirStmt::Display {
            operands: vec![
                coblang_hir::HirExpr::Place(HirPlace {
                    item: empty_odo_id,
                    static_offset: ByteOffset::ZERO,
                    subscripts: Vec::new(),
                    refmod: Some(HirRefMod {
                        offset: Box::new(one_expr.clone()),
                        length: None,
                    }),
                    effective_ty: alpha1.clone(),
                    check_mask: ExceptionCheckMask::BOUND_REF_MOD,
                    span: unmapped_span,
                }),
                coblang_hir::HirExpr::Place(HirPlace {
                    item: unb_id,
                    static_offset: ByteOffset::ZERO,
                    subscripts: vec![(one_expr.clone(), 1), (one_expr, 1)],
                    refmod: None,
                    effective_ty: alpha1,
                    check_mask: ExceptionCheckMask::BOUND_SUBSCRIPT,
                    span: unmapped_span,
                }),
            ],
            device: HirDisplayDevice::Stdout,
            advancing: HirAdvancing::AdvanceLine,
            span: unmapped_span,
        });
    }
    let cob3 = lower_hir_to_cobol_mlir(&ctx, &ResolvedHirModule(syn_hir), &sm2, &int2)?;
    let _ = lower_cobol_to_llvm_mlir(&ctx, &cob3, OptimizationLevel::O0, MainEntryMode::OmitMain)?;
    Ok(())
}

/// Exercises M10 `DisplayEnv`, `Accept`, `StringVerb`, `Unstring`, and `Inspect` MLIR/LLVM lowering across all standard forms.
#[test]
fn m10_mlir_and_llvm_lowering_full_coverage() -> Result<()> {
    let m10_src = concat!(
        "IDENTIFICATION DIVISION.\u{000a}",
        "PROGRAM-ID. M10M.\u{000a}",
        "DATA DIVISION.\u{000a}",
        "WORKING-STORAGE SECTION.\u{000a}",
        "01 S PIC X(12) VALUE 'AABBCC'.\u{000a}",
        "01 D PIC X(12).\u{000a}",
        "01 DM PIC X(2).\u{000a}",
        "01 P PIC 9(2) VALUE 1.\u{000a}",
        "01 C PIC 9(2) VALUE 0.\u{000a}",
        "PROCEDURE DIVISION.\u{000a}",
        "P1.\u{000a}",
        "    DISPLAY 'K' UPON ENVIRONMENT-NAME.\u{000a}",
        "    DISPLAY 'V' UPON ENVIRONMENT-VALUE.\u{000a}",
        "    DISPLAY '1' UPON ARGUMENT-NUMBER.\u{000a}",
        "    DISPLAY 'A' & 'B' UPON COMMAND-LINE.\u{000a}",
        "    ACCEPT OMITTED.\u{000a}",
        "    ACCEPT S FROM DATE.\u{000a}",
        "    ACCEPT S FROM DATE YYYYMMDD.\u{000a}",
        "    ACCEPT S FROM DAY.\u{000a}",
        "    ACCEPT S FROM DAY YYYYDDD.\u{000a}",
        "    ACCEPT S FROM DAY-OF-WEEK.\u{000a}",
        "    ACCEPT S FROM TIME.\u{000a}",
        "    ACCEPT S FROM MICROSECOND-TIME.\u{000a}",
        "    ACCEPT S FROM ARGUMENT-NUMBER.\u{000a}",
        "    ACCEPT S FROM ARGUMENT-VALUE ON EXCEPTION DISPLAY 'E' END-ACCEPT.\u{000a}",
        "    ACCEPT S FROM COMMAND-LINE NOT ON EXCEPTION DISPLAY 'O' END-ACCEPT.\u{000a}",
        "    ACCEPT S FROM ENVIRONMENT-NAME.\u{000a}",
        "    ACCEPT S FROM ENVIRONMENT-VALUE.\u{000a}",
        "    STRING S DELIMITED BY SIZE 'X' DELIMITED BY SPACES FUNCTION TRIM(S) DELIMITED BY ZERO\u{000a}",
        "        'Y' DELIMITED BY HIGH-VALUE 'Z' DELIMITED BY LOW-VALUE 'Q' DELIMITED BY QUOTE\u{000a}",
        "        'N' DELIMITED BY NULL 'M' DELIMITED BY ALL '*' 'W' DELIMITED BY ALL 'AB' 'P' DELIMITED BY '|'\u{000a}",
        "        INTO D.\u{000a}",
        "    STRING 'A' DELIMITED BY SIZE INTO D WITH POINTER P ON OVERFLOW DISPLAY 'OV' END-STRING.\u{000a}",
        "    STRING 'A' DELIMITED BY SIZE INTO D NOT ON OVERFLOW DISPLAY 'NO' END-STRING.\u{000a}",
        "    UNSTRING S INTO D.\u{000a}",
        "    UNSTRING S DELIMITED BY ',' OR ALL '-' INTO D DELIMITER IN DM COUNT IN C WITH POINTER P TALLYING IN C ON OVERFLOW DISPLAY 'U1' END-UNSTRING.\u{000a}",
        "    UNSTRING S INTO D NOT ON OVERFLOW DISPLAY 'U2' END-UNSTRING.\u{000a}",
        "    INSPECT S TALLYING C FOR CHARACTERS AFTER 'A' BEFORE 'Z' C FOR ALL 'A' C FOR LEADING 'B' C FOR FIRST 'C' C FOR TRAILING 'D'.\u{000a}",
        "    INSPECT S REPLACING CHARACTERS BY SPACE ALL 'A' BY ZERO LEADING 'B' BY '*' FIRST 'C' BY ALL '+' TRAILING 'D' BY ALL 'XY'.\u{000a}",
        "    INSPECT S CONVERTING 'abc' TO SPACES AFTER 'A' BEFORE 'Z'.\u{000a}",
        "    ALTER P2 TO PROCEED TO P3.\u{000a}",
        "P2.\u{000a}",
        "    GO TO P3.\u{000a}",
        "P3.\u{000a}",
        "    STOP RUN.\u{000a}",
    );
    let ctx = CobolMlirContext::new();
    let (sm, interner, hir) = compile_to_hir(m10_src, Dialect::Gnu)?;
    let cob_mod = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner)?;
    let llvm_mod = lower_cobol_to_llvm_mlir(
        &ctx,
        &cob_mod,
        OptimizationLevel::O0,
        MainEntryMode::EmitMain,
    )?;
    assert!(llvm_mod.to_mlir_text().contains("coblang_rt_insp_convert"));
    Ok(())
}

/// Exercises M10 synthetic ACU `TimeAcu12`, empty operand slices, `"ACCEPT-SCREEN"` tag, and unsupported operand error paths.
#[test]
fn m10_synthetic_hir_and_acu_lowering_coverage() -> Result<()> {
    let ctx = CobolMlirContext::new();
    let acu_src = "IDENTIFICATION DIVISION.\u{000a}PROGRAM-ID. PA.\u{000a}DATA DIVISION.\u{000a}WORKING-STORAGE SECTION.\u{000a}01 T PIC X(12).\u{000a}PROCEDURE DIVISION.\u{000a}ACCEPT T FROM TIME.\u{000a}STOP RUN.\u{000a}";
    let (sm_a, int_a, mut hir_a) = compile_to_hir(acu_src, Dialect::Acu)?;
    if let Some(prog) = hir_a.0.programs.first_mut() {
        let sp = prog.span;
        let t_pl = coblang_hir::HirPlace {
            item: coblang_common::DataItemId::new(0),
            static_offset: ByteOffset::ZERO,
            subscripts: Vec::new(),
            refmod: None,
            effective_ty: coblang_hir::HirType::Alphanumeric {
                byte_len: 12,
                justified: coblang_hir::JustifiedSpec::Left,
            },
            check_mask: coblang_hir::ExceptionCheckMask::NONE,
            span: sp,
        };
        prog.entry_statements.insert(
            0,
            coblang_hir::HirStmt::DisplayEnv {
                operands: Vec::new(),
                target: coblang_hir::HirEnvTargetKind::EnvironmentName,
                span: sp,
            },
        );
        prog.entry_statements.insert(
            1,
            coblang_hir::HirStmt::StringVerb(Box::new(coblang_hir::HirStringSpec {
                pieces: Vec::new(),
                into: t_pl.clone(),
                pointer: None,
                on_overflow: Vec::new(),
                not_on_overflow: Vec::new(),
                span: sp,
            })),
        );
        prog.entry_statements.insert(
            2,
            coblang_hir::HirStmt::Unstring(Box::new(coblang_hir::HirUnstringSpec {
                source: t_pl,
                delimiters: Vec::new(),
                targets: Vec::new(),
                pointer: None,
                tallying: None,
                on_overflow: Vec::new(),
                not_on_overflow: Vec::new(),
                span: sp,
            })),
        );
    }
    let cob_a = lower_hir_to_cobol_mlir(&ctx, &hir_a, &sm_a, &int_a)?;
    let _ = lower_cobol_to_llvm_mlir(&ctx, &cob_a, OptimizationLevel::O0, MainEntryMode::OmitMain)?;

    let scr_src = "IDENTIFICATION DIVISION.\u{000a}PROGRAM-ID. PS.\u{000a}DATA DIVISION.\u{000a}WORKING-STORAGE SECTION.\u{000a}01 T PIC X(4).\u{000a}PROCEDURE DIVISION.\u{000a}ACCEPT T FROM CRT.\u{000a}STOP RUN.\u{000a}";
    let (sm_s, int_s, hir_s) = compile_to_hir(scr_src, Dialect::Gnu)?;
    let cob_s = lower_hir_to_cobol_mlir(&ctx, &hir_s, &sm_s, &int_s)?;
    let _ = lower_cobol_to_llvm_mlir(&ctx, &cob_s, OptimizationLevel::O0, MainEntryMode::OmitMain)?;

    if let Some(prog) = hir_a.0.programs.first_mut() {
        let sp = prog.span;
        let t_pl = coblang_hir::HirPlace {
            item: coblang_common::DataItemId::new(0),
            static_offset: ByteOffset::ZERO,
            subscripts: Vec::new(),
            refmod: None,
            effective_ty: coblang_hir::HirType::Alphanumeric {
                byte_len: 12,
                justified: coblang_hir::JustifiedSpec::Left,
            },
            check_mask: coblang_hir::ExceptionCheckMask::NONE,
            span: sp,
        };
        prog.entry_statements.insert(
            0,
            coblang_hir::HirStmt::StringVerb(Box::new(coblang_hir::HirStringSpec {
                pieces: vec![(
                    coblang_hir::HirExpr::AddressOf(t_pl.clone()),
                    coblang_hir::HirStringDelim::Size,
                )],
                into: t_pl,
                pointer: None,
                on_overflow: Vec::new(),
                not_on_overflow: Vec::new(),
                span: sp,
            })),
        );
    }
    assert!(matches!(
        lower_hir_to_cobol_mlir(&ctx, &hir_a, &sm_a, &int_a),
        Err(CoblangError::UnimplementedFeature { .. })
    ));
    Ok(())
}

/// Exercises M11 `POINTER`, `BASED`, `ADDRESS OF`, `SET`, `ALLOCATE`, `FREE`, `GLOBAL`, `EXTERNAL`,
/// `defaultbyte`, and `DeferredBss` `BASED` records across `coblang-mlir`.
#[test]
fn m11_mlir_and_llvm_lowering_full_coverage() -> Result<()> {
    let mut cfg = DialectConfig::for_dialect(Dialect::Gnu);
    cfg.apply_conf_key_value("odoslide", "yes", "<test>", 1)?;
    cfg.apply_conf_key_value("defaultbyte", "0x58", "<test>", 1)?;

    let m11_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M11-MAIN.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 G-SHARED PIC X(4) IS GLOBAL VALUE 'GLOB'.\n",
        "01 EXT-REC IS EXTERNAL.\n",
        "   05 EXT-A PIC X(2).\n",
        "   05 EXT-B PIC X(2).\n",
        "01 ODO-CNT PIC 99 COMP-5 VALUE 2.\n",
        "01 ODO-GRP.\n",
        "   05 ODO-ARR PIC X(2) OCCURS 1 TO 4 DEPENDING ON ODO-CNT INDEXED BY ODO-IDX.\n",
        "   05 ODO-AFTER PIC X(4).\n",
        "01 WS-NAME PIC X(8) VALUE 'SUB1    '.\n",
        "01 PTR-1 USAGE POINTER.\n",
        "01 PTR-2 USAGE POINTER.\n",
        "01 PROG-P USAGE PROGRAM-POINTER.\n",
        "01 B-SMALL BASED.\n",
        "   05 B-PTR-0 USAGE POINTER.\n",
        "   05 B-TXT PIC X(8) VALUE 'HELLO!  '.\n",
        "   05 B-PTR-RED REDEFINES B-TXT USAGE POINTER.\n",
        "   05 B-PTR-8 USAGE POINTER.\n",
        "01 B-HUGE BASED.\n",
        "   05 B-HUGE-HDR PIC X(4) VALUE 'BIG!'.\n",
        "   05 B-HUGE-PAD PIC X(100) OCCURS 700.\n",
        "PROCEDURE DIVISION.\n",
        "    DISPLAY EXT-A EXT-B.\n",
        "    DISPLAY ADDRESS OF WS-NAME, ADDRESS OF RETURN-CODE, ADDRESS OF ODO-GRP.\n",
        "    DISPLAY ADDRESS OF ODO-ARR(1), ADDRESS OF ODO-AFTER, ADDRESS OF B-TXT(1:2).\n",
        "    IF PTR-1 = NULL AND NULL = NULL AND ADDRESS OF WS-NAME = NULL AND PTR-1 NOT = PTR-2 CONTINUE END-IF.\n",
        "    IF PTR-1 < ADDRESS OF WS-NAME OR PTR-1 <= ZERO CONTINUE END-IF.\n",
        "    IF PTR-1 > LOW-VALUE OR PTR-1 >= 0 OR 1 = PTR-1 CONTINUE END-IF.\n",
        "    SET PTR-1 TO NULL. SET PTR-2 TO PTR-1. SET PTR-1 TO ADDRESS OF WS-NAME.\n",
        "    SET ADDRESS OF B-SMALL TO PTR-1. SET PROG-P TO ENTRY 'SUB1'.\n",
        "    SET PROG-P TO ENTRY WS-NAME. SET PROG-P TO ENTRY FUNCTION TRIM(WS-NAME).\n",
        "    SET PTR-1 UP BY 2. SET ADDRESS OF B-SMALL DOWN BY 2.\n",
        "    ALLOCATE 16 CHARACTERS RETURNING PTR-1.\n",
        "    ALLOCATE 16 CHARACTERS INITIALIZED RETURNING PTR-1.\n",
        "    ALLOCATE 16 CHARACTERS INITIALIZED TO ZERO RETURNING PTR-1.\n",
        "    ALLOCATE 16 CHARACTERS INITIALIZED TO SPACE RETURNING PTR-1.\n",
        "    ALLOCATE 16 CHARACTERS INITIALIZED TO HIGH-VALUE RETURNING PTR-1.\n",
        "    ALLOCATE 16 CHARACTERS INITIALIZED TO LOW-VALUE RETURNING PTR-1.\n",
        "    ALLOCATE 16 CHARACTERS INITIALIZED TO NULL RETURNING PTR-1.\n",
        "    ALLOCATE 16 CHARACTERS INITIALIZED TO QUOTE RETURNING PTR-1.\n",
        "    ALLOCATE 16 CHARACTERS INITIALIZED TO ALL 'XY' RETURNING PTR-1.\n",
        "    ALLOCATE 16 CHARACTERS INITIALIZED TO WS-NAME RETURNING PTR-1.\n",
        "    ALLOCATE B-SMALL. ALLOCATE B-SMALL INITIALIZED RETURNING PTR-2.\n",
        "    ALLOCATE B-HUGE INITIALIZED. DISPLAY B-SMALL B-TXT.\n",
        "    FREE PTR-1, B-SMALL, B-HUGE. STOP RUN.\n",
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M11-NEST.\n",
        "PROCEDURE DIVISION.\n",
        "    DISPLAY G-SHARED.\n",
        "    GOBACK.\n",
        "END PROGRAM M11-NEST.\n",
        "END PROGRAM M11-MAIN.\n",
    );

    let resolver = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &cfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &resolver,
    };
    let mut sm = SourceMap::new();
    let mut interner = SymbolInterner::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let pp = preprocess_source_file(
        "m11.cob",
        std::path::Path::new("."),
        m11_src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let grp = parse_preprocessed_unit(&pp, &cfg, &mut interner, &mut sink)?;
    let hir = lower_compilation_group(&grp, &cfg, &mut interner, &mut sink)?;
    assert!(!sink.has_errors(), "sema errors: {:?}", sink.diagnostics());

    let ctx = CobolMlirContext::new();
    let cob_mod = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner)?;
    let llvm_mod = lower_cobol_to_llvm_mlir(
        &ctx,
        &cob_mod,
        OptimizationLevel::O0,
        MainEntryMode::EmitMain,
    )?;
    assert!(
        llvm_mod
            .to_mlir_text()
            .contains("coblang_rt_resolve_external")
    );
    Ok(())
}

/// Exercises M11 synthetic HIR error/fallback branches (`AddressOf` in decimal expr, unresolved roots,
/// `default_byte = None` `BASED` alloc, `DeferredBss::static_image_slice`, and invalid spans).
#[test]
fn m11_synthetic_hir_and_storage_init_edge_coverage() -> Result<()> {
    use coblang_mlir::storage_init::{RootInitStrategy, build_program_storage_plan};

    let def_bss = RootInitStrategy::DeferredBss {
        byte_size: 8,
        plan_bytes: vec![1, 2],
    };
    assert_eq!(def_bss.static_image_slice(), &[] as &[u8]);

    // Default `default_byte = None` (`def_fill == 0`) with `ALLOCATE B-PLAIN.` + unnamed `FILLER BASED` + invalid span.
    let plain_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M11-PLAIN.\n",
        "DATA DIVISION.\n",
        "WORKING-STORAGE SECTION.\n",
        "01 FILLER PIC X(4) BASED.\n",
        "01 B-PLAIN PIC X(4) BASED.\n",
        "01 BIG-DEF.\n",
        "   05 BIG-PAD PIC X(100) OCCURS 700.\n",
        "PROCEDURE DIVISION.\n",
        "    ALLOCATE B-PLAIN.\n",
        "    DISPLAY B-PLAIN.\n",
        "    STOP RUN.\n",
    );
    let ctx = CobolMlirContext::new();
    let (sm, interner, mut hir) = compile_to_hir(plain_src, Dialect::Gnu)?;
    let bad_sp = SourceSpan::new(
        FileId::new(99),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );
    if let Some(prog) = hir.0.programs.first_mut() {
        for it in &mut prog.data_items {
            it.span = bad_sp;
        }
        prog.formatting.default_byte = Some(b' ');
        let _ = build_program_storage_plan(prog, "P", &sm, &interner)?;
        prog.formatting.default_byte = None;
    }
    let cob_ok = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner)?;
    let _ = lower_cobol_to_llvm_mlir(
        &ctx,
        &cob_ok,
        OptimizationLevel::O0,
        MainEntryMode::OmitMain,
    )?;

    // Invalid `SourceSpan` on `FileSection` item (`storage_init.rs:368`).
    let file_src = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. M11-FS.\n",
        "ENVIRONMENT DIVISION.\n",
        "INPUT-OUTPUT SECTION.\n",
        "FILE-CONTROL.\n",
        "    SELECT F1 ASSIGN TO 'f1.dat'.\n",
        "DATA DIVISION.\n",
        "FILE SECTION.\n",
        "FD F1.\n",
        "01 F1-REC PIC X(4).\n",
        "PROCEDURE DIVISION.\n",
        "    STOP RUN.\n",
    );
    let (sm_f, int_f, mut hir_f) = compile_to_hir(file_src, Dialect::Gnu)?;
    if let Some(prog) = hir_f.0.programs.first_mut() {
        for it in &mut prog.data_items {
            it.span = bad_sp;
        }
        assert!(build_program_storage_plan(prog, "P", &sm_f, &int_f).is_ok());
    }
    Ok(())
}

/// Exercises M11 synthetic HIR error branches in `lower_from_hir` and `lower_m11`.
#[test]
fn m11_synthetic_hir_lowering_error_branches_coverage() -> Result<()> {
    use coblang_hir::{
        HirAllocateInit, HirAllocateSize, HirAllocateSpec, HirExpr, HirFreeTarget, HirPlace,
        HirPointerTarget, HirPointerValue, HirSetPointerSpec, HirStmt, HirType, JustifiedSpec,
    };

    let ctx = CobolMlirContext::new();
    let bad_sp = SourceSpan::new(
        FileId::new(99),
        ByteOffset::ZERO,
        ByteLength::ZERO,
        ExpansionId::ROOT,
    );

    // Synthetic HIR error paths in `lower_from_hir` and `lower_m11`.
    let base_src = "IDENTIFICATION DIVISION.\nPROGRAM-ID. M11-ERR.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 N PIC 9(4) VALUE 0.\n01 P USAGE POINTER.\nPROCEDURE DIVISION.\n    STOP RUN.\n";
    let (sm_b, mut int_b, base_hir) = compile_to_hir(base_src, Dialect::Gnu)?;
    let alloc_sym = int_b.intern("ALLOCATE")?;
    let sp = base_hir.0.programs.first().map_or(bad_sp, |p| p.span);
    let ok_pl = HirPlace {
        item: coblang_common::DataItemId::new(0),
        static_offset: ByteOffset::ZERO,
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        },
        check_mask: coblang_hir::ExceptionCheckMask::NONE,
        span: sp,
    };
    let bogus_pl = HirPlace {
        item: coblang_common::DataItemId::new(999),
        ..ok_pl.clone()
    };
    let ptr_pl = HirPlace {
        item: coblang_common::DataItemId::new(1),
        effective_ty: HirType::Pointer,
        ..ok_pl.clone()
    };

    for err_stmt in [
        HirStmt::Display {
            operands: vec![HirExpr::AddressOf(bogus_pl)],
            device: coblang_hir::HirDisplayDevice::Stdout,
            advancing: coblang_hir::HirAdvancing::AdvanceLine,
            span: sp,
        },
        HirStmt::Move {
            src: HirExpr::Neg(Box::new(HirExpr::AddressOf(ok_pl.clone()))),
            dst: ok_pl.clone(),
            span: sp,
        },
        HirStmt::SetPointer(Box::new(HirSetPointerSpec {
            targets: vec![HirPointerTarget::PointerPlace(ptr_pl.clone())],
            value: HirPointerValue::Entry(HirExpr::LengthOf(ok_pl)),
            span: sp,
        })),
        HirStmt::Allocate(Box::new(HirAllocateSpec {
            size: HirAllocateSize::BasedItem(coblang_common::DataItemId::new(999)),
            init: HirAllocateInit::Uninitialized,
            returning: None,
            span: sp,
        })),
        HirStmt::Free {
            targets: vec![HirFreeTarget::BasedItem(coblang_common::DataItemId::new(
                999,
            ))],
            span: sp,
        },
        HirStmt::HighLevelOp {
            verb: alloc_sym,
            span: sp,
        },
    ] {
        let mut h = base_hir.clone();
        if let Some(p) = h.0.programs.first_mut() {
            p.entry_statements.insert(0, err_stmt);
        }
        assert!(lower_hir_to_cobol_mlir(&ctx, &h, &sm_b, &int_b).is_err());
    }

    let ret_only_src = "IDENTIFICATION DIVISION.\nPROGRAM-ID. M11-RET.\nDATA DIVISION.\nLINKAGE SECTION.\n01 L-RET PIC S9(4) COMP-5.\nPROCEDURE DIVISION RETURNING L-RET.\n    GOBACK.\n";
    let (sm_r, int_r, hir_r) = compile_to_hir(ret_only_src, Dialect::Gnu)?;
    assert!(lower_hir_to_cobol_mlir(&ctx, &hir_r, &sm_r, &int_r).is_ok());

    let len_move_src = "IDENTIFICATION DIVISION.\nPROGRAM-ID. M11-LENMV.\nDATA DIVISION.\nWORKING-STORAGE SECTION.\n01 TXT PIC X(12) VALUE 'HELLO'.\n01 OUT-N PIC 9(4) VALUE 0.\n01 OUT-AE PIC XX0XX.\nPROCEDURE DIVISION.\n    MOVE LENGTH OF TXT TO OUT-N.\n    MOVE 0123 TO OUT-AE.\n    DISPLAY OUT-N OUT-AE.\n    STOP RUN.\n";
    let (sm_lm, int_lm, hir_lm) = compile_to_hir(len_move_src, Dialect::Gnu)?;
    let verified_lm = lower_hir_to_cobol_mlir(&ctx, &hir_lm, &sm_lm, &int_lm)?;
    let lowered_lm = lower_cobol_to_llvm_mlir(
        &ctx,
        &verified_lm,
        OptimizationLevel::O0,
        MainEntryMode::OmitMain,
    )?;
    assert!(lowered_lm.to_mlir_text().contains("coblang_rt_dec_store"));
    verify_m11_subprogram_call_cancel_chaining_coverage(&ctx)
}

/// Exercises all Milestone M11 `CALL` (`BY REFERENCE` / `BY CONTENT` / `BY VALUE` / `OMITTED` /
/// `RETURNING` / `ON EXCEPTION`), `PROCEDURE DIVISION USING / CHAINING / RETURNING`, `IS INITIAL`,
/// `LOCAL-STORAGE`, `NUMBER-OF-CALL-PARAMETERS`, `IS OMITTED`, `CANCEL`, `EXIT PROGRAM RETURNING`,
/// and `STOP RUN expr` lowering paths.
///
/// # Arguments
///
/// * `ctx` - Shared `CobolMlirContext`.
fn verify_m11_subprogram_call_cancel_chaining_coverage(ctx: &CobolMlirContext) -> Result<()> {
    let full_m11_src = "\
IDENTIFICATION DIVISION.
PROGRAM-ID. CALLER-MAIN.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 WS-A     PIC X(4) VALUE 'ABCD'.
01 WS-BIG   PIC X(12) VALUE '0123456789AB'.
01 WS-COMP  PIC S9(4) COMP VALUE 12.
01 WS-NAT   PIC S9(9) COMP-5 VALUE 34.
01 WS-PTR   USAGE POINTER VALUE NULL.
01 WS-PPTR  USAGE PROGRAM-POINTER VALUE NULL.
01 WS-DYN   PIC X(8) VALUE 'EXTMOD'.
01 WS-CH    PIC X(4) VALUE 'INIT'.
01 BS-X     PIC X BASED.
PROCEDURE DIVISION CHAINING WS-CH RETURNING OMITTED.
    DISPLAY NUMBER-OF-CALL-PARAMETERS BS-X.
    CALL 'SUB-INMOD' USING BY REFERENCE WS-A 'RLIT' WS-COMP OMITTED
                           BY CONTENT WS-A 'LIT' NULL 99
                           BY VALUE ADDRESS OF WS-A WS-PTR WS-NAT WS-A WS-BIG 7
         RETURNING WS-NAT
         NOT ON EXCEPTION DISPLAY WS-NAT
    END-CALL.
    CALL 'SUB-INMOD' RETURNING OMITTED.
    CALL WS-DYN USING WS-A RETURNING WS-NAT
         ON EXCEPTION DISPLAY 'EX'
         NOT ON EXCEPTION DISPLAY 'OK'
    END-CALL.
    CALL WS-PPTR RETURNING NOTHING.
    CALL 'EXT2' RETURNING WS-NAT
         NOT ON EXCEPTION DISPLAY 'N'
    END-CALL.
    CALL 'EXT3' RETURNING WS-NAT.
    CANCEL 'SUB-INMOD' WS-DYN.
    STOP RUN 33.

IDENTIFICATION DIVISION.
PROGRAM-ID. SUB-INMOD IS INITIAL.
DATA DIVISION.
LOCAL-STORAGE SECTION.
01 LS-TMP   PIC X(2) VALUE 'XY'.
LINKAGE SECTION.
01 LK-REF   PIC X(4).
01 LK-RLIT  PIC X(4).
01 LK-BE    PIC S9(4) COMP.
01 LK-OPT   PIC X(2).
01 LK-CNT1  PIC X(4).
01 LK-CNT2  PIC X(3).
01 LK-CNT3  USAGE POINTER.
01 LK-CNT4  PIC S9(9) COMP-5.
01 LK-VALP  USAGE POINTER.
01 LK-VALQ  USAGE POINTER.
01 LK-VALN  PIC S9(9) COMP-5.
01 LK-VALA  PIC X(4).
01 LK-VALB  PIC X(12).
01 LK-VALBE PIC S9(4) COMP.
PROCEDURE DIVISION USING BY REFERENCE LK-REF LK-RLIT LK-BE OPTIONAL LK-OPT
                         BY CONTENT LK-CNT1 LK-CNT2 LK-CNT3 LK-CNT4
                         BY VALUE LK-VALP LK-VALQ LK-VALN LK-VALA LK-VALB LK-VALBE
                   RETURNING LK-VALN.
    IF LK-OPT IS OMITTED
        DISPLAY LS-TMP LK-REF
    END-IF.
    EXIT PROGRAM RETURNING 5.
END PROGRAM SUB-INMOD.
END PROGRAM CALLER-MAIN.
";
    let (sm, int, hir) = compile_to_hir(full_m11_src, Dialect::Gnu)?;
    let verified = lower_hir_to_cobol_mlir(ctx, &hir, &sm, &int)?;
    let lowered = lower_cobol_to_llvm_mlir(
        ctx,
        &verified,
        OptimizationLevel::O0,
        MainEntryMode::EmitMain,
    )?;
    assert!(lowered.to_mlir_text().contains("__coblang_cancel_"));
    verify_m11_synthetic_call_and_cancel_guards(ctx, &sm, &int, hir)
}

/// Exercises synthetic HIR error/guard arms on `CANCEL`, `CALL`, ghost `USING`/`RETURNING` IDs,
/// and `CobolCallOp(DynamicLiteral)`.
///
/// # Arguments
///
/// * `ctx` - Shared `CobolMlirContext`.
/// * `sm` - Source map.
/// * `int` - Symbol interner.
/// * `hir` - Base `ResolvedHirModule`.
fn verify_m11_synthetic_call_and_cancel_guards(
    ctx: &CobolMlirContext,
    sm: &SourceMap,
    int: &SymbolInterner,
    hir: ResolvedHirModule,
) -> Result<()> {
    use coblang_common::DataItemId;
    use coblang_hir::{
        ExceptionCheckMask, HirCallArg, HirCallReturning, HirCallSpec, HirExpr,
        HirParamOptionality, HirParamSource, HirPassingMode, HirPlace, HirStmt, HirType,
        HirUsingParam, JustifiedSpec,
    };

    let sp = SourceSpan::new(
        FileId::new(0),
        ByteOffset::new(0),
        ByteLength::new(1),
        ExpansionId::ROOT,
    );
    let bad_expr = HirExpr::LengthOf(HirPlace {
        item: DataItemId::new(0),
        static_offset: ByteOffset::new(0),
        subscripts: Vec::new(),
        refmod: None,
        effective_ty: HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        },
        check_mask: ExceptionCheckMask::NONE,
        span: sp,
    });
    for bad_stmt in [
        HirStmt::Cancel {
            targets: vec![bad_expr.clone()],
            span: sp,
        },
        HirStmt::Call(Box::new(HirCallSpec {
            callee: bad_expr,
            args: Vec::new(),
            returning: HirCallReturning::ImplicitReturnCode,
            on_exception: Vec::new(),
            not_on_exception: Vec::new(),
            span: sp,
        })),
    ] {
        let mut h = hir.clone();
        if let Some(p) = h.0.programs.first_mut() {
            p.entry_statements.insert(0, bad_stmt);
        }
        assert!(lower_hir_to_cobol_mlir(ctx, &h, sm, int).is_err());
    }

    let mut h_ghost = hir;
    if let Some(p) = h_ghost.0.programs.first_mut() {
        for src_kind in [HirParamSource::Using, HirParamSource::Chaining] {
            p.using_params.push(HirUsingParam {
                source: src_kind,
                mode: HirPassingMode::ByReference,
                optionality: HirParamOptionality::Optional,
                length_mode: coblang_hir::HirParamLengthMode::Fixed,
                item: DataItemId::new(999),
                span: sp,
            });
        }
        p.returning_item = Some(DataItemId::new(999));
        let _ = HirCallArg::Omitted;
    }
    assert!(lower_hir_to_cobol_mlir(ctx, &h_ghost, sm, int).is_ok());
    verify_dynamic_literal_call_op_llvm_lowering(ctx)
}

/// Exercises `CobolCallOp(DynamicLiteral)` with a non-empty `callee` in `lower_to_llvm`.
///
/// # Arguments
///
/// * `ctx` - Shared `CobolMlirContext`.
fn verify_dynamic_literal_call_op_llvm_lowering(ctx: &CobolMlirContext) -> Result<()> {
    let loc = ctx.unknown_location();
    let dyn_mod = Module::new(loc);
    let _ = dyn_mod.body().append_operation(
        CobolStorageOp {
            sym_name: "__rc_dyn",
            storage_kind: StorageKind::Working,
            byte_size: 4,
            alignment: 4,
            init_bytes: &[0; 4],
        }
        .build(ctx, loc)?,
    );
    let body_reg = Region::new();
    let body_blk = body_reg.append_block(Block::new(&[]));
    let _ = body_blk.append_operation(
        CobolCallOp {
            dispatch: CallDispatchKind::DynamicLiteral,
            callee: "EXT_TARGET",
            args: &[],
            exception_branches: ExceptionBranches::None,
        }
        .build(ctx, loc)?,
    );
    let _ = body_blk.append_operation(
        CobolGobackOp {
            mode: GobackMode::ExitProgram,
        }
        .build(ctx, loc)?,
    );
    let _ = dyn_mod.body().append_operation(
        CobolProgramOp {
            sym_name: String::from("__coblang_prog_DYNTEST"),
            raw_c_alias: String::new(),
            rc_slot_sym: String::from("__rc_dyn"),
            initial_state: InitialStateMode::Retained,
            recursivity: RecursivityMode::NonRecursive,
            entry_convention: EntryConventionMode::Cobol,
            body: body_reg,
        }
        .build(ctx, loc)?,
    );
    let verified_dyn = verify_cobol_module(dyn_mod)?;
    let _ = lower_cobol_to_llvm_mlir(
        ctx,
        &verified_dyn,
        OptimizationLevel::O0,
        MainEntryMode::OmitMain,
    )?;
    Ok(())
}
