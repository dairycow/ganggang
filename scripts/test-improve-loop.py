#!/usr/bin/env python3
"""Tests for scripts/improve-loop.sh --compare.

Run with ``python3 scripts/test-improve-loop.py``.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "improve-loop.sh"


def write_jsonl(path, **metrics):
    """Write one eval.jsonl record with the given metric fields."""
    path.write_text(json.dumps(metrics) + "\n", encoding="utf-8")


def write_jsonl_many(path, records):
    """Write several eval.jsonl records, one per repetition."""
    path.write_text(
        "\n".join(json.dumps(record) for record in records) + "\n",
        encoding="utf-8",
    )


def run_compare(base, cand, heldout_check=False, base_heldout=None, cand_heldout=None):
    """Invoke improve-loop.sh --compare and return (exit_code, stdout)."""
    args = ["bash", str(SCRIPT)]
    if heldout_check:
        args.append("--heldout-check")
    args.append("--compare")
    args.append(base)
    args.append(cand)
    if base_heldout is not None:
        args.append(base_heldout)
    if cand_heldout is not None:
        args.append(cand_heldout)
    proc = subprocess.run(args, capture_output=True, text=True)
    return proc.returncode, proc.stdout


def run_compare_raw(args):
    """Invoke improve-loop.sh with raw args and return the completed process."""
    return subprocess.run(["bash", str(SCRIPT)] + args, capture_output=True, text=True)


def assert_verdict(stdout, expected_keep, expected_reason_prefix):
    lines = [line for line in stdout.splitlines() if line.strip()]
    assert len(lines) == 1, f"expected a single verdict line, got {lines!r}"
    keep, _, verdict = lines[0].partition(" ")
    assert keep == expected_keep, f"expected keep={expected_keep}, got {lines[0]!r}"
    assert verdict.startswith(expected_reason_prefix), (
        f"expected verdict to start with {expected_reason_prefix!r}, got {verdict!r}"
    )
    assert verdict == verdict.strip(), "verdict should have no trailing whitespace"


class CompareRunsTest(unittest.TestCase):
    def test_core_win_no_heldout(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            write_jsonl(base, passed=2, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            write_jsonl(cand, passed=3, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            code, out = run_compare(str(base), str(cand))
            self.assertEqual(code, 0)
            assert_verdict(out, "1", "passed 3/5 > 2/5")

    def test_core_win_more_passed_heldout_regression(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            base_h = tmp / "base_heldout.jsonl"
            cand_h = tmp / "cand_heldout.jsonl"
            write_jsonl(base, passed=2, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            write_jsonl(cand, passed=3, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            write_jsonl(base_h, passed=3, total=3)
            write_jsonl(cand_h, passed=2, total=3)
            code, out = run_compare(str(base), str(cand), heldout_check=True, base_heldout=str(base_h), cand_heldout=str(cand_h))
            self.assertEqual(code, 0)
            assert_verdict(out, "0", "heldout regressed 2/3 < 3/3 (core: passed 3/5 > 2/5)")

    def test_core_win_fewer_turns_heldout_regression(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            base_h = tmp / "base_heldout.jsonl"
            cand_h = tmp / "cand_heldout.jsonl"
            write_jsonl(base, passed=2, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            write_jsonl(cand, passed=2, total=5, total_py_calls=1, total_turns=8, total_sum_prompt=100, total_gen_tok=50)
            write_jsonl(base_h, passed=2, total=3)
            write_jsonl(cand_h, passed=1, total=3)
            code, out = run_compare(str(base), str(cand), heldout_check=True, base_heldout=str(base_h), cand_heldout=str(cand_h))
            self.assertEqual(code, 0)
            assert_verdict(out, "0", "heldout regressed 1/3 < 2/3 (core: same pass, same py_calls, fewer turns 8 < 10)")

    def test_core_win_fewer_tokens_heldout_unchanged(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            base_h = tmp / "base_heldout.jsonl"
            cand_h = tmp / "cand_heldout.jsonl"
            write_jsonl(base, passed=2, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            write_jsonl(cand, passed=2, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=80, total_gen_tok=40)
            write_jsonl(base_h, passed=2, total=3)
            write_jsonl(cand_h, passed=2, total=3)
            code, out = run_compare(str(base), str(cand), heldout_check=True, base_heldout=str(base_h), cand_heldout=str(cand_h))
            self.assertEqual(code, 0)
            assert_verdict(out, "1", "same pass, same py_calls, same turns, fewer tokens 120 < 150")

    def test_core_tie(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            write_jsonl(base, passed=2, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            write_jsonl(cand, passed=2, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            code, out = run_compare(str(base), str(cand))
            self.assertEqual(code, 0)
            assert_verdict(out, "0", "no metric improved")

    def test_core_py_calls_regression(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            write_jsonl(base, passed=2, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            write_jsonl(cand, passed=2, total=5, total_py_calls=2, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            code, out = run_compare(str(base), str(cand))
            self.assertEqual(code, 0)
            assert_verdict(out, "0", "same pass but py_calls 2 > 1")

    def test_total_task_count_mismatch(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            write_jsonl(base, passed=2, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            write_jsonl(cand, passed=3, total=4, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            code, out = run_compare(str(base), str(cand))
            self.assertEqual(code, 0)
            assert_verdict(out, "0", "total task count mismatch 4 != 5")

    def test_repeated_runs_use_medians(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            write_jsonl_many(
                base,
                [
                    {"passed": 2, "total": 5, "total_py_calls": 1, "total_turns": 10, "total_sum_prompt": 100, "total_gen_tok": 50},
                    {"passed": 2, "total": 5, "total_py_calls": 1, "total_turns": 11, "total_sum_prompt": 110, "total_gen_tok": 50},
                    {"passed": 3, "total": 5, "total_py_calls": 1, "total_turns": 9, "total_sum_prompt": 90, "total_gen_tok": 50},
                ],
            )
            write_jsonl_many(
                cand,
                [
                    {"passed": 3, "total": 5, "total_py_calls": 1, "total_turns": 10, "total_sum_prompt": 100, "total_gen_tok": 50},
                    {"passed": 3, "total": 5, "total_py_calls": 1, "total_turns": 10, "total_sum_prompt": 100, "total_gen_tok": 50},
                    {"passed": 2, "total": 5, "total_py_calls": 1, "total_turns": 10, "total_sum_prompt": 100, "total_gen_tok": 50},
                ],
            )
            code, out = run_compare(str(base), str(cand))
            self.assertEqual(code, 0)
            assert_verdict(out, "1", "passed 3/5 > 2/5")
            self.assertIn("[median of 3 runs]", out)

    def test_repeated_runs_reject_a_lucky_sample(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            write_jsonl_many(
                base,
                [
                    {"passed": 2, "total": 5, "total_py_calls": 1, "total_turns": 10, "total_sum_prompt": 100, "total_gen_tok": 50},
                    {"passed": 2, "total": 5, "total_py_calls": 1, "total_turns": 10, "total_sum_prompt": 100, "total_gen_tok": 50},
                    {"passed": 2, "total": 5, "total_py_calls": 1, "total_turns": 10, "total_sum_prompt": 100, "total_gen_tok": 50},
                ],
            )
            write_jsonl_many(
                cand,
                [
                    {"passed": 3, "total": 5, "total_py_calls": 1, "total_turns": 10, "total_sum_prompt": 100, "total_gen_tok": 50},
                    {"passed": 2, "total": 5, "total_py_calls": 1, "total_turns": 10, "total_sum_prompt": 100, "total_gen_tok": 50},
                    {"passed": 2, "total": 5, "total_py_calls": 1, "total_turns": 10, "total_sum_prompt": 100, "total_gen_tok": 50},
                ],
            )
            code, out = run_compare(str(base), str(cand))
            self.assertEqual(code, 0)
            assert_verdict(out, "0", "no metric improved")
            self.assertIn("[median of 3 runs]", out)

    def test_fewer_py_calls_is_a_win(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            write_jsonl(base, passed=2, total=5, total_py_calls=2, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            write_jsonl(cand, passed=2, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            code, out = run_compare(str(base), str(cand))
            self.assertEqual(code, 0)
            assert_verdict(out, "1", "same pass, fewer py_calls 1 < 2")

    def test_repetition_count_mismatch(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            write_jsonl_many(
                base,
                [
                    {"passed": 2, "total": 5},
                    {"passed": 2, "total": 5},
                ],
            )
            write_jsonl(cand, passed=3, total=5)
            code, out = run_compare(str(base), str(cand))
            self.assertEqual(code, 0)
            assert_verdict(out, "0", "repetition count mismatch 1 != 2")

    def test_compare_requires_heldout_paths(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            write_jsonl(base, passed=2, total=5)
            write_jsonl(cand, passed=3, total=5)
            proc = run_compare_raw(
                ["--heldout-check", "--compare", str(base), str(cand)]
            )
            self.assertEqual(proc.returncode, 1)
            self.assertIn("requires BASE_HELDOUT and CAND_HELDOUT", proc.stderr)

    def test_compare_requires_heldout_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            base_h = tmp / "base_heldout.jsonl"
            write_jsonl(base, passed=2, total=5)
            write_jsonl(cand, passed=3, total=5)
            write_jsonl(base_h, passed=3, total=3)
            proc = run_compare_raw(
                [
                    "--heldout-check",
                    "--compare",
                    str(base),
                    str(cand),
                    str(base_h),
                    str(tmp / "missing.jsonl"),
                ]
            )
            self.assertEqual(proc.returncode, 1)
            self.assertIn("heldout eval record not found", proc.stderr)

    def test_all_suite_rejects_heldout_check(self):
        proc = run_compare_raw(
            ["--eval-suite", "all", "--heldout-check", "--dry-run"]
        )
        self.assertEqual(proc.returncode, 1)
        self.assertIn("already includes the heldout suite", proc.stderr)

    def test_compare_flag_order_heldout_after_compare(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            base = tmp / "base.jsonl"
            cand = tmp / "cand.jsonl"
            base_h = tmp / "base_heldout.jsonl"
            cand_h = tmp / "cand_heldout.jsonl"
            write_jsonl(base, passed=2, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            write_jsonl(cand, passed=3, total=5, total_py_calls=1, total_turns=10, total_sum_prompt=100, total_gen_tok=50)
            write_jsonl(base_h, passed=3, total=3)
            write_jsonl(cand_h, passed=2, total=3)
            proc = run_compare_raw(
                [
                    "--compare",
                    str(base),
                    str(cand),
                    str(base_h),
                    str(cand_h),
                    "--heldout-check",
                ]
            )
            self.assertEqual(proc.returncode, 0)
            assert_verdict(
                proc.stdout,
                "0",
                "heldout regressed 2/3 < 3/3 (core: passed 3/5 > 2/5)",
            )

    def test_non_compare_mode_no_unbound_variable(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            subprocess.run(["git", "init"], cwd=tmp, check=True, capture_output=True)
            scripts_dir = tmp / "scripts"
            scripts_dir.mkdir()
            shutil.copy(SCRIPT, scripts_dir / "improve-loop.sh")
            proc = subprocess.run(
                ["bash", str(scripts_dir / "improve-loop.sh"), "--allow-main"],
                capture_output=True,
                text=True,
            )
            self.assertEqual(proc.returncode, 1)
            self.assertNotIn("unbound variable", proc.stderr)
            self.assertIn("working tree is dirty", proc.stderr)


class LoopIntegrationTest(unittest.TestCase):
    def test_multi_iteration_keep_advances_start_head(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            repo = tmp / "repo"
            repo.mkdir()
            work_dir = tmp / "work"
            stub_bin = tmp / "bin"
            stub_bin.mkdir()

            def git(*args):
                return subprocess.run(
                    ["git", *args],
                    cwd=repo,
                    check=True,
                    capture_output=True,
                    text=True,
                )

            git("init", "-b", "exp/test")
            git("config", "user.name", "Test")
            git("config", "user.email", "test@example.com")

            (repo / ".gitignore").write_text("target/\n", encoding="utf-8")
            (repo / "src").mkdir()
            (repo / "src" / "dummy.txt").write_text("initial\n", encoding="utf-8")
            scripts_dir = repo / "scripts"
            scripts_dir.mkdir()
            shutil.copy(SCRIPT, scripts_dir / "improve-loop.sh")

            eval_sh = scripts_dir / "eval.sh"
            eval_sh.write_text(
                """#!/usr/bin/env bash
# EVAL_ROOT
set -euo pipefail
mkdir -p "$EVAL_SESSIONS_DIR"
state=$(cat "$EVAL_ROOT/src/dummy.txt" | tr -d '\\n')
if [[ "$state" == "win1" ]]; then
  passed=2
elif [[ "$state" == "initial" ]]; then
  passed=1
else
  passed=0
fi
printf '{"passed":%d,"total":2,"total_py_calls":0,"total_turns":2,"total_sum_prompt":100,"total_gen_tok":50,"results":[]}\\n' "$passed" >> "$EVAL_OUT"
""",
                encoding="utf-8",
            )
            eval_sh.chmod(0o755)
            (scripts_dir / "session-digest.py").write_text(
                "import sys\nsys.exit(0)\n", encoding="utf-8"
            )

            git("add", "-A")
            git("commit", "-m", "initial")

            gg_dir = repo / "target" / "debug"
            gg_dir.mkdir(parents=True)
            gg_stub = gg_dir / "gg"
            counter_file = tmp / "gg_calls"
            gg_stub.write_text(
                f"""#!/usr/bin/env bash
n=0
if [[ -f "{counter_file}" ]]; then
  n=$(cat "{counter_file}")
fi
n=$((n + 1))
echo "$n" > "{counter_file}"
if [[ "$n" -eq 1 ]]; then
  printf 'win1\\n' > src/dummy.txt
else
  printf 'lose2\\n' > src/dummy.txt
fi
""",
                encoding="utf-8",
            )
            gg_stub.chmod(0o755)

            cargo_stub = stub_bin / "cargo"
            cargo_stub.write_text(
                """#!/usr/bin/env bash
if [[ "${1:-}" == "test" ]]; then
  echo "test result: ok. 1 passed; 0 failed"
fi
exit 0
""",
                encoding="utf-8",
            )
            cargo_stub.chmod(0o755)

            env = os.environ.copy()
            env["PATH"] = f"{stub_bin}:{env.get('PATH', '')}"
            env["GG_BIN"] = str(gg_stub)
            proc = subprocess.run(
                [
                    "bash",
                    str(scripts_dir / "improve-loop.sh"),
                    "--iterations",
                    "2",
                    "--repetitions",
                    "1",
                    "--work-dir",
                    str(work_dir),
                ],
                cwd=repo,
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(proc.returncode, 0, msg=proc.stderr + proc.stdout)

            ledger = [
                json.loads(line)
                for line in (work_dir / "ledger.jsonl").read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            self.assertEqual([entry["outcome"] for entry in ledger], ["keep", "revert"])
            rev_count = git("rev-list", "--count", "HEAD").stdout.strip()
            self.assertEqual(rev_count, "2")
            self.assertEqual((repo / "src" / "dummy.txt").read_text(encoding="utf-8"), "win1\n")


if __name__ == "__main__":
    unittest.main()
