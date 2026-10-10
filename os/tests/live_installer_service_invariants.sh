#!/usr/bin/env bash
# Services which need durable machine identity must be quiet on disposable
# installation media and automatically resume once tilo-install seals the target.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
backend="$repo/os/usr/libexec/tilo-install"
containerfile="$repo/os/Containerfile"
fail=0

ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

units=(
  os/usr/lib/systemd/system/fs.service
  os/usr/lib/systemd/system/fs-identity.path
  os/usr/lib/systemd/system/tilo-store-volume.service
  os/usr/lib/systemd/system/tilo-windows-vm-egress-reaper.service
  os/usr/lib/systemd/system/tilo-windows-vm-egress-reaper.timer
  os/usr/lib/systemd/system/tilo-windows-vm-egress-reaper.path
  os/usr/lib/systemd/system/tilo-windows-vm-egress-lease.path
  os/usr/lib/systemd/system/systemd-tpm2-setup.service.d/10-tilo-live-installer.conf
  os/usr/lib/systemd/system/systemd-tpm2-setup-early.service.d/10-tilo-live-installer.conf
)

for relative in "${units[@]}"; do
  unit="$repo/$relative"
  if [ -f "$unit" ] \
    && [ "$(grep -Fxc 'ConditionKernelCommandLine=!tilo.install' "$unit")" -eq 1 ]; then
    ok "$relative is disabled only on the live installer"
  else
    bad "$relative lacks the exact live-installer-only condition"
  fi
done

for dropin in \
  systemd-tpm2-setup.service.d/10-tilo-live-installer.conf \
  systemd-tpm2-setup-early.service.d/10-tilo-live-installer.conf; do
  grep -Fq "COPY os/usr/lib/systemd/system/$dropin /usr/lib/systemd/system/$dropin" \
    "$containerfile" \
    && ok "$dropin is shipped in the image" \
    || bad "$dropin is not copied into the image"
done

# The negative conditions deliberately become true on the installed target.
# Check both UKI specialization and bootc's kernel-argument path so neither
# installation implementation can strand these services in live mode.
if grep -Fq 'target UKI still contains the live installer marker' "$backend" \
   && grep -Fq 'mv -f "$TARGET_UKI.new" "$TARGET_UKI"' "$backend" \
   && [ "$(grep -c -- '--karg-delete tilo.install' "$backend" || true)" -ge 2 ]; then
  ok 'every installed-image path removes the live marker and re-enables services'
else
  bad 'an installed-image path can retain tilo.install and disable normal services'
fi

# Never broaden the gate to similarly named or mutable markers.
if grep -Rqs 'ConditionKernelCommandLine=.*tilo\.installer' \
  "${units[@]/#/$repo/}"; then
  bad 'a unit uses the wrong tilo.installer marker'
else
  ok 'all service gates use the sealed bare tilo.install marker'
fi

exit "$fail"
