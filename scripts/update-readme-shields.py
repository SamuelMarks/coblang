#!/usr/bin/env python3
"""Computes workspace doc/test coverage percentages and updates README.md shields."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import subprocess
import sys
from typing import Sequence

FN_RE = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)\s*(?:<[^>]*>)?\s*\((.*)"
)
ITEM_RE = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|type|const|static|macro_rules!)\s+([A-Za-z0-9_]+)"
)

DOC_SHIELD_RE = re.compile(
    r"!\[%? ?doc coverage\]\(https://img\.shields\.io/badge/[^)]*doc(?:%20|_)coverage-[^)]+\)",
    re.IGNORECASE,
)
TEST_SHIELD_RE = re.compile(
    r"!\[%? ?test coverage\]\(https://img\.shields\.io/badge/[^)]*test(?:%20|_)coverage-[^)]+\)",
    re.IGNORECASE,
)


def parse_fn_param_names(params_part: str) -> list[str]:
    """Extracts non-receiver parameter names from a Rust function parameter list.

    Args:
        params_part: Raw text inside `fn name(...)` parentheses.

    Returns:
        Ordered list of parameter identifiers excluding `self` and `_`.
    """
    if not params_part:
        return []
    raw_params: list[str] = []
    depth = 0
    cur: list[str] = []
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

    arg_names: list[str] = []
    for rp in raw_params:
        if rp in ("self", "&self", "&mut self", "mut self"):
            continue
        if ":" in rp:
            lhs = rp.split(":", 1)[0].strip().removeprefix("mut ").strip()
            if lhs and lhs != "_":
                arg_names.append(lhs)
    return arg_names


def collect_preceding_docs(lines: Sequence[str], index: int) -> tuple[str, bool]:
    """Collects contiguous `///` or `#[doc = ...]` lines preceding `lines[index]`.

    Args:
        lines: Source file lines.
        index: Zero-based line index of the item declaration.

    Returns:
        Tuple of `(joined_doc_text, has_test_attribute)`.
    """
    k = index - 1
    doc_lines: list[str] = []
    has_test_attr = False
    while k >= 0:
        stripped = lines[k].strip()
        if stripped.startswith("///") or stripped.startswith("#[doc"):
            doc_lines.append(stripped)
        elif stripped.startswith("#["):
            if "test" in stripped:
                has_test_attr = True
        else:
            break
        k -= 1
    return chr(10).join(reversed(doc_lines)), has_test_attr


def count_compiler_missing_doc_warnings(root: pathlib.Path) -> int:
    """Counts compiler/Clippy missing-documentation diagnostics across the workspace.

    Args:
        root: Repository root directory.

    Returns:
        Number of distinct `missing_docs` / `missing_docs_in_private_items` diagnostics.
    """
    proc = subprocess.run(
        [
            "cargo",
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--message-format=json",
            "--quiet",
        ],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    missing_spans: set[tuple[str, int, int]] = set()
    doc_codes = {
        "missing_docs",
        "clippy::missing_docs_in_private_items",
        "rustdoc::missing_crate_level_docs",
    }
    for raw_line in proc.stdout.splitlines():
        raw_line = raw_line.strip()
        if not raw_line.startswith("{"):
            continue
        try:
            msg = json.loads(raw_line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") != "compiler-message":
            continue
        inner = msg.get("message", {})
        code_obj = inner.get("code") or {}
        if code_obj.get("code") in doc_codes:
            for span in inner.get("spans", []):
                if span.get("is_primary"):
                    missing_spans.add(
                        (
                            str(span.get("file_name", "")),
                            int(span.get("line_start", 0)),
                            int(span.get("column_start", 0)),
                        )
                    )
    return len(missing_spans)


def compute_doc_coverage(root: pathlib.Path) -> tuple[int, int, list[str]]:
    """Computes workspace documentation coverage across headers, items, and parameters.

    Args:
        root: Repository root directory.

    Returns:
        Tuple of `(documented_units, total_units, strict_error_messages)`.
    """
    rs_files = sorted(
        list(root.glob("src/**/*.rs"))
        + list(root.glob("tools/**/*.rs"))
        + list(root.glob("tests/**/*.rs"))
    )
    total_units = 0
    documented_units = 0
    errors: list[str] = []

    for path in rs_files:
        rel = path.relative_to(root)
        lines = path.read_text(encoding="utf-8").splitlines()
        total_units += 1
        if lines and lines[0].startswith("//!"):
            documented_units += 1
        else:
            errors.append(f"{rel}:1: missing `//!` file/module documentation header")

        is_test_file = rel.parts[0] == "tests" or "tests" in rel.parts
        i = 0
        while i < len(lines):
            fn_match = FN_RE.match(lines[i])
            if fn_match:
                fn_name = fn_match.group(1)
                sig_lines = [lines[i]]
                j = i
                while ")" not in lines[j] and j + 1 < len(lines):
                    j += 1
                    sig_lines.append(lines[j])
                sig = " ".join(sig_lines)
                params_part = sig.split("(", 1)[1].rsplit(")", 1)[0].strip()

                doc_text, has_test_attr = collect_preceding_docs(lines, i)
                total_units += 1
                if doc_text:
                    documented_units += 1
                else:
                    errors.append(
                        f"{rel}:{i + 1}: function `{fn_name}` is missing `///` doc comment"
                    )

                if not is_test_file and not has_test_attr:
                    arg_names = parse_fn_param_names(params_part)
                    if arg_names and "# Arguments" not in doc_text:
                        errors.append(
                            f"{rel}:{i + 1}: function `{fn_name}` takes parameters {arg_names} but lacks `# Arguments` section"
                        )
                    for arg in arg_names:
                        total_units += 1
                        if "# Arguments" in doc_text and f"`{arg}`" in doc_text:
                            documented_units += 1
                        elif f"`{arg}`" not in doc_text:
                            errors.append(
                                f"{rel}:{i + 1}: function `{fn_name}` does not document parameter `{arg}`"
                            )
                i = j + 1
                continue

            item_match = ITEM_RE.match(lines[i])
            if item_match:
                item_name = item_match.group(1)
                doc_text, _ = collect_preceding_docs(lines, i)
                total_units += 1
                if doc_text:
                    documented_units += 1
                else:
                    errors.append(
                        f"{rel}:{i + 1}: item `{item_name}` is missing `///` doc comment"
                    )
            i += 1

    extra_compiler_missing = count_compiler_missing_doc_warnings(root)
    if extra_compiler_missing > 0:
        total_units += extra_compiler_missing
        errors.append(
            f"compiler reported {extra_compiler_missing} missing documentation diagnostic(s)"
        )

    return documented_units, total_units, errors


def compute_test_coverage(root: pathlib.Path) -> tuple[int, int]:
    """Runs `cargo +nightly llvm-cov --branch` and aggregates function/line/branch totals.

    Args:
        root: Repository root directory.

    Returns:
        Tuple of `(covered_points, total_points)` summed across functions, lines, and branches.
    """
    proc = subprocess.run(
        [
            "cargo",
            "+nightly",
            "llvm-cov",
            "--workspace",
            "--branch",
            "--json",
            "--summary-only",
        ],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        raise RuntimeError(f"cargo llvm-cov failed with exit code {proc.returncode}")

    report = json.loads(proc.stdout)
    totals = report["data"][0]["totals"]
    covered = 0
    total = 0
    for metric in ("functions", "lines", "branches"):
        info = totals[metric]
        covered += int(info["covered"])
        total += int(info["count"])
    return covered, total


def format_percentage_and_color(covered: int, total: int) -> tuple[str, str]:
    """Formats a coverage ratio into a badge text string and Shields.io color name.

    Never rounds an incomplete ratio (`covered < total`) up to `100%`.

    Args:
        covered: Covered items/points count.
        total: Total items/points count.

    Returns:
        Tuple of `(display_pct_without_percent_sign, shield_color)`.
    """
    if total == 0 or covered >= total:
        return "100", "brightgreen"
    pct = (covered * 100.0) / total
    capped = min(pct, 99.9)
    display = f"{capped:.1f}"
    if pct >= 95.0:
        color = "green"
    elif pct >= 90.0:
        color = "yellowgreen"
    elif pct >= 80.0:
        color = "yellow"
    elif pct >= 70.0:
        color = "orange"
    else:
        color = "red"
    return display, color


def build_shield_markdown(kind: str, covered: int, total: int) -> str:
    """Builds a Markdown Shields.io badge for `doc` or `test` coverage.

    Args:
        kind: Either `"doc"` or `"test"`.
        covered: Covered count.
        total: Total count.

    Returns:
        Markdown image badge string.
    """
    pct_str, color = format_percentage_and_color(covered, total)
    label = f"{kind} coverage"
    url_label = f"{kind}%20coverage"
    return f"![{label}](https://img.shields.io/badge/{url_label}-{pct_str}%25-{color})"


def upsert_shield_in_text(text: str, kind: str, shield_md: str) -> str:
    """Inserts or updates a single coverage shield inside Markdown text.

    Args:
        text: Existing Markdown content of `README.md`.
        kind: Either `"doc"` or `"test"`.
        shield_md: Rendered Markdown shield string to place into `text`.

    Returns:
        Updated Markdown content with `shield_md` present and up to date.
    """
    target_re = DOC_SHIELD_RE if kind == "doc" else TEST_SHIELD_RE
    sibling_re = TEST_SHIELD_RE if kind == "doc" else DOC_SHIELD_RE

    if target_re.search(text):
        return target_re.sub(shield_md, text, count=1)

    sibling_match = sibling_re.search(text)
    if sibling_match:
        if kind == "doc":
            return (
                text[: sibling_match.start()]
                + shield_md
                + " "
                + text[sibling_match.start() :]
            )
        return (
            text[: sibling_match.end()] + " " + shield_md + text[sibling_match.end() :]
        )

    lf = chr(10)
    lines = text.splitlines(keepends=True)
    if lines and lines[0].startswith("#"):
        insert_pos = len(lines[0])
        prefix = "" if lines[0].endswith(lf) else lf
        return text[:insert_pos] + f"{prefix}{lf}{shield_md}{lf}" + text[insert_pos:]

    if text.strip():
        return f"# coblang{lf}{lf}{shield_md}{lf}{lf}{text}"
    return f"# coblang{lf}{lf}{shield_md}{lf}"


def update_or_check_readme(
    readme_path: pathlib.Path, kind: str, shield_md: str, check_only: bool
) -> bool:
    """Applies or checks a coverage shield in `readme_path`.

    Args:
        readme_path: Path to `README.md`.
        kind: `"doc"` or `"test"`.
        shield_md: Expected Markdown badge string.
        check_only: When `True`, do not write changes and return `False` if outdated.

    Returns:
        `True` when `readme_path` is already up to date (or updated cleanly in write mode).
    """
    existing = readme_path.read_text(encoding="utf-8") if readme_path.exists() else ""
    updated = upsert_shield_in_text(existing, kind, shield_md)
    if updated == existing:
        return True
    if check_only:
        print(
            f"{readme_path}: `{kind} coverage` shield is missing or out of date (expected {shield_md})",
            file=sys.stderr,
        )
        return False
    readme_path.write_text(updated, encoding="utf-8")
    print(f"Updated `{kind} coverage` shield in {readme_path}: {shield_md}")
    return True


def main(argv: Sequence[str] | None = None) -> int:
    """CLI entry point for computing coverage metrics and updating `README.md` shields.

    Args:
        argv: Optional CLI argument sequence excluding `argv[0]`.

    Returns:
        Process exit status (`0` on success, `1` on failure).
    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--metric",
        choices=("doc", "test", "all"),
        required=True,
        help="Coverage shield metric to compute and update.",
    )
    parser.add_argument(
        "--readme",
        default="README.md",
        help="Path to README.md relative to repository root.",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Verify README.md shield(s) are already up to date without modifying files.",
    )
    parser.add_argument(
        "--verify-strict-100",
        action="store_true",
        help="Exit non-zero if documentation coverage has any undocumented item/parameter.",
    )
    args = parser.parse_args(argv)

    root = pathlib.Path(__file__).resolve().parent.parent
    os.chdir(root)
    readme_path = root / args.readme

    if args.verify_strict_100:
        doc_cov, doc_tot, errors = compute_doc_coverage(root)
        if errors or doc_cov != doc_tot:
            for err in errors:
                print(err, file=sys.stderr)
            return 1
        return 0

    ok = True
    if args.metric in ("doc", "all"):
        doc_cov, doc_tot, _ = compute_doc_coverage(root)
        doc_shield = build_shield_markdown("doc", doc_cov, doc_tot)
        print(f"doc coverage: {doc_cov}/{doc_tot} -> {doc_shield}")
        ok = update_or_check_readme(readme_path, "doc", doc_shield, args.check) and ok

    if args.metric in ("test", "all"):
        test_cov, test_tot = compute_test_coverage(root)
        test_shield = build_shield_markdown("test", test_cov, test_tot)
        print(f"test coverage: {test_cov}/{test_tot} -> {test_shield}")
        ok = update_or_check_readme(readme_path, "test", test_shield, args.check) and ok

    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
