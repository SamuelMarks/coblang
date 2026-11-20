//! Exhaustive 100% fn/line/branch coverage tests for the clean-room `libcob` and `libgcobol`
//! C-ABI surface in `coblang-rt-abi`.

use std::ffi::{c_char, c_int, c_void};
use std::fs;
use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use coblang_common::Result;
use coblang_dynload::{ResolvedEntrySymbol, register_in_binary_module};
use coblang_rt::{
    __gg__add, __gg__call, __gg__compare, __gg__display, __gg__divide, __gg__move, __gg__multiply,
    __gg__subtract, ActiveModuleFrame, C_CALLEDBY, C_COPY, C_DELETE, C_FILEINFO, C_GETPID,
    C_JUSTIFY, C_MAKEDIR, C_NARG, C_PARAMSIZE, C_PRINTABLE, C_SLEEP, C_TOLOWER, C_TOUPPER, CBL_AND,
    CBL_CHANGE_DIR, CBL_CHECK_FILE_EXIST, CBL_CLOSE_FILE, CBL_COPY_FILE, CBL_CREATE_DIR,
    CBL_CREATE_FILE, CBL_DELETE_DIR, CBL_DELETE_FILE, CBL_EQ, CBL_FLUSH_FILE, CBL_GET_CURRENT_DIR,
    CBL_IMP, CBL_NIMP, CBL_NOR, CBL_NOT, CBL_OC_NANOSLEEP, CBL_OPEN_FILE, CBL_OR, CBL_READ_FILE,
    CBL_RENAME_FILE, CBL_TOLOWER, CBL_TOUPPER, CBL_WRITE_FILE, CBL_XOR, COB_DECIMAL_NAN,
    COB_FLAG_BINARY_SWAP, COB_FLAG_BLANK_ZERO, COB_FLAG_HAVE_SIGN, COB_FLAG_JUSTIFIED,
    COB_FLAG_REAL_BINARY, COB_FLAG_SIGN_LEADING, COB_FLAG_SIGN_SEPARATE, COB_POSIX_O_APPEND,
    COB_POSIX_O_CREAT, COB_POSIX_O_RDONLY, COB_POSIX_O_RDWR, COB_POSIX_O_TRUNC, COB_POSIX_O_WRONLY,
    COB_STORE_KEEP_ON_OVERFLOW, COB_STORE_ROUND, COB_STORE_TRUNC_ON_OVERFLOW,
    COB_TYPE_ALPHANUMERIC, COB_TYPE_ALPHANUMERIC_ALL, COB_TYPE_NUMERIC_BINARY,
    COB_TYPE_NUMERIC_DISPLAY, COB_TYPE_NUMERIC_DOUBLE, COB_TYPE_NUMERIC_FLOAT,
    COB_TYPE_NUMERIC_PACKED, CobDecimal, CobField, CobFieldAttr, CobFile, CobModule,
    CobPosixStatBuf, SYSTEM, c_abi_runtime_state, cob_accept, cob_accept_arg_number,
    cob_accept_arg_value, cob_accept_command_line, cob_accept_date, cob_accept_date_yyyymmdd,
    cob_accept_day, cob_accept_day_of_week, cob_accept_day_yyyyddd, cob_accept_environment,
    cob_accept_from_with_io, cob_accept_time, cob_add_int, cob_alloc_base, cob_call,
    cob_call_error, cob_call_error_to_writer, cob_cancel, cob_cmp, cob_cmp_int, cob_cmp_uint,
    cob_decimal_add, cob_decimal_clear, cob_decimal_cmp, cob_decimal_div, cob_decimal_get_field,
    cob_decimal_init, cob_decimal_mul, cob_decimal_pow, cob_decimal_set_field,
    cob_decimal_set_llint, cob_decimal_set_ullint, cob_decimal_sub, cob_display,
    cob_display_to_writers, cob_free, cob_func, cob_get_int, cob_init_nomain, cob_inspect_after,
    cob_inspect_all, cob_inspect_before, cob_inspect_characters, cob_inspect_converting,
    cob_inspect_finish, cob_inspect_first, cob_inspect_init, cob_inspect_leading,
    cob_inspect_start, cob_inspect_trailing, cob_module_enter, cob_module_leave, cob_move,
    cob_numeric_cmp, cob_real_get_sign, cob_real_put_sign, cob_resolve, cob_set_environment,
    cob_set_int, cob_set_location, cob_stop_run, cob_string_append, cob_string_delimited,
    cob_string_finish, cob_string_init, cob_sub_int, cob_tidy, cob_types, cob_unstring_delimited,
    cob_unstring_finish, cob_unstring_init, cob_unstring_into, cob_unstring_tallying,
    coblang_rt_call_invoke, coblang_rt_cancel_by_slice, coblang_rt_copy_cli_arg,
    coblang_rt_current_narg, coblang_rt_get_call_param, coblang_rt_pop_call_frame,
    coblang_rt_push_call_frame, coblang_rt_raw_copy, gg_display_to_writer, posix_close,
    posix_errno, posix_exit, posix_open, posix_read, posix_stat, posix_write, with_c_abi_state,
};
use coblang_rt_core::accept_env::RuntimeAcceptSource;
use coblang_rt_core::with_global_runtime;

#[expect(
    unsafe_code,
    reason = "POSIX `fork`/`waitpid` declarations for testing `cob_stop_run` and `posix_exit` child termination."
)]
unsafe extern "C" {
    /// Forks a child process so `std::process::exit(0)` flushes LLVM coverage counters cleanly.
    fn fork() -> c_int;
    /// Waits for child `pid` to exit.
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
}

/// Monotonic scratch directory counter.
static SCRATCH_ID: AtomicU64 = AtomicU64::new(0);

/// Serializes stateful global-runtime C-ABI tests inside this binary.
fn serial_guard() -> MutexGuard<'static, ()> {
    /// Global test serialization lock.
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    match LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Creates a unique temporary directory for file/directory C-ABI tests.
fn make_temp_dir() -> Result<PathBuf> {
    let seq = SCRATCH_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("coblang-cabi-{}-{seq}", std::process::id()));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Builds a [`CobFieldAttr`] helper value.
const fn make_attr(field_type: u8, digits: u8, scale: i16, flags: u8) -> CobFieldAttr {
    CobFieldAttr {
        field_type,
        digits,
        scale,
        flags,
        pic_ptr: core::ptr::null(),
    }
}

/// Sample in-binary 0-arg C-ABI subprogram returning `77`.
extern "C" fn sample_subprog() -> i32 {
    77
}

/// Verifies `cob_types` descriptors, `CobDecimal` NaN/scale bounds, `CobFile`, and `validate_cob_field_*` branches.
#[test]
fn cob_types_and_field_validation_branches() {
    let mut dec = CobDecimal::default();
    assert!(!dec.is_nan());
    dec.set_nan();
    assert!(dec.is_nan());
    assert!(dec.to_fixed_decimal().is_err());
    dec.clear_zero();
    dec.scale = i32::from(i16::MAX) + 10;
    assert!(dec.to_fixed_decimal().is_err());

    let attr_variants = [
        make_attr(COB_TYPE_NUMERIC_DISPLAY, 0, 0, 0),
        make_attr(COB_TYPE_NUMERIC_DISPLAY, 4, 2, COB_FLAG_HAVE_SIGN),
        make_attr(
            COB_TYPE_NUMERIC_DISPLAY,
            4,
            0,
            COB_FLAG_HAVE_SIGN | COB_FLAG_SIGN_LEADING,
        ),
        make_attr(
            COB_TYPE_NUMERIC_DISPLAY,
            4,
            0,
            COB_FLAG_HAVE_SIGN | COB_FLAG_SIGN_SEPARATE,
        ),
        make_attr(
            COB_TYPE_NUMERIC_DISPLAY,
            4,
            0,
            COB_FLAG_HAVE_SIGN
                | COB_FLAG_SIGN_LEADING
                | COB_FLAG_SIGN_SEPARATE
                | COB_FLAG_BLANK_ZERO,
        ),
        make_attr(COB_TYPE_NUMERIC_BINARY, 9, 0, COB_FLAG_BINARY_SWAP),
        make_attr(COB_TYPE_NUMERIC_BINARY, 9, 0, COB_FLAG_REAL_BINARY),
        make_attr(COB_TYPE_NUMERIC_PACKED, 5, 1, COB_FLAG_HAVE_SIGN),
        make_attr(COB_TYPE_NUMERIC_FLOAT, 8, 0, 0),
        make_attr(COB_TYPE_NUMERIC_DOUBLE, 16, 0, 0),
        make_attr(COB_TYPE_ALPHANUMERIC, 0, 0, COB_FLAG_JUSTIFIED),
    ];
    for a in attr_variants {
        let _ = cob_types::cob_attr_to_move_desc(4, a);
    }

    let mut assign_buf = *b"input.dat";
    let mut assign_f = CobField {
        size: assign_buf.len(),
        data: assign_buf.as_mut_ptr(),
        attr: core::ptr::null(),
    };
    let file_desc = CobFile {
        select_name: core::ptr::null(),
        file_status: core::ptr::null_mut(),
        assign: &raw mut assign_f,
        record: core::ptr::null_mut(),
        variable_record: core::ptr::null_mut(),
        record_min: 1,
        record_max: 80,
        fd: -1,
        organization: 0,
        access_mode: 1,
        lock_mode: 0,
        open_mode: 0,
        flag_optional: 0,
    };
    assert_eq!(file_desc.fd, -1);

    #[expect(
        unsafe_code,
        reason = "Testing `validate_cob_field_read/write` error and empty-size paths."
    )]
    // SAFETY: Exercising null, zero-size, null-data, and valid `CobField` pointers.
    unsafe {
        assert!(cob_types::validate_cob_field_read(core::ptr::null(), "t", "f").is_err());
        assert!(cob_types::validate_cob_field_write(core::ptr::null_mut(), "t", "f").is_err());
        let mut zero_f = CobField {
            size: 0,
            data: core::ptr::null_mut(),
            attr: core::ptr::null(),
        };
        assert!(cob_types::validate_cob_field_read(&raw const zero_f, "t", "f").is_ok());
        assert!(cob_types::validate_cob_field_write(&raw mut zero_f, "t", "f").is_ok());
        let mut bad_f = CobField {
            size: 3,
            data: core::ptr::null_mut(),
            attr: core::ptr::null(),
        };
        assert!(cob_types::validate_cob_field_read(&raw const bad_f, "t", "f").is_err());
        assert!(cob_types::validate_cob_field_write(&raw mut bad_f, "t", "f").is_err());
    }
}

/// Verifies `cob_init_nomain`, `cob_stop_run`, `cob_module_enter`/`leave`, and `cob_set_location`.
#[test]
fn cob_lifecycle_and_module_frames_abi() {
    let _g = serial_guard();
    #[expect(
        unsafe_code,
        reason = "Testing lifecycle and module-frame C-ABI entrypoints."
    )]
    // SAFETY: All test pointers and C strings are valid for the duration of each call.
    unsafe {
        let arg0 = c"host_bin";
        let arg1 = c"alpha";
        let raw_args = [arg0.as_ptr(), arg1.as_ptr()];
        cob_init_nomain(2, raw_args.as_ptr());

        with_c_abi_state(|st| st.stop_run_exit_enabled = false);
        cob_stop_run(5);
        posix_exit(3);
        with_c_abi_state(|st| st.stop_run_exit_enabled = true);

        let pid1 = fork();
        if pid1 == 0 {
            cob_stop_run(0);
        } else if pid1 > 0 {
            let mut st = 0;
            let _ = waitpid(pid1, &raw mut st, 0);
        }
        let pid2 = fork();
        if pid2 == 0 {
            posix_exit(0);
        } else if pid2 > 0 {
            let mut st = 0;
            let _ = waitpid(pid2, &raw mut st, 0);
        }

        cob_set_location(
            core::ptr::null(),
            1,
            core::ptr::null(),
            core::ptr::null(),
            core::ptr::null(),
        );
        cob_module_enter(core::ptr::null_mut(), core::ptr::null_mut(), 0);
        cob_module_leave(core::ptr::null_mut());
        let mut null_mod: *mut CobModule = core::ptr::null_mut();
        let mut glob_ptr: *mut c_void = core::ptr::null_mut();
        cob_module_enter(&raw mut null_mod, &raw mut glob_ptr, 0);
        assert!(!glob_ptr.is_null());
        cob_module_leave(core::ptr::null_mut());

        let mut pbuf = *b"ABCD";
        let mut pfield = CobField {
            size: 4,
            data: pbuf.as_mut_ptr(),
            attr: core::ptr::null(),
        };
        let params: [*mut CobField; 2] = [&raw mut pfield, core::ptr::null_mut()];
        let mut m1 = CobModule {
            next: core::ptr::null_mut(),
            module_name: c"CALLER_MOD".as_ptr(),
            module_formatted_date: core::ptr::null(),
            module_source: c"caller.cob".as_ptr(),
            module_entry: core::ptr::null(),
            cob_procedure_params: params.as_ptr(),
            module_num_params: 2,
            ebcdic_sign: 0,
            decimal_point: b'.',
            currency_symbol: b'$',
            numeric_separator: b',',
            flag_filename_mapping: 0,
            flag_binary_truncate: 1,
            flag_pretty_display: 1,
            flag_host_sign: 0,
        };
        let mut m1_ptr: *mut CobModule = &raw mut m1;
        cob_module_enter(&raw mut m1_ptr, core::ptr::null_mut(), 2);
        cob_set_location(
            c"caller.cob".as_ptr(),
            42,
            c"MAIN-SEC".as_ptr(),
            c"P1".as_ptr(),
            c"CALL".as_ptr(),
        );
        cob_module_leave(m1_ptr);
        cob_tidy();
    }
}

/// Verifies `cob_resolve`, `cob_call_error`, `cob_cancel`, `cob_call`, `cob_func`, and `cob_alloc_base`.
#[test]
fn cob_subprogram_and_alloc_base_abi() {
    let _g = serial_guard();
    #[expect(unsafe_code, reason = "Testing subprogram and heap C-ABI entrypoints.")]
    // SAFETY: All test pointers and C strings are valid for the duration of each call.
    unsafe {
        let mut err_buf = Vec::new();
        assert_eq!(cob_call_error_to_writer(&mut err_buf), -1);
        assert!(cob_resolve(c"NO_SUCH_MOD_999".as_ptr()).is_null());
        assert_eq!(cob_call_error(), -1);

        register_in_binary_module("SAMPLE_SUB", ResolvedEntrySymbol::from_fn(sample_subprog));
        assert!(!cob_resolve(c"SAMPLE_SUB".as_ptr()).is_null());
        with_global_runtime(|rt| rt.set_return_code(12));
        assert_eq!(cob_func(c"SAMPLE_SUB".as_ptr(), 0, core::ptr::null()), 77);
        assert_eq!(with_global_runtime(|rt| rt.return_code()), 12);
        assert_eq!(cob_call(c"SAMPLE_SUB".as_ptr(), 0, core::ptr::null()), 77);
        let seven_args = [core::ptr::null_mut::<c_void>(); 7];
        assert_eq!(cob_call(c"SAMPLE_SUB".as_ptr(), 7, seven_args.as_ptr()), -1);
        assert_eq!(
            cob_call(c"NO_SUCH_MOD_999".as_ptr(), 0, core::ptr::null()),
            -1
        );
        cob_cancel(c"SAMPLE_SUB".as_ptr());

        let mut base_slot: *mut c_void = core::ptr::null_mut();
        let b1 = cob_alloc_base(0, &raw mut base_slot);
        assert_eq!(b1, base_slot);
        cob_free(b1);
        let b2 = cob_alloc_base(16, core::ptr::null_mut());
        cob_free(b2);

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = c_abi_runtime_state().lock();
            std::panic::resume_unwind(Box::new("poison c_abi_state"));
        }));
        cob_tidy();
    }
}

/// Verifies `cob_move`, `cob_cmp`, `cob_numeric_cmp`, `cob_cmp_int`, `cob_cmp_uint`,
/// `cob_get_int`, and `cob_set_int`.
#[test]
fn cob_move_compare_and_int_abi() {
    let _g = serial_guard();
    let num_attr = make_attr(COB_TYPE_NUMERIC_DISPLAY, 4, 0, COB_FLAG_HAVE_SIGN);
    let all_attr = make_attr(COB_TYPE_ALPHANUMERIC_ALL, 0, 0, 0);
    let mut n1_b = *b"0042";
    let mut n2_b = *b"0010";
    let mut a1_b = *b"ABC ";
    let mut a2_b = *b"XYZ ";
    let mut all_b = *b"*-";
    let mut n1 = CobField {
        size: 4,
        data: n1_b.as_mut_ptr(),
        attr: &raw const num_attr,
    };
    let mut n2 = CobField {
        size: 4,
        data: n2_b.as_mut_ptr(),
        attr: &raw const num_attr,
    };
    let mut a1 = CobField {
        size: 4,
        data: a1_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };
    let a2 = CobField {
        size: 4,
        data: a2_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };
    let all_f = CobField {
        size: 2,
        data: all_b.as_mut_ptr(),
        attr: &raw const all_attr,
    };

    #[expect(
        unsafe_code,
        reason = "Testing `cob_move`, `cob_cmp*`, and `cob_get/set_int`."
    )]
    // SAFETY: All `CobField` structs reference stack buffers alive for this test.
    unsafe {
        cob_move(core::ptr::null(), &raw mut a1);
        cob_move(&raw const a1, core::ptr::null_mut());
        cob_move(&raw const all_f, &raw mut a1);
        assert_eq!(&a1_b, b"*-*-");
        cob_move(&raw const n1, &raw mut n2);
        assert_eq!(&n2_b, b"0042");

        assert_eq!(cob_cmp(core::ptr::null(), &raw const a1), 0);
        assert_eq!(cob_cmp(&raw const a1, core::ptr::null()), 0);
        assert_eq!(cob_cmp(&raw const a1, &raw const a2), -1);
        assert_eq!(cob_cmp(&raw const a2, &raw const a1), 1);
        assert_eq!(cob_cmp(&raw const a1, &raw const a1), 0);
        assert_eq!(cob_cmp(&raw const n1, &raw const n2), 0);

        assert_eq!(cob_numeric_cmp(core::ptr::null(), &raw const n1), 0);
        assert_eq!(cob_numeric_cmp(&raw const n1, core::ptr::null()), 0);
        assert_eq!(cob_cmp_int(core::ptr::null(), 5), 0);
        assert_eq!(cob_cmp_int(&raw const n1, 42), 0);
        assert_eq!(cob_cmp_int(&raw const n1, 50), -1);
        assert_eq!(cob_cmp_uint(core::ptr::null(), 5), 0);
        assert_eq!(cob_cmp_uint(&raw const n1, 10), 1);

        assert_eq!(cob_get_int(core::ptr::null()), 0);
        assert_eq!(cob_get_int(&raw const n1), 42);
        cob_set_int(core::ptr::null_mut(), 9);
        cob_set_int(&raw mut n1, -15);
        assert_eq!(cob_get_int(&raw const n1), -15);
        cob_set_int(&raw mut a1, 7);
        assert_eq!(cob_get_int(&raw const a1), 7);
        cob_tidy();
    }
}

/// Verifies `cob_real_get_sign` and `cob_real_put_sign` across unsigned, separate, and overpunch modes.
#[test]
fn cob_real_sign_extraction_and_restoration_abi() {
    let _g = serial_guard();
    let num_attr = make_attr(COB_TYPE_NUMERIC_DISPLAY, 4, 0, COB_FLAG_HAVE_SIGN);
    let mut a1_b = *b"ABC ";
    let mut a1 = CobField {
        size: 4,
        data: a1_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };

    #[expect(
        unsafe_code,
        reason = "Testing `cob_real_get_sign` and `cob_real_put_sign`."
    )]
    // SAFETY: All `CobField` structs reference stack buffers alive for this test.
    unsafe {
        assert_eq!(cob_real_get_sign(core::ptr::null_mut()), 0);
        cob_real_put_sign(core::ptr::null_mut(), -1);
        assert_eq!(cob_real_get_sign(&raw mut a1), 0);
        cob_real_put_sign(&raw mut a1, -1);

        let sep_lead = make_attr(
            COB_TYPE_NUMERIC_DISPLAY,
            3,
            0,
            COB_FLAG_HAVE_SIGN | COB_FLAG_SIGN_SEPARATE | COB_FLAG_SIGN_LEADING,
        );
        let mut sl_b = *b"-123";
        let mut sl_f = CobField {
            size: 4,
            data: sl_b.as_mut_ptr(),
            attr: &raw const sep_lead,
        };
        assert_eq!(cob_real_get_sign(&raw mut sl_f), -1);
        cob_real_put_sign(&raw mut sl_f, 1);
        assert_eq!(sl_b.first(), Some(&b'+'));
        assert_eq!(cob_real_get_sign(&raw mut sl_f), 1);
        cob_real_put_sign(&raw mut sl_f, -1);
        assert_eq!(sl_b.first(), Some(&b'-'));

        let emb_lead = make_attr(
            COB_TYPE_NUMERIC_DISPLAY,
            3,
            0,
            COB_FLAG_HAVE_SIGN | COB_FLAG_SIGN_LEADING,
        );
        let mut empty_f = CobField {
            size: 0,
            data: core::ptr::null_mut(),
            attr: &raw const emb_lead,
        };
        assert_eq!(cob_real_get_sign(&raw mut empty_f), 0);
        cob_real_put_sign(&raw mut empty_f, -1);

        for (init_ch, expected_sign) in [
            (b'p', -1),
            (b'}', -1),
            (b'K', -1),
            (b'{', 1),
            (b'B', 1),
            (b'5', 1),
        ] {
            let mut ob = [b'1', b'2', init_ch];
            let mut of = CobField {
                size: 3,
                data: ob.as_mut_ptr(),
                attr: &raw const num_attr,
            };
            assert_eq!(cob_real_get_sign(&raw mut of), expected_sign);
        }
        let mut eb = *b"123";
        let mut ef = CobField {
            size: 3,
            data: eb.as_mut_ptr(),
            attr: &raw const emb_lead,
        };
        cob_real_put_sign(&raw mut ef, -1);
        assert_eq!(eb.first(), Some(&b'q'));
        cob_real_put_sign(&raw mut ef, 1);
        assert_eq!(eb.first(), Some(&b'1'));
    }
}

/// Verifies `cob_decimal_*`, `cob_add_int`, and `cob_sub_int` across valid, rounded, overflow, and NaN cases.
#[test]
fn cob_decimal_engine_and_overflow_branches() {
    let _g = serial_guard();
    let d2_attr = make_attr(COB_TYPE_NUMERIC_DISPLAY, 2, 0, COB_FLAG_HAVE_SIGN);
    let mut small_b = *b"12";
    let mut small_f = CobField {
        size: 2,
        data: small_b.as_mut_ptr(),
        attr: &raw const d2_attr,
    };
    let mut alnum_b = *b"    ";
    let mut alnum_f = CobField {
        size: 4,
        data: alnum_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };
    let mut bad_txt = *b"NOT#";
    let bad_f = CobField {
        size: 4,
        data: bad_txt.as_mut_ptr(),
        attr: core::ptr::null(),
    };

    let mut d1 = CobDecimal::default();
    let mut d2 = CobDecimal::default();
    #[expect(
        unsafe_code,
        reason = "Testing `cob_decimal_*` C-ABI engine and null/NaN guards."
    )]
    // SAFETY: Stack `CobDecimal` and `CobField` instances remain valid throughout.
    unsafe {
        cob_decimal_init(core::ptr::null_mut());
        cob_decimal_clear(&raw mut d1);
        cob_decimal_set_llint(core::ptr::null_mut(), 10);
        cob_decimal_set_ullint(core::ptr::null_mut(), 10);
        cob_decimal_set_ullint(&raw mut d1, 0);
        cob_decimal_set_ullint(&raw mut d1, 25);
        cob_decimal_set_llint(&raw mut d2, -5);

        cob_decimal_add(core::ptr::null_mut(), &raw const d2);
        cob_decimal_add(&raw mut d1, &raw const d2);
        assert_eq!(cob_decimal_cmp(&raw const d1, &raw const d2), 1);
        assert_eq!(cob_decimal_cmp(core::ptr::null(), &raw const d2), 0);

        cob_decimal_sub(&raw mut d1, &raw const d2);
        cob_decimal_mul(&raw mut d1, &raw const d2);
        cob_decimal_div(&raw mut d1, &raw const d2);
        cob_decimal_set_llint(&raw mut d2, 2);
        cob_decimal_pow(&raw mut d1, &raw const d2);

        assert_eq!(
            cob_decimal_get_field(core::ptr::null(), &raw mut small_f, 0),
            1
        );
        assert_eq!(
            cob_decimal_get_field(&raw const d1, core::ptr::null_mut(), 0),
            1
        );
        assert_eq!(cob_decimal_get_field(&raw const d1, &raw mut alnum_f, 0), 0);

        // Overflow keep vs modulo-truncate vs explicit trunc flag + rounding.
        cob_decimal_set_llint(&raw mut d1, 199);
        assert_eq!(
            cob_decimal_get_field(&raw const d1, &raw mut small_f, COB_STORE_KEEP_ON_OVERFLOW),
            1
        );
        assert_eq!(&small_b, b"12");
        assert_eq!(
            cob_decimal_get_field(&raw const d1, &raw mut small_f, COB_STORE_ROUND),
            1
        );
        assert_eq!(
            cob_decimal_get_field(&raw const d1, &raw mut small_f, COB_STORE_TRUNC_ON_OVERFLOW),
            0
        );

        // NaN propagation on invalid field and division by zero.
        cob_decimal_set_field(core::ptr::null_mut(), &raw const small_f);
        cob_decimal_set_field(&raw mut d1, &raw const bad_f);
        assert_eq!(d1.scale, COB_DECIMAL_NAN);
        assert_eq!(cob_decimal_get_field(&raw const d1, &raw mut small_f, 0), 1);
        assert_eq!(cob_decimal_cmp(&raw const d1, &raw const d2), 0);
        cob_decimal_add(&raw mut d1, &raw const d2);
        cob_decimal_set_llint(&raw mut d1, 10);
        cob_decimal_set_llint(&raw mut d2, 0);
        cob_decimal_div(&raw mut d1, &raw const d2);
        assert!(d1.is_nan());

        cob_set_int(&raw mut small_f, 10);
        assert_eq!(cob_add_int(&raw mut small_f, 5, 0), 0);
        assert_eq!(cob_sub_int(&raw mut small_f, 3, 0), 0);
        assert_eq!(cob_get_int(&raw const small_f), 12);
    }
}

/// Verifies `cob_display`, `cob_accept*`, and `cob_set_environment`.
#[test]
fn cob_display_accept_and_environment_abi() {
    let _g = serial_guard();
    let mut buf = *b"HELLO   ";
    let mut f = CobField {
        size: buf.len(),
        data: buf.as_mut_ptr(),
        attr: core::ptr::null(),
    };
    let ptrs = [&raw const f, core::ptr::null()];
    let mut out_v = Vec::new();
    let mut err_v = Vec::new();

    #[expect(
        unsafe_code,
        reason = "Testing `cob_display`, `cob_accept*`, and `cob_set_environment`."
    )]
    // SAFETY: All `CobField` and C string arguments are valid stack values.
    unsafe {
        let a0 = c"prog";
        let a1 = c"arg_one";
        let argv = [a0.as_ptr(), a1.as_ptr()];
        cob_init_nomain(2, argv.as_ptr());

        cob_display_to_writers(0, 1, &ptrs, &mut out_v, &mut err_v);
        cob_display_to_writers(1, 0, &ptrs, &mut out_v, &mut err_v);
        assert_eq!(out_v, b"HELLO   \n");
        assert_eq!(err_v, b"HELLO   ");
        cob_display(0, 0, 0, core::ptr::null());
        cob_display(0, 0, 1, ptrs.as_ptr());

        cob_accept_from_with_io(
            RuntimeAcceptSource::ConsoleLine,
            core::ptr::null_mut(),
            &mut Cursor::new(b"LINE\n"),
            &mut err_v,
        );
        cob_accept_from_with_io(
            RuntimeAcceptSource::ConsoleLine,
            &raw mut f,
            &mut Cursor::new(b"WORLD\n"),
            &mut err_v,
        );
        assert_eq!(&buf, b"WORLD   ");

        cob_accept_date(&raw mut f);
        cob_accept_date_yyyymmdd(&raw mut f);
        cob_accept_day(&raw mut f);
        cob_accept_day_yyyyddd(&raw mut f);
        cob_accept_day_of_week(&raw mut f);
        cob_accept_time(&raw mut f);
        cob_accept_arg_number(&raw mut f);
        cob_accept_arg_value(&raw mut f);
        cob_accept_command_line(&raw mut f);
        cob_accept(core::ptr::null_mut());

        let mut key_bytes = *b"COB_TEST_ENV_KEY";
        let mut val_bytes = *b"VAL42   ";
        let env_k = CobField {
            size: key_bytes.len(),
            data: key_bytes.as_mut_ptr(),
            attr: core::ptr::null(),
        };
        let env_v = CobField {
            size: val_bytes.len(),
            data: val_bytes.as_mut_ptr(),
            attr: core::ptr::null(),
        };
        cob_set_environment(core::ptr::null(), &raw const env_v);
        cob_set_environment(&raw const env_k, core::ptr::null());
        cob_set_environment(&raw const env_k, &raw const env_v);

        cob_accept_environment(core::ptr::null_mut());
        let mut query_b = *b"COB_TEST_ENV_KEY";
        let mut query_f = CobField {
            size: query_b.len(),
            data: query_b.as_mut_ptr(),
            attr: core::ptr::null(),
        };
        cob_accept_environment(&raw mut query_f);
        assert!(query_b.starts_with(b"VAL42"));
        let mut blank_b = *b"        ";
        let mut blank_f = CobField {
            size: blank_b.len(),
            data: blank_b.as_mut_ptr(),
            attr: core::ptr::null(),
        };
        cob_accept_environment(&raw mut blank_f);
        cob_tidy();
    }
}

/// Verifies `cob_string_*` staging, pointer updates, and overflow paths.
#[test]
fn cob_string_abi_branches() {
    let _g = serial_guard();
    let num_attr = make_attr(COB_TYPE_NUMERIC_DISPLAY, 2, 0, 0);
    let mut dst_b = *b"        ";
    let mut ptr_b = *b"01";
    let mut s1_b = *b"AB,CD";
    let mut dlm_b = *b",";
    let mut dst_f = CobField {
        size: 8,
        data: dst_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };
    let mut ptr_f = CobField {
        size: 2,
        data: ptr_b.as_mut_ptr(),
        attr: &raw const num_attr,
    };
    let s1_f = CobField {
        size: 5,
        data: s1_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };
    let dlm_f = CobField {
        size: 1,
        data: dlm_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };

    #[expect(unsafe_code, reason = "Testing `cob_string_*` C-ABI sequences.")]
    // SAFETY: All `CobField` pointers remain valid throughout each `init..finish` sequence.
    unsafe {
        cob_string_finish();
        cob_string_init(&raw mut dst_f, &raw mut ptr_f);
        cob_string_delimited(&raw const dlm_f);
        cob_string_append(core::ptr::null());
        cob_string_append(&raw const s1_f);
        cob_string_delimited(core::ptr::null());
        cob_string_append(&raw const s1_f);
        cob_string_finish();
        assert!(dst_b.starts_with(b"ABAB,CD"));

        cob_set_int(&raw mut ptr_f, 99);
        cob_string_init(&raw mut dst_f, &raw mut ptr_f);
        cob_string_append(&raw const s1_f);
        cob_string_finish();
        cob_string_init(&raw mut dst_f, core::ptr::null_mut());
        cob_string_finish();
        cob_tidy();
    }
}

/// Verifies `cob_unstring_*` staging, pointer/tally updates, normal completion, and overflow paths.
#[test]
fn cob_unstring_abi_branches() {
    let _g = serial_guard();
    let num_attr = make_attr(COB_TYPE_NUMERIC_DISPLAY, 2, 0, 0);
    let mut ptr_b = *b"01";
    let mut dlm_b = *b",";
    let mut ptr_f = CobField {
        size: 2,
        data: ptr_b.as_mut_ptr(),
        attr: &raw const num_attr,
    };
    let dlm_f = CobField {
        size: 1,
        data: dlm_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };

    #[expect(unsafe_code, reason = "Testing `cob_unstring_*` C-ABI sequences.")]
    // SAFETY: All `CobField` pointers remain valid throughout each `init..finish` sequence.
    unsafe {
        cob_unstring_finish();
        let mut u_src_b = *b"ONE,,TWO;THREE";
        let u_src = CobField {
            size: u_src_b.len(),
            data: u_src_b.as_mut_ptr(),
            attr: core::ptr::null(),
        };
        let mut semi_b = *b";";
        let semi_f = CobField {
            size: 1,
            data: semi_b.as_mut_ptr(),
            attr: core::ptr::null(),
        };
        let mut o1_b = *b"    ";
        let mut o2_b = *b"    ";
        let mut d_out_b = *b" ";
        let mut c_out_b = *b"00";
        let mut tally_b = *b"00";
        let mut o1 = CobField {
            size: 4,
            data: o1_b.as_mut_ptr(),
            attr: core::ptr::null(),
        };
        let mut o2 = CobField {
            size: 4,
            data: o2_b.as_mut_ptr(),
            attr: core::ptr::null(),
        };
        let mut d_out = CobField {
            size: 1,
            data: d_out_b.as_mut_ptr(),
            attr: core::ptr::null(),
        };
        let mut c_out = CobField {
            size: 2,
            data: c_out_b.as_mut_ptr(),
            attr: &raw const num_attr,
        };
        let mut tally = CobField {
            size: 2,
            data: tally_b.as_mut_ptr(),
            attr: &raw const num_attr,
        };

        cob_set_int(&raw mut ptr_f, 1);
        cob_unstring_init(&raw const u_src, &raw mut ptr_f, 2);
        cob_unstring_delimited(core::ptr::null(), 0);
        cob_unstring_delimited(&raw const dlm_f, 1);
        cob_unstring_delimited(&raw const semi_f, 0);
        cob_unstring_into(
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        cob_unstring_into(&raw mut o1, &raw mut d_out, &raw mut c_out);
        cob_unstring_into(&raw mut o2, core::ptr::null_mut(), core::ptr::null_mut());
        cob_unstring_tallying(&raw mut tally);
        cob_unstring_finish();
        assert_eq!(&o1_b, b"ONE ");
        assert_eq!(&o2_b, b"TWO ");
        assert_eq!(cob_get_int(&raw const tally), 2);

        let mut exact_src_b = *b"OK";
        let exact_src = CobField {
            size: 2,
            data: exact_src_b.as_mut_ptr(),
            attr: core::ptr::null(),
        };
        cob_unstring_init(&raw const exact_src, core::ptr::null_mut(), 0);
        cob_unstring_into(&raw mut o1, core::ptr::null_mut(), core::ptr::null_mut());
        cob_unstring_finish();
        assert_eq!(&o1_b, b"OK  ");
        cob_tidy();
    }
}

/// Verifies `cob_inspect_*` across `TALLYING`, `REPLACING`, and `CONVERTING` modes with `BEFORE`/`AFTER`.
#[test]
fn cob_inspect_tally_replace_and_convert_abi() {
    let _g = serial_guard();
    let num_attr = make_attr(COB_TYPE_NUMERIC_DISPLAY, 2, 0, 0);
    let mut target_b = *b"AABBAACC";
    let mut cnt_b = *b"00";
    let mut alpha_pat_b = *b"A";
    let mut omega_pat_b = *b"C";
    let mut by_x_b = *b"X";
    let mut target_f = CobField {
        size: 8,
        data: target_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };
    let mut cnt_f = CobField {
        size: 2,
        data: cnt_b.as_mut_ptr(),
        attr: &raw const num_attr,
    };
    let mut pat_a = CobField {
        size: 1,
        data: alpha_pat_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };
    let pat_c = CobField {
        size: 1,
        data: omega_pat_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };
    let mut by_x = CobField {
        size: 1,
        data: by_x_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };

    #[expect(unsafe_code, reason = "Testing `cob_inspect_*` C-ABI sequences.")]
    // SAFETY: All `CobField` descriptors point to stack buffers alive for this test.
    unsafe {
        cob_inspect_finish();
        // TALLYING mode
        cob_inspect_init(&raw mut target_f, 0);
        cob_inspect_start();
        cob_inspect_before(&raw const pat_c);
        cob_inspect_after(&raw const pat_a);
        cob_inspect_characters(core::ptr::null_mut());
        cob_inspect_characters(&raw mut cnt_f);
        cob_inspect_all(core::ptr::null_mut(), &raw const pat_a);
        cob_inspect_all(&raw mut cnt_f, &raw const pat_a);
        cob_inspect_leading(&raw mut cnt_f, &raw const pat_a);
        cob_inspect_first(&raw mut cnt_f, &raw const pat_a);
        cob_inspect_trailing(&raw mut cnt_f, &raw const pat_c);
        cob_inspect_finish();
        assert!(cob_get_int(&raw const cnt_f) > 0);

        // REPLACING + CONVERTING mode
        cob_inspect_init(&raw mut target_f, 1);
        cob_inspect_start();
        cob_inspect_first(&raw mut by_x, &raw const pat_a);
        cob_inspect_all(&raw mut by_x, &raw const pat_c);
        cob_inspect_leading(&raw mut by_x, &raw const pat_a);
        cob_inspect_trailing(&raw mut by_x, &raw const pat_a);
        cob_inspect_converting(core::ptr::null(), &raw const by_x);
        cob_inspect_converting(&raw mut pat_a, &raw const by_x);
        cob_inspect_characters(&raw mut by_x);
        cob_inspect_finish();
        assert_eq!(&target_b, b"XXXXXXXX");
        cob_tidy();
    }
}

/// Verifies `SYSTEM`, `CBL_AND`/`OR`/`XOR`/`EQ`/`IMP`/`NIMP`/`NOT`, `CBL_TOUPPER`/`TOLOWER`,
/// `C_TOUPPER`/`TOLOWER`, and `C_JUSTIFY`.
#[test]
fn cbl_bitwise_case_justify_and_system_abi() {
    let _g = serial_guard();
    #[expect(
        unsafe_code,
        reason = "Testing `SYSTEM`, bitwise, case, and justify C-ABI routines."
    )]
    // SAFETY: All test slices and C strings are valid stack buffers.
    unsafe {
        assert_eq!(SYSTEM(core::ptr::null()), -1);
        assert_eq!(SYSTEM(c"   ".as_ptr().cast::<u8>()), -1);
        assert_eq!(SYSTEM(c"true".as_ptr().cast::<u8>()), 0);
        let cmd_argv: [*mut c_void; 1] = [c"true".as_ptr().cast_mut().cast::<c_void>()];
        assert_eq!(cob_call(c"SYSTEM".as_ptr(), 1, cmd_argv.as_ptr()), 0);

        let src = [0x0F_u8, 0xAA_u8];
        let mut dst = [0xF0_u8, 0x55_u8];
        assert_eq!(CBL_AND(src.as_ptr(), dst.as_mut_ptr(), 0), 0);
        assert_eq!(CBL_AND(core::ptr::null(), dst.as_mut_ptr(), 2), -1);
        assert_eq!(CBL_AND(src.as_ptr(), dst.as_mut_ptr(), 2), 0);
        assert_eq!(CBL_OR(src.as_ptr(), dst.as_mut_ptr(), 2), 0);
        assert_eq!(CBL_XOR(src.as_ptr(), dst.as_mut_ptr(), 2), 0);
        assert_eq!(CBL_EQ(src.as_ptr(), dst.as_mut_ptr(), 2), 0);
        assert_eq!(CBL_IMP(src.as_ptr(), dst.as_mut_ptr(), 2), 0);
        assert_eq!(CBL_NIMP(src.as_ptr(), dst.as_mut_ptr(), 2), 0);
        assert_eq!(CBL_NOT(dst.as_mut_ptr(), 0), 0);
        assert_eq!(CBL_NOT(core::ptr::null_mut(), 2), -1);
        assert_eq!(CBL_NOT(dst.as_mut_ptr(), 2), 0);

        let mut text = *b"AbCd";
        assert_eq!(CBL_TOUPPER(text.as_mut_ptr(), 0), 0);
        assert_eq!(CBL_TOUPPER(core::ptr::null_mut(), 2), -1);
        assert_eq!(C_TOUPPER(text.as_mut_ptr(), 4), 0);
        assert_eq!(&text, b"ABCD");
        assert_eq!(CBL_TOLOWER(text.as_mut_ptr(), 0), 0);
        assert_eq!(CBL_TOLOWER(core::ptr::null_mut(), 2), -1);
        assert_eq!(C_TOLOWER(text.as_mut_ptr(), 4), 0);
        assert_eq!(&text, b"abcd");

        assert_eq!(C_JUSTIFY(core::ptr::null_mut(), core::ptr::null()), -1);
        let mut empty_c = [0_u8];
        assert_eq!(C_JUSTIFY(empty_c.as_mut_ptr(), core::ptr::null()), 0);
        let mut spaces_c = *b"    \0";
        assert_eq!(C_JUSTIFY(spaces_c.as_mut_ptr(), core::ptr::null()), 0);
        let mut jbuf = *b" HI \0";
        assert_eq!(C_JUSTIFY(jbuf.as_mut_ptr(), core::ptr::null()), 0);
        assert!(jbuf.starts_with(b"  HI"));
        let mode_l = b'L';
        let mode_c = b'C';
        assert_eq!(C_JUSTIFY(jbuf.as_mut_ptr(), &raw const mode_l), 0);
        assert!(jbuf.starts_with(b"HI  "));
        assert_eq!(C_JUSTIFY(jbuf.as_mut_ptr(), &raw const mode_c), 0);
        assert!(jbuf.starts_with(b" HI "));

        with_c_abi_state(|st| {
            st.module_stack.push(ActiveModuleFrame {
                param_sizes: vec![4],
                ..ActiveModuleFrame::default()
            });
        });
        let mut fbuf = *b"XY  ";
        assert_eq!(C_JUSTIFY(fbuf.as_mut_ptr(), core::ptr::null()), 0);
        assert_eq!(&fbuf, b"  XY");
        cob_tidy();
    }
}

/// Verifies `CBL_CREATE_FILE`, `CBL_WRITE_FILE`, and `CBL_FLUSH_FILE` byte-stream write paths.
#[test]
fn cbl_bytestream_create_write_and_flush_abi() -> Result<()> {
    let _g = serial_guard();
    let dir = make_temp_dir()?;
    let f1 = format!("{}/a.bin\0", dir.display());
    let mut handle = [0_u8; 4];

    #[expect(unsafe_code, reason = "Testing `CBL_CREATE/WRITE/FLUSH_FILE`.")]
    // SAFETY: Scratch paths and stack buffers are valid for every call.
    unsafe {
        assert_eq!(
            CBL_CREATE_FILE(
                f1.as_ptr(),
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null_mut()
            ),
            -1
        );
        assert_eq!(
            CBL_CREATE_FILE(
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                handle.as_mut_ptr()
            ),
            30
        );
        assert_eq!(
            CBL_CREATE_FILE(
                f1.as_ptr(),
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                handle.as_mut_ptr()
            ),
            0
        );

        let off0 = 0_u64.to_ne_bytes();
        let cnt5 = 5_u32.to_ne_bytes();
        let flag0 = 0_u8;
        let flag128 = 128_u8;
        assert_eq!(
            CBL_WRITE_FILE(
                core::ptr::null(),
                off0.as_ptr(),
                cnt5.as_ptr(),
                &raw const flag0,
                b"HELLO".as_ptr()
            ),
            -1
        );
        assert_eq!(
            CBL_WRITE_FILE(
                handle.as_ptr(),
                off0.as_ptr(),
                cnt5.as_ptr(),
                core::ptr::null(),
                core::ptr::null()
            ),
            -1
        );
        assert_eq!(
            CBL_WRITE_FILE(
                handle.as_ptr(),
                off0.as_ptr(),
                cnt5.as_ptr(),
                &raw const flag0,
                b"HELLO".as_ptr()
            ),
            0
        );
        assert_eq!(
            CBL_WRITE_FILE(
                handle.as_ptr(),
                off0.as_ptr(),
                cnt5.as_ptr(),
                &raw const flag128,
                b"WORLD".as_ptr()
            ),
            0
        );
        assert_eq!(CBL_FLUSH_FILE(core::ptr::null()), -1);
        assert_eq!(CBL_FLUSH_FILE(handle.as_ptr()), 0);
        assert_eq!(CBL_CLOSE_FILE(handle.as_ptr()), 0);
        cob_tidy();
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies `CBL_OPEN_FILE` and `CBL_READ_FILE` normal, size-probe (`flags=128`), and EOF paths.
#[test]
fn cbl_bytestream_open_and_read_abi() -> Result<()> {
    let _g = serial_guard();
    let dir = make_temp_dir()?;
    let f1 = format!("{}/a.bin\0", dir.display());
    fs::write(dir.join("a.bin"), b"HELLOWORLD")?;
    let mut handle = [0_u8; 4];

    #[expect(unsafe_code, reason = "Testing `CBL_OPEN/READ/CLOSE_FILE`.")]
    // SAFETY: Scratch paths and stack buffers are valid for every call.
    unsafe {
        let mode_rw = 3_u8;
        assert_eq!(
            CBL_OPEN_FILE(
                f1.as_ptr(),
                &raw const mode_rw,
                core::ptr::null(),
                core::ptr::null(),
                handle.as_mut_ptr()
            ),
            0
        );
        let off0 = 0_u64.to_ne_bytes();
        let flag0 = 0_u8;
        let flag128 = 128_u8;
        let mut sz_out = [0_u8; 8];
        let mut rcnt = 10_u32.to_ne_bytes();
        let mut rbuf = [0_u8; 10];
        assert_eq!(
            CBL_READ_FILE(
                core::ptr::null(),
                off0.as_ptr(),
                rcnt.as_mut_ptr(),
                &raw const flag0,
                rbuf.as_mut_ptr()
            ),
            -1
        );
        assert_eq!(
            CBL_READ_FILE(
                handle.as_ptr(),
                core::ptr::null(),
                rcnt.as_mut_ptr(),
                &raw const flag128,
                rbuf.as_mut_ptr()
            ),
            -1
        );
        assert_eq!(
            CBL_READ_FILE(
                handle.as_ptr(),
                sz_out.as_mut_ptr(),
                rcnt.as_mut_ptr(),
                &raw const flag128,
                rbuf.as_mut_ptr()
            ),
            0
        );
        assert_eq!(u64::from_ne_bytes(sz_out), 10);
        assert_eq!(
            CBL_READ_FILE(
                handle.as_ptr(),
                off0.as_ptr(),
                rcnt.as_mut_ptr(),
                core::ptr::null(),
                rbuf.as_mut_ptr()
            ),
            0
        );
        assert_eq!(&rbuf, b"HELLOWORLD");
        let off_eof = 10_u64.to_ne_bytes();
        assert_eq!(
            CBL_READ_FILE(
                handle.as_ptr(),
                off_eof.as_ptr(),
                rcnt.as_mut_ptr(),
                &raw const flag0,
                rbuf.as_mut_ptr()
            ),
            10
        );
        assert_eq!(CBL_CLOSE_FILE(handle.as_ptr()), 0);
        cob_tidy();
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies 0-byte `CBL_READ_FILE`, 0-byte truncation `CBL_WRITE_FILE`, `CBL_CLOSE_FILE`, and open modes.
#[test]
fn cbl_bytestream_truncate_close_and_open_modes_abi() -> Result<()> {
    let _g = serial_guard();
    let dir = make_temp_dir()?;
    let f1 = format!("{}/a.bin\0", dir.display());
    fs::write(dir.join("a.bin"), b"HELLOWORLD")?;
    let mut handle = [0_u8; 4];

    #[expect(unsafe_code, reason = "Testing `CBL_OPEN/WRITE/CLOSE_FILE` modes.")]
    // SAFETY: Scratch paths and stack buffers are valid for every call.
    unsafe {
        let mode_rw = 3_u8;
        assert_eq!(
            CBL_OPEN_FILE(
                f1.as_ptr(),
                &raw const mode_rw,
                core::ptr::null(),
                core::ptr::null(),
                handle.as_mut_ptr()
            ),
            0
        );
        let off0 = 0_u64.to_ne_bytes();
        let cnt0 = 0_u32.to_ne_bytes();
        let flag0 = 0_u8;
        let mut zcnt = 0_u32.to_ne_bytes();
        assert_eq!(
            CBL_READ_FILE(
                handle.as_ptr(),
                off0.as_ptr(),
                zcnt.as_mut_ptr(),
                &raw const flag0,
                core::ptr::null_mut()
            ),
            0
        );
        assert_eq!(
            CBL_WRITE_FILE(
                handle.as_ptr(),
                off0.as_ptr(),
                cnt0.as_ptr(),
                &raw const flag0,
                core::ptr::null()
            ),
            0
        );
        assert_eq!(CBL_CLOSE_FILE(core::ptr::null()), -1);
        assert_eq!(CBL_CLOSE_FILE(handle.as_ptr()), 0);
        assert_eq!(CBL_CLOSE_FILE(handle.as_ptr()), -1);

        for mode in [1_u8, 2_u8, 3_u8] {
            assert_eq!(
                CBL_OPEN_FILE(
                    f1.as_ptr(),
                    &raw const mode,
                    core::ptr::null(),
                    core::ptr::null(),
                    handle.as_mut_ptr()
                ),
                0
            );
            assert_eq!(CBL_CLOSE_FILE(handle.as_ptr()), 0);
        }
        assert_eq!(
            CBL_OPEN_FILE(
                f1.as_ptr(),
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                handle.as_mut_ptr()
            ),
            -1
        );
        cob_tidy();
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies `CBL_CHECK_FILE_EXIST`, `CBL_COPY_FILE`, `CBL_RENAME_FILE`, `CBL_DELETE_FILE`,
/// `C_DELETE`, `C_COPY`, and `C_FILEINFO`.
#[test]
fn cbl_file_metadata_copy_rename_delete_abi() -> Result<()> {
    let _g = serial_guard();
    let dir = make_temp_dir()?;
    let f1 = format!("{}/a.bin\0", dir.display());
    let f2 = format!("{}/b.bin\0", dir.display());
    let f3 = format!("{}/c.bin\0", dir.display());
    fs::write(dir.join("a.bin"), b"DATA")?;

    #[expect(
        unsafe_code,
        reason = "Testing `CBL_*` and `C$*` filesystem metadata/copy/rename/delete."
    )]
    // SAFETY: Scratch paths and stack buffers remain valid throughout.
    unsafe {
        let mut details = [0_u8; 24];
        let mut finfo = [0_u8; 16];
        assert_eq!(
            CBL_CHECK_FILE_EXIST(core::ptr::null(), core::ptr::null_mut()),
            35
        );
        assert_eq!(CBL_CHECK_FILE_EXIST(f1.as_ptr(), details.as_mut_ptr()), 0);
        assert_eq!(C_FILEINFO(f1.as_ptr(), core::ptr::null_mut()), 1);
        assert_eq!(C_FILEINFO(core::ptr::null(), finfo.as_mut_ptr()), 1);
        assert_eq!(C_FILEINFO(f1.as_ptr(), finfo.as_mut_ptr()), 0);

        assert_eq!(CBL_COPY_FILE(core::ptr::null(), f2.as_ptr()), -1);
        assert_eq!(C_COPY(f1.as_ptr(), f2.as_ptr(), core::ptr::null()), 0);
        assert_eq!(CBL_RENAME_FILE(core::ptr::null(), f3.as_ptr()), -1);
        assert_eq!(CBL_RENAME_FILE(f2.as_ptr(), f3.as_ptr()), 0);
        assert_eq!(CBL_DELETE_FILE(core::ptr::null()), -1);
        assert_eq!(CBL_DELETE_FILE(f3.as_ptr()), 0);
        assert_eq!(C_DELETE(core::ptr::null(), core::ptr::null()), 1);
        assert_eq!(C_DELETE(f1.as_ptr(), core::ptr::null()), 0);
        cob_tidy();
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies `CBL_GET_CURRENT_DIR`, `CBL_CHANGE_DIR`, `CBL_CREATE_DIR`, `CBL_DELETE_DIR`,
/// `C_MAKEDIR`, `CBL_OC_NANOSLEEP`, and `C_SLEEP`.
#[test]
fn cbl_directory_and_sleep_abi() -> Result<()> {
    let _g = serial_guard();
    let orig_cwd = std::env::current_dir()?;
    let dir = make_temp_dir()?;
    let sub1 = format!("{}/sub1\0", dir.display());
    let sub2 = format!("{}/sub2\0", dir.display());

    #[expect(unsafe_code, reason = "Testing directory and sleep C-ABI subroutines.")]
    // SAFETY: All buffers and paths are valid and CWD is restored before returning.
    unsafe {
        let mut cwd_buf = [0_u8; 512];
        assert_eq!(CBL_GET_CURRENT_DIR(0, -1, cwd_buf.as_mut_ptr()), -1);
        assert_eq!(CBL_GET_CURRENT_DIR(0, 0, cwd_buf.as_mut_ptr()), -1);
        assert_eq!(CBL_GET_CURRENT_DIR(0, 1, cwd_buf.as_mut_ptr()), 1);
        assert_eq!(CBL_GET_CURRENT_DIR(0, 512, cwd_buf.as_mut_ptr()), 0);

        assert_eq!(CBL_CREATE_DIR(core::ptr::null()), -1);
        assert_eq!(CBL_CREATE_DIR(sub1.as_ptr()), 0);
        assert_eq!(CBL_CHANGE_DIR(core::ptr::null()), -1);
        assert_eq!(CBL_CHANGE_DIR(sub1.as_ptr()), 0);
        let orig_c = format!("{}\0", orig_cwd.display());
        assert_eq!(CBL_CHANGE_DIR(orig_c.as_ptr()), 0);
        assert_eq!(CBL_DELETE_DIR(core::ptr::null()), -1);
        assert_eq!(CBL_DELETE_DIR(sub1.as_ptr()), 0);

        assert_eq!(C_MAKEDIR(sub2.as_ptr()), 0);
        let flag_d = b'D';
        assert_eq!(C_DELETE(sub2.as_ptr(), &raw const flag_d), 0);

        assert_eq!(CBL_OC_NANOSLEEP(core::ptr::null()), -1);
        let ns_bin = 1_u64.to_ne_bytes();
        assert_eq!(CBL_OC_NANOSLEEP(ns_bin.as_ptr()), 0);
        assert_eq!(C_SLEEP(core::ptr::null()), -1);
        let sec_bin = 0_u32.to_ne_bytes();
        assert_eq!(C_SLEEP(sec_bin.as_ptr()), 0);
        cob_tidy();
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies `C_NARG`, `C_PARAMSIZE`, `C_CALLEDBY`, and `try_dispatch_builtin_c_abi` through `cob_call`.
#[test]
fn c_dollar_call_frame_introspection_and_dispatch_abi() -> Result<()> {
    let _g = serial_guard();
    let dir = make_temp_dir()?;
    let sub1 = format!("{}/sub1\0", dir.display());
    let sub2 = format!("{}/sub2\0", dir.display());

    #[expect(unsafe_code, reason = "Testing call-frame and `C$*` dispatch C-ABI.")]
    // SAFETY: All buffers, paths, and frame stacks are valid and cleaned up.
    unsafe {
        let mut cwd_buf = [0_u8; 512];
        let sec_bin = 0_u32.to_ne_bytes();
        let flag_d = b'D';
        with_c_abi_state(|st| {
            st.module_stack.push(ActiveModuleFrame {
                module_name: String::from("PARENT"),
                param_ptrs: vec![1, 2],
                param_sizes: vec![6, 4],
                ..ActiveModuleFrame::default()
            });
            st.module_stack.push(ActiveModuleFrame {
                module_name: String::from("CHILD"),
                param_ptrs: vec![1, 2],
                param_sizes: vec![6, 4],
                ..ActiveModuleFrame::default()
            });
        });
        assert_eq!(C_NARG(core::ptr::null_mut()), -1);
        let mut narg = 0_i32;
        assert_eq!(C_NARG(&raw mut narg), 0);
        assert_eq!(narg, 2);
        assert_eq!(C_PARAMSIZE(core::ptr::null()), -1);
        let bad_idx = 0_i32;
        let idx1 = 1_i32;
        assert_eq!(C_PARAMSIZE(&raw const bad_idx), -1);
        assert_eq!(C_PARAMSIZE(&raw const idx1), 6);

        assert_eq!(C_CALLEDBY(core::ptr::null_mut(), 6), -1);
        assert_eq!(C_CALLEDBY(cwd_buf.as_mut_ptr(), 0), 1);
        let mut caller_b = [0_u8; 8];
        assert_eq!(C_CALLEDBY(caller_b.as_mut_ptr(), 8), 1);
        assert_eq!(&caller_b, b"PARENT  ");
        cob_tidy();
        assert_eq!(C_CALLEDBY(cwd_buf.as_mut_ptr(), 0), 0);
        assert_eq!(C_NARG(&raw mut narg), 0);
        assert_eq!(C_CALLEDBY(caller_b.as_mut_ptr(), 8), 0);

        let mut jtxt = *b"A \0";
        let argv_j = [jtxt.as_mut_ptr().cast::<c_void>(), core::ptr::null_mut()];
        assert_eq!(cob_call(c"C$JUSTIFY".as_ptr(), 2, argv_j.as_ptr()), 0);
        let argv_u = [
            jtxt.as_mut_ptr().cast::<c_void>(),
            core::ptr::dangling_mut::<c_void>(),
        ];
        assert_eq!(cob_call(c"C$TOUPPER".as_ptr(), 2, argv_u.as_ptr()), 0);
        assert_eq!(cob_call(c"C$TOLOWER".as_ptr(), 2, argv_u.as_ptr()), 0);
        let argv_s = [sec_bin.as_ptr().cast_mut().cast::<c_void>()];
        assert_eq!(cob_call(c"C$SLEEP".as_ptr(), 1, argv_s.as_ptr()), 0);
        let argv_n = [(&raw mut narg).cast::<c_void>()];
        assert_eq!(cob_call(c"C$NARG".as_ptr(), 1, argv_n.as_ptr()), 0);
        let param_args = [(&raw const idx1).cast_mut().cast::<c_void>()];
        let _ = cob_call(c"C$PARAMSIZE".as_ptr(), 1, param_args.as_ptr());
        let argv_cb = [
            caller_b.as_mut_ptr().cast::<c_void>(),
            8_usize as *mut c_void,
        ];
        assert_eq!(cob_call(c"C$CALLEDBY".as_ptr(), 2, argv_cb.as_ptr()), 0);
        let argv_mk = [sub2.as_ptr().cast_mut().cast::<c_void>()];
        assert_eq!(cob_call(c"C$MAKEDIR".as_ptr(), 1, argv_mk.as_ptr()), 0);
        let argv_dl = [
            sub2.as_ptr().cast_mut().cast::<c_void>(),
            (&raw const flag_d).cast_mut().cast::<c_void>(),
        ];
        assert_eq!(cob_call(c"C$DELETE".as_ptr(), 2, argv_dl.as_ptr()), 0);
        let copy_args = [
            sub2.as_ptr().cast_mut().cast::<c_void>(),
            sub1.as_ptr().cast_mut().cast::<c_void>(),
            core::ptr::null_mut(),
        ];
        let _ = cob_call(c"C$COPY".as_ptr(), 3, copy_args.as_ptr());
        let mut fi_b = [0_u8; 16];
        let argv_fi = [
            sub2.as_ptr().cast_mut().cast::<c_void>(),
            fi_b.as_mut_ptr().cast::<c_void>(),
        ];
        let _ = cob_call(c"C$FILEINFO".as_ptr(), 2, argv_fi.as_ptr());
        cob_tidy();
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies `__gg__*` `libgcobol` compatibility shims and `posix_*` bindings.
#[test]
fn gcobol_compat_and_posix_shims_abi() -> Result<()> {
    let _g = serial_guard();
    let dir = make_temp_dir()?;
    let pfile = format!("{}/p.txt\0", dir.display());
    let num_attr = make_attr(COB_TYPE_NUMERIC_DISPLAY, 4, 0, COB_FLAG_HAVE_SIGN);
    let mut a_b = *b"0020";
    let mut b_b = *b"0004";
    let mut a_f = CobField {
        size: 4,
        data: a_b.as_mut_ptr(),
        attr: &raw const num_attr,
    };
    let b_f = CobField {
        size: 4,
        data: b_b.as_mut_ptr(),
        attr: &raw const num_attr,
    };

    #[expect(unsafe_code, reason = "Testing `__gg__*` and `posix_*` C-ABI shims.")]
    // SAFETY: Stack `CobField` structs, buffers, and scratch paths are valid throughout.
    unsafe {
        let mut out_v = Vec::new();
        gg_display_to_writer(core::ptr::null(), 0, 0, &mut out_v);
        gg_display_to_writer(b"GC".as_ptr(), 2, 1, &mut out_v);
        assert_eq!(out_v, b"GC\n");
        __gg__display(b"".as_ptr(), 0, 0);

        assert_eq!(__gg__add(core::ptr::null_mut(), &raw const b_f, 0), 1);
        assert_eq!(__gg__add(&raw mut a_f, &raw const b_f, 0), 0);
        assert_eq!(__gg__subtract(&raw mut a_f, &raw const b_f, 0), 0);
        assert_eq!(__gg__multiply(&raw mut a_f, &raw const b_f, 0), 0);
        assert_eq!(__gg__divide(&raw mut a_f, &raw const b_f, 0), 0);
        assert_eq!(cob_get_int(&raw const a_f), 20);
        assert_eq!(__gg__compare(&raw const a_f, &raw const b_f), 1);
        __gg__move(&raw mut a_f, &raw const b_f);
        assert_eq!(cob_get_int(&raw const a_f), 4);

        register_in_binary_module("GG_SUB", ResolvedEntrySymbol::from_fn(sample_subprog));
        assert_eq!(__gg__call(c"GG_SUB".as_ptr(), 0, core::ptr::null()), 77);

        assert_eq!(posix_open(core::ptr::null(), COB_POSIX_O_RDONLY, 0), -1);
        assert_eq!(
            posix_open(c"/nonexistent_coblang_999".as_ptr(), COB_POSIX_O_RDONLY, 0),
            -1
        );
        let mut msg = [0_u8; 32];
        let _ = posix_errno(core::ptr::null_mut(), 0);
        let _ = posix_errno(msg.as_mut_ptr(), msg.len());

        let write_fd = posix_open(
            pfile.as_ptr().cast::<c_char>(),
            COB_POSIX_O_WRONLY | COB_POSIX_O_CREAT | COB_POSIX_O_TRUNC,
            0o644,
        );
        assert!(write_fd >= 0);
        assert_eq!(posix_write(write_fd, core::ptr::null(), 0), 0);
        assert_eq!(posix_write(-1, b"X".as_ptr(), 1), -1);
        assert_eq!(posix_write(write_fd, b"POSIX".as_ptr(), 5), 5);
        assert_eq!(posix_close(write_fd), 0);
        assert_eq!(posix_close(-1), -1);

        let reopen_fd = posix_open(
            pfile.as_ptr().cast::<c_char>(),
            COB_POSIX_O_RDWR | COB_POSIX_O_APPEND,
            0,
        );
        assert!(reopen_fd >= 0);
        let mut rbuf = [0_u8; 5];
        assert_eq!(posix_read(reopen_fd, core::ptr::null_mut(), 0), 0);
        assert_eq!(posix_read(-1, rbuf.as_mut_ptr(), 5), -1);
        assert_eq!(posix_read(reopen_fd, rbuf.as_mut_ptr(), 5), 5);
        assert_eq!(&rbuf, b"POSIX");
        assert_eq!(posix_close(reopen_fd), 0);

        let mut st_buf = CobPosixStatBuf::default();
        assert_eq!(
            posix_stat(
                core::ptr::null(),
                &raw mut st_buf,
                size_of::<CobPosixStatBuf>()
            ),
            -1
        );
        assert_eq!(
            posix_stat(pfile.as_ptr().cast::<c_char>(), &raw mut st_buf, 8),
            -1
        );
        assert_eq!(
            posix_stat(
                c"/nonexistent_coblang_999".as_ptr(),
                &raw mut st_buf,
                size_of::<CobPosixStatBuf>()
            ),
            -1
        );
        assert_eq!(
            posix_stat(
                pfile.as_ptr().cast::<c_char>(),
                &raw mut st_buf,
                size_of::<CobPosixStatBuf>()
            ),
            0
        );
        assert_eq!(st_buf.st_size, 5);
        cob_tidy();
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Exercises remaining null, mixed-type, sign, and invalid-descriptor branches across
/// `cob_types`, `cob_lifecycle_abi`, and `cob_field_decimal_abi`.
#[test]
fn c_abi_edge_branches_types_lifecycle_and_decimal() {
    let _g = serial_guard();
    let d0 = CobDecimal {
        limbs: [5, 0, 0, 0],
        sign: 0,
        scale: 0,
    };
    assert!(d0.to_fixed_decimal().is_ok());
    let mut dn = CobDecimal::default();
    if let Ok(z) = coblang_decimal::FixedDecimal::parse_str("0.00") {
        dn.set_from_fixed_decimal(&z);
    }
    if let Ok(nz) = coblang_decimal::FixedDecimal::parse_str("1.25") {
        dn.set_from_fixed_decimal(&nz);
    }

    let num_attr = make_attr(COB_TYPE_NUMERIC_DISPLAY, 3, 0, COB_FLAG_HAVE_SIGN);
    let mut n_b = *b"12 ";
    let mut a_b = *b"ABC";
    let mut n_f = CobField {
        size: 3,
        data: n_b.as_mut_ptr(),
        attr: &raw const num_attr,
    };
    let a_f = CobField {
        size: 3,
        data: a_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };

    #[expect(
        unsafe_code,
        reason = "Testing remaining edge/null branches across lifecycle and decimal C-ABI modules."
    )]
    // SAFETY: Stack structs and intentional null/invalid field views are guarded by each C-ABI entrypoint.
    unsafe {
        let mut m_base = CobModule {
            next: core::ptr::null_mut(),
            module_name: c"M1".as_ptr(),
            module_formatted_date: core::ptr::null(),
            module_source: core::ptr::null(),
            module_entry: core::ptr::null(),
            cob_procedure_params: core::ptr::null(),
            module_num_params: 0,
            ebcdic_sign: 0,
            decimal_point: b'.',
            currency_symbol: b'$',
            numeric_separator: b',',
            flag_filename_mapping: 0,
            flag_binary_truncate: 0,
            flag_pretty_display: 0,
            flag_host_sign: 0,
        };
        let mut mp: *mut CobModule = &raw mut m_base;
        cob_module_enter(&raw mut mp, core::ptr::null_mut(), 0);
        cob_module_enter(&raw mut mp, core::ptr::null_mut(), -1);
        cob_module_enter(&raw mut mp, core::ptr::null_mut(), 2);
        cob_module_leave(mp);
        cob_module_leave(mp);
        cob_module_leave(mp);

        register_in_binary_module("EDGE_SUB", ResolvedEntrySymbol::from_fn(sample_subprog));
        assert_eq!(cob_call(c"EDGE_SUB".as_ptr(), -1, core::ptr::null()), 77);
        assert_eq!(cob_call(c"EDGE_SUB".as_ptr(), 2, core::ptr::null()), 77);

        let _ = cob_cmp(&raw const n_f, &raw const a_f);
        cob_real_put_sign(&raw mut n_f, -1);
        n_b.copy_from_slice(b"123");
        cob_real_put_sign(&raw mut n_f, -1);
        cob_real_put_sign(&raw mut n_f, -1);
        n_b.copy_from_slice(b"123");
        cob_real_put_sign(&raw mut n_f, 1);
        assert_eq!(n_b.len(), 3);
        cob_decimal_add(&raw mut dn, core::ptr::null());
        assert_eq!(cob_decimal_cmp(&raw const dn, core::ptr::null()), 0);
        cob_tidy();
    }
}

/// Exercises remaining null/invalid-descriptor branches across `cob_io_string_abi` and `cob_gcobol_posix_abi`.
#[test]
fn c_abi_edge_branches_io_string_and_gcobol_posix() {
    let _g = serial_guard();
    let num_attr = make_attr(COB_TYPE_NUMERIC_DISPLAY, 3, 0, COB_FLAG_HAVE_SIGN);
    let pack_attr = make_attr(COB_TYPE_NUMERIC_PACKED, 3, 0, COB_FLAG_HAVE_SIGN);
    let mut n_b = *b"12 ";
    let mut a_b = *b"ABC";
    let mut bad_bcd = [0xFF_u8, 0xFF_u8];
    let mut n_f = CobField {
        size: 3,
        data: n_b.as_mut_ptr(),
        attr: &raw const num_attr,
    };
    let a_f = CobField {
        size: 3,
        data: a_b.as_mut_ptr(),
        attr: core::ptr::null(),
    };
    let bad_pack_f = CobField {
        size: 2,
        data: bad_bcd.as_mut_ptr(),
        attr: &raw const pack_attr,
    };
    let mut invalid_f = CobField {
        size: 3,
        data: core::ptr::null_mut(),
        attr: core::ptr::null(),
    };
    let mut zero_f = CobField {
        size: 0,
        data: core::ptr::null_mut(),
        attr: core::ptr::null(),
    };

    #[expect(
        unsafe_code,
        reason = "Testing remaining edge/null branches across string and gcobol/posix C-ABI modules."
    )]
    // SAFETY: Stack structs and intentional null/invalid field views are guarded by each C-ABI entrypoint.
    unsafe {
        let mut out_v = Vec::new();
        let mut err_v = Vec::new();
        let disp_list = [&raw const bad_pack_f];
        cob_display_to_writers(0, 0, &disp_list, &mut out_v, &mut err_v);
        cob_display(0, 0, -1, core::ptr::null());
        cob_display(0, 0, 2, core::ptr::null());

        cob_string_init(&raw mut invalid_f, core::ptr::null_mut());
        cob_string_finish();

        cob_unstring_init(&raw const a_f, core::ptr::null_mut(), 0);
        cob_unstring_into(
            &raw mut invalid_f,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        cob_unstring_finish();

        cob_unstring_init(&raw const a_f, core::ptr::null_mut(), 0);
        cob_unstring_into(&raw mut zero_f, &raw mut zero_f, core::ptr::null_mut());
        cob_unstring_finish();

        let mut p_b = *b"999";
        let mut p_f = CobField {
            size: 3,
            data: p_b.as_mut_ptr(),
            attr: &raw const num_attr,
        };
        let mut u_dst = *b"   ";
        let mut u_f = CobField {
            size: 3,
            data: u_dst.as_mut_ptr(),
            attr: core::ptr::null(),
        };
        cob_unstring_init(&raw const a_f, &raw mut p_f, 0);
        cob_unstring_into(&raw mut u_f, core::ptr::null_mut(), core::ptr::null_mut());
        cob_unstring_finish();

        cob_inspect_init(&raw mut invalid_f, 1);
        cob_inspect_characters(&raw mut invalid_f);
        cob_inspect_all(&raw mut u_f, core::ptr::null());
        cob_inspect_all(&raw mut u_f, &raw const invalid_f);
        cob_inspect_all(&raw mut invalid_f, &raw const a_f);
        cob_inspect_finish();

        gg_display_to_writer(core::ptr::null(), 3, 0, &mut out_v);
        assert_eq!(__gg__add(&raw mut n_f, core::ptr::null(), 0), 1);
        let _ = posix_errno(core::ptr::null_mut(), 8);
        assert_eq!(posix_read(0, core::ptr::null_mut(), 4), -1);
        assert_eq!(posix_write(1, core::ptr::null(), 4), -1);
        assert_eq!(
            posix_stat(
                c".".as_ptr(),
                core::ptr::null_mut(),
                size_of::<CobPosixStatBuf>()
            ),
            -1
        );
        cob_tidy();
    }
}

/// Exercises `CBL_OPEN_FILE` / `CBL_CREATE_FILE` / `CBL_CHECK_FILE_EXIST` null and missing-path branches.
#[test]
fn c_abi_edge_branches_cbl_open_and_exist_errors() -> Result<()> {
    let _g = serial_guard();
    let dir = make_temp_dir()?;
    let valid_f = format!("{}/ok.dat\0", dir.display());
    let missing_f = format!("{}/missing_999.dat\0", dir.display());
    let bad_dir_f = c"/nonexistent_coblang_dir_999/x.dat";
    fs::write(dir.join("ok.dat"), b"ABCD")?;

    #[expect(
        unsafe_code,
        reason = "Testing `CBL_OPEN/CREATE/CHECK` error branches."
    )]
    // SAFETY: Stack buffers and scratch paths remain valid throughout.
    unsafe {
        let src = [1_u8, 2_u8];
        assert_eq!(CBL_AND(src.as_ptr(), core::ptr::null_mut(), 2), -1);
        with_c_abi_state(|st| {
            st.module_stack.push(ActiveModuleFrame {
                param_sizes: vec![valid_f.len()],
                ..ActiveModuleFrame::default()
            });
        });
        assert_eq!(
            CBL_CHECK_FILE_EXIST(valid_f.as_ptr(), core::ptr::null_mut()),
            0
        );
        assert_eq!(
            CBL_CHECK_FILE_EXIST(missing_f.as_ptr(), core::ptr::null_mut()),
            35
        );
        cob_tidy();

        let access_read_only = 1_u8;
        let mut handle = [0_u8; 4];
        assert_eq!(
            CBL_OPEN_FILE(
                valid_f.as_ptr(),
                &raw const access_read_only,
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null_mut()
            ),
            -1
        );
        assert_eq!(
            CBL_OPEN_FILE(
                core::ptr::null(),
                &raw const access_read_only,
                core::ptr::null(),
                core::ptr::null(),
                handle.as_mut_ptr()
            ),
            35
        );
        assert_eq!(
            CBL_OPEN_FILE(
                missing_f.as_ptr(),
                &raw const access_read_only,
                core::ptr::null(),
                core::ptr::null(),
                handle.as_mut_ptr()
            ),
            35
        );
        assert_eq!(
            CBL_CREATE_FILE(
                bad_dir_f.as_ptr().cast::<u8>(),
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                handle.as_mut_ptr()
            ),
            30
        );
        cob_tidy();
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Exercises `CBL_READ_FILE` and `CBL_WRITE_FILE` null-parameter and read-only/write-only handle failures.
#[test]
fn c_abi_edge_branches_cbl_read_write_permission_and_nulls() -> Result<()> {
    let _g = serial_guard();
    let dir = make_temp_dir()?;
    let valid_f = format!("{}/ok.dat\0", dir.display());
    fs::write(dir.join("ok.dat"), b"ABCD")?;

    #[expect(unsafe_code, reason = "Testing `CBL_READ/WRITE_FILE` error branches.")]
    // SAFETY: Stack buffers and scratch paths remain valid throughout.
    unsafe {
        let access_read_only = 1_u8;
        let mut handle = [0_u8; 4];
        assert_eq!(
            CBL_OPEN_FILE(
                valid_f.as_ptr(),
                &raw const access_read_only,
                core::ptr::null(),
                core::ptr::null(),
                handle.as_mut_ptr()
            ),
            0
        );
        let off8 = 0_u64.to_ne_bytes();
        let cnt4 = 2_u32.to_ne_bytes();
        let cnt0 = 0_u32.to_ne_bytes();
        let mut rbuf = [0_u8; 4];
        assert_eq!(
            CBL_READ_FILE(
                handle.as_ptr(),
                core::ptr::null(),
                rbuf.as_mut_ptr(),
                core::ptr::null(),
                rbuf.as_mut_ptr()
            ),
            -1
        );
        assert_eq!(
            CBL_READ_FILE(
                handle.as_ptr(),
                off8.as_ptr(),
                core::ptr::null_mut(),
                core::ptr::null(),
                rbuf.as_mut_ptr()
            ),
            -1
        );
        assert_eq!(
            CBL_READ_FILE(
                handle.as_ptr(),
                off8.as_ptr(),
                cnt4.as_ptr().cast_mut(),
                core::ptr::null(),
                core::ptr::null_mut()
            ),
            -1
        );
        assert_eq!(
            CBL_WRITE_FILE(
                handle.as_ptr(),
                core::ptr::null(),
                cnt4.as_ptr(),
                core::ptr::null(),
                b"XY".as_ptr()
            ),
            -1
        );
        assert_eq!(
            CBL_WRITE_FILE(
                handle.as_ptr(),
                off8.as_ptr(),
                core::ptr::null(),
                core::ptr::null(),
                b"XY".as_ptr()
            ),
            -1
        );
        assert_eq!(
            CBL_WRITE_FILE(
                handle.as_ptr(),
                off8.as_ptr(),
                cnt4.as_ptr(),
                core::ptr::null(),
                b"XY".as_ptr()
            ),
            30
        );
        assert_eq!(
            CBL_WRITE_FILE(
                handle.as_ptr(),
                off8.as_ptr(),
                cnt0.as_ptr(),
                core::ptr::null(),
                core::ptr::null()
            ),
            30
        );
        assert_eq!(CBL_CLOSE_FILE(handle.as_ptr()), 0);
        cob_tidy();
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Exercises `COMP-5` parameter widths `1..=3` and unknown handle (`999`) branches on `CBL_*_FILE`.
#[test]
fn c_abi_edge_branches_cbl_widths_and_ghost_handles() -> Result<()> {
    let _g = serial_guard();
    let dir = make_temp_dir()?;
    let valid_f = format!("{}/ok.dat\0", dir.display());
    fs::write(dir.join("ok.dat"), b"ABCD")?;

    #[expect(unsafe_code, reason = "Testing `CBL_*_FILE` widths and ghost handles.")]
    // SAFETY: Stack buffers and scratch paths remain valid throughout.
    unsafe {
        let access_read_only = 1_u8;
        let mut handle = [0_u8; 4];
        assert_eq!(
            CBL_OPEN_FILE(
                valid_f.as_ptr(),
                &raw const access_read_only,
                core::ptr::null(),
                core::ptr::null(),
                handle.as_mut_ptr()
            ),
            0
        );
        let off8 = 0_u64.to_ne_bytes();
        let cnt4 = 2_u32.to_ne_bytes();
        let mut rbuf = [0_u8; 4];
        for (w_off, w_cnt) in [(1_usize, 1_usize), (2, 2), (3, 3)] {
            with_c_abi_state(|st| {
                st.module_stack.push(ActiveModuleFrame {
                    param_sizes: vec![4, w_off, w_cnt],
                    ..ActiveModuleFrame::default()
                });
            });
            let o_b = [0_u8; 4];
            let mut c_b = [1_u8, 0, 0, 0];
            let _ = CBL_READ_FILE(
                handle.as_ptr(),
                o_b.as_ptr(),
                c_b.as_mut_ptr(),
                core::ptr::null(),
                rbuf.as_mut_ptr(),
            );
            let _ = CBL_WRITE_FILE(
                handle.as_ptr(),
                o_b.as_ptr(),
                c_b.as_ptr(),
                core::ptr::null(),
                rbuf.as_ptr(),
            );
            cob_tidy();
        }

        let ghost = 999_u32.to_ne_bytes();
        let flag128 = 128_u8;
        let mut sz8 = [0_u8; 8];
        assert_eq!(
            CBL_READ_FILE(
                ghost.as_ptr(),
                sz8.as_mut_ptr(),
                rbuf.as_mut_ptr(),
                &raw const flag128,
                rbuf.as_mut_ptr()
            ),
            -1
        );
        assert_eq!(
            CBL_READ_FILE(
                ghost.as_ptr(),
                off8.as_ptr(),
                cnt4.as_ptr().cast_mut(),
                core::ptr::null(),
                rbuf.as_mut_ptr()
            ),
            -1
        );
        assert_eq!(
            CBL_WRITE_FILE(
                ghost.as_ptr(),
                off8.as_ptr(),
                cnt4.as_ptr(),
                core::ptr::null(),
                rbuf.as_ptr()
            ),
            -1
        );
        assert_eq!(CBL_FLUSH_FILE(ghost.as_ptr()), -1);
        cob_tidy();
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Exercises filesystem error paths, ASCII sleep durations, write-only `CBL_READ_FILE`, and signal `SYSTEM`.
#[test]
fn c_abi_edge_branches_cbl_fs_sleep_and_system() -> Result<()> {
    let _g = serial_guard();
    let dir = make_temp_dir()?;
    let valid_f = format!("{}/ok.dat\0", dir.display());
    let missing_f = format!("{}/missing_999.dat\0", dir.display());
    let bad_dir_f = c"/nonexistent_coblang_dir_999/x.dat";
    fs::write(dir.join("ok.dat"), b"ABCD")?;

    #[expect(
        unsafe_code,
        reason = "Testing `CBL_*` / `C$*` error and signal branches."
    )]
    // SAFETY: Stack buffers and scratch paths remain valid throughout.
    unsafe {
        let mut finfo = [0_u8; 16];
        assert_eq!(C_FILEINFO(missing_f.as_ptr(), finfo.as_mut_ptr()), 1);
        assert_eq!(CBL_COPY_FILE(missing_f.as_ptr(), valid_f.as_ptr()), -1);
        assert_eq!(CBL_RENAME_FILE(missing_f.as_ptr(), valid_f.as_ptr()), -1);
        assert_eq!(CBL_DELETE_FILE(missing_f.as_ptr()), -1);
        let flag_low_d = b'd';
        assert_eq!(C_DELETE(missing_f.as_ptr(), &raw const flag_low_d), 1);
        assert_eq!(CBL_GET_CURRENT_DIR(0, 16, core::ptr::null_mut()), -1);
        assert_eq!(CBL_CHANGE_DIR(missing_f.as_ptr()), -1);
        assert_eq!(CBL_CREATE_DIR(bad_dir_f.as_ptr().cast::<u8>()), -1);
        assert_eq!(CBL_DELETE_DIR(missing_f.as_ptr()), -1);

        with_c_abi_state(|st| {
            st.module_stack.push(ActiveModuleFrame {
                param_sizes: vec![2],
                ..ActiveModuleFrame::default()
            });
        });
        assert_eq!(CBL_OC_NANOSLEEP(b"0 ".as_ptr()), 0);
        assert_eq!(CBL_OC_NANOSLEEP([0xFF_u8, 0xFF].as_ptr()), 0);
        assert_eq!(C_SLEEP(b"0 ".as_ptr()), 0);
        assert_eq!(C_SLEEP([0xFF_u8, 0xFF].as_ptr()), 0);
        let neg_idx = -1_i32;
        assert_eq!(C_PARAMSIZE(&raw const neg_idx), -1);
        cob_tidy();

        let access_write_only = 2_u8;
        let mut wo_handle = [0_u8; 4];
        let off8 = 0_u64.to_ne_bytes();
        let mut rbuf = [0_u8; 4];
        assert_eq!(
            CBL_OPEN_FILE(
                valid_f.as_ptr(),
                &raw const access_write_only,
                core::ptr::null(),
                core::ptr::null(),
                wo_handle.as_mut_ptr(),
            ),
            0
        );
        let mut wo_cnt = 2_u32.to_ne_bytes();
        assert_eq!(
            CBL_READ_FILE(
                wo_handle.as_ptr(),
                off8.as_ptr(),
                wo_cnt.as_mut_ptr(),
                core::ptr::null(),
                rbuf.as_mut_ptr(),
            ),
            30
        );
        assert_eq!(CBL_CLOSE_FILE(wo_handle.as_ptr()), 0);
        assert_eq!(SYSTEM(c"kill -9 $$".as_ptr().cast::<u8>()), 1);
        cob_tidy();
    }
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies `CBL_NOR`, `C_GETPID`, and `C_PRINTABLE`.
#[test]
fn c_abi_printable_getpid_and_nor() {
    let _g = serial_guard();
    #[expect(unsafe_code, reason = "Testing `CBL_NOR`, `C_GETPID`, `C_PRINTABLE`.")]
    // SAFETY: Stack buffers match target C-ABI preconditions.
    unsafe {
        assert_ne!(C_GETPID(), 0);
        let src = [0xF0_u8, 0x00];
        let mut dst = [0x0F_u8, 0x00];
        assert_eq!(CBL_NOR(src.as_ptr(), dst.as_mut_ptr(), 2), 0);
        assert_eq!(dst, [0x00, 0xFF]);

        assert_eq!(C_PRINTABLE(core::ptr::null_mut(), core::ptr::null()), -1);
        let mut p_nul = [0x01_u8, b'B', b'C', 0];
        assert_eq!(C_PRINTABLE(p_nul.as_mut_ptr(), core::ptr::null()), 0);
        assert_eq!(p_nul, [b'.', b'B', b'C', 0]);
        let repl_q = b'?';
        let mut p_raw = [0x00_u8, b'X', 0x7F_u8];
        let meta_pr = [
            p_raw.as_mut_ptr() as u64,
            3_u64,
            (&raw const repl_q) as u64,
            1_u64,
        ];
        assert_eq!(
            coblang_rt_call_invoke(
                c"parent".as_ptr(),
                b"C$PRINTABLE".as_ptr(),
                11,
                0,
                2,
                meta_pr.as_ptr(),
                core::ptr::null_mut()
            ),
            0
        );
        assert_eq!(p_raw, [b'?', b'X', b'?']);
        cob_tidy();
    }
}

/// Verifies sized and endian-aware `C$NARG`, `C$PARAMSIZE`, and `C$CALLEDBY` dispatch.
#[test]
fn c_abi_sized_endian_introspection_dispatch() {
    let _g = serial_guard();
    #[expect(unsafe_code, reason = "Testing sized `C$*` dispatch.")]
    // SAFETY: Stack buffers hold 8 valid bytes for every width/endian combination.
    unsafe {
        let mut n_buf = [0_u8; 8];
        assert_eq!(
            coblang_rt::cob_builtins_abi::try_dispatch_builtin_with_meta(
                "C$NARG",
                &[0],
                &[4],
                &[0]
            ),
            Some(-1)
        );
        assert_eq!(
            coblang_rt::cob_builtins_abi::try_dispatch_builtin_with_meta(
                "C$PARAMSIZE",
                &[0],
                &[4],
                &[0]
            ),
            Some(-1)
        );
        with_c_abi_state(|st| {
            st.module_stack.push(ActiveModuleFrame {
                module_name: String::from("caller"),
                ..ActiveModuleFrame::default()
            });
            st.module_stack.push(ActiveModuleFrame {
                module_name: String::from("callee"),
                param_ptrs: vec![1, 2],
                param_sizes: vec![9, 5],
                ..ActiveModuleFrame::default()
            });
        });
        for (w, be, idx_bytes) in [
            (1_usize, 0_u8, [1_u8, 0, 0, 0, 0, 0, 0, 0]),
            (2, 0, {
                let [a, b] = 1_i16.to_ne_bytes();
                [a, b, 0, 0, 0, 0, 0, 0]
            }),
            (2, 1, [0, 1, 0, 0, 0, 0, 0, 0]),
            (4, 0, {
                let [a, b, c, d] = 1_i32.to_ne_bytes();
                [a, b, c, d, 0, 0, 0, 0]
            }),
            (4, 1, [0, 0, 0, 1, 0, 0, 0, 0]),
            (8, 0, 1_i64.to_ne_bytes()),
            (8, 1, 1_i64.to_be_bytes()),
        ] {
            assert_eq!(
                coblang_rt::cob_builtins_abi::try_dispatch_builtin_with_meta(
                    "C$NARG",
                    &[n_buf.as_mut_ptr() as usize],
                    &[w],
                    &[be]
                ),
                Some(0)
            );
            assert_eq!(
                coblang_rt::cob_builtins_abi::try_dispatch_builtin_with_meta(
                    "C$PARAMSIZE",
                    &[idx_bytes.as_ptr() as usize],
                    &[w],
                    &[be]
                ),
                Some(9)
            );
        }
        let mut cb_buf = *b"XXXXXX";
        assert_eq!(
            coblang_rt::cob_builtins_abi::try_dispatch_builtin_with_meta(
                "C$CALLEDBY",
                &[cb_buf.as_mut_ptr() as usize],
                &[6],
                &[0]
            ),
            Some(1)
        );
        assert_eq!(&cb_buf, b"caller");
        let mut cb2 = *b"XXXXXX";
        assert_eq!(
            coblang_rt::cob_builtins_abi::try_dispatch_builtin_with_meta(
                "C$CALLEDBY",
                &[cb2.as_mut_ptr() as usize, 6],
                &[6, 4],
                &[0, 0]
            ),
            Some(1)
        );
        cob_tidy();
    }
}

/// Verifies `coblang_rt_get_call_param`, `coblang_rt_current_narg`, `coblang_rt_copy_cli_arg`, and `coblang_rt_cancel_by_slice`.
#[test]
fn c_abi_m11_params_chaining_and_cancel() {
    let _g = serial_guard();
    #[expect(unsafe_code, reason = "Testing M11 parameter/chaining/cancel helpers.")]
    // SAFETY: Stack buffers remain valid throughout.
    unsafe {
        let a0 = c"prog";
        let a1 = c"HELLO";
        let argv = [a0.as_ptr(), a1.as_ptr()];
        cob_init_nomain(2, argv.as_ptr());
        assert_eq!(coblang_rt_current_narg(), 1);

        let mut ch_buf = [0_u8; 3];
        assert_eq!(coblang_rt_copy_cli_arg(9, ch_buf.as_mut_ptr(), 3), -1);
        assert_eq!(coblang_rt_copy_cli_arg(1, core::ptr::null_mut(), 0), 0);
        assert_eq!(coblang_rt_copy_cli_arg(1, core::ptr::null_mut(), 3), 3);
        assert_eq!(coblang_rt_copy_cli_arg(1, ch_buf.as_mut_ptr(), 3), 3);
        assert_eq!(&ch_buf, b"HEL");

        let mut out_sz = 99_usize;
        assert!(coblang_rt_get_call_param(0, &raw mut out_sz).is_null());
        assert_eq!(out_sz, 0);
        assert!(coblang_rt_get_call_param(0, core::ptr::null_mut()).is_null());

        with_c_abi_state(|st| {
            st.module_stack.push(ActiveModuleFrame {
                module_name: String::from("SUB"),
                param_ptrs: vec![0x1234],
                param_sizes: vec![8],
                ..ActiveModuleFrame::default()
            });
        });
        assert_eq!(coblang_rt_current_narg(), 1);
        assert_eq!(
            coblang_rt_get_call_param(0, &raw mut out_sz) as usize,
            0x1234
        );
        assert_eq!(out_sz, 8);
        cob_tidy();

        coblang_rt_cancel_by_slice(core::ptr::null(), 4);
        coblang_rt_cancel_by_slice(b"SUB".as_ptr(), 0);
        coblang_rt_cancel_by_slice([b'S', b'U', b'B', 0, b' '].as_ptr(), 5);
        coblang_rt_cancel_by_slice(b"SUB ".as_ptr(), 4);
    }
}

/// Verifies `coblang_rt_call_invoke` across registered modules, code pointers, and negative-cache misses.
#[test]
fn c_abi_m11_call_invoke_registered_and_cache() {
    let _g = serial_guard();
    #[expect(unsafe_code, reason = "Testing `coblang_rt_call_invoke` branches.")]
    // SAFETY: Stack buffers and registered entry symbols remain valid throughout.
    unsafe {
        register_in_binary_module("INV_SUB", ResolvedEntrySymbol::from_fn(sample_subprog));
        let mut rc_out = 0_i32;
        let inv_nul = [b'I', b'N', b'V', b'_', b'S', b'U', b'B', 0];
        assert_eq!(
            coblang_rt_call_invoke(
                c"main".as_ptr(),
                inv_nul.as_ptr(),
                8,
                2,
                0,
                core::ptr::null(),
                &raw mut rc_out
            ),
            0
        );
        assert_eq!(rc_out, 77);

        let fn_ptr = sample_subprog as *const u8;
        assert_eq!(
            coblang_rt_call_invoke(
                core::ptr::null(),
                fn_ptr,
                -1,
                0,
                0,
                core::ptr::null(),
                core::ptr::null_mut()
            ),
            0
        );
        for _ in 0..2 {
            assert_eq!(
                coblang_rt_call_invoke(
                    c"main".as_ptr(),
                    b"NO_MOD_999".as_ptr(),
                    10,
                    0,
                    0,
                    core::ptr::null(),
                    &raw mut rc_out
                ),
                -1
            );
        }
        cob_tidy();
    }
}

/// Verifies `coblang_rt_call_invoke` null-target, non-empty stack, and arity-error paths.
#[test]
fn c_abi_m11_call_invoke_error_and_arity_branches() {
    let _g = serial_guard();
    #[expect(
        unsafe_code,
        reason = "Testing `coblang_rt_call_invoke` error branches."
    )]
    // SAFETY: Stack buffers and registered entry symbols remain valid throughout.
    unsafe {
        register_in_binary_module("INV_SUB", ResolvedEntrySymbol::from_fn(sample_subprog));
        let mut rc_out = 0_i32;
        assert_eq!(
            coblang_rt_call_invoke(
                c"main".as_ptr(),
                core::ptr::null(),
                -1,
                1,
                0,
                core::ptr::null(),
                &raw mut rc_out
            ),
            -1
        );
        assert_eq!(
            coblang_rt_call_invoke(
                c"main".as_ptr(),
                core::ptr::null(),
                0,
                0,
                1,
                core::ptr::null(),
                &raw mut rc_out
            ),
            -1
        );
        with_c_abi_state(|st| {
            st.module_stack.push(ActiveModuleFrame::default());
        });
        assert_eq!(
            coblang_rt_call_invoke(
                c"main".as_ptr(),
                core::ptr::null(),
                3,
                1,
                0,
                core::ptr::null(),
                &raw mut rc_out
            ),
            -1
        );
        cob_tidy();

        let seven_pairs = [0_u64; 14];
        assert_eq!(
            coblang_rt_call_invoke(
                c"main".as_ptr(),
                b"INV_SUB".as_ptr(),
                7,
                1,
                7,
                seven_pairs.as_ptr(),
                &raw mut rc_out
            ),
            -1
        );
        assert_eq!(
            coblang_rt_call_invoke(
                core::ptr::null(),
                b"INV_SUB".as_ptr(),
                7,
                0,
                7,
                seven_pairs.as_ptr(),
                &raw mut rc_out
            ),
            -1
        );
        cob_tidy();
    }
}

/// Exercises all remaining `CBL_*` bitwise arms in `try_dispatch_builtin_with_meta`.
#[test]
fn c_abi_try_dispatch_builtin_bitwise_and_getpid_arms() {
    let _g = serial_guard();
    #[expect(unsafe_code, reason = "Testing `try_dispatch_builtin_with_meta` arms.")]
    // SAFETY: Stack buffers `s` and `d` hold 2 valid bytes for every call.
    unsafe {
        let s = [0xAA_u8, 0x55];
        let mut d = [0x0F_u8, 0xF0];
        for name in [
            "CBL_AND", "CBL_OR", "CBL_NOR", "CBL_XOR", "CBL_EQ", "CBL_IMP", "CBL_NIMP",
        ] {
            assert_eq!(
                coblang_rt::cob_builtins_abi::try_dispatch_builtin_with_meta(
                    name,
                    &[s.as_ptr() as usize, d.as_mut_ptr() as usize, 2],
                    &[2, 2, 4],
                    &[0, 0, 0]
                ),
                Some(0)
            );
        }
        assert_eq!(
            coblang_rt::cob_builtins_abi::try_dispatch_builtin_with_meta(
                "CBL_NOT",
                &[d.as_mut_ptr() as usize, 2],
                &[2, 4],
                &[0, 0]
            ),
            Some(0)
        );
        assert!(
            coblang_rt::cob_builtins_abi::try_dispatch_builtin_with_meta("C$GETPID", &[], &[], &[])
                .is_some()
        );
        cob_tidy();
    }
}

/// Verifies `coblang_rt_push_call_frame`, `coblang_rt_pop_call_frame`, and `coblang_rt_raw_copy`.
#[test]
fn c_abi_push_pop_call_frame_and_raw_copy_branches() {
    let _g = serial_guard();
    #[expect(
        unsafe_code,
        reason = "Testing `coblang_rt_push_call_frame`, `pop_call_frame`, and `raw_copy`."
    )]
    // SAFETY: Stack C strings and byte arrays remain valid throughout.
    unsafe {
        with_global_runtime(|rt| rt.set_return_code(21));
        let meta = [0x10_u64, 4_u64];
        let s1 = coblang_rt_push_call_frame(c"main".as_ptr(), c"sub1".as_ptr(), 1, meta.as_ptr());
        assert_eq!(s1, 21);
        let s2 =
            coblang_rt_push_call_frame(c"sub1".as_ptr(), c"sub2".as_ptr(), 0, core::ptr::null());
        assert_eq!(s2, 21);
        let _ =
            coblang_rt_push_call_frame(core::ptr::null(), c"sub3".as_ptr(), 0, core::ptr::null());
        with_global_runtime(|rt| rt.set_return_code(99));
        assert_eq!(coblang_rt_pop_call_frame(s2, 0), 99);
        assert_eq!(coblang_rt_pop_call_frame(s2, 0), 99);
        assert_eq!(coblang_rt_pop_call_frame(s1, 1), 99);
        assert_eq!(with_global_runtime(|rt| rt.return_code()), 21);
        cob_tidy();
        let _ = coblang_rt_push_call_frame(
            core::ptr::null(),
            c"sub_empty".as_ptr(),
            0,
            core::ptr::null(),
        );
        cob_tidy();
        assert_eq!(coblang_rt_pop_call_frame(0, 0), 21);

        let src = *b"ABCD";
        let mut dst = [0_u8; 4];
        coblang_rt_raw_copy(dst.as_mut_ptr(), src.as_ptr(), 0);
        coblang_rt_raw_copy(core::ptr::null_mut(), src.as_ptr(), 4);
        coblang_rt_raw_copy(dst.as_mut_ptr(), core::ptr::null(), 4);
        coblang_rt_raw_copy(dst.as_mut_ptr(), src.as_ptr(), 4);
        assert_eq!(&dst, b"ABCD");
    }
}
