# QuickDesktop Architecture

## Overview

QuickDesktop (codename **nirios**) is a collaborative, spatial Linux OS built on Fedora Bootc. Instead of a traditional desktop metaphor, users navigate an infinite 2D canvas of **workspaces** — clusters of application windows organized into strips and tab-groups. Users can invite peers P2P to join individual workspaces, where native and legacy apps are streamed with near-native fidelity. Every workspace runs apps inside its own Bubblewrap sandbox with explicit capability grants.

## System Stack

```
┌──────────────────────────────────────────────────────────┐
│                    User Applications                      │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌─────────┐  │
│  │ Flatpak  │  │Distrobox │  │ Native   │  │ Wine /  │  │
│  │  Apps    │  │Containers│  │ quick-ui │  │  Other  │  │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬────┘  │
│       │              │             │              │       │
│  ┌────┴──────────────┴─────────────┴──────────────┴────┐ │
│  │  Per-workspace Bubblewrap sandbox (seccomp + ns)    │ │
│  │  Per-workspace Wayland bridge socket (filtered)     │ │
│  │  xdg-desktop-portal D-Bus backend (grant-gated)     │ │
│  └─────────────────────┬───────────────────────────────┘ │
│                        │ Wayland Protocol                 │
│  ┌─────────────────────┴───────────────────────────────┐ │
│  │     quick-desktop (Spatial Smithay Compositor)      │ │
│  │  ┌───────────┐ ┌──────────┐ ┌───────────────────┐  │ │
│  │  │  Smithay  │ │ vello +  │ │  Collaboration    │  │ │
│  │  │  (proto)  │ │  wgpu    │ │  (libp2p+automerge)│ │ │
│  │  └───────────┘ └──────────┘ └───────────────────┘  │ │
│  │  ┌───────────┐ ┌──────────┐ ┌───────────────────┐  │ │
│  │  │  openh264 │ │ PipeWire │ │   Control socket  │  │ │
│  │  │ (peer enc)│ │(screencst)│ │   (NIRIOS_TEST_…) │  │ │
│  │  └───────────┘ └──────────┘ └───────────────────┘  │ │
│  └──────────────────────────────────────────────────── │ │
│                                                          │
│  ┌──────────────────────────────────────────────────┐   │
│  │  quick-fs-fuse  (Iroh + iroh-blobs + blake3)     │   │
│  │  mounted at /var/workspace, cache /var/cache/    │   │
│  └──────────────────────────────────────────────────┘   │
│                                                          │
│  ┌──────────────────────────────────────────────────┐   │
│  │  Fedora Bootc 42 (immutable container OCI image) │   │
│  │  Linux · Btrfs · Wayland · Mesa · greetd         │   │
│  └──────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────┘
```

## Three Pillars

### 1. OS Layer (Fedora Bootc)
- Immutable base OS defined as an OCI container image (`quay.io/fedora/fedora-bootc:42`).
- Atomic updates via `bootc upgrade`; rollback via `bootc rollback`.
- Read-only root; persistent state lives under `/var`.
- User apps via Flatpak; dev environments via Distrobox.
- See `plan/os-layer.md`.

### 2. Compositor (`quick-desktop`)
- Smithay-based Wayland **server** (since the Phase 1-7 rewrite). Hosts foreign Wayland clients and adopts them as tabs in the active strip via `app-id` matching.
- Two modes selected at boot: `--mode hosted` (smithay-backed Wayland session running as a client of a parent display server, for dev) and `--mode native` (default — smithay running on bare hardware via `--backend drm` or in winit for development).
- Backends are a trait (`compositor::backend::Backend`); current implementations are `winit` and `drm` (libseat + libinput + udev + GBM).
- Per-frame paint uses `vello` over `wgpu`. Shell chrome (top bar, wheel disc, command palette, modals) and foreign-surface buffers compose in the same `vello::Scene`.
- See `plan/compositor.md`.

### 3. Collaboration + Sandbox Layer
- **Collaboration:** `libp2p` (gossipsub + mdns + noise + tcp + yamux) carries one topic per workspace (`/nirios/ws/<id>/v1`). CRDT changes ride on top via `automerge`'s incremental change protocol.
- **Surface streaming:** owners encode foreign-client surfaces to H.264 (openh264 software path) and publish them on the workspace topic. Peers decode in a `peer_view` widget; their input events are serialised and replayed into the owner's smithay seat.
- **Sandbox:** every workspace pins a per-workspace `SandboxProfile` rendered into `bwrap` argv, with a seccomp-bpf blocklist, a private DBus session bus, and a per-workspace Wayland bridge socket. The portal D-Bus backend (`org.freedesktop.impl.portal.*`) gates capability requests through the compositor's prompt UI.
- See `plan/collaboration.md`, `plan/security.md`.

## Data flow

### Local rendering
```
Foreign Wayland client commits buffer → smithay CompositorHandler::commit
  → buffer stored on surface user-data
  → compositor frame loop reads buffer, draws into vello::Scene
  → wgpu submit → backend (winit window or DRM scanout)
```

### Foreign-client adoption
```
xdg_toplevel maps → app-id present
  → search active workspace's strips for tab with desired_app_id == app_id
  → if match: adopt the surface into that tab
  → else: create Tab::Foreign { app_id, surface_id } in active group of active strip
```

### CRDT replication
```
Local mutation → WorkspaceTree write (cache + automerge AutoCommit)
  → SyncReq heartbeat (every 2 s) or immediate Publish
  → libp2p gossipsub workspace topic
  → remote peer: NeighbourMsg::SyncResp { changes }
  → WorkspaceTree::apply_changes → rebuild_cache_from_doc → re-render
```

### Foreign-surface streaming (peer-sync Phase 4)
```
Owner: surface commit → damage hint → openh264 encode (30 fps cap)
  → NeighbourMsg::SurfaceFrame published on workspace topic
  → peer: H264Decoder → peer_view widget draws decoded frames into tab content rect
```

### Remote input replay
```
Peer pointer/keyboard event on a peer_view widget
  → NeighbourMsg::RemoteInput { surface, event } published
  → owner compositor: smithay Seat::input injects into the foreign client
  → owner can flip per-peer read_only flag to drop events
```

## State model

The single source of truth for per-session shell state is `WorkspaceTree` in `quick-shell/src/shell/state.rs`. It uses a **cached materialisation** pattern (see `docs/superpowers/specs/2026-05-04-workspace-tree-automerge-design.md`):

- A public struct of `Vec<Workspace>`, `HashMap<StripId, GroupId>`, etc. — every existing reader keeps `tree.workspaces.iter()` style access.
- A backing `automerge::AutoCommit` document mirrors the cache. Every mutator writes through to both sides.
- `current_heads`, `export_changes_since`, `apply_changes` operate on the doc for peer-sync. `apply_changes` rebuilds the cache from the doc.
- A debug-only `assert_cache_matches_doc()` runs after each mutator to catch drift.

```
WorkspaceTree
├── workspaces: Vec<Workspace>
│   ├── id, name, strips: Vec<Strip>, grants: HashMap<AppId, Vec<Capability>>
│   └── Strip ├─ id, name
│             └─ groups: Vec<WindowGroup>
│                ├─ id, maximized, width
│                └─ tabs: Vec<Tab>
│                   └─ TileContent::{Notes | Editor | Files | Terminal | Browser | Foreign | Placeholder}
├── active: WorkspaceId
├── active_strip: HashMap<WorkspaceId, StripId>
├── active_group: HashMap<StripId, GroupId>
├── collaborators: Vec<Collaborator>
└── doc: automerge::AutoCommit          # backing CRDT, #[serde(skip)]
```

`TileContent` variants with payload encode the discriminant as a string scalar plus a serde-JSON blob in a sibling `Bytes` field — payload merges are last-write-wins for v1.

## E2E test infrastructure

Three layers cover different parts of the stack:

1. **In-process driver** (`quick-shell/src/bin/shots/`) — `ShellDriver` builds the same widget tree the live binary builds, runs the same paint pass, and exposes a `widget_registry: HashMap<String, Rect>` that walks the live tree via `Widget::test_id(&self) -> Option<&str>` (from `quick-ui::widget::Widget`). Path-fallback (`Parent/Child[index]`) works without opt-in. Used by >100 tests in `quick-shell/tests/*` and by `cargo run --bin shots`.
2. **Offscreen / recording renderer** — `cargo run --bin shots -- --mode offscreen --out target/<dir>` writes PNGs per scenario; `--mode recording --out target/<file>.h264` writes a real H.264 video. Output is identical to the live binary's rendering. **Never** screenshot the user's running session — use the offscreen path.
3. **Control socket** (spec at `docs/superpowers/specs/2026-05-16-real-input-e2e-design.md`) — when the compositor boots with `NIRIOS_TEST_CONTROL=<sock>` set, it binds an extra unix socket alongside the wayland one. Newline-delimited JSON commands inject through `smithay::input::Seat::input` so they go through real focus rules. Lives in `quick-shell/src/compositor/test_control.rs`. Phase 1 (pure-logic dispatcher + `list_targets` + `click_at`) has landed; later phases (drag, key, type, snapshot, state_dump, CLI wrapper, `tests/support/live_compositor.rs`) are tracked in the spec.

The control socket is the out-of-process path for covering the seat layer, frame-pacing regressions, and focus-stealing behaviour the in-process driver can't reproduce.

## Project directory structure (Cargo workspace + submodules)

```
quickdesktop/                            # superproject
├── plan/                                # this directory — design intent
├── docs/                                # architecture + testing + superpowers plans/specs
├── quick-ui/                            # in-tree — UI toolkit
├── quick-shell/                         # submodule — compositor + shell
│   └── src/
│       ├── compositor/                  # smithay state, backends, frame, tab-binding, test_control
│       │   └── backend/{winit,drm}/
│       ├── shell/                       # WorkspaceTree, overlays, chrome, network bridge
│       ├── sandbox/                     # bwrap profile, seccomp blob, spawn
│       ├── portal/                      # FileChooser, ScreenCast, Camera D-Bus backends
│       ├── network/                     # libp2p actor, behaviour, hub, replica, encoder, invite
│       └── bin/{shots,nirios-control}/  # e2e harness + control-socket CLI
├── quick-fs-fuse/                       # submodule — FUSE3 driver
├── apps/{terminal,file-explorer,browser}/  # submodules
└── os/                                  # submodule — Containerfile + systemd units
```

Only `quick-ui` lives in the main repo; everything else is a submodule. See `CLAUDE.md` for the submodule workflow.

## Key spatial concepts

- **Canvas**: the infinite 2D surface the user navigates via the wheel disc and palette.
- **Workspace**: a cluster of strips on the canvas that can be shared with peers; the unit of sandboxing, capability grants, and CRDT replication.
- **Strip**: a horizontal lane within a workspace, containing one or more window-groups.
- **WindowGroup**: a tabbed container holding one or more tabs (formerly "deck"). Tracks `maximized` and `width`.
- **Tab**: a single application window. Either `TileContent::Native(...)` (a quick-ui app) or `Tab::Foreign { app_id, surface_id }` (a Wayland client adopted via app-id matching).
