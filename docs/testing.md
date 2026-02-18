# VM Testing Guide

This guide explains how to build and test the QuickDesktop OS using QEMU.

## Verification System

QuickDesktop uses a tiered testing system to ensure the FUSE filesystem and system applications are correctly integrated without bloating the production image.

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
-   `quick-terminal`, `file-explorer`, and `quick-browser` are installed and executable in `/usr/bin/`.

---

## Manual Testing

If you want to test the production image manually:

1.  **Build**: Run `./os/build.sh`.
2.  **Launch**: Run `./os/test-vm.sh`.
3.  **Login**: Use the default credentials:
    -   **User**: `quickuser`
    -   **Password**: `quickdesktop`

### Useful VM Commands

-   **Check FUSE Mount**: `mountpoint /var/workspace`
-   **Inspect Cache**: `ls -lah /var/cache/quick-fs`
-   **Run Apps**:
    -   `quick-terminal`
    -   `file-explorer`
    -   `quick-browser`
