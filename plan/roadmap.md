# Implementation Roadmap

## Phase 0: Foundation (Current)

**Goal**: Planning docs + bootable OS image + compilation on stable Rust

- [x] Write architecture, OS layer, compositor, collaboration, security, roadmap docs
- [ ] Fix `Cargo.toml` edition to 2021, add `[[bin]]` section
- [ ] Update `config.rs` for multi-path config search (`~/.config/quick-desktop/` -> `/etc/quick-desktop/` -> `./`)
- [ ] Add `--firstboot` CLI argument to `main.rs`
- [ ] Create `os/Containerfile` with Fedora bootc 42 base
- [ ] Create supporting OS files (greetd config, session entry, systemd units)
- [ ] Create `build.sh` and `test-vm.sh` scripts
- [ ] Verify `cargo build --release` succeeds
- [ ] Build container image with podman
- [ ] Build QCOW2 with bootc-image-builder
- [ ] Boot in QEMU, verify greetd login works

**Deliverable**: A QCOW2 image that boots to a login screen and launches the compositor.

## Phase 1: Pixel Rendering + Input Forwarding

**Goal**: Make the compositor actually display application windows and accept input

- [ ] Implement pixel extraction in `network/wayland.rs` commit handler (SHM buffer -> Vec<u8>)
- [ ] Render pixel buffers in `ui/canvas.rs` using `iced::widget::image::Handle::from_rgba()`
- [ ] Unify `state.rs` and `network/wayland.rs` (merge XDG shell, seat, etc. into WaylandServer)
- [ ] Forward keyboard/mouse input from Iced to focused Wayland client's Smithay seat
- [ ] Test: spawn `foot` terminal in a deck, see actual terminal output, type into it

**Deliverable**: A functional compositor where you can open apps and interact with them on the spatial canvas.

## Phase 2: P2P Networking + Shared State

**Goal**: Two instances can connect and share workspace state

- [ ] Replace `libp2p` with `iroh` in Cargo.toml
- [ ] Implement Ed25519 identity key management (`security/identity.rs`)
- [ ] Build Iroh endpoint and connection logic (`network/p2p.rs`)
- [ ] Integrate Automerge with WorkspaceState (`workspace.rs`)
- [ ] Wire up P2P subscription alongside Wayland and keyboard subscriptions in `main.rs`
- [ ] Implement cursor presence (broadcast at 30Hz, render remote cursors)
- [ ] Test: two instances on same machine, create zone on one, appears on other

**Deliverable**: Two peers can connect, see each other's cursors, and share workspace layout changes in real-time.

## Phase 3: Window Streaming + Authentication

**Goal**: Remote users can see and interact with shared application windows

- [ ] Implement Macaroon token minting and verification (`security/auth.rs`)
- [ ] Integrate auth into Iroh connection handshake
- [ ] Build GStreamer pipeline for damage-aware encoding (`network/streaming.rs`)
- [ ] Implement WebRTC transport over Iroh QUIC streams
- [ ] Implement remote input injection with region checks and keyboard scrubbing
- [ ] Add PipeWire socket passthrough to bubblewrap args
- [ ] Add host override (physical input priority, kill switch)
- [ ] Test: User A shares a zone, User B sees streamed windows and can click/type

**Deliverable**: Full collaborative workspace sharing with streaming, input, and security.

## Phase 4: Polish + Ecosystem

**Goal**: A usable daily-driver desktop experience

- [ ] Flatpak integration (install/launch Flatpak apps from within the compositor)
- [ ] Distrobox integration (spawn dev containers as zones)
- [ ] First-boot wizard UX (user creation, theme selection, Flatpak remote setup)
- [ ] Workspace persistence (save/load workspace layouts)
- [ ] File sharing via tarpc (remote file browsing)
- [ ] JuiceFS integration for cloud-backed shared filesystems
- [ ] Zoom levels (overview -> zone -> focused deck)
- [ ] Drag-and-drop between decks
- [ ] Notification system
- [ ] Settings panel

**Deliverable**: A complete, usable collaborative spatial desktop OS.

## Tech Stack Summary

| Component | Technology | Phase |
|-----------|-----------|-------|
| OS Image | Fedora bootc 42 | 0 |
| Filesystem | Btrfs (local), JuiceFS (remote) | 0, 4 |
| Compositor | Smithay | 0 |
| UI Framework | Iced | 0 |
| App Sandbox | Bubblewrap | 0 |
| Package Format | Flatpak | 0, 4 |
| Login Manager | greetd + tuigreet | 0 |
| P2P Networking | Iroh | 2 |
| Shared State | Automerge (CRDT) | 2 |
| Video Encoding | GStreamer + SVT-AV1 | 3 |
| Video Transport | WebRTC | 3 |
| Authentication | Ed25519 + Macaroons | 2, 3 |
| Remote Files | tarpc | 4 |
| Cloud Storage | JuiceFS (S3-backed) | 4 |
