# VM testing guide

This guide covers image-level checks in a Tilo QEMU VM. For headless
compositor and application lanes, see [`headless-testing.md`](headless-testing.md).

## Verification System

The test image adds a validation service to the production image so FUSE,
shipped binaries, portal registration, and basic compositor startup can be
checked together without changing the production image.

### 1. Automated Verification Build

The dedicated script builds the components, test image, and bootable disk.

```bash
./os/verify-test.sh
```

It compiles the compositor, filesystem driver, and system apps, builds
`os/Containerfile.test`, and writes `os/output-test/qcow2/disk.qcow2`.

### 2. Running the Test VM

Once the test image is built, you can boot it in QEMU to see the results.

```bash
./os/test-vm.sh os/output-test/qcow2/disk.qcow2
```

**Requirements:**
-   **UEFI Support**: The VM requires `OVMF` (Open Virtual Machine Firmware). The script searches for it in common paths like `/usr/share/edk2/ovmf/`.
-   **KVM**: Hardware acceleration is highly recommended (`/dev/kvm`).

### 3. Checking Automated Test Results

Upon booting, `fs-test.service` runs automatically. Verify the results with:

```bash
journalctl -u fs-test
```

The test confirms that `/var/workspace` is a FUSE mount, the synthetic
`hello.txt` content is available, core binaries are executable, the Tilo portal
definition is registered, and the shell command surface runs. The browser is
optional when the image was built with `TILO_WITH_BROWSER=0`.

---

## Compositor (Window Manager) Testing Strategy

To ensure the window manager logic (2D clustering, non-overlapping constraints, sandbox permissions) is robust, the Wayland state is decoupled from the graphical UI layer:

1. **Headless Mode:** The compositor (`shell`) can be instantiated with
   `--backend headless`, without a host display window.
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
    -   **User**: `quickuser`
    -   **Password**: `tilo`

### Useful VM commands

-   **Check FUSE mount**: `mountpoint /var/workspace`
-   **Inspect cache**: `ls -lah /var/cache/quick-fs`
-   **Run Apps**:
    -   `terminal`
    -   `file-explorer`
    -   `browser`
