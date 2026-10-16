# Direct scanout of the focused client surface

Goal: a key's commit reaches the panel without the compositor re-rendering the 2944x1840 desktop. Target: client render + one vblank (~2 + 8 avg + scanout, under 20 ms at 60 Hz).

Measured on the key-latency harness (`scripts/perf/headless_key_latency.py`: headless, real GPU, the DRM presenter's renderer drawing into scanout-sized buffers on a simulated 60 Hz vblank, thin-app build, keys typed through the compositor's input funnel as a physical keyboard's arrive, 300 keys per run, p50/p90/p99 ms):

| Client, damage per key | key->present | GPU frame p50 |
|---|---|---|
| wl_shm, whole 1708x1736 window | 50 / 60 / 66 | 33 (window over half the output) |
| wl_shm, one text line | 20-22 / 28 / 30 | 12 (273 KB uploaded: the line and the line the texture missed) |
| LINEAR dma-buf, one text line | 18-21 / 26-27 / 28-30 | 10 |
| LINEAR dma-buf, whole window | 32 / 39 / 41 | 24 |
| wl_shm one line, before a change seen on a tick that could not paint stayed owed | 134 / 144 / 148 | |

One frame per key while typing. With one-line damage the compositor rasterises 82k pixels; what remains is about 10 ms of GPU time per frame for those pixels and the copy into the scanout buffer, and the wait for the next vblank. Direct scanout removes both from the key path.

## 1. What the display hardware offers (read live, read-only)

Queried on a Snapdragon X laptop with a non-master fd on /dev/dri/card1 (GETPLANE, GETPROPERTY, GETPROPBLOB; per-fd UNIVERSAL_PLANES and ATOMIC caps; no commits, no modeset, compositor untouched). Driver `msm_dpu`, `qcom,x1e80100-dpu`, panel eDP-1 2944x1840.

| Fact (measured) | Value |
|---|---|
| Planes | 10: 5 Primary (type 1), 3 Overlay (type 0), 2 Cursor (type 2); every plane `possible_crtcs=0xff` |
| Formats per plane | 41 (AR24 AB24 RA24 BX24 BA24 AR30 XR30 XR24 XB24 RX24 RG24 BG24 ...) with modifier LINEAR (0x0); 9 of them also with 0x0500000000000001 (Qualcomm compressed, UBWC): AR24 AB24 AR30 XR30 XR24 XB24 and 3 more |
| Blending | `alpha` 0-65535 mutable; `pixel blend mode` None / Pre-multiplied / Coverage |
| Stacking | `zpos` mutable range 0-255 |
| Geometry | `rotation` rotate-0/180, reflect-x/y; CRTC_*/SRC_* present (scaling not probed) |
| Sync | `IN_FENCE_FD` and `FB_DAMAGE_CLIPS` present on the planes |
| Active now | plane 50 (primary) 2944 wide on the CRTC; cursor planes 98/104 |

Not verifiable here: how many planes the DPU mixer can actually blend on one CRTC at once (kernel docs for dpu1 say stages are limited and shared between the two active CRTCs), per-plane max size, scaling limits, whether a TEST_ONLY commit with primary + overlay at 2944 wide passes bandwidth checks. Only a TEST_ONLY probe from the compositor answers that (stage 0 below). Async page flip support was not probed.

What the repo does today: the DRM presenter flips with the **legacy** `page_flip` (`drm/scanout.rs:3105`, `:3374`, `:3545`). Atomic is enabled only to run a `TEST_ONLY` commit on the **primary** plane (`drm/client_scanout.rs:295` probe_primary_plane, `:445` test_on_plane). The hardware cursor is the legacy cursor ioctl (`scanout.rs:1488`, `:2130`). A client buffer is scanned out only when it is a *fullscreen* candidate: the frame builder proves one whole-image fill on top of the scene (`shell/scanout_candidate.rs:35` DrawnOnTop, `:145` ScanoutCandidate, `:163` covers = exactly one output, pixel for pixel), the presenter checks rotation, ICC, opacity, SDR, format (`client_scanout.rs:~90` client_scanout_rejection), and the client is told via dma-buf feedback which formats the plane takes (`compositor/scanout_feedback.rs:81` scanout_tranche, commit 626fc6c585). The live log shows `client_scanout_frames=0` (that build predates it, and no client is fullscreen). A tiled window with tab bar, top bar and sidebar around it never qualifies, so every key frame is a full vello repaint.

## 2. Formats, modifiers, buffers

- Tilo apps and Alacritty/Chromium commit dma-bufs from Mesa/turnip. The zero-copy client-buffer work imports tiled and UBWC dma-bufs on the compositor's own device and advertises the modifiers the driver confirmed, with a per-commit foreign acquire. Before it, import was linear only (`sdk/ui-core/src/dmabuf_import.rs`), which the DPU accepts on every plane. Compressed (UBWC) scanout is therefore possible later; **stage 2 starts with LINEAR XRGB8888 only** and adds a modifier only after a TEST_ONLY commit with that exact modifier passed on the running machine.
- ARGB buffers scan out as XRGB only when the surface declared its whole area opaque (`client_scanout.rs:~123` scanout_fourcc, `scanout_feedback.rs` ScanoutOpacity). Alacritty and the Tilo apps declare opaque regions only sometimes; translucent surfaces need an overlay with `Pre-multiplied` blending (hardware supports it), which is an extension, not stage 1.
- `wl_shm` clients (and any buffer not a dma-buf) cannot be scanned out without a copy into a scanout-capable buffer (stage 3).
- Rule: never import or scan out a modifier the driver has not confirmed. Every candidate buffer must pass a TEST_ONLY commit first (existing behaviour, `client_scanout.rs:445`) and a refusal composites instead (`refuse_client_buffer`, `scanout.rs:3530`). Tile overlays must keep that rule, and a GPU firmware (GMU) hang has been seen when GPU work ran concurrently, so tests that touch the real GPU run one at a time.

## 3. A tiled window is a sub-rectangle

The focused tile is surrounded by compositor chrome, so full-screen primary-plane flipping does not apply. Two layouts:

1. **Overlay over the desktop** (preferred): primary plane carries the compositor-rendered desktop at its own cadence, an overlay plane at the tile's rect (zpos above) shows the client buffer. The desktop frame must not draw under the tile (draw nothing or transparent there; the overlay is opaque XRGB so no blending cost). Chrome drawn *on top of* the tile (drag handle, toast, popup, menu, IME popup, notification, peer cursor) is not representable: the existing structural proof already voids a candidate when anything is drawn after the client image, so extending it to "nothing drawn over the tile rect" reuses the mechanism. Rounded corners: the corners are drawn in the desktop layer and the overlay rect is inset by the corner radius, so the client buffer is never required to be square.
2. Primary-only fullscreen (exists) for maximised/fullscreen windows.

Fractional placement, scale != 1 and clipping by the strip viewport (partially scrolled tile) force fallback; a tile scrolling off-screen mid-animation falls back for the duration of the transition.

## 4. Commit -> flip, damage, fallback

Flow for a key: client commits (~2 ms) -> compositor gets the new dma-buf for the focused tile -> if the candidate still holds (nothing drawn over the rect, format/modifier tested, no animation) -> atomic commit on the overlay plane with the new FB_ID (and `IN_FENCE_FD` from the client's release/acquire sync if available, else rely on implicit sync via the dma-buf fences), no desktop render. The primary plane is only re-committed when the desktop itself changed.

- **One flip per CRTC**: the kernel queues one pending commit per CRTC. A key flip and a desktop flip share it; atomic commits can carry both planes in one request. The scheduler must prefer the key's overlay flip and coalesce the desktop (latest wins), keeping the existing in-flight bound (`shell/present.rs:43` QUEUE_DEPTH, `:56` admits) intact. The present thread today renders for ~30 ms per frame; an overlay flip needs no render, so it should bypass the render queue entirely and go through a small flip path with its own retirement (reusing `ClientHold`, `client_scanout.rs:~470`, which holds the buffer from the queueing flip until the replacing flip retires, and `gpu_frames.rs:200-216` hold semantics).
- **Presentation feedback**: the flip's vblank timestamp is the commit's feedback, same as `Retirement.display` for composited frames.
- **Damage**: `FB_DAMAGE_CLIPS` exists on every plane; unnecessary for correctness, only a power optimisation for panel self refresh. The compositor desktop damage work (separate branch) still applies to the primary plane.
- **Fall back to GPU composition when**: anything is drawn over the rect (popups, menus, drag, toasts, overlays), a window transition/animation, HDR layers or an ICC profile on the output, rotated output, scale mismatch or fractional position, a blur/backdrop material touching the rect, screenshot/screencast/capture mirrors and projection (they read the composited scene), multi-output where the rect spans outputs, buffer not dma-buf, format/modifier refused by TEST_ONLY, client buffer larger than the plane limit. Fallback frames composite the same frame; the output only ever shows the frame or the frame before it (existing invariant, module doc of `client_scanout.rs`).
- Switching on/off: a flip from overlay to composited must not tear or flash; the composited frame that takes over is built before the overlay plane is disabled, in the same atomic commit.

## 5. Latency and risk

Expected key->present: ~2 ms client + wait for the next vblank (avg 8.3, max 16.7 at 60 Hz) + scanout ~ **10-20 ms**; p50 under 20 ms is plausible only if the flip path never waits behind a render (above). Async flips (tearing) would cut the vblank wait but the plane's async support was not probed and tearing is a visible regression; leave off. Risks: DPU bandwidth/stage limits (TEST_ONLY decides at runtime), GMU/GPU stability under the new buffer sharing (serialise GPU tests), power (overlay use disables panel self-refresh paths less than continuous full repaints, probably net positive), and correctness when the client buffer is reused before KMS is done with it (the hold logic covers it).

## Staged plan

0. **Probe** (first, as a small logged-only change): on startup, run TEST_ONLY atomic commits primary+overlay at the tile size with a linear XRGB buffer; log plane/zpos/blend results (`direct_scanout_status`-style line). No behaviour change. Measure: log shows pass/fail per output.
1. **Maximised window** via the existing fullscreen path: relax `covers` (`scanout_candidate.rs:163`) to accept a maximised tile that covers the output below a fixed chrome? No: only when chrome is hidden (fullscreen/maximised without bars). Verify `client_scanout_frames>0` and key->present on the harness with `latency_client` fullscreen: expected ~10-20 ms.
2. **Tile-rect overlay** (its own stage with its own tests): the presenter moves from legacy `page_flip` to atomic commits for the whole CRTC, since a legacy flip cannot be mixed with an atomic overlay commit on the same CRTC. The frame-queue invariants stay: `QUEUE_DEPTH` and `admits`, and every submitted frame retires exactly once, in submission order, with its client buffers released exactly once. Atomic commit path for the overlay plane, candidate = "client image drawn at an integer rect with nothing after it", desktop primary drawn with the rect cut out. Harness: GPU frame leaves the key path (no render per key), key->present ~vblank + 2 ms. Tests: fallback on popup, on transition, on capture; buffer hold; TEST_ONLY refusal.
3. **shm / translucent**: CPU copy into a scanout-capable linear buffer per commit (damage-limited) and `Pre-multiplied` overlay blending for translucent surfaces. Measure copy cost at 1708x1736.

## Could not verify

Mixer stage/bandwidth limits and the real max plane width; scaling support; async flip; whether turnip allocates scanout-capable (contiguous/UBWC-free) linear buffers for the client swapchains when asked via the feedback tranche on msm (needs a client run); behaviour of the compositor's legacy flip when mixed with atomic overlay commits on the same CRTC (probably must switch the whole presenter to atomic); the display was reading plane 50 active at probe time, a locked/blanked screen may differ.


Everything is fallback-first: any doubt (a failed TEST_ONLY commit, anything drawn over the rect, a transition, capture) composites the frame instead.
