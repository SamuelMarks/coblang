//! Comprehensive unit and binary integration tests for `coblang-scaffold-driver`.

use std::process::Command;

use coblang_common::{
    CompilationMode, Dialect, DriverFlavor, OptimizationLevel, Result, SourceFormat, TextEncoding,
};
use coblang_scaffold_driver::{
    DriverQuery, EXIT_CLI_USAGE, EXIT_INPUT_ERROR, EXIT_UNIMPLEMENTED, execute_cobcrun,
    execute_compiler_driver, execute_driver, flavor_label, parse_cobcrun_args, parse_driver_args,
    scaffold_unimplemented_exit_code,
};

/// Force Cargo to build `libcoblang_rt` for native `-x` and `-m` driver tests.
use coblang_rt as _;

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

/// Runs `execute_compiler_driver` and returns `(exit_code, stdout_string)`.
fn run_cdrv(args: &[&str], env_vars: &[(String, String)], cwd: &std::path::Path) -> (u8, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = execute_compiler_driver(
        DriverFlavor::Coblang,
        &svec(args),
        env_vars,
        cwd,
        &mut out,
        &mut err,
    );
    (code, String::from_utf8_lossy(&out).into_owned())
}

/// Exercises `execute_compiler_driver` queries, invalid flags, missing files, and unimplemented modes.
#[test]
fn execute_compiler_driver_preprocess_and_listing_coverage() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-compdrv-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    std::fs::write(
        tmp.join("ok.cob"),
        "000100 IDENTIFICATION DIVISION.\u{000a}",
    )?;
    std::fs::write(
        tmp.join("bit_unimpl.cob"),
        "       IDENTIFICATION DIVISION.\u{000a}       PROGRAM-ID. B1.\u{000a}       DATA DIVISION.\u{000a}       WORKING-STORAGE SECTION.\u{000a}       01 BT PIC 1 USAGE BIT.\u{000a}       PROCEDURE DIVISION.\u{000a}           STOP RUN.\u{000a}",
    )?;

    assert_eq!(run_cdrv(&["--version"], &[], &tmp).0, 0);
    assert_eq!(run_cdrv(&["--help"], &[], &tmp).0, 0);
    assert_eq!(run_cdrv(&[], &[], &tmp).0, EXIT_CLI_USAGE);
    assert_eq!(run_cdrv(&["nope.cob"], &[], &tmp).0, EXIT_INPUT_ERROR);
    assert_eq!(
        run_cdrv(&["-Wbogus", "ok.cob"], &[], &tmp).0,
        EXIT_INPUT_ERROR
    );
    assert_eq!(
        run_cdrv(&["-x", "bit_unimpl.cob"], &[], &tmp).0,
        EXIT_UNIMPLEMENTED
    );
    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Exercises `execute_compiler_driver` `-E` outputs (`stdout`, `-o .`, `-o file`), listings, and error paths.
#[test]
fn execute_compiler_driver_outputs_and_errors() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-compdrv2-{}", std::process::id()));
    std::fs::create_dir_all(tmp.join("libsub"))?;
    std::fs::write(tmp.join("sub.cpy"), "       01 SUB-V PIC X.\u{000a}")?;
    std::fs::write(tmp.join("libsub/l.cpy"), "       01 L-V PIC X.\u{000a}")?;
    std::fs::write(tmp.join("extra.conf"), "tab-width: 4\u{000a}")?;
    std::fs::write(
        tmp.join("my.conf"),
        "include \u{0022}extra.conf\u{0022}\u{000a}",
    )?;
    std::fs::write(
        tmp.join("ok.cob"),
        "000100 IDENTIFICATION DIVISION.\u{000a}000200D      DISPLAY \u{0022}DBG\u{0022}.\u{000a}000300       COPY sub.\u{000a}000400       COPY l IN MYLIB.\u{000a}",
    )?;
    std::fs::write(
        tmp.join("bad.cob"),
        "000100       COPY missing_book.\u{000a}",
    )?;

    let env_vars = [
        (
            String::from("COB_COPY_DIR"),
            tmp.to_string_lossy().to_string(),
        ),
        (String::from("COB_COPY_LIB_MYLIB"), String::from("libsub")),
    ];

    let (c1, out1) = run_cdrv(
        &[
            "-E",
            "-conf=my.conf",
            "-cb_conf=pic-length=38",
            "-DFLAG",
            "-DN=1",
            "-DS=TXT",
            "-debug",
            "-Wall",
            "-t",
            "narrow.lst",
            "--tlines=10",
            "-ftsymbols",
            "-fno-theader",
            "-fno-tmessages",
            "ok.cob",
        ],
        &env_vars,
        &tmp,
    );
    assert_eq!(c1, 0);
    assert!(out1.contains("SUB-V"));
    assert!(tmp.join("narrow.lst").is_file());

    assert_eq!(
        run_cdrv(
            &["-E", "-T", "wide.lst", "-o", ".", "ok.cob"],
            &env_vars,
            &tmp
        )
        .0,
        0
    );
    assert!(tmp.join("ok.i").is_file());
    assert!(tmp.join("wide.lst").is_file());

    assert_eq!(
        run_cdrv(&["-E", "-o", "explicit.i", "ok.cob"], &env_vars, &tmp).0,
        0
    );
    assert!(tmp.join("explicit.i").is_file());
    assert_eq!(run_cdrv(&["-E", "bad.cob"], &[], &tmp).0, EXIT_INPUT_ERROR);
    assert_eq!(
        run_cdrv(
            &["-E", "-o", "no_such_dir/out.i", "ok.cob"],
            &env_vars,
            &tmp
        )
        .0,
        EXIT_INPUT_ERROR
    );
    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Exercises remaining `-conf=` / `-cb_conf=` / `-w` / `-d` / `-I` / directory-read edge cases.
#[test]
fn execute_compiler_driver_conf_and_read_error_edges() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-compdrv3-{}", std::process::id()));
    std::fs::create_dir_all(tmp.join("libsub"))?;
    std::fs::write(
        tmp.join("ok.cob"),
        "000100 IDENTIFICATION DIVISION.\u{000a}",
    )?;

    // Exercise -w, -fdebugging-line, -d, -I, non-COB env var, --tlines=bad, -cb_conf errors, missing conf, and dir input.
    let env_misc = [(String::from("UNRELATED_ENV"), String::from("1"))];
    assert_eq!(
        run_cdrv(
            &[
                "-E",
                "-w",
                "-fdebugging-line",
                "-d",
                "-I",
                ".",
                "--tlines=bad",
                "-cb_conf=noeq",
                "-o",
                "-",
                "ok.cob"
            ],
            &env_misc,
            &tmp
        )
        .0,
        0
    );
    assert_eq!(
        run_cdrv(&["-E", "-conf=missing.conf", "ok.cob"], &[], &tmp).0,
        EXIT_INPUT_ERROR
    );
    assert_eq!(
        run_cdrv(&["-E", "-conf=cobol2014.conf", "ok.cob"], &[], &tmp).0,
        0
    );
    std::fs::write(
        tmp.join("incbuiltin.conf"),
        "include \u{0022}default.conf\u{0022}\u{000a}",
    )?;
    assert_eq!(
        run_cdrv(&["-E", "-conf=incbuiltin.conf", "ok.cob"], &[], &tmp).0,
        0
    );
    std::fs::write(
        tmp.join("badinc.conf"),
        "include \u{0022}nope.conf\u{0022}\u{000a}",
    )?;
    assert_eq!(
        run_cdrv(&["-E", "-conf=badinc.conf", "ok.cob"], &[], &tmp).0,
        EXIT_INPUT_ERROR
    );
    assert_eq!(
        run_cdrv(&["-E", "-cb_conf=tab-width=0", "ok.cob"], &[], &tmp).0,
        EXIT_INPUT_ERROR
    );
    assert_eq!(run_cdrv(&["-E", "libsub"], &[], &tmp).0, EXIT_INPUT_ERROR);

    // Exercise `-fsyntax-only` success, listing emission, syntax errors, and `read_bounded_source_bytes` limit.
    std::fs::write(
        tmp.join("valid_syn.cob"),
        "000100 IDENTIFICATION DIVISION.\u{000a}000200 PROGRAM-ID. SYN1.\u{000a}",
    )?;
    std::fs::write(
        tmp.join("bad_syn.cob"),
        "000100 IDENTIFICATION DIVISION.\u{000a}000200 PROGRAM-ID. SYN2.\u{000a}000300 PROCEDURE DIVISION.\u{000a}000400     BOGUSVERB.\u{000a}",
    )?;
    assert_eq!(
        run_cdrv(
            &["-fsyntax-only", "-t", "syn.lst", "valid_syn.cob"],
            &[],
            &tmp
        )
        .0,
        0
    );
    assert!(tmp.join("syn.lst").is_file());
    assert_eq!(
        run_cdrv(&["-fsyntax-only", "bad_syn.cob"], &[], &tmp).0,
        EXIT_INPUT_ERROR
    );
    assert!(
        coblang_scaffold_driver::pipeline::read_bounded_source_bytes(&tmp.join("valid_syn.cob"), 4)
            .is_err()
    );
    std::fs::write(tmp.join("_driver_stub.cob"), "")?;
    assert_eq!(
        run_cdrv(&["-fsyntax-only", "_driver_stub.cob"], &[], &tmp).0,
        EXIT_UNIMPLEMENTED
    );

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Exercises `--emit-mlir`, `--emit-mlir-llvm`, `-emit-llvm`, `-S`, `-c`, `-m`, `-x`, `-save-temps`, and codegen error paths.
#[test]
fn execute_compiler_driver_mlir_and_native_codegen_modes() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-cgdrv-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    std::fs::write(
        tmp.join("hello.cob"),
        "       IDENTIFICATION DIVISION.\u{000a}       PROGRAM-ID. HELLO.\u{000a}       PROCEDURE DIVISION.\u{000a}           DISPLAY \u{0022}DRV-OK\u{0022}.\u{000a}           STOP RUN.\u{000a}",
    )?;
    std::fs::write(
        tmp.join("bad_cg.cob"),
        "       IDENTIFICATION DIVISION.\u{000a}       PROGRAM-ID. BADCG.\u{000a}       PROCEDURE DIVISION.\u{000a}           BOGUS_STMT.\u{000a}",
    )?;

    // `--emit-mlir` & `--emit-mlir-llvm` to stdout and `-o` file.
    let (c_mlir, out_mlir) = run_cdrv(&["--emit-mlir", "hello.cob"], &[], &tmp);
    assert_eq!(c_mlir, 0);
    assert!(out_mlir.contains("cobol.program"));

    let (c_mllvm, out_mllvm) = run_cdrv(&["--emit-mlir-llvm", "-o", "-", "hello.cob"], &[], &tmp);
    assert_eq!(c_mllvm, 0);
    assert!(out_mllvm.contains("llvm.func"));

    assert_eq!(
        run_cdrv(&["-emit-llvm", "-o", "hello.ll", "hello.cob"], &[], &tmp).0,
        0
    );
    assert!(tmp.join("hello.ll").is_file());
    assert_eq!(run_cdrv(&["-emit-llvm", "hello.cob"], &[], &tmp).0, 0);

    // `-S`, `-c`, `-m`, `-x` (+ `-t` listing, `-save-temps`, multi-unit without `-save-temps`, prebuilt `.o`, EBCDIC unimplemented, syntax error).
    std::fs::write(
        tmp.join("sub2.cob"),
        "       IDENTIFICATION DIVISION.\u{000a}       PROGRAM-ID. SUB2.\u{000a}       PROCEDURE DIVISION.\u{000a}           EXIT PROGRAM.\u{000a}",
    )?;
    assert_eq!(run_cdrv(&["-S", "hello.cob"], &[], &tmp).0, 0);
    assert!(tmp.join("hello.s").is_file());

    assert_eq!(
        run_cdrv(&["-c", "-t", "cg.lst", "hello.cob"], &[], &tmp).0,
        0
    );
    assert!(tmp.join("hello.o").is_file());
    assert!(tmp.join("cg.lst").is_file());

    assert_eq!(run_cdrv(&["-m", "hello.cob", "-o", "."], &[], &tmp).0, 0);
    assert!(tmp.join("hello.so").is_file());

    assert_eq!(
        run_cdrv(
            &["-x", "-save-temps", "hello.cob", "-o", "hello.bin"],
            &[],
            &tmp
        )
        .0,
        0
    );
    let out = Command::new(tmp.join("hello.bin"))
        .current_dir(&tmp)
        .output()?;
    assert!(out.status.success());
    assert_eq!(out.stdout, [b'D', b'R', b'V', b'-', b'O', b'K', 0x0A]);

    let exe_path = std::env::current_exe()?;
    let rt_dir = coblang_codegen::locate_runtime_lib_dir(None, Some(&exe_path), &[])?;
    let rt_env = [(
        String::from("COBLANG_RT_LIB_DIR"),
        rt_dir.to_string_lossy().into_owned(),
    )];
    assert_eq!(
        run_cdrv(
            &["-x", "-L", ".", "hello.cob", "sub2.cob", "-o", "multi.bin"],
            &rt_env,
            &tmp
        )
        .0,
        0
    );
    assert!(tmp.join("multi.bin").is_file());

    // Link prebuilt `.o` directly with `-main`.
    assert_eq!(
        run_cdrv(&["-c", "-main", "hello.cob", "-o", "pre.o"], &[], &tmp).0,
        0
    );
    assert_eq!(
        run_cdrv(&["-x", "pre.o", "-o", "from_pre.bin"], &[], &tmp).0,
        0
    );
    assert!(tmp.join("from_pre.bin").is_file());

    assert_eq!(
        run_cdrv(&["-x", "bad_cg.cob"], &[], &tmp).0,
        EXIT_INPUT_ERROR
    );
    assert_eq!(
        run_cdrv(
            &["-x", "-finternal-ebcdic", "hello.cob", "-o", "ebcdic.bin"],
            &[],
            &tmp
        )
        .0,
        0
    );
    assert_eq!(
        run_cdrv(&["-x", "-fmemory-check", "hello.cob"], &[], &tmp).0,
        EXIT_UNIMPLEMENTED
    );
    assert_eq!(
        run_cdrv(
            &[
                "-fsyntax-only",
                "-fdump=ALL",
                "-fno-dump",
                "-fmax-errors=50",
                "-fscreen-section-rules=std",
                "-faccept-display-extensions=warning",
                "-fassign-ext-dyn=ok",
                "-fregister=LIN,COL",
                "hello.cob",
            ],
            &[],
            &tmp,
        )
        .0,
        0
    );
    verify_c_companion_and_dialect_cli_flags(&tmp, &rt_env)?;

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Verifies truncation/sign/pretty-display CLI flags and companion `.c` source compilation.
///
/// # Arguments
///
/// * `tmp` - Scratch directory containing `hello.cob`.
/// * `rt_env` - Runtime library environment pairs.
fn verify_c_companion_and_dialect_cli_flags(
    tmp: &std::path::Path,
    rt_env: &[(String, String)],
) -> Result<()> {
    assert_eq!(
        run_cdrv(
            &[
                "-fsyntax-only",
                "-fnotrunc",
                "-fno-binary-truncate",
                "-fbinary-truncate",
                "-fsign=ebcdic",
                "-fbinary-byteorder=native",
                "-fbinary-size=1--8",
                "-fpretty-display",
                "-fno-pretty-display",
                "-debug",
                "-fec=bound-subscript",
                "-fno-ec=bound",
                "-fodoslide",
                "-fno-odoslide",
                "-fdefault-colseq=ebcdic",
                "-fdefault-colseq=ascii",
                "-fdefaultbyte=0x00",
                "-frelax-syntax-checks",
                "hello.cob",
            ],
            &[],
            tmp,
        )
        .0,
        0
    );
    assert_eq!(
        run_cdrv(
            &["-fsyntax-only", "-fdefault-colseq=bad", "hello.cob"],
            &[],
            tmp
        )
        .0,
        EXIT_INPUT_ERROR
    );
    std::fs::write(
        tmp.join("helper.c"),
        "int cob_c_helper(void) { return 0; }\n",
    )?;
    std::fs::write(tmp.join("broken.c"), "int broken( { syntax error }\n")?;
    assert_eq!(
        run_cdrv(&["-c", "-I", ".", "helper.c", "-o", "helper.o"], &[], tmp).0,
        0
    );
    assert!(tmp.join("helper.o").is_file());
    assert_eq!(
        run_cdrv(
            &["-x", "-I", ".", "helper.c", "hello.cob", "-o", "c_cob.bin"],
            rt_env,
            tmp,
        )
        .0,
        0
    );
    assert!(tmp.join("c_cob.bin").is_file());
    assert_eq!(run_cdrv(&["-c", "broken.c"], &[], tmp).0, EXIT_INPUT_ERROR);
    let mut g_out = Vec::new();
    let mut g_err = Vec::new();
    assert_eq!(
        execute_compiler_driver(
            DriverFlavor::Gcobol,
            &svec(&["-fsyntax-only", "hello.cob"]),
            &[],
            tmp,
            &mut g_out,
            &mut g_err,
        ),
        0
    );
    Ok(())
}

/// Runs `execute_cobcrun_driver` and returns `(exit_code, stdout, stderr)`.
///
/// # Arguments
///
/// * `args` - CLI arguments.
/// * `env_vars` - Environment pairs.
/// * `cwd` - Working directory.
fn run_crun(
    args: &[&str],
    env_vars: &[(String, String)],
    cwd: &std::path::Path,
) -> (u8, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = coblang_scaffold_driver::pipeline::execute_cobcrun_driver(
        &svec(args),
        env_vars,
        cwd,
        &mut out,
        &mut err,
    );
    (
        code,
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

/// Exercises all `cobcrun` CLI option parsing, short-circuit banners, and `.cfg`/`.conf` rules.
#[test]
fn cobcrun_driver_cli_and_config_parsing_coverage() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-crun-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let sub = tmp.join("sub");
    let cfg_dir = tmp.join("cfgd");
    std::fs::create_dir_all(&sub)?;
    std::fs::create_dir_all(&cfg_dir)?;

    let cfg_text = concat!(
        "\n",
        "# comment line\n",
        "=\n",
        "pre_load = \"\"\n",
        "pre_load = modA::modB\n",
        "COB_PRE_LOAD = 'modC'\n",
        "library_path = sub::.\n",
        "COB_LIBRARY_PATH = sub\n",
        "other_key = ignored\n",
    );
    std::fs::write(tmp.join("rt.cfg"), cfg_text)?;
    std::fs::write(tmp.join("rt.CONF"), cfg_text)?;
    std::fs::write(cfg_dir.join("in_dir.cfg"), "pre_load = modD\n")?;

    for ok_flag in [
        &["--version"][..],
        &["-V"][..],
        &["--help"][..],
        &["-h"][..],
        &["--info"][..],
        &["-i"][..],
        &["--runtime-conf"][..],
        &["-r"][..],
        &["rt.cfg", "ANY", "--runtime-conf"][..],
        &["rt.CONF", "ANY", "-r"][..],
    ] {
        assert_eq!(run_crun(ok_flag, &[], &tmp).0, 0);
    }

    for err_flag in [
        &[][..],
        &["-M"][..],
        &["-M", ""][..],
        &["-M"][..],
        &["--module="][..],
        &["-c"][..],
        &["--bogus"][..],
        &[
            "-M",
            "sub",
            "-c",
            "missing.cfg",
            "--config=rt.cfg",
            "--module=sub/m",
        ][..],
        &["ABCDEFGHIJKLMNOPQRSTUVWXYZ123456"][..],
    ] {
        assert_eq!(run_crun(err_flag, &[], &tmp).0, 1);
    }

    let abs_cfg = tmp.join("rt.cfg").to_string_lossy().into_owned();
    for env_set in [
        vec![
            (String::from("COB_RUNTIME_CONFIG"), abs_cfg),
            (String::from("COB_PRE_LOAD"), String::from("p1::p2")),
            (String::from("UNRELATED"), String::from("1")),
        ],
        vec![(String::from("COB_RUNTIME_CONFIG"), String::from("rt.cfg"))],
        vec![
            (String::from("COB_CONFIG_DIR"), String::from("cfgd")),
            (
                String::from("COB_RUNTIME_CONFIG"),
                String::from("in_dir.cfg"),
            ),
        ],
        vec![(
            String::from("COB_RUNTIME_CONFIG"),
            String::from("no_such_cfg.cfg"),
        )],
    ] {
        assert_eq!(run_crun(&["--version"], &env_set, &tmp).0, 0);
    }

    let bad_rt_env = [(
        String::from("COBLANG_RT_LIB_DIR"),
        String::from("/nonexistent_coblang_rt_dir_xyz"),
    )];
    let (rc_nort, _, err_nort) = run_crun(&["ANYMOD"], &bad_rt_env, &tmp);
    assert_eq!(rc_nort, 1);
    assert!(err_nort.contains("runtime library not found"));

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Exercises `execute_cobcrun_driver` end-to-end with compiled `.so` modules, directory/config targets,
/// >31-char preload shifting, `COB_LIBRARY_PATH`, and preload warning diagnostics.
#[test]
fn cobcrun_driver_dynamic_execution_and_error_coverage() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-crun-exec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let sub = tmp.join("sub");
    let mock_rt = tmp.join("mock_rt");
    std::fs::create_dir_all(&sub)?;
    std::fs::create_dir_all(&mock_rt)?;

    std::fs::write(
        tmp.join("hello.cob"),
        "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. HELLO.\n       PROCEDURE DIVISION.\n           STOP RUN.\n",
    )?;
    let bad_rt = [(
        String::from("COBLANG_RT_LIB_DIR"),
        String::from("/nonexistent_coblang_rt_dir_xyz"),
    )];
    assert_ne!(
        run_cdrv(&["-m", "hello.cob", "-o", "sub/BAD.so"], &bad_rt, &tmp).0,
        0
    );

    std::fs::write(
        tmp.join("mock_rt.c"),
        "void cob_init(int c, const char **v) { (void)c; (void)v; }\nvoid cob_tidy(void) {}\n",
    )?;
    std::fs::write(
        tmp.join("hello_mod.c"),
        "int __coblang_prog_HELLO(void) { return 0; }\n",
    )?;
    assert!(
        Command::new("cc")
            .args([
                "-shared",
                "-fPIC",
                "mock_rt.c",
                "-o",
                "mock_rt/libcoblang_rt.so"
            ])
            .current_dir(&tmp)
            .status()?
            .success()
    );
    assert!(
        Command::new("cc")
            .args(["-shared", "-fPIC", "hello_mod.c", "-o", "sub/HELLO.so"])
            .current_dir(&tmp)
            .status()?
            .success()
    );
    std::fs::write(tmp.join("load.cfg"), "library_path = sub\n")?;

    let env_ok = vec![
        (
            String::from("COBLANG_RT_LIB_DIR"),
            mock_rt.to_string_lossy().into_owned(),
        ),
        (String::from("COB_LIBRARY_PATH"), String::from("sub::.")),
    ];

    assert_eq!(run_crun(&["HELLO"], &env_ok, &tmp).0, 0);
    assert_eq!(run_crun(&["sub/", "HELLO"], &env_ok, &tmp).0, 0);
    assert_eq!(run_crun(&["sub", "HELLO"], &env_ok, &tmp).0, 0);
    assert_eq!(run_crun(&["load.cfg", "HELLO"], &env_ok, &tmp).0, 0);
    assert_eq!(run_crun(&["sub/HELLO"], &env_ok, &tmp).0, 0);
    let abs_mod = sub.join("HELLO").to_string_lossy().into_owned();
    assert_eq!(run_crun(&[abs_mod.as_str()], &env_ok, &tmp).0, 0);

    let (rc_warn_ok, _, err_warn_ok) = run_crun(
        &["ABCDEFGHIJKLMNOPQRSTUVWXYZ123456", "HELLO"],
        &env_ok,
        &tmp,
    );
    assert_eq!(rc_warn_ok, 0);
    assert!(err_warn_ok.contains("warning: preloading"));

    for fail_args in [
        &["sub"][..],
        &["load.cfg"][..],
        &["missing.cfg", "HELLO"][..],
        &["sub/ABCDEFGHIJKLMNOPQRSTUVWXYZ123456"][..],
        &["ABCDEFGHIJKLMNOPQRSTUVWXYZ123456.so"][..],
        &["-M", "sub/HELLO", "-M", "missing_preload", "NO_SUCH_MOD"][..],
    ] {
        assert_eq!(run_crun(fail_args, &env_ok, &tmp).0, 1);
    }

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Verifies `-fmemory-check` and `-fmemory-check=<scope>` return deterministic `EXIT_UNIMPLEMENTED`.
#[test]
fn driver_memory_check_flags_return_unimplemented() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-memchk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let cob = tmp.join("p.cob");
    std::fs::write(
        &cob,
        "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. P.\n       PROCEDURE DIVISION.\n           STOP RUN.\n",
    )?;
    for flag in ["-fmemory-check", "-fmemory-check=using"] {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let rc = execute_compiler_driver(
            DriverFlavor::Cobc,
            &svec(&["-x", flag, "p.cob"]),
            &[],
            &tmp,
            &mut out,
            &mut err,
        );
        assert_eq!(rc, EXIT_UNIMPLEMENTED);
    }
    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}
