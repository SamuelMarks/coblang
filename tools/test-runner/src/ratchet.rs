//! Expectation baseline ratchet, scoreboard summary formatting, and `llvm-lit` bridge.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use coblang_common::{CoblangError, FeatureTag, Result, SuiteId, TestManifest, TestOutcome};

use crate::engine::TestExecutionRecord;

/// Tab character (`0x09`) separating columns in expectation files.
const TAB: char = '\u{0009}';

/// Line-feed character (`0x0A`).
const LF: char = '\u{000a}';

/// Baseline expectation entry mapping a test ID to its expected [`TestOutcome`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectationEntry {
    /// Expected outcome (`PASS`, `XFAIL`, or `SKIP`).
    pub outcome: TestOutcome,
    /// Short note explaining why `XFAIL` or `SKIP` is expected.
    pub note: String,
}

/// Summary statistics for a group of tests (by suite or by [`FeatureTag`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutcomeCounts {
    /// Total tests in the bucket.
    pub total: usize,
    /// Passing tests (`PASS`).
    pub pass: usize,
    /// Expected failures (`XFAIL`).
    pub xfail: usize,
    /// Unexpected failures (`FAIL`).
    pub fail: usize,
    /// Unexpected passes (`XPASS`).
    pub xpass: usize,
    /// Skipped tests (`SKIP`).
    pub skip: usize,
}

impl OutcomeCounts {
    /// Increments the counter matching `outcome`.
    ///
    /// # Arguments
    ///
    /// * `outcome` - Classified test outcome to record.
    pub fn record(&mut self, outcome: TestOutcome) {
        self.total += 1;
        match outcome {
            TestOutcome::Pass => self.pass += 1,
            TestOutcome::XFail => self.xfail += 1,
            TestOutcome::Fail => self.fail += 1,
            TestOutcome::XPass => self.xpass += 1,
            TestOutcome::Skip => self.skip += 1,
        }
    }
}

/// Serializes an expectation map into deterministic sorted line-oriented text.
///
/// # Arguments
///
/// * `entries` - Map from canonical test ID to [`ExpectationEntry`].
#[must_use]
pub fn encode_expectations(entries: &BTreeMap<String, ExpectationEntry>) -> String {
    let mut out = format!("# coblang expectation baseline v1{LF}");
    for (id, entry) in entries {
        let clean_note = entry.note.replace([TAB, LF, '\u{000d}'], " ");
        let _ = writeln!(out, "{id}{TAB}{}{TAB}{clean_note}", entry.outcome.as_str());
    }
    out
}

/// Builds a fresh baseline expectation map from observed [`TestExecutionRecord`] items.
///
/// # Arguments
///
/// * `records` - Execution results to snapshot as the baseline.
#[must_use]
pub fn snapshot_expectations(
    records: &[TestExecutionRecord],
) -> BTreeMap<String, ExpectationEntry> {
    let mut map = BTreeMap::new();
    for rec in records {
        map.insert(
            rec.id.clone(),
            ExpectationEntry {
                outcome: rec.outcome,
                note: rec.detail.clone(),
            },
        );
    }
    map
}

/// Parses an expectation baseline file created by [`encode_expectations`].
///
/// # Arguments
///
/// * `text` - UTF-8 baseline file contents.
///
/// # Errors
///
/// Returns [`CoblangError::ManifestFormat`] if any non-comment line is malformed.
pub fn decode_expectations(text: &str) -> Result<BTreeMap<String, ExpectationEntry>> {
    let mut map = BTreeMap::new();
    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = raw.splitn(3, TAB).collect();
        let &[c0, c1, c2] =
            <&[&str; 3]>::try_from(cols.as_slice()).map_err(|_| CoblangError::ManifestFormat {
                line: line_no,
                message: String::from("expected 3 tab-separated columns in expectation file"),
            })?;
        let outcome = TestOutcome::parse_token(c1, line_no)?;
        map.insert(
            String::from(c0),
            ExpectationEntry {
                outcome,
                note: String::from(c2),
            },
        );
    }
    Ok(map)
}

/// Enforces the expectation ratchet across `records` and optional baseline `expectations`.
///
/// Fails if any test has outcome `FAIL`, outcome `XPASS`, or regressed from `PASS` in `expectations`
/// to `XFAIL`/`FAIL`, or progressed from `XFAIL` in `expectations` to `PASS` without updating baseline.
///
/// # Arguments
///
/// * `records` - Executed test records.
/// * `expectations` - Optional baseline expectation map to compare against.
///
/// # Errors
///
/// Returns [`CoblangError::RatchetViolation`] when any regression, `FAIL`, or `XPASS` occurs.
pub fn verify_ratchet(
    records: &[TestExecutionRecord],
    expectations: Option<&BTreeMap<String, ExpectationEntry>>,
) -> Result<()> {
    let mut violations = Vec::new();
    for rec in records {
        if rec.outcome == TestOutcome::Fail {
            violations.push(format!("FAIL {}: {}", rec.id, rec.detail));
            continue;
        }
        if rec.outcome == TestOutcome::XPass {
            violations.push(format!("XPASS {}: {}", rec.id, rec.detail));
            continue;
        }
        if let Some(exp_map) = expectations
            && let Some(baseline) = exp_map.get(&rec.id)
        {
            if baseline.outcome == TestOutcome::Pass && rec.outcome != TestOutcome::Pass {
                violations.push(format!(
                    "REGRESSION {}: baseline PASS became {}",
                    rec.id,
                    rec.outcome.as_str()
                ));
            } else if baseline.outcome == TestOutcome::XFail && rec.outcome == TestOutcome::Pass {
                violations.push(format!(
                    "UNRECORDED PROGRESSION {}: baseline XFAIL became PASS (update baseline!)",
                    rec.id
                ));
            }
        }
    }

    if !violations.is_empty() {
        return Err(CoblangError::RatchetViolation {
            summary: violations.join("; "),
        });
    }
    Ok(())
}

/// Renders a Markdown scoreboard broken down by suite and [`FeatureTag`].
///
/// # Arguments
///
/// * `records` - Executed test records to summarize.
#[must_use]
pub fn render_summary_markdown(records: &[TestExecutionRecord]) -> String {
    let mut total = OutcomeCounts::default();
    let mut by_suite: BTreeMap<SuiteId, OutcomeCounts> = BTreeMap::new();
    let mut by_tag: BTreeMap<FeatureTag, OutcomeCounts> = BTreeMap::new();

    for rec in records {
        total.record(rec.outcome);
        by_suite.entry(rec.suite).or_default().record(rec.outcome);
        for &tag in &rec.feature_tags {
            by_tag.entry(tag).or_default().record(rec.outcome);
        }
    }

    let mut md = format!("# `coblang` Conformance Scoreboard{LF}{LF}");
    let _ = writeln!(md, "## Suite Summary{LF}");
    let _ = writeln!(md, "| Suite | Total | PASS | XFAIL | FAIL | XPASS | SKIP |");
    let _ = writeln!(md, "| :--- | ---: | ---: | ---: | ---: | ---: | ---: |");
    for (suite, c) in &by_suite {
        let _ = writeln!(
            md,
            "| `{}` | {} | {} | {} | {} | {} | {} |",
            suite.as_str(),
            c.total,
            c.pass,
            c.xfail,
            c.fail,
            c.xpass,
            c.skip
        );
    }
    let _ = writeln!(
        md,
        "| **TOTAL** | **{}** | **{}** | **{}** | **{}** | **{}** | **{}** |{LF}",
        total.total, total.pass, total.xfail, total.fail, total.xpass, total.skip
    );

    let _ = writeln!(md, "## Capability & Feature Taxonomy{LF}");
    let _ = writeln!(
        md,
        "| Feature Tag | Total | PASS | XFAIL | FAIL | XPASS | SKIP |"
    );
    let _ = writeln!(md, "| :--- | ---: | ---: | ---: | ---: | ---: | ---: |");
    for (tag, c) in &by_tag {
        let _ = writeln!(
            md,
            "| `{}` | {} | {} | {} | {} | {} | {} |",
            tag.as_str(),
            c.total,
            c.pass,
            c.xfail,
            c.fail,
            c.xpass,
            c.skip
        );
    }
    md
}

/// Renders a deterministic JSON scoreboard broken down by suite and [`FeatureTag`].
///
/// # Arguments
///
/// * `records` - Executed test records to summarize.
#[must_use]
pub fn render_summary_json(records: &[TestExecutionRecord]) -> String {
    let mut total = OutcomeCounts::default();
    let mut by_suite: BTreeMap<SuiteId, OutcomeCounts> = BTreeMap::new();
    let mut by_tag: BTreeMap<FeatureTag, OutcomeCounts> = BTreeMap::new();
    for rec in records {
        total.record(rec.outcome);
        by_suite.entry(rec.suite).or_default().record(rec.outcome);
        for &tag in &rec.feature_tags {
            by_tag.entry(tag).or_default().record(rec.outcome);
        }
    }

    let fmt_counts = |c: &OutcomeCounts| -> String {
        format!(
            r#"{{"total":{},"pass":{},"xfail":{},"fail":{},"xpass":{},"skip":{}}}"#,
            c.total, c.pass, c.xfail, c.fail, c.xpass, c.skip
        )
    };

    let suites_json: Vec<String> = by_suite
        .iter()
        .map(|(s, c)| format!(r#""{}":{}"#, s.as_str(), fmt_counts(c)))
        .collect();
    let tags_json: Vec<String> = by_tag
        .iter()
        .map(|(t, c)| format!(r#""{}":{}"#, t.as_str(), fmt_counts(c)))
        .collect();

    format!(
        r#"{{"total":{},"suites":{{{}}},"tags":{{{}}}}}"#,
        fmt_counts(&total),
        suites_json.join(","),
        tags_json.join(",")
    )
}

/// Emits `.lit.test` wrapper files enabling `llvm-lit` to discover and run every manifest entry.
///
/// # Arguments
///
/// * `manifests` - Slice of manifests in `bundle_path`.
/// * `bundle_path` - Path to the manifest bundle passed to `coblang-test run-one`.
/// * `out_dir` - Directory into which `.lit.test` files are written.
///
/// # Errors
///
/// Returns [`CoblangError::Io`] if writing any `.lit.test` file fails.
pub fn emit_lit_wrappers(
    manifests: &[TestManifest],
    bundle_path: &Path,
    out_dir: &Path,
) -> Result<usize> {
    std::fs::create_dir_all(out_dir)?;
    for m in manifests {
        let rel = format!("{}.lit.test", m.id);
        if let Some((dir, _)) = rel.rsplit_once('/') {
            std::fs::create_dir_all(out_dir.join(dir))?;
        }
        let dest = out_dir.join(&rel);
        let mut content = format!(
            "# RUN: %coblang_test run-one --bundle '{}' --id '{}'{LF}",
            bundle_path.display(),
            m.id
        );
        if m.xfail_reason.is_some() {
            let _ = writeln!(content, "# XFAIL: *");
        }
        if m.skip_reason.is_some() {
            let _ = writeln!(content, "# UNSUPPORTED: true");
        }
        std::fs::write(dest, content)?;
    }
    Ok(manifests.len())
}
