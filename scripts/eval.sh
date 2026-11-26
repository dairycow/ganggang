#!/usr/bin/env bash
# Local-compute eval suite: runs deterministic tasks against a live Strata
# server in isolated temp workspaces and reports functional pass/fail plus
# tool-discipline & token-efficiency metrics.
#
# Suites: core (T1 to T9), heldout (H1 to H3), all (both).
# Sessions land in $SCRATCH by default; set EVAL_SESSIONS_DIR to persist
# them (and record "session_file" per task) across the scratch cleanup.
#
# Usage:
#   scripts/eval.sh
#   EVAL_SUITE=heldout GG_BIN=./target/release/gg EVAL_OUT=/tmp/eval.jsonl scripts/eval.sh
#
# EVAL_ROOT is internal: improve-loop.sh sets it when running a pristine copy
# of this script, so ROOT still names the candidate repository.
set -euo pipefail

# EVAL_ROOT overrides the repository root: improve-loop.sh runs a pristine
# copy of this script from a work directory and points it back at the tree.
ROOT="${EVAL_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." >/dev/null 2>&1 && pwd)}"
GG_BIN="${GG_BIN:-$ROOT/target/debug/gg}"
EVAL_TIMEOUT="${EVAL_TIMEOUT:-180}"
EVAL_MAX_TURNS="${EVAL_MAX_TURNS:-25}"
EVAL_BASE_URL="${EVAL_BASE_URL:-http://127.0.0.1:8080/v1}"
EVAL_MODEL="${EVAL_MODEL:-}"
EVAL_LABEL="${EVAL_LABEL:-}"
GG_ARGS="${GG_ARGS:-}"
EVAL_OUT="${EVAL_OUT:-}"
EVAL_SUITE="${EVAL_SUITE:-core}"
EVAL_SESSIONS_DIR="${EVAL_SESSIONS_DIR:-}"

if [[ "$EVAL_SUITE" != "core" && "$EVAL_SUITE" != "heldout" && "$EVAL_SUITE" != "all" ]]; then
  echo "FAIL: EVAL_SUITE must be one of: core, heldout, all (got: $EVAL_SUITE)" >&2
  exit 1
fi

if [[ ! -x "$GG_BIN" ]]; then
  echo "FAIL: $GG_BIN not built (cargo build)" >&2
  exit 1
fi
GG_BIN=$(cd -- "$(dirname -- "$GG_BIN")" && pwd)/$(basename -- "$GG_BIN")

if [[ -n "$EVAL_SESSIONS_DIR" ]]; then
  mkdir -p "$EVAL_SESSIONS_DIR"
  EVAL_SESSIONS_DIR=$(cd -- "$EVAL_SESSIONS_DIR" && pwd)
fi

if [[ -n "$EVAL_OUT" ]]; then
  mkdir -p "$(dirname -- "$EVAL_OUT")"
  EVAL_OUT=$(cd -- "$(dirname -- "$EVAL_OUT")" && pwd)/$(basename -- "$EVAL_OUT")
fi

BASE_TRIMMED="${EVAL_BASE_URL%/}"
ROOT_URL="${BASE_TRIMMED%/v1}"

# Probe /health first, then fall back to /v1/models (llama-server returns
# {"status":"ok"} on /health and lists the active model on /v1/models).
HEALTH_JSON=$(curl -sf -m 5 "${ROOT_URL}/health" 2>/dev/null) || {
  echo "FAIL: server not answering on ${ROOT_URL}/health" >&2
  exit 1
}
MODELS_JSON=$(curl -sf -m 5 "${BASE_TRIMMED}/models" 2>/dev/null || echo "")

HEALTH_PARSED=$(python3 - "$HEALTH_JSON" "$MODELS_JSON" "$EVAL_MODEL" <<'PY'
import json, sys

health_raw, models_raw, override_model = sys.argv[1:4]
model = ""
max_context = 0

try:
    health = json.loads(health_raw)
    model = str(health.get("model") or "").strip()
    max_context = int(health.get("max_context") or 0)
except Exception:
    pass

if (not model or max_context == 0) and models_raw:
    try:
        data = json.loads(models_raw).get("data") or []
        chosen = None
        for item in data:
            st = item.get("status")
            if isinstance(st, dict) and st.get("value") == "loaded":
                chosen = item
                break
        if chosen is None and data:
            chosen = data[0]
        if chosen is not None:
            if not model:
                model = str(chosen.get("id") or "").strip()
            if max_context == 0:
                meta = chosen.get("meta") if isinstance(chosen.get("meta"), dict) else {}
                for k in ("max_model_len", "context_length", "max_context_length"):
                    v = chosen.get(k)
                    if isinstance(v, int) and v > 0:
                        max_context = v
                        break
                if max_context == 0 and isinstance(meta.get("n_ctx"), int) and meta["n_ctx"] > 0:
                    max_context = meta["n_ctx"]
                if max_context == 0:
                    st = chosen.get("status")
                    args = st.get("args") if isinstance(st, dict) else None
                    if isinstance(args, list):
                        for i, arg in enumerate(args[:-1]):
                            if arg in ("--ctx-size", "-c"):
                                try:
                                    val = int(args[i + 1])
                                    if val > 0:
                                        max_context = val
                                        break
                                except (TypeError, ValueError):
                                    pass
    except Exception:
        pass

if override_model.strip():
    model = override_model.strip()

if not model:
    sys.exit(1)

print(f"{model}\t{max_context}")
PY
) || {
  echo "FAIL: neither ${ROOT_URL}/health nor ${BASE_TRIMMED}/models reported a loaded model" >&2
  exit 1
}
IFS=$'\t' read -r STRATA_MODEL STRATA_MAX_CONTEXT <<< "$HEALTH_PARSED"

SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/gg-eval.XXXXXX")
trap 'rm -rf "$SCRATCH"' EXIT

HOME_DIR="$SCRATCH/home"
mkdir -p "$HOME_DIR/.gg"
{
  echo 'provider = "strata"'
  echo ''
  echo '[providers.strata]'
  echo "base_url = \"$BASE_TRIMMED\""
  if [[ -n "$EVAL_MODEL" ]]; then
    echo "model = \"$EVAL_MODEL\""
  fi
} > "$HOME_DIR/.gg/config.toml"

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

# --- Task 8: replace_all refactor across two modules ---
T8="$SCRATCH/t8"
mkdir -p "$T8/src"
cat > "$T8/Cargo.toml" <<'EOF'
[package]
name = "eval_rename"
version = "0.1.0"
edition = "2024"
EOF
cat > "$T8/src/lib.rs" <<'EOF'
//! Crate whose legacy helper is called from this module and from `extra`.
#![allow(dead_code)]

pub mod extra;

pub fn legacy_compute(x: i32) -> i32 {
    x * 2 + 1
}

fn wrap_a(x: i32) -> i32 {
    legacy_compute(x) + 1
}

fn wrap_b(x: i32) -> i32 {
    legacy_compute(wrap_a(x))
}

fn wrap_c(x: i32) -> i32 {
    legacy_compute(x) * 2
}

fn wrap_d(x: i32) -> i32 {
    legacy_compute(x) - 3
}

#[cfg(test)]
mod tests {
    use super::{extra, legacy_compute, wrap_b, wrap_c, wrap_d};

    #[test]
    fn legacy_compute_doubles_and_increments() {
        assert_eq!(legacy_compute(3), 7);
    }

    #[test]
    fn wrappers_compose() {
        assert_eq!(wrap_b(1), 9);
        assert_eq!(wrap_c(2), 10);
        assert_eq!(wrap_d(4), 6);
    }

    #[test]
    fn extra_module_shares_the_helper() {
        assert_eq!(extra::extra_total(2), 35);
    }
}
EOF
cat > "$T8/src/extra.rs" <<'EOF'
//! Second module that imports the same crate-level helper.
#![allow(dead_code)]

use crate::legacy_compute;

fn extra_a(x: i32) -> i32 {
    legacy_compute(x)
}

fn extra_b(x: i32) -> i32 {
    legacy_compute(x + 1)
}

fn extra_c(x: i32) -> i32 {
    legacy_compute(legacy_compute(x))
}

fn extra_d(x: i32) -> i32 {
    legacy_compute(x) + legacy_compute(x + 1)
}

pub fn extra_total(x: i32) -> i32 {
    extra_a(x) + extra_b(x) + extra_c(x) + extra_d(x)
}

#[cfg(test)]
mod tests {
    use super::extra_total;

    #[test]
    fn extra_total_sums_the_helper_calls() {
        assert_eq!(extra_total(2), 35);
    }
}
EOF

# --- Task 9: locate one helper in a ~700-line file ---
T9="$SCRATCH/t9"
mkdir -p "$T9/src"
cat > "$T9/Cargo.toml" <<'EOF'
[package]
name = "eval_discount"
version = "0.1.0"
edition = "2024"
EOF
python3 - "$T9/src/lib.rs" <<'PY'
import sys

lines = [
    "//! Eval crate with a deliberately wrong discount helper.",
    "#![allow(dead_code)]",
    "",
]
for i in range(1, 161):
    lines.append(f"fn helper_{i:03d}(x: i32) -> i32 {{")
    lines.append("    x + 1")
    lines.append("}")
for i in range(14):
    lines.append("// padding comment for layout" if i else "// Discount helpers live below this block.")
lines.append("/// Compute a 15% discount (integer division).")
lines.append("pub fn calculate_discount(price: i32) -> i32 {")
lines.append("    price * 25 / 100")
lines.append("}")
for i in range(161, 211):
    lines.append(f"fn helper_{i:03d}(x: i32) -> i32 {{")
    lines.append("    x + 1")
    lines.append("}")
lines.extend([
    "",
    "#[cfg(test)]",
    "mod tests {",
    "    use super::calculate_discount;",
    "",
    "    #[test]",
    "    fn discount_is_fifteen_percent() {",
    "        assert_eq!(calculate_discount(200), 30);",
    "    }",
    "}",
])
with open(sys.argv[1], "w") as f:
    f.write("\n".join(lines) + "\n")
PY

# --- Task H1: multi-file fix (heldout) ---
H1="$SCRATCH/h1"
mkdir -p "$H1/src"
cat > "$H1/Cargo.toml" <<'EOF'
[package]
name = "eval_invoice"
version = "0.1.0"
edition = "2024"
EOF
cat > "$H1/src/lib.rs" <<'EOF'
pub mod calc;
pub mod format;

#[cfg(test)]
mod tests {
    use super::{calc, format};

    #[test]
    fn subtotal_is_qty_times_price() {
        assert_eq!(calc::subtotal(3, 20), 60);
    }

    #[test]
    fn format_total_uses_the_aud_prefix() {
        assert_eq!(format::format_total(60), "AUD 60");
    }
}
EOF
cat > "$H1/src/calc.rs" <<'EOF'
/// Compute the line subtotal for `qty` units at `price` each.
pub fn subtotal(qty: i32, price: i32) -> i32 {
    qty + price
}
EOF
cat > "$H1/src/format.rs" <<'EOF'
/// Format a total as an AUD string.
pub fn format_total(total: i32) -> String {
    format!("USD {total}")
}
EOF

# --- Task H2: log needle grep (heldout) ---
H2="$SCRATCH/h2"
mkdir -p "$H2/logs" "$H2/archive"
printf 'archive/\n' > "$H2/.gitignore"
cat > "$H2/logs/app.log" <<'EOF'
2026-11-25T10:00:00Z app boot ok
2026-11-25T10:00:01Z app request path=/health status=200
2026-11-25T10:00:02Z app request path=/items status=200
2026-11-25T10:00:03Z ERR-1001 upstream timeout
DETAIL=TIMEOUT_UPSTREAM
2026-11-25T10:00:04Z app request path=/orders status=200
2026-11-25T10:00:05Z app request path=/items status=200
EOF
cat > "$H2/logs/worker.log" <<'EOF'
2026-11-25T10:01:00Z worker job=42 start
2026-11-25T10:01:01Z worker job=42 attempt=1
2026-11-25T10:01:02Z worker job=42 attempt=2
2026-11-25T10:01:03Z ERR-7319 retry budget exhausted
DETAIL=RETRY_EXHAUSTED
2026-11-25T10:01:04Z worker job=42 stop
EOF
cat > "$H2/archive/old.log" <<'EOF'
2026-11-01T09:00:00Z ERR-7319 retry budget exhausted
DETAIL=STALE_ARCHIVE
EOF

# --- Task H3: skill header tag (heldout) ---
H3="$SCRATCH/h3"
mkdir -p "$H3/src" "$H3/.agents/skills/header-tag"
cat > "$H3/Cargo.toml" <<'EOF'
[package]
name = "eval_tagged"
version = "0.1.0"
edition = "2024"
EOF
cat > "$H3/src/lib.rs" <<'EOF'
//! Utility helpers for the eval_tagged crate.
#![allow(dead_code)]

pub mod util;

#[cfg(test)]
mod tests {
    use super::util::clamp;

    #[test]
    fn clamps_within_bounds() {
        assert_eq!(clamp(5, 0, 10), 5);
        assert_eq!(clamp(-1, 0, 10), 0);
        assert_eq!(clamp(11, 0, 10), 10);
    }
}
EOF
cat > "$H3/src/util.rs" <<'EOF'
//! Clamping helper used by the tests in `lib.rs`.
#![allow(dead_code)]

/// Clamp `value` to the inclusive range `[lo, hi]`.
pub fn clamp(value: i32, lo: i32, hi: i32) -> i32 {
    if value < lo {
        lo
    } else if value > hi {
        hi
    } else {
        value
    }
}
EOF
cat > "$H3/.agents/skills/header-tag/SKILL.md" <<'EOF'
---
name: header-tag
description: "Tag Rust source files with the required first-line header comment."
---

# Header tag

The first line of every `.rs` file under `src/` must be exactly:

```
//! Tag: GG-HELDOUT-42
```

- Replace any existing first line with this exact line.
- Keep the rest of the file unchanged.
EOF

python3 - "$ROOT" "$GG_BIN" "$HOME_DIR" "$EVAL_TIMEOUT" "$GG_ARGS" "$EVAL_OUT" "$SCRATCH" \
    "$T1" "$T2" "$T3" "$T4" "$T5" "$T6" "$T7" "$T8" "$T9" \
    "$H1" "$H2" "$H3" \
    "$STRATA_MODEL" "$STRATA_MAX_CONTEXT" "$EVAL_MAX_TURNS" "$BASE_TRIMMED" "$EVAL_LABEL" \
    "$EVAL_SESSIONS_DIR" "$EVAL_SUITE" <<'PY'
import json, os, re, shlex, subprocess, sys, time

# Variables a gg session's bash tool hands its commands (the nesting
# handshake). The eval runs gg with this script's flags and config
# defaults, so it drops them: started from inside a session, the runs
# would otherwise be clamped to that session's tools, hooks, model and
# turn budget, and measure something else.
NESTING_VARS = (
    "GG_NEST_DEPTH",
    "GG_PARENT_SESSION",
    "GG_PARENT_MODEL",
    "GG_TOOL_CAP",
    "GG_MAX_TURNS_CAP",
    "GG_MAX_SECONDS_CAP",
    "GG_NO_HOOKS",
)

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
    t8,
    t9,
    h1,
    h2,
    h3,
    model,
    max_context,
    eval_max_turns,
    base_url,
    eval_label,
    sessions_dir,
    eval_suite,
) = sys.argv[1:]
timeout_s = int(timeout_s)
max_context = int(max_context)
eval_max_turns = int(eval_max_turns)
extra_args = shlex.split(gg_args)

def rename_refactor_ok(cwd):
    """Cargo tests pass, and the rename landed in both modules."""
    if subprocess.run(
        ["cargo", "test", "--quiet"], cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    ).returncode != 0:
        return False
    for fname in ("lib.rs", "extra.rs"):
        try:
            with open(os.path.join(cwd, "src", fname)) as fh:
                text = fh.read()
        except OSError:
            return False
        if "legacy_compute" in text or "modern_compute" not in text:
            return False
    return True


def header_tag_ok(cwd):
    """Cargo tests pass and both source files open with the exact tag line."""
    if subprocess.run(
        ["cargo", "test", "--quiet"], cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    ).returncode != 0:
        return False
    for fname in ("lib.rs", "util.rs"):
        try:
            with open(os.path.join(cwd, "src", fname)) as fh:
                first = fh.read().splitlines()[0]
        except (OSError, IndexError):
            return False
        if first != "//! Tag: GG-HELDOUT-42":
            return False
    return True


core_tasks = [
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
    (
        "T8_replace_all_refactor",
        t8,
        "Rename the function `legacy_compute` to `modern_compute` across `src/lib.rs` and `src/extra.rs` (definition, imports, call sites and tests), then run `cargo test` to verify it passes.",
        lambda ans, cwd: rename_refactor_ok(cwd),
    ),
    (
        "T9_large_file_outline",
        t9,
        "A test in this Rust crate is failing because `calculate_discount` uses the wrong percentage (it should be 15%). Locate `calculate_discount`, fix it, and run `cargo test` to verify it passes.",
        lambda ans, cwd: subprocess.run(["cargo", "test", "--quiet"], cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0,
    ),
]

heldout_tasks = [
    (
        "H1_multi_file_fix",
        h1,
        "This Rust crate has two bugs: `calc::subtotal` uses the wrong formula and "
        "`format::format_total` returns the wrong currency prefix. "
        "Fix both so `cargo test` passes.",
        lambda ans, cwd: subprocess.run(["cargo", "test", "--quiet"], cwd=cwd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0,
    ),
    (
        "H2_log_needle_grep",
        h2,
        "The `logs/` directory contains log files. Find the `ERR-7319` entry in the "
        "non-ignored logs and return the `DETAIL=...` value on the line immediately "
        "following it. Reply with only that `DETAIL=...` value.",
        lambda ans, cwd: (
            "DETAIL=RETRY_EXHAUSTED" in ans
            and "STALE_ARCHIVE" not in ans
            and "archive/old.log" not in ans
        ),
    ),
    (
        "H3_skill_header_tag",
        h3,
        "Apply the repo skill `header-tag` to both files under `src/`, "
        "then run `cargo test` to verify the crate still passes.",
        lambda ans, cwd: header_tag_ok(cwd),
    ),
]

if eval_suite == "core":
    tasks = core_tasks
elif eval_suite == "heldout":
    tasks = heldout_tasks
else:
    tasks = core_tasks + heldout_tasks

results = []
for name, cwd, prompt, check_fn in tasks:
    env = dict(os.environ, HOME=home_dir)
    for var in NESTING_VARS:
        env.pop(var, None)
    session_dir = sessions_dir if sessions_dir else scratch_dir
    session_file = os.path.join(session_dir, f"{name}.jsonl")
    if os.path.exists(session_file):
        os.remove(session_file)
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
    cache_n = 0
    pred_n = 0
    pred_ms = 0.0
    prompt_n = 0
    prompt_ms = 0.0
    draft_n = 0
    draft_acc = 0
    pred_tps_samples = []
    prompt_tps_samples = []
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
            timings = ev.get("timings") or {}
            cache_n += int(timings.get("cache_n") or 0)
            pred_n += int(timings.get("predicted_n") or 0)
            pred_ms += float(timings.get("predicted_ms") or 0.0)
            prompt_n += int(timings.get("prompt_n") or 0)
            prompt_ms += float(timings.get("prompt_ms") or 0.0)
            draft_n += int(timings.get("draft_n") or 0)
            draft_acc += int(timings.get("draft_n_accepted") or 0)
            if timings.get("predicted_per_second"):
                pred_tps_samples.append(float(timings["predicted_per_second"]))
            if timings.get("prompt_per_second"):
                prompt_tps_samples.append(float(timings["prompt_per_second"]))
        elif et == "warned":
            message = ev.get("message", "")
            if "had no content" in message:
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
                # Count legitimate bash (cargo/git) as non-workaround; sed/cat/grep/rg/find/ls/perl/python as workaround
                workarounds = {"sed", "cat", "head", "tail", "grep", "rg", "find", "ls", "perl", "python", "python3"}
                is_workaround = False
                uses_python = False
                for seg in re.split(r"\s*(?:&&|\|\||[;\n])\s*", cstr):
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
                    tool_bytes += len((m.get("content") or "").encode("utf-8"))
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
    outline_calls = tools.get("outline", 0)
    cache_hit_pct = round(100.0 * cache_n / sum_prompt, 1) if sum_prompt else 0.0
    if pred_n > 0 and pred_ms > 0:
        dec_tps = round(pred_n / (pred_ms / 1000.0), 1)
    elif pred_tps_samples:
        dec_tps = round(sum(pred_tps_samples) / len(pred_tps_samples), 1)
    else:
        dec_tps = 0.0
    if prompt_n > 0 and prompt_ms > 0:
        pre_tps = round(prompt_n / (prompt_ms / 1000.0), 1)
    elif prompt_tps_samples:
        pre_tps = round(sum(prompt_tps_samples) / len(prompt_tps_samples), 1)
    else:
        pre_tps = 0.0
    draft_acc_pct = round(100.0 * draft_acc / draft_n, 1) if draft_n > 0 else None
    if reasoning_tok > 0 and gen_tok > 0:
        think_pct = min(100.0, 100.0 * reasoning_tok / gen_tok)
    elif reason_chars + out_chars:
        think_pct = 100.0 * reason_chars / (reason_chars + out_chars)
    else:
        think_pct = 0.0
    result = {
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
        "outline_calls": outline_calls,
        "cache_n": cache_n,
        "cache_hit_pct": cache_hit_pct,
        "pred_n": pred_n,
        "pred_ms": round(pred_ms, 1),
        "dec_tps": dec_tps,
        "prompt_n": prompt_n,
        "prompt_ms": round(prompt_ms, 1),
        "pre_tps": pre_tps,
        "draft_n": draft_n,
        "draft_acc": draft_acc,
        "draft_acc_pct": draft_acc_pct,
        "tools": tools,
    }
    if sessions_dir:
        result["session_file"] = session_file
    results.append(result)

label_part = f" | Label: {eval_label}" if eval_label else ""
print(f"Suite: {eval_suite} | Model: {model} (max_context={max_context}){label_part} | Binary: {gg_bin}")
print(
    f"{'Task':<24} {'Status':<6} {'Wall':>7} {'Turns':>5} {'Calls':>5} {'Batch':>5} "
    f"{'Native%':>7} {'Py':>4} {'Cd':>3} {'ToolKB':>7} {'PeakCtx':>8} {'SumCtx':>8} "
    f"{'GenTok':>7} {'Think%':>7} {'Cut':>4} {'Outl':>4} {'Cache%':>7} {'DecT/s':>7} {'Draft%':>7}"
)
print("-" * 150)
for r in results:
    st = "PASS" if r["pass"] else ("TIME" if r["timed_out"] else "FAIL")
    draft_str = f"{r['draft_acc_pct']:>6.1f}%" if r["draft_acc_pct"] is not None else f"{'-':>7}"
    print(
        f"{r['task']:<24} {st:<6} {r['wall_s']:>6.1f}s {r['turns']:>5} {r['calls']:>5} "
        f"{r['batch']:>5.2f} {r['native_pct']:>6.1f}% {r['py_calls']:>4} "
        f"{r['redundant_cd']:>3} {r['tool_bytes'] / 1024:>7.1f} {r['peak_prompt']:>8,} "
        f"{r['sum_prompt']:>8,} {r['gen_tok']:>7,} {r['think_pct']:>6.1f}% {r['empty_trunc']:>4} "
        f"{r['outline_calls']:>4} {r['cache_hit_pct']:>6.1f}% {r['dec_tps']:>7.1f} {draft_str}"
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
tot_outline = sum(r["outline_calls"] for r in results)
tot_cache_n = sum(r["cache_n"] for r in results)
tot_pred_n = sum(r["pred_n"] for r in results)
tot_pred_ms = sum(r["pred_ms"] for r in results)
tot_prompt_n = sum(r["prompt_n"] for r in results)
tot_prompt_ms = sum(r["prompt_ms"] for r in results)
tot_draft_n = sum(r["draft_n"] for r in results)
tot_draft_acc = sum(r["draft_acc"] for r in results)
overall_cache_hit_pct = (
    round(100.0 * tot_cache_n / tot_sum_prompt, 1) if tot_sum_prompt else 0.0
)
overall_dec_tps = round(tot_pred_n / (tot_pred_ms / 1000.0), 1) if tot_pred_n > 0 and tot_pred_ms > 0 else 0.0
overall_pre_tps = round(tot_prompt_n / (tot_prompt_ms / 1000.0), 1) if tot_prompt_n > 0 and tot_prompt_ms > 0 else 0.0
overall_draft_acc_pct = round(100.0 * tot_draft_acc / tot_draft_n, 1) if tot_draft_n > 0 else None
tot_draft_str = f"{overall_draft_acc_pct:>6.1f}%" if overall_draft_acc_pct is not None else f"{'-':>7}"
passed = sum(1 for r in results if r["pass"])
print("-" * 150)
print(
    f"{'TOTAL':<24} {f'{passed}/{len(results)}':<6} {tot_wall:>6.1f}s {tot_turns:>5} {tot_calls:>5} "
    f"{'':>5} {'':>7} {tot_py:>4} {tot_cd:>3} {tot_tool_bytes / 1024:>7.1f} "
    f"{max(r['peak_prompt'] for r in results):>8,} {tot_sum_prompt:>8,} {tot_gen:>7,} {'':>7} {tot_cut:>4} "
    f"{tot_outline:>4} {overall_cache_hit_pct:>6.1f}% {overall_dec_tps:>7.1f} {tot_draft_str}"
)

if eval_out:
    rev = subprocess.check_output(["git", "-C", root, "rev-parse", "--short", "HEAD"], text=True).strip()
    rec = {
        "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "rev": rev,
        "label": eval_label or None,
        "base_url": base_url,
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
        "total_outline_calls": tot_outline,
        "overall_cache_hit_pct": overall_cache_hit_pct,
        "total_pred_n": tot_pred_n,
        "total_pred_ms": round(tot_pred_ms, 1),
        "overall_dec_tps": overall_dec_tps,
        "total_prompt_n": tot_prompt_n,
        "total_prompt_ms": round(tot_prompt_ms, 1),
        "overall_pre_tps": overall_pre_tps,
        "total_draft_n": tot_draft_n,
        "total_draft_acc": tot_draft_acc,
        "overall_draft_acc_pct": overall_draft_acc_pct,
        "results": results,
        "suite": eval_suite,
    }
    with open(eval_out, "a") as f:
        f.write(json.dumps(rec) + "\n")

sys.exit(0 if passed == len(results) else 1)
PY
