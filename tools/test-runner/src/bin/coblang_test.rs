//! Entry point for the `coblang-test` unified test runner CLI binary.

use std::process::ExitCode;

/// Launches `coblang-test` with command-line arguments from the environment.
fn main() -> ExitCode {
    coblang_test_runner::cli_main_env()
}
