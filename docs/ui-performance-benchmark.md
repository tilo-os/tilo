# UI performance benchmark

This executable benchmark is the single source of truth for the retained UI
framework. A performance change is successful only when the measured budgets
improve without breaking pixel equivalence and framework tests. Presentation
cadence still has to be checked on the named physical display because an
offscreen VM cannot measure its compositor.

## Acceptance commands

Build once so every run measures the same binary:

```sh
cargo build -p ui --release --example ui-perf
```

Native 4K animated redraw and resource gate:

```sh
QUICK_UI_DASH_ITERS=1000 \
QUICK_UI_DASH_WIDTH=3840 \
QUICK_UI_DASH_HEIGHT=2160 \
QUICK_UI_DASH_RASTER_PIXEL_BUDGET=600000 \
QUICK_UI_DASH_GPU_BUDGET_BYTES=65000000 \
QUICK_UI_DASH_GPU_GROWTH_BUDGET_BYTES=0 \
QUICK_UI_DASH_RSS_BUDGET_KIB=350000 \
QUICK_UI_DASH_RSS_GROWTH_BUDGET_KIB=32768 \
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

One-million-event no-redraw CPU/allocation gate:

```sh
QUICK_UI_DASH_ITERS=1000000 \
QUICK_UI_DASH_STATIC=1 \
QUICK_UI_DASH_FROZEN=1 \
QUICK_UI_DASH_EVENT_BUDGET_US=3 \
QUICK_UI_DASH_ALLOC_BUDGET_PER_1000=1 \
target/release/examples/ui-perf dashboard_perf_bench
```

Strict 4K/60 frame gate:

```sh
QUICK_UI_DASH_ITERS=1000 \
QUICK_UI_DASH_WIDTH=3840 \
QUICK_UI_DASH_HEIGHT=2160 \
QUICK_UI_DASH_FRAME_BUDGET_US=16667 \
target/release/examples/ui-perf dashboard_perf_bench
```

The last command exits with status 2 unless redrawing-frame p99 is at most
16,667 microseconds. End-to-end compositor cadence is measured separately:

```sh
scripts/perf/frame-loop-bench.py \
  --backend drm --load fullscreen --width 3840 --height 2160 \
  --refresh-hz 60 --seconds 30 --label 4k60-target
```

## 2026-09-04 final A/B

The preserved baseline is revision
`84b348f5639b9157a4f4ff9d95dc45752509313e`. Both binaries use the corrected
GPU-wait harness: no screenshot readback, mapping, row repack, resolution
reduction, or upscale is included. Binary SHA-256 values are:

- baseline: `011533a173495d025675fd174431ef2019451a80412831386dafbc000b7c1179`
- optimized: `0b436236470bfddbd71ec5e9ded978ebca5ffc6f148b79ba8b755edab56d82ce`

The host was an 8-core ARM Oryon virtual machine with a Virtio GPU. Lower is
better. This is code-path evidence, not a claim about untested 2010 hardware.

### One million events, no redraw

All one million events are identical stationary cursor moves. They exercise
dispatch and dirty checks while correctly skipping layout, scene construction,
GPU submission, and redraw.

| Metric | Baseline | Optimized | Change |
| --- | ---: | ---: | ---: |
| Process wall time, median of 3 | 8.95 s | 3.49 s | **-61.0%** |
| Samples | 9.46 / 8.95 / 8.76 s | 3.44 / 3.61 / 3.49 s | |
| Redraws | 0 | 0 | unchanged |
| Measured allocations in final optimized loop | not instrumented | 0 calls / 0 bytes | bounded at zero |
| Warm-to-final RSS / GPU growth | not instrumented | 0 KiB / 0 bytes | bounded at zero |

The allocation profiler starts after the timing histogram is reserved, so
harness storage is excluded from the framework result.

### 1,000 continuously animated native-4K redraws

The optimized values are three independent runs. The conservative improvement
uses the fastest observed corrected baseline (69.7 ms), rather than the slower
77.9 ms matched rerun.

| Metric | Baseline | Optimized | Change |
| --- | ---: | ---: | ---: |
| Mean time per redraw | 69.7 ms (fastest observed) | 26.08 ms (median of 3) | **-62.6%** |
| Optimized samples | — | 26.48 / 26.08 / 25.79 ms | |
| Native pixels rasterized per redraw | 8,294,400 | about 453,900 | **-94.5%** |
| CPU target readback | 0 bytes | 0 bytes | unchanged |

The optimized GPU-path p99 samples were 97.41, 106.76, and 87.62 ms. Mean
performance exceeds the requested 50% improvement, but this VM does **not**
pass a constant-60-fps/p99 gate. The optional `QUICK_UI_VELLO_CPU=1` software
path measured 24.53 ms mean and 72.14 ms p99 over 1,000 frames here; it remains
opt-in because real GPUs need their own A/B.

### Framework resource usage at native 4K

The resource A/B uses the same final code and workload, changing only Vello's
allocation policy. `VELLO_BUMP_BUDGET=upstream` pins the former upstream
reservations; the default uses the bounded adaptive policy.

| Metric | Upstream policy | Optimized default | Change |
| --- | ---: | ---: | ---: |
| Known retained GPU bytes | 230,584,488 | 64,024,744 | **-72.2%** |
| Steady process RSS | 489,676 KiB | 325,640 KiB (median) | **-33.5%** |
| Peak process RSS | 490,476 KiB | 363,596 KiB (median) | **-25.9%** |
| Warm-to-final RSS growth | 9,204 KiB | 3,860 KiB | **-58.1%** |
| Exact damage textures | — | 1,774,336 bytes | |
| Full retained output | 33,177,600 bytes | 33,177,600 bytes | unchanged native 4K |

Known GPU bytes cover the retained output, exact-sized damage textures, Vello
pooled/live buffers, and image atlas. Driver-private and compositor-owned
allocations are visible only indirectly in process RSS. Reporting both avoids
presenting tracked bytes as total physical GPU memory.

The benchmark additionally prints signed warm-to-final RSS and known-GPU
deltas. `QUICK_UI_DASH_RSS_GROWTH_BUDGET_KIB` and
`QUICK_UI_DASH_GPU_GROWTH_BUDGET_BYTES` turn those deltas into soak/leak gates;
zero is a valid GPU-growth budget.

## Correctness and verdict

The native-damage pixel test renders a full reference frame, updates a bounded
region, and verifies that the partial result is pixel-identical while untouched
pixels remain preserved. The strict clippy gate, the 43-test Vello encoding
suite, and all new focused resource-bound tests pass. The full UI suite reports
2,637 passing tests and one pre-existing unrelated presence-label failure:
`authenticated_participant_kind_survives_presence_and_clears_on_disconnect`
expects `Release assistant` while the environment supplies
`Release assistant · Agent`.

The implementation passes the requested 50% native-4K mean, 25% no-redraw CPU,
and 30% steady framework-resource improvements. It does not yet prove constant
60 fps: the strict 16.667 ms p99 gate still fails on this VM, and the physical
2010 laptop plus 4K compositor test remains mandatory release evidence.
