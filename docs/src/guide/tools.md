# Tools

The model gets seven tools: `read`, `bash`, `edit`, `write`, `grep`, `find`, `ls`. Parameters and descriptions are in the generated [Built-in Tools](../reference/tools.md) reference; this page covers behaviour and limits.

## Principles

- Tools run **one at a time**, in the order the model called them.
- A tool failure is a **value**: the error text goes back to the model, which reads it and retries. Only transport and session I/O errors abort a run.
- Every cap is **visible**: truncated output says so, so the model never mistakes a cut for the whole.

## Choosing which tools are offered

```sh
gg --tools read,grep,find,ls -p "review src/ for panics"   # read-only
```

or `tools = [...]` in [config](configuration.md). The flag overrides the config.

## `read`

Reads a text file: at most 2000 lines and 50 KiB per call, with a `[truncated: …]` note when cut. `offset` (1-based line) and `limit` page through large files. Non-UTF-8 files are refused.

## `bash`

Runs `bash -c` in the session directory with your permissions. stdout and stderr are merged in arrival order; output is capped at 10 KiB (the command still runs to completion, so the exit code is real) and a non-zero exit is reported as an error result. A per-call `timeout_secs` is clamped to `bash_timeout_secs` (default 30). On timeout the **whole process group** is killed.

## `edit`

Exact-match replacement. Each `old_text` must occur **exactly once**; any miss or ambiguity writes nothing, and the error tells the model which edit failed. All edits are applied in memory first, then written once. Files over 50 KiB are refused, because editing a truncated view would destroy the tail.

## `write`

Creates or overwrites a file, creating parent directories. Paths that resolve (lexically, after `..`) outside the working directory are refused unless the call passes `force: true`. This is a guardrail, not a sandbox: `bash` can write anywhere.

## `grep`

Searches file contents for a **literal substring** (not a regex). Defaults to 50 results; each matching line is cut at 250 characters; only the first 1 MiB of each file is searched; binary files and `.git` are skipped; symlinks are not followed. For regex, alternation, or case-insensitivity, the model uses `bash`.

## `find`

Matches entry **names** against a glob with `*` and `?` (no `**`, no character classes). The walk is sorted and deterministic and includes hidden entries. Defaults to 50 results with a visible note when it stops.

## `ls`

One directory level: directories first (`name/`), then files with byte sizes, alphabetical, hidden entries included. Not recursive; use `find` to go deeper.
