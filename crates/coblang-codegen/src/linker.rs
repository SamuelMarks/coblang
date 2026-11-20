//! Pure-safe system linker invocation builder for `-x` executables and `-m` shared modules.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use coblang_common::{CoblangError, Result};

/// Output binary kind produced by the system linker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkArtifactKind {
    /// Standalone native executable (`-x`).
    Executable,
    /// Dynamically loadable shared module (`-m` / `-shared`).
    SharedModule,
}

/// Complete specification for invoking the host C/system linker (`cc`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkerInvocationSpec {
    /// Linker executable name or path (defaults to `"cc"`).
    pub linker_bin: String,
    /// Executable vs shared-module mode.
    pub kind: LinkArtifactKind,
    /// Input `.o`, `.a`, or companion `.so`/`.dylib` files.
    pub object_inputs: Vec<PathBuf>,
    /// Final linked output file path.
    pub output_path: PathBuf,
    /// Directory containing `libcoblang_rt.{dylib,so,a}`.
    pub runtime_lib_dir: PathBuf,
    /// Extra `-L` library search directories.
    pub extra_lib_dirs: Vec<PathBuf>,
    /// Extra `-l` library names.
    pub extra_libs: Vec<String>,
}

impl LinkerInvocationSpec {
    /// Builds the exact command-line argument vector passed to `self.linker_bin`.
    #[must_use]
    pub fn build_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.kind == LinkArtifactKind::SharedModule {
            args.push(String::from("-shared"));
            #[cfg(target_os = "macos")]
            args.push(String::from("-Wl,-undefined,dynamic_lookup"));
        }
        for obj in &self.object_inputs {
            args.push(obj.to_string_lossy().into_owned());
        }
        args.push(format!("-L{}", self.runtime_lib_dir.display()));
        args.push(format!("-Wl,-rpath,{}", self.runtime_lib_dir.display()));
        for dir in &self.extra_lib_dirs {
            args.push(format!("-L{}", dir.display()));
            args.push(format!("-Wl,-rpath,{}", dir.display()));
        }
        args.push(String::from("-lcoblang_rt"));
        for lib in &self.extra_libs {
            args.push(format!("-l{lib}"));
        }
        args.push(String::from("-o"));
        args.push(self.output_path.to_string_lossy().into_owned());
        args
    }
}

/// Checks whether `dir` contains a built `libcoblang_rt` dynamic or static library.
///
/// # Arguments
///
/// * `dir` - Candidate directory path.
#[must_use]
pub fn has_runtime_lib(dir: &Path) -> bool {
    ["libcoblang_rt.dylib", "libcoblang_rt.so", "libcoblang_rt.a"]
        .iter()
        .any(|name| dir.join(name).is_file())
}

/// Resolves the directory containing `libcoblang_rt.{dylib,so,a}`.
///
/// Searches `override_dir` (e.g. `COBLANG_RT_LIB_DIR`), then `exe_path`'s parent and grandparent
/// (`target/<profile>` vs `target/<profile>/deps`), then `fallback_dirs`.
///
/// # Arguments
///
/// * `override_dir` - Optional explicit directory from `COBLANG_RT_LIB_DIR`.
/// * `exe_path` - Optional current executable path (`std::env::current_exe().ok()`).
/// * `fallback_dirs` - Additional candidate directories (e.g. workspace `target/debug`).
///
/// # Errors
///
/// Returns [`CoblangError::LinkerInvocation`] if no candidate directory contains `libcoblang_rt`.
pub fn locate_runtime_lib_dir(
    override_dir: Option<&Path>,
    exe_path: Option<&Path>,
    fallback_dirs: &[PathBuf],
) -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(ov) = override_dir
        && has_runtime_lib(ov)
    {
        return Ok(ov.to_path_buf());
    }
    if let Some(ov) = override_dir {
        candidates.push(ov.to_path_buf());
    }
    if let Some(exe) = exe_path {
        for anc in exe.ancestors().skip(1).take(5) {
            candidates.push(anc.to_path_buf());
            candidates.push(anc.join("deps"));
            let rt_pkg_dir = anc.join("build").join("coblang-rt-abi");
            if let Ok(entries) = std::fs::read_dir(rt_pkg_dir) {
                for entry in entries.flatten() {
                    candidates.push(entry.path().join("out"));
                }
            }
        }
    }
    candidates.extend_from_slice(fallback_dirs);

    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for dir in &candidates {
        for name in ["libcoblang_rt.dylib", "libcoblang_rt.so", "libcoblang_rt.a"] {
            let p = dir.join(name);
            if let Ok(meta) = std::fs::metadata(&p)
                && meta.is_file()
            {
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().is_none_or(|(prev_t, _)| mtime > *prev_t) {
                    best = Some((mtime, dir.clone()));
                }
            }
        }
    }
    if let Some((_, chosen)) = best {
        return Ok(chosen);
    }

    let searched: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
    Err(CoblangError::LinkerInvocation {
        linker_bin: String::from("coblang-rt-locator"),
        exit_code: None,
        stderr: format!(
            "could not locate `libcoblang_rt` in: {}",
            searched.join(", ")
        ),
    })
}

/// Computes the concrete output file path for `-o` (handling directory `-o .` targets and default extensions).
///
/// # Arguments
///
/// * `cwd` - Working directory.
/// * `primary_input` - First input source path (used to derive stem).
/// * `explicit_output` - Optional `-o <path>` argument.
/// * `default_ext` - Extension without leading dot (`""` for `-x`, `"so"` for `-m`, `"o"` for `-c`, `"s"` for `-S`, `"ll"` for `-emit-llvm`).
#[must_use]
pub fn resolve_output_path(
    cwd: &Path,
    primary_input: &Path,
    explicit_output: Option<&str>,
    default_ext: &str,
) -> PathBuf {
    let stem = primary_input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("a.out");
    let default_filename = if default_ext.is_empty() {
        String::from(stem)
    } else {
        format!("{stem}.{default_ext}")
    };

    if let Some(out_str) = explicit_output {
        let candidate = Path::new(out_str);
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            cwd.join(candidate)
        };
        if resolved.is_dir() {
            return resolved.join(default_filename);
        }
        return resolved;
    }

    cwd.join(default_filename)
}

/// Executes `spec` using `std::process::Command` and returns `Ok(())` when exit status is `0`.
///
/// # Arguments
///
/// * `spec` - Complete [`LinkerInvocationSpec`].
///
/// # Errors
///
/// Returns [`CoblangError::LinkerInvocation`] if spawning `spec.linker_bin` fails or exits non-zero.
pub fn invoke_system_linker(spec: &LinkerInvocationSpec) -> Result<()> {
    let args = spec.build_args();
    let output = Command::new(&spec.linker_bin)
        .args(&args)
        .output()
        .map_err(|err| CoblangError::LinkerInvocation {
            linker_bin: spec.linker_bin.clone(),
            exit_code: None,
            stderr: err.to_string(),
        })?;

    if !output.status.success() {
        return Err(CoblangError::LinkerInvocation {
            linker_bin: spec.linker_bin.clone(),
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}
