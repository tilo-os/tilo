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
  rounded 60. Multi-output ticks at the fastest connected mode; each direct
  output renders only once its own previous flip has retired, so a slower
  output skips the frames it could not show (see Direct scanout below).
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

**Vblank-locked scheduling.** While every powered output renders straight
into its own scanout target, the DRM presenter waits for each frame's page
flip and reports the vblank it retired on (`Presenter::display_timing`: the
kernel's `CLOCK_MONOTONIC` flip stamp, the earliest among the outputs that
pace the loop). The native loop's `pacing::VblankScheduler` then re-anchors
the grid on that vblank every frame (`FrameClock::note_retirement`) and makes
the next frame due one *lead* before the following vblank. The lead is the
worst deadline-to-queued-flip cost of the last 64 frames plus 2 ms
(`FrameLead`), clamped to one interval; with no cost known yet the frame is
due at the vblank itself. A frame built at its deadline therefore reaches the
very next vblank: input that arrives just before a build is on the panel one
lead later, and input just after it one interval plus a lead later. A grid
that is locked advances at handoff (`FrameClock::note_submitted`) so a wakeup
between the handoff and the flip is not a due frame; an unchanged frame that
queues no flip leaves it in place. Readback, the CPU renderer, a blank and a
VT switch report no vblank pacing and drop the lock, back to the
retirement-paced grid above. Adaptive sync keeps its content-driven rule.

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

The pipe admits `Presenter::queue_depth` frames beyond the one being
presented: one by default, none while the DRM presenter is vblank-paced. A
frame queued behind one still waiting for its flip would start rendering at
that vblank and be shown an interval later, so on the direct path at most one
frame is in flight. When a frame no longer fits one interval (worst cost plus
margin above it), the loop raises the floor to one queued frame
(`PresentPipe::set_min_queue_depth`) so building overlaps presenting, as
before; `TILO_PRESENT_PIPELINED=1` keeps one queued frame for A/B
measurement. When the pipe is full the paint gate declines to build rather
than grow a queue the display can never drain. A transition's clock is held
only while a *due* frame is refused (`present_hold_credit`); the normal wait
for a vblank-paced flip is animation time.
Resize and DPMS travel the same channel as frames, so a blank cannot
overtake a frame already queued.

Clip-only workspace and overview transitions require a separate presenter
capability. Multi-output DRM GPU readback can retain and replay those frames;
single-output direct/XRGB, CPU, nested and headless paths receive complete scenes.
The native loop refreshes this capability from the current output topology before
each paint, without disabling ordinary retained damage. If an old empty held frame
is still queued when a display is unplugged, the single-output presenter keeps its
shown pixels until the next complete frame instead of rasterising the empty scene.

With `TILO_PROFILE_GPU_STAGES=1`, the existing `FRAME-LIFECYCLE` retirement
record includes the exact pipe serial and scene revision, CPU encode/submission
wall time, GPU completion wait, bump-counter settlement and separate KMS
retirement wait. Direct scanout collects those bridge phases without staging
or reading back pixels. These CPU clock intervals locate blocking work; they
are not GPU timestamp samples or proof of successful physical presentation.
Vello dispatch timestamps additionally require both `VELLO_GPU_TIMESTAMPS=1`
and `VELLO_GPU_DISPATCH_TIMESTAMPS=1` before device/renderer creation. Enable
these fixed options through the greeter's ordinary next-sign-in diagnostics
action. A disabled/unsupported recorder and discarded presentation remain
ineligible for a hardware latency claim.

Native app render workers use those same startup options and the existing
Vello timestamp trackers. `tilo-native-gpu-stages` reports bounded pass and
per-dispatch samples with observer session, worker, worker frame, CPU frame and
Vello recording IDs. A delayed sample retains its originating frame; it never
settles a newer frame. Attempt rows have provisional `missing_current=true`;
only a completed row with `matched_current=true` proves a sample. Unknown,
duplicate or evicted owners are explicitly stale. The final Vello recording
of a frame is observed; backdrop or recovery recordings are not a complete
GPU timeline. Pass spans and dispatch totals overlap and must not be added.

The recorder is disabled unless startup GPU timestamps and native presentation
observation are both enabled. It keeps 32 owners per tracker and emits at most
2048 content-free rows per render-worker lifetime. Existing latest-recording
readback slots can skip a recording while another map is pending. Idle workers
service those callbacks with nonblocking `device.poll(Poll)`, at most 128 polls
of 5 ms per received frame; no synchronous GPU wait, repaint or render request
is introduced. Missing, unsupported, stale or exhausted evidence cannot certify
client shader duration. Startup options require a new renderer; the live
session diagnostic setter does not reconfigure an existing renderer.

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
`TILO_REFRESH_HZ` override. Synchronous presenters still call
`FrameClock::note_presented` at submit; a vblank-paced DRM presenter anchors
the cadence on its flip events instead. `TILO_PROFILE_PRESENTATION=1` adds a
`FRAME-PACING` line every two seconds (lead, queue depth, missed vblanks,
deadline-to-flip cost and deadline-to-vblank p50/p99), and `FRAME-LIFECYCLE`
records carry `vblank_paced` and `flip_to_vblank_us`; `KEY-LATENCY` dates a
frame that queued its own flip at that flip's vblank. Whether this reaches
one interval of compositor input-to-photon on real panels is still to be
measured on hardware.

Standard `wp_presentation` feedback separately binds a client's committed
buffer generation to the pixels drawn by the foreign-surface widget and
then to the exact `PresentPipe` submission serial. On one physical DRM
output, a successful page-flip event for that output's CRTC completes the
feedback with the original kernel timestamp and sequence, provided the
driver advertises monotonic timestamps. Presenter return and ordinary
frame retirement do not prove presentation. Failed capture, uncertain
completion, timeout, missing output, nested/headless backends and ambiguous
multi-output frames discard feedback. Timeout or resetting a still-pending
flip disables physical proof for that scanout: a late event without a
submission cookie must not certify a newer frame. Refresh is unknown when adaptive
sync is active. Feedback does not change the frame clock's cadence policy.

Native protocol tests verify commit/frame ownership and discard paths;
DRM event fixtures verify timestamp preservation and completion guards.
They do not prove Yoga latency. Opt-in `TILO_PROFILE_PRESENTATION=1`
content-free traces preserve the original libinput monotonic event time,
Wayland delivery serial and delivery time for correlation with actual app
state changes and presentation feedback. Synthetic input and repeats lack
an original physical timestamp and cannot qualify as physical latency
samples. Panel timing and latency goals require a real hardware run.

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

Eligibility is decided per output. An output with a normal transform, no
colour transform, a hardware cursor plane, event-driven page flips, and a
renderer on the same PCI function as its scanout card renders direct,
however many other outputs there are: the DRM backend allocates two linear
GBM buffer objects the size of that output (XRGB8888, or a 10-bit or
half-float format in HDR) with `SCANOUT | RENDERING`, registers them as KMS
framebuffers (`add_planar_framebuffer`), and imports them into Vulkan as
render targets (`ui::dmabuf_import`). Vello renders the scene through the
output's `OutputView` (the scene translated to the output's position) into
a retained surface of the output's size, then copies, PQ-encodes or
HDR-composites it into the back slot, and the backend flips to it
(`VelloDrmBridge::paint_output_until`). Every output keeps its own retained
surface and two-slot ring; the renderer and the device are shared. A slot
stays owned by KMS until its page-flip event retires it, so the renderer
never touches the front or flip-pending slot. Page-flip events are drained
once per card and routed to the output whose CRTC they name.

While every output renders direct, no texture spans the desktop: the
bridge's own target is the output at the scene origin, which renders
straight into it, and the others render into their own surfaces, so the
space between outputs of different shapes is never rasterised and two 4K
heads side by side render without banding (banding remains for a single
output past Vello's bin limit). Damage is routed per output: an output none
of the frame's damage falls on is neither rendered nor flipped. A lone
output shares the bridge's target with the scene as a whole, so it keeps
only a frame proven identical to the previous one (empty damage); that
case is pointer motion or typing over an unchanged desktop, and it costs no
GPU work, copy or flip (`keeps_shown_image`). A frame dropped because KMS
reported the flip busy invalidates the output's retained pixels, so the
next frame renders and KMS never keeps showing an older image. An output
whose view is the whole scene while a readback renders the same target
keeps no frame: the readback can bring that target to a revision KMS does
not show on the direct output (`TargetSharing::Readback`).

Outputs are paced apart. A frame retires when the flips of the outputs at
the fastest refresh have (`DrmPresenter::retire_direct_flips`), so the loop
runs at that rate; a slower output's flip retires through the next frame's
non-blocking drain, and until it has, that output skips frames
(`DirectOutcome::Deferred`): it renders at most once per its own vblank.
The damage of a skipped frame is owed to its next render
(`VelloDrmBridge::defer_output`), which stays sparse. A slow output whose
flip has not retired within the retirement timeout is demoted to the
readback.

When an output fails a check (`direct_scanout_rejection` in
`backend/drm/mod.rs` logs the reason as `direct_scanout_status=unavailable
reason=… output=…`), or its direct submission fails, that output alone
falls back to the readback path: render, copy to CPU bytes, write the two
dumb buffers the backend allocates at modeset. One such output renders and
swizzles straight into its dumb buffer; with several outputs the bridge's
target grows to the bounding box of all of them (logged once), which is
rendered once, and only the outputs on the readback receive their sub-rect.
It returns to one output's size once every output renders direct again.

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
| Super+Space               | Search all of Tilo                             |
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

**Hotplug.** Every udev edge re-probes the connectors and reconciles against what is live (`backend::drm::hotplug`): new connectors are modeset, gone ones torn down, re-moded ones rebuilt keeping their id. Configured `x/y`, rotation and mirror topology is reapplied after every change; connectors without saved placement fall back to a gap-free side-by-side layout. The pointer is projected back onto the nearest live output rectangle, never merely into the arrangement's bounding box. Windows are *not* pinned to an output — the shell places groups across whatever displays it is told about — so re-pushing the display geometry re-homes whatever was on the display that went away. Each card is held open on the loop thread for the whole session because libseat refuses a second `open` of the same node. Only startup and udev edges force-probe connectors: a forced probe of every connector, slow on DP-MST docks, runs on the input thread. A display settings change (`display`, `display_transaction`, `display_profiles`) and capability publication re-apply intent to the connectors the last probe found.

**Per-output scale.** Each output's factor is derived from the panel's physical size against its resolution (`backend::drm::scale`), snapped to quarter steps and clamped to 100–200 %, with the `display.scale` setting as an override. Each surface is told the scale of the output it is actually on as its preferred `wp_fractional_scale` — a laptop docked to an external monitor has two right answers.

Multi-monitor support is feature-complete for the shared-workspace-plane model.
Every usable seat GPU is retained once through libseat; its connector scanouts
receive duplicate fds and render their own views of the same scene, so hybrid-GPU docks
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

**Input device configuration.** The persisted `input.*` preferences reach libinput on every `DEVICE_ADDED` and are re-applied to attached devices when the settings file's `input`, `accessibility` or `shortcuts` section changes (`backend::drm::input_config`). The compositor fingerprints each top-level section of the settings file and runs only the work a changed section needs, so a theme or notification toggle neither reconfigures input devices nor touches outputs. Tap-to-click defaults **on** — libinput's default off is right for a tablet on a desk and wrong for a laptop — with disable-while-typing and tap-and-drag turned on alongside it. Keyboard auto-repeat comes from the same `key_repeat` slider.

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
- `crates/session/shell/tests/{compositor_test.rs, interaction_test.rs, control_socket_test.rs}` — the integration coverage.
- `crates/session/shell/examples/drm_backend_boot.rs` — the DRM backend's hardware probe, run from a free TTY; it fails rather than skips when it cannot claim the seat.

### Client close admission

Closing a native client tab sends its ordinary `xdg_toplevel.close` request.
The tab stays in the authoritative tree and remains visible while the client
saves or presents its declared close decision. Only actual surface destruction
removes and persists the tab; a successful close-request receipt does not mean
that the application has exited. Chrome cancels a provisional pill/host fade
when the tree still owns the tab, preserving position, metadata and focus.
Undo history is published only after the captured tabs have actually gone.
The existing separately supervised unconfined host-launch teardown retains its
own scoped lifecycle; native close admission does not add a process kill rule.
