---
name: orchestrator
description: "Run delegated gg jobs in one git worktree each, review them, run the commit gate, and fast-forward main once every job passes: the orchestrator pattern. Use when jobs edit files and belong on main once reviewed, when parallel jobs must not share a working tree or touch the main checkout's uncommitted work, or when merged worktrees need removing."
---

# Orchestrator

This run orchestrates. Each job is a `delegate.sh` job that starts in a git worktree of its own, `.worktrees/<name>`, on a branch cut from `main`, so jobs that edit never share a working tree and the main checkout keeps whatever uncommitted work it has. `orchestrator.sh`, in this directory, does the git. This run writes the prompts, reads the diffs and decides what lands.

## When to use

- The task splits into jobs that edit files and belong on `main` once they pass review.
- Jobs that edit must run at once, so each needs a working tree of its own.
- Merged worktrees have piled up: `list` shows them and `drop` removes them.

Use `delegate` on its own for read-only jobs. Make a small change in this run, on a branch. A nested run cannot orchestrate: subagents are one level deep.

## How

`S` is the absolute path of this directory. Keep prompts, answers and diffs in `$O="$HOME/.gg/delegate/<task>"`, outside every checkout (`write` needs `force: true` there).

1. Plan one worktree per job. Jobs that edit the same file run one after the other: the second starts once the first has landed.
2. Create each worktree from `main`. The command refuses a name or a branch that exists and prints the path:

   ```sh
   "$S/orchestrator.sh" new <name> -b feat/<name>
   ```

3. Write each prompt to `$O/<name>.md`. The job starts empty, so give it the goal and the files, the rules under **For the job**, and the shape of the answer: commit hashes, files changed, how it verified the work, what is unresolved.
4. Start the jobs in one `bash` call with `background: true`, then collect the call with `wait_for`. `run` starts the job in the worktree. Check that `git -C <worktree> log --oneline main..HEAD` lists the commits each answer claims, and that `status --short` is empty:

   ```sh
   O="$HOME/.gg/delegate/<task>"
   "$S/orchestrator.sh" run a --prompt "$O/a.md" --dir "$O" --timeout 3600 &
   "$S/orchestrator.sh" run b --prompt "$O/b.md" --dir "$O" --timeout 3600 &
   wait
   ```

5. Review each job. Read `$O/<name>.out`, then `read` the diff from a file, since `bash` returns only the start and end of a long one:

   ```sh
   git -C <worktree> diff main...HEAD > "$O/<name>.diff"
   ```

   Check the diff against the prompt: only the files the job was given, the style in `AGENTS.md`, tests with the change, and docs updated when user-facing behaviour changed (`docs-maintenance`).
6. Gate it: `"$S/orchestrator.sh" gate <name>` in a call with `background: true`, collected with `wait_for` until it exits. It runs `scripts/pre-commit-check.sh` in the worktree and records the commit that passed.
7. Apply review notes as a follow-up prompt under the same job name, which continues the job's session, then review and gate again. A fix of a line or two can be made in the worktree and committed there.
8. Land it: `"$S/orchestrator.sh" merge <name>`. It fast-forwards `main` in the main checkout and lands only the commit `gate` recorded. When `main` has moved it refuses: run `update <name>`, which merges `main` into the job's branch, hand any conflicts to the job as a follow-up prompt, then review, gate and merge again.
9. Drop it: `"$S/orchestrator.sh" drop <name>` once the job is on `main`. Finish with `"$S/orchestrator.sh" list`, which shows what is merged and what is stale.

## For the job

Put these rules in every prompt:

- Stay in the worktree. No other checkout, no branch switch, no stash, no merge, no push, no delegation.
- Commit on the branch. The hook runs the commit gate, which outlives a plain `bash` call: commit with `background: true` and collect with `wait_for`. Never `--no-verify`.
- Leave `CARGO_TARGET_DIR` alone. A worktree builds into its own `target/`; the main checkout's holds the installed `gg`.

## Rules

- Leave the main checkout alone: no edits, stash, reset, checkout or `cargo` there. `merge` is the only command that writes in it, and nothing is pushed.
- Never rebase reviewed commits. When `main` moves, `update` merges it into the job's branch, and `main` itself moves only by fast-forward.
- `update` must be followed by `gate`: git runs `pre-merge-commit` for a merge, this repository has no such hook, and `merge` lands only the commit `gate` recorded.
- Never pass `--force` to a worktree removal or `-D` to `branch`. When git or the script refuses, stop and find out why.
- When `merge` refuses because the main checkout has changes in files the merge would touch, or staged changes, ask the developer.
- Drop only worktrees this run created or the developer named.
- A job that fails its review twice or cannot pass the gate is stopped: leave its worktree and branch, and report the diff and the failure.

## Limits

- A fresh worktree builds from nothing, so its first gate takes minutes. The job's `--timeout` has to cover its own gate run, and `gate` takes several `wait_for` calls.
- Everything under **Limits** in the `delegate` skill holds: a job with `--timeout` outlives `stop`, and every job must finish or time out before you answer.
