# Lenovo 83ED Tilo alongside-install plan

Status: prepared from read-only observations on 2026-09-21. This document does
not authorize or perform the resize or installation.

## Chosen layout

The internal disk is the 1,024,209,543,168-byte Samsung NVMe device with GPT
UUID `e6a469fd-e19b-4974-ad23-9c234a4e225a` and Windows identity
`eui.002538A441BE8A46`. Keep all four Windows partitions. Windows must shrink
only the end of C:; Tilo then appends two partitions inside the resulting
single free extent before WinRE.

| GPT entry | Byte offset | Byte size | Role | Install action |
| --- | ---: | ---: | --- | --- |
| 1 | 1,048,576 | 272,629,760 | Windows ESP, PARTUUID `6e3c8d29-eefd-452f-a856-9bc30f84aace` | Preserve byte-for-byte |
| 2 | 273,678,336 | 134,217,728 | Microsoft reserved, PARTUUID `e8e71c5f-d195-4e58-b0f4-d3076c3a936b` | Preserve byte-for-byte |
| 3 | 407,896,064 | **858,993,459,200** | Windows C:, PARTUUID `fa6ad2c9-bd0e-4780-9d68-324b93376d01` | Shrink in Windows to exactly 800 GiB |
| 5 (new) | 859,401,355,264 | 2,147,483,648 | Dedicated FAT32 Tilo ESP, PARTUUID `162e4fc3-6689-48ca-868e-d124ecbf887f` | Create and format as `TILO-EFI` |
| 6 (new) | 861,548,838,912 | 160,563,200,000 | Encrypted Tilo root, PARTUUID `29651c1b-7556-4066-b6c8-6114008ff60a` | Create; installer initializes LUKS2 and Btrfs |
| 4 | 1,022,112,038,912 | 2,097,152,000 | Windows Recovery, PARTUUID `cbb0cbe1-feef-44dd-948b-409061320e11` | Preserve in place |

GPT entry numbers 5 and 6 are expected because the helper appends entries even
though their byte offsets precede entry 4. The free extent is exactly
162,710,683,648 bytes (151.536 GiB). The new 2 GiB ESP and 149.536 GiB root
fill that extent and end exactly where WinRE starts.

The existing Windows ESP is only 260 MiB. Tilo's current alongside contract
requires a 2 GiB ESP with at least 1 GiB free for retained, staged and recovery
UKIs. The guided path therefore creates a dedicated Tilo ESP and never mounts,
formats or writes the Windows ESP. Each firmware entry points directly to the
loader on its own ESP.

## Gate 1: prepare Windows

The backup is complete. Before resizing, confirm that the BitLocker recovery
password is accessible from another device or printed record; do not copy it
into this repository. Keep the TPM and recovery-password protectors. Suspend
protection for the installation reboot window rather than removing protectors:

```powershell
Suspend-BitLocker -MountPoint C: -RebootCount 0
```

`-RebootCount 0` keeps protection suspended until it is explicitly resumed.
If the current Windows preflight lane is used unchanged, it deliberately
requires full decryption instead; in that stricter mode run
`Disable-BitLocker -MountPoint C:` and wait until `VolumeStatus` is
`FullyDecrypted`. Do not confuse suspended protection with decryption.

Run the remaining commands in one elevated native Windows PowerShell. They are
read-only until the separately marked resize command:

```powershell
$targetC = 858993459200L
$requiredFreeBeforeShrink = 205660356608L # shrink extent plus 40 GiB Windows headroom

$disk = Get-Disk -Number 0
$c = Get-Partition -DriveLetter C
$volume = Get-Volume -DriveLetter C
$supported = $c | Get-PartitionSupportedSize

if ($disk.Guid -ne '{e6a469fd-e19b-4974-ad23-9c234a4e225a}' -or
    $disk.UniqueId -ne 'eui.002538A441BE8A46' -or
    $disk.Size -ne 1024209543168L) { throw 'internal disk identity changed' }
if ($volume.SizeRemaining -lt $requiredFreeBeforeShrink) {
    throw "C: needs at least $requiredFreeBeforeShrink free bytes before shrinking"
}
if ($supported.SizeMin -gt $targetC) {
    throw "immovable data still prevents the planned shrink; SizeMin=$($supported.SizeMin)"
}
if ((Get-BitLockerVolume -MountPoint C:).ProtectionStatus -ne 'Off') {
    throw 'BitLocker protection is not suspended/off'
}
chkdsk C: /scan
reagentc /info
powercfg /a
```

Require a clean `chkdsk`, WinRE enabled on partition 4, and hibernation/Fast
Startup unavailable. The last elevated measurement allowed only a
307,191,808-byte shrink, so cleanup alone is insufficient evidence. If
`SizeMin` remains too large, run `defrag C: /A /V` and inspect Windows
Application event 259 to identify the exact immovable tail allocation. Resolve
that file (commonly a VHDX, page file, restore data or NTFS allocation), reboot,
and rerun the gate. Never move or delete WinRE partition 4 to gain space.

Only after every gate passes, perform the one Windows disk mutation:

```powershell
Resize-Partition -DriveLetter C -Size 858993459200L
```

Then require `Get-Disk -Number 0` to report a 162,710,683,648-byte largest free
extent and confirm the existing PARTUUID/offset map. Do not create or format
the Tilo partitions in Windows; the guided installer owns that exact step.

## Gate 2: boot and review the installation medium

Use the Lenovo one-time UEFI boot menu. For the development medium, Secure Boot
must follow the separate firmware plan for its actual signing state. In the
live session, require all of the following before starting installation:

1. usable display and installer UI;
2. working keyboard and touchpad;
3. the 1,024,209,543,168-byte Samsung NVMe and all four Windows partitions;
4. the USB medium remains present and is not selected as the target;
5. working network access.

Record the live Linux NVMe path (normally `/dev/nvme0n1`), `MAJ:MIN`, WWN and
serial. Abort if the GPT UUID, disk size, logical sector size or any existing
partition tuple differs from this plan.

## Exact guided-install contract

The GUI should produce this backend contract. The shell form below is a
reviewable fallback and intentionally computes only the kernel-assigned device
number and stable Linux identity at runtime. The account password is stdin line
1 and the strong disk passphrase is line 2; neither belongs in command history.

```bash
DISK=/dev/nvme0n1
DEVNUM="$(lsblk -dno MAJ:MIN "$DISK" | xargs)"
WWN="$(lsblk -dno WWN "$DISK" | xargs)"
SERIAL="$(lsblk -dno SERIAL "$DISK" | xargs)"
IDENTITY_ARGS=()
[ -z "$WWN" ] || IDENTITY_ARGS+=(--expect-disk-wwn "$WWN")
[ -z "$SERIAL" ] || IDENTITY_ARGS+=(--expect-disk-serial "$SERIAL")
[ "${#IDENTITY_ARGS[@]}" -gt 0 ] || { echo 'no stable NVMe identity' >&2; exit 1; }

read -rsp 'Tilo account password: ' ACCOUNT_PASSWORD; echo
read -rsp 'Tilo disk passphrase: ' DISK_PASSPHRASE; echo
printf '%s\n%s\n' "$ACCOUNT_PASSWORD" "$DISK_PASSPHRASE" |
  sudo /usr/libexec/tilo-install \
    --disk "$DISK" \
    --guided-alongside --confirm-free-space "$DISK" \
    --expect-disk-size-bytes 1024209543168 \
    --expect-disk-devnum "$DEVNUM" "${IDENTITY_ARGS[@]}" \
    --expect-disk-ptuuid e6a469fd-e19b-4974-ad23-9c234a4e225a \
    --expect-logical-sector-size 512 \
    --free-start-bytes 859401355264 \
    --free-size-bytes 162710683648 \
    --efi-start-bytes 859401355264 \
    --efi-size-bytes 2147483648 \
    --root-start-bytes 861548838912 \
    --root-size-bytes 160563200000 \
    --new-efi-partuuid 162e4fc3-6689-48ca-868e-d124ecbf887f \
    --new-root-partuuid 29651c1b-7556-4066-b6c8-6114008ff60a \
    --expect-partition 6e3c8d29-eefd-452f-a856-9bc30f84aace:1048576:272629760 \
    --expect-partition e8e71c5f-d195-4e58-b0f4-d3076c3a936b:273678336:134217728 \
    --expect-partition fa6ad2c9-bd0e-4780-9d68-324b93376d01:407896064:858993459200 \
    --expect-partition cbb0cbe1-feef-44dd-948b-409061320e11:1022112038912:2097152000 \
    --user xaver --full-name 'Xaver Emmerling' \
    --hostname tilo-lenovo --locale de_DE.UTF-8 \
    --timezone Europe/Berlin --keymap de \
    --filesystem btrfs --encrypt --pin --yes
unset ACCOUNT_PASSWORD DISK_PASSPHRASE
```

The guided helper takes an exclusive disk lock, revalidates the complete GPT
map and free extent, appends only the two planned entries, formats only the new
ESP, and hands only the new root to LUKS setup. Any identity or geometry drift
must stop the install.

## Rollback points

1. **Before the Windows resize:** resume BitLocker; no layout rollback exists
   because no layout changed.
2. **After the resize, before Tilo partitions exist:** Windows remains complete.
   If abandoning installation, extend C: back to `1021704142848` bytes with
   `Resize-Partition`; this is safe only while the whole gap is still free.
3. **After entries 5/6 exist, before successful install:** remove only the two
   partitions after verifying both planned PARTUUIDs and exact byte geometry,
   then extend C:. Never delete by partition number alone.
4. **After successful install:** keep Windows ESP, MSR and WinRE untouched. A
   Tilo removal consists of its exact root and dedicated ESP plus its firmware
   entry, followed by extending C:. Preserve the install marker/recovery receipt
   until removal is complete.

The installer snapshots firmware boot state and restores it on failure. The
dedicated ESP means a failed Tilo install has no reason to modify Windows EFI
loader bytes.

## Post-install acceptance

Cold-boot Tilo and verify the encrypted root, dedicated ESP, selected UKI,
composefs seal, desktop, keyboard/touchpad, storage, USB and network. Then use
the firmware boot menu to cold-boot Windows, confirm normal login, run
`reagentc /info`, and compare partitions 1-4 to this plan. After both boots:

```powershell
Resume-BitLocker -MountPoint C:
```

Keep the recovery password until BitLocker protection is confirmed On and both
operating systems have completed another cold boot.
