#!/usr/bin/env bash
# Runs strict Clippy, Rustdoc, 5-way documentation, and GPLv3 clean-room lints.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ -z "${MLIR_SYS_200_PREFIX:-}" ]]; then
    if [[ -d "/opt/homebrew/opt/llvm@20" ]]; then
        export MLIR_SYS_200_PREFIX="/opt/homebrew/opt/llvm@20"
    elif [[ -d "/usr/lib/llvm-20" ]]; then
        export MLIR_SYS_200_PREFIX="/usr/lib/llvm-20"
    fi
fi
if [[ -z "${LLVM_SYS_201_PREFIX:-}" && -n "${MLIR_SYS_200_PREFIX:-}" ]]; then
    export LLVM_SYS_201_PREFIX="$MLIR_SYS_200_PREFIX"
fi
if [[ -z "${TABLEGEN_200_PREFIX:-}" && -n "${MLIR_SYS_200_PREFIX:-}" ]]; then
    export TABLEGEN_200_PREFIX="$MLIR_SYS_200_PREFIX"
fi
if [[ -d "/opt/homebrew/lib" ]]; then
    export LIBRARY_PATH="/opt/homebrew/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
fi

echo "==> [1/4] Checking strict Clippy lints (workspace, all targets)..."
cargo clippy --workspace --all-targets -- -D warnings

echo "==> [2/4] Checking Rustdoc (including private items)..."
RUSTDOCFLAGS="-D warnings -D missing_docs -D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links" cargo doc --workspace --no-deps --document-private-items --quiet

echo "==> [3/4] Checking 5-way file, module, item & function-argument doc coverage..."
python3 scripts/update-readme-shields.py --metric doc --verify-strict-100

echo "==> [4/4] Checking workspace members, no-push policy & GPLv3 clean-room boundary..."
python3 - <<'PY'
import pathlib, re, sys

cargo_text = pathlib.Path("Cargo.toml").read_text(encoding="utf-8")
members_match = re.search(r"(?ms)^\[workspace\]\s.*?^members\s*=\s*\[(.*?)\]", cargo_text)
configured_members = set(re.findall(r'"([^"]+)"', members_match.group(1))) if members_match else set()
discovered_members = {"."}
for container in (pathlib.Path("crates"), pathlib.Path("tools")):
    if container.is_dir():
        for child in sorted(container.iterdir()):
            if child.is_dir() and (child / "Cargo.toml").is_file():
                discovered_members.add(child.as_posix())
if configured_members != discovered_members:
    missing = sorted(discovered_members - configured_members)
    stale = sorted(configured_members - discovered_members)
    print(f"Workspace member mismatch in Cargo.toml: missing={missing}, stale={stale}", file=sys.stderr)
    sys.exit(1)

no_push_re = re.compile(r"\bgit" + r"\s+" + r"push\b")
for wf_path in [*sorted(pathlib.Path("scripts").glob("*")), *sorted(pathlib.Path(".github/workflows").glob("*.yml"))]:
    if wf_path.is_file() and no_push_re.search(wf_path.read_text(encoding="utf-8")):
        print(f"Policy violation in {wf_path}: forbidden push command detected", file=sys.stderr)
        sys.exit(1)

forbidden = re.compile(r'include_(?:str|bytes)!\s*\([^)]*ported/')
for base in (pathlib.Path("src"), pathlib.Path("tools"), pathlib.Path("crates")):
    if not base.exists():
        continue
    for path in sorted(base.rglob("*.rs")):
        content = path.read_text(encoding="utf-8")
        if forbidden.search(content):
            print(f"Clean-room violation in {path}: compiler/tooling embeds ported GPLv3 test data", file=sys.stderr)
            sys.exit(1)
PY
