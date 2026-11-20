//! Shared strongly-typed domain model, arena/span/bound newtypes, manifest codec, and unified error enum for `coblang`.

/// Validated COBOL level numbers, numeric precision bounds, exit codes, and `FILE STATUS` types.
pub mod bounds;
/// Strongly-typed compiler modes, dialects, capabilities, and taxonomy tags.
pub mod domain;
/// Single unified `derive_more` error enumeration (`CoblangError`) and `Result<T>` alias.
pub mod error;
/// Strongly-typed arena, file, scope, and symbol handles.
pub mod ids;
/// Deterministic line-oriented test manifest bundle encoder and decoder.
pub mod manifest;
/// Byte-exact source spans and 1-based line/column location types.
pub mod span;

pub use bounds::{
    CobolExitCode, DigitCount, FileStatusCode, FileStatusKind, LevelNumber, ResourceLimitKind,
    ScaleFactor,
};
pub use domain::{
    Capability, CompilationMode, DiagnosticSeverity, Dialect, DriverFlavor, FeatureTag, NistModule,
    OptimizationLevel, SourceFormat, SuiteId, TestOutcome, TextEncoding,
};
pub use error::{CoblangError, Result};
pub use ids::{
    DataItemId, ExpansionId, FileConnectorId, FileId, ProcedureId, ProgramUnitId, ReportId,
    ScopeId, SymbolId,
};
pub use manifest::{
    CompileCSpec, CompileCobolSpec, ExpectedDiagnostic, OutputMatcher, Provenance, RunBinarySpec,
    TestManifest, TestStep, WorkspaceFile, decode_manifest_bundle, encode_manifest_bundle,
    hex_decode, hex_decode_utf8, hex_encode,
};
pub use span::{ByteLength, ByteOffset, ColumnNumber, LineNumber, ResolvedLocation, SourceSpan};
