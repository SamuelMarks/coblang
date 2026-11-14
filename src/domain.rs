//! Strongly-typed compiler modes, dialects, capabilities, and taxonomy tags for `coblang`.

use crate::error::{CoblangError, Result};

/// Compilation pipeline stage requested from the COBOL driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompilationMode {
    /// Preprocess `COPY` / `REPLACE` / CDF directives only (`-E`).
    PreprocessOnly,
    /// Lex, parse, and validate semantics without code generation (`-fsyntax-only`).
    SyntaxOnly,
    /// Emit target assembly text (`-S`).
    EmitAssembly,
    /// Emit textual LLVM IR (`-emit-llvm`).
    EmitLlvmIr,
    /// Compile to a relocatable native object file (`-c`).
    CompileObject,
    /// Compile to a dynamically loadable COBOL module (`-m` / `-shared`).
    CompileModule,
    /// Compile and link a standalone native executable (`-x` or default driver action).
    BuildExecutable,
}

impl CompilationMode {
    /// Returns the canonical manifest token for this compilation mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreprocessOnly => "preprocess",
            Self::SyntaxOnly => "syntax-only",
            Self::EmitAssembly => "emit-asm",
            Self::EmitLlvmIr => "emit-llvm",
            Self::CompileObject => "compile-object",
            Self::CompileModule => "compile-module",
            Self::BuildExecutable => "build-exe",
        }
    }

    /// Parses a [`CompilationMode`] from its canonical manifest token.
    ///
    /// # Arguments
    ///
    /// * `token` - Canonical string token representing the compilation mode.
    /// * `line` - Manifest line number used for error reporting.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ManifestFormat`] if `token` is unrecognized.
    pub fn parse_token(token: &str, line: usize) -> Result<Self> {
        match token {
            "preprocess" => Ok(Self::PreprocessOnly),
            "syntax-only" => Ok(Self::SyntaxOnly),
            "emit-asm" => Ok(Self::EmitAssembly),
            "emit-llvm" => Ok(Self::EmitLlvmIr),
            "compile-object" => Ok(Self::CompileObject),
            "compile-module" => Ok(Self::CompileModule),
            "build-exe" => Ok(Self::BuildExecutable),
            _ => Err(CoblangError::ManifestFormat {
                line,
                message: format!("unknown compilation mode `{token}`"),
            }),
        }
    }
}

/// COBOL source reference format layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceFormat {
    /// Infer format from directives (`>>SOURCE FORMAT`) or default dialect convention.
    Auto,
    /// Traditional 80-column fixed reference format (`-fixed` / `-ffixed-form`).
    Fixed,
    /// Modern free-form layout (`-free` / `-ffree-form` / `-F`).
    Free,
}

impl SourceFormat {
    /// Returns the canonical manifest token for this source format.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fixed => "fixed",
            Self::Free => "free",
        }
    }

    /// Parses a [`SourceFormat`] from its canonical manifest token.
    ///
    /// # Arguments
    ///
    /// * `token` - Canonical string token representing the source format.
    /// * `line` - Manifest line number used for error reporting.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ManifestFormat`] if `token` is unrecognized.
    pub fn parse_token(token: &str, line: usize) -> Result<Self> {
        match token {
            "auto" => Ok(Self::Auto),
            "fixed" => Ok(Self::Fixed),
            "free" => Ok(Self::Free),
            _ => Err(CoblangError::ManifestFormat {
                line,
                message: format!("unknown source format `{token}`"),
            }),
        }
    }
}

/// Standard or vendor COBOL dialect profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dialect {
    /// Default compiler dialect profile.
    Default,
    /// ANS X3.23-1985 / ISO 1989:1985 COBOL-85 standard.
    Cobol85,
    /// ISO/IEC 1989:2002 standard.
    Cobol2002,
    /// ISO/IEC 1989:2014 standard.
    Cobol2014,
    /// ISO/IEC 1989:2023 standard (`iso` in `gcobol`).
    Cobol2023,
    /// IBM Enterprise COBOL compatibility dialect.
    Ibm,
    /// Micro Focus COBOL compatibility dialect.
    MicroFocus,
    /// `GnuCOBOL` extended dialect (`gnu`).
    Gnu,
    /// Siemens BS2000 COBOL compatibility dialect.
    Bs2000,
    /// ACUCOBOL-GT compatibility dialect.
    Acu,
    /// X/Open CAE COBOL portability profile.
    XOpen,
}

impl Dialect {
    /// Returns the canonical manifest token for this dialect.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Cobol85 => "cobol85",
            Self::Cobol2002 => "cobol2002",
            Self::Cobol2014 => "cobol2014",
            Self::Cobol2023 => "cobol2023",
            Self::Ibm => "ibm",
            Self::MicroFocus => "mf",
            Self::Gnu => "gnu",
            Self::Bs2000 => "bs2000",
            Self::Acu => "acu",
            Self::XOpen => "xopen",
        }
    }

    /// Parses a [`Dialect`] from a CLI `-std=` / `-dialect` value or manifest token.
    ///
    /// # Arguments
    ///
    /// * `name` - Dialect identifier passed on CLI or stored in a manifest.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::InvalidCli`] when `name` is not a recognized dialect.
    pub fn from_cli_name(name: &str) -> Result<Self> {
        match name {
            "default" => Ok(Self::Default),
            "cobol85" | "cobol-85" => Ok(Self::Cobol85),
            "cobol2002" => Ok(Self::Cobol2002),
            "cobol2014" => Ok(Self::Cobol2014),
            "cobol2023" | "iso" => Ok(Self::Cobol2023),
            "ibm" | "ibm-strict" | "mvs" | "mvs-strict" => Ok(Self::Ibm),
            "mf" | "mf-strict" => Ok(Self::MicroFocus),
            "gnu" | "cobol-gnu" => Ok(Self::Gnu),
            "bs2000" | "bs2000-strict" => Ok(Self::Bs2000),
            "acu" | "acu-strict" => Ok(Self::Acu),
            "xopen" | "rm" | "rm-strict" => Ok(Self::XOpen),
            _ => Err(CoblangError::InvalidCli {
                message: format!("unsupported dialect `{name}`"),
            }),
        }
    }
}

/// Optimization level requested for code generation and torture testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OptimizationLevel {
    /// No optimization (`-O0`).
    O0,
    /// Basic optimization (`-O` / `-O1`).
    O1,
    /// Standard optimization (`-O2`).
    O2,
    /// Aggressive optimization including loop transformations (`-O3`).
    O3,
    /// Optimize for code size (`-Os`).
    Os,
}

impl OptimizationLevel {
    /// Returns the CLI flag string (`-O0`, `-O1`, `-O2`, `-O3`, `-Os`) for this level.
    #[must_use]
    pub const fn as_flag(self) -> &'static str {
        match self {
            Self::O0 => "-O0",
            Self::O1 => "-O1",
            Self::O2 => "-O2",
            Self::O3 => "-O3",
            Self::Os => "-Os",
        }
    }

    /// Parses an [`OptimizationLevel`] from its canonical flag representation.
    ///
    /// # Arguments
    ///
    /// * `flag` - Optimization flag string such as `"-O0"` or `"-Os"`.
    /// * `line` - Manifest line number used for error reporting (`0` for CLI parsing).
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ManifestFormat`] if `flag` is unrecognized.
    pub fn parse_flag(flag: &str, line: usize) -> Result<Self> {
        match flag {
            "-O0" => Ok(Self::O0),
            "-O" | "-O1" => Ok(Self::O1),
            "-O2" => Ok(Self::O2),
            "-O3" | "-Ofast" => Ok(Self::O3),
            "-Os" | "-Oz" => Ok(Self::Os),
            _ => Err(CoblangError::ManifestFormat {
                line,
                message: format!("unknown optimization flag `{flag}`"),
            }),
        }
    }
}

/// Internal runtime character set configuration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TextEncoding {
    /// Standard ASCII / UTF-8 execution character set.
    Ascii,
    /// Internal EBCDIC character set (`-finternal-ebcdic`).
    Ebcdic,
    /// Explicit `iconv` codepage name (e.g., `CP1140`).
    Codepage(String),
}

impl TextEncoding {
    /// Encodes this [`TextEncoding`] as a single manifest token.
    #[must_use]
    pub fn to_token(&self) -> String {
        match self {
            Self::Ascii => String::from("ascii"),
            Self::Ebcdic => String::from("ebcdic"),
            Self::Codepage(cp) => format!("codepage:{cp}"),
        }
    }

    /// Parses a [`TextEncoding`] from a manifest token produced by [`Self::to_token`].
    ///
    /// # Arguments
    ///
    /// * `token` - Encoded token (`"ascii"`, `"ebcdic"`, or `"codepage:<NAME>"`).
    /// * `line` - Manifest line number used for error reporting.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ManifestFormat`] if `token` is malformed.
    pub fn parse_token(token: &str, line: usize) -> Result<Self> {
        if token == "ascii" {
            return Ok(Self::Ascii);
        }
        if token == "ebcdic" {
            return Ok(Self::Ebcdic);
        }
        if let Some(cp) = token.strip_prefix("codepage:")
            && !cp.is_empty()
        {
            return Ok(Self::Codepage(String::from(cp)));
        }
        Err(CoblangError::ManifestFormat {
            line,
            message: format!("unknown text encoding `{token}`"),
        })
    }
}

/// CLI personality invoked on the scaffold or oracle driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriverFlavor {
    /// Native `coblang` LLVM COBOL frontend CLI.
    Coblang,
    /// `GnuCOBOL` `cobc` compatibility shim.
    Cobc,
    /// `GnuCOBOL` `cobcrun` dynamic module runner shim.
    Cobcrun,
    /// GCC COBOL `gcobol` compatibility shim.
    Gcobol,
}

/// Top-level test suite namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SuiteId {
    /// Internal self-test canary suite verifying the runner and shims.
    Canary,
    /// Ported GCC COBOL (`cobol.dg` + `libgcobol`) suite.
    Gcobol,
    /// Ported `GnuCOBOL` Autotest (`tests/testsuite.src/*.at`) suite.
    GnuCobol,
    /// Ported NIST CCVS-85 (`newcob.val`) standard conformance suite.
    Nist85,
}

impl SuiteId {
    /// Returns the canonical lowercase suite identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Canary => "canary",
            Self::Gcobol => "gcobol",
            Self::GnuCobol => "gnucobol",
            Self::Nist85 => "nist85",
        }
    }

    /// Parses a [`SuiteId`] from its canonical lowercase string.
    ///
    /// # Arguments
    ///
    /// * `token` - Suite name (`"canary"`, `"gcobol"`, `"gnucobol"`, `"nist85"`).
    /// * `line` - Manifest line number used for error reporting.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ManifestFormat`] if `token` is unrecognized.
    pub fn parse_token(token: &str, line: usize) -> Result<Self> {
        match token {
            "canary" => Ok(Self::Canary),
            "gcobol" => Ok(Self::Gcobol),
            "gnucobol" => Ok(Self::GnuCobol),
            "nist85" => Ok(Self::Nist85),
            _ => Err(CoblangError::ManifestFormat {
                line,
                message: format!("unknown suite `{token}`"),
            }),
        }
    }
}

/// Diagnostic severity level asserted by compile-time tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticSeverity {
    /// Hard compilation error (`dg-error` / `error:`).
    Error,
    /// Compiler warning (`dg-warning` / `warning:`).
    Warning,
    /// Informational compiler note (`dg-message` / `note:`).
    Note,
}

impl DiagnosticSeverity {
    /// Returns the canonical lowercase label for this severity.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
        }
    }

    /// Parses a [`DiagnosticSeverity`] from its canonical lowercase label.
    ///
    /// # Arguments
    ///
    /// * `token` - Severity label (`"error"`, `"warning"`, `"note"`).
    /// * `line` - Manifest line number used for error reporting.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ManifestFormat`] if `token` is unrecognized.
    pub fn parse_token(token: &str, line: usize) -> Result<Self> {
        match token {
            "error" => Ok(Self::Error),
            "warning" => Ok(Self::Warning),
            "note" => Ok(Self::Note),
            _ => Err(CoblangError::ManifestFormat {
                line,
                message: format!("unknown diagnostic severity `{token}`"),
            }),
        }
    }
}

/// Final classified outcome of executing a test case against expectations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestOutcome {
    /// Test succeeded as expected.
    Pass,
    /// Test failed unexpectedly (regression, assertion mismatch, timeout, or crash).
    Fail,
    /// Test failed as expected (`xfail` or unimplemented scaffold feature).
    XFail,
    /// Test marked `xfail` unexpectedly succeeded (triggers ratchet update prompt).
    XPass,
    /// Test skipped due to host capability predicate or explicit skip rule.
    Skip,
}

impl TestOutcome {
    /// Returns the standard uppercase `lit` / `DejaGnu` status code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::XFail => "XFAIL",
            Self::XPass => "XPASS",
            Self::Skip => "SKIP",
        }
    }

    /// Parses a [`TestOutcome`] from its uppercase status token.
    ///
    /// # Arguments
    ///
    /// * `token` - Status string (`"PASS"`, `"FAIL"`, `"XFAIL"`, `"XPASS"`, `"SKIP"`).
    /// * `line` - Line number in the expectation file for error reporting.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ManifestFormat`] if `token` is unrecognized.
    pub fn parse_token(token: &str, line: usize) -> Result<Self> {
        match token {
            "PASS" => Ok(Self::Pass),
            "FAIL" => Ok(Self::Fail),
            "XFAIL" => Ok(Self::XFail),
            "XPASS" => Ok(Self::XPass),
            "SKIP" => Ok(Self::Skip),
            _ => Err(CoblangError::ManifestFormat {
                line,
                message: format!("unknown test outcome `{token}`"),
            }),
        }
    }
}

/// Host or target environment capability required to execute a test case.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Capability {
    /// Host supports building and loading shared libraries (`.dylib` / `.so`).
    SharedLibraries,
    /// Host provides a working C compiler (`clang` / `cc`) for companion `.c` sources.
    CCompiler,
    /// Host provides standard POSIX filesystem & process semantics.
    PosixTools,
    /// Target byte order is big-endian.
    BigEndian,
    /// Target byte order is little-endian.
    LittleEndian,
    /// Target supports native 128-bit integer arithmetic (`__int128`).
    Int128,
    /// Runtime environment supports indexed ISAM files.
    Isam,
    /// Runtime environment supports XML parsing/generation.
    Xml,
    /// Runtime environment supports JSON parsing/generation.
    Json,
    /// Interactive terminal / PTY harness available for `SCREEN SECTION` TUI tests.
    CursesPty,
    /// Host `iconv` supports converting to/from the named character set (e.g., `CP1140`).
    IconvCodepage(String),
}

impl Capability {
    /// Converts this [`Capability`] into a canonical manifest token.
    #[must_use]
    pub fn to_token(&self) -> String {
        match self {
            Self::SharedLibraries => String::from("shared-libs"),
            Self::CCompiler => String::from("c-compiler"),
            Self::PosixTools => String::from("posix"),
            Self::BigEndian => String::from("big-endian"),
            Self::LittleEndian => String::from("little-endian"),
            Self::Int128 => String::from("int128"),
            Self::Isam => String::from("isam"),
            Self::Xml => String::from("xml"),
            Self::Json => String::from("json"),
            Self::CursesPty => String::from("curses-pty"),
            Self::IconvCodepage(cp) => format!("iconv:{cp}"),
        }
    }

    /// Parses a [`Capability`] from its canonical manifest token.
    ///
    /// # Arguments
    ///
    /// * `token` - Capability token string.
    /// * `line` - Manifest line number used for error reporting.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ManifestFormat`] if `token` is unrecognized.
    pub fn parse_token(token: &str, line: usize) -> Result<Self> {
        match token {
            "shared-libs" => Ok(Self::SharedLibraries),
            "c-compiler" => Ok(Self::CCompiler),
            "posix" => Ok(Self::PosixTools),
            "big-endian" => Ok(Self::BigEndian),
            "little-endian" => Ok(Self::LittleEndian),
            "int128" => Ok(Self::Int128),
            "isam" => Ok(Self::Isam),
            "xml" => Ok(Self::Xml),
            "json" => Ok(Self::Json),
            "curses-pty" => Ok(Self::CursesPty),
            _ => {
                if let Some(cp) = token.strip_prefix("iconv:")
                    && !cp.is_empty()
                {
                    return Ok(Self::IconvCodepage(String::from(cp)));
                }
                Err(CoblangError::ManifestFormat {
                    line,
                    message: format!("unknown capability `{token}`"),
                })
            }
        }
    }
}

/// Compiler capability and language-feature taxonomy tag used to sequence frontend milestones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FeatureTag {
    /// Source preprocessing (`fixed-form`, `free-form`, `COPY`, `REPLACE`, CDF directives).
    StagePreprocess,
    /// Syntax & semantic validation only (`-fsyntax-only` without codegen).
    StageParseSema,
    /// `PIC X` / `PIC A` alphanumeric & alphabetic items.
    TypeAlphanumeric,
    /// `USAGE DISPLAY` zoned decimal items.
    TypeNumericDisplay,
    /// Numeric-edited and alphanumeric-edited `PICTURE` items.
    TypeNumericEdited,
    /// `BINARY` / `COMP` / `COMP-4` / `COMP-5` binary integers.
    TypeBinaryComp5,
    /// `PACKED-DECIMAL` / `COMP-3` / `COMP-6` packed decimals.
    TypePackedComp3,
    /// `COMP-1` / `COMP-2` / `FLOAT-*` floating-point items.
    TypeFloat,
    /// `USAGE POINTER` and `PROGRAM-POINTER` items.
    TypePointer,
    /// `NATIONAL` (`UTF-16`) and `UTF-8` items/literals.
    TypeNationalUtf8,
    /// Extended 37-digit / 128-bit decimal precision.
    TypeDecimal128,
    /// Level-88 condition names (`VALUE`, `THRU`, `WHEN SET TO FALSE`).
    FeatureLevel88,
    /// `REDEFINES` and level-66 `RENAMES`.
    FeatureRedefinesRenames,
    /// Fixed `OCCURS` and `OCCURS DEPENDING ON` tables.
    FeatureOccursOdo,
    /// Reference modification `var(offset:length)`.
    FeatureRefmod,
    /// Arithmetic verbs (`ADD`, `SUBTRACT`, `MULTIPLY`, `DIVIDE`, `COMPUTE`, `ROUNDED`).
    FeatureArith,
    /// `MOVE` and `INITIALIZE` semantics.
    FeatureMoveInit,
    /// `STRING`, `UNSTRING`, `INSPECT`, and `TRANSFORM`.
    FeatureStringInspect,
    /// `PERFORM`, `EVALUATE`, `IF`, `GO TO`, and `ALTER`.
    FeaturePerformEvalIf,
    /// `SEARCH`, `SEARCH ALL`, and table `SORT`.
    FeatureSearchSortTable,
    /// `CALL`, `CANCEL`, `ENTRY`, and `LINKAGE SECTION`.
    FeatureCallSubprog,
    /// Nested programs, `GLOBAL`, and `EXTERNAL` sharing.
    FeatureNestedGlobalExternal,
    /// Dynamic memory (`ALLOCATE`, `FREE`, `BASED`).
    FeatureAllocateBased,
    /// `DECLARATIVES`, `USE`, and `EC-*` runtime exception handling.
    FeatureDeclarativesExceptions,
    /// Intrinsic `FUNCTION` library.
    FeatureIntrinsics,
    /// User-Defined Functions (`FUNCTION-ID`).
    FeatureUdf,
    /// `SEQUENTIAL` and `LINE SEQUENTIAL` file I/O.
    FeatureFileSeq,
    /// `RELATIVE` file I/O.
    FeatureFileRel,
    /// `INDEXED` (ISAM) file I/O.
    FeatureFileIdx,
    /// `SORT` and `MERGE` file pipelines.
    FeatureSortMerge,
    /// `REPORT SECTION` Report Writer module.
    FeatureReportWriter,
    /// `SCREEN SECTION` terminal UI handling.
    FeatureScreen,
    /// `XML GENERATE/PARSE` and `JSON GENERATE/PARSE`.
    FeatureXmlJson,
    /// Object-Oriented COBOL (`CLASS-ID`, `METHOD-ID`, `INVOKE`).
    FeatureOo,
    /// Companion C ABI interoperability and `CBL_*` / `C$*` routines.
    FeatureCInterop,
    /// EBCDIC character set and collating sequence support.
    FeatureEbcdic,
    /// ANS/ISO COBOL-85 standard dialect profile (`dialect:iso85`).
    DialectIso85,
    /// ISO COBOL 2002 standard dialect profile (`dialect:iso2002`).
    DialectIso2002,
    /// ISO COBOL 2014 standard dialect profile (`dialect:iso2014`).
    DialectIso2014,
    /// ISO COBOL 2023 standard dialect profile (`dialect:iso2023`).
    DialectIso2023,
    /// IBM Enterprise COBOL vendor dialect (`dialect:ibm`).
    DialectIbm,
    /// Micro Focus COBOL vendor dialect (`dialect:mf`).
    DialectMf,
    /// `GnuCOBOL` extended dialect (`dialect:gnucobol`).
    DialectGnucobol,
}

impl FeatureTag {
    /// Ordered slice of all defined [`FeatureTag`] variants.
    pub const ALL: &'static [Self] = &[
        Self::StagePreprocess,
        Self::StageParseSema,
        Self::TypeAlphanumeric,
        Self::TypeNumericDisplay,
        Self::TypeNumericEdited,
        Self::TypeBinaryComp5,
        Self::TypePackedComp3,
        Self::TypeFloat,
        Self::TypePointer,
        Self::TypeNationalUtf8,
        Self::TypeDecimal128,
        Self::FeatureLevel88,
        Self::FeatureRedefinesRenames,
        Self::FeatureOccursOdo,
        Self::FeatureRefmod,
        Self::FeatureArith,
        Self::FeatureMoveInit,
        Self::FeatureStringInspect,
        Self::FeaturePerformEvalIf,
        Self::FeatureSearchSortTable,
        Self::FeatureCallSubprog,
        Self::FeatureNestedGlobalExternal,
        Self::FeatureAllocateBased,
        Self::FeatureDeclarativesExceptions,
        Self::FeatureIntrinsics,
        Self::FeatureUdf,
        Self::FeatureFileSeq,
        Self::FeatureFileRel,
        Self::FeatureFileIdx,
        Self::FeatureSortMerge,
        Self::FeatureReportWriter,
        Self::FeatureScreen,
        Self::FeatureXmlJson,
        Self::FeatureOo,
        Self::FeatureCInterop,
        Self::FeatureEbcdic,
        Self::DialectIso85,
        Self::DialectIso2002,
        Self::DialectIso2014,
        Self::DialectIso2023,
        Self::DialectIbm,
        Self::DialectMf,
        Self::DialectGnucobol,
    ];

    /// Returns the canonical taxonomy tag string (matching Phase 5.1 of `TODO_PLAN.md`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StagePreprocess => "stage:preprocess",
            Self::StageParseSema => "stage:parse-sema",
            Self::TypeAlphanumeric => "type:alphanumeric",
            Self::TypeNumericDisplay => "type:numeric-display",
            Self::TypeNumericEdited => "type:numeric-edited",
            Self::TypeBinaryComp5 => "type:binary-comp5",
            Self::TypePackedComp3 => "type:packed-comp3",
            Self::TypeFloat => "type:float",
            Self::TypePointer => "type:pointer",
            Self::TypeNationalUtf8 => "type:national-utf8",
            Self::TypeDecimal128 => "type:128bit-decimal",
            Self::FeatureLevel88 => "feature:level-88",
            Self::FeatureRedefinesRenames => "feature:redefines-renames",
            Self::FeatureOccursOdo => "feature:occurs-odo",
            Self::FeatureRefmod => "feature:refmod",
            Self::FeatureArith => "feature:arith",
            Self::FeatureMoveInit => "feature:move-init",
            Self::FeatureStringInspect => "feature:string-inspect",
            Self::FeaturePerformEvalIf => "feature:perform-eval-if",
            Self::FeatureSearchSortTable => "feature:search-sort-table",
            Self::FeatureCallSubprog => "feature:call-subprog",
            Self::FeatureNestedGlobalExternal => "feature:nested-global-external",
            Self::FeatureAllocateBased => "feature:allocate-based",
            Self::FeatureDeclarativesExceptions => "feature:declaratives-exceptions",
            Self::FeatureIntrinsics => "feature:intrinsics",
            Self::FeatureUdf => "feature:udf",
            Self::FeatureFileSeq => "feature:file-seq",
            Self::FeatureFileRel => "feature:file-rel",
            Self::FeatureFileIdx => "feature:file-idx",
            Self::FeatureSortMerge => "feature:sort-merge",
            Self::FeatureReportWriter => "feature:report-writer",
            Self::FeatureScreen => "feature:screen",
            Self::FeatureXmlJson => "feature:xml-json",
            Self::FeatureOo => "feature:oo",
            Self::FeatureCInterop => "feature:c-interop",
            Self::FeatureEbcdic => "feature:ebcdic",
            Self::DialectIso85 => "dialect:iso85",
            Self::DialectIso2002 => "dialect:iso2002",
            Self::DialectIso2014 => "dialect:iso2014",
            Self::DialectIso2023 => "dialect:iso2023",
            Self::DialectIbm => "dialect:ibm",
            Self::DialectMf => "dialect:mf",
            Self::DialectGnucobol => "dialect:gnucobol",
        }
    }

    /// Parses a [`FeatureTag`] from its canonical taxonomy string.
    ///
    /// # Arguments
    ///
    /// * `token` - Taxonomy label such as `"feature:level-88"`.
    /// * `line` - Manifest line number used for error reporting.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ManifestFormat`] if `token` is unrecognized.
    pub fn parse_token(token: &str, line: usize) -> Result<Self> {
        for candidate in Self::ALL {
            if candidate.as_str() == token {
                return Ok(*candidate);
            }
        }
        Err(CoblangError::ManifestFormat {
            line,
            message: format!("unknown feature tag `{token}`"),
        })
    }
}

/// Twelve standard validation modules of the NIST CCVS-85 (COBOL-85) test suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NistModule {
    /// `NC` — Nucleus.
    Nc,
    /// `SM` — Source Text Manipulation (`COPY`, `REPLACE`).
    Sm,
    /// `IC` — Inter-Program Communication (`CALL`, `CANCEL`).
    Ic,
    /// `SQ` — Sequential I-O.
    Sq,
    /// `RL` — Relative I-O.
    Rl,
    /// `IX` — Indexed I-O.
    Ix,
    /// `ST` — Sort-Merge.
    St,
    /// `SG` — Segmentation.
    Sg,
    /// `OB` — Obsolete Language Elements.
    Ob,
    /// `IF` — Intrinsic Functions.
    If,
    /// `RW` — Report Writer.
    Rw,
    /// `DB` — Debug Module.
    Db,
}

impl NistModule {
    /// Ordered slice of all 12 NIST CCVS-85 modules.
    pub const ALL: &'static [Self] = &[
        Self::Nc,
        Self::Sm,
        Self::Ic,
        Self::Sq,
        Self::Rl,
        Self::Ix,
        Self::St,
        Self::Sg,
        Self::Ob,
        Self::If,
        Self::Rw,
        Self::Db,
    ];

    /// Returns the uppercase two-letter NIST module code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Nc => "NC",
            Self::Sm => "SM",
            Self::Ic => "IC",
            Self::Sq => "SQ",
            Self::Rl => "RL",
            Self::Ix => "IX",
            Self::St => "ST",
            Self::Sg => "SG",
            Self::Ob => "OB",
            Self::If => "IF",
            Self::Rw => "RW",
            Self::Db => "DB",
        }
    }

    /// Parses a [`NistModule`] from its two-letter uppercase code.
    ///
    /// # Arguments
    ///
    /// * `code` - Two-letter NIST module identifier (e.g., `"NC"`).
    /// * `line` - Line number used for error reporting.
    ///
    /// # Errors
    ///
    /// Returns [`CoblangError::ImportParse`] if `code` is not one of the 12 NIST modules.
    pub fn from_code(code: &str, line: usize) -> Result<Self> {
        for m in Self::ALL {
            if m.code() == code {
                return Ok(*m);
            }
        }
        Err(CoblangError::ImportParse {
            source_file: String::from("newcob.val"),
            line,
            message: format!("unknown NIST CCVS-85 module `{code}`"),
        })
    }
}
