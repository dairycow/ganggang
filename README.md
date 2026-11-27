# ganggang

A small, sharp agentic harness in Rust. One agent loop, eight tools, and a terminal UI, modelled on [pi](https://github.com/earendil-works/pi).

**[Read the user guide](https://ganggang.io)**

`gg` defaults to the `glm-coding-plan` preset and its `glm-5.3-flash` model while you hold a key for it (`ZHIPU_API_KEY`, `GG_API_KEY` or one in your config), and to a local [Strata](https://github.com/Niko1221/Strata) server otherwise. It also speaks OpenAI Chat Completions and Anthropic Messages endpoints, with built-in provider presets and custom `[providers.<name>]` configs.

## Install and run

```sh
cargo install ganggang         # installs the `gg` binary
```

Or build from source:

```sh
cargo build --release
./target/release/gg            # TUI on a TTY
./target/release/gg -p "…"     # one-shot print mode
```

With a Z.ai coding plan key, the default provider is ready to use:

```sh
export ZHIPU_API_KEY=...         # https://z.ai
./target/release/gg --list-models
```

Without the key, `gg` starts on a local Strata server instead. With the key, select Strata with `gg -P strata`:

```sh
strata launch
curl -s 127.0.0.1:8080/health
```

## What it does

- **Agent loop**: request, tool calls, results, repeat until the model answers. Read-only tool calls in a contiguous run dispatch concurrently, responses stream, and `--max-turns` caps the run. A truncated answer is kept and continued at most 3 times. Esc (or Ctrl+C) aborts the run promptly.
- **Tools**: `read`, `bash`, `edit`, `write`, plus `grep`, `find`, `ls` and `outline` for search and file structure. Every truncation is visible in the output, and tool failures are returned to the model as values rather than crashing the run.
- **Skills**: drop an Agent Skills `SKILL.md` directory into `~/.agents/skills/` or `.agents/skills/`. Names and descriptions are advertised in the system prompt; the model reads the instructions when a task matches. `/skill <name>` forces one (arguments expand `@file` mentions).
- **Delegation**: a `bash` command can start a headless `gg -p` subagent. Nested runs keep the parent's tools, hooks, model and limits, and record the parent in their session. The repository's `delegate` skill wraps the pattern: a read-only job runs in place, and a job that edits runs in a git worktree of its own, then is reviewed, gated and landed on `main` by fast-forward.
- **Hooks**: `~/.gg/hooks.toml`, and a `.gg/hooks.toml` in a project you list in `trusted_projects`, run one-shot `pre_tool`, `post_tool` and `run_end` shell commands with JSON on stdin; a `pre_tool` hook may deny. Hook failures log a warning and do not abort the run. `/hooks` lists; `--no-hooks` skips.
- **`@file` mentions**: `@src/main.rs` in the editor attaches the file's content to your message (text files, visible 50 KiB cap); `user@host` stays literal.
- **Context files**: `~/.gg/AGENTS.md` plus `AGENTS.md` from the repo root to the cwd are loaded into the system prompt.
- **Sessions**: append-only JSONL under `~/.gg/sessions/`. `gg -c` continues the newest for the directory; `--resume <id|prefix>` picks one; `--session-file <path>` targets an exact file; `--no-session` opts out. Torn tails and interrupted tool calls are repaired on resume.
- **Compaction**: when context nears the window, older history is summarised in one extra call and kept as a `Compaction` entry. Original history stays in the file. `/compact [instructions]` runs it on demand.
- **Interfaces**: fullscreen TUI with markdown rendering and thinking behind Ctrl+T, pipeable print mode (`-p`), JSON mode (`--mode json`), and `--export <session> [out.html]` for a self-contained HTML transcript.

## Configuration

Precedence: CLI flags > environment (`GG_PROVIDER`, `GG_BASE_URL`, `GG_MODEL`, `GG_API_KEY`, `GG_BASH_TIMEOUT`, `GG_NESTED_BASH_TIMEOUT`, `GG_MAX_RETRIES`, `GG_REQUEST_TIMEOUT`, `GG_MAX_TURNS`, `GG_DEBUG_PROMPT=1`, plus provider keys `ZHIPU_API_KEY`, `OPENCODE_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY`) > `.gg/config.toml` > `~/.gg/config.toml` > defaults.

```toml
# ~/.gg/config.toml
# provider = "glm-coding-plan"    # names it outright; unset, the first default you hold a key for wins
max_tokens = 16384
# context_window = 131072   # auto-probed from Strata /health when unset
bash_timeout_secs = 300
nested_bash_timeout_secs = 600
max_retries = 5           # extra attempts on a transient transport failure
request_timeout_secs = 600  # whole-response timeout (a stream included)
tools = ["read", "bash", "edit", "write", "grep", "find", "ls", "outline"]

[providers.strata]
base_url = "http://127.0.0.1:8080/v1"
model = "qwen3.8-flash-next-iq3_s"

[providers.deepseek]
model = "deepseek-flash"
# api_key = "…"  # or export DEEPSEEK_API_KEY
```

## Security

Like pi, `gg` does not prompt for tool permissions. The `bash` tool and hooks run unsandboxed with your user permissions in the working directory. Run `gg` in a container or VM for untrusted work. A `pre_tool` deny is workflow policy, not a security boundary.

## Development

```sh
cargo fmt --all && cargo test && cargo clippy --all-targets -- -D warnings
git config core.hooksPath .githooks   # once per clone: gate commits with scripts/pre-commit-check.sh
scripts/pre-commit-check.sh           # the commit gate: fmt check, clippy, tests
scripts/smoke.sh            # live acceptance against a running Strata (print mode)
scripts/eval.sh             # local-compute eval suite (tool discipline, tokens, latency)
scripts/hooks-live.sh       # hooks: deny path, observe paths, --no-hooks, project hooks
scripts/tui-live.sh         # the TUI's terminal hand-off through tmux
scripts/watch-live.sh       # --watch: rebuild, re-exec, session continuity, draft carry
scripts/sync-docs.sh        # regenerate and verify the user guide's reference pages
scripts/session-digest.py   # digest sessions for friction: aborts, tool errors, corrections
scripts/improve-loop.sh     # gated improvement iterations against the eval baseline
```

`scripts/improve-loop.sh` and `scripts/session-digest.py` run on demand. `gg` never invokes either after a session. See [Improving the harness](https://ganggang.io/improving-the-harness.html) for the loop and the two self-improvement skills.

The user guide lives in `docs/` (mdBook). `AGENTS.md` explains how docs stay in sync.
