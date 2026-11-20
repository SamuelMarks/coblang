//! Comprehensive C-ABI integration & branch-coverage tests for `coblang_rt::fileio_abi` (`M14`/`M15`)
//! and `coblang_rt::sort_abi` (`M16`), including simulated ISAM B+Tree page CRC32 corruption (`"30"`).

use std::fs;
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

use coblang_common::{DigitCount, Result, ScaleFactor};
use coblang_decimal::{OverpunchDialect, PackedSignPolicy};
use coblang_hir::{
    BinaryEndianness, BlankZeroSpec, Comp6Mode, DynamicOdoPresence, FloatBinaryWidth,
    FloatDecimalWidth, HirAltKeySuppression, HirDisplayStyle, HirDuplicateKeyPolicy,
    HirFileAccessMode, HirFileLockMode, HirFileOptionality, HirFileOrganization, HirKeySegment,
    HirPrintAdvancingMode, HirRecordFormat, HirSortDirection, HirType, JustifiedSpec,
    NumericSignSpec, Signedness, TruncationPolicy,
};
use coblang_pic::MoveFieldDesc;
use coblang_rt::{
    cob_tidy,
    fileio_abi::{
        INLINE_FLAG_HAS_NOT_ON, INLINE_FLAG_HAS_ON, close_all_open_connectors,
        coblang_rt_decl_enter, coblang_rt_decl_leave, coblang_rt_file_close,
        coblang_rt_file_delete, coblang_rt_file_delete_physical, coblang_rt_file_fatal_abort,
        coblang_rt_file_last_linage_counter, coblang_rt_file_last_rec_len,
        coblang_rt_file_last_rel_key, coblang_rt_file_open, coblang_rt_file_read,
        coblang_rt_file_rewrite, coblang_rt_file_set_linage, coblang_rt_file_start,
        coblang_rt_file_unlock, coblang_rt_file_write, set_suppress_fatal_exit_for_tests,
    },
    sort_abi::{
        SORT_FLAG_DUPLICATES_IN_ORDER, SORT_FLAG_IS_MERGE, SORT_RETURN_ABORTED, SORT_RETURN_AT_END,
        coblang_rt_sort_begin, coblang_rt_sort_begin_merge_stream, coblang_rt_sort_end,
        coblang_rt_sort_finish_input, coblang_rt_sort_release, coblang_rt_sort_return,
        reset_all_sort_sessions,
    },
};
use coblang_rt_core::{
    desc_codec::{
        DecodedTableSortPlan, RuntimeFieldSpec, RuntimeTableSortKey, encode_table_sort_plan,
    },
    fileio_wire::{
        DECL_MASK_EXTEND, DECL_MASK_FILE, DECL_MASK_INPUT, DECL_MASK_IO, DECL_MASK_OUTPUT,
        FileConnectorWireSpec, POST_IO_CONTINUE, POST_IO_FATAL_ABORT, POST_IO_FIRE_AT_END,
        POST_IO_FIRE_AT_EOP, POST_IO_FIRE_INVALID_KEY, POST_IO_FIRE_NOT_AT_END,
        POST_IO_FIRE_NOT_AT_EOP, POST_IO_FIRE_NOT_INVALID_KEY, POST_IO_INVOKE_DECL_FILE,
        POST_IO_INVOKE_DECL_INPUT, POST_IO_INVOKE_DECL_IO, POST_IO_INVOKE_DECL_OUTPUT, WireAltKey,
        WireAssignKind, WireConnectorIdentity, encode_file_connector_spec,
    },
};

/// Monotonic nonce for isolated per-test scratch directories.
static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(1);

/// Process-wide test mutex serializing shared `FILEIO_STATE` / `ACTIVE_SORT_SESSIONS` C-ABI tests.
static ABI_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquires [`ABI_TEST_LOCK`] for sort/fileio ABI tests.
fn lock_abi_test() -> std::sync::MutexGuard<'static, ()> {
    ABI_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Creates a fresh isolated temporary directory for one test while holding [`ABI_TEST_LOCK`].
fn fresh_dir(tag: &str) -> Result<(PathBuf, std::sync::MutexGuard<'static, ()>)> {
    let guard = lock_abi_test();
    let n = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("coblang-m14-abi-{tag}-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir)?;
    Ok((dir, guard))
}

/// Builds a baseline [`FileConnectorWireSpec`].
fn base_spec(unit: u32, fc: u32, org: HirFileOrganization, path: &str) -> FileConnectorWireSpec {
    FileConnectorWireSpec {
        identity: WireConnectorIdentity::UnitOwned {
            owner_unit: unit,
            owner_fc: fc,
        },
        file_name: format!("F-{unit}-{fc}"),
        organization: org,
        access_mode: HirFileAccessMode::Sequential,
        optionality: HirFileOptionality::Required,
        assign: WireAssignKind::Literal(String::from(path)),
        record_format: HirRecordFormat::Fixed,
        min_record_bytes: 8,
        max_record_bytes: 8,
        relative_key_digits: 0,
        lock_mode: HirFileLockMode::Default,
        sharing_mode: None,
        padding_byte: b' ',
        print_mode: HirPrintAdvancingMode::RawBinary,
        collating_ranks: None,
        primary_key_segments: Vec::new(),
        alternate_keys: Vec::new(),
    }
}

/// Exercises `SEQUENTIAL` `LINAGE`, `ADVANCING`, `CLOSE` reel/no-rewind dispositions, and `DECLARATIVES`.
#[test]
#[expect(unsafe_code, reason = "Direct C-ABI test harness calls.")]
fn sequential_linage_and_declaratives_abi() -> Result<()> {
    let (dir, _guard) = fresh_dir("seq-lin")?;
    let p_seq = dir.join("seq.dat");
    let mut spec = base_spec(
        1,
        1,
        HirFileOrganization::Sequential,
        p_seq.to_str().unwrap_or("seq.dat"),
    );
    spec.print_mode = HirPrintAdvancingMode::LineAdvancing;
    let sb = encode_file_connector_spec(&spec);
    let mut st = [b'0', b'0'];

    assert_eq!(coblang_rt_decl_enter(1, 10), 1);
    assert_eq!(coblang_rt_decl_enter(1, 10), 0);
    assert_eq!(coblang_rt_decl_leave(1, 10), 0);
    assert_eq!(coblang_rt_decl_enter(1, 10), 1);
    assert_eq!(coblang_rt_decl_leave(1, 10), 0);

    // SAFETY: All slice/buffer pointers and lengths passed to `coblang_rt_file_*` are valid in-bounds locals.
    unsafe {
        let rc_miss = coblang_rt_file_open(
            sb.as_ptr(),
            sb.len(),
            ptr::null(),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            DECL_MASK_INPUT,
        );
        assert_eq!(rc_miss, POST_IO_INVOKE_DECL_INPUT);
        assert_eq!(&st, b"35");

        let rc_open = coblang_rt_file_open(
            sb.as_ptr(),
            sb.len(),
            ptr::null(),
            0,
            1,
            0,
            4,
            3,
            1,
            1,
            st.as_mut_ptr(),
            DECL_MASK_FILE,
        );
        assert_eq!(rc_open, POST_IO_CONTINUE);
        assert_eq!(&st, b"00");
        assert_eq!(coblang_rt_file_last_linage_counter(), 1);

        let rc_dup = coblang_rt_file_open(
            sb.as_ptr(),
            sb.len(),
            ptr::null(),
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            DECL_MASK_OUTPUT,
        );
        assert_eq!(rc_dup, POST_IO_INVOKE_DECL_OUTPUT);
        assert_eq!(&st, b"41");

        let _ = coblang_rt_file_set_linage(sb.as_ptr(), sb.len(), 4, 3, 1, 1);
        let _ = coblang_rt_file_set_linage(sb.as_ptr(), sb.len(), 0, 0, 0, 0);
        let eop_flags = INLINE_FLAG_HAS_ON | INLINE_FLAG_HAS_NOT_ON;
        let r1 = coblang_rt_file_write(
            sb.as_ptr(),
            sb.len(),
            b"LINE0001".as_ptr(),
            8,
            0,
            0,
            0,
            1,
            eop_flags,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(r1, POST_IO_FIRE_NOT_AT_EOP);

        let r2 = coblang_rt_file_write(
            sb.as_ptr(),
            sb.len(),
            b"LINE0002".as_ptr(),
            8,
            2,
            2,
            0,
            1,
            eop_flags,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(r2, POST_IO_FIRE_AT_EOP);

        for ak in [1, 3, 4] {
            let _ = coblang_rt_file_write(
                sb.as_ptr(),
                sb.len(),
                b"LINEPAGE".as_ptr(),
                8,
                ak,
                1,
                0,
                0,
                0,
                st.as_mut_ptr(),
                0,
            );
            assert_eq!(&st, b"00");
        }

        assert_eq!(
            coblang_rt_file_close(sb.as_ptr(), sb.len(), 3, st.as_mut_ptr(), 0),
            POST_IO_CONTINUE
        );
        assert_eq!(&st, b"07");
        assert_eq!(
            coblang_rt_file_close(sb.as_ptr(), sb.len(), 2, st.as_mut_ptr(), 0),
            POST_IO_CONTINUE
        );
        assert_eq!(&st, b"07");
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Exercises raw binary `SEQUENTIAL` `READ`, `REWRITE`, `DELETE` (`"49"`), `CLOSE WITH LOCK`, and `DELETE FILE` (`"38"`).
#[test]
#[expect(unsafe_code, reason = "Direct C-ABI test harness calls.")]
fn sequential_binary_rewrite_and_close_with_lock_abi() -> Result<()> {
    let (dir, _guard) = fresh_dir("seq-bin")?;
    let p_bin = dir.join("bin.dat");
    let bin_spec = base_spec(
        1,
        2,
        HirFileOrganization::Sequential,
        p_bin.to_str().unwrap_or("bin.dat"),
    );
    let bsb = encode_file_connector_spec(&bin_spec);
    let mut st = [b'0', b'0'];

    // SAFETY: All slice/buffer pointers and lengths passed to `coblang_rt_file_*` are valid in-bounds locals.
    unsafe {
        let _ = coblang_rt_file_open(
            bsb.as_ptr(),
            bsb.len(),
            ptr::null(),
            0,
            1,
            1,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&st, b"07");
        let _ = coblang_rt_file_write(
            bsb.as_ptr(),
            bsb.len(),
            b"ABCDEFGH".as_ptr(),
            8,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        let _ = coblang_rt_file_close(bsb.as_ptr(), bsb.len(), 0, st.as_mut_ptr(), 0);
        let _ = coblang_rt_file_open(
            bsb.as_ptr(),
            bsb.len(),
            ptr::null(),
            0,
            2,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        let mut buf = [0_u8; 8];
        let r_ok = coblang_rt_file_read(
            bsb.as_ptr(),
            bsb.len(),
            buf.as_mut_ptr(),
            8,
            0,
            0,
            ptr::null(),
            0,
            0,
            0,
            INLINE_FLAG_HAS_ON | INLINE_FLAG_HAS_NOT_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(r_ok, POST_IO_FIRE_NOT_AT_END);
        assert_eq!(&buf, b"ABCDEFGH");
        assert_eq!(coblang_rt_file_last_rec_len(), 8);

        let rw_ok = coblang_rt_file_rewrite(
            bsb.as_ptr(),
            bsb.len(),
            b"12345678".as_ptr(),
            8,
            0,
            INLINE_FLAG_HAS_NOT_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(rw_ok, POST_IO_FIRE_NOT_INVALID_KEY);

        let d_rc = coblang_rt_file_delete(
            bsb.as_ptr(),
            bsb.len(),
            buf.as_ptr(),
            8,
            0,
            0,
            st.as_mut_ptr(),
            DECL_MASK_IO,
        );
        assert_eq!(d_rc, POST_IO_INVOKE_DECL_IO);
        assert_eq!(&st, b"49");

        let r_eof = coblang_rt_file_read(
            bsb.as_ptr(),
            bsb.len(),
            buf.as_mut_ptr(),
            8,
            0,
            0,
            ptr::null(),
            0,
            0,
            0,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(r_eof, POST_IO_FIRE_AT_END);

        let _ = coblang_rt_file_close(bsb.as_ptr(), bsb.len(), 1, st.as_mut_ptr(), 0);
        let df_rc = coblang_rt_file_delete_physical(
            bsb.as_ptr(),
            bsb.len(),
            ptr::null(),
            0,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(df_rc, POST_IO_FIRE_INVALID_KEY);
        assert_eq!(&st, b"38");
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Exercises `LINE SEQUENTIAL` `< cmd` input and `> cmd` output shell pipes.
#[test]
#[expect(unsafe_code, reason = "Direct C-ABI test harness calls.")]
fn line_sequential_pipes_abi() -> Result<()> {
    let (dir, _guard) = fresh_dir("lspipe")?;
    let pipe_target = dir.join("piped_out.txt");
    let mut ls_spec = base_spec(2, 1, HirFileOrganization::LineSequential, "");
    ls_spec.assign = WireAssignKind::DynamicBuffer;
    ls_spec.record_format = HirRecordFormat::Varying;
    ls_spec.min_record_bytes = 1;
    ls_spec.max_record_bytes = 32;
    let lsb = encode_file_connector_spec(&ls_spec);
    let mut st = [b'0', b'0'];
    let out_cmd = format!("> cat > '{}'", pipe_target.display());

    // SAFETY: All slice/buffer pointers and lengths passed to `coblang_rt_file_*` are valid in-bounds locals.
    unsafe {
        let rc = coblang_rt_file_open(
            lsb.as_ptr(),
            lsb.len(),
            out_cmd.as_ptr(),
            out_cmd.len(),
            1,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(rc, POST_IO_CONTINUE);
        let _ = coblang_rt_file_write(
            lsb.as_ptr(),
            lsb.len(),
            b"HELLO PIPE   ".as_ptr(),
            13,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        let _ = coblang_rt_file_close(lsb.as_ptr(), lsb.len(), 0, st.as_mut_ptr(), 0);
    }
    assert_eq!(fs::read_to_string(&pipe_target)?.trim(), "HELLO PIPE");

    let in_cmd = String::from(
        "< printf 'FROM_PIPE
'",
    );
    // SAFETY: All slice/buffer pointers and lengths passed to `coblang_rt_file_*` are valid in-bounds locals.
    unsafe {
        let rc = coblang_rt_file_open(
            lsb.as_ptr(),
            lsb.len(),
            in_cmd.as_ptr(),
            in_cmd.len(),
            0,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(rc, POST_IO_CONTINUE);
        let mut buf = [b'?'; 16];
        let _ = coblang_rt_file_read(
            lsb.as_ptr(),
            lsb.len(),
            buf.as_mut_ptr(),
            16,
            0,
            0,
            ptr::null(),
            0,
            0,
            0,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&buf, b"FROM_PIPE       ");
        let _ = coblang_rt_file_close(lsb.as_ptr(), lsb.len(), 0, st.as_mut_ptr(), 0);
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Exercises `RELATIVE` dynamic I/O, digit-capacity overflow (`"24"`), `START FIRST`/`LAST`, `READ PREVIOUS`, and `UNLOCK`.
#[test]
#[expect(unsafe_code, reason = "Direct C-ABI test harness calls.")]
fn relative_dynamic_and_overflow_abi() -> Result<()> {
    let (dir, _guard) = fresh_dir("rel")?;
    let p_rel = dir.join("rel.dat");
    let mut rel_spec = base_spec(
        2,
        2,
        HirFileOrganization::Relative,
        p_rel.to_str().unwrap_or("rel.dat"),
    );
    rel_spec.access_mode = HirFileAccessMode::Dynamic;
    rel_spec.relative_key_digits = 1;
    let rsb = encode_file_connector_spec(&rel_spec);
    let mut st = [b'0', b'0'];

    // SAFETY: All slice/buffer pointers and lengths passed to `coblang_rt_file_*` are valid in-bounds locals.
    unsafe {
        let _ = coblang_rt_file_open(
            rsb.as_ptr(),
            rsb.len(),
            ptr::null(),
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        let _ = coblang_rt_file_write(
            rsb.as_ptr(),
            rsb.len(),
            b"SLOT0002".as_ptr(),
            8,
            0,
            0,
            2,
            2,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&st, b"00");
        let _ = coblang_rt_file_write(
            rsb.as_ptr(),
            rsb.len(),
            b"SLOT0005".as_ptr(),
            8,
            0,
            0,
            5,
            2,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        let ovf = coblang_rt_file_write(
            rsb.as_ptr(),
            rsb.len(),
            b"SLOT0010".as_ptr(),
            8,
            0,
            0,
            10,
            2,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(ovf, POST_IO_FIRE_INVALID_KEY);
        assert_eq!(&st, b"24");
        let _ = coblang_rt_file_close(rsb.as_ptr(), rsb.len(), 0, st.as_mut_ptr(), 0);

        let _ = coblang_rt_file_open(
            rsb.as_ptr(),
            rsb.len(),
            ptr::null(),
            0,
            2,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        let _ = coblang_rt_file_start(
            rsb.as_ptr(),
            rsb.len(),
            ptr::null(),
            0,
            5,
            0,
            ptr::null(),
            0,
            0,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        let mut rbuf = [0_u8; 8];
        let _ = coblang_rt_file_read(
            rsb.as_ptr(),
            rsb.len(),
            rbuf.as_mut_ptr(),
            8,
            0,
            0,
            ptr::null(),
            0,
            0,
            1,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&rbuf, b"SLOT0002");
        assert_eq!(coblang_rt_file_last_rel_key(), 2);
        let _ = coblang_rt_file_unlock(rsb.as_ptr(), rsb.len(), st.as_mut_ptr(), 0);
        assert_eq!(&st, b"00");

        let _ = coblang_rt_file_start(
            rsb.as_ptr(),
            rsb.len(),
            ptr::null(),
            0,
            6,
            0,
            ptr::null(),
            0,
            0,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        let _ = coblang_rt_file_read(
            rsb.as_ptr(),
            rsb.len(),
            rbuf.as_mut_ptr(),
            8,
            1,
            0,
            ptr::null(),
            0,
            0,
            2,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&rbuf, b"SLOT0005");

        let _ = coblang_rt_file_read(
            rsb.as_ptr(),
            rsb.len(),
            rbuf.as_mut_ptr(),
            8,
            2,
            0,
            ptr::null(),
            0,
            5,
            3,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&rbuf, b"SLOT0005");
        let _ = coblang_rt_file_rewrite(
            rsb.as_ptr(),
            rsb.len(),
            b"UPD00005".as_ptr(),
            8,
            5,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&st, b"00");
        let _ = coblang_rt_file_delete(
            rsb.as_ptr(),
            rsb.len(),
            rbuf.as_ptr(),
            8,
            5,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&st, b"00");
        let _ = coblang_rt_file_close(rsb.as_ptr(), rsb.len(), 0, st.as_mut_ptr(), 0);
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Exercises `INDEXED` ISAM `SUPPRESS WHEN ALL '*'`, `START FIRST`/`LAST`, `REWRITE`, and `DELETE`.
#[test]
#[expect(unsafe_code, reason = "Direct C-ABI test harness calls.")]
fn isam_suppress_when_and_dynamic_ops_abi() -> Result<()> {
    let (dir, _guard) = fresh_dir("isam")?;
    let p_idx = dir.join("cust.idx");
    let mut idx_spec = base_spec(
        3,
        1,
        HirFileOrganization::Indexed,
        p_idx.to_str().unwrap_or("cust.idx"),
    );
    idx_spec.access_mode = HirFileAccessMode::Dynamic;
    idx_spec.primary_key_segments = vec![HirKeySegment {
        offset: 0,
        length: 4,
    }];
    idx_spec.alternate_keys = vec![WireAltKey {
        segments: vec![HirKeySegment {
            offset: 4,
            length: 4,
        }],
        duplicates: HirDuplicateKeyPolicy::Unique,
        suppression: Some(HirAltKeySuppression::WhenAllByte(b'*')),
    }];
    let isb = encode_file_connector_spec(&idx_spec);
    let mut st = [b'0', b'0'];

    // SAFETY: All slice/buffer pointers and lengths passed to `coblang_rt_file_*` are valid in-bounds locals.
    unsafe {
        let _ = coblang_rt_file_open(
            isb.as_ptr(),
            isb.len(),
            ptr::null(),
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        for rec in [b"0001****", b"0002****", b"0003AAAA"] {
            let _ = coblang_rt_file_write(
                isb.as_ptr(),
                isb.len(),
                rec.as_ptr(),
                8,
                0,
                0,
                0,
                2,
                INLINE_FLAG_HAS_ON,
                st.as_mut_ptr(),
                0,
            );
            assert_eq!(&st, b"00");
        }
        let dup_rc = coblang_rt_file_write(
            isb.as_ptr(),
            isb.len(),
            b"0004AAAA".as_ptr(),
            8,
            0,
            0,
            0,
            2,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(dup_rc, POST_IO_FIRE_INVALID_KEY);
        assert_eq!(&st, b"22");
        let _ = coblang_rt_file_close(isb.as_ptr(), isb.len(), 0, st.as_mut_ptr(), 0);

        let _ = coblang_rt_file_open(
            isb.as_ptr(),
            isb.len(),
            ptr::null(),
            0,
            2,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        let mut rbuf = [0_u8; 8];
        let sup_rd = coblang_rt_file_read(
            isb.as_ptr(),
            isb.len(),
            rbuf.as_mut_ptr(),
            8,
            2,
            1,
            b"****".as_ptr(),
            4,
            0,
            0,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(sup_rd, POST_IO_FIRE_INVALID_KEY);
        assert_eq!(&st, b"23");

        let _ = coblang_rt_file_start(
            isb.as_ptr(),
            isb.len(),
            rbuf.as_ptr(),
            8,
            2,
            1,
            b"    ".as_ptr(),
            4,
            0,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        let _ = coblang_rt_file_read(
            isb.as_ptr(),
            isb.len(),
            rbuf.as_mut_ptr(),
            8,
            0,
            0,
            ptr::null(),
            0,
            0,
            0,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&rbuf, b"0003AAAA");

        for sk in [5, 6] {
            let _ = coblang_rt_file_start(
                isb.as_ptr(),
                isb.len(),
                rbuf.as_ptr(),
                8,
                sk,
                0,
                ptr::null(),
                0,
                0,
                INLINE_FLAG_HAS_ON,
                st.as_mut_ptr(),
                0,
            );
            assert_eq!(&st, b"00");
        }
        let _ = coblang_rt_file_rewrite(
            isb.as_ptr(),
            isb.len(),
            b"0003BBBB".as_ptr(),
            8,
            0,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&st, b"00");
        let _ = coblang_rt_file_delete(
            isb.as_ptr(),
            isb.len(),
            b"0003BBBB".as_ptr(),
            8,
            0,
            INLINE_FLAG_HAS_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&st, b"00");
        let _ = coblang_rt_file_close(isb.as_ptr(), isb.len(), 0, st.as_mut_ptr(), 0);
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Exercises simulated ISAM B+Tree page CRC32 corruption (`"30"`), fatal abort, and physical `DELETE FILE`.
#[test]
#[expect(unsafe_code, reason = "Direct C-ABI test harness calls.")]
fn isam_corrupted_page_crc32_and_physical_delete_abi() -> Result<()> {
    let (dir, _guard) = fresh_dir("isam-crc")?;
    let p_idx = dir.join("corrupt.idx");
    let mut idx_spec = base_spec(
        3,
        2,
        HirFileOrganization::Indexed,
        p_idx.to_str().unwrap_or("corrupt.idx"),
    );
    idx_spec.primary_key_segments = vec![HirKeySegment {
        offset: 0,
        length: 4,
    }];
    let isb = encode_file_connector_spec(&idx_spec);
    let mut st = [b'0', b'0'];

    // SAFETY: All slice/buffer pointers and lengths passed to `coblang_rt_file_*` are valid in-bounds locals.
    unsafe {
        let _ = coblang_rt_file_open(
            isb.as_ptr(),
            isb.len(),
            ptr::null(),
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        let _ = coblang_rt_file_write(
            isb.as_ptr(),
            isb.len(),
            b"0001DATA".as_ptr(),
            8,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        let _ = coblang_rt_file_close(isb.as_ptr(), isb.len(), 0, st.as_mut_ptr(), 0);

        let mut raw_pages = fs::read(&p_idx)?;
        if let Some(byte_slot) = raw_pages.get_mut(24) {
            *byte_slot ^= 0xFF;
        }
        fs::write(&p_idx, &raw_pages)?;

        let corrupt_rc = coblang_rt_file_open(
            isb.as_ptr(),
            isb.len(),
            ptr::null(),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            DECL_MASK_FILE,
        );
        assert_eq!(corrupt_rc, POST_IO_INVOKE_DECL_FILE);
        assert_eq!(&st, b"30");

        let fatal_rc = coblang_rt_file_open(
            isb.as_ptr(),
            isb.len(),
            ptr::null(),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            0,
        );
        assert_eq!(fatal_rc, POST_IO_FATAL_ABORT);
        set_suppress_fatal_exit_for_tests(true);
        assert_eq!(coblang_rt_file_fatal_abort(isb.as_ptr(), isb.len()), 1);
        set_suppress_fatal_exit_for_tests(false);

        let del_ok = coblang_rt_file_delete_physical(
            isb.as_ptr(),
            isb.len(),
            ptr::null(),
            0,
            INLINE_FLAG_HAS_ON | INLINE_FLAG_HAS_NOT_ON,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(del_ok, POST_IO_FIRE_NOT_INVALID_KEY);
        assert_eq!(&st, b"00");
        let del_miss = coblang_rt_file_delete_physical(
            isb.as_ptr(),
            isb.len(),
            ptr::null(),
            0,
            0,
            st.as_mut_ptr(),
            DECL_MASK_EXTEND,
        );
        assert_eq!(del_miss, POST_IO_CONTINUE);
        assert_eq!(&st, b"35");
    }
    close_all_open_connectors();
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Builds a [`RuntimeTableSortKey`] helper for `sort_abi` type-mapping tests.
fn make_sort_key(
    offset: u32,
    len: u32,
    dir: HirSortDirection,
    ty: HirType,
    overpunch: OverpunchDialect,
) -> RuntimeTableSortKey {
    RuntimeTableSortKey {
        direction: dir,
        rel_offset: offset,
        byte_len: len,
        spec: RuntimeFieldSpec {
            desc: MoveFieldDesc {
                ty,
                overpunch,
                decimal_char: b'.',
                currency_symbol: '$',
                display_style: HirDisplayStyle::IsoPicWidth,
            },
            packed_sign_rule: PackedSignPolicy::Gnu,
        },
    }
}

/// Exercises `SORT` (with `DUPLICATES IN ORDER`) and `MERGE` (with custom reverse collation) C-ABI.
#[test]
#[expect(unsafe_code, reason = "Direct C-ABI test harness calls.")]
fn sort_and_merge_execution_abi() -> Result<()> {
    let _guard = lock_abi_test();
    let d4 = DigitCount::try_new(4)?;
    let keys = vec![
        make_sort_key(
            0,
            4,
            HirSortDirection::Ascending,
            HirType::ZonedDecimal {
                digits: d4,
                scale: ScaleFactor::ZERO,
                sign: NumericSignSpec::Unsigned,
                blank_zero: BlankZeroSpec::RetainZero,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            4,
            4,
            HirSortDirection::Descending,
            HirType::Alphanumeric {
                byte_len: 4,
                justified: JustifiedSpec::Left,
            },
            OverpunchDialect::AsciiGnu,
        ),
    ];
    let plan = encode_table_sort_plan(&DecodedTableSortPlan {
        stride: 8,
        collating_ranks: None,
        keys,
    });

    // SAFETY: All slice/buffer pointers and lengths passed to `coblang_rt_sort_*` are valid in-bounds locals.
    unsafe {
        assert_eq!(
            coblang_rt_sort_begin(
                plan.as_ptr(),
                plan.len(),
                6,
                8,
                SORT_FLAG_DUPLICATES_IN_ORDER
            ),
            0
        );
        assert_eq!(coblang_rt_sort_release(b"0020BB".as_ptr(), 6, 0), 0);
        assert_eq!(
            coblang_rt_sort_release(b"0010AAAA_EXTRA".as_ptr(), 14, 0),
            0
        );
        assert_eq!(coblang_rt_sort_release(b"0020ZZZZ".as_ptr(), 8, 0), 0);
        assert_eq!(coblang_rt_sort_finish_input(0), 0);

        let mut out = [0_u8; 8];
        assert_eq!(coblang_rt_sort_return(out.as_mut_ptr(), 8, 32, 0), 8);
        assert_eq!(&out, b"0010AAAA");
        assert_eq!(coblang_rt_sort_return(out.as_mut_ptr(), 8, 32, 0), 8);
        assert_eq!(&out, b"0020ZZZZ");
        assert_eq!(coblang_rt_sort_return(out.as_mut_ptr(), 8, 32, 0), 6);
        assert_eq!(&out, b"0020BB  ");
        assert_eq!(
            coblang_rt_sort_return(out.as_mut_ptr(), 8, 32, 0),
            SORT_RETURN_AT_END
        );
        assert_eq!(coblang_rt_sort_end(), 0);

        let mut rev_ranks = [0_u8; 256];
        for (i, slot) in rev_ranks.iter_mut().enumerate() {
            *slot = u8::try_from(255 - i).unwrap_or(0);
        }
        let merge_plan = encode_table_sort_plan(&DecodedTableSortPlan {
            stride: 4,
            collating_ranks: Some(rev_ranks),
            keys: Vec::new(),
        });
        assert_eq!(
            coblang_rt_sort_begin(
                merge_plan.as_ptr(),
                merge_plan.len(),
                4,
                4,
                SORT_FLAG_IS_MERGE
            ),
            0
        );
        assert_eq!(coblang_rt_sort_begin_merge_stream(), 0);
        assert_eq!(coblang_rt_sort_release(b"ZZZZ".as_ptr(), 4, 0), 0);
        assert_eq!(coblang_rt_sort_release(b"BBBB".as_ptr(), 4, 0), 0);
        assert_eq!(coblang_rt_sort_begin_merge_stream(), 0);
        assert_eq!(coblang_rt_sort_release(b"MMMM".as_ptr(), 4, 0), 0);
        assert_eq!(coblang_rt_sort_finish_input(0), 0);

        let mut m4 = [0_u8; 4];
        assert_eq!(coblang_rt_sort_return(m4.as_mut_ptr(), 4, 32, 0), 4);
        assert_eq!(&m4, b"ZZZZ");
        assert_eq!(coblang_rt_sort_return(m4.as_mut_ptr(), 4, 32, 0), 4);
        assert_eq!(&m4, b"MMMM");
        assert_eq!(coblang_rt_sort_return(m4.as_mut_ptr(), 4, 32, 0), 4);
        assert_eq!(&m4, b"BBBB");
        assert_eq!(coblang_rt_sort_end(), 0);
    }
    Ok(())
}

/// Exercises all `SortKeyType` numeric/float/national arms, `SORT-RETURN = 16` early abort, and session teardown.
#[test]
#[expect(unsafe_code, reason = "Direct C-ABI test harness calls.")]
fn sort_all_key_types_and_abort_abi() -> Result<()> {
    let _guard = lock_abi_test();
    let d4 = DigitCount::try_new(4)?;
    let d5 = DigitCount::try_new(5)?;
    let rich_keys = vec![
        make_sort_key(
            0,
            3,
            HirSortDirection::Ascending,
            HirType::PackedDecimal {
                digits: d5,
                scale: ScaleFactor::ZERO,
                sign: Signedness::Signed,
                unsigned_comp6: Comp6Mode::StandardComp3,
            },
            OverpunchDialect::EbcdicIbm,
        ),
        make_sort_key(
            3,
            4,
            HirSortDirection::Ascending,
            HirType::BinaryInt {
                byte_width: 4,
                digits: d4,
                scale: ScaleFactor::ZERO,
                sign: Signedness::Signed,
                endian: BinaryEndianness::BigEndian,
                trunc: TruncationPolicy::TruncateToPic,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            7,
            4,
            HirSortDirection::Ascending,
            HirType::Index,
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            11,
            4,
            HirSortDirection::Ascending,
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits32,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            15,
            8,
            HirSortDirection::Ascending,
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits64,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            23,
            16,
            HirSortDirection::Ascending,
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits128,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            39,
            8,
            HirSortDirection::Ascending,
            HirType::FloatDecimal {
                width: FloatDecimalWidth::Decimal64,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            47,
            16,
            HirSortDirection::Ascending,
            HirType::FloatDecimal {
                width: FloatDecimalWidth::Decimal128,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            63,
            4,
            HirSortDirection::Ascending,
            HirType::National {
                char_len: 2,
                justified: JustifiedSpec::Left,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            67,
            5,
            HirSortDirection::Ascending,
            HirType::Group {
                static_byte_len: 5,
                odo_presence: DynamicOdoPresence::StaticSize,
            },
            OverpunchDialect::EbcdicIbm,
        ),
    ];
    let rich_plan = encode_table_sort_plan(&DecodedTableSortPlan {
        stride: 72,
        collating_ranks: None,
        keys: rich_keys,
    });

    // SAFETY: All slice/buffer pointers and lengths passed to `coblang_rt_sort_*` are valid in-bounds locals.
    unsafe {
        let mut m4 = [0_u8; 4];
        assert_eq!(
            coblang_rt_sort_begin(rich_plan.as_ptr(), rich_plan.len(), 72, 72, 0),
            0
        );
        assert_eq!(coblang_rt_sort_release(b"X".as_ptr(), 1, 16), 16);
        assert_eq!(coblang_rt_sort_finish_input(16), 16);
        assert_eq!(
            coblang_rt_sort_return(m4.as_mut_ptr(), 4, 32, 16),
            SORT_RETURN_ABORTED
        );
        assert_eq!(coblang_rt_sort_end(), 16);

        assert_eq!(coblang_rt_sort_begin(b"bad".as_ptr(), 3, 4, 4, 0), 16);
        assert_eq!(coblang_rt_sort_begin_merge_stream(), 16);
        assert_eq!(coblang_rt_sort_release(b"X".as_ptr(), 1, 0), 16);
        assert_eq!(coblang_rt_sort_finish_input(0), 16);
        assert_eq!(
            coblang_rt_sort_return(ptr::null_mut(), 4, 32, 0),
            SORT_RETURN_ABORTED
        );
        assert_eq!(
            coblang_rt_sort_return(m4.as_mut_ptr(), 4, 32, 0),
            SORT_RETURN_ABORTED
        );
        assert_eq!(coblang_rt_sort_end(), 16);

        let _ = coblang_rt_sort_begin(rich_plan.as_ptr(), rich_plan.len(), 72, 72, 0);
        reset_all_sort_sessions();
        cob_tidy();
    }
    Ok(())
}

/// Covers remaining `sort_abi` state-machine errors, null pointers, short-record padding, and sign/width variants.
#[test]
#[expect(unsafe_code, reason = "Direct C-ABI test harness calls.")]
fn sort_abi_edge_cases_and_all_sign_modes() -> Result<()> {
    let _guard = lock_abi_test();
    let d4 = DigitCount::try_new(4)?;
    let keys = vec![
        make_sort_key(
            0,
            4,
            HirSortDirection::Ascending,
            HirType::ZonedDecimal {
                digits: d4,
                scale: ScaleFactor::ZERO,
                sign: NumericSignSpec::EmbeddedLeading,
                blank_zero: BlankZeroSpec::BlankWhenZero,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            4,
            4,
            HirSortDirection::Ascending,
            HirType::ZonedDecimal {
                digits: d4,
                scale: ScaleFactor::ZERO,
                sign: NumericSignSpec::EmbeddedTrailing,
                blank_zero: BlankZeroSpec::RetainZero,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            8,
            5,
            HirSortDirection::Ascending,
            HirType::ZonedDecimal {
                digits: d4,
                scale: ScaleFactor::ZERO,
                sign: NumericSignSpec::SeparateLeading,
                blank_zero: BlankZeroSpec::RetainZero,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            13,
            5,
            HirSortDirection::Ascending,
            HirType::ZonedDecimal {
                digits: d4,
                scale: ScaleFactor::ZERO,
                sign: NumericSignSpec::SeparateTrailing,
                blank_zero: BlankZeroSpec::RetainZero,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            18,
            3,
            HirSortDirection::Ascending,
            HirType::ZonedDecimal {
                digits: d4,
                scale: ScaleFactor::ZERO,
                sign: NumericSignSpec::Unsigned,
                blank_zero: BlankZeroSpec::RetainZero,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            21,
            2,
            HirSortDirection::Ascending,
            HirType::PackedDecimal {
                digits: d4,
                scale: ScaleFactor::ZERO,
                sign: Signedness::Unsigned,
                unsigned_comp6: Comp6Mode::UnsignedComp6,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            23,
            3,
            HirSortDirection::Ascending,
            HirType::PackedDecimal {
                digits: d4,
                scale: ScaleFactor::ZERO,
                sign: Signedness::Unsigned,
                unsigned_comp6: Comp6Mode::StandardComp3,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            26,
            5,
            HirSortDirection::Ascending,
            HirType::PackedDecimal {
                digits: d4,
                scale: ScaleFactor::ZERO,
                sign: Signedness::Signed,
                unsigned_comp6: Comp6Mode::StandardComp3,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            31,
            4,
            HirSortDirection::Ascending,
            HirType::BinaryInt {
                byte_width: 4,
                digits: d4,
                scale: ScaleFactor::ZERO,
                sign: Signedness::Unsigned,
                endian: BinaryEndianness::Native,
                trunc: TruncationPolicy::FullBinaryRange,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            35,
            3,
            HirSortDirection::Ascending,
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits32,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            38,
            3,
            HirSortDirection::Ascending,
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits64,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            41,
            3,
            HirSortDirection::Ascending,
            HirType::FloatBinary {
                width: FloatBinaryWidth::Bits128,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            44,
            3,
            HirSortDirection::Ascending,
            HirType::FloatDecimal {
                width: FloatDecimalWidth::Decimal64,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            47,
            3,
            HirSortDirection::Ascending,
            HirType::FloatDecimal {
                width: FloatDecimalWidth::Decimal128,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            50,
            3,
            HirSortDirection::Ascending,
            HirType::National {
                char_len: 1,
                justified: JustifiedSpec::Left,
            },
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            53,
            17,
            HirSortDirection::Ascending,
            HirType::Index,
            OverpunchDialect::AsciiGnu,
        ),
        make_sort_key(
            70,
            2,
            HirSortDirection::Ascending,
            HirType::BinaryInt {
                byte_width: 4,
                digits: d4,
                scale: ScaleFactor::ZERO,
                sign: Signedness::Signed,
                endian: BinaryEndianness::Native,
                trunc: TruncationPolicy::TruncateToPic,
            },
            OverpunchDialect::AsciiGnu,
        ),
    ];
    let plan = encode_table_sort_plan(&DecodedTableSortPlan {
        stride: 72,
        collating_ranks: None,
        keys,
    });
    let oob_plan = encode_table_sort_plan(&DecodedTableSortPlan {
        stride: 8,
        collating_ranks: None,
        keys: vec![make_sort_key(
            0,
            0,
            HirSortDirection::Ascending,
            HirType::Index,
            OverpunchDialect::AsciiGnu,
        )],
    });

    // SAFETY: Intentional null-pointer and state-transition checks on audited C-ABI entrypoints.
    unsafe {
        assert_eq!(coblang_rt_sort_begin(ptr::null(), 4, 8, 8, 0), 16);
        assert_eq!(
            coblang_rt_sort_begin(plan.as_ptr(), plan.len(), 8, usize::MAX, 0),
            16
        );
        assert_eq!(
            coblang_rt_sort_begin(oob_plan.as_ptr(), oob_plan.len(), 8, 8, 0),
            16
        );

        assert_eq!(
            coblang_rt_sort_begin(plan.as_ptr(), plan.len(), 72, 72, 0),
            0
        );
        // Short record (< min=72) triggers padding; null rec_ptr returns 16; return before finish returns -2:
        assert_eq!(coblang_rt_sort_release(ptr::null(), 4, 0), 16);
        assert_eq!(coblang_rt_sort_release(b"AB".as_ptr(), 2, 0), 0);
        let mut buf = [0_u8; 4];
        assert_eq!(
            coblang_rt_sort_return(buf.as_mut_ptr(), 4, 32, 0),
            SORT_RETURN_ABORTED
        );
        // begin_merge_stream on non-MERGE session returns 16:
        assert_eq!(coblang_rt_sort_begin_merge_stream(), 16);
        let _ = coblang_rt_sort_end();

        // Release after finish_input + double finish_input + max_len=0 return:
        assert_eq!(
            coblang_rt_sort_begin(plan.as_ptr(), plan.len(), 72, 72, 0),
            0
        );
        assert_eq!(coblang_rt_sort_release(b"CD".as_ptr(), 2, 0), 0);
        assert_eq!(coblang_rt_sort_finish_input(0), 0);
        assert_eq!(coblang_rt_sort_return(ptr::null_mut(), 0, 32, 0), 72);
        assert_eq!(coblang_rt_sort_finish_input(0), 16);
        assert_eq!(coblang_rt_sort_release(b"EF".as_ptr(), 2, 0), 16);
        let _ = coblang_rt_sort_end();

        // MERGE release before begin_merge_stream returns 16:
        assert_eq!(
            coblang_rt_sort_begin(plan.as_ptr(), plan.len(), 72, 72, SORT_FLAG_IS_MERGE),
            0
        );
        assert_eq!(coblang_rt_sort_release(b"GH".as_ptr(), 2, 0), 16);
        let _ = coblang_rt_sort_end();
    }
    Ok(())
}

/// Covers null/invalid wire pointers across all `coblang_rt_file_*` entrypoints, devices, sharing modes, lock contention (`"61"`), and `"02"` duplicates.
#[test]
#[expect(unsafe_code, reason = "Direct C-ABI test harness calls.")]
fn fileio_abi_bad_pointers_devices_locks_and_duplicates() -> Result<()> {
    let (dir, _guard) = fresh_dir("edge")?;
    let mut st = [b'0', b'0'];
    let mut buf = [0_u8; 8];

    // SAFETY: Intentional null-pointer and malformed-spec boundary tests across every C-ABI entrypoint.
    unsafe {
        for (sp, sl) in [(ptr::null(), 4_usize), (b"bad".as_ptr(), 3_usize)] {
            assert_eq!(
                coblang_rt_file_open(sp, sl, ptr::null(), 0, 0, 0, 0, 0, 0, 0, ptr::null_mut(), 0),
                POST_IO_FATAL_ABORT
            );
            assert_eq!(
                coblang_rt_file_close(sp, sl, 0, ptr::null_mut(), 0),
                POST_IO_FATAL_ABORT
            );
            assert_eq!(
                coblang_rt_file_read(
                    sp,
                    sl,
                    buf.as_mut_ptr(),
                    8,
                    0,
                    0,
                    ptr::null(),
                    0,
                    0,
                    0,
                    0,
                    ptr::null_mut(),
                    0
                ),
                POST_IO_FATAL_ABORT
            );
            assert_eq!(
                coblang_rt_file_write(sp, sl, buf.as_ptr(), 8, 0, 0, 0, 0, 0, ptr::null_mut(), 0),
                POST_IO_FATAL_ABORT
            );
            assert_eq!(
                coblang_rt_file_rewrite(sp, sl, buf.as_ptr(), 8, 0, 0, ptr::null_mut(), 0),
                POST_IO_FATAL_ABORT
            );
            assert_eq!(
                coblang_rt_file_delete(sp, sl, buf.as_ptr(), 8, 0, 0, ptr::null_mut(), 0),
                POST_IO_FATAL_ABORT
            );
            assert_eq!(
                coblang_rt_file_delete_physical(sp, sl, ptr::null(), 0, 0, ptr::null_mut(), 0),
                POST_IO_FATAL_ABORT
            );
            assert_eq!(
                coblang_rt_file_start(
                    sp,
                    sl,
                    buf.as_ptr(),
                    8,
                    0,
                    0,
                    ptr::null(),
                    0,
                    0,
                    0,
                    ptr::null_mut(),
                    0
                ),
                POST_IO_FATAL_ABORT
            );
            assert_eq!(
                coblang_rt_file_unlock(sp, sl, ptr::null_mut(), 0),
                POST_IO_FATAL_ABORT
            );
            assert_eq!(coblang_rt_file_set_linage(sp, sl, 4, 2, 1, 1), 0);
        }
        set_suppress_fatal_exit_for_tests(true);
        assert_eq!(coblang_rt_file_fatal_abort(ptr::null(), 4), 1);
        set_suppress_fatal_exit_for_tests(false);
    }

    // Intra-process lock conflict ("61") + SharingMode::ReadOnly rejecting OUTPUT ("37"):
    let p_sh = dir.join("shared.dat");
    let mut s1 = base_spec(
        9,
        1,
        HirFileOrganization::Sequential,
        p_sh.to_str().unwrap_or("shared.dat"),
    );
    s1.lock_mode = HirFileLockMode::Exclusive;
    let mut s2 = base_spec(
        9,
        2,
        HirFileOrganization::Sequential,
        p_sh.to_str().unwrap_or("shared.dat"),
    );
    s2.sharing_mode = Some(coblang_hir::HirFileSharingMode::ReadOnly);
    let sb1 = encode_file_connector_spec(&s1);
    let sb2 = encode_file_connector_spec(&s2);

    // SAFETY: Valid local slices/buffers.
    unsafe {
        // ReadOnly connector opened OUTPUT -> "37":
        let _ = coblang_rt_file_open(
            sb2.as_ptr(),
            sb2.len(),
            ptr::null(),
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&st, b"37");

        // Exclusive s1 opens OUTPUT; s2 attempting INPUT gets "61" lock conflict; DELETE FILE while s1 open gets "61":
        let _ = coblang_rt_file_open(
            sb1.as_ptr(),
            sb1.len(),
            ptr::null(),
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&st, b"00");
        let _ = coblang_rt_file_open(
            sb2.as_ptr(),
            sb2.len(),
            ptr::null(),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&st, b"61");
        let _ = coblang_rt_file_delete_physical(
            sb2.as_ptr(),
            sb2.len(),
            ptr::null(),
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&st, b"61");
        let _ = coblang_rt_file_close(sb1.as_ptr(), sb1.len(), 0, st.as_mut_ptr(), 0);
    }

    // WithDuplicates + SUPPRESS WHEN returning "02" on unsuppressed duplicate, and Sequential ISAM DELETE:
    let p_dup = dir.join("dup02.idx");
    let mut dup_spec = base_spec(
        9,
        3,
        HirFileOrganization::Indexed,
        p_dup.to_str().unwrap_or("dup02.idx"),
    );
    dup_spec.identity = WireConnectorIdentity::External(String::from("EXT-DUP02"));
    dup_spec.lock_mode = HirFileLockMode::Manual;
    dup_spec.primary_key_segments = vec![HirKeySegment {
        offset: 0,
        length: 4,
    }];
    dup_spec.alternate_keys = vec![WireAltKey {
        segments: vec![HirKeySegment {
            offset: 4,
            length: 4,
        }],
        duplicates: HirDuplicateKeyPolicy::WithDuplicates,
        suppression: Some(HirAltKeySuppression::WhenAllByte(b'*')),
    }];
    let dsb = encode_file_connector_spec(&dup_spec);

    // SAFETY: Valid local slices/buffers.
    unsafe {
        let _ = coblang_rt_file_open(
            dsb.as_ptr(),
            dsb.len(),
            ptr::null(),
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        let _ = coblang_rt_file_write(
            dsb.as_ptr(),
            dsb.len(),
            b"0001SAME".as_ptr(),
            8,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        let _ = coblang_rt_file_write(
            dsb.as_ptr(),
            dsb.len(),
            b"0002SAME".as_ptr(),
            8,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&st, b"02");
        let _ = coblang_rt_file_close(dsb.as_ptr(), dsb.len(), 0, st.as_mut_ptr(), 0);

        let _ = coblang_rt_file_open(
            dsb.as_ptr(),
            dsb.len(),
            ptr::null(),
            0,
            2,
            0,
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        let _ = coblang_rt_file_read(
            dsb.as_ptr(),
            dsb.len(),
            buf.as_mut_ptr(),
            8,
            0,
            0,
            ptr::null(),
            0,
            0,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        let _ = coblang_rt_file_delete(
            dsb.as_ptr(),
            dsb.len(),
            buf.as_ptr(),
            8,
            0,
            0,
            st.as_mut_ptr(),
            0,
        );
        assert_eq!(&st, b"00");
        let _ = coblang_rt_file_close(dsb.as_ptr(), dsb.len(), 0, st.as_mut_ptr(), 0);
    }

    close_all_open_connectors();
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}
