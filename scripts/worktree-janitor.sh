#!/usr/bin/env bash
# worktree-janitor.sh — vagus worktree hygiene. ADR 0018 / G23.
#
#   --list   (default) print worktrees whose branch is already merged into the base ref. Quiet when
#            none, so the SessionStart hook adds no noise on a clean tree.
#   --prune  remove the merged worktrees that are clean (and delete their merged branch); refuse any
#            worktree with uncommitted changes.
#
# Pure git; FAIL-OPEN (no git / no base ref => exit 0, say nothing).

set -u

mode="${1:---list}"
command -v git >/dev/null 2>&1 || exit 0
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || exit 0

# Base ref: prefer origin/main, fall back to local main; bail quietly if neither exists.
if git rev-parse --verify -q origin/main >/dev/null 2>&1; then
  base="origin/main"
elif git rev-parse --verify -q main >/dev/null 2>&1; then
  base="main"
else
  exit 0
fi

# Collect (path TAB branch) for every LINKED worktree on a branch that is merged into base,
# excluding the main worktree (its `.git` is a real dir, not a gitfile), the worktree we're standing
# in, and the base branch itself.
self="$(git rev-parse --show-toplevel 2>/dev/null || true)"
merged=""
path="" ; branch=""
while IFS= read -r line; do
  case "$line" in
    "worktree "*) path="${line#worktree }" ;;
    "branch "*)
      ref="${line#branch }"            # refs/heads/<name>
      branch="${ref#refs/heads/}"
      ;;
    "")  # end of one worktree record
      if [ -n "$path" ] && [ -n "$branch" ] && [ "$branch" != "main" ] \
         && [ ! -d "$path/.git" ] && [ "$path" != "$self" ]; then
        if git merge-base --is-ancestor "$branch" "$base" 2>/dev/null; then
          merged="${merged}${path}	${branch}
"
        fi
      fi
      path="" ; branch=""
      ;;
  esac
done < <(git worktree list --porcelain)

# Trim trailing newline.
merged="$(printf '%s' "$merged" | sed '/^$/d')"
[ -n "$merged" ] || exit 0

if [ "$mode" = "--list" ]; then
  echo "vagus: merged worktrees ready to prune (G23 — run \`scripts/worktree-janitor.sh --prune\`):"
  printf '%s\n' "$merged" | while IFS=$'\t' read -r p b; do
    echo "  - $p  [$b]"
  done
  exit 0
fi

if [ "$mode" = "--prune" ]; then
  printf '%s\n' "$merged" | while IFS=$'\t' read -r p b; do
    if [ -n "$(git -C "$p" status --porcelain 2>/dev/null)" ]; then
      echo "skip (dirty): $p  [$b]"
      continue
    fi
    if git worktree remove "$p" 2>/dev/null; then
      git branch -d "$b" >/dev/null 2>&1 || true
      echo "removed: $p  [$b]"
    else
      echo "skip (could not remove): $p  [$b]"
    fi
  done
  git worktree prune >/dev/null 2>&1 || true
  exit 0
fi

echo "usage: worktree-janitor.sh [--list|--prune]" >&2
exit 0
