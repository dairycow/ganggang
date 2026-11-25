# Context and skills

## The system prompt

`gg` builds the system prompt fresh for every request from named sections, in this order: preamble, context files (`AGENTS.md`), skills, then environment (cwd, OS, date, active model). Empty sections are omitted. Tool names, descriptions and JSON schemas ride in the API `tools` array on every request, so the system prompt does not repeat them; the preamble's working-style guidance names only the enabled tools (see [Choosing which tools are offered](tools.md#choosing-which-tools-are-offered)), so a read-only run never advertises `edit` or `bash`. Inspect exactly what the next request carries with `/context` in the TUI, or set `GG_DEBUG_PROMPT=1` to dump it to stderr.

## AGENTS.md

`gg` loads context files, broadest first:

1. `~/.gg/AGENTS.md`
2. `AGENTS.md` from the repository root (the nearest ancestor containing `.git`) down to the current directory

Files closer to the current directory act as more specific instructions. With no repository, discovery runs from the filesystem root. Unreadable files produce a warning, not a failure.

## Skills

A skill is a directory containing a `SKILL.md` (the Agent Skills format):

```text
---
name: release-notes
description: Draft release notes from git history. Use when asked for a changelog.
---
Instructions the model follows when the task matches…
```

Put skills in `~/.agents/skills/<name>/SKILL.md` (user) or `.agents/skills/<name>/SKILL.md` (project; searched from the current directory up to the repository root). Only immediate child directories count. On a name collision the first found wins, and `gg` warns about the skipped one.

Only each skill's **name, description and path** are advertised in the system prompt; the model reads the full instructions with the ordinary `read` tool when a task matches. Skills add no new execution path.

- Frontmatter is simple `key: value` lines (quotes are stripped); unknown keys are ignored. `name` and `description` are required, otherwise the skill is skipped with a warning.
- `disable-model-invocation: true` hides a skill from the prompt while keeping it available on demand.
- `/skills` lists discovered skills; `/skill:<name> [args]` forces one: its instructions become your next message and run immediately.
- `@file` mentions in the arguments work as they do in a plain message: the transcript keeps the arguments you typed, and the attached file content reaches the model with the instructions. The skill's own instructions are never scanned for mentions.
