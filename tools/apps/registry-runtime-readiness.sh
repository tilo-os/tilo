#!/usr/bin/env bash
# Source this from publish-collections.sh after defining fetch_snapshot.

# An accepted provider is not necessarily in the owner-authorized signed
# catalogue yet. Retry only the package CLI's typed exact-runtime refusal;
# every attempt re-verifies the newly fetched snapshot before it may produce
# a signed app entry. Two collector opportunities (10 minutes each) bound the
# wait, and an actually missing provider remains a visible failure.
await_registry_runtime_ready() {
  local output=$1 refusal=$2
  shift 2
  local deadline=$((SECONDS + 1200))
  local error_file="${refusal}.stderr"
  local missing
  while :; do
    rm -f -- "$refusal" "$error_file"
    if "$@" > "$output" 2> "$error_file"; then
      cat "$error_file" >&2
      rm -f -- "$error_file"
      return 0
    fi
    if [[ ! -s $refusal ]]; then
      cat "$error_file" >&2
      return 1
    fi
    missing=$(jq -er '
      select((.package_id | type) == "string"
        and (.version | type) == "string"
        and (.architecture | type) == "string"
        and (.runtime | type) == "string")
      | "\(.package_id)@\(.version) (\(.architecture)) needs \(.runtime)"
    ' "$refusal") || {
      cat "$error_file" >&2
      return 1
    }
    if (( SECONDS >= deadline )); then
      printf 'exact runtime still absent from the signed catalogue after 20 minutes: %s\n' "$missing" >&2
      cat "$error_file" >&2
      return 1
    fi
    printf 'waiting for signed-catalogue promotion: %s\n' "$missing" >&2
    sleep 60
    fetch_snapshot || return 1
  done
}
