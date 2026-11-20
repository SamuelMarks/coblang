//! `COPY ... IN/OF ... REPLACING` copybook resolver and recursive token-stream expander.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use coblang_common::{CoblangError, ResourceLimitKind, Result, SourceSpan};
use coblang_diag::{Diagnostic, DiagnosticCode, DiagnosticSink, ExpansionKind, SourceMap};

use crate::cdf::{self, CdfEnv};
use crate::format::normalize_text;
use crate::pseudo_text::{
    PpToken, PpTokenKind, apply_replacement_rules, parse_replacing_clauses,
    tokenize_normalized_lines,
};
use crate::{PreprocessConfig, PreprocessLimits};

/// Double-quote character (`0x22`).
const DQ: char = '"';
/// Single-quote character (`0x27`).
const SQ: char = '\u{0027}';

/// Default copybook file extensions probed in standard `GnuCOBOL` / `gcobol` search order.
const DEFAULT_COPY_EXTS: &[&str] = &[
    "", ".cpy", ".CPY", ".inc", ".INC", ".cbl", ".CBL", ".cob", ".COB",
];

/// Pluggable lookup interface resolving `COPY <name> [IN/OF <library>]` to `(path, text)`.
pub trait CopybookResolver {
    /// Resolves a copybook by name and optional library qualifier relative to `caller_dir`.
    ///
    /// # Arguments
    ///
    /// * `name` - Unquoted copybook name.
    /// * `library` - Optional unquoted `IN` / `OF` library qualifier.
    /// * `caller_dir` - Directory containing the file that issued the `COPY` statement.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::PreprocessError`] if no matching copybook file exists.
    fn resolve_copybook(
        &self,
        name: &str,
        library: Option<&str>,
        caller_dir: &Path,
    ) -> Result<(PathBuf, String)>;
}

/// Production filesystem [`CopybookResolver`] honoring `-I`, `COB_COPY_DIR`, `COB_COPY_LIB_*`, and `-ext`.
#[derive(Debug, Clone, Default)]
pub struct FilesystemCopybookResolver {
    /// Explicit `-I` search directories.
    pub include_dirs: Vec<PathBuf>,
    /// Additional copybook extensions (`-copyext` / `-ext`).
    pub extra_extensions: Vec<String>,
    /// Optional `COB_COPY_DIR` colon-separated path list.
    pub cob_copy_dir: Option<String>,
    /// Optional `COB_COPY_LIB_<UPPER_LIB>` directory overrides.
    pub library_dirs: BTreeMap<String, PathBuf>,
}

impl CopybookResolver for FilesystemCopybookResolver {
    /// Resolves `name` (and optional `library`) on disk across configured copybook directories.
    ///
    /// # Arguments
    ///
    /// * `name` - Copybook name token.
    /// * `library` - Optional library name token (`IN` / `OF`).
    /// * `caller_dir` - Directory containing the including file.
    fn resolve_copybook(
        &self,
        name: &str,
        library: Option<&str>,
        caller_dir: &Path,
    ) -> Result<(PathBuf, String)> {
        let mut bases = Vec::new();
        if let Some(lib) = library {
            let clean_lib = unquote(lib);
            if let Some(mapped) = self.library_dirs.get(&clean_lib.to_ascii_uppercase()) {
                bases.push(mapped.clone());
            }
            bases.push(caller_dir.join(clean_lib));
            for inc in &self.include_dirs {
                bases.push(inc.join(clean_lib));
            }
        } else {
            bases.push(caller_dir.to_path_buf());
            bases.extend(self.include_dirs.iter().cloned());
            if let Some(env_dirs) = &self.cob_copy_dir {
                for entry in env_dirs.split(':').filter(|s| !s.is_empty()) {
                    bases.push(PathBuf::from(entry));
                }
            }
        }

        let clean_name = unquote(name);
        for base in &bases {
            for ext in self.candidate_extensions() {
                let candidate = if ext.is_empty() {
                    base.join(clean_name)
                } else if let Some(stripped) = ext.strip_prefix('.') {
                    base.join(format!("{clean_name}.{stripped}"))
                } else {
                    base.join(format!("{clean_name}.{ext}"))
                };
                if candidate.is_file() {
                    let content = std::fs::read_to_string(&candidate)?;
                    return Ok((candidate, content));
                }
            }
        }

        if library.is_none()
            && let Some((logical_path, text)) = lookup_builtin_copybook(clean_name)
        {
            return Ok((PathBuf::from(logical_path), String::from(text)));
        }

        Err(CoblangError::PreprocessError {
            file: String::from(clean_name),
            line: 1,
            column: 1,
            message: format!("copybook `{clean_name}`: No such file or directory"),
        })
    }
}

/// Resolves clean-room built-in runtime interface copybooks (`gcwindow`, `screenio`).
///
/// # Arguments
///
/// * `clean_name` - Unquoted copybook basename or stem.
#[must_use]
fn lookup_builtin_copybook(clean_name: &str) -> Option<(&'static str, &'static str)> {
    let stem = clean_name
        .strip_suffix(".cpy")
        .or_else(|| clean_name.strip_suffix(".CPY"))
        .unwrap_or(clean_name);
    if stem.eq_ignore_ascii_case("gcwindow") {
        Some(("<builtin:gcwindow.cpy>", BUILTIN_GCWINDOW_CPY))
    } else if stem.eq_ignore_ascii_case("screenio") {
        Some(("<builtin:screenio.cpy>", BUILTIN_SCREENIO_CPY))
    } else {
        None
    }
}

/// Clean-room `gcwindow` TUI window parameter definitions for `CBL_GC_WINDOW`.
const BUILTIN_GCWINDOW_CPY: &str = "       01  WP-WINDOW-MODE              PIC X(02) VALUE '01'.
           88  WP-MODE-NEW             VALUE '01'.
           88  WP-MODE-DELETE          VALUE '02'.
           88  WP-MODE-HIDE            VALUE '03'.
           88  WP-MODE-SHOW            VALUE '04'.
           88  WP-MODE-TOP             VALUE '05'.
           88  WP-MODE-BOTTOM          VALUE '06'.
           88  WP-MODE-MOVE            VALUE '07'.
       01  WP-WINDOW-LIST              PIC X(02) VALUE '08'.
       01  WP-FLAG-VISIBLE             PIC S9(4) COMP-5 VALUE 0.
       01  WP-FLAG-HIDDEN              PIC S9(4) COMP-5 VALUE 1.
       01  WP-WINDOW-PARMS.
           05  WP-WINDOW-NUMBER        PIC S9(4) COMP-5 VALUE 0.
           05  WP-WINDOW-START-LINE    PIC S9(4) COMP-5 VALUE 1.
           05  WP-WINDOW-START-COL     PIC S9(4) COMP-5 VALUE 1.
           05  WP-WINDOW-LINES         PIC S9(4) COMP-5 VALUE 24.
           05  WP-WINDOW-COLUMNS       PIC S9(4) COMP-5 VALUE 80.
           05  WP-WINDOW-FG-COLOR      PIC S9(4) COMP-5 VALUE 7.
           05  WP-WINDOW-BG-COLOR      PIC S9(4) COMP-5 VALUE 0.
           05  WP-WINDOW-FLAGS         PIC S9(4) COMP-5 VALUE 0.
       01  WL-WINDOW-LIST.
           05  WL-WINDOW OCCURS 20 TIMES INDEXED BY WL-INDEX.
               10  WL-DEPTH                PIC S9(4) COMP-5.
               10  WL-WINDOW-NUMBER        PIC S9(4) COMP-5.
               10  WL-WINDOW-POSITION-LINE PIC S9(4) COMP-5.
               10  WL-WINDOW-POSITION-COL  PIC S9(4) COMP-5.
               10  WL-WINDOW-SIZE-LINES    PIC S9(4) COMP-5.
               10  WL-WINDOW-SIZE-COLS     PIC S9(4) COMP-5.
               10  WL-WINDOW-FG-COLOR      PIC S9(4) COMP-5.
               10  WL-WINDOW-BG-COLOR      PIC S9(4) COMP-5.
               10  WL-WINDOW-FLAGS         PIC S9(4) COMP-5.
";

/// Clean-room `screenio` CRT status and color constant definitions.
const BUILTIN_SCREENIO_CPY: &str = "       78  COB-SCR-OK                  VALUE 0000.
       78  COB-SCR-F1                  VALUE 1001.
       78  COB-SCR-F2                  VALUE 1002.
       78  COB-SCR-F3                  VALUE 1003.
       78  COB-SCR-F4                  VALUE 1004.
       78  COB-SCR-F5                  VALUE 1005.
       78  COB-SCR-F6                  VALUE 1006.
       78  COB-SCR-F7                  VALUE 1007.
       78  COB-SCR-F8                  VALUE 1008.
       78  COB-SCR-F9                  VALUE 1009.
       78  COB-SCR-F10                 VALUE 1010.
       78  COB-SCR-F11                 VALUE 1011.
       78  COB-SCR-F12                 VALUE 1012.
       78  COB-SCR-PAGE-UP             VALUE 2001.
       78  COB-SCR-PAGE-DOWN           VALUE 2002.
       78  COB-SCR-KEY-UP              VALUE 2003.
       78  COB-SCR-KEY-DOWN            VALUE 2004.
       78  COB-SCR-ESC                 VALUE 2005.
       78  COB-SCR-PRINT               VALUE 2006.
       78  COB-SCR-TAB                 VALUE 2007.
       78  COB-SCR-BACK-TAB            VALUE 2008.
       78  COB-SCR-KEY-LEFT            VALUE 2009.
       78  COB-SCR-KEY-RIGHT           VALUE 2010.
       78  COB-SCR-NO-FIELD            VALUE 8000.
       78  COB-SCR-TIME-OUT            VALUE 8001.
       78  COB-COLOR-BLACK             VALUE 0.
       78  COB-COLOR-BLUE              VALUE 1.
       78  COB-COLOR-GREEN             VALUE 2.
       78  COB-COLOR-CYAN              VALUE 3.
       78  COB-COLOR-RED               VALUE 4.
       78  COB-COLOR-MAGENTA           VALUE 5.
       78  COB-COLOR-YELLOW            VALUE 6.
       78  COB-COLOR-WHITE             VALUE 7.
";

impl FilesystemCopybookResolver {
    /// Builds the ordered extension probe list combining [`DEFAULT_COPY_EXTS`] and `extra_extensions`.
    #[must_use]
    fn candidate_extensions(&self) -> Vec<String> {
        let mut exts: Vec<String> = DEFAULT_COPY_EXTS
            .iter()
            .copied()
            .map(String::from)
            .collect();
        for extra in &self.extra_extensions {
            if !exts.contains(extra) {
                exts.push(extra.clone());
            }
        }
        exts
    }
}

/// Hermetic in-memory [`CopybookResolver`] used in unit tests.
#[derive(Debug, Clone, Default)]
pub struct InMemoryCopybookResolver {
    /// Map from `(optional_upper_lib, upper_name)` to `(display_path, content)`.
    files: BTreeMap<(Option<String>, String), (PathBuf, String)>,
}

impl InMemoryCopybookResolver {
    /// Creates an empty [`InMemoryCopybookResolver`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an in-memory copybook entry.
    ///
    /// # Arguments
    ///
    /// * `library` - Optional library name (`IN`/`OF`).
    /// * `name` - Copybook stem or filename.
    /// * `display_path` - Logical path reported in diagnostics.
    /// * `content` - Copybook COBOL source text.
    pub fn insert(&mut self, library: Option<&str>, name: &str, display_path: &str, content: &str) {
        self.files.insert(
            (
                library.map(|l| unquote(l).to_ascii_uppercase()),
                unquote(name).to_ascii_uppercase(),
            ),
            (PathBuf::from(display_path), String::from(content)),
        );
    }
}

impl CopybookResolver for InMemoryCopybookResolver {
    /// Looks up `(library, name)` inside the in-memory copybook map.
    ///
    /// # Arguments
    ///
    /// * `name` - Copybook name token.
    /// * `library` - Optional library qualifier.
    /// * `_caller_dir` - Unused caller directory.
    fn resolve_copybook(
        &self,
        name: &str,
        library: Option<&str>,
        _caller_dir: &Path,
    ) -> Result<(PathBuf, String)> {
        let key = (
            library.map(|l| unquote(l).to_ascii_uppercase()),
            unquote(name).to_ascii_uppercase(),
        );
        self.files
            .get(&key)
            .cloned()
            .ok_or_else(|| CoblangError::PreprocessError {
                file: String::from(name),
                line: 1,
                column: 1,
                message: format!("copybook `{}`: No such file or directory", unquote(name)),
            })
    }
}

/// Mutable state threaded through recursive `COPY` expansion.
struct CopyExpandState<'a, 'b> {
    /// Preprocessor configuration.
    cfg: &'a PreprocessConfig<'a>,
    /// Resource safeguard budgets.
    limits: PreprocessLimits,
    /// Cumulative expanded token count across the entire compilation unit.
    emitted_tokens: usize,
    /// Active CDF environment.
    cdf_env: &'b mut CdfEnv,
    /// Active source map.
    source_map: &'b mut SourceMap,
    /// Diagnostic sink.
    sink: &'b mut DiagnosticSink,
    /// Active inclusion stack for cycle detection.
    active_stack: &'b mut Vec<String>,
}

/// Recursively expands all `COPY` statements inside `tokens` under default [`PreprocessLimits`].
///
/// # Arguments
///
/// * `tokens` - Input [`PpToken`] stream.
/// * `caller_dir` - Directory of the file that owns `tokens`.
/// * `cfg` - Active [`PreprocessConfig`].
/// * `cdf_env` - Mutable [`CdfEnv`] shared across included copybooks.
/// * `source_map` - Mutable [`SourceMap`].
/// * `sink` - Mutable [`DiagnosticSink`].
/// * `active_stack` - Canonical path stack used to detect recursive `COPY` cycles.
///
/// # Errors
///
/// Propagates [`CoblangError`] if `SourceMap` or [`PreprocessLimits`] limits are exceeded.
pub fn expand_copy_tokens(
    tokens: &[PpToken],
    caller_dir: &Path,
    cfg: &PreprocessConfig<'_>,
    cdf_env: &mut CdfEnv,
    source_map: &mut SourceMap,
    sink: &mut DiagnosticSink,
    active_stack: &mut Vec<String>,
) -> Result<Vec<PpToken>> {
    expand_copy_tokens_with_limits(
        tokens,
        caller_dir,
        (cfg, PreprocessLimits::default()),
        cdf_env,
        source_map,
        sink,
        active_stack,
    )
}

/// Recursively expands all `COPY` statements inside `tokens` under explicit [`PreprocessLimits`].
///
/// # Arguments
///
/// * `tokens` - Input [`PpToken`] stream.
/// * `caller_dir` - Directory of the file that owns `tokens`.
/// * `cfg_and_limits` - Active [`PreprocessConfig`] paired with explicit [`PreprocessLimits`] budgets.
/// * `cdf_env` - Mutable [`CdfEnv`] shared across included copybooks.
/// * `source_map` - Mutable [`SourceMap`].
/// * `sink` - Mutable [`DiagnosticSink`].
/// * `active_stack` - Canonical path stack used to detect recursive `COPY` cycles.
///
/// # Errors
///
/// Returns [`CoblangError::ResourceLimitExceeded`] if `COPY` depth or expanded token budgets trip.
pub fn expand_copy_tokens_with_limits(
    tokens: &[PpToken],
    caller_dir: &Path,
    cfg_and_limits: (&PreprocessConfig<'_>, PreprocessLimits),
    cdf_env: &mut CdfEnv,
    source_map: &mut SourceMap,
    sink: &mut DiagnosticSink,
    active_stack: &mut Vec<String>,
) -> Result<Vec<PpToken>> {
    let (cfg, limits) = cfg_and_limits;
    let mut state = CopyExpandState {
        cfg,
        limits,
        emitted_tokens: 0,
        cdf_env,
        source_map,
        sink,
        active_stack,
    };
    expand_copy_inner(tokens, caller_dir, &mut state)
}

/// Inner recursive `COPY` loop operating on [`CopyExpandState`].
///
/// # Arguments
///
/// * `tokens` - Token slice to scan.
/// * `caller_dir` - Caller directory.
/// * `state` - Mutable expansion state.
fn expand_copy_inner(
    tokens: &[PpToken],
    caller_dir: &Path,
    state: &mut CopyExpandState<'_, '_>,
) -> Result<Vec<PpToken>> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut idx = 0_usize;

    while let Some(tok) = tokens.get(idx) {
        if tok.kind == PpTokenKind::Word && tok.text.eq_ignore_ascii_case("COPY") {
            let (next_idx, expanded) =
                expand_single_copy_stmt(tokens, idx, tok.span, caller_dir, state)?;
            out.extend(expanded);
            idx = next_idx;
        } else {
            state.emitted_tokens = state.emitted_tokens.saturating_add(1);
            if state.emitted_tokens > state.limits.max_expanded_tokens {
                return Err(CoblangError::ResourceLimitExceeded {
                    resource: ResourceLimitKind::ExpandedTokens,
                    limit: state.limits.max_expanded_tokens,
                    observed: state.emitted_tokens,
                });
            }
            out.push(tok.clone());
            idx += 1;
        }
    }
    Ok(out)
}

/// Parsed header of a `COPY <name> [IN/OF <lib>] [SUPPRESS [PRINTING]] [REPLACING ...]` statement.
struct ParsedCopyStmt<'a> {
    /// Name token of the requested copybook.
    name_tok: &'a PpToken,
    /// Optional `IN` / `OF` library qualifier.
    library: Option<&'a str>,
    /// Compiled `REPLACING` rules.
    rules: Vec<crate::pseudo_text::ReplacementRule>,
}

/// Parses the body tokens of a `COPY` statement (between `COPY` and `.`).
///
/// # Arguments
///
/// * `stmt_body` - Tokens inside the `COPY` statement.
/// * `copy_span` - Span of the `COPY` keyword.
/// * `sink` - Diagnostic sink.
#[must_use]
fn parse_copy_header<'a>(
    stmt_body: &'a [PpToken],
    copy_span: SourceSpan,
    sink: &mut DiagnosticSink,
) -> Option<ParsedCopyStmt<'a>> {
    let Some(name_tok) = stmt_body.first() else {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0001Preprocessor,
            Some(copy_span),
            String::from("missing copybook name after `COPY`"),
        ));
        return None;
    };

    let mut cursor = 1_usize;
    let mut library: Option<&str> = None;
    if let Some(kw) = stmt_body.get(cursor)
        && (kw.text.eq_ignore_ascii_case("IN") || kw.text.eq_ignore_ascii_case("OF"))
    {
        let Some(lib_tok) = stmt_body.get(cursor + 1) else {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0001Preprocessor,
                Some(kw.span),
                String::from("missing library name after `IN`/`OF` in `COPY`"),
            ));
            return None;
        };
        library = Some(lib_tok.text.as_str());
        cursor += 2;
    }

    if stmt_body
        .get(cursor)
        .is_some_and(|t| t.text.eq_ignore_ascii_case("SUPPRESS"))
    {
        cursor += 1;
        if stmt_body
            .get(cursor)
            .is_some_and(|t| t.text.eq_ignore_ascii_case("PRINTING"))
        {
            cursor += 1;
        }
    }

    let rules = if let Some(rep_kw) = stmt_body.get(cursor) {
        if rep_kw.text.eq_ignore_ascii_case("REPLACING") {
            let clause_slice = stmt_body.get(cursor + 1..).unwrap_or(&[]);
            parse_replacing_clauses(clause_slice, rep_kw.span, sink)
        } else {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0001Preprocessor,
                Some(rep_kw.span),
                format!("unexpected token `{}` in `COPY` statement", rep_kw.text),
            ));
            Vec::new()
        }
    } else {
        Vec::new()
    };

    Some(ParsedCopyStmt {
        name_tok,
        library,
        rules,
    })
}

/// Parses and expands one `COPY name [OF/IN lib] [SUPPRESS [PRINTING]] [REPLACING ...].` statement.
///
/// # Arguments
///
/// * `tokens` - Full token slice.
/// * `copy_idx` - Index of the `COPY` keyword.
/// * `copy_span` - Span of the `COPY` keyword.
/// * `caller_dir` - Caller directory for relative lookup.
/// * `state` - Mutable expansion state.
fn expand_single_copy_stmt(
    tokens: &[PpToken],
    copy_idx: usize,
    copy_span: SourceSpan,
    caller_dir: &Path,
    state: &mut CopyExpandState<'_, '_>,
) -> Result<(usize, Vec<PpToken>)> {
    let mut end_idx = copy_idx + 1;
    while let Some(t) = tokens.get(end_idx) {
        if t.kind == PpTokenKind::Period {
            break;
        }
        end_idx += 1;
    }
    if tokens.get(end_idx).is_none() {
        state.sink.emit(Diagnostic::error(
            DiagnosticCode::E0001Preprocessor,
            Some(copy_span),
            String::from("unterminated `COPY` statement (missing `.`)"),
        ));
        return Ok((tokens.len(), Vec::new()));
    }

    let stmt_body = tokens.get(copy_idx + 1..end_idx).unwrap_or(&[]);
    let Some(hdr) = parse_copy_header(stmt_body, copy_span, state.sink) else {
        return Ok((end_idx + 1, Vec::new()));
    };

    let (resolved_path, copy_text) = match state.cfg.copybook_resolver.resolve_copybook(
        &hdr.name_tok.text,
        hdr.library,
        caller_dir,
    ) {
        Ok(pair) => pair,
        Err(err) => {
            state.sink.emit(Diagnostic::error(
                DiagnosticCode::E0001Preprocessor,
                Some(hdr.name_tok.span),
                err.to_string(),
            ));
            return Ok((end_idx + 1, Vec::new()));
        }
    };

    let display_key = resolved_path.to_string_lossy().to_string();
    let canonical_key = std::fs::canonicalize(&resolved_path)
        .map_or_else(|_| display_key.clone(), |p| p.to_string_lossy().to_string());
    if state.active_stack.contains(&canonical_key) {
        state.sink.emit(Diagnostic::error(
            DiagnosticCode::E0001Preprocessor,
            Some(hdr.name_tok.span),
            format!("recursive `COPY` of `{display_key}`"),
        ));
        return Ok((end_idx + 1, Vec::new()));
    }
    if state.active_stack.len() > state.limits.max_copy_depth {
        return Err(CoblangError::ResourceLimitExceeded {
            resource: ResourceLimitKind::CopyDepth,
            limit: state.limits.max_copy_depth,
            observed: state.active_stack.len(),
        });
    }

    let exp_id = state.source_map.record_expansion(
        copy_span.expansion_id,
        copy_span,
        ExpansionKind::Copybook,
    )?;
    let cpy_file_id = state.source_map.add_file(display_key, copy_text.clone())?;
    let cpy_lines = normalize_text(
        cpy_file_id,
        &copy_text,
        state.cfg.initial_format,
        state.cfg.dialect_config,
        state.cfg.debug_policy,
        exp_id,
        state.sink,
    );
    let cpy_cdf = cdf::process_cdf_lines(&cpy_lines, state.cdf_env, state.sink);
    let cpy_tokens = tokenize_normalized_lines(&cpy_cdf, state.sink);
    let replaced = apply_replacement_rules(&cpy_tokens, &hdr.rules);

    let child_dir = resolved_path.parent().unwrap_or(caller_dir);
    state.active_stack.push(canonical_key);
    let nested_expanded = expand_copy_inner(&replaced, child_dir, state)?;
    state.active_stack.pop();

    Ok((end_idx + 1, nested_expanded))
}

/// Strips optional surrounding single or double quotes from a copybook or library token.
///
/// # Arguments
///
/// * `raw` - Raw token text.
#[must_use]
fn unquote(raw: &str) -> &str {
    raw.trim_matches(DQ).trim_matches(SQ)
}
