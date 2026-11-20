//! Strongly-typed arena and symbol identifiers for compiler IRs.

#![deny(clippy::indexing_slicing)]

/// Strongly-typed identifier for a loaded source file or copybook in a source map.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(
    /// Zero-based index inside the owning source map.
    u32,
);

impl FileId {
    /// Constructs a [`FileId`] from a zero-based index.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based index inside the owning source map.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying zero-based index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Strongly-typed identifier for a `COPY`, `REPLACE`, or CDF directive expansion frame.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExpansionId(
    /// Raw expansion frame table index (`0` denotes [`ExpansionId::ROOT`]).
    u32,
);

impl ExpansionId {
    /// Root unexpanded source context (`0`).
    pub const ROOT: Self = Self(0);

    /// Constructs an [`ExpansionId`] from a raw frame index.
    ///
    /// # Arguments
    ///
    /// * `raw` - Expansion table index (`0` denotes [`Self::ROOT`]).
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw expansion table index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Returns `true` when this identifier represents [`Self::ROOT`].
    #[must_use]
    pub const fn is_root(self) -> bool {
        self.0 == 0
    }
}

/// Strongly-typed handle for an interned COBOL word or external symbol name.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolId(
    /// Zero-based interner slot index.
    u32,
);

impl SymbolId {
    /// Constructs a [`SymbolId`] from an interner slot index.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based interner slot index.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying zero-based interner slot index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Strongly-typed identifier for a lexical program, function, or method symbol scope.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeId(
    /// Zero-based lexical scope arena index.
    u32,
);

impl ScopeId {
    /// Constructs a [`ScopeId`] from a zero-based scope arena index.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based scope arena index.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying zero-based scope arena index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Strongly-typed identifier for a compilation unit (`PROGRAM-ID`, `FUNCTION-ID`, or `CLASS-ID`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProgramUnitId(
    /// Zero-based compilation unit arena index.
    u32,
);

impl ProgramUnitId {
    /// Constructs a [`ProgramUnitId`] from a zero-based unit index.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based compilation unit index.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying zero-based compilation unit index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Strongly-typed handle for a resolved Data Division item.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DataItemId(
    /// Zero-based Data Division item arena index.
    u32,
);

impl DataItemId {
    /// Constructs a [`DataItemId`] from a zero-based item arena index.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based data item arena index.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying zero-based data item arena index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Strongly-typed handle for a resolved Procedure Division section or paragraph.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcedureId(
    /// Zero-based Procedure Division target arena index.
    u32,
);

impl ProcedureId {
    /// Constructs a [`ProcedureId`] from a zero-based procedure arena index.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based procedure arena index.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying zero-based procedure arena index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Strongly-typed handle for a resolved `SELECT` / `FD` / `SD` file connector.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileConnectorId(
    /// Zero-based file connector arena index.
    u32,
);

impl FileConnectorId {
    /// Constructs a [`FileConnectorId`] from a zero-based file connector index.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based file connector arena index.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying zero-based file connector index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Strongly-typed handle for a resolved `RD` report descriptor.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReportId(
    /// Zero-based report descriptor arena index.
    u32,
);

impl ReportId {
    /// Constructs a [`ReportId`] from a zero-based report descriptor index.
    ///
    /// # Arguments
    ///
    /// * `raw` - Zero-based report descriptor index.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the underlying zero-based report descriptor index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}
