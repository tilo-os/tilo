# QuickDesktop Architecture

## Overview

QuickDesktop is a collaborative, spatial Linux desktop OS. Instead of a traditional desktop metaphor, users navigate an infinite 2D canvas containing **workspaces** — clusters of application windows organized into zones and decks. Users can invite peers P2P to join individual workspaces, where legacy applications are streamed with near-native fidelity.

## System Stack

```
┌─────────────────────────────────────────────────────────┐
│                    User Applications                     │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌─────────┐ │
│  │ Flatpak  │  │Distrobox │  │  Native  │  │  Wine   │ │
│  │  Apps    │  │Containers│  │   Apps   │  │  Apps   │ │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬────┘ │
│       │              │             │              │      │
│  ┌────┴──────────────┴─────────────┴──────────────┴────┐ │
│  │              Bubblewrap Sandbox Layer                │ │
│  └─────────────────────┬───────────────────────────────┘ │
│                        │ Wayland Protocol                │
│  ┌─────────────────────┴───────────────────────────────┐ │
│  │         quick-desktop (Spatial Compositor)           │ │
│  │  ┌───────────┐ ┌──────────┐ ┌────────────────────┐  │ │
│  │  │  Smithay  │ │ Iced UI  │ │ Collaboration Layer│  │ │
│  │  │ (Wayland) │ │ (Canvas) │ │  (Iroh+Automerge)  │  │ │
│  │  └───────────┘ └──────────┘ └────────────────────┘  │ │
│  │  ┌───────────┐ ┌──────────┐ ┌────────────────────┐  │ │
│  │  │ PipeWire  │ │ GStreamer│ │     WebRTC         │  │ │
│  │  │ (Capture) │ │ (Encode) │ │   (Transport)      │  │ │
│  │  └───────────┘ └──────────┘ └────────────────────┘  │ │
│  └─────────────────────────────────────────────────────┘ │
│                                                          │
│  ┌──────────────────────────────────────────────────────┐│
│  │                 System Services                       ││
│  │  greetd (login) · PipeWire · systemd · NetworkManager ││
│  └──────────────────────────────────────────────────────┘│
│                                                          │
│  ┌──────────────────────────────────────────────────────┐│
│  │            Fedora bootc (Atomic OS Image)             ││
│  │  Linux Kernel · Btrfs · Wayland · Mesa · Firmware     ││
│  └──────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────┘
```

## Three Pillars

### 1. OS Layer (bootc / Fedora Atomic)
- Immutable base OS defined as an OCI container image
- Atomic updates via `bootc upgrade` (container image pull + bootloader swap)
- User apps via Flatpak, dev environments via Distrobox
- Btrfs for snapshots, compression, and copy-on-write

### 2. Compositor (Smithay + Iced)
- Smithay handles Wayland protocol: surfaces, input, buffers, XDG shell
- Iced renders the spatial canvas: infinite grid, zones, decks, tabs
- Applications run as Wayland clients inside bubblewrap sandboxes
- Pixel buffers captured from Wayland surfaces, rendered on the canvas

### 3. Collaboration Layer (Iroh + Automerge + WebRTC)
- Iroh provides P2P mesh networking with NAT traversal
- Automerge CRDTs synchronize workspace state (zone positions, window metadata, cursors)
- GStreamer encodes damage regions from Wayland surfaces
- WebRTC transports encoded frames with <100ms latency
- Remote input events are validated and injected into the compositor

## Data Flow

### Local Application Rendering
```
App (Wayland Client) -> Smithay surface commit -> SHM buffer read
  -> Pixel data via mpsc channel -> Iced canvas -> wgpu -> Display
```

### Remote Collaboration
```
Local state change -> Automerge document mutation -> Sync message
  -> Iroh QUIC connection -> Remote peer -> Apply Automerge change
  -> Update workspace state -> Re-render canvas
```

### Window Streaming (Host -> Guest)
```
Wayland damage event -> PipeWire capture (DMA-BUF) -> GStreamer encode
  (AV1, ROI for damaged region) -> WebRTC RTP -> Remote peer
  -> Decode -> Render as texture on canvas
```

### Remote Input (Guest -> Host)
```
Guest click on streamed window -> Calculate relative coords
  -> Serialize InputEvent -> Send via Iroh -> Host validates
  (region check, rate limit, key filter) -> Inject via Smithay seat
```

## Project Directory Structure

```
quickdesktop/
├── plan/                    # Architecture & planning docs
│   ├── architecture.md
│   ├── os-layer.md
│   ├── compositor.md
│   ├── collaboration.md
│   ├── security.md
│   └── roadmap.md
├── os/                      # bootc OS image definition
│   ├── Containerfile
│   ├── config.toml
│   ├── build.sh
│   ├── test-vm.sh
│   ├── usr/
│   │   ├── share/wayland-sessions/quick-desktop.desktop
│   │   └── lib/systemd/system/quick-desktop-firstboot.service
│   └── etc/
│       ├── quick-desktop/desktop.toml
│       └── greetd/config.toml
└── quick-gnome-shell/       # Compositor source (Rust)
    ├── Cargo.toml
    ├── desktop.toml
    └── src/
        ├── main.rs          # Entry point, iced app
        ├── state.rs         # Full Smithay state (to be unified)
        ├── config.rs        # TOML config loading
        ├── workspace.rs     # Zone/Deck/Tab data structures
        ├── security.rs      # Module root
        ├── security/
        │   └── container.rs # Bubblewrap sandboxing
        ├── ui.rs            # Module root
        ├── ui/
        │   └── canvas.rs    # Iced canvas renderer
        └── network/
            ├── mod.rs       # Module root
            ├── p2p.rs       # P2P networking (stub -> Iroh)
            └── wayland.rs   # Wayland server thread
```

## Key Spatial Concepts

- **Canvas**: The infinite 2D surface the user navigates by panning/zooming
- **Workspace**: A conceptual grouping — a cluster of zones on the canvas that can be shared with peers
- **Zone**: A rectangular region containing one or more decks, associated with a project directory
- **Deck**: A container within a zone holding tabbed windows, similar to a terminal pane
- **Tab**: A single application window running inside a deck
