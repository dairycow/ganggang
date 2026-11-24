# Tools

The model gets seven tools: `read`, `bash`, `edit`, `write`, `grep`, `find`, `ls`. Parameters and descriptions are in the generated [Built-in Tools](../reference/tools.md) reference; this page covers behaviour and limits.

## Principles

- Mutating tools (`bash`, `edit`, `write`) run **one at a time**, in the order the model called them. Contiguous runs of 2+ read-only tools (`read`, `grep`, `find`, `ls`) execute **in parallel** (up to 8 in flight) while preserving call order in events and session history.
- A tool failure is a **value**: the error text goes back to the model, which reads it and retries. Only transport and session I/O errors abort a run.
- Every cap is **visible**: truncated output says so, so the model knows when a result is cut.

## Stopping a run

`Esc` or `Ctrl+C` (or `SIGTERM`) stops a run as it is running, not at the next call boundary:

- A `bash` command has its whole process group killed; the model is told `command aborted by user and killed`.
- A `grep` or `find` walk stops and returns what it found so far, as an error saying the results are partial. A call that had not started does not start.
- A call the user stopped while it ran is not an execution: no `post_tool` [hook](hooks.md) observes it.

## Choosing which tools are offered

```sh
gg --tools read,grep,find,ls -p "review src/ for panics"   # read-only
```

or `tools = [...]` in [config](configuration.md). The flag overrides the config.

## `read`

Reads a text file with 1-based line numbers (`N| text`): at most 2000 lines and 50 KiB per call, with a `[truncated: …]` note when cut. `offset` (1-based line) and `limit` stream sequentially so you can page cleanly past 50 KiB. Non-UTF-8 files are refused.

## `bash`

Runs `bash -c` in the session directory with your permissions. stdout and stderr are merged in arrival order; output is capped at 10 KiB (the command still runs to completion, so the exit code is real) and a non-zero exit is reported as an error result. A per-call `timeout_secs` is clamped to `bash_timeout_secs` (default 30). On timeout the **whole process group** is killed.

## `edit`

Exact-match replacement. Each `old_text` must occur **exactly once**; any miss or ambiguity writes nothing, and the error tells the model which edit failed (including line-numbered nearest-match context on a miss, or match line numbers on ambiguity). All edits are applied in memory first, then written once with a compact hunk preview. Files over 512 KiB are refused.

## `write`

Creates or overwrites a file, creating parent directories. Paths that resolve (lexically, after `..`) outside the working directory are refused unless the call passes `force: true`. This is a guardrail, not a sandbox: `bash` can write anywhere.

## `grep`

Searches file contents using a regular expression (or literal text with `fixed_strings: true`). Respects `.gitignore` and skips `.git`, `target`, `node_modules`, and binary files. Supports `glob` filtering, `case_insensitive`, `context` lines (up to 10), and optional `path` (default `.`). Defaults to 50 results; each matching line is cut at 250 characters; only the first 1 MiB of each file is searched; symlinks are not followed.

## `find`

Matches entry names (or relative paths when `pattern` contains `/`) against a `globset` glob (e.g. `*.rs`, `**/*.md`). Respects `.gitignore` and skips `.git`, `target`, and `node_modules` by default (`include_ignored: true` includes them). The walk is sorted and deterministic and includes dotfiles. Defaults to 50 results with a visible note when it stops.

## `ls`

One directory level: directories first (`name/`), then files with byte sizes, alphabetical, hidden entries included. Not recursive; use `find` to go deeper.
