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
| `↑` / `↓` | Navigate the `/` suggestion list while it is open; otherwise recall prompts while the composer is empty or single-line, or move between its lines |
| `Ctrl+P` / `Ctrl+N` | Recall the previous / next prompt, whatever the composer holds |
| `Ctrl+A`, `Home` | Start of line |
| `Ctrl+E`, `End` | End of line |
| `Ctrl+K` | Kill to end of line |
| `Ctrl+U` | Kill to start of line |
| `Tab` | Complete the highlighted `/` suggestion |
| `Esc` | Stop the run in flight: an in-flight stream is cut and a running tool is killed; what had arrived is kept. Idle, it does nothing |
| `Ctrl+O` | Expand or collapse tool calls |
| `Ctrl+T` | Show or hide thinking |
| `Ctrl+X` | Copy the last answer to the clipboard |
| `Ctrl+G` | Edit the draft in your external editor |
| `Ctrl+R` | Rebuild and restart (only in [`--watch`](watch-mode.md) mode) |
| `PageUp` / `PageDown` | Scroll the transcript (10 rows a page) |
| Mouse wheel | Scroll the transcript (3 rows a notch) |
| `Ctrl+C` | Cancel: stop a run in flight, clear a typed draft, or, with nothing left to cancel, quit. A second press while a run is stopping quits at once |

When you stop a run, the footer shows `stopping…` while active streams close and running tools terminate. Partial output received before stopping stays in the transcript and session file ([Stopping a run](tools.md#stopping-a-run)). Use `/quit` to exit directly without cancelling a draft or run. `SIGTERM` shuts down cleanly.

## Prompt history

Submitted prompts, `!`/`!!` commands, and slash commands (except `/quit`) are stored in `~/.gg/history.jsonl` (one JSON string per line, mode `0600`, newest 500 entries). Use `↑`/`↓` to browse history when the composer is empty or single-line, or `Ctrl+P`/`Ctrl+N` at any time. Pressing `↑` saves your current draft; pressing `↓` past the latest entry restores it. Recalled entries can be edited before sending. Without `HOME`, history is kept in memory for the current run only.

## Theme and layout

The frame is themed on the gang-gang cockatoo, and the theme sets the full terminal background: a charcoal background, warm off-white text, a scarlet crest for `gg`'s own marks (the `you │ ` prefix, the footer's `gg`, an aborted run, the scrollbar's thumb), orange-red for anything live (a tool call, the spinner, the composer's border while a run is in flight), pale-grey secondaries, wattle-gold warnings, and a raised slate band behind code, expanded tool output and the suggestion popup. On a terminal that supports 24-bit colour (`COLORTERM=truecolor` or `24bit`, or a `TERM` advertising direct colour) the palette is exact; anywhere else the same palette draws in xterm-256 colours.

- **Composer**: a rounded box titled `compose`, one to eight text rows. It grows with the draft and then scrolls inside itself so the cursor stays visible; its border turns orange-red while a run is in flight and scarlet while one is stopping. The cursor's column is counted in characters, so a wide glyph (CJK, emoji) can leave it a cell left of where the terminal draws it.
- **Footer**: one row of fields: `gg`, the directory, the model, its thinking level (`think medium` from `reasoning_effort`, or `think 4096t` when only `reasoning_budget_tokens` is set), `↑in ↓out`, `ctx N%` (gold past 60%, red past 85%), the throughput figures when the backend reports them, the `--watch` state, and the status: `○ ready`, a turning spinner and `working`, or `stopping…`.
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

`/think` shows or sets the active OpenAI-compatible model's thinking configuration: with no argument it reports the current setting; a bare word sets the effort; a bare number sets the budget in tokens (0 clears it); `effort budget` sets both; `off` or `none` sets effort to `none` and clears the budget; `default` or `clear` reverts both to provider defaults.

`/reload` re-discovers `AGENTS.md` files and skills from disk: the next system prompt picks up new files and skills, and the `/skills` list and `/skill:` rows refresh. The session's entries and identity are kept (see [Reloading context and skills](context-and-skills.md#reloading-context-and-skills)).

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
