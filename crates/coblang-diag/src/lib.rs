//! Expansion-aware source maps, GCC/cobc-compatible diagnostics, and dialect configurations.

#![deny(clippy::indexing_slicing)]

/// Strongly-typed COBOL dialect policies and `.conf` configuration loader.
pub mod dialect_conf;
/// Built-in EBCDIC<->ASCII translation tables and `.ttbl` custom table parser.
pub mod ebcdic_table;
/// Structured compiler diagnostic records and GCC/`cobc`-compatible text emitter.
pub mod emitter;
/// Case-folding COBOL word interner preserving raw identifier spelling.
pub mod interner;
/// Byte-exact source file table and `COPY` / `REPLACE` / CDF expansion backtraces.
pub mod source_map;
/// Compiler warning groups and `-W*` / `-Werror` CLI flag state machine.
pub mod warning;

pub use dialect_conf::{
    AssignClauseDefault, BinaryByteOrder, BinarySizePolicy, BoundCheckToggle,
    CliIntrinsicImportMode, DefaultColSeqMode, DialectConfig, DisplayFormatMode, DisplaySignMode,
    DpcInDataMode, FeatureDisposition, InitialBoundsChecks, OdoComplexity, OdoSlidePolicy,
    PerformReturnStyle, RedefinesChainPolicy, RelaxedSyntaxMode, ScreenConflictPolicy,
    ScreenSectionRules, StickyLinkageMode, TruncationMode, XmlParseMode,
};
pub use ebcdic_table::{DEFAULT_EBCDIC_TO_ASCII, EbcdicTranslationTables, InternalCharsetMode};
pub use emitter::{Diagnostic, DiagnosticCode, DiagnosticNote, DiagnosticSink};
pub use interner::SymbolInterner;
pub use source_map::{ExpansionKind, ExpansionRecord, SourceFile, SourceMap};
pub use warning::{WarningGroup, WarningPolicy};
