#!/usr/bin/env bash
# The select job of check.yml: find the comparison base and plan what the
# change affects (tools/ci/affected_packages.py). It runs from the tree under
# test, so the plan always follows that tree's rules rather than those of an
# older commit's workflow file.
#
# The base of a push is the main commit it was pushed onto (the event's
# `before`), so the plan covers exactly the commits that push added; every push
# gets its own run.
#
# Environment (from the workflow):
#   EVENT_NAME          github.event_name
#   PULL_REQUEST_BASE   the main commit a dispatch was merged onto
#   PUSH_BEFORE         the main commit the push landed on (github.event.before)
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
  # A push to main: compare with the commit it landed on. When that commit is
  # unknown or not an ancestor (a new branch, or history was rewritten), there
  # is no diff to trust and the plan is the full one.
  base=''
  before=${PUSH_BEFORE:-}
  if [ -n "$before" ] && [ "$before" != "$zero" ] \
    && git rev-parse --verify --quiet "$before^{commit}" >/dev/null \
    && git merge-base --is-ancestor "$before" HEAD; then
    base=$(git rev-parse "$before^{commit}")
  else
    echo "::notice::no usable push base for $HEAD_SHA; planning the full workspace"
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
