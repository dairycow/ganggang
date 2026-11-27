#!/usr/bin/env bash
# worktrees.sh: the git side of the delegate skill, one worktree per job
# that edits.
#
# A job is a delegate.sh job that runs in a worktree of its own,
# $MAIN/.worktrees/NAME, on a branch cut from main. $MAIN is the
# repository's main checkout. The run that orchestrates reviews the diff
# and decides what lands; this script creates the worktree, starts the
# job, runs the gate and lands the result.
#
# Commands:
#   new NAME [-b BRANCH] [--from REF]  add the worktree on a new branch
#                                     (default feat/NAME) at REF (default main)
#   run NAME --prompt FILE [...]       start delegate.sh --job NAME in the
#                                     worktree; the other options are its
#   gate NAME                          run scripts/pre-commit-check.sh in the
#                                     worktree and record the commit that
#                                     passed in refs/gg/worktrees/BRANCH
#   update NAME                        merge main into the job's branch
#   merge NAME                         fast-forward main to the recorded commit
#   drop NAME                          remove a clean worktree whose branch tip
#                                     is on main, and delete the branch
#   list                               every worktree, against main
#
# The gate record is a ref, which git keeps with the repository, so merge
# needs no state file of its own: it lands only the commit gate recorded,
# and only by fast-forward. merge is the one command that writes in the
# main checkout. Nothing is pushed, rebased, forced or stashed.
#
# Exit status: 0 done, 2 refused or misused (nothing was changed), and 1
# when a step ran and failed: the gate, an update with conflicts, a merge
# git refused. See scripts/test-worktrees.py.
set -euo pipefail

S=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
MAIN_BRANCH=main

usage() {
  cat <<'EOF'
usage: worktrees.sh new NAME [-b BRANCH] [--from REF]
       worktrees.sh run NAME --prompt FILE [delegate.sh options]
       worktrees.sh gate NAME
       worktrees.sh update NAME
       worktrees.sh merge NAME
       worktrees.sh drop NAME
       worktrees.sh list

Runs the git side of the delegate skill: one worktree per job that edits,
under the main checkout's .worktrees/. NAME may use letters, digits, dot,
dash and underscore.
EOF
}

# die: refused or misused, nothing changed. fail: a step ran and failed.
die() {
  echo "worktrees: $*" >&2
  exit 2
}

fail() {
  echo "worktrees: $*" >&2
  exit 1
}

check_name() {
  [[ "$1" =~ ^[A-Za-z0-9._-]+$ && "$1" != "." && "$1" != ".." ]] ||
    die "NAME may use letters, digits, dot, dash and underscore (got: $1)"
}

# The main checkout is the first worktree git lists, from any worktree.
main_checkout() {
  local line
  git rev-parse --git-dir >/dev/null 2>&1 || die "not inside a git repository"
  [[ "$(git rev-parse --is-bare-repository 2>/dev/null || true)" != true ]] ||
    die "a bare repository has no main checkout to land on"
  MAIN=''
  while IFS= read -r line; do
    [[ "$line" == "worktree "* ]] && {
      MAIN=${line#worktree }
      break
    }
  done < <(git worktree list --porcelain) || true
  [[ -n "$MAIN" ]] || die "git lists no worktree here"
}

# Prints the path git recorded for NAME's worktree, or nothing. The path
# has to be a registered worktree: a plain directory under .worktrees/
# belongs to the main checkout, and git -C there would reach that instead.
registered() {
  local entry want
  [[ -d "$wt" ]] || return 1
  want=$(cd -P "$wt" 2>/dev/null && pwd -P) || return 1
  while IFS= read -r entry; do
    [[ "$entry" == "worktree "* ]] || continue
    entry=${entry#worktree }
    [[ "$(cd -P "$entry" 2>/dev/null && pwd -P || true)" == "$want" ]] && {
      printf '%s\n' "$entry"
      return 0
    }
  done < <(git worktree list --porcelain)
  return 1
}

# NAME's worktree: registered, under $MAIN/.worktrees/NAME.
job() {
  check_name "$1"
  name=$1
  wt="$MAIN/.worktrees/$name"
  registered >/dev/null || die "no worktree $name at $wt (start with: new $name)"
}

clean() {
  [[ -z "$(git -C "$wt" status --porcelain)" ]] ||
    die "worktree $name has changes; commit them first"
}

# The job's branch, and the ref gate records its commit under.
on_branch() {
  branch=$(git -C "$wt" symbolic-ref --short -q HEAD) ||
    die "worktree $name is detached; a job runs on a branch"
  ref="refs/gg/worktrees/$branch"
}

short() {
  git rev-parse --short "$1"
}

cmd_new() {
  local branch='' from=$MAIN_BRANCH tip common exclude
  name=''
  while [[ $# -gt 0 ]]; do
    case "$1" in
      -b) [[ $# -ge 2 ]] || die "-b needs a branch name"
        branch=$2
        shift 2 ;;
      --from) [[ $# -ge 2 ]] || die "--from needs a commit or branch"
        from=$2
        shift 2 ;;
      -h|--help) usage
        exit 0 ;;
      -*) usage >&2
        die "unknown option: $1" ;;
      *) [[ -z "$name" ]] || die "one NAME only (got: $name and $1)"
        name=$1
        shift ;;
    esac
  done
  check_name "$name"
  [[ -n "$branch" ]] || branch="feat/$name"
  wt="$MAIN/.worktrees/$name"
  git check-ref-format --branch "$branch" >/dev/null 2>&1 || die "not a branch name: $branch"
  tip=$(git rev-parse --verify --quiet --end-of-options "$from^{commit}") ||
    die "$from is not a commit in this repository"
  [[ ! -e "$wt" ]] || die "$wt already exists"
  ! git show-ref --verify --quiet "refs/heads/$branch" || die "branch $branch already exists"
  mkdir -p "$MAIN/.worktrees"
  git -C "$MAIN" worktree add -b "$branch" "$wt" "$tip" || fail "git did not add the worktree"
  # Keep .worktrees/ out of the main checkout's status. The exclude file is
  # git's own, per repository, and is never committed.
  common=$(git rev-parse --git-common-dir)
  mkdir -p "$common/info"
  exclude="$common/info/exclude"
  grep -qxF '.worktrees/' "$exclude" 2>/dev/null || echo '.worktrees/' >>"$exclude"
  echo "worktrees: worktree $name at $wt, on $branch at $(short "$tip") ($from)"
}

cmd_run() {
  job "$1"
  shift
  [[ -x "$S/delegate.sh" ]] || die "delegate.sh not found beside worktrees.sh"
  echo "worktrees: job $name in $wt" >&2
  cd "$wt"
  # --job goes last, so NAME wins over one of the caller's.
  exec "$S/delegate.sh" "$@" --job "$name"
}

cmd_gate() {
  job "$1"
  clean
  on_branch
  local before after
  [[ -x "$wt/scripts/pre-commit-check.sh" ]] ||
    die "no executable scripts/pre-commit-check.sh in $wt"
  before=$(git -C "$wt" rev-parse HEAD)
  echo "worktrees: gating $name at $(short "$before")" >&2
  (cd "$wt" && scripts/pre-commit-check.sh) || fail "gate FAILED for $name; nothing recorded"
  after=$(git -C "$wt" rev-parse HEAD)
  [[ "$after" == "$before" ]] || fail "$name moved during the gate; gate it again"
  clean
  git -C "$wt" update-ref "$ref" "$after"
  echo "worktrees: gate passed for $name; recorded $(short "$after")"
}

cmd_update() {
  job "$1"
  clean
  on_branch
  local tip
  tip=$(git -C "$wt" rev-parse HEAD)
  if git -C "$wt" merge-base --is-ancestor $MAIN_BRANCH "$tip"; then
    echo "worktrees: $name already has $MAIN_BRANCH"
    return
  fi
  if ! git -C "$wt" merge --no-edit $MAIN_BRANCH; then
    if git -C "$wt" rev-parse -q --verify MERGE_HEAD >/dev/null; then
      # git leaves the conflicts in the worktree for the job to resolve.
      fail "$name conflicts with $MAIN_BRANCH: hand the conflicts to the job as a follow-up prompt, then review, gate and merge again"
    fi
    fail "git did not merge $MAIN_BRANCH into $name"
  fi
  echo "worktrees: $name now has $MAIN_BRANCH, at $(git -C "$wt" rev-parse --short HEAD); review, then gate"
}

# The main checkout is on main: a fast-forward moves its working tree.
on_main() {
  local at
  at=$(git -C "$MAIN" symbolic-ref --short -q HEAD) || at="(detached)"
  [[ "$at" == $MAIN_BRANCH ]] || die "the main checkout is on $at; merge needs it on $MAIN_BRANCH"
}

# Nothing staged in the main checkout, and no local change in a path the
# fast-forward rewrites. git refuses both by itself; this says so first,
# and leaves the decision with the developer, since nothing is stashed.
main_checkout_clear() {
  local tip=$1 local_paths hit
  [[ -z "$(git -C "$MAIN" diff --cached --name-only)" ]] ||
    die "the main checkout has staged changes; ask the developer"
  local_paths=$(git -C "$MAIN" ls-files --modified --others --exclude-standard)
  [[ -n "$local_paths" ]] || return 0
  hit=$(git -C "$MAIN" diff --name-only $MAIN_BRANCH "$tip" |
    grep -xF -f <(printf '%s\n' "$local_paths") || true)
  [[ -z "$hit" ]] ||
    die "the main checkout has changes in files the merge would touch; ask the developer: $(printf '%s ' $hit)"
}

cmd_merge() {
  job "$1"
  clean
  on_branch
  on_main
  local tip gated count
  tip=$(git -C "$wt" rev-parse HEAD)
  if git -C "$wt" merge-base --is-ancestor "$tip" $MAIN_BRANCH; then
    echo "worktrees: $name is already on $MAIN_BRANCH at $(short "$tip"); nothing to land"
    return
  fi
  gated=$(git -C "$wt" rev-parse --verify --quiet "$ref") ||
    die "no gate recorded for $name; run: gate $name"
  [[ "$tip" == "$gated" ]] ||
    die "$name moved since the gate (tip $(short "$tip"), gated $(short "$gated")); review, then gate again"
  git -C "$wt" merge-base --is-ancestor $MAIN_BRANCH "$tip" ||
    die "$MAIN_BRANCH moved since $name branched; run: update $name, then gate and merge again"
  main_checkout_clear "$tip"
  count=$(git -C "$MAIN" rev-list --count "$MAIN_BRANCH..$tip")
  git -C "$MAIN" merge --ff-only --quiet "$tip" || fail "git refused the merge; ask the developer, and stash nothing"
  echo "worktrees: landed $name on $MAIN_BRANCH at $(short "$tip"), $count commit(s)"
}

cmd_drop() {
  job "$1"
  clean
  on_branch
  local tip
  tip=$(git -C "$wt" rev-parse HEAD)
  git -C "$wt" merge-base --is-ancestor "$tip" $MAIN_BRANCH ||
    die "$name is not on $MAIN_BRANCH yet; merge it first"
  git -C "$MAIN" worktree remove "$wt" || fail "git refused to remove the worktree"
  # The branch goes by a compare-and-delete on the tip just found to be
  # on main, since that is the stronger fact. `branch -d` judges a branch
  # against the current HEAD or its upstream instead, and keeps this one
  # when the main checkout is on another branch, after the worktree is
  # already gone. The old value makes the deletion a no-op if the branch
  # moved since the check; it is never forced.
  git -C "$MAIN" update-ref -d "refs/heads/$branch" "$tip" ||
    fail "worktree removed, but branch $branch moved since the check and is kept"
  git -C "$MAIN" update-ref -d "$ref" 2>/dev/null || true
  git -C "$MAIN" worktree prune
  echo "worktrees: dropped $name, and branch $branch"
}

# One line per worktree past the main one: branch, behind and ahead of
# main, clean, and whether it is merged, gated or stale.
cmd_list() {
  local path name branch count behind ahead state flags tip
  echo "worktrees: against $MAIN_BRANCH"
  local paths=() line
  while IFS= read -r line; do
    [[ "$line" == "worktree "* ]] && paths+=("${line#worktree }")
  done < <(git worktree list --porcelain)
  for path in "${paths[@]:1}"; do
    name=$(basename "$path")
    if [[ ! -d "$path" ]]; then
      printf '%-20s stale  %s\n' "$name" "$path"
      continue
    fi
    branch=$(git -C "$path" symbolic-ref --short -q HEAD 2>/dev/null) || branch='(detached)'
    count=$(git -C "$path" rev-list --left-right --count "$MAIN_BRANCH...HEAD" 2>/dev/null) || count='? ?'
    read -r behind ahead <<<"$count"
    state=dirty
    [[ -z "$(git -C "$path" status --porcelain)" ]] && state=clean
    flags=''
    tip=$(git -C "$path" rev-parse HEAD)
    git merge-base --is-ancestor "$tip" $MAIN_BRANCH && flags+=' merged'
    if [[ "$branch" != '(detached)' && "$(git rev-parse --verify --quiet "refs/gg/worktrees/$branch")" == "$tip" ]]; then
      flags+=' gated'
    fi
    printf '%-20s %-28s behind %-3s ahead %-3s %-6s%s  %s\n' "$name" "$branch" "$behind" "$ahead" "$state" "$flags" "$path"
  done
}

main_checkout
cmd=${1:-}
[[ $# -gt 0 ]] && shift
case $cmd in
  new) cmd_new "$@" ;;
  run) [[ $# -ge 1 ]] || die "run needs a NAME"
    cmd_run "$@" ;;
  gate | update | merge | drop) [[ $# -eq 1 ]] || die "$cmd takes one NAME"
    "cmd_$cmd" "$1" ;;
  list) [[ $# -eq 0 ]] || die "list takes no arguments"
    cmd_list ;;
  -h | --help | help) usage
    exit 0 ;;
  *) usage >&2
    die "unknown command: ${cmd:-(none)}" ;;
esac
