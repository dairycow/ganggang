---
name: delegate
description: "Hand bounded, independent pieces of work to headless gg subagents with delegate.sh, several at once where they do not depend on each other. Use when a task splits into parts that each need their own reading, or when a subproblem would crowd this run's context."
---

# Delegate

Each subagent is a separate headless `gg -p` run with an empty context, its own session file, and this run's tools, hooks, model and limits. It returns one answer. `delegate.sh`, in this directory, runs it and keeps the files.

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
| `--timeout SECS` | Stops the job after `SECS` |

## Reading the result

Each job ends with one line: the exit status, the answer's size and the session file.

- `NO ANSWER`: the job ended without writing an answer, usually because its last response was cut off. Read `NAME.err`, then run the same job again with a short follow-up prompt that asks for the write-up. The job keeps its session, so it does not repeat the work it finished.
- `timed out`: raise `--timeout`, then continue the job the same way.
- Any other exit: read `NAME.err`.

## Limits

- A job runs for minutes, so either `bash_timeout_secs` is at least the longest `--timeout` plus a margin, or the call that starts the jobs passes `background: true` and is collected with `wait_for`. Each `wait_for` waits at most the ceiling, 30 s by default, and reports a job still running.
- A job started with `--timeout` runs in a session of its own (`delegate.sh` uses `setsid`), so `stop: <id>` cannot reach it: `stop` kills the call's process group, which holds the job's watchdog but not the job, and so does a call killed at the deadline or the end of the session. Wait for every job to finish or time out before answering.
- To end one job early, kill its process group by the pid `delegate.sh` wrote: `kill -TERM -- -$(cat <dir>/<job>.pid)`.
- A subagent cannot delegate: subagents are one level deep.
- Jobs share the working tree. Give jobs that edit disjoint files.
- `delegate.sh` runs `gg` from `PATH`. Set `GG=/path/to/gg` to use another build.
