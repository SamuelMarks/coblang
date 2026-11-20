//! Fixed, Free, and Auto reference-format line normalizer and continuation joiner.

use coblang_common::{ByteLength, ByteOffset, ExpansionId, FileId, SourceFormat, SourceSpan};
use coblang_diag::{
    Diagnostic, DiagnosticCode, DiagnosticSink, DialectConfig, SourceFile, WarningGroup,
};

/// Tab byte (`0x09`).
const TAB_CHAR: char = '\u{0009}';
/// Double-quote character (`0x22`).
const DQ: char = '"';
/// Single-quote character (`0x27`).
const SQ: char = '\u{0027}';

/// Classification of the COBOL indicator area on a source line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineIndicator {
    /// Blank `' '` normal source line.
    NormalSpace,
    /// `'*'` comment line.
    CommentAsterisk,
    /// `'/'` page-eject comment line.
    CommentSlashPageEject,
    /// `'D'` or `'d'` debugging line.
    DebugD,
    /// `'-'` continuation line in fixed format.
    ContinuationHyphen,
    /// `*>` floating inline/full-line comment.
    FloatingComment,
    /// `'$'` vendor directive line (`$SET`).
    VendorDirectiveDollar,
}

/// Syntactic margin area of a COBOL line or token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarginZone {
    /// Fixed-form columns 1–6 sequence number area.
    SequenceArea,
    /// Fixed-form column 7 indicator area.
    IndicatorArea,
    /// Fixed-form columns 8–11 Area A (division/section/paragraph/`01`/`77`/`FD` headers).
    AreaA,
    /// Fixed-form columns 12–72 Area B (statements and subordinate clauses).
    AreaB,
    /// Fixed-form columns 73+ program identification area.
    IdentificationArea,
    /// Free-form unconstrained code flow.
    FreeFlow,
}

/// Policy controlling whether column-7 `'D'` / `'d'` debugging lines are compiled or stripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DebugLinePolicy {
    /// Treat `'D'` lines as comments (`WITH DEBUGGING MODE` inactive).
    StripDebugLines,
    /// Compile `'D'` lines as normal code (`WITH DEBUGGING MODE` or `-fdebugging-line`).
    CompileDebugLines,
}

/// Single normalized logical/physical line produced by [`normalize_file`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedLine {
    /// 1-based physical line number inside the [`SourceFile`].
    pub line_no: u32,
    /// Extracted columns 1–6 sequence text (empty in `Free` format).
    pub sequence: String,
    /// Classified line indicator.
    pub indicator: LineIndicator,
    /// Starting margin zone of the code payload (`AreaA`, `AreaB`, or `FreeFlow`).
    pub zone: MarginZone,
    /// Code payload with comments stripped and continuations joined.
    pub code: String,
    /// Source span covering the active code payload on this line.
    pub span: SourceSpan,
}

/// Normalizes a [`SourceFile`] into [`NormalizedLine`] records, resolving continuations and inline format switches.
///
/// # Arguments
///
/// * `file` - Loaded [`SourceFile`] to scan.
/// * `initial_format` - Requested [`SourceFormat`] (`Fixed`, `Free`, or `Auto`).
/// * `dialect_cfg` - Active [`DialectConfig`] (`tab_width`, `text_column`).
/// * `debug_policy` - Policy governing `'D'` indicator lines.
/// * `expansion_id` - Active [`ExpansionId`] stamped onto emitted spans.
/// * `sink` - [`DiagnosticSink`] receiving sequence, overflow, or continuation diagnostics.
#[must_use]
pub fn normalize_file(
    file: &SourceFile,
    initial_format: SourceFormat,
    dialect_cfg: &DialectConfig,
    debug_policy: DebugLinePolicy,
    expansion_id: ExpansionId,
    sink: &mut DiagnosticSink,
) -> Vec<NormalizedLine> {
    normalize_text(
        file.id(),
        file.content(),
        initial_format,
        dialect_cfg,
        debug_policy,
        expansion_id,
        sink,
    )
}

/// Normalizes raw `content` associated with `file_id` into [`NormalizedLine`] records.
///
/// # Arguments
///
/// * `file_id` - Owning [`FileId`] in `SourceMap`.
/// * `content` - Raw UTF-8 source text.
/// * `initial_format` - Requested [`SourceFormat`] (`Fixed`, `Free`, or `Auto`).
/// * `dialect_cfg` - Active [`DialectConfig`].
/// * `debug_policy` - Policy governing `'D'` indicator lines.
/// * `expansion_id` - Active [`ExpansionId`].
/// * `sink` - [`DiagnosticSink`] receiving layout diagnostics.
#[must_use]
pub fn normalize_text(
    file_id: FileId,
    content: &str,
    initial_format: SourceFormat,
    dialect_cfg: &DialectConfig,
    debug_policy: DebugLinePolicy,
    expansion_id: ExpansionId,
    sink: &mut DiagnosticSink,
) -> Vec<NormalizedLine> {
    let mut active_format = match initial_format {
        SourceFormat::Fixed => SourceFormat::Fixed,
        SourceFormat::Free => SourceFormat::Free,
        SourceFormat::Auto => detect_initial_format(content),
    };

    let mut out: Vec<NormalizedLine> = Vec::new();
    let mut byte_cursor: u32 = 0;
    let mut active_debug = debug_policy;

    for (idx, raw_segment) in content.split_terminator('\u{000a}').enumerate() {
        let seg_len = u32::try_from(raw_segment.len()).unwrap_or(u32::MAX);
        let clean_line = raw_segment.strip_suffix('\u{000d}').unwrap_or(raw_segment);
        let expanded = expand_tabs(clean_line, dialect_cfg.tab_width);
        let line_no = u32::try_from(idx + 1).unwrap_or(u32::MAX);
        let line_span = SourceSpan::new(
            file_id,
            ByteOffset::new(byte_cursor),
            ByteLength::new(u32::try_from(clean_line.len()).unwrap_or(0)),
            expansion_id,
        );

        if let Some(switched) = detect_inline_format_directive(&expanded) {
            active_format = switched;
            byte_cursor = byte_cursor.saturating_add(seg_len).saturating_add(1);
            continue;
        }

        if active_format == SourceFormat::Fixed {
            process_fixed_line(
                &expanded,
                line_no,
                line_span,
                dialect_cfg.text_column as usize,
                active_debug,
                &mut out,
                sink,
            );
        } else {
            process_free_line(&expanded, line_no, line_span, active_debug, &mut out);
        }

        if let Some(last) = out.last()
            && !last.code.is_empty()
        {
            update_active_debug_policy(&last.code, debug_policy, &mut active_debug);
        }

        byte_cursor = byte_cursor.saturating_add(seg_len).saturating_add(1);
    }
    out
}

/// Updates `active_debug` when a unit header or `DEBUGGING MODE` clause appears on `code`.
///
/// # Arguments
///
/// * `code` - Normalized non-comment code text on the line.
/// * `base_policy` - Initial CLI [`DebugLinePolicy`].
/// * `active_debug` - Mutable per-unit active [`DebugLinePolicy`].
fn update_active_debug_policy(
    code: &str,
    base_policy: DebugLinePolicy,
    active_debug: &mut DebugLinePolicy,
) {
    let words: Vec<String> = code
        .split(|c: char| c.is_ascii_whitespace() || c == '.')
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    if words
        .iter()
        .any(|w| matches!(w.as_str(), "PROGRAM-ID" | "FUNCTION-ID" | "CLASS-ID"))
    {
        *active_debug = base_policy;
    }
    if words
        .windows(2)
        .any(|pair| matches!(pair, [a, b] if a == "DEBUGGING" && b == "MODE"))
    {
        *active_debug = DebugLinePolicy::CompileDebugLines;
    }
}

/// Auto-detects `Fixed` vs `Free` format from the first informative line of `content`.
///
/// # Arguments
///
/// * `content` - Full source file text.
#[must_use]
fn detect_initial_format(content: &str) -> SourceFormat {
    for raw in content.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let upper = trimmed.to_ascii_uppercase();
        if upper.starts_with("*>") || upper.starts_with(">>") {
            return SourceFormat::Free;
        }
        // Unindented division/directive keyword in columns 1-6 indicates free format.
        let leading_spaces = raw.chars().take_while(|&c| c == ' ').count();
        if leading_spaces < 6
            && (upper.starts_with("IDENTIFICATION ")
                || upper.starts_with("ID DIVISION")
                || upper.starts_with("PROGRAM-ID")
                || upper.starts_with("FUNCTION-ID")
                || upper.starts_with("DATA DIVISION")
                || upper.starts_with("PROCEDURE DIVISION"))
        {
            return SourceFormat::Free;
        }
        return SourceFormat::Fixed;
    }
    SourceFormat::Fixed
}

/// Checks for `>>SOURCE [FORMAT] [IS] {FREE|FIXED}` or `$SET SOURCEFORMAT"FREE"|"FIXED"`.
///
/// # Arguments
///
/// * `expanded_line` - Tab-expanded line without trailing CR/LF.
#[must_use]
fn detect_inline_format_directive(expanded_line: &str) -> Option<SourceFormat> {
    let trimmed = expanded_line.trim_start();
    let candidate = if expanded_line.len() > 6 && expanded_line.as_bytes().get(6) == Some(&b'$') {
        expanded_line.get(7..).unwrap_or("").trim()
    } else {
        trimmed
    };
    let upper = candidate.to_ascii_uppercase();
    if let Some(rest) = upper.strip_prefix(">>SOURCE") {
        let words: Vec<&str> = rest.split_whitespace().collect();
        if words.contains(&"FREE") {
            return Some(SourceFormat::Free);
        }
        if words.contains(&"FIXED") {
            return Some(SourceFormat::Fixed);
        }
    }
    if upper.starts_with("SET ") && upper.contains("SOURCEFORMAT") {
        if upper.contains("FREE") {
            return Some(SourceFormat::Free);
        }
        if upper.contains("FIXED") {
            return Some(SourceFormat::Fixed);
        }
    }
    None
}

/// Expands horizontal tabs (`0x09`) to spaces aligned to `tab_width` columns.
///
/// # Arguments
///
/// * `line` - Input line without trailing CR/LF.
/// * `tab_width` - Tab stop interval (`>= 1`).
#[must_use]
fn expand_tabs(line: &str, tab_width: u8) -> String {
    if !line.contains(TAB_CHAR) {
        return String::from(line);
    }
    let step = usize::from(tab_width.max(1));
    let mut out = String::with_capacity(line.len() + 8);
    let mut col = 0_usize;
    for ch in line.chars() {
        if ch == TAB_CHAR {
            let spaces = step - (col % step);
            for _ in 0..spaces {
                out.push(' ');
            }
            col += spaces;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

/// Slices a single fixed-format line and resolves `'-'` continuations onto the previous active line.
///
/// # Arguments
///
/// * `expanded` - Tab-expanded line.
/// * `line_no` - 1-based line number.
/// * `span` - Line [`SourceSpan`].
/// * `right_margin` - Fixed-format text column ceiling (typically `72`).
/// * `debug_policy` - Policy governing `'D'` lines.
/// * `out` - Output [`NormalizedLine`] buffer.
/// * `sink` - Diagnostic sink.
fn process_fixed_line(
    expanded: &str,
    line_no: u32,
    span: SourceSpan,
    right_margin: usize,
    debug_policy: DebugLinePolicy,
    out: &mut Vec<NormalizedLine>,
    sink: &mut DiagnosticSink,
) {
    let chars: Vec<char> = expanded.chars().collect();
    let seq: String = chars.iter().take(6).collect();
    if !seq.trim().is_empty() && !seq.chars().all(|c| c.is_ascii_digit() || c == ' ') {
        sink.emit(Diagnostic::warning(
            DiagnosticCode::W0005SourceLayout,
            WarningGroup::Sequence,
            Some(span),
            format!("non-numeric sequence number `{}`", seq.trim()),
        ));
    }

    let Some(&ind_ch) = chars.get(6) else {
        out.push(NormalizedLine {
            line_no,
            sequence: seq,
            indicator: LineIndicator::NormalSpace,
            zone: MarginZone::SequenceArea,
            code: String::new(),
            span,
        });
        return;
    };

    let indicator = match ind_ch {
        ' ' => LineIndicator::NormalSpace,
        '*' => LineIndicator::CommentAsterisk,
        '/' => LineIndicator::CommentSlashPageEject,
        'D' | 'd' => LineIndicator::DebugD,
        '-' => LineIndicator::ContinuationHyphen,
        '$' => LineIndicator::VendorDirectiveDollar,
        bad => {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0001Preprocessor,
                Some(span),
                format!("invalid indicator `{bad}` in column 7"),
            ));
            LineIndicator::CommentAsterisk
        }
    };

    let body_end = chars.len().min(right_margin);
    let raw_body: String = chars.get(7..body_end).unwrap_or(&[]).iter().collect();
    let code_body = strip_floating_comment(&raw_body);

    match indicator {
        LineIndicator::CommentAsterisk | LineIndicator::CommentSlashPageEject => {
            out.push(NormalizedLine {
                line_no,
                sequence: seq,
                indicator,
                zone: MarginZone::IndicatorArea,
                code: String::new(),
                span,
            });
        }
        LineIndicator::VendorDirectiveDollar => {
            out.push(NormalizedLine {
                line_no,
                sequence: seq,
                indicator,
                zone: MarginZone::IndicatorArea,
                code: code_body,
                span,
            });
        }
        LineIndicator::DebugD if debug_policy == DebugLinePolicy::StripDebugLines => {
            out.push(NormalizedLine {
                line_no,
                sequence: seq,
                indicator,
                zone: MarginZone::IndicatorArea,
                code: String::new(),
                span,
            });
        }
        LineIndicator::ContinuationHyphen => {
            apply_fixed_continuation(&code_body, right_margin, span, out, sink);
            out.push(NormalizedLine {
                line_no,
                sequence: seq,
                indicator,
                zone: MarginZone::AreaB,
                code: String::new(),
                span,
            });
        }
        LineIndicator::NormalSpace | LineIndicator::DebugD | LineIndicator::FloatingComment => {
            let area_a_has_non_space = code_body.chars().take(4).any(|c| c != ' ');
            let zone = if area_a_has_non_space {
                MarginZone::AreaA
            } else {
                MarginZone::AreaB
            };
            out.push(NormalizedLine {
                line_no,
                sequence: seq,
                indicator,
                zone,
                code: code_body,
                span,
            });
        }
    }
}

/// Joins a fixed-format `'-'` continuation line onto the most recent non-comment [`NormalizedLine`].
///
/// # Arguments
///
/// * `cont_body` - Columns 8..=`right_margin` of the continuation line.
/// * `right_margin` - Fixed-format text column (`72`).
/// * `span` - Continuation line [`SourceSpan`].
/// * `out` - Existing normalized lines.
/// * `sink` - Diagnostic sink for orphan/malformed continuations.
fn apply_fixed_continuation(
    cont_body: &str,
    right_margin: usize,
    span: SourceSpan,
    out: &mut [NormalizedLine],
    sink: &mut DiagnosticSink,
) {
    let area_b_text = cont_body.chars().skip(4).collect::<String>();
    let trimmed_b = area_b_text.trim_start();
    let Some(prev) = out
        .iter_mut()
        .rev()
        .find(|l| l.indicator != LineIndicator::VendorDirectiveDollar && !l.code.trim().is_empty())
    else {
        sink.emit(Diagnostic::error(
            DiagnosticCode::E0001Preprocessor,
            Some(span),
            String::from("continuation indicator `-` with no preceding line to continue"),
        ));
        return;
    };

    if let Some(open_q) = unclosed_quote_delimiter(&prev.code) {
        let Some(after_q) = trimmed_b.strip_prefix(open_q) else {
            sink.emit(Diagnostic::error(
                DiagnosticCode::E0001Preprocessor,
                Some(span),
                format!("continued string literal must begin with `{open_q}` in Area B"),
            ));
            return;
        };
        // Pad previous line to full fixed margin (65 code columns: cols 8..=72) before appending.
        let target_width = right_margin.saturating_sub(7);
        let cur_chars = prev.code.chars().count();
        for _ in cur_chars..target_width {
            prev.code.push(' ');
        }
        prev.code.push_str(after_q);
    } else {
        let trimmed_prev = prev.code.trim_end().len();
        prev.code.truncate(trimmed_prev);
        prev.code.push_str(trimmed_b);
    }
}

/// Processes a single free-format line (stripping `*>` comments, `>>D` debug lines, and joining `"..."-` continuations).
///
/// # Arguments
///
/// * `expanded` - Tab-expanded line.
/// * `line_no` - 1-based line number.
/// * `span` - Line [`SourceSpan`].
/// * `debug_policy` - Active [`DebugLinePolicy`] for `>>D` debugging lines.
/// * `out` - Output [`NormalizedLine`] buffer.
fn process_free_line(
    expanded: &str,
    line_no: u32,
    span: SourceSpan,
    debug_policy: DebugLinePolicy,
    out: &mut Vec<NormalizedLine>,
) {
    if expanded.starts_with('*')
        || expanded.starts_with('/')
        || expanded.trim_start().starts_with("*>")
    {
        out.push(NormalizedLine {
            line_no,
            sequence: String::new(),
            indicator: LineIndicator::FloatingComment,
            zone: MarginZone::FreeFlow,
            code: String::new(),
            span,
        });
        return;
    }
    let clean = strip_floating_comment(expanded);
    let trimmed_clean = clean.trim_start();
    if let Some(after_d) = trimmed_clean
        .strip_prefix(">>D")
        .or_else(|| trimmed_clean.strip_prefix(">>d"))
        && (after_d.is_empty() || after_d.starts_with(char::is_whitespace))
    {
        let kept = match debug_policy {
            DebugLinePolicy::StripDebugLines => String::new(),
            DebugLinePolicy::CompileDebugLines => String::from(after_d),
        };
        out.push(NormalizedLine {
            line_no,
            sequence: String::new(),
            indicator: LineIndicator::DebugD,
            zone: MarginZone::FreeFlow,
            code: kept,
            span,
        });
        return;
    }
    if let Some(dollar_body) = trimmed_clean.strip_prefix('$') {
        out.push(NormalizedLine {
            line_no,
            sequence: String::new(),
            indicator: LineIndicator::VendorDirectiveDollar,
            zone: MarginZone::FreeFlow,
            code: String::from(dollar_body),
            span,
        });
        return;
    }
    if let Some(prev) = out
        .iter_mut()
        .rev()
        .find(|l| l.indicator != LineIndicator::VendorDirectiveDollar && !l.code.trim().is_empty())
        && let Some((prefix, quote_ch)) = strip_free_continuation_marker(&prev.code)
        && let Some(cont_tail) = clean.trim_start().strip_prefix(quote_ch)
    {
        prev.code = format!("{prefix}{cont_tail}");
        out.push(NormalizedLine {
            line_no,
            sequence: String::new(),
            indicator: LineIndicator::ContinuationHyphen,
            zone: MarginZone::FreeFlow,
            code: String::new(),
            span,
        });
        return;
    }

    out.push(NormalizedLine {
        line_no,
        sequence: String::new(),
        indicator: LineIndicator::NormalSpace,
        zone: MarginZone::FreeFlow,
        code: clean,
        span,
    });
}

/// Checks if a free-format line ends with `"..."-` or `'...'-` string continuation syntax.
///
/// # Arguments
///
/// * `code` - Trimmed/uncommented free-format line.
#[must_use]
fn strip_free_continuation_marker(code: &str) -> Option<(&str, char)> {
    let without_dash = code.trim_end().strip_suffix('-')?;
    if let Some(stem) = without_dash.strip_suffix(DQ) {
        return Some((stem, DQ));
    }
    if let Some(stem) = without_dash.strip_suffix(SQ) {
        return Some((stem, SQ));
    }
    None
}

/// Strips inline `*>` floating comments occurring outside string literals.
///
/// # Arguments
///
/// * `text` - Source code slice.
#[must_use]
pub fn strip_floating_comment(text: &str) -> String {
    let mut active_quote: Option<char> = None;
    let bytes = text.as_bytes();
    for (idx, ch) in text.char_indices() {
        if let Some(q) = active_quote {
            if ch == q {
                active_quote = None;
            }
        } else if ch == DQ || ch == SQ {
            active_quote = Some(ch);
        } else if ch == '*' && bytes.get(idx + 1) == Some(&b'>') {
            return String::from(text.get(..idx).unwrap_or(text));
        }
    }
    String::from(text)
}

/// Returns `Some('"' | ''')` if `code` ends inside an unclosed string literal.
///
/// # Arguments
///
/// * `code` - Active code fragment.
#[must_use]
fn unclosed_quote_delimiter(code: &str) -> Option<char> {
    let mut active: Option<char> = None;
    for ch in code.chars() {
        if let Some(q) = active {
            if ch == q {
                active = None;
            }
        } else if ch == DQ || ch == SQ {
            active = Some(ch);
        }
    }
    active
}
