# Getting a qOS machine back

Everything here is about one question: the machine came up wrong, now what.

There are three answers, and they are ordered by how broken the machine is.

| The machine | Use | Needs |
|---|---|---|
| boots, session works | `qos rollback`, or Settings → Updates → Roll back | to be logged in |
| boots, session does not work | it rolls back **by itself** after three tries | nothing |
| boots, nothing works | Repair Mode, from the boot menu | the disk passphrase |

## What a rollback actually promises

This is the part that is usually left unsaid, so it is said first.

**`/usr` reverts.** The system image, the compositor, the shipped apps, the
kernel, and the runtime bundle at `/usr/lib/qos-runtime/<version>/` all go back
to what the previous deployment contained. That is the whole of what an image is.

**`/var` does not.** Everything you would call *your* machine lives there and is
shared by every deployment on the disk:

- `~/.config/qos/state.json` — your workspaces, strips, groups and tabs
- every app's `state.automerge` under `~/.local/share/<package>/`
- the device identity keys under `/var/lib/qos`
- the content store the collaborative filesystem hydrates from

A rollback does not restore any of it, and anything a newer version wrote into it
stays written. This cuts both ways and both ways are worth knowing: your work is
never lost by rolling back, and a bug that *corrupted* your data is not fixed by
rolling back either.

**Packages you installed yourself do not revert.** A `.qpkg` lives under `/var`,
so it survives, but the runtime bundle it links against went back with `/usr`. An
app built for a newer runtime can stop launching after a rollback. Reinstall it,
or roll forward again.

Every path that offers a rollback prints this before it does anything —
`qos rollback`, the Settings control, and the repair console all call
`qos-deployment caveats`. Finding it out afterwards is the failure this is written
to prevent.

Rollback is image recovery. File recovery is separate: Repair Mode can copy
personal files from `/var/home` to a mounted external disk, and the installer
medium can open an otherwise unbootable qOS volume read-only. Neither workflow
rewrites the original while recovering files.

## From a working session

```bash
qos status                 # what is booted, staged, and rollable-to
qos rollback               # prints the caveats, asks, then reorders the boot
qos rollback --yes         # for a script
sudo reboot
```

Settings → Updates has the same thing with a button. It runs
`qos-deployment-rollback.service`, which polkit authorises for the user sitting
at the machine (`os/etc/polkit-1/rules.d/49-qos-updates.rules`) — narrowly, by
unit name, for a local active session in `wheel`.

Nothing is deleted. The deployment you were on stays on the disk and stays in the
boot menu, so this is reversible from either side.

### When `bootc rollback` is the problem

`bootc rollback` is the command that should do this, and on the composefs backend
qOS uses it is not dependable: [bootc#2208](https://github.com/bootc-dev/bootc/issues/2208)
has a series of updates leaving rollback broken, and broken *through reboots*.

So `qos rollback` does not trust it. It runs `bootc rollback`, then asks the boot
loader what the next boot will actually be, and if that did not change it writes
the boot loader's own default entry instead (`bootctl set-default`). The output
says which of the two happened. The bootloader is the component that decides;
going straight to it is the shorter statement of the same intent.

## Automatic rollback

A deployment that has never booted successfully gets a **try counter** in its boot
loader entry's file name — `bootc_fedora-44-1+3.conf`. systemd-boot decrements it
on every attempt, and when it reaches zero it stops offering that entry first.

The counter is only removed when the boot is declared good.
`qos-boot-health.service` is what declares it: it is ordered before
`boot-complete.target`, which `systemd-bless-boot.service` requires, so an
unblessed boot keeps its spent try. Health means **greetd came up**, i.e. the
machine reached the point where a person can log in.

Three failed boots and the previous deployment becomes the default. Nothing runs
a rollback command — the boot loader simply declines to keep choosing an entry it
has already given three chances, which is the only mechanism that still works when
userspace is too broken to run anything.

The fallback order is:

1. deployments with tries left, newest first
2. **qOS Repair Mode**
3. deployments that have used up their tries

So a machine with an older good deployment rolls back to it, and a machine with
nothing else lands at the repair console instead of a black screen.

Health is deliberately *not* "no unit failed". That test belongs to
`qos-rollback-confirm`, which decides whether to **destroy** the fallback by
raising the TPM rollback floor, and there being strict costs nothing. Here the
same strictness would be a trap: one unrelated broken unit would fail every boot
of every generation, walk the retention window down, and leave a machine with
nothing it is willing to boot. Failed units are reported and do not condemn the
boot.

`qos status` shows the counters:

```
Boot loader entries (what the firmware will actually offer):
  bootc_fedora-44-0.conf                        tries left 2 (of 3)
  bootc_fedora-44-1.conf                        no boot counter (booted now)
  qos-repair.conf                               no boot counter
```

## Repair Mode

A boot menu entry called **qOS Repair Mode**, always present, that brings the
machine up without the desktop: no compositor, no greeter, no collaborative
filesystem, no update timer, no sshd. It gives you a console with

1. roll back to the previous deployment
2. boot a specific deployment once
3. re-fetch and stage the current image
4. make `/var` writable, and say where your data is
5. the **previous** boot's warnings and errors
6. copy personal files to an external disk
7. a root shell

It is the same rootfs the desktop boots, selected by `qos.mode=repair` on a
*sealed* kernel command line — see [security/repair-mode.md](security/repair-mode.md)
for how that is derived and for the authentication story (short version: on an
encrypted machine you have to prove a LUKS passphrase, and the check is real).

What repair mode cannot fix: a rootfs that will not mount. The composefs root is
verity-enforced, so corruption is refused rather than silently used, and repair
mode runs from that same rootfs. That case needs external media, and no
on-disk mode can change it.

## Recover files with the installer medium

Boot the qOS installer and press **Ctrl+Alt+F2**. The recovery console lists
disks before asking for a source. Encrypted qOS volumes are opened read-only;
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
sudo qos-recovery list
sudo qos-recovery open --device /dev/nvme0n1p4
sudo qos-recovery copy --device /dev/nvme0n1p4 \
  --from alex/Documents --destination /run/media/backup
sudo qos-recovery close
```

## Remove an alongside installation

Removal is intentionally narrower than installation. From the installer
medium's recovery console, qOS will remove only an **alongside** root that has a
matching install-time ownership marker. You must type the partition's exact
device, size, and UUID fingerprint. qOS then removes only boot files recorded by
that installation and deletes that one root partition. It never formats the
shared EFI filesystem, edits another partition, or resizes the remaining OS.

The released space is unallocated. Expand the remaining OS with its own disk
manager, which understands that filesystem. If firmware still shows qOS first,
boot the remaining OS once and select it as the default in firmware settings.
Older installations without an ownership marker fail closed: recover the files,
then use a trusted partition editor rather than letting qOS guess.

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

None of this is established by reading it. What can be checked, and where:

```bash
# Host-side, no VM: the invariants the mechanism rests on — that the two
# qos.mode values are the same length, that the repair entry sorts below every
# deployment and carries no counter, that the health gate is wired the way
# systemd expects, that every repair verb is behind the passphrase gate.
./os/tests/repair_mode.sh

# And that the ESP is sized for the retention window plus the repair image.
./os/tests/esp_capacity.sh

# In a booted guest: the facts only a real machine has. Fails rather than skips
# when it cannot observe its subject.
sudo /usr/libexec/qos-recovery-boot-check

# End to end, in a VM: the repair entry, a repair-mode boot driven over the
# serial console, and a deliberately broken generation left behind.
./os/dev/composefs/build.sh 1 && ./os/dev/composefs/install.sh 1
QOS_CFS_SESSION_OK=0 ./os/dev/composefs/build.sh 2
./os/dev/composefs/recovery-series.sh

# The state-version refusal, against the real compositor binary:
cargo build -p shell --bin shell
./scripts/headless-examples/older-binary-meets-newer-state.py
```

## The state a rollback cannot help with

An older binary can meet state a newer one wrote. That is not a hypothetical after
a rollback, it is the expected case. Both of the places qOS keeps state carry a
schema version and **refuse** rather than guess:

- `~/.config/qos/state.json` — the compositor's workspace tree. A file from a
  newer schema is left untouched, the session starts from a default tree, and a
  notification that does not auto-dismiss says so with the path. Silently
  replacing your workspaces with a demo tree is indistinguishable from having lost
  them, which is what used to happen.
- `state.automerge` — every app's document. Same rule: refuse, preserve, say so.

So the honest sequence after rolling back across a schema change is: the older
version comes up empty and tells you your data is still there; roll forward again
to reach it.
