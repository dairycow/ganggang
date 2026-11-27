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
  cut) echo "warning: response truncated by max_tokens=100 before producing visible output; stopping instead of continuing" >&2 ;;
  budget) echo "warning: reached max_turns (40); stopping run" >&2 ;;
  noise) echo '▸ bash grep -n max_seconds src/agent' >&2; echo "some progress" >&2 ;;
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

    def records(self, path):
        """The job log's tab-separated records, as lists of fields."""
        return [line.split("\t") for line in path.read_text(encoding="utf-8").splitlines()]

    def test_one_job_keeps_its_files_and_reports_the_answer(self):
        proc = self.delegate("--job", "a", "--prompt", str(self.prompt))
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("job a: ok | 11 bytes | answer: a.out | progress: a.err | session: a.jsonl", proc.stdout)
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

    def test_a_no_answer_says_why(self):
        # The detail is read from NAME.err: a response cut off by max_tokens,
        # a budget gg names, or a pointer to the progress file. A command line
        # in the file that names a budget is not gg's warning.
        cases = {
            "empty": "read a.err",
            "cut": "cut off by max_tokens before any visible output",
            "budget": "reached its turn or time budget",
            "noise": "read a.err",
        }
        for mode, detail in cases.items():
            with self.subTest(mode=mode):
                proc = self.delegate("--job", "a", "--prompt", str(self.prompt), env=self.env(STUB_MODE=mode))
                self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
                self.assertIn(
                    f"job a: NO ANSWER | {detail} | answer: a.out | progress: a.err | session: a.jsonl",
                    proc.stdout,
                )

    def test_the_jobs_own_exit_status_is_returned(self):
        log = self.root / "fail.log"
        proc = self.delegate(
            "--job", "a", "--prompt", str(self.prompt), env=self.env(STUB_MODE="fail", GG_JOB_LOG=str(log))
        )
        self.assertEqual(proc.returncode, 3)
        self.assertIn("job a: failed | exit 3 | answer: a.out | progress: a.err | session: a.jsonl", proc.stdout)
        self.assertEqual(self.records(log)[-1], ["done", "a", "3", "failed: exit 3"])

    def test_a_hanging_job_is_stopped_at_its_timeout(self):
        started = time.monotonic()
        log = self.root / "timeout.log"
        proc = self.delegate(
            "--job", "a",
            "--prompt", str(self.prompt),
            "--timeout", "1",
            env=self.env(STUB_MODE="hang", GG_JOB_LOG=str(log)),
        )
        self.assertEqual(proc.returncode, 124, proc.stdout + proc.stderr)
        self.assertIn("job a: timed out | after 1s | answer: a.out | progress: a.err | session: a.jsonl", proc.stdout)
        self.assertLess(time.monotonic() - started, 15)
        self.assertEqual(self.records(log)[-1], ["done", "a", "124", "timed out: after 1s"])

    def test_the_job_log_records_start_and_done(self):
        log = self.root / "job.log"
        directory = self.root / "jobs"
        proc = self.delegate(
            "--job", "a", "--prompt", str(self.prompt), "--dir", str(directory),
            env=self.env(GG_JOB_LOG=str(log)),
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        pid = (directory / "a.pid").read_text(encoding="utf-8").strip()
        self.assertEqual(
            self.records(log),
            [["start", "a", pid, str(directory)], ["done", "a", "0", "ok"]],
        )

    def test_the_job_log_notes_a_no_answer(self):
        log = self.root / "job.log"
        proc = self.delegate(
            "--job", "a", "--prompt", str(self.prompt),
            env=self.env(STUB_MODE="budget", GG_JOB_LOG=str(log)),
        )
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertEqual(
            self.records(log)[-1],
            ["done", "a", "1", "NO ANSWER: reached its turn or time budget"],
        )

    def test_without_gg_job_log_no_log_is_written(self):
        directory = self.root / "jobs"
        proc = self.delegate("--job", "a", "--prompt", str(self.prompt), "--dir", str(directory))
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual(list(directory.glob("*.log")), [])

    def test_a_job_log_with_a_missing_directory_is_ignored(self):
        # The log must not fail the job, and nothing is created.
        proc = self.delegate(
            "--job", "a", "--prompt", str(self.prompt),
            env=self.env(GG_JOB_LOG=str(self.root / "nope" / "job.log")),
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertFalse((self.root / "nope").exists())

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
        # A nested run's bash tool marks its commands with the depth past the
        # limit, 2; gg would refuse to boot from there as well.
        for depth in ("2", "3"):
            with self.subTest(depth=depth):
                proc = self.delegate(
                    "--job", "a", "--prompt", str(self.prompt), env=self.env(GG_NEST_DEPTH=depth)
                )
                self.assertEqual(proc.returncode, 2)
                self.assertIn("already a nested run", proc.stderr)
                self.assertFalse((self.stub_dir / "argv").exists(), "gg must not be started")

    def test_a_top_level_run_can_delegate(self):
        # The bash tool of a top-level run marks its commands GG_NEST_DEPTH=1:
        # the depth a gg started there has. That is the one run that may
        # delegate, so the job starts.
        directory = self.root / "jobs"
        proc = self.delegate(
            "--job", "a",
            "--prompt", str(self.prompt),
            "--dir", str(directory),
            env=self.env(GG_NEST_DEPTH="1"),
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual((directory / "a.out").read_text(), "the answer\n")

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
