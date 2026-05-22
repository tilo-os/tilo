# Spatial Compositor Design

The compositor is a single Rust binary, `quick-desktop`, that is a real **Wayland server** built on `smithay` 0.6+. It hosts the in-tree shell chrome (top bar, wheel disc, command palette, modals) and third-party Wayland clients adopted into the workspace/strip/group model.

The phased rewrite (`docs/superpowers/plans/2026-05-02-real-wayland-compositor.md` phases 0-7) is complete; this document describes the resulting design.

## Modes and backends

The binary picks a mode and a backend at startup:

- `--mode native` (default since Phase 7) — `quick-desktop` IS the compositor. Hosts a real Wayland socket, accepts third-party clients, paints frames itself.
- `--mode hosted` — runs as a regular `quick-ui` Wayland *client* of an existing display server. Useful for development inside an existing GNOME/Sway session; **not** a separate compositor.

Within `--mode native`, the backend is a trait (`compositor::backend::Backend`) and `--backend` picks the implementation:

- `--backend winit` — smithay's winit backend. Compositor renders into a winit-managed window on top of an existing parent compositor. Used for dev and for the CI integration test.
- `--backend drm` — libseat session + udev device enumeration + libinput for input + DRM/GBM for scanout. The path for booting on a bare TTY via greetd.

The `Backend` trait surface (`quick-shell/src/compositor/backend/mod.rs`) is:

```rust
pub trait Backend {
    fn submit_frame(&mut self, scene: &vello::Scene, output: OutputId);
    fn poll_events(&mut self, callback: impl FnMut(BackendEvent));
    fn outputs(&self) -> &[Output];
}
```

`drm` Phase 5.3 (vello → DMA-BUF direct interop, zero-copy GPU scanout) is deferred — see the compositor plan's Phase 5 notes. The current DRM path renders via wgpu's GL fallback into a GBM buffer.

## Event loop + state

A single `calloop::EventLoop<state::State>` owns everything. The `State` struct (`quick-shell/src/compositor/state.rs`) holds all smithay globals:

```rust
pub struct State {
    pub loop_handle: LoopHandle<'static, Self>,
    pub compositor_state:     CompositorState,
    pub xdg_shell_state:      XdgShellState,
    pub shm_state:            ShmState,
    pub seat_state:           SeatState<Self>,
    pub output_manager_state: OutputManagerState,
    // … per-workspace bridge sockets, shell chrome handle, frame builder, …
}
```

Each protocol handler attaches via `delegate_compositor!`, `delegate_xdg_shell!`, `delegate_shm!`, `delegate_seat!`, `delegate_output!`. The main socket is registered via `ListeningSocketSource`; per-workspace bridge sockets register additional sources on the same loop (see `plan/security.md`).

The Wayland state is decoupled from the graphical backend so the compositor's logic can run in the headless integration test harness (`quick-shell/tests/interaction_test.rs`, `compositor_test.rs`) without a GPU.

## Per-frame paint

The frame builder (`quick-shell/src/compositor/frame.rs`) produces one `vello::Scene` per output per frame:

1. Drive the shell-chrome widget tree (`ShellOverlayHost`) into the scene — top bar, wheel disc, palette, modals.
2. For each foreign Wayland surface visible in the active workspace, look up its committed buffer (stored on the surface user-data in `CompositorHandler::commit`) and draw it into the scene at the tab's content-area rect. SHM buffers are converted ARGB8888 → RGBA8888 and rendered as `peniko::Image`. dmabuf foreign buffers are also supported via wgpu native textures.
3. Submit the scene to the backend; the backend either presents into a winit window (winit backend) or schedules a page flip on the DRM output (drm backend).

The same `FrameBuilder` is used by `cargo run --bin shots -- --mode offscreen|recording` — that's how scenario screenshots and recordings stay byte-identical to the live binary. See `plan/architecture.md` for the e2e infrastructure overview.

## Foreign-client adoption

Foreign Wayland clients are placed in the workspace model via app-id matching (`quick-shell/src/compositor/tab_binding.rs`). Rule:

When `XdgShellHandler::new_toplevel` fires:

1. Read the toplevel's `app-id`. It may not be set yet — wait for the first commit if absent.
2. Search the active workspace's strips for a tab whose `desired_app_id == Some(app_id)`. If found, adopt the surface into that tab.
3. Else, create a new `Tab::Foreign { app_id, surface_id }` in the active group of the active strip.

Tab lifecycle mirrors surface lifecycle:

- `xdg_toplevel.set_app_id` retroactively triggers adoption if the app-id was previously unset.
- `xdg_toplevel.destroy` marks the corresponding tab as `closing` (the existing animation runs); after the animation the tab is reaped.

A CI integration test (`quick-shell/tests/compositor_test.rs`) spawns the compositor with `--backend winit` headless, connects a fake `wayland-client`, opens a toplevel, and asserts the toplevel becomes a tab in the active group; then closes the client and asserts the tab is reaped.

## Spatial constraints (invariants)

These are **rules**, not preferences:

- **2D clusters, no overlap.** Tabs tile within a group; groups tile within a strip; strips stack within a workspace; workspaces tile on the infinite 2D canvas. Any design that reintroduces overlapping floating windows is rejected at review.
- **Active group / active strip are explicit.** New tabs land in `(active_strip, active_group)`. Cross-strip drags update both pointers.
- **Workspace is the boundary for sandboxing and replication.** A surface from sandbox A cannot adopt into a workspace B tab — its bridge tag would mismatch.

## Input

### Local input

Backend events (`BackendEvent::Pointer / Key / …`) flow into `smithay::input::Seat::input`, which dispatches through smithay's focus rules. The compositor adds policy on top:

- Shell-chrome shortcuts (palette open, wheel disc, workspace switching) are intercepted before the seat dispatch.
- Per-workspace input policy (host-priority over guests, per-peer `read_only` flags) is enforced when `RemoteInput` arrives — see `plan/collaboration.md`.

### Test-control input (`NIRIOS_TEST_CONTROL`)

When the env var is set, the compositor binds an additional unix socket alongside the wayland one. Newline-delimited JSON commands dispatch on the calloop main thread via `calloop::ping::Ping`, so seat injection stays single-threaded. Inputs go through `smithay::input::Seat::input` — same focus rules as physical input.

Phase 1 of the spec has landed (`quick-shell/src/compositor/test_control.rs`): the pure-logic dispatcher plus `list_targets` and `click_at`. Later phases (drag, key, type, modifiers, snapshot, state_dump, the `nirios-control` CLI binary, and `tests/support/live_compositor.rs`) are described in `docs/superpowers/specs/2026-05-16-real-input-e2e-design.md`.

### Keyboard shortcuts (host-only)

| Shortcut       | Action                                            |
|----------------|---------------------------------------------------|
| Ctrl+K         | Toggle command palette                            |
| Super+1..9     | Switch active workspace                           |
| Ctrl+Tab       | Switch active strip                               |
| Ctrl+T         | New tab in active group                           |
| Ctrl+W         | Close active tab                                  |
| Super+Esc      | Kill switch — drop peers, revoke caps, lock      |

(Remote-injected keystrokes go through a scrubber that denies system shortcuts. See `plan/security.md`.)

## Outputs

`OutputManagerState` advertises outputs to clients. The winit backend exposes a single output sized to the winit window. The drm backend enumerates DRM connectors via udev, configures their preferred mode, allocates GBM buffers, and registers a page-flip event source on the calloop loop per output.

Multi-monitor support is feature-complete for the single-workspace case; multi-workspace-per-output and per-output workspace pinning are tracked in the foundation spec §4 follow-up.

## Sessions and seat acquisition (DRM mode)

- `libseat` provides the session (TTY swap, DRM master) — works under `seatd`, `logind`, or `consolekit2`.
- `udev` enumerates DRM and input devices and emits hotplug events into calloop.
- `libinput` produces keyboard/pointer/touch events; the seat-state delegate routes them.

Fallback if bare DRM init fails: relaunch under `cage` as a single-window Wayland session. That's a deployment fallback, not a supported mode.

## What's not here

- **Sandboxing protocol details and the per-workspace Wayland bridge socket** — `plan/security.md`.
- **Peer sync, foreign-surface streaming, remote input replay** — `plan/collaboration.md`.
- **xdg-desktop-portal backend** — `plan/security.md` and `quick-shell/src/portal/`.
- **WorkspaceTree CRDT shape** — `plan/architecture.md` plus `docs/superpowers/specs/2026-05-04-workspace-tree-automerge-design.md`.
- **Future**: vello → DMA-BUF direct interop (deferred Phase 5.3), libei input emulation in addition to the control socket, multi-monitor workspace pinning.

## Where to read first

- `quick-shell/src/compositor/{mod.rs, state.rs, native.rs, hosted.rs, frame.rs, tab_binding.rs, test_control.rs}` — the live wiring.
- `quick-shell/src/compositor/backend/{mod.rs, winit.rs, drm/}` — backend implementations.
- `docs/superpowers/plans/2026-05-02-real-wayland-compositor.md` — phase-by-phase implementation log.
- `quick-shell/tests/{compositor_test.rs, interaction_test.rs, drm_backend_boot.rs, control_socket_test.rs}` — the integration coverage.
