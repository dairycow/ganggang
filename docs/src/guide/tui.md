# Interactive TUI

Running `gg` on a terminal opens a fullscreen UI with three regions: the **transcript** (markdown rendering, tool calls, optional thinking), the **composer** where you type, and a **footer** showing the active model and how full the context window is.

If stdin is not a terminal and you gave no prompt, `gg` exits with an error rather than hanging; use `gg -p "…"` for scripts.

## Keys

| Key | Action |
|---|---|
| `Enter` | Send the message |
| `Alt+Enter`, `Ctrl+J` | Newline (`Shift+Enter` too, where the terminal reports modified keys) |
| `Backspace` | Delete before the cursor |
| `←` / `→` | Move the cursor |
| `↑` / `↓` | Move between lines (and drive the `/` suggestion list while it is open) |
| `Ctrl+A`, `Home` | Start of line |
| `Ctrl+E`, `End` | End of line |
| `Ctrl+K` | Kill to end of line |
| `Ctrl+U` | Kill to start of line |
| `Tab` | Complete the highlighted `/` suggestion |
| `Esc` | Abort the run in flight (it stops between tool calls and finishes cleanly) |
| `Ctrl+O` | Expand or collapse tool calls |
| `Ctrl+T` | Show or hide thinking |
| `Ctrl+X` | Copy the last answer to the clipboard |
| `Ctrl+G` | Edit the draft in your external editor |
| `Ctrl+R` | Rebuild and restart (only in [`--watch`](watch-mode.md) mode) |
| `PageUp` / `PageDown` | Scroll the transcript |
| `Ctrl+C` | Quit |

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
