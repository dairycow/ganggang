#!/usr/bin/env python3
"""Tests for scripts/module-sizes.py.

Run with ``python3 scripts/test-module-sizes.py``.
"""

from __future__ import annotations

import contextlib
import importlib
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))
module_sizes = importlib.import_module("module-sizes")


def run_main(args):
    """Run main(argv), capturing stdout and stderr as (code, out, err)."""
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = module_sizes.main(args)
    return code, out.getvalue(), err.getvalue()


def rs_lines(count):
    """``count`` plain Rust lines, sized exactly ``count``."""
    return "\n".join(f"fn item_{i}() {{}}" for i in range(count)) + "\n"


def write_tree(root, files):
    """Write ``rel_path -> content`` files under ``root``."""
    for rel, content in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")


class ModuleSizesTest(unittest.TestCase):
    def run_json(self, files, extra_args=()):
        """Write a tree, run --json, and return the payload."""
        with tempfile.TemporaryDirectory() as tmp:
            write_tree(Path(tmp), files)
            code, out, _ = run_main(["--root", str(tmp), *extra_args, "--json"])
            self.assertEqual(code, 0)
            return json.loads(out)

    def test_size_stops_at_cfg_test_marker(self):
        content = rs_lines(5) + "#[cfg(test)]\n" + rs_lines(3)
        payload = self.run_json({"src/main.rs": content})
        self.assertEqual(payload["files"], [{"path": "src/main.rs", "lines": 5}])

    def test_file_without_marker_counts_whole(self):
        payload = self.run_json({"src/main.rs": rs_lines(7)})
        self.assertEqual(payload["files"], [{"path": "src/main.rs", "lines": 7}])

    def test_tests_rs_file_is_not_measured(self):
        payload = self.run_json({"src/tests.rs": rs_lines(12), "src/main.rs": rs_lines(4)})
        self.assertEqual(payload["files"], [{"path": "src/main.rs", "lines": 4}])
        self.assertEqual(payload["summary"]["files"], 1)

    def test_underscore_tests_rs_file_is_not_measured(self):
        payload = self.run_json(
            {"src/tui_tests.rs": rs_lines(9), "src/main.rs": rs_lines(4)}
        )
        self.assertEqual(payload["files"], [{"path": "src/main.rs", "lines": 4}])
        self.assertEqual(payload["summary"]["files"], 1)

    def test_files_under_tests_directory_are_not_measured(self):
        payload = self.run_json(
            {
                "src/tests/helper.rs": rs_lines(6),
                "src/tui/tests/widget.rs": rs_lines(8),
                "src/main.rs": rs_lines(4),
            }
        )
        self.assertEqual(payload["files"], [{"path": "src/main.rs", "lines": 4}])
        self.assertEqual(payload["summary"]["files"], 1)

    def test_marker_inside_string_or_comment_is_not_the_marker(self):
        content = 'let marker = "#[cfg(test)]";\n// #[cfg(test)]\nfn main() {}\n'
        payload = self.run_json({"src/main.rs": content})
        self.assertEqual(payload["files"], [{"path": "src/main.rs", "lines": 3}])

    def test_min_lists_files_at_or_over_the_minimum(self):
        files = {
            "src/big.rs": rs_lines(10),
            "src/mid.rs": rs_lines(5),
            "src/small.rs": rs_lines(3),
        }
        with tempfile.TemporaryDirectory() as tmp:
            write_tree(Path(tmp), files)
            code, out, _ = run_main(["--root", str(tmp), "--min", "5"])
            self.assertEqual(code, 0)
            self.assertIn("10 src/big.rs", out)
            self.assertIn(" 5 src/mid.rs", out)
            self.assertNotIn("src/small.rs", out)
            # The summary still covers every measured file.
            self.assertIn("Files measured: 3", out)
            self.assertIn("Total lines: 18", out)

    def test_all_lists_every_measured_file(self):
        files = {
            "src/big.rs": rs_lines(10),
            "src/mid.rs": rs_lines(5),
            "src/small.rs": rs_lines(3),
        }
        with tempfile.TemporaryDirectory() as tmp:
            write_tree(Path(tmp), files)
            code, out, _ = run_main(["--root", str(tmp), "--all"])
            self.assertEqual(code, 0)
            self.assertIn("src/big.rs", out)
            self.assertIn("src/mid.rs", out)
            self.assertIn("src/small.rs", out)

    def test_summary_counts_at_default_thresholds(self):
        files = {
            "src/a.rs": rs_lines(350),
            "src/b.rs": rs_lines(450),
            "src/c.rs": rs_lines(550),
            "src/d.rs": rs_lines(100),
        }
        with tempfile.TemporaryDirectory() as tmp:
            write_tree(Path(tmp), files)
            code, out, _ = run_main(["--root", str(tmp)])
            self.assertEqual(code, 0)
            self.assertIn("Files measured: 4", out)
            self.assertIn("Total lines: 1450", out)
            self.assertIn("Over 300: 3", out)
            self.assertIn("Over 400: 2", out)
            self.assertIn("Over 500: 1", out)
            self.assertIn("Largest: src/c.rs (550 lines)", out)

    def test_summary_counts_at_custom_thresholds(self):
        files = {
            "src/a.rs": rs_lines(350),
            "src/b.rs": rs_lines(450),
            "src/c.rs": rs_lines(550),
            "src/d.rs": rs_lines(100),
        }
        with tempfile.TemporaryDirectory() as tmp:
            write_tree(Path(tmp), files)
            code, out, _ = run_main(["--root", str(tmp), "--thresholds", "100,200"])
            self.assertEqual(code, 0)
            # A file of exactly 100 lines is not over the 100 threshold.
            self.assertIn("Over 100: 3", out)
            self.assertIn("Over 200: 3", out)
            self.assertNotIn("Over 300", out)

    def test_json_keys_and_order(self):
        payload = self.run_json(
            {
                "src/b.rs": rs_lines(20),
                "src/a.rs": rs_lines(20),
                "src/c.rs": rs_lines(30),
            }
        )
        self.assertEqual(list(payload), ["rule", "files", "summary"])
        self.assertEqual(
            [list(f) for f in payload["files"]],
            [["path", "lines"], ["path", "lines"], ["path", "lines"]],
        )
        self.assertTrue(payload["rule"].endswith("measured."))
        self.assertEqual(
            list(payload["summary"]),
            ["files", "total_lines", "over_300", "over_400", "over_500", "largest"],
        )

    def test_order_largest_first_then_path(self):
        payload = self.run_json(
            {
                "src/b.rs": rs_lines(20),
                "src/a.rs": rs_lines(20),
                "src/c.rs": rs_lines(30),
            }
        )
        paths = [f["path"] for f in payload["files"]]
        self.assertEqual(paths, ["src/c.rs", "src/a.rs", "src/b.rs"])

    def test_exit_status_zero_regardless_of_sizes(self):
        with tempfile.TemporaryDirectory() as tmp:
            write_tree(Path(tmp), {"src/huge.rs": rs_lines(600)})
            code, out, _ = run_main(["--root", str(tmp)])
            self.assertEqual(code, 0)
            self.assertIn("600 src/huge.rs", out)
            self.assertIn("Over 500: 1", out)
            code, out, _ = run_main(["--root", str(tmp), "--json"])
            self.assertEqual(code, 0)

    def test_exit_two_when_root_has_no_src(self):
        with tempfile.TemporaryDirectory() as tmp:
            code, out, err = run_main(["--root", str(tmp)])
            self.assertEqual(code, 2)
            self.assertEqual(out, "")
            self.assertIn("no src/ directory", err)

    def test_exit_two_for_non_whole_threshold(self):
        with tempfile.TemporaryDirectory() as tmp:
            write_tree(Path(tmp), {"src/main.rs": rs_lines(4)})
            for thresholds in ("300.5", "abc", "300,"):
                code, _, err = run_main(
                    ["--root", str(tmp), "--thresholds", thresholds]
                )
                self.assertEqual(code, 2, thresholds)
                self.assertIn("whole numbers", err)


if __name__ == "__main__":
    unittest.main()
