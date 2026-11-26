# ganggang: contributor & agent guide

A small agentic harness in Rust (`gg`).

## Before every commit

Gate: `.githooks/pre-commit` (runs `scripts/pre-commit-check.sh`: format, clippy, tests). Enable per clone: `git config core.hooksPath .githooks`; never `--no-verify`.

Pitfall: the `bash` tool clamps `timeout_secs` to `bash_timeout_secs` (default 30 s), so `cargo test`, `cargo publish`, `git rebase --exec` and `scripts/improve-loop.sh` get killed at 30 s; pass `background: true` on the call and collect it with `wait_for`, or raise `bash_timeout_secs` in `~/.gg/config.toml` for build/release work.

Style: standard OSS Rust, `//!`/`///` docs, no `unwrap` outside tests, modules ≲300 lines.

Prose, branding, and documentation-sync rules live in `.agents/skills/`.
