//! Phase-2 `REPLACE`, `REPLACE ALSO`, and `REPLACE [LAST] OFF` statement processor.

use coblang_common::{CoblangError, ResourceLimitKind, Result};
use coblang_diag::{Diagnostic, DiagnosticCode, DiagnosticSink, ExpansionKind, SourceMap};

use crate::PreprocessLimits;
use crate::pseudo_text::{
    PpToken, PpTokenKind, ReplacementRule, apply_replacement_rules, parse_replacing_clauses,
};

/// Executes Phase-2 `REPLACE` text-manipulation statements across `tokens` after `COPY` expansion.
///
/// # Arguments
///
/// * `tokens` - `COPY`-expanded [`PpToken`] stream.
/// * `source_map` - Mutable [`SourceMap`] recording [`ExpansionKind::ReplaceStatement`] frames.
/// * `sink` - Mutable [`DiagnosticSink`] receiving malformed `REPLACE` diagnostics.
///
/// # Errors
///
/// Propagates [`CoblangError`] if `SourceMap` or [`PreprocessLimits`] capacity is exceeded.
pub fn apply_replace_statements(
    tokens: &[PpToken],
    source_map: &mut SourceMap,
    sink: &mut DiagnosticSink,
) -> Result<Vec<PpToken>> {
    apply_replace_statements_with_limit(
        tokens,
        PreprocessLimits::default().max_expanded_tokens,
        source_map,
        sink,
    )
}

/// Executes Phase-2 `REPLACE` statements enforcing an explicit `max_expanded_tokens` budget.
///
/// # Arguments
///
/// * `tokens` - `COPY`-expanded [`PpToken`] stream.
/// * `max_expanded_tokens` - Maximum permitted substituted [`PpToken`] count.
/// * `source_map` - Mutable [`SourceMap`] recording [`ExpansionKind::ReplaceStatement`] frames.
/// * `sink` - Mutable [`DiagnosticSink`] receiving malformed `REPLACE` diagnostics.
///
/// # Errors
///
/// Returns [`CoblangError::ResourceLimitExceeded`] if substituted output exceeds `max_expanded_tokens`.
pub fn apply_replace_statements_with_limit(
    tokens: &[PpToken],
    max_expanded_tokens: usize,
    source_map: &mut SourceMap,
    sink: &mut DiagnosticSink,
) -> Result<Vec<PpToken>> {
    let mut rule_stack: Vec<(Vec<ReplacementRule>, coblang_common::ExpansionId)> = Vec::new();
    let mut out = Vec::with_capacity(tokens.len());
    let mut pending_segment = Vec::new();
    let mut idx = 0_usize;

    while let Some(tok) = tokens.get(idx) {
        if tok.kind == PpTokenKind::Word && tok.text.eq_ignore_ascii_case("REPLACE") {
            flush_segment(
                &mut pending_segment,
                &rule_stack,
                max_expanded_tokens,
                &mut out,
            )?;
            idx =
                handle_replace_directive(tokens, idx, tok.span, &mut rule_stack, source_map, sink)?;
        } else {
            pending_segment.push(tok.clone());
            idx += 1;
        }
    }
    flush_segment(
        &mut pending_segment,
        &rule_stack,
        max_expanded_tokens,
        &mut out,
    )?;
    Ok(out)
}

/// Flushes accumulated non-directive tokens through the active `REPLACE` stack into `out`.
///
/// # Arguments
///
/// * `segment` - Buffered non-`REPLACE` tokens (cleared upon return).
/// * `rule_stack` - Stack of active `(rules, expansion_id)` frames (`REPLACE ALSO` pushes, `REPLACE` replaces).
/// * `max_expanded_tokens` - Maximum total output tokens allowed.
/// * `out` - Destination token vector.
fn flush_segment(
    segment: &mut Vec<PpToken>,
    rule_stack: &[(Vec<ReplacementRule>, coblang_common::ExpansionId)],
    max_expanded_tokens: usize,
    out: &mut Vec<PpToken>,
) -> Result<()> {
    if segment.is_empty() {
        return Ok(());
    }
    let mut current = core::mem::take(segment);
    for (rules, exp_id) in rule_stack {
        let mut next = apply_replacement_rules(&current, rules);
        if out.len().saturating_add(next.len()) > max_expanded_tokens {
            return Err(CoblangError::ResourceLimitExceeded {
                resource: ResourceLimitKind::ExpandedTokens,
                limit: max_expanded_tokens,
                observed: out.len().saturating_add(next.len()),
            });
        }
        for tok in &mut next {
            tok.span.expansion_id = *exp_id;
        }
        current = next;
    }
    if out.len().saturating_add(current.len()) > max_expanded_tokens {
        return Err(CoblangError::ResourceLimitExceeded {
            resource: ResourceLimitKind::ExpandedTokens,
            limit: max_expanded_tokens,
            observed: out.len().saturating_add(current.len()),
        });
    }
    out.extend(current);
    Ok(())
}

/// Parses one `REPLACE [ALSO] ... .` or `REPLACE [LAST] OFF.` statement starting at `replace_idx`.
///
/// # Arguments
///
/// * `tokens` - Full token stream.
/// * `replace_idx` - Index of the `REPLACE` keyword token.
/// * `replace_span` - Span of the `REPLACE` keyword token.
/// * `rule_stack` - Active `REPLACE` rule stack.
/// * `source_map` - Mutable [`SourceMap`].
/// * `sink` - Diagnostic sink.
///
/// # Errors
///
/// Propagates [`coblang_common::CoblangError`] from `source_map.record_expansion`.
fn handle_replace_directive(
    tokens: &[PpToken],
    replace_idx: usize,
    replace_span: coblang_common::SourceSpan,
    rule_stack: &mut Vec<(Vec<ReplacementRule>, coblang_common::ExpansionId)>,
    source_map: &mut SourceMap,
    sink: &mut DiagnosticSink,
) -> Result<usize> {
    let mut end_idx = replace_idx + 1;
    while let Some(t) = tokens.get(end_idx) {
        if t.kind == PpTokenKind::Period {
            break;
        }
        end_idx += 1;
    }
    if tokens.get(end_idx).is_none() {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0001Preprocessor,
            Some(replace_span),
            String::from("unterminated `REPLACE` statement (missing `.`)"),
        ));
        return Ok(tokens.len());
    }

    let body = tokens.get(replace_idx + 1..end_idx).unwrap_or(&[]);
    if body.len() == 1
        && body
            .first()
            .is_some_and(|t| t.text.eq_ignore_ascii_case("OFF"))
    {
        rule_stack.clear();
        return Ok(end_idx + 1);
    }
    if body.len() == 2
        && body
            .first()
            .is_some_and(|t| t.text.eq_ignore_ascii_case("LAST"))
        && body
            .get(1)
            .is_some_and(|t| t.text.eq_ignore_ascii_case("OFF"))
    {
        let _ = rule_stack.pop();
        return Ok(end_idx + 1);
    }

    let (is_also, clause_tokens) = if body
        .first()
        .is_some_and(|t| t.text.eq_ignore_ascii_case("ALSO"))
    {
        (true, body.get(1..).unwrap_or(&[]))
    } else {
        (false, body)
    };

    let rules = parse_replacing_clauses(clause_tokens, replace_span, sink);
    let exp_id = source_map.record_expansion(
        replace_span.expansion_id,
        replace_span,
        ExpansionKind::ReplaceStatement,
    )?;
    if !is_also {
        rule_stack.clear();
    }
    if !rules.is_empty() {
        rule_stack.push((rules, exp_id));
    }
    Ok(end_idx + 1)
}
