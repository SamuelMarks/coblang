//! Entry point for the `GnuCOBOL` `cobc` compatibility shim binary.

use std::process::ExitCode;

use coblang_common::DriverFlavor;

/// Runs the `cobc` compatibility shim CLI driver.
fn main() -> ExitCode {
    coblang_scaffold_driver::main_entry(DriverFlavor::Cobc)
}
