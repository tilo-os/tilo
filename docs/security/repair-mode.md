# Repair Mode

A boot menu entry that brings the machine up without the desktop, so a machine
whose session is broken can still be told what to do. What it offers, and how to
use it, is in [../recovery.md](../recovery.md). This document is about how it is
built and what it is allowed to do.

## Design and rejected alternatives

Repair mode is **the same rootfs, booted to a different systemd target, selected
by a second UKI whose sealed command line differs by one token.**

That token is `tilo.mode=`. `04-mode.toml` puts `tilo.mode=normal` on every image's
kernel command line; the repair entry carries `tilo.mode=repair`;
`tilo-boot-mode-generator` reads `/proc/cmdline` and, for `repair`, points
`default.target` at `tilo-repair.target` and masks the desktop's units.

Three other shapes were considered and each fails on something specific to this
image's constraints:

**A separate deployment.** A second rootfs on disk, updated separately. It costs
the disk and the update path of a whole second OS, and `bootc install reset` is
[not implemented for composefs](https://github.com/bootc-dev/bootc/issues/1886)
so there is no supported way to manage one. Its intended benefit — surviving a
corrupt primary rootfs — is not provided when both deployments share the same
disk, because both fail in that case.

**A UKI addon.** systemd-stub does load separately signed PE addons that append
kernel arguments, and `tilo.mode=repair` is exactly the kind of argument they carry.
But addons are picked up for *every* boot of the UKI they sit beside; there is no
way to select one from the boot menu. An addon that makes repair mode selectable
would have to make it unavoidable.

**An initramfs target.** Break in the initramfs and give a shell there. It is
generation-independent, which is useful — but every verb repair mode requires
(`bootc`, `tilo-rollback`, `journalctl -b -1`, a package
manager's worth of diagnosis) lives in the rootfs, and putting them in the
initramfs means a second copy of them inside every UKI on the ESP. The initramfs
is already ~120 MB of the 134 MB UKI.

### Why the repair UKI is derived on the machine and not built in the image

The design depends on this constraint.

A sealed UKI's command line carries `composefs=<digest>`, and that digest is the
digest of the rootfs the UKI is sealing. A second UKI *shipped inside* that rootfs
would change the digest it has to name — circular by construction. `/boot` is the
one tree excluded from the digest, so a second UKI could live there, but then
`bootc install` has two `*.efi` under `/boot/EFI/Linux` and has to be trusted to
pick the right one, and the two-pass digest reseal `bootc#2334` forces would have
to rewrite both.

Deriving instead sidesteps all of it. `/usr/libexec/tilo-deployment repair-entry`
takes whichever UKI `bootc install` wrote, copies it, and rewrites
`tilo.mode=normal` to `tilo.mode=repair` **in place**. Both values are 15 bytes, so
the `.cmdline` section keeps its size, every following PE section keeps its file
offset, and the image still loads — the same length-preserving edit
`tilo-uki-cmdline` (`crates/system/esp-write/src/uki.rs`) already performs to close the composefs seal.
`tilo-uki-cmdline` refuses a length-changing replacement, so a future rename of
either value fails loudly here instead of producing a UKI that will not boot.

The result is always sealed against the deployment stored on the disk,
including after a reseal, and costs one extra UKI on the ESP rather than one per
generation.

### Why it is a Type #2 entry

The Type #1 loader entry is a small text file in
`/boot/loader/entries` carrying a title, a sort key and a path. Testing on a
booted machine showed that it does not work:

- With the image at `EFI/tilo/repair.efi`, `bootc status` fails outright with
  `Getting composefs deployment status: Listing bootloader entries: efi does not
  start with custom prefix`, and everything that reads it — `bootc upgrade`, the
  Settings pane, `tilo status` — goes with it.
- Moving the image under bootc's own prefix does not help. It then fails with
  `Could not determine boot type`, and bootc rewrites the ESP around the entry it
  cannot understand, renaming the deployment's own entry in the process.

bootc's composefs backend parses **every** file in that directory as one of its
own. So the repair image goes where there is no file to parse: a bare `.efi` at
the top level of `EFI/Linux`, which systemd-boot discovers by itself as a Type #2
entry. bootc's own UKIs live one directory deeper, in `EFI/Linux/bootc/`, and
sd-boot does not scan subdirectories — so the two do not see each other in either
direction. Verified on a booted machine: `bootc status` is fine with the repair
image present, and `bootctl list` shows it.

The cost is that a Type #2 entry has no text file to carry its title or its
ordering; both come from the image's own `.osrel` section. Left alone, the repair
image appears in the boot menu as *Fedora Linux 44 (Forty Four)* —
indistinguishable from the thing it exists to repair. `tilo-uki-cmdline set-osrel`
writes `PRETTY_NAME=Tilo Repair Mode` and `SORT_KEY=zz-tilo-repair` into it. That
edit cannot be length-preserving, so it uses the other room the PE allows:
`.osrel` is a small text section with raw slack after it (629 bytes of content in
a 1024-byte raw section), the new text is written into that slack, and the
section's `VirtualSize` header field is updated. No file offset moves, so every
following section is untouched — the same property the `.cmdline` edit has,
bought a different way.

`zz-tilo-repair` sorts after everything bootc emits (`bootc-<stateroot>-<n>`), and
the image carries **no boot counter**, which puts it below every healthy
deployment and above every exhausted one. See
[../recovery.md](../recovery.md#automatic-rollback) for the ladder.

A machine installed before this shape existed may still carry the Type #1 entry.
`tilo-deployment repair-entry` deletes it on sight, which is the upgrade path
rather than tidiness: left in place, it breaks `bootc` on every subsequent boot.

## Threat model

### What repair mode is allowed to assume

Repair mode is reachable by anyone who can reach the boot menu — that is, anyone
with physical access to the machine, or with access to its virtual console. It
must therefore grant **no authority that physical access does not already grant**.

### The gate

Every action in the repair console — rollback, boot-once, re-pull, remounting
`/var`, reading the previous boot's log, the root shell, and even reboot — is
behind `authenticate()`, which runs

```
cryptsetup open --test-passphrase --key-file - <the root's LUKS device>
```

Only a passphrase actually enrolled in the LUKS header satisfies it. The recovery
key qualifies; so does the user's own passphrase. Nothing is cached to disk and
the passphrase never reaches `argv`.

The **status report** is available without authenticating. It shows the booted
generation, the composefs digest, whether the disk is encrypted, and the boot menu
— all of which are already visible to someone standing at the boot menu, and all
of which someone diagnosing the machine needs before they can decide what to try.

### Why a gate at all, when the disk is already open

Because an open mapper proves that some unlock factor was supplied earlier; it
does not prove that the same person is still at the keyboard. Tilo ships no
TPM auto-unlock slot: every unlock needs a human factor. A verified TPM2
install unlocks normal boots with a short PIN; other installs use the strong
passphrase; the recovery key and an explicitly enrolled FIDO2 token work on
every install. Without a fresh gate, repair mode would become an unattended
physical-access root shell after any of those factors opened the disk: strictly
worse than the normal session, where privileged actions still require
authentication.

With the gate, repair mode grants exactly what knowing a disk secret already
grants. The gate is not defeated by the disk being open, because it does not ask
the disk whether it is open — it asks the LUKS header whether this is a key.

### Why generation-bound unlock is still stronger

The bounded-rollback design additionally ensures the TPM factor cannot open
repair mode: `tilo-rollback` seals against **PCR 7 and 11** with a signed
prediction, and a repair-mode command line produces a different PCR 11, so the
TPM2+PIN keyslot does not open there and the recovery key or an enrolled FIDO2
token is the way in. The installer enrolls that slot on verified TPM2 installs
from the signed prediction of the target UKI
([boot-chain.md](boot-chain.md), "Target-independent enrollment"), and the
initramfs consumer falls back to the other factors when it cannot unseal.

On a verified TPM2 install, repair mode gets the stronger property because the
mechanism that gives it is the same one that makes the mode selectable at all: the
command line is inside the measurement. On other installs the gate above is what
holds, and it holds on its own.

### What repair mode excludes

| Absent | Reason |
|---|---|
| `sshd` | repair mode is a physical-presence console. A remotely reachable root console on a machine in an unknown state is a larger hole than the one it closes. |
| `greetd`, the compositor | the normal session is the target of repair. |
| the collaborative filesystem | it joins a peer network from a machine whose state is by definition not trusted yet. |
| the update timer | a machine in repair mode must not have its deployments changed under it. |

The generator masks each of these with a `/dev/null` symlink in the early
generator directory. This also prevents another unit from pulling one in
through a socket unit, a D-Bus activation, or a `Wants=` dependency.

### Where the mode comes from, and why it cannot come from anywhere else

`/proc/cmdline`, and nothing else. On a sealed image that string is the UKI's
signed `.cmdline` section, measured into PCR 11 by systemd-stub, so it has the
same trust basis as the running kernel.

A marker file under `/boot` or `/etc` would be writable by anyone who can mount
the ESP — which, on a machine with an encrypted root, is anyone at all, since the
ESP is not encrypted and cannot be. That would turn "I can touch this laptop's
disk" into "I can make its next boot a root shell", and the sealed command line is
the specific thing that stops it.

The same reasoning is why the *derivation* of the repair UKI runs as root on a
booted system and signs its output when the machine has enrolled its own Secure
Boot key. An unsigned repair image on a Secure Boot machine is a menu entry the
firmware refuses, so `tilo-deployment repair-entry` declines to install one at all
rather than leaving a decoy.

## What it does not defend against

- **Secure Boot not enrolled.** Owner enrolment is a deliberate act
  (`tilo-secureboot enroll`), and until it happens anyone can boot their own kernel
  on this machine. There is no TPM slot for that kernel to auto-unlock, but it can
  imitate the same prompt and capture a passphrase the owner enters. Repair
  mode neither causes nor worsens this; verified boot remains required.
- **A disk removed from the machine.** The LUKS passphrase is the boundary there,
  as it always was.
- **Someone who knows the recovery key.** They are the owner as far as the disk is
  concerned. Repair mode gives them a shell; they could equally have unlocked the
  disk anywhere.
- **A rootfs that will not mount.** Repair mode runs from the rootfs. Verity
  refuses a corrupt one rather than using it, which is correct and also means this
  case needs external media.
