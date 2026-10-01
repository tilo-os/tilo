#!/bin/bash
# The seal loop reads both digests out of bootc install's composefs refusal, in
# the message format of current bootc and of the release before it.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck disable=SC1091
source "$repo/os/dev/_common.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

sealed="$(printf '0%.0s' {1..128})"
expected="941dd6c26b806a1db8f0fa863246348e80c1f38ad730bd832e809dbdc43753c5e2fe67615f21c88e7cea749eb20d5e18194fab78e2748dd8ca5785eeb41ec6c4"

check() {
  local name=$1 log=$2 want_expected=$3 want_sealed=$4 got
  got="$(composefs_expected_digest "$log")"
  [ "$got" = "$want_expected" ] || { echo "FAIL $name: expected digest '$got'" >&2; exit 1; }
  got="$(composefs_sealed_digest "$log")"
  [ "$got" = "$want_sealed" ] || { echo "FAIL $name: sealed digest '$got'" >&2; exit 1; }
  echo "ok $name"
}

cat >"$work/current.log" <<EOF
Installing bootloader via systemd-boot
error: Installing to filesystem: Setting up composefs boot: The UKI has the wrong composefs= parameter (is '$sealed', should be '$expected')
EOF
check "current bootc (quoted, unprefixed)" "$work/current.log" "$expected" "$sealed"

cat >"$work/prefixed.log" <<EOF
error: Installing to filesystem: Setting up composefs boot: The UKI has the wrong composefs= parameter (is 'sha512:$sealed', should be sha512:$expected)
EOF
check "earlier bootc (sha512: prefix)" "$work/prefixed.log" "$expected" "$sealed"

printf 'error: bootc install failed for another reason\n' >"$work/other.log"
check "an unrelated failure" "$work/other.log" "" ""
check "a missing log" "$work/absent.log" "" ""

cat >"$work/short.log" <<EOF
The UKI has the wrong composefs= parameter (is '${sealed:0:64}', should be '${expected:0:64}')
EOF
check "a truncated digest" "$work/short.log" "" ""
