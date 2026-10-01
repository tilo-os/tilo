# Tilo headless VM dev toolkit (`os/dev/`)

Build the Tilo bootc image and boot it in a headless QEMU VM to inspect bootup,
capture screenshots, drive the greeter, and pull logs — no physical display, no
GUI. Built for the reference dev box (immutable Fedora/Bazzite host, build deps
in a distrobox), but configurable for any host via env vars.

```
os/dev/
  _common.sh       shared config + helpers (override via env)
  build-image.sh   podman build (in-image compile) -> disk-image.sh -> qcow2
  disk-image.sh    container image -> bootable qcow2 (systemd-repart + bootc install)
  vm.sh            boot / login / screenshot / logs / stop
```

## Prerequisites

The host is assumed immutable, so qemu and friends live in a **distrobox**
(default name `my-distrobox`). Install once:

```bash
distrobox enter my-distrobox -- sudo dnf install -y \
    qemu-system-x86 qemu-img edk2-ovmf virglrenderer socat ImageMagick
```

Note that `qemu-img` may resolve to an Android SDK copy earlier on `$PATH`;
`disk-image.sh` calls `/usr/bin/qemu-img` by path for that reason.

Host needs: rootless `podman`, **passwordless `sudo`** (for the loopback mounts
`disk-image.sh` needs and for offline journal extraction), `/dev/kvm`, and a GPU
render node under `/dev/dri` (a Mesa iGPU is strongly preferred for GL — see
gotchas). The disk-image step also needs a **local registry** it can push to
(`docker.io/library/registry:2`, started on demand): the composefs backend
refuses to install from container storage.

If your host has qemu + tools natively, set `TILO_DISTROBOX=""` to skip the box.

## Quick start

```bash
# 1. Build the image (≈ a few min incremental; first cargo build is long).
TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=<published-64-hex-owner-key> ./os/dev/build-image.sh

# …or skip the browser and the WPE WebKit engine stage that builds its engine
# from source — ~45 of the ~60 minutes a cold build costs. The boot chain is
# untouched, so this is the build to use when the question is "does it boot?".
TILO_WITH_BROWSER=0 TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=<published-64-hex-owner-key> ./os/dev/build-image.sh

# …or don't build at all. The image builds in CI here, so the local half of
# the pipeline is often just the disk step: pull what CI published and image
# that. Anyone may pull from registry.tilo.so; no login.
podman pull registry.tilo.so/tilo/tilo:<tag>
podman tag  registry.tilo.so/tilo/tilo:<tag> localhost/tilo:latest
TILO_SKIP_BUILD=1 ./os/dev/build-image.sh

# 2. Boot + greeter + login + desktop, screenshots into a dir:
./os/dev/vm.sh capture-boot ./vm-artifacts

# …or drive it step by step:
./os/dev/vm.sh boot --gl                # GL so the DRM compositor can render
./os/dev/vm.sh wait                     # block until greetd is up
./os/dev/vm.sh login --passcode-only    # one-account disk: types the passcode (tilo)
./os/dev/vm.sh login                    # two-field form: types tester / tilo
./os/dev/vm.sh shot desktop             # -> /tmp/tilo-vm/desktop.png
./os/dev/vm.sh serial 'greetd|shell|panic'   # grep the boot console
./os/dev/vm.sh stop
```

Login: **`tester` / `tilo`** (from `os/config.toml`).

## Display modes

QEMU's `screendump` can only read the surface its display backend exposes. There
is **no single headless mode that captures both the 2D greeter and the GL
compositor**, so pick per task:

| Mode | qemu flags | `screendump` captures | Compositor (`shell`) |
|------|-----------|----------------------|----------------------|
| `--2d` (default) | `-device virtio-vga -display none` | GRUB, kernel console, greeter, **and the desktop** | llvmpipe — slow, but it runs |
| `--gl` | `-device virtio-vga-gl -display egl-headless,rendernode=<iGPU>` | **GL compositor scanout** (2D content → "no surface") | hardware GL via virgl |

`vm.sh serial` works in both (the image boots with `console=ttyS0` on x86 and
`console=ttyAMA0` on ARM).

* **`--2d` reaches the desktop.** `/usr/bin/shell --backend drm` paints the
  full desktop through llvmpipe, slowly. `--2d` captures a whole boot — firmware,
  bootloader, console, greeter, login and desktop — in one continuous take, and
  it is what `record-boot.py` defaults to.
* **Graphical greeter.** greetd runs `cage -s -- tilo-greeter` on vt1
  (`/etc/greetd/config.toml`), a graphical Wayland session with a two-field
  form. Return on the username submits, so anything driving it wants Tab
  between the fields.

`--gl` additionally needs `qemu-ui-egl-headless` installed in the VM
environment; without it qemu exits with `Display 'egl-headless' is not
available`. On the reference machine qemu 10.1.5 then **segfaults** in
`hw-display-virtio-vga-gl.so` at startup, so GL headless is currently
unavailable there — which costs nothing for boot capture, given the row above.

## Recording a boot as video

Record a boot with:

```bash
./os/dev/record-boot.py --login --after-login 8 --out boot.mp4
```

The recording preserves capture timestamps. `--login` enters credentials when
greetd is ready. Under `--mode gl`, framebuffer capture is unavailable until
the compositor starts; the script reports the missing frames and time range.

## Testing the boot chain (TPM, encryption, Secure Boot)

```bash
distrobox enter my-distrobox -- sudo dnf install -y swtpm swtpm-tools \
```

| Flag | Effect |
|------|--------|
| `--tpm` | software TPM 2.0 via swtpm, **started** (`--flags not-need-init,startup-clear`); state persists in `$TILO_TPM_DIR`, so a disk enrolled in one boot unlocks in the next |
| `--secboot` | keep the variable store across boots, so keys enrolled in the guest survive a restart of qemu |
| `--attach PATH` | a blank second virtio disk (`/dev/vdb`), created if missing — lets a booted image install onto a fresh disk without building the installer medium |

For a reproducible installer-to-target cycle, give the run a unique, portable
name and initialize it once:

```bash
export TILO_VM_CYCLE="release-${CI_RUN_ID:-local}-x86_64"
./os/dev/vm.sh init-cycle
./os/dev/vm.sh boot-installer --no-tpm
# After installation and power-off, retain the same environment:
./os/dev/vm.sh boot-target --no-tpm
```

`init-cycle` removes only that validated cycle's mutable overlay, target, TPM,
runtime and firmware state. Subsequent boots with the same name preserve them,
while the release-medium qcow2 remains read-only behind its overlay. A named
cycle also keeps the OVMF variable store across the installer-to-target reboot.

The same commands run natively on ARM64. `TILO_VM_ARCH` defaults to the normalized
runner architecture (`x86_64` or `aarch64`); set it only for a deliberate TCG
cross-architecture diagnostic. ARM uses `qemu-system-aarch64 -machine
virt,gic-version=max`, AAVMF/QEMU_EFI pflash, `virtio-gpu-pci`, and an explicit
xHCI tablet. Install `edk2-aarch64` on Fedora or `qemu-efi-aarch64` on
Debian/Ubuntu. The ARM full cycle currently supports the strong-passphrase
`--no-tpm` scenario. It rejects `--tpm` and Secure Boot key-store selection
until one exact measured AAVMF build and its PCR behavior have a release proof;
merely finding a TPM-capable firmware is not evidence of measured boot.

Protected `imager-v*` release jobs additionally run the x86-64 graphical cycle
with `--software-tpm`. The job exports only the public certificate from the
short-lived release signer, verifies its protected fingerprint, and uses
`virt-fw-vars` to create a disposable enforcing OVMF store which trusts that
exact certificate. The installed target must then cold-boot and unlock with the
configured short PIN. Manual rehearsals have no signing lease and therefore do
not claim this proof. This gate intentionally does not run on ARM64 until a
specific measured AAVMF build and stable PCR behavior have been qualified.

### Firmware: only the 4 MB OVMF build can measure

Fedora ships two. `OVMF_CODE_4M.secboot.qcow2` has TPM support compiled in;
`OVMF_CODE.secboot.fd` (2 MB) does not, and its TCG2 reports **zero active PCR
banks** — every PCR reads zero, no event log appears, and the TPM is never
started, which is indistinguishable from a UKI that failed to measure. So the
4 MB build is selected unconditionally, and `--tpm` against a host that only has
the 2 MB one is **refused** rather than run. `vm.sh firmware [--tpm] [--secboot]`
prints the selection, including whether it can measure.

Secure Boot *enforcement* is not a qemu flag — it is whether the variable store
carries keys. `TILO_SECBOOT_KEYS` picks the store:

| Value | Store |
|-------|-------|
| `none` (default) | `OVMF_VARS_4M.qcow2` — no keys, setup mode. Measures into PCR 7, accepts an owner-signed UKI; re-seeded on ad-hoc boots and persisted within a named VM cycle |
| `persist` (`--secboot`) | the same pristine store, kept across boots so an enrolment in the guest sticks |
| `microsoft` | `OVMF_VARS_4M.secboot.qcow2` — Microsoft's KEK/db. **Rejects owner-signed images**, so nothing boots; for testing vendor-key enforcement on purpose |
| *path* | a prepared store |

Enrolling owner keys is a deliberate step, never something the template arrives
with: boot with `--secboot`, run `/usr/libexec/tilo-secureboot enroll` in the
guest, reboot the guest.

Use one TPM state dir per scenario (`TILO_TPM_DIR=…`) or a second run unseals
against the first run's TPM.

See `docs/security/boot-chain.md` for what these are for.

Two drivers automate the scenarios end to end. Both drive the guest's own
`/usr/libexec/` tooling, so what they exercise is the shipped path rather than a
re-implementation of it, and both need `sshpass` on the host.

```bash
# encrypted install + boot the result. --passphrase types the secret at the
# LUKS prompt, which is the proof that early userspace opened the root.
./os/dev/test-encrypted-install.sh tpm      os/output/qcow2/disk.qcow2
./os/dev/test-encrypted-install.sh no-tpm   os/output/qcow2/disk.qcow2 \
    --no-tpm --passphrase hunter2
# reproduce the pre-initramfs state on a real boot: same install, initrd line
# removed from the boot entry.
./os/dev/test-encrypted-install.sh noinitrd os/output/qcow2/disk.qcow2 --no-initrd

# generate keys, sign, check enrolment refuses unsigned, enrol, reboot enforcing
./os/dev/test-secureboot.sh os/output/qcow2/disk.qcow2
```

## Extracting logs

1. **Serial console** (`vm.sh serial [grep]`) — always available; captures the
   kernel + systemd boot and anything a unit routes to the console.
2. **App/service errors → console**: add to the unit and rebuild —
   ```ini
   Environment=RUST_BACKTRACE=1
   StandardError=journal+console
   ```
   This is exactly how the compositor's startup panic was first captured.
3. **Offline journal** (`vm.sh journal [grep]`) — stop the VM, then mount the
   qcow2 via `qemu-nbd` and run `journalctl -D`. General (no image changes) but
   only yields data if the guest has a *persistent* journal (Tilo does:
   `/var/log/journal`). Caveat: `qemu-nbd` run inside a **rootless** distrobox
   cannot open the host `/dev/nbd0` (userns permission). If you hit
   `Failed to open /dev/nbd0: Permission denied`, run `qemu-nbd` from a real-root
   context, or fall back to method 2 (route the unit's stderr to the console).

## Operational notes

- **Image base = `fedora-bootc:44`.** F42 is EOL. Binaries are built in a
  Fedora 43 toolbox (glibc 2.42) and run on F44 (newer glibc) — forward-compatible.
  Keep the image base ≥ the toolbox's Fedora release. (Our binaries only need
  GLIBC ≤ 2.39 anyway.)
- **Browser (Servo) omitted.** Unmaintained and doesn't build on GCC 15 / C23
  (glsl-optimizer `once_flag` conflict). `--with-browser` re-enables it with
  `CFLAGS=-std=gnu17`, but you must also restore the `COPY browser` lines in the
  Containerfile.
- **AF_UNIX socket path ≤ 108 bytes.** The monitor socket lives at
  `/tmp/tilo-vm/mon.sock`; don't relocate `TILO_RUN` somewhere deep.
- **Use the AMD/Intel iGPU for GL, not NVIDIA.** virglrenderer + NVIDIA's EGL is
  unreliable; `detect_rendernode` skips `0x10de`. Override with `TILO_RENDERNODE`.
- **Compositor backend.** `shell` defaults to the **winit** backend (nested —
  needs a parent Wayland/X display) and panics on a bare TTY. The Wayland-session
  `.desktop` entries launch it with **`--backend drm`** (smithay libseat + udev +
  DRM/KMS) so it can be the display server on real hardware / in the VM.

## Installer (self-install)

Tilo installs with its own installer: a bootable live qcow2 that
runs the graphical installer (`installer/`, binary `tilo-installer`) under `cage`
and **self-installs the booted image** onto a target disk — offline, no network
pull.

For Windows ARM64 development, dispatch `os-dev-installer.yml` with
`installer_rootfs=ext4` and the exact
`registry.tilo.so/tilo/tilo-ci@sha256:<digest>` from a successful `os-dev.yml`
ARM64 run. It layers the installer on that already-built unsigned image and
uploads a compressed qcow2; it does not rebuild or sign the OS. Download the
`tilo-dev-installer-aarch64-<run>` artifact, verify its SHA-256, decompress the
qcow2 once, then run `tilo-vm scenario install --medium <qcow2> --graphics
software` on the Windows WHPX host. Keep the medium as the immutable base for
later throwaway install overlays. Component changes in a running dev VM use
`tilo-vm vm deploy`; normal OS image changes use `tilo-vm vm upgrade`.
When building that disposable medium in WSL instead, set
`TILO_PODMAN_BUILD_NETWORK=host` only if Podman's private build namespace
cannot reach the package mirrors while the WSL host can.
Keep `TILO_INSTALLER_OUTPUT` on WSL's native ext4 filesystem during the
build; the raw disk can allocate its full logical size on `/mnt/c`. Export
only the finished qcow2 or its compressed form to Windows.
For a running WHPX installer, `installer-console.py --socket
tcp://127.0.0.1:<console-port> state` reads the same action protocol that the
scenario driver uses.

Alongside installs on a Windows disk have two ESPs. Stock bootc can choose the
first ESP instead of the one mounted under the new Tilo root. The live installer
therefore builds a pinned, installer-only bootc helper that verifies and uses
the mounted ESP; `tilo-install` checks for it before guided partition creation.
The helper is absent from the installed OS image. `release-imager.yml` may layer
newer main installer code onto an earlier CI-signed OS only when that OS source
is an ancestor of the dispatched main commit and the signed target's SLSA
source still matches the explicit OS pin.

A CI install medium can instead carry a separately signed generic or Lenovo
ARM64 target in
`/var/lib/tilo-installer/target-oci/`. Before changing a disk, the guest
checks its exact OCI manifest digest, offline Cosign signature, and CI SLSA
provenance with the immutable Tilo public key. It imports only that target
from the local OCI layout, checks the target image's bound update origin, and
installs from the imported image ID. An invalid or partial target stops the
install; it never falls back to copying the live medium. VM proof v2 records
the booted medium and installed target identities separately. The existing
self-install path remains the development default when no offline target is
present.

How it works:

- `Containerfile.installer` bakes the kernel arg **`tilo.install`**. That single
  flag drives everything: `tilo-session-select` launches the installer (not the
  greeter), the `tilo-install` backend refuses to run without it, and
  `tilo-installer-setup.service` (which drops in the passwordless sudoers rule)
  only fires on the live medium. The **installed** system has no such arg, so it
  boots straight to the desktop.
- The backend (`/usr/libexec/tilo-install`) does: `bootc image copy-to-storage`
  → privileged `podman run … localhost/bootc bootc install to-disk --wipe
  --filesystem btrfs --target-imgref "$TILO_TARGET_IMGREF" <disk>` → mounts the
  fresh root and writes `name:hash` (password via the pure-Rust `tilo-crypto`) into the
  deployment's writable `/etc/tilo/install-user.conf`.
- On the installed system's first boot, `tilo-firstboot-user.service` creates the
  account from that file (`useradd -p <sha512-crypt-hash> -G wheel`) and shreds it. The
  installer then launches that account once into OOBE; after greetd consumes that
  first-session handoff, it restores the normal password greeter for every later
  login and reboot.

```bash
# 1. Build + load the desktop image (the installer is FROM localhost/tilo).
./os/dev/build-image.sh

# 2. Build the installer qcow2 (-> os/output/installer/qcow2/disk.qcow2).
./os/dev/build-custom-installer.sh
#    …or test the OS plumbing without the real UI:
./os/dev/build-custom-installer.sh --stub-ui
# VM size experiment: use a fixed 40 GiB ext4 live root. The builder rejects
# images whose 2x OCI size plus 4 GiB slack would exceed it. Physical use
# requires end-to-end VM peak usage and boot checks for this filesystem.
TILO_INSTALLER_OUTPUT="$PWD/os/output/installer-compact-vm" \
  ./os/dev/build-custom-installer.sh --compact-ext4
#    A local diagnostic image may explicitly add SSH + a wheel account:
TILO_INSTALLER_DEV_PASSWORD='use-a-unique-local-secret' \
  ./os/dev/build-custom-installer.sh --dev-login

# 3. Boot the installer with a blank target disk attached as /dev/vdb.
./os/dev/vm.sh boot-installer --gl     # GL so the installer UI can render
./os/dev/vm.sh wait                    # block until greetd brings it up
#    (drive the UI: pick /dev/vdb, set user/pass, install)

# 4. Stop, then boot the installed result.
./os/dev/vm.sh stop
./os/dev/vm.sh boot-target --gl        # boots $TILO_RUN/target.qcow2
```

Release installer media contain no login account and mask `sshd`; joining a
network must not expose a repository-known maintenance credential. The
explicit `--dev-login` build is for isolated local diagnostics only.

To prove the SELinux handoff in a disposable VM, build a diagnostic medium
from the same source and run the full graphical installer/first-boot cycle:

```bash
export TILO_INSTALLER_OUTPUT="$PWD/os/output/installer-selinux-proof"
export TILO_INSTALLER_DEV_PASSWORD="$(python3 -c 'import secrets; print(secrets.token_urlsafe(24))')"
./os/dev/build-custom-installer.sh --dev-login
./os/dev/test-installer-cycle.sh \
  "$TILO_INSTALLER_OUTPUT/qcow2/disk.qcow2" \
  --build-profile-encoder --selinux-audit-ssh \
  --out "$PWD/os/output/installer-selinux-proof-result"
unset TILO_INSTALLER_DEV_PASSWORD
```

`--selinux-audit-ssh` requires `sshpass` on the VM host and `auditctl` and
`ausearch` in the guest. Guest inspection runs through
`os/dev/installer-selinux-audit.sh` so the release-gate cycle script stays free
of guest-shell channels. It fails unless the installer is enforcing, its
live-only policy is loaded, kernel audit is enabled with zero lost records,
and both audit records and the system journal contain no AVC or USER_AVC. The
existing cycle then proves the Finish screen, encrypted target boot, first
login, and a second cold boot. The diagnostic medium adds SSH and a temporary
wheel account, so this result complements the release workflow's graphical
cycle on the SSH-disabled published medium; it does not replace that gate.

The installed system's update origin defaults to
`registry.tilo.so/tilo/tilo:latest`. `TILO_TARGET_IMGREF` may select a tag or digest
in that same signature-required repository; the backend rejects other origins.

## Configuration (env vars)

`TILO_DISTROBOX` `TILO_IMAGE` `TILO_OUTPUT` `TILO_DISK` `TILO_RUN`
`TILO_VM_CYCLE` `TILO_RENDERNODE` `TILO_USER` `TILO_PASS` `TILO_MEM` `TILO_SMP` — see `_common.sh`.
Custom installer: `TILO_INSTALLER_IMAGE` `TILO_INSTALLER_OUTPUT`
`TILO_INSTALLER_DISK` `TILO_TARGET_DISK` `TILO_TARGET_SIZE` `TILO_TARGET_IMGREF`.
`TILO_INSTALLER_DEV_PASSWORD` is required only with `--dev-login`.
PCR signing runs only when `TILO_REQUIRE_PCR_PREDICTION=1` (release CI) or the
developer explicitly sets `TILO_ALLOW_LOCAL_PCR_SIGNING=1`; exporting a signer
path alone never invokes it. `use-published-image.sh` verifies a digest-pinned
Tilo release and its provenance, loads it as `TILO_IMAGE`, and prints its verified
embedded generation for installer-media builds.
Disk image: `TILO_DISK_REG_PORT` (registry port), `TILO_RETAINED_GENERATIONS`.
