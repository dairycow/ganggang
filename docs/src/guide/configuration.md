# Configuration & Providers

## Precedence

Highest wins:

1. CLI flags: `-P`/`--provider`, `-m`/`--model`, `--tools`
2. Environment variables
3. `.gg/config.toml` (project, relative to the directory you run `gg` in)
4. `~/.gg/config.toml` (user)
5. Built-in defaults

Merging is shallow: a layer that sets a key wins whole. An invalid value is an error wherever it comes from; a typo'd `GG_MAX_TOKENS` fails loudly instead of silently falling back. Unknown keys in a config file are ignored.

## Config file

```toml
# ~/.gg/config.toml
provider = "strata"
max_tokens = 4096
context_window = 131072   # match `strata launch --context`
bash_timeout_secs = 30
tools = ["read", "bash", "edit", "write", "grep", "find", "ls"]

[providers.strata]
base_url = "http://127.0.0.1:8080/v1"
model = "qwen3.8-flash-next-iq3_s"

[providers.deepseek]
model = "deepseek-flash"
# api_key = "…"  # or export DEEPSEEK_API_KEY
```

Top-level keys: `provider`, `model`, `base_url`, `api`, `auth`, `max_tokens`, `api_key`, `context_window`, `replay_reasoning`, `reasoning_effort`, `bash_timeout_secs`, `tools`. The flat provider-level keys only apply when they belong to the active provider.

- `api` is `"open-ai-completions"` (default) or `"anthropic-messages"` (note the spelling: the `openai-completions` shown by `--list-models` is display text, not the config value).
- `auth` is `"auto"` (Bearer for OpenAI, `x-api-key` for Anthropic), `"bearer"`, or `"x-api-key"`.
- `replay_reasoning` controls whether earlier thinking is sent back to the model.
- `tools` is the allowlist of tools offered to the model (also `--tools read,grep`). Unknown names are an error. See [Tools](tools.md).
- `context_window` should match the server's real window: compaction and the footer's context percentage use it. See [Sessions & Compaction](sessions-compaction.md).

## Providers

Built-in providers: `strata` (local default), `glm-coding-plan` (alias `zai-coding-plan`), `opencode-go` (routes OpenAI Completions and Anthropic Messages per model), and `deepseek`. Their models, endpoints and limits are in [Providers & Models](../reference/providers-and-models.md); `gg --list-models` prints the same with live key status.

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

Provider keys: `base_url`, `api`, `auth`, `api_key`, `api_key_env`, `model`, `max_tokens`, `context_window`, `replay_reasoning`, `reasoning_effort`, and `models`. Each `models` entry is a bare id or a table with `id` plus optional `api`, `base_url`, `max_tokens`, `context_window`, `replay_reasoning`, `reasoning_effort`. A provider with `api_key_env` and no key set refuses to start, naming the variable to export.

Choose at the command line with `-P deepseek`, `-m deepseek-v4-pro`, or `-m opencode-go/claude-haiku-5-5` (`provider/model`). Naming conflicting providers in `-P` and `-m` is an error.

## Environment variables

| Variable | Effect |
|---|---|
| `GG_PROVIDER` | Active provider |
| `GG_MODEL` | Model id or `provider/model` |
| `GG_BASE_URL` | Override the endpoint base URL |
| `GG_API_KEY` | API key, overriding every other key source |
| `GG_MAX_TOKENS` | Max output tokens per request |
| `GG_CONTEXT_WINDOW` | Context window size, in tokens |
| `GG_BASH_TIMEOUT` | Maximum seconds for one bash call |
| `GG_DEBUG_PROMPT` | Set to `1` to dump the assembled system prompt to stderr |
| `ZHIPU_API_KEY` | Key for `glm-coding-plan` |
| `OPENCODE_API_KEY` | Key for `opencode-go` |
| `DEEPSEEK_API_KEY` | Key for `deepseek` |
| `HOME` | Locates `~/.gg` and `~/.agents`; must be an absolute path |
| `VISUAL`, `EDITOR` | External editor for `Ctrl+G` (`nano` otherwise) |

`gg` also reads `TMUX` to ask the terminal for extended keys.

## Files `gg` reads

| Path | Purpose |
|---|---|
| `~/.gg/config.toml`, `.gg/config.toml` | Configuration |
| `~/.gg/hooks.toml` | [Hooks](hooks.md) (user level only) |
| `~/.gg/prompts/*.md` | [Prompt templates](context-skills-prompts.md#prompt-templates) |
| `~/.gg/AGENTS.md`, `AGENTS.md` | [Context files](context-skills-prompts.md#agentsmd) |
| `~/.agents/skills/`, `.agents/skills/` | [Skills](context-skills-prompts.md#skills) |
| `~/.gg/sessions/` | [Sessions](sessions-compaction.md) |
