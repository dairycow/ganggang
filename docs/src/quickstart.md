# Quick start

## Start the TUI

```sh
cd your-project
gg
```

On a terminal, `gg` opens the fullscreen UI. Type a message and press `Enter`. The model reads, searches, edits and runs commands in the current directory; every tool call shows in the transcript (`Ctrl+O` expands them).

## Attach files and run shell lines

```text
explain the retry logic in @src/client.rs
!cargo test
```

`@path` attaches a file's content to your message. `!cmd` runs a shell line and puts its output into the model's context; `!!cmd` runs it for you only. Details in [Interactive TUI](guide/tui.md).

## One-shot print mode

```sh
gg -p "what version is in Cargo.toml?"
git diff | gg -p -     # '-' reads the prompt from stdin
```

Progress goes to stderr, the final answer to stdout, so it pipes cleanly.

## Resume a session

```sh
gg -c                  # continue the newest session for this directory
gg --resume 3fa9       # a specific session, by id or prefix
```

In the TUI, `/new` and `/resume` do the same. See [Sessions and compaction](guide/sessions-compaction.md).

## Switch models

```sh
gg -P deepseek
gg -m opencode-go/claude-haiku-5-5 -p "summarise this repo"
```

In the TUI, `/model` lists models and `/model <n|provider|provider/model>` switches. Next: [Configuration and providers](guide/configuration.md).
