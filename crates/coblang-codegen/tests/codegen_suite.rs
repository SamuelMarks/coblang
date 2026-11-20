//! Exhaustive unit, FFI error-guard, and native execution tests for `coblang-codegen`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use coblang_codegen::linker::has_runtime_lib;
use coblang_codegen::llvm_bridge::{
    check_emit_buffer_result, consume_llvm_error_message, map_codegen_opt_level,
    translate_raw_mlir_op,
};
use coblang_codegen::{
    LinkArtifactKind, LinkerInvocationSpec, emit_llvm_ir_text, emit_target_assembly,
    emit_target_object, host_default_target_triple, invoke_system_linker, locate_runtime_lib_dir,
    resolve_output_path, translate_to_llvm_module,
};
use coblang_common::{CoblangError, Dialect, OptimizationLevel, Result, SourceFormat};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap, SymbolInterner, WarningPolicy};
use coblang_mlir::{
    CobolMlirContext, MainEntryMode, lower_cobol_to_llvm_mlir, lower_hir_to_cobol_mlir,
};
use coblang_pp::{
    DebugLinePolicy, InMemoryCopybookResolver, PreprocessConfig, preprocess_source_file,
};
use coblang_sema::lower_compilation_group;
use coblang_syntax::parse_preprocessed_unit;
use llvm_sys::core::{LLVMCreateMemoryBufferWithMemoryRangeCopy, LLVMCreateMessage};
use llvm_sys::target_machine::LLVMCodeGenOptLevel;

/// Force Cargo to link `coblang-rt-abi` (`libcoblang_rt`) when building this test suite.
use coblang_rt as _;

/// Monotonic counter for isolated test directories.
static CG_SEQ: AtomicU64 = AtomicU64::new(1);

/// Creates an isolated temporary directory for codegen/linker tests.
fn make_temp_dir() -> Result<PathBuf> {
    let n = CG_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("coblang-codegen-test-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Compiles `src` through `coblang-mlir` and translates to `OwnedLlvmModule`.
///
/// # Arguments
///
/// * `src` - COBOL source text.
/// * `main_mode` - Whether to synthesize `@main`.
fn compile_cobol_to_llvm_mod(
    src: &str,
    main_mode: MainEntryMode,
) -> Result<coblang_codegen::OwnedLlvmModule> {
    let dcfg = DialectConfig::for_dialect(Dialect::Gnu);
    let resolver = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dcfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &resolver,
    };
    let mut sm = SourceMap::new();
    let mut interner = SymbolInterner::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let unit = preprocess_source_file(
        "prog.cob",
        std::path::Path::new("."),
        src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let grp = parse_preprocessed_unit(&unit, &dcfg, &mut interner, &mut sink)?;
    let hir = lower_compilation_group(&grp, &dcfg, &mut interner, &mut sink)?;

    let ctx = CobolMlirContext::new();
    let verified = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner)?;
    let lowered = lower_cobol_to_llvm_mlir(&ctx, &verified, OptimizationLevel::O2, main_mode)?;
    translate_to_llvm_module(&lowered)
}

/// Verifies `.ll`, `.s`, `.o` emission, `OptimizationLevel` mapping, and `llvm_bridge` error guards.
#[test]
fn llvm_bridge_ir_asm_object_and_error_guards() -> Result<()> {
    assert_eq!(
        map_codegen_opt_level(OptimizationLevel::O0),
        LLVMCodeGenOptLevel::LLVMCodeGenLevelNone
    );
    assert_eq!(
        map_codegen_opt_level(OptimizationLevel::O1),
        LLVMCodeGenOptLevel::LLVMCodeGenLevelLess
    );
    assert_eq!(
        map_codegen_opt_level(OptimizationLevel::O2),
        LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault
    );
    assert_eq!(
        map_codegen_opt_level(OptimizationLevel::Os),
        LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault
    );
    assert_eq!(
        map_codegen_opt_level(OptimizationLevel::O3),
        LLVMCodeGenOptLevel::LLVMCodeGenLevelAggressive
    );

    let src = "IDENTIFICATION DIVISION.
PROGRAM-ID. CG1.
PROCEDURE DIVISION.
DISPLAY 'HELLO-CG'.
STOP RUN 0.
";
    let llvm_mod = compile_cobol_to_llvm_mod(src, MainEntryMode::EmitMain)?;

    let ll = emit_llvm_ir_text(&llvm_mod);
    assert!(ll.contains("@main"));

    let triple = host_default_target_triple();
    assert_ne!(triple, "");

    let asm = emit_target_assembly(&llvm_mod, &triple, OptimizationLevel::O1)?;
    assert_ne!(asm, "");

    let obj = emit_target_object(&llvm_mod, &triple, OptimizationLevel::O2)?;
    assert_eq!(obj.target_triple(), triple);
    assert_ne!(obj.as_bytes(), []);
    assert_ne!(obj.into_bytes(), [] as [u8; 0]);

    // Invalid target triple and interior NUL target triple errors.
    assert!(matches!(
        emit_target_assembly(&llvm_mod, "bogus-unknown-triple-xyz", OptimizationLevel::O0),
        Err(CoblangError::CodegenTarget { .. })
    ));
    assert!(matches!(
        emit_target_object(&llvm_mod, "bad\0triple", OptimizationLevel::O0),
        Err(CoblangError::CodegenTarget { .. })
    ));

    // Negative `translate_raw_mlir_op`: unlowered `cobol.program` cannot be translated to LLVM IR.
    test_unlowered_mlir_translation_rejection(src)?;
    test_low_level_buffer_and_message_helpers()
}

/// Verifies `translate_raw_mlir_op` cleanly returns `CoblangError::CodegenTarget` on unlowered `cobol.*` ops.
///
/// # Arguments
///
/// * `src` - Valid COBOL source text.
fn test_unlowered_mlir_translation_rejection(src: &str) -> Result<()> {
    let dcfg = DialectConfig::for_dialect(Dialect::Gnu);
    let resolver = InMemoryCopybookResolver::new();
    let pp_cfg = PreprocessConfig {
        initial_format: SourceFormat::Free,
        dialect_config: &dcfg,
        debug_policy: DebugLinePolicy::StripDebugLines,
        initial_defines: &[],
        copybook_resolver: &resolver,
    };
    let mut sm = SourceMap::new();
    let mut interner = SymbolInterner::new();
    let mut sink = DiagnosticSink::new(WarningPolicy::new(), 50);
    let unit = preprocess_source_file(
        "u.cob",
        std::path::Path::new("."),
        src,
        &pp_cfg,
        &mut sm,
        &mut sink,
    )?;
    let grp = parse_preprocessed_unit(&unit, &dcfg, &mut interner, &mut sink)?;
    let hir = lower_compilation_group(&grp, &dcfg, &mut interner, &mut sink)?;

    let ctx = CobolMlirContext::new();
    let verified = lower_hir_to_cobol_mlir(&ctx, &hir, &sm, &interner)?;
    let raw_op = verified.as_module().as_operation().to_raw();
    assert!(matches!(
        translate_raw_mlir_op(raw_op),
        Err(CoblangError::CodegenTarget { .. })
    ));
    Ok(())
}

/// Exercises `consume_llvm_error_message` and `check_emit_buffer_result` boundary branches.
fn test_low_level_buffer_and_message_helpers() -> Result<()> {
    #[expect(
        unsafe_code,
        reason = "Testing null/non-null LLVM error message and zero-length memory buffer branches."
    )]
    // SAFETY: `LLVMCreateMessage` and `LLVMCreateMemoryBufferWithMemoryRangeCopy` allocate valid LLVM handles disposed by the functions under test.
    unsafe {
        assert_eq!(
            consume_llvm_error_message(core::ptr::null_mut()),
            "unknown LLVM target error"
        );
        let msg = LLVMCreateMessage(c"custom llvm msg".as_ptr());
        assert_eq!(consume_llvm_error_message(msg), "custom llvm msg");

        let err_res =
            check_emit_buffer_result(1, core::ptr::null_mut(), core::ptr::null_mut(), "t");
        assert!(matches!(err_res, Err(CoblangError::CodegenTarget { .. })));

        let empty_buf =
            LLVMCreateMemoryBufferWithMemoryRangeCopy(c"".as_ptr(), 0, c"empty".as_ptr());
        let empty_vec = check_emit_buffer_result(0, core::ptr::null_mut(), empty_buf, "t")?;
        assert_eq!(empty_vec.len(), 0);
    }
    Ok(())
}

/// Verifies `linker.rs` path resolution, runtime locator, executable/module linking, and error branches.
#[test]
fn linker_executable_shared_module_and_error_paths() -> Result<()> {
    let dir = make_temp_dir()?;

    // Path resolution: default ext, explicit relative file, explicit directory, explicit absolute file.
    assert_eq!(
        resolve_output_path(&dir, std::path::Path::new("sub/prog.cob"), None, ""),
        dir.join("prog")
    );
    assert_eq!(
        resolve_output_path(&dir, std::path::Path::new("prog.cob"), None, "so"),
        dir.join("prog.so")
    );
    assert_eq!(
        resolve_output_path(&dir, std::path::Path::new("prog.cob"), Some("."), "so"),
        dir.join(".").join("prog.so")
    );
    let abs_out = dir.join("custom.bin");
    assert_eq!(
        resolve_output_path(&dir, std::path::Path::new("prog.cob"), abs_out.to_str(), ""),
        abs_out
    );
    assert_eq!(
        resolve_output_path(
            &dir,
            std::path::Path::new("rel_out.bin"),
            Some("rel_out.bin"),
            ""
        ),
        dir.join("rel_out.bin")
    );

    // Runtime locator failure vs override hit vs current_exe hit.
    assert!(!has_runtime_lib(&dir));
    assert!(matches!(
        locate_runtime_lib_dir(Some(&dir), None, &[]),
        Err(CoblangError::LinkerInvocation { .. })
    ));

    let fake_rt_dir = dir.join("fakert");
    fs::create_dir_all(&fake_rt_dir)?;
    fs::write(fake_rt_dir.join("libcoblang_rt.a"), b"stub")?;
    assert_eq!(
        locate_runtime_lib_dir(Some(&fake_rt_dir), None, &[])?,
        fake_rt_dir
    );

    let exe_path = std::env::current_exe()?;
    let real_rt_dir = locate_runtime_lib_dir(None, Some(&exe_path), &[])?;

    link_and_run_native_artifacts(&dir, &real_rt_dir)?;

    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Compiles, links, and executes a shared module (`-m`) + standalone executable (`-x`), plus linker error cases.
///
/// # Arguments
///
/// * `dir` - Temporary working directory.
/// * `rt_dir` - Directory containing compiled `libcoblang_rt`.
fn link_and_run_native_artifacts(dir: &std::path::Path, rt_dir: &std::path::Path) -> Result<()> {
    let triple = host_default_target_triple();

    // 1. Build shared module `submod.so`.
    let mod_src = "IDENTIFICATION DIVISION.
PROGRAM-ID. SUBMOD.
PROCEDURE DIVISION.
MOVE 7 TO RETURN-CODE.
EXIT PROGRAM.
";
    let mod_llvm = compile_cobol_to_llvm_mod(mod_src, MainEntryMode::OmitMain)?;
    let mod_obj = emit_target_object(&mod_llvm, &triple, OptimizationLevel::O1)?;
    let mod_o_path = dir.join("submod.o");
    let mod_so_path = dir.join("submod.so");
    fs::write(&mod_o_path, mod_obj.as_bytes())?;

    let mod_spec = LinkerInvocationSpec {
        linker_bin: String::from("cc"),
        kind: LinkArtifactKind::SharedModule,
        object_inputs: vec![mod_o_path],
        output_path: mod_so_path.clone(),
        runtime_lib_dir: rt_dir.to_path_buf(),
        extra_lib_dirs: vec![dir.to_path_buf()],
        extra_libs: vec![String::from("m")],
    };
    invoke_system_linker(&mod_spec)?;
    assert!(mod_so_path.is_file());

    // 2. Build executable `mainprog` calling `"SUBMOD"` dynamically and printing `"OK"`.
    let exe_src = "IDENTIFICATION DIVISION.
PROGRAM-ID. MAINPROG.
PROCEDURE DIVISION.
CALL 'SUBMOD'.
DISPLAY 'OK'.
STOP RUN.
";
    let exe_llvm = compile_cobol_to_llvm_mod(exe_src, MainEntryMode::EmitMain)?;
    let exe_obj = emit_target_object(&exe_llvm, &triple, OptimizationLevel::O2)?;
    let exe_o_path = dir.join("mainprog.o");
    let exe_bin_path = dir.join("mainprog");
    fs::write(&exe_o_path, exe_obj.as_bytes())?;

    let exe_spec = LinkerInvocationSpec {
        linker_bin: String::from("cc"),
        kind: LinkArtifactKind::Executable,
        object_inputs: vec![exe_o_path.clone()],
        output_path: exe_bin_path.clone(),
        runtime_lib_dir: rt_dir.to_path_buf(),
        extra_lib_dirs: Vec::new(),
        extra_libs: Vec::new(),
    };
    invoke_system_linker(&exe_spec)?;

    let run_out = Command::new(&exe_bin_path).current_dir(dir).output()?;
    assert_eq!(run_out.status.code(), Some(7));
    assert_eq!(run_out.stdout, [b'O', b'K', 0x0A]);

    // 3. Missing linker binary + failing linker exit code branches.
    let bad_bin = LinkerInvocationSpec {
        linker_bin: String::from("/definitely/missing/linker-xyz"),
        ..exe_spec.clone()
    };
    assert!(matches!(
        invoke_system_linker(&bad_bin),
        Err(CoblangError::LinkerInvocation {
            exit_code: None,
            ..
        })
    ));

    let bad_obj = dir.join("invalid.o");
    fs::write(&bad_obj, b"not an object file")?;
    let bad_link = LinkerInvocationSpec {
        object_inputs: vec![bad_obj],
        output_path: dir.join("fail.bin"),
        ..exe_spec
    };
    assert!(matches!(
        invoke_system_linker(&bad_link),
        Err(CoblangError::LinkerInvocation {
            exit_code: Some(_),
            ..
        })
    ));
    Ok(())
}
