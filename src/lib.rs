//! Shared strongly-typed domain model, manifest codec, and unified error enum for `coblang`.

pub mod domain;
pub mod error;
pub mod manifest;

pub use domain::{
    Capability, CompilationMode, DiagnosticSeverity, Dialect, DriverFlavor, FeatureTag, NistModule,
    OptimizationLevel, SourceFormat, SuiteId, TestOutcome, TextEncoding,
};
pub use error::{CoblangError, Result};
pub use manifest::{
    CompileCSpec, CompileCobolSpec, ExpectedDiagnostic, OutputMatcher, Provenance, RunBinarySpec,
    TestManifest, TestStep, WorkspaceFile, decode_manifest_bundle, encode_manifest_bundle,
    hex_decode, hex_decode_utf8, hex_encode,
};
