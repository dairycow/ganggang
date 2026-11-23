#!/usr/bin/env bash
# Live test of the TUI's terminal hand-off: Ctrl+G and the external editor,
# and what the pane's terminal looks like once gg has quit. A complement to
# smoke.sh (print mode): the loop that lends the terminal and takes it back
# has no unit test — it needs a real tty.
#
# Usage: scripts/tui-live.sh    (from the repo root; cargo build --release first)
# Needs: tmux, bash, stty, a running Strata (as smoke.sh does); nvim for
# four cases, run without its config.
#
# Every case gets a tmux server of its own, on a socket under this script's
# temp directory — never the user's server. tmux options are server-wide
# (tmux 3.8: even `set-window-option -t <window>` changes them for every
# session), and one case here turns `extended-keys` on.
set -u

GG=$(readlink -f "${GG:-$PWD/target/release/gg}")   # like smoke.sh: run from the repo root. Canonical: gg_pid() compares /proc/<pid>/exe with it as a string, and a `..` in the path matches nothing.
WORK=$(mktemp -d)
TAG="ggx-live-$RANDOM$RANDOM"
S="$TAG"
SOCK=""                            # this case's tmux server, set by start()
SERVERS=""
FAKE="$WORK/fake-editor.sh"
NVIM="nvim -u NONE -n"             # no config, no swap file: nothing left in the user's state
EXT_KEYS=off                       # the case's tmux server: off as tmux ships it, on for one case
printf 'set -g default-terminal "tmux-256color"\n' > "$WORK/tmux.conf"

# Pane modes tmux keeps for the pane's program: alternate screen, mouse
# reporting (standard / button / UTF-8), keypad and cursor-key application,
# key encoding (VT10x = legacy), synchronized output, autowrap, origin.
MODES='alt=#{alternate_on} mouse=#{mouse_standard_flag}/#{mouse_button_flag}/#{mouse_utf8_flag} keypad=#{keypad_flag}/#{keypad_cursor_flag} keymode=#{pane_key_mode} sync=#{synchronized_output_flag} wrap=#{wrap_flag} origin=#{origin_flag}'
OFF="mouse=0/0/0 keypad=0/0 keymode=VT10x sync=0 wrap=1 origin=0"
WANT_MODES="alt=0 $OFF"            # once gg has quit
WANT_RUNNING="alt=1 $OFF"          # while gg runs: on the alternate screen, nothing else on
pass=0; fail=0; skipped=0; before_drafts=""; first_drafts=""; CASE_GGPID=""

cleanup() {
  # Only ever a server started here, by its socket under $WORK.
  for sock in $SERVERS; do
    case "$sock" in
      "$WORK"/*) tmux -S "$sock" -f /dev/null kill-server 2>/dev/null ;;
    esac
  done
  rm -rf "$WORK"
}
# A signalled run does its own cleaning: a bash killed by TERM has been
# seen to skip the EXIT trap. What the in-flight case's gg kept on disk —
# a draft with typing in it — stays, as gg's own crash rule keeps it.
trap 'cleanup; exit 1' HUP INT TERM PIPE
trap cleanup EXIT

# ---------------------------------------------------------------- harness --
ok()  { ((pass++)); echo "  PASS $1: $2"; }
bad() { ((fail++)); echo "  FAIL $1: $2"; [[ -z "${3:-}" ]] || echo "$3" | tail -8 | sed 's/^/       | /'; }
t() { tmux -S "$SOCK" -f "$WORK/tmux.conf" "$@"; }   # the case's own server, with this script's config only
keys() { t send-keys -t "$S" "$@"; }
pane() { t capture-pane -p -t "$S"; }
show() { pane | grep -v '^ *$' | tail -8 | sed 's/^/       | /'; }
wait_for() { # until the pane shows $1 (up to $2 s)
  local deadline=$((SECONDS + $2))
  while ((SECONDS < deadline)); do
    if pane | grep -qF -- "$1"; then return 0; fi
    sleep 0.2
  done
  return 1
}
wait_nvim() { # nvim's empty-buffer tildes: the editor has the screen
  local deadline=$((SECONDS + 20))
  while ((SECONDS < deadline)); do
    if pane | grep -q '^~'; then sleep 1; return 0; fi   # and a moment to finish starting
    sleep 0.2
  done
  return 1
}
composer() { pane | tail -5 | head -4 | sed 's/ *$//'; }   # the editor box's text rows
line() { composer | head -1; }
draft_dirs() { find "${TMPDIR:-/tmp}" -maxdepth 1 -name 'gg-draft-*' -type d 2>/dev/null | sort; }
draft_files() { find "${TMPDIR:-/tmp}" -maxdepth 2 -path '*/gg-draft-*/*' 2>/dev/null | sort; }
wait_log() { # until the fake editor's log has a line starting with $1
  local deadline=$((SECONDS + 15))
  while ((SECONDS < deadline)); do
    if grep -q "^$1" "$WORK/fake.log" 2>/dev/null; then return 0; fi
    sleep 0.1
  done
  return 1
}
check_modes() { # check_modes <label> — while gg runs: no mode a crashed editor switched on is still on
  local now want; now=$(t display-message -p -t "$S" "$MODES")
  # With extended keys on, gg's own request puts the pane in tmux's
  # mode 1 (the keys it needs are reported as CSI-u); off, the request
  # is ignored and the pane stays legacy.
  want="$WANT_RUNNING"
  [[ "$EXT_KEYS" != off ]] && want="${want/keymode=VT10x/keymode=Ext 1}"
  [[ "$now" == "$want" ]] && ok "$1" "no mode the dead editor switched on is still on ($now)" || bad "$1" "terminal modes left on" "$now"
}
gg_pid() { # this session's gg — a match on the binary alone would be any gg of it
  local frontier next p c _
  frontier=$(t display-message -p -t "$S" '#{pane_pid}' 2>/dev/null) || return 0
  for _ in 1 2 3; do
    next=""
    for p in $frontier; do
      [[ "$(readlink "/proc/$p/exe" 2>/dev/null)" == "$GG" ]] && { echo "$p"; return 0; }
      for c in $(pgrep -P "$p"); do next="$next $c"; done
    done
    frontier=$next
  done
}

# What the pane's terminal looks like once gg has quit. This is where an
# editor that never put the terminal back would show: gg has to leave the
# shell a cooked tty with no modes left on, whatever the editor did.
quit_and_check() { # quit_and_check <label>
  local label=$1 deadline
  keys C-c
  deadline=$((SECONDS + 10))
  while ((SECONDS < deadline)); do grep -q '^gg-exit=' "$WORK/gg-exit.log" 2>/dev/null && break; sleep 0.1; done
  if grep -q '^gg-exit=0$' "$WORK/gg-exit.log" 2>/dev/null; then ok "$label.1" "gg exited cleanly"
  else bad "$label.1" "gg exit" "$(cat "$WORK/gg-exit.log" 2>&1)"; fi
  local stty_flags
  stty_flags=$(tr -s ' \n' '  ' < "$WORK/stty-after.txt" 2>/dev/null)
  if [[ "$stty_flags" == *" isig "* && "$stty_flags" != *"-isig"* && "$stty_flags" == *" icanon "* &&
        "$stty_flags" == *" echo "* && "$stty_flags" == *" opost "* && "$stty_flags" == *" icrnl "* ]]; then
    ok "$label.2" "the shell gets a cooked tty back"
  else bad "$label.2" "the tty was left raw" "$stty_flags"; fi
  local now; now=$(t display-message -p -t "$S" "$MODES")
  [[ "$now" == "$WANT_MODES" ]] && ok "$label.3" "no alternate screen, mouse reporting or keypad mode left on ($now)" || bad "$label.3" "terminal modes left on" "$now"
  t kill-session -t "$S" 2>/dev/null
  # gg was signalled rather than quit (it did not exit on Ctrl+C): its
  # draft directory is this case's to remove, if empty.
  [[ -n "$CASE_GGPID" ]] && rmdir "${TMPDIR:-/tmp}/gg-draft-$CASE_GGPID" 2>/dev/null
}

start() { # start <EDITOR value> <FAKE_MODE> [FAKE_SLEEP]
  SOCK="$WORK/tmux-$RANDOM$RANDOM.sock"
  SERVERS="$SERVERS $SOCK"
  rm -rf "$WORK/scratch" "$WORK/fake.log" "$WORK/gg-exit.log" "$WORK/stty-after.txt"
  mkdir -p "$WORK/scratch"
  before_drafts=$(draft_files)
  # A placeholder session keeps the server alive while the options are
  # set: gg must see `extended-keys` before it starts, because it asks
  # for modified keys on entering (a server with no sessions exits).
  t new-session -d -s boot "sleep 30"
  t set -g extended-keys "$EXT_KEYS"
  t new-session -d -s "$S" -x 150 -y 40 "bash -c '$WORK/launch.sh $WORK $GG \"$1\" ${2:-ok} ${3:-}'"
  t set-window-option -t "$S" extended-keys "$EXT_KEYS"
  t kill-session -t boot
  wait_for "○ ready" 15 || { echo "gg did not start:"; show; exit 1; }
  CASE_GGPID=$(gg_pid)
}
no_leak() { # no_leak <label>  (while gg runs: nothing left from an earlier session)
  local now; now=$(draft_files)
  [[ "$now" == "$before_drafts" ]] && ok "$1" "no draft file left behind" || bad "$1" "draft files leaked:" "$now"
}
stderr_clean() { if [[ -s "$WORK/gg-err.log" ]]; then bad "$1" "stderr:" "$WORK/gg-err.log"; else ok "$1" "nothing on stderr"; fi; }
kept_dir() { # the file a kept draft was named as
  KEPT_FILE=$(pane | grep -o '/[^ ]*gg-draft-[^ ]*' | tail -1)
}
kept_survives() { # kept_survives <label> — once gg has quit: the draft is still there
  [[ -f "$KEPT_FILE" ]] && ok "$1" "the kept draft outlives gg" || bad "$1" "kept draft" "$KEPT_FILE"
  rm -f "$KEPT_FILE"; rmdir "${KEPT_FILE%/*}" 2>/dev/null
}
finish() { quit_and_check "$1"; stderr_clean "$1.4"; }

cat > "$FAKE" <<'EDITOR'
#!/usr/bin/env bash
# Fake editor for the live test. $1 is the draft; $FAKE_MODE picks what it does.
# Everything it observes is appended to $FAKE_LOG.
log() { printf '%s\n' "$*" >> "$FAKE_LOG"; }
log "start mode=${FAKE_MODE:-ok}"
log "draft=[$(cat -- "$1" 2>/dev/null)]"
stty -a < /dev/tty >> "$FAKE_LOG" 2>&1
case "${FAKE_MODE:-ok}" in
  ok)      IFS= read -r -t 5 -N 1 key; log "key=[$key]"; printf ' + edited\n' >> "$1" ;;
  fail)    printf 'typed then failed\n' >> "$1"; exit 3 ;;
  cancel)  exit 1 ;;
  slow)    printf 'draft while busy' > "$1"; sleep "${FAKE_SLEEP:-8}" ;;
  instant) exit 0 ;;
  killed)  printf 'typed before the kill\n' > "$1"; kill -9 "$$" ;;
  rawcrash) # a full-screen editor that dies: raw tty, a frame on screen, cursor parked
            stty raw -echo < /dev/tty
            printf '\033[2J\033[H\033[7m %s \033[0m\033[12;60H' "$(printf 'EDITOR FRAME %.0s' $(seq 10))" > /dev/tty
            printf 'typed before the crash\n' > "$1"; kill -9 "$$" ;;
  *)       exit 9 ;;
esac
EDITOR
cat > "$WORK/launch.sh" <<'LAUNCH'
#!/usr/bin/env bash
# launch.sh <work dir> <gg binary> <EDITOR value> <FAKE_MODE> [FAKE_SLEEP]
# gg in the scratch dir, then — once it has quit — a record of what the tty
# was left like. The marker goes last: whoever waits for it may read the
# record at once. (tmux does not pass the client's environment into new
# panes, so everything travels as arguments.)
base=$1
export EDITOR="$3" FAKE_MODE="$4" FAKE_SLEEP="${5:-}" FAKE_LOG="$base/fake.log"
cd "$base/scratch" || exit 1
"$2" --no-session 2> "$base/gg-err.log"
status=$?
stty -a < /dev/tty > "$base/stty-after.txt" 2>&1
echo "gg-exit=$status" > "$base/gg-exit.log"
sleep 60
LAUNCH
chmod +x "$FAKE" "$WORK/launch.sh"

echo "== 0. What the test needs =="
if [[ ! -x "$GG" ]]; then echo "FAIL: $GG not built (cargo build --release)" >&2; exit 1; fi
for tool in tmux stty pgrep readlink; do command -v "$tool" >/dev/null || { echo "FAIL: $tool is needed" >&2; exit 1; }; done
if ! curl -sf -m 5 127.0.0.1:8080/health >/dev/null; then echo "FAIL: Strata not answering on 127.0.0.1:8080 (strata launch)" >&2; exit 1; fi
command -v nvim >/dev/null || echo "note: no nvim — cases B, C, L and M are skipped"
first_drafts=$(draft_dirs)
echo "ok"

echo "== A: fake editor round trip; the editor, not gg, gets the keys"
start "$FAKE" ok
keys -l 'hello'; wait_for 'hello' 5
keys C-g; wait_log 'start'; sleep 1
keys -l 'Z'                       # must reach the editor's read, not the composer
wait_for '+ edited' 10 || show; sleep 0.4
[[ "$(line)" == "hello + edited" ]] && ok A1 "composer holds exactly the saved draft" || bad A1 "composer" "$(composer)"
keys -l 'Q'; wait_for 'editedQ' 3 || true
keys -l 'R'; wait_for 'editedQR' 3 || true
[[ "$(line)" == "hello + editedQR" ]] && ok A2 "typing works again after the return; nothing leaked in, nothing dropped" || bad A2 "composer" "$(composer)"
grep -q '^key=\[Z\]' "$WORK/fake.log" && ok A3 "the editor read the key itself" || bad A3 "editor log" "$(cat "$WORK/fake.log")"
if grep -q -- '-icanon' "$WORK/fake.log"; then bad A4 "raw mode was still on when the editor started" "$(grep -- '--' "$WORK/fake.log" | head -2)"
elif ! grep -qE '(^|[[:space:]])isig[[:space:]]' "$WORK/fake.log"; then bad A4 "signals were still off" "$(head -8 "$WORK/fake.log")"
else ok A4 "the editor started on a cooked, signal-capable tty"; fi
no_leak A5
keys Enter; wait_for 'you │ hello + editedQR' 5 || bad A6 "the edited draft did not submit" "$(show)"
wait_for "○ ready" 90 && ok A6 "the edited draft submits like typed text" || bad A6 "run did not finish" "$(show)"
finish A7

if command -v nvim >/dev/null; then
echo "== B: real nvim, on a draft that already has text"
start "$NVIM" ok
keys -l 'from the composer'; wait_for 'from the composer' 5
keys C-g; wait_nvim || show
keys -l 'A and from nvim'; sleep 2; keys Escape; sleep 1; keys -l ':wq'; keys Enter
wait_for 'from nvim' 30 || show; sleep 0.4
keys -l 'Q'; wait_for 'from nvimQ' 3 || true
[[ "$(line)" == "from the composer and from nvimQ" ]] && ok B1 "nvim's save is the composer text, nothing ghost-typed" || bad B1 "composer" "$(composer)"
no_leak B2; finish B3

echo "== C: real nvim, from an empty composer"
start "$NVIM" ok
keys C-g; wait_nvim || show
keys -l 'ihello fresh'; sleep 2; keys Escape; sleep 1; keys -l ':wq'; keys Enter
wait_for 'hello fresh' 30 || show; sleep 0.4
keys -l 'Q'; wait_for 'hello freshQ' 3 || true
[[ "$(line)" == "hello freshQ" ]] && ok C1 "text composed in nvim lands in the composer, nothing ghost-typed" || bad C1 "composer" "$(composer)"
finish C2
else ((skipped += 4)); fi

echo "== D: editor fails after typing — composer unchanged, draft kept"
start "$FAKE" fail
keys -l 'keep me'; wait_for 'keep me' 5; keys C-g
wait_for 'status 3' 10 || show
pane | grep -qF 'exited with status 3; composer unchanged; the edited draft is kept at' && ok D1 "says why, and where" || bad D1 "transcript" "$(show)"
sleep 0.4; keys -l 'Q'; wait_for 'keep meQ' 3 || true
[[ "$(line)" == "keep meQ" ]] && ok D2 "composer untouched" || bad D2 "composer" "$(composer)"
kept_dir
grep -qF 'typed then failed' "$KEPT_FILE" && ok D3 "the kept draft holds the typing" || bad D3 "kept draft" "$(cat "$KEPT_FILE" 2>&1)"
quit_and_check D
kept_survives D4
stderr_clean D5

echo "== E: editor cancelled with no edits — nothing kept"
start "$FAKE" cancel
keys -l 'still here'; wait_for 'still here' 5; keys C-g
wait_for 'status 1' 10 || show
pane | grep -qF 'exited with status 1; composer unchanged' && ok E1 "says why" || bad E1 "transcript" "$(show)"
pane | grep -q 'kept at' && bad E2 "a cancel left a draft behind" "$(show)" || ok E2 "no draft kept"
sleep 0.4; keys -l 'Q'; wait_for 'hereQ' 3 || true
[[ "$(line)" == "still hereQ" ]] && ok E3 "composer untouched, keys live" || bad E3 "composer" "$(composer)"
finish E4

echo "== F: editor cannot start — named, composer intact, keys live"
start gg-no-such-editor-xyz ok
keys -l 'intact'; wait_for 'intact' 5; keys C-g
wait_for 'could not start' 10 || show
pane | grep -qF 'could not start "gg-no-such-editor-xyz"' && ok F1 "names the program" || bad F1 "transcript" "$(show)"
sleep 0.4; keys -l 'Q'; wait_for 'intactQ' 3 || true
[[ "$(line)" == "intactQ" ]] && ok F2 "typing works after the failure" || bad F2 "composer" "$(composer)"
no_leak F3; finish F4

echo "== G: Ctrl+G while a run is in flight — the run continues behind the editor"
start "$FAKE" slow 10
keys -l 'Count from 1 up to 25, one number per line, no other text.'; keys Enter; wait_for '● working' 10
keys -l 'draft text'; wait_for 'draft text' 3; keys C-g
wait_log 'draft='
grep -qF 'draft=[draft text]' "$WORK/fake.log" && ok G1 "the editor got the composer's text while busy" || bad G1 "editor log" "$(grep -E '^(draft=|start)' "$WORK/fake.log")"
wait_for 'draft while busy' 25 || show
[[ "$(line)" == "draft while busy" ]] && ok G2 "composer is the saved draft" || bad G2 "composer" "$(composer)"
wait_for "○ ready" 120 && ok G3 "the run finished, the footer is back to ready" || bad G3 "run did not finish" "$(show)"
numbers=$(pane | grep -cE '^ *[0-9]{1,2} *$')
((numbers >= 20)) && ok G4 "the answer's $numbers number lines streamed in behind the editor" || bad G4 "expected the answer's number lines" "$(show)"
pane | grep -q 'run failed' && bad G5 "the run failed" "$(show)" || ok G5 "no run failure"
no_leak G6; finish G7

echo "== H: the ! shell lines still fold back into the view"
start "$FAKE" instant
keys -l "!!printf 'SHELL-%s\n' LOCAL"; keys Enter
wait_for 'SHELL-LOCAL' 10 && ok H1 "a !! line shows its output locally" || bad H1 "transcript" "$(show)"
wait_for "○ ready" 5 && ok H2 "back to ready" || bad H2 "still busy" "$(show)"
keys -l "!printf 'SHELL-%s\n' CONTEXT"; keys Enter
wait_for 'SHELL-CONTEXT' 10 && ok H3 "a ! line shows its output" || bad H3 "transcript" "$(show)"
wait_for "○ ready" 90 && ok H4 "the run it starts finishes" || bad H4 "run did not finish" "$(show)"
finish H5

echo "== I: editor returns at once (a GUI editor without a wait flag) — a hint, composer intact"
start "$FAKE" instant
keys -l 'draft here'; wait_for 'draft here' 5; keys C-g
wait_for 'returned at once' 10 || show
pane | grep -qF 'code --wait' && ok I1 "says the editor came back at once and why that matters" || bad I1 "transcript" "$(show)"
sleep 0.4; keys -l 'Q'; wait_for 'hereQ' 3 || true
[[ "$(line)" == "draft hereQ" ]] && ok I2 "composer untouched, keys live" || bad I2 "composer" "$(composer)"
no_leak I3; finish I4

echo "== J: editor killed after typing — composer unchanged, draft kept"
start "$FAKE" killed
keys -l 'keep this'; wait_for 'keep this' 5; keys C-g
wait_for 'was killed by a signal' 10 || show
pane | grep -qF 'was killed by a signal; composer unchanged; the edited draft is kept at' && ok J1 "says what happened, and where the typing is" || bad J1 "transcript" "$(show)"
sleep 0.4; keys -l 'Q'; wait_for 'thisQ' 3 || true
[[ "$(line)" == "keep thisQ" ]] && ok J2 "composer untouched, keys live" || bad J2 "composer" "$(composer)"
kept_dir
quit_and_check J
kept_survives J3
stderr_clean J4

echo "== K: a full-screen editor dies on a raw tty — gg recovers, and the shell is none the worse"
start "$FAKE" rawcrash
keys -l 'before'; wait_for 'before' 5; keys C-g
wait_for 'was killed by a signal' 10 || show; sleep 0.4
pane | grep -q 'EDITOR FRAME' && bad K1 "the dead editor's frame is still on screen" "$(show)" || ok K1 "the screen was repainted whole"
check_modes K2
keys -l 'Q'; wait_for 'beforeQ' 3 || true
[[ "$(line)" == "beforeQ" ]] && ok K3 "gg took the terminal back and typing works" || bad K3 "composer" "$(composer)"
kept_dir
quit_and_check K          # .1–.3: cooked tty, no modes left on — the point of this case
kept_survives K4
stderr_clean K5

# Real nvim killed mid-edit, on a tmux server with `extended-keys` as
# given: the modes it switched on (keypad, cursor keys, key modification,
# the key encoding itself) must be off again — or the numpad types nothing
# and, with extended keys on, Ctrl+C itself no longer reaches gg.
killed_nvim_case() { # killed_nvim_case <label> <extended-keys: off|on>
  local L=$1
  echo "== $L: real nvim killed mid-edit, tmux extended-keys $2 — the modes it switched on are off again"
  EXT_KEYS=$2
  start "$NVIM" ok
  # The case only means something if the option really is as ordered.
  [[ "$(t show -gw extended-keys)" == "extended-keys $2" ]] && ok "$L.0" "this case's tmux server has extended-keys $2" || bad "$L.0" "extended-keys" "$(t show -gw extended-keys)"
  keys -l 'before'; wait_for 'before' 5; keys C-g; wait_nvim || show
  keys -l 'ityping in nvim'; sleep 1; keys Escape; sleep 0.5
  local nvims; nvims=$(pgrep -f "^nvim -u NONE -n .*gg-draft-$CASE_GGPID/" | tr '\n' ' ')
  [[ -n "$nvims" ]] && ok "$L.1" "found the editor on this session's draft" || bad "$L.1" "no nvim on this session's draft" "gg pid: ${CASE_GGPID:-none}"
  kill -9 $nvims 2>/dev/null
  wait_for 'was killed by a signal' 10 || show; sleep 0.4
  check_modes "$L.2"
  keys KP7; wait_for 'before7' 3 || true          # a numpad key: dead while keypad mode is left on
  [[ "$(line)" == "before7" ]] && ok "$L.3" "a numpad key types its digit" || bad "$L.3" "composer" "$(composer)"
  keys -l 'Q'; wait_for 'before7Q' 3 || true
  [[ "$(line)" == "before7Q" ]] && ok "$L.4" "typing works" || bad "$L.4" "composer" "$(composer)"
  finish "$L.5"                                    # .1: Ctrl+C still quits gg
  EXT_KEYS=off
}
if command -v nvim >/dev/null; then
  killed_nvim_case L off
  killed_nvim_case M on
fi

echo "== N: composer keys — Ctrl+J/Alt+Enter/Shift+Enter break lines; Ctrl+A/E/K/U edit"
EXT_KEYS=on                                       # Shift+Enter needs the pane in extended-key mode
start "$FAKE" instant
keys -l 'one'; keys C-j; keys -l 'two'; wait_for 'two' 5
pane | grep -qx 'one' && pane | grep -qx 'two' && ok N1 "Ctrl+J broke the line" || bad N1 "Ctrl+J" "$(show)"
keys M-Enter; keys -l 'three'; wait_for 'three' 5
pane | grep -qx 'three' && ok N2 "Alt+Enter broke the line" || bad N2 "Alt+Enter" "$(show)"
keys C-a; keys -l 'X'; wait_for 'Xthree' 5 && ok N3 "Ctrl+A put the cursor at the line start" || bad N3 "Ctrl+A" "$(show)"
keys C-e; keys -l '!'; wait_for 'Xthree!' 5 && ok N4 "Ctrl+E put the cursor at the line end" || bad N4 "Ctrl+E" "$(show)"
keys C-k; sleep 0.4
pane | grep -q 'Xthree!' && ok N5 "Ctrl+K at the end of the last line is a no-op" || bad N5 "Ctrl+K" "$(show)"
keys C-u; sleep 0.4
pane | grep -q 'Xthree!' && bad N6 "Ctrl+U did not clear the line" "$(show)" || ok N6 "Ctrl+U killed back to the line start"
keys S-Enter; keys -l 'four'; wait_for 'four' 5
pane | grep -qx 'four' && ok N7 "Shift+Enter broke the line (extended keys on)" || bad N7 "Shift+Enter" "$(show)"
pane | grep -q 'you │' && bad N8 "an unsubmitted draft was sent" "$(show)" || ok N8 "nothing was submitted"
EXT_KEYS=off
finish N9

left=$(comm -13 <(echo "$first_drafts") <(draft_dirs))
if [[ -z "$left" ]]; then ((pass++)); echo "  PASS Z: no draft directory outlived its gg"; else ((fail++)); echo "  FAIL Z: draft directories left: $left"; fi

echo
echo "RESULT: $pass passed, $fail failed, $skipped skipped"
exit $((fail > 0))
