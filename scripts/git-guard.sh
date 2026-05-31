#!/usr/bin/env bash
# git-guard.sh — vagus PreToolUse(Bash) hook. ADR 0018 / G22 + G24.
#
# Two jobs, both FAIL-OPEN (a missing dep or non-git cwd never blocks work):
#   G22  deny `git commit` on `main` and `git push` of the `main` branch — land changes via a feature
#        branch + PR. Releases are exempt: a release-only commit (staging just Cargo.toml / Cargo.lock /
#        CHANGELOG.md / Formula/) and `vX.Y.Z` tag pushes are allowed on `main` (RELEASING.md).
#   G24  on a `git commit` that stages src/** but no design/** or CHANGELOG.md, emit a soft reminder
#        to leave a breadcrumb (allowed, not blocked).
#
# Input: tool-call JSON on stdin (.tool_input.command). Output: a PreToolUse decision JSON on stdout.

set -u

# --- fail-open guards -------------------------------------------------------
command -v jq  >/dev/null 2>&1 || exit 0
command -v git >/dev/null 2>&1 || exit 0

payload="$(cat)"
cmd="$(printf '%s' "$payload" | jq -r '.tool_input.command // empty' 2>/dev/null)"
[ -n "$cmd" ] || exit 0

# Only care about git commit / git push.
case "$cmd" in
  *"git commit"*) verb=commit ;;
  *"git push"*)   verb=push ;;
  *)              exit 0 ;;
esac

# Not in a git work tree? fail open.
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || exit 0
branch="$(git symbolic-ref --short -q HEAD 2>/dev/null || true)"

deny() {
  # $1 = reason
  jq -cn --arg r "$1" '{
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "deny",
      permissionDecisionReason: $r
    }
  }'
  exit 0
}

# --- G22: no direct commits/pushes to main (releases exempt) ---------------
# Release commits may land directly on main (RELEASING.md): a version bump or the CI formula bump — a
# commit staging ONLY release files (Cargo.toml / Cargo.lock / CHANGELOG.md / Formula/) — plus vX.Y.Z
# tag pushes. Anything touching code or docs still needs a feature branch + PR.
if [ "$verb" = "commit" ] && [ "$branch" = "main" ]; then
  staged="$(git diff --cached --name-only 2>/dev/null || true)"
  # Deny unless something is staged AND every staged path is a release file.
  if [ -z "$staged" ] || printf '%s\n' "$staged" | grep -qvE '^(Cargo\.toml|Cargo\.lock|CHANGELOG\.md|Formula/)'; then
    deny "G22 (ADR 0018): no direct commits to main. Create a feature branch (git switch -c feat/<name>) and open a PR. (Release-only commits — Cargo.toml / Cargo.lock / CHANGELOG.md / Formula/ — are allowed on main.)"
  fi
fi

if [ "$verb" = "push" ]; then
  # Release carve-out: allow tag pushes (vX.Y.Z / --tags / --follow-tags) from anywhere, including main —
  # cutting a release tags a green main (RELEASING.md). Otherwise deny pushing the main branch itself.
  if printf '%s' "$cmd" | grep -Eq '(--tags|--follow-tags|(^|[[:space:]])v[0-9]+\.[0-9]+\.[0-9]+([[:space:]]|$))'; then
    :
  elif [ "$branch" = "main" ] || printf '%s' "$cmd" | grep -Eq '(^|[[:space:]])main([[:space:]]|$)'; then
    deny "G22 (ADR 0018): no direct pushes to the main branch. Push your feature branch and open a PR. (Release tag pushes vX.Y.Z are allowed.)"
  fi
fi

# --- G24: soft breadcrumb nudge on src-only commits ------------------------
if [ "$verb" = "commit" ]; then
  staged="$(git diff --cached --name-only 2>/dev/null || true)"
  if printf '%s\n' "$staged" | grep -Eq '^src/' \
     && ! printf '%s\n' "$staged" | grep -Eq '^design/' \
     && ! printf '%s\n' "$staged" | grep -Eq '(^|/)CHANGELOG\.md$'; then
    jq -cn '{
      systemMessage: "G24 (ADR 0018): src/ changed without a design/ or CHANGELOG.md update staged. If this was an architectural decision, add/update an ADR; if user-noticeable, add a CHANGELOG.md entry."
    }'
    exit 0
  fi
fi

exit 0
