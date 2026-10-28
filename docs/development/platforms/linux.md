# Linux development

Start with [shared setup](../setup.md) for Rust, Python and scoped checks. A
portable library change does not require installing a desktop, configuring
virtualization or satisfying every dependency below. Build only the affected
Cargo packages; the UI package is `tilo-ui`, not `ui`.

## Native dependencies, by component

Install libraries where compilation runs, including on a remote builder or
inside a build container. A native compiler/linker and `pkg-config` are needed
for packages with C dependencies. The maintained
[build-dependency installer](../../../dist/ci/install-build-deps.sh) records
Fedora, Debian and Arch build-image inputs; it is an installer, not a diagnostic
or a required first-contribution step.

This table maps the relevant native libraries, not a minimum for every crate:

| Library or tool                                                        | Fedora package                                                               | Debian/Ubuntu package                                 | Needed by                             |
| ---------------------------------------------------------------------- | ---------------------------------------------------------------------------- | ----------------------------------------------------- | ------------------------------------- |
| `pkg-config`                                                           | `pkgconf-pkg-config`                                                         | `pkg-config`                                          | Native library discovery              |
| `xkbcommon`                                                            | `libxkbcommon-devel`                                                         | `libxkbcommon-dev`                                    | Linux keyboard handling               |
| `libseat`, `libinput`, `libudev`                                       | `libseat-devel`, `libinput-devel`, `systemd-devel`                           | `libseat-dev`, `libinput-dev`, `libudev-dev`          | Compositor seat/input/device handling |
| `libdrm`, `gbm`, `gl`, `egl`                                           | `libdrm-devel`, `mesa-libgbm-devel`, `mesa-libGL-devel`, `mesa-libEGL-devel` | `libdrm-dev`, `libgbm-dev`, `libgl-dev`, `libegl-dev` | DRM backend and graphics              |
| `wayland-client`                                                       | `wayland-devel`                                                              | `libwayland-dev`                                      | Wayland clients/nested compositor     |
| `libpipewire-0.3`                                                      | `pipewire-devel`                                                             | `libpipewire-0.3-dev`                                 | Screen capture and PipeWire media     |
| `fuse3`                                                                | `fuse3-devel`                                                                | `libfuse3-dev`                                        | Filesystem projection                 |
| `dbus-1`                                                               | `dbus-devel`                                                                 | `libdbus-1-dev`                                       | Native D-Bus consumers                |
| `gstreamer-1.0`                                                        | `gstreamer1-devel`                                                           | `libgstreamer1.0-dev`                                 | GStreamer-enabled media               |
| `gstreamer-app-1.0`, `gstreamer-video-1.0`, `gstreamer-allocators-1.0` | `gstreamer1-plugins-base-devel`                                              | `libgstreamer-plugins-base1.0-dev`                    | Media app/video/allocation APIs       |
| `autoreconf`, `libtoolize`, `libgcrypt.m4`                             | `autoconf`, `automake`, `libtool`, `libgcrypt-devel`                         | `autoconf`, `automake`, `libtool`, `libgcrypt20-dev`  | NTFS helper's pinned native reader    |
| Loadable `libclang`                                                    | `clang-libs`                                                                 | `libclang-dev`                                        | Bindgen, including PipeWire bindings  |

Use `libxkbcommon-devel`, not `xkbcommon-devel`, and `systemd-devel`, not
`libudev-devel`, on Fedora. Tilo's media backend is GStreamer; an FFmpeg
development package is not a substitute. Media features require GStreamer 1.24
or newer, plus the runtime plugins for the codecs being exercised.

For the **full desktop/test environment**, from the repository root:

```sh
tools/ci/check-build-env.sh
```

The [checker source](../../../tools/ci/check-build-env.sh) also lists test
subjects: sandbox tools, a language server, software TPM/LUKS tools, Xwayland,
fonts and media plugins. A missing item is not automatically a dependency of
your selected package. Passing it is dependency discovery, not a test result.
For fonts, use the
[font artifact guide](../../../crates/sdk/ui-core/assets/fonts/ARTIFACTS.md).

## Choose a desktop execution path

- **Desktop in a window:** use the
  [nested-session tool](../../../dist/dev/nested-session.sh). Start with
  `dist/dev/nested-session.sh --help`. It obtains a matching shell, signed
  development apps and runtime from [t](../../t.md), and keeps a private profile
  and session bus. Windowed mode needs a Wayland session on the host, not a
  toolbox/container namespace. It does not test DRM seat ownership or
  login-session locking. See [headless testing](../../headless-testing.md) for
  offscreen work.
- **Tilo as the login desktop:** this is Linux-only. The packaged session
  wrapper, display-manager entry and PAM helper are part of the
  [host session contract](../../../dist/session/README.md). A bare nested shell
  is not that session. Test lock/unlock, input, outputs and returning to the
  previous desktop through an actual login cycle, preferably in a disposable
  environment. Do not copy a compositor into `/usr` by hand.
- **Complete OS or device integration:** use `tilo device guide quickstart` and
  `tilo device guide platforms` (also available as `tilo-vm guide ...`). The
  [canonical platform guide](../../../crates/dev/tilo-vm/guide/platforms.md)
  covers libvirt's rootless `qemu:///session`, KVM checks and host
  prerequisites. Keep VM lifecycle, swaps and rollback under that tool rather
  than raw SSH deployment. A nested or headless check does not qualify physical
  hardware.

## WSL checkouts

For Linux work in WSL, keep an independent checkout on the Linux filesystem, for
example under `$HOME/src`, rather than on `/mnt/c`. Restored timestamp precision
on Windows-mounted filesystems can invalidate Cargo fingerprints and cause
unnecessary recompilation.

Keep native Windows work in its Windows checkout. Do not move an active
checkout, copy another checkout's `target/`, share target directories or set
`CARGO_TARGET_DIR`. Transfer intended source changes explicitly; an independent
clone does not contain another checkout's uncommitted edits. WSL compilation
alone is not proof of a Linux login session, GPU behavior or hardware support.
