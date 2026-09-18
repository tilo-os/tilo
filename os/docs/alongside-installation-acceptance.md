# Disposable alongside installation acceptance

Use the existing `os/dev/test-installer-cycle.sh`; it already supplies the fixture,
UI driver, partition preservation checker and separate installed-target boot.
Do not build another OS, installer binary, kernel or foreign operating system to
run this check. Prepare the native ARM QEMU host and tooling while image signing
finishes, then use the exact final signed **generic ARM** installer artifact.

## Scope and inputs

- The Lenovo image's board-specific DTB targets physical Lenovo hardware, not the
  QEMU virt machine. A generic ARM VM run tests the shared installer/payload and
  alongside behavior; it does not establish Lenovo media boot acceptance.
- Preserve the pristine signed medium. `vm.sh boot-installer` creates a writable
  qcow2 overlay, plus a separate disposable 40 GiB logical target disk.
- Record the medium SHA256, final signed OCI digest, public trust identity and
  helper source revision before running. The helper overlay must include the
  target-sidecar, exact boot-entry promotion and composefs `/etc` fixes.
- Use native aarch64 KVM, architecture-matched QEMU/UEFI, either GL/virgl or explicit software graphics, and no physical disks, USB,
  TPM or firmware passthrough.
  No-TPM passphrase mode is the first required path. Development Secure Boot and
  software TPM evidence are separate, explicitly configured tests.
- Required fixture tools: `qemu-img`, `sfdisk`, `mkfs.vfat`, `mkfs.ntfs`,
  `mkfs.ext4`, `mattrib`, `mcopy`, `mdir`, `mmd`, `mtype`, `ntfscp`, `debugfs`.
  UI observation also requires `tesseract`; vm.sh requires its documented QEMU,
  monitor and screenshot dependencies. Inspect installed tools; install missing
  host tools without rebuilding the retained native payload.

## Two bounded cycles

Run in an isolated native Linux VM host with storage outside `/tmp`; environment
paths below are placeholders for that host's reviewed artifact directory:

```sh
TILO_DISTROBOX='' TILO_VM_ARCH=aarch64 TILO_ACCEL=kvm \
TILO_VM_CYCLE=prepared-alongside-ARTIFACT_ID \
TILO_INSTALLER_CYCLE_STORAGE=/work/prepared-storage \
  ./os/dev/test-installer-cycle.sh /work/verified-installer.qcow2 \
  --prepared-alongside --graphics software --out /work/prepared-evidence

TILO_DISTROBOX='' TILO_VM_ARCH=aarch64 TILO_ACCEL=kvm \
TILO_VM_CYCLE=guided-alongside-ARTIFACT_ID \
TILO_INSTALLER_CYCLE_STORAGE=/work/guided-storage \
  ./os/dev/test-installer-cycle.sh /work/verified-installer.qcow2 \
  --guided-alongside --graphics software --out /work/guided-evidence
```

Prepared mode uses an existing 2 GiB shared ESP, 512 MiB NTFS and ext4 sentinel
partitions, and a distinct prepared Tilo partition. Guided mode uses a foreign
512 MiB ESP and the same foreign filesystem sizes, leaving the rest unallocated;
the installer must create its dedicated ESP/root in that free range.

The synthetic EFI files are recognizable sentinel text, **not executable Windows
or Linux bootloaders**. This deliberately tests preservation and correct menu
paths without redistributing or installing another OS. Passing these cycles
never means that real Windows boot was tested.

## Evidence required

1. The real graphical installer reaches Finish using its normal keyboard/UI flow.
   Raw recovery-key frames and serial are withheld/scrubbed by the existing tool.
2. After power-off, entire NTFS and ext4 partition hashes equal their recorded
   originals. Existing Microsoft/Fedora loader bytes also match. GPT partition
   identities and geometry match the expected preserved layout. Guided mode
   additionally requires the **entire foreign ESP** to remain byte-identical;
   prepared mode permits only the intended shared-ESP Tilo additions.
3. `alongside-preservation-before-target-boot.txt` records that first independent
   installation-stage result. A later OOBE failure does not erase this evidence
   or turn it into a complete cycle pass.
4. `vm.sh boot-target` attaches only the target qcow2 plus private firmware; the
   installer medium/overlay is absent. Retain QEMU command/device evidence from
   the host when observing the run, and confirm no backing link points to media.
5. The cold boot accepts the enrolled passphrase, reaches the real greeter and
   completes the normal first-login/desktop checks. After stopping the target,
   preservation is verified again. Only then does the tool write `verdict.txt`
   containing `PASS`.

Keep stage evidence separate: image/signature verification, byte preservation,
installed-target boot and physical Lenovo compatibility prove different things.
If the next run fails, fix/retry that exact stage using the same immutable medium
when its contents are unchanged; do not restart unrelated compilation.

The physical Lenovo installation-medium session must separately verify display/GPU, keyboard/touchpad, storage discovery, USB and network access before any internal installation. Audio, Bluetooth, battery reporting and suspend/resume remain deferred follow-up checks. Merely reaching a generic VM desktop cannot satisfy those Lenovo checks.

`--graphics software` selects virtio 2D for both boots, permitting the retained
shell's CPU-rendered KMS dumb-buffer path on hosts without `/dev/dri`. It retains
the exact same graphical installer, framebuffer OCR, unlock, OOBE and desktop
acceptance checks. Passing it proves software-rendered behavior, not accelerated
GPU functionality. `--graphics gl` (the default) retains the GL/virgl path for
suitable hosts. `scenario.txt` records the explicitly selected mode. Neither
selection pre-judges whether the real framebuffer checks will pass.

## Profile encoder on the acceptance host

Build the headless production-shared encoder once using
`tools/imager-profile/README.md`, then put the native executable in the tools
image. Set both `TILO_PROFILE_ENCODER=/absolute/path/to/tilo-imager-profile` and
`TILO_PROFILE_ENCODER_SHA256=<independently recorded SHA256>` for each cycle.
The cycle checks the digest before preparation and again immediately before
encoding, and records it in `scenario.txt`. Missing or mismatched pins fail
before VM creation. No Cargo dependency is needed on that host.

Local developers can explicitly select `--build-profile-encoder` to use the
small standalone locked Cargo workspace instead. This cannot be combined with
prebuilt encoder settings and is recorded as `explicit-local-build`, not as a
verified binary hash. No path invokes the Imager GUI workspace build.
