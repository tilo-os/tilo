# Secure Boot and disk encryption on qOS

qOS boots UEFI → shim → GRUB → its own kernel → its own initramfs → systemd.
shim and GRUB are inherited from the Fedora bootc base and keep their vendor
signatures; the kernel is built here rather than taken from Fedora. Every one of
those facts changes what Secure Boot can mean, so this document describes the
chain as it is, not as a stock Fedora bootc image's would be.

The loader is not a free choice. bootc's ostree backend — the backend every qOS
install path uses — installs the bootloader by calling bootupd, and bootupd's
only EFI payload is grub2 + shim. `bootloader = "systemd"` is refused outright by
that backend rather than falling back, so systemd-boot would mean moving to
bootc's composefs backend. See `os/usr/lib/bootc/install/40-qos.toml`.

## The chain's vendor signatures stop at the kernel

A stock Fedora bootc image inherits a chain that verifies out of the box on a
factory machine: shim is signed by the Microsoft UEFI CA, GRUB by Fedora, the
kernel by Fedora. qOS keeps the first two and breaks the third: the kernel is a
local rebuild (`os/kernel/Containerfile`) that no distribution key has ever
touched.

So on a machine with the factory keys enrolled and Secure Boot on, shim and GRUB
load and the kernel is rejected — qOS does not boot. That is not a packaging
oversight to be fixed by getting the kernel signed by someone: a qOS vendor key
would only move the problem. A vendor key means images *we* signed are attestable
and an image the owner built is not, which is the property this system is
specifically designed to keep.

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

- **`qos-secureboot-sign.path`** watches `/boot/ostree`, so a hand-run
  `bootc upgrade` gets its new kernel signed immediately rather than at the next
  boot — which would be the boot the firmware refuses. It also watches
  `/boot/efi/EFI/systemd` and `/boot/efi/EFI/BOOT`, because `bootupctl update`
  rewrites the loaders there and that change has no natural second chance.
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
`cert-to-efi-sig-list`/`efi-updatevar`/`efi-readvar` (`efitools`). Neither is in
the image's frozen package snapshot yet, and each subcommand refuses with the
exact remedy rather than half-signing a boot chain. To ship them: add
`sbsigntools efitools` to a `dnf install` block in `os/Containerfile` and re-run
the `snapshot-rpms` workflow, which scrapes those blocks to derive the snapshot's
package set (`os/Containerfile.rpms`), then commit the new `RPMS_REF`.

### What owner-enrolled Secure Boot buys

`lockdown=integrity` (`os/usr/lib/bootc/kargs.d/01-hardening.toml`) only engages
when the kernel is told by firmware that it booted verified. Without Secure Boot
that karg is inert, so root can still load unsigned modules, read `/dev/mem` and
kexec.

PCR 7 measures the Secure Boot policy, and the LUKS TPM slot binds to it (below).
With Secure Boot off, that binding records "Secure Boot is off" — which is a
consistent measurement but not a meaningful one, since the state it attests to is
the state in which the boot chain is unverified.

### The Unified Kernel Image gap, and what it costs

The design calls for kernel, initramfs and command line sealed into one signed
binary. qOS does not have that, and the consequences are larger than "a nice
property we lack", so they are worth setting out rather than filing.

#### What signing actually covers on the ostree backend

The firmware verifies what it `LoadImage()`s. On this backend that is shim, then
GRUB, then the kernel — all PE binaries, all signable, all signed by
`qos-secureboot sign`. shim and GRUB already carry Fedora/Microsoft signatures,
but owner enrolment replaces the factory db with the machine's own, so those
signatures stop being trusted and the owner's is the one that counts; signing
appends rather than replaces, so a machine that has *not* been through enrolment
still boots on the vendor chain. What signing does **not** cover:

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

## Interface for generation-bound unlock

`crates/system/qos-rollback` wants four things from the boot chain. Two are
available now, two are not, and the two that are not both depend on the UKI gap
above rather than on anything that could be wired up around it.

**PCR 7 — available.** The firmware measures the Secure Boot policy into it on
every boot, UKI or not, and the LUKS TPM slot already binds to it.

**PCR 11 — not available, and a prediction for it would seal against nothing.**
PCR 11 is extended by `systemd-stub`, which only runs when the firmware loads a
UKI. On the ostree backend the firmware loads the kernel directly and no stub is
involved, so PCR 11 is never extended and stays at its reset value. A
per-generation signed prediction for PCR 11 would therefore predict a constant,
and a policy bound to it would be satisfied by any generation — including a rolled
back one, which is the thing it exists to prevent.

**`qos.generation=<N>` inside the signature — not available.** On this backend
the command line lives in a Boot Loader Specification entry under `/boot`,
assembled by the bootloader from `/usr/lib/bootc/kargs.d` plus install-time
`--karg`s. The value can be put there in one line, and it would be readable by
early userspace — but it would be exactly what the requirement rules out: a file
under `/boot`, outside any signature, editable by anything that can write the ESP.
Shipping it that way would look like compliance and provide none of it, so it is
not shipped.

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
| TPM, PIN given | recovery key, TPM2+PIN | type the PIN |
| TPM, no PIN | recovery key, TPM2 | unlocks silently |
| No TPM, PIN given | recovery key, passphrase | type the passphrase |
| No TPM, no PIN | recovery key | type the recovery key |

The installer now refuses to start an encrypted install on a machine with no TPM
until a passphrase is given, so the last row is reachable only by driving the
backend directly. It works, but it means typing a 48-character recovery key at
every boot, which is not a choice anyone makes on purpose.

The TPM slot binds to **PCR 7 only**, set explicitly rather than inherited.
PCR 7 is the Secure Boot policy, so the seal breaks if the boot policy changes.
PCR 4 covers the boot artefacts themselves, which sounds stronger and is
unusable: it changes on every OS update, so every update would send the user to
the recovery key. Making PCR 4 workable needs signed PCR policies
(`systemd-pcrlock` or a signed prediction), which is what
`keys/tpm2-pcr-signing.public.pem` is reserved for.

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

`os/dev/vm.sh` boots the plain OVMF build by default, which has no Secure Boot
support at all — an SB test against it proves nothing.

```bash
./os/dev/vm.sh boot --tpm --attach /tmp/qos-vm/enc.qcow2   # TPM 2.0 via swtpm
./os/dev/vm.sh boot --secboot                              # SB firmware, SMM enforced
```

`--secboot` selects `OVMF_CODE.secboot.fd`, enables SMM, and marks the pflash
secure so the variable store is not writable from outside SMM — without that,
enforcement is nominal and the guest could rewrite `db` itself. Its variable
store starts in setup mode with no keys enrolled, which is exactly the state
`qos-secureboot enroll` expects, so the whole owner-enrolment flow can be driven
inside the guest.

`--tpm` attaches a software TPM whose state persists in `$QOS_TPM_DIR`, so a disk
enrolled in one boot can be unlock-tested in the next.
