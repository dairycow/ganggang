# Keeping Docs Up to Date

ganggang moves fast, so documentation is enforced by the build rather than by memory. There are four layers.

## 1. Generated reference pages

Everything under [Reference](reference/cli.md) is rendered from the code: the clap argument tree, the built-in provider table, the tool registry (names, descriptions, JSON schemas) and the slash-command catalogue. Nobody edits these by hand.

```sh
./scripts/sync-docs.sh                 # regenerate, verify, build the book if mdbook is installed
UPDATE_DOCS=1 cargo test doc_sync      # just regenerate
```

A plain `cargo test` **fails** when a generated page is stale, with a message naming the page and the command above. A change to a flag, tool, provider or command cannot merge without its reference page.

## 2. Contract tests for the guide

`src/doc_sync/contracts.rs` ties the hand-written pages to the code:

- every `GG_*` variable in the source and every provider key variable appears in [Configuration](guide/configuration.md);
- every hook event, every JSON-stream event type and every TUI key action must be named in its page. The tests use exhaustive `match`es, so adding an enum variant is a compile error until it is covered;
- every ` ```toml ` example tagged `# ~/.gg/config.toml` or `# ~/.gg/hooks.toml` is parsed and validated by the real loaders;
- every page is listed in `SUMMARY.md` and every relative link resolves;
- `Cargo.toml` and the README point at the published guide.

When you add a behaviour that has no machine check, write the prose in `docs/src/guide/` in the same change.

## 3. CI and publishing

`.github/workflows/ci.yml` runs `cargo fmt --check`, `cargo test` (including the doc checks), `cargo clippy -D warnings`, and `mdbook build`. On pull requests it also leaves a reminder when `src/` changed without any change under `docs/`. `.github/workflows/docs.yml` deploys the book to GitHub Pages whenever `docs/` changes on `main`.

Because the reference pages are only committed after the tests agree with the code, the published site always matches `main`.

## 4. Releasing to crates.io

The guide tracks `main`; crates.io tracks releases. Before `cargo publish`:

1. Bump `version` in `Cargo.toml`.
2. Run `./scripts/sync-docs.sh` and `cargo test`.
3. Skim the README and this guide for anything newer than the release notes.
4. `cargo publish --dry-run`, then `cargo publish`, then tag the release.

`Cargo.toml` already carries `documentation = "https://dairycow.github.io/ganggang/"`, so each published version links to this guide from its crates.io page. `docs/`, `.github/` and `AGENTS.md` are excluded from the published package.

Contributors and coding agents get the same instructions from `AGENTS.md` at the repository root. User-facing prose follows `.agents/skills/style-guide/SKILL.md` (Australian English, no em dashes, no agentic filler), and visual surfaces follow `.agents/skills/gg-branding/SKILL.md`.
