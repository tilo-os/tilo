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

Both must be **runnable, not `#[ignore]`d into oblivion.** Gate them behind an
env var so hermetic CI self-skips (green) offline, and give them a clear name +
documented run command.

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
MAPS_LIVE=1 MAPS_LIVE_OUT=apps/maps/live-out \
  qos-build test -p maps --test live_maps -- --nocapture --test-threads 1
```

Without `MAPS_LIVE`, the lane self-skips and `qos-build test -p maps` stays green
offline. The pre-existing `tiles::tests::live_tile_fetch_real_network` (an
`#[ignore]`d raw-fetch smoke test) still exists; the `live_maps` lane supersedes
it by driving the whole app, not just `load_tile`.

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

Capture `$XDG_RUNTIME_DIR/qos/display-state.json`, compositor DRM logs, and a
photo/video of the Keep/Revert path with the report. Do not mark HDR or VRR
available from EDID/kernel capability alone: the lane must also verify HDR DRM
metadata/colorspace plus 10-bit encoding, or VRR CRTC activation plus measured
variable presentation. The XRGB8888 presenter currently leaves both controls
unavailable by design.
