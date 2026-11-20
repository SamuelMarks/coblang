//! Audited clean-room C-ABI entrypoints for Milestone M10 (`STRING`, `UNSTRING`, `INSPECT`, `TRANSFORM`, `ACCEPT`, and `DISPLAY UPON ENVIRONMENT-*`).

use std::io;

use coblang_rt_core::{
    accept_env::{AcceptFlowOutcome, AcceptTargetDest, EnvDisplayTarget, RuntimeAcceptSource},
    m10_staging::{
        InspectRegSlot, InspectWindowEdge, StagedInspectOperand, StagedStringDelim,
        StringFlowOutcome, UnstringMatchMode, decode_inspect_match_kind,
    },
    with_global_runtime,
};

use crate::field_abi::{read_raw_slice, write_raw_slice};

/// Flushes staged `DISPLAY` bytes into `ENVIRONMENT-NAME` (`0`), `ENVIRONMENT-VALUE` (`1`),
/// `ARGUMENT-NUMBER` (`2`), or `COMMAND-LINE` (`3`).
///
/// # Arguments
///
/// * `target_code` - [`EnvDisplayTarget`] ABI integer code (`0..=3`).
#[expect(
    unsafe_code,
    reason = "Audited M10 `DISPLAY UPON ENVIRONMENT-*` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_disp_flush_env(target_code: i32) -> i32 {
    let target = EnvDisplayTarget::from_abi_code(target_code);
    with_global_runtime(|rt| rt.flush_display_to_env_target(target));
    0
}

/// Executes an `ACCEPT` statement (`0` = `Succeeded`, `1` = `Exception`, `-1` = invalid pointer/spec).
///
/// # Arguments
///
/// * `src_code` - [`RuntimeAcceptSource`] ABI integer code (`0..=13`).
/// * `is_omitted` - Non-zero when target is `OMITTED`, `0` when target is an addressable field.
/// * `dst_ptr` - Mutable destination field pointer (ignored when `is_omitted != 0`).
/// * `dst_len` - Destination byte length.
/// * `spec_ptr` - Serialized [`coblang_rt_core::desc_codec::RuntimeFieldSpec`] pointer.
/// * `spec_len` - Serialized field spec byte length.
///
/// # Safety
///
/// When `is_omitted == 0`, `(dst_ptr, dst_len)` must reference `dst_len` writable bytes and
/// `(spec_ptr, spec_len)` must reference `spec_len` readable bytes.
#[expect(unsafe_code, reason = "Audited M10 `ACCEPT` C-ABI entrypoint.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_accept(
    src_code: i32,
    is_omitted: i32,
    dst_ptr: *mut u8,
    dst_len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
) -> i32 {
    let source = RuntimeAcceptSource::from_abi_code(src_code);
    let stdin = io::stdin();
    let mut stdin_lock = stdin.lock();
    let mut stderr = io::stderr();
    let mut host_env = |k: &str| std::env::var(k).ok();
    if is_omitted != 0 {
        return with_global_runtime(|rt| {
            rt.execute_accept(
                source,
                AcceptTargetDest::Omitted,
                &mut stdin_lock,
                &mut stderr,
                &mut host_env,
            )
        })
        .map_or(-1, AcceptFlowOutcome::to_abi_code);
    }
    // SAFETY: Caller guarantees `(dst_ptr, dst_len)` and `(spec_ptr, spec_len)` when `is_omitted == 0`.
    let Ok(buf) = (unsafe { write_raw_slice(dst_ptr, dst_len, "coblang_rt_accept", "dst_ptr") })
    else {
        return -1;
    };
    // SAFETY: Caller guarantees `(spec_ptr, spec_len)` is readable.
    let Ok(spec_bytes) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_accept", "spec_ptr") })
    else {
        return -1;
    };
    with_global_runtime(|rt| {
        rt.execute_accept(
            source,
            AcceptTargetDest::Field { buf, spec_bytes },
            &mut stdin_lock,
            &mut stderr,
            &mut host_env,
        )
    })
    .map_or(-1, AcceptFlowOutcome::to_abi_code)
}

/// Resets `STRING` piece staging before pushing source pieces.
#[expect(unsafe_code, reason = "Audited M10 `STRING` begin C-ABI entrypoint.")]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_str_begin() {
    with_global_runtime(|rt| rt.m10_staging_mut().string_begin());
}

/// Sets the active `STRING` delimiter (`0` = `Size`, `1` = `Pattern`, `2` = `FigurativeByte`).
///
/// # Arguments
///
/// * `delim_kind` - Delimiter kind code (`0..=2`).
/// * `pat_ptr` - Pattern bytes pointer (`len == 0` allows null).
/// * `pat_len` - Pattern byte length.
/// * `spec_ptr` - Optional serialized field spec pointer (`spec_len == 0` allows null).
/// * `spec_len` - Serialized field spec byte length.
///
/// # Safety
///
/// Non-empty `(pat_ptr, pat_len)` and `(spec_ptr, spec_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited M10 `STRING` delimiter C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_str_set_delim(
    delim_kind: i32,
    pat_ptr: *const u8,
    pat_len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
) -> i32 {
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(pat) =
        (unsafe { read_raw_slice(pat_ptr, pat_len, "coblang_rt_str_set_delim", "pat_ptr") })
    else {
        return -1;
    };
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(spec) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_str_set_delim", "spec_ptr") })
    else {
        return -1;
    };
    let Ok(delim) = StagedStringDelim::from_raw(delim_kind, pat, Some(spec)) else {
        return -1;
    };
    with_global_runtime(|rt| rt.m10_staging_mut().string_set_delim(delim));
    0
}

/// Appends one source operand to the active `STRING` statement under the active delimiter.
///
/// # Arguments
///
/// * `src_ptr` - Source bytes pointer.
/// * `src_len` - Source byte length.
/// * `spec_ptr` - Optional serialized field spec pointer (`spec_len == 0` for literals).
/// * `spec_len` - Serialized field spec byte length.
///
/// # Safety
///
/// Non-empty `(src_ptr, src_len)` and `(spec_ptr, spec_len)` must reference readable bytes.
#[expect(unsafe_code, reason = "Audited M10 `STRING` source C-ABI entrypoint.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_str_push_src(
    src_ptr: *const u8,
    src_len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
) -> i32 {
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(src) =
        (unsafe { read_raw_slice(src_ptr, src_len, "coblang_rt_str_push_src", "src_ptr") })
    else {
        return -1;
    };
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(spec) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_str_push_src", "spec_ptr") })
    else {
        return -1;
    };
    with_global_runtime(|rt| rt.m10_staging_mut().string_push_src(src, Some(spec)))
        .map_or(-1, |()| 0)
}

/// Executes the staged `STRING` into `(dst_ptr, dst_len)` starting at 1-based `initial_ptr`.
///
/// Returns `0` (`Completed`), `1` (`Overflowed`), or `-1` on invalid destination pointer.
///
/// # Arguments
///
/// * `dst_ptr` - Mutable `INTO` buffer pointer.
/// * `dst_len` - `INTO` byte length.
/// * `initial_ptr` - Initial 1-based `POINTER` value (`1` when omitted).
///
/// # Safety
///
/// When `dst_len > 0`, `dst_ptr` must reference `dst_len` writable bytes.
#[expect(unsafe_code, reason = "Audited M10 `STRING` execute C-ABI entrypoint.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_str_exec(
    dst_ptr: *mut u8,
    dst_len: usize,
    initial_ptr: i64,
) -> i32 {
    // SAFETY: Validated via `write_raw_slice`.
    let Ok(dst) = (unsafe { write_raw_slice(dst_ptr, dst_len, "coblang_rt_str_exec", "dst_ptr") })
    else {
        return -1;
    };
    let outcome = with_global_runtime(|rt| rt.m10_staging_mut().string_exec(dst, initial_ptr));
    match outcome {
        StringFlowOutcome::Completed => 0,
        StringFlowOutcome::Overflowed => 1,
    }
}

/// Reads the final 1-based `POINTER` value produced by the most recent `STRING` or `UNSTRING`.
#[expect(
    unsafe_code,
    reason = "Audited M10 `STRING`/`UNSTRING` pointer query C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_str_last_ptr() -> i64 {
    with_global_runtime(|rt| rt.m10_staging_mut().last_pointer())
}

/// Initializes an `UNSTRING` statement with `(src_ptr, src_len)` and 1-based `initial_ptr`.
///
/// # Arguments
///
/// * `src_ptr` - Source field bytes pointer.
/// * `src_len` - Source field byte length.
/// * `spec_ptr` - Optional serialized source field spec pointer.
/// * `spec_len` - Serialized source field spec byte length.
/// * `initial_ptr` - Initial 1-based `POINTER` value (`1` when omitted).
///
/// # Safety
///
/// Non-empty `(src_ptr, src_len)` and `(spec_ptr, spec_len)` must reference readable bytes.
#[expect(unsafe_code, reason = "Audited M10 `UNSTRING` begin C-ABI entrypoint.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_unstr_begin(
    src_ptr: *const u8,
    src_len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
    initial_ptr: i64,
) -> i32 {
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(src) =
        (unsafe { read_raw_slice(src_ptr, src_len, "coblang_rt_unstr_begin", "src_ptr") })
    else {
        return -1;
    };
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(spec) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_unstr_begin", "spec_ptr") })
    else {
        return -1;
    };
    with_global_runtime(|rt| {
        rt.m10_staging_mut()
            .unstring_begin(src, Some(spec), initial_ptr)
    })
    .map_or(-1, |()| 0)
}

/// Appends one delimiter pattern to the active `UNSTRING` statement.
///
/// # Arguments
///
/// * `is_all` - Non-zero for `DELIMITED BY ALL`, `0` for single delimiter match.
/// * `pat_ptr` - Delimiter pattern bytes pointer.
/// * `pat_len` - Delimiter pattern byte length.
/// * `spec_ptr` - Optional serialized field spec pointer.
/// * `spec_len` - Serialized field spec length.
///
/// # Safety
///
/// Non-empty `(pat_ptr, pat_len)` and `(spec_ptr, spec_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited M10 `UNSTRING` delimiter C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_unstr_push_delim(
    is_all: i32,
    pat_ptr: *const u8,
    pat_len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
) -> i32 {
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(pat) =
        (unsafe { read_raw_slice(pat_ptr, pat_len, "coblang_rt_unstr_push_delim", "pat_ptr") })
    else {
        return -1;
    };
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(spec) = (unsafe {
        read_raw_slice(
            spec_ptr,
            spec_len,
            "coblang_rt_unstr_push_delim",
            "spec_ptr",
        )
    }) else {
        return -1;
    };
    let mode = if is_all == 0 {
        UnstringMatchMode::Single
    } else {
        UnstringMatchMode::AllRepeated
    };
    with_global_runtime(|rt| {
        rt.m10_staging_mut()
            .unstring_push_delim(mode, pat, Some(spec))
    })
    .map_or(-1, |()| 0)
}

/// Stages serialized field spec metadata for the next `UNSTRING` `DELIMITER IN` slot.
///
/// # Arguments
///
/// * `spec_ptr` - Serialized [`coblang_rt_core::desc_codec::RuntimeFieldSpec`] pointer.
/// * `spec_len` - Serialized field spec byte length.
///
/// # Safety
///
/// When `spec_len > 0`, `spec_ptr` must reference `spec_len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited M10 `UNSTRING` `DELIMITER IN` spec C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_unstr_set_delim_spec(
    spec_ptr: *const u8,
    spec_len: usize,
) -> i32 {
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(spec) = (unsafe {
        read_raw_slice(
            spec_ptr,
            spec_len,
            "coblang_rt_unstr_set_delim_spec",
            "spec_ptr",
        )
    }) else {
        return -1;
    };
    with_global_runtime(|rt| rt.m10_staging_mut().unstring_set_delim_in_spec(spec));
    0
}

/// Advances the active `UNSTRING` into one `INTO` slot.
///
/// Returns `>= 0` (`COUNT IN` character count) when the slot was acted upon, `-1` when skipped
/// (source already exhausted / initial pointer overflow), or `-2` on pointer/descriptor error.
///
/// # Arguments
///
/// * `into_ptr` - Mutable `INTO` field pointer.
/// * `into_len` - `INTO` field byte length.
/// * `into_spec_ptr` - Serialized `INTO` field spec pointer.
/// * `into_spec_len` - Serialized `INTO` field spec byte length.
/// * `delim_ptr` - Optional mutable `DELIMITER IN` buffer pointer (null when omitted).
/// * `delim_len` - `DELIMITER IN` byte length.
///
/// # Safety
///
/// `(into_ptr, into_len)`, `(into_spec_ptr, into_spec_len)`, and non-null `(delim_ptr, delim_len)`
/// must reference valid memory regions.
#[expect(unsafe_code, reason = "Audited M10 `UNSTRING` step C-ABI entrypoint.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_unstr_step(
    into_ptr: *mut u8,
    into_len: usize,
    into_spec_ptr: *const u8,
    into_spec_len: usize,
    delim_ptr: *mut u8,
    delim_len: usize,
) -> i64 {
    // SAFETY: Validated via `write_raw_slice`.
    let Ok(into_buf) =
        (unsafe { write_raw_slice(into_ptr, into_len, "coblang_rt_unstr_step", "into_ptr") })
    else {
        return -2;
    };
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(into_spec) = (unsafe {
        read_raw_slice(
            into_spec_ptr,
            into_spec_len,
            "coblang_rt_unstr_step",
            "into_spec_ptr",
        )
    }) else {
        return -2;
    };
    let delim_opt = if delim_len == 0 && delim_ptr.is_null() {
        None
    } else {
        // SAFETY: Validated via `write_raw_slice` (`delim_len > 0 && delim_ptr.is_null()` -> `-2`).
        let Ok(dbuf) = (unsafe {
            write_raw_slice(delim_ptr, delim_len, "coblang_rt_unstr_step", "delim_ptr")
        }) else {
            return -2;
        };
        Some(dbuf)
    };
    match with_global_runtime(|rt| {
        rt.m10_staging_mut()
            .unstring_step_into(into_buf, into_spec, delim_opt)
    }) {
        Ok(Some(cnt)) => cnt,
        Ok(None) => -1,
        Err(_) => -2,
    }
}

/// Finishes the active `UNSTRING`, returning `0` (`Completed`) or `1` (`Overflowed`).
#[expect(
    unsafe_code,
    reason = "Audited M10 `UNSTRING` finish C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_unstr_finish() -> i32 {
    match with_global_runtime(|rt| rt.m10_staging_mut().unstring_finish()) {
        StringFlowOutcome::Completed => 0,
        StringFlowOutcome::Overflowed => 1,
    }
}

/// Reads the `TALLYING` increment accumulated during the most recent `UNSTRING`.
#[expect(
    unsafe_code,
    reason = "Audited M10 `UNSTRING` tally delta query C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_unstr_tally_delta() -> i64 {
    with_global_runtime(|rt| rt.m10_staging_mut().last_unstring_tally_delta())
}

/// Resets `INSPECT` staging state for `counter_count` `TALLYING` counter slots.
///
/// # Arguments
///
/// * `counter_count` - Number of distinct `TALLYING` counters (`>= 0`).
#[expect(unsafe_code, reason = "Audited M10 `INSPECT` begin C-ABI entrypoint.")]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_insp_begin(counter_count: i64) {
    let n = usize::try_from(counter_count).unwrap_or(0);
    with_global_runtime(|rt| rt.m10_staging_mut().inspect_begin(n));
}

/// Clears staged `AFTER` / `BEFORE` window bounds on `INSPECT`.
#[expect(
    unsafe_code,
    reason = "Audited M10 `INSPECT` clear-window C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_insp_clear_win() {
    with_global_runtime(|rt| rt.m10_staging_mut().inspect_clear_window());
}

/// Stages an `AFTER INITIAL` (`is_before == 0`) or `BEFORE INITIAL` (`is_before != 0`) window bound.
///
/// # Arguments
///
/// * `is_before` - `0` for `AFTER`, non-zero for `BEFORE`.
/// * `op_kind` - `1` for `FigurativeByte`, `0` for `Bytes`.
/// * `ptr` - Operand bytes pointer.
/// * `len` - Operand byte length.
/// * `spec_ptr` - Optional serialized field spec pointer.
/// * `spec_len` - Serialized field spec length.
///
/// # Safety
///
/// Non-empty `(ptr, len)` and `(spec_ptr, spec_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited M10 `INSPECT` set-window C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_insp_set_win(
    is_before: i32,
    op_kind: i32,
    ptr: *const u8,
    len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
) -> i32 {
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(raw) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_insp_set_win", "ptr") }) else {
        return -1;
    };
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(spec) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_insp_set_win", "spec_ptr") })
    else {
        return -1;
    };
    let Ok(op) = StagedInspectOperand::from_raw(op_kind, raw, Some(spec)) else {
        return -1;
    };
    let edge = if is_before == 0 {
        InspectWindowEdge::After
    } else {
        InspectWindowEdge::Before
    };
    with_global_runtime(|rt| rt.m10_staging_mut().inspect_set_window(edge, op));
    0
}

/// Stages a pattern/`FROM` (`slot == 0`) or `BY`/`TO` (`slot != 0`) `INSPECT` operand register.
///
/// # Arguments
///
/// * `slot` - `0` for pattern/`FROM`, non-zero for `BY`/`TO`.
/// * `op_kind` - `1` for `FigurativeByte`, `0` for `Bytes`.
/// * `ptr` - Operand bytes pointer.
/// * `len` - Operand byte length.
/// * `spec_ptr` - Optional serialized field spec pointer.
/// * `spec_len` - Serialized field spec byte length.
///
/// # Safety
///
/// Non-empty `(ptr, len)` and `(spec_ptr, spec_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited M10 `INSPECT` set-operand C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_insp_set_op(
    slot: i32,
    op_kind: i32,
    ptr: *const u8,
    len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
) -> i32 {
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(raw) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_insp_set_op", "ptr") }) else {
        return -1;
    };
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(spec) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_insp_set_op", "spec_ptr") })
    else {
        return -1;
    };
    let Ok(op) = StagedInspectOperand::from_raw(op_kind, raw, Some(spec)) else {
        return -1;
    };
    let reg = if slot == 0 {
        InspectRegSlot::PatternOrFrom
    } else {
        InspectRegSlot::ByOrTo
    };
    with_global_runtime(|rt| rt.m10_staging_mut().inspect_set_operand(reg, op));
    0
}

/// Appends a `TALLYING` sub-clause using the currently staged registers.
///
/// # Arguments
///
/// * `counter_slot` - Zero-based counter slot index.
/// * `match_kind` - [`coblang_rt_core::m10_staging::InspectMatchKind`] integer code (`0..=4`).
#[expect(
    unsafe_code,
    reason = "Audited M10 `INSPECT` push-tally C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_insp_push_tally(counter_slot: i64, match_kind: i32) {
    let slot = usize::try_from(counter_slot).unwrap_or(0);
    let kind = decode_inspect_match_kind(match_kind);
    with_global_runtime(|rt| {
        rt.m10_staging_mut()
            .inspect_push_tally_from_regs(slot, kind);
    });
}

/// Appends a `REPLACING` sub-clause using the currently staged registers.
///
/// # Arguments
///
/// * `match_kind` - [`coblang_rt_core::m10_staging::InspectMatchKind`] integer code (`0..=4`).
#[expect(
    unsafe_code,
    reason = "Audited M10 `INSPECT` push-replace C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_insp_push_replace(match_kind: i32) {
    let kind = decode_inspect_match_kind(match_kind);
    with_global_runtime(|rt| {
        rt.m10_staging_mut().inspect_push_replace_from_regs(kind);
    });
}

/// Executes all staged `TALLYING` and `REPLACING` clauses on `(target_ptr, target_len)`.
///
/// # Arguments
///
/// * `target_ptr` - Mutable inspected field pointer.
/// * `target_len` - Inspected field byte length.
/// * `spec_ptr` - Optional serialized field spec pointer.
/// * `spec_len` - Serialized field spec byte length.
///
/// # Safety
///
/// `(target_ptr, target_len)` must reference writable bytes and `(spec_ptr, spec_len)` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited M10 `INSPECT` execute C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_insp_exec(
    target_ptr: *mut u8,
    target_len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
) -> i32 {
    // SAFETY: Validated via `write_raw_slice`.
    let Ok(target) =
        (unsafe { write_raw_slice(target_ptr, target_len, "coblang_rt_insp_exec", "target_ptr") })
    else {
        return -1;
    };
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(spec) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_insp_exec", "spec_ptr") })
    else {
        return -1;
    };
    with_global_runtime(|rt| {
        rt.m10_staging_mut()
            .inspect_exec_tally_replace(target, Some(spec))
    })
    .map_or(-1, |()| 0)
}

/// Reads the `TALLYING` increment accumulated for `counter_slot` during `coblang_rt_insp_exec`.
///
/// # Arguments
///
/// * `counter_slot` - Zero-based counter slot index.
#[expect(
    unsafe_code,
    reason = "Audited M10 `INSPECT` tally delta query C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_insp_get_delta(counter_slot: i64) -> i64 {
    let slot = usize::try_from(counter_slot).unwrap_or(0);
    with_global_runtime(|rt| rt.m10_staging_mut().inspect_tally_delta(slot))
}

/// Executes `INSPECT ... CONVERTING ...` or `TRANSFORM` on `(target_ptr, target_len)` using staged registers.
///
/// # Arguments
///
/// * `target_ptr` - Mutable inspected field pointer.
/// * `target_len` - Inspected field byte length.
/// * `spec_ptr` - Optional serialized field spec pointer.
/// * `spec_len` - Serialized field spec byte length.
///
/// # Safety
///
/// `(target_ptr, target_len)` must reference writable bytes and `(spec_ptr, spec_len)` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited M10 `INSPECT CONVERTING` / `TRANSFORM` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_insp_convert(
    target_ptr: *mut u8,
    target_len: usize,
    spec_ptr: *const u8,
    spec_len: usize,
) -> i32 {
    // SAFETY: Validated via `write_raw_slice`.
    let Ok(target) = (unsafe {
        write_raw_slice(
            target_ptr,
            target_len,
            "coblang_rt_insp_convert",
            "target_ptr",
        )
    }) else {
        return -1;
    };
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(spec) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_insp_convert", "spec_ptr") })
    else {
        return -1;
    };
    with_global_runtime(|rt| {
        rt.m10_staging_mut()
            .inspect_exec_converting_from_regs(target, Some(spec))
    })
    .map_or(-1, |()| 0)
}

/// Stages the `INTO` destination field spec for `STRING` (`NATIONAL` / `UTF-8` character-unit mode).
///
/// # Arguments
///
/// * `spec_ptr` - Serialized `RuntimeFieldSpec` pointer (`spec_len == 0` allows null).
/// * `spec_len` - Serialized `RuntimeFieldSpec` byte length.
///
/// # Safety
///
/// Non-empty `(spec_ptr, spec_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited M17 `STRING` destination spec C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_str_set_dst_spec(spec_ptr: *const u8, spec_len: usize) -> i32 {
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(spec) = (unsafe {
        read_raw_slice(
            spec_ptr,
            spec_len,
            "coblang_rt_str_set_dst_spec",
            "spec_ptr",
        )
    }) else {
        return -1;
    };
    with_global_runtime(|rt| rt.m10_staging_mut().string_set_dst_spec(Some(spec)))
        .map_or(-1, |()| 0)
}

/// Computes the 0-based byte offset of 1-based UTF-8 character position `one_based_char` in `(ptr, len)`.
///
/// # Arguments
///
/// * `ptr` - UTF-8 field byte pointer (`len == 0` allows null).
/// * `len` - Field byte length.
/// * `one_based_char` - 1-based character index.
///
/// # Safety
///
/// Non-empty `(ptr, len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited M17 UTF-8 refmod byte-offset C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_utf8_refmod_off(
    ptr: *const u8,
    len: usize,
    one_based_char: i64,
) -> i64 {
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(bytes) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_utf8_refmod_off", "ptr") })
    else {
        return 0;
    };
    coblang_rt_core::field_ops::utf8_refmod_byte_offset(bytes, one_based_char)
}

/// Computes the UTF-8 slice byte length for `char_count` scalars starting at `byte_off` in `(ptr, len)`.
///
/// # Arguments
///
/// * `ptr` - UTF-8 field byte pointer (`len == 0` allows null).
/// * `len` - Field byte length.
/// * `byte_off` - 0-based starting byte offset.
/// * `char_count` - Requested scalar count (`< 0` selects rest of field).
///
/// # Safety
///
/// Non-empty `(ptr, len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited M17 UTF-8 refmod byte-length C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_utf8_refmod_len(
    ptr: *const u8,
    len: usize,
    byte_off: i64,
    char_count: i64,
) -> i64 {
    // SAFETY: Validated via `read_raw_slice`.
    let Ok(bytes) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_utf8_refmod_len", "ptr") })
    else {
        return 0;
    };
    coblang_rt_core::field_ops::utf8_refmod_byte_length(bytes, byte_off, char_count)
}
