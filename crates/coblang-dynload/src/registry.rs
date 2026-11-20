//! Audited in-binary and shared-library dynamic `CALL` / `CANCEL` module registry.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use coblang_common::{CoblangError, Result};

use crate::{
    LoadedModuleHandle, ResolvedEntrySymbol, resolve_and_load_module, resolve_in_current_process,
};

/// Outcome of a COBOL `CANCEL` operation against [`DynamicModuleRegistry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CancelOutcome {
    /// Target matched an in-binary or in-process COBOL module and executed its reset hook if present.
    ResetInBinary,
    /// Target matched a dynamically loaded shared object, invoked any reset hook, and unloaded the DSO.
    UnloadedDynamic,
    /// Target was not active (ISO 2023 `CANCEL` no-op).
    NotActive,
}

/// Run-unit dynamic module table resolving in-binary registered modules first, followed by
/// cached shared objects, current-process symbols, and `-M` / `COB_LIBRARY_PATH` / `.` DSOs.
#[derive(Debug)]
pub struct DynamicModuleRegistry {
    /// Statically or explicitly registered in-binary COBOL/runtime modules keyed by uppercase name.
    in_binary: BTreeMap<String, ResolvedEntrySymbol>,
    /// Explicit `-M <dir>` module search directories prepended ahead of `COB_LIBRARY_PATH`.
    extra_search_dirs: Vec<PathBuf>,
    /// Active dynamically loaded modules retained until `CANCEL` or `cob_tidy`.
    dso_cache: BTreeMap<String, (LoadedModuleHandle, ResolvedEntrySymbol)>,
}

impl Default for DynamicModuleRegistry {
    /// Creates an empty [`DynamicModuleRegistry`].
    fn default() -> Self {
        Self::new()
    }
}

impl DynamicModuleRegistry {
    /// Creates an empty [`DynamicModuleRegistry`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            in_binary: BTreeMap::new(),
            extra_search_dirs: Vec::new(),
            dso_cache: BTreeMap::new(),
        }
    }

    /// Registers an in-binary module or `ENTRY` point that takes precedence over external DSOs.
    ///
    /// # Arguments
    ///
    /// * `module_name` - Logical COBOL `PROGRAM-ID` or `ENTRY` name.
    /// * `entry` - Resolved in-binary entrypoint descriptor.
    pub fn register_in_binary(&mut self, module_name: &str, entry: ResolvedEntrySymbol) {
        let key = module_name.trim().to_ascii_uppercase();
        if !key.is_empty() {
            self.in_binary.insert(key, entry);
        }
    }

    /// Replaces the explicit `-M` module search directory list.
    ///
    /// # Arguments
    ///
    /// * `dirs` - Ordered `-M` directory paths searched ahead of `COB_LIBRARY_PATH`.
    pub fn set_extra_search_dirs(&mut self, dirs: Vec<PathBuf>) {
        self.extra_search_dirs = dirs;
    }

    /// Returns the configured `-M` extra module search directories.
    #[must_use]
    pub fn extra_search_dirs(&self) -> &[PathBuf] {
        &self.extra_search_dirs
    }

    /// Builds the ordered DSO search list combining `-M` directories, `COB_LIBRARY_PATH`, and `.`.
    ///
    /// # Arguments
    ///
    /// * `env_lib_path` - Optional `:`-separated `COB_LIBRARY_PATH` string.
    #[must_use]
    pub fn build_search_dirs(&self, env_lib_path: Option<&str>) -> Vec<PathBuf> {
        let mut dirs = self.extra_search_dirs.clone();
        if let Some(raw) = env_lib_path {
            for seg in raw.split(':').filter(|s| !s.is_empty()) {
                dirs.push(PathBuf::from(seg));
            }
        }
        dirs.push(PathBuf::from("."));
        dirs
    }

    /// Resolves `target` across in-binary modules first, then active DSO cache, then current-process
    /// and `-M` / `COB_LIBRARY_PATH` / `.` shared libraries (`.dylib`, `.so`).
    ///
    /// # Arguments
    ///
    /// * `target` - Unquoted COBOL `CALL` target name.
    /// * `env_lib_path` - Optional `COB_LIBRARY_PATH` value.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::DynamicModuleLoad`] (`EC-PROGRAM-NOT-FOUND`) when `target` is empty
    /// or cannot be resolved from any registered or filesystem module.
    pub fn resolve_module(
        &mut self,
        target: &str,
        env_lib_path: Option<&str>,
    ) -> Result<ResolvedEntrySymbol> {
        let trimmed = target.trim();
        if trimmed.is_empty() {
            return Err(CoblangError::DynamicModuleLoad {
                module_name: String::new(),
                searched_paths: String::new(),
                message: String::from("empty dynamic CALL module name"),
            });
        }
        let key = trimmed.to_ascii_uppercase();
        if let Some(&entry) = self.in_binary.get(&key) {
            return Ok(entry);
        }
        if let Some(&(_, entry)) = self.dso_cache.get(&key) {
            return Ok(entry);
        }
        let dirs = self.build_search_dirs(env_lib_path);
        let (handle, entry) = resolve_and_load_module(trimmed, &dirs)?;
        self.dso_cache.insert(key, (handle, entry));
        Ok(entry)
    }

    /// Resets state and/or unloads `target` for COBOL `CANCEL`.
    ///
    /// # Arguments
    ///
    /// * `target` - Unquoted COBOL `CANCEL` target name.
    #[must_use]
    pub fn cancel_module(&mut self, target: &str) -> CancelOutcome {
        let trimmed = target.trim();
        if trimmed.is_empty() {
            return CancelOutcome::NotActive;
        }
        let key = trimmed.to_ascii_uppercase();
        if let Some(&entry) = self.in_binary.get(&key) {
            let _ = entry.reset_state();
            return CancelOutcome::ResetInBinary;
        }
        if let Some((_handle, entry)) = self.dso_cache.remove(&key) {
            let _ = entry.reset_state();
            return CancelOutcome::UnloadedDynamic;
        }
        if let Some((_handle, entry)) = resolve_in_current_process(trimmed)
            && entry.reset_state()
        {
            return CancelOutcome::ResetInBinary;
        }
        CancelOutcome::NotActive
    }

    /// Releases all cached dynamic shared-object handles while keeping in-binary registrations intact,
    /// or clears all state during run-unit teardown (`cob_tidy`).
    pub fn clear_all(&mut self) {
        self.dso_cache.clear();
        self.extra_search_dirs.clear();
        self.in_binary.clear();
    }

    /// Releases only dynamically loaded DSO handles while preserving static in-binary registrations.
    pub fn clear_loaded_dsos(&mut self) {
        self.dso_cache.clear();
    }
}

/// Returns a reference to the process-wide [`DynamicModuleRegistry`] mutex.
#[must_use]
pub fn global_module_registry() -> &'static Mutex<DynamicModuleRegistry> {
    /// Process-wide lazy module registry singleton.
    static REGISTRY: OnceLock<Mutex<DynamicModuleRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(DynamicModuleRegistry::new()))
}

/// Acquires [`global_module_registry`], recovering cleanly if poisoned.
fn lock_module_registry() -> MutexGuard<'static, DynamicModuleRegistry> {
    match global_module_registry().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Executes `op` against [`global_module_registry`], recovering cleanly if poisoned.
///
/// # Arguments
///
/// * `op` - Closure receiving `&mut DynamicModuleRegistry`.
pub fn with_module_registry<R, F>(op: F) -> R
where
    F: FnOnce(&mut DynamicModuleRegistry) -> R,
{
    let mut guard = lock_module_registry();
    op(&mut guard)
}

/// Registers an in-binary module in [`global_module_registry`].
///
/// # Arguments
///
/// * `module_name` - Logical COBOL `PROGRAM-ID` or `ENTRY` name.
/// * `entry` - Resolved in-binary entrypoint descriptor.
pub fn register_in_binary_module(module_name: &str, entry: ResolvedEntrySymbol) {
    with_module_registry(|reg| reg.register_in_binary(module_name, entry));
}

/// Updates explicit `-M` search directories in [`global_module_registry`].
///
/// # Arguments
///
/// * `dirs` - Ordered `-M` search directories.
pub fn set_extra_module_search_dirs(dirs: Vec<PathBuf>) {
    with_module_registry(|reg| reg.set_extra_search_dirs(dirs));
}

/// Resolves a dynamic `CALL` target through [`global_module_registry`].
///
/// # Arguments
///
/// * `target` - Unquoted COBOL `CALL` target name.
/// * `env_lib_path` - Optional `COB_LIBRARY_PATH` environment string.
///
/// # Errors
///
/// Returns [`CoblangError::DynamicModuleLoad`] when `target` cannot be resolved.
pub fn resolve_dynamic_module(
    target: &str,
    env_lib_path: Option<&str>,
) -> Result<ResolvedEntrySymbol> {
    with_module_registry(|reg| reg.resolve_module(target, env_lib_path))
}

/// Performs COBOL `CANCEL` state reset and/or DSO unload on `target` in [`global_module_registry`].
///
/// # Arguments
///
/// * `target` - Unquoted COBOL `CANCEL` target name.
#[must_use]
pub fn cancel_dynamic_module(target: &str) -> CancelOutcome {
    with_module_registry(|reg| reg.cancel_module(target))
}

/// Clears cached dynamic shared objects from [`global_module_registry`].
pub fn clear_dynamic_module_registry() {
    with_module_registry(DynamicModuleRegistry::clear_loaded_dsos);
}
