//! COBOL reference-format scanner, `COPY`/`REPLACE` text manipulation, CDF directives, and source listings.

#![deny(clippy::indexing_slicing)]

use std::path::Path;

use coblang_common::{ExpansionId, FileId, Result, SourceFormat, SourceSpan};
use coblang_diag::{DiagnosticSink, DialectConfig, SourceMap};

/// Compiler Directing Facility (`>>DEFINE`, `>>IF`, `>>EVALUATE`, `>>TURN`, `*CBL`) processor.
pub mod cdf;
/// `COPY ... IN/OF ... REPLACING` resolver and recursive expander.
pub mod copy;
/// Fixed, Free, and Auto reference-format line normalizer and continuation joiner.
pub mod format;
/// `-E` preprocessed source emitter and `-t` / `-T` formatted compiler listing generator.
pub mod listing;
/// Preprocessor text-word tokenizer and `==...==` pseudo-text replacement engine.
pub mod pseudo_text;
/// Phase-2 `REPLACE` / `REPLACE ALSO` / `REPLACE OFF` statement engine.
pub mod replace;

pub use cdf::{CdfEnv, CdfValue, DpcInDataRecord, TurnCheckingState, TurnDirectiveRecord};
pub use copy::{CopybookResolver, FilesystemCopybookResolver, InMemoryCopybookResolver};
pub use format::{DebugLinePolicy, LineIndicator, MarginZone, NormalizedLine, normalize_file};
pub use listing::{
    HeaderVisibility, ListingOptions, ListingSymbols, ListingWidth, MessageVisibility,
    emit_preprocessed_source, render_compiler_listing,
};
pub use pseudo_text::{
    PpToken, PpTokenKind, ReplacementPatternKind, ReplacementRule, SpacingHint,
    apply_replacement_rules, parse_replacing_clauses, tokenize_normalized_lines,
};
pub use replace::{apply_replace_statements, apply_replace_statements_with_limit};

/// Deterministic resource ceilings protecting the preprocessor against recursive or exponential bombs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreprocessLimits {
    /// Maximum nested `COPY` inclusion depth (`64` by default).
    pub max_copy_depth: usize,
    /// Maximum expanded [`PpToken`] count produced by `COPY` or `REPLACE` (`500_000` by default).
    pub max_expanded_tokens: usize,
}

impl Default for PreprocessLimits {
    /// Returns production preprocessor safeguard ceilings (`64` depth, `500_000` tokens).
    fn default() -> Self {
        Self::new(64, 500_000)
    }
}

impl PreprocessLimits {
    /// Constructs explicit [`PreprocessLimits`].
    ///
    /// # Arguments
    ///
    /// * `max_copy_depth` - Maximum nested `COPY` depth.
    /// * `max_expanded_tokens` - Maximum total expanded token count.
    #[must_use]
    pub const fn new(max_copy_depth: usize, max_expanded_tokens: usize) -> Self {
        Self {
            max_copy_depth,
            max_expanded_tokens,
        }
    }
}

/// Complete configuration for running the multi-phase COBOL preprocessor on a root file.
pub struct PreprocessConfig<'a> {
    /// Initial source reference format (`Fixed`, `Free`, or `Auto`).
    pub initial_format: SourceFormat,
    /// Active dialect configuration profile.
    pub dialect_config: &'a DialectConfig,
    /// Policy governing column-7 `'D'` debugging lines.
    pub debug_policy: DebugLinePolicy,
    /// Pre-populated CDF variable definitions (`-Dname=val`).
    pub initial_defines: &'a [(String, CdfValue)],
    /// Copybook lookup implementation.
    pub copybook_resolver: &'a dyn CopybookResolver,
}

/// Output produced by [`preprocess_source_file`] across all preprocessor phases.
#[derive(Debug, Clone)]
pub struct PreprocessedUnit {
    /// Root [`FileId`] inside `source_map`.
    pub root_file_id: FileId,
    /// End-of-file byte offset of `source_text` inside `root_file_id`.
    pub root_eof_offset: coblang_common::ByteOffset,
    /// Normalized lines of the root source file (used for `-t` / `-T` source listings).
    pub root_lines: Vec<NormalizedLine>,
    /// Final expanded token stream after CDF, `COPY`, and `REPLACE` processing.
    pub tokens: Vec<PpToken>,
    /// Final CDF environment state (including `>>TURN` directives).
    pub cdf_env: CdfEnv,
}

/// Runs reference-format normalization, CDF filtering, `COPY` expansion, and `REPLACE` on `source_text`.
///
/// # Arguments
///
/// * `path` - Display path of the root source file.
/// * `caller_dir` - Directory containing the root source file for relative `COPY` lookup.
/// * `source_text` - Decoded UTF-8 source text.
/// * `cfg` - Preprocessor configuration and copybook resolver.
/// * `source_map` - Mutable [`SourceMap`] recording files and expansion frames.
/// * `sink` - Mutable [`DiagnosticSink`] collecting preprocessor errors and warnings.
///
/// # Errors
///
/// Propagates [`coblang_common::CoblangError`] if `SourceMap` or [`PreprocessLimits`] capacity is exceeded.
pub fn preprocess_source_file(
    path: &str,
    caller_dir: &Path,
    source_text: &str,
    cfg: &PreprocessConfig<'_>,
    source_map: &mut SourceMap,
    sink: &mut DiagnosticSink,
) -> Result<PreprocessedUnit> {
    preprocess_source_file_with_limits(
        path,
        caller_dir,
        source_text,
        cfg,
        PreprocessLimits::default(),
        source_map,
        sink,
    )
}

/// Runs all preprocessor phases under explicit [`PreprocessLimits`] budgets.
///
/// # Arguments
///
/// * `path` - Display path of the root source file.
/// * `caller_dir` - Directory containing the root source file for relative `COPY` lookup.
/// * `source_text` - Decoded UTF-8 source text.
/// * `cfg` - Preprocessor configuration and copybook resolver.
/// * `limits` - Explicit [`PreprocessLimits`] budgets.
/// * `source_map` - Mutable [`SourceMap`] recording files and expansion frames.
/// * `sink` - Mutable [`DiagnosticSink`] collecting preprocessor errors and warnings.
///
/// # Errors
///
/// Returns [`coblang_common::CoblangError::ResourceLimitExceeded`] if `COPY` depth or expanded
/// token budgets are exceeded, or propagates `SourceMap` capacity errors.
pub fn preprocess_source_file_with_limits(
    path: &str,
    caller_dir: &Path,
    source_text: &str,
    cfg: &PreprocessConfig<'_>,
    limits: PreprocessLimits,
    source_map: &mut SourceMap,
    sink: &mut DiagnosticSink,
) -> Result<PreprocessedUnit> {
    let root_file_id = source_map.add_file(String::from(path), String::from(source_text))?;
    let mut cdf_env = CdfEnv::with_defines(cfg.initial_defines);
    let root_lines = format::normalize_text(
        root_file_id,
        source_text,
        cfg.initial_format,
        cfg.dialect_config,
        cfg.debug_policy,
        ExpansionId::ROOT,
        sink,
    );
    let cdf_filtered = cdf::process_cdf_lines(&root_lines, &mut cdf_env, sink);
    let initial_tokens = tokenize_normalized_lines(&cdf_filtered, sink);
    let root_candidate = caller_dir.join(path);
    let root_key = std::fs::canonicalize(&root_candidate)
        .map_or_else(|_| String::from(path), |p| p.to_string_lossy().to_string());
    let mut active_copy_stack = vec![root_key];
    let copy_expanded = copy::expand_copy_tokens_with_limits(
        &initial_tokens,
        caller_dir,
        (cfg, limits),
        &mut cdf_env,
        source_map,
        sink,
        &mut active_copy_stack,
    )?;
    let final_tokens = apply_replace_statements_with_limit(
        &copy_expanded,
        limits.max_expanded_tokens,
        source_map,
        sink,
    )?;
    let _ = SourceSpan::new;
    let root_eof_offset =
        coblang_common::ByteOffset::new(u32::try_from(source_text.len()).unwrap_or(0));
    Ok(PreprocessedUnit {
        root_file_id,
        root_eof_offset,
        root_lines,
        tokens: final_tokens,
        cdf_env,
    })
}
