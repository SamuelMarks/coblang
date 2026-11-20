# `coblang` LLVM/MLIR 20 COBOL Frontend & Pure-Rust Runtime Implementation Plan

This document specifies the end-to-end engineering plan to transform `coblang` from its current zero-compiler scaffold (`0 PASS / 2,323 XFAIL / 204 SKIP` across **2,527** conformance tests) into a production-grade, multi-dialect **MLIR 20 (`melior`) -> LLVM 20** COBOL compiler frontend and **pure-Rust `libcob` / `libgcobol` ABI-compatible runtime** licensed under `Apache-2.0 WITH LLVM-exception`.

---

## 1. Architectural Overview & Non-Negotiable Quality Invariants

### 1.1 Compilation & Runtime Pipeline

```text
COBOL Source (.cob / .cbl / .cpy)
  │
  ▼  [crates/coblang-pp]         Fixed/Free/Auto normalization, COPY/REPLACING, REPLACE, >>DEFINE/>>IF CDF
Expanded Source + SpanMap
  │
  ▼  [crates/coblang-syntax]     Context-sensitive Lexer + Lossless CST & Typed AST (4 Divisions, UDF, OO)
AST
  │
  ▼  [crates/coblang-sema]       Qualified Symbol Resolution, PICTURE Parser, Storage Layout, Dialect Rules
Typed HIR (crates/coblang-hir)
  │
  ▼  [crates/coblang-mlir]       HIR -> Custom `cobol` MLIR Dialect (melior / MLIR 20)
`cobol` Dialect MLIR Module
  │
  ▼  [crates/coblang-mlir]       COBOL Canonicalization & Fast-Path Specialization (i64/i128 arith + CFG)
  │                              + Lowering to `func` / `scf` / `cf` / `arith` / `memref` / `llvm` Dialects
LLVM Dialect MLIR Module
  │
  ▼  [crates/coblang-codegen]    MLIR PassManager (-O0..-Os) -> LLVM 20 IR (.ll) -> TargetMachine (.s / .o)
Native Object / Shared Module / Executable
  │
  ▼  [crates/coblang-rt-abi]     Clean-room `extern "C"` libcob (`cob_*`, `CBL_*`, `C$*`) & libgcobol (`__gg__*`)
  │                              backed 100% by pure-Rust safe crates (`decimal`, `pic`, `fileio`, `isam`, etc.)
Native Execution / `cobcrun` Dynamic Module Loader (`crates/coblang-dynload`)
```

### 1.2 Mandatory Quality Gates (Enforced at Every Single Milestone)

Every vertical slice MUST pass `./scripts/check-quality.sh` with zero exceptions:

1. **Formatting**: `cargo fmt --all -- --check` passes cleanly.
2. **Strictest Clippy & Rustc Lints (`Cargo.toml`)**:
   - `edition = "2024"`, `license = "Apache-2.0 WITH LLVM-exception"`.
   - `unwrap_used = "forbid"`, `expect_used = "forbid"`, `todo = "forbid"`, `unimplemented = "forbid"`, `dbg_macro = "forbid"`, `panic = "deny"`.
   - `unsafe_code = "forbid"` inherited via `[lints] workspace = true` on **all** compiler & runtime logic crates; only the three audited thin FFI crates (`crates/coblang-codegen`, `crates/coblang-rt-abi`, `crates/coblang-dynload`) configure `unsafe_code = "deny"` with explicit `#[expect(unsafe_code, reason = "...")]` annotations for Rust 2024 `#[unsafe(no_mangle)] extern "C"`, C-pointer slice validation, LLVM/MLIR C-API calls, and `dlopen`/`dlsym`.
3. **100% Documentation Coverage (`rustdoc` + `scripts/update-readme-shields.py --metric doc --verify-strict-100`)**:
   - Every `.rs` file across `src/`, `crates/`, `tools/`, and `tests/` begins on line 1 with `//!`.
   - Every public and private item (`fn`, `struct`, `enum`, `trait`, `type`, `const`, `static`, `macro_rules!`) carries `///` docs.
   - Every non-test function taking parameters (excluding `self`) includes `# Arguments` documenting every parameter `` `name` ``, plus `# Errors` and `# Panics` sections where required by Clippy.
4. **Single Unified Error Enum (`src/error.rs`)**:
   - All fallible Rust APIs return `coblang_common::Result<T>` (`core::result::Result<T, CoblangError>`) using `derive_more::{Display, Error, From}`. Never use `anyhow` or `thiserror`.
5. **GPLv3 Clean-Room Boundary (`tests/LICENSE_MANIFEST.md`)**:
   - Zero `include_str!` / `include_bytes!` from `tests/ported/`, zero copying of `GnuCOBOL` or `GCC` source code, and zero linking against GPLv3 `libcob` or `libgcobol`. All runtime ABIs are clean-room implementations derived from public specifications and black-box behavioral tests.
6. **100% Function, Line, and Branch Coverage**:
   - `cargo +nightly llvm-cov --workspace --branch --json --summary-only` MUST report `covered == count` (`100.00%`) across **functions**, **lines**, and **branches**. Avoid unreachable catch-all match arms or defensive dead branches; test every error path with unit tests.
7. **Monotonic Conformance Ratchet (`2,527` Tests)**:
   - Unimplemented AST/HIR/MLIR features deterministically return `EXIT_UNIMPLEMENTED` (`78`), which `coblang-test` classifies as `XFail`.
   - As each slice lands, newly passing tests flip `XFAIL -> PASS` in `tests/expectations/baseline.toml`, `SCOREBOARD.md`, and `SCOREBOARD.json`, verified via:
     ```bash
     cargo run -p coblang-test-runner --bin coblang-test -- run 
       --bundle-dir tests --baseline tests/expectations/baseline.toml --ratchet -j 8
     ```

### 1.3 Target Workspace Crate Topology

| Crate | Path | `unsafe_code` Policy | Responsibility |
| :--- | :--- | :--- | :--- |
| `coblang-common` | `.` | `forbid` | Shared domain enums, spans, manifest codec, unified `CoblangError`. |
| `coblang-diag` | `crates/coblang-diag` | `forbid` | `SourceMap`, `Span`, expansion backtraces, warning groups, `-conf=` dialect rules, diagnostic emitter. |
| `coblang-pp` | `crates/coblang-pp` | `forbid` | Fixed/Free/Auto scanner, `COPY`/`REPLACING`, `REPLACE`, ISO CDF (`>>DEFINE`, `>>IF`, `>>EVALUATE`, `>>TURN`), `-t`/`-T` listings. |
| `coblang-syntax` | `crates/coblang-syntax` | `forbid` | Context-sensitive lexer, lossless CST, typed AST across all 4 divisions, nested programs, UDFs, OO COBOL. |
| `coblang-hir` | `crates/coblang-hir` | `forbid` | Canonical desugared High-Level IR, `PictureSpec`, `StorageLayout`, resolved symbols, control-flow graphs. |
| `coblang-sema` | `crates/coblang-sema` | `forbid` | Name resolution (`OF`/`IN`), `PICTURE` analyzer, storage offset/alignment solver, dialect legality checker, AST -> HIR lowering. |
| `coblang-mlir` | `crates/coblang-mlir` | `forbid` | `melior` (MLIR 20) custom `cobol` dialect builders, canonicalizers, fast-path `i64`/`i128` specializer, lowering to `scf`/`cf`/`arith`/`llvm`. |
| `coblang-codegen` | `crates/coblang-codegen` | `deny` (audited FFI) | MLIR-to-LLVM-IR translation, LLVM 20 `TargetMachine` (`.ll`, `.s`, `.o`), DWARF debug metadata, system linker invocation (`-x`, `-m`/`-shared`). |
| `coblang-decimal` | `crates/coblang-decimal` | `forbid` | Pure-Rust 38-digit BCD/fixed-point & `i128`/multi-limb decimal math, IEEE/decimal floats, 8 ISO rounding modes, ASCII/EBCDIC tables. |
| `coblang-pic` | `crates/coblang-pic` | `forbid` | `PICTURE` editing/de-editing, `MOVE` conversion matrix, `INITIALIZE`, `STRING`, `UNSTRING`, `INSPECT`, `TRANSFORM`. |
| `coblang-intrinsics` | `crates/coblang-intrinsics` | `forbid` | Pure-Rust implementations of all ~105 ISO/GnuCOBOL/gcobol intrinsic functions (math, calendar, string, stats, UTF-8, UUID4). |
| `coblang-fileio` | `crates/coblang-fileio` | `forbid` | `SEQUENTIAL`, `LINE SEQUENTIAL`, and `RELATIVE` file engines, `FILE STATUS` state machine, `LINAGE`, record locks, `DD_` env resolution. |
| `coblang-isam` | `crates/coblang-isam` | `forbid` | Pure-Rust B+Tree `INDEXED` (ISAM) file engine (primary + duplicate alternate/split keys, `START`, dynamic read/rewrite/delete). |
| `coblang-sort` | `crates/coblang-sort` | `forbid` | In-memory & external merge-sort (`SORT`/`MERGE`, `SD`, `INPUT`/`OUTPUT PROCEDURE`, custom collating sequences, table `SORT`). |
| `coblang-report` | `crates/coblang-report` | `forbid` | `REPORT SECTION` (`RD`, control breaks, `SUM`/`RESET`, `GROUP INDICATE`, page layout, `INITIATE`/`GENERATE`/`TERMINATE`). |
| `coblang-screen` | `crates/coblang-screen` | `forbid` | Pure-Rust `SCREEN SECTION` layout/field editor, VT100/ANSI terminal driver, and deterministic PTY/headless test backend. |
| `coblang-xml-json` | `crates/coblang-xml-json` | `forbid` | Pure-Rust streaming `XML PARSE`/`XML GENERATE` and `JSON PARSE`/`JSON GENERATE` engines. |
| `coblang-rt-core` | `crates/coblang-rt-core` | `forbid` | Safe runtime context, module stack, `EXTERNAL`/`BASED` memory, `EC-*` exception state, `CBL_*` / `C$*` / POSIX service logic. |
| `coblang-dynload` | `crates/coblang-dynload` | `deny` (audited FFI) | `dlopen`/`dlsym` module loader for dynamic `CALL`, `CANCEL`, `ENTRY`, and `cobcrun` (`COB_LIBRARY_PATH`, `-M`). |
| `coblang-rt-abi` | `crates/coblang-rt-abi` | `deny` (audited FFI) | `staticlib`/`cdylib` (`libcoblang_rt`) exporting clean-room `libcob` (`cob_*`, `CBL_*`, `C$*`) & `libgcobol` (`__gg__*`) `extern "C"` ABIs. |
| `coblang-scaffold-driver` | `tools/scaffold-driver` | `forbid` | Production `coblang`, `cobc-shim`, `cobcrun-shim`, and `gcobol-shim` CLIs orchestrating all compilation & execution modes. |
| `coblang-test-runner` | `tools/test-runner` | `forbid` | Parallel test harness, torture matrix (`-O0`..`-Os`), diagnostic/CCVS-85 matchers, capability detector, and baseline ratchet. |

---

## 2. Granular Checkbox Implementation Roadmap

### Milestone 0: Workspace Bootstrapping, LLVM/MLIR 20 Integration, & Quality Gate Updates

- [ ] **Maintain roadmap synchronization in repository root (`PLAN.md`)**
  - [x] Establish `/Users/samuel/repos/coblang/PLAN.md` in the workspace root tracking all compiler, runtime, bundle, taxonomy, and crate sign-off checkboxes.
  - [ ] Reference `PLAN.md` in `README.md` under the Workspace Crates / Architecture overview section.
- [ ] **Update quality scripts & CI to cover `crates/**/*.rs` and LLVM/MLIR 20**
  - [ ] Update `scripts/check-quality.sh` Gate 4 (`rs_files`) and Gate 5 (GPLv3 clean-room scan) to include `root.glob("crates/**/*.rs")`.
  - [ ] Update `scripts/precommit-lint.sh` Gate 4 clean-room scan to include `pathlib.Path("crates").rglob("*.rs")`.
  - [ ] Update `scripts/update-readme-shields.py` (`compute_doc_coverage` and `count_compiler_missing_doc_warnings`) to scan `root.glob("crates/**/*.rs")`.
  - [ ] Update `.github/workflows/ci.yml` (`lint`, `test`, `coverage`, and `build-release` jobs) to install LLVM 20 + MLIR 20 (`llvm-20-dev`, `libmlir-20-dev`, `mlir-20-tools`, `libpolly-20-dev`, `zlib1g-dev`, `libzstd-dev` via `apt.llvm.org` on Ubuntu; `brew install llvm@20` on macOS) and export `MLIR_SYS_200_PREFIX` and `LLVM_SYS_201_PREFIX` (or `TABLEGEN_200_PREFIX`).
- [ ] **Extend `coblang-common` (`src/error.rs` & `src/domain.rs`)**
  - [ ] Add `SourceSpan` (`file_id: u32`, `start_byte: u32`, `end_byte: u32`, `line: u32`, `column: u32`) to `src/domain.rs` with 100% unit test coverage.
  - [ ] Extend `CoblangError` in `src/error.rs` (keeping `derive_more::{Display, Error, From}`) with structured compiler/runtime variants:
    ```rust
    /// Compiler diagnostic failure halted compilation before artifact emission.
    #[display("compilation failed with {error_count} error(s)")]
    #[from(ignore)]
    CompilationFailed {
        /// Total hard errors emitted during compilation.
        error_count: usize,
    },
    /// MLIR verification or lowering failure.
    #[display("MLIR pipeline error in stage `{stage}`: {message}")]
    #[from(ignore)]
    MlirPipeline {
        /// Pipeline pass or lowering stage identifier.
        stage: String,
        /// Detailed verifier or pass failure message.
        message: String,
    },
    /// Native target code generation or linker failure.
    #[display("codegen/link error: {message}")]
    #[from(ignore)]
    CodegenLink {
        /// Underlying target machine or linker error message.
        message: String,
    },
    /// Unimplemented language feature returning scaffold exit status `78` during incremental bring-up.
    #[display("unimplemented COBOL feature `{feature}` at {location}")]
    #[from(ignore)]
    UnimplementedFeature {
        /// Canonical feature tag or construct name.
        feature: String,
        /// Formatted `file:line:col` location.
        location: String,
    },
    ```
- [ ] **Create workspace skeleton crates & pin `melior` / `mlir-sys` / `llvm-sys` for LLVM 20**
  - [ ] Add `melior` (`0.22+` targeting MLIR 20), `mlir-sys` (`0.4+` / `200`), and `llvm-sys` (`201`) to `[workspace.dependencies]` in root `Cargo.toml`.
  - [ ] Scaffold `crates/coblang-diag/Cargo.toml` and `src/lib.rs` with `[lints] workspace = true`.
  - [ ] Scaffold `crates/coblang-mlir/Cargo.toml` and `src/lib.rs` verifying `melior::Context::new()` initializes an MLIR 20 context, loads `func`, `arith`, `scf`, `cf`, `memref`, and `llvm` dialects, and verifies an empty `Module`.
  - [ ] Scaffold `crates/coblang-codegen/Cargo.toml` with explicit lint table (`unsafe_code = "deny"` + all other workspace lints mirrored) and verify translating an empty MLIR `llvm` module into textual LLVM IR (`-emit-llvm`) and a host `.o` object file.
  - [ ] Scaffold `crates/coblang-rt-core`, `crates/coblang-rt-abi` (`crate-type = ["rlib", "staticlib", "cdylib"]`), and `crates/coblang-dynload`.
- [ ] **Upgrade `coblang-test-runner` to support real compiler execution alongside incremental `XFAIL`s**
  - [ ] Add `CompilerBackend::CoblangInTree { compiler_bin: PathBuf, cobcrun_bin: PathBuf, strict_diagnostic_text: bool, compile_timeout_ms: u64 }` (or wire default backend resolution in `tools/test-runner/src/cli.rs`) so `coblang-test` executes compiled binaries and `cobcrun` modules whenever `coblang` exits `0`, while still classifying `EXIT_UNIMPLEMENTED` (`78`) as `RawStepDisposition::Unimplemented` -> `TestOutcome::XFail`.
  - [ ] Add `--filter <substring>` and `--tag <feature-tag>` flags to `coblang-test run` and `coblang-test summary` in `tools/test-runner/src/cli.rs` for rapid sub-second iteration on targeted vertical slices.
  - [ ] Verify Milestone 0 with `./scripts/check-quality.sh` (all 7 gates green, 100% fn/line/branch coverage, 0 regressions on 2,527 tests).

---

### Milestone 1: Diagnostics Engine, Source Spans, & Multi-Dialect Configuration Profiles

- [ ] **Implement `SourceMap` and expansion-aware `Span` tracking (`crates/coblang-diag/src/source_map.rs`)**
  - [ ] Define `FileId(u32)` and `SourceFile` storing original path, raw bytes, decoded UTF-8/codepage text, and precomputed line-start byte offsets (`O(log N)` line/col lookup).
  - [ ] Implement `ExpansionId` and `ExpansionInfo` recording `COPY`, `REPLACE`, and CDF macro/directive call sites (`caller_span`, `callee_file`, `kind`) so diagnostics inside copybooks emit primary `copybook.cpy:12:8: error: ...` followed by `prog.cob:4:8: note: in COPY included from here`.
  - [ ] Write unit tests covering multi-line spans, EOF spans, CRLF vs LF line numbering, and nested `COPY` backtraces at 100% branch coverage.
- [ ] **Implement GCC / Clang / `cobc`-compatible Diagnostic Formatter (`crates/coblang-diag/src/emitter.rs`)**
  - [ ] Define `Diagnostic { severity: DiagnosticSeverity, code: DiagnosticCode, primary_span: Option<Span>, message: String, notes: Vec<(Option<Span>, String)>, warning_flag: Option<WarningGroup> }`.
  - [ ] Emit canonical single-line headers (`{file}:{line}:{col}: error: {message}`) strictly compatible with `tools/test-runner/src/matcher.rs::parse_single_diagnostic_line`!
  - [ ] Implement warning-control state machine (`-w` suppresses all warnings, `-Wall`/`-Wextra`, `-Warchaic`, `-Wobsolete`, `-Wredefinition`, `-Wtruncate`, `-Wstrict-typing`, `-Wimplicit-define`, `-Wparentheses`, `-Wterminator`, `-Wno-<group>`, `-Werror`, `-Werror=<group>`, `-fmax-errors=N`).
- [ ] **Implement Dialect & `-conf=` Configuration Matrix (`crates/coblang-diag/src/dialect_conf.rs`)**
  - [ ] Define `DialectConfig` struct encoding every configurable rule from `GnuCOBOL` (`config/*.conf` tested in `tests/ported/gnucobol/configuration.bundle`) and `gcobol` (`-dialect ibm|mf|gnu|iso`):
    ```rust
    pub struct DialectConfig {
        pub base_dialect: Dialect,
        pub binary_byteorder: BinaryByteOrder,       // Native vs BigEndian
        pub binary_size: BinarySizePolicy,           // 1-2-4-8 vs 2-4-8 vs 1..8
        pub binary_truncate: bool,                   // -fnotrunc vs PICTURE truncation
        pub pretty_display: bool,                    // GnuCOBOL vs ISO display formatting
        pub complex_odo: bool,                       // IBM/Gnu complex OCCURS DEPENDING ON
        pub indirect_redefines: bool,                // REDEFINES of REDEFINES item
        pub larger_redefines_ok: SeverityDisposition,// Error in ISO/COBOL-85, Warning in Gnu/IBM
        pub relaxed_syntax_checks: bool,
        pub perform_osvs: bool,                      // OS/VS PERFORM continuation semantics
        pub sticky_linkage: bool,
        pub assign_clause: AssignMappingKind,        // Dynamic vs External vs MF
        pub reserved_words: ReservedWordSet,         // Per-dialect reserved & context-sensitive words
    }
    ```
  - [ ] Implement `.conf` file parser supporting `include "..."`, `reserved: ...`, `not-reserved: ...`, boolean/enum keys (`yes`/`no`/`ok`/`warning`/`error`/`archaic`/`obsolete`/`skip`/`ignore`/`unconformable`), and CLI overrides (`-cb_conf=...`, `-frelax-syntax-checks`, `-fnotrunc`, `-fbinary-size=...`, `-fbinary-byteorder=...`, `-fassign-clause=...`, `-fdefaultbyte=...`).
  - [ ] Verify against `configuration.bundle` diagnostic test cases and maintain 100% coverage in `./scripts/check-quality.sh`.

---

### Milestone 2: Reference-Format Scanner, `COPY`/`REPLACE` Preprocessor, CDF Directives, & Listings (`stage:preprocess` — 478 Tests)

- [ ] **Implement Reference-Format Normalizer (`crates/coblang-pp/src/format.rs`)**
  - [ ] Implement `SourceFormat::Fixed` line slicer:
    - [ ] Columns 1–6: sequence number area (validate/warn on non-numeric or out-of-order if `-Wsequence` enabled; preserve for listing emitter).
    - [ ] Column 7 indicator area: `' '` (normal), `'*'` / `'/'` (comment / page-eject comment), `'D'` / `'d'` (debugging line — elided unless `WITH DEBUGGING MODE` or `-fdebugging-line` / `-debug` is active!), `'-'` (continuation of non-numeric literal or split word), `'$'` (vendor directive).
    - [ ] Area A (columns 8–11) and Area B (columns 12–72) preservation with exact column mapping into `SourceMap`.
    - [ ] Columns 73+ program-identification area (strip cleanly; handle lines shorter than 72 columns and tab expansion at 8-column tab stops).
  - [ ] Implement `SourceFormat::Free` line slicer:
    - [ ] `*> ` inline comments to end-of-line, continued string literals (`"..."-` / `'...'`), no column-72 truncation, Area A/B distinction disabled (except syntactic division/section checks).
  - [ ] Implement `SourceFormat::Auto` detector and inline `>>SOURCE FORMAT IS FREE` / `>>SOURCE FORMAT IS FIXED` / `$SET SOURCEFORMAT"FREE"` mid-file switching!
  - [ ] Handle fixed-form continuation edge cases tested in `nist85/SM.bundle` and `gnucobol/syn_literals.bundle` (continuing unclosed `'...'` / `"..."` / `X"..."` / `Z"..."` / `N"..."` across multiple `'-'` lines, doubled quote escapes `""` / `''` split across column 72!).
- [ ] **Implement Pre-Parser Token Stream & `COPY` Statement Resolver (`crates/coblang-pp/src/copy.rs`)**
  - [ ] Implement copybook file lookup searching `-I <dir>`, current source file directory, `COB_COPY_DIR`, and trying configured extensions (`""`, `.cpy`, `.CPY`, `.cbl`, `.CBL`, `.cob`, `.COB`, plus `-copyext` / `-ext` values) and library qualification (`COPY text-name OF/IN library-name` mapped via `COB_COPY_LIB_<LIB>` or subdirectory `<library-name>/`).
  - [ ] Detect and diagnose recursive `COPY` cycles cleanly (`error: recursive COPY of 'FOO'`).
  - [ ] Implement `COPY ... SUPPRESS` and `COPY ... REPLACING`:
    - [ ] Pseudo-text matching `== ... == BY == ... ==` (whitespace/newline-insensitive COBOL text-word matching per ANSI-85 SM rules!).
    - [ ] Identifier, literal, and COBOL word replacement.
    - [ ] Partial-word replacement `REPLACING LEADING ==prefix== BY ==new==` and `TRAILING ==suffix== BY ==new==` (COBOL 2002 / GnuCOBOL / gcobol).
- [ ] **Implement `REPLACE` Statement Engine (`crates/coblang-pp/src/replace.rs`)**
  - [ ] Implement `REPLACE ==...== BY ==...==.` and `REPLACE ALSO ==...== BY ==...==.` (stacking vs replacing active table) and `REPLACE OFF.` running *after* `COPY` expansion per ISO standard two-phase text manipulation rules!
- [ ] **Implement Compiler Directing Facility (CDF) (`crates/coblang-pp/src/cdf.rs`)**
  - [ ] Implement `>>DEFINE name AS literal/PARAMETER [OVERRIDE]` and CLI `-Dname[=val]`.
  - [ ] Implement constant conditional expressions (`DEFINED(x)`, `=`, `<>`, `<`, `<=`, `>`, `>=`, `AND`, `OR`, `NOT`, integer arithmetic) for `>>IF ... [>>ELIF ...] [>>ELSE ...] >>END-IF` and `>>EVALUATE ... >>WHEN ... >>END-EVALUATE`.
  - [ ] Implement `>>SET`, `>>TURN ec-name CHECKING ON/OFF [WITH LOCATION]`, `>>CALL-CONVENTION`, `>>PUSH` / `>>POP`, `>>DISPLAY` (compile-time message), and IBM `*CONTROL` / `*CBL` (`SOURCE`/`NOSOURCE`, `LIST`/`NOLIST`, `MAP`/`NOMAP`).
- [ ] **Implement `-E` Preprocessed Output & `-t` / `-T` Compiler Listing Emitter (`crates/coblang-pp/src/listing.rs`)**
  - [ ] Emit clean preprocessed COBOL source on `-E` (`CompilationMode::PreprocessOnly`).
  - [ ] Emit formatted compiler listings (`-t <file>`, `-T <file>`, `--tlines=N`, `-ftsymbols`, `-fno-theader`, `-fno-tmessages`) matching expectations in `tests/ported/gnucobol/listings.bundle`.
- [ ] **Verify & Ratchet Milestone 2**
  - [ ] Add exhaustive unit tests covering `Fixed`/`Free`/`Auto`, split-quote continuations, nested `COPY ... REPLACING`, `REPLACE ALSO`/`OFF`, CDF conditionals, and listings at 100% branch coverage.
  - [ ] Run `coblang-test run --bundle tests/ported/gnucobol/syn_copy.bundle --bundle tests/ported/gnucobol/listings.bundle --bundle tests/ported/nist85/SM.bundle` and ratchet newly passing `-E` / preprocessor diagnostic tests in `tests/expectations/baseline.toml`.
  - [ ] Run `./scripts/check-quality.sh`.

---

### Milestone 3: Context-Sensitive Lexer, 4-Division Parser, & Semantic Validator (`-fsyntax-only`, `stage:parse-sema` — 547 Tests)

- [ ] **Implement Context-Sensitive COBOL Lexer (`crates/coblang-syntax/src/lexer.rs`)**
  - [ ] Switch lexer modes contextually for:
    - [ ] Normal COBOL words (ASCII/Latin/hyphen/underscore rules per dialect, case-folding to canonical uppercase while preserving raw spelling for diagnostics and C symbol names), level numbers (`01`..`49`, `66`, `77`, `78`, `88`), paragraph/section integer labels (`1000-MAIN`, `0010`).
    - [ ] `PICTURE` / `PIC` character-string mode (`PIC IS S9(5)V99.`, where trailing `.` or `,` followed by whitespace/EOF is a separator period/comma, NOT part of the `PICTURE` string!).
    - [ ] String & byte literals: `'...'`, `"..."`, hex `X"..."` / `H"..."`, null-terminated `Z"..."`, boolean/bit `B"..."` / `BX"..."`, national `N"..."` / `NX"..."`, UTF-8 `U"..."` / `UX"..."`, concatenation operator `&`.
    - [ ] Numeric literals: signed/unsigned fixed-point decimals (up to 38 digits) and floating-point (`1.5E-3`), distinguishing relational `<`/`>`/`=`/`<=`/`>=` and arithmetic `+`/`-`/`*`/`/`/`**` (note: `A-B` is an identifier, `A - B` is subtraction, except when relaxed syntax allows tight operators in specific dialects!).
    - [ ] Comment entries in `IDENTIFICATION DIVISION` (`AUTHOR.`, `INSTALLATION.`, `DATE-WRITTEN.`, `DATE-COMPILED.`, `SECURITY.`, `REMARKS.`) consumed as free-text paragraphs under COBOL-85 / GnuCOBOL dialects (and flagged obsolete/error per dialect config).
- [ ] **Define Lossless CST & Typed AST (`crates/coblang-syntax/src/ast.rs`)**
  - [ ] Model compilation groups containing multiple sequential or nested `CompilationUnit`s (`PROGRAM-ID`, `FUNCTION-ID`, `CLASS-ID`, `INTERFACE-ID`, `END PROGRAM`, `END FUNCTION`, `END CLASS`).
  - [ ] **Identification Division AST**: `ProgramId` (`IS COMMON`, `IS INITIAL`, `IS RECURSIVE`, `AS "ext-name"`), `FunctionId`, `OptionsParagraph` (`ARITHMETIC IS STANDARD-BINARY / STANDARD-DECIMAL`, `DEFAULT ROUNDED MODE IS ...`, `ENTRY-CONVENTION`, `FLOAT-BINARY`, `FLOAT-DECIMAL`, `INITIALIZE WORKING-STORAGE / LOCAL-STORAGE`).
  - [ ] **Environment Division AST**:
    - [ ] `CONFIGURATION SECTION`: `SOURCE-COMPUTER` (`WITH DEBUGGING MODE`), `OBJECT-COMPUTER` (`PROGRAM COLLATING SEQUENCE IS ...`, `CHARACTER CLASSIFICATION`), `SPECIAL-NAMES` (`DECIMAL-POINT IS COMMA`, `CURRENCY SIGN IS ... WITH PICTURE SYMBOL ...`, `ALPHABET name IS STANDARD-1 / STANDARD-2 / NATIVE / EBCDIC / literal-thru-list`, `SYMBOLIC CHARACTERS`, `CLASS name IS ...`, `CRT STATUS IS ...`, `CURSOR IS ...`, `CONSOLE IS CRT`, `SWITCH-0..36 IS mnemonic ON/OFF STATUS IS cond`, `SYSIN`/`SYSOUT`/`SYSERR` mnemonics), `REPOSITORY` (`FUNCTION ALL INTRINSIC`, `FUNCTION udf-name`, `CLASS cls-name`).
    - [ ] `INPUT-OUTPUT SECTION`: `FILE-CONTROL` (`SELECT [OPTIONAL] file ASSIGN TO [EXTERNAL/DYNAMIC] target`, `ORGANIZATION IS [LINE] SEQUENTIAL / RELATIVE / INDEXED`, `ACCESS MODE IS SEQUENTIAL / RANDOM / DYNAMIC`, `RECORD KEY`, `ALTERNATE RECORD KEY [WITH DUPLICATES]`, `RELATIVE KEY`, `FILE STATUS`, `LOCK MODE`, `PADDING CHARACTER`, `RECORD DELIMITER`), `I-O-CONTROL` (`SAME [RECORD / SORT / SORT-MERGE] AREA FOR ...`, `MULTIPLE FILE TAPE`, `APPLY WRITE-ONLY`).
  - [ ] **Data Division AST**:
    - [ ] Sections: `FILE SECTION` (`FD`, `SD`), `WORKING-STORAGE SECTION`, `LOCAL-STORAGE SECTION`, `LINKAGE SECTION`, `REPORT SECTION` (`RD`), `SCREEN SECTION`, `COMMUNICATION SECTION` (`CD` — COBOL-85 obsolete NC/OB support).
    - [ ] Data Description Entry (`DataEntry`): level (`01`..`49`, `66`, `77`, `78`, `88`), name or `FILLER`, `REDEFINES`, `RENAMES ... THRU ...`, `PIC` / `PICTURE`, `USAGE` (`DISPLAY`, `BINARY`, `COMP`, `COMP-1`..`COMP-6`, `COMP-X`, `COMP-N`, `PACKED-DECIMAL`, `INDEX`, `POINTER`, `PROGRAM-POINTER`, `FUNCTION-POINTER`, `OBJECT REFERENCE`, `NATIONAL`, `UTF-8`, `BINARY-CHAR/SHORT/LONG/DOUBLE [SIGNED/UNSIGNED]`, `FLOAT-SHORT/LONG/EXTENDED`, `FLOAT-DECIMAL-16/34`), `SIGN IS [LEADING/TRAILING] [SEPARATE CHARACTER]`, `OCCURS min TO max TIMES [DEPENDING ON odo] [ASCENDING/DESCENDING KEY IS ...] [INDEXED BY ...]`, `SYNCHRONIZED [LEFT/RIGHT]`, `JUSTIFIED RIGHT`, `BLANK WHEN ZERO`, `VALUE IS ...` / `VALUES ARE ... THRU ... [WHEN SET TO FALSE IS ...]`, `EXTERNAL [AS ...]`, `GLOBAL`, `BASED`, `ANY LENGTH`, `DYNAMIC LENGTH`, `TYPEDEF`, `SAME AS`, `CONSTANT [IS GLOBAL] AS ...`.
  - [ ] **Procedure Division AST**:
    - [ ] Header: `USING [BY REFERENCE / BY CONTENT / BY VALUE] [OPTIONAL] param...`, `RETURNING ret`, `CHAINING`.
    - [ ] `DECLARATIVES ... END DECLARATIVES` (`section-name SECTION. USE [GLOBAL] AFTER STANDARD EXCEPTION/ERROR PROCEDURE ON file/INPUT/OUTPUT/I-O/EXTEND | USE FOR DEBUGGING ON ...`).
    - [ ] Sections, paragraphs, sentences, separator-period scope recovery (handling nested `IF ... ELSE ...` terminated by `.` vs `END-IF`, abbreviated combined relation conditions `IF A = B OR C AND > D`!).
    - [ ] All COBOL-85 / 2002 / 2014 / 2023 / GnuCOBOL statements: `ACCEPT`, `ADD`, `ALLOCATE`, `ALTER`, `CALL`, `CANCEL`, `CLOSE`, `COMMIT`, `COMPUTE`, `CONTINUE`, `DELETE`, `DISPLAY`, `DIVIDE`, `ENTRY`, `EVALUATE`, `EXHIBIT`, `EXEC` (diagnostic/preprocess hook), `EXIT` (`PROGRAM`/`PARAGRAPH`/`SECTION`/`PERFORM [CYCLE]`/`FUNCTION`/`METHOD`), `FREE`, `GENERATE`, `GO TO`, `GOBACK`, `IF`, `INITIALIZE`, `INITIATE`, `INSPECT`, `INVOKE`, `JSON GENERATE/PARSE`, `MERGE`, `MOVE`, `MULTIPLY`, `NEXT SENTENCE`, `OPEN`, `PERFORM`, `RAISE`, `READ`, `RELEASE`, `RESUME`, `RETURN`, `REWRITE`, `ROLLBACK`, `SEARCH`, `SET`, `SORT`, `START`, `STOP`, `STRING`, `SUBTRACT`, `SUPPRESS`, `TERMINATE`, `TRANSFORM`, `UNLOCK`, `UNSTRING`, `VALIDATE`, `WRITE`, `XML GENERATE/PARSE`.
- [ ] **Implement Semantic Analyzer & `-fsyntax-only` Validator (`crates/coblang-sema`)**
  - [ ] Implement hierarchical symbol table & qualified name resolver (`A OF B OF C`, subscript vs reference-modification disambiguation `FOO(1:2)` vs `FOO(1)` vs `FUNCTION FOO(1)`).
  - [ ] Implement `PICTURE` string parser & validator (`crates/coblang-sema/src/picture.rs`):
    - [ ] Classify category (`Alphabetic`, `Alphanumeric`, `AlphanumericEdited`, `Numeric`, `NumericEdited`, `National`, `NationalEdited`, `Boolean`).
    - [ ] Validate mutually exclusive symbols, maximum digit count (18 in strict COBOL-85, 38 in COBOL 2023 / GnuCOBOL / gcobol), `S`, `V`, `P` scaling positions, floating insertion (`$$`, `++`, `--`), suppression (`Z`, `*`), fixed insertion (`B`, `0`, `/`, `,`, `.`, `CR`, `DB`), and compatibility with `USAGE`, `BLANK WHEN ZERO`, `JUSTIFIED`, `SIGN`, and `VALUE` literals!
  - [ ] Implement storage layout & hierarchy builder (`crates/coblang-sema/src/layout.rs`):
    - [ ] Normalize non-contiguous level numbers (`01` -> `05` -> `10` -> `07` sibling/parent rules!), validate `77` elementary-only rules, `66 RENAMES` contiguous span rules, `88` condition-variable parent rules, `REDEFINES` level/offset/size rules, and `OCCURS` / `DEPENDING ON` restrictions.
  - [ ] Implement statement-level semantic checks (`MOVE` category legality matrix, `CALL` parameter modes, read-only `BY CONTENT` / constant mutation errors, duplicate paragraph ambiguity errors, `PERFORM` target resolution, file/record correspondence, `EVALUATE` selection subject/object count & type compatibility).
- [ ] **Wire `-fsyntax-only` (`CompilationMode::SyntaxOnly`) into `coblang-scaffold-driver`**
  - [ ] Update `execute_driver` so `CompilationMode::PreprocessOnly` (`-E`) and `CompilationMode::SyntaxOnly` (`-fsyntax-only`) invoke the real `coblang-pp` -> `coblang-syntax` -> `coblang-sema` pipeline!
  - [ ] Verify **`canary/scaffold-syntax` flips from `XFAIL` to `PASS`**!
  - [ ] Run syntax/diagnostic bundles (`syn_definition.bundle`, `syn_literals.bundle`, `syn_misc.bundle`, `syn_move.bundle`, `syn_multiply.bundle`, `syn_occurs.bundle`, `syn_redefines.bundle`, `syn_refmod.bundle`, `syn_set.bundle`, `syn_subscripts.bundle`, `syn_value.bundle`, `syn_file.bundle`, `syn_functions.bundle`, `gcobol/top_level.bundle`), triage diagnostic line/severity accuracy, and ratchet `tests/expectations/baseline.toml` + `SCOREBOARD.md` + `SCOREBOARD.json`.
  - [ ] Run `./scripts/check-quality.sh` (confirming 100% fn/line/branch coverage).

---

### Milestone 4: Custom `cobol` MLIR Dialect (`melior` 20), Minimal Codegen Pipeline, & First Native Executables (`DISPLAY`, `STOP RUN`, `RETURN-CODE`)

- [ ] **Design & Implement the `cobol` MLIR Dialect (`crates/coblang-mlir/src/dialect.rs`)**
  - [ ] Build typed safe Rust constructors/wrappers around `melior::ir::operation::OperationBuilder` (using `unregistered` dialect `"cobol"` or dynamic dialect registration in MLIR 20 so zero C++ TableGen build fragility is required while maintaining strict custom MLIR verification in `crates/coblang-mlir/src/verifier.rs`!):
    - [ ] `cobol.program` (`sym_name`, `is_initial`, `is_recursive`, `entry_convention`)
    - [ ] `cobol.storage` (`sym_name`, `section` = working/local/external/static, `size_bytes`, `align`, `initial_bytes`)
    - [ ] `cobol.field_view` (`base_ptr`, `byte_offset`, `byte_len`, `field_attr` encoding `type_tag`, `digits`, `scale`, `flags`)
    - [ ] `cobol.display` (`operands`, `upon_device` = stdout/stderr, `with_no_advancing`: `bool`)
    - [ ] `cobol.set_return_code` / `cobol.get_return_code`
    - [ ] `cobol.stop_run` (`exit_status`) and `cobol.goback`
  - [ ] Implement textual MLIR pretty-printer & custom `cobol` dialect verifier (`crates/coblang-mlir/src/verifier.rs`) checking operand types, attributes, regions, and terminators before lowering.
  - [ ] Add `--emit-mlir` (and `-fdump-mlir-cobol` / `-fdump-mlir-llvm` extra flags) to `coblang` for unit/golden testing of both high-level `cobol` MLIR and lowered `llvm` MLIR.
- [ ] **Implement Minimal Clean-Room Runtime ABI (`crates/coblang-rt-core` & `crates/coblang-rt-abi`)**
  - [ ] Define `#[repr(C)]` ABI descriptors in `crates/coblang-rt-abi/src/abi_types.rs` matching `libcob` layout conventions:
    ```rust
    /// Clean-room C-ABI field attribute descriptor (`cob_field_attr` layout).
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CobFieldAttr {
        /// Field type tag (alphanumeric, numeric display, binary, packed, edited, float, etc.).
        pub field_type: u8,
        /// Total decimal digits (`PIC 9` count).
        pub digits: u8,
        /// Signed decimal scale (`PIC V99` -> `2`, `PIC P9` -> `2`, `PIC 9P` -> `-1`).
        pub scale: i8,
        /// Attribute bitflags (`HAVE_SIGN`, `SIGN_SEPARATE`, `SIGN_LEADING`, `BLANK_ZERO`, `JUSTIFIED`, `BINARY_SWAP`).
        pub flags: u8,
        /// Pointer to edited PICTURE control string (null when non-edited).
        pub pic: *const core::ffi::c_char,
    }

    /// Clean-room C-ABI runtime field view (`cob_field` layout).
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct CobField {
        /// Storage byte length of the data item.
        pub size: usize,
        /// Pointer to the item's raw storage bytes.
        pub data: *mut u8,
        /// Pointer to the immutable [`CobFieldAttr`] descriptor.
        pub attr: *const CobFieldAttr,
    }
    ```
  - [ ] Implement safe `CobolRuntimeContext` in `crates/coblang-rt-core` (thread-local / process-global state holding `argv`/`argc`, `RETURN-CODE: i32`, buffered/unbuffered stdout & stderr writers, `COB_*` env settings) and expose audited `extern "C"` wrappers in `crates/coblang-rt-abi`:
    - [ ] `cob_init(argc: c_int, argv: *mut *mut c_char)`
    - [ ] `cob_stop_run(status: c_int) -> !` (or flush + `std::process::exit(status)` in binary mode, plus testable non-exiting hook for unit tests so 100% coverage is preserved!)
    - [ ] `cob_display(out_stderr: c_int, newline: c_int, varcnt: c_int, ...)` / `coblang_rt_display_fields(out_stderr: i32, newline: i32, fields: *const CobField, count: usize)` + `__gg__display` compatibility alias.
- [ ] **Implement `cobol` MLIR -> `llvm` MLIR -> LLVM 20 Native Executable Lowering (`crates/coblang-mlir` & `crates/coblang-codegen`)**
  - [ ] Lower `cobol.program` (when `CompilationMode::BuildExecutable` or `-main` is active) to both the callable COBOL entry function `@<PROG_ID>` AND a C `@main(i32 %argc, ptr %argv) -> i32` wrapper that calls `@cob_init`, invokes `@<PROG_ID>`, and exits with `RETURN-CODE`.
  - [ ] Configure `melior::pass::PassManager` for `-O0`, `-O1`, `-O2`, `-O3`, `-Os` (`convert-scf-to-cf`, `convert-cf-to-llvm`, `convert-func-to-llvm`, `convert-arith-to-llvm`, `finalize-memref-to-llvm`, `reconcile-unrealized-casts`).
  - [ ] Translate MLIR `llvm` module to `LLVMModuleRef`, run LLVM 20 optimization pipeline matching `OptimizationLevel`, and emit `.ll` (`-emit-llvm`), `.s` (`-S`), or `.o` (`-c`, `-m`, `-x`).
  - [ ] Implement system linker driver in `crates/coblang-codegen/src/linker.rs` invoking `clang`/`cc` to link emitted `.o` + companion `.o`/`.so` inputs + `-L`/`-l` flags + `libcoblang_rt.a` (with `-Wl,-rpath` when dynamic) on both Linux (`ELF`) and macOS (`Mach-O`).
- [ ] **Verify & Ratchet Milestone 4**
  - [ ] Verify end-to-end compilation and execution of `DISPLAY "HELLO WORLD"`, `STOP RUN`, `STOP RUN RETURNING n`, and `MOVE n TO RETURN-CODE` across `-O0`..`-Os`.
  - [ ] Run `coblang-test` on `tests/ported/gnucobol/run_returncode.bundle`, `tests/ported/gcobol/group1.bundle` (`display.cob`, `display2.cob`, `escape.cob`), and `tests/ported/gnucobol/run_fundamental.bundle`, ratcheting `baseline.toml` and `SCOREBOARD.md`.
  - [ ] Run `./scripts/check-quality.sh`.

---

### Milestone 5: Alphanumeric & Zoned-Decimal Storage, Figurative Constants, `MOVE`, & `INITIALIZE` (`type:alphanumeric` — 1,555, `type:numeric-display` — 1,395, `feature:move-init` — 1,410)

- [ ] **Implement Complete Storage Layout & `VALUE` Static Initializer Generator (`crates/coblang-sema/src/init_image.rs`)**
  - [ ] Compute exact byte size and byte offset of every group and elementary item in `WORKING-STORAGE` and `LOCAL-STORAGE`:
    - [ ] Group items: treated as alphanumeric (`PIC X(sum_of_children)`), initialized to spaces (or `-fdefaultbyte`) unless overridden by group/child `VALUE` clauses.
    - [ ] Alphanumeric (`PIC X`), Alphabetic (`PIC A`), and Zoned Decimal (`PIC [S]9...V9...` `USAGE DISPLAY`) with embedded overpunch sign (ASCII/IBM/MicroFocus/GnuCOBOL sign nibble tables: `{`, `A`..`I`, `}`, `J`..`R` vs ASCII `0x30..0x39` / `0x70..0x79`!) and `SIGN IS LEADING/TRAILING SEPARATE` (`'+'`/`'-'`).
    - [ ] `SYNCHRONIZED LEFT/RIGHT` slack byte insertion per dialect (`Cobol85`, `Ibm`, `Gnu`, `MicroFocus`).
  - [ ] Emit static byte arrays in `cobol.storage` for `WORKING-STORAGE` (`is_initial = false` retains state across calls; `IS INITIAL` program or `LOCAL-STORAGE` stack-allocates / re-memcpy's initial image on every entry!).
- [ ] **Implement Figurative Constants (`SPACE`/`SPACES`, `ZERO`/`ZEROS`/`ZEROES`, `HIGH-VALUE`/`HIGH-VALUES`, `LOW-VALUE`/`LOW-VALUES`, `QUOTE`/`QUOTES`, `ALL literal`, `NULL`/`NULLS`)**
  - [ ] Support context-dependent sizing and encoding (`ASCII` `0x20`/`0x00`/`0xFF` vs `EBCDIC` `0x40`/`0x00`/`0xFF` vs `NATIONAL` UTF-16 code units) in `VALUE`, `MOVE`, `INITIALIZE`, `IF` comparisons, `DISPLAY`, and `STRING`/`INSPECT`.
- [ ] **Implement Full `MOVE` & `MOVE CORRESPONDING` Engine (`crates/coblang-pic/src/move_engine.rs` + MLIR Fast Paths)**
  - [ ] Implement the complete ISO/GnuCOBOL `MOVE` conversion matrix:
    - [ ] Alphanumeric / Group -> Alphanumeric / Group: left-justified space-padded (or right-justified when `JUSTIFIED RIGHT` is set) with deterministic truncation; inline as `llvm.memcpy` + `llvm.memset` in MLIR when lengths are compile-time constants!
    - [ ] Numeric / Numeric literal -> Alphanumeric: convert unsigned integer value to digit characters (per dialect rules when signed or scaled).
    - [ ] Alphanumeric -> Numeric: parse digits / zoned bytes per GnuCOBOL/ISO rules.
    - [ ] Numeric -> Numeric: decimal point alignment (`V` and `P` scaling!), high-order digit truncation (unless `-fnotrunc` on binary), low-order fractional truncation, sign normalization (`PIC 9` unsigned strips negative sign to positive!).
  - [ ] Implement `MOVE CORRESPONDING src-group TO dst-group` in `coblang-sema` by expanding matching elementary/sub-group paths at compile time into individual `HIR::Move` ops.
- [ ] **Implement `INITIALIZE` Statement (`crates/coblang-sema` & `crates/coblang-pic/src/initialize.rs`)**
  - [ ] Implement standard `INITIALIZE id-1 ...` (spaces to alphabetic/alphanumeric/alphanumeric-edited, zeros to numeric/numeric-edited, skipping `FILLER` unless `WITH FILLER` is specified, skipping `REDEFINES` children, skipping `USAGE POINTER`/`INDEX` unless `ALL TO VALUE` / explicit category).
  - [ ] Implement `INITIALIZE ... REPLACING ALPHABETIC / ALPHANUMERIC / NUMERIC / ALPHANUMERIC-EDITED / NUMERIC-EDITED BY data-or-literal`, `INITIALIZE ... TO VALUE` (`DEFAULT`), and `OCCURS` table element expansion.
- [ ] **Verify & Ratchet Milestone 5**
  - [ ] Unit test every `(source_category, dest_category, sign_mode, justified, scale_diff)` cell in `crates/coblang-pic` at 100% branch coverage.
  - [ ] Run `coblang-test` on `data_display.bundle`, `run_initialize.bundle`, `syn_move.bundle`, `syn_value.bundle`, `gcobol/group2` (`37-digit_Initialization*`, `INITIALIZE*`, `MOVE*`), and `nist85/NC.bundle`, ratcheting `baseline.toml` and `SCOREBOARD.md`.
  - [ ] Run `./scripts/check-quality.sh`.

---

### Milestone 6: Binary (`COMP`/`COMP-5`), Packed-Decimal (`COMP-3`/`COMP-6`), Floats, 128-Bit Decimal, & Exact Arithmetic (`feature:arith` — 722 Tests)

- [ ] **Implement Pure-Rust Exact Decimal & Binary Storage Codecs (`crates/coblang-decimal`)**
  - [ ] Implement `FixedDecimal` (`i128` mantissa + `i16` scale for `<= 38` digits, escalating seamlessly to fixed-capacity 4-limb `[u64; 4]` 76-digit intermediate accumulator so `MULTIPLY` / `DIVIDE` / `COMPUTE` intermediate precision NEVER overflows on 37/38-digit operands!).
  - [ ] Implement binary integer codecs (`USAGE BINARY` / `COMP` / `COMP-4` big-endian by default in IBM/GnuCOBOL vs `COMP-5` / `BINARY-CHAR/SHORT/LONG/DOUBLE` native endian; 1/2/4/8/16-byte widths per `DialectConfig::binary_size`; `PIC` decimal truncation vs full binary range when `-fnotrunc` or `COMP-5`).
  - [ ] Implement `PACKED-DECIMAL` / `COMP-3` (`0xC` positive, `0xD` negative, `0xF` unsigned nibbles; `(digits / 2) + 1` bytes) and `COMP-6` (unsigned packed BCD, `(digits + 1) / 2` bytes).
  - [ ] Implement `COMP-1` (`f32`), `COMP-2` (`f64`), `FLOAT-EXTENDED` (`f128` / software IEEE quad), and IEEE 754-2008 decimal floating-point (`FLOAT-DECIMAL-16` / `FLOAT-DECIMAL-34` for `type:128bit-decimal` — 7 tests).
- [ ] **Implement All 8 ISO COBOL 2023 Rounding Modes & `SIZE ERROR` Detection (`crates/coblang-decimal/src/rounding.rs`)**
  - [ ] Implement `RoundingMode`: `Truncation` (default unrounded), `NearestAwayFromZero` (default `ROUNDED`), `NearestEven`, `NearestTowardZero`, `Prohibited` (triggers `EC-SIZE-TRUNCATION` / `SIZE ERROR` if any non-zero digit discarded), `TowardGreater`, `TowardLesser`, `AwayFromZero`.
  - [ ] Implement `SIZE ERROR` / `NOT ON SIZE ERROR` detection on division-by-zero, `0 ** 0` / negative exponent errors, and target `PICTURE` high-order overflow (preserving target storage unchanged when `SIZE ERROR` fires!).
- [ ] **Implement Arithmetic Verbs & `COMPUTE` Expression Lowering (`ADD`, `SUBTRACT`, `MULTIPLY`, `DIVIDE`, `COMPUTE`)**
  - [ ] Lower `ADD ... TO ... [GIVING ...]`, `ADD CORRESPONDING`, `SUBTRACT ... FROM ... [GIVING ...]`, `SUBTRACT CORRESPONDING`, `MULTIPLY ... BY ... [GIVING ...]`, `DIVIDE ... INTO/BY ... [GIVING ...] [REMAINDER ...]` (with exact COBOL `REMAINDER` order-of-evaluation when `GIVING` target is rounded or truncated!).
  - [ ] Lower arbitrary `COMPUTE target [ROUNDED] ... = expr` with unary `+`/`-`, `+`, `-`, `*`, `/`, and `**` exponentiation (supporting integer and fractional/floating exponents) under both `STANDARD` and dialect-specific intermediate precision rules.
  - [ ] **MLIR Specialization Pass (`crates/coblang-mlir/src/opt_numeric.rs`)**:
    - [ ] Lower `COMP-5` / `BINARY-*` and small fixed-scale `BINARY`/`PACKED` arithmetic (`digits <= 18` -> `i64`, `19..=37` -> `i128`) directly to MLIR `arith.addi` / `arith.subi` / `arith.muli` / `arith.divsi` + `@llvm.sadd.with.overflow` intrinsics when `SIZE ERROR` is present, falling back to `@cob_decimal_*` runtime calls only for multi-target composite/transcendental/38-digit cases!
- [ ] **Verify & Ratchet Milestone 6**
  - [ ] Run `coblang-test` on `data_binary.bundle`, `data_packed.bundle`, `syn_multiply.bundle`, `gcobol/group1` (`comp5.cob`, `packed.cob`, `multiply2.cob`, `simple-arithmetic.cob`), `gcobol/group2` (`ADD_*`, `SUBTRACT_*`, `MULTIPLY_*`, `DIVIDE_*`, `COMPUTE_*`), and `nist85/NC.bundle`, ratcheting `baseline.toml` and `SCOREBOARD.md`.
  - [ ] Verify `-O0` through `-Os` torture equivalence on binary and packed-decimal arithmetic tests.
  - [ ] Run `./scripts/check-quality.sh`.

---

### Milestone 7: Numeric-Edited & Alphanumeric-Edited `PICTURE` Engine (`type:numeric-edited` — 412 Tests)

- [ ] **Implement `PICTURE` Editing Automaton (`crates/coblang-pic/src/edit.rs`)**
  - [ ] Compile parsed `PictureSpec` into a compact bytecode/control sequence stored in static read-only data (`CobFieldAttr::pic`).
  - [ ] Implement alphanumeric-edited formatting (`A`, `X`, `9` with `B`, `0`, `/` insertion characters).
  - [ ] Implement full numeric-edited formatting:
    - [ ] Zero suppression with spaces (`Z`) and check-protection asterisks (`*`), including all-zero suppression rules when every digit position is `Z` or `*`.
    - [ ] Floating insertion strings (`$$...`, `++...`, `--...`) positioning the currency or sign symbol immediately left of the first significant digit or decimal point.
    - [ ] Fixed leading/trailing signs (`+`, `-`, `CR`, `DB`).
    - [ ] Simple insertion (`B`, `0`, `/`, `,`) suppressed cleanly inside leading zero-suppression or floating-insertion zones.
    - [ ] `DECIMAL-POINT IS COMMA` swapping `.` and `,` roles, custom multi-byte `CURRENCY SIGN IS "..." WITH PICTURE SYMBOL c`, and `BLANK WHEN ZERO` overriding `*` suppression to spaces when value == 0.
- [ ] **Implement Numeric De-Editing (`MOVE edited-item TO numeric-item` — COBOL 2002 / GnuCOBOL / gcobol)**
  - [ ] Parse edited strings back into `FixedDecimal` by stripping currency symbols, grouping separators, spaces/`*`, and interpreting `+`/`-`/`CR`/`DB` signs so `MOVE numeric-edited TO numeric` works seamlessly.
- [ ] **Verify & Ratchet Milestone 7**
  - [ ] Add exhaustive unit tests covering every `PICTURE` editing combination (`Z`, `*`, `$`, `+`, `-`, `CR`, `DB`, `DECIMAL-POINT IS COMMA`, `BLANK WHEN ZERO`, de-editing) at 100% branch coverage.
  - [ ] Ratchet `data_display.bundle`, `gcobol/group2`, and `nist85/NC.bundle` numeric-edited tests, then run `./scripts/check-quality.sh`.

---

### Milestone 8: Control Flow, Conditions, Level-88, `EVALUATE`, `PERFORM`, `GO TO`, & `ALTER` (`feature:perform-eval-if` — 1,560, `feature:level-88` — 239)

- [ ] **Implement Full Conditional Expression & Abbreviated Combined Relation Lowering (`crates/coblang-sema/src/cond.rs`)**
  - [ ] Relational comparisons (`=`, `<>`, `<`, `<=`, `>`, `>=`, `EQUAL TO`, `NOT ...`):
    - [ ] Numeric vs Numeric: algebraic comparison across any mix of `DISPLAY`, `COMP-3`, `COMP`, `COMP-5`, `FLOAT`, and edited (when dialect allows).
    - [ ] Non-numeric vs Non-numeric: byte-by-byte comparison using active `PROGRAM COLLATING SEQUENCE` (`ASCII`, `EBCDIC`, or custom `ALPHABET`), padding the shorter operand with `SPACE` (`0x20` or `0x40` in EBCDIC!).
    - [ ] Numeric vs Alphanumeric (group/elementary) per dialect coercion rules.
  - [ ] Sign conditions (`IS [NOT] POSITIVE / NEGATIVE / ZERO`).
  - [ ] Class conditions (`IS [NOT] NUMERIC / ALPHABETIC / ALPHABETIC-LOWER / ALPHABETIC-UPPER / KANJI / DBCS / custom-CLASS-name`, validating packed/zoned sign & digit nibbles accurately for `IS NUMERIC`!).
  - [ ] Switch-status conditions (`SWITCH-n` / mnemonic ON/OFF via `COB_SWITCH_0..36` environment variables).
  - [ ] **Level-88 Condition Names & `SET cond-name TO TRUE / FALSE`**:
    - [ ] Lower `IF cond-88` to disjunction of equality and `VALUE a THRU b` inclusive range checks in the parent item's type domain.
    - [ ] Lower `SET cond-88 TO TRUE` (moving first `VALUE` literal to parent) and `SET cond-88 TO FALSE` (moving `WHEN SET TO FALSE IS` literal to parent).
  - [ ] **Abbreviated Combined Relation Conditions**:
    - [ ] Desugar `IF A > B AND < C OR = D` and parenthesized abbreviated forms (`IF A = (B OR C) AND NOT (D OR E)`) strictly per ANSI COBOL-85 precedence (`NOT` vs relational operator inheritance rules tested heavily in `nist85/NC.bundle` and `gcobol/group1/multiple-compares.cob`!).
- [ ] **Implement `IF` & `EVALUATE` Statements (`crates/coblang-mlir/src/from_hir.rs`)**
  - [ ] Lower `IF ... THEN ... [ELSE ...] END-IF` and `NEXT SENTENCE` (jumping to the implicit label immediately following the enclosing separator `.`!).
  - [ ] Lower `EVALUATE subj-1 [ALSO subj-2 ...]`:
    - [ ] Evaluate each selection subject once into a temporary (or lazy `TRUE`/`FALSE` subject).
    - [ ] Match `WHEN ANY`, `WHEN TRUE`, `WHEN FALSE`, `WHEN [NOT] expr [THRU expr2]`, `WHEN cond` across `ALSO` columns in strict top-to-bottom order, plus `WHEN OTHER`, lowering to `scf.if` / `cf.cond_br` chains.
- [ ] **Implement `PERFORM` (Inline & Out-of-Line Paragraph/Section Ranges), `GO TO`, & `ALTER` (`crates/coblang-mlir/src/cfg_perform.rs`)**
  - [ ] Inline `PERFORM`:
    - [ ] `PERFORM n TIMES ... END-PERFORM`
    - [ ] `PERFORM [WITH TEST BEFORE | AFTER] UNTIL cond ... END-PERFORM`
    - [ ] `PERFORM [WITH TEST BEFORE | AFTER] VARYING id-1 FROM f1 BY b1 UNTIL c1 [AFTER id-2 FROM f2 BY b2 UNTIL c2 ...] ... END-PERFORM` (implementing exact ANSI-85 multi-level `AFTER` reset/test/increment state-machine order!).
    - [ ] `EXIT PERFORM` (break) and `EXIT PERFORM CYCLE` (continue).
  - [ ] **Out-of-Line `PERFORM proc-1 [THRU proc-2]` + Fallthrough + `GO TO` + `ALTER` Lowering**:
    - [ ] Model the `PROCEDURE DIVISION` as an explicit paragraph/section basic-block graph where each paragraph `P_i` falls through sequentially to `P_{i+1}` UNLESS `P_i` is the active return point of an open `PERFORM ... THRU P_i` frame!
    - [ ] Maintain a fast stack-allocated `PerformReturnStack` (or Continuation Return-ID slot per paragraph end-point — zero heap allocation!) supporting recursive/nested `PERFORM`, `PERFORM` ranges that internally `GO TO` and jump back, OS/VS vs ISO `PERFORM` frame popping rules (`DialectConfig::perform_osvs`), `EXIT PARAGRAPH`, `EXIT SECTION`, `GO TO p1 p2 ... DEPENDING ON idx`, and `ALTER p1 TO [PROCEED TO] p2` (mutable target-index variable on altered `GO TO` paragraphs, tested in `nist85/OB.bundle` and `SG.bundle`!).
- [ ] **Verify & Ratchet Milestone 8**
  - [ ] Run `coblang-test` on `gcobol/group1` (`check_88.cob`, `multiple-compares.cob`, `perform-nested-exit.cob`, `simple-if.cob`, `simple-perform.cob`), `gnucobol/run_fundamental.bundle`, `gnucobol/run_misc.bundle`, `gnucobol/backcomp.bundle`, and `nist85/{NC,OB,SG}.bundle`.
  - [ ] Ratchet `baseline.toml`, `SCOREBOARD.md`, and `SCOREBOARD.json`, and verify `./scripts/check-quality.sh`.

---

### Milestone 9: `REDEFINES`, `RENAMES`, `OCCURS` / `ODO` Tables, Subscripts, Reference Modification, & `SEARCH` (`feature:redefines-renames` — 579, `feature:occurs-odo` — 556, `feature:refmod` — 150, `feature:search-sort-table` — 90)

- [ ] **Implement `REDEFINES` & Level-66 `RENAMES` Storage Aliasing (`crates/coblang-sema/src/layout.rs`)**
  - [ ] Assign `REDEFINES` group/elementary items the exact starting byte offset of the redefined target while advancing the enclosing group's high-water mark by `max(target_size, redefines_size)` (emitting error or warning on larger `REDEFINES` per `DialectConfig::larger_redefines_ok`).
  - [ ] Compute `66 new-name RENAMES item-1 [THRU item-2]` as an alphanumeric group view spanning `[offset(item-1) .. offset(item-2) + size(item-2))`.
- [ ] **Implement Fixed `OCCURS`, Multi-Dimensional Subscripting, `INDEXED BY`, & `OCCURS DEPENDING ON` (`ODO`)**
  - [ ] Support up to 7+ subscript dimensions (`ITEM(i, j, k)`), relative indexing (`ITEM(IDX + 3, IDX2 - 1)`), and `ALL` subscripts in intrinsic functions (`FUNCTION SUM(TBL(ALL))`).
  - [ ] Implement `USAGE IS INDEX` items (`4` or `8` byte byte-displacement/occurrence storage) and `SET idx TO n`, `SET idx UP BY n`, `SET idx DOWN BY n`.
  - [ ] Implement **Simple and Complex `OCCURS min TO max TIMES DEPENDING ON odo-obj`**:
    - [ ] Dynamically compute group lengths and variably-located trailing/subordinate fields when an `ODO` table precedes sibling items or nested `ODO`s!
    - [ ] Emit optional runtime subscript bounds checks (`EC-BOUND-SUBSCRIPT` / `EC-BOUND-ODO` when `-debug` / `-fec=bound` / `>>TURN EC-BOUND-SUBSCRIPT CHECKING ON` is enabled!).
- [ ] **Implement Reference Modification `identifier(offset : [length])` (`crates/coblang-mlir`)**
  - [ ] Lower 1-based dynamic/static `offset` and optional `length` (defaulting to `item_char_len - offset + 1`, scaled by character width `1` for ASCII/EBCDIC, `2` for `NATIONAL` UTF-16!) into `cobol.refmod` slice views, with `EC-BOUND-REF-MOD` bounds checking when enabled.
- [ ] **Implement `SEARCH` (Serial), `SEARCH ALL` (Binary), & Table `SORT` (`crates/coblang-mlir` & `crates/coblang-sort`)**
  - [ ] Serial `SEARCH tbl [VARYING idx] [AT END ...] WHEN cond-1 stmt-1 ... END-SEARCH`.
  - [ ] Binary `SEARCH ALL tbl [AT END ...] WHEN key-1 = expr-1 [AND key-2 = expr-2 ...] stmt ... END-SEARCH` honoring `ASCENDING` / `DESCENDING KEY` declarations and ODO current table bounds.
  - [ ] In-memory table `SORT tbl [ON ASCENDING/DESCENDING KEY k1 ...] [WITH DUPLICATES IN ORDER] [COLLATING SEQUENCE IS alpha]` (COBOL 2002 / GnuCOBOL / gcobol).
- [ ] **Verify & Ratchet Milestone 9**
  - [ ] Run `coblang-test` on `run_subscripts.bundle`, `run_refmod.bundle`, `syn_occurs.bundle`, `syn_redefines.bundle`, `syn_refmod.bundle`, `syn_subscripts.bundle`, `gcobol/group2`, and `nist85/NC.bundle`.
  - [ ] Ratchet `baseline.toml` and `SCOREBOARD.md`, and run `./scripts/check-quality.sh`.

---

### Milestone 10: String Verbs (`STRING`, `UNSTRING`, `INSPECT`, `TRANSFORM`) & Console/Env `ACCEPT` (`feature:string-inspect` — 166 Tests)

- [ ] **Implement `STRING` & `UNSTRING` (`crates/coblang-pic/src/string_verbs.rs` + `cob_string_*` / `cob_unstring_*` ABI)**
  - [ ] `STRING src-1 ... DELIMITED BY {SIZE | literal | id} ... INTO dst [WITH POINTER ptr] [ON OVERFLOW ...] [NOT ON OVERFLOW ...] END-STRING` (evaluating 1-based `POINTER`, transferring characters without space-filling untouching suffix bytes of `dst`, and firing `OVERFLOW` if `ptr < 1` or `ptr > len(dst)`!).
  - [ ] `UNSTRING src [DELIMITED BY [ALL] d1 [OR [ALL] d2 ...]] INTO dst-1 [DELIMITER IN del-1] [COUNT IN cnt-1] ... [WITH POINTER ptr] [TALLYING IN tally] [ON OVERFLOW ...] [NOT ON OVERFLOW ...] END-UNSTRING`.
- [ ] **Implement `INSPECT` & `TRANSFORM` (`crates/coblang-pic/src/inspect.rs` + `cob_inspect_*` ABI)**
  - [ ] `INSPECT target TALLYING tally-1 FOR {CHARACTERS | ALL p | LEADING p} [{BEFORE | AFTER} INITIAL bound] ...`
  - [ ] `INSPECT target REPLACING {CHARACTERS BY r | {ALL | LEADING | FIRST} p BY r} [{BEFORE | AFTER} INITIAL bound] ...` (including `TRAILING` extension in GnuCOBOL/gcobol).
  - [ ] `INSPECT target TALLYING ... REPLACING ...` combined form and `INSPECT target CONVERTING from-chars TO to-chars [{BEFORE | AFTER} INITIAL bound]`, plus legacy `TRANSFORM target CHARACTERS FROM a TO b`.
- [ ] **Implement Standard & Extended `ACCEPT` (`crates/coblang-rt-core/src/accept.rs`)**
  - [ ] `ACCEPT dst [FROM SYSIN / CONSOLE]` reading from `stdin` (handling EOF space-fill / zero-fill / `AT END` / `ON EXCEPTION` per dialect).
  - [ ] `ACCEPT dst FROM DATE [YYYYMMDD]`, `DAY [YYYYDDD]`, `DAY-OF-WEEK`, `TIME`, `MICROSECOND-TIME` (supporting deterministic `COB_CURRENT_DATE` test override used in `GnuCOBOL` & `gcobol` tests!).
  - [ ] `ACCEPT dst FROM ARGUMENT-NUMBER`, `ACCEPT dst FROM ARGUMENT-VALUE`, `ACCEPT dst FROM COMMAND-LINE`, `DISPLAY env-name UPON ENVIRONMENT-NAME` + `DISPLAY val UPON ENVIRONMENT-VALUE` + `ACCEPT dst FROM ENVIRONMENT-VALUE` / `FROM ENVIRONMENT "NAME"`!
- [ ] **Verify & Ratchet Milestone 10**
  - [ ] Run `coblang-test` on `run_accept.bundle`, `gcobol/group2` (`ACCEPT_*`, `INSPECT_*`, `STRING_*`, `UNSTRING_*`), and `nist85/NC.bundle`.
  - [ ] Ratchet `baseline.toml` and `SCOREBOARD.md`, and run `./scripts/check-quality.sh`.

---

### Milestone 11: Pointers, `BASED`/`ALLOCATE`/`FREE`, Subprograms (`CALL`/`CANCEL`/`ENTRY`), `cobcrun`, Nested/`GLOBAL`/`EXTERNAL`, & C Interop (`feature:call-subprog` — 722, `type:pointer` — 116, `feature:nested-global-external` — 146, `feature:c-interop` — 56, `feature:allocate-based` — 37)

- [ ] **Implement `USAGE POINTER`, `PROGRAM-POINTER`, `FUNCTION-POINTER`, `ADDRESS OF`, & Dynamic Memory (`ALLOCATE` / `FREE` / `BASED`)**
  - [ ] Implement `SET ptr TO ADDRESS OF item`, `SET ADDRESS OF linkage-or-based-item TO ptr`, `SET ptr UP/DOWN BY bytes`, `SET ptr TO NULL`, and `SET prog-ptr TO ENTRY "name"` / `ENTRY id`.
  - [ ] Implement `ALLOCATE {char-expr CHARACTERS [INITIALIZED] [RETURNING ptr] | based-item [INITIALIZED] [RETURNING ptr]}` (honoring `INITIALIZE` `VALUE` vs default byte rules tested in `gcobol/group2/ALLOCATE_Rules_6_-_9*`!) and `FREE ptr-or-based-item ...` (setting freed pointer to `NULL`, ignoring `NULL` safely, raising `EC-STORAGE-NOT-ALLOC` if invalid when checking active).
- [ ] **Implement Static & Dynamic `CALL`, `CANCEL`, `ENTRY`, `LINKAGE SECTION`, & `OMITTED` Parameters (`crates/coblang-mlir` & `crates/coblang-dynload`)**
  - [ ] Parameter passing modes:
    - [ ] `BY REFERENCE` (pass raw data pointer; pass `NULL` for `OMITTED` optional parameter, supporting `IF param IS [NOT] OMITTED`!).
    - [ ] `BY CONTENT` (`LENGTH OF` / copy into caller temporary buffer and pass temporary pointer so callee cannot mutate caller storage).
    - [ ] `BY VALUE` (`SIZE IS 1/2/4/8/AUTO` integer/float/pointer passed directly in C ABI register!).
  - [ ] Populate per-module `CobModule` metadata (`cob_call_params` / `NUMBER-OF-CALL-PARAMETERS` / `C$NARG` / `C$PARAMSIZE`!) before each COBOL call so callees can query parameter counts and sizes!
  - [ ] Implement `ENTRY "alt-entry" [USING ...]` secondary entry points jumping into mid-procedure dispatch while binding the alternate `LINKAGE` signature.
  - [ ] Implement `crates/coblang-dynload`:
    - [ ] Resolve `CALL "NAME"` or `CALL var`: check in-process registered `CobModule` symbol table first (for multi-unit compilation & nested programs), then search `COB_LIBRARY_PATH` / `-M` directories and current directory for `<NAME>.so` / `lib<NAME>.so` / `<NAME>.dylib` / `lib<NAME>.dylib` via `dlopen` + `dlsym` (encoding hyphen `-` -> `__` or hex mangling per `GnuCOBOL` / `gcobol` conventions!).
    - [ ] Implement `CANCEL` resetting a subprogram's `WORKING-STORAGE` to its initial image (and closing open local files) on next `CALL`!
- [ ] **Upgrade `cobcrun-shim` (`tools/scaffold-driver/src/bin/cobcrun_shim.rs`) to Production Dynamic Module Runner**
  - [ ] Load `<MODULE>` from `-M <dir>`, `COB_LIBRARY_PATH`, or `.` via `coblang-dynload`, initialize `CobolRuntimeContext` with forwarded `program_args`, invoke entry symbol, and exit with `RETURN-CODE`!
  - [ ] Verify `CompileModule` (`cobc -m` / `gcobol -shared`) + `RunBinary { via_cobcrun: true }` end-to-end!
- [ ] **Implement Nested Programs, `IS COMMON`, `IS INITIAL`, `IS RECURSIVE`, `GLOBAL`, & `EXTERNAL`**
  - [ ] Implement lexical scoping of nested programs: `IS COMMON` visibility to sibling programs, `GLOBAL` data items and `GLOBAL` `FD` file connectors shared with contained nested programs, and `EXTERNAL [AS "name"]` data items/files allocated via runtime named shared-segment registry (`cob_alloc_base` / `coblang_rt_external_lookup`) so separately compiled modules share the exact same bytes!
- [ ] **Implement Built-in System Subroutines (`CBL_*`, `C$*`, `SYSTEM`) & `libgcobol` POSIX Shims (`crates/coblang-rt-core` & `crates/coblang-rt-abi`)**
  - [ ] Implement `SYSTEM`, `CBL_OC_NANOSLEEP`, `CBL_AND`/`CBL_OR`/`CBL_XOR`/`CBL_EQ`/`CBL_IMP`/`CBL_NIMP`/`CBL_NOT`, `CBL_TOUPPER`/`CBL_TOLOWER`, `CBL_OPEN_FILE`/`CBL_CREATE_FILE`/`CBL_READ_FILE`/`CBL_WRITE_FILE`/`CBL_CLOSE_FILE`/`CBL_FLUSH_FILE`/`CBL_DELETE_FILE`/`CBL_COPY_FILE`/`CBL_CHECK_FILE_EXIST`/`CBL_RENAME_FILE`, `CBL_GET_CSR_POS`/`CBL_GET_CURRENT_DIR`/`CBL_CHANGE_DIR`/`CBL_CREATE_DIR`/`CBL_DELETE_DIR`, `C$NARG`, `C$PARAMSIZE`, `C$CALLEDBY`, `C$JUSTIFY`, `C$SLEEP`, `C$TOUPPER`, `C$TOLOWER`, `C$MAKEDIR`, `C$DELETE`, `C$COPY`, `C$FILEINFO`, and POSIX shims exercised by `tests/ported/gcobol/libgcobol.bundle`.
- [ ] **Verify & Ratchet Milestone 11**
  - [ ] Run `coblang-test` on `nist85/IC.bundle`, `gnucobol/data_pointer.bundle`, `gnucobol/used_binaries.bundle`, `gnucobol/run_extensions.bundle`, `gnucobol/standalone_cob.bundle`, and `gcobol/libgcobol.bundle`.
  - [ ] Ratchet `baseline.toml`, `SCOREBOARD.md`, and `SCOREBOARD.json`, and run `./scripts/check-quality.sh`.

---

### Milestone 12: Intrinsic `FUNCTION` Library (~105 Functions) & User-Defined Functions (`FUNCTION-ID`) (`feature:intrinsics` — 528, `feature:udf` — 30)

- [ ] **Implement Repository Resolution & Intrinsic Signature Typechecker (`crates/coblang-sema/src/intrinsics.rs`)**
  - [ ] Support `FUNCTION func-name [(arg-1 ...)] [refmod]` AND `REPOSITORY. FUNCTION ALL INTRINSIC` / `FUNCTION specific-name` bare-name invocation without the `FUNCTION` keyword!
  - [ ] Validate argument count (fixed, optional, variadic `arg-1 ...`, and table `ALL` subscript slice expansion) and return category (`Numeric`, `Integer`, `Alphanumeric`, `National`, `UTF-8`).
- [ ] **Implement Mathematical, Financial, & Statistical Intrinsics (`crates/coblang-intrinsics/src/math_stats.rs`)**
  - [ ] Implement `ABS`, `MOD`, `REM`, `INTEGER`, `INTEGER-PART`, `FRACTION-PART`, `SIGN`, `FACTORIAL`, `SQRT`, `EXP`, `EXP10`, `LOG`, `LOG10`, `PI`, `E`.
  - [ ] Implement trigonometric functions (`ACOS`, `ASIN`, `ATAN`, `COS`, `SIN`, `TAN`).
  - [ ] Implement financial functions (`ANNUITY`, `PRESENT-VALUE`).
  - [ ] Implement variadic & `ALL`-subscript statistical functions (`MAX`, `MIN`, `ORD-MAX`, `ORD-MIN`, `SUM`, `MEAN`, `MEDIAN`, `MIDRANGE`, `RANGE`, `VARIANCE`, `STANDARD-DEVIATION`) and stateful deterministic PRNG `RANDOM [(seed)]`.
  - [ ] Implement algebraic limit queries (`HIGHEST-ALGEBRAIC`, `LOWEST-ALGEBRAIC`, `SMALLEST-ALGEBRAIC`).
- [ ] **Implement Calendar, Date/Time, & ISO 8601 Formatted Date Intrinsics (`crates/coblang-intrinsics/src/datetime.rs`)**
  - [ ] Implement `CURRENT-DATE` (21-char `YYYYMMDDHHMMSSss+HHMM`, honoring `COB_CURRENT_DATE`), `WHEN-COMPILED`, `SECONDS-PAST-MIDNIGHT`.
  - [ ] Implement Lilian day/date conversions (`INTEGER-OF-DATE` from 1601-01-01 = day 1, `DATE-OF-INTEGER`, `INTEGER-OF-DAY`, `DAY-OF-INTEGER`, `DATE-TO-YYYYMMDD`, `DAY-TO-YYYYDDD`, `YEAR-TO-YYYY` sliding window).
  - [ ] Implement ISO 2014/2023 formatted date/time parser & formatter (`FORMATTED-CURRENT-DATE`, `FORMATTED-DATE`, `FORMATTED-TIME`, `FORMATTED-DATETIME`, `INTEGER-OF-FORMATTED-DATE`, `SECONDS-FROM-FORMATTED-TIME`, `COMBINED-DATETIME`, `TEST-DATE-YYYYMMDD`, `TEST-DAY-YYYYDDD`, `TEST-FORMATTED-DATETIME`, `LOCALE-DATE`, `LOCALE-TIME`).
- [ ] **Implement String, Conversion, Exception, & UTF-8 Intrinsics (`crates/coblang-intrinsics/src/strings.rs`)**
  - [ ] Implement `LENGTH`, `BYTE-LENGTH`, `CHAR`, `ORD`, `CHAR-NATIONAL`, `REVERSE`, `UPPER-CASE`, `LOWER-CASE`, `TRIM` (`LEADING` / `TRAILING` / both), `CONCAT` / `CONCATENATE`, `SUBSTITUTE`, `SUBSTITUTE-CASE`, `FIND-STRING` (COBOL 2023), `BASECONVERT`, `BOOLEAN-OF-INTEGER`, `INTEGER-OF-BOOLEAN`, `CONVERT` (hex/base64/format conversions), `STANDARD-COMPARE`, `LOCALE-COMPARE`.
  - [ ] Implement numeric string parsers & validators (`NUMVAL`, `NUMVAL-C`, `NUMVAL-F`, `TEST-NUMVAL`, `TEST-NUMVAL-C`, `TEST-NUMVAL-F`).
  - [ ] Implement reflection/exception intrinsics (`MODULE-NAME` / `MODULE-ID` / `MODULE-CALLER-ID` / `MODULE-DATE` / `MODULE-TIME` / `MODULE-SOURCE` / `MODULE-PATH` / `MODULE-FORMATTED-DATE`, `CONTENT-OF`, `EXCEPTION-FILE`, `EXCEPTION-FILE-N`, `EXCEPTION-LOCATION`, `EXCEPTION-LOCATION-N`, `EXCEPTION-STATEMENT`, `EXCEPTION-STATUS`, `UUID4`).
  - [ ] Implement National & UTF-8 intrinsics (`DISPLAY-OF`, `NATIONAL-OF`, `ULENGTH`, `UPOS`, `USUBSTR`, `USUPPLEMENTARY`, `UVALID`, `UWIDTH`).
- [ ] **Implement User-Defined Functions (`FUNCTION-ID`) (`crates/coblang-sema` & `crates/coblang-mlir`)**
  - [ ] Support `IDENTIFICATION DIVISION. FUNCTION-ID. func-name [AS "ext"]`, `LINKAGE SECTION` formal parameters + `RETURNING ret-item`, `PROCEDURE DIVISION USING ... RETURNING ret-item`, `EXIT FUNCTION`, `GOBACK`, and recursive/external UDF invocation inside arbitrary expressions (`MOVE FUNCTION MY-FN(X, Y) TO Z`).
- [ ] **Verify & Ratchet Milestone 12**
  - [ ] Unit test every single intrinsic function (including domain boundary errors -> `EC-ARGUMENT-FUNCTION` and default fallback return values) at 100% branch coverage.
  - [ ] Run `coblang-test` on `nist85/IF.bundle`, `gnucobol/syn_functions.bundle`, `gnucobol/run_functions.bundle`, and `gcobol/group2` (`FUNCTION_*`), ratcheting `baseline.toml` and `SCOREBOARD.md`.
  - [ ] Run `./scripts/check-quality.sh`.

---

### Milestone 13: Exception Hierarchy (`EC-*`), `>>TURN`, `DECLARATIVES`, & NIST `DB` Debug Module (`feature:declaratives-exceptions` — 532 Tests)

- [ ] **Implement Full ISO COBOL 2023 `EC-*` Exception Hierarchy & `>>TURN` Scoping (`crates/coblang-rt-core/src/exceptions.rs`)**
  - [ ] Model the hierarchical `ExceptionCondition` tree (`EC-ALL` -> `EC-ARGUMENT-*`, `EC-BOUND-*` [`SUBSCRIPT`, `ODO`, `REF-MOD`, `PTR`, `SET`, `TABLE-LIMIT`], `EC-DATA-*` [`INCOMPATIBLE`, `CONVERSION`, `PTR-NULL`, `INFINITY`, `NOT-A-NUMBER`], `EC-FLOW-*`, `EC-FUNCTION-*`, `EC-I-O-*` [`AT-END`, `EOP`, `IMP`, `FILE-SHARING`, `LOGIC-ERROR`, `PERMANENT-ERROR`, `RECORD-CONTENT`, `RECORD-OPERATION`, `WARNING`], `EC-OO-*`, `EC-ORDER-*`, `EC-OVERFLOW-*`, `EC-PROGRAM-*` [`ARG-MISMATCH`, `ARG-OMITTED`, `CANCEL-ACTIVE`, `NOT-FOUND`, `PTR-NULL`, `RECURSIVE-CALL`, `RESOURCES`], `EC-RAISING-*`, `EC-RANGE-*`, `EC-REPORT-*`, `EC-SCREEN-*`, `EC-SIZE-*` [`ADDRESS`, `EXPONENTIATION`, `OVERFLOW`, `TRUNCATION`, `UNDERFLOW`, `ZERO-DIVIDE`], `EC-SORT-MERGE-*`, `EC-STORAGE-*`, `EC-USER-*`, `EC-VALIDATE-*`).
  - [ ] Track compile-time `>>TURN ec-name CHECKING ON/OFF` + CLI `-debug` / `-fcobol-exceptions` / `-fec=...` / `-fno-ec=...` per HIR statement span so zero-cost straight-line code is emitted when checking is `OFF` and exact source location (`EXCEPTION-LOCATION`, `EXCEPTION-STATEMENT`) is recorded when `ON`!
  - [ ] Implement `RAISE [EXCEPTION ec-name | obj]` and `RESUME [AT NEXT STATEMENT | AT proc-name]`.
- [ ] **Implement `DECLARATIVES` (`USE AFTER STANDARD EXCEPTION/ERROR` & `USE FOR DEBUGGING`) (`crates/coblang-mlir/src/declaratives.rs`)**
  - [ ] Lower `USE [GLOBAL] AFTER STANDARD {EXCEPTION | ERROR} PROCEDURE ON {file-name ... | INPUT | OUTPUT | I-O | EXTEND | ec-name}`:
    - [ ] Dispatch order: file-specific local declarative -> file-specific `GLOBAL` parent declarative -> mode-specific (`INPUT`/`OUTPUT`/`I-O`/`EXTEND`) local declarative -> mode-specific `GLOBAL` parent declarative.
    - [ ] Guard against recursive declarative re-entry per ISO standard rules and return cleanly (`EXIT SECTION` / end of declarative section / `RESUME`) to the statement following the I/O or raising verb.
  - [ ] Implement the **NIST CCVS-85 `DB` Debug Module** (`USE FOR DEBUGGING ON {ALL PROCEDURES | proc-name | ALL REFERENCES OF id | file-name}` + `DEBUG-ITEM` special register: `DEBUG-LINE`, `DEBUG-NAME`, `DEBUG-SUB-1..3`, `DEBUG-CONTENTS` — runtime switch gated by `WITH DEBUGGING MODE` + runtime debug toggle)!
- [ ] **Verify & Ratchet Milestone 13**
  - [ ] Run `coblang-test` on `nist85/DB.bundle`, `gcobol/group1/declarative_1.cob`, `gcobol/group2` (`EC-*`, `DECLARATIVES*`), and `gnucobol/run_misc.bundle`.
  - [ ] Ratchet `baseline.toml` and `SCOREBOARD.md`, and run `./scripts/check-quality.sh`.

---

### Milestone 14: File I/O Subsystem Part 1 — `SEQUENTIAL`, `LINE SEQUENTIAL`, & `RELATIVE` Files (`feature:file-seq` — 692, `feature:file-rel` — 77)

- [ ] **Implement File Connector Architecture, `ASSIGN TO` Resolution, & `FILE STATUS` Table (`crates/coblang-fileio`)**
  - [ ] Implement `ASSIGN TO` filename resolution supporting literal paths, dynamic data-item paths, and environment-variable indirection (`DD_<NAME>`, `dd_<name>`, `<NAME>`, `COB_FILE_PATH`) matching both `GnuCOBOL` and `gcobol` expectations!
  - [ ] Implement complete two-character `FileStatus` state machine (`"00"`, `"02"`, `"04"`, `"05"`, `"07"`, `"10"`, `"14"`, `"21"`, `"22"`, `"23"`, `"24"`, `"30"`, `"34"`, `"35"`, `"37"`, `"38"`, `"39"`, `"41"`, `"42"`, `"43"`, `"44"`, `"46"`, `"47"`, `"48"`, `"49"`, `"51"`, `"61"`, `"91"`):
    - [ ] Update declared `FILE STATUS` data item on every I/O verb, fire `AT END` / `NOT AT END` or `INVALID KEY` / `NOT INVALID KEY` branches when present, invoke matching `USE AFTER EXCEPTION` declarative if no inline clause handled it, and abort with runtime diagnostic only when neither inline handler nor `FILE STATUS` nor declarative exists!
  - [ ] Support `SAME AREA` and `SAME RECORD AREA` buffer sharing across `FD`s, `EXTERNAL` and `GLOBAL` file connectors, and automatic close-on-`CANCEL` / `STOP RUN`.
- [ ] **Implement `ORGANIZATION IS SEQUENTIAL` & `LINE SEQUENTIAL` (`crates/coblang-fileio/src/sequential.rs`)**
  - [ ] Fixed-length `SEQUENTIAL` records (`RECORD CONTAINS n CHARACTERS`) and variable-length `SEQUENTIAL` records (`RECORD IS VARYING IN SIZE [[FROM min] TO max] DEPENDING ON rec-len` with configurable GnuCOBOL/IBM 2- or 4-byte record-length prefix headers).
  - [ ] `LINE SEQUENTIAL` text records (`LF` / `CRLF` delimiter handling, trailing whitespace stripping on `WRITE`, space-padding on `READ`, `COB_LS_FIXED` / `COB_LS_NULLS` control char escaping).
  - [ ] Verbs: `OPEN {INPUT | OUTPUT | I-O | EXTEND}` (`OPTIONAL` file `"05"` creation/EOF semantics!), `READ file [NEXT] [INTO dst] [AT END ...]`, `WRITE rec [FROM src] [{BEFORE | AFTER} ADVANCING {n LINES | PAGE | mnemonic}] [AT END-OF-PAGE ...]`, `REWRITE rec [FROM src]`, `CLOSE file [WITH LOCK | REEL/UNIT]`.
  - [ ] Implement full `LINAGE IS n LINES [WITH FOOTING AT f] [LINES AT TOP t] [LINES AT BOTTOM b]` and `LINAGE-COUNTER` special register pagination (`nist85/SQ.bundle`)!
- [ ] **Implement `ORGANIZATION IS RELATIVE` (`crates/coblang-fileio/src/relative.rs`)**
  - [ ] Slot-addressed file format (`slot_size = 1_flag_byte + max_record_size` or length-header slot) supporting `ACCESS MODE IS SEQUENTIAL`, `RANDOM`, and `DYNAMIC` keyed by 1-based `RELATIVE KEY`!
  - [ ] Verbs: `OPEN`, `READ [NEXT | PREVIOUS]`, `READ` by `RELATIVE KEY` (`"23"` on empty/deleted slot), `WRITE` (auto-incrementing `RELATIVE KEY` in `SEQUENTIAL` output mode; `"22"` duplicate key if slot occupied in `RANDOM`/`DYNAMIC`), `REWRITE`, `DELETE` (marking slot empty), `START file KEY IS {= | > | >= | < | <=} rel-key`!
  - [ ] Implement POSIX advisory/in-process record & file locking (`LOCK MODE IS EXCLUSIVE / AUTOMATIC / MANUAL`, `READ WITH [NO | KEPT] LOCK`, `UNLOCK file`).
- [ ] **Verify & Ratchet Milestone 14**
  - [ ] Run `coblang-test` on `nist85/SQ.bundle`, `nist85/RL.bundle`, `gnucobol/syn_file.bundle`, `gnucobol/run_file.bundle`, `gcobol/group1/line-sequential.cob`, and `gcobol/group2`.
  - [ ] Ratchet `baseline.toml`, `SCOREBOARD.md`, and `SCOREBOARD.json`, and run `./scripts/check-quality.sh`.

---

### Milestone 15: File I/O Subsystem Part 2 — Pure-Rust B+Tree ISAM Engine & Unlocking `Capability::Isam` (`feature:file-idx` — 204 Tests)

- [ ] **Implement Pure-Rust `coblang-isam` B+Tree Indexed File Engine (`crates/coblang-isam`)**
  - [ ] Design deterministic, portable single-file (or `.idx` + `.dat`) paged storage format with magic header (`COBLISAM`), schema descriptor (fixed/variable record bounds, primary key offset/length/split-segments, alternate key descriptors + `WITH DUPLICATES` flags, collating sequence ID), free-page bitmap, and order-$B$ B+Tree internal/leaf pages with doubly-linked leaf cursors for `O(1)` `READ NEXT` and `READ PREVIOUS`!
  - [ ] Support composite / split keys (`RECORD KEY IS seg-1 seg-2` — COBOL 2002 / GnuCOBOL) and arbitrary byte keys (binary, packed, alphanumeric, EBCDIC).
  - [ ] Enforce exact ISO COBOL-85 / 2023 `INDEXED` `FILE STATUS` semantics:
    - [ ] `"02"` on `READ` / `WRITE` / `REWRITE` when an `ALTERNATE RECORD KEY WITH DUPLICATES` has/creates a duplicate key value!
    - [ ] `"21"` sequence error on sequential `WRITE` out of ascending primary key order or `REWRITE` mutating the primary `RECORD KEY`.
    - [ ] `"22"` duplicate primary key (or duplicate alternate key without `WITH DUPLICATES`) on `WRITE` / `REWRITE`.
    - [ ] `"23"` record not found on keyed `READ` (`KEY IS alt-key`) or `START` (`=`, `>`, `>=`, `<`, `<=`, including generic/partial-key `START`!).
    - [ ] `"43"` on `REWRITE` / `DELETE` in `SEQUENTIAL` access mode without a preceding successful `READ`.
    - [ ] Preserve monotonic FIFO duplicate ordering for `ALTERNATE RECORD KEY WITH DUPLICATES`!
- [ ] **Enable `Capability::Isam` in `coblang-test-runner` (`tools/test-runner/src/capabilities.rs`)**
  - [ ] Insert `Capability::Isam` into `HostCapabilities::detect_host()` in `tools/test-runner/src/capabilities.rs`, unlocking ~92+ previously `SKIP`-gated ISAM tests across `nist85/IX.bundle` and `gnucobol/run_file.bundle`!
- [ ] **Verify & Ratchet Milestone 15**
  - [ ] Fuzz/property-test B+Tree page splits, merges, duplicate-key FIFO cursors, `START` boundary conditions, and `REWRITE` secondary-index maintenance in `crates/coblang-isam` with 100% fn/line/branch coverage.
  - [ ] Run `coblang-test` on `nist85/IX.bundle`, `gnucobol/run_file.bundle`, and `gcobol/group2`, ratcheting `baseline.toml` and `SCOREBOARD.md`.
  - [ ] Run `./scripts/check-quality.sh`.

---

### Milestone 16: File `SORT` & `MERGE` Pipeline (`feature:sort-merge` — 51 Tests)

- [ ] **Implement `SD` Sort-Merge File Descriptors & Pure-Rust `coblang-sort` Engine (`crates/coblang-sort`)**
  - [ ] Implement multi-key comparator supporting mixed `ASCENDING` / `DESCENDING` keys of any COBOL data type (`ALPHANUMERIC` under `PROGRAM` or `SORT` `COLLATING SEQUENCE`, `DISPLAY`, `COMP-3`, `COMP`, `COMP-5`, `FLOAT`), preserving stable input order when `WITH DUPLICATES IN ORDER` is specified!
  - [ ] Implement in-memory timsort/mergesort with spill-to-temp-run k-way external merge when record volume exceeds configurable memory threshold (`COB_SORT_MEMORY`).
  - [ ] Lower all four `SORT` I/O combinations in `crates/coblang-mlir`:
    - [ ] `USING file-1 ...` -> `GIVING out-file ...`
    - [ ] `INPUT PROCEDURE IS p1 [THRU p2]` (`RELEASE sort-rec [FROM src]`) -> `GIVING out-file ...`
    - [ ] `USING file-1 ...` -> `OUTPUT PROCEDURE IS p3 [THRU p4]` (`RETURN sort-file [INTO dst] AT END ...`)
    - [ ] `INPUT PROCEDURE` -> `OUTPUT PROCEDURE` (staged via the `SortSession` handle: run `INPUT PROCEDURE` collecting `RELEASE` records, sort, then run `OUTPUT PROCEDURE` pulling `RETURN` records — clean, deterministic, zero unsafe coroutines required!)
  - [ ] Implement `MERGE sd-file ON ... USING file-1 file-2 ... {GIVING ... | OUTPUT PROCEDURE IS ...}` and `SORT-RETURN` special register (`MOVE 16 TO SORT-RETURN` early-abort support).
- [ ] **Verify & Ratchet Milestone 16**
  - [ ] Run `coblang-test` on `nist85/ST.bundle`, `gnucobol/run_file.bundle`, `gnucobol/run_misc.bundle`, and `gcobol/group2`.
  - [ ] Ratchet `baseline.toml` and `SCOREBOARD.md`, and run `./scripts/check-quality.sh`.

---

### Milestone 17: `NATIONAL` (`UTF-16`), `UTF-8`, EBCDIC (`-finternal-ebcdic`), & Codepage Transcoding (`type:national-utf8` — 425, `feature:ebcdic` — 37)

- [ ] **Implement `PIC N` (`USAGE NATIONAL` UTF-16) & `PIC U` (`USAGE UTF-8`) Storage and Operations (`crates/coblang-pic/src/unicode.rs`)**
  - [ ] Support `N"..."`, `NX"..."`, `U"..."`, `UX"..."` literals, `NATIONAL` figurative constants (`SPACE` -> `U+0020`, `ZERO` -> `U+0030`, `HIGH-VALUE` -> `U+FFFF`), `PIC N` / National-Edited `MOVE`s, character-indexed reference modification, relational comparisons, `INSPECT` / `STRING` / `UNSTRING` on `NATIONAL` & `UTF-8`, and `DISPLAY-OF` / `NATIONAL-OF` conversions.
- [ ] **Implement `-finternal-ebcdic`, Custom `ALPHABET`s, & `-fexec-charset=<cp>` (`crates/coblang-decimal/src/charset.rs`)**
  - [ ] Embed clean-room pure-Rust bijective & multi-byte transcoding tables for `CP1140`, `CP037`, `CP500`, `CP1047`, `ISO-8859-1`, `ISO-8859-15`, `CP1252`, `UTF-8`, `UTF-16BE`, and `UTF-16LE` (falling back to `iconv` CLI/FFI only for exotic host-specific codepages if ever requested).
  - [ ] When `-finternal-ebcdic` (`TextEncoding::Ebcdic`) is active:
    - [ ] Encode string literals, `SPACE` (`0x40`), `ZERO` (`0xF0`), zoned decimal sign nibbles (`0xC0..0xC9` positive, `0xD0..0xD9` negative, `0xF0..0xF9` unsigned), and default comparisons in EBCDIC while transcoding `DISPLAY` / `ACCEPT` / `LINE SEQUENTIAL` console boundaries cleanly between internal EBCDIC and external ASCII/UTF-8 per `gcobol` & `GnuCOBOL` semantics!
- [ ] **Verify & Ratchet Milestone 17**
  - [ ] Run `coblang-test` across all `type:national-utf8` (`425` tests) and `feature:ebcdic` (`37` tests) in `gcobol/group2` and `gnucobol/*`.
  - [ ] Ratchet `baseline.toml` and `SCOREBOARD.md`, and run `./scripts/check-quality.sh`.

---

### Milestone 18: Declarative `REPORT SECTION` Report Writer Module (`feature:report-writer` — 74 Tests)

- [ ] **Implement Pure-Rust `coblang-report` Engine (`crates/coblang-report`)**
  - [ ] Parse and validate `REPORT IS rep-name` in `FD` and `RD rep-name` in `REPORT SECTION` (`CODE literal`, `CONTROLS ARE [FINAL] ctrl-1 ...`, `PAGE LIMIT IS n LINES [HEADING h] [FIRST DETAIL fd] [LAST DETAIL ld] [FOOTING f]`).
  - [ ] Support all 7 report group types (`REPORT HEADING`, `PAGE HEADING`, `CONTROL HEADING`, `DETAIL`, `CONTROL FOOTING`, `PAGE FOOTING`, `REPORT FOOTING`), `LINE NUMBER IS {n [ON NEXT PAGE] | PLUS n}`, `NEXT GROUP IS {n | PLUS n | NEXT PAGE}`, `COLUMN NUMBER IS c`, `SOURCE IS id`, `VALUE IS lit`, `SUM id ... [UPON detail-grp] [RESET ON ctrl]`, `GROUP INDICATE`, and `PRESENT WHEN cond`.
  - [ ] Maintain `LINE-COUNTER` and `PAGE-COUNTER` special registers per `RD`.
  - [ ] Implement `INITIATE`, `GENERATE {detail-group | report-name}` (summary reporting), `TERMINATE`, `SUPPRESS PRINTING`, and `USE BEFORE REPORTING group-name` declaratives!
- [ ] **Verify & Ratchet Milestone 18**
  - [ ] Run `coblang-test` on `nist85/RW.bundle`, `gnucobol/syn_reportwriter.bundle`, `gnucobol/run_reportwriter.bundle`, and `gcobol/group2`.
  - [ ] Ratchet `baseline.toml` and `SCOREBOARD.md`, and run `./scripts/check-quality.sh`.

---

### Milestone 19: Pure-Rust `SCREEN SECTION` TUI Engine & Unlocking `Capability::CursesPty` (`feature:screen` — 44 Tests)

- [ ] **Implement Pure-Rust `coblang-screen` Engine (`crates/coblang-screen`)**
  - [ ] Support both `SCREEN SECTION` hierarchies (`LINE [NUMBER IS [PLUS] n]`, `COLUMN [NUMBER IS [PLUS] c]`, `VALUE`, `PIC`, `FROM src`, `TO dst`, `USING var`, `BLANK SCREEN`, `BLANK LINE`, `BELL`, `BLINK`, `HIGHLIGHT`, `LOWLIGHT`, `REVERSE-VIDEO`, `UNDERLINE`, `FOREGROUND-COLOR`, `BACKGROUND-COLOR`, `AUTO`, `SECURE`, `REQUIRED`, `FULL`, `PROMPT CHARACTER IS ...`, `ZERO-FILL`, `LEFTLINE`, `OVERLINE`) and extended `DISPLAY`/`ACCEPT ... AT LINE l COLUMN c` / `AT llcc`!
  - [ ] Populate `CRT STATUS` (4-digit function/escape key codes: `0000` normal enter, `1001`..`1064` F-keys, `2001`.. page/arrow keys, `8000` timeout) and `CURSOR` (`LLCC` / `LLLCCC`) special registers.
  - [ ] Implement dual rendering/input backends behind a safe `TerminalBackend` trait:
    - [ ] ANSI/VT100 raw-termios TTY backend when attached to an interactive PTY/terminal.
    - [ ] Deterministic virtual 24x80 (or `COB_SCREEN_LINES` x `COB_SCREEN_COLS`) frame-buffer + keystroke-stream backend for automated/piped test execution!
- [ ] **Enable `Capability::CursesPty` in `coblang-test-runner` (`tools/test-runner/src/capabilities.rs`)**
  - [ ] Insert `Capability::CursesPty` into `HostCapabilities::detect_host()` (using a PTY wrapper in `process.rs` when `required_capabilities` contains `CursesPty`!), unlocking all `23` skipped `SCREEN` tests!
- [ ] **Verify & Ratchet Milestone 19**
  - [ ] Run `coblang-test` on `gnucobol/syn_screen.bundle` and `gnucobol/run_manual_screen.bundle`.
  - [ ] Ratchet `baseline.toml` and `SCOREBOARD.md`, and run `./scripts/check-quality.sh`.

---

### Milestone 20: Pure-Rust `XML` / `JSON` Engines (Unlocking `Capability::Xml` & `Capability::Json`) & Object-Oriented COBOL (`feature:xml-json` — 28, `feature:oo` — 16)

- [ ] **Implement Pure-Rust `XML PARSE`, `XML GENERATE`, `JSON PARSE`, & `JSON GENERATE` (`crates/coblang-xml-json`)**
  - [ ] Streaming `XML PARSE doc PROCESSING PROCEDURE IS p1 [THRU p2] [ON EXCEPTION ...] [NOT ON EXCEPTION ...] END-XML`:
    - [ ] Populate `XML-EVENT` (`"START-OF-DOCUMENT"`, `"START-OF-ELEMENT"`, `"ATTRIBUTE-NAME"`, `"ATTRIBUTE-CHARACTERS"`, `"CONTENT-CHARACTERS"`, `"END-OF-ELEMENT"`, `"COMMENT"`, `"PROCESSING-INSTRUCTION-TARGET"`, `"PROCESSING-INSTRUCTION-DATA"`, `"EXCEPTION"`, `"END-OF-DOCUMENT"`), `XML-CODE` (`S9(9) COMP-5` — allow user procedure to `MOVE -1 TO XML-CODE` to abort!), `XML-TEXT`, `XML-NTEXT`, `XML-NAMESPACE`, `XML-NAMESPACE-PREFIX`.
  - [ ] `XML GENERATE dst FROM src-group [COUNT IN cnt] [WITH ATTRIBUTES] [WITH XML-DECLARATION] [NAMESPACE IS ns] [NAME OF field IS "alias" ...] [SUPPRESS field WHEN ZERO/SPACE/LOW-VALUE ...] [ON EXCEPTION ...]`.
  - [ ] `JSON GENERATE dst FROM src-group [COUNT IN cnt] [NAME OF field IS "alias" | OMITTED ...] [SUPPRESS ...] [CONVERTING bool-field TO BOOLEAN USING cond-88] [ON EXCEPTION ...]` and `JSON PARSE src INTO dst-group [WITH DETAIL] [NAME OF ...] [SUPPRESS ...] [ON EXCEPTION ...]` updating `JSON-CODE` and `JSON-STATUS`!
  - [ ] Insert `Capability::Xml` and `Capability::Json` into `HostCapabilities::detect_host()` in `tools/test-runner/src/capabilities.rs`, unlocking all `26` previously skipped `run_ml.bundle` XML/JSON tests!
- [ ] **Implement ISO COBOL 2002/2023 Object-Oriented COBOL (`CLASS-ID`, `INTERFACE-ID`, `METHOD-ID`, `INVOKE`)**
  - [ ] Lower `CLASS-ID cls [INHERITS FROM base]`, `FACTORY ... END FACTORY` (singleton class methods & factory data), `OBJECT ... END OBJECT` (heap-allocated instance layout + vtable pointer), `METHOD-ID. meth-name`, `USAGE OBJECT REFERENCE [cls]`, `INVOKE obj "meth" [USING ...] [RETURNING ...]`, and inline method invocation `obj::"meth"(...)`!
- [ ] **Verify & Ratchet Milestone 20**
  - [ ] Run `coblang-test` on `gnucobol/run_ml.bundle`, `gnucobol/syn_oo.bundle`, `gcobol/group1/simple-classes.cob`, and `gcobol/group2`.
  - [ ] Ratchet `baseline.toml` and `SCOREBOARD.md`, and run `./scripts/check-quality.sh`.

---

### Milestone 21: MLIR Optimization Passes, Torture-Matrix Hardening (`-O0`..`-Os`), DWARF Debug Info (`-g`), Scaffold Retirement, & Final Release Verification

- [ ] **Implement COBOL-Aware MLIR Optimization Passes (`crates/coblang-mlir/src/passes.rs`)**
  - [ ] **Static `PERFORM` Inlining / Direct-Call Promotion**: Promote non-escaped, single-exit `PERFORM para` ranges without `ALTER` or internal `GO TO` jumps into structured `func.call` / inlined regions so LLVM's SROA, GVN, and loop vectorizers optimize `PERFORM VARYING` loops aggressively at `-O2`/`-O3`!
  - [ ] **Constant `MOVE` & `PICTURE` Folding**: Precompute constant literal -> `PICTURE` formatting (`MOVE 123.45 TO WS-EDITED` or `MOVE "ABC" TO WS-X10`) at compile time into static constant byte slices.
  - [ ] **Redundant Decimal Pack/Unpack Elimination**: Fuse consecutive `cobol.from_decimal` + `cobol.to_decimal` pairs on intermediate `WORKING-STORAGE` values when no aliasing `REDEFINES` write intervenes.
- [ ] **Implement DWARF Source-Level Debug Metadata (`-g`) (`crates/coblang-codegen/src/debug_info.rs`)**
  - [ ] Propagate `SourceSpan` -> `melior::ir::Location::new(&ctx, filename, line, col)` on every `cobol.*` and lowered `llvm.*` MLIR operation.
  - [ ] Emit `DICompileUnit`, `DISubprogram` (for programs, sections/paragraphs, UDFs, methods), and `DILocalVariable` / `DIGlobalVariableExpression` for `WORKING-STORAGE` / `LOCAL-STORAGE` / `LINKAGE` items so `lldb` and `gdb` step line-by-line through `.cob` files!
- [ ] **Retire Zero-Compiler Scaffold Fallback (`tools/scaffold-driver` & `tools/test-runner`)**
  - [ ] Remove `EXIT_UNIMPLEMENTED` (`78`) fallback paths once every construct across the 2,527-test corpus is lowered cleanly; update version banners from `[zero-compiler mode]` to `[MLIR/LLVM 20]`.
  - [ ] Audit every remaining `XFAIL` and `SKIP` in `tests/expectations/baseline.toml` to verify each one is strictly an upstream-annotated `XFAIL` (`AT_XFAIL_IF` / `dg-xfail-if`), upstream `SKIP` (`AT_SKIP_IF` / `dg-skip-if`), or opposite-endian target test (`Capability::BigEndian` on little-endian hosts).
- [ ] **Final Full-Suite Torture, Coverage, & Cross-Target Verification**
  - [ ] Run torture optimization verification (`-O0`, `-O1`, `-O2`, `-O3`, `-Os`) across `gcobol`, `gnucobol`, and `nist85`.
  - [ ] Regenerate `tests/expectations/baseline.toml`, `tests/expectations/SCOREBOARD.md`, `tests/expectations/SCOREBOARD.json`, and `README.md` doc/test coverage badges (`scripts/update-readme-shields.py --metric all`).
  - [ ] Verify `llvm-lit` integration end-to-end (`coblang-test emit-lit` + `lit tests/`).
  - [ ] Execute `./scripts/check-quality.sh` — confirming **100% rustfmt, 0 clippy warnings, 100% rustdoc + arg docs, 0 clean-room violations, 100.00% function/line/branch `llvm-cov`, and 100% ratcheted pass rate across all 2,527 conformance tests**.

---

## 3. Per-Bundle Conformance Ratchet Checklist (All 56 `.bundle` Files / 2,527 Tests)

Each bundle below is verified via `cargo run -p coblang-test-runner --bin coblang-test -- run --bundle <path> --baseline tests/expectations/baseline.toml --ratchet`:

### 3.1 Canary Suite (`tests/canary/` — 2 Tests)
- [ ] `tests/canary/canary.bundle` — `canary/scaffold-syntax` (`PASS`) + `canary/scaffold-skip` (`SKIP`)

### 3.2 NIST CCVS-85 Suite (`tests/ported/nist85/` — 426 Tests across 12 Modules)
- [ ] `tests/ported/nist85/NC.bundle` — COBOL Nucleus (`PASS` all CCVS-85 subtest counts, zero `*FAIL*`)
- [ ] `tests/ported/nist85/SM.bundle` — Source Text Manipulation (`COPY` / `REPLACE`)
- [ ] `tests/ported/nist85/IC.bundle` — Inter-Program Communication (`CALL` / `CANCEL`)
- [ ] `tests/ported/nist85/SQ.bundle` — Sequential I-O & `LINAGE`
- [ ] `tests/ported/nist85/RL.bundle` — Relative I-O
- [ ] `tests/ported/nist85/IX.bundle` — Indexed I-O (`Capability::Isam` unlocked)
- [ ] `tests/ported/nist85/ST.bundle` — Sort-Merge (`SORT` / `MERGE` / `SD`)
- [ ] `tests/ported/nist85/SG.bundle` — Segmentation & `PERFORM`/`ALTER` control flow
- [ ] `tests/ported/nist85/OB.bundle` — Obsolete COBOL-85 facilities (`ALTER`, `AUTHOR`, memory size, etc.)
- [ ] `tests/ported/nist85/IF.bundle` — Intrinsic Functions Addendum
- [ ] `tests/ported/nist85/RW.bundle` — Report Writer (`RD` / `INITIATE` / `GENERATE` / `TERMINATE`)
- [ ] `tests/ported/nist85/DB.bundle` — Debug Module (`USE FOR DEBUGGING` / `DEBUG-ITEM`)

### 3.3 GCC `gcobol` Suite (`tests/ported/gcobol/` — 543 Tests across 4 Bundles)
- [ ] `tests/ported/gcobol/top_level.bundle` — `cobol.dg/*.cob` DejaGnu diagnostics, preprocess, and PR regressions
- [ ] `tests/ported/gcobol/group1.bundle` — All 16 foundational `cobol.dg/group1/*.cob` programs (`check_88`, `comp5`, `declarative_1`, `display`, `display2`, `escape`, `line-sequential`, `multiple-compares`, `multiply2`, `packed`, `perform-nested-exit`, `pointer1`, `simple-arithmetic`, `simple-classes`, `simple-if`, `simple-perform`)
- [ ] `tests/ported/gcobol/group2.bundle` — COBOLworx UAT & ISO 2023 regression suite across `-O0`..`-Os` torture levels
- [ ] `tests/ported/gcobol/libgcobol.bundle` — Runtime compatibility & POSIX binding tests (`libgcobol/{compat,posix}/t/*.cbl`)

### 3.4 `GnuCOBOL` Suite (`tests/ported/gnucobol/` — 1,556 Tests across 39 Bundles)
- [ ] `tests/ported/gnucobol/configuration.bundle` — `-conf=`, dialect flags, warning/error overrides
- [ ] `tests/ported/gnucobol/listings.bundle` — `-t` / `-T` source & symbol listings
- [ ] `tests/ported/gnucobol/syn_copy.bundle` — `COPY`, `REPLACE`, CDF directives & error diagnostics
- [ ] `tests/ported/gnucobol/syn_definition.bundle` — Data/Procedure definition diagnostics
- [ ] `tests/ported/gnucobol/syn_literals.bundle` — String, hex, national, UTF-8, numeric literal diagnostics
- [ ] `tests/ported/gnucobol/syn_value.bundle` — `VALUE` clause legality & range diagnostics
- [ ] `tests/ported/gnucobol/syn_move.bundle` — `MOVE` category compatibility diagnostics
- [ ] `tests/ported/gnucobol/syn_multiply.bundle` — Arithmetic target/operand diagnostics
- [ ] `tests/ported/gnucobol/syn_redefines.bundle` — `REDEFINES` & `66 RENAMES` diagnostics
- [ ] `tests/ported/gnucobol/syn_occurs.bundle` — `OCCURS` & `DEPENDING ON` diagnostics
- [ ] `tests/ported/gnucobol/syn_subscripts.bundle` — Subscript & index bounds/type diagnostics
- [ ] `tests/ported/gnucobol/syn_refmod.bundle` — Reference modification static bounds/type diagnostics
- [ ] `tests/ported/gnucobol/syn_set.bundle` — `SET` target/source legality diagnostics
- [ ] `tests/ported/gnucobol/syn_functions.bundle` — Intrinsic & UDF `FUNCTION` signature diagnostics
- [ ] `tests/ported/gnucobol/syn_file.bundle` — `SELECT`, `FD`, `SD`, and file I/O clause diagnostics
- [ ] `tests/ported/gnucobol/syn_reportwriter.bundle` — `REPORT SECTION` (`RD`) syntax/sema diagnostics
- [ ] `tests/ported/gnucobol/syn_screen.bundle` — `SCREEN SECTION` syntax/sema diagnostics
- [ ] `tests/ported/gnucobol/syn_oo.bundle` — `CLASS-ID` / `METHOD-ID` / `OBJECT REFERENCE` diagnostics
- [ ] `tests/ported/gnucobol/syn_misc.bundle` — Cross-cutting COBOL dialect & extension diagnostics
- [ ] `tests/ported/gnucobol/data_display.bundle` — Zoned `USAGE DISPLAY` & edited `PICTURE` execution tests
- [ ] `tests/ported/gnucobol/data_binary.bundle` — `BINARY` / `COMP` / `COMP-4` / `COMP-5` endian & truncation tests
- [ ] `tests/ported/gnucobol/data_packed.bundle` — `PACKED-DECIMAL` (`COMP-3`) & `COMP-6` BCD execution tests
- [ ] `tests/ported/gnucobol/data_pointer.bundle` — `POINTER` & `PROGRAM-POINTER` arithmetic and aliasing tests
- [ ] `tests/ported/gnucobol/run_returncode.bundle` — `RETURN-CODE` & `STOP RUN` exit status tests
- [ ] `tests/ported/gnucobol/run_fundamental.bundle` — Core control flow, expressions, and verbs
- [ ] `tests/ported/gnucobol/run_initialize.bundle` — `INITIALIZE` (`REPLACING`, `FILLER`, `VALUE`, tables)
- [ ] `tests/ported/gnucobol/run_subscripts.bundle` — Runtime multi-dimensional subscripts, `ODO`, `EC-BOUND-SUBSCRIPT`
- [ ] `tests/ported/gnucobol/run_refmod.bundle` — Runtime reference modification & `EC-BOUND-REF-MOD`
- [ ] `tests/ported/gnucobol/run_accept.bundle` — `ACCEPT` (`DATE`/`DAY`/`TIME`, `COMMAND-LINE`, `ARGUMENT-VALUE`, `ENVIRONMENT`)
- [ ] `tests/ported/gnucobol/run_functions.bundle` — Runtime execution of all ~105 intrinsic functions & UDFs
- [ ] `tests/ported/gnucobol/run_file.bundle` — Runtime `SEQUENTIAL`, `LINE SEQUENTIAL`, `RELATIVE`, `INDEXED`, & `SORT`
- [ ] `tests/ported/gnucobol/run_reportwriter.bundle` — Runtime `REPORT SECTION` pagination, control breaks, & `SUM`
- [ ] `tests/ported/gnucobol/run_manual_screen.bundle` — Automated PTY/headless `SCREEN SECTION` & `CRT STATUS` tests
- [ ] `tests/ported/gnucobol/run_ml.bundle` — Runtime `XML GENERATE/PARSE` & `JSON GENERATE/PARSE`
- [ ] `tests/ported/gnucobol/run_extensions.bundle` — `GnuCOBOL` extensions, `CBL_*`, `C$*`, `CALL`, C companions
- [ ] `tests/ported/gnucobol/run_misc.bundle` — Miscellaneous runtime edge cases, declaratives, `EC-*`, nested programs
- [ ] `tests/ported/gnucobol/backcomp.bundle` — Backward-compatibility dialect & runtime behavior
- [ ] `tests/ported/gnucobol/used_binaries.bundle` — `cobc` + `cobcrun` `-m` / `-x` multi-module & C binary tests
- [ ] `tests/ported/gnucobol/standalone_cob.bundle` — Standalone `.cob` end-to-end programs

---

## 4. Per-Taxonomy `FeatureTag` Scoreboard Ratchet Checklist (All 41 Active Tags)

- [ ] `stage:preprocess` (478 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `stage:parse-sema` (547 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `type:alphanumeric` (1,555 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `type:numeric-display` (1,395 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `type:numeric-edited` (412 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `type:binary-comp5` (611 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `type:packed-comp3` (205 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `type:float` (74 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `type:pointer` (116 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `type:national-utf8` (425 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `type:128bit-decimal` (7 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:level-88` (239 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:redefines-renames` (579 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:occurs-odo` (556 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:refmod` (150 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:arith` (722 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:move-init` (1,410 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:string-inspect` (166 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:perform-eval-if` (1,560 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:search-sort-table` (90 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:call-subprog` (722 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:nested-global-external` (146 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:allocate-based` (37 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:declaratives-exceptions` (532 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:intrinsics` (528 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:udf` (30 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:file-seq` (692 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:file-rel` (77 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:file-idx` (204 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:sort-merge` (51 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:report-writer` (74 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:screen` (44 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:xml-json` (28 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:oo` (16 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:ebcdic` (37 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `feature:c-interop` (56 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `dialect:iso85` (426 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `dialect:iso2023` (485 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `dialect:ibm` (18 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `dialect:mf` (39 tests) — ratcheted in `SCOREBOARD.md`
- [ ] `dialect:gnucobol` (1,557 tests) — ratcheted in `SCOREBOARD.md`

---

## 5. Per-Crate 100% Documentation & 100% Branch Coverage Sign-Off Checklist

- [ ] `coblang-common` (`.`) — 100% docs (`//!`, `///`, `# Arguments`) + 100% fn/line/branch `llvm-cov`
- [ ] `coblang-diag` (`crates/coblang-diag`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-pp` (`crates/coblang-pp`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-syntax` (`crates/coblang-syntax`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-hir` (`crates/coblang-hir`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-sema` (`crates/coblang-sema`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-mlir` (`crates/coblang-mlir`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-codegen` (`crates/coblang-codegen`) — 100% docs + 100% fn/line/branch `llvm-cov` + audited `deny(unsafe_code)`
- [ ] `coblang-decimal` (`crates/coblang-decimal`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-pic` (`crates/coblang-pic`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-intrinsics` (`crates/coblang-intrinsics`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-fileio` (`crates/coblang-fileio`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-isam` (`crates/coblang-isam`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-sort` (`crates/coblang-sort`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-report` (`crates/coblang-report`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-screen` (`crates/coblang-screen`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-xml-json` (`crates/coblang-xml-json`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-rt-core` (`crates/coblang-rt-core`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-dynload` (`crates/coblang-dynload`) — 100% docs + 100% fn/line/branch `llvm-cov` + audited `deny(unsafe_code)`
- [ ] `coblang-rt-abi` (`crates/coblang-rt-abi`) — 100% docs + 100% fn/line/branch `llvm-cov` + audited `deny(unsafe_code)`
- [ ] `coblang-scaffold-driver` (`tools/scaffold-driver`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`
- [ ] `coblang-test-runner` & `coblang-test-importer` (`tools/test-*`) — 100% docs + 100% fn/line/branch `llvm-cov` + `forbid(unsafe_code)`

---

## 6. Technical Reference Appendices

### Appendix A: Custom `cobol` MLIR Dialect Operation Catalog (`crates/coblang-mlir`)

| MLIR Operation | Key Attributes & Operands | Lowering Target (`-O0` / General vs `-O2` Fast Path) |
| :--- | :--- | :--- |
| `cobol.program` | `@sym`, `is_initial`, `is_recursive`, `convention` | `llvm.func @sym(...)` + `CobModule` frame push/pop + optional `@main` |
| `cobol.storage` | `@sym`, `section`, `size`, `align`, `init_bytes` | `llvm.mlir.global` (`WORKING-STORAGE`), `llvm.alloca` (`LOCAL-STORAGE`), `@coblang_rt_external_lookup` (`EXTERNAL`) |
| `cobol.field_view` | `%base_ptr`, `%offset`, `%len`, `#attr` | Stack/SSA `{size, data_ptr, &static_attr}` `CobField` struct |
| `cobol.subscript` | `%base`, `%indices...`, `%strides...`, `%odo_max` | `arith.muli` + `arith.addi` + optional `@coblang_rt_check_subscript` -> `llvm.getelementptr` |
| `cobol.refmod` | `%field`, `%one_based_off`, `%opt_len`, `char_width` | Offset/length slice math + optional `@coblang_rt_check_refmod` -> sliced `CobField` |
| `cobol.move` | `%src`, `%dst` | Inline `llvm.memcpy`/`llvm.memset` or `arith` int cast when static; `@cob_move` / `@__gg__move` for general/edited |
| `cobol.init` | `%dst`, `replacing_rules`, `to_value` | Unrolled `cobol.move` sequence for fixed groups; `@coblang_rt_initialize` for dynamic ODO tables |
| `cobol.arith` | `op` (`add`/`sub`/`mul`/`div`/`pow`), `%lhs`, `%rhs`, `rounded_mode` | Inline `arith.*i` / `@llvm.s*.with.overflow.i64/i128` for `COMP-5`/scaled `<=37` digits; `@cob_decimal_*` for 38-digit/composite |
| `cobol.cmp` | `pred`, `%lhs`, `%rhs`, `collating_seq` | Inline `arith.cmpi` / `arith.cmpf` / `memcmp`; `@cob_cmp` / `@cob_numeric_cmp` for mixed/custom alphabet |
| `cobol.class_check` | `class_kind`, `%field` | Inline byte-range loop or `@coblang_rt_is_numeric` / `@coblang_rt_check_class` |
| `cobol.perform_call` | `@start_para`, `@end_para` | `func.call` when statically structured; return-slot push + `cf.br ^start_para` when `GO TO`/`ALTER`/fallthrough present |
| `cobol.call` | `%callee_name_or_ptr`, `args`, `modes`, `has_exception` | Direct `llvm.call` when static symbol known; `@cob_call` / `@cob_resolve` via `coblang-dynload` when dynamic |
| `cobol.display` / `accept` | `operands`, `device`, `no_advancing` | `@cob_display` / `@__gg__display` and `@cob_accept` |
| `cobol.string` / `unstring` / `inspect` | `%target`, `clauses...`, `%ptr`, `%tally` | `@cob_string_*`, `@cob_unstring_*`, `@cob_inspect_*` state-machine calls |
| `cobol.file_op` | `verb` (`open`/`read`/`write`/`rewrite`/`delete`/`start`/`close`), `%file` | `@cob_open`, `@cob_read`, `@cob_write`, `@cob_rewrite`, `@cob_delete`, `@cob_start`, `@cob_close` + declarative dispatch |
| `cobol.sort_merge` | `kind`, `%sd_file`, `keys`, `using`, `giving`, `@in_proc`, `@out_proc` | `@coblang_rt_sort_init` -> `@in_proc` / `USING` -> `@coblang_rt_sort_process` -> `@out_proc` / `GIVING` |
| `cobol.report_op` | `verb` (`initiate`/`generate`/`terminate`), `%rd` | `@coblang_rt_report_initiate`, `@coblang_rt_report_generate`, `@coblang_rt_report_terminate` |
| `cobol.screen_op` | `verb` (`display`/`accept`), `%screen_desc` | `@coblang_rt_screen_display`, `@coblang_rt_screen_accept` |
| `cobol.xml_json_op` | `verb`, `%doc`, `%target`, `@proc` | `@coblang_rt_xml_parse`, `@coblang_rt_xml_generate`, `@coblang_rt_json_parse`, `@coblang_rt_json_generate` |

### Appendix B: Clean-Room Runtime C-ABI Export Surface (`crates/coblang-rt-abi`)

All symbols below are implemented in 100% clean-room Rust (`Apache-2.0 WITH LLVM-exception`), delegating immediately after pointer/slice validation in `coblang-rt-abi` to `#[forbid(unsafe_code)]` crates:

- **Lifecycle & Module Stack**: `cob_init`, `cob_init_nomain`, `cob_tidy`, `cob_stop_run`, `cob_module_enter`, `cob_module_leave`, `cob_get_Iteration`, `cob_set_location`.
- **Data Movement & Comparison**: `cob_move`, `cob_cmp`, `cob_numeric_cmp`, `cob_cmp_int`, `cob_cmp_uint`, `cob_get_int`, `cob_set_int`, `cob_real_get_sign`, `cob_real_put_sign`, `__gg__move`, `__gg__compare`.
- **Decimal & Fast-Int Arithmetic**: `cob_decimal_init`, `cob_decimal_clear`, `cob_decimal_set_field`, `cob_decimal_get_field`, `cob_decimal_set_llint`, `cob_decimal_add`, `cob_decimal_sub`, `cob_decimal_mul`, `cob_decimal_div`, `cob_decimal_pow`, `cob_decimal_cmp`, `cob_add_int`, `cob_sub_int`, `__gg__add`, `__gg__subtract`, `__gg__multiply`, `__gg__divide`.
- **Console, String, & Intrinsic Operations**: `cob_display`, `cob_accept`, `cob_accept_date`, `cob_accept_day`, `cob_accept_day_of_week`, `cob_accept_time`, `cob_accept_arg_number`, `cob_accept_arg_value`, `cob_accept_command_line`, `cob_accept_environment`, `cob_set_environment`, `cob_string_init`, `cob_string_delimited`, `cob_string_append`, `cob_string_finish`, `cob_unstring_init`, `cob_unstring_delimited`, `cob_unstring_into`, `cob_unstring_tallying`, `cob_unstring_finish`, `cob_inspect_init`, `cob_inspect_start`, `cob_inspect_before`, `cob_inspect_after`, `cob_inspect_characters`, `cob_inspect_all`, `cob_inspect_leading`, `cob_inspect_first`, `cob_inspect_trailing`, `cob_inspect_converting`, `cob_inspect_finish`, `cob_intr_*`, `__gg__display`.
- **Dynamic Linking & Memory**: `cob_call`, `cob_func`, `cob_cancel`, `cob_resolve`, `cob_call_error`, `cob_malloc`, `cob_free`, `cob_alloc_base`, `__gg__call`.
- **File I/O, ISAM, Sort, Report, Screen, XML/JSON**: `cob_open`, `cob_close`, `cob_read`, `cob_read_next`, `cob_write`, `cob_rewrite`, `cob_delete`, `cob_start`, `cob_unlock_file`, `cob_commit`, `cob_rollback`, `cob_file_sort_init`, `cob_file_sort_init_key`, `cob_file_sort_using`, `cob_file_sort_giving`, `cob_file_release`, `cob_file_return`, `cob_file_sort_close`, `coblang_rt_report_*`, `coblang_rt_screen_*`, `coblang_rt_xml_*`, `coblang_rt_json_*`.
- **Standard System & Companion C Routines**: `SYSTEM`, `CBL_AND`, `CBL_OR`, `CBL_XOR`, `CBL_EQ`, `CBL_IMP`, `CBL_NIMP`, `CBL_NOT`, `CBL_TOUPPER`, `CBL_TOLOWER`, `CBL_OC_NANOSLEEP`, `CBL_OPEN_FILE`, `CBL_CREATE_FILE`, `CBL_READ_FILE`, `CBL_WRITE_FILE`, `CBL_CLOSE_FILE`, `CBL_FLUSH_FILE`, `CBL_DELETE_FILE`, `CBL_COPY_FILE`, `CBL_CHECK_FILE_EXIST`, `CBL_RENAME_FILE`, `CBL_GET_CURRENT_DIR`, `CBL_CHANGE_DIR`, `CBL_CREATE_DIR`, `CBL_DELETE_DIR`, `C$NARG`, `C$PARAMSIZE`, `C$CALLEDBY`, `C$JUSTIFY`, `C$SLEEP`, `C$TOUPPER`, `C$TOLOWER`, `C$MAKEDIR`, `C$DELETE`, `C$COPY`, `C$FILEINFO`.

### Appendix C: Ported Test Bundle Mapping Across All 56 Bundles (`2,527` Tests)

| Suite | Bundles (`tests/...`) | Primary Target Milestones |
| :--- | :--- | :--- |
| **`canary`** (2) | `canary/canary.bundle` | M0 (harness), M3 (`canary/scaffold-syntax` -> `PASS`) |
| **`nist85`** (426) | `SM.bundle` | M2 (`COPY`, `REPLACE`) |
| | `NC.bundle`, `OB.bundle`, `SG.bundle` | M4–M10 (Nucleus, obsolete elements, segmentation/`ALTER`/`PERFORM`) |
| | `IC.bundle` | M11 (`CALL`, `CANCEL`, subprograms) |
| | `IF.bundle` | M12 (COBOL-85 intrinsic functions) |
| | `DB.bundle` | M13 (`USE FOR DEBUGGING`, `DEBUG-ITEM`) |
| | `SQ.bundle`, `RL.bundle` | M14 (`SEQUENTIAL`, `LINAGE`, `RELATIVE`) |
| | `IX.bundle` | M15 (`INDEXED` ISAM B+Tree + `Capability::Isam`) |
| | `ST.bundle` | M16 (`SORT`, `MERGE`) |
| | `RW.bundle` | M18 (`REPORT SECTION` Report Writer) |
| **`gnucobol`** (1,556) | `configuration.bundle`, `listings.bundle`, `syn_copy.bundle` | M1–M2 (`-conf=`, `-E`, `-t`/`-T` listings, `COPY`/`REPLACE`/CDF) |
| | `syn_definition.bundle`, `syn_literals.bundle`, `syn_misc.bundle`, `syn_move.bundle`, `syn_multiply.bundle`, `syn_occurs.bundle`, `syn_redefines.bundle`, `syn_refmod.bundle`, `syn_set.bundle`, `syn_subscripts.bundle`, `syn_value.bundle`, `syn_file.bundle`, `syn_functions.bundle`, `syn_reportwriter.bundle`, `syn_screen.bundle`, `syn_oo.bundle` | M3 (`-fsyntax-only` parse & semantic diagnostics across all features) |
| | `run_returncode.bundle`, `run_fundamental.bundle`, `run_initialize.bundle`, `data_display.bundle` | M4–M5, M7–M8 (executables, `DISPLAY`, `MOVE`, `INITIALIZE`, edited `PIC`, control flow) |
| | `data_binary.bundle`, `data_packed.bundle` | M6 (binary `COMP`/`COMP-5`, packed `COMP-3`/`COMP-6`, exact math) |
| | `run_subscripts.bundle`, `run_refmod.bundle` | M9 (`OCCURS`/`ODO`, subscripts, reference modification, `SEARCH`) |
| | `run_accept.bundle`, `run_misc.bundle`, `backcomp.bundle` | M8–M10, M13 (`ACCEPT`, `STRING`/`UNSTRING`/`INSPECT`, legacy extensions, exceptions) |
| | `data_pointer.bundle`, `used_binaries.bundle`, `run_extensions.bundle`, `standalone_cob.bundle` | M11 (`POINTER`, `ALLOCATE`/`BASED`, `cobcrun`, `CALL`, `CBL_*`/`C$*`, C companion `.c`) |
| | `run_functions.bundle` | M12 (~105 intrinsics + `FUNCTION-ID` UDFs) |
| | `run_file.bundle` | M14–M16 (`SEQUENTIAL`, `LINE SEQUENTIAL`, `RELATIVE`, `INDEXED`, `SORT`/`MERGE`) |
| | `run_reportwriter.bundle` | M18 (`REPORT SECTION`) |
| | `run_manual_screen.bundle` | M19 (`SCREEN SECTION` + `Capability::CursesPty`) |
| | `run_ml.bundle` | M20 (`XML` & `JSON` + `Capability::Xml` / `Capability::Json`) |
| **`gcobol`** (543) | `top_level.bundle` | M2–M4 (DejaGnu `dg-error`/`dg-warning`, preprocess, basic compile/run) |
| | `group1.bundle` (16 foundational programs) | M4 (`display*`, `escape`), M6 (`comp5`, `packed`, `multiply2`, `simple-arithmetic`), M8 (`check_88`, `multiple-compares`, `perform-nested-exit`, `simple-if`, `simple-perform`), M11 (`pointer1`), M13 (`declarative_1`), M14 (`line-sequential`), M20 (`simple-classes`) |
| | `group2.bundle` (COBOLworx UAT & ISO 2023 regressions) | M5–M20 (37-digit decimals, 88 `FALSE IS`, `ACCEPT`, `ALLOCATE`, `ANY LENGTH`, `EC-*`, `EBCDIC`, `NATIONAL`/`UTF-8`, `FILE`, `SORT`, `INTRINSICS`, `UDF`, `OO`) |
| | `libgcobol.bundle` | M11 & M14 (POSIX runtime bindings & compatibility library routines) |

### Appendix D: Standard Per-Checkbox Iteration & Verification Loop

For every checkbox item implemented across Milestones 0–21:

- [ ] **Step 1 — Write Clean-Room Code & Exhaustive Unit Tests**:
  - [ ] Begin every `.rs` file with `//!`, document every item with `///` and `# Arguments` (for non-test functions with parameters), avoid `.unwrap()`/`.expect()`/`panic!`, and return `coblang_common::Result<T>`.
- [ ] **Step 2 — Targeted Conformance Run & Baseline Ratchet**:
  ```bash
  # Build compiler & shims
  cargo build --workspace --bins

  # Run targeted bundle(s) and inspect newly passing tests (XPASS)
  cargo run -p coblang-test-runner --bin coblang-test -- run 
    --bundle tests/ported/gcobol/group1.bundle

  # Snapshot updated baseline & regenerate scoreboards once XFAIL -> PASS transitions are verified
  cargo run -p coblang-test-runner --bin coblang-test -- run 
    --bundle-dir tests 
    --baseline tests/expectations/baseline.toml 
    --write-baseline tests/expectations/baseline.toml 
    --ratchet -j 8
  cargo run -p coblang-test-runner --bin coblang-test -- summary 
    --bundle-dir tests 
    --output tests/expectations/SCOREBOARD.md 
    --json tests/expectations/SCOREBOARD.json -j 8
  ```
- [ ] **Step 3 — Update `README.md` Shields & Execute All 7 Quality Gates**:
  ```bash
  python3 scripts/update-readme-shields.py --metric all
  ./scripts/check-quality.sh
  ```
