#!/usr/bin/env python3
"""Tests for .agents/skills/orchestrator/orchestrator.sh, the orchestrator skill's git helper.

Run with ``python3 scripts/test-orchestrator.py``.

A temporary git repository stands in for the developer's main checkout.
The ``gg`` on PATH is a stub that records how it was called and, in
``commit`` mode, commits a file in the worktree it starts in, as a job
would. ``scripts/pre-commit-check.sh`` in that repository is a stub too,
so no test runs cargo.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / ".agents" / "skills" / "orchestrator" / "orchestrator.sh"

STUB = """#!/usr/bin/env bash
printf '%s\\0' "$@" > "$STUB_DIR/argv"
cat > "$STUB_DIR/stdin"
case "${STUB_MODE:-ok}" in
  ok) echo "the answer" ;;
  commit) echo "${STUB_MSG:-job}" >> job.txt && git add job.txt && git commit -q -m "${STUB_MSG:-job}" && echo "the answer" ;;
esac
"""

GATE = """#!/usr/bin/env bash
# A stand-in for the commit gate: no cargo runs in the tests.
echo "stub gate in $PWD"
case "${GATE_MODE:-ok}" in
  ok) ;;
  fail) echo "pre-commit: FAILED test"; exit 1 ;;
  move) git commit -q --allow-empty -m "mid-gate" ;;
esac
"""


class OrchestratorTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="gg-orchestrator-"))
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.home = self.root / "home"
        self.bin = self.root / "bin"
        self.stub_dir = self.root / "stub"
        self.main = self.root / "proj"  # the developer's main checkout
        self.outside = self.root / "outside"  # in no repository
        self.jobs = self.root / "jobs"
        for path in (self.home, self.bin, self.stub_dir, self.outside, self.jobs):
            path.mkdir(parents=True)
        self.write(self.bin / "gg", STUB)
        self.prompt = self.root / "task.md"
        self.prompt.write_text("do the thing\n", encoding="utf-8")
        self.git("init", "-q", "-b", "main", str(self.main), cwd=self.root)
        for key, value in (("user.name", "Test"), ("user.email", "test@example.com"), ("commit.gpgsign", "false")):
            self.git("config", key, value)
        (self.main / "scripts").mkdir()
        self.write(self.main / "scripts" / "pre-commit-check.sh", GATE)
        (self.main / "README.md").write_text("readme\n", encoding="utf-8")
        self.git("add", "-A")
        self.git("commit", "-qm", "init")

    def write(self, path, text):
        path.write_text(text, encoding="utf-8")
        path.chmod(0o755)

    def env(self, **extra):
        """A clean environment: no GG_* or GIT_* from the shell that runs the tests."""
        env = {k: v for k, v in os.environ.items() if not k.startswith(("GG_", "GIT_"))}
        env["HOME"] = str(self.home)
        env["PATH"] = f"{self.bin}{os.pathsep}{env.get('PATH', '')}"
        env["STUB_DIR"] = str(self.stub_dir)
        env["GIT_CONFIG_GLOBAL"] = os.devnull
        env["GIT_CONFIG_NOSYSTEM"] = "1"
        env.update({k: str(v) for k, v in extra.items()})
        return env

    def git(self, *args, cwd=None):
        proc = subprocess.run(
            ["git", *args], cwd=cwd or self.main, env=self.env(), capture_output=True, text=True
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        return proc.stdout.strip()

    def orch(self, *args, cwd=None, env=None, timeout=20):
        return subprocess.run(
            [str(SCRIPT), *args],
            cwd=cwd or self.main,
            env=env or self.env(),
            capture_output=True,
            text=True,
            timeout=timeout,
        )

    def new(self, name, *options):
        proc = self.orch("new", name, *options)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        return proc

    def wt(self, name):
        return self.main / ".worktrees" / name

    def work(self, name, msg="job"):
        """A stub run: the job commits job.txt in its worktree."""
        proc = self.orch(
            "run", name, "--prompt", str(self.prompt), "--dir", str(self.jobs),
            env=self.env(STUB_MODE="commit", STUB_MSG=msg),
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)

    def gate(self, name, **mode):
        return self.orch("gate", name, env=self.env(**mode))

    def head(self, name):
        return self.git("rev-parse", "HEAD", cwd=self.wt(name))

    def recorded(self, branch):
        """The commit gate recorded for the branch, or None."""
        proc = subprocess.run(
            ["git", "rev-parse", "--verify", "-q", f"refs/gg/orchestrator/{branch}"],
            cwd=self.main,
            env=self.env(),
            capture_output=True,
            text=True,
        )
        return proc.stdout.strip() or None

    def move_main(self, text="main", name="main.txt"):
        (self.main / name).write_text(text + "\n", encoding="utf-8")
        self.git("add", name)
        self.git("commit", "-qm", f"main: {name}")

    def line(self, name):
        """The one line `list` prints for NAME."""
        proc = self.orch("list")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        lines = [line for line in proc.stdout.splitlines() if line.split()[:1] == [name]]
        self.assertEqual(len(lines), 1, proc.stdout)
        return lines[0]

    def refused(self, proc, *messages):
        """Exit 2, a message of its own, and nothing done."""
        self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
        self.assertIn("orchestrator:", proc.stderr)
        for message in messages:
            self.assertIn(message, proc.stderr)

    def argv(self):
        path = self.stub_dir / "argv"
        return [p for p in path.read_text(encoding="utf-8").split("\0") if p] if path.exists() else None

    def test_the_whole_path_lands_a_job_and_drops_its_worktree(self):
        # The main checkout keeps unrelated uncommitted work throughout.
        (self.main / "scratch.txt").write_text("mine\n", encoding="utf-8")
        (self.main / "README.md").write_text("edited by the developer\n", encoding="utf-8")
        before = self.main_head()

        proc = self.new("a", "-b", "feat/a")
        self.assertIn(f"worktree a at {self.wt('a')}, on feat/a", proc.stdout)
        self.assertEqual(self.git("rev-parse", "--abbrev-ref", "HEAD", cwd=self.wt("a")), "feat/a")
        self.assertEqual(self.head("a"), before)
        # The worktrees stay out of the main checkout's status.
        self.assertIn(".worktrees/", (self.main / ".git" / "info" / "exclude").read_text())
        self.assertEqual(self.git("status", "--porcelain", "--untracked-files=all", "--", ".worktrees"), "")
        path = re.escape(str(self.wt("a")))
        # A fresh worktree is at main's tip, so it counts as merged: it has
        # nothing to land.
        self.assertRegex(self.line("a"), rf"^a\s+feat/a\s+behind 0\s+ahead 0\s+clean\s+merged\s+{path}$")

        self.work("a", "job a")
        self.assertEqual((self.wt("a") / "job.txt").read_text(), "job a\n")
        self.assertFalse((self.main / "job.txt").exists())
        self.assertEqual(len(self.git("log", "--oneline", "main..HEAD", cwd=self.wt("a")).splitlines()), 1)
        self.assertEqual((self.jobs / "a.out").read_text(), "the answer\n")
        self.assertEqual(self.argv()[:2], ["-p", "-"])

        proc = self.gate("a")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("gate passed for a", proc.stdout)
        self.assertEqual(self.recorded("feat/a"), self.head("a"))
        self.assertRegex(self.line("a"), rf"^a\s+feat/a\s+behind 0\s+ahead 1\s+clean\s+gated\s+{path}$")

        proc = self.orch("merge", "a")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual(self.main_head(), self.recorded("feat/a"))
        self.assertEqual((self.main / "job.txt").read_text(), "job a\n")
        self.assertEqual(self.git("diff", "--name-only"), "README.md")
        self.assertEqual(self.git("ls-files", "--others", "--exclude-standard"), "scratch.txt")
        self.assertRegex(self.line("a"), rf"^a\s+feat/a\s+behind 0\s+ahead 0\s+clean\s+merged gated\s+{path}$")

        proc = self.orch("drop", "a")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertFalse(self.wt("a").exists())
        self.assertEqual(self.git("branch", "--list", "feat/a"), "")
        self.assertIsNone(self.recorded("feat/a"))
        self.assertEqual(self.orch("list").stdout.splitlines(), ["orchestrator: worktrees against main"])

    def main_head(self):
        return self.git("rev-parse", "main")

    def test_update_after_main_moved_then_gate_and_merge(self):
        self.new("b")
        self.work("b", "job b")
        self.assertEqual(self.gate("b").returncode, 0)
        self.move_main()
        moved = self.main_head()
        self.refused(self.orch("merge", "b"), "main moved since b branched", "update b")
        self.assertEqual(self.main_head(), moved)

        proc = self.orch("update", "b")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("now has main", proc.stdout)
        self.assertEqual(self.git("merge-base", "main", "HEAD", cwd=self.wt("b")), moved)
        self.assertTrue((self.wt("b") / "main.txt").exists())
        self.assertEqual(len(self.git("log", "--oneline", "--merges", "main..HEAD", cwd=self.wt("b")).splitlines()), 1)
        # The merge commit ran no hook, so the gate's record no longer
        # covers the tip: update must be followed by gate.
        self.refused(self.orch("merge", "b"), "moved since the gate")
        self.assertEqual(self.gate("b").returncode, 0)
        proc = self.orch("merge", "b")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual(self.main_head(), self.head("b"))
        landed = self.git("log", "--oneline", f"{moved}..main")
        self.assertIn("job b", landed)
        self.assertIn("Merge branch 'main' into feat/b", landed)
        self.assertEqual(self.orch("drop", "b").returncode, 0)

    def test_update_leaves_conflicts_for_the_job(self):
        self.new("c")
        self.work("c")
        self.move_main(text="main", name="job.txt")
        proc = self.orch("update", "c")
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("conflicts with main", proc.stderr)
        status = self.git("status", "--porcelain", cwd=self.wt("c"))
        self.assertTrue(status.endswith("job.txt") and status[:2] in {"AA", "UU", "AU", "UA", "DD"}, status)
        self.refused(self.gate("c"), "has changes")
        # The job resolves and commits; then the usual gate and merge.
        (self.wt("c") / "job.txt").write_text("resolved\n", encoding="utf-8")
        self.git("add", "job.txt", cwd=self.wt("c"))
        self.git("commit", "-qm", "resolve", cwd=self.wt("c"))
        self.assertEqual(self.gate("c").returncode, 0)
        self.assertEqual(self.orch("merge", "c").returncode, 0)
        self.assertEqual((self.main / "job.txt").read_text(), "resolved\n")

    def test_update_with_main_unmoved_changes_nothing(self):
        self.new("d")
        self.work("d")
        tip = self.head("d")
        proc = self.orch("update", "d")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("already has main", proc.stdout)
        self.assertEqual(self.head("d"), tip)

    def test_new_refusals(self):
        for args in (
            ["new"],
            ["new", "../escape"],
            ["new", "a/b"],
            ["new", "."],
            ["new", ".."],
            ["new", "e", "--nope"],
            ["new", "e", "extra"],
            ["new", "e", "-b"],
            ["new", "e", "--from"],
            ["new", "e", "-b", "bad..name"],
            ["new", "e", "--from", "nope"],
        ):
            with self.subTest(args=args):
                proc = self.orch(*args)
                self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
                self.assertIn("orchestrator:", proc.stderr)
        self.new("e", "-b", "feat/e")
        self.refused(self.orch("new", "e"), "already exists")
        self.refused(self.orch("new", "f", "-b", "feat/e"), "branch feat/e already exists")
        # The worktree is cut from the given commit; the branch defaults to feat/NAME.
        first = self.git("rev-list", "--max-parents=0", "HEAD")
        self.new("old", "--from", first)
        self.assertEqual(self.head("old"), first)
        proc = self.new("plain")
        self.assertEqual(self.git("rev-parse", "--abbrev-ref", "HEAD", cwd=self.wt("plain")), "feat/plain")
        self.assertIn("on feat/plain", proc.stdout)
        # A repository without a main branch has nothing to cut from.
        other = self.root / "other"
        self.git("init", "-q", "-b", "master", str(other))
        self.refused(self.orch("new", "x", cwd=other), "main is not a commit")
        self.assertEqual(sorted(self.git("branch", "--format=%(refname:short)").split()), ["feat/e", "feat/old", "feat/plain", "main"])

    def test_run_refusals(self):
        self.refused(self.orch("run", "nope"), "no worktree nope")
        # A plain directory under .worktrees/ is no worktree: git -C there
        # would reach the main checkout.
        (self.main / ".worktrees" / "fake").mkdir(parents=True)
        self.refused(self.orch("run", "fake"), "no worktree fake")
        self.new("ok")
        proc = self.orch("run", "ok")
        self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
        self.assertIn("delegate: --prompt is required", proc.stderr)
        self.assertIsNone(self.argv(), "gg must not be started")
        # NAME wins over a --job of the caller's; the other options pass through.
        proc = self.orch(
            "run", "ok", "--job", "other", "--prompt", str(self.prompt),
            "--dir", str(self.jobs), "--max-turns", "5",
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertTrue((self.jobs / "ok.out").exists())
        self.assertFalse((self.jobs / "other.out").exists())
        self.assertEqual(self.argv()[self.argv().index("--max-turns") + 1], "5")

    def test_gate_refusals(self):
        self.refused(self.orch("gate", "nope"), "no worktree nope")
        self.new("g")
        (self.wt("g") / "x.txt").write_text("x\n", encoding="utf-8")
        self.refused(self.gate("g"), "has changes")
        (self.wt("g") / "x.txt").unlink()
        self.git("checkout", "-q", "--detach", cwd=self.wt("g"))
        self.refused(self.gate("g"), "detached")
        self.new("h")
        self.git("rm", "-q", "scripts/pre-commit-check.sh", cwd=self.wt("h"))
        self.git("commit", "-qm", "no gate", cwd=self.wt("h"))
        self.refused(self.gate("h"), "no executable scripts/pre-commit-check.sh")
        self.new("i")
        self.work("i")
        proc = self.gate("i", GATE_MODE="fail")
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("gate FAILED for i", proc.stderr)
        self.assertIsNone(self.recorded("feat/i"))
        proc = self.gate("i", GATE_MODE="move")
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("moved during the gate", proc.stderr)
        self.assertIsNone(self.recorded("feat/i"))
        self.refused(self.orch("merge", "i"), "no gate recorded")

    def test_merge_refusals(self):
        self.refused(self.orch("merge", "nope"), "no worktree nope")
        self.new("m")
        self.work("m")
        self.assertEqual(self.gate("m").returncode, 0)
        before = self.main_head()
        self.git("checkout", "-q", "-b", "side")
        self.refused(self.orch("merge", "m"), "the main checkout is on side")
        self.git("checkout", "-q", "--detach")
        self.refused(self.orch("merge", "m"), "the main checkout is on (detached)")
        self.git("checkout", "-q", "main")
        (self.main / "staged.txt").write_text("s\n", encoding="utf-8")
        self.git("add", "staged.txt")
        self.refused(self.orch("merge", "m"), "has staged changes")
        self.git("rm", "-q", "--cached", "staged.txt")
        (self.main / "staged.txt").unlink()
        (self.main / "job.txt").write_text("local\n", encoding="utf-8")
        self.refused(self.orch("merge", "m"), "changes in files the merge would touch")
        self.assertEqual((self.main / "job.txt").read_text(), "local\n")
        (self.main / "job.txt").unlink()
        (self.wt("m") / "x.txt").write_text("x\n", encoding="utf-8")
        self.refused(self.orch("merge", "m"), "has changes")
        (self.wt("m") / "x.txt").unlink()
        self.work("m", "more")
        self.refused(self.orch("merge", "m"), "moved since the gate")
        self.assertEqual(self.main_head(), before)
        # A job with no commits has nothing to land.
        self.new("n")
        proc = self.orch("merge", "n")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("nothing to land", proc.stdout)

    def test_drop_refusals(self):
        self.refused(self.orch("drop", "nope"), "no worktree nope")
        self.new("u")
        self.work("u")
        self.assertEqual(self.gate("u").returncode, 0)
        (self.wt("u") / "x.txt").write_text("x\n", encoding="utf-8")
        self.refused(self.orch("drop", "u"), "has changes")
        (self.wt("u") / "x.txt").unlink()
        self.refused(self.orch("drop", "u"), "not on main yet")
        self.assertTrue(self.wt("u").exists())
        self.assertIn("feat/u", self.git("branch", "--list", "feat/u"))
        self.assertEqual(self.recorded("feat/u"), self.head("u"))
        # The script forces nothing, wherever it is used.
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertNotIn("--force", source)
        self.assertNotIn(" -D ", source)

    def test_drop_deletes_the_branch_with_the_main_checkout_on_another_branch(self):
        self.new("o")
        self.work("o")
        self.assertEqual(self.gate("o").returncode, 0)
        self.assertEqual(self.orch("merge", "o").returncode, 0)
        # A branch that does not contain the job: `branch -d` would keep it.
        self.git("checkout", "-q", "-b", "side", "main~1")
        proc = self.orch("drop", "o")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertFalse(self.wt("o").exists())
        self.assertEqual(self.git("branch", "--list", "feat/o"), "")
        self.assertIsNone(self.recorded("feat/o"))

    def test_list_shows_the_state_of_each_worktree(self):
        self.new("a")
        self.work("a", "job a")
        self.gate("a")
        self.new("b")
        (self.wt("b") / "x.txt").write_text("x\n", encoding="utf-8")
        self.new("c")
        self.work("c", "job c")
        self.gate("c")
        self.assertEqual(self.orch("merge", "c").returncode, 0)
        self.move_main()
        self.git("worktree", "add", "-q", str(self.wt("gone")), "-b", "gone")
        shutil.rmtree(self.wt("gone"))
        self.git("checkout", "-q", "--detach", cwd=self.wt("b"))
        proc = self.orch("list")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        lines = {line.split()[0]: line for line in proc.stdout.splitlines()[1:]}
        self.assertEqual(sorted(lines), ["a", "b", "c", "gone"])
        # main has c's commit and the move on top of where a and b branched.
        self.assertRegex(lines["a"], rf"^a\s+feat/a\s+behind 2\s+ahead 1\s+clean\s+gated\s+{re.escape(str(self.wt('a')))}$")
        self.assertRegex(lines["b"], rf"^b\s+\(detached\)\s+behind 2\s+ahead 0\s+dirty\s+merged\s+{re.escape(str(self.wt('b')))}$")
        self.assertRegex(lines["c"], rf"^c\s+feat/c\s+behind 1\s+ahead 0\s+clean\s+merged gated\s+{re.escape(str(self.wt('c')))}$")
        self.assertRegex(lines["gone"], rf"^gone\s+stale\s+{re.escape(str(self.wt('gone')))}$")

    def test_outside_a_repository_it_refuses(self):
        for args in (["list"], ["new", "x"], ["drop", "x"]):
            with self.subTest(args=args):
                self.refused(self.orch(*args, cwd=self.outside), "not inside a git repository")
        bare = self.root / "bare.git"
        subprocess.run(["git", "init", "-q", "--bare", str(bare)], env=self.env(), check=True)
        self.refused(self.orch("list", cwd=bare), "bare repository")

    def test_the_script_passes_bash_n_and_is_executable(self):
        proc = subprocess.run(["bash", "-n", str(SCRIPT)], capture_output=True, text=True)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(os.access(SCRIPT, os.X_OK))


if __name__ == "__main__":
    unittest.main()
