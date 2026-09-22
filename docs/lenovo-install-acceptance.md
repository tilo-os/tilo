# Lenovo 83ED development installation acceptance

This checklist scopes the first development installation alongside Windows on
the Lenovo 83ED. It does not replace the general release acceptance contract or
claim that the current image is ready.

## Preparation gates

- Produce the complete native ARM image with the Lenovo profile, matching kernel
  device tree and hash-verified firmware.
- Verify the final image, boot-chain signatures, filesystem seal and installation
  medium. Exercise the actual installer and Windows-preserving layout in a
  disposable environment before using the internal disk.
- Prepare a Windows backup and recovery path, a reviewed space allocation, and
  an explicit plan for Windows encryption and firmware trust.
- Keep installation, partition changes, media overwrite and physical firmware
  changes separate from preparation; perform them only when authorized.

## Essential checks in the installation-medium session

Use the intended installation medium on this laptop. No separate, duplicate
live-boot acceptance session is required. Before starting installation, confirm:

- USB/UEFI boot reaches a usable desktop and installer.
- Display rendering and keyboard/touchpad input support the installation flow.
- Internal storage and existing Windows partitions are correctly identified.
- USB remains usable and networking works through the intended connection.
- The reviewed alongside layout preserves Windows and its recovery/boot path.

Do not record these checks as passed from unit tests, generic VM results or
firmware hashes alone. They require observations from the laptop.

## Write the VM-accepted medium on Windows

Do this only after the supervised ARM lane has produced its complete operation
evidence directory and the byte-identical `installer.raw`. Keep
`acceptance.json`, `raw-identity.json`, `finalized-verification.json`,
`cycle-evidence.json` and `cycle-evidence.tar` together. The handoff command refuses receipts that
do not prove independent verification, prepared-alongside installation, cold
boot and desktop acceptance. A 64 GiB raw image needs a device with at least
68,719,476,736 usable bytes; a nominal 64 GB stick is too small, so use a
128 GB device.

Disconnect backup and unrelated removable drives. Inventory disks without
changing them. Run these commands from native Windows PowerShell with a native
Windows Python build (ARM64 is supported), not from WSL. Inventory and planning
do not need elevation; only the final `write` command needs an Administrator
PowerShell:

```powershell
python .\os\dev\write-verified-installer-media.py inventory `
  --output .\lenovo-media-inventory.json
```

Review the selected disk's number, model, unique ID and byte capacity. Create a
read-only plan; this hashes the full raw image and binds it to the successful VM
acceptance receipt and one removable disk:

```powershell
python .\os\dev\write-verified-installer-media.py plan `
  --image C:\tilo-handoff\installer.raw `
  --acceptance C:\tilo-handoff\acceptance.json `
  --disk-number 3 `
  --evidence C:\tilo-handoff\lenovo-media-write.json `
  --output C:\tilo-handoff\lenovo-media-plan.json
```

Read the plan, physically check that it names the intended USB device, then
open an Administrator PowerShell and copy the plan's
`required_confirmation` value exactly:

```powershell
python .\os\dev\write-verified-installer-media.py write `
  --plan C:\tilo-handoff\lenovo-media-plan.json `
  --confirmation 'ERASE PhysicalDrive3 IDHASH MEDIAHASH'
```

The writer rehashes the image before changing the device, checks that the
image, receipt, plan and evidence are not stored on the target, revalidates the
disk immediately before taking it offline, and opens only the reviewed
`PhysicalDriveN`. It writes the raw byte range, reads every byte back, restores
the same device online and records both hashes and the partition table in the
evidence JSON. On a larger device it also clears and reads back the terminal
metadata area so a stale backup partition table cannot survive. Any identity,
capacity, model, safety-flag, hash or read-back
change fails without a success receipt. Do not substitute `dd`, Rufus or a
filesystem copy for this development handoff.

Boot the medium with the firmware's one-time boot menu. Before starting the
actual installation, record these five observations in the installation
record:

- display reaches a usable desktop and installer;
- keyboard and touchpad can operate the complete flow;
- internal storage and the existing Windows partitions are correctly shown;
- USB remains present and usable;
- Wi-Fi connects and passes traffic.

Audio, Bluetooth, battery reporting and suspend/resume remain optional for the
first installation and belong in issue #49.

## Deferred hardware checks

Audio, Bluetooth, battery reporting and suspend/resume are not gates for this
first installation. Track their hardware results and fixes in
[issue #49](https://code.sandbox.example/qos/qos/issues/49) once the base system is
usable. Deferral does not imply these functions work.

## Windows encryption choices

Suspending BitLocker protection keeps the volume encrypted but temporarily
disables its normal protector enforcement. This can support planned boot-chain
changes; it is not full decryption or a replacement for a recovery plan.

Turning BitLocker off decrypts the volume. If that option is selected, verify
that decryption has completed before treating encryption as disabled. Retain
the recovery key until then. Neither option replaces a backup or changes the
requirement to preserve Windows during installation.

Choose and perform the encryption change explicitly; preparation must not
silently suspend protection or start decryption.

## Windows alongside preflight

Before the physical Lenovo alongside procedure, run the read-only,
machine-checkable [Windows dual-boot preflight](lenovo-dualboot-preflight.md).
It blocks that procedure until BitLocker is either fully decrypted or fully
encrypted with its protectors explicitly suspended for the bounded boot-change
window, the final required unallocated extent exists, a safe external
installation medium is identified, and the Windows ESP matches its recorded
baseline. It does not replace the installer's own target-identity checks or
authorize a partition change.

The [Lenovo dual-boot evidence matrix](lenovo-dualboot-audit.md) lists what the
preflight, generic VM acceptance, and the eventual physical clean boot prove.
