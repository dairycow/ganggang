#!/usr/bin/env python3
"""Measure Rust module sizes in lines before their tests.

Walks ``src/`` under a repository root and sizes every ``.rs`` file as its
line count before the first line whose trimmed text starts with
``#[cfg(test)]``; blank and comment lines count, and a file with no such
line counts whole. Test files (``tests.rs``, ``*_tests.rs``, anything under
a ``tests/`` directory) are not measured. The script reports sizes only:
nothing fails and nothing is enforced.

Stdlib only. See ``scripts/test-module-sizes.py``.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

DEFAULT_THRESHOLDS = "300,400,500"

RULE = (
    "A file's size is its line count before the first line that starts with "
    "#[cfg(test)], blank and comment lines included, and test files are not "
    "measured."
)


def module_lines(path):
    """Lines before the first line that starts with #[cfg(test)]."""
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return 0
    count = 0
    for line in text.splitlines():
        if line.strip().startswith("#[cfg(test)]"):
            break
        count += 1
    return count


def is_test_file(rel_path):
    """True for tests.rs, *_tests.rs, or anything under a tests/ directory."""
    parts = rel_path.split("/")
    name = parts[-1]
    if name == "tests.rs" or name.endswith("_tests.rs"):
        return True
    return "tests" in parts[:-1]


def measure_tree(root):
    """Every measured .rs file under root/src, largest first then by path."""
    files = []
    for dirpath, dirnames, filenames in os.walk(root / "src"):
        dirnames.sort()
        for name in sorted(filenames):
            if not name.endswith(".rs"):
                continue
            path = Path(dirpath) / name
            rel = path.relative_to(root).as_posix()
            if is_test_file(rel):
                continue
            files.append({"path": rel, "lines": module_lines(path)})
    files.sort(key=lambda f: (-f["lines"], f["path"]))
    return files


def parse_thresholds(text):
    """Whole-number thresholds, or None when the value is not usable."""
    thresholds = []
    for part in text.split(","):
        try:
            thresholds.append(int(part.strip()))
        except ValueError:
            return None
    return thresholds


def build_summary(files, thresholds):
    """Counts for every measured file at the given thresholds."""
    summary = {
        "files": len(files),
        "total_lines": sum(f["lines"] for f in files),
    }
    for threshold in thresholds:
        summary[f"over_{threshold}"] = sum(1 for f in files if f["lines"] > threshold)
    summary["largest"] = (
        {"path": files[0]["path"], "lines": files[0]["lines"]} if files else None
    )
    return summary


def render_text(listed, summary, thresholds):
    """The table of listed files, then the summary."""
    out = []
    width = max((len(str(f["lines"])) for f in listed), default=1)
    for f in listed:
        out.append(f"{f['lines']:>{width}} {f['path']}")
    if out:
        out.append("")
    out.append(f"Files measured: {summary['files']}")
    out.append(f"Total lines: {summary['total_lines']}")
    for threshold in thresholds:
        out.append(f"Over {threshold}: {summary[f'over_{threshold}']}")
    largest = summary["largest"]
    if largest:
        out.append(f"Largest: {largest['path']} ({largest['lines']} lines)")
    return "\n".join(out) + "\n"


def parse_args(argv=None):
    parser = argparse.ArgumentParser(
        description="Measure Rust module sizes in lines before their tests."
    )
    parser.add_argument(
        "--min",
        type=int,
        default=300,
        help="List files with this many lines or more (default: 300).",
    )
    parser.add_argument(
        "--all",
        action="store_true",
        help="List every measured file instead of only those over the minimum.",
    )
    parser.add_argument(
        "--thresholds",
        default=DEFAULT_THRESHOLDS,
        help=(
            "Comma-separated whole-number summary thresholds "
            "(default: 300,400,500)."
        ),
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit one JSON object instead of the table.",
    )
    parser.add_argument(
        "--root",
        default=".",
        help="Repository root to walk (default: current directory).",
    )
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    root = Path(args.root)
    if not (root / "src").is_dir():
        print(f"module-sizes: no src/ directory under {root}", file=sys.stderr)
        return 2
    thresholds = parse_thresholds(args.thresholds)
    if thresholds is None:
        print(
            f"module-sizes: thresholds must be whole numbers, "
            f"got {args.thresholds!r}",
            file=sys.stderr,
        )
        return 2

    files = measure_tree(root)
    summary = build_summary(files, thresholds)
    if args.json:
        print(json.dumps({"rule": RULE, "files": files, "summary": summary}, indent=2))
    else:
        listed = files if args.all else [
            f for f in files if f["lines"] >= args.min
        ]
        sys.stdout.write(render_text(listed, summary, thresholds))
    return 0


if __name__ == "__main__":
    sys.exit(main())
