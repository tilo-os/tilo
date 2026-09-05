#!/usr/bin/env bash
# Post one terminal Forgejo Actions result to Omnigent. Delivery is fail-open:
# Forgejo remains the source of truth if the agent control plane is unavailable.
set -u

conclusion=${1:-${CI_CONCLUSION:-}}
if [ -z "$conclusion" ] && [ -n "${CI_NEEDS_JSON:-}" ]; then
  results=$(printf '%s' "$CI_NEEDS_JSON" | jq -r '.[]?.result // empty' 2>/dev/null || true)
  if grep -qx failure <<<"$results"; then
    conclusion=failure
  elif grep -qx cancelled <<<"$results"; then
    conclusion=cancelled
  elif grep -qx success <<<"$results"; then
    conclusion=success
  else
    conclusion=skipped
  fi
fi
case "$conclusion" in
  success|failure|cancelled|skipped|timed_out|action_required|neutral) ;;
  *) conclusion=neutral ;;
esac

if [ -z "${OMNIGENT_CI_INGEST_TOKEN:-}" ] \
  || [ -z "${OMNIGENT_CI_URL:-}" ] \
  || [ -z "${GITHUB_REPOSITORY:-}" ] \
  || [ -z "${GITHUB_RUN_ID:-}" ]; then
  exit 0
fi

commit_sha=${GITHUB_SHA:-}
if [ -n "${GITHUB_EVENT_PATH:-}" ] && [ -f "$GITHUB_EVENT_PATH" ]; then
  event_sha=$(jq -r '.pull_request.head.sha // .after // empty' "$GITHUB_EVENT_PATH" 2>/dev/null || true)
  [ -z "$event_sha" ] || commit_sha=$event_sha
fi
case "$commit_sha" in
  [0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]*) ;;
  *) exit 0 ;;
esac

log_tail=${CI_LOG_TAIL:-}
if [ -n "${CI_LOG_FILE:-}" ] && [ -f "$CI_LOG_FILE" ]; then
  log_tail=$(tail -c 32768 "$CI_LOG_FILE" 2>/dev/null || true)
fi

run_url="${GITHUB_SERVER_URL%/}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}"
payload=$(jq -cn \
  --arg repository "$GITHUB_REPOSITORY" \
  --arg commit_sha "$commit_sha" \
  --arg run_id "$GITHUB_RUN_ID" \
  --argjson attempt "${GITHUB_RUN_ATTEMPT:-1}" \
  --arg workflow "${GITHUB_WORKFLOW:-CI}" \
  --arg conclusion "$conclusion" \
  --arg url "$run_url" \
  --arg log_tail "$log_tail" \
  '{repository:$repository, commit_sha:$commit_sha, run_id:$run_id,
    attempt:$attempt, workflow:$workflow, status:"completed",
    conclusion:$conclusion, url:$url, artifacts:[]}
   + if $log_tail == "" then {} else {log_tail:$log_tail} end') || exit 0

if ! curl --fail --silent --show-error \
  --retry 2 --retry-all-errors --connect-timeout 5 --max-time 20 \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $OMNIGENT_CI_INGEST_TOKEN" \
  --data-binary "$payload" \
  "${OMNIGENT_CI_URL%/}/v1/ci/results" >/dev/null; then
  printf 'warning: could not deliver CI result to the agent control plane\n' >&2
fi

exit 0
