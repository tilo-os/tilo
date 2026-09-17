#!/usr/bin/env bash
# Release-image contract for shell-owned remote capture backends.
set -euo pipefail

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd -- "$here/../.." && pwd)"
containerfile="$repo/os/Containerfile"

fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

for package in sane-backends sane-backends-drivers-scanners; do
  if grep -q "^    $package \\\\$" "$containerfile"; then
    ok "$package is part of the immutable scanner source"
  else
    bad "$package is absent while the shell advertises remote scanner support"
  fi
done

# Keep the image contract tied to the production probe rather than merely
# proving that an unrelated scanner package happens to be installed.
source_adapter="$repo/shell/src/shell/remote_capture_source.rs"
if grep -Fq 'trusted_command("scanimage")' "$source_adapter"; then
  ok 'remote scanner discovery requires the packaged scanimage binary'
else
  bad 'remote scanner discovery no longer matches the image contract'
fi

exit "$fail"
