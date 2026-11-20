//! Clean-room C-ABI runtime surface (`libcoblang_rt`) for compiled COBOL executables and modules.

/// Audited `SYSTEM`, `CBL_*`, and `C$*` (`C_*`) built-in runtime subroutines.
pub mod cob_builtins_abi;
/// Audited `CobField` move/compare/sign/integer operations and 256-bit `CobDecimal` C-ABI engine.
pub mod cob_field_decimal_abi;
/// Audited `libgcobol` (`__gg__*`) compatibility shims and portable POSIX C-ABI bindings.
pub mod cob_gcobol_posix_abi;
/// Audited `cob_display`, `cob_accept*`, `cob_set_environment`, `cob_string_*`, `cob_unstring_*`, and `cob_inspect_*` entrypoints.
pub mod cob_io_string_abi;
/// Audited lifecycle, module-frame, and subprogram `cob_call`/`cob_func`/`cob_cancel`/`cob_resolve` entrypoints.
pub mod cob_lifecycle_abi;
/// Clean-room `#[repr(C)]` runtime descriptor types (`CobFieldAttr`, `CobField`, `CobModule`, `CobDecimal`, `CobFile`).
pub mod cob_types;
/// Audited C-ABI entrypoints for Part 5 `MOVE`, `INITIALIZE`, decimal arithmetic, comparisons, and `DISPLAY`.
pub mod field_abi;
/// Audited C-ABI entrypoints for Milestones M14 (`SEQUENTIAL`, `LINE SEQUENTIAL`, `RELATIVE`) and M15 (`INDEXED` ISAM).
pub mod fileio_abi;
/// Audited C-ABI entrypoints for Milestone M10 (`STRING`, `UNSTRING`, `INSPECT`, `TRANSFORM`, `ACCEPT`, and `DISPLAY UPON ENVIRONMENT-*`).
pub mod m10_abi;
/// Audited C-ABI entrypoints for Milestone M11 (`ALLOCATE`, `FREE`, `BASED` checks, `ENTRY` resolution, `cob_malloc`/`cob_free`).
pub mod m11_abi;
/// Audited C-ABI entrypoints for Milestone M12 (intrinsics, `ALL` table slices, `MODULE-*` stack, and `FUNCTION-ID` UDFs).
pub mod m12_abi;
/// Audited C-ABI entrypoints for Milestone M13 (`>>TURN`, `RAISE`/`RESUME`, `DECLARATIVES`, and NIST `DB` `DEBUG-ITEM`).
pub mod m13_abi;
/// Audited C-ABI entrypoints for Milestone M18 (`REPORT SECTION`, `INITIATE`, `GENERATE`, `TERMINATE`, `SUPPRESS`).
pub mod report_abi;
/// Audited C-ABI entrypoints for Milestone M19 (`SCREEN SECTION`, extended `DISPLAY`/`ACCEPT`, `SET ATTRIBUTE`, `CBL_GC_WINDOW`).
pub mod screen_abi;
/// Audited C-ABI entrypoints for Milestone M16 (`SORT`, `MERGE`, `RELEASE`, `RETURN`, and `SORT-RETURN`).
pub mod sort_abi;
/// Audited C-ABI entrypoints for Milestone M20 (`XML`, `JSON`, special registers, and OO COBOL `INVOKE`).
pub mod xml_json_oo_abi;

use std::ffi::{CStr, c_char};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

pub use cob_builtins_abi::{
    C_CALLEDBY, C_COPY, C_DELETE, C_FILEINFO, C_GETPID, C_JUSTIFY, C_MAKEDIR, C_NARG, C_PARAMSIZE,
    C_PRINTABLE, C_SLEEP, C_TOLOWER, C_TOUPPER, CBL_AND, CBL_CHANGE_DIR, CBL_CHECK_FILE_EXIST,
    CBL_CLOSE_FILE, CBL_COPY_FILE, CBL_CREATE_DIR, CBL_CREATE_FILE, CBL_DELETE_DIR,
    CBL_DELETE_FILE, CBL_EQ, CBL_FLUSH_FILE, CBL_GET_CURRENT_DIR, CBL_IMP, CBL_NIMP, CBL_NOR,
    CBL_NOT, CBL_OC_NANOSLEEP, CBL_OPEN_FILE, CBL_OR, CBL_READ_FILE, CBL_RENAME_FILE, CBL_TOLOWER,
    CBL_TOUPPER, CBL_WRITE_FILE, CBL_XOR, SYSTEM,
};
pub use cob_field_decimal_abi::{
    cob_add_int, cob_cmp, cob_cmp_int, cob_cmp_uint, cob_decimal_add, cob_decimal_clear,
    cob_decimal_cmp, cob_decimal_div, cob_decimal_get_field, cob_decimal_init, cob_decimal_mul,
    cob_decimal_pow, cob_decimal_set_field, cob_decimal_set_llint, cob_decimal_set_ullint,
    cob_decimal_sub, cob_get_int, cob_move, cob_numeric_cmp, cob_real_get_sign, cob_real_put_sign,
    cob_set_int, cob_sub_int,
};
pub use cob_gcobol_posix_abi::{
    __gg__add, __gg__call, __gg__compare, __gg__display, __gg__divide, __gg__move, __gg__multiply,
    __gg__subtract, COB_POSIX_O_ACCMODE, COB_POSIX_O_APPEND, COB_POSIX_O_CREAT, COB_POSIX_O_RDONLY,
    COB_POSIX_O_RDWR, COB_POSIX_O_TRUNC, COB_POSIX_O_WRONLY, CobPosixStatBuf, gg_display_to_writer,
    posix_close, posix_errno, posix_exit, posix_open, posix_read, posix_stat, posix_write,
};
pub use cob_io_string_abi::{
    cob_accept, cob_accept_arg_number, cob_accept_arg_value, cob_accept_command_line,
    cob_accept_date, cob_accept_date_yyyymmdd, cob_accept_day, cob_accept_day_of_week,
    cob_accept_day_yyyyddd, cob_accept_environment, cob_accept_from_with_io, cob_accept_time,
    cob_display, cob_display_to_writers, cob_inspect_after, cob_inspect_all, cob_inspect_before,
    cob_inspect_characters, cob_inspect_converting, cob_inspect_finish, cob_inspect_first,
    cob_inspect_init, cob_inspect_leading, cob_inspect_start, cob_inspect_trailing,
    cob_set_environment, cob_string_append, cob_string_delimited, cob_string_finish,
    cob_string_init, cob_unstring_delimited, cob_unstring_finish, cob_unstring_init,
    cob_unstring_into, cob_unstring_tallying,
};
pub use cob_lifecycle_abi::{
    ActiveModuleFrame, CAbiRuntimeState, c_abi_runtime_state, cob_alloc_base, cob_call,
    cob_call_error, cob_call_error_to_writer, cob_cancel, cob_func, cob_init_nomain,
    cob_module_enter, cob_module_leave, cob_resolve, cob_set_location, cob_stop_run,
    coblang_rt_call_invoke, coblang_rt_cancel_by_slice, coblang_rt_copy_cli_arg,
    coblang_rt_current_narg, coblang_rt_get_call_param, coblang_rt_pop_call_frame,
    coblang_rt_push_call_frame, coblang_rt_raw_copy, with_c_abi_state,
};
pub use cob_types::{
    COB_DECIMAL_NAN, COB_FLAG_BINARY_SWAP, COB_FLAG_BLANK_ZERO, COB_FLAG_HAVE_SIGN,
    COB_FLAG_IS_POINTER, COB_FLAG_JUSTIFIED, COB_FLAG_REAL_BINARY, COB_FLAG_SIGN_LEADING,
    COB_FLAG_SIGN_SEPARATE, COB_STORE_KEEP_ON_OVERFLOW, COB_STORE_ROUND,
    COB_STORE_TRUNC_ON_OVERFLOW, COB_TYPE_ALPHANUMERIC, COB_TYPE_ALPHANUMERIC_ALL,
    COB_TYPE_ALPHANUMERIC_EDITED, COB_TYPE_BOOL, COB_TYPE_GROUP, COB_TYPE_NATIONAL,
    COB_TYPE_NATIONAL_EDITED, COB_TYPE_NUMERIC_BINARY, COB_TYPE_NUMERIC_DISPLAY,
    COB_TYPE_NUMERIC_DOUBLE, COB_TYPE_NUMERIC_EDITED, COB_TYPE_NUMERIC_FLOAT,
    COB_TYPE_NUMERIC_PACKED, CobDecimal, CobField, CobFieldAttr, CobFile, CobModule,
};
use coblang_common::{CoblangError, Result};
use coblang_dynload::{LoadedModuleHandle, clear_dynamic_module_registry, resolve_dynamic_module};
use coblang_rt_core::{
    FieldDisplayKind, LineAdvancing, OutputDevice, RuntimeDisplayItem, TrimDirection,
    exceptions::ExceptionCode, trim_ascii_spaces, with_global_runtime, write_display_items,
};
pub use field_abi::{
    coblang_rt_check_odo, coblang_rt_check_refmod, coblang_rt_check_sub, coblang_rt_class_check,
    coblang_rt_class_custom, coblang_rt_cmp_alpha, coblang_rt_cmp_alpha_ex, coblang_rt_dec_begin,
    coblang_rt_dec_binop, coblang_rt_dec_cmp, coblang_rt_dec_div_rem, coblang_rt_dec_neg,
    coblang_rt_dec_push_ascii, coblang_rt_dec_push_field, coblang_rt_dec_store,
    coblang_rt_disp_begin, coblang_rt_disp_flush, coblang_rt_disp_push_field,
    coblang_rt_disp_push_hex, coblang_rt_disp_push_raw, coblang_rt_get_switch,
    coblang_rt_init_begin, coblang_rt_init_exec, coblang_rt_init_push_rep, coblang_rt_move_desc,
    coblang_rt_set_switch, coblang_rt_sort_table, flush_staged_display_to, read_raw_slice,
    write_raw_slice,
};
pub use m10_abi::{
    coblang_rt_accept, coblang_rt_disp_flush_env, coblang_rt_insp_begin, coblang_rt_insp_clear_win,
    coblang_rt_insp_convert, coblang_rt_insp_exec, coblang_rt_insp_get_delta,
    coblang_rt_insp_push_replace, coblang_rt_insp_push_tally, coblang_rt_insp_set_op,
    coblang_rt_insp_set_win, coblang_rt_str_begin, coblang_rt_str_exec, coblang_rt_str_last_ptr,
    coblang_rt_str_push_src, coblang_rt_str_set_delim, coblang_rt_unstr_begin,
    coblang_rt_unstr_finish, coblang_rt_unstr_push_delim, coblang_rt_unstr_set_delim_spec,
    coblang_rt_unstr_step, coblang_rt_unstr_tally_delta,
};
pub use m11_abi::{
    cob_free, cob_malloc, coblang_rt_alloc, coblang_rt_alloc_fill_pattern, coblang_rt_check_based,
    coblang_rt_free, coblang_rt_resolve_entry, coblang_rt_resolve_external,
};
pub use m12_abi::{
    coblang_rt_m12_apply_refmod, coblang_rt_m12_begin, coblang_rt_m12_enter_module,
    coblang_rt_m12_eval_bytes, coblang_rt_m12_eval_to_dec, coblang_rt_m12_format_display_dec,
    coblang_rt_m12_invoke_udf, coblang_rt_m12_leave_module, coblang_rt_m12_pop_dec_arg,
    coblang_rt_m12_push_alg_probe, coblang_rt_m12_push_bytes_arg, coblang_rt_m12_push_keyword_arg,
    coblang_rt_m12_push_omitted_arg, coblang_rt_m12_register_udf, coblang_rt_m12_series_begin,
    coblang_rt_m12_series_end, coblang_rt_m12_set_context,
};
pub use m13_abi::{
    coblang_rt_m13_check_fatal_unhandled, coblang_rt_m13_clear_last_exception,
    coblang_rt_m13_debug_prepare, coblang_rt_m13_debug_set_suppress, coblang_rt_m13_enter_decl,
    coblang_rt_m13_finish_decl, coblang_rt_m13_last_exception_off, coblang_rt_m13_match_ec,
    coblang_rt_m13_raise, coblang_rt_m13_rec_frame_begin, coblang_rt_m13_rec_frame_restore,
    coblang_rt_m13_rec_frame_save, coblang_rt_m13_set_resume, coblang_rt_m13_set_stmt_loc,
    coblang_rt_m13_set_turn, coblang_rt_m13_take_pending_ec, coblang_rt_m13_unhandled_ec,
};
pub use report_abi::{
    coblang_rt_report_add_sum_ascii, coblang_rt_report_after_hook,
    coblang_rt_report_begin_generate, coblang_rt_report_begin_render_line,
    coblang_rt_report_begin_terminate, coblang_rt_report_check_after_new,
    coblang_rt_report_collect_line_spec, coblang_rt_report_config_group,
    coblang_rt_report_config_sum, coblang_rt_report_config_sum_link,
    coblang_rt_report_finish_collect, coblang_rt_report_finish_render_group,
    coblang_rt_report_init_schema, coblang_rt_report_initiate, coblang_rt_report_next_step,
    coblang_rt_report_put_item, coblang_rt_report_set_dynamic_env,
    coblang_rt_report_snapshot_control, coblang_rt_report_submit_gate, coblang_rt_report_suppress,
    coblang_rt_report_sync_sum_field,
};
pub use screen_abi::{
    CBL_GC_SET_SCR_SIZE, CBL_GC_WINDOW, CBL_GET_CSR_POS, coblang_rt_screen_begin_stmt,
    coblang_rt_screen_execute_accept, coblang_rt_screen_execute_display,
    coblang_rt_screen_set_attr, coblang_rt_screen_stage_display_item,
    coblang_rt_screen_stage_edit_item,
};

/// Returns a reference to the run-unit loaded-module handle cache mutex.
#[must_use]
pub fn loaded_modules_cache() -> &'static Mutex<Vec<LoadedModuleHandle>> {
    /// Lazily initialized loaded-module list.
    static CACHE: OnceLock<Mutex<Vec<LoadedModuleHandle>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Executes `op` against [`loaded_modules_cache`], recovering cleanly if poisoned.
///
/// # Arguments
///
/// * `op` - Closure mutating the retained handle vector.
pub fn with_loaded_modules<F>(op: F)
where
    F: FnOnce(&mut Vec<LoadedModuleHandle>),
{
    let mut guard = match loaded_modules_cache().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    op(&mut guard);
}

/// Initializes the COBOL run unit and captures command-line arguments.
///
/// # Arguments
///
/// * `arg_count` - Number of elements in `arg_values` (`<= 0` treated as empty).
/// * `arg_values` - Pointer to `arg_count` NUL-terminated C argument pointers, or null.
///
/// # Safety
///
/// When `arg_count > 0` and `arg_values` is non-null, `arg_values` must point to `arg_count` readable `*const c_char`
/// slots, each being either null or a valid NUL-terminated C string.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `cob_init` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cob_init(arg_count: i32, arg_values: *const *const c_char) {
    let mut parsed_args = Vec::new();
    if let Ok(count) = usize::try_from(arg_count)
        && count > 0
        && !arg_values.is_null()
    {
        // SAFETY: Caller guarantees `arg_values` points to `count` valid `*const c_char` pointers.
        let raw_ptrs = unsafe { core::slice::from_raw_parts(arg_values, count) };
        for &arg_ptr in raw_ptrs {
            if !arg_ptr.is_null() {
                // SAFETY: Caller guarantees each non-null entry in `argv` is NUL-terminated.
                let text = unsafe { CStr::from_ptr(arg_ptr) }
                    .to_string_lossy()
                    .into_owned();
                parsed_args.push(text);
            }
        }
    }
    with_c_abi_state(CAbiRuntimeState::reset);
    with_global_runtime(|rt| rt.initialize(parsed_args));
}

/// Flushes standard streams and tears down run-unit state on normal exit.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `cob_tidy` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn cob_tidy() {
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();
    m11_abi::free_all_tracked_heap_blocks();
    m12_abi::reset_m12_state();
    m13_abi::reset_m13_state();
    fileio_abi::close_all_open_connectors();
    report_abi::reset_report_runtime_state();
    screen_abi::reset_screen_runtime_state();
    sort_abi::reset_all_sort_sessions();
    with_c_abi_state(CAbiRuntimeState::reset);
    clear_dynamic_module_registry();
    with_global_runtime(coblang_rt_core::CobolRuntimeContext::tidy);
    with_loaded_modules(Vec::clear);
}

/// Updates `RETURN-CODE`, flushes I/O, tears down runtime state for `STOP RUN`, and echoes `status`.
///
/// # Arguments
///
/// * `status` - Exit status integer passed to `STOP RUN`.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `STOP RUN` preparation C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_prepare_stop_run(status: i32) -> i32 {
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();
    fileio_abi::close_all_open_connectors();
    report_abi::reset_report_runtime_state();
    screen_abi::reset_screen_runtime_state();
    sort_abi::reset_all_sort_sessions();
    m11_abi::free_all_tracked_heap_blocks();
    with_global_runtime(|rt| {
        rt.tidy();
        rt.set_return_code(status);
    });
    status
}

/// Reads the current `RETURN-CODE` special register (`i32`).
#[expect(
    unsafe_code,
    reason = "Audited clean-room `RETURN-CODE` reader C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_get_return_code() -> i32 {
    with_global_runtime(|rt| rt.return_code())
}

/// Sets the current `RETURN-CODE` special register (`i32`).
///
/// # Arguments
///
/// * `code` - Signed 32-bit value assigned to `RETURN-CODE`.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `RETURN-CODE` writer C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_set_return_code(code: i32) {
    with_global_runtime(|rt| rt.set_return_code(code));
}

/// Marks entry into a `CALL`ed COBOL subprogram.
#[expect(
    unsafe_code,
    reason = "Audited clean-room subprogram entry C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_enter_call() {
    with_global_runtime(coblang_rt_core::CobolRuntimeContext::enter_call);
}

/// Marks return from a `CALL`ed COBOL subprogram.
#[expect(
    unsafe_code,
    reason = "Audited clean-room subprogram leave C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_leave_call() {
    with_global_runtime(coblang_rt_core::CobolRuntimeContext::leave_call);
}

/// Returns `1` when executing inside a `CALL`ed subprogram (`call_depth > 0`), else `0`.
#[expect(
    unsafe_code,
    reason = "Audited clean-room subprogram depth query C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_is_subprogram() -> i32 {
    i32::from(with_global_runtime(|rt| rt.call_depth() > 0))
}

/// Validates and formats parallel `DISPLAY` operand arrays into `stdout_out` or `stderr_out`.
///
/// # Arguments
///
/// * `device` - `0` for `Stdout`, non-zero for `Stderr`.
/// * `advancing` - `0` for `NoAdvancing`, non-zero for `AdvanceLine`.
/// * `arrays` - Parallel raw pointer tuple `(ptrs, lens, kinds, scales, count)`.
/// * `stdout_out` - Destination writer when `device` selects `Stdout`.
/// * `stderr_out` - Destination writer when `device` selects `Stderr`.
///
/// # Errors
///
/// Returns [`CoblangError::AbiNullOrInvalidPointer`] if any required array or element pointer is null,
/// or [`CoblangError::Io`] if writing fails.
///
/// # Safety
///
/// When `count > 0`, `ptrs`, `lens`, `kinds`, and `scales` must each reference `count` initialized elements,
/// and every `(ptrs[i], lens[i])` with `lens[i] > 0` must point to readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited validation of C-ABI `DISPLAY` descriptor arrays."
)]
pub unsafe fn validate_and_write_display<W1: Write, W2: Write>(
    device: i32,
    advancing: i32,
    arrays: (*const *const u8, *const usize, *const u8, *const i16, usize),
    stdout_out: &mut W1,
    stderr_out: &mut W2,
) -> Result<()> {
    let (ptrs, lens, kinds, scales, count) = arrays;
    let mut items = Vec::with_capacity(count);
    if count > 0 {
        if ptrs.is_null() {
            return Err(CoblangError::AbiNullOrInvalidPointer {
                symbol: "coblang_rt_display",
                param_name: "ptrs",
            });
        }
        if lens.is_null() {
            return Err(CoblangError::AbiNullOrInvalidPointer {
                symbol: "coblang_rt_display",
                param_name: "lens",
            });
        }
        if kinds.is_null() {
            return Err(CoblangError::AbiNullOrInvalidPointer {
                symbol: "coblang_rt_display",
                param_name: "kinds",
            });
        }
        if scales.is_null() {
            return Err(CoblangError::AbiNullOrInvalidPointer {
                symbol: "coblang_rt_display",
                param_name: "scales",
            });
        }
        // SAFETY: Non-null checks above + caller contract guarantee `count` readable elements per array.
        let ptr_slice = unsafe { core::slice::from_raw_parts(ptrs, count) };
        // SAFETY: Validated non-null `lens` with `count` elements.
        let len_slice = unsafe { core::slice::from_raw_parts(lens, count) };
        // SAFETY: Validated non-null `kinds` with `count` elements.
        let kind_slice = unsafe { core::slice::from_raw_parts(kinds, count) };
        // SAFETY: Validated non-null `scales` with `count` elements.
        let scale_slice = unsafe { core::slice::from_raw_parts(scales, count) };

        for (((&data_ptr, &byte_len), &raw_kind), &scale) in ptr_slice
            .iter()
            .zip(len_slice.iter())
            .zip(kind_slice.iter())
            .zip(scale_slice.iter())
        {
            let bytes: &[u8] = if byte_len == 0 {
                &[]
            } else if data_ptr.is_null() {
                return Err(CoblangError::AbiNullOrInvalidPointer {
                    symbol: "coblang_rt_display",
                    param_name: "ptrs[i]",
                });
            } else {
                // SAFETY: Caller guarantees non-null `data_ptr` has `byte_len` readable bytes.
                unsafe { core::slice::from_raw_parts(data_ptr, byte_len) }
            };
            items.push(RuntimeDisplayItem {
                kind: FieldDisplayKind::from_abi_tag(raw_kind),
                scale,
                bytes,
            });
        }
    }

    let adv = LineAdvancing::from_abi_code(advancing);
    match OutputDevice::from_abi_code(device) {
        OutputDevice::Stdout => write_display_items(&items, adv, stdout_out),
        OutputDevice::Stderr => write_display_items(&items, adv, stderr_out),
    }
}

/// Emits `count` `DISPLAY` operands to `stdout` (`device == 0`) or `stderr` (`device != 0`).
///
/// Returns `0` on success or `-1` on null-pointer / I/O error.
///
/// # Arguments
///
/// * `device` - `0` for `Stdout`, non-zero for `Stderr`.
/// * `advancing` - `0` for `NoAdvancing`, non-zero for `AdvanceLine`.
/// * `ptrs` - Array of `count` byte pointers.
/// * `lens` - Array of `count` byte lengths.
/// * `kinds` - Array of `count` [`FieldDisplayKind`] tag bytes.
/// * `scales` - Array of `count` signed decimal scale factors.
/// * `count` - Number of operands.
///
/// # Safety
///
/// Must uphold the pointer validity contract of [`validate_and_write_display`].
#[expect(unsafe_code, reason = "Audited clean-room `DISPLAY` C-ABI entrypoint.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_display(
    device: i32,
    advancing: i32,
    ptrs: *const *const u8,
    lens: *const usize,
    kinds: *const u8,
    scales: *const i16,
    count: usize,
) -> i32 {
    let mut out = io::stdout();
    let mut err = io::stderr();
    // SAFETY: Forwarded directly from caller's `coblang_rt_display` preconditions.
    let res = unsafe {
        validate_and_write_display(
            device,
            advancing,
            (ptrs, lens, kinds, scales, count),
            &mut out,
            &mut err,
        )
    };
    if res.is_ok() { 0 } else { -1 }
}

/// Computes `FUNCTION TRIM` view over `(ptr, len)` without heap allocation.
///
/// Returns `0` on success or `-1` if required pointers are null.
///
/// # Arguments
///
/// * `ptr` - Source byte buffer (`null` permitted only when `len == 0`).
/// * `len` - Byte length of `ptr`.
/// * `mode` - Trim mode (`0` = `Both`, `1` = `Leading`, `2` = `Trailing`).
/// * `out_ptr` - Non-null output slot receiving trimmed subslice pointer.
/// * `out_len` - Non-null output slot receiving trimmed byte length.
///
/// # Safety
///
/// `out_ptr` and `out_len` must be valid writable pointers, and when `len > 0`, `ptr` must
/// reference `len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `FUNCTION TRIM` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_trim(
    ptr: *const u8,
    len: usize,
    mode: i32,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) -> i32 {
    if out_ptr.is_null() || out_len.is_null() {
        return -1;
    }
    let slice: &[u8] = if len == 0 {
        &[]
    } else if ptr.is_null() {
        return -1;
    } else {
        // SAFETY: Caller guarantees `ptr` has `len` readable bytes.
        unsafe { core::slice::from_raw_parts(ptr, len) }
    };
    let trimmed = trim_ascii_spaces(slice, TrimDirection::from_abi_code(mode));
    // SAFETY: Validated non-null `out_ptr` and `out_len`.
    unsafe {
        *out_ptr = trimmed.as_ptr();
        *out_len = trimmed.len();
    }
    0
}

/// Collects dynamic module search directories from `lib_path_env` plus `.`.
///
/// # Arguments
///
/// * `lib_path_env` - Optional `COB_LIBRARY_PATH` value (`:`-separated on Unix).
#[must_use]
pub fn build_module_search_dirs(lib_path_env: Option<&str>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(raw) = lib_path_env {
        for part in raw.split(':').filter(|s| !s.is_empty()) {
            dirs.push(PathBuf::from(part));
        }
    }
    dirs.push(PathBuf::from("."));
    dirs
}

/// Dynamically resolves and invokes a zero-argument COBOL module by name, storing its `RETURN-CODE` into `out_rc`.
///
/// Returns `0` on successful call or `-1` if `name_ptr` / `out_rc` is null or module resolution fails.
///
/// # Arguments
///
/// * `name_ptr` - UTF-8 / ASCII module name bytes.
/// * `name_len` - Byte length of `name_ptr`.
/// * `out_rc` - Non-null output slot receiving the callee's returned `RETURN-CODE`.
///
/// # Safety
///
/// `out_rc` must be a valid writable `*mut i32`, and when `name_len > 0`, `name_ptr` must
/// reference `name_len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room dynamic `CALL` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_call_dynamic(
    name_ptr: *const u8,
    name_len: usize,
    out_rc: *mut i32,
) -> i32 {
    if out_rc.is_null() {
        return -1;
    }
    let raw_bytes: &[u8] = if name_len == 0 {
        &[]
    } else if name_ptr.is_null() {
        return -1;
    } else {
        // SAFETY: Caller guarantees `name_ptr` has `name_len` readable bytes.
        unsafe { core::slice::from_raw_parts(name_ptr, name_len) }
    };
    let name = String::from_utf8_lossy(raw_bytes);
    let env_val = std::env::var("COB_LIBRARY_PATH").ok();
    let Ok(entry) = resolve_dynamic_module(&name, env_val.as_deref()) else {
        with_global_runtime(|rt| {
            rt.exceptions_mut()
                .raise_code(ExceptionCode::EcProgramNotFound);
        });
        return -1;
    };
    coblang_rt_enter_call();
    let callee_rc = entry.call();
    coblang_rt_leave_call();
    // SAFETY: Validated non-null `out_rc` above.
    unsafe {
        *out_rc = callee_rc;
    }
    0
}
