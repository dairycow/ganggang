# Context, skills and prompts

## The system prompt

`gg` builds the system prompt fresh for every request from named sections, in this order: preamble, context files (`AGENTS.md`), tools, skills, then working directory (cwd, OS, date). Empty sections are omitted. Inspect exactly what the next request carries with `/context` in the TUI, or set `GG_DEBUG_PROMPT=1` to dump it to stderr.

## AGENTS.md

`gg` loads context files, broadest first:

1. `~/.gg/AGENTS.md`
2. `AGENTS.md` from the repository root (the nearest ancestor containing `.git`) down to the current directory

Deeper files read as refinements of shallower ones. With no repository, discovery runs from the filesystem root. Unreadable files produce a warning, not a failure.

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

## Prompt templates

Markdown files in `~/.gg/prompts/` become `/name` commands:

```text
---
name: review
description: Review a diff carefully
---
Review this diff line by line. Flag bugs first, style last.
```

`/review src/main.rs` drops the body (plus your arguments) into the composer as an editable **draft**; nothing is sent until you press `Enter`. Frontmatter is optional: without a `name`, the file name stem is used. `/prompts` lists templates.

A template whose name contains whitespace, matches a built-in command, or starts with `skill:` is skipped with a warning, as are duplicates (first, by file name, wins).
