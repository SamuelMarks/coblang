//! Unified error enumeration for all `coblang` scaffold, runner, and importer crates.

use derive_more::{Display, Error, From};

/// Unified result alias returning [`CoblangError`] on failure.
pub type Result<T> = core::result::Result<T, CoblangError>;

/// Single workspace-wide error enumeration covering CLI, I/O, manifest, importer, and runner failures.
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
}
