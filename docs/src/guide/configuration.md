# Configuration and providers

## Precedence

Highest wins:

1. CLI flags: `-P`/`--provider`, `-m`/`--model`, `--tools`, `--reasoning-effort`, `--thinking-budget`, `--max-turns`, `--session-file`
2. Environment variables
3. `.gg/config.toml` (project, relative to the directory you run `gg` in)
4. `~/.gg/config.toml` (user)
5. Built-in defaults

Merging is shallow: a higher-precedence key replaces the lower value completely. Invalid values from any source cause a startup error. Unknown keys in config files are ignored.

## Config file

```toml
# ~/.gg/config.toml
provider = "strata"
max_tokens = 16384
# context_window = 131072   # auto-probed from Strata /health when unset
# reasoning_effort = "medium"       # strata default
# reasoning_budget_tokens = 4096    # strata default (0 disables)
bash_timeout_secs = 30
max_retries = 3
request_timeout_secs = 600
tools = ["read", "bash", "edit", "write", "grep", "find", "ls", "outline"]

[providers.strata]
base_url = "http://127.0.0.1:8080/v1"
model = "qwen3.8-flash-next-iq3_s"

[providers.deepseek]
model = "deepseek-flash"
# api_key = "…"  # or export DEEPSEEK_API_KEY
```

Top-level keys: `provider`, `model`, `base_url`, `api`, `auth`, `max_tokens`, `api_key`, `context_window`, `replay_reasoning`, `reasoning_effort`, `reasoning_budget_tokens`, `compact_threshold_tokens`, `max_turns`, `bash_timeout_secs`, `max_retries`, `request_timeout_secs`, `tools`. The flat provider-level keys only apply when they belong to the active provider; `max_retries` and `request_timeout_secs` are top-level only.

- `api` is `"open-ai-completions"` (default) or `"anthropic-messages"` (note the spelling: the `openai-completions` shown by `--list-models` is display text, not the config value).
- `auth` is `"auto"` (Bearer for OpenAI, `x-api-key` for Anthropic), `"bearer"`, or `"x-api-key"`.
- `replay_reasoning` controls whether earlier thinking is sent back to the model: `"never"` (`false`), `"turn"` (only assistant turns since the last user message; `strata` default), or `"all"` (`true`).
- `compact_threshold_tokens` caps when auto-compaction triggers (e.g. `65536` on `strata` to stay within local VRAM KV residency; `0` disables).
- `max_turns` caps turns per run (`0` disables); `gg` nudges the model to wrap up at 80% of the budget.
- `bash_timeout_secs` is the ceiling for one `bash` call, in seconds (minimum `1`); a call's `timeout_secs` is clamped to it. A [nested run](delegation.md) started from `bash` lives inside that call, so delegating needs a ceiling of minutes.
- `max_retries` sets the number of extra attempts for transient LLM failures (HTTP 408, 429, 5xx, timeouts, and dropped connections). Backoff starts at 500 ms with jitter; a `Retry-After` header overrides the backoff (capped at 30 s). Set to `0` to disable retries. Other 4xx responses fail immediately.
- `request_timeout_secs` sets the total timeout in seconds for a complete LLM request and response stream (minimum `1`). Responses exceeding this limit are cut off and partial text is kept (see [Output modes and export](modes.md#print-mode)).
- `tools` is the allowlist of tools offered to the model (also `--tools read,grep`). Unknown names are an error. See [Tools](tools.md).
- `context_window` should match the server's real window: compaction and the footer's context percentage use it. When unset on `strata` or a local URL, `gg` probes `/health` at startup for `max_context` (and `kv_resident` when `compact_threshold_tokens` is unset), falling back to `/v1/models` when needed. On `strata`, `gg` also adopts whichever model the server currently has loaded unless overridden via `-m` or `GG_MODEL`. See [Sessions and compaction](sessions-compaction.md).

## Providers

Built-in providers: `strata` (local default), `glm-coding-plan` (alias `zai-coding-plan`), `opencode-go` (routes OpenAI Completions and Anthropic Messages per model), and `deepseek`. Their models, endpoints and limits are in [Providers and models](../reference/providers-and-models.md); `gg --list-models` probes `GET /v1/models` on `strata` and any local OpenAI-compatible provider (such as `llama-server --models-preset`) to show live models, `loaded`/`unloaded` status, and context windows alongside cloud providers and key status; the TUI's `/model` list is refreshed by the same probe in the background.

Patch a built-in or define your own under `[providers.<name>]`. A custom provider must set `base_url` and `model`:

```toml
# ~/.gg/config.toml
provider = "local-vllm"

[providers.local-vllm]
base_url = "http://localhost:8000/v1"
model = "my-model"
api = "open-ai-completions"
api_key_env = "VLLM_API_KEY"
max_tokens = 8192
context_window = 65536
models = ["my-model", { id = "my-long-model", context_window = 262144 }]

[providers.opencode-go]
# override only what you need on a built-in
model = "deepseek-v4-pro"
```

Provider keys: `base_url`, `api`, `auth`, `api_key`, `api_key_env`, `model`, `max_tokens`, `context_window`, `replay_reasoning`, `reasoning_effort`, `reasoning_budget_tokens`, `compact_threshold_tokens`, `models`, and `disabled`. Each `models` entry is a bare id or a table with `id` plus optional `api`, `base_url`, `max_tokens`, `context_window`, `replay_reasoning`, `reasoning_effort`, `reasoning_budget_tokens`, `compact_threshold_tokens`. A provider with `api_key_env` and no key set refuses to start, naming the variable to export.

### Removing a built-in provider

Built-in providers are defined as data using the fields above. Override individual fields or disable a preset with `disabled = true`:

```toml
# ~/.gg/config.toml
[providers.opencode-go]
disabled = true
```

A disabled provider is removed from `/model`, `--list-models`, `-P`, and `-m`. Selecting it with `-P` or `provider/model` returns an error. Disabling is final across config layers: a project `.gg/config.toml` cannot re-enable or patch a disabled provider. Disabling an unknown provider name is ignored, and disabling the currently selected provider causes a startup error.

Choose at the command line with `-P deepseek`, `-m deepseek-flash`, or `-m opencode-go/claude-haiku-5-5` (`provider/model`). Naming conflicting providers in `-P` and `-m` is an error.

## Environment variables

| Variable | Effect |
|---|---|
| `GG_PROVIDER` | Active provider |
| `GG_MODEL` | Model id or `provider/model` |
| `GG_BASE_URL` | Override the endpoint base URL |
| `GG_API_KEY` | API key, overriding every other key source |
| `GG_BASH_TIMEOUT` | Maximum seconds for one bash call |
| `GG_MAX_TURNS` | Maximum turns per run (`0` disables) |
| `GG_MAX_RETRIES` | Extra attempts per transient LLM failure (`0` disables) |
| `GG_REQUEST_TIMEOUT` | Whole-request timeout in seconds, stream included |
| `GG_DEBUG_PROMPT` | Set to `1` to dump the assembled system prompt to stderr |
| `GG_NEST_DEPTH` | Marks a nested run: how many runs sit above it. A headless boot that finds it clamps to its parent; any value other than `1` is an error |
| `GG_PARENT_SESSION` | The parent run's session id, recorded in the nested run's session header |
| `GG_PARENT_MODEL` | The parent run's current model, the nested run's default |
| `GG_TOOL_CAP` | The parent run's tool allowlist, the most a nested run may have |
| `GG_MAX_TURNS_CAP` | The parent run's `max_turns`, the most a nested run may take |
| `GG_NO_HOOKS` | `1` when the parent run's hooks are off, so the nested run's are too |
| `ZHIPU_API_KEY` | Key for `glm-coding-plan` |
| `OPENCODE_API_KEY` | Key for `opencode-go` |
| `DEEPSEEK_API_KEY` | Key for `deepseek` |
| `HOME` | Locates `~/.gg` and `~/.agents`; must be an absolute path |
| `VISUAL`, `EDITOR` | External editor for `Ctrl+G` (`nano` otherwise) |

Model-scoped settings (`max_tokens`, `context_window`, `replay_reasoning`, `reasoning_effort`, `reasoning_budget_tokens`, `compact_threshold_tokens`) are configured per provider or per model in `config.toml`. Use `--reasoning-effort`, `--thinking-budget`, and `/think` for one-off overrides.

The `bash` tool sets `GG_NEST_DEPTH` and the five variables after it in every command it runs. They describe the run for a `gg -p` the command starts: see [What a nested run inherits](delegation.md#what-a-nested-run-inherits).

`gg` also reads `TMUX` to ask the terminal for extended keys.

## Files `gg` reads

| Path | Purpose |
|---|---|
| `~/.gg/config.toml`, `.gg/config.toml` | Configuration |
| `~/.gg/hooks.toml` | [Hooks](hooks.md) (user level only) |
| `~/.gg/AGENTS.md`, `AGENTS.md` | [Context files](context-and-skills.md#agentsmd) |
| `~/.agents/skills/`, `.agents/skills/` | [Skills](context-and-skills.md#skills) |
| `~/.gg/sessions/` | [Sessions](sessions-compaction.md), mode `0600` |
| `~/.gg/history.jsonl` | [Prompt history](tui.md#prompt-history); written by the TUI, mode `0600` |
