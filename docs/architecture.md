# OS Architecture Documentation

QuickDesktop is a bootable, container-based operating system built on the **Fedora Bootc** framework.

## Core Components

### 1. Compositor (`quick-desktop`)
A Wayland compositor responsible for the spatial desktop environment. It handles window management, input, and basic UI elements. Located in `quick-gnome-shell/`.

### 2. FUSE Filesystem (`quick-fs-fuse`)
A P2P collaborative filesystem mounted at `/var/workspace`.
-   **Projection Layer**: Shows files immediately based on P2P metadata.
-   **Hydration**: Fetches file content on-demand from the network via Iroh.
-   **Performance**: Uses a 10s metadata TTL and 1MB readahead for optimized VM performance.

### 3. System Applications
-   **quick-terminal**: A GPU-accelerated terminal emulator built with Iced.
-   **file-explorer**: A spatial file manager for browsing the local and collaborative filesystem.
-   **quick-browser**: A modern, Servo-based web browser with native collaborative features.

## Image Structure

The OS is built using `Containerfile` and converted to a bootable image via `bootc-image-builder`.

-   **Base**: `quay.io/fedora/fedora-bootc:42`
-   **FUSE Support**: `fuse3` installed for userspace filesystem mounting.
-   **Persistence**: Caching is directed to `/var/cache/quick-fs` and mounting happens in `/var/workspace` to ensure compatibility with the read-only root.

## Boot Sequence

1.  **UEFI/GRUB**: Standard boot process.
2.  **greetd**: Displays the login prompt (tuigreet).
3.  **quick-fs.service**: Mounts the collaborative filesystem to `/var/workspace`.
4.  **quick-desktop**: Started by `tuigreet` as the Wayland session.
5.  **quick-desktop-firstboot.service**: Handles initial user setup on the first boot.
