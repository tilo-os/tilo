# OS Architecture Documentation

qOS is a bootable, container-based operating system built on the **Fedora Bootc** framework.

## Core Components

### 1. Compositor (`shell`)
A Wayland compositor responsible for the desktop environment. It handles window management, input, and basic UI elements. Located in `shell/`.

**Window Management Paradigm:**
- **Spatial model:** Windows cannot overlap. Apps are organized as `Workspace → Strip → WindowGroup → Tab`: tabs tile within a group, groups scroll horizontally within a strip (Niri-inspired), strips stack within a workspace, and workspaces are switched via a wheel disc.
- **Per-workspace sandboxing:** Each workspace runs in an isolated Bubblewrap sandbox with its own file-access allowlist. When an app requests a path outside it, the compositor prompts the user, who can grant access temporarily or permanently to the workspace.
- **Collaboration:** Users can invite peers to a specific workspace, sharing its context via Wayland buffer streaming and remote input injection.

### 2. FUSE Filesystem (`fs`)
A P2P collaborative filesystem mounted at `/var/workspace`.
-   **Projection Layer**: Shows files immediately based on P2P metadata.
-   **Hydration**: Fetches file content on-demand from the network via Iroh.
-   **Performance**: Uses a 10s metadata TTL and 1MB readahead for optimized VM performance.

### 3. System Applications & QuickUI Toolkit
-   **QuickUI Framework:** A custom, lightweight retained-mode UI framework built over wgpu/Vello. It is fundamentally designed around Automerge CRDTs to provide Out-Of-The-Box (OOTB) multi-cursor and collaborative text editing. It enforces OS-controlled styling for consistent, user-themeable presentation.
-   **terminal**: A GPU-accelerated terminal emulator.
-   **file-explorer**: A file manager for browsing the local and collaborative filesystem.
-   **browser**: A modern web browser with native collaborative features.

## Image Structure

The OS is built using `Containerfile` and converted to a bootable image via `os/dev/disk-image.sh`, which partitions with `systemd-repart` and installs with `bootc install to-filesystem`.

-   **Base**: Fedora bootc 44, mirrored into `code.podesta.ai/qos/fedora-bootc` and pinned by digest in `os/base-snapshot.env` (see `docs/supply-chain.md` for why the mirror exists).
-   **FUSE Support**: `fuse3` installed for userspace filesystem mounting.
-   **Persistence**: Caching is directed to `/var/cache/quick-fs` and mounting happens in `/var/workspace` to ensure compatibility with the read-only root.

## Boot Sequence

1.  **UEFI/GRUB**: Standard boot process.
2.  **greetd**: Displays the login prompt (tuigreet).
3.  **quick-fs.service**: Mounts the collaborative filesystem to `/var/workspace`.
4.  **shell**: Started by `tuigreet` as the Wayland session.
5.  **shell-firstboot.service**: Handles initial user setup on the first boot.
