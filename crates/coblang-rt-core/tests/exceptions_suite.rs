//! Dedicated ISO/IEC 1989:2023 `EC-*` exception hierarchy, `>>TURN WITH LOCATION`,
//! `RESUME` target, `DEBUG-ITEM`, and runtime error-branch tests (`Milestone M13`).

use coblang_common::{DigitCount, Result, ScaleFactor};
use coblang_decimal::{HighOrderPolicy, PackedSignPolicy, RoundingMode, SizeErrorStatus};
use coblang_hir::{BinaryEndianness, HirType, JustifiedSpec, Signedness, TruncationPolicy};
use coblang_pic::{
    DefaultInitMode, FillerPolicy, InitCategoryMask, MoveFieldDesc, StringFlowOutcome,
    UnstringMatchMode,
};
use coblang_rt_core::CobolRuntimeContext;
use coblang_rt_core::desc_codec::{
    DecodedInitPlan, FillerMembership, RedefinesMembership, RuntimeFieldSpec, RuntimeInitEntry,
    encode_field_spec, encode_init_plan,
};
use coblang_rt_core::exceptions::{
    DEBUG_ITEM_HEADER_LEN, DebugItemState, DebugRuntimeSwitch, DeclarativeGuardState,
    EXCEPTION_CATALOG_LEN, ExceptionCategory, ExceptionCode, ExceptionLevel, ExceptionRecord,
    ExceptionRuntimeState, ExceptionSeverity, ProgramDebugMode, ResumeTarget, SizeOverflowCause,
    TurnCheckingMode, parse_cob_set_debug,
};
use coblang_rt_core::field_ops::{
    AlphaCmpOperand, RelCmpOp, RuntimeMoveSourceKind, runtime_compare_alphanumeric_ex,
    runtime_sort_table,
};
use coblang_rt_core::m10_staging::{
    M10StringInspectStaging, StagedInspectOperand, StagedStringDelim, decode_optional_move_desc,
    normalize_with_spec,
};

/// Verifies all `146` ISO/IEC 1989:2023 `EC-*` catalog entries and default severities.
#[test]
fn iso2023_catalog_entries_and_severities() -> Result<()> {
    assert_eq!(ExceptionCode::catalog().len(), EXCEPTION_CATALOG_LEN);

    for (idx, spec) in ExceptionCode::catalog().iter().enumerate() {
        let raw_id = u16::try_from(idx).unwrap_or(u16::MAX);
        assert_eq!(spec.code.to_abi_id(), raw_id);
        assert_eq!(ExceptionCode::from_abi_id(raw_id)?, spec.code);
        assert_eq!(
            ExceptionCode::from_cobol_name(&spec.name.to_ascii_lowercase())?,
            spec.code
        );
        let sev = spec.code.default_severity();
        match spec.level {
            ExceptionLevel::All | ExceptionLevel::Category => {
                assert_eq!(sev, ExceptionSeverity::Selector);
            }
            ExceptionLevel::Leaf => {
                assert!(matches!(
                    sev,
                    ExceptionSeverity::Fatal | ExceptionSeverity::NonFatal
                ));
            }
        }
    }

    assert_eq!(
        ExceptionCode::EcContinueLessThanZero.category(),
        ExceptionCategory::Continue
    );
    assert_eq!(
        ExceptionCode::EcContinueLessThanZero.default_severity(),
        ExceptionSeverity::Fatal
    );
    assert_eq!(
        ExceptionCode::EcFlowApplyCommit.category(),
        ExceptionCategory::Flow
    );
    assert_eq!(
        ExceptionCode::EcFlowCommit.category(),
        ExceptionCategory::Flow
    );
    assert_eq!(
        ExceptionCode::EcFlowRollback.category(),
        ExceptionCategory::Flow
    );
    assert_eq!(
        ExceptionCode::EcIoAtEnd.default_severity(),
        ExceptionSeverity::NonFatal
    );
    assert_eq!(
        ExceptionCode::EcIoPermanentError.default_severity(),
        ExceptionSeverity::Fatal
    );
    Ok(())
}

/// Verifies `SizeErrorStatus` and 2-character `FILE STATUS` mappers onto `ExceptionCode`.
#[test]
fn size_error_and_io_status_domain_mappers() {
    assert_eq!(
        ExceptionCode::from_size_error(
            SizeErrorStatus::Exact,
            SizeOverflowCause::HighOrderOverflow
        ),
        None
    );
    assert_eq!(
        ExceptionCode::from_size_error(
            SizeErrorStatus::DivisionByZero,
            SizeOverflowCause::HighOrderOverflow
        ),
        Some(ExceptionCode::EcSizeZeroDivide)
    );
    assert_eq!(
        ExceptionCode::from_size_error(
            SizeErrorStatus::InvalidExponent,
            SizeOverflowCause::HighOrderOverflow
        ),
        Some(ExceptionCode::EcSizeExponentiation)
    );
    assert_eq!(
        ExceptionCode::from_size_error(
            SizeErrorStatus::Overflowed,
            SizeOverflowCause::HighOrderOverflow
        ),
        Some(ExceptionCode::EcSizeOverflow)
    );
    assert_eq!(
        ExceptionCode::from_size_error(
            SizeErrorStatus::Overflowed,
            SizeOverflowCause::ProhibitedTruncation
        ),
        Some(ExceptionCode::EcSizeTruncation)
    );

    let cases: &[([u8; 2], Option<ExceptionCode>)] = &[
        (*b"00", None),
        (*b"02", Some(ExceptionCode::EcIoWarning)),
        (*b"10", Some(ExceptionCode::EcIoAtEnd)),
        (*b"23", Some(ExceptionCode::EcIoInvalidKey)),
        (*b"35", Some(ExceptionCode::EcIoPermanentError)),
        (*b"41", Some(ExceptionCode::EcIoLogicError)),
        (*b"57", Some(ExceptionCode::EcIoLinage)),
        (*b"51", Some(ExceptionCode::EcIoFileSharing)),
        (*b"61", Some(ExceptionCode::EcIoFileSharing)),
        (*b"71", Some(ExceptionCode::EcIoRecordContent)),
        (*b"91", Some(ExceptionCode::EcIoImp)),
    ];
    for &(status, expected) in cases {
        assert_eq!(ExceptionCode::from_io_status_bytes(status), expected);
    }
}

/// Verifies `>>TURN WITH LOCATION`, `>>PUSH`/`>>POP TURN`, `raise_io`, `RESUME`, and `EXCEPTION-*` formatting.
#[test]
fn turn_location_stack_io_status_and_resume_flow() -> Result<()> {
    assert_eq!(
        TurnCheckingMode::from_abi_code(0),
        TurnCheckingMode::Disabled
    );
    assert_eq!(
        TurnCheckingMode::from_abi_code(1),
        TurnCheckingMode::Enabled
    );
    assert_eq!(
        TurnCheckingMode::from_abi_code(2),
        TurnCheckingMode::EnabledWithLocation
    );
    assert_eq!(
        TurnCheckingMode::from_abi_code(99),
        TurnCheckingMode::Disabled
    );
    assert_eq!(TurnCheckingMode::Disabled.to_abi_code(), 0);
    assert_eq!(TurnCheckingMode::Enabled.to_abi_code(), 1);
    assert_eq!(TurnCheckingMode::EnabledWithLocation.to_abi_code(), 2);

    let mut ex = ExceptionRuntimeState::new();
    assert!(ex.pop_turn_state().is_err());
    assert_eq!(ex.format_exception_location_iso(), " ");
    assert_eq!(ex.format_exception_file_iso(), "00");
    assert_eq!(ex.last_io_status(), *b"00");

    ex.push_turn_state();
    ex.apply_turn(ExceptionCode::EcSize, TurnCheckingMode::Enabled);
    assert!(ex.is_enabled(ExceptionCode::EcSizeZeroDivide));
    assert!(!ex.is_location_enabled(ExceptionCode::EcSizeZeroDivide));

    ex.raise_respecting_turn(ExceptionRecord {
        code: ExceptionCode::EcSizeZeroDivide,
        statement: String::from("DIVIDE"),
        location: String::from("PROG; PARA-A; 19"),
        file_name: String::new(),
    });
    assert_eq!(ex.exception_location(), "");
    assert_eq!(ex.format_exception_location_iso(), " ");

    ex.apply_turn(
        ExceptionCode::EcSizeZeroDivide,
        TurnCheckingMode::EnabledWithLocation,
    );
    assert!(ex.is_location_enabled(ExceptionCode::EcSizeZeroDivide));
    ex.raise_respecting_turn(ExceptionRecord {
        code: ExceptionCode::EcSizeZeroDivide,
        statement: String::from("DIVIDE"),
        location: String::from("PROG; PARA-A; 19"),
        file_name: String::new(),
    });
    assert_eq!(ex.exception_location(), "PROG; PARA-A; 19");
    assert_eq!(ex.format_exception_location_iso(), "PROG; PARA-A; 19");

    ex.pop_turn_state()?;
    assert!(!ex.is_enabled(ExceptionCode::EcSizeZeroDivide));
    assert!(!ex.is_location_enabled(ExceptionCode::EcSizeZeroDivide));

    verify_io_and_resume_helpers(&mut ex)?;
    Ok(())
}

/// Exercises `raise_io`, `format_exception_file_iso`, and `RESUME` target transitions on `ex`.
///
/// # Arguments
///
/// * `ex` - Mutable [`ExceptionRuntimeState`] under test.
///
/// # Errors
///
/// Returns an error if valid declarative entry or `RESUME` target assignment fails.
fn verify_io_and_resume_helpers(ex: &mut ExceptionRuntimeState) -> Result<()> {
    ex.raise_io(
        *b"35",
        ExceptionRecord {
            code: ExceptionCode::EcIoPermanentError,
            statement: String::from("OPEN"),
            location: String::from("PROG; IO-SEC; 30"),
            file_name: String::from("MASTER-FILE"),
        },
    );
    assert_eq!(ex.last_io_status(), *b"35");
    assert_eq!(ex.format_exception_file_iso(), "35MASTER-FILE");
    assert_eq!(ex.exception_location(), "");

    ex.apply_turn(ExceptionCode::EcIo, TurnCheckingMode::EnabledWithLocation);
    ex.raise_io(
        *b"23",
        ExceptionRecord {
            code: ExceptionCode::EcIoInvalidKey,
            statement: String::from("READ"),
            location: String::from("PROG; IO-SEC; 44"),
            file_name: String::from("IDX-FILE"),
        },
    );
    assert_eq!(ex.format_exception_file_iso(), "23IDX-FILE");
    assert_eq!(ex.exception_location(), "PROG; IO-SEC; 44");

    ex.raise_io_verbatim(
        *b"10",
        ExceptionRecord {
            code: ExceptionCode::EcIoAtEnd,
            statement: String::from("READ"),
            location: String::from("PROG; IO-SEC; 45"),
            file_name: String::from("SEQ-FILE"),
        },
    );
    assert_eq!(ex.format_exception_file_iso(), "10SEQ-FILE");
    assert_eq!(ex.exception_location(), "PROG; IO-SEC; 45");

    assert!(ex.set_resume_target(ResumeTarget::NextStatement).is_err());
    assert_eq!(
        ex.last_record().map(|r| r.code),
        Some(ExceptionCode::EcFlowUse)
    );

    ex.enter_declarative()?;
    assert_eq!(ex.declarative_guard(), DeclarativeGuardState::InDeclarative);
    assert_eq!(ex.resume_target(), ResumeTarget::Unset);
    ex.set_resume_target(ResumeTarget::NextStatement)?;
    assert_eq!(ex.resume_target(), ResumeTarget::NextStatement);
    ex.set_resume_target(ResumeTarget::ProcedureId(12))?;
    assert_eq!(ex.resume_target(), ResumeTarget::ProcedureId(12));
    ex.leave_declarative();
    assert_eq!(ex.declarative_guard(), DeclarativeGuardState::Idle);
    assert_eq!(ex.resume_target(), ResumeTarget::ProcedureId(12));

    ex.clear_last_exception();
    assert_eq!(ex.last_io_status(), *b"00");
    ex.reset();
    assert_eq!(ex.resume_target(), ResumeTarget::Unset);
    Ok(())
}

/// Verifies `DEBUG-ITEM` 56-byte header formatting, `COB_SET_DEBUG` switches, and `CobolRuntimeContext` accessors.
#[test]
fn debug_item_runtime_switch_and_context_sync() {
    assert_eq!(parse_cob_set_debug("1"), DebugRuntimeSwitch::ForceEnabled);
    assert_eq!(parse_cob_set_debug("yes"), DebugRuntimeSwitch::ForceEnabled);
    assert_eq!(parse_cob_set_debug("0"), DebugRuntimeSwitch::ForceDisabled);
    assert_eq!(
        parse_cob_set_debug("off"),
        DebugRuntimeSwitch::ForceDisabled
    );
    assert_eq!(
        parse_cob_set_debug("auto"),
        DebugRuntimeSwitch::DefaultFromProgram
    );

    let mut dbg = DebugItemState::new();
    assert_eq!(dbg.runtime_switch(), DebugRuntimeSwitch::DefaultFromProgram);
    assert!(dbg.is_active_for_program(ProgramDebugMode::WithDebuggingMode));
    assert!(!dbg.is_active_for_program(ProgramDebugMode::WithoutDebuggingMode));

    dbg.set_runtime_switch(DebugRuntimeSwitch::ForceEnabled);
    assert!(dbg.is_active_for_program(ProgramDebugMode::WithoutDebuggingMode));

    dbg.set_runtime_switch(DebugRuntimeSwitch::ForceDisabled);
    assert!(!dbg.is_active_for_program(ProgramDebugMode::WithDebuggingMode));

    dbg.populate(
        "000120",
        "CALC-TOTAL",
        [Some(1), Some(-42), None],
        b"FALL THROUGH",
    );
    let mut stack_buf = [0_u8; DEBUG_ITEM_HEADER_LEN + 16];
    dbg.write_into_buffer(&mut stack_buf);
    assert_eq!(dbg.to_record_bytes(16), stack_buf);

    let mut rt = CobolRuntimeContext::new();
    rt.initialize_with_env(Vec::new(), |key| {
        if key == "COB_SET_DEBUG" {
            Some(String::from("ON"))
        } else {
            None
        }
    });
    assert_eq!(
        rt.debug_item().runtime_switch(),
        DebugRuntimeSwitch::ForceEnabled
    );

    rt.exceptions_mut().raise_code(ExceptionCode::EcBoundOdo);
    assert_eq!(rt.last_exception(), "EC-BOUND-ODO");
    assert_eq!(
        rt.exceptions().last_record().map(|r| r.code),
        Some(ExceptionCode::EcBoundOdo)
    );
    rt.exceptions_mut().clear_last_exception();
    assert_eq!(rt.last_exception(), "");
}

/// Exercises partial `UNSTRING` overflow branch and fallible `?` error regions in `m10_staging`.
#[test]
fn m10_staging_remaining_branch_and_error_paths() -> Result<()> {
    let mut st = M10StringInspectStaging::new();
    let alpha2_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 2,
            justified: JustifiedSpec::Left,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    st.unstring_begin(b"AB,CD", None, 1)?;
    st.unstring_push_delim(UnstringMatchMode::Single, b",", None)?;
    let mut slot = *b"  ";
    st.unstring_step_into(&mut slot, &alpha2_spec, None)?;
    assert_eq!(&slot, b"AB");
    assert_eq!(st.unstring_finish(), StringFlowOutcome::Overflowed);

    let bad_spec = [0xFF_u8];
    assert!(StagedStringDelim::from_raw(1, b"X", Some(&bad_spec)).is_err());
    assert!(StagedInspectOperand::from_raw(0, b"X", Some(&bad_spec)).is_err());
    assert!(normalize_with_spec(b"X", Some(&bad_spec)).is_err());
    assert!(decode_optional_move_desc(Some(&bad_spec)).is_err());
    assert!(st.string_push_src(b"X", Some(&bad_spec)).is_err());
    assert!(st.unstring_begin(b"X", Some(&bad_spec), 1).is_err());
    assert!(
        st.unstring_push_delim(UnstringMatchMode::Single, b",", Some(&bad_spec))
            .is_err()
    );
    st.unstring_begin(b"AB,CD", None, 1)?;
    assert!(st.unstring_step_into(&mut slot, &bad_spec, None).is_err());

    st.unstring_begin(b"AB,CD", None, 1)?;
    st.unstring_push_delim(UnstringMatchMode::Single, b",", None)?;
    st.unstring_set_delim_in_spec(&bad_spec);
    let mut d_out = *b" ";
    assert!(
        st.unstring_step_into(&mut slot, &alpha2_spec, Some(&mut d_out))
            .is_err()
    );

    let bin4_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::BinaryInt {
            digits: DigitCount::try_new(9)?,
            scale: ScaleFactor::ZERO,
            sign: Signedness::Signed,
            byte_width: 4,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::TruncateToPic,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    st.unstring_begin(b"ABC", None, 1)?;
    let mut bin4_buf = [0_u8; 4];
    assert!(
        st.unstring_step_into(&mut bin4_buf, &bin4_spec, None)
            .is_err()
    );

    let mut insp_target = *b"A";
    assert!(
        st.inspect_exec_tally_replace(&mut insp_target, Some(&bad_spec))
            .is_err()
    );
    assert!(
        st.inspect_exec_converting_from_regs(&mut insp_target, Some(&bad_spec))
            .is_err()
    );
    Ok(())
}

/// Exercises remaining fallible `?` error regions in `field_ops`.
#[test]
fn field_ops_remaining_error_paths() -> Result<()> {
    let bad_spec = [0xFF_u8];
    let alpha2_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 2,
            justified: JustifiedSpec::Left,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let bin4_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::BinaryInt {
            digits: DigitCount::try_new(9)?,
            scale: ScaleFactor::ZERO,
            sign: Signedness::Signed,
            byte_width: 4,
            endian: BinaryEndianness::Native,
            trunc: TruncationPolicy::TruncateToPic,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });

    let empty_buf: [u8; 0] = [];
    let lhs_bad_spec = AlphaCmpOperand {
        bytes: b"12",
        kind: RuntimeMoveSourceKind::Field,
        spec_bytes: &bad_spec,
    };
    let rhs_raw = AlphaCmpOperand {
        bytes: b"12",
        kind: RuntimeMoveSourceKind::AlphanumericLiteral,
        spec_bytes: &[],
    };
    assert!(runtime_compare_alphanumeric_ex(
        &lhs_bad_spec,
        &rhs_raw,
        RelCmpOp::Eq,
        None,
    ));

    let lhs_short_bin = AlphaCmpOperand {
        bytes: &empty_buf,
        kind: RuntimeMoveSourceKind::Field,
        spec_bytes: &bin4_spec,
    };
    let _ = runtime_compare_alphanumeric_ex(&lhs_short_bin, &rhs_raw, RelCmpOp::Eq, None);

    let mut rt = CobolRuntimeContext::new();
    rt.decimal_stack_mut().begin();
    rt.decimal_stack_mut().push_ascii(b"10")?;
    rt.decimal_stack_mut().push_ascii(b"3")?;
    let q_buf = *b"00";
    let r_buf = *b"00";
    assert!(
        rt.decimal_stack_mut()
            .divide_remainder(
                (&q_buf, &alpha2_spec, RoundingMode::Truncation),
                (&r_buf, &bad_spec),
                HighOrderPolicy::ModuloTruncate,
            )
            .is_err()
    );

    assert!(
        rt.display_staging_mut()
            .push_field(&empty_buf, &bin4_spec)
            .is_err()
    );

    let bad_init_plan = encode_init_plan(&DecodedInitPlan {
        filler_policy: FillerPolicy::SkipFiller,
        to_value_mask: InitCategoryMask::NONE,
        default_mode: DefaultInitMode::StandardCobol,
        entries: vec![RuntimeInitEntry {
            offset: 0,
            dims: Vec::new(),
            byte_len: 4,
            desc: MoveFieldDesc::standard_gnu(HirType::BinaryInt {
                digits: DigitCount::try_new(9)?,
                scale: ScaleFactor::ZERO,
                sign: Signedness::Signed,
                byte_width: 4,
                endian: BinaryEndianness::Native,
                trunc: TruncationPolicy::TruncateToPic,
            }),
            filler: FillerMembership::NamedField,
            redefines: RedefinesMembership::PrimaryStorage,
            value_bytes: None,
        }],
    });
    let mut bin4_buf = [0_u8; 4];
    rt.init_staging_mut().clear();
    rt.init_staging_mut().push_replacement(
        coblang_hir::DataCategory::Numeric,
        RuntimeMoveSourceKind::AlphanumericLiteral,
        b"ABC".to_vec(),
        &[],
    )?;
    assert!(
        rt.init_staging_mut()
            .execute_plan(&mut bin4_buf, &bad_init_plan)
            .is_err()
    );

    let mut slot = *b"  ";
    assert!(runtime_sort_table(&mut slot, 1, &bad_spec).is_err());
    Ok(())
}
