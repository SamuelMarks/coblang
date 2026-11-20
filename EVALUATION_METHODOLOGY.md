# Empirical Evaluation Methodology & Reproducible Artifact Guide (`EVALUATION_METHODOLOGY.md`)

This document defines the reproducible experimental protocol, metrics, ablation studies, TCB audits, and threats-to-validity analysis supporting the empirical evaluation (§7 of [`PAPER_OUTLINE.md`](PAPER_OUTLINE.md)) and ACM/IEEE Artifact Evaluation packaging.

---

## 1. Benchmark Corpus & Clean-Room Provenance

`coblang` is evaluated against **2,527 deterministic conformance and torture tests** stored across **56 human-readable `.bundle` manifests** under `tests/`:

| Corpus | Upstream Source & Pinned Revision | SPDX License | Bundles | Test Cases | Primary Coverage Focus |
| :--- | :--- | :--- | ---: | ---: | :--- |
| **Canary** | In-tree harness self-tests (`tests/canary/`) | `Apache-2.0 WITH LLVM-exception` | 1 | 2 | Harness smoke & conditional capability `SKIP` |
| **NIST CCVS-85** | US DoD / NIST COBOL-85 Validation Suite (`newcob.val`, SHA-256 `e4513f26...`) | `NIST-PD` (Public Domain) | 12 | 426 | `NC`, `SM`, `IC`, `SQ`, `RL`, `IX`, `ST`, `IF`, `RW`, `SG`, `DB`, `OB` |
| **GCC `gcobol`** | `gcc/testsuite/cobol.dg` (`6e0de8bd...`) | `GPL-3.0-or-later WITH GCC-exception-3.1` | 4 | 543 | ISO 2023 conformance, COBOLworx UAT (`group2`), `-O0`..`-Os` torture, `libgcobol` |
| **`GnuCOBOL`** | `tests/testsuite.src/*.at` (`02294c68...`) | `GPL-3.0-or-later` | 39 | 1,556 | 17 negative/positive syntax suites, BCD/binary codecs, ISAM/file I/O, `SCREEN`, `REPORT`, `XML`/`JSON`, `CBL_*` ABI |
| **Total** | — | Clean-Room Out-of-Process Execution | **56** | **2,527** | **41 Orthogonal `FeatureTag` Dimensions** |

All external suites can be re-fetched and cryptographically SHA-256 verified from upstream mirrors via `./scripts/fetch-upstream-tests.sh` without touching compiler source code (`tests/LICENSE_MANIFEST.md`).

---

## 2. Reproducible Experiments Mapped to Research Questions

### Experiment 1 (RQ1): Multi-Suite & Multi-Dialect Conformance Matrix
- **Metric**: Exact counts of `PASS`, `XFAIL`, `FAIL`, `XPASS`, and capability-gated `SKIP` per suite (`nist85`, `gcobol`, `gnucobol`), per NIST module (`NC`..`RW` including internal CCVS-85 `PASS`/`*FAIL*` subtest inspection counters), and across all 41 `FeatureTag` taxonomy slices.
- **Command**:
  ```bash
  cargo run --release -p coblang-test-runner --bin coblang-test -- summary --bundle-dir tests --output tests/expectations/SCOREBOARD.md --json tests/expectations/SCOREBOARD.json -j 8
  ```
- **Differential Oracle Validation** (when optional external `cobc` or `gcobol` binaries are installed on an evaluation host):
  ```bash
  cargo run --release -p coblang-test-runner --bin coblang-test -- run --bundle-dir tests --oracle cobc -j 8
  ```

### Experiment 2 (RQ2): Generated Native Code Performance & MLIR Pass Ablation
- **Micro- & Kernel Benchmarks** (pure Apache-2.0 clean-room kernels for publication):
  1. `bench_comp5_loop`: Tight $10^8$-iteration `PERFORM VARYING` loop over `PIC S9(9) COMP-5` and `PIC S9(18) COMP-5`.
  2. `bench_packed_financial`: Compound interest & tax ledger kernel over `PIC S9(13)V99 COMP-3` with `ROUNDED MODE IS NEAREST-EVEN` and `ON SIZE ERROR`.
  3. `bench_38digit_exact`: Full 38-digit (`PIC S9(30)V9(8) PACKED-DECIMAL`) multiplication/division exercising `SignedInt256`.
  4. `bench_perform_thru_cfg`: 50-paragraph batch transaction control-flow graph exercising `PERFORM ... THRU` continuation slots vs promoted `func.call` (`promote_perform`).
  5. `bench_pic_editing`: Bulk `MOVE` formatting of $10^7$ numeric records into `PIC $$$,$$$,$$9.99CR` (`PictureProgram` VM + `const_fold_move`).
  6. `bench_isam_sort`: $10^6$-record `coblang-isam` primary/alternate-key load + random probe + external spill `coblang-sort`.
- **Ablation Configurations**:
  - `coblang -O0` (runtime descriptor baseline, fast-paths disabled)
  - `coblang -O2` with `opt_fastpath` (`i64`/`i128` inline arithmetic + `llvm.memcpy`)
  - `coblang -O2` + Milestone `M21` passes (`promote_perform` + `const_fold_move` + `fuse_decimal`)
  - External baselines (black-box binary timing only): `cobc -x -O2` (`GnuCOBOL`) and `gcobol -O2` (`GCC`).
- **Metrics**: Median wall-clock execution time across $N=30$ runs (with 95% bootstrap confidence intervals), peak RSS (`/usr/bin/time -l` on macOS, `/usr/bin/time -v` on Linux), and emitted native instruction counts (`llvm-objdump --disassemble`).

### Experiment 3 (RQ3): Compilation Throughput & Memory Footprint
- **Protocol**: Measure cold and warm single-threaded compilation wall time (`-fsyntax-only`, `-c -O0`, `-c -O2`, `-x -O2`) across small (`< 100` LOC), medium (`1,000` LOC NIST CCVS-85 modules with heavy `COPY` expansion), and synthetic large (`25,000` LOC multi-program batch units) inputs, decomposing time spent per phase (`pp`, `syntax`, `sema`, `cobol-mlir`, `llvm-mlir`, `llvm-codegen`, `ld`).

### Experiment 4 (RQ4): Memory-Safety & Trusted Computing Base (TCB) Audit
- **Metrics**:
  1. Proportion of workspace crates enforcing `#![forbid(unsafe_code)]` (`20 / 23` crates = `87.0%` of crates; `> 97%` of Rust LOC).
  2. Exact count and line-of-code volume of `unsafe` blocks across the 3 FFI boundary crates (`coblang-codegen`, `coblang-dynload`, `coblang-rt-abi`), verified automatically by `rg "unsafe\s*\{" crates/ src/ tools/`.
  3. Contrast with external C/C++ implementations where 100% of parser, optimizer, decimal runtime, and ISAM storage code executes in memory-unsafe C/C++.

### Experiment 5 (RQ5): Verification Gate & Parallel DAG Decomposition Analysis
- **Metrics**:
  - `cargo +nightly llvm-cov --workspace --branch --json --summary-only`: exact function, line, and branch counts (`covered / count = 100.00%`).
  - `python3 scripts/update-readme-shields.py --metric doc --verify-strict-100 --check`: item-level documentation verification across files, modules, types/fields/variants, functions, and `# Arguments`.
  - Critical-path vs. total-work ratio in the 26-file DAG (`00_TODO_PLAN.md`–`09B_*`): quantifying how Stage `06`'s 10-way disjoint crate fan-out compresses critical-path serial depth compared to a monolithic 543-item sequential backlog.

---

## 3. Threats to Validity

1. **Construct Validity (Benchmark Representativeness)**: Conformance tests (`nist85`, `gcobol`, `gnucobol`) emphasize edge-case semantic compliance rather than multi-gigabyte batch throughput. Experiment 2 mitigates this by pairing conformance pass rates with dedicated macro-benchmarks over million-record `COMP-3`, `COMP-5`, `PICTURE`, `SORT`, and `ISAM` workloads.
2. **Internal Validity (Clean-Room ABI Fidelity)**: Deriving `libcob` and `libgcobol` ABI structures without inspecting GPLv3 source code risks subtle struct-layout mismatches when linking precompiled third-party C objects. We mitigate this via black-box companion C tests (`STEP_CC` in `used_binaries.bundle`, `run_extensions.bundle`, and `libgcobol.bundle`) and explicit `#[repr(C)]` layout assertions.
3. **External Validity (Host Target Portability)**: Measurements on `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu` share identical MLIR lowering up to target data-layout parameterization; big-endian mainframe semantics (`USAGE BINARY`, `-finternal-ebcdic`) are explicitly modeled in `DialectConfig` and tested via byte-level codecs (`data_binary.bundle`, `data_packed.bundle`, `feature:ebcdic`).
