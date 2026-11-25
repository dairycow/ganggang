#!/usr/bin/env bash
# gg pre_tool hook: run the repository's pre-commit gate before git commits.
# Reads one hook JSON object on stdin. Prints a deny decision when the command
# is a commit that bypasses hooks or that fails the gate; prints nothing
# otherwise (allow). Requires jq and git 2.36 or newer.

input=$(cat 2>/dev/null || true)

if ! command -v jq >/dev/null 2>&1; then
  echo "gg-commit-gate: jq is not installed; skipping the gate" >&2
  exit 0
fi

cmd=$(printf '%s' "$input" | jq -r '.tool.args.command // empty' 2>/dev/null) || {
  echo "gg-commit-gate: malformed hook input; skipping the gate" >&2
  exit 0
}
[ -n "$cmd" ] || exit 0

cwd=$(printf '%s' "$input" | jq -r '.cwd // empty')
[ -n "$cwd" ] || cwd=$(pwd)

# Act only on a real commit command, not on quoted or embedded mentions.
if ! grep -Eq '(^|[;&|(]|\s)git(\s+-C\s+\S+)?\s+commit(\s|$)' <<<"$cmd"; then
  exit 0
fi

# Deny attempts to bypass the hooks.
if grep -Eq -- '(^|\s)--no-verify(\s|$)|(^|\s)git(\s+-C\s+\S+)?\s+commit\s+-n(\s|$)' <<<"$cmd"; then
  echo '{"decision":"deny","reason":"commit hooks may not be bypassed (--no-verify)"}'
  exit 0
fi

cd "$cwd" 2>/dev/null || exit 0
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || exit 0

output=$(git hook run --ignore-missing pre-commit 2>&1) || {
  last=$(printf '%s\n' "$output" | tail -n 40)
  jq -n --arg reason "pre-commit gate failed:
$last" '{"decision":"deny","reason":$reason}'
  exit 0
}
exit 0
