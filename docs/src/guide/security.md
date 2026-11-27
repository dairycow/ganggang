# Security model

> **No permission gating.** Like pi, `gg` does not ask before tool calls. Run `gg` in a container or VM when working with untrusted code.

## The trust boundary

- The `bash` tool runs with **your user permissions**, in the session's working directory. There is no sandbox and no approval prompt, by design.
- [Hooks](hooks.md) run with the same permissions and are equally unsandboxed.
- Anything the model reads (files, `@file` attachments, command output, skills) can influence what it does next. Do not point `gg` at a repository or prompt you do not trust on a sensitive host.
- Your messages and tool output are sent to whichever provider you configured. While you hold a key for it (`ZHIPU_API_KEY`, `GG_API_KEY` or an `api_key` in your config) the default is the cloud-based `glm-coding-plan` preset; otherwise `gg` uses the local `strata` preset, which keeps them on your machine. A `base_url` you set without naming a provider never receives a provider's own key. `gg` itself has no telemetry.

## Guardrails (not sandboxes)

These reduce accidents:

- **`write` stays in the working directory** unless the call passes `force: true`. `bash` can still write anywhere, and symlinks can escape the check.
- **Project hooks need your trust.** A `.gg/hooks.toml` loads only when its directory is in `trusted_projects` (`~/.gg/config.toml`) or under one that is, so a cloned repository cannot add executable behaviour. Any other project file is left unloaded with a warning.
- **`HOME` must be absolute.** A relative or empty `HOME` is ignored for `~/.gg` and `~/.agents`, so user-level config, hooks and skills cannot silently become paths inside the current directory.
- **Prompt history is plain text.** `~/.gg/history.jsonl` keeps what you submit in the TUI (the newest 500 lines, mode `0600`), secrets pasted into a prompt included. `--no-session` does not turn it off; running without `HOME` keeps the history in memory only.
- **Resource limits.** Tool output, attachments, hook output and timeouts all have visible caps; runaway commands are killed with their whole process group.
- **A `pre_tool` hook can deny** calls you want blocked: useful workflow policy, but not a security boundary.

## Practical advice

- Use a container or VM for untrusted repositories.
- Offer fewer tools: `gg --tools read,grep,find,ls` gives a read-only session. (omitting `bash` prevents arbitrary command execution).
- Keep API keys in environment variables rather than committed config files.
