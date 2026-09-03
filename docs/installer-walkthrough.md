# qOS installation: download to first login

This is the user journey and the release acceptance checklist for qOS Imager
and the live installer. A release is not complete unless the walkthrough works
on Linux, Windows, and macOS with a real removable drive, and then in a UEFI VM
for both erase-disk and alongside installation.

For the shorter user guide, start with [`getting-started.md`](getting-started.md).
The machine-readable platform and requirement source of truth is
[`install-contract.json`](install-contract.json); release tooling copies that
contract to the website only after the matching artifacts pass promotion.

## Before downloading

Back up the computer. For a Windows dual boot, save the BitLocker recovery key,
turn off Fast Startup, and use Windows Disk Management to shrink Windows. Leave
at least **34 GiB unallocated**. qOS creates a dedicated 2 GiB EFI System
Partition and uses the rest for its encrypted root. It intentionally does not
resize NTFS: Windows understands BitLocker, hibernation, and its recovery
partitions better than an offline Linux tool can.

Changing UEFI boot files, Secure Boot keys, or boot order can cause BitLocker to
ask for its recovery key even when the Windows partition was not modified.
That prompt is a security feature, not data loss.

## Download and launch qOS Imager

Download the signed qOS Imager from the landing page or qOS release page. The
release matrix is Linux x86-64/ARM64 (AppImage, portable tar,
Homebrew), Windows x86-64 (setup EXE, standalone EXE, WinGet), and macOS
x86-64/Apple silicon (DMG, app zip, Homebrew cask). Verify the published
SHA-256 digest before launching it. Never use
an imager or OS image obtained from a mirror whose digest is not listed by qOS.
Each signed host installer embeds the same release catalog. It names the
architecture-matched compressed live installer by immutable tag and includes
SHA-256 digests for both the downloaded archive and expanded raw disk image.
The Imager caches and verifies both before allowing the medium to be written.
The cross-platform build workflow also emits digests, but its unsigned CI
artifacts are for validation only: they must not be presented as production
downloads. Code signing on Linux and Windows and signing plus notarization on
macOS are release gates performed with protected release credentials. An
`imager-v*` tag fails rather than producing a release artifact if any required
credential is absent; see `apps/imager/packaging/README.md`.

- Linux: launch the signed application bundle and authorize its polkit prompt.
- Windows: launch qOS Imager and accept the Administrator prompt. The app uses
  `Get-Disk`, takes only the chosen physical disk offline during the write, and
  brings it online afterwards.
- macOS: launch the notarized app and authorize its administrator prompt. The app lists only
  external physical media, unmounts the selected disk with `diskutil`, writes
  through `/dev/rdiskN`, verifies it, and ejects it.

The application starts with no fabricated releases or drives. Choose a local
`.iso`, `.img`, or `.raw` file using the operating system's own file dialog.
Insert a removable drive, refresh, select it, and inspect its model, capacity,
and current partitions. Internal and running-system disks are hidden behind
additional acknowledgement. The confirmation page requires the exact device
name. Immediately before opening the device, qOS probes it again and refuses if
its stable hardware/media ID, capacity, model, removability, or system-disk
status changed. Writing is followed by a byte-for-byte readback; “Complete”
means that verification succeeded, not merely that the write call returned.

Everything currently on the selected installation medium is erased.

For a qOS image, Customize collects the non-destructive install profile before
the medium is written: account name, host name, locale, keyboard, time zone and
filesystem. The imager writes a checksummed, secret-free profile into a fixed
reserved block on the medium and reads it back during verification. Before the
live UI starts, a privileged one-shot service extracts and verifies that block;
the installer validates every field again, applies it, and starts at the first
local decision. Disk selection, erase/alongside choice, a fresh account/startup
secret and recovery-key acknowledgement remain on the target computer. The
Review screen shows the resulting choices before any destructive operation.

The current host bundles elevate the imager process for raw-device access; the
application still has its typed target gate, system-disk acknowledgement, and
readback verification. Flatpak is not offered because safely supporting raw
media from its sandbox requires a separately installed privileged host helper;
elevating the Flatpak itself would not provide a meaningful security boundary.

## Boot the installation medium

Reboot and choose the USB drive in the firmware's one-time boot menu. Keep UEFI
mode enabled. The live installer asks for keyboard first, then optional network,
disk method, account, filesystem/encryption, and a final review. No OS download
is required: installation uses the signed image on the medium.

### Erase disk

This removes every partition and operating system on the selected disk. The
review page names the exact disk and repeats the impact. Use this for an empty
computer or when qOS will be its only operating system. Installation remains
disabled until you type the displayed device name exactly; changing the disk or
switching install methods clears that confirmation.

### Install alongside

This preserves the GPT and existing OS partitions. The guided path is offered
when exactly one aligned, unallocated GPT extent of at least 34 GiB exists. The
review names the exact extent and the new 2 GiB qOS ESP plus >=32 GiB root. The
privileged backend locks the disk, revalidates its stable hardware identity,
GPT UUID, logical sector size, and the PARTUUID/start/size of every existing
partition, then appends only those two new partitions. It never deletes, moves,
resizes, or formats an existing partition.

The expert prepared-partition path remains available when the disk has both:

1. an unmounted FAT32 EFI System Partition of at least 2 GiB, with at least
   1 GiB currently available for qOS's signed boot images; and
2. an unmounted, signature-free Linux partition of at least 32 GiB.

For the prepared path, the review page names both partitions. The privileged backend independently
checks their device ancestry, GPT types, size, mount state, signatures, FAT
filesystem health, and EFI free space just before formatting. It formats only the named qOS root and
mounts the EFI partition without formatting it. Small Windows/Linux EFI
partitions are deliberately ignored: they cannot hold qOS's rollback, staged
update, and repair boot images safely. If anything changed since discovery,
install stops. Automatic filesystem shrinking and ambiguous free extents are rejected.

Encryption remains available in alongside mode. qOS uses LUKS2 for its root;
Windows/macOS partitions retain their own protection. A recovery key is always
created and verified. TPM2, TPM2+PIN, and FIDO2 are convenience factors layered
on top, never replacements for recovery. Save the displayed recovery key away
from the computer before rebooting.

## Account setup and first boot

The account password and optional disk PIN stay outside collaborative document
state, travel to the backend on standard input rather than the process command
line, and are cleared from the live UI at installation start. The backend writes
only a salted password hash into a private, atomic first-boot seed. It also
restores the normal greeter, removes installer-only sudo/SSH state, and safely
copies eligible NetworkManager profiles.

When installation completes, remove the USB drive and reboot. The signed UKI
shows the qOS splash directly—routine boot chatter stays on the serial journal,
and fatal errors still replace the splash. Select qOS from
the firmware menu if it is not already first. Unlock the disk, sign in with the
new account, and finish the desktop's onboarding. Confirm keyboard, time zone,
network, display scaling, accessibility, and recovery-key storage. The first
login then offers a short, skippable tour of search and overview, the
Workspace → Strip → Window group model, and deliberate collaboration. Signed qOS
updates are checked after first boot when online; the installer never silently
swaps the offline image for a network download.

For dual boot, reboot once into the other operating system too. A BitLocker
recovery prompt is possible after the firmware change. Confirm both operating
systems boot before deleting backups or recovery media.
