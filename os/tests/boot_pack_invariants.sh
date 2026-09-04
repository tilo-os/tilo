#!/usr/bin/env bash
# Invariants for the privileged, reboot-bound BootPack path only.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
manager="$repo/crates/system/system-extension/src/bin/qos-system-extension-manager.rs"
stage="$repo/os/usr/lib/systemd/system/qos-system-extension-boot-pack-stage.service"
path="$repo/os/usr/lib/systemd/system/qos-system-extension-boot-pack-stage.path"
dispatch="$repo/os/usr/libexec/qos-system-extension-dispatch"
catalog="$repo/os/usr/share/qos/system-extensions/hardware-catalog.d/pci-recommendations.json"
trust="$repo/os/usr/share/qos/system-extensions/hardware-trust.d/pci-recommendations.json"

need() { grep -Fq -- "$2" "$1" || { printf 'FAIL %s\n' "$3" >&2; exit 1; }; }

need "$manager" 'verify_pci_catalog_against' 'PCI catalogue signature is not verified'
need "$manager" 'recommended_boot_packs' 'staging does not authorize an exact local PCI match'
need "$manager" '["switch", "--transport=registry", image]' 'bootc is not called with fixed digest-pinned argv'
need "$manager" 'terminal_failure_consumes_the_queue_and_cannot_retry_forever' 'terminal failure queue-consumption test is absent'
need "$path" 'PathExists=/var/lib/qos/system-extensions/boot-pack-request' 'pre-existing queue is stranded after path activation'
need "$path" 'PathChanged=/var/lib/qos/system-extensions/boot-pack-request' 'new queue writes are not activated'
need "$stage" 'CapabilityBoundingSet=CAP_SYS_ADMIN CAP_CHOWN CAP_DAC_OVERRIDE CAP_FOWNER CAP_MKNOD CAP_SETFCAP' 'bootc deployment capabilities are missing'
need "$stage" 'ReadWritePaths=/var/lib/qos/system-extensions /run/qos/system-extension-api /var/lib/containers /var/lib/bootc /ostree /sysroot /boot' 'bootc deployment storage is not explicitly scoped'
need "$dispatch" 'boot-pack-recommend)' 'unprivileged code has no zero-input hardware recommendation request'
need "$catalog" '"signature"' 'shipped PCI recommendation catalogue is unsigned'
need "$trust" '"public_key_hex"' 'shipped PCI recommendation trust root is absent'

if grep -Fq 'PrivateDevices=yes' "$stage" || awk '$1 !~ /^#/ && /@mount/' "$stage" | grep -q .; then
    printf '%s\n' 'FAIL bootc deployment remains blocked by device or mount restrictions' >&2
    exit 1
fi

printf '%s\n' 'ok: BootPack policy, durable queue, and executable bootc boundary hold'
