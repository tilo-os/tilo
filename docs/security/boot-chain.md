# Secure Boot and disk encryption on qOS

qOS boots UEFI → systemd-boot → a **unified kernel image** → systemd. There is no
shim and no GRUB: the image removes both, and the kernel is built here rather
than taken from Fedora. Every one of those facts changes what Secure Boot can
mean, so this document describes the chain as it is, not as a stock Fedora bootc
image's would be.

**One correction to note if you have read an earlier version of this file.** It
used to describe a bare kernel plus a separate initramfs, and before that a
shim → GRUB chain the image had already deleted. Both were wrong at the time of
writing. The chain is now a UKI on bootc's composefs backend, which changes three
things that matter:

- **The command line is inside the signature.** It used to sit beside the kernel
  as an editable file on the EFI system partition, so any hardening option could
  be stripped by anything that could write there. Sealing it into the UKI is what
  makes `qos.generation=` trustworthy enough to bind an unlock policy to — see
  [rollback.md](rollback.md).
- **The initramfs is inside the signature too.** Previously it was a plain archive
  the loader handed to the kernel *after* verification, and it is where the
  encrypted root gets unlocked — so anyone able to write the boot partition ran
  code before the root opened, with Secure Boot on and every signature valid.
  That gap is closed: secure boot here now buys a verified *boot*, not merely a
  verified kernel.
- **There is no bootupd.** A sealed image must not carry it, and it is not needed:
  signing is `sbsign` over one artefact.

Why composefs rather than ostree at all is recorded in
[composefs-decision.md](composefs-decision.md).

## The chain has no vendor signature, and cannot get one

A stock Fedora bootc image inherits a chain that verifies out of the box on a
factory machine: shim is signed by the Microsoft UEFI CA, GRUB by Fedora, the
kernel by Fedora. qOS ships none of those three. `systemd-boot-unsigned` is, as
the name says, unsigned, and the kernel is a local rebuild
(`os/kernel/Containerfile`) that no distribution key has ever touched.

So on a machine with the factory keys enrolled and Secure Boot on, qOS does not
boot. That is not a packaging oversight to be fixed by re-adding shim — a
qOS-signed chain would only move the problem. A vendor key means images *we*
signed are attestable and an image the owner built is not, which is the property
this system is specifically designed to keep.

The answer is that the machine's own key is the root: the owner clears the
factory keys, enrols a Platform Key generated on that machine, and signs the boot
artefacts with it. A collaborator who chooses to trust that key gets an
attestable machine, whether the image came from CI or from a laptop.

`/usr/libexec/qos-secureboot` implements this. Nothing about it runs during the
image build or automatically on a running machine.

### The order, which is the whole safety story

Enrolling keys before anything signs the kernel produces a machine whose firmware
refuses its own boot artefacts. `qos-secureboot enroll` therefore refuses unless

1. the keys exist,
2. **every** artefact the firmware will check already verifies against `db`,
3. `qos-secureboot-sign.path` and `.service` are enabled, so the next OS update
   cannot stage a kernel that nothing signs, and
4. the firmware is in **setup mode** — which is the only state that accepts these
   writes, and a state the owner has to reach deliberately.

`sign` is safe at any time: a signature on an artefact the firmware is not
checking is inert. That asymmetry is why the signing side ships enabled and the
enrolling side does not.

Three things keep the artefacts signed, in decreasing order of how much they are
relied on:

- **`qos-secureboot-sign.path`** watches `/boot/EFI/Linux`, where a `bootc upgrade`
  writes the incoming generation's UKI, so it gets signed immediately rather than at
  the next boot — which would be the boot the firmware refuses. It also watches
  `/boot/EFI/systemd` and `/boot/EFI/BOOT` for whatever replaces the loaders there,
  a change with no natural second chance. Both mount points are watched
  (`/boot/EFI/…` and `/boot/efi/EFI/…`) because the ESP is at `/boot` on the sealed
  layout and under `/boot/efi` on the ostree one, and `/boot/ostree` still is too,
  for a machine that predates the move.
- **`qos-secureboot-sign-shutdown.service`** signs once more on the way down.
  The path unit depends on inotify firing and on the change happening somewhere
  we thought to watch; this one needs neither. Whatever touched the boot
  artefacts and however it did it, the last thing before the machine goes down
  is a signing pass — while there is still a running system to do it with.
- **`qos-secureboot-sign.service`** at `multi-user.target`, which cannot rescue
  the boot it runs in but does converge a machine that got here some other way.

`enroll` requires all three.

```bash
# On the machine, before touching firmware setup:
sudo qos-secureboot capture-vendor-keys   # save the factory db/KEK while readable
sudo qos-secureboot generate-keys         # PK, KEK, db -> /var/lib/qos/secureboot
sudo qos-secureboot sign                  # loaders + every deployment's kernel
sudo qos-secureboot status                # read this before going further

# Reboot into firmware setup, CLEAR the Secure Boot keys, boot back, then:
sudo qos-secureboot enroll --yes
# Reboot into firmware setup, turn Secure Boot ON.
```

`capture-vendor-keys` matters: `enroll` carries the factory `db`/`KEK` alongside
the owner's, so firmware capsule updates and signed option ROMs keep verifying.
Once cleared, those keys cannot be read back, and proceeding without them takes
`--no-vendor-keys`.

If an enrolled machine does not boot, clearing the Secure Boot keys in firmware
setup returns it to setup mode and it boots exactly as before. That is the escape
hatch, and it is why enrolment is gated on setup mode rather than attempting to
displace a live Platform Key.

### Prerequisite: the signing tools

`qos-secureboot` needs `sbsign`/`sbverify` (`sbsigntools`) and
`cert-to-efi-sig-list`/`efi-updatevar`/`efi-readvar` (`efitools`). Both are now in
the image and in the frozen package snapshot, along with `systemd-ukify` for
building the UKI. Each subcommand still refuses with the exact remedy if a tool is
missing, rather than half-signing a boot chain.

Worth knowing how they were absent for so long: they were *named* in
`os/Containerfile` but not present in the digest-pinned snapshot, and they live in
a deliberately tolerant `dnf install` — so it installed nothing, silently, while
every build stayed green. The snapshot's package set is scraped from those
`dnf install` blocks, so adding a package means re-running the `snapshot-rpms`
workflow and committing the new `RPMS_REF`; naming it alone does nothing. The
tolerance is right (the tool fails closed), but it means a missing package is
invisible unless something checks.

### What owner-enrolled Secure Boot buys

`lockdown=integrity` (`os/usr/lib/bootc/kargs.d/01-hardening.toml`) only engages
when the kernel is told by firmware that it booted verified. Without Secure Boot
that karg is inert, so root can still load unsigned modules, read `/dev/mem` and
kexec.

PCR 7 measures the Secure Boot policy, and the LUKS TPM slot binds to it (below).
With Secure Boot off, that binding records "Secure Boot is off" — which is a
consistent measurement but not a meaningful one, since the state it attests to is
the state in which the boot chain is unverified.

### The gap this used to have, and what closing it changed

**Everything from here to the end of this section describes the *ostree* backend,
which qOS has left.** It is kept because it is the clearest account of why the UKI
matters, and because the reasoning is what justified moving backends at all — but
read it as history, not as a description of the current image. On composefs with a
UKI: the initramfs and the command line are both inside the signature, `PCR 11`
carries `qos.generation=`, and `os/tests/rollback-boot-check.sh` passes on a VM
booted from an image `os/dev/build-image.sh` built.

`os/Containerfile` builds that UKI. It used to install `systemd-ukify` and never
call it, which is why this check failed on a real image for entirely correct
reasons; the stages that close it are described under [Building the
UKI](#building-the-uki) below.

#### What signing covered on the ostree backend

The firmware verifies what it `LoadImage()`s. On this backend that is
systemd-boot, and then the kernel — both PE binaries, both signable, both signed
by `qos-secureboot sign`. What it does **not** cover:

- **The initramfs.** ostree deploys it as a plain cpio blob beside the kernel and
  the loader hands it to the kernel after the verification step, so it is not
  part of any signature check. That matters more here than it would elsewhere,
  because the initramfs is exactly where the disk gets unlocked: it runs
  `systemd-cryptsetup`, it holds the TPM unseal, it sees the passphrase. Someone
  who can write `/boot` can replace it and get code execution before the root is
  open, with Secure Boot on and every signature valid. I have not tested whether
  the kernel applies any check of its own to an initrd received this way; the
  firmware certainly does not.
- **The command line.** It lives in a Boot Loader Specification entry, an
  editable text file next to the kernel. Every hardening karg
  (`lockdown=integrity`, `init_on_free=1`, …) can be removed by anything that can
  write the ESP or `/boot`.

So owner-enrolled Secure Boot here buys a verified *kernel*, and that is worth
having — it is what makes `lockdown=integrity` engage — but it does not buy a
verified *boot*. Nobody should read "Secure Boot is on" on this backend as
meaning the early boot is trustworthy.

#### Which PCR distinguishes one generation from another

Not PCR 11. `systemd-stub` extends PCR 11 with the UKI's sections, and the stub
only runs when the firmware loads a UKI. Here the firmware loads the kernel
directly, no stub is involved, and PCR 11 stays at its reset value — so a
per-generation prediction for PCR 11 would predict a constant that every
generation satisfies, including the rolled-back one it exists to reject. PCR 12,
which the stub uses for the command line it was passed, is empty for the same
reason.

**PCR 4 is the one that does distinguish them.** The firmware measures every
image it loads into PCR 4, and each generation deploys its own kernel file at its
own path, so PCR 4 already separates generations without any UKI. That reframes
the earlier advice in this document: PCR 4 was called unusable because it changes
on every OS update and would send the user to the recovery key each time — but
that is only true for a *fixed* policy. A per-generation **signed** PCR policy is
precisely the mechanism that makes a changing PCR usable, and it is what
`keys/tpm2-pcr-signing.public.pem` was reserved for. If generation-bound unlock is
being built, PCR 4 + PCR 7 with signed per-generation predictions is the shape
that works on the backend we actually have.

One thing it still does not give you: **anti-rollback**. A signed prediction for
generation N-1 stays valid, so an attacker who puts back the older signed kernel
gets a PCR 4 value that matches a prediction the TPM will honour. Refusing that
needs monotonicity the PCRs cannot express — a TPM NV counter that only moves
forward, with the policy bound to its current value. `systemd-cryptenroll` does
not expose that today, so anti-rollback is a gap in any PCR-only design, UKI or
not. Worth knowing before PCR 11 gets treated as the thing standing between the
system and a rollback attack.

#### Is `bootc --composefs-backend` close enough to depend on?

Evidence from bootc 1.15.2 as shipped, rather than from upstream's roadmap:

- `--composefs-backend` is an ordinary documented flag on `install to-disk` and
  `install to-filesystem`. It is not hidden, and no `BOOTC_*` environment
  variable gates it — the only `BOOTC_*` knobs in the binary are for debugging,
  loopback cleanup and SELinux.
- The lifecycle verbs exist, not just install: the binary carries
  `bootc_composefs/{boot,update,switch,status,delete,finalize,export,repo,selinux}.rs`,
  and there is a top-level `composefs-finalize-staged` subcommand.
- `--uki-addon` exists alongside it, which is the UKI-shaped half.
- `bootc upgrade` has no composefs flag, consistent with the backend being
  recorded at install time and detected afterwards.

So it is a real implementation with a complete-looking lifecycle, not a stub. What
that evidence cannot tell you is whether upstream considers it stable, and the
switch is not additive: it replaces the root filesystem model with a
content-addressed, fs-verity-backed one, so it cannot be trialled on the side of
an existing install. It also changes what `--allow-missing-verity` has to be set
to depending on the filesystem underneath.

My view: it is the only path to a signature-covered initramfs and command line on
this stack, the UKI work should be planned against it rather than against
anything bolted onto ostree, and it needs its own verified-boot campaign with the
recovery path proven before anything depends on it. What it should **not** do is
block generation-bound unlock, because PCR 4 gives that today.

## Building the UKI

Three stages at the end of `os/Containerfile`, after the rootfs is finished and
`bootc container lint` has run over it:

```
rootfs ──▶ split ──▶ uki ──▶ final
           kernel     bootc container    split rootfs
           moves out  ukify over the     + the UKI under
           of the     split rootfs       /boot
           rootfs
```

The order is forced. bootc refuses to seal a rootfs that still carries
`vmlinuz`/`initramfs.img` under `/usr/lib/modules/<kver>`, and the digest ukify
seals has to be the digest of what will actually be mounted — so the split comes
first and nothing may be added to the rootfs afterwards. `/boot` is the one tree
excluded from the digest, which is what lets the UKI live inside the image it
describes.

`bootc container ukify` is the piece that makes this cheap: it computes the
composefs digest itself and reads `/usr/lib/bootc/kargs.d`, so the hardening set
and `qos.generation=` reach the sealed command line through the same mechanism
that already existed for the unsealed one. `os/dev/uki.sh` is the script it runs,
and it fails the build if the generation did not make it into the seal.

Two things the build does *not* do:

- **It does not sign the UKI.** There is no key at image-build time that a target
  machine's firmware would accept — the root of trust is a Platform Key that
  machine generates for itself. `qos-secureboot` signs the UKI on the installed
  system, and `qos-secureboot-sign.path` watches `/boot/EFI/Linux` so a staged
  update gets signed before the reboot that would otherwise be refused.
  Measurement does not wait for any of that: `systemd-stub` extends PCR 11 with
  the command line whether or not anyone signed the PE.
- **It does not get the digest right first time.** `bootc container ukify` and
  `bootc install` compute different composefs digests for the same image, so
  install refuses the UKI:

  ```
  The UKI has the wrong composefs= parameter (is 'sha512:<A>', should be sha512:<B>)
  ```

  That is [bootc#2334](https://github.com/bootc-dev/bootc/issues/2334). The
  recovery is to seal the value install asked for and install again, and it
  converges because the digest covers the rootfs — rebuilding the UKI changes only
  `/boot`, so install's expected value does not move.
  `os/dev/build-image.sh` runs that loop; `os/dev/disk-image.sh` recognises the
  refusal and prints the digest to seal rather than the raw error. If the expected
  value ever *does* move between passes, the build says so and stops, because then
  the seal is circular and needs the upstream fix.

## Interface for generation-bound unlock

`crates/system/qos-rollback` wants four things from the boot chain, and the sealed
layout supplies all four.

**PCR 7.** The firmware measures the Secure Boot policy into it on every boot, UKI
or not, and the LUKS TPM slot already binds to it.

**PCR 11.** Extended by `systemd-stub`, which runs because the firmware loads a
UKI. It carries the command line, so it distinguishes one generation from another
— which is exactly what a policy bound to it needs, and what PCR 11 could not do
on the ostree backend, where no stub was involved and the register stayed at its
reset value.

Verifying that claim took replaying the firmware's own event log: twelve firmware
measurements plus systemd's four runtime phases, reproducing the live register
exactly. Reading a non-zero PCR 11 is not evidence that a *particular* string was
measured into it.

**`qos.generation=<N>` inside the signature.** Written into
`/usr/lib/bootc/kargs.d/02-generation.toml` at image build from the
`QOS_GENERATION` build argument, which is where `bootc container ukify` reads it.
The alternative — a Boot Loader Specification entry under `/boot`, assembled by the
bootloader — was available all along and deliberately not taken: a file on the ESP,
outside any signature, editable by anything that can write there, would look like
compliance and provide none of it.

**The unlock hook — available in shape, with these concrete facts.** An encrypted
install made by `qos-disk-encrypt` names the volume `luks-<LUKS-UUID>`, because
that is what dracut derives from the `rd.luks.uuid=` karg. So the key file the
hook must write is:

```
/run/cryptsetup-keys.d/luks-<LUKS-UUID>.key
```

`systemd-cryptsetup` consults that path before it tries any token, so a hook that
writes it takes precedence over the `tpm2-device=auto` slot without conflicting
with it — the TPM slot stays as the fallback for a boot where the hook did not
run. Ordering the hook `Before=systemd-cryptsetup@luks-<uuid>.service` inside the
initramfs is the remaining work, and it needs the `qos-rollback` binary installed
into the initramfs by a dracut module.

None of this is wired up here, because the crate is not on this branch.

## Disk encryption

`qos-install --encrypt` produces a LUKS2 root. The block work is
`/usr/libexec/qos-disk-encrypt`.

**It does not require a TPM.** `bootc install --block-setup tpm2-luks` seals the
one and only key slot to a TPM and refuses to run without one, which turns
hardware absence into no encryption at all — the opposite of what absence should
do. So qOS builds the layout itself (ESP + `/boot` + LUKS2 root) and installs
`bootc ... to-filesystem` into the opened mapping.

The volume is always created with a **recovery key**, shown once during install.
Everything else is a slot layered on top of it:

| Machine | Slots after install | Boot experience |
|---|---|---|
| TPM + continuous target verified-boot preflight | recovery key, TPM2+PIN | type the PIN |
| TPM without verified boot | recovery key, passphrase | type the passphrase |
| No TPM | recovery key, passphrase | type the passphrase |

The installer and privileged backend both require a startup secret. A short PIN
is accepted only when TPM2 and Secure Boot are active, the running kernel
reports integrity or confidentiality lockdown, **and a dedicated preflight
proves the installed target retains that verified-boot boundary**. The current
installer ships no such preflight, so it deliberately requires a strong
passphrase even on a TPM-equipped machine until installation-time UKI signing
and firmware trust enrollment are integrated. Checking the EFI variable
without checking either its runtime consequence or the target handoff would
make a live-medium firmware flag sufficient evidence for a security boundary.
TPM presence with unverified early boot instead
uses a normal passphrase slot: sealing against a PCR 7 value that merely says
"Secure Boot is off" would not prevent replacement boot code from capturing the
PIN and must not be presented as hardware-backed protection.

The TPM slot binds to **PCR 7 only**, set explicitly rather than inherited.
PCR 7 is the Secure Boot policy, so the seal breaks if the boot policy changes.
Binding to a register that covers the boot artefacts themselves sounds stronger and
is unusable on its own: it changes on every OS update, so every update would send
the user to the recovery key. That is what the signed per-generation predictions in
[rollback.md](rollback.md) are for — the authority signs the values the *next*
generation will produce, so an expected change stops being a lockout. It is also
why the generation has to be on a measured command line: without it, a prediction
the authority signed for one generation would be satisfied by any other.

Consequences to expect:

- Turning Secure Boot on or off after install changes PCR 7, so the TPM slot
  stops opening. Unlock with the recovery key, then:

  ```bash
  sudo qos-disk-encrypt enrol /dev/<luks-partition> --reseal
  ```

  which replaces the stale TPM slot, authorising with the recovery key you just
  typed. Add `--pin-file` to re-seal with a PIN.
- A replaced motherboard or a cleared TPM is the same case, and has the same fix.
  This is the reason the recovery key is mandatory rather than offered.

### An initramfs is required

The kernel links the boot-path storage, USB, HID and network drivers statically,
which is why `/etc/kernel/install.conf` used to disable initrd generation. That
was wrong for an encrypted root: static drivers get the kernel as far as *seeing*
the block device, and opening a LUKS volume takes early userspace —
device-mapper setup, a TPM unseal or a passphrase prompt, `systemd-cryptsetup`.
`os/Containerfile` now generates the initramfs explicitly after the kernel swap,
because the base's `layout=ostree` kernel-install config deliberately leaves
initrd generation to the image build.

## Testing it in a VM

`os/dev/vm.sh` always boots the 4 MB Secure-Boot OVMF build with SMM on and the
pflash marked secure, because that is the only Fedora build with TPM support: the
2 MB one reports zero active PCR banks, so nothing is measured and every PCR reads
zero. `vm.sh firmware` prints what was selected and whether it can measure; a
`--tpm` boot on a host that has only the 2 MB build is refused rather than run
against a firmware that cannot measure into it.

```bash
./os/dev/vm.sh boot --tpm --attach /tmp/qos-vm/enc.qcow2   # TPM 2.0 via swtpm
./os/dev/vm.sh boot --secboot                              # keep the variable store
```

Enforcement is a property of the **variable store**, not of a qemu flag. By
default the store is pristine — no keys, Secure Boot in setup mode, re-seeded
every boot — which still measures into PCR 7 and is exactly the state
`qos-secureboot enroll` expects. `--secboot` keeps that store across boots so the
enrolment done inside the guest survives a restart of qemu, making enrolment an
explicit step rather than something the firmware template arrived with.
`QOS_SECBOOT_KEYS=microsoft` boots the vendor-key store on purpose; note that it
rejects an owner-signed UKI outright, so nothing boots.

`--tpm` attaches a software TPM, **started** (`--flags
not-need-init,startup-clear`), whose state persists in `$QOS_TPM_DIR`, so a disk
enrolled in one boot can be unlock-tested in the next. Without that flag the
emulator answers `TPM_RC_INITIALIZE` to every command and the boot is not measured
at all, which looks exactly like a UKI that failed to measure.
