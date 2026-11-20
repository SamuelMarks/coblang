//! Audited clean-room C-ABI entrypoints for Milestone M12 (~120 ISO/`GnuCOBOL`/`gcobol`
//! intrinsic functions, `ALL` table-slice series evaluation, `MODULE-*` reflection stack,
//! `FUNCTION-ID` UDF registry/invocation, and function result reference modification).

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard, OnceLock};

use coblang_common::{DigitCount, Result, ScaleFactor};
use coblang_decimal::charset::{Codepage, UnmappedCharPolicy};
use coblang_decimal::{FixedDecimal, RoundingMode, SignednessPolicy};
use coblang_intrinsics::datetime::{
    ClockSnapshot, dt_combined_datetime, dt_current_date, dt_date_of_integer, dt_date_to_yyyymmdd,
    dt_day_of_integer, dt_day_to_yyyyddd, dt_formatted_current_date, dt_formatted_date,
    dt_formatted_datetime, dt_formatted_time, dt_integer_of_date, dt_integer_of_day,
    dt_integer_of_formatted_date, dt_locale_date, dt_locale_time, dt_locale_time_from_seconds,
    dt_seconds_from_formatted_time, dt_seconds_past_midnight, dt_test_date_yyyymmdd,
    dt_test_day_yyyyddd, dt_test_formatted_datetime, dt_when_compiled, dt_year_to_yyyy,
};
use coblang_intrinsics::math::{
    math_abs, math_acos, math_asin, math_atan, math_cos, math_e, math_exp, math_exp10,
    math_factorial, math_fraction_part, math_highest_algebraic, math_integer, math_integer_part,
    math_log, math_log10, math_lowest_algebraic, math_mod, math_pi, math_rem, math_sign, math_sin,
    math_smallest_algebraic, math_sqrt, math_tan,
};
use coblang_intrinsics::meta_unicode::{
    ExceptionReflectionState, ModuleReflectionInfo, NationalByteOrder, UnicodeSlice,
    Uuid4Generator, meta_char_national, meta_content_of, meta_currency_symbol, meta_display_of,
    meta_exception_file, meta_exception_file_n, meta_exception_location, meta_exception_location_n,
    meta_exception_statement, meta_exception_status, meta_module_caller_id, meta_module_date,
    meta_module_formatted_date, meta_module_id, meta_module_path, meta_module_source,
    meta_module_time, meta_national_of, meta_ulength, meta_upos, meta_usubstr, meta_usupplementary,
    meta_uuid4, meta_uvalid, meta_uwidth,
};
use coblang_intrinsics::stats::{
    AlphaSeriesArg, NumericSeriesArg, RandomGenerator, RandomSeed, expand_numeric_series,
    stats_annuity, stats_max_alpha, stats_max_series, stats_mean, stats_median, stats_midrange,
    stats_min_alpha, stats_min_series, stats_ord_max_alpha, stats_ord_max_series,
    stats_ord_min_alpha, stats_ord_min_series, stats_present_value, stats_random, stats_range,
    stats_standard_deviation, stats_sum_series, stats_variance,
};
use coblang_intrinsics::strings::{
    CaseSensitivity, CollatingTable, ConvertFormat, ConvertSpec, CurrencyCasePolicy,
    FindStringMode, FindStringSpec, MeasuredItem, NumericBase, TrimDirection, str_baseconvert,
    str_bit_of, str_bit_to_char, str_boolean_of_integer, str_byte_length, str_char,
    str_concatenate, str_convert, str_find_string, str_hex_of, str_hex_to_char,
    str_integer_of_boolean, str_length, str_locale_compare, str_lower_case, str_numval,
    str_numval_c, str_numval_f, str_ord, str_reverse, str_standard_compare, str_stored_char_length,
    str_substitute, str_substitute_case, str_test_numval, str_test_numval_c, str_test_numval_f,
    str_trim, str_upper_case,
};
use coblang_intrinsics::{AlgebraicTargetSpec, IntrinsicId, ec_argument_function};
use coblang_pic::DecimalPointKind;
use coblang_rt_core::exceptions::ExceptionCode;
use coblang_rt_core::field_ops::{RefModLengthArg, RuntimeBoundFailureMode, evaluate_refmod_bound};
use coblang_rt_core::with_global_runtime;

use crate::m13_abi::note_runtime_exception;
use crate::{read_raw_slice, resolve_dynamic_module};

/// C-ABI function pointer signature of a compiled `FUNCTION-ID` User-Defined Function.
pub type CobUdfEntryFn = unsafe extern "C" fn(
    argc: i32,
    argv_ptrs: *const *mut u8,
    argv_lens: *const usize,
    ret_ptr: *mut u8,
    ret_len: usize,
) -> i32;

/// Maximum pinned byte-slice ring slots retained across concurrent expression operands.
const ARENA_RING_SLOTS: usize = 256;

/// Maximum scan ceiling (`1 MiB`) when measuring NUL-terminated `CONTENT-OF` / `CONTENT-LENGTH` C strings.
const MAX_CSTR_SCAN_LEN: usize = 1 << 20;

/// Byte category tag passed to [`coblang_rt_m12_push_bytes_arg`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BytesArgTag {
    /// Alphanumeric / SBCS bytes (`0`).
    Alphanumeric,
    /// `USAGE NATIONAL` UTF-16BE bytes (`1`).
    NationalUtf16,
    /// `USAGE UTF-8` bytes (`2`).
    Utf8,
    /// 8-byte `USAGE POINTER` storage slot (`3`).
    PointerSlot,
}

impl BytesArgTag {
    /// Decodes a C-ABI `cat_code` integer into [`BytesArgTag`].
    ///
    /// # Arguments
    ///
    /// * `code` - Raw integer category code (`0..=3`).
    #[must_use]
    const fn from_abi(code: i32) -> Self {
        match code {
            1 => Self::NationalUtf16,
            2 => Self::Utf8,
            3 => Self::PointerSlot,
            _ => Self::Alphanumeric,
        }
    }
}

/// Staged argument item inside an [`M12EvalFrame`].
#[derive(Debug, Clone)]
enum StagedM12Arg {
    /// Scalar `FixedDecimal` argument.
    Decimal(FixedDecimal),
    /// Expanded `ALL` numeric table slice (`Vec<FixedDecimal>`).
    NumericSlice(Vec<FixedDecimal>),
    /// Scalar byte-string argument + encoding tag.
    Bytes(Vec<u8>, BytesArgTag),
    /// Expanded `ALL` alphanumeric/national table slice (`Vec<Vec<u8>>`).
    BytesSlice(Vec<Vec<u8>>),
    /// Raw C pointer address (`0` when `NULL`).
    RawPointer(usize),
    /// Canonical uppercase modifier keyword (`LEADING`, `TRAILING`, `SYSTEM-OFFSET`, etc.).
    Keyword(String),
    /// Explicit or trailing `OMITTED` argument.
    Omitted,
    /// Compile-time algebraic type probe (`digits`, `scale`, `signed`).
    AlgProbe {
        /// Digit count (`0` encodes IEEE binary float width in `scale`: `1`=f32, `2`=f64, `3`=f128).
        digits: i32,
        /// Decimal scale or float width selector.
        scale: i32,
        /// Non-zero when signed.
        signed: bool,
    },
}

/// In-progress `ALL` table-slice collector on an [`M12EvalFrame`].
#[derive(Debug, Clone)]
enum ActiveSeriesCollector {
    /// Collecting `FixedDecimal` elements.
    Numeric(Vec<FixedDecimal>),
    /// Collecting byte-slice elements.
    Bytes(Vec<Vec<u8>>),
}

/// Single re-entrant intrinsic evaluation frame on [`M12ProcessState::frames`].
#[derive(Debug)]
struct M12EvalFrame {
    /// Positive `IntrinsicId::to_abi_code()`, or negative extension code (`-1..-5`).
    abi_code: i32,
    /// Active `DECIMAL-POINT` mode (`Period` vs `Comma`).
    dec_point: DecimalPointKind,
    /// Active currency sign string (default `"$"`).
    currency_sign: String,
    /// Optional 256-entry custom collation rank table (`ranks[byte] = rank`).
    collating_ranks: Option<[u8; 256]>,
    /// Formatted ISO exception location `"<unit>; <proc>; <line>"`.
    location: String,
    /// Staged arguments in positional order.
    args: Vec<StagedM12Arg>,
    /// Open `ALL` table-slice collector, if currently inside `series_begin..series_end`.
    open_series: Option<ActiveSeriesCollector>,
}

/// Active `(module_id, source_file)` entry on the run-unit call stack for `MODULE-*` intrinsics.
#[derive(Debug, Clone)]
struct ActiveModuleEntry {
    /// Original `PROGRAM-ID` / `FUNCTION-ID` spelling.
    module_id: String,
    /// Basename of compilation source file.
    source_file: String,
}

/// Process-wide Milestone M12 evaluation frames, PRNG/UUID4 state, arena ring, module stack, and UDF registry.
struct M12ProcessState {
    /// Nested intrinsic evaluation frames.
    frames: Vec<M12EvalFrame>,
    /// Deterministic `FUNCTION RANDOM` state.
    rng: RandomGenerator,
    /// `FUNCTION UUID4` entropy state.
    uuid_gen: Uuid4Generator,
    /// Pinned byte-slice ring keeping returned `*const u8` pointers valid across multi-operand statements.
    arena_ring: Vec<Box<[u8]>>,
    /// Next overwrite cursor in `arena_ring` (`0..ARENA_RING_SLOTS`).
    arena_cursor: usize,
    /// Active call stack of `(module_id, source_file)` frames.
    module_stack: Vec<ActiveModuleEntry>,
    /// Static/in-process UDF symbol table mapping uppercase external name to `CobUdfEntryFn`.
    udf_registry: BTreeMap<String, CobUdfEntryFn>,
    /// Fixed run-unit compile timestamp snapshot backing `WHEN-COMPILED` and `MODULE-DATE`/`TIME`.
    compile_clock: ClockSnapshot,
}

impl Default for M12ProcessState {
    /// Initializes an empty [`M12ProcessState`] with seeded PRNG, `UUIDv4`, and compile timestamp.
    fn default() -> Self {
        Self {
            frames: Vec::new(),
            rng: RandomGenerator::new(),
            uuid_gen: Uuid4Generator::from_system_clock(),
            arena_ring: Vec::with_capacity(ARENA_RING_SLOTS),
            arena_cursor: 0,
            module_stack: Vec::new(),
            udf_registry: BTreeMap::new(),
            compile_clock: ClockSnapshot::now_system_utc(),
        }
    }
}

/// Returns the singleton [`M12ProcessState`] mutex.
fn global_m12_state() -> &'static Mutex<M12ProcessState> {
    /// Singleton M12 staging and module-stack mutex.
    static STATE: OnceLock<Mutex<M12ProcessState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(M12ProcessState::default()))
}

/// Locks [`global_m12_state`], recovering cleanly if poisoned.
fn lock_m12() -> MutexGuard<'static, M12ProcessState> {
    global_m12_state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Resets transient M12 evaluation frames, PRNG seed, and module stack on `cob_tidy` (preserving static `udf_registry`).
pub fn reset_m12_state() {
    let mut st = lock_m12();
    st.frames.clear();
    st.rng = RandomGenerator::new();
    st.module_stack.clear();
}

/// Pins `bytes` in the ring arena and returns a stable `(ptr, len)` pair valid for at least [`ARENA_RING_SLOTS`] evaluations.
///
/// # Arguments
///
/// * `st` - Locked [`M12ProcessState`].
/// * `bytes` - Result byte vector (`empty` returns `(non-null dangling, 0)`).
fn pin_arena_bytes(st: &mut M12ProcessState, bytes: Vec<u8>) -> (*const u8, i64) {
    if bytes.is_empty() {
        return (std::ptr::NonNull::<u8>::dangling().as_ptr(), 0);
    }
    let len_i64 = i64::try_from(bytes.len()).unwrap_or(0);
    let boxed: Box<[u8]> = bytes.into_boxed_slice();
    let ptr = boxed.as_ptr();
    if st.arena_ring.len() < ARENA_RING_SLOTS {
        st.arena_ring.push(boxed);
    } else {
        let idx = st.arena_cursor % ARENA_RING_SLOTS;
        for (i, slot) in st.arena_ring.iter_mut().enumerate() {
            if i == idx {
                *slot = boxed;
                break;
            }
        }
        st.arena_cursor = st.arena_cursor.wrapping_add(1);
    }
    (ptr, len_i64)
}

/// Decodes a raw `(ptr, len)` C slice as UTF-8, returning `default` if invalid/null.
///
/// # Arguments
///
/// * `ptr` - Raw byte pointer.
/// * `len` - Byte length.
/// * `default` - Fallback string when `(ptr, len)` is null or empty.
///
/// # Safety
///
/// Non-zero `(ptr, len)` must reference readable bytes when non-null.
#[expect(unsafe_code, reason = "Internal helper wrapping read_raw_slice.")]
unsafe fn decode_c_utf8_or(ptr: *const u8, len: usize, default: &str) -> String {
    // SAFETY: Forwarded from caller preconditions.
    match unsafe { read_raw_slice(ptr, len, "m12_abi", "utf8") } {
        Ok(b) if !b.is_empty() => String::from_utf8_lossy(b).into_owned(),
        _ => String::from(default),
    }
}

/// Pushes `(module_id, source_file)` onto the active module reflection stack on unit entry.
///
/// # Arguments
///
/// * `mod_ptr` - UTF-8 `PROGRAM-ID` / `FUNCTION-ID` bytes.
/// * `mod_len` - Byte length of `mod_ptr`.
/// * `src_ptr` - UTF-8 source filename bytes.
/// * `src_len` - Byte length of `src_ptr`.
///
/// # Safety
///
/// Non-zero `(mod_ptr, mod_len)` and `(src_ptr, src_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `MODULE-*` stack push C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m12_enter_module(
    mod_ptr: *const u8,
    mod_len: usize,
    src_ptr: *const u8,
    src_len: usize,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let mod_s = unsafe { decode_c_utf8_or(mod_ptr, mod_len, "") };
    // SAFETY: Forwarded from caller preconditions.
    let src_s = unsafe { decode_c_utf8_or(src_ptr, src_len, "") };
    lock_m12().module_stack.push(ActiveModuleEntry {
        module_id: mod_s,
        source_file: src_s,
    });
    0
}

/// Pops the top `(module_id, source_file)` entry from the active module reflection stack on unit exit.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `MODULE-*` stack pop C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m12_leave_module() -> i32 {
    let _ = lock_m12().module_stack.pop();
    0
}

/// Registers a compiled `FUNCTION-ID` User-Defined Function in the process-wide UDF registry.
///
/// # Arguments
///
/// * `name_ptr` - UTF-8 external function name bytes.
/// * `name_len` - Byte length of `name_ptr`.
/// * `fn_ptr` - Compiled [`CobUdfEntryFn`] address cast to `*const c_void`.
///
/// # Safety
///
/// `(name_ptr, name_len)` must be readable and `fn_ptr` must be a valid [`CobUdfEntryFn`] symbol.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `FUNCTION-ID` registration C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m12_register_udf(
    name_ptr: *const u8,
    name_len: usize,
    fn_ptr: *const c_void,
) -> i32 {
    if fn_ptr.is_null() {
        return -1;
    }
    // SAFETY: Forwarded from caller preconditions.
    let Ok(name_b) =
        (unsafe { read_raw_slice(name_ptr, name_len, "coblang_rt_m12_register_udf", "name") })
    else {
        return -1;
    };
    let key = String::from_utf8_lossy(name_b).to_ascii_uppercase();
    // SAFETY: `fn_ptr` is a compiled `@__cobol_udf_*` function matching `CobUdfEntryFn`.
    let entry: CobUdfEntryFn =
        unsafe { std::mem::transmute::<*const c_void, CobUdfEntryFn>(fn_ptr) };
    let _ = lock_m12().udf_registry.insert(key, entry);
    0
}

/// Invokes a registered or dynamically loaded `FUNCTION-ID` User-Defined Function by external name.
///
/// # Arguments
///
/// * `name_ptr` - UTF-8 external UDF name bytes.
/// * `name_len` - Byte length of `name_ptr`.
/// * `argc` - Argument count (`>= 0`).
/// * `argv_ptrs` - Array of `argc` argument data pointers (individual entries may be null for `OMITTED`).
/// * `argv_lens` - Array of `argc` argument byte lengths.
/// * `ret_ptr` - Mutable `RETURNING` buffer pointer.
/// * `ret_len` - Byte length of `ret_ptr`.
///
/// # Safety
///
/// Pointer/length buffers must be valid for the duration of the call.
#[expect(
    unsafe_code,
    reason = "Audited clean-room cross-unit `FUNCTION-ID` invocation C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m12_invoke_udf(
    name_ptr: *const u8,
    name_len: usize,
    argc: i32,
    argv_ptrs: *const *mut u8,
    argv_lens: *const usize,
    ret_ptr: *mut u8,
    ret_len: usize,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(name_b) =
        (unsafe { read_raw_slice(name_ptr, name_len, "coblang_rt_m12_invoke_udf", "name") })
    else {
        return -1;
    };
    let raw_name = String::from_utf8_lossy(name_b);
    let key = raw_name.to_ascii_uppercase();

    let mut found = { lock_m12().udf_registry.get(&key).copied() };
    if found.is_none() {
        let env_val = std::env::var("COB_LIBRARY_PATH").ok();
        let _ = resolve_dynamic_module(&raw_name, env_val.as_deref());
        found = lock_m12().udf_registry.get(&key).copied();
    }
    let Some(func_fn) = found else {
        note_runtime_exception(ExceptionCode::EcProgramNotFound, "FUNCTION", "", true);
        return -1;
    };

    // SAFETY: Invokes compiled UDF with caller-supplied argument arrays and `RETURNING` buffer.
    unsafe { func_fn(argc, argv_ptrs, argv_lens, ret_ptr, ret_len) }
}

/// Begins a new re-entrant intrinsic evaluation frame.
///
/// # Arguments
///
/// * `abi_code` - Positive `IntrinsicId::to_abi_code()`, or negative `HirExtIntrinsic` code (`-1..-5`).
/// * `dec_is_comma` - Non-zero when `DECIMAL-POINT IS COMMA` is active.
#[expect(
    unsafe_code,
    reason = "Audited clean-room intrinsic frame prologue C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m12_begin(abi_code: i32, dec_is_comma: i32) -> i32 {
    let dec_point = if dec_is_comma != 0 {
        DecimalPointKind::Comma
    } else {
        DecimalPointKind::Period
    };
    lock_m12().frames.push(M12EvalFrame {
        abi_code,
        dec_point,
        currency_sign: String::from("$"),
        collating_ranks: None,
        location: String::new(),
        args: Vec::new(),
        open_series: None,
    });
    0
}

/// Attaches unit formatting/collation/location context to the active intrinsic evaluation frame.
///
/// # Arguments
///
/// * `curr_ptr` - Active currency symbol bytes.
/// * `curr_len` - Byte length of `curr_ptr`.
/// * `coll_ptr` - Optional 256-byte `PROGRAM COLLATING SEQUENCE` rank table (`coll_len == 256`).
/// * `coll_len` - Byte length of `coll_ptr` (`0` when native ASCII).
/// * `loc_ptr` - Formatted ISO location `"<unit>; <proc>; <line>"`.
/// * `loc_len` - Byte length of `loc_ptr`.
///
/// # Safety
///
/// Non-zero pointer/length slices must reference readable memory.
#[expect(
    unsafe_code,
    reason = "Audited clean-room intrinsic context setter C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m12_set_context(
    curr_ptr: *const u8,
    curr_len: usize,
    coll_ptr: *const u8,
    coll_len: usize,
    loc_ptr: *const u8,
    loc_len: usize,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let curr_s = unsafe { decode_c_utf8_or(curr_ptr, curr_len, "$") };
    let coll_opt = if coll_len == 256 && !coll_ptr.is_null() {
        let mut buf = [0_u8; 256];
        // SAFETY: Validated non-null `coll_ptr` and `coll_len == 256` above.
        buf.copy_from_slice(unsafe { core::slice::from_raw_parts(coll_ptr, 256) });
        Some(buf)
    } else {
        None
    };
    // SAFETY: Forwarded from caller preconditions.
    let loc_s = unsafe { decode_c_utf8_or(loc_ptr, loc_len, "") };

    if let Some(frame) = lock_m12().frames.last_mut() {
        frame.currency_sign = curr_s;
        frame.collating_ranks = coll_opt;
        frame.location = loc_s;
    }
    0
}

/// Pops the top [`FixedDecimal`] from `rt.decimal_stack_mut()` and appends it to the active M12 frame (or open `ALL` series).
#[expect(
    unsafe_code,
    reason = "Audited clean-room numeric intrinsic argument push C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m12_pop_dec_arg() -> i32 {
    let val = with_global_runtime(|rt| {
        rt.decimal_stack_mut()
            .pop_decimal()
            .unwrap_or(FixedDecimal::ZERO)
    });
    let mut m12 = lock_m12();
    if let Some(frame) = m12.frames.last_mut() {
        if let Some(ActiveSeriesCollector::Numeric(vec)) = &mut frame.open_series {
            vec.push(val);
        } else {
            frame.args.push(StagedM12Arg::Decimal(val));
        }
    }
    0
}

/// Appends a byte-slice (or 8-byte `USAGE POINTER` slot when `cat_code == 3`) argument to the active M12 frame.
///
/// # Arguments
///
/// * `ptr` - Source byte slice pointer (or 8-byte pointer variable address when `cat_code == 3`).
/// * `len` - Byte length of `ptr`.
/// * `cat_code` - `0` = Alphanumeric, `1` = `NationalUtf16`, `2` = Utf8, `3` = `PointerSlot`.
///
/// # Safety
///
/// Non-zero `(ptr, len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room bytes/pointer intrinsic argument push C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m12_push_bytes_arg(
    ptr: *const u8,
    len: usize,
    cat_code: i32,
) -> i32 {
    let tag = BytesArgTag::from_abi(cat_code);
    // SAFETY: Forwarded from caller preconditions.
    let Ok(raw) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_m12_push_bytes_arg", "ptr") })
    else {
        return -1;
    };
    let arg = if tag == BytesArgTag::PointerSlot {
        let addr = raw
            .get(..8)
            .and_then(|b| <[u8; 8]>::try_from(b).ok())
            .map_or(0_usize, usize::from_ne_bytes);
        StagedM12Arg::RawPointer(addr)
    } else {
        StagedM12Arg::Bytes(raw.to_vec(), tag)
    };

    let mut m12 = lock_m12();
    if let Some(frame) = m12.frames.last_mut() {
        if let (Some(ActiveSeriesCollector::Bytes(vec)), StagedM12Arg::Bytes(b, _)) =
            (&mut frame.open_series, &arg)
        {
            vec.push(b.clone());
        } else {
            frame.args.push(arg);
        }
    }
    0
}

/// Appends a canonical uppercase modifier keyword (`LEADING`, `TRAILING`, `SYSTEM-OFFSET`, etc.) to the active M12 frame.
///
/// # Arguments
///
/// * `ptr` - UTF-8 keyword bytes.
/// * `len` - Byte length of `ptr`.
///
/// # Safety
///
/// Non-zero `(ptr, len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room keyword intrinsic argument push C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m12_push_keyword_arg(ptr: *const u8, len: usize) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(raw) = (unsafe { read_raw_slice(ptr, len, "coblang_rt_m12_push_keyword_arg", "ptr") })
    else {
        return -1;
    };
    let kw = String::from_utf8_lossy(raw).to_ascii_uppercase();
    if let Some(frame) = lock_m12().frames.last_mut() {
        frame.args.push(StagedM12Arg::Keyword(kw));
    }
    0
}

/// Appends an `OMITTED` placeholder argument to the active M12 frame.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `OMITTED` intrinsic argument push C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m12_push_omitted_arg() -> i32 {
    if let Some(frame) = lock_m12().frames.last_mut() {
        frame.args.push(StagedM12Arg::Omitted);
    }
    0
}

/// Appends a compile-time algebraic type probe (`HIGHEST-ALGEBRAIC`, `LOWEST-ALGEBRAIC`, `SMALLEST-ALGEBRAIC`).
///
/// # Arguments
///
/// * `digits` - Total decimal digits (`0` encodes IEEE binary float width in `scale`).
/// * `scale` - Decimal scale factor (or `1`/`2`/`3` for `COMP-1`/`COMP-2`/`FLOAT-EXTENDED`).
/// * `signed_flag` - Non-zero when signed.
#[expect(
    unsafe_code,
    reason = "Audited clean-room algebraic probe push C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m12_push_alg_probe(digits: i32, scale: i32, signed_flag: i32) -> i32 {
    if let Some(frame) = lock_m12().frames.last_mut() {
        frame.args.push(StagedM12Arg::AlgProbe {
            digits,
            scale,
            signed: signed_flag != 0,
        });
    }
    0
}

/// Opens an `ALL` table-slice collector on the active M12 frame (`is_numeric != 0` for numeric tables).
///
/// # Arguments
///
/// * `is_numeric` - Non-zero for numeric table slices, `0` for alphanumeric/national table slices.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `ALL` table-slice begin C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m12_series_begin(is_numeric: i32) -> i32 {
    if let Some(frame) = lock_m12().frames.last_mut() {
        frame.open_series = Some(if is_numeric != 0 {
            ActiveSeriesCollector::Numeric(Vec::new())
        } else {
            ActiveSeriesCollector::Bytes(Vec::new())
        });
    }
    0
}

/// Closes the active `ALL` table-slice collector and commits it as a single slice argument on the M12 frame.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `ALL` table-slice end C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m12_series_end() -> i32 {
    if let Some(frame) = lock_m12().frames.last_mut()
        && let Some(coll) = frame.open_series.take()
    {
        match coll {
            ActiveSeriesCollector::Numeric(v) => frame.args.push(StagedM12Arg::NumericSlice(v)),
            ActiveSeriesCollector::Bytes(v) => frame.args.push(StagedM12Arg::BytesSlice(v)),
        }
    }
    0
}

/// Pops the active M12 frame, evaluates the numeric/integer intrinsic, and pushes the resulting
/// [`FixedDecimal`] onto `rt.decimal_stack_mut()` (recording `EC-ARGUMENT-FUNCTION` and pushing `0` on domain error).
#[expect(
    unsafe_code,
    reason = "Audited clean-room numeric intrinsic evaluation C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m12_eval_to_dec() -> i32 {
    let popped = lock_m12().frames.pop();
    let Some(frame) = popped else {
        with_global_runtime(|rt| rt.decimal_stack_mut().push_decimal(FixedDecimal::ZERO));
        return -1;
    };
    let loc = frame.location.clone();
    let res = evaluate_numeric_frame(&frame);
    match res {
        Ok(dec) => {
            with_global_runtime(|rt| rt.decimal_stack_mut().push_decimal(dec));
            0
        }
        Err((ec, fallback)) => {
            note_runtime_exception(ec, "FUNCTION", &loc, true);
            with_global_runtime(|rt| rt.decimal_stack_mut().push_decimal(fallback));
            1
        }
    }
}

/// Pops the active M12 frame, evaluates the alphanumeric/national intrinsic, pins its result in the
/// ring arena, and writes `(*out_ptr, *out_len)`.
///
/// # Arguments
///
/// * `out_ptr` - Output slot receiving `*const u8`.
/// * `out_len` - Output slot receiving byte length (`i64`).
///
/// # Safety
///
/// `out_ptr` and `out_len` must be non-null writable stack pointers.
#[expect(
    unsafe_code,
    reason = "Audited clean-room alphanumeric/national intrinsic evaluation C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m12_eval_bytes(
    out_ptr: *mut *const u8,
    out_len: *mut i64,
) -> i32 {
    if out_ptr.is_null() || out_len.is_null() {
        return -1;
    }
    let popped = lock_m12().frames.pop();
    let Some(frame) = popped else {
        // SAFETY: Validated non-null `out_ptr` and `out_len` above.
        unsafe {
            *out_ptr = std::ptr::NonNull::<u8>::dangling().as_ptr();
            *out_len = 0;
        }
        return -1;
    };
    let loc = frame.location.clone();
    let (bytes, status) = match evaluate_bytes_frame(&frame) {
        Ok(b) => (b, 0),
        Err((ec, fallback)) => {
            note_runtime_exception(ec, "FUNCTION", &loc, true);
            (fallback, 1)
        }
    };
    let (p, l) = {
        let mut st = lock_m12();
        pin_arena_bytes(&mut st, bytes)
    };
    // SAFETY: Validated non-null `out_ptr` and `out_len` above.
    unsafe {
        *out_ptr = p;
        *out_len = l;
    }
    status
}

/// Applies 1-based COBOL reference modification `(off_1based : [len_opt])` to a function result byte slice.
///
/// # Arguments
///
/// * `in_ptr` - Input result pointer.
/// * `in_len` - Input byte length (`>= 0`).
/// * `is_national` - Non-zero when `in_ptr` is UTF-16BE (`2` bytes per character position).
/// * `off_1based` - 1-based character offset.
/// * `len_opt` - Explicit character length (`>= 0`), or `< 0` when length was omitted.
/// * `has_explicit_len` - Non-zero when `len` was explicitly written in `(off:len)`.
/// * `check_enabled` - Non-zero when `EC-BOUND-REF-MOD` checking is active.
/// * `is_fatal` - Non-zero when CLI `-debug` / `-fec=` fatal abort is active without declaratives.
/// * `loc_ptr` - UTF-8 source location string.
/// * `loc_len` - Byte length of `loc_ptr`.
/// * `out_ptr` - Output sliced pointer.
/// * `out_len` - Output sliced byte length.
///
/// # Safety
///
/// Input slice and output slots must reference valid memory.
#[expect(
    unsafe_code,
    reason = "Audited clean-room function reference-modification C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m12_apply_refmod(
    in_ptr: *const u8,
    in_len: i64,
    is_national: i32,
    off_1based: i64,
    len_opt: i64,
    has_explicit_len: i32,
    check_enabled: i32,
    is_fatal: i32,
    loc_ptr: *const u8,
    loc_len: usize,
    out_ptr: *mut *const u8,
    out_len: *mut i64,
) -> i32 {
    if out_ptr.is_null() || out_len.is_null() {
        return -1;
    }
    let unit = if is_national != 0 { 2_i64 } else { 1_i64 };
    let total_chars = (in_len.max(0)) / unit;
    let eff_len = if has_explicit_len != 0 {
        len_opt
    } else {
        total_chars.saturating_sub(off_1based).saturating_add(1)
    };

    if check_enabled != 0
        || with_global_runtime(|rt| rt.exceptions().is_enabled(ExceptionCode::EcBoundRefMod))
    {
        // SAFETY: Forwarded from caller preconditions.
        let loc_s = unsafe { decode_c_utf8_or(loc_ptr, loc_len, "") };
        let mode = if is_fatal != 0 {
            RuntimeBoundFailureMode::FatalAbort
        } else {
            RuntimeBoundFailureMode::RecordOnly
        };
        let len_arg = if has_explicit_len != 0 {
            RefModLengthArg::Explicit(len_opt)
        } else {
            RefModLengthArg::Omitted
        };
        let disp = evaluate_refmod_bound(
            total_chars,
            off_1based,
            len_arg,
            mode,
            &loc_s,
            &mut std::io::stderr(),
        );
        match disp {
            coblang_rt_core::field_ops::BoundCheckDisposition::Pass => {}
            coblang_rt_core::field_ops::BoundCheckDisposition::RecordedNonFatal(_) => {
                note_runtime_exception(ExceptionCode::EcBoundRefMod, "FUNCTION", &loc_s, true);
            }
            coblang_rt_core::field_ops::BoundCheckDisposition::FatalAbortExitOne(_) => {
                note_runtime_exception(ExceptionCode::EcBoundRefMod, "FUNCTION", &loc_s, true);
                with_global_runtime(|rt| rt.set_return_code(1));
                crate::cob_tidy();
                return 2;
            }
        }
    }

    let clamped_off = off_1based.clamp(1, total_chars.max(1)) - 1;
    let max_rem = (total_chars - clamped_off).max(0);
    let clamped_chars = eff_len.clamp(0, max_rem);
    let byte_off = usize::try_from(clamped_off * unit).unwrap_or(0);
    let byte_cnt = usize::try_from(clamped_chars * unit).unwrap_or(0);
    let total_b = usize::try_from(in_len.max(0)).unwrap_or(0);

    // SAFETY: Forwarded from caller preconditions.
    let sliced = if total_b == 0 || byte_cnt == 0 {
        Vec::new()
    } else if let Ok(full) =
        unsafe { read_raw_slice(in_ptr, total_b, "coblang_rt_m12_apply_refmod", "in") }
    {
        full.iter().skip(byte_off).take(byte_cnt).copied().collect()
    } else {
        Vec::new()
    };

    let (p, l) = {
        let mut st = lock_m12();
        pin_arena_bytes(&mut st, sliced)
    };
    // SAFETY: Validated non-null `out_ptr` and `out_len` above.
    unsafe {
        *out_ptr = p;
        *out_len = l;
    }
    0
}

/// Evaluates a numeric/integer [`M12EvalFrame`] into `FixedDecimal` (or `Err((ExceptionCode, default_fallback))`).
///
/// # Arguments
///
/// * `frame` - Completed [`M12EvalFrame`].
fn evaluate_numeric_frame(
    frame: &M12EvalFrame,
) -> core::result::Result<FixedDecimal, (ExceptionCode, FixedDecimal)> {
    if frame.abi_code == -1 {
        // `FUNCTION CONTENT-LENGTH(ptr)` (`0` when NULL!).
        let len = match frame.args.first() {
            Some(StagedM12Arg::RawPointer(0) | StagedM12Arg::Omitted) | None => 0_usize,
            Some(StagedM12Arg::RawPointer(addr)) => scan_cstr_bytes(*addr, None).len(),
            Some(StagedM12Arg::Bytes(b, _)) => b.iter().position(|&c| c == 0).unwrap_or(b.len()),
            _ => 0,
        };
        return Ok(FixedDecimal::from_i64(i64::try_from(len).unwrap_or(0)));
    }

    let Some(id) = u16::try_from(frame.abi_code)
        .ok()
        .and_then(|c| IntrinsicId::from_abi_code(c).ok())
    else {
        return Ok(FixedDecimal::ZERO);
    };

    eval_core_numeric_intrinsic(id, frame).map_err(|_| {
        let fallback = if matches!(id, IntrinsicId::Numval | IntrinsicId::NumvalC) {
            let raw = match frame.args.first() {
                Some(StagedM12Arg::Bytes(b, _)) => b.as_slice(),
                _ => &[],
            };
            coblang_intrinsics::strings::str_numval_salvage(raw, frame.dec_point)
        } else {
            FixedDecimal::ZERO
        };
        (ExceptionCode::EcArgumentFunction, fallback)
    })
}

/// Truncates `dec` toward zero and extracts an `i64` (returning `EC-ARGUMENT-FUNCTION` if out of `i64` range).
///
/// # Arguments
///
/// * `dec` - Input [`FixedDecimal`].
fn decimal_to_i64(dec: &FixedDecimal) -> Result<i64> {
    let (tr, _) = dec.rescale(0, RoundingMode::Truncation)?;
    tr.to_string()
        .parse::<i64>()
        .map_err(|_| ec_argument_function("INTEGER", tr.to_string(), "value out of i64 range"))
}

/// Extracts `StagedM12Arg::Decimal` at `idx`, defaulting to `FixedDecimal::ZERO`.
///
/// # Arguments
///
/// * `frame` - Active [`M12EvalFrame`].
/// * `idx` - Zero-based argument index.
#[must_use]
fn dec_arg_at(frame: &M12EvalFrame, idx: usize) -> FixedDecimal {
    match frame.args.get(idx) {
        Some(StagedM12Arg::Decimal(d)) => *d,
        _ => FixedDecimal::ZERO,
    }
}

/// Extracts `StagedM12Arg::Bytes` at `idx`, defaulting to `&[]`.
///
/// # Arguments
///
/// * `frame` - Active [`M12EvalFrame`].
/// * `idx` - Zero-based argument index.
#[must_use]
fn bytes_arg_at(frame: &M12EvalFrame, idx: usize) -> &[u8] {
    match frame.args.get(idx) {
        Some(StagedM12Arg::Bytes(b, _)) => b.as_slice(),
        _ => &[],
    }
}

/// Converts `Result<Vec<u8>>` into M12 byte evaluation error `(EC-ARGUMENT-FUNCTION, fallback)`.
///
/// # Arguments
///
/// * `res` - Intrinsic evaluation result.
/// * `fallback` - Default byte payload on `EC-ARGUMENT-FUNCTION`.
fn bytes_ok_or(
    res: Result<Vec<u8>>,
    fallback: &[u8],
) -> core::result::Result<Vec<u8>, (ExceptionCode, Vec<u8>)> {
    match res {
        Ok(v) => Ok(v),
        Err(_) => Err((ExceptionCode::EcArgumentFunction, fallback.to_vec())),
    }
}

/// Converts `Result<String>` into M12 byte evaluation error `(EC-ARGUMENT-FUNCTION, fallback)`.
///
/// # Arguments
///
/// * `res` - Intrinsic evaluation result.
/// * `fallback` - Default byte payload on `EC-ARGUMENT-FUNCTION`.
fn str_ok_or(
    res: Result<String>,
    fallback: &[u8],
) -> core::result::Result<Vec<u8>, (ExceptionCode, Vec<u8>)> {
    match res {
        Ok(s) => Ok(s.into_bytes()),
        Err(_) => Err((ExceptionCode::EcArgumentFunction, fallback.to_vec())),
    }
}

/// Dispatches all numeric and integer [`IntrinsicId`] variants to `coblang-intrinsics`.
///
/// # Arguments
///
/// * `id` - Resolved [`IntrinsicId`].
/// * `frame` - Active [`M12EvalFrame`].
#[expect(
    clippy::too_many_lines,
    reason = "Exhaustive flat dispatch table for all ~75 numeric/integer ISO & GnuCOBOL intrinsics."
)]
fn eval_core_numeric_intrinsic(id: IntrinsicId, frame: &M12EvalFrame) -> Result<FixedDecimal> {
    let dec_at = |idx: usize| -> FixedDecimal { dec_arg_at(frame, idx) };
    let i64_at = |idx: usize| -> Result<i64> { decimal_to_i64(&dec_arg_at(frame, idx)) };
    let bytes_at = |idx: usize| -> &[u8] { bytes_arg_at(frame, idx) };
    let coll_table = build_collation_bytes(frame);
    let coll_ref = match &coll_table {
        Some(arr) => CollatingTable::Custom(arr),
        None => CollatingTable::NativeAscii,
    };

    match id {
        IntrinsicId::Abs => Ok(math_abs(&dec_at(0))),
        IntrinsicId::Acos => math_acos(&dec_at(0)),
        IntrinsicId::Asin => math_asin(&dec_at(0)),
        IntrinsicId::Atan => Ok(math_atan(&dec_at(0))),
        IntrinsicId::Cos => Ok(math_cos(&dec_at(0))),
        IntrinsicId::E => Ok(math_e()),
        IntrinsicId::Exp => math_exp(&dec_at(0)),
        IntrinsicId::Exp10 => math_exp10(&dec_at(0)),
        IntrinsicId::Factorial => math_factorial(&dec_at(0)),
        IntrinsicId::FractionPart => Ok(math_fraction_part(&dec_at(0))),
        IntrinsicId::Integer => Ok(math_integer(&dec_at(0))),
        IntrinsicId::IntegerPart => Ok(math_integer_part(&dec_at(0))),
        IntrinsicId::Log => math_log(&dec_at(0)),
        IntrinsicId::Log10 => math_log10(&dec_at(0)),
        IntrinsicId::Mod => math_mod(&dec_at(0), &dec_at(1)),
        IntrinsicId::Pi => Ok(math_pi()),
        IntrinsicId::Rem => math_rem(&dec_at(0), &dec_at(1)),
        IntrinsicId::Sign => Ok(math_sign(&dec_at(0))),
        IntrinsicId::Sin => Ok(math_sin(&dec_at(0))),
        IntrinsicId::Sqrt => math_sqrt(&dec_at(0)),
        IntrinsicId::Tan => Ok(math_tan(&dec_at(0))),
        IntrinsicId::HighestAlgebraic
        | IntrinsicId::LowestAlgebraic
        | IntrinsicId::SmallestAlgebraic => Ok(eval_algebraic_intrinsic(id, frame)),
        IntrinsicId::Annuity => stats_annuity(&dec_at(0), &dec_at(1)),
        IntrinsicId::PresentValue => {
            let rate = dec_at(0);
            let series = collect_numeric_series(&frame.args, 1);
            let flat = expand_numeric_series(&series);
            stats_present_value(&rate, &flat)
        }
        IntrinsicId::Max => {
            let series = collect_numeric_series(&frame.args, 0);
            stats_max_series(&series)
        }
        IntrinsicId::Min => {
            let series = collect_numeric_series(&frame.args, 0);
            stats_min_series(&series)
        }
        IntrinsicId::OrdMax => {
            if is_alpha_series_frame(&frame.args) {
                let alpha = collect_alpha_series(&frame.args);
                stats_ord_max_alpha(&alpha)
                    .map(|u| FixedDecimal::from_i64(i64::try_from(u).unwrap_or(0)))
            } else {
                let series = collect_numeric_series(&frame.args, 0);
                stats_ord_max_series(&series)
                    .map(|u| FixedDecimal::from_i64(i64::try_from(u).unwrap_or(0)))
            }
        }
        IntrinsicId::OrdMin => {
            if is_alpha_series_frame(&frame.args) {
                let alpha = collect_alpha_series(&frame.args);
                stats_ord_min_alpha(&alpha)
                    .map(|u| FixedDecimal::from_i64(i64::try_from(u).unwrap_or(0)))
            } else {
                let series = collect_numeric_series(&frame.args, 0);
                stats_ord_min_series(&series)
                    .map(|u| FixedDecimal::from_i64(i64::try_from(u).unwrap_or(0)))
            }
        }
        IntrinsicId::Sum => stats_sum_series(&collect_numeric_series(&frame.args, 0)),
        IntrinsicId::Mean => stats_mean(&expand_numeric_series(&collect_numeric_series(
            &frame.args,
            0,
        ))),
        IntrinsicId::Median => stats_median(&expand_numeric_series(&collect_numeric_series(
            &frame.args,
            0,
        ))),
        IntrinsicId::Midrange => stats_midrange(&expand_numeric_series(&collect_numeric_series(
            &frame.args,
            0,
        ))),
        IntrinsicId::Range => stats_range(&expand_numeric_series(&collect_numeric_series(
            &frame.args,
            0,
        ))),
        IntrinsicId::Variance => stats_variance(&expand_numeric_series(&collect_numeric_series(
            &frame.args,
            0,
        ))),
        IntrinsicId::StandardDeviation => stats_standard_deviation(&expand_numeric_series(
            &collect_numeric_series(&frame.args, 0),
        )),
        IntrinsicId::Random => {
            let seed_dec = frame.args.first().and_then(|a| match a {
                StagedM12Arg::Decimal(d) => Some(*d),
                _ => None,
            });
            let seed = match &seed_dec {
                Some(d) => RandomSeed::Seeded(d),
                None => RandomSeed::Unseeded,
            };
            let mut st = lock_m12();
            stats_random(&mut st.rng, seed)
        }
        IntrinsicId::SecondsPastMidnight => {
            let clk = effective_runtime_clock()?;
            dt_seconds_past_midnight(clk)
        }
        IntrinsicId::IntegerOfDate => dt_integer_of_date(i64_at(0)?).map(FixedDecimal::from_i64),
        IntrinsicId::DateOfInteger => dt_date_of_integer(i64_at(0)?).map(FixedDecimal::from_i64),
        IntrinsicId::IntegerOfDay => dt_integer_of_day(i64_at(0)?).map(FixedDecimal::from_i64),
        IntrinsicId::DayOfInteger => dt_day_of_integer(i64_at(0)?).map(FixedDecimal::from_i64),
        IntrinsicId::YearToYyyy | IntrinsicId::DateToYyyymmdd | IntrinsicId::DayToYyyyddd => {
            let arg0 = i64_at(0)?;
            let win_off = if frame.args.len() >= 2
                && !matches!(frame.args.get(1), Some(StagedM12Arg::Omitted))
            {
                Some(i64_at(1)?)
            } else {
                None
            };
            let exec_yr = if frame.args.len() >= 3
                && !matches!(frame.args.get(2), Some(StagedM12Arg::Omitted))
            {
                let yr_i64 = i64_at(2)?;
                if !(1601..=9999).contains(&yr_i64) {
                    return Err(ec_argument_function(
                        "YEAR-TO-YYYY",
                        yr_i64.to_string(),
                        "execution year out of 1601..=9999",
                    ));
                }
                u16::try_from(yr_i64).unwrap_or(2026)
            } else {
                effective_runtime_clock()?.year
            };
            let v = match id {
                IntrinsicId::YearToYyyy => dt_year_to_yyyy(arg0, win_off, exec_yr)?,
                IntrinsicId::DateToYyyymmdd => dt_date_to_yyyymmdd(arg0, win_off, exec_yr)?,
                _ => dt_day_to_yyyyddd(arg0, win_off, exec_yr)?,
            };
            Ok(FixedDecimal::from_i64(v))
        }
        IntrinsicId::CombinedDatetime => dt_combined_datetime(i64_at(0)?, &dec_at(1)),
        IntrinsicId::IntegerOfFormattedDate => {
            let fmt = String::from_utf8_lossy(bytes_at(0));
            let val = String::from_utf8_lossy(bytes_at(1));
            dt_integer_of_formatted_date(&fmt, &val).map(FixedDecimal::from_i64)
        }
        IntrinsicId::SecondsFromFormattedTime => {
            let fmt = String::from_utf8_lossy(bytes_at(0));
            let val = String::from_utf8_lossy(bytes_at(1));
            dt_seconds_from_formatted_time(&fmt, &val)
        }
        IntrinsicId::TestDateYyyymmdd => Ok(FixedDecimal::from_i64(i64::from(
            dt_test_date_yyyymmdd(i64_at(0)?),
        ))),
        IntrinsicId::TestDayYyyyddd => Ok(FixedDecimal::from_i64(i64::from(dt_test_day_yyyyddd(
            i64_at(0)?,
        )))),
        IntrinsicId::TestFormattedDatetime => {
            let fmt = String::from_utf8_lossy(bytes_at(0));
            let val = String::from_utf8_lossy(bytes_at(1));
            dt_test_formatted_datetime(&fmt, &val)
                .map(|u| FixedDecimal::from_i64(i64::try_from(u).unwrap_or(0)))
        }
        IntrinsicId::ByteLength => {
            if let Some(StagedM12Arg::Decimal(d)) = frame.args.first() {
                return Ok(*d);
            }
            let n = str_byte_length(bytes_at(0));
            Ok(FixedDecimal::from_i64(i64::try_from(n).unwrap_or(0)))
        }
        IntrinsicId::Length | IntrinsicId::LengthAn => {
            if let Some(StagedM12Arg::Decimal(d)) = frame.args.first() {
                return Ok(*d);
            }
            let has_physical = frame
                .args
                .get(1)
                .is_some_and(|a| matches!(a, StagedM12Arg::Keyword(k) if k == "PHYSICAL"));
            let n = match frame.args.first() {
                Some(StagedM12Arg::Bytes(b, _)) if has_physical => b.len(),
                Some(StagedM12Arg::Bytes(b, BytesArgTag::NationalUtf16)) => {
                    str_length(MeasuredItem::NationalUtf16(b))
                }
                Some(StagedM12Arg::Bytes(b, BytesArgTag::Utf8)) => match core::str::from_utf8(b) {
                    Ok(s) => str_length(MeasuredItem::Utf8(s)),
                    Err(_) => b.len(),
                },
                Some(StagedM12Arg::Bytes(b, _)) => str_length(MeasuredItem::Alphanumeric(b)),
                _ => 0,
            };
            Ok(FixedDecimal::from_i64(i64::try_from(n).unwrap_or(0)))
        }
        IntrinsicId::StoredCharLength => {
            let n = str_stored_char_length(bytes_at(0));
            Ok(FixedDecimal::from_i64(i64::try_from(n).unwrap_or(0)))
        }
        IntrinsicId::Ord => {
            let explicit_coll = frame.args.get(1).and_then(|a| match a {
                StagedM12Arg::Bytes(b, _) if b.len() == 256 => Some(rank_table_to_char_order(b)),
                _ => None,
            });
            let eff_coll = match &explicit_coll {
                Some(arr) => CollatingTable::Custom(arr),
                None => coll_ref,
            };
            str_ord(bytes_at(0), eff_coll).map(|u| FixedDecimal::from_i64(i64::from(u)))
        }
        IntrinsicId::FindString => {
            let mut spec = FindStringSpec::first_sensitive();
            let mut idx = 2_usize;
            while let Some(arg) = frame.args.get(idx) {
                match arg {
                    StagedM12Arg::Keyword(k) if k == "LAST" => spec.mode = FindStringMode::Last,
                    StagedM12Arg::Keyword(k) if k == "ANYCASE" => {
                        spec.case_mode = CaseSensitivity::Insensitive;
                    }
                    StagedM12Arg::Decimal(d) => {
                        let pos = decimal_to_i64(d)
                            .ok()
                            .and_then(|v| usize::try_from(v).ok())
                            .unwrap_or(0);
                        spec.after_pos = pos;
                    }
                    _ => {}
                }
                idx += 1;
            }
            str_find_string(bytes_at(0), bytes_at(1), spec)
                .map(|u| FixedDecimal::from_i64(i64::try_from(u).unwrap_or(0)))
        }
        IntrinsicId::TestNumval => {
            let r = str_test_numval(bytes_at(0), frame.dec_point);
            Ok(FixedDecimal::from_i64(i64::try_from(r).unwrap_or(0)))
        }
        IntrinsicId::Numval => str_numval(bytes_at(0), frame.dec_point),
        IntrinsicId::TestNumvalC | IntrinsicId::NumvalC => {
            let (curr_opt, case_pol) = extract_numval_c_options(frame);
            let curr_slice = curr_opt.as_deref();
            if id == IntrinsicId::TestNumvalC {
                str_test_numval_c(bytes_at(0), curr_slice, case_pol, frame.dec_point)
                    .map(|u| FixedDecimal::from_i64(i64::try_from(u).unwrap_or(0)))
            } else {
                str_numval_c(bytes_at(0), curr_slice, case_pol, frame.dec_point)
            }
        }
        IntrinsicId::TestNumvalF => {
            let r = str_test_numval_f(bytes_at(0), frame.dec_point);
            Ok(FixedDecimal::from_i64(i64::try_from(r).unwrap_or(0)))
        }
        IntrinsicId::NumvalF => str_numval_f(bytes_at(0), frame.dec_point),
        IntrinsicId::IntegerOfBoolean => str_integer_of_boolean(bytes_at(0)),
        IntrinsicId::Ulength => meta_ulength(unicode_arg_at(frame, 0))
            .map(|u| FixedDecimal::from_i64(i64::try_from(u).unwrap_or(0))),
        IntrinsicId::Upos => {
            let idx = usize::try_from(i64_at(1)?)
                .map_err(|_| ec_argument_function("UPOS", "neg", "index must be >= 1"))?;
            meta_upos(unicode_arg_at(frame, 0), idx)
                .map(|u| FixedDecimal::from_i64(i64::try_from(u).unwrap_or(0)))
        }
        IntrinsicId::Usupplementary => meta_usupplementary(unicode_arg_at(frame, 0))
            .map(|u| FixedDecimal::from_i64(i64::try_from(u).unwrap_or(0))),
        IntrinsicId::Uvalid => {
            let u = meta_uvalid(unicode_arg_at(frame, 0));
            Ok(FixedDecimal::from_i64(i64::try_from(u).unwrap_or(0)))
        }
        IntrinsicId::Uwidth => {
            let idx = usize::try_from(i64_at(1)?)
                .map_err(|_| ec_argument_function("UWIDTH", "neg", "index must be >= 1"))?;
            meta_uwidth(unicode_arg_at(frame, 0), idx)
                .map(|u| FixedDecimal::from_i64(i64::try_from(u).unwrap_or(0)))
        }
        _ => Ok(FixedDecimal::ZERO),
    }
}

/// Evaluates `HIGHEST-ALGEBRAIC` / `LOWEST-ALGEBRAIC` / `SMALLEST-ALGEBRAIC`.
///
/// # Arguments
///
/// * `id` - Algebraic intrinsic ID.
/// * `frame` - Active [`M12EvalFrame`].
fn eval_algebraic_intrinsic(id: IntrinsicId, frame: &M12EvalFrame) -> FixedDecimal {
    let Some(&StagedM12Arg::AlgProbe {
        digits,
        scale,
        signed,
    }) = frame.args.first()
    else {
        return FixedDecimal::ZERO;
    };
    if digits == 0 && scale >= 10 {
        let bw = scale - 10;
        return match (id, bw, signed) {
            (IntrinsicId::HighestAlgebraic, 1, true) => FixedDecimal::from_i64(i64::from(i8::MAX)),
            (IntrinsicId::HighestAlgebraic, 1, false) => FixedDecimal::from_i64(i64::from(u8::MAX)),
            (IntrinsicId::HighestAlgebraic, 2, true) => FixedDecimal::from_i64(i64::from(i16::MAX)),
            (IntrinsicId::HighestAlgebraic, 2, false) => {
                FixedDecimal::from_i64(i64::from(u16::MAX))
            }
            (IntrinsicId::HighestAlgebraic, 4, true) => FixedDecimal::from_i64(i64::from(i32::MAX)),
            (IntrinsicId::HighestAlgebraic, 4, false) => {
                FixedDecimal::from_i64(i64::from(u32::MAX))
            }
            (IntrinsicId::HighestAlgebraic, _, true) => FixedDecimal::from_i64(i64::MAX),
            (IntrinsicId::HighestAlgebraic, _, false) => {
                FixedDecimal::parse_str("18446744073709551615").unwrap_or(FixedDecimal::ZERO)
            }
            (IntrinsicId::LowestAlgebraic, _, false) => FixedDecimal::ZERO,
            (IntrinsicId::LowestAlgebraic, 1, true) => FixedDecimal::from_i64(i64::from(i8::MIN)),
            (IntrinsicId::LowestAlgebraic, 2, true) => FixedDecimal::from_i64(i64::from(i16::MIN)),
            (IntrinsicId::LowestAlgebraic, 4, true) => FixedDecimal::from_i64(i64::from(i32::MIN)),
            (IntrinsicId::LowestAlgebraic, _, true) => FixedDecimal::from_i64(i64::MIN),
            _ => FixedDecimal::from_i64(1),
        };
    }
    if digits == 0 {
        // IEEE binary floating-point target (`COMP-1` / `COMP-2` / `FLOAT-EXTENDED`).
        let d_cap = if scale == 1 { 7 } else { 15 };
        let spec = AlgebraicTargetSpec::new(
            DigitCount::try_new(d_cap).unwrap_or(DigitCount::ONE),
            ScaleFactor::ZERO,
            SignednessPolicy::Signed,
        );
        return match id {
            IntrinsicId::HighestAlgebraic => math_highest_algebraic(spec),
            IntrinsicId::LowestAlgebraic => math_lowest_algebraic(spec),
            _ => math_smallest_algebraic(spec),
        };
    }
    let d_u8 = u8::try_from(digits.clamp(1, 38)).unwrap_or(1);
    let s_i16 = i16::try_from(scale.clamp(-38, 38)).unwrap_or(0);
    let pol = if signed {
        SignednessPolicy::Signed
    } else {
        SignednessPolicy::Unsigned
    };
    let spec = AlgebraicTargetSpec::new(
        DigitCount::try_new(d_u8).unwrap_or(DigitCount::ONE),
        ScaleFactor::try_new(s_i16).unwrap_or(ScaleFactor::ZERO),
        pol,
    );
    match id {
        IntrinsicId::HighestAlgebraic => math_highest_algebraic(spec),
        IntrinsicId::LowestAlgebraic => math_lowest_algebraic(spec),
        _ => math_smallest_algebraic(spec),
    }
}

/// Result alias for M12 byte-returning intrinsic helpers.
type M12BytesEvalResult<T> = core::result::Result<T, (ExceptionCode, Vec<u8>)>;

/// Evaluates an alphanumeric/national [`M12EvalFrame`] into `Vec<u8>` (or `Err((ExceptionCode, fallback_bytes))`).
///
/// # Arguments
///
/// * `frame` - Completed [`M12EvalFrame`].
#[expect(
    clippy::too_many_lines,
    reason = "Exhaustive flat dispatch table for all ~45 string/datetime/national/reflection intrinsics."
)]
fn evaluate_bytes_frame(frame: &M12EvalFrame) -> M12BytesEvalResult<Vec<u8>> {
    if frame.abi_code < 0 {
        let ch = match (frame.abi_code, frame.dec_point) {
            (-2 | -4, DecimalPointKind::Period) | (-3 | -5, DecimalPointKind::Comma) => b'.',
            _ => b',',
        };
        return Ok(vec![ch]);
    }

    let Some(id) = u16::try_from(frame.abi_code)
        .ok()
        .and_then(|c| IntrinsicId::from_abi_code(c).ok())
    else {
        return Ok(vec![b' ']);
    };

    let bytes_at = |idx: usize| -> &[u8] { bytes_arg_at(frame, idx) };
    let dec_at = |idx: usize| -> FixedDecimal { dec_arg_at(frame, idx) };
    let i64_at = |idx: usize| -> core::result::Result<i64, (ExceptionCode, Vec<u8>)> {
        match decimal_to_i64(&dec_arg_at(frame, idx)) {
            Ok(v) => Ok(v),
            Err(_) => Err((ExceptionCode::EcArgumentFunction, vec![b' '])),
        }
    };
    let usize_at = |idx: usize| -> core::result::Result<usize, (ExceptionCode, Vec<u8>)> {
        let v = i64_at(idx)?;
        match usize::try_from(v) {
            Ok(u) => Ok(u),
            Err(_) => Err((ExceptionCode::EcArgumentFunction, Vec::new())),
        }
    };
    let coll_table = build_collation_bytes(frame);
    let coll_ref = match &coll_table {
        Some(arr) => CollatingTable::Custom(arr),
        None => CollatingTable::NativeAscii,
    };

    match id {
        IntrinsicId::ContentOf => {
            with_global_runtime(|rt| rt.exceptions_mut().clear_last_exception());
            let opt_len = if frame.args.len() >= 2
                && !matches!(frame.args.get(1), Some(StagedM12Arg::Omitted))
            {
                let ul = usize_at(1)?;
                if ul == 0 {
                    return Err((ExceptionCode::EcArgumentFunction, Vec::new()));
                }
                Some(ul)
            } else {
                None
            };
            match frame.args.first() {
                Some(StagedM12Arg::RawPointer(0) | StagedM12Arg::Omitted) | None => {
                    Err((ExceptionCode::EcDataPtrNull, Vec::new()))
                }
                Some(StagedM12Arg::RawPointer(addr)) => {
                    let scanned = scan_cstr_bytes(*addr, opt_len);
                    bytes_ok_or(meta_content_of(Some(scanned.as_slice()), opt_len), &[])
                }
                Some(StagedM12Arg::Bytes(b, _)) => {
                    let nul_cut = if opt_len.is_none() {
                        b.split(|&c| c == 0).next().unwrap_or(b.as_slice())
                    } else {
                        b.as_slice()
                    };
                    bytes_ok_or(meta_content_of(Some(nul_cut), opt_len), &[])
                }
                _ => Err((ExceptionCode::EcDataPtrNull, Vec::new())),
            }
        }
        IntrinsicId::Char => {
            let explicit_coll = frame.args.get(1).and_then(|a| match a {
                StagedM12Arg::Bytes(b, _) if b.len() == 256 => Some(rank_table_to_char_order(b)),
                _ => None,
            });
            let eff_coll = match &explicit_coll {
                Some(arr) => CollatingTable::Custom(arr),
                None => coll_ref,
            };
            bytes_ok_or(str_char(i64_at(0)?, eff_coll).map(|b| vec![b]), &[0])
        }
        IntrinsicId::CharNational => bytes_ok_or(
            meta_char_national(i64_at(0)?, NationalByteOrder::BigEndian),
            &[0, b' '],
        ),
        IntrinsicId::DisplayOf => {
            let pad_pol =
                frame
                    .args
                    .get(1)
                    .map_or(UnmappedCharPolicy::Substitute('?'), |a| match a {
                        StagedM12Arg::Bytes(b, _) if !b.is_empty() => {
                            UnmappedCharPolicy::Substitute(char::from(
                                b.first().copied().unwrap_or(b'?'),
                            ))
                        }
                        _ => UnmappedCharPolicy::Substitute('?'),
                    });
            bytes_ok_or(
                meta_display_of(
                    bytes_at(0),
                    NationalByteOrder::BigEndian,
                    Codepage::Iso8859_1,
                    pad_pol,
                ),
                b" ",
            )
        }
        IntrinsicId::NationalOf => {
            let pad_pol =
                frame
                    .args
                    .get(1)
                    .map_or(UnmappedCharPolicy::Substitute('?'), |a| match a {
                        StagedM12Arg::Bytes(b, _) if b.len() >= 2 => {
                            let cp = u16::from_be_bytes([
                                b.first().copied().unwrap_or(0),
                                b.get(1).copied().unwrap_or(b'?'),
                            ]);
                            UnmappedCharPolicy::Substitute(
                                char::from_u32(u32::from(cp)).unwrap_or('?'),
                            )
                        }
                        _ => UnmappedCharPolicy::Substitute('?'),
                    });
            bytes_ok_or(
                meta_national_of(
                    bytes_at(0),
                    Codepage::Iso8859_1,
                    NationalByteOrder::BigEndian,
                    pad_pol,
                ),
                &[0, b' '],
            )
        }
        IntrinsicId::Concatenate => {
            let pieces: Vec<&[u8]> = frame
                .args
                .iter()
                .filter_map(|a| match a {
                    StagedM12Arg::Bytes(b, _) => Some(b.as_slice()),
                    _ => None,
                })
                .collect();
            Ok(str_concatenate(&pieces))
        }
        IntrinsicId::Reverse => {
            if matches!(
                frame.args.first(),
                Some(StagedM12Arg::Bytes(_, BytesArgTag::NationalUtf16))
            ) {
                let b = bytes_at(0);
                let mut out = Vec::with_capacity(b.len());
                for chunk in b.as_chunks::<2>().0.iter().rev() {
                    out.extend_from_slice(chunk);
                }
                Ok(out)
            } else {
                Ok(str_reverse(bytes_at(0)))
            }
        }
        IntrinsicId::UpperCase => Ok(str_upper_case(bytes_at(0))),
        IntrinsicId::LowerCase => Ok(str_lower_case(bytes_at(0))),
        IntrinsicId::Trim => {
            let is_nat = matches!(
                frame.args.first(),
                Some(StagedM12Arg::Bytes(_, BytesArgTag::NationalUtf16))
            );
            let mut dir = TrimDirection::Both;
            let mut pad_idx = 1_usize;
            if let Some(StagedM12Arg::Keyword(kw)) = frame.args.get(1) {
                dir = match kw.as_str() {
                    "LEADING" => TrimDirection::Leading,
                    "TRAILING" => TrimDirection::Trailing,
                    _ => TrimDirection::Both,
                };
                pad_idx = 2;
            }
            if is_nat {
                Ok(trim_national_utf16(
                    bytes_at(0),
                    dir,
                    frame.args.get(pad_idx),
                ))
            } else {
                let pad_byte = match frame.args.get(pad_idx) {
                    Some(StagedM12Arg::Bytes(b, _)) if !b.is_empty() => {
                        if b.len() != 1 {
                            return Err((ExceptionCode::EcArgumentFunction, Vec::new()));
                        }
                        b.first().copied()
                    }
                    _ => None,
                };
                Ok(str_trim(bytes_at(0), dir, pad_byte))
            }
        }
        IntrinsicId::Substitute | IntrinsicId::SubstituteCase => {
            let subj = bytes_at(0);
            let mut pairs = Vec::new();
            let mut idx = 1_usize;
            while idx + 1 < frame.args.len() {
                pairs.push((bytes_at(idx), bytes_at(idx + 1)));
                idx += 2;
            }
            let res = if id == IntrinsicId::Substitute {
                str_substitute(subj, &pairs)
            } else {
                str_substitute_case(subj, &pairs)
            };
            bytes_ok_or(res, subj)
        }
        IntrinsicId::BaseConvert => {
            let r_src = u8::try_from(i64_at(1)?).unwrap_or(0);
            let r_dst = u8::try_from(i64_at(2)?).unwrap_or(0);
            let res = NumericBase::try_new(r_src).and_then(|sb| {
                NumericBase::try_new(r_dst).and_then(|db| str_baseconvert(bytes_at(0), sb, db))
            });
            bytes_ok_or(res, b"0")
        }
        IntrinsicId::BooleanOfInteger => {
            let bit_len = usize_at(1)?;
            bytes_ok_or(str_boolean_of_integer(&dec_at(0), bit_len), &[])
        }
        IntrinsicId::BitOf => Ok(str_bit_of(bytes_at(0))),
        IntrinsicId::BitToChar => bytes_ok_or(str_bit_to_char(bytes_at(0)), &[]),
        IntrinsicId::HexOf => Ok(str_hex_of(bytes_at(0))),
        IntrinsicId::HexToChar => bytes_ok_or(str_hex_to_char(bytes_at(0)), &[]),
        IntrinsicId::Convert => {
            let res =
                if let (Some(StagedM12Arg::Keyword(src_k)), Some(StagedM12Arg::Keyword(dst_k))) =
                    (frame.args.get(1), frame.args.get(2))
                {
                    let map_fmt = |s: &str| match s {
                        "HEX" | "HEXADECIMAL" => ConvertFormat::HexUpper,
                        "BIT" => ConvertFormat::BitString,
                        _ => ConvertFormat::Alphanumeric,
                    };
                    str_convert(
                        bytes_at(0),
                        ConvertSpec::Format {
                            source: map_fmt(src_k),
                            target: map_fmt(dst_k),
                        },
                    )
                } else {
                    str_convert(
                        bytes_at(0),
                        ConvertSpec::Alphabet {
                            from_chars: bytes_at(1),
                            to_chars: bytes_at(2),
                        },
                    )
                };
            bytes_ok_or(res, &[])
        }
        IntrinsicId::StandardCompare => {
            let lvl = if frame.args.len() >= 4 {
                u8::try_from(i64_at(3)?).ok()
            } else {
                None
            };
            bytes_ok_or(
                str_standard_compare(bytes_at(0), bytes_at(1), coll_ref, lvl)
                    .map(|c| vec![c.as_byte()]),
                b"=",
            )
        }
        IntrinsicId::LocaleCompare => {
            let loc_opt = (frame.args.len() >= 3).then(|| String::from_utf8_lossy(bytes_at(2)));
            bytes_ok_or(
                str_locale_compare(bytes_at(0), bytes_at(1), loc_opt.as_deref())
                    .map(|c| vec![c.as_byte()]),
                b"=",
            )
        }
        IntrinsicId::Max | IntrinsicId::Min => {
            let alpha = collect_alpha_series(&frame.args);
            let res = if id == IntrinsicId::Max {
                stats_max_alpha(&alpha)
            } else {
                stats_min_alpha(&alpha)
            };
            bytes_ok_or(res.map(<[u8]>::to_vec), b" ")
        }
        IntrinsicId::CurrentDate => {
            let env_ov = with_global_runtime(|rt| rt.lookup_env("COB_CURRENT_DATE"));
            bytes_ok_or(
                dt_current_date(ClockSnapshot::now_system_utc(), env_ov.as_deref())
                    .map(|a| a.to_vec()),
                &[b' '; 21],
            )
        }
        IntrinsicId::WhenCompiled => {
            let clk = lock_m12().compile_clock;
            bytes_ok_or(dt_when_compiled(clk).map(|a| a.to_vec()), &[b' '; 21])
        }
        IntrinsicId::FormattedCurrentDate => {
            let fmt = String::from_utf8_lossy(bytes_at(0));
            let env_ov = with_global_runtime(|rt| rt.lookup_env("COB_CURRENT_DATE"));
            str_ok_or(
                dt_formatted_current_date(&fmt, ClockSnapshot::now_system_utc(), env_ov.as_deref()),
                b" ",
            )
        }
        IntrinsicId::FormattedDate => {
            let fmt = String::from_utf8_lossy(bytes_at(0));
            str_ok_or(dt_formatted_date(&fmt, i64_at(1)?), b" ")
        }
        IntrinsicId::FormattedTime => {
            let fmt = String::from_utf8_lossy(bytes_at(0));
            let secs = dec_at(1);
            let (mut off_m, mut sys_m) = extract_time_offsets(frame, 2)?;
            let ft = fmt.trim_end_matches(' ');
            if off_m.is_none() && (ft.ends_with("+hhmm") || ft.ends_with("+hh:mm")) {
                off_m = sys_m.take().or(Some(0));
            }
            str_ok_or(dt_formatted_time(&fmt, &secs, off_m, sys_m), b" ")
        }
        IntrinsicId::FormattedDatetime => {
            let fmt = String::from_utf8_lossy(bytes_at(0));
            let lil = i64_at(1)?;
            let secs = dec_at(2);
            let (mut off_m, mut sys_m) = extract_time_offsets(frame, 3)?;
            let ft = fmt.trim_end_matches(' ');
            if off_m.is_none() && (ft.ends_with("+hhmm") || ft.ends_with("+hh:mm")) {
                off_m = sys_m.take().or(Some(0));
            }
            str_ok_or(dt_formatted_datetime(&fmt, lil, &secs, off_m, sys_m), b" ")
        }
        IntrinsicId::LocaleDate => {
            let ymd = String::from_utf8_lossy(bytes_at(0));
            let loc_opt = (frame.args.len() >= 2).then(|| String::from_utf8_lossy(bytes_at(1)));
            str_ok_or(dt_locale_date(&ymd, loc_opt.as_deref()), b" ")
        }
        IntrinsicId::LocaleTime => {
            let hms = String::from_utf8_lossy(bytes_at(0));
            let loc_opt = (frame.args.len() >= 2).then(|| String::from_utf8_lossy(bytes_at(1)));
            str_ok_or(dt_locale_time(&hms, loc_opt.as_deref()), b" ")
        }
        IntrinsicId::LocaleTimeFromSeconds => {
            let loc_opt = (frame.args.len() >= 2).then(|| String::from_utf8_lossy(bytes_at(1)));
            str_ok_or(
                dt_locale_time_from_seconds(&dec_at(0), loc_opt.as_deref()),
                b" ",
            )
        }
        IntrinsicId::Usubstr => {
            let start = usize_at(1)?;
            let count = usize_at(2)?;
            bytes_ok_or(meta_usubstr(unicode_arg_at(frame, 0), start, count), &[])
        }
        IntrinsicId::CurrencySymbol => str_ok_or(meta_currency_symbol(&frame.currency_sign), b"$"),
        IntrinsicId::ModuleCallerId
        | IntrinsicId::ModuleDate
        | IntrinsicId::ModuleFormattedDate
        | IntrinsicId::ModuleId
        | IntrinsicId::ModulePath
        | IntrinsicId::ModuleSource
        | IntrinsicId::ModuleTime => Ok(eval_module_reflection(id)),
        IntrinsicId::ExceptionFile
        | IntrinsicId::ExceptionFileN
        | IntrinsicId::ExceptionLocation
        | IntrinsicId::ExceptionLocationN
        | IntrinsicId::ExceptionStatement
        | IntrinsicId::ExceptionStatus => Ok(eval_exception_reflection(id)),
        IntrinsicId::Uuid4 => {
            let mut st = lock_m12();
            Ok(meta_uuid4(&mut st.uuid_gen).to_vec())
        }
        _ => Ok(vec![b' ']),
    }
}

/// Builds `UnicodeSlice` for argument `idx` of `frame`.
///
/// # Arguments
///
/// * `frame` - Active [`M12EvalFrame`].
/// * `idx` - Argument index.
#[must_use]
fn unicode_arg_at(frame: &M12EvalFrame, idx: usize) -> UnicodeSlice<'_> {
    match frame.args.get(idx) {
        Some(StagedM12Arg::Bytes(b, BytesArgTag::NationalUtf16)) => UnicodeSlice::Utf16Be(b),
        Some(StagedM12Arg::Bytes(b, _)) => UnicodeSlice::Utf8(b),
        _ => UnicodeSlice::Utf8(&[]),
    }
}

/// Trims UTF-16BE code units on `USAGE NATIONAL` strings for `FUNCTION TRIM`.
///
/// # Arguments
///
/// * `bytes` - UTF-16BE byte slice.
/// * `dir` - Leading, trailing, or both-sides trim direction.
/// * `pad_arg` - Optional 2-byte UTF-16BE pad character argument.
#[must_use]
fn trim_national_utf16(
    bytes: &[u8],
    dir: TrimDirection,
    pad_arg: Option<&StagedM12Arg>,
) -> Vec<u8> {
    let pad_unit: [u8; 2] = match pad_arg {
        Some(StagedM12Arg::Bytes(p, _)) if p.len() >= 2 => [
            p.first().copied().unwrap_or(0),
            p.get(1).copied().unwrap_or(b' '),
        ],
        _ => [0x00, 0x20],
    };
    let units = bytes.as_chunks::<2>().0;
    let start = match dir {
        TrimDirection::Both | TrimDirection::Leading => {
            units.iter().take_while(|&&u| u == pad_unit).count()
        }
        TrimDirection::Trailing => 0,
    };
    let tail = units.get(start..).unwrap_or(&[]);
    let kept = match dir {
        TrimDirection::Both | TrimDirection::Trailing => {
            let trailing = tail.iter().rev().take_while(|&&u| u == pad_unit).count();
            tail.get(..tail.len().saturating_sub(trailing))
                .unwrap_or(&[])
        }
        TrimDirection::Leading => tail,
    };
    let mut out = Vec::with_capacity(kept.len() * 2);
    for u in kept {
        out.extend_from_slice(u);
    }
    out
}

/// Extracts `(explicit_offset_minutes, system_offset_minutes)` from `frame.args[start_idx..]`.
///
/// # Arguments
///
/// * `frame` - Active [`M12EvalFrame`].
/// * `start_idx` - First optional offset argument index (`2` for `FORMATTED-TIME`, `3` for `FORMATTED-DATETIME`).
fn extract_time_offsets(
    frame: &M12EvalFrame,
    start_idx: usize,
) -> M12BytesEvalResult<(Option<i32>, Option<i32>)> {
    let mut off_m = None;
    let mut sys_m = None;
    for arg in frame.args.iter().skip(start_idx) {
        match arg {
            StagedM12Arg::Keyword(k) if k == "SYSTEM-OFFSET" => {
                sys_m = Some(0);
            }
            StagedM12Arg::Decimal(d) => {
                let v = decimal_to_i64(d)
                    .ok()
                    .and_then(|x| i32::try_from(x).ok())
                    .ok_or_else(|| (ExceptionCode::EcArgumentFunction, vec![b' ']))?;
                off_m = Some(v);
            }
            _ => {}
        }
    }
    Ok((off_m, sys_m))
}

/// Extracts optional currency string and `ANYCASE` policy for `NUMVAL-C` / `TEST-NUMVAL-C`.
///
/// # Arguments
///
/// * `frame` - Active [`M12EvalFrame`].
#[must_use]
fn extract_numval_c_options(frame: &M12EvalFrame) -> (Option<Vec<u8>>, CurrencyCasePolicy) {
    let mut curr_bytes: Option<Vec<u8>> = Some(frame.currency_sign.as_bytes().to_vec());
    let mut pol = CurrencyCasePolicy::ExactCase;
    for arg in frame.args.iter().skip(1) {
        match arg {
            StagedM12Arg::Keyword(k) if k == "ANYCASE" => pol = CurrencyCasePolicy::AnyCase,
            StagedM12Arg::Bytes(b, _) => {
                let trimmed = str_trim(b, TrimDirection::Both, Some(b' '));
                curr_bytes = Some(if trimmed.is_empty() {
                    b.clone()
                } else {
                    trimmed
                });
            }
            _ => {}
        }
    }
    (curr_bytes, pol)
}

/// Builds a 256-entry ordinal-to-byte table from `frame.collating_ranks` when custom collation is active.
///
/// # Arguments
///
/// * `frame` - Active [`M12EvalFrame`].
#[must_use]
fn build_collation_bytes(frame: &M12EvalFrame) -> Option<[u8; 256]> {
    frame
        .collating_ranks
        .as_ref()
        .map(|ranks| rank_table_to_char_order(ranks))
}

/// Inverts a 256-byte `byte -> rank` table into `ordinal_0_based -> byte` expected by [`CollatingTable::Custom`].
///
/// # Arguments
///
/// * `ranks` - 256-entry byte-to-rank weight slice.
#[must_use]
fn rank_table_to_char_order(ranks: &[u8]) -> [u8; 256] {
    let mut pairs: Vec<(u8, u8)> = ranks
        .iter()
        .enumerate()
        .map(|(byte_idx, &rk)| (rk, u8::try_from(byte_idx).unwrap_or(0)))
        .collect();
    pairs.sort_by_key(|&(rk, b)| (rk, b));
    let mut out = [0_u8; 256];
    for (dst, (_, byte_val)) in out.iter_mut().zip(pairs) {
        *dst = byte_val;
    }
    out
}

/// Collects [`NumericSeriesArg`] slices starting at `start_idx` in `args`.
///
/// # Arguments
///
/// * `args` - Staged frame arguments.
/// * `start_idx` - Starting index (`0` for `MAX`/`SUM`, `1` for `PRESENT-VALUE`).
#[must_use]
fn collect_numeric_series(args: &[StagedM12Arg], start_idx: usize) -> Vec<NumericSeriesArg<'_>> {
    args.iter()
        .skip(start_idx)
        .filter_map(|a| match a {
            StagedM12Arg::Decimal(d) => Some(NumericSeriesArg::Scalar(*d)),
            StagedM12Arg::NumericSlice(s) => Some(NumericSeriesArg::TableAll(s.as_slice())),
            _ => None,
        })
        .collect()
}

/// Returns `true` when `args` starts with an alphanumeric/national scalar or `ALL` slice.
///
/// # Arguments
///
/// * `args` - Staged frame arguments.
#[must_use]
fn is_alpha_series_frame(args: &[StagedM12Arg]) -> bool {
    matches!(
        args.first(),
        Some(StagedM12Arg::Bytes(..) | StagedM12Arg::BytesSlice(_))
    )
}

/// Collects [`AlphaSeriesArg`] slices from `args`.
///
/// # Arguments
///
/// * `args` - Staged frame arguments.
#[must_use]
fn collect_alpha_series(args: &[StagedM12Arg]) -> Vec<AlphaSeriesArg<'_>> {
    // Note: `BytesSlice` stores `Vec<Vec<u8>>`; expand each element as scalar in exact table order.
    let mut out = Vec::new();
    for a in args {
        match a {
            StagedM12Arg::Bytes(b, _) => out.push(AlphaSeriesArg::Scalar(b.as_slice())),
            StagedM12Arg::BytesSlice(rows) => {
                for r in rows {
                    out.push(AlphaSeriesArg::Scalar(r.as_slice()));
                }
            }
            _ => {}
        }
    }
    out
}

/// Computes the effective [`ClockSnapshot`] honoring `COB_CURRENT_DATE` when set.
fn effective_runtime_clock() -> Result<ClockSnapshot> {
    let env_ov = with_global_runtime(|rt| rt.lookup_env("COB_CURRENT_DATE"));
    let raw_21 = dt_current_date(ClockSnapshot::now_system_utc(), env_ov.as_deref())?;
    let s = core::str::from_utf8(&raw_21).unwrap_or("2026010100000000+0000");
    let parse_u = |lo: usize, hi: usize| -> u32 {
        s.get(lo..hi)
            .and_then(|sub| sub.parse::<u32>().ok())
            .unwrap_or(0)
    };
    Ok(ClockSnapshot {
        year: u16::try_from(parse_u(0, 4)).unwrap_or(2026),
        month: u8::try_from(parse_u(4, 6)).unwrap_or(1),
        day: u8::try_from(parse_u(6, 8)).unwrap_or(1),
        hour: u8::try_from(parse_u(8, 10)).unwrap_or(0),
        minute: u8::try_from(parse_u(10, 12)).unwrap_or(0),
        second: u8::try_from(parse_u(12, 14)).unwrap_or(0),
        nanosecond: parse_u(14, 16) * 10_000_000,
        offset: coblang_intrinsics::datetime::UtcOffsetSpec::Minutes(0),
    })
}

/// Evaluates `MODULE-*` reflection intrinsics from [`M12ProcessState::module_stack`] and `compile_clock`.
///
/// # Arguments
///
/// * `id` - `MODULE-*` [`IntrinsicId`].
#[must_use]
fn eval_module_reflection(id: IntrinsicId) -> Vec<u8> {
    let (cur_id, caller_id, src_file, clk) = {
        let st = lock_m12();
        let len = st.module_stack.len();
        let cur = st
            .module_stack
            .last()
            .map_or_else(String::new, |m| m.module_id.clone());
        let caller = len
            .checked_sub(2)
            .and_then(|i| st.module_stack.get(i))
            .map_or_else(String::new, |m| m.module_id.clone());
        let src = st
            .module_stack
            .last()
            .map_or_else(String::new, |m| m.source_file.clone());
        (cur, caller, src, st.compile_clock)
    };
    let ts21 = dt_when_compiled(clk).unwrap_or(*b"2026010100000000+0000");
    let [d0, d1, d2, d3, d4, d5, d6, d7, t0, t1, t2, t3, t4, t5, ..] = ts21;
    let date8 = [d0, d1, d2, d3, d4, d5, d6, d7];
    let time6 = [t0, t1, t2, t3, t4, t5];
    let fmt_compile =
        dt_formatted_current_date("YYYY-MM-DDThh:mm:ss+hh:mm", clk, None).unwrap_or_default();
    let exe_path = std::env::current_exe()
        .ok()
        .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
    let info = ModuleReflectionInfo {
        module_id: cur_id,
        caller_id,
        compile_date_yyyymmdd: date8,
        compile_time_hhmmss: time6,
        formatted_compile_date: fmt_compile,
        module_path: exe_path,
        module_source: src_file,
    };
    match id {
        IntrinsicId::ModuleCallerId => meta_module_caller_id(&info).into_bytes(),
        IntrinsicId::ModuleDate => meta_module_date(&info).to_vec(),
        IntrinsicId::ModuleFormattedDate => meta_module_formatted_date(&info).into_bytes(),
        IntrinsicId::ModuleId => meta_module_id(&info).into_bytes(),
        IntrinsicId::ModulePath => meta_module_path(&info).into_bytes(),
        IntrinsicId::ModuleSource => meta_module_source(&info).into_bytes(),
        _ => meta_module_time(&info).to_vec(),
    }
}

/// Evaluates `EXCEPTION-*` reflection intrinsics from the run-unit [`coblang_rt_core::exceptions::ExceptionRuntimeState`].
///
/// # Arguments
///
/// * `id` - `EXCEPTION-*` [`IntrinsicId`].
#[must_use]
fn eval_exception_reflection(id: IntrinsicId) -> Vec<u8> {
    let refl = with_global_runtime(|rt| {
        let ex = rt.exceptions();
        match ex.last_record() {
            Some(rec) => ExceptionReflectionState {
                status_name: Some(String::from(rec.code.cobol_name())),
                statement_verb: (!rec.statement.is_empty()).then(|| rec.statement.clone()),
                location: (!rec.location.is_empty()).then(|| rec.location.clone()),
                io_status: ex.last_io_status(),
                file_name: rec.file_name.clone(),
            },
            None => ExceptionReflectionState::cleared(),
        }
    });
    match id {
        IntrinsicId::ExceptionFile => meta_exception_file(&refl).into_bytes(),
        IntrinsicId::ExceptionFileN => meta_exception_file_n(&refl, NationalByteOrder::BigEndian),
        IntrinsicId::ExceptionLocation => meta_exception_location(&refl).into_bytes(),
        IntrinsicId::ExceptionLocationN => {
            meta_exception_location_n(&refl, NationalByteOrder::BigEndian)
        }
        IntrinsicId::ExceptionStatement => meta_exception_statement(&refl).to_vec(),
        _ => meta_exception_status(&refl).to_vec(),
    }
}

/// Scans a non-null raw C pointer `addr` for either `explicit_len` bytes or up to the first NUL byte (`0x00`),
/// bounded by [`MAX_CSTR_SCAN_LEN`].
///
/// # Arguments
///
/// * `addr` - Non-zero pointer address.
/// * `explicit_len` - Optional explicit byte count (`Some(n)` reads `n` bytes directly).
#[expect(
    unsafe_code,
    reason = "Scans C-ABI pointer passed to `FUNCTION CONTENT-OF` / `CONTENT-LENGTH`."
)]
fn scan_cstr_bytes(addr: usize, explicit_len: Option<usize>) -> Vec<u8> {
    let ptr = std::ptr::with_exposed_provenance::<u8>(addr);
    if let Some(n) = explicit_len {
        let bounded = n.min(MAX_CSTR_SCAN_LEN);
        // SAFETY: Caller supplied explicit `CONTENT-OF(ptr, len)` length on valid `USAGE POINTER`.
        let slice = unsafe { core::slice::from_raw_parts(ptr, bounded) };
        return slice.to_vec();
    }
    let mut out = Vec::new();
    for off in 0..MAX_CSTR_SCAN_LEN {
        // SAFETY: Scans NUL-terminated C buffer addressed by `USAGE POINTER` up to first `0x00`.
        let b = unsafe { *ptr.add(off) };
        if b == 0 {
            break;
        }
        out.push(b);
    }
    out
}

/// Pops the top [`FixedDecimal`] from `rt.decimal_stack_mut()`, formats it into pinned `arena_ring`
/// ASCII bytes (`mode == 0` for lossless `DecimalAscii` `MOVE`, `mode == 1` for `DISPLAY FUNCTION`),
/// and writes `(*out_ptr, *out_len)`.
///
/// # Arguments
///
/// * `mode` - `0` = canonical decimal ASCII (`FixedDecimal::to_string`), `1` = `DISPLAY` presentation.
/// * `abi_code` - Originating intrinsic `abi_code` (`> 0` = `IntrinsicId`, `-1` = `CONTENT-LENGTH`).
/// * `digits` - Target display digit count from `c.return_ty` (`0` if unscaled/dynamic).
/// * `scale` - Target display scale from `c.return_ty`.
/// * `signed_flag` - Non-zero when `c.return_ty` is signed.
/// * `dec_is_comma` - Non-zero when `DECIMAL-POINT IS COMMA` is active (applied when `mode == 1`).
/// * `out_ptr` - Output `*const u8` slot.
/// * `out_len` - Output `i64` byte-length slot.
///
/// # Safety
///
/// `out_ptr` and `out_len` must be non-null writable pointers.
#[expect(
    unsafe_code,
    reason = "Audited clean-room numeric intrinsic ASCII/DISPLAY formatter C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m12_format_display_dec(
    mode: i32,
    abi_code: i32,
    digits: i32,
    scale: i32,
    signed_flag: i32,
    dec_is_comma: i32,
    out_ptr: *mut *const u8,
    out_len: *mut i64,
) -> i32 {
    if out_ptr.is_null() || out_len.is_null() {
        return -1;
    }
    let val = with_global_runtime(|rt| {
        rt.decimal_stack_mut()
            .pop_decimal()
            .unwrap_or(FixedDecimal::ZERO)
    });
    let formatted = if mode == 0 {
        val.to_string().into_bytes()
    } else {
        let mut s = format_intrinsic_for_display(&val, abi_code, digits, scale, signed_flag != 0);
        if dec_is_comma != 0 {
            s = s.replace('.', ",");
        }
        s.into_bytes()
    };
    let (p, l) = {
        let mut st = lock_m12();
        pin_arena_bytes(&mut st, formatted)
    };
    // SAFETY: Validated non-null `out_ptr` and `out_len` above.
    unsafe {
        *out_ptr = p;
        *out_len = l;
    }
    0
}

/// Formats `val` according to `GnuCOBOL` / ISO `DISPLAY FUNCTION` conventions.
///
/// # Arguments
///
/// * `val` - Evaluated [`FixedDecimal`].
/// * `abi_code` - Intrinsic ABI code.
/// * `digits` - Inferred HIR digit count.
/// * `scale` - Inferred HIR scale.
/// * `signed` - Whether the inferred HIR type is signed.
#[must_use]
fn format_intrinsic_for_display(
    val: &FixedDecimal,
    abi_code: i32,
    digits: i32,
    scale: i32,
    signed: bool,
) -> String {
    let id_opt = u16::try_from(abi_code)
        .ok()
        .and_then(|c| IntrinsicId::from_abi_code(c).ok());

    if (abi_code == -1
        || id_opt.is_some_and(|id| {
            matches!(
                id,
                IntrinsicId::ByteLength
                    | IntrinsicId::Length
                    | IntrinsicId::LengthAn
                    | IntrinsicId::StoredCharLength
                    | IntrinsicId::Ord
                    | IntrinsicId::OrdMax
                    | IntrinsicId::OrdMin
                    | IntrinsicId::IntegerOfDate
                    | IntrinsicId::DateOfInteger
                    | IntrinsicId::IntegerOfDay
                    | IntrinsicId::DayOfInteger
                    | IntrinsicId::YearToYyyy
                    | IntrinsicId::DateToYyyymmdd
                    | IntrinsicId::DayToYyyyddd
                    | IntrinsicId::TestDateYyyymmdd
                    | IntrinsicId::TestDayYyyyddd
                    | IntrinsicId::TestFormattedDatetime
                    | IntrinsicId::TestNumval
                    | IntrinsicId::TestNumvalC
                    | IntrinsicId::TestNumvalF
                    | IntrinsicId::FindString
                    | IntrinsicId::Ulength
                    | IntrinsicId::Upos
                    | IntrinsicId::Usupplementary
                    | IntrinsicId::Uvalid
                    | IntrinsicId::Uwidth
            )
        }))
        && let Ok(iv) = decimal_to_i64(val)
        && iv >= 0
    {
        return format!("{iv:010}");
    }

    if let Some(IntrinsicId::Abs) = id_opt
        && let Ok(d_usize) = usize::try_from(digits)
        && d_usize > 0
        && let Ok(s_usize) = usize::try_from(scale)
    {
        return format_fixed_pic_display(val, d_usize, s_usize, signed);
    }

    if let Some(IntrinsicId::Mean) = id_opt {
        return format_padded_int_trimmed_frac(val, 8);
    }
    if let Some(IntrinsicId::Midrange) = id_opt {
        return format_padded_int_trimmed_frac(val, 9);
    }
    if let Some(IntrinsicId::PresentValue) = id_opt {
        return format_padded_int_trimmed_frac(val, 5);
    }

    strip_trailing_frac_zeros(&val.to_string())
}

/// Formats `val` with fixed `digits` and `scale` (leading `'+'`/`'-'` when `signed`).
///
/// # Arguments
///
/// * `val` - Decimal value.
/// * `digits` - Total decimal digits.
/// * `scale` - Fractional digits (`<= digits`).
/// * `signed` - Emit `'+'` / `'-'` prefix.
#[must_use]
fn format_fixed_pic_display(
    val: &FixedDecimal,
    digits: usize,
    scale: usize,
    signed: bool,
) -> String {
    let sc_i16 = i16::try_from(scale).unwrap_or(0);
    let rescaled = val
        .rescale(sc_i16, RoundingMode::NearestAwayFromZero)
        .map_or(*val, |(d, _)| d);
    let raw = rescaled.abs().to_string();
    let (int_p, frac_p) = raw.split_once('.').unwrap_or((raw.as_str(), ""));
    let int_width = digits.saturating_sub(scale);
    let sign_prefix = if val.is_negative() {
        "-"
    } else if signed {
        "+"
    } else {
        ""
    };
    if scale == 0 {
        format!("{sign_prefix}{int_p:0>int_width$}")
    } else {
        format!("{sign_prefix}{int_p:0>int_width$}.{frac_p:0<scale$}")
    }
}

/// Formats `val` with `int_width` zero-padded integer digits and trimmed fractional zeros.
///
/// # Arguments
///
/// * `val` - Decimal value.
/// * `int_width` - Zero-padded integer digit count.
#[must_use]
fn format_padded_int_trimmed_frac(val: &FixedDecimal, int_width: usize) -> String {
    let norm = strip_trailing_frac_zeros(&val.abs().to_string());
    let sign_prefix = if val.is_negative() { "-" } else { "" };
    if let Some((ip, fp)) = norm.split_once('.') {
        format!("{sign_prefix}{ip:0>int_width$}.{fp}")
    } else {
        format!("{sign_prefix}{norm:0>int_width$}")
    }
}

/// Strips redundant trailing `'0'` digits after `'.'` (and trailing `'.'` if all fractional digits were zero).
///
/// # Arguments
///
/// * `s` - Raw `FixedDecimal::to_string()` text.
#[must_use]
fn strip_trailing_frac_zeros(s: &str) -> String {
    let Some((ip, fp)) = s.split_once('.') else {
        return String::from(s);
    };
    let trimmed = fp.trim_end_matches('0');
    if trimmed.is_empty() {
        String::from(ip)
    } else {
        format!("{ip}.{trimmed}")
    }
}
