#!/usr/bin/env bash
# delegate.sh: run one headless gg subagent and keep its files.
#
# Delegation in gg is headless. A job is `gg -p` with its prompt on stdin
# and a session file of its own. gg's bash tool hands the command the
# nesting handshake, so the job runs with this run's tools, hooks, model
# and limits, and its session records this run as its parent.
#
# Files, in DIR (one directory per bash call by default):
#   NAME.prompt.md  the prompt, as given
#   NAME.out        the answer (gg's stdout)
#   NAME.err        progress (gg's stderr)
#   NAME.jsonl      the job's session; run the same job again to continue it
#   NAME.pid        the job's pid, which is also its process group id when
#                   the job got a group of its own: kill -TERM -- -$(cat NAME.pid)
#                   ends the job and every command it started
#
# Exit status: 0 with an answer, 124 on a timeout, 1 with no answer, and
# otherwise the job's own. See scripts/test-delegate.py.
#
# The one-line summary says the status (ok, timed out, NO ANSWER or failed),
# the detail, and the answer, progress and session files. A NO ANSWER detail
# is the reason read from NAME.err: a response cut off by max_tokens, a turn
# or time budget spent, or a pointer to NAME.err.
#
# When GG_JOB_LOG names a file whose directory exists, the script appends two
# tab-separated records, one per line: `start NAME PID DIR` once the job
# starts, and `done NAME EXIT NOTE` when it ends, NOTE holding the status and
# the detail on one line. A log that cannot be written never fails the job.
#
# Runs from inside a gg session as well as from a plain shell: a top-level
# run's bash tool sets GG_NEST_DEPTH=1 on its commands, and only a value past
# that marks a nested run, which cannot delegate.
#
# A --timeout job is stopped by a watchdog that writes $dir/.watchdog_fired
# when it fires, then terms the job, then kills it five seconds later, so
# a job that ignores the term cannot outlive the timeout. The job runs in
# its own process group (started with setsid) only when this script is not
# a group leader itself, so the signals reach the job and the commands it
# starts; a script backgrounded from an interactive shell is a leader and
# would share its group with the job, so there the watchdog targets the
# job's pid alone.
set -euo pipefail

GG="${GG:-gg}"

usage() {
  cat <<'EOF'
usage: delegate.sh --job NAME --prompt FILE [--dir DIR] [--model SPEC]
                   [--tools LIST] [--max-turns N] [--timeout SECS]

Runs one headless gg job. GG selects the binary (default: gg on PATH).
GG_DELEGATE_DIR sets the root of the default directory.
EOF
}

die() {
  echo "delegate: $*" >&2
  exit 2
}

# One tab-separated record for GG_JOB_LOG, when it is set and its directory
# exists. A bad path or a failed write is ignored: the log never fails the
# job. NAME has no tab in it, and the caller keeps the other fields flat.
job_log() {
  [[ -n "${GG_JOB_LOG:-}" && -d "$(dirname "$GG_JOB_LOG")" ]] || return 0
  printf '%s\t%s\t%s\t%s\n' "$1" "$job" "$2" "$3" >>"$GG_JOB_LOG" 2>/dev/null || true
}

# The summary line: status, detail, and the three files to read next.
summarise() {
  echo "job $job: $1 | $2 | answer: $job.out | progress: $job.err | session: $job.jsonl"
}

# The last record, then the script's exit status.
finish() {
  job_log done "$1" "$2"
  exit "$1"
}

# Why the job ended without an answer, read from its progress file: gg says
# when a final response was cut off by max_tokens, and names the budget that
# spent the run. Only gg's own warning lines count: the file also echoes the
# job's command lines, and one of those may name a budget. Anything else is
# in NAME.err.
no_answer_reason() {
  if grep -qE '^warning: .*before producing visible output' "$err" 2>/dev/null; then
    echo "cut off by max_tokens before any visible output"
  elif grep -qE '^warning: reached max_(turns|seconds) \(' "$err" 2>/dev/null; then
    echo "reached its turn or time budget"
  else
    echo "read $job.err"
  fi
}

job='' prompt='' dir='' model='' tools='' max_turns='' timeout=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --job) job="${2:-}"; shift 2 ;;
    --prompt) prompt="${2:-}"; shift 2 ;;
    --dir) dir="${2:-}"; shift 2 ;;
    --model) model="${2:-}"; shift 2 ;;
    --tools) tools="${2:-}"; shift 2 ;;
    --max-turns) max_turns="${2:-}"; shift 2 ;;
    --timeout) timeout="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

[[ -n "$job" ]] || die "--job is required"
[[ "$job" =~ ^[A-Za-z0-9._-]+$ ]] || die "--job may use letters, digits, dot, dash and underscore (got: $job)"
[[ "$job" != "." && "$job" != ".." ]] || die "--job cannot be . or .."
[[ -n "$prompt" ]] || die "--prompt is required"
[[ -f "$prompt" ]] || die "prompt file not found: $prompt"
[[ -z "$timeout" || "$timeout" =~ ^[0-9]+$ ]] || die "--timeout must be a whole number of seconds (got: $timeout)"
# The bash tool marks every command with the depth a gg started there would
# have (ParentEnv::exports in src/nested.rs): 1 in a top-level run, the only
# kind that may delegate, and 2 in a nested run. So 1 is no refusal; that
# starts one past it. Judging 1 a refusal refused every session, top-level
# ones too.
if [[ "${GG_NEST_DEPTH:-}" =~ ^[0-9]+$ && "$GG_NEST_DEPTH" -gt 1 ]]; then
  die "this run is already a nested run, and a nested run cannot delegate"
fi
command -v "$GG" >/dev/null 2>&1 || die "$GG not found on PATH (set GG=/path/to/gg)"

if [[ -z "$dir" ]]; then
  [[ -n "${HOME:-}" ]] || die "HOME is not set and --dir was not given"
  # Jobs started from one bash call share a directory: the shell that
  # started them is their common parent.
  dir="${GG_DELEGATE_DIR:-$HOME/.gg/delegate}/${PWD//\//-}/$PPID"
fi
mkdir -p "$dir"
dir=$(cd "$dir" && pwd)

prompt=$(cd "$(dirname "$prompt")" && pwd)/$(basename "$prompt")
out="$dir/$job.out"
err="$dir/$job.err"
session="$dir/$job.jsonl"
if [[ "$prompt" != "$dir/$job.prompt.md" ]]; then
  cp "$prompt" "$dir/$job.prompt.md"
fi

args=(-p - --session-file "$session")
if [[ -n "$model" ]]; then args+=(-m "$model"); fi
if [[ -n "$tools" ]]; then args+=(--tools "$tools"); fi
if [[ -n "$max_turns" ]]; then args+=(--max-turns "$max_turns"); fi
# A job with a timeout gets a time budget ten seconds short of it: gg asks
# the model to wrap up before the watchdog fires, so the answer lands in
# NAME.out instead of being cut off by its SIGTERM.
if [[ -n "$timeout" && "$timeout" -ge 20 ]]; then args+=(--max-seconds "$((timeout - 10))"); fi

# A job in its own process group gives the timeout a group to kill: the
# job and the commands it starts die together. That is only safe when this
# script is not a group leader itself: a script backgrounded from an
# interactive shell is, and then the job would share this script's group
# and a group kill would take the script down with it. Without setsid (or
# as a leader) the watchdog targets the job's pid alone. Not a leader,
# setsid execs directly (no fork), so $! is the new group's pid.
own_group=0
if [[ -n "$timeout" ]] && ! kill -0 -$$ 2>/dev/null && command -v setsid >/dev/null 2>&1; then
  own_group=1
fi
if (( own_group )); then
  setsid "$GG" "${args[@]}" < "$prompt" > "$out" 2> "$err" &
else
  "$GG" "${args[@]}" < "$prompt" > "$out" 2> "$err" &
fi
pid=$!
echo "$pid" > "$dir/$job.pid"
job_log start "$pid" "$dir"
watchdog=''
if [[ -n "$timeout" ]]; then
  # The watchdog is self-contained: the script's main path stays parked
  # in `wait` on the job, so the escalation must fire from here. It
  # writes the marker before the term: a job that dies from the term is
  # stopped by the deadline, whatever its exit status. Then it kills the
  # same target five seconds later, so a job that ignores the term still
  # dies at the deadline; both are ignored when nothing is left.
  if (( own_group )); then
    (
      sleep "$timeout"
      echo 1 > "$dir/.watchdog_fired"
      kill -TERM -"$pid" 2>/dev/null || true
      sleep 5
      kill -KILL -"$pid" 2>/dev/null || true
    ) &
  else
    (
      sleep "$timeout"
      echo 1 > "$dir/.watchdog_fired"
      kill -TERM "$pid" 2>/dev/null || true
      sleep 5
      kill -KILL "$pid" 2>/dev/null || true
    ) &
  fi
  watchdog=$!
fi

rc=0
wait "$pid" || rc=$?
timed_out=0
if [[ -n "$watchdog" ]]; then
  if [[ -f "$dir/.watchdog_fired" ]]; then
    # The marker is written before the watchdog's term: the deadline
    # stopped the job, whatever its exit status.
    timed_out=1
  elif kill -0 "$watchdog" 2>/dev/null; then
    # The job beat the deadline: stop the watchdog before it can fire,
    # then check the marker once more for a firing that landed just now.
    kill "$watchdog" 2>/dev/null
    wait "$watchdog" 2>/dev/null || true
    if [[ -f "$dir/.watchdog_fired" ]]; then
      timed_out=1
    fi
  fi
  rm -f "$dir/.watchdog_fired"
fi

bytes=$(wc -c < "$out" | tr -d '[:space:]')
echo "delegate dir: $dir"
if (( timed_out )); then
  summarise "timed out" "after ${timeout}s"
  finish 124 "timed out: after ${timeout}s"
elif (( rc != 0 )); then
  summarise "failed" "exit $rc"
  finish "$rc" "failed: exit $rc"
elif (( bytes == 0 )); then
  # gg stopped without an answer, usually a final response cut off by
  # max_tokens. The session holds the work; a follow-up prompt gets the
  # write-up without repeating it.
  reason=$(no_answer_reason)
  summarise "NO ANSWER" "$reason"
  finish 1 "NO ANSWER: $reason"
fi
summarise "ok" "$bytes bytes"
finish 0 "ok"
