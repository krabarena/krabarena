#!/usr/bin/env bash
set -euo pipefail

PUBLIC_URL="${PUBLIC_URL:-https://github.com/krabarena/krabarena.git}"
PRIVATE_REPO="${PRIVATE_REPO:-krabarena/krabarena-private}"
BASE="${BASE:-main}"
CI_MODE=false
[[ "${1:-}" == "--ci" ]] && CI_MODE=true

if $CI_MODE; then
  git config user.name "github-actions[bot]"
  git config user.email "41898333+github-actions[bot]@users.noreply.github.com"
fi

git remote get-url public >/dev/null 2>&1 || git remote add public "$PUBLIC_URL"
git fetch --quiet public main
git fetch --quiet origin "$BASE"

if git merge-base --is-ancestor public/main "origin/$BASE"; then
  echo "Up to date: public/main is already merged into $BASE."
  exit 0
fi

short=$(git rev-parse --short public/main)
branch="sync/public-$short"
git checkout --quiet -B "$branch" "origin/$BASE"

if ! git merge --no-ff --no-edit -m "chore(sync): merge public main ($short)" public/main; then
  git merge --abort || true
  echo "Merge conflict between public/main and $BASE. Resolve manually:" >&2
  echo "  git checkout -B $branch origin/$BASE && git merge public/main" >&2
  exit 1
fi

git push --quiet --force-with-lease origin "$branch"

if gh pr view "$branch" --repo "$PRIVATE_REPO" --json state -q .state 2>/dev/null | grep -q OPEN; then
  echo "PR for $branch already open."
  exit 0
fi

commits=$(git log --no-merges --format='- %s (%h)' "origin/$BASE..public/main" | head -50)
gh pr create --repo "$PRIVATE_REPO" --base "$BASE" --head "$branch" \
  --title "chore(sync): merge public main ($short)" \
  --body "Brings these public commits into the deploy repo:

$commits

Merging ships them through Build & Push → Deploy to Test → Deploy."
