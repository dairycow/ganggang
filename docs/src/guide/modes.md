# Output Modes & Export

All modes share one agent core and persist a session by default (`--no-session` opts out).

## TUI

The default on a terminal. See [Interactive TUI](tui.md).

## Print mode

```sh
gg -p "what changed in the last commit?"
echo "explain this" | gg -p -
```

The final answer goes to **stdout**; progress (turn markers, formatted tool-call lines, per-turn token/throughput stats, a final run summary, and a spinner on a terminal) goes to **stderr**. A consumer that closes the pipe early (`… | head -c 5`) ends the run quietly.

`Ctrl+C` aborts the run: an in-flight request or stream is cut within about 100 ms rather than left to finish, a running tool is stopped (a `bash` command loses its whole process group), and what had already streamed in is the answer on stdout. A second `Ctrl+C` exits immediately with status 130. `SIGTERM` triggers the same graceful abort, flushes output, and exits with status 143.

### Truncated responses

A response can end before it is finished: the provider reports `finish_reason: "length"` (the answer hit `max_tokens`), or the stream ends without its sentinel (the request timed out, the connection dropped). Either way the partial answer is kept, a `warning:` line says what happened, and `gg` asks the model to continue from exactly where it stopped, at most 3 times per run and never past `--max-turns`. The pieces are joined, so stdout still holds one answer. A tool call cut off mid-arguments is never run; the model is told it was not executed.

Transient transport failures are retried (see `max_retries` in [Configuration](configuration.md)); each retry is a `warning:` line on stderr.

## JSON mode

```sh
gg --mode json "list the TODOs in src/" | jq -c 'select(.type == "tool_call_started")'
```

Stdout carries the session header record first (when a session is kept), then one JSON event per line, flushed per line. Stderr is for diagnostics. It is the same event stream the TUI consumes.

| `type` | Fields | Meaning |
|---|---|---|
| `run_started` | | The run begins |
| `turn_started` | | One request to the model begins |
| `assistant_text` | `delta` | A fragment of the answer |
| `thinking` | `delta` | A fragment of reasoning (`reasoning_content`) |
| `tool_call_started` | `name`, `id`, `args` | A tool call begins |
| `tool_call_finished` | `name`, `id`, `output`, `is_error` | The tool's result |
| `turn_ended` | `stop_reason`, optional `usage`, `timings`, `thinking_pct` | The model's turn finished. `stop_reason` is `stop` (a final answer), `tool_calls` (the turn wants tools), `length` (hit `max_tokens`: the answer is a prefix and will be continued), or `stream_cut` (the stream ended early: the same). A turn the user stopped emits no `turn_ended` |
| `run_ended` | `turns` | A clean finish |
| `run_aborted` | | The run was aborted; emitted after a `warned` |
| `warned` | `message` | A non-fatal problem (hook failure, compaction, a retried request, a truncated answer and whether it is being continued…) |

A run ends with either `run_ended` or `run_aborted`, so a consumer can tell them apart:

```json
{"type":"run_started"}
{"type":"turn_started"}
{"type":"tool_call_started","name":"read","id":"call_1","args":{"path":"Cargo.toml"}}
{"type":"tool_call_finished","name":"read","id":"call_1","output":"…","is_error":false}
{"type":"turn_ended","stop_reason":"tool_calls"}
{"type":"turn_started"}
{"type":"assistant_text","delta":"0.1.0"}
{"type":"turn_ended","stop_reason":"stop"}
{"type":"run_ended","turns":2}
```

The prompt is positional (`gg --mode json "…"`) or `-p`; `-` reads it from stdin. `Ctrl+C` and `SIGTERM` trigger a graceful abort (`run_aborted`) and exit with status 143 on `SIGTERM`.

## HTML export

```sh
gg --export 3fa9c1d2              # → <session-file-stem>.html in the current directory
gg --export path/to/session.jsonl report.html
```

`SESSION` is a file path or the id (or unique prefix) that `--resume` takes. The output is one self-contained, dark-themed HTML file: no scripts, no external references; tool calls and thinking collapse with native `<details>`. All conversation text is HTML-escaped. Exporting needs no model and no server.
