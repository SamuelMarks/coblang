//! Audited clean-room C-ABI entrypoints for Milestones M14 (`SEQUENTIAL`, `LINE SEQUENTIAL`, `RELATIVE`)
//! and M15 (`INDEXED` ISAM), including `LINAGE`, `DELETE FILE`, OS `flock` sharing, shell pipes,
//! `SUPPRESS WHEN`, `FILE STATUS`, and post-I/O `DECLARATIVES` dispatch.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use coblang_common::{DigitCount, FileConnectorId, FileStatusCode, FileStatusKind, Result};
use coblang_fileio::{
    AccessMode, AdvancingPhase, AdvancingSpec, AssignResolutionConfig, AssignSource,
    CloseDisposition, DeclarativePresence, EnvLookup, FileLockRegistry, FileOptionality,
    InlineClausePresence, LinageParams, LineSequentialConfig, LineSequentialEnvOptions,
    LineSequentialFileEngine, LockMode, OpenMode, PageOverflowOutcome, PostIoDispatch,
    ReadDirection, ReadLockPolicy, RecordByteLen, RecordLayout, RelativeFileConfig,
    RelativeFileEngine, RelativeSlotKey, ResolvedAssignTarget, SequentialFileConfig,
    SequentialFileEngine, StandardDevice, StartRelation, StatusClausePresence, VaryingHeaderFormat,
    VerbHandlerConfig, resolve_assign_target, resolve_post_io_action, write_status_bytes,
};
use coblang_hir::{
    HirAltKeySuppression, HirDuplicateKeyPolicy, HirFileAccessMode, HirFileLockMode,
    HirFileOptionality, HirFileOrganization, HirFileSharingMode, HirKeySegment,
    HirPrintAdvancingMode, HirRecordFormat,
};
use coblang_isam::{
    CloseDisposition as IsamCloseDisposition, CollatingId, CollatingSequence, DuplicateKeyPolicy,
    FileOptionality as IsamFileOptionality, IsamAccessMode, IsamFile, IsamOpenMode, IsamSchema,
    KeyIndexId, KeySegmentSpec, KeySpec, RecordBounds, StartRelation as IsamStartRelation,
};
use coblang_rt_core::{
    fileio_wire::{
        DECL_MASK_EXTEND, DECL_MASK_FILE, DECL_MASK_INPUT, DECL_MASK_IO, DECL_MASK_OUTPUT,
        FileConnectorWireSpec, POST_IO_CONTINUE, POST_IO_FATAL_ABORT, POST_IO_FIRE_AT_END,
        POST_IO_FIRE_AT_EOP, POST_IO_FIRE_INVALID_KEY, POST_IO_FIRE_NOT_AT_END,
        POST_IO_FIRE_NOT_AT_EOP, POST_IO_FIRE_NOT_INVALID_KEY, POST_IO_INVOKE_DECL_EXTEND,
        POST_IO_INVOKE_DECL_FILE, POST_IO_INVOKE_DECL_INPUT, POST_IO_INVOKE_DECL_IO,
        POST_IO_INVOKE_DECL_OUTPUT, WireAltKey, WireAssignKind, WireConnectorIdentity,
        decode_file_connector_spec,
    },
    with_global_runtime,
};

use crate::field_abi::read_raw_slice;

/// Bit `0` of `inline_flags`: statement has an inline `ON` (`AT END` / `INVALID KEY` / `AT EOP` / `ON EXCEPTION`) body.
pub const INLINE_FLAG_HAS_ON: i32 = 1 << 0;
/// Bit `1` of `inline_flags`: statement has an inline `NOT ON` body.
pub const INLINE_FLAG_HAS_NOT_ON: i32 = 1 << 1;

/// Monotonic nonce for temporary pipe staging files and synthetic connector handles.
static NEXT_PIPE_NONCE: AtomicU64 = AtomicU64::new(1);

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "Audited POSIX flock binding from system libc for cross-process file sharing (status 61)."
)]
unsafe extern "C" {
    /// Applies or removes an advisory lock on open file descriptor `fd`.
    ///
    /// # Arguments
    ///
    /// * `fd` - Open POSIX file descriptor.
    /// * `operation` - `LOCK_SH`, `LOCK_EX`, `LOCK_UN`, optionally OR'd with `LOCK_NB`.
    fn flock(fd: core::ffi::c_int, operation: core::ffi::c_int) -> core::ffi::c_int;
}

/// POSIX `flock` shared lock constant (`1`).
const FLOCK_SH_NB: core::ffi::c_int = 1 | 4;
/// POSIX `flock` exclusive lock constant (`2 | 4`).
const FLOCK_EX_NB: core::ffi::c_int = 2 | 4;
/// POSIX `flock` unlock constant (`8`).
const FLOCK_UN: core::ffi::c_int = 8;

/// Environment lookup adapter consulting `coblang-rt-core` `SET ENVIRONMENT` overrides before `std::env`.
struct RuntimeEnvLookup;

impl EnvLookup for RuntimeEnvLookup {
    /// Looks up `key` against active `SET ENVIRONMENT` overrides before falling back to `std::env::var`.
    ///
    /// # Arguments
    ///
    /// * `key` - Environment variable name.
    fn get_var(&self, key: &str) -> Option<String> {
        with_global_runtime(|rt| rt.lookup_env(key))
    }
}

/// Pending pipe action associated with a `LINE SEQUENTIAL` shell-pipe connector (`< cmd` or `> cmd`).
#[derive(Debug)]
enum PipeAttachment {
    /// Input pipe materialized into `temp_path` (removed on `CLOSE`).
    InputTemp {
        /// Temporary file holding captured command stdout.
        temp_path: PathBuf,
    },
    /// Output pipe buffered in `temp_path` and streamed to `sh -c command` on `CLOSE`.
    OutputCommand {
        /// Temporary file holding written lines.
        temp_path: PathBuf,
        /// Shell command string (without leading `>`).
        command: String,
    },
}

/// Underlying engine variant for a runtime file connector.
enum ConnectorEngine {
    /// `ORGANIZATION IS SEQUENTIAL`.
    Sequential(SequentialFileEngine),
    /// `ORGANIZATION IS LINE SEQUENTIAL`.
    LineSequential(LineSequentialFileEngine),
    /// `ORGANIZATION IS RELATIVE`.
    Relative(RelativeFileEngine),
    /// `ORGANIZATION IS INDEXED` (`coblang-isam` B+Tree).
    Indexed(Box<IsamRuntimeAdapter>),
}

/// Wrapper around [`IsamFile`] adding dynamic path re-binding, `SUPPRESS WHEN`, and open-mode tracking.
struct IsamRuntimeAdapter {
    /// Parsed `IsamSchema` with `AllowedFifo` enabled on suppressed alternate keys.
    schema: IsamSchema,
    /// Declared [`IsamAccessMode`].
    access_mode: IsamAccessMode,
    /// Declared [`IsamFileOptionality`].
    optionality: IsamFileOptionality,
    /// Declared alternate key metadata (original uniqueness + `SUPPRESS WHEN` rule).
    alt_specs: Vec<WireAltKey>,
    /// Active [`IsamFile`] instance (bound to `bound_path`).
    file: IsamFile,
    /// Path bound to `self.file`.
    bound_path: PathBuf,
    /// Currently open [`OpenMode`] (`None` when closed).
    open_mode: Option<OpenMode>,
    /// Whether `CLOSE ... WITH LOCK` has permanently locked this connector (`"38"`).
    closed_with_lock: bool,
    /// Active Key of Reference (`0` = primary, `1..` = alternate) for `SUPPRESS WHEN` filtering on `READ NEXT`.
    active_kor: u16,
    /// Map `(alt_key_1based, collated_alt_bytes) -> primary_key_bytes` enforcing `Unique` on non-suppressed `SUPPRESS WHEN` keys across `OUTPUT`/`I-O`/`EXTEND`.
    unsuppressed_unique_alt_map: BTreeMap<(u16, Vec<u8>), Vec<u8>>,
    /// Last recorded [`FileStatusCode`].
    last_status: FileStatusCode,
}

/// Complete live runtime state of one COBOL file connector.
struct ManagedConnector {
    /// Monotonic connector handle used by [`FileLockRegistry`].
    lock_id: FileConnectorId,
    /// Decoded wire schema.
    spec: FileConnectorWireSpec,
    /// Concrete storage engine.
    engine: ConnectorEngine,
    /// Currently active [`OpenMode`] (`None` when closed).
    active_mode: Option<OpenMode>,
    /// Last resolved filesystem path (for `UNLOCK` / lock cleanup).
    active_path: Option<PathBuf>,
    /// Held advisory OS lock file handle (released on `CLOSE`).
    os_lock_file: Option<File>,
    /// Optional shell-pipe attachment (`< cmd` / `> cmd`).
    pipe: Option<PipeAttachment>,
    /// Whether `CLOSE ... WITH LOCK` has permanently locked this connector (`"38"`).
    closed_with_lock: bool,
    /// Most recent 2-byte [`FileStatusCode`] produced on this connector.
    last_code: FileStatusCode,
}

/// Per-thread File I/O runtime table, staging registers, and declarative re-entrancy set.
struct FileIoThreadState {
    /// Shared intra-process lock registry.
    locks: FileLockRegistry,
    /// Next monotonic `FileConnectorId` assigned to newly registered connectors.
    next_id: u32,
    /// Registered connectors keyed by [`WireConnectorIdentity`].
    connectors: BTreeMap<String, ManagedConnector>,
    /// Active `(owner_unit, proc_start)` declarative frames preventing recursive self-invocation.
    active_declaratives: BTreeSet<(u32, u32)>,
    /// Staged actual record byte length from the last `READ` (`-1` when none).
    last_rec_len: i64,
    /// Staged 1-based `RELATIVE KEY` from the last sequential `READ`/`WRITE` (`-1` when none).
    last_rel_key: i64,
    /// Staged 1-based `LINAGE-COUNTER` from the last `OPEN`/`WRITE` (`-1` when none).
    last_linage_counter: i64,
    /// Last fatal `(file_name, status)` for [`coblang_rt_file_fatal_abort`].
    last_fatal: Option<(String, FileStatusCode)>,
    /// Test-only flag suppressing `std::process::exit(1)` inside [`coblang_rt_file_fatal_abort`].
    suppress_exit_for_tests: bool,
}

impl FileIoThreadState {
    /// Creates an empty [`FileIoThreadState`].
    fn new() -> Self {
        Self {
            locks: FileLockRegistry::new(),
            next_id: 1,
            connectors: BTreeMap::new(),
            active_declaratives: BTreeSet::new(),
            last_rec_len: -1,
            last_rel_key: -1,
            last_linage_counter: -1,
            last_fatal: None,
            suppress_exit_for_tests: false,
        }
    }

    /// Clears per-statement output staging registers prior to an I/O verb.
    fn clear_staging(&mut self) {
        self.last_rec_len = -1;
        self.last_rel_key = -1;
        self.last_linage_counter = -1;
    }
}

thread_local! {
    /// Thread-local COBOL File I/O connector registry.
    static FILEIO_STATE: RefCell<FileIoThreadState> = RefCell::new(FileIoThreadState::new());
}

/// Executes `f` with shared access to [`FILEIO_STATE`].
///
/// # Arguments
///
/// * `f` - Closure inspecting [`FileIoThreadState`].
fn with_fileio_state<R>(f: impl FnOnce(&FileIoThreadState) -> R) -> R {
    FILEIO_STATE.with_borrow(f)
}

/// Executes `f` with exclusive access to [`FILEIO_STATE`].
///
/// # Arguments
///
/// * `f` - Closure mutating [`FileIoThreadState`].
fn with_fileio_state_mut<R>(f: impl FnOnce(&mut FileIoThreadState) -> R) -> R {
    FILEIO_STATE.with_borrow_mut(f)
}

/// Flushes, closes, and clears all open file connectors on `STOP RUN` or `cob_tidy`.
pub fn close_all_open_connectors() {
    with_fileio_state_mut(|st| {
        for conn in st.connectors.values_mut() {
            if conn.active_mode.is_some() {
                let _ = close_managed_connector(conn, CloseDisposition::Normal, &st.locks);
            }
        }
        st.connectors.clear();
        st.active_declaratives.clear();
        st.clear_staging();
    });
}

/// Writes a single rendered `REPORT SECTION` line to open file connector `file_name`.
///
/// # Arguments
///
/// * `file_name` - COBOL `FD` connector name.
/// * `line_bytes` - Formatted report line bytes (including any `CODE` prefix, without trailing `\n`).
/// * `adv_kind` - Advancing mode (`2` = `AFTER n LINES`, `4` = `AFTER PAGE`).
/// * `adv_count` - Line advance count when `adv_kind == 2`.
pub(crate) fn write_report_line_to_open_file(
    file_name: &str,
    line_bytes: &[u8],
    adv_kind: u32,
    adv_count: i64,
) {
    with_fileio_state_mut(|st| {
        let Some(conn) = st
            .connectors
            .values_mut()
            .find(|c| c.active_mode.is_some() && c.spec.file_name.eq_ignore_ascii_case(file_name))
        else {
            return;
        };
        let mut payload = line_bytes.to_vec();
        match conn.spec.record_format {
            HirRecordFormat::Fixed => {
                let rec_len = conn.spec.max_record_bytes.max(1) as usize;
                payload.resize(rec_len, b' ');
            }
            HirRecordFormat::Varying => {
                let min_len = conn.spec.min_record_bytes as usize;
                let max_len = conn.spec.max_record_bytes.max(1) as usize;
                if payload.len() < min_len {
                    payload.resize(min_len, b' ');
                } else if payload.len() > max_len {
                    payload.truncate(max_len);
                }
            }
        }
        let eff_adv_kind = if matches!(conn.engine, ConnectorEngine::LineSequential(_))
            && adv_kind == 2
            && adv_count == 1
        {
            0
        } else {
            i32::try_from(adv_kind).unwrap_or(2)
        };
        let mut dummy_rel = -1_i64;
        let mut dummy_lc = -1_i64;
        let mut dummy_eop = false;
        let code = execute_write_on_connector(
            conn,
            &payload,
            eff_adv_kind,
            adv_count,
            0,
            &mut dummy_rel,
            &mut dummy_lc,
            &mut dummy_eop,
        );
        conn.last_code = code;
    });
}

/// Emits `libcob: <site>: warning: implicit CLOSE of <file> ('<assign>')` for all open connectors,
/// closes them, and returns `true` unless fatal process exit is suppressed for unit tests.
///
/// # Arguments
///
/// * `site_label` - Source `<file>:<line>` site string.
pub(crate) fn implicit_close_open_files_with_warning(site_label: &str) -> bool {
    with_fileio_state_mut(|st| {
        for conn in st.connectors.values_mut() {
            if conn.active_mode.is_some() {
                let assign_lbl = match &conn.spec.assign {
                    WireAssignKind::Literal(s)
                    | WireAssignKind::ExternalEnv(s)
                    | WireAssignKind::EnvOrLiteral(s) => s.as_str(),
                    WireAssignKind::DynamicBuffer
                    | WireAssignKind::DisplayDevice
                    | WireAssignKind::KeyboardDevice => conn.spec.file_name.as_str(),
                };
                let _ = writeln!(
                    io::stderr(),
                    "libcob: {site_label}: warning: implicit CLOSE of {} ('{assign_lbl}')",
                    conn.spec.file_name
                );
                let _ = close_managed_connector(conn, CloseDisposition::Normal, &st.locks);
            }
        }
        let _ = io::stderr().flush();
        st.connectors.clear();
        st.active_declaratives.clear();
        st.clear_staging();
        !st.suppress_exit_for_tests
    })
}

/// Returns the actual record byte length staged by the most recent `READ` (`-1` if none).
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI staging getter."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_file_last_rec_len() -> i64 {
    with_fileio_state(|st| st.last_rec_len)
}

/// Returns the 1-based `RELATIVE KEY` staged by the most recent `RELATIVE` `READ`/`WRITE` (`-1` if none).
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI staging getter."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_file_last_rel_key() -> i64 {
    with_fileio_state(|st| st.last_rel_key)
}

/// Returns the 1-based `LINAGE-COUNTER` staged by the most recent `OPEN`/`WRITE` (`-1` if none).
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI staging getter."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_file_last_linage_counter() -> i64 {
    with_fileio_state(|st| st.last_linage_counter)
}

/// Marks declarative `(owner_unit, proc_start)` active; returns `1` if entered or `0` if already active.
///
/// # Arguments
///
/// * `owner_unit` - Defining unit ID of the declarative section.
/// * `proc_start` - Entry `ProcedureId` of the declarative section.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI declarative guard."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_decl_enter(owner_unit: u32, proc_start: u32) -> i32 {
    with_fileio_state_mut(|st| i32::from(st.active_declaratives.insert((owner_unit, proc_start))))
}

/// Releases the active re-entrancy guard for declarative `(owner_unit, proc_start)`.
///
/// # Arguments
///
/// * `owner_unit` - Defining unit ID of the declarative section.
/// * `proc_start` - Entry `ProcedureId` of the declarative section.
#[expect(
    unsafe_code,
    reason = "Audited `#[unsafe(no_mangle)]` C-ABI declarative guard."
)]
#[unsafe(no_mangle)]
pub extern "C" fn coblang_rt_decl_leave(owner_unit: u32, proc_start: u32) -> i32 {
    with_fileio_state_mut(|st| {
        let _ = st.active_declaratives.remove(&(owner_unit, proc_start));
        0
    })
}

/// Configures whether [`coblang_rt_file_fatal_abort`] returns `1` instead of exiting the process (for unit tests).
///
/// # Arguments
///
/// * `suppress` - `true` to return `1` in-process during unit tests; `false` for normal process exit.
pub fn set_suppress_fatal_exit_for_tests(suppress: bool) {
    with_fileio_state_mut(|st| st.suppress_exit_for_tests = suppress);
}

/// Prints a clean fatal unhandled file I/O diagnostic to `stderr` and terminates the run unit (`1`).
///
/// # Arguments
///
/// * `spec_ptr` - Connector wire spec pointer (used as fallback label if `last_fatal` is empty).
/// * `spec_len` - Byte length of `spec_ptr`.
///
/// # Safety
///
/// When `spec_len > 0` and `!spec_ptr.is_null()`, `spec_ptr` must point to `spec_len` readable bytes.
#[expect(unsafe_code, reason = "Audited C-ABI fatal file I/O reporter.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_file_fatal_abort(spec_ptr: *const u8, spec_len: usize) -> i32 {
    let fallback_name = {
        // SAFETY: Caller guarantees `spec_ptr` is valid for `spec_len` bytes when non-null.
        unsafe {
            read_raw_slice(
                spec_ptr,
                spec_len,
                "coblang_rt_file_fatal_abort",
                "spec_ptr",
            )
        }
    }
    .ok()
    .and_then(|b| decode_file_connector_spec(b).ok())
    .map_or_else(|| String::from("FILE"), |s| s.file_name);

    let (fname, code, suppress) = with_fileio_state_mut(|st| {
        let (f, c) = st
            .last_fatal
            .take()
            .unwrap_or((fallback_name, FileStatusKind::PermanentError30.code()));
        (f, c, st.suppress_exit_for_tests)
    });
    let _ = writeln!(
        io::stderr(),
        "libcob: permanent file error (status = {}{}) for file '{fname}'",
        char::from(code.as_bytes().first().copied().unwrap_or(b'3')),
        char::from(code.as_bytes().get(1).copied().unwrap_or(b'0')),
    );
    crate::coblang_rt_prepare_stop_run(1);
    if !suppress {
        std::process::exit(1);
    }
    1
}

/// Updates dynamic `LINAGE` parameters on a sequential connector prior to `WRITE` (`SQ208M`).
///
/// # Arguments
///
/// * `spec_ptr` - Serialized [`FileConnectorWireSpec`] pointer.
/// * `spec_len` - Byte length of `spec_ptr`.
/// * `lin_body` - `LINAGE` body line count (`> 0`).
/// * `lin_footing` - `LINAGE` footing line (`<= 0` when omitted).
/// * `lin_top` - `LINAGE` top margin (`>= 0`).
/// * `lin_bottom` - `LINAGE` bottom margin (`>= 0`).
///
/// # Safety
///
/// When `spec_len > 0` and `!spec_ptr.is_null()`, `spec_ptr` must point to `spec_len` readable bytes.
#[expect(
    unsafe_code,
    reason = "Audited C-ABI entrypoint updating dynamic `LINAGE` parameters."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_file_set_linage(
    spec_ptr: *const u8,
    spec_len: usize,
    lin_body: i64,
    lin_footing: i64,
    lin_top: i64,
    lin_bottom: i64,
) -> i32 {
    if lin_body <= 0 {
        return 0;
    }
    // SAFETY: Validated slice bounds.
    let Ok(spec_bytes) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_file_set_linage", "spec_ptr") })
    else {
        return 0;
    };
    with_fileio_state_mut(|st| {
        let Ok(key) = ensure_connector_registered(st, spec_bytes) else {
            return 0;
        };
        if let Some(conn) = st.connectors.get_mut(&key)
            && let ConnectorEngine::Sequential(seq) = &mut conn.engine
        {
            let body = usize::try_from(lin_body).unwrap_or(1);
            let foot = (lin_footing > 0).then(|| usize::try_from(lin_footing).unwrap_or(1));
            let top = usize::try_from(lin_top.max(0)).unwrap_or(0);
            let bot = usize::try_from(lin_bottom.max(0)).unwrap_or(0);
            if let Ok(params) = LinageParams::try_new(body, foot, top, bot) {
                seq.set_next_linage_params(params);
            }
        }
        0
    })
}

/// Executes COBOL `OPEN` on a file connector and returns a `POST_IO_*` dispatch code.
///
/// # Arguments
///
/// * `spec_ptr` - Serialized [`FileConnectorWireSpec`] pointer.
/// * `spec_len` - Byte length of `spec_ptr`.
/// * `dyn_path_ptr` - Dynamic `ASSIGN TO` variable bytes (ignored unless `WireAssignKind::DynamicBuffer`).
/// * `dyn_path_len` - Byte length of `dyn_path_ptr`.
/// * `mode_code` - `OpenMode` (`0=Input`, `1=Output`, `2=Io`, `3=Extend`).
/// * `opt_code` - `HirOpenOption` (`0=Standard`, `1=NoRewind`, `2=Reversed`, `3=WithLock`, `4=ShareAll`, `5=ShareNo`, `6=ShareRead`).
/// * `lin_body` - `LINAGE` body lines (`<= 0` when no `LINAGE`).
/// * `lin_footing` - `LINAGE` footing (`<= 0` when omitted).
/// * `lin_top` - `LINAGE` top margin (`>= 0`).
/// * `lin_bottom` - `LINAGE` bottom margin (`>= 0`).
/// * `status_ptr` - Optional 2-byte `FILE STATUS` buffer pointer (or null).
/// * `decl_mask` - 5-bit `DECL_MASK_*` bitmask.
///
/// # Safety
///
/// Slice pointers must be valid for their given lengths; non-null `status_ptr` must point to 2 writable bytes.
#[expect(unsafe_code, reason = "Audited C-ABI entrypoint for COBOL `OPEN`.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_file_open(
    spec_ptr: *const u8,
    spec_len: usize,
    dyn_path_ptr: *const u8,
    dyn_path_len: usize,
    mode_code: i32,
    opt_code: i32,
    lin_body: i64,
    lin_footing: i64,
    lin_top: i64,
    lin_bottom: i64,
    status_ptr: *mut u8,
    decl_mask: i32,
) -> i32 {
    // SAFETY: Validated slice bounds.
    let Ok(spec_bytes) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_file_open", "spec_ptr") })
    else {
        return POST_IO_FATAL_ABORT;
    };
    // SAFETY: Validated slice bounds.
    let dyn_bytes = (unsafe {
        read_raw_slice(
            dyn_path_ptr,
            dyn_path_len,
            "coblang_rt_file_open",
            "dyn_path_ptr",
        )
    })
    .unwrap_or(&[]);

    let mode = decode_open_mode(mode_code);
    with_fileio_state_mut(|st| {
        st.clear_staging();
        let Ok(key) = ensure_connector_registered(st, spec_bytes) else {
            return POST_IO_FATAL_ABORT;
        };
        let locks = st.locks.clone();
        let mut out_lc: Option<i64> = None;
        let (fname, status_code) = {
            let Some(conn) = st.connectors.get_mut(&key) else {
                return POST_IO_FATAL_ABORT;
            };
            let code = execute_open_on_connector(
                conn,
                dyn_bytes,
                mode,
                opt_code,
                (lin_body, lin_footing, lin_top, lin_bottom),
                &locks,
                &mut out_lc,
            );
            conn.last_code = code;
            (conn.spec.file_name.clone(), code)
        };
        if let Some(lc) = out_lc {
            st.last_linage_counter = lc;
        }
        // SAFETY: Caller guarantees non-null `status_ptr` points to 2 writable bytes.
        unsafe { store_status_if_present(status_ptr, status_code) };
        finalize_post_io(
            st,
            &fname,
            status_code,
            Some(mode),
            0,
            0,
            !status_ptr.is_null(),
            decl_mask,
            false,
        )
    })
}

/// Executes COBOL `CLOSE` on a file connector and returns a `POST_IO_*` dispatch code.
///
/// # Arguments
///
/// * `spec_ptr` - Serialized [`FileConnectorWireSpec`] pointer.
/// * `spec_len` - Byte length of `spec_ptr`.
/// * `disp_code` - `HirCloseDisposition` (`0=Standard`, `1=WithLock`, `2=WithNoRewind`, `3=ReelUnit`, `4=ReelUnitForRemoval`).
/// * `status_ptr` - Optional 2-byte `FILE STATUS` buffer pointer (or null).
/// * `decl_mask` - 5-bit `DECL_MASK_*` bitmask.
///
/// # Safety
///
/// `spec_ptr` must point to `spec_len` readable bytes; non-null `status_ptr` must point to 2 writable bytes.
#[expect(unsafe_code, reason = "Audited C-ABI entrypoint for COBOL `CLOSE`.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_file_close(
    spec_ptr: *const u8,
    spec_len: usize,
    disp_code: i32,
    status_ptr: *mut u8,
    decl_mask: i32,
) -> i32 {
    // SAFETY: Validated slice bounds.
    let Ok(spec_bytes) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_file_close", "spec_ptr") })
    else {
        return POST_IO_FATAL_ABORT;
    };
    let disp = decode_close_disp(disp_code);

    with_fileio_state_mut(|st| {
        st.clear_staging();
        let Ok(key) = ensure_connector_registered(st, spec_bytes) else {
            return POST_IO_FATAL_ABORT;
        };
        let locks = st.locks.clone();
        let (fname, prior_mode, status_code) = {
            let Some(conn) = st.connectors.get_mut(&key) else {
                return POST_IO_FATAL_ABORT;
            };
            let pm = conn.active_mode;
            let code = close_managed_connector(conn, disp, &locks);
            conn.last_code = code;
            (conn.spec.file_name.clone(), pm, code)
        };
        // SAFETY: Caller guarantees non-null `status_ptr` points to 2 writable bytes.
        unsafe { store_status_if_present(status_ptr, status_code) };
        finalize_post_io(
            st,
            &fname,
            status_code,
            prior_mode,
            0,
            0,
            !status_ptr.is_null(),
            decl_mask,
            false,
        )
    })
}

/// Executes COBOL `READ` (sequential `NEXT`/`PREVIOUS` or random by key) and returns a `POST_IO_*` dispatch code.
///
/// # Arguments
///
/// * `spec_ptr` - Serialized [`FileConnectorWireSpec`] pointer.
/// * `spec_len` - Byte length of `spec_ptr`.
/// * `rec_ptr` - Mutable `FD` record area pointer.
/// * `rec_max_len` - Byte size of `rec_ptr`.
/// * `read_kind` - `0 = SequentialNext`, `1 = SequentialPrevious`, `2 = Random`.
/// * `key_idx` - Key index (`0 = primary/relative`, `1.. = alternate`).
/// * `key_ptr` - Optional explicit key bytes pointer (or null to extract key from `rec_ptr`).
/// * `key_len` - Byte length of `key_ptr`.
/// * `rel_key_in` - Current 1-based `RELATIVE KEY` value (used for random relative `READ`).
/// * `lock_code` - `HirReadLockPolicy` (`0=Default`, `1=WithLock`, `2=WithKeptLock`, `3=WithNoLock`, `4=IgnoreLock`).
/// * `inline_flags` - Bitmask of [`INLINE_FLAG_HAS_ON`] and [`INLINE_FLAG_HAS_NOT_ON`].
/// * `status_ptr` - Optional 2-byte `FILE STATUS` buffer pointer (or null).
/// * `decl_mask` - 5-bit `DECL_MASK_*` bitmask.
///
/// # Safety
///
/// Non-null pointers must be valid for their corresponding byte lengths.
#[expect(unsafe_code, reason = "Audited C-ABI entrypoint for COBOL `READ`.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_file_read(
    spec_ptr: *const u8,
    spec_len: usize,
    rec_ptr: *mut u8,
    rec_max_len: usize,
    read_kind: i32,
    key_idx: i32,
    key_ptr: *const u8,
    key_len: usize,
    rel_key_in: i64,
    lock_code: i32,
    inline_flags: i32,
    status_ptr: *mut u8,
    decl_mask: i32,
) -> i32 {
    if rec_max_len > 0 && rec_ptr.is_null() {
        return POST_IO_FATAL_ABORT;
    }
    // SAFETY: Validated slice bounds.
    let Ok(spec_bytes) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_file_read", "spec_ptr") })
    else {
        return POST_IO_FATAL_ABORT;
    };
    let explicit_key = if !key_ptr.is_null() && key_len > 0 {
        // SAFETY: Validated non-null `key_ptr` with `key_len > 0`.
        Some(unsafe { core::slice::from_raw_parts(key_ptr, key_len) }.to_vec())
    } else {
        None
    };
    let rec_buf = if rec_max_len > 0 {
        // SAFETY: Validated non-null `rec_ptr` for `rec_max_len` writable bytes above.
        unsafe { core::slice::from_raw_parts_mut(rec_ptr, rec_max_len) }
    } else {
        &mut []
    };

    with_fileio_state_mut(|st| {
        st.clear_staging();
        let Ok(key) = ensure_connector_registered(st, spec_bytes) else {
            return POST_IO_FATAL_ABORT;
        };
        let mut staged_len = -1_i64;
        let mut staged_rel = -1_i64;
        let (fname, cur_mode, status_code) = {
            let Some(conn) = st.connectors.get_mut(&key) else {
                return POST_IO_FATAL_ABORT;
            };
            let code = execute_read_on_connector(
                conn,
                rec_buf,
                read_kind,
                u16::try_from(key_idx.max(0)).unwrap_or(0),
                explicit_key.as_deref(),
                rel_key_in,
                decode_read_lock(lock_code),
                &mut staged_len,
                &mut staged_rel,
            );
            conn.last_code = code;
            (conn.spec.file_name.clone(), conn.active_mode, code)
        };
        st.last_rec_len = staged_len;
        st.last_rel_key = staged_rel;
        // SAFETY: Caller guarantees non-null `status_ptr` points to 2 writable bytes.
        unsafe { store_status_if_present(status_ptr, status_code) };
        let verb_flavor = if read_kind == 2 { 2 } else { 1 };
        finalize_post_io(
            st,
            &fname,
            status_code,
            cur_mode,
            verb_flavor,
            inline_flags,
            !status_ptr.is_null(),
            decl_mask,
            false,
        )
    })
}

/// Executes COBOL `WRITE` and returns a `POST_IO_*` dispatch code.
///
/// # Arguments
///
/// * `spec_ptr` - Serialized [`FileConnectorWireSpec`] pointer.
/// * `spec_len` - Byte length of `spec_ptr`.
/// * `rec_ptr` - Source `FD` record buffer pointer.
/// * `rec_len` - Active record byte length to write.
/// * `adv_kind` - `0=Default`, `1=BeforeLines`, `2=AfterLines`, `3=BeforePage`, `4=AfterPage`.
/// * `adv_count` - Line count when `adv_kind` is `1` or `2`.
/// * `rel_key_in` - Current 1-based `RELATIVE KEY` value (for random/dynamic relative `WRITE`).
/// * `handler_kind` - `0=None`, `1=EndOfPage`, `2=InvalidKey`.
/// * `inline_flags` - Bitmask of [`INLINE_FLAG_HAS_ON`] and [`INLINE_FLAG_HAS_NOT_ON`].
/// * `status_ptr` - Optional 2-byte `FILE STATUS` buffer pointer (or null).
/// * `decl_mask` - 5-bit `DECL_MASK_*` bitmask.
///
/// # Safety
///
/// Non-null pointers must be valid for their corresponding byte lengths.
#[expect(unsafe_code, reason = "Audited C-ABI entrypoint for COBOL `WRITE`.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_file_write(
    spec_ptr: *const u8,
    spec_len: usize,
    rec_ptr: *const u8,
    rec_len: usize,
    adv_kind: i32,
    adv_count: i64,
    rel_key_in: i64,
    handler_kind: i32,
    inline_flags: i32,
    status_ptr: *mut u8,
    decl_mask: i32,
) -> i32 {
    // SAFETY: Validated slice bounds.
    let Ok(spec_bytes) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_file_write", "spec_ptr") })
    else {
        return POST_IO_FATAL_ABORT;
    };
    // SAFETY: Validated slice bounds.
    let Ok(rec_slice) =
        (unsafe { read_raw_slice(rec_ptr, rec_len, "coblang_rt_file_write", "rec_ptr") })
    else {
        return POST_IO_FATAL_ABORT;
    };

    with_fileio_state_mut(|st| {
        st.clear_staging();
        let Ok(key) = ensure_connector_registered(st, spec_bytes) else {
            return POST_IO_FATAL_ABORT;
        };
        let mut staged_rel = -1_i64;
        let mut staged_lc = -1_i64;
        let mut reached_eop = false;
        let (fname, cur_mode, status_code) = {
            let Some(conn) = st.connectors.get_mut(&key) else {
                return POST_IO_FATAL_ABORT;
            };
            let code = execute_write_on_connector(
                conn,
                rec_slice,
                adv_kind,
                adv_count,
                rel_key_in,
                &mut staged_rel,
                &mut staged_lc,
                &mut reached_eop,
            );
            conn.last_code = code;
            (conn.spec.file_name.clone(), conn.active_mode, code)
        };
        st.last_rel_key = staged_rel;
        st.last_linage_counter = staged_lc;
        // SAFETY: Caller guarantees non-null `status_ptr` points to 2 writable bytes.
        unsafe { store_status_if_present(status_ptr, status_code) };
        let verb_flavor = match handler_kind {
            1 => 3,
            2 => 2,
            _ => 0,
        };
        finalize_post_io(
            st,
            &fname,
            status_code,
            cur_mode,
            verb_flavor,
            inline_flags,
            !status_ptr.is_null(),
            decl_mask,
            reached_eop,
        )
    })
}

/// Executes COBOL `REWRITE` and returns a `POST_IO_*` dispatch code.
///
/// # Arguments
///
/// * `spec_ptr` - Serialized [`FileConnectorWireSpec`] pointer.
/// * `spec_len` - Byte length of `spec_ptr`.
/// * `rec_ptr` - Replacement `FD` record buffer pointer.
/// * `rec_len` - Active record byte length.
/// * `rel_key_in` - Current 1-based `RELATIVE KEY` value (for random/dynamic relative `REWRITE`).
/// * `inline_flags` - Bitmask of [`INLINE_FLAG_HAS_ON`] and [`INLINE_FLAG_HAS_NOT_ON`].
/// * `status_ptr` - Optional 2-byte `FILE STATUS` buffer pointer (or null).
/// * `decl_mask` - 5-bit `DECL_MASK_*` bitmask.
///
/// # Safety
///
/// Non-null pointers must be valid for their corresponding byte lengths.
#[expect(unsafe_code, reason = "Audited C-ABI entrypoint for COBOL `REWRITE`.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_file_rewrite(
    spec_ptr: *const u8,
    spec_len: usize,
    rec_ptr: *const u8,
    rec_len: usize,
    rel_key_in: i64,
    inline_flags: i32,
    status_ptr: *mut u8,
    decl_mask: i32,
) -> i32 {
    // SAFETY: Validated slice bounds.
    let Ok(spec_bytes) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_file_rewrite", "spec_ptr") })
    else {
        return POST_IO_FATAL_ABORT;
    };
    // SAFETY: Validated slice bounds.
    let Ok(rec_slice) =
        (unsafe { read_raw_slice(rec_ptr, rec_len, "coblang_rt_file_rewrite", "rec_ptr") })
    else {
        return POST_IO_FATAL_ABORT;
    };

    with_fileio_state_mut(|st| {
        st.clear_staging();
        let Ok(key) = ensure_connector_registered(st, spec_bytes) else {
            return POST_IO_FATAL_ABORT;
        };
        let (fname, cur_mode, status_code) = {
            let Some(conn) = st.connectors.get_mut(&key) else {
                return POST_IO_FATAL_ABORT;
            };
            let code = execute_rewrite_on_connector(conn, rec_slice, rel_key_in);
            conn.last_code = code;
            (conn.spec.file_name.clone(), conn.active_mode, code)
        };
        // SAFETY: Caller guarantees non-null `status_ptr` points to 2 writable bytes.
        unsafe { store_status_if_present(status_ptr, status_code) };
        finalize_post_io(
            st,
            &fname,
            status_code,
            cur_mode,
            2,
            inline_flags,
            !status_ptr.is_null(),
            decl_mask,
            false,
        )
    })
}

/// Executes COBOL `DELETE file [RECORD]` and returns a `POST_IO_*` dispatch code.
///
/// # Arguments
///
/// * `spec_ptr` - Serialized [`FileConnectorWireSpec`] pointer.
/// * `spec_len` - Byte length of `spec_ptr`.
/// * `rec_ptr` - Current `FD` record buffer pointer (carries primary key for random ISAM `DELETE`).
/// * `rec_max_len` - Byte length of `rec_ptr`.
/// * `rel_key_in` - Current 1-based `RELATIVE KEY` value.
/// * `inline_flags` - Bitmask of [`INLINE_FLAG_HAS_ON`] and [`INLINE_FLAG_HAS_NOT_ON`].
/// * `status_ptr` - Optional 2-byte `FILE STATUS` buffer pointer (or null).
/// * `decl_mask` - 5-bit `DECL_MASK_*` bitmask.
///
/// # Safety
///
/// Non-null pointers must be valid for their corresponding byte lengths.
#[expect(unsafe_code, reason = "Audited C-ABI entrypoint for COBOL `DELETE`.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_file_delete(
    spec_ptr: *const u8,
    spec_len: usize,
    rec_ptr: *const u8,
    rec_max_len: usize,
    rel_key_in: i64,
    inline_flags: i32,
    status_ptr: *mut u8,
    decl_mask: i32,
) -> i32 {
    // SAFETY: Validated slice bounds.
    let Ok(spec_bytes) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_file_delete", "spec_ptr") })
    else {
        return POST_IO_FATAL_ABORT;
    };
    // SAFETY: Validated slice bounds.
    let rec_slice =
        (unsafe { read_raw_slice(rec_ptr, rec_max_len, "coblang_rt_file_delete", "rec_ptr") })
            .unwrap_or(&[]);

    with_fileio_state_mut(|st| {
        st.clear_staging();
        let Ok(key) = ensure_connector_registered(st, spec_bytes) else {
            return POST_IO_FATAL_ABORT;
        };
        let (fname, cur_mode, status_code) = {
            let Some(conn) = st.connectors.get_mut(&key) else {
                return POST_IO_FATAL_ABORT;
            };
            let code = execute_delete_on_connector(conn, rec_slice, rel_key_in);
            conn.last_code = code;
            (conn.spec.file_name.clone(), conn.active_mode, code)
        };
        // SAFETY: Caller guarantees non-null `status_ptr` points to 2 writable bytes.
        unsafe { store_status_if_present(status_ptr, status_code) };
        finalize_post_io(
            st,
            &fname,
            status_code,
            cur_mode,
            2,
            inline_flags,
            !status_ptr.is_null(),
            decl_mask,
            false,
        )
    })
}

/// Executes ISO 2023 / `GnuCOBOL` `DELETE FILE file` physical deletion and returns a `POST_IO_*` dispatch code.
///
/// # Arguments
///
/// * `spec_ptr` - Serialized [`FileConnectorWireSpec`] pointer.
/// * `spec_len` - Byte length of `spec_ptr`.
/// * `dyn_path_ptr` - Optional dynamic `ASSIGN TO` variable bytes.
/// * `dyn_path_len` - Byte length of `dyn_path_ptr`.
/// * `inline_flags` - Bitmask of [`INLINE_FLAG_HAS_ON`] and [`INLINE_FLAG_HAS_NOT_ON`].
/// * `status_ptr` - Optional 2-byte `FILE STATUS` buffer pointer (or null).
/// * `decl_mask` - 5-bit `DECL_MASK_*` bitmask.
///
/// # Safety
///
/// Non-null pointers must be valid for their corresponding byte lengths.
#[expect(
    unsafe_code,
    reason = "Audited C-ABI entrypoint for COBOL `DELETE FILE`."
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_file_delete_physical(
    spec_ptr: *const u8,
    spec_len: usize,
    dyn_path_ptr: *const u8,
    dyn_path_len: usize,
    inline_flags: i32,
    status_ptr: *mut u8,
    decl_mask: i32,
) -> i32 {
    // SAFETY: Validated slice bounds.
    let Ok(spec_bytes) = (unsafe {
        read_raw_slice(
            spec_ptr,
            spec_len,
            "coblang_rt_file_delete_physical",
            "spec_ptr",
        )
    }) else {
        return POST_IO_FATAL_ABORT;
    };
    // SAFETY: Validated slice bounds.
    let dyn_bytes = (unsafe {
        read_raw_slice(
            dyn_path_ptr,
            dyn_path_len,
            "coblang_rt_file_delete_physical",
            "dyn_path_ptr",
        )
    })
    .unwrap_or(&[]);

    with_fileio_state_mut(|st| {
        st.clear_staging();
        let Ok(key) = ensure_connector_registered(st, spec_bytes) else {
            return POST_IO_FATAL_ABORT;
        };
        let locks = st.locks.clone();
        let (fname, cur_mode, status_code) = {
            let Some(conn) = st.connectors.get_mut(&key) else {
                return POST_IO_FATAL_ABORT;
            };
            let code = execute_delete_physical(conn, dyn_bytes, &locks);
            conn.last_code = code;
            (conn.spec.file_name.clone(), conn.active_mode, code)
        };
        // SAFETY: Caller guarantees non-null `status_ptr` points to 2 writable bytes.
        unsafe { store_status_if_present(status_ptr, status_code) };
        let verb_flavor = if (inline_flags & INLINE_FLAG_HAS_ON) != 0 {
            4
        } else {
            2
        };
        finalize_post_io(
            st,
            &fname,
            status_code,
            cur_mode,
            verb_flavor,
            inline_flags,
            !status_ptr.is_null(),
            decl_mask,
            false,
        )
    })
}

/// Executes COBOL `START` (`FIRST`, `LAST`, or `KEY IS rel_op key`) and returns a `POST_IO_*` dispatch code.
///
/// # Arguments
///
/// * `spec_ptr` - Serialized [`FileConnectorWireSpec`] pointer.
/// * `spec_len` - Byte length of `spec_ptr`.
/// * `rec_ptr` - `FD` record buffer pointer (fallback key extraction source).
/// * `rec_max_len` - Byte length of `rec_ptr`.
/// * `start_kind` - `0=Equal`, `1=Greater`, `2=GreaterOrEqual`, `3=Less`, `4=LessOrEqual`, `5=First`, `6=Last`.
/// * `key_idx` - Key index (`0=primary/relative`, `1..=alternate`).
/// * `key_ptr` - Optional explicit key prefix pointer (or null).
/// * `key_len` - Byte length of `key_ptr` (partial key prefix length).
/// * `rel_key_in` - Current 1-based `RELATIVE KEY` value.
/// * `inline_flags` - Bitmask of [`INLINE_FLAG_HAS_ON`] and [`INLINE_FLAG_HAS_NOT_ON`].
/// * `status_ptr` - Optional 2-byte `FILE STATUS` buffer pointer (or null).
/// * `decl_mask` - 5-bit `DECL_MASK_*` bitmask.
///
/// # Safety
///
/// Non-null pointers must be valid for their corresponding byte lengths.
#[expect(unsafe_code, reason = "Audited C-ABI entrypoint for COBOL `START`.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_file_start(
    spec_ptr: *const u8,
    spec_len: usize,
    rec_ptr: *const u8,
    rec_max_len: usize,
    start_kind: i32,
    key_idx: i32,
    key_ptr: *const u8,
    key_len: usize,
    rel_key_in: i64,
    inline_flags: i32,
    status_ptr: *mut u8,
    decl_mask: i32,
) -> i32 {
    // SAFETY: Validated slice bounds.
    let Ok(spec_bytes) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_file_start", "spec_ptr") })
    else {
        return POST_IO_FATAL_ABORT;
    };
    // SAFETY: Validated slice bounds.
    let rec_slice =
        (unsafe { read_raw_slice(rec_ptr, rec_max_len, "coblang_rt_file_start", "rec_ptr") })
            .unwrap_or(&[]);
    let explicit_key = if !key_ptr.is_null() && key_len > 0 {
        // SAFETY: Validated non-null `key_ptr`.
        Some(unsafe { core::slice::from_raw_parts(key_ptr, key_len) })
    } else {
        None
    };

    with_fileio_state_mut(|st| {
        st.clear_staging();
        let Ok(key) = ensure_connector_registered(st, spec_bytes) else {
            return POST_IO_FATAL_ABORT;
        };
        let (fname, cur_mode, status_code) = {
            let Some(conn) = st.connectors.get_mut(&key) else {
                return POST_IO_FATAL_ABORT;
            };
            let code = execute_start_on_connector(
                conn,
                rec_slice,
                start_kind,
                u16::try_from(key_idx.max(0)).unwrap_or(0),
                explicit_key,
                key_len,
                rel_key_in,
            );
            conn.last_code = code;
            (conn.spec.file_name.clone(), conn.active_mode, code)
        };
        // SAFETY: Caller guarantees non-null `status_ptr` points to 2 writable bytes.
        unsafe { store_status_if_present(status_ptr, status_code) };
        finalize_post_io(
            st,
            &fname,
            status_code,
            cur_mode,
            2,
            inline_flags,
            !status_ptr.is_null(),
            decl_mask,
            false,
        )
    })
}

/// Executes COBOL `UNLOCK file` and returns a `POST_IO_*` dispatch code.
///
/// # Arguments
///
/// * `spec_ptr` - Serialized [`FileConnectorWireSpec`] pointer.
/// * `spec_len` - Byte length of `spec_ptr`.
/// * `status_ptr` - Optional 2-byte `FILE STATUS` buffer pointer (or null).
/// * `decl_mask` - 5-bit `DECL_MASK_*` bitmask.
///
/// # Safety
///
/// `spec_ptr` must point to `spec_len` readable bytes; non-null `status_ptr` must point to 2 writable bytes.
#[expect(unsafe_code, reason = "Audited C-ABI entrypoint for COBOL `UNLOCK`.")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coblang_rt_file_unlock(
    spec_ptr: *const u8,
    spec_len: usize,
    status_ptr: *mut u8,
    decl_mask: i32,
) -> i32 {
    // SAFETY: Validated slice bounds.
    let Ok(spec_bytes) =
        (unsafe { read_raw_slice(spec_ptr, spec_len, "coblang_rt_file_unlock", "spec_ptr") })
    else {
        return POST_IO_FATAL_ABORT;
    };

    with_fileio_state_mut(|st| {
        st.clear_staging();
        let Ok(key) = ensure_connector_registered(st, spec_bytes) else {
            return POST_IO_FATAL_ABORT;
        };
        let locks = st.locks.clone();
        let (fname, cur_mode, status_code) = {
            let Some(conn) = st.connectors.get_mut(&key) else {
                return POST_IO_FATAL_ABORT;
            };
            let code = if conn.active_mode.is_none() {
                FileStatusKind::Success00.code()
            } else {
                if let Some(p) = &conn.active_path {
                    locks.release_all_record_locks(p, conn.lock_id);
                }
                match &mut conn.engine {
                    ConnectorEngine::Relative(rel) => rel.unlock_records(),
                    _ => FileStatusKind::Success00.code(),
                }
            };
            conn.last_code = code;
            (conn.spec.file_name.clone(), conn.active_mode, code)
        };
        // SAFETY: Caller guarantees non-null `status_ptr` points to 2 writable bytes.
        unsafe { store_status_if_present(status_ptr, status_code) };
        finalize_post_io(
            st,
            &fname,
            status_code,
            cur_mode,
            0,
            0,
            !status_ptr.is_null(),
            decl_mask,
            false,
        )
    })
}

/// Lazily registers a [`ManagedConnector`] in `st` if not already present, returning its lookup key.
///
/// # Arguments
///
/// * `st` - Mutable thread file state.
/// * `spec_bytes` - Serialized [`FileConnectorWireSpec`].
fn ensure_connector_registered(st: &mut FileIoThreadState, spec_bytes: &[u8]) -> Result<String> {
    let spec = decode_file_connector_spec(spec_bytes)?;
    let key = match &spec.identity {
        WireConnectorIdentity::UnitOwned {
            owner_unit,
            owner_fc,
        } => format!("U:{owner_unit}:{owner_fc}"),
        WireConnectorIdentity::External(name) => format!("E:{name}"),
    };
    if !st.connectors.contains_key(&key) {
        let lock_id = FileConnectorId::new(st.next_id);
        st.next_id = st.next_id.saturating_add(1);
        let engine = build_connector_engine(&spec, lock_id, st.locks.clone())?;
        st.connectors.insert(
            key.clone(),
            ManagedConnector {
                lock_id,
                spec,
                engine,
                active_mode: None,
                active_path: None,
                os_lock_file: None,
                pipe: None,
                closed_with_lock: false,
                last_code: FileStatusKind::Success00.code(),
            },
        );
    }
    Ok(key)
}

/// Instantiates the concrete [`ConnectorEngine`] for `spec`.
///
/// # Arguments
///
/// * `spec` - Decoded connector wire spec.
/// * `lock_id` - Assigned [`FileConnectorId`].
/// * `locks` - Shared [`FileLockRegistry`].
fn build_connector_engine(
    spec: &FileConnectorWireSpec,
    lock_id: FileConnectorId,
    locks: FileLockRegistry,
) -> Result<ConnectorEngine> {
    let min_b = RecordByteLen::try_new(spec.min_record_bytes.max(1) as usize)?;
    let max_b =
        RecordByteLen::try_new(spec.max_record_bytes.max(spec.min_record_bytes).max(1) as usize)?;
    let rec_layout = match spec.record_format {
        HirRecordFormat::Fixed => RecordLayout::Fixed {
            record_len: max_b,
            padding_byte: spec.padding_byte,
        },
        HirRecordFormat::Varying => RecordLayout::Varying {
            min_len: min_b,
            max_len: max_b,
            header_format: VaryingHeaderFormat::FourByteBigEndian,
        },
    };
    let opt = match spec.optionality {
        HirFileOptionality::Required => FileOptionality::Required,
        HirFileOptionality::Optional => FileOptionality::Optional,
    };
    let lmode = map_lock_mode(spec.lock_mode, spec.sharing_mode);

    match spec.organization {
        HirFileOrganization::Sequential => {
            let mut cfg = SequentialFileConfig::new(&spec.file_name, lock_id, rec_layout);
            cfg.optionality = opt;
            cfg.lock_mode = lmode;
            Ok(ConnectorEngine::Sequential(
                SequentialFileEngine::with_backend(
                    cfg,
                    Box::new(coblang_fileio::OsFileSystem),
                    locks,
                ),
            ))
        }
        HirFileOrganization::LineSequential => {
            let env_opts = LineSequentialEnvOptions::from_env(&RuntimeEnvLookup);
            let mut cfg = LineSequentialConfig::new(&spec.file_name, lock_id, rec_layout);
            cfg.optionality = opt;
            cfg.trailing_spaces = env_opts.trailing_spaces;
            cfg.null_escapes = env_opts.null_escapes;
            cfg.lock_mode = lmode;
            Ok(ConnectorEngine::LineSequential(
                LineSequentialFileEngine::with_backend(
                    cfg,
                    Box::new(coblang_fileio::OsFileSystem),
                    locks,
                ),
            ))
        }
        HirFileOrganization::Relative => {
            let acc = match spec.access_mode {
                HirFileAccessMode::Sequential => AccessMode::Sequential,
                HirFileAccessMode::Random => AccessMode::Random,
                HirFileAccessMode::Dynamic => AccessMode::Dynamic,
            };
            let mut cfg = RelativeFileConfig::new(&spec.file_name, lock_id, acc, rec_layout);
            cfg.optionality = opt;
            cfg.lock_mode = lmode;
            if spec.relative_key_digits > 0 {
                cfg.key_digits = DigitCount::try_new(spec.relative_key_digits).ok();
            }
            Ok(ConnectorEngine::Relative(RelativeFileEngine::with_backend(
                cfg,
                Box::new(coblang_fileio::OsFileSystem),
                locks,
            )))
        }
        HirFileOrganization::Indexed => build_indexed_adapter(spec),
    }
}

/// Builds a boxed [`IsamRuntimeAdapter`] for an `ORGANIZATION IS INDEXED` connector.
///
/// # Arguments
///
/// * `spec` - Decoded connector wire spec.
fn build_indexed_adapter(spec: &FileConnectorWireSpec) -> Result<ConnectorEngine> {
    let bounds = match spec.record_format {
        HirRecordFormat::Fixed => RecordBounds::fixed(spec.max_record_bytes.max(1))?,
        HirRecordFormat::Varying => RecordBounds::varying(
            spec.min_record_bytes.max(1),
            spec.max_record_bytes.max(spec.min_record_bytes).max(1),
        )?,
    };
    let pk_segs = convert_isam_segments(&spec.primary_key_segments)?;
    let pk_spec = KeySpec::try_new(pk_segs, DuplicateKeyPolicy::Unique)?;
    let mut alt_specs = Vec::with_capacity(spec.alternate_keys.len());
    for ak in &spec.alternate_keys {
        let segs = convert_isam_segments(&ak.segments)?;
        let dup_pol =
            if ak.suppression.is_some() || ak.duplicates == HirDuplicateKeyPolicy::WithDuplicates {
                DuplicateKeyPolicy::AllowedFifo
            } else {
                DuplicateKeyPolicy::Unique
            };
        alt_specs.push(KeySpec::try_new(segs, dup_pol)?);
    }
    let collating = match spec.collating_ranks {
        Some(ranks) => CollatingSequence::custom(CollatingId::new(2), ranks),
        None => CollatingSequence::native(),
    };
    let schema = IsamSchema::try_new(bounds, pk_spec, alt_specs, collating)?;
    let isam_acc = match spec.access_mode {
        HirFileAccessMode::Sequential => IsamAccessMode::Sequential,
        HirFileAccessMode::Random => IsamAccessMode::Random,
        HirFileAccessMode::Dynamic => IsamAccessMode::Dynamic,
    };
    let isam_opt = match spec.optionality {
        HirFileOptionality::Required => IsamFileOptionality::Required,
        HirFileOptionality::Optional => IsamFileOptionality::Optional,
    };
    let placeholder_path = PathBuf::from(&spec.file_name);
    let file = IsamFile::new_disk(&placeholder_path, schema.clone(), isam_acc, isam_opt);
    Ok(ConnectorEngine::Indexed(Box::new(IsamRuntimeAdapter {
        schema,
        access_mode: isam_acc,
        optionality: isam_opt,
        alt_specs: spec.alternate_keys.clone(),
        file,
        bound_path: placeholder_path,
        open_mode: None,
        closed_with_lock: false,
        active_kor: 0,
        unsuppressed_unique_alt_map: BTreeMap::new(),
        last_status: FileStatusKind::Success00.code(),
    })))
}

/// Converts HIR key segments into validated [`KeySegmentSpec`]s.
///
/// # Arguments
///
/// * `segs` - HIR key segments.
fn convert_isam_segments(segs: &[HirKeySegment]) -> Result<Vec<KeySegmentSpec>> {
    if segs.is_empty() {
        return Ok(vec![KeySegmentSpec::try_new(0, 1)?]);
    }
    let mut out = Vec::with_capacity(segs.len());
    for s in segs {
        out.push(KeySegmentSpec::try_new(s.offset, s.length.max(1))?);
    }
    Ok(out)
}

/// Maps HIR lock and sharing modes to `coblang-fileio` [`LockMode`].
///
/// # Arguments
///
/// * `lock` - Declared `LOCK MODE`.
/// * `sharing` - Optional `SHARING WITH` mode.
const fn map_lock_mode(lock: HirFileLockMode, sharing: Option<HirFileSharingMode>) -> LockMode {
    if matches!(lock, HirFileLockMode::Exclusive)
        || matches!(sharing, Some(HirFileSharingMode::NoOther))
    {
        LockMode::Exclusive
    } else if matches!(lock, HirFileLockMode::Manual) {
        LockMode::Manual
    } else {
        LockMode::Automatic
    }
}

/// Executes `OPEN` against `conn`, handling path resolution, pipes, OS `flock`, and `LINAGE`.
///
/// # Arguments
///
/// * `conn` - Target managed connector.
/// * `dyn_bytes` - Dynamic `ASSIGN` variable slice.
/// * `mode` - Requested [`OpenMode`].
/// * `opt_code` - `HirOpenOption` integer code.
/// * `lin_vals` - `(body, footing, top, bottom)` `LINAGE` values.
/// * `locks` - Shared [`FileLockRegistry`].
/// * `out_lc` - Output `LINAGE-COUNTER` value when `LINAGE` is active.
fn execute_open_on_connector(
    conn: &mut ManagedConnector,
    dyn_bytes: &[u8],
    mode: OpenMode,
    opt_code: i32,
    lin_vals: (i64, i64, i64, i64),
    locks: &FileLockRegistry,
    out_lc: &mut Option<i64>,
) -> FileStatusCode {
    if conn.active_mode.is_some() {
        return FileStatusKind::AlreadyOpen41.code();
    }
    if conn.closed_with_lock {
        return FileStatusKind::ClosedWithLock38.code();
    }
    let (mut resolved_path, pipe_opt) = match resolve_connector_open_path(conn, dyn_bytes, mode) {
        Ok(pair) => pair,
        Err(code) => return code,
    };

    if matches!(conn.engine, ConnectorEngine::LineSequential(_))
        && let Ok(fresh) = build_connector_engine(&conn.spec, conn.lock_id, locks.clone())
    {
        conn.engine = fresh;
    }

    if lin_vals.0 > 0
        && let ConnectorEngine::Sequential(seq) = &mut conn.engine
    {
        let body = usize::try_from(lin_vals.0).unwrap_or(1);
        let foot = (lin_vals.1 > 0).then(|| usize::try_from(lin_vals.1).unwrap_or(1));
        let top = usize::try_from(lin_vals.2.max(0)).unwrap_or(0);
        let bot = usize::try_from(lin_vals.3.max(0)).unwrap_or(0);
        let Ok(params) = LinageParams::try_new(body, foot, top, bot) else {
            return FileStatusKind::PermanentError30.code();
        };
        seq.set_next_linage_params(params);
    }

    let wants_exclusive = conn.spec.lock_mode == HirFileLockMode::Exclusive
        || conn.spec.sharing_mode == Some(HirFileSharingMode::NoOther)
        || matches!(opt_code, 3 | 5);
    let wants_shared_only =
        conn.spec.sharing_mode == Some(HirFileSharingMode::ReadOnly) || opt_code == 6;

    if wants_shared_only && mode != OpenMode::Input {
        return FileStatusKind::OpenPermissionDenied37.code();
    }

    // Probe/acquire cross-process OS advisory lock on existing file prior to truncation (`OUTPUT`).
    let existed_before = pipe_opt.is_none() && resolved_path.exists();
    let pre_lock = if existed_before {
        match acquire_os_flock(&resolved_path, wants_exclusive) {
            Ok(f) => Some(f),
            Err(code) => return code,
        }
    } else {
        None
    };

    let mut code = match &mut conn.engine {
        ConnectorEngine::Sequential(seq) => seq.open(&resolved_path, mode),
        ConnectorEngine::LineSequential(ls) => ls.open(&resolved_path, mode),
        ConnectorEngine::Relative(rel) => rel.open(&resolved_path, mode),
        ConnectorEngine::Indexed(isam) => open_isam_adapter(
            isam,
            &mut resolved_path,
            mode,
            locks,
            conn.lock_id,
            wants_exclusive,
        ),
    };

    if is_status_success(code) {
        if !existed_before
            && pipe_opt.is_none()
            && conn.spec.optionality == HirFileOptionality::Optional
            && matches!(mode, OpenMode::Input | OpenMode::Io | OpenMode::Extend)
            && code == FileStatusKind::Success00.code()
        {
            code = FileStatusKind::SuccessOptionalFile05.code();
        }
        if matches!(opt_code, 1 | 2) {
            code = FileStatusKind::SuccessNonReel07.code();
        }
        let held_lock = if pre_lock.is_some() {
            pre_lock
        } else if pipe_opt.is_none() && resolved_path.exists() {
            match acquire_os_flock(&resolved_path, wants_exclusive) {
                Ok(f) => Some(f),
                Err(conflict) => {
                    let _ = close_engine_only(&mut conn.engine, CloseDisposition::Normal);
                    return conflict;
                }
            }
        } else {
            None
        };
        conn.active_mode = Some(mode);
        conn.active_path = Some(resolved_path);
        conn.os_lock_file = held_lock;
        conn.pipe = pipe_opt;
        if let ConnectorEngine::Sequential(seq) = &conn.engine
            && let Some(lc) = seq.linage_counter()
        {
            *out_lc = i64::try_from(lc).ok();
        }
    } else if let Some(lf) = pre_lock {
        release_os_flock(lf);
    }
    code
}

/// Resolves a connector's `ASSIGN TO` path (and optional `< cmd` / `> cmd` shell pipe) at `OPEN`.
///
/// # Arguments
///
/// * `conn` - Target connector.
/// * `dyn_bytes` - Dynamic `ASSIGN` variable bytes.
/// * `mode` - Requested [`OpenMode`].
fn resolve_connector_open_path(
    conn: &ManagedConnector,
    dyn_bytes: &[u8],
    mode: OpenMode,
) -> core::result::Result<(PathBuf, Option<PipeAttachment>), FileStatusCode> {
    let assign_src = match &conn.spec.assign {
        WireAssignKind::Literal(s) => AssignSource::Literal(s.clone()),
        WireAssignKind::ExternalEnv(s) | WireAssignKind::EnvOrLiteral(s) => {
            AssignSource::ExternalName(s.clone())
        }
        WireAssignKind::DynamicBuffer => AssignSource::DynamicVariable(dyn_bytes.to_vec()),
        WireAssignKind::DisplayDevice => AssignSource::Device(StandardDevice::DisplayStdout),
        WireAssignKind::KeyboardDevice => AssignSource::Device(StandardDevice::KeyboardStdin),
    };
    let resolved = resolve_assign_target(
        &conn.spec.file_name,
        &assign_src,
        AssignResolutionConfig::default(),
        &RuntimeEnvLookup,
    )
    .map_err(|_| FileStatusKind::PermanentError30.code())?;

    let raw_path = match resolved.target {
        ResolvedAssignTarget::FilesystemPath(p) => p,
        ResolvedAssignTarget::StandardDevice(
            StandardDevice::DisplayStdout | StandardDevice::SyserrStderr,
        ) => PathBuf::from("/dev/stdout"),
        ResolvedAssignTarget::StandardDevice(StandardDevice::KeyboardStdin) => {
            PathBuf::from("/dev/stdin")
        }
    };

    if conn.spec.organization == HirFileOrganization::LineSequential
        && let Some(txt) = raw_path.to_str()
    {
        let trimmed = txt.trim_start();
        if let Some(cmd_raw) = trimmed.strip_prefix('<') {
            if mode != OpenMode::Input {
                return Err(FileStatusKind::OpenPermissionDenied37.code());
            }
            let cmd = cmd_raw.trim();
            let out = Command::new("sh")
                .args(["-c", cmd])
                .output()
                .map_err(|_| FileStatusKind::PermanentError30.code())?;
            let tmp = make_temp_pipe_path("in");
            fs::write(&tmp, &out.stdout).map_err(|_| FileStatusKind::PermanentError30.code())?;
            return Ok((
                tmp.clone(),
                Some(PipeAttachment::InputTemp { temp_path: tmp }),
            ));
        } else if let Some(cmd_raw) = trimmed.strip_prefix('>') {
            if !matches!(mode, OpenMode::Output | OpenMode::Extend) {
                return Err(FileStatusKind::OpenPermissionDenied37.code());
            }
            let cmd = cmd_raw.trim_start_matches('>').trim();
            let tmp = make_temp_pipe_path("out");
            return Ok((
                tmp.clone(),
                Some(PipeAttachment::OutputCommand {
                    temp_path: tmp,
                    command: String::from(cmd),
                }),
            ));
        }
    }
    Ok((raw_path, None))
}

/// Generates a unique temporary file path for shell-pipe staging.
///
/// # Arguments
///
/// * `tag` - Direction tag (`"in"` or `"out"`).
fn make_temp_pipe_path(tag: &str) -> PathBuf {
    let n = NEXT_PIPE_NONCE.fetch_add(1, AtomicOrdering::Relaxed);
    std::env::temp_dir().join(format!("coblang-pipe-{tag}-{}-{n}.tmp", std::process::id()))
}

/// Acquires a non-blocking POSIX advisory `flock` (`EX` or `SH`) on `path`, returning `"61"` on contention.
///
/// # Arguments
///
/// * `path` - Target filesystem path.
/// * `exclusive` - `true` for `LOCK_EX | LOCK_NB`, `false` for `LOCK_SH | LOCK_NB`.
#[expect(
    unsafe_code,
    reason = "Audited POSIX flock call on owned open File descriptor."
)]
fn acquire_os_flock(path: &Path, exclusive: bool) -> core::result::Result<File, FileStatusCode> {
    let Ok(f) = OpenOptions::new().read(true).open(path) else {
        return Err(FileStatusKind::OpenPermissionDenied37.code());
    };
    #[cfg(unix)]
    {
        let op = if exclusive { FLOCK_EX_NB } else { FLOCK_SH_NB };
        // SAFETY: `f.as_raw_fd()` is a valid open file descriptor owned by `f`.
        let rc = unsafe { flock(f.as_raw_fd(), op) };
        if rc != 0 {
            return Err(FileStatusKind::FileSharingConflict61.code());
        }
    }
    #[cfg(not(unix))]
    let _ = exclusive;
    Ok(f)
}

/// Releases an advisory POSIX `flock` and closes `file`.
///
/// # Arguments
///
/// * `file` - Held lock [`File`].
#[expect(
    unsafe_code,
    reason = "Audited POSIX flock unlock call on owned open File descriptor."
)]
fn release_os_flock(file: File) {
    #[cfg(unix)]
    {
        // SAFETY: `file.as_raw_fd()` is a valid open file descriptor.
        let _ = unsafe { flock(file.as_raw_fd(), FLOCK_UN) };
    }
    drop(file);
}

/// Opens an [`IsamRuntimeAdapter`], rebinding its target path if closed and registering intra-process locks.
///
/// # Arguments
///
/// * `isam` - Mutable ISAM adapter.
/// * `path` - Resolved target path.
/// * `mode` - Requested [`OpenMode`].
/// * `locks` - Intra-process [`FileLockRegistry`].
/// * `lock_id` - Connector handle.
/// * `exclusive` - Whether exclusive lock mode is active.
fn open_isam_adapter(
    isam: &mut IsamRuntimeAdapter,
    path: &mut PathBuf,
    mode: OpenMode,
    locks: &FileLockRegistry,
    lock_id: FileConnectorId,
    exclusive: bool,
) -> FileStatusCode {
    if !isam.file.is_open() && !isam.closed_with_lock && isam.bound_path != *path {
        isam.bound_path.clone_from(path);
        isam.file = IsamFile::new_disk(
            &isam.bound_path,
            isam.schema.clone(),
            isam.access_mode,
            isam.optionality,
        );
    }
    let reg_mode = if exclusive {
        LockMode::Exclusive
    } else {
        LockMode::Automatic
    };
    if let coblang_fileio::LockOutcome::Denied(kind) = locks.register_open(path, lock_id, reg_mode)
    {
        return kind.code();
    }
    let needs_unique_sup_cache = matches!(mode, OpenMode::Output | OpenMode::Io | OpenMode::Extend)
        && isam
            .alt_specs
            .iter()
            .any(|a| a.suppression.is_some() && a.duplicates == HirDuplicateKeyPolicy::Unique);
    if needs_unique_sup_cache {
        seed_unsuppressed_unique_map(isam, path, mode);
    }

    let isam_mode = match mode {
        OpenMode::Input => IsamOpenMode::Input,
        OpenMode::Output => IsamOpenMode::Output,
        OpenMode::Io => IsamOpenMode::Io,
        OpenMode::Extend => IsamOpenMode::Extend,
    };
    if let Ok(kind) = isam.file.open(isam_mode) {
        let code = kind.code();
        if is_status_success(code) {
            isam.open_mode = Some(mode);
            isam.active_kor = 0;
        } else {
            locks.unregister_close(path, lock_id);
        }
        isam.last_status = code;
        code
    } else {
        locks.unregister_close(path, lock_id);
        let code = FileStatusKind::PermanentError30.code();
        isam.last_status = code;
        code
    }
}

/// Closes `conn`, flushing output pipes and releasing OS and intra-process locks.
///
/// # Arguments
///
/// * `conn` - Target connector.
/// * `disp` - Requested [`CloseDisposition`].
/// * `locks` - Shared [`FileLockRegistry`].
fn close_managed_connector(
    conn: &mut ManagedConnector,
    disp: CloseDisposition,
    locks: &FileLockRegistry,
) -> FileStatusCode {
    if conn.active_mode.is_none() {
        return FileStatusKind::NotOpenOnClose42.code();
    }
    let code = match &mut conn.engine {
        ConnectorEngine::Sequential(seq) => seq.close(disp),
        ConnectorEngine::LineSequential(ls) => ls.close(disp),
        ConnectorEngine::Relative(rel) => rel.close(disp),
        ConnectorEngine::Indexed(isam) => {
            if disp == CloseDisposition::ReelUnit {
                return FileStatusKind::SuccessNonReel07.code();
            }
            let isam_disp = if disp == CloseDisposition::WithLock {
                isam.closed_with_lock = true;
                IsamCloseDisposition::WithLock
            } else {
                IsamCloseDisposition::Normal
            };
            let kind = isam.file.close(isam_disp);
            isam.open_mode = None;
            if let Some(p) = &conn.active_path {
                locks.unregister_close(p, conn.lock_id);
            }
            if disp == CloseDisposition::NoRewind {
                FileStatusKind::SuccessNonReel07.code()
            } else {
                kind.code()
            }
        }
    };
    if disp != CloseDisposition::ReelUnit {
        if disp == CloseDisposition::WithLock && is_status_success(code) {
            conn.closed_with_lock = true;
        }
        conn.active_mode = None;
        conn.active_path = None;
        if let Some(lf) = conn.os_lock_file.take() {
            release_os_flock(lf);
        }
        if let Some(pipe) = conn.pipe.take() {
            finish_pipe_attachment(pipe);
        }
    }
    code
}

/// Closes only the underlying engine when rolling back a conflicting `OPEN`.
///
/// # Arguments
///
/// * `engine` - Engine to close.
/// * `disp` - Close disposition.
fn close_engine_only(engine: &mut ConnectorEngine, disp: CloseDisposition) -> FileStatusCode {
    match engine {
        ConnectorEngine::Sequential(s) => s.close(disp),
        ConnectorEngine::LineSequential(ls) => ls.close(disp),
        ConnectorEngine::Relative(r) => r.close(disp),
        ConnectorEngine::Indexed(i) => i.file.close(IsamCloseDisposition::Normal).code(),
    }
}

/// Flushes an output shell pipe (`> cmd`) and removes temporary pipe staging files.
///
/// # Arguments
///
/// * `pipe` - Completed [`PipeAttachment`].
fn finish_pipe_attachment(pipe: PipeAttachment) {
    match pipe {
        PipeAttachment::InputTemp { temp_path } => {
            let _ = fs::remove_file(temp_path);
        }
        PipeAttachment::OutputCommand { temp_path, command } => {
            if let Ok(bytes) = fs::read(&temp_path)
                && let Ok(mut child) = Command::new("sh")
                    .args(["-c", &command])
                    .stdin(Stdio::piped())
                    .spawn()
            {
                if let Some(mut sin) = child.stdin.take() {
                    let _ = sin.write_all(&bytes);
                }
                let _ = child.wait();
            }
            let _ = fs::remove_file(temp_path);
        }
    }
}

/// Executes `READ` against `conn`, populating `rec_buf` and staging `out_len` / `out_rel`.
///
/// # Arguments
///
/// * `conn` - Target connector.
/// * `rec_buf` - Destination `FD` record buffer.
/// * `read_kind` - `0 = Next`, `1 = Previous`, `2 = Random`.
/// * `key_idx` - Key index (`0 = primary`, `1.. = alternate`).
/// * `explicit_key` - Optional explicit `KEY IS` bytes.
/// * `rel_key_in` - Current 1-based `RELATIVE KEY`.
/// * `lock_pol` - Record lock policy.
/// * `out_len` - Output actual record length (`-1` if none).
/// * `out_rel` - Output 1-based `RELATIVE KEY` (`-1` if none).
#[expect(
    clippy::too_many_arguments,
    reason = "Unified READ dispatcher across all four COBOL file organizations."
)]
fn execute_read_on_connector(
    conn: &mut ManagedConnector,
    rec_buf: &mut [u8],
    read_kind: i32,
    key_idx: u16,
    explicit_key: Option<&[u8]>,
    rel_key_in: i64,
    lock_pol: ReadLockPolicy,
    out_len: &mut i64,
    out_rel: &mut i64,
) -> FileStatusCode {
    let pad = conn.spec.padding_byte;
    match &mut conn.engine {
        ConnectorEngine::Sequential(seq) => {
            if read_kind != 0 {
                return FileStatusKind::ReadWrongOpenMode47.code();
            }
            let res = seq.read_next();
            if let Some(bytes) = &res.record {
                copy_record_into_fd(rec_buf, bytes, pad);
                *out_len = i64::try_from(res.actual_len.unwrap_or(bytes.len())).unwrap_or(0);
            }
            res.status
        }
        ConnectorEngine::LineSequential(ls) => {
            if read_kind != 0 {
                return FileStatusKind::ReadWrongOpenMode47.code();
            }
            let res = ls.read_next();
            if let Some(bytes) = &res.record {
                copy_record_into_fd(rec_buf, bytes, pad);
                *out_len = i64::try_from(res.actual_len.unwrap_or(bytes.len())).unwrap_or(0);
            }
            res.status
        }
        ConnectorEngine::Relative(rel) => {
            let res = if read_kind == 2 {
                let raw_slot = u64::try_from(rel_key_in).unwrap_or(0);
                rel.read_random(raw_slot, lock_pol)
            } else {
                let dir = if read_kind == 1 {
                    ReadDirection::Previous
                } else {
                    ReadDirection::Next
                };
                rel.read_sequential(dir, lock_pol)
            };
            if let Some(bytes) = &res.record {
                copy_record_into_fd(rec_buf, bytes, pad);
                *out_len = i64::try_from(res.actual_len.unwrap_or(bytes.len())).unwrap_or(0);
            }
            if read_kind != 2
                && let Some(sk) = res.slot_key
            {
                *out_rel = i64::try_from(sk.get()).unwrap_or(-1);
            }
            res.status
        }
        ConnectorEngine::Indexed(isam) => execute_isam_read(
            isam,
            rec_buf,
            pad,
            read_kind,
            key_idx,
            explicit_key,
            out_len,
        ),
    }
}

/// Executes sequential or keyed `READ` on an [`IsamRuntimeAdapter`], enforcing `SUPPRESS WHEN` rules.
///
/// # Arguments
///
/// * `isam` - Mutable ISAM adapter.
/// * `rec_buf` - Destination `FD` record buffer.
/// * `pad` - Trailing pad byte.
/// * `read_kind` - `0 = Next`, `1 = Previous`, `2 = Random`.
/// * `key_idx` - Key index (`0 = primary`, `1.. = alternate`).
/// * `explicit_key` - Optional explicit `KEY IS` slice.
/// * `out_len` - Output actual record length.
fn execute_isam_read(
    isam: &mut IsamRuntimeAdapter,
    rec_buf: &mut [u8],
    pad: u8,
    read_kind: i32,
    key_idx: u16,
    explicit_key: Option<&[u8]>,
    out_len: &mut i64,
) -> FileStatusCode {
    if read_kind == 2 {
        let key_handle = resolve_isam_key_id(key_idx);
        let Ok(kspec) = isam.schema.key_spec(key_handle) else {
            return FileStatusKind::RecordNotFound23.code();
        };
        let full_len = kspec.total_byte_len();
        let raw_key = build_full_isam_key(kspec, rec_buf, explicit_key, full_len);
        if is_alt_key_suppressed(&isam.alt_specs, key_idx, &raw_key) {
            return FileStatusKind::RecordNotFound23.code();
        }
        match isam.file.read_by_key(key_handle, &raw_key) {
            Ok((kind, rec_opt)) => {
                isam.active_kor = key_idx;
                if let Some(bytes) = &rec_opt {
                    copy_record_into_fd(rec_buf, bytes, pad);
                    *out_len = i64::try_from(bytes.len()).unwrap_or(0);
                }
                normalize_suppressed_dup_status(isam, kind, rec_opt.as_deref()).code()
            }
            Err(_) => FileStatusKind::PermanentError30.code(),
        }
    } else {
        loop {
            let step = if read_kind == 1 {
                isam.file.read_previous()
            } else {
                isam.file.read_next()
            };
            match step {
                Ok((kind, Some(bytes))) => {
                    if isam.active_kor > 0
                        && let Ok(kid) = KeyIndexId::alternate(isam.active_kor - 1)
                        && let Ok(kspec) = isam.schema.key_spec(kid)
                    {
                        let extracted = extract_segments_from_record(kspec, &bytes);
                        if is_alt_key_suppressed(&isam.alt_specs, isam.active_kor, &extracted) {
                            continue;
                        }
                    }
                    copy_record_into_fd(rec_buf, &bytes, pad);
                    *out_len = i64::try_from(bytes.len()).unwrap_or(0);
                    return normalize_suppressed_dup_status(isam, kind, Some(&bytes)).code();
                }
                Ok((kind, None)) => return kind.code(),
                Err(_) => return FileStatusKind::PermanentError30.code(),
            }
        }
    }
}

/// Executes `WRITE` against `conn`.
///
/// # Arguments
///
/// * `conn` - Target connector.
/// * `rec_slice` - Record payload to write.
/// * `adv_kind` - `0=Default`, `1=BeforeLines`, `2=AfterLines`, `3=BeforePage`, `4=AfterPage`.
/// * `adv_count` - Advancing line count.
/// * `rel_key_in` - 1-based `RELATIVE KEY`.
/// * `out_rel` - Output updated `RELATIVE KEY`.
/// * `out_lc` - Output updated `LINAGE-COUNTER`.
/// * `out_eop` - Output `AT END-OF-PAGE` flag.
#[expect(
    clippy::too_many_arguments,
    reason = "Unified WRITE dispatcher across all four COBOL file organizations."
)]
fn execute_write_on_connector(
    conn: &mut ManagedConnector,
    rec_slice: &[u8],
    adv_kind: i32,
    adv_count: i64,
    rel_key_in: i64,
    out_rel: &mut i64,
    out_lc: &mut i64,
    out_eop: &mut bool,
) -> FileStatusCode {
    let adv_spec = decode_advancing_spec(adv_kind, adv_count, conn.spec.print_mode);
    match &mut conn.engine {
        ConnectorEngine::Sequential(seq) => {
            let res = seq.write_record(rec_slice, adv_spec);
            *out_eop = res.page_overflow == PageOverflowOutcome::EndOfPageReached;
            if let Some(lc) = res.linage_counter {
                *out_lc = i64::try_from(lc).unwrap_or(-1);
            }
            res.status
        }
        ConnectorEngine::LineSequential(ls) => {
            let ls_adv = if adv_kind == 0 {
                AdvancingSpec::DefaultSingleLine
            } else {
                adv_spec
            };
            ls.write_record(rec_slice, ls_adv)
        }
        ConnectorEngine::Relative(rel) => {
            if conn.spec.access_mode == HirFileAccessMode::Sequential {
                let res = rel.write_sequential(rec_slice);
                if let Some(sk) = res.slot_key {
                    *out_rel = i64::try_from(sk.get()).unwrap_or(-1);
                }
                res.status
            } else {
                let raw_slot = u64::try_from(rel_key_in).unwrap_or(0);
                rel.write_keyed(raw_slot, rec_slice).status
            }
        }
        ConnectorEngine::Indexed(isam) => execute_isam_write(isam, rec_slice),
    }
}

/// Executes `WRITE` on an [`IsamRuntimeAdapter`], enforcing `Unique` on non-suppressed `SUPPRESS WHEN` keys.
///
/// # Arguments
///
/// * `isam` - Mutable ISAM adapter.
/// * `rec_slice` - Record payload.
fn execute_isam_write(isam: &mut IsamRuntimeAdapter, rec_slice: &[u8]) -> FileStatusCode {
    if isam.open_mode.is_some()
        && let Some(dup_code) = check_unsuppressed_unique_violation(isam, rec_slice, None)
    {
        return dup_code;
    }
    match isam.file.write(rec_slice) {
        Ok(kind) => {
            let norm = normalize_suppressed_dup_status(isam, kind, Some(rec_slice)).code();
            if is_status_success(norm) {
                record_unsuppressed_unique_keys(isam, rec_slice);
            }
            norm
        }
        Err(_) => FileStatusKind::PermanentError30.code(),
    }
}

/// Executes `REWRITE` against `conn`.
///
/// # Arguments
///
/// * `conn` - Target connector.
/// * `rec_slice` - Replacement record payload.
/// * `rel_key_in` - 1-based `RELATIVE KEY`.
fn execute_rewrite_on_connector(
    conn: &mut ManagedConnector,
    rec_slice: &[u8],
    rel_key_in: i64,
) -> FileStatusCode {
    match &mut conn.engine {
        ConnectorEngine::Sequential(seq) => seq.rewrite_record(rec_slice),
        ConnectorEngine::LineSequential(ls) => ls.rewrite_unsupported(),
        ConnectorEngine::Relative(rel) => {
            let rk = (conn.spec.access_mode != HirFileAccessMode::Sequential)
                .then(|| u64::try_from(rel_key_in).unwrap_or(0));
            rel.rewrite_record(rk, rec_slice)
        }
        ConnectorEngine::Indexed(isam) => {
            let self_pk = extract_segments_from_record(isam.schema.primary_key(), rec_slice);
            if isam.open_mode == Some(OpenMode::Io)
                && let Some(dup_code) =
                    check_unsuppressed_unique_violation(isam, rec_slice, Some(&self_pk))
            {
                return dup_code;
            }
            match isam.file.rewrite(rec_slice) {
                Ok(kind) => {
                    let norm = normalize_suppressed_dup_status(isam, kind, Some(rec_slice)).code();
                    if is_status_success(norm) {
                        isam.unsuppressed_unique_alt_map
                            .retain(|_, pk| pk != &self_pk);
                        record_unsuppressed_unique_keys(isam, rec_slice);
                    }
                    norm
                }
                Err(_) => FileStatusKind::PermanentError30.code(),
            }
        }
    }
}

/// Executes `DELETE` against `conn`.
///
/// # Arguments
///
/// * `conn` - Target connector.
/// * `rec_slice` - Current `FD` record bytes.
/// * `rel_key_in` - 1-based `RELATIVE KEY`.
fn execute_delete_on_connector(
    conn: &mut ManagedConnector,
    rec_slice: &[u8],
    rel_key_in: i64,
) -> FileStatusCode {
    match &mut conn.engine {
        ConnectorEngine::Sequential(_) | ConnectorEngine::LineSequential(_) => {
            FileStatusKind::RewriteDeleteWrongOpenMode49.code()
        }
        ConnectorEngine::Relative(rel) => {
            let rk = (conn.spec.access_mode != HirFileAccessMode::Sequential)
                .then(|| u64::try_from(rel_key_in).unwrap_or(0));
            rel.delete_record(rk)
        }
        ConnectorEngine::Indexed(isam) => {
            let pk_spec = isam.schema.primary_key();
            let pk_bytes = build_full_isam_key(pk_spec, rec_slice, None, pk_spec.total_byte_len());
            let res = if isam.access_mode == IsamAccessMode::Sequential {
                isam.file.delete_sequential()
            } else {
                isam.file.delete_by_primary_key(&pk_bytes)
            };
            match res {
                Ok(kind) => {
                    let code = kind.code();
                    if is_status_success(code) {
                        isam.unsuppressed_unique_alt_map
                            .retain(|_, pk| pk != &pk_bytes);
                    }
                    code
                }
                Err(_) => FileStatusKind::PermanentError30.code(),
            }
        }
    }
}

/// Executes physical `DELETE FILE` on `conn`.
///
/// # Arguments
///
/// * `conn` - Target connector.
/// * `dyn_bytes` - Optional dynamic `ASSIGN` bytes.
/// * `locks` - Intra-process [`FileLockRegistry`].
fn execute_delete_physical(
    conn: &mut ManagedConnector,
    dyn_bytes: &[u8],
    locks: &FileLockRegistry,
) -> FileStatusCode {
    if conn.active_mode.is_some() {
        return FileStatusKind::AlreadyOpen41.code();
    }
    if conn.closed_with_lock {
        return FileStatusKind::ClosedWithLock38.code();
    }
    let Ok((path, _)) = resolve_connector_open_path(conn, dyn_bytes, OpenMode::Output) else {
        return FileStatusKind::PermanentError30.code();
    };
    if let coblang_fileio::LockOutcome::Denied(kind) =
        locks.register_open(&path, conn.lock_id, LockMode::Exclusive)
    {
        return kind.code();
    }
    locks.unregister_close(&path, conn.lock_id);

    if !path.exists() {
        return if conn.spec.optionality == HirFileOptionality::Optional {
            FileStatusKind::SuccessOptionalFile05.code()
        } else {
            FileStatusKind::FileNotFound35.code()
        };
    }
    let Ok(lf) = acquire_os_flock(&path, true) else {
        return FileStatusKind::FileSharingConflict61.code();
    };
    release_os_flock(lf);
    match fs::remove_file(&path) {
        Ok(()) => FileStatusKind::Success00.code(),
        Err(_) => FileStatusKind::OpenPermissionDenied37.code(),
    }
}

/// Executes `START` (`FIRST`, `LAST`, or relational key positioning) on `conn`.
///
/// # Arguments
///
/// * `conn` - Target connector.
/// * `rec_slice` - `FD` record buffer.
/// * `start_kind` - `0..=4` relation, `5 = First`, `6 = Last`.
/// * `key_idx` - Key index (`0 = primary/relative`, `1.. = alternate`).
/// * `explicit_key` - Optional explicit key prefix bytes.
/// * `req_key_len` - Requested partial key byte length (`0` = full key).
/// * `rel_key_in` - 1-based `RELATIVE KEY`.
fn execute_start_on_connector(
    conn: &mut ManagedConnector,
    rec_slice: &[u8],
    start_kind: i32,
    key_idx: u16,
    explicit_key: Option<&[u8]>,
    req_key_len: usize,
    rel_key_in: i64,
) -> FileStatusCode {
    match &mut conn.engine {
        ConnectorEngine::Sequential(_) | ConnectorEngine::LineSequential(_) => {
            FileStatusKind::ReadWrongOpenMode47.code()
        }
        ConnectorEngine::Relative(rel) => match start_kind {
            5 => {
                let Ok(first_slot) = RelativeSlotKey::try_new(1) else {
                    return FileStatusKind::RecordNotFound23.code();
                };
                rel.start(StartRelation::GreaterOrEqual, first_slot.get())
            }
            6 => rel.start(StartRelation::LessOrEqual, u64::MAX / 4),
            _ => {
                let raw_slot = u64::try_from(rel_key_in).unwrap_or(0);
                rel.start(decode_rel_start_relation(start_kind), raw_slot)
            }
        },
        ConnectorEngine::Indexed(isam) => execute_isam_start(
            isam,
            rec_slice,
            start_kind,
            key_idx,
            explicit_key,
            req_key_len,
        ),
    }
}

/// Executes `START` on an [`IsamRuntimeAdapter`].
///
/// # Arguments
///
/// * `isam` - Mutable ISAM adapter.
/// * `rec_slice` - `FD` record buffer.
/// * `start_kind` - `0..=4` relation, `5 = First`, `6 = Last`.
/// * `key_idx` - Key index (`0 = primary`, `1.. = alternate`).
/// * `explicit_key` - Optional explicit key slice.
/// * `req_key_len` - Optional partial key prefix length.
fn execute_isam_start(
    isam: &mut IsamRuntimeAdapter,
    rec_slice: &[u8],
    start_kind: i32,
    key_idx: u16,
    explicit_key: Option<&[u8]>,
    req_key_len: usize,
) -> FileStatusCode {
    if start_kind == 5 || start_kind == 6 {
        let extreme_byte = find_collation_extreme_byte(isam.schema.collating(), start_kind == 6);
        let rel = if start_kind == 5 {
            IsamStartRelation::GreaterOrEqual
        } else {
            IsamStartRelation::LessOrEqual
        };
        return match isam.file.start(KeyIndexId::PRIMARY, rel, &[extreme_byte]) {
            Ok(kind) => {
                if kind.code().is_success() {
                    isam.active_kor = 0;
                }
                kind.code()
            }
            Err(_) => FileStatusKind::PermanentError30.code(),
        };
    }

    let kid = resolve_isam_key_id(key_idx);
    let Ok(kspec) = isam.schema.key_spec(kid) else {
        return FileStatusKind::RecordNotFound23.code();
    };
    let full_len = kspec.total_byte_len();
    if req_key_len > full_len {
        return FileStatusKind::RecordNotFound23.code();
    }
    let eff_len = if req_key_len > 0 {
        req_key_len
    } else {
        explicit_key.map_or(full_len, |k| k.len().clamp(1, full_len))
    };
    let full_raw = build_full_isam_key(kspec, rec_slice, explicit_key, full_len);
    let prefix = full_raw.get(..eff_len).unwrap_or(&full_raw);

    let rel = decode_isam_start_relation(start_kind);
    if rel == IsamStartRelation::Equal
        && eff_len == full_len
        && is_alt_key_suppressed(&isam.alt_specs, key_idx, prefix)
    {
        return FileStatusKind::RecordNotFound23.code();
    }

    match isam.file.start(kid, rel, prefix) {
        Ok(st_kind) => {
            if st_kind.code().is_success() {
                isam.active_kor = key_idx;
            }
            st_kind.code()
        }
        Err(_) => FileStatusKind::PermanentError30.code(),
    }
}

/// Finds the raw byte (`0x00..=0xFF`) with minimum (`want_max == false`) or maximum (`want_max == true`) weight.
///
/// # Arguments
///
/// * `collating` - ISAM [`CollatingSequence`].
/// * `want_max` - `false` for lowest-collating byte, `true` for highest-collating byte.
fn find_collation_extreme_byte(collating: &CollatingSequence, want_max: bool) -> u8 {
    let mut best_byte = 0_u8;
    let mut best_weight = collating.encode_key(&[0_u8]).first().copied().unwrap_or(0);
    for b in 1_u8..=u8::MAX {
        let w = collating.encode_key(&[b]).first().copied().unwrap_or(b);
        if (want_max && w > best_weight) || (!want_max && w < best_weight) {
            best_weight = w;
            best_byte = b;
        }
    }
    best_byte
}

/// Assembles a full-width raw key for `kspec` from `explicit_key` or `rec_buf` segments.
///
/// # Arguments
///
/// * `kspec` - ISAM [`KeySpec`].
/// * `rec_buf` - `FD` record buffer.
/// * `explicit_key` - Optional explicit `KEY IS` slice.
/// * `full_len` - Expected full key length.
fn build_full_isam_key(
    kspec: &KeySpec,
    rec_buf: &[u8],
    explicit_key: Option<&[u8]>,
    full_len: usize,
) -> Vec<u8> {
    if let Some(ek) = explicit_key
        && kspec.segments().len() == 1
    {
        let mut out = vec![b' '; full_len];
        for (d, &s) in out.iter_mut().zip(ek.iter().take(full_len)) {
            *d = s;
        }
        return out;
    }
    extract_segments_from_record(kspec, rec_buf)
}

/// Extracts and concatenates all byte segments of `kspec` from `record` (space-padding if short).
///
/// # Arguments
///
/// * `kspec` - ISAM [`KeySpec`].
/// * `record` - Record bytes.
fn extract_segments_from_record(kspec: &KeySpec, record: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(kspec.total_byte_len());
    for seg in kspec.segments() {
        let start = seg.offset() as usize;
        let len = seg.length() as usize;
        for idx in 0..len {
            let b = record
                .get(start.saturating_add(idx))
                .copied()
                .unwrap_or(b' ');
            out.push(b);
        }
    }
    out
}

/// Checks whether `raw_key` matches a `SUPPRESS WHEN` rule on alternate key `key_idx` (`1..`).
///
/// # Arguments
///
/// * `alt_specs` - Alternate key specs.
/// * `key_idx` - 1-based alternate key index (`0` = primary -> never suppressed).
/// * `raw_key` - Extracted raw key bytes.
fn is_alt_key_suppressed(alt_specs: &[WireAltKey], key_idx: u16, raw_key: &[u8]) -> bool {
    if key_idx == 0 {
        return false;
    }
    let Some(ak) = alt_specs.get(usize::from(key_idx - 1)) else {
        return false;
    };
    match &ak.suppression {
        None => false,
        Some(HirAltKeySuppression::WhenAllByte(b)) => raw_key.iter().all(|x| *x == *b),
        Some(HirAltKeySuppression::WhenExactBytes(bs)) => raw_key == bs.as_slice(),
    }
}

/// Seeds `isam.unsuppressed_unique_alt_map` on `OPEN OUTPUT` / `OPEN I-O` / `OPEN EXTEND`.
///
/// # Arguments
///
/// * `isam` - Mutable ISAM adapter.
/// * `path` - Target file path.
/// * `mode` - Requested [`OpenMode`].
fn seed_unsuppressed_unique_map(isam: &mut IsamRuntimeAdapter, path: &Path, mode: OpenMode) {
    isam.unsuppressed_unique_alt_map.clear();
    if mode == OpenMode::Output || !path.exists() {
        return;
    }
    let mut probe = IsamFile::new_disk(
        path,
        isam.schema.clone(),
        IsamAccessMode::Sequential,
        IsamFileOptionality::Optional,
    );
    if let Ok(kind) = probe.open(IsamOpenMode::Input)
        && is_status_success(kind.code())
    {
        while let Ok((rk, Some(rec))) = probe.read_next() {
            if !is_status_success(rk.code()) {
                break;
            }
            record_unsuppressed_unique_keys(isam, &rec);
        }
        let _ = probe.close(IsamCloseDisposition::Normal);
    }
}

/// Inserts all non-suppressed `Unique` alternate keys of `rec` into `isam.unsuppressed_unique_alt_map`.
///
/// # Arguments
///
/// * `isam` - Mutable ISAM adapter.
/// * `rec` - Committed record bytes.
fn record_unsuppressed_unique_keys(isam: &mut IsamRuntimeAdapter, rec: &[u8]) {
    let pk = extract_segments_from_record(isam.schema.primary_key(), rec);
    let coll = isam.schema.collating();
    for (i, ak) in isam.alt_specs.iter().enumerate() {
        if ak.suppression.is_none() || ak.duplicates != HirDuplicateKeyPolicy::Unique {
            continue;
        }
        let Ok(kid) = KeyIndexId::alternate(u16::try_from(i).unwrap_or(0)) else {
            continue;
        };
        let Ok(kspec) = isam.schema.key_spec(kid) else {
            continue;
        };
        let raw_alt = extract_segments_from_record(kspec, rec);
        let key_idx = u16::try_from(i.saturating_add(1)).unwrap_or(1);
        if is_alt_key_suppressed(&isam.alt_specs, key_idx, &raw_alt) {
            continue;
        }
        let ranked = coll.encode_key(&raw_alt);
        isam.unsuppressed_unique_alt_map
            .insert((key_idx, ranked), pk.clone());
    }
}

/// Checks before `WRITE`/`REWRITE` whether any `Unique` alternate key that had `AllowedFifo`
/// enabled for `SUPPRESS WHEN` has an unsuppressed duplicate in `isam.unsuppressed_unique_alt_map`.
///
/// # Arguments
///
/// * `isam` - ISAM adapter.
/// * `rec` - Candidate record to write/rewrite.
/// * `self_pk` - Primary key of the record being rewritten (`None` for `WRITE`).
fn check_unsuppressed_unique_violation(
    isam: &IsamRuntimeAdapter,
    rec: &[u8],
    self_pk: Option<&[u8]>,
) -> Option<FileStatusCode> {
    let coll = isam.schema.collating();
    for (i, ak) in isam.alt_specs.iter().enumerate() {
        if ak.suppression.is_none() || ak.duplicates != HirDuplicateKeyPolicy::Unique {
            continue;
        }
        let Ok(kid) = KeyIndexId::alternate(u16::try_from(i).unwrap_or(0)) else {
            continue;
        };
        let Ok(kspec) = isam.schema.key_spec(kid) else {
            continue;
        };
        let raw_alt = extract_segments_from_record(kspec, rec);
        let key_idx = u16::try_from(i.saturating_add(1)).unwrap_or(1);
        if is_alt_key_suppressed(&isam.alt_specs, key_idx, &raw_alt) {
            continue;
        }
        let ranked = coll.encode_key(&raw_alt);
        if let Some(existing_pk) = isam.unsuppressed_unique_alt_map.get(&(key_idx, ranked)) {
            if Some(existing_pk.as_slice()) == self_pk {
                continue;
            }
            return Some(FileStatusKind::DuplicateKey22.code());
        }
    }
    None
}

/// Downgrades `"02"` (`SuccessDuplicateKey02`) to `"00"` when all `SUPPRESS WHEN` keys on `rec` are suppressed.
///
/// # Arguments
///
/// * `isam` - ISAM adapter.
/// * `kind` - Raw [`FileStatusKind`] returned by `IsamFile`.
/// * `rec_opt` - Optional record payload.
fn normalize_suppressed_dup_status(
    isam: &IsamRuntimeAdapter,
    kind: FileStatusKind,
    rec_opt: Option<&[u8]>,
) -> FileStatusKind {
    if kind != FileStatusKind::SuccessDuplicateKey02 {
        return kind;
    }
    let Some(rec) = rec_opt else {
        return kind;
    };
    let has_any_suppression = isam.alt_specs.iter().any(|a| a.suppression.is_some());
    if !has_any_suppression {
        return kind;
    }
    for (i, ak) in isam.alt_specs.iter().enumerate() {
        if ak.duplicates != HirDuplicateKeyPolicy::WithDuplicates {
            continue;
        }
        let Ok(key_handle) = KeyIndexId::alternate(u16::try_from(i).unwrap_or(0)) else {
            continue;
        };
        let Ok(kspec) = isam.schema.key_spec(key_handle) else {
            continue;
        };
        let raw_alt = extract_segments_from_record(kspec, rec);
        let key_idx = u16::try_from(i.saturating_add(1)).unwrap_or(1);
        if !is_alt_key_suppressed(&isam.alt_specs, key_idx, &raw_alt) {
            return FileStatusKind::SuccessDuplicateKey02;
        }
    }
    FileStatusKind::Success00
}

/// Copies `src` into `dst`, padding any remaining trailing bytes of `dst` with `pad_byte`.
///
/// # Arguments
///
/// * `dst` - Mutable `FD` record buffer.
/// * `src` - Read record bytes.
/// * `pad_byte` - Pad byte (`b' '` by default).
fn copy_record_into_fd(dst: &mut [u8], src: &[u8], pad_byte: u8) {
    let n = src.len().min(dst.len());
    for (d, &s) in dst.iter_mut().zip(src.iter().take(n)) {
        *d = s;
    }
    if let Some(tail) = dst.get_mut(n..) {
        tail.fill(pad_byte);
    }
}

/// Writes 2 ASCII status bytes into `status_ptr` when non-null.
///
/// # Arguments
///
/// * `status_ptr` - Optional 2-byte status destination pointer.
/// * `code` - [`FileStatusCode`] to store.
///
/// # Safety
///
/// When non-null, `status_ptr` must point to at least 2 writable bytes.
#[expect(
    unsafe_code,
    reason = "Audited 2-byte `FILE STATUS` store when non-null."
)]
unsafe fn store_status_if_present(status_ptr: *mut u8, code: FileStatusCode) {
    if !status_ptr.is_null() {
        // SAFETY: Caller guarantees non-null `status_ptr` points to 2 writable bytes.
        let slot: &mut [u8; 2] = unsafe { &mut *status_ptr.cast::<[u8; 2]>() };
        write_status_bytes(code, slot);
    }
}

/// Computes the final `POST_IO_*` return code for MLIR structured control-flow dispatch.
///
/// # Arguments
///
/// * `st` - Thread file state (records `last_fatal` on `POST_IO_FATAL_ABORT`).
/// * `file_name` - Diagnostic connector name.
/// * `code` - Completed [`FileStatusCode`].
/// * `effective_mode` - Active or attempted [`OpenMode`].
/// * `verb_flavor` - `0=plain`, `1=AT END`, `2=INVALID KEY`, `3=WRITE EOP`, `4=DELETE FILE ON EXCEPTION`.
/// * `inline_flags` - Bitmask of `INLINE_FLAG_HAS_ON` and `INLINE_FLAG_HAS_NOT_ON`.
/// * `has_status_item` - Whether `FILE STATUS` was declared on the connector.
/// * `decl_mask` - 5-bit `DECL_MASK_*` bitmask.
/// * `reached_eop` - Whether `WRITE` reached `LINAGE` `END-OF-PAGE`.
#[expect(
    clippy::too_many_arguments,
    reason = "Complete post-I/O decision inputs across inline clauses, DECLARATIVES, and FILE STATUS."
)]
fn finalize_post_io(
    st: &mut FileIoThreadState,
    file_name: &str,
    code: FileStatusCode,
    effective_mode: Option<OpenMode>,
    verb_flavor: i32,
    inline_flags: i32,
    has_status_item: bool,
    decl_mask: i32,
    reached_eop: bool,
) -> i32 {
    let has_on = (inline_flags & INLINE_FLAG_HAS_ON) != 0;
    let has_not_on = (inline_flags & INLINE_FLAG_HAS_NOT_ON) != 0;
    if *code.as_bytes() != *b"00" {
        crate::m13_abi::note_io_exception(*code.as_bytes(), file_name);
    }

    if verb_flavor == 4 {
        return if is_status_success(code) {
            if has_not_on {
                POST_IO_FIRE_NOT_INVALID_KEY
            } else {
                POST_IO_CONTINUE
            }
        } else if has_on {
            POST_IO_FIRE_INVALID_KEY
        } else if let Some(decl_rc) = select_matching_declarative_rc(decl_mask, effective_mode) {
            decl_rc
        } else if has_status_item {
            POST_IO_CONTINUE
        } else {
            st.last_fatal = Some((String::from(file_name), code));
            POST_IO_FATAL_ABORT
        };
    }

    let matched_decl = select_matching_declarative_rc(decl_mask, effective_mode);
    let kind = status_code_to_kind(code);
    let handlers = VerbHandlerConfig {
        at_end_clause: if verb_flavor == 1 && has_on {
            InlineClausePresence::Present
        } else {
            InlineClausePresence::Absent
        },
        invalid_key_clause: if verb_flavor == 2 && has_on {
            InlineClausePresence::Present
        } else {
            InlineClausePresence::Absent
        },
        declarative: if matched_decl.is_some() {
            DeclarativePresence::MatchingUseProcedure
        } else {
            DeclarativePresence::None
        },
        file_status_item: if has_status_item {
            StatusClausePresence::Declared
        } else {
            StatusClausePresence::Undeclared
        },
    };

    match resolve_post_io_action(file_name, kind, handlers) {
        Ok(PostIoDispatch::NormalCompletion) => match verb_flavor {
            1 if has_not_on => POST_IO_FIRE_NOT_AT_END,
            2 if has_not_on => POST_IO_FIRE_NOT_INVALID_KEY,
            3 if reached_eop && has_on => POST_IO_FIRE_AT_EOP,
            3 if !reached_eop && has_not_on => POST_IO_FIRE_NOT_AT_EOP,
            _ => POST_IO_CONTINUE,
        },
        Ok(PostIoDispatch::FireAtEnd) => POST_IO_FIRE_AT_END,
        Ok(PostIoDispatch::FireInvalidKey) => POST_IO_FIRE_INVALID_KEY,
        Ok(PostIoDispatch::InvokeDeclarative) => matched_decl.unwrap_or(POST_IO_CONTINUE),
        Ok(PostIoDispatch::AbsorbViaFileStatus) => POST_IO_CONTINUE,
        Err(_) => {
            st.last_fatal = Some((String::from(file_name), code));
            POST_IO_FATAL_ABORT
        }
    }
}

/// Selects the matching `POST_IO_INVOKE_DECL_*` return code from `decl_mask` and `mode`.
///
/// # Arguments
///
/// * `decl_mask` - 5-bit `DECL_MASK_*` bitmask.
/// * `mode` - Active or attempted [`OpenMode`].
const fn select_matching_declarative_rc(decl_mask: i32, mode: Option<OpenMode>) -> Option<i32> {
    if (decl_mask & DECL_MASK_FILE) != 0 {
        return Some(POST_IO_INVOKE_DECL_FILE);
    }
    match mode {
        Some(OpenMode::Input) if (decl_mask & DECL_MASK_INPUT) != 0 => {
            Some(POST_IO_INVOKE_DECL_INPUT)
        }
        Some(OpenMode::Output) if (decl_mask & DECL_MASK_OUTPUT) != 0 => {
            Some(POST_IO_INVOKE_DECL_OUTPUT)
        }
        Some(OpenMode::Io) if (decl_mask & DECL_MASK_IO) != 0 => Some(POST_IO_INVOKE_DECL_IO),
        Some(OpenMode::Extend) if (decl_mask & DECL_MASK_EXTEND) != 0 => {
            Some(POST_IO_INVOKE_DECL_EXTEND)
        }
        _ => None,
    }
}

/// Returns `true` when `code` is in class `0x` (`'0'` leading status character).
///
/// # Arguments
///
/// * `code` - [`FileStatusCode`] to inspect.
fn is_status_success(code: FileStatusCode) -> bool {
    code.as_bytes().first().copied() == Some(b'0')
}

/// Maps a 2-byte [`FileStatusCode`] back to its [`FileStatusKind`] variant.
///
/// # Arguments
///
/// * `code` - 2-byte [`FileStatusCode`].
fn status_code_to_kind(code: FileStatusCode) -> FileStatusKind {
    FileStatusKind::ALL
        .iter()
        .copied()
        .find(|k| k.code() == code)
        .unwrap_or(FileStatusKind::PermanentError30)
}

/// Maps `0..=3` to [`OpenMode`].
///
/// # Arguments
///
/// * `code` - ABI integer code.
const fn decode_open_mode(code: i32) -> OpenMode {
    match code {
        1 => OpenMode::Output,
        2 => OpenMode::Io,
        3 => OpenMode::Extend,
        _ => OpenMode::Input,
    }
}

/// Maps `0..=4` to [`CloseDisposition`].
///
/// # Arguments
///
/// * `code` - ABI integer code.
const fn decode_close_disp(code: i32) -> CloseDisposition {
    match code {
        1 => CloseDisposition::WithLock,
        2 => CloseDisposition::NoRewind,
        3 | 4 => CloseDisposition::ReelUnit,
        _ => CloseDisposition::Normal,
    }
}

/// Maps `0..=4` to [`ReadLockPolicy`].
///
/// # Arguments
///
/// * `code` - ABI integer code.
const fn decode_read_lock(code: i32) -> ReadLockPolicy {
    match code {
        1 => ReadLockPolicy::WithLock,
        2 => ReadLockPolicy::WithKeptLock,
        3 | 4 => ReadLockPolicy::WithNoLock,
        _ => ReadLockPolicy::DefaultLock,
    }
}

/// Maps `adv_kind` (`0..=4`), `adv_count`, and connector `print_mode` to [`AdvancingSpec`].
///
/// # Arguments
///
/// * `adv_kind` - ABI advancing code.
/// * `adv_count` - Line count.
/// * `print_mode` - Connector print framing mode.
fn decode_advancing_spec(
    adv_kind: i32,
    adv_count: i64,
    print_mode: HirPrintAdvancingMode,
) -> AdvancingSpec {
    let lines = usize::try_from(adv_count.max(0)).unwrap_or(0);
    match adv_kind {
        1 => AdvancingSpec::Lines {
            phase: AdvancingPhase::Before,
            count: lines,
        },
        2 => AdvancingSpec::Lines {
            phase: AdvancingPhase::After,
            count: lines,
        },
        3 => AdvancingSpec::Page {
            phase: AdvancingPhase::Before,
        },
        4 => AdvancingSpec::Page {
            phase: AdvancingPhase::After,
        },
        _ => match print_mode {
            HirPrintAdvancingMode::RawBinary => AdvancingSpec::NoneRaw,
            HirPrintAdvancingMode::LineAdvancing => AdvancingSpec::DefaultSingleLine,
        },
    }
}

/// Maps `0..=4` to `coblang-fileio` [`StartRelation`].
///
/// # Arguments
///
/// * `code` - ABI relation code.
const fn decode_rel_start_relation(code: i32) -> StartRelation {
    match code {
        1 => StartRelation::Greater,
        2 => StartRelation::GreaterOrEqual,
        3 => StartRelation::Less,
        4 => StartRelation::LessOrEqual,
        _ => StartRelation::Equal,
    }
}

/// Maps `0..=4` to `coblang-isam` [`IsamStartRelation`].
///
/// # Arguments
///
/// * `code` - ABI relation code.
const fn decode_isam_start_relation(code: i32) -> IsamStartRelation {
    match code {
        1 => IsamStartRelation::Greater,
        2 => IsamStartRelation::GreaterOrEqual,
        3 => IsamStartRelation::Less,
        4 => IsamStartRelation::LessOrEqual,
        _ => IsamStartRelation::Equal,
    }
}

/// Resolves `0` -> `KeyIndexId::PRIMARY` and `1..` -> `KeyIndexId::alternate(k - 1)`.
///
/// # Arguments
///
/// * `key_idx` - 0-based key slot (`0 = primary`, `1.. = alternate`).
fn resolve_isam_key_id(key_idx: u16) -> KeyIndexId {
    if key_idx == 0 {
        KeyIndexId::PRIMARY
    } else {
        KeyIndexId::alternate(key_idx - 1).unwrap_or(KeyIndexId::PRIMARY)
    }
}
