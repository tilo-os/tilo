#!/usr/bin/env bash
# Keep optional system extensions inside the immutable-image trust boundary.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
containerfile="$repo/os/Containerfile"
tmpfiles="$repo/os/usr/lib/tmpfiles.d/qos.conf"
user_tmpfiles="$repo/os/usr/lib/user-tmpfiles.d/qos-model-manager.conf"
helper="$repo/os/usr/libexec/qos-system-extension-activate"
userspace="$repo/os/usr/lib/systemd/system/qos-system-extension-userspace.service"
boot="$repo/os/usr/lib/systemd/system/qos-system-extension-boot-stage.service"
model="$repo/os/usr/lib/systemd/system/qos-local-model.service"
user_model="$repo/os/usr/lib/systemd/user/qos-model-manager.service"
socket="$repo/os/usr/lib/systemd/system/qos-system-extension-dispatcher.socket"
dispatcher="$repo/os/usr/lib/systemd/system/qos-system-extension-dispatcher@.service"
dispatch_helper="$repo/os/usr/libexec/qos-system-extension-dispatch"
manager_source="$repo/crates/system/system-extension/src/bin/qos-system-extension-manager.rs"
boot_status="$repo/os/usr/lib/systemd/system/qos-system-extension-boot-pack-status.service"
boot_stage="$repo/os/usr/lib/systemd/system/qos-system-extension-boot-pack-stage.service"
boot_stage_path="$repo/os/usr/lib/systemd/system/qos-system-extension-boot-pack-stage.path"
polkit="$repo/os/etc/polkit-1/rules.d/49-qos-updates.rules"
contract="$repo/os/usr/share/qos/system-extensions/README.md"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
need() { grep -Fq -- "$2" "$1" || bad "$3"; }

for source in \
    'os/usr/libexec/qos-system-extension-activate /usr/libexec/qos-system-extension-activate' \
    'os/usr/lib/systemd/system/qos-system-extension-userspace.service /usr/lib/systemd/system/qos-system-extension-userspace.service' \
    'os/usr/lib/systemd/system/qos-system-extension-boot-stage.service /usr/lib/systemd/system/qos-system-extension-boot-stage.service' \
    'os/usr/lib/systemd/system/qos-local-model.service /usr/lib/systemd/system/qos-local-model.service' \
    'os/usr/lib/systemd/user/qos-model-manager.service /usr/lib/systemd/user/qos-model-manager.service' \
    'os/usr/lib/user-tmpfiles.d/qos-model-manager.conf /usr/lib/user-tmpfiles.d/qos-model-manager.conf' \
    'os/usr/lib/systemd/system/qos-system-extension-dispatcher.socket /usr/lib/systemd/system/qos-system-extension-dispatcher.socket' \
    'os/usr/lib/systemd/system/qos-system-extension-dispatcher@.service /usr/lib/systemd/system/qos-system-extension-dispatcher@.service' \
    'os/usr/libexec/qos-system-extension-dispatch /usr/libexec/qos-system-extension-dispatch' \
    'os/usr/lib/systemd/system/qos-system-extension-boot-pack-status.service /usr/lib/systemd/system/qos-system-extension-boot-pack-status.service' \
    'os/usr/lib/systemd/system/qos-system-extension-boot-pack-stage.service /usr/lib/systemd/system/qos-system-extension-boot-pack-stage.service' \
    'os/usr/lib/systemd/system/qos-system-extension-boot-pack-stage.path /usr/lib/systemd/system/qos-system-extension-boot-pack-stage.path' \
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

need "$user_model" 'ExecStart=/usr/bin/qos-model-manager serve --socket %t/qos/services/models.sock' 'per-user model service does not publish the launcher service socket'
need "$user_model" 'ProtectSystem=strict' 'per-user model manager can write the immutable root'
need "$user_model" 'ProtectHome=read-only' 'per-user model manager has broad home access'
need "$user_model" 'ReadWritePaths=%h/.local/share/qos %h/.local/share/containers %t/qos' 'per-user model manager lacks narrowly writable model/runtime state'
need "$user_model" 'RuntimeDirectory=qos' 'per-user model manager does not create its runtime parent'
need "$user_model" 'ExecStartPre=/usr/bin/systemd-tmpfiles --user --create /usr/lib/user-tmpfiles.d/qos-model-manager.conf' 'per-user model manager does not provision fresh-user persistent state before startup'
for line in \
    'd %h/.local/share/qos            0700' \
    'd %h/.local/share/qos/models     0700' \
    'd %h/.local/share/containers     0700' \
    'd %t/qos/services                0700'; do
    need "$user_tmpfiles" "$line" "fresh-user tmpfiles does not provision $line"
done
need "$containerfile" 'systemctl --global enable qos-model-manager.service' 'per-user model manager is not enabled in the image'
[ "$fail" -ne 0 ] || ok 'Settings reaches a confined per-user model manager through the app-service channel with fresh-user storage'

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
need "$containerfile" 'COPY --from=builder /out/qos-system-extension-manager /usr/libexec/qos-system-extension-manager' 'image does not package privileged BootPack manager'
need "$containerfile" '-p system-extension' 'image builder does not compile privileged BootPack manager'
need "$containerfile" 'systemctl enable qos-system-extension-boot-pack-status.service' 'BootPack reboot status is not projected after boot'
need "$containerfile" 'systemctl enable qos-system-extension-boot-pack-stage.path' 'BootPack fixed queue is not path-activated'
need "$repo/os/tests/system_extension_boot_pack_stage.sh" 'systemd-run' 'BootPack stage has no systemd execution-profile integration test'

need "$dispatch_helper" 'boot-pack-stage\ *)' 'dispatcher has no exact catalog-id BootPack request'
need "$dispatch_helper" 'boot-pack-recommend)' 'dispatcher has no zero-input signed PCI recommendation request'
need "$dispatch_helper" 'qos-system-extension-manager recommend-pci' 'dispatcher lets unprivileged code guess PCI-to-pack mapping'
need "$boot_stage" 'qos-system-extension-manager stage-pending' 'path-activated service does not hand fixed queue to manager verb'
need "$manager_source" '"stage-boot"' 'manager has no fixed BootPack stage verb'
need "$manager_source" 'CATALOG_DIR' 'manager does not resolve image-owned catalog entries'
need "$manager_source" 'TRUST_DIR' 'manager does not resolve image-owned trust keys'
need "$manager_source" 'HARDWARE_CATALOG_PATH' 'manager has no separate signed PCI hardware catalogue'
need "$manager_source" 'HARDWARE_TRUST_PATH' 'manager has no separate PCI hardware trust key'
need "$manager_source" 'recommended_boot_packs' 'manager does not authorize packs from observed hardware'
need "$manager_source" 'PCI_DEVICES_PATH' 'manager accepts caller-supplied PCI identity'
need "$manager_source" 'subsystem_rule_wins_over_generic_pci_rule' 'PCI matching has no deterministic specificity test'
need "$manager_source" 'verify(&keyring)' 'manager does not verify catalog signatures'
need "$manager_source" 'preflight(&host)' 'manager does not revalidate deployment image/ABI'
need "$manager_source" 'bootc://' 'manager accepts non-BootPack artifact sources'
need "$manager_source" '["switch", "--transport=registry", image]' 'manager does not use fixed digest-pinned bootc staging'
need "$manager_source" 'reboot_required' 'manager has no typed reboot-required projection'
need "$boot_status" 'ExecStart=/usr/libexec/qos-system-extension-manager project-status' 'BootPack status unit does not re-project state after reboot'
need "$boot_status" 'PrivateNetwork=yes' 'BootPack status unit has network access'
need "$boot_stage_path" 'PathExists=/var/lib/qos/system-extensions/boot-pack-request' 'BootPack request queued before path activation can be stranded'
need "$boot_stage_path" 'PathChanged=/var/lib/qos/system-extensions/boot-pack-request' 'BootPack stage has no fixed root-owned queue watch'
need "$boot_stage" 'ExecStart=/usr/libexec/qos-system-extension-manager stage-pending' 'BootPack stage service accepts arbitrary execution input'
need "$boot_stage" 'PrivateNetwork=no' 'networked bootc stage service cannot fetch the pinned deployment'
need "$boot_stage" 'RequiresMountsFor=/boot /sysroot /var/lib/containers' 'BootPack stage can run before its bootc storage is mounted'
need "$boot_stage" 'CapabilityBoundingSet=CAP_SYS_ADMIN CAP_CHOWN CAP_DAC_OVERRIDE CAP_FOWNER CAP_MKNOD CAP_SETFCAP' 'BootPack stage lacks narrowly bounded bootc deployment capabilities'
need "$boot_stage" 'ReadWritePaths=/var/lib/qos/system-extensions /run/qos/system-extension-api /var/lib/containers /var/lib/bootc /ostree /sysroot /boot' 'BootPack stage does not expose only bootc deployment storage'
if grep -Fq 'PrivateDevices=yes' "$boot_stage" || awk '$1 !~ /^#/ && /@mount/' "$boot_stage" | grep -q .; then
    bad 'BootPack stage blocks bootc mount or device operations and cannot stage a deployment'
else
    ok 'BootPack stage permits only the bootc deployment operations it needs'
fi
need "$dispatch_helper" 'boot-pack-request' 'dispatcher does not enqueue BootPack id in a fixed root-owned path'
need "$dispatch_helper" '"state":"queued"' 'dispatcher does not project queued BootPack state'
need "$manager_source" 'could not clear terminal BootPack request' 'BootPack failure can loop forever instead of consuming a terminal request'
need "$dispatcher" 'ReadOnlyPaths=/usr/share/qos/system-extensions /sys/bus/pci/devices' 'PCI recommendation worker cannot use the local hardware facts safely'
if grep -Eiq '(Command::new\([^)]*(sh|bash|curl|wget|dnf|rpm-ostree|modprobe|insmod|mount)|system\()' "$manager_source"; then
    bad 'BootPack manager gained shell, package-manager, downloader, mount, or module-load authority'
else
    ok 'BootPack manager has only fixed bootc and uname execution paths'
fi

need "$contract" 'must call `ExtensionState::accepted_for`' 'contract does not require deployment revalidation'
need "$contract" 'private signing keys must never enter' 'contract does not prohibit device signing keys'
need "$contract" 'boot-pack-stage <catalog-id>' 'contract does not document exact catalog-id request'
need "$contract" 'boot-pack-recommend' 'contract does not document zero-input PCI recommendation'
need "$contract" 'never hot' 'contract could imply a hot driver install'

exit "$fail"
