#!/usr/bin/env bash
# First-boot prerequisites must be expected state, not crash loops or red units.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
harden="$repo/os/usr/lib/systemd/system/qos-rollback-harden.service"
harden_helper="$repo/os/usr/libexec/qos-rollback-harden"
fs_unit="$repo/os/usr/lib/systemd/system/fs.service"
fs_path="$repo/os/usr/lib/systemd/system/fs-identity.path"
containerfile="$repo/os/Containerfile"
# shellcheck source=lib/containerfile.sh
. "$repo/os/tests/lib/containerfile.sh"
boot_mode_generator="$repo/os/usr/lib/systemd/system-generators/qos-boot-mode-generator"
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

# Missing identity is normal until the compositor OOBE is acknowledged. The
# service stays enabled for ordered later boots; the change watcher provides
# event-driven activation during the first session.
if exactly_once "$fs_unit" 'ConditionPathExists=/var/lib/qos/device/cert.bin' \
   && exactly_once "$fs_path" 'PathChanged=/var/lib/qos/device/cert.bin' \
   && exactly_once "$fs_path" 'Unit=fs.service'; then
  ok 'QuickFS waits cleanly for identity and starts on certificate creation'
else
  bad 'QuickFS readiness gate or identity activation path is incomplete'
fi

# A permanently true PathExists path would bypass RestartSec after a real fs
# crash. Only the service owns crash recovery/backoff.
if grep -q '^PathExists=/var/lib/qos/device/cert.bin$' "$fs_path"; then
  bad 'identity path can immediately retrigger every genuine QuickFS failure'
elif grep -Eq '^RestartSec=([5-9]|[1-9][0-9]+)(s)?$' "$fs_unit"; then
  ok 'genuine QuickFS failures retain a bounded restart backoff'
else
  bad 'QuickFS crash recovery has no meaningful restart backoff'
fi

if exactly_once "$fs_path" 'ConditionKernelCommandLine=!qos.install' \
   && containerfile_has_copy "$containerfile" \
        'os/usr/lib/systemd/system/fs-identity.path' '/usr/lib/systemd/system/fs-identity.path' \
   && grep -Fq 'systemctl enable fs.service fs-identity.path' "$containerfile" \
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
   && grep -Fq 'systemctl enable qos-boot-entries.service qos-boot-health.service' "$containerfile" \
   && grep -Fq 'systemctl enable bootc-fetch-apply-updates.timer' "$containerfile" \
   && grep -Fq 'systemctl enable NetworkManager.service' "$containerfile"; then
  ok 'service activation retains one useful rootfs layer boundary'
else
  bad 'service activation is fragmented across expensive rootfs commits or incomplete'
fi

exit "$fail"
