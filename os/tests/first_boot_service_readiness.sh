#!/usr/bin/env bash
# First-boot prerequisites must be expected state, not crash loops or red units.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
harden="$repo/os/usr/lib/systemd/system/tilo-rollback-harden.service"
harden_timer="$repo/os/usr/lib/systemd/system/tilo-rollback-harden.timer"
harden_helper="$repo/os/usr/libexec/tilo-rollback-harden"
fs_unit="$repo/os/usr/lib/systemd/system/fs.service"
fs_path="$repo/os/usr/lib/systemd/system/fs-identity.path"
fs_timer="$repo/os/usr/lib/systemd/system/tilo-fs-start.timer"
device_key_unit="$repo/os/usr/lib/systemd/system/tilo-device-key-init.service"
growfs_unit="$repo/os/usr/lib/systemd/system/tilo-growfs-sysroot.service"
tmpfiles="$repo/os/usr/lib/tmpfiles.d/tilo.conf"
containerfile="$repo/os/Containerfile"
# shellcheck source=lib/containerfile.sh
. "$repo/os/tests/lib/containerfile.sh"
boot_mode_generator="$repo/os/usr/lib/systemd/system-generators/tilo-boot-mode-generator"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
exactly_once() { [ "$(grep -Fxc "$2" "$1")" -eq 1 ]; }

# `|` makes these systemd trigger conditions: at least one TPM device must
# exist, while all ordinary conditions in the unit must still pass.
if exactly_once "$harden" 'ConditionPathExists=|/dev/tpmrm0' \
   && exactly_once "$harden" 'ConditionPathExists=|/dev/tpm0' \
   && grep -Fq 'if [ ! -e /dev/tpmrm0 ] && [ ! -e /dev/tpm0 ]; then' "$harden_helper"; then
  ok 'TPM hardening skips cleanly without hardware and retains a race-safe helper guard'
else
  bad 'TPM hardening can become a failed unit on a machine with no TPM'
fi
if exactly_once "$harden_timer" 'OnBootSec=5s' \
   && exactly_once "$harden_timer" 'Unit=tilo-rollback-harden.service' \
   && grep -Fq 'systemctl enable tilo-rollback-harden.timer' "$containerfile" \
   && ! grep -Fq 'systemctl enable tilo-rollback-harden.service' "$containerfile"; then
  ok 'one-time TPM mutation is scheduled promptly without blocking first login'
else
  bad 'TPM first-boot hardening remains on the greeter critical path'
fi

if exactly_once "$growfs_unit" 'ReadWritePaths=/sysroot' \
   && exactly_once "$growfs_unit" 'PrivateNetwork=yes' \
   && exactly_once "$growfs_unit" 'PrivateMounts=yes' \
   && exactly_once "$growfs_unit" 'MountFlags=slave' \
   && exactly_once "$growfs_unit" "ExecStart=/usr/bin/sh -c 'mount -o remount,rw /sysroot && exec /usr/lib/systemd/systemd-growfs /sysroot'" \
   && ! grep -q '^ExecStartPre=' "$growfs_unit" \
   && exactly_once "$growfs_unit" 'RestrictNamespaces=yes' \
   && exactly_once "$growfs_unit" 'CapabilityBoundingSet=CAP_SYS_ADMIN CAP_SYS_RESOURCE' \
   && exactly_once "$growfs_unit" 'AmbientCapabilities='; then
  ok 'background root growth remounts sysroot only in its slave namespace before resizing'
else
  bad 'background root growth can expose a sysroot remount or lacks resize access'
fi

# The session must be able to use the machine identity without being able to
# unlink or replace its private key. Root provisions it before login in a
# non-writable group-readable directory; public certificates remain separate.
if grep -Eq '^d[[:space:]]+/var/lib/tilo/device-private[[:space:]]+2750[[:space:]]+root[[:space:]]+tilo-identity' "$tmpfiles" \
   && exactly_once "$device_key_unit" 'Before=shell-firstboot.service greetd.service fs.service' \
   && exactly_once "$device_key_unit" 'ConditionPathExists=!/var/lib/tilo/device-private/keypair.bin' \
   && exactly_once "$device_key_unit" 'ReadWritePaths=/var/lib/tilo/device-private' \
   && exactly_once "$device_key_unit" 'PrivateNetwork=yes' \
   && exactly_once "$device_key_unit" 'ProtectProc=invisible' \
   && exactly_once "$device_key_unit" 'RestrictNamespaces=yes' \
   && exactly_once "$device_key_unit" 'MemoryDenyWriteExecute=yes' \
   && exactly_once "$device_key_unit" 'CapabilityBoundingSet=' \
   && containerfile_installs_output "$containerfile" tilo-device-key-init /usr/bin/tilo-device-key-init \
   && grep -Fq 'systemctl enable tilo-device-key-init.service' "$containerfile"; then
  ok 'device private key is provisioned before login only when creation or migration is needed'
else
  bad 'device private key remains replaceable by the session or is not provisioned before login'
fi

# Missing identity is normal until the compositor OOBE is acknowledged. The
# boot timer starts existing identities without putting the mount on the login
# critical path; the change watcher provides activation during first-session OOBE.
if exactly_once "$fs_unit" 'ConditionPathExists=/var/lib/tilo/device/cert.bin' \
   && exactly_once "$fs_path" 'PathChanged=/var/lib/tilo/device/cert.bin' \
   && exactly_once "$fs_path" 'Unit=fs.service' \
   && exactly_once "$fs_timer" 'Unit=fs.service'; then
  ok 'QuickFS waits cleanly for identity and starts on certificate creation'
else
  bad 'QuickFS readiness gate or identity activation path is incomplete'
fi

# A permanently true PathExists path would bypass RestartSec after a real fs
# crash. Only the service owns crash recovery/backoff.
if grep -q '^PathExists=/var/lib/tilo/device/cert.bin$' "$fs_path"; then
  bad 'identity path can immediately retrigger every genuine QuickFS failure'
elif grep -Eq '^RestartSec=([5-9]|[1-9][0-9]+)(s)?$' "$fs_unit"; then
  ok 'genuine QuickFS failures retain a bounded restart backoff'
else
  bad 'QuickFS crash recovery has no meaningful restart backoff'
fi

if exactly_once "$fs_path" 'ConditionKernelCommandLine=!tilo.install' \
   && containerfile_has_copy "$containerfile" \
        'os/usr/lib/systemd/system/fs-identity.path' '/usr/lib/systemd/system/fs-identity.path' \
   && grep -Fq 'systemctl enable tilo-fs-start.timer fs-identity.path' "$containerfile" \
   && grep -Fq 'fs-identity.path' "$boot_mode_generator"; then
  ok 'identity activation is shipped, enabled, and quiet on installer/repair boots'
else
  bad 'identity activation is not wired into the installed image correctly'
fi

# Every Containerfile instruction becomes a rootfs commit with Podman's layer
# cache enabled. Keep the adjacent service activations atomic: splitting these
# no-op-sized filesystem changes into separate RUNs cost 90--120 seconds each
# on the cycle-6 rootless builder, without creating a useful cache boundary.
enable_runs="$(sed -n \
  '/^# Keep service activation in one transaction\./,/^# Boot-time prune\./p' \
  "$containerfile" | grep -c '^RUN systemctl' || true)"
if [ "$enable_runs" -eq 1 ] \
   && exactly_once "$containerfile" '    systemctl enable cups.socket && \' \
   && grep -Fq 'systemctl enable tilo-boot-entries.timer tilo-boot-health.service' "$containerfile" \
   && grep -Fq 'systemctl enable bootc-fetch-apply-updates.timer' "$containerfile" \
   && grep -Fq 'systemctl enable tilo-network-start.timer' "$containerfile"; then
  ok 'service activation retains one useful rootfs layer boundary'
else
  bad 'service activation is fragmented across expensive rootfs commits or incomplete'
fi

exit "$fail"
