//! Unit, full 12-module NIST CCVS-85 import, and live `cobc` conformance verification tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use coblang_common::{CoblangError, NistModule, Result, TestOutcome, encode_manifest_bundle};
use coblang_test_importer::nist85::{
    Exec85Config, build_nist_manifests_from_archive, extract_newcob_from_str, import_nist85_suite,
    nist_program_extra_flags, parse_exec85_config, parse_module_baseline, preprocess_exec85_lines,
};
use coblang_test_runner::capabilities::HostCapabilities;
use coblang_test_runner::cli::resolve_oracle_backend;
use coblang_test_runner::engine::{
    CompilerBackend, RunnerConfig, SandboxRetention, execute_single_test, execute_suite_parallel,
};

/// Exercises `EXEC85` config parsing, col-7 optional switches, `XXXX` substitution, and error cases.
#[test]
fn exec85_preprocessor_and_error_branches() -> Result<()> {
    let conf = r#"
*KILL-DELETIONS
X-00173 OUTFILE.
X-00273 "PLAIN"
X-04873 "custom_alt"
X-04873 "../copyalt"
X-ABC73 BADNUM
X-BAD
*BEGIN-UPDATE
000010       * ORPHAN SEQ BEFORE START
ab
*START
*START P1
000050       * INSERTED BEFORE
000200       * REPLACED LINE
009900       * INSERTED AT TAIL
ignored non-seq
*END-UPDATE
"#;
    let empty_start =
        parse_exec85_config("*BEGIN-UPDATE\u{000a}*START \u{0020}\u{000a}*END-UPDATE\u{000a}");
    assert!(empty_start.updates.is_empty());
    let mut cfg = parse_exec85_config(conf);
    assert!(cfg.kill_deletions);
    assert_eq!(
        cfg.x_cards.get(&48).map(String::as_str),
        Some(r#""copyalt""#)
    );

    let raw_lines = vec![
        String::from("short"),
        String::from("000100A    DISPLAY 'SELECTED SWITCH A'."),
        String::from("000150Z    DISPLAY 'DELETED SWITCH Z'."),
        String::from("000200     ORIGINAL LINE."),
        String::from("000300     XXXXX001 "),
        String::from(
            "000400     XXXXX002.                                                    SEQ80",
        ),
        String::from("000500     XXXXX999."),
        String::from("000550     XXXXXABC."),
        String::from("NONSEQ line"),
    ];
    let killed = preprocess_exec85_lines(&raw_lines, "P1", &cfg);
    assert!(killed.contains("INSERTED BEFORE"));
    assert!(killed.contains("REPLACED LINE"));
    assert!(killed.contains("INSERTED AT TAIL"));
    assert!(!killed.contains("DELETED SWITCH Z"));
    assert!(killed.contains("OUTFILE "));
    assert!(killed.contains(r#""PLAIN"."#));

    cfg.kill_deletions = false;
    let commented = preprocess_exec85_lines(
        &[
            String::from("000100Z"),
            String::from("000200Z   BODY"),
            String::from("000300     XXXXX001."),
            String::from("000400     XXXXX002 "),
        ],
        "NONE",
        &cfg,
    );
    assert!(commented.contains("000100*"));
    assert!(commented.contains("000200*Z"));
    assert_eq!(
        nist_program_extra_flags("IC115A"),
        vec![String::from("-Wno-goto-section")]
    );

    exercise_nist_parse_errors()
}

/// Exercises malformed `newcob.val` and `<MOD>.txt` error branches.
fn exercise_nist_parse_errors() -> Result<()> {
    let empty_cfg = Exec85Config {
        kill_deletions: true,
        x_cards: BTreeMap::new(),
        updates: BTreeMap::new(),
    };
    assert!(extract_newcob_from_str("no headers here\u{000a}", &empty_cfg).is_err());

    let synthetic_val = r"*HEADER,COBOL,
*END-OF,
*HEADER,UNKNOWN,XX101A
*END-OF,XX101A
*HEADER,COBOL,Z,SUBRTN,ZSUB
*END-OF,ZSUB
*HEADER,COBOL,ZZ101A,SUBRTN,ZZSUB
*END-OF,ZZSUB
*HEADER,CLBRY,CPY1
000100     COPYLINE.
*HEADER,CLBRY,CPY1
000100     DUP.
";
    let arch = extract_newcob_from_str(synthetic_val, &empty_cfg)?;
    assert_eq!(arch.copybooks.len(), 1);

    for bad_row in [
        "NC101A.CBL 1 1 OK\u{000a}",
        "NC101A.CBL x 1 0 0 0 OK\u{000a}",
        "NC101A.CBL 1 x 0 0 0 OK\u{000a}",
        "NC101A.CBL 1 1 0 x 0 OK\u{000a}",
        "NC101A.CBL 1 1 0 0 x OK\u{000a}",
    ] {
        assert!(parse_module_baseline(bad_row, "NC.txt").is_err());
    }

    let tmp = std::env::temp_dir().join(format!("coblang-nist-err-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    std::fs::write(tmp.join("NC.txt"), "MISSING.CBL 1 1 0 0 0 OK\u{000a}")?;
    assert!(build_nist_manifests_from_archive(&arch, &tmp).is_err());
    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Imports all 12 NIST CCVS-85 modules (`NC`..`DB`), dry-runs all 426 programs, and verifies `NC101A`, `SM101A`, and `IC101A` live on `cobc`!
#[test]
fn import_all_12_nist_modules_and_verify_cobc_oracle() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let newcob_val = root.join(".upstream-cache/nist85/newcob.val");
    let cobol85_dir = root.join(".upstream-cache/gnucobol/tests/cobol85");

    let by_module = import_nist85_suite(&newcob_val, &cobol85_dir)?;
    assert_eq!(by_module.len(), 12);

    let expected_counts = [
        (NistModule::Nc, 95_usize),
        (NistModule::Sm, 17),
        (NistModule::Ic, 25),
        (NistModule::Sq, 85),
        (NistModule::Rl, 35),
        (NistModule::Ix, 42),
        (NistModule::St, 40),
        (NistModule::Sg, 13),
        (NistModule::Ob, 7),
        (NistModule::If, 45),
        (NistModule::Rw, 6),
        (NistModule::Db, 16),
    ];
    let out_dir = std::env::temp_dir().join(format!("coblang-nist-bundles-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir)?;

    let mut all_manifests = Vec::new();
    for (m, exp_len) in expected_counts {
        let list = by_module.get(&m).ok_or_else(|| CoblangError::InvalidCli {
            message: format!("missing module {}", m.code()),
        })?;
        assert_eq!(
            list.len(),
            exp_len,
            "count mismatch for module {}",
            m.code()
        );
        std::fs::write(
            out_dir.join(format!("{}.bundle", m.code())),
            encode_manifest_bundle(list),
        )?;
        all_manifests.extend(list.iter().cloned());
    }
    std::fs::remove_dir_all(&out_dir)?;
    assert_eq!(all_manifests.len(), 426);

    let scaf_cfg = RunnerConfig {
        backend: CompilerBackend::InProcessScaffold,
        sandbox_root: std::env::temp_dir().join("coblang-nist-dryrun"),
        sandbox_retention: SandboxRetention::CleanAlways,
        c_compiler: PathBuf::from("clang"),
        capabilities: HostCapabilities::detect_host(),
        jobs: 8,
    };
    let records = execute_suite_parallel(&all_manifests, &scaf_cfg)?;
    assert_eq!(records.len(), 426);
    for rec in &records {
        assert!(
            matches!(rec.outcome, TestOutcome::XFail | TestOutcome::Skip),
            "unexpected outcome {:?} on {}: {}",
            rec.outcome,
            rec.id,
            rec.detail
        );
    }

    let oracle_cfg = RunnerConfig {
        backend: resolve_oracle_backend(Some("cobc"))?,
        sandbox_root: std::env::temp_dir().join("coblang-nist-oracle"),
        sandbox_retention: SandboxRetention::CleanAlways,
        c_compiler: PathBuf::from("clang"),
        capabilities: HostCapabilities::detect_host(),
        jobs: 1,
    };
    for (slot, m) in [
        (0, NistModule::Nc),
        (1, NistModule::Sm),
        (2, NistModule::Ic),
    ] {
        let target =
            by_module
                .get(&m)
                .and_then(|v| v.first())
                .ok_or_else(|| CoblangError::InvalidCli {
                    message: format!("missing first test in {}", m.code()),
                })?;
        let res = execute_single_test(target, &oracle_cfg, slot)?;
        assert_eq!(
            res.outcome,
            TestOutcome::Pass,
            "oracle failed on {}: {}",
            res.id,
            res.detail
        );
    }

    Ok(())
}
