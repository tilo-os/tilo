# Secure Boot and disk encryption on qOS

qOS boots UEFI → systemd-boot → its own kernel → its own initramfs → systemd.
There is no shim and no GRUB: the image removes both, and the kernel is built
here rather than taken from Fedora. Every one of those facts changes what Secure
Boot can mean, so this document describes the chain as it is, not as a stock
Fedora bootc image's would be.

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

### The Unified Kernel Image gap

The design calls for kernel, initramfs and command line sealed into one signed
binary, so the command line cannot be edited to disable a protection. qOS does
not have that yet, and the reason is worth writing down rather than filing.

`bootc install` on the ostree backend deploys the kernel and initramfs as
separate files under `/boot/ostree/<stateroot>-<csum>/` with the command line in
a Boot Loader Specification entry beside them. Signing the kernel gets the
firmware to verify the kernel; it does nothing about the BLS entry, which stays
an editable text file. A UKI cannot simply be dropped into `EFI/Linux/` either,
because its embedded command line has to name the ostree deployment, and that
name changes with every update — a static UKI stops matching after the first one.

`bootc` 1.15.2 does carry the pieces (`--composefs-backend`, `--uki-addon`) for
the composefs backend, where the root is content-addressed and the UKI is the
deployed artefact. That is the path, and it also delivers the sealed root
filesystem the design asks for. It is experimental, it changes the root
filesystem model rather than adding to it, and enabling it demands its own
verified-boot campaign. Until then, the command line is protected by nothing
stronger than the fact that `/boot` is only writable by root.

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

The last row works but is unpleasant, and the installer should collect a
passphrase whenever no TPM is present. The backend already accepts one — the
`--pin` value becomes an ordinary passphrase slot on a TPM-less machine — so this
is a UI gap, not a capability gap.

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
