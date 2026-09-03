# Getting qOS onto a computer

qOS can be used in three ways. Pick the least invasive route that answers your
question; a virtual machine is the right first stop for most people.

The website's download page is the authority for current public availability.
At the time this guide was added, the targets below pass the release build and
install matrices, but the first public Imager, installer image, and Linux
package registries have not been promoted. Do not substitute unsigned CI
artifacts or guess a release URL.

| Goal | What changes | Supported hosts or systems |
| --- | --- | --- |
| Try qOS in a virtual machine | One new virtual disk | Linux, Windows, or macOS; x86-64 and ARM64 guests |
| Add the qOS desktop session | Packages and a sign-in option | Ubuntu 24.04, Debian 13, Fedora 44, Arch, or NixOS; x86-64 currently (ARM64 packaging is not release-gated yet) |
| Install the complete qOS system | A selected physical disk or prepared partitions | x86-64 and ARM64 computers with UEFI |

The exact release platform contract lives in
[`docs/install-contract.json`](install-contract.json). The website consumes the
same contract during a release, so a new target cannot silently appear in only
one set of instructions.

## Before you start

qOS is alpha software. Keep another working computer or operating system
available, and open several important files directly from your backup before
changing a physical disk. Disconnect backup and unrelated removable drives
before opening the installer.

For a useful hardware trial, check all of these from the live USB before
installing: display resolution and scaling, keyboard layout, pointer or
touchpad, Wi-Fi or Ethernet, speakers and microphone, suspend and resume, and
every external display you need. A virtual machine cannot prove those things.

## Route 1: virtual machine

Use the installer image matching the virtual CPU architecture. Expand the
`.img.zst`, attach the image read-only as the first disk, and add a separate
blank destination disk of at least 40 GiB. Use UEFI firmware and at least 4 GiB
of memory. NAT networking is sufficient; leave shared folders, clipboard,
physical disks, USB passthrough, and drag-and-drop disabled.

Install only to the blank virtual disk. After qOS shuts down, detach the
installer image and boot the destination disk. Deleting that virtual disk
removes the trial without touching the host.

## Route 2: add the desktop to Linux

The desktop-session package keeps the existing distribution, display manager,
kernel, bootloader, home directory, and package manager in charge. It adds a
qOS entry to the sign-in screen. Use the repository and commands in
[`docs/packaging.md`](packaging.md), then save your work, sign out, select qOS
from the session menu, and sign in with the same local account.

The first login creates or connects your qOS collaboration identity and then
offers a skippable three-screen desktop tour. It does not copy another
desktop's extensions or private settings. To switch back, sign out and select
the previous session. Remove `qos-session` with the same package manager only
after signing in to another desktop.

## Route 3: install the complete system

Download qOS Imager on the computer you are using now:

| Host | Architectures | Preferred download | Package manager |
| --- | --- | --- | --- |
| Linux | x86-64, ARM64 | signed AppImage | Homebrew formula after promotion |
| Windows 10 or 11 | x86-64 | signed setup EXE | WinGet after promotion |
| macOS | Intel, Apple silicon | signed and notarized DMG | Homebrew cask after promotion |

The Imager authenticates the selected qOS image, writes an 8 GiB or larger USB
drive, reads it back, and can carry account name, host name, locale, keyboard,
time zone, and filesystem choice into the live installer. It does not inspect
or copy your files, resize an internal disk, or carry a password or recovery
secret.

Boot the USB in UEFI mode and test the hardware. The live installer offers:

- **Erase disk**, which removes every partition from the named destination.
- **Install alongside**, which uses an empty, unmounted Linux root partition of
  at least 32 GiB and a healthy FAT EFI partition on the same disk of at least
  2 GiB with 1 GiB free. The root is formatted; the EFI partition is not.

Prepare alongside partitions before booting the qOS installer. qOS does not
shrink NTFS or another Linux filesystem. On Windows, save the BitLocker
recovery key, disable Fast Startup, shrink Windows with Windows-aware tooling,
and create the required typed partitions with a trusted GPT partition editor.

The final review names the disk and every partition that will change. Create a
fresh startup secret on the target computer, save the generated recovery key
somewhere else, remove the USB after installation, and restart.

## First login

The first-login screen is part of the trusted desktop, not a web page or normal
application. Create a new collaboration identity or connect this device using
an existing recovery copy. If this is your first device, qOS makes clear that
the recovery copy is the only way to recover that identity after losing the
machine. Store it separately before continuing.

The optional tour teaches the real navigation model:

1. **Move** through a horizontal strip of windows.
2. **Arrange** strips inside workspaces.
3. **Share** a workspace deliberately and review its permissions.

You can reopen it later by searching for **Desktop tour**. Start with Personal,
open Notes from search, create one note, move between its window and Files, and
open History once. That short loop exercises search, the strip, app state, and
undo without asking you to configure collaboration first.

## If something fails

Do not repeat a disk operation until you understand whether the destination was
changed. Keep the installer transcript and exact error. The complete recovery,
rollback, alongside-install, and first-boot acceptance steps are in
[`docs/installer-walkthrough.md`](installer-walkthrough.md); developer and
nested-session setup is in [`DEVELOPMENT.md`](../DEVELOPMENT.md).
