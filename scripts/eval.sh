#!/usr/bin/env bash
# Local-compute eval suite: runs 4 deterministic tasks against a live Strata
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
GG_ARGS="${GG_ARGS:-}"
EVAL_OUT="${EVAL_OUT:-}"

if [[ ! -x "$GG_BIN" ]]; then
  echo "FAIL: $GG_BIN not built (cargo build)" >&2
  exit 1
fi
if ! curl -sf -m 5 127.0.0.1:8080/health >/dev/null; then
  echo "FAIL: Strata not answering on 127.0.0.1:8080 (strata launch)" >&2
  exit 1
fi

SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/gg-eval.XXXXXX")
trap 'rm -rf "$SCRATCH"' EXIT

HOME_DIR="$SCRATCH/home"
mkdir -p "$HOME_DIR/.gg"
cat > "$HOME_DIR/.gg/config.toml" <<'EOF'
provider = "strata"

[providers.strata]
base_url = "http://127.0.0.1:8080/v1"
model = "qwen3.8-flash-next-iq3_s"
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

python3 - "$ROOT" "$GG_BIN" "$HOME_DIR" "$EVAL_TIMEOUT" "$GG_ARGS" "$EVAL_OUT" "$SCRATCH" "$T1" "$T2" "$T3" "$T4" <<'PY'
import json, os, re, shlex, subprocess, sys, time

root, gg_bin, home_dir, timeout_s, gg_args, eval_out, scratch_dir, t1, t2, t3, t4 = sys.argv[1:]
timeout_s = int(timeout_s)
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
]

results = []
for name, cwd, prompt, check_fn in tasks:
    env = dict(os.environ, HOME=home_dir)
    session_file = os.path.join(scratch_dir, f"{name}.jsonl")
    cmd = [gg_bin, "--no-hooks", "--mode", "json", "--session-file", session_file, *extra_args, prompt]
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
    bash_calls = 0
    redundant_cd = 0
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
        elif et == "tool_call_started":
            calls += 1
            tname = ev.get("name", "")
            args = ev.get("args") or {}
            if tname == "bash":
                bash_calls += 1
                cstr = str(args.get("command", ""))
                if f"cd {cwd}" in cstr:
                    redundant_cd += 1
                # Count legitimate bash (cargo/git) as non-workaround; sed/cat/grep/rg/find/ls/python as workaround
                workarounds = {"sed", "cat", "head", "tail", "grep", "rg", "find", "ls", "python", "python3"}
                is_workaround = False
                for seg in re.split(r"\s*(?:&&|\|\||[;|\n])\s*", cstr):
                    tokens = seg.strip().split()
                    while tokens and "=" in tokens[0] and not tokens[0].startswith("="):
                        tokens.pop(0)
                    if not tokens or tokens[0] == "cd":
                        continue
                    if os.path.basename(tokens[0]) in workarounds:
                        is_workaround = True
                        break
                if not is_workaround:
                    native_calls += 1
            else:
                native_calls += 1

    answer = "".join(answer_parts)
    peak_prompt = 0
    gen_tok = 0
    reason_chars = 0
    out_chars = 0
    if os.path.exists(session_file):
        with open(session_file) as sf:
            for sl in sf:
                try:
                    se = json.loads(sl)
                except json.JSONDecodeError:
                    continue
                if se.get("type") == "message":
                    m = se.get("message") or {}
                    u = m.get("usage") or {}
                    peak_prompt = max(peak_prompt, int(u.get("prompt_tokens") or 0))
                    gen_tok += int(u.get("completion_tokens") or 0)
                    reason_chars += len(m.get("reasoning") or "")
                    out_chars += len(m.get("content") or "")
                    for tc in m.get("tool_calls") or []:
                        out_chars += len(json.dumps(tc.get("args") or {}))

    ok = (not timed_out) and check_fn(answer, cwd)
    native_pct = (100.0 * native_calls / calls) if calls else 100.0
    think_pct = (100.0 * reason_chars / (reason_chars + out_chars)) if (reason_chars + out_chars) else 0.0
    results.append({
        "task": name,
        "pass": ok,
        "timed_out": timed_out,
        "wall_s": round(wall_s, 1),
        "turns": turns,
        "calls": calls,
        "native_pct": round(native_pct, 1),
        "redundant_cd": redundant_cd,
        "peak_prompt": peak_prompt,
        "gen_tok": gen_tok,
        "think_pct": round(think_pct, 1),
    })

print(f"{'Task':<18} {'Status':<6} {'Wall':>6} {'Turns':>5} {'Calls':>5} {'Native%':>7} {'Cd':>3} {'PeakCtx':>8} {'GenTok':>7} {'Think%':>7}")
print("-" * 82)
for r in results:
    st = "PASS" if r["pass"] else ("TIME" if r["timed_out"] else "FAIL")
    print(f"{r['task']:<18} {st:<6} {r['wall_s']:>5.1f}s {r['turns']:>5} {r['calls']:>5} {r['native_pct']:>6.1f}% {r['redundant_cd']:>3} {r['peak_prompt']:>8,} {r['gen_tok']:>7,} {r['think_pct']:>6.1f}%")

tot_wall = sum(r["wall_s"] for r in results)
tot_turns = sum(r["turns"] for r in results)
tot_calls = sum(r["calls"] for r in results)
tot_gen = sum(r["gen_tok"] for r in results)
passed = sum(1 for r in results if r["pass"])
print("-" * 82)
print(f"{'TOTAL':<18} {passed}/{len(results):<4} {tot_wall:>5.1f}s {tot_turns:>5} {tot_calls:>5} {'':>7} {sum(r['redundant_cd'] for r in results):>3} {max(r['peak_prompt'] for r in results):>8,} {tot_gen:>7,}")

if eval_out:
    rev = subprocess.check_output(["git", "-C", root, "rev-parse", "--short", "HEAD"], text=True).strip()
    rec = {"ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "rev": rev, "gg_bin": gg_bin, "passed": passed, "total": len(results), "results": results}
    with open(eval_out, "a") as f:
        f.write(json.dumps(rec) + "\n")

sys.exit(0 if passed == len(results) else 1)
PY
