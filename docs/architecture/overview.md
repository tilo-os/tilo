# Tilo Architecture

## Overview

Tilo (codename **tilo**) is a collaborative Linux OS built on Fedora Bootc. See `README.md` in this directory for the resource/action model the rest follows from. Users organize application windows into **workspaces** (`Workspace → Strip → WindowGroup → Tab`): each workspace holds vertically-stacked strips of horizontally-scrollable window-groups (Niri-inspired), and workspaces and strips are switched from the strip switcher at the right screen edge. Users can invite peers P2P to join individual workspaces, where native and legacy apps are streamed with near-native fidelity. Every workspace runs apps inside its own Bubblewrap sandbox with explicit capability grants.

## System Stack

```
┌──────────────────────────────────────────────────────────┐
│                    User Applications                      │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌─────────┐  │
│  │ Flatpak  │  │Distrobox │  │ Native   │  │ Wine /  │  │
│  │  Apps    │  │Containers│  │ ui │  │  Other  │  │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬────┘  │
│       │              │             │              │       │
│  ┌────┴──────────────┴─────────────┴──────────────┴────┐ │
│  │  Per-workspace Bubblewrap sandbox (seccomp + ns)    │ │
│  │  Wayland clients attributed by peer credentials     │ │
│  │  xdg-desktop-portal D-Bus backend (grant-gated)     │ │
│  └─────────────────────┬───────────────────────────────┘ │
│                        │ Wayland Protocol                 │
│  ┌─────────────────────┴───────────────────────────────┐ │
│  │     shell (Spatial Smithay Compositor)      │ │
│  │  ┌───────────┐ ┌──────────┐ ┌───────────────────┐  │ │
│  │  │  Smithay  │ │ vello +  │ │  Collaboration    │  │ │
│  │  │  (proto)  │ │  wgpu    │ │   (Iroh+automerge) │ │ │
│  │  └───────────┘ └──────────┘ └───────────────────┘  │ │
│  │  ┌───────────┐ ┌──────────┐ ┌───────────────────┐  │ │
│  │  │ GStreamer │ │ PipeWire │ │   Control socket  │  │ │
│  │  │ (peer enc)│ │(screencst)│ │ (TILO_TEST_CONTROL)│  │ │
│  │  └───────────┘ └──────────┘ └───────────────────┘  │ │
│  └──────────────────────────────────────────────────── │ │
│                                                          │
│  ┌──────────────────────────────────────────────────┐   │
│  │  fs  (Iroh + iroh-blobs + blake3)     │   │
│  │  mounted at /var/workspace, cache /var/cache/    │   │
│  └──────────────────────────────────────────────────┘   │
│                                                          │
│  ┌──────────────────────────────────────────────────┐   │
│  │  Fedora Bootc (immutable container OCI image)    │   │
│  │  Linux · Btrfs · Wayland · Mesa · greetd         │   │
│  └──────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────┘
```

## Three Pillars

### 1. OS Layer (Fedora Bootc)
- Immutable base OS defined as an OCI container image (`quay.io/fedora/fedora-bootc:44`).
- Atomic updates via `bootc upgrade`; rollback via `bootc rollback`.
- Read-only root; persistent state lives under `/var`.
- User apps via Flatpak; dev environments via Distrobox.
- See `docs/architecture/os-layer.md`.

### 2. Compositor (`shell`)
- Smithay-based Wayland **server**. Hosts foreign Wayland clients and adopts them as tabs in the active strip via `app-id` matching.
- One session, on the backend selected at boot: `--backend drm` on bare hardware, `--backend winit` (default) in a window inside an existing desktop for development, `--backend headless` for CI.
- Backends are a trait (`compositor::backend::Backend`); current implementations are `winit` and `drm` (libseat + libinput + udev + GBM).
- Per-frame paint uses `vello` over `wgpu`. Shell chrome (top bar, strip switcher, command palette, modals) and foreign-surface buffers compose in the same `vello::Scene`.
- See `docs/architecture/compositor.md`.

### 3. Collaboration + Sandbox Layer
- **Collaboration:** Iroh gossip carries direct workspace traffic; the authenticated Iroh workspace host persists and store-and-forwards durable changes. CRDT changes ride on top via `automerge`'s incremental change protocol.
- **Surface streaming:** owners encode foreign-client surfaces through the media stack's real-time encoder (AV1/VP9 when every viewer decodes it in hardware, else H.264; hardware first) and publish them on the topic of the room the window's tab is in. Peers decode in a `peer_view` widget mounted in that exact tab; their input events are serialised and replayed into the owner's smithay seat. Between a person's own devices only legacy app windows are streamed, each to the devices currently showing its tab (see `docs/device-mesh.md`); no device streams its desktop.
- **Sandbox:** the compositor execs `tilo-launcher`, which builds each app's `bwrap` argv from its manifest and the workspace's grants, with a seccomp-bpf blocklist, Landlock, a private DBus session bus, and the compositor's public Wayland socket, whose connections are attributed to package and workspace from the launch record. The portal D-Bus backend (`org.freedesktop.impl.portal.*`) gates capability requests through the compositor's prompt UI.
- See `docs/architecture/collaboration.md`, `docs/architecture/security.md`.

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
  → immediate Publish (SyncResp); SyncReq repair pull 2 s after activity settles,
    on a peer joining, and every 5 min while idle
  → Iroh gossip workspace topic and optional persistent workspace host
  → remote peer: NeighbourMsg::SyncResp { changes }
  → WorkspaceTree::apply_changes → rebuild_cache_from_doc → re-render
```

### Foreign-surface streaming
```
Viewer (own device): a HostedWindow tab is laid out → PersonalFabricAction::WindowViews { tab, size } to its host
Owner: surface commit → damage hint → media real-time encode (cap; a held frame
  is flushed when due) → NeighbourMsg::SurfaceStreamStart { tab } / SurfaceFrame
  on the tab's room topic → direct per-window fanout
  → peer: decoder → peer_view widget draws decoded frames into that tab
```

Raw captures may be coalesced before encoding. Encoded video units preserve
reference order within the existing 64 MiB pending storage budget, including
payload and queue allocations. A fresh keyframe replaces the older pending
chain. After overflow or a failed direct send, dependent units wait for the
next codec-specific keyframe; share encoders already emit periodic keyframes.
Independent pictures keep newest-picture coalescing. Fanout completion carries
a generation, so an old send cannot change a restarted stream's task or queue.

The receiving side can still lose units (a saturated actor queue, a failed
read, the bound on frames held before a stream's start), so each
`SurfaceFrame` carries `seq`, its place in the owner's encoded sequence. The
peer view feeds its decoder only a keyframe or the delta that continues the
chain; after a gap it holds deltas back and asks the owner for a keyframe
(`SurfaceKeyframeRequest`, naming the unit it could not use), and it drops
units older than one it already decoded. The owner forces at most one
keyframe per 250 ms and none for a request a keyframe already answers, then
encodes a frame at once so a still window recovers too. The shell's inbox
keeps every frame in arrival order rather than replacing older ones. The same
rule (`neighbour_wire::unit_chain`) gates the Remote app's workspace-control
decoder, which asks through the consented route, and the relay's browser
bridge, which ends the members' group and asks the session itself.

### Remote input replay
```
Peer pointer/keyboard event on a peer_view widget
  → NeighbourMsg::RemoteInput { surface, event } published
  → owner compositor: smithay Seat::input injects into the foreign client
  → owner can flip per-peer read_only flag to drop events
```

## State model

The single source of truth for per-session shell state is `WorkspaceTree` in `crates/session/shell/src/shell/state.rs`. It uses a **cached materialisation** pattern; what of it is a workspace's shared state and what is the device's own is in `docs/architecture/workspace.md`:

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
└── doc: automerge::AutoCommit          # backing CRDT; not in state.json, saved beside it as state.automerge + a .log of later changes
```

`TileContent` variants with payload encode the discriminant as a string scalar plus a serde-JSON blob in a sibling `Bytes` field — payload merges are last-write-wins for v1.

## E2E test infrastructure

Three layers cover different parts of the stack:

1. **In-process driver** (`crates/session/shell/src/bin/shots/`) — `ShellDriver` builds the same widget tree the live binary builds, runs the same paint pass, and exposes a `widget_registry: HashMap<String, Rect>` that walks the live tree via `Widget::test_id(&self) -> Option<&str>` (from `ui::widget::Widget`). Path-fallback (`Parent/Child[index]`) works without opt-in. Used by >100 tests in `crates/session/shell/tests/*` and by `cargo run --bin shots`.
2. **Offscreen / recording renderer** — `cargo run --bin shots -- --mode offscreen --out target/<dir>` writes PNGs per scenario; `--mode recording --out target/<file>.h264` writes a real H.264 video. Output is identical to the live binary's rendering. **Never** screenshot the user's running session — use the offscreen path.
3. **Control socket** — when the compositor boots with `TILO_TEST_CONTROL=<sock>` set, it binds an extra Unix socket alongside the Wayland one. `tilo-control` uses its newline-delimited JSON protocol to list and activate semantic targets, inject pointer and keyboard input through the real seat, inspect state, and capture snapshots. The implementation lives in `crates/session/shell/src/compositor/test_control.rs`; integration coverage is in `crates/session/shell/tests/control_socket_test.rs`.

The control socket is the out-of-process path for covering the seat layer, frame-pacing regressions, and focus-stealing behaviour the in-process driver can't reproduce.

## Project directory structure (Cargo workspace / monorepo)

```
tilo/                            # workspace root
├── docs/                                # architecture, testing, security, and specifications
│   └── architecture/                    # this directory — product and system design
├── crates/sdk/ui-core/                  # Internal UI toolkit behind tilo-app
├── crates/session/shell/                # compositor + shell
│   └── src/
│       ├── compositor/                  # smithay state, backends, frame, tab-binding, test_control
│       │   └── backend/{winit,drm}/
│       ├── shell/                       # WorkspaceTree, overlays, chrome, network bridge
│       ├── sandbox/                     # bwrap profile, seccomp blob, spawn
│       ├── portal/                      # FileChooser, ScreenCast, Camera D-Bus backends
│       ├── network/                     # Iroh actor, identity, hub, replica, encoder, invite
│       └── bin/{shots,tilo-control}/  # e2e harness + control-socket CLI
├── crates/system/fs/                    # FUSE3 driver
├── apps/{terminal,file-explorer,browser,…}/  # native apps
├── crates/<domain>/                     # supporting libraries (collab, doc, runtime, packaging, …)
└── os/                                  # Containerfile + systemd units
```

Everything lives in this one repository — see `AGENTS.md` for the monorepo workflow.

## Key spatial concepts

- **Strip switcher**: the right-edge strip ribbon, which expands on hover intent (or Super+S) into a card listing the workspace's strips and the workspaces, to switch, add or rename them.
- **Workspace**: a stack of strips that can be shared with peers; the unit of sandboxing, capability grants, and CRDT replication.
- **Strip**: a horizontal lane within a workspace, containing one or more window-groups.
- **WindowGroup**: a tabbed container holding one or more tabs (formerly "deck"). Tracks `maximized` and `width`.
- **Tab**: a single application window. Either `TileContent::Native(...)` (a ui app) or `Tab::Foreign { app_id, surface_id }` (a Wayland client adopted via app-id matching).
