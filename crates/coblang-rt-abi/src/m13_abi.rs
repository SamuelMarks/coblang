//! Audited clean-room C-ABI entrypoints for Milestone M13 (`>>TURN`, `SET LAST EXCEPTION TO OFF`,
//! `RAISE`, `RESUME`, `DECLARATIVES` `USE AFTER EXCEPTION` specificity & re-entrancy dispatch,
//! recursive `FUNCTION-ID`/`PROGRAM-ID` frame snapshots, and NIST CCVS-85 `DB` `DEBUG-ITEM` hooks).

use std::io::{self, Write as _};
use std::sync::{Mutex, MutexGuard, OnceLock};

use coblang_rt_core::exceptions::{
    DeclarativeGuardState, ExceptionCode, ExceptionLevel, ExceptionRecord, ExceptionSeverity,
    ProgramDebugMode, ResumeTarget, TurnCheckingMode, parse_cob_set_debug,
};
use coblang_rt_core::with_global_runtime;

use crate::{read_raw_slice, write_raw_slice};

/// Saved `(address, bytes)` snapshot for recursive `LOCAL-STORAGE`, `LINKAGE`, and `PERFORM` frames.
struct SavedMemorySlot {
    /// Original global/static storage address (`usize` so `M13ProcessState` is `Send`).
    addr: usize,
    /// Captured byte contents prior to nested invocation.
    bytes: Vec<u8>,
}

/// Complete saved outer invocation state for a recursive `FUNCTION-ID` or `PROGRAM-ID` call.
struct RecursiveSaveFrame {
    /// Saved static memory buffers (`LOCAL-STORAGE`, `LINKAGE` base slots, `PERFORM` stacks).
    slots: Vec<SavedMemorySlot>,
    /// Saved outer `DecimalEvalStack` so nested `COMPUTE` / `IF` expressions do not clobber outer operands.
    saved_dec_stack: coblang_rt_core::field_ops::DecimalEvalStack,
}

/// Process-wide Milestone M13 pending-declarative, debug-latch, and recursive-frame staging state.
#[derive(Default)]
struct M13ProcessState {
    /// Pending enabled exception awaiting post-statement `CheckException` dispatch.
    pending_stmt_ec: Option<ExceptionCode>,
    /// Pre-armed ISO `"<unit>; <proc>; <line>"` location for inline `ON SIZE ERROR` statements.
    armed_stmt_location: String,
    /// Pre-armed COBOL verb name for inline `ON SIZE ERROR` statements.
    armed_stmt_verb: String,
    /// Paragraph entry transition mode (`0` = fallthrough, `1` = suppress after `PERFORM`, `2` = `SPACES` after `GO TO`).
    debug_entry_mode: u8,
    /// Active recursive invocation stack saving outer `LOCAL-STORAGE`, `LINKAGE`, `PERFORM`, and RPN decimal stack.
    recursive_frames: Vec<RecursiveSaveFrame>,
}

/// Returns the singleton [`M13ProcessState`] mutex.
fn global_m13_state() -> &'static Mutex<M13ProcessState> {
    /// Singleton M13 pending-exception and recursive-frame mutex.
    static STATE: OnceLock<Mutex<M13ProcessState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(M13ProcessState::default()))
}

/// Locks [`global_m13_state`], recovering cleanly if poisoned.
fn lock_m13() -> MutexGuard<'static, M13ProcessState> {
    global_m13_state()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Resets transient M13 process state on `cob_tidy` / test boundaries.
pub fn reset_m13_state() {
    let mut st = lock_m13();
    st.pending_stmt_ec = None;
    st.armed_stmt_location.clear();
    st.armed_stmt_verb.clear();
    st.debug_entry_mode = 0;
    st.recursive_frames.clear();
}

/// Arms the current statement's ISO location and verb name before evaluating an `ON SIZE ERROR` statement.
///
/// # Arguments
///
/// * `loc_ptr` - UTF-8 ISO location bytes.
/// * `loc_len` - Byte length of `loc_ptr`.
/// * `stmt_ptr` - UTF-8 verb bytes.
/// * `stmt_len` - Byte length of `stmt_ptr`.
///
/// # Safety
///
/// Non-zero `(loc_ptr, loc_len)` and `(stmt_ptr, stmt_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room statement location arming C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m13_set_stmt_loc(
    loc_ptr: *const u8,
    loc_len: usize,
    stmt_ptr: *const u8,
    stmt_len: usize,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let loc_s = unsafe { read_raw_slice(loc_ptr, loc_len, "coblang_rt_m13_set_stmt_loc", "loc") }
        .map_or_else(
            |_| String::new(),
            |b| String::from_utf8_lossy(b).into_owned(),
        );
    // SAFETY: Forwarded from caller preconditions.
    let stmt_s =
        unsafe { read_raw_slice(stmt_ptr, stmt_len, "coblang_rt_m13_set_stmt_loc", "stmt") }
            .map_or_else(
                |_| String::new(),
                |b| String::from_utf8_lossy(b).into_owned(),
            );
    let mut m13 = lock_m13();
    m13.armed_stmt_location = loc_s;
    m13.armed_stmt_verb = stmt_s;
    0
}

/// Records `code` into `LAST EXCEPTION` (with `statement` and `location`), and if `mark_pending_when_enabled`
/// holds and `rt.exceptions().is_enabled(code)` is `true`, latches `pending_stmt_ec` for `CheckException`.
///
/// # Arguments
///
/// * `code` - Raised [`ExceptionCode`].
/// * `statement` - Originating COBOL verb name (e.g. `"DIVIDE"`, `"MOVE"`).
/// * `location` - Formatted ISO location `"<unit>; <proc>; <line>"` (`""` falls back to `armed_stmt_location`).
/// * `mark_pending_when_enabled` - Whether enabled checking should trigger `DECLARATIVES` / fatal check at statement end.
pub fn note_runtime_exception(
    code: ExceptionCode,
    statement: &str,
    location: &str,
    mark_pending_when_enabled: bool,
) {
    let (armed_loc, armed_verb) = {
        let m13 = lock_m13();
        (m13.armed_stmt_location.clone(), m13.armed_stmt_verb.clone())
    };
    let enabled = with_global_runtime(|rt| {
        let eff_loc = if !location.is_empty() {
            location.to_owned()
        } else if !armed_loc.is_empty() {
            armed_loc
        } else {
            rt.exceptions().exception_location().to_owned()
        };
        let eff_stmt = if !statement.is_empty() {
            statement.to_owned()
        } else if !armed_verb.is_empty() {
            armed_verb
        } else {
            rt.exceptions()
                .last_record()
                .map_or("", |r| r.statement.as_str())
                .to_owned()
        };
        rt.exceptions_mut().raise(ExceptionRecord {
            code,
            statement: eff_stmt,
            location: eff_loc,
            file_name: String::new(),
        });
        rt.exceptions().is_enabled(code)
    });

    if mark_pending_when_enabled && enabled {
        let mut m13 = lock_m13();
        if m13.pending_stmt_ec.is_none() {
            m13.pending_stmt_ec = Some(code);
        }
    }
}

/// Records a non-`"00"` COBOL `FILE STATUS` into [`coblang_rt_core::exceptions::ExceptionRuntimeState`]
/// so `FUNCTION EXCEPTION-FILE`, `EXCEPTION-LOCATION`, `EXCEPTION-STATEMENT`, and `EXCEPTION-STATUS`
/// reflect the I/O condition, and arms `pending_stmt_ec` when `>>TURN` checking is enabled for it.
///
/// # Arguments
///
/// * `io_status` - Two-byte ASCII `FILE STATUS` (e.g. `*b"35"`).
/// * `file_name` - COBOL file connector name (e.g. `"TEST-FILE"`).
pub fn note_io_exception(io_status: [u8; 2], file_name: &str) {
    let Some(code) = ExceptionCode::from_io_status_bytes(io_status) else {
        return;
    };
    let (armed_loc, armed_verb) = {
        let m13 = lock_m13();
        (m13.armed_stmt_location.clone(), m13.armed_stmt_verb.clone())
    };
    let enabled = with_global_runtime(|rt| {
        let is_en = rt.exceptions().is_enabled(code);
        rt.exceptions_mut().raise_io_verbatim(
            io_status,
            ExceptionRecord {
                code,
                statement: armed_verb,
                location: armed_loc,
                file_name: String::from(file_name),
            },
        );
        is_en
    });
    if enabled {
        let mut m13 = lock_m13();
        if m13.pending_stmt_ec.is_none() {
            m13.pending_stmt_ec = Some(code);
        }
    }
}

/// Applies `>>TURN selector CHECKING {ON [WITH LOCATION] | OFF}` at runtime.
///
/// # Arguments
///
/// * `ec_abi_id` - `ExceptionCode::to_abi_id()` (`<= 0` selects `EC-ALL`).
/// * `mode_code` - `0` = `Disabled`, `1` = `Enabled`, `2` = `EnabledWithLocation`.
#[expect(unsafe_code, reason = "Audited clean-room `>>TURN` C-ABI entrypoint.")]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m13_set_turn(ec_abi_id: i32, mode_code: i32) -> i32 {
    let selector = if ec_abi_id <= 0 {
        ExceptionCode::EcAll
    } else {
        u16::try_from(ec_abi_id)
            .ok()
            .and_then(|id| ExceptionCode::from_abi_id(id).ok())
            .unwrap_or(ExceptionCode::EcAll)
    };
    let mode = TurnCheckingMode::from_abi_code(mode_code);
    with_global_runtime(|rt| rt.exceptions_mut().apply_turn(selector, mode));
    0
}

/// Executes `SET LAST EXCEPTION TO OFF`, clearing `LAST EXCEPTION` and any pending statement exception.
#[expect(
    unsafe_code,
    reason = "Audited clean-room `SET LAST EXCEPTION TO OFF` C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m13_last_exception_off() -> i32 {
    with_global_runtime(|rt| rt.exceptions_mut().clear_last_exception());
    lock_m13().pending_stmt_ec = None;
    0
}

/// Alias for [`coblang_rt_m13_last_exception_off`].
#[expect(
    unsafe_code,
    reason = "Audited clean-room `SET LAST EXCEPTION TO OFF` C-ABI alias."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m13_clear_last_exception() -> i32 {
    coblang_rt_m13_last_exception_off()
}

/// Executes `RAISE [EXCEPTION ec-name]` (`ec_abi_id <= 0` re-raises `LAST EXCEPTION`).
///
/// # Arguments
///
/// * `ec_abi_id` - Target `ExceptionCode::to_abi_id()`, or `<= 0` for bare `RAISE` (`LAST EXCEPTION`).
/// * `loc_ptr` - UTF-8 ISO exception location bytes.
/// * `loc_len` - Byte length of `loc_ptr`.
/// * `stmt_ptr` - UTF-8 statement verb bytes (`"RAISE"`).
/// * `stmt_len` - Byte length of `stmt_ptr`.
///
/// # Safety
///
/// Non-zero `(loc_ptr, loc_len)` and `(stmt_ptr, stmt_len)` must reference readable bytes.
#[expect(unsafe_code, reason = "Audited clean-room `RAISE` C-ABI entrypoint.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m13_raise(
    ec_abi_id: i32,
    loc_ptr: *const u8,
    loc_len: usize,
    stmt_ptr: *const u8,
    stmt_len: usize,
) -> i32 {
    // SAFETY: Forwarded from caller preconditions.
    let Ok(loc_b) = (unsafe { read_raw_slice(loc_ptr, loc_len, "coblang_rt_m13_raise", "loc") })
    else {
        return -1;
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(stmt_b) =
        (unsafe { read_raw_slice(stmt_ptr, stmt_len, "coblang_rt_m13_raise", "stmt") })
    else {
        return -1;
    };
    let loc_s = String::from_utf8_lossy(loc_b).into_owned();
    let stmt_s = String::from_utf8_lossy(stmt_b).into_owned();

    let raised_code = with_global_runtime(|rt| {
        if ec_abi_id <= 0 {
            let prev = rt.exceptions().last_record().cloned()?;
            rt.exceptions_mut().raise(ExceptionRecord {
                code: prev.code,
                statement: stmt_s,
                location: loc_s,
                file_name: prev.file_name,
            });
            Some(prev.code)
        } else {
            let code = u16::try_from(ec_abi_id)
                .ok()
                .and_then(|id| ExceptionCode::from_abi_id(id).ok())
                .unwrap_or(ExceptionCode::EcUserException);
            rt.exceptions_mut().raise(ExceptionRecord {
                code,
                statement: stmt_s,
                location: loc_s,
                file_name: String::new(),
            });
            Some(code)
        }
    });

    if let Some(code) = raised_code {
        lock_m13().pending_stmt_ec = Some(code);
    }
    0
}

/// Atomically pops `pending_stmt_ec` at the end of a COBOL statement (`0` when no exception occurred,
/// or `code.to_abi_id() >= 1` when an enabled/raised exception requires declarative/fatal handling)
/// and backfills `(location, statement)` on `LAST EXCEPTION` if not already set by an inner call.
///
/// # Arguments
///
/// * `loc_ptr` - UTF-8 ISO exception location `"<unit>; <proc>; <line>"`.
/// * `loc_len` - Byte length of `loc_ptr`.
/// * `stmt_ptr` - UTF-8 COBOL verb name.
/// * `stmt_len` - Byte length of `stmt_ptr`.
///
/// # Safety
///
/// Non-zero `(loc_ptr, loc_len)` and `(stmt_ptr, stmt_len)` must reference readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room post-statement exception check C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m13_take_pending_ec(
    loc_ptr: *const u8,
    loc_len: usize,
    stmt_ptr: *const u8,
    stmt_len: usize,
) -> i32 {
    let popped = lock_m13().pending_stmt_ec.take();
    let Some(code) = popped else {
        return 0;
    };
    // SAFETY: Forwarded from caller preconditions.
    let loc_s =
        unsafe { read_raw_slice(loc_ptr, loc_len, "coblang_rt_m13_take_pending_ec", "loc") }
            .map_or_else(
                |_| String::new(),
                |b| String::from_utf8_lossy(b).into_owned(),
            );
    // SAFETY: Forwarded from caller preconditions.
    let stmt_s =
        unsafe { read_raw_slice(stmt_ptr, stmt_len, "coblang_rt_m13_take_pending_ec", "stmt") }
            .map_or_else(
                |_| String::new(),
                |b| String::from_utf8_lossy(b).into_owned(),
            );

    with_global_runtime(|rt| {
        let cur = rt.exceptions().last_record().cloned();
        let (final_stmt, final_loc, final_file) = match cur {
            Some(rec) if rec.code == code => (
                if rec.statement.is_empty() {
                    stmt_s
                } else {
                    rec.statement
                },
                if rec.location.is_empty() {
                    loc_s
                } else {
                    rec.location
                },
                rec.file_name,
            ),
            _ => (stmt_s, loc_s, String::new()),
        };
        rt.exceptions_mut().raise(ExceptionRecord {
            code,
            statement: final_stmt,
            location: final_loc,
            file_name: final_file,
        });
    });

    i32::from(code.to_abi_id())
}

/// Computes the ISO 2023 specificity score (`3` = exact leaf, `2` = category, `1` = `EC-ALL`, `0` = no match)
/// of `rule_abi_id` against `raised_abi_id`.
///
/// # Arguments
///
/// * `raised_abi_id` - Raised `ExceptionCode::to_abi_id()`.
/// * `rule_abi_id` - Declarative rule `ExceptionCode::to_abi_id()` (`0` = `EC-ALL`).
#[expect(
    unsafe_code,
    reason = "Audited clean-room `USE AFTER EXCEPTION` specificity matcher C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m13_match_ec(raised_abi_id: i32, rule_abi_id: i32) -> i32 {
    let Some(raised) = u16::try_from(raised_abi_id)
        .ok()
        .and_then(|id| ExceptionCode::from_abi_id(id).ok())
    else {
        return 0;
    };
    let selector = if rule_abi_id <= 0 {
        ExceptionCode::EcAll
    } else {
        let Some(sel) = u16::try_from(rule_abi_id)
            .ok()
            .and_then(|id| ExceptionCode::from_abi_id(id).ok())
        else {
            return 0;
        };
        sel
    };
    if !raised.matches_selector(selector) {
        return 0;
    }
    match selector.level() {
        ExceptionLevel::Leaf => 3,
        ExceptionLevel::Category => 2,
        ExceptionLevel::All => 1,
    }
}

/// Attempts to enter the run-unit `DECLARATIVES` re-entrancy guard (`1` = entered, `0` = blocked nested `USE`).
#[expect(
    unsafe_code,
    reason = "Audited clean-room declarative re-entrancy guard entry C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m13_enter_decl() -> i32 {
    with_global_runtime(|rt| i32::from(rt.exceptions_mut().enter_declarative().is_ok()))
}

/// Records `RESUME [AT NEXT STATEMENT | AT proc-name]` inside an active declarative section.
///
/// # Arguments
///
/// * `kind` - `0` for `RESUME NEXT STATEMENT`, `1` for `RESUME AT proc-name`.
/// * `target_pid` - Zero-based `ProcedureId` when `kind == 1`.
#[expect(unsafe_code, reason = "Audited clean-room `RESUME` C-ABI entrypoint.")]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m13_set_resume(kind: i32, target_pid: i32) -> i32 {
    let target = if kind == 1 && target_pid >= 0 {
        ResumeTarget::ProcedureId(u32::try_from(target_pid).unwrap_or(0))
    } else {
        ResumeTarget::NextStatement
    };
    let ok = with_global_runtime(|rt| rt.exceptions_mut().set_resume_target(target).is_ok());
    if ok { 0 } else { -1 }
}

/// Completes a declarative section invocation, releasing the re-entrancy guard and returning:
/// - `0` for `RESUME NEXT STATEMENT` (or non-fatal completion without `RESUME`),
/// - `pid + 1 >= 1` for `RESUME AT proc-name`, or
/// - `-1` when a fatal exception declarative terminated without `RESUME` (caller invokes `@exit(1)`).
///
/// # Arguments
///
/// * `raised_abi_id` - `ExceptionCode::to_abi_id()` that triggered the declarative (`0` for debug declaratives).
#[expect(
    unsafe_code,
    reason = "Audited clean-room declarative epilogue C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m13_finish_decl(raised_abi_id: i32) -> i64 {
    let (resume, is_fatal, msg) = with_global_runtime(|rt| {
        let r = rt.exceptions().resume_target();
        rt.exceptions_mut().leave_declarative();
        if raised_abi_id <= 0 {
            return (ResumeTarget::NextStatement, false, String::new());
        }
        let code = u16::try_from(raised_abi_id)
            .ok()
            .and_then(|id| ExceptionCode::from_abi_id(id).ok())
            .unwrap_or(ExceptionCode::EcAll);
        let fatal = code.default_severity() == ExceptionSeverity::Fatal;
        let loc = rt.exceptions().exception_location().to_owned();
        let text = format!(
            "libcoblang: fatal exception {} ({loc}): declarative exited without RESUME\n",
            code.cobol_name()
        );
        (r, fatal, text)
    });

    match resume {
        ResumeTarget::NextStatement => 0,
        ResumeTarget::ProcedureId(pid) => i64::from(pid) + 1,
        ResumeTarget::Unset => {
            if is_fatal {
                let _ = io::stdout().flush();
                let _ = io::stderr().write_all(msg.as_bytes());
                let _ = io::stderr().flush();
                with_global_runtime(|rt| rt.set_return_code(1));
                crate::cob_tidy();
                -1
            } else {
                0
            }
        }
    }
}

/// Handles an enabled exception when no `USE AFTER EXCEPTION` declarative matched (or re-entrancy blocked it).
/// Returns `1` when fatal termination requires `@exit(1)` and `0` when non-fatal execution continues.
///
/// # Arguments
///
/// * `raised_abi_id` - Unhandled `ExceptionCode::to_abi_id()`.
/// * `force_fatal` - Non-zero to force fatal termination regardless of default severity.
#[expect(
    unsafe_code,
    reason = "Audited clean-room unhandled exception disposition C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m13_check_fatal_unhandled(
    raised_abi_id: i32,
    force_fatal: i32,
) -> i32 {
    let Some(code) = u16::try_from(raised_abi_id)
        .ok()
        .and_then(|id| ExceptionCode::from_abi_id(id).ok())
    else {
        return 0;
    };
    if force_fatal == 0 && code.default_severity() != ExceptionSeverity::Fatal {
        return 0;
    }
    let loc = with_global_runtime(|rt| {
        rt.set_return_code(1);
        rt.exceptions().exception_location().to_owned()
    });
    let _ = io::stdout().flush();
    let _ = writeln!(
        io::stderr(),
        "libcoblang: fatal exception {} at {}",
        code.cobol_name(),
        if loc.is_empty() { "<unknown>" } else { &loc }
    );
    let _ = io::stderr().flush();
    crate::cob_tidy();
    1
}

/// Alias for [`coblang_rt_m13_check_fatal_unhandled`] with `force_fatal = 0`.
///
/// # Arguments
///
/// * `raised_abi_id` - Unhandled `ExceptionCode::to_abi_id()`.
#[expect(
    unsafe_code,
    reason = "Audited clean-room unhandled exception disposition C-ABI alias."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m13_unhandled_ec(raised_abi_id: i32) -> i32 {
    coblang_rt_m13_check_fatal_unhandled(raised_abi_id, 0)
}

/// Sets or clears the single-shot `"FALL THROUGH"` debug suppression latch before `PERFORM` or `GO TO`.
///
/// # Arguments
///
/// * `flag` - Non-zero to arm suppression of the next paragraph-header `"FALL THROUGH"` hook.
#[expect(
    unsafe_code,
    reason = "Audited clean-room NIST `DB` fallthrough suppression C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m13_debug_set_suppress(flag: i32) -> i32 {
    lock_m13().debug_entry_mode = u8::try_from(flag.clamp(0, 5)).unwrap_or(0);
    0
}

/// Prepares `01 DEBUG-ITEM` in `dbg_ptr[..dbg_len]` before a NIST CCVS-85 `USE FOR DEBUGGING` hook.
/// Returns `1` (after entering the declarative re-entrancy guard) when the declarative section should
/// execute and `0` when suppressed (`COB_SET_DEBUG=N`, re-entrant declarative, or suppressed duplicate
/// `"FALL THROUGH"` after `PERFORM`).
///
/// # Arguments
///
/// * `dbg_ptr` - Mutable `01 DEBUG-ITEM` buffer pointer.
/// * `dbg_len` - Byte length of `dbg_ptr` (`>= 88`).
/// * `line_num` - 1-based source line number (`<= 0` leaves `DEBUG-LINE` spaces).
/// * `name_ptr` - `DEBUG-NAME` text pointer.
/// * `name_len` - Byte length of `name_ptr`.
/// * `sub_cnt` - Number of active subscripts (`0..=3`).
/// * `s1` - Subscript 1 value (when `sub_cnt >= 1`).
/// * `s2` - Subscript 2 value (when `sub_cnt >= 2`).
/// * `s3` - Subscript 3 value (when `sub_cnt >= 3`).
/// * `cont_ptr` - `DEBUG-CONTENTS` payload pointer (or null when `cont_len == 0`).
/// * `cont_len` - Byte length of `cont_ptr`.
///
/// # Safety
///
/// Non-zero slice pointer/length pairs must reference valid memory (`dbg_ptr` exclusively writable).
#[expect(
    unsafe_code,
    reason = "Audited clean-room NIST `DB` `DEBUG-ITEM` preparation C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m13_debug_prepare(
    dbg_ptr: *mut u8,
    dbg_len: usize,
    line_num: i32,
    name_ptr: *const u8,
    name_len: usize,
    sub_cnt: i32,
    s1: i64,
    s2: i64,
    s3: i64,
    cont_ptr: *const u8,
    cont_len: usize,
) -> i32 {
    let active = with_global_runtime(|rt| {
        if rt.exceptions().declarative_guard() == DeclarativeGuardState::InDeclarative {
            return false;
        }
        if let Some(raw_env) = rt.lookup_env("COB_SET_DEBUG") {
            rt.debug_item_mut()
                .set_runtime_switch(parse_cob_set_debug(&raw_env));
        }
        rt.debug_item()
            .is_active_for_program(ProgramDebugMode::WithDebuggingMode)
    });
    if !active {
        return 0;
    }

    // SAFETY: Forwarded from caller preconditions; `cont_vec` copied before taking mutable `dbg_slice`.
    let Ok(cont_raw) =
        (unsafe { read_raw_slice(cont_ptr, cont_len, "coblang_rt_m13_debug_prepare", "cont") })
    else {
        return 0;
    };
    let mut cont_vec = cont_raw.to_vec();

    {
        let mut m13 = lock_m13();
        if cont_vec.as_slice() == b"FALL THROUGH" {
            let mode = m13.debug_entry_mode;
            m13.debug_entry_mode = 0;
            match mode {
                1 => return 0,
                2 => cont_vec.clear(),
                3 => cont_vec = b"SORT INPUT".to_vec(),
                4 => cont_vec = b"SORT OUTPUT".to_vec(),
                5 => cont_vec = b"MERGE OUTPUT".to_vec(),
                _ => {}
            }
        } else if cont_vec.as_slice() == b"PERFORM LOOP" {
            m13.debug_entry_mode = 1;
        } else {
            m13.debug_entry_mode = 0;
        }
    }

    let line_s = if line_num > 0 {
        format!("{line_num:06}")
    } else {
        String::new()
    };
    // SAFETY: Forwarded from caller preconditions.
    let Ok(name_b) =
        (unsafe { read_raw_slice(name_ptr, name_len, "coblang_rt_m13_debug_prepare", "name") })
    else {
        return 0;
    };
    let name_s = String::from_utf8_lossy(name_b).into_owned();
    // SAFETY: Forwarded from caller preconditions (`cont_vec` already cloned if aliasing `dbg_ptr`).
    let Ok(dbg_slice) =
        (unsafe { write_raw_slice(dbg_ptr, dbg_len, "coblang_rt_m13_debug_prepare", "dbg") })
    else {
        return 0;
    };

    let clamp_sub = |v: i64| i16::try_from(v.clamp(-9999, 9999)).unwrap_or(0);
    let subs = [
        (sub_cnt >= 1).then(|| clamp_sub(s1)),
        (sub_cnt >= 2).then(|| clamp_sub(s2)),
        (sub_cnt >= 3).then(|| clamp_sub(s3)),
    ];

    with_global_runtime(|rt| {
        let _ = rt.exceptions_mut().enter_declarative();
        let di = rt.debug_item_mut();
        di.populate(&line_s, &name_s, subs, &cont_vec);
        di.write_into_buffer(dbg_slice);
    });
    1
}

/// Opens a new recursive invocation save frame for a `RECURSIVE` program or `FUNCTION-ID` unit.
#[expect(
    unsafe_code,
    reason = "Audited clean-room recursive frame prologue C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_m13_rec_frame_begin() -> i32 {
    let saved_dec_stack = with_global_runtime(|rt| std::mem::take(rt.decimal_stack_mut()));
    lock_m13().recursive_frames.push(RecursiveSaveFrame {
        slots: Vec::new(),
        saved_dec_stack,
    });
    0
}

/// Snapshots `(ptr, len)` into the active recursive save frame so outer `LOCAL-STORAGE`,
/// `LINKAGE` base pointers, and `PERFORM` stacks are restored on recursive return.
///
/// # Arguments
///
/// * `ptr` - Mutable storage global address.
/// * `len` - Byte size of `ptr`.
///
/// # Safety
///
/// When `len > 0` and `!ptr.is_null()`, `ptr` must reference `len` readable/writable static bytes.
#[expect(
    unsafe_code,
    reason = "Audited clean-room recursive slot snapshot C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m13_rec_frame_save(ptr: *mut u8, len: usize) -> i32 {
    if ptr.is_null() || len == 0 {
        return 0;
    }
    // SAFETY: Validated non-null `ptr` and `len > 0` above; caller passes static storage buffer.
    let snapshot = unsafe { core::slice::from_raw_parts(ptr.cast_const(), len) }.to_vec();
    let mut m13 = lock_m13();
    if let Some(frame) = m13.recursive_frames.last_mut() {
        frame.slots.push(SavedMemorySlot {
            addr: ptr.expose_provenance(),
            bytes: snapshot,
        });
    }
    0
}

/// Pops the active recursive save frame and restores all saved static buffers in reverse order.
///
/// # Safety
///
/// Every address previously passed to [`coblang_rt_m13_rec_frame_save`] in the matching frame
/// must remain a valid static/module global of the recorded length.
#[expect(
    unsafe_code,
    reason = "Audited clean-room recursive frame restore C-ABI entrypoint."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_m13_rec_frame_restore() -> i32 {
    let popped = lock_m13().recursive_frames.pop();
    let Some(frame) = popped else {
        return 0;
    };
    for slot in frame.slots.into_iter().rev() {
        let ptr = std::ptr::with_exposed_provenance_mut::<u8>(slot.addr);
        // SAFETY: Restores module-global static storage buffer recorded in `coblang_rt_m13_rec_frame_save`.
        let dst = unsafe { core::slice::from_raw_parts_mut(ptr, slot.bytes.len()) };
        dst.copy_from_slice(&slot.bytes);
    }
    with_global_runtime(|rt| *rt.decimal_stack_mut() = frame.saved_dec_stack);
    0
}
