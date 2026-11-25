# Improving the harness

This page is for contributors who work on `gg` itself. The improvement tooling runs on demand: `gg` never starts it after a session, and neither script runs unless you invoke it.

## Session digest

`scripts/session-digest.py` reads the JSONL sessions that `gg` writes under `~/.gg/sessions/<cwd-slug>/` and reports friction:

- aborts and tool errors;
- repeated reads and bash workarounds that duplicate a native tool;
- user corrections and compactions;
- prompt and completion token totals.

```sh
python3 scripts/session-digest.py
python3 scripts/session-digest.py --last 10
python3 scripts/session-digest.py --session 1a2b3c4d --json
```

The digest redacts known token shapes (API keys, Bearer tokens, private-key blocks and JWTs), but redaction is best-effort. Free-form text still reaches whichever model reads the digest, so prefer a local endpoint for sensitive sessions.

## The improvement loop

`scripts/improve-loop.sh` is an eval-gated hill climb over the agent's behaviour. It needs a clean working tree, a branch other than `main` or `master` unless `--allow-main`, a built binary, and a running Strata server for the eval suites. Each iteration:

1. captures a baseline with `scripts/eval.sh`, retaining sessions;
2. digests the baseline sessions for friction;
3. asks a second `gg` for one focused hypothesis change, bounded by `--max-turns`;
4. rejects a changed `HEAD`, protected paths, removed test or assertion lines, and a test count below the baseline;
5. gates with `cargo fmt --all`, `cargo test`, `cargo clippy --all-targets -- -D warnings` and the selected build;
6. evaluates the candidate and compares medians against the baseline;
7. keeps a win as a commit and promotes the candidate records to the new baseline, or restores the starting branch and commit.

The loop never reads the diff for intent. The verdict is the gates plus the metrics.

### Commands

```sh
scripts/improve-loop.sh --dry-run --repetitions 1
scripts/improve-loop.sh --iterations 3 --heldout-check --max-turns 30
scripts/improve-loop.sh --compare base.jsonl candidate.jsonl
```

`--repetitions` defaults to three. Each suite runs that many times on each side and the comparison uses medians, so a single lucky run does not decide the outcome. The work directory must be outside the repository. `--eval-suite all` already includes the heldout tasks, so it cannot be combined with `--heldout-check`.

The editor may not touch `scripts/`, `Cargo.toml`, `Cargo.lock`, `fixtures/`, `benches/`, or a dedicated test path. A normal run evaluates a pristine copy of `scripts/eval.sh` from the starting commit, so editing the harness cannot change the measurement.

The heldout tasks live in `scripts/eval.sh`. The editor is told not to inspect the eval definitions, but the file is in the repository. Treat the heldout pass as an overfitting smoke test, not a blind benchmark.

## Skills

Two repository skills carry the procedure:

- `self-improve` mines recent sessions and routes durable findings into `AGENTS.md`, a new skill, or a hook suggestion.
- `harness-improve` is the manual version of the eval-driven loop.

Both are `disable-model-invocation: true`. Run `/skill:self-improve` or `/skill:harness-improve` explicitly. After writing a new skill, run `/reload` so the next system prompt and the `/skill:` rows pick it up without losing the conversation.

## See also

- [Context and skills](guide/context-and-skills.md)
- [Keeping docs up to date](maintaining-docs.md)
