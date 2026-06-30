# Secure Boot on qOS — assessment (E2)

Status: **assessment only.** No qOS code change is required to *support* Secure
Boot on real hardware today; the gaps are verification (HW-gated) and the dev
VM firmware. Tracks roadmap item **E2** — "Secure Boot validation + disk
encryption". Disk encryption (LUKS/TPM2) is already implemented in the installer
(`qos-install --encrypt` → `bootc install to-disk --block-setup tpm2-luks`).

## Bottom line

qOS is built `FROM quay.io/fedora/fedora-bootc:44`. That base already carries
the **signed** Secure Boot bootchain components, so a qOS install on a normal
Secure-Boot PC (Microsoft UEFI CA keys enrolled, the factory default) is
expected to boot SB-enabled **with no qOS-specific signing work**, because:

- **shim** (`shim-x64`) is signed by the **Microsoft UEFI CA** — firmware trusts
  it out of the box.
- **GRUB2** (`grub2-efi-x64`) is signed by **Fedora's** key; shim carries
  Fedora's cert (vendor cert) and validates GRUB.
- **the kernel** is the stock Fedora kernel, **signed by Fedora's** Secure Boot
  signing key (same trust path as GRUB).

The bootchain is: firmware → shim (MS-signed) → GRUB (Fedora-signed, checked by
shim) → kernel (Fedora-signed, checked by shim/GRUB) → systemd.

`bootc install to-disk` installs the bootloader from the image's signed
artifacts onto the ESP (via bootupd), so the SB chain is preserved by an
install. No re-signing happens at install time.

## Why qOS doesn't need MOK / custom signing (today)

- qOS ships **no out-of-tree kernel modules** and **no DKMS** — it rides the
  signed Fedora kernel as-is. So there is nothing for the user to enroll via MOK
  (Machine Owner Key) for Secure Boot to work.
- The compositor, UI toolkit, FUSE driver and apps are all **userspace** — they
  run after the kernel and are not part of the SB measured/verified chain.
- **If that ever changes** (a custom/patched kernel, an out-of-tree driver, a
  custom GRUB), Secure Boot would then require either Fedora-style signing or a
  qOS signing key enrolled via shim+MOK (`mokutil --import`, confirmed in the
  MokManager UI at next boot). Document this loudly at that point.

## `bootc install` considerations

- bootc/bootupd write the **prebuilt, signed** shim+GRUB from the image; they do
  not generate or sign bootloaders. As long as the image keeps the signed RPMs
  (`shim-x64`, `grub2-efi-x64`, `kernel`), an installed disk is SB-capable.
- The custom self-install path (`os/usr/libexec/qos-install`) runs `bootc
  install to-disk` from the booted image, so it inherits the same signed
  artifacts. No extra work needed for SB there.
- The Anaconda ISO path (`os/installer.toml`) delegates ESP/Secure-Boot/
  bootloader setup to Anaconda, which is already SB-aware.
- **Caveat — `--wipe` and existing keys:** installing does not change the
  firmware's enrolled keys (db/KEK/PK) or any prior MOK enrollments; it only
  writes the ESP. SB enablement remains a firmware-side setting the user
  controls.

## Secure Boot + disk encryption (the TPM tie-in)

The installer's `--encrypt` binds the LUKS unlock to the TPM2. For that binding
to be *meaningful* against tamper, the measured-boot state it seals against
should include Secure Boot:

- TPM2 LUKS enrollment typically seals to PCRs that include **PCR 7** (the
  Secure Boot policy/state). With SB **on**, PCR 7 reflects the SB keys+state,
  so an attacker disabling SB or swapping the bootloader changes PCR 7 and the
  TPM refuses to release the key.
- With SB **off**, TPM2-LUKS still works but the seal is weaker (it doesn't
  catch an SB-state change). So **SB-on is the intended companion to
  `--encrypt`** on real hardware. (Exact PCR selection is an E2 follow-up; today
  `bootc ... --block-setup tpm2-luks` uses its default policy.)

## The dev VM does NOT exercise Secure Boot (HW-gated)

`os/dev/_common.sh` (`ovmf_code`/`ovmf_vars`) points QEMU at the **non-SB**
firmware: `OVMF_CODE.fd` / `OVMF_VARS.fd`. That build of OVMF has no Secure Boot
support, so `./os/dev/vm.sh` boots are always SB-off — Secure Boot cannot be
validated in the current dev VM.

To exercise SB in QEMU (a safe, optional follow-up — does not affect prod):

1. Install the SB firmware (`edk2-ovmf` provides it) **inside the VM distrobox**.
2. Point QEMU at the SB code blob and a **pre-enrolled** vars template:
   - code: `/usr/share/edk2/ovmf/OVMF_CODE.secboot.fd`
   - vars: seed `$QOS_RUN/vars.fd` from
     `/usr/share/edk2/ovmf/OVMF_VARS.secboot.fd` (this template ships with the
     Microsoft UEFI CA keys enrolled), instead of the current `OVMF_VARS.fd`.
3. Keep `-machine q35,smm=on` and `-global driver=cfi.pflash01,property=secure,value=on`
   so the firmware actually enforces SB.
4. Boot the installer/disk image; confirm with `mokutil --sb-state` (expect
   "SecureBoot enabled") and `bootctl status` inside the guest.

This is the right place to add an automated SB smoke test later; until then SB
is verified by booting a qOS install on real Secure-Boot hardware.

## Recommended next steps (E2)

1. **Real-hardware boot test**: install qOS on an SB-enabled machine, confirm it
   boots without disabling SB, `mokutil --sb-state` shows enabled.
2. **Optional QEMU SB harness**: add an `--secboot` flag to `os/dev/vm.sh`
   selecting the `*.secboot.fd` firmware + enrolled-vars template, for CI smoke
   coverage.
3. **TPM PCR policy review** for `--encrypt`: confirm/seal to PCR 7 (+ others as
   desired) so encryption gains tamper-evidence from Secure Boot.
4. **Document the MOK path** only if/when qOS introduces signed-out-of-tree
   kernel content.
