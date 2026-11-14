#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "==> [1/7] Checking rustfmt..."
cargo fmt --all -- --check

echo "==> [2/7] Checking strictest Clippy lints (workspace, all targets)..."
cargo clippy --workspace --all-targets -- -D warnings

echo "==> [3/7] Checking Rustdoc (including private items)..."
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items --quiet

echo "==> [4/7] Checking file-header & function-argument doc coverage..."
python3 - <<'PY'
import pathlib, re, sys

root = pathlib.Path(".")
rs_files = (
    list(root.glob("src/**/*.rs"))
    + list(root.glob("tools/**/*.rs"))
    + list(root.glob("tests/**/*.rs"))
)

fn_re = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)\s*(?:<[^>]*>)?\s*\((.*)"
)

errors = []
for path in sorted(rs_files):
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    if not lines or not lines[0].startswith("//!"):
        errors.append(f"{path}:1: missing `//!` file/module documentation header")

    is_test_file = path.parts[0] == "tests" or "tests" in path.parts
    i = 0
    while i < len(lines):
        m = fn_re.match(lines[i])
        if m:
            fn_name = m.group(1)
            sig_lines = [lines[i]]
            j = i
            while ")" not in lines[j] and j + 1 < len(lines):
                j += 1
                sig_lines.append(lines[j])
            sig = " ".join(sig_lines)
            params_part = sig.split("(", 1)[1].rsplit(")", 1)[0].strip()

            # Collect preceding doc comments & attributes
            k = i - 1
            doc_lines = []
            has_test_attr = False
            while k >= 0:
                stripped = lines[k].strip()
                if stripped.startswith("///"):
                    doc_lines.append(stripped)
                elif stripped.startswith("#["):
                    if "test" in stripped:
                        has_test_attr = True
                elif stripped == "":
                    break
                else:
                    break
                k -= 1
            doc_text = chr(10).join(reversed(doc_lines))
            if not doc_lines:
                errors.append(f"{path}:{i+1}: function `{fn_name}` is missing `///` doc comment")

            # Parse non-self parameter names for production code
            if not is_test_file and not has_test_attr and params_part:
                raw_params = []
                depth = 0
                cur = []
                for ch in params_part:
                    if ch in "([<":
                        depth += 1
                    elif ch in ")]>":
                        depth -= 1
                    if ch == "," and depth == 0:
                        tok = "".join(cur).strip()
                        if tok:
                            raw_params.append(tok)
                        cur = []
                    else:
                        cur.append(ch)
                tail = "".join(cur).strip()
                if tail:
                    raw_params.append(tail)
                arg_names = []
                for rp in raw_params:
                    if rp in ("self", "&self", "&mut self", "mut self"):
                        continue
                    if ":" in rp:
                        lhs = rp.split(":", 1)[0].strip()
                        lhs = lhs.removeprefix("mut ").strip()
                        if lhs and lhs != "_":
                            arg_names.append(lhs)
                if arg_names:
                    if "# Arguments" not in doc_text:
                        errors.append(
                            f"{path}:{i+1}: function `{fn_name}` takes parameters {arg_names} but lacks `# Arguments` section"
                        )
                    for arg in arg_names:
                        if f"`{arg}`" not in doc_text:
                            errors.append(
                                f"{path}:{i+1}: function `{fn_name}` does not document parameter `{arg}`"
                            )
            i = j + 1
        else:
            i += 1

if errors:
    for err in errors:
        print(err, file=sys.stderr)
    sys.exit(1)
PY

echo "==> [5/7] Checking GPLv3 clean-room boundary..."
python3 - <<'PY'
import pathlib, re, sys

forbidden = re.compile(r'include_(?:str|bytes)!\s*\([^)]*ported/')
for path in list(pathlib.Path("src").rglob("*.rs")) + list(pathlib.Path("tools").rglob("*.rs")):
    content = path.read_text(encoding="utf-8")
    if forbidden.search(content):
        print(f"Clean-room violation in {path}: compiler/tooling embeds ported GPLv3 test data", file=sys.stderr)
        sys.exit(1)
PY

echo "==> [6/7] Running 100% function, line & branch coverage gate..."
SUMMARY_JSON="$(cargo +nightly llvm-cov --workspace --branch --json --summary-only)"
SUMMARY_JSON="$SUMMARY_JSON" python3 - <<'PY'
import json, os, sys

report = json.loads(os.environ["SUMMARY_JSON"])
totals = report["data"][0]["totals"]
for metric in ("functions", "lines", "branches"):
    info = totals[metric]
    count = info["count"]
    covered = info["covered"]
    pct = info["percent"]
    print(f"  {metric:10s}: {covered}/{count} ({pct:.2f}%)")
    if count > 0 and covered != count:
        print(f"Coverage gate failed: {metric} covered {covered}/{count}", file=sys.stderr)
        for f in report["data"][0].get("files", []):
            sm = f["summary"][metric]
            if sm["count"] > 0 and sm["covered"] != sm["count"]:
                print(f"  uncovered in {f['filename']}: {sm['covered']}/{sm['count']}", file=sys.stderr)
        sys.exit(1)
PY

echo "==> [7/7] Running full ported test suite (gcobol + gnucobol + nist85 + canary) with ratchet..."
cargo run --quiet -p coblang-test-runner --bin coblang-test -- run --bundle-dir tests --baseline tests/expectations/baseline.toml --ratchet -j 8

echo "All 7 quality gates passed (100% docs, strict lints, clean-room, 100% fn/line/branch coverage, full-suite ratchet)."
