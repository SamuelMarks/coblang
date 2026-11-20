//! Audited POSIX `dlopen`/`dlsym` dynamic COBOL module loader for `CALL` and `cobcrun`.

/// Audited in-binary and shared-library dynamic `CALL` / `CANCEL` module registry.
pub mod registry;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use coblang_common::{CoblangError, Result};
pub use registry::{
    CancelOutcome, DynamicModuleRegistry, cancel_dynamic_module, clear_dynamic_module_registry,
    global_module_registry, register_in_binary_module, resolve_dynamic_module,
    set_extra_module_search_dirs, with_module_registry,
};

#[cfg(target_os = "macos")]
/// `RTLD_NOW | RTLD_LOCAL` flags on macOS (`2 | 4`).
const RTLD_OPEN_FLAGS: c_int = 6;

#[cfg(target_os = "macos")]
/// `RTLD_NOW | RTLD_GLOBAL` flags on macOS (`2 | 8`).
const RTLD_GLOBAL_OPEN_FLAGS: c_int = 10;

#[cfg(not(target_os = "macos"))]
/// `RTLD_NOW | RTLD_LOCAL` flags on Linux/ELF (`2 | 0`).
const RTLD_OPEN_FLAGS: c_int = 2;

#[cfg(not(target_os = "macos"))]
/// `RTLD_NOW | RTLD_GLOBAL` flags on Linux/ELF (`2 | 256`).
const RTLD_GLOBAL_OPEN_FLAGS: c_int = 258;

#[expect(
    unsafe_code,
    reason = "POSIX dynamic linker C-ABI declarations (`dlopen`, `dlsym`, `dlclose`, `dlerror`)."
)]
unsafe extern "C" {
    /// Opens a dynamic shared object or returns a handle to the main program when `filename` is null.
    ///
    /// # Arguments
    ///
    /// * `filename` - NUL-terminated library path or null.
    /// * `flags` - `RTLD_*` mode flags.
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    /// Resolves a symbol address inside `handle`.
    ///
    /// # Arguments
    ///
    /// * `handle` - Open `dlopen` handle.
    /// * `symbol` - NUL-terminated symbol name.
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    /// Decrements reference count on `handle`.
    ///
    /// # Arguments
    ///
    /// * `handle` - Open `dlopen` handle to release.
    fn dlclose(handle: *mut c_void) -> c_int;
    /// Returns the most recent human-readable dynamic linker diagnostic string, or null.
    fn dlerror() -> *const c_char;
}

/// Symbol mangling convention for COBOL `PROGRAM-ID`, uniform thunks, `CANCEL` hooks, and C companions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolMangleStyle {
    /// Canonical collision-free COBOL entrypoint (`__coblang_prog_<ENCODED>`).
    CobolCanonical,
    /// Uniform `(argc: i32, argv: *const usize) -> i64` COBOL entry thunk (`__coblang_thunk_<ENCODED>`).
    CobolThunk,
    /// Zero-argument `CANCEL` state-reset hook (`__coblang_cancel_<ENCODED>`).
    CobolCancel,
    /// Unprefixed C identifier (`-` / `$` mapped to `_`) for companion C functions.
    RawC,
}

/// Encodes `trimmed` with uppercase ASCII alphanumerics and `_XX_` hex escapes after `prefix`.
///
/// # Arguments
///
/// * `prefix` - Symbol namespace prefix (`__coblang_prog_`, `__coblang_thunk_`, or `__coblang_cancel_`).
/// * `trimmed` - Whitespace-trimmed logical COBOL identifier.
#[must_use]
fn encode_prefixed_cobol_symbol(prefix: &str, trimmed: &str) -> String {
    let mut out = String::from(prefix);
    for byte in trimmed.bytes() {
        let upper = byte.to_ascii_uppercase();
        if upper.is_ascii_alphanumeric() {
            out.push(char::from(upper));
        } else {
            let _ = write!(out, "_{byte:02X}_");
        }
    }
    out
}

/// Mangles a COBOL program or entry identifier according to `style`.
///
/// # Arguments
///
/// * `raw_name` - Unquoted COBOL `PROGRAM-ID` or `CALL` literal string.
/// * `style` - Canonical COBOL prefix, thunk, cancel hook, or plain C identifier convention.
#[must_use]
pub fn mangle_program_symbol(raw_name: &str, style: SymbolMangleStyle) -> String {
    let trimmed = raw_name.trim();
    match style {
        SymbolMangleStyle::CobolCanonical => {
            encode_prefixed_cobol_symbol("__coblang_prog_", trimmed)
        }
        SymbolMangleStyle::CobolThunk => encode_prefixed_cobol_symbol("__coblang_thunk_", trimmed),
        SymbolMangleStyle::CobolCancel => {
            encode_prefixed_cobol_symbol("__coblang_cancel_", trimmed)
        }
        SymbolMangleStyle::RawC => {
            let mut out = String::with_capacity(trimmed.len());
            for ch in trimmed.chars() {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    out.push(ch);
                } else {
                    out.push('_');
                }
            }
            out
        }
    }
}

/// Uniform C-ABI thunk signature `(argc, argv_words) -> return_word` emitted for COBOL entrypoints.
pub type CobolThunkFn = extern "C" fn(i32, *const usize) -> i64;

/// Zero-argument C-ABI hook resetting static `WORKING-STORAGE` and `ALTER` state upon `CANCEL`.
pub type CancelResetFn = extern "C" fn();

/// RAII handle owning an opened POSIX shared object (`dlclose` on drop).
#[derive(Debug)]
pub struct LoadedModuleHandle {
    /// Non-null opaque `dlopen` handle.
    raw: NonNull<c_void>,
}

#[expect(
    unsafe_code,
    reason = "POSIX `dlopen`/`dlclose` handles are process-wide and thread-safe to transfer across threads."
)]
// SAFETY: POSIX guarantees `dlopen` / `dlclose` operate on the process-wide dynamic loader table with internal synchronization.
unsafe impl Send for LoadedModuleHandle {}

impl Drop for LoadedModuleHandle {
    /// Releases the underlying POSIX dynamic library handle via `dlclose`.
    fn drop(&mut self) {
        #[expect(
            unsafe_code,
            reason = "Audited RAII release of valid non-null `dlopen` handle."
        )]
        // SAFETY: `self.raw` was returned by a successful `dlopen` call and is closed once in `Drop`.
        unsafe {
            let _ = dlclose(self.raw.as_ptr());
        }
    }
}

/// Direct C-ABI 0-arg companion function signature.
type RawCFn0 = extern "C" fn() -> isize;
/// Direct C-ABI 1-arg companion function signature.
type RawCFn1 = extern "C" fn(usize) -> isize;
/// Direct C-ABI 2-arg companion function signature.
type RawCFn2 = extern "C" fn(usize, usize) -> isize;
/// Direct C-ABI 3-arg companion function signature.
type RawCFn3 = extern "C" fn(usize, usize, usize) -> isize;
/// Direct C-ABI 4-arg companion function signature.
type RawCFn4 = extern "C" fn(usize, usize, usize, usize) -> isize;
/// Direct C-ABI 5-arg companion function signature.
type RawCFn5 = extern "C" fn(usize, usize, usize, usize, usize) -> isize;
/// Direct C-ABI 6-arg companion function signature.
type RawCFn6 = extern "C" fn(usize, usize, usize, usize, usize, usize) -> isize;

/// Typed COBOL or C module entrypoint resolved from an in-binary registration, shared object, or main binary.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedEntrySymbol {
    /// Underlying C-ABI zero-argument function pointer returning `i32`.
    func: extern "C" fn() -> i32,
    /// Optional arity-generic COBOL dispatcher thunk (`__coblang_thunk_<ENCODED>`).
    thunk: Option<CobolThunkFn>,
    /// Optional `CANCEL` state-reset hook (`__coblang_cancel_<ENCODED>`).
    cancel_hook: Option<CancelResetFn>,
}

impl ResolvedEntrySymbol {
    /// Wraps a zero-argument `extern "C" fn() -> i32` function pointer.
    ///
    /// # Arguments
    ///
    /// * `func` - Direct C-ABI entrypoint function.
    #[must_use]
    pub const fn from_fn(func: extern "C" fn() -> i32) -> Self {
        Self {
            func,
            thunk: None,
            cancel_hook: None,
        }
    }

    /// Wraps a COBOL entrypoint together with its uniform [`CobolThunkFn`] and optional [`CancelResetFn`].
    ///
    /// # Arguments
    ///
    /// * `func` - Direct C-ABI entrypoint function.
    /// * `thunk` - Uniform `(argc, argv)` dispatcher thunk.
    /// * `cancel_hook` - Optional `CANCEL` state-reset callback.
    #[must_use]
    pub const fn from_thunk(
        func: extern "C" fn() -> i32,
        thunk: CobolThunkFn,
        cancel_hook: Option<CancelResetFn>,
    ) -> Self {
        Self {
            func,
            thunk: Some(thunk),
            cancel_hook,
        }
    }

    /// Constructs a [`ResolvedEntrySymbol`] from a raw function pointer (`CALL prog-ptr`), returning
    /// `None` if `code_ptr` is null.
    ///
    /// # Arguments
    ///
    /// * `code_ptr` - Candidate C-ABI code pointer, or null.
    ///
    /// # Safety
    ///
    /// When non-null, `code_ptr` must point to a valid C-ABI function whose module remains loaded
    /// for the duration of any call.
    #[must_use]
    #[expect(
        unsafe_code,
        reason = "Audited C function-pointer reconstruction for `PROGRAM-POINTER` indirect `CALL`."
    )]
    pub unsafe fn from_raw_code_ptr(code_ptr: *const u8) -> Option<Self> {
        let nn = NonNull::new(code_ptr.cast_mut().cast::<c_void>())?;
        // SAFETY: Caller guarantees non-null `code_ptr` is a valid C-ABI code entrypoint.
        let func: extern "C" fn() -> i32 = unsafe { core::mem::transmute(nn.as_ptr()) };
        Some(Self::from_fn(func))
    }

    /// Invokes the resolved COBOL module entrypoint with zero arguments and returns its `i32` status.
    #[must_use]
    pub fn call(self) -> i32 {
        if let Some(thunk_fn) = self.thunk {
            let ret = thunk_fn(0, core::ptr::null());
            let [b0, b1, b2, b3, ..] = ret.to_le_bytes();
            i32::from_le_bytes([b0, b1, b2, b3])
        } else {
            (self.func)()
        }
    }

    /// Invokes the resolved entrypoint passing `args` (`usize` pointer or `BY VALUE` scalar words).
    ///
    /// Uses [`CobolThunkFn`] when available (supporting arbitrary parameter counts), or dispatches
    /// `0..=6` direct C-ABI pointer/word arguments for companion C functions.
    ///
    /// # Arguments
    ///
    /// * `args` - Slice of pointer/scalar argument words.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DynamicModuleLoad`] if `self` has no COBOL thunk and `args.len() > 6`.
    ///
    /// # Safety
    ///
    /// When `self` wraps a raw C companion function without a COBOL thunk, the target C function's
    /// ABI parameter count and word/pointer parameter types must be compatible with `args`.
    #[expect(
        unsafe_code,
        reason = "Audited C-ABI companion arity dispatch across `0..=6` word/pointer arguments."
    )]
    pub unsafe fn call_with_args(self, args: &[usize]) -> Result<i64> {
        if let Some(thunk_fn) = self.thunk {
            let word_count = i32::try_from(args.len()).unwrap_or(i32::MAX);
            return Ok(thunk_fn(word_count, args.as_ptr()));
        }
        let code = self.func as *const ();
        let ret: isize = match *args {
            [] => {
                // SAFETY: Caller guarantees 0-arg C-ABI compatibility.
                let f: RawCFn0 = unsafe { core::mem::transmute(code) };
                f()
            }
            [a0] => {
                // SAFETY: Caller guarantees 1-arg C-ABI compatibility.
                let f: RawCFn1 = unsafe { core::mem::transmute(code) };
                f(a0)
            }
            [a0, a1] => {
                // SAFETY: Caller guarantees 2-arg C-ABI compatibility.
                let f: RawCFn2 = unsafe { core::mem::transmute(code) };
                f(a0, a1)
            }
            [a0, a1, a2] => {
                // SAFETY: Caller guarantees 3-arg C-ABI compatibility.
                let f: RawCFn3 = unsafe { core::mem::transmute(code) };
                f(a0, a1, a2)
            }
            [a0, a1, a2, a3] => {
                // SAFETY: Caller guarantees 4-arg C-ABI compatibility.
                let f: RawCFn4 = unsafe { core::mem::transmute(code) };
                f(a0, a1, a2, a3)
            }
            [a0, a1, a2, a3, a4] => {
                // SAFETY: Caller guarantees 5-arg C-ABI compatibility.
                let f: RawCFn5 = unsafe { core::mem::transmute(code) };
                f(a0, a1, a2, a3, a4)
            }
            [a0, a1, a2, a3, a4, a5] => {
                // SAFETY: Caller guarantees 6-arg C-ABI compatibility.
                let f: RawCFn6 = unsafe { core::mem::transmute(code) };
                f(a0, a1, a2, a3, a4, a5)
            }
            _ => {
                return Err(CoblangError::DynamicModuleLoad {
                    module_name: String::from("<c-companion>"),
                    searched_paths: String::new(),
                    message: format!(
                        "direct C target call with {} arguments exceeds 6-arg companion limit",
                        args.len()
                    ),
                });
            }
        };
        Ok(ret as i64)
    }

    /// Executes the module's optional `CANCEL` state-reset hook, returning `true` when present.
    #[must_use]
    pub fn reset_state(self) -> bool {
        if let Some(hook) = self.cancel_hook {
            hook();
            true
        } else {
            false
        }
    }

    /// Returns `true` if this entrypoint carries an arity-generic [`CobolThunkFn`].
    #[must_use]
    pub const fn has_thunk(self) -> bool {
        self.thunk.is_some()
    }

    /// Returns `true` if this entrypoint carries a [`CancelResetFn`] hook.
    #[must_use]
    pub const fn has_cancel_hook(self) -> bool {
        self.cancel_hook.is_some()
    }

    /// Returns the raw code address of the resolved entrypoint as `*const u8` (`SET prog-ptr TO ENTRY`).
    #[must_use]
    pub fn as_fn_ptr(self) -> *const u8 {
        (self.func as *const ()).cast::<u8>()
    }
}

/// Formats a raw `dlerror()` pointer (`null` or NUL-terminated C string) into an owned message.
///
/// # Arguments
///
/// * `ptr` - Pointer returned by `dlerror()`, or null.
///
/// # Safety
///
/// When non-null, `ptr` must reference a valid NUL-terminated C string.
#[must_use]
#[expect(
    unsafe_code,
    reason = "Audited C-string conversion of `dlerror()` result pointer."
)]
pub unsafe fn format_dlerror_ptr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        String::from("unknown dynamic linker error")
    } else {
        // SAFETY: Caller guarantees non-null `ptr` points to a valid NUL-terminated C string.
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Reads the most recent `dlerror()` string if non-null.
#[must_use]
fn last_dlerror_message() -> String {
    #[expect(
        unsafe_code,
        reason = "Audited inspection of thread-local `dlerror()` C string."
    )]
    // SAFETY: `dlerror` returns either null or a valid thread-local NUL-terminated C string.
    unsafe {
        format_dlerror_ptr(dlerror())
    }
}

/// Looks up a single mangled ASCII symbol inside `raw_handle`.
///
/// # Arguments
///
/// * `raw_handle` - Valid open `dlopen` handle.
/// * `mangled` - Mangled ASCII symbol string without interior NUL bytes.
fn dlsym_mangled_ptr(raw_handle: NonNull<c_void>, mangled: &str) -> Option<NonNull<c_void>> {
    let c_bytes = nul_terminated_mangled_bytes(mangled);
    #[expect(
        unsafe_code,
        reason = "Audited `dlsym` lookup on validated `dlopen` handle and NUL-terminated buffer."
    )]
    // SAFETY: `raw_handle` is a valid open `dlopen` handle and `c_bytes` ends with a trailing `0` byte.
    unsafe {
        let _ = dlerror();
        NonNull::new(dlsym(
            raw_handle.as_ptr(),
            c_bytes.as_ptr().cast::<c_char>(),
        ))
    }
}

/// Converts raw `dlsym` pointers into a [`ResolvedEntrySymbol`].
///
/// # Arguments
///
/// * `sym_ptr` - Non-null primary symbol address.
/// * `thunk_ptr` - Optional `__coblang_thunk_*` address.
/// * `cancel_ptr` - Optional `__coblang_cancel_*` address.
///
/// # Safety
///
/// Non-null pointers must reference valid `extern "C"` entrypoints with matching signatures
/// whose backing module stays loaded for the lifetime of any invocation.
#[expect(
    unsafe_code,
    reason = "Audited function-pointer casts of `dlsym` symbol addresses."
)]
unsafe fn symbol_from_raw_ptrs(
    sym_ptr: NonNull<c_void>,
    thunk_ptr: Option<NonNull<c_void>>,
    cancel_ptr: Option<NonNull<c_void>>,
) -> ResolvedEntrySymbol {
    // SAFETY: Caller guarantees `sym_ptr` is a valid C-ABI code entrypoint.
    let func: extern "C" fn() -> i32 = unsafe { core::mem::transmute(sym_ptr.as_ptr()) };
    // SAFETY: Caller guarantees `thunk_ptr`, if present, is a valid `CobolThunkFn`.
    let thunk: Option<CobolThunkFn> =
        thunk_ptr.map(|p| unsafe { core::mem::transmute(p.as_ptr()) });
    // SAFETY: Caller guarantees `cancel_ptr`, if present, is a valid `CancelResetFn`.
    let cancel_hook: Option<CancelResetFn> =
        cancel_ptr.map(|p| unsafe { core::mem::transmute(p.as_ptr()) });
    ResolvedEntrySymbol {
        func,
        thunk,
        cancel_hook,
    }
}

/// Appends a trailing NUL byte to a mangled ASCII symbol (guaranteed free of interior NUL bytes).
///
/// # Arguments
///
/// * `mangled` - Mangled ASCII identifier produced by [`mangle_program_symbol`].
#[must_use]
fn nul_terminated_mangled_bytes(mangled: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(mangled.len().saturating_add(1));
    bytes.extend_from_slice(mangled.as_bytes());
    bytes.push(0);
    bytes
}

/// Resolves canonical or C-fallback entry symbol (plus optional COBOL thunk and `CANCEL` hook)
/// for `module_name` inside raw `dlopen` `handle`.
///
/// # Arguments
///
/// * `raw_handle` - Valid `dlopen` handle.
/// * `module_name` - Logical COBOL module name.
fn lookup_symbol_in_raw_handle(
    raw_handle: NonNull<c_void>,
    module_name: &str,
) -> Option<ResolvedEntrySymbol> {
    let canon = mangle_program_symbol(module_name, SymbolMangleStyle::CobolCanonical);
    let raw_c = mangle_program_symbol(module_name, SymbolMangleStyle::RawC);
    for candidate in [canon, raw_c] {
        if let Some(sym_ptr) = dlsym_mangled_ptr(raw_handle, &candidate) {
            let thunk_sym = mangle_program_symbol(module_name, SymbolMangleStyle::CobolThunk);
            let cancel_sym = mangle_program_symbol(module_name, SymbolMangleStyle::CobolCancel);
            let thunk_ptr = dlsym_mangled_ptr(raw_handle, &thunk_sym);
            let cancel_ptr = dlsym_mangled_ptr(raw_handle, &cancel_sym);
            #[expect(
                unsafe_code,
                reason = "Audited assembly of `ResolvedEntrySymbol` from `dlsym` addresses."
            )]
            // SAFETY: `sym_ptr`, `thunk_ptr`, and `cancel_ptr` were resolved from `raw_handle` via `dlsym`.
            let entry = unsafe { symbol_from_raw_ptrs(sym_ptr, thunk_ptr, cancel_ptr) };
            return Some(entry);
        }
    }
    None
}

/// Opens a specific shared-library file `lib_path` and resolves `module_name`.
///
/// # Arguments
///
/// * `lib_path` - Filesystem path to `.so` or `.dylib` module.
/// * `module_name` - Logical COBOL program/entry name to look up.
///
/// # Errors
///
/// Returns [`CoblangError::DynamicModuleLoad`] if `lib_path` contains interior NUL bytes,
/// `dlopen` fails, or neither canonical nor C entry symbols exist in the library.
pub fn load_module_from_file(
    lib_path: &Path,
    module_name: &str,
) -> Result<(LoadedModuleHandle, ResolvedEntrySymbol)> {
    let path_str = lib_path.to_string_lossy().into_owned();
    let c_path = CString::new(path_str.clone()).map_err(|_| CoblangError::DynamicModuleLoad {
        module_name: String::from(module_name),
        searched_paths: path_str.clone(),
        message: String::from("library path contains interior NUL byte"),
    })?;

    #[expect(
        unsafe_code,
        reason = "Audited `dlopen` call with NUL-terminated filesystem path."
    )]
    // SAFETY: `c_path` is a valid NUL-terminated string.
    let raw_opt = unsafe {
        let _ = dlerror();
        NonNull::new(dlopen(c_path.as_ptr(), RTLD_OPEN_FLAGS))
    };

    let Some(raw) = raw_opt else {
        return Err(CoblangError::DynamicModuleLoad {
            module_name: String::from(module_name),
            searched_paths: path_str,
            message: last_dlerror_message(),
        });
    };

    let handle = LoadedModuleHandle { raw };
    let Some(entry) = lookup_symbol_in_raw_handle(handle.raw, module_name) else {
        return Err(CoblangError::DynamicModuleLoad {
            module_name: String::from(module_name),
            searched_paths: path_str,
            message: format!("entry symbol for `{module_name}` not found in shared object"),
        });
    };
    Ok((handle, entry))
}

/// Searches the current process image (`dlopen(NULL, ...)`) for `module_name`.
///
/// # Arguments
///
/// * `module_name` - Logical COBOL program/entry name.
#[must_use]
pub fn resolve_in_current_process(
    module_name: &str,
) -> Option<(LoadedModuleHandle, ResolvedEntrySymbol)> {
    #[expect(
        unsafe_code,
        reason = "Audited `dlopen(NULL, RTLD_OPEN_FLAGS)` query against current process image."
    )]
    // SAFETY: Passing null filename to `dlopen` is standard POSIX behavior returning the global process handle.
    let raw = unsafe { NonNull::new(dlopen(core::ptr::null(), RTLD_OPEN_FLAGS)) }?;
    let handle = LoadedModuleHandle { raw };
    let entry = lookup_symbol_in_raw_handle(handle.raw, module_name)?;
    Some((handle, entry))
}

/// Builds candidate shared-object filenames for `module_name` across `.dylib` and `.so`.
///
/// # Arguments
///
/// * `module_name` - Trimmed COBOL module identifier.
#[must_use]
pub fn candidate_library_filenames(module_name: &str) -> Vec<String> {
    let trimmed = module_name.trim();
    let lower = trimmed.to_ascii_lowercase();
    let mut stems = vec![String::from(trimmed)];
    if lower != trimmed {
        stems.push(lower);
    }
    let mut out = Vec::with_capacity(stems.len().saturating_mul(4));
    for stem in stems {
        for ext in ["dylib", "so"] {
            out.push(format!("{stem}.{ext}"));
            out.push(format!("lib{stem}.{ext}"));
        }
    }
    out
}

/// Resolves and loads `module_name` across current-process symbols first, then `search_dirs`.
///
/// # Arguments
///
/// * `module_name` - Target COBOL module identifier (`CALL "mod1"`).
/// * `search_dirs` - Ordered directory list (`COB_LIBRARY_PATH` entries followed by `.`).
///
/// # Errors
///
/// Returns [`CoblangError::DynamicModuleLoad`] if no matching shared module or symbol is found.
pub fn resolve_and_load_module(
    module_name: &str,
    search_dirs: &[PathBuf],
) -> Result<(LoadedModuleHandle, ResolvedEntrySymbol)> {
    let trimmed = module_name.trim();
    if trimmed.is_empty() {
        return Err(CoblangError::DynamicModuleLoad {
            module_name: String::new(),
            searched_paths: String::new(),
            message: String::from("empty dynamic CALL module name"),
        });
    }

    let req_path = Path::new(trimmed);
    let (explicit_parent, symbol_stem) = match req_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            let stem = req_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(trimmed);
            (Some(parent), stem)
        }
        _ => (None, trimmed),
    };

    if explicit_parent.is_none()
        && let Some(found) = resolve_in_current_process(symbol_stem)
    {
        return Ok(found);
    }

    let filenames = candidate_library_filenames(symbol_stem);
    let mut searched = Vec::new();
    let mut last_err: Option<CoblangError> = None;

    let owned_parent_dirs: Vec<PathBuf>;
    let dirs_to_scan: &[PathBuf] = if let Some(parent) = explicit_parent {
        if parent.is_absolute() {
            owned_parent_dirs = vec![parent.to_path_buf()];
        } else {
            let mut v = Vec::with_capacity(search_dirs.len() + 1);
            v.push(parent.to_path_buf());
            for d in search_dirs {
                v.push(d.join(parent));
            }
            owned_parent_dirs = v;
        }
        &owned_parent_dirs
    } else {
        search_dirs
    };

    if req_path.extension().is_some() && req_path.is_file() {
        searched.push(req_path.display().to_string());
        match load_module_from_file(req_path, symbol_stem) {
            Ok(pair) => return Ok(pair),
            Err(err) => last_err = Some(err),
        }
    }

    for dir in dirs_to_scan {
        for fname in &filenames {
            let candidate = dir.join(fname);
            searched.push(candidate.display().to_string());
            if candidate.is_file() {
                match load_module_from_file(&candidate, symbol_stem) {
                    Ok(pair) => return Ok(pair),
                    Err(err) => last_err = Some(err),
                }
            }
        }
    }

    if let Some(err) = last_err {
        return Err(err);
    }

    Err(CoblangError::DynamicModuleLoad {
        module_name: String::from(trimmed),
        searched_paths: searched.join(":"),
        message: String::from("shared module file not found"),
    })
}

impl LoadedModuleHandle {
    /// Looks up a COBOL or C entrypoint named `entry_name` inside this loaded shared object.
    ///
    /// # Arguments
    ///
    /// * `entry_name` - Logical COBOL `PROGRAM-ID`, `ENTRY`, or C symbol name.
    #[must_use]
    pub fn lookup_entry(&self, entry_name: &str) -> Option<ResolvedEntrySymbol> {
        lookup_symbol_in_raw_handle(self.raw, entry_name)
    }
}

/// Opens `lib_path` with `RTLD_NOW | RTLD_GLOBAL` so its exported symbols participate in
/// subsequent `resolve_in_current_process` queries (`COB_PRE_LOAD`, `-M <dso>`, `libcoblang_rt`).
///
/// # Arguments
///
/// * `lib_path` - Path to `.dylib` or `.so` shared library.
///
/// # Errors
///
/// Returns [`CoblangError::DynamicModuleLoad`] if `lib_path` contains interior NUL bytes or `dlopen` fails.
pub fn open_shared_object_global(lib_path: &Path) -> Result<LoadedModuleHandle> {
    let path_str = lib_path.to_string_lossy().into_owned();
    let c_path = CString::new(path_str.clone()).map_err(|_| CoblangError::DynamicModuleLoad {
        module_name: path_str.clone(),
        searched_paths: path_str.clone(),
        message: String::from("library path contains interior NUL byte"),
    })?;

    #[expect(
        unsafe_code,
        reason = "Audited `dlopen` with `RTLD_GLOBAL_OPEN_FLAGS` on validated NUL-terminated path."
    )]
    // SAFETY: `c_path` is a valid NUL-terminated path string.
    let raw_opt = unsafe {
        let _ = dlerror();
        NonNull::new(dlopen(c_path.as_ptr(), RTLD_GLOBAL_OPEN_FLAGS))
    };

    let Some(raw) = raw_opt else {
        return Err(CoblangError::DynamicModuleLoad {
            module_name: path_str.clone(),
            searched_paths: path_str,
            message: last_dlerror_message(),
        });
    };
    Ok(LoadedModuleHandle { raw })
}

/// Preloads a shared object specified by `spec` (file path or bare stem) across `search_dirs`
/// with `RTLD_GLOBAL` visibility.
///
/// # Arguments
///
/// * `spec` - Shared library path or stem (`./caller`, `libfoo.so`, `COBOL`).
/// * `search_dirs` - Ordered directory list to probe.
///
/// # Errors
///
/// Returns [`CoblangError::DynamicModuleLoad`] if no matching shared object can be opened.
pub fn preload_module_by_name(spec: &str, search_dirs: &[PathBuf]) -> Result<LoadedModuleHandle> {
    let trimmed = spec.trim();
    let direct = Path::new(trimmed);
    if direct.is_file() {
        return open_shared_object_global(direct);
    }
    let (parent_opt, stem) = match direct.parent() {
        Some(p) if !p.as_os_str().is_empty() => {
            let st = direct
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(trimmed);
            (Some(p.to_path_buf()), st)
        }
        _ => (None, trimmed),
    };
    let filenames = candidate_library_filenames(stem);
    let mut dirs = Vec::new();
    if let Some(p) = parent_opt {
        if p.is_absolute() {
            dirs.push(p);
        } else {
            dirs.push(p.clone());
            for d in search_dirs {
                dirs.push(d.join(&p));
            }
        }
    } else {
        dirs.extend_from_slice(search_dirs);
    }
    let mut searched = Vec::new();
    for dir in &dirs {
        for fname in &filenames {
            let cand = dir.join(fname);
            searched.push(cand.display().to_string());
            if cand.is_file() {
                return open_shared_object_global(&cand);
            }
        }
    }
    Err(CoblangError::DynamicModuleLoad {
        module_name: String::from(trimmed),
        searched_paths: searched.join(":"),
        message: String::from("preload shared object not found"),
    })
}

/// Looks up a raw C symbol `sym_cstr` inside `handle`.
///
/// # Arguments
///
/// * `handle` - Open [`LoadedModuleHandle`].
/// * `sym_cstr` - NUL-terminated C symbol literal.
fn lookup_raw_symbol(handle: &LoadedModuleHandle, sym_cstr: &CStr) -> Result<NonNull<c_void>> {
    #[expect(
        unsafe_code,
        reason = "Audited `dlsym` lookup on open `LoadedModuleHandle` and `CStr`."
    )]
    // SAFETY: `handle.raw` is a valid open `dlopen` handle and `sym_cstr` is NUL-terminated.
    let raw_opt = unsafe {
        let _ = dlerror();
        NonNull::new(dlsym(handle.raw.as_ptr(), sym_cstr.as_ptr()))
    };
    raw_opt.ok_or_else(|| CoblangError::DynamicModuleLoad {
        module_name: sym_cstr.to_string_lossy().into_owned(),
        searched_paths: String::new(),
        message: String::from("required runtime ABI symbol missing"),
    })
}

/// Converts `text` into a NUL-terminated byte buffer (stripping any embedded NUL bytes).
///
/// # Arguments
///
/// * `text` - Source argument text.
#[must_use]
fn to_c_arg_buffer(text: &str) -> Vec<u8> {
    let mut buf: Vec<u8> = text.bytes().filter(|&b| b != 0).collect();
    buf.push(0);
    buf
}

/// Specification for a `cobcrun` dynamic module execution session.
#[derive(Debug, Clone)]
pub struct CobcrunExecSpec<'a> {
    /// Directory containing `libcoblang_rt.{dylib,so}`.
    pub runtime_lib_dir: &'a Path,
    /// Optional module names/paths to preload (`COB_PRE_LOAD`, `-c` `pre_load`, `-M <dso>`).
    pub preloads: &'a [String],
    /// Module search directories (`-M <dir>`, `COB_LIBRARY_PATH`, `.`).
    pub search_dirs: &'a [PathBuf],
    /// Requested program or DSO target.
    pub target: &'a str,
    /// Trailing CLI arguments passed to the COBOL program (or `[entry, args...]` when `target` is a DSO path).
    pub args: &'a [String],
}

/// Audited C-ABI function type for `cob_init`.
type CobInitFn = extern "C" fn(c_int, *const *const c_char);

/// Audited C-ABI function type for `cob_tidy`.
type CobTidyFn = extern "C" fn();

/// Loads `libcoblang_rt`, preloads any requested shared objects, resolves and executes `spec.target`
/// (`cob_init` -> `entry.call()` -> `cob_tidy`), and returns `(exit_status_u8, failed_preloads)`.
///
/// # Arguments
///
/// * `spec` - [`CobcrunExecSpec`] describing the runtime directory, preloads, search dirs, target, and args.
///
/// # Errors
///
/// Returns [`CoblangError::DynamicModuleLoad`] if `libcoblang_rt` or the target module cannot be resolved.
pub fn execute_cobcrun_session(spec: &CobcrunExecSpec<'_>) -> Result<(u8, Vec<String>)> {
    let rt_handle = preload_module_by_name("coblang_rt", &[spec.runtime_lib_dir.to_path_buf()])?;
    let init_sym = lookup_raw_symbol(&rt_handle, c"cob_init")?;
    let tidy_sym = lookup_raw_symbol(&rt_handle, c"cob_tidy")?;

    #[expect(
        unsafe_code,
        reason = "Audited function-pointer cast of `cob_init` and `cob_tidy` from `libcoblang_rt`."
    )]
    // SAFETY: `cob_init` and `cob_tidy` in `libcoblang_rt` have exact signatures `(c_int, *const *const c_char)` and `()`.
    let (cob_init_fn, cob_tidy_fn): (CobInitFn, CobTidyFn) = unsafe {
        (
            core::mem::transmute::<*mut c_void, CobInitFn>(init_sym.as_ptr()),
            core::mem::transmute::<*mut c_void, CobTidyFn>(tidy_sym.as_ptr()),
        )
    };

    let mut kept_handles = vec![rt_handle];
    let mut failed_preloads = Vec::new();
    for preload in spec.preloads {
        match preload_module_by_name(preload, spec.search_dirs) {
            Ok(h) => kept_handles.push(h),
            Err(_) => failed_preloads.push(preload.clone()),
        }
    }

    let (entry, eff_name, eff_args) = resolve_cobcrun_target(spec, &mut kept_handles)?;
    let mut c_bufs = Vec::with_capacity(eff_args.len().saturating_add(1));
    c_bufs.push(to_c_arg_buffer(&eff_name));
    for arg in eff_args {
        c_bufs.push(to_c_arg_buffer(arg));
    }
    let c_ptrs: Vec<*const c_char> = c_bufs.iter().map(|b| b.as_ptr().cast::<c_char>()).collect();
    let argc = c_int::try_from(c_ptrs.len()).unwrap_or(c_int::MAX);

    cob_init_fn(argc, c_ptrs.as_ptr());
    let rc = entry.call();
    cob_tidy_fn();
    drop(kept_handles);
    let [exit_byte, _, _, _] = rc.to_le_bytes();
    Ok((exit_byte, failed_preloads))
}

/// Resolves `(ResolvedEntrySymbol, effective_argv0, remaining_args)` for `spec`, supporting both
/// standard `cobcrun PROG args...` and DSO-prefixed `cobcrun ./dso entry args...`.
///
/// # Arguments
///
/// * `spec` - [`CobcrunExecSpec`].
/// * `kept_handles` - Open module handles kept alive through `cob_tidy`.
fn resolve_cobcrun_target<'a>(
    spec: &'a CobcrunExecSpec<'a>,
    kept_handles: &mut Vec<LoadedModuleHandle>,
) -> Result<(ResolvedEntrySymbol, String, &'a [String])> {
    if spec.target.contains('/')
        && let Some((first_arg, rest_args)) = spec.args.split_first()
        && let Ok(dso_handle) = preload_module_by_name(spec.target, spec.search_dirs)
    {
        if let Some(alt_entry) = dso_handle.lookup_entry(first_arg) {
            kept_handles.push(dso_handle);
            return Ok((alt_entry, first_arg.clone(), rest_args));
        }
        let stem = Path::new(spec.target)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(spec.target);
        if let Some(stem_entry) = dso_handle.lookup_entry(stem) {
            kept_handles.push(dso_handle);
            return Ok((stem_entry, String::from(stem), spec.args));
        }
    }

    let (mod_handle, entry) = resolve_and_load_module(spec.target, spec.search_dirs)?;
    kept_handles.push(mod_handle);
    Ok((entry, String::from(spec.target), spec.args))
}

#[cfg(target_os = "macos")]
/// `O_RDWR | O_NOCTTY` flags on macOS (`0x0002 | 0x20000`).
const PTY_OPEN_FLAGS: c_int = 0x0002_0002;

#[cfg(not(target_os = "macos"))]
/// `O_RDWR | O_NOCTTY` flags on Linux (`0x0002 | 0x0100`).
const PTY_OPEN_FLAGS: c_int = 0x0102;

#[cfg(target_os = "macos")]
/// `O_NONBLOCK` flag on macOS (`0x0004`).
const PTY_NONBLOCK_FLAG: c_int = 0x0004;

#[cfg(not(target_os = "macos"))]
/// `O_NONBLOCK` flag on Linux (`0x0800`).
const PTY_NONBLOCK_FLAG: c_int = 0x0800;

#[cfg(target_os = "macos")]
/// `TIOCSWINSZ` ioctl request number on macOS.
const PTY_TIOCSWINSZ: std::ffi::c_ulong = 0x8008_7467;

#[cfg(not(target_os = "macos"))]
/// `TIOCSWINSZ` ioctl request number on Linux.
const PTY_TIOCSWINSZ: std::ffi::c_ulong = 0x5414;

/// POSIX `struct winsize` passed to `ioctl(TIOCSWINSZ)`.
#[repr(C)]
struct RawPtyWinsize {
    /// Terminal row count.
    row: u16,
    /// Terminal column count.
    col: u16,
    /// Horizontal pixel width (`0`).
    xpixel: u16,
    /// Vertical pixel height (`0`).
    ypixel: u16,
}

/// Opaque aligned buffer large enough to hold `struct termios` on macOS and Linux.
#[repr(C, align(8))]
struct RawPtyTermios([u8; 128]);

#[expect(
    unsafe_code,
    reason = "POSIX pseudo-terminal C-ABI declarations (`posix_openpt`, `grantpt`, `unlockpt`, `ptsname`, `open`, `close`, `ioctl`, `fcntl`, `tcgetattr`, `cfmakeraw`, `tcsetattr`)."
)]
unsafe extern "C" {
    /// Allocates a new POSIX pseudo-terminal master descriptor.
    ///
    /// # Arguments
    ///
    /// * `flags` - `O_RDWR | O_NOCTTY`.
    fn posix_openpt(flags: c_int) -> c_int;
    /// Grants ownership and permissions for the slave PTY corresponding to `fd`.
    ///
    /// # Arguments
    ///
    /// * `fd` - Master PTY descriptor.
    fn grantpt(fd: c_int) -> c_int;
    /// Unlocks the slave PTY corresponding to `fd`.
    ///
    /// # Arguments
    ///
    /// * `fd` - Master PTY descriptor.
    fn unlockpt(fd: c_int) -> c_int;
    /// Returns a pointer to the slave PTY device path for `fd`.
    ///
    /// # Arguments
    ///
    /// * `fd` - Master PTY descriptor.
    fn ptsname(fd: c_int) -> *const c_char;
    /// Opens a filesystem path with POSIX `O_*` flags.
    ///
    /// # Arguments
    ///
    /// * `path` - NUL-terminated path string.
    /// * `oflag` - File status and access mode flags.
    fn open(path: *const c_char, oflag: c_int, ...) -> c_int;
    /// Closes an open POSIX file descriptor.
    ///
    /// # Arguments
    ///
    /// * `fd` - File descriptor to close.
    fn close(fd: c_int) -> c_int;
    /// Performs a terminal control request (`TIOCSWINSZ`).
    ///
    /// # Arguments
    ///
    /// * `fd` - Terminal file descriptor.
    /// * `request` - `TIOC*` request code.
    fn ioctl(fd: c_int, request: std::ffi::c_ulong, ...) -> c_int;
    /// Manipulates file descriptor flags (`F_GETFL`, `F_SETFL`).
    ///
    /// # Arguments
    ///
    /// * `fd` - Open file descriptor.
    /// * `cmd` - `F_*` command selector.
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    /// Reads terminal attributes from `fd` into `termios_p`.
    ///
    /// # Arguments
    ///
    /// * `fd` - Terminal file descriptor.
    /// * `termios_p` - Output `termios` buffer pointer.
    fn tcgetattr(fd: c_int, termios_p: *mut RawPtyTermios) -> c_int;
    /// Configures `termios_p` for raw non-canonical, non-echo byte pass-through.
    ///
    /// # Arguments
    ///
    /// * `termios_p` - Mutable `termios` buffer pointer.
    fn cfmakeraw(termios_p: *mut RawPtyTermios);
    /// Applies terminal attributes in `termios_p` to `fd`.
    ///
    /// # Arguments
    ///
    /// * `fd` - Terminal file descriptor.
    /// * `optional_actions` - `TCSANOW` (`0`).
    /// * `termios_p` - Input `termios` buffer pointer.
    fn tcsetattr(fd: c_int, optional_actions: c_int, termios_p: *const RawPtyTermios) -> c_int;
}

/// Allocates a raw-mode POSIX pseudo-terminal pair `(nonblocking_master_file, slave_fd)` sized to
/// `(rows, cols)` for headless `Capability::CursesPty` test execution.
///
/// # Arguments
///
/// * `rows` - Terminal row height (e.g. `25`).
/// * `cols` - Terminal column width (e.g. `80`).
/// * `test_id` - Test identifier attached to [`CoblangError::RunnerExecution`] on failure.
///
/// # Errors
///
/// Returns [`CoblangError::RunnerExecution`] if POSIX PTY allocation fails.
#[expect(
    unsafe_code,
    reason = "Audited POSIX PTY setup wrapping raw file descriptors into `OwnedFd` immediately."
)]
pub fn open_raw_pty_pair(
    rows: u16,
    cols: u16,
    test_id: &str,
) -> Result<(std::fs::File, std::os::fd::OwnedFd)> {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::Mutex;

    /// Serializes `ptsname()` calls across worker threads (`ptsname` uses a static buffer in libc).
    static PTY_OPEN_LOCK: Mutex<()> = Mutex::new(());
    let _pty_guard = PTY_OPEN_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // SAFETY: `posix_openpt` takes plain integer flags and returns a new fd or `-1`.
    let master_raw = unsafe { posix_openpt(PTY_OPEN_FLAGS) };
    if master_raw < 0 {
        return Err(CoblangError::RunnerExecution {
            test_id: String::from(test_id),
            message: String::from("posix_openpt failed"),
        });
    }
    // SAFETY: `master_raw >= 0` is a freshly opened descriptor owned exclusively here.
    if unsafe { grantpt(master_raw) } != 0 || unsafe { unlockpt(master_raw) } != 0 {
        // SAFETY: Closing `master_raw` on setup failure before wrapping in `OwnedFd`.
        let _ = unsafe { close(master_raw) };
        return Err(CoblangError::RunnerExecution {
            test_id: String::from(test_id),
            message: String::from("grantpt/unlockpt failed"),
        });
    }
    // SAFETY: `ptsname` returns a NUL-terminated slave device path or null on single-threaded setup.
    let slave_name_ptr = unsafe { ptsname(master_raw) };
    if slave_name_ptr.is_null() {
        // SAFETY: Closing `master_raw` on `ptsname` failure.
        let _ = unsafe { close(master_raw) };
        return Err(CoblangError::RunnerExecution {
            test_id: String::from(test_id),
            message: String::from("ptsname returned null"),
        });
    }
    // SAFETY: `slave_name_ptr` is non-null NUL-terminated path returned by `ptsname`.
    let slave_raw = unsafe { open(slave_name_ptr, PTY_OPEN_FLAGS) };
    if slave_raw < 0 {
        // SAFETY: Closing `master_raw` on slave `open` failure.
        let _ = unsafe { close(master_raw) };
        return Err(CoblangError::RunnerExecution {
            test_id: String::from(test_id),
            message: String::from("failed to open slave PTY"),
        });
    }

    let ws = RawPtyWinsize {
        row: rows.max(1),
        col: cols.max(1),
        xpixel: 0,
        ypixel: 0,
    };
    let mut term = RawPtyTermios([0_u8; 128]);
    // SAFETY: `slave_raw` is a valid TTY fd; `ws` and `term` are valid aligned stack structs.
    unsafe {
        let _ = ioctl(slave_raw, PTY_TIOCSWINSZ, &raw const ws);
        if tcgetattr(slave_raw, &raw mut term) == 0 {
            cfmakeraw(&raw mut term);
            let _ = tcsetattr(slave_raw, 0, &raw const term);
        }
        let flags = fcntl(master_raw, 3);
        if flags >= 0 {
            let _ = fcntl(master_raw, 4, flags | PTY_NONBLOCK_FLAG);
        }
    }

    // SAFETY: Both `master_raw` and `slave_raw` are valid, uniquely owned file descriptors.
    let master_fd = unsafe { OwnedFd::from_raw_fd(master_raw) };
    // SAFETY: Same as above for `slave_raw`.
    let slave_fd = unsafe { OwnedFd::from_raw_fd(slave_raw) };
    Ok((std::fs::File::from(master_fd), slave_fd))
}
