#!/usr/bin/env bash
# The VM host image before it is published: every program the host runs answers, the jail
# helper is root-owned, runnable only by the service's group and given exactly cap_setuid and
# cap_setgid by the start, its configuration is root-owned and parses as the helper reads it, and the guest's
# signature policy and keys are where fetch-guest's policy names them.
#
# usage: test-image.sh ARCHIVE
# ARCHIVE is an OCI archive as t's image build writes it (the check image-vm-host). It runs in
# the buildah image, without a daemon, as root in a micro-VM step.
set -euo pipefail
archive=${1:?usage: test-image.sh ARCHIVE}
. "$(dirname -- "${BASH_SOURCE[0]}")/../../../tools/ci/publish/lib.sh"
use_buildah vm-host-test
image=$(b pull --quiet "oci-archive:$archive")
container=$(b from --quiet "$image")
trap 'b rm "$container" >/dev/null 2>&1 || true' EXIT
root=$(b mount "$container")
inside() { b run "$container" -- "$@"; }

[[ $(inside firecracker --version | head -1) == 'Firecracker v1.17.0' ]] || die 'not Firecracker 1.17.0'
inside jailer --version | grep -q 'v1.17.0' || die 'not the jailer of Firecracker 1.17.0'
inside pasta --version >/dev/null || die 'no pasta'
inside skopeo --version >/dev/null || die 'no skopeo'
inside curl --version >/dev/null || die 'no curl for the placer'
inside tilo-legacy hosted help | grep -q -- 'place --listen' || die 'tilo-legacy hosted lacks place'
inside mkfs.xfs -V >/dev/null || die 'no mkfs.xfs'
inside tilo-legacy hosted help | grep -q -- '--evict-notice' || die 'tilo-legacy hosted lacks --evict-notice'
inside tilo-legacy hosted help | grep -q -- '--memory-mib' || die 'tilo-legacy hosted lacks --memory-mib'
# The start's own mkfs.xfs line, on a scratch file: the state disk's filesystem is made only
# on a node, the first time, so a bad option there would otherwise first fail in production.
mkfs=$(grep -E '^\s*mkfs\.xfs ' "$root/usr/local/bin/tilo-vm-host" | sed 's/"\$state_device"/\/tmp\/state.img/')
[[ -n $mkfs ]] || die 'the start makes no XFS'
inside sh -c "truncate -s 512M /tmp/state.img && $mkfs && rm /tmp/state.img" \
  || die "the start's mkfs.xfs fails: $mkfs"

helper=$root/usr/libexec/tilo/tilo-hosted-jail
[[ $(stat -c '%u:%g %a' "$helper") == "0:10001 750" ]] \
  || die "the jail helper is $(stat -c '%u:%g %a' "$helper"), not root:tilo-hosted 0750"
grep -qx 'setcap cap_setuid,cap_setgid+ep /usr/libexec/tilo/tilo-hosted-jail' \
  "$root/usr/local/bin/tilo-vm-host" || die 'the start does not give the jail helper its capabilities'
inside setcap -v cap_setuid,cap_setgid+ep /usr/libexec/tilo/tilo-hosted-jail >/dev/null 2>&1 \
  && die 'the image itself gives the jail helper capabilities'

for file in etc/tilo/hosted-jail.json etc/tilo usr/share/tilo/cosign.pub usr/share/tilo/release/rekor.pub; do
  [[ $(stat -c '%u' "$root/$file") == 0 ]] || die "/$file is not root-owned"
  (( ($(stat -c '%#a' "$root/$file") & 022) == 0 )) || die "/$file is writable by others"
done
grep -q '"service_uid": 10001' "$root/etc/tilo/hosted-jail.json" || die 'the jail serves another uid'
grep -q '"pasta": "/usr/bin/pasta"' "$root/etc/tilo/hosted-jail.json" || die 'the jail names another pasta'
grep -q 'use-sigstore-attachments: true' "$root/etc/containers/registries.d/registry.tilo.so.yaml" \
  || die 'skopeo is not told where the guest'"'"'s signature is'
grep -A3 '"registry.tilo.so/tilo/hosted-guest"' "$root/etc/containers/policy.json" \
  | grep -q '"keyPath": "/usr/share/tilo/cosign.pub"' || die 'the guest is not held to the release key'
echo 'the VM host image holds what a host runs'
