//! Clean-room C-ABI entrypoints for `cob_display`, `cob_accept*`, `cob_set_environment`,
//! `cob_string_*`, `cob_unstring_*`, and `cob_inspect_*`.

use std::io::{self, BufRead, Write};

use coblang_decimal::PackedSignPolicy;
use coblang_pic::{
    InspectConvertSpec, InspectMatchKind, InspectOperand, InspectReplaceClause, InspectTallyClause,
    InspectWindowSpec, StringDelimiter, StringFlowOutcome, StringSourcePiece, UnstringDelimSpec,
    UnstringIntoSlot, UnstringMatchMode, execute_inspect_converting, execute_inspect_replacing,
    execute_inspect_tallying, execute_string, execute_unstring, format_field_for_display,
    normalize_display_operand_bytes,
};
use coblang_rt_core::accept_env::{AcceptTargetDest, EnvDisplayTarget, RuntimeAcceptSource};
use coblang_rt_core::desc_codec::{RuntimeFieldSpec, encode_field_spec};
use coblang_rt_core::exceptions::ExceptionCode;
use coblang_rt_core::with_global_runtime;

use crate::cob_field_decimal_abi::{cob_add_int, cob_get_int, cob_set_int};
use crate::cob_lifecycle_abi::{
    CobInspectAbiSession, CobStringAbiSession, CobUnstringAbiSession, StagedAbiInspectOp,
    StagedUnstringInto, with_c_abi_state,
};
use crate::cob_types::{CobField, validate_cob_field_read, validate_cob_field_write};

/// Formats and writes `fields` to `out_stream` (or `err_stream` when `to_stderr != 0`).
///
/// # Arguments
///
/// * `to_stderr` - Non-zero to write to `err_stream`, `0` for `out_stream`.
/// * `newline` - Non-zero to append `0x0A`.
/// * `fields` - Slice of `*const CobField` pointers.
/// * `out_stream` - Standard output writer.
/// * `err_stream` - Standard error writer.
///
/// # Safety
///
/// Each non-null `*const CobField` in `fields` must point to a valid readable [`CobField`].
#[expect(
    unsafe_code,
    reason = "Audited `CobField` display helper shared by `cob_display` and unit tests."
)]
pub unsafe fn cob_display_to_writers(
    to_stderr: i32,
    newline: i32,
    fields: &[*const CobField],
    out_stream: &mut dyn Write,
    err_stream: &mut dyn Write,
) {
    let target: &mut dyn Write = if to_stderr != 0 {
        err_stream
    } else {
        out_stream
    };
    for &fp in fields {
        // SAFETY: Validated via `validate_cob_field_read`.
        let field_res = unsafe { validate_cob_field_read(fp, "cob_display", "fields") };
        if let Ok((bytes, desc, _)) = field_res {
            let rendered =
                format_field_for_display(bytes, &desc).unwrap_or_else(|_| bytes.to_vec());
            let _ = target.write_all(&rendered);
        }
    }
    if newline != 0 {
        let _ = target.write_all(&[0x0A]);
    }
    let _ = target.flush();
}

/// Displays `field_count` [`CobField`] items to `stdout` or `stderr` (`cob_display`).
///
/// # Arguments
///
/// * `to_stderr` - Non-zero for `stderr`, `0` for `stdout`.
/// * `newline` - Non-zero to append a trailing newline.
/// * `field_count` - Number of `*const CobField` entries in `fields`.
/// * `fields` - Pointer to `field_count` `*const CobField` pointers, or null.
///
/// # Safety
///
/// When `field_count > 0` and `fields` is non-null, `fields` must point to `field_count`
/// `*const CobField` entries whose non-null elements reference readable [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_display` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_display(
    to_stderr: i32,
    newline: i32,
    field_count: i32,
    fields: *const *const CobField,
) {
    let slice: &[*const CobField] = if let Ok(n) = usize::try_from(field_count)
        && n > 0
        && !fields.is_null()
    {
        // SAFETY: Validated `n > 0` and non-null `fields`.
        unsafe { core::slice::from_raw_parts(fields, n) }
    } else {
        &[]
    };
    let mut out = io::stdout();
    let mut err = io::stderr();
    // SAFETY: Caller upholds per-element `CobField` validity.
    unsafe {
        cob_display_to_writers(to_stderr, newline, slice, &mut out, &mut err);
    }
}

/// Executes `source` `ACCEPT` into `f` using `stdin_reader` and `stderr_writer`.
///
/// # Arguments
///
/// * `source` - [`RuntimeAcceptSource`] selector.
/// * `f` - Destination `*mut CobField` pointer.
/// * `stdin_reader` - Buffered console input stream.
/// * `stderr_writer` - Warning stream.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`].
#[expect(
    unsafe_code,
    reason = "Shared `CobField` `ACCEPT` helper used by `cob_accept*` and deterministic tests."
)]
pub unsafe fn cob_accept_from_with_io(
    source: RuntimeAcceptSource,
    f: *mut CobField,
    stdin_reader: &mut dyn BufRead,
    stderr_writer: &mut dyn Write,
) {
    // SAFETY: Validated via `validate_cob_field_write`.
    let Ok((buf, desc, _)) = (unsafe { validate_cob_field_write(f, "cob_accept", "f") }) else {
        return;
    };
    let spec_bytes = encode_field_spec(&RuntimeFieldSpec {
        desc,
        packed_sign_rule: PackedSignPolicy::Gnu,
    });
    with_global_runtime(|rt| {
        let mut env_fn = |k: &str| std::env::var(k).ok();
        let _ = rt.execute_accept(
            source,
            AcceptTargetDest::field(buf, &spec_bytes),
            stdin_reader,
            stderr_writer,
            &mut env_fn,
        );
    });
}

/// Reads one line from `stdin` into `f` (`cob_accept`).
///
/// # Arguments
///
/// * `f` - Destination [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`].
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "Exported clean-room `cob_accept` C-ABI symbol.")]
pub unsafe extern "C" fn cob_accept(f: *mut CobField) {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut err = io::stderr();
    // SAFETY: Forwarded to null-guarded `cob_accept_from_with_io`.
    unsafe {
        cob_accept_from_with_io(RuntimeAcceptSource::ConsoleLine, f, &mut reader, &mut err);
    }
}

/// Accepts `YYMMDD` current date into `f` (`cob_accept_date`).
///
/// # Arguments
///
/// * `f` - Destination [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_accept_date` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_accept_date(f: *mut CobField) {
    let mut empty = io::empty();
    let mut err = io::stderr();
    // SAFETY: Forwarded to null-guarded `cob_accept_from_with_io`.
    unsafe {
        cob_accept_from_with_io(RuntimeAcceptSource::DateYymmdd, f, &mut empty, &mut err);
    }
}

/// Accepts `YYYYMMDD` current date into `f` (`cob_accept_date_yyyymmdd`).
///
/// # Arguments
///
/// * `f` - Destination [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_accept_date_yyyymmdd` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_accept_date_yyyymmdd(f: *mut CobField) {
    let mut empty = io::empty();
    let mut err = io::stderr();
    // SAFETY: Forwarded to null-guarded `cob_accept_from_with_io`.
    unsafe {
        cob_accept_from_with_io(RuntimeAcceptSource::DateYyyymmdd, f, &mut empty, &mut err);
    }
}

/// Accepts `YYDDD` ordinal day into `f` (`cob_accept_day`).
///
/// # Arguments
///
/// * `f` - Destination [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_accept_day` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_accept_day(f: *mut CobField) {
    let mut empty = io::empty();
    let mut err = io::stderr();
    // SAFETY: Forwarded to null-guarded `cob_accept_from_with_io`.
    unsafe {
        cob_accept_from_with_io(RuntimeAcceptSource::DayYyddd, f, &mut empty, &mut err);
    }
}

/// Accepts `YYYYDDD` ordinal day into `f` (`cob_accept_day_yyyyddd`).
///
/// # Arguments
///
/// * `f` - Destination [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_accept_day_yyyyddd` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_accept_day_yyyyddd(f: *mut CobField) {
    let mut empty = io::empty();
    let mut err = io::stderr();
    // SAFETY: Forwarded to null-guarded `cob_accept_from_with_io`.
    unsafe {
        cob_accept_from_with_io(RuntimeAcceptSource::DayYyyyddd, f, &mut empty, &mut err);
    }
}

/// Accepts ISO `1..=7` day-of-week into `f` (`cob_accept_day_of_week`).
///
/// # Arguments
///
/// * `f` - Destination [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_accept_day_of_week` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_accept_day_of_week(f: *mut CobField) {
    let mut empty = io::empty();
    let mut err = io::stderr();
    // SAFETY: Forwarded to null-guarded `cob_accept_from_with_io`.
    unsafe {
        cob_accept_from_with_io(RuntimeAcceptSource::DayOfWeek, f, &mut empty, &mut err);
    }
}

/// Accepts `HHMMSShh` wall-clock time into `f` (`cob_accept_time`).
///
/// # Arguments
///
/// * `f` - Destination [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_accept_time` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_accept_time(f: *mut CobField) {
    let mut empty = io::empty();
    let mut err = io::stderr();
    // SAFETY: Forwarded to null-guarded `cob_accept_from_with_io`.
    unsafe {
        cob_accept_from_with_io(RuntimeAcceptSource::TimeStandard8, f, &mut empty, &mut err);
    }
}

/// Accepts CLI argument count (`argc - 1`) into `f` (`cob_accept_arg_number`).
///
/// # Arguments
///
/// * `f` - Destination [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_accept_arg_number` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_accept_arg_number(f: *mut CobField) {
    let mut empty = io::empty();
    let mut err = io::stderr();
    // SAFETY: Forwarded to null-guarded `cob_accept_from_with_io`.
    unsafe {
        cob_accept_from_with_io(RuntimeAcceptSource::ArgumentNumber, f, &mut empty, &mut err);
    }
}

/// Accepts the current CLI argument value into `f` (`cob_accept_arg_value`).
///
/// # Arguments
///
/// * `f` - Destination [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_accept_arg_value` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_accept_arg_value(f: *mut CobField) {
    let mut empty = io::empty();
    let mut err = io::stderr();
    // SAFETY: Forwarded to null-guarded `cob_accept_from_with_io`.
    unsafe {
        cob_accept_from_with_io(RuntimeAcceptSource::ArgumentValue, f, &mut empty, &mut err);
    }
}

/// Accepts joined CLI arguments into `f` (`cob_accept_command_line`).
///
/// # Arguments
///
/// * `f` - Destination [`CobField`] pointer.
///
/// # Safety
///
/// Non-null `f` must point to a valid writable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_accept_command_line` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_accept_command_line(f: *mut CobField) {
    let mut empty = io::empty();
    let mut err = io::stderr();
    // SAFETY: Forwarded to null-guarded `cob_accept_from_with_io`.
    unsafe {
        cob_accept_from_with_io(RuntimeAcceptSource::CommandLine, f, &mut empty, &mut err);
    }
}

/// Accepts the environment variable named in `f` (or `ENVIRONMENT-NAME` register) back into `f` (`cob_accept_environment`).
///
/// # Arguments
///
/// * `f` - In/out [`CobField`] holding the variable name (when non-blank) and receiving its value.
///
/// # Safety
///
/// Non-null `f` must point to a valid readable/writable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_accept_environment` C-ABI symbol with null guard."
)]
pub unsafe extern "C" fn cob_accept_environment(f: *mut CobField) {
    // SAFETY: Validated via `validate_cob_field_read`.
    let Ok((cur_bytes, _, _)) =
        (unsafe { validate_cob_field_read(f.cast_const(), "cob_accept_environment", "f") })
    else {
        return;
    };
    let trimmed = String::from_utf8_lossy(cur_bytes).trim().to_owned();
    if !trimmed.is_empty() {
        with_global_runtime(|rt| {
            rt.display_staging_mut().clear();
            rt.display_staging_mut().push_raw(trimmed.as_bytes());
            rt.flush_display_to_env_target(EnvDisplayTarget::EnvironmentName);
        });
    }
    let mut empty = io::empty();
    let mut err = io::stderr();
    // SAFETY: Validated non-null `f` above.
    unsafe {
        cob_accept_from_with_io(
            RuntimeAcceptSource::EnvironmentValue,
            f,
            &mut empty,
            &mut err,
        );
    }
}

/// Sets in-process environment variable `name` to `value` (`cob_set_environment`).
///
/// # Arguments
///
/// * `name` - Environment variable name [`CobField`].
/// * `value` - Environment variable value [`CobField`].
///
/// # Safety
///
/// Non-null `name` and `value` must point to valid readable [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_set_environment` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_set_environment(name: *const CobField, value: *const CobField) {
    // SAFETY: Validated via `validate_cob_field_read`.
    let Ok((name_b, _, _)) =
        (unsafe { validate_cob_field_read(name, "cob_set_environment", "name") })
    else {
        return;
    };
    // SAFETY: Validated via `validate_cob_field_read`.
    let Ok((val_b, _, _)) =
        (unsafe { validate_cob_field_read(value, "cob_set_environment", "value") })
    else {
        return;
    };
    with_global_runtime(|rt| {
        rt.display_staging_mut().clear();
        rt.display_staging_mut().push_raw(name_b);
        rt.flush_display_to_env_target(EnvDisplayTarget::EnvironmentName);
        rt.display_staging_mut().push_raw(val_b);
        rt.flush_display_to_env_target(EnvDisplayTarget::EnvironmentValue);
    });
}

/// Reads normalized display bytes of `f` for `STRING`/`UNSTRING`/`INSPECT` operands.
///
/// # Arguments
///
/// * `f` - Candidate `*const CobField`.
/// * `sym` - Exporting C-ABI symbol name.
///
/// # Safety
///
/// Non-null `f` must point to a valid readable [`CobField`].
#[expect(
    unsafe_code,
    reason = "Audited `CobField` display normalization helper."
)]
unsafe fn read_normalized_cob_bytes(f: *const CobField, sym: &'static str) -> Option<Vec<u8>> {
    // SAFETY: Validated via `validate_cob_field_read`.
    let (raw, desc, _) = unsafe { validate_cob_field_read(f, sym, "f") }.ok()?;
    Some(normalize_display_operand_bytes(raw, Some(&desc)))
}

/// Begins a `STRING` statement targeting `dst` with optional 1-based cursor `ptr` (`cob_string_init`).
///
/// # Arguments
///
/// * `dst` - Destination `*mut CobField` (`INTO` target).
/// * `ptr` - Optional 1-based `*mut CobField` (`WITH POINTER`), or null.
///
/// # Safety
///
/// Non-null `dst` and `ptr` must remain valid until [`cob_string_finish`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_string_init` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_string_init(dst: *mut CobField, ptr: *mut CobField) {
    with_c_abi_state(|st| {
        st.string_session = CobStringAbiSession {
            dst_field_addr: dst as usize,
            ptr_field_addr: ptr as usize,
            active_delim: None,
            pieces: Vec::new(),
        };
    });
}

/// Sets the active `DELIMITED BY` pattern for subsequent [`cob_string_append`] calls (`cob_string_delimited`).
///
/// Passing null selects `DELIMITED BY SIZE`.
///
/// # Arguments
///
/// * `dlm` - Delimiter `*const CobField`, or null for `SIZE`.
///
/// # Safety
///
/// When non-null, `dlm` must point to a valid readable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_string_delimited` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_string_delimited(dlm: *const CobField) {
    let delim = if dlm.is_null() {
        None
    } else {
        // SAFETY: Validated non-null `dlm` via `read_normalized_cob_bytes`.
        unsafe { read_normalized_cob_bytes(dlm, "cob_string_delimited") }
    };
    with_c_abi_state(|st| {
        st.string_session.active_delim = delim;
    });
}

/// Appends source field `src` under the active delimiter to the staged `STRING` session (`cob_string_append`).
///
/// # Arguments
///
/// * `src` - Source `*const CobField` piece.
///
/// # Safety
///
/// When non-null, `src` must point to a valid readable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_string_append` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_string_append(src: *const CobField) {
    // SAFETY: Validated via `read_normalized_cob_bytes`.
    let Some(bytes) = (unsafe { read_normalized_cob_bytes(src, "cob_string_append") }) else {
        return;
    };
    with_c_abi_state(|st| {
        let delim = st.string_session.active_delim.clone();
        st.string_session.pieces.push((bytes, delim));
    });
}

/// Executes and finalizes the staged `STRING` session (`cob_string_finish`).
///
/// # Safety
///
/// Pointers registered in [`cob_string_init`] must still reference valid writable [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_string_finish` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_string_finish() {
    let session = with_c_abi_state(|st| core::mem::take(&mut st.string_session));
    if session.dst_field_addr == 0 {
        return;
    }
    let dst_ptr = session.dst_field_addr as *mut CobField;
    let ptr_ptr = session.ptr_field_addr as *mut CobField;
    let mut cursor = if ptr_ptr.is_null() {
        1_i64
    } else {
        // SAFETY: Caller registered valid `ptr_ptr` in `cob_string_init`.
        i64::from(unsafe { cob_get_int(ptr_ptr.cast_const()) })
    };
    // SAFETY: Caller registered valid `dst_ptr` in `cob_string_init`.
    let Ok((dst_slice, _, _)) =
        (unsafe { validate_cob_field_write(dst_ptr, "cob_string_finish", "dst") })
    else {
        return;
    };
    let mut dst_copy = dst_slice.to_vec();
    let refs: Vec<StringSourcePiece<'_>> = session
        .pieces
        .iter()
        .map(|(b, d)| StringSourcePiece {
            bytes: b.as_slice(),
            delimiter: match d {
                None => StringDelimiter::Size,
                Some(pat) => StringDelimiter::Pattern(pat.as_slice()),
            },
        })
        .collect();
    let outcome = execute_string(&refs, &mut dst_copy, &mut cursor);
    dst_slice.copy_from_slice(&dst_copy);
    if !ptr_ptr.is_null() {
        let updated = i32::try_from(cursor).unwrap_or(i32::MAX);
        // SAFETY: Validated non-null `ptr_ptr`.
        unsafe {
            cob_set_int(ptr_ptr, updated);
        }
    }
    if outcome == StringFlowOutcome::Overflowed {
        with_global_runtime(|rt| {
            rt.exceptions_mut()
                .raise_code(ExceptionCode::EcOverflowString);
        });
    }
}

/// Begins an `UNSTRING` statement over `src` with optional `ptr` (`cob_unstring_init`).
///
/// # Arguments
///
/// * `src` - Source `*const CobField` to split.
/// * `ptr` - Optional 1-based `*mut CobField` (`WITH POINTER`), or null.
/// * `num_dlm` - Expected delimiter count hint.
///
/// # Safety
///
/// Non-null `src` and `ptr` must remain valid until [`cob_unstring_finish`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_unstring_init` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_unstring_init(
    src: *const CobField,
    ptr: *mut CobField,
    num_dlm: usize,
) {
    // SAFETY: Validated via `read_normalized_cob_bytes`.
    let src_bytes =
        unsafe { read_normalized_cob_bytes(src, "cob_unstring_init") }.unwrap_or_default();
    with_c_abi_state(|st| {
        st.unstring_session = CobUnstringAbiSession {
            src_bytes,
            ptr_field_addr: ptr as usize,
            tally_field_addr: 0,
            delimiters: Vec::with_capacity(num_dlm),
            into_slots: Vec::new(),
        };
    });
}

/// Registers a `DELIMITED BY [ALL]` pattern in the active `UNSTRING` session (`cob_unstring_delimited`).
///
/// # Arguments
///
/// * `dlm` - Delimiter `*const CobField`.
/// * `all` - Non-zero for `DELIMITED BY ALL`.
///
/// # Safety
///
/// Non-null `dlm` must point to a valid readable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_unstring_delimited` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_unstring_delimited(dlm: *const CobField, all: i32) {
    // SAFETY: Validated via `read_normalized_cob_bytes`.
    let Some(pat) = (unsafe { read_normalized_cob_bytes(dlm, "cob_unstring_delimited") }) else {
        return;
    };
    with_c_abi_state(|st| {
        st.unstring_session.delimiters.push((pat, all != 0));
    });
}

/// Registers an `INTO dst [DELIMITER IN dlm] [COUNT IN cnt]` receiver (`cob_unstring_into`).
///
/// # Arguments
///
/// * `dst` - Destination `*mut CobField`.
/// * `dlm` - Optional `DELIMITER IN` `*mut CobField`, or null.
/// * `cnt` - Optional `COUNT IN` `*mut CobField`, or null.
///
/// # Safety
///
/// Non-null pointers must remain valid writable [`CobField`] descriptors until [`cob_unstring_finish`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_unstring_into` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_unstring_into(
    dst: *mut CobField,
    dlm: *mut CobField,
    cnt: *mut CobField,
) {
    if dst.is_null() {
        return;
    }
    with_c_abi_state(|st| {
        st.unstring_session.into_slots.push(StagedUnstringInto {
            dst_addr: dst as usize,
            dlm_addr: dlm as usize,
            cnt_addr: cnt as usize,
        });
    });
}

/// Registers a `TALLYING IN f` counter for the active `UNSTRING` session (`cob_unstring_tallying`).
///
/// # Arguments
///
/// * `f` - Counter `*mut CobField`, or null.
///
/// # Safety
///
/// Non-null `f` must remain a valid writable [`CobField`] until [`cob_unstring_finish`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_unstring_tallying` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_unstring_tallying(f: *mut CobField) {
    with_c_abi_state(|st| {
        st.unstring_session.tally_field_addr = f as usize;
    });
}

/// Executes and finalizes the staged `UNSTRING` session (`cob_unstring_finish`).
///
/// # Safety
///
/// All pointers staged during [`cob_unstring_init`], [`cob_unstring_into`], and [`cob_unstring_tallying`]
/// must still reference valid writable [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_unstring_finish` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_unstring_finish() {
    let session = with_c_abi_state(|st| core::mem::take(&mut st.unstring_session));
    if session.into_slots.is_empty() {
        return;
    }
    let ptr_ptr = session.ptr_field_addr as *mut CobField;
    let mut cursor = if ptr_ptr.is_null() {
        1_i64
    } else {
        // SAFETY: Validated by caller contract.
        i64::from(unsafe { cob_get_int(ptr_ptr.cast_const()) })
    };

    let dlm_specs: Vec<UnstringDelimSpec<'_>> = session
        .delimiters
        .iter()
        .map(|(pat, is_all)| UnstringDelimSpec {
            mode: if *is_all {
                UnstringMatchMode::AllRepeated
            } else {
                UnstringMatchMode::Single
            },
            pattern: pat.as_slice(),
        })
        .collect();

    let mut prepared = Vec::with_capacity(session.into_slots.len());
    for slot in &session.into_slots {
        let dst_p = slot.dst_addr as *mut CobField;
        // SAFETY: Validated via `validate_cob_field_read`.
        let Ok((dst_raw, dst_desc, _)) =
            (unsafe { validate_cob_field_read(dst_p.cast_const(), "cob_unstring_finish", "dst") })
        else {
            return;
        };
        let dlm_opt = if slot.dlm_addr == 0 {
            None
        } else {
            let dlm_p = slot.dlm_addr as *const CobField;
            // SAFETY: Validated via `validate_cob_field_read`.
            unsafe { validate_cob_field_read(dlm_p, "cob_unstring_finish", "dlm") }
                .ok()
                .map(|(b, d, _)| (b.to_vec(), d))
        };
        prepared.push((*slot, dst_raw.to_vec(), dst_desc, dlm_opt, 0_i64));
    }

    let mut tally_delta = 0_i64;
    let outcome = {
        let mut refs: Vec<UnstringIntoSlot<'_>> = prepared
            .iter_mut()
            .map(
                |(raw_slot, dst_buf, dst_desc, dlm_opt, cnt_val)| UnstringIntoSlot {
                    into_buf: dst_buf.as_mut_slice(),
                    into_desc: dst_desc,
                    delim_in: dlm_opt.as_mut().map(|(b, d)| (b.as_mut_slice(), &*d)),
                    count_out: if raw_slot.cnt_addr != 0 {
                        Some(cnt_val)
                    } else {
                        None
                    },
                },
            )
            .collect();
        execute_unstring(
            &session.src_bytes,
            &dlm_specs,
            &mut refs,
            &mut cursor,
            &mut tally_delta,
        )
    };

    // SAFETY: All `prepared` pointers were validated when staged in the current `UNSTRING` session.
    unsafe {
        write_back_unstring_slots(&prepared);
    }

    if !ptr_ptr.is_null() {
        let updated = i32::try_from(cursor).unwrap_or(i32::MAX);
        // SAFETY: Validated non-null `ptr_ptr`.
        unsafe {
            cob_set_int(ptr_ptr, updated);
        }
    }
    if session.tally_field_addr != 0 {
        let tally_p = session.tally_field_addr as *mut CobField;
        let delta_i32 = i32::try_from(tally_delta).unwrap_or(i32::MAX);
        // SAFETY: Validated non-null `tally_p`.
        unsafe {
            let _ = cob_add_int(tally_p, delta_i32, 0);
        }
    }
    if let Ok(StringFlowOutcome::Overflowed) = outcome {
        with_global_runtime(|rt| {
            rt.exceptions_mut()
                .raise_code(ExceptionCode::EcOverflowUnstring);
        });
    }
}

/// Prepared per-`INTO` receiver buffer tuple used during [`cob_unstring_finish`].
type PreparedUnstringSlot = (
    StagedUnstringInto,
    Vec<u8>,
    coblang_pic::MoveFieldDesc,
    Option<(Vec<u8>, coblang_pic::MoveFieldDesc)>,
    i64,
);

/// Writes temporary `UNSTRING` receiver buffers and counts back to their destination [`CobField`] pointers.
///
/// # Arguments
///
/// * `prepared` - Completed `INTO` receiver buffers and counts.
///
/// # Safety
///
/// Every non-zero `dst_addr`, `dlm_addr`, and `cnt_addr` in `prepared` must point to a valid writable [`CobField`].
#[expect(
    unsafe_code,
    reason = "Audited writeback helper for `cob_unstring_finish`."
)]
unsafe fn write_back_unstring_slots(prepared: &[PreparedUnstringSlot]) {
    for (raw_slot, dst_buf, _, dlm_opt, cnt_val) in prepared {
        let dst_p = raw_slot.dst_addr as *mut CobField;
        if !dst_buf.is_empty() {
            // SAFETY: `dst_p` passed `validate_cob_field_read` with `size == dst_buf.len() > 0`.
            unsafe {
                core::ptr::copy_nonoverlapping(dst_buf.as_ptr(), (*dst_p).data, dst_buf.len());
            }
        }
        if let Some((dlm_buf, _)) = dlm_opt
            && !dlm_buf.is_empty()
        {
            let dlm_p = raw_slot.dlm_addr as *mut CobField;
            // SAFETY: `dlm_p` passed `validate_cob_field_read` with `size == dlm_buf.len() > 0`.
            unsafe {
                core::ptr::copy_nonoverlapping(dlm_buf.as_ptr(), (*dlm_p).data, dlm_buf.len());
            }
        }
        if raw_slot.cnt_addr != 0 {
            let cnt_p = raw_slot.cnt_addr as *mut CobField;
            let c_i32 = i32::try_from(*cnt_val).unwrap_or(i32::MAX);
            // SAFETY: Validated non-null `cnt_p`.
            unsafe {
                cob_set_int(cnt_p, c_i32);
            }
        }
    }
}

/// Initializes an `INSPECT` session on `var` (`replacing != 0` selects `REPLACING` mode) (`cob_inspect_init`).
///
/// # Arguments
///
/// * `var` - Target `*mut CobField` to inspect/modify.
/// * `replacing` - `0` for `TALLYING`, non-zero for `REPLACING`.
///
/// # Safety
///
/// Non-null `var` must remain a valid writable [`CobField`] until [`cob_inspect_finish`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_inspect_init` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_inspect_init(var: *mut CobField, replacing: i32) {
    with_c_abi_state(|st| {
        st.inspect_session = CobInspectAbiSession {
            target_field_addr: var as usize,
            replacing_mode: replacing != 0,
            pending_before: None,
            pending_after: None,
            ops: Vec::new(),
        };
    });
}

/// Resets per-clause `BEFORE` / `AFTER` bounds before staging a new `INSPECT` sub-clause (`cob_inspect_start`).
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_inspect_start` C-ABI symbol."
)]
pub extern "C" fn cob_inspect_start() {
    with_c_abi_state(|st| {
        st.inspect_session.pending_before = None;
        st.inspect_session.pending_after = None;
    });
}

/// Sets the `BEFORE INITIAL` bound for the current `INSPECT` sub-clause (`cob_inspect_before`).
///
/// # Arguments
///
/// * `str_f` - Bound pattern `*const CobField`.
///
/// # Safety
///
/// Non-null `str_f` must point to a valid readable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_inspect_before` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_inspect_before(str_f: *const CobField) {
    // SAFETY: Validated via `read_normalized_cob_bytes`.
    let bound = unsafe { read_normalized_cob_bytes(str_f, "cob_inspect_before") };
    with_c_abi_state(|st| {
        st.inspect_session.pending_before = bound;
    });
}

/// Sets the `AFTER INITIAL` bound for the current `INSPECT` sub-clause (`cob_inspect_after`).
///
/// # Arguments
///
/// * `str_f` - Bound pattern `*const CobField`.
///
/// # Safety
///
/// Non-null `str_f` must point to a valid readable [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_inspect_after` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_inspect_after(str_f: *const CobField) {
    // SAFETY: Validated via `read_normalized_cob_bytes`.
    let bound = unsafe { read_normalized_cob_bytes(str_f, "cob_inspect_after") };
    with_c_abi_state(|st| {
        st.inspect_session.pending_after = bound;
    });
}

/// Stages a `CHARACTERS` `TALLYING` (counter `f1`) or `REPLACING BY` (replacement `f1`) clause (`cob_inspect_characters`).
///
/// # Arguments
///
/// * `f1` - Counter `*mut CobField` in `TALLYING` mode, or `BY` replacement `*mut CobField` in `REPLACING` mode.
///
/// # Safety
///
/// Non-null `f1` must point to a valid [`CobField`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_inspect_characters` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_inspect_characters(f1: *mut CobField) {
    if f1.is_null() {
        return;
    }
    let is_replacing = with_c_abi_state(|st| st.inspect_session.replacing_mode);
    if is_replacing {
        // SAFETY: Validated non-null `f1`.
        let Some(by) =
            (unsafe { read_normalized_cob_bytes(f1.cast_const(), "cob_inspect_characters") })
        else {
            return;
        };
        with_c_abi_state(|st| {
            let after = st.inspect_session.pending_after.take();
            let before = st.inspect_session.pending_before.take();
            st.inspect_session.ops.push(StagedAbiInspectOp::Replace {
                kind: InspectMatchKind::Characters,
                pattern: Vec::new(),
                by,
                after,
                before,
            });
        });
    } else {
        with_c_abi_state(|st| {
            let after = st.inspect_session.pending_after.take();
            let before = st.inspect_session.pending_before.take();
            st.inspect_session.ops.push(StagedAbiInspectOp::Tally {
                counter_field_addr: f1 as usize,
                kind: InspectMatchKind::Characters,
                pattern: Vec::new(),
                after,
                before,
            });
        });
    }
}

/// Shared helper staging `ALL` / `LEADING` / `FIRST` / `TRAILING` `INSPECT` clauses.
///
/// In `TALLYING` mode, `f1` is the counter field and `f2` is the target pattern.
/// In `REPLACING` mode, `f1` is the `BY` replacement field and `f2` is the target pattern.
///
/// # Arguments
///
/// * `kind` - [`InspectMatchKind`] variant.
/// * `f1` - Counter field (`TALLYING`) or `BY` replacement field (`REPLACING`).
/// * `f2` - Target pattern field.
///
/// # Safety
///
/// Non-null `f1` and `f2` must point to valid [`CobField`] descriptors.
#[expect(
    unsafe_code,
    reason = "Shared `cob_inspect_{all,leading,first,trailing}` staging helper."
)]
unsafe fn stage_inspect_pattern_clause(
    kind: InspectMatchKind,
    f1: *mut CobField,
    f2: *const CobField,
) {
    if f1.is_null() || f2.is_null() {
        return;
    }
    // SAFETY: Validated non-null `f2`.
    let Some(pat) = (unsafe { read_normalized_cob_bytes(f2, "cob_inspect_pattern") }) else {
        return;
    };
    let is_replacing = with_c_abi_state(|st| st.inspect_session.replacing_mode);
    if is_replacing {
        // SAFETY: Validated non-null `f1`.
        let Some(by) =
            (unsafe { read_normalized_cob_bytes(f1.cast_const(), "cob_inspect_pattern") })
        else {
            return;
        };
        with_c_abi_state(|st| {
            let after = st.inspect_session.pending_after.take();
            let before = st.inspect_session.pending_before.take();
            st.inspect_session.ops.push(StagedAbiInspectOp::Replace {
                kind,
                pattern: pat,
                by,
                after,
                before,
            });
        });
    } else {
        with_c_abi_state(|st| {
            let after = st.inspect_session.pending_after.take();
            let before = st.inspect_session.pending_before.take();
            st.inspect_session.ops.push(StagedAbiInspectOp::Tally {
                counter_field_addr: f1 as usize,
                kind,
                pattern: pat,
                after,
                before,
            });
        });
    }
}

/// Stages an `INSPECT ... ALL` sub-clause (`cob_inspect_all`).
///
/// # Arguments
///
/// * `f1` - Counter `*mut CobField` (`TALLYING`) or `BY` replacement `*mut CobField` (`REPLACING`).
/// * `f2` - Pattern `*const CobField`.
///
/// # Safety
///
/// Non-null `f1` and `f2` must point to valid [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_inspect_all` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_inspect_all(f1: *mut CobField, f2: *const CobField) {
    // SAFETY: Forwarded to null-guarded `stage_inspect_pattern_clause`.
    unsafe {
        stage_inspect_pattern_clause(InspectMatchKind::All, f1, f2);
    }
}

/// Stages an `INSPECT ... LEADING` sub-clause (`cob_inspect_leading`).
///
/// # Arguments
///
/// * `f1` - Counter `*mut CobField` (`TALLYING`) or `BY` replacement `*mut CobField` (`REPLACING`).
/// * `f2` - Pattern `*const CobField`.
///
/// # Safety
///
/// Non-null `f1` and `f2` must point to valid [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_inspect_leading` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_inspect_leading(f1: *mut CobField, f2: *const CobField) {
    // SAFETY: Forwarded to null-guarded `stage_inspect_pattern_clause`.
    unsafe {
        stage_inspect_pattern_clause(InspectMatchKind::Leading, f1, f2);
    }
}

/// Stages an `INSPECT ... FIRST` sub-clause (`cob_inspect_first`).
///
/// # Arguments
///
/// * `f1` - Counter `*mut CobField` (`TALLYING`) or `BY` replacement `*mut CobField` (`REPLACING`).
/// * `f2` - Pattern `*const CobField`.
///
/// # Safety
///
/// Non-null `f1` and `f2` must point to valid [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_inspect_first` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_inspect_first(f1: *mut CobField, f2: *const CobField) {
    // SAFETY: Forwarded to null-guarded `stage_inspect_pattern_clause`.
    unsafe {
        stage_inspect_pattern_clause(InspectMatchKind::First, f1, f2);
    }
}

/// Stages an `INSPECT ... TRAILING` sub-clause (`cob_inspect_trailing`).
///
/// # Arguments
///
/// * `f1` - Counter `*mut CobField` (`TALLYING`) or `BY` replacement `*mut CobField` (`REPLACING`).
/// * `f2` - Pattern `*const CobField`.
///
/// # Safety
///
/// Non-null `f1` and `f2` must point to valid [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_inspect_trailing` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_inspect_trailing(f1: *mut CobField, f2: *const CobField) {
    // SAFETY: Forwarded to null-guarded `stage_inspect_pattern_clause`.
    unsafe {
        stage_inspect_pattern_clause(InspectMatchKind::Trailing, f1, f2);
    }
}

/// Stages an `INSPECT ... CONVERTING f1 TO f2` sub-clause (`cob_inspect_converting`).
///
/// # Arguments
///
/// * `f1` - Source alphabet `*const CobField`.
/// * `f2` - Target alphabet `*const CobField`.
///
/// # Safety
///
/// Non-null `f1` and `f2` must point to valid readable [`CobField`] descriptors.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_inspect_converting` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_inspect_converting(f1: *const CobField, f2: *const CobField) {
    // SAFETY: Validated via `read_normalized_cob_bytes`.
    let (Some(from), Some(to)) = (unsafe {
        (
            read_normalized_cob_bytes(f1, "cob_inspect_converting"),
            read_normalized_cob_bytes(f2, "cob_inspect_converting"),
        )
    }) else {
        return;
    };
    with_c_abi_state(|st| {
        let after = st.inspect_session.pending_after.take();
        let before = st.inspect_session.pending_before.take();
        st.inspect_session.ops.push(StagedAbiInspectOp::Convert {
            from,
            to,
            after,
            before,
        });
    });
}

/// Executes all staged `TALLYING`, `REPLACING`, and `CONVERTING` clauses on the target [`CobField`] (`cob_inspect_finish`).
///
/// # Safety
///
/// Target and counter [`CobField`] pointers registered during the `INSPECT` session must remain valid.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_inspect_finish` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_inspect_finish() {
    let session = with_c_abi_state(|st| core::mem::take(&mut st.inspect_session));
    if session.target_field_addr == 0 {
        return;
    }
    let target_p = session.target_field_addr as *mut CobField;
    let mut counter_updates: Vec<(usize, i32)> = Vec::new();
    {
        // SAFETY: Validated via `validate_cob_field_write`; borrow ends at block exit before touching counters.
        let Ok((out_target, target_desc, _)) =
            (unsafe { validate_cob_field_write(target_p, "cob_inspect_finish", "var") })
        else {
            return;
        };
        let mut work = out_target.to_vec();

        for op in &session.ops {
            match op {
                StagedAbiInspectOp::Tally {
                    counter_field_addr,
                    kind,
                    pattern,
                    after,
                    before,
                } => {
                    let clause = InspectTallyClause {
                        counter_slot: 0,
                        kind: *kind,
                        pattern: InspectOperand::Bytes(pattern),
                        window: InspectWindowSpec {
                            after: after.as_deref().map(InspectOperand::Bytes),
                            before: before.as_deref().map(InspectOperand::Bytes),
                        },
                    };
                    let mut delta = [0_i64; 1];
                    execute_inspect_tallying(&work, Some(&target_desc), &[clause], &mut delta);
                    let [d0] = delta;
                    let inc = i32::try_from(d0).unwrap_or(i32::MAX);
                    counter_updates.push((*counter_field_addr, inc));
                }
                StagedAbiInspectOp::Replace {
                    kind,
                    pattern,
                    by,
                    after,
                    before,
                } => {
                    let clause = InspectReplaceClause {
                        kind: *kind,
                        pattern: InspectOperand::Bytes(pattern),
                        by: InspectOperand::Bytes(by),
                        window: InspectWindowSpec {
                            after: after.as_deref().map(InspectOperand::Bytes),
                            before: before.as_deref().map(InspectOperand::Bytes),
                        },
                    };
                    execute_inspect_replacing(&mut work, Some(&target_desc), &[clause]);
                }
                StagedAbiInspectOp::Convert {
                    from,
                    to,
                    after,
                    before,
                } => {
                    let spec = InspectConvertSpec {
                        from: InspectOperand::Bytes(from),
                        to: InspectOperand::Bytes(to),
                        window: InspectWindowSpec {
                            after: after.as_deref().map(InspectOperand::Bytes),
                            before: before.as_deref().map(InspectOperand::Bytes),
                        },
                    };
                    execute_inspect_converting(&mut work, Some(&target_desc), spec);
                }
            }
        }
        out_target.copy_from_slice(&work);
    }
    for (addr, inc) in counter_updates {
        let cnt_p = addr as *mut CobField;
        // SAFETY: Validated non-null `cnt_p` when staged.
        unsafe {
            let _ = cob_add_int(cnt_p, inc, 0);
        }
    }
}
