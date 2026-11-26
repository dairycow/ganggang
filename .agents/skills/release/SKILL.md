---
name: release
description: "Cut a ganggang release: version bump, docs sync, cargo publish, tag, GitHub release. Use when asked to release, tag, or publish a new ganggang version."
---

# Release

Run from the repository root. The `bash` tool caps every call at `bash_timeout_secs` (default 30 s, see `AGENTS.md`), so anything that takes longer runs with `background: true` on the call (then `wait_for`), in the shell background as in step 4, or with the cap raised first.

1. Bump `version` in `Cargo.toml`; commit as `Bump version to X` (matches the existing tag history).
2. `./scripts/sync-docs.sh`, then `cargo test` (the doc contracts fail on stale reference pages).
3. Confirm `X` is not already published: `curl -sS -A "ganggang-release" https://crates.io/api/v1/crates/ganggang/versions`.
4. `cargo publish --dry-run`, then `cargo publish` in the background: `nohup cargo publish > /tmp/gg-publish.log 2>&1 &`, poll the log. After it lands, run `cargo build` so `target/debug/gg` comes from the live `src/` again (see the pitfall below).
5. `git tag vX` on the bump commit, `git push origin vX`.
6. `gh release create vX --title "ganggang X" --notes "## Highlights …"` (list the user-visible changes since the previous tag).
7. Verify the version appears on crates.io (same curl as step 3).

Pitfalls:

- The crates.io API returns 403 to the default `curl` User-Agent; always pass `-A "ganggang-release"` (or any non-default UA).
- `cargo publish` runs in a background process group, so a gg run abort does not kill it; always confirm the outcome from the log before claiming the release is done.
- `gh release list` / `gh release view` are fine inside the 30 s cap; `cargo publish` and `cargo test` are not.
- `cargo publish`'s verification build writes `target/debug/gg` from the packed copy (`target/package/ganggang-<v>/src`) and points the cargo fingerprint at those packed files. Every later `cargo build` then reports "Finished" no matter how new the live `src/` is, so `gg --watch` and `cargo run` keep running the stale binary. The `cargo build` in step 4 restores the live-source fingerprint.

Full checklist: `docs/src/maintaining-docs.md` §4.
