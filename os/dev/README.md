# qOS headless VM dev toolkit (`os/dev/`)

Build the qOS bootc image and boot it in a headless QEMU VM to inspect bootup,
capture screenshots, drive the greeter, and pull logs — no physical display, no
GUI. Built for the reference dev box (immutable Fedora/Bazzite host, build deps
in a distrobox), but configurable for any host via env vars.

```
os/dev/
  _common.sh       shared config + helpers (override via env)
  build-image.sh   podman build (in-image compile) -> disk-image.sh -> qcow2
  disk-image.sh    container image -> bootable qcow2 (systemd-repart + bootc install)
  vm.sh            boot / login / screenshot / logs / stop
  README.md        this file
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

If your host has qemu + tools natively, set `QOS_DISTROBOX=""` to skip the box.

## Quick start

```bash
# 1. Build the image (≈ a few min incremental; first cargo build is long).
./os/dev/build-image.sh

# …or skip the browser and the WPE WebKit engine stage that builds its engine
# from source — ~45 of the ~60 minutes a cold build costs. The boot chain is
# untouched, so this is the build to use when the question is "does it boot?".
QOS_WITH_BROWSER=0 ./os/dev/build-image.sh

# …or don't build at all. The image builds in CI here, so the local half of
# the pipeline is often just the disk step: pull what CI published and image
# that. Needs `podman login code.sandbox.example` once.
podman pull code.sandbox.example/qos/qos:<tag>
podman tag  code.sandbox.example/qos/qos:<tag> localhost/qos:latest
QOS_SKIP_BUILD=1 ./os/dev/build-image.sh

# 2. Boot + greeter + login + desktop, screenshots into a dir:
./os/dev/vm.sh capture-boot ./vm-artifacts

# …or drive it step by step:
./os/dev/vm.sh boot --gl                # GL so the DRM compositor can render
./os/dev/vm.sh wait                     # block until greetd is up
./os/dev/vm.sh login                    # types tester / quickdesktop
./os/dev/vm.sh shot desktop             # -> /tmp/qos-vm/desktop.png
./os/dev/vm.sh serial 'greetd|shell|panic'   # grep the boot console
./os/dev/vm.sh stop
```

Login: **`tester` / `quickdesktop`** (from `os/config.toml`).

## Display modes — the key gotcha

QEMU's `screendump` can only read the surface its display backend exposes. There
is **no single headless mode that captures both the 2D greeter and the GL
compositor**, so pick per task:

| Mode | qemu flags | `screendump` captures | Compositor (`shell`) |
|------|-----------|----------------------|----------------------|
| `--2d` (default) | `-device virtio-vga -display none` | GRUB, kernel console, greeter, **and the desktop** | llvmpipe — slow, but it runs |
| `--gl` | `-device virtio-vga-gl -display egl-headless,rendernode=<iGPU>` | **GL compositor scanout** (2D content → "no surface") | hardware GL via virgl |

`vm.sh serial` works in both (the image boots with `console=ttyS0`).

Two corrections to what this table used to say, both established by booting the
image rather than by reading the flags:

* **`--2d` does reach the desktop.** The compositor was described as possibly
  not starting under software GL. It starts: `/usr/bin/shell --backend drm`
  paints the full desktop through llvmpipe, just slowly. So `--2d` is the mode
  that can capture a whole boot — firmware, bootloader, console, greeter, login
  and desktop — in one continuous take, which no GL mode can, and it is what
  `record-boot.py` defaults to.
* **The greeter is not tuigreet.** greetd runs `cage -s -- qos-greeter` on vt1
  (`/etc/greetd/config.toml`), a graphical Wayland session with a two-field
  form. Return on the username submits, so anything driving it wants Tab
  between the fields.

`--gl` additionally needs `qemu-ui-egl-headless` installed in the VM
environment; without it qemu exits with `Display 'egl-headless' is not
available`. On the reference machine qemu 10.1.5 then **segfaults** in
`hw-display-virtio-vga-gl.so` at startup, so GL headless is currently
unavailable there — which costs nothing for boot capture, given the row above.

## Recording a boot as video

`vm.sh shot` gives stills, so a boot could only ever be shown as a
before-and-after pair. `record-boot.py` films it:

```bash
./os/dev/record-boot.py --login --after-login 8 --out boot.mp4
```

It dumps the framebuffer on a timer, keeps each frame's wall-clock timestamp,
and encodes at the rate the frames were really captured — so the clip runs at
the speed the machine actually booted. `--login` types the credentials when
greetd reports itself up rather than at a guessed offset.

The table above is the constraint here too, and the script does not pretend
otherwise: it reports how many frames came back empty, and warns with the
window they span, because under `--mode gl` every dump before the compositor
starts answers "no surface". A gap in that report is the honest shape of the
capture.

## Testing the boot chain (TPM, encryption, Secure Boot)

```bash
distrobox enter my-distrobox -- sudo dnf install -y swtpm swtpm-tools \
    sbsigntools efitools
```

| Flag | Effect |
|------|--------|
| `--tpm` | software TPM 2.0 via swtpm, **started** (`--flags not-need-init,startup-clear`); state persists in `$QOS_TPM_DIR`, so a disk enrolled in one boot unlocks in the next |
| `--secboot` | keep the variable store across boots, so keys enrolled in the guest survive a restart of qemu |
| `--attach PATH` | a blank second virtio disk (`/dev/vdb`), created if missing — lets a booted image install onto a fresh disk without building the installer medium |

### Firmware: only the 4 MB OVMF build can measure

Fedora ships two. `OVMF_CODE_4M.secboot.qcow2` has TPM support compiled in;
`OVMF_CODE.secboot.fd` (2 MB) does not, and its TCG2 reports **zero active PCR
banks** — every PCR reads zero, no event log appears, and the TPM is never
started, which is indistinguishable from a UKI that failed to measure. So the
4 MB build is selected unconditionally, and `--tpm` against a host that only has
the 2 MB one is **refused** rather than run. `vm.sh firmware [--tpm] [--secboot]`
prints the selection, including whether it can measure.

Secure Boot *enforcement* is not a qemu flag — it is whether the variable store
carries keys. `QOS_SECBOOT_KEYS` picks the store:

| Value | Store |
|-------|-------|
| `none` (default) | `OVMF_VARS_4M.qcow2` — no keys, setup mode. Measures into PCR 7, accepts an owner-signed UKI, re-seeded every boot |
| `persist` (`--secboot`) | the same pristine store, kept across boots so an enrolment in the guest sticks |
| `microsoft` | `OVMF_VARS_4M.secboot.qcow2` — Microsoft's KEK/db. **Rejects owner-signed images**, so nothing boots; for testing vendor-key enforcement on purpose |
| *path* | a prepared store |

Enrolling owner keys is a deliberate step, never something the template arrives
with: boot with `--secboot`, run `/usr/libexec/qos-secureboot enroll` in the
guest, reboot the guest.

Use one TPM state dir per scenario (`QOS_TPM_DIR=…`) or a second run unseals
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
   only yields data if the guest has a *persistent* journal (qOS does:
   `/var/log/journal`). Caveat: `qemu-nbd` run inside a **rootless** distrobox
   cannot open the host `/dev/nbd0` (userns permission). If you hit
   `Failed to open /dev/nbd0: Permission denied`, run `qemu-nbd` from a real-root
   context, or fall back to method 2 (route the unit's stderr to the console).

## Gotchas baked into these scripts (bringup notes)

- **Image base = `fedora-bootc:44`.** F42 is EOL. Binaries are built in a
  Fedora 43 toolbox (glibc 2.42) and run on F44 (newer glibc) — forward-compatible.
  Keep the image base ≥ the toolbox's Fedora release. (Our binaries only need
  GLIBC ≤ 2.39 anyway.)
- **Browser (Servo) omitted.** Unmaintained and doesn't build on GCC 15 / C23
  (glsl-optimizer `once_flag` conflict). `--with-browser` re-enables it with
  `CFLAGS=-std=gnu17`, but you must also restore the `COPY browser` lines in the
  Containerfile.
- **AF_UNIX socket path ≤ 108 bytes.** The monitor socket lives at
  `/tmp/qos-vm/mon.sock`; don't relocate `QOS_RUN` somewhere deep.
- **Use the AMD/Intel iGPU for GL, not NVIDIA.** virglrenderer + NVIDIA's EGL is
  unreliable; `detect_rendernode` skips `0x10de`. Override with `QOS_RENDERNODE`.
- **Compositor backend.** `shell` defaults to the **winit** backend (nested —
  needs a parent Wayland/X display) and panics on a bare TTY. The Wayland-session
  `.desktop` entries launch it with **`--backend drm`** (smithay libseat + udev +
  DRM/KMS) so it can be the display server on real hardware / in the VM.

## Installer (self-install)

qOS installs with its own **qOS-branded installer**: a bootable live qcow2 that
runs the graphical installer (`installer/`, binary `qos-installer`) under `cage`
and **self-installs the booted image** onto a target disk — offline, no network
pull. This is the only installer; an early Anaconda ISO path was scaffolding
from before this existed and has been removed.

How it works:

- `installer-disk.toml` bakes the kernel arg **`qos.installer=1`**. That single
  flag drives everything: `qos-session-select` launches the installer (not the
  greeter), the `qos-install` backend refuses to run without it, and
  `qos-installer-setup.service` (which drops in the passwordless sudoers rule)
  only fires on the live medium. The **installed** system has no such arg, so it
  boots straight to the desktop.
- The backend (`/usr/libexec/qos-install`) does: `bootc image copy-to-storage`
  → privileged `podman run … localhost/bootc bootc install to-disk --wipe
  --filesystem btrfs --target-imgref "$QOS_TARGET_IMGREF" <disk>` → mounts the
  fresh root and writes `name:hash` (password via `openssl passwd -6`) into the
  deployment's writable `/etc/qos/install-user.conf`.
- On the installed system's first boot, `qos-firstboot-user.service` creates the
  account from that file (`useradd -G wheel` + `chpasswd -e`) and shreds it.

```bash
# 1. Build + load the desktop image (the installer is FROM localhost/qos).
./os/dev/build-image.sh

# 2. Build the installer qcow2 (-> os/output/installer/qcow2/disk.qcow2).
./os/dev/build-custom-installer.sh
#    …or test the OS plumbing without the real UI:
./os/dev/build-custom-installer.sh --stub-ui

# 3. Boot the installer with a blank target disk attached as /dev/vdb.
./os/dev/vm.sh boot-installer --gl     # GL so the installer UI can render
./os/dev/vm.sh wait                    # block until greetd brings it up
#    (drive the UI: pick /dev/vdb, set user/pass, install)

# 4. Stop, then boot the installed result.
./os/dev/vm.sh stop
./os/dev/vm.sh boot-target --gl        # boots $QOS_RUN/target.qcow2
```

The installed system's update origin defaults to `ghcr.io/nirios/qos:latest`;
override with `QOS_TARGET_IMGREF=…` (preserved across the backend's sudo
re-exec via the live-medium sudoers `env_keep`).

## Configuration (env vars)

`QOS_DISTROBOX` `QOS_IMAGE` `QOS_OUTPUT` `QOS_DISK` `QOS_RUN`
`QOS_RENDERNODE` `QOS_USER` `QOS_PASS` `QOS_MEM` `QOS_SMP` — see `_common.sh`.
Custom installer: `QOS_INSTALLER_IMAGE` `QOS_INSTALLER_OUTPUT`
`QOS_INSTALLER_DISK` `QOS_TARGET_DISK` `QOS_TARGET_SIZE` `QOS_TARGET_IMGREF`.
Disk image: `QOS_DISK_REG_PORT` (registry port), `QOS_RETAINED_GENERATIONS`.
