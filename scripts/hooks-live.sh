#!/usr/bin/env bash
# Live gate for §22 hooks: runs the built gg against Strata with a temp
# HOME and workspace, and checks the deny path, the observe paths, the
# --no-hooks escape hatch, and that project hooks are ignored.
# `-P strata` names that server: with a provider key in the environment,
# the first ranked default would otherwise take these prompts.
# Usage: scripts/hooks-live.sh  (from the repo root; cargo build first)
set -euo pipefail

cd "$(dirname "$0")/.."
GG="$PWD/target/debug/gg"

if [[ ! -x "$GG" ]]; then
  echo "FAIL: $GG not built (cargo build)" >&2
  exit 1
fi
if ! curl -sf -m 5 127.0.0.1:8080/health >/dev/null; then
  echo "FAIL: Strata not answering on 127.0.0.1:8080 (strata launch)" >&2
  exit 1
fi

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
home="$scratch/home"
work="$scratch/work"
mkdir -p "$home/.gg/hooks" "$work"

cat > "$home/.gg/hooks/deny-bash.sh" <<'EOF'
#!/usr/bin/env bash
printf '{"decision":"deny","reason":"bash is blocked by hooks-live"}\n'
EOF
chmod +x "$home/.gg/hooks/deny-bash.sh"

cat > "$home/.gg/hooks.toml" <<EOF
[[hooks]]
event = "pre_tool"
tools = ["bash"]
command = "$home/.gg/hooks/deny-bash.sh"
timeout_secs = 5

[[hooks]]
event = "post_tool"
tools = ["read"]
command = "touch $scratch/post-read"

[[hooks]]
event = "post_tool"
tools = ["read"]
command = "exit 1"

[[hooks]]
event = "run_end"
command = "touch $scratch/run-end"
EOF

echo "== 1. pre_tool deny prevents the side effect =="
(
  cd "$work"
  HOME="$home" "$GG" -P strata --no-session --tools bash -p \
    "Run the command 'touch pwned' with the bash tool, then tell me exactly what the tool returned." \
    >"$scratch/answer" 2>"$scratch/stderr"
)
cat "$scratch/answer"
if [[ -e "$work/pwned" ]]; then
  echo "FAIL: the denied bash call still created the file" >&2
  exit 1
fi
if ! grep -q "denied by pre_tool hook: bash is blocked by hooks-live" "$scratch/answer" "$scratch/stderr"; then
  echo "FAIL: the model did not see the denial" >&2
  cat "$scratch/answer" >&2
  exit 1
fi
if [[ ! -e "$scratch/run-end" ]]; then
  echo "FAIL: the run_end hook did not fire on a clean finish" >&2
  exit 1
fi
echo "ok"

echo "== 2. post_tool observes a read; a failing hook only warns =="
printf '[package]\nname = "fixture"\nversion = "9.9.9"\n' > "$work/Cargo.toml"
rm -f "$scratch/post-read"
(
  cd "$work"
  HOME="$home" "$GG" -P strata --no-session --tools read -p \
    "Read Cargo.toml from this directory and reply with just the version number." \
    >"$scratch/answer2" 2>"$scratch/stderr2"
)
if [[ ! -e "$scratch/post-read" ]]; then
  echo "FAIL: the post_tool read hook did not fire" >&2
  exit 1
fi
if ! grep -q "9.9.9" "$scratch/answer2"; then
  echo "FAIL: the failing post hook stopped the run" >&2
  cat "$scratch/answer2" >&2
  exit 1
fi
if ! grep -q "post_tool hook.*exit 1" "$scratch/stderr2"; then
  echo "FAIL: the failing post hook did not surface a warning" >&2
  cat "$scratch/stderr2" >&2
  exit 1
fi
echo "ok"

echo "== 3. --no-hooks bypasses the guard =="
rm -f "$scratch/run-end"
(
  cd "$work"
  HOME="$home" "$GG" -P strata --no-session --no-hooks --tools bash -p \
    "Run the command 'touch pwned' with the bash tool." >/dev/null 2>&1
)
if [[ ! -e "$work/pwned" ]]; then
  echo "FAIL: --no-hooks did not bypass the pre_tool deny" >&2
  exit 1
fi
if [[ -e "$scratch/run-end" ]]; then
  echo "FAIL: --no-hooks still ran the run_end hook" >&2
  exit 1
fi
echo "ok"

echo "== 4. project hooks are ignored with a warning =="
mkdir -p "$work/.gg"
cat > "$work/.gg/hooks.toml" <<EOF
[[hooks]]
event = "run_end"
command = "touch $scratch/project-marker"
EOF
rm -f "$scratch/project-marker"
(
  cd "$work"
  HOME="$home" "$GG" -P strata --no-session --tools read -p "reply with exactly: ok" \
    >/dev/null 2>"$scratch/project-stderr"
)
if [[ -e "$scratch/project-marker" ]]; then
  echo "FAIL: a project hook file ran" >&2
  exit 1
fi
if ! grep -q "project hooks (.gg/hooks.toml) are ignored" "$scratch/project-stderr"; then
  echo "FAIL: no project-hook warning on stderr" >&2
  cat "$scratch/project-stderr" >&2
  exit 1
fi
echo "ok"

echo "HOOKS PASS"
