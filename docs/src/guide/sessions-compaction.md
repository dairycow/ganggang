# Sessions and compaction

## Sessions

Every conversation is an append-only JSONL file under `~/.gg/sessions/<cwd-slug>/`, named `<YYYYMMDD-HHMMSS>-<8hex>.jsonl`. The slug is the working directory with `/` turned into `-`, so each directory has its own history. Each entry is flushed as it is written. A new session's file appears on its first message, so launching and quitting leaves nothing behind.

| Command | Action |
|---|---|
| `gg -c` | Continue the newest session for this directory |
| `gg --resume <id\|prefix>` | Open a specific session (full id or the 8-hex id in the file name; ambiguous prefixes list the candidates) |
| `gg --session-file <path>` | Persist to and/or resume an exact session JSONL file path |
| `gg --no-session` | Keep nothing (cannot combine with `-c`/`--resume`/`--session-file`) |
| `/new` | Start a fresh session in the TUI |
| `/resume` | List sessions, then `/resume <n\|prefix>` to open one |
| `/session` | Show the session path, model, entry count, and token totals |

### Crash safety

A crash mid-run leaves a file that still loads:

- A torn final line is skipped with a warning; a missing trailing newline is repaired before appending.
- Tool calls whose results never landed (killed mid-batch) get synthesised `aborted before execution` error results on resume, so a call is never replayed without its result.
- A session with corrupt lines is never appended to; `gg` refuses.

Session files are plain JSON you can read, `grep`, or [export](modes.md#html-export) to HTML.

### Aborted and truncated turns

- A run the user stopped records `[aborted by user]` after whatever had arrived. Tool calls that had started are answered (`aborted before execution`), so the file still replays; a stream stopped before its first token leaves no blank assistant entry behind.
- A truncated answer is [continued](modes.md#truncated-responses): the next user entry is `[continue exactly where you stopped; do not repeat text you already wrote]`, and any tool call cut off with the answer is answered `not executed: response was truncated` rather than run.

## Compaction

When the conversation nears the context limit, older history is summarised in one extra model call and recorded as a `Compaction` entry. Raw history is never deleted from the file.

- **Trigger:** after a tool batch, and before a continuation request, when the last response's token usage exceeds `min(compact_threshold_tokens, context_window − 16,384)` (or `context_window − 16,384` when `compact_threshold_tokens` is unset; `strata` defaults `compact_threshold_tokens` to `65536`). Set `context_window` to your server's real window, or let `gg` probe `/health` on local servers (see [Configuration](configuration.md)).
- **What is kept:** roughly the most recent 20,000 tokens, cut at a user-message boundary (or an assistant-message boundary in single-prompt runs, pinning the original user request) so a tool result is never separated from its call. A second compaction re-summarises only what accumulated since the first.
- **Manual:** `/compact` runs it now; `/compact focus on the parser rework` passes focus instructions to the summariser.
- **Failure is not fatal:** if the summary call fails, `gg` warns and carries on uncompacted. If no cut point exists yet, it warns once per run.

Compaction events appear as warnings in the transcript and in the [JSON event stream](modes.md#json-mode).
