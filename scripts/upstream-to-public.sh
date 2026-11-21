#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <commit-or-range> [branch-name]" >&2
  echo "  e.g. $0 abc1234            # one private commit" >&2
  echo "       $0 main~3..main      # a range (merge commits are skipped)" >&2
  exit 2
}

[[ $# -ge 1 ]] || usage
RANGE="$1"
PUBLIC_URL="${PUBLIC_URL:-https://github.com/krabarena/krabarena.git}"
PUBLIC_REPO="${PUBLIC_REPO:-krabarena/krabarena}"

git remote get-url public >/dev/null 2>&1 || git remote add public "$PUBLIC_URL"
git fetch --quiet public main

if [[ "$RANGE" == *..* ]]; then
  commits=$(git rev-list --reverse --no-merges "$RANGE")
else
  commits=$(git rev-parse "$RANGE^{commit}")
fi
[[ -n "$commits" ]] || { echo "No commits in $RANGE." >&2; exit 1; }

branch="${2:-upstream/$(git rev-parse --short "$(echo "$commits" | tail -1)")}"
git checkout --quiet -B "$branch" public/main

for c in $commits; do
  if ! git cherry-pick "$c"; then
    echo "Cherry-pick of $c conflicts. Resolve, then: git cherry-pick --continue" >&2
    exit 1
  fi
done

if command -v gitleaks >/dev/null 2>&1; then
  gitleaks git . --no-banner --redact --log-opts="public/main..HEAD" \
    || { echo "gitleaks found something; not pushing." >&2; exit 1; }
else
  echo "gitleaks not installed; review the diff for secrets before pushing:" >&2
  git diff --stat public/main..HEAD >&2
  read -r -p "Push to public? [y/N] " ok
  [[ "$ok" == y || "$ok" == Y ]] || exit 1
fi

git push --quiet -u public "$branch"
gh pr create --repo "$PUBLIC_REPO" --base main --head "$branch" --fill
