#!/usr/bin/env bash
# Check the member names of an app collection archive (`tar -t` output on
# stdin) against the one allowlist of what a collection may contain.
#
#   tar -tzf apps.tar.gz | check-collection-members.sh [--require-tapp] LABEL
#
# LABEL names the archive in the message ("unexpected LABEL member: ...").
# With --require-tapp, a collection without a single tapp is refused too.
#
# The publication that reuses the prior collection
# (tools/reuse_prior_app_collection.sh) reads a published collection with this.

set -euo pipefail

require_tapp=0
if [[ ${1:-} == --require-tapp ]]; then
  require_tapp=1
  shift
fi
label=${1:?usage: check-collection-members.sh [--require-tapp] LABEL < members}

component='[A-Za-z0-9][A-Za-z0-9._+-]*'
# Variable font filenames carry OpenType axis tags in brackets. Permit that
# syntax only for font files in the runtime's font directory, retaining the
# strict component allowlist for every other archive member.
font_member="^runtime/tilo-runtime/$component/lib/fonts/$component\\[[A-Za-z]{4}(,[A-Za-z]{4})*\\]\\.(ttf|otf)\$"
tapp_member="^tapp/$component\\.tapp\$"
runtime_member="^runtime(/$component)+/?\$"

tapps=0
while IFS= read -r member; do
  if [[ $member == tapp/ || $member == runtime/ || $member == index.json || $member == SHA256SUMS \
        || $member =~ $runtime_member || $member =~ $font_member ]]; then
    continue
  fi
  if [[ $member =~ $tapp_member ]]; then
    tapps=$((tapps + 1))
    continue
  fi
  printf 'unexpected %s member: %s\n' "$label" "$member" >&2
  exit 1
done

if [[ $require_tapp == 1 && $tapps == 0 ]]; then
  printf 'the %s holds no tapp\n' "$label" >&2
  exit 1
fi
