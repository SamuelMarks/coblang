//! Comprehensive integration and fault-injection tests for `SequentialFileEngine`,
//! `LineSequentialFileEngine`, and `RelativeFileEngine` (`Milestone M14`).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use coblang_common::{DigitCount, FileConnectorId, FileStatusKind, Result};
use coblang_fileio::{
    AccessMode, AdvancingPhase, AdvancingSpec, CloseDisposition, FileLockRegistry, FileOptionality,
    FixedAlignmentPolicy, InMemoryFileSystem, LinageParams, LineSequentialConfig,
    LineSequentialFileEngine, LineTerminator, LockMode, NullEscapePolicy, OpenMode,
    PageOverflowOutcome, ReadDirection, ReadLockPolicy, RecordLayout, RelativeFileConfig,
    RelativeFileEngine, RelativeSlotKey, SequentialFileConfig, SequentialFileEngine, StartRelation,
    TrailingSpacePolicy, VaryingHeaderFormat,
};

/// Monotonic counter for unique OS temporary paths in engine round-trip tests.
static ENGINE_TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Creates a unique OS temporary path.
///
/// # Arguments
///
/// * `label` - Descriptive test file label.
fn temp_engine_path(label: &str) -> PathBuf {
    let seq = ENGINE_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("coblang_eng_{pid}_{seq}_{label}.dat"))
}

/// Tests [`SequentialFileEngine`] on [`coblang_fileio::OsFileSystem`] + fixed, varying, `REWRITE`, and `DELETE FILE`.
#[test]
fn sequential_engine_fixed_varying_rewrite_and_os_roundtrip() -> Result<()> {
    let os_path = temp_engine_path("seq_os");
    let _ = std::fs::remove_file(&os_path);

    let cfg = SequentialFileConfig::new(
        "SEQ-OS",
        FileConnectorId::new(1),
        RecordLayout::fixed_spaces(4)?,
    );
    let mut os_eng = SequentialFileEngine::new(cfg);
    assert_eq!(os_eng.last_status(), FileStatusKind::Success00.code());
    assert_eq!(os_eng.last_status_kind(), FileStatusKind::Success00);
    assert_eq!(os_eng.open(&os_path, OpenMode::Output).as_str(), "00");
    assert_eq!(os_eng.open(&os_path, OpenMode::Output).as_str(), "41");
    assert_eq!(os_eng.delete_file(&os_path).as_str(), "41");
    assert_eq!(
        os_eng
            .write_record(b"REC1", AdvancingSpec::NoneRaw)
            .status
            .as_str(),
        "00"
    );
    assert_eq!(
        os_eng
            .write_record(b"REC2", AdvancingSpec::NoneRaw)
            .status
            .as_str(),
        "00"
    );
    assert_eq!(
        os_eng
            .write_record(b"BAD", AdvancingSpec::NoneRaw)
            .status
            .as_str(),
        "44"
    );
    assert_eq!(os_eng.read_next().status.as_str(), "47");
    assert_eq!(os_eng.rewrite_record(b"XXXX").as_str(), "49");
    assert_eq!(os_eng.close(CloseDisposition::Normal).as_str(), "00");
    assert_eq!(os_eng.close(CloseDisposition::Normal).as_str(), "42");

    assert_eq!(os_eng.open(&os_path, OpenMode::Io).as_str(), "00");
    assert_eq!(
        os_eng
            .write_record(b"NOPE", AdvancingSpec::NoneRaw)
            .status
            .as_str(),
        "48"
    );
    assert_eq!(os_eng.rewrite_record(b"XXXX").as_str(), "43");
    let r1 = os_eng.read_next();
    assert_eq!(r1.status.as_str(), "00");
    assert_eq!(r1.record.as_deref(), Some(b"REC1".as_slice()));
    assert_eq!(os_eng.rewrite_record(b"BAD").as_str(), "44");
    assert_eq!(os_eng.rewrite_record(b"UPD1").as_str(), "00");
    assert_eq!(os_eng.rewrite_record(b"UPD1").as_str(), "43");
    let r2 = os_eng.read_next();
    assert_eq!(r2.record.as_deref(), Some(b"REC2".as_slice()));
    assert_eq!(os_eng.read_next().status.as_str(), "10");
    assert_eq!(os_eng.read_next().status.as_str(), "46");
    assert_eq!(os_eng.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(os_eng.delete_file(&os_path).as_str(), "00");
    assert_eq!(os_eng.delete_file(&os_path).as_str(), "35");
    Ok(())
}

/// Tests [`SequentialFileEngine`] varying 2-byte/4-byte headers, optional `"05"`, `"39"`, advancing, and `LINAGE`.
#[test]
fn sequential_engine_varying_optional_alignment_and_linage() -> Result<()> {
    let fs = InMemoryFileSystem::new();
    let locks = FileLockRegistry::new();
    let p_var2 = Path::new("/seq/var2.dat");

    let cfg2 = SequentialFileConfig::new(
        "VAR2",
        FileConnectorId::new(10),
        RecordLayout::varying(2, 8, VaryingHeaderFormat::TwoByteBigEndian)?,
    );
    let mut e2 = SequentialFileEngine::with_backend(cfg2, Box::new(fs.clone()), locks.clone());
    assert_eq!(e2.open(p_var2, OpenMode::Input).as_str(), "35");
    assert_eq!(e2.open(p_var2, OpenMode::Output).as_str(), "00");
    assert_eq!(
        e2.write_record(b"AB", AdvancingSpec::NoneRaw)
            .status
            .as_str(),
        "00"
    );
    assert_eq!(
        e2.write_record(b"ABCDEFGH", AdvancingSpec::NoneRaw)
            .status
            .as_str(),
        "00"
    );
    assert_eq!(e2.close(CloseDisposition::NoRewind).as_str(), "07");

    assert_eq!(e2.open(p_var2, OpenMode::Input).as_str(), "00");
    assert_eq!(e2.read_next().actual_len, Some(2));
    assert_eq!(e2.read_next().actual_len, Some(8));
    assert_eq!(e2.read_next().status.as_str(), "10");
    assert_eq!(e2.close(CloseDisposition::ReelUnit).as_str(), "07");
    assert_eq!(e2.close(CloseDisposition::WithLock).as_str(), "00");
    assert_eq!(e2.open(p_var2, OpenMode::Input).as_str(), "38");

    let p_var4 = Path::new("/seq/var4.dat");
    let mut cfg4 = SequentialFileConfig::new(
        "VAR4",
        FileConnectorId::new(11),
        RecordLayout::varying(3, 6, VaryingHeaderFormat::FourByteBigEndian)?,
    );
    cfg4.optionality = FileOptionality::Optional;
    let mut e4 = SequentialFileEngine::with_backend(cfg4, Box::new(fs.clone()), locks.clone());
    assert_eq!(e4.open(p_var4, OpenMode::Input).as_str(), "05");
    assert_eq!(e4.read_next().status.as_str(), "10");
    assert_eq!(e4.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(e4.open(p_var4, OpenMode::Io).as_str(), "05");
    assert_eq!(e4.close(CloseDisposition::Normal).as_str(), "00");
    let p_opt_ext = Path::new("/seq/opt_ext.dat");
    assert_eq!(e4.open(p_opt_ext, OpenMode::Extend).as_str(), "05");
    assert_eq!(
        e4.write_record(b"1234", AdvancingSpec::NoneRaw)
            .status
            .as_str(),
        "00"
    );
    assert_eq!(e4.close(CloseDisposition::Normal).as_str(), "00");
    assert_eq!(e4.open(p_opt_ext, OpenMode::Extend).as_str(), "00");
    assert_eq!(
        e4.write_record(b"567", AdvancingSpec::NoneRaw)
            .status
            .as_str(),
        "00"
    );
    assert_eq!(e4.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(e4.open(p_opt_ext, OpenMode::Io).as_str(), "00");
    assert_eq!(e4.read_next().record.as_deref(), Some(b"1234".as_slice()));
    assert_eq!(e4.rewrite_record(b"WXYZ").as_str(), "00");
    assert_eq!(e4.read_next().record.as_deref(), Some(b"567".as_slice()));
    assert_eq!(e4.close(CloseDisposition::Normal).as_str(), "00");

    verify_sequential_corrupt_linage_and_faults(&fs, &locks)
}

/// Tests [`SequentialFileEngine`] `"04"`, `"30"`, `"34"`, `"39"`, `"61"`, `ADVANCING`, `LINAGE`, and I/O faults.
///
/// # Arguments
///
/// * `fs` - Shared in-memory filesystem.
/// * `locks` - Shared file-lock registry.
fn verify_sequential_corrupt_linage_and_faults(
    fs: &InMemoryFileSystem,
    locks: &FileLockRegistry,
) -> Result<()> {
    let p_short = Path::new("/seq/short.dat");
    fs.seed_file(p_short, b"ABCDEF");
    let mut cfg_strict = SequentialFileConfig::new(
        "STRICT",
        FileConnectorId::new(12),
        RecordLayout::fixed_spaces(4)?,
    );
    cfg_strict.alignment_policy = FixedAlignmentPolicy::RequireExactMultiple39;
    let mut e_strict =
        SequentialFileEngine::with_backend(cfg_strict.clone(), Box::new(fs.clone()), locks.clone());
    assert_eq!(e_strict.open(p_short, OpenMode::Input).as_str(), "39");

    fs.inject_fault_after(1, ErrorKind::InvalidData);
    assert_eq!(e_strict.open(p_short, OpenMode::Input).as_str(), "30");

    let p_aligned = Path::new("/seq/aligned.dat");
    fs.seed_file(p_aligned, b"ABCD1234");
    assert_eq!(e_strict.open(p_aligned, OpenMode::Input).as_str(), "00");
    assert_eq!(e_strict.close(CloseDisposition::Normal).as_str(), "00");

    cfg_strict.alignment_policy = FixedAlignmentPolicy::AllowShortFinalRecord04;
    let mut e_allow =
        SequentialFileEngine::with_backend(cfg_strict, Box::new(fs.clone()), locks.clone());
    assert_eq!(e_allow.open(p_short, OpenMode::Input).as_str(), "00");
    assert_eq!(e_allow.read_next().status.as_str(), "00");
    let short_out = e_allow.read_next();
    assert_eq!(short_out.status.as_str(), "04");
    assert_eq!(short_out.record.as_deref(), Some(b"EF  ".as_slice()));
    assert_eq!(e_allow.close(CloseDisposition::Normal).as_str(), "00");

    let p_bad_var = Path::new("/seq/bad_var.dat");
    fs.seed_file(
        p_bad_var,
        &[
            0x00, 0x01, b'X', 0x00, 0x09, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0x00,
        ],
    );
    let mut e_bv = SequentialFileEngine::with_backend(
        SequentialFileConfig::new(
            "BV",
            FileConnectorId::new(13),
            RecordLayout::varying(2, 4, VaryingHeaderFormat::TwoByteBigEndian)?,
        ),
        Box::new(fs.clone()),
        locks.clone(),
    );
    assert_eq!(e_bv.open(p_bad_var, OpenMode::Input).as_str(), "00");
    assert_eq!(e_bv.read_next().status.as_str(), "04");
    let over_var = e_bv.read_next();
    assert_eq!(over_var.status.as_str(), "04");
    assert_eq!(over_var.actual_len, Some(4));
    assert_eq!(e_bv.read_next().status.as_str(), "30");
    assert_eq!(e_bv.close(CloseDisposition::Normal).as_str(), "00");

    let p_trunc_pay = Path::new("/seq/trunc_pay.dat");
    fs.seed_file(p_trunc_pay, &[0x00, 0x04, b'A']);
    assert_eq!(e_bv.open(p_trunc_pay, OpenMode::Input).as_str(), "00");
    assert_eq!(e_bv.read_next().status.as_str(), "30");
    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(e_bv.read_next().status.as_str(), "30");
    fs.inject_fault_after(1, ErrorKind::BrokenPipe);
    assert_eq!(e_bv.read_next().status.as_str(), "30");
    assert_eq!(e_bv.close(CloseDisposition::Normal).as_str(), "00");

    verify_sequential_advancing_linage_and_sharing(fs, locks)
}

/// Tests [`SequentialFileEngine`] non-`LINAGE` advancing, `LINAGE` lifecycle, sharing `"61"`, and write/close faults.
///
/// # Arguments
///
/// * `fs` - Shared in-memory filesystem.
/// * `locks` - Shared file-lock registry.
fn verify_sequential_advancing_linage_and_sharing(
    fs: &InMemoryFileSystem,
    locks: &FileLockRegistry,
) -> Result<()> {
    let p_adv = Path::new("/seq/adv.dat");
    let mut cfg_adv = SequentialFileConfig::new(
        "ADV",
        FileConnectorId::new(14),
        RecordLayout::fixed_spaces(2)?,
    );
    cfg_adv.lock_mode = LockMode::Exclusive;
    let mut e_adv =
        SequentialFileEngine::with_backend(cfg_adv, Box::new(fs.clone()), locks.clone());
    assert_eq!(e_adv.open(p_adv, OpenMode::Output).as_str(), "00");

    let mut e_contend = SequentialFileEngine::with_backend(
        SequentialFileConfig::new(
            "C2",
            FileConnectorId::new(15),
            RecordLayout::fixed_spaces(2)?,
        ),
        Box::new(fs.clone()),
        locks.clone(),
    );
    assert_eq!(e_contend.open(p_adv, OpenMode::Input).as_str(), "61");

    for spec in [
        AdvancingSpec::DefaultSingleLine,
        AdvancingSpec::Lines {
            phase: AdvancingPhase::Before,
            count: 0,
        },
        AdvancingSpec::Lines {
            phase: AdvancingPhase::Before,
            count: 2,
        },
        AdvancingSpec::Lines {
            phase: AdvancingPhase::After,
            count: 0,
        },
        AdvancingSpec::Lines {
            phase: AdvancingPhase::After,
            count: 1,
        },
        AdvancingSpec::Page {
            phase: AdvancingPhase::Before,
        },
        AdvancingSpec::Page {
            phase: AdvancingPhase::After,
        },
    ] {
        assert_eq!(e_adv.write_record(b"XY", spec).status.as_str(), "00");
    }
    fs.inject_fault_after(0, ErrorKind::StorageFull);
    assert_eq!(
        e_adv
            .write_record(b"XY", AdvancingSpec::NoneRaw)
            .status
            .as_str(),
        "34"
    );
    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(e_adv.close(CloseDisposition::Normal).as_str(), "30");

    fs.mark_read_only(p_adv);
    assert_eq!(e_adv.open(p_adv, OpenMode::Output).as_str(), "37");
    verify_sequential_linage_engine(fs, locks)
}

/// Exercises `LINAGE` page-overflow and fault paths on [`SequentialFileEngine`].
///
/// # Arguments
///
/// * `fs` - Shared in-memory filesystem.
/// * `locks` - Shared file-lock registry.
fn verify_sequential_linage_engine(
    fs: &InMemoryFileSystem,
    locks: &FileLockRegistry,
) -> Result<()> {
    let p_lin = Path::new("/seq/lin.dat");
    let mut cfg_lin = SequentialFileConfig::new(
        "LIN",
        FileConnectorId::new(16),
        RecordLayout::fixed_spaces(3)?,
    );
    cfg_lin.linage = Some(LinageParams::try_new(2, Some(2), 1, 1)?);
    let mut e_lin =
        SequentialFileEngine::with_backend(cfg_lin, Box::new(fs.clone()), locks.clone());
    e_lin.set_next_linage_params(LinageParams::try_new(2, Some(2), 1, 1)?);
    assert_eq!(e_lin.linage_counter(), None);
    assert_eq!(e_lin.open(p_lin, OpenMode::Io).as_str(), "37");

    fs.inject_fault_after(1, ErrorKind::StorageFull);
    assert_eq!(e_lin.open(p_lin, OpenMode::Output).as_str(), "34");

    assert_eq!(e_lin.open(p_lin, OpenMode::Output).as_str(), "00");
    assert_eq!(e_lin.linage_counter(), Some(1));
    e_lin.set_next_linage_params(LinageParams::try_new(3, None, 0, 0)?);
    let w_eop = e_lin.write_record(b"AAA", AdvancingSpec::DefaultSingleLine);
    assert_eq!(w_eop.status.as_str(), "00");
    assert_eq!(w_eop.page_overflow, PageOverflowOutcome::EndOfPageReached);
    assert_eq!(w_eop.linage_counter, Some(2));

    fs.inject_fault_after(0, ErrorKind::StorageFull);
    assert_eq!(
        e_lin
            .write_record(b"BBB", AdvancingSpec::DefaultSingleLine)
            .status
            .as_str(),
        "34"
    );

    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(e_lin.close(CloseDisposition::Normal).as_str(), "30");

    assert_eq!(e_lin.open(p_lin, OpenMode::Output).as_str(), "00");
    assert_eq!(
        e_lin
            .write_record(
                b"CCC",
                AdvancingSpec::Lines {
                    phase: AdvancingPhase::Before,
                    count: 1
                }
            )
            .status
            .as_str(),
        "00"
    );
    assert_eq!(e_lin.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(e_allow_read_faults(fs, locks)?.as_str(), "00");
    Ok(())
}

/// Exercises fixed sequential read and rewrite I/O fault branches.
///
/// # Arguments
///
/// * `fs` - Shared in-memory filesystem.
/// * `locks` - Shared file-lock registry.
fn e_allow_read_faults(
    fs: &InMemoryFileSystem,
    locks: &FileLockRegistry,
) -> Result<coblang_common::FileStatusCode> {
    let p = Path::new("/seq/fault_rw.dat");
    fs.seed_file(p, b"12345678");
    let mut eng = SequentialFileEngine::with_backend(
        SequentialFileConfig::new(
            "FRW",
            FileConnectorId::new(17),
            RecordLayout::fixed_spaces(4)?,
        ),
        Box::new(fs.clone()),
        locks.clone(),
    );
    assert_eq!(eng.open(p, OpenMode::Io).as_str(), "00");
    fs.inject_fault_after(1, ErrorKind::BrokenPipe);
    assert_eq!(eng.read_next().status.as_str(), "30");
    assert_eq!(eng.read_next().status.as_str(), "00");
    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(eng.rewrite_record(b"ABCD").as_str(), "30");
    Ok(eng.close(CloseDisposition::Normal))
}

/// Tests [`LineSequentialFileEngine`] `LF`/`CRLF`/`CR`, `COB_LS_FIXED`, `COB_LS_NULLS`, varying, and faults.
#[test]
fn line_sequential_engine_framing_nulls_fixed_varying_and_faults() -> Result<()> {
    let os_path = temp_engine_path("ls_os");
    let _ = std::fs::remove_file(&os_path);

    let mut os_cfg = LineSequentialConfig::new(
        "LS-OS",
        FileConnectorId::new(20),
        RecordLayout::fixed_spaces(6)?,
    );
    os_cfg.write_terminator = LineTerminator::CrLf;
    let mut os_eng = LineSequentialFileEngine::new(os_cfg);
    assert_eq!(os_eng.last_status().as_str(), "00");
    assert_eq!(os_eng.open(&os_path, OpenMode::Io).as_str(), "37");
    assert_eq!(os_eng.open(&os_path, OpenMode::Output).as_str(), "00");
    assert_eq!(
        os_eng
            .write_record(b"HI    ", AdvancingSpec::DefaultSingleLine)
            .as_str(),
        "00"
    );
    assert_eq!(
        os_eng
            .write_record(b"      ", AdvancingSpec::DefaultSingleLine)
            .as_str(),
        "00"
    );
    assert_eq!(
        os_eng
            .write_record(b"TOOLONG", AdvancingSpec::DefaultSingleLine)
            .as_str(),
        "44"
    );
    assert_eq!(os_eng.rewrite_unsupported().as_str(), "49");
    assert_eq!(os_eng.close(CloseDisposition::Normal).as_str(), "00");
    assert_eq!(
        std::fs::read(&os_path)?,
        vec![b'H', b'I', 0x0D, 0x0A, 0x0D, 0x0A]
    );

    assert_eq!(os_eng.open(&os_path, OpenMode::Input).as_str(), "00");
    assert_eq!(
        os_eng
            .write_record(b"ABCDEF", AdvancingSpec::DefaultSingleLine)
            .as_str(),
        "48"
    );
    let l1 = os_eng.read_next();
    assert_eq!(l1.status.as_str(), "00");
    assert_eq!(l1.record.as_deref(), Some(b"HI    ".as_slice()));
    let l2 = os_eng.read_next();
    assert_eq!(l2.record.as_deref(), Some(b"      ".as_slice()));
    assert_eq!(os_eng.read_next().status.as_str(), "10");
    assert_eq!(os_eng.read_next().status.as_str(), "46");
    assert_eq!(os_eng.close(CloseDisposition::Normal).as_str(), "00");
    let _ = std::fs::remove_file(&os_path);

    verify_line_sequential_nulls_varying_and_faults()
}

/// Exercises `COB_LS_FIXED`, `COB_LS_NULLS`, `OPTIONAL`, `ADVANCING`, and write faults.
fn verify_line_sequential_nulls_varying_and_faults() -> Result<()> {
    let fs = InMemoryFileSystem::new();
    let locks = FileLockRegistry::new();
    let p_nulls = Path::new("/ls/nulls.txt");

    let mut cfg = LineSequentialConfig::new(
        "LS-NULLS",
        FileConnectorId::new(21),
        RecordLayout::varying(3, 6, VaryingHeaderFormat::TwoByteBigEndian)?,
    );
    cfg.trailing_spaces = TrailingSpacePolicy::PreserveFixedSpaces;
    cfg.null_escapes = NullEscapePolicy::NullPrefixControlBytes;
    cfg.optionality = FileOptionality::Optional;

    let mut eng = LineSequentialFileEngine::with_backend(cfg, Box::new(fs.clone()), locks.clone());
    assert_eq!(eng.open(p_nulls, OpenMode::Input).as_str(), "05");
    assert_eq!(eng.read_next().status.as_str(), "10");
    assert_eq!(eng.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(eng.open(p_nulls, OpenMode::Extend).as_str(), "05");
    let binary_rec = [0x00, 0x0A, 0x0D, b'A', b' '];
    assert_eq!(
        eng.write_record(&binary_rec, AdvancingSpec::DefaultSingleLine)
            .as_str(),
        "00"
    );
    assert_eq!(eng.close(CloseDisposition::ReelUnit).as_str(), "07");
    assert_eq!(eng.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(eng.open(p_nulls, OpenMode::Extend).as_str(), "00");
    for spec in [
        AdvancingSpec::NoneRaw,
        AdvancingSpec::Lines {
            phase: AdvancingPhase::Before,
            count: 0,
        },
        AdvancingSpec::Lines {
            phase: AdvancingPhase::Before,
            count: 2,
        },
        AdvancingSpec::Lines {
            phase: AdvancingPhase::After,
            count: 0,
        },
        AdvancingSpec::Lines {
            phase: AdvancingPhase::After,
            count: 1,
        },
        AdvancingSpec::Page {
            phase: AdvancingPhase::Before,
        },
        AdvancingSpec::Page {
            phase: AdvancingPhase::After,
        },
    ] {
        assert_eq!(eng.write_record(b"XYZ", spec).as_str(), "00");
    }
    fs.inject_fault_after(0, ErrorKind::StorageFull);
    assert_eq!(
        eng.write_record(b"XYZ", AdvancingSpec::DefaultSingleLine)
            .as_str(),
        "34"
    );
    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(eng.close(CloseDisposition::Normal).as_str(), "30");
    verify_line_sequential_cr_and_lock_faults(&mut eng, &fs, &locks)
}

/// Exercises bare `CR` lookahead, truncated `NUL` escape, exclusive locks, and read-only open on `LineSequentialFileEngine`.
///
/// # Arguments
///
/// * `eng` - Configured varying `LineSequentialFileEngine`.
/// * `fs` - Shared in-memory filesystem.
/// * `locks` - Shared file-lock registry.
fn verify_line_sequential_cr_and_lock_faults(
    eng: &mut LineSequentialFileEngine,
    fs: &InMemoryFileSystem,
    locks: &FileLockRegistry,
) -> Result<()> {
    let p_cr = Path::new("/ls/cr_mix.txt");
    fs.seed_file(
        p_cr,
        &[
            b'A', 0x0D, b'B', b'C', b'D', b'E', b'F', b'G', b'H', 0x0A, b'T', b'A', b'I', b'L',
        ],
    );
    assert_eq!(eng.open(p_cr, OpenMode::Input).as_str(), "00");
    let r_short = eng.read_next();
    assert_eq!(r_short.status.as_str(), "00");
    assert_eq!(r_short.record.as_deref(), Some(b"A  ".as_slice()));
    let r_long = eng.read_next();
    assert_eq!(r_long.status.as_str(), "04");
    assert_eq!(r_long.record.as_deref(), Some(b"BCDEFG".as_slice()));
    let r_tail = eng.read_next();
    assert_eq!(r_tail.record.as_deref(), Some(b"TAIL".as_slice()));
    assert_eq!(eng.close(CloseDisposition::Normal).as_str(), "00");

    let p_fix_over = Path::new("/ls/fix_over.txt");
    fs.seed_file(p_fix_over, &[b'1', b'2', b'3', b'4', b'5', b'6', 0x0D]);
    let mut e_fix = LineSequentialFileEngine::with_backend(
        LineSequentialConfig::new(
            "LFX",
            FileConnectorId::new(22),
            RecordLayout::fixed_spaces(3)?,
        ),
        Box::new(fs.clone()),
        locks.clone(),
    );
    assert_eq!(
        e_fix
            .open(Path::new("/ls/missing_req.txt"), OpenMode::Input)
            .as_str(),
        "35"
    );
    assert_eq!(e_fix.open(p_fix_over, OpenMode::Input).as_str(), "00");
    assert_eq!(e_fix.read_next().status.as_str(), "04");
    assert_eq!(e_fix.close(CloseDisposition::Normal).as_str(), "00");

    let p_trunc_null = Path::new("/ls/trunc_null.txt");
    fs.seed_file(p_trunc_null, &[b'A', 0x00]);
    assert_eq!(eng.open(p_trunc_null, OpenMode::Input).as_str(), "00");
    assert_eq!(eng.read_next().status.as_str(), "30");
    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(eng.read_next().status.as_str(), "30");
    assert_eq!(eng.close(CloseDisposition::Normal).as_str(), "00");

    let mut cfg_excl = LineSequentialConfig::new(
        "LEX",
        FileConnectorId::new(23),
        RecordLayout::fixed_spaces(3)?,
    );
    cfg_excl.lock_mode = LockMode::Exclusive;
    let mut e_ex =
        LineSequentialFileEngine::with_backend(cfg_excl, Box::new(fs.clone()), locks.clone());
    assert_eq!(e_ex.open(p_fix_over, OpenMode::Input).as_str(), "00");
    assert_eq!(e_fix.open(p_fix_over, OpenMode::Input).as_str(), "61");
    assert_eq!(e_ex.close(CloseDisposition::Normal).as_str(), "00");

    fs.mark_read_only(p_fix_over);
    assert_eq!(e_fix.open(p_fix_over, OpenMode::Output).as_str(), "37");
    assert_eq!(e_fix.close(CloseDisposition::Normal).as_str(), "42");
    Ok(())
}

/// Tests [`RelativeFileEngine`] sequential mode (`OUTPUT`, `EXTEND`, `I-O`, `"14"`, `"24"`, `"43"`).
#[test]
fn relative_engine_sequential_access_and_digit_bounds() -> Result<()> {
    let os_path = temp_engine_path("rel_os");
    let _ = std::fs::remove_file(&os_path);

    let mut cfg = RelativeFileConfig::new(
        "REL-SEQ",
        FileConnectorId::new(30),
        AccessMode::Sequential,
        RecordLayout::fixed_spaces(4)?,
    );
    cfg.key_digits = Some(DigitCount::try_new(1)?);
    cfg.max_slots = Some(9);

    let mut os_eng = RelativeFileEngine::new(cfg.clone());
    assert_eq!(os_eng.last_status().as_str(), "00");
    assert_eq!(os_eng.unlock_records().as_str(), "00");
    assert_eq!(os_eng.write_sequential(b"NOPE").status.as_str(), "48");
    assert_eq!(os_eng.open(&os_path, OpenMode::Input).as_str(), "35");
    assert_eq!(os_eng.open(&os_path, OpenMode::Output).as_str(), "00");
    assert_eq!(os_eng.write_keyed(1, b"NOPE").status.as_str(), "48");
    assert_eq!(os_eng.write_sequential(b"BAD").status.as_str(), "44");

    for i in 1_u8..=9_u8 {
        let rec = [b'R', b'0' + i, b' ', b' '];
        let w = os_eng.write_sequential(&rec);
        assert_eq!(w.status.as_str(), "00");
        assert_eq!(w.slot_key, Some(RelativeSlotKey::try_new(u64::from(i))?));
    }
    assert_eq!(os_eng.write_sequential(b"OVER").status.as_str(), "24");
    assert_eq!(os_eng.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(os_eng.open(&os_path, OpenMode::Io).as_str(), "00");
    assert_eq!(os_eng.write_sequential(b"NOPE").status.as_str(), "48");
    assert_eq!(os_eng.rewrite_record(None, b"UPD1").as_str(), "43");
    assert_eq!(os_eng.delete_record(None).as_str(), "43");
    let r1 = os_eng.read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock);
    assert_eq!(r1.status.as_str(), "00");
    assert_eq!(r1.slot_key, Some(RelativeSlotKey::try_new(1)?));
    assert_eq!(os_eng.rewrite_record(None, b"UPD1").as_str(), "00");
    let r2 = os_eng.read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock);
    assert_eq!(r2.slot_key, Some(RelativeSlotKey::try_new(2)?));
    assert_eq!(os_eng.delete_record(None).as_str(), "00");
    assert_eq!(os_eng.close(CloseDisposition::Normal).as_str(), "00");

    cfg.key_digits = None;
    cfg.max_slots = Some(10);
    let mut ext_eng = RelativeFileEngine::new(cfg);
    assert_eq!(ext_eng.open(&os_path, OpenMode::Extend).as_str(), "00");
    let w10 = ext_eng.write_sequential(b"TEN!");
    assert_eq!(w10.status.as_str(), "00");
    assert_eq!(w10.slot_key, Some(RelativeSlotKey::try_new(10)?));
    assert_eq!(ext_eng.write_sequential(b"ELEV").status.as_str(), "24");
    assert_eq!(ext_eng.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(os_eng.open(&os_path, OpenMode::Input).as_str(), "00");
    assert_eq!(
        os_eng
            .read_sequential(ReadDirection::Previous, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "10"
    );
    assert_eq!(os_eng.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(os_eng.open(&os_path, OpenMode::Input).as_str(), "00");
    assert_eq!(
        os_eng.start(StartRelation::GreaterOrEqual, 9).as_str(),
        "00"
    );
    assert_eq!(
        os_eng
            .read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "00"
    );
    assert_eq!(
        os_eng
            .read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "14"
    );
    assert_eq!(
        os_eng
            .read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "46"
    );
    assert_eq!(os_eng.close(CloseDisposition::Normal).as_str(), "00");
    let _ = std::fs::remove_file(&os_path);
    Ok(())
}

/// Tests [`RelativeFileEngine`] `DYNAMIC` / `RANDOM` access, `START` (`=`, `>`, `>=`, `<`, `<=`), and locks (`"51"`, `"61"`).
#[test]
fn relative_engine_dynamic_start_relations_and_multi_connector_locks() -> Result<()> {
    let fs = InMemoryFileSystem::new();
    let locks = FileLockRegistry::new();
    let p = Path::new("/rel/dyn.dat");

    let mut cfg1 = RelativeFileConfig::new(
        "REL-DYN-1",
        FileConnectorId::new(40),
        AccessMode::Dynamic,
        RecordLayout::varying(2, 6, VaryingHeaderFormat::FourByteBigEndian)?,
    );
    cfg1.optionality = FileOptionality::Optional;

    let mut e1 =
        RelativeFileEngine::with_backend(cfg1.clone(), Box::new(fs.clone()), locks.clone());
    assert_eq!(e1.open(p, OpenMode::Extend).as_str(), "37");

    assert_eq!(e1.open(p, OpenMode::Input).as_str(), "05");
    assert_eq!(e1.start(StartRelation::Equal, 1).as_str(), "23");
    assert_eq!(
        e1.read_random(1, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "23"
    );
    assert_eq!(e1.close(CloseDisposition::Normal).as_str(), "00");
    assert_eq!(e1.open(p, OpenMode::Input).as_str(), "05");
    assert_eq!(
        e1.read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "10"
    );
    assert_eq!(e1.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(e1.open(p, OpenMode::Io).as_str(), "05");
    assert_eq!(e1.write_sequential(b"NO").status.as_str(), "48");
    assert_eq!(e1.write_keyed(0, b"NO").status.as_str(), "24");
    assert_eq!(e1.write_keyed(1, b"X").status.as_str(), "44");
    assert_eq!(e1.write_keyed(2, b"TWO").status.as_str(), "00");
    assert_eq!(e1.write_keyed(5, b"FIVE").status.as_str(), "00");
    assert_eq!(e1.write_keyed(4, b"FOUR").status.as_str(), "00");
    assert_eq!(e1.delete_record(Some(4)).as_str(), "00");
    assert_eq!(e1.write_keyed(2, b"DUP").status.as_str(), "22");

    assert_eq!(
        e1.read_random(0, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "23"
    );
    assert_eq!(
        e1.read_random(3, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "23"
    );
    assert_eq!(
        e1.read_random(9, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "23"
    );

    assert_eq!(e1.start(StartRelation::Equal, 3).as_str(), "23");
    assert_eq!(e1.start(StartRelation::Greater, 5).as_str(), "23");
    assert_eq!(e1.start(StartRelation::Less, 2).as_str(), "23");

    assert_eq!(e1.start(StartRelation::Equal, 2).as_str(), "00");
    assert_eq!(
        e1.read_sequential(ReadDirection::Next, ReadLockPolicy::WithLock)
            .slot_key,
        Some(RelativeSlotKey::try_new(2)?)
    );
    assert_eq!(e1.held_lock_count(), 1);

    assert_eq!(e1.start(StartRelation::Greater, 2).as_str(), "00");
    assert_eq!(
        e1.read_sequential(ReadDirection::Next, ReadLockPolicy::WithKeptLock)
            .slot_key,
        Some(RelativeSlotKey::try_new(5)?)
    );
    assert_eq!(e1.held_lock_count(), 2);

    assert_eq!(e1.start(StartRelation::LessOrEqual, 5).as_str(), "00");
    assert_eq!(
        e1.read_sequential(ReadDirection::Previous, ReadLockPolicy::DefaultLock)
            .slot_key,
        Some(RelativeSlotKey::try_new(5)?)
    );
    assert_eq!(
        e1.read_sequential(ReadDirection::Previous, ReadLockPolicy::DefaultLock)
            .slot_key,
        Some(RelativeSlotKey::try_new(2)?)
    );
    assert_eq!(
        e1.read_sequential(ReadDirection::Previous, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "10"
    );

    assert_eq!(e1.start(StartRelation::Less, 5).as_str(), "00");
    assert_eq!(
        e1.read_sequential(ReadDirection::Next, ReadLockPolicy::WithNoLock)
            .slot_key,
        Some(RelativeSlotKey::try_new(2)?)
    );

    verify_relative_contention_mutations_and_corruptions(&mut e1, &fs, &locks, p)
}

/// Exercises multi-connector `"51"` contention, `REWRITE`/`DELETE` edge cases, and corrupt slot `"30"`/`"04"`.
///
/// # Arguments
///
/// * `e1` - First relative file engine connector.
/// * `fs` - Shared in-memory filesystem.
/// * `locks` - Shared file-lock registry.
/// * `p` - Target virtual relative file path.
fn verify_relative_contention_mutations_and_corruptions(
    e1: &mut RelativeFileEngine,
    fs: &InMemoryFileSystem,
    locks: &FileLockRegistry,
    p: &Path,
) -> Result<()> {
    let mut cfg2 = RelativeFileConfig::new(
        "REL-RND-2",
        FileConnectorId::new(41),
        AccessMode::Dynamic,
        RecordLayout::varying(2, 6, VaryingHeaderFormat::FourByteBigEndian)?,
    );
    cfg2.lock_mode = LockMode::Manual;
    let mut e2 = RelativeFileEngine::with_backend(cfg2, Box::new(fs.clone()), locks.clone());
    assert_eq!(e2.open(p, OpenMode::Io).as_str(), "00");

    let _ = e1.read_random(2, ReadLockPolicy::WithKeptLock);
    assert_eq!(
        e2.read_random(2, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "51"
    );
    assert_eq!(
        e2.read_random(2, ReadLockPolicy::WithNoLock)
            .status
            .as_str(),
        "00"
    );
    assert_eq!(e2.start(StartRelation::Equal, 2).as_str(), "00");
    assert_eq!(
        e2.read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "51"
    );
    assert_eq!(e2.write_keyed(2, b"NO").status.as_str(), "51");
    assert_eq!(e2.rewrite_record(Some(2), b"NO").as_str(), "51");
    assert_eq!(e2.delete_record(Some(2)).as_str(), "51");

    assert_eq!(e1.unlock_records().as_str(), "00");
    assert_eq!(e1.held_lock_count(), 0);

    assert_eq!(e2.rewrite_record(None, b"NO").as_str(), "23");
    assert_eq!(e2.rewrite_record(Some(0), b"NO").as_str(), "23");
    assert_eq!(e2.rewrite_record(Some(3), b"NO").as_str(), "23");
    assert_eq!(e2.rewrite_record(Some(99), b"NO").as_str(), "23");
    assert_eq!(e2.rewrite_record(Some(2), b"X").as_str(), "44");
    assert_eq!(e2.rewrite_record(Some(2), b"TWO2").as_str(), "00");

    assert_eq!(e1.start(StartRelation::Equal, 2).as_str(), "00");
    assert_eq!(e2.delete_record(None).as_str(), "23");
    assert_eq!(e2.delete_record(Some(3)).as_str(), "23");
    assert_eq!(e2.delete_record(Some(99)).as_str(), "23");
    assert_eq!(e2.delete_record(Some(2)).as_str(), "00");
    assert_eq!(
        e1.read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock)
            .slot_key,
        Some(RelativeSlotKey::try_new(5)?)
    );

    assert_eq!(e1.close(CloseDisposition::ReelUnit).as_str(), "07");
    assert_eq!(e1.close(CloseDisposition::Normal).as_str(), "00");
    assert_eq!(e2.close(CloseDisposition::Normal).as_str(), "00");

    verify_relative_corrupt_and_io_faults(fs, locks)
}

/// Exercises corrupted relative slot headers (`"30"` / `"04"`), exclusive `"61"`, and injected I/O faults.
///
/// # Arguments
///
/// * `fs` - Shared in-memory filesystem.
/// * `locks` - Shared file-lock registry.
fn verify_relative_corrupt_and_io_faults(
    fs: &InMemoryFileSystem,
    locks: &FileLockRegistry,
) -> Result<()> {
    let p_bad_align = Path::new("/rel/bad_align.dat");
    fs.seed_file(p_bad_align, &[0x01, 0x00, 0x00]);
    let mut cfg = RelativeFileConfig::new(
        "REL-FLT",
        FileConnectorId::new(42),
        AccessMode::Dynamic,
        RecordLayout::varying(2, 4, VaryingHeaderFormat::TwoByteBigEndian)?,
    );
    let mut eng =
        RelativeFileEngine::with_backend(cfg.clone(), Box::new(fs.clone()), locks.clone());
    assert_eq!(eng.open(p_bad_align, OpenMode::Input).as_str(), "30");

    let p_bad_flag = Path::new("/rel/bad_flag.dat");
    fs.seed_file(p_bad_flag, &[0xFF, 0, 0, 0, 2, b'A', b'B', 0, 0]);
    assert_eq!(eng.open(p_bad_flag, OpenMode::Input).as_str(), "30");

    let p_mismatch = Path::new("/rel/len_mismatch.dat");
    fs.seed_file(p_mismatch, &[0x01, 0, 0, 0, 9, b'A', b'B', b'C', b'D']);
    assert_eq!(eng.open(p_mismatch, OpenMode::Input).as_str(), "00");
    let r04 = eng.read_random(1, ReadLockPolicy::DefaultLock);
    assert_eq!(r04.status.as_str(), "04");
    assert_eq!(r04.actual_len, Some(4));
    assert_eq!(eng.start(StartRelation::Equal, 1).as_str(), "00");
    assert_eq!(
        eng.read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "04"
    );
    assert_eq!(eng.rewrite_record(Some(1), b"AB").as_str(), "49");
    assert_eq!(eng.delete_record(Some(1)).as_str(), "49");

    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(eng.start(StartRelation::Equal, 1).as_str(), "30");
    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(
        eng.read_random(1, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "30"
    );
    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(
        eng.read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "30"
    );
    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(eng.close(CloseDisposition::Normal).as_str(), "30");

    cfg.connector_id = FileConnectorId::new(43);
    cfg.lock_mode = LockMode::Exclusive;
    let mut ex = RelativeFileEngine::with_backend(cfg, Box::new(fs.clone()), locks.clone());
    assert_eq!(ex.open(p_mismatch, OpenMode::Io).as_str(), "00");
    assert_eq!(eng.open(p_mismatch, OpenMode::Input).as_str(), "61");

    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(ex.write_keyed(3, b"AB").status.as_str(), "30");
    fs.inject_fault_after(2, ErrorKind::StorageFull);
    assert_eq!(ex.write_keyed(3, b"AB").status.as_str(), "24");
    assert_eq!(ex.write_keyed(3, b"AB").status.as_str(), "00");

    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(ex.rewrite_record(Some(1), b"AB").as_str(), "30");
    fs.inject_fault_after(0, ErrorKind::BrokenPipe);
    assert_eq!(ex.delete_record(Some(1)).as_str(), "30");
    assert_eq!(ex.close(CloseDisposition::Normal).as_str(), "00");

    fs.mark_read_only(p_mismatch);
    assert_eq!(eng.open(p_mismatch, OpenMode::Output).as_str(), "37");
    assert_eq!(eng.close(CloseDisposition::Normal).as_str(), "42");
    Ok(())
}

/// Covers closed-connector verb rejections, `COB_LS_NULLS` round-trip readback, and edge transitions.
#[test]
fn closed_connectors_null_unescape_and_relative_edge_transitions() -> Result<()> {
    let fs = InMemoryFileSystem::new();
    let locks = FileLockRegistry::new();

    let mut seq_var = SequentialFileEngine::with_backend(
        {
            let mut c = SequentialFileConfig::new(
                "SV",
                FileConnectorId::new(60),
                RecordLayout::varying(2, 4, VaryingHeaderFormat::TwoByteBigEndian)?,
            );
            c.alignment_policy = FixedAlignmentPolicy::RequireExactMultiple39;
            c.linage = Some(LinageParams::try_new(3, None, 0, 0)?);
            c
        },
        Box::new(fs.clone()),
        locks.clone(),
    );
    assert_eq!(seq_var.read_next().status.as_str(), "47");
    assert_eq!(
        seq_var
            .write_record(b"AB", AdvancingSpec::NoneRaw)
            .status
            .as_str(),
        "48"
    );
    assert_eq!(seq_var.rewrite_record(b"AB").as_str(), "49");
    assert_eq!(seq_var.close(CloseDisposition::ReelUnit).as_str(), "42");

    let p_sv = Path::new("/edge/sv.dat");
    assert_eq!(seq_var.open(p_sv, OpenMode::Output).as_str(), "00");
    assert_eq!(
        seq_var
            .write_record(b"AB", AdvancingSpec::DefaultSingleLine)
            .status
            .as_str(),
        "00"
    );
    assert_eq!(seq_var.close(CloseDisposition::Normal).as_str(), "00");

    let p_sv_io = Path::new("/edge/sv_io.dat");
    fs.seed_file(p_sv_io, &[0x00, 0x01, b'Z', 0x00, 0x02, b'O', b'K']);
    let mut seq_var_io = SequentialFileEngine::with_backend(
        {
            let mut c = SequentialFileConfig::new(
                "SVIO",
                FileConnectorId::new(61),
                RecordLayout::varying(2, 4, VaryingHeaderFormat::TwoByteBigEndian)?,
            );
            c.alignment_policy = FixedAlignmentPolicy::RequireExactMultiple39;
            c
        },
        Box::new(fs.clone()),
        locks.clone(),
    );
    assert_eq!(seq_var_io.open(p_sv_io, OpenMode::Io).as_str(), "00");
    assert_eq!(seq_var_io.read_next().status.as_str(), "04");
    assert_eq!(seq_var_io.rewrite_record(b"Q").as_str(), "44");
    fs.inject_fault_after(2, ErrorKind::BrokenPipe);
    assert_eq!(seq_var_io.read_next().status.as_str(), "30");
    assert_eq!(seq_var_io.close(CloseDisposition::Normal).as_str(), "00");

    verify_ls_and_rel_closed_and_null_roundtrip(&fs, &locks)
}

/// Covers closed `LineSequentialFileEngine`/`RelativeFileEngine`, `COB_LS_NULLS` unescape, and relative tail scans.
///
/// # Arguments
///
/// * `fs` - Shared in-memory filesystem.
/// * `locks` - Shared file-lock registry.
fn verify_ls_and_rel_closed_and_null_roundtrip(
    fs: &InMemoryFileSystem,
    locks: &FileLockRegistry,
) -> Result<()> {
    let mut ls_cfg = LineSequentialConfig::new(
        "LSN",
        FileConnectorId::new(62),
        RecordLayout::varying(2, 8, VaryingHeaderFormat::TwoByteBigEndian)?,
    );
    ls_cfg.null_escapes = NullEscapePolicy::NullPrefixControlBytes;
    ls_cfg.trailing_spaces = TrailingSpacePolicy::PreserveFixedSpaces;
    let mut ls =
        LineSequentialFileEngine::with_backend(ls_cfg, Box::new(fs.clone()), locks.clone());
    assert_eq!(ls.read_next().status.as_str(), "47");
    assert_eq!(
        ls.write_record(b"AB", AdvancingSpec::DefaultSingleLine)
            .as_str(),
        "48"
    );
    assert_eq!(ls.close(CloseDisposition::ReelUnit).as_str(), "42");

    let p_lsn = Path::new("/edge/lsn.txt");
    assert_eq!(ls.open(p_lsn, OpenMode::Output).as_str(), "00");
    assert_eq!(ls.read_next().status.as_str(), "47");
    let raw_ctrl = [0x00, 0x0A, 0x0D, b'K'];
    assert_eq!(
        ls.write_record(&raw_ctrl, AdvancingSpec::DefaultSingleLine)
            .as_str(),
        "00"
    );
    assert_eq!(ls.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(ls.open(p_lsn, OpenMode::Input).as_str(), "00");
    let r_unesc = ls.read_next();
    assert_eq!(r_unesc.status.as_str(), "00");
    assert_eq!(r_unesc.record.as_deref(), Some(raw_ctrl.as_slice()));
    assert_eq!(ls.close(CloseDisposition::Normal).as_str(), "00");
    verify_relative_closed_and_edge_roundtrip(fs, locks)
}

/// Exercises closed-handle guards, varying under-min warning (`"04"`), and sequential boundary (`"24"`) on [`RelativeFileEngine`].
///
/// # Arguments
///
/// * `fs` - Shared in-memory filesystem.
/// * `locks` - Shared file-lock registry.
fn verify_relative_closed_and_edge_roundtrip(
    fs: &InMemoryFileSystem,
    locks: &FileLockRegistry,
) -> Result<()> {
    let mut rel = RelativeFileEngine::with_backend(
        RelativeFileConfig::new(
            "RELE",
            FileConnectorId::new(63),
            AccessMode::Dynamic,
            RecordLayout::varying(2, 4, VaryingHeaderFormat::TwoByteBigEndian)?,
        ),
        Box::new(fs.clone()),
        locks.clone(),
    );
    assert_eq!(rel.held_lock_count(), 0);
    assert_eq!(rel.start(StartRelation::Equal, 1).as_str(), "47");
    assert_eq!(
        rel.read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "47"
    );
    assert_eq!(
        rel.read_random(1, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "47"
    );
    assert_eq!(rel.write_keyed(1, b"AB").status.as_str(), "48");
    assert_eq!(rel.rewrite_record(Some(1), b"AB").as_str(), "49");
    assert_eq!(rel.delete_record(Some(1)).as_str(), "49");
    assert_eq!(rel.close(CloseDisposition::ReelUnit).as_str(), "42");

    let p_rel = Path::new("/edge/rel.dat");
    assert_eq!(rel.open(p_rel, OpenMode::Output).as_str(), "00");
    assert_eq!(rel.start(StartRelation::Equal, 1).as_str(), "47");
    assert_eq!(
        rel.read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "47"
    );
    assert_eq!(
        rel.read_random(1, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "47"
    );
    assert_eq!(rel.write_keyed(1, b"ONE").status.as_str(), "00");
    assert_eq!(rel.write_keyed(2, b"TWO").status.as_str(), "00");
    assert_eq!(rel.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(rel.open(p_rel, OpenMode::Input).as_str(), "00");
    assert_eq!(rel.write_keyed(3, b"NO").status.as_str(), "48");
    assert_eq!(rel.start(StartRelation::Equal, 0).as_str(), "23");
    assert_eq!(rel.start(StartRelation::Equal, 99).as_str(), "23");
    assert_eq!(rel.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(rel.open(p_rel, OpenMode::Io).as_str(), "00");
    let _ = rel.read_random(1, ReadLockPolicy::DefaultLock);
    let _ = rel.read_random(1, ReadLockPolicy::DefaultLock);
    assert_eq!(
        rel.read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "00"
    );
    assert_eq!(
        rel.read_sequential(ReadDirection::Next, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "10"
    );
    assert_eq!(rel.delete_record(Some(2)).as_str(), "00");
    assert_eq!(rel.delete_record(Some(1)).as_str(), "00");
    assert_eq!(rel.close(CloseDisposition::Normal).as_str(), "00");

    assert_eq!(rel.open(p_rel, OpenMode::Io).as_str(), "00");
    assert_eq!(rel.close(CloseDisposition::Normal).as_str(), "00");
    verify_relative_under_min_and_seq_boundary(&mut rel, fs, locks)
}

/// Exercises varying under-min warning (`"04"`) and sequential boundary (`"24"`) on [`RelativeFileEngine`].
///
/// # Arguments
///
/// * `rel` - Configured dynamic varying `RelativeFileEngine`.
/// * `fs` - Shared in-memory filesystem.
/// * `locks` - Shared file-lock registry.
fn verify_relative_under_min_and_seq_boundary(
    rel: &mut RelativeFileEngine,
    fs: &InMemoryFileSystem,
    locks: &FileLockRegistry,
) -> Result<()> {
    let p_under = Path::new("/edge/rel_under.dat");
    fs.seed_file(p_under, &[0x01, 0, 0, 0, 1, b'A', 0, 0, 0]);
    assert_eq!(rel.open(p_under, OpenMode::Input).as_str(), "00");
    assert_eq!(
        rel.read_random(1, ReadLockPolicy::DefaultLock)
            .status
            .as_str(),
        "04"
    );
    assert_eq!(rel.close(CloseDisposition::Normal).as_str(), "00");

    let mut rel_seq = RelativeFileEngine::with_backend(
        RelativeFileConfig::new(
            "RSEQF",
            FileConnectorId::new(64),
            AccessMode::Sequential,
            RecordLayout::fixed_spaces(2)?,
        ),
        Box::new(fs.clone()),
        locks.clone(),
    );
    assert_eq!(
        rel_seq
            .open(Path::new("/edge/rseqf.dat"), OpenMode::Output)
            .as_str(),
        "00"
    );
    fs.inject_fault_after(0, ErrorKind::StorageFull);
    assert_eq!(rel_seq.write_sequential(b"AB").status.as_str(), "24");
    assert_eq!(rel_seq.close(CloseDisposition::Normal).as_str(), "00");
    Ok(())
}
