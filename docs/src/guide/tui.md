# Interactive TUI

Running `gg` on a terminal opens a fullscreen interface in three regions: the **transcript** (markdown rendering, tool calls, optional thinking), the **composer** where you type, and a one-row **footer** with the active model and its thinking level, token totals, context %, and live throughput (`t/s`, `think %`) when the backend reports it. `SIGTERM` shuts down cleanly and restores the terminal.

If stdin is not a terminal and no prompt is given, `gg` exits with an error; use `gg -p "…"` in scripts.

## Keys

| Key | Action |
|---|---|
| `Enter` | Send the message |
| `Alt+Enter`, `Ctrl+J` | Newline (`Shift+Enter` too, where the terminal reports modified keys) |
| `Backspace` | Delete before the cursor |
| `←` / `→` | Move the cursor |
| `Ctrl+←` / `Ctrl+→` | Move the cursor by word |
| `↑` / `↓` | Navigate the `/` suggestion list or the `/resume` picker while one is open; otherwise recall prompts while the composer is empty or single-line, or move between its lines |
| `Ctrl+P` / `Ctrl+N` | Recall the previous / next prompt, whatever the composer holds, unless the suggestion list or the `/resume` picker is open |
| `Ctrl+A`, `Home` | Start of line |
| `Ctrl+E`, `End` | End of line |
| `Ctrl+K` | Kill to end of line |
| `Ctrl+U` | Kill to start of line |
| `Tab` | Complete the highlighted `/` suggestion; in the `/resume` picker, switch between every directory's sessions and the current directory's |
| `Esc` | Stop the run in flight: an in-flight stream is cut and a running tool is killed; what had arrived is kept. Idle, it closes the `/resume` picker when one is open and otherwise does nothing |
| `Ctrl+O` | Expand or collapse tool calls |
| `Ctrl+T` | Show or hide thinking |
| `Ctrl+X` | Copy the last answer to the clipboard |
| `Ctrl+G` | Edit the draft in your external editor |
| `Ctrl+R` | Rebuild and restart (only in [`--watch`](watch-mode.md) mode) |
| `PageUp` / `PageDown` | Scroll the transcript (10 rows a page) |
| Mouse wheel | Scroll the transcript (3 rows a notch); move the highlight in the `/resume` picker |
| `Ctrl+C` | Cancel: stop a run in flight, close the `/resume` picker, clear a typed draft, or, with nothing left to cancel, quit. A second press while a run is stopping quits at once |

When you stop a run, the footer shows `stopping…` while active streams close and running tools terminate. Partial output received before stopping stays in the transcript and session file ([Stopping a run](tools.md#stopping-a-run)). Use `/quit` to exit directly without cancelling a draft or run. `SIGTERM` shuts down cleanly.

## Prompt history

Submitted prompts, `!`/`!!` commands, and slash commands (except `/quit`) are stored in `~/.gg/history.jsonl` (one JSON string per line, mode `0600`, newest 500 entries). Use `↑`/`↓` to browse history when the composer is empty or single-line, or `Ctrl+P`/`Ctrl+N` at any time. Pressing `↑` saves your current draft; pressing `↓` past the latest entry restores it. Recalled entries can be edited before sending. Without `HOME`, history is kept in memory for the current run only.

## Theme and layout

The frame is themed on the gang-gang cockatoo, and the theme sets the full terminal background: a charcoal background, warm off-white text, a scarlet crest for `gg`'s own marks (the `you │ ` prefix, the footer's `gg`, an aborted run, the scrollbar's thumb), orange-red for anything live (a tool call, the spinner, the composer's border while a run is in flight), pale-grey secondaries, wattle-gold warnings, and a raised slate band behind code, expanded tool output and the suggestion popup. On a terminal that supports 24-bit colour (`COLORTERM=truecolor` or `24bit`, or a `TERM` advertising direct colour) the palette is exact; anywhere else the same palette draws in xterm-256 colours.

- **Composer**: a rounded box titled `compose`, one to eight text rows. It grows with the draft and then scrolls inside itself so the cursor stays visible; its border turns orange-red while a run is in flight and scarlet while one is stopping. The cursor's column is counted in characters, so a wide glyph (CJK, emoji) can leave it a cell left of where the terminal draws it.
- **Footer**: one row of fields: `gg`, the directory, the model, its thinking level (`think medium` from `reasoning_effort`, or `think 4096t` when only `reasoning_budget_tokens` is set), `↑in ↓out`, `ctx N%` (gold past 60%, red past 85%), `cache NN%` — the session's cache hit rate, cache-read tokens over prompt tokens, shown when the provider reports cache reads — the throughput figures when the backend reports them, the `--watch` state, and the status: `○ ready`, a turning spinner and `working`, or `stopping…`.
- **Transcript**: tool calls collapse to one line: `▸ name` and the useful argument (a command, a path, a search pattern), cut with `…` past 100 characters; `Ctrl+O` expands a call's output. A scrollbar appears only when the content overflows, in a dedicated column. Empty sessions display a welcome block.
- **Mouse**: `gg` turns on mouse reporting while it runs so the wheel can scroll, handles scroll-wheel events and ignores clicks and drags, and turns it off when it exits, when it panics, and while `Ctrl+G` opens your editor. To select text with the terminal's own selection, hold `Shift` while dragging.

### tmux

`Alt+Enter` and `Ctrl+J` work as they are. `Shift+Enter` needs tmux's extended keys and a terminal that reports modified keys: add `set -g extended-keys on` (and, if you changed it, `set -g extended-keys-format csi-u`, the default) to `~/.tmux.conf`, reload, and restart `gg`. GNOME Terminal/VTE does not report `Shift+Enter` at all; use `Alt+Enter` there.

## Markdown

Assistant answers render a deliberately small markdown subset; your own messages and tool output stay literal.

- Fenced code blocks render on a raised band with no inline formatting inside. An unclosed fence renders as code until the reply closes it.
- `**bold**` spans render bold; unpaired `**` markers stay literal.
- `- ` and `* ` bullets become `• ` with their indent kept; ordered lists keep their numbers.
- Pipe tables render as a grid with each column padded to its widest cell and aligned by the delimiter's colons (`:--` left, `--:` right, `:--:` centre); data cells strip `**` without bolding. A table too wide for the transcript falls back to a bulleted list of `header: value` lines.
- Everything else, including headings, links and italics, renders as typed.

## Slash commands

Typing `/` opens a suggestion list above the composer: the built-ins and, once `/skill:` is typed, one row per skill. `↑`/`↓` move the highlight, `Tab` completes, and `Enter` still sends exactly what you typed.

Typing `/model` lists the models in the same popup: the built-in row first, then one row per model with its context window, any discovery status, whether a key is missing, and the `· active` mark on the model in use. Typing after the space filters the list case-insensitively, and `Tab` completes the highlighted `provider/model` into the command. Live local models (Strata, `llama-server`, Ollama, vLLM) refresh in the background at startup and on `/reload`: their `/v1/models` endpoints are probed off the render loop with short timeouts, so the popup picks up models loaded after the session began without waiting on a down server.

`/resume` opens a session picker in the same popup: one row per saved session, from every directory gg has run in, newest first. Each row shows the session's age (`just now`, `5m ago`, `3h ago`, `2d ago`, then the date), the 8-hex id from its file name, its title or first message, and the directory it started in, with `~` for your home directory. The session you are in is not listed. What you type filters the rows: every word has to appear in a row's id, title, first message, model or directory, so a repository's name or a worktree's narrows the list to it. `↑`/`↓` and the mouse wheel move the highlight, the panel scrolls to follow it, and `Enter` opens the highlighted session. With nothing to show, because a filter matches no row or `Tab` has narrowed the picker to a directory that has no sessions, the panel stays up and says so. If the highlighted session cannot be opened, the picker stays and its panel shows the error. `Esc` and `Ctrl+C` close the picker without opening anything.

`Tab` narrows the picker to the current directory's sessions, where each row shows the session's model instead of its directory, and widens it back.

A session continues in the directory it started in, wherever you launched gg. Opening it moves gg there: its history is replayed, and its tools, hooks, `AGENTS.md` files and skills, and the `@mentions` and `!shell` lines you type, are that directory's. The footer shows the directory, and `/new` then starts a session there. If the directory no longer exists, as with a removed worktree, the session continues in the current directory and the transcript says so. In [`--watch`](watch-mode.md) mode the watched directory stays the one gg was launched in, and a restart continues the session there: one opened from another directory is continued by its file, in the launch directory.

`/resume <n>` opens the session at position `n` in the listing the picker last showed, counting from 1. `/resume <prefix>` opens the session whose id starts with the prefix, among the current directory's sessions and, when none of them matches, among every directory's (see [Sessions](sessions-compaction.md#sessions)).

`/think` shows or sets the active OpenAI-compatible model's thinking configuration: with no argument it reports the current setting; a bare word sets the effort; a bare number sets the budget in tokens (0 clears it); `effort budget` sets both; `off` or `none` sets effort to `none` and clears the budget; `default` or `clear` reverts both to provider defaults.

`/reload` re-discovers `AGENTS.md` files and skills from disk: the next system prompt picks up new files and skills, the `/skill:` rows refresh, and the local servers' `/v1/models` probes run again in the background. The session's entries and identity are kept (see [Reloading context and skills](context-and-skills.md#reloading-context-and-skills)).

The command list is in [TUI slash commands](../reference/commands.md). Unknown commands are rejected with the valid set.

## Shell lines: `!` and `!!`

- `!cmd` runs a shell command in the session directory and its output **enters the model's context**.
- `!!cmd` runs it and shows the output to you only.

Both share the bash tool's runner: 30-second timeout (the whole process group is killed), output capped at 10 KiB with a visible note.

## `@file` mentions

`@src/main.rs` in a message or `/skill:<name>` arguments attaches the file's content below your text. Attached files are appended below your message as fenced code blocks.

- A token counts as a mention when what follows `@` contains `/` or `.`, so `user@host` and a lone `@` stay literal. Trailing punctuation (`@file.txt,`) is not part of the path.
- Paths resolve against the session directory; two spellings of the same file attach once.
- Total attachment budget is 50 KiB per message. Over-budget files are truncated or skipped, binary files report their size, and include a missing-file notice in the attachment.

## External editor

`Ctrl+G` opens your draft in `$VISUAL`, else `$EDITOR`, else `nano`. Arguments are supported (for example, `code --wait`). When the editor exits cleanly, what you saved replaces the composer; nothing is sent until you press `Enter`, and a run in flight keeps going behind the editor.

If the editor exits non-zero, the composer is unchanged and the edited draft path is reported in a warning. If a GUI editor exits immediately without saving, `gg` shows a reminder to pass a wait flag (such as `code --wait`).

## Clipboard

`Ctrl+X` copies the last answer using the OSC 52 terminal escape, which works over SSH. Terminals without clipboard support ignore it silently. Payloads are capped at about 70 KB.
