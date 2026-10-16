#!/usr/bin/env bash
# Source this from publish-collections.sh after setting registry_url and snapshot.

fetch_snapshot() {
  curl --fail --silent --show-error --connect-timeout 10 --max-time 30 \
    --retry 3 --retry-all-errors "$registry_url/entries" --output "$snapshot" || return $?
  jq -e '.entries | type == "array"' "$snapshot" >/dev/null
}

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
  local missing remaining
  while :; do
    rm -f -- "$refusal" "$error_file" || return $?
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
    remaining=$((deadline - SECONDS))
    if (( remaining <= 0 )); then
      printf 'exact runtime still absent from the signed catalogue after 20 minutes: %s\n' "$missing" >&2
      return 1
    fi
    if (( remaining < 60 )); then
      sleep "$remaining" || return $?
    else
      sleep 60 || return $?
    fi
    fetch_snapshot || return $?
    if (( SECONDS >= deadline )); then
      printf 'exact runtime still absent from the signed catalogue after 20 minutes: %s\n' "$missing" >&2
      return 1
    fi
  done
}
