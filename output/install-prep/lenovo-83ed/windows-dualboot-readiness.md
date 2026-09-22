# Lenovo 83ED Windows dual-boot readiness

Latest host snapshot: 2026-09-23 00:42 Europe/Berlin. The audit was read-only.
It did not resize a partition, change BitLocker, alter Windows Recovery
Environment (WinRE), or change boot configuration. The elevated Windows facts
listed below are intentionally marked with their last verification time.

## Internal disk layout

The internal drive is a healthy 1,024,209,543,168-byte Samsung NVMe disk using GPT.

| Partition | Offset | Size | Role | Action |
| --- | ---: | ---: | --- | --- |
| 1 | 1,048,576 | 272,629,760 bytes (260 MiB) | FAT32 Windows EFI System Partition (`SYSTEM_DRV`) | Preserve as Windows-only; do not modify or format |
| 2 | 273,678,336 | 134,217,728 bytes (128 MiB) | Microsoft Reserved Partition | Preserve |
| 3 | 407,896,064 | 1,021,704,142,848 bytes (951.54 GiB) | Windows `C:` NTFS | Shrink from its end in Windows |
| 4 | 1,022,112,038,912 | 2,097,152,000 bytes (1.953 GiB) | Windows Recovery (`WINRE_DRV`) | Preserve in place |

There is no unallocated space between `C:` and WinRE today. Shrinking `C:` creates a contiguous installation region immediately before WinRE without moving the recovery partition.

The installed ReAgent configuration points to partition 4 at the exact offset shown above, has automatic repair enabled, and reports the recovery image as installed. The next elevated pre-install check must still run `reagentc /info` and require `Windows RE status: Enabled` before partitioning.

## Windows capacity and proposed split

The current read-only snapshot reports about 839.02 GiB used and 112.51 GiB
free on `C:`. The proposed target is:

- Windows `C:`: **800 GiB** (858,993,459,200 bytes)
- New unallocated Tilo region: **151.54 GiB** (162,710,683,648 bytes), divided into a dedicated exact 2 GiB Tilo ESP and about 149.54 GiB for the Tilo root
- Existing ESP, MSR, and WinRE: unchanged

This split provides substantially more than the 64 GiB media/image baseline while leaving most of the disk to Windows. The existing 260 MiB Windows ESP is too small for, and excluded by, the current guided-installer contract; Tilo gets its own exact 2 GiB ESP with at least 1 GiB free. The current data set still exceeds the proposed Windows target: move or remove at least 39.02 GiB to fit the target, or 79.02 GiB to retain the planned 40 GiB Windows headroom after shrinking.

An elevated audit on 2026-09-20 found Windows' supported shrink limit was only 307,191,808 bytes (292.96 MiB), even with about 92 GiB free at that time. That is the safe maximum verified by Windows, and it is insufficient. The 2026-09-23 non-elevated snapshot cannot query a new `SizeMin`; cleanup alone is not proof that the partition can shrink. Rerun `Get-PartitionSupportedSize` after cleanup and require `SizeMin` at or below 858,993,459,200 bytes.

Use Windows' storage API for the eventual resize, only after the prerequisite checks pass:

```powershell
$target = 800GB
$supported = Get-Partition -DriveLetter C | Get-PartitionSupportedSize
if ($supported.SizeMin -gt $target) {
    throw "Windows cannot yet shrink C: to 800 GiB; current minimum is $($supported.SizeMin) bytes"
}
Resize-Partition -DriveLetter C -Size $target
```

Do not delete or move partition 4 to work around the shrink limit. Find and relocate the immovable NTFS allocation first. An elevated `defrag C: /A /V` and Event Viewer application event 259 after a failed shrink query identify the blocking file/cluster. Common candidates are VHDX files, the page file, restore data, or NTFS metadata; the result must determine the remedy.

## Encryption and Windows boot state

The most recent elevated BitLocker audit (2026-09-20) reported:

- `C:` fully encrypted with XTS-AES-128
- protection enabled and volume unlocked
- TPM and recovery-password protectors present
- no recovery password was read or recorded by this audit

Full decryption is not required to use Windows' supported shrink operation. Before changing the partition table or firmware boot entries, independently verify that the recovery key is accessible, then suspend BitLocker protection for a bounded reboot count. Resume protection after both Windows and Tilo have booted successfully. Do not remove either protector.

The 2026-09-23 snapshot still finds hibernation disabled and `hiberfil.sys` absent. Windows reports Fast Startup unavailable because hibernation is unavailable. The stale `HiberbootEnabled=1` registry preference has no operational effect in this state. Secure Boot is enabled according to the native Windows registry; keep it enabled for normal Windows operation and use the firmware's one-time boot menu when testing signed media. Recheck the final state immediately before installation; hibernation/Fast Startup must remain unavailable while sharing a disk with Tilo.

## Installation gate

Internal installation is ready to proceed only when all of these are true:

1. Cleanup leaves enough data below the 800 GiB target with practical Windows headroom.
2. An elevated `Get-PartitionSupportedSize` reports `SizeMin <= 858993459200`.
3. `chkdsk C: /scan` succeeds and all four existing partitions still match this map.
4. `reagentc /info` reports WinRE enabled on partition 4.
5. BitLocker recovery access is independently confirmed, then protection is suspended for the installation reboot window.
6. `powercfg /a` still reports hibernation and Fast Startup unavailable.
7. The installer selects only the newly unallocated 151.54 GiB region, creates its exact 2 GiB ESP plus Tilo root there, and preserves partitions 1, 2, 3, and 4.

At present, item 2 is the hard partitioning blocker and item 1 is its likely prerequisite.

## Latest shrink diagnosis and fastest safe sequence

The latest read-only Application event 259 (2026-09-21 21:38 Europe/Berlin)
identifies `\pagefile.sys::$DATA` as the last immovable allocation. The current
automatically managed pagefile is 34.11 GiB. C: now has about 112.51 GiB free,
so reaching the exact 800 GiB target needs about 39.02 GiB more free merely to
fit, or about 79.02 GiB more free to retain the planned 40 GiB of Windows
headroom. The pagefile still requires the reversible preparation and reboot
before the final shrink-size measurement.

Do not change paging while local image extraction or a VM is active. Once the
local Tilo boot attempt is complete, the shortest evidence-driven sequence is:

1. Remove completed local build/VM artifacts and other reviewed data until the
   read-only report shows `AdditionalFreeBytesNeeded` is zero.
2. Close applications and VMs. In one elevated PowerShell, run
   `scripts/windows-dual-boot-preflight.ps1 -PrepareShrink` with a checkpoint
   path outside temporary build storage. This records paging configuration,
   disables paging and hibernation, and performs no resize or BitLocker action.
3. Reboot Windows. Run the default report elevated and require
   `SupportedSize.SizeMin <= 858993459200` plus a clean `chkdsk` and enabled
   WinRE. If event 259 names a different tail file, handle that exact file;
   do not generically delete restore points, the USN journal, or WinRE.
4. Suspend BitLocker separately only for the bounded installation window, then
   run the single reviewed `Resize-Partition` command from the install plan.
5. Restore the checkpoint with `-RestoreWindowsState` and reboot. Normal paging
   must be active before returning to build workloads.

## Final elevated fact collection

After local VM work has stopped and the additional space has been freed, run
this exact block in one elevated native Windows PowerShell. It is read-only: it
only creates the JSON receipt and gathers the facts Windows does not expose to
an ordinary process. A nonzero exit means a gate is unsatisfied; the receipt is
still written for inspection.

```powershell
$repo = 'C:\Users\xaver\qos-tilo-install-ready'
$handoff = 'C:\tilo-handoff\windows-shrink-readiness.json'
New-Item -ItemType Directory -Force (Split-Path -Parent $handoff) | Out-Null
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File `
  "$repo\scripts\windows-dual-boot-preflight.ps1" -OutputPath $handoff
$exit = $LASTEXITCODE
$report = Get-Content -Raw $handoff | ConvertFrom-Json
$report.SupportedSize, $report.BitLocker, $report.BitLockerProtectors, $report.WinRE, $report.CheckDisk
"preflight exit code: $exit"
```

Proceed only when `SupportedSize.TargetAdmitted` is true, `CheckDisk.Succeeded`
is true, WinRE reports enabled, and the BitLocker report still lists the TPM and
recovery-password protectors. The later `-PrepareShrink` mode remains the
separate reversible action for removing the pagefile as the final shrink
obstacle; it is not part of the fact-collection block.
