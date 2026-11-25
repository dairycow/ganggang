#!/usr/bin/env python3
"""Tests for scripts/gg-commit-gate.sh, the generic gg pre_tool commit gate.

Run with ``python3 scripts/test-commit-gate.py``.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "gg-commit-gate.sh"

HOOK = """#!/usr/bin/env bash
echo "pre-commit: running"
if [ -f "$(git rev-parse --show-toplevel)/FAIL" ]; then
  echo "pre-commit: FAILED test"
  exit 1
fi
exit 0
"""


def run_gate(stdin, cwd=None):
    """Invoke the gate script with raw stdin and return the completed process."""
    return subprocess.run(
        ["bash", str(SCRIPT)],
        input=stdin,
        capture_output=True,
        text=True,
        cwd=cwd,
    )


def hook_payload(command, cwd):
    """Build the pre_tool hook JSON object for a bash command."""
    return json.dumps(
        {
            "version": 1,
            "event": "pre_tool",
            "cwd": cwd,
            "session_id": "test",
            "tool": {"id": "call_1", "name": "bash", "args": {"command": command}},
        }
    )


class CommitGateTest(unittest.TestCase):
    def setUp(self):
        self.repo = Path(tempfile.mkdtemp(prefix="gg-commit-gate-"))
        self.outside = Path(tempfile.mkdtemp(prefix="gg-commit-gate-outside-"))
        self.addCleanup(self._cleanup)
        self.git("init", "-q")
        self.git("config", "user.name", "test")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "core.hooksPath", ".githooks")
        hooks = self.repo / ".githooks"
        hooks.mkdir()
        hook = hooks / "pre-commit"
        hook.write_text(HOOK, encoding="utf-8")
        hook.chmod(0o755)

    def _cleanup(self):
        for path in (self.repo, self.outside):
            shutil.rmtree(path, ignore_errors=True)

    def git(self, *args):
        subprocess.run(
            ["git", *args],
            cwd=self.repo,
            check=True,
            capture_output=True,
        )

    def fail_hook(self):
        (self.repo / "FAIL").write_text("fail\n", encoding="utf-8")

    def test_non_commit_command_is_allowed(self):
        proc = run_gate(hook_payload("ls -la", str(self.repo)))
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout, "")

    def test_passing_commit_is_allowed(self):
        proc = run_gate(hook_payload("git commit -m x", str(self.repo)))
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout, "")

    def test_failing_commit_is_denied(self):
        self.fail_hook()
        proc = run_gate(hook_payload("git commit -m x", str(self.repo)))
        self.assertEqual(proc.returncode, 0)
        decision = json.loads(proc.stdout)
        self.assertEqual(decision["decision"], "deny")
        self.assertIn("pre-commit: FAILED test", decision["reason"])

    def test_no_verify_is_denied(self):
        proc = run_gate(hook_payload("git commit --no-verify -m x", str(self.repo)))
        self.assertEqual(proc.returncode, 0)
        decision = json.loads(proc.stdout)
        self.assertEqual(decision["decision"], "deny")
        self.assertIn("--no-verify", decision["reason"])

    def test_short_n_flag_is_denied(self):
        proc = run_gate(hook_payload("git commit -n -m x", str(self.repo)))
        self.assertEqual(proc.returncode, 0)
        decision = json.loads(proc.stdout)
        self.assertEqual(decision["decision"], "deny")
        self.assertIn("--no-verify", decision["reason"])

    def test_git_dash_c_commit_is_detected(self):
        self.fail_hook()
        proc = run_gate(hook_payload("git -C . commit -m x", str(self.repo)))
        self.assertEqual(proc.returncode, 0)
        decision = json.loads(proc.stdout)
        self.assertEqual(decision["decision"], "deny")
        self.assertIn("pre-commit: FAILED test", decision["reason"])

    def test_quoted_commit_mention_is_not_detected(self):
        proc = run_gate(hook_payload('echo "git commit"', str(self.repo)))
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout, "")

    def test_cwd_outside_git_is_allowed(self):
        proc = run_gate(hook_payload("git commit -m x", str(self.outside)))
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout, "")

    def test_malformed_stdin_is_allowed(self):
        proc = run_gate("not json {")
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout, "")


if __name__ == "__main__":
    unittest.main()
