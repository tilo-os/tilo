#!/usr/bin/env bash
# Merge the pull request's base branch into its checked-out head (see
# action.yml). The base is main for ordinary pull requests and the lower branch
# for a stacked one.
set -euo pipefail

if [ "${EVENT:-}" != pull_request ]; then
  echo "main=" >> "${GITHUB_OUTPUT:-/dev/null}"
  exit 0
fi

head=$(git rev-parse HEAD)
if [ -z "${MAIN:-}" ]; then
  base_ref=${BASE_REF:-main}
  git fetch --no-tags --depth=1 origin "+refs/heads/$base_ref:refs/remotes/origin/$base_ref"
  MAIN=$(git rev-parse "refs/remotes/origin/$base_ref")
fi
[[ "$MAIN" =~ ^[0-9a-f]{40}$ ]] || { echo "::error::invalid main commit: $MAIN" >&2; exit 1; }

# The checkout is shallow; deepen both sides until they share a merge base.
for depth in 64 256 1024 4096; do
  git fetch --no-tags --depth="$depth" origin "$head" "$MAIN"
  git merge-base "$head" "$MAIN" >/dev/null 2>&1 && break
done
git merge-base "$head" "$MAIN" >/dev/null \
  || { echo "::error::no common history with main $MAIN" >&2; exit 1; }

# Identical identity and dates make every job's merge commit the same.
date=$(git log -1 --format=%ct "$head" "$MAIN" | sort -n | tail -n 1)
export GIT_AUTHOR_NAME=CI GIT_AUTHOR_EMAIL=ci@invalid
export GIT_COMMITTER_NAME=CI GIT_COMMITTER_EMAIL=ci@invalid
export GIT_AUTHOR_DATE="@$date +0000" GIT_COMMITTER_DATE="@$date +0000"
if ! git -c commit.gpgsign=false merge --no-ff --no-edit -q -m "Merge main $MAIN into $head" "$MAIN"; then
  git merge --abort || true
  echo "::error::the pull request conflicts with main $MAIN; merge main into the branch" >&2
  exit 1
fi
echo "Testing $head merged onto main $MAIN as $(git rev-parse HEAD)"
echo "main=$MAIN" >> "${GITHUB_OUTPUT:-/dev/null}"
