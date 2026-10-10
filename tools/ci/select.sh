#!/usr/bin/env bash
# The select job of check.yml: find the comparison base and plan what the
# change affects (tools/ci/affected_packages.py). It runs from the tree under
# test, after a pull request was merged onto main, so the plan always follows
# that tree's rules rather than those of the pull request's own workflow file.
#
# Environment (from the workflow):
#   EVENT_NAME          github.event_name
#   PULL_REQUEST_BASE   the main commit a pull request was merged onto
#   PUSH_BEFORE         github.event.before
#   RUN_ATTEMPT, HEAD_SHA, LANDER_LOGIN, GITHUB_TOKEN  see recheck_base.py
#   GITHUB_OUTPUT, GITHUB_STEP_SUMMARY
set -euo pipefail

zero=0000000000000000000000000000000000000000
base=${PULL_REQUEST_BASE:-}
selection_args=()
if [ "$EVENT_NAME" = pull_request ]; then
  selection_args+=(--allow-security-skip)
  # A lander re-check of a green run after main moved narrows to what
  # main changed since the base that run tested.
  tested_base=$(python3 tools/ci/recheck_base.py)
  if [ -n "$tested_base" ]; then
    git fetch --no-tags --depth=256 origin "$tested_base" "$base"
    selection_args+=(--recheck-from "$tested_base")
  fi
elif [ "$EVENT_NAME" = workflow_dispatch ]; then
  # Dispatch has no pull-request base or push-before SHA. Do not
  # silently reduce an acceptance run to the last commit's changes.
  git fetch origin +refs/heads/main:refs/remotes/origin/main
  base=origin/main
  selection_args+=(--full)
elif [ -z "$base" ] || [ "$base" = "null" ]; then
  base=${PUSH_BEFORE:-}
  if [ -n "$base" ] && [ "$base" != "$zero" ] && ! git cat-file -e "$base^{commit}" 2>/dev/null; then
    git fetch origin "$base"
  fi
fi
if [ "$EVENT_NAME" = push ]; then
  # Pull requests run the hermetic suites their paths reach. Every
  # push runs all of them, so an input a suite forgot to declare
  # still fails at the merge that broke it.
  selection_args+=(--all-suites)
fi
if [ -z "$base" ] || [ "$base" = "$zero" ]; then
  base=HEAD^
fi
# Every diff-style gate compares with the common ancestor, as package
# selection does. A pull request is tested merged onto main, so its
# ancestor with main is the merged main commit itself. A push to main
# extends its previous tip, so there the common ancestor is exactly
# github.event.before.
base=$(git merge-base "$base" HEAD)
echo "base=$base" >> "$GITHUB_OUTPUT"
python3 tools/ci/affected_packages.py --base "$base" --shards 2 "${selection_args[@]}" \
  --github-output "$GITHUB_OUTPUT" | tee ci-plan.json
{
  echo '```json'
  cat ci-plan.json
  echo '```'
} >> "$GITHUB_STEP_SUMMARY"
# The affected apps whose tapps the `tapps` job packages and holds to their
# size budgets before the merge (DEVELOPMENT.md, "App package size"):
# those of the default collection closest to a budget, at most a few, so a
# toolkit change does not package the whole collection. Only pull requests:
# on main, Release / apps measures every affected app after the merge.
if [ "$EVENT_NAME" = pull_request ]; then
  affected=$(python3 tools/apps/app_release.py --base "$base" \
    | python3 -c 'import json, sys; print(" ".join(json.load(sys.stdin)["apps"]))')
  # The run follows the pull request's own check.yml (that of HEAD_SHA, its
  # head), not the merged tree's. A branch older than the tapps job would
  # pass without measuring, so select fails it instead.
  workflow=$(mktemp)
  git show "$HEAD_SHA:.forgejo/workflows/check.yml" > "$workflow"
  {
    echo '```'
    python3 tools/perf/tapp_size_ratchet.py select --apps "$affected" --github-output "$GITHUB_OUTPUT" \
      --workflow "$workflow"
    echo '```'
  } | tee -a "$GITHUB_STEP_SUMMARY"
else
  printf 'tapp_apps=\ntapp_required=false\n' >> "$GITHUB_OUTPUT"
fi
