#!/bin/bash
# Verify the snapshot pins a pull request changes, against this checkout.
#
#   os/dev/verify-snapshot-pins.sh os/kernel-snapshot.env
#
# Each argument names a pin file the pull request changed; only those pins
# are verified, because a pin the change does not touch is main's business.
#
# The pin-only fast path of check.yml runs this instead of the Rust and
# security suites: a pull request that changes only os/kernel-snapshot.env
# changes no code, so what needs proving is that each pinned reference is the
# one CI published and signed for this source.
#
# Frozen RPM snapshots resolve by package-set fingerprint
# (os/dev/resolve-rpm-snapshot.sh); there is no RPM pin file to verify here.
#
#   os/kernel-snapshot.env  each architecture's kernel passes
#                           os/dev/verify-kernel-provenance.sh.
#
# Needs cosign, skopeo, jq and registry read access. Exit 0 when every pin
# verifies, 1 otherwise.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo"
fail() { printf 'verify-snapshot-pins: %s\n' "$*" >&2; exit 1; }
[ "$#" -gt 0 ] || fail "name the changed pin files"
kernel=0
for pin in "$@"; do
  case "$pin" in
    os/kernel-snapshot.env) kernel=1 ;;
    os/rpms-snapshot.env)
      fail "os/rpms-snapshot.env is removed; RPM snapshots resolve by fingerprint" ;;
    *) fail "not a pin this check verifies: $pin" ;;
  esac
done
for tool in cosign skopeo jq; do
  command -v "$tool" >/dev/null || fail "$tool is required"
done

[ "$kernel" = 1 ] || exit 0
for key in KERNEL_RPMS_X86_64_REF KERNEL_RPMS_AARCH64_REF; do
  ref="$(sed -n "s/^$key=//p" os/kernel-snapshot.env)"
  [ -n "$ref" ] || continue
  case "$key" in
    KERNEL_RPMS_X86_64_REF) architecture=x86_64 ;;
    KERNEL_RPMS_AARCH64_REF) architecture=aarch64 ;;
  esac
  ./os/dev/verify-kernel-provenance.sh "$ref" --architecture "$architecture" >/dev/null \
    || fail "$key=$ref does not verify"
  echo "verify-snapshot-pins: $key $ref verifies"
done
