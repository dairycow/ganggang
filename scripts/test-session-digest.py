#!/usr/bin/env python3
"""Tests for scripts/session-digest.py.

Run with ``python3 scripts/test-session-digest.py``.
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
session_digest = importlib.import_module("session-digest")


def run_main(args):
    """Run main(args), capturing stdout and returning (exit_code, text)."""
    buffer = io.StringIO()
    with contextlib.redirect_stdout(buffer):
        code = session_digest.main(args)
    return code, buffer.getvalue()


def write_session(directory, stamp, session_id, entries, raw_tail=None):
    """Write a session file and return its path."""
    header = {
        "type": "session",
        "id": session_id,
        "timestamp": f"2026-11-25T{stamp[8:]}:00Z" if len(stamp) == 15 else stamp,
        "model": "test-model",
        "cwd": "/home/hf/Projects/ganggang",
    }
    lines = [json.dumps(header)]
    lines.extend(json.dumps(entry) for entry in entries)
    if raw_tail is not None:
        lines.append(raw_tail)
    path = directory / f"{stamp}-abcdef01.jsonl"
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return path


def simple_session(directory, stamp, session_id):
    """A minimal session with no friction."""
    entries = [
        {"type": "message", "id": "u1", "message": {"role": "user", "content": "hi"}},
        {"type": "message", "id": "a1", "message": {"role": "assistant", "content": "hello"}},
    ]
    return write_session(directory, stamp, session_id, entries)


class SessionDigestTest(unittest.TestCase):
    def test_full_synthetic_session(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            cwd = "/home/hf/Projects/ganggang"
            entries = [
                {"type": "message", "id": "e1", "message": {"role": "user", "content": "read the file"}},
                {
                    "type": "message",
                    "id": "e2",
                    "message": {
                        "role": "assistant",
                        "content": None,
                        "tool_calls": [
                            {"id": "call_1", "name": "read", "args": {"path": "/a"}},
                            {"id": "call_2", "name": "read", "args": {"path": "/a"}},
                            {"id": "call_3", "name": "bash", "args": {"command": "cat /a"}},
                        ],
                        "usage": {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110},
                    },
                },
                {"type": "message", "id": "e3", "message": {"role": "tool_result", "tool_call_id": "call_1", "content": "No such file", "is_error": True}},
                {"type": "message", "id": "e4", "message": {"role": "tool_result", "tool_call_id": "call_2", "content": "file contents", "is_error": False}},
                {"type": "message", "id": "e5", "message": {"role": "tool_result", "tool_call_id": "call_3", "content": "output", "is_error": False}},
                {"type": "message", "id": "e6", "message": {"role": "assistant", "content": "done", "usage": {"prompt_tokens": 200, "completion_tokens": 20, "total_tokens": 220}}},
                {"type": "message", "id": "e7", "message": {"role": "user", "content": "[aborted by user]"}},
                {"type": "message", "id": "e8", "message": {"role": "assistant", "content": None, "tool_calls": [{"id": "call_4", "name": "bash", "args": {"command": "cargo test"}}]}},
                {"type": "message", "id": "e9", "message": {"role": "tool_result", "tool_call_id": "call_4", "content": "aborted before execution", "is_error": True}},
                {"type": "message", "id": "e10", "message": {"role": "user", "content": "no, that's wrong, actually use the other file"}},
                {"type": "compaction", "id": "e11", "first_kept_id": "e6", "summary": "summary"},
            ]
            path = write_session(tmp, "20261125-120000", "aaaaaaaa-bbbb-cccc-dddd-eeeeffff0000", entries)
            code, out = run_main(["--sessions-dir", str(tmp), "--json"])
            self.assertEqual(code, 0)
            payload = json.loads(out)
            self.assertEqual(len(payload["sessions"]), 1)
            session = payload["sessions"][0]
            self.assertEqual(session["session_id"], "aaaaaaaa-bbbb-cccc-dddd-eeeeffff0000")
            self.assertEqual(session["model"], "test-model")
            self.assertEqual(session["cwd"], cwd)
            self.assertEqual(session["entry_count"], 11)
            self.assertEqual(session["skipped_lines"], 0)
            self.assertEqual(session["user_turns"], 3)
            self.assertEqual(session["assistant_turns"], 3)
            self.assertEqual(session["tool_calls"]["total"], 4)
            self.assertEqual(session["tool_calls"]["by_name"], {"read": 2, "bash": 2})
            self.assertEqual(session["tool_errors"]["total"], 2)
            groups = {(g["tool"], g["first_line"]): g["count"] for g in session["tool_errors"]["by_tool_and_first_line"]}
            self.assertEqual(groups, {("read", "No such file"): 1, ("bash", "aborted before execution"): 1})
            self.assertEqual(session["aborts"], 2)
            self.assertEqual(session["compactions"], 1)
            self.assertEqual(session["repeated_reads"], {"/a": 2})
            self.assertEqual(session["bash_workarounds"], 1)
            self.assertEqual(len(session["correction_candidates"]), 1)
            self.assertEqual(session["correction_candidates"][0], "no, that's wrong, actually use the other file")
            self.assertEqual(session["tokens"], {"peak_prompt": 200, "sum_prompt": 300, "sum_completion": 30})
            self.assertEqual(session["friction_score"], 22)
            # aggregate
            self.assertEqual(payload["aggregate"]["sessions"], 1)
            self.assertEqual(payload["aggregate"]["total_friction"], 22)
            self.assertEqual(payload["aggregate"]["total_tool_errors"], 2)
            self.assertEqual(payload["aggregate"]["total_aborts"], 2)
            self.assertEqual(payload["aggregate"]["sum_prompt"], 300)
            self.assertEqual(payload["aggregate"]["sum_completion"], 30)
            self.assertEqual(payload["aggregate"]["peak_prompt"], 200)

    def test_corrupt_trailing_line_tolerance(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            entries = [
                {"type": "message", "id": "e1", "message": {"role": "user", "content": "hi"}},
                {"type": "message", "id": "e2", "message": {"role": "assistant", "content": "hello"}},
            ]
            torn = '{"type": "message", "id": "e3", "message": {"role": "user", "content": "trunc'
            path = write_session(tmp, "20261125-120000", "bbbbbbbb-0000-1111-2222-333344445555", entries, raw_tail=torn)
            code, out = run_main(["--sessions-dir", str(tmp), "--json"])
            self.assertEqual(code, 0)
            payload = json.loads(out)
            session = payload["sessions"][0]
            self.assertEqual(session["skipped_lines"], 1)
            self.assertEqual(session["entry_count"], 2)
            self.assertEqual(session["user_turns"], 1)
            self.assertEqual(session["assistant_turns"], 1)

    def test_secret_redaction(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            entries = [
                {"type": "message", "id": "e1", "message": {"role": "assistant", "content": "ok", "tool_calls": [{"id": "c1", "name": "bash", "args": {"command": "echo hi"}}]}},
                {"type": "message", "id": "e2", "message": {"role": "tool_result", "tool_call_id": "c1", "content": "Authorization: Bearer abc123xyz failed", "is_error": True}},
                {"type": "message", "id": "e3", "message": {"role": "user", "content": "no, use sk-abc1234567890abcdef instead"}},
                {"type": "message", "id": "e4", "message": {"role": "user", "content": "no, token github_pat_abcdefghijklmnopqrstuvwx and glpat-abcdefghijklmnop and eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"}},
                {"type": "message", "id": "e5", "message": {"role": "user", "content": "no, -----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\n-----END RSA PRIVATE KEY-----"}},
            ]
            write_session(tmp, "20261125-120000", "cccccccc-0000-1111-2222-333344445555", entries)
            # JSON mode
            code, out = run_main(["--sessions-dir", str(tmp), "--json"])
            self.assertEqual(code, 0)
            self.assertNotIn("abc123xyz", out)
            self.assertNotIn("abc1234567890abcdef", out)
            self.assertNotIn("github_pat_abcdefghijklmnopqrstuvwx", out)
            self.assertNotIn("glpat-abcdefghijklmnop", out)
            self.assertNotIn("eyJhbGciOiJIUzI1NiJ9", out)
            self.assertNotIn("MIIEowIBAAKCAQEA", out)
            self.assertIn("[REDACTED]", out)
            # Text mode
            code, out = run_main(["--sessions-dir", str(tmp)])
            self.assertEqual(code, 0)
            self.assertNotIn("abc123xyz", out)
            self.assertNotIn("abc1234567890abcdef", out)
            self.assertNotIn("github_pat_abcdefghijklmnopqrstuvwx", out)
            self.assertNotIn("glpat-abcdefghijklmnop", out)
            self.assertNotIn("eyJhbGciOiJIUzI1NiJ9", out)
            self.assertNotIn("MIIEowIBAAKCAQEA", out)
            self.assertIn("[REDACTED]", out)

    def test_key_value_secret_redaction(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            entries = [
                {
                    "type": "message",
                    "id": "e1",
                    "message": {
                        "role": "assistant",
                        "content": None,
                        "tool_calls": [
                            {"id": "c1", "name": "bash", "args": {"command": "echo hi"}},
                            {"id": "c2", "name": "bash", "args": {"command": "echo hi"}},
                        ],
                    },
                },
                {
                    "type": "message",
                    "id": "e2",
                    "message": {
                        "role": "tool_result",
                        "tool_call_id": "c1",
                        "content": "API_KEY=super_secret_12345 trailing_word",
                        "is_error": True,
                    },
                },
                {
                    "type": "message",
                    "id": "e3",
                    "message": {
                        "role": "tool_result",
                        "tool_call_id": "c2",
                        "content": "password: hunter2 next_word",
                        "is_error": True,
                    },
                },
            ]
            write_session(tmp, "20261125-120000", "dddddddd-0000-1111-2222-333344445555", entries)
            code, out = run_main(["--sessions-dir", str(tmp), "--json"])
            self.assertEqual(code, 0)
            self.assertNotIn("super_secret_12345", out)
            self.assertNotIn("hunter2", out)
            self.assertIn("[REDACTED]", out)
            self.assertIn("trailing_word", out)
            self.assertIn("next_word", out)

    def test_user_message_none_content(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            entries = [
                {"type": "message", "id": "e1", "message": {"role": "assistant", "content": "hello"}},
                {"type": "message", "id": "e2", "message": {"role": "user", "content": None}},
                {"type": "message", "id": "e3", "message": {"role": "assistant", "content": "ok"}},
            ]
            write_session(tmp, "20261125-120000", "eeeeeeee-0000-1111-2222-333344445555", entries)
            code, out = run_main(["--sessions-dir", str(tmp), "--json"])
            self.assertEqual(code, 0)
            payload = json.loads(out)
            self.assertEqual(payload["sessions"][0]["user_turns"], 1)

    def test_markdown_tool_error_first_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            entries = [
                {
                    "type": "message",
                    "id": "e1",
                    "message": {
                        "role": "assistant",
                        "content": None,
                        "tool_calls": [{"id": "c1", "name": "edit", "args": {"path": "/a"}}],
                    },
                },
                {
                    "type": "message",
                    "id": "e2",
                    "message": {
                        "role": "tool_result",
                        "tool_call_id": "c1",
                        "content": "edit 1 failed: old_text not found: API_KEY=super_secret_12345\nsecond line",
                        "is_error": True,
                    },
                },
            ]
            write_session(tmp, "20261125-120000", "ffffffff-0000-1111-2222-333344445555", entries)
            code, out = run_main(["--sessions-dir", str(tmp)])
            self.assertEqual(code, 0)
            self.assertIn("- Tool errors: 1", out)
            self.assertIn(
                "  - `edit` (x1): edit 1 failed: old_text not found: [REDACTED]",
                out,
            )
            self.assertNotIn("super_secret_12345", out)

    def test_last_n_ordering(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            simple_session(tmp, "20261125-100000", "11111111-0000-0000-0000-000000000001")
            simple_session(tmp, "20261125-110000", "22222222-0000-0000-0000-000000000002")
            simple_session(tmp, "20261125-120000", "33333333-0000-0000-0000-000000000003")
            code, out = run_main(["--sessions-dir", str(tmp), "--last", "2", "--json"])
            self.assertEqual(code, 0)
            payload = json.loads(out)
            ids = [s["session_id"] for s in payload["sessions"]]
            self.assertEqual(len(ids), 2)
            # Newest two, newest first (friction is 0 for all, so timestamp desc).
            self.assertEqual(ids[0], "33333333-0000-0000-0000-000000000003")
            self.assertEqual(ids[1], "22222222-0000-0000-0000-000000000002")
            self.assertNotIn("11111111-0000-0000-0000-000000000001", ids)

    def test_missing_directory(self):
        code, out = run_main(["--sessions-dir", "/nonexistent/path/xyz", "--json"])
        self.assertEqual(code, 0)
        payload = json.loads(out)
        self.assertEqual(payload["sessions"], [])
        self.assertEqual(payload["aggregate"]["sessions"], 0)
        # Text mode
        code, out = run_main(["--sessions-dir", "/nonexistent/path/xyz"])
        self.assertEqual(code, 0)
        self.assertIn("No session directory", out)


if __name__ == "__main__":
    unittest.main()
