//! Entry point for the `GnuCOBOL` `cobcrun` compatibility shim binary.

use std::process::ExitCode;

use coblang_common::DriverFlavor;

/// Runs the `cobcrun` compatibility shim CLI driver.
fn main() -> ExitCode {
    coblang_scaffold_driver::main_entry(DriverFlavor::Cobcrun)
}
