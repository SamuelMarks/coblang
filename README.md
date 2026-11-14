# `coblang`

![doc coverage](https://img.shields.io/badge/doc%20coverage-100%25-brightgreen) ![test coverage](https://img.shields.io/badge/test%20coverage-99.9%25-green) ![license](https://img.shields.io/badge/license-Apache--2.0%20WITH%20LLVM--exception-blue)

Clean-room COBOL compiler infrastructure, compatibility CLI shims (`coblang`, `cobc`, `cobcrun`, `gcobol`), and unified conformance test harness targeting GCC COBOL (`gcobol`), `GnuCOBOL`, and NIST CCVS-85 (COBOL-85).

## Workspace Crates

| Crate | Path | Description |
| :--- | :--- | :--- |
| `coblang-common` | `.` | Strongly-typed compiler/harness domain model, deterministic manifest bundle codec, and unified `CoblangError` enum. |
| `coblang-scaffold-driver` | `tools/scaffold-driver` | CLI flag parser and driver shims (`coblang`, `cobc-shim`, `cobcrun-shim`, `gcobol-shim`). |
| `coblang-test-importer` | `tools/test-importer` | Importers for GCC DejaGnu (`cobol.dg` / `libgcobol`), `GnuCOBOL` Autotest (`.at`), and NIST CCVS-85 (`newcob.val`). |
| `coblang-test-runner` | `tools/test-runner` | Parallel isolated test runner (`coblang-test`), diagnostic/output matchers, `llvm-lit` emitter, and expectation ratchet. |

## Ported Conformance Suites

All **2,527** conformance and regression tests are isolated under `tests/` with cryptographic upstream verification (`./scripts/fetch-upstream-tests.sh`) and strict clean-room license boundaries (`tests/LICENSE_MANIFEST.md`):

- **GCC COBOL (`gcobol`)**: 543 tests (`tests/ported/gcobol/*.bundle`)
- **`GnuCOBOL`**: 1,556 tests (`tests/ported/gnucobol/*.bundle`)
- **NIST CCVS-85**: 426 validation programs across all 12 modules (`tests/ported/nist85/*.bundle`)
- **Canary Self-Tests**: Harness verification cases (`tests/canary/canary.bundle`)

## Development & Pre-Commit Hooks

Install and run the [pre-commit](https://pre-commit.com/) hooks (`pip install pre-commit`):

```bash
# Fetch and verify pinned upstream COBOL test archives (one-time setup)
./scripts/fetch-upstream-tests.sh

# Install git pre-commit hook
pre-commit install

# Run all pre-commit hooks manually (format + recheck, build, lint, test, README shields)
pre-commit run --all-files
```

Run the full 7-gate quality verification script directly:

```bash
./scripts/check-quality.sh
```

## License

Compiler and harness crates under `src/` and `tools/` are licensed under `Apache-2.0 WITH LLVM-exception`. See [`tests/LICENSE_MANIFEST.md`](tests/LICENSE_MANIFEST.md) for the SPDX licenses and clean-room isolation rules governing ported external test suites under `tests/ported/`.
