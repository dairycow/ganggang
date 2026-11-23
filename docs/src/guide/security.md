# Security Model

> **No permission gating.** Like pi, `gg` does not ask before tool calls. Safe use means running it where the `bash` tool is allowed to be: a container or VM for untrusted work.

## The trust boundary

- The `bash` tool runs with **your user permissions**, in the session's working directory. There is no sandbox and no approval prompt, by design.
- [Hooks](hooks.md) run with the same permissions and are equally unsandboxed.
- Anything the model reads (files, `@file` attachments, command output, skills) can influence what it does next. Do not point `gg` at a repository or prompt you do not trust on a machine you care about.
- Your messages and tool output are sent to whichever provider you configured. The default, a local Strata server, keeps them on your machine; `gg` itself has no telemetry.

## Guardrails (not sandboxes)

These reduce accidents; none of them contains a determined model:

- **`write` stays in the working directory** unless the call passes `force: true`. `bash` can still write anywhere, and symlinks can escape the check.
- **Hooks are user-level only.** A project `.gg/hooks.toml` is ignored with a warning, so a cloned repository cannot add executable behaviour.
- **`HOME` must be absolute.** A relative or empty `HOME` is ignored for `~/.gg` and `~/.agents`, so user-level config, hooks and skills cannot silently become paths inside the current directory.
- **Bounded everything.** Tool output, attachments, hook output and timeouts all have visible caps; runaway commands are killed with their whole process group.
- **A `pre_tool` hook can deny** calls you want blocked: useful workflow policy, but not a security boundary.

## Practical advice

- Use a container or VM for untrusted repositories.
- Offer fewer tools: `gg --tools read,grep,find,ls` gives a read-only session. (`bash` is the one that can do anything.)
- Keep API keys in environment variables rather than committed config files.
