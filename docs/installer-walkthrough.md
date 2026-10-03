# Tilo installation: download to first login

Guided migration is still under development. USB-free Full Tilo installation
and Connected folders are unavailable; see [migration feature status](migration-status.md)
for implemented paths, platform limits, and remaining validation.

This is the user journey and the release acceptance checklist for Tilo Imager
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
at least **34 GiB unallocated**. Tilo creates a dedicated 2 GiB EFI System
Partition and uses the rest for its encrypted root. It intentionally does not
resize NTFS: Windows understands BitLocker, hibernation, and its recovery
partitions better than an offline Linux tool can.

Before the shrink window, open one Administrator PowerShell in the repository,
keep it open for the coordinated Windows preparation, and run the read-only
preflight. Retain its JSON beside the installation record:

```powershell
PowerShell -ExecutionPolicy Bypass -File .\scripts\windows-dual-boot-preflight.ps1 `
  -OutputPath .\windows-dual-boot-preflight.json
```

The report records the Windows-supported shrink limit, the latest NTFS shrink
analysis event, paging and hibernation state, shadow-copy storage, BitLocker,
Secure Boot, and Windows Recovery. A large amount of free space does not prove
that the partition can shrink by the same amount: an allocated extent near the
end of NTFS can set a much smaller supported limit.

Keep normal automatic paging active during image preparation. If an immovable
page file limits the final shrink, close applications and virtual machines,
checkpoint its exact configuration, disable paging only for the short shrink
window, and reboot before measuring again. Restore the checkpoint and reboot
immediately after the shrink. Running build workloads without active paging is
not a safe steady state.

The helper makes that exceptional preparation explicit and reversible. Run it
only after image assembly and VM work have stopped:

```powershell
PowerShell -ExecutionPolicy Bypass -File .\scripts\windows-dual-boot-preflight.ps1 `
  -PrepareShrink -CheckpointPath C:\tilo-handoff\windows-shrink-checkpoint.json
# Reboot, then run the default read-only report again.
```

After the supported Windows shrink, restore the recorded paging configuration
and reboot:

```powershell
PowerShell -ExecutionPolicy Bypass -File .\scripts\windows-dual-boot-preflight.ps1 `
  -RestoreWindowsState `
  -RestoreCheckpointPath C:\tilo-handoff\windows-shrink-checkpoint.json
```

Disabling hibernation with `powercfg /hibernate off` also disables Fast Startup
and removes `hiberfil.sys`; it is reversible with `powercfg /hibernate on`.
Do this before the final supported shrink measurement, then keep Fast Startup
off while dual booting. Use Windows' `defrag C: /X` only when the shrink event
shows that free-space consolidation is useful. Do not repeatedly optimize the
volume when the limiting extent has not changed.

Do not delete the NTFS change journal, restore points, or shadow copies as a
generic preparation step. Those operations discard history or recovery state.
If the shrink event names one of them, review the affected consumers and take
that action explicitly in the final shrink window. Never use an offline Linux
tool to shrink a BitLocker-protected Windows volume.

Changing UEFI boot files, Secure Boot keys, or boot order can cause BitLocker to
ask for its recovery key even when the Windows partition was not modified.
That prompt is a security feature, not data loss.

## Download and launch Tilo Imager

Download the signed Tilo Imager from the landing page or Tilo release page. The
release matrix is Linux x86-64/ARM64 (Flatpak, AppImage, portable tar,
Homebrew), Windows x86-64 (setup EXE, standalone EXE, WinGet), and macOS
x86-64/Apple silicon (DMG, app zip, Homebrew cask). Verify the published
SHA-256 digest before launching it. Never use
an imager or OS image obtained from a mirror whose digest is not listed by Tilo.
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
- Windows: launch Tilo Imager and accept the Administrator prompt. The app uses
  `Get-Disk`, takes only the chosen physical disk offline during the write, and
  brings it online afterwards.
- macOS: launch the notarized app and authorize its administrator prompt. The app lists only
  external physical media, unmounts the selected disk with `diskutil`, writes
  through `/dev/rdiskN`, verifies it, and ejects it.

The application starts with no fabricated releases or drives. Choose a local
`.iso`, `.img`, or `.raw` file using the operating system's own file dialog.
Local files are always labelled **Local · unverified** and written as-is; a
Tilo-looking filename or ISO volume label never grants official status or setup
profile access. Only an image selected from the authenticated bundled release
catalog is labelled **Official · verified** and can be pre-configured.
Insert a removable drive, refresh, select it, and inspect its model, capacity,
and current partitions. Internal and running-system disks are hidden behind
additional acknowledgement. The confirmation page requires the exact device
name. Immediately before opening the device, Tilo probes it again and refuses if
its stable hardware/media ID, capacity, model, removability, or system-disk
status changed. Writing is followed by a byte-for-byte readback; “Complete”
means that verification succeeded, not merely that the write call returned.

Everything currently on the selected installation medium is erased.

For a verified official Tilo image, Customize collects the non-destructive
install profile before the medium is written: account name, host name, locale,
keyboard, time zone, and an explicit Btrfs (recommended), Ext4, or XFS
filesystem choice. The imager writes a checksummed, secret-free profile into a
fixed
reserved block on the medium and reads it back during verification. Before the
live UI starts, a privileged one-shot service extracts and verifies that block;
the installer validates every field independently, retains every sound value,
and starts at the first missing or local decision. A missing required or invalid
value opens only the screen that can repair it; an omitted optional full name
remains a valid choice. Disk selection, erase/alongside
choice, a fresh account/startup secret and recovery-key acknowledgement remain
on the target computer. Network and optional hardware-key setup remain
available from the normal step trail. The Review screen shows the resulting
choices before any destructive operation.

Language and locale choices come from the same catalog compiled into both
applications and installed on the live system. The target-side repair is a
bounded chooser rather than free text, and the privileged installer checks the
immutable catalog again before writing `locale.conf`; a plausible but
unsupported locale cannot silently fall back to English after first boot.

The current host bundles keep the Imager UI unprivileged. Only after the target
and destructive warning are confirmed does the operating system authenticate a
short-lived writer mode. Its request contains no profile secret, is bound to a
digest of the reviewed operation, and revalidates and locks the selected device
before writing and reading it back. The Flatpak instead asks the host's
root-owned UDisks2 service for one polkit-authorized, exclusive descriptor to
the reviewed removable drive. It has no `/dev`, host-filesystem, or arbitrary
host-command permission; writing and verification use the same engine as the
native packages.

## Boot the installation medium

Reboot and choose the USB drive in the firmware's one-time boot menu. Keep UEFI
mode enabled. The live installer asks for keyboard first, then optional network,
disk method, account, filesystem/encryption, and a final review. No OS download
is required: installation uses the signed image on the medium.

On a laptop, connect its charger before beginning. If Tilo can definitively see
that the machine is discharging below 25%, Review explains the problem and the
destructive install does not start. Plugging in is enough: press **Begin
Install** again and the installer rechecks power immediately. Desktops, virtual
machines, charging systems, and hardware without a trustworthy battery reading
are not blocked.

### Erase disk

This removes every partition and operating system on the selected disk. The
review page names the exact disk and repeats the impact. Use this for an empty
computer or when Tilo will be its only operating system. Installation remains
disabled until you type the displayed device name exactly; changing the disk or
switching install methods clears that confirmation.

### Install alongside

This preserves the GPT and existing OS partitions. The guided path is offered
when the disk has an aligned, unallocated GPT extent of at least 34 GiB. When
several extents are eligible, choose one explicitly; changing that choice clears
the destructive confirmation. The review names the exact extent and the new
2 GiB Tilo ESP plus >=32 GiB root. The privileged backend locks the disk,
revalidates its stable hardware identity, GPT UUID, logical sector size, and the
PARTUUID/start/size of every existing partition, then appends only those two new
partitions. It never deletes, moves, resizes, or formats an existing partition.

If power is lost after the new GPT entries become durable, a fresh installer
does not automatically format or reinitialize them. Partition names, labels,
UUIDs, checksums, and other records on the destination are all writable by an
attacker, so none can authenticate that Tilo created the partitions or that the
owner approved erasing them. The failure screen clears the selected target and
returns to storage review. Use the recovery console to inspect or remove the
partial pair before starting again; Tilo deliberately does not turn a typed
device name into authorization to reuse an otherwise ineligible target.

This means an interrupted encrypted alongside install may require explicit
manual cleanup. Automatic restart recovery needs a transaction proof held
outside the writable destination and a reproducible recovery factor established
before the first partitioning write. Until that architecture exists, Tilo fails
closed instead of presenting an unsafe or misleading Resume action.

The expert prepared-partition path remains available when the disk has both:

1. an unmounted FAT32 EFI System Partition of at least 2 GiB, with at least
   1 GiB currently available for Tilo's signed boot images; and
2. an unmounted, signature-free Linux partition of at least 32 GiB.

For the prepared path, the review page names both partitions. The privileged backend independently
checks their device ancestry, GPT types, size, mount state, signatures, FAT
filesystem health, and EFI free space just before formatting. It formats only the named Tilo root and
mounts the EFI partition without formatting it. Small Windows/Linux EFI
partitions are deliberately ignored: they cannot hold Tilo's rollback, staged
update, and repair boot images safely. If anything changed since discovery,
install stops. Automatic filesystem shrinking is rejected; free-space choices
are never guessed when more than one extent is safe.

Encryption remains available in alongside mode. Tilo uses LUKS2 for its root;
Windows/macOS partitions retain their own protection. A recovery key is always
created and verified. On a Secure Boot + TPM2 machine whose exact target UKI and
PCR11 policy carry the release authority's signatures, setup accepts a short
numeric PIN and binds the key to that release measurement plus the machine's
PCR7 platform state. If any prerequisite is absent or changes during setup, it
requires a strong ordinary boot passphrase instead; it never creates a PCR7-only
or weak passphrase slot. FIDO2 can be enrolled as an additional convenience
factor, never a replacement for recovery. Save the displayed recovery key away
from the computer before rebooting.

### What the automated dual-boot gate proves

The native release VM runs both prepared and guided alongside installs against
a GPT containing an NTFS filesystem, a Linux filesystem, and a foreign ESP. It
checks their partition identities and bytes before the first target boot and
again after desktop acceptance. In prepared mode, the foreign loader paths hold
architecture-matched controlled EFI applications rather than text sentinels.
The gate opens the installed systemd-boot menu with keyboard input, selects the
displayed numeric entries, and requires distinct Windows-path and Linux-path
markers on the firmware framebuffer. Entry-file presence alone cannot pass.

That controlled execution proves that Tilo preserved executable EFI bytes and
that its same-ESP shortcuts are selectable. It is not evidence that Windows or
a complete foreign Linux distribution booted, and the evidence records both of
those claims as false. The controlled applications are unsigned, so their menu
execution is not treated as Secure Boot acceptance. Guided mode deliberately
uses a separate Tilo ESP; the gate proves the foreign ESP is byte-identical and
that no invalid cross-ESP shortcut was created, but does not invent a firmware
entry for the synthetic fixture. A release or hardware acceptance still needs
an installed, licensed Windows system and a real Linux installation, their
native signed loaders, the machine's real firmware picker, and—where enabled—
BitLocker/recovery and Secure Boot observations.

### Staged installer on the same disk (USB-free)

This path is not yet released (see the status note at the top). When Tilo
Imager stages the live installer onto the computer's own disk, the firmware
starts it once through `BootNext`. Before the greeter starts,
`tilo-staging-handoff detect` reads the firmware's `LoaderDevicePartUUID`,
mounts that ESP read-only, and publishes `/EFI/tilo-staging/handoff.json` to
the installer only if the handoff names that same ESP. The live root grows to
its partition first, so the download is only the used size of the installer.

The installer then treats the disk it runs from as a destination, but only for
guided alongside in the space the Imager left unallocated. Erase disk and the
prepared-partition path stay unavailable there, and the privileged backend
refuses them even if a caller asks. The UI shows the one plan, the Imager's
setup choices, and a review line stating that the disk also holds the running
installer and that Windows or Linux stays untouched. **Return to Windows** (or
Linux) restarts into the retained system at any point before installation
starts or after it fails; Imager then offers to try again or remove the
installer.

`tilo-install --internal-handoff` re-derives every fact itself instead of
trusting the UI copy: the live ESP and root are exactly the staged partitions
(by PARTUUID, type and extent), the disk GUID and size match, only the two
staged partitions are mounted, and the guided extent is exactly the staged
target extent, still unallocated. The new Tilo ESP starts at the beginning of
that extent and the root ends exactly where the staged installer begins.

After a successful installation, and only then, the backend restores
`BootOrder` as for any alongside install, deletes the staged `Boot####` entry
only while its device path still names the staging ESP, and sets `BootNext` to
the installed Tilo so the next restart starts it once while Windows or Linux
stays the default. It also records the staging identities in the target's
`/var/lib/tilo/staging-reclaim.json`. After the first user session,
`tilo-staging-reclaim` verifies every recorded identity, deletes the two
staging partitions, and grows the adjacent Tilo root partition, its LUKS
mapping, and its filesystem into the freed space. Any mismatch leaves every
partition untouched and logs why; an interrupted reclaim resumes from the
disk's actual state on the next run.

The VM round trip `os/dev/test-internal-install-cycle.sh` (CI: the opt-in
`internal-install-cycle` job of the release lane) exercises this path end to
end. It uses a real Fedora Cloud Base guest pinned by SHA-256, with at least
120 GiB unallocated after its root:
1. The real Imager stages the bundle from Fedora.
2. The firmware's BootNext starts the staged installer, which is driven by
   keyboard, pointer and OCR, including the Review lines above.
3. The installed Tilo boots once, completes its first sign-in and reclaims the
   staging space.
4. A cold boot without BootNext must start Fedora again, with its BootOrder,
   ESP loaders and partitions unchanged.

Separate scenarios cover an interrupted helper resuming, `remove`, and a
foreign BootNext refused with `boot-next-busy`. The helper accepts only
releases in the Imager's compiled-in catalog, so the harness builds a test
Imager for the exported bundle with `os/dev/build-internal-cycle-imager.sh`,
which uses the production catalog merger. Every step leaves JSON evidence.

## Account setup and first boot

The account password and mandatory disk passphrase stay outside collaborative document
state, travel to the backend on standard input rather than the process command
line, and are cleared from the live UI at installation start. The backend writes
only a salted password hash into a private, atomic first-boot seed. It also
restores the normal greeter, removes installer-only sudo/SSH state, and safely
copies eligible NetworkManager profiles.

When installation completes, remove the USB drive and reboot. The signed UKI
shows the Tilo splash directly—routine boot chatter stays on the serial journal,
and fatal errors still replace the splash. Select Tilo from
the firmware menu if it is not already first. Unlock the disk, sign in with the
new account, and finish the desktop's onboarding. Choices already completed in
the Imager or installer are applied without asking again. First login creates
or imports the local collaboration identity, offers an encrypted portable
identity backup, and then presents a short, skippable tour of search and
overview, the Workspace → Strip → Window group model, and deliberate
collaboration. Signed Tilo updates are checked after first boot when online; the
installer never silently swaps the offline image for a network download.

For dual boot, reboot once into the other operating system too. A BitLocker
recovery prompt is possible after the firmware change. Confirm both operating
systems boot before deleting backups or recovery media.

For a Windows-direct install that used a one-time `BootNext`, Tilo can be
added to the persistent firmware picker after its first healthy boot while
Windows remains the default. See [Windows-first firmware boot order](windows-first-tilo-boot-order.md).
