//! Build script adding Homebrew library search paths (`zstd` / `zlib`) on macOS when linking LLVM/MLIR 20.

use std::path::Path;

/// Emits native link search directories for Homebrew system dependencies required by `llvm-sys` and `mlir-sys`.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    for candidate in ["/opt/homebrew/lib", "/usr/local/lib"] {
        if Path::new(candidate).is_dir() {
            println!("cargo:rustc-link-search=native={candidate}");
        }
    }
}
