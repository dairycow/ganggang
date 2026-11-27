# Output modes and export

All modes share one agent core and persist a session by default (`--no-session` opts out).

## TUI

The default on a terminal. See [Interactive TUI](tui.md).

## Print mode

```sh
gg -p "what changed in the last commit?"
echo "explain this" | gg -p -
```

The final answer goes to **stdout**; progress (turn markers, formatted tool-call lines, per-turn token/throughput stats, a final run summary that includes the session's cache hit rate when the provider reports cache reads, and a spinner on a terminal) goes to **stderr**. A consumer that closes the pipe early (`… | head -c 5`) ends the run quietly.

`Ctrl+C` aborts the run: in-flight requests close within about 100 ms, running `bash` commands have their process group killed, except a kept [background job](tools.md#bash), and any partial text already streamed is printed to stdout. A second `Ctrl+C` exits immediately with status 130; `SIGTERM` triggers the same graceful abort, flushes output, and exits with status 143. Both take kept [background jobs](tools.md#bash) down with the process.

### Truncated responses

A response can end early if the provider returns `finish_reason: "length"` (the `max_tokens` limit) or the stream closes before its end marker (timeout or dropped connection). `gg` keeps the partial output, logs a `warning:` line, and asks the model to continue from where it stopped: up to 3 times in a row, 12 times in a run, and within `--max-turns`. Each complete turn between truncations starts the count of 3 again. When the cut answer carried a tool call, the next request asks for it in smaller pieces. A truncation that produced no visible text or tool call is not continued: `gg` warns and stops instead, leaving no blank assistant entry. A turn that spent the whole `max_tokens` on reasoning, with nothing else, is the exception: `gg` asks for the same turn again with `reasoning_effort: "none"`, without recording an entry for the cut turn, and the `warning:` names that field, which asks the endpoint not to reason. A provider with its own thinking switch gets that switch on the retry: `thinking_off` (see [Configuration](configuration.md)) names the `thinking.type` value sent beside `reasoning_effort: "none"`, for endpoints such as z.ai's GLM models that read it and ignore the reasoning fields. A model marked `forced_thinking = true` cannot switch its thinking off, so `gg` does not retry: it warns that there is no retry and stops the run at once. When the retried turn spends the whole `max_tokens` on reasoning too, `gg` warns that the retry went out with reasoning off and the endpoint thought anyway, gives the reasoning token count when the provider reports one, and stops instead of continuing. On the Anthropic Messages API the request has no reasoning field, so the retry sends the same request again and the warning says so. The retried turn alone goes out that way; the turn after it returns to the configured effort. If a second turn in a row spends the whole `max_tokens` on reasoning, reasoning stays off for the rest of the run, with a warning that says so. A server that refuses that request ends the run with a warning that quotes the refusal. The next run starts with the configured reasoning settings. Continuation chunks are joined into a single answer on stdout. Tool calls cut off mid-arguments are not executed and receive an error result.

Transient transport failures are retried (see `max_retries` in [Configuration](configuration.md)), including a stream that drops before its first output; each retry is a `warning:` line on stderr. When a run fails for good, the error is [recorded in the session](sessions-compaction.md#failed-runs).

## Run budgets

Two settings bound one run, and either can be left off:

- `--max-turns N` (or `max_turns`, `GG_MAX_TURNS`) caps the number of requests to the model. At 80% of the budget the next request carries a notice asking the model to wrap up; the last request tells it to answer without tools.
- `--max-seconds SECS` (or `max_seconds`, `GG_MAX_SECONDS`) caps the run's wall-clock time. The clock starts with the run and counts every request and tool call. At 80% of the budget the next request carries a wrap-up notice, and emits `warning: approaching max_seconds (80s/100s); nudging model to wrap up`. Once the budget is spent, `gg` emits `warning: reached max_seconds (100s); asking model for final response` and the next request tells the model to answer without tools. The run then ends with `warning: reached max_seconds (100s); stopping run`, whether the model answered or asked for tools again. Tool calls in that last answer are not run; each gets an error result, so the session stays replayable.

The notices ride the last message already in the history, so each request is the one before with text appended and the provider's prefix cache keeps its hits. A short answer on the last request is not continued. A [nested run](delegation.md) cannot exceed its parent's budget.

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
| `turn_ended` | `stop_reason`, optional `usage`, `timings`, `thinking_pct` | The model's turn finished. `stop_reason` is `stop` (a final answer), `tool_calls` (the model requested tools), `length` (hit `max_tokens`: the answer is a prefix and will be continued when it has visible output, or the turn is asked again with reasoning off when it was nothing but reasoning), or `stream_cut` (the stream closed early and will be continued when it has visible output). A turn the user stopped emits no `turn_ended` |
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

`SESSION` is a file path or the id (or unique prefix) that `--resume` takes. The output is one self-contained, dark-themed HTML file: no scripts, no external references; tool calls and thinking collapse with native `<details>`. All conversation text is HTML-escaped. Exporting runs offline without a model or server.
