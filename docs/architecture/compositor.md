# Spatial Compositor Design

The compositor is a single Rust binary, `shell`, that is a real **Wayland server** built on `smithay` 0.6+. It hosts the in-tree shell chrome (top bar, wheel disc, command palette, modals) and third-party Wayland clients adopted into the workspace/strip/group model.

## Modes and backends

The binary picks a mode and a backend at startup:

- `--mode native` (default) — `shell` is the compositor. It hosts a Wayland socket, accepts third-party clients, and paints frames itself.
- `--mode hosted` — runs as a regular `ui` Wayland *client* of an existing display server. Useful for development inside an existing GNOME/Sway session; **not** a separate compositor.

Within `--mode native`, the backend is a trait (`compositor::backend::Backend`) and `--backend` picks the implementation:

- `--backend winit` — smithay's winit backend. Compositor renders into a winit-managed window on top of an existing parent compositor. Used for dev and for the CI integration test.
- `--backend drm` — libseat session + udev device enumeration + libinput for input + DRM/GBM for scanout. The path for booting on a bare TTY via greetd.

The `Backend` trait surface (`crates/session/shell/src/compositor/backend/mod.rs`) is:

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

The DRM path currently renders via wgpu's GL fallback into a GBM buffer. Direct vello-to-DMA-BUF interop remains future work.

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
`PresentPipe`, which owns a `tilo-present` thread. The seam is the
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

`crates/session/shell/src/compositor/metrics.rs` instruments all of it
(`TILO_FRAME_TRACE`, `TILO_FRAME_SUMMARY`); `scripts/perf/frame-loop-bench.py`
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
`TILO_REFRESH_HZ` override. That is a statement about the *loop*. It is
not a statement about frames landing on a real panel's vblank: nothing in
this design consumes hardware presentation feedback (DRM page-flip
completion timestamps, or a `presentation-time` protocol event from a
nested parent). `FrameClock::note_presented` is called at submit, so the
deadline series is anchored on production, not on photons. Closing that
loop — feeding real flip timestamps back into `note_presented`, which the
clock is already shaped to accept — is the next step, and it needs
hardware to be worth anything.

## Event loop + state

A single `calloop::EventLoop<state::State>` owns everything. The `State` struct (`crates/session/shell/src/compositor/state.rs`) holds all smithay globals:

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

Each protocol handler attaches via `delegate_compositor!`, `delegate_xdg_shell!`, `delegate_shm!`, `delegate_seat!`, `delegate_output!`. The main socket is registered via `ListeningSocketSource`; per-workspace bridge sockets register additional sources on the same loop (see `docs/architecture/security.md`).

The Wayland state is decoupled from the graphical backend so the compositor's logic can run in the headless integration test harness (`crates/session/shell/tests/interaction_test.rs`, `compositor_test.rs`) without a GPU.

## Per-frame paint

The frame builder (`crates/session/shell/src/compositor/frame.rs`) produces one `vello::Scene` per output per frame:

1. Drive the shell-chrome widget tree (`ShellOverlayHost`) into the scene — top bar, wheel disc, palette, modals.
2. For each foreign Wayland surface visible in the active workspace, look up its committed buffer (stored on the surface user-data in `CompositorHandler::commit`) and draw it into the scene at the tab's content-area rect. SHM buffers are converted ARGB8888 → RGBA8888 and rendered as `peniko::Image`. dmabuf foreign buffers are also supported via wgpu native textures.
3. Hand the scene to the presenter — the render thread where the backend
   supports one, inline otherwise. It either presents into a winit window
   (winit backend) or schedules a page flip on the DRM output (drm backend).

### Composited frames: GBM render targets, dumb-buffer fallback

On a single output with a normal transform, no colour transform, a hardware
cursor plane, event-driven page flips, and a renderer on the same PCI
function as the scanout card, the DRM backend allocates two linear
XRGB8888 GBM buffer objects with `SCANOUT | RENDERING`, registers them as
KMS framebuffers (`add_planar_framebuffer`), and imports them into Vulkan as
render targets (`ui::dmabuf_import`). Vello renders the composited scene
straight into the back slot and the backend flips to it; a slot stays
owned by KMS until its page-flip event retires it, so the renderer never
touches the front or flip-pending slot. When any eligibility check fails
(`direct_scanout_rejection` in `backend/drm/mod.rs` names the reason in
the log), or a direct submission fails, presentation falls back to the
readback path: render, copy to CPU bytes, write the two dumb buffers the
backend allocates at modeset.

### No client fullscreen fast path

These are the compositor's own buffers. **A client's buffer is never
handed to KMS**: a fullscreen client pays exactly the same cost as a
windowed one — its buffer is imported, drawn into the scene as a
full-viewport image, and the whole scene is rendered and presented.
`compositor::gpu_import` imports client dma-bufs via EGL into a GL texture
that it *reads back to CPU bytes*; that is a path to `peniko::ImageBrush`,
the opposite direction from scanout.

**What building it entails.** Importing a client dmabuf as a KMS
framebuffer (`drmModeAddFB2WithModifiers`); negotiating format and
modifier against the primary plane's advertised set, and re-negotiating
when the client's allocation changes; holding the buffer alive and
un-released until the flip that shows it retires, or the client
overwrites a buffer mid-scanout; and a tested fallback for every case
where the plane refuses — wrong format, wrong modifier, scaling needed,
a flip that fails at commit time.

**No client qualification logic is wired either**, deliberately. Deciding
"does this frame qualify for the short route" (one visible surface,
covering the output, no chrome overlay, not locked, no notifications, no
remote cursors) is tractable on its own, but a decision with no route
behind it is a switch wired to nothing. The decision and the route land
together.

The same `FrameBuilder` is used by `cargo run --bin shots -- --mode offscreen|recording` — that's how scenario screenshots and recordings stay byte-identical to the live binary. See `docs/architecture/overview.md` for the e2e infrastructure overview.

## Foreign-client adoption

Foreign Wayland clients are placed in the workspace model via app-id matching (`crates/session/shell/src/compositor/tab_binding.rs`). Rule:

When `XdgShellHandler::new_toplevel` fires:

1. Read the toplevel's `app-id`. It may not be set yet — wait for the first commit if absent.
2. Search the active workspace's strips for a tab whose `desired_app_id == Some(app_id)`. If found, adopt the surface into that tab.
3. Else, create a new `Tab::Foreign { app_id, surface_id }` in the active group of the active strip.

Tab lifecycle mirrors surface lifecycle:

- `xdg_toplevel.set_app_id` retroactively triggers adoption if the app-id was previously unset.
- `xdg_toplevel.destroy` marks the corresponding tab as `closing` (the existing animation runs); after the animation the tab is reaped.

A CI integration test (`crates/session/shell/tests/compositor_test.rs`) spawns the compositor with `--backend winit` headless, connects a fake `wayland-client`, opens a toplevel, and asserts the toplevel becomes a tab in the active group; then closes the client and asserts the tab is reaped.

## Spatial constraints (invariants)

These are **rules**, not preferences:

- **`Workspace → Strip → WindowGroup → Tab`, no overlap.** Tabs tile within a group; groups scroll horizontally within a strip (Niri-inspired columnar workflow plus a tab dimension); strips stack within a workspace; workspaces are switched via the wheel disc. The infinite 2-D canvas (Niri-OG X/Y panning) is deliberately **out** — the strip is the navigable unit, not a free 2-D pan surface. Any design that reintroduces overlapping floating windows, or the infinite canvas, is rejected at review.
- **Active group / active strip are explicit.** New tabs land in `(active_strip, active_group)`. Cross-strip drags update both pointers.
- **Workspace is the boundary for sandboxing and replication.** A surface from sandbox A cannot adopt into a workspace B tab — its bridge tag would mismatch.

## Input

### Local input

Backend events (`BackendEvent::Pointer / Key / …`) flow into `smithay::input::Seat::input`, which dispatches through smithay's focus rules. The compositor adds policy on top:

- Shell-chrome shortcuts (palette open, wheel disc, workspace switching) are intercepted before the seat dispatch.
- Per-workspace input policy (host-priority over guests, per-peer `read_only` flags) is enforced when `RemoteInput` arrives — see `docs/architecture/collaboration.md`.

### Test-control input (`TILO_TEST_CONTROL`)

When the env var is set, the compositor binds an additional unix socket alongside the wayland one. The socket drives the shell as the user, so it is published 0600 (bound in a 0700 staging directory, then renamed into place) and a missing parent is created 0700; anything other than a stale socket at the path is refused. The conventional path, and `tilo-control`'s default, is `$XDG_RUNTIME_DIR/tilo-session/test-control.sock` — never `/tmp`. Newline-delimited JSON commands dispatch on the calloop main thread via `calloop::ping::Ping`, so seat injection stays single-threaded. Inputs go through `smithay::input::Seat::input` — same focus rules as physical input.

`crates/session/shell/src/compositor/test_control.rs` implements semantic target listing and activation, pointer and keyboard input, state inspection, and snapshots. `tilo-control` exposes the protocol as a CLI, and `crates/session/shell/tests/control_socket_test.rs` covers the live socket.

### Keyboard shortcuts (host-only)

| Shortcut                  | Action                                         |
|---------------------------|------------------------------------------------|
| Ctrl+K                    | Search the focused app                         |
| Super+K / Super+Space     | Search all of Tilo                             |
| Super+Page Up / Page Down | Previous / next workspace                      |
| Super+Up / Down           | Previous / next strip                          |
| Super+Shift+Up / Down     | Carry the focused group to the strip above / below |
| Super+Left / Right        | Previous / next window in the strip            |
| Super+1..9                | Focus the Nth window group of the strip        |
| Alt+Tab                   | Most recently used window of the workspace     |
| Ctrl+Tab                  | Next tab in the focused window group           |
| Super+T                   | New tab in the focused window group            |
| Super+W                   | Close the focused tab                          |
| Super+Esc                 | Stop all remote input and end every share      |

The complete map, including the rebindable defaults and the chords Settings
cannot rebind, is `docs/shell-navigation-keyboard-map.md`. How chords, gestures,
pointer controls and overlays fit one model, and how each transition moves, is
[`window-management.md`](window-management.md).

(Remote-injected keystrokes go through a scrubber that denies system shortcuts. See `docs/architecture/security.md`.)

## Outputs

`OutputManagerState` advertises outputs to clients, and `compositor::output_globals` keeps one `wl_output` global per backend output in step with it — created on plug, **updated in place** on a mode or scale change (so a client sees a re-mode rather than a monitor disappearing and a different one appearing), withdrawn on unplug. Matching is by `OutputId`, which the DRM registry mints per `(device, connector)` and keeps stable across re-enumeration.

The winit backend exposes a single output sized to the winit window. The DRM
backend enumerates every seat GPU and connector via udev, configures the saved
or preferred mode, and drives every enabled connector across all retained
cards.

**Hotplug.** Every udev edge re-probes the connectors and reconciles against what is live (`backend::drm::hotplug`): new connectors are modeset, gone ones torn down, re-moded ones rebuilt keeping their id. Configured `x/y`, rotation and mirror topology is reapplied after every change; connectors without saved placement fall back to a gap-free side-by-side layout. The pointer is projected back onto the nearest live output rectangle, never merely into the arrangement's bounding box. Windows are *not* pinned to an output — the shell places groups across whatever displays it is told about — so re-pushing the display geometry re-homes whatever was on the display that went away. Each card is held open on the loop thread for the whole session because libseat refuses a second `open` of the same node.

**Per-output scale.** Each output's factor is derived from the panel's physical size against its resolution (`backend::drm::scale`), snapped to quarter steps and clamped to 100–200 %, with the `display.scale` setting as an override. Each surface is told the scale of the output it is actually on as its preferred `wp_fractional_scale` — a laptop docked to an external monitor has two right answers.

Multi-monitor support is feature-complete for the shared-workspace-plane model.
Every usable seat GPU is retained once through libseat; its connector scanouts
receive duplicate fds and consume the same combined scene, so hybrid-GPU docks
participate in topology, input routing, hotplug, VT resume and presentation.
The cursor is confined to the union of live output rectangles rather than the
combined bounding box, which keeps L-shaped and vertically offset layouts from
creating invisible dead regions. Tilo deliberately does not implement
multi-workspace-per-output or workspace pinning: every display shows the same
workspace, and each holds its own window groups, partitioned per device (see
[`displays-and-infinite-workspaces.md`](displays-and-infinite-workspaces.md)).

## Sessions and seat acquisition (DRM mode)

- `libseat` provides the session (TTY swap, DRM master) — works under `seatd`, `logind`, or `consolekit2`.
- `udev` enumerates DRM and input devices and emits hotplug events into calloop.
- `libinput` produces keyboard/pointer/touch events; the seat-state delegate routes them.

**Seat acquisition failures are diagnosed, not reported as an errno.** libseat collapses "not in a logind session", "this session owns no seat" and "no seat backend is running" into one `ENOSYS`. `backend::drm::seat_diag` classifies the failure against the observed environment (`XDG_SESSION_ID`, `XDG_SEAT`, `XDG_VTNR`, the seatd socket, `/run/systemd/seats`) and prints what to do about it.

**VT switch and resume.** `backend::drm::session` owns the ordered sequence both paths need. Leaving: stop scanout, then drop DRM master — in that order, or the presenter queues flips against a device it no longer owns. Returning: re-acquire master, re-modeset every output, re-bind the cursor planes, restore the panels' power state, resume presenting, force a full repaint. Nothing about a CRTC survives either event, including the in-flight page flip whose completion is thrown away with the master lock.

Resume from sleep runs the *same* sequence, driven off `PowerSignal::ResumedFromSleep`, because a suspend never pauses the seat — a state machine that only acts on transitions does nothing there, which is exactly "lid close, lid open, black screen".

`Ctrl+Alt+F1..F12` is handled by the compositor, ahead of every other chord and never offered to a client. The kernel does not see it: the compositor holds the keyboard in a raw mode, so without this the user cannot leave the terminal for the life of the session.

**Input device configuration.** The persisted `input.*` preferences reach libinput on every `DEVICE_ADDED` and are re-applied to attached devices when the settings file changes (`backend::drm::input_config`). Tap-to-click defaults **on** — libinput's default off is right for a tablet on a desk and wrong for a laptop — with disable-while-typing and tap-and-drag turned on alongside it. Keyboard auto-repeat comes from the same `key_repeat` slider.

**Gestures and touch.** Three- and four-finger swipes and pinches belong to the shell: three fingers pan the strip and step between strips, four switch workspaces, and a pinch opens the overview, in the same directions as the equivalent chords and inverted by `natural_scroll`. One- and two-finger gestures belong to the focused client and go out over `zwp_pointer_gestures_v1`. The split is by finger count, made in one place (`backend::drm::gestures::claims`), so a gesture is never handled twice. Touch reaches clients over `wl_touch` and the shell chrome as synthesised pointer events, since the chrome has no touch path of its own.

Fallback if bare DRM init fails: relaunch under `cage` as a single-window Wayland session. That's a deployment fallback, not a supported mode.

## What's not here

- **Sandboxing protocol details and the per-workspace Wayland bridge socket** — `docs/architecture/security.md`.
- **Peer sync, foreign-surface streaming, remote input replay** — `docs/architecture/collaboration.md`.
- **xdg-desktop-portal backend** — `docs/architecture/security.md` and `crates/session/shell/src/portal/`.
- **WorkspaceTree CRDT shape** — `docs/architecture/overview.md` plus `docs/superpowers/specs/2026-05-04-workspace-tree-automerge-design.md`.
- **Future**: vello → DMA-BUF direct interop and libei input emulation in addition to the control socket.

## Where to read first

- `crates/session/shell/src/compositor/{mod.rs, state.rs, native.rs, hosted.rs, frame.rs, tab_binding.rs, test_control.rs}` — the live wiring.
- `crates/session/shell/src/compositor/backend/{mod.rs, winit.rs, drm/}` — backend implementations.
- `crates/session/shell/tests/{compositor_test.rs, interaction_test.rs, drm_backend_boot.rs, control_socket_test.rs}` — the integration coverage.
