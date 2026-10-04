# Verified Lenovo installer USB handoff

Use this only after a 64 GiB `installer.raw` and its matching accepted ARM VM
receipt or profile-specific hardware receipt exist. It writes one full raw image to a removable device,
so a 128 GB USB drive is required. The wrapper does not write anything until it
has generated a plan for the currently attached device and the operator types
that plan's exact confirmation phrase.

Disconnect other removable media. The prepared Lenovo target was observed as
the SSK USB3.2 device on `PhysicalDrive1` (volume label `Vincent USB`,
125,068,902,400 bytes); do not rely on that number after reconnecting it.
`Get-Disk` inventory in the wrapper and its generated plan are authoritative.

From an Administrator native Windows PowerShell in the repository, run this
single command, substituting the final accepted operation directory. It plans,
shows the bound target identity, requires the exact typed confirmation, writes,
then reads back all 64 GiB before it emits success evidence:

```powershell
& .\os\dev\Write-TiloInstallerUsb.ps1 `
  -Image C:\tilo-handoff\accepted\installer.raw `
  -Acceptance C:\tilo-handoff\accepted\acceptance.json `
  -DiskNumber 1 `
  -HandoffDirectory C:\tilo-handoff\accepted `
  -PythonPath 'C:\Users\you\AppData\Local\Programs\Python\Python311-arm64\python.exe'
```

To perform only the non-destructive planning pass, append `-PlanOnly`. The
writer refuses internal, boot, system, offline, read-only, non-removable,
identity-less, too-small, and source-containing disks. It locks and hashes the
raw image before changing the USB, takes only the reviewed device offline,
rechecks the retained raw handle, clears stale trailing metadata, reads every
written image byte back, restores the same device online, and records the
partition-table read-back in `lenovo-media-write.json`.

Do not replace this procedure with `dd`, Rufus, Etcher, Explorer, or a
filesystem copy: those do not bind the device to the accepted VM evidence or
provide the full raw-image read-back receipt.
