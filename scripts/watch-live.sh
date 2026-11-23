#!/usr/bin/env bash
# Live check for `gg --watch` (development hot restart), driven through a
# private tmux server so the user's server options are never touched.
#
#   scripts/watch-live.sh
#
# Three things are asserted, with no model calls and no Strata:
#   1. `--watch` boots, materializes the session file, and shows the
#      profile in the footer.
#   2. Touching a source file rebuilds and re-execs the NEW binary, and the
#      same session is resumed (the session id in argv and the session line
#      are unchanged, and the process's executable inode changed).
#   3. With text in the composer, a rebuild holds at `rebuild ready`;
#      Ctrl+R restarts anyway and the text is back in the composer.
#
# Needs `cargo build` first (the script builds it if the binary is missing).
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." >/dev/null 2>&1 && pwd)
BIN="$ROOT/target/debug/gg"
SOCK="gg-watch-$$"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/gg-watch.XXXXXX")
LOG="$TMP/stderr.log"
FAILED=0

cleanup() {
    tmux -L "$SOCK" kill-server 2>/dev/null || true
    if [ "$FAILED" -ne 0 ]; then
        echo "--- stderr ---" >&2
        cat "$LOG" >&2 2>/dev/null || true
        echo "--- artifacts kept: $TMP ---" >&2
    else
        rm -rf "$TMP"
    fi
}
trap cleanup EXIT

fail() {
    FAILED=1
    echo "FAIL: $*" >&2
    exit 1
}

pane() { tmux -L "$SOCK" capture-pane -p -t w 2>/dev/null || true; }
restarts() { grep -c 'gg: restarting' "$LOG" 2>/dev/null || true; }

wait_for() { # wait_for <seconds> <description> <shell test...>
    local limit=$1 what=$2
    shift 2
    local i
    for i in $(seq 1 "$limit"); do
        if "$@" >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    fail "timed out waiting for $what"
}

has_watch_footer() { pane | grep -q 'watch debug · watching'; }
has_ready_footer() { pane | grep -q 'rebuild ready · Ctrl+R'; }
has_draft() { pane | grep -q 'DRAFT-OK'; }
restarted_once() { [ "$(restarts)" -ge 1 ]; }
restarted_twice() { [ "$(restarts)" -ge 2 ]; }

[ -x "$BIN" ] || cargo build --manifest-path "$ROOT/Cargo.toml"

mkdir -p "$TMP/home"
# rustup still needs the real toolchain homes; only gg's state moves.
KEEP_RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}
KEEP_CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
tmux -L "$SOCK" new-session -d -s w -x 110 -y 32 \
    "cd '$ROOT' && HOME='$TMP/home' RUSTUP_HOME='$KEEP_RUSTUP_HOME' CARGO_HOME='$KEEP_CARGO_HOME' '$BIN' --watch 2>'$LOG'"

wait_for 20 "the watch footer" has_watch_footer
session_file=$(find "$TMP/home" -name '*.jsonl' | head -1)
[ -n "$session_file" ] || fail "no session file was materialized for --watch"
grep -q "session: $session_file" "$LOG" || fail "startup did not report $session_file"

# --- 2. automatic restart on a source edit -------------------------------
pid=$(tmux -L "$SOCK" list-panes -t w -F '#{pane_pid}')
child=$(ps -o pid= --ppid "$pid" | head -1 | tr -d ' ')
[ -n "$child" ] || child=$pid
before=$(stat -Lc %i "/proc/$child/exe")
touch "$ROOT/src/main.rs"
wait_for 120 "a rebuild and restart" restarted_once
grep -q "gg: restarting $BIN" "$LOG" || fail "restart did not exec $BIN"
after=$(stat -Lc %i "/proc/$child/exe" 2>/dev/null || echo gone)
[ "$before" != "$after" ] || fail "process still runs the old executable inode"
wait_for 10 "the watch footer after restart" has_watch_footer
[ "$(find "$TMP/home" -name '*.jsonl' | wc -l)" -eq 1 ] || fail "restart created a new session file"
[ "$(grep -c "session: $session_file" "$LOG")" -ge 2 ] || fail "restart did not resume $session_file"

# --- 3. a draft holds the restart; Ctrl+R carries it across --------------
tmux -L "$SOCK" send-keys -t w -l 'DRAFT-OK'
sleep 1
touch "$ROOT/src/main.rs"
wait_for 120 "the held restart" has_ready_footer
[ "$(restarts)" -eq 1 ] || fail "auto-restarted with unsent text in the composer"
tmux -L "$SOCK" send-keys -t w C-r
wait_for 120 "the Ctrl+R restart" restarted_twice
wait_for 10 "the draft restored in the composer" has_draft
[ ! -e "${session_file%.jsonl}.watch-draft" ] || fail "the draft file was not consumed"

echo "PASS: watch boot, automatic restart, session continuity, draft hold + carry"
echo "      binary: $BIN"
echo "      session: $session_file"
