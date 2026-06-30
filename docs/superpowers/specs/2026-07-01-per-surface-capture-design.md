# Per-surface Wayland capture — design

Date: 2026-07-01
Branch: `legacy-collab-per-surface-capture`
Status: prototype landed; production wiring partially in place

## Problem

qOS hosts both native qOS apps (using the `ui` toolkit, whose state already
syncs over CRDT + a peer-sync broker) and **legacy Wayland clients** whose
state cannot be replicated CRDT-style. To bring those legacy windows into a
shared workspace the host has to capture each individual client's surface,
encode it as H.264, and stream the bytes to remote workspace members.

The unit of capture is a **single `WlSurface`** (or its toplevel role). The
whole output is the wrong granularity — a workspace's overlapping chrome,
notifications, and unrelated tabs would all leak — and the public
screencast-style protocols are designed for *other* clients (e.g.
OBS) to scrape the compositor. We are the compositor; we have a better
seat.

## Survey of approaches

### A. Direct smithay-internal `WlBuffer` readback (recommended)

The compositor's `CompositorHandler::commit` hook fires every time a client
finishes a frame. At that point the surface's cached state holds a
`BufferAssignment::NewBuffer(WlBuffer)`. For `wl_shm` buffers we already
have `smithay::wayland::shm::with_buffer_contents`, which exposes the
mmap'd pool to a closure under smithay's lifetime guarantees.

For DMA-BUF buffers, smithay surfaces them through
`smithay::wayland::dmabuf::get_dmabuf(buffer)`, which yields a `Dmabuf`
descriptor (fd + format + modifier + plane offsets). The compositor can
then:

  1. Import the dmabuf into an EGL/Vulkan/wgpu context.
  2. Blit it to a host-side staging texture.
  3. Read the staging texture back as RGBA.

Or, when dmabuf imports aren't available, advertise only `wl_shm` formats
and force clients onto the shm fallback (which gtk4, Qt, SDL2, Chromium,
and Electron all already implement).

**Pros:**
  * Already gets called on every commit — no extra protocol round-trip.
  * Zero copies for shm: the readback walks the client's pool directly,
    and `swizzle_row_to_rgba` writes into a vec the encoder consumes.
  * Per-surface by construction. The `SurfaceId` (smithay's
    `surface.id().protocol_id() as u64`) is the keying unit.
  * No new protocol surface for clients to misuse.

**Cons:**
  * Dmabuf path needs a GPU import + readback ring; software path only
    handles shm.
  * Capture cadence is "every commit" — needs the 30-fps throttle (already
    exists: `network::streaming::StreamingThrottle`).

### B. `ext-image-copy-capture-v1` / `ext-image-capture-source-v1`

The newer staging protocol, designed to replace
`wlr-screencopy-unstable-v1`. It targets **outputs** by default, with a
`ext_foreign_toplevel_image_capture_source_manager_v1` variant that
captures a *specific toplevel* via the `ext-foreign-toplevel-list-v1`
handle.

**Pros:**
  * Standard wire protocol — third-party tools (OBS, mumble screen-share)
    can consume the same capture without bespoke integration.
  * Toplevel granularity matches the use case.

**Cons:**
  * Designed for **another client** to drive the compositor — meaning we
    would have to spin up an *internal* client of our own compositor to
    use it. That round-trips bytes through wayland-server's IPC.
  * No DMA-BUF zero-copy path on the consumer side without a custom
    extension; the protocol mostly carries shm or dmabuf-on-the-wire,
    which is an extra import inside our process.
  * Smithay 0.7 does not yet implement this protocol server-side. We'd
    have to implement it ourselves, then *also* implement a client of
    it. That's strictly more code than approach A.

### C. `wlr-screencopy-unstable-v1`

The older wlroots protocol. Captures whole outputs or sub-rectangles.

**Cons:**
  * No per-surface granularity — capture by rectangle, not by surface
    identity. A surface that moves (cluster reflow, window dragged) drifts
    out of the capture rect.
  * Same self-client problem as B.
  * Marked unstable. wlroots itself is moving to `ext-image-copy-capture`.

### D. `xdg-desktop-portal` ScreenCast

What flatpak'd / sandboxed apps see. Designed for *apps* to ask the
compositor to capture, not for the compositor to capture itself. Not
applicable to the host side of the protocol; we already implement the
*server* side of this portal in `portal::serve` (`org.freedesktop.impl.portal.ScreenCast`).

### E. PipeWire client embedding

Spin up a PipeWire node that mirrors the surface to a stream, then read
from PipeWire as a consumer. Adds a daemon dependency, a second IPC layer,
and the same drawbacks as B (need a self-client to drive).

## Recommended approach: A, in two phases

**Phase 1 (shm path):** ship the readback that already exists in
`SurfaceTracker::on_commit` + `buffer_to_rgba`. Advertise only `Argb8888`
and `Xrgb8888` in the compositor's `ShmState::new`, which forces every
client off the dmabuf path onto shm. This is what the prototype
demonstrates and what `network/streaming.rs::encode_tick` already plumbs
end-to-end.

**Phase 2 (dmabuf path):** add a parallel reader that calls
`smithay::wayland::dmabuf::get_dmabuf` when `with_buffer_contents` returns
`NotManaged`, imports the dmabuf into the existing wgpu context the
compositor uses for chrome paint, and reads back via
`wgpu::Buffer::map_async`. Same `SurfaceTracker` egress so the encoder
loop is unchanged.

Approach B remains a useful **fallback** for sharing surfaces to consumers
outside qOS (e.g. an OBS user wanting to record one tab); the underlying
egress is the same encoded stream.

## Pixel format handling

Wayland's pixel-format bytes are little-endian with the format name
reading the high-order byte first:

| `wl_shm::Format` | Wire bytes | Action |
|---|---|---|
| `Argb8888` | `[B, G, R, A]` | Swap B↔R; keep alpha verbatim |
| `Xrgb8888` | `[B, G, R, X]` | Swap B↔R; force alpha to `0xFF` |

`swizzle_row_to_rgba` (in `compositor/surface_readback.rs`) handles both
and returns `UnsupportedFormat` for anything else. The compositor's
`ShmState::new` already advertises **only these two** formats so a
well-behaved client cannot land bytes we can't read.

For **dmabuf** the typical modern formats are `ARGB8888` /
`XRGB8888` / `ABGR8888` / `XBGR8888` plus 10-bit
`ARGB2101010` for HDR. Phase 2 should add a wgpu blit shader that
normalises any incoming format to the encoder's RGBA input.

### Alpha

The H.264 encoder discards alpha (4:2:0 chroma sub-sampling). For shared
toplevels with transparency the alpha is lost; backgrounds will composite
as opaque on the receiver. Acceptable for v1 — transparent panels are
already rare in shared-app scenarios. A v2 could ship a separate alpha
plane (`H.264 alpha extension` or a parallel 1-bit channel).

## H.264 encoding pipeline

Reuses what's already in `shell/src/network/encoder.rs`:

  * `H264Encoder::new(width, height, fps)` constructs an openh264 SVC
    encoder built from source (no external `.so` dependency — bootc is
    immutable, runtime installs of `libopenh264.so.X` are not viable).
  * `H264Encoder::encode(&rgba)` converts RGBA → I420 via
    `openh264::formats::YUVBuffer::from_rgb_source`, then returns the
    Annex-B NALU bytes for one encoded frame. The first call ships the
    SPS + PPS metadata NALs ahead of the first IDR slice.
  * Round-trip quality is measured by the existing
    `gradient_round_trip_psnr_at_least_35_db` test (≥35 dB floor) and
    asserted on the production egress path by
    `shared_foreign_surface_round_trips_rgba`.

The existing recording-mode tooling in `shell/src/bin/shots/mode.rs` uses
the same encoder for scenario-driven video captures; nothing new is
needed for the H.264 side of the prototype.

## Performance budget

Target: **30 fps at 720p**, **< 50 ms** capture-to-encode latency on an
x86 laptop.

Steady-state per-frame cost on the host side:

| Stage | Cost (rough, 720p) | Notes |
|---|---|---|
| `commit` → `on_commit` | < 0.5 ms | hashmap lookup + `with_buffer_contents` |
| `buffer_to_rgba` (shm, 1280×720 ARGB) | ~3–5 ms | row-major swizzle, ~3.7 MB in/out |
| `take_latest` | ~0 ms | `mem::take` swap |
| `H264Encoder::encode` (software openh264) | ~20–30 ms first frame, ~10–15 ms steady-state | I420 conversion + entropy coding |
| Total | ~13–35 ms | well under the 33 ms / 30 fps interval |

Memory: `SurfaceTracker` holds one `Vec<u8>` per shared surface, sized to
`width*height*4`. At 720p that's ~3.7 MB resident per shared surface. With
the 30-fps throttle there is at most **one frame in flight at a time** —
no buffer queue.

When the encoder can't keep up (e.g. during a workspace switch storm),
the throttle silently drops frames at the `should_encode` gate before any
work runs. Backpressure surfaces as `EncodeTickOutcome::Throttled` in the
streaming loop.

## Threading model

Today (and in the prototype): single-threaded encode on the streaming
task's own thread, driven by `encode_tick` per damage event. This is
correct but not optimal — at 720p a 10–30 ms software encode pauses the
streaming task for that interval.

Future move: encode on a dedicated thread.

  * Streaming task posts `(surface_id, Arc<Vec<u8>>, w, h)` to a
    `tokio::sync::mpsc::channel(2)` ("buffer queue depth 2" — one
    in-flight + one pending).
  * Encoder thread blocks on the channel, runs `encoder.encode(...)`,
    forwards NALU bytes back via a separate channel to the gossipsub
    publisher.
  * Channel size 2 caps memory growth at 2 × frame size per surface.
    Older frames are dropped by the throttle before they reach the
    channel; if the channel still fills, drop the newest frame
    (latest-wins) so latency doesn't drift.

The encoder type (`openh264::Encoder` inside `H264Encoder`) is `Send` but
not `Sync`, which is fine — one encoder lives on the encode thread.

## Prototype

`shell/src/bin/per-surface-capture-demo/main.rs` exercises the full
pipeline end-to-end with no external dependencies:

  1. Brings up an in-process wayland-server `Display`, calloop event
     loop, and a `compositor::state::State` (the same code path
     `compositor::native::run` boots).
  2. Binds a fresh `wayland-N` socket and prints its path on stdout so
     a client can be launched with `WAYLAND_DISPLAY=<sock>`.
  3. Forks a Wayland child: the existing
     `shell/src/bin/click_test_client.rs` binary, which renders a
     solid-colour `wl_shm` `Argb8888` toplevel.
  4. Once the child's toplevel is adopted into the workspace tree
     (`tab_binding::adopt_toplevel`), the demo flips its surface into
     `Workspace::shared_surfaces` and registers a `StreamingShare` so
     the `commit` hook stages buffers into `SurfaceTracker`.
  5. The encode loop pulls frames from the tracker via `encode_tick`,
     pipes Annex-B NALUs to `target/per-surface-capture-demo.h264`,
     and runs for 3 seconds.
  6. Tears down the child, drops the demo, writes the file.

`shell/tests/per_surface_capture_test.rs` runs the binary and asserts
the file exists, has nonzero size, and starts with an Annex-B NAL prefix.

## What does NOT work as described in the brief

  * **"Use existing openh264 from shots"** — already used. The prototype
    imports `H264Encoder` directly from `shell::network::encoder`. No
    adaptation was needed.
  * **"Pick smithay-internal vs ext-image-copy-capture"** — the answer is
    smithay-internal; smithay 0.7 does not yet ship
    `ext-image-copy-capture-v1` as a server-side global, and writing it
    plus a self-client is strictly more code than the readback we already
    have.
  * **"Most clients ship ARGB/XRGB via DMA-BUF"** — partially true for
    GPU-accelerated clients (Mesa-backed gtk4 / Chromium). The prototype
    handles only **shm** in v1 because the compositor advertises only shm
    formats. A real legacy-app shoreline must implement the dmabuf path;
    sketched in Phase 2 above but **not** in the prototype.
  * **"Spawn the compositor headlessly (use existing headless backend
    path)"** — the existing `--backend headless` path runs the full
    nirios chrome (`FrameBuilder` + winit-shaped output emulation),
    which is unnecessary for capture-only and adds wgpu adapter
    dependencies. The prototype instead boots a minimal `State` +
    `Display` + `ListeningSocketSource` in-process, the same surface the
    integration test `tests/compositor_test.rs` uses, with no backend
    at all. Per-surface readback fires from `CompositorHandler::commit`
    regardless of whether a backend is attached.

## Open questions / follow-ups

  * **DMA-BUF readback**: requires a wgpu/EGL import path. Smithay's
    `dmabuf::get_dmabuf` is the entry point; wgpu's
    `wgpu::Device::import_external_texture` (via the
    `wgpu-hal` `import_image_handle` low-level API) is the consumer. Open
    question: does the compositor's existing wgpu context's queue handle
    parallel dmabuf imports without stalling the chrome paint, or do we
    need a second device on the encode thread?
  * **Multi-surface fairness**: when N surfaces are shared, the
    `encode_tick` loop today is per-surface independent. With one
    encoder thread per surface, fairness is implicit; with a single
    shared encoder thread we'd want a round-robin or priority scheme so
    one chatty surface doesn't starve the others.
  * **HDR / 10-bit**: out of scope. The encoder is 8-bit 4:2:0.
  * **Hardware encode (VAAPI / NVENC)**: openh264 software keeps the
    test image self-contained, but a daily-driver build needs hardware
    encode to scale beyond 3–4 720p shares. Pluggable encoder behind
    the same `H264Encoder::encode` interface; the streaming task is
    encoder-agnostic.

## File map

| Concern | Code |
|---|---|
| Per-surface buffer readback (shm) | `shell/src/compositor/surface_readback.rs` |
| Commit-hook integration | `shell/src/compositor/state.rs` (`CompositorHandler::commit`) |
| Damage-aware throttle + encode loop | `shell/src/network/streaming.rs` |
| H.264 encode | `shell/src/network/encoder.rs` |
| Prototype binary | `shell/src/bin/per-surface-capture-demo/main.rs` |
| Prototype integration test | `shell/tests/per_surface_capture_test.rs` |
| Pre-existing client fixture | `shell/src/bin/click_test_client.rs` |
