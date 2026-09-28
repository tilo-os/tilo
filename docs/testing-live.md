# Live-render testing

Hermetic tests prove **layout**, not that a backend works. Two blind spots let
the maps "tiles never load" bug ship green:

- **Unit tests hit a mock.** They fetch from a local/closed HTTP endpoint, so the
  real `network → PNG decode → draw` path was never run.
- **`--shots` renders offline SAMPLE data.** The `--shots` screenshot uses the
  offline app (no worker, no network, stylised vector basemap), so it looks fully
  populated even when live imagery is completely broken.

## The rule

Every app with a **backend** or a **render-critical path** needs, in addition to
its unit tests, both of:

1. **A full-plumbing integration test** on the *real* data path — construct the
   LIVE app (network on, real worker), pump the real frame cycle
   (`sync → build_layout → apply_layout → draw → drain`) until data arrives, and
   assert the production outcome in memory + on disk (not a mock, not sample data).
2. **A live-render screenshot e2e** — render the live app to a PNG after data
   loads and **inspect the pixels**: assert the content area is not the
   placeholder and not a uniform fill, and that it *changed* vs. the pre-load
   frame.

Pixel variety alone cannot tell imagery from a refusal. A provider that stops
serving can still answer every request with a valid, colourful PNG: CARTO's
"API KEY REQUIRED" tile arrived with HTTP 200 and `image/png`, and the maps lane
stayed green while it was on screen. Assert on what the backend actually
returned as well. The maps lane requires the tiles the worker cached for
distinct city-centre coordinates to be distinct images, and the tile layer
records error statuses and non-PNG answers as rejected failures, not as imagery.

Both must remain runnable through a named, documented opt-in lane. Network tests
must use `#[ignore]` rather than returning early: ordinary CI should report that
it did not execute them, never count a no-assertion branch as a passing test.

Service-backed tests are different: an ordinary run must report them as
explicitly ignored, rather than executing an empty test body and reporting a
false pass. Their documented runner must provision the service and select the
ignored tests so missing infrastructure fails closed.

## The reusable harness

`maps/src/livetest.rs` (`maps::livetest`) is the template. It is generic over
`ui::widget::Widget`, so any app can reuse the pattern:

- `Harness::new((w, h))` — one render context (theme, text cache, CRDT doc).
- `Harness::pump_frame(widget, &mut scene)` — one real frame, exactly like
  `ui::App` / `--shots`. `draw` is what issues the real requests; the next
  frame's `sync` drains results. No GPU needed to drive plumbing.
- `pump_frames_until(&mut h, &mut app, max_wait, poll, |a| a.tiles_mem_len() > 0)`
  — loop frames until a predicate holds or it times out.
- `render_live_to_png(&mut h, &mut app, path)` — one frame rasterised on the GPU
  (lavapipe is fine headlessly) → PNG + a `Frame` of RGBA.
- `Frame::{distinct_colors, is_uniform, changed_fraction}` — pixel-content
  assertions: real imagery is varied and differs from the placeholder; a broken
  path leaves a near-uniform placeholder that these catch.

## Running the maps live lane

```bash
# Full plumbing + live-render pixel inspection (needs real network + a GPU).
# Writes maps-live-{before,after}.png to $MAPS_LIVE_OUT (default apps/maps/live-out/).
MAPS_LIVE_OUT=apps/maps/live-out \
  tilo-build test -p maps --test live_maps -- \
  --ignored --nocapture --test-threads 1
```

The lane loads about one viewport of OpenStreetMap Standard tiles per case,
the interactive scale the OSMF tile policy permits. Do not widen it into
area sweeps.

Ordinary `tilo-build test -p maps` compiles these cases but reports them as
ignored. The lane drives the whole app rather than duplicating a weaker raw
`load_tile` network smoke test.

## Running the Chat live-media lane

```bash
# Real iroh endpoints exchange GStreamer-encoded Opus over QUIC.
tilo-build test -p chat --lib two_peers_exchange_encoded_audio_over_quic -- \
  --ignored --nocapture --test-threads 1
```

Ordinary Chat tests report this case as ignored. An explicit run fails if the
network endpoints or required GStreamer Opus elements are unavailable; it does
not convert a missing prerequisite into a passing test.

## Database client live lane

The database client proves its production PostgreSQL and MySQL query,
introspection, truncation, and cancellation paths against disposable services:

```bash
apps/db-client/tests/run-live.sh
```

The runner starts rootless-Podman PostgreSQL 16 and MariaDB 11 containers on
random loopback ports, waits for both servers, selects all four ignored live
tests, and removes the containers on exit. Override the images with
`TILO_DB_TEST_POSTGRES_IMAGE` and `TILO_DB_TEST_MARIADB_IMAGE` when validating a
different server release. Direct execution against existing servers remains
available through the `TILO_DB_TEST_{PG,MYSQL}_*` connection variables described
in `apps/db-client/tests/live_roundtrip.rs`.

## Display configuration live lane

Display unit tests cannot prove that a monitor accepted a KMS mode or that its
ICC calibration is visually correct. Before release, run the compositor on a
TTY and record this matrix:

- internal Intel and AMD panels plus HDMI and DisplayPort sinks;
- mixed refresh (59.94/60/120/144 Hz) and mixed scale;
- all four rotations, including a touchscreen on a 90-degree output;
- extend, mirror, primary switch, disable one output, and dock unplug/replug;
- close/kill Settings during the 15-second confirmation and verify automatic
  restoration; repeat with a deliberately unusable sink/mode;
- select a known test ICC profile and compare a measured color target, then
  enable Night Light and verify calibration remains applied beneath it.

Capture `$XDG_RUNTIME_DIR/tilo/display-state.json`, compositor DRM logs, and a
photo/video of the Keep/Revert path with the report. Do not mark HDR or VRR
working from EDID/kernel capability alone: the lane must also verify HDR DRM
metadata/colorspace plus 10-bit encoding (below), or VRR CRTC activation plus
measured variable presentation.

### HDR content

HDR client surfaces and HDR peer streams are composited at full precision in
linear light and encoded once to the output's PQ signal; an SDR output gets
them tone-mapped. Unit and `it` tests cover the maths and a real-adapter
composite (`hdr_layer_composite`); only a panel shows whether it looks right.
On an HDR monitor (and, for the mixed case, a second SDR one):

1. In Settings → Displays turn HDR on for the HDR monitor. The compositor log
   shows `direct_scanout_status=enabled format=Xbgr2101010` (or
   `Abgr16161616f`); Settings shows no HDR failure reason. Note the SDR
   brightness value.
2. Play an HDR10 clip with a known bright highlight (a 1000 cd/m² master) in
   mpv on Wayland:

   ```bash
   mpv --vo=gpu-next --gpu-api=vulkan --target-colorspace-hint --fs=no clip-hdr10.mkv
   ```

   mpv's stats overlay (`i`) must show the output as PQ / BT.2020, not sRGB.
   Highlights must be clearly brighter than a white Tilo window beside the
   video, while that window stays at its usual brightness. Moving the SDR
   brightness slider must change the desktop and leave the video's
   highlights where they are.
3. With the video playing, open quick settings, press a volume key for the
   OSD and move the pointer over the video: each draws cleanly on top, the
   window's rounded corners stay clean, and the video around them stays HDR.
   Open and close another window over it and switch workspaces: no black
   rectangles at any point of the animations.
4. Take a screenshot and record a short screencast of the playing video: both
   show the video tone-mapped to SDR, neither black nor washed out.
5. Play an HLG clip the same way: its graphics white sits at the desktop's
   white.
6. Mixed setup: plug in the SDR monitor and drag the mpv window from the HDR
   monitor onto it and back. On the SDR monitor the video is tone-mapped, on
   the HDR monitor it is HDR again. (Two outputs use the readback presenter,
   which composites on the CPU; note its CPU load at 4K.)
7. HDR game: run a Vulkan game with an HDR output mode natively on Wayland
   (for a Proton title: `PROTON_ENABLE_WAYLAND=1 PROTON_ENABLE_HDR=1
   DXVK_HDR=1 %command%` on a Mesa with Wayland colour management). The game
   must offer HDR only while every output is HDR. Turn it on, run the game's
   own calibration, and check that its peak is well above SDR white, its
   paper-white/UI slider moves the HUD against the desktop's white, and a
   game using scRGB (the Windows-style linear swapchain) looks the same as
   one using HDR10. Alt-tab to the desktop and back: nothing flashes black.

Record the monitor, GPU, kernel, Mesa and mpv versions, photos of steps 2 and
7, and the compositor log. Known limits of this build: a video in a subsurface
(browsers) shows its tone-mapped rendition, as do HDR surfaces beyond the
third on screen at once and anything under Night Light, a colour filter or
the magnifier; a translucent HDR surface composites as opaque.
