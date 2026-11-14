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
    if parts.len() == 3
        && parts[0].chars().all(|c| c.is_ascii_digit())
        && let Ok(ln) = parts[1].trim().parse::<u32>()
    {
        return (String::from(parts[2]), Some(ln));
    }
    if parts.len() >= 2
        && let Ok(ln) = parts[0].trim().parse::<u32>()
    {
        let head = prefix[..prefix.len() - parts[0].len() - 1].to_string();
        return (head, Some(ln));
    }
    (String::from(prefix), None)
}

/// Validates compiler output against expected diagnostics, bogus patterns, and excess-error rules.
///
/// # Arguments
///
/// * `raw_output` - Combined compiler stderr/stdout text.
/// * `expected` - Expected diagnostic assertions from the test manifest.
/// * `bogus_patterns` - Forbidden regex patterns (`dg-bogus`).
/// * `prune_patterns` - Pruned output regex patterns (`dg-prune-output`).
/// * `allow_excess_errors` - Whether extra unmatched errors are permitted (`dg-excess-errors`).
/// * `strict_message_match` - Whether diagnostic text must also match `message_pattern`.
///
/// # Errors
///
/// Returns [`CoblangError::InvalidRegex`] if any regex pattern is malformed.
pub fn verify_compile_diagnostics(
    raw_output: &str,
    expected: &[ExpectedDiagnostic],
    bogus_patterns: &[String],
    prune_patterns: &[String],
    allow_excess_errors: bool,
    strict_message_match: bool,
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
        let msg_re = if strict_message_match && !exp.message_pattern.is_empty() {
            Some(compile_regex(&exp.message_pattern)?)
        } else {
            None
        };
        let found = actual
            .iter()
            .enumerate()
            .position(|(idx, act)| !used[idx] && diagnostic_matches(exp, act, msg_re.as_ref()));
        let Some(hit) = found else {
            return Ok(Some(format!(
                "missing expected {} at {}:{:?} (`{}`)",
                exp.severity.as_str(),
                exp.file,
                exp.line,
                exp.message_pattern
            )));
        };
        used[hit] = true;
    }

    if !allow_excess_errors {
        for (idx, act) in actual.iter().enumerate() {
            if !used[idx] && act.severity == DiagnosticSeverity::Error {
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
    if !exp.file.is_empty() && !act.file.ends_with(&exp.file) {
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
            if re.is_match(&actual) {
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
    let (mut pass, mut fail, mut deleted, mut inspect) = (0_u32, 0_u32, 0_u32, 0_u32);
    let mut saw_summary_footer = false;

    for line in report.lines() {
        let trimmed = line.trim();
        let toks: Vec<&str> = trimmed.split_whitespace().collect();
        if toks.len() >= 5 && toks[1] == "OF" && toks[3] == "TESTS" && toks[4] == "WERE" {
            if let Ok(n) = toks[0].parse::<u32>() {
                pass += n;
                saw_summary_footer = true;
            }
        } else if toks.len() >= 3 && toks[1] == "TEST(S)" {
            let num = toks[0].parse::<u32>().unwrap_or(0);
            match toks[2] {
                "FAILED" => fail += num,
                "DELETED" => deleted += num,
                "REQUIRE" => inspect += num,
                _ => {}
            }
            saw_summary_footer = true;
        } else if toks.len() >= 5
            && toks[1] == "TESTS"
            && toks[2] == "REQUIRE"
            && toks[3] == "VISUAL"
        {
            if let Ok(n) = toks[0].parse::<u32>() {
                inspect += n;
                saw_summary_footer = true;
            }
        } else if !saw_summary_footer {
            accumulate_ccvs85_detail_line(
                trimmed,
                &mut pass,
                &mut fail,
                &mut deleted,
                &mut inspect,
            );
        }
    }

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
    if trimmed.contains("*FAIL*") {
        *fail += 1;
    } else if trimmed.starts_with("PASS ") || trimmed.ends_with(" PASS") {
        *pass += 1;
    } else if trimmed.contains("DELETED") && trimmed.contains("TEST") {
        *deleted += 1;
    } else if trimmed.contains("*INSPECT*") || trimmed.starts_with("INSPECT ") {
        *inspect += 1;
    }
}
