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

## 5. Headless testing — screenshots + remote control without a window

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

## 6. Prerequisites

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
