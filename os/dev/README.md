# qOS headless VM dev toolkit (`os/dev/`)

Build the qOS bootc image and boot it in a headless QEMU VM to inspect bootup,
capture screenshots, drive the greeter, and pull logs — no physical display, no
GUI. Built for the reference dev box (immutable Fedora/Bazzite host, build deps
in a distrobox), but configurable for any host via env vars.

```
os/dev/
  _common.sh       shared config + helpers (override via env)
  build-image.sh   cargo build -> podman build -> bootc-image-builder -> qcow2
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

Host needs: rootless `podman`, **passwordless `sudo`** (for bootc-image-builder
and offline journal extraction), `/dev/kvm`, and a GPU render node under
`/dev/dri` (a Mesa iGPU is strongly preferred for GL — see gotchas).

If your host has qemu + tools natively, set `QOS_DISTROBOX=""` to skip the box.

## Quick start

```bash
# 1. Build the image (≈ a few min incremental; first cargo build is long).
./os/dev/build-image.sh                 # 4 core binaries, browser omitted

# 2. Boot + greeter + login + desktop, screenshots into a dir:
./os/dev/vm.sh capture-boot ./vm-artifacts

# …or drive it step by step:
./os/dev/vm.sh boot --gl                # GL so the DRM compositor can render
./os/dev/vm.sh wait                     # block until greetd is up
./os/dev/vm.sh login                    # types quickuser / quickdesktop
./os/dev/vm.sh shot desktop             # -> /tmp/qos-vm/desktop.png
./os/dev/vm.sh serial 'greetd|shell|panic'   # grep the boot console
./os/dev/vm.sh stop
```

Login: **`quickuser` / `quickdesktop`** (from `os/config.toml`).

## Display modes — the key gotcha

QEMU's `screendump` can only read the surface its display backend exposes. There
is **no single headless mode that captures both the 2D greeter and the GL
compositor**, so pick per task:

| Mode | qemu flags | `screendump` captures | Compositor (`shell`) |
|------|-----------|----------------------|----------------------|
| `--2d` (default) | `-device virtio-vga -display none` | GRUB, kernel console, **tuigreet greeter** | software GL only (may not run) |
| `--gl` | `-device virtio-vga-gl -display egl-headless,rendernode=<iGPU>` | **GL compositor scanout** (2D content → "no surface") | hardware GL via virgl |

So: screenshot the **greeter in `--2d`**, screenshot the **desktop in `--gl`**.
`vm.sh serial` works in both (the image boots with `console=ttyS0`).

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

## Custom installer (self-install)

Besides the Anaconda ISO (`build-installer.sh` / `installer.toml`), there is a
**custom, qOS-branded installer**: a bootable live qcow2 that runs the graphical
installer (`installer/`, binary `qos-installer`) under `cage` and **self-installs
the booted image** onto a target disk — offline, no network pull.

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

`QOS_DISTROBOX` `QOS_IMAGE` `QOS_OUTPUT` `QOS_DISK` `QOS_BIB` `QOS_RUN`
`QOS_RENDERNODE` `QOS_USER` `QOS_PASS` `QOS_MEM` `QOS_SMP` — see `_common.sh`.
Custom installer: `QOS_INSTALLER_IMAGE` `QOS_INSTALLER_OUTPUT`
`QOS_INSTALLER_DISK` `QOS_TARGET_DISK` `QOS_TARGET_SIZE` `QOS_TARGET_IMGREF`.
