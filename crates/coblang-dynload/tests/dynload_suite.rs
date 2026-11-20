//! Exhaustive unit and 100% branch-coverage tests for `coblang-dynload`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use coblang_common::{CoblangError, Result};
use coblang_dynload::{
    SymbolMangleStyle, candidate_library_filenames, format_dlerror_ptr, load_module_from_file,
    mangle_program_symbol, resolve_and_load_module, resolve_in_current_process,
};

/// Monotonic counter ensuring isolated per-test scratch directories.
static TEST_SEQ: AtomicU64 = AtomicU64::new(1);

/// Creates a fresh temporary directory for compiling test shared objects.
fn make_scratch_dir() -> Result<PathBuf> {
    let id = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("coblang-dynload-test-{}-{id}", std::process::id()));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Compiles a small C snippet into a shared library at `out_lib`.
///
/// # Arguments
///
/// * `dir` - Temporary working directory.
/// * `stem` - C source stem name.
/// * `c_src` - C source text.
/// * `out_lib` - Destination `.so` / `.dylib` path.
fn compile_c_shared(dir: &Path, stem: &str, c_src: &str, out_lib: &Path) -> Result<()> {
    let c_path = dir.join(format!("{stem}.c"));
    fs::write(&c_path, c_src)?;
    let status = Command::new("cc")
        .arg("-shared")
        .arg("-fPIC")
        .arg(&c_path)
        .arg("-o")
        .arg(out_lib)
        .status()?;
    assert!(status.success(), "cc -shared failed with {status}");
    Ok(())
}

/// Verifies [`mangle_program_symbol`] and [`candidate_library_filenames`] across mixed-case and hyphenated names.
#[test]
fn symbol_mangling_and_candidate_filenames() {
    assert_eq!(
        mangle_program_symbol("mod-1", SymbolMangleStyle::CobolCanonical),
        "__coblang_prog_MOD_2D_1"
    );
    assert_eq!(
        mangle_program_symbol("mod-1", SymbolMangleStyle::RawC),
        "mod_1"
    );
    assert_eq!(
        mangle_program_symbol("c_func_2", SymbolMangleStyle::RawC),
        "c_func_2"
    );

    let upper_files = candidate_library_filenames("MOD1");
    assert_eq!(upper_files.len(), 8);
    let lower_files = candidate_library_filenames("mod1");
    assert_eq!(lower_files.len(), 4);
}

/// Verifies canonical, raw-C fallback, missing-symbol, invalid-binary, interior-NUL, empty-name, and in-process resolution paths.
#[test]
fn dynamic_loader_end_to_end_and_error_branches() -> Result<()> {
    let dir = make_scratch_dir()?;

    // 1. Canonical COBOL symbol inside `.so` shared library.
    let mod1_lib = dir.join("mod1.so");
    compile_c_shared(
        &dir,
        "mod1",
        "int __coblang_prog_MOD1(void) { return 77; }",
        &mod1_lib,
    )?;
    let (_h1, sym1) = resolve_and_load_module("MOD1", std::slice::from_ref(&dir))?;
    assert_eq!(sym1.call(), 77);

    // 2. Raw-C fallback symbol inside `.dylib` shared library.
    let raw_lib = dir.join("rawmod.dylib");
    compile_c_shared(&dir, "rawmod", "int rawmod(void) { return 88; }", &raw_lib)?;
    let (_h2, sym2) = load_module_from_file(&raw_lib, "rawmod")?;
    assert_eq!(sym2.call(), 88);

    // 3. Valid shared library lacking target symbol -> `last_err` propagation in `resolve_and_load_module`.
    let nosym_lib = dir.join("nosym.so");
    compile_c_shared(
        &dir,
        "nosym",
        "int unrelated_fn(void) { return 1; }",
        &nosym_lib,
    )?;
    let err_nosym = resolve_and_load_module("nosym", std::slice::from_ref(&dir));
    assert!(matches!(
        err_nosym,
        Err(CoblangError::DynamicModuleLoad { .. })
    ));

    // 4. Corrupted non-ELF/non-Mach-O file -> `dlopen` failure + `dlerror()` message.
    let bad_lib = dir.join("corrupt.so");
    fs::write(&bad_lib, b"not a valid shared object")?;
    let err_bad = load_module_from_file(&bad_lib, "corrupt");
    assert!(matches!(
        err_bad,
        Err(CoblangError::DynamicModuleLoad { .. })
    ));

    // 5. Interior NUL byte in path -> `CString::new` error branch.
    let nul_path = PathBuf::from("bad\0path.so");
    let err_nul = load_module_from_file(&nul_path, "bad");
    assert!(matches!(
        err_nul,
        Err(CoblangError::DynamicModuleLoad { .. })
    ));

    // 6. Empty module name -> immediate validation error.
    let err_empty = resolve_and_load_module("   ", std::slice::from_ref(&dir));
    assert!(matches!(
        err_empty,
        Err(CoblangError::DynamicModuleLoad { .. })
    ));

    // 7. Non-existent module in valid directory -> not-found error.
    let err_missing = resolve_and_load_module("does_not_exist_xyz", std::slice::from_ref(&dir));
    assert!(matches!(
        err_missing,
        Err(CoblangError::DynamicModuleLoad { .. })
    ));

    // 8. Current-process lookup (`getpid` in libSystem/libc) + `format_dlerror_ptr` null/non-null branches.
    assert!(resolve_in_current_process("definitely_not_in_proc_xyz").is_none());
    let (_h_inproc, entry_inproc) = resolve_and_load_module("getpid", std::slice::from_ref(&dir))?;
    assert!(entry_inproc.call() > 0);

    #[expect(
        unsafe_code,
        reason = "Exercising both null and valid CStr branches of `format_dlerror_ptr`."
    )]
    // SAFETY: `core::ptr::null()` and `c"sample error".as_ptr()` satisfy `format_dlerror_ptr` preconditions.
    unsafe {
        assert_eq!(
            format_dlerror_ptr(core::ptr::null()),
            "unknown dynamic linker error"
        );
        assert_eq!(format_dlerror_ptr(c"sample error".as_ptr()), "sample error");
    }

    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies `as_fn_ptr`, explicit parent/extension resolution, `open_shared_object_global`,
/// and `preload_module_by_name` across all success and error branches.
#[test]
fn preload_and_global_resolution_branches() -> Result<()> {
    use coblang_dynload::{open_shared_object_global, preload_module_by_name};

    let dir = make_scratch_dir()?;
    let sub = dir.join("sub");
    fs::create_dir_all(&sub)?;

    let sub_mod = sub.join("submod.so");
    compile_c_shared(
        &dir,
        "submod",
        "int __coblang_prog_SUBMOD(void) { return 12; }\nint __coblang_prog_ALTENT(void) { return 34; }",
        &sub_mod,
    )?;
    let nosym_lib = sub.join("nosym2.so");
    compile_c_shared(
        &dir,
        "nosym2",
        "int other_fn(void) { return 0; }",
        &nosym_lib,
    )?;

    let abs_stem = sub.join("submod").to_string_lossy().into_owned();
    let (_h_abs, entry_abs) = resolve_and_load_module(&abs_stem, &[])?;
    assert!(!entry_abs.as_fn_ptr().is_null());
    assert_eq!(entry_abs.call(), 12);

    let abs_ext_ok = sub_mod.to_string_lossy().into_owned();
    let (_h_ext, entry_ext) = resolve_and_load_module(&abs_ext_ok, &[])?;
    assert_eq!(entry_ext.call(), 12);

    let abs_ext_err = nosym_lib.to_string_lossy().into_owned();
    assert!(resolve_and_load_module(&abs_ext_err, &[]).is_err());

    let (_h_rel, entry_rel) = resolve_and_load_module("sub/submod", std::slice::from_ref(&dir))?;
    assert_eq!(entry_rel.call(), 12);
    assert!(resolve_and_load_module("sub/missing_ext.so", std::slice::from_ref(&dir)).is_err());

    assert!(open_shared_object_global(Path::new("bad\0global.so")).is_err());
    let corrupt_so = dir.join("corrupt_global.so");
    fs::write(&corrupt_so, b"invalid dso bytes")?;
    assert!(open_shared_object_global(&corrupt_so).is_err());

    let g_direct = preload_module_by_name(&abs_ext_ok, &[])?;
    assert!(g_direct.lookup_entry("ALTENT").is_some());
    assert!(g_direct.lookup_entry("NO_SUCH_SYM").is_none());
    let _g_abs_parent = preload_module_by_name(&abs_stem, &[])?;
    let _g_rel_parent = preload_module_by_name("sub/submod", std::slice::from_ref(&dir))?;
    let _g_stem = preload_module_by_name("submod", std::slice::from_ref(&sub))?;
    assert!(preload_module_by_name("missing_preload_xyz", std::slice::from_ref(&dir)).is_err());

    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Verifies `execute_cobcrun_session` across missing runtime symbols, preloads, and DSO/entry dispatch.
#[test]
fn cobcrun_session_dispatch_and_error_branches() -> Result<()> {
    use coblang_dynload::{CobcrunExecSpec, execute_cobcrun_session};

    let dir = make_scratch_dir()?;
    let sub = dir.join("sub");
    fs::create_dir_all(&sub)?;

    compile_c_shared(
        &dir,
        "submod_s",
        "int __coblang_prog_SUBMOD(void) { return 12; }\nint __coblang_prog_ALTENT(void) { return 34; }",
        &sub.join("submod.so"),
    )?;
    compile_c_shared(
        &dir,
        "nosym_s",
        "int other_fn(void) { return 0; }",
        &sub.join("nosym2.so"),
    )?;

    for (sub_name, stem, c_src) in [
        ("rt_empty", None, ""),
        ("rt_no_init", Some("rt1"), "void unrelated_rt(void) {}"),
        (
            "rt_no_tidy",
            Some("rt2"),
            "void cob_init(int c, const char **v) { (void)c; (void)v; }",
        ),
    ] {
        let rt_dir = dir.join(sub_name);
        fs::create_dir_all(&rt_dir)?;
        if let Some(st) = stem {
            compile_c_shared(&dir, st, c_src, &rt_dir.join("libcoblang_rt.so"))?;
        }
        assert!(
            execute_cobcrun_session(&CobcrunExecSpec {
                runtime_lib_dir: &rt_dir,
                preloads: &[],
                search_dirs: std::slice::from_ref(&sub),
                target: "SUBMOD",
                args: &[],
            })
            .is_err()
        );
    }

    let rt_valid = dir.join("rt_valid");
    fs::create_dir_all(&rt_valid)?;
    compile_c_shared(
        &dir,
        "rt3",
        "void cob_init(int c, const char **v) { (void)c; (void)v; }\nvoid cob_tidy(void) {}",
        &rt_valid.join("libcoblang_rt.so"),
    )?;

    let preloads = vec![String::from("submod"), String::from("no_such_preload")];
    let args_nul = vec![String::from("arg\0one")];
    let (rc_bare, failed_list) = execute_cobcrun_session(&CobcrunExecSpec {
        runtime_lib_dir: &rt_valid,
        preloads: &preloads,
        search_dirs: std::slice::from_ref(&sub),
        target: "SUBMOD",
        args: &args_nul,
    })?;
    assert_eq!(rc_bare, 12);
    assert_eq!(failed_list, vec![String::from("no_such_preload")]);

    let slash_target = sub.join("submod").to_string_lossy().into_owned();
    for (args_slice, expected_rc) in [
        (&[String::from("ALTENT"), String::from("x")][..], 34_u8),
        (&[String::from("--regular-arg")][..], 12_u8),
        (&[][..], 12_u8),
    ] {
        let (rc_val, _) = execute_cobcrun_session(&CobcrunExecSpec {
            runtime_lib_dir: &rt_valid,
            preloads: &[],
            search_dirs: std::slice::from_ref(&sub),
            target: &slash_target,
            args: args_slice,
        })?;
        assert_eq!(rc_val, expected_rc);
    }

    let args_reg = [String::from("--regular-arg")];
    let slash_nosym = sub.join("nosym2").to_string_lossy().into_owned();
    for bad_target in [slash_nosym.as_str(), "sub/nonexistent_dso"] {
        assert!(
            execute_cobcrun_session(&CobcrunExecSpec {
                runtime_lib_dir: &rt_valid,
                preloads: &[],
                search_dirs: std::slice::from_ref(&sub),
                target: bad_target,
                args: &args_reg,
            })
            .is_err()
        );
    }

    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

/// Sample in-binary 0-arg C-ABI callback.
extern "C" fn inbin_zero_fn() -> i32 {
    41
}

/// Counter mutated by [`inbin_cancel_hook`] to prove `CANCEL` state reset.
static INBIN_CANCEL_COUNT: AtomicU64 = AtomicU64::new(0);

/// Sample in-binary `CANCEL` reset callback.
extern "C" fn inbin_cancel_hook() {
    let _ = INBIN_CANCEL_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Sample arity-generic COBOL thunk summing `word_count` `usize` words.
///
/// # Arguments
///
/// * `word_count` - Word count.
/// * `words_ptr` - Pointer to `word_count` `usize` words (or null when `word_count <= 0`).
extern "C" fn inbin_sum_thunk(word_count: i32, words_ptr: *const usize) -> i64 {
    let Ok(n) = usize::try_from(word_count) else {
        return -1;
    };
    if n == 0 || words_ptr.is_null() {
        return 99;
    }
    #[expect(
        unsafe_code,
        reason = "Audited slice reconstruction inside unit-test COBOL thunk."
    )]
    // SAFETY: Unit test passes valid `words_ptr` slice of length `n`.
    let words = unsafe { core::slice::from_raw_parts(words_ptr, n) };
    let total: usize = words.iter().copied().sum();
    i64::try_from(total).unwrap_or(i64::MAX)
}

/// Sample 0-arg C companion function.
extern "C" fn raw_c_0() -> isize {
    10
}
/// Sample 1-arg C companion function.
extern "C" fn raw_c_1(a0: usize) -> isize {
    a0.cast_signed()
}
/// Sample 2-arg C companion function.
extern "C" fn raw_c_2(a0: usize, a1: usize) -> isize {
    (a0 + a1).cast_signed()
}
/// Sample 3-arg C companion function.
extern "C" fn raw_c_3(a0: usize, a1: usize, a2: usize) -> isize {
    (a0 + a1 + a2).cast_signed()
}
/// Sample 4-arg C companion function.
extern "C" fn raw_c_4(a0: usize, a1: usize, a2: usize, a3: usize) -> isize {
    (a0 + a1 + a2 + a3).cast_signed()
}
/// Sample 5-arg C companion function.
extern "C" fn raw_c_5(a0: usize, a1: usize, a2: usize, a3: usize, a4: usize) -> isize {
    (a0 + a1 + a2 + a3 + a4).cast_signed()
}
/// Sample 6-arg C companion function.
extern "C" fn raw_c_6(a0: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize) -> isize {
    (a0 + a1 + a2 + a3 + a4 + a5).cast_signed()
}

/// Verifies `CobolThunk`/`CobolCancel` mangling and `ResolvedEntrySymbol::call_with_args` across all arities.
#[test]
fn resolved_entry_thunk_and_companion_arity_dispatch() -> Result<()> {
    use coblang_dynload::ResolvedEntrySymbol;

    assert_eq!(
        mangle_program_symbol("sub-1", SymbolMangleStyle::CobolThunk),
        "__coblang_thunk_SUB_2D_1"
    );
    assert_eq!(
        mangle_program_symbol("sub-1", SymbolMangleStyle::CobolCancel),
        "__coblang_cancel_SUB_2D_1"
    );

    let plain_sym = ResolvedEntrySymbol::from_fn(inbin_zero_fn);
    assert!(!plain_sym.has_thunk());
    assert!(!plain_sym.has_cancel_hook());
    assert_eq!(plain_sym.call(), 41);
    assert!(!plain_sym.reset_state());

    let thunk_sym =
        ResolvedEntrySymbol::from_thunk(inbin_zero_fn, inbin_sum_thunk, Some(inbin_cancel_hook));
    assert!(thunk_sym.has_thunk());
    assert!(thunk_sym.has_cancel_hook());
    assert_eq!(thunk_sym.call(), 99);
    let before_cnt = INBIN_CANCEL_COUNT.load(Ordering::Relaxed);
    assert!(thunk_sym.reset_state());
    assert_eq!(INBIN_CANCEL_COUNT.load(Ordering::Relaxed), before_cnt + 1);

    #[expect(
        unsafe_code,
        reason = "Testing `from_raw_code_ptr` and `call_with_args` across all arity arms."
    )]
    // SAFETY: All function pointers are valid matching C-ABI functions in this test binary.
    unsafe {
        assert!(ResolvedEntrySymbol::from_raw_code_ptr(core::ptr::null()).is_none());
        let from_ptr = ResolvedEntrySymbol::from_raw_code_ptr(plain_sym.as_fn_ptr());
        assert_eq!(from_ptr.map(ResolvedEntrySymbol::call), Some(41));
        assert_eq!(thunk_sym.call_with_args(&[1, 2, 3, 4, 5, 6, 7, 8])?, 36);

        let fns: [(*const u8, &[usize], i64); 7] = [
            ((raw_c_0 as *const ()).cast(), &[], 10),
            ((raw_c_1 as *const ()).cast(), &[5], 5),
            ((raw_c_2 as *const ()).cast(), &[2, 3], 5),
            ((raw_c_3 as *const ()).cast(), &[1, 2, 3], 6),
            ((raw_c_4 as *const ()).cast(), &[1, 2, 3, 4], 10),
            ((raw_c_5 as *const ()).cast(), &[1, 2, 3, 4, 5], 15),
            ((raw_c_6 as *const ()).cast(), &[1, 2, 3, 4, 5, 6], 21),
        ];
        for (ptr, slice, expected) in fns {
            let Some(sym) = ResolvedEntrySymbol::from_raw_code_ptr(ptr) else {
                continue;
            };
            assert_eq!(sym.call_with_args(slice)?, expected);
        }
        assert!(plain_sym.call_with_args(&[1, 2, 3, 4, 5, 6, 7]).is_err());
    }
    Ok(())
}

/// Verifies [`DynamicModuleRegistry`] in-binary priority, `-M` / `COB_LIBRARY_PATH` search,
/// `CANCEL` state reset, and singleton poison recovery.
#[test]
fn registry_resolution_and_cancel_state_reset() -> Result<()> {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use coblang_dynload::{
        CancelOutcome, DynamicModuleRegistry, ResolvedEntrySymbol, cancel_dynamic_module,
        clear_dynamic_module_registry, global_module_registry, open_shared_object_global,
        register_in_binary_module, resolve_dynamic_module, set_extra_module_search_dirs,
        with_module_registry,
    };

    let plain_sym = ResolvedEntrySymbol::from_fn(inbin_zero_fn);
    let thunk_sym =
        ResolvedEntrySymbol::from_thunk(inbin_zero_fn, inbin_sum_thunk, Some(inbin_cancel_hook));
    let dir = make_scratch_dir()?;
    let m_dir = dir.join("mdir");
    let env_dir = dir.join("envdir");
    fs::create_dir_all(&m_dir)?;
    fs::create_dir_all(&env_dir)?;

    compile_c_shared(
        &dir,
        "stateful",
        concat!(
            "static int counter = 10;\n",
            "int __coblang_prog_STATEFUL(void) { counter += 1; return counter; }\n",
            "long long __coblang_thunk_STATEFUL(int wc, const unsigned long *wp) {\n",
            "  (void)wc; (void)wp; counter += 1; return counter;\n",
            "}\n",
            "void __coblang_cancel_STATEFUL(void) { counter = 10; }\n"
        ),
        &m_dir.join("stateful.so"),
    )?;
    compile_c_shared(
        &dir,
        "envonly",
        "int __coblang_prog_ENVONLY(void) { return 55; }",
        &env_dir.join("envonly.so"),
    )?;
    let gproc_path = dir.join("gproc.so");
    compile_c_shared(
        &dir,
        "gproc",
        concat!(
            "static int gstate = 0;\n",
            "int __coblang_prog_GPROCMOD(void) { gstate += 1; return gstate; }\n",
            "void __coblang_cancel_GPROCMOD(void) { gstate = 0; }\n"
        ),
        &gproc_path,
    )?;
    let _global_keep = open_shared_object_global(&gproc_path)?;

    let mut reg = DynamicModuleRegistry::default();
    reg.register_in_binary("   ", plain_sym);
    reg.register_in_binary("stateful", thunk_sym);
    reg.set_extra_search_dirs(vec![m_dir.clone()]);
    assert_eq!(reg.extra_search_dirs(), std::slice::from_ref(&m_dir));
    let env_str = format!("{}:", env_dir.display());
    assert_eq!(reg.build_search_dirs(None).len(), 2);
    assert_eq!(reg.build_search_dirs(Some(&env_str)).len(), 3);

    assert!(reg.resolve_module("  ", None).is_err());
    assert_eq!(reg.resolve_module("STATEFUL", None)?.call(), 99);
    assert_eq!(reg.cancel_module("   "), CancelOutcome::NotActive);
    assert_eq!(reg.cancel_module("stateful"), CancelOutcome::ResetInBinary);
    assert_eq!(reg.cancel_module("getpid"), CancelOutcome::NotActive);
    assert_eq!(reg.cancel_module("GPROCMOD"), CancelOutcome::ResetInBinary);
    assert_eq!(
        reg.cancel_module("NEVER_LOADED_XYZ"),
        CancelOutcome::NotActive
    );

    reg.clear_all();
    reg.set_extra_search_dirs(vec![m_dir]);
    let s1 = reg.resolve_module("stateful", Some(&env_str))?;
    assert!(s1.has_thunk() && s1.has_cancel_hook() && s1.call() == 11);
    assert_eq!(reg.resolve_module("STATEFUL", Some(&env_str))?.call(), 12);
    assert_eq!(
        reg.cancel_module("stateful"),
        CancelOutcome::UnloadedDynamic
    );
    assert_eq!(reg.resolve_module("stateful", Some(&env_str))?.call(), 11);
    assert_eq!(reg.resolve_module("envonly", Some(&env_str))?.call(), 55);
    assert!(reg.resolve_module("missing_mod_xyz", None).is_err());
    reg.clear_loaded_dsos();

    register_in_binary_module("INBIN_GLOBAL", plain_sym);
    set_extra_module_search_dirs(vec![env_dir]);
    assert_eq!(resolve_dynamic_module("inbin_global", None)?.call(), 41);
    assert_eq!(
        cancel_dynamic_module("inbin_global"),
        CancelOutcome::ResetInBinary
    );
    clear_dynamic_module_registry();

    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _guard = global_module_registry().lock();
        std::panic::resume_unwind(Box::new("simulated registry poison"));
    }));
    with_module_registry(DynamicModuleRegistry::clear_all);
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}
