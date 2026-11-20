//! `-E` preprocessed source emitter and `-t` / `-T` formatted compiler listing generator.

use std::fmt::Write as _;

use coblang_diag::DiagnosticSink;

use crate::format::{LineIndicator, MarginZone, NormalizedLine};
use crate::pseudo_text::{PpToken, SpacingHint};

/// Line-feed character (`0x0A`).
const LF: char = '\u{000a}';

/// Listing column width mode (`-t` narrow vs `-T` wide).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListingWidth {
    /// Standard 80-column listing (`-t`).
    Narrow80,
    /// Wide 120-column listing (`-T`).
    Wide120,
}

/// Whether a symbol cross-reference table is appended to the compiler listing (`-ftsymbols`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListingSymbols {
    /// Omit symbol table.
    OmitSymbols,
    /// Append symbol table (`-ftsymbols`).
    IncludeSymbols,
}

/// Whether page headers/banners are rendered in the compiler listing (`-fno-theader`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeaderVisibility {
    /// Render page banner headers.
    ShowHeader,
    /// Suppress page banner headers (`-fno-theader`).
    HideHeader,
}

/// Whether diagnostic messages are appended to the compiler listing (`-fno-tmessages`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageVisibility {
    /// Render diagnostic summary in listing.
    ShowMessages,
    /// Suppress diagnostic summary (`-fno-tmessages`).
    HideMessages,
}

/// Strongly-typed formatting options for `-t` / `-T` compiler source listings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListingOptions {
    /// Column width mode (`-t` vs `-T`).
    pub width: ListingWidth,
    /// Lines per page before page break (`--tlines=N`, `0` disables pagination).
    pub lines_per_page: u32,
    /// Symbol table visibility.
    pub symbols: ListingSymbols,
    /// Page header visibility.
    pub header: HeaderVisibility,
    /// Diagnostic message visibility.
    pub messages: MessageVisibility,
}

impl Default for ListingOptions {
    /// Default `-t` narrow listing options (`55` lines/page, header & messages shown).
    fn default() -> Self {
        Self {
            width: ListingWidth::Narrow80,
            lines_per_page: 55,
            symbols: ListingSymbols::OmitSymbols,
            header: HeaderVisibility::ShowHeader,
            messages: MessageVisibility::ShowMessages,
        }
    }
}

/// Reconstructs clean preprocessed COBOL source text (`-E`) from expanded [`PpToken`]s.
///
/// # Arguments
///
/// * `source_path` - Display path emitted in the `#line 1 "..."` header comment.
/// * `tokens` - Final preprocessed [`PpToken`] stream.
#[must_use]
pub fn emit_preprocessed_source(source_path: &str, tokens: &[PpToken]) -> String {
    let mut out = format!("#line 1 \u{0022}{source_path}\u{0022}{LF}");
    let mut at_start = true;

    for tok in tokens {
        match tok.spacing {
            SpacingHint::Newline => {
                if !at_start {
                    out.push(LF);
                }
                match tok.zone {
                    MarginZone::AreaA => out.push_str("       "),
                    MarginZone::AreaB => out.push_str("           "),
                    MarginZone::FreeFlow
                    | MarginZone::SequenceArea
                    | MarginZone::IndicatorArea
                    | MarginZone::IdentificationArea => {}
                }
            }
            SpacingHint::Space => out.push(' '),
            SpacingHint::Tight => {}
        }
        out.push_str(&tok.text);
        at_start = false;
    }
    if !at_start {
        out.push(LF);
    }
    out
}

/// Renders a formatted `-t` / `-T` compiler listing from `lines` and `sink`.
///
/// # Arguments
///
/// * `source_path` - Source file path displayed in page headers.
/// * `lines` - Normalized lines of the root source file.
/// * `expanded_tokens` - Expanded tokens (used when `ListingSymbols::IncludeSymbols` is active).
/// * `sink` - Collected diagnostics.
/// * `opts` - [`ListingOptions`] controlling width, pagination, header, symbols, and messages.
#[must_use]
pub fn render_compiler_listing(
    source_path: &str,
    lines: &[NormalizedLine],
    expanded_tokens: &[PpToken],
    sink: &DiagnosticSink,
    opts: ListingOptions,
) -> String {
    let max_code_cols = match opts.width {
        ListingWidth::Narrow80 => 65_usize,
        ListingWidth::Wide120 => 105_usize,
    };
    let mut out = String::new();
    let mut page_no = 1_u32;
    let mut line_in_page = 0_u32;

    if opts.header == HeaderVisibility::ShowHeader {
        let _ = write!(
            out,
            "coblang 0.1.0  {source_path}  Page {page_no:04}{LF}{LF}  LINE    SOURCE{LF}"
        );
    }

    for line in lines {
        if opts.lines_per_page > 0 && line_in_page >= opts.lines_per_page {
            page_no += 1;
            line_in_page = 0;
            if opts.header == HeaderVisibility::ShowHeader {
                let _ = write!(
                    out,
                    "\u{000c}coblang 0.1.0  {source_path}  Page {page_no:04}{LF}{LF}"
                );
            }
        }
        let ind_char = match line.indicator {
            LineIndicator::NormalSpace => ' ',
            LineIndicator::CommentAsterisk | LineIndicator::FloatingComment => '*',
            LineIndicator::CommentSlashPageEject => '/',
            LineIndicator::DebugD => 'D',
            LineIndicator::ContinuationHyphen => '-',
            LineIndicator::VendorDirectiveDollar => '$',
        };
        let truncated: String = line.code.chars().take(max_code_cols).collect();
        let _ = write!(out, "{:6}  {ind_char}{truncated}{LF}", line.line_no);
        line_in_page += 1;
    }

    if opts.symbols == ListingSymbols::IncludeSymbols {
        let _ = write!(
            out,
            "{LF}SYMBOL CROSS-REFERENCE ({} tokens){LF}",
            expanded_tokens.len()
        );
    }
    if opts.messages == MessageVisibility::ShowMessages {
        let _ = write!(
            out,
            "{LF}{} warnings in compilation group{LF}{} errors in compilation group{LF}",
            sink.warning_count(),
            sink.error_count()
        );
    }
    out
}
