# `coblang` Compiler & Clean-Room Runtime Architecture

`coblang` is a memory-safe, multi-dialect **COBOL -> MLIR 20 (`melior`) -> LLVM 20** native compiler and **100% clean-room pure-Rust runtime** (`libcoblang_rt`) licensed under `Apache-2.0 WITH LLVM-exception`. It is engineered to compile legacy ANSI X3.23-1985 (`COBOL-85`), ISO/IEC 1989:2002/2014/2023, IBM Enterprise, Micro Focus, and GnuCOBOL source programs directly to native object files, shared modules, and standalone executables while maintaining binary C-ABI compatibility with both `libcob` (`GnuCOBOL`) and `libgcobol` (`GCC COBOL`) companion ecosystems.

---

## 1. Architectural Design Principles

1. **Domain-Specific MLIR Lowering Over C Transpilation or Low-Level AST Codegen**:
   Unlike `GnuCOBOL` (`cobc`), which transpiles COBOL into C source code, or GCC `gcobol`, which lowers directly from parser actions into low-level `GENERIC` trees, `coblang` introduces two explicit, strongly-typed intermediate representations:
   - **`coblang-hir`**: A symbol-resolved, storage-layout-solved, desugared High-Level IR (`HirModule`).
   - **`cobol.*` MLIR 20 Dialect (`coblang-mlir`)**: A domain-specific MLIR dialect preserving high-level COBOL storage views (`cobol.field_view`, `cobol.subscript`, `cobol.refmod`), fixed-point arithmetic (`cobol.arith`, `cobol.div_rem`), data movement (`cobol.move`, `cobol.init`), and non-structured paragraph control flow (`cobol.perform_range`, `cobol.alter`) prior to progressive lowering into `scf`, `arith`, `cf`, `func`, and `llvm` dialects.
2. **Pervasive Memory Safety & Minimal Trusted Computing Base (TCB)**:
   Out of **23 workspace crates** (`16` active on `HEAD`, `7` decoupled runtime crates in Stage `06`), **20 crates enforce `#![forbid(unsafe_code)]`**, including the entire lexer, parser, preprocessor, semantic analyzer, MLIR dialect builders, 256-bit decimal engine, `PICTURE` bytecode VM, file I/O, B+Tree ISAM engine, sort/merge engine, report writer, TUI screen engine, and XML/JSON processors. `unsafe` is confined to **3 thin, audited FFI boundary crates** (`coblang-codegen`, `coblang-dynload`, `coblang-rt-abi`) under `unsafe_code = "deny"` and `undocumented_unsafe_blocks = "deny"`.
3. **Compile-Time Typestate & Invariant-Preserving Domain Types**:
   Raw primitive obsession (`usize`, `u32`, `&str`, `bool` flags) is forbidden across module boundaries (`fn_params_excessive_bools = "deny"`). Domain bounds (`LevelNumber`, `DigitCount`, `ScaleFactor`, `LineNumber`, `FileStatusCode`) are validated once at smart-constructor boundaries, and compilation stages (`VerifiedCobolMlirModule<'ctx>` -> `LoweredLlvmMlirModule<'ctx>` -> `OwnedLlvmModule`) are distinct Rust typestates.
4. **Total Functions & Unified Error Algebra**:
   `unwrap_used = "forbid"`, `expect_used = "forbid"`, `panic = "deny"`, and `indexing_slicing = "deny"` are enforced across all library, binary, and `#[test]` targets. Every fallible operation returns `coblang_common::Result<T>` backed by a single workspace-wide `CoblangError` enum (`derive_more::{Display, Error, From}`).
5. **Strict GPLv3 Clean-Room Isolation**:
   All compiler and runtime logic is implemented from ISO/IEC 1989:2023 specifications, public C-ABI documentation, and black-box behavioral test manifests (`tests/LICENSE_MANIFEST.md`). External test suites (`tests/ported/`) are consumed strictly out-of-process as passive `.bundle` inputs by `coblang-test`.

---

## 2. End-to-End Compilation & Execution Pipeline

```text
  COBOL Source (.cob / .cbl) + Copybooks (.cpy) + Dialect Profile (.conf)
                                    │
                                    ▼
 ┌─────────────────────────────────────────────────────────────────────────┐
 │ 1. coblang-diag + coblang-pp (Reference Format & Two-Phase Text Engine) │
 │  • Fixed (cols 1-6, 7, 8-11, 12-72) / Free / Auto inline >>SOURCE switch│
 │  • Phase 1: COPY ... REPLACING (LEADING/TRAILING/pseudo-text ==...==)   │
 │  • Phase 2: REPLACE / REPLACE ALSO / CDF (>>DEFINE, >>IF, >>EVALUATE)   │
 │  • Emits PpToken stream + ExpansionId provenance chain + -E / -t / -T   │
 └──────────────────────────────────┬──────────────────────────────────────┘
                                    │
                                    ▼
 ┌─────────────────────────────────────────────────────────────────────────┐
 │ 2. coblang-syntax (Context-Sensitive Lexer & 4-Division Pratt Parser)   │
 │  • LexerMode::{Normal, Picture, IdCommentParagraph} period disambig.    │
 │  • Dialect soft/hard reserved words + 10 string literal prefix codecs   │
 │  • Resilient recursive-descent + abbreviated relation Pratt parser      │
 └──────────────────────────────────┬──────────────────────────────────────┘
                                    │
                                    ▼
 ┌─────────────────────────────────────────────────────────────────────────┐
 │ 3. coblang-sema -> coblang-hir (Symbols, Layout, & HIR Desugaring)      │
 │  • Nested scopes (GLOBAL / IS COMMON) & OF/IN qualification resolver    │
 │  • PICTURE parser -> PicEditOp bytecode compiler                        │
 │  • Byte-exact 01..49/66/77/78/88 layout, SYNCHRONIZED, REDEFINES, ODO   │
 │  • Desugars CORRESPONDING, 88 conditions, & 78 constants -> HirModule   │
 └──────────────────────────────────┬──────────────────────────────────────┘
                                    │
                                    ▼
 ┌─────────────────────────────────────────────────────────────────────────┐
 │ 4. coblang-mlir (Custom `cobol.*` MLIR 20 Dialect & Lowering Passes)    │
 │  • HIR -> VerifiedCobolMlirModule<'ctx> (strict custom op verifier)     │
 │  • opt_fastpath: inline i64/i128 + overflow intrinsics & llvm.memcpy    │
 │  • cfg_perform: zero-heap continuation slots & hybrid stack for PERFORM │
 │  • PassManager (-O0..-Os) -> LoweredLlvmMlirModule<'ctx>                │
 └──────────────────────────────────┬──────────────────────────────────────┘
                                    │
                                    ▼
 ┌─────────────────────────────────────────────────────────────────────────┐
 │ 5. coblang-codegen (Audited LLVM 20 TargetMachine & System Linker)      │
 │  • mlirTranslateModuleToLLVMIR -> OwnedLlvmModule                       │
 │  • Emits .ll (-emit-llvm), .s (-S), .o (-c), .so/.dylib (-m), exe (-x)  │
 └──────────────────────────────────┬──────────────────────────────────────┘
                                    │
                                    ▼
 ┌─────────────────────────────────────────────────────────────────────────┐
 │ 6. Pure-Rust Runtime & Dual C-ABI Surface (libcoblang_rt.{a,so,dylib})  │
 │  • Safe Core: coblang-{decimal, pic, rt-core, intrinsics, fileio,       │
 │               isam, sort, report, screen, xml-json}                     │
 │  • Audited FFI: coblang-dynload (dlopen/cobcrun) + coblang-rt-abi       │
 │                 (cob_*, CBL_*, C$*, and libgcobol __gg__* symbols)      │
 └─────────────────────────────────────────────────────────────────────────┘
```

---

## 3. Workspace Crate Topology & Safety Classification

| Crate | Path | Safety Policy | Layer & Primary Responsibility | Status |
| :--- | :--- | :--- | :--- | :--- |
| `coblang-common` | `.` (`src/`) | `forbid(unsafe_code)` | Arena IDs (`ids.rs`), `SourceSpan` (`span.rs`), validated bounds (`bounds.rs`), `CoblangError`, bundle manifest schema | Active (`M0`) |
| `coblang-diag` | `crates/coblang-diag` | `forbid(unsafe_code)` | Case-folding `SymbolInterner`, `SourceMap` expansion unwinding, `WarningPolicy`, `DiagnosticSink`, 11-dialect `.conf` loader | Active (`M1`) |
| `coblang-pp` | `crates/coblang-pp` | `forbid(unsafe_code)` | Fixed/Free/Auto scanner, literal continuations, `COPY ... REPLACING`, `REPLACE`, ISO CDF (`>>IF`, `>>DEFINE`, `>>TURN`), `-t`/`-T` listings | Active (`M2`) |
| `coblang-syntax` | `crates/coblang-syntax` | `forbid(unsafe_code)` | Multi-mode lexer (`LexerMode::Picture`), typed 4-division AST (`Program`, `Function`, `Class`, `Interface`), resilient parser | Active (`M3`) |
| `coblang-hir` | `crates/coblang-hir` | `forbid(unsafe_code)` | Canonical `HirModule`, `HirType` (28 usages), `HirPlace` (static offset + ODO strides + refmod), `HirStmt`, `HirExpr` | Active (`M3`) |
| `coblang-sema` | `crates/coblang-sema` | `forbid(unsafe_code)` | Lexical/nested `SymbolTable`, `PicEditOp` compiler, byte-exact `REDEFINES`/`RENAMES`/`ODO` layout solver, category typechecker, HIR lowerer | Active (`M3`) |
| `coblang-mlir` | `crates/coblang-mlir` | `forbid(unsafe_code)` | Safe `melior` `cobol.*` dialect ops, custom IR verifier, `opt_fastpath`, `cfg_perform` CFG builder, `llvm` dialect lowering | Active (`M4–M11`) |
| `coblang-codegen` | `crates/coblang-codegen` | **Audited `deny`** | RAII `OwnedLlvmModule` (`llvm-sys` 201), LLVM 20 `TargetMachine` `.ll`/`.s`/`.o` emission, safe `linker.rs` driver | Active (`M4`) |
| `coblang-decimal` | `crates/coblang-decimal` | `forbid(unsafe_code)` | Stack-allocated `SignedInt256` (76-digit intermediate), `FixedDecimal`, 8 ISO 2023 `RoundingMode`s, Zoned/Packed/Binary/Float/BID128 codecs | Active (`M5–M6`) |
| `coblang-pic` | `crates/coblang-pic` | `forbid(unsafe_code)` | `PictureProgram` VM (numeric/alpha edit & de-edit), category `MOVE` matrix, `INITIALIZE`, `STRING`, `UNSTRING`, `INSPECT` | Active (`M5–M10`) |
| `coblang-rt-core` | `crates/coblang-rt-core` | `forbid(unsafe_code)` | Process/thread runtime state, field descriptor codecs, `DISPLAY`/`ACCEPT` environment/date/args, heap registry, `EC-*` exceptions | Active (`M4–M13`) |
| `coblang-dynload` | `crates/coblang-dynload` | **Audited `deny`** | Thin POSIX `dlopen`/`dlsym` loader, clean-room COBOL identifier mangling, `COB_LIBRARY_PATH` search, `CANCEL` lifecycle | Active (`M11`) |
| `coblang-rt-abi` | `crates/coblang-rt-abi` | **Audited `deny`** | `#[repr(C)]` `libcoblang_rt` static/shared library exporting internal `coblang_rt_*`, `libcob` `cob_*`, `CBL_*`/`C$*`, and `libgcobol` `__gg__*` ABIs | Active (`M4–M11`) |
| `coblang-intrinsics` | `crates/coblang-intrinsics` | `forbid(unsafe_code)` | All ~105 ISO 2023 / GnuCOBOL / `gcobol` math, financial, statistical, Lilian/ISO-8601 datetime, string, & Unicode intrinsics | Stage `06B` |
| `coblang-fileio` | `crates/coblang-fileio` | `forbid(unsafe_code)` | `DD_*` path resolution, `FileStatusCode` state machine, `SEQUENTIAL`, `LINAGE`, `LINE SEQUENTIAL`, and `RELATIVE` file engines | Stage `06D` |
| `coblang-isam` | `crates/coblang-isam` | `forbid(unsafe_code)` | Pure-Rust 4096-byte CRC32 paged Order-$B$ B+Tree `INDEXED` engine with primary + FIFO duplicate alternate/split keys | Stage `06E` |
| `coblang-sort` | `crates/coblang-sort` | `forbid(unsafe_code)` | Multi-key algebraic/collating `SortComparator` and bounded-memory k-way external spill-merge `SortSession` | Stage `06F` |
| `coblang-report` | `crates/coblang-report` | `forbid(unsafe_code)` | Declarative `REPORT SECTION` (`RD`) page-zone layout, hierarchical control breaks, `SUM ... RESET ON`, `GROUP INDICATE` | Stage `06H` |
| `coblang-screen` | `crates/coblang-screen` | `forbid(unsafe_code)` | `SCREEN SECTION` field model, `AnsiVt100Backend` interactive TUI, and deterministic 24x80 `VirtualGridBackend` | Stage `06I` |
| `coblang-xml-json` | `crates/coblang-xml-json` | `forbid(unsafe_code)` | Event-driven `XML PARSE`, `XML GENERATE`, `JSON PARSE`, and `JSON GENERATE` | Stage `06J` |
| `coblang-scaffold-driver` | `tools/scaffold-driver` | `forbid(unsafe_code)` | Multi-personality CLI drivers (`coblang`, `cobc-shim`, `gcobol-shim`, `cobcrun-shim`) orchestrating pipeline stages | Active (`M0–M21`) |
| `coblang-test-importer` | `tools/test-importer` | `forbid(unsafe_code)` | Clean-room parsers converting NIST CCVS-85 (`newcob.val`), GnuCOBOL (`.at`), and GCC (`cobol.dg`) into `.bundle` manifests | Active (`M0`) |
| `coblang-test-runner` | `tools/test-runner` | `forbid(unsafe_code)` | Parallel `coblang-test` execution engine, GCC/cobc diagnostic matcher, capability detector, and `baseline.toml` ratchet | Active (`M0–M1`) |

---

## 4. Key Compiler & Runtime Subsystems

### 4.1 Provenance-Preserving Two-Phase Preprocessing (`coblang-diag` & `coblang-pp`)
COBOL's Source Text Manipulation facility mandates two distinct rewrite phases with column-sensitive margin rules:
1. **Reference-Format Normalization (`format.rs`)**: Expands tabs against configurable `TabWidth` while maintaining an exact inverse map to raw `ByteOffset`s in `SourceFile`. Handles Fixed-format sequence numbers (cols 1–6), indicator column 7 (`*`, `/`, `D`, `-`), Area A (8–11), Area B (12–72), Free-format (`*>`), and mid-file `>>SOURCE FORMAT IS FREE|FIXED` switching.
2. **Phase 1 (`copy.rs` & `pseudo_text.rs`)**: Resolves `COPY name [IN/OF lib] [REPLACING ...]` recursively (detecting inclusion cycles) and performs token-level ANSI-85 pseudo-text (`==...==`) as well as COBOL-2002 partial-word (`LEADING` / `TRAILING`) substitution.
3. **Phase 2 (`replace.rs` & `cdf.rs`)**: Evaluates `>>DEFINE`, `>>IF`/`>>ELIF`/`>>ELSE`, `>>EVALUATE`, and `>>TURN` directives alongside stacked `REPLACE [ALSO] ...` tables. Every substituted token receives an `ExpansionId` linked in `SourceMap`, enabling full multi-level `note: in expansion of COPY ...` diagnostic backtraces.

### 4.2 Context-Sensitive Lexing, `PICTURE` Bytecode, & Storage Layout (`coblang-syntax` & `coblang-sema`)
- **Period Disambiguation (`lexer.rs`)**: In COBOL, `.` inside `PIC S9(4).99.` is both an insertion character and a clause terminator. Triggering `LexerMode::Picture` immediately after `PIC` / `PICTURE [IS]` consumes the picture specification greedily while preserving a trailing separator `.` as `TokenKind::Period`.
- **Abbreviated Combined Relation Conditions (`expr_cond.rs`)**: ANSI-85 expressions such as `IF A = B OR C AND NOT > D` distribute the implied subject (`A`) and implied relational operator (`=`) across logical connectives while respecting `NOT` ambiguity rules (distinguishing `NOT > D` relational negation from `NOT (cond)` boolean negation).
- **Zero-Copy Aliasing & Dynamic `ODO` (`layout.rs`)**: `REDEFINES` and `66 RENAMES THRU` are solved statically into byte-offset views over their root `01` record allocation, eliminating data copying. Variable-length `OCCURS min TO max TIMES DEPENDING ON odo` groups record static prefix offsets paired with dynamic runtime stride expressions in `HirPlace`.

### 4.3 Custom `cobol.*` MLIR Dialect, Fast-Path Specialization, & `PERFORM` CFG Lowering (`coblang-mlir`)
- **Dual-Path Arithmetic & `MOVE` Specialization (`opt_fastpath.rs`)**:
  - Fixed-length alphanumeric `MOVE`s with static lengths lower directly to `llvm.memcpy` and `llvm.memset` padding.
  - Arithmetic on `COMP-5`, `BINARY`, and fixed-scale `PACKED-DECIMAL` items with $\le 37$ decimal digits lowers inline to native `i64`/`i128` SSA operations (`arith.addi`, `arith.subi`, `arith.muli`, `arith.divsi`) plus `@llvm.sadd.with.overflow` / `@llvm.smul.with.overflow` intrinsics when `ON SIZE ERROR` or `EC-SIZE-OVERFLOW` is active — avoiding runtime descriptor overhead on hot loops while delegating arbitrary 38-digit/76-intermediate operations seamlessly to `coblang-decimal`.
- **Non-Structured `PERFORM ... THRU` & `ALTER` CFG (`cfg_perform.rs`)**:
  COBOL paragraphs execute sequentially by fallthrough when not intercepted by an active `PERFORM p1 THRU p2` range end, while `ALTER p1 TO PROCEED TO p2` dynamically mutates unconditional `GO TO` paragraphs at runtime. `cfg_perform.rs` lowers each paragraph as a basic-block subgraph ending in an epilogue dispatcher driven by zero-heap-allocation `PerformContinuationSlot` state variables (augmented by a bounded `PerformStack` under `PerformReturnStyle::IsoStack` for recursive/overlapping ranges).

### 4.4 Monotonic Ratchet Verification Architecture (`coblang-test`)
All 2,527 conformance tests across 56 `.bundle` archives (`canary`, `nist85`, `gcobol`, `gnucobol`) execute continuously on every commit:
- Unimplemented compiler constructs return `CoblangError::UnimplementedFeature`, which maps deterministically to process exit code `78` (`CobolExitCode::UNIMPLEMENTED`) and harness outcome `XFAIL`.
- Passing `--ratchet` to `coblang-test` enforces strict monotonicity: any `PASS -> FAIL` or `PASS -> XFAIL` regression fails CI immediately, while newly implemented features (`XFAIL -> PASS`, reported as `XPASS`) atomically advance `tests/expectations/baseline.toml` and `SCOREBOARD.md`.
