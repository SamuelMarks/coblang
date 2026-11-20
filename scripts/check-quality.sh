#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ -d "$HOME/.cargo/bin" ]]; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

# Auto-detect LLVM/MLIR 20 installation prefix when not explicitly exported.
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
if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
    DEFAULT_TGT="$(cargo metadata --no-deps --format-version 1 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("target_directory",""))' || true)"
    if [[ -n "$DEFAULT_TGT" && -d "$(dirname "$DEFAULT_TGT")" ]]; then
        AVAIL_KB="$(df -Pk "$(dirname "$DEFAULT_TGT")" 2>/dev/null | awk 'NR==2 {print $4}')"
        if [[ -n "$AVAIL_KB" && "$AVAIL_KB" -lt 1048576 ]]; then
            export CARGO_TARGET_DIR="$ROOT_DIR/target"
        fi
    fi
fi

echo "==> [1/7] Checking rustfmt..."
cargo fmt --all -- --check

echo "==> [2/7] Checking strictest Clippy lints (workspace, all targets)..."
cargo clippy --workspace --all-targets -- -D warnings

echo "==> [3/7] Checking Rustdoc (including private items)..."
RUSTDOCFLAGS="-D warnings -D missing_docs -D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links" cargo doc --workspace --no-deps --document-private-items --quiet

echo "==> [4/7] Checking 5-way doc coverage (file, module, struct/enum/trait, fn, & # Arguments)..."
python3 scripts/update-readme-shields.py --metric doc --verify-strict-100

echo "==> [5/7] Checking workspace members, no-push policy & GPLv3 clean-room boundary..."
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
scan_roots = [pathlib.Path("src"), pathlib.Path("tools"), pathlib.Path("crates")]
for base in scan_roots:
    if not base.exists():
        continue
    for path in sorted(base.rglob("*.rs")):
        content = path.read_text(encoding="utf-8")
        if forbidden.search(content):
            print(f"Clean-room violation in {path}: compiler/tooling embeds ported GPLv3 test data", file=sys.stderr)
            sys.exit(1)
PY

echo "==> [6/7] Running 100% function, line & branch coverage gate..."
LLVM_COV_DIR="${CARGO_LLVM_COV_TARGET_DIR:-${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')}/llvm-cov-target}"
mkdir -p "$LLVM_COV_DIR"
printf 'Signature: 8a477f597d28d172789f06886806bc55\n' > "$LLVM_COV_DIR/CACHEDIR.TAG"
LLVM_COV_DIR="$LLVM_COV_DIR" python3 - <<'PY'
import fcntl, glob, json, os, shutil, subprocess, sys

cov_dir = os.environ["LLVM_COV_DIR"]
lock_path = os.path.join(cov_dir, ".check-quality.lock")
with open(lock_path, "w", encoding="utf-8") as lock_file:
    fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)
    for pattern in (
        "debug/build/coblang-*",
        "debug/deps/libcoblang*",
        "debug/libcoblang_rt*",
        "debug/coblang*",
        "debug/cobc*",
        "debug/gcobol*",
    ):
        for entry in glob.glob(os.path.join(cov_dir, pattern)):
            if os.path.isdir(entry) and not os.path.islink(entry):
                shutil.rmtree(entry, ignore_errors=True)
            else:
                try:
                    os.remove(entry)
                except OSError:
                    pass
    subprocess.run(["cargo", "+nightly", "llvm-cov", "clean", "--workspace"], check=True)
    proc = subprocess.run(
        ["cargo", "+nightly", "llvm-cov", "--workspace", "--branch", "--json", "--summary-only"],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    report = json.loads(proc.stdout)

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
cargo build --quiet -p coblang-rt-abi -p coblang-scaffold-driver --bin cobcrun-shim
cargo run --quiet -p coblang-test-runner --bin coblang-test -- run --bundle-dir tests --baseline tests/expectations/baseline.toml --ratchet -j 8

echo "All 7 quality gates passed (100% docs, strict lints, clean-room, 100% fn/line/branch coverage, full-suite ratchet)."
