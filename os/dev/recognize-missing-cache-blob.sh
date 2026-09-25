#!/usr/bin/env bash
# Decide whether the most recent build on a Buildx builder failed only because
# an imported registry cache names a layer blob the registry no longer serves.
#
# BuildKit imports cache manifests lazily: a hit records the layer descriptor
# and fetches its content only when a later step needs it. A manifest whose
# blob was lost (a full registry volume, an interrupted export) therefore
# passes the import and fails the build hours later with
#   could not fetch content descriptor sha256:... from remote: not found
# That is a cache miss, not a build failure. Exit 0 for exactly that error so
# the caller rebuilds without cache-from and re-exports a complete cache;
# every other failure stays fatal.
set -euo pipefail

builder=${1:?usage: recognize-missing-cache-blob.sh BUILDER}
message=$(docker buildx history inspect --builder "$builder" --format json \
  | jq -r '.Error.Message // empty')
pattern='could not fetch content descriptor (sha256:[0-9a-f]{64}) .* from remote: not found'
if [[ $message =~ $pattern ]]; then
  printf '::warning::the registry layer cache references missing blob %s; rebuilding without imported cache\n' \
    "${BASH_REMATCH[1]}"
  exit 0
fi
printf 'the build did not fail on a missing cache blob: %s\n' \
  "${message:-no error was recorded}" >&2
exit 1
