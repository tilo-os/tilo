# VM Testing Guide

This guide explains how to build and test the qOS OS using QEMU.

## Verification System

qOS uses a tiered testing system to ensure the FUSE filesystem and system applications are correctly integrated without bloating the production image.

### 1. Automated Verification Build

We provide a dedicated script that builds the OS and performs automated checks on the resulting image.

```bash
./os/verify-test.sh
```

**What this script does:**
1.  **Builds Production Components**: Compiles the compositor, FUSE driver, and system apps.
2.  **Builds Test Image**: Creates a specialized container image (`os/Containerfile.test`) that includes a validation script and a systemd test service.
3.  **Generates Test Disk**: Produces a bootable QCOW2 image at `os/output-test/qcow2/disk.qcow2`.

### 2. Running the Test VM

Once the test image is built, you can boot it in QEMU to see the results.

```bash
./os/test-vm.sh os/output-test/qcow2/disk.qcow2
```

**Requirements:**
-   **UEFI Support**: The VM requires `OVMF` (Open Virtual Machine Firmware). The script searches for it in common paths like `/usr/share/edk2/ovmf/`.
-   **KVM**: Hardware acceleration is highly recommended (`/dev/kvm`).

### 3. Checking Automated Test Results

Upon booting, the `quick-fs-test.service` runs automatically. You can verify the results via the system journal:

```bash
journalctl -u quick-fs-test
```

**The test confirms:**
-   `/var/workspace` is a valid FUSE mount point.
-   `hello.txt` exists and contains the expected collaborative metadata.
-   `terminal`, `file-explorer`, and `browser` are installed and executable in `/usr/bin/`.

---

## Compositor (Window Manager) Testing Strategy

To ensure the window manager logic (2D clustering, non-overlapping constraints, sandbox permissions) is robust, the Wayland state is decoupled from the graphical UI layer:

1. **Headless Mode:** The compositor (`shell`) can be instantiated in a headless mode without requiring a GPU or Iced winit window.
2. **Automated Client Integration Tests:** Using the `wayland-client` crate, test suites can connect to the headless compositor socket, spawn mock applications, and assert correct clustering behavior and portal permission prompts.
3. **Running Headless Tests:**
   ```bash
   cd shell
   cargo test --test interaction_test
   ```

---

## Manual Testing

If you want to test the production image manually:

1.  **Build**: Run `./os/build.sh`.
2.  **Launch**: Run `./os/test-vm.sh`.
3.  **Login**: Use the default credentials:
    -   **User**: `tester`
    -   **Password**: `quickdesktop`

### Useful VM Commands

-   **Check FUSE Mount**: `mountpoint /var/workspace`
-   **Inspect Cache**: `ls -lah /var/cache/qos-fs`
-   **Run Apps**:
    -   `terminal`
    -   `file-explorer`
    -   `browser`
