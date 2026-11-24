# Tools

`gg` provides eight built-in tools: `read`, `bash`, `edit`, `write`, `grep`, `find`, `ls`, `outline`. Parameters and descriptions are in the generated [Built-in tools](../reference/tools.md) reference; this page covers behaviour and limits.

## Principles

- Mutating tools (`bash`, `edit`, `write`) run **one at a time**, in the order the model called them. Contiguous runs of 2+ read-only tools (`read`, `grep`, `find`, `ls`, `outline`) execute **in parallel** (up to 8 in flight) while preserving call order in events and session history.
- Tool failures are returned to the model as tool results so it can recover. Only transport and session I/O errors abort a run.
- Truncated outputs include an explicit truncation marker.

## Stopping a run

`Esc` or `Ctrl+C` (or `SIGTERM`) aborts an active run immediately:

- A `bash` command has its whole process group killed; the model is told `command aborted by user and killed`, followed by any output received before the kill.
- A `grep` or `find` walk stops and returns what it found so far, as an error saying the results are partial. A call that had not started does not start. An `outline` scan stops and reports `aborted by user`.
- Aborted tool calls do not trigger `post_tool` [hooks](hooks.md).

## Choosing which tools are offered

```sh
gg --tools read,outline,grep,find,ls -p "review src/ for panics"   # read-only
```

or `tools = [...]` in [config](configuration.md). The flag overrides the config.

## `read`

Reads a text file with 1-based line numbers (`N| text`). The default page is 400 lines or 24 KiB, whichever comes first, with a `[truncated: ...]` note. `offset` (1-based line) and `limit` stream sequentially so you can page cleanly past 50 KiB; `limit` caps at 2000 lines. A line longer than 500 characters is cut with `…` so a minified file cannot fill the page. A negative `offset` (for example `offset: -30`) reads the last 30 lines and keeps their real line numbers. Non-UTF-8 files are refused.

## `outline`

Returns a file's structure as one line per symbol or heading: `12-48: impl Point`, indented by nesting, so you can jump straight to `read(offset, limit)` instead of reading the whole file. The first line is `<path> (<n> lines)`. Scanners cover Rust, Python, JS/TS, Go, C/C++/Java/C#, shell, Markdown and TOML, with a keyword fallback for other text; Rust comments and string bodies are skipped, so braces inside them do not count. Files over 1 MiB, binary files and non-UTF-8 files are refused. Up to 200 entries by default (`max_entries`, capped at 500) and at most 12 KiB of output, each with a visible `[truncated: ...]` notice. Signatures longer than 140 characters are cut with `…`. A file with no recognised symbols says so and points at `read`.

## `bash`

Runs `bash -c` in the session directory with your permissions. stdout and stderr are merged in arrival order. Output over 10 KiB keeps the first quarter and the last three quarters, with a `[... N bytes omitted ...]` marker between; the command runs to completion so its exit code is accurate. A non-zero exit is reported as an error result. A per-call `timeout_secs` is clamped to `bash_timeout_secs` (default 30). On timeout the **whole process group** is killed, and output received before the kill is returned after `[partial output]`. A command that re-implements a native tool (`cat`, `head`, `sed`, `grep`, `find`, `ls`, inline python touching files, or inline `perl -i` rewriting a file) gets a `[hint: ...]` line after its output pointing at the native tool; pipelines such as `cargo test | tail -20` are untouched.

## `edit`

Exact-match replacement. Each `old_text` must occur **exactly once**; any miss or ambiguity writes nothing, and the error tells the model which edit failed (including line-numbered nearest-match context on a miss, or match line numbers on ambiguity). An edit can set `replace_all: true` to rewrite every non-overlapping occurrence instead of requiring a unique match, and its hunk preview names the occurrence count. On a miss the error also names the first differing line, quoting the expected and actual text and flagging an indentation-only difference. All edits are applied in memory first, then written once with a compact hunk preview. Files over 512 KiB are refused.

## `write`

Creates or overwrites a file, creating parent directories. Paths that resolve (lexically, after `..`) outside the working directory are refused unless the call passes `force: true`. This guardrail prevents accidental writes outside the workspace but is not a sandbox (`bash` can write anywhere).

## `grep`

Searches file contents using a regular expression (or literal text with `fixed_strings: true`). Respects `.gitignore` and skips `.git`, `target`, `node_modules`, and binary files. Results are grouped by file: `<path>:` followed by `  <line>: <text>` for matches, `  <line>- <text>` for context lines, and `  --` between separated hunks. Supports `glob` filtering, `case_insensitive`, `context` lines (up to 10), `files_only`, and optional `path` (default `.`). `files_only: true` reports `<path> (<count>)` for each matching file. Output is capped at 16 KiB with a visible notice; each matching line is cut at 250 characters; only the first 1 MiB of each file is searched; symlinks are not followed. The default result cap is 50 matching lines.

## `find`

Matches entry names (or relative paths when `pattern` contains `/`) against a `globset` glob (e.g. `*.rs`, `**/*.md`). Respects `.gitignore` and skips `.git`, `target`, and `node_modules` by default (`include_ignored: true` includes them). The walk is sorted and deterministic and includes dotfiles. Defaults to 50 results with a visible note when it stops.

## `ls`

One directory level: directories first (`name/`), then files with byte sizes, alphabetical, hidden entries included. Not recursive; use `find` to go deeper.
