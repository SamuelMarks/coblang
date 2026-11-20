//! GNU Autotest (`.at`) / M4 parser and `GnuCOBOL` (`tests/testsuite.src/`) importer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use coblang_common::{
    Capability, CoblangError, CompilationMode, CompileCSpec, CompileCobolSpec, DiagnosticSeverity,
    Dialect, ExpectedDiagnostic, OptimizationLevel, OutputMatcher, Provenance, Result,
    RunBinarySpec, SourceFormat, SuiteId, TestManifest, TestStep, TextEncoding, WorkspaceFile,
};

use crate::taxonomy::infer_feature_tags;

/// Upstream `GnuCOBOL` mirror URL recorded in [`Provenance`].
pub const GNUCOBOL_UPSTREAM_REPO: &str = "https://github.com/OCamlPro/gnucobol";

/// Pinned `GnuCOBOL` Git commit SHA-1 recorded in [`Provenance`].
pub const GNUCOBOL_PINNED_COMMIT: &str = "02294c68b67eef1e407f3567b50173593dd160af";

/// Imported `GnuCOBOL` bundles keyed by `.at` stem (plus `"standalone_cob"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GnuCobolImportResult {
    /// Map from suite file stem (e.g., `"syn_move"`, `"run_fundamental"`) to its [`TestManifest`] list.
    pub by_file: BTreeMap<String, Vec<TestManifest>>,
    /// Total number of `AT_SETUP` groups + standalone `.cob` programs imported.
    pub total_tests: usize,
}

/// Top-level M4 macro invocation parsed from an Autotest `.at` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct M4MacroCall {
    /// Macro identifier (e.g., `"AT_SETUP"`, `"AT_DATA"`, `"AT_CHECK"`).
    pub name: String,
    /// Unquoted and quadrigraph-decoded positional arguments.
    pub args: Vec<String>,
    /// One-based starting line number in the `.at` file.
    pub line: u32,
}

/// Decodes Autoconf quadrigraphs (`@%:@`, `@S|@`, `@<:@`, `@:>@`, `@&t@`, `@@`) in `text`.
///
/// # Arguments
///
/// * `text` - Raw Autotest string potentially containing quadrigraph sequences.
#[must_use]
pub fn decode_quadrigraphs(text: &str) -> String {
    text.replace("@%:@", "#")
        .replace("@S|@", "$")
        .replace("@<:@", "[")
        .replace("@:>@", "]")
        .replace("@&t@", "")
        .replace("@@", "@")
}

/// Parses all M4 macro calls of interest from an Autotest `.at` source text.
///
/// # Arguments
///
/// * `source` - Full UTF-8 text of the `.at` file.
/// * `source_file` - Filename used for [`CoblangError::ImportParse`] diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if nested `[...]` or `(...)` delimiters are unbalanced.
pub fn parse_m4_calls(source: &str, source_file: &str) -> Result<Vec<M4MacroCall>> {
    let bytes = source.as_bytes();
    let mut calls = Vec::new();
    let mut macros: BTreeMap<String, String> = BTreeMap::new();
    let mut i = 0;
    let mut line = 1_u32;

    while let Some(&b) = bytes.get(i) {
        if b == 0x0A {
            line += 1;
            i += 1;
        } else if b == b'#' {
            while bytes.get(i).is_some_and(|&c| c != 0x0A) {
                i += 1;
            }
        } else if b == b'[' {
            let (_, next_i, next_ln) = read_m4_bracketed(source, i, line, source_file)?;
            i = next_i;
            line = next_ln;
        } else if is_ident_start(b) {
            let start = i;
            while bytes.get(i).copied().is_some_and(is_ident_continue) {
                i += 1;
            }
            let ident = &source[start..i];
            if ident == "dnl" {
                while bytes.get(i).is_some_and(|&c| c != 0x0A) {
                    i += 1;
                }
            } else if bytes.get(i) == Some(&b'(') && is_tracked_macro(ident, &macros) {
                let call_line = line;
                let (args, next_i, next_ln) = read_m4_arg_list(source, i, line, source_file)?;
                i = next_i;
                line = next_ln;
                handle_parsed_call(ident, args, call_line, source_file, &mut macros, &mut calls)?;
            } else if ident == "AT_CLEANUP" {
                calls.push(M4MacroCall {
                    name: String::from("AT_CLEANUP"),
                    args: Vec::new(),
                    line,
                });
            }
        } else {
            i += 1;
        }
    }
    Ok(calls)
}

/// Checks whether `ident` is an Autotest macro or user-defined `m4_define` macro to parse.
///
/// # Arguments
///
/// * `ident` - Candidate identifier preceding `(`.
/// * `macros` - Currently registered `m4_define` macros.
#[must_use]
fn is_tracked_macro(ident: &str, macros: &BTreeMap<String, String>) -> bool {
    ident.starts_with("AT_") || ident == "m4_define" || macros.contains_key(ident)
}

/// Records `m4_define`, expands user-defined macros (`MANUAL_CHECK`), or pushes [`M4MacroCall`].
///
/// # Arguments
///
/// * `ident` - Invoked macro name.
/// * `args` - Decoded argument list.
/// * `call_line` - Starting line number.
/// * `source_file` - Filename for error reporting.
/// * `macros` - Active `m4_define` dictionary.
/// * `calls` - Output call vector.
///
/// # Errors
///
/// Propagates parse errors from expanded macro bodies.
fn handle_parsed_call(
    ident: &str,
    args: Vec<String>,
    call_line: u32,
    source_file: &str,
    macros: &mut BTreeMap<String, String>,
    calls: &mut Vec<M4MacroCall>,
) -> Result<()> {
    if ident == "m4_define" {
        if let (Some(name), Some(body)) = (args.first(), args.get(1)) {
            macros.insert(name.clone(), body.clone());
        }
        return Ok(());
    }
    if let Some(template) = macros.get(ident).cloned() {
        let expanded = expand_positional_params(&template, &args);
        let mut nested = parse_m4_calls(&expanded, source_file)?;
        for c in &mut nested {
            c.line = call_line;
        }
        calls.extend(nested);
        return Ok(());
    }
    calls.push(M4MacroCall {
        name: String::from(ident),
        args,
        line: call_line,
    });
    Ok(())
}

/// Substitutes `$1`..`$9` positional M4 arguments inside `template`, wrapping in `[...]` safely.
///
/// # Arguments
///
/// * `template` - Macro body from `m4_define`.
/// * `args` - Supplied call arguments.
#[must_use]
fn expand_positional_params(template: &str, args: &[String]) -> String {
    let mut out = String::from(template);
    for idx in (1..=9).rev() {
        let needle = format!("${idx}");
        let raw_val = args.get(idx - 1).map_or("", String::as_str);
        let safe_val = raw_val.replace('[', "@<:@").replace(']', "@:>@");
        out = out.replace(&needle, &safe_val);
    }
    out
}

/// Reads a balanced `[...]` M4 quoted block starting at `open_idx` (`source.as_bytes()[open_idx] == b'['`).
///
/// # Arguments
///
/// * `source` - Full `.at` source string.
/// * `open_idx` - Byte offset of the opening `[`.
/// * `start_line` - Line number at `open_idx`.
/// * `source_file` - Filename for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if EOF is reached before the matching `]`.
fn read_m4_bracketed(
    source: &str,
    open_idx: usize,
    start_line: u32,
    source_file: &str,
) -> Result<(String, usize, u32)> {
    let bytes = source.as_bytes();
    let mut depth = 1_usize;
    let mut i = open_idx + 1;
    let mut line = start_line;
    while let Some(&b) = bytes.get(i) {
        if b == 0x0A {
            line += 1;
        } else if b == b'[' {
            depth += 1;
        } else if b == b']' {
            depth -= 1;
            if depth == 0 {
                let inner = decode_quadrigraphs(&source[open_idx + 1..i]);
                return Ok((inner, i + 1, line));
            }
        }
        i += 1;
    }
    Err(CoblangError::ImportParse {
        source_file: String::from(source_file),
        line: start_line as usize,
        message: String::from("unterminated M4 `[` block"),
    })
}

/// Quoting style observed while scanning a single M4 macro argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum M4ArgQuoting {
    /// Unquoted argument text requiring trim and quadrigraph decoding.
    Unbracketed,
    /// Argument contained `[...]` M4 quoting preserving verbatim whitespace.
    Bracketed,
}

/// Reads a `(arg1, arg2, ...)` M4 argument list handling nested `[...]` and `(...)`.
///
/// # Arguments
///
/// * `source` - Full `.at` source string.
/// * `open_paren` - Byte offset of the opening `(`.
/// * `start_line` - Line number at `open_paren`.
/// * `source_file` - Filename for error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if `(` or an inner `[` is unterminated.
fn read_m4_arg_list(
    source: &str,
    open_paren: usize,
    start_line: u32,
    source_file: &str,
) -> Result<(Vec<String>, usize, u32)> {
    let bytes = source.as_bytes();
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut paren_depth = 1_usize;
    let mut quoting = M4ArgQuoting::Unbracketed;
    let mut i = open_paren + 1;
    let mut line = start_line;

    while let Some(&b) = bytes.get(i) {
        if b == b'[' {
            let (inner, next_i, next_ln) = read_m4_bracketed(source, i, line, source_file)?;
            cur.push_str(&inner);
            quoting = M4ArgQuoting::Bracketed;
            i = next_i;
            line = next_ln;
            continue;
        }
        if b == b'#' {
            while bytes.get(i).is_some_and(|&c| c != 0x0A) {
                i += 1;
            }
            continue;
        }
        if b == 0x0A {
            line += 1;
        }
        if b == b'(' {
            paren_depth += 1;
            cur.push('(');
        } else if b == b')' {
            paren_depth -= 1;
            if paren_depth == 0 {
                if !args.is_empty() || !cur.is_empty() || quoting == M4ArgQuoting::Bracketed {
                    push_m4_arg(&mut args, &mut cur, quoting);
                }
                return Ok((args, i + 1, line));
            }
            cur.push(')');
        } else if b == b',' && paren_depth == 1 {
            push_m4_arg(&mut args, &mut cur, quoting);
            quoting = M4ArgQuoting::Unbracketed;
        } else if !(cur.is_empty()
            && quoting == M4ArgQuoting::Unbracketed
            && b.is_ascii_whitespace())
        {
            cur.push(char::from(b));
        }
        i += 1;
    }

    Err(CoblangError::ImportParse {
        source_file: String::from(source_file),
        line: start_line as usize,
        message: String::from("unterminated M4 `(` argument list"),
    })
}

/// Finalizes one argument in an M4 argument list, preserving exact content when `[...]` was used.
///
/// # Arguments
///
/// * `args` - Output argument list.
/// * `cur` - Accumulated current argument text.
/// * `quoting` - Quoting mode observed for the argument.
fn push_m4_arg(args: &mut Vec<String>, cur: &mut String, quoting: M4ArgQuoting) {
    let raw = std::mem::take(cur);
    match quoting {
        M4ArgQuoting::Bracketed => args.push(raw),
        M4ArgQuoting::Unbracketed => args.push(decode_quadrigraphs(raw.trim())),
    }
}

/// Checks if `b` can start an M4 macro identifier.
///
/// # Arguments
///
/// * `b` - ASCII byte to classify.
const fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Checks if `b` can continue an M4 macro identifier.
///
/// # Arguments
///
/// * `b` - ASCII byte to classify.
const fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Extracts `cobc` expected error and warning diagnostics from an `AT_CHECK` stderr expectation.
///
/// # Arguments
///
/// * `stderr_text` - Expected stderr string from `AT_CHECK`.
#[must_use]
pub fn extract_cobc_expected_diagnostics(stderr_text: &str) -> Vec<ExpectedDiagnostic> {
    let mut diags = Vec::new();
    for line in stderr_text.lines() {
        if let Some((prefix, msg)) = line.split_once(": error:") {
            let (file, ln) = split_cobc_file_line(prefix);
            diags.push(ExpectedDiagnostic {
                file,
                line: ln,
                severity: DiagnosticSeverity::Error,
                message_pattern: String::from(msg.trim()),
            });
        } else if let Some((prefix, msg)) = line.split_once(": warning:") {
            let (file, ln) = split_cobc_file_line(prefix);
            diags.push(ExpectedDiagnostic {
                file,
                line: ln,
                severity: DiagnosticSeverity::Warning,
                message_pattern: String::from(msg.trim()),
            });
        }
    }
    diags
}

/// Splits a `cobc` `"prog.cob:12"` prefix into `(file, Option<line>)`.
///
/// # Arguments
///
/// * `prefix` - Substring before `: error:` or `: warning:`.
#[must_use]
fn split_cobc_file_line(prefix: &str) -> (String, Option<u32>) {
    if let Some((file, ln_str)) = prefix.rsplit_once(':')
        && let Ok(ln) = ln_str.trim().parse::<u32>()
    {
        return (String::from(file.trim()), Some(ln));
    }
    (String::from(prefix.trim()), None)
}

/// Converts a single `.at` file (with optional `prelude` macro text like `testsuite_manual.at`) into [`TestManifest`] entries.
///
/// # Arguments
///
/// * `at_path` - Path to the `.at` file on disk.
/// * `file_stem` - Canonical stem name (e.g., `"syn_move"`).
/// * `upstream_rel` - Upstream repo-relative path for [`Provenance`].
/// * `prelude` - Optional M4 macro prelude prepended before parsing (e.g., `MANUAL_CHECK`).
///
/// # Errors
///
/// Propagates I/O or M4 syntax errors.
pub fn import_autotest_file(
    at_path: &Path,
    file_stem: &str,
    upstream_rel: &str,
    prelude: &str,
) -> Result<Vec<TestManifest>> {
    let body = std::fs::read_to_string(at_path)?;
    let full = if prelude.is_empty() {
        body
    } else {
        format!("{prelude}\u{000a}{body}")
    };
    let calls = parse_m4_calls(&full, upstream_rel)?;
    build_manifests_from_calls(&calls, file_stem, upstream_rel)
}

/// Mutable builder state for an active `AT_SETUP ... AT_CLEANUP` group.
struct SetupGroupBuilder {
    /// 1-based ordinal within the `.at` file.
    ordinal: usize,
    /// Title argument from `AT_SETUP([title])`.
    title: String,
    /// Starting line in `.at` file.
    line: u32,
    /// Initial files created before any `AT_CHECK`.
    files: Vec<WorkspaceFile>,
    /// Set of filenames currently materialized in the sandbox.
    known_files: BTreeSet<String>,
    /// Map of `expout`/`experr` contents written via `AT_DATA`.
    named_outputs: BTreeMap<String, String>,
    /// Persistent `export KEY=VAL` bindings active across subsequent commands in this group.
    exported_env: Vec<(String, String)>,
    /// Ordered execution steps.
    steps: Vec<TestStep>,
    /// Required capabilities inferred from `AT_SKIP_IF` / manual screen macros.
    capabilities: BTreeSet<Capability>,
    /// Optional `XFAIL` reason.
    xfail_reason: Option<String>,
    /// Optional `SKIP` reason.
    skip_reason: Option<String>,
    /// Accumulated COBOL source text for taxonomy tagging.
    cobol_text_upper: String,
}

/// Groups a linear [`M4MacroCall`] stream into `AT_SETUP ... AT_CLEANUP` [`TestManifest`] items.
///
/// # Arguments
///
/// * `calls` - Parsed M4 macro invocations.
/// * `file_stem` - Stem of the `.at` file.
/// * `upstream_rel` - Upstream repo-relative path.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if `AT_SETUP` blocks are nested or unterminated.
fn build_manifests_from_calls(
    calls: &[M4MacroCall],
    file_stem: &str,
    upstream_rel: &str,
) -> Result<Vec<TestManifest>> {
    let mut out = Vec::new();
    let mut active: Option<SetupGroupBuilder> = None;
    let mut ordinal = 0_usize;

    for call in calls {
        if call.name == "AT_SETUP" {
            if active.is_some() {
                return Err(CoblangError::ImportParse {
                    source_file: String::from(upstream_rel),
                    line: call.line as usize,
                    message: String::from("nested AT_SETUP before AT_CLEANUP"),
                });
            }
            ordinal += 1;
            let mut caps = BTreeSet::new();
            if file_stem == "run_manual_screen" {
                caps.insert(Capability::CursesPty);
            }
            active = Some(SetupGroupBuilder {
                ordinal,
                title: call.args.first().cloned().unwrap_or_default(),
                line: call.line,
                files: Vec::new(),
                known_files: BTreeSet::new(),
                named_outputs: BTreeMap::new(),
                exported_env: Vec::new(),
                steps: Vec::new(),
                capabilities: caps,
                xfail_reason: None,
                skip_reason: None,
                cobol_text_upper: String::new(),
            });
            continue;
        }

        if call.name == "AT_CLEANUP" {
            let Some(group) = active.take() else {
                return Err(CoblangError::ImportParse {
                    source_file: String::from(upstream_rel),
                    line: call.line as usize,
                    message: String::from("AT_CLEANUP without matching AT_SETUP"),
                });
            };
            out.push(finalize_setup_group(group, file_stem, upstream_rel));
            continue;
        }

        if let Some(group) = active.as_mut() {
            apply_group_call(group, call);
        }
    }

    if let Some(unclosed) = active {
        return Err(CoblangError::ImportParse {
            source_file: String::from(upstream_rel),
            line: unclosed.line as usize,
            message: String::from("unterminated AT_SETUP at end of file"),
        });
    }

    Ok(out)
}

/// Applies `AT_DATA`, `AT_SKIP_IF`, `AT_XFAIL_IF`, or `AT_CHECK` inside an active `AT_SETUP` block.
///
/// # Arguments
///
/// * `group` - Active group builder.
/// * `call` - Macro call inside the group.
fn apply_group_call(group: &mut SetupGroupBuilder, call: &M4MacroCall) {
    match call.name.as_str() {
        "AT_DATA" => {
            if let (Some(fname), Some(body)) = (call.args.first(), call.args.get(1)) {
                let clean_name = fname.trim().trim_start_matches("./").to_string();
                if matches!(clean_name.as_str(), "expout" | "experr") {
                    group.named_outputs.insert(clean_name.clone(), body.clone());
                }
                if has_cobol_ext(&clean_name) {
                    group.cobol_text_upper.push_str(&body.to_uppercase());
                }
                if Path::new(&clean_name)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("c"))
                    && body.contains("libcob.h")
                {
                    group.xfail_reason = Some(String::from("uses internal libcob.h C API"));
                }
                let wf = WorkspaceFile {
                    relative_path: clean_name.clone(),
                    contents: body.as_bytes().to_vec(),
                };
                group.known_files.insert(clean_name);
                if group.steps.is_empty() {
                    group.files.push(wf);
                } else {
                    group.steps.push(TestStep::WriteFile(wf));
                }
            }
        }
        "AT_SKIP_IF" => {
            if let Some(pred) = call.args.first() {
                apply_skip_predicate(group, pred);
            }
        }
        "AT_XFAIL_IF" => {
            if let Some(pred) = call.args.first()
                && (pred.trim() == "true" || pred.contains("1 = 1"))
            {
                group.xfail_reason = Some(String::from("AT_XFAIL_IF([true])"));
            }
        }
        "AT_CHECK" => apply_at_check(group, &call.args),
        _ => {}
    }
}

/// Maps `AT_SKIP_IF` shell predicates onto declarative [`Capability`] requirements or `skip_reason`.
///
/// # Arguments
///
/// * `group` - Active group builder.
/// * `pred` - Shell predicate inside `AT_SKIP_IF([...])`.
fn apply_skip_predicate(group: &mut SetupGroupBuilder, pred: &str) {
    let trimmed = pred.trim();
    if trimmed == "true" {
        group.skip_reason = Some(String::from("AT_SKIP_IF([true])"));
    } else if trimmed.contains("COB_HAS_CURSES") {
        group.capabilities.insert(Capability::CursesPty);
    } else if trimmed.contains("COB_HAS_ISAM") {
        group.capabilities.insert(Capability::Isam);
    } else if trimmed.contains("COB_HAS_XML2") {
        group.capabilities.insert(Capability::Xml);
    } else if trimmed.contains("COB_HAS_JSON") {
        group.capabilities.insert(Capability::Json);
    } else if trimmed.contains("COB_BIGENDIAN") && trimmed.contains(r#"!= "yes""#) {
        group.capabilities.insert(Capability::BigEndian);
    } else if trimmed.contains("COB_HAS_ICONV") {
        group
            .capabilities
            .insert(Capability::IconvCodepage(String::from("UTF-8")));
    }
}

/// Translates an `AT_CHECK([cmd], [status], [stdout], [stderr])` call into declarative [`TestStep`]s.
///
/// # Arguments
///
/// * `group` - Active group builder.
/// * `args` - Arguments passed to `AT_CHECK`.
fn apply_at_check(group: &mut SetupGroupBuilder, args: &[String]) {
    let Some(cmd_raw) = args.first() else {
        return;
    };
    let exit_str = args.get(1).map_or("0", |s| s.trim());
    let expected_code = exit_str.parse::<i32>().unwrap_or(0);
    let stdout_arg = args.get(2).map_or("", String::as_str);
    let stderr_arg = args.get(3).map_or("", String::as_str);

    let mut joined_lines: Vec<String> = Vec::new();
    let mut pending = String::new();
    for raw_line in cmd_raw.lines() {
        let trimmed = raw_line.trim();
        if pending.is_empty() && (trimmed.is_empty() || trimmed.starts_with('#')) {
            continue;
        }
        if let Some(prefix) = trimmed.strip_suffix('\\') {
            pending.push_str(prefix);
            pending.push(' ');
        } else {
            pending.push_str(trimmed);
            joined_lines.push(std::mem::take(&mut pending));
        }
    }
    if !pending.trim().is_empty() {
        joined_lines.push(pending);
    }

    for joined in &joined_lines {
        for subcmd in joined.split(';').flat_map(|s| s.split("&&")) {
            let clean = subcmd.trim();
            if !clean.is_empty() {
                translate_shell_line(group, clean, expected_code, stdout_arg, stderr_arg);
            }
        }
    }
}

/// Translates a single non-comment command line inside `AT_CHECK`.
///
/// # Arguments
///
/// * `group` - Active group builder.
/// * `line` - Trimmed shell command line.
/// * `expected_code` - Expected exit status from `AT_CHECK`.
/// * `stdout_arg` - Expected stdout specification from `AT_CHECK`.
/// * `stderr_arg` - Expected stderr specification from `AT_CHECK`.
fn translate_shell_line(
    group: &mut SetupGroupBuilder,
    line: &str,
    expected_code: i32,
    stdout_arg: &str,
    stderr_arg: &str,
) {
    let (line_env, tokens) = split_env_and_tokens(line);
    let Some(head) = tokens.first().map(String::as_str) else {
        for (k, v) in line_env {
            if let Some(slot) = group.exported_env.iter_mut().find(|(ek, _)| ek == &k) {
                slot.1 = v;
            } else {
                group.exported_env.push((k, v));
            }
        }
        return;
    };
    let mut env_vars = group.exported_env.clone();
    for (k, v) in line_env {
        if let Some(slot) = env_vars.iter_mut().find(|(ek, _)| ek == &k) {
            slot.1 = v;
        } else {
            env_vars.push((k, v));
        }
    }

    if matches!(
        head,
        "$COMPILE"
            | "$COMPILE_ONLY"
            | "$COMPILE_MODULE"
            | "$COMPILE_LISTING"
            | "$COMPILE_LISTING0"
            | "$COBC"
    ) {
        if head.starts_with("$COMPILE_LISTING") {
            group.xfail_reason = Some(String::from("cobc print-listing format check"));
        }
        if let Some(step) = build_cobc_step(
            group,
            head,
            tokens.get(1..).unwrap_or(&[]),
            env_vars,
            expected_code,
            stderr_arg,
        ) {
            group.steps.push(step);
        }
    } else if matches!(head, "$CC" | "$COMPILE_C") {
        if let Some(step) = build_cc_step(tokens.get(1..).unwrap_or(&[])) {
            group.capabilities.insert(Capability::CCompiler);
            group.steps.push(step);
        }
    } else if matches!(head, "$COBCRUN_DIRECT" | "$COBCRUN" | "$RUN_PROG_MANUAL")
        || head.starts_with("./")
    {
        if head == "$RUN_PROG_MANUAL" {
            group.capabilities.insert(Capability::CursesPty);
        }
        let step = build_run_step(
            group,
            head,
            &tokens,
            env_vars,
            expected_code,
            stdout_arg,
            stderr_arg,
        );
        group.steps.push(step);
    } else if head == "cat"
        && let Some(target_file) = tokens.get(1)
        && let Some(TestStep::RunBinary(prev_run)) = group.steps.last_mut()
    {
        let matcher = resolve_at_output(stdout_arg, &group.named_outputs);
        prev_run
            .expected_files
            .push((target_file.trim_start_matches("./").to_string(), matcher));
    } else if head == "$DIFF"
        && expected_code == 0
        && let (Some(ref_arg), Some(target_arg)) = (tokens.get(1), tokens.get(2))
    {
        append_diff_file_expectation(group, ref_arg, target_arg);
    }
}

/// Attaches a `$DIFF ref target` file expectation onto the most recent `RunBinary` step in `group`.
///
/// # Arguments
///
/// * `group` - Active group builder.
/// * `ref_arg` - Reference filename argument to `$DIFF`.
/// * `target_arg` - Produced output filename argument to `$DIFF`.
fn append_diff_file_expectation(group: &mut SetupGroupBuilder, ref_arg: &str, target_arg: &str) {
    let ref_clean = ref_arg.trim_start_matches("./");
    let target_clean = target_arg.trim_start_matches("./").to_string();
    let ref_bytes = group
        .steps
        .iter()
        .rev()
        .find_map(|st| match st {
            TestStep::WriteFile(wf) if wf.relative_path == ref_clean => Some(wf.contents.clone()),
            _ => None,
        })
        .or_else(|| {
            group
                .files
                .iter()
                .find(|wf| wf.relative_path == ref_clean)
                .map(|wf| wf.contents.clone())
        });
    if let Some(expected_bytes) = ref_bytes
        && let Some(prev_run) = group.steps.iter_mut().rev().find_map(|st| match st {
            TestStep::RunBinary(rb) => Some(rb),
            _ => None,
        })
    {
        prev_run.expected_files.push((
            target_clean,
            OutputMatcher::Exact(String::from_utf8_lossy(&expected_bytes).into_owned()),
        ));
    }
}

/// Splits leading `KEY=VAL` environment assignments from command tokens and handles simple quotes.
///
/// # Arguments
///
/// * `line` - Single shell command line.
#[must_use]
fn split_env_and_tokens(line: &str) -> (Vec<(String, String)>, Vec<String>) {
    let raw_toks = shell_like_split(line);
    let mut env = Vec::new();
    let mut idx = 0;
    while let Some(raw_tok) = raw_toks.get(idx) {
        let tok = raw_tok.as_str();
        if tok == "export" {
            idx += 1;
            continue;
        }
        if !tok.starts_with('-')
            && !tok.starts_with('$')
            && !tok.starts_with('.')
            && let Some((k, v)) = tok.split_once('=')
            && !k.is_empty()
            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            env.push((String::from(k), String::from(v)));
            idx += 1;
            continue;
        }
        break;
    }
    (env, raw_toks.get(idx..).unwrap_or(&[]).to_vec())
}

/// Splits a shell command line into words respecting `'...'` and `"..."` quotes.
///
/// # Arguments
///
/// * `line` - Shell line to split.
#[must_use]
pub fn shell_like_split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in line.chars() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                cur.push(ch);
            }
        } else if ch == '\u{0027}' || ch == '"' {
            quote = Some(ch);
        } else if ch == ';' || ch == '|' {
            break;
        } else if ch.is_ascii_whitespace() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Builds a [`TestStep::CompileCobol`] from a `$COMPILE*` / `$COBC` invocation.
///
/// # Arguments
///
/// * `group` - Active group builder (providing `known_files`).
/// * `head` - Compiler macro variable (`"$COMPILE"`, `"$COMPILE_ONLY"`, etc.).
/// * `rest` - Arguments following `head`.
/// * `env` - Environment variables set for this command.
/// * `expected_code` - Expected compiler exit status.
/// * `stderr_arg` - Expected stderr string for diagnostic extraction.
fn build_cobc_step(
    group: &mut SetupGroupBuilder,
    head: &str,
    rest: &[String],
    env: Vec<(String, String)>,
    expected_code: i32,
    stderr_arg: &str,
) -> Option<TestStep> {
    let mut mode = match head {
        "$COMPILE_ONLY" | "$COMPILE_LISTING" | "$COMPILE_LISTING0" => CompilationMode::SyntaxOnly,
        "$COMPILE_MODULE" => CompilationMode::CompileModule,
        _ => CompilationMode::BuildExecutable,
    };
    let mut format = SourceFormat::Fixed;
    let mut dialect = Dialect::Gnu;
    let mut include_dirs = Vec::new();
    let mut copy_exts = Vec::new();
    let mut raw_flags = Vec::new();
    let mut sources = Vec::new();
    let mut output = String::new();

    let mut i = 0;
    while let Some(raw_tok) = rest.get(i) {
        let tok = raw_tok.as_str();
        match tok {
            "-x" => mode = CompilationMode::BuildExecutable,
            "-m" | "-b" => mode = CompilationMode::CompileModule,
            "-c" => mode = CompilationMode::CompileObject,
            "-E" => mode = CompilationMode::PreprocessOnly,
            "-S" => mode = CompilationMode::EmitAssembly,
            "-fsyntax-only" => mode = CompilationMode::SyntaxOnly,
            "-free" | "-F" => format = SourceFormat::Free,
            "-fixed" => format = SourceFormat::Fixed,
            "-o" => {
                if let Some(v) = rest.get(i + 1) {
                    output = v.trim_start_matches("./").to_string();
                    i += 1;
                }
            }
            "-I" => {
                if let Some(v) = rest.get(i + 1) {
                    include_dirs.push(v.clone());
                    i += 1;
                }
            }
            "-ext" => {
                if let Some(v) = rest.get(i + 1) {
                    copy_exts.push(v.clone());
                    i += 1;
                }
            }
            "-std" => {
                if let Some(v) = rest.get(i + 1) {
                    if let Ok(d) = Dialect::from_cli_name(v) {
                        dialect = d;
                    }
                    i += 1;
                }
            }
            "<" => break,
            _ => classify_cobc_extra_token(
                tok,
                &mut dialect,
                &mut include_dirs,
                &mut raw_flags,
                &mut sources,
            ),
        }
        i += 1;
    }

    sources.retain(|s| group.known_files.contains(s));
    if sources.is_empty() {
        group.xfail_reason = Some(String::from("driver CLI or dynamic shell source check"));
        return None;
    }
    if output.is_empty() && mode == CompilationMode::BuildExecutable {
        let first_src = sources.first().map_or("prog.cob", String::as_str);
        let first_stem = Path::new(first_src)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("prog");
        output = String::from(first_stem);
        group.known_files.insert(output.clone());
    }

    let diags = extract_cobc_expected_diagnostics(stderr_arg);
    Some(TestStep::CompileCobol(CompileCobolSpec {
        mode,
        format,
        dialect,
        optimization: OptimizationLevel::O0,
        encoding: TextEncoding::Ascii,
        sources,
        include_dirs,
        copy_exts,
        library_dirs: Vec::new(),
        libraries: Vec::new(),
        raw_flags,
        output,
        expected_success: expected_code == 0,
        expected_diagnostics: diags,
        bogus_patterns: Vec::new(),
        allow_excess_errors: expected_code != 0,
        prune_patterns: Vec::new(),
        env,
    }))
}

/// Classifies attached `-std=...`, `-I...`, `-W*`/`-f*` flags, or positional source filenames.
///
/// # Arguments
///
/// * `tok` - Single token from a `cobc` command line.
/// * `dialect` - Mutable dialect field.
/// * `include_dirs` - Mutable `-I` list.
/// * `raw_flags` - Mutable raw compiler flag list.
/// * `sources` - Mutable source filename list.
fn classify_cobc_extra_token(
    tok: &str,
    dialect: &mut Dialect,
    include_dirs: &mut Vec<String>,
    raw_flags: &mut Vec<String>,
    sources: &mut Vec<String>,
) {
    if let Some(val) = tok.strip_prefix("-std=") {
        if let Ok(d) = Dialect::from_cli_name(val) {
            *dialect = d;
        }
    } else if let Some(dir) = tok.strip_prefix("-I") {
        include_dirs.push(String::from(dir));
    } else if tok.starts_with('-') {
        if tok.starts_with("-W")
            || tok.starts_with("-f")
            || tok.starts_with("-D")
            || matches!(tok, "-g" | "-v" | "-q" | "-w")
            || tok.starts_with("-conf=")
            || tok.starts_with("--tlines=")
        {
            raw_flags.push(String::from(tok));
        }
    } else {
        sources.push(tok.trim_start_matches("./").to_string());
    }
}

/// Builds a [`TestStep::CompileC`] from a `$CC` / `$COMPILE_C` command line.
///
/// # Arguments
///
/// * `rest` - Tokens following `$CC`.
fn build_cc_step(rest: &[String]) -> Option<TestStep> {
    let mut sources = Vec::new();
    let mut shared = false;
    let mut output = String::from("helper.o");
    let mut i = 0;
    while let Some(raw_tok) = rest.get(i) {
        let t = raw_tok.as_str();
        if matches!(t, "-shared" | "-m") {
            shared = true;
        } else if t == "-o" {
            if let Some(v) = rest.get(i + 1) {
                output.clone_from(v);
                i += 1;
            }
        } else if !t.starts_with('-') && Path::new(t).extension().is_some_and(|e| e == "c") {
            sources.push(t.trim_start_matches("./").to_string());
        }
        i += 1;
    }
    if sources.is_empty() {
        return None;
    }
    Some(TestStep::CompileC(CompileCSpec {
        sources,
        shared,
        output,
    }))
}

/// Builds a [`TestStep::RunBinary`] from `$COBCRUN_DIRECT`, `$COBCRUN`, `$RUN_PROG_MANUAL`, or `./prog`.
///
/// # Arguments
///
/// * `group` - Active group builder.
/// * `head` - Leading command token.
/// * `tokens` - Full command tokens.
/// * `env` - Environment variable pairs.
/// * `expected_code` - Expected process exit code.
/// * `stdout_arg` - Expected stdout spec from `AT_CHECK`.
/// * `stderr_arg` - Expected stderr spec from `AT_CHECK`.
fn build_run_step(
    group: &SetupGroupBuilder,
    head: &str,
    tokens: &[String],
    env: Vec<(String, String)>,
    expected_code: i32,
    stdout_arg: &str,
    stderr_arg: &str,
) -> TestStep {
    let mut via_cobcrun = head == "$COBCRUN";
    let start_idx = usize::from(!head.starts_with("./"));
    let mut target = String::from("./prog");
    let mut prog_args = Vec::new();
    let mut stdin_bytes = Vec::new();

    let mut idx = start_idx;
    let mut seen_target = false;
    while let Some(raw_tok) = tokens.get(idx) {
        let tok = raw_tok.as_str();
        if !seen_target && matches!(tok, "$COBCRUN_DIRECT" | "$COBCRUN" | "$RUN_PROG_MANUAL") {
            if tok == "$COBCRUN" {
                via_cobcrun = true;
            }
            idx += 1;
            continue;
        }
        if tok == "<" {
            if let Some(inp_name) = tokens.get(idx + 1) {
                let clean = inp_name.trim_start_matches("./");
                for f in &group.files {
                    if f.relative_path == clean {
                        stdin_bytes.clone_from(&f.contents);
                    }
                }
            }
            break;
        }
        if !seen_target && !tok.starts_with('-') {
            target = if via_cobcrun || tok.starts_with("./") {
                String::from(tok)
            } else {
                format!("./{tok}")
            };
            seen_target = true;
        } else if seen_target {
            prog_args.push(String::from(tok));
        }
        idx += 1;
    }

    TestStep::RunBinary(RunBinarySpec {
        target,
        via_cobcrun,
        args: prog_args,
        stdin: stdin_bytes,
        env,
        expected_exit_code: expected_code,
        stdout: resolve_at_output(stdout_arg, &group.named_outputs),
        stderr: resolve_at_output(stderr_arg, &group.named_outputs),
        expected_files: Vec::new(),
        timeout_ms: 10_000,
    })
}

/// Converts an Autotest `stdout` / `stderr` expectation argument into an [`OutputMatcher`].
///
/// # Arguments
///
/// * `spec` - Raw 3rd or 4th argument of `AT_CHECK`.
/// * `named_outputs` - Contents of `expout` / `experr` defined via `AT_DATA`.
#[must_use]
fn resolve_at_output(spec: &str, named_outputs: &BTreeMap<String, String>) -> OutputMatcher {
    let trimmed = spec.trim();
    if matches!(trimmed, "ignore" | "stdout" | "stderr") {
        return OutputMatcher::Ignore;
    }
    if matches!(trimmed, "expout" | "experr") {
        return named_outputs
            .get(trimmed)
            .cloned()
            .map_or(OutputMatcher::Ignore, OutputMatcher::Exact);
    }
    OutputMatcher::Exact(String::from(spec))
}

/// Finalizes a completed [`SetupGroupBuilder`] into a canonical [`TestManifest`].
///
/// # Arguments
///
/// * `group` - Completed `AT_SETUP` builder.
/// * `file_stem` - `.at` filename stem.
/// * `upstream_rel` - Upstream repo-relative file path.
#[must_use]
fn finalize_setup_group(
    mut group: SetupGroupBuilder,
    file_stem: &str,
    upstream_rel: &str,
) -> TestManifest {
    let has_compile = group
        .steps
        .iter()
        .any(|s| matches!(s, TestStep::CompileCobol(_)));
    if !has_compile {
        let cob_files: Vec<String> = group
            .files
            .iter()
            .filter(|f| has_cobol_ext(&f.relative_path))
            .map(|f| f.relative_path.clone())
            .collect();
        let sources = if cob_files.is_empty() {
            group.files.push(WorkspaceFile {
                relative_path: String::from("_driver_stub.cob"),
                contents: "       IDENTIFICATION DIVISION.\u{000a}       PROGRAM-ID. STUB.\u{000a}"
                    .as_bytes()
                    .to_vec(),
            });
            group.xfail_reason = Some(String::from("cobc CLI / shell environment test"));
            vec![String::from("_driver_stub.cob")]
        } else {
            cob_files
        };
        group.steps.insert(
            0,
            TestStep::CompileCobol(CompileCobolSpec {
                mode: CompilationMode::SyntaxOnly,
                format: SourceFormat::Fixed,
                dialect: Dialect::Gnu,
                optimization: OptimizationLevel::O0,
                encoding: TextEncoding::Ascii,
                sources,
                include_dirs: Vec::new(),
                copy_exts: Vec::new(),
                library_dirs: Vec::new(),
                libraries: Vec::new(),
                raw_flags: Vec::new(),
                output: String::from("stub.o"),
                expected_success: true,
                expected_diagnostics: Vec::new(),
                bogus_patterns: Vec::new(),
                allow_excess_errors: false,
                prune_patterns: Vec::new(),
                env: Vec::new(),
            }),
        );
    }

    let primary_mode = group
        .steps
        .iter()
        .find_map(|s| match s {
            TestStep::CompileCobol(c) => Some(c.mode),
            _ => None,
        })
        .unwrap_or(CompilationMode::SyntaxOnly);

    let tags = infer_feature_tags(
        &group.cobol_text_upper,
        primary_mode,
        Dialect::Gnu,
        &TextEncoding::Ascii,
    );

    let slug = sanitize_slug(&group.title);
    let id = format!("gnucobol/{file_stem}/{:04}_{slug}", group.ordinal);

    TestManifest {
        id,
        suite: SuiteId::GnuCobol,
        provenance: Provenance {
            upstream_repo: String::from(GNUCOBOL_UPSTREAM_REPO),
            upstream_commit: String::from(GNUCOBOL_PINNED_COMMIT),
            source_file: String::from(upstream_rel),
            source_line: group.line,
            original_test_name: group.title,
            spdx_license: String::from("GPL-3.0-or-later"),
        },
        files: group.files,
        steps: group.steps,
        torture_optimizations: Vec::new(),
        required_capabilities: group.capabilities.into_iter().collect(),
        xfail_reason: group.xfail_reason,
        skip_reason: group.skip_reason,
        feature_tags: tags,
        equivalent_tests: Vec::new(),
    }
}

/// Checks whether `filename` has a COBOL source extension (`.cob`, `.cbl`, `.COB`, `.CBL`).
///
/// # Arguments
///
/// * `filename` - Relative workspace filename.
#[must_use]
fn has_cobol_ext(filename: &str) -> bool {
    Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cob") || e.eq_ignore_ascii_case("cbl"))
}

/// Sanitizes an `AT_SETUP` title into a compact filesystem/ID-safe ASCII slug.
///
/// # Arguments
///
/// * `title` - Human-readable test title.
#[must_use]
fn sanitize_slug(title: &str) -> String {
    let mut slug = String::new();
    let mut prev_sep = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_sep = false;
        } else if !prev_sep && !slug.is_empty() {
            slug.push('_');
            prev_sep = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    while slug.ends_with('_') {
        slug.pop();
    }
    if slug.is_empty() {
        String::from("case")
    } else {
        slug
    }
}

/// Imports the entire `GnuCOBOL` `tests/` directory (`38` `.at` files + `3` standalone `.cob` files).
///
/// # Arguments
///
/// * `gnucobol_tests_root` - Path to `.upstream-cache/gnucobol/tests/`.
///
/// # Errors
///
/// Propagates I/O or Autotest parse errors.
pub fn import_gnucobol_tree(gnucobol_tests_root: &Path) -> Result<GnuCobolImportResult> {
    let manual_prelude = std::fs::read_to_string(gnucobol_tests_root.join("testsuite_manual.at"))?;
    let prelude_only: String = manual_prelude
        .lines()
        .filter(|l| !l.trim_start().starts_with("m4_include"))
        .collect::<Vec<_>>()
        .join("\u{000a}");

    let src_dir = gnucobol_tests_root.join("testsuite.src");
    let mut at_files = Vec::new();
    let mut cob_files = Vec::new();
    for entry in std::fs::read_dir(&src_dir)? {
        let p = entry?.path();
        if p.extension().is_some_and(|e| e == "at") {
            at_files.push(p);
        } else if p.extension().is_some_and(|e| e == "cob") {
            cob_files.push(p);
        }
    }
    at_files.sort();
    cob_files.sort();

    let mut by_file = BTreeMap::new();
    let mut total_tests = 0_usize;

    for at_path in at_files {
        let stem = at_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("suite")
            .to_string();
        let fname = at_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("suite.at");
        let rel = format!("tests/testsuite.src/{fname}");
        let prelude = if stem == "run_manual_screen" {
            prelude_only.as_str()
        } else {
            ""
        };
        let list = import_autotest_file(&at_path, &stem, &rel, prelude)?;
        total_tests += list.len();
        by_file.insert(stem, list);
    }

    let standalone = import_standalone_cob_files(&cob_files)?;
    total_tests += standalone.len();
    by_file.insert(String::from("standalone_cob"), standalone);

    Ok(GnuCobolImportResult {
        by_file,
        total_tests,
    })
}

/// Imports standalone `.cob` programs in `tests/testsuite.src/` (`numeric-display.cob`, `numeric-dump.cob`, `tutorial.cob`).
///
/// # Arguments
///
/// * `cob_files` - Sorted paths to standalone `.cob` files.
///
/// # Errors
///
/// Propagates file read errors.
fn import_standalone_cob_files(cob_files: &[std::path::PathBuf]) -> Result<Vec<TestManifest>> {
    let mut list = Vec::with_capacity(cob_files.len());
    for path in cob_files {
        let raw = std::fs::read(path)?;
        let upper = String::from_utf8_lossy(&raw).to_uppercase();
        let fname = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("prog.cob")
            .to_string();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("prog")
            .to_string();
        let tags = infer_feature_tags(
            &upper,
            CompilationMode::BuildExecutable,
            Dialect::Gnu,
            &TextEncoding::Ascii,
        );
        list.push(TestManifest {
            id: format!("gnucobol/standalone/{stem}"),
            suite: SuiteId::GnuCobol,
            provenance: Provenance {
                upstream_repo: String::from(GNUCOBOL_UPSTREAM_REPO),
                upstream_commit: String::from(GNUCOBOL_PINNED_COMMIT),
                source_file: format!("tests/testsuite.src/{fname}"),
                source_line: 1,
                original_test_name: stem,
                spdx_license: String::from("GPL-3.0-or-later"),
            },
            files: vec![WorkspaceFile {
                relative_path: fname.clone(),
                contents: raw,
            }],
            steps: vec![
                TestStep::CompileCobol(CompileCobolSpec {
                    mode: CompilationMode::BuildExecutable,
                    format: SourceFormat::Fixed,
                    dialect: Dialect::Gnu,
                    optimization: OptimizationLevel::O0,
                    encoding: TextEncoding::Ascii,
                    sources: vec![fname],
                    include_dirs: Vec::new(),
                    copy_exts: Vec::new(),
                    library_dirs: Vec::new(),
                    libraries: Vec::new(),
                    raw_flags: Vec::new(),
                    output: String::from("prog.bin"),
                    expected_success: true,
                    expected_diagnostics: Vec::new(),
                    bogus_patterns: Vec::new(),
                    allow_excess_errors: false,
                    prune_patterns: Vec::new(),
                    env: Vec::new(),
                }),
                TestStep::RunBinary(RunBinarySpec {
                    target: String::from("./prog.bin"),
                    via_cobcrun: false,
                    args: Vec::new(),
                    stdin: Vec::new(),
                    env: Vec::new(),
                    expected_exit_code: 0,
                    stdout: OutputMatcher::Ignore,
                    stderr: OutputMatcher::Ignore,
                    expected_files: Vec::new(),
                    timeout_ms: 10_000,
                }),
            ],
            torture_optimizations: Vec::new(),
            required_capabilities: Vec::new(),
            xfail_reason: None,
            skip_reason: None,
            feature_tags: tags,
            equivalent_tests: Vec::new(),
        });
    }
    Ok(list)
}
