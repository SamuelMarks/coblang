//! Comprehensive unit and binary integration tests for `coblang-scaffold-driver`.

use std::process::Command;

use coblang_common::{
    CompilationMode, Dialect, DriverFlavor, OptimizationLevel, Result, SourceFormat, TextEncoding,
};
use coblang_scaffold_driver::{
    DriverQuery, EXIT_CLI_USAGE, EXIT_INPUT_ERROR, EXIT_UNIMPLEMENTED, execute_cobcrun,
    execute_driver, flavor_label, parse_cobcrun_args, parse_driver_args,
    scaffold_unimplemented_exit_code,
};

/// Helper converting string slices into owned `Vec<String>`.
fn svec(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| String::from(*s)).collect()
}

/// Tests all compilation modes, dialects, formats, optimization levels, and flags across flavors.
#[test]
fn driver_flag_matrix_and_error_cases() -> Result<()> {
    assert_eq!(scaffold_unimplemented_exit_code(), EXIT_UNIMPLEMENTED);
    assert_eq!(flavor_label(DriverFlavor::Coblang), "coblang");
    assert_eq!(flavor_label(DriverFlavor::Cobc), "cobc");
    assert_eq!(flavor_label(DriverFlavor::Cobcrun), "cobcrun");
    assert_eq!(flavor_label(DriverFlavor::Gcobol), "gcobol");

    // Default modes differ between cobc (module) and coblang/gcobol (executable)
    let cobc_def = parse_driver_args(DriverFlavor::Cobc, &svec(&["prog.cob"]))?;
    assert_eq!(cobc_def.mode, CompilationMode::CompileModule);
    let gcob_def = parse_driver_args(DriverFlavor::Gcobol, &svec(&["prog.cob"]))?;
    assert_eq!(gcob_def.mode, CompilationMode::BuildExecutable);

    // Queries
    let ver = parse_driver_args(DriverFlavor::Coblang, &svec(&["--version"]))?;
    assert_eq!(ver.query, Some(DriverQuery::Version));
    let hlp = parse_driver_args(DriverFlavor::Coblang, &svec(&["-h"]))?;
    assert_eq!(hlp.query, Some(DriverQuery::Help));

    // Mode switches
    for (flag, mode) in [
        ("-E", CompilationMode::PreprocessOnly),
        ("-fsyntax-only", CompilationMode::SyntaxOnly),
        ("-S", CompilationMode::EmitAssembly),
        ("-emit-llvm", CompilationMode::EmitLlvmIr),
        ("-c", CompilationMode::CompileObject),
        ("-m", CompilationMode::CompileModule),
        ("-shared", CompilationMode::CompileModule),
        ("-b", CompilationMode::CompileModule),
        ("-x", CompilationMode::BuildExecutable),
    ] {
        let inv = parse_driver_args(DriverFlavor::Coblang, &svec(&[flag, "a.cob"]))?;
        assert_eq!(inv.mode, mode);
    }

    // Format, dialect, include/library, listing, warning, encoding, optimization options
    let rich = parse_driver_args(
        DriverFlavor::Gcobol,
        &svec(&[
            "-free",
            "-fixed",
            "-ffree-form",
            "-ffixed-form",
            "-F",
            "-finternal-ebcdic",
            "-debug",
            "-o",
            "out.bin",
            "-I",
            "inc1",
            "-Iinc2",
            "-L",
            "lib1",
            "-Llib2",
            "-l",
            "m",
            "-lc",
            "-copyext",
            ".cpy",
            "-copyext=.inc",
            "-ext",
            ".cob",
            "-dialect",
            "ibm",
            "-dialect=iso",
            "-std",
            "mf",
            "-std=cobol2014",
            "-t",
            "prog.lst",
            "-T",
            "wide.lst",
            "--tlines=0",
            "-conf=default.conf",
            "-O2",
            "-Wall",
            "-Wno-unused",
            "-fec=all",
            "-DTEST=1",
            "-g",
            "-w",
            "-v",
            "-q",
            "-save-temps",
            "-static",
            "-fexec-charset=CP1140",
            "-oobin",
            "prog.cob",
        ]),
    )?;
    assert_eq!(rich.format, SourceFormat::Free);
    assert_eq!(rich.dialect, Dialect::Cobol2014);
    assert_eq!(rich.optimization, OptimizationLevel::O2);
    assert_eq!(
        rich.encoding,
        TextEncoding::Codepage(String::from("CP1140"))
    );
    assert!(rich.exceptions_enabled);
    assert_eq!(rich.output_path.as_deref(), Some("obin"));
    assert_eq!(rich.include_dirs, svec(&["inc1", "inc2"]));
    assert_eq!(rich.library_dirs, svec(&["lib1", "lib2"]));
    assert_eq!(rich.libraries, svec(&["m", "c"]));
    assert_eq!(rich.copy_exts, svec(&[".cpy", ".inc", ".cob"]));
    assert_eq!(rich.inputs, svec(&["prog.cob"]));
    Ok(())
}

/// Verifies that malformed or incomplete CLI option sequences return `Err`.
#[test]
fn driver_invalid_cli_cases() {
    for bad in [
        &[][..],
        &["-o"][..],
        &["-I"][..],
        &["-L"][..],
        &["-l"][..],
        &["-copyext"][..],
        &["-dialect"][..],
        &["-dialect", "bogus", "a.cob"][..],
        &["-std=bogus", "a.cob"][..],
        &["-dialect=bogus", "a.cob"][..],
        &["-t"][..],
        &["-fexec-charset=", "a.cob"][..],
        &["-O99", "a.cob"][..],
        &["--unknown-flag", "a.cob"][..],
    ] {
        assert!(parse_driver_args(DriverFlavor::Coblang, &svec(bad)).is_err());
    }
}

/// Tests `execute_driver` and `execute_cobcrun` in-memory stream execution and exit codes.
#[test]
fn execute_driver_and_cobcrun_streams() -> Result<()> {
    let temp = std::env::temp_dir().join(format!("coblang-drv-test-{}", std::process::id()));
    std::fs::create_dir_all(&temp)?;
    let src_file = temp.join("hello.cob");
    std::fs::write(
        &src_file,
        "IDENTIFICATION DIVISION.
",
    )?;

    let mut out = Vec::new();
    let mut err = Vec::new();
    assert_eq!(
        execute_driver(
            DriverFlavor::Coblang,
            &svec(&["--version"]),
            &temp,
            &mut out,
            &mut err
        ),
        0
    );
    assert!(String::from_utf8_lossy(&out).contains("coblang"));

    out.clear();
    assert_eq!(
        execute_driver(
            DriverFlavor::Gcobol,
            &svec(&["--help"]),
            &temp,
            &mut out,
            &mut err
        ),
        0
    );
    assert!(String::from_utf8_lossy(&out).contains("Usage: gcobol"));

    err.clear();
    assert_eq!(
        execute_driver(DriverFlavor::Cobc, &svec(&[]), &temp, &mut out, &mut err),
        EXIT_CLI_USAGE
    );
    assert!(String::from_utf8_lossy(&err).contains("cobc: error:"));

    err.clear();
    assert_eq!(
        execute_driver(
            DriverFlavor::Coblang,
            &svec(&["missing.cob"]),
            &temp,
            &mut out,
            &mut err
        ),
        EXIT_INPUT_ERROR
    );
    assert!(String::from_utf8_lossy(&err).contains("input file not found"));

    err.clear();
    assert_eq!(
        execute_driver(
            DriverFlavor::Coblang,
            &svec(&["-x", "hello.cob"]),
            &temp,
            &mut out,
            &mut err
        ),
        EXIT_UNIMPLEMENTED
    );
    assert!(String::from_utf8_lossy(&err).contains("UNIMPLEMENTED: mode=build-exe"));

    // cobcrun parsing & execution
    let cr_ver = parse_cobcrun_args(&svec(&["-V"]), Some("/a::/b"))?;
    assert_eq!(cr_ver.query, Some(DriverQuery::Version));
    assert_eq!(cr_ver.module_dirs, svec(&["/a", "/b"]));

    let cr_help = parse_cobcrun_args(&svec(&["--help"]), None)?;
    assert_eq!(cr_help.query, Some(DriverQuery::Help));

    let cr_mod = parse_cobcrun_args(&svec(&["-M", "d1", "-Md2", "MYPROG", "arg1"]), None)?;
    assert_eq!(cr_mod.module_dirs, svec(&["d1", "d2"]));
    assert_eq!(cr_mod.module_name, "MYPROG");
    assert_eq!(cr_mod.program_args, svec(&["arg1"]));

    assert!(parse_cobcrun_args(&svec(&[]), None).is_err());
    assert!(parse_cobcrun_args(&svec(&["-M"]), None).is_err());
    assert!(parse_cobcrun_args(&svec(&["--bad"]), None).is_err());

    out.clear();
    assert_eq!(
        execute_cobcrun(&svec(&["--version"]), None, &mut out, &mut err),
        0
    );
    assert!(String::from_utf8_lossy(&out).contains("cobcrun"));

    out.clear();
    assert_eq!(
        execute_cobcrun(&svec(&["--help"]), None, &mut out, &mut err),
        0
    );
    assert!(String::from_utf8_lossy(&out).contains("Usage: cobcrun"));

    err.clear();
    assert_eq!(
        execute_cobcrun(&svec(&[]), None, &mut out, &mut err),
        EXIT_CLI_USAGE
    );
    assert_eq!(
        execute_cobcrun(&svec(&["MYMOD", "x"]), Some("/lib"), &mut out, &mut err),
        EXIT_UNIMPLEMENTED
    );

    std::fs::remove_dir_all(&temp)?;
    Ok(())
}

/// Spawns all four scaffold binary targets to verify process entry points and exit codes.
#[test]
fn spawn_all_four_scaffold_binaries() -> Result<()> {
    for bin in [
        env!("CARGO_BIN_EXE_coblang"),
        env!("CARGO_BIN_EXE_cobc-shim"),
        env!("CARGO_BIN_EXE_cobcrun-shim"),
        env!("CARGO_BIN_EXE_gcobol-shim"),
    ] {
        let out = Command::new(bin).arg("--version").output()?;
        assert!(out.status.success());
    }
    Ok(())
}
