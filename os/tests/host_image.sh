#!/usr/bin/env bash
# The server image before it is published: every program the machine runs
# answers, the jail helper is root-owned and runnable only by the service's
# group (its capabilities come from the unit, not the image), its configuration
# and the guest's signature policy are where they are read from, the boot and
# update units are enabled with this image's own health verdict and update
# window, and the API server is inert until an operator configures it.
#
# With --dev, the development server image (os/Containerfile.host-dev, sealed by
# seal-host-dev) is held to all of that but the one difference it is for:
# systemd-sysext merges layers at boot. Beside the server it carries the
# runtime that runs the device's steps and the dev-VM CLI, the component
# manifest the steps read, their boot units, and a
# base.json naming the check image, target, toolchain and Cargo.lock the
# server's own programs were built with.
#
# usage: host_image.sh [--dev] ARCHIVE TOOLS
# ARCHIVE is an OCI archive as t's image build writes it (the check image-host,
# or seal-host-dev with --dev). TOOLS is the OS tools image os-tools-ARCH built, as
# an OCI layout (os/image/build-layer.sh), whose bootc the server image must carry.
# It runs in the buildah image, without a daemon, as root in a micro-VM step.
set -euo pipefail
dev=0
if [[ ${1:-} == --dev ]]; then
  dev=1
  shift
fi
archive=${1:?usage: host_image.sh [--dev] ARCHIVE TOOLS}
tools=${2:?usage: host_image.sh [--dev] ARCHIVE TOOLS}
. "$(dirname -- "${BASH_SOURCE[0]}")/../../tools/ci/publish/lib.sh"
use_buildah host-test
image=$(b pull --quiet "oci-archive:$archive")
container=$(b from --quiet "$image")
trap 'b rm "$container" >/dev/null 2>&1 || true' EXIT
root=$(b mount "$container")
inside() { b run "$container" -- "$@"; }

# The bootc on the image is the OS tools image's build, not the base's stock binary:
# a server stages its updates with it and may lose power in between, and the base's
# release then leaves boot entries every later `bootc status` fails to parse. The
# content is compared with the tools image's own, so the image is held to the build
# the OS image ships and not to what its own build says it installed.
[[ -f $tools/image-tool-stages/oci/index.json ]] \
  || die "no image-tool-stages layout in $tools: it is not an os-tools build"
tools_image=$(b pull --quiet "oci:$tools/image-tool-stages/oci:tilo")
tools_container=$(b from --quiet "$tools_image")
trap 'b rm "$container" >/dev/null 2>&1 || true; b rm "$tools_container" >/dev/null 2>&1 || true' EXIT
tools_root=$(b mount "$tools_container")
[[ -f $tools_root/bootc/usr/bin/bootc ]] \
  || die "the OS tools image in $tools carries no bootc"
cmp -s "$tools_root/bootc/usr/bin/bootc" "$root/usr/bin/bootc" \
  || die 'the image ships another bootc than the one the OS tools stage built'
[[ -f $root/usr/share/licenses/tilo-bootc/NOTICE ]] \
  || die "the image ships no record of the bootc it carries, so that is the base's stock binary"
b umount "$tools_container" >/dev/null 2>&1 || true
b rm "$tools_container" >/dev/null 2>&1 || true

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
# It runs on this image's own libraries, and lints the image it ships in, as the
# update units and the boot-entry maintenance that call it do on an installed server.
if ! linked=$(inside ldd /usr/bin/bootc 2>&1) || grep -q 'not found' <<<"$linked"; then
  printf '%s\n' "$linked" >&2
  die "bootc's libraries are not all on this image"
fi
if ! lint=$(inside bootc container lint 2>&1); then
  printf '%s\n' "$lint" >&2
  die 'the bootc the image carries does not lint it'
fi
inside efibootmgr --version >/dev/null || die 'no efibootmgr, so a server cannot change its boot order'

# The programs, at the paths their units and scripts name.
for bin in usr/libexec/tilo/tilo-runtime \
  usr/bin/fs usr/libexec/tilo/tilo-api \
  usr/libexec/tilo/tilolang-process \
  usr/libexec/tilo/tilo-linux-vm usr/libexec/tilo-esp-write \
  usr/libexec/tilo-image-policy-check usr/libexec/tilo-board-successor \
  usr/libexec/tilo-deployment usr/libexec/tilo-verified-update \
  usr/libexec/tilo-boot-health usr/libexec/tilo-server-health \
  usr/libexec/tilo-server-update-window usr/libexec/tilo-server-update-apply \
  usr/libexec/tilo-hosted-prepare usr/libexec/tilo-hosted-serve \
  usr/libexec/tilo-update-channel usr/libexec/tilo-update-policy-set \
  usr/libexec/tilo-server-update-policy usr/libexec/tilo-server-control \
  usr/libexec/tilo-default-start \
  usr/libexec/tilo-server-device-config \
  usr/libexec/tilo-image-trust-staged \
  usr/libexec/tilo-esp-mount usr/lib/systemd/system-generators/tilo-esp-generator; do
  [[ -x $root/$bin ]] || die "no executable /$bin"
done
# The program the API server starts a task's tilolang programs in lies beside it, where the
# server looks for it, and starts on this base: handed no api it says so in its own words,
# which a missing library would not let it.
if said=$(inside /usr/libexec/tilo/tilolang-process 2>&1); then
  die 'tilolang-process starts with no api handed to it'
fi
grep -q 'tilolang-process: exiting: no api' <<<"$said" \
  || die "tilolang-process on the server image's base said: $said"
# Scripts call these bare; the esp writer answers to the UKI editor's name too.
[[ -x $root/usr/bin/tilo-image-trust ]] || die 'tilo-image-trust is not on PATH for the boot tools'
[[ $(readlink "$root/usr/libexec/tilo-uki-cmdline") == tilo-esp-write ]] \
  || die 'no /usr/libexec/tilo-uki-cmdline link to the esp writer'

# Opening the encrypted root in early boot. The installed machine's initramfs is
# generated from the image's dracut modules and the programs they start, so the
# server image ships the desktop's unlock chain rather than a copy of it.
for bin in usr/libexec/tilo/tilo-auth usr/libexec/tilo/tilo-boot-signin \
  usr/libexec/tilo/tilo-rollback \
  usr/libexec/tilo-disk-encrypt usr/libexec/tilo-store-volume \
  usr/libexec/tilo-rollback-initrd-unlock usr/libexec/tilo-root-discover \
  usr/share/tilo/tpm2-pcr-signing.public.pem; do
  [[ -e $root/$bin ]] || die "no /$bin, so an encrypted root cannot be opened"
done
for module in 91tilo-rollback; do
  [[ -f $root/usr/lib/dracut/modules.d/$module/module-setup.sh ]] \
    || die "the image carries no $module, so its initramfs cannot unlock the root"
done
[[ -d $root/usr/share/tilo/pcr-predictions ]] \
  || die 'no /usr/share/tilo/pcr-predictions, where a signed PCR prediction is read from'
# The initramfs the UKI below carries was built with the unlock module: without
# it, the kernel sees a LUKS device it cannot open.
grep -qx tilo-rollback "$root/usr/lib/tilo/initramfs-modules.txt" \
  || die 'the initramfs was not built with the unlock module (91tilo-rollback)'
for t in cryptsetup tpm2_createprimary; do
  inside sh -c "command -v $t" >/dev/null || die "no $t, which the early-boot unlock runs"
done

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

# What boots, and what judges the boot: the server, the hosted-VM
# service, the API (inert), and the assessment that rolls a bad update back,
# judged by this image's own server probe.
wants=$root/etc/systemd/system
for unit in tilo-server.service tilo-server-firstboot.service tilo-server-unlock.service \
  tilo-hosted.service tilo-api.service; do
  [[ -L $wants/multi-user.target.wants/$unit ]] || die "$unit is not enabled"
done
for timer in tilo-boot-entries.timer bootc-fetch-apply-updates.timer \
  tilo-server-update-apply.timer tilo-rollback-harden.timer tilo-rollback-confirm.timer; do
  [[ -L $wants/timers.target.wants/$timer ]] || die "$timer is not enabled"
done
[[ -L $wants/boot-complete.target.requires/tilo-boot-health.service ]] \
  || die 'tilo-boot-health is not required by boot-complete.target, so no boot is judged'
[[ -L $wants/multi-user.target.wants/tilo-boot-fallback.service ]] \
  || die 'tilo-boot-fallback is not enabled, so a fallback would leave bootc queued on the failed image'
grep -q 'Environment=TILO_BOOT_HEALTH_UNIT=tilo-server-health.service' \
  "$root/usr/lib/systemd/system/tilo-boot-health.service.d/10-tilo-server.conf" \
  || die 'boot health is not judged by the server probe'
# The server's verdict asks the daemon a status call, as the daemon's own account and in its
# environment: the socket file a native endpoint leaves behind outlives the daemon, and the
# daemon's 0700 directories are not even readable by root without capabilities.
grep -q 'status --daemon' "$root/usr/libexec/tilo-server-health" \
  || die 'the server health check does not ask the daemon a status call'
grep -q '^User=tilo-server$' "$root/usr/lib/systemd/system/tilo-server-health.service" \
  || die 'the server health check does not run as the daemon account'
# The disk unlock is asked of the daemon as its own account, the way the health check asks it,
# while the volume's header stays root's.
grep -q '^User=tilo-server$' "$root/usr/lib/systemd/system/tilo-server-unlock.service" \
  || die 'the server unlock does not ask the daemon as the daemon account'
grep -q '^ExecStartPre=+/usr/libexec/tilo-server-unlock prepare$' \
  "$root/usr/lib/systemd/system/tilo-server-unlock.service" \
  || die 'the server unlock does not add its key slot as root'
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
# A server's updates and window are its document's, applied by a root
# helper that only the request file starts.
[[ -L $wants/multi-user.target.wants/tilo-server-update-policy.path ]] \
  || die "nothing applies the update settings a server's document asks for"
grep -q '^ExecStart=/usr/libexec/tilo-server-update-policy$' \
  "$root/usr/lib/systemd/system/tilo-server-update-policy.service" \
  || die 'the update-policy service runs something else'
[[ $(stat -c '%u' "$root/usr/libexec/tilo-server-update-policy") == 0 ]] \
  || die 'the update-policy helper is not root-owned'
[[ ! -e $root/usr/libexec/tilo-update-policy.request ]] || die 'the image ships an update-policy request'
# What a server's owner asks it to do from another of their devices is carried
# out by another root-run helper, started by its request file alone.
[[ -L $wants/multi-user.target.wants/tilo-server-control.path ]] \
  || die "nothing carries out what a server's owner asks it to do"
grep -q '^ExecStart=/usr/libexec/tilo-server-control$' \
  "$root/usr/lib/systemd/system/tilo-server-control.service" \
  || die 'the control service runs something else'
[[ $(stat -c '%u' "$root/usr/libexec/tilo-server-control") == 0 ]] \
  || die 'the control helper is not root-owned'
# The machine's own configuration — hostname, drives, network — is its
# document's too, applied by a root helper only the request starts.
[[ -L $wants/multi-user.target.wants/tilo-server-device-config.path ]] \
  || die "nothing applies the machine configuration a server's document asks for"
grep -q '^ExecStart=/usr/libexec/tilo-server-device-config$' \
  "$root/usr/lib/systemd/system/tilo-server-device-config.service" \
  || die 'the device-config service runs something else'
[[ $(stat -c '%u' "$root/usr/libexec/tilo-server-device-config") == 0 ]] \
  || die 'the device-config helper is not root-owned'
[[ -x $root/usr/libexec/tilo-server-device-config ]] || die 'the device-config helper is not executable'

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

# `server`, not `dev`, so nothing treats it as a development image.
[[ $(cat "$root/usr/lib/tilo/image-flavour") == server ]] || die 'the image does not say it is a server'

# The image is sealed: one UKI under /boot carries the kernel, the initramfs and
# the command line, and the rootfs keeps neither of the first two, so nothing
# boots it but the UKI. Its command line names the generation the image carries,
# the composefs digest it mounts and the hardening arguments.
shopt -s nullglob
ukis=("$root"/boot/EFI/Linux/*.efi)
shopt -u nullglob
(( ${#ukis[@]} == 1 )) || die "the image carries ${#ukis[@]} UKIs under /boot/EFI/Linux, not one"
leftover=$(cd "$root" && find usr/lib/modules -maxdepth 2 \( -name vmlinuz -o -name initramfs.img \) | sort)
[[ -z $leftover ]] || die "the rootfs still carries a kernel or initramfs beside the UKI: $leftover"
cmdline=$(inside /usr/libexec/tilo-uki-cmdline show "/boot/EFI/Linux/${ukis[0]##*/}")
generation=$(cat "$root/usr/lib/tilo/generation")
[[ $generation =~ ^[1-9][0-9]*$ ]] || die "the image carries no generation: '$generation'"
for karg in "tilo.generation=$generation" root=tilo-auto lockdown=integrity init_on_alloc=1 \
  slab_nomerge vsyscall=none systemd.import_credentials=no console=ttyS0,115200; do
  grep -qw -- "$karg" <<<"$cmdline" || die "the sealed command line does not carry $karg: $cmdline"
done
grep -Eq '(^| )composefs=[0-9a-f]{128}( |$)' <<<"$cmdline" \
  || die "the sealed command line names no composefs digest: $cmdline"

# Boot hardening besides: no extension merged over /usr at boot, nobody signing
# in at a console, sudo the only setuid program, no listener of the base's left
# and no port the firewall opens for one.
masked=(systemd-confext.service systemd-sysext.socket avahi-daemon.service avahi-daemon.socket)
(( dev )) || masked+=(systemd-sysext.service)
for unit in "${masked[@]}"; do
  [[ $(readlink "$root/etc/systemd/system/$unit") == /dev/null ]] || die "$unit is not masked"
done
unlocked=$(awk -F: '$2 !~ /^[!*]/ { print $1 }' "$root/etc/shadow")
[[ -z $unlocked ]] || die "accounts a console would sign in: $unlocked"
setuid=$(cd "$root" && find usr -xdev -type f -perm /6000 | sort)
[[ $setuid == usr/bin/sudo ]] || die "setuid or setgid programs besides sudo: $setuid"
for pkg in systemd-resolved systemd-networkd systemd-coredump; do
  if inside rpm -q "$pkg" >/dev/null 2>&1; then die "$pkg is installed"; fi
done
[[ $(readlink "$root/etc/resolv.conf") == ../run/NetworkManager/resolv.conf ]] \
  || die 'the resolver file is not the one NetworkManager writes'
[[ $(inside firewall-offline-cmd --zone=FedoraServer --list-services) == dhcpv6-client ]] \
  || die 'the firewall opens a service besides dhcpv6-client'

# Nothing of a desktop's session is in it.
[[ ! -e $root/usr/bin/greetd && ! -e $root/usr/libexec/tilo/tilo-greeter ]] \
  || die 'the server image carries a sign-in stack'
if (( ! dev )); then
  [[ ! -e $root/usr/libexec/tilo/tilo-vm && ! -e $root/usr/share/tilo-vm/base.json ]] \
    || die 'the server image carries the dev-VM tools'
  echo 'the server image holds what a server runs'
  exit 0
fi

# The development server: layers merge at boot, and nothing else of the boot
# changes.
[[ ! -e $root/etc/systemd/system/systemd-sysext.service ]] || die 'systemd-sysext.service is still masked'
[[ -L $wants/sysinit.target.wants/systemd-sysext.service ]] || die 'systemd-sysext.service is not enabled'
[[ -L $wants/sysinit.target.wants/tilo-vm-reconcile-image.service ]] \
  || die 'tilo-vm-reconcile-image is not enabled, so no booted image is recorded before layers merge'
[[ -L $wants/multi-user.target.wants/tilo-vm-restore.service ]] || die 'tilo-vm-restore is not enabled'
[[ -L $wants/multi-user.target.wants/tilo-vm-incoming.service ]] \
  || die 'tilo-vm-incoming is not enabled, so the layers an installation leaves are never applied'
# The wants links are absolute, so each unit is read where it is installed.
for unit in "$wants"/*.wants/tilo-vm-*.service; do
  unit=${unit##*/}
  program=$(sed -n 's/^ExecStart=\([^ ]*\).*/\1/p' "$root/usr/lib/systemd/system/$unit")
  inside test -x "$program" || die "$unit runs $program, which the image does not have"
done
[[ -x $root/usr/libexec/tilo/tilo-runtime ]] || die 'no tilo-runtime, which runs the device steps'
grep -q '^tilo-vm ' <<<"$(inside tilo-vm --version)" || die 'tilo-vm does not start on the server'
cmp -s "$root/usr/share/tilo/components.toml" "$repo_root/os/components.toml" \
  || die "the image's component manifest is not this tree's"
# The device refuses a layer built for another target or by another check
# image, so base.json names the ones that built the server's own programs and
# the runtime beside them.
base=$root/usr/share/tilo-vm/base.json
builder=$(awk '/^\[/ { exit } /^image = "/ { gsub(/^image = "|"$/, ""); print; exit }' "$repo_root/t.toml")
field() { inside python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))[sys.argv[2]] or "")' \
  /usr/share/tilo-vm/base.json "$1"; }
[[ -f $base ]] || die 'no /usr/share/tilo-vm/base.json, so the device checks no layer'
[[ $(field builder_image_digest) == "${builder#*@}" ]] \
  || die "base.json names builder $(field builder_image_digest), not t's check image ${builder#*@}"
[[ $(field target) == x86_64-unknown-linux-gnu ]] || die "base.json names target $(field target)"
toolchain=$(field toolchain)
[[ $toolchain =~ ^rustc\ [0-9]+\.[0-9]+\.[0-9]+ ]] || die "base.json names toolchain '$toolchain'"
grep -aqF "rustc version ${toolchain#rustc }" "$root/usr/libexec/tilo/tilo-runtime" \
  || die "tilo-runtime was not built by $toolchain"
[[ $(field cargo_lock_hash) == "$(sha256sum "$repo_root/Cargo.lock" | cut -d' ' -f1)" ]] \
  || die "base.json names another Cargo.lock than this tree's"
[[ $(field image_digest) =~ ^sha256:[0-9a-f]{64}$ ]] || die 'base.json names no base image digest'
echo 'the development server image holds what a server runs, and takes layers at boot'
