# Interactive TUI

Running `gg` on a terminal opens a fullscreen interface in three regions: the **transcript** (markdown rendering, tool calls, optional thinking), the **composer** where you type, and a one-row **footer** with the active model, its thinking level, token totals, context %, and live throughput (`t/s`, `think %`) when the backend reports it. `SIGTERM` shuts down cleanly and restores the terminal.

If stdin is not a terminal and no prompt is given, `gg` exits with an error; use `gg -p "…"` in scripts.

## Keys

| Key | Action |
|---|---|
| `Enter` | Send the message; in an open list, act on the highlighted row: open the session or run its `copy path` or `info` row, switch to the model, or run the skill |
| `Alt+Enter`, `Ctrl+J` | Newline (`Shift+Enter` too, where the terminal reports modified keys) |
| `Backspace` | Delete before the cursor |
| `←` / `→` | Move the cursor |
| `Ctrl+←` / `Ctrl+→` | Move the cursor by word |
| `↑` / `↓` | Navigate the `/` suggestion list or the `/session`, `/model` or `/skill` list while one is open; otherwise recall prompts while the composer is empty or single-line, or move between its lines |
| `Ctrl+P` / `Ctrl+N` | Recall the previous / next prompt, whatever the composer holds, unless the suggestion list or the `/session`, `/model` or `/skill` list is open |
| `Ctrl+A`, `Home` | Start of line |
| `Ctrl+E`, `End` | End of line |
| `Ctrl+K` | Kill to end of line |
| `Ctrl+U` | Kill to start of line |
| `Tab` | Complete the highlighted `/` suggestion; in the `/session` list, switch between every directory's sessions and the current directory's; in the `/model` and `/skill` lists, complete the highlighted row's command |
| `Esc` | Stop the run in flight: an in-flight stream is cut and a running tool is killed, except that a kept [background job](tools.md#bash) keeps running; what had arrived is kept. Idle, it clears the line and closes the open list when one is, and otherwise does nothing |
| `Ctrl+O` | Expand or collapse tool calls |
| `Ctrl+T` | Show or hide thinking |
| `Ctrl+X` | Copy the last answer to the clipboard |
| `Ctrl+G` | Edit the draft in your external editor |
| `Ctrl+R` | Rebuild and restart (only in [`--watch`](watch-mode.md) mode) |
| `PageUp` / `PageDown` | Scroll the transcript (10 rows a page) |
| Mouse wheel | Scroll the transcript (3 rows a notch), or move an open list's highlight |
| `Ctrl+C` | Cancel: stop a run in flight, close an open list and clear the line, clear a typed draft, or, with nothing left to cancel, quit. A second press while a run is stopping quits at once |

When you stop a run, the footer shows `stopping…` while active streams close and running tools terminate. Partial output received before stopping stays in the transcript and session file ([Stopping a run](tools.md#stopping-a-run)). Use `/quit` to exit directly without cancelling a draft or run. `SIGTERM` shuts down cleanly.

## Prompt history

Submitted prompts, `!`/`!!` commands, and slash commands (except `/quit`) are stored in `~/.gg/history.jsonl` (one JSON string per line, mode `0600`, newest 500 entries). Use `↑`/`↓` to browse history when the composer is empty or single-line, or `Ctrl+P`/`Ctrl+N` at any time. Pressing `↑` saves your current draft; pressing `↓` past the latest entry restores it. Recalled entries can be edited before sending. Without `HOME`, history is kept in memory for the current run only.

## Theme and layout

The frame is themed on the gang-gang cockatoo, and the theme sets the full terminal background: a charcoal background, warm off-white text, a scarlet crest for `gg`'s own marks (the `> ` mark of your messages, the footer's `gg`, an aborted run, the scrollbar's thumb, and the crest frame of a turning spinner), orange-red for anything live (a tool call, the wings of a working turn — scarlet on the crest frame only — the composer's border while a run is in flight), pale-grey secondaries, wattle-gold warnings, and a raised slate band behind code, expanded tool output and the suggestion popup. On a terminal that supports 24-bit colour (`COLORTERM=truecolor` or `24bit`, or a `TERM` advertising direct colour) the palette is exact; anywhere else the same palette draws in xterm-256 colours.

- **Composer**: a rounded box titled `compose`, one to eight text rows. It grows with the draft and then scrolls inside itself so the cursor stays visible; its border turns orange-red while a run is in flight and scarlet while one is stopping. The cursor's column is counted in characters, so a wide glyph (CJK, emoji) can leave it a cell left of where the terminal draws it.
- **Footer**: one row of fields: the crest at the left of the row — `gg` at rest, or the wings of a turn while a run works — then the directory, the model, its thinking level as the bare word (`medium` from `reasoning_effort`, `4096t` when only `reasoning_budget_tokens` is set, `think off` for effort `none`), `↑in ↓out`, `ctx N%` (gold past 60%, red past 85%), `cache NN%` (the last turn's cache hit rate: cache-read tokens over prompt tokens, shown when the provider reports cache reads at the top level, under `prompt_tokens_details.cached_tokens`, or in Anthropic's `cache_read_input_tokens`) — the throughput figures when the backend reports them, the `--watch` state, and the status: `○ ready`, a turning spinner and `working`, or `stopping…`.
- **Transcript**: tool calls collapse to one line: `▸ name`, the useful argument (a command, a path, a search pattern) and any facts after a middle dot (`▸ edit src/main.rs · 2 edits`, `▸ bash cargo test · timeout 60s`). The argument is cut with `…` past 100 characters and the facts follow the cut. Your own messages open with `> ` in scarlet; the lines after the first sit two columns in, so a multi-line message reads as one block. `Ctrl+O` expands a call's output. A scrollbar appears only when the content overflows, in a dedicated column with a black edge. Empty sessions display a welcome block.
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

Typing `/` opens a suggestion list above the composer: the built-ins. `↑`/`↓` move the highlight, `Tab` completes, and `Enter` still sends exactly what you typed, except in the `/session`, `/model` and `/skill` lists, where it acts on the highlighted row.

Those three commands open a list in the panel above the composer as soon as the command is typed, and what follows the command filters it. In every list, `↑`/`↓` and the mouse wheel move the highlight, the panel scrolls to follow it, `Enter` acts on the highlighted row, and `Esc` or `Ctrl+C` clear the line and close it. A failed action keeps the list and shows the error in its last row.

Typing `/model` opens the model list in the same panel, with one row per model: `provider/model`, its context window, any discovery status, `no key` when its key is missing, and the `· active` mark on the model in use. With nothing typed after the command the active model is highlighted, so `Enter` on a bare `/model` changes nothing (`already on …`). Typing after the command filters the list case-insensitively: every word has to appear in a row's `provider/model` or its description, so `loaded`, `no key` and `active` narrow the list, and `Enter` switches to the highlighted model; a switch that fails, say over a missing key or an unknown model, keeps the list and shows the error in its last row. `Tab` completes the highlighted `provider/model` into the command. Live local models (Strata, `llama-server`, Ollama, vLLM) refresh in the background at startup and on `/reload`: their `/v1/models` endpoints are probed off the render loop with short timeouts, so the list picks up models loaded after the session began without waiting on a down server.

`/session` opens a session list in the same panel as soon as the command is typed: one row per saved session, from every directory gg has run in, newest first. Each row shows the session's age (`just now`, `5m ago`, `3h ago`, `2d ago`, then the date), the 8-hex id from its file name, its title or first message, and the directory it started in, with `~` for your home directory. The session you are in is not listed. What you type after the command filters the rows: every word has to appear in a row's id, title, first message, model or directory, so a repository's name or a worktree's narrows the list to it. Once a filter is typed, the two action rows `copy path` and `info` sit above the matching sessions: `copy path` copies the current session's `.jsonl` path to the clipboard by the same copy as `Ctrl+X`, so `/session copy` then `Enter` copies the path, and `info` prints the path, model, entry count and token totals. Both action rows act while a run is in flight; opening a session, and the `/model` and `/skill` picks, wait for the run to finish and say that it is busy. `↑`/`↓` move the highlight, the panel scrolls to follow it, and `Enter` opens the highlighted session or runs the highlighted action row. With nothing to show, because there are no sessions yet or a filter matches no row, the panel stays up and says so. If a session cannot be opened, or a copy fails, the list stays and its panel shows the error. `Esc` and `Ctrl+C` clear the line and close the list without opening anything. The listing is read in the background when gg starts, each time the list opens, and after a session switch. A run started with `--no-session` has no session file: `copy path` there says so instead, and `info` says the session is not persisted. A fresh session's file is written on its first message; until then, `info` says the path is not written yet.

`Tab` narrows the list to the current directory's sessions, where each row shows the session's model instead of its directory, and widens it back.

A session continues in the directory it started in, wherever you launched gg. Opening it moves gg there: its history is replayed, and its tools, hooks, `AGENTS.md` files and skills, and the `@mentions` and `!shell` lines you type, are that directory's. The footer shows the directory, and `/new` then starts a session there. If the directory no longer exists, as with a removed worktree, the session continues in the current directory and the transcript says so. In [`--watch`](watch-mode.md) mode the watched directory stays the one gg was launched in, and a restart continues the session there: one opened from another directory is continued by its file, in the launch directory.

A session's 8-hex id works as a filter: `/session 3fa9` leaves that session highlighted, and `Enter` opens it. Typed `copy` and `info` are filters too: `/session copy` highlights the `copy path` row, and `Enter` runs it.

`/skill` opens the skill list in the same panel as soon as the command is typed: one row per discovered skill, its name and its description, with ` · manual only` on a skill the model may not invoke itself. The first word typed after the command filters the list (it has to appear in a skill's name or description), and anything after that word is passed to the skill as arguments. `Enter` runs the highlighted skill with those arguments; `Tab` completes its name so arguments can be typed. `/skill <name> [args]` typed in full forces one: its instructions become your next message and run immediately. Skills whose name contains a space are not listed and cannot be run this way.

`/think` shows or sets the active OpenAI-compatible model's thinking configuration: with no argument it reports the current setting; a bare word sets the effort; a bare number sets the budget in tokens (0 clears it); `effort budget` sets both; `off` or `none` sets effort to `none` and clears the budget; `default` or `clear` reverts both to provider defaults.

`/reload` re-discovers `AGENTS.md` files and skills from disk: the next system prompt picks up new files and skills, the `/skill` rows refresh, and the local servers' `/v1/models` probes run again in the background. The session's entries and identity are kept (see [Reloading context and skills](context-and-skills.md#reloading-context-and-skills)).

The command list is in [TUI slash commands](../reference/commands.md). Unknown commands are rejected with the valid set.

## Shell lines: `!` and `!!`

- `!cmd` runs a shell command in the session directory and its output **enters the model's context**.
- `!!cmd` runs it and shows the output to you only.

Both share the bash tool's runner: 30-second timeout (the whole process group is killed), output capped at 10 KiB with a visible note.

## `@file` mentions

`@src/main.rs` in a message or `/skill <name>` arguments attaches the file's content below your text. Attached files are appended below your message as fenced code blocks.

- A token counts as a mention when what follows `@` contains `/` or `.`, so `user@host` and a lone `@` stay literal. Trailing punctuation (`@file.txt,`) is not part of the path.
- Paths resolve against the session directory; two spellings of the same file attach once.
- Total attachment budget is 50 KiB per message. Over-budget files are truncated or skipped, binary files report their size, and include a missing-file notice in the attachment.

## External editor

`Ctrl+G` opens your draft in `$VISUAL`, else `$EDITOR`, else `nano`. Arguments are supported (for example, `code --wait`). When the editor exits cleanly, what you saved replaces the composer; nothing is sent until you press `Enter`, and a run in flight keeps going behind the editor.

If the editor exits non-zero, the composer is unchanged and the edited draft path is reported in a warning. If a GUI editor exits immediately without saving, `gg` shows a reminder to pass a wait flag (such as `code --wait`).

## Clipboard

`Ctrl+X` copies the last answer, and the `/session` list's `copy path` row copies the session's file path. On a desktop with a clipboard tool installed (`wl-copy` on Wayland, `xclip` or `xsel` on X11, `pbcopy` on macOS), `gg` sends the text through it and the copy lands on the system clipboard. Everywhere else it writes the OSC 52 terminal escape to `/dev/tty`, which works over SSH; a terminal that does not support OSC 52 ignores it silently, so there the copy may not reach the clipboard. The clipboard tools take the text whole; an OSC 52 payload is capped at about 70 KB.
