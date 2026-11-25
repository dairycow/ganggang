---
name: harness-improve
description: "Run an eval-driven self-improvement iteration on gg itself using scripts/eval.sh, session digests and cargo gates."
disable-model-invocation: true
---

# Harness-Improve

Run one eval-driven improvement iteration on gg: baseline, one change, gates, verify, keep only wins.

This is an on-demand procedure. gg never runs it after a session and never runs `scripts/improve-loop.sh` or `scripts/session-digest.py` on its own. Invoke it explicitly, or run the script for an unattended iteration.

## Steps

1. Confirm the working tree is clean and you are on a dedicated branch or worktree. Never auto-mutate `main`.
2. Capture the baseline with `EVAL_SUITE=core EVAL_SESSIONS_DIR=<dir>/sessions EVAL_OUT=<dir>/baseline.jsonl scripts/eval.sh`. For unattended iterations, use `scripts/improve-loop.sh` instead; it runs each suite three times by default and compares medians.
3. Analyse the retained traces with `python3 scripts/session-digest.py --sessions-dir <dir>/sessions`.
4. Apply one falsifiable change: a preamble edit in `src/agent/context.rs`, a tool hint in `src/agent/tools/`, or a loop or compaction heuristic. Do not weaken tests or remove test or assertion lines, and do not edit `scripts/`, `Cargo.toml`, `Cargo.lock`, fixtures or dedicated test paths. Do not read the eval definitions if you are the editor; the heldout check is meant to be unseen.
5. Gate with `cargo fmt --all && cargo test && cargo clippy --all-targets -- -D warnings`, rebuild, then re-run the core suite plus `EVAL_SUITE=heldout`. Use `scripts/improve-loop.sh --compare <baseline.jsonl> <candidate.jsonl> [<baseline-heldout.jsonl> <candidate-heldout.jsonl>] --heldout-check` to decide. Keep the change only if the comparison returns `1`; revert otherwise.
6. If user-facing behaviour changed, follow the `docs-maintenance` skill.

The heldout tasks live in `scripts/eval.sh`, which a repository-capable editor can read. Treat the heldout pass as an overfitting smoke test, not a blind benchmark.
