# Development Guide

Welcome to the qOS project! This document outlines how to set up your environment and work with the codebase.

## 1. Project Structure

qOS is organized as a single Cargo Workspace (monorepo).

-   `shell/`: The Wayland compositor (`shell`).
-   `fs/`: The collaborative P2P filesystem.
-   `os/`: Build scripts and configurations for the Fedora Bootc OS.
-   `apps/`:
    -   `browser/`: The Servo-based browser (`browser`).
    -   `file-explorer/`: Spatial file manager.
    -   `terminal/`: GPU-accelerated terminal.

## 2. Working in the monorepo

All components (`shell`, `fs`, `os`, `ui`, `apps/*`, and the top-level crates)
live in this single repository — clone, edit, and commit normally.

### Initial Setup
```bash
git clone ssh://git@code.sandbox.example/qos/qos.git
```

### Making Changes
Edit any component in place and commit — no submodule pointer dance. Prefer
explicit paths over `git add -A` so you only stage what you intend.

## 3. Building the Project

### Local Development (Rust)
You can build individual components using Cargo from the root:
```bash
cargo build -p shell
cargo build -p fs
cargo build -p browser
cargo build -p terminal
cargo build -p file-explorer
```

### Building the OS Image
To build the full bootc-based OS image:
```bash
./os/build.sh
```
*Note: This requires `podman` and `sudo` for the disk image generation.*

## 4. Testing

### Unit Tests
Run tests for a specific package:
```bash
cargo test -p fs
```

### VM Testing
To test the OS in a virtual machine:
1. Build the image: `./os/build.sh`
2. Run the VM: `./os/test-vm.sh`

For automated verification:
```bash
./os/verify-test.sh
./os/test-vm.sh os/output-test/qcow2/disk.qcow2
```

## 5. Running qOS on your own machine (Silverblue / Bazzite)

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

### 5.1 Install once

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
§5.3 for what it can do.

### 5.2 Edit → build → run nested

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
WAYLAND_DISPLAY=$(cat "$XDG_RUNTIME_DIR/qos-session/wayland-display") cargo run -p terminal
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

### 5.3 Edit → build → log into your own build

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

### 5.4 What a stock host does not give you

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

## 6. Headless testing — screenshots + remote control without a window

For automated tests, AI agents, and exploratory probes you can drive the live compositor without opening a window:

```bash
# Spawn → drive → snapshot → tear down (the full end-to-end recipe):
./scripts/headless-examples/full-loop.py
# Writes /tmp/full-loop-{palette,final}.png in ~5 seconds.

# OR: use the qos-control CLI directly against a manually-spawned compositor:
cargo build -p shell --bin shell --bin qos-control
env -u DISPLAY -u WAYLAND_DISPLAY \
    QOS_TEST_CONTROL=/tmp/nirios.sock \
    cargo run --bin shell -- --backend headless &
qos-control list-targets
qos-control click wheel_disc
qos-control snapshot /tmp/state.png
```

Three layers, each useful on its own:
- **`shots --mode offscreen|recording`** for pre-written scenarios → PNG / H.264
- **`QOS_TEST_CONTROL` socket + `qos-control` CLI** for ad-hoc remote control
- **`--backend headless`** for running the live compositor in CI / SSH / distrobox without forwarding a display

Full guide: [`docs/headless-testing.md`](docs/headless-testing.md). Example scripts: [`scripts/headless-examples/`](scripts/headless-examples/).

## 7. Prerequisites

-   **Rust**: Latest stable version.
-   **Podman**: For building the OS container image.
-   **QEMU**: For running the test VM.
-   **FUSE3**: Required for local testing of `fs`.
-   **Wayland**: Required for local testing of `shell`.

## Pre-commit hook

Enable the repo's git hooks once per clone:

```bash
git config core.hooksPath .githooks
```

`pre-commit` runs `scripts/check-workspace-complete.sh` — it fails if any crate
under `apps/` or `crates/` is missing from `[workspace].members` (which once let
37 apps drop out of the build). Compilation + tests run in CI (`.forgejo/workflows/check.yml`),
not the hook, to keep commits fast.
