#!/usr/bin/env bash
# The select job of check.yml: find the comparison base and plan what the
# change affects (tools/ci/affected_packages.py). It runs from the tree under
# test, so the plan always follows that tree's rules rather than those of an
# older commit's workflow file.
#
# The base is the last commit main promoted to `stable`, not the parent of the
# push: main is allowed to be unstable, so a batch of commits is planned and
# tested as one change against the last state that was known good. A push that
# supersedes an in-flight run cancels it, and its commits stay covered by the
# newer run's diff.
#
# Environment (from the workflow):
#   EVENT_NAME          github.event_name
#   PULL_REQUEST_BASE   the main commit a dispatch was merged onto
#   STABLE_REF          the promotion ref to diff against (origin/stable)
#   HEAD_SHA, GITHUB_OUTPUT, GITHUB_STEP_SUMMARY
set -euo pipefail

zero=0000000000000000000000000000000000000000
base=${PULL_REQUEST_BASE:-}
selection_args=()
if [ "$EVENT_NAME" = workflow_dispatch ]; then
  # Dispatch has no base commit: validate the complete workspace on both
  # native architectures instead of silently reducing the run to one diff.
  selection_args+=(--full)
elif [ -z "$base" ] || [ "$base" = "null" ]; then
  # A push to main: compare with the promoted commit. When stable is missing
  # or is not an ancestor of this commit (history was rewritten, or the ref was
  # recreated), there is no diff to trust and the plan is the full one.
  base=''
  stable_ref=${STABLE_REF:-origin/stable}
  if git rev-parse --verify --quiet "$stable_ref^{commit}" >/dev/null; then
    candidate=$(git rev-parse "$stable_ref^{commit}")
    if git merge-base --is-ancestor "$candidate" HEAD; then
      base=$candidate
    else
      echo "::notice::$stable_ref is not an ancestor of $HEAD_SHA; planning the full workspace"
    fi
  else
    echo "::notice::no $stable_ref to compare against; planning the full workspace"
  fi
  if [ -z "$base" ]; then
    selection_args+=(--full)
  fi
fi
if [ -z "$base" ] || [ "$base" = "$zero" ]; then
  # No base at all (an unborn or rewritten ref): compare with the parent.
  base=$(git rev-parse HEAD^ 2>/dev/null || git rev-parse HEAD)
fi
echo "base=$base" >> "$GITHUB_OUTPUT"
python3 tools/ci/affected_packages.py --base "$base" --shards 2 "${selection_args[@]}" \
  --github-output "$GITHUB_OUTPUT" | tee ci-plan.json
{
  echo '```json'
  cat ci-plan.json
  echo '```'
} >> "$GITHUB_STEP_SUMMARY"
# App size budgets are gated where the collection publishes: Release / apps
# measures every affected tapp on stable; t's app-sizes-<arch> checks measure
# each commit's. Packaging the affected apps here would repeat both, so the
# tapps job of the pre-promotion workflow is not part of this one.
printf 'tapp_apps=\ntapp_required=false\n' >> "$GITHUB_OUTPUT"
