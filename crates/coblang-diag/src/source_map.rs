//! Byte-exact source file table and `COPY` / `REPLACE` / CDF expansion backtraces.

use coblang_common::{
    ByteOffset, CoblangError, ColumnNumber, ExpansionId, FileId, LineNumber, ResolvedLocation,
    Result, SourceSpan,
};

/// Line-feed byte (`0x0A`).
const LF_BYTE: u8 = 0x0A;

/// Kind of preprocessor text expansion that produced a [`SourceSpan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExpansionKind {
    /// `COPY` copybook inclusion (with or without `REPLACING`).
    Copybook,
    /// `REPLACE` text-manipulation substitution.
    ReplaceStatement,
    /// `>>DEFINE` constant/parameter substitution.
    CdfDefine,
    /// `>>IF` / `>>EVALUATE` conditional compilation block.
    CdfConditional,
}

impl ExpansionKind {
    /// Returns the human-readable diagnostic `note:` phrase for this expansion kind.
    #[must_use]
    pub const fn diagnostic_note_text(self) -> &'static str {
        match self {
            Self::Copybook => "in COPY included from here",
            Self::ReplaceStatement => "in REPLACE expansion applied here",
            Self::CdfDefine => "in >>DEFINE macro expanded from here",
            Self::CdfConditional => "in conditional directive evaluated here",
        }
    }
}

/// Single frame in the preprocessor expansion tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpansionRecord {
    /// Enclosing parent expansion frame ([`ExpansionId::ROOT`] at top level).
    pub parent: ExpansionId,
    /// Source span of the directive (`COPY`, `REPLACE`, `>>DEFINE`) that triggered expansion.
    pub call_site: SourceSpan,
    /// Classification of the expansion construct.
    pub kind: ExpansionKind,
}

/// Loaded source or copybook file with precomputed `O(log N)` line-start byte offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// Unique identifier assigned by [`SourceMap`].
    id: FileId,
    /// Display file path used in diagnostics and DWARF metadata.
    path: String,
    /// Decoded UTF-8 text buffer.
    content: String,
    /// Sorted zero-based byte offsets where each 1-based line begins (always starts with `0`).
    line_starts: Vec<u32>,
}

impl SourceFile {
    /// Constructs a [`SourceFile`] enforcing `max_bytes` and indexing all `LF` (`0x0A`) boundaries.
    ///
    /// # Arguments
    ///
    /// * `id` - Assigned [`FileId`].
    /// * `path` - Display path of the source file.
    /// * `content` - Decoded file text.
    /// * `max_bytes` - Maximum allowed byte length (`u32::MAX` by default).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `content.len() > max_bytes as usize`.
    pub fn with_max_bytes(
        id: FileId,
        path: String,
        content: String,
        max_bytes: u32,
    ) -> Result<Self> {
        if content.len() > max_bytes as usize {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "SourceFile",
                value: content.len().to_string(),
                reason: format!("source file exceeds byte limit {max_bytes}"),
            });
        }
        let mut line_starts = vec![0_u32];
        for (idx, &byte) in content.as_bytes().iter().enumerate() {
            if byte == LF_BYTE {
                let next_offset = u32::try_from(idx + 1).unwrap_or(u32::MAX);
                line_starts.push(next_offset);
            }
        }
        Ok(Self {
            id,
            path,
            content,
            line_starts,
        })
    }

    /// Returns the assigned [`FileId`].
    #[must_use]
    pub const fn id(&self) -> FileId {
        self.id
    }

    /// Returns the display path of this file.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the full source text buffer.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Returns the total number of indexed lines (`>= 1`).
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Resolves a [`ByteOffset`] into 1-based `(LineNumber, ColumnNumber)`.
    ///
    /// # Arguments
    ///
    /// * `offset` - Zero-based byte offset inside `self.content` (`<= content.len()`).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `offset` exceeds `content.len()`.
    pub fn resolve_line_col(&self, offset: ByteOffset) -> Result<(LineNumber, ColumnNumber)> {
        let raw_off = offset.as_u32();
        if (raw_off as usize) > self.content.len() {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "ByteOffset",
                value: raw_off.to_string(),
                reason: format!(
                    "offset exceeds file `{}` length {}",
                    self.path,
                    self.content.len()
                ),
            });
        }
        let partition = self.line_starts.partition_point(|&start| start <= raw_off);
        let zero_based_line = partition.saturating_sub(1);
        let line_start = self.line_starts.get(zero_based_line).copied().unwrap_or(0);
        let line_u32 = u32::try_from(zero_based_line).unwrap_or(u32::MAX);
        let col_zero_based = raw_off.saturating_sub(line_start);
        Ok((
            LineNumber::from_zero_based(line_u32),
            ColumnNumber::from_zero_based(col_zero_based),
        ))
    }
}

/// Multi-file source registry paired with the `COPY` / `REPLACE` / CDF expansion tree.
#[derive(Debug, Clone)]
pub struct SourceMap {
    /// Registered source and copybook files indexed by [`FileId::as_u32`].
    files: Vec<SourceFile>,
    /// Expansion records indexed by `expansion_id.as_u32() - 1` (`0` is [`ExpansionId::ROOT`]).
    expansions: Vec<ExpansionRecord>,
    /// Maximum registered files permitted (`u32::MAX` by default).
    max_files: u32,
    /// Maximum byte length per file (`u32::MAX` by default).
    max_file_bytes: u32,
    /// Maximum expansion frames permitted (`u32::MAX - 1` by default).
    max_expansions: u32,
}

impl Default for SourceMap {
    /// Creates an empty [`SourceMap`] with full `u32` capacity limits.
    fn default() -> Self {
        Self::new()
    }
}

impl SourceMap {
    /// Creates an empty [`SourceMap`] with full `u32` capacity limits.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_limits(u32::MAX, u32::MAX, u32::MAX - 1)
    }

    /// Creates an empty [`SourceMap`] with explicit table ceilings.
    ///
    /// # Arguments
    ///
    /// * `max_files` - Maximum number of files that may be added.
    /// * `max_file_bytes` - Maximum byte size of any single file.
    /// * `max_expansions` - Maximum number of expansion records.
    #[must_use]
    pub const fn with_limits(max_files: u32, max_file_bytes: u32, max_expansions: u32) -> Self {
        Self {
            files: Vec::new(),
            expansions: Vec::new(),
            max_files,
            max_file_bytes,
            max_expansions,
        }
    }

    /// Registers a new source or copybook file and returns its [`FileId`].
    ///
    /// # Arguments
    ///
    /// * `path` - Display file path.
    /// * `content` - Decoded source content.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if file count or byte length exceeds configured limits.
    pub fn add_file(&mut self, path: String, content: String) -> Result<FileId> {
        if self.files.len() >= self.max_files as usize {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "FileId",
                value: self.files.len().to_string(),
                reason: format!("SourceMap file table exceeded limit {}", self.max_files),
            });
        }
        let raw_id = u32::try_from(self.files.len()).unwrap_or(u32::MAX);
        let id = FileId::new(raw_id);
        let file = SourceFile::with_max_bytes(id, path, content, self.max_file_bytes)?;
        self.files.push(file);
        Ok(id)
    }

    /// Looks up a registered [`SourceFile`] by [`FileId`].
    ///
    /// # Arguments
    ///
    /// * `file_id` - File handle to retrieve.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `file_id` is unknown.
    pub fn get_file(&self, file_id: FileId) -> Result<&SourceFile> {
        self.files
            .get(file_id.as_u32() as usize)
            .ok_or_else(|| CoblangError::InvalidDomainValue {
                domain_type: "FileId",
                value: file_id.as_u32().to_string(),
                reason: String::from("unknown FileId in SourceMap"),
            })
    }

    /// Appends a preprocessor [`ExpansionRecord`] and returns its non-root [`ExpansionId`].
    ///
    /// # Arguments
    ///
    /// * `parent` - Enclosing [`ExpansionId`] ([`ExpansionId::ROOT`] when top-level).
    /// * `call_site` - Directive call-site span in the parent file.
    /// * `kind` - Kind of expansion construct.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if expansion count exceeds `self.max_expansions`.
    pub fn record_expansion(
        &mut self,
        parent: ExpansionId,
        call_site: SourceSpan,
        kind: ExpansionKind,
    ) -> Result<ExpansionId> {
        if self.expansions.len() >= self.max_expansions as usize {
            return Err(CoblangError::InvalidDomainValue {
                domain_type: "ExpansionId",
                value: self.expansions.len().to_string(),
                reason: format!(
                    "SourceMap expansion table exceeded limit {}",
                    self.max_expansions
                ),
            });
        }
        self.expansions.push(ExpansionRecord {
            parent,
            call_site,
            kind,
        });
        let next_raw = u32::try_from(self.expansions.len()).unwrap_or(u32::MAX);
        Ok(ExpansionId::new(next_raw))
    }

    /// Resolves a [`SourceSpan`] to its primary [`ResolvedLocation`].
    ///
    /// # Arguments
    ///
    /// * `span` - Span to resolve.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `span.file_id` or `span.start` is invalid.
    pub fn resolve_span(&self, span: SourceSpan) -> Result<ResolvedLocation> {
        let file = self.get_file(span.file_id)?;
        let (line, column) = file.resolve_line_col(span.start)?;
        Ok(ResolvedLocation {
            file_path: String::from(file.path()),
            line,
            column,
        })
    }

    /// Unwinds all enclosing `COPY` / `REPLACE` / CDF expansion call sites from innermost to outermost.
    ///
    /// # Arguments
    ///
    /// * `span` - Primary diagnostic span whose `expansion_id` chain should be unwound.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if any [`ExpansionId`] or [`SourceSpan`] is invalid.
    pub fn unwind_expansion_chain(
        &self,
        span: SourceSpan,
    ) -> Result<Vec<(ResolvedLocation, ExpansionKind)>> {
        let mut chain = Vec::new();
        let mut cur = span.expansion_id;
        while !cur.is_root() {
            let slot = (cur.as_u32() - 1) as usize;
            let rec = self.expansions.get(slot).copied().ok_or_else(|| {
                CoblangError::InvalidDomainValue {
                    domain_type: "ExpansionId",
                    value: cur.as_u32().to_string(),
                    reason: String::from("unknown ExpansionId in SourceMap"),
                }
            })?;
            let loc = self.resolve_span(rec.call_site)?;
            chain.push((loc, rec.kind));
            cur = rec.parent;
        }
        Ok(chain)
    }
}
