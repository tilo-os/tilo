#!/usr/bin/env bash
# Candidate inputs can produce different images from the same source revision.
# Keep that run's base, dev and board tags together and distinct from normal CI.
set -euo pipefail
[[ $# == 1 ]] || { echo 'usage: dev-image-cohort.sh SERIES' >&2; exit 2; }
series="$1"
cohort="${TILO_DEV_INPUT_COHORT:-}"
if [[ -n $series && -n $cohort ]]; then
  [[ $series =~ ^[A-Za-z0-9_.-]+$ && $cohort =~ ^[1-9][0-9]{0,19}$ ]] \
    || { echo 'invalid dev image cohort' >&2; exit 1; }
  # Leave space for an architecture or board suffix within OCI's tag limit.
  printf '%s-w%s\n' "${series:0:80}" "$cohort"
else
  printf '%s\n' "$series"
fi
