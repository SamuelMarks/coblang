//! Build script ensuring test binaries on ELF targets export dynamic symbols for `dlopen(NULL)`.

/// Emits `-rdynamic` for test targets on non-Apple Unix platforms so in-process symbol lookup tests succeed identically to macOS.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        println!("cargo:rustc-link-arg-tests=-rdynamic");
    }
}
