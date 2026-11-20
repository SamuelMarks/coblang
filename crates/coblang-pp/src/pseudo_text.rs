//! Preprocessor text-word tokenizer and `==...==` pseudo-text replacement engine.

use coblang_common::{ByteLength, ByteOffset, SourceSpan};
use coblang_diag::{Diagnostic, DiagnosticCode, DiagnosticSink};

use crate::format::{MarginZone, NormalizedLine};

/// Double-quote character (`0x22`).
const DQ: char = '"';
/// Single-quote character (`0x27`).
const SQ: char = '\u{0027}';

/// Classification of a preprocessor token for ANSI-85 / ISO 2023 text manipulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PpTokenKind {
    /// COBOL word, identifier, keyword, or numeric literal.
    Word,
    /// Quoted string literal (`"..."` or `'...'`, optionally prefixed by `X`, `Z`, `N`, `U`, `B`).
    StringLiteral,
    /// Pseudo-text delimiter `==`.
    PseudoDelimiter,
    /// Statement/entry separator period `.`.
    Period,
    /// Comma or semicolon separator `,` / `;`.
    SeparatorPunct,
    /// Left parenthesis `(`.
    LParen,
    /// Right parenthesis `)`.
    RParen,
    /// Reference-modification colon `:`.
    Colon,
}

/// Formatting whitespace hint preserved before a [`PpToken`] for `-E` output reconstruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpacingHint {
    /// Immediately adjacent to preceding token (`FOO(1)`).
    Tight,
    /// Separated by a single space (`MOVE A TO B`).
    Space,
    /// Starts on a new logical line.
    Newline,
}

/// Single preprocessor token carrying text, kind, margin zone, spacing hint, and [`SourceSpan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PpToken {
    /// Raw token text.
    pub text: String,
    /// Token classification.
    pub kind: PpTokenKind,
    /// Margin zone where the token originated.
    pub zone: MarginZone,
    /// Formatting whitespace preceding this token.
    pub spacing: SpacingHint,
    /// Byte-exact span in [`coblang_diag::SourceMap`].
    pub span: SourceSpan,
}

/// Matching mode for a `COPY ... REPLACING` or `REPLACE` substitution rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReplacementPatternKind {
    /// Match a sequence of full COBOL text-words (`==A OF B==` or `OLD-VAR`).
    FullSequence,
    /// Match and replace a prefix of a single COBOL word (`REPLACING LEADING ==TAG-== BY ==WS-==`).
    LeadingWordPrefix,
    /// Match and replace a suffix of a single COBOL word (`REPLACING TRAILING ==-IN== BY ==-OUT==`).
    TrailingWordSuffix,
}

/// Compiled `REPLACING` / `REPLACE` substitution rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacementRule {
    /// Full-sequence vs `LEADING` / `TRAILING` partial-word replacement mode.
    pub kind: ReplacementPatternKind,
    /// Canonical uppercase pattern tokens (`len >= 1`).
    pub pattern: Vec<String>,
    /// Replacement token sequence inserted on match (may be empty for `BY ====`).
    pub replacement: Vec<PpToken>,
}

/// Tokenizes [`NormalizedLine`] records into [`PpToken`]s for `COPY` and `REPLACE` manipulation.
///
/// # Arguments
///
/// * `lines` - Normalized source lines (with comments already stripped).
/// * `sink` - [`DiagnosticSink`] receiving unterminated literal diagnostics.
#[must_use]
pub fn tokenize_normalized_lines(
    lines: &[NormalizedLine],
    sink: &mut DiagnosticSink,
) -> Vec<PpToken> {
    let mut tokens = Vec::new();
    let mut quote_mode = QuoteDiagMode::ReportUnclosed;
    for line in lines {
        if line.indicator == crate::format::LineIndicator::VendorDirectiveDollar
            || line.code.trim().is_empty()
        {
            continue;
        }
        tokenize_single_line(
            &line.code,
            line.zone,
            line.span,
            &mut quote_mode,
            &mut tokens,
            sink,
        );
    }
    tokens
}

/// Controls whether unclosed quotes on a normalized line emit lexical errors or are inside an ID comment paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteDiagMode {
    /// Normal COBOL source: report unclosed string literals.
    ReportUnclosed,
    /// Inside `AUTHOR.`..`REMARKS.` free-form comment entry: tolerate English apostrophes/quotes.
    IdCommentEntry,
}

/// Tokenizes a single line's code payload into `out`.
///
/// # Arguments
///
/// * `code` - Code payload of the line.
/// * `line_zone` - Line margin zone (`AreaA`, `AreaB`, or `FreeFlow`).
/// * `base_span` - Line [`SourceSpan`].
/// * `quote_mode` - Active [`QuoteDiagMode`] tracking `IDENTIFICATION DIVISION` comment paragraphs.
/// * `out` - Output token vector.
/// * `sink` - Diagnostic sink.
fn tokenize_single_line(
    code: &str,
    line_zone: MarginZone,
    base_span: SourceSpan,
    quote_mode: &mut QuoteDiagMode,
    out: &mut Vec<PpToken>,
    sink: &mut DiagnosticSink,
) {
    let chars: Vec<(usize, char)> = code.char_indices().collect();
    let mut idx = 0_usize;
    let mut first_on_line = true;
    let mut had_space = false;

    while let Some(&(byte_pos, ch)) = chars.get(idx) {
        if ch.is_ascii_whitespace() {
            had_space = true;
            idx += 1;
            continue;
        }
        let spacing = if first_on_line {
            SpacingHint::Newline
        } else if had_space {
            SpacingHint::Space
        } else {
            SpacingHint::Tight
        };
        let zone = if first_on_line {
            line_zone
        } else if line_zone == MarginZone::FreeFlow {
            MarginZone::FreeFlow
        } else {
            MarginZone::AreaB
        };
        first_on_line = false;
        had_space = false;

        let loc = TokenLocCtx {
            zone,
            spacing,
            base_span,
            byte_pos,
        };
        let prev_len = out.len();
        idx = lex_one_pp_token(code, &chars, idx, loc, *quote_mode, out, sink);
        update_id_comment_mode(out, prev_len, base_span, quote_mode);
    }
}

/// Updates [`QuoteDiagMode`] when entering or leaving an `IDENTIFICATION DIVISION` comment paragraph.
///
/// # Arguments
///
/// * `out` - Accumulated preprocessor tokens.
/// * `prev_len` - Length of `out` prior to appending the latest token.
/// * `base_span` - Fallback line span.
/// * `mode` - Mutable [`QuoteDiagMode`] state.
fn update_id_comment_mode(
    out: &[PpToken],
    prev_len: usize,
    base_span: SourceSpan,
    mode: &mut QuoteDiagMode,
) {
    let fallback = PpToken {
        text: String::new(),
        kind: PpTokenKind::SeparatorPunct,
        zone: MarginZone::AreaB,
        spacing: SpacingHint::Tight,
        span: base_span,
    };
    let latest = out.get(prev_len).unwrap_or(&fallback);
    if *mode == QuoteDiagMode::IdCommentEntry
        && latest.kind == PpTokenKind::Word
        && (latest.zone == MarginZone::AreaA
            || (latest.zone == MarginZone::FreeFlow && latest.spacing == SpacingHint::Newline))
        && is_id_comment_exit_header(&latest.text)
    {
        *mode = QuoteDiagMode::ReportUnclosed;
    }
    if *mode == QuoteDiagMode::ReportUnclosed
        && latest.kind == PpTokenKind::Period
        && let Some(prev) = prev_len.checked_sub(1).and_then(|p| out.get(p))
        && prev.kind == PpTokenKind::Word
        && (prev.zone == MarginZone::AreaA
            || (prev.zone == MarginZone::FreeFlow && prev.spacing == SpacingHint::Newline))
        && is_id_comment_paragraph_name(&prev.text)
    {
        *mode = QuoteDiagMode::IdCommentEntry;
    }
}

/// Checks if `word` names one of the 6 COBOL-85 `IDENTIFICATION DIVISION` comment paragraphs.
///
/// # Arguments
///
/// * `word` - Candidate token spelling.
#[must_use]
fn is_id_comment_paragraph_name(word: &str) -> bool {
    matches!(
        word.to_ascii_uppercase().as_str(),
        "AUTHOR" | "INSTALLATION" | "DATE-WRITTEN" | "DATE-COMPILED" | "SECURITY" | "REMARKS"
    )
}

/// Checks if `word` is an Area-A header that exits an `IDENTIFICATION DIVISION` comment paragraph.
///
/// # Arguments
///
/// * `word` - Candidate token spelling.
#[must_use]
fn is_id_comment_exit_header(word: &str) -> bool {
    is_id_comment_paragraph_name(word)
        || matches!(
            word.to_ascii_uppercase().as_str(),
            "PROGRAM-ID"
                | "FUNCTION-ID"
                | "CLASS-ID"
                | "INTERFACE-ID"
                | "FACTORY"
                | "OBJECT"
                | "METHOD-ID"
                | "OPTIONS"
                | "ENVIRONMENT"
                | "DATA"
                | "PROCEDURE"
                | "IDENTIFICATION"
                | "ID"
                | "END"
        )
}

/// Location and spacing metadata for a single token being scanned on a normalized line.
#[derive(Debug, Clone, Copy)]
struct TokenLocCtx {
    /// Margin zone of the token.
    zone: MarginZone,
    /// Preceding whitespace hint.
    spacing: SpacingHint,
    /// Span of the enclosing line.
    base_span: SourceSpan,
    /// Starting byte offset within the line.
    byte_pos: usize,
}

/// Lexes a single non-whitespace [`PpToken`] starting at `chars[idx]` and returns the next char index.
///
/// # Arguments
///
/// * `code` - Full line string.
/// * `chars` - `(byte_offset, char)` pairs of `code`.
/// * `idx` - Starting index in `chars`.
/// * `loc` - Token location and spacing metadata.
/// * `quote_mode` - Active [`QuoteDiagMode`] governing unclosed-quote error emission.
/// * `out` - Output token list.
/// * `sink` - Diagnostic sink.
fn lex_one_pp_token(
    code: &str,
    chars: &[(usize, char)],
    idx: usize,
    loc: TokenLocCtx,
    quote_mode: QuoteDiagMode,
    out: &mut Vec<PpToken>,
    sink: &mut DiagnosticSink,
) -> usize {
    let ch = chars.get(idx).map_or(' ', |p| p.1);

    // Pseudo-text delimiter `==`.
    if ch == '=' && chars.get(idx + 1).map(|p| p.1) == Some('=') {
        out.push(make_tok("==", PpTokenKind::PseudoDelimiter, loc, 2));
        return idx + 2;
    }

    // Single-character punctuation separators.
    let single_kind = match ch {
        '(' => Some(PpTokenKind::LParen),
        ')' => Some(PpTokenKind::RParen),
        ':' => Some(PpTokenKind::Colon),
        ',' | ';' => Some(PpTokenKind::SeparatorPunct),
        '.' => {
            let next_ch = chars.get(idx + 1).map(|p| p.1);
            if next_ch.is_none_or(|c| c.is_ascii_whitespace()) {
                Some(PpTokenKind::Period)
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(kind) = single_kind {
        let slice = code
            .get(loc.byte_pos..loc.byte_pos + ch.len_utf8())
            .unwrap_or(".");
        out.push(make_tok(slice, kind, loc, ch.len_utf8()));
        return idx + 1;
    }

    // Quoted literal or prefixed literal (`X"..."`, `N'...'`, `Z"..."`, `NX"..."`).
    if ch == DQ || ch == SQ {
        return lex_quoted_literal(code, chars, idx, loc, quote_mode, out, sink);
    }
    if is_literal_prefix(ch, chars, idx) {
        let q_idx = if chars.get(idx + 1).is_some_and(|p| p.1 == DQ || p.1 == SQ) {
            idx + 1
        } else {
            idx + 2
        };
        return lex_quoted_literal(code, chars, q_idx, loc, quote_mode, out, sink);
    }

    // General COBOL word / numeric token.
    let mut end_idx = idx + 1;
    while let Some(&(_, c)) = chars.get(end_idx) {
        if c.is_ascii_whitespace() || matches!(c, '(' | ')' | ':' | ',' | ';' | DQ | SQ) {
            break;
        }
        if c == '=' && chars.get(end_idx + 1).map(|p| p.1) == Some('=') {
            break;
        }
        if c == '.'
            && chars
                .get(end_idx + 1)
                .map(|p| p.1)
                .is_none_or(|n| n.is_ascii_whitespace())
        {
            break;
        }
        end_idx += 1;
    }
    let end_byte = chars.get(end_idx).map_or(code.len(), |p| p.0);
    let text = code.get(loc.byte_pos..end_byte).unwrap_or("");
    out.push(make_tok(
        text,
        PpTokenKind::Word,
        loc,
        end_byte - loc.byte_pos,
    ));
    end_idx
}

/// Checks if `chars[idx..]` begins a `X"`, `Z"`, `N"`, `U"`, `B"`, `H"`, `NX"`, `UX"`, or `BX"` literal.
///
/// # Arguments
///
/// * `first_ch` - Character at `chars[idx]`.
/// * `chars` - Character slice.
/// * `idx` - Candidate prefix index.
#[must_use]
fn is_literal_prefix(first_ch: char, chars: &[(usize, char)], idx: usize) -> bool {
    let c0 = first_ch.to_ascii_uppercase();
    let Some(c1) = chars.get(idx + 1).map(|p| p.1) else {
        return false;
    };
    if matches!(c0, 'X' | 'Z' | 'N' | 'U' | 'B' | 'H') && (c1 == DQ || c1 == SQ) {
        return true;
    }
    let c1_up = c1.to_ascii_uppercase();
    let c2 = chars.get(idx + 2).map(|p| p.1);
    matches!((c0, c1_up), ('N' | 'U' | 'B', 'X')) && matches!(c2, Some(DQ | SQ))
}

/// Lexes a string literal whose opening quote is at `quote_idx`.
///
/// # Arguments
///
/// * `code` - Full line string.
/// * `chars` - Character index list.
/// * `quote_idx` - Index of opening `"` or `'`.
/// * `loc` - Token location metadata.
/// * `quote_mode` - Active [`QuoteDiagMode`] governing unclosed-quote error emission.
/// * `out` - Output token list.
/// * `sink` - Diagnostic sink.
fn lex_quoted_literal(
    code: &str,
    chars: &[(usize, char)],
    quote_idx: usize,
    loc: TokenLocCtx,
    quote_mode: QuoteDiagMode,
    out: &mut Vec<PpToken>,
    sink: &mut DiagnosticSink,
) -> usize {
    let delim = chars.get(quote_idx).map_or(DQ, |p| p.1);
    let mut i = quote_idx + 1;
    let mut closed = false;
    while let Some(&(_, c)) = chars.get(i) {
        if c == delim {
            if chars.get(i + 1).map(|p| p.1) == Some(delim) {
                i += 2;
                continue;
            }
            i += 1;
            closed = true;
            break;
        }
        i += 1;
    }
    let end_byte = chars.get(i).map_or(code.len(), |p| p.0);
    let tok_span = sub_span(
        loc.base_span,
        loc.byte_pos,
        end_byte.saturating_sub(loc.byte_pos),
    );
    if !closed && quote_mode == QuoteDiagMode::ReportUnclosed {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0002Lexical,
            Some(tok_span),
            format!("unterminated string literal (missing closing `{delim}`)"),
        ));
    }
    let text = code.get(loc.byte_pos..end_byte).unwrap_or("");
    out.push(PpToken {
        text: String::from(text),
        kind: PpTokenKind::StringLiteral,
        zone: loc.zone,
        spacing: loc.spacing,
        span: tok_span,
    });
    i
}

/// Constructs a [`PpToken`] with an offset-adjusted [`SourceSpan`].
///
/// # Arguments
///
/// * `text` - Token text.
/// * `kind` - Token [`PpTokenKind`].
/// * `loc` - Token location metadata.
/// * `byte_len` - Token byte length.
#[must_use]
fn make_tok(text: &str, kind: PpTokenKind, loc: TokenLocCtx, byte_len: usize) -> PpToken {
    PpToken {
        text: String::from(text),
        kind,
        zone: loc.zone,
        spacing: loc.spacing,
        span: sub_span(loc.base_span, loc.byte_pos, byte_len),
    }
}

/// Computes a child [`SourceSpan`] inside `base_span`.
///
/// # Arguments
///
/// * `base_span` - Parent line span.
/// * `rel_byte` - Relative starting byte offset.
/// * `byte_len` - Slice byte length.
#[must_use]
fn sub_span(base_span: SourceSpan, rel_byte: usize, byte_len: usize) -> SourceSpan {
    let add_off = u32::try_from(rel_byte).unwrap_or(0);
    let len_u32 = u32::try_from(byte_len).unwrap_or(0);
    SourceSpan::new(
        base_span.file_id,
        ByteOffset::new(base_span.start.as_u32().saturating_add(add_off)),
        ByteLength::new(len_u32),
        base_span.expansion_id,
    )
}

/// Parses `[LEADING|TRAILING] {==...== | operand} BY {==...== | operand}` clauses from `tokens`.
///
/// # Arguments
///
/// * `tokens` - Slice of tokens inside a `REPLACING` or `REPLACE` clause (excluding trailing `.`).
/// * `err_span` - Fallback [`SourceSpan`] for syntax diagnostics.
/// * `sink` - [`DiagnosticSink`] receiving malformed clause errors.
#[must_use]
pub fn parse_replacing_clauses(
    tokens: &[PpToken],
    err_span: SourceSpan,
    sink: &mut DiagnosticSink,
) -> Vec<ReplacementRule> {
    let mut rules = Vec::new();
    let mut pos = 0_usize;

    while let Some(tok) = tokens.get(pos) {
        if tok.kind == PpTokenKind::SeparatorPunct {
            pos += 1;
            continue;
        }
        let mut kind = ReplacementPatternKind::FullSequence;
        if tok.text.eq_ignore_ascii_case("LEADING") {
            kind = ReplacementPatternKind::LeadingWordPrefix;
            pos += 1;
        } else if tok.text.eq_ignore_ascii_case("TRAILING") {
            kind = ReplacementPatternKind::TrailingWordSuffix;
            pos += 1;
        }

        let Some((lhs_tokens, after_lhs)) = take_replacing_operand(tokens, pos) else {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0001Preprocessor,
                Some(tokens.get(pos).map_or(err_span, |t| t.span)),
                String::from("malformed REPLACING pattern operand"),
            ));
            break;
        };
        pos = after_lhs;

        let Some(by_tok) = tokens.get(pos) else {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0001Preprocessor,
                Some(err_span),
                String::from("expected `BY` in REPLACING clause"),
            ));
            break;
        };
        if !by_tok.text.eq_ignore_ascii_case("BY") {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0001Preprocessor,
                Some(by_tok.span),
                format!("expected `BY` in REPLACING clause, found `{}`", by_tok.text),
            ));
            break;
        }
        pos += 1;

        let Some((rhs_tokens, after_rhs)) = take_replacing_operand(tokens, pos) else {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0001Preprocessor,
                Some(tokens.get(pos).map_or(err_span, |t| t.span)),
                String::from("malformed REPLACING target after `BY`"),
            ));
            break;
        };
        pos = after_rhs;

        let pattern: Vec<String> = lhs_tokens
            .iter()
            .filter(|t| t.kind != PpTokenKind::SeparatorPunct)
            .map(canonical_match_text)
            .collect();
        if pattern.is_empty() {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0001Preprocessor,
                Some(err_span),
                String::from("empty pseudo-text pattern `====` is not allowed on LHS of REPLACING"),
            ));
            continue;
        }
        rules.push(ReplacementRule {
            kind,
            pattern,
            replacement: rhs_tokens,
        });
    }
    rules
}

/// Extracts a `== ... ==` pseudo-text operand or a single/qualified COBOL operand starting at `pos`.
///
/// # Arguments
///
/// * `tokens` - Clause token slice.
/// * `pos` - Starting token index.
#[must_use]
fn take_replacing_operand(tokens: &[PpToken], pos: usize) -> Option<(Vec<PpToken>, usize)> {
    let first = tokens.get(pos)?;
    if first.kind == PpTokenKind::PseudoDelimiter {
        let mut cursor = pos + 1;
        let mut body = Vec::new();
        while let Some(tok) = tokens.get(cursor) {
            if tok.kind == PpTokenKind::PseudoDelimiter {
                return Some((body, cursor + 1));
            }
            body.push(tok.clone());
            cursor += 1;
        }
        return None;
    }

    // Single literal or potentially `IN`/`OF`-qualified / subscripted identifier before `BY` or next operand.
    let mut out = vec![first.clone()];
    let mut cursor = pos + 1;
    while let Some(next) = tokens.get(cursor) {
        if next.text.eq_ignore_ascii_case("IN") || next.text.eq_ignore_ascii_case("OF") {
            let qual = tokens.get(cursor + 1)?;
            out.push(next.clone());
            out.push(qual.clone());
            cursor += 2;
        } else {
            break;
        }
    }
    Some((out, cursor))
}

/// Applies `rules` in strict declaration order across `input` tokens (`first matching rule wins`).
///
/// # Arguments
///
/// * `input` - Input [`PpToken`] sequence.
/// * `rules` - Ordered [`ReplacementRule`] list.
#[must_use]
pub fn apply_replacement_rules(input: &[PpToken], rules: &[ReplacementRule]) -> Vec<PpToken> {
    if rules.is_empty() {
        return input.to_vec();
    }
    let mut out = Vec::with_capacity(input.len());
    let mut idx = 0_usize;

    while let Some(cur_tok) = input.get(idx) {
        let mut matched = false;
        for rule in rules {
            if let Some((consumed, produced)) = try_apply_single_rule(cur_tok, input, idx, rule) {
                out.extend(produced);
                idx += consumed;
                matched = true;
                break;
            }
        }
        if !matched {
            out.push(cur_tok.clone());
            idx += 1;
        }
    }
    out
}

/// Attempts to match `rule` at `input[start_idx]`, returning `(tokens_consumed, replacement_tokens)`.
///
/// # Arguments
///
/// * `first_tok` - Token at `input[start_idx]`.
/// * `input` - Input token slice.
/// * `start_idx` - Candidate match index.
/// * `rule` - Substitution rule to test.
#[must_use]
fn try_apply_single_rule(
    first_tok: &PpToken,
    input: &[PpToken],
    start_idx: usize,
    rule: &ReplacementRule,
) -> Option<(usize, Vec<PpToken>)> {
    if first_tok.kind == PpTokenKind::SeparatorPunct {
        return None;
    }

    match rule.kind {
        ReplacementPatternKind::FullSequence => {
            if rule.pattern.is_empty() {
                return None;
            }
            let mut cursor = start_idx;
            for pat_word in &rule.pattern {
                while input
                    .get(cursor)
                    .is_some_and(|t| t.kind == PpTokenKind::SeparatorPunct && cursor > start_idx)
                {
                    cursor += 1;
                }
                let candidate = input.get(cursor)?;
                if canonical_match_text(candidate) != *pat_word {
                    return None;
                }
                cursor += 1;
            }
            let mut repl = Vec::with_capacity(rule.replacement.len());
            for (r_idx, r_tok) in rule.replacement.iter().enumerate() {
                let mut cloned = r_tok.clone();
                if r_idx == 0 {
                    cloned.spacing = first_tok.spacing;
                    cloned.zone = first_tok.zone;
                }
                repl.push(cloned);
            }
            Some((cursor - start_idx, repl))
        }
        ReplacementPatternKind::LeadingWordPrefix => {
            if first_tok.kind != PpTokenKind::Word {
                return None;
            }
            let prefix = strip_affix_quotes(rule.pattern.first()?);
            let upper_word = first_tok.text.to_ascii_uppercase();
            let suffix = upper_word.strip_prefix(&prefix)?;
            let repl_prefix = join_affix_replacement(&rule.replacement);
            let mut updated = first_tok.clone();
            updated.text = format!("{repl_prefix}{suffix}");
            Some((1, vec![updated]))
        }
        ReplacementPatternKind::TrailingWordSuffix => {
            if first_tok.kind != PpTokenKind::Word {
                return None;
            }
            let suffix = strip_affix_quotes(rule.pattern.first()?);
            let upper_word = first_tok.text.to_ascii_uppercase();
            let stem = upper_word.strip_suffix(&suffix)?;
            let repl_suffix = join_affix_replacement(&rule.replacement);
            let mut updated = first_tok.clone();
            updated.text = format!("{stem}{repl_suffix}");
            Some((1, vec![updated]))
        }
    }
}

/// Computes canonical text-word comparison key for `tok`.
///
/// # Arguments
///
/// * `tok` - Preprocessor token to normalize.
#[must_use]
fn canonical_match_text(tok: &PpToken) -> String {
    if tok.kind == PpTokenKind::StringLiteral {
        tok.text.clone()
    } else {
        tok.text.to_ascii_uppercase()
    }
}

/// Strips optional surrounding quotes on `LEADING` / `TRAILING` partial-word patterns.
///
/// # Arguments
///
/// * `raw` - Raw pattern string.
#[must_use]
fn strip_affix_quotes(raw: &str) -> String {
    raw.trim_matches(DQ).trim_matches(SQ).to_ascii_uppercase()
}

/// Concatenates replacement tokens for `LEADING` / `TRAILING` partial-word substitution.
///
/// # Arguments
///
/// * `tokens` - Replacement token slice.
#[must_use]
fn join_affix_replacement(tokens: &[PpToken]) -> String {
    let mut buf = String::new();
    for tok in tokens {
        buf.push_str(tok.text.trim_matches(DQ).trim_matches(SQ));
    }
    buf
}
