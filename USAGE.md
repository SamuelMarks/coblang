# `coblang` Usage, CLI Reference, & Developer Guide

This manual documents building `coblang`, invoking its four compiler/runtime driver personalities (`coblang`, `cobc-shim`, `gcobol-shim`, `cobcrun-shim`), configuring dialects and runtime environment variables, running the 2,527-test conformance suite (`coblang-test`), and executing parallel development workflows.

---

## 1. Host Prerequisites & Environment Setup

`coblang` targets **LLVM 20** and **MLIR 20** via `melior` (`0.27`), `mlir-sys` (`200`), and `llvm-sys` (`201`).

### macOS (Homebrew)
```bash
brew install llvm@20 zstd
export MLIR_SYS_200_PREFIX="$(brew --prefix llvm@20)"
export LLVM_SYS_201_PREFIX="$(brew --prefix llvm@20)"
export TABLEGEN_200_PREFIX="$(brew --prefix llvm@20)"
```

### Ubuntu / Debian (`apt.llvm.org`)
```bash
sudo apt-get update
sudo apt-get install -y llvm-20-dev libmlir-20-dev mlir-20-tools clang-20 lld-20 zlib1g-dev libzstd-dev
export MLIR_SYS_200_PREFIX=/usr/lib/llvm-20
export LLVM_SYS_201_PREFIX=/usr/lib/llvm-20
export TABLEGEN_200_PREFIX=/usr/lib/llvm-20
```

> **Note**: `./scripts/check-quality.sh` automatically detects `/opt/homebrew/opt/llvm@20`, `/usr/local/opt/llvm@20`, and `/usr/lib/llvm-20` when these environment variables are unset.

### Building All Compiler, Shim, Runtime, & Test Binaries
```bash
cargo build --workspace --bins
cargo build -p coblang-rt-abi
```

---

## 2. Compiler Driver Personalities

`tools/scaffold-driver` produces four binaries sharing the unified `coblang` compilation pipeline while matching upstream CLI conventions:

| Binary | Default Mode | Primary Use Case |
| :--- | :--- | :--- |
| `coblang` | `-m` (shared module unless `-x`/`-c`/`-S`/`-E` given) | Primary native COBOL compiler CLI with MLIR/LLVM inspection flags |
| `cobc-shim` | `-m` (shared module) | Drop-in `GnuCOBOL` `cobc` CLI compatibility for build scripts & Autotest suites |
| `gcobol-shim` | `-x` (executable) | Drop-in GCC `gcobol` CLI compatibility for `gcc/testsuite/cobol.dg` & DejaGnu workflows |
| `cobcrun-shim` | Dynamic module runner | Loads and executes compiled `-m` / `-shared` COBOL modules (`cobcrun [-M <dir>] PROG [args...]`) |

---

## 3. Quick-Start Compilation Examples

### Compile & Run a Standalone Native Executable (`-x`)
```bash
cargo run -p coblang-scaffold-driver --bin coblang -- -x -O2 -free hello.cob -o hello
./hello
```

### Inspect Intermediate Representations (`-E`, `--emit-mlir`, `--emit-mlir-llvm`, `-emit-llvm`, `-S`)
```bash
# 1. Normalized preprocessed COBOL source (after COPY / REPLACE / CDF)
coblang -E -I ./copybooks payroll.cbl -o payroll.i

# 2. High-level verified `cobol.*` MLIR 20 dialect
coblang --emit-mlir payroll.cbl -o payroll.cobol.mlir

# 3. Lowered `llvm.*` MLIR 20 dialect (after opt_fastpath & cfg_perform passes)
coblang --emit-mlir-llvm -O2 payroll.cbl -o payroll.llvm.mlir

# 4. LLVM 20 textual IR (.ll) and target assembly (.s)
coblang -emit-llvm -O2 payroll.cbl -o payroll.ll
coblang -S -O2 payroll.cbl -o payroll.s
```

### Compile Dynamically Loadable Module (`-m`) & Execute via `cobcrun-shim`
```bash
coblang -m subprog.cob -o subprog.so
cobcrun-shim -M . SUBPROG arg1 arg2
```

### Generate Narrow (`-t`) or Wide (`-T`) Compiler Source & Symbol Listings
```bash
coblang -fsyntax-only -t report_narrow.lst -T report_wide.lst --tlines=60 ledger.cob
```

---

## 4. Complete CLI Flag Reference

### 4.1 Pipeline Stage & Output Mode Flags
| Flag | `CompilationMode` | Description |
| :--- | :--- | :--- |
| `-E` | `PreprocessOnly` | Preprocess `COPY`, `REPLACE`, and CDF directives to stdout or `-o <file>` |
| `-fsyntax-only` | `SyntaxOnly` | Run preprocessor, lexer, parser, symbol resolution, layout, and typecheck only |
| `--emit-mlir` | `EmitLlvmIr` (`MlirCobol`) | Emit verified custom `cobol.*` MLIR 20 module text |
| `--emit-mlir-llvm` | `EmitLlvmIr` (`MlirLlvm`) | Emit lowered `llvm.*` MLIR 20 module text |
| `-emit-llvm` | `EmitLlvmIr` (`LlvmIr`) | Emit LLVM 20 IR (`.ll`) |
| `-S` | `EmitAssembly` | Emit native target assembly (`.s`) |
| `-c` | `CompileObject` | Emit native relocatable object file (`.o`) without linking |
| `-m`, `-b`, `-shared` | `CompileModule` | Build dynamically loadable COBOL shared module (`.so` / `.dylib`) |
| `-x` | `BuildExecutable` | Build standalone native executable with `@main` lifecycle wrapper |
| `-o <path>` | — | Write primary output artifact to `<path>` |

### 4.2 Dialect & Configuration Flags
| Flag | Description |
| :--- | :--- |
| `-std=default` | Default `coblang` / GnuCOBOL-compatible permissive mode |
| `-std=cobol85` | Strict ANSI X3.23-1985 / ISO 1989:1985 (`nist85` profile) |
| `-std=cobol2002` | ISO/IEC 1989:2002 standard profile |
| `-std=cobol2014` | ISO/IEC 1989:2014 standard profile |
| `-std=cobol2023`, `-dialect iso` | ISO/IEC 1989:2023 standard profile |
| `-std=gnu`, `-dialect gnu` | GnuCOBOL extended dialect |
| `-std=ibm`, `-std=mvs`, `-std=ibm-strict`, `-dialect ibm` | IBM Enterprise COBOL compatibility profile |
| `-std=mf`, `-std=mf-strict`, `-dialect mf` | Micro Focus COBOL compatibility profile |
| `-std=bs2000`, `-std=bs2000-strict` | Siemens BS2000 COBOL profile |
| `-std=acu`, `-std=acu-strict` | ACUCOBOL-GT compatibility profile |
| `-std=xopen` | X/Open CAE COBOL specification profile |
| `-conf=<path>` | Load clean-room `.conf` dialect definition file (supports `include "..."`) |
| `-cb_conf=<key>=<val>` | Override individual `.conf` key from the command line (e.g., `-cb_conf=binary-byteorder=native`) |

### 4.3 Source Format, Preprocessor, Diagnostics, Optimization, & Linking
| Flag | Description |
| :--- | :--- |
| `-fixed`, `-fformat=fixed` | Force 80-column ANSI Fixed Reference Format (cols 1–6 seq, col 7 indicator, cols 8–72 program text) |
| `-free`, `-F`, `-fformat=free` | Force Free Reference Format (`*>` inline comments, unlimited line width) |
| `-fformat=auto` | Auto-detect format per file and honor inline `>>SOURCE FORMAT` directives |
| `-I <dir>`, `-I<dir>` | Append `<dir>` to copybook search path |
| `-copyext <ext>`, `-ext <ext>` | Register additional copybook file extension |
| `-D<name>[=<val>]`, `-D <name>[=<val>]` | Define CDF compilation variable for `>>IF` / `>>EVALUATE` |
| `-t <file>`, `-T <file>`, `--tlines=<n>` | Write 80-column (`-t`) or 120-column (`-T`) compiler listing with `<n>` lines/page |
| `-w`, `-Wall`, `-Wextra`, `-Werror` | Suppress all warnings, enable standard/extra warning groups, or promote warnings to hard errors |
| `-W<group>`, `-Wno-<group>`, `-Werror=<group>` | Configure individual `WarningGroup` (`archaic`, `obsolete`, `redefinition`, `truncate`, `strict-typing`, `implicit-define`, `parentheses`, `terminator`, `sequence`, `column-overflow`, `call-params`, `dialect`, `unreachable`, `unused`) |
| `-fmax-errors=<n>` | Abort diagnostic emission after `<n>` hard errors |
| `-debug`, `-fdebugging-line` | Compile column-7 `'D'` debugging lines and enable runtime bounds/debug checks |
| `-fcobol-exceptions`, `-fec=<ec>`, `-fno-ec=<ec>` | Enable/disable ISO `EC-*` runtime exception checking globally or per category |
| `-finternal-ebcdic`, `-fexec-charset=<cs>` | Configure internal EBCDIC (`CP1140`/`CP037`) or execution character set |
| `-O0`, `-O`, `-O1`, `-O2`, `-O3`, `-Ofast`, `-Os`, `-Oz` | Select MLIR PassManager & LLVM 20 `TargetMachine` optimization pipeline |
| `-g` | Emit source location / DWARF debug metadata |
| `-L <dir>`, `-l <lib>`, `-M <dir>` | Add native linker search directory, link C/system library, or add dynamic module directory |

---

## 5. Runtime Environment Variables

Compiled executables and `cobcrun-shim` honor standard clean-room runtime environment variables:

| Variable | Subsystem | Behavior |
| :--- | :--- | :--- |
| `COB_LIBRARY_PATH` | `coblang-dynload` | Colon-separated directory search list for dynamic `CALL` and `cobcrun-shim` module resolution |
| `COB_PRE_LOAD` | `coblang-dynload` | Pre-load shared library modules at runtime startup (`cob_init`) |
| `COB_CURRENT_DATE` | `coblang-rt-core` | Deterministic date/time override (`YYYY/MM/DD HH:MM:SS` or `YYYYMMDDHHMMSS`) for `ACCEPT ... FROM DATE/TIME` & `FUNCTION CURRENT-DATE` |
| `COB_COPY_DIR` | `coblang-pp` | Fallback copybook search directory at compile time |
| `COB_FILE_PATH` | `coblang-fileio` | Base directory prepended to relative `ASSIGN TO` filenames |
| `DD_<NAME>` / `dd_<name>` / `<NAME>` | `coblang-fileio` | Mainframe JCL-style external file path mapping for `SELECT ... ASSIGN TO <NAME>` |
| `COB_LS_FIXED` / `COB_LS_NULLS` | `coblang-fileio` | Preserve trailing spaces (`COB_LS_FIXED=1`) or escape binary bytes (`COB_LS_NULLS=1`) in `LINE SEQUENTIAL` files |
| `COB_SORT_MEMORY` | `coblang-sort` | Maximum in-memory buffer size in bytes before spilling sorted runs to temporary merge files |
| `COB_SWITCH_0` .. `COB_SWITCH_36` | `coblang-rt-core` | Initialize `SPECIAL-NAMES` external `SWITCH-n` (`ON`/`1` vs `OFF`/`0`) |

---

## 6. Running the Conformance Harness (`coblang-test`) & Quality Gates

### Run Filtered Conformance Tests During Feature Development
```bash
# Run a single bundle file
cargo run -p coblang-test-runner --bin coblang-test -- run --bundle tests/ported/gcobol/group1.bundle -j 8

# Filter across all 2,527 tests by name substring or taxonomy FeatureTag
cargo run -p coblang-test-runner --bin coblang-test -- run --bundle-dir tests --tag feature:arith --filter multiply -j 8

# Debug a single test ID with verbose step stdout/stderr
cargo run -p coblang-test-runner --bin coblang-test -- run-one --bundle tests/ported/gnucobol/data_packed.bundle --id gnucobol/data_packed/0001
```

### Ratchet Baseline Expectations & Regenerate Scoreboard
```bash
cargo run -p coblang-test-runner --bin coblang-test -- run --bundle-dir tests --baseline tests/expectations/baseline.toml --write-baseline tests/expectations/baseline.toml --ratchet -j 8

cargo run -p coblang-test-runner --bin coblang-test -- summary --bundle-dir tests --output tests/expectations/SCOREBOARD.md --json tests/expectations/SCOREBOARD.json -j 8
```

### Export LLVM `lit` Suite
```bash
cargo run -p coblang-test-runner --bin coblang-test -- emit-lit --bundle-dir tests --out-dir target/lit-suite
```

### Execute All 7 Mandatory Repository Quality Gates
```bash
./scripts/check-quality.sh
```
`check-quality.sh` enforces: (1) `cargo fmt --check`, (2) strict workspace `cargo clippy --all-targets -- -D warnings`, (3) `RUSTDOCFLAGS="-D warnings ..."` private-item Rustdoc build, (4) `100%` 5-way documentation coverage verification (`scripts/update-readme-shields.py --metric doc --verify-strict-100 --check`), (5) GPLv3 clean-room & forbidden-crate audit, (6) `100.00%` function, line, and branch coverage via `cargo +nightly llvm-cov --workspace --branch`, and (7) zero-regression `coblang-test --ratchet` across all 2,527 tests.

---

## 7. Parallel Multi-Agent / Git-Worktree Execution Workflow

Consult [`TODO_PLAN.md`](TODO_PLAN.md) and [`00_TODO_PLAN.md`](00_TODO_PLAN.md) to launch concurrent development tracks without file collisions. For example, to run three Stage `06` tracks in parallel git worktrees:

```bash
git worktree add ../coblang-06b -b feat/06b-intrinsics
git worktree add ../coblang-06d -b feat/06d-fileio
git worktree add ../coblang-06e -b feat/06e-isam
```

Each agent works strictly inside its assigned `06X_*_TODO_PLAN.md` crate directory, runs its fast per-crate `clippy` + `llvm-cov --branch` gate, and merges cleanly into the main integration branch without `git push`.
