#!/usr/bin/env bash
# Keep optional system extensions inside the immutable-image trust boundary.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
containerfile="$repo/os/Containerfile"
tmpfiles="$repo/os/usr/lib/tmpfiles.d/qos.conf"
helper="$repo/os/usr/libexec/qos-system-extension-activate"
userspace="$repo/os/usr/lib/systemd/system/qos-system-extension-userspace.service"
boot="$repo/os/usr/lib/systemd/system/qos-system-extension-boot-stage.service"
model="$repo/os/usr/lib/systemd/system/qos-local-model.service"
socket="$repo/os/usr/lib/systemd/system/qos-system-extension-dispatcher.socket"
dispatcher="$repo/os/usr/lib/systemd/system/qos-system-extension-dispatcher@.service"
dispatch_helper="$repo/os/usr/libexec/qos-system-extension-dispatch"
polkit="$repo/os/etc/polkit-1/rules.d/49-qos-updates.rules"
contract="$repo/os/usr/share/qos/system-extensions/README.md"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
need() { grep -Fq "$2" "$1" || bad "$3"; }

for source in \
    'os/usr/libexec/qos-system-extension-activate /usr/libexec/qos-system-extension-activate' \
    'os/usr/lib/systemd/system/qos-system-extension-userspace.service /usr/lib/systemd/system/qos-system-extension-userspace.service' \
    'os/usr/lib/systemd/system/qos-system-extension-boot-stage.service /usr/lib/systemd/system/qos-system-extension-boot-stage.service' \
    'os/usr/lib/systemd/system/qos-local-model.service /usr/lib/systemd/system/qos-local-model.service' \
    'os/usr/lib/systemd/system/qos-system-extension-dispatcher.socket /usr/lib/systemd/system/qos-system-extension-dispatcher.socket' \
    'os/usr/lib/systemd/system/qos-system-extension-dispatcher@.service /usr/lib/systemd/system/qos-system-extension-dispatcher@.service' \
    'os/usr/libexec/qos-system-extension-dispatch /usr/libexec/qos-system-extension-dispatch' \
    'os/usr/lib/sysusers.d/qos-local-model.conf /usr/lib/sysusers.d/qos-local-model.conf' \
    'os/usr/share/qos/system-extensions /usr/share/qos/system-extensions'; do
    set -- $source
    need "$containerfile" "COPY $1 $2" "image does not package $1"
done
[ "$fail" -ne 0 ] || ok 'image packages extension contracts and service boundary'

for line in \
    'd  /var/lib/qos/system-extensions          0700  root  root' \
    'd  /var/lib/qos/local-models/staging       0700  root  root' \
    'd  /var/lib/qos/local-models/verified      0750  root  qos-local-model' \
    'd  /run/qos/system-extensions   0700  root  root' \
    'd  /run/qos/system-extension-api 0750 root  qos-identity'; do
    need "$tmpfiles" "$line" "tmpfiles does not pin $line"
done
[ "$fail" -ne 0 ] || ok 'state, transfer bytes, and markers have distinct ownership'

need "$helper" 'userspace|boot) class=$1' 'helper accepts an unbounded activation class'
need "$helper" "'0:0:600'" 'helper does not require private root-owned desired state'
need "$helper" '"pending"' 'helper does not fail closed on a pending transaction'
need "$helper" 'mktemp "$parent/.system-extension.XXXXXX"' 'marker is not atomically created beside its target'
need "$helper" 'kernel_module|gpu_driver_stack' 'helper does not distinguish reboot-bound packs'
need "$helper" 'local_model_runtime' 'helper does not distinguish local-model packs'
if awk '$1 !~ /^#/' "$helper" | grep -Eiq '(curl|wget|dnf|rpm-ostree|bootc|modprobe|insmod|mount|https?://)'; then
    bad 'activation helper gained download, installer, mount, or module-load authority'
else
    ok 'activation helper has no network, installer, mount, or module-load authority'
fi

for unit in "$userspace" "$boot"; do
    need "$unit" 'ProtectSystem=strict' "$(basename "$unit") can write immutable root"
    need "$unit" 'PrivateNetwork=yes' "$(basename "$unit") can reach a network"
    need "$unit" 'NoNewPrivileges=yes' "$(basename "$unit") can gain privilege"
    need "$unit" 'CapabilityBoundingSet=' "$(basename "$unit") retains capabilities"
    need "$unit" 'ProtectKernelModules=yes' "$(basename "$unit") can manipulate modules"
done
need "$boot" 'Before=qos-deployment-update.service' 'boot stage does not precede image staging'
need "$boot" 'ReadWritePaths=/var/lib/qos/system-extensions' 'boot stage cannot persist marker'
need "$userspace" 'ReadWritePaths=/run/qos/system-extensions' 'userspace gate cannot persist marker'
[ "$fail" -ne 0 ] || ok 'activation units are confined and class-specific'

need "$model" 'ConditionPathExists=/usr/libexec/qos-local-model-service' 'absent local-model binary fails boot instead of cleanly skipping'
need "$model" 'User=qos-local-model' 'local-model runtime is not unprivileged'
need "$model" 'IPAddressDeny=any' 'local-model runtime has ambient network access'
need "$model" 'IPAddressAllow=127.0.0.1/32' 'local-model runtime may bind beyond loopback IPv4'
need "$model" 'IPAddressAllow=::1/128' 'local-model runtime may bind beyond loopback IPv6'
need "$model" 'PrivateDevices=yes' 'local-model runtime has ambient device access'
need "$model" 'ReadOnlyPaths=/var/lib/qos/system-extensions /var/lib/qos/local-models /usr/share/qos/system-extensions' 'local-model runtime can replace verified packs'
[ "$fail" -ne 0 ] || ok 'future local-model service is unprivileged and condition-gated'

need "$socket" 'ListenStream=/run/qos/system-extension-api/request.sock' 'desktop request socket is not fixed-path'
need "$socket" 'SocketGroup=qos-identity' 'desktop request socket is not group-gated'
need "$socket" 'SocketMode=0660' 'desktop request socket permissions drifted'
need "$socket" 'Accept=yes' 'desktop request is not backgrounded per request'
need "$dispatcher" 'StandardInput=socket' 'dispatcher does not receive only its socket request'
need "$dispatcher" 'PrivateNetwork=yes' 'dispatcher can reach a network'
need "$dispatcher" 'ReadWritePaths=/run/qos/system-extension-api /run/qos/system-extensions /var/lib/qos/system-extensions' 'dispatcher receives a broad writable root'
need "$dispatch_helper" 'userspace-stage)' 'dispatcher has no fixed userspace verb'
need "$dispatch_helper" 'boot-stage)' 'dispatcher has no fixed boot verb'
need "$dispatch_helper" 'unsupported fixed request verb' 'dispatcher accepts arbitrary request verbs'
need "$dispatch_helper" 'request must contain exactly one line' 'dispatcher accepts arbitrary request fields'
need "$dispatch_helper" 'status=$api_dir/status.json' 'desktop has no unprivileged status projection'
if awk '$1 !~ /^#/' "$dispatch_helper" | grep -Eiq '(^|[[:space:]])(curl|wget|dnf|rpm-ostree|bootc|systemctl|reboot|modprobe|insmod|mount)([[:space:]]|$)|https?://'; then
    bad 'dispatcher gained installer, restart, network, mount, or module-load authority'
else
    ok 'socket dispatcher exposes fixed activation verbs without root execution'
fi
if grep -Fq 'qos-system-extension-userspace.service": true' "$polkit" \
   || grep -Fq 'qos-system-extension-boot-stage.service": true' "$polkit" \
   || grep -Fq 'qos-local-model.service": true' "$polkit"; then
    bad 'polkit grants direct management of extension or model services'
else
    ok 'desktop has no broad polkit grant for model or extension services'
fi
need "$containerfile" 'systemctl enable qos-system-extension-dispatcher.socket' 'fixed request socket is not enabled in the image'

need "$contract" 'must call `ExtensionState::accepted_for`' 'contract does not require deployment revalidation'
need "$contract" 'private signing keys must never enter' 'contract does not prohibit device signing keys'

exit "$fail"
