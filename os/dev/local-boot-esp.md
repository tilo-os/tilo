# Local ARM64 early-boot iteration

Use a small, replaceable EFI System Partition (ESP) to test a candidate ARM64
UKI without rebuilding, recopying, or modifying the full OS disk. This lane is
for firmware, bootloader, UKI, kernel, initramfs, and early userspace changes.

## Build the override ESP

Run this in the local Fedora ARM64 builder. It needs `util-linux` and `mtools`:

```sh
./os/dev/make-boot-esp.sh \
  --base /path/to/tilo-base.raw \
  --uki /path/to/candidate.efi \
  --root-partuuid 11111111-2222-4333-8444-555555555555 \
  --output /path/to/tilo-boot-override.raw
```

The root PARTUUID must be the identity already embedded in or selected by the
candidate's boot configuration. The helper verifies that it occurs exactly once
in the base disk before writing anything. It installs the UKI at the ARM64 UEFI
fallback path, `EFI/BOOT/BOOTAA64.EFI`.

To exercise systemd-boot as well, provide its ARM64 EFI binary:

```sh
./os/dev/make-boot-esp.sh \
  --base /path/to/tilo-base.raw \
  --uki /path/to/candidate.efi \
  --root-partuuid 11111111-2222-4333-8444-555555555555 \
  --bootloader /usr/lib/systemd/boot/efi/systemd-bootaa64.efi \
  --output /path/to/tilo-boot-override.raw
```

Each run emits `<output>.receipt.json` with the UKI, bootloader, and ESP SHA-256
hashes plus the exact base partition table and selected root PARTUUID. ESP disk
and partition UUIDs derive from the UKI hash, which avoids accumulating new UEFI
device identities for repeated copies of the same candidate.

## Boot locally on Windows ARM64

```powershell
.\tools\launch-arm64-boot-esp.ps1 `
  -BaseDisk '\\wsl.localhost\FedoraLinux-42\home\xemmerling\tilo-vm\media\disk.raw' `
  -EspDisk '\\wsl.localhost\FedoraLinux-42\home\xemmerling\tilo-vm\media\tilo-boot-override.raw' `
  -CheckOnly

.\tools\launch-arm64-boot-esp.ps1 `
  -BaseDisk '\\wsl.localhost\FedoraLinux-42\home\xemmerling\tilo-vm\media\disk.raw' `
  -EspDisk '\\wsl.localhost\FedoraLinux-42\home\xemmerling\tilo-vm\media\tilo-boot-override.raw'
```

QEMU attaches the ESP read-only at boot index 1 and the OS disk at boot index 2.
Writes to the OS disk go to QEMU's disposable snapshot. Replacing a candidate
therefore copies only the UKI-sized payload and a small ESP, while the installed
root disk and its PARTUUID remain unchanged.

## Limits

- This proves the candidate through early userspace against the retained root.
  It does not replace a clean boot of the production, same-disk ESP layout.
- A candidate that discovers root only by looking beside the loader's ESP cannot
  use a separate ESP. Give it an explicit, verified root PARTUUID for this lane.
- A UKI bound to a composefs digest must still match the retained root content.
  Root filesystem changes require a new matching base disk.
- Secure Boot must be disabled for unsigned development UKIs. Signed candidates
  can exercise Secure Boot when the matching authority is enrolled in the UEFI
  variable store.
