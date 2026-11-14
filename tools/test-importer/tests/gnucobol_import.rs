//! Unit, full-tree import, and live `cobc` oracle tests for Phase 3 (`GnuCOBOL` importer).

use std::path::{Path, PathBuf};

use coblang_common::{Capability, DiagnosticSeverity, Result, TestOutcome, encode_manifest_bundle};
use coblang_test_importer::gnucobol::{
    decode_quadrigraphs, extract_cobc_expected_diagnostics, import_autotest_file,
    import_gnucobol_tree, parse_m4_calls, shell_like_split,
};
use coblang_test_runner::capabilities::HostCapabilities;
use coblang_test_runner::cli::resolve_oracle_backend;
use coblang_test_runner::engine::{
    CompilerBackend, RunnerConfig, execute_single_test, execute_suite_parallel,
};

/// Tests quadrigraph decoding, M4 lexer/macro expansion, and M4 error branches.
#[test]
fn m4_parser_and_quadrigraph_coverage() -> Result<()> {
    assert_eq!(
        decode_quadrigraphs("@%:@ @S|@ @<:@ @:>@ @&t@ @@"),
        "# $ [ ]  @"
    );
    assert_eq!(
        shell_like_split(r#"export A='1 2' B="3 4" ./prog arg ; echo tail"#),
        vec!["export", "A=1 2", "B=3 4", "./prog", "arg"]
    );

    let sample = r"
# comment to eol
dnl m4 comment to eol
[top level bracket block
spanning two lines]
m4_define([MY_CHECK], [AT_CHECK([$1], [$2], [$3])])
m4_define([INCOMPLETE_DEF])
OTHER_IGNORED(1, 2)
AT_SETUP([] )
MY_CHECK([$COMPILE_ONLY (unbracketed)], 0, [ok])
AT_CLEANUP";
    let calls = parse_m4_calls(sample, "sample.at")?;
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[1].name, "AT_CHECK");

    assert_eq!(
        parse_m4_calls("# comment at eof without newline", "ok.at")?,
        Vec::new()
    );
    for bad_m4 in [
        "AT_SETUP([unclosed bracket",
        "AT_SETUP(unclosed paren",
        "AT_SETUP([a]# trailing comment in arg list at eof",
        "[unclosed top bracket",
        "dnl comment without newline",
    ] {
        if bad_m4.starts_with("dnl") {
            assert_eq!(parse_m4_calls(bad_m4, "ok.at")?, Vec::new());
        } else {
            assert!(parse_m4_calls(bad_m4, "bad.at").is_err());
        }
    }

    let diag_text = r"prog.cob:9: error: bad MOVE
prog.cob:11: warning: trunc
prog.cob:notnum: error: ctx
cobc: error: no line
plain
";
    let diags = extract_cobc_expected_diagnostics(diag_text);
    assert_eq!(diags.len(), 4);
    assert_eq!(diags[0].severity, DiagnosticSeverity::Error);
    assert_eq!(diags[0].line, Some(9));
    assert_eq!(diags[1].severity, DiagnosticSeverity::Warning);
    assert_eq!(diags[2].line, None);
    assert_eq!(diags[3].line, None);
    Ok(())
}

/// Tests synthetic `.at` structural errors and specialized `AT_CHECK` step translations.
#[test]
fn synthetic_autotest_edge_cases() -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("coblang-gnu-syn-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;

    for (name, bad_at) in [
        (
            "nested.at",
            "AT_SETUP([a])\u{000a}AT_SETUP([b])\u{000a}AT_CLEANUP\u{000a}",
        ),
        ("orphan.at", "AT_CLEANUP\u{000a}"),
        ("unclosed.at", "AT_SETUP([a])\u{000a}"),
    ] {
        let p = tmp.join(name);
        std::fs::write(&p, bad_at)?;
        assert!(import_autotest_file(&p, "bad", name, "").is_err());
    }

    let rich_path = tmp.join("rich.at");
    std::fs::write(&rich_path, sample_rich_autotest_script())?;
    let imported = import_autotest_file(&rich_path, "rich", "rich.at", "")?;
    assert_eq!(imported.len(), 2);
    assert!(imported[0].id.ends_with("_case"));
    assert!(
        imported[0]
            .required_capabilities
            .contains(&Capability::BigEndian)
    );
    assert!(
        imported[0]
            .required_capabilities
            .contains(&Capability::CCompiler)
    );

    let mini_gnu = tmp.join("mini_tests");
    std::fs::create_dir_all(mini_gnu.join("testsuite.src"))?;
    std::fs::write(mini_gnu.join("testsuite_manual.at"), "AT_INIT([m])\u{000a}")?;
    std::fs::write(mini_gnu.join("testsuite.src/README.txt"), "notes\u{000a}")?;
    std::fs::write(
        mini_gnu.join("testsuite.src/mini.at"),
        "AT_SETUP([m])\u{000a}AT_CLEANUP\u{000a}",
    )?;
    assert_eq!(import_gnucobol_tree(&mini_gnu)?.total_tests, 1);

    std::fs::remove_dir_all(&tmp)?;
    Ok(())
}

/// Returns a synthetic `.at` script exercising all rare `AT_*` and `cobc`/`$CC` option shapes.
fn sample_rich_autotest_script() -> &'static str {
    r#"
AT_SETUP([---])
AT_DATA()
AT_SKIP_IF()
AT_XFAIL_IF()
AT_CHECK()
AT_SKIP_IF([test "$COB_BIGENDIAN" != "yes"])
AT_SKIP_IF([test "$COB_BIGENDIAN" = "yes"])
AT_SKIP_IF([test "$COB_HAS_ICONV" = "no"])
AT_XFAIL_IF([1 = 1])
AT_DATA([helper.c], [
#include <libcob.h>
int helper(void) { return 0; }
])
AT_DATA([inp.txt], [HELLO_IN
])
AT_DATA([expout], [EXPECTED_OUT
])
AT_DATA([prog.cob], [
       IDENTIFICATION DIVISION.
       PROGRAM-ID. PROG.
       PROCEDURE DIVISION.
           STOP RUN.
])
AT_CHECK([])
AT_CHECK([# comment only])
AT_CHECK([export ONLY_ENV=1])
AT_CHECK([=empty_key])
AT_CHECK([BAD-KEY=1])
AT_CHECK([$CC -Wall -shared -o helper.so helper.c helper.o], [0], [], [])
AT_CHECK([$CC -o], [0], [], [])
AT_CHECK([$COBC -o], [1], [], [])
AT_CHECK([$COBC -I], [1], [], [])
AT_CHECK([$COBC -ext], [1], [], [])
AT_CHECK([$COBC -std], [1], [], [])
AT_CHECK([$COBC -std bad prog.cob;], [1], [], [])
AT_CHECK([$COBC -x -free -fixed -I inc -Iinc2 -ext .cpy -std cobol85 -std=ibm -std=bad -Wall --tlines=0 --unknown-extra -o ./prog prog.cob < inp.txt], [0], [expout], [experr])
AT_CHECK([./prog arg1 < inp.txt], [0], [expout], [ignore])
AT_CHECK([./prog <], [0], [], [])
AT_CHECK([cat])
AT_CHECK([cat ./out.txt], [0], [DONE])
AT_DATA([prog.cob], [OVERWRITTEN])
AT_CLEANUP

AT_SETUP([cob files without AT_CHECK])
AT_DATA([only.cob], [       IDENTIFICATION DIVISION.])
AT_CLEANUP
"#
}

/// Imports all 38 real `GnuCOBOL` `.at` files + 3 standalone `.cob` files, dry-runs all 1,556 tests, and runs `cobc` oracle!
#[test]
fn import_full_gnucobol_suite_and_verify_oracle() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let gnu_root = root.join(".upstream-cache/gnucobol/tests");
    let imported = import_gnucobol_tree(&gnu_root)?;

    assert_eq!(imported.by_file.len(), 39);
    assert_eq!(imported.total_tests, 1556);

    let out_dir = std::env::temp_dir().join(format!("coblang-gnu-bundles-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir)?;
    let mut all_manifests = Vec::with_capacity(imported.total_tests);
    for (stem, list) in &imported.by_file {
        let bundle_path = out_dir.join(format!("{stem}.bundle"));
        std::fs::write(bundle_path, encode_manifest_bundle(list))?;
        all_manifests.extend(list.iter().cloned());
    }
    std::fs::remove_dir_all(&out_dir)?;

    let cfg = RunnerConfig {
        backend: CompilerBackend::InProcessScaffold,
        sandbox_root: std::env::temp_dir().join("coblang-gnu-dryrun"),
        preserve_on_failure: false,
        c_compiler: PathBuf::from("clang"),
        capabilities: HostCapabilities::detect_host(),
        jobs: 8,
    };
    let records = execute_suite_parallel(&all_manifests, &cfg)?;
    assert_eq!(records.len(), 1556);
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
        sandbox_root: std::env::temp_dir().join("coblang-gnu-oracle"),
        preserve_on_failure: false,
        c_compiler: PathBuf::from("clang"),
        capabilities: HostCapabilities::detect_host(),
        jobs: 1,
    };
    let syn_move_first = &imported.by_file["syn_move"][0];
    let run_fund_first = &imported.by_file["run_fundamental"][0];
    assert_eq!(
        execute_single_test(syn_move_first, &oracle_cfg, 0)?.outcome,
        TestOutcome::Pass
    );
    assert_eq!(
        execute_single_test(run_fund_first, &oracle_cfg, 1)?.outcome,
        TestOutcome::Pass
    );
    Ok(())
}
