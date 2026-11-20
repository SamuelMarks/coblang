//! Unit and integration tests for `coblang-fileio` storage backends (`OsFileSystem`,
//! `InMemoryFileSystem`, `FileLockRegistry`) and `LINAGE` pagination (`LinagePaginationState`).

use std::io::{ErrorKind, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use coblang_common::{FileConnectorId, FileStatusKind, Result};
use coblang_fileio::{
    AdvancingPhase, AdvancingSpec, FileLockRegistry, FilePresence, InMemoryFileSystem,
    LinagePaginationState, LinageParams, LockMode, LockOutcome, OsFileSystem, PageOverflowOutcome,
    StorageBackend, StorageOpenRequest,
};

/// Monotonic sequence counter for isolated per-test OS temporary files.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Allocates a unique temporary file path under `std::env::temp_dir()`.
///
/// # Arguments
///
/// * `stem` - Descriptive test file stem.
fn unique_temp_path(stem: &str) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("coblang_fileio_{pid}_{seq}_{stem}.tmp"))
}

/// Exercises every [`StorageOpenRequest`] and [`RandomAccessStream`] operation on [`OsFileSystem`].
#[test]
fn os_filesystem_all_open_modes_and_stream_ops() -> Result<()> {
    let mut os = OsFileSystem;
    let path = unique_temp_path("os_ops");
    let _ = std::fs::remove_file(&path);

    assert_eq!(os.path_exists(&path), FilePresence::Absent);
    assert!(os.open_stream(&path, StorageOpenRequest::ReadOnly).is_err());
    assert!(os.remove_path(&path).is_err());

    {
        let mut s = os.open_stream(&path, StorageOpenRequest::CreateTruncateWrite)?;
        s.write_bytes(b"ABCD")?;
        s.flush_bytes()?;
    }
    assert_eq!(os.path_exists(&path), FilePresence::Present);

    {
        let mut s = os.open_stream(&path, StorageOpenRequest::AppendExisting)?;
        s.write_bytes(b"EF")?;
        s.flush_bytes()?;
    }

    {
        let mut s = os.open_stream(&path, StorageOpenRequest::ReadWriteExisting)?;
        assert_eq!(s.seek_pos(SeekFrom::Start(2))?, 2);
        s.write_bytes(b"xy")?;
        s.flush_bytes()?;
    }

    {
        let mut s = os.open_stream(&path, StorageOpenRequest::ReadOnly)?;
        let mut buf = [0_u8; 6];
        let n = s.read_bytes(&mut buf)?;
        assert_eq!(n, 6);
        assert_eq!(&buf, b"ABxyEF");
    }

    os.remove_path(&path)?;
    assert_eq!(os.path_exists(&path), FilePresence::Absent);

    {
        let mut s = os.open_stream(&path, StorageOpenRequest::ReadWriteCreate)?;
        s.write_bytes(b"12")?;
        s.flush_bytes()?;
    }
    os.remove_path(&path)?;

    {
        let mut s = os.open_stream(&path, StorageOpenRequest::AppendCreate)?;
        s.write_bytes(b"34")?;
        s.flush_bytes()?;
    }
    os.remove_path(&path)?;
    Ok(())
}

/// Exercises [`InMemoryFileSystem`], [`InMemoryStream`], read-only paths, seeks, and fault injection.
#[test]
fn in_memory_filesystem_all_modes_seeks_and_faults() -> Result<()> {
    let mut fs = InMemoryFileSystem::new();
    let p = Path::new("/mem/a.dat");
    assert_eq!(fs.path_exists(p), FilePresence::Absent);
    assert_eq!(fs.snapshot(p), None);

    assert!(fs.open_stream(p, StorageOpenRequest::ReadOnly).is_err());
    assert!(
        fs.open_stream(p, StorageOpenRequest::ReadWriteExisting)
            .is_err()
    );
    assert!(
        fs.open_stream(p, StorageOpenRequest::AppendExisting)
            .is_err()
    );
    assert!(fs.remove_path(p).is_err());

    fs.seed_file(p, b"HELLO");
    assert_eq!(fs.path_exists(p), FilePresence::Present);
    assert_eq!(fs.snapshot(p), Some(b"HELLO".to_vec()));

    {
        let mut ro = fs.open_stream(p, StorageOpenRequest::ReadOnly)?;
        assert!(ro.write_bytes(b"X").is_err());
        assert_eq!(ro.seek_pos(SeekFrom::End(-2))?, 3);
        assert_eq!(ro.seek_pos(SeekFrom::Current(-1))?, 2);
        assert!(ro.seek_pos(SeekFrom::Current(-10)).is_err());
        let mut out = [0_u8; 3];
        assert_eq!(ro.read_bytes(&mut out)?, 3);
        assert_eq!(&out, b"LLO");
        assert_eq!(ro.seek_pos(SeekFrom::Start(100))?, 100);
        assert_eq!(ro.read_bytes(&mut out)?, 0);
    }

    {
        let mut rw = fs.open_stream(p, StorageOpenRequest::ReadWriteExisting)?;
        assert_eq!(rw.seek_pos(SeekFrom::Start(5))?, 5);
        rw.write_bytes(b"_WORLD")?;
        rw.flush_bytes()?;
        #[cfg(target_pointer_width = "64")]
        {
            let _ = rw.seek_pos(SeekFrom::Start(usize::MAX as u64))?;
            assert!(rw.write_bytes(b"AB").is_err());
        }
    }
    assert_eq!(fs.snapshot(p), Some(b"HELLO_WORLD".to_vec()));

    let p2 = Path::new("/mem/b.dat");
    {
        let mut ap = fs.open_stream(p2, StorageOpenRequest::AppendCreate)?;
        ap.write_bytes(b"ONE")?;
    }
    {
        let mut ap2 = fs.open_stream(p2, StorageOpenRequest::AppendExisting)?;
        ap2.write_bytes(b"TWO")?;
    }
    assert_eq!(fs.snapshot(p2), Some(b"ONETWO".to_vec()));

    let p3 = Path::new("/mem/c.dat");
    {
        let mut rwc = fs.open_stream(p3, StorageOpenRequest::ReadWriteCreate)?;
        rwc.write_bytes(b"NEW")?;
    }
    {
        let mut trunc = fs.open_stream(p3, StorageOpenRequest::CreateTruncateWrite)?;
        trunc.write_bytes(b"T")?;
    }
    assert_eq!(fs.snapshot(p3), Some(b"T".to_vec()));

    fs.mark_read_only(p3);
    assert!(
        fs.open_stream(p3, StorageOpenRequest::CreateTruncateWrite)
            .is_err()
    );
    assert!(fs.remove_path(p3).is_err());
    assert!(fs.open_stream(p3, StorageOpenRequest::ReadOnly).is_ok());

    fs.inject_fault_after(0, ErrorKind::TimedOut);
    assert!(fs.open_stream(p3, StorageOpenRequest::ReadOnly).is_err());

    fs.inject_fault_after(1, ErrorKind::StorageFull);
    assert!(fs.remove_path(p2).is_ok());
    assert!(fs.remove_path(p).is_err());

    fs.inject_fault_after(0, ErrorKind::TimedOut);
    assert!(fs.open_stream(p, StorageOpenRequest::ReadOnly).is_err());
    Ok(())
}

/// Exercises all file-sharing (`"61"`) and record-locking (`"51"`) paths in [`FileLockRegistry`].
#[test]
fn file_lock_registry_sharing_and_record_locks() {
    let reg = FileLockRegistry::new();
    let p = Path::new("/locks/rel.dat");
    let c1 = FileConnectorId::new(1);
    let c2 = FileConnectorId::new(2);

    assert_eq!(reg.held_lock_count(p, c1), 0);
    assert_eq!(
        reg.check_not_locked_by_other(p, c1, 1),
        LockOutcome::Granted
    );
    assert_eq!(
        reg.check_not_locked_by_other(p, c1, 1),
        LockOutcome::Granted
    );
    reg.unregister_close(p, c1);
    reg.release_single_lock(p, c1, 1);
    reg.release_all_record_locks(p, c1);

    assert_eq!(
        reg.register_open(p, c1, LockMode::Automatic),
        LockOutcome::Granted
    );
    assert_eq!(
        reg.register_open(p, c1, LockMode::Exclusive),
        LockOutcome::Granted
    );
    assert_eq!(
        reg.register_open(p, c2, LockMode::Automatic),
        LockOutcome::Denied(FileStatusKind::FileSharingConflict61)
    );
    reg.unregister_close(p, c1);

    assert_eq!(
        reg.register_open(p, c1, LockMode::Manual),
        LockOutcome::Granted
    );
    assert_eq!(
        reg.register_open(p, c2, LockMode::Exclusive),
        LockOutcome::Denied(FileStatusKind::FileSharingConflict61)
    );
    assert_eq!(
        reg.register_open(p, c2, LockMode::Manual),
        LockOutcome::Granted
    );

    assert_eq!(reg.acquire_record_lock(p, c1, 7), LockOutcome::Granted);
    assert_eq!(reg.acquire_record_lock(p, c1, 7), LockOutcome::Granted);
    assert_eq!(reg.held_lock_count(p, c1), 1);
    assert_eq!(
        reg.check_not_locked_by_other(p, c1, 7),
        LockOutcome::Granted
    );
    assert_eq!(
        reg.check_not_locked_by_other(p, c2, 7),
        LockOutcome::Denied(FileStatusKind::RecordLocked51)
    );
    assert_eq!(
        reg.acquire_record_lock(p, c2, 7),
        LockOutcome::Denied(FileStatusKind::RecordLocked51)
    );

    reg.release_single_lock(p, c2, 7);
    assert_eq!(reg.held_lock_count(p, c1), 1);
    reg.release_single_lock(p, c1, 7);
    assert_eq!(reg.held_lock_count(p, c1), 0);

    assert_eq!(reg.acquire_record_lock(p, c1, 3), LockOutcome::Granted);
    assert_eq!(reg.acquire_record_lock(p, c2, 4), LockOutcome::Granted);
    reg.release_all_record_locks(p, c1);
    assert_eq!(reg.held_lock_count(p, c1), 0);
    assert_eq!(reg.held_lock_count(p, c2), 1);

    reg.unregister_close(p, c1);
    reg.unregister_close(p, c2);
    assert_eq!(reg.held_lock_count(p, c2), 0);
}

/// Exercises [`LinageParams`] validation and [`LinagePaginationState`] `BEFORE`/`AFTER` & `EOP` rules.
#[test]
fn linage_pagination_before_after_footing_overflow_and_dynamic_relatch() -> Result<()> {
    assert!(LinageParams::try_new(0, None, 0, 0).is_err());
    assert!(LinageParams::try_new(10, Some(0), 0, 0).is_err());
    assert!(LinageParams::try_new(10, Some(11), 0, 0).is_err());

    let p_no_foot = LinageParams::try_new(4, None, 2, 1)?;
    assert_eq!(p_no_foot.body_lines(), 4);
    assert_eq!(p_no_foot.footing_at(), None);
    assert_eq!(p_no_foot.lines_at_top(), 2);
    assert_eq!(p_no_foot.lines_at_bottom(), 1);
    assert_eq!(p_no_foot.total_page_lines(), 7);
    assert!(!p_no_foot.is_in_footing(4));

    let p_with_foot = LinageParams::try_new(5, Some(4), 1, 2)?;
    assert!(p_with_foot.is_in_footing(4));
    assert!(!p_with_foot.is_in_footing(3));

    let (mut st, top) = LinagePaginationState::open_page(p_with_foot);
    assert_eq!(top, vec![0x0A]);
    assert_eq!(st.linage_counter(), 1);
    assert_eq!(st.active_params(), p_with_foot);
    assert_eq!(st.finish_on_close(), Vec::<u8>::new());

    let r0 = st.write_record(
        b"H0",
        AdvancingSpec::Lines {
            phase: AdvancingPhase::After,
            count: 0,
        },
    );
    assert_eq!(r0.linage_counter, 1);
    assert_eq!(r0.framed_bytes, b"H0");
    assert_eq!(r0.page_overflow, PageOverflowOutcome::WithinPage);

    let r0_overprint = st.write_record(
        b"H1",
        AdvancingSpec::Lines {
            phase: AdvancingPhase::After,
            count: 0,
        },
    );
    assert_eq!(r0_overprint.framed_bytes, vec![0x0D, b'H', b'1']);

    let r_def = st.write_record(b"L2", AdvancingSpec::DefaultSingleLine);
    assert_eq!(r_def.linage_counter, 2);
    assert_eq!(r_def.page_overflow, PageOverflowOutcome::WithinPage);

    let r_foot = st.write_record(
        b"L4",
        AdvancingSpec::Lines {
            phase: AdvancingPhase::After,
            count: 2,
        },
    );
    assert_eq!(r_foot.linage_counter, 4);
    assert_eq!(r_foot.page_overflow, PageOverflowOutcome::EndOfPageReached);

    st.set_next_page_params(p_no_foot);
    assert_eq!(st.active_params(), p_with_foot);

    let r_ovf = st.write_record(
        b"P2L1",
        AdvancingSpec::Lines {
            phase: AdvancingPhase::After,
            count: 2,
        },
    );
    assert_eq!(r_ovf.linage_counter, 1);
    assert_eq!(r_ovf.page_overflow, PageOverflowOutcome::EndOfPageReached);
    assert_eq!(st.active_params(), p_no_foot);

    verify_linage_before_and_page_variants()?;
    Ok(())
}

/// Exercises `BEFORE ADVANCING` (`0`, `n`, overflow, `PAGE`) and `AFTER ADVANCING PAGE` (`FOOTING AT 1`).
fn verify_linage_before_and_page_variants() -> Result<()> {
    let p_foot1 = LinageParams::try_new(3, Some(1), 0, 0)?;
    let (mut st, _) = LinagePaginationState::open_page(p_foot1);

    let ap = st.write_record(
        b"A",
        AdvancingSpec::Page {
            phase: AdvancingPhase::After,
        },
    );
    assert_eq!(ap.linage_counter, 1);
    assert_eq!(ap.page_overflow, PageOverflowOutcome::EndOfPageReached);

    let b_after_print = st.write_record(
        b"B",
        AdvancingSpec::Lines {
            phase: AdvancingPhase::Before,
            count: 0,
        },
    );
    assert_eq!(b_after_print.framed_bytes, vec![0x0D, b'B', 0x0D]);
    assert_eq!(b_after_print.linage_counter, 1);
    assert_eq!(
        b_after_print.page_overflow,
        PageOverflowOutcome::EndOfPageReached
    );

    let bp_foot = st.write_record(
        b"C",
        AdvancingSpec::Page {
            phase: AdvancingPhase::Before,
        },
    );
    assert_eq!(bp_foot.linage_counter, 1);
    assert_eq!(bp_foot.page_overflow, PageOverflowOutcome::EndOfPageReached);

    let p_plain = LinageParams::try_new(3, None, 0, 0)?;
    let (mut st2, _) = LinagePaginationState::open_page(p_plain);
    let b1 = st2.write_record(
        b"L1",
        AdvancingSpec::Lines {
            phase: AdvancingPhase::Before,
            count: 1,
        },
    );
    assert_eq!(b1.linage_counter, 2);
    assert_eq!(b1.page_overflow, PageOverflowOutcome::WithinPage);

    let b_ovf = st2.write_record(
        b"L2",
        AdvancingSpec::Lines {
            phase: AdvancingPhase::Before,
            count: 5,
        },
    );
    assert_eq!(b_ovf.linage_counter, 1);
    assert_eq!(b_ovf.page_overflow, PageOverflowOutcome::EndOfPageReached);

    let ap_plain = st2.write_record(
        b"P",
        AdvancingSpec::Page {
            phase: AdvancingPhase::After,
        },
    );
    assert_eq!(ap_plain.page_overflow, PageOverflowOutcome::WithinPage);

    let bp_after_text = st2.write_record(
        b"Q",
        AdvancingSpec::Page {
            phase: AdvancingPhase::Before,
        },
    );
    assert_eq!(bp_after_text.page_overflow, PageOverflowOutcome::WithinPage);

    let _ = st2.write_record(b"END", AdvancingSpec::NoneRaw);
    assert_eq!(st2.finish_on_close(), vec![0x0A]);
    Ok(())
}
