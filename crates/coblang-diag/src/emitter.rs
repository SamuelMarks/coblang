//! Structured compiler diagnostic records and GCC/`cobc`-compatible text emitter.

use std::fmt::Write as _;

use coblang_common::{DiagnosticSeverity, Result, SourceSpan};

use crate::source_map::SourceMap;
use crate::warning::{WarningGroup, WarningPolicy};

/// Line-feed character (`0x0A`).
const LF: char = '\u{000a}';

/// Strongly-typed compiler diagnostic identifier code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticCode {
    /// `E0001` — Preprocessor, continuation, copybook, or CDF error.
    E0001Preprocessor,
    /// `E0002` — Lexical token or literal syntax error.
    E0002Lexical,
    /// `E0003` — Division, clause, or statement grammar error.
    E0003Syntax,
    /// `E0004` — Undefined data item, file, or procedure symbol.
    E0004UndefinedSymbol,
    /// `E0005` — Ambiguous qualified identifier reference.
    E0005AmbiguousSymbol,
    /// `E0006` — Invalid `PICTURE` string specification.
    E0006InvalidPicture,
    /// `E0007` — Invalid storage hierarchy, `REDEFINES`, `RENAMES`, or `OCCURS` layout.
    E0007InvalidLayout,
    /// `E0008` — Operand category or signature mismatch.
    E0008TypeMismatch,
    /// `E0009` — Construct prohibited by active dialect configuration (`unconformable`).
    E0009DialectUnconformable,
    /// `W0001` — Archaic COBOL feature warning.
    W0001Archaic,
    /// `W0002` — Obsolete COBOL feature warning.
    W0002Obsolete,
    /// `W0003` — `REDEFINES` size/alignment warning.
    W0003Redefinition,
    /// `W0004` — Potential value truncation warning.
    W0004Truncate,
    /// `W0005` — Sequence area or column-72 overflow warning.
    W0005SourceLayout,
    /// `W0006` — General dialect or semantic warning.
    W0006GeneralWarning,
}

impl DiagnosticCode {
    /// Returns the short alphanumeric diagnostic identifier (e.g. `"E0001"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::E0001Preprocessor => "E0001",
            Self::E0002Lexical => "E0002",
            Self::E0003Syntax => "E0003",
            Self::E0004UndefinedSymbol => "E0004",
            Self::E0005AmbiguousSymbol => "E0005",
            Self::E0006InvalidPicture => "E0006",
            Self::E0007InvalidLayout => "E0007",
            Self::E0008TypeMismatch => "E0008",
            Self::E0009DialectUnconformable => "E0009",
            Self::W0001Archaic => "W0001",
            Self::W0002Obsolete => "W0002",
            Self::W0003Redefinition => "W0003",
            Self::W0004Truncate => "W0004",
            Self::W0005SourceLayout => "W0005",
            Self::W0006GeneralWarning => "W0006",
        }
    }
}

/// Secondary `note:` attachment associated with a primary [`Diagnostic`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticNote {
    /// Optional source span where the note applies.
    pub span: Option<SourceSpan>,
    /// Explanatory note message.
    pub message: String,
}

/// Structured compiler diagnostic with optional primary span, warning group, and notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Initial severity (`Error`, `Warning`, or `Note`).
    pub severity: DiagnosticSeverity,
    /// Strongly-typed diagnostic code.
    pub code: DiagnosticCode,
    /// Optional primary byte span inside [`SourceMap`].
    pub primary_span: Option<SourceSpan>,
    /// Primary human-readable diagnostic message.
    pub message: String,
    /// Supplemental `note:` attachments.
    pub notes: Vec<DiagnosticNote>,
    /// Controlling [`WarningGroup`] when this diagnostic is policy-governed.
    pub warning_group: Option<WarningGroup>,
}

impl Diagnostic {
    /// Creates a hard error [`Diagnostic`].
    ///
    /// # Arguments
    ///
    /// * `code` - Error [`DiagnosticCode`].
    /// * `span` - Optional primary [`SourceSpan`].
    /// * `message` - Primary diagnostic message.
    #[must_use]
    pub fn error(code: DiagnosticCode, span: Option<SourceSpan>, message: String) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            code,
            primary_span: span,
            message,
            notes: Vec::new(),
            warning_group: None,
        }
    }

    /// Creates a policy-governed warning [`Diagnostic`].
    ///
    /// # Arguments
    ///
    /// * `code` - Warning [`DiagnosticCode`].
    /// * `group` - Controlling [`WarningGroup`].
    /// * `span` - Optional primary [`SourceSpan`].
    /// * `message` - Primary diagnostic message.
    #[must_use]
    pub fn warning(
        code: DiagnosticCode,
        group: WarningGroup,
        span: Option<SourceSpan>,
        message: String,
    ) -> Self {
        Self {
            severity: DiagnosticSeverity::Warning,
            code,
            primary_span: span,
            message,
            notes: Vec::new(),
            warning_group: Some(group),
        }
    }

    /// Appends a supplemental [`DiagnosticNote`] and returns the updated [`Diagnostic`].
    ///
    /// # Arguments
    ///
    /// * `span` - Optional span for the note.
    /// * `message` - Note message text.
    #[must_use]
    pub fn with_note(mut self, span: Option<SourceSpan>, message: String) -> Self {
        self.notes.push(DiagnosticNote { span, message });
        self
    }
}

/// Collector that filters, promotes, caps, and renders compiler [`Diagnostic`]s.
#[derive(Debug, Clone)]
pub struct DiagnosticSink {
    /// Active warning suppression/promotion policy.
    warning_policy: WarningPolicy,
    /// Maximum hard errors collected before further errors are dropped (`-fmax-errors`).
    max_errors: usize,
    /// Maximum total diagnostic entries (`Error` + `Warning` + `Note`) retained in memory.
    max_items: usize,
    /// Retained diagnostics in emission order.
    items: Vec<Diagnostic>,
    /// Count of retained `Error`-severity diagnostics.
    error_count: usize,
    /// Count of retained `Warning`-severity diagnostics.
    warning_count: usize,
}

impl DiagnosticSink {
    /// Creates a new [`DiagnosticSink`] with a bounded total diagnostic capacity.
    ///
    /// # Arguments
    ///
    /// * `warning_policy` - Active [`WarningPolicy`].
    /// * `max_errors` - Maximum hard errors to record (`>= 1` in normal operation).
    #[must_use]
    pub const fn new(warning_policy: WarningPolicy, max_errors: usize) -> Self {
        let scaled = max_errors.saturating_mul(4);
        let max_items = if scaled < 256 { 256 } else { scaled };
        Self::with_limits(warning_policy, max_errors, max_items)
    }

    /// Creates a new [`DiagnosticSink`] with explicit `max_errors` and `max_items` ceilings.
    ///
    /// # Arguments
    ///
    /// * `warning_policy` - Active [`WarningPolicy`].
    /// * `max_errors` - Maximum hard errors to record.
    /// * `max_items` - Maximum total diagnostic entries (`Error`, `Warning`, or `Note`) retained.
    #[must_use]
    pub const fn with_limits(
        warning_policy: WarningPolicy,
        max_errors: usize,
        max_items: usize,
    ) -> Self {
        Self {
            warning_policy,
            max_errors,
            max_items,
            items: Vec::new(),
            error_count: 0,
            warning_count: 0,
        }
    }

    /// Filters, promotes, and records `diag` according to [`WarningPolicy`], `max_errors`, and `max_items`.
    ///
    /// # Arguments
    ///
    /// * `diag` - Candidate diagnostic to emit.
    pub fn emit(&mut self, mut diag: Diagnostic) {
        if self.items.len() >= self.max_items {
            return;
        }
        if let Some(group) = diag.warning_group {
            let Some(effective) = self.warning_policy.classify(group) else {
                return;
            };
            diag.severity = effective;
        }

        match diag.severity {
            DiagnosticSeverity::Error => {
                if self.error_count >= self.max_errors {
                    return;
                }
                self.error_count += 1;
            }
            DiagnosticSeverity::Warning => {
                self.warning_count += 1;
            }
            DiagnosticSeverity::Note => {}
        }
        self.items.push(diag);
    }

    /// Returns `true` if at least one hard `Error` diagnostic was recorded.
    #[must_use]
    pub const fn has_errors(&self) -> bool {
        self.error_count > 0
    }

    /// Returns the number of recorded `Error` diagnostics.
    #[must_use]
    pub const fn error_count(&self) -> usize {
        self.error_count
    }

    /// Returns the number of recorded `Warning` diagnostics.
    #[must_use]
    pub const fn warning_count(&self) -> usize {
        self.warning_count
    }

    /// Returns a slice of all recorded [`Diagnostic`]s.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.items
    }

    /// Renders all recorded diagnostics in canonical GCC/`cobc` single-line format.
    ///
    /// Each primary diagnostic renders as `{file}:{line}:{col}: {error|warning|note}: {msg}`
    /// (or `coblang: {error|warning|note}: {msg}` when spanless), followed by any expansion
    /// trace notes and explicit [`DiagnosticNote`] entries.
    ///
    /// # Arguments
    ///
    /// * `source_map` - [`SourceMap`] used to resolve spans and expansion chains.
    ///
    /// # Errors
    ///
    /// Returns [`coblang_common::CoblangError`] if a span references an unregistered file/offset.
    pub fn render_all(&self, source_map: &SourceMap) -> Result<String> {
        let mut out = String::new();
        for diag in &self.items {
            render_single_line(
                &mut out,
                source_map,
                diag.primary_span,
                diag.severity,
                &diag.message,
            )?;
            if let Some(span) = diag.primary_span {
                for (call_loc, kind) in source_map.unwind_expansion_chain(span)? {
                    let _ = write!(
                        out,
                        "{}: note: {}{LF}",
                        call_loc.to_header_prefix(),
                        kind.diagnostic_note_text()
                    );
                }
            }
            for note in &diag.notes {
                render_single_line(
                    &mut out,
                    source_map,
                    note.span,
                    DiagnosticSeverity::Note,
                    &note.message,
                )?;
            }
        }
        Ok(out)
    }
}

/// Formats one `{prefix}: {severity}: {message}` line into `out`.
///
/// # Arguments
///
/// * `out` - Destination buffer.
/// * `source_map` - Source map for span lookup.
/// * `span` - Optional source span.
/// * `severity` - Rendered diagnostic severity.
/// * `message` - Message text.
///
/// # Errors
///
/// Propagates span resolution errors from `source_map`.
fn render_single_line(
    out: &mut String,
    source_map: &SourceMap,
    span: Option<SourceSpan>,
    severity: DiagnosticSeverity,
    message: &str,
) -> Result<()> {
    if let Some(sp) = span {
        let loc = source_map.resolve_span(sp)?;
        let _ = write!(
            out,
            "{}: {}: {message}{LF}",
            loc.to_header_prefix(),
            severity.as_str()
        );
    } else {
        let _ = write!(out, "coblang: {}: {message}{LF}", severity.as_str());
    }
    Ok(())
}
