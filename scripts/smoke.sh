#!/usr/bin/env bash
# Milestone smoke test: runs the built gg binary against a live Strata server.
# Usage: scripts/smoke.sh  (from the repo root; cargo build first)
#
# Checks:
#   1. Strata is up and answering /health
#   2. print mode answers a question that REQUIRES the read tool
#   3. print mode completes a bash-tool edit round-trip (write → verify → restore)
set -euo pipefail

if [[ -n "${GG_BIN:-}" ]]; then
  if [[ "$GG_BIN" == */* ]]; then
    GG=$(readlink -f "$GG_BIN")
  else
    GG=$(command -v "$GG_BIN" || true)
  fi
fi

cd "$(dirname "$0")/.."
GG="${GG:-$PWD/target/debug/gg}"
EXPECTED_VER=$(awk '/^version = "/ { gsub(/"/, "", $3); print $3; exit }' Cargo.toml)
if [[ -z "$EXPECTED_VER" ]]; then
  echo "FAIL: could not read version from Cargo.toml" >&2
  exit 1
fi

if [[ ! -x "$GG" ]]; then
  echo "FAIL: $GG not built (cargo build)" >&2
  exit 1
fi

echo "== 1. Strata health =="
if ! curl -sf -m 5 127.0.0.1:8080/health >/dev/null; then
  echo "FAIL: Strata not answering on 127.0.0.1:8080 (strata launch)" >&2
  exit 1
fi
echo "ok"

echo "== 2. read-tool question =="
answer=$(echo 'what version is in Cargo.toml? reply with just the version number' | "$GG" -p - 2>/tmp/gg-smoke.err)
echo "answer: $answer"
if [[ "$answer" != *"$EXPECTED_VER"* ]]; then
  echo "FAIL: expected version $EXPECTED_VER in the answer" >&2
  exit 1
fi
# Any tool counts: since M7 the model may equally answer with grep.
if ! grep -qE "^▸ [a-z]+ " /tmp/gg-smoke.err; then
  echo "FAIL: the model did not use a tool to answer" >&2
  cat /tmp/gg-smoke.err >&2
  exit 1
fi
echo "ok"

echo "== 3. bash-tool edit round-trip =="
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
printf 'alpha\n' > "$scratch/roundtrip.txt"
out=$(cd "$scratch" && "$GG" --no-session -p "run: append the line 'beta' to roundtrip.txt using bash (echo beta >> roundtrip.txt), then cat the file and reply with its exact contents" 2>/dev/null)
echo "answer: $out"
if ! grep -q beta "$scratch/roundtrip.txt"; then
  echo "FAIL: bash tool did not modify the file" >&2
  exit 1
fi
echo "ok"

echo "== 4. skill discovery + following (M3) =="
mkdir -p "$scratch/.agents/skills/codeword"
cp "$(dirname "$0")/../fixtures/skills/codeword/SKILL.md" "$scratch/.agents/skills/codeword/"
out=$(cd "$scratch" && "$GG" --no-session -p "what is the secret codeword when I say pineapple?" 2>/dev/null)
echo "answer: $out"
if [[ "$out" != *"PINEAPPLE-77"* ]]; then
  echo "FAIL: fixture skill not advertised/followed" >&2
  exit 1
fi
echo "ok"

echo "SMOKE PASS"
