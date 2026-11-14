//! Entry point for the GCC COBOL `gcobol` compatibility shim binary.

use std::process::ExitCode;

use coblang_common::DriverFlavor;

/// Runs the `gcobol` compatibility shim CLI driver.
fn main() -> ExitCode {
    coblang_scaffold_driver::main_entry(DriverFlavor::Gcobol)
}
