#!/usr/bin/env bash
# Invariants for the privileged, reboot-bound BootPack path only.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
manager="$repo/crates/system/system-extension/src/bin/tilo-system-extension-manager.rs"
stage="$repo/os/usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.service"
path="$repo/os/usr/lib/systemd/system/tilo-system-extension-boot-pack-stage.path"
dispatch="$repo/os/usr/libexec/tilo-system-extension-dispatch"
catalog="$repo/os/usr/share/tilo/system-extensions/hardware-catalog.d/pci-recommendations.json"
trust="$repo/os/usr/share/tilo/system-extensions/hardware-trust.d/pci-recommendations.json"

need() { grep -Fq -- "$2" "$1" || { printf 'FAIL %s\n' "$3" >&2; exit 1; }; }

need "$manager" 'verify_pci_catalog_against' 'PCI catalogue signature is not verified'
need "$manager" 'recommended_boot_packs' 'staging does not authorize an exact local PCI match'
need "$manager" '["switch", "--transport=registry", image]' 'bootc is not called with fixed digest-pinned argv'
need "$manager" 'fn catalog_ids_and_terminal_failures_are_bounded' 'terminal failure queue-consumption test is absent'
need "$path" 'PathExists=/var/lib/tilo/system-extensions/boot-pack-request' 'pre-existing queue is stranded after path activation'
need "$path" 'PathChanged=/var/lib/tilo/system-extensions/boot-pack-request' 'new queue writes are not activated'
need "$stage" 'CapabilityBoundingSet=CAP_SYS_ADMIN CAP_CHOWN CAP_DAC_OVERRIDE CAP_FOWNER CAP_MKNOD CAP_SETFCAP' 'bootc deployment capabilities are missing'
need "$stage" 'ReadWritePaths=/var/lib/tilo/system-extensions /run/tilo/system-extension-api /var/lib/containers /var/lib/bootc /ostree /sysroot /boot' 'bootc deployment storage is not explicitly scoped'
need "$dispatch" 'boot-pack-recommend)' 'unprivileged code has no zero-input hardware recommendation request'
need "$catalog" '"signature"' 'shipped PCI recommendation catalogue is unsigned'
need "$trust" '"public_key_hex"' 'shipped PCI recommendation trust root is absent'

if grep -Fq 'PrivateDevices=yes' "$stage" || awk '$1 !~ /^#/ && /@mount/' "$stage" | grep -q .; then
    printf '%s\n' 'FAIL bootc deployment remains blocked by device or mount restrictions' >&2
    exit 1
fi

printf '%s\n' 'ok: BootPack policy, durable queue, and executable bootc boundary hold'
