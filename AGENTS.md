# ganggang: contributor & agent guide

A small agentic harness in Rust (`gg`).

## Before every commit

Gate: `.githooks/pre-commit` (runs `scripts/pre-commit-check.sh`: format, clippy, tests). Enable per clone: `git config core.hooksPath .githooks`; never `--no-verify`.

Pitfall: the `bash` tool refuses a `timeout_secs` over `bash_timeout_secs` (default 300 s) and kills the command at that ceiling, so `cargo publish`, `git rebase --exec` and `scripts/improve-loop.sh` can run for many minutes and outlive one call; pass `background: true` on the call and collect it with `wait_for`, or raise `bash_timeout_secs` in `~/.gg/config.toml` for build/release work.

Style: standard OSS Rust, `//!`/`///` docs, no `unwrap` outside tests, modules ≲300 lines. `scripts/module-sizes.py` prints each module's size in lines before its tests, with the counts over 300, 400 and 500.

Prose, branding, and documentation-sync rules live in `.agents/skills/`.
