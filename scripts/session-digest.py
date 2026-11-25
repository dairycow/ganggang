#!/usr/bin/env python3
"""Digest gg session files into friction metrics.

Reads append-only JSONL session files under ``~/.gg/sessions/<slug>/`` (the
same layout ``gg`` writes, see ``src/agent/session/io.rs``) and reports per
session: turn counts, tool calls, tool errors, aborts, compactions, repeated
reads, bash workarounds, correction candidates, token usage, and a friction
score. Corrupt or torn trailing lines are counted and skipped, never fatal.

Stdlib only. See ``scripts/test-session-digest.py``.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path

# Commands that duplicate a native tool when run through bash. Same set as
# scripts/eval.sh.
WORKAROUND_COMMANDS = {
    "sed",
    "cat",
    "head",
    "tail",
    "grep",
    "rg",
    "find",
    "ls",
    "perl",
    "python",
    "python3",
}

# Lower-cased cues that mark a user message as steering the agent.
CORRECTION_CUES = (
    "no,",
    "don't",
    "do not",
    "instead",
    "wrong",
    "actually",
    "not that",
    "revert",
    "undo",
    "missed",
    "stop ",
)

# Secret scrubbers, applied in order. Each match is replaced with [REDACTED].
# This is best-effort: free-form user text and unfamiliar token shapes still
# reach the digest and whichever model reads it. Prefer a local endpoint for
# sensitive sessions.
SECRET_PATTERNS = [
    (
        re.compile(
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
            re.DOTALL,
        ),
        "[REDACTED]",
    ),
    (re.compile(r"sk-[A-Za-z0-9_-]{8,}"), "[REDACTED]"),
    (re.compile(r"github_pat_[A-Za-z0-9_]{20,}"), "[REDACTED]"),
    (re.compile(r"glpat-[A-Za-z0-9_-]{16,}"), "[REDACTED]"),
    (re.compile(r"ghp_[A-Za-z0-9]{10,}"), "[REDACTED]"),
    (re.compile(r"xox[baprs]-[A-Za-z0-9-]+"), "[REDACTED]"),
    (re.compile(r"AIza[A-Za-z0-9_-]{10,}"), "[REDACTED]"),
    (re.compile(r"AKIA[0-9A-Z]{16}"), "[REDACTED]"),
    (
        re.compile(
            r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"
        ),
        "[REDACTED]",
    ),
    (re.compile(r"Bearer\s+\S+"), "Bearer [REDACTED]"),
    (
        re.compile(
            r"(api[_-]?key|token|secret|password)\s*[:=]\s*['\"]?[^\s'\"]+",
            re.IGNORECASE,
        ),
        "[REDACTED]",
    ),
]

# Split a shell command into segments the same way scripts/eval.sh does.
SEGMENT_SPLIT = re.compile(r"\s*(?:&&|\|\||[;\n])\s*")


def cwd_slug(cwd):
    """Filesystem-safe cwd slug: ``/`` to ``-`` (matches io.rs ``slug``)."""
    return str(Path(cwd).resolve()).replace("/", "-")


def resolve_session_dir(args):
    """The session directory to read from."""
    if args.sessions_dir:
        return Path(args.sessions_dir)
    return Path.home() / ".gg" / "sessions" / cwd_slug(args.cwd)


def has_entries(path):
    """True when the file has at least one non-empty line after the header."""
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return False
    return any(line.strip() for line in lines[1:])


def session_sort_key(path):
    """Newest first: filename stamp descending, then mtime descending."""
    stem = path.stem
    stamp = stem.rsplit("-", 1)[0] if "-" in stem else stem
    try:
        mtime = path.stat().st_mtime
    except OSError:
        mtime = 0.0
    return (stamp, mtime)


def list_session_files(directory):
    """All ``.jsonl`` files under ``directory``, newest first."""
    if not directory.is_dir():
        return []
    files = [p for p in directory.iterdir() if p.is_file() and p.suffix == ".jsonl"]
    files.sort(key=session_sort_key, reverse=True)
    return files


def read_header_id(path):
    """The header uuid for a session file, or None when unreadable."""
    try:
        with path.open("r", encoding="utf-8", errors="replace") as handle:
            first = handle.readline()
        header = json.loads(first)
    except (OSError, ValueError):
        return None
    if isinstance(header, dict):
        return header.get("id")
    return None


def resolve_by_prefix(directory, prefix):
    """Session files whose header id or 8-hex stem id starts with ``prefix``."""
    matches = []
    if not directory.is_dir():
        return matches
    for path in list_session_files(directory):
        stem = path.stem
        stem_id = stem.rsplit("-", 1)[-1] if "-" in stem else stem
        header_id = read_header_id(path)
        if (stem_id and stem_id.startswith(prefix)) or (
            header_id and header_id.startswith(prefix)
        ):
            matches.append(path)
    return matches


def resolve_targets(session_dir, session_specs, last_n):
    """(paths, warnings) for the sessions to analyse."""
    if session_specs:
        paths = []
        warnings = []
        for spec in session_specs:
            direct = Path(spec)
            if direct.is_file() and direct.suffix == ".jsonl":
                paths.append(direct)
                continue
            matches = resolve_by_prefix(session_dir, spec)
            if not matches:
                warnings.append(f"no session matches {spec!r}")
            else:
                paths.extend(matches)
        return paths, warnings
    files = [f for f in list_session_files(session_dir) if has_entries(f)]
    return files[:last_n], []


def redact_secrets(text):
    """Scrub known secret shapes from ``text``."""
    if not isinstance(text, str) or not text:
        return text
    for pattern, replacement in SECRET_PATTERNS:
        text = pattern.sub(replacement, text)
    return text


def truncate(text, max_snippet):
    """Cap a snippet to ``max_snippet`` chars, trimming the tail cleanly."""
    text = text.strip()
    if len(text) <= max_snippet:
        return text
    return text[:max_snippet].rstrip() + "\u2026"


def redact(text, max_snippet):
    """Redact secrets then cap the snippet."""
    return truncate(redact_secrets(text), max_snippet)


def bash_is_workaround(command, cwd):
    """True when a bash command duplicates a native tool or cds to the cwd."""
    if not command:
        return False
    if cwd and ("cd " + cwd) in command:
        return True
    for segment in SEGMENT_SPLIT.split(command):
        tokens = segment.strip().split()
        while tokens and "=" in tokens[0] and not tokens[0].startswith("="):
            tokens.pop(0)
        if not tokens or tokens[0] == "cd":
            continue
        base = os.path.basename(tokens[0])
        if base in WORKAROUND_COMMANDS:
            return True
    return False


def first_line(content):
    if not isinstance(content, str):
        return ""
    return content.split("\n", 1)[0].strip()


def parse_session(path, max_snippet):
    """Metrics for one session file, or None when it cannot be read."""
    try:
        raw = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None

    session_id = None
    timestamp = None
    model = None
    cwd = None
    skipped_lines = 0
    entry_count = 0
    user_turns = 0
    assistant_turns = 0
    tool_calls_total = 0
    tool_calls_by_name = {}
    tool_errors_total = 0
    tool_error_groups = {}
    abort_user = 0
    abort_tool = 0
    compactions = 0
    read_counts = {}
    bash_workarounds = 0
    correction_candidates = []
    peak_prompt = 0
    sum_prompt = 0
    sum_completion = 0
    seen_assistant = False
    tool_call_name_by_id = {}

    for index, line in enumerate(raw.splitlines()):
        if index == 0:
            if not line.strip():
                skipped_lines += 1
                continue
            try:
                header = json.loads(line)
            except ValueError:
                skipped_lines += 1
                continue
            if not isinstance(header, dict) or header.get("type") != "session":
                skipped_lines += 1
                continue
            session_id = header.get("id")
            timestamp = header.get("timestamp")
            model = header.get("model")
            cwd = header.get("cwd")
            continue

        if not line.strip():
            continue
        try:
            entry = json.loads(line)
        except ValueError:
            skipped_lines += 1
            continue
        if not isinstance(entry, dict):
            skipped_lines += 1
            continue

        entry_count += 1
        entry_type = entry.get("type")

        if entry_type == "compaction":
            compactions += 1
            continue

        if entry_type != "message":
            continue

        message = entry.get("message")
        if not isinstance(message, dict):
            continue
        role = message.get("role")
        content = message.get("content")

        if role == "user":
            user_turns += 1
            if content == "[aborted by user]":
                abort_user += 1
                continue
            if (
                isinstance(content, str)
                and seen_assistant
                and not content.startswith("[continue exactly where you stopped")
            ):
                lowered = content.lower()
                if any(cue in lowered for cue in CORRECTION_CUES):
                    correction_candidates.append(redact(content, max_snippet))
        elif role == "assistant":
            assistant_turns += 1
            seen_assistant = True
            for call in message.get("tool_calls") or []:
                if not isinstance(call, dict):
                    continue
                tool_calls_total += 1
                name = call.get("name") or "unknown"
                tool_calls_by_name[name] = tool_calls_by_name.get(name, 0) + 1
                call_id = call.get("id")
                if call_id:
                    tool_call_name_by_id[call_id] = name
                args = call.get("args")
                if name in ("read", "grep") and isinstance(args, dict):
                    target = args.get("path")
                    if target:
                        read_counts[target] = read_counts.get(target, 0) + 1
                if name == "bash" and isinstance(args, dict):
                    if bash_is_workaround(args.get("command") or "", cwd):
                        bash_workarounds += 1
            usage = message.get("usage")
            if isinstance(usage, dict):
                prompt = usage.get("prompt_tokens") or 0
                completion = usage.get("completion_tokens") or 0
                peak_prompt = max(peak_prompt, prompt)
                sum_prompt += prompt
                sum_completion += completion
        elif role == "tool_result":
            if message.get("is_error"):
                tool_errors_total += 1
                if isinstance(content, str) and content.strip() == "aborted before execution":
                    abort_tool += 1
                tool_name = tool_call_name_by_id.get(message.get("tool_call_id"))
                if tool_name is None:
                    tool_name = "unknown"
                line_preview = redact(first_line(content), max_snippet)
                key = (tool_name, line_preview)
                tool_error_groups[key] = tool_error_groups.get(key, 0) + 1

    repeated_reads = {k: v for k, v in read_counts.items() if v >= 2}
    error_groups = [
        {"tool": tool, "first_line": line, "count": count}
        for (tool, line), count in sorted(
            tool_error_groups.items(), key=lambda item: (-item[1], item[0][0], item[0][1])
        )
    ]
    aborts = abort_user + abort_tool
    friction = (
        aborts * 5
        + tool_errors_total * 2
        + bash_workarounds * 2
        + len(correction_candidates) * 3
        + compactions * 2
        + sum(count - 1 for count in repeated_reads.values())
    )

    return {
        "session_id": session_id,
        "path": str(path),
        "timestamp": timestamp,
        "model": model,
        "cwd": cwd,
        "entry_count": entry_count,
        "skipped_lines": skipped_lines,
        "user_turns": user_turns,
        "assistant_turns": assistant_turns,
        "tool_calls": {"total": tool_calls_total, "by_name": tool_calls_by_name},
        "tool_errors": {
            "total": tool_errors_total,
            "by_tool_and_first_line": error_groups,
        },
        "aborts": aborts,
        "compactions": compactions,
        "repeated_reads": repeated_reads,
        "bash_workarounds": bash_workarounds,
        "correction_candidates": correction_candidates,
        "tokens": {
            "peak_prompt": peak_prompt,
            "sum_prompt": sum_prompt,
            "sum_completion": sum_completion,
        },
        "friction_score": friction,
    }


def build_aggregate(sessions):
    """Totals across the analysed sessions."""
    return {
        "sessions": len(sessions),
        "total_friction": sum(s["friction_score"] for s in sessions),
        "total_user_turns": sum(s["user_turns"] for s in sessions),
        "total_assistant_turns": sum(s["assistant_turns"] for s in sessions),
        "total_tool_calls": sum(s["tool_calls"]["total"] for s in sessions),
        "total_tool_errors": sum(s["tool_errors"]["total"] for s in sessions),
        "total_aborts": sum(s["aborts"] for s in sessions),
        "total_compactions": sum(s["compactions"] for s in sessions),
        "total_bash_workarounds": sum(s["bash_workarounds"] for s in sessions),
        "total_correction_candidates": sum(
            len(s["correction_candidates"]) for s in sessions
        ),
        "sum_prompt": sum(s["tokens"]["sum_prompt"] for s in sessions),
        "sum_completion": sum(s["tokens"]["sum_completion"] for s in sessions),
        "peak_prompt": max((s["tokens"]["peak_prompt"] for s in sessions), default=0),
    }


def session_label(session):
    """A short human label for a session row."""
    session_id = session.get("session_id") or "unknown"
    return session_id[:8]


def render_markdown(sessions, aggregate):
    """A concise Markdown report, sessions ordered by friction descending."""
    lines = []
    lines.append("# gg session digest")
    lines.append("")
    lines.append(f"Sessions analysed: {aggregate['sessions']}")
    lines.append(f"Total friction: {aggregate['total_friction']}")
    lines.append("")
    lines.append(
        "| Session | Friction | User/Asst | Tool calls | Errors | "
        "Aborts | Compactions | Workarounds | Corrections | Peak prompt |"
    )
    lines.append("|---|---|---|---|---|---|---|---|---|---|")
    for session in sessions:
        tokens = session["tokens"]
        lines.append(
            "| {label} | {friction} | {user}/{asst} | {calls} | {errors} | "
            "{aborts} | {compactions} | {workarounds} | {corrections} | {peak} |".format(
                label=session_label(session),
                friction=session["friction_score"],
                user=session["user_turns"],
                asst=session["assistant_turns"],
                calls=session["tool_calls"]["total"],
                errors=session["tool_errors"]["total"],
                aborts=session["aborts"],
                compactions=session["compactions"],
                workarounds=session["bash_workarounds"],
                corrections=len(session["correction_candidates"]),
                peak=tokens["peak_prompt"],
            )
        )
    lines.append("")
    for session in sessions:
        lines.append(f"## {session_label(session)}")
        lines.append("")
        lines.append(f"- Friction score: {session['friction_score']}")
        lines.append(
            "- Turns: {} user, {} assistant, {} entries".format(
                session["user_turns"], session["assistant_turns"], session["entry_count"]
            )
        )
        by_name = session["tool_calls"]["by_name"]
        if by_name:
            detail = ", ".join(f"{name} x{count}" for name, count in sorted(by_name.items()))
            lines.append(f"- Tool calls: {session['tool_calls']['total']} ({detail})")
        groups = session["tool_errors"]["by_tool_and_first_line"]
        if groups:
            lines.append(f"- Tool errors: {session['tool_errors']['total']}")
            for group in groups[:5]:
                lines.append(
                    f"  - `{group['tool']}` (x{group['count']}): {group['first_line']}"
                )
            if len(groups) > 5:
                lines.append(f"  - (+{len(groups) - 5} more error groups)")
        if session["aborts"]:
            lines.append(f"- Aborts: {session['aborts']}")
        if session["compactions"]:
            lines.append(f"- Compactions: {session['compactions']}")
        if session["repeated_reads"]:
            detail = ", ".join(
                f"{path} x{count}" for path, count in sorted(session["repeated_reads"].items())
            )
            lines.append(f"- Repeated reads: {detail}")
        if session["bash_workarounds"]:
            lines.append(f"- Bash workarounds: {session['bash_workarounds']}")
        for candidate in session["correction_candidates"]:
            lines.append(f"- Correction: {candidate}")
        tokens = session["tokens"]
        lines.append(
            "- Tokens: peak prompt {}, sum prompt {}, sum completion {}".format(
                tokens["peak_prompt"], tokens["sum_prompt"], tokens["sum_completion"]
            )
        )
        if session["skipped_lines"]:
            lines.append(f"- Skipped lines: {session['skipped_lines']}")
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def parse_args(argv=None):
    parser = argparse.ArgumentParser(
        description="Digest gg session files into friction metrics."
    )
    parser.add_argument(
        "--cwd",
        default=".",
        help="Working directory whose sessions to read (default: current dir).",
    )
    parser.add_argument(
        "--sessions-dir",
        help="Override the session directory directly.",
    )
    parser.add_argument(
        "--last",
        type=int,
        default=5,
        help="Newest N non-empty sessions to analyse (default: 5).",
    )
    parser.add_argument(
        "--session",
        action="append",
        default=[],
        help="Analyse a specific .jsonl path or id/8-hex prefix (repeatable).",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit machine-readable JSON.",
    )
    parser.add_argument(
        "--max-snippet",
        type=int,
        default=160,
        help="Cap user and error snippets to this many characters (default: 160).",
    )
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    session_dir = resolve_session_dir(args)
    dir_exists = session_dir.is_dir()
    paths, warnings = resolve_targets(session_dir, args.session, args.last)
    for warning in warnings:
        print(f"session-digest: {warning}", file=sys.stderr)

    sessions = []
    for path in paths:
        metrics = parse_session(path, args.max_snippet)
        if metrics is not None:
            sessions.append(metrics)
    sessions.sort(
        key=lambda s: (s["friction_score"], s["timestamp"] or ""), reverse=True
    )
    aggregate = build_aggregate(sessions)

    if args.json:
        print(json.dumps({"sessions": sessions, "aggregate": aggregate}, indent=2))
    elif not sessions and not dir_exists:
        print(f"No session directory at {session_dir}.")
    elif not sessions:
        print(f"No sessions found under {session_dir}.")
    else:
        sys.stdout.write(render_markdown(sessions, aggregate))
    return 0


if __name__ == "__main__":
    sys.exit(main())
