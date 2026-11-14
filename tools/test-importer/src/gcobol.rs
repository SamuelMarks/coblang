//! `DejaGnu` (`cobol.dg`) directive parser, `gcobol` flag mapper, and GCC COBOL suite importer.

use std::path::Path;

use coblang_common::{
    Capability, CoblangError, CompilationMode, CompileCobolSpec, DiagnosticSeverity, Dialect,
    ExpectedDiagnostic, OptimizationLevel, OutputMatcher, Provenance, Result, RunBinarySpec,
    SourceFormat, SuiteId, TestManifest, TestStep, TextEncoding, WorkspaceFile,
};

use crate::taxonomy::infer_feature_tags;

/// Upstream GCC repository URL recorded in [`Provenance`].
pub const GCC_UPSTREAM_REPO: &str = "https://github.com/gcc-mirror/gcc";

/// Pinned GCC commit SHA-1 recorded in [`Provenance`].
pub const GCC_PINNED_COMMIT: &str = "6e0de8bd5ebb9a2b9903cb7348ae015d9ec7d765";

/// Partitioned GCC COBOL manifest bundles ready to write under `tests/ported/gcobol/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcobolImportBundles {
    /// Top-level `gcc/testsuite/cobol.dg/*.cob` smoke, diagnostic, and PR regression tests.
    pub top_level: Vec<TestManifest>,
    /// Foundational `gcc/testsuite/cobol.dg/group1/*.cob` tests (16 programs).
    pub group1: Vec<TestManifest>,
    /// Comprehensive `gcc/testsuite/cobol.dg/group2/*.cob` + `.out` UAT regression suite.
    pub group2: Vec<TestManifest>,
    /// Runtime smoke & POSIX binding tests under `libgcobol/{compat,posix}/t/*.cbl`.
    pub libgcobol: Vec<TestManifest>,
}

/// Parsed `DejaGnu` action requested by `{ dg-do ... }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DgAction {
    /// `{ dg-do preprocess }`
    Preprocess,
    /// `{ dg-do compile }`
    Compile,
    /// `{ dg-do assemble }`
    Assemble,
    /// `{ dg-do link }`
    Link,
    /// `{ dg-do run }`
    Run,
}

/// Structured accumulation of all `{ dg-... }` directives extracted from a `.cob` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDejaGnuDirectives {
    /// Primary `dg-do` action (`Compile` when omitted per `cobol-dg.exp` default).
    pub action: DgAction,
    /// Raw option strings from `dg-options` and `dg-additional-options`.
    pub option_strings: Vec<String>,
    /// Whether an explicit `-O*` flag was present in `dg-options`.
    pub explicit_opt: bool,
    /// Sequential regex fragments from `{ dg-output ... }`.
    pub output_regexes: Vec<String>,
    /// Companion expected output file path from `{ dg-output-file "..." }`.
    pub output_file: Option<String>,
    /// Expected compiler diagnostics (`dg-error`, `dg-warning`, `dg-message`).
    pub diagnostics: Vec<ExpectedDiagnostic>,
    /// Forbidden output regexes (`dg-bogus`).
    pub bogus_patterns: Vec<String>,
    /// Whether `dg-excess-errors` was specified.
    pub allow_excess_errors: bool,
    /// Pruned output regexes (`dg-prune-output`).
    pub prune_patterns: Vec<String>,
    /// Extra companion source files (`dg-additional-sources`).
    pub additional_sources: Vec<String>,
    /// Runtime environment variables (`dg-set-target-env-var`).
    pub runtime_env: Vec<(String, String)>,
    /// Required capabilities (`dg-require-iconv`, etc.).
    pub capabilities: Vec<Capability>,
    /// Optional `XFAIL` reason (`{ xfail ... }`, `dg-xfail-if`, or `dg-xfail-run-if`).
    pub xfail_reason: Option<String>,
    /// Optional `SKIP` reason (`dg-skip-if`).
    pub skip_reason: Option<String>,
    /// Runtime timeout multiplier (`dg-timeout-factor`, default `1`).
    pub timeout_factor: u64,
}

/// Tokenizes a Tcl-style `{ dg-... }` directive body into unquoted / unbraced words.
///
/// # Arguments
///
/// * `body` - Substring inside `{ ... }` excluding the outer braces.
/// * `source_file` - Filename for parse error reporting.
/// * `line` - One-based line number for parse error reporting.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] on unterminated `"..."` or `{...}` delimiters.
pub fn tokenize_tcl_words(body: &str, source_file: &str, line: usize) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        while i < chars.len() && chars[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        if chars[i] == '"' {
            let (word, next_i) = read_quoted_tcl_word(&chars, i + 1, source_file, line)?;
            words.push(word);
            i = next_i;
        } else if chars[i] == '{' {
            let (word, next_i) = read_braced_tcl_word(&chars, i + 1, source_file, line)?;
            words.push(word);
            i = next_i;
        } else {
            let start = i;
            while i < chars.len() && !chars[i].is_ascii_whitespace() {
                i += 1;
            }
            words.push(chars[start..i].iter().collect());
        }
    }
    Ok(words)
}

/// Reads a double-quoted Tcl string handling backslash escapes (`n`, `r`, `t`, `"`, `\`).
///
/// # Arguments
///
/// * `chars` - Full character slice.
/// * `start` - Index immediately following the opening `"`.
/// * `source_file` - Source filename for diagnostics.
/// * `line` - Line number for diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if no closing `"` is found.
fn read_quoted_tcl_word(
    chars: &[char],
    start: usize,
    source_file: &str,
    line: usize,
) -> Result<(String, usize)> {
    let mut out = String::new();
    let mut i = start;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '"' {
            return Ok((out, i + 1));
        }
        if ch == '\u{005c}' && i + 1 < chars.len() {
            i += 1;
            match chars[i] {
                'n' => out.push('\u{000a}'),
                'r' => out.push('\u{000d}'),
                't' => out.push('\u{0009}'),
                other => out.push(other),
            }
        } else {
            out.push(ch);
        }
        i += 1;
    }
    Err(CoblangError::ImportParse {
        source_file: String::from(source_file),
        line,
        message: String::from("unterminated double-quoted Tcl argument in dg-* directive"),
    })
}

/// Reads a brace-delimited `{...}` Tcl word preserving nested balanced braces verbatim.
///
/// # Arguments
///
/// * `chars` - Full character slice.
/// * `start` - Index immediately following the opening `{`.
/// * `source_file` - Source filename for diagnostics.
/// * `line` - Line number for diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if braces are unbalanced.
fn read_braced_tcl_word(
    chars: &[char],
    start: usize,
    source_file: &str,
    line: usize,
) -> Result<(String, usize)> {
    let mut out = String::new();
    let mut depth = 1_usize;
    let mut i = start;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '{' {
            depth += 1;
            out.push(ch);
        } else if ch == '}' {
            depth -= 1;
            if depth == 0 {
                return Ok((out, i + 1));
            }
            out.push(ch);
        } else {
            out.push(ch);
        }
        i += 1;
    }
    Err(CoblangError::ImportParse {
        source_file: String::from(source_file),
        line,
        message: String::from("unterminated `{...}` group in dg-* directive"),
    })
}

/// Extracts all `{ dg-... }` directives from a COBOL source file (`*>` and fixed-form `*`).
///
/// # Arguments
///
/// * `source_text` - Full UTF-8 text of the `.cob` file.
/// * `source_file` - Source filename used for diagnostics and `ExpectedDiagnostic::file`.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if a `{ dg-... }` directive has malformed syntax.
pub fn parse_dejagnu_directives(
    source_text: &str,
    source_file: &str,
) -> Result<ParsedDejaGnuDirectives> {
    let mut parsed = ParsedDejaGnuDirectives {
        action: DgAction::Compile,
        option_strings: Vec::new(),
        explicit_opt: false,
        output_regexes: Vec::new(),
        output_file: None,
        diagnostics: Vec::new(),
        bogus_patterns: Vec::new(),
        allow_excess_errors: false,
        prune_patterns: Vec::new(),
        additional_sources: Vec::new(),
        runtime_env: Vec::new(),
        capabilities: Vec::new(),
        xfail_reason: None,
        skip_reason: None,
        timeout_factor: 1,
    };

    let basename = source_file
        .rsplit('/')
        .next()
        .unwrap_or(source_file)
        .to_string();

    let mut in_multiline_output = false;
    for (idx, raw_line) in source_text.lines().enumerate() {
        let line_no = idx + 1;
        if let Some(body) = extract_dg_brace_body(raw_line, source_file, line_no)? {
            let words = tokenize_tcl_words(&body, source_file, line_no)?;
            let first = words[0].as_str();
            if first == "dg-begin-multiline-output" {
                in_multiline_output = true;
                continue;
            }
            if first == "dg-end-multiline-output" {
                in_multiline_output = false;
                continue;
            }
            if !in_multiline_output {
                apply_dg_directive(&mut parsed, &words, &basename, source_file, line_no)?;
            }
        }
    }

    Ok(parsed)
}

/// Locates `{ dg-... }` on a source line and returns the inner text between `{` and matching `}`.
///
/// # Arguments
///
/// * `line_text` - Single source line.
/// * `source_file` - Source filename for diagnostics.
/// * `line_no` - One-based line number.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if `{ dg-...` is opened without a matching `}`.
fn extract_dg_brace_body(
    line_text: &str,
    source_file: &str,
    line_no: usize,
) -> Result<Option<String>> {
    let chars: Vec<char> = line_text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' {
            let rest: String = chars[i + 1..].iter().collect();
            if rest.trim_start().starts_with("dg-") {
                let (inner, _) = read_braced_tcl_word(&chars, i + 1, source_file, line_no)?;
                return Ok(Some(inner));
            }
        }
        i += 1;
    }
    Ok(None)
}

/// Applies one tokenized `dg-*` directive to `parsed`.
///
/// # Arguments
///
/// * `parsed` - Mutable directive accumulator.
/// * `words` - Tokenized directive words (`words[0]` is `"dg-..."`).
/// * `basename` - Basename of the `.cob` file for diagnostic attribution.
/// * `source_file` - Full relative path for error reporting.
/// * `line_no` - One-based line number where the directive appeared.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] on invalid `dg-do` verbs or missing required arguments.
fn apply_dg_directive(
    parsed: &mut ParsedDejaGnuDirectives,
    words: &[String],
    basename: &str,
    source_file: &str,
    line_no: usize,
) -> Result<()> {
    match words[0].as_str() {
        "dg-do" => apply_dg_do(parsed, words, source_file, line_no)?,
        "dg-options" | "dg-additional-options" => {
            if let Some(opts) = words.get(1) {
                if opts.split_whitespace().any(|t| t.starts_with("-O")) {
                    parsed.explicit_opt = true;
                }
                parsed.option_strings.push(opts.clone());
            }
        }
        "dg-output" => {
            if let Some(pat) = words.get(1) {
                parsed.output_regexes.push(pat.clone());
            }
        }
        "dg-output-file" => {
            if let Some(path) = words.get(1) {
                parsed.output_file = Some(path.clone());
            }
        }
        "dg-error" => push_dg_diag(parsed, words, basename, line_no, DiagnosticSeverity::Error),
        "dg-warning" => push_dg_diag(
            parsed,
            words,
            basename,
            line_no,
            DiagnosticSeverity::Warning,
        ),
        "dg-message" => push_dg_diag(parsed, words, basename, line_no, DiagnosticSeverity::Note),
        "dg-bogus" => {
            if let Some(pat) = words.get(1) {
                parsed.bogus_patterns.push(pat.clone());
            }
        }
        _ => apply_dg_aux_directive(parsed, words),
    }
    Ok(())
}

/// Handles `dg-do` action selection and inline `{ xfail ... }` selectors.
///
/// # Arguments
///
/// * `parsed` - Mutable directive accumulator.
/// * `words` - Tokenized `dg-do` directive words.
/// * `source_file` - Source filename for diagnostics.
/// * `line_no` - One-based line number.
///
/// # Errors
///
/// Returns [`CoblangError::ImportParse`] if the `dg-do` action keyword is missing or unknown.
fn apply_dg_do(
    parsed: &mut ParsedDejaGnuDirectives,
    words: &[String],
    source_file: &str,
    line_no: usize,
) -> Result<()> {
    let Some(kind) = words.get(1).map(String::as_str) else {
        return Err(CoblangError::ImportParse {
            source_file: String::from(source_file),
            line: line_no,
            message: String::from("missing action in dg-do"),
        });
    };
    parsed.action = match kind {
        "preprocess" => DgAction::Preprocess,
        "compile" => DgAction::Compile,
        "assemble" => DgAction::Assemble,
        "link" => DgAction::Link,
        "run" => DgAction::Run,
        other => {
            return Err(CoblangError::ImportParse {
                source_file: String::from(source_file),
                line: line_no,
                message: format!("unknown dg-do action `{other}`"),
            });
        }
    };
    if let Some(sel) = words.get(2)
        && sel.contains("xfail")
    {
        parsed.xfail_reason = Some(String::from("dg-do xfail selector"));
    }
    Ok(())
}

/// Handles auxiliary `DejaGnu` directives (`dg-excess-errors`, `dg-prune-output`, env, capabilities, xfail/skip).
///
/// # Arguments
///
/// * `parsed` - Mutable directive accumulator.
/// * `words` - Tokenized directive words.
fn apply_dg_aux_directive(parsed: &mut ParsedDejaGnuDirectives, words: &[String]) {
    match words[0].as_str() {
        "dg-excess-errors" => parsed.allow_excess_errors = true,
        "dg-prune-output" => {
            if let Some(pat) = words.get(1) {
                parsed.prune_patterns.push(pat.clone());
            }
        }
        "dg-additional-sources" => {
            if let Some(srcs) = words.get(1) {
                for item in srcs.split_whitespace() {
                    parsed.additional_sources.push(String::from(item));
                }
            }
        }
        "dg-set-target-env-var" => {
            if let (Some(k), Some(v)) = (words.get(1), words.get(2)) {
                parsed.runtime_env.push((k.clone(), v.clone()));
            }
        }
        "dg-require-iconv" => {
            if let Some(cp) = words.get(1) {
                parsed
                    .capabilities
                    .push(Capability::IconvCodepage(cp.clone()));
            }
        }
        "dg-require-effective-target" => {
            if words.get(1).is_some_and(|tgt| tgt == "int128") {
                parsed.capabilities.push(Capability::Int128);
            }
        }
        "dg-xfail-if" | "dg-xfail-run-if" => {
            let note = words
                .get(1)
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| words[0].clone());
            parsed.xfail_reason = Some(note);
        }
        "dg-skip-if" => {
            let note = words
                .get(1)
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| String::from("dg-skip-if"));
            parsed.skip_reason = Some(note);
        }
        "dg-timeout-factor" => {
            if let Some(raw) = words.get(1) {
                let int_part = raw.split_once('.').map_or(raw.as_str(), |(lhs, _)| lhs);
                if let Ok(val) = int_part.parse::<u64>() {
                    parsed.timeout_factor = val.max(1);
                }
            }
        }
        _ => {}
    }
}

/// Appends an [`ExpectedDiagnostic`] resolving `.` / `.+N` / `.-N` / absolute line numbers.
///
/// # Arguments
///
/// * `parsed` - Mutable directive accumulator.
/// * `words` - Tokenized `dg-error` / `dg-warning` / `dg-message` words.
/// * `basename` - Source file basename.
/// * `directive_line` - 1-based line number of the comment containing the directive.
/// * `severity` - Diagnostic severity.
fn push_dg_diag(
    parsed: &mut ParsedDejaGnuDirectives,
    words: &[String],
    basename: &str,
    directive_line: usize,
    severity: DiagnosticSeverity,
) {
    let pattern = words.get(1).cloned().unwrap_or_default();
    let target_line = resolve_dg_line_spec(words.get(4).map(String::as_str), directive_line);
    parsed.diagnostics.push(ExpectedDiagnostic {
        file: String::from(basename),
        line: target_line,
        severity,
        message_pattern: pattern,
    });
}

/// Resolves a `DejaGnu` line selector (`None`, `"."`, `".-1"`, `".+2"`, `"0"`, or `"42"`).
///
/// # Arguments
///
/// * `spec` - Optional 4th argument of `dg-error` / `dg-warning`.
/// * `current_line` - 1-based line number of the directive comment.
#[must_use]
pub fn resolve_dg_line_spec(spec: Option<&str>, current_line: usize) -> Option<u32> {
    let default_ln = u32::try_from(current_line).ok();
    let Some(raw) = spec.map(str::trim) else {
        return default_ln;
    };
    if raw == "." || raw.is_empty() {
        return default_ln;
    }
    if raw == "0" {
        return None;
    }
    if let Some(off) = raw.strip_prefix(".+")
        && let Ok(delta) = off.parse::<usize>()
    {
        return u32::try_from(current_line.saturating_add(delta)).ok();
    }
    if let Some(off) = raw.strip_prefix(".-")
        && let Ok(delta) = off.parse::<usize>()
    {
        return u32::try_from(current_line.saturating_sub(delta)).ok();
    }
    raw.parse::<u32>().ok()
}

/// Applies `gcobol` CLI flags from `dg-options` onto a [`CompileCobolSpec`].
///
/// # Arguments
///
/// * `spec` - Mutable compilation spec to update.
/// * `option_strings` - Raw option strings extracted from `dg-options`.
pub fn apply_gcobol_options(spec: &mut CompileCobolSpec, option_strings: &[String]) {
    for raw in option_strings {
        let tokens: Vec<&str> = raw.split_whitespace().collect();
        let mut i = 0;
        while i < tokens.len() {
            let tok = tokens[i];
            match tok {
                "-ffixed-form" => spec.format = SourceFormat::Fixed,
                "-ffree-form" | "-free" => spec.format = SourceFormat::Free,
                "-finternal-ebcdic" => spec.encoding = TextEncoding::Ebcdic,
                "-dialect" => {
                    if let Some(&val) = tokens.get(i + 1) {
                        if let Ok(d) = Dialect::from_cli_name(val) {
                            spec.dialect = d;
                        }
                        i += 1;
                    }
                }
                "-I" => {
                    if let Some(&dir) = tokens.get(i + 1) {
                        spec.include_dirs.push(String::from(dir));
                        i += 1;
                    }
                }
                "-copyext" => {
                    if let Some(&ext) = tokens.get(i + 1) {
                        spec.copy_exts.push(String::from(ext));
                        i += 1;
                    }
                }
                "-fdefaultbyte" | "-fcobol-exceptions" => {
                    if let Some(&val) = tokens.get(i + 1) {
                        spec.raw_flags.push(format!("{tok}={val}"));
                        i += 1;
                    } else {
                        spec.raw_flags.push(String::from(tok));
                    }
                }
                _ => apply_gcobol_single_flag(spec, tok),
            }
            i += 1;
        }
    }
}

/// Applies a single-token `gcobol` flag to `spec`.
///
/// # Arguments
///
/// * `spec` - Mutable compilation spec.
/// * `tok` - Single whitespace-split option token.
fn apply_gcobol_single_flag(spec: &mut CompileCobolSpec, tok: &str) {
    if let Some(cp) = tok.strip_prefix("-fexec-charset=") {
        spec.encoding = TextEncoding::Codepage(String::from(cp));
    } else if let Some(dir) = tok.strip_prefix("-I") {
        spec.include_dirs.push(String::from(dir));
    } else if let Ok(opt) = OptimizationLevel::parse_flag(tok, 0) {
        spec.optimization = opt;
    } else {
        spec.raw_flags.push(String::from(tok));
    }
}

/// Converts a single `cobol.dg` `.cob` file into a canonical [`TestManifest`].
///
/// # Arguments
///
/// * `cob_path` - Full filesystem path to the `.cob` source file.
/// * `cobol_dg_root` - Path to `gcc/testsuite/cobol.dg/` (used to resolve `dg-output-file` paths).
/// * `id_prefix` - Manifest ID prefix (e.g., `"gcobol/top"`, `"gcobol/group1"`, `"gcobol/group2"`).
/// * `upstream_rel` - Upstream repository-relative path for [`Provenance`].
///
/// # Errors
///
/// Propagates I/O or `DejaGnu` directive parse errors.
pub fn import_cobol_dg_file(
    cob_path: &Path,
    cobol_dg_root: &Path,
    id_prefix: &str,
    upstream_rel: &str,
) -> Result<TestManifest> {
    let raw_bytes = std::fs::read(cob_path)?;
    let source_text = String::from_utf8_lossy(&raw_bytes).to_string();
    let dg = parse_dejagnu_directives(&source_text, upstream_rel)?;

    let (file_name, stem) = path_name_and_stem(cob_path);

    let mode = match dg.action {
        DgAction::Preprocess => CompilationMode::PreprocessOnly,
        DgAction::Compile => CompilationMode::SyntaxOnly,
        DgAction::Assemble => CompilationMode::CompileObject,
        DgAction::Link | DgAction::Run => CompilationMode::BuildExecutable,
    };

    let has_errors = dg
        .diagnostics
        .iter()
        .any(|d| d.severity == DiagnosticSeverity::Error);

    let (files, sources) = collect_dg_workspace_files(
        cob_path,
        cobol_dg_root,
        file_name,
        raw_bytes,
        &dg.additional_sources,
    );

    let stdout_matcher = if dg.action == DgAction::Run && !has_errors {
        Some(resolve_dg_stdout_matcher(
            dg.output_file.as_deref(),
            &dg.output_regexes,
            cobol_dg_root,
        )?)
    } else {
        None
    };

    let mut compile_spec = CompileCobolSpec {
        mode,
        format: SourceFormat::Auto,
        dialect: Dialect::Cobol2023,
        optimization: OptimizationLevel::O0,
        encoding: TextEncoding::Ascii,
        sources,
        include_dirs: Vec::new(),
        copy_exts: Vec::new(),
        library_dirs: Vec::new(),
        libraries: Vec::new(),
        raw_flags: Vec::new(),
        output: String::from("test.bin"),
        expected_success: !has_errors,
        expected_diagnostics: dg.diagnostics,
        bogus_patterns: dg.bogus_patterns,
        allow_excess_errors: dg.allow_excess_errors,
        prune_patterns: dg.prune_patterns,
        env: Vec::new(),
    };
    apply_gcobol_options(&mut compile_spec, &dg.option_strings);

    let tags = infer_feature_tags(
        &source_text.to_uppercase(),
        compile_spec.mode,
        compile_spec.dialect,
        &compile_spec.encoding,
    );

    let mut steps = vec![TestStep::CompileCobol(compile_spec)];
    if let Some(matcher) = stdout_matcher {
        steps.push(TestStep::RunBinary(RunBinarySpec {
            target: String::from("./test.bin"),
            via_cobcrun: false,
            args: Vec::new(),
            stdin: Vec::new(),
            env: dg.runtime_env,
            expected_exit_code: 0,
            stdout: matcher,
            stderr: OutputMatcher::Ignore,
            expected_files: Vec::new(),
            timeout_ms: 10_000 * dg.timeout_factor,
        }));
    }

    let torture_optimizations = default_dg_torture_levels(dg.action, dg.explicit_opt);

    Ok(TestManifest {
        id: format!("{id_prefix}/{stem}"),
        suite: SuiteId::Gcobol,
        provenance: Provenance {
            upstream_repo: String::from(GCC_UPSTREAM_REPO),
            upstream_commit: String::from(GCC_PINNED_COMMIT),
            source_file: String::from(upstream_rel),
            source_line: 1,
            original_test_name: stem,
            spdx_license: String::from("GPL-3.0-or-later"),
        },
        files,
        steps,
        torture_optimizations,
        required_capabilities: dg.capabilities,
        xfail_reason: dg.xfail_reason,
        skip_reason: dg.skip_reason,
        feature_tags: tags,
        equivalent_tests: Vec::new(),
    })
}

/// Returns the standard GCC optimization torture matrix when `action == Run` and no `-O` was pinned.
///
/// # Arguments
///
/// * `action` - Parsed `dg-do` action.
/// * `explicit_opt` - Whether `dg-options` explicitly specified an `-O` flag.
#[must_use]
fn default_dg_torture_levels(action: DgAction, explicit_opt: bool) -> Vec<OptimizationLevel> {
    if action == DgAction::Run && !explicit_opt {
        vec![
            OptimizationLevel::O0,
            OptimizationLevel::O1,
            OptimizationLevel::O2,
            OptimizationLevel::O3,
            OptimizationLevel::Os,
        ]
    } else {
        Vec::new()
    }
}

/// Assembles primary and `dg-additional-sources` files for a `cobol.dg` test case.
///
/// # Arguments
///
/// * `cob_path` - Path to primary `.cob` source file.
/// * `cobol_dg_root` - Fallback parent directory.
/// * `file_name` - Primary `.cob` filename.
/// * `raw_bytes` - Raw bytes of the primary `.cob` file.
/// * `additional_sources` - Companion filenames from `dg-additional-sources`.
fn collect_dg_workspace_files(
    cob_path: &Path,
    cobol_dg_root: &Path,
    file_name: String,
    raw_bytes: Vec<u8>,
    additional_sources: &[String],
) -> (Vec<WorkspaceFile>, Vec<String>) {
    let mut files = vec![WorkspaceFile {
        relative_path: file_name.clone(),
        contents: raw_bytes,
    }];
    let mut sources = vec![file_name];
    let parent_dir = cob_path.parent().unwrap_or(cobol_dg_root);
    for extra in additional_sources {
        let extra_path = parent_dir.join(extra);
        if let Ok(extra_bytes) = std::fs::read(&extra_path) {
            files.push(WorkspaceFile {
                relative_path: extra.clone(),
                contents: extra_bytes,
            });
        }
        sources.push(extra.clone());
    }
    (files, sources)
}

/// Builds the [`OutputMatcher`] for a `dg-do run` test from `dg-output-file` or `dg-output`.
///
/// # Arguments
///
/// * `output_file` - Optional companion `.out` file path relative to `cobol_dg_root`.
/// * `output_regexes` - Slice of regex fragments from `dg-output`.
/// * `cobol_dg_root` - Root `gcc/testsuite/cobol.dg/` directory.
///
/// # Errors
///
/// Returns [`CoblangError::Io`] if a referenced `dg-output-file` cannot be read.
fn resolve_dg_stdout_matcher(
    output_file: Option<&str>,
    output_regexes: &[String],
    cobol_dg_root: &Path,
) -> Result<OutputMatcher> {
    if let Some(rel_out) = output_file {
        let out_bytes = std::fs::read(cobol_dg_root.join(rel_out))?;
        let text = String::from_utf8_lossy(&out_bytes).to_string();
        return Ok(OutputMatcher::Exact(text));
    }
    if !output_regexes.is_empty() {
        return Ok(OutputMatcher::Regex(output_regexes.join("")));
    }
    Ok(OutputMatcher::Ignore)
}

/// Imports the entire cached GCC COBOL tree (`cobol.dg` top-level, `group1`, `group2`, and `libgcobol`).
///
/// # Arguments
///
/// * `gcobol_cache_root` - Path to `.upstream-cache/gcobol/`.
///
/// # Errors
///
/// Propagates I/O or `DejaGnu` parse errors.
pub fn import_gcobol_tree(gcobol_cache_root: &Path) -> Result<GcobolImportBundles> {
    let dg_root = gcobol_cache_root.join("gcc/testsuite/cobol.dg");
    let top_level = import_dg_dir(&dg_root, &dg_root, "gcobol/top", "gcc/testsuite/cobol.dg")?;
    let group1 = import_dg_dir(
        &dg_root.join("group1"),
        &dg_root,
        "gcobol/group1",
        "gcc/testsuite/cobol.dg/group1",
    )?;
    let group2 = import_dg_dir(
        &dg_root.join("group2"),
        &dg_root,
        "gcobol/group2",
        "gcc/testsuite/cobol.dg/group2",
    )?;
    let libgcobol = import_libgcobol_tests(&gcobol_cache_root.join("libgcobol"))?;

    Ok(GcobolImportBundles {
        top_level,
        group1,
        group2,
        libgcobol,
    })
}

/// Imports all sorted `*.cob` files directly inside `dir`.
///
/// # Arguments
///
/// * `dir` - Directory containing `.cob` files.
/// * `dg_root` - Root `cobol.dg` directory for resolving companion `.out` paths.
/// * `id_prefix` - Canonical ID prefix.
/// * `rel_prefix` - Upstream repo-relative directory prefix.
///
/// # Errors
///
/// Propagates filesystem or parse errors.
fn import_dg_dir(
    dir: &Path,
    dg_root: &Path,
    id_prefix: &str,
    rel_prefix: &str,
) -> Result<Vec<TestManifest>> {
    let mut cob_paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "cob") {
            cob_paths.push(path);
        }
    }
    cob_paths.sort();

    let mut out = Vec::with_capacity(cob_paths.len());
    for p in cob_paths {
        let (fname, _) = path_name_and_stem(&p);
        let rel = format!("{rel_prefix}/{fname}");
        out.push(import_cobol_dg_file(&p, dg_root, id_prefix, &rel)?);
    }
    Ok(out)
}

/// Extracts `(filename, stem)` strings from a [`Path`] without fallible closures.
///
/// # Arguments
///
/// * `path` - Filesystem path to inspect.
#[must_use]
fn path_name_and_stem(path: &Path) -> (String, String) {
    let fname = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("test.cob")
        .to_string();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("test")
        .to_string();
    (fname, stem)
}

/// Imports `libgcobol/{compat,posix}/t/*.cbl` along with their copybook & UDF workspace dependencies.
///
/// # Arguments
///
/// * `libgcobol_root` - Path to `.upstream-cache/gcobol/libgcobol/`.
///
/// # Errors
///
/// Propagates filesystem errors.
fn import_libgcobol_tests(libgcobol_root: &Path) -> Result<Vec<TestManifest>> {
    let shared_files = collect_libgcobol_support_files(libgcobol_root)?;
    let mut cbl_entries = Vec::new();
    for sub in ["compat/t", "posix/t"] {
        let dir = libgcobol_root.join(sub);
        for entry in std::fs::read_dir(&dir)? {
            let p = entry?.path();
            if p.extension().is_some_and(|e| e == "cbl") {
                cbl_entries.push((sub, p));
            }
        }
    }
    cbl_entries.sort_by(|a, b| a.1.cmp(&b.1));

    let mut manifests = Vec::with_capacity(cbl_entries.len());
    for (sub, path) in cbl_entries {
        manifests.push(build_libgcobol_manifest(sub, &path, &shared_files)?);
    }
    Ok(manifests)
}

/// Collects `.cpy` and `.cbl` support files under `libgcobol/{compat/gnu/cpy,posix/cpy,posix/udf}`.
///
/// # Arguments
///
/// * `libgcobol_root` - Root of cached `libgcobol/`.
///
/// # Errors
///
/// Propagates filesystem errors.
fn collect_libgcobol_support_files(libgcobol_root: &Path) -> Result<Vec<WorkspaceFile>> {
    let mut support = Vec::new();
    for sub in ["compat/gnu/cpy", "posix/cpy", "posix/udf"] {
        let dir = libgcobol_root.join(sub);
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            entries.push(entry?.path());
        }
        entries.sort();
        for p in entries {
            let (fname, _) = path_name_and_stem(&p);
            support.push(WorkspaceFile {
                relative_path: format!("{sub}/{fname}"),
                contents: std::fs::read(&p)?,
            });
        }
    }
    Ok(support)
}

/// Constructs a single `libgcobol` [`TestManifest`] with its include paths and POSIX capability tags.
///
/// # Arguments
///
/// * `sub` - Subdirectory (`"compat/t"` or `"posix/t"`).
/// * `path` - Full path to the `.cbl` test file.
/// * `support_files` - Pre-collected copybook and UDF workspace files.
///
/// # Errors
///
/// Propagates filesystem read errors.
fn build_libgcobol_manifest(
    sub: &str,
    path: &Path,
    support_files: &[WorkspaceFile],
) -> Result<TestManifest> {
    let raw = std::fs::read(path)?;
    let text_upper = String::from_utf8_lossy(&raw).to_uppercase();
    let (fname, stem) = path_name_and_stem(path);

    let mut files = support_files.to_vec();
    files.push(WorkspaceFile {
        relative_path: fname.clone(),
        contents: raw,
    });

    let is_compat = sub == "compat/t";
    let format = if is_compat {
        SourceFormat::Fixed
    } else {
        SourceFormat::Auto
    };
    let dialect = if is_compat {
        Dialect::MicroFocus
    } else {
        Dialect::Cobol2023
    };
    let tags = infer_feature_tags(
        &text_upper,
        CompilationMode::BuildExecutable,
        dialect,
        &TextEncoding::Ascii,
    );

    Ok(TestManifest {
        id: format!("gcobol/libgcobol/{sub}/{stem}"),
        suite: SuiteId::Gcobol,
        provenance: Provenance {
            upstream_repo: String::from(GCC_UPSTREAM_REPO),
            upstream_commit: String::from(GCC_PINNED_COMMIT),
            source_file: format!("libgcobol/{sub}/{fname}"),
            source_line: 1,
            original_test_name: stem,
            spdx_license: String::from("GPL-3.0-or-later WITH GCC-exception-3.1"),
        },
        files,
        steps: vec![
            TestStep::CompileCobol(CompileCobolSpec {
                mode: CompilationMode::BuildExecutable,
                format,
                dialect,
                optimization: OptimizationLevel::O0,
                encoding: TextEncoding::Ascii,
                sources: vec![fname],
                include_dirs: vec![
                    String::from("."),
                    String::from("compat/gnu/cpy"),
                    String::from("posix/cpy"),
                    String::from("posix/udf"),
                ],
                copy_exts: Vec::new(),
                library_dirs: Vec::new(),
                libraries: Vec::new(),
                raw_flags: Vec::new(),
                output: String::from("test.bin"),
                expected_success: true,
                expected_diagnostics: Vec::new(),
                bogus_patterns: Vec::new(),
                allow_excess_errors: false,
                prune_patterns: Vec::new(),
                env: Vec::new(),
            }),
            TestStep::RunBinary(RunBinarySpec {
                target: String::from("./test.bin"),
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
        required_capabilities: vec![Capability::PosixTools],
        xfail_reason: None,
        skip_reason: None,
        feature_tags: tags,
        equivalent_tests: Vec::new(),
    })
}
