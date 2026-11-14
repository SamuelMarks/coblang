#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CACHE_DIR="$ROOT_DIR/.upstream-cache"
mkdir -p "$CACHE_DIR"

GNUCOBOL_COMMIT="02294c68b67eef1e407f3567b50173593dd160af"
GNUCOBOL_SHA256="fa1d5aaa042f29f631259ac4e79a7d64c24c9b77a797ec34e0f17568097af03c"

GCC_COMMIT="6e0de8bd5ebb9a2b9903cb7348ae015d9ec7d765"

NIST_URL="https://gnucobol.sourceforge.io/files/newcob.val.tar.gz"
NIST_FALLBACK_URL="https://web.archive.org/web/20240322143655if_/https://www.itl.nist.gov/div897/ctg/suites/newcob.val.Z"
NIST_TAR_SHA256="e4513f26a9b38911f7bf882fe3d3339a80b45cabc2caf85eb3055b0f5ce87ee0"

verify_sha256() {
    local file="$1"
    local expected="$2"
    local actual
    actual="$(shasum -a 256 "$file" | awk '{print $1}')"
    if [[ "$actual" != "$expected" ]]; then
        echo "SHA-256 mismatch for $file: expected $expected, got $actual" >&2
        exit 1
    fi
}

echo "==> [1/3] Fetching & verifying NIST CCVS-85 (newcob.val)..."
NIST_ARCHIVE="$CACHE_DIR/newcob.val.tar.gz"
if [[ ! -f "$NIST_ARCHIVE" ]]; then
    curl -fsSL "$NIST_URL" -o "$NIST_ARCHIVE" || {
        echo "Primary NIST mirror unavailable; note fallback $NIST_FALLBACK_URL" >&2
        exit 1
    }
fi
verify_sha256 "$NIST_ARCHIVE" "$NIST_TAR_SHA256"
mkdir -p "$CACHE_DIR/nist85"
if [[ ! -f "$CACHE_DIR/nist85/newcob.val" ]]; then
    tar -xzf "$NIST_ARCHIVE" -C "$CACHE_DIR/nist85"
fi
echo "  Verified newcob.val ($(wc -l < "$CACHE_DIR/nist85/newcob.val" | tr -d ' ') lines)"

echo "==> [2/3] Fetching & verifying GnuCOBOL tests ($GNUCOBOL_COMMIT)..."
GNUCOBOL_ARCHIVE="$CACHE_DIR/gnucobol-02294c68.tar.gz"
if [[ ! -f "$GNUCOBOL_ARCHIVE" ]]; then
    curl -fsSL "https://codeload.github.com/OCamlPro/gnucobol/tar.gz/$GNUCOBOL_COMMIT" -o "$GNUCOBOL_ARCHIVE"
fi
verify_sha256 "$GNUCOBOL_ARCHIVE" "$GNUCOBOL_SHA256"
if [[ ! -f "$CACHE_DIR/gnucobol/tests/testsuite.at" ]]; then
    rm -rf "$CACHE_DIR/gnucobol"
    mkdir -p "$CACHE_DIR/gnucobol"
    tar -xzf "$GNUCOBOL_ARCHIVE" -C "$CACHE_DIR/gnucobol" --strip-components=1 "gnucobol-$GNUCOBOL_COMMIT/tests"
fi
echo "  Verified GnuCOBOL tests/ ($(ls "$CACHE_DIR/gnucobol/tests/testsuite.src/"*.at | wc -l | tr -d ' ') .at files)"

echo "==> [3/3] Fetching & blob-verifying GCC COBOL testsuite ($GCC_COMMIT)..."
GCC_COMMIT="$GCC_COMMIT" CACHE_DIR="$CACHE_DIR" python3 - <<'PY'
import concurrent.futures
import hashlib
import json
import os
import pathlib
import sys
import urllib.request

commit = os.environ["GCC_COMMIT"]
cache_dir = pathlib.Path(os.environ["CACHE_DIR"]) / "gcobol"
stamp_file = cache_dir / ".verified_commit"

if stamp_file.exists() and stamp_file.read_text(encoding="utf-8").strip() == commit:
    cob_count = len(list((cache_dir / "gcc/testsuite/cobol.dg").rglob("*.cob")))
    print(f"  Already verified GCC cobol.dg cache ({cob_count} .cob files)")
    sys.exit(0)

def api_get(url: str):
    req = urllib.request.Request(url, headers={"User-Agent": "coblang-test-fetcher/0.1"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read().decode("utf-8"))

targets = []

# 1. gcc/testsuite/lib/cobol-dg.exp
lib_items = api_get(f"https://api.github.com/repos/gcc-mirror/gcc/contents/gcc/testsuite/lib/cobol-dg.exp?ref={commit}")
targets.append(("gcc/testsuite/lib/cobol-dg.exp", lib_items["sha"]))

# 2. gcc/testsuite/cobol.dg (top-level + subdirs)
dg_items = api_get(f"https://api.github.com/repos/gcc-mirror/gcc/contents/gcc/testsuite/cobol.dg?ref={commit}")
for item in dg_items:
    if item["type"] == "file":
        targets.append((f"gcc/testsuite/cobol.dg/{item['name']}", item["sha"]))
    elif item["type"] == "dir":
        sub_name = item["name"]
        tree_data = api_get(item["git_url"])
        if tree_data.get("truncated"):
            raise RuntimeError(f"Tree truncated for {sub_name}")
        for entry in tree_data["tree"]:
            if entry["type"] == "blob":
                targets.append((f"gcc/testsuite/cobol.dg/{sub_name}/{entry['path']}", entry["sha"]))

# 3. libgcobol/{compat,posix} tests + companion copybooks/UDFs
for sub in ("compat/t", "compat/gnu/cpy", "posix/t", "posix/cpy", "posix/udf"):
    t_items = api_get(f"https://api.github.com/repos/gcc-mirror/gcc/contents/libgcobol/{sub}?ref={commit}")
    for item in t_items:
        if item["type"] == "file":
            targets.append((f"libgcobol/{sub}/{item['name']}", item["sha"]))

def git_blob_sha1(data: bytes) -> str:
    header = f"blob {len(data)}\0".encode("ascii")
    h = hashlib.sha1()
    h.update(header)
    h.update(data)
    return h.hexdigest()

def fetch_one(entry):
    rel_path, expected_sha = entry
    dest = cache_dir / rel_path
    if dest.exists():
        existing = dest.read_bytes()
        if git_blob_sha1(existing) == expected_sha:
            return rel_path, expected_sha
    url = f"https://raw.githubusercontent.com/gcc-mirror/gcc/{commit}/{rel_path}"
    req = urllib.request.Request(url, headers={"User-Agent": "coblang-test-fetcher/0.1"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = resp.read()
    actual_sha = git_blob_sha1(data)
    if actual_sha != expected_sha:
        raise RuntimeError(f"Git blob SHA-1 mismatch for {rel_path}: expected {expected_sha}, got {actual_sha}")
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_bytes(data)
    return rel_path, actual_sha

with concurrent.futures.ThreadPoolExecutor(max_workers=20) as pool:
    results = list(pool.map(fetch_one, targets))

manifest_lines = [f"{sha}  {path}" for path, sha in sorted(results)]
manifest_bytes = (chr(10).join(manifest_lines) + chr(10)).encode("utf-8")
manifest_sha256 = hashlib.sha256(manifest_bytes).hexdigest()
expected_manifest_sha256 = "86b47da19786a061b554ff7cd6f1898592a90718d73ff10f676c672bad632b93"
if manifest_sha256 != expected_manifest_sha256:
    raise RuntimeError(f"GCC manifest SHA-256 mismatch: expected {expected_manifest_sha256}, got {manifest_sha256}")
(cache_dir / "MANIFEST.sha1").write_bytes(manifest_bytes)
stamp_file.write_text(commit + chr(10), encoding="utf-8")
print(f"  Verified {len(results)} GCC COBOL files (tree manifest SHA-256: {manifest_sha256})")
PY

echo "All upstream test suites fetched and cryptographically verified in $CACHE_DIR."
