#!/usr/bin/env python3
"""Tests for .agents/skills/delegate/delegate.sh, the delegation helper.

Run with ``python3 scripts/test-delegate.py``.

The ``gg`` on PATH is a stub that records how it was called and answers
from ``STUB_MODE``, so no model is contacted.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / ".agents" / "skills" / "delegate" / "delegate.sh"

STUB = """#!/usr/bin/env bash
printf '%s\\0' "$@" > "$STUB_DIR/argv"
cat > "$STUB_DIR/stdin"
case "${STUB_MODE:-ok}" in
  ok) echo "the answer" ;;
  empty) echo "some progress" >&2 ;;
  fail) echo "something went wrong" >&2; exit 3 ;;
  hang) sleep 30 ;;
esac
"""


class DelegateTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="gg-delegate-"))
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.home = self.root / "home"
        self.work = self.root / "work" / "proj"
        self.stub_dir = self.root / "stub"
        for path in (self.home, self.work, self.stub_dir, self.root / "bin"):
            path.mkdir(parents=True)
        self.write_stub(self.root / "bin" / "gg")
        self.prompt = self.work / "task.md"
        self.prompt.write_text("do the thing\n", encoding="utf-8")

    def write_stub(self, path):
        path.write_text(STUB, encoding="utf-8")
        path.chmod(0o755)

    def env(self, home=None, **extra):
        """A clean environment: no GG_* from the shell that runs the tests."""
        env = {k: v for k, v in os.environ.items() if not k.startswith("GG_")}
        env["HOME"] = str(home or self.home)
        env["PATH"] = f"{self.root / 'bin'}{os.pathsep}{env.get('PATH', '')}"
        env["STUB_DIR"] = str(self.stub_dir)
        env.update({k: str(v) for k, v in extra.items()})
        return env

    def delegate(self, *args, env=None, timeout=20):
        return subprocess.run(
            ["bash", str(SCRIPT), *args],
            cwd=self.work,
            env=env or self.env(),
            capture_output=True,
            text=True,
            timeout=timeout,
        )

    def argv(self):
        return [
            part
            for part in (self.stub_dir / "argv").read_text(encoding="utf-8").split("\0")
            if part
        ]

    def job_dir(self, proc):
        for line in proc.stdout.splitlines():
            if line.startswith("delegate dir: "):
                return Path(line.removeprefix("delegate dir: "))
        self.fail(f"no directory line in: {proc.stdout!r}")

    def test_one_job_keeps_its_files_and_reports_the_answer(self):
        proc = self.delegate("--job", "a", "--prompt", str(self.prompt))
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("job a: exit 0 | answer: a.out (", proc.stdout)
        directory = self.job_dir(proc)
        # The default directory is under HOME, one per calling shell,
        # keyed by the cwd slug gg's session store uses.
        self.assertEqual(
            directory,
            self.home / ".gg" / "delegate" / str(self.work).replace("/", "-") / str(os.getpid()),
        )
        self.assertEqual((directory / "a.out").read_text(), "the answer\n")
        self.assertEqual((directory / "a.prompt.md").read_text(), "do the thing\n")

    def test_gg_runs_headless_with_the_prompt_on_stdin_and_its_own_session(self):
        directory = self.root / "jobs"
        proc = self.delegate("--job", "a", "--prompt", str(self.prompt), "--dir", str(directory))
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        argv = self.argv()
        self.assertEqual(argv[:2], ["-p", "-"])
        self.assertIn("--session-file", argv)
        at = argv.index("--session-file")
        self.assertEqual(argv[at + 1], str(directory / "a.jsonl"))
        # Never hooks off, never without a session: the job is auditable.
        self.assertNotIn("--no-hooks", argv)
        self.assertNotIn("--no-session", argv)
        self.assertEqual((self.stub_dir / "stdin").read_text(), "do the thing\n")

    def test_model_tools_and_turns_pass_through(self):
        proc = self.delegate(
            "--job", "a",
            "--prompt", str(self.prompt),
            "--dir", str(self.root / "jobs"),
            "--model", "provider/model",
            "--tools", "read,grep",
            "--max-turns", "5",
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        argv = self.argv()
        self.assertEqual(argv[argv.index("-m") + 1], "provider/model")
        self.assertEqual(argv[argv.index("--tools") + 1], "read,grep")
        self.assertEqual(argv[argv.index("--max-turns") + 1], "5")
        self.assertNotIn("--max-seconds", argv)

    def test_a_timeout_gives_the_job_a_time_budget_ten_seconds_short(self):
        # The budget ends the run before the watchdog terms the job, so the
        # answer reaches NAME.out. A `sleep` that waits at most 2 s stands
        # in: the watchdog's `sleep 60` outlives the job, and would hold
        # this test's pipe open until it ended.
        shim = self.root / "bin" / "sleep"
        shim.write_text('#!/usr/bin/env bash\nd=$1; (( d > 2 )) && d=2\nexec /bin/sleep "$d"\n', encoding="utf-8")
        shim.chmod(0o755)
        proc = self.delegate(
            "--job", "a", "--prompt", str(self.prompt), "--timeout", "60",
            "--dir", str(self.root / "jobs"),
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        argv = self.argv()
        self.assertEqual(argv[argv.index("--max-seconds") + 1], "50")

    def test_a_short_timeout_sets_no_time_budget(self):
        # Too short to leave room for a wrap-up: twenty seconds is the least.
        proc = self.delegate(
            "--job", "a", "--prompt", str(self.prompt), "--timeout", "1",
            env=self.env(STUB_MODE="empty"),
        )
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertNotIn("--max-seconds", self.argv())

    def test_a_job_without_an_answer_says_so(self):
        proc = self.delegate(
            "--job", "a", "--prompt", str(self.prompt), env=self.env(STUB_MODE="empty")
        )
        self.assertEqual(proc.returncode, 1)
        self.assertIn("job a: NO ANSWER", proc.stdout)
        self.assertEqual((self.job_dir(proc) / "a.err").read_text(), "some progress\n")

    def test_the_jobs_own_exit_status_is_returned(self):
        proc = self.delegate(
            "--job", "a", "--prompt", str(self.prompt), env=self.env(STUB_MODE="fail")
        )
        self.assertEqual(proc.returncode, 3)
        self.assertIn("job a: exit 3", proc.stdout)

    def test_a_hanging_job_is_stopped_at_its_timeout(self):
        started = time.monotonic()
        proc = self.delegate(
            "--job", "a",
            "--prompt", str(self.prompt),
            "--timeout", "1",
            env=self.env(STUB_MODE="hang"),
        )
        self.assertEqual(proc.returncode, 124, proc.stdout + proc.stderr)
        self.assertIn("job a: timed out after 1s", proc.stdout)
        self.assertLess(time.monotonic() - started, 15)

    def test_bad_names_and_missing_prompts_are_refused(self):
        for args in (
            ["--prompt", str(self.prompt)],
            ["--job", "../escape", "--prompt", str(self.prompt)],
            ["--job", "a/b", "--prompt", str(self.prompt)],
            ["--job", "."],
            ["--job", "a"],
            ["--job", "a", "--prompt", str(self.work / "missing.md")],
            ["--job", "a", "--prompt", str(self.prompt), "--timeout", "soon"],
            ["--job", "a", "--prompt", str(self.prompt), "--nope"],
        ):
            with self.subTest(args=args):
                proc = self.delegate(*args)
                self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
                self.assertIn("delegate:", proc.stderr)
        self.assertFalse((self.stub_dir / "argv").exists())

    def test_a_nested_run_cannot_delegate(self):
        proc = self.delegate(
            "--job", "a", "--prompt", str(self.prompt), env=self.env(GG_NEST_DEPTH="1")
        )
        self.assertEqual(proc.returncode, 2)
        self.assertIn("already a nested run", proc.stderr)
        self.assertFalse((self.stub_dir / "argv").exists(), "gg must not be started")

    def test_jobs_from_one_shell_share_a_directory(self):
        jobs = " & ".join(
            f'bash "{SCRIPT}" --job {name} --prompt "{self.prompt}"' for name in ("a", "b")
        )
        # One `bash -c` stands in for the bash tool's call.
        proc = subprocess.run(
            ["bash", "-c", f"{jobs} & wait"],
            cwd=self.work,
            env=self.env(),
            capture_output=True,
            text=True,
            timeout=20,
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        directories = {line for line in proc.stdout.splitlines() if line.startswith("delegate dir: ")}
        self.assertEqual(len(directories), 1, proc.stdout)
        directory = Path(directories.pop().removeprefix("delegate dir: "))
        self.assertEqual((directory / "a.out").read_text(), "the answer\n")
        self.assertEqual((directory / "b.out").read_text(), "the answer\n")
        self.assertEqual(len(list(directory.glob("*.prompt.md"))), 2)

    def test_the_root_directory_and_the_binary_can_be_chosen(self):
        root = self.root / "elsewhere"
        other = self.root / "other-gg"
        self.write_stub(other)
        proc = self.delegate(
            "--job", "a",
            "--prompt", str(self.prompt),
            env=self.env(GG=str(other), GG_DELEGATE_DIR=str(root)),
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual(self.job_dir(proc).parent.parent, root)
        self.assertTrue((root).is_dir())


if __name__ == "__main__":
    unittest.main()
