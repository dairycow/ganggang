---
name: docs-maintenance
description: "Keep reference pages, mdBook guide, and doc_sync contracts in sync with code changes. Use when changing CLI flags, tools, providers, models, slash commands, config, env vars, hooks, events, keys, or limits."
---

# Docs Maintenance

- Changing a CLI flag, tool, provider, model, or slash command? Run `./scripts/sync-docs.sh` from the repository root. It regenerates `docs/src/reference/*.md`; never edit those by hand, and `cargo test` fails while they are stale.
- Changing user-visible behaviour (keys, config keys, env vars, hooks, session format, limits)? Update the matching page in `docs/src/guide/` in the same commit. `src/doc_sync/contracts.rs` fails the build on drift.
- New page? Add it to `docs/src/SUMMARY.md`. Keep `README.md` short; the guide is the long form.

See `docs/src/maintaining-docs.md` for full details and the crates.io release checklist.
