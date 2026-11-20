//! Audited clean-room C-ABI entrypoints for Part 5 `MOVE`, `INITIALIZE`, `FixedDecimal` arithmetic,
//! relational comparisons, `IS NUMERIC`/`ALPHABETIC` class checks, and `DISPLAY`/`HEX-OF` staging.

use std::io::{self, Write as _};

use coblang_common::{CoblangError, Result};
use coblang_decimal::HighOrderPolicy;
use coblang_rt_core::{
    LineAdvancing, OutputDevice, SwitchState,
    desc_codec::decode_data_category,
    field_ops::{
        AlphaCmpOperand, BoundCheckDisposition, ClassTestKind, DecimalArithOp, NegZeroPolicy,
        RefModLengthArg, RelCmpOp, RuntimeBoundFailureMode, RuntimeMoveSourceKind,
        evaluate_odo_bound, evaluate_refmod_bound, evaluate_subscript_bound,
        rounding_from_abi_code, runtime_check_class, runtime_check_custom_class,
        runtime_compare_alphanumeric, runtime_compare_alphanumeric_ex, runtime_execute_move,
        runtime_sort_table,
    },
    with_global_runtime,
};

/// Validates `(ptr, len)` as a shared byte slice (`len == 0` permits null `ptr`).
///
/// # Arguments
///
/// * `ptr` - Raw byte pointer.
/// * `len` - Byte count.
/// * `symbol` - Enclosing C-ABI symbol name.
/// * `param_name` - Parameter name for error diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::AbiNullOrInvalidPointer`] if `len > 0 && ptr.is_null()`.
///
/// # Safety
///
/// When `len > 0` and `!ptr.is_null()`, `ptr` must point to `len` initialized readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited shared-slice constructor at C-ABI boundary."
)]
pub unsafe fn read_raw_slice<'a>(
    ptr: *const u8,
    len: usize,
    symbol: &'static str,
    param_name: &'static str,
) -> Result<&'a [u8]> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() {
        return Err(CoblangError::AbiNullOrInvalidPointer { symbol, param_name });
    }
    // SAFETY: Caller guarantees non-null `ptr` references `len` readable bytes.
    Ok(unsafe { core::slice::from_raw_parts(ptr, len) })
}

/// Validates `(ptr, len)` as an exclusive mutable byte slice (`len == 0` permits null `ptr`).
///
/// # Arguments
///
/// * `ptr` - Mutable raw byte pointer.
/// * `len` - Byte count.
/// * `symbol` - Enclosing C-ABI symbol name.
/// * `param_name` - Parameter name for error diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::AbiNullOrInvalidPointer`] if `len > 0 && ptr.is_null()`.
///
/// # Safety
///
/// When `len > 0` and `!ptr.is_null()`, `ptr` must point to `len` writable bytes with no active aliases.
#[expect(
    unsafe_code,
    reason = "Audited mutable-slice constructor at C-ABI boundary."
)]
pub unsafe fn write_raw_slice<'a>(
    ptr: *mut u8,
    len: usize,
    symbol: &'static str,
    param_name: &'static str,
) -> Result<&'a mut [u8]> {
    if len == 0 {
        return Ok(&mut []);
    }
    if ptr.is_null() {
        return Err(CoblangError::AbiNullOrInvalidPointer { symbol, param_name });
    }
    // SAFETY: Caller guarantees non-null `ptr` exclusively references `len` writable bytes.
    Ok(unsafe { core::slice::from_raw_parts_mut(ptr, len) })
}

/// Overlap-safe C-ABI `MOVE` entrypoint supporting all 28 COBOL `USAGE`s, `PICTURE` editing, and de-editing.
///
/// Returns `0` on success or `-1` on null-pointer / descriptor error.
///
/// # Arguments
///
/// * `src_kind` - [`RuntimeMoveSourceKind`] integer code (`0..=9`).
/// * `src_ptr` - Source payload bytes.
/// * `src_len` - Byte length of `src_ptr`.
/// * `src_spec_ptr` - Serialized source `RuntimeFieldSpec` (`null` when `src_spec_len == 0`).
/// * `src_spec_len` - Byte length of `src_spec_ptr`.
/// * `dst_ptr` - Mutable destination storage bytes (may alias `src_ptr`!).
/// * `dst_len` - Byte length of `dst_ptr`.
/// * `dst_spec_ptr` - Serialized destination `RuntimeFieldSpec`.
/// * `dst_spec_len` - Byte length of `dst_spec_ptr`.
///
/// # Safety
///
/// Every non-zero-length pointer/length pair must reference valid readable (or writable for `dst_ptr`) bytes.
#[expect(unsafe_code, reason = "Audited clean-room `MOVE` C-ABI entrypoint.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_move_desc(
    src_kind: i32,
    src_ptr: *const u8,
    src_len: usize,
    src_spec_ptr: *const u8,
    src_spec_len: usize,
    dst_ptr: *mut u8,
    dst_len: usize,
    dst_spec_ptr: *const u8,
    dst_spec_len: usize,
) -> i32 {
    // Snapshot `src` into owned `Vec<u8>` BEFORE creating `&mut` over `dst_ptr` so aliasing `REDEFINES` moves are UB-free.
    // SAFETY: Forwarded from caller preconditions; shared borrow ends immediately after `.to_vec()`.
    let Ok(src_owned) =
        (unsafe { read_raw_slice(src_ptr, src_len, "coblang_rt_move_desc", "src_ptr") })
            .map(<[u8]>::to_vec)
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions (`StaticConst` descriptor cannot alias `dst_ptr`).
    let Ok(src_spec) = (unsafe {
        read_raw_slice(
            src_spec_ptr,
            src_spec_len,
            "coblang_rt_move_desc",
            "src_spec_ptr",
        )
    }) else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions (`StaticConst` descriptor cannot alias `dst_ptr`).
    let Ok(dst_spec) = (unsafe {
        read_raw_slice(
            dst_spec_ptr,
            dst_spec_len,
            "coblang_rt_move_desc",
            "dst_spec_ptr",
        )
    }) else {
        return -1;
    };
    // SAFETY: Shared `src_ptr` borrow ended above; `dst_ptr` is now exclusively borrowed.
    let Ok(dst_slice) =
        (unsafe { write_raw_slice(dst_ptr, dst_len, "coblang_rt_move_desc", "dst_ptr") })
    else {
        return -1;
    };
    let kind = RuntimeMoveSourceKind::from_abi_code(src_kind);
    if runtime_execute_move(kind, &src_owned, src_spec, dst_slice, dst_spec).is_ok() {
        0
    } else {
        -1
    }
}

/// Clears the run-unit `DISPLAY` staging buffer before appending operands.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `DISPLAY` begin C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_disp_begin() {
    with_global_runtime(|rt| rt.display_staging_mut().clear());
}

/// Appends raw literal or alphanumeric slice bytes to the active `DISPLAY` staging buffer.
///
/// # Arguments
///
/// * `ptr` - Raw byte buffer.
/// * `len` - Byte length of `ptr`.
///
/// # Safety
///
/// When `len > 0`, `ptr` must reference `len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `DISPLAY` raw-bytes C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_disp_push_raw(ptr: *const u8, len: usize) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(bytes) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_disp_push_raw", "ptr") }) else {
        return -1;
    };
    with_global_runtime(|rt| rt.display_staging_mut().push_raw(bytes));
    0
}

/// Formats a typed COBOL field and appends it to the active `DISPLAY` staging buffer.
///
/// # Arguments
///
/// * `ptr` - Stored field bytes.
/// * `len` - Byte length of `ptr`.
/// * `spec_ptr` - Serialized `RuntimeFieldSpec` bytes.
/// * `spec_len` - Byte length of `spec_ptr`.
///
/// # Safety
///
/// Non-zero-length `(ptr, len)` and `(spec_ptr, spec_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `DISPLAY` typed-field C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_disp_push_field(
    ptr: *const u8,
    len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(bytes) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_disp_push_field", "ptr") })
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(spec) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_disp_push_field", "spec_ptr") })
    else {
        return -1;
    };
    let ok = with_global_runtime(|rt| rt.display_staging_mut().push_field(bytes, spec).is_ok());
    if ok { 0 } else { -1 }
}

/// Appends uppercase `FUNCTION HEX-OF(...)` hex digits of `(ptr, len)` to the `DISPLAY` staging buffer.
///
/// # Arguments
///
/// * `ptr` - Raw byte buffer.
/// * `len` - Byte length of `ptr`.
///
/// # Safety
///
/// When `len > 0`, `ptr` must reference `len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `DISPLAY HEX-OF` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_disp_push_hex(ptr: *const u8, len: usize) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(bytes) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_disp_push_hex", "ptr") }) else {
        return -1;
    };
    with_global_runtime(|rt| rt.display_staging_mut().push_hex_of(bytes));
    0
}

/// Flushes the run-unit `DISPLAY` staging buffer into injectable `stdout`/`stderr` writers.
///
/// Returns `0` on success or `-1` on I/O failure.
///
/// # Arguments
///
/// * `device` - `0` for `Stdout`, non-zero for `Stderr`.
/// * `advancing` - `0` for `NoAdvancing`, non-zero for `AdvanceLine`.
/// * `stdout_out` - Standard output stream implementation.
/// * `stderr_out` - Standard error stream implementation.
pub fn flush_staged_display_to(
    device: i32,
    advancing: i32,
    stdout_out: &mut dyn io::Write,
    stderr_out: &mut dyn io::Write,
) -> i32 {
    let adv = LineAdvancing::from_abi_code(advancing);
    let res = match OutputDevice::from_abi_code(device) {
        OutputDevice::Stdout => {
            with_global_runtime(|rt| rt.display_staging_mut().flush_to(adv, stdout_out))
        }
        OutputDevice::Stderr => {
            with_global_runtime(|rt| rt.display_staging_mut().flush_to(adv, stderr_out))
        }
    };
    if res.is_ok() { 0 } else { -1 }
}

/// Flushes staged `DISPLAY` output to `stdout` (`device == 0`) or `stderr` (`device != 0`).
///
/// # Arguments
///
/// * `device` - `0` for `Stdout`, non-zero for `Stderr`.
/// * `advancing` - `0` for `NoAdvancing`, non-zero for `AdvanceLine`.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `DISPLAY` flush C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_disp_flush(device: i32, advancing: i32) -> i32 {
    let mut out = io::stdout();
    let mut err = io::stderr();
    flush_staged_display_to(device, advancing, &mut out, &mut err)
}

/// Resets the run-unit 38/76-digit `DecimalEvalStack`.
#[expect(
    unsafe_code,
    reason = "Audited clean-room decimal-stack reset C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_dec_begin() {
    with_global_runtime(|rt| rt.decimal_stack_mut().begin());
}

/// Decodes a numeric COBOL field and pushes it onto the run-unit `DecimalEvalStack`.
///
/// # Arguments
///
/// * `ptr` - Stored field bytes.
/// * `len` - Byte length of `ptr`.
/// * `spec_ptr` - Serialized `RuntimeFieldSpec` bytes.
/// * `spec_len` - Byte length of `spec_ptr`.
///
/// # Safety
///
/// Non-zero-length `(ptr, len)` and `(spec_ptr, spec_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room decimal field push C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_dec_push_field(
    ptr: *const u8,
    len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(bytes) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_dec_push_field", "ptr") })
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(spec) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_dec_push_field", "spec_ptr") })
    else {
        return -1;
    };
    let ok = with_global_runtime(|rt| rt.decimal_stack_mut().push_field(bytes, spec).is_ok());
    if ok { 0 } else { -1 }
}

/// Parses an ASCII decimal literal and pushes it onto the run-unit `DecimalEvalStack`.
///
/// # Arguments
///
/// * `ptr` - ASCII decimal bytes (`[+|-]digits[.digits]`).
/// * `len` - Byte length of `ptr`.
///
/// # Safety
///
/// When `len > 0`, `ptr` must reference `len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room decimal ASCII constant push C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_dec_push_ascii(ptr: *const u8, len: usize) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(bytes) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_dec_push_ascii", "ptr") })
    else {
        return -1;
    };
    let ok = with_global_runtime(|rt| rt.decimal_stack_mut().push_ascii(bytes).is_ok());
    if ok { 0 } else { -1 }
}

/// Negates the top value on the run-unit `DecimalEvalStack`.
#[expect(
    unsafe_code,
    reason = "Audited clean-room decimal unary-negate C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_dec_neg() {
    with_global_runtime(|rt| rt.decimal_stack_mut().negate_top());
}

/// Applies a binary arithmetic operator (`0=Add`, `1=Sub`, `2=Mul`, `3=Div`, `4=Pow`) on the `DecimalEvalStack`.
///
/// # Arguments
///
/// * `op` - [`DecimalArithOp`] integer opcode.
#[expect(
    unsafe_code,
    reason = "Audited clean-room decimal binary-op C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_dec_binop(op: i32) -> i32 {
    let dop = DecimalArithOp::from_abi_code(op);
    let ok = with_global_runtime(|rt| rt.decimal_stack_mut().apply_binop(dop).is_ok());
    if ok { 0 } else { -1 }
}

/// Peeks the top `FixedDecimal` and stores it into `(dst_ptr, dst_len)` with rounding and size-error policy.
///
/// Returns `0` on exact/truncated success, `1` on `SIZE ERROR`, or `-1` on pointer/codec failure.
///
/// # Arguments
///
/// * `dst_ptr` - Mutable destination field bytes.
/// * `dst_len` - Byte length of `dst_ptr`.
/// * `dst_spec_ptr` - Serialized destination `RuntimeFieldSpec`.
/// * `dst_spec_len` - Byte length of `dst_spec_ptr`.
/// * `rounding` - ISO 2023 `RoundingMode` code (`0..=7`).
/// * `flag_overflow` - Non-zero when `ON SIZE ERROR` / `NOT ON SIZE ERROR` is attached (leaves `dst` untouched on overflow).
/// * `neg_zero` - Non-zero to preserve existing `-0` (`'p'`) on unrounded zero store.
///
/// # Safety
///
/// Non-zero `(dst_ptr, dst_len)` must be exclusively writable and `(dst_spec_ptr, dst_spec_len)` readable.
#[expect(
    unsafe_code,
    reason = "Audited clean-room decimal target-store C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_dec_store(
    dst_ptr: *mut u8,
    dst_len: usize,
    dst_spec_ptr: *const u8,
    dst_spec_len: usize,
    rounding: i32,
    flag_overflow: i32,
    neg_zero: i32,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(spec) =
        (unsafe { read_raw_slice(dst_spec_ptr, dst_spec_len, "coblang_rt_dec_store", "spec") })
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(dst) = (unsafe { write_raw_slice(dst_ptr, dst_len, "coblang_rt_dec_store", "dst_ptr") })
    else {
        return -1;
    };
    let mode = rounding_from_abi_code(rounding);
    let ec_size_overflow_enabled = with_global_runtime(|rt| {
        rt.exceptions()
            .is_enabled(coblang_rt_core::exceptions::ExceptionCode::EcSizeOverflow)
    });
    let ho = if flag_overflow != 0 || ec_size_overflow_enabled {
        HighOrderPolicy::FlagOverflow
    } else {
        HighOrderPolicy::ModuloTruncate
    };
    let nz = NegZeroPolicy::from_abi_code(neg_zero);
    let res = with_global_runtime(|rt| rt.decimal_stack_mut().store_top(dst, spec, mode, ho, nz));
    match res {
        Ok(st) => {
            if let Some(ec) = map_size_error_to_exception_code(st) {
                crate::m13_abi::note_runtime_exception(ec, "", "", flag_overflow == 0);
            }
            i32::from(st.is_error())
        }
        Err(_) => -1,
    }
}

/// Maps a `coblang_decimal::SizeErrorStatus` to its corresponding ISO 2023 `EC-SIZE-*` code.
///
/// # Arguments
///
/// * `st` - Evaluated [`coblang_decimal::SizeErrorStatus`].
#[must_use]
fn map_size_error_to_exception_code(
    st: coblang_decimal::SizeErrorStatus,
) -> Option<coblang_rt_core::exceptions::ExceptionCode> {
    use coblang_decimal::SizeErrorStatus;
    use coblang_rt_core::exceptions::ExceptionCode;
    match st {
        SizeErrorStatus::Exact => None,
        SizeErrorStatus::DivisionByZero => Some(ExceptionCode::EcSizeZeroDivide),
        SizeErrorStatus::InvalidExponent => Some(ExceptionCode::EcSizeExponentiation),
        SizeErrorStatus::Overflowed => Some(ExceptionCode::EcSizeOverflow),
    }
}

/// Pops `(dividend, divisor)` and executes `DIVIDE ... GIVING q [ROUNDED] REMAINDER r`.
///
/// Returns `0` on success, `1` on `SIZE ERROR`, or `-1` on pointer/codec failure.
///
/// # Arguments
///
/// * `q_ptr` - Quotient storage pointer (may alias `r_ptr` safely!).
/// * `q_len` - Byte length of `q_ptr`.
/// * `q_spec_ptr` - Serialized quotient `RuntimeFieldSpec`.
/// * `q_spec_len` - Byte length of `q_spec_ptr`.
/// * `q_rounding` - Quotient `RoundingMode` code (`0..=7`).
/// * `r_ptr` - Remainder storage pointer.
/// * `r_len` - Byte length of `r_ptr`.
/// * `r_spec_ptr` - Serialized remainder `RuntimeFieldSpec`.
/// * `r_spec_len` - Byte length of `r_spec_ptr`.
/// * `flag_overflow` - Non-zero when `ON SIZE ERROR` / `NOT ON SIZE ERROR` is attached.
///
/// # Safety
///
/// All non-zero-length pointer/length pairs must reference valid memory (`q_ptr` and `r_ptr` writable).
#[expect(
    unsafe_code,
    reason = "Audited clean-room `DIVIDE ... REMAINDER` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_dec_div_rem(
    q_ptr: *mut u8,
    q_len: usize,
    q_spec_ptr: *const u8,
    q_spec_len: usize,
    q_rounding: i32,
    r_ptr: *mut u8,
    r_len: usize,
    r_spec_ptr: *const u8,
    r_spec_len: usize,
    flag_overflow: i32,
) -> i32 {
    // Snapshot initial `q` and `r` bytes before mutating so even aliasing `q_ptr == r_ptr` is 100% sound.
    // SAFETY: Shared reads end immediately via `.to_vec()`.
    let Ok(q_init) =
        (unsafe { read_raw_slice(q_ptr.cast_const(), q_len, "coblang_rt_dec_div_rem", "q_ptr") })
            .map(<[u8]>::to_vec)
    else {
        return -1;
    };
    // SAFETY: Shared reads end immediately via `.to_vec()`.
    let Ok(r_init) =
        (unsafe { read_raw_slice(r_ptr.cast_const(), r_len, "coblang_rt_dec_div_rem", "r_ptr") })
            .map(<[u8]>::to_vec)
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(q_spec) = (unsafe {
        read_raw_slice(
            q_spec_ptr,
            q_spec_len,
            "coblang_rt_dec_div_rem",
            "q_spec_ptr",
        )
    }) else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(r_spec) = (unsafe {
        read_raw_slice(
            r_spec_ptr,
            r_spec_len,
            "coblang_rt_dec_div_rem",
            "r_spec_ptr",
        )
    }) else {
        return -1;
    };
    let ec_size_overflow_enabled = with_global_runtime(|rt| {
        rt.exceptions()
            .is_enabled(coblang_rt_core::exceptions::ExceptionCode::EcSizeOverflow)
    });
    let ho = if flag_overflow != 0 || ec_size_overflow_enabled {
        HighOrderPolicy::FlagOverflow
    } else {
        HighOrderPolicy::ModuloTruncate
    };
    let q_mode = rounding_from_abi_code(q_rounding);
    let computed = with_global_runtime(|rt| {
        rt.decimal_stack_mut()
            .divide_remainder((&q_init, q_spec, q_mode), (&r_init, r_spec), ho)
    });
    let Ok(outcome) = computed else {
        return -1;
    };
    if let Some(qb) = outcome.quotient_bytes
        && !qb.is_empty()
    {
        // SAFETY: `q_ptr` was validated non-null for `q_len == qb.len()` above; sequential copy avoids aliasing UB.
        unsafe {
            core::ptr::copy_nonoverlapping(qb.as_ptr(), q_ptr, qb.len());
        }
    }
    if let Some(rb) = outcome.remainder_bytes
        && !rb.is_empty()
    {
        // SAFETY: `r_ptr` was validated non-null for `r_len == rb.len()` above; sequential copy avoids aliasing UB.
        unsafe {
            core::ptr::copy_nonoverlapping(rb.as_ptr(), r_ptr, rb.len());
        }
    }
    if let Some(ec) = map_size_error_to_exception_code(outcome.status) {
        crate::m13_abi::note_runtime_exception(ec, "DIVIDE", "", flag_overflow == 0);
    }
    i32::from(outcome.status.is_error())
}

/// Pops `(lhs, rhs)` from the `DecimalEvalStack` and returns `1` if `lhs <rel> rhs`, `0` if false, or `-1` on underflow.
///
/// # Arguments
///
/// * `rel` - [`RelCmpOp`] integer code (`0..=5`).
#[expect(
    unsafe_code,
    reason = "Audited clean-room decimal relational-compare C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_dec_cmp(rel: i32) -> i32 {
    let rop = RelCmpOp::from_abi_code(rel);
    match with_global_runtime(|rt| rt.decimal_stack_mut().compare_top_two(rop)) {
        Ok(matched) => i32::from(matched),
        Err(_) => -1,
    }
}

/// Compares two non-numeric / group / figurative operands with ISO COBOL right-space padding.
///
/// Returns `1` when `lhs <rel> rhs` holds, `0` when false, or `-1` on null pointer.
///
/// # Arguments
///
/// * `lhs_kind` - Left [`RuntimeMoveSourceKind`] code.
/// * `lhs_ptr` - Left byte buffer.
/// * `lhs_len` - Left byte length.
/// * `rhs_kind` - Right [`RuntimeMoveSourceKind`] code.
/// * `rhs_ptr` - Right byte buffer.
/// * `rhs_len` - Right byte length.
/// * `rel` - [`RelCmpOp`] code (`0..=5`).
///
/// # Safety
///
/// Non-zero `(lhs_ptr, lhs_len)` and `(rhs_ptr, rhs_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room alphanumeric comparison C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_cmp_alpha(
    lhs_kind: i32,
    lhs_ptr: *const u8,
    lhs_len: usize,
    rhs_kind: i32,
    rhs_ptr: *const u8,
    rhs_len: usize,
    rel: i32,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(lb) = (unsafe { read_raw_slice(lhs_ptr, lhs_len, "coblang_rt_cmp_alpha", "lhs_ptr") })
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(rb) = (unsafe { read_raw_slice(rhs_ptr, rhs_len, "coblang_rt_cmp_alpha", "rhs_ptr") })
    else {
        return -1;
    };
    let lk = RuntimeMoveSourceKind::from_abi_code(lhs_kind);
    let rk = RuntimeMoveSourceKind::from_abi_code(rhs_kind);
    i32::from(runtime_compare_alphanumeric(
        (lk, lb),
        (rk, rb),
        RelCmpOp::from_abi_code(rel),
    ))
}

/// Evaluates `IS NUMERIC` / `IS ALPHABETIC*` on `(ptr, len)` using `(spec_ptr, spec_len)`.
///
/// Returns `1` when true, `0` when false, or `-1` on invalid pointer/descriptor.
///
/// # Arguments
///
/// * `ptr` - Stored field bytes.
/// * `len` - Byte length of `ptr`.
/// * `spec_ptr` - Serialized `RuntimeFieldSpec`.
/// * `spec_len` - Byte length of `spec_ptr`.
/// * `class_code` - [`ClassTestKind`] code (`0..=3`).
///
/// # Safety
///
/// Non-zero `(ptr, len)` and `(spec_ptr, spec_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `IS NUMERIC` / class-check C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_class_check(
    ptr: *const u8,
    len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
    class_code: i32,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(bytes) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_class_check", "ptr") }) else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(spec) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_class_check", "spec_ptr") })
    else {
        return -1;
    };
    match runtime_check_class(bytes, spec, ClassTestKind::from_abi_code(class_code)) {
        Ok(ok) => i32::from(ok),
        Err(_) => -1,
    }
}

/// Clears staged `INITIALIZE ... REPLACING` rules.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `INITIALIZE` begin C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_init_begin() {
    with_global_runtime(|rt| rt.init_staging_mut().clear());
}

/// Stages one `INITIALIZE ... REPLACING <cat> BY <src>` rule.
///
/// # Arguments
///
/// * `cat_code` - Encoded `DataCategory` (`0..=12`).
/// * `kind` - [`RuntimeMoveSourceKind`] code (`0..=9`).
/// * `ptr` - Source payload bytes.
/// * `len` - Byte length of `ptr`.
/// * `spec_ptr` - Serialized source `RuntimeFieldSpec` (`null` when `spec_len == 0`).
/// * `spec_len` - Byte length of `spec_ptr`.
///
/// # Safety
///
/// Non-zero `(ptr, len)` and `(spec_ptr, spec_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `INITIALIZE REPLACING` staging C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_init_push_rep(
    cat_code: u8,
    kind: i32,
    ptr: *const u8,
    len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
) -> i32 {
    let Ok(cat) = decode_data_category(cat_code) else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(payload) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_init_push_rep", "ptr") })
        .map(<[u8]>::to_vec)
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(spec) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_init_push_rep", "spec_ptr") })
    else {
        return -1;
    };
    let sk = RuntimeMoveSourceKind::from_abi_code(kind);
    let ok = with_global_runtime(|rt| {
        rt.init_staging_mut()
            .push_replacement(cat, sk, payload, spec)
            .is_ok()
    });
    if ok { 0 } else { -1 }
}

/// Executes a serialized `INITIALIZE` plan over `(dst_ptr, dst_len)` and clears staged `REPLACING` rules.
///
/// # Arguments
///
/// * `dst_ptr` - Mutable target buffer.
/// * `dst_len` - Byte length of `dst_ptr`.
/// * `plan_ptr` - Serialized `DecodedInitPlan` bytes.
/// * `plan_len` - Byte length of `plan_ptr`.
///
/// # Safety
///
/// Non-zero `(dst_ptr, dst_len)` must be writable and `(plan_ptr, plan_len)` readable without aliasing.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `INITIALIZE` execution C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_init_exec(
    dst_ptr: *mut u8,
    dst_len: usize,
    plan_ptr: *const u8,
    plan_len: usize,
) -> i32 {
    // SAFETY: `plan_ptr` points to a read-only `StaticConst` descriptor separate from `dst_ptr`.
    let Ok(plan) =
        (unsafe { read_raw_slice(plan_ptr, plan_len, "coblang_rt_init_exec", "plan_ptr") })
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(dst) = (unsafe { write_raw_slice(dst_ptr, dst_len, "coblang_rt_init_exec", "dst_ptr") })
    else {
        return -1;
    };
    let ok = with_global_runtime(|rt| rt.init_staging_mut().execute_plan(dst, plan).is_ok());
    if ok { 0 } else { -1 }
}

/// Extended C-ABI alphanumeric / integer-vs-alphanumeric comparison supporting field descriptors and
/// optional 256-byte `SPECIAL-NAMES. ALPHABET` collating rank tables.
///
/// Returns `1` when `lhs <rel> rhs` holds, `0` when false, or `-1` on invalid pointer.
///
/// # Arguments
///
/// * `lhs_kind` - Left [`RuntimeMoveSourceKind`] code.
/// * `lhs_ptr` - Left byte buffer.
/// * `lhs_len` - Left byte length.
/// * `lhs_spec_ptr` - Left serialized `RuntimeFieldSpec` (`null` when `lhs_spec_len == 0`).
/// * `lhs_spec_len` - Left spec byte length.
/// * `rhs_kind` - Right [`RuntimeMoveSourceKind`] code.
/// * `rhs_ptr` - Right byte buffer.
/// * `rhs_len` - Right byte length.
/// * `rhs_spec_ptr` - Right serialized `RuntimeFieldSpec` (`null` when `rhs_spec_len == 0`).
/// * `rhs_spec_len` - Right spec byte length.
/// * `rel` - [`RelCmpOp`] code (`0..=5`).
/// * `coll_ptr` - Optional 256-byte collation rank table (`null` when `coll_len == 0`).
/// * `coll_len` - Byte length of `coll_ptr` (`0` or `256`).
///
/// # Safety
///
/// Every non-zero-length `(ptr, len)` pair must reference readable initialized bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room collated / integer-coercing alphanumeric comparison C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_cmp_alpha_ex(
    lhs_kind: i32,
    lhs_ptr: *const u8,
    lhs_len: usize,
    lhs_spec_ptr: *const u8,
    lhs_spec_len: usize,
    rhs_kind: i32,
    rhs_ptr: *const u8,
    rhs_len: usize,
    rhs_spec_ptr: *const u8,
    rhs_spec_len: usize,
    rel: i32,
    coll_ptr: *const u8,
    coll_len: usize,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(lb) =
        (unsafe { read_raw_slice(lhs_ptr, lhs_len, "coblang_rt_cmp_alpha_ex", "lhs_ptr") })
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(ls) = (unsafe {
        read_raw_slice(
            lhs_spec_ptr,
            lhs_spec_len,
            "coblang_rt_cmp_alpha_ex",
            "lhs_spec_ptr",
        )
    }) else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(rb) =
        (unsafe { read_raw_slice(rhs_ptr, rhs_len, "coblang_rt_cmp_alpha_ex", "rhs_ptr") })
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(rs) = (unsafe {
        read_raw_slice(
            rhs_spec_ptr,
            rhs_spec_len,
            "coblang_rt_cmp_alpha_ex",
            "rhs_spec_ptr",
        )
    }) else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(coll_raw) =
        (unsafe { read_raw_slice(coll_ptr, coll_len, "coblang_rt_cmp_alpha_ex", "coll_ptr") })
    else {
        return -1;
    };
    let ranks_arr = <[u8; 256]>::try_from(coll_raw).ok();
    let res = runtime_compare_alphanumeric_ex(
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::from_abi_code(lhs_kind),
            bytes: lb,
            spec_bytes: ls,
        },
        &AlphaCmpOperand {
            kind: RuntimeMoveSourceKind::from_abi_code(rhs_kind),
            bytes: rb,
            spec_bytes: rs,
        },
        RelCmpOp::from_abi_code(rel),
        ranks_arr.as_ref(),
    );
    i32::from(res)
}

/// Evaluates a custom `SPECIAL-NAMES. CLASS` 32-byte (`256`-bit) character bitmap over `(ptr, len)`.
///
/// Returns `1` when all bytes belong to the class, `0` when false, or `-1` on invalid pointer/length.
///
/// # Arguments
///
/// * `ptr` - Subject byte buffer.
/// * `len` - Subject byte length.
/// * `bitmap_ptr` - Non-null 32-byte (`256`-bit) class membership bitmap.
///
/// # Safety
///
/// `bitmap_ptr` must reference `32` readable bytes, and when `len > 0`, `ptr` must reference `len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room custom `CLASS` check C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_class_custom(
    ptr: *const u8,
    len: usize,
    bitmap_ptr: *const u8,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(bytes) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_class_custom", "ptr") }) else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(bm_raw) =
        (unsafe { read_raw_slice(bitmap_ptr, 32, "coblang_rt_class_custom", "bitmap_ptr") })
    else {
        return -1;
    };
    let mut bm = [0_u8; 32];
    bm.copy_from_slice(bm_raw);
    i32::from(runtime_check_custom_class(bytes, &bm))
}

/// Reads the current status (`1` = `ON`, `0` = `OFF`) of external `SWITCH-<idx>` (`0..=36`).
///
/// # Arguments
///
/// * `idx` - Zero-based switch index (`0..=36`).
#[expect(
    unsafe_code,
    reason = "Audited clean-room `SWITCH` getter C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_get_switch(idx: u32) -> i32 {
    let idx_u8 = u8::try_from(idx).unwrap_or(255);
    with_global_runtime(|rt| rt.get_switch(idx_u8).to_abi_code())
}

/// Updates external `SWITCH-<idx>` (`0..=36`) to `ON` (`state != 0`) or `OFF` (`state == 0`).
///
/// # Arguments
///
/// * `idx` - Zero-based switch index (`0..=36`).
/// * `state` - `0` for `OFF`, non-zero for `ON`.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `SWITCH` setter C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_set_switch(idx: u32, state: i32) -> i32 {
    let idx_u8 = u8::try_from(idx).unwrap_or(255);
    with_global_runtime(|rt| rt.set_switch(idx_u8, SwitchState::from_abi_code(state)));
    0
}

/// Applies [`BoundCheckDisposition`] to run-unit state, returning `1` when caller must `@exit(1)` and `0` otherwise.
///
/// # Arguments
///
/// * `disp` - Evaluated [`BoundCheckDisposition`].
fn finalize_bound_disposition(disp: BoundCheckDisposition) -> i32 {
    match disp {
        BoundCheckDisposition::Pass => 0,
        BoundCheckDisposition::RecordedNonFatal(ec) => {
            if let Ok(code) = coblang_rt_core::exceptions::ExceptionCode::from_cobol_name(ec) {
                crate::m13_abi::note_runtime_exception(code, "", "", true);
            } else {
                with_global_runtime(|rt| rt.record_exception(ec));
            }
            0
        }
        BoundCheckDisposition::FatalAbortExitOne(ec) => {
            with_global_runtime(|rt| {
                rt.record_exception(ec);
                rt.set_return_code(1);
            });
            crate::cob_tidy();
            1
        }
    }
}

/// Converts a C-ABI `is_fatal` integer (`0` vs non-zero) to [`RuntimeBoundFailureMode`], flushing `stdout` on fatal checks.
///
/// # Arguments
///
/// * `is_fatal` - `0` for non-fatal `RecordOnly`, non-zero for `FatalAbort`.
fn prepare_bound_mode(is_fatal: i32) -> RuntimeBoundFailureMode {
    if is_fatal == 0 {
        RuntimeBoundFailureMode::RecordOnly
    } else {
        let _ = io::stdout().flush();
        RuntimeBoundFailureMode::FatalAbort
    }
}

/// Validates an `OCCURS min TO max TIMES DEPENDING ON` object at runtime (`EC-BOUND-ODO`).
///
/// Returns `0` when valid or non-fatally recorded, `1` when fatal abort requires `@exit(1)`, or `-1` on invalid pointer.
///
/// # Arguments
///
/// * `odo_val` - Evaluated `DEPENDING ON` value.
/// * `min_val` - Declared minimum occurrence count.
/// * `max_val` - Declared maximum occurrence count.
/// * `is_fatal` - Non-zero when violation terminates the run unit with exit status `1`.
/// * `loc_ptr` - UTF-8 source location string bytes (or null if `loc_len == 0`).
/// * `loc_len` - Byte length of `loc_ptr`.
///
/// # Safety
///
/// When `loc_len > 0` and `!loc_ptr.is_null()`, `loc_ptr` must reference `loc_len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `EC-BOUND-ODO` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_check_odo(
    odo_val: i64,
    min_val: i64,
    max_val: i64,
    is_fatal: i32,
    loc_ptr: *const u8,
    loc_len: usize,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(loc_bytes) =
        (unsafe { read_raw_slice(loc_ptr, loc_len, "coblang_rt_check_odo", "loc_ptr") })
    else {
        return -1;
    };
    let site_text = String::from_utf8_lossy(loc_bytes);
    let mode = prepare_bound_mode(is_fatal);
    let disp = evaluate_odo_bound(
        odo_val,
        min_val,
        max_val,
        mode,
        &site_text,
        &mut io::stderr(),
    );
    finalize_bound_disposition(disp)
}

/// Validates a 1-based table subscript or index occurrence against `1..=max_val` (`EC-BOUND-SUBSCRIPT`).
///
/// Returns `0` when valid or non-fatally recorded, `1` when fatal abort requires `@exit(1)`, or `-1` on invalid pointer.
///
/// # Arguments
///
/// * `idx_val` - Evaluated 1-based subscript value.
/// * `max_val` - Active upper occurrence bound.
/// * `is_fatal` - Non-zero when violation terminates the run unit with exit status `1`.
/// * `loc_ptr` - UTF-8 source location string bytes (or null if `loc_len == 0`).
/// * `loc_len` - Byte length of `loc_ptr`.
///
/// # Safety
///
/// When `loc_len > 0` and `!loc_ptr.is_null()`, `loc_ptr` must reference `loc_len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `EC-BOUND-SUBSCRIPT` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_check_sub(
    idx_val: i64,
    max_val: i64,
    is_fatal: i32,
    loc_ptr: *const u8,
    loc_len: usize,
) -> i32 {
    let mut eff_fatal = is_fatal;
    if is_fatal < 0 {
        let dyn_en = with_global_runtime(|rt| {
            rt.exceptions()
                .is_enabled(coblang_rt_core::exceptions::ExceptionCode::EcBoundSubscript)
        });
        if !dyn_en {
            return 0;
        }
        if is_fatal == -1 {
            if idx_val < 1 || idx_val > max_val {
                crate::m13_abi::note_runtime_exception(
                    coblang_rt_core::exceptions::ExceptionCode::EcBoundSubscript,
                    "",
                    "",
                    true,
                );
                return 2;
            }
            return 0;
        }
        eff_fatal = 1;
    }
    // SAFETY: Forwarded from caller preconditions.
    let Ok(loc_bytes) =
        (unsafe { read_raw_slice(loc_ptr, loc_len, "coblang_rt_check_sub", "loc_ptr") })
    else {
        return -1;
    };
    let site_text = String::from_utf8_lossy(loc_bytes);
    let mode = prepare_bound_mode(eff_fatal);
    let disp = evaluate_subscript_bound(idx_val, max_val, mode, &site_text, &mut io::stderr());
    finalize_bound_disposition(disp)
}

/// Validates a 1-based `(offset : [length])` reference modification slice (`EC-BOUND-REF-MOD`).
///
/// Returns `0` when valid or non-fatally recorded, `1` when fatal abort requires `@exit(1)`, or `-1` on invalid pointer.
///
/// # Arguments
///
/// * `base_len` - Active item length in character units.
/// * `offset` - 1-based start offset.
/// * `has_len` - Non-zero when `length` was explicitly specified.
/// * `length` - Explicit length operand (ignored when `has_len == 0`).
/// * `is_fatal` - Non-zero when violation terminates the run unit with exit status `1`.
/// * `loc_ptr` - UTF-8 source location string bytes (or null if `loc_len == 0`).
/// * `loc_len` - Byte length of `loc_ptr`.
///
/// # Safety
///
/// When `loc_len > 0` and `!loc_ptr.is_null()`, `loc_ptr` must reference `loc_len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `EC-BOUND-REF-MOD` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_check_refmod(
    base_len: i64,
    offset: i64,
    has_len: i32,
    length: i64,
    is_fatal: i32,
    loc_ptr: *const u8,
    loc_len: usize,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(loc_bytes) =
        (unsafe { read_raw_slice(loc_ptr, loc_len, "coblang_rt_check_refmod", "loc_ptr") })
    else {
        return -1;
    };
    let site_text = String::from_utf8_lossy(loc_bytes);
    let len_arg = if has_len == 0 {
        RefModLengthArg::Omitted
    } else {
        RefModLengthArg::Explicit(length)
    };
    let mode = prepare_bound_mode(is_fatal);
    let disp = evaluate_refmod_bound(
        base_len,
        offset,
        len_arg,
        mode,
        &site_text,
        &mut io::stderr(),
    );
    finalize_bound_disposition(disp)
}

/// Executes an in-memory `SORT table` over `count` elements in `base_ptr[..buf_cap]`.
///
/// # Arguments
///
/// * `base_ptr` - Mutable pointer to element `1` of the target table.
/// * `buf_cap` - Allocated byte capacity of the table (`max_occurrences * stride`).
/// * `count` - Active occurrence count (`<= 0` treated as `0`).
/// * `plan_ptr` - Serialized [`coblang_rt_core::desc_codec::DecodedTableSortPlan`] bytes.
/// * `plan_len` - Byte length of `plan_ptr`.
///
/// # Safety
///
/// When `buf_cap > 0` and `!base_ptr.is_null()`, `base_ptr` must reference `buf_cap` mutable bytes.
/// When `plan_len > 0` and `!plan_ptr.is_null()`, `plan_ptr` must reference `plan_len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room in-memory `SORT table` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_sort_table(
    base_ptr: *mut u8,
    buf_cap: usize,
    count: i64,
    plan_ptr: *const u8,
    plan_len: usize,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(buf) =
        (unsafe { write_raw_slice(base_ptr, buf_cap, "coblang_rt_sort_table", "base_ptr") })
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(plan_bytes) =
        (unsafe { read_raw_slice(plan_ptr, plan_len, "coblang_rt_sort_table", "plan_ptr") })
    else {
        return -1;
    };
    let active_count = usize::try_from(count).unwrap_or(0);
    match runtime_sort_table(buf, active_count, plan_bytes) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}
