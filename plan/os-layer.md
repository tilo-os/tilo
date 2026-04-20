# OS Layer: Fedora bootc

## Base Image

```
FROM quay.io/fedora/fedora-bootc:42
```

Fedora 42 is the latest stable release with full bootc support. The image is immutable — the root filesystem is read-only except for `/etc` (config) and `/var` (mutable data).

## Filesystem

**Root**: Btrfs
- Copy-on-Write for efficient snapshots and rollbacks
- Transparent compression (zstd) saves disk space
- Subvolumes for `/home`, `/var` enable independent snapshot policies

**Remote (future)**: JuiceFS
- FUSE-mounted S3-backed filesystem with aggressive local caching
- Metadata via SQLite/Redis (fast ls/stat), data via S3
- Mounted at workspace-specific paths for collaborative file access

## Package Strategy

### Tier 1: Base OS (in Containerfile)
Minimal system packages baked into the immutable image:
- **Display**: greetd + tuigreet (lightweight Wayland login manager)
- **Graphics**: mesa-dri-drivers, mesa-vulkan-drivers, libglvnd-egl
- **Audio/Video**: pipewire, pipewire-pulseaudio, wireplumber
- **Media**: gstreamer1, gstreamer1-plugins-base, gstreamer1-plugins-good
- **Sandbox**: bubblewrap
- **Fonts**: google-noto-sans-fonts, google-noto-sans-mono-fonts
- **Utils**: wl-clipboard, foot (terminal)
- **Build deps**: wayland-devel, libxkbcommon-devel (for Smithay)
- **Containers**: flatpak, distrobox, podman

### Tier 2: User Apps (Flatpak via Flathub)
- Covers 95%+ of GUI application needs
- Sandboxed, independent update cycle
- Wine/Gaming via Bottles or Lutris Flatpaks

### Tier 3: Dev Environments (Distrobox)
- Mutable containers on an immutable host
- Access to any Linux distro's package manager
- Seamless integration with host display/audio

## Systemd Integration

### Login Flow
```
systemd -> graphical.target -> greetd.service
  -> tuigreet (TUI login screen)
  -> user selects "QuickDesktop" session
  -> /usr/bin/quick-desktop launches as compositor
```

### Services
- `greetd.service`: Display manager, starts on graphical.target
- `quick-desktop-firstboot.service`: One-shot, generates identity key, configures Flatpak remotes
- PipeWire + WirePlumber: User session services (auto-started)

### Session Entry
`/usr/share/wayland-sessions/quick-desktop.desktop` registers the compositor as a selectable Wayland session in greetd/tuigreet.

## Build Pipeline

```
1. cargo build --release    (in quick-shell/)
2. podman build             (builds container image from os/Containerfile)
3. bootc-image-builder      (converts container -> QCOW2/ISO)
4. qemu-system-x86_64       (test in VM)
```

### Build Requirements
- Podman (for container build)
- Rust toolchain (for compositor compilation)
- bootc-image-builder (OCI -> disk image conversion)
- QEMU/KVM (for VM testing)

## Update Mechanism

```bash
# On deployed systems:
sudo bootc upgrade        # Pulls latest container image, stages for next boot
sudo bootc rollback       # Reverts to previous image if update breaks
```

Updates are atomic — the bootloader swaps the entire rootfs. No partial update states.

## Directory Layout (Deployed)

```
/usr/bin/quick-desktop                              # Compositor binary
/usr/share/wayland-sessions/quick-desktop.desktop   # Session entry
/usr/lib/systemd/system/quick-desktop-firstboot.service
/etc/quick-desktop/desktop.toml                     # Default config
/etc/greetd/config.toml                             # Login manager config
/var/home/<user>/.config/quick-desktop/             # User config overrides
/var/home/<user>/.config/quick-desktop/identity.key # Ed25519 keypair
```
