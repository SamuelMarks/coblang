//! End-to-end native executable integration tests for Milestone M20 (`XML`/`JSON` & OO COBOL).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use coblang_common::{DriverFlavor, Result};
use coblang_rt as _;
use coblang_scaffold_driver::execute_compiler_driver;

/// Monotonic scratch directory counter.
static NEXT_TMP_ID: AtomicU64 = AtomicU64::new(1);

/// Creates a unique scratch directory inside `std::env::temp_dir()`.
fn make_tmp_dir(tag: &str) -> Result<PathBuf> {
    let n = NEXT_TMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("coblang_m20_{tag}_{}_{n}", std::process::id()));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Locates the directory containing `libcoblang_rt` so `-x` linking finds `-lcoblang_rt`.
fn find_rt_lib_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let deps_dir = exe.parent()?;
    let target_profile = deps_dir.parent()?;
    if target_profile.join("libcoblang_rt.a").exists()
        || target_profile.join("libcoblang_rt.dylib").exists()
        || target_profile.join("libcoblang_rt.so").exists()
    {
        return Some(target_profile.to_path_buf());
    }
    Some(deps_dir.to_path_buf())
}

/// Compiles `src` with `coblang -x -free` + `extra_flags` and executes the resulting binary.
fn compile_and_run_cobol(tag: &str, src: &str, extra_flags: &[&str]) -> Result<String> {
    let dir = make_tmp_dir(tag)?;
    let cob_path = dir.join("prog.cob");
    let bin_path = dir.join("prog.bin");
    fs::write(&cob_path, src)?;

    let mut args = vec![
        String::from("-x"),
        String::from("-free"),
        cob_path.display().to_string(),
        String::from("-o"),
        bin_path.display().to_string(),
    ];
    if let Some(rt_dir) = find_rt_lib_dir() {
        args.push(format!("-L{}", rt_dir.display()));
    }
    for &fl in extra_flags {
        args.push(String::from(fl));
    }
    let mut out_buf = Vec::new();
    let mut err_buf = Vec::new();
    let rc = execute_compiler_driver(
        DriverFlavor::Coblang,
        &args,
        &[],
        &dir,
        &mut out_buf,
        &mut err_buf,
    );
    assert_eq!(
        rc,
        0,
        "coblang compile failed ({rc}): {}",
        String::from_utf8_lossy(&err_buf)
    );

    let mut cmd = Command::new(&bin_path);
    if let Some(rt_dir) = find_rt_lib_dir() {
        cmd.env("DYLD_LIBRARY_PATH", &rt_dir);
        cmd.env("LD_LIBRARY_PATH", &rt_dir);
    }
    let run = cmd.output()?;
    let _ = fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "binary exited with {:?}: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
    Ok(String::from_utf8_lossy(&run.stdout).into_owned())
}

/// Verifies end-to-end OO COBOL inheritance, `FACTORY` `new`, instance state, `SUPER`, `SELF`, and inline `::`.
#[test]
fn m20_oo_inheritance_vtable_and_inline_invoke_e2e() -> Result<()> {
    let src = r#"
IDENTIFICATION DIVISION.
CLASS-ID. Wallet AS "Wallet".
OBJECT.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 AMOUNT PIC 9(4) VALUE 100.
PROCEDURE DIVISION.
METHOD-ID. deposit.
DATA DIVISION.
LINKAGE SECTION.
01 DELTA PIC 9(4).
PROCEDURE DIVISION USING DELTA.
    ADD DELTA TO AMOUNT.
END METHOD deposit.

METHOD-ID. currentAmount.
DATA DIVISION.
LINKAGE SECTION.
01 RES PIC 9(4).
PROCEDURE DIVISION RETURNING RES.
    MOVE AMOUNT TO RES.
END METHOD currentAmount.
END OBJECT.
END CLASS Wallet.

IDENTIFICATION DIVISION.
CLASS-ID. BonusWallet AS "BonusWallet" INHERITS FROM Wallet.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
REPOSITORY.
    CLASS Wallet AS "Wallet".
OBJECT.
PROCEDURE DIVISION.
METHOD-ID. deposit.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 BONUS PIC 9(4) VALUE 5.
LINKAGE SECTION.
01 DELTA PIC 9(4).
PROCEDURE DIVISION USING DELTA.
    INVOKE SUPER "deposit" USING DELTA.
    INVOKE SUPER "deposit" USING BONUS.
END METHOD deposit.
END OBJECT.
END CLASS BonusWallet.

IDENTIFICATION DIVISION.
PROGRAM-ID. MAIN-OO.
ENVIRONMENT DIVISION.
CONFIGURATION SECTION.
REPOSITORY.
    CLASS BonusWallet AS "BonusWallet".
DATA DIVISION.
WORKING-STORAGE SECTION.
01 W USAGE OBJECT REFERENCE BonusWallet.
01 ADD-V PIC 9(4) VALUE 20.
01 TOT PIC 9(4) VALUE 0.
PROCEDURE DIVISION.
    INVOKE BonusWallet "new" RETURNING W.
    INVOKE W "deposit" USING ADD-V.
    MOVE W::"currentAmount"() TO TOT.
    DISPLAY "TOTAL=" TOT.
    STOP RUN.
END PROGRAM MAIN-OO.
"#;
    let stdout = compile_and_run_cobol("oo_e2e", src, &["-std=cobol2023"])?;
    assert_eq!(
        stdout,
        "TOTAL=0125
"
    );
    Ok(())
}

/// Verifies end-to-end `JSON GENERATE` -> `JSON PARSE INTO` roundtrip including booleans and `WITH DETAIL`.
#[test]
fn m20_json_generate_and_parse_roundtrip_e2e() -> Result<()> {
    let src = r#"
IDENTIFICATION DIVISION.
PROGRAM-ID. JSON-RT.
DATA DIVISION.
WORKING-STORAGE SECTION.
01 SRC-ORDER.
   05 ORD-ID PIC 9(3) VALUE 77.
   05 IS-PAID PIC X VALUE 'Y'.
   05 BUYER PIC X(6) VALUE 'Alice'.
01 DST-ORDER.
   05 ORD-ID PIC 9(3) VALUE 0.
   05 IS-PAID PIC X VALUE 'N'.
   05 BUYER PIC X(6) VALUE SPACES.
01 JBUF PIC X(80) VALUE SPACES.
PROCEDURE DIVISION.
    JSON GENERATE JBUF FROM SRC-ORDER
        NAME OF SRC-ORDER IS OMITTED ORD-ID IS 'id'
        CONVERTING IS-PAID TO JSON BOOLEAN USING 'Y'
    END-JSON.
    JSON PARSE JBUF INTO DST-ORDER WITH DETAIL
        NAME OF DST-ORDER IS OMITTED ORD-ID IS 'id'
        CONVERTING IS-PAID FROM JSON BOOLEAN USING 'T' AND 'F'
    END-JSON.
    DISPLAY ORD-ID OF DST-ORDER "|" IS-PAID OF DST-ORDER "|" FUNCTION TRIM(BUYER OF DST-ORDER) "|" JSON-CODE.
    STOP RUN.
"#;
    let stdout = compile_and_run_cobol("json_rt", src, &["-std=cobol2023"])?;
    assert_eq!(
        stdout,
        "077|T|Alice|+000000000
"
    );
    Ok(())
}
