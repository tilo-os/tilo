# Spatial Compositor Design

The compositor is a single Rust binary, `shell`, that is a real **Wayland server** built on `smithay` 0.6+. It hosts the in-tree shell chrome (top bar, strip switcher, command palette, modals) and third-party Wayland clients adopted into the workspace/strip/group model.

## Modes and backends

`shell` is the compositor: it hosts a Wayland socket, accepts third-party clients, and paints frames itself. There is one session; everything it does before its first frame (id salt, state load and migrations, ownership adoption, collaboration broker) lives once in `compositor::session_bootstrap`. The former `--mode hosted` loop, which kept its own drifting copy of that bootstrap, is removed: the binary refuses the flag with exit status 2. Develop inside an existing GNOME/Sway session with `--backend winit`.

The backend is a trait (`compositor::backend::Backend`) and `--backend` picks the implementation:

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

**`shell::present`** — presentation off the input thread. Backends
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
measurement.

A single vblank-paced direct output overlaps rendering with the previous
frame's flip instead (`Presenter::overlaps_presentation`). Its swapchain has
three slots: while KMS shows one and a queued flip owns another, the next
frame renders into the third, then is queued once that flip retired (KMS takes
one flip per CRTC at a time). The presenter returns as soon as the frame is
rendered and its flip queued; the pipe frees capacity then, and publishes the
frame's retirement, physical proof and vblank only when its flip retires
(`Presenter::retire_owed`), in submission order. No frame is admitted behind
one rendering, the render-bound floor included: the GPU is busy either way,
so the frame rendered next is built from the newest state, and at most two
frames are between handoff and the screen (`present::admits`). The
GPU-completion wait stays on the render thread: vello's bump-allocator
counters must be read back before a frame may be shown, and a legacy page
flip takes no in-fence. `TILO_PRESENT_OVERLAP=0` restores waiting for each
frame's own flip, for A/B measurement; `FRAME-LIFECYCLE` reports
`overlapped=` and `retire_wait_us=` (render return to flip retirement), and
`kms_retirement_us=` then counts only the wait for the previous flip.

When the pipe is full the paint gate declines to build rather
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
cause was per-commit buffer capture running inline on the logic thread,
so a client that commits faster than the compositor presents made the
compositor late. Capture now runs on workers, coalesced to the newest
buffer per surface. For a dma-buf no thread waits on the client's GPU:
the commit's buffer is captured once the loop sees its implicit fence
signal, so a slow client delays only its own commits. The GPU orders its
reads after the client's writes as well, whoever imports the buffer: each
commit's import exports the buffer's pending write fence as a sync file
(`DMA_BUF_IOCTL_EXPORT_SYNC_FILE`) and, when it has not signalled yet,
imports it as a semaphore the presenting queue's next submission waits for
(`ui::dmabuf_import::wait_for_writes`, `VK_KHR_external_semaphore_fd`); no
thread waits on it. Vulkan does not follow a dma-buf's implicit fences on
its own, and without this a frame sampled what the buffer held a frame or
two earlier. The shared GPU
route imports each client buffer once and copies nothing at capture: the
presenter draws the frame into Vello's image atlas straight from the
client's buffer (`vello::Renderer::draw_image_from`, a render pass that
also converts `BGRA` and forces `X` alpha), so a client frame costs one
full-frame GPU copy — at 3840×2160, 66 MB of GPU memory traffic instead of
the 133 MB of the earlier copy-then-atlas route.

An SHM commit still copies on the logic thread — the client's pool may only
be read inside Smithay's guarded mapping — but only its buffer damage, when
the surface's previous commit was an SHM buffer of the same size
(`ForeignFrameStore::capture_shm_commit`). Any other commit (a dmabuf, an
unmap, a commit the capture never saw), a failure, unknown damage or a resize
clears that base and the next copy is whole. A damaged copy carries the
generation of the commit it is relative to, and the worker lays it only over
the frame it built for exactly that commit (`build_shm_frame`); against
anything else the copy is dropped and the next one forced whole — never laid
over another frame or over zeros. The worker writes into the frame the
previous one replaced when nothing draws it any more, so a caret blink in a
4K terminal moves kilobytes, not 33 MB; that spare is released once the
worker idles. Queued damaged copies chain rather than replace each other,
and a whole copy supersedes them, which bounds a slow worker's queue.
Published frames are shared `Arc`s: publication swaps one under the
frame-store lock and the tile draws the stored pixels without a copy. What
remains is the CPU swizzle of the damage and Vello's upload of the whole
image into its atlas; the next step is a persistent per-surface GPU texture
that takes only the damaged rows straight from the guarded pool copy.

Drawing from the client's buffer means the compositor decides when it goes
back. Smithay sends `wl_buffer.release` for the replaced buffer the moment
the next commit applies, so the commit handler takes the applied buffer out
of the surface state (`State::hold_drawn_buffer`) and counts it per buffer
(`foreign::hold_buffer`). The frame drawn from it carries the lease; every
frame queued on `PresentPipe` pins the client frames its scenes draw
(`FramePaint::client_frame_pins`) until it was presented. When the store
has replaced the frame and the last pin and readback are gone, a GPU queue
marker is submitted, and `tilo-buffer-retire` sends the release on the loop
once that marker completed — after every render that could read the
buffer. A client therefore gets a buffer back as soon as the frames already
in flight that drew it are presented: with the one-frame `PresentPipe`
queue that is at most about two refresh intervals after its newer commit
applied, so a two-buffer swapchain renders into the other buffer meanwhile
and does not stall. `CLIENT-BUFFER-RELEASE` lines (every 600 releases) give
p50, p99 and maximum of that hold time, from the newer frame replacing the
buffer's frame to the release; `gpu_frames::release_latency()` the same in
process. Not yet measured on DRM hardware: run one 60 fps client in a GPU
`tilo-vm` slot and read those lines (expected p50 ≤ one refresh interval).

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
    // … shell chrome handle, frame builder, …
}
```

Each protocol handler attaches via `delegate_compositor!`, `delegate_xdg_shell!`, `delegate_shm!`, `delegate_seat!`, `delegate_output!`. The main socket is registered via `ListeningSocketSource`; each accepted client is attributed from its peer credentials (see `docs/architecture/security.md`).

The Wayland state is decoupled from the graphical backend so the compositor's logic can run in the headless integration test harness (`crates/session/shell/tests/interaction_test.rs`, `compositor_test.rs`) without a GPU.

## Per-frame paint

The frame builder (`crates/session/shell/src/compositor/frame.rs`) produces one `vello::Scene` per output per frame:

1. Drive the shell-chrome widget tree (`ShellOverlayHost`) into the scene — top bar, strip switcher, palette, modals.
2. For each foreign Wayland surface visible in the active workspace, look up its committed buffer (stored on the surface user-data in `CompositorHandler::commit`) and draw it into the scene at the tab's content-area rect. SHM buffers are converted ARGB8888 → RGBA8888 and rendered as `peniko::Image`. dmabuf foreign buffers are imported on the presenting device (`shell::gpu_frames`), once per client buffer, and drawn into Vello's image atlas straight from the client's buffer, which the compositor holds until the last frame that drew it was presented.
3. Hand the scene to the presenter — the render thread where the backend
   supports one, inline otherwise. It either presents into a winit window
   (winit backend) or schedules a page flip on the DRM output (drm backend).

### Frame damage

`FrameBuilder::paint_with_damage` records the shell in ordered sections and
proves which pixels changed since the previous frame; `None` means "redraw
everything". The top bar and Quick Settings are bounded sections; Quick
Settings also has parts (its panel, the strip ribbon and the strip rail), so
moving focus to another strip repaints the ribbon and the rail rather than
everything between them. The
workspace, its overlays and everything above Quick Settings are split into
keyed **damage parts** (`ui::scene_parts`): a draw site marks a stretch of
commands with a key and the bounds those commands can paint, and every
boundary splits the section's encoded streams into pieces that are compared
with the previous frame's piece by piece. A changed piece of a part damages
that part's old and new bounds; a changed piece outside every part leaves the
frame dense. When the parts nested inside a part differ between the frames (a
menu opened inside an overlay drew a part of its own), the outermost parts
are compared instead, each as a whole. Parts today: each tile's clipped
content, each tab pill, the peers' pointers and follow tethers, the follow
chip, the notification stack, and every overlay above Quick Settings: the
palette's scrim and its card, the tab menu, the sheets and prompts, and the
client surfaces floating over the desktop (popups, floating and fullscreen
windows, the on-screen keyboard). An overlay's part is there whether or not
the overlay is open, so opening, animating or closing it repaints that
overlay; an overlay that cannot bound what it paints claims the whole
desktop. A retained scene carries its parts with it (`scene_parts::record` /
`scene_parts::append`), so an in-process app's own parts (the terminal's
rows and cursor) reach the compositor: typing or a peer's pointer moving
repaints well under 1 % of a 4K output.
The shell's adaptive Area renderer canonicalizes each tile's segment order
before coverage accumulation. Dense and retained repairs then round the same
unchanged geometry to the same pixels despite GPU scheduling. Each successful
repair settles its overflow counters before the next frame; explicit pinned
`VELLO_BUMP_BUDGET=upstream` experiments retain Vello's existing exclusion from
canonical ordering and do not carry this pixel-repeatability guarantee.
An in-process app also hands over its widget damage, as a window of its own
would submit it (`App::surface_frame_with_damage`): its scene is a **hinted**
part (`scene_parts::hinted`) whose hint names the generations it changed
between and where, and its chrome and the layers over its widgets are
window-sized parts. A frame compared with the one that painted the generation
before repaints only that damage (a tooltip repaints its chip); any other
change to the app (it moved, repainted whole, a frame was skipped) repaints
its tile. A tile's edge and the tab list's focus ring are parts too; the
edge's bounds are a square at each rounded corner and a band as thin as the
stroke along each side, so a focus change repaints the outline, not a band as
deep as the corners. Foreign
buffer swaps keep their
client-reported damage, from `wl_surface.damage_buffer` as from
`wl_surface.damage` (scaled to buffer pixels unless a viewport crops or
scales the buffer). A strip pan moves pixels instead of repainting them:
while a strip pans, the producer draws the desktop as layers
(`FramePaint::cache`: what is behind the windows, each strip on its own in
canvas coordinates, the chrome above), in device
pixels, and the DRM GPU presenter keeps the backdrop, the strips and the
chrome in textures of the output's size
(`render/strip_layers.rs`). A pure pan copies the pixels whose strips all
moved by the same distance, rasterises only the columns it uncovered and the
band where a moving strip's shadows meet a strip that stayed, and composites
from the textures, and a layer is rasterised again only when its scene
changes. Each window group of a strip is a damage part of the
strip's drawing (`GroupRow::draw_group_apart`, bounded by its sprites' reach
and the lane's clip), so a strip whose drawing changed is rasterised only
where a changed group shows now (`strip_layers::strip_change`): a window off
screen that changed unseen (an app the canvas draw drew for the first time
presenting its content, an animation that ran on the clock) rasterises none
of the screen, and a strip drawn without well-nested parts is rasterised in
its whole row.
While it pans, a strip draws only the windows the screen shows: a window
wholly off screen replays its last drawing at the same canvas place
(`GroupRow::draw_group_apart`), which no pixel shows, and is drawn afresh in
every frame that shows any of it, so the strip's scene stays byte for byte
the same while only the pan moves (a window carrying a blurred material or a
client's GPU buffer is drawn every frame).
On a scaled display pans step by one device pixel: the drawn pan is the
gesture's rounded to a device pixel (a layout pixel at 100 %, half of one at
200 %, two thirds at 150 %, four fifths at 125 %). A pan then moves strip
pixels by whole pixels without resampling them, as finely as the panel shows,
and the panned frame is what the desktop drawn without layers shows: what
rides the strip moves by exactly the pan, so nothing
lands a fraction of a pixel apart and twitches when the pan ends. Layout and
input use the same drawn pan. A rotated, calibrated or HDR output composites
the layers the same way and then turns, calibrates or PQ-encodes the result
in its final pass, as it would a render of the scene. Frames at rest keep the
screen-space damage above, so the layers fall behind while the desktop
changes at rest; once it has rested a second after such a frame
(`LAYER_WARM_SETTLE`), the loop draws the desktop as layers once
(`FrameBuilder::layers_at_rest`, which is no frame: revisions, damage and the
pans' motion stay as the frames left them) and hands them to the render
thread as idle work (`PresentPipe::warm`). The presenter rasterises only the
layers whose scenes changed into their textures, the strips only where a
changed group shows, the chrome only in the tiles that changed since the
drawing its texture holds (`FrameCache::chrome_damage`, measured from the
previous drawing at rest by the same section damage frames use, so a clock
tick re-warms the clock's tiles), and anything else whole in bands of about
four million pixels; it stops between two bands as soon as a frame is
handed over (`RetainedGpuTarget::warm_layers`); nothing reaches KMS. The next
pan's first frame then copies and composites like any other instead of
rasterising every layer. When a pan ends, the first frame drawn in screen
space again also carries the layers, so it is composited from the textures
that already show it rather than rendered whole, and an unchanged frame kept
after it keeps them current. A blurred surface, an HDR surface or several
displays render the scene as before. Layers on several displays are not
worth it yet: each display draws its own view of the desktop at its own
place and scale, so every display would need its own drawing of each strip
as layers (a strip spans displays as one lane per display, each panned on its
own), its own three output-sized textures (about 100 MB at 4K) and its own
warm, and the frame producer, the planner and the DRM presenter assume one
view throughout (`FrameBuilder::lone_output`, `DrmPresenter::warm`). Until a
measured multi-display pan shows the cost, such a desktop keeps its
screen-space damage: a pan repaints what moved on its display.
Night light and the
colour filters are per-pixel tints drawn into the same scene, so frames under
them stay sparse while the tint is unchanged; the magnifier and the
crosshair, which follow the pointer, keep every frame dense.

### Composited frames: GBM render targets, dumb-buffer fallback

Eligibility is decided per output. An output with a hardware cursor plane,
event-driven page flips, and a renderer on the same PCI function as its
scanout card renders direct, however it is rotated, whatever ICC profile it
has, and however many other outputs there are: the DRM backend allocates two linear
GBM buffer objects the size of that output's mode (XRGB8888, or a 10-bit or
half-float format in HDR) with `SCANOUT | RENDERING`, registers them as KMS
framebuffers (`add_planar_framebuffer`), and imports them into Vulkan as
render targets (`ui::dmabuf_import`). Vello renders the scene through the
output's `OutputView` (the scene translated to the output's position) into
a retained surface of the output's upright size, then copies, PQ-encodes or
HDR-composites it into the back slot, and the backend flips to it
(`VelloDrmBridge::paint_output_until`). A rotated output's final pass reads
each scanout pixel from the surface pixel it shows
(`shell/output_orientation.wgsl`, `shell::render::orientation`): the same
permutation the readback applies on the CPU, so the pixels are identical,
and damage still maps onto the upright surface by translation alone. KMS
plane rotation is not used: the legacy KMS path and linear buffers leave
quarter turns unsupported on most drivers. An ICC-profiled SDR output is
calibrated in the same pass: when the profile is chosen, its qcms transform
(the one the readback runs per pixel on the CPU) is sampled on a 65³ grid of
8-bit codes into a 3D texture (`shell::render::icc_lut`), which the pass
interpolates tetrahedrally per pixel (`shell/output_color_lut.wgsl`). Against
qcms over every sRGB colour that is at most one code value per channel for
profiles that hold sRGB in gamut, 3 for a lut-based profile, and 10 in the
saturated colours qcms clips into a narrower gamut; mean ΔE2000 stays under
0.15. In HDR the profile is ignored, as on the readback: the PQ encode
replaces it. KMS `CTM`/`GAMMA_LUT` are not used either: they express only
matrix-and-curve profiles, and their precision is the driver's. Every output keeps its own retained
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
The phase summary's `readback_bytes` and `cpu_pixel_passes` (passes over an
output's pixels on the CPU: rotate and swizzle or PQ encode, and an ICC
profile's copy and qcms pass) are both zero while every output renders
direct, rotated and ICC-profiled outputs included.

### GPUs that cannot run vello

On a Mali Bifrost (`cpu_present::cpu_render_reason`: vello's compute
pipeline faults under panvk) the scene is rasterized on the CPU
(`tilo_cpu_raster`). Where the GPU's GLES driver comes up on the render
node, frames come as layers on every frame (`FrameBuilder::keep_strip_canvases`,
`LayerPolicy::Always`): `cpu_layers::CpuLayers` keeps the backdrop, the
workspace, each strip in canvas coordinates and the chrome in canvases of
their own, and rasterizes only what changed in them, where it is shown.
`gl_compose::GlCompositor` uploads what changed and composites the layers
and the pointer into GBM scanout buffers (`gl_present`), skipping tiles that
hold no ink; a strip pan only moves where its canvas is read. The present
thread is not realtime there: rasterizing runs on it for longer than a
realtime thread may run without blocking. `GL-PRESENT` lines in the session
log give the frames' layer update, composite and flip wait (p50/p90), and
`CPU-FRAME … layers_px=` what each layer rasterized. `TILO_DRM_GL_COMPOSE=0`
keeps the dumb-buffer copies of one screen canvas.

### Fullscreen client scanout

A fullscreen client whose buffer is the only thing visible on its output is
not composited: the direct path page-flips the client's own dma-buf onto
the primary plane (`backend/drm/client_scanout.rs`). The frame costs no
raster, no copy into a scanout slot and no Vello submission — one flip —
and the client's pixels reach the panel a render earlier.

**Which frames qualify.** The proof is structural, about the scene each
frame actually encodes, so an overlay nobody listed still disqualifies the
frame it appears in:

- The frame builder (`FrameBuilder::paint_with_damage`) records whether the
  fullscreen surface drew exactly one fill of one whole image, outside any
  layer, and whether anything was drawn after it: a popup, the on-screen
  keyboard, night light, a colour filter or the magnifier (which redraws
  every pixel without drawing more) all void it
  (`shell::scanout_candidate::DrawnOnTop`). Through the output's
  magnification the image must land on whole scene pixels at unit scale
  (`pixel_exact_origin`); on several magnified displays their scene
  rectangles must not overlap. The result is `FramePaint::scanout`, with a
  fingerprint of the finished scene: anything the loop appends afterwards
  (a drag, a presentation surface) voids it on that frame
  (`FramePaint::scanout_candidate`).
- Per output, the presenter requires the direct path's own contract
  (hardware cursor plane, renderer on the scanout card) plus: the output is
  upright (a rotated scanout would show the buffer unturned), it has no ICC
  profile (the buffer would skip the calibration the final pass applies),
  the candidate covers exactly this output, it is a
  client dma-buf (`gpu_frames::client_frame`), the buffer is opaque, a flip
  can reach its format from the output's `XRGB8888` targets — any format on
  atomic flips, only `XRGB8888` on legacy flips, which cannot change format
  (see "Page flips" below) — the output is SDR and the frame composites no
  HDR layer (`client_scanout_rejection`).
- An `ARGB8888` buffer counts as opaque when the commit's
  `wl_surface.set_opaque_region` covers the whole surface — what browsers
  and GPU toolkits send for an opaque window, since they allocate `ARGB`
  swapchains regardless. The pre-commit hook judges the region
  (`compositor::opaque_region::covers`, measuring the surface at its
  largest so it errs toward compositing) and the verdict travels with the
  buffer (`foreign::DmabufCommit::opaque`) to `GpuFrame::opaque`. Such a
  frame is drawn ignoring its alpha on every route, exactly as an `X`
  format is, and the presenter makes its framebuffer as `XRGB8888`
  (`client_scanout::scanout_fourcc`; an `ABGR8888` one as `XBGR8888`): the
  same bytes, alpha ignored. A partial region — rounded corners, a shadow —
  or none keeps the buffer composited.
- KMS is asked before anything changes. The buffer must be the mode's size,
  and the primary plane's `IN_FORMATS` must list its format and modifier;
  the buffer becomes a framebuffer (`ADDFB2` with modifiers) and a
  `TEST_ONLY` atomic commit of the very request its flip would make must
  pass (`kms_flip::test_on_plane`). The verdict is remembered per buffer, a
  refusal included, and forgotten when the CRTC is set up again. On legacy
  flips atomic is enabled on the card for that test only (mode
  aspect-ratio flags, which atomic would turn on, are switched back off).

Captures, screencasts and peer streams never read the presenter's output:
they render the scene themselves (`FrameBuilder::capture_scene`), so
scanout changes nothing they see, and capture protection, which only
applies to those copies, needs no check here.

Anything refused composites the same frame, so the output only ever shows
this frame or the one before it. Moving between the two routes is a page
flip at a vblank either way. While an output scans out, the frame's damage
on it is owed to its retained surface (`defer_output`; a lone output's is
invalidated), so the first composited frame after it repaints everything
the client covered. A client that commits nothing new while the desktop
below changes keeps its flip (`DirectOutcome::Kept`).

**Holding the buffer.** A client buffer KMS shows, or has queued, is held
by the output's targets (`ClientHold`): the `GpuFrame` it was imported for
carries the lease that gives the buffer back (see client buffer release
above), and the hold drops it only when the flip that replaces it — the
client's next buffer, or a composited target — retires. Its framebuffer is
removed only after that, never while KMS reads it. A pause for a VT
switch, a demotion and output power-off put the CRTC back on the dumb
front first; the holds go with the targets after that. Scanout flips go
through the same present gate, retirement and vblank pacing as composited
direct frames.

**Page flips.** Every flip the DRM backend makes — the readback ring's dumb
buffers, the direct targets, a client's buffer — goes through one function,
`kms_flip::KmsFlip::queue`, under the presenter's one gated submit body.
When an output is brought up (`DrmScanout::from_card`, after the legacy
modeset) its flips are chosen once: where the driver accepts
`DRM_CLIENT_CAP_ATOMIC` and the CRTC's primary plane and its properties are
found, a flip is a `NONBLOCK | PAGE_FLIP_EVENT` atomic commit of the primary
plane (`FB_ID`, `CRTC_ID`, and `SRC_*`/`CRTC_*` covering the whole mode —
the state the modeset left), never `ALLOW_MODESET` and never `ASYNC`;
otherwise it is a legacy `drmModePageFlip`. Both complete with the same
page-flip event on the card's event stream, so retirement, the present gate
and vblank pacing are the same. What atomic adds is that a flip may change
the framebuffer's format: a fullscreen client's `XBGR8888` buffer (or an
`ABGR8888` one declared opaque) can follow the output's `XRGB8888` targets
and the targets can follow it back, which a legacy flip refuses. The
session log says `kms_flip_api=atomic connector=…` or
`kms_flip_api=legacy reason=… connector=…` once per output.
`TILO_KMS_ATOMIC=0` keeps every flip legacy without touching the cap: the
flips Tilo made before. An atomic flip that fails for anything but a busy
CRTC or lost DRM master logs `kms_flip_api=legacy reason="an atomic flip
failed: …"`, turns that output to legacy flips for good and retries the
frame legacy. Modesets, HDR binding, DPMS, adaptive sync and the cursor
plane stay on their legacy ioctls (`set_crtc`, `SETPROPERTY`,
`set_cursor2`), which the kernel runs as atomic commits on atomic drivers.

The session log says `client_scanout_status=active` when an output starts
scanning a client out and `client_scanout_status=inactive reason=…` when it
stops or a candidate is refused, once per change. The `DRM render phase
summary` counts `client_scanout_frames` beside `direct_frames`.
`TILO_CLIENT_SCANOUT=0` composites every frame, for A/B measurement or a
driver that misbehaves.

**Telling the client (dma-buf feedback).** Whether a client's buffer can be
flipped depends on how it was allocated — format, modifier, and whether the
driver placed it for scanout, which Mesa only does when the compositor's
linux-dmabuf v4 feedback names a tranche flagged `scanout`. Every surface
starts on the default feedback (the render node, the formats the compositor
composites). After each frame is final, the loop hands
`State::update_scanout_feedback` the frame's candidate with the plane of the
output it covers (`compositor::scanout_feedback::offer`, the plane from
`Backend::scanout_plane`). While the candidate is the fullscreen surface and
its latest buffer is opaque (an 8-bit `X` format, or `ARGB`/`ABGR` with a
covering opaque region: `ScanoutOpacity`, recorded by the pre-commit hook),
its feedback gains a tranche flagged `scanout` on the output's KMS device,
before the default tranche. The tranche lists only what the plane scans out
*and* the compositor can import (`scanout_tranche`), so a buffer allocated
from it is still drawn when a popup opens over it. The frame the candidate
is voided — chrome, a popup, the on-screen keyboard, night light — or the
buffer turns translucent, the output has no plane, or the client leaves
fullscreen, the surface is sent the default again; unchanged frames resend
nothing, and a surface that asks for feedback while offered a plane gets it
in its first batch (`DmabufHandler::new_surface_feedback`). The feedback only
steers allocation: each frame's scanout is still the presenter's decision.

The plane comes from the presenter. On every frame whose candidate covers
an output, `refresh_client_offer` (`backend/drm/mod.rs`) checks the
output's own facts with `client_scanout_rejection` (every buffer fact
granted) and, when they allow client scanout, probes the primary plane —
the probe the first client buffer would have made anyway, already made on
atomic flips — and lists the client formats and modifiers a flip can reach
(`client_scanout::client_formats`): on legacy flips those that scan out as
the targets' format (`XRGB8888`, and `ARGB8888` shown as it, on `XRGB8888`
targets), on atomic flips every one the plane scans out (`XBGR8888` and
`ABGR8888` as well, where the plane lists `XBGR8888`). Changed offers are published per output into
`ClientPlanes`, which the loop thread reads; a replugged output starts
without one. The CPU renderer, the readback path, a rotated, calibrated or
HDR output and `TILO_CLIENT_SCANOUT=0` publish none, so headless and
lavapipe sessions keep the default feedback everywhere. The importer accepts
linear buffers only, so the tranche offers linear today.

**Not yet:** windowed video on overlay planes — flips are atomic now, but
assigning a client buffer to an overlay plane also needs a `TEST_ONLY`
commit of the plane assignment every frame, with a fallback to compositing,
and the frame builder to name a windowed candidate. On legacy flips
(`TILO_KMS_ATOMIC=0`, or a driver without atomic) `XBGR8888`/`ABGR8888`
still composite on `XRGB8888` targets. Tiled and compressed modifiers in the
scanout tranche need the importer to accept them first.

**Verifying on hardware.** CI's llvmpipe cannot exercise KMS planes; the
decision logic is unit-tested (`scanout_candidate`, `client_scanout`, the
fullscreen phases in `shell::frame`). On a `tilo-vm` slot with a real GPU —
a `device` target (`tilo-vm vm add`) or a VM with the GPU passed through:

1. `tilo-vm swap shell` from the branch, then play a 4K60 video fullscreen
   in an opaque client (`mpv --fs`, or `vkcube` made fullscreen). With
   `WAYLAND_DEBUG=1` the client's log shows a second
   `zwp_linux_dmabuf_feedback_v1` batch once it is fullscreen, whose first
   tranche has `tranche_flags 1` (scanout) and the card's `dev_t` as
   `tranche_target_device`; it gets a batch without it when an overlay
   appears.
2. `tilo-vm logs -n 400` shows `client_scanout_status=active`, and the
   phase summaries count `client_scanout_frames` at the refresh rate while
   `direct_frames` stays near zero. `intel_gpu_top` (or `radeontop`) shows
   the render engine idle apart from the video decoder.
3. Raise a notification or the volume OSD over it: the log shows
   `inactive reason="no fullscreen client on top"`, the overlay appears on
   that frame without a flash, and scanout resumes when it is gone.
4. Switch VT away and back: no page-flip `EACCES` in the log, and scanout
   resumes.
5. `CLIENT-BUFFER-RELEASE` lines keep p50 within about one refresh
   interval: buffers go back as their replacing flip retires.
6. Repeat with `TILO_CLIENT_SCANOUT=0` for the composited baseline.

Atomic page flips need the same GPU slot: a headful slot's vkms card has a
`Virtual` connector, which presents without page flips, so its log says
`kms_flip_api=legacy reason="the connector presents without page flips"`
and proves nothing about flips.

1. `tilo-vm logs -n 400 | grep kms_flip_api` shows
   `kms_flip_api=atomic connector=…` once per output, and no
   `an atomic flip failed` line during the session.
2. Animate the desktop (open the overview, switch workspaces back and
   forth): no tearing, no flash, and the `DRM render phase summary` and
   `FRAME-LIFECYCLE` lines show the same `direct_frames` rate and
   `kms_retirement_us` as on `main`; `scripts/perf/frame-loop-bench.py
   --backend drm` and `scripts/perf/device_latency_bench.py`
   (`hw.workspace_switch.*`) do not regress.
3. A fullscreen client allocating the `B` order — `vkcube` made fullscreen
   on a driver whose swapchain is `B8G8R8A8`, or `mpv --gpu-api=vulkan
   --fs` — reaches `client_scanout_status=active` (on `main` it is refused
   with `the client's buffer format differs from the output's`); its
   `WAYLAND_DEBUG=1` scanout tranche lists `XBGR8888`/`ABGR8888`. Raising
   the volume OSD over it flips back to the `XRGB8888` targets on that frame
   without a flash, and scanout resumes when it is gone.
4. Switch VT away and back with the client fullscreen: no page-flip
   `EACCES`, still `kms_flip_api=atomic` (a lost master never demotes).
5. Restart the session with `TILO_KMS_ATOMIC=0`: the log says
   `kms_flip_api=legacy reason="forced by TILO_KMS_ATOMIC=0"`, the `B`-order
   client is refused as on `main`, and everything else behaves as in steps
   1–4: the switch-off a driver problem needs, without a rebuild.

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

- **`Workspace → Strip → WindowGroup → Tab`, no overlap.** Tabs tile within a group; groups scroll horizontally within a strip (Niri-inspired columnar workflow plus a tab dimension); strips stack within a workspace; workspaces are switched from the strip switcher. The infinite 2-D canvas (Niri-OG X/Y panning) is deliberately **out** — the strip is the navigable unit, not a free 2-D pan surface. Any design that reintroduces overlapping floating windows, or the infinite canvas, is rejected at review.
- **Active group / active strip are explicit.** New tabs land in `(active_strip, active_group)`. Cross-strip drags update both pointers.
- **Workspace is the boundary for sandboxing and replication.** A surface from sandbox A cannot adopt into a workspace B tab — its bridge tag would mismatch.

## Input

### Local input

Backend events (`BackendEvent::Pointer / Key / …`) flow into `smithay::input::Seat::input`, which dispatches through smithay's focus rules. The compositor adds policy on top:

- Shell-chrome shortcuts (palette open, strip switcher, workspace switching) are intercepted before the seat dispatch.
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
| Super+Up / Down           | Maximize / restore the focused window group    |
| Ctrl+Super+Up / Down      | Strip above / below                            |
| Shift+Ctrl+Super+Up / Down | Carry the focused group to the strip above / below |
| Super+1..9                | Go to the Nth strip from the top               |
| Super+Left / Right        | Previous / next window in the strip            |
| Ctrl+Super+1..9           | Focus the Nth window group of the strip        |
| Alt+Tab                   | Most recently used window of the workspace     |
| Super+Ctrl+Tab            | Next tab in the focused window group (Ctrl+Tab too, where the app leaves it unhandled) |
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
mode, and drives every enabled connector across all retained cards.

**Default mode.** Without a saved mode (Settings, or the `display.refresh-rate`
action) a connector runs at its native resolution, the size of its PREFERRED
mode, at the fastest progressive rate it offers there
(`backend::drm::devices::select_mode`). Panels flag their 60 Hz timing
PREFERRED and list 90 or 120 Hz beside it, so the flagged mode alone would cap
a fast panel at 60 Hz. A faster rate at a different resolution never wins. The
frame clock paces to the chosen mode's exact timing, and with adaptive sync
that rate is the ceiling.

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

**Several GPUs.** A hybrid laptop's panel usually hangs off the integrated GPU
while its HDMI or DisplayPort outputs are wired to the discrete one, so no
single card can light every display. At startup the backend opens every KMS
card on the seat (ranked by connected outputs, then a connected built-in
panel: `backend::drm::devices`), probes all their connectors before choosing
configuration keys (two cards may both have an `HDMI-A-1`), and brings up
every enabled connector on its own card. A card that fails to open, or one
without connected outputs, costs nothing to the others; a connectorless card
stays held for a later hotplug. Only when no card lights anything does the
backend fail (`DrmBackendError::NoScanout`), so the display manager takes the
seat back instead of the session running behind its last framebuffer. The
layout does not depend on which card is first: an unarranged set puts the
built-in panel first and leftmost and makes it primary.

Rendering happens once, on the renderer's GPU. An output on that GPU's card
renders direct into its own GBM targets (above); an output on another card
presents by readback: the frame is copied to the CPU and written into that
card's dumb buffers, a card-local allocation every KMS driver can scan out.
There is no cross-device DMA-BUF import, so no buffer placement or format
negotiation between vendors can fail at flip time.

Each attempt is journaled on the session log: every card tried
(`tilo compositor: trying KMS scanout device ...`), every connector attempt
(`DRM connector attempt card=... connector=... result=lit|disabled|failed`),
and the topology driven, written whenever it changes
(`DRM topology cards=N driven_cards=N outputs=N; card ... outputs=N; output
ID CONNECTOR on CARD WxH@RATE at X,Y scale S`, `backend::drm::topology`).
While another session holds the seat no card has DRM master, so hotplug
edges then are left for re-activation (a VT switch back, waking from sleep),
which probes every card again. The host-session VM test boots with two
virtio-gpu cards and requires an output on each (`dist/session/README.md`).

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

- **Sandboxing protocol details and Wayland client attribution** — `docs/architecture/security.md`.
- **Peer sync, foreign-surface streaming, remote input replay** — `docs/architecture/collaboration.md`.
- **xdg-desktop-portal backend** — `docs/architecture/security.md` and `crates/session/shell/src/portal/`.
- **WorkspaceTree CRDT shape** — `docs/architecture/overview.md` plus `docs/architecture/workspace.md`.
- **Future**: vello → DMA-BUF direct interop and libei input emulation in addition to the control socket.

## Where to read first

- `crates/session/shell/src/compositor/{mod.rs, state.rs, native.rs, session_bootstrap.rs, frame.rs, tab_binding.rs, test_control.rs}` — the live wiring.
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
