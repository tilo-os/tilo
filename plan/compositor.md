# Spatial Compositor Design

The compositor is a single Rust binary, `shell`, that is a real **Wayland server** built on `smithay` 0.6+. It hosts the in-tree shell chrome (top bar, wheel disc, command palette, modals) and third-party Wayland clients adopted into the workspace/strip/group model.

The phased rewrite (`docs/superpowers/archive/2026-05-02-real-wayland-compositor.md` phases 0-7) is complete; this document describes the resulting design.

## Modes and backends

The binary picks a mode and a backend at startup:

- `--mode native` (default since Phase 7) — `shell` IS the compositor. Hosts a real Wayland socket, accepts third-party clients, paints frames itself.
- `--mode hosted` — runs as a regular `ui` Wayland *client* of an existing display server. Useful for development inside an existing GNOME/Sway session; **not** a separate compositor.

Within `--mode native`, the backend is a trait (`compositor::backend::Backend`) and `--backend` picks the implementation:

- `--backend winit` — smithay's winit backend. Compositor renders into a winit-managed window on top of an existing parent compositor. Used for dev and for the CI integration test.
- `--backend drm` — libseat session + udev device enumeration + libinput for input + DRM/GBM for scanout. The path for booting on a bare TTY via greetd.

The `Backend` trait surface (`shell/src/compositor/backend/mod.rs`) is:

```rust
pub trait Backend {
    fn submit_frame(&mut self, scene: &vello::Scene, output: OutputId);
    fn poll_events(&mut self, callback: impl FnMut(BackendEvent));
    fn outputs(&self) -> &[Output];
    fn set_dpms(&mut self, on: bool) {}
    /// The display's presentation interval — what the loop paces to.
    fn refresh_interval(&self) -> Option<Duration> { None }
    /// Hand the presentation half over to a dedicated render thread.
    fn take_presenter(&mut self) -> Option<Box<dyn Presenter>> { None }
}
```

`drm` Phase 5.3 (vello → DMA-BUF direct interop, zero-copy GPU scanout) is deferred — see the compositor plan's Phase 5 notes. The current DRM path renders via wgpu's GL fallback into a GBM buffer.

## Presentation: cadence and threading

Two modules own the presentation path's timing.

**`compositor::pacing`** — the presentation clock. The loop paces to the
display's real interval, taken from `Backend::refresh_interval`:

- `drm` derives it from the connector's raw mode timing (pixel clock over
  htotal×vtotal), so a 59.94 Hz panel is paced at 59.94 rather than a
  rounded 60. Multi-output paces to the fastest connected mode, because
  the compositor paints one scene across the combined geometry and there
  is only one cadence to pick.
- `winit` reports the host monitor's rate — what a nested client can
  actually present at.
- `headless` reports `None`. It has no display, so it falls back to 60 Hz
  rather than acquiring a fake one; this is the same decoupling that lets
  the Wayland state run without a GPU.

`FrameClock::due` gates the paint (protocol traffic and input wake the
loop off-cadence; presenting then submits frames inside a refresh
interval that the display discards) and `FrameClock::budget` is the event
loop's dispatch timeout — the *remainder* of the interval, so a tick that
worked for 6 ms parks for the other 10.7. Deadlines advance in whole
intervals from the previous deadline, so a late frame doesn't drag the
grid, and a frame that overruns several intervals skips them rather than
queueing a burst.

**`compositor::present`** — presentation off the input thread. Backends
that can be split hand a `Presenter` (their render state) to a
`PresentPipe`, which owns a `qos-present` thread. The seam is the
finished `vello::Scene`: an owned encoding that borrows nothing from the
Automerge document, the widget tree or the text renderer, so the logic
thread keeps sole ownership of the CRDT and `ui`'s "widgets mutate the
document in `handle_event`" model is untouched.

- `winit` and `drm` split. For DRM only the scanouts and the vello bridge
  move; libseat/udev/libinput and the hardware cursor plane stay on the
  loop thread (the cursor owns its own dup of the card fd, so pointer
  motion is now genuinely decoupled from paint).
- `headless` deliberately does not: the test-control `snapshot` command
  reads its rendered bytes back immediately after the submit, and an
  asynchronous present would make every headless harness racy.

The pipe admits one frame beyond the one being presented. When it is full
the paint gate declines to build — capping input-to-photon at two frame
intervals — rather than growing a queue the display can never drain.
Resize and DPMS travel the same channel as frames, so a blank cannot
overtake a frame already queued.

`shell/src/compositor/metrics.rs` instruments all of it
(`QOS_FRAME_TRACE`, `QOS_FRAME_SUMMARY`); `scripts/perf/frame-loop-bench.py`
drives a measured run and diffs two of them.

**Known residual: client dispatch is the next thing on the logic
thread.** With presentation moved off, `dispatch_clients` is the largest
remaining phase there — 16.8% of wall time under a client committing at
163 fps, against 1.9% for scene building and 0.8% for the submit
hand-off. It is also where the pacing tail comes from: on a nested run
at 92.5% on-cadence, the off-cadence spans carry a median 4.1 ms of
client dispatch against 2.5 ms for the on-cadence ones, and the worst
gaps (77 ms, 57 ms, 45 ms) are 23-25 ms of client dispatch each. The
cause is per-commit buffer capture (`foreign::ForeignFrameStore::capture_commit`
copies an SHM buffer, or reads back a dma-buf) running inline on the
logic thread, so a client that commits faster than the compositor
presents makes the compositor late. Moving capture off the logic thread,
or coalescing commits so only the newest buffer per surface is imported
per frame, is the follow-up — and the frame trace already measures it.

**What is verified, and what isn't.** The loop demonstrably holds the
cadence it is configured for — including non-60 rates, via the
`QOS_REFRESH_HZ` override. That is a statement about the *loop*. It is
not a statement about frames landing on a real panel's vblank: nothing in
this design consumes hardware presentation feedback (DRM page-flip
completion timestamps, or a `presentation-time` protocol event from a
nested parent). `FrameClock::note_presented` is called at submit, so the
deadline series is anchored on production, not on photons. Closing that
loop — feeding real flip timestamps back into `note_presented`, which the
clock is already shaped to accept — is the next step, and it needs
hardware to be worth anything.

## Event loop + state

A single `calloop::EventLoop<state::State>` owns everything. The `State` struct (`shell/src/compositor/state.rs`) holds all smithay globals:

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

The Wayland state is decoupled from the graphical backend so the compositor's logic can run in the headless integration test harness (`shell/tests/interaction_test.rs`, `compositor_test.rs`) without a GPU.

## Per-frame paint

The frame builder (`shell/src/compositor/frame.rs`) produces one `vello::Scene` per output per frame:

1. Drive the shell-chrome widget tree (`ShellOverlayHost`) into the scene — top bar, wheel disc, palette, modals.
2. For each foreign Wayland surface visible in the active workspace, look up its committed buffer (stored on the surface user-data in `CompositorHandler::commit`) and draw it into the scene at the tab's content-area rect. SHM buffers are converted ARGB8888 → RGBA8888 and rendered as `peniko::Image`. dmabuf foreign buffers are also supported via wgpu native textures.
3. Hand the scene to the presenter — the render thread where the backend
   supports one, inline otherwise. It either presents into a winit window
   (winit backend) or schedules a page flip on the DRM output (drm backend).

### No fullscreen fast path — none of it exists

**There is no direct scanout in this compositor, in any partial form.** A
fullscreen client pays exactly the same cost as a windowed one: its
buffer is imported to bytes, drawn into the scene as a full-viewport
image, and the whole scene is rendered and presented. If you are here
because you expect a fast path to exist and be disabled, or half-wired,
or gated behind a flag — it is none of those. Nothing was started.

**What is absent, concretely.** There is no dmabuf→KMS import anywhere in
`shell/src/compositor/backend/drm/`. `add_framebuffer` is called in
exactly one place — `scanout.rs`, on the two dumb buffers the backend
allocates for itself at modeset — and nowhere else. The compositor never
hands a client's buffer to KMS. `compositor::gpu_import` does import
client dma-bufs, but via EGL into a GL texture that it *reads back to
CPU bytes*; that is a path to `peniko::ImageBrush`, the opposite
direction from scanout.

**What building it entails.** Importing a client dmabuf as a KMS
framebuffer (`drmModeAddFB2WithModifiers`); negotiating format and
modifier against the primary plane's advertised set, and re-negotiating
when the client's allocation changes; holding the buffer alive and
un-released until the flip that shows it retires, or the client
overwrites a buffer mid-scanout; and a tested fallback for every case
where the plane refuses — wrong format, wrong modifier, scaling needed,
a flip that fails at commit time. It is a project, not a branch in
`submit_frame`.

**No qualification logic is wired either**, deliberately. Deciding
"does this frame qualify for the short route" (one visible surface,
covering the output, no chrome overlay, not locked, no notifications, no
remote cursors) is tractable on its own and tempting to land early. It
was left out because a decision with no route behind it is a switch
wired to nothing: it reads as progress, it accumulates callers, and the
first person to implement scanout has to re-derive whether the existing
predicate was ever right. The decision and the route land together.

The same `FrameBuilder` is used by `cargo run --bin shots -- --mode offscreen|recording` — that's how scenario screenshots and recordings stay byte-identical to the live binary. See `plan/architecture.md` for the e2e infrastructure overview.

## Foreign-client adoption

Foreign Wayland clients are placed in the workspace model via app-id matching (`shell/src/compositor/tab_binding.rs`). Rule:

When `XdgShellHandler::new_toplevel` fires:

1. Read the toplevel's `app-id`. It may not be set yet — wait for the first commit if absent.
2. Search the active workspace's strips for a tab whose `desired_app_id == Some(app_id)`. If found, adopt the surface into that tab.
3. Else, create a new `Tab::Foreign { app_id, surface_id }` in the active group of the active strip.

Tab lifecycle mirrors surface lifecycle:

- `xdg_toplevel.set_app_id` retroactively triggers adoption if the app-id was previously unset.
- `xdg_toplevel.destroy` marks the corresponding tab as `closing` (the existing animation runs); after the animation the tab is reaped.

A CI integration test (`shell/tests/compositor_test.rs`) spawns the compositor with `--backend winit` headless, connects a fake `wayland-client`, opens a toplevel, and asserts the toplevel becomes a tab in the active group; then closes the client and asserts the tab is reaped.

## Spatial constraints (invariants)

These are **rules**, not preferences:

- **`Workspace → Strip → WindowGroup → Tab`, no overlap.** Tabs tile within a group; groups scroll horizontally within a strip (Niri-inspired columnar workflow plus a tab dimension); strips stack within a workspace; workspaces are switched via the wheel disc. The infinite 2-D canvas (Niri-OG X/Y panning) is deliberately **out** — the strip is the navigable unit, not a free 2-D pan surface. Any design that reintroduces overlapping floating windows, or the infinite canvas, is rejected at review.
- **Active group / active strip are explicit.** New tabs land in `(active_strip, active_group)`. Cross-strip drags update both pointers.
- **Workspace is the boundary for sandboxing and replication.** A surface from sandbox A cannot adopt into a workspace B tab — its bridge tag would mismatch.

## Input

### Local input

Backend events (`BackendEvent::Pointer / Key / …`) flow into `smithay::input::Seat::input`, which dispatches through smithay's focus rules. The compositor adds policy on top:

- Shell-chrome shortcuts (palette open, wheel disc, workspace switching) are intercepted before the seat dispatch.
- Per-workspace input policy (host-priority over guests, per-peer `read_only` flags) is enforced when `RemoteInput` arrives — see `plan/collaboration.md`.

### Test-control input (`QOS_TEST_CONTROL`)

When the env var is set, the compositor binds an additional unix socket alongside the wayland one. Newline-delimited JSON commands dispatch on the calloop main thread via `calloop::ping::Ping`, so seat injection stays single-threaded. Inputs go through `smithay::input::Seat::input` — same focus rules as physical input.

Phase 1 of the spec has landed (`shell/src/compositor/test_control.rs`): the pure-logic dispatcher plus `list_targets` and `click_at`. Later phases (drag, key, type, modifiers, snapshot, state_dump, the `qos-control` CLI binary, and `tests/support/live_compositor.rs`) are described in `docs/superpowers/specs/2026-05-16-real-input-e2e-design.md`.

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
- **xdg-desktop-portal backend** — `plan/security.md` and `shell/src/portal/`.
- **WorkspaceTree CRDT shape** — `plan/architecture.md` plus `docs/superpowers/specs/2026-05-04-workspace-tree-automerge-design.md`.
- **Future**: vello → DMA-BUF direct interop (deferred Phase 5.3), libei input emulation in addition to the control socket, multi-monitor workspace pinning.

## Where to read first

- `shell/src/compositor/{mod.rs, state.rs, native.rs, hosted.rs, frame.rs, tab_binding.rs, test_control.rs}` — the live wiring.
- `shell/src/compositor/backend/{mod.rs, winit.rs, drm/}` — backend implementations.
- `docs/superpowers/archive/2026-05-02-real-wayland-compositor.md` — phase-by-phase implementation log.
- `shell/tests/{compositor_test.rs, interaction_test.rs, drm_backend_boot.rs, control_socket_test.rs}` — the integration coverage.
