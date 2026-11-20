//! Unified error enumeration for all `coblang` compiler, runtime, runner, and importer crates.

#![deny(clippy::indexing_slicing)]

use derive_more::{Display, Error, From};

use crate::bounds::{FileStatusCode, ResourceLimitKind};
use crate::domain::{FeatureTag, TextEncoding};

/// Unified result alias returning [`CoblangError`] on failure.
pub type Result<T> = core::result::Result<T, CoblangError>;

/// Single workspace-wide error enumeration covering frontend, MLIR/LLVM, runtime, CLI, and harness failures.
#[derive(Debug, Display, Error, From)]
pub enum CoblangError {
    /// Standard I/O failure encountered while reading or writing workspace artifacts.
    #[display("I/O error: {_0}")]
    #[from]
    Io(std::io::Error),

    /// Invalid command-line argument or unsupported flag combination.
    #[display("invalid CLI usage: {message}")]
    #[from(ignore)]
    InvalidCli {
        /// Human-readable description of the CLI error.
        message: String,
    },

    /// Malformed serialized test manifest or expectation entry.
    #[display("manifest format error at line {line}: {message}")]
    #[from(ignore)]
    ManifestFormat {
        /// One-based line number in the manifest text where parsing failed.
        line: usize,
        /// Human-readable description of the format violation.
        message: String,
    },

    /// Invalid regular expression encountered in a test directive or diagnostic matcher.
    #[display("invalid regex `{pattern}`: {reason}")]
    #[from(ignore)]
    InvalidRegex {
        /// Regular expression pattern string that failed to compile.
        pattern: String,
        /// Underlying regex parser failure message.
        reason: String,
    },

    /// Parse failure while importing an upstream test file (`cobol.dg`, `.at`, or `newcob.val`).
    #[display("importer parse error in `{source_file}`:{line}: {message}")]
    #[from(ignore)]
    ImportParse {
        /// Path of the upstream file being imported.
        source_file: String,
        /// One-based line number in the upstream file.
        line: usize,
        /// Human-readable description of the parse error.
        message: String,
    },

    /// Process execution or sandbox error inside the test runner.
    #[display("runner execution error for `{test_id}`: {message}")]
    #[from(ignore)]
    RunnerExecution {
        /// Canonical identifier of the test case being executed.
        test_id: String,
        /// Human-readable description of the execution failure.
        message: String,
    },

    /// Expectation ratchet failure (`PASS` regression, crash, or unexpected `XPASS`).
    #[display("ratchet violation: {summary}")]
    #[from(ignore)]
    RatchetViolation {
        /// Summary of all expectation regressions or unexpected passes.
        summary: String,
    },

    /// Malformed `.conf` dialect configuration file or `-cb_conf=` override.
    #[display("dialect configuration error in `{path}`:{line}: {message}")]
    #[from(ignore)]
    ConfigLoad {
        /// Configuration file path or `"<cli>"`.
        path: String,
        /// One-based line number inside `path`.
        line: usize,
        /// Detailed failure description.
        message: String,
    },

    /// Invalid source or runtime character-encoding byte sequence.
    #[display(
        "character encoding error ({}) at byte {byte_offset}: {message}",
        encoding.to_token()
    )]
    #[from(ignore)]
    EncodingConversion {
        /// Active character set configuration.
        encoding: TextEncoding,
        /// Zero-based byte offset where decoding/encoding failed.
        byte_offset: usize,
        /// Detailed conversion failure message.
        message: String,
    },

    /// Preprocessor failure (unclosed continuation, missing/recursive `COPY`, or invalid CDF directive).
    #[display("{file}:{line}:{column}: error: {message}")]
    #[from(ignore)]
    PreprocessError {
        /// Source or copybook file path.
        file: String,
        /// One-based line number.
        line: u32,
        /// One-based column number.
        column: u32,
        /// Preprocessor error description.
        message: String,
    },

    /// Lexical analysis error (invalid literal, malformed hex/national token, or invalid `PICTURE`).
    #[display("{file}:{line}:{column}: error: {message}")]
    #[from(ignore)]
    LexError {
        /// Source file path.
        file: String,
        /// One-based line number.
        line: u32,
        /// One-based column number.
        column: u32,
        /// Lexer error description.
        message: String,
    },

    /// Syntactic division, clause, or statement parse error.
    #[display("{file}:{line}:{column}: error: expected {expected}, found {found}")]
    #[from(ignore)]
    ParseError {
        /// Source file path.
        file: String,
        /// One-based line number.
        line: u32,
        /// One-based column number.
        column: u32,
        /// Expected grammar construct description.
        expected: String,
        /// Encountered token description.
        found: String,
    },

    /// One or more hard semantic, type, or storage-layout errors halted compilation.
    #[display("semantic validation failed with {diagnostic_count} error(s): {first_message}")]
    #[from(ignore)]
    SemanticError {
        /// Total number of hard error diagnostics emitted.
        diagnostic_count: usize,
        /// Message text of the first emitted hard error.
        first_message: String,
    },

    /// Domain newtype smart-constructor validation failure.
    #[display("invalid {domain_type} value `{value}`: {reason}")]
    #[from(ignore)]
    InvalidDomainValue {
        /// Name of the domain newtype (`LevelNumber`, `DigitCount`, `LineNumber`, etc.).
        domain_type: &'static str,
        /// Rejected input representation.
        value: String,
        /// Violated domain invariant explanation.
        reason: String,
    },

    /// AST-to-HIR desugaring or canonicalization failure.
    #[display("HIR lowering error for `{construct}` at {location}: {message}")]
    #[from(ignore)]
    HirLowering {
        /// Syntactic construct name being lowered.
        construct: &'static str,
        /// Formatted `file:line:col` location.
        location: String,
        /// Detailed lowering failure explanation.
        message: String,
    },

    /// Custom `cobol` or `llvm` MLIR module verification failure.
    #[display("MLIR verification failed in stage `{stage}`: {message} ({ir_snippet})")]
    #[from(ignore)]
    MlirVerification {
        /// Pipeline stage where verification ran.
        stage: &'static str,
        /// Offending operation or IR excerpt.
        ir_snippet: String,
        /// Verifier diagnostic message.
        message: String,
    },

    /// MLIR `PassManager` optimization or conversion pipeline failure.
    #[display("MLIR pass pipeline `{pass_pipeline}` failed: {message}")]
    #[from(ignore)]
    MlirPassExecution {
        /// Name or textual description of the pass pipeline.
        pass_pipeline: String,
        /// Detailed failure message.
        message: String,
    },

    /// LLVM 20 target lookup, data-layout configuration, or machine-code emission failure.
    #[display("LLVM target codegen error for `{triple}`: {message}")]
    #[from(ignore)]
    CodegenTarget {
        /// Target triple string.
        triple: String,
        /// LLVM error message.
        message: String,
    },

    /// System linker invocation failure while producing an executable or shared module.
    #[display("linker `{linker_bin}` failed (exit {exit_code:?}): {stderr}")]
    #[from(ignore)]
    LinkerInvocation {
        /// Invoked linker binary path/name.
        linker_bin: String,
        /// Optional linker exit status code.
        exit_code: Option<i32>,
        /// Captured linker standard error output.
        stderr: String,
    },

    /// Exact decimal or BCD domain error (division by zero, invalid nibble, invalid exponent).
    #[display("decimal domain error in `{operation}`: {message}")]
    #[from(ignore)]
    DecimalDomain {
        /// Arithmetic or codec operation name.
        operation: &'static str,
        /// Detailed domain failure explanation.
        message: String,
    },

    /// `PICTURE` editing bytecode compilation or de-editing runtime failure.
    #[display("PICTURE codec error for `{picture}`: {message}")]
    #[from(ignore)]
    PictureCodec {
        /// Raw or canonical `PICTURE` string.
        picture: String,
        /// Detailed failure explanation.
        message: String,
    },

    /// Unhandled COBOL runtime file I/O failure.
    #[display(
        "runtime file I/O error on `{file_name}` (status `{}`): {message}",
        status.as_str()
    )]
    #[from(ignore)]
    RuntimeFileIo {
        /// Logical or physical COBOL file name.
        file_name: String,
        /// Resulting two-character COBOL `FILE STATUS` code.
        status: FileStatusCode,
        /// Detailed I/O error description.
        message: String,
    },

    /// Pure-Rust ISAM B+Tree page corruption, checksum mismatch, or key index violation.
    #[display("ISAM storage error in `{path}` (page {page_id}): {message}")]
    #[from(ignore)]
    IsamStorage {
        /// ISAM file path.
        path: String,
        /// Zero-based B+Tree page identifier.
        page_id: u64,
        /// Detailed storage error description.
        message: String,
    },

    /// `SORT` or `MERGE` session state or external run-spill failure.
    #[display("SORT/MERGE error: {message}")]
    #[from(ignore)]
    SortMerge {
        /// Detailed sort/merge failure message.
        message: String,
    },

    /// `REPORT SECTION` (`INITIATE` / `GENERATE` / `TERMINATE`) state machine violation.
    #[display("report writer error in `{report_name}`: {message}")]
    #[from(ignore)]
    ReportEngine {
        /// Report descriptor (`RD`) name.
        report_name: String,
        /// Detailed protocol or layout error message.
        message: String,
    },

    /// Terminal or headless PTY `SCREEN SECTION` backend error.
    #[display("screen backend error: {message}")]
    #[from(ignore)]
    ScreenBackend {
        /// Detailed terminal/screen error message.
        message: String,
    },

    /// `XML` or `JSON` streaming parser or generator error.
    #[display("{kind} processing error at byte {offset}: {message}")]
    #[from(ignore)]
    XmlJsonProcess {
        /// `"XML"` or `"JSON"`.
        kind: &'static str,
        /// Zero-based byte offset where processing failed.
        offset: usize,
        /// Detailed error message.
        message: String,
    },

    /// Dynamic `CALL` or `cobcrun` shared-module loader failure.
    #[display("dynamic module `{module_name}` load failed (searched {searched_paths}): {message}")]
    #[from(ignore)]
    DynamicModuleLoad {
        /// Requested COBOL program/module identifier.
        module_name: String,
        /// Colon-formatted search path summary.
        searched_paths: String,
        /// Underlying loader error message.
        message: String,
    },

    /// Audited C-ABI boundary guard rejection on a null or misaligned pointer argument.
    #[display("C-ABI symbol `{symbol}` received null or invalid pointer for `{param_name}`")]
    #[from(ignore)]
    AbiNullOrInvalidPointer {
        /// Exported `extern "C"` symbol name.
        symbol: &'static str,
        /// Parameter identifier that failed validation.
        param_name: &'static str,
    },

    /// Language feature not yet lowered during incremental ratchet bring-up (`EXIT_UNIMPLEMENTED = 78`).
    #[display(
        "unimplemented COBOL feature `{}` (`{construct}`) at {location}",
        feature.as_str()
    )]
    #[from(ignore)]
    UnimplementedFeature {
        /// Taxonomy [`FeatureTag`] associated with the construct.
        feature: FeatureTag,
        /// Specific AST/HIR/CLI construct name.
        construct: String,
        /// Formatted `file:line:col` location.
        location: String,
    },

    /// Deterministic memory/CPU safeguard budget exceeded during compilation or test execution.
    #[display(
        "resource limit `{}` exceeded: observed {observed} > limit {limit}",
        resource.as_str()
    )]
    #[from(ignore)]
    ResourceLimitExceeded {
        /// Specific bounded resource category that tripped.
        resource: ResourceLimitKind,
        /// Configured hard limit ceiling.
        limit: usize,
        /// Observed count or byte length that exceeded `limit`.
        observed: usize,
    },
}
