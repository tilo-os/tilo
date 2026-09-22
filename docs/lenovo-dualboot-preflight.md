# Lenovo 83ED Windows alongside preflight

This read-only gate blocks the documented physical Lenovo alongside procedure
until Windows prerequisites are met. It does not suspend or decrypt BitLocker,
resize C:, write a USB drive, modify firmware, or install Tilo.

The Linux installer cannot authoritatively inspect Windows BitLocker or the
current largest unallocated extent. Its own backend still refuses to format an
existing partition and revalidates reviewed target identities. This Windows
gate supplies the independent check before the installation medium is booted.

The gate requires either a fully decrypted C: volume or, for the normal
alongside path, a fully encrypted and unlocked C: volume with BitLocker
protection explicitly suspended. It also requires a named safe external
USB/SD/MMC device, an unallocated Windows-disk extent at least as large as the
exact final Tilo requirement, and an unchanged Windows ESP identity. It rejects
partially encrypted/decrypted and locked volumes.

Run from an elevated native Windows PowerShell. First capture a baseline before
changing C: or booting the live medium:

```powershell
python .\os\dev\verify-windows-dualboot-preflight.py collect `
  --windows-disk-number 0 --media-disk-number 4 `
  --output C:\tilo-handoff\windows-before.json
```

After Windows creates the final unallocated extent, independently confirm the
recovery key is accessible and suspend protection for the bounded installation
window. For a normal install plus the first cross-boot checks, three restarts
is sufficient; do not remove TPM or recovery protectors. Then collect again
and verify. Replace the example byte count with the admitted final ESP-plus-root
extent, including the reviewed Windows headroom decision.

```powershell
Suspend-BitLocker -MountPoint C: -RebootCount 3
python .\os\dev\verify-windows-dualboot-preflight.py collect `
  --windows-disk-number 0 --media-disk-number 4 `
  --output C:\tilo-handoff\windows-current.json
python .\os\dev\verify-windows-dualboot-preflight.py verify `
  --baseline C:\tilo-handoff\windows-before.json `
  --receipt C:\tilo-handoff\windows-current.json `
  --required-unallocated-bytes EXACT_TILO_EXTENT_BYTES
```

Only a zero exit status permits continuing with the Lenovo alongside checklist.
Keep both receipts outside the partition being changed. Re-run collection after
installation to compare the Windows ESP again, then boot Windows through its
retained entry and verify recovery and normal login. Resume protection as soon
as that check completes:

```powershell
Resume-BitLocker -MountPoint C:
```

The receipt intentionally excludes recovery secrets. It cannot prove that a
backup or recovery key is accessible, and it does not replace final media
authentication, verified media writing, the disposable ARM GUI cycle, or live
Lenovo display/GPU, input, storage, USB and Wi-Fi checks.
