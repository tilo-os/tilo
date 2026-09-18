# Development Guide

Build dependencies, local development and testing. See `AGENTS.md` for repository
conventions.

## What the build needs

Install Rust through rustup and the native dependencies below, either on the host
or in your build container or Nix shell. Rustup reads `rust-toolchain.toml`.
Check dependencies with:

```bash
tools/check-build-env.sh
```

Modules, and the Fedora packages providing them. Note `xkbcommon-devel`,
`libudev-devel` and `ffmpeg-devel` do not exist:

| pkg-config | Fedora package | for |
|---|---|---|
| `xkbcommon` | `libxkbcommon-devel` | every build |
| `libseat` `libinput` `libudev` | `libseat-devel` `libinput-devel` `systemd-devel` | compositor DRM backend, input, device enumeration |
| `libdrm` `gbm` `gl` `egl` | `libdrm-devel` `mesa-libgbm-devel` `mesa-libGL-devel` `mesa-libEGL-devel` | compositor DRM and paint |
| `wayland-client` | `wayland-devel` | |
| `libpipewire-0.3` | `pipewire-devel` | portal ScreenCast |
| `fuse3` | `fuse3-devel` | `fs` |
| `dbus-1` | `dbus-devel` | portals |
| `libavcodec` `libavformat` `libavutil` | `libavcodec-free-devel` `libavformat-free-devel` | shell's default hdr-decode feature |
| `libswscale` | `libswscale-free-devel` | FFmpeg software scaling used by the shell |
| `gstreamer-1.0` `gstreamer-app-1.0` | `gstreamer1-devel` `gstreamer1-plugins-base-devel` | media apps |
| `capnp` (binary) | `capnproto` | runtime-collab schema |
| `libclang` (shared library) | `clang-libs` | bindgen for PipeWire and FFmpeg bindings; Python 3 checks that it can load |
| `bwrap` (binary) | `bubblewrap` | the launcher's confinement tests drive the real sandbox |
| `rust-analyzer` (binary) | `rustup component add rust-analyzer` | the code editor's language-server test drives a real server |

The sandbox and language-server tests require `bwrap` and `rust-analyzer`.
Install them where Cargo runs, including inside a build container.

### Native Windows Imager builds

Install the pinned Rust toolchain, a Windows SDK, and the Visual Studio C++
tools for the target architecture. Windows ARM64 needs the Build Tools
component `Microsoft.VisualStudio.Component.VC.Tools.ARM64`; installing only
the x64 C++ tools does not provide ARM64 target libraries. Put a working
`capnp` schema compiler on PATH. For a fully native ARM64 toolchain, build it
from the [Cap'n Proto source](https://capnproto.org/install.html) with CMake's
Visual Studio generator and `-A ARM64`.

From an ARM64 host, run:

```powershell
cargo test -p imager --lib --features host-media-writer --target aarch64-pc-windows-msvc --locked
cargo test -p ui --test hosted-platform --target aarch64-pc-windows-msvc --locked
```

Use `x86_64-pc-windows-msvc` on an x64 host. Keep the target explicit: an
emulated shell or tool must not determine the release architecture. These
tests cover the hosted client; the compositor and local identity authority
remain Linux/Unix services. Signed installer prerequisites and both native
release lanes are documented in [Imager release operations](docs/imager-release-operations.md).

### Video editor on macOS

Install the native media dependencies with Homebrew, then launch from the
workspace root:

```bash
brew install pkgconf gstreamer
cargo run -p video_editor
```

The editor enables GStreamer playback and export by default. Homebrew's
`gstreamer` formula supplies the plugins and GLib dependency as well. Check
that Cargo can discover the libraries with:

```bash
pkg-config --modversion glib-2.0 gstreamer-1.0 gstreamer-app-1.0
```

### If you build in a container (optional)

For image-based hosts (Silverblue, Bazzite) where `/usr` is read-only and the
libraries cannot be installed without `rpm-ostree` layering plus a reboot per base
update. Create a local `.build-container` containing your distrobox name (for
example, `printf '%s\n' tilo-build > .build-container`). This file is ignored by
Git; there is no shared machine-specific default. Install `tools/cargo-shim`
as `cargo` on PATH; every invocation from the tree or a worktree routes into it.

It also selects the sccache server, which this route requires: sccache's server
executes rustc, so it must share the build's mount namespace or it cannot see that
namespace's headers. distrobox shares the host network namespace and copies the host
environment inward, so without an explicit choice on both sides the container build
reaches the host's server and every compilation misses.

Not needed if the libraries are present natively — a nix/devenv shell removes both
the routing and the sccache-namespace problem, and the shim with them.

### Fedora 44 disposable-VM loop

For desktop development, build the selected debug executable in a Fedora 44
toolbox and copy it to a disposable VM:

```bash
# One time.  This builds a dependency image and creates a persistent distrobox.
dist/dev/fedora44-vm-iterate.sh bootstrap

# Usually the entire edit/build step.  This is debug, with no release fat LTO.
dist/dev/fedora44-vm-iterate.sh build shell

# The VM's loopback SSH forward is intentionally required, never guessed.
TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh deploy shell
dist/dev/fedora44-vm-iterate.sh handoff
# After the normal VM UI logout/login:
TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh verify

# The three commands above, after the first container setup.
TILO_VM_SSH_PORT=2230 dist/dev/fedora44-vm-iterate.sh cycle shell
```

Use SSH keys normally (set `TILO_VM_SSH_IDENTITY` when the key is not in the
SSH agent), or provide `TILO_VM_PASSWORD` only in the invoking environment.
The script checks `systemd-detect-virt` before copying files. After `handoff`
or `cycle`, log out and back in through the VM's UI to test session cleanup.
`verify` compares the deployed debug shell with the running guest process's
`/proc/<pid>/exe`.

The VM needs the `tilo-session-devel` developer entry once, and **"Tilo (my
build)"** must be selected in its greeter once.  `deploy shell` writes the
marked debug binary to `~/.local/share/tilo-dev/shell` and updates the existing
developer-session config, so later cycles do not install an RPM.  Change the
remote account/location with `TILO_VM_USER`, `TILO_VM_HOST`, and
`TILO_VM_DEV_ROOT`; `TILO_VM_SSH_PORT` is always mandatory. Build an app or
the two canonical host executables on demand with, for example,
`build calculator`, `build tilo-launcher`, `build tilo-unlock-helper`, or
`build host`, rather than rebuilding the application collection.

The shell command builds `--bin shell --features dev-session` so Cargo does not
also compile screenshot tooling during compositor iteration. It retains the
normal shell features inside Fedora 44, matching the actual session rather than
silently dropping media behavior to work around a host ABI mismatch.

The loop also gives this checkout a dedicated sccache server and namespace.
Distrobox copies host environment variables inward; sharing either its generic
cache or the host cache can replay a path-sensitive Rust build script (such as
the vendored shader compiler, which embeds `CARGO_MANIFEST_DIR`) into the host
target directory. This is not a valid host or Fedora 44 artifact. The cache is
persistent for warm iterations; set `TILO_FEDORA44_SCCACHE_PORT` only if two
active checkouts collide on its derived port.

### Hardware-dependent settings

Two settings are derived from the hardware rather than committed:

- **Job count** — bounded by memory, not cores: rustc peaks around 7 GB on the
  larger crates. The shim exports `CARGO_BUILD_JOBS` as `min(cores, RAM ÷ 7 GB)`.
  `jobs = 4` in `.cargo/config.toml` is only a floor for builds that bypass the
  shim, such as CI.
- **sccache ceiling** — 20% of the filesystem, floor 64 GB, cap 200 GB.

Override either with `CARGO_BUILD_JOBS` / `SCCACHE_CACHE_SIZE`. Neither is part
of sccache's cache key, so varying them does not fragment it.

`CARGO_TARGET_DIR` **is** in the cache key: measured, a clean rebuild of one
crate hits with it unset and misses with it set to the identical path. Never set
it, and never point two worktrees at one target directory — cargo's artifact key
omits the package path, so worktrees of one repo collide and cargo reports
`Fresh` while handing you the other's binary.

### What does not make builds faster here

Measurements from `ui`'s test binary:

| change | effect |
|---|---|
| mold instead of GNU ld | none (40 s vs 41 s, and 61 s vs 61 s) |
| `-C split-debuginfo=unpacked` | binary 697 -> 507 MB, build 14% **slower** |
| `-C debuginfo=0` | binary 665 -> 475 MB, build unchanged |
| nightly `--jobs-frontend=4` / `=8` | +13% / 0% |
| nightly cranelift backend | 23% slower |

Compilation dominates: `-Z time-passes` puts type checking at 14% of a debug
build and LLVM at 29%, the rest rustc lowering MIR to IR.

Run-to-run noise on a busy machine is ±20%, which is larger than most of the
effects above. Measure on an idle machine.

Use sccache to share compilation results across worktrees; target directories
cannot be shared. A measured `ui` build in a fresh worktree took 126 s at 77%
cache hits, compared with ~562 s cold.

Changing `build.rustflags` invalidates cached compilations across worktrees.
Batch flag changes and avoid them during verification runs.

### Sizing the compilation cache

116 members over ~1,480 external crates. One full debug+test build is ~2,200
cacheable units, a few GB compressed (measured: 105,000 objects in 100 GB, mean
0.97 MB, median 0.22 MB).

Cache dependencies between lockfile changes. Changes to a shared crate such as
`ui` rebuild the compositor and dependent apps. At roughly 3–5 GB per full set,
200 GB holds about forty versions.

Prefer cache over target directories when trading disk: a cached object serves
every worktree and survives `cargo clean`, a target directory serves one and
does not.

The compiler is part of the cache key, so bumping `rust-toolchain.toml` costs one
fully cold rebuild.

## Disk, and why builds hang

One worktree's `target/` reaches 150+ GB. Delete `target/` from worktrees whose
branches have landed; sccache retains reusable compilation results.

If builds *hang* rather than run slowly — uninterruptible sleep, no progress at
all — that is btrfs metadata exhaustion, and deleting files alone will not fix
it: freed space returns inside existing chunks, so `df` moves while
`unallocated` stays near zero and metadata cannot grow. Free something first,
**then**:

```bash
sudo btrfs balance start -dusage=80 -dlimit=25 /var/home
```

In this environment, `-dusage=30` relocated no chunks despite exiting
successfully; `-dusage=80` was needed.

`/tmp` is a 16 GB tmpfs. Concurrent rustc processes fill it, and the resulting
failures do not name the cause: bogus "Quota exceeded" messages, exit 1 with no
output, from any command. Point `TMPDIR` at real disk for large parallel builds.

## Building and testing

```bash
cargo build -p shell            # or ui, fs, terminal, file-explorer, browser
cargo test -p fs
cargo test -p shell --lib room_activity      # scope it; the compile dominates
cargo check -p shell --tests                 # before finishing
```

Full OS image, which needs rootless `podman` for the container step and `sudo`
for the disk-image step:

```bash
./os/build.sh
./os/test-vm.sh                              # needs OVMF + KVM
./os/verify-test.sh                          # test image with a validation unit
./os/test-vm.sh os/output-test/qcow2/disk.qcow2
```

VM login `tester` / `tilo`. Check the FUSE mount with
`mountpoint /var/workspace`; the test image's results are in
`journalctl -u fs-test`.

## Running Tilo on your own machine (Silverblue / Bazzite)

`dist/` packages the compositor as a Wayland login session for Silverblue-family
hosts. An optional developer entry starts a local build.

- **Nested** — the compositor runs as a window inside your current session.
  No logout is needed. DRM, seat access and session locking are unavailable.
- **Real session** — the compositor drives KMS on its own VT, as a login
  session. Use this to test screen locking, input and multiple outputs.
  Log out and back in after each build.

### Install once

```bash
dist/rpm/build-rpm.sh
```

Builds `shell`, `tilo-launcher` and `tilo-unlock-helper` in release, stages them
with the session entries and PAM stack, and runs `rpmbuild` (in the build
container, since an image-based host has no `rpm-build`). Two packages land in
`dist/rpm/build/RPMS/x86_64/`:

```console
$ rpm -qlp dist/rpm/build/RPMS/x86_64/tilo-session-0.1.0-1.fc43.x86_64.rpm
/etc/pam.d/tilo-unlock
/usr/bin/tilo-launcher
/usr/bin/tilo-shell
/usr/libexec/tilo-session
/usr/libexec/tilo-unlock-helper
/usr/share/wayland-sessions/tilo.desktop
/usr/share/xdg-desktop-portal/portals/tilo.portal

$ rpm -qlp dist/rpm/build/RPMS/x86_64/tilo-session-devel-0.1.0-1.fc43.x86_64.rpm
/usr/libexec/tilo-session-dev
/usr/share/wayland-sessions/tilo-dev.desktop
```

GDM reads session entries from `/usr/share/wayland-sessions/`. On an ostree host,
this directory is read-only, so install the entries through the package.

Then install, either way:

```bash
# Persistent, and the one you want long term. Needs a reboot to take effect.
sudo rpm-ostree install ./dist/rpm/build/RPMS/x86_64/tilo-session-[0-9]*.rpm

# Or: this boot only, no reboot. Mounts a writable overlay on /usr and unpacks
# the same files into it. Everything under /usr is gone at the next boot.
sudo dist/rpm/usroverlay-install.sh
# Add `--devel` only when you explicitly need the developer session entry.
sudo dist/rpm/usroverlay-install.sh --devel
sudo dist/rpm/usroverlay-install.sh --uninstall
```

Either way, log **out** (not lock) afterwards: the greeter reads
`/usr/share/wayland-sessions` when it starts, so a new entry appears at the next
greeter, and "Tilo" shows up in the session menu next to your usual desktop.

Skip `tilo-session-devel` on any machine you are not developing Tilo on — see
"Edit → build → log into your own build" below for what it can do.

### Edit → build → run nested

The compositor prints its socket name and writes it to
`$XDG_RUNTIME_DIR/tilo-session/wayland-display`:

```console
$ cargo build -p shell && ./target/debug/shell --backend winit
tilo compositor listening on "wayland-1"
  WAYLAND_DISPLAY=wayland-1 — e.g. `WAYLAND_DISPLAY=wayland-1 cargo run -p terminal`
  also written to /run/user/1000/tilo-session/wayland-display
```

Run an app on that socket:

```bash
cargo build -p terminal
WAYLAND_DISPLAY=$(cat "$XDG_RUNTIME_DIR/tilo-session/wayland-display") ./target/debug/terminal
```

The file is last-writer-wins — start a second compositor and it names the
second one. The printed line stays authoritative for a particular process.

To test a nested Tilo compositor:

```bash
./target/debug/shell --backend winit &                     # outer → wayland-1
WAYLAND_DISPLAY=wayland-1 ./target/debug/shell --backend winit &   # inner → wayland-2
```

Measured here on a debug build: ~7 s from exec to the inner compositor serving
its own socket, and both stayed interactive afterwards. A debug build is heavy —
it is `vello` + `wgpu` unoptimised — so expect the nested one to feel soft.
`--backend drm` is not an option from inside a running session: `libseat` will
not hand you a seat the running desktop owns, and fails with `ENOSYS` (error
code 38), which is a refusal, not an unimplemented backend.

### Edit → build → log into your own build

Once, after installing `tilo-session-devel`:

```bash
cargo build -p shell --features dev-session
echo "$PWD/target/debug/shell" > ~/.config/tilo/dev-session
chmod 600 ~/.config/tilo/dev-session
```

Then log out and pick **"Tilo (my build)"**. Every iteration after that is
`cargo build -p shell --features dev-session`, log out, log back in — the config
file keeps pointing at the same path.

`--features dev-session` is not optional and not a formality. It is the only
thing that stamps a marker into the binary, and `/usr/libexec/tilo-session-dev`
refuses to start anything that lacks one; a `--release` build never carries it
whatever features it is given. Before it execs, the entry also requires that the
config file, the target binary, and every directory above each of them are owned
by you or root and writable by nobody else. All of that is fatal, never a
fallback to the installed compositor: silently testing the wrong binary is worse
than failing to start. `dist/session/tests/dev-session-refusals.sh` drives each
refusal.

None of this exists in the production path. `/usr/libexec/tilo-session` hard-codes
`/usr/bin/tilo-shell` and reads no config, and the developer entry ships in a
subpackage precisely so "this machine can start a login session from a binary in
someone's home directory" is a visible `rpm -q` fact rather than a property every
installation has.

### What a stock host does not give you

The compositor starts on a plain Bazzite host with none of the Tilo system
furniture present — no `/var/workspace`, `/etc/tilo`, `/var/lib/tilo` or
`/usr/share/tilo-apps` — and says so rather than failing. Two warnings are worth
knowing about:

- **No `ush` runtime**, so the terminal falls back to `$SHELL` unconfined. That
  works interactively and fails inside an app sandbox.
- **No first-boot unit.** The image runs `shell --firstboot` from a systemd
  unit before the session; a host has none, so without help nothing ever
  creates `~/.config/tilo/identity/primary.json` and every session logs *"no Tilo
  identity found"*. Both session wrappers therefore pass `--firstboot`
  themselves — it does the setup and falls through into the compositor, and
  provisioning returns early once the record exists. Start the compositor by
  hand (`./target/debug/shell`) and you get the warning, because you skipped
  the wrapper.
- **`shell/src/config.rs`'s `load_config` is dead code** — nothing calls it, and
  it looks for `/etc/tilo-desktop/desktop.toml` while the image ships
  `/etc/tilo/desktop.toml`. So `desktop.toml` configures nothing today, on a host
  or on the image. The host package does not ship one rather than pretend
  otherwise.

`/var/workspace` is a FUSE mount `fs.service` makes on the Tilo image; there is
no such unit here, so the permission UI will talk about a path that is not
mounted.

The app list is *not* empty, though: alongside Tilo packages the shell scans the
ordinary XDG entry directories — `$XDG_DATA_HOME/applications`, each
`$XDG_DATA_DIRS` entry's `applications`, and both Flatpak export directories
(`crates/system/desktop-entry/src/scan.rs`). On this host that is 108 system
entries plus 97 Flatpak ones, so host-installed apps and Flatpaks show up
without a Tilo registry existing at all.

The one that used to be a trap is fixed by installing the package: the
compositor spawns `/usr/libexec/tilo-unlock-helper` to verify an unlock, and with
no helper at that path `verifier_from_env` falls closed — you could lock the
session and never get back in. The package installs the helper at exactly that
path together with `/etc/pam.d/tilo-unlock`, and the helper reaches PAM on a
stock host:

```console
$ printf 'not-the-password\n' | /usr/libexec/tilo-unlock-helper "$USER"; echo $?
1     # 1 = PAM ran and denied it. 2 would mean the check could not run at all.
```

Do not try to paper over a missing helper with `TILO_UNLOCK_HELPER`. It is a
developer-build variable now: a release binary does not read it, along with
`TILO_LOCK_TEST_PASSWORD`, `TILO_TEST_CONTROL`, `TILO_TEST_STATE_DUMP` and
`TILO_LAUNCHER_BIN`. On a systemd desktop the session environment comes from
`~/.config/environment.d/`, so an installed binary that honoured those would let
anything able to write a file in your home decide what unlocks your screen. See
`shell/src/compositor/dev_env.rs`.


## Headless testing

`AGENTS.md` covers the entry points. The full guide is
[`docs/headless-testing.md`](docs/headless-testing.md), and
[`scripts/headless-examples/`](scripts/headless-examples/) holds working scripts —
`spawn-headless.sh` starts a compositor and prints `READY <socket>`,
`full-loop.py` goes from nothing to a PNG in about five seconds, and
`_control.py` is the socket client to inline rather than rewrite.

`--backend headless` is what makes the live compositor runnable in CI, over SSH
and inside a container with no display forwarded.

## Pre-commit hook

Enable the repo's git hooks once per clone:

```bash
git config core.hooksPath .githooks
```

`pre-commit` runs `scripts/check-workspace-complete.sh`, which fails if any crate
under `apps/` or `crates/` is missing from `[workspace].members`. Compilation and
tests run in CI (`.forgejo/workflows/check.yml`).
