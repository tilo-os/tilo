#!/usr/bin/env bash
# Source this from publish-collections.sh after defining fetch_snapshot.

# An accepted provider is not necessarily in the owner-authorized signed
# catalogue yet. Retry only the package CLI's typed exact-runtime refusal;
# every attempt re-verifies the newly fetched snapshot before it may produce
# a signed app entry. One caller-owned deadline covers the whole publication
# phase: two collector opportunities (10 minutes each), even if multiple apps
# or architectures wait. An actually missing provider remains a visible failure.
await_registry_runtime_ready() {
  local deadline=$1 output=$2 refusal=$3
  shift 3
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
