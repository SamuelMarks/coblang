//! Unified parallel test runner, diagnostic matcher, and expectation ratchet for `coblang`.

use std::process::ExitCode;

pub mod capabilities;
pub mod cli;
pub mod engine;
pub mod matcher;
pub mod process;
pub mod ratchet;

/// Validates that the test runner crate links cleanly with [`coblang_scaffold_driver`].
///
/// # Errors
///
/// Never returns an error; returns the deterministic scaffold exit status (`78`).
pub const fn runner_smoke_check() -> coblang_common::Result<u8> {
    Ok(coblang_scaffold_driver::scaffold_unimplemented_exit_code())
}

/// Process entry point invoked by the `coblang-test` binary.
pub fn cli_main_env() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sandbox_root = std::env::temp_dir().join("coblang-runner-sandboxes");
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let code = cli::run_cli(&args, &sandbox_root, &mut stdout, &mut stderr);
    ExitCode::from(code)
}
