# ganggang — contributor & agent guide

A small agentic harness in Rust (`gg`). The spec is `design.html`; `TASKS.md` tracks the build.

## Before every commit

```sh
cargo fmt --all
cargo test
cargo clippy --all-targets -- -D warnings
```

Style: standard OSS Rust, `//!`/`///` docs, no `unwrap` outside tests, modules ≲300 lines.

## Documentation is part of the change

The user guide lives in `docs/` (mdBook) and is published to https://dairycow.github.io/ganggang/.

- Changing a CLI flag, tool, provider, model, or slash command? Run `./scripts/sync-docs.sh` — it regenerates `docs/src/reference/*.md` (never edit those by hand). `cargo test` fails while they are stale.
- Changing user-visible behaviour (keys, config keys, env vars, hooks, session format, limits)? Update the matching page in `docs/src/guide/` in the same commit. `src/doc_sync/contracts.rs` fails the build when env vars, hook events, JSON events, or key actions go undocumented, when documented TOML stops validating, or when links break.
- New page? Add it to `docs/src/SUMMARY.md`.
- Update the README only for the short overview; the guide is the long form.

See `docs/src/maintaining-docs.md` for the full picture, including the crates.io release checklist.
