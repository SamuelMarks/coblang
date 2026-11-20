//! Exhaustive unit and 100% branch-coverage tests for `coblang-rt-abi`.

use std::ffi::c_char;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use coblang_common::{CoblangError, Result};
use coblang_rt::{
    CobField, CobFieldAttr, build_module_search_dirs, cob_init, cob_tidy, coblang_rt_call_dynamic,
    coblang_rt_display, coblang_rt_enter_call, coblang_rt_get_return_code,
    coblang_rt_is_subprogram, coblang_rt_leave_call, coblang_rt_prepare_stop_run,
    coblang_rt_set_return_code, coblang_rt_trim, loaded_modules_cache, validate_and_write_display,
};

/// Monotonic counter for per-test temp directories.
static ABI_TEST_SEQ: AtomicU64 = AtomicU64::new(1);

/// Mutex serializing tests that mutate process-global `CobolRuntimeContext` stacks/buffers.
static ABI_STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Verifies `#[repr(C)]` descriptors, lifecycle functions, `RETURN-CODE`, and subprogram call depth.
#[test]
fn lifecycle_return_code_and_subprogram_depth() {
    let _guard = ABI_STATE_LOCK.lock();
    let attr = CobFieldAttr {
        field_type: 0,
        digits: 4,
        scale: 0,
        flags: 0,
        pic_ptr: core::ptr::null(),
    };
    let mut byte = b'X';
    let field = CobField {
        size: 1,
        data: &raw mut byte,
        attr: &raw const attr,
    };
    assert_eq!(field.size, 1);

    #[expect(
        unsafe_code,
        reason = "Testing `cob_init` with null, negative, and valid `argv` arrays."
    )]
    // SAFETY: Both null `argv` and the stack `ptrs` array of NUL-terminated C strings uphold `cob_init` preconditions.
    unsafe {
        cob_init(0, core::ptr::null());
        cob_init(-1, core::ptr::null());
        let arg0 = c"prog";
        let arg1 = c"--flag";
        let ptrs: [*const c_char; 3] = [arg0.as_ptr(), core::ptr::null(), arg1.as_ptr()];
        cob_init(3, ptrs.as_ptr());
        cob_init(2, core::ptr::null());
    }

    assert_eq!(coblang_rt_get_return_code(), 0);
    coblang_rt_set_return_code(19);
    assert_eq!(coblang_rt_get_return_code(), 19);

    assert_eq!(coblang_rt_is_subprogram(), 0);
    coblang_rt_enter_call();
    assert_eq!(coblang_rt_is_subprogram(), 1);
    coblang_rt_leave_call();
    assert_eq!(coblang_rt_is_subprogram(), 0);

    assert_eq!(coblang_rt_prepare_stop_run(5), 5);
    assert_eq!(coblang_rt_get_return_code(), 5);
    cob_tidy();
}

/// Verifies `validate_and_write_display` valid stdout, stderr, and zero-count paths.
#[test]
fn display_abi_valid_writes() -> Result<()> {
    let msg = b"HI";
    let ptrs = [msg.as_ptr(), core::ptr::null()];
    let lens = [msg.len(), 0_usize];
    let kinds = [0_u8, 0_u8];
    let scales = [0_i16, 0_i16];

    let mut out = Vec::new();
    let mut err = Vec::new();

    #[expect(
        unsafe_code,
        reason = "Testing `validate_and_write_display` valid array slices."
    )]
    // SAFETY: All non-null test slices have matching `count` lengths.
    unsafe {
        validate_and_write_display(
            0,
            1,
            (
                ptrs.as_ptr(),
                lens.as_ptr(),
                kinds.as_ptr(),
                scales.as_ptr(),
                2,
            ),
            &mut out,
            &mut err,
        )?;
        assert_eq!(out, [b'H', b'I', 0x0A]);
        assert_eq!(err.len(), 0);

        validate_and_write_display(
            1,
            0,
            (
                ptrs.as_ptr(),
                lens.as_ptr(),
                kinds.as_ptr(),
                scales.as_ptr(),
                1,
            ),
            &mut out,
            &mut err,
        )?;
        assert_eq!(err, b"HI");

        validate_and_write_display(
            0,
            0,
            (
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                0,
            ),
            &mut out,
            &mut err,
        )?;
        assert_eq!(
            coblang_rt_display(
                1,
                0,
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                0,
            ),
            0
        );
    }
    Ok(())
}

/// Verifies all five null-pointer guard branches of `validate_and_write_display` and `coblang_rt_display`.
#[test]
fn display_abi_null_guards() {
    let msg = b"HI";
    let ptrs = [msg.as_ptr()];
    let bad_ptrs = [core::ptr::null::<u8>()];
    let lens = [msg.len()];
    let kinds = [0_u8];
    let scales = [0_i16];
    let mut out = Vec::new();
    let mut err = Vec::new();

    #[expect(
        unsafe_code,
        reason = "Testing null-pointer guard branches of `validate_and_write_display` and `coblang_rt_display`."
    )]
    // SAFETY: Null pointers intentionally exercise boundary guards before any dereference.
    unsafe {
        let cases = [
            (
                (
                    core::ptr::null(),
                    lens.as_ptr(),
                    kinds.as_ptr(),
                    scales.as_ptr(),
                    1,
                ),
                "ptrs",
            ),
            (
                (
                    ptrs.as_ptr(),
                    core::ptr::null(),
                    kinds.as_ptr(),
                    scales.as_ptr(),
                    1,
                ),
                "lens",
            ),
            (
                (
                    ptrs.as_ptr(),
                    lens.as_ptr(),
                    core::ptr::null(),
                    scales.as_ptr(),
                    1,
                ),
                "kinds",
            ),
            (
                (
                    ptrs.as_ptr(),
                    lens.as_ptr(),
                    kinds.as_ptr(),
                    core::ptr::null(),
                    1,
                ),
                "scales",
            ),
            (
                (
                    bad_ptrs.as_ptr(),
                    lens.as_ptr(),
                    kinds.as_ptr(),
                    scales.as_ptr(),
                    1,
                ),
                "ptrs[i]",
            ),
        ];
        for (arrays, expected_param) in cases {
            let res = validate_and_write_display(0, 0, arrays, &mut out, &mut err);
            assert!(matches!(
                res,
                Err(CoblangError::AbiNullOrInvalidPointer { param_name, .. }) if param_name == expected_param
            ));
        }

        assert_eq!(
            coblang_rt_display(
                0,
                0,
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                0,
            ),
            0
        );
        assert_eq!(
            coblang_rt_display(
                0,
                0,
                core::ptr::null(),
                lens.as_ptr(),
                kinds.as_ptr(),
                scales.as_ptr(),
                1,
            ),
            -1
        );
    }
}

/// Verifies `coblang_rt_trim` valid, empty, and null-pointer guard branches.
#[test]
fn trim_abi_valid_and_null_guards() {
    #[expect(
        unsafe_code,
        reason = "Testing `coblang_rt_trim` across valid slices and null-pointer guards."
    )]
    // SAFETY: Stack byte slice and output pointers are valid; null pointers test guard branches.
    unsafe {
        // `coblang_rt_trim` valid, empty, and null-pointer guards.
        let padded = b"  Marty  ";
        let mut t_ptr: *const u8 = core::ptr::null();
        let mut t_len: usize = 0;
        assert_eq!(
            coblang_rt_trim(
                padded.as_ptr(),
                padded.len(),
                0,
                &raw mut t_ptr,
                &raw mut t_len
            ),
            0
        );
        let trimmed = core::slice::from_raw_parts(t_ptr, t_len);
        assert_eq!(trimmed, b"Marty");

        assert_eq!(
            coblang_rt_trim(core::ptr::null(), 0, 0, &raw mut t_ptr, &raw mut t_len),
            0
        );
        assert_eq!(t_len, 0);

        assert_eq!(
            coblang_rt_trim(core::ptr::null(), 4, 0, &raw mut t_ptr, &raw mut t_len),
            -1
        );
        assert_eq!(
            coblang_rt_trim(
                padded.as_ptr(),
                padded.len(),
                0,
                core::ptr::null_mut(),
                &raw mut t_len
            ),
            -1
        );
        assert_eq!(
            coblang_rt_trim(
                padded.as_ptr(),
                padded.len(),
                0,
                &raw mut t_ptr,
                core::ptr::null_mut()
            ),
            -1
        );
    }
}

/// Verifies `build_module_search_dirs` and `coblang_rt_call_dynamic` across valid shared modules and error guards.
#[test]
fn dynamic_call_abi_and_search_dirs() -> Result<()> {
    let _guard = ABI_STATE_LOCK.lock();
    let dirs_none = build_module_search_dirs(None);
    assert_eq!(dirs_none.len(), 1);
    let dirs_some = build_module_search_dirs(Some("/tmp/a::/tmp/b"));
    assert_eq!(dirs_some.len(), 3);

    let id = ABI_TEST_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("coblang-rt-abi-test-{}-{id}", std::process::id()));
    fs::create_dir_all(&dir)?;

    let c_path = dir.join("abimod.c");
    let so_path = dir.join("abimod.so");
    fs::write(&c_path, "int __coblang_prog_ABIMOD(void) { return 23; }\n")?;
    let status = Command::new("cc")
        .arg("-shared")
        .arg("-fPIC")
        .arg(&c_path)
        .arg("-o")
        .arg(&so_path)
        .status()?;
    assert!(status.success());

    #[expect(
        unsafe_code,
        reason = "Testing `coblang_rt_call_dynamic` across null, empty, missing, and valid `COB_LIBRARY_PATH` modules."
    )]
    // SAFETY: Test runs single-threaded on this scratch module name; pointer slices are valid stack bytes.
    unsafe {
        let mut rc_out = 0_i32;
        assert_eq!(
            coblang_rt_call_dynamic(b"ABIMOD".as_ptr(), 6, core::ptr::null_mut()),
            -1
        );
        assert_eq!(
            coblang_rt_call_dynamic(core::ptr::null(), 3, &raw mut rc_out),
            -1
        );
        assert_eq!(
            coblang_rt_call_dynamic(core::ptr::null(), 0, &raw mut rc_out),
            -1
        );
        let missing = b"NO_SUCH_MOD_XYZ";
        assert_eq!(
            coblang_rt_call_dynamic(missing.as_ptr(), missing.len(), &raw mut rc_out),
            -1
        );

        std::env::set_var("COB_LIBRARY_PATH", &dir);
        let valid = b"ABIMOD";
        assert_eq!(
            coblang_rt_call_dynamic(valid.as_ptr(), valid.len(), &raw mut rc_out),
            0
        );
        assert_eq!(rc_out, 23);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = loaded_modules_cache().lock();
            std::panic::resume_unwind(Box::new("simulated cache poison"));
        }));
        cob_tidy();
    }

    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Deterministic failing `Write` stream for testing `flush_staged_display_to` I/O failure paths.
struct AbiFailWriter;

impl std::io::Write for AbiFailWriter {
    /// Always fails with an I/O error.
    ///
    /// # Arguments
    ///
    /// * `_buf` - Ignored write slice.
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("simulated ABI write failure"))
    }

    /// Always fails with an I/O error.
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("simulated ABI flush failure"))
    }
}

/// Verifies `read_raw_slice`, `write_raw_slice`, and `coblang_rt_move_desc`.
#[test]
fn part5_move_abi_coverage() -> Result<()> {
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{HirType, JustifiedSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt::{coblang_rt_move_desc, read_raw_slice, write_raw_slice};
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};

    let alpha_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 3,
            justified: JustifiedSpec::Left,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let np: *const u8 = core::ptr::null();
    let nmp: *mut u8 = core::ptr::null_mut();

    #[expect(
        unsafe_code,
        reason = "Exercising audited `coblang_rt_move_desc` across valid buffers and null guards."
    )]
    // SAFETY: Non-null pointers reference `buf3` / `alpha_spec`; null pointers test guard branches.
    unsafe {
        assert_eq!(read_raw_slice(np, 0, "s", "p")?.len(), 0);
        assert!(read_raw_slice(np, 2, "s", "p").is_err());
        assert_eq!(write_raw_slice(nmp, 0, "s", "p")?.len(), 0);
        assert!(write_raw_slice(nmp, 2, "s", "p").is_err());

        let mut buf3 = *b"ABC";
        let ap = alpha_spec.as_ptr();
        let al = alpha_spec.len();
        for (sk, sp, sl, ssp, ssl, dp, dsp, dsl) in [
            (1, np, 2, np, 0, buf3.as_mut_ptr(), ap, al),
            (0, buf3.as_ptr(), 3, np, 2, buf3.as_mut_ptr(), ap, al),
            (1, buf3.as_ptr(), 3, np, 0, buf3.as_mut_ptr(), np, 2),
            (1, buf3.as_ptr(), 3, np, 0, nmp, ap, al),
            (1, buf3.as_ptr(), 3, np, 0, buf3.as_mut_ptr(), np, 0),
        ] {
            assert_eq!(
                coblang_rt_move_desc(sk, sp, sl, ssp, ssl, dp, 3, dsp, dsl),
                -1
            );
        }
        assert_eq!(
            coblang_rt_move_desc(1, buf3.as_ptr(), 2, np, 0, buf3.as_mut_ptr(), 3, ap, al),
            0
        );
        assert_eq!(&buf3, b"AB ");
    }
    Ok(())
}

/// Verifies `coblang_rt_disp_*` and `flush_staged_display_to`.
#[test]
fn part5_display_staging_abi_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{BlankZeroSpec, HirType, NumericSignSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt::{
        coblang_rt_disp_begin, coblang_rt_disp_flush, coblang_rt_disp_push_field,
        coblang_rt_disp_push_hex, coblang_rt_disp_push_raw, flush_staged_display_to,
    };
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};

    let _guard = ABI_STATE_LOCK.lock();

    let zd2_spec = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::try_new(2)?,
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let np: *const u8 = core::ptr::null();

    #[expect(
        unsafe_code,
        reason = "Exercising audited `coblang_rt_disp_*` C-ABI functions across valid buffers and null guards."
    )]
    // SAFETY: Non-null pointers reference stack slices; null pointers test guard branches.
    unsafe {
        coblang_rt_disp_begin();
        assert_eq!(coblang_rt_disp_push_raw(np, 2), -1);
        assert_eq!(coblang_rt_disp_push_raw(b"V=".as_ptr(), 2), 0);
        assert_eq!(
            coblang_rt_disp_push_field(np, 2, zd2_spec.as_ptr(), zd2_spec.len()),
            -1
        );
        assert_eq!(coblang_rt_disp_push_field(b"12".as_ptr(), 2, np, 2), -1);
        assert_eq!(coblang_rt_disp_push_field(b"12".as_ptr(), 2, np, 0), -1);
        assert_eq!(
            coblang_rt_disp_push_field(b"12".as_ptr(), 2, zd2_spec.as_ptr(), zd2_spec.len()),
            0
        );
        assert_eq!(coblang_rt_disp_push_hex(np, 1), -1);
        assert_eq!(coblang_rt_disp_push_hex(&0xAF_u8, 1), 0);

        let mut out_vec = Vec::new();
        let mut err_vec = Vec::new();
        assert_eq!(flush_staged_display_to(0, 1, &mut out_vec, &mut err_vec), 0);
        assert_eq!(out_vec, b"V=12AF\n");
        assert_eq!(coblang_rt_disp_push_raw(b"E".as_ptr(), 1), 0);
        assert_eq!(flush_staged_display_to(1, 0, &mut out_vec, &mut err_vec), 0);
        assert_eq!(err_vec, b"E");
        assert_eq!(
            flush_staged_display_to(0, 1, &mut AbiFailWriter, &mut err_vec),
            -1
        );
        assert_eq!(
            flush_staged_display_to(1, 1, &mut out_vec, &mut AbiFailWriter),
            -1
        );
        coblang_rt_disp_begin();
        assert_eq!(coblang_rt_disp_flush(0, 0), 0);
    }
    Ok(())
}

/// Verifies `coblang_rt_dec_*` stack, store, and `cmp` C-ABI entrypoints.
#[test]
fn part5_decimal_stack_abi_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{BlankZeroSpec, HirType, NumericSignSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt::{
        coblang_rt_dec_begin, coblang_rt_dec_binop, coblang_rt_dec_cmp, coblang_rt_dec_neg,
        coblang_rt_dec_push_ascii, coblang_rt_dec_push_field, coblang_rt_dec_store,
    };
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};

    let _guard = ABI_STATE_LOCK.lock();

    let zd2 = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::try_new(2)?,
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let zd1 = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::ONE,
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let np: *const u8 = core::ptr::null();
    let nmp: *mut u8 = core::ptr::null_mut();

    #[expect(
        unsafe_code,
        reason = "Exercising audited `coblang_rt_dec_*` C-ABI functions across valid buffers and null guards."
    )]
    // SAFETY: Non-null pointers reference stack buffers of matching lengths; null pointers test guard branches.
    unsafe {
        coblang_rt_dec_begin();
        assert_eq!(coblang_rt_dec_binop(0), -1);
        assert_eq!(coblang_rt_dec_cmp(0), -1);
        let mut z2 = *b"00";
        assert_eq!(
            coblang_rt_dec_store(z2.as_mut_ptr(), 2, zd2.as_ptr(), zd2.len(), 0, 0, 0),
            -1
        );
        assert_eq!(coblang_rt_dec_store(z2.as_mut_ptr(), 2, np, 2, 0, 0, 0), -1);
        assert_eq!(
            coblang_rt_dec_store(nmp, 2, zd2.as_ptr(), zd2.len(), 0, 0, 0),
            -1
        );

        assert_eq!(
            coblang_rt_dec_push_field(np, 2, zd2.as_ptr(), zd2.len()),
            -1
        );
        assert_eq!(coblang_rt_dec_push_field(b"15".as_ptr(), 2, np, 2), -1);
        assert_eq!(coblang_rt_dec_push_field(b"15".as_ptr(), 2, np, 0), -1);
        assert_eq!(
            coblang_rt_dec_push_field(b"15".as_ptr(), 2, zd2.as_ptr(), zd2.len()),
            0
        );
        assert_eq!(coblang_rt_dec_push_ascii(np, 2), -1);
        assert_eq!(coblang_rt_dec_push_ascii(b"xx".as_ptr(), 2), -1);
        assert_eq!(coblang_rt_dec_push_ascii(b"5".as_ptr(), 1), 0);
        coblang_rt_dec_neg();
        assert_eq!(coblang_rt_dec_binop(0), 0);
        assert_eq!(
            coblang_rt_dec_store(z2.as_mut_ptr(), 2, zd2.as_ptr(), zd2.len(), 0, 1, 0),
            0
        );
        assert_eq!(&z2, b"10");
        assert_eq!(
            coblang_rt_dec_store(z2.as_mut_ptr(), 1, zd1.as_ptr(), zd1.len(), 0, 1, 0),
            1
        );
        assert_eq!(coblang_rt_dec_push_ascii(b"10".as_ptr(), 2), 0);
        assert_eq!(coblang_rt_dec_cmp(0), 1);
    }
    Ok(())
}

/// Verifies `coblang_rt_dec_div_rem` across null guards, underflow, exact, overflow, and 0-len buffers.
#[test]
fn part5_div_rem_abi_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{BlankZeroSpec, HirType, NumericSignSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt::{coblang_rt_dec_begin, coblang_rt_dec_div_rem, coblang_rt_dec_push_ascii};
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};

    let _guard = ABI_STATE_LOCK.lock();

    let zd2 = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::try_new(2)?,
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let idx = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Index),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let np: *const u8 = core::ptr::null();
    let nmp: *mut u8 = core::ptr::null_mut();

    #[expect(
        unsafe_code,
        reason = "Exercising audited `coblang_rt_dec_div_rem` across valid buffers and null guards."
    )]
    // SAFETY: Non-null pointers reference stack buffers; null pointers test guard branches.
    unsafe {
        let mut q2 = *b"00";
        let mut r2 = *b"00";
        let zp = zd2.as_ptr();
        let zl = zd2.len();
        coblang_rt_dec_begin();
        for (qp, qsp, qsl, rp, rsp, rsl) in [
            (nmp, zp, zl, r2.as_mut_ptr(), zp, zl),
            (q2.as_mut_ptr(), zp, zl, nmp, zp, zl),
            (q2.as_mut_ptr(), np, 2, r2.as_mut_ptr(), zp, zl),
            (q2.as_mut_ptr(), zp, zl, r2.as_mut_ptr(), np, 2),
            (q2.as_mut_ptr(), zp, zl, r2.as_mut_ptr(), zp, zl),
        ] {
            assert_eq!(
                coblang_rt_dec_div_rem(qp, 2, qsp, qsl, 0, rp, 2, rsp, rsl, 0),
                -1
            );
        }
        assert_eq!(coblang_rt_dec_push_ascii(b"17".as_ptr(), 2), 0);
        assert_eq!(coblang_rt_dec_push_ascii(b"5".as_ptr(), 1), 0);
        assert_eq!(
            coblang_rt_dec_div_rem(q2.as_mut_ptr(), 2, zp, zl, 0, r2.as_mut_ptr(), 2, zp, zl, 0),
            0
        );
        assert_eq!((&q2, &r2), (b"03", b"02"));

        assert_eq!(coblang_rt_dec_push_ascii(b"10".as_ptr(), 2), 0);
        assert_eq!(coblang_rt_dec_push_ascii(b"0".as_ptr(), 1), 0);
        assert_eq!(
            coblang_rt_dec_div_rem(q2.as_mut_ptr(), 2, zp, zl, 0, r2.as_mut_ptr(), 2, zp, zl, 1),
            1
        );
        assert_eq!(coblang_rt_dec_push_ascii(b"7".as_ptr(), 1), 0);
        assert_eq!(coblang_rt_dec_push_ascii(b"2".as_ptr(), 1), 0);
        assert_eq!(
            coblang_rt_dec_div_rem(
                nmp,
                0,
                idx.as_ptr(),
                idx.len(),
                0,
                nmp,
                0,
                idx.as_ptr(),
                idx.len(),
                0
            ),
            0
        );
    }
    Ok(())
}

/// Verifies `coblang_rt_cmp_alpha`, `coblang_rt_class_check`, and `coblang_rt_init_*` C-ABI entrypoints.
#[test]
fn part5_cmp_class_and_init_abi_coverage() -> Result<()> {
    use coblang_common::{DigitCount, ScaleFactor};
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{BlankZeroSpec, DataCategory, HirType, JustifiedSpec, NumericSignSpec};
    use coblang_pic::{DefaultInitMode, FillerPolicy, InitCategoryMask, MoveFieldDesc};
    use coblang_rt::{
        coblang_rt_class_check, coblang_rt_cmp_alpha, coblang_rt_init_begin, coblang_rt_init_exec,
        coblang_rt_init_push_rep,
    };
    use coblang_rt_core::desc_codec::{
        DecodedInitPlan, FillerMembership, RedefinesMembership, RuntimeFieldSpec, RuntimeInitEntry,
        encode_data_category, encode_field_spec, encode_init_plan,
    };

    let _guard = ABI_STATE_LOCK.lock();

    let zd2 = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::ZonedDecimal {
            digits: DigitCount::try_new(2)?,
            scale: ScaleFactor::ZERO,
            sign: NumericSignSpec::Unsigned,
            blank_zero: BlankZeroSpec::RetainZero,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let mut buf3 = *b"ABC";
    let np: *const u8 = core::ptr::null();
    let nmp: *mut u8 = core::ptr::null_mut();

    #[expect(
        unsafe_code,
        reason = "Exercising audited `cmp_alpha`, `class_check`, and `init_*` C-ABI entrypoints."
    )]
    // SAFETY: Non-null pointers reference stack buffers of matching lengths; null pointers test guard branches.
    unsafe {
        assert_eq!(coblang_rt_cmp_alpha(1, np, 2, 1, b"AB".as_ptr(), 2, 0), -1);
        assert_eq!(coblang_rt_cmp_alpha(1, b"AB".as_ptr(), 2, 1, np, 2, 0), -1);
        assert_eq!(
            coblang_rt_cmp_alpha(1, b"AB".as_ptr(), 2, 1, b"AB ".as_ptr(), 3, 0),
            1
        );

        assert_eq!(
            coblang_rt_class_check(np, 2, zd2.as_ptr(), zd2.len(), 0),
            -1
        );
        assert_eq!(coblang_rt_class_check(b"12".as_ptr(), 2, np, 2, 0), -1);
        assert_eq!(coblang_rt_class_check(b"12".as_ptr(), 2, np, 0, 0), -1);
        assert_eq!(
            coblang_rt_class_check(b"12".as_ptr(), 2, zd2.as_ptr(), zd2.len(), 0),
            1
        );

        coblang_rt_init_begin();
        for (cat, k, p, l, sp, sl) in [
            (99, 1, b"A".as_ptr(), 1, np, 0),
            (1, 1, np, 2, np, 0),
            (1, 0, b"A".as_ptr(), 1, np, 2),
            (1, 0, b"A".as_ptr(), 1, np, 0),
        ] {
            assert_eq!(coblang_rt_init_push_rep(cat, k, p, l, sp, sl), -1);
        }
        let cat_an = encode_data_category(DataCategory::Alphanumeric);
        assert_eq!(
            coblang_rt_init_push_rep(cat_an, 1, b"ZZZ".as_ptr(), 3, np, 0),
            0
        );

        let plan_bytes = encode_init_plan(&DecodedInitPlan {
            filler_policy: FillerPolicy::SkipFiller,
            to_value_mask: InitCategoryMask::NONE,
            default_mode: DefaultInitMode::StandardCobol,
            entries: vec![RuntimeInitEntry {
                offset: 0,
                dims: Vec::new(),
                byte_len: 3,
                desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
                    byte_len: 3,
                    justified: JustifiedSpec::Left,
                }),
                filler: FillerMembership::NamedField,
                redefines: RedefinesMembership::PrimaryStorage,
                value_bytes: None,
            }],
        });
        assert_eq!(coblang_rt_init_exec(buf3.as_mut_ptr(), 3, np, 2), -1);
        assert_eq!(
            coblang_rt_init_exec(nmp, 3, plan_bytes.as_ptr(), plan_bytes.len()),
            -1
        );
        assert_eq!(coblang_rt_init_exec(buf3.as_mut_ptr(), 3, np, 0), -1);
        assert_eq!(
            coblang_rt_init_exec(buf3.as_mut_ptr(), 3, plan_bytes.as_ptr(), plan_bytes.len()),
            0
        );
        assert_eq!(&buf3, b"ZZZ");
    }
    Ok(())
}

/// Verifies M8 `coblang_rt_cmp_alpha_ex`, `coblang_rt_class_custom`, and `coblang_rt_{get,set}_switch`.
#[test]
fn part6_m8_cmp_alpha_ex_custom_class_and_switches_abi() {
    use coblang_rt::{
        coblang_rt_class_custom, coblang_rt_cmp_alpha_ex, coblang_rt_get_switch,
        coblang_rt_set_switch,
    };

    let _guard = ABI_STATE_LOCK.lock();
    let np: *const u8 = core::ptr::null();
    let a = b"AB";
    let mut coll = [0_u8; 256];
    for (i, slot) in coll.iter_mut().enumerate() {
        *slot = u8::try_from(i).unwrap_or(0);
    }
    let mut bm = [0_u8; 32];
    let mut dummy = 0_u8;
    *bm.get_mut(usize::from(b'A' >> 3)).unwrap_or(&mut dummy) |= 1_u8 << (b'A' & 7);
    *bm.get_mut(usize::from(b'B' >> 3)).unwrap_or(&mut dummy) |= 1_u8 << (b'B' & 7);

    #[expect(
        unsafe_code,
        reason = "Exercising audited M8 C-ABI comparison and custom-class entrypoints."
    )]
    // SAFETY: Non-null pointers reference valid stack arrays; null pointers with non-zero lengths test guard branches.
    unsafe {
        assert_eq!(
            coblang_rt_cmp_alpha_ex(1, np, 2, np, 0, 1, a.as_ptr(), 2, np, 0, 0, np, 0),
            -1
        );
        assert_eq!(
            coblang_rt_cmp_alpha_ex(1, a.as_ptr(), 2, np, 2, 1, a.as_ptr(), 2, np, 0, 0, np, 0),
            -1
        );
        assert_eq!(
            coblang_rt_cmp_alpha_ex(1, a.as_ptr(), 2, np, 0, 1, np, 2, np, 0, 0, np, 0),
            -1
        );
        assert_eq!(
            coblang_rt_cmp_alpha_ex(1, a.as_ptr(), 2, np, 0, 1, a.as_ptr(), 2, np, 2, 0, np, 0),
            -1
        );
        assert_eq!(
            coblang_rt_cmp_alpha_ex(1, a.as_ptr(), 2, np, 0, 1, a.as_ptr(), 2, np, 0, 0, np, 256),
            -1
        );
        assert_eq!(
            coblang_rt_cmp_alpha_ex(
                1,
                a.as_ptr(),
                2,
                np,
                0,
                1,
                a.as_ptr(),
                2,
                np,
                0,
                0,
                coll.as_ptr(),
                256
            ),
            1
        );

        assert_eq!(coblang_rt_class_custom(np, 2, bm.as_ptr()), -1);
        assert_eq!(coblang_rt_class_custom(a.as_ptr(), 2, np), -1);
        assert_eq!(coblang_rt_class_custom(a.as_ptr(), 2, bm.as_ptr()), 1);
        assert_eq!(coblang_rt_class_custom(b"AZ".as_ptr(), 2, bm.as_ptr()), 0);
    }

    assert_eq!(coblang_rt_set_switch(3, 1), 0);
    assert_eq!(coblang_rt_set_switch(3, 0), 0);
    assert_eq!(coblang_rt_get_switch(3), 0);
    assert_eq!(coblang_rt_set_switch(999, 1), 0);
    assert_eq!(coblang_rt_get_switch(999), 0);
}

/// Verifies M9 `coblang_rt_check_odo`, `coblang_rt_check_sub`, `coblang_rt_check_refmod`, and `coblang_rt_sort_table` C-ABI entrypoints.
#[test]
fn m9_bounds_and_sort_table_abi_coverage() {
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{HirSortDirection, HirType, JustifiedSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt::{
        coblang_rt_check_odo, coblang_rt_check_refmod, coblang_rt_check_sub,
        coblang_rt_get_return_code, coblang_rt_sort_table,
    };
    use coblang_rt_core::desc_codec::{
        DecodedTableSortPlan, RuntimeFieldSpec, RuntimeTableSortKey, encode_table_sort_plan,
    };

    let _guard = ABI_STATE_LOCK.lock();
    let loc = b"m9.cob:10";
    let np: *const u8 = core::ptr::null();
    let nmp: *mut u8 = core::ptr::null_mut();

    let plan_bytes = encode_table_sort_plan(&DecodedTableSortPlan {
        stride: 2,
        collating_ranks: None,
        keys: vec![RuntimeTableSortKey {
            direction: HirSortDirection::Ascending,
            rel_offset: 0,
            byte_len: 2,
            spec: RuntimeFieldSpec {
                desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
                    byte_len: 2,
                    justified: JustifiedSpec::Left,
                }),
                packed_sign_rule: PackedSignPolicy::Gnu,
            },
        }],
    });

    #[expect(
        unsafe_code,
        reason = "Exercising audited M9 bounds-checking and table-sort C-ABI entrypoints."
    )]
    // SAFETY: Non-null pointers reference stack buffers; null pointers test guard branches.
    unsafe {
        assert_eq!(coblang_rt_check_odo(2, 1, 5, 0, np, 3), -1);
        assert_eq!(coblang_rt_check_odo(2, 1, 5, 0, loc.as_ptr(), loc.len()), 0);
        assert_eq!(coblang_rt_check_odo(9, 1, 5, 0, loc.as_ptr(), loc.len()), 0);
        assert_eq!(coblang_rt_check_odo(9, 1, 5, 1, loc.as_ptr(), loc.len()), 1);
        assert_eq!(coblang_rt_get_return_code(), 1);

        assert_eq!(coblang_rt_check_sub(1, 5, 0, np, 3), -1);
        assert_eq!(coblang_rt_check_sub(1, 5, 0, loc.as_ptr(), loc.len()), 0);
        assert_eq!(coblang_rt_check_sub(0, 5, 0, loc.as_ptr(), loc.len()), 0);
        assert_eq!(coblang_rt_check_sub(0, 5, 1, loc.as_ptr(), loc.len()), 1);

        assert_eq!(coblang_rt_check_refmod(4, 1, 0, 0, 0, np, 3), -1);
        assert_eq!(
            coblang_rt_check_refmod(4, 1, 0, 0, 0, loc.as_ptr(), loc.len()),
            0
        );
        assert_eq!(
            coblang_rt_check_refmod(4, 1, 1, 2, 0, loc.as_ptr(), loc.len()),
            0
        );
        assert_eq!(
            coblang_rt_check_refmod(4, 9, 1, 2, 0, loc.as_ptr(), loc.len()),
            0
        );
        assert_eq!(
            coblang_rt_check_refmod(4, 9, 0, 0, 1, loc.as_ptr(), loc.len()),
            1
        );

        let mut buf = *b"CCBbaa";
        assert_eq!(
            coblang_rt_sort_table(nmp, 6, 3, plan_bytes.as_ptr(), plan_bytes.len()),
            -1
        );
        assert_eq!(coblang_rt_sort_table(buf.as_mut_ptr(), 6, 3, np, 4), -1);
        assert_eq!(
            coblang_rt_sort_table(buf.as_mut_ptr(), 6, 3, b"x".as_ptr(), 1),
            -1
        );
        assert_eq!(
            coblang_rt_sort_table(
                buf.as_mut_ptr(),
                6,
                -2,
                plan_bytes.as_ptr(),
                plan_bytes.len()
            ),
            0
        );
        assert_eq!(
            coblang_rt_sort_table(
                buf.as_mut_ptr(),
                6,
                3,
                plan_bytes.as_ptr(),
                plan_bytes.len()
            ),
            0
        );
        assert_eq!(&buf, b"BbCCaa");
    }
}

/// Verifies M10 `coblang_rt_accept`, `coblang_rt_disp_flush_env`, and `coblang_rt_str_*` C-ABI entrypoints.
#[test]
fn m10_accept_and_string_abi_coverage() {
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{HirType, JustifiedSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt::{
        coblang_rt_accept, coblang_rt_disp_begin, coblang_rt_disp_flush_env,
        coblang_rt_disp_push_raw, coblang_rt_str_begin, coblang_rt_str_exec,
        coblang_rt_str_last_ptr, coblang_rt_str_push_src, coblang_rt_str_set_delim,
    };
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};

    let _guard = ABI_STATE_LOCK.lock();
    let np: *const u8 = core::ptr::null();
    let nmp: *mut u8 = core::ptr::null_mut();
    let a4 = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });

    #[expect(
        unsafe_code,
        reason = "Testing audited M10 `ACCEPT` and `STRING` C-ABI entrypoints."
    )]
    // SAFETY: Non-null pointers reference stack buffers; null pointers test guard branches.
    unsafe {
        coblang_rt_disp_begin();
        assert_eq!(coblang_rt_disp_push_raw(b"ABI_K".as_ptr(), 5), 0);
        assert_eq!(coblang_rt_disp_flush_env(0), 0);
        assert_eq!(coblang_rt_accept(13, 1, nmp, 0, np, 0), 1);
        coblang_rt_disp_begin();
        assert_eq!(coblang_rt_disp_push_raw(b"OK".as_ptr(), 2), 0);
        assert_eq!(coblang_rt_disp_flush_env(1), 0);

        let mut dst4 = *b"....";
        assert_eq!(coblang_rt_accept(13, 0, nmp, 4, a4.as_ptr(), a4.len()), -1);
        assert_eq!(coblang_rt_accept(13, 0, dst4.as_mut_ptr(), 4, np, 4), -1);
        assert_eq!(
            coblang_rt_accept(13, 0, dst4.as_mut_ptr(), 4, b"x".as_ptr(), 1),
            -1
        );
        assert_eq!(
            coblang_rt_accept(13, 0, dst4.as_mut_ptr(), 4, a4.as_ptr(), a4.len()),
            0
        );
        assert_eq!(&dst4, b"OK  ");

        coblang_rt_str_begin();
        assert_eq!(coblang_rt_str_set_delim(1, np, 1, np, 0), -1);
        assert_eq!(coblang_rt_str_set_delim(1, b"|".as_ptr(), 1, np, 1), -1);
        assert_eq!(
            coblang_rt_str_set_delim(1, b"|".as_ptr(), 1, b"x".as_ptr(), 1),
            -1
        );
        assert_eq!(coblang_rt_str_set_delim(1, b"|".as_ptr(), 1, np, 0), 0);
        assert_eq!(coblang_rt_str_push_src(np, 1, np, 0), -1);
        assert_eq!(coblang_rt_str_push_src(b"A".as_ptr(), 1, np, 1), -1);
        assert_eq!(
            coblang_rt_str_push_src(b"A".as_ptr(), 1, b"x".as_ptr(), 1),
            -1
        );
        assert_eq!(coblang_rt_str_push_src(b"AB|Z".as_ptr(), 4, np, 0), 0);
        assert_eq!(coblang_rt_str_exec(nmp, 4, 1), -1);
        assert_eq!(coblang_rt_str_exec(dst4.as_mut_ptr(), 4, 99), 1);
        coblang_rt_str_begin();
        assert_eq!(coblang_rt_str_push_src(b"HI".as_ptr(), 2, np, 0), 0);
        assert_eq!(coblang_rt_str_exec(dst4.as_mut_ptr(), 4, 1), 0);
        assert_eq!(coblang_rt_str_last_ptr(), 3);
    }
}

/// Verifies M10 `coblang_rt_unstr_begin` and `coblang_rt_unstr_push_delim` C-ABI guards and staging.
#[test]
fn m10_unstring_abi_begin_and_delim_coverage() {
    use coblang_rt::{
        coblang_rt_unstr_begin, coblang_rt_unstr_push_delim, coblang_rt_unstr_set_delim_spec,
    };

    let _guard = ABI_STATE_LOCK.lock();
    let np: *const u8 = core::ptr::null();

    #[expect(
        unsafe_code,
        reason = "Testing audited M10 `UNSTRING` begin/delim C-ABI entrypoints."
    )]
    // SAFETY: Non-null pointers reference stack buffers; null pointers test guard branches.
    unsafe {
        assert_eq!(coblang_rt_unstr_begin(np, 1, np, 0, 1), -1);
        assert_eq!(coblang_rt_unstr_begin(b"A".as_ptr(), 1, np, 1, 1), -1);
        assert_eq!(
            coblang_rt_unstr_begin(b"A".as_ptr(), 1, b"x".as_ptr(), 1, 1),
            -1
        );
        assert_eq!(coblang_rt_unstr_begin(b"A--B,C".as_ptr(), 6, np, 0, 1), 0);
        assert_eq!(coblang_rt_unstr_push_delim(0, np, 1, np, 0), -1);
        assert_eq!(coblang_rt_unstr_push_delim(0, b"-".as_ptr(), 1, np, 1), -1);
        assert_eq!(
            coblang_rt_unstr_push_delim(0, b"-".as_ptr(), 1, b"x".as_ptr(), 1),
            -1
        );
        assert_eq!(coblang_rt_unstr_push_delim(1, b"-".as_ptr(), 1, np, 0), 0);
        assert_eq!(coblang_rt_unstr_push_delim(0, b",".as_ptr(), 1, np, 0), 0);
        assert_eq!(coblang_rt_unstr_set_delim_spec(np, 1), -1);
    }
}

/// Verifies M10 `coblang_rt_unstr_step`, `coblang_rt_unstr_finish`, and `coblang_rt_unstr_tally_delta` C-ABI entrypoints.
#[test]
fn m10_unstring_abi_step_and_finish_coverage() {
    use coblang_decimal::PackedSignPolicy;
    use coblang_hir::{HirType, JustifiedSpec};
    use coblang_pic::MoveFieldDesc;
    use coblang_rt::{
        coblang_rt_unstr_begin, coblang_rt_unstr_finish, coblang_rt_unstr_push_delim,
        coblang_rt_unstr_set_delim_spec, coblang_rt_unstr_step, coblang_rt_unstr_tally_delta,
    };
    use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};

    let _guard = ABI_STATE_LOCK.lock();
    let np: *const u8 = core::ptr::null();
    let nmp: *mut u8 = core::ptr::null_mut();
    let a4 = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 4,
            justified: JustifiedSpec::Left,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    let a1 = encode_field_spec(&RuntimeFieldSpec {
        desc: MoveFieldDesc::standard_gnu(HirType::Alphanumeric {
            byte_len: 1,
            justified: JustifiedSpec::Left,
        }),
        packed_sign_rule: PackedSignPolicy::Gnu,
    });

    #[expect(
        unsafe_code,
        reason = "Testing audited M10 `UNSTRING` step/finish C-ABI entrypoints."
    )]
    // SAFETY: Non-null pointers reference stack buffers; null pointers test guard branches.
    unsafe {
        let mut dst4 = *b"....";
        let mut dm = [b' '; 1];
        assert_eq!(coblang_rt_unstr_begin(b"A--B,C".as_ptr(), 6, np, 0, 1), 0);
        assert_eq!(coblang_rt_unstr_push_delim(1, b"-".as_ptr(), 1, np, 0), 0);
        assert_eq!(coblang_rt_unstr_push_delim(0, b",".as_ptr(), 1, np, 0), 0);
        assert_eq!(coblang_rt_unstr_set_delim_spec(b"x".as_ptr(), 1), 0);
        assert_eq!(
            coblang_rt_unstr_step(
                dst4.as_mut_ptr(),
                4,
                a4.as_ptr(),
                a4.len(),
                dm.as_mut_ptr(),
                1
            ),
            -2
        );
        assert_eq!(coblang_rt_unstr_set_delim_spec(a1.as_ptr(), a1.len()), 0);
        assert_eq!(
            coblang_rt_unstr_step(nmp, 4, a4.as_ptr(), a4.len(), nmp, 0),
            -2
        );
        assert_eq!(
            coblang_rt_unstr_step(dst4.as_mut_ptr(), 4, np, 4, nmp, 0),
            -2
        );
        assert_eq!(
            coblang_rt_unstr_step(dst4.as_mut_ptr(), 4, b"x".as_ptr(), 1, nmp, 0),
            -2
        );
        assert_eq!(
            coblang_rt_unstr_step(dst4.as_mut_ptr(), 4, a4.as_ptr(), a4.len(), nmp, 1),
            -2
        );
        assert_eq!(
            coblang_rt_unstr_step(
                dst4.as_mut_ptr(),
                4,
                a4.as_ptr(),
                a4.len(),
                dm.as_mut_ptr(),
                1
            ),
            1
        );
        assert_eq!(coblang_rt_unstr_finish(), 1);
        assert_eq!(coblang_rt_unstr_tally_delta(), 1);

        assert_eq!(coblang_rt_unstr_begin(b"Z".as_ptr(), 1, np, 0, 1), 0);
        assert_eq!(
            coblang_rt_unstr_step(dst4.as_mut_ptr(), 4, a4.as_ptr(), a4.len(), nmp, 0),
            1
        );
        assert_eq!(
            coblang_rt_unstr_step(
                dst4.as_mut_ptr(),
                4,
                a4.as_ptr(),
                a4.len(),
                dm.as_mut_ptr(),
                0
            ),
            -1
        );
        assert_eq!(coblang_rt_unstr_finish(), 0);
    }
}

/// Verifies M10 `coblang_rt_insp_*` C-ABI entrypoints across valid, null-pointer, and malformed-spec paths.
#[test]
fn m10_inspect_abi_coverage() {
    use coblang_rt::{
        coblang_rt_insp_begin, coblang_rt_insp_clear_win, coblang_rt_insp_convert,
        coblang_rt_insp_exec, coblang_rt_insp_get_delta, coblang_rt_insp_push_replace,
        coblang_rt_insp_push_tally, coblang_rt_insp_set_op, coblang_rt_insp_set_win,
    };

    let _guard = ABI_STATE_LOCK.lock();
    let np: *const u8 = core::ptr::null();
    let nmp: *mut u8 = core::ptr::null_mut();

    #[expect(
        unsafe_code,
        reason = "Testing audited M10 `INSPECT` C-ABI entrypoints."
    )]
    // SAFETY: Non-null pointers reference stack buffers; null pointers test guard branches.
    unsafe {
        coblang_rt_insp_begin(1);
        coblang_rt_insp_clear_win();
        assert_eq!(coblang_rt_insp_set_win(0, 0, np, 1, np, 0), -1);
        assert_eq!(coblang_rt_insp_set_win(0, 0, b"A".as_ptr(), 1, np, 1), -1);
        assert_eq!(
            coblang_rt_insp_set_win(0, 0, b"A".as_ptr(), 1, b"x".as_ptr(), 1),
            -1
        );
        assert_eq!(coblang_rt_insp_set_win(0, 0, b"A".as_ptr(), 1, np, 0), 0);
        assert_eq!(coblang_rt_insp_set_win(1, 0, b"Z".as_ptr(), 1, np, 0), 0);

        assert_eq!(coblang_rt_insp_set_op(0, 0, np, 1, np, 0), -1);
        assert_eq!(coblang_rt_insp_set_op(0, 0, b"B".as_ptr(), 1, np, 1), -1);
        assert_eq!(
            coblang_rt_insp_set_op(0, 0, b"B".as_ptr(), 1, b"x".as_ptr(), 1),
            -1
        );
        assert_eq!(coblang_rt_insp_set_op(0, 0, b"B".as_ptr(), 1, np, 0), 0);
        assert_eq!(coblang_rt_insp_set_op(1, 1, b"-".as_ptr(), 1, np, 0), 0);

        coblang_rt_insp_push_tally(0, 1);
        coblang_rt_insp_push_replace(1);
        let mut t = *b"ABBZ";
        assert_eq!(coblang_rt_insp_exec(nmp, 4, np, 0), -1);
        assert_eq!(coblang_rt_insp_exec(t.as_mut_ptr(), 4, np, 1), -1);
        assert_eq!(
            coblang_rt_insp_exec(t.as_mut_ptr(), 4, b"x".as_ptr(), 1),
            -1
        );
        assert_eq!(coblang_rt_insp_exec(t.as_mut_ptr(), 4, np, 0), 0);
        assert_eq!((&t, coblang_rt_insp_get_delta(0)), (b"A--Z", 2));

        assert_eq!(coblang_rt_insp_set_op(0, 0, b"-".as_ptr(), 1, np, 0), 0);
        assert_eq!(coblang_rt_insp_set_op(1, 0, b"+".as_ptr(), 1, np, 0), 0);
        assert_eq!(coblang_rt_insp_convert(nmp, 4, np, 0), -1);
        assert_eq!(coblang_rt_insp_convert(t.as_mut_ptr(), 4, np, 1), -1);
        assert_eq!(
            coblang_rt_insp_convert(t.as_mut_ptr(), 4, b"x".as_ptr(), 1),
            -1
        );
        assert_eq!(coblang_rt_insp_convert(t.as_mut_ptr(), 4, np, 0), 0);
        assert_eq!(&t, b"A++Z");
    }
}

/// Verifies all M11 heap, pattern-fill, `BASED` check, `ENTRY` lookup, and `EXTERNAL` segment C-ABI entrypoints.
#[test]
fn m11_heap_based_entry_and_external_abi_coverage() {
    use coblang_rt::{
        cob_free, cob_malloc, coblang_rt_alloc, coblang_rt_alloc_fill_pattern,
        coblang_rt_check_based, coblang_rt_free, coblang_rt_resolve_entry,
        coblang_rt_resolve_external,
    };

    let _guard = ABI_STATE_LOCK.lock();
    let np = core::ptr::null::<u8>();
    let huge = (1_i64 << 40) + 1;

    #[expect(
        unsafe_code,
        reason = "Testing audited M11 C-ABI entrypoints across valid, null, and boundary inputs."
    )]
    // SAFETY: Non-null pointers come from `coblang_rt_alloc`/`cob_malloc` or stack slices; null pointers test boundary guards.
    unsafe {
        cob_tidy();
        assert!(coblang_rt_alloc(huge, 0).is_null());
        let p1 = coblang_rt_alloc(20, i32::from(b'A'));
        assert!(!p1.is_null());
        assert_eq!(core::slice::from_raw_parts(p1, 20), &[b'A'; 20]);
        assert_eq!(coblang_rt_free(core::ptr::null_mut()), 0);
        let mut stack_byte = 0_u8;
        assert_eq!(coblang_rt_free(&raw mut stack_byte), 1);
        assert_eq!(coblang_rt_free(p1), 0);

        assert!(coblang_rt_alloc_fill_pattern(8, np, 2, 0).is_null());
        assert!(coblang_rt_alloc_fill_pattern(huge, b"XY".as_ptr(), 2, 0).is_null());
        let p_empty = coblang_rt_alloc_fill_pattern(4, np, 0, i32::from(b'.'));
        assert_eq!(core::slice::from_raw_parts(p_empty, 4), b"....");
        let p_pad = coblang_rt_alloc_fill_pattern(6, b"AB".as_ptr(), 2, i32::from(b' '));
        assert_eq!(core::slice::from_raw_parts(p_pad, 6), b"AB    ");
        let p_cyc = coblang_rt_alloc_fill_pattern(5, b"XY".as_ptr(), 2, -1);
        assert_eq!(core::slice::from_raw_parts(p_cyc, 5), b"XYXYX");

        let cm = cob_malloc(24);
        assert!(!cm.is_null());
        cob_free(cm);

        let non_null_probe = &raw const stack_byte;
        assert_eq!(
            coblang_rt_check_based(non_null_probe, np, 0, np, 0, np, 0),
            0
        );
        assert_eq!(coblang_rt_check_based(np, np, 1, np, 0, np, 0), -1);
        assert_eq!(
            coblang_rt_check_based(np, b"x:1".as_ptr(), 3, np, 1, np, 0),
            -1
        );
        assert_eq!(
            coblang_rt_check_based(np, b"x:1".as_ptr(), 3, b"R".as_ptr(), 1, np, 1),
            -1
        );
        // Note: `p_empty`, `p_pad`, and `p_cyc` are still live here, so `cob_tidy()` inside `check_based`
        // also exercises `free_all_tracked_heap_blocks` with live heap blocks!
        assert_eq!(
            coblang_rt_check_based(
                np,
                b"t.cob:9".as_ptr(),
                7,
                b"REC".as_ptr(),
                3,
                b"FLD".as_ptr(),
                3
            ),
            1
        );

        assert!(coblang_rt_resolve_entry(np, 1).is_null());
        assert!(coblang_rt_resolve_entry(b"no_such_entry_xyz".as_ptr(), 17).is_null());
        assert!(!coblang_rt_resolve_entry(b"getpid".as_ptr(), 6).is_null());

        assert!(coblang_rt_resolve_external(np, 1, 8, np, 0).is_null());
        assert!(coblang_rt_resolve_external(b"E1".as_ptr(), 2, 8, np, 1).is_null());
        assert!(coblang_rt_resolve_external(b"E1".as_ptr(), 2, huge, np, 0).is_null());
        let ext1 = coblang_rt_resolve_external(b"EXT_SEG".as_ptr(), 7, 6, b"HI".as_ptr(), 2);
        assert!(!ext1.is_null());
        assert_eq!(core::slice::from_raw_parts(ext1, 6), b"HI\0\0\0\0");
        let ext2 = coblang_rt_resolve_external(b"EXT_SEG".as_ptr(), 7, 6, b"ZZ".as_ptr(), 2);
        assert_eq!(ext1, ext2);
        cob_tidy();
    }
}
