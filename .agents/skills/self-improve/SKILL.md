---
name: self-improve
description: "Mine recent gg sessions for friction, corrections and repeated errors, then update AGENTS.md, skills or hook suggestions."
disable-model-invocation: true
---

# Self-Improve

Mine recent gg sessions for friction and route durable findings into repo conventions, skills or hook guardrails.

This is an on-demand procedure. gg never runs the digest or updates conventions after a session; invoke this skill explicitly.

## Steps

1. Run `python3 scripts/session-digest.py`, passing `--session` or `--last` from the skill arguments when given.
2. Inspect only the session entries the digest flags as friction: aborts, repeated tool errors, bash workarounds, user corrections.
3. Route durable findings:
   - Repo conventions and pitfalls: update `AGENTS.md`. Merge or tighten the existing lines; do not append raw bullets.
   - Reusable multi-step procedures: write `.agents/skills/<name>/SKILL.md` or `~/.agents/skills/<name>/SKILL.md`. The `write` tool requires `force: true` for paths outside the working directory.
   - Deterministic command guardrails: propose a `~/.gg/hooks.toml` snippet and wait for user confirmation. Never edit `~/.gg/hooks.toml` silently.
   - One-off mistakes: ignore.
4. Guardrails:
   - Never copy secrets or tokens from session logs into any file.
   - Keep `AGENTS.md` short.
   - Verify the new `SKILL.md` frontmatter has `name` and `description`.
   - Remind the user to run `/reload` in the TUI so the new skill is picked up.
   - The digest is sent to the configured model. Redaction covers known token shapes only, so prefer a local endpoint for sensitive sessions and review snippets before routing them.
