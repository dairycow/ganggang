# Interactive TUI

Running `gg` on a terminal opens a fullscreen interface in three regions: the **transcript** (markdown rendering, tool calls, optional thinking), the **composer** where you type, and a one-row **footer** with the active model, token totals, context %, and live throughput (`t/s`, `think %`) when the backend reports it. `SIGTERM` shuts down cleanly and restores the terminal.

If stdin is not a terminal and you gave no prompt, `gg` exits with an error rather than hanging; use `gg -p "…"` for scripts.

## Keys

| Key | Action |
|---|---|
| `Enter` | Send the message |
| `Alt+Enter`, `Ctrl+J` | Newline (`Shift+Enter` too, where the terminal reports modified keys) |
| `Backspace` | Delete before the cursor |
| `←` / `→` | Move the cursor |
| `↑` / `↓` | Drive the `/` suggestion list while it is open; otherwise recall prompts while the composer is empty or single-line, or move between its lines |
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
| `Ctrl+C` | Cancel: stop a run in flight, clear a typed draft, or — with nothing left to cancel — quit. A second press while a run is stopping quits at once |

A run asked to stop shows `stopping…` in the footer while it winds down, and it winds down promptly: a generation still streaming is cut rather than left to finish, and what it had streamed stays in the transcript and in the session ([Stopping a run](tools.md#stopping-a-run)). `/quit` is the way out that cancels nothing. `SIGTERM` ends the TUI at once, with the same clean shutdown.

## Prompt history

Everything you submit — a prompt, a `!`/`!!` shell line, a slash command other than `/quit` — is remembered in `~/.gg/history.jsonl` (one JSON string per line, mode `0600`), newest 500 entries. `↑`/`↓` walk it while the composer is empty or holds a single line; `Ctrl+P`/`Ctrl+N` walk it whatever it holds. The first `↑` saves what you were typing, and `↓` past the newest gives it back, so a recall never costs a draft. Recalled text is ordinary composer text: edit it and send it again. Without `HOME` the history lives in memory for the run only.

## Theme and layout

The frame is themed on the gang-gang cockatoo, and the theme owns the whole screen: a charcoal background, warm off-white text, a scarlet crest for `gg`'s own marks (the `you │ ` prefix, the footer's `gg`, an aborted run, the scrollbar's thumb), orange-red for anything live (a tool call, the spinner, the composer's border while a run is in flight), pale-grey secondaries, wattle-gold warnings, and a raised slate band behind code, expanded tool output and the suggestion popup. On a terminal that claims 24-bit colour (`COLORTERM=truecolor` or `24bit`, or a `TERM` advertising direct colour) the palette is exact; anywhere else the same palette draws in xterm-256 colours.

- **Composer**: a rounded box titled `compose`, one to eight text rows. It grows with the draft and then scrolls inside itself so the cursor stays visible; its border turns orange-red while a run is in flight and scarlet while one is stopping. The cursor's column is counted in characters, so a wide glyph (CJK, emoji) can leave it a cell left of where the terminal draws it.
- **Footer**: one row of fields — `gg`, the directory, the model, `↑in ↓out`, `ctx N%` (gold past 60%, red past 85%), the throughput figures when the backend reports them, the `--watch` state, and the status: `○ ready`, a turning spinner and `working`, or `stopping…`.
- **Transcript**: a scrollbar appears only when the content overflows, in a column reserved for it so no text hides under it. An empty session shows a short welcome block instead of a blank area.
- **Mouse**: `gg` turns on mouse reporting while it runs so the wheel can scroll, reads the wheel and nothing else (a click or a drag does nothing), and turns it off when it exits, when it panics, and while `Ctrl+G` lends the terminal to your editor. To select text with the terminal's own selection, hold `Shift` while dragging.

### tmux

`Alt+Enter` and `Ctrl+J` work as they are. `Shift+Enter` needs tmux's extended keys and a terminal that reports modified keys: add `set -g extended-keys on` (and, if you changed it, `set -g extended-keys-format csi-u`, the default) to `~/.tmux.conf`, reload, and restart `gg`. GNOME Terminal/VTE does not report `Shift+Enter` at all; use `Alt+Enter` there.

## Slash commands

Typing `/` opens a suggestion list above the composer: the built-ins, your [prompt templates](context-skills-prompts.md#prompt-templates), and, once `/skill:` is typed, one row per skill. `↑`/`↓` move the highlight, `Tab` completes, and `Enter` still sends exactly what you typed.

The command list is in [TUI Slash Commands](../reference/commands.md). Unknown commands are rejected with the valid set.

## Shell lines: `!` and `!!`

- `!cmd` runs a shell command in the session directory and its output **enters the model's context**.
- `!!cmd` runs it and shows the output to you only.

Both share the bash tool's runner: 30-second timeout (the whole process group is killed), output capped at 10 KiB with a visible note.

## `@file` mentions

`@src/main.rs` in a message attaches the file's content below your text. Your line stays readable; attachments follow as fenced blocks.

- A token counts as a mention when what follows `@` contains `/` or `.`, so `user@host` and a lone `@` stay literal. Trailing punctuation (`@file.txt,`) is not part of the path.
- Paths resolve against the session directory; two spellings of the same file attach once.
- Total attachment budget is 50 KiB per message. Over-budget files are truncated or skipped, binary files report their size, and missing files say so, always visibly in the attachment.

## External editor

`Ctrl+G` opens your draft in `$VISUAL`, else `$EDITOR`, else `nano`. Arguments are fine (`code --wait`). When the editor exits cleanly, what you saved replaces the composer; nothing is sent until you press `Enter`, and a run in flight keeps going behind the editor.

If the editor exits with an error, the composer is left alone and an edited draft is kept where the warning says. An editor that returns at once (a GUI editor without a wait flag) leaves a note suggesting one, such as `code --wait`.

## Clipboard

`Ctrl+X` copies the last answer using the OSC 52 terminal escape, which works over SSH. Terminals without clipboard support ignore it silently. Payloads are capped at about 70 KB.
