# UI performance benchmark

This executable benchmark is the single source of truth for the retained UI
framework. A performance change is successful only when the measured budgets
improve without breaking pixel equivalence and framework tests. Presentation
cadence still has to be checked on the named physical display because an
offscreen VM cannot measure its compositor.

See [Performance controls](performance-flags.md) for rendering and benchmark
flags, defaults, value parsing, dependencies, and diagnostic caveats. Historic
measurements belong in version control; they are not evidence for the current
checkout.

Current results come from the scheduled performance lane, not from this page.
Each benchmark writes records with `tools/perf/record.py`, and the records are
stored in the `tilo-perf-results` package. See
[Performance results](measurements/performance.md) for how a benchmark
such as `ui-perf` or `redraw_bench` joins that lane.

## Where this runs

`scripts/perf/render_bench.py` runs both harnesses the same way in each lane.
Every lane starts with the pixel-equivalence preflight below, and a lane whose
preflight fails records nothing.

| Lane | When | What it checks |
|---|---|---|
| **CI / render counters** (`ci-render-counters.yml`) | Changes to `crates/sdk/ui-core`, the vendored renderer stack (`vello*`, `wgpu*`, `cosmic-text`) or this harness. It is not a required check. | `render_bench.py counters`: the 4K redraw gate's allocator calls and bytes per redraw and its raster pixels per redraw (200 events), and the no-redraw gate's allocations per 1,000 events (100,000 events). It uses the budgets below and never compares a clock. |
| **Perf / scheduled**, group `render` (`perf-scheduled.yml`) | Nightly, and by dispatch with `kpis=render` | `render_bench.py timing`: `render.ui.frame_us.4k` (redrawing frames of 1,000 dashboard events) and `render.shell.redraw_us.4k` (the 1,000 `redraw_bench` clicks), then the same counters. |

Both lanes run on the shared `runner` class, which has no GPU. Lavapipe
rasterises on the CPU, so the nightly records carry `renderer=lavapipe` and are
compared only with each other. They follow regressions and never support a
4K/144 Hz claim. `redraw_bench`'s own mean and p99 gate is a hardware
threshold: the nightly lane records its outcome in each record's `meta.gate`
and does not fail on it. The shell's runner-independent render counters (pixels
repainted per keyed scene part, GPU copies and readback bytes per client frame)
are `shell` package tests, which run on every change to `shell`.

## Acceptance commands

Build once so every run measures the same binary:

```sh
cargo build -p ui-examples --release --example ui-perf
```

Native 4K animated redraw and resource gate:

```sh
TILO_UI_DASH_ITERS=1000 \
TILO_UI_DASH_WIDTH=3840 \
TILO_UI_DASH_HEIGHT=2160 \
TILO_UI_DASH_ALLOC_CALL_BUDGET_PER_REDRAW=650 \
TILO_UI_DASH_ALLOC_BYTE_BUDGET_PER_REDRAW=70000 \
TILO_UI_DASH_RASTER_PIXEL_BUDGET=150000 \
TILO_UI_DASH_GPU_BUDGET_BYTES=65000000 \
TILO_UI_DASH_GPU_GROWTH_BUDGET_BYTES=4194304 \
TILO_UI_DASH_RSS_BUDGET_KIB=350000 \
TILO_UI_DASH_RSS_GROWTH_BUDGET_KIB=32768 \
target/release/examples/ui-perf dashboard_perf_bench
```

The harness prints `output target: 3840x2160 (native; 1:1 damage raster, no
upscale)`. The retained output texture is 33,177,600 bytes, exactly
`3840 * 2160 * 4`. Partial rendering changes how many native pixels are
rasterized; it does not reduce the target resolution.

The growth baseline is captured after warm-up and after the timing histogram
has been materialized and page-touched. Consequently, RSS growth does not
misclassify benchmark storage as framework retention. The raster-pixel gate
also makes a regression from damage rendering back to an 8,294,400-pixel full
frame fail independently of timing noise.

The redraw allocation budgets enable phase attribution automatically and fail
if total allocator traffic exceeds either rate. To print the same phase table
without enforcing a budget, set `TILO_UI_DASH_ALLOC_PROFILE=1` together with
`TILO_UI_DASH_ALLOC_BREAKDOWN=1`. The no-redraw gate deliberately omits phase
snapshots so profiler bookkeeping does not contaminate its event timings.

One-million-event no-redraw CPU/allocation gate:

```sh
TILO_UI_DASH_ITERS=1000000 \
TILO_UI_DASH_STATIC=1 \
TILO_UI_DASH_FROZEN=1 \
TILO_UI_DASH_EVENT_BUDGET_US=3 \
TILO_UI_DASH_ALLOC_BUDGET_PER_1000=1 \
target/release/examples/ui-perf dashboard_perf_bench
```

Pixel-equivalence preflight (separate from performance measurements):

```sh
TILO_UI_DASH_VERIFY=1 \
TILO_UI_DASH_VERIFY_FRAMES=40 \
TILO_UI_DASH_ITERS=40 \
TILO_UI_DASH_WIDTH=3840 \
TILO_UI_DASH_HEIGHT=2160 \
target/release/examples/ui-perf dashboard_perf_bench
```

This compares each submitted retained frame against a full render of the exact
same scene, allowing at most one channel value of antialiasing variation. All
40 frames must match. Verification adds native-size reference textures and
readbacks, suppresses performance reports, and rejects benchmark budget flags;
its runtime and memory are not performance samples.

Current requested native-4K gate (1,000 redraws, p99 at most 30,000 microseconds,
with default renderer options and no optimization environment overrides):

```sh
TILO_UI_DASH_ITERS=1000 \
TILO_UI_DASH_WIDTH=3840 \
TILO_UI_DASH_HEIGHT=2160 \
TILO_UI_DASH_FRAME_BUDGET_US=30000 \
target/release/examples/ui-perf dashboard_perf_bench
```

The current executable shell redraw gate is mean at most 25,000 microseconds
and p99 at most 50,000 microseconds. Its
production render/readback harness is:

```sh
cargo build -p shell --release --example redraw_bench --locked
target/release/examples/redraw_bench
```

It drives 1,000 clicks through `FrameBuilder` at native 3840x2160, including
input, layout, scene paint, DRM-bridge GPU completion, damage-aware texture
readback (with full-frame fallback), and XRGB conversion. The destination is a
reused CPU allocation, not a real KMS dumb-buffer mapping; physical page flip
and display scanout are not timed.
These current gates do not establish constant 60 fps; older 8,000 us mean and
30,000 us p99 targets are not the executable's present thresholds. The older
`crates/session/shell/examples/draw_bench.rs` measures CPU scene painting only and cannot
establish this target. The UI harness also accepts
`TILO_UI_DASH_MEAN_FRAME_BUDGET_US` as an optional exact-mean gate; that is a
different workload, not a replacement for the shell measurement.

For a separate shell diagnostic, run the same release binary with
`TILO_REDRAW_BENCH_PROFILE=1`. It still performs all 1,000 native-4K action
redraws, but reports input, registry, layout, scene encoding, submission,
render-completion/readback mapping, settlement, and CPU copy/swizzle times.
The completion/map phase is **not** a copy-only measurement. Copy counters
report pixel payload bytes per submitted attempt (including recovery) and
bytes actually written to the CPU destination, excluding row-padding holes.
The bridge also prints the adapter it actually opened. Diagnostic runs
deliberately return a nonzero exit status and cannot pass acceptance gates;
unset the profiling variable for acceptance measurements.

The earlier, stricter 4K/60 frame gate remains available:

```sh
TILO_UI_DASH_ITERS=1000 \
TILO_UI_DASH_WIDTH=3840 \
TILO_UI_DASH_HEIGHT=2160 \
TILO_UI_DASH_FRAME_BUDGET_US=16667 \
target/release/examples/ui-perf dashboard_perf_bench
```

The last command exits with status 2 unless redrawing-frame p99 is at most
16,667 microseconds. End-to-end compositor cadence is measured separately:

```sh
scripts/perf/frame-loop-bench.py \
  --backend drm --load fullscreen --width 3840 --height 2160 \
  --refresh-hz 60 --seconds 30 --label 4k60-target
```
