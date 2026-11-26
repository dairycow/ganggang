# Delegation

A run can hand a bounded piece of work to a subagent: a separate headless `gg -p` with an empty context, started from the `bash` tool. The subagent does the work, writes one answer and keeps a session file of its own.

```sh
gg -p - --session-file review.jsonl < prompt.md > answer.md 2> progress.log
```

The prompt goes in through stdin (`-p -`), so it needs no quoting. The answer is `gg`'s stdout and the progress is its stderr, so the two stay apart. `read answer.md` then returns up to 2000 lines, where a `bash` result keeps 10 KiB.

Jobs that do not depend on each other run at once with `&` and `wait`:

```sh
gg -p - --session-file a.jsonl < a.md > a.out 2> a.err &
gg -p - --session-file b.jsonl < b.md > b.out 2> b.err &
wait
```

## What a nested run inherits

The `bash` tool describes the run in a few environment variables (see [Configuration and providers](configuration.md#environment-variables)). A headless boot that finds them is a nested run, and keeps to its parent:

| | Rule |
|---|---|
| Tools | A subset of the parent's allowlist. `--tools` may only name tools the parent has; a name outside it is an error |
| Turns | At most the parent's `max_turns`, when it has one |
| Hooks | The parent's, on or off. `--no-hooks` is ignored, with a warning |
| Model | The parent's current model, unless `-m` or `-P` is given |
| Depth | One level: a nested run cannot start a nested run |
| Session | A new session's header records the parent's session id as `parent_id`. A parent on `--no-session` keeps no session, so its nested runs record none |

A nested run fails with `a nested run cannot start a nested run` when it starts another. It discovers the same context files and skills as its parent.

A TUI boot ignores the variables, and so does a `!` command: both are yours to run. A script that must run `gg` on config defaults however it was started unsets them, as `scripts/eval.sh` and `scripts/improve-loop.sh` do. The variables guard against mistakes rather than forming a boundary: a command can unset them.

## The bash timeout

A nested run lives inside the `bash` call that started it, so `bash_timeout_secs` (default 30) bounds it, and so does the call's `timeout_secs`. A job that runs for minutes needs both raised: set `timeout_secs` on the call, and `bash_timeout_secs` in [configuration](configuration.md#config-file) to at least as much. Or pass `background: true` and leave the ceiling alone: the call returns at the limit, the nested run is kept as a [background job](tools.md#bash), and `wait_for` returns its output and exit code once it exits. Each `wait_for` waits at most the ceiling, 30 s by default, and reports a run still going with its output so far.

Without `background: true`, when the limit is reached the whole process group is killed. The nested run's session file keeps the work it finished. A job kept with `background: true` ends when it finishes, when `stop` kills it, or when the session ends.

The [delegate skill](#the-delegate-skill) starts a job that has `--timeout` in a session of its own, so that job outlives `stop`, a kill at the deadline and the end of the session. Those also end the job's watchdog, and the job then runs until it finishes on its own. Wait for every delegate before you answer.

## Resuming a job

`--session-file` resumes a non-empty file, so the next prompt continues that session. Run the same command with a short follow-up to get an answer from a job that was stopped, or from one that ended without writing an answer. A job that reaches `max_tokens` while reasoning writes nothing: `gg` warns on stderr and stops.

## The `delegate` skill

The `ganggang` repository ships a `delegate` skill in `.agents/skills/delegate/`:

- `SKILL.md` tells the model when and how to delegate.
- `delegate.sh` runs one job and keeps `NAME.prompt.md`, `NAME.out`, `NAME.err` and `NAME.jsonl` together. It stops a job at `--timeout` and reports an empty answer as `NO ANSWER`.

Copy the directory into `~/.agents/skills/` to use it in any repository.
