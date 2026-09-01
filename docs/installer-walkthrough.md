# qOS installation: download to first login

This is the user journey and the release acceptance checklist for qOS Imager
and the live installer. A release is not complete unless the walkthrough works
on Linux, Windows, and macOS with a real removable drive, and then in a UEFI VM
for both erase-disk and alongside installation.

## Before downloading

Back up the computer. For a Windows dual boot, save the BitLocker recovery key,
turn off Fast Startup, and use Windows Disk Management to shrink Windows. Leave
the result unallocated. Then use a trusted partition editor that can assign GPT
partition types to create one **unformatted Linux filesystem partition of at
least 32 GiB** in that space. (Windows Disk Management cannot assign that Linux
GPT type.) qOS intentionally does not resize NTFS: Windows understands
BitLocker, hibernation, and its recovery partitions better than an offline
Linux tool can.

Changing UEFI boot files, Secure Boot keys, or boot order can cause BitLocker to
ask for its recovery key even when the Windows partition was not modified.
That prompt is a security feature, not data loss.

## Download and launch qOS Imager

Download the signed qOS Imager for the host operating system from the qOS
release page. Verify the published SHA-256 digest before launching it. Never use
an imager or OS image obtained from a mirror whose digest is not listed by qOS.
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

The current host bundles elevate the imager process for raw-device access; the
application still has its typed target gate, system-disk acknowledgement, and
readback verification. A future split privileged helper would reduce the amount
of elevated UI code and remains preferable before a stable release.

## Boot the installation medium

Reboot and choose the USB drive in the firmware's one-time boot menu. Keep UEFI
mode enabled. The live installer asks for keyboard first, then optional network,
disk method, account, filesystem/encryption, and a final review. No OS download
is required: installation uses the signed image on the medium.

### Erase disk

This removes every partition and operating system on the selected disk. The
review page names the exact disk and repeats the impact. Use this for an empty
computer or when qOS will be its only operating system.

### Install alongside

This preserves the GPT and existing OS partitions. It is offered only when the
selected disk has both:

1. an existing EFI System Partition; and
2. an unmounted, signature-free Linux partition of at least 32 GiB.

The review page names both partitions. The privileged backend independently
checks their device ancestry, GPT types, size, mount state, and signatures just
before formatting. It formats only the named qOS root and mounts the shared EFI
partition without formatting it. If anything changed since discovery, install
stops. Automatic NTFS shrinking and ambiguous “free” partitions are rejected.

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

When installation completes, remove the USB drive and reboot. Select qOS from
the firmware menu if it is not already first. Unlock the disk, sign in with the
new account, and finish the desktop's onboarding. Confirm keyboard, time zone,
network, display scaling, accessibility, and recovery-key storage. Signed qOS
updates are checked after first boot when online; the installer never silently
swaps the offline image for a network download.

For dual boot, reboot once into the other operating system too. A BitLocker
recovery prompt is possible after the firmware change. Confirm both operating
systems boot before deleting backups or recovery media.
