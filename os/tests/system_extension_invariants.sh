#!/usr/bin/env bash
# Keep optional system extensions inside the immutable-image trust boundary.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
containerfile="$repo/os/Containerfile"
tmpfiles="$repo/os/usr/lib/tmpfiles.d/tilo.conf"
user_tmpfiles="$repo/os/usr/lib/user-tmpfiles.d/tilo-model-manager.conf"
helper="$repo/os/usr/libexec/tilo-system-extension-activate"
userspace="$repo/os/usr/lib/systemd/system/tilo-system-extension-userspace.service"
boot="$repo/os/usr/lib/systemd/system/tilo-system-extension-boot-stage.service"
model="$repo/os/usr/lib/systemd/system/tilo-local-model.service"
user_model="$repo/os/usr/lib/systemd/user/tilo-models.service"
gateway="$repo/os/usr/lib/systemd/user/tilo-model-gateway.service"
user_model_socket="$repo/os/usr/lib/systemd/user/tilo-models.socket"
model_gateway="$repo/os/usr/lib/systemd/user/tilo-model-gateway.service"
model_gateway_socket="$repo/os/usr/lib/systemd/user/tilo-model-gateway.socket"
user_model_source="$repo/crates/agent/local-model-service/src/main.rs"
model_gateway_source="$repo/crates/agent/model-gateway/src/main.rs"
settings_models_source="$repo/apps/settings/src/panes/agent_models.rs"
socket="$repo/os/usr/lib/systemd/system/tilo-system-extension-dispatcher.socket"
dispatcher="$repo/os/usr/lib/systemd/system/tilo-system-extension-dispatcher@.service"
dispatch_helper="$repo/os/usr/libexec/tilo-system-extension-dispatch"
manager_source="$repo/crates/system/system-extension/src/bin/tilo-system-extension-manager.rs"
boot_status="$repo/os/usr/lib/systemd/system/tilo-system-extension-boot-pack-status.service"
boot_status_timer="$repo/os/usr/lib/systemd/system/tilo-system-extension-boot-pack-status.timer"
boot_stage="$repo/os/usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.service"
boot_stage_path="$repo/os/usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.path"
polkit="$repo/os/etc/polkit-1/rules.d/49-tilo-updates.rules"
contract="$repo/os/usr/share/tilo/system-extensions/README.md"
# shellcheck source=lib/containerfile.sh
. "$repo/os/tests/lib/containerfile.sh"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
need() { grep -Fq -- "$2" "$1" || bad "$3"; }

for source in \
    'os/usr/libexec/tilo-system-extension-activate /usr/libexec/tilo-system-extension-activate' \
    'os/usr/lib/systemd/system/tilo-system-extension-userspace.service /usr/lib/systemd/system/tilo-system-extension-userspace.service' \
    'os/usr/lib/systemd/system/tilo-system-extension-boot-stage.service /usr/lib/systemd/system/tilo-system-extension-boot-stage.service' \
    'os/usr/lib/systemd/system/tilo-local-model.service /usr/lib/systemd/system/tilo-local-model.service' \
    'os/usr/lib/systemd/user/tilo-models.service /usr/lib/systemd/user/tilo-models.service' \
    'os/usr/lib/user-tmpfiles.d/tilo-model-manager.conf /usr/lib/user-tmpfiles.d/tilo-model-manager.conf' \
    'os/usr/lib/systemd/system/tilo-system-extension-dispatcher.socket /usr/lib/systemd/system/tilo-system-extension-dispatcher.socket' \
    'os/usr/lib/systemd/system/tilo-system-extension-dispatcher@.service /usr/lib/systemd/system/tilo-system-extension-dispatcher@.service' \
    'os/usr/libexec/tilo-system-extension-dispatch /usr/libexec/tilo-system-extension-dispatch' \
    'os/usr/lib/systemd/system/tilo-system-extension-boot-pack-status.service /usr/lib/systemd/system/tilo-system-extension-boot-pack-status.service' \
    'os/usr/lib/systemd/system/tilo-system-extension-boot-pack-status.timer /usr/lib/systemd/system/tilo-system-extension-boot-pack-status.timer' \
    'os/usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.service /usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.service' \
    'os/usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.path /usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.path' \
    'os/usr/lib/sysusers.d/tilo-local-model.conf /usr/lib/sysusers.d/tilo-local-model.conf' \
    'os/usr/share/tilo/system-extensions /usr/share/tilo/system-extensions'; do
    set -- $source
    containerfile_has_copy "$containerfile" "$1" "$2" \
      || bad "image does not package $1"
done
[ "$fail" -ne 0 ] || ok 'image packages extension contracts and service boundary'

for line in \
    'd  /var/lib/tilo/system-extensions          0700  root  root' \
    'd  /var/lib/tilo/local-models/staging       0700  root  root' \
    'd  /var/lib/tilo/local-models/verified      0750  root  tilo-local-model' \
    'd  /run/tilo/system-extensions   0700  root  root' \
    'd  /run/tilo/system-extension-api 0750 root  tilo-identity'; do
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
need "$boot" 'Before=tilo-deployment-update.service' 'boot stage does not precede image staging'
need "$boot" 'ReadWritePaths=/var/lib/tilo/system-extensions' 'boot stage cannot persist marker'
need "$userspace" 'ReadWritePaths=/run/tilo/system-extensions' 'userspace gate cannot persist marker'
[ "$fail" -ne 0 ] || ok 'activation units are confined and class-specific'

need "$model" 'ConditionPathExists=/usr/libexec/tilo-local-model-service' 'absent local-model binary fails boot instead of cleanly skipping'
need "$model" 'User=tilo-local-model' 'local-model runtime is not unprivileged'
need "$model" 'IPAddressDeny=any' 'local-model runtime has ambient network access'
need "$model" 'IPAddressAllow=127.0.0.1/32' 'local-model runtime may bind beyond loopback IPv4'
need "$model" 'IPAddressAllow=::1/128' 'local-model runtime may bind beyond loopback IPv6'
need "$model" 'PrivateDevices=yes' 'local-model runtime has ambient device access'
need "$model" 'ReadOnlyPaths=/var/lib/tilo/system-extensions /var/lib/tilo/local-models /usr/share/tilo/system-extensions' 'local-model runtime can replace verified packs'
[ "$fail" -ne 0 ] || ok 'future local-model service is unprivileged and condition-gated'

need "$user_model" 'ExecStart=/usr/bin/tilo-model-manager serve --socket %t/tilo/services/models.sock' 'per-user model service does not publish the launcher service socket'
need "$user_model" 'ProtectSystem=strict' 'per-user model manager can write the immutable root'
need "$user_model" 'ProtectHome=read-only' 'per-user model manager has broad home access'
need "$user_model" 'ReadWritePaths=%h/.local/share/tilo %h/.local/share/containers %t/tilo' 'per-user model manager lacks narrowly writable model/runtime state'
models_dropin="$repo/os/dev-image/usr/lib/systemd/user/tilo-models.service.d/10-nested-user-namespace.conf"
for off in ProtectSystem=false ProtectHome=false ProtectKernelTunables=false ProtectKernelModules=false ProtectKernelLogs=false PrivateIPC=false PrivateTmp=false ProtectControlGroups=false ReadWritePaths=; do
    need "$models_dropin" "$off" "nested-session drop-in does not set $off for the model manager"
done
if grep -Eq '^(ProtectSystem|ProtectHome|ProtectKernelTunables|ProtectKernelModules|ProtectKernelLogs|PrivateIPC|PrivateTmp|ProtectControlGroups)=$' "$models_dropin"; then
    bad 'nested-session drop-in uses an empty boolean assignment'
fi
merged_ipc=""
while IFS= read -r line; do
    case "$line" in
        PrivateIPC=) ;;
        PrivateIPC=*) merged_ipc="${line#PrivateIPC=}" ;;
    esac
done < <(grep -h -E '^PrivateIPC=' "$user_model" "$models_dropin" || true)
if [ "$merged_ipc" = yes ]; then
    bad 'model manager dev-image merge still sets PrivateIPC=yes'
fi
if grep -R -F -- '10-nested-user-namespace.conf' "$repo/os/usr" >/dev/null; then
    bad 'nested-session drop-in leaked into the production image'
fi
need "$user_model" 'RuntimeDirectory=tilo' 'per-user model manager does not create its runtime parent'
need "$user_model" 'StateDirectory=tilo containers' 'model manager does not provision its persistent roots without a helper process'
need "$gateway" 'ConfigurationDirectory=tilo' 'model gateway does not provision its private configuration root'
need "$gateway" 'ConfigurationDirectoryMode=0700' 'model gateway configuration root is not private'
need "$gateway" 'ProtectSystem=strict' 'model gateway can write the immutable root'
need "$gateway" 'ProtectHome=read-only' 'model gateway has broad home access'
need "$gateway" 'ReadWritePaths=%h/.config/tilo %t/tilo' 'model gateway lacks narrowly writable provider/runtime state'
if grep -Fq 'tilo-model-storage.service' "$containerfile" "$user_model" "$gateway"; then
    bad 'socket activation still forks a redundant model-storage helper'
else
    ok 'systemd directory management avoids a helper process on model activation'
fi
for line in \
    'd %h/.local/share/tilo            0700' \
    'd %h/.local/share/tilo/models     0700' \
    'd %h/.local/share/containers     0700' \
    'd %h/.config/tilo                0700' \
    'd %t/tilo/services                0700'; do
    need "$user_tmpfiles" "$line" "fresh-user tmpfiles does not provision $line"
done
need "$user_model_socket" 'ListenStream=%t/tilo/services/models.sock' 'model manager has no fixed activation socket'
need "$user_model_socket" 'SocketMode=0600' 'model manager activation socket is not private'
need "$user_model_socket" 'Backlog=16' 'model manager activation queue is unbounded'
need "$user_model_socket" 'Service=tilo-models.service' 'model activation socket starts the wrong service'
need "$user_model_socket" 'PartOf=tilo-graphical-session.target' 'model activation socket can outlive a Tilo session'
need "$user_model_socket" 'WantedBy=tilo-graphical-session.target' 'model activation socket starts outside a Tilo session'
need "$user_model_source" 'std::env::var("LISTEN_PID")' 'model manager ignores systemd socket activation'
need "$user_model_source" 'actual.as_pathname() != Some(expected)' 'model manager accepts a listener at an unexpected path'
need "$user_model_source" 'bounded_command::run_prepared(' 'model manager trusts unbounded runtime helpers'
if grep -Fq '.output()' "$user_model_source"; then
    bad 'model manager can allocate unbounded helper output'
fi
need "$containerfile" 'systemctl --global enable tilo-models.socket' 'per-user model socket is not enabled in the image'
if grep -Eq '^(After|Wants)=.*network-online\.target' "$user_model"; then
    bad 'local model manager blocks socket activation on network readiness'
fi
if grep -Fq 'systemctl --global enable tilo-models.service' "$containerfile"; then
    bad 'model manager process still starts eagerly at every login'
fi
need "$model_gateway_socket" 'ListenStream=%t/tilo/services/model-gateway.sock' 'hosted-model gateway has no fixed activation socket'
need "$model_gateway_socket" 'SocketMode=0600' 'hosted-model activation socket is not private'
need "$model_gateway_socket" 'Backlog=16' 'hosted-model activation queue is unbounded'
need "$model_gateway_socket" 'Service=tilo-model-gateway.service' 'hosted-model socket starts the wrong service'
need "$model_gateway_socket" 'PartOf=tilo-graphical-session.target' 'hosted-model socket can outlive a Tilo session'
need "$model_gateway_socket" 'WantedBy=tilo-graphical-session.target' 'hosted-model socket starts outside a Tilo session'
need "$model_gateway_source" 'std::env::var("LISTEN_PID")' 'hosted-model gateway ignores systemd socket activation'
need "$model_gateway_source" 'actual.as_pathname() != Some(expected)' 'hosted-model gateway accepts a listener at an unexpected path'
need "$model_gateway" 'RuntimeDirectoryPreserve=yes' 'hosted-model shutdown can remove sibling service sockets'
need "$model_gateway" 'PrivateDevices=yes' 'hosted-model gateway has ambient device access'
need "$model_gateway" 'PrivateIPC=yes' 'hosted-model gateway shares host IPC objects'
need "$model_gateway" 'RestrictNamespaces=yes' 'hosted-model gateway can create new namespaces'
need "$model_gateway" 'MemoryDenyWriteExecute=yes' 'hosted-model gateway can create executable writable memory'
need "$model_gateway" 'CapabilityBoundingSet=' 'hosted-model gateway retains capabilities'
need "$model_gateway" 'AmbientCapabilities=' 'hosted-model gateway can inherit ambient capabilities'
need "$model_gateway" 'KeyringMode=private' 'hosted-model gateway shares the session keyring'
need "$model_gateway" 'SystemCallFilter=@system-service' 'hosted-model gateway lacks a syscall allowlist'
need "$model_gateway" 'SystemCallFilter=~@clock @debug @keyring @module @mount @obsolete @raw-io @reboot @resources @swap' 'hosted-model gateway retains privileged syscall groups'
need "$containerfile" 'systemctl --global enable tilo-model-gateway.socket' 'hosted-model socket is not enabled in the image'
if grep -Eq '^(After|Wants)=.*network-online\.target' "$model_gateway"; then
    bad 'hosted-model gateway blocks socket activation on network readiness'
fi
if grep -Fq 'systemctl --global enable tilo-model-gateway.service' "$containerfile"; then
    bad 'hosted-model process still starts eagerly at every login'
fi
if grep -Eq '^Wants=.*tilo-(models|model-gateway|app-host)\.service' \
    "$repo/os/usr/lib/systemd/user/tilo-graphical-session.target"; then
    bad 'graphical session target bypasses socket activation for app/model daemons'
fi
[ "$fail" -ne 0 ] || ok 'Settings reaches a confined per-user model manager through the app-service channel with fresh-user storage'

need "$socket" 'ListenStream=/run/tilo/system-extension-api/request.sock' 'desktop request socket is not fixed-path'
need "$socket" 'SocketGroup=tilo-identity' 'desktop request socket is not group-gated'
need "$socket" 'SocketMode=0660' 'desktop request socket permissions drifted'
need "$socket" 'Accept=yes' 'desktop request is not backgrounded per request'
need "$socket" 'Backlog=16' 'desktop request activation queue is unbounded'
need "$socket" 'MaxConnections=16' 'desktop request socket can fan out unbounded root workers'
need "$dispatcher" 'StandardInput=socket' 'dispatcher does not receive only its socket request'
need "$dispatcher" 'PrivateNetwork=yes' 'dispatcher can reach a network'
need "$dispatcher" 'ReadWritePaths=/run/tilo/system-extension-api /run/tilo/system-extensions /var/lib/tilo/system-extensions' 'dispatcher receives a broad writable root'
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
if grep -Fq 'tilo-system-extension-userspace.service": true' "$polkit" \
   || grep -Fq 'tilo-system-extension-boot-stage.service": true' "$polkit" \
   || grep -Fq 'tilo-local-model.service": true' "$polkit"; then
    bad 'polkit grants direct management of extension or model services'
else
    ok 'desktop has no broad polkit grant for model or extension services'
fi
need "$polkit" 'action.lookup("unit") == "tilo-deployment-update.service"' 'ush cannot request the exact signed image staging unit'
need "$polkit" 'subject.isInGroup("wheel")' 'signed image staging is not limited to administrators'
if grep -F '"tilo-deployment-update.service": true' "$polkit" >/dev/null; then
    bad 'signed image staging is mixed into the broader boot-control grant'
else
    ok 'ush receives only the exact signed image staging operation'
fi
need "$containerfile" 'systemctl enable tilo-system-extension-dispatcher.socket' 'fixed request socket is not enabled in the image'
containerfile_installs_output "$containerfile" tilo-system-extension-manager \
  /usr/libexec/tilo-system-extension-manager \
  || bad 'image does not package privileged BootPack manager'
need "$containerfile" '-p system-extension' 'image builder does not compile privileged BootPack manager'
need "$containerfile" 'systemctl enable tilo-system-extension-boot-pack-status.timer' 'BootPack reboot status is not projected after boot'
need "$boot_status_timer" 'Unit=tilo-system-extension-boot-pack-status.service' 'BootPack status timer starts the wrong unit'
need "$boot_status_timer" 'OnBootSec=5s' 'BootPack status projection is not deferred beyond the login-critical path'
if grep -Fq 'systemctl enable tilo-system-extension-boot-pack-status.service' "$containerfile"; then
    bad 'BootPack status projection is still enabled directly on the boot path'
fi
need "$containerfile" 'systemctl enable tilo-system-extension-boot-pack-stage.path' 'BootPack fixed queue is not path-activated'
need "$repo/os/tests/system_extension_boot_pack_stage.sh" 'systemd-run' 'BootPack stage has no systemd execution-profile integration test'

need "$dispatch_helper" 'boot-pack-stage\ *)' 'dispatcher has no exact catalog-id BootPack request'
need "$dispatch_helper" 'boot-pack-recommend)' 'dispatcher has no zero-input signed PCI recommendation request'
need "$dispatch_helper" 'tilo-system-extension-manager recommend-pci' 'dispatcher lets unprivileged code guess PCI-to-pack mapping'
need "$boot_stage" 'tilo-system-extension-manager stage-pending' 'path-activated service does not hand fixed queue to manager verb'
need "$manager_source" '"stage-boot"' 'manager has no fixed BootPack stage verb'
need "$manager_source" 'CATALOG_DIR' 'manager does not resolve image-owned catalog entries'
need "$manager_source" 'TRUST_DIR' 'manager does not resolve image-owned trust keys'
need "$manager_source" 'HARDWARE_CATALOG_PATH' 'manager has no separate signed PCI hardware catalogue'
need "$manager_source" 'HARDWARE_TRUST_PATH' 'manager has no separate PCI hardware trust key'
need "$manager_source" 'recommended_boot_packs' 'manager does not authorize packs from observed hardware'
need "$manager_source" 'PCI_DEVICES_PATH' 'manager accepts caller-supplied PCI identity'
need "$manager_source" 'fn boot_packs_and_catalogues_are_digest_pinned_signed_and_matched' 'PCI matching has no deterministic specificity test'
need "$manager_source" 'verify(&keyring)' 'manager does not verify catalog signatures'
need "$manager_source" 'preflight(&host)' 'manager does not revalidate deployment image/ABI'
need "$manager_source" 'bootc://' 'manager accepts non-BootPack artifact sources'
need "$manager_source" '["switch", "--transport=registry", image]' 'manager does not use fixed digest-pinned bootc staging'
need "$manager_source" 'reboot_required' 'manager has no typed reboot-required projection'
need "$boot_status" 'ExecStart=/usr/libexec/tilo-system-extension-manager project-status' 'BootPack status unit does not re-project state after reboot'
need "$boot_status" 'PrivateNetwork=yes' 'BootPack status unit has network access'
for resource_policy in 'Nice=10' 'CPUWeight=20' 'IOWeight=20' \
    'IOSchedulingClass=best-effort' 'IOSchedulingPriority=7'; do
    need "$boot_status" "$resource_policy" "BootPack status projection does not yield with $resource_policy"
done
need "$boot_stage_path" 'PathExists=/var/lib/tilo/system-extensions/boot-pack-request' 'BootPack request queued before path activation can be stranded'
need "$boot_stage_path" 'PathChanged=/var/lib/tilo/system-extensions/boot-pack-request' 'BootPack stage has no fixed root-owned queue watch'
need "$boot_stage" 'ExecStart=/usr/libexec/tilo-system-extension-manager stage-pending' 'BootPack stage service accepts arbitrary execution input'
need "$boot_stage" 'PrivateNetwork=no' 'networked bootc stage service cannot fetch the pinned deployment'
need "$boot_stage" 'RequiresMountsFor=/boot /sysroot /var/lib/containers' 'BootPack stage can run before its bootc storage is mounted'
need "$boot_stage" 'CapabilityBoundingSet=CAP_SYS_ADMIN CAP_CHOWN CAP_DAC_OVERRIDE CAP_FOWNER CAP_MKNOD CAP_SETFCAP' 'BootPack stage lacks narrowly bounded bootc deployment capabilities'
need "$boot_stage" 'ReadWritePaths=/var/lib/tilo/system-extensions /run/tilo/system-extension-api /var/lib/containers /var/lib/bootc /ostree /sysroot /boot' 'BootPack stage does not expose only bootc deployment storage'
if grep -Fq 'PrivateDevices=yes' "$boot_stage" || awk '$1 !~ /^#/ && /@mount/' "$boot_stage" | grep -q .; then
    bad 'BootPack stage blocks bootc mount or device operations and cannot stage a deployment'
else
    ok 'BootPack stage permits only the bootc deployment operations it needs'
fi
need "$dispatch_helper" 'boot-pack-request' 'dispatcher does not enqueue BootPack id in a fixed root-owned path'
need "$dispatch_helper" '"state":"queued"' 'dispatcher does not project queued BootPack state'
need "$manager_source" 'could not clear terminal BootPack request' 'BootPack failure can loop forever instead of consuming a terminal request'
need "$settings_models_source" 'bounded_command::run(' 'Settings model lifecycle can hang on an unbounded manager client'
if grep -Eq 'Command::new|\.output\(\)|wait_with_output|\.spawn\(\)' "$settings_models_source"; then
    bad 'Settings model lifecycle contains an unbounded manager subprocess'
else
    ok 'Settings model lifecycle uses only bounded manager transactions'
fi
need "$dispatcher" 'ReadOnlyPaths=/usr/share/tilo/system-extensions /sys/bus/pci/devices' 'PCI recommendation worker cannot use the local hardware facts safely'
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
