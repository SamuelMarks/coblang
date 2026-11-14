//! Entry point for the native `coblang` scaffold compiler binary.

use std::process::ExitCode;

use coblang_common::DriverFlavor;

/// Runs the `coblang` scaffold CLI driver.
fn main() -> ExitCode {
    coblang_scaffold_driver::main_entry(DriverFlavor::Coblang)
}
