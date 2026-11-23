# gg — gap analysis for an everyday-driver harness

**Status:** 2026-11-23, against `main` @ 4557359 (`ganggang` v0.1.0).
Every claim below was read out of the source in this tree; file references
are the place to look, not a suggestion.

The core is solid: one agent loop, seven tools, sessions with crash-safe
JSONL, compaction, streaming, a working TUI, print mode, JSON mode, HTML
export. What's missing is the stuff you only notice after a week of daily
use — resilience, escapability, and the ability to audit what the model did.

---

## Would bite you in week one

1. **No retry/backoff on transient failures.** `LlmClient::send` and
   `send_stream` make exactly one attempt, and `check_status`
   (`src/llm/client.rs`) bails on any non-2xx. One 429/503/connection reset
   against a cloud provider (deepseek, glm-coding-plan, opencode-go) kills
   the run; your retry is `gg -c`. This is the single biggest everyday gap.

2. **A cut stream throws away the partial answer.** When the SSE body ends
   without `[DONE]`, `src/llm/stream.rs` bails with "partial output
   discarded; retry the run". Combined with the fixed 300 s whole-response
   timeout in `client.rs`, a long generation that gets guillotined costs
   the whole turn. There is no salvage and no continue.

3. **Esc cannot stop the tool that is running.** The abort flag is checked
   only between tool calls (`src/agent/loop_.rs`); `Tool::execute`
   (`src/agent/tools/mod.rs`) has no cancellation path. A `cargo build`,
   `npm test`, or any bash configured above the 30 s default keeps running
   after Esc, and the UI looks dead. It is bounded, but it is exactly the
   moment you most want control.

4. **No prompt history.** `src/tui/keys.rs` maps ↑/↓ to cursor movement and
   the `/` suggestion popup. There is no recall buffer: every follow-up is
   re-typed or pasted.

5. **Ctrl+C quits.** `KeyCode::Char('c') if ctrl => Quit` in
   `src/tui/keys.rs`. Esc is the abort key, but Ctrl+C is muscle memory, and
   here it ends the TUI mid-run instead of cancelling it.

6. **No undo/checkpoint.** Edits are written in place; there is no git
   snapshot and no `/undo`. With permission gating deliberately absent (see
   below), the only safety net is your own VCS — fine until the one time you
   had not committed.

---

## Sharp edges

7. **You cannot see what the model saw.** Expanded tool output caps at 30
   lines (`EXPAND_LINES` in `src/tui/transcript.rs`), while the model
   receives up to 10 KiB from `bash` (`OUTPUT_CAP`) and 2000 lines / 50 KiB
   from `read`. There is no pager, no transcript search, and no full-output
   view — only `--export` or reading the session JSONL. Mouse capture is
   never enabled (`src/tui/term.rs`), so no scroll wheel either.

8. **`/resume` is `timestamp model 8-hex`.** No title, no first-message
   preview, no rename (`src/tui/commands.rs` renders the list;
   `SessionFile` in `src/agent/session/io.rs` does not carry a title). And
   sessions are cwd-scoped, so there is no cross-directory picker.

9. **`context_window` is manual.** Nothing queries model metadata, so a
   wrong value silently mis-times compaction, or the provider 400s
   (`src/config.rs`). The fallback token estimate is chars/4, documented in
   `src/agent/compaction.rs` as drifting on CJK/code.

10. **No cost, no latency.** The footer shows tokens only
    (`src/tui/footer.rs`); Strata's `timings` are parsed permissively and
    ignored. You cannot answer "what did today cost?".

11. **`finish_reason: length` only warns.** The loop emits a `Warned`
    event but does not continue the answer or raise `max_tokens`
    (`src/agent/loop_.rs`).

12. **Tool calls run strictly sequentially** (`src/agent/loop_.rs`), so a
    batch of independent reads or greps serializes.

---

## Missing, and deliberately so

`TASKS.md` §5 lists the non-scope. These are the ones an everyday driver
will feel:

13. **MCP/plugins** — every integration is a code change and a rebuild.
14. **Images, web search/fetch, subagents/plan mode, git tooling, LSP
    diagnostics.**
15. **Background processes.** `bash` is foreground-only with
    `stdin(Stdio::null())` and a hard timeout (`src/agent/tools/bash.rs`):
    no way to start a dev server, keep it, and read its logs.
16. **Permission gating.** Not a bug — the stated design (README, "Security
    stance"). But "ask before bash" is the first thing most people look for
    in a daily driver; at minimum an opt-in approve mode would help.
17. **Unix-only** in practice: `stty`, `/bin/kill`, and bash are all load-
    bearing (`src/tui/term.rs`, `src/agent/tools/bash.rs`). Windows means
    WSL.

Minor: the system prompt carries a UTC **date** but no time or timezone
(`src/agent/context.rs`), which matters for "today"/"now" questions.

---

## If you fix five things

1. Retry with backoff on 429/5xx/connection errors, and keep the partial
   stream on a cut (`src/llm/client.rs`, `src/llm/stream.rs`).
2. Cancellable tool execution — kill the process group on abort
   (`src/agent/tools/mod.rs`, `src/agent/loop_.rs`).
3. ↑/↓ prompt history; Ctrl+C cancels the run instead of quitting
   (`src/tui/keys.rs`).
4. A real tool-output viewer in the TUI — pager or true full expand
   (`src/tui/transcript/render.rs`). Auditing is the point of a harness.
5. Session titles plus a cross-directory resume picker
   (`src/agent/session/io.rs`, `src/tui/commands.rs`).

Items 1–3 are hours of work. They are the difference between "impressive
build" and "the thing I reach for."
