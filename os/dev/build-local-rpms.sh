#!/usr/bin/env bash
# Build the frozen RPM snapshot for this checkout's package set locally, exactly
# as the snapshot-rpms workflow does, but unsigned and for the native
# architecture only. Prints the RPMS_REF to build with:
#
#   RPMS_REF=$(os/dev/build-local-rpms.sh) TILO_RPMS_TRUST=unsigned ./os/build.sh
#
# The tag is the package set's hash, so an unchanged package set reuses the
# snapshot already in local storage. Use it when a change adds packages that no
# published snapshot carries yet; CI still builds and signs the snapshot after
# the change lands on main.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"
set -euo pipefail

# shellcheck disable=SC1091
. "$TILO_OS_DIR/base-snapshot.env"
[ -n "${BASE_REF:-}" ] || die "os/base-snapshot.env pins no BASE_REF"

packages="$("$TILO_OS_DIR/dev/derive-rpm-package-set.py" --arch "$(uname -m)" | LC_ALL=C sort -u)"
[ -n "$packages" ] || die "the Containerfiles declare no packages"
set_hash="$(printf '%s\n' "$packages" | sha256sum | cut -c1-16)"
ref="localhost/tilo-rpms:$(uname -m)-$set_hash"

if podman image exists "$ref"; then
  msg "reusing $ref (package set unchanged)" >&2
else
  "$TILO_OS_DIR/dev/cache-github-assets.sh" >&2
  msg "building $ref from $BASE_REF ($(printf '%s\n' "$packages" | wc -l) packages)" >&2
  podman build -f "$TILO_OS_DIR/Containerfile.rpms" \
    --build-context "github-assets=$TILO_OS_DIR/external-assets/cache" \
    --build-arg "BASE_REF=$BASE_REF" \
    --build-arg "PACKAGES=$(printf '%s ' $packages)" \
    --label "org.tilo.rpms.trust=unsigned" \
    --tag "$ref" \
    "$TILO_REPO" >&2
fi
printf '%s\n' "$ref"
