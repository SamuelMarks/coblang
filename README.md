# `coblang`

![doc coverage](https://img.shields.io/badge/doc%20coverage-100%25-brightgreen) ![test coverage](https://img.shields.io/badge/test%20coverage-100%25-brightgreen) ![license](https://img.shields.io/badge/license-Apache--2.0%20WITH%20LLVM--exception-blue)
[![CI & Release](https://github.com/SamuelMarks/coblang/actions/workflows/ci.yml/badge.svg)](https://github.com/SamuelMarks/coblang/actions/workflows/ci.yml)

`coblang` is a clean-room, memory-safe **MLIR 20 (`melior`) -> LLVM 20** COBOL compiler frontend and **pure-Rust `libcob` / `libgcobol` ABI-compatible runtime** (`libcoblang_rt`), driven monotonically by **2,527** conformance tests across **56 `.bundle` manifests** (`canary`, `nist85`, `gcobol`, `gnucobol`).

## Root Documentation & Academic Paper Artifacts

- **[`ARCHITECTURE.md`](ARCHITECTURE.md)** — End-to-end compiler & runtime architecture, 23-crate safety matrix (`20/23` `#![forbid(unsafe_code)]`), custom `cobol.*` MLIR 20 dialect, and lowering passes.
- **[`USAGE.md`](USAGE.md)** — Complete CLI reference (`coblang`, `cobc-shim`, `gcobol-shim`, `cobcrun-shim`), dialect profiles, runtime environment variables, and `coblang-test` harness guide.
- **[`FORMAL_SEMANTICS.md`](FORMAL_SEMANTICS.md)** — Mathematical models for Data Division `REDEFINES`/`ODO` byte layout, `SignedInt256` 38/76-digit decimal rounding, and `PERFORM ... THRU` / `ALTER` CFG semantics.
- **[`PAPER_OUTLINE.md`](PAPER_OUTLINE.md)** — Academic paper thesis, contributions (C1–C5), research questions (RQ1–RQ5), and comparison with `GnuCOBOL`, GCC `gcobol`, and IBM Enterprise COBOL.
- **[`EVALUATION_METHODOLOGY.md`](EVALUATION_METHODOLOGY.md)** — Reproducible experimental design, MLIR fast-path ablation benchmarks, TCB safety audit, and ACM Artifact Evaluation guide.
- **[`TODO_PLAN.md`](TODO_PLAN.md) & [`00_TODO_PLAN.md`](00_TODO_PLAN.md)** — Modular **26-file sequential/parallel DAG plan** (`00` through `09B`, including 10 conflict-free parallel Stage `06` tracks `06A`–`06J`) refactored from [`PLAN.md`](PLAN.md).

## Active Workspace Crates

| Crate | Path | Responsibility |
| :--- | :--- | :--- |
| `coblang-common` | `.` (`src/`) | Arena IDs, `SourceSpan`, validated domain bounds, unified `CoblangError`, `.bundle` manifest parser |
| `coblang-diag` | `crates/coblang-diag` | Case-folding `SymbolInterner`, expansion-aware `SourceMap`, warning engine, 11-dialect `.conf` loader |
| `coblang-pp` | `crates/coblang-pp` | Fixed/Free/Auto scanner, `COPY ... REPLACING`, `REPLACE`, ISO CDF (`>>IF`, `>>DEFINE`), `-t`/`-T` listings |
| `coblang-syntax` | `crates/coblang-syntax` | Context-sensitive lexer (`LexerMode::Picture`), 4-division AST, resilient recursive-descent & Pratt parser |
| `coblang-hir` | `crates/coblang-hir` | Canonical symbol-resolved `HirModule`, `HirType` (28 usages), and `HirPlace` |
| `coblang-sema` | `crates/coblang-sema` | Nested scopes (`GLOBAL`/`COMMON`), `PicEditOp` compiler, byte-exact `REDEFINES`/`ODO` layout, typechecker |
| `coblang-decimal` | `crates/coblang-decimal` | Zero-alloc `SignedInt256` (76-digit intermediate), 8 ISO 2023 rounding modes, Zoned/Packed/Binary/Float codecs |
| `coblang-pic` | `crates/coblang-pic` | `PictureProgram` edit/de-edit VM, category `MOVE` matrix, `INITIALIZE`, `STRING`, `UNSTRING`, `INSPECT` |
| `coblang-mlir` | `crates/coblang-mlir` | Custom `cobol.*` MLIR 20 dialect, verifier, `opt_fastpath` (`i64`/`i128`), `cfg_perform`, `llvm` lowering |
| `coblang-codegen` | `crates/coblang-codegen` | Audited LLVM 20 `TargetMachine` bridge (`.ll`, `.s`, `.o`) and native executable/module linker |
| `coblang-rt-core` | `crates/coblang-rt-core` | Pure-Rust runtime context, field descriptors, `DISPLAY`, extended `ACCEPT`, heap & exception state |
| `coblang-dynload` | `crates/coblang-dynload` | Audited POSIX `dlopen`/`dlsym` dynamic module resolver for `CALL`, `CANCEL`, and `cobcrun` |
| `coblang-rt-abi` | `crates/coblang-rt-abi` | Audited clean-room C-ABI library (`libcoblang_rt`: internal, `libcob`, `CBL_*`/`C$*`, and `libgcobol` shims) |
| `coblang-scaffold-driver` | `tools/scaffold-driver` | Multi-personality compiler & module runner CLIs (`coblang`, `cobc-shim`, `gcobol-shim`, `cobcrun-shim`) |
| `coblang-test-importer` | `tools/test-importer` | Clean-room importers for NIST CCVS-85 (`newcob.val`), GnuCOBOL (`.at`), and GCC COBOL (`cobol.dg`) |
| `coblang-test-runner` | `tools/test-runner` | Parallel `coblang-test` runner, diagnostic matcher, capability detector, and `baseline.toml` ratchet |

*(Stage `06` parallel runtime crates scheduled in `06B`–`06J`: `coblang-intrinsics`, `coblang-fileio`, `coblang-isam`, `coblang-sort`, `coblang-report`, `coblang-screen`, `coblang-xml-json`.)*

## Quick Start

```bash
# Run all 2,527 tests against the monotonic baseline ratchet
cargo run -p coblang-test-runner --bin coblang-test -- run --bundle-dir tests --baseline tests/expectations/baseline.toml --ratchet -j 8

# Regenerate the conformance scoreboard (currently 428 PASS / 1,895 XFAIL / 204 SKIP)
cargo run -p coblang-test-runner --bin coblang-test -- summary --bundle-dir tests --output tests/expectations/SCOREBOARD.md --json tests/expectations/SCOREBOARD.json -j 8

# Run all 7 quality gates (fmt, clippy, rustdoc, 100% 5-way docs, clean-room, 100% branch cov, ratchet)
./scripts/check-quality.sh
```

## Clean-Room Licensing

All compiler, runtime, and harness code in `src/`, `crates/`, and `tools/` is licensed under `Apache-2.0 WITH LLVM-exception`. See [`tests/LICENSE_MANIFEST.md`](tests/LICENSE_MANIFEST.md) for the SPDX licenses and strict clean-room isolation rules governing ported external test suites under `tests/ported/`.
