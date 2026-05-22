# Development Guide

Welcome to the QuickDesktop project! This document outlines how to set up your environment and work with our submodule-based architecture.

## 1. Project Structure

QuickDesktop is organized as a Cargo Workspace using Git Submodules for its core components.

-   `quick-shell/`: The Wayland compositor (`quick-desktop`).
-   `quick-fs-fuse/`: The collaborative P2P filesystem.
-   `os/`: Build scripts and configurations for the Fedora Bootc OS.
-   `apps/`:
    -   `browser/`: The Servo-based browser (`quick-browser`).
    -   `file-explorer/`: Spatial file manager.
    -   `terminal/`: GPU-accelerated terminal.

## 2. Working with Submodules

Since this project uses submodules, you need to be aware of how to manage them.

### Initial Setup
If you just cloned the repository, initialize the submodules:
```bash
git submodule update --init --recursive
```

### Pulling Updates
To pull the latest changes for all submodules:
```bash
git submodule update --remote --merge
```

### Making Changes
When working on a sub-project (e.g., the browser):
1. `cd apps/browser`
2. Make your changes and commit them *inside* the browser directory.
3. `cd ../..` (back to root)
4. `git add apps/browser` to update the pointer in the main repository.
5. Commit the pointer update.

## 3. Building the Project

### Local Development (Rust)
You can build individual components using Cargo from the root:
```bash
cargo build -p quick-desktop
cargo build -p quick-fs-fuse
cargo build -p quick-browser
cargo build -p quick-terminal
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
cargo test -p quick-fs-fuse
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

# OR: use the nirios-control CLI directly against a manually-spawned compositor:
cargo build -p quick-desktop --bin quick-desktop --bin nirios-control
env -u DISPLAY -u WAYLAND_DISPLAY \
    NIRIOS_TEST_CONTROL=/tmp/nirios.sock \
    cargo run --bin quick-desktop -- --backend headless &
nirios-control list-targets
nirios-control click wheel_disc
nirios-control snapshot /tmp/state.png
```

Three layers, each useful on its own:
- **`shots --mode offscreen|recording`** for pre-written scenarios → PNG / H.264
- **`NIRIOS_TEST_CONTROL` socket + `nirios-control` CLI** for ad-hoc remote control
- **`--backend headless`** for running the live compositor in CI / SSH / distrobox without forwarding a display

Full guide: [`docs/headless-testing.md`](docs/headless-testing.md). Example scripts: [`scripts/headless-examples/`](scripts/headless-examples/).

## 6. Prerequisites

-   **Rust**: Latest stable version.
-   **Podman**: For building the OS container image.
-   **QEMU**: For running the test VM.
-   **FUSE3**: Required for local testing of `quick-fs-fuse`.
-   **Wayland**: Required for local testing of `quick-desktop`.
