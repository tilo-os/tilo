# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Scope

QuickDesktop is a collaborative, spatial Linux OS built on **Fedora Bootc**. It is a stack, not a single app:

- An immutable container-based OS image
- A custom Wayland compositor with a 2D-cluster tile-first window model
- A P2P FUSE filesystem that projects remote files and hydrates on demand
- A custom CRDT-native UI toolkit used by system apps
- Native system apps (terminal, file explorer, browser)

Treat these as deeply interrelated — a change in `quick-ui` or `quick-fs-fuse` can break the OS image build; changes to the compositor affect sandboxing and collaboration semantics.

## Repository Layout

This is a **Cargo workspace with git submodules**. The workspace members in `Cargo.toml` are:

- `quick-shell/` (submodule, binary `quick-desktop`) — Wayland compositor. Built on `smithay` + `iced` + `libp2p` (gossipsub, mdns). Contains the 2D cluster/sandbox/collaboration logic.
- `quick-fs-fuse/` (submodule) — FUSE3 driver, Iroh-based P2P, mounted at `/var/workspace` with cache at `/var/cache/quick-fs`. 10s metadata TTL, 1MB readahead.
- `quick-ui/` (in-tree) — The UI toolkit. Retained-mode, CRDT-native. Stack: `wgpu` + `vello` (render), `taffy` (layout), `cosmic-text` (text), `automerge` (state), `winit` (windowing), `fluent` (i18n).
- `apps/terminal/` (submodule, binary `quick-terminal`) — GPU-accelerated terminal. Uses `quick-ui`.
- `apps/file-explorer/` (submodule) — Uses `iced` directly (not `quick-ui`).
- `apps/browser/` (submodule, binary `quick-browser`) — Servo-based, uses `iced` (not `quick-ui`).
- `os/` (submodule) — `Containerfile`, `Containerfile.test`, systemd units, build and VM scripts.
- `plan/`, `docs/` — Architecture, collaboration, compositor, security design notes. Read these before non-trivial changes in those areas.

Only `quick-ui` lives in the main repo. Everything else is a submodule — see the "Submodule workflow" section before committing.

## Build & Test

### First time / after pulling

```bash
git submodule update --init --recursive
```

### Building Rust components individually (fast inner loop)

```bash
cargo build -p quick-desktop        # compositor
cargo build -p quick-fs-fuse        # FUSE driver
cargo build -p quick-ui             # UI toolkit
cargo build -p quick-terminal
cargo build -p file-explorer
cargo build -p quick-browser
```

### Running tests

**Prefer targeted tests over workspace-wide runs.** Workspace-wide `cargo test` can take a long time and blocks the shared build lock across worktrees. Run tests for the crate you're changing:

```bash
cargo test -p quick-fs-fuse
cargo test -p quick-ui
```

The compositor has a headless integration test harness that exercises the Wayland state without a GPU:

```bash
cd quick-shell && cargo test --test interaction_test
```

### Running `quick-ui` examples

```bash
cargo run -p quick-ui --example gallery
cargo run -p quick-ui --example responsive_gallery
# Enable debug overlays:
QUICK_UI_DEBUG=1 cargo run -p quick-ui --example gallery
```

### Full OS image

```bash
./os/build.sh            # production image (requires podman)
./os/verify-test.sh      # test image with validation service
./os/test-vm.sh [path/to/disk.qcow2]    # boot it in QEMU (needs OVMF + KVM)
```

`./os/build.sh` compiles each component in release mode, copies the binaries into `os/`, then calls `bootc-image-builder` to produce `os/output*/qcow2/disk.qcow2`. The container build step uses rootless `podman`; the disk image step needs `sudo` for loopback mounts.

**VM login:** `quickuser` / `quickdesktop`. Verify the FUSE mount with `mountpoint /var/workspace`. The test image runs `quick-fs-test.service` on boot — check results with `journalctl -u quick-fs-test`.

## Architecture Essentials

### `quick-ui` — the UI toolkit

Every widget implements the `Widget` trait in `quick-ui/src/widget/mod.rs`. A frame is **five passes**, not one:

1. `sync(state, path)` — pull visual state from the Automerge CRDT document.
2. `build_layout(taffy, theme, node_map)` — build the Taffy tree; widgets that size themselves implement `measure`.
3. `apply_layout(taffy, node, origin)` — taffy's computed layout → widget's `Rect`.
4. `draw(scene, transform, text_renderer, theme)` — render into a Vello `Scene`.
5. `handle_event(event, ...)` — winit events may **mutate the CRDT directly** (e.g. text input splices into an Automerge `Text` object). No message-passing layer.

Key implications when writing or modifying widgets:
- State lives in the CRDT (`State::doc`, an `AutoCommit`), **not** in widget fields. Widget structs hold render/layout caches only.
- Undo/redo is implemented by snapshotting the full CRDT (`State::commit_action`, see `quick-ui/src/state/mod.rs`). Widgets signal "this is an undo point" via `needs_commit()`/`on_commit()`. Ephemeral per-widget state (caret position, selection) is restored across undo via `get_state`/`set_state`.
- Focus is 2D and directional, not tab-order-only. The `App` in `quick-ui/src/app.rs` maintains a `focusable_order` and does spatial navigation for arrow keys.
- Collaboration (`quick-ui/src/collaboration.rs`) is implemented as a Unix-socket protocol to the compositor (`CollabRequest`/`CollabEvent`, serde-encoded). CRDT sync uses `SyncIncremental`. Remote cursors come in as `RemoteCursorMoved` events and render on top of the scene.
- Theming is **OS-controlled**: widgets must read colors/spacing from the `Theme` passed to `draw`, never hard-code. This is how the OS enforces consistent look across system apps.
- i18n via `fluent` — pass translation keys, not literal strings, to user-facing widgets.

### Compositor (`quick-desktop`)

- **2D clusters, no overlap**: windows tile within a cluster; clusters tile on an infinite 2D canvas. Reject any design that reintroduces overlapping floating windows.
- **Per-cluster sandbox**: each cluster is a Bubblewrap sandbox with its own file-access policy. When an app requests a path not in the cluster's allowlist, the compositor prompts the user; approvals can be granted temporarily or permanently to the cluster.
- **Collaboration**: peers can be invited to a specific cluster. Sharing is via Wayland buffer streaming + remote input injection, coordinated over `libp2p` (gossipsub + mdns). Sandboxing is the security boundary — do not weaken it for convenience.
- The Wayland state is decoupled from the graphical Iced layer so it can run headless for integration tests.

### `quick-fs-fuse`

- Mounted at `/var/workspace`. Uses Iroh for content addressing + `iroh-blobs` for transfer; `blake3` hashes; `moka` for caching.
- **Projection vs. hydration**: metadata is visible immediately via P2P gossip; content is fetched on first read. Don't assume `open()` implies bytes are local.
- Cache lives at `/var/cache/quick-fs` so the root can stay read-only under bootc.

### OS image

- Base: `quay.io/fedora/fedora-bootc:42`. The image is immutable; persistent state goes under `/var`.
- Boot chain: UEFI/GRUB → `greetd` (tuigreet) → `quick-fs.service` mounts the FUSE → `quick-desktop` starts as the Wayland session → `quick-desktop-firstboot.service` handles first-run setup.
- `Containerfile.test` adds a validation systemd service; use it instead of shipping test code in the production image.

## Submodule workflow

Everything except `quick-ui` is a submodule. Changes in those directories must be committed **inside** the submodule first, then the pointer bump is committed in the superproject:

```bash
# inside e.g. apps/browser
git add . && git commit -m "..."
cd ../..
git add apps/browser            # updates the submodule pointer
git commit -m "bump browser to <sha>"
```

Don't run `git add -A` from the root — it catches submodule pointer changes you may not have intended. Prefer explicit paths.

Pull all submodules with `git submodule update --remote --merge`.

## Conventions

- **Rust edition:** `quick-ui`, `quick-shell`, `quick-fs-fuse` are on edition **2024**; the apps (`terminal`, `file-explorer`, `browser`) are on **2021**. Don't "normalize" these in passing.
- **Container tooling:** use rootless `podman` (no `sudo`) for image builds where possible. `os/build.sh` only needs `sudo` for the disk-image step.
- **Don't run workspace-wide `cargo test`** unless you genuinely need it — it blocks the shared target lock and is slow. Scope to `-p <crate>`.
- Debug overlays in `quick-ui` are gated by `QUICK_UI_DEBUG` **and** `debug_assertions`; they're unavailable in release builds.

## Where to read first

- `docs/architecture.md` — component overview
- `docs/testing.md` — VM + headless-compositor testing
- `plan/architecture.md`, `plan/collaboration.md`, `plan/compositor.md`, `plan/security.md` — design intent (read before non-trivial changes in those subsystems)
- `DEVELOPMENT.md` — submodule & build basics
- `docs/BRANCHBOX.md` — the feature-worktree workflow (`branchbox feature start …`) if the user is working in that model
