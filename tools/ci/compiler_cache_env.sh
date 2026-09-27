#!/usr/bin/env bash
# Choose one complete compiler-cache configuration for this job.
#
# Prefer the values the runner injected. When that set is incomplete, use this
# repository's cache secret. That bucket is the one existing CI objects live
# in. A publication bind must exit before sourcing this script: the shared
# credential can write every object, so a signed build never receives it.
set -euo pipefail

ready=1
for name in SCCACHE_BUCKET SCCACHE_ENDPOINT SCCACHE_REGION SCCACHE_S3_USE_SSL AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY; do
  value="${!name:-}"
  if [[ -z $value || $value == *$'\n'* ]]; then
    ready=0
  fi
done

if [[ $ready -ne 1 && ( -z ${FALLBACK_AWS_ACCESS_KEY_ID:-} || -z ${FALLBACK_AWS_SECRET_ACCESS_KEY:-} ) ]]; then
  echo '::notice::no compiler cache credentials; compiling without the remote cache'
elif [[ $ready -ne 1 ]]; then
  # Do not mix a runner bucket with this repository's keys. They authorize
  # different buckets.
  export SCCACHE_BUCKET=tilo-ci-sccache-125650147312-us-west-2
  export SCCACHE_REGION=us-west-2
  export SCCACHE_ENDPOINT=https://s3.us-west-2.amazonaws.com
  export SCCACHE_S3_USE_SSL=true
  export SCCACHE_S3_KEY_PREFIX="${SCCACHE_S3_KEY_PREFIX:-forgejo-runners/v1}"
  export SCCACHE_IGNORE_SERVER_IO_ERROR="${SCCACHE_IGNORE_SERVER_IO_ERROR:-1}"
  export AWS_ACCESS_KEY_ID="$FALLBACK_AWS_ACCESS_KEY_ID"
  export AWS_SECRET_ACCESS_KEY="$FALLBACK_AWS_SECRET_ACCESS_KEY"
  echo "::notice::runner compiler cache is incomplete; using the repository cache at ${SCCACHE_ENDPOINT}"
else
  echo "::notice::using the runner compiler cache at ${SCCACHE_ENDPOINT}"
fi

if [[ -z ${SCCACHE_BUCKET:-} || -z ${AWS_ACCESS_KEY_ID:-} ]]; then
  return 0 2>/dev/null || exit 0
fi

echo "::add-mask::${AWS_ACCESS_KEY_ID}"
echo "::add-mask::${AWS_SECRET_ACCESS_KEY}"

if [[ -n ${GITHUB_ENV:-} ]]; then
  {
    printf 'SCCACHE_BUCKET=%s\n' "$SCCACHE_BUCKET"
    printf 'SCCACHE_REGION=%s\n' "$SCCACHE_REGION"
    printf 'SCCACHE_ENDPOINT=%s\n' "$SCCACHE_ENDPOINT"
    printf 'SCCACHE_S3_USE_SSL=%s\n' "$SCCACHE_S3_USE_SSL"
    printf 'SCCACHE_S3_KEY_PREFIX=%s\n' "${SCCACHE_S3_KEY_PREFIX:-}"
    printf 'SCCACHE_IGNORE_SERVER_IO_ERROR=%s\n' "${SCCACHE_IGNORE_SERVER_IO_ERROR:-1}"
    printf 'AWS_ACCESS_KEY_ID=%s\n' "$AWS_ACCESS_KEY_ID"
    printf 'AWS_SECRET_ACCESS_KEY=%s\n' "$AWS_SECRET_ACCESS_KEY"
  } >> "$GITHUB_ENV"
fi
