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
#
# Exit status: 0 with an answer, 124 on a timeout, 1 with no answer, and
# otherwise the job's own. See scripts/test-delegate.py.
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
if [[ "${GG_NEST_DEPTH:-}" =~ ^[0-9]+$ && "$GG_NEST_DEPTH" -ge 1 ]]; then
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

"$GG" "${args[@]}" < "$prompt" > "$out" 2> "$err" &
pid=$!
watchdog=''
if [[ -n "$timeout" ]]; then
  (sleep "$timeout"; kill -TERM "$pid" 2>/dev/null) &
  watchdog=$!
fi

rc=0
wait "$pid" || rc=$?
timed_out=0
if [[ -n "$watchdog" ]]; then
  if kill -0 "$watchdog" 2>/dev/null; then
    kill "$watchdog" 2>/dev/null
    wait "$watchdog" 2>/dev/null || true
  else
    timed_out=1
  fi
fi

bytes=$(wc -c < "$out" | tr -d '[:space:]')
echo "delegate dir: $dir"
if (( timed_out )); then
  echo "job $job: timed out after ${timeout}s | progress: $job.err | session: $job.jsonl"
  exit 124
elif (( rc != 0 )); then
  echo "job $job: exit $rc | progress: $job.err | session: $job.jsonl"
  exit "$rc"
elif (( bytes == 0 )); then
  # gg stopped without an answer, usually a final response cut off by
  # max_tokens. The session holds the work; a follow-up prompt gets the
  # write-up without repeating it.
  echo "job $job: NO ANSWER (exit 0, empty $job.out) | progress: $job.err | session: $job.jsonl"
  exit 1
fi
echo "job $job: exit 0 | answer: $job.out ($bytes bytes) | session: $job.jsonl"
