# gg: gap analysis for an everyday-driver harness

**Status:** 2026-11-25, against `main` (`ganggang` `v0.3.6`).
Every claim below was read out of the source in this tree; file references
are the place to look, not a suggestion.

The core is solid: one agent loop, eight tools, sessions with crash-safe
JSONL, compaction, streaming, a working TUI, print mode, JSON mode, HTML
export. What is missing is the stuff you only notice after a week of daily
use: escapability, auditability, and cost.

---

## Resolved since v0.1.0

The 2026-11-23 audit's "would bite you in week one" list has shipped:

1. **`outline` tool.** The 8th tool; file outline for navigating large
   files without reading them.
2. **Retry with backoff.** Transient 429/5xx/connection errors retry
   automatically (`src/llm/retry.rs`; `max_retries`).
3. **Cut-stream salvage.** A partial answer kept when the SSE body ends
   without `[DONE]` (`src/llm/stream.rs`).
4. **Continuation.** Cut streams and `finish_reason: length` continue,
   up to 3 times.
5. **`request_timeout_secs`.** Configurable whole-response timeout,
   default 600 s (was a fixed 300 s).
6. **Cancellable tools.** Tools take the abort flag and report
   `aborted by user` (`src/agent/tools/mod.rs`).
7. **Process-group kill.** Esc kills the running tool's process group,
   not just the flag.
8. **Prompt history.** Up/down arrow recall buffer
   (`src/tui/history.rs`).
9. **Ctrl+C cancel.** Ctrl+C cancels the run instead of quitting the TUI.
10. **Mouse wheel capture.** Mouse capture enabled; wheel scrolls the
    transcript (`src/tui/term.rs`).
11. **Context auto-probing.** `context_window` probed from `/health` and
    `/v1/models` (`src/llm/health.rs`).
12. **Footer throughput.** The footer shows tokens and throughput, not
    tokens only (`src/tui/footer.rs`).
13. **Concurrent read-only tool dispatch.** Contiguous read-only calls
    dispatch concurrently, up to 8 in flight (`src/agent/loop_.rs`).
14. **`--session-file`.** Open a session by explicit file path, across
    directories.

---

## Remaining gaps for v0.3.6

1. **No undo/checkpoint.** Edits are written in place; there is no git
   snapshot and no `/undo`. With permission gating deliberately absent
   (see below), the only safety net is your own VCS, and it fails the
   one time you had not committed.

2. **30-line expand cap, no full view.** Expanded tool output caps at
   30 lines (`EXPAND_LINES`, `src/tui/transcript.rs`) while the model
   receives up to 10 KiB from `bash` and 2000 lines / 50 KiB from
   `read`. Wheel scroll now works, but there is no pager and no
   transcript search; `--export` or the session JSONL is still the
   full-output path.

3. **No session titles.** The `/session` list names a session by its
   first message: `SessionFile` in `src/agent/session/io/list.rs` reads a
   `"type":"title"` line when a file has one, and nothing in gg writes
   one. A session cannot be renamed from the TUI. (The cross-directory
   picker itself shipped in `ebb5e20`.)

4. **No cost tracking.** The footer shows tokens and throughput; Strata
   `timings` are parsed permissively and ignored
   (`src/tui/footer.rs`). "What did today cost?" is still unanswerable.

5. **UTC date only.** The system prompt carries a UTC date but no time
   or timezone (`src/agent/context.rs`), which matters for "today" and
   "now" questions.

---

## Deliberately non-scope

These are the ones an everyday driver will feel:

- **MCP/plugins.** Every integration is a code change and a rebuild.
- **Images, web search/fetch, subagents/plan mode, git tooling, LSP
  diagnostics.**
- **Background processes.** `bash` is foreground-only with a hard
  timeout (`src/agent/tools/bash.rs`): no way to start a dev server,
  keep it, and read its logs.
- **Opt-in approvals.** The stated design (README, "Security") is no
  permission gating. An opt-in approve mode before `bash` would help
  daily use.
- **Unix-only in practice.** `stty`, `/bin/kill`, and bash are load-
  bearing (`src/tui/term.rs`, `src/agent/tools/bash.rs`). Windows means
  WSL.
