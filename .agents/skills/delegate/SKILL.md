---
name: delegate
description: "Hand bounded, independent pieces of work to headless gg subagents with delegate.sh, several at once where they do not depend on each other. A read-only job runs in place; a job that edits gets a git worktree of its own, then is reviewed, gated and fast-forwarded to main. Use when a task splits into parts that each need their own reading, when a subproblem would crowd this run's context, when parallel jobs must not share a working tree or touch the main checkout's uncommitted work, or when merged worktrees need removing."
---

# Delegate

A read-only job runs in place. A job that edits gets a git worktree of its own. Either way the subagent is a separate headless `gg -p` run with an empty context, its own session file, and this run's tools, hooks, model and limits. It returns one answer.

- A read-only job: `delegate.sh`, in this directory, runs it and keeps its files. **How** has the steps. **Options**, **Reading the result** and **Limits** apply to every job.
- A job that edits: `worktrees.sh`, also in this directory, runs the same job in a worktree under `.worktrees/<name>`, on a branch cut from `main`, so jobs that edit never share a working tree and the main checkout keeps whatever uncommitted work it has. **Jobs that edit** has the flow.

## When to delegate

- The work splits into parts that do not depend on each other: review four files, search three subsystems, draft independent modules.
- A subproblem needs a lot of reading that this run would otherwise carry in its own context.

Do a quick read, or work where each step depends on the last, in this run.

## How

1. Write each prompt to a file. The subagent starts empty, so name the goal, the files, the constraints and the shape of the answer you want.
2. Run one job per line in a single `bash` call, backgrounded, then `wait`. Set `timeout_secs` on the call above every job's `--timeout`:

   ```sh
   D=.agents/skills/delegate
   $D/delegate.sh --job review-a --prompt review-a.md --timeout 600 &
   $D/delegate.sh --job review-b --prompt review-b.md --timeout 600 &
   wait
   ```

3. Read each answer from the directory the jobs print: `read <dir>/review-a.out`.

Jobs in one `bash` call share a directory. Each job writes `NAME.prompt.md`, `NAME.out` (the answer), `NAME.err` (progress) and `NAME.jsonl` (its session).

## Options

| Option | Effect |
|---|---|
| `--job NAME` | Required. Letters, digits, dot, dash and underscore |
| `--prompt FILE` | Required. Read as the prompt |
| `--dir DIR` | Where the files go. Default: one directory per `bash` call under `~/.gg/delegate/` |
| `--model SPEC` | A `provider/model`. Default: this run's model |
| `--tools LIST` | Comma-separated, a subset of this run's tools. Default: this run's tools |
| `--max-turns N` | The job's turn budget, at most this run's |
| `--timeout SECS` | Stops the job after `SECS`. From 20 s up, the job also gets a time budget 10 s short of it, so it writes its answer before the stop |

## Reading the result

Each job ends with one line: the exit status, the answer's size and the session file.

- `NO ANSWER`: the job ended without writing an answer, usually because its last response was cut off. Read `NAME.err`, then run the same job again with a short follow-up prompt that asks for the write-up. The job keeps its session, so it does not repeat the work it finished.
- `timed out`: raise `--timeout`, then continue the job the same way.
- Any other exit: read `NAME.err`.

## Limits

- A job runs for minutes: a wait is no longer capped at the ceiling. `wait_job` collects a job by name, waiting 600 s by default and up to 3600 s, and `wait_for` takes a `timeout_secs` past the ceiling; either reports a job still running, and a later call keeps waiting.
- A job started with `--timeout` runs in a session of its own (`delegate.sh` uses `setsid`), so `stop: <id>` cannot reach it: `stop` kills the call's process group, which holds the job's watchdog but not the job, and so does a call killed at the deadline or the end of the session. Wait for every job to finish or time out before answering.
- To end one job early, kill its process group by the pid `delegate.sh` wrote: `kill -TERM -- -$(cat <dir>/<job>.pid)`.
- A subagent cannot delegate: subagents are one level deep.
- Jobs share the working tree. Give jobs that edit disjoint files, or give each one a worktree (**Jobs that edit**).
- A fresh worktree builds from nothing, so its first gate takes minutes. The job's `--timeout` has to cover its own gate run, and `gate` takes several `wait_for` calls.
- `delegate.sh` runs `gg` from `PATH`. Set `GG=/path/to/gg` to use another build.

## Jobs that edit

Use this flow when:

- The task splits into jobs that edit files and belong on `main` once they pass review.
- Jobs that edit must run at once, so each needs a working tree of its own.
- Merged worktrees have piled up: `list` shows them and `drop` removes them.

Make a small change in this run, on a branch. This run writes the prompts, reads the diffs and decides what lands. `S` is the absolute path of this directory. Keep prompts, answers and diffs in `$O="$HOME/.gg/delegate/<task>"`, outside every checkout (`write` needs `force: true` there).

1. Plan one worktree per job. Jobs that edit the same file run one after the other: the second starts once the first has landed.
2. Create each worktree from `main`: `"$S/worktrees.sh" new <name> -b feat/<name>`. The command refuses a name or a branch that exists and prints the path.
3. Write each prompt to `$O/<name>.md`. The job starts empty, so give it the goal and the files, the rules under **Rules for the job**, and the shape of the answer: commit hashes, files changed, how it verified the work, what is unresolved.
4. Start the jobs in one `bash` call with `background: true`, then collect the call with `wait_for`; a later call with `wait_job: <name>` waits for one delegate job by name and can outlast the ceiling. `run` starts the job in the worktree with the options above. Check that `git -C <worktree> log --oneline main..HEAD` lists the commits each answer claims, and that `status --short` is empty:

   ```sh
   O="$HOME/.gg/delegate/<task>"
   "$S/worktrees.sh" run a --prompt "$O/a.md" --dir "$O" --timeout 3600 &
   "$S/worktrees.sh" run b --prompt "$O/b.md" --dir "$O" --timeout 3600 &
   wait
   ```

5. Review each job. Read `$O/<name>.out`, then `read` the diff from a file, since `bash` returns only the start and end of a long one: `git -C <worktree> diff main...HEAD > "$O/<name>.diff"`.

   Check the diff against the prompt: only the files the job was given, the style in `AGENTS.md`, tests with the change, and docs updated when user-facing behaviour changed (`docs-maintenance`).
6. Gate it: `"$S/worktrees.sh" gate <name>` in a call with `background: true`, collected with `wait_for` until it exits. It runs `scripts/pre-commit-check.sh` in the worktree and records the commit that passed.
7. Apply review notes as a follow-up prompt under the same job name, which continues the job's session, then review and gate again. A fix of a line or two can be made in the worktree and committed there.
8. Land it: `"$S/worktrees.sh" merge <name>`. It fast-forwards `main` in the main checkout and lands only the commit `gate` recorded. When `main` has moved it refuses: run `update <name>`, which merges `main` into the job's branch, hand any conflicts to the job as a follow-up prompt, then review, gate and merge again.
9. Drop it: `"$S/worktrees.sh" drop <name>` once the job is on `main`. Finish with `"$S/worktrees.sh" list`, which shows what is merged and what is stale.

### Rules for the job

- Stay in the worktree. No other checkout, no branch switch, no stash, no merge, no push, no delegation.
- Commit on the branch. The hook runs the commit gate, which outlives a plain `bash` call: commit with `background: true` and collect with `wait_for`. Never `--no-verify`.
- Leave `CARGO_TARGET_DIR` alone. A worktree builds into its own `target/`; the main checkout's holds the installed `gg`.

### Rules for the run

- Leave the main checkout alone: no edits, stash, reset, checkout or `cargo` there. `merge` is the only command that writes in it, and nothing is pushed.
- Never rebase reviewed commits. When `main` moves, `update` merges it into the job's branch, and `main` itself moves only by fast-forward.
- `update` must be followed by `gate`: git runs `pre-merge-commit` for a merge, this repository has no such hook, and `merge` lands only the commit `gate` recorded.
- Never pass `--force` to a worktree removal or `-D` to `branch`. When git or the script refuses, stop and find out why.
- When `merge` refuses because the main checkout has changes in files the merge would touch, or staged changes, ask the developer.
- Drop only worktrees this run created or the developer named.
- A job that fails its review twice or cannot pass the gate is stopped: leave its worktree and branch, and report the diff and the failure.
