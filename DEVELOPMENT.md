# Development Guide

Machine and environment notes: getting a working build on a host, and running the
result. What qOS *is* and how to work in the code is in `CLAUDE.md`.

## What the build needs

Rust (rustup reads `rust-toolchain.toml` automatically) and a set of native
libraries. How the libraries get there is out of scope — container, nix shell, or
installed on the host all work.

Rust reports an absent one as `pkg-config exited with status code 1` inside a
`-sys` crate without naming it, so:

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
| `gstreamer-1.0` `gstreamer-app-1.0` | `gstreamer1-devel` `gstreamer1-plugins-base-devel` | media apps |
| `openssl` | `openssl-devel` | |
| `capnp` (binary) | `capnproto` | runtime-collab schema |
| `bwrap` (binary) | `bubblewrap` | the launcher's confinement tests drive the real sandbox |
| `rust-analyzer` (binary) | `rustup component add rust-analyzer` | the code editor's language-server test drives a real server |

The last two are test *subjects*: those suites fail rather than skip without
them, which is deliberate. They belong wherever `cargo` runs — inside the
container if you build in one, since that is the `/usr` the sandbox binds.

### If you build in a container (optional)

For image-based hosts (Silverblue, Bazzite) where `/usr` is read-only and the
libraries cannot be installed without `rpm-ostree` layering plus a reboot per base
update. Put the container name in `.build-container` and install `tools/cargo-shim`
as `cargo` on PATH; every invocation from the tree or a worktree routes into it.

It also selects the sccache server, which this route requires: sccache's server
executes rustc, so it must share the build's mount namespace or it cannot see that
namespace's headers. distrobox shares the host network namespace and copies the host
environment inward, so without an explicit choice on both sides the container build
reaches the host's server and every compilation misses.

Not needed if the libraries are present natively — a nix/devenv shell removes both
the routing and the sccache-namespace problem, and the shim with them.

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

Measured on `ui`'s test binary, so nobody spends the day rediscovering it:

| change | effect |
|---|---|
| mold instead of GNU ld | none (40 s vs 41 s, and 61 s vs 61 s) |
| `-C split-debuginfo=unpacked` | binary 697 -> 507 MB, build 14% **slower** |
| `-C debuginfo=0` | binary 665 -> 475 MB, build unchanged |
| nightly `--jobs-frontend=4` / `=8` | +13% / 0% |
| nightly cranelift backend | 23% slower |

Linking is not the bottleneck and the multi-hundred-MB binaries are not either.
Compilation dominates: `-Z time-passes` puts type checking at 14% of a debug
build and LLVM at 29%, the rest rustc lowering MIR to IR.

Run-to-run noise on a busy machine is ±20%, which is larger than most of the
effects above. Measure on an idle machine or not at all.

What *is* real for concurrent work, where several worktrees build the same
tree at once: sccache is the only sharing there is, since target directories
cannot be shared. Measured, a fresh worktree building `ui` takes 126 s at 77%
hits against ~562 s cold.

That is also the thing easiest to destroy. Every change to `build.rustflags`
invalidates the whole cache for every worktree simultaneously — measured at
1.75% hits during a day of flag changes, recovering to 33% once they stopped.
Batch flag changes, and do not make one while a long verification run is in
flight.

### Sizing the compilation cache

116 members over ~1,480 external crates. One full debug+test build is ~2,200
cacheable units, a few GB compressed (measured: 105,000 objects in 100 GB, mean
0.97 MB, median 0.22 MB).

Two halves. Dependencies never change unless `Cargo.lock` does and should stay
resident. Workspace members turn over whenever a shared crate is touched —
`ui` invalidates the compositor and every app behind it — at roughly 3–5 GB per
full set, so 200 GB keeps about forty branch-versions hot.

Prefer cache over target directories when trading disk: a cached object serves
every worktree and survives `cargo clean`, a target directory serves one and
does not.

The compiler is part of the cache key, so bumping `rust-toolchain.toml` costs one
fully cold rebuild.

## Disk, and why builds hang

One worktree's `target/` reaches 150+ GB. Delete `target/` from worktrees whose
branches have landed — the sccache keeps the compilations, so the cost is one
warmer-than-cold rebuild rather than a cold one.

If builds *hang* rather than run slowly — uninterruptible sleep, no progress at
all — that is btrfs metadata exhaustion, and deleting files alone will not fix
it: freed space returns inside existing chunks, so `df` moves while
`unallocated` stays near zero and metadata cannot grow. Free something first,
**then**:

```bash
sudo btrfs balance start -dusage=80 -dlimit=25 /var/home
```

Use 80, not 30. Data chunks here average ~96% full, so `-dusage=30` relocates
nothing and exits 0 — which reads as the remedy failing rather than not
applying.

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

VM login `tester` / `quickdesktop`. Check the FUSE mount with
`mountpoint /var/workspace`; the test image's results are in
`journalctl -u quick-fs-test`.

## Running qOS on your own machine (Silverblue / Bazzite)

You do not have to boot the qOS image to run the qOS shell. `dist/` packages the
compositor as a Wayland session you can pick at the login screen of the
Silverblue-family host you already use, and adds a second, developer-only entry
that starts *the build you just made* instead of the installed one.

Two loops come out of that, and they are for different things:

- **Nested** — the compositor runs as a window inside your current session.
  Seconds per iteration, no logging out, but it is a client of your real
  compositor: no DRM, no seat, no session lock worth testing.
- **Real session** — the compositor drives KMS on its own VT, as a login
  session. That is where the lock screen, input, and multi-output paths are
  actually exercised, and it costs a log-out per iteration.

### Install once

```bash
dist/rpm/build-rpm.sh
```

Builds `shell`, `qos-launcher` and `qos-unlock-helper` in release, stages them
with the session entries and PAM stack, and runs `rpmbuild` (in the build
container, since an image-based host has no `rpm-build`). Two packages land in
`dist/rpm/build/RPMS/x86_64/`:

```console
$ rpm -qlp dist/rpm/build/RPMS/x86_64/qos-session-0.1.0-1.fc43.x86_64.rpm
/etc/pam.d/qos-unlock
/usr/bin/qos-launcher
/usr/bin/qos-shell
/usr/libexec/qos-session
/usr/libexec/qos-unlock-helper
/usr/share/wayland-sessions/qos.desktop
/usr/share/xdg-desktop-portal/portals/qos.portal

$ rpm -qlp dist/rpm/build/RPMS/x86_64/qos-session-devel-0.1.0-1.fc43.x86_64.rpm
/usr/libexec/qos-session-dev
/usr/share/wayland-sessions/qos-dev.desktop
```

`/usr/share/wayland-sessions/` is the point of the exercise: GDM scans that
directory and nothing else — not `/usr/local/share`, not `~/.local/share` — and
on an ostree host it is inside the read-only composefs, so a session entry can
only get there as part of `/usr`.

Then install, either way:

```bash
# Persistent, and the one you want long term. Needs a reboot to take effect.
sudo rpm-ostree install ./dist/rpm/build/RPMS/x86_64/qos-session-*.rpm

# Or: this boot only, no reboot. Mounts a writable overlay on /usr and unpacks
# the same files into it. Everything under /usr is gone at the next boot.
sudo dist/rpm/usroverlay-install.sh
sudo dist/rpm/usroverlay-install.sh --uninstall
```

Either way, log **out** (not lock) afterwards: the greeter reads
`/usr/share/wayland-sessions` when it starts, so a new entry appears at the next
greeter, and "qOS" shows up in the session menu next to your usual desktop.

Skip `qos-session-devel` on any machine you are not developing qOS on — see
"Edit → build → log into your own build" below for what it can do.

### Edit → build → run nested

The compositor prints its socket name as an assignment you can paste, and
writes it to a fixed path, so nothing has to be read out of a log:

```console
$ cargo build -p shell && ./target/debug/shell --backend winit
nirios compositor listening on "wayland-1"
  WAYLAND_DISPLAY=wayland-1 — e.g. `WAYLAND_DISPLAY=wayland-1 cargo run -p terminal`
  also written to /run/user/1000/qos-session/wayland-display
```

So pointing anything at it is one step:

```bash
cargo build -p terminal
WAYLAND_DISPLAY=$(cat "$XDG_RUNTIME_DIR/qos-session/wayland-display") ./target/debug/terminal
```

The file is last-writer-wins — start a second compositor and it names the
second one. The printed line stays authoritative for a particular process.

qOS nests inside qOS, which is the quickest way to check that a build is a
working Wayland server and not just a window that painted:

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

Once, after installing `qos-session-devel`:

```bash
cargo build -p shell --features dev-session
echo "$PWD/target/debug/shell" > ~/.config/qos/dev-session
chmod 600 ~/.config/qos/dev-session
```

Then log out and pick **"qOS (my build)"**. Every iteration after that is
`cargo build -p shell --features dev-session`, log out, log back in — the config
file keeps pointing at the same path.

`--features dev-session` is not optional and not a formality. It is the only
thing that stamps a marker into the binary, and `/usr/libexec/qos-session-dev`
refuses to start anything that lacks one; a `--release` build never carries it
whatever features it is given. Before it execs, the entry also requires that the
config file, the target binary, and every directory above each of them are owned
by you or root and writable by nobody else. All of that is fatal, never a
fallback to the installed compositor: silently testing the wrong binary is worse
than failing to start. `dist/session/tests/dev-session-refusals.sh` drives each
refusal.

None of this exists in the production path. `/usr/libexec/qos-session` hard-codes
`/usr/bin/qos-shell` and reads no config, and the developer entry ships in a
subpackage precisely so "this machine can start a login session from a binary in
someone's home directory" is a visible `rpm -q` fact rather than a property every
installation has.

### What a stock host does not give you

The compositor starts on a plain Bazzite host with none of the qOS system
furniture present — no `/var/workspace`, `/etc/qos`, `/var/lib/qos` or
`/usr/share/qos-apps` — and says so rather than failing. Two warnings are worth
knowing about:

- **No `ush` runtime**, so the terminal falls back to `$SHELL` unconfined. That
  works interactively and fails inside an app sandbox.
- **No first-boot unit.** The image runs `shell --firstboot` from a systemd
  unit before the session; a host has none, so without help nothing ever
  creates `~/.config/qos/identity/primary.json` and every session logs *"no qOS
  identity found"*. Both session wrappers therefore pass `--firstboot`
  themselves — it does the setup and falls through into the compositor, and
  provisioning returns early once the record exists. Start the compositor by
  hand (`./target/debug/shell`) and you get the warning, because you skipped
  the wrapper.
- **`shell/src/config.rs`'s `load_config` is dead code** — nothing calls it, and
  it looks for `/etc/quick-desktop/desktop.toml` while the image ships
  `/etc/qos/desktop.toml`. So `desktop.toml` configures nothing today, on a host
  or on the image. The host package does not ship one rather than pretend
  otherwise.

`/var/workspace` is a FUSE mount `fs.service` makes on the qOS image; there is
no such unit here, so the permission UI will talk about a path that is not
mounted.

The app list is *not* empty, though: alongside qOS packages the shell scans the
ordinary XDG entry directories — `$XDG_DATA_HOME/applications`, each
`$XDG_DATA_DIRS` entry's `applications`, and both Flatpak export directories
(`crates/system/desktop-entry/src/scan.rs`). On this host that is 108 system
entries plus 97 Flatpak ones, so host-installed apps and Flatpaks show up
without a qOS registry existing at all.

The one that used to be a trap is fixed by installing the package: the
compositor spawns `/usr/libexec/qos-unlock-helper` to verify an unlock, and with
no helper at that path `verifier_from_env` falls closed — you could lock the
session and never get back in. The package installs the helper at exactly that
path together with `/etc/pam.d/qos-unlock`, and the helper reaches PAM on a
stock host:

```console
$ printf 'not-the-password\n' | /usr/libexec/qos-unlock-helper "$USER"; echo $?
1     # 1 = PAM ran and denied it. 2 would mean the check could not run at all.
```

Do not try to paper over a missing helper with `QOS_UNLOCK_HELPER`. It is a
developer-build variable now: a release binary does not read it, along with
`QOS_LOCK_TEST_PASSWORD`, `QOS_TEST_CONTROL`, `NIRIOS_TEST_STATE_DUMP` and
`QOS_LAUNCHER_BIN`. On a systemd desktop the session environment comes from
`~/.config/environment.d/`, so an installed binary that honoured those would let
anything able to write a file in your home decide what unlocks your screen. See
`shell/src/compositor/dev_env.rs`.


## Headless testing

`CLAUDE.md` covers the entry points. The full guide is
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
under `apps/` or `crates/` is missing from `[workspace].members` — that once let
37 apps drop out of the build. Compilation and tests run in CI
(`.forgejo/workflows/check.yml`), not the hook, so commits stay fast.
