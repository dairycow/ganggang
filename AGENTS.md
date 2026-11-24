# ganggang: contributor & agent guide

A small agentic harness in Rust (`gg`). `TASKS.md` tracks the build.

## Before every commit

```sh
cargo fmt --all
cargo test
cargo clippy --all-targets -- -D warnings
```

Style: standard OSS Rust, `//!`/`///` docs, no `unwrap` outside tests, modules ≲300 lines.

Prose, branding, and documentation-sync rules live in `.agents/skills/`.
