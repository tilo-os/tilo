# Getting a Tilo machine back

Everything here is about one question: the machine came up wrong, now what.

There are three answers, and they are ordered by how broken the machine is.

| The machine | Use | Needs |
|---|---|---|
| boots, session works | `tilo rollback`, or Settings → Updates → Roll back | to be logged in |
| boots, session does not work | it rolls back **by itself** after three tries | nothing |
| does not boot (hangs at unlock, crashes) | it rolls back **by itself** after three tries; power off a hung try | nothing |
| boots, nothing works | Repair Mode, from the boot menu | the disk passphrase |

## What a rollback changes

**`/usr` reverts.** The system image, the compositor, the shipped apps, the
kernel, and the runtime bundle at `/usr/lib/tilo-runtime/<version>/` all go back
to what the previous deployment contained. That is the whole of what an image is.

**`/var` does not.** Everything you would call *your* machine lives there and is
shared by every deployment on the disk:

- `~/.config/tilo/state.json` — your workspaces, strips, groups and tabs
- every app's `state.automerge` under `~/.local/share/<package>/`
- the device identity keys under `/var/lib/tilo`
- the content store the collaborative filesystem hydrates from

A rollback does not restore any of it, and anything a newer version wrote into it
stays written. This cuts both ways and both ways are worth knowing: your work is
never lost by rolling back, and a bug that *corrupted* your data is not fixed by
rolling back either.

**Packages you installed yourself do not revert.** A `.tapp` lives under `/var`,
so it survives, but the runtime bundle it links against went back with `/usr`.
A copy whose runtime build is missing never runs over the image's own copy of
the app, so an app the image ships runs as that image ships it. Any other app
counts as out of date until its runtime is back, so the next `app update`
(hourly, `tilo-app-update.timer`) fetches that runtime. Rolling forward
needs nothing: the newer copy was never removed and runs again.

Every path that offers a rollback prints this before it does anything —
`tilo rollback`, the Settings control, and the repair console all call
`tilo-deployment caveats`. Finding it out afterwards is the failure this is written
to prevent.

Rollback is image recovery. File recovery is separate: Repair Mode can copy
personal files from `/var/home` to a mounted external disk, and the installer
medium can open an otherwise unbootable Tilo volume read-only. Neither workflow
rewrites the original while recovering files.

## From a working session

```bash
tilo status                 # what is booted, staged, and rollable-to (version, build date)
tilo rollback               # prints the caveats, asks, then reorders the boot
tilo rollback --yes         # for a script
sudo reboot
```

Settings → Updates has the same thing with a button. It runs
`tilo-deployment-rollback.service`, which polkit authorises for the user sitting
at the machine (`os/etc/polkit-1/rules.d/49-tilo-updates.rules`) — narrowly, by
unit name, for a local active session in `wheel`.

Nothing is deleted. The deployment you were on stays on the disk and stays in the
boot menu, so this is reversible from either side.

### When `bootc rollback` is the problem

`bootc rollback` is the command that should do this, and on the composefs backend
Tilo uses it is not dependable: [bootc#2208](https://github.com/bootc-dev/bootc/issues/2208)
has a series of updates leaving rollback broken, and broken *through reboots*.

So `tilo rollback` does not trust it. It runs `bootc rollback`, then asks the boot
loader what the next boot will be, and if that did not change it writes
the boot loader's own default entry instead (`bootctl set-default`). The output
says which of the two happened. The bootloader is the component that decides;
going straight to it is the shorter statement of the same intent.

## Automatic rollback

A deployment that has never booted successfully gets a **try counter** in its boot
loader entry's file name — `bootc_fedora-44-1+3.conf`. systemd-boot decrements it
on every attempt, and when it reaches zero it stops offering that entry first.

`tilo-boot-entries.service` adds the counter to every bootc entry except the one
running now, which it recognises by the `composefs=` identity of the running root.
On the sealed image every entry has to name a UKI under `EFI/Linux/bootc/`, and the
identity is read from that UKI's sealed command line; any other entry fails the
pass. Only a dev image (`/usr/lib/tilo/image-flavour` reads `dev`) also accepts
bootc's split kernel/initrd entries: `linux` and `initrd` from one
`EFI/Linux/bootc_composefs-<id>/` directory and exactly one `composefs=` on the
`options` line. Such a deployment counts and rolls back the same way; it gets no
repair entry, because that is derived from a UKI.

sd-boot spends the try when it picks the entry, before the kernel starts. A
boot that never reaches userspace (a disk unlock prompt nobody can answer
because the keyboard is dead, a crash in the initrd) has spent its try all
the same: switch the machine off and on, and after the third such boot the
previous deployment starts. The counter has to be on the entry **before** the
first reboot into it, so every path that stages an image runs the boot-entry
pass right after staging: `tilo update`, `bootc-fetch-apply-updates`, and
`tilo-vm vm upgrade` (and `vm rollback-image`). An image older than
`tilo-esp-generator` cannot see its ESP from that pass; `tilo-vm` then counts
the staged entry itself, on the ESP the boot loader names, so moving a
machine off such an image is protected too.

The counter is only removed when the boot is declared good.
`tilo-boot-health.service` is what declares it: it is ordered before
`boot-complete.target`, which `systemd-bless-boot.service` requires, so an
unblessed boot keeps its spent try. Good means **someone can sign in**: the
sign-in screen is drawn and stays, and a keyboard or touch screen reaches it.
It does not wait for a person, so an unattended machine is judged by itself,
and a working update is not rolled back because the power went out at its
sign-in screen:

1. The greeter came up and stayed up: greetd is active, its greeter has
   presented a frame (or a person is already signed in through greetd), and
   for 20 seconds after that greetd is not restarted and the greeter keeps
   presenting. The check waits up to three minutes for the first frame,
   because a machine that renders in software needs tens of seconds for it,
   and fails the boot when the greeter never holds.
2. Input reaches it: the kiosk compositor gave the greeter's window a
   keyboard or a touch screen. It advertises on its seat the keyboard and
   touch devices it has opened, and the greeter records the first one in
   `/run/tilo-greeter/input` (`crates/session/greeter/src/seat_input.rs`); a
   mouse alone records nothing, so a machine with only a mouse falls back. A
   person signed in through greetd settles this as well, whichever comes
   first. A boot where neither happens within the
   deadline (at least 30 seconds after the greeter held) fails.

A disk with TPM unlock reaches its greeter without anyone; one that asks for
a passphrase gets there once it was typed. A boot that never gets past the
unlock prompt never reaches the check at all and keeps its spent try: that is
the update whose keyboard does not work at unlock. `tilo-rollback-confirm`
(which destroys the fallback) waits for the blessing rather than acting on an
unproven boot.

`greetd is active` alone is not enough. greetd restarts when its greeter
exits, so it is active between restarts, and a greeter that cannot draw (an
update that breaks the GPU driver, cage or the greeter) crash-loops behind an
active greetd. Judged by that alone, such a boot was blessed on its first try
and the machine never rolled back. The greeter's evidence is its own startup
record, `/run/tilo-greeter/startup-metrics.json`, which its first presented
frame completes; `journalctl -b -u tilo-boot-health` says which of the checks
a failed boot missed.

One machine keeps the plain verdict: a virtual machine made without any display
device (the headless dev VMs reached over SSH), where no greeter can present on
any generation. On real hardware no display device at all is what a broken GPU
driver leaves behind, so there the greeter is still required.

Blessing renames the booted entry (`name+2-1.conf` to `name.conf`) on the ESP,
which stays mounted read-only. `systemd-bless-boot.service` therefore remounts
it writable in its own private mount namespace, as `tilo-esp-write@` does;
the global mount never becomes writable. Without that, every rename failed with
`EROFS`, and a deployment that booted fine spent a try on each boot until the
boot loader abandoned it.

Where the ESP is mounted depends on the layout. On a sealed disk `/etc/fstab`
mounts it at `/boot`, on a dev disk the entry's `systemd.mount-extra` does. An
installed system has an encrypted root and a separate ext4 `/boot` (`TILO_BOOT`)
that only its signed target UKI mounts; it has no fstab, and an update's own UKI
mounts no `/boot` at all. Nothing mounted its ESP: `systemd-gpt-auto-generator`,
which would take an unclaimed `/boot`, mounts nothing on this composefs layout,
and the image has no `/efi`. The boot-entry maintenance found no boot loader and
gave no update a try counter ("nothing to maintain"), so a broken update was
never rolled back, and `systemd-bless-boot` stopped with "Couldn't find $BOOT
partition", so a good boot was never blessed. `tilo-esp-generator` now mounts the
ESP the boot loader names (`LoaderDevicePartUUID`) where every one of those tools
looks: at `/boot/efi` when `/boot` comes from anything but that ESP, and at
`/boot` itself when nothing claims `/boot`. Its FAT superblock stays writable and
the mount everyone sees is a read-only bind (`tilo-esp-mount`): bootc mounts the
same ESP read-write to stage an update, and a read-only superblock would refuse
that. The live installer is left alone: it refuses to install to a mounted ESP.

Three failed boots and the previous deployment starts. Nothing has to run for
that — the boot loader simply declines to keep choosing an entry it has
already given three chances, which is the only mechanism that still works when
userspace is too broken to run anything.

What the boot loader cannot do is change bootc's mind: bootc still queues the
failed deployment as the default (`rollbackQueued` in `bootc status`), and the
first thing that rewrites the entries would hand it fresh tries.
`tilo-boot-fallback.service` settles that on the deployment that came back,
once that boot is confirmed: when bootc queues a deployment whose entry was
tried and never confirmed (`+LEFT-DONE`, at least one try spent), it runs
`bootc rollback` so the running deployment is the default again, and writes a
report to `/var/lib/tilo/boot-fallback/<when>-<composefs>/`:

- `summary.txt`: the image and version that did not start, how many tries it
  spent, and what each try left behind;
- `boot-N.log`: for each try that got far enough to keep a journal, its
  health verdict and its warnings;
- the tries that left no log at all (stopped at the unlock prompt, in the
  initrd, or in a crash) counted as such;
- `pstore/`: crash records the firmware kept, as systemd-pstore archived them.

The same happens when a person picks the previous entry from the boot menu
after the new one failed: the failed one stays on the disk, but it no longer
boots next. An entry that was never tried, or one that booted well before, was
chosen deliberately and is left alone. At the next sign-in the person gets a
notification that stays until dismissed (`tilo-boot-fallback-notice`), and
`tilo status` shows the last fallback with the path of its report.

The fallback order is:

1. deployments with tries left, newest first
2. **Tilo Repair Mode**
3. deployments that have used up their tries

So a machine with an older good deployment rolls back to it, and a machine with
nothing else lands at the repair console instead of a black screen.

Health is deliberately *not* "no unit failed". That test belongs to
`tilo-rollback-confirm`, which decides whether to **destroy** the fallback by
raising the TPM rollback floor, and there being strict costs nothing. Here the
same strictness would be a trap: one unrelated broken unit would fail every boot
of every generation, walk the retention window down, and leave a machine with
nothing it is willing to boot. Failed units are reported and do not condemn the
boot.

`tilo status` shows the counters:

```
Boot loader entries (what the firmware will actually offer):
  bootc_fedora-44-0.conf                        tries left 2 (of 3)
  bootc_fedora-44-1.conf                        no boot counter (booted now)
  tilo-repair.conf                               no boot counter
```

## Repair Mode

A boot menu entry called **Tilo Repair Mode**, always present, that brings the
machine up without the desktop: no compositor, no greeter, no collaborative
filesystem, no update timer, no sshd. It gives you a console with

1. roll back to the previous deployment
2. boot a specific deployment once
3. re-fetch and stage the current image
4. make `/var` writable, and say where your data is
5. the **previous** boot's warnings and errors
6. copy personal files to an external disk
7. a root shell

It is the same rootfs the desktop boots, selected by `tilo.mode=repair` on a
*sealed* kernel command line — see [security/repair-mode.md](security/repair-mode.md)
for how that is derived and for the authentication story (short version: on an
encrypted machine the recovery gate verifies a passphrase against the LUKS
header, even when the disk is already unlocked).

What repair mode cannot fix: a rootfs that will not mount. The composefs root is
verity-enforced, so corruption is refused rather than silently used, and repair
mode runs from that same rootfs. That case needs external media, and no
on-disk mode can change it.

## Recover files with the installer medium

Boot the Tilo installer and press **Ctrl+Alt+F2**. The recovery console lists
disks before asking for a source. Encrypted Tilo volumes are opened read-only;
the recovery key or disk passphrase goes directly to `cryptsetup` and is not
stored. The source mount also forbids devices, programs, and set-user-ID files.

Choose either one folder below the installed home directory or `.` for all
homes. The guided flow lists destination partitions and mounts the one you
choose without formatting it. It must be on a different physical disk, be
writable, and have enough free space for the selection. Recovery copies links
without following them outside the source and syncs the result before reporting
success. Review the copy before repairing or removing the original disk.
Ctrl+Alt+F1 returns to the graphical installer.

The same boundary is scriptable for technicians:

```bash
sudo tilo-recovery list
sudo tilo-recovery open --device /dev/nvme0n1p4
sudo tilo-recovery copy --device /dev/nvme0n1p4 \
  --from alex/Documents --destination /run/media/backup
sudo tilo-recovery close
```

## Remove an alongside installation

Removal is intentionally narrower than installation. From the installer
medium's recovery console, Tilo will remove only an **alongside** root that has a
matching install-time ownership marker. You must type the partition's exact
device, size, and UUID fingerprint. Tilo then removes only boot files recorded by
that installation and deletes that one root partition. It never formats the
shared EFI filesystem, edits another partition, or resizes the remaining OS.

The released space is unallocated. Expand the remaining OS with its own disk
manager, which understands that filesystem. If firmware still shows Tilo first,
boot the remaining OS once and select it as the default in firmware settings.
Older installations without an ownership marker fail closed: recover the files,
then use a trusted partition editor rather than letting Tilo guess.

## Reading a machine that will not boot at all

```bash
./os/dev/vm.sh serial                     # the boot's serial console
./os/dev/vm.sh journal 'greetd|shell'     # offline journal, VM stopped
```

On real hardware: boot Repair Mode and use option 5, which runs `journalctl -b -1`
— the boot *before* this one, which is the failed one. If the journal is not
persistent the console says so instead of showing you this boot's log and letting
you read it as the failure.

## Proving it

Check the following invariants and paths:

```bash
# Host-side, no VM: the invariants the mechanism rests on — that the two
# tilo.mode values are the same length, that the repair entry sorts below every
# deployment and carries no counter, that the health gate is wired the way
# systemd expects, that every repair verb is behind the passphrase gate.
./os/tests/repair_mode.sh

# And that the ESP is sized for the retention window plus the repair image.
./os/tests/esp_capacity.sh

# In a booted guest: the facts only a booted machine has. Fails rather than skips
# when it cannot observe its subject.
sudo /usr/libexec/tilo-recovery-boot-check

# End to end, in a VM: the repair entry, a repair-mode boot driven over the
# serial console, and a deliberately broken generation left behind.
./os/dev/composefs/build.sh 1 && ./os/dev/composefs/install.sh 1
TILO_CFS_SESSION_OK=0 ./os/dev/composefs/build.sh 2
./os/dev/composefs/recovery-series.sh

# The same rollback on the installed image itself, not the probe image: stage a
# real update, make only it unable to reach the greeter (a greetd drop-in keyed
# to its composefs identity), and require it to spend exactly its tries before
# the boot loader goes back by itself. RUNNER is anything that runs a command
# line in the guest; RESET power-cycles a guest that stopped answering. CI runs
# it on a published dev disk: Dev / native disk boot diagnostic with
# boot_assessment_image set. FAULT=greeter keeps greetd active and makes its
# greeter exit 12 s after each start instead, the crash loop a check of
# "greetd is active" would bless (boot_assessment_fault=greeter in CI).
RUNNER='./os/dev/vm.sh ssh' SUDO_PASS=tilo \
  ./os/tests/boot_assessment_rollback.sh registry.tilo.so/tilo/tilo-vm@sha256:<other-digest>

# FAULT=unlock leaves the staged deployment's boots at a disk unlock prompt in
# the initrd and power-cycles them there (RESET); after its tries the boot
# loader goes back, and tilo-boot-fallback makes the previous deployment the
# default again with a report of the failed one. t runs it on this tree's own
# dev image, with an update that is that image plus one layer (tools/ci/boot-assessment.sh; fault=greetd or greeter
# picks the other faults):
t dispatch boot-assessment-x86_64 --ref <branch>

# The health verdict itself against a stand-in greetd, no VM: a crash-looping
# greeter fails, one that holds needs the greeter's record of a keyboard or
# touch screen (or a person signed in), boards are judged by their own unit.
./os/tests/boot_health_greeter.sh

# Settling after a fallback against a stand-in bootc and journal, no VM: what is
# made the default, what is left alone, and the report.
./os/tests/boot_fallback.sh

# The state-version refusal, against the compositor binary:
cargo build -p shell --bin shell
./scripts/headless-examples/older-binary-meets-newer-state.py
```

## The state a rollback cannot help with

An older binary can meet state a newer one wrote. This is expected after a
rollback across a schema change. Both places Tilo keeps state carry a
schema version and **refuse** rather than guess:

- `~/.config/tilo/state.json` — the compositor's workspace tree. A file from a
  newer schema is left untouched, the session starts from a default tree, and a
  notification that does not auto-dismiss says so with the path. Silently
  replacing your workspaces with a demo tree is indistinguishable from having lost
  them, which is what used to happen.
- `state.automerge` — every app's document. Same rule: refuse, preserve, say so.

So the honest sequence after rolling back across a schema change is: the older
version comes up empty and tells you your data is still there; roll forward again
to reach it.
