//! Unit tests for `coblang-fileio` domain types (`types`), `ASSIGN TO` resolver (`assign`),
//! and exhaustive 28-code `FILE STATUS` transition table (`status`).

use std::io::{self, ErrorKind};
use std::path::PathBuf;

use coblang_common::{CoblangError, DigitCount, FileStatusKind, Result};
use coblang_fileio::{
    ASCII_CR, ASCII_FF, ASCII_LF, ASCII_NUL, AccessMode, AssignPrefixPolicy,
    AssignResolutionConfig, AssignResolutionOrigin, AssignSource, CloseDisposition,
    CloseTransitionOutcome, ConnectorLifecycle, ConnectorStateMachine, DeclarativePresence,
    EnvLookup, FileOrganization, InlineClausePresence, IoErrorContext, IoExceptionClass,
    LineSequentialEnvOptions, LineTerminator, LiteralEnvPolicy, MapEnv, MutationPrecheckOutcome,
    NullEscapePolicy, OpenMode, PostIoDispatch, PrecheckOutcome, PriorReadRecord, ProcessEnv,
    RecordByteLen, RecordLayout, ReelMediumKind, RelativeSlotKey, ResolvedAssignTarget,
    SequentialCursorState, StandardDevice, StartRelation, StatusCategory, StatusClausePresence,
    TrailingSpacePolicy, VaryingHeaderFormat, VerbHandlerConfig, classify_io_error,
    classify_storage_error, resolve_assign_target, resolve_post_io_action, status_descriptor,
    write_status_bytes,
};

/// Verifies [`StartRelation`], [`RecordByteLen`], [`RecordLayout`], [`LineTerminator`], and [`RelativeSlotKey`].
#[test]
fn domain_types_validation_and_relations() -> Result<()> {
    assert_eq!(ASCII_NUL, 0x00);
    assert_eq!(ASCII_LF, 0x0A);
    assert_eq!(ASCII_FF, 0x0C);
    assert_eq!(ASCII_CR, 0x0D);

    assert!(StartRelation::Equal.matches_u64(5, 5));
    assert!(!StartRelation::Equal.matches_u64(5, 6));
    assert!(StartRelation::Greater.matches_u64(6, 5));
    assert!(!StartRelation::Greater.matches_u64(5, 5));
    assert!(StartRelation::GreaterOrEqual.matches_u64(5, 5));
    assert!(!StartRelation::GreaterOrEqual.matches_u64(4, 5));
    assert!(StartRelation::Less.matches_u64(4, 5));
    assert!(!StartRelation::Less.matches_u64(5, 5));
    assert!(StartRelation::LessOrEqual.matches_u64(5, 5));
    assert!(!StartRelation::LessOrEqual.matches_u64(6, 5));

    assert!(RecordByteLen::try_new(0).is_err());
    assert_eq!(RecordByteLen::try_new(80)?.get(), 80);

    assert_eq!(VaryingHeaderFormat::TwoByteBigEndian.header_bytes(), 2);
    assert_eq!(VaryingHeaderFormat::FourByteBigEndian.header_bytes(), 4);

    assert!(RecordLayout::fixed_spaces(0).is_err());
    let fixed = RecordLayout::fixed_spaces(10)?;
    assert_eq!(fixed.min_bytes(), 10);
    assert_eq!(fixed.max_bytes(), 10);
    assert!(fixed.accepts_write_len(10));
    assert!(!fixed.accepts_write_len(9));
    assert!(!fixed.accepts_write_len(11));

    assert!(RecordLayout::varying(0, 10, VaryingHeaderFormat::TwoByteBigEndian).is_err());
    assert!(RecordLayout::varying(12, 10, VaryingHeaderFormat::TwoByteBigEndian).is_err());
    assert!(RecordLayout::varying(1, 70_000, VaryingHeaderFormat::TwoByteBigEndian).is_err());
    #[cfg(target_pointer_width = "64")]
    assert!(
        RecordLayout::varying(
            1,
            (u32::MAX as usize) + 1,
            VaryingHeaderFormat::FourByteBigEndian,
        )
        .is_err()
    );

    let varying = RecordLayout::varying(4, 16, VaryingHeaderFormat::FourByteBigEndian)?;
    assert_eq!(varying.min_bytes(), 4);
    assert_eq!(varying.max_bytes(), 16);
    assert!(!varying.accepts_write_len(3));
    assert!(varying.accepts_write_len(4));
    assert!(varying.accepts_write_len(16));
    assert!(!varying.accepts_write_len(17));

    assert_eq!(LineTerminator::Lf.as_bytes(), &[0x0A]);
    assert_eq!(LineTerminator::CrLf.as_bytes(), &[0x0D, 0x0A]);

    assert!(RelativeSlotKey::try_new(0).is_err());
    let key9 = RelativeSlotKey::try_new(9)?;
    let key10 = RelativeSlotKey::try_new(10)?;
    assert_eq!(key9.get(), 9);
    assert_eq!(key9.zero_based_index(), 8);
    let one_digit = DigitCount::try_new(1)?;
    let twenty_digits = DigitCount::try_new(20)?;
    assert!(key9.fits_digit_count(one_digit));
    assert!(!key10.fits_digit_count(one_digit));
    assert!(key10.fits_digit_count(twenty_digits));
    Ok(())
}

/// Verifies `ASSIGN TO` device, dynamic variable, external `DD_*` priority, and literal resolution.
#[test]
fn assign_resolver_covers_all_priorities_and_policies() -> Result<()> {
    let proc_env = ProcessEnv;
    let _ = proc_env.get_var("PATH");
    assert!(
        proc_env
            .get_var("COBLANG_DEFINITELY_UNSET_VAR_06D")
            .is_none()
    );

    let mut env = MapEnv::new().with_var("DD_MY_FILE", "  sub/dd_upper.dat  ");
    env.insert("dd_my_file", "sub/dd_lower.dat");
    assert_eq!(
        env.get_var("dd_my_file").as_deref(),
        Some("sub/dd_lower.dat")
    );

    let cfg = AssignResolutionConfig::default();
    for dev in [
        StandardDevice::DisplayStdout,
        StandardDevice::KeyboardStdin,
        StandardDevice::SyserrStderr,
    ] {
        let res = resolve_assign_target("F", &AssignSource::Device(dev), cfg, &env)?;
        assert_eq!(res.target, ResolvedAssignTarget::StandardDevice(dev));
        assert_eq!(res.origin, AssignResolutionOrigin::StandardDevice);
    }

    assert!(
        resolve_assign_target("F", &AssignSource::DynamicVariable(vec![0xFF]), cfg, &env).is_err()
    );
    assert!(
        resolve_assign_target(
            "F",
            &AssignSource::DynamicVariable(b"   \0 ".to_vec()),
            cfg,
            &env
        )
        .is_err()
    );
    let dyn_rel = resolve_assign_target(
        "F",
        &AssignSource::DynamicVariable(b"dyn.dat   \0".to_vec()),
        cfg,
        &env,
    )?;
    assert_eq!(
        dyn_rel.target,
        ResolvedAssignTarget::FilesystemPath(PathBuf::from("dyn.dat"))
    );
    assert_eq!(dyn_rel.origin, AssignResolutionOrigin::DynamicVariable);

    let env_blank_base = MapEnv::new().with_var("COB_FILE_PATH", "   ");
    let dyn_blank_base = resolve_assign_target(
        "F",
        &AssignSource::DynamicVariable(b"dyn2.dat".to_vec()),
        cfg,
        &env_blank_base,
    )?;
    assert_eq!(
        dyn_blank_base.target,
        ResolvedAssignTarget::FilesystemPath(PathBuf::from("dyn2.dat"))
    );

    let env_with_base = MapEnv::new().with_var("COB_FILE_PATH", "/var/cobol");
    let dyn_joined = resolve_assign_target(
        "F",
        &AssignSource::DynamicVariable(b"rel/item.dat".to_vec()),
        cfg,
        &env_with_base,
    )?;
    assert_eq!(
        dyn_joined.target,
        ResolvedAssignTarget::FilesystemPath(PathBuf::from("/var/cobol/rel/item.dat"))
    );
    let dyn_abs = resolve_assign_target(
        "F",
        &AssignSource::DynamicVariable(b"/abs/item.dat".to_vec()),
        cfg,
        &env_with_base,
    )?;
    assert_eq!(
        dyn_abs.target,
        ResolvedAssignTarget::FilesystemPath(PathBuf::from("/abs/item.dat"))
    );
    Ok(())
}

/// Verifies `ExternalName` and mainframe prefix (`UT-S-`, `AS-`, `S-`) resolution policies.
#[test]
fn assign_external_and_mainframe_prefix_policies() -> Result<()> {
    let cfg = AssignResolutionConfig::default();
    let preserve_cfg = AssignResolutionConfig {
        literal_env_policy: LiteralEnvPolicy::LiteralOnly,
        prefix_policy: AssignPrefixPolicy::PreserveFull,
    };

    assert!(
        resolve_assign_target(
            "F",
            &AssignSource::ExternalName(String::from("  ")),
            cfg,
            &MapEnv::new()
        )
        .is_err()
    );
    assert!(
        resolve_assign_target(
            "F",
            &AssignSource::Literal(String::from("  ")),
            cfg,
            &MapEnv::new()
        )
        .is_err()
    );

    let e1 = MapEnv::new().with_var("DD_MY_FILE", "upper.dat");
    let r1 = resolve_assign_target(
        "F",
        &AssignSource::ExternalName(String::from("UT-S-my-file")),
        cfg,
        &e1,
    )?;
    assert_eq!(r1.origin, AssignResolutionOrigin::DdUpperEnv);
    assert_eq!(
        r1.target,
        ResolvedAssignTarget::FilesystemPath(PathBuf::from("upper.dat"))
    );

    let e2 = MapEnv::new()
        .with_var("DD_FILE2", "   ")
        .with_var("dd_file2", "lower.dat");
    let r2 = resolve_assign_target(
        "F",
        &AssignSource::ExternalName(String::from("AS-FILE2")),
        cfg,
        &e2,
    )?;
    assert_eq!(r2.origin, AssignResolutionOrigin::DdLowerEnv);

    let e3 = MapEnv::new()
        .with_var("dd_file3", " ")
        .with_var("FILE3", "direct_upper.dat");
    let r3 = resolve_assign_target(
        "F",
        &AssignSource::ExternalName(String::from("S-file3")),
        cfg,
        &e3,
    )?;
    assert_eq!(r3.origin, AssignResolutionOrigin::DirectEnv);

    let e4 = MapEnv::new()
        .with_var("MIXEDNAME", " ")
        .with_var("MixedName", "exact_mixed.dat");
    let r4 = resolve_assign_target(
        "F",
        &AssignSource::ExternalName(String::from("MixedName")),
        cfg,
        &e4,
    )?;
    assert_eq!(r4.origin, AssignResolutionOrigin::DirectEnv);

    let e4_blank = MapEnv::new().with_var("MixedName", "   ");
    let r4_def = resolve_assign_target(
        "F",
        &AssignSource::ExternalName(String::from("MixedName")),
        cfg,
        &e4_blank,
    )?;
    assert_eq!(
        r4_def.origin,
        AssignResolutionOrigin::DefaultExternalFilename
    );

    for short_prefix in ["UT-S-", "AS-", "S-"] {
        let r_short = resolve_assign_target(
            "F",
            &AssignSource::ExternalName(String::from(short_prefix)),
            cfg,
            &MapEnv::new(),
        )?;
        assert_eq!(
            r_short.target,
            ResolvedAssignTarget::FilesystemPath(PathBuf::from(short_prefix))
        );
    }

    let r_pres = resolve_assign_target(
        "F",
        &AssignSource::ExternalName(String::from("S-KEEP")),
        preserve_cfg,
        &MapEnv::new(),
    )?;
    assert_eq!(
        r_pres.target,
        ResolvedAssignTarget::FilesystemPath(PathBuf::from("S-KEEP"))
    );
    Ok(())
}

/// Verifies literal `ASSIGN` environment policies and `LineSequentialEnvOptions` flags.
#[test]
fn assign_literal_policies_and_ls_env_options() -> Result<()> {
    let cfg = AssignResolutionConfig::default();
    let preserve_cfg = AssignResolutionConfig {
        literal_env_policy: LiteralEnvPolicy::LiteralOnly,
        prefix_policy: AssignPrefixPolicy::PreserveFull,
    };

    let lit_env = MapEnv::new().with_var("DD_BARELIT", "from_dd.dat");
    let r_lit_dd = resolve_assign_target(
        "F",
        &AssignSource::Literal(String::from("BARELIT")),
        cfg,
        &lit_env,
    )?;
    assert_eq!(r_lit_dd.origin, AssignResolutionOrigin::DdUpperEnv);

    let r_lit_only = resolve_assign_target(
        "F",
        &AssignSource::Literal(String::from("BARELIT")),
        preserve_cfg,
        &lit_env,
    )?;
    assert_eq!(r_lit_only.origin, AssignResolutionOrigin::LiteralPath);

    let r_lit_miss = resolve_assign_target(
        "F",
        &AssignSource::Literal(String::from("UNMAPPED")),
        cfg,
        &lit_env,
    )?;
    assert_eq!(r_lit_miss.origin, AssignResolutionOrigin::LiteralPath);

    let r_slash = resolve_assign_target(
        "F",
        &AssignSource::Literal(String::from("dir/BARELIT")),
        cfg,
        &lit_env,
    )?;
    assert_eq!(r_slash.origin, AssignResolutionOrigin::LiteralPath);

    let win_path: String = [b'd', 0x5C_u8, b'f'].into_iter().map(char::from).collect();
    let r_win_sep = resolve_assign_target("F", &AssignSource::Literal(win_path), cfg, &lit_env)?;
    assert_eq!(r_win_sep.origin, AssignResolutionOrigin::LiteralPath);

    let def_ls = LineSequentialEnvOptions::from_env(&MapEnv::new());
    assert_eq!(
        def_ls.trailing_spaces,
        TrailingSpacePolicy::StripTrailingSpaces
    );
    assert_eq!(def_ls.null_escapes, NullEscapePolicy::RawBytes);

    for truthy in ["1", "y", "YES", "on", " True "] {
        let on_env = MapEnv::new()
            .with_var("COB_LS_FIXED", truthy)
            .with_var("COB_LS_NULLS", truthy);
        let opts = LineSequentialEnvOptions::from_env(&on_env);
        assert_eq!(
            opts.trailing_spaces,
            TrailingSpacePolicy::PreserveFixedSpaces
        );
        assert_eq!(opts.null_escapes, NullEscapePolicy::NullPrefixControlBytes);
    }
    let off_env = MapEnv::new()
        .with_var("COB_LS_FIXED", "0")
        .with_var("COB_LS_NULLS", "no");
    let off_opts = LineSequentialEnvOptions::from_env(&off_env);
    assert_eq!(
        off_opts.trailing_spaces,
        TrailingSpacePolicy::StripTrailingSpaces
    );
    assert_eq!(off_opts.null_escapes, NullEscapePolicy::RawBytes);
    Ok(())
}

/// Verifies all 28 [`FileStatusKind`] entries and `EC-I-O-*` exception class names.
#[test]
fn exhaustive_status_table_and_ec_names() {
    assert_eq!(FileStatusKind::ALL.len(), 28);
    for &kind in FileStatusKind::ALL {
        let desc = status_descriptor(kind);
        assert_eq!(desc.kind, kind);
        assert_eq!(desc.code, kind.code());
        assert_ne!(desc.description, "");
        if kind == FileStatusKind::Success00 {
            assert_eq!(desc.category, StatusCategory::SuccessExact);
            assert!(desc.exception.is_none());
        } else {
            assert!(desc.exception.is_some());
        }
    }

    for (cls, expected) in [
        (IoExceptionClass::Warning, "EC-I-O-WARNING"),
        (IoExceptionClass::AtEnd, "EC-I-O-AT-END"),
        (IoExceptionClass::InvalidKey, "EC-I-O-INVALID-KEY"),
        (IoExceptionClass::PermanentError, "EC-I-O-PERMANENT-ERROR"),
        (IoExceptionClass::LogicError, "EC-I-O-LOGIC-ERROR"),
        (IoExceptionClass::FileSharing, "EC-I-O-FILE-SHARING"),
        (IoExceptionClass::Implementor, "EC-I-O-IMP"),
    ] {
        assert_eq!(cls.ec_name(), expected);
    }

    let mut status_buf = [0_u8; 2];
    write_status_bytes(
        FileStatusKind::SuccessOptionalFile05.code(),
        &mut status_buf,
    );
    assert_eq!(&status_buf, b"05");
}

/// Verifies OS error classifiers and post-I/O handler dispatch matrix.
#[test]
fn exhaustive_io_error_classifiers_and_dispatcher() -> Result<()> {
    assert_eq!(
        classify_io_error(
            &io::Error::new(ErrorKind::NotFound, "x"),
            IoErrorContext::Open
        ),
        FileStatusKind::FileNotFound35
    );
    assert_eq!(
        classify_io_error(
            &io::Error::new(ErrorKind::NotFound, "x"),
            IoErrorContext::GeneralIo
        ),
        FileStatusKind::PermanentError30
    );
    assert_eq!(
        classify_io_error(
            &io::Error::new(ErrorKind::PermissionDenied, "x"),
            IoErrorContext::Open
        ),
        FileStatusKind::OpenPermissionDenied37
    );
    assert_eq!(
        classify_io_error(
            &io::Error::new(ErrorKind::PermissionDenied, "x"),
            IoErrorContext::KeyedWrite
        ),
        FileStatusKind::PermanentError30
    );
    assert_eq!(
        classify_io_error(
            &io::Error::new(ErrorKind::StorageFull, "x"),
            IoErrorContext::SequentialWrite
        ),
        FileStatusKind::SequentialBoundary34
    );
    assert_eq!(
        classify_io_error(
            &io::Error::new(ErrorKind::WriteZero, "x"),
            IoErrorContext::KeyedWrite
        ),
        FileStatusKind::BoundaryViolation24
    );
    assert_eq!(
        classify_io_error(
            &io::Error::new(ErrorKind::FileTooLarge, "x"),
            IoErrorContext::GeneralIo
        ),
        FileStatusKind::PermanentError30
    );
    assert_eq!(
        classify_io_error(
            &io::Error::new(ErrorKind::WouldBlock, "x"),
            IoErrorContext::GeneralIo
        ),
        FileStatusKind::RecordLocked51
    );
    for kind in [
        ErrorKind::UnexpectedEof,
        ErrorKind::InvalidData,
        ErrorKind::BrokenPipe,
    ] {
        assert_eq!(
            classify_io_error(&io::Error::new(kind, "x"), IoErrorContext::GeneralIo),
            FileStatusKind::PermanentError30
        );
    }
    assert_eq!(
        classify_io_error(
            &io::Error::new(ErrorKind::TimedOut, "x"),
            IoErrorContext::GeneralIo
        ),
        FileStatusKind::ImplementationDefined91
    );

    let non_io = CoblangError::SortMerge {
        message: String::from("other"),
    };
    assert_eq!(
        classify_storage_error(&non_io, IoErrorContext::GeneralIo),
        FileStatusKind::PermanentError30
    );

    verify_post_io_dispatch_matrix()
}

/// Exercises every branch of [`resolve_post_io_action`].
fn verify_post_io_dispatch_matrix() -> Result<()> {
    let bare = VerbHandlerConfig {
        at_end_clause: InlineClausePresence::Absent,
        invalid_key_clause: InlineClausePresence::Absent,
        file_status_item: StatusClausePresence::Undeclared,
        declarative: DeclarativePresence::None,
    };
    assert_eq!(
        resolve_post_io_action("F", FileStatusKind::Success00, bare)?,
        PostIoDispatch::NormalCompletion
    );
    assert_eq!(
        resolve_post_io_action("F", FileStatusKind::SuccessOptionalFile05, bare)?,
        PostIoDispatch::NormalCompletion
    );
    assert!(resolve_post_io_action("F", FileStatusKind::AtEnd10, bare).is_err());
    assert!(resolve_post_io_action("F", FileStatusKind::DuplicateKey22, bare).is_err());
    assert!(resolve_post_io_action("F", FileStatusKind::FileNotFound35, bare).is_err());

    let inline_both = VerbHandlerConfig {
        at_end_clause: InlineClausePresence::Present,
        invalid_key_clause: InlineClausePresence::Present,
        ..bare
    };
    assert_eq!(
        resolve_post_io_action("F", FileStatusKind::RelativeKeyOverflow14, inline_both)?,
        PostIoDispatch::FireAtEnd
    );
    assert_eq!(
        resolve_post_io_action("F", FileStatusKind::RecordNotFound23, inline_both)?,
        PostIoDispatch::FireInvalidKey
    );

    let with_decl = VerbHandlerConfig {
        declarative: DeclarativePresence::MatchingUseProcedure,
        ..bare
    };
    for kind in [
        FileStatusKind::AtEnd10,
        FileStatusKind::InvalidKeySequence21,
        FileStatusKind::PermanentError30,
        FileStatusKind::AlreadyOpen41,
        FileStatusKind::RecordLocked51,
        FileStatusKind::FileSharingConflict61,
        FileStatusKind::ImplementationDefined91,
    ] {
        assert_eq!(
            resolve_post_io_action("F", kind, with_decl)?,
            PostIoDispatch::InvokeDeclarative
        );
    }

    let with_fs = VerbHandlerConfig {
        file_status_item: StatusClausePresence::Declared,
        ..bare
    };
    assert_eq!(
        resolve_post_io_action("F", FileStatusKind::FileNotFound35, with_fs)?,
        PostIoDispatch::AbsorbViaFileStatus
    );
    Ok(())
}

/// Verifies [`ConnectorStateMachine`] mode matrices, reel media `"07"`/`"00"`, and cursor transitions.
#[test]
fn connector_state_machine_mode_and_reel_matrix() {
    let mut bad_seq = ConnectorStateMachine::new(
        FileOrganization::Sequential,
        AccessMode::Random,
        ReelMediumKind::NonReelDisk,
    );
    assert_eq!(
        bad_seq.precheck_open(OpenMode::Input),
        PrecheckOutcome::Rejected(FileStatusKind::OpenPermissionDenied37)
    );

    let mut bad_ls_access = ConnectorStateMachine::new(
        FileOrganization::LineSequential,
        AccessMode::Dynamic,
        ReelMediumKind::NonReelDisk,
    );
    assert_eq!(
        bad_ls_access.precheck_open(OpenMode::Input),
        PrecheckOutcome::Rejected(FileStatusKind::OpenPermissionDenied37)
    );

    let mut idx_rand = ConnectorStateMachine::new(
        FileOrganization::Indexed,
        AccessMode::Random,
        ReelMediumKind::ReelTape,
    );
    assert_eq!(
        idx_rand.precheck_open(OpenMode::Extend),
        PrecheckOutcome::Rejected(FileStatusKind::OpenPermissionDenied37)
    );
    assert_eq!(
        idx_rand.precheck_sequential_read(),
        PrecheckOutcome::Rejected(FileStatusKind::ReadWrongOpenMode47)
    );
    assert_eq!(
        idx_rand.precheck_start(),
        PrecheckOutcome::Rejected(FileStatusKind::ReadWrongOpenMode47)
    );
    assert_eq!(
        idx_rand.precheck_random_read(),
        PrecheckOutcome::Rejected(FileStatusKind::ReadWrongOpenMode47)
    );
    assert_eq!(
        idx_rand.precheck_open(OpenMode::Output),
        PrecheckOutcome::Allowed
    );
    idx_rand.commit_open(OpenMode::Output, FileStatusKind::Success00);
    assert_eq!(
        idx_rand.lifecycle(),
        ConnectorLifecycle::Open(OpenMode::Output)
    );
    assert_eq!(idx_rand.cursor(), SequentialCursorState::Positioned);
    assert_eq!(idx_rand.prior_read(), None);
    assert_eq!(idx_rand.last_status_kind(), FileStatusKind::Success00);
    assert_eq!(
        idx_rand.precheck_random_read(),
        PrecheckOutcome::Rejected(FileStatusKind::ReadWrongOpenMode47)
    );
    assert_eq!(idx_rand.precheck_write(), PrecheckOutcome::Allowed);
    assert_eq!(
        idx_rand.transition_close(CloseDisposition::ReelUnit),
        CloseTransitionOutcome::RetainedReelOpen(FileStatusKind::Success00.code())
    );
    assert_eq!(
        idx_rand.transition_close(CloseDisposition::NoRewind),
        CloseTransitionOutcome::ClosedHandle(FileStatusKind::Success00.code())
    );
    assert_eq!(
        idx_rand.transition_close(CloseDisposition::Normal),
        CloseTransitionOutcome::NotOpen(FileStatusKind::NotOpenOnClose42.code())
    );

    idx_rand.commit_open(OpenMode::Input, FileStatusKind::Success00);
    assert_eq!(
        idx_rand.precheck_write(),
        PrecheckOutcome::Rejected(FileStatusKind::WriteWrongOpenMode48)
    );
    assert_eq!(idx_rand.precheck_random_read(), PrecheckOutcome::Allowed);
    let rec = PriorReadRecord {
        position_key: 1,
        payload_len: 4,
        framed_len: 4,
    };
    idx_rand.commit_read_success(rec, FileStatusKind::Success00);
    assert_eq!(idx_rand.prior_read(), Some(rec));
    let _ = idx_rand.transition_close(CloseDisposition::WithLock);
    assert_eq!(
        idx_rand.precheck_open(OpenMode::Input),
        PrecheckOutcome::Rejected(FileStatusKind::ClosedWithLock38)
    );
}

/// Verifies sequential [`ConnectorStateMachine`] non-reel `"07"` and precheck rejections.
#[test]
fn connector_state_machine_sequential_non_reel_and_prechecks() {
    let mut seq_sm = ConnectorStateMachine::new(
        FileOrganization::Sequential,
        AccessMode::Sequential,
        ReelMediumKind::NonReelDisk,
    );
    assert_eq!(
        seq_sm.precheck_random_read(),
        PrecheckOutcome::Rejected(FileStatusKind::ReadWrongOpenMode47)
    );
    assert_eq!(
        seq_sm.precheck_start(),
        PrecheckOutcome::Rejected(FileStatusKind::ReadWrongOpenMode47)
    );
    seq_sm.commit_open(OpenMode::Io, FileStatusKind::Success00);
    assert_eq!(
        seq_sm.precheck_rewrite_or_delete(),
        MutationPrecheckOutcome::Rejected(FileStatusKind::NoPriorRead43)
    );
    assert_eq!(
        seq_sm.transition_close(CloseDisposition::ReelUnit),
        CloseTransitionOutcome::RetainedReelOpen(FileStatusKind::SuccessNonReel07.code())
    );
    assert_eq!(
        seq_sm.transition_close(CloseDisposition::NoRewind),
        CloseTransitionOutcome::ClosedHandle(FileStatusKind::SuccessNonReel07.code())
    );
}
