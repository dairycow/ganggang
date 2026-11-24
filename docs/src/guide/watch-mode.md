# Watch mode

`gg --watch` is for developing ganggang itself. It watches the sources, rebuilds, and re-executes the fresh binary **on the same session**.

```sh
cd ganggang
cargo build        # or --release; watch follows the binary you launched
./target/debug/gg --watch
```

## What it does

- Watches `*.rs` files under `src/` plus `Cargo.toml` and `Cargo.lock`. Editor swap files and symlinks are ignored.
- Runs `cargo build` once an edit has settled (after a short quiet period). The profile follows the binary you launched (`target/debug` or `target/release`), and the footer shows `watch debug · watching`.
- On success, re-executes the new binary with the same session. Every restart re-reads config, prompt templates, skills and `AGENTS.md`.
- A build that finishes while a run is in flight **waits for idle**. With unsent text in the composer the restart holds at `rebuild ready · Ctrl+R`.
- `Ctrl+R` forces a rebuild and restart when idle. Any unsent draft in the composer is saved beside the session and restored after restart.
- A failed build stays in the transcript (the tail of cargo's output) and keeps the old binary running until the next edit or `Ctrl+R`.

## Limits

- `--watch` is a TUI mode: it cannot combine with `-p`, `--mode json`, `--export` or `--list-models`.
- It refuses `--no-session`, because a restart needs a session file to resume.
- It refuses any workspace that does not build the ganggang `gg` binary, so it executes only the binary built from the current workspace.
- Watch sessions create their file at startup. Header-only watch sessions sort below real conversations for `-c` and `/resume`, but can still be opened by id.
