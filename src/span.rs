//! Byte-exact source spans and 1-based line/column coordinates.

#![deny(clippy::indexing_slicing)]

use core::num::NonZeroU32;

use crate::error::{CoblangError, Result};
use crate::ids::{ExpansionId, FileId};

/// Zero-based byte offset inside a source file buffer.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteOffset(
    /// Raw zero-based byte index within the owning file buffer.
    u32,
);

impl ByteOffset {
    /// Zero byte offset at the start of a file.
    pub const ZERO: Self = Self(0);

    /// Constructs a [`ByteOffset`] from a raw byte index.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based byte offset.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw zero-based byte offset.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Returns `self + len` as a checked [`ByteOffset`].
    ///
    /// # Arguments
    ///
    /// * `len` - Byte length to advance by.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `self.0 + len.0` overflows `u32`.
    pub fn checked_advance(self, len: ByteLength) -> Result<Self> {
        self.0
            .checked_add(len.as_u32())
            .map(Self)
            .ok_or_else(|| CoblangError::InvalidDomainValue {
                domain_type: "ByteOffset",
                value: format!("{} + {}", self.0, len.as_u32()),
                reason: String::from("byte offset overflowed u32::MAX"),
            })
    }

    /// Returns `self - len` as a checked [`ByteOffset`].
    ///
    /// # Arguments
    ///
    /// * `len` - Byte length to step backward by.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `self.0 < len.0`.
    pub fn checked_sub(self, len: ByteLength) -> Result<Self> {
        self.0
            .checked_sub(len.as_u32())
            .map(Self)
            .ok_or_else(|| CoblangError::InvalidDomainValue {
                domain_type: "ByteOffset",
                value: format!("{} - {}", self.0, len.as_u32()),
                reason: String::from("byte offset underflowed zero"),
            })
    }
}

/// Byte length of a source token or syntactic construct.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteLength(
    /// Raw byte count covered by the span.
    u32,
);

impl ByteLength {
    /// Zero-length span (e.g. EOF or synthesized insertion point).
    pub const ZERO: Self = Self(0);

    /// Constructs a [`ByteLength`] from a raw byte count.
    ///
    /// # Arguments
    ///
    /// * `raw` - Number of bytes covered.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw byte count.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Returns `self + other` as a checked [`ByteLength`].
    ///
    /// # Arguments
    ///
    /// * `other` - Additional byte length to add.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `self.0 + other.0` overflows `u32`.
    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or_else(|| CoblangError::InvalidDomainValue {
                domain_type: "ByteLength",
                value: format!("{} + {}", self.0, other.0),
                reason: String::from("byte length overflowed u32::MAX"),
            })
    }

    /// Returns `self - other` as a checked [`ByteLength`].
    ///
    /// # Arguments
    ///
    /// * `other` - Byte length to subtract from `self`.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `self.0 < other.0`.
    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or_else(|| CoblangError::InvalidDomainValue {
                domain_type: "ByteLength",
                value: format!("{} - {}", self.0, other.0),
                reason: String::from("byte length underflowed zero"),
            })
    }
}

/// Validated one-based source line number (`>= 1`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineNumber(
    /// Non-zero one-based line number.
    NonZeroU32,
);

impl LineNumber {
    /// Validated first line (`1`).
    pub const ONE: Self = Self(NonZeroU32::MIN);

    /// Constructs a [`LineNumber`] from a one-based integer (`>= 1`).
    ///
    /// # Arguments
    ///
    /// * `raw` - One-based line index (`0` is rejected).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw == 0`.
    pub fn try_from_one_based(raw: u32) -> Result<Self> {
        NonZeroU32::new(raw)
            .map(Self)
            .ok_or_else(|| CoblangError::InvalidDomainValue {
                domain_type: "LineNumber",
                value: String::from("0"),
                reason: String::from("line numbers are 1-based and must be >= 1"),
            })
    }

    /// Constructs a 1-based [`LineNumber`] from a 0-based line index (`zero_based + 1`).
    ///
    /// # Arguments
    ///
    /// * `zero_based` - Zero-based line index (`0` maps to line `1`).
    #[must_use]
    pub const fn from_zero_based(zero_based: u32) -> Self {
        Self(NonZeroU32::MIN.saturating_add(zero_based))
    }

    /// Returns the underlying one-based `u32` line number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// Validated one-based source column number (`>= 1`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ColumnNumber(
    /// Non-zero one-based column number.
    NonZeroU32,
);

impl ColumnNumber {
    /// Validated first column (`1`).
    pub const ONE: Self = Self(NonZeroU32::MIN);

    /// Constructs a [`ColumnNumber`] from a one-based integer (`>= 1`).
    ///
    /// # Arguments
    ///
    /// * `raw` - One-based column index (`0` is rejected).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidDomainValue`] if `raw == 0`.
    pub fn try_from_one_based(raw: u32) -> Result<Self> {
        NonZeroU32::new(raw)
            .map(Self)
            .ok_or_else(|| CoblangError::InvalidDomainValue {
                domain_type: "ColumnNumber",
                value: String::from("0"),
                reason: String::from("column numbers are 1-based and must be >= 1"),
            })
    }

    /// Constructs a 1-based [`ColumnNumber`] from a 0-based column offset (`zero_based + 1`).
    ///
    /// # Arguments
    ///
    /// * `zero_based` - Zero-based column offset (`0` maps to column `1`).
    #[must_use]
    pub const fn from_zero_based(zero_based: u32) -> Self {
        Self(NonZeroU32::MIN.saturating_add(zero_based))
    }

    /// Returns the underlying one-based `u32` column number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// Half-open byte span inside a [`FileId`] paired with its [`ExpansionId`] trace context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    /// Owning source or copybook file identifier.
    pub file_id: FileId,
    /// Starting byte offset within `file_id`.
    pub start: ByteOffset,
    /// Byte length of the span.
    pub len: ByteLength,
    /// Preprocessor expansion frame active where this span was produced.
    pub expansion_id: ExpansionId,
}

impl SourceSpan {
    /// Constructs a new [`SourceSpan`].
    ///
    /// # Arguments
    ///
    /// * `file_id` - Owning source file handle.
    /// * `start` - Zero-based starting byte offset.
    /// * `len` - Byte length of the span.
    /// * `expansion_id` - Active preprocessor expansion frame.
    #[must_use]
    pub const fn new(
        file_id: FileId,
        start: ByteOffset,
        len: ByteLength,
        expansion_id: ExpansionId,
    ) -> Self {
        Self {
            file_id,
            start,
            len,
            expansion_id,
        }
    }

    /// Computes the saturating end [`ByteOffset`] (`start + len`).
    #[must_use]
    pub const fn end_saturating(self) -> ByteOffset {
        ByteOffset::new(self.start.as_u32().saturating_add(self.len.as_u32()))
    }

    /// Merges two spans from the same file into a bounding [`SourceSpan`].
    ///
    /// When `other.file_id != self.file_id`, returns `self` unchanged so cross-copybook
    /// AST nodes retain their primary leading span deterministically.
    ///
    /// # Arguments
    ///
    /// * `other` - Trailing span to merge into `self`.
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        if self.file_id.as_u32() != other.file_id.as_u32() {
            return self;
        }
        let min_start = if self.start.as_u32() <= other.start.as_u32() {
            self.start.as_u32()
        } else {
            other.start.as_u32()
        };
        let self_end = self.end_saturating().as_u32();
        let other_end = other.end_saturating().as_u32();
        let max_end = if self_end >= other_end {
            self_end
        } else {
            other_end
        };
        Self {
            file_id: self.file_id,
            start: ByteOffset::new(min_start),
            len: ByteLength::new(max_end - min_start),
            expansion_id: self.expansion_id,
        }
    }
}

/// Human-readable `file:line:column` location resolved from a [`SourceSpan`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedLocation {
    /// Display path of the source or copybook file.
    pub file_path: String,
    /// Validated 1-based line number.
    pub line: LineNumber,
    /// Validated 1-based column number.
    pub column: ColumnNumber,
}

impl ResolvedLocation {
    /// Formats this location in canonical `file:line:col` notation.
    #[must_use]
    pub fn to_header_prefix(&self) -> String {
        format!(
            "{}:{}:{}",
            self.file_path,
            self.line.get(),
            self.column.get()
        )
    }
}
