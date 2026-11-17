#!/usr/bin/env bash
# The server image before it is published: every program the machine runs
# answers, the jail helper is root-owned and runnable only by the service's
# group (its capabilities come from the unit, not the image), its configuration
# and the guest's signature policy are where they are read from, the boot and
# update units are enabled with this image's own health verdict and update
# window, and the API server is inert until an operator configures it.
#
# usage: host_image.sh ARCHIVE
# ARCHIVE is an OCI archive as t's image build writes it (the check image-host).
# It runs in the buildah image, without a daemon, as root in a micro-VM step.
set -euo pipefail
archive=${1:?usage: host_image.sh ARCHIVE}
. "$(dirname -- "${BASH_SOURCE[0]}")/../../tools/ci/publish/lib.sh"
use_buildah host-test
image=$(b pull --quiet "oci-archive:$archive")
container=$(b from --quiet "$image")
trap 'b rm "$container" >/dev/null 2>&1 || true' EXIT
root=$(b mount "$container")
inside() { b run "$container" -- "$@"; }

# Every Tilo image is Fedora, the OS included: a server trusts the same
# libraries and certificates the desktop does.
inside grep -qx 'ID=fedora' /etc/os-release || die 'the runtime is not a Fedora base'
[[ $(inside firecracker --version | head -1) == 'Firecracker v1.17.0' ]] || die 'not Firecracker 1.17.0'
# Each answer is read whole before it is searched: under pipefail, `grep -q`
# leaving at its first match can cut off the program mid-write and fail the
# check on an answer that was right.
grep -q 'v1.17.0' <<<"$(inside jailer --version)" || die 'not the jailer of Firecracker 1.17.0'
inside pasta --version >/dev/null || die 'no pasta'
inside skopeo --version >/dev/null || die 'no skopeo'
inside mkfs.xfs -V >/dev/null || die 'no mkfs.xfs'
inside setpriv --help >/dev/null || die 'no setpriv'
inside bootc --version >/dev/null || die 'no bootc'

# The programs, at the paths their units and scripts name.
for bin in usr/libexec/tilo/tilo-server usr/libexec/tilo/tilo-server-admin \
  usr/libexec/tilo/tilo-enroll usr/bin/fs usr/libexec/tilo/tilo-api \
  usr/libexec/tilo/tilo-legacy usr/libexec/tilo-esp-write \
  usr/libexec/tilo-image-policy-check usr/libexec/tilo-board-successor \
  usr/libexec/tilo-deployment usr/libexec/tilo-verified-update \
  usr/libexec/tilo-boot-health usr/libexec/tilo-server-health \
  usr/libexec/tilo-server-update-window usr/libexec/tilo-server-update-apply \
  usr/libexec/tilo-hosted-prepare usr/libexec/tilo-hosted-serve \
  usr/libexec/tilo-update-channel usr/libexec/tilo-update-policy-set \
  usr/libexec/tilo-image-trust-staged \
  usr/libexec/tilo-esp-mount; do
  [[ -x $root/$bin ]] || die "no executable /$bin"
done
# Scripts call these bare; the esp writer answers to the UKI editor's name too.
[[ -x $root/usr/bin/tilo-image-trust ]] || die 'tilo-image-trust is not on PATH for the boot tools'
[[ $(readlink "$root/usr/libexec/tilo-uki-cmdline") == tilo-esp-write ]] \
  || die 'no /usr/libexec/tilo-uki-cmdline link to the esp writer'

helper=$root/usr/libexec/tilo/tilo-hosted-jail
[[ $(stat -c '%u:%g %a' "$helper") == "0:10001 750" ]] \
  || die "the jail helper is $(stat -c '%u:%g %a' "$helper"), not root:tilo-hosted 0750"
inside getcap /usr/libexec/tilo/tilo-hosted-jail | grep -q cap_setuid \
  && die 'the image itself gives the jail helper capabilities; tilo-hosted.service does'

for file in etc/tilo/hosted-jail.json etc/tilo usr/share/tilo/cosign.pub \
  usr/share/tilo/release/rekor.pub; do
  [[ $(stat -c '%u' "$root/$file") == 0 ]] || die "/$file is not root-owned"
  (( ($(stat -c '%#a' "$root/$file") & 022) == 0 )) || die "/$file is writable by others"
done
grep -q '"service_uid": 10001' "$root/etc/tilo/hosted-jail.json" || die 'the jail serves another uid'
grep -q '"pasta": "/usr/bin/pasta"' "$root/etc/tilo/hosted-jail.json" || die 'the jail names another pasta'
[[ -x $root/usr/local/bin/firecracker ]] || die 'the jail names no firecracker at /usr/local/bin'
[[ -x $root/usr/local/bin/jailer ]] || die 'the jail names no jailer at /usr/local/bin'

grep -q 'use-sigstore-attachments: true' "$root/etc/containers/registries.d/registry.tilo.so.yaml" \
  || die "skopeo is not told where the guest's signature is"
guest_policy=$(grep -A3 '"registry.tilo.so/tilo/hosted-guest"' "$root/etc/containers/policy.json")
grep -q '"keyPath": "/usr/share/tilo/cosign.pub"' <<<"$guest_policy" || die 'the guest is not held to the release key'

# The accounts their units name.
grep -q '^tilo-hosted:x:10001:' "$root/etc/passwd" || die 'no tilo-hosted account at 10001'
grep -q '^tilo-api:x:65532:' "$root/etc/passwd" || die 'no tilo-api account at 65532'

# What boots, and what judges the boot: the server and the replica, the hosted-VM
# service, the API (inert), and the assessment that rolls a bad update back,
# judged by this image's own server probe.
wants=$root/etc/systemd/system
for unit in tilo-server.service tilo-fs-replica.service tilo-server-firstboot.service \
  tilo-hosted.service tilo-api.service; do
  [[ -L $wants/multi-user.target.wants/$unit ]] || die "$unit is not enabled"
done
for timer in tilo-boot-entries.timer bootc-fetch-apply-updates.timer \
  tilo-server-update-apply.timer; do
  [[ -L $wants/timers.target.wants/$timer ]] || die "$timer is not enabled"
done
[[ -L $wants/boot-complete.target.requires/tilo-boot-health.service ]] \
  || die 'tilo-boot-health is not required by boot-complete.target, so no boot is judged'
[[ -L $wants/multi-user.target.wants/tilo-boot-fallback.service ]] \
  || die 'tilo-boot-fallback is not enabled, so a fallback would leave bootc queued on the failed image'
grep -q 'Environment=TILO_BOOT_HEALTH_UNIT=tilo-server-health.service' \
  "$root/usr/lib/systemd/system/tilo-boot-health.service.d/10-tilo-server.conf" \
  || die 'boot health is not judged by the server probe'
grep -q '^ExecStartPre=+/usr/libexec/tilo-hosted-prepare' \
  "$root/usr/lib/systemd/system/tilo-hosted.service" \
  || die 'the hosted-VM service does not run its machine setup as root'
# Every VM's network opens /dev/net/tun, whose node exists only once the module
# is loaded; nothing else in the image loads it.
grep -qx 'tun' "$root/usr/lib/modules-load.d/tilo-hosted.conf" \
  || die "nothing loads the tun module its VMs' networks open"
grep -q '^ConditionPathExists=/etc/tilo/api.env' \
  "$root/usr/lib/systemd/system/tilo-api.service" \
  || die 'the API server does not wait for its configuration'
[[ ! -e $root/etc/tilo/api.env ]] || die 'the image carries an API configuration; the deployment owns it'

# An update stages (verified, counted) and is never applied blindly: the boot
# into it is this image's own step, inside the operator's window.
grep -qx 'ExecStart=/usr/libexec/tilo-verified-update --quiet' \
  "$root/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/20-tilo-update-policy.conf" \
  || die 'the update service applies images blindly'
[[ -f $root/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/15-tilo-server-window.conf ]] \
  || die 'the update service ignores the update window'
grep -q '^ExecStartPost=-/usr/libexec/tilo-deployment entries' \
  "$root/usr/lib/systemd/system/bootc-fetch-apply-updates.service.d/10-tilo-boot-entries.conf" \
  || die 'a staged update gets no try counter before its reboot'

# This image boots split kernel/initrd entries, which the boot-entry pass accepts
# only from an image that says which layout it boots; `server`, not `dev`, so
# nothing treats it as a development image.
[[ $(cat "$root/usr/lib/tilo/image-flavour") == server ]] || die 'the image does not name its boot layout'

# Nothing of a desktop's session is in it.
[[ ! -e $root/usr/bin/greetd && ! -e $root/usr/libexec/tilo/tilo-greeter ]] \
  || die 'the server image carries a sign-in stack'
echo 'the server image holds what a server runs'
