# ganggang

A small, sharp agentic harness in Rust. One agent loop, four-plus tools, an
interactive TUI — modelled on [pi](https://github.com/earendil-works/pi),
built to be read.

`gg` defaults to a local [Strata](https://github.com/Niko1221/Strata) server — no cloud, no accounts, no telemetry — and also speaks OpenAI Chat Completions and Anthropic Messages endpoints with built-in provider presets and custom `[providers.<name>]` configs.

## Install & run

```sh
cargo install ganggang         # installs the `gg` binary
```

Or build from source:

```sh
cargo build --release
./target/release/gg            # TUI on a TTY
./target/release/gg -p "…"     # one-shot print mode
```

Backend: start Strata and check it answers —

```sh
strata launch
curl -s 127.0.0.1:8080/health
```

## The pieces

- **Agent loop** (§09 of the design): request → tool calls → results → repeat
  until answered. Sequential tool execution, streaming responses, abort with
  Esc. No turn cap — you are the runaway check; a truncated answer is the one
  exception, continued at most 3 times. A cut stream keeps its partial answer.
- **Tools** (§10): `read`, `bash`, `edit`, `write` — plus `grep`, `find`,
  `ls` for search. Plain values in and out; every truncation is visible in
  the output; tool failures are values the model reacts to, never crashes.
- **Skills** (§11): drop an Agent Skills `SKILL.md` directory into
  `~/.agents/skills/` or `.agents/skills/` — names + descriptions are
  advertised in the system prompt; the model reads the instructions when a
  task matches. `/skill:<name>` forces one.
- **Prompt templates**: `~/.gg/prompts/*.md` (frontmatter `name`/
  `description`) become `/name` editor drafts — `/prompts` lists them.
- **Hooks** (§22): `~/.gg/hooks.toml` runs one-shot `pre_tool` /
  `post_tool` / `run_end` shell commands with JSON on stdin; a `pre_tool`
  hook may deny. Failures warn, never abort. `/hooks` lists; `--no-hooks`
  skips.
- **`@file` mentions**: `@src/main.rs` in the editor attaches the file's
  content to your message (text files, visible 50 KB cap); `user@host`
  stays literal.
- **Context files** (§12): `~/.gg/AGENTS.md` plus `AGENTS.md` from repo root
  to cwd ride along as context.
- **Sessions** (§13): append-only JSONL under `~/.gg/sessions/`. `gg -c`
  continues the newest for the directory; `--resume <id|prefix>` picks one;
  `--no-session` opts out. Crash mid-run? The file still loads — torn tails
  and interrupted tool calls are repaired on resume.
- **Compaction** (§14): when context crowds the window, older history is
  summarized in one extra call and kept as a `Compaction` entry — raw
  history is never destroyed. `/compact [instructions]` runs it on demand.
- **Interfaces** (§15): fullscreen TUI (transcript / editor / footer, with
  markdown rendering and thinking behind Ctrl+T), print mode (`-p`,
  pipeable), JSON mode (`--mode json` — the same event stream the TUI
  consumes, one object per line), and `--export <session> [out.html]` —
  one self-contained HTML transcript.

## Keys & commands (TUI)

Enter send · Alt+Enter / Ctrl+J newline (Shift+Enter too, where the
terminal reports modified keys) · Ctrl+A / Ctrl+E line start/end · Ctrl+K
/ Ctrl+U kill to line end/start · ←/→ move the cursor, ↑/↓ move between
lines (and drive the `/` suggestion list while it is open) · Home/End
line start/end · Esc abort run · Ctrl+O expand
tool calls · Ctrl+X copy last answer · Ctrl+T toggle thinking display ·
Ctrl+G edit the draft in your external editor · Ctrl+R rebuild and restart
(`--watch`) · PageUp/Down scroll · Ctrl+C quit.

**tmux:** Alt+Enter and Ctrl+J work as they are. Shift+Enter needs tmux's
extended keys and a terminal that reports modified keys: add
`set -g extended-keys on` (and, if you have changed it,
`set -g extended-keys-format csi-u`, the default) to `~/.tmux.conf`,
reload it, and restart gg — gg asks for the mode when it starts. GNOME
Terminal/VTE does not report Shift+Enter at all, so use Alt+Enter there.

Typing a `/` command opens a suggestion list above the editor (the
built-ins, your `~/.gg/prompts/*.md` templates, and one row per skill
once `/skill:` is typed): ↑/↓ move the highlight, Tab completes it,
Enter still sends what you typed.

**External editor** (Ctrl+G): your draft opens in `$VISUAL`, else
`$EDITOR`, else `nano` — arguments are fine (`code --wait`, for editors
that return straight away). Whatever you save replaces the composer when
the editor exits; nothing is sent until Enter, and a run in flight keeps
going behind the editor. If the editor exits with an error the composer is
left alone and an edited draft is kept where the warning says. An editor
that returns at once (a GUI editor without `--wait`) leaves a short note
instead of failing silently.

`/new /resume /compact /skills /session /context /prompts /hooks /model
/skill:<name> /quit` · `!cmd` runs shell with output entering the model's
context (`!!` doesn't).

## Configuration & Providers (§16)

Precedence: CLI flags (`-P`/`--provider`, `-m`/`--model`, `--tools`) > environment
(`GG_PROVIDER`, `GG_BASE_URL`, `GG_MODEL`, `GG_MAX_TOKENS`, `GG_API_KEY`,
`GG_CONTEXT_WINDOW`, `GG_BASH_TIMEOUT`, `GG_MAX_RETRIES`, `GG_REQUEST_TIMEOUT`,
`GG_DEBUG_PROMPT=1` to dump the
assembled system prompt to stderr, plus provider env vars `ZHIPU_API_KEY`,
`OPENCODE_API_KEY`, `DEEPSEEK_API_KEY`) > `.gg/config.toml` >
`~/.gg/config.toml` > defaults.

Transient LLM transport failures are retried: connection errors, request
timeouts, and HTTP 408/429/5xx earn up to `max_retries` extra attempts
(default 3) with exponential backoff from 500 ms and a little jitter, and a
provider's `Retry-After` (seconds, capped at 30 s) wins over the backoff.
Any other 4xx fails at once, and a stream that already delivered a delta is
never replayed — a cut mid-answer never becomes a duplicated answer.
Each retry prints a `warning:` line to stderr; nothing above the HTTP client
has to know retries happened. `max_retries = 0` turns retrying off.

One LLM request gets `request_timeout_secs` (default 600) for its whole
response — head and body, so a stream too — and connecting stays a fixed
10 s. A stream whose body ends before the protocol says it is done, and an
answer that hits `max_tokens` (`finish_reason: length`), are both kept and
continued: the partial answer stays on screen and in the session, the loop
appends "continue exactly where you stopped", and the run asks again — at
most 3 times per run. Tool calls from a truncated turn are never executed;
each gets an `is_error` result ("not executed: response was truncated") so
the replayed session stays valid. A continued turn's `turn_ended` event
carries `stop_reason: "length"` or `"stream_cut"` instead of `"stop"`, and
print mode prints the pieces as one answer. Every case warns and the turn
ends cleanly once the budget is spent — never a hang, never an unbounded
loop.

Built-in providers: `strata` (local default), `glm-coding-plan` (Z.AI,
alias `zai-coding-plan`), `opencode-go` (routes OpenAI Completions and
Anthropic Messages per model), and `deepseek`. Custom `[providers.<name>]`
sections are also supported (`base_url`, `model`, optional `api =
"openai-completions"` | `"anthropic-messages"`, `api_key_env`, etc.).

```sh
gg --list-models
gg -P deepseek -p "…"
gg -m opencode-go/claude-haiku-5-5 -p "…"
```

```toml
provider = "strata"
max_tokens = 4096
context_window = 131072   # match `strata launch --context`
bash_timeout_secs = 30
max_retries = 3           # extra attempts on a transient transport failure
request_timeout_secs = 600  # whole-response timeout (a stream included)
tools = ["read", "bash", "edit", "write", "grep", "find", "ls"]

[providers.strata]
base_url = "http://127.0.0.1:8080/v1"
model = "qwen3.8-flash-next-iq3_s"

[providers.deepseek]
model = "deepseek-flash"
# api_key = "…"  # or export DEEPSEEK_API_KEY
```

Hooks are outside this precedence: they live in `~/.gg/hooks.toml` (user
level only) and are never read from a project.

## Hooks (§22)

One-shot shell commands at three loop points: `pre_tool` (can deny),
`post_tool` (observe), `run_end` (observe, clean finishes only). Each hook
gets one JSON object on stdin; a `pre_tool` hook may answer

```json
{"decision":"deny","reason":"blocked by the user guard hook"}
```

A denial is a normal tool error the model reads and reacts to. A hook that
crashes, times out, or prints garbage is a warning and the run continues
(fail-open). Hooks share the bash tool's timeout and process-group kill.

```toml
# ~/.gg/hooks.toml
[[hooks]]
event = "pre_tool"              # pre_tool | post_tool | run_end
tools = ["bash"]                # omit or ["*"] for all; exact names
command = "~/.gg/hooks/guard-bash.sh"
timeout_secs = 5                # default 10

[[hooks]]
event = "post_tool"
tools = ["edit", "write"]
command = "./scripts/format.sh"

[[hooks]]
event = "run_end"
command = "notify-send gg 'run finished'"
```

There is no plugin runtime, no argument rewriting, and no approval prompt.
A project `.gg/hooks.toml` is ignored with a startup warning — a cloned
repository cannot add executable behavior. `--no-hooks` skips loading;
`/hooks` lists what loaded. The full protocol is in design §22.

## Security stance

> No permission gating: like pi, ganggang does not ask before tool calls.
> Safe use = run it where the bash tool is allowed to be (container/VM for
> untrusted work).

The `bash` tool runs with your user permissions, in the session's working
directory. There is no sandbox and no approval prompt — by design. Point it
at a container or VM when the work is untrusted. Hooks (§22) run with the
same permissions and are equally unsandboxed; a `pre_tool` deny is workflow
policy, not a security boundary.

## Watch mode (development)

`gg --watch` turns the harness into its own dev loop: it watches `src/` plus
`Cargo.toml`/`Cargo.lock`, runs `cargo build` once an edit settles, and
re-execs the fresh binary on the same session. The profile follows the binary
you launched (`target/debug` or `target/release`), the footer shows
`watch debug · watching`, and every restart re-reads config, prompt templates,
skills and `AGENTS.md`.

A build that succeeds while a run is in flight waits for idle. With unsent
text in the composer the restart holds at `rebuild ready · Ctrl+R` — nothing
typed is lost either way: Ctrl+R saves the draft beside the session and the
new process puts it back in the composer. A failed build stays in the
transcript and keeps the old binary running until the next edit (or Ctrl+R).

`--watch` refuses `--no-session` (a restart needs a session file) and refuses
any workspace that does not build the ganggang `gg` binary, so it only ever
execs the binary this workspace just produced. Watch runs materialize the
session file at boot; ordinary runs keep creating it lazily, and header-only
watch sessions sort below real conversations in `-c`/`/resume`.
`scripts/watch-live.sh` is the live gate.

## Development

```sh
cargo fmt --all && cargo test && cargo clippy --all-targets -- -D warnings
scripts/smoke.sh      # live acceptance against a running Strata (print mode)
scripts/hooks-live.sh # hooks: deny path, observe paths, --no-hooks, project hooks
scripts/tui-live.sh   # the TUI's terminal hand-off through tmux: Ctrl+G, !, tty state
scripts/watch-live.sh # --watch: rebuild, re-exec, session continuity, draft carry
```

The spec is `design.html` — read it first; `TASKS.md` tracks the build.
