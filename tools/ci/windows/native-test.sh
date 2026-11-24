#!/usr/bin/env bash
# Run the daemon's platform tests on Windows itself, all of them: what Wine
# cannot show (wine-skip.txt) included. Pipe access lists, the first pipe
# instance, identity storage's private directories and apps confined in their
# AppContainers are proven here alone.
#
#   native-test.sh <runtime-dir>   the platform-tests/*.exe the Windows build made
set -euo pipefail

runtime="${1:?the directory the Windows runtime build wrote}"
found=0
for tests in "$runtime"/platform-tests/*.exe; do
  [ -e "$tests" ] || continue
  found=1
  printf 'running %s\n' "$(basename -- "$tests")"
  "$tests" --test-threads 1 --nocapture
done
[ "$found" = 1 ] || { printf 'no platform tests under %s\n' "$runtime" >&2; exit 1; }
