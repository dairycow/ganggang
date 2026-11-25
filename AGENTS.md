# ganggang: contributor & agent guide

A small agentic harness in Rust (`gg`). `TASKS.md` tracks the build.

## Before every commit

Commits are gated by `.githooks/pre-commit`, which runs `scripts/pre-commit-check.sh` (format check, clippy, tests). Enable it once per clone with `git config core.hooksPath .githooks`, run the gate before committing, and never pass `--no-verify`.

Style: standard OSS Rust, `//!`/`///` docs, no `unwrap` outside tests, modules ≲300 lines.

Prose, branding, and documentation-sync rules live in `.agents/skills/`.
