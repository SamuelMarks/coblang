//! Comprehensive C-ABI unit and branch-coverage suite for `coblang_rt::m12_abi` (`M12`)
//! and `coblang_rt::m13_abi` (`M13`).

use std::ffi::c_void;
use std::ptr;
use std::sync::Mutex;

use coblang_decimal::FixedDecimal;
use coblang_intrinsics::IntrinsicId;
use coblang_rt::m12_abi::{
    CobUdfEntryFn, coblang_rt_m12_apply_refmod, coblang_rt_m12_begin, coblang_rt_m12_enter_module,
    coblang_rt_m12_eval_bytes, coblang_rt_m12_eval_to_dec, coblang_rt_m12_format_display_dec,
    coblang_rt_m12_invoke_udf, coblang_rt_m12_leave_module, coblang_rt_m12_pop_dec_arg,
    coblang_rt_m12_push_alg_probe, coblang_rt_m12_push_bytes_arg, coblang_rt_m12_push_keyword_arg,
    coblang_rt_m12_push_omitted_arg, coblang_rt_m12_register_udf, coblang_rt_m12_series_begin,
    coblang_rt_m12_series_end, coblang_rt_m12_set_context, reset_m12_state,
};
use coblang_rt::m13_abi::{
    coblang_rt_m13_check_fatal_unhandled, coblang_rt_m13_clear_last_exception,
    coblang_rt_m13_debug_prepare, coblang_rt_m13_debug_set_suppress, coblang_rt_m13_enter_decl,
    coblang_rt_m13_finish_decl, coblang_rt_m13_last_exception_off, coblang_rt_m13_match_ec,
    coblang_rt_m13_raise, coblang_rt_m13_rec_frame_begin, coblang_rt_m13_rec_frame_restore,
    coblang_rt_m13_rec_frame_save, coblang_rt_m13_set_resume, coblang_rt_m13_set_stmt_loc,
    coblang_rt_m13_set_turn, coblang_rt_m13_take_pending_ec, coblang_rt_m13_unhandled_ec,
    note_io_exception, note_runtime_exception, reset_m13_state,
};
use coblang_rt_core::exceptions::ExceptionCode;
use coblang_rt_core::with_global_runtime;

/// Serializes tests that mutate process-global M12/M13 runtime state.
static ABI_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Sample compiled UDF callback writing `"UDF!"` into `ret_ptr`.
#[expect(unsafe_code, reason = "Test UDF callback matching CobUdfEntryFn.")]
unsafe extern "C" fn sample_udf_cb(
    argc: i32,
    argv_ptrs: *const *mut u8,
    argv_lens: *const usize,
    ret_ptr: *mut u8,
    ret_len: usize,
) -> i32 {
    if argc > 0 && !argv_ptrs.is_null() && !argv_lens.is_null() && !ret_ptr.is_null() {
        // SAFETY: Test caller provides valid argument arrays and return buffer.
        unsafe {
            let p0 = *argv_ptrs;
            let l0 = (*argv_lens).min(ret_len);
            if !p0.is_null() && l0 > 0 {
                ptr::copy_nonoverlapping(p0, ret_ptr, l0);
            }
        }
    }
    42
}

/// Pushes a decimal string onto the runtime decimal stack and stages it as an M12 argument.
fn stage_dec(s: &str) {
    let d = FixedDecimal::parse_str(s).unwrap_or(FixedDecimal::ZERO);
    with_global_runtime(|rt| rt.decimal_stack_mut().push_decimal(d));
    assert_eq!(coblang_rt_m12_pop_dec_arg(), 0);
}

/// Pops the top decimal result from the runtime decimal stack.
fn pop_dec() -> FixedDecimal {
    with_global_runtime(|rt| {
        rt.decimal_stack_mut()
            .pop_decimal()
            .unwrap_or(FixedDecimal::ZERO)
    })
}

/// Stages a byte slice with `cat` (`0`=Alpha, `1`=National, `2`=Utf8, `3`=PointerSlot).
#[expect(unsafe_code, reason = "Helper wrapping coblang_rt_m12_push_bytes_arg.")]
fn stage_bytes(b: &[u8], cat: i32) {
    unsafe {
        assert_eq!(coblang_rt_m12_push_bytes_arg(b.as_ptr(), b.len(), cat), 0);
    }
}

/// Stages an uppercase modifier keyword.
#[expect(
    unsafe_code,
    reason = "Helper wrapping coblang_rt_m12_push_keyword_arg."
)]
fn stage_kw(kw: &str) {
    unsafe {
        assert_eq!(coblang_rt_m12_push_keyword_arg(kw.as_ptr(), kw.len()), 0);
    }
}

/// Evaluates the active M12 numeric frame and pops the resulting `FixedDecimal`.
fn eval_num_ok() -> FixedDecimal {
    let rc = coblang_rt_m12_eval_to_dec();
    assert!(rc == 0 || rc == 1);
    pop_dec()
}

/// Evaluates the active M12 byte frame and returns the copied `Vec<u8>`.
#[expect(unsafe_code, reason = "Helper wrapping coblang_rt_m12_eval_bytes.")]
fn eval_bytes_vec(expected_rc: i32) -> Vec<u8> {
    unsafe {
        let mut p: *const u8 = ptr::null();
        let mut l: i64 = 0;
        let rc = coblang_rt_m12_eval_bytes(&raw mut p, &raw mut l);
        if expected_rc == 1 {
            assert_eq!(rc, 1);
        } else {
            assert!(rc == 0 || rc == 1);
        }
        let len = usize::try_from(l.max(0)).unwrap_or(0);
        if len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(p, len).to_vec()
        }
    }
}

/// Exercises all numeric/integer intrinsics, `ALL` series, algebraic probes, `CONTENT-LENGTH`,
/// UDF registration/invocation, arena ring wrap, and `format_display_dec` modes in `m12_abi.rs`.
#[test]
#[expect(
    unsafe_code,
    reason = "Integration test exercising exported M12 C-ABI functions."
)]
fn m12_abi_all_numeric_intrinsics_udf_and_display_formatting() {
    let _guard = ABI_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_m12_state();
    reset_m13_state();

    unsafe {
        // Empty-stack guard paths on `eval_to_dec`, `eval_bytes`, `format_display_dec`, and pushers.
        assert_eq!(coblang_rt_m12_eval_to_dec(), -1);
        let _ = pop_dec();
        let mut out_p: *const u8 = ptr::null();
        let mut out_l: i64 = 0;
        assert_eq!(
            coblang_rt_m12_eval_bytes(ptr::null_mut(), &raw mut out_l),
            -1
        );
        assert_eq!(
            coblang_rt_m12_eval_bytes(&raw mut out_p, ptr::null_mut()),
            -1
        );
        assert_eq!(
            coblang_rt_m12_eval_bytes(&raw mut out_p, &raw mut out_l),
            -1
        );
        assert_eq!(
            coblang_rt_m12_format_display_dec(0, 1, 0, 0, 0, 0, ptr::null_mut(), &raw mut out_l),
            -1
        );
        assert_eq!(
            coblang_rt_m12_format_display_dec(0, 1, 0, 0, 0, 0, &raw mut out_p, ptr::null_mut()),
            -1
        );
        assert_eq!(coblang_rt_m12_push_bytes_arg(ptr::null(), 4, 0), -1);
        assert_eq!(coblang_rt_m12_push_keyword_arg(ptr::null(), 4), -1);
        assert_eq!(coblang_rt_m12_push_bytes_arg(b"a".as_ptr(), 1, 0), 0);
        assert_eq!(coblang_rt_m12_push_keyword_arg(b"A".as_ptr(), 1), 0);
        assert_eq!(coblang_rt_m12_pop_dec_arg(), 0);
        assert_eq!(coblang_rt_m12_push_omitted_arg(), 0);
        assert_eq!(coblang_rt_m12_push_alg_probe(1, 0, 0), 0);
        assert_eq!(coblang_rt_m12_series_begin(1), 0);
        assert_eq!(coblang_rt_m12_series_end(), 0);
        assert_eq!(
            coblang_rt_m12_set_context(ptr::null(), 2, ptr::null(), 256, ptr::null(), 2),
            0
        );
        coblang_rt_m12_begin(i32::from(IntrinsicId::Abs.to_abi_code()), 0);
        assert_eq!(coblang_rt_m12_series_end(), 0);
        assert_eq!(
            coblang_rt_m12_set_context(ptr::null(), 2, ptr::null(), 0, ptr::null(), 2),
            0
        );
        assert_eq!(
            coblang_rt_m12_set_context(ptr::null(), 2, ptr::null(), 256, ptr::null(), 2),
            0
        );
        assert_eq!(eval_num_ok(), FixedDecimal::ZERO);

        // UDF registration & invocation (null fn, bad name, missing UDF, valid UDF).
        let udf_name = b"my-udf";
        assert_eq!(
            coblang_rt_m12_register_udf(udf_name.as_ptr(), udf_name.len(), ptr::null()),
            -1
        );
        let fn_void = sample_udf_cb as CobUdfEntryFn as *const c_void;
        assert_eq!(coblang_rt_m12_register_udf(ptr::null(), 3, fn_void), -1);
        assert_eq!(
            coblang_rt_m12_register_udf(udf_name.as_ptr(), udf_name.len(), fn_void),
            0
        );
        assert_eq!(
            coblang_rt_m12_invoke_udf(
                ptr::null(),
                3,
                0,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
                0
            ),
            -1
        );
        let missing = b"no-such-udf";
        assert_eq!(
            coblang_rt_m12_invoke_udf(
                missing.as_ptr(),
                missing.len(),
                0,
                ptr::null(),
                ptr::null(),
                ptr::null_mut(),
                0
            ),
            -1
        );
        let mut arg_buf = *b"HELLO";
        let mut ret_buf = [0_u8; 5];
        let argv_p = [arg_buf.as_mut_ptr()];
        let argv_l = [arg_buf.len()];
        assert_eq!(
            coblang_rt_m12_invoke_udf(
                udf_name.as_ptr(),
                udf_name.len(),
                1,
                argv_p.as_ptr(),
                argv_l.as_ptr(),
                ret_buf.as_mut_ptr(),
                ret_buf.len()
            ),
            42
        );
        assert_eq!(&ret_buf, b"HELLO");

        // `CONTENT-LENGTH` (`abi_code = -1`): null pointer, valid C-string pointer, bytes with NUL, keyword fallback.
        coblang_rt_m12_begin(-1, 0);
        stage_bytes(&0_usize.to_ne_bytes(), 3);
        assert_eq!(eval_num_ok(), FixedDecimal::ZERO);

        let cstr = b"abcd\0ef";
        let cstr_addr = cstr.as_ptr().expose_provenance();
        coblang_rt_m12_begin(-1, 0);
        stage_bytes(&cstr_addr.to_ne_bytes(), 3);
        assert_eq!(eval_num_ok(), FixedDecimal::from_i64(4));

        coblang_rt_m12_begin(-1, 0);
        stage_bytes(cstr, 0);
        assert_eq!(eval_num_ok(), FixedDecimal::from_i64(4));

        coblang_rt_m12_begin(-1, 0);
        stage_kw("OTHER");
        assert_eq!(eval_num_ok(), FixedDecimal::ZERO);

        // Invalid `abi_code` fallback (`9999` -> `0`, byte intrinsic evaluated via `eval_to_dec` -> `0`).
        coblang_rt_m12_begin(9999, 0);
        assert_eq!(eval_num_ok(), FixedDecimal::ZERO);
        coblang_rt_m12_begin(i32::from(IntrinsicId::UpperCase.to_abi_code()), 0);
        assert_eq!(eval_num_ok(), FixedDecimal::ZERO);

        // Unary & binary math intrinsics + domain error path (`eval_to_dec` == 1).
        for (id, a0) in [
            (IntrinsicId::Abs, "-5.25"),
            (IntrinsicId::Acos, "0.5"),
            (IntrinsicId::Asin, "0.5"),
            (IntrinsicId::Atan, "1.0"),
            (IntrinsicId::Cos, "0.5"),
            (IntrinsicId::Exp, "1.0"),
            (IntrinsicId::Exp10, "2.0"),
            (IntrinsicId::Factorial, "5"),
            (IntrinsicId::FractionPart, "-3.75"),
            (IntrinsicId::Integer, "-3.2"),
            (IntrinsicId::IntegerPart, "-3.75"),
            (IntrinsicId::Log, "2.5"),
            (IntrinsicId::Log10, "100"),
            (IntrinsicId::Sign, "-9"),
            (IntrinsicId::Sin, "0.5"),
            (IntrinsicId::Sqrt, "9"),
            (IntrinsicId::Tan, "0.5"),
        ] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            stage_dec(a0);
            let _ = eval_num_ok();
        }
        for id in [
            IntrinsicId::E,
            IntrinsicId::Pi,
            IntrinsicId::SecondsPastMidnight,
        ] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            let _ = eval_num_ok();
        }
        for id in [IntrinsicId::Mod, IntrinsicId::Rem, IntrinsicId::Annuity] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            stage_dec("11");
            stage_dec("4");
            let _ = eval_num_ok();
        }
        // Domain error (`SQRT(-1)`) returns status `1` and pushes `0`.
        coblang_rt_m12_begin(i32::from(IntrinsicId::Sqrt.to_abi_code()), 0);
        stage_dec("-1");
        assert_eq!(coblang_rt_m12_eval_to_dec(), 1);
        assert_eq!(pop_dec(), FixedDecimal::ZERO);

        // Algebraic probes: binary widths 1/2/4/8 signed & unsigned, floats scale 1/2, decimal signed & unsigned, missing probe.
        coblang_rt_m12_begin(i32::from(IntrinsicId::HighestAlgebraic.to_abi_code()), 0);
        assert_eq!(eval_num_ok(), FixedDecimal::ZERO);
        for id in [
            IntrinsicId::HighestAlgebraic,
            IntrinsicId::LowestAlgebraic,
            IntrinsicId::SmallestAlgebraic,
        ] {
            for &(digits, scale, signed) in &[
                (0, 11, 1),
                (0, 11, 0),
                (0, 12, 1),
                (0, 12, 0),
                (0, 14, 1),
                (0, 14, 0),
                (0, 18, 1),
                (0, 18, 0),
                (0, 1, 1),
                (0, 2, 1),
                (5, 2, 1),
                (5, 2, 0),
            ] {
                coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
                coblang_rt_m12_push_alg_probe(digits, scale, signed);
                let _ = eval_num_ok();
            }
        }

        // Statistical series intrinsics (`NumericSlice` + `Scalar` + ignored `Omitted`).
        coblang_rt_m12_begin(i32::from(IntrinsicId::PresentValue.to_abi_code()), 0);
        stage_dec("0.05");
        coblang_rt_m12_series_begin(1);
        stage_dec("100");
        stage_dec("200");
        coblang_rt_m12_series_end();
        stage_dec("300");
        coblang_rt_m12_push_omitted_arg();
        let _ = eval_num_ok();

        for id in [
            IntrinsicId::Max,
            IntrinsicId::Min,
            IntrinsicId::OrdMax,
            IntrinsicId::OrdMin,
            IntrinsicId::Sum,
            IntrinsicId::Mean,
            IntrinsicId::Median,
            IntrinsicId::Midrange,
            IntrinsicId::Range,
            IntrinsicId::Variance,
            IntrinsicId::StandardDeviation,
        ] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            stage_dec("10");
            coblang_rt_m12_series_begin(1);
            stage_dec("30");
            stage_dec("20");
            coblang_rt_m12_series_end();
            let _ = eval_num_ok();
        }
        // Alpha `ORD-MAX` and `ORD-MIN` with both scalar and `BytesSlice`.
        for id in [IntrinsicId::OrdMax, IntrinsicId::OrdMin] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            coblang_rt_m12_series_begin(0);
            stage_bytes(b"bb", 0);
            stage_bytes(b"aa", 0);
            coblang_rt_m12_series_end();
            stage_bytes(b"cc", 0);
            coblang_rt_m12_push_omitted_arg();
            let _ = eval_num_ok();
        }

        // `RANDOM` seeded, omitted, and unseeded.
        coblang_rt_m12_begin(i32::from(IntrinsicId::Random.to_abi_code()), 0);
        stage_dec("7");
        let _ = eval_num_ok();
        coblang_rt_m12_begin(i32::from(IntrinsicId::Random.to_abi_code()), 0);
        coblang_rt_m12_push_omitted_arg();
        let _ = eval_num_ok();
        coblang_rt_m12_begin(i32::from(IntrinsicId::Random.to_abi_code()), 0);
        let _ = eval_num_ok();

        // Date/time numeric intrinsics + sliding window variants + invalid execution year.
        for (id, arg) in [
            (IntrinsicId::IntegerOfDate, "20261120"),
            (IntrinsicId::DateOfInteger, "155600"),
            (IntrinsicId::IntegerOfDay, "2026324"),
            (IntrinsicId::DayOfInteger, "155600"),
            (IntrinsicId::TestDateYyyymmdd, "20261120"),
            (IntrinsicId::TestDayYyyyddd, "2026324"),
        ] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            stage_dec(arg);
            let _ = eval_num_ok();
        }
        for (id, a0) in [
            (IntrinsicId::YearToYyyy, "26"),
            (IntrinsicId::DateToYyyymmdd, "261120"),
            (IntrinsicId::DayToYyyyddd, "26324"),
        ] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            stage_dec(a0);
            let _ = eval_num_ok();

            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            stage_dec(a0);
            stage_dec("50");
            stage_dec("2020");
            let _ = eval_num_ok();
        }
        coblang_rt_m12_begin(i32::from(IntrinsicId::YearToYyyy.to_abi_code()), 0);
        stage_dec("26");
        stage_dec("50");
        coblang_rt_m12_push_omitted_arg();
        let _ = eval_num_ok();

        coblang_rt_m12_begin(i32::from(IntrinsicId::YearToYyyy.to_abi_code()), 0);
        stage_dec("26");
        coblang_rt_m12_push_omitted_arg();
        stage_dec("1200");
        assert_eq!(coblang_rt_m12_eval_to_dec(), 1);
        let _ = pop_dec();

        coblang_rt_m12_begin(i32::from(IntrinsicId::YearToYyyy.to_abi_code()), 0);
        stage_dec("26");
        coblang_rt_m12_push_omitted_arg();
        stage_dec("10000");
        assert_eq!(coblang_rt_m12_eval_to_dec(), 1);
        let _ = pop_dec();

        // Out-of-i64 argument error on `decimal_to_i64`.
        coblang_rt_m12_begin(i32::from(IntrinsicId::IntegerOfDate.to_abi_code()), 0);
        stage_dec("999999999999999999999999999999");
        assert_eq!(coblang_rt_m12_eval_to_dec(), 1);
        let _ = pop_dec();

        coblang_rt_m12_begin(i32::from(IntrinsicId::CombinedDatetime.to_abi_code()), 0);
        stage_dec("1");
        stage_dec("43200");
        let _ = eval_num_ok();

        coblang_rt_m12_begin(
            i32::from(IntrinsicId::IntegerOfFormattedDate.to_abi_code()),
            0,
        );
        stage_bytes(b"YYYY-MM-DD", 0);
        stage_bytes(b"2026-11-20", 0);
        let _ = eval_num_ok();

        coblang_rt_m12_begin(
            i32::from(IntrinsicId::SecondsFromFormattedTime.to_abi_code()),
            0,
        );
        stage_bytes(b"hh:mm:ss", 0);
        stage_bytes(b"01:02:03", 0);
        let _ = eval_num_ok();

        coblang_rt_m12_begin(
            i32::from(IntrinsicId::TestFormattedDatetime.to_abi_code()),
            0,
        );
        stage_bytes(b"YYYY-MM-DD", 0);
        stage_bytes(b"2026-11-20", 0);
        assert_eq!(eval_num_ok(), FixedDecimal::ZERO);

        // String/Unicode numeric intrinsics (`ByteLength`, `Length` 5 modes, `StoredCharLength`, `Ord`, `FindString`, `Numval*`, `U*`).
        coblang_rt_m12_begin(i32::from(IntrinsicId::ByteLength.to_abi_code()), 0);
        stage_bytes(b"hello", 0);
        assert_eq!(eval_num_ok(), FixedDecimal::from_i64(5));

        for (id, bytes, cat, kw) in [
            (IntrinsicId::Length, &b"hello"[..], 0, Some("PHYSICAL")),
            (IntrinsicId::Length, &b"hello"[..], 0, Some("LOGICAL")),
            (IntrinsicId::Length, &[0x00, b'A', 0x00, b'B'][..], 1, None),
            (IntrinsicId::Length, "éa".as_bytes(), 2, None),
            (IntrinsicId::Length, &[0xFF, 0xFE][..], 2, None),
            (IntrinsicId::LengthAn, &b"abc"[..], 0, None),
        ] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            stage_bytes(bytes, cat);
            if let Some(k) = kw {
                stage_kw(k);
            }
            let _ = eval_num_ok();
        }
        coblang_rt_m12_begin(i32::from(IntrinsicId::Length.to_abi_code()), 0);
        assert_eq!(eval_num_ok(), FixedDecimal::ZERO);

        coblang_rt_m12_begin(i32::from(IntrinsicId::StoredCharLength.to_abi_code()), 0);
        stage_bytes(b"hi  ", 0);
        assert_eq!(eval_num_ok(), FixedDecimal::from_i64(2));

        let mut custom_ranks = [0_u8; 256];
        for (i, slot) in custom_ranks.iter_mut().enumerate() {
            *slot = u8::try_from(255 - i).unwrap_or(0);
        }
        coblang_rt_m12_begin(i32::from(IntrinsicId::Ord.to_abi_code()), 0);
        coblang_rt_m12_set_context(
            b"".as_ptr(),
            0,
            custom_ranks.as_ptr(),
            256,
            b"L".as_ptr(),
            1,
        );
        stage_bytes(b"A", 0);
        let _ = eval_num_ok();

        coblang_rt_m12_begin(i32::from(IntrinsicId::Ord.to_abi_code()), 0);
        stage_bytes(b"A", 0);
        stage_bytes(&custom_ranks, 0);
        let _ = eval_num_ok();

        coblang_rt_m12_begin(i32::from(IntrinsicId::Ord.to_abi_code()), 0);
        stage_bytes(b"A", 0);
        stage_bytes(b"short", 0);
        let _ = eval_num_ok();

        coblang_rt_m12_begin(i32::from(IntrinsicId::FindString.to_abi_code()), 0);
        stage_bytes(b"AbcAbc", 0);
        stage_bytes(b"abc", 0);
        stage_kw("LAST");
        stage_kw("ANYCASE");
        stage_kw("FIRST");
        stage_dec("1");
        coblang_rt_m12_push_omitted_arg();
        let _ = eval_num_ok();

        coblang_rt_m12_begin(i32::from(IntrinsicId::TestNumval.to_abi_code()), 1);
        stage_bytes(b"12,34", 0);
        assert_eq!(eval_num_ok(), FixedDecimal::ZERO);

        coblang_rt_m12_begin(i32::from(IntrinsicId::Numval.to_abi_code()), 1);
        stage_bytes(b"12,34", 0);
        let _ = eval_num_ok();

        coblang_rt_m12_begin(i32::from(IntrinsicId::TestNumvalC.to_abi_code()), 0);
        stage_bytes(b"eur 12.34", 0);
        stage_bytes(b" EUR ", 0);
        stage_kw("ANYCASE");
        stage_kw("OTHER");
        assert_eq!(eval_num_ok(), FixedDecimal::ZERO);

        coblang_rt_m12_begin(i32::from(IntrinsicId::NumvalC.to_abi_code()), 0);
        stage_bytes(b" 12.34", 0);
        stage_bytes(b"   ", 0);
        coblang_rt_m12_push_omitted_arg();
        stage_dec("0");
        let _ = eval_num_ok();

        coblang_rt_m12_begin(i32::from(IntrinsicId::TestNumvalF.to_abi_code()), 0);
        stage_bytes(b"1.5E+2", 0);
        assert_eq!(eval_num_ok(), FixedDecimal::ZERO);

        coblang_rt_m12_begin(i32::from(IntrinsicId::NumvalF.to_abi_code()), 0);
        stage_bytes(b"1.5E+2", 0);
        let _ = eval_num_ok();

        coblang_rt_m12_begin(i32::from(IntrinsicId::IntegerOfBoolean.to_abi_code()), 0);
        stage_bytes(b"101", 0);
        assert_eq!(eval_num_ok(), FixedDecimal::from_i64(5));

        for id in [
            IntrinsicId::Ulength,
            IntrinsicId::Usupplementary,
            IntrinsicId::Uvalid,
        ] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            stage_bytes("A😀".as_bytes(), 2);
            let _ = eval_num_ok();
        }
        coblang_rt_m12_begin(i32::from(IntrinsicId::Ulength.to_abi_code()), 0);
        stage_bytes(&[0x00, b'A'], 1);
        let _ = eval_num_ok();
        coblang_rt_m12_begin(i32::from(IntrinsicId::Ulength.to_abi_code()), 0);
        assert_eq!(eval_num_ok(), FixedDecimal::ZERO);

        for id in [IntrinsicId::Upos, IntrinsicId::Uwidth] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            stage_bytes("A😀".as_bytes(), 2);
            stage_dec("1");
            let _ = eval_num_ok();

            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            stage_bytes("A".as_bytes(), 2);
            stage_dec("-1");
            assert_eq!(coblang_rt_m12_eval_to_dec(), 1);
            let _ = pop_dec();
        }

        // `coblang_rt_m12_format_display_dec` modes + arena ring wrap (> 260 calls).
        for &(mode, abi, digits, scale, signed, comma, val_s, expected) in &[
            (0, 1, 0, 0, 0, 0, "12.50", &b"12.50"[..]),
            (1, -1, 0, 0, 0, 0, "7", &b"0000000007"[..]),
            (
                1,
                IntrinsicId::ByteLength.to_abi_code() as i32,
                0,
                0,
                0,
                0,
                "12",
                &b"0000000012"[..],
            ),
            (
                1,
                IntrinsicId::ByteLength.to_abi_code() as i32,
                0,
                0,
                0,
                0,
                "999999999999999999999999999999",
                &b"999999999999999999999999999999"[..],
            ),
            (
                1,
                IntrinsicId::Abs.to_abi_code() as i32,
                0,
                0,
                0,
                0,
                "25",
                &b"25"[..],
            ),
            (
                1,
                IntrinsicId::Abs.to_abi_code() as i32,
                4,
                -1,
                0,
                0,
                "25",
                &b"25"[..],
            ),
            (
                1,
                IntrinsicId::Abs.to_abi_code() as i32,
                4,
                0,
                0,
                0,
                "25",
                &b"0025"[..],
            ),
            (
                1,
                IntrinsicId::ByteLength.to_abi_code() as i32,
                0,
                0,
                0,
                0,
                "-1",
                &b"-1"[..],
            ),
            (
                1,
                IntrinsicId::Abs.to_abi_code() as i32,
                4,
                0,
                1,
                0,
                "25",
                &b"+0025"[..],
            ),
            (
                1,
                IntrinsicId::Abs.to_abi_code() as i32,
                4,
                2,
                0,
                1,
                "-3.5",
                &b"-03,50"[..],
            ),
            (
                1,
                IntrinsicId::Mean.to_abi_code() as i32,
                0,
                0,
                1,
                0,
                "-4.200",
                &b"-00000004.2"[..],
            ),
            (
                1,
                IntrinsicId::Midrange.to_abi_code() as i32,
                0,
                0,
                1,
                0,
                "-4.200",
                &b"-000000004.2"[..],
            ),
            (
                1,
                IntrinsicId::PresentValue.to_abi_code() as i32,
                0,
                0,
                0,
                0,
                "12.00",
                &b"00012"[..],
            ),
            (
                1,
                IntrinsicId::Sin.to_abi_code() as i32,
                0,
                0,
                1,
                0,
                "5.000",
                &b"5"[..],
            ),
            (
                1,
                IntrinsicId::Sin.to_abi_code() as i32,
                0,
                0,
                1,
                0,
                "9",
                &b"9"[..],
            ),
        ] {
            let d = FixedDecimal::parse_str(val_s).unwrap_or(FixedDecimal::ZERO);
            with_global_runtime(|rt| rt.decimal_stack_mut().push_decimal(d));
            assert_eq!(
                coblang_rt_m12_format_display_dec(
                    mode,
                    abi,
                    digits,
                    scale,
                    signed,
                    comma,
                    &raw mut out_p,
                    &raw mut out_l
                ),
                0
            );
            let got = std::slice::from_raw_parts(out_p, usize::try_from(out_l).unwrap_or(0));
            assert_eq!(got, expected);
        }
        // Trigger `ARENA_RING_SLOTS` (256) wraparound.
        for _ in 0..265 {
            with_global_runtime(|rt| {
                rt.decimal_stack_mut()
                    .push_decimal(FixedDecimal::from_i64(1))
            });
            assert_eq!(
                coblang_rt_m12_format_display_dec(0, 1, 0, 0, 0, 0, &raw mut out_p, &raw mut out_l),
                0
            );
        }
    }
}

/// Exercises all alphanumeric/national/datetime/reflection intrinsics and reference modification in `m12_abi.rs`.
#[test]
#[expect(
    unsafe_code,
    reason = "Integration test exercising exported M12 byte/refmod C-ABI functions."
)]
fn m12_abi_all_bytes_intrinsics_and_refmod_branches() {
    let _guard = ABI_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_m12_state();
    reset_m13_state();

    unsafe {
        // Extension codes `-2..-5` under Period and Comma, plus invalid code `9999` and numeric-id fallback.
        for &(code, comma, exp) in &[
            (-2, 0, b'.'),
            (-4, 1, b','),
            (-3, 1, b'.'),
            (-5, 0, b','),
            (9999, 0, b' '),
            (IntrinsicId::Abs.to_abi_code() as i32, 0, b' '),
        ] {
            coblang_rt_m12_begin(code, comma);
            assert_eq!(eval_bytes_vec(0), vec![exp]);
        }

        // `CONTENT-OF`: null, omitted, neg len, zero len, raw ptr (NUL + explicit len), bytes (NUL + explicit len), keyword.
        coblang_rt_m12_begin(i32::from(IntrinsicId::ContentOf.to_abi_code()), 0);
        stage_bytes(&0_usize.to_ne_bytes(), 3);
        assert!(eval_bytes_vec(1).is_empty());

        coblang_rt_m12_begin(i32::from(IntrinsicId::ContentOf.to_abi_code()), 0);
        stage_kw("BAD");
        assert!(eval_bytes_vec(1).is_empty());

        let cbuf = b"hello\0world";
        let caddr = cbuf.as_ptr().expose_provenance();
        coblang_rt_m12_begin(i32::from(IntrinsicId::ContentOf.to_abi_code()), 0);
        stage_bytes(&caddr.to_ne_bytes(), 3);
        assert_eq!(eval_bytes_vec(0), b"hello");

        coblang_rt_m12_begin(i32::from(IntrinsicId::ContentOf.to_abi_code()), 0);
        stage_bytes(&caddr.to_ne_bytes(), 3);
        stage_dec("3");
        assert_eq!(eval_bytes_vec(0), b"hel");

        coblang_rt_m12_begin(i32::from(IntrinsicId::ContentOf.to_abi_code()), 0);
        stage_bytes(&caddr.to_ne_bytes(), 3);
        stage_dec("0");
        assert!(eval_bytes_vec(1).is_empty());

        coblang_rt_m12_begin(i32::from(IntrinsicId::ContentOf.to_abi_code()), 0);
        stage_bytes(&caddr.to_ne_bytes(), 3);
        stage_dec("-2");
        assert!(eval_bytes_vec(1).is_empty());

        coblang_rt_m12_begin(i32::from(IntrinsicId::ContentOf.to_abi_code()), 0);
        stage_bytes(cbuf, 0);
        coblang_rt_m12_push_omitted_arg();
        assert_eq!(eval_bytes_vec(0), b"hello");

        coblang_rt_m12_begin(i32::from(IntrinsicId::ContentOf.to_abi_code()), 0);
        stage_bytes(b"abcdef", 0);
        stage_dec("4");
        assert_eq!(eval_bytes_vec(0), b"abcd");

        // `CHAR`, `CHAR-NATIONAL`, `DISPLAY-OF`, `NATIONAL-OF`.
        let ranks = [0_u8; 256];
        coblang_rt_m12_begin(i32::from(IntrinsicId::Char.to_abi_code()), 0);
        coblang_rt_m12_set_context(b"$".as_ptr(), 1, ranks.as_ptr(), 256, b"L".as_ptr(), 1);
        stage_dec("66");
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::Char.to_abi_code()), 0);
        stage_dec("66");
        stage_bytes(&ranks, 0);
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::Char.to_abi_code()), 0);
        stage_dec("66");
        stage_bytes(b"x", 0);
        assert_eq!(eval_bytes_vec(0), b"A");

        coblang_rt_m12_begin(i32::from(IntrinsicId::CharNational.to_abi_code()), 0);
        stage_dec("66");
        assert_eq!(eval_bytes_vec(0), vec![0x00, b'A']);

        coblang_rt_m12_begin(i32::from(IntrinsicId::DisplayOf.to_abi_code()), 0);
        stage_bytes(&[0x00, b'H', 0x00, b'I'], 1);
        assert_eq!(eval_bytes_vec(0), b"HI");

        coblang_rt_m12_begin(i32::from(IntrinsicId::DisplayOf.to_abi_code()), 0);
        stage_bytes(&[0x26, 0x03], 1);
        stage_bytes(b"!", 0);
        assert_eq!(eval_bytes_vec(0), b"!");

        coblang_rt_m12_begin(i32::from(IntrinsicId::DisplayOf.to_abi_code()), 0);
        stage_bytes(&[0x00, b'X'], 1);
        stage_bytes(b"", 0);
        assert_eq!(eval_bytes_vec(0), b"X");

        coblang_rt_m12_begin(i32::from(IntrinsicId::DisplayOf.to_abi_code()), 0);
        stage_bytes(&[0x00, b'X'], 1);
        coblang_rt_m12_push_omitted_arg();
        assert_eq!(eval_bytes_vec(0), b"X");

        coblang_rt_m12_begin(i32::from(IntrinsicId::NationalOf.to_abi_code()), 0);
        stage_bytes(b"HI", 0);
        assert_eq!(eval_bytes_vec(0), vec![0x00, b'H', 0x00, b'I']);

        coblang_rt_m12_begin(i32::from(IntrinsicId::NationalOf.to_abi_code()), 0);
        stage_bytes(b"A", 0);
        stage_bytes(b"?", 0);
        assert_eq!(eval_bytes_vec(0), vec![0x00, b'A']);

        coblang_rt_m12_begin(i32::from(IntrinsicId::NationalOf.to_abi_code()), 0);
        stage_bytes(b"A", 0);
        stage_bytes(&[0x00, b'?'], 1);
        assert_eq!(eval_bytes_vec(0), vec![0x00, b'A']);

        coblang_rt_m12_begin(i32::from(IntrinsicId::NationalOf.to_abi_code()), 0);
        stage_bytes(b"A", 0);
        coblang_rt_m12_push_omitted_arg();
        assert_eq!(eval_bytes_vec(0), vec![0x00, b'A']);

        // `CONCATENATE`, `REVERSE` (ASCII + National), `UPPER-CASE`, `LOWER-CASE`, `TRIM` (ASCII + National).
        coblang_rt_m12_begin(i32::from(IntrinsicId::Concatenate.to_abi_code()), 0);
        stage_bytes(b"ab", 0);
        coblang_rt_m12_push_omitted_arg();
        stage_bytes(b"cd", 0);
        assert_eq!(eval_bytes_vec(0), b"abcd");

        coblang_rt_m12_begin(i32::from(IntrinsicId::Reverse.to_abi_code()), 0);
        stage_bytes(b"abc", 0);
        assert_eq!(eval_bytes_vec(0), b"cba");

        coblang_rt_m12_begin(i32::from(IntrinsicId::Reverse.to_abi_code()), 0);
        stage_bytes(&[0x00, b'A', 0x00, b'B'], 1);
        assert_eq!(eval_bytes_vec(0), vec![0x00, b'B', 0x00, b'A']);

        coblang_rt_m12_begin(i32::from(IntrinsicId::UpperCase.to_abi_code()), 0);
        stage_bytes(b"aBc", 0);
        assert_eq!(eval_bytes_vec(0), b"ABC");

        coblang_rt_m12_begin(i32::from(IntrinsicId::LowerCase.to_abi_code()), 0);
        assert!(eval_bytes_vec(0).is_empty());

        coblang_rt_m12_begin(i32::from(IntrinsicId::FormattedDate.to_abi_code()), 0);
        stage_bytes(b"BAD-FMT", 0);
        stage_dec("999999999999999999999999999999");
        let _ = eval_bytes_vec(1);

        coblang_rt_m12_begin(i32::from(IntrinsicId::FormattedDate.to_abi_code()), 0);
        stage_bytes(b"BAD-FMT", 0);
        stage_dec("1");
        let _ = eval_bytes_vec(1);

        coblang_rt_m12_begin(i32::from(IntrinsicId::FormattedDatetime.to_abi_code()), 0);
        stage_bytes(b"YYYY-MM-DDThh:mm:ssZ", 0);
        stage_dec("1");
        stage_dec("3600");
        stage_dec("0");
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::LowerCase.to_abi_code()), 0);
        stage_bytes(b"AbC", 0);
        assert_eq!(eval_bytes_vec(0), b"abc");

        for kw in ["LEADING", "TRAILING", "BOTH"] {
            coblang_rt_m12_begin(i32::from(IntrinsicId::Trim.to_abi_code()), 0);
            stage_bytes(b"**hi**", 0);
            stage_kw(kw);
            stage_bytes(b"*", 0);
            let _ = eval_bytes_vec(0);

            coblang_rt_m12_begin(i32::from(IntrinsicId::Trim.to_abi_code()), 0);
            stage_bytes(&[0x00, b'*', 0x00, b'A', 0x00, b'*'], 1);
            stage_kw(kw);
            stage_bytes(&[0x00, b'*'], 1);
            let _ = eval_bytes_vec(0);
        }
        coblang_rt_m12_begin(i32::from(IntrinsicId::Trim.to_abi_code()), 0);
        stage_bytes(&[0x00, b' ', 0x00, b'A'], 1);
        stage_bytes(b"?", 1);
        assert_eq!(eval_bytes_vec(0), vec![0x00, b'A']);

        coblang_rt_m12_begin(i32::from(IntrinsicId::Trim.to_abi_code()), 0);
        stage_bytes(b" hi ", 0);
        stage_bytes(b"", 0);
        assert_eq!(eval_bytes_vec(0), b"hi");

        coblang_rt_m12_begin(i32::from(IntrinsicId::Trim.to_abi_code()), 0);
        stage_bytes(b" hi ", 0);
        coblang_rt_m12_push_omitted_arg();
        assert_eq!(eval_bytes_vec(0), b"hi");

        coblang_rt_m12_begin(i32::from(IntrinsicId::Trim.to_abi_code()), 0);
        stage_bytes(b"hi", 0);
        stage_bytes(b"xy", 0);
        assert!(eval_bytes_vec(1).is_empty());

        // `SUBSTITUTE`, `SUBSTITUTE-CASE`, `BASECONVERT`, `BOOLEAN-OF-INTEGER`, `BIT-*`, `HEX-*`, `CONVERT`.
        for id in [IntrinsicId::Substitute, IntrinsicId::SubstituteCase] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            stage_bytes(b"Hello World", 0);
            stage_bytes(b"world", 0);
            stage_bytes(b"COBOL", 0);
            let _ = eval_bytes_vec(0);
        }

        coblang_rt_m12_begin(i32::from(IntrinsicId::BaseConvert.to_abi_code()), 0);
        stage_bytes(b"255", 0);
        stage_dec("10");
        stage_dec("16");
        assert_eq!(eval_bytes_vec(0), b"FF");

        coblang_rt_m12_begin(i32::from(IntrinsicId::BaseConvert.to_abi_code()), 0);
        stage_bytes(b"255", 0);
        stage_dec("99");
        stage_dec("16");
        let _ = eval_bytes_vec(1);

        coblang_rt_m12_begin(i32::from(IntrinsicId::BooleanOfInteger.to_abi_code()), 0);
        stage_dec("5");
        stage_dec("4");
        assert_eq!(eval_bytes_vec(0), b"0101");

        coblang_rt_m12_begin(i32::from(IntrinsicId::BitOf.to_abi_code()), 0);
        stage_bytes(b"A", 0);
        assert_eq!(eval_bytes_vec(0), b"01000001");

        coblang_rt_m12_begin(i32::from(IntrinsicId::BitToChar.to_abi_code()), 0);
        stage_bytes(b"01000001", 0);
        assert_eq!(eval_bytes_vec(0), b"A");

        coblang_rt_m12_begin(i32::from(IntrinsicId::HexOf.to_abi_code()), 0);
        stage_bytes(b"AB", 0);
        assert_eq!(eval_bytes_vec(0), b"4142");

        coblang_rt_m12_begin(i32::from(IntrinsicId::HexToChar.to_abi_code()), 0);
        stage_bytes(b"4142", 0);
        assert_eq!(eval_bytes_vec(0), b"AB");

        coblang_rt_m12_begin(i32::from(IntrinsicId::Convert.to_abi_code()), 0);
        stage_bytes(b"AB", 0);
        stage_kw("ALPHANUMERIC");
        stage_kw("HEX");
        assert_eq!(eval_bytes_vec(0), b"4142");

        coblang_rt_m12_begin(i32::from(IntrinsicId::Convert.to_abi_code()), 0);
        stage_bytes(b"01000001", 0);
        stage_kw("BIT");
        stage_kw("ALPHANUMERIC");
        assert_eq!(eval_bytes_vec(0), b"A");

        coblang_rt_m12_begin(i32::from(IntrinsicId::Convert.to_abi_code()), 0);
        stage_bytes(b"abc", 0);
        stage_bytes(b"abc", 0);
        stage_bytes(b"XYZ", 0);
        assert_eq!(eval_bytes_vec(0), b"XYZ");

        // `STANDARD-COMPARE`, `LOCALE-COMPARE`, alpha `MAX`/`MIN`.
        coblang_rt_m12_begin(i32::from(IntrinsicId::StandardCompare.to_abi_code()), 0);
        stage_bytes(b"a", 0);
        stage_bytes(b"b", 0);
        assert_eq!(eval_bytes_vec(0), b"<");

        coblang_rt_m12_begin(i32::from(IntrinsicId::StandardCompare.to_abi_code()), 0);
        stage_bytes(b"a", 0);
        stage_bytes(b"a", 0);
        coblang_rt_m12_push_omitted_arg();
        stage_dec("1");
        assert_eq!(eval_bytes_vec(0), b"=");

        coblang_rt_m12_begin(i32::from(IntrinsicId::LocaleCompare.to_abi_code()), 0);
        stage_bytes(b"a", 0);
        stage_bytes(b"b", 0);
        assert_eq!(eval_bytes_vec(0), b"<");

        coblang_rt_m12_begin(i32::from(IntrinsicId::LocaleCompare.to_abi_code()), 0);
        stage_bytes(b"a", 0);
        stage_bytes(b"a", 0);
        stage_bytes(b"C", 0);
        assert_eq!(eval_bytes_vec(0), b"=");

        for id in [IntrinsicId::Max, IntrinsicId::Min] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            stage_bytes(b"x", 0);
            stage_bytes(b"z", 0);
            let _ = eval_bytes_vec(0);
        }

        // Date/time formatting & locale intrinsics + `USUBSTR` + `CURRENCY-SYMBOL` + `UUID4`.
        for id in [
            IntrinsicId::CurrentDate,
            IntrinsicId::WhenCompiled,
            IntrinsicId::Uuid4,
        ] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            let _ = eval_bytes_vec(0);
        }
        coblang_rt_m12_begin(
            i32::from(IntrinsicId::FormattedCurrentDate.to_abi_code()),
            0,
        );
        stage_bytes(b"YYYY-MM-DDThh:mm:ss+hh:mm", 0);
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::FormattedDate.to_abi_code()), 0);
        stage_bytes(b"YYYY-MM-DD", 0);
        stage_dec("155600");
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::FormattedTime.to_abi_code()), 0);
        stage_bytes(b"hh:mm:ssZ", 0);
        stage_dec("3600");
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::FormattedTime.to_abi_code()), 0);
        stage_bytes(b"hh:mm:ss+hhmm", 0);
        stage_dec("3600");
        stage_kw("OTHER");
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::FormattedTime.to_abi_code()), 0);
        stage_bytes(b"hh:mm:ss+hh:mm", 0);
        stage_dec("3600");
        stage_kw("SYSTEM-OFFSET");
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::FormattedTime.to_abi_code()), 0);
        stage_bytes(b"hh:mm:ssZ", 0);
        stage_dec("3600");
        stage_dec("0");
        coblang_rt_m12_push_omitted_arg();
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::FormattedDatetime.to_abi_code()), 0);
        stage_bytes(b"YYYY-MM-DDThh:mm:ss+hhmm", 0);
        stage_dec("1");
        stage_dec("3600");
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::FormattedDatetime.to_abi_code()), 0);
        stage_bytes(b"YYYY-MM-DDThh:mm:ss+hh:mm", 0);
        stage_dec("1");
        stage_dec("3600");
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::FormattedDatetime.to_abi_code()), 0);
        stage_bytes(b"YYYY-MM-DDThh:mm:ssZ", 0);
        stage_dec("1");
        stage_dec("3600");
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::FormattedDatetime.to_abi_code()), 0);
        stage_bytes(b"YYYY-MM-DDThh:mm:ss+hh:mm", 0);
        stage_dec("1");
        stage_dec("3600");
        stage_dec("999999999999999999999");
        let _ = eval_bytes_vec(1);

        coblang_rt_m12_begin(i32::from(IntrinsicId::LocaleDate.to_abi_code()), 0);
        stage_bytes(b"20261120", 0);
        let _ = eval_bytes_vec(0);
        coblang_rt_m12_begin(i32::from(IntrinsicId::LocaleDate.to_abi_code()), 0);
        stage_bytes(b"20261120", 0);
        stage_bytes(b"C", 0);
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::LocaleTime.to_abi_code()), 0);
        stage_bytes(b"123045", 0);
        let _ = eval_bytes_vec(0);
        coblang_rt_m12_begin(i32::from(IntrinsicId::LocaleTime.to_abi_code()), 0);
        stage_bytes(b"123045", 0);
        stage_bytes(b"C", 0);
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(
            i32::from(IntrinsicId::LocaleTimeFromSeconds.to_abi_code()),
            0,
        );
        stage_dec("3661");
        let _ = eval_bytes_vec(0);
        coblang_rt_m12_begin(
            i32::from(IntrinsicId::LocaleTimeFromSeconds.to_abi_code()),
            0,
        );
        stage_dec("3661");
        stage_bytes(b"C", 0);
        let _ = eval_bytes_vec(0);

        coblang_rt_m12_begin(i32::from(IntrinsicId::Usubstr.to_abi_code()), 0);
        stage_bytes("A😀B".as_bytes(), 2);
        stage_dec("2");
        stage_dec("1");
        assert_eq!(eval_bytes_vec(0), "😀".as_bytes());

        coblang_rt_m12_begin(i32::from(IntrinsicId::CurrencySymbol.to_abi_code()), 0);
        assert_eq!(eval_bytes_vec(0), b"$");

        // `MODULE-*` (with 0, 1, and 2 active modules) & `EXCEPTION-*` (cleared and active).
        for id in [
            IntrinsicId::ModuleCallerId,
            IntrinsicId::ModuleDate,
            IntrinsicId::ModuleFormattedDate,
            IntrinsicId::ModuleId,
            IntrinsicId::ModulePath,
            IntrinsicId::ModuleSource,
            IntrinsicId::ModuleTime,
        ] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            let _ = eval_bytes_vec(0);
        }
        coblang_rt_m12_enter_module(b"OUTER".as_ptr(), 5, b"outer.cob".as_ptr(), 9);
        coblang_rt_m12_enter_module(b"INNER".as_ptr(), 5, b"inner.cob".as_ptr(), 9);
        coblang_rt_m12_begin(i32::from(IntrinsicId::ModuleCallerId.to_abi_code()), 0);
        assert_eq!(eval_bytes_vec(0), b"OUTER");
        coblang_rt_m12_begin(i32::from(IntrinsicId::ModuleId.to_abi_code()), 0);
        assert_eq!(eval_bytes_vec(0), b"INNER");
        coblang_rt_m12_leave_module();
        coblang_rt_m12_leave_module();

        coblang_rt_m13_last_exception_off();
        coblang_rt_m12_begin(i32::from(IntrinsicId::ExceptionStatus.to_abi_code()), 0);
        let _ = eval_bytes_vec(0);

        note_io_exception(*b"23", "IDX-F");
        note_runtime_exception(ExceptionCode::EcUserException, "RAISE", "M; P; 1", false);
        for id in [
            IntrinsicId::ExceptionFile,
            IntrinsicId::ExceptionFileN,
            IntrinsicId::ExceptionLocation,
            IntrinsicId::ExceptionLocationN,
            IntrinsicId::ExceptionStatement,
            IntrinsicId::ExceptionStatus,
        ] {
            coblang_rt_m12_begin(i32::from(id.to_abi_code()), 0);
            let _ = eval_bytes_vec(0);
        }

        // `coblang_rt_m12_apply_refmod`: null out, national omitted len, non-fatal & fatal OOB, empty input, bad input ptr.
        let mut rm_p: *const u8 = ptr::null();
        let mut rm_l: i64 = 0;
        let loc = b"M;P;1";
        assert_eq!(
            coblang_rt_m12_apply_refmod(
                b"abc".as_ptr(),
                3,
                0,
                1,
                1,
                1,
                0,
                0,
                loc.as_ptr(),
                loc.len(),
                ptr::null_mut(),
                &raw mut rm_l
            ),
            -1
        );
        assert_eq!(
            coblang_rt_m12_apply_refmod(
                b"abc".as_ptr(),
                3,
                0,
                1,
                1,
                1,
                0,
                0,
                loc.as_ptr(),
                loc.len(),
                &raw mut rm_p,
                ptr::null_mut()
            ),
            -1
        );
        let nat = [0x00, b'A', 0x00, b'B', 0x00, b'C'];
        assert_eq!(
            coblang_rt_m12_apply_refmod(
                b"abc".as_ptr(),
                3,
                0,
                1,
                0,
                1,
                0,
                0,
                loc.as_ptr(),
                loc.len(),
                &raw mut rm_p,
                &raw mut rm_l
            ),
            0
        );
        assert_eq!(rm_l, 0);
        assert_eq!(
            coblang_rt_m12_apply_refmod(
                nat.as_ptr(),
                6,
                1,
                2,
                -1,
                0,
                1,
                0,
                loc.as_ptr(),
                loc.len(),
                &raw mut rm_p,
                &raw mut rm_l
            ),
            0
        );
        assert_eq!(
            std::slice::from_raw_parts(rm_p, usize::try_from(rm_l).unwrap_or(0)),
            &[0x00, b'B', 0x00, b'C']
        );
        // RecordedNonFatal (`off = 0, is_fatal = 0`).
        assert_eq!(
            coblang_rt_m12_apply_refmod(
                b"abc".as_ptr(),
                3,
                0,
                0,
                1,
                1,
                1,
                0,
                ptr::null(),
                2,
                &raw mut rm_p,
                &raw mut rm_l
            ),
            0
        );
        // FatalAbortExitOne (`off = 0, is_fatal = 1`) -> returns `2`.
        assert_eq!(
            coblang_rt_m12_apply_refmod(
                b"abc".as_ptr(),
                3,
                0,
                0,
                1,
                1,
                1,
                1,
                loc.as_ptr(),
                loc.len(),
                &raw mut rm_p,
                &raw mut rm_l
            ),
            2
        );
        assert_eq!(
            coblang_rt_m13_set_turn(i32::from(ExceptionCode::EcBoundRefMod.to_abi_id()), 1),
            0
        );
        assert_eq!(
            coblang_rt_m12_apply_refmod(
                b"abc".as_ptr(),
                3,
                0,
                1,
                1,
                1,
                0,
                0,
                loc.as_ptr(),
                loc.len(),
                &raw mut rm_p,
                &raw mut rm_l
            ),
            0
        );
        // Empty `in_len == 0` and null `in_ptr` with `in_len > 0`.
        assert_eq!(
            coblang_rt_m12_apply_refmod(
                b"".as_ptr(),
                0,
                0,
                1,
                0,
                1,
                0,
                0,
                loc.as_ptr(),
                loc.len(),
                &raw mut rm_p,
                &raw mut rm_l
            ),
            0
        );
        assert_eq!(
            coblang_rt_m12_apply_refmod(
                ptr::null(),
                4,
                0,
                1,
                2,
                1,
                0,
                0,
                loc.as_ptr(),
                loc.len(),
                &raw mut rm_p,
                &raw mut rm_l
            ),
            0
        );
    }
}

/// Exercises every function and branch in `coblang_rt::m13_abi` (`>>TURN`, `RAISE`, `RESUME`,
/// specificity matching, fatal unhandled checks, NIST `DB` modes `0..=5`, and recursive frames).
#[test]
#[expect(
    unsafe_code,
    reason = "Integration test exercising exported M13 C-ABI functions."
)]
fn m13_abi_complete_exception_debug_and_recursive_frame_coverage() {
    let _guard = ABI_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_m13_state();

    unsafe {
        // `set_stmt_loc` valid and invalid slice fallbacks + `note_runtime_exception` location/verb resolution.
        assert_eq!(
            coblang_rt_m13_set_stmt_loc(ptr::null(), 2, ptr::null(), 2),
            0
        );
        coblang_rt_m13_last_exception_off();
        note_runtime_exception(ExceptionCode::EcSizeZeroDivide, "", "", false);
        note_runtime_exception(ExceptionCode::EcSizeZeroDivide, "", "", false);

        let loc = b"P1; S1; 10";
        let verb = b"DIVIDE";
        assert_eq!(
            coblang_rt_m13_set_stmt_loc(loc.as_ptr(), loc.len(), verb.as_ptr(), verb.len()),
            0
        );
        assert_eq!(coblang_rt_m13_set_turn(0, 2), 0);
        assert_eq!(coblang_rt_m13_set_turn(99999, 1), 0);
        let zd_id = i32::from(ExceptionCode::EcSizeZeroDivide.to_abi_id());
        assert_eq!(coblang_rt_m13_set_turn(zd_id, 2), 0);
        note_runtime_exception(ExceptionCode::EcSizeZeroDivide, "", "", true);
        // Second enabled exception while one is already pending keeps first pending code.
        note_runtime_exception(ExceptionCode::EcUserException, "MOVE", "P1; S1; 11", true);
        assert_eq!(
            coblang_rt_m13_take_pending_ec(loc.as_ptr(), loc.len(), verb.as_ptr(), verb.len()),
            zd_id
        );
        // Empty pending queue returns `0`.
        assert_eq!(
            coblang_rt_m13_take_pending_ec(ptr::null(), 2, ptr::null(), 2),
            0
        );

        // `note_io_exception`: `"00"` ignored, `"35"` when disabled does not queue pending, `"35"` when `EC-ALL` enabled sets pending.
        note_io_exception(*b"00", "OK-FILE");
        note_io_exception(*b"35", "DISABLED-FILE");
        assert_eq!(coblang_rt_m13_set_turn(0, 1), 0);
        note_io_exception(*b"35", "MISS-FILE");
        let io_ec = coblang_rt_m13_take_pending_ec(ptr::null(), 2, ptr::null(), 2);
        assert!(io_ec > 0);
        assert_eq!(coblang_rt_m13_clear_last_exception(), 0);

        // `coblang_rt_m13_raise`: invalid slices, bare `RAISE` with no prior exception, explicit `RAISE`, bare re-raise.
        assert_eq!(
            coblang_rt_m13_raise(zd_id, ptr::null(), 2, verb.as_ptr(), verb.len()),
            -1
        );
        assert_eq!(
            coblang_rt_m13_raise(zd_id, loc.as_ptr(), loc.len(), ptr::null(), 2),
            -1
        );
        assert_eq!(
            coblang_rt_m13_raise(0, loc.as_ptr(), loc.len(), b"RAISE".as_ptr(), 5),
            0
        );
        let user_ec = i32::from(ExceptionCode::EcUserException.to_abi_id());
        assert_eq!(
            coblang_rt_m13_raise(user_ec, b"".as_ptr(), 0, b"".as_ptr(), 0),
            0
        );
        assert_eq!(
            coblang_rt_m13_take_pending_ec(loc.as_ptr(), loc.len(), b"RAISE".as_ptr(), 5),
            user_ec
        );
        assert_eq!(
            coblang_rt_m13_raise(0, loc.as_ptr(), loc.len(), b"RAISE".as_ptr(), 5),
            0
        );
        // Overwrite `last_record` with a different code before `take_pending_ec` to cover the `_ =>` arm.
        note_runtime_exception(ExceptionCode::EcSizeOverflow, "ADD", "P;S;1", false);
        assert_eq!(
            coblang_rt_m13_take_pending_ec(loc.as_ptr(), loc.len(), b"RAISE".as_ptr(), 5),
            user_ec
        );

        // `coblang_rt_m13_match_ec`: invalid raised, `EC-ALL` (`1`), category (`2`), exact leaf (`3`), mismatch (`0`), invalid rule (`0`).
        let cat_size = i32::from(ExceptionCode::EcSize.to_abi_id());
        assert_eq!(coblang_rt_m13_match_ec(-5, 0), 0);
        assert_eq!(coblang_rt_m13_match_ec(zd_id, 0), 1);
        assert_eq!(coblang_rt_m13_match_ec(zd_id, cat_size), 2);
        assert_eq!(coblang_rt_m13_match_ec(zd_id, zd_id), 3);
        assert_eq!(coblang_rt_m13_match_ec(zd_id, user_ec), 0);
        assert_eq!(coblang_rt_m13_match_ec(zd_id, 99999), 0);

        // Declarative guard + `set_resume` + `finish_decl` (`NextStatement`, `ProcedureId`, `Unset` non-fatal, `Unset` fatal).
        assert_eq!(coblang_rt_m13_set_resume(0, 0), -1);
        assert_eq!(coblang_rt_m13_enter_decl(), 1);
        assert_eq!(coblang_rt_m13_enter_decl(), 0);
        assert_eq!(coblang_rt_m13_set_resume(1, 4), 0);
        assert_eq!(coblang_rt_m13_finish_decl(user_ec), 5);

        assert_eq!(coblang_rt_m13_enter_decl(), 1);
        assert_eq!(coblang_rt_m13_set_resume(1, -1), 0);
        assert_eq!(coblang_rt_m13_set_resume(0, 0), 0);
        assert_eq!(coblang_rt_m13_finish_decl(user_ec), 0);

        assert_eq!(coblang_rt_m13_enter_decl(), 1);
        assert_eq!(coblang_rt_m13_finish_decl(user_ec), 0);

        assert_eq!(coblang_rt_m13_enter_decl(), 1);
        assert_eq!(coblang_rt_m13_finish_decl(zd_id), -1);

        // `check_fatal_unhandled` & `unhandled_ec`: invalid id, non-fatal (`0`), forced fatal (`1`), natural fatal (`1`) with and without record.
        assert_eq!(coblang_rt_m13_check_fatal_unhandled(-1, 0), 0);
        assert_eq!(coblang_rt_m13_unhandled_ec(user_ec), 0);
        assert_eq!(coblang_rt_m13_check_fatal_unhandled(user_ec, 1), 1);
        note_runtime_exception(ExceptionCode::EcSizeZeroDivide, "DIVIDE", "P;S;9", false);
        assert_eq!(coblang_rt_m13_unhandled_ec(zd_id), 1);
        coblang_rt_m13_last_exception_off();
        assert_eq!(coblang_rt_m13_unhandled_ec(zd_id), 1);

        // NIST `DB` `coblang_rt_m13_debug_prepare`: unset `COB_SET_DEBUG`, `COB_SET_DEBUG=N`, `COB_SET_DEBUG=Y`,
        // in-declarative guard, bad slice args, modes `0, 1, 2, 3, 4, 5`, `"PERFORM LOOP"`, `line_num = 0`, 3 subscripts.
        let mut dbg = [b' '; 132];
        let nm = b"P-SEC";
        let ft = b"FALL THROUGH";
        assert_eq!(
            coblang_rt_m13_debug_prepare(
                dbg.as_mut_ptr(),
                dbg.len(),
                10,
                nm.as_ptr(),
                nm.len(),
                0,
                0,
                0,
                0,
                ft.as_ptr(),
                ft.len()
            ),
            1
        );
        assert_eq!(coblang_rt_m13_finish_decl(0), 0);
        coblang_rt::coblang_rt_disp_push_raw(b"COB_SET_DEBUG".as_ptr(), 13);
        coblang_rt::m10_abi::coblang_rt_disp_flush_env(0);
        coblang_rt::coblang_rt_disp_push_raw(b"N".as_ptr(), 1);
        coblang_rt::m10_abi::coblang_rt_disp_flush_env(1);
        assert_eq!(
            coblang_rt_m13_debug_prepare(
                dbg.as_mut_ptr(),
                dbg.len(),
                10,
                nm.as_ptr(),
                nm.len(),
                0,
                0,
                0,
                0,
                ft.as_ptr(),
                ft.len()
            ),
            0
        );
        coblang_rt::coblang_rt_disp_push_raw(b"COB_SET_DEBUG".as_ptr(), 13);
        coblang_rt::m10_abi::coblang_rt_disp_flush_env(0);
        coblang_rt::coblang_rt_disp_push_raw(b"Y".as_ptr(), 1);
        coblang_rt::m10_abi::coblang_rt_disp_flush_env(1);
        // Bad cont_ptr, bad name_ptr, bad dbg_ptr.
        assert_eq!(
            coblang_rt_m13_debug_prepare(
                dbg.as_mut_ptr(),
                dbg.len(),
                10,
                nm.as_ptr(),
                nm.len(),
                0,
                0,
                0,
                0,
                ptr::null(),
                2
            ),
            0
        );
        assert_eq!(
            coblang_rt_m13_debug_prepare(
                dbg.as_mut_ptr(),
                dbg.len(),
                0,
                ptr::null(),
                2,
                0,
                0,
                0,
                0,
                b"X".as_ptr(),
                1
            ),
            0
        );
        assert_eq!(
            coblang_rt_m13_debug_prepare(
                ptr::null_mut(),
                10,
                0,
                nm.as_ptr(),
                nm.len(),
                0,
                0,
                0,
                0,
                b"X".as_ptr(),
                1
            ),
            0
        );
        // Exercise modes `0, 2, 3, 4, 5`, `"PERFORM LOOP"` (sets mode 1 which suppresses next `"FALL THROUGH"`), and `"START PROGRAM"`.
        for mode in [0, 2, 3, 4, 5] {
            coblang_rt_m13_debug_set_suppress(mode);
            assert_eq!(
                coblang_rt_m13_debug_prepare(
                    dbg.as_mut_ptr(),
                    dbg.len(),
                    15,
                    nm.as_ptr(),
                    nm.len(),
                    3,
                    1,
                    2,
                    3,
                    ft.as_ptr(),
                    ft.len()
                ),
                1
            );
            // While inside declarative, nested `debug_prepare` returns `0`.
            assert_eq!(
                coblang_rt_m13_debug_prepare(
                    dbg.as_mut_ptr(),
                    dbg.len(),
                    15,
                    nm.as_ptr(),
                    nm.len(),
                    0,
                    0,
                    0,
                    0,
                    ft.as_ptr(),
                    ft.len()
                ),
                0
            );
            assert_eq!(coblang_rt_m13_finish_decl(0), 0);
        }
        let pl = b"PERFORM LOOP";
        assert_eq!(
            coblang_rt_m13_debug_prepare(
                dbg.as_mut_ptr(),
                dbg.len(),
                0,
                nm.as_ptr(),
                nm.len(),
                0,
                0,
                0,
                0,
                pl.as_ptr(),
                pl.len()
            ),
            1
        );
        assert_eq!(coblang_rt_m13_finish_decl(0), 0);
        // Next `"FALL THROUGH"` is suppressed (`0`) because `"PERFORM LOOP"` armed mode `1`!
        assert_eq!(
            coblang_rt_m13_debug_prepare(
                dbg.as_mut_ptr(),
                dbg.len(),
                16,
                nm.as_ptr(),
                nm.len(),
                0,
                0,
                0,
                0,
                ft.as_ptr(),
                ft.len()
            ),
            0
        );

        // Recursive frame save/restore edge cases (empty restore, null/0-len save, save without frame, normal save+restore).
        assert_eq!(coblang_rt_m13_rec_frame_restore(), 0);
        assert_eq!(coblang_rt_m13_rec_frame_save(ptr::null_mut(), 4), 0);
        let mut cell = [7_u8, 8];
        assert_eq!(coblang_rt_m13_rec_frame_save(cell.as_mut_ptr(), 0), 0);
        assert_eq!(coblang_rt_m13_rec_frame_save(cell.as_mut_ptr(), 2), 0);
        assert_eq!(coblang_rt_m13_rec_frame_begin(), 0);
        assert_eq!(coblang_rt_m13_rec_frame_save(cell.as_mut_ptr(), 2), 0);
        cell = [0, 0];
        assert_eq!(coblang_rt_m13_rec_frame_restore(), 0);
        assert_eq!(cell, [7, 8]);
    }
}
