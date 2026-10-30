#!/usr/bin/env bash
# The select job of check.yml: find the comparison base and plan what the
# change affects (tools/ci/affected_packages.py). It runs from the tree under
# test, so the plan always follows that tree's rules rather than those of an
# older commit's workflow file.
#
# The base of a push is the head of the newest earlier push run on main whose
# window was measured (the `supersede` job's decision, tools/ci/push_window.py),
# so a run covers every commit whose own run never ran: cancelling a run that is
# waiting to start loses nothing. When that decision has no answer, the push's
# own `before` is the base and the plan covers exactly the commits the push
# added; when neither is usable the plan is the full workspace.
#
# Environment (from the workflow):
#   EVENT_NAME          github.event_name
#   PLAN_BASE           head of the newest earlier run on main whose window ran
#   PULL_REQUEST_BASE   the main commit a dispatch was merged onto
#   PUSH_BEFORE         the main commit the push landed on (github.event.before)
#   HEAD_SHA, GITHUB_OUTPUT, GITHUB_STEP_SUMMARY
set -euo pipefail

zero=0000000000000000000000000000000000000000
selection_args=()
base=''
if [ "$EVENT_NAME" = workflow_dispatch ]; then
  # Dispatch has no base commit: validate the complete workspace on both
  # native architectures instead of silently reducing the run to one diff.
  selection_args+=(--full)
  base=$(git rev-parse HEAD^ 2>/dev/null || git rev-parse HEAD)
else
  # A push to main, newest base first. A base is only usable when it is a commit
  # in this history: anything else (unknown, or not an ancestor after a rewrite)
  # has no diff to trust.
  for candidate in "${PLAN_BASE:-}" "${PULL_REQUEST_BASE:-}" "${PUSH_BEFORE:-}"; do
    case "$candidate" in '' | null | "$zero") continue ;; esac
    if git rev-parse --verify --quiet "$candidate^{commit}" >/dev/null \
      && git merge-base --is-ancestor "$candidate" HEAD; then
      base=$(git rev-parse "$candidate^{commit}")
      break
    fi
  done
  if [ -z "$base" ]; then
    echo "::notice::no usable base for $HEAD_SHA; planning the full workspace"
    selection_args+=(--full)
  fi
fi
if [ -z "$base" ]; then
  # No base at all (a dispatch, or an unborn or rewritten ref): compare with the
  # parent.
  base=$(git rev-parse HEAD^ 2>/dev/null || git rev-parse HEAD)
fi
echo "plan base: $base (PLAN_BASE=${PLAN_BASE:-none} before=${PUSH_BEFORE:-none})"
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
