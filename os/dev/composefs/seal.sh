#!/bin/bash
# Build and install one generation, working around bootc#2334.
#
#   ./os/dev/composefs/seal.sh [GENERATION]
#
# `bootc container ukify` computes a composefs digest for the rootfs and seals it
# into the UKI's command line. `bootc install` then recomputes the digest of the
# image it pulled and refuses the UKI if they differ:
#
#   The UKI has the wrong composefs= parameter
#   (is 'sha512:<A>', should be sha512:<B>)
#
# They do differ. So this does two passes: install once to learn the digest
# install wants, rebuild the UKI sealing that value, install again. Whether the
# second pass succeeds is the finding — if install's expected digest is stable
# when the UKI changes, sealing works today with a two-pass build; if it moves
# each time, the seal is circular and cannot be closed without an upstream fix.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/common.sh"

GEN="${1:-1}"
LOG="$TILO_CFS_RUN/seal-gen$GEN.log"
mkdir -p "$TILO_CFS_RUN"

# The digest install expects, out of the mismatch message.
expected_digest() {
  composefs_expected_digest "$1"
}
# The digest the UKI actually carries, so both can be reported.
sealed_digest() {
  composefs_sealed_digest "$1"
}

msg "pass 1: build generation $GEN with the digest bootc computes for the rootfs"
bash "$HERE/build.sh" "$GEN" 2>&1 | tee "$LOG.build1" | tail -3

msg "pass 1: install (expected to be refused, to learn the digest install wants)"
if bash "$HERE/install.sh" "$GEN" 2>&1 | tee "$LOG.install1" | tail -5; then
  msg "pass 1 installed — bootc#2334 did not bite, no second pass needed"
  exit 0
fi

want="$(expected_digest "$LOG.install1")"
had="$(sealed_digest "$LOG.install1")"
[ -n "$want" ] || { warn "install failed for some other reason:"; tail -20 "$LOG.install1" >&2; exit 1; }

msg "UKI carries    sha512:${had:0:32}…"
msg "install wants  sha512:${want:0:32}…"

msg "pass 2: rebuild sealing the digest install asked for"
TILO_CFS_DIGEST="$want" bash "$HERE/build.sh" "$GEN" 2>&1 | tee "$LOG.build2" | tail -3

msg "pass 2: install"
if bash "$HERE/install.sh" "$GEN" 2>&1 | tee "$LOG.install2" | tail -5; then
  msg "pass 2 installed: install's expected digest is STABLE across a UKI change,"
  msg "so a sealed image is buildable today with a two-pass build."
  exit 0
fi

want2="$(expected_digest "$LOG.install2")"
if [ -n "$want2" ] && [ "$want2" != "$want" ]; then
  warn "install now wants sha512:${want2:0:32}… — a DIFFERENT value."
  warn "The expected digest depends on the UKI, so it changes every time the UKI"
  warn "does. The seal is circular and cannot be closed without an upstream fix."
else
  warn "pass 2 failed:"; tail -20 "$LOG.install2" >&2
fi
exit 1
