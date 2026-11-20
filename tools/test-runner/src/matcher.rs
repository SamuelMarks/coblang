//! Compiler diagnostic normalization and stdout/stderr/CCVS-85 output matchers.

use coblang_common::{CoblangError, DiagnosticSeverity, ExpectedDiagnostic, OutputMatcher, Result};
use regex::Regex;

/// Normalized compiler diagnostic extracted from `coblang`, `cobc`, or `gcobol` stderr output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDiagnostic {
    /// Reported source file path or basename.
    pub file: String,
    /// Optional 1-based source line number.
    pub line: Option<u32>,
    /// Normalized severity (`error`, `warning`, or `note`).
    pub severity: DiagnosticSeverity,
    /// Diagnostic message body following the severity prefix.
    pub message: String,
}

/// Compiles a regular expression and maps syntax errors into [`CoblangError::InvalidRegex`].
///
/// # Arguments
///
/// * `pattern` - Regular expression string to compile.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidRegex`] if `pattern` is syntactically invalid.
pub fn compile_regex(pattern: &str) -> Result<Regex> {
    Regex::new(pattern).map_err(|err| CoblangError::InvalidRegex {
        pattern: String::from(pattern),
        reason: err.to_string(),
    })
}

/// Parses compiler diagnostics from `coblang`/Clang, `cobc`, or `gcobol` output lines.
///
/// # Arguments
///
/// * `raw_output` - Combined compiler stderr/stdout text.
/// * `prune_patterns` - Regex patterns whose matching lines are discarded prior to parsing.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidRegex`] if any pattern in `prune_patterns` is invalid.
pub fn parse_compiler_diagnostics(
    raw_output: &str,
    prune_patterns: &[String],
) -> Result<Vec<ParsedDiagnostic>> {
    let mut pruners = Vec::with_capacity(prune_patterns.len());
    for pat in prune_patterns {
        pruners.push(compile_regex(pat)?);
    }

    let mut out = Vec::new();
    for raw_line in raw_output.lines() {
        let line = raw_line.trim_end_matches('\u{000d}');
        if pruners.iter().any(|re| re.is_match(line)) {
            continue;
        }
        if let Some(diag) = parse_single_diagnostic_line(line) {
            out.push(diag);
        }
    }
    Ok(out)
}

/// Attempts to parse a single compiler output line as a structured [`ParsedDiagnostic`].
///
/// # Arguments
///
/// * `line` - Trimmed compiler output line.
#[must_use]
pub fn parse_single_diagnostic_line(line: &str) -> Option<ParsedDiagnostic> {
    for (marker, severity) in [
        (": error:", DiagnosticSeverity::Error),
        (": fatal error:", DiagnosticSeverity::Error),
        (": sorry, unimplemented:", DiagnosticSeverity::Error),
        (": warning:", DiagnosticSeverity::Warning),
        (": note:", DiagnosticSeverity::Note),
    ] {
        if let Some((prefix, msg)) = line.split_once(marker) {
            let (file, line_no) = parse_file_line_prefix(prefix);
            return Some(ParsedDiagnostic {
                file,
                line: line_no,
                severity,
                message: String::from(msg.trim()),
            });
        }
    }
    None
}

/// Splits a `"file:line[:col]"` prefix into `(file, Option<line>)`.
///
/// # Arguments
///
/// * `prefix` - Substring preceding `: error:` / `: warning:` / `: note:`.
#[must_use]
fn parse_file_line_prefix(prefix: &str) -> (String, Option<u32>) {
    let parts: Vec<&str> = prefix.rsplitn(3, ':').collect();
    if let &[p0, p1, p2] = parts.as_slice()
        && p0.chars().all(|c| c.is_ascii_digit())
        && let Ok(ln) = p1.trim().parse::<u32>()
    {
        return (String::from(p2), Some(ln));
    }
    if let &[p0, _, ..] = parts.as_slice()
        && let Ok(ln) = p0.trim().parse::<u32>()
    {
        let head = prefix
            .get(..prefix.len().saturating_sub(p0.len() + 1))
            .unwrap_or("")
            .to_string();
        return (head, Some(ln));
    }
    (String::from(prefix), None)
}

/// Policy controlling whether unmatched compiler errors fail diagnostic verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcessErrorPolicy {
    /// Reject any unmatched error diagnostic (`default`).
    ForbidExcessErrors,
    /// Allow unmatched error diagnostics (`dg-excess-errors`).
    AllowExcessErrors,
}

impl ExcessErrorPolicy {
    /// Converts a manifest `allow_excess_errors` field into an [`ExcessErrorPolicy`].
    ///
    /// # Arguments
    ///
    /// * `allow_excess_errors` - Manifest excess-error flag.
    #[must_use]
    pub const fn from_allow_flag(allow_excess_errors: bool) -> Self {
        if allow_excess_errors {
            Self::AllowExcessErrors
        } else {
            Self::ForbidExcessErrors
        }
    }
}

/// Policy controlling whether `ExpectedDiagnostic::message_pattern` regexes are enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticMessagePolicy {
    /// Match diagnostics by severity, file, and line without requiring wording regex parity.
    IgnoreMessagePattern,
    /// Require `ExpectedDiagnostic::message_pattern` to match the diagnostic message text.
    VerifyMessagePattern,
}

/// Validates compiler output against expected diagnostics, bogus patterns, and excess-error rules.
///
/// # Arguments
///
/// * `raw_output` - Combined compiler stderr/stdout text.
/// * `expected` - Expected diagnostic assertions from the test manifest.
/// * `bogus_patterns` - Forbidden regex patterns (`dg-bogus`).
/// * `prune_patterns` - Pruned output regex patterns (`dg-prune-output`).
/// * `excess_policy` - Policy governing unmatched compiler error diagnostics.
/// * `message_policy` - Policy governing diagnostic message regex enforcement.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidRegex`] if any regex pattern is malformed.
pub fn verify_compile_diagnostics(
    raw_output: &str,
    expected: &[ExpectedDiagnostic],
    bogus_patterns: &[String],
    prune_patterns: &[String],
    excess_policy: ExcessErrorPolicy,
    message_policy: DiagnosticMessagePolicy,
) -> Result<Option<String>> {
    for bogus in bogus_patterns {
        let re = compile_regex(bogus)?;
        if re.is_match(raw_output) {
            return Ok(Some(format!(
                "matched forbidden dg-bogus pattern `{bogus}`"
            )));
        }
    }

    let actual = parse_compiler_diagnostics(raw_output, prune_patterns)?;
    let mut used = vec![false; actual.len()];

    for exp in expected {
        let msg_re = if message_policy == DiagnosticMessagePolicy::VerifyMessagePattern
            && !exp.message_pattern.is_empty()
        {
            Some(compile_regex(&exp.message_pattern)?)
        } else {
            None
        };
        let found = actual.iter().enumerate().position(|(idx, act)| {
            !used.get(idx).copied().unwrap_or(true) && diagnostic_matches(exp, act, msg_re.as_ref())
        });
        let Some(hit) = found else {
            return Ok(Some(format!(
                "missing expected {} at {}:{:?} (`{}`)",
                exp.severity.as_str(),
                exp.file,
                exp.line,
                exp.message_pattern
            )));
        };
        used.get_mut(hit).into_iter().for_each(|u| *u = true);
    }

    if excess_policy == ExcessErrorPolicy::ForbidExcessErrors {
        for (idx, act) in actual.iter().enumerate() {
            if !used.get(idx).copied().unwrap_or(true) && act.severity == DiagnosticSeverity::Error
            {
                return Ok(Some(format!(
                    "unexpected excess error at {}:{:?}: {}",
                    act.file, act.line, act.message
                )));
            }
        }
    }

    Ok(None)
}

/// Checks whether a [`ParsedDiagnostic`] satisfies an [`ExpectedDiagnostic`] specification.
///
/// # Arguments
///
/// * `exp` - Expected diagnostic assertion.
/// * `act` - Actual parsed compiler diagnostic.
/// * `msg_re` - Optional compiled message regex when strict matching is active.
#[must_use]
fn diagnostic_matches(
    exp: &ExpectedDiagnostic,
    act: &ParsedDiagnostic,
    msg_re: Option<&Regex>,
) -> bool {
    if exp.severity != act.severity {
        return false;
    }
    let clean_exp_file = exp.file.trim_start_matches('[');
    if !clean_exp_file.is_empty() && !act.file.ends_with(clean_exp_file) {
        return false;
    }
    if exp.line.is_some() && exp.line != act.line {
        return false;
    }
    if let Some(re) = msg_re
        && !re.is_match(&act.message)
    {
        return false;
    }
    true
}

/// Evaluates an [`OutputMatcher`] against raw process or file output bytes.
///
/// # Arguments
///
/// * `matcher` - Output verification rule.
/// * `actual_bytes` - Raw bytes captured from stdout, stderr, or an output file.
///
/// # Returns
///
/// `Ok(None)` when the output matches, or `Ok(Some(reason))` describing the mismatch.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidRegex`] if a `Regex` matcher contains an invalid pattern.
pub fn verify_output(matcher: &OutputMatcher, actual_bytes: &[u8]) -> Result<Option<String>> {
    let actual = String::from_utf8_lossy(actual_bytes).replace("\u{000d}\u{000a}", "\u{000a}");
    match matcher {
        OutputMatcher::Ignore => Ok(None),
        OutputMatcher::Exact(expected) => {
            let exp_norm = expected.replace("\u{000d}\u{000a}", "\u{000a}");
            if actual == exp_norm {
                Ok(None)
            } else {
                Ok(Some(format!(
                    "exact output mismatch: expected {} bytes, got {} bytes",
                    exp_norm.len(),
                    actual.len()
                )))
            }
        }
        OutputMatcher::Regex(pattern) => {
            let re = compile_regex(pattern)?;
            let actual_dejagnu = actual.replace('\u{0000}', "");
            if re.is_match(&actual_dejagnu) {
                Ok(None)
            } else {
                Ok(Some(format!("output did not match regex `{pattern}`")))
            }
        }
        OutputMatcher::NormalizedLines(expected) => {
            let act_norm = normalize_lines(&actual);
            let exp_norm = normalize_lines(expected);
            if act_norm == exp_norm {
                Ok(None)
            } else {
                Ok(Some(String::from("normalized line output mismatch")))
            }
        }
        OutputMatcher::Ccvs85Report {
            expected_pass,
            expected_deleted,
            expected_inspect,
        } => Ok(verify_ccvs85_report(
            &actual,
            *expected_pass,
            *expected_deleted,
            *expected_inspect,
        )),
    }
}

/// Normalizes text by converting `CRLF` to `LF`, trimming trailing spaces per line, and trimming trailing blank lines.
///
/// # Arguments
///
/// * `text` - Input text to normalize.
#[must_use]
pub fn normalize_lines(text: &str) -> String {
    let replaced = text.replace("\u{000d}\u{000a}", "\u{000a}");
    let mut lines: Vec<&str> = replaced
        .lines()
        .map(|l| l.trim_end_matches([' ', '\u{0009}']))
        .collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\u{000a}")
}

/// Parses NIST CCVS-85 program printout lines and checks subtest counts and zero `*FAIL*` occurrences.
///
/// # Arguments
///
/// * `report` - Standard CCVS-85 printout text.
/// * `expected_pass` - Expected number of passing subtests.
/// * `expected_deleted` - Expected number of deleted subtests.
/// * `expected_inspect` - Expected number of inspect subtests.
#[must_use]
pub fn verify_ccvs85_report(
    report: &str,
    expected_pass: u32,
    expected_deleted: u32,
    expected_inspect: u32,
) -> Option<String> {
    let (mut f_pass, mut f_fail, mut f_deleted, mut f_inspect) = (0_u32, 0_u32, 0_u32, 0_u32);
    let (mut d_pass, mut d_fail, mut d_deleted, mut d_inspect) = (0_u32, 0_u32, 0_u32, 0_u32);
    let mut saw_summary_footer = false;

    for line in report.lines() {
        let trimmed = line.trim();
        let toks: Vec<&str> = trimmed.split_whitespace().collect();
        if let &[t0, "OF", _, "TESTS", "WERE", ..] = toks.as_slice() {
            if let Ok(n) = t0.parse::<u32>() {
                f_pass += n;
                saw_summary_footer = true;
            }
        } else if let &[t0, "TEST(S)", t2, ..] = toks.as_slice() {
            let num = t0.parse::<u32>().unwrap_or(0);
            match t2 {
                "FAILED" => f_fail += num,
                "DELETED" => f_deleted += num,
                "REQUIRE" => f_inspect += num,
                _ => {}
            }
            saw_summary_footer = true;
        } else if let &[t0, "TESTS", "REQUIRE", "VISUAL", ..] = toks.as_slice() {
            if let Ok(n) = t0.parse::<u32>() {
                f_inspect += n;
                saw_summary_footer = true;
            }
        } else if !saw_summary_footer {
            accumulate_ccvs85_detail_line(
                trimmed,
                &mut d_pass,
                &mut d_fail,
                &mut d_deleted,
                &mut d_inspect,
            );
        }
    }

    let fail = f_fail.max(d_fail);
    let (pass, deleted, inspect) = if saw_summary_footer {
        (f_pass, f_deleted, f_inspect)
    } else {
        (d_pass, d_deleted, d_inspect)
    };

    if fail > 0 {
        return Some(format!("CCVS-85 report contained {fail} *FAIL* subtest(s)"));
    }
    if pass != expected_pass || deleted != expected_deleted || inspect != expected_inspect {
        return Some(format!(
            "CCVS-85 counts (pass={pass}, deleted={deleted}, inspect={inspect}) != expected ({expected_pass}, {expected_deleted}, {expected_inspect})"
        ));
    }
    None
}

/// Counts detail-level `PASS` / `*FAIL*` / `DELETED` / `INSPECT` lines when no summary footer was seen yet.
///
/// # Arguments
///
/// * `trimmed` - Trimmed report line.
/// * `pass` - Mutable pass counter.
/// * `fail` - Mutable fail counter.
/// * `deleted` - Mutable deleted counter.
/// * `inspect` - Mutable inspect counter.
fn accumulate_ccvs85_detail_line(
    trimmed: &str,
    pass: &mut u32,
    fail: &mut u32,
    deleted: &mut u32,
    inspect: &mut u32,
) {
    if trimmed.contains("*FAIL*") || trimmed.contains("FAIL*") {
        *fail += 1;
    } else if trimmed.starts_with("PASS ") || trimmed.ends_with(" PASS") {
        *pass += 1;
    } else if trimmed.contains("DELETED") && trimmed.contains("TEST") {
        *deleted += 1;
    } else if trimmed.contains("*INSPECT*") || trimmed.starts_with("INSPECT ") {
        *inspect += 1;
    }
}
