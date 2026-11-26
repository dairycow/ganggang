#!/usr/bin/env bash
# Hypothesis improve loop: each iteration asks the gg editor for one focused
# agent change, gates it (fmt, test, clippy, build), evaluates it against the
# captured baseline, and keeps it only if it wins the gated comparison.
# Reverts are hard resets to the iteration's starting commit and branch; every
# outcome lands in ledger.jsonl under the work dir.
#
# The loop runs on demand. gg never invokes it after a session, and neither
# does gg run scripts/session-digest.py. Use this script or the
# harness-improve skill explicitly.
#
# Integrity guards: the editor may not touch scripts/, Cargo.toml, Cargo.lock,
# fixtures/, benches/, or a dedicated test path; it may not remove test or
# assertion lines from a Rust file; and a normal run compares with a pristine
# copy of scripts/eval.sh from the starting commit, so the measurement cannot
# change under the candidate. A test-count floor catches deleted tests. Reverts
# restore the starting branch and commit, not whatever HEAD the editor left.
#
# Each eval suite runs --repetitions times (default 3) and the comparison uses
# medians, because a sampling model is not deterministic. Heldout tasks are
# defined in scripts/eval.sh; treat that check as an overfitting smoke test,
# not a blind benchmark, because the editor has repository read access.
#
# Usage:
#   scripts/improve-loop.sh --dry-run --repetitions 1
#   scripts/improve-loop.sh --iterations 3 --heldout-check --max-turns 30
#   scripts/improve-loop.sh --compare BASE.jsonl CAND.jsonl
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." >/dev/null 2>&1 && pwd)
GG_BIN="${GG_BIN:-$ROOT/target/debug/gg}"
if [[ "$GG_BIN" != /* ]]; then
  GG_BIN="$PWD/$GG_BIN"
fi

ITERATIONS=1
EVAL_SUITE="core"
HELDOUT_CHECK=0
MAX_TURNS=25
REPETITIONS=3
TARGET_SCOPE=""
WORK_DIR="${TMPDIR:-/tmp}/gg-improve-$(date -u +%Y%m%d-%H%M%S)"
ALLOW_MAIN=0
DRY_RUN=0
COMPARE_BASE=""
COMPARE_CAND=""
COMPARE_BASE_HELDOUT=""
COMPARE_CAND_HELDOUT=""
BUILD_PROFILE=""
START_HEAD=""
START_BRANCH=""
BASELINE_TEST_COUNT=0

usage() {
  cat <<'EOF'
Usage: scripts/improve-loop.sh [OPTIONS]

Runs hypothesis iterations on demand: each iteration asks the gg editor for
one focused agent change, gates it (fmt, test, clippy, build), evaluates it
against the baseline, and keeps it only if it wins.

Options:
  --compare BASE CAND [BASE_HELDOUT CAND_HELDOUT]
                      Compare two eval.jsonl runs and print the verdict line
  --iterations N        Number of iterations (default: 1)
  --eval-suite SUITE    Eval suite for the main run: core or all (default: core;
                        all includes the heldout tasks)
  --heldout-check       Also require the heldout suite not to regress. Rejected
                        with --eval-suite all, which already runs it
  --repetitions N       Eval runs per suite per side (default: 3); the
                        comparison uses medians
  --max-turns N         Max gg turns per iteration (default: 25)
  --target-scope TEXT   Focus hint for the hypothesis prompt
  --work-dir DIR        Work directory outside the repository (default:
                        the system temp dir under gg-improve-YYYYMMDD-HHMMSS)
  --allow-main          Allow running on main or master
  --dry-run             Run baseline + digest, print the prompt, no editor gg or git mutations
  -h, --help            Print this help and exit
EOF
}

require_value() {
  if [[ $# -lt 2 || -z "$2" ]]; then
    echo "FAIL: $1 requires a value" >&2
    usage >&2
    exit 1
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --compare)
      require_value "$@"
      shift
      COMPARE_BASE="${1:-}"
      shift
      if [[ $# -gt 0 && "$1" != -* ]]; then COMPARE_CAND="$1"; shift; fi
      if [[ $# -gt 0 && "$1" != -* ]]; then COMPARE_BASE_HELDOUT="$1"; shift; fi
      if [[ $# -gt 0 && "$1" != -* ]]; then COMPARE_CAND_HELDOUT="$1"; shift; fi
      ;;
    --iterations) require_value "$@"; ITERATIONS="$2"; shift 2 ;;
    --eval-suite) require_value "$@"; EVAL_SUITE="$2"; shift 2 ;;
    --heldout-check) HELDOUT_CHECK=1; shift ;;
    --repetitions) require_value "$@"; REPETITIONS="$2"; shift 2 ;;
    --max-turns) require_value "$@"; MAX_TURNS="$2"; shift 2 ;;
    --target-scope) require_value "$@"; TARGET_SCOPE="$2"; shift 2 ;;
    --work-dir) require_value "$@"; WORK_DIR="$2"; shift 2 ;;
    --allow-main) ALLOW_MAIN=1; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "FAIL: unknown flag: $1" >&2; usage >&2; exit 1 ;;
  esac
done

if [[ "$EVAL_SUITE" != "core" && "$EVAL_SUITE" != "all" ]]; then
  echo "FAIL: --eval-suite must be core or all (got: $EVAL_SUITE)" >&2
  exit 1
fi
if [[ ! "$ITERATIONS" =~ ^[0-9]+$ || "$ITERATIONS" -lt 1 ]]; then
  echo "FAIL: --iterations must be a positive integer" >&2
  exit 1
fi
if [[ ! "$MAX_TURNS" =~ ^[0-9]+$ || "$MAX_TURNS" -lt 1 ]]; then
  echo "FAIL: --max-turns must be a positive integer" >&2
  exit 1
fi
if [[ ! "$REPETITIONS" =~ ^[0-9]+$ || "$REPETITIONS" -lt 1 ]]; then
  echo "FAIL: --repetitions must be a positive integer" >&2
  exit 1
fi
if [[ "$EVAL_SUITE" == "all" && "$HELDOUT_CHECK" -eq 1 ]]; then
  echo "FAIL: --eval-suite all already includes the heldout suite; drop --heldout-check or use --eval-suite core" >&2
  exit 1
fi

compare_runs() {
  # $1 base jsonl, $2 cand jsonl, $3 heldout check (0/1), $4 base heldout, $5 cand heldout.
  # Each file may hold one record per repetition; the comparison uses medians.
  # Prints a single line: "1 <reason>" or "0 <reason>".
  python3 - "$1" "$2" "$3" "$4" "$5" <<'PY'
import json
import statistics
import sys

base_path, cand_path, heldout_check, base_heldout_path, cand_heldout_path = sys.argv[1:6]


def load_all(path):
    records = []
    try:
        with open(path) as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    records.append(json.loads(line))
                except ValueError:
                    pass
    except OSError:
        pass
    return records


def med(records, key):
    values = [int(rec.get(key) or 0) for rec in records]
    return statistics.median(values) if values else 0


def token_total(records):
    values = [
        int(rec.get("total_sum_prompt") or 0) + int(rec.get("total_gen_tok") or 0)
        for rec in records
    ]
    return statistics.median(values) if values else 0


def totals(records):
    return sorted({int(rec.get("total") or 0) for rec in records})


def fmt_num(value):
    return int(value) if float(value).is_integer() else value


def set_str(values):
    return ",".join(str(value) for value in values) or "0"


base_records = load_all(base_path)
cand_records = load_all(cand_path)
base_totals = totals(base_records)
cand_totals = totals(cand_records)
if (
    not base_records
    or not cand_records
    or len(base_totals) != 1
    or len(cand_totals) != 1
    or base_totals != cand_totals
    or base_totals[0] <= 0
):
    print(f"0 total task count mismatch {set_str(cand_totals)} != {set_str(base_totals)}")
    raise SystemExit(0)
if len(base_records) != len(cand_records):
    print(
        f"0 repetition count mismatch {len(cand_records)} != {len(base_records)}"
    )
    raise SystemExit(0)

task_total = base_totals[0]
base_passed = med(base_records, "passed")
cand_passed = med(cand_records, "passed")
base_py = med(base_records, "total_py_calls")
cand_py = med(cand_records, "total_py_calls")
base_turns = med(base_records, "total_turns")
cand_turns = med(cand_records, "total_turns")
base_tokens = token_total(base_records)
cand_tokens = token_total(cand_records)

core_win = False
reason = "no metric improved"
if cand_passed > base_passed:
    core_win = True
    reason = f"passed {fmt_num(cand_passed)}/{task_total} > {fmt_num(base_passed)}/{task_total}"
elif cand_passed < base_passed:
    reason = f"passed {fmt_num(cand_passed)}/{task_total} < {fmt_num(base_passed)}/{task_total}"
elif cand_py > base_py:
    reason = f"same pass but py_calls {fmt_num(cand_py)} > {fmt_num(base_py)}"
elif cand_py < base_py:
    core_win = True
    reason = f"same pass, fewer py_calls {fmt_num(cand_py)} < {fmt_num(base_py)}"
elif cand_turns < base_turns:
    core_win = True
    reason = (
        f"same pass, same py_calls, fewer turns "
        f"{fmt_num(cand_turns)} < {fmt_num(base_turns)}"
    )
elif cand_turns > base_turns:
    reason = (
        f"same pass, same py_calls, more turns "
        f"{fmt_num(cand_turns)} > {fmt_num(base_turns)}"
    )
elif cand_tokens < base_tokens:
    core_win = True
    reason = (
        f"same pass, same py_calls, same turns, fewer tokens "
        f"{fmt_num(cand_tokens)} < {fmt_num(base_tokens)}"
    )

if core_win and heldout_check == "1":
    base_heldout = load_all(base_heldout_path)
    cand_heldout = load_all(cand_heldout_path)
    base_ht = totals(base_heldout)
    cand_ht = totals(cand_heldout)
    if (
        not base_heldout
        or not cand_heldout
        or len(base_ht) != 1
        or len(cand_ht) != 1
        or base_ht != cand_ht
        or base_ht[0] <= 0
    ):
        core_win = False
        reason = f"heldout task count mismatch {set_str(cand_ht)} != {set_str(base_ht)} (core: {reason})"
    elif len(base_heldout) != len(cand_heldout):
        core_win = False
        reason = (
            f"heldout repetition count mismatch {len(cand_heldout)} != "
            f"{len(base_heldout)} (core: {reason})"
        )
    else:
        base_h = med(base_heldout, "passed")
        cand_h = med(cand_heldout, "passed")
        if cand_h < base_h:
            core_win = False
            reason = (
                f"heldout regressed {fmt_num(cand_h)}/{base_ht[0]} < "
                f"{fmt_num(base_h)}/{base_ht[0]} (core: {reason})"
            )

runs = max(len(base_records), len(cand_records))
suffix = f" [median of {runs} runs]" if runs > 1 else ""
print(f"{'1' if core_win else '0'} {reason}{suffix}")
PY
}

if [[ "$COMPARE_BASE" != "" ]]; then
  if [[ -z "$COMPARE_CAND" ]]; then
    echo "FAIL: --compare requires BASE and CAND eval.jsonl paths" >&2
    exit 1
  fi
  if [[ "$HELDOUT_CHECK" -eq 1 ]]; then
    if [[ -z "$COMPARE_BASE_HELDOUT" || -z "$COMPARE_CAND_HELDOUT" ]]; then
      echo "FAIL: --heldout-check with --compare requires BASE_HELDOUT and CAND_HELDOUT paths" >&2
      exit 1
    fi
    for heldout_file in "$COMPARE_BASE_HELDOUT" "$COMPARE_CAND_HELDOUT"; do
      if [[ ! -f "$heldout_file" ]]; then
        echo "FAIL: heldout eval record not found: $heldout_file" >&2
        exit 1
      fi
    done
  fi
  compare_runs "$COMPARE_BASE" "$COMPARE_CAND" "$HELDOUT_CHECK" \
    "${COMPARE_BASE_HELDOUT:-}" "${COMPARE_CAND_HELDOUT:-}"
  exit $?
fi

BRANCH=$(git -C "$ROOT" branch --show-current 2>/dev/null || echo "")
if [[ "$DRY_RUN" -eq 0 ]]; then
  if [[ -n "$(git -C "$ROOT" status --porcelain)" ]]; then
    echo "FAIL: working tree is dirty; commit or stash first (use --dry-run to preview)" >&2
    exit 1
  fi
  if [[ ( "$BRANCH" == "main" || "$BRANCH" == "master" ) && "$ALLOW_MAIN" -eq 0 ]]; then
    echo "FAIL: refusing to run on branch '$BRANCH'; use --allow-main to override" >&2
    exit 1
  fi
fi

START_BRANCH="$BRANCH"
START_HEAD=$(git -C "$ROOT" rev-parse HEAD)

case "$GG_BIN" in
  "$ROOT"/target/debug/gg) BUILD_PROFILE="debug" ;;
  "$ROOT"/target/release/gg) BUILD_PROFILE="release" ;;
  *)
    echo "FAIL: GG_BIN must be $ROOT/target/debug/gg or $ROOT/target/release/gg (got: $GG_BIN)" >&2
    exit 1
    ;;
esac

if [[ "$WORK_DIR" != /* ]]; then
  WORK_DIR="$PWD/$WORK_DIR"
fi
cd "$ROOT"

mkdir -p "$WORK_DIR"
WORK_DIR=$(cd "$WORK_DIR" && pwd)
case "$WORK_DIR/" in
  "$ROOT"/*)
    rmdir "$WORK_DIR" 2>/dev/null || true
    echo "FAIL: work dir $WORK_DIR is inside the repository; choose a path outside $ROOT" >&2
    exit 1
    ;;
esac

LEDGER="$WORK_DIR/ledger.jsonl"
BASELINE="$WORK_DIR/baseline.jsonl"
BASELINE_HELDOUT="$WORK_DIR/baseline-heldout.jsonl"
BASELINE_TESTS="$WORK_DIR/baseline-tests.log"
BASE_SESSIONS="$WORK_DIR/baseline-sessions"
BASE_HELDOUT_SESSIONS="$WORK_DIR/baseline-heldout-sessions"
DIGEST="$WORK_DIR/baseline-digest.md"
EVAL_SCRIPT="$WORK_DIR/pristine-eval.sh"

echo "improve-loop: iterations=$ITERATIONS suite=$EVAL_SUITE heldout_check=$HELDOUT_CHECK max_turns=$MAX_TURNS repetitions=$REPETITIONS"
echo "improve-loop: work_dir=$WORK_DIR branch=$BRANCH head=$START_HEAD dry_run=$DRY_RUN"

build_gg() {
  if [[ "$BUILD_PROFILE" == "release" ]]; then
    cargo build --quiet --release
  else
    cargo build --quiet
  fi
}

revert_tree() {
  if [[ -n "$START_BRANCH" ]]; then
    git -C "$ROOT" checkout --quiet --force "$START_BRANCH"
  fi
  git -C "$ROOT" reset --hard --quiet "$START_HEAD"
  git -C "$ROOT" clean -fd >/dev/null 2>&1 || true
  local restored
  restored=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo "")
  if [[ "$restored" != "$START_HEAD" ]]; then
    echo "FAIL: could not restore $START_HEAD (HEAD is ${restored:-unknown})" >&2
    exit 1
  fi
}

prepare_harness() {
  if [[ -f "$EVAL_SCRIPT" ]]; then
    return
  fi
  # A normal run is clean, so the committed script and the working copy are
  # the same. A dry-run may be previewing uncommitted harness changes, so use
  # the file the operator can see.
  if [[ "$DRY_RUN" -eq 1 ]]; then
    cp "$ROOT/scripts/eval.sh" "$EVAL_SCRIPT"
  elif ! git -C "$ROOT" show "$START_HEAD:scripts/eval.sh" > "$EVAL_SCRIPT" 2>/dev/null; then
    echo "FAIL: cannot read scripts/eval.sh at $START_HEAD" >&2
    exit 1
  fi
  if ! grep -q 'EVAL_ROOT' "$EVAL_SCRIPT"; then
    echo "FAIL: scripts/eval.sh does not support EVAL_ROOT; commit the harness update first" >&2
    exit 1
  fi
  chmod +x "$EVAL_SCRIPT"
}

test_count() {
  # $1 log file. Sums the passed count from every `test result: ok.` line.
  python3 - "$1" <<'PY'
import re
import sys

total = 0
with open(sys.argv[1], errors="replace") as fh:
    for line in fh:
        line = re.sub(r"\x1b\[[0-9;]*m", "", line)
        match = re.match(r"test result: ok\. (\d+) passed", line)
        if match:
            total += int(match.group(1))
print(total)
PY
}

protected_path() {
  case "$1" in
    scripts/*|Cargo.toml|Cargo.lock|fixtures/*|benches/*) return 0 ;;
    tests/*|*/tests/*|*/tests.rs|*_test.rs|test_*.py) return 0 ;;
  esac
  return 1
}

changed_paths() {
  {
    git -C "$ROOT" diff --name-only "$START_HEAD" --
    git -C "$ROOT" ls-files --others --exclude-standard
  } | sort -u
}

removed_test_lines() {
  git -C "$ROOT" diff --unified=0 "$START_HEAD" -- '*.rs' \
    | grep -E '^-' \
    | grep -Ev '^---' \
    | grep -E '#\[(tokio::)?test\]|#\[cfg\(test\)\]|assert(_eq|_ne)?!|debug_assert' \
    || true
}

run_eval() {
  # $1 suite, $2 sessions dir, $3 out jsonl.
  # Returns 0 when eval.sh completed (rc 0 or 1) and $out gained a new
  # non-empty JSONL line; returns 1 on crash (rc > 1) or no new record.
  local suite="$1" sessions="$2" out="$3" rc=0 before=0 after=0
  if [[ -f "$out" ]]; then
    before=$(wc -l < "$out")
  fi
  set +e
  EVAL_ROOT="$ROOT" EVAL_SUITE="$suite" EVAL_SESSIONS_DIR="$sessions" EVAL_OUT="$out" \
    GG_BIN="$GG_BIN" "$EVAL_SCRIPT"
  rc=$?
  set -e
  if [[ -f "$out" ]]; then
    after=$(wc -l < "$out")
  fi
  if [[ $rc -le 1 && $after -gt $before && -n "$(tail -1 "$out" 2>/dev/null)" ]]; then
    return 0
  fi
  echo "FAIL: eval.sh exited $rc (1 = some tasks failed, >1 = crash) or wrote no new record" >&2
  return 1
}

run_suite() {
  # $1 suite, $2 sessions dir, $3 out jsonl. Runs REPETITIONS times.
  local suite="$1" sessions="$2" out="$3" run=0
  : > "$out"
  for ((run = 1; run <= REPETITIONS; run++)); do
    echo "eval run $run/$REPETITIONS (suite: $suite)"
    run_eval "$suite" "$sessions" "$out" || return 1
  done
}

capture_baseline() {
  prepare_harness
  echo "Building gg ($BUILD_PROFILE)..."
  build_gg
  if [[ ! -f "$BASELINE_TESTS" ]]; then
    echo "Recording baseline test count..."
    if ! cargo test --quiet >"$BASELINE_TESTS" 2>&1; then
      echo "FAIL: baseline cargo test failed" >&2
      tail -20 "$BASELINE_TESTS" >&2
      exit 1
    fi
  fi
  BASELINE_TEST_COUNT=$(test_count "$BASELINE_TESTS")
  if [[ -f "$BASELINE" ]]; then
    echo "Baseline already captured: $BASELINE"
  else
    echo "Capturing baseline (suite: $EVAL_SUITE, runs: $REPETITIONS)..."
    run_suite "$EVAL_SUITE" "$BASE_SESSIONS" "$BASELINE" || exit 1
  fi
  if [[ "$HELDOUT_CHECK" -eq 1 ]]; then
    if [[ -f "$BASELINE_HELDOUT" ]]; then
      echo "Heldout baseline already captured: $BASELINE_HELDOUT"
    else
      echo "Capturing heldout baseline (runs: $REPETITIONS)..."
      run_suite heldout "$BASE_HELDOUT_SESSIONS" "$BASELINE_HELDOUT" || exit 1
    fi
  fi
}

digest_baseline() {
  if [[ -f "$DIGEST" ]]; then
    echo "Digest already captured: $DIGEST"
  else
    echo "Digesting baseline sessions..."
    python3 scripts/session-digest.py --sessions-dir "$BASE_SESSIONS" > "$DIGEST"
  fi
}

build_prompt() {
  python3 - "$ROOT" "$WORK_DIR" "$EVAL_SUITE" "$MAX_TURNS" "$REPETITIONS" "$TARGET_SCOPE" \
    "$BASELINE" "$DIGEST" "$LEDGER" <<'PY'
import json
import statistics
import sys

(
    root,
    work_dir,
    eval_suite,
    max_turns,
    repetitions,
    target_scope,
    baseline_path,
    digest_path,
    ledger_path,
) = sys.argv[1:10]


def load_all(path):
    records = []
    try:
        with open(path) as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    records.append(json.loads(line))
                except ValueError:
                    pass
    except OSError:
        pass
    return records


def load_lines(path):
    lines = []
    try:
        with open(path) as fh:
            for line in fh:
                line = line.strip()
                if line:
                    lines.append(line)
    except OSError:
        pass
    return lines


def med(values):
    numbers = [int(value or 0) for value in values]
    return statistics.median(numbers) if numbers else 0


def fmt_num(value):
    return int(value) if float(value).is_integer() else value


base_records = load_all(baseline_path)
digest_lines = load_lines(digest_path)

ledger_lines = []
for line in load_lines(ledger_path):
    try:
        entry = json.loads(line)
    except ValueError:
        continue
    outcome = entry.get("outcome") or "?"
    summary = entry.get("summary") or ""
    ledger_lines.append(f"- {outcome}: {summary}")
if not ledger_lines:
    ledger_lines = ["- (no prior iterations)"]

task_names = []
task_data = {}
for rec in base_records:
    for result in rec.get("results") or []:
        name = result.get("task")
        if not name:
            continue
        if name not in task_data:
            task_names.append(name)
            task_data[name] = {
                "passes": [],
                "timed_out": [],
                "turns": [],
                "calls": [],
                "py": [],
                "sum_prompt": [],
                "gen_tok": [],
            }
        entry = task_data[name]
        entry["passes"].append(bool(result.get("pass")))
        entry["timed_out"].append(bool(result.get("timed_out")))
        entry["turns"].append(result.get("turns", 0))
        entry["calls"].append(result.get("calls", 0))
        entry["py"].append(result.get("py_calls", 0))
        entry["sum_prompt"].append(result.get("sum_prompt", 0))
        entry["gen_tok"].append(result.get("gen_tok", 0))

task_lines = []
for name in task_names:
    entry = task_data[name]
    passes = sum(entry["passes"])
    runs = len(entry["passes"])
    if passes == runs:
        status = "PASS"
    elif passes == 0 and any(entry["timed_out"]):
        status = "TIMEOUT"
    elif passes == 0:
        status = "FAIL"
    else:
        status = "MIX"
    task_lines.append(
        f"{name:<24} {status:<8} turns={fmt_num(med(entry['turns'])):<4} "
        f"calls={fmt_num(med(entry['calls'])):<4} py={fmt_num(med(entry['py'])):<3} "
        f"sum_prompt={fmt_num(med(entry['sum_prompt'])):<8} "
        f"gen_tok={fmt_num(med(entry['gen_tok']))}"
    )
if not task_lines:
    task_lines = ["(baseline not captured)"]

if target_scope:
    scope_line = f"Target scope: {target_scope}"
else:
    scope_line = "No target scope given; choose the highest-leverage area yourself."

nl = "\n"
print(f"""You are the hypothesis editor for the ganggang improve loop.

Repository: {root}
Baseline suite: {eval_suite} (max turns per task: {max_turns}, runs per side: {repetitions})

Baseline task results (median across runs):
{nl.join(task_lines)}

Baseline session friction digest:
{nl.join(digest_lines) if digest_lines else '(no digest)'}

Prior iterations (ledger):
{nl.join(ledger_lines)}

{scope_line}

Strict rules:
- Make ONE focused hypothesis change to the gg agent: for example src/agent/context.rs, src/agent/tools/, or compaction/loop heuristics.
- Never weaken tests, and never remove test or assertion lines from any file.
- Never edit scripts/, Cargo.toml, Cargo.lock, fixtures/, benches/, or a dedicated test path (tests/, */tests.rs, */tests/*, *_test.rs, test_*.py).
- Do not read, grep or inspect scripts/eval.sh or other eval definitions; the heldout check is meant to be unseen.
- Do not run git commit, git checkout, git reset, or switch branches. Leave the change uncommitted.
- Keep the change minimal and reversible.
- When done, run `cargo test` to confirm the tree still passes, and finish with a one-paragraph summary of the change and why it should improve the eval.
""")
PY
}

run_gate() {
  # $1 log file. Runs the AGENTS.md gate; returns nonzero on the first failure.
  local log="$1"
  cargo fmt --all >"$log" 2>&1 || return 1
  cargo test >>"$log" 2>&1 || return 1
  cargo clippy --all-targets -- -D warnings >>"$log" 2>&1 || return 1
  build_gg >>"$log" 2>&1 || return 1
  return 0
}

ledger_append() {
  # $1 outcome, $2 summary, $3 detail
  python3 - "$LEDGER" "$1" "$2" "$3" <<'PY'
import json
import sys
import time

path, outcome, summary, detail = sys.argv[1:5]
rec = {
    "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "outcome": outcome,
    "summary": summary or None,
    "detail": detail or None,
}
with open(path, "a") as fh:
    fh.write(json.dumps(rec) + "\n")
PY
}

extract_summary() {
  # $1 session file. Prints the last assistant text, whitespace-collapsed.
  python3 - "$1" <<'PY'
import json
import sys

text = ""
try:
    with open(sys.argv[1]) as fh:
        for line in fh:
            try:
                entry = json.loads(line)
            except ValueError:
                continue
            if entry.get("type") == "message":
                message = entry.get("message") or {}
                if message.get("role") == "assistant":
                    text = message.get("content") or ""
except OSError:
    pass
print(" ".join(text.split())[:120])
PY
}

report_gate_failure() {
  # $1 iteration, $2 log file, $3 summary, $4 detail.
  revert_tree
  echo "gate failed; reverted"
  echo "--- gate tail ---"
  tail -20 "$2"
  ledger_append "gate_failed" "$3" "$4"
  echo "=== iteration $1: gate_failed ==="
}

for ((iter = 1; iter <= ITERATIONS; iter++)); do
  SESSION_FILE="$WORK_DIR/iter-$iter.jsonl"
  PROMPT_FILE="$WORK_DIR/iter-$iter-prompt.txt"
  echo "=== iteration $iter/$ITERATIONS ==="

  capture_baseline
  digest_baseline
  build_prompt > "$PROMPT_FILE"

  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "--- prompt (dry-run, no editor gg, no git mutations) ---"
    cat "$PROMPT_FILE"
    echo "--- dry-run complete ---"
    break
  fi

  echo "--- hypothesis edit (max turns: $MAX_TURNS) ---"
  # Unset the nesting handshake: the editor runs with this loop's flags and
  # config defaults however the loop was started, so a gg session that
  # launched it does not change what an iteration measures.
  if ! env -u GG_NEST_DEPTH -u GG_PARENT_SESSION -u GG_PARENT_MODEL \
       -u GG_TOOL_CAP -u GG_MAX_TURNS_CAP -u GG_MAX_SECONDS_CAP -u GG_NO_HOOKS \
       "$GG_BIN" --no-hooks --session-file "$SESSION_FILE" --max-turns "$MAX_TURNS" -p - < "$PROMPT_FILE"; then
    echo "WARN: gg exited nonzero; continuing"
  fi

  NEW_HEAD=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo "")
  if [[ "$NEW_HEAD" != "$START_HEAD" ]]; then
    revert_tree
    ledger_append "editor_escaped" "editor moved HEAD" "$START_HEAD -> ${NEW_HEAD:-unknown}"
    echo "=== iteration $iter: editor_escaped (HEAD moved) ==="
    continue
  fi

  CHANGED=$(changed_paths)
  if [[ -z "$CHANGED" ]]; then
    ledger_append "no_change" "gg made no changes" ""
    echo "=== iteration $iter: no_change ==="
    continue
  fi

  BAD_PATHS=""
  while IFS= read -r changed; do
    [[ -z "$changed" ]] && continue
    if protected_path "$changed"; then
      BAD_PATHS+="$changed "
    fi
  done <<< "$CHANGED"
  REMOVED_TESTS=$(removed_test_lines)
  if [[ -n "$BAD_PATHS" || -n "$REMOVED_TESTS" ]]; then
    revert_tree
    detail="protected paths: ${BAD_PATHS:-none}; removed test lines: $(printf '%s' "$REMOVED_TESTS" | tr '\n' ' ' | cut -c1-300)"
    ledger_append "gate_failed" "protected path or test change" "$detail"
    echo "=== iteration $iter: gate_failed (protected path or test change) ==="
    echo "$detail"
    continue
  fi

  echo "--- gate (fmt, test, clippy, build) ---"
  GATE_LOG="$WORK_DIR/iter-$iter-gate.log"
  if ! run_gate "$GATE_LOG"; then
    report_gate_failure "$iter" "$GATE_LOG" "gate failed" \
      "$(tail -2 "$GATE_LOG" | tr '\n' ' ' | tr -s ' ')"
    continue
  fi
  echo "gate passed"

  CAND_TEST_COUNT=$(test_count "$GATE_LOG")
  if [[ "$BASELINE_TEST_COUNT" -gt 0 && "$CAND_TEST_COUNT" -lt "$BASELINE_TEST_COUNT" ]]; then
    report_gate_failure "$iter" "$GATE_LOG" "test count dropped" \
      "baseline $BASELINE_TEST_COUNT, candidate $CAND_TEST_COUNT"
    continue
  fi

  echo "--- candidate eval (suite: $EVAL_SUITE, runs: $REPETITIONS) ---"
  CAND_EVAL="$WORK_DIR/iter-$iter-eval.jsonl"
  CAND_SESSIONS="$WORK_DIR/iter-$iter-sessions"
  if ! run_suite "$EVAL_SUITE" "$CAND_SESSIONS" "$CAND_EVAL"; then
    revert_tree
    ledger_append "gate_failed" "candidate eval crashed" "eval.sh exited >1"
    echo "=== iteration $iter: gate_failed (eval crash) ==="
    continue
  fi

  CAND_HELDOUT="$WORK_DIR/iter-$iter-heldout.jsonl"
  CAND_HELDOUT_SESSIONS="$WORK_DIR/iter-$iter-heldout-sessions"
  if [[ "$HELDOUT_CHECK" -eq 1 ]]; then
    echo "--- candidate eval (suite: heldout, runs: $REPETITIONS) ---"
    if ! run_suite heldout "$CAND_HELDOUT_SESSIONS" "$CAND_HELDOUT"; then
      revert_tree
      ledger_append "gate_failed" "heldout eval crashed" "eval.sh exited >1"
      echo "=== iteration $iter: gate_failed (heldout eval crash) ==="
      continue
    fi
  fi

  read -r keep verdict < <(compare_runs "$BASELINE" "$CAND_EVAL" "$HELDOUT_CHECK" "$BASELINE_HELDOUT" "$CAND_HELDOUT")
  echo "verdict: keep=$keep ($verdict)"

  if [[ "$keep" == "1" ]]; then
    SUMMARY=$(extract_summary "$SESSION_FILE")
    git -C "$ROOT" add -A
    git -C "$ROOT" commit -m "improve: iteration $iter (${SUMMARY:-no summary})"
    # Advance START_HEAD so subsequent iterations diff, check editor_escaped, and
    # revert against this kept commit. EVAL_SCRIPT stays pinned to the initial
    # pristine commit because prepare_harness skips extraction once it exists.
    START_HEAD=$(git -C "$ROOT" rev-parse HEAD)
    rm -rf "$BASE_SESSIONS" && cp -R "$CAND_SESSIONS" "$BASE_SESSIONS"
    rm -f "$DIGEST"
    cp "$CAND_EVAL" "$BASELINE"
    cp "$GATE_LOG" "$BASELINE_TESTS"
    BASELINE_TEST_COUNT=$(test_count "$BASELINE_TESTS")
    if [[ "$HELDOUT_CHECK" -eq 1 ]]; then
      cp "$CAND_HELDOUT" "$BASELINE_HELDOUT"
    fi
    ledger_append "keep" "$SUMMARY" "$verdict"
    echo "=== iteration $iter: keep ==="
  else
    revert_tree
    SUMMARY=$(extract_summary "$SESSION_FILE")
    ledger_append "revert" "$SUMMARY" "$verdict"
    echo "=== iteration $iter: revert ==="
  fi
done

echo "improve-loop: done (ledger: $LEDGER)"
