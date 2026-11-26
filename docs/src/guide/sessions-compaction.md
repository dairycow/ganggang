# Sessions and compaction

## Sessions

Every conversation is an append-only JSONL file under `~/.gg/sessions/<cwd-slug>/`, named `<YYYYMMDD-HHMMSS>-<8hex>.jsonl` (mode `0600`). The slug is the working directory with `/` turned into `-`, so each directory has its own history. Each entry is flushed as it is written. A new session's file appears on its first message. Starting and quitting without sending a message creates no file.

The first line is a header (`type`, `version`, `id`, timestamp, cwd and model). A session started by another run's `bash` tool also records that run's id as `parent_id`; see [Delegation](delegation.md).

| Command | Action |
|---|---|
| `gg -c` | Continue the newest session for this directory |
| `gg --resume <id\|prefix>` | Open a specific session (full id or the 8-hex id in the file name; ambiguous prefixes list the candidates) |
| `gg --session-file <path>` | Persist to and/or resume an exact session JSONL file path |
| `gg --no-session` | Keep nothing (cannot combine with `-c`/`--resume`/`--session-file`) |
| `/new` | Start a fresh session in the TUI |
| `/session` | Open the session list as soon as the command is typed, across every directory (`Tab` in it narrows it to this one); typing after the command filters it and `Enter` opens the highlighted session or runs the highlighted `copy path` or `info` row. A session continues in the directory it started in |

### Crash safety

Sessions remain loadable after an unexpected exit:

- A torn final line is skipped with a warning; a missing trailing newline is repaired before appending.
- Interrupted tool calls without recorded results get synthesised `aborted before execution` error results on resume, so a call is never replayed without its result.
- `gg` refuses to append to a session file containing corrupt lines.

Session files are plain JSON you can read, `grep`, or [export](modes.md#html-export) to HTML.

### Aborted and truncated turns

- Aborted runs append `[aborted by user]` after any partial output. Interrupted tool calls receive synthetic `aborted before execution` results so the session stays valid. Aborting before the first token creates no blank assistant entry.
- A truncated answer is [continued](modes.md#truncated-responses): the next user entry is `[continue exactly where you stopped; do not repeat text you already wrote]`, or `[your previous tool call was cut off by max_tokens and was not executed; retry it in smaller pieces or continue]` when the cut answer carried a tool call, and any tool call cut off with the answer is answered `not executed: response was truncated` rather than run. A truncation that produced no visible text or tool call is not continued: `gg` warns and stops, and no blank assistant entry is recorded. A turn that was nothing but reasoning is asked again with reasoning off instead (see [Truncated responses](modes.md#truncated-responses)): for that turn alone the first time, and for the rest of the run when it happens twice in a row. The cut turn leaves no entry.

### Failed runs

When a run ends on an error, such as every retry of a request failing or the server refusing it, `gg` appends a diagnostic entry carrying the error text, so the file says why the history stops where it does:

```json
{"type":"error","id":"e2a9c4f1","parentId":"c3d4e5f6","timestamp":"2026-11-26T01:02:03Z","error":"POST http://127.0.0.1:8080/v1/chat/completions failed (503 Service Unavailable): upstream exploded"}
```

The entry is for you, not the model: it is never sent in a later request and counts for no context tokens. A resumed session shows it as a `run failed:` line, and the [HTML export](modes.md#html-export) renders it as a marked block. An aborted run records no such entry; its marker is the `[aborted by user]` line above.

## Compaction

When the conversation nears the context limit, older history is summarised in one extra model call and recorded as a `Compaction` entry. Raw history is never deleted from the file.

- **Reserve:** `max(16,384, max_tokens)` tokens are held free for the completion, capped at `context_window / 4`, plus up to 8,192 tokens of headroom, capped at `context_window / 8`. A server refuses a request whose input and `max_tokens` together exceed its window, and the headroom covers the estimated part of the token count. A `max_tokens` above `16384` therefore lowers the line by the difference. Small windows reserve proportionally less.
- **Trigger:** after a tool batch, and before a continuation request, when the token count exceeds the compaction line. The count is the last response's token usage plus an estimate of the entries appended since it (the tool results of the batch just run). The line is `min(compact_threshold_tokens, context_window − reserve)` with a positive `compact_threshold_tokens`, or `context_window − reserve` otherwise (strata defaults `compact_threshold_tokens` to `65536`). Set `context_window` to your server's real window, or let `gg` probe `/health` on local servers (see [Configuration](configuration.md)).
- **What is kept:** `min(20,000, compaction_line / 3)` tokens, cut at a user-message boundary (or an assistant-message boundary in single-prompt runs, pinning the original user request) so a tool result is never separated from its call. A second compaction re-summarises only what accumulated since the first. A compaction line of 60,000 tokens or more keeps the full 20,000; a smaller one keeps a third so the window has room for the turns after a compaction.
- **Summary budget:** the summary request includes the working directory, omits tools, caps output at 4,096 tokens, and lowers configured reasoning effort to `low` (capping any thinking token budget at 512) so compaction stays fast.
- **Bounded input:** the transcript the summariser reads is cut to what the context window holds beside the summary: about 3 characters per token, less 4,096 tokens for the summary and 2,048 for the instruction. A tool result longer than 4,000 characters is reduced to its first 1,500 and last 2,500 characters first. A transcript still over the limit keeps its leading quarter (where the previous summary and the original request sit) and its newest three-quarters, with a note saying how much was left out.
- **Manual:** `/compact` runs it now; `/compact focus on the parser rework` passes focus instructions to the summariser.
- **Failure is not fatal:** if the summary call fails, `gg` warns and continues without compacting. If no cut point exists yet, it warns once per run.

Compaction events appear as warnings in the transcript and in the [JSON event stream](modes.md#json-mode).
