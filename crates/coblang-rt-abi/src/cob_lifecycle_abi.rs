//! Clean-room C-ABI lifecycle, module-frame, subprogram resolution/invocation, and `cob_alloc_base`
//! entrypoints (`cob_init_nomain`, `cob_stop_run`, `cob_module_enter`, `cob_module_leave`,
//! `cob_set_location`, `cob_call`, `cob_func`, `cob_cancel`, `cob_resolve`, `cob_call_error`).

use std::collections::BTreeMap;
use std::ffi::{CStr, c_char, c_void};
use std::fs::File;
use std::io::{self, Write};
use std::sync::{Mutex, MutexGuard, OnceLock};

use coblang_dynload::{cancel_dynamic_module, resolve_dynamic_module};
use coblang_pic::InspectMatchKind;
use coblang_rt_core::exceptions::ExceptionCode;
use coblang_rt_core::with_global_runtime;

use crate::cob_types::CobModule;
use crate::m11_abi::cob_malloc;
use crate::{
    cob_init, cob_tidy, coblang_rt_enter_call, coblang_rt_leave_call, coblang_rt_prepare_stop_run,
};

/// Active module call-frame metadata pushed by [`cob_module_enter`] or [`cob_call`].
#[derive(Debug, Clone, Default)]
pub struct ActiveModuleFrame {
    /// Raw address of caller's [`CobModule`] (`0` when synthesized by [`cob_call`]).
    pub module_ptr: usize,
    /// Logical COBOL `PROGRAM-ID` or subprogram name.
    pub module_name: String,
    /// Active source file path (`cob_set_location`).
    pub source_file: String,
    /// Active section name (`cob_set_location`).
    pub section: String,
    /// Active paragraph name (`cob_set_location`).
    pub paragraph: String,
    /// Active verb name (`cob_set_location`).
    pub statement: String,
    /// Active 1-based source line (`cob_set_location`).
    pub line: u32,
    /// Raw parameter pointers passed to this frame.
    pub param_ptrs: Vec<usize>,
    /// Declared or observed byte lengths of parameters passed to this frame.
    pub param_sizes: Vec<usize>,
    /// Whether this frame synthesized a root caller frame below itself on `module_stack`.
    pub pushed_root_caller: bool,
}

/// Staged `cob_string_*` C-ABI session state.
#[derive(Debug, Clone)]
pub struct CobStringAbiSession {
    /// Destination `*mut CobField` address (`0` when inactive).
    pub dst_field_addr: usize,
    /// Optional `WITH POINTER` `*mut CobField` address (`0` when omitted).
    pub ptr_field_addr: usize,
    /// Current delimiter bytes (`None` = `DELIMITED BY SIZE`).
    pub active_delim: Option<Vec<u8>>,
    /// Staged `(source_bytes, optional_delimiter)` pieces.
    pub pieces: Vec<(Vec<u8>, Option<Vec<u8>>)>,
}

impl Default for CobStringAbiSession {
    /// Creates an empty [`CobStringAbiSession`].
    fn default() -> Self {
        Self {
            dst_field_addr: 0,
            ptr_field_addr: 0,
            active_delim: None,
            pieces: Vec::new(),
        }
    }
}

/// Staged `INTO` receiver slot for `cob_unstring_*`.
#[derive(Debug, Clone, Copy)]
pub struct StagedUnstringInto {
    /// Destination `*mut CobField` address.
    pub dst_addr: usize,
    /// Optional `DELIMITER IN` `*mut CobField` address (`0` when null).
    pub dlm_addr: usize,
    /// Optional `COUNT IN` `*mut CobField` address (`0` when null).
    pub cnt_addr: usize,
}

/// Staged `cob_unstring_*` C-ABI session state.
#[derive(Debug, Clone, Default)]
pub struct CobUnstringAbiSession {
    /// Normalized source bytes.
    pub src_bytes: Vec<u8>,
    /// Optional `WITH POINTER` `*mut CobField` address (`0` when null).
    pub ptr_field_addr: usize,
    ///Optional `TALLYING IN` `*mut CobField` address (`0` when null).
    pub tally_field_addr: usize,
    /// Staged `(pattern_bytes, is_all_repeated)` delimiters.
    pub delimiters: Vec<(Vec<u8>, bool)>,
    /// Staged `INTO` destination descriptors.
    pub into_slots: Vec<StagedUnstringInto>,
}

/// Staged clause inside [`CobInspectAbiSession`].
#[derive(Debug, Clone)]
pub enum StagedAbiInspectOp {
    /// `TALLYING` clause incrementing `counter_field_addr`.
    Tally {
        /// Target `*mut CobField` counter address.
        counter_field_addr: usize,
        /// Match mode (`Characters`, `All`, `Leading`, `First`, `Trailing`).
        kind: InspectMatchKind,
        /// Pattern bytes (empty for `Characters`).
        pattern: Vec<u8>,
        /// Optional `AFTER INITIAL` bound.
        after: Option<Vec<u8>>,
        /// Optional `BEFORE INITIAL` bound.
        before: Option<Vec<u8>>,
    },
    /// `REPLACING` clause mutating the `INSPECT` target in place.
    Replace {
        /// Match mode (`Characters`, `All`, `Leading`, `First`, `Trailing`).
        kind: InspectMatchKind,
        /// Pattern bytes (empty for `Characters`).
        pattern: Vec<u8>,
        /// Replacement bytes (`BY ...`).
        by: Vec<u8>,
        /// Optional `AFTER INITIAL` bound.
        after: Option<Vec<u8>>,
        /// Optional `BEFORE INITIAL` bound.
        before: Option<Vec<u8>>,
    },
    /// `CONVERTING` clause translating `from` -> `to`.
    Convert {
        /// Source alphabet bytes.
        from: Vec<u8>,
        /// Replacement alphabet bytes.
        to: Vec<u8>,
        /// Optional `AFTER INITIAL` bound.
        after: Option<Vec<u8>>,
        /// Optional `BEFORE INITIAL` bound.
        before: Option<Vec<u8>>,
    },
}

/// Staged `cob_inspect_*` C-ABI session state.
#[derive(Debug, Clone, Default)]
pub struct CobInspectAbiSession {
    /// Target `*mut CobField` address (`0` when inactive).
    pub target_field_addr: usize,
    /// Non-zero when initialized in `REPLACING` mode (`cob_inspect_init(_, 1)`).
    pub replacing_mode: bool,
    /// Pending `BEFORE INITIAL` bytes for the current sub-clause.
    pub pending_before: Option<Vec<u8>>,
    /// Pending `AFTER INITIAL` bytes for the current sub-clause.
    pub pending_after: Option<Vec<u8>>,
    /// Staged `TALLYING` / `REPLACING` / `CONVERTING` operations.
    pub ops: Vec<StagedAbiInspectOp>,
}

/// Open byte-stream file entry managed by `CBL_OPEN_FILE` / `CBL_CREATE_FILE`.
#[derive(Debug)]
pub struct OpenByteStreamFile {
    /// Underlying OS [`File`] handle.
    pub file: File,
}

/// Process-wide C-ABI frame, staging, and byte-stream file table.
#[derive(Debug)]
pub struct CAbiRuntimeState {
    /// Active [`CobModule`] / [`cob_call`] frame stack.
    pub module_stack: Vec<ActiveModuleFrame>,
    /// Most recent unresolved module name for [`cob_call_error`].
    pub last_unresolved_call: String,
    /// Controls whether [`cob_stop_run`] terminates the OS process (`true` in production).
    pub stop_run_exit_enabled: bool,
    /// Open `CBL_*_FILE` byte-stream handles keyed by non-zero 32-bit handle ID.
    pub byte_files: BTreeMap<u32, OpenByteStreamFile>,
    /// Next monotonic handle ID for `CBL_OPEN_FILE` / `CBL_CREATE_FILE`.
    pub next_file_handle: u32,
    /// Active `cob_string_*` staging state.
    pub string_session: CobStringAbiSession,
    /// Active `cob_unstring_*` staging state.
    pub unstring_session: CobUnstringAbiSession,
    /// Active `cob_inspect_*` staging state.
    pub inspect_session: CobInspectAbiSession,
    /// Negative lookup cache of `(target_name, COB_LIBRARY_PATH)` avoiding repeated `dlopen` probes.
    pub unresolved_cache: std::collections::BTreeSet<(String, Option<String>)>,
}

impl Default for CAbiRuntimeState {
    /// Creates a fresh [`CAbiRuntimeState`] with process exit enabled and handle counter at `1`.
    fn default() -> Self {
        Self {
            module_stack: Vec::new(),
            last_unresolved_call: String::new(),
            stop_run_exit_enabled: true,
            byte_files: BTreeMap::new(),
            next_file_handle: 1,
            string_session: CobStringAbiSession::default(),
            unstring_session: CobUnstringAbiSession::default(),
            inspect_session: CobInspectAbiSession::default(),
            unresolved_cache: std::collections::BTreeSet::new(),
        }
    }
}

impl CAbiRuntimeState {
    /// Resets transient C-ABI frame, staging, and open byte-stream handles (`cob_init` / `cob_tidy`).
    pub fn reset(&mut self) {
        self.module_stack.clear();
        self.last_unresolved_call.clear();
        self.byte_files.clear();
        self.next_file_handle = 1;
        self.string_session = CobStringAbiSession::default();
        self.unstring_session = CobUnstringAbiSession::default();
        self.inspect_session = CobInspectAbiSession::default();
        self.unresolved_cache.clear();
    }
}

/// Opaque non-null token exposed to C callers requesting `*cob_glob_ptr` in [`cob_module_enter`].
static GLOBAL_RUNTIME_TOKEN: u8 = 1;

/// Returns a reference to the process-wide [`CAbiRuntimeState`] mutex.
#[must_use]
pub fn c_abi_runtime_state() -> &'static Mutex<CAbiRuntimeState> {
    /// Singleton C-ABI runtime state mutex.
    static STATE: OnceLock<Mutex<CAbiRuntimeState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(CAbiRuntimeState::default()))
}

/// Acquires [`c_abi_runtime_state`], recovering cleanly if poisoned.
fn lock_c_abi_state() -> MutexGuard<'static, CAbiRuntimeState> {
    match c_abi_runtime_state().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Executes `op` against [`c_abi_runtime_state`], recovering cleanly if poisoned.
///
/// # Arguments
///
/// * `op` - Closure mutating [`CAbiRuntimeState`].
pub fn with_c_abi_state<R, F>(op: F) -> R
where
    F: FnOnce(&mut CAbiRuntimeState) -> R,
{
    let mut guard = lock_c_abi_state();
    op(&mut guard)
}

/// Converts a nullable C string pointer into an owned `String` (`""` when null).
///
/// # Arguments
///
/// * `ptr` - Nullable NUL-terminated C string pointer.
///
/// # Safety
///
/// When non-null, `ptr` must point to a valid NUL-terminated C string.
#[must_use]
#[expect(
    unsafe_code,
    reason = "Audited nullable C-string helper for `CobModule` and `cob_*` entrypoints."
)]
pub unsafe fn c_str_or_empty(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        // SAFETY: Caller guarantees non-null `ptr` references a NUL-terminated C string.
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Initializes the COBOL runtime from a non-COBOL `main()` host (`cob_init_nomain`).
///
/// # Arguments
///
/// * `arg_count` - Number of `argv` entries (`0` when none).
/// * `arg_values` - Pointer to `arg_count` C string pointers, or null.
///
/// # Safety
///
/// When `arg_count > 0` and `arg_values` is non-null, `arg_values` must reference `arg_count`
/// valid NUL-terminated C string pointers.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_init_nomain` C-ABI symbol forwarding to `cob_init`."
)]
pub unsafe extern "C" fn cob_init_nomain(arg_count: i32, arg_values: *const *const c_char) {
    // SAFETY: Same preconditions as `cob_init`.
    unsafe {
        cob_init(arg_count, arg_values);
    }
}

/// Flushes runtime state and terminates the COBOL run unit with `status` (`cob_stop_run`).
///
/// # Arguments
///
/// * `status` - Process exit status code.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_stop_run` C-ABI symbol."
)]
pub extern "C" fn cob_stop_run(status: i32) {
    let code = coblang_rt_prepare_stop_run(status);
    cob_tidy();
    let should_exit = with_c_abi_state(|st| st.stop_run_exit_enabled);
    if should_exit {
        std::process::exit(code);
    }
}

/// Enters a compiled COBOL module frame and links `*module` onto the active module chain (`cob_module_enter`).
///
/// # Arguments
///
/// * `module` - Pointer to caller's `*mut CobModule` slot, or null.
/// * `cob_glob_ptr` - Optional out-pointer receiving an opaque non-null runtime handle, or null.
/// * `num_params` - Number of procedure parameters supplied by caller.
///
/// # Safety
///
/// Non-null `module` (and non-null `*module`) must reference writable [`CobModule`] storage;
/// non-null `cob_glob_ptr` must reference a writable `*mut c_void` slot.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_module_enter` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_module_enter(
    module: *mut *mut CobModule,
    cob_glob_ptr: *mut *mut c_void,
    num_params: i32,
) {
    if !cob_glob_ptr.is_null() {
        // SAFETY: Validated non-null `cob_glob_ptr`.
        unsafe {
            *cob_glob_ptr = (&raw const GLOBAL_RUNTIME_TOKEN)
                .cast_mut()
                .cast::<c_void>();
        }
    }
    let mut mod_ptr_usize = 0_usize;
    let mut mod_name = String::new();
    let mut mod_src = String::new();
    let mut param_ptrs = Vec::new();
    let mut param_sizes = Vec::new();

    if !module.is_null() {
        // SAFETY: Validated non-null `module`.
        let cur_mod = unsafe { *module };
        if !cur_mod.is_null() {
            let prev_top =
                with_c_abi_state(|st| st.module_stack.last().map_or(0, |frame| frame.module_ptr));
            // SAFETY: Validated non-null `cur_mod`.
            unsafe {
                (*cur_mod).next = prev_top as *mut CobModule;
                (*cur_mod).module_num_params = num_params;
                mod_ptr_usize = cur_mod as usize;
                mod_name = c_str_or_empty((*cur_mod).module_name);
                mod_src = c_str_or_empty((*cur_mod).module_source);
                if let Ok(n) = usize::try_from(num_params)
                    && n > 0
                    && !(*cur_mod).cob_procedure_params.is_null()
                {
                    let params = core::slice::from_raw_parts((*cur_mod).cob_procedure_params, n);
                    for &fp in params {
                        if fp.is_null() {
                            param_ptrs.push(0);
                            param_sizes.push(0);
                        } else {
                            param_ptrs.push((*fp).data as usize);
                            param_sizes.push((*fp).size);
                        }
                    }
                }
            }
        }
    }

    with_c_abi_state(|st| {
        st.module_stack.push(ActiveModuleFrame {
            module_ptr: mod_ptr_usize,
            module_name: mod_name,
            source_file: mod_src,
            section: String::new(),
            paragraph: String::new(),
            statement: String::new(),
            line: 0,
            param_ptrs,
            param_sizes,
            pushed_root_caller: false,
        });
    });
    coblang_rt_enter_call();
}

/// Leaves the active COBOL module frame (`cob_module_leave`).
///
/// # Arguments
///
/// * `module` - Leaving [`CobModule`] pointer (unused/nullable for compatibility).
///
/// # Safety
///
/// Safe for both null and non-null `module` pointers.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_module_leave` C-ABI symbol."
)]
pub unsafe extern "C" fn cob_module_leave(module: *mut CobModule) {
    let _ = module;
    with_c_abi_state(|st| {
        let _ = st.module_stack.pop();
    });
    coblang_rt_leave_call();
}

/// Records the current COBOL source location on the active module frame (`cob_set_location`).
///
/// # Arguments
///
/// * `sfile` - Nullable C string source file path.
/// * `sline` - 1-based source line number.
/// * `csect` - Nullable C string section name.
/// * `cpara` - Nullable C string paragraph name.
/// * `cstmt` - Nullable C string COBOL verb name.
///
/// # Safety
///
/// Each non-null C string argument must be NUL-terminated.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_set_location` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_set_location(
    sfile: *const c_char,
    sline: u32,
    csect: *const c_char,
    cpara: *const c_char,
    cstmt: *const c_char,
) {
    // SAFETY: Nullable C strings converted via `c_str_or_empty`.
    let (file_s, sect_s, para_s, stmt_s) = unsafe {
        (
            c_str_or_empty(sfile),
            c_str_or_empty(csect),
            c_str_or_empty(cpara),
            c_str_or_empty(cstmt),
        )
    };
    with_c_abi_state(|st| {
        if let Some(top) = st.module_stack.last_mut() {
            top.source_file = file_s;
            top.line = sline;
            top.section = sect_s;
            top.paragraph = para_s;
            top.statement = stmt_s;
        }
    });
}

/// Resolves subprogram `name` into a raw C function pointer, returning null and recording
/// `EC-PROGRAM-NOT-FOUND` on failure (`cob_resolve`).
///
/// # Arguments
///
/// * `name` - Nullable NUL-terminated target subprogram name.
///
/// # Safety
///
/// When non-null, `name` must point to a valid NUL-terminated C string.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_resolve` C-ABI symbol with null guard."
)]
pub unsafe extern "C" fn cob_resolve(name: *const c_char) -> *mut c_void {
    // SAFETY: Validated nullable `name` via `c_str_or_empty`.
    let target = unsafe { c_str_or_empty(name) };
    let env_lib = std::env::var("COB_LIBRARY_PATH").ok();
    if let Ok(entry) = resolve_dynamic_module(&target, env_lib.as_deref()) {
        entry.as_fn_ptr().cast_mut().cast::<c_void>()
    } else {
        with_c_abi_state(|st| {
            st.last_unresolved_call.clone_from(&target);
        });
        with_global_runtime(|rt| {
            rt.exceptions_mut()
                .raise_code(ExceptionCode::EcProgramNotFound);
        });
        core::ptr::null_mut()
    }
}

/// Formats and writes the last unresolved `CALL` diagnostic to `err_out`, updating `RETURN-CODE`
/// and raising `EC-PROGRAM-NOT-FOUND` without unwinding across FFI.
///
/// # Arguments
///
/// * `err_out` - Destination error writer.
pub fn cob_call_error_to_writer(err_out: &mut dyn Write) -> i32 {
    let missing = with_c_abi_state(|st| st.last_unresolved_call.clone());
    let label = if missing.is_empty() {
        "<unknown>"
    } else {
        missing.as_str()
    };
    let _ = writeln!(err_out, "libcob: cannot find module '{label}'");
    let _ = err_out.flush();
    with_global_runtime(|rt| {
        rt.set_return_code(-1);
        rt.exceptions_mut()
            .raise_code(ExceptionCode::EcProgramNotFound);
    });
    -1
}

/// Reports the most recent dynamic `CALL` resolution error on `stderr` without unwinding (`cob_call_error`).
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_call_error` C-ABI symbol."
)]
pub extern "C" fn cob_call_error() -> i32 {
    let mut err = io::stderr();
    cob_call_error_to_writer(&mut err)
}

/// Cancels subprogram `name`, resetting state and/or unloading its DSO (`cob_cancel`).
///
/// # Arguments
///
/// * `name` - Nullable NUL-terminated subprogram name.
///
/// # Safety
///
/// When non-null, `name` must point to a valid NUL-terminated C string.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_cancel` C-ABI symbol with null guard."
)]
pub unsafe extern "C" fn cob_cancel(name: *const c_char) {
    // SAFETY: Validated nullable `name` via `c_str_or_empty`.
    let target = unsafe { c_str_or_empty(name) };
    with_c_abi_state(|st| st.unresolved_cache.clear());
    let _ = cancel_dynamic_module(&target);
}

/// Dynamically resolves and calls subprogram `name` with `arg_count` pointer arguments (`cob_call`),
/// updating `RETURN-CODE` on completion.
///
/// # Arguments
///
/// * `name` - Nullable NUL-terminated subprogram name.
/// * `arg_count` - Number of argument pointers in `arg_values` (`<= 0` when none).
/// * `arg_values` - Pointer to `arg_count` `*mut c_void` argument pointers, or null.
///
/// # Safety
///
/// Non-null `name` must be NUL-terminated; when `arg_count > 0` and `arg_values` is non-null, `arg_values` must
/// reference `arg_count` valid pointer words compatible with the target callee's ABI.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_call` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn cob_call(
    name: *const c_char,
    arg_count: i32,
    arg_values: *const *mut c_void,
) -> i32 {
    // SAFETY: Validated nullable `name`.
    let target = unsafe { c_str_or_empty(name) };
    let mut arg_words = Vec::new();
    if let Ok(n) = usize::try_from(arg_count)
        && n > 0
        && !arg_values.is_null()
    {
        // SAFETY: Caller guarantees `arg_values` holds `n` pointer slots.
        let raw_args = unsafe { core::slice::from_raw_parts(arg_values, n) };
        arg_words.extend(raw_args.iter().map(|&p| p as usize));
    }
    if let Some(builtin_rc) =
        crate::cob_builtins_abi::try_dispatch_builtin_c_abi(&target, &arg_words)
    {
        with_global_runtime(|rt| rt.set_return_code(builtin_rc));
        return builtin_rc;
    }
    let env_lib = std::env::var("COB_LIBRARY_PATH").ok();
    let Ok(entry) = resolve_dynamic_module(&target, env_lib.as_deref()) else {
        with_c_abi_state(|st| {
            st.last_unresolved_call.clone_from(&target);
        });
        return cob_call_error();
    };
    with_c_abi_state(|st| {
        let count = arg_words.len();
        st.module_stack.push(ActiveModuleFrame {
            module_ptr: 0,
            module_name: target,
            source_file: String::new(),
            section: String::new(),
            paragraph: String::new(),
            statement: String::new(),
            line: 0,
            param_ptrs: arg_words.clone(),
            param_sizes: vec![0; count],
            pushed_root_caller: false,
        });
    });
    coblang_rt_enter_call();
    // SAFETY: `arg_words` derived from caller-supplied `arg_values` matching target C/COBOL ABI.
    let call_res = unsafe { entry.call_with_args(&arg_words) };
    coblang_rt_leave_call();
    with_c_abi_state(|st| {
        let _ = st.module_stack.pop();
    });
    match call_res {
        Ok(ret64) => {
            let [b0, b1, b2, b3, ..] = ret64.to_le_bytes();
            let rc = i32::from_le_bytes([b0, b1, b2, b3]);
            with_global_runtime(|rt| rt.set_return_code(rc));
            rc
        }
        Err(_) => cob_call_error(),
    }
}

/// Invokes subprogram `name` like [`cob_call`] while preserving the caller's `RETURN-CODE` (`cob_func`).
///
/// # Arguments
///
/// * `name` - Nullable NUL-terminated subprogram name.
/// * `arg_count` - Number of argument pointers in `arg_values`.
/// * `arg_values` - Pointer to `arg_count` `*mut c_void` argument pointers, or null.
///
/// # Safety
///
/// Upholds the same pointer invariants as [`cob_call`].
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_func` C-ABI symbol preserving `RETURN-CODE`."
)]
pub unsafe extern "C" fn cob_func(
    name: *const c_char,
    arg_count: i32,
    arg_values: *const *mut c_void,
) -> i32 {
    let saved_rc = with_global_runtime(|rt| rt.return_code());
    // SAFETY: Same preconditions as `cob_call`.
    let rc = unsafe { cob_call(name, arg_count, arg_values) };
    with_global_runtime(|rt| rt.set_return_code(saved_rc));
    rc
}

/// Allocates `size` zeroed heap bytes via [`cob_malloc`] and writes the resulting address into
/// `*baseptr` when `baseptr` is non-null (`cob_alloc_base`).
///
/// # Arguments
///
/// * `size` - Requested allocation byte count (`0` rounded up to `1`).
/// * `baseptr` - Optional out-pointer receiving the allocated block address.
///
/// # Safety
///
/// When non-null, `baseptr` must point to a writable `*mut c_void` slot.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `cob_alloc_base` C-ABI symbol with null guard."
)]
pub unsafe extern "C" fn cob_alloc_base(size: usize, baseptr: *mut *mut c_void) -> *mut c_void {
    let allocated = cob_malloc(size.max(1));
    if !baseptr.is_null() {
        // SAFETY: Validated non-null `baseptr`.
        unsafe {
            *baseptr = allocated;
        }
    }
    allocated
}

/// Returns the active frame's 0-based `USING` parameter pointer (or null when omitted) and
/// optionally writes its byte length to `*out_size`.
///
/// # Arguments
///
/// * `param_idx` - Zero-based formal parameter index.
/// * `out_size` - Optional out-pointer receiving the caller-supplied byte size.
///
/// # Safety
///
/// When non-null, `out_size` must point to writable storage for one `usize`.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `coblang_rt_get_call_param` C-ABI symbol with null guard."
)]
pub unsafe extern "C" fn coblang_rt_get_call_param(
    param_idx: usize,
    out_size: *mut usize,
) -> *mut u8 {
    let (ptr_word, sz) = with_c_abi_state(|st| {
        st.module_stack.last().map_or((0, 0), |f| {
            let p = f.param_ptrs.get(param_idx).copied().unwrap_or(0);
            let s = f.param_sizes.get(param_idx).copied().unwrap_or(0);
            (p, s)
        })
    });
    if !out_size.is_null() {
        // SAFETY: Validated non-null `out_size`.
        unsafe {
            *out_size = sz;
        }
    }
    ptr_word as *mut u8
}

/// Returns the active subprogram's parameter count (`NUMBER-OF-CALL-PARAMETERS`), or CLI `argc - 1` at root.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `coblang_rt_current_narg` C-ABI symbol."
)]
pub extern "C" fn coblang_rt_current_narg() -> i32 {
    with_c_abi_state(|st| {
        if let Some(top) = st.module_stack.last() {
            i32::try_from(top.param_ptrs.len()).unwrap_or(0)
        } else {
            with_global_runtime(|rt| i32::try_from(rt.args().len().saturating_sub(1)).unwrap_or(0))
        }
    })
}

/// Copies 1-based CLI argument `idx1_based` into `buf[..cap]` for `PROCEDURE DIVISION CHAINING`,
/// returning the copied byte count (`>= 0`) or `-1` when no such CLI argument was supplied.
///
/// # Arguments
///
/// * `idx1_based` - 1-based CLI argument index (`1` = first user argument after `argv[0]`).
/// * `buf` - Destination byte buffer.
/// * `cap` - Capacity of `buf` in bytes.
///
/// # Safety
///
/// When `cap > 0`, non-null `buf` must point to `cap` writable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `coblang_rt_copy_cli_arg` C-ABI symbol with null guard."
)]
pub unsafe extern "C" fn coblang_rt_copy_cli_arg(
    idx1_based: usize,
    buf: *mut u8,
    cap: usize,
) -> i64 {
    let Some(arg_str) = with_global_runtime(|rt| rt.args().get(idx1_based).cloned()) else {
        return -1;
    };
    let n = arg_str.len().min(cap);
    if n > 0 && !buf.is_null() {
        // SAFETY: Validated non-null `buf` and `n <= arg_str.len().min(cap)`.
        unsafe {
            core::ptr::copy_nonoverlapping(arg_str.as_ptr(), buf, n);
        }
    }
    i64::try_from(n).unwrap_or(0)
}

/// Executes COBOL `CANCEL` on raw byte slice `name_ptr[..name_len]`.
///
/// # Arguments
///
/// * `name_ptr` - Target module name byte buffer.
/// * `name_len` - Byte length of `name_ptr`.
///
/// # Safety
///
/// When `name_len > 0`, non-null `name_ptr` must reference `name_len` readable bytes.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `coblang_rt_cancel_by_slice` C-ABI symbol with null guard."
)]
pub unsafe extern "C" fn coblang_rt_cancel_by_slice(name_ptr: *const u8, name_len: usize) {
    if name_len == 0 || name_ptr.is_null() {
        return;
    }
    // SAFETY: Validated non-null `name_ptr` and `name_len > 0`.
    let raw = unsafe { core::slice::from_raw_parts(name_ptr, name_len) };
    let nul_cut = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let head = raw.get(..nul_cut).unwrap_or(&[]);
    let trimmed = String::from_utf8_lossy(head).trim().to_owned();
    with_c_abi_state(|st| st.unresolved_cache.clear());
    let _ = cancel_dynamic_module(&trimmed);
}

/// Unpacks `(arg_words, arg_sizes, be_flags)` from `packed_meta` (`2 * argc` `u64` words).
///
/// # Arguments
///
/// * `argc` - Argument count.
/// * `packed_meta` - Interleaved `[ptr_word, size | (be_flag << 56), ...]` buffer, or null.
///
/// # Safety
///
/// When `argc > 0` and `packed_meta` is non-null, `packed_meta` must reference `2 * argc` readable `u64`s.
#[expect(
    unsafe_code,
    reason = "Audited unpacking of compiler-emitted `CALL` argument table."
)]
unsafe fn unpack_call_meta(
    argc: usize,
    packed_meta: *const u64,
) -> (Vec<usize>, Vec<usize>, Vec<u8>) {
    if argc == 0 || packed_meta.is_null() {
        return (Vec::new(), Vec::new(), Vec::new());
    }
    let total = argc.saturating_mul(2);
    // SAFETY: Validated non-null `packed_meta` with `2 * argc` `u64` words.
    let words = unsafe { core::slice::from_raw_parts(packed_meta, total) };
    let mut ptrs = Vec::with_capacity(argc);
    let mut sizes = Vec::with_capacity(argc);
    let mut flags = Vec::with_capacity(argc);
    for &[pw, mw] in words.as_chunks::<2>().0 {
        ptrs.push(usize::try_from(pw).unwrap_or(0));
        sizes.push(usize::try_from(mw & 0x00FF_FFFF_FFFF_FFFF).unwrap_or(0));
        flags.push(((mw >> 56) & 0xFF) as u8);
    }
    (ptrs, sizes, flags)
}

/// Unified Milestone M11 `CALL` runtime dispatcher supporting static COBOL subprograms, dynamic
/// identifier/literal modules, `PROGRAM-POINTER` targets, companion C functions, and `CBL_*`/`C$*` built-ins.
///
/// # Arguments
///
/// * `caller_cstr` - Nullable NUL-terminated caller `PROGRAM-ID` string.
/// * `target_ptr` - Target name byte buffer (when `target_len_or_mode >= 0`) or raw code pointer (`-1`/`-2`).
/// * `target_len_or_mode` - `>= 0` name byte length, `-1` `PROGRAM-POINTER`, or `-2` static code pointer.
/// * `flags` - Bit `0`: caller has `ON EXCEPTION`; Bit `1`: preserve caller `RETURN-CODE` (`RETURNING OMITTED`).
/// * `argc` - Number of `CALL USING` arguments.
/// * `packed_meta` - Pointer to `2 * argc` `u64` words `[ptr_0, meta_0, ...]`, or null when `argc == 0`.
/// * `out_rc` - Optional out-pointer receiving the callee `i32` return value.
///
/// # Safety
///
/// Non-null pointers must uphold the lifetimes and ABI contracts of the invoked callee and `packed_meta`.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `coblang_rt_call_invoke` C-ABI entrypoint with full null guards."
)]
pub unsafe extern "C" fn coblang_rt_call_invoke(
    caller_cstr: *const c_char,
    target_ptr: *const u8,
    target_len_or_mode: i64,
    flags: i32,
    argc: usize,
    packed_meta: *const u64,
    out_rc: *mut i32,
) -> i32 {
    let has_on_exception = (flags & 1) != 0;
    let preserve_rc = (flags & 2) != 0;
    let saved_rc = with_global_runtime(|rt| rt.return_code());
    // SAFETY: Nullable `caller_cstr` handled by `c_str_or_empty`.
    let caller_name = unsafe { c_str_or_empty(caller_cstr) };
    // SAFETY: Caller guarantees `packed_meta` has `2 * argc` `u64` words when non-null.
    let (arg_words, arg_sizes, be_flags) = unsafe { unpack_call_meta(argc, packed_meta) };

    let pushed_caller = with_c_abi_state(|st| {
        if st.module_stack.is_empty() && !caller_name.is_empty() {
            st.module_stack.push(ActiveModuleFrame {
                module_name: caller_name,
                ..ActiveModuleFrame::default()
            });
            true
        } else {
            false
        }
    });

    let resolved = if let Ok(name_len) = usize::try_from(target_len_or_mode) {
        let target_name = if name_len == 0 || target_ptr.is_null() {
            String::new()
        } else {
            // SAFETY: Validated non-null `target_ptr` and `name_len > 0`.
            let raw = unsafe { core::slice::from_raw_parts(target_ptr, name_len) };
            let nul_cut = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            String::from_utf8_lossy(raw.get(..nul_cut).unwrap_or(&[]))
                .trim()
                .to_owned()
        };
        // SAFETY: `arg_words` and `arg_sizes` match caller-supplied argument descriptors.
        if let Some(b_rc) = unsafe {
            crate::cob_builtins_abi::try_dispatch_builtin_with_meta(
                &target_name,
                &arg_words,
                &arg_sizes,
                &be_flags,
            )
        } {
            finalize_call_invoke(b_rc, saved_rc, preserve_rc, pushed_caller, out_rc);
            return 0;
        }
        resolve_dynamic_cached(target_name)
    } else {
        // SAFETY: Mode `-1`/`-2` supplies a candidate C-ABI code pointer (or null).
        unsafe { coblang_dynload::ResolvedEntrySymbol::from_raw_code_ptr(target_ptr) }
            .map(|e| (e, String::from("PTR")))
            .ok_or_else(|| String::from("NULL"))
    };

    let (entry, invoked_label) = match resolved {
        Ok(pair) => pair,
        Err(missing) => {
            with_c_abi_state(|st| st.last_unresolved_call = missing);
            if pushed_caller {
                with_c_abi_state(|st| {
                    let _ = st.module_stack.pop();
                });
            }
            let _ = has_on_exception;
            with_global_runtime(|rt| {
                rt.exceptions_mut()
                    .raise_code(ExceptionCode::EcProgramNotFound);
            });
            return -1;
        }
    };

    with_c_abi_state(|st| {
        st.module_stack.push(ActiveModuleFrame {
            module_name: invoked_label,
            param_ptrs: arg_words.clone(),
            param_sizes: arg_sizes,
            ..ActiveModuleFrame::default()
        });
    });
    coblang_rt_enter_call();
    // SAFETY: Validated `entry` and `arg_words` matching target C/COBOL ABI.
    let call_res = unsafe { entry.call_with_args(&arg_words) };
    coblang_rt_leave_call();
    with_c_abi_state(|st| {
        let _ = st.module_stack.pop();
    });
    if let Ok(ret64) = call_res {
        let [b0, b1, b2, b3, ..] = ret64.to_le_bytes();
        let rc = i32::from_le_bytes([b0, b1, b2, b3]);
        finalize_call_invoke(rc, saved_rc, preserve_rc, pushed_caller, out_rc);
        0
    } else {
        if pushed_caller {
            with_c_abi_state(|st| {
                let _ = st.module_stack.pop();
            });
        }
        with_global_runtime(|rt| {
            rt.exceptions_mut()
                .raise_code(ExceptionCode::EcProgramNotFound);
        });
        -1
    }
}

/// Restores caller stack frame and stores callee return code into `*out_rc` and `RETURN-CODE`.
///
/// # Arguments
///
/// * `rc` - Callee return code.
/// * `saved_rc` - Caller's prior `RETURN-CODE`.
/// * `preserve_rc` - Whether `RETURNING OMITTED` preserves `saved_rc`.
/// * `pushed_caller` - Whether a synthetic root caller frame was pushed.
/// * `out_rc` - Optional out-pointer receiving `rc`.
#[expect(
    unsafe_code,
    reason = "Audited write to validated optional `out_rc` pointer."
)]
fn finalize_call_invoke(
    rc: i32,
    saved_rc: i32,
    preserve_rc: bool,
    pushed_caller: bool,
    out_rc: *mut i32,
) {
    if pushed_caller {
        with_c_abi_state(|st| {
            let _ = st.module_stack.pop();
        });
    }
    if !out_rc.is_null() {
        // SAFETY: Validated non-null `out_rc`.
        unsafe {
            *out_rc = rc;
        }
    }
    with_global_runtime(|rt| {
        rt.set_return_code(if preserve_rc { saved_rc } else { rc });
    });
}

/// Resolves `target_name` via [`resolve_dynamic_module`] with negative lookup caching.
///
/// # Arguments
///
/// * `target_name` - Trimmed dynamic module name.
fn resolve_dynamic_cached(
    target_name: String,
) -> Result<(coblang_dynload::ResolvedEntrySymbol, String), String> {
    let env_lib = std::env::var("COB_LIBRARY_PATH").ok();
    let cache_key = (target_name.clone(), env_lib.clone());
    let known_missing = with_c_abi_state(|st| st.unresolved_cache.contains(&cache_key));
    if known_missing {
        return Err(target_name);
    }
    resolve_dynamic_module(&target_name, env_lib.as_deref())
        .map(|e| (e, target_name.clone()))
        .map_err(|_| {
            with_c_abi_state(|st| {
                let _ = st.unresolved_cache.insert(cache_key);
            });
            target_name
        })
}

/// Pushes an [`ActiveModuleFrame`] prior to a static in-module COBOL `CALL`, returning the caller's
/// prior `RETURN-CODE`.
///
/// # Arguments
///
/// * `caller_cstr` - Nullable NUL-terminated caller `PROGRAM-ID`.
/// * `target_cstr` - Nullable NUL-terminated target `PROGRAM-ID`.
/// * `argc` - Number of arguments.
/// * `packed_meta` - Interleaved `[ptr_word, meta_word, ...]` table (`2 * argc` `u64`s), or null.
///
/// # Safety
///
/// Non-null C strings must be NUL-terminated; non-null `packed_meta` must hold `2 * argc` `u64`s.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `coblang_rt_push_call_frame` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn coblang_rt_push_call_frame(
    caller_cstr: *const c_char,
    target_cstr: *const c_char,
    argc: usize,
    packed_meta: *const u64,
) -> i32 {
    let saved_rc = with_global_runtime(|rt| rt.return_code());
    // SAFETY: Nullable C strings handled by `c_str_or_empty`.
    let caller_name = unsafe { c_str_or_empty(caller_cstr) };
    // SAFETY: Nullable C strings handled by `c_str_or_empty`.
    let target_name = unsafe { c_str_or_empty(target_cstr) };
    // SAFETY: Caller guarantees `2 * argc` `u64`s when `packed_meta` is non-null.
    let (arg_words, arg_sizes, _) = unsafe { unpack_call_meta(argc, packed_meta) };
    with_c_abi_state(|st| {
        let pushed_root = if st.module_stack.is_empty() && !caller_name.is_empty() {
            st.module_stack.push(ActiveModuleFrame {
                module_name: caller_name,
                ..ActiveModuleFrame::default()
            });
            true
        } else {
            false
        };
        st.module_stack.push(ActiveModuleFrame {
            module_name: target_name,
            param_ptrs: arg_words,
            param_sizes: arg_sizes,
            pushed_root_caller: pushed_root,
            ..ActiveModuleFrame::default()
        });
    });
    saved_rc
}

/// Pops the active static `CALL` frame and returns the callee's `RETURN-CODE` (restoring `saved_rc`
/// when `preserve_rc != 0`).
///
/// # Arguments
///
/// * `saved_rc` - Caller's saved `RETURN-CODE` from [`coblang_rt_push_call_frame`].
/// * `preserve_rc` - Non-zero when `RETURNING OMITTED` preserves `saved_rc`.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `coblang_rt_pop_call_frame` C-ABI symbol."
)]
pub extern "C" fn coblang_rt_pop_call_frame(saved_rc: i32, preserve_rc: i32) -> i32 {
    let callee_rc = with_global_runtime(|rt| rt.return_code());
    with_c_abi_state(|st| {
        if let Some(popped) = st.module_stack.pop()
            && popped.pushed_root_caller
        {
            let _ = st.module_stack.pop();
        }
    });
    if preserve_rc != 0 {
        with_global_runtime(|rt| rt.set_return_code(saved_rc));
    }
    callee_rc
}

/// Copies `len` raw bytes from `src` to non-overlapping `dst` (`BY CONTENT` stack staging).
///
/// # Arguments
///
/// * `dst` - Destination byte buffer.
/// * `src` - Source byte buffer.
/// * `len` - Byte count to copy.
///
/// # Safety
///
/// When `len > 0` and both pointers are non-null, `src[..len]` and `dst[..len]` must be valid and non-overlapping.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "Exported clean-room `coblang_rt_raw_copy` C-ABI symbol with null guards."
)]
pub unsafe extern "C" fn coblang_rt_raw_copy(dst: *mut u8, src: *const u8, len: usize) {
    if len == 0 || dst.is_null() || src.is_null() {
        return;
    }
    // SAFETY: Validated non-null `dst`/`src` and caller guarantees `len` non-overlapping bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(src, dst, len);
    }
}
