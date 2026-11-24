#!/usr/bin/env bash
# Local-compute eval suite: runs 7 deterministic tasks against a live Strata
# server in isolated temp workspaces and reports functional pass/fail plus
# tool-discipline & token-efficiency metrics.
#
# Usage:
#   scripts/eval.sh
#   GG_BIN=./target/release/gg EVAL_OUT=/tmp/eval.jsonl scripts/eval.sh
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." >/dev/null 2>&1 && pwd)
GG_BIN="${GG_BIN:-$ROOT/target/debug/gg}"
EVAL_TIMEOUT="${EVAL_TIMEOUT:-180}"
EVAL_MAX_TURNS="${EVAL_MAX_TURNS:-25}"
GG_ARGS="${GG_ARGS:-}"
EVAL_OUT="${EVAL_OUT:-}"

if [[ ! -x "$GG_BIN" ]]; then
  echo "FAIL: $GG_BIN not built (cargo build)" >&2
  exit 1
fi

# Auto-detect the model Strata has loaded. The config below deliberately omits
# `model`, so `gg` adopts the active model from /health on every run.
HEALTH_JSON=$(curl -sf -m 5 http://127.0.0.1:8080/health 2>/dev/null) || {
  echo "FAIL: Strata not answering on 127.0.0.1:8080/health (strata launch)" >&2
  exit 1
}
HEALTH_PARSED=$(printf '%s' "$HEALTH_JSON" | python3 -c '
import json, sys
try:
    health = json.load(sys.stdin)
except Exception:
    sys.exit(1)
model = str(health.get("model") or "").strip()
if not model:
    sys.exit(1)
try:
    max_context = int(health.get("max_context") or 0)
except (TypeError, ValueError):
    max_context = 0
print(f"{model}\t{max_context}")
') || {
  echo "FAIL: Strata /health did not report a loaded model" >&2
  exit 1
}
IFS=$'\t' read -r STRATA_MODEL STRATA_MAX_CONTEXT <<< "$HEALTH_PARSED"

SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/gg-eval.XXXXXX")
trap 'rm -rf "$SCRATCH"' EXIT

HOME_DIR="$SCRATCH/home"
mkdir -p "$HOME_DIR/.gg"
cat > "$HOME_DIR/.gg/config.toml" <<'EOF'
provider = "strata"

[providers.strata]
base_url = "http://127.0.0.1:8080/v1"
EOF

# --- Task 1: regex grep ---
T1="$SCRATCH/t1"
mkdir -p "$T1/src" "$T1/target"
printf 'fn handle_foo() {}\n' > "$T1/src/a.rs"
printf 'fn other() {}\nfn handle_bar() {}\n' > "$T1/src/b.rs"
printf 'fn handle_baz() {}\n' > "$T1/src/c.rs"
printf 'fn handle_foo() {}\n' > "$T1/target/gen.rs"
printf 'ignored.rs\n' > "$T1/.gitignore"
printf 'fn handle_bar() {}\n' > "$T1/ignored.rs"

# --- Task 2: find files ---
T2="$SCRATCH/t2"
mkdir -p "$T2/src" "$T2/target/debug" "$T2/node_modules/pkg" "$T2/scratch"
printf 'fn main() {}\n' > "$T2/src/main.rs"
printf 'pub fn util() {}\n' > "$T2/src/util.rs"
printf 'fn gen() {}\n' > "$T2/target/debug/out.rs"
printf 'fn dep() {}\n' > "$T2/node_modules/pkg/a.rs"
printf 'scratch/\n' > "$T2/.gitignore"
printf 'fn tmp() {}\n' > "$T2/scratch/tmp.rs"

# --- Task 3: large read past 50 KiB ---
T3="$SCRATCH/t3"
mkdir -p "$T3"
python3 - "$T3/big.log" <<'PY'
import sys
with open(sys.argv[1], "w") as f:
    for i in range(1, 3001):
        if i == 2517:
            f.write("2517: TOKEN-EVAL-9472 padding-xxxxxxxxxxxxxxxxxxxxxxxx\n")
        else:
            f.write(f"{i:04d}: filler-line-padding-xxxxxxxxxxxxxxxxxxxxxxxxxxxx\n")
PY

# --- Task 4: multi-file edit + cargo test ---
T4="$SCRATCH/t4"
mkdir -p "$T4/src"
cat > "$T4/Cargo.toml" <<'EOF'
[package]
name = "eval_math"
version = "0.1.0"
edition = "2024"
EOF
cat > "$T4/src/lib.rs" <<'EOF'
pub mod math;

#[cfg(test)]
mod tests {
    use super::math::add_one;

    #[test]
    fn adds_one() {
        assert_eq!(add_one(4), 5);
    }
}
EOF
cat > "$T4/src/math.rs" <<'EOF'
pub fn add_one(x: i32) -> i32 {
    x + 2
}
EOF

# --- Task 5: load a repo skill ---
T5="$SCRATCH/t5"
mkdir -p "$T5/.agents/skills/banner-format"
cat > "$T5/.agents/skills/banner-format/SKILL.md" <<'EOF'
---
name: banner-format
description: "Format release banners with the required prefix and checksum tag."
---

# Banner format

Format any word `<word>` as `GANGGANG::<UPPERCASE_WORD>::V7`.

- Uppercase the word.
- Prefix with `GANGGANG::`.
- Suffix with `::V7`.
EOF

# --- Task 6: fix a config value hidden behind noisy output ---
T6="$SCRATCH/t6"
mkdir -p "$T6"
printf 'DELTA=41\n' > "$T6/config.env"
cat > "$T6/run_checks.sh" <<'EOF'
#!/usr/bin/env bash
set -u
here=$(cd -- "$(dirname -- "$0")" >/dev/null 2>&1 && pwd)
# shellcheck disable=SC1091
source "$here/config.env"

i=1
while [ "$i" -le 320 ]; do
  printf 'check %03d: PASS detail-padding-padding-padding-padding-pad\n' "$i"
  i=$((i + 1))
done

if [ "${DELTA:-}" != "42" ]; then
  printf 'FAIL: check_delta expected=42 actual=%s\n' "${DELTA:-}"
  exit 1
fi

echo "ALL CHECKS PASSED"
EOF
chmod +x "$T6/run_checks.sh"

# --- Task 7: large-file edit + cargo test ---
T7="$SCRATCH/t7"
mkdir -p "$T7/src"
cat > "$T7/Cargo.toml" <<'EOF'
[package]
name = "eval_tax"
version = "0.1.0"
edition = "2024"
EOF
python3 - "$T7/src/lib.rs" <<'PY'
import sys

lines = [
    "//! Eval crate with a deliberately wrong tax helper.",
    "#![allow(dead_code)]",
    "",
]
for i in range(1, 171):
    lines.append(f"fn helper_{i:03d}(x: i32) -> i32 {{")
    lines.append("    x + 1")
    lines.append("}")
for i in range(14):
    lines.append("// padding comment for layout" if i else "// Tax helpers live below this block.")
lines.append("/// Compute a 10% tax (integer division).")
lines.append("pub fn compute_tax(amount: i32) -> i32 {")
lines.append("    amount * 15 / 100")
lines.append("}")
for i in range(171, 206):
    lines.append(f"fn helper_{i:03d}(x: i32) -> i32 {{")
    lines.append("    x + 1")
    lines.append("}")
lines.extend([
    "",
    "#[cfg(test)]",
    "mod tests {",
    "    use super::compute_tax;",
    "",
    "    #[test]",
    "    fn tax_is_ten_percent() {",
    "        assert_eq!(compute_tax(200), 20);",
    "    }",
    "}",
])
with open(sys.argv[1], "w") as f:
    f.write("\n".join(lines) + "\n")
PY

python3 - "$ROOT" "$GG_BIN" "$HOME_DIR" "$EVAL_TIMEOUT" "$GG_ARGS" "$EVAL_OUT" "$SCRATCH" \
    "$T1" "$T2" "$T3" "$T4" "$T5" "$T6" "$T7" \
    "$STRATA_MODEL" "$STRATA_MAX_CONTEXT" "$EVAL_MAX_TURNS" <<'PY'
import json, os, re, shlex, subprocess, sys, time

(
    root,
    gg_bin,
    home_dir,
    timeout_s,
    gg_args,
    eval_out,
    scratch_dir,
    t1,
    t2,
    t3,
    t4,
    t5,
    t6,
    t7,
    model,
    max_context,
    eval_max_turns,
) = sys.argv[1:]
timeout_s = int(timeout_s)
max_context = int(max_context)
eval_max_turns = int(eval_max_turns)
extra_args = shlex.split(gg_args)

tasks = [
    (
        "T1_regex_grep",
        t1,
        "Find all function definitions matching `fn handle_(foo|bar)` in this project. "
        "Reply with only the `file:line` pairs, one per line.",
        lambda ans, cwd: ("src/a.rs" in ans and "src/b.rs" in ans and "target/gen.rs" not in ans and "ignored.rs" not in ans),
    ),
    (
        "T2_find_files",
        t2,
        "List all `.rs` project source files (excluding build/dependency/ignored directories). "
        "Reply with the relative paths only.",
        lambda ans, cwd: ("src/main.rs" in ans and "src/util.rs" in ans and "target/" not in ans and "node_modules/" not in ans and "scratch/" not in ans),
    ),
    (
        "T3_large_read",
        t3,
        "Read line 2517 of `big.log` and reply with just the `TOKEN-EVAL-XXXX` token on that line.",
        lambda ans, cwd: "TOKEN-EVAL-9472" in ans,
    ),
    (
        "T4_edit_and_test",
        t4,
        "Inspect the Rust crate in this directory, fix the bug in `add_one`, and run `cargo test` to verify it passes.",
        lambda ans, cwd: subprocess.run(["cargo", "test", "--quiet"], cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0,
    ),
    (
        "T5_skill_load",
        t5,
        "Format a release banner for the word 'cockatoo' following the repo skill. "
        "Reply with only the formatted banner.",
        lambda ans, cwd: "GANGGANG::COCKATOO::V7" in ans,
    ),
    (
        "T6_test_tail",
        t6,
        "Run ./run_checks.sh, find the failing check, fix config.env so ./run_checks.sh passes, "
        "and confirm it exits 0.",
        lambda ans, cwd: subprocess.run(["./run_checks.sh"], cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0,
    ),
    (
        "T7_large_edit",
        t7,
        "Fix the failing test in this Rust crate and run cargo test to verify it passes.",
        lambda ans, cwd: subprocess.run(["cargo", "test", "--quiet"], cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0,
    ),
]

results = []
for name, cwd, prompt, check_fn in tasks:
    env = dict(os.environ, HOME=home_dir)
    session_file = os.path.join(scratch_dir, f"{name}.jsonl")
    cmd = [
        gg_bin,
        "--no-hooks",
        "--mode",
        "json",
        "--session-file",
        session_file,
        "--max-turns",
        str(eval_max_turns),
        *extra_args,
        prompt,
    ]
    t0 = time.monotonic()
    try:
        proc = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout_s)
        timed_out = False
    except subprocess.TimeoutExpired as e:
        timed_out = True
        proc = e
    wall_s = time.monotonic() - t0

    stdout = (proc.stdout or "") if hasattr(proc, "stdout") else ""
    turns = 0
    calls = 0
    native_calls = 0
    tool_turns = 0
    py_calls = 0
    redundant_cd = 0
    empty_trunc = 0
    tools = {}
    answer_parts = []
    session_id = None

    for line in stdout.splitlines():
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        et = ev.get("type")
        if et == "session":
            session_id = ev.get("id")
        elif et == "turn_started":
            turns += 1
            answer_parts = []
        elif et == "assistant_text":
            answer_parts.append(ev.get("delta", ""))
        elif et == "turn_ended":
            if ev.get("stop_reason") in {"length", "stream_cut"}:
                empty_trunc += 1
        elif et == "warned":
            message = ev.get("message", "")
            if "had no content" in message or "before producing visible output" in message:
                empty_trunc += 1
        elif et == "tool_call_started":
            calls += 1
            tname = ev.get("name", "")
            tools[tname] = tools.get(tname, 0) + 1
            args = ev.get("args") or {}
            if tname == "bash":
                cstr = str(args.get("command", ""))
                if f"cd {cwd}" in cstr:
                    redundant_cd += 1
                # Count legitimate bash (cargo/git) as non-workaround; sed/cat/grep/rg/find/ls/python as workaround
                workarounds = {"sed", "cat", "head", "tail", "grep", "rg", "find", "ls", "python", "python3"}
                is_workaround = False
                uses_python = False
                for seg in re.split(r"\s*(?:&&|\|\||[;|\n])\s*", cstr):
                    tokens = seg.strip().split()
                    while tokens and "=" in tokens[0] and not tokens[0].startswith("="):
                        tokens.pop(0)
                    if not tokens or tokens[0] == "cd":
                        continue
                    base = os.path.basename(tokens[0])
                    if base in {"python", "python3"}:
                        uses_python = True
                    if base in workarounds:
                        is_workaround = True
                if uses_python:
                    py_calls += 1
                if not is_workaround:
                    native_calls += 1
            else:
                native_calls += 1

    answer = "".join(answer_parts)
    peak_prompt = 0
    sum_prompt = 0
    gen_tok = 0
    reasoning_tok = 0
    reason_chars = 0
    out_chars = 0
    tool_bytes = 0
    if os.path.exists(session_file):
        with open(session_file) as sf:
            for sl in sf:
                try:
                    se = json.loads(sl)
                except json.JSONDecodeError:
                    continue
                if se.get("type") != "message":
                    continue
                m = se.get("message") or {}
                role = m.get("role")
                u = m.get("usage") or {}
                if role == "tool_result":
                    tool_bytes += len(m.get("content") or "")
                if role != "assistant":
                    continue
                prompt_tokens = int(u.get("prompt_tokens") or 0)
                completion_tokens = int(u.get("completion_tokens") or 0)
                peak_prompt = max(peak_prompt, prompt_tokens)
                sum_prompt += prompt_tokens
                gen_tok += completion_tokens
                details = u.get("completion_tokens_details") or {}
                r_tok = details.get("reasoning_tokens")
                if r_tok is None:
                    r_tok = u.get("reasoning_tokens")
                reasoning_tok += int(r_tok or 0)
                reason_chars += len(m.get("reasoning") or "")
                out_chars += len(m.get("content") or "")
                for tc in m.get("tool_calls") or []:
                    out_chars += len(json.dumps(tc.get("args") or {}))
                if m.get("tool_calls"):
                    tool_turns += 1

    ok = (not timed_out) and check_fn(answer, cwd)
    native_pct = (100.0 * native_calls / calls) if calls else 100.0
    batch = round(calls / tool_turns, 2) if tool_turns else 0.0
    if reasoning_tok > 0 and gen_tok > 0:
        think_pct = min(100.0, 100.0 * reasoning_tok / gen_tok)
    elif reason_chars + out_chars:
        think_pct = 100.0 * reason_chars / (reason_chars + out_chars)
    else:
        think_pct = 0.0
    results.append({
        "task": name,
        "pass": ok,
        "timed_out": timed_out,
        "wall_s": round(wall_s, 1),
        "turns": turns,
        "calls": calls,
        "batch": batch,
        "native_pct": round(native_pct, 1),
        "py_calls": py_calls,
        "redundant_cd": redundant_cd,
        "tool_bytes": tool_bytes,
        "peak_prompt": peak_prompt,
        "sum_prompt": sum_prompt,
        "gen_tok": gen_tok,
        "think_pct": round(think_pct, 1),
        "empty_trunc": empty_trunc,
        "tools": tools,
    })

print(f"Model: {model} (max_context={max_context}) | Binary: {gg_bin}")
print(
    f"{'Task':<18} {'Status':<6} {'Wall':>7} {'Turns':>5} {'Calls':>5} {'Batch':>5} "
    f"{'Native%':>7} {'Py':>4} {'Cd':>3} {'ToolKB':>7} {'PeakCtx':>8} {'SumCtx':>8} "
    f"{'GenTok':>7} {'Think%':>7} {'Cut':>4}"
)
print("-" * 116)
for r in results:
    st = "PASS" if r["pass"] else ("TIME" if r["timed_out"] else "FAIL")
    print(
        f"{r['task']:<18} {st:<6} {r['wall_s']:>6.1f}s {r['turns']:>5} {r['calls']:>5} "
        f"{r['batch']:>5.2f} {r['native_pct']:>6.1f}% {r['py_calls']:>4} "
        f"{r['redundant_cd']:>3} {r['tool_bytes'] / 1024:>7.1f} {r['peak_prompt']:>8,} "
        f"{r['sum_prompt']:>8,} {r['gen_tok']:>7,} {r['think_pct']:>6.1f}% {r['empty_trunc']:>4}"
    )

tot_wall = sum(r["wall_s"] for r in results)
tot_turns = sum(r["turns"] for r in results)
tot_calls = sum(r["calls"] for r in results)
tot_tool_bytes = sum(r["tool_bytes"] for r in results)
tot_sum_prompt = sum(r["sum_prompt"] for r in results)
tot_gen = sum(r["gen_tok"] for r in results)
tot_py = sum(r["py_calls"] for r in results)
tot_cd = sum(r["redundant_cd"] for r in results)
tot_cut = sum(r["empty_trunc"] for r in results)
passed = sum(1 for r in results if r["pass"])
print("-" * 116)
print(
    f"{'TOTAL':<18} {f'{passed}/{len(results)}':<6} {tot_wall:>6.1f}s {tot_turns:>5} {tot_calls:>5} "
    f"{'':>5} {'':>7} {tot_py:>4} {tot_cd:>3} {tot_tool_bytes / 1024:>7.1f} "
    f"{max(r['peak_prompt'] for r in results):>8,} {tot_sum_prompt:>8,} {tot_gen:>7,} {'':>7} {tot_cut:>4}"
)

if eval_out:
    rev = subprocess.check_output(["git", "-C", root, "rev-parse", "--short", "HEAD"], text=True).strip()
    rec = {
        "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "rev": rev,
        "gg_bin": gg_bin,
        "model": model,
        "max_context": max_context,
        "eval_max_turns": eval_max_turns,
        "passed": passed,
        "total": len(results),
        "total_wall_s": round(tot_wall, 1),
        "total_turns": tot_turns,
        "total_calls": tot_calls,
        "total_tool_bytes": tot_tool_bytes,
        "total_sum_prompt": tot_sum_prompt,
        "total_gen_tok": tot_gen,
        "total_py_calls": tot_py,
        "total_empty_trunc": tot_cut,
        "results": results,
    }
    with open(eval_out, "a") as f:
        f.write(json.dumps(rec) + "\n")

sys.exit(0 if passed == len(results) else 1)
PY
