# Configuration and providers

## Precedence

Highest wins:

1. CLI flags: `-P`/`--provider`, `-m`/`--model`, `--tools`, `--reasoning-effort`, `--thinking-budget`, `--max-turns`, `--max-seconds`, `--session-file`
2. Environment variables
3. `.gg/config.toml` (project, relative to the directory you run `gg` in)
4. `~/.gg/config.toml` (user)
5. Built-in defaults

Merging is shallow: a higher-precedence key replaces the lower value completely. Invalid values from any source cause a startup error. Unknown keys in config files are ignored.

## Config file

```toml
# ~/.gg/config.toml
# provider = "glm-coding-plan"    # names it outright: no ranking, and its key is required
max_tokens = 16384
# context_window = 131072   # auto-probed from Strata /health when unset
# reasoning_effort = "high"         # empty string clears it
# reasoning_budget_tokens = 8192    # 0 disables
bash_timeout_secs = 30
max_retries = 5
request_timeout_secs = 600
tools = ["read", "bash", "edit", "write", "grep", "find", "ls", "outline"]

[providers.strata]
base_url = "http://127.0.0.1:8080/v1"
model = "qwen3.8-flash-next-iq3_s"

[providers.deepseek]
model = "deepseek-flash"
# api_key = "…"  # or export DEEPSEEK_API_KEY
```

Top-level keys: `provider`, `model`, `base_url`, `api`, `auth`, `max_tokens`, `api_key`, `context_window`, `replay_reasoning`, `reasoning_effort`, `reasoning_budget_tokens`, `compact_threshold_tokens`, `max_turns`, `max_seconds`, `bash_timeout_secs`, `max_retries`, `request_timeout_secs`, `tools`. The flat provider-level keys only apply when they belong to the active provider; `max_retries` and `request_timeout_secs` are top-level only.

- `api` is `"open-ai-completions"` (default) or `"anthropic-messages"` (note the spelling: the `openai-completions` shown by `--list-models` is display text, not the config value).
- `auth` is `"auto"` (Bearer for OpenAI, `x-api-key` for Anthropic), `"bearer"`, or `"x-api-key"`.
- `replay_reasoning` controls whether earlier thinking is sent back to the model: `"never"` (`false`), `"turn"` (only assistant turns since the last user message; `strata` default), or `"all"` (`true`).
- `compact_threshold_tokens` caps when auto-compaction triggers (e.g. `65536` on `strata` to stay within local VRAM KV residency; `0` disables).
- `max_turns` caps turns per run (`0` disables); `gg` nudges the model to wrap up at 80% of the budget.
- `max_seconds` caps the wall-clock time of one run, in seconds (`0` disables). At 80% of the budget `gg` nudges the model to wrap up. Once the budget is spent, the next request tells the model to answer without tools and the run ends with that answer. A budget of under 5 s has no nudge. See [Run budgets](modes.md#run-budgets).
- `bash_timeout_secs` is the ceiling for one `bash` call, in seconds (minimum `1`); a call's `timeout_secs` is clamped to it. A [nested run](delegation.md) started from `bash` lives inside that call, so delegating needs either a ceiling of minutes or `background: true` on the call, which keeps the run as a [background job](tools.md#bash) past the ceiling.
- `max_retries` sets the number of extra attempts for transient LLM failures (HTTP 408, 429, 5xx, timeouts, and dropped connections). Backoff starts at 1000 ms with jitter and doubles with each retry, up to 30 s; a `Retry-After` header overrides the backoff (also capped at 30 s). A stream that fails before its first output, such as a connection that drops or a body that ends empty, is retried on the same budget. Once any text has streamed, a cut answer is [continued](modes.md#truncated-responses) rather than retried, so nothing is shown twice. Set to `0` to disable retries. Other 4xx responses fail immediately. When every attempt fails, the run ends and the error is [recorded in the session](sessions-compaction.md#failed-runs).
- `request_timeout_secs` sets the total timeout in seconds for a complete LLM request and response stream (minimum `1`). Responses exceeding this limit are cut off and partial text is kept (see [Output modes and export](modes.md#print-mode)).
- `tools` is the allowlist of tools offered to the model (also `--tools read,grep`). Unknown names are an error. See [Tools](tools.md).
- `context_window` should match the server's real window: compaction and the footer's context percentage use it. When unset on `strata` or a local URL, `gg` probes `/health` at startup for `max_context` (and `kv_resident` when `compact_threshold_tokens` is unset), falling back to `/v1/models` when needed. On `strata`, `gg` also adopts whichever model the server currently has loaded unless overridden via `-m` or `GG_MODEL`. See [Sessions and compaction](sessions-compaction.md).

## Providers

When nothing names a provider (no `provider` key, `-P`, `GG_PROVIDER`, or a `provider/model` prefix in `-m` or `GG_MODEL`), `gg` takes the first of the ranked defaults you hold a key for: `glm-coding-plan` while you hold a key for it (`ZHIPU_API_KEY`, `GG_API_KEY`, or an `api_key` in this file, at the top or under `[providers.glm-coding-plan]`), otherwise `strata`, which needs no key. A bare `model` key, a bare `-m` id or a bare `GG_MODEL` names no provider: it applies to whichever of the two is chosen, with the flat keys set for it, and the notice names that model. When the choice passes over `glm-coding-plan`, `gg` prints one line on stderr, naming the model in use as a local `/health` probe may have replaced it:

```text
gg: passed over glm-coding-plan (no ZHIPU_API_KEY): using strata/qwen3.8-flash-next-iq3_s
```

Naming the provider (`provider = "glm-coding-plan"` or `-P glm-coding-plan`) turns the ranking off: `gg` uses what you named, and reports the missing key if there is none.

A `base_url` key, or `GG_BASE_URL`, is for a server of your own. With no provider named, no key of a provider's own is sent to that address: `gg` takes the first ranked default that neither needs nor holds a key, sends whatever key you gave outright (`GG_API_KEY` or a top-level `api_key`), and the notice says so when that passes one over. To send a provider's own key there, name the provider in the same file, or set the address with `GG_BASE_URL` and use `-P`. If no default can run without a key of its own, `gg` refuses to start.

The ranked list is generated into [Providers and models](../reference/providers-and-models.md).

Built-in providers: `glm-coding-plan` (alias `zai-coding-plan`), `strata` (local), `opencode-go` (routes OpenAI Completions and Anthropic Messages per model), `deepseek`, and `openrouter` (OpenAI Completions). Their models, endpoints and limits are in [Providers and models](../reference/providers-and-models.md); `gg --list-models` probes `GET /v1/models` on `strata` and any local OpenAI-compatible provider (such as `llama-server --models-preset`) to show live models, `loaded`/`unloaded` status, and context windows alongside cloud providers and key status; the TUI's `/model` list is refreshed by the same probe in the background.

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

Provider keys: `base_url`, `api`, `auth`, `api_key`, `api_key_env`, `model`, `max_tokens`, `context_window`, `replay_reasoning`, `reasoning_effort`, `reasoning_budget_tokens`, `thinking_off`, `compact_threshold_tokens`, `models`, and `disabled`. Each `models` entry is a bare id or a table with `id` plus optional `api`, `base_url`, `max_tokens`, `context_window`, `replay_reasoning`, `reasoning_effort`, `reasoning_budget_tokens`, `thinking_off`, `compact_threshold_tokens`. A provider with `api_key_env` and no key set refuses to start, naming the variable to export.

- `thinking_off` is the `thinking.type` value sent with a request that asks for no reasoning, beside `reasoning_effort: "none"`. Set it on providers that read this switch instead, such as z.ai's GLM models (`thinking_off = "disabled"`). See [Truncated responses](modes.md#truncated-responses).

### Removing a built-in provider

Built-in providers are defined as data using the fields above. Override individual fields or disable a preset with `disabled = true`:

```toml
# ~/.gg/config.toml
[providers.opencode-go]
disabled = true
```

A disabled provider is removed from `/model`, `--list-models`, `-P`, and `-m`. Selecting it with `-P` or `provider/model` returns an error. Disabling is final across config layers: a project `.gg/config.toml` cannot re-enable or patch a disabled provider. Disabling an unknown provider name is ignored, and disabling the currently selected provider causes a startup error.

Choose at the command line with `-P deepseek`, `-m deepseek-flash`, or `-m opencode-go/claude-haiku-5-5` (`provider/model`). Naming conflicting providers in `-P` and `-m` is an error.

Only the first segment names the provider. An OpenRouter model id has a vendor prefix of its own, so its spec has two slashes: `-m openrouter/upstage/solar-mini-4-5` selects `upstage/solar-mini-4-5`. Any other OpenRouter model can be named the same way. A vendor that is also a provider name (`deepseek`) needs the full spec: `-P openrouter -m deepseek/deepseek-v4.1-flash` reads `deepseek` as the provider and reports a conflict. A model the preset does not list takes the provider's `context_window` (131072), so give it a `models` entry with its real window. The `openrouter` provider sends OpenRouter its attribution headers on every call — the site `https://ganggang.io` and the name `ganggang` — so the app shows up in OpenRouter's app page and rankings. Both can be overridden for a private deployment with `GG_OPENROUTER_REFERER` and `GG_OPENROUTER_TITLE`; see [Environment variables](#environment-variables).

```toml
# ~/.gg/config.toml
[providers.openrouter]
models = [
  { id = "upstage/solar-mini-4-5", context_window = 524288 },
  { id = "anthropic/claude-haiku-4.5", context_window = 200000 },
]
```

## Environment variables

| Variable | Effect |
|---|---|
| `GG_PROVIDER` | Active provider |
| `GG_MODEL` | Model id or `provider/model` |
| `GG_BASE_URL` | Override the endpoint base URL |
| `GG_API_KEY` | API key, overriding every other key source |
| `GG_BASH_TIMEOUT` | Maximum seconds for one bash call |
| `GG_MAX_TURNS` | Maximum turns per run (`0` disables) |
| `GG_MAX_SECONDS` | Maximum wall-clock seconds per run (`0` disables) |
| `GG_MAX_RETRIES` | Extra attempts per transient LLM failure (`0` disables) |
| `GG_REQUEST_TIMEOUT` | Whole-request timeout in seconds, stream included |
| `GG_DEBUG_PROMPT` | Set to `1` to dump the assembled system prompt to stderr |
| `GG_NEST_DEPTH` | Marks a nested run: how many runs sit above it. A headless boot that finds it clamps to its parent; any value other than `1` is an error |
| `GG_PARENT_SESSION` | The parent run's session id, recorded in the nested run's session header |
| `GG_PARENT_MODEL` | The parent run's current model, the nested run's default |
| `GG_TOOL_CAP` | The parent run's tool allowlist, the most a nested run may have |
| `GG_MAX_TURNS_CAP` | The parent run's `max_turns`, the most a nested run may take |
| `GG_MAX_SECONDS_CAP` | The parent run's `max_seconds`, the most a nested run may take |
| `GG_NO_HOOKS` | `1` when the parent run's hooks are off, so the nested run's are too |
| `ZHIPU_API_KEY` | Key for `glm-coding-plan` |
| `OPENCODE_API_KEY` | Key for `opencode-go` |
| `DEEPSEEK_API_KEY` | Key for `deepseek` |
| `OPENROUTER_API_KEY` | Key for `openrouter` |
| `GG_OPENROUTER_REFERER` | Site name the `openrouter` provider sends as its OpenRouter attribution header; defaults to `https://ganggang.io` |
| `GG_OPENROUTER_TITLE` | App name the `openrouter` provider sends as its OpenRouter attribution header; defaults to `ganggang` |
| `HOME` | Locates `~/.gg` and `~/.agents`; must be an absolute path |
| `VISUAL`, `EDITOR` | External editor for `Ctrl+G` (`nano` otherwise) |

Model-scoped settings (`max_tokens`, `context_window`, `replay_reasoning`, `reasoning_effort`, `reasoning_budget_tokens`, `thinking_off`, `compact_threshold_tokens`) are configured per provider or per model in `config.toml`. Use `--reasoning-effort`, `--thinking-budget`, and `/think` for one-off overrides.

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
