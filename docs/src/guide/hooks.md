# Hooks

Hooks are one-shot shell commands that run at three points in the agent loop. A hook is not a plugin: it is a process that receives one JSON object on stdin and, for `pre_tool`, may print a decision. There is no plugin runtime, no argument rewriting, and no approval prompt.

| Event | When | Can it block? |
|---|---|---|
| `pre_tool` | Before a tool call | Yes: it may deny |
| `post_tool` | After the tool result is recorded | No, observe only |
| `run_end` | Once, after a clean finish (not an abort) | No, observe only |

## Configuring

Hooks live in `~/.gg/hooks.toml` and **only** there:

```toml
# ~/.gg/hooks.toml
[[hooks]]
event = "pre_tool"              # pre_tool | post_tool | run_end
tools = ["bash"]                # omit or ["*"] for all; exact tool names
command = "~/.gg/hooks/guard-bash.sh"
timeout_secs = 5                # default 10

[[hooks]]
event = "post_tool"
tools = ["edit", "write"]
command = "./scripts/format.sh"

[[hooks]]
event = "run_end"
command = "notify-send gg 'run finished'"
```

- `command` runs as `bash -c` in the session's working directory.
- `tools` names tools exactly (see [Tools](tools.md)); omit it, or use `["*"]`, to match all. It is rejected for `run_end`, and an empty list is an error.
- Hooks for one event run in file order. The first `pre_tool` denial wins.
- A project `.gg/hooks.toml` is **ignored** with a startup warning: a cloned repository cannot add executable behaviour.
- `/hooks` in the TUI lists what loaded. `gg --no-hooks` skips loading entirely, even a broken hooks file.

## What a hook receives

One JSON object on stdin:

```json
{"version":1,"event":"pre_tool","cwd":"/work/proj","session_id":"…",
 "tool":{"id":"call_1","name":"bash","args":{"command":"rm -rf build"}}}
```

- `pre_tool`: `version`, `event`, `cwd`, `session_id` (null with `--no-session`), `tool` (`id`, `name`, `args`).
- `post_tool`: the same, plus `result` (`output`, `is_error`).
- `run_end`: `version`, `event`, `cwd`, `session_id`, and `turns`.

## Denying a call

A `pre_tool` hook allows by printing nothing (or `{"decision":"allow"}`) and exiting 0. To deny, print:

```json
{"decision":"deny","reason":"blocked by the user guard hook"}
```

A denial is a normal tool error: the model reads the reason (capped at 2 KiB; a missing reason names the hook command) and reacts to it. The tool does not run and `post_tool` hooks are skipped. Calls whose arguments were not valid JSON never reach hooks. Neither pattern runs `post_tool`: a call that never executed has nothing to observe, which includes a call the user stopped while it ran (`Esc`, `Ctrl+C`).

## Failure handling

Hooks **fail open**. A non-zero exit, a timeout, output that is not valid decision JSON, or a spawn error becomes a warning and the run continues; a broken guard must not wedge the agent.

- The deadline is `timeout_secs`; on timeout the whole process group is killed.
- stdout is capped at 64 KiB, and stderr keeps its last 8 KiB, which is quoted in warnings.
- Aborting a run (`Esc` or `Ctrl+C`) kills a running hook's process group.

## Recipes

Block destructive shell commands (`~/.gg/hooks/guard-bash.sh`):

```sh
#!/usr/bin/env bash
cmd=$(jq -r '.tool.args.command')
if grep -Eq 'rm -rf|git push --force' <<<"$cmd"; then
  echo '{"decision":"deny","reason":"destructive command; ask the user first"}'
fi
```

Format after edits (`post_tool` on `edit` and `write`), or notify when a long run finishes (`run_end`), as in the configuration above.

> Hooks run with your permissions and are unsandboxed. A `pre_tool` deny is workflow policy, not a security boundary. See the [Security Model](security.md).
