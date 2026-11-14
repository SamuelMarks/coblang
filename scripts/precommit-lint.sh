#!/usr/bin/env bash
# Runs strict Clippy, Rustdoc, file/argument documentation, and GPLv3 clean-room lints.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "==> [1/4] Checking strict Clippy lints (workspace, all targets)..."
cargo clippy --workspace --all-targets --locked -- -D warnings

echo "==> [2/4] Checking Rustdoc (including private items)..."
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items --locked --quiet

echo "==> [3/4] Checking file-header & function-argument doc coverage..."
python3 "$ROOT_DIR/scripts/update-readme-shields.py" --metric doc --verify-strict-100

echo "==> [4/4] Checking GPLv3 clean-room boundary..."
python3 - <<'PY'
import pathlib, re, sys

forbidden = re.compile(r'include_(?:str|bytes)!\s*\([^)]*ported/')
for path in list(pathlib.Path("src").rglob("*.rs")) + list(pathlib.Path("tools").rglob("*.rs")):
    content = path.read_text(encoding="utf-8")
    if forbidden.search(content):
        print(f"Clean-room violation in {path}: compiler/tooling embeds ported GPLv3 test data", file=sys.stderr)
        sys.exit(1)
PY
