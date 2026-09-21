# Lenovo 83ED Windows dual-boot readiness

Audit time: 2026-09-21 21:30 Europe/Berlin. The audit was read-only. It did not resize a partition, change BitLocker, or alter Windows Recovery Environment (WinRE).

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

Current `C:` usage is about 904.38 GiB used and 47.18 GiB free. The proposed target is:

- Windows `C:`: **800 GiB** (858,993,459,200 bytes)
- New unallocated Tilo region: **151.54 GiB** (162,710,683,648 bytes), divided into a dedicated exact 2 GiB Tilo ESP and about 149.54 GiB for the Tilo root
- Existing ESP, MSR, and WinRE: unchanged

This split provides substantially more than the 64 GiB media/image baseline while leaving most of the disk to Windows. The existing 260 MiB Windows ESP is too small for, and excluded by, the current guided-installer contract; Tilo gets its own exact 2 GiB ESP with at least 1 GiB free. The split is not currently achievable: at least 104.38 GiB must be removed from `C:` merely to fit, and about 144.38 GiB must be removed to leave Windows 40 GiB free after shrinking.

An elevated audit on 2026-09-20 found Windows' supported shrink limit was only 307,191,808 bytes (292.96 MiB), even with about 92 GiB free at that time. That is the safe maximum verified by Windows, and it is insufficient. The cause is an immovable allocation near the NTFS tail, not the partition table. Cleanup alone is not proof that the partition can shrink: rerun `Get-PartitionSupportedSize` after cleanup and require `SizeMin` at or below 858,993,459,200 bytes.

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

Hibernation is disabled and `hiberfil.sys` is absent. Windows reports Fast Startup unavailable because hibernation is unavailable. The stale `HiberbootEnabled=1` registry preference has no operational effect in this state. Recheck `powercfg /a` immediately before installation; hibernation/Fast Startup must remain unavailable while sharing a disk with Tilo.

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
