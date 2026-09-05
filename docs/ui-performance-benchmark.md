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
QUICK_UI_DASH_ALLOC_CALL_BUDGET_PER_REDRAW=650 \
QUICK_UI_DASH_ALLOC_BYTE_BUDGET_PER_REDRAW=70000 \
QUICK_UI_DASH_RASTER_PIXEL_BUDGET=600000 \
QUICK_UI_DASH_GPU_BUDGET_BYTES=65000000 \
QUICK_UI_DASH_GPU_GROWTH_BUDGET_BYTES=4194304 \
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

The redraw allocation budgets enable phase attribution automatically and fail
if total allocator traffic exceeds either rate. To print the same phase table
without enforcing a budget, set `QUICK_UI_DASH_ALLOC_PROFILE=1` together with
`QUICK_UI_DASH_ALLOC_BREAKDOWN=1`. The no-redraw gate deliberately omits phase
snapshots so profiler bookkeeping does not contaminate its event timings.

One-million-event no-redraw CPU/allocation gate:

```sh
QUICK_UI_DASH_ITERS=1000000 \
QUICK_UI_DASH_STATIC=1 \
QUICK_UI_DASH_FROZEN=1 \
QUICK_UI_DASH_EVENT_BUDGET_US=3 \
QUICK_UI_DASH_ALLOC_BUDGET_PER_1000=1 \
target/release/examples/ui-perf dashboard_perf_bench
```

Pixel-equivalence preflight (separate from performance measurements):

```sh
QUICK_UI_DASH_VERIFY=1 \
QUICK_UI_DASH_VERIFY_FRAMES=40 \
QUICK_UI_DASH_ITERS=40 \
QUICK_UI_DASH_WIDTH=3840 \
QUICK_UI_DASH_HEIGHT=2160 \
target/release/examples/ui-perf dashboard_perf_bench
```

This compares each submitted retained frame against a full render of the exact
same scene, allowing at most one channel value of antialiasing variation. All
40 frames must match. Verification adds native-size reference textures and
readbacks, suppresses performance reports, and rejects benchmark budget flags;
its runtime and memory are not performance samples.

Current requested 4K/30 gate (1,000 redraws, p99 at most 33,333 microseconds):

```sh
QUICK_UI_DASH_ITERS=1000 \
QUICK_UI_DASH_WIDTH=3840 \
QUICK_UI_DASH_HEIGHT=2160 \
QUICK_UI_DASH_FRAME_BUDGET_US=33333 \
target/release/examples/ui-perf dashboard_perf_bench
```

The earlier, stricter 4K/60 frame gate remains available:

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
- optimized: `20d39a8473631ba17a958dfe0ee75be5caf864626edbc5b697c96df01325afc9`

The final renderer additionally retains the encoded damage scene, packed-scene
upload, and command storage; stores small dispatch binding lists inline; and
caches exact bind groups in a 128-entry map. Animated damage targets are
quantized and reused, progress-bar wraparound remains split into disconnected
regions, and replacing a scratch texture evicts only bind groups retaining that
view. The preserved executable is
`target/ui-perf-comparison/ui-perf-final-stable-damage-bindings`.

The host was an 8-core ARM Oryon virtual machine with a Virtio GPU. Lower is
better. This is code-path evidence, not a claim about untested 2010 hardware.

### One million events, no redraw

All one million events are identical stationary cursor moves. They exercise
dispatch and dirty checks while correctly skipping layout, scene construction,
GPU submission, and redraw.

| Metric | Baseline | Optimized | Change |
| --- | ---: | ---: | ---: |
| Process wall time, median of 3 | 8.95 s | 3.35 s | **-62.6%** |
| Samples | 9.46 / 8.95 / 8.76 s | 3.35 / 3.57 / 3.21 s | |
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
| Mean time per redraw | 69.7 ms (fastest observed) | 20.176 ms (median of 3) | **-71.1%** |
| Optimized samples | — | 20.492 / 19.720 / 20.176 ms | |
| Native pixels rasterized per redraw | 8,294,400 | 234,718 (median) | **-97.2%** |
| Allocation calls per redraw | 876 | 548 | **-37.4%** |
| Allocation bytes per redraw | 1,031,807 | 52,688 | **-94.9%** |
| CPU target readback | 0 bytes | 0 bytes | unchanged |

The optimized GPU-path p99 samples were 58.861, 65.970, and 66.944 ms. Mean
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
| Known retained GPU bytes | 230,584,488 | 62,950,824 (median) | **-72.7%** |
| Steady process RSS | 489,676 KiB | 328,072 KiB (median) | **-33.0%** |
| Peak process RSS | 490,476 KiB | 363,692 KiB (median) | **-25.8%** |
| Warm-to-final RSS growth | 9,204 KiB | 9,452 KiB (median) | bounded below 32 MiB |
| Quantized reusable damage textures | — | 700,416 bytes (median) | |
| Full retained output | 33,177,600 bytes | 33,177,600 bytes | unchanged native 4K |

Known GPU bytes cover the retained output, exact-sized damage textures, Vello
pooled/live buffers, and image atlas. Driver-private and compositor-owned
allocations are visible only indirectly in process RSS. Reporting both avoids
presenting tracked bytes as total physical GPU memory.

The benchmark additionally prints signed warm-to-final RSS and known-GPU
deltas. `QUICK_UI_DASH_RSS_GROWTH_BUDGET_KIB` and
`QUICK_UI_DASH_GPU_GROWTH_BUDGET_BYTES` turn those deltas into soak/leak gates.
The animated scratch cache may legitimately grow after its two warm-up frames,
so the acceptance command bounds that growth at 4 MiB rather than pretending
every animation phase was visited during warm-up. The observed median was
237,568 bytes.

## Correctness and verdict

The native-damage pixel test renders a full reference frame, updates a bounded
region, and verifies that the partial result is pixel-identical while untouched
pixels remain preserved. The strict clippy gate, the 43-test Vello encoding
suite, and all new focused resource-bound tests pass. The full UI suite reports
2,643 passing tests and one pre-existing unrelated presence-label failure:
`authenticated_participant_kind_survives_presence_and_clears_on_disconnect`
expects `Release assistant` while the environment supplies
`Release assistant · Agent`.

The implementation passes the requested 50% native-4K mean, 25% no-redraw CPU,
and 30% steady framework-resource improvements. It does not yet prove constant
60 fps: the strict 16.667 ms p99 gate still fails on this VM, and the physical
2010 laptop plus 4K compositor test remains mandatory release evidence.

## 2026-09-05 investigation: rejected atlas experiment

The revised target is 1,000 native-4K redraws with p99 at most 33.333 ms.
A fresh run of the preserved optimized executable measured 17.514 ms mean,
65.513 ms p99, and 63,724,968 tracked GPU bytes: the target still failed.

An experimental renderer packed disconnected damage into shared scratch
atlases, appending one clipped copy of the scene per atlas entry. The same
1,000-redraw timing command measured:

| Metric | Preserved optimized renderer | Rejected atlas experiment |
| --- | ---: | ---: |
| Mean redraw | 17.514 ms | 35.806 ms |
| Redraw p99 | 65.513 ms | 114.058 ms |
| Native raster pixels/redraw | 240,316 | 139,490 |
| Tracked GPU bytes | 63,724,968 | 110,428,728 |
| Steady RSS | 329,776 KiB | 411,112 KiB |

The experiment failed both performance and correctness acceptance: its
separate 40-frame native-4K preflight first reported 13 differing pixels at
frame 18 and failed 23 frames in total. Timing is diagnostic only, not a
successful result. Fewer raster pixels did not compensate for duplicated
scene/coarse-render work. Atlas packing was rejected for the default renderer.

The rejected binary is preserved at
`target/ui-perf-comparison/ui-perf-rejected-atlas-20260905`, SHA-256
`551756d723cd02be7c664d724c62a9b2a0da3bee5265b4dcc925c0a90c942f36`.
No 30-fps or 60-fps p99 success is established by these measurements.

### Native-coordinate replacement

The expanded dashboard preflight also rejected independently translated
damage rectangles: 13 text-edge pixels differed after the first selection
change, even though they were inside the copied damage. Aligning damage to
16-pixel tiles did not help. A full render through the same renderer matched
the independent reference, and an untranslated native-origin partial render
also matched. The temporary diagnostic variants were then removed.

The replacement renders the original scene once into a reusable scratch target
extending from the native origin through all dirty bounds. It copies only the
dirty rectangles, at unchanged coordinates, into the retained 4K target. It
does not duplicate or translate the encoded scene. Scratch extents covering
half the target or more use a direct full render; the raster count includes
that fallback and any overflow-recovery passes. Full/partial transitions
release obsolete pooled buffers, while ordinary partial-frame changes retain
their caches.

The default 40-frame native-4K preflight passes with zero differing pixels in
both debug and release builds, and all 13 focused damage/lifecycle tests pass.
The full UI suite reports 2,651 passing tests and only the same pre-existing
identity-label failure described above.

The first prefix release binary is preserved at
`target/ui-perf-comparison/ui-perf-native-origin-prefix-20260905`, SHA-256
`94ca94a82adad607db7db680db442b397e18c55d56fed3b51c90c742d64fdce6`.
Its default 1,000-redraw run measured 19.991 ms mean, 81.219 ms p99,
548,079 native raster pixels/redraw, and 62,791,112 tracked GPU bytes.
The 33.333 ms p99 gate **failed**.

A separate allocation/resource run measured 533 allocation calls and 51,314
allocated bytes per redraw, 542,944 raster pixels/redraw, and 327,948 KiB RSS.
Those gates passed, but tracked GPU memory reached 65,199,560 bytes, exceeding
the unchanged 65,000,000-byte cap by 199,560 bytes. This binary still used
32-pixel scratch-height allocation steps. The subsequent 16-pixel height
allocation change requires its own acceptance run; do not apply these results
to a different binary.

The matching one-million-event idle run passed with no redraws, zero
allocations, zero warm-to-final RSS/GPU growth, and 2 microseconds event p99.
Process wall time was 3.24 seconds (one sample, default 1800×1200 canvas).
The opt-in CPU-coarse variant was not adopted: at native 4K it measured
27.701 ms mean and 129.802 ms p99, also failing the frame gate.

For tail diagnosis, `QUICK_UI_DASH_FRAME_BREAKDOWN=1` prints post-loop full/
recovery versus partial sample counts and percentiles, plus the ten slowest
frame indices and their raster work. It does not change the timed workload,
resource snapshots, or all-frame acceptance population, and verification mode
rejects it. These diagnostics must not be used to exclude slow frames.

### Verified height-16 checkpoint

Preserved release executable:
`target/ui-perf-comparison/ui-perf-native-origin-prefix-height16-20260905`.
SHA-256:
`78a0d96f2000a829e458a9f49fb161ea39fda7234c0fefe72874e12b6c2643aa`.

| Check | Measured result | Verdict |
| --- | ---: | --- |
| Native-4K retained/full image comparison | 40/40 frames, zero differing pixels beyond the 1-channel allowance | pass |
| 1,000 native-4K redraws, frame-breakdown run | 20.489 ms mean; 98.112 ms p99 | **fails 33.333 ms** |
| Separate allocation/resource run | 19.797 ms mean; 69.985 ms p99 | also above the frame target |
| Allocation calls / bytes per redraw | 533 / 51,688 | pass unchanged budgets |
| Native raster pixels/redraw, resource run | 534,508 | pass 600,000 cap |
| Tracked GPU bytes, resource run | 62,475,720 | pass 65,000,000 cap |
| Steady RSS, resource run | 326,904 KiB | pass 350,000 KiB cap |
| Positive warm-to-final GPU / RSS growth | 0 bytes / 0 KiB | pass unchanged growth budgets |
| One million idle events, default 1800×1200 canvas | 2 µs p99; zero redraws/allocations/growth; 3.07 s process wall time | pass |

The frame breakdown found 17 full/recovery samples (56.804 ms mean,
111.384 ms p99) and 983 partial samples (19.861 ms mean, 93.804 ms p99).
The slowest sample was a partial redraw at event 486: 123.595 ms while
rasterizing 623,672 pixels. Startup full redraws therefore do **not** explain
the remaining tail by themselves, and excluding them would not pass the gate.

All measurements above use the same release binary. The resource run enables
allocation profiling; the frame-breakdown run only classifies the existing
histogram after timing. These are individual diagnostic/acceptance runs, not
three-run medians. This checkpoint fixes image equivalence and passes resource
and idle gates, but it does not establish a redraw-latency improvement over
the preserved optimized renderer or achieve the current 30-fps-p99 objective.

The next investigation is to split host encode/submit time from the blocking
GPU-completion wait. Source inspection found unconditional packed-scene rebuild
and upload on every redraw, but no unconditional steady-state 4 MiB image-atlas
upload. A new cache or scheduling change needs timing evidence from that split
before adoption.

### Submission and completion-wait diagnostic

```sh
QUICK_UI_DASH_ITERS=1000 \
QUICK_UI_DASH_WIDTH=3840 QUICK_UI_DASH_HEIGHT=2160 \
QUICK_UI_DASH_RENDER_PROFILE=1 QUICK_UI_DASH_FRAME_BREAKDOWN=1 \
QUICK_UI_DASH_FRAME_BUDGET_US=33333 \
target/release/examples/ui-perf dashboard_perf_bench
```

The opt-in profile separates Vello scene resolution/encoding/uploads/submission,
dirty-copy encoding, and the blocking completion/counter-collection wait.
The current single-submit path appends the copies to Vello's encoder before
submission. Its `vello-submit` value excludes separately timed `copy-encode`
work, so the two boundaries remain additive without counting encoding twice.
Full renders and overflow-recovery attempts are included. Unaccounted
render time includes damage collection and scratch-target/cache setup. The
slowest samples include event indices, raster pixels, and full-attempt counts;
the existing all-redraw p99 acceptance gate remains unchanged.

These are host-clock boundaries, not GPU timestamp queries: a long wait may
include device execution, driver work, and scheduling. Component percentiles
are separate distributions and must not be added to obtain frame p99.
Timing storage is allocated only when profiling is enabled, before the resource
baseline. The default histogram layout is unchanged, and normal rendering
does not take these extra timestamps. Pixel-verification mode rejects this
profile flag to keep readback/reference work out of performance results.

The first profiled native-prefix executable is preserved as
`target/ui-perf-comparison/ui-perf-native-prefix-profile-20260905`, SHA-256
`f11a54f6a9550458588feab442227885b5b3900e28766d8119312e84764d030e`.
Its 1,000-redraw native-4K diagnostic measured:

| Host boundary | Mean | p99 |
| --- | ---: | ---: |
| Vello encode/upload/submit | 0.615 ms | 1.140 ms |
| Separate dirty-copy encode/submit | 17.558 ms | 69.779 ms |
| Completion/counter wait | 1.163 ms | 45.539 ms |
| Unaccounted render time | 1.573 ms | 30.598 ms |
| Complete event | 21.031 ms | 78.393 ms |

All 1,000 events redrew. The 33.333 ms gate failed. There were 19 full/recovery
frames and 981 partial frames; partial-frame p99 alone was 76.886 ms. The
second queue submission was the dominant host-side stall for partial frames,
whereas full renders stalled in the final completion wait. This identifies
a synchronization boundary, not the independent cost of copying the pixels:
that submission may be waiting on previously queued rendering. The next
experiment combines render and dirty-copy commands in one submission and must
improve complete-event timing, not merely move the wait to another label.

### Single-submission render and damage copies

Vello now accepts a post-render encoder callback. Both offscreen and live
partial rendering append their existing native-coordinate texture copies
before Vello's queue submission. No copies, pixels, recovery attempts, or
completion waits are omitted. The ordinary Vello entry point uses a no-op
callback, without boxing or an additional submission. The partial raster-plus-
copy transaction goes from two queue submissions to one; live presentation
and optional postprocessing are separate and are not included in that count.

Measured candidate executable:
`target/ui-perf-comparison/ui-perf-native-prefix-single-submit-20260905`.
SHA-256:
`87f63db3b50bc15abbca8bdaf25a48a142093dd18ba86ec096885c49af1f970c`.
The preceding `f11a54...` executable is the comparison baseline. Both pass
the 40-frame native-4K retained/full-image check with zero differing pixels
beyond the unchanged one-channel-value allowance.

Three ordinary timing runs per executable, each with 1,000 native-4K redraws,
profiling disabled, and the unchanged 33.333 ms frame gate:

| Metric | Two submissions | One submission | Observed change |
| --- | ---: | ---: | ---: |
| Mean redraw, median of three runs | 20.150 ms | 20.400 ms | +1.2% |
| Redraw p99, median of three runs | 99.472 ms | 74.981 ms | -24.6% |
| Mean samples | 23.814 / 20.150 / 20.140 ms | 18.752 / 20.400 / 20.817 ms | |
| p99 samples | 104.320 / 99.472 / 79.161 ms | 66.606 / 74.981 / 93.705 ms | |
| Tracked GPU bytes, median | 62,903,752 | 64,075,208 | +1.9% |
| Process RSS, median | 329,188 KiB | 323,124 KiB | -1.8% |

All six frame-gate runs fail. These are observed A/B samples on a variable VM,
not a guaranteed 24.6% tail-speed improvement: the distributions overlap and
there is no mean-redraw gain. Animation phase also changes scratch extents and
final tracked memory. Vello's retained buffer pool is unchanged at 24,878,536
bytes in these animated runs. Separate allocation-profiled runs even reverse
the timing ordering (baseline 18.632 ms mean / 66.676 ms p99; candidate
19.996 ms / 85.430 ms), reinforcing the need not to overstate the tail result.

The separate allocation/resource A/B reports:

| Metric per redraw unless stated | Two submissions | One submission |
| --- | ---: | ---: |
| Allocation calls, excluding reallocations | 532.847 | 508.734 |
| Allocation bytes, excluding reallocations | 51,397 | 44,847 |
| Allocated plus reallocated bytes | 72,648 | 67,105 |
| Native raster pixels | 515,184 | 580,650 |
| Tracked GPU bytes | 63,804,872 | 62,520,776 |
| Process RSS | 325,240 KiB | 319,144 KiB |
| Positive warm-to-final GPU / RSS growth | 0 / 0 | 0 / 0 |

Both pass all seven unchanged allocation/resource gates. Removing the second
encoder/submission reduces allocation calls by 4.5% and total allocated plus
reallocated byte traffic by 7.6% in this A/B. Neither run reads back the 4K
output during timing.

The candidate's separate boundary-profile run measured 19.516 ms mean and
70.233 ms p99, failing the frame gate. Combined Vello submission, excluding
copy encoding, measured 0.640 ms mean / 1.206 ms p99; copy encoding measured
less than 1 microsecond mean / 2 microseconds p99. Completion wait measured
17.693 ms mean / 67.728 ms p99, and unaccounted render time measured 1.062 ms
mean / 23.281 ms p99. Much of the previous copy-submit stall therefore moved
to the final wait. GPU execution/driver completion remains the unresolved
bottleneck; reducing command-buffer/pass overhead is a possible next
experiment, not an established fix.

One-million-event idle runs were also repeated with explicit **3840×2160**
dimensions, unlike the earlier default-size idle checkpoints. Both executables
pass at 2 microseconds event p99 with zero redraws, allocations, reallocations,
or measured warm-to-final memory growth; each takes 3.27 seconds process wall
time (one sample each). Both still retain 114,154,952 tracked GPU bytes after
their full-frame warm-up. This idle check does not apply the animated run's
65 MB retained-GPU cap and must not be described as a low-memory idle pass.

Live-path review additionally found that `ui/src/gpu.rs::create_targets` lacked
`COPY_DST`, although the render worker copies partial updates into that target.
The custom UI context, not Vello's similarly named helper, creates the live
target; there was no fallback for that missing usage. The descriptor now
includes `COPY_DST`, with a headless validation-copy regression using the
process-wide shared GPU. This subsequent live-target-only fix does not change
the offscreen target creation exercised by the preserved benchmark binaries.

Final source checks for this checkpoint pass: strict UI/Vello clippy, formatting
of the changed renderer files, and 16 focused tests (14 library tests including
the live-target validation copy, plus two benchmark-verifier/profile tests).
The current 30-fps/p99 objective remains open; this checkpoint is not a claim
of constant 30 or 60 fps, nor a physical-2010-laptop qualification.

### Experimental shared compute passes

`VELLO_COMPUTE_BATCH=1` groups consecutive GPU dispatches into bounded shared
compute passes; unset or `0` retains the unbatched path. CPU dispatches and
profiler builds remain unbatched. Uploads, downloads, clears, and other actual
encoder operations remain boundaries. Pending lazy clears also force a boundary
before materializing the next dispatch's bindings, preserving command ordering.
This experiment does not remove dispatches, damage copies, overflow recovery,
or GPU-completion waits. `VELLO_COMPUTE_BATCH_LOG=1` is a separate diagnostic
that prints actual per-pass dispatch counts and must stay off in timing runs.

Preserved executable:
`target/ui-perf-comparison/ui-perf-native-prefix-compute-batch-20260905`.
SHA-256:
`c8dd81605edea8595826d3d902171f3ce274a970da339b9b4e6943ee6790b03e`.
Unlike the earlier single-submission executable, this includes the live-target
`COPY_DST` fix. Both sides below use this same executable, alternating modes
`0` and `1`; the native target and all-frame gate remain unchanged.

| Metric | Unbatched (`0`) | Consecutive batching (`1`) |
| --- | ---: | ---: |
| Mean redraw samples | 20.618 / 22.354 / 25.157 ms | 18.495 / 24.603 / 20.519 ms |
| Redraw p99 samples | 79.215 / 98.614 / 100.236 ms | 68.699 / 98.921 / 74.043 ms |
| Median mean redraw | 22.354 ms | 20.519 ms |
| Median redraw p99 | 98.614 ms | 74.043 ms |
| Median tracked GPU bytes | 63,692,232 | 63,579,592 |
| Median process RSS | 321,492 KiB | 321,172 KiB |

Each run completes all 1,000 native-3840x2160 redraws with zero output-image
readback bytes. **All six 33.333 ms p99 gates fail.** The observed median changes
are -8.2% mean and -24.9% p99, but the second pair regresses; this variable VM
does not establish a guaranteed tail improvement. No slow frames are excluded.

Separate allocation-profiled runs pass all seven unchanged resource gates:

| Metric per redraw unless stated | Unbatched (`0`) | Consecutive batching (`1`) |
| --- | ---: | ---: |
| Allocation calls, excluding reallocations | 508.286 | 415.919 |
| Allocated plus reallocated bytes | 66,830 | 49,274 |
| Native raster pixels | 535,258 | 544,242 |
| Tracked GPU bytes | 63,286,728 | 62,520,776 |
| Process RSS | 323,068 KiB | 317,144 KiB |
| Positive warm-to-final GPU / RSS growth | 0 / 0 | 0 / 0 |

This is an 18.2% reduction in allocation calls and 26.3% reduction in total
allocation byte traffic. Resource-profile timing is not used as the timing A/B:
it measured 20.464 / 23.900 ms mean and 90.944 / 85.310 ms p99 respectively.

Correctness preflight with batching enabled passes all 40 native-4K frames.
The full reference renderer now explicitly disables batching before its first
render, so a common-mode batching bug cannot make both paths agree falsely.
An added transparent clipped-image regression covers image reuse, movement,
replacement, overlaps, and full/partial transitions against an unbatched
reference. All 17 focused UI/benchmark tests pass with mode `1` and mode `0`;
the three Vello unit tests and strict UI/Vello clippy also pass at this checkpoint.

The timing label `settle_wait_us` still includes driver and mapping-callback
latency, not just device execution. In wgpu 29, `on_submitted_work_done` is ordered
after mapped-buffer callbacks and therefore cannot independently isolate that
latency; a future GPU-only diagnostic requires adapter-supported timestamps.

The next opt-in mode, `VELLO_COMPUTE_BATCH=2`, also consumes deferred
`FreeBuffer`/`FreeImage` bookkeeping inside compute runs. These commands only
collect IDs: resources still return to the pool after submission, as before.
True GPU operations and lazy-clear hazards remain boundaries. Its fixed inline
capacity limits actual dispatches to 16; mode `1` preserves its earlier
16-command scan limit. Diagnostic image-test passes shrink from eight passes
(`2 / 1 / 3 / 1 / 1 / 1 / 6 / 1` dispatches) to three (`3 / 12 / 1`) without
removing work; that pass count is a fixture diagnostic, not yet a timing result.

Mode `2` passes 17 focused UI/benchmark tests and all four Vello unit tests,
including deferred-free and pending-clear regressions. Strict UI/Vello clippy
passes. The broad UI run reports 2,653 passes and the one previously observed
unrelated failure:
`presence::own_identity_tests::authenticated_participant_kind_survives_presence_and_clears_on_disconnect`
expects `Release assistant` where the implementation produces
`Release assistant · Agent`. The full suite is therefore not claimed green.

The measured mode-2 executable is
`target/ui-perf-comparison/ui-perf-native-prefix-compute-batch-frees-20260905`,
SHA-256
`bfd5be581a5b14a7e9a75b04038a451769d1f0d492b101e266ce82d8a1dac8e9`.
It passes the 40-frame native-4K preflight against the explicitly unbatched
reference. The clipped-image test also passes with the CPU fallback requested;
its diagnostic confirms CPU dispatches remain unbatched.

Three alternating same-executable pairs, profiling disabled, with all 1,000
native-4K redraws retained in each run:

| Metric | Unbatched (`0`) | Batching across deferred frees (`2`) |
| --- | ---: | ---: |
| Mean redraw samples | 20.161 / 20.343 / 19.823 ms | 19.533 / 18.878 / 19.670 ms |
| Redraw p99 samples | 78.333 / 80.967 / 77.694 ms | 75.131 / 69.650 / 70.175 ms |
| Maximum redraw samples | 157.334 / 115.582 / 138.580 ms | 114.181 / 174.311 / 105.788 ms |
| Median mean redraw | 20.161 ms | 19.533 ms (-3.1%) |
| Median redraw p99 | 78.333 ms | 70.175 ms (-10.4%) |
| Median tracked GPU bytes | 63,309,256 | 64,953,800 |
| Median process RSS | 322,264 KiB | 319,252 KiB |

All six frame gates fail. The updated default remains unbatched: modes `1` and
`2` are experimental opt-ins, not a claim that the normal application now meets
30 fps p99. Nor should the earlier mode-1 percentage be compared directly to
this mode-2 percentage: each has its own contemporaneous same-binary control.
Tracked GPU memory is not reduced by batching; phase-dependent scratch extent
is responsible for the different final totals, and the Vello buffer pool remains
24,878,536 bytes on both sides.

The separate mode-0/mode-2 allocation-profiled pair passes all seven resource
gates with no positive GPU or RSS growth:

| Metric per redraw unless stated | Unbatched (`0`) | Extended batching (`2`) |
| --- | ---: | ---: |
| Allocation calls, excluding reallocations | 508.819 | 353.833 (-30.5%) |
| Allocated plus reallocated bytes | 66,686 | 40,444 (-39.4%) |
| Native raster pixels | 561,121 | 543,793 |
| Tracked GPU bytes | 63,106,504 | 62,903,752 |
| Process RSS | 324,272 KiB | 315,380 KiB |

Allocation-profile timing was 20.218 / 20.134 ms mean and 78.104 / 77.206 ms p99
respectively. Byte traffic is allocation activity, **not** a 39.4% reduction in
resident RAM or GPU-copy volume. No output-image readback occurs in timing runs.

The million-event idle check at explicit 3840x2160 passes in both modes:
2 microseconds p99, zero redraws/allocations/reallocations, and zero measured
warm-to-final memory growth. Process wall time was 3.42 / 3.24 seconds (one
sample each). Both retain 114,154,952 tracked GPU bytes from full-frame warm-up;
these idle runs do not apply the animated 65 MB GPU cap.

A separate extended-batching host-boundary profile fails at 68.134 ms p99 and
18.141 ms mean. Its 22 full/recovery frames have 63.353 ms p99; its 978 partial
frames alone have 68.134 ms p99, so excluding full redraws would not solve the
target. Vello submission measures 0.592 ms mean / 1.026 ms p99, copy encoding
less than 1 microsecond mean / 2 microseconds p99, completion settlement
16.233 ms mean / 60.000 ms p99, and unaccounted render work
1.200 ms mean / 22.425 ms p99. These remain host timings, not GPU timestamps.
The slowest partial sample with only 82,388 raster pixels spends 104.164 ms in
settlement. The bottleneck is unresolved; fewer passes and allocations alone
do not establish the requested p99 performance.

Follow-up candidates, not implemented or measured at this checkpoint, are
adapter-supported GPU timestamps and bounded geometric scratch-width capacity
to reduce texture recreation. The latter must preserve actual native raster
dimensions and avoid reallocating an identical texture when quantized capacity
trips the current area-based shrink threshold. Generic scene pruning is not a
safe worker-side shortcut: composed subtree caches, ancestor clips, overlays,
and draw-updated animation bounds require a conservative fragment-cache contract.

The UI clippy check with Vello's `wgpu-profiler` and `debug_layers` features
enabled also passes. These feature checks are separate from the timing builds;
the profiler-enabled path deliberately retains its original dispatch behavior.
The complete captured reports for the final same-binary timing, resource,
million-event idle, and boundary-profile runs are retained locally at
`target/ui-perf-comparison/bfd5be581a5b14a7-benchmarks.txt` (ignored build artifact).

## Geometric scratch capacity and GPU timestamps (2026-09-05)

`QUICK_UI_DAMAGE_GEOMETRIC=1` is a new experimental opt-in, sampled once per
renderer. Both offscreen and live rendering use power-of-two scratch widths,
capped at the viewport/device limit, with fourfold shrink hysteresis. Height
still uses the 16-pixel quantum. Actual render dimensions, original scene
coordinates, native damage-copy regions, full-frame fallback, and completion
waits are unchanged. The default/unset mode retains the previous allocation
policy; neither this experiment nor compute batching is enabled by default.

The geometric GPU regression explicitly checks 64 -> 128 -> 256 -> 64 scratch
capacity, reuse between transitions, transparent clearing of old positions,
allocation counts, and full-render pixel equivalence. A separate 40-frame
3840x2160 preflight passes against the explicitly unbatched full reference.

Measured executable:
`target/ui-perf-comparison/ui-perf-native-prefix-geometric-timestamps-20260905`,
SHA-256
`4947ad65ec263e08a563f11da60ec0c72350059d719801e87e8461958ea6bc93`.
This is a dirty-worktree build on `4000ae4bab30b1e8c477eb1633d22d071feaf150`.
All three alternating pairs below use that same executable, with
`VELLO_COMPUTE_BATCH=2`, `VELLO_GPU_TIMESTAMPS=0`, 1,000 events,
3840x2160 native output, and `QUICK_UI_DASH_FRAME_BUDGET_US=33333`.
Every event redraws; all full/recovery frames remain in the gate.

| Metric | Previous scratch policy (`0`) | Geometric capacity (`1`) |
| --- | ---: | ---: |
| Mean redraw samples | 19.575 / 20.307 / 19.168 ms | 17.630 / 18.523 / 19.150 ms |
| Redraw p99 samples | 73.424 / 75.713 / 73.789 ms | 63.801 / 68.627 / 66.927 ms |
| Maximum redraw samples | 104.967 / 117.384 / 119.626 ms | 100.104 / 167.260 / 102.015 ms |
| Median mean redraw | 19.575 ms | 18.523 ms (-5.4%) |
| Median redraw p99 | 73.789 ms | 66.927 ms (-9.3%) |
| Median tracked GPU bytes | 64,751,048 | 64,953,800 (+0.3%) |
| Median process RSS | 319,332 KiB | 318,236 KiB (-0.3%) |

All six frame gates **fail**. These are observed paired-run improvements, not
proof of constant 30 or 60 FPS; the worst maximum regresses. Animation follows
wall time, so native raster work and final scratch dimensions vary slightly
between runs. Do not combine percentages with the older batching experiments,
which have different contemporaneous controls. This VM is not a physical
2010 laptop or a display-presentation benchmark.

A separate allocation + host-phase profiled pair isolates scratch churn:

| Metric | Previous scratch policy (`0`) | Geometric capacity (`1`) |
| --- | ---: | ---: |
| Scratch texture creations / 1,000 redraws | 641 | 37 (-94.2%) |
| Damage setup mean / p99 | 2.066 / 31.804 ms | 0.171 / 0.760 ms |
| Allocation calls / redraw, excluding reallocations | 354.495 | 345.249 (-2.6%) |
| Allocated + reallocated bytes / redraw | 40,248.808 | 41,825.340 (+3.9%) |
| Tracked GPU bytes | 64,953,800 | 64,953,800 (unchanged) |
| Process RSS | 320,728 KiB | 318,324 KiB (-0.7%) |

Both profiled runs print seven passing resource gates and no positive warm-to-final
GPU or RSS growth. The measured binary's byte-allocation gate, however, only
counts newly allocated bytes, not reallocated bytes. The combined traffic above
is calculated from both raw counters and is also below the 70,000-byte budget.
The post-measurement source correction makes that gate count both, using a
conservative rounded-up per-redraw rate. Historical gate output must not be
described as having enforced the corrected definition. Allocation traffic is
not resident RAM, actual bytes copied by realloc, or GPU-copy volume.

The geometric profiled run has 21 full/recovery and 979 partial frames; their
p99 values are 62.620 and 66.939 ms respectively. Excluding full frames would
still miss the goal. The resource-profiled runs have total-frame p99 values of
83.680 / 66.939 ms; these are separate from the unprofiled comparison above.
The measured binary's `unaccounted` profile field still includes damage setup;
it must not be added to the separately reported setup field. This post-timing
accounting error was subsequently fixed in source with a nonzero-setup
regression test; it does not alter the measured frame timings.

At explicit 3840x2160, both million-event frozen/idle runs pass at 2 microseconds
p99 with zero redraws, allocations, reallocations, and warm-to-final memory
growth. Process wall times are 3.54 / 3.41 seconds, one sample each. Both retain
114,154,952 tracked GPU bytes from warm-up; the animated 65 MB cap is not applied
to these idle checks.

### GPU timestamp diagnostic

`VELLO_GPU_TIMESTAMPS=1` requests adapter-supported timestamp queries and adds
bounded compute-pass begin/end timestamps without disabling batching or adding
a submission. It uses 2,048 bytes of diagnostic resolve/readback buffers plus
opaque query metadata, reported separately from the usual tracked memory.
The diagnostic is off by default; it rejects acceptance-budget flags and image
verification, but composes with `QUICK_UI_DASH_FRAME_BREAKDOWN=1`.

The first-to-last compute-pass span and sum of individual pass intervals are
device timestamps, not exclusive GPU busy time. They can include scheduling or
preemption. They exclude host scene/command preparation, queue backlog before
the first pass, post-render dirty copies, and host settlement/map-callback work.
Missing, stale, failed, or overflowed timestamp recordings are counted explicitly;
only complete frames contribute to GPU quantiles, never zero-filled missing data.
The ordinary all-frame acceptance gate is unaffected by this diagnostic.

A separate 1,000-frame native-4K run with batching `2`, geometric capacity `1`,
timestamps enabled, and no budgets reports:

| Diagnostic | Mean | p99 |
| --- | ---: | ---: |
| Total frame | 20.988 ms | 66.748 ms |
| GPU compute span | 18.465 ms | 61.380 ms |
| Sum of GPU compute-pass intervals | 12.885 ms | 47.999 ms |
| Host Vello submit (excluding dirty-copy encoding) | 0.583 ms | 0.969 ms |
| Host completion settlement | 20.061 ms | 65.567 ms |

Coverage is 1,000/1,000 complete frames and 1,000/1,000 submissions, with zero
missing samples. The mean and p99 compute-pass counts are both three. A slow
full frame measures 78.368 ms in the GPU span versus 79.640 ms render time; a slow
partial frame measures 15.889 ms in its GPU span but 80.573 ms settling. This
shows a mixture of expensive/device-delayed compute intervals and delays outside
those intervals, not merely CPU allocation overhead. It does not identify a
specific driver fault or prove a hardware limit.

Validation includes eight Vello unit tests; 16 focused UI damage tests in both
baseline and experimental modes; four geometric tests with GPU timestamps
enabled, asserting complete matching samples when supported; benchmark helper
tests; strict default UI/Vello clippy; and the native-4K image preflight. The
previously recorded unrelated presence-test failure is not claimed fixed.
After the reporting corrections, all six benchmark-example tests and strict
clippy pass again, including the UI library with Vello's `wgpu-profiler` and
`debug_layers` features enabled. No rendering-path changes were made after the
measured executable was preserved.
Raw commands, exit codes, and complete output are preserved locally at
`target/ui-perf-comparison/4947ad65ec263e08-benchmarks.txt` (ignored build artifact).

## Direct masked sparse fine rendering (2026-09-05)

`VELLO_SPARSE_FINE=1` is a new experimental opt-in, sampled at renderer creation.
It uses the original scene and native coordinates, prepares a deduplicated list
of 16x16 tiles with exact pixel-write masks, and dispatches the Area-AA fine
shader only for those tiles. The shader masks final texture stores, not thread
execution before blending or barriers. It writes directly into the retained
3840x2160 storage texture; successful sparse updates allocate no damage texture
and encode no scratch-to-retained dirty-copy command. Scene uploads, coarse
rendering, image uploads, and bump-counter readbacks still exist. This is not
elimination of all GPU memory copies.

Coarse rendering still covers the native-origin prefix. A thin far-edge update
can therefore have a full-screen coarse prefix while fine shading remains
sparse. Initial/invalid targets, large actual damage, unsupported configurations,
and overflow recovery retain the dense path. CPU and non-Area-AA rendering fall
back without submitting a sparse attempt. Reusable host mask/record vectors are
bounded, but the tile-list upload still allocates/copies a payload per sparse
frame. The default/unset renderer is unchanged; this experiment, geometric
scratch capacity, and compute batching remain opt-in.

### Same-binary acceptance comparison

Release build: `cargo build -p ui --release --example ui-perf` (4m49s).
Preserved executable:
`target/ui-perf-comparison/ui-perf-native-masked-sparse-fine-20260905`.
SHA-256:
`d5078a49369b3b4ddfd9d3c23097dd68984e8b18bd8426e49e06c167c6d6146a`.
This is a dirty-worktree build on `4000ae4bab30b1e8c477eb1633d22d071feaf150`.

Three alternating off/on pairs use that same executable with
`VELLO_COMPUTE_BATCH=2`, `QUICK_UI_DAMAGE_GEOMETRIC=1`,
`VELLO_GPU_TIMESTAMPS=0`, `QUICK_UI_DASH_ITERS=1000`,
`QUICK_UI_DASH_WIDTH=3840`, `QUICK_UI_DASH_HEIGHT=2160`, and
`QUICK_UI_DASH_FRAME_BUDGET_US=33333`. Only `VELLO_SPARSE_FINE=0|1` changes.
All six runs redraw all 1,000 events, skip none, and report zero CPU screenshot
readback bytes. No build or test process runs alongside these measurements.

| Metric | Previous prefix renderer (`0`) | Sparse fine (`1`) | Median change |
| --- | ---: | ---: | ---: |
| Mean frame samples | 17.425 / 17.781 / 17.866 ms | 14.925 / 13.941 / 14.882 ms | -16.3% |
| Frame p99 samples | 56.285 / 65.543 / 65.291 ms | 55.189 / 35.382 / 43.569 ms | -33.3% |
| Maximum frame samples | 139.348 / 83.625 / 99.742 ms | 108.747 / 141.564 / 89.684 ms | +9.0% |
| Median mean frame | 17.781 ms | 14.882 ms | -16.3% |
| Median frame p99 | 65.291 ms | 43.569 ms | -33.3% |
| Median native fine-shaded pixels / redraw | 552,838 | 97,252 | -82.4% |
| Median tracked GPU bytes | 62,610,888 | 62,322,120 | -0.5% |
| Median process RSS | 316,460 KiB | 325,944 KiB | +3.0% |

All six acceptance gates **fail**. The median p99 is still above 33.333 ms;
neither constant 30 FPS nor constant 60 FPS has been established. Maximum-frame
latency also regresses in some runs. Animation is wall-clock driven, so the
native damage trace, fallback count, and final baseline scratch capacity vary
between runs. These are contemporaneous measured comparisons, not percentages
to compound with earlier experiments. The ARM/Virtio-GPU VM is not a physical
2010 laptop, and this offscreen test does not measure display presentation.

### Resource gates and copy-path evidence

A separate 1,000-frame off/on pair enables allocation attribution, host phase
timing, and frame breakdown, with GPU timestamps off and all seven existing
resource gates unchanged. The allocation-byte gate includes both allocations
and reallocations, with a conservative rounded-up per-redraw rate.

| Resource/profile metric | Previous prefix renderer | Sparse fine | Change |
| --- | ---: | ---: | ---: |
| Allocations / redraw, excluding reallocations | 345.157 | 351.778 | +1.9% |
| Allocated + reallocated bytes / redraw | 42,185.465 | 62,261.783 | +47.6% |
| Tracked GPU bytes | 63,692,232 | 62,322,120 | -2.2% |
| Process RSS | 316,996 KiB | 325,280 KiB | +2.6% |
| Damage-texture creations / 1,000 redraws | 32 | 0 | -100% |
| Native fine-shaded pixels / redraw | 561,604 | 96,938 | -82.7% |
| Coarse-prefix pixels / redraw | 561,604 | 778,010 | +38.5% |

Both runs pass all seven resource gates and show no positive warm-to-final GPU
or RSS growth. The sparse run reports 1,000 sparse submissions, zero dense
full/recovery attempts, 385,777 fine tiles, 75,792,088 exact written pixels,
and 778,010,008 coarse-prefix pixels across all attempts. Native fine-shaded
coverage includes masked-out pixels in selected tiles, clipped to the target;
it is not the exact store count or a count of all coarse GPU work. In particular,
coarse-prefix p99 remains 8,294,400 pixels. Dense attempts are counted when they
occur; their absence in this sparse run is not a filter or exclusion.

The baseline resource run contains 21 full/recovery and 979 partial frames.
Its frame p99 is 57.527 ms versus 42.183 ms for the sparse resource run. These
profiled values are separate from the six acceptance runs above. Zero scratch
creations, zero retained scratch bytes, and the direct sparse API establish
the eliminated damage-copy path, not a hardware measurement of total GPU copy
bandwidth. Allocation traffic is neither resident RAM nor bytes actually moved
by realloc. The increased allocation traffic and RSS remain explicit costs.

### GPU timestamp diagnosis

Separate timestamp-enabled runs use the same release executable and settings,
but no acceptance budgets. Sparse mode is measured first, then the control.
Both provide 1,000/1,000 complete frames and submissions with no missing data,
three compute passes per sample, and 2,048 bytes of diagnostic buffers.

| Device/host interval | Previous mean / p99 | Sparse mean / p99 |
| --- | ---: | ---: |
| Total frame | 20.424 / 65.259 ms | 17.863 / 47.423 ms |
| GPU first-to-last compute span | 18.198 / 57.493 ms | 15.880 / 43.343 ms |
| Sum of compute-pass intervals | 12.741 / 47.445 ms | 10.472 / 29.781 ms |
| Last compute pass (current fine stage) | 3.551 / 35.843 ms | 1.176 / 4.965 ms |
| Other compute passes combined | 9.190 / 25.071 ms | 9.296 / 28.496 ms |
| Host completion settlement | 19.615 / 64.348 ms | 17.123 / 46.708 ms |

The last-pass split validates lengths and monotonic begin/end timestamp pairs;
malformed samples are missing data, not zeros. The final pass is the fine stage
in this pipeline, not a universal shader-label guarantee. Quantiles from
different columns must not be added. Device intervals can include scheduling
or preemption; they are not exclusive GPU busy time.

The sparse slow sample at event 43 spends 52.742 ms rendering: 51.208 ms in the
GPU span, 48.661 ms in other compute, and only 1.559 ms in fine. Event 280 spends
135.902 ms rendering but only 30.808 ms in the GPU span. These observations
prioritize remaining coarse work and gaps/completion delays over more fine-pass
micro-optimization; they do not establish a driver fault or a hardware limit.

### Million-event idle check and validation

Both modes pass the explicit native-4K million-event frozen/idle check at
2 microseconds p99, zero redraws, zero allocation/reallocation calls or bytes,
and zero warm-to-final GPU/RSS growth. One-sample process wall times are
3.43 / 3.22 seconds. Tracked GPU bytes are 114,154,952 / 114,167,240 and RSS is
474,144 / 486,024 KiB: the extra sparse shader/cache setup is not free even when
the timed loop is idle. The animated 65 MB cap is not applied to these idle
runs. This is one million events, not one million objects or redraws.

Validation passes: all 40 native-4K image-preflight frames against the explicitly
unbatched, sparse-disabled full reference; 11 Vello unit tests; 17 focused UI
damage tests with the sparse GPU path enabled; baseline damage tests (the
sparse-only GPU case skips when disabled); rich clip/image parity on the
explicit CPU fallback; three benchmark damage helpers; and strict UI/Vello
clippy, including the optional profiler/debug-layer feature configuration.
The new GPU test checks exactly four changed pixels without modifying other
pixels in the same tile, and a thin far-edge region with a fullish coarse
prefix. The previously documented unrelated presence-test failure is not
claimed fixed.

After preserving the measured executable, a reporting-only correction changes
host-only slow-row timestamp labels from misleading `gpu=incomplete` to
`gpu=off`, and clarifies that dirty copies occur only when required. The raw
resource reports above retain the old labels; they are not evidence of missing
GPU samples because timestamps were disabled. The timestamp-enabled runs have
complete coverage as reported. No timed path, acceptance gate, or shader is
changed by this wording correction. After the correction, strict UI/Vello
clippy, all 17 focused sparse-enabled UI damage tests, and all six benchmark
example tests pass again.

Raw commands, exit codes, and complete output are preserved locally at
`target/ui-perf-comparison/d5078a49369b3b4d-benchmarks.txt` (ignored build artifact).

## Sparse upload reuse and allocation-free post-draw cleanup (2026-09-05)

Row, Column, Container, Card, and CardGrid now implement direct `mark_drawn`
child traversal instead of materializing temporary child vectors. Traversal
order and dirty-state semantics are unchanged. This cleanup is always enabled.

`VELLO_SPARSE_UPLOAD_REUSE=1` separately opts into a persistent, geometrically
grown storage/COPY_DST buffer for sparse fine tile records. It removes the
per-frame owned recording payload allocation/copy and reuses GPU buffer
capacity. The queue still uploads the records every redraw; neither GPU upload
bytes nor all CPU/GPU copies are eliminated. The old upload path remains the
default and the fallback for unsupported sizes. Persistent capacity is reported
as `sparse_upload` and included in tracked GPU memory; old buffer bindings are
invalidated on growth. No extra submission, wait, or reduced-resolution render
is introduced.

Release build: `cargo build -p ui --release --example ui-perf` (4m37s).
Preserved executable:
`target/ui-perf-comparison/ui-perf-sparse-upload-reuse-dispatch-timestamps-20260905`.
SHA-256:
`60c3f42e60968e9793e86bfa0f36035b4b2440be1a6a61247fffc1b6dfc634f4`.
This is a dirty-worktree build on `4000ae4bab30b1e8c477eb1633d22d071feaf150`.
It does not contain the subsequent sparse-coarse experiment.

### Same-binary upload-reuse acceptance comparison

Three alternating off/on pairs hold `VELLO_SPARSE_FINE=1`,
`VELLO_COMPUTE_BATCH=2`, `QUICK_UI_DAMAGE_GEOMETRIC=1`, both GPU timestamp flags
at `0`, and native dimensions at 3840x2160. Each run uses 1,000 events and the
unchanged 33,333-microsecond p99 frame budget. All six redraw 1,000/1,000 events,
skip none, and report zero CPU screenshot readback. Both sides include the
direct post-draw cleanup. No build or test runs concurrently.

| Metric | Upload reuse off | Upload reuse on | Median change |
| --- | ---: | ---: | ---: |
| Mean frame samples | 14.098 / 15.434 / 14.446 ms | 14.503 / 15.019 / 14.866 ms | +2.9% |
| Frame p99 samples | 42.208 / 42.571 / 39.966 ms | 36.485 / 42.701 / 41.715 ms | -1.2% |
| Maximum frame samples | 101.543 / 78.725 / 83.468 ms | 84.211 / 121.894 / 71.937 ms | +0.9% |
| Median tracked GPU bytes | 62,322,120 | 62,283,208 | -0.06% |
| Median process RSS | 324,276 KiB | 326,464 KiB | +0.7% |
| Median native fine-shaded pixels / redraw | 96,065 | 98,050 | +2.1% |

All six frame gates **fail**. The p99 change is small and mixed across pairs;
mean latency regresses. Upload reuse is not established as a frame-time win and
remains opt-in. The goal of p99 <=33.333 ms across 1,000 redraws is not reached.
Wall-clock animation changes the damage trace between runs. These percentages
must not be compounded with earlier experiments or interpreted as physical
2010-laptop/display qualification.

### Resource comparison and previous-executable control

A separate resource/host-profile off/on pair uses all seven unchanged resource
budgets, with GPU timestamps disabled. An additional run of the preserved
previous `d5078a49...` executable uses the same sparse-fine settings and budgets.
This older executable lacks both the post-draw cleanup and upload reuse.

| Resource metric | Previous executable | New, reuse off | New, reuse on |
| --- | ---: | ---: | ---: |
| Allocation calls / redraw, excluding reallocations | 351.778 | 179.778 | 178.678 |
| Allocated + reallocated bytes / redraw | 62,004.447 | 54,371.821 | 39,896.209 |
| Post-render/unclassified calls / 1,000 redraws | 172,000 | 0 | 0 |
| Post-render/unclassified bytes / 1,000 redraws | 8,256,000 | 0 | 0 |
| Tracked GPU bytes | 62,322,120 | 62,322,120 | 62,283,208 |
| Process RSS | 325,328 KiB | 324,416 KiB | 327,796 KiB |
| Sparse upload capacity | n/a | 0 | 32,768 bytes |

All three pass all seven resource gates with no positive warm-to-final GPU/RSS
growth. The old/new upload-off allocation-call difference is exactly 172 calls
per redraw, matching the eliminated temporary child-vector traversal. The
same-binary upload-reuse pair reduces allocator traffic by 26.6%; the whole
old-executable to new/reuse-on comparison reduces calls by 49.2% and traffic by
35.7%. Only the same-binary pair isolates the upload option. Byte totals also
depend on each run's animation trace; allocator traffic is not resident RAM or
a hardware measurement of bytes copied.

Both new resource runs contain 1,000 sparse submissions, no dense/full recovery
attempts, no scratch creations, and no dirty-copy encoding. Fine-shaded pixels
average 99,348 / 95,598; coarse-prefix pixels average 848,927 / 848,973 and still
reach 8,294,400 at p99. Profiled frame p99 is 38.259 / 34.671 ms, reported
separately from acceptance. The old control retains its documented obsolete
`gpu=incomplete` host-only labels; timestamps were disabled, not missing.

### Per-dispatch GPU diagnostic and its overhead

`VELLO_GPU_DISPATCH_TIMESTAMPS=1` requires `VELLO_GPU_TIMESTAMPS=1` and adapter
support for timestamps inside passes. It timestamps actual dispatch boundaries
without splitting compute passes or adding a submission/wait. CPU and unsupported
configurations explicitly report unavailable. IDs, stage names, invalid samples,
missing/stale submissions, and stage presence are checked. Stage quantiles only
include complete matching frames where that stage actually ran; absent or failed
samples are not zeros. The bounded dispatch query buffers cost 2,048 bytes in
addition to the base pass diagnostic's 2,048 bytes, plus opaque query metadata.
Acceptance budgets and image verification are rejected in timestamp mode.

The new executable, reuse on, provides 1,000/1,000 matching pass and dispatch
samples, with zero missing/stale submissions and no unknown stages. The adapter
reports dispatch status `Idle` (supported and initialized). All 16 observed
stages run once per frame. Selected stage durations are:

| Dispatch stage | Mean | p99 |
| --- | ---: | ---: |
| flatten | 2.936 ms | 7.502 ms |
| draw_leaf | 1.981 ms | 9.508 ms |
| pathtag_scan_small | 1.974 ms | 7.550 ms |
| fine_sparse_area | 1.317 ms | 5.201 ms |
| coarse | 1.230 ms | 5.484 ms |

The dispatch-enabled run measures 30.910 ms mean / 77.737 ms p99 total frame.
A separate pass-only run measures 20.102 / 69.160 ms and also has 1,000/1,000
complete samples. Its other-compute mean is 9.718 ms versus 19.972 ms with
per-dispatch instrumentation. The diagnostic materially perturbs execution;
these device intervals are not uninstrumented stage costs or exclusive GPU busy
time. Independent quantiles cannot be added. Use them to guide experiments,
then judge success with diagnostics disabled and the unchanged all-frame gate.

### Idle and correctness checks

Both upload modes pass one million frozen/static native-4K events at 2 us p99,
zero redraws, zero allocations/reallocations, and zero warm-to-final GPU/RSS
growth. They retain 114,167,240 / 114,171,336 tracked GPU bytes and report
483,596 / 484,488 KiB RSS. Warm-up retains 16,384 bytes of persistent upload
capacity in the enabled case. These are idle events, not a million redraws;
the animated 65 MB GPU budget is not applied to this idle workload.

Validation passes: all 40 native-4K image-preflight frames against the unbatched,
sparse-disabled full reference; 14 Vello tests; 17 focused damage tests;
persistent-upload growth/reuse and exact-pixel GPU parity; all six benchmark
helpers; focused Column/Card/Container tests; strict
default UI/Vello clippy and the optional profiler/debug-layer configuration.
The explicit CPU fallback rich clip/image parity test also passes with both
timestamp flags requested. The known unrelated presence-test failure remains
unresolved; a full-suite pass is not claimed.

Raw commands, exit codes, and complete output are preserved locally at
`target/ui-perf-comparison/60c3f42e60968e97-benchmarks.txt` (ignored build artifact).

## Sparse coarse selection: initial revision regressed (2026-09-05)

`VELLO_SPARSE_COARSE=1` is an additional default-off experiment requiring the
sparse GPU Area-AA path. A fixed 8 KiB uniform bitset selects native tiles;
using a uniform keeps the existing eight-storage-buffer device limit unchanged.
Empty 256x256-pixel bins return workgroup-uniformly after failure propagation.
Active bins preserve shared barriers and draw/clip ordering, but only selected
tiles contribute command bitmaps and final tile commands. The parallel bitmap
loop tests each candidate tile, not the invocation's own tile. Earlier stages,
including path counting and backdrop computation, still cover the native
prefix. Path tiling keeps its original indirect dispatch and can skip segment
writes for tiles to which coarse assigned no segment allocation. Fine rendering
and exact pixel-write masks are unchanged.

The host bitset vector is reused and fully cleared; input bounds, record
alignment, duplicates, and the last of 65,536 addressable tiles are tested.
The initial implementation still allocates/copies/uploads an 8 KiB uniform
payload each sparse frame. Its GPU storage belongs to the tracked resource
pool. CPU, non-Area, disabled, and invalid-selection cases retain dense coarse
processing. All experiments remain disabled unless explicitly requested.

### Preserved first revision and acceptance result

Build: `cargo build -p ui --release --example ui-perf` (4m37s).
Executable: `target/ui-perf-comparison/ui-perf-native-masked-sparse-coarse-20260905`.
SHA-256:
`febe90ea0e926f8f8f2ea8c01dd31479e2203d26de378ac69b64a05969ab7cfd`.
Dirty-worktree base: `4000ae4bab30b1e8c477eb1633d22d071feaf150`.

Three alternating off/on pairs use this same binary, sparse fine and upload
reuse on, compute batching `2`, geometric capacity `1`, both timestamp flags
off, and explicit native 3840x2160 output. All six include all 1,000 redraws,
zero skipped events, and zero CPU screenshot readback bytes. No build/test runs
concurrently. The unchanged p99 budget is 33,333 us.

| Metric | Coarse selection off | Initial coarse selection on | Median change |
| --- | ---: | ---: | ---: |
| Mean frame samples | 14.512 / 14.931 / 15.231 ms | 15.081 / 15.451 / 15.189 ms | +1.7% |
| Frame p99 samples | 36.425 / 36.624 / 43.467 ms | 41.944 / 68.245 / 59.704 ms | +63.0% |
| Maximum frame samples | 89.634 / 68.198 / 78.268 ms | 93.731 / 101.528 / 110.387 ms | +29.7% |
| Median tracked GPU bytes | 62,283,208 | 62,291,400 | +8,192 bytes |
| Median process RSS | 325,512 KiB | 330,004 KiB | +1.4% |

All six frame gates **fail**. Coarse selection regresses p99 in every pair and
is not a performance success. It remains off by default. The control median
also misses the 30-FPS-at-p99 target. Wall-clock animation and VM/desktop load
vary; these are paired observations, not hardware qualification or percentages
to compound with earlier results.

### Resource gates, pass-count diagnosis, and correctness

A separate same-binary resource pair passes all seven unchanged resource gates
with no positive warm-to-final GPU/RSS growth. Both record 1,000 sparse
submissions, zero full/recovery attempts, zero scratch creations, and no dirty
copy encoding. Coarse-prefix p99 remains 8,294,400 pixels.

| Resource metric | Off | Initial on |
| --- | ---: | ---: |
| Allocation calls / redraw, excluding reallocations | 178.679 | 198.686 |
| Allocated + reallocated bytes / redraw | 39,900.217 | 49,971.276 |
| Tracked GPU bytes | 62,283,208 | 62,291,400 |
| RSS | 327,152 KiB | 330,740 KiB |
| Profiled frame p99, separate from acceptance | 36.796 ms | 41.888 ms |

The option increases allocation calls by 11.2% and allocator traffic by 25.2%.
Separate pass-timestamp runs provide 1,000/1,000 matching samples in both modes,
with zero missing data and 2,048 diagnostic-buffer bytes each. Mean/p99 compute
pass counts are **3/3 off versus 4/4 on**. Total profiled frame mean/p99 is
19.640/58.153 ms off and 21.869/81.882 ms on. Device span mean/p99 is
17.647/41.795 ms off and 19.620/68.670 ms on. These diagnostic timings are not
acceptance measurements or exclusive GPU busy time.

Code inspection identifies a batching interruption: the new uniform upload
was recorded between backdrop and coarse dispatches. A subsequent source
revision moves that upload immediately after recording reset, before any
compute dispatch. This change is **not in the preserved executable above**.
Strict clippy and the GPU parity regression pass after the move, including a
new narrowly gated assertion requiring matched timestamps and at most three
passes for a single batched sparse attempt. Its release performance must be
measured separately; removal of the extra pass alone is not proof that the
overall p99 regression is solved.

Before that upload-order revision, validation passes all 40 native-4K reference
frames, 16 Vello tests, 17 damage tests, 13 sparse-related UI tests, six benchmark
helpers, strict default and optional profiler/debug-layer clippy, and explicit
CPU fallback parity. The focused coarse test crosses 512-pixel bin boundaries
on a 544x528 target with clips and overlapping images, checks untouched pixels
in active and empty bins, and verifies dispatch stage 24 rather than dense
coarse stage 17 when timestamps are supported. Sparse-fine-disabled fallback
also passes. The known unrelated presence-suite failure is not claimed fixed.

Both million-event native-4K frozen/idle runs pass at 2 us p99 with zero redraws,
allocations/reallocations, and warm-to-final growth. Tracked GPU bytes are
114,171,336 / 114,179,528 and RSS is 486,536 / 489,680 KiB; the animated 65 MB
GPU cap is not applied to idle warm-up storage.

Raw commands, exit codes, and complete output are preserved locally at
`target/ui-perf-comparison/febe90ea0e926f8f-benchmarks.txt` (ignored build artifact).

### Upload-order revision: batching fixed, p99 still regresses

The revised executable moves only the coarse-selector uniform upload ahead of
all compute dispatches, after recording reset. Its shader and acceptance rules
are unchanged. Build: `cargo build -p ui --release --example ui-perf` (4m30s).
Executable:
`target/ui-perf-comparison/ui-perf-native-sparse-coarse-early-upload-20260905`.
SHA-256:
`39e42517445836ea8538592668782f5278bcd1681ad57fd98c0dfa8fa7f01403`.
The dirty-worktree base remains `4000ae4bab30b1e8c477eb1633d22d071feaf150`.

Three fresh alternating off/on pairs use this same executable and all settings
from the preceding acceptance comparison. Every run again includes all 1,000
native-4K redraws, skips none, and reports zero CPU screenshot readback. Both GPU
diagnostics are off; no build/test runs concurrently.

| Metric | Coarse selection off | Revised coarse selection on | Median change |
| --- | ---: | ---: | ---: |
| Mean frame samples | 14.162 / 13.772 / 14.955 ms | 13.876 / 13.739 / 16.054 ms | -2.0% |
| Frame p99 samples | 36.774 / 39.060 / 42.574 ms | 36.879 / 43.292 / 66.496 ms | +10.8% |
| Maximum frame samples | 78.585 / 68.951 / 76.867 ms | 73.929 / 104.351 / 73.855 ms | -3.8% |
| Median tracked GPU bytes | 62,283,208 | 62,291,400 | +8,192 bytes |
| Median process RSS | 325,960 KiB | 329,480 KiB | +1.1% |

All six frame gates **fail**, and enabled p99 is worse in every pair. The
revised coarse experiment still does not qualify for adoption and remains off.
Removing the batching interruption is verified separately, but does not solve
the overall p99 regression. Neither this option nor the control meets the
33.333 ms p99 target; the latest control median is 39.060 ms. Different build
batches are not a same-binary measurement of the upload-order change itself.

The revised resource pair passes all seven unchanged budgets with no positive
warm-to-final GPU/RSS growth. Both runs have 1,000 sparse submissions, zero
full/recovery attempts, zero scratch creations, and no dirty-copy encoding.

| Resource metric | Off | Revised on |
| --- | ---: | ---: |
| Allocation calls / redraw, excluding reallocations | 178.679 | 185.685 |
| Allocated + reallocated bytes / redraw | 39,899.025 | 48,678.474 |
| Tracked GPU bytes | 62,283,208 | 62,291,400 |
| RSS | 326,132 KiB | 331,368 KiB |
| Profiled frame p99, separate from acceptance | 36.270 ms | 37.181 ms |

Enabled allocation calls remain 3.9% higher and allocator traffic 22.0% higher.
The native coarse-prefix p99 remains 8,294,400 pixels in both modes. Fewer tile
command writes do not eliminate prefix scans, path counting, or backdrop work.

A separate revised-on pass diagnostic has 1,000/1,000 matching samples, zero
missing samples, and **mean/p99 compute-pass counts of 3/3**, confirming the
extra pass is removed in the release executable. It reports total frame mean/p99
19.363/70.244 ms, GPU span 17.071/56.439 ms, and summed pass intervals
10.665/42.640 ms. These instrumented intervals are not acceptance results or
exclusive GPU busy time. Diagnostic buffers remain 2,048 bytes.

The revised release again passes all 40 native-4K pixel-preflight frames.
Strict default clippy and the enhanced GPU parity/pass-count regression pass
after the source move. Both revised million-event frozen/idle modes pass at
2 us p99, zero redraws and allocation/reallocation traffic, and zero warm-to-final
growth. Tracked GPU bytes are 114,171,336 / 114,179,528 and RSS is
484,468 / 490,164 KiB. As before, these are idle events, not a million redraws,
and the animated GPU capacity cap does not apply to their warm-up storage.

Raw commands, exit codes, and complete output are preserved locally at
`target/ui-perf-comparison/39e42517445836ea-benchmarks.txt` (ignored build artifact).

## Selective CPU path-tag scan experiment (2026-09-05)

`VELLO_CPU_PATHTAG=1` moves only path-tag reduction and the exclusive prefix
scan to the existing CPU implementations. It is exact-match, default-off,
and distinct from full CPU coarse rendering. Scene/config inputs already exist
in host memory; geometry flattening, drawing, clipping, path counting, coarse,
and fine rendering remain GPU stages. There is no new GPU-to-CPU readback,
submission, or synchronization wait. The computed tag-monoid buffer still
requires a CPU-to-GPU upload every redraw: this is not a copy-elimination claim.
GPU tag pipelines remain compiled, and disabling the option restores them.
The full-frame verification renderer explicitly disables CPU path tags as well
as batching and sparse fine, preserving an independent GPU reference.

Executable: `target/ui-perf-comparison/ui-perf-native-cpu-pathtag-20260905`.
SHA-256: `92bb167acd9baf82e2a3dc02c64e3130ddab38350274438ba4d213c1beec0c59`.
Build: `cargo build -p ui --release --example ui-perf` (4m45s), from dirty
base `4000ae4bab30b1e8c477eb1633d22d071feaf150`.

Three alternating off/on pairs use that same executable, native 3840x2160,
1,000 animated events, sparse fine and sparse upload reuse enabled, sparse
coarse disabled, compute batching `2`, and geometric capacity `1`. Both GPU
diagnostics are disabled. Every run redraws all 1,000 events, skips none,
includes every slow frame, and reports zero CPU screenshot readback. No builds
or tests run concurrently. The unchanged frame gate is 33,333 us p99.

| Metric | GPU path tags | CPU path tags | Median change |
| --- | ---: | ---: | ---: |
| Mean frame samples | 14.805 / 14.752 / 14.704 ms | 13.244 / 12.560 / 13.600 ms | -10.2% |
| Frame p99 samples | 39.268 / 40.808 / 39.903 ms | 38.481 / 34.642 / 41.399 ms | -3.6% |
| Maximum frame samples | 102.016 / 132.281 / 68.293 ms | 77.973 / 97.902 / 102.942 ms | -4.0% |
| Median tracked GPU bytes | 62,283,208 | 62,282,184 | -1,024 bytes |
| Median RSS | 325,964 KiB | 325,956 KiB | effectively unchanged |

All six frame gates **fail**. Mean latency improves in every pair, but p99
improves in only two of three pairs. This does not meet the target and remains
an opt-in experiment. It does not qualify the physical 2010-laptop/display goal.

The separate allocation-profiled pair uses the same seven resource budgets as
before. GPU mode passes all seven; CPU mode fails allocator-byte traffic and
returns early from budget checking, so it is not reported as passing the rest.

| Resource metric | GPU path tags | CPU path tags |
| --- | ---: | ---: |
| Allocation calls / redraw, excluding reallocations | 178.679 | 183.376 |
| Allocated + reallocated bytes / redraw | 39,897.513 | 281,292.418 |
| Tracked GPU bytes | 62,283,208 | 62,282,184 |
| RSS | 327,128 KiB | 326,280 KiB |
| Profiled p99, not acceptance | 37.003 ms | 37.513 ms |

Fresh CPU tag buffers raise allocation traffic by **605.0%**, exceeding the
70,000-byte/redraw cap. Both runs retain bounded GPU/RSS growth, use 1,000 sparse
submissions, and have zero scratch creations or dirty-copy encoding. A follow-up
will reuse only those CPU buffers with a hard retention cap; it must be measured
separately and must not count host allocations as GPU memory.

The diagnostic run has 1,000/1,000 complete pass and dispatch samples, no missing
or stale samples, and three compute passes per redraw. GPU stages 1 (reduce)
and 4 (scan-small) are absent; the remaining 14 stages, including flatten and
sparse fine, each execute 1,000 times. Diagnostic total mean/p99 is
32.839/96.292 ms, illustrating why instrumented timings are excluded from
acceptance; dispatch intervals include scheduling effects and are not exclusive
GPU busy time. The pass and dispatch readback buffers each use 2,048 bytes.

Validation passes 40/40 native-4K reference frames; the focused clipped-curve,
image-update, tag-change, and runtime-disable GPU parity regression; a pure
257-workgroup reduce/scan boundary test with nonzero tag base and exact prefix
checks; 17 damage tests; sparse UI regressions; explicit full-CPU fallback
parity; six benchmark helpers; and strict default plus optional profiler/debug
clippy. The known unrelated presence-suite failure is not claimed fixed.

Both million-event native-4K frozen/idle runs pass at 2 us p99 with zero redraws,
allocations/reallocations, and warm-to-final growth. GPU bytes are
114,171,336 / 114,170,312; RSS is 486,336 / 485,200 KiB. These are idle events,
not a million rendered frames, and do not use the animated GPU memory cap.

Raw commands, exit codes, and complete output are preserved locally at
`target/ui-perf-comparison/92bb167acd9baf82-benchmarks.txt` (ignored build artifact).

### Bounded CPU tag-buffer reuse: allocation regression fixed, p99 still fails

`VELLO_CPU_PATHTAG_REUSE=1` adds two exact-label host-vector slots only for
selective CPU path tags, not global CPU rendering. Each slot retains at most
1 MiB of vector capacity (2 MiB total); unknown labels and oversized vectors
bypass retention. Reused buffers have the exact requested length and are zeroed.
The reduced buffer returns at deferred free, and the tag-monoid vector returns
after `queue.write_buffer` copies its contents. Disabling CPU path tags or clearing
the renderer pool drops retained vectors. Their capacity is reported separately
from GPU memory. CPU computation and the per-redraw GPU upload remain.

Executable: `target/ui-perf-comparison/ui-perf-native-cpu-pathtag-reuse-20260905`.
SHA-256: `a7f3f3beb811b07a9c2eea815b05b127cedca685aa6302ac790ed565ad8c44ac`.
Release build takes 4m37s from the same dirty-worktree base. Three fresh rounds
each run GPU tags, CPU tags without reuse, then CPU tags with reuse, using this
same executable and the preceding native-4K settings. All nine runs redraw all
1,000 events, skip none, include all slow frames, and have zero screenshot
readback. GPU diagnostics remain disabled, and no build/test runs concurrently.

| Metric | GPU tags | CPU tags, no reuse | CPU tags, reuse |
| --- | ---: | ---: | ---: |
| Mean frame samples, ms | 13.750 / 15.687 / 15.275 | 12.368 / 12.968 / 13.895 | 13.876 / 13.227 / 14.195 |
| Frame p99 samples, ms | 36.155 / 52.248 / 40.880 | 36.019 / 38.276 / 40.405 | 39.225 / 40.202 / 39.982 |
| Maximum frame samples, ms | 67.133 / 73.155 / 80.838 | 74.958 / 108.371 / 247.256 | 91.838 / 72.085 / 71.836 |
| Median mean frame | 15.275 ms | 12.968 ms | 13.876 ms |
| Median frame p99 | 40.880 ms | 38.276 ms | 39.982 ms |
| Tracked GPU bytes | 62,283,208 | 62,282,184 | 62,282,184 |
| Median RSS | 325,776 KiB | 326,368 KiB | 326,788 KiB |
| Retained CPU pool capacity | 0 | 0 | 241,580 bytes |

All nine 33.333 ms p99 gates **fail**. Against GPU tags, CPU tags with reuse
reduce median mean time by 9.2% and p99 by 2.2%, with RSS 0.3% higher. Against
uncached CPU tags, reuse makes median mean time 7.0% worse and p99 4.5% worse;
mean is worse in all three paired rounds. It is therefore an allocation fix,
not a demonstrated frame-time improvement. The experimental modes remain off
by default and do not qualify the target.

Three separate resource runs retain the seven unchanged budgets. GPU tags and
CPU tags with reuse pass all seven; uncached CPU tags again fail allocation
traffic and return before later budget checks.

| Resource metric | GPU tags | CPU tags, no reuse | CPU tags, reuse |
| --- | ---: | ---: | ---: |
| Allocation calls / redraw, excluding reallocations | 178.679 | 183.375 | 181.378 |
| Allocated + reallocated bytes / redraw | 39,896.531 | 281,290.450 | 39,916.848 |
| RSS | 328,372 KiB | 327,820 KiB | 327,184 KiB |
| Profiled p99, not acceptance | 36.788 ms | 36.632 ms | 41.462 ms |

Reuse cuts CPU-tag allocator traffic by **85.8%**, back to approximately the
GPU-tag baseline (+0.05%). This saving is not a reduction in upload bytes or
GPU memory. The host pool is 241,580 bytes after warm-up and at the end. All
resource runs have zero positive warm-to-final GPU/RSS growth, 1,000 sparse
submissions, zero scratch creations, and no dirty-copy encoding.

The reused release passes all 40 native-4K reference frames, 19 Vello tests,
the enhanced CPU/GPU stage-and-image parity test including pool reset on runtime
disable, 17 damage tests, sparse regressions, explicit full-CPU fallback,
six benchmark helpers, and strict default/optional profiler/debug clippy.
The known unrelated presence-suite failure is not claimed fixed.

A separate dispatch diagnostic has 1,000/1,000 complete pass and dispatch
samples, zero missing/stale samples, and three compute passes per redraw.
The same 14 GPU stages execute 1,000 times each, with path-tag stages 1/4 absent.
Diagnostic total mean/p99 is 30.840/73.473 ms, not an acceptance result. Retained
host capacity remains 241,580 bytes; each diagnostic readback buffer is 2,048 B.

Both GPU-tag and reused-CPU-tag million-event native-4K frozen/idle runs pass at
2 us p99, zero redraws/allocations/reallocations, and zero warm-to-final growth.
GPU bytes are 114,171,336 / 114,170,312, RSS is 484,172 / 484,224 KiB, and retained
host-pool capacity is 0 / 241,580 bytes. These remain idle events, not redraws.

Raw commands, exit codes, and complete output are preserved locally at
`target/ui-perf-comparison/a7f3f3beb811b07a-benchmarks.txt` (ignored build artifact).

## Selective CPU metadata experiment (2026-09-05)

`VELLO_CPU_METADATA=1` extends the CPU setup experiment to bbox-clear and
draw-reduce only. It is default-off, requires selective CPU path tags, and is
inactive in full-CPU rendering. Flatten, draw-leaf, clipping, binning, path
counting, coarse, and fine remain GPU work. CPU draw-reduce is recorded before
the first GPU use of config/scene: moving it to CPU at its former post-flatten
position would otherwise access inputs already materialized on GPU. The default
GPU and full-CPU paths retain their existing order. Runtime disable restores
GPU stages, and the full-frame reference disables metadata and path tags.

Metadata output vectors have two independent exact-label host slots: at most
1 MiB for path bboxes and 4 KiB for reduced draw tags. They are zeroed on reuse,
bypass retention above their caps, and clear when disabled or trimmed. Their
retained capacity is reported separately from tag-host capacity and GPU memory.
Their contents still upload every redraw; no readback or new wait is introduced.

Executable: `target/ui-perf-comparison/ui-perf-native-cpu-metadata-20260905`.
SHA-256: `b5dc747ddfa9af3fcdebd86d9d0193adcc0cb2ff5cbfa926bcea1c89ce6bc351`.
Release build takes 4m38s from dirty base
`4000ae4bab30b1e8c477eb1633d22d071feaf150`.

Three rounds each run GPU tags, CPU tags with reuse, then CPU tags with reuse
plus CPU metadata. Other settings remain sparse fine/upload reuse on, sparse
coarse off, compute batching `2`, geometric capacity `1`, both GPU diagnostics
off, native 3840x2160, and the unchanged 33,333 us gate. All nine runs include
all 1,000 actual redraws, skip none, and report zero screenshot readback. No
build/test runs concurrently.

| Metric | GPU tags | CPU tags + reuse | Plus CPU metadata |
| --- | ---: | ---: | ---: |
| Mean samples, ms | 13.469 / 14.996 / 14.440 | 13.699 / 13.680 / 13.419 | 13.163 / 12.907 / 13.654 |
| P99 samples, ms | 36.286 / 38.073 / 40.655 | 38.624 / 39.348 / 35.826 | 42.099 / 39.578 / 38.999 |
| Maximum samples, ms | 71.900 / 100.955 / 71.173 | 103.738 / 105.027 / 129.452 | 71.322 / 73.291 / 73.361 |
| Median mean | 14.440 ms | 13.680 ms | 13.163 ms |
| Median p99 | 38.073 ms | 38.624 ms | 39.578 ms |
| Tracked GPU bytes | 62,283,208 | 62,282,184 | 62,282,184 |
| Median RSS | 326,228 KiB | 326,660 KiB | 326,684 KiB |
| Final tag-host / metadata-host bytes | 0 / 0 | 241,580 / 0 | 241,580 / 14,520 |

All nine frame gates **fail**. Metadata improves median mean time 3.8% against
reused CPU tags but worsens median p99 2.5%; p99 is worse in every paired round.
Against GPU tags, the combined setup improves mean 8.8% but worsens p99 4.0%.
It is not adopted as a p99 improvement and remains opt-in. GPU bytes are
unchanged versus CPU tags; RSS is essentially unchanged in that comparison.

All three separate resource-profiled runs pass all seven unchanged budgets.

| Resource metric | GPU tags | CPU tags + reuse | Plus CPU metadata |
| --- | ---: | ---: | ---: |
| Allocation calls / redraw, excluding reallocations | 178.677 | 181.378 | 177.118 |
| Allocated + reallocated bytes / redraw | 39,892.643 | 39,953.152 | 37,317.493 |
| Reported process RSS | 327,256 KiB | 328,296 KiB | 326,536 KiB |
| Profiled p99, not acceptance | 39.395 ms | 37.347 ms | 41.539 ms |

Metadata reduces allocation calls by 2.3% and allocator traffic by 6.6% versus
reused CPU tags. All three runs have zero positive warm-to-final GPU/RSS growth,
1,000 sparse submissions, zero full/recovery samples, zero scratch creations,
and no dirty-copy encoding. The RSS gate samples process memory again later;
its metadata-run reading is 328,584 KiB and also passes the 350,000 KiB cap.

Separate same-executable dispatch diagnostics confirm the structural change:
both modes have 1,000/1,000 complete pass and dispatch samples, zero missing or
stale samples. Metadata removes GPU bbox-clear (6) and draw-reduce (8), leaving
12 GPU stages each executed 1,000 times; tag stages 1/4 were already absent.
Compute-pass mean/p99 drops from **3/3 to 2/2**, because bbox-clear no longer
needs a GPU pass before the bump-buffer clear. This is not sufficient to improve
acceptance p99. Instrumented total mean/p99 is 29.163/71.087 ms without metadata
and 29.902/92.428 ms with it; these perturbed diagnostics are not acceptance
results or exclusive GPU busy-time claims. Each diagnostic buffer remains 2,048 B.

Validation passes 40/40 native-4K reference frames, 22 Vello unit tests, the
extended stage/image parity test and runtime metadata disable, 17 damage tests,
sparse regressions, full-CPU fallback, metadata requested without CPU tags,
sparse-disabled parity, six benchmark helpers, and strict default/optional
profiler/debug clippy. The known unrelated presence-suite failure remains.

GPU-only and combined-metadata million-event frozen/idle runs both pass at
2 us p99 with zero redraws, allocations/reallocations, and warm-to-final GPU/RSS
growth. GPU bytes are 114,171,336 / 114,170,312; reported RSS is
484,180 / 485,092 KiB. Combined final host pools are 241,580 tag bytes plus
13,656 metadata bytes. These are idle events, not a million rendered frames.

Raw commands, exit codes, and complete output are preserved locally at
`target/ui-perf-comparison/b5dc747ddfa9af3f-benchmarks.txt` (ignored build artifact).

## Bounded CPU draw-prefix experiment (2026-09-05)

`VELLO_CPU_DRAW_PREFIX=1` is a default-off experiment requiring selective CPU
path tags, independently of CPU metadata. It computes the exclusive integer
draw-monoid prefix in host memory before the first GPU use of scene/config.
A GPU draw-leaf permutation reads that table, omitting its workgroup scans and
barriers; its floating-point geometry/brush decode body remains unchanged.
Draw-reduce is skipped when this route is eligible. The existing GPU monoid
buffer is used, not a second GPU table. The CPU table still uploads each redraw.

An independent, zero-on-reuse host slot is capped at 1 MiB. Larger draw tables
take the original GPU path. Disabling the option clears its host slot and
restores GPU draw-leaf. The pure CPU test covers empty input and draw counts
1, 255, 256, 257, and 513; the real renderer eligibility API is checked at
16 bytes, exactly 1 MiB, and 1 MiB plus 16 bytes.

Executable: `target/ui-perf-comparison/ui-perf-native-cpu-draw-prefix-20260905`.
SHA-256: `28cd4f511be6fbcc84336802cc1a0b04941ddffc71ce417e115bf29a59e6e7da`.
Release build: 4m47s, dirty base
`4000ae4bab30b1e8c477eb1633d22d071feaf150`.

Three same-executable rounds rotate order: tags/prefix/combined,
prefix/combined/tags, combined/tags/prefix. "Tags" is CPU path tags with host
reuse; "prefix" adds CPU draw-prefix; "combined" also adds CPU metadata.
All use native 3840x2160, sparse fine and upload reuse, sparse coarse off,
compute batching `2`, geometric capacity `1`, and both GPU diagnostics off.
Every run contains all 1,000 actual redraws, zero skipped events, and zero
screenshot readback. No build or test runs concurrently with measurement.

| Metric | CPU tags + reuse | Plus draw-prefix | Plus prefix + metadata |
| --- | ---: | ---: | ---: |
| Mean samples, ms | 12.680 / 13.703 / 13.010 | 12.907 / 12.238 / 13.745 | 11.285 / 12.750 / 12.695 |
| P99 samples, ms | 34.828 / 42.192 / 37.336 | 35.776 / 36.279 / 63.680 | 34.168 / 39.552 / 57.636 |
| Maximum samples, ms | 76.373 / 75.446 / 100.090 | 96.875 / 99.421 / 101.416 | 69.483 / 72.556 / 71.727 |
| Median mean | 13.010 ms | 12.907 ms | 12.695 ms |
| Median p99 | 37.336 ms | 36.279 ms | 39.552 ms |
| Tracked GPU bytes | 62,282,184 | 62,282,136 | 62,282,136 |
| Median RSS | 327,608 KiB | 327,384 KiB | 327,500 KiB |
| Final tag / metadata / prefix host bytes | 241,580 / 0 / 0 | 241,580 / 0 / 9,648 | 241,580 / 14,472 / 9,648 |

**All nine 33,333 us frame gates fail.** Prefix alone changes median mean by
-0.8% and median p99 by -2.8%, but its p99 is worse in two of the three paired
rounds and its third p99 is 63.680 ms. This is not a reliable tail-latency win.
The combined mode changes median mean by -2.4% but worsens median p99 by 5.9%.
Neither route is adopted as satisfying the p99 target; both remain opt-in.
The 48-byte GPU reduction is not a meaningful GPU-memory saving, and host
retention increases.

All three separate resource-profiled runs pass all seven unchanged budgets.

| Resource metric | CPU tags + reuse | Plus draw-prefix | Plus prefix + metadata |
| --- | ---: | ---: | ---: |
| Allocation calls / redraw, excluding reallocations | 181.377 | 185.042 | 177.087 |
| Allocated + reallocated bytes / redraw | 39,948.230 | 41,348.197 | 37,413.824 |
| Reported RSS | 327,044 KiB | 327,448 KiB | 327,232 KiB |
| Profiled p99, not acceptance | 40.118 ms | 35.901 ms | 38.468 ms |

Prefix alone increases allocator traffic 3.5% and allocation calls 2.0%.
Combined metadata/prefix reduces traffic 6.3% and calls 2.4% versus tags.
All three report 1,000 sparse submissions, zero full/recovery samples, zero
scratch creation or dirty-copy encoding, and zero positive warm-to-final
GPU/RSS growth. Allocator traffic does not measure CPU-to-GPU upload traffic.

Separate diagnostics confirm 1,000/1,000 complete pass and dispatch samples in
each mode, with zero missing/stale samples. GPU stage counts are 14 / 13 / 12;
prefix modes replace draw-leaf (9) with draw-leaf-prefix (25), remove draw-reduce
(8), and combined metadata also removes bbox-clear (6). Each present stage runs
1,000 times. Compute-pass mean/p99 is 3/3, 3/3, and 2/2 respectively.
Instrumented total mean/p99 is 29.141/77.868, 29.241/93.559, and 25.824/70.972 ms;
these perturbed diagnostics are not acceptance timings or exclusive GPU busy
time. Each timestamp buffer remains 2,048 bytes.

Validation passes 40/40 native-4K reference images, 24 Vello tests, two shader
tests, 17 damage tests, sparse regressions, six benchmark helpers, strict
default and optional profiler/debug clippy, and focused image/stage/runtime
disable tests with metadata on/off and sparse fine off. Full-CPU and no-CPU-tags
fallback checks pass. The sparse-coarse-specific test skips because that
unrelated option is off; the known unrelated presence-suite failure remains.

Both tags and combined million-event frozen/idle runs pass at 2 us p99, with
zero redraws, allocations/reallocations, or warm-to-final GPU/RSS growth.
GPU bytes are 114,170,312 / 114,170,264 and reported RSS is
485,816 / 485,196 KiB. Combined final host retention is 241,580 tag bytes,
13,608 metadata bytes, and 9,072 prefix bytes. These are idle events, not one
million redraws; this experiment provides no measured idle speedup.

Raw commands, exit codes, and complete output are preserved locally at
`target/ui-perf-comparison/28cd4f511be6fbcc-benchmarks.txt` (ignored build artifact).

## Deferred synchronous bump-readback experiment (2026-09-05)

`VELLO_DEFER_BUMP_READBACK=1` is a default-off command-order experiment, stored
once per renderer and excluded from full-CPU rendering. In synchronous GPU
full/sparse recordings it moves the existing 32-byte allocation-counter
download and its resource free from after coarse processing to after fine
rasterization. Fine does not bind or change those counters. It does not remove
the copy, map, measurement, wait, or overflow recovery. Async between-phase
readback and debug intermediate capture retain their existing ordering.
The full-frame benchmark reference explicitly disables the experiment.

Executable: `target/ui-perf-comparison/ui-perf-native-deferred-bump-20260905`.
SHA-256: `d53139e20e9161bd093d68f2e504f002bf352c9e55d7f43e60a152b4abecc4ff`.
Release build: 4m32s, dirty base
`4000ae4bab30b1e8c477eb1633d22d071feaf150`.

Three rounds rotate early/late/late-without-prefix, then
late/late-without-prefix/early, then late-without-prefix/early/late.
All modes use CPU path tags with host reuse and CPU metadata. Early and late
also use CPU draw-prefix; the third mode leaves GPU draw-leaf unchanged.
Sparse fine/upload reuse, batching `2`, geometric capacity `1`, and native
3840x2160 remain enabled; sparse coarse and both GPU diagnostics remain off.
Every timed run includes all 1,000 redraws, zero skips, and zero screenshot
readback. Builds and tests do not overlap measurement.

| Metric | Early copy (control) | Deferred copy | Deferred, no CPU draw-prefix |
| --- | ---: | ---: | ---: |
| Mean samples, ms | 11.576 / 11.461 / 11.692 | 13.374 / 11.264 / 12.259 | 12.688 / 13.399 / 12.562 |
| P99 samples, ms | 33.095 / 34.849 / 36.186 | 49.125 / 37.683 / 42.897 | 41.288 / 43.863 / 41.332 |
| Maximum samples, ms | 64.801 / 64.981 / 69.329 | 97.659 / 69.966 / 73.810 | 101.003 / 99.786 / 101.485 |
| Median mean | 11.576 ms | 12.259 ms | 12.688 ms |
| Median p99 | 34.849 ms | 42.897 ms | 41.332 ms |
| 33,333 us frame gate | PASS / FAIL / FAIL | FAIL / FAIL / FAIL | FAIL / FAIL / FAIL |
| Tracked GPU bytes | 62,282,136 | 62,282,136 | 62,282,184 |
| Median RSS | 327,496 KiB | 327,368 KiB | 327,300 KiB |

The first control run passes at 33.095 ms p99, but its next two runs fail; this
is not a reproducible constant-frame-rate result. The new deferred-copy path
worsens median mean 5.9% and median p99 23.1%; its p99 is worse in every paired
round. Removing CPU draw-prefix from that path still fails all three gates.
**The new ordering is not adopted and remains off.** A lower compute-pass count
is not accepted as a substitute for the frame benchmark.

All three separate resource runs pass all seven unchanged budgets.

| Resource metric | Early copy | Deferred copy | Deferred, no CPU draw-prefix |
| --- | ---: | ---: | ---: |
| Allocation calls / redraw, excluding reallocations | 176.910 | 165.087 | 165.107 |
| Allocated + reallocated bytes / redraw | 37,444.904 | 35,114.530 | 35,209.010 |
| Reported RSS | 327,556 KiB | 327,352 KiB | 327,928 KiB |
| Profiled p99, not acceptance | 35.986 ms | 39.138 ms | 36.463 ms |

Deferral reduces allocator traffic 6.2% and allocation calls 6.7%, but does not
reduce tracked GPU memory. All resource runs contain 1,000 sparse submissions,
zero full/recovery samples, zero scratch creations or dirty-copy encoding, and
zero positive warm-to-final GPU/RSS growth. Early and late final host pools are
241,580 tag bytes, 14,472 metadata bytes, and 9,648 prefix bytes; without CPU
draw-prefix they are 241,580 / 14,520 / 0 bytes.

Matched diagnostics confirm the intended structural change: compute-pass
mean/p99 falls from **2/2 to 1/1** in both deferred variants. All modes have
1,000/1,000 complete pass and dispatch samples, zero missing/stale samples,
and the same 12 GPU dispatches per redraw. The no-prefix variant uses stage 9
instead of 25. Each present stage executes 1,000 times. Instrumented total
mean/p99 is 26.876/71.734, 24.920/68.422, and 27.455/82.692 ms respectively.
These perturbed diagnostics are not acceptance timings or exclusive GPU busy
time. Both diagnostic buffers remain 2,048 bytes each.

Both candidate release variants pass 40/40 native-4K image comparisons.
Validation also passes 26 Vello unit tests, 17 damage tests, sparse regressions,
six benchmark helpers, strict default/optional profiler/debug clippy, dense
pass-count tests with metadata on/off, runtime disable, full-CPU/GPU-tag
fallbacks, and a pinned-budget smoke test. A dedicated 512-overlapping-rectangle
test actually overflows the initial tile allocation (`failed=0x2`), requires
at least two full attempts, and matches the unbatched oracle after recovery.
Async/debug call-site ordering was inspected and optional-feature compilation
passed; no runtime async/debug benchmark is claimed. The known unrelated
presence-suite failure remains.

Early and deferred million-event native-4K frozen/idle runs both pass at 2 us
p99, zero redraws/allocations/reallocations, and zero warm-to-final GPU/RSS
growth. Both retain 114,170,264 tracked GPU bytes; reported RSS is
485,272 / 487,572 KiB. Both final host pools are 241,580 tag bytes, 13,608 metadata
bytes, and 9,072 prefix bytes. There is no measured idle speedup.

Raw commands, exit codes, and complete output are preserved locally at
`target/ui-perf-comparison/d53139e20e9161bd-benchmarks.txt` (ignored build artifact).

## Direct GPU path-dispatch experiment (2026-09-05)

**The native-4K 1,000-redraw 33,333 us p99 gate now passes in all three
uninstrumented direct-path runs.** The selected measured profile enables
`VELLO_DIRECT_PATH_DISPATCH=1` and leaves deferred bump readback off.
The experiment remains opt-in; this is not a claim that the default renderer
or a physical 2010 laptop has been qualified.

Direct path-count and path-tiling use 128 workgroups of 256 lanes, with
grid-stride loops over the actual GPU-produced line/segment counts. This
replaces the two GPU setup dispatches plus indirect dispatch arguments.
It removes the 16-byte indirect-count buffer, not CPU readback. The existing
32-byte bump-counter download, wait, overflow accounting, and recovery remain.
Failure guards preserve the prior pipeline's failure signal, including a
nonempty tiling launch setting `ptcl[0]` on failure with zero segments.
The original indirect path and global-CPU fallback remain available.

Executable: `target/ui-perf-comparison/ui-perf-native-direct-path-20260905`.
SHA-256: `6724f6222e6795e2992bf2600a637268383bd0e30ee6f1b82e3aa5e2faea6efe`.
Release build: 4m47s, dirty base
`4000ae4bab30b1e8c477eb1633d22d071feaf150`.

Three rounds rotate control/direct/direct+deferred, then
direct/direct+deferred/control, then direct+deferred/control/direct.
All modes use CPU path tags with host reuse, CPU metadata, CPU draw-prefix,
sparse fine/upload reuse, batching `2`, and geometric capacity `1`.
Sparse coarse and both GPU diagnostics are off. All 1,000 animated redraws
are included in each timing run; zero events are skipped. The output is native
3840x2160 with 1:1 damage rasterization, not a smaller image upscaled to 4K.
Partial redraws shade damage, not all 8,294,400 pixels on every frame.
No builds or tests overlap measurements.

| Metric | Control (indirect) | Direct | Direct + deferred bump copy |
| --- | ---: | ---: | ---: |
| Mean samples, ms | 11.955 / 12.553 / 12.639 | 7.636 / 8.150 / 7.582 | 7.547 / 7.674 / 7.722 |
| P99 samples, ms | 35.900 / 40.662 / 36.265 | 22.202 / 27.692 / 24.577 | 23.997 / 24.760 / 32.288 |
| Maximum samples, ms | 107.812 / 111.723 / 69.287 | 45.258 / 42.741 / 69.461 | 90.080 / 80.997 / 80.791 |
| Median mean | 12.553 ms | 7.636 ms | 7.674 ms |
| Median p99 | 36.265 ms | 24.577 ms | 24.760 ms |
| 33,333 us frame gate | FAIL / FAIL / FAIL | PASS / PASS / PASS | PASS / PASS / PASS |
| Tracked GPU bytes | 62,282,136 | 62,282,120 | 62,282,120 |
| Median RSS | 328,524 KiB | 328,564 KiB | 328,996 KiB |

Direct reduces median mean time **39.2%** (1.64x mean throughput) and median
p99 **32.2%** versus its same-binary control. Its p99 improves in every paired
round. The slowest direct p99 is 27.692 ms, below 33.333 ms; nevertheless its
maximum is 69.461 ms, so this is a p99 result, not a guarantee about every frame.
It does not meet a 16.667 ms p99 / constant-60-fps target. GPU/RSS footprint is
essentially unchanged. Direct+deferred also passes the three timing gates but
has less p99 margin and no mean advantage, so deferred readback remains off
in the selected profile.

All three separate resource runs pass all seven unchanged budgets.

| Resource metric | Control | Direct | Direct + deferred |
| --- | ---: | ---: | ---: |
| Allocation calls / redraw, excluding reallocations | 177.086 | 172.646 | 160.657 |
| Allocated + reallocated bytes / redraw | 37,205.626 | 35,566.688 | 33,508.979 |
| Reported RSS | 329,868 KiB | 328,008 KiB | 327,596 KiB |
| Profiled p99, not acceptance | 40.449 ms | 27.408 ms | 39.664 ms |

Direct reduces allocation calls 2.5% and allocator traffic 4.4%; these are
allocator metrics, not GPU-upload byte counts. Every resource run includes
1,000 sparse submissions, zero full/recovery samples, zero scratch creations
or dirty-copy encoding, and zero positive warm-to-final GPU/RSS growth.
Final host pools remain 241,580 tag bytes, 14,472 metadata bytes, and 9,648
prefix bytes, explicitly excluded from GPU memory accounting.

Matched diagnostics confirm 12 -> 10 GPU dispatches per redraw. Control
stages 14/15/18/19 (setup/count/setup/tiling) are replaced by direct stages
26/27. Each present stage runs exactly 1,000 times; pass and dispatch samples
are 1,000/1,000 complete with zero missing/stale samples. Compute-pass
mean/p99 is 2/2 for control and direct, and 1/1 for direct+deferred.
Instrumented total mean/p99 is 26.956/69.572, 18.937/75.121, and
19.432/82.034 ms. These perturbed diagnostic timings are not acceptance
measurements or exclusive GPU busy time. Each diagnostic buffer is 2,048 bytes.

Both direct release variants pass 40/40 native-4K reference-image comparisons.
Validation passes 28 Vello unit tests, two shader tests, 17 damage tests,
sparse regressions, six benchmark helpers, and strict default/optional
profiler/debug clippy. Image/stage tests cover empty scenes, 8,193 visible
non-overlapping rectangles crossing the 32,768-lane stride, clipped motion,
sparse/dense rendering, CPU/GPU path tags, runtime disable, and full-CPU
exclusion. A 512-full-viewport-rectangle test really overflows the initial tile
allocation (`failed=0x2`), requires at least two full attempts, and matches
the unbatched indirect/early-copy oracle with direct enabled and deferral
both off and on. The known unrelated presence-suite failure remains; no
full-suite-green or runtime async/debug benchmark claim is made.

Control and direct million-event native-4K frozen/idle runs both pass at 2 us
p99 with zero redraws, allocations, reallocations, or warm-to-final GPU/RSS
growth. GPU bytes are 114,170,264 / 114,170,248; RSS is
472,196 / 472,288 KiB. Both retain 241,580 tag bytes, 13,608 metadata bytes,
and 9,072 prefix bytes on the host. This is one million idle events, not
one million redraws; there is no measured idle speedup.

Reproduce the selected measured profile after building the current sources:

```sh
cargo build -p ui --release --example ui-perf
env VELLO_SPARSE_FINE=1 VELLO_SPARSE_COARSE=0 \
  VELLO_SPARSE_UPLOAD_REUSE=1 VELLO_COMPUTE_BATCH=2 \
  QUICK_UI_DAMAGE_GEOMETRIC=1 VELLO_CPU_PATHTAG=1 \
  VELLO_CPU_PATHTAG_REUSE=1 VELLO_CPU_METADATA=1 \
  VELLO_CPU_DRAW_PREFIX=1 VELLO_DIRECT_PATH_DISPATCH=1 \
  VELLO_DEFER_BUMP_READBACK=0 VELLO_GPU_TIMESTAMPS=0 \
  VELLO_GPU_DISPATCH_TIMESTAMPS=0 QUICK_UI_DASH_WIDTH=3840 \
  QUICK_UI_DASH_HEIGHT=2160 QUICK_UI_DASH_ITERS=1000 \
  QUICK_UI_DASH_FRAME_BUDGET_US=33333 \
  target/release/examples/ui-perf dashboard_perf_bench
```

Raw commands, exit codes, and complete output from all 19 runs are preserved
locally at `target/ui-perf-comparison/6724f6222e6795e29-benchmarks.txt`
(ignored build artifact). This ARM VM / Virtio-GPU offscreen result does not
qualify physical display scanout or a 2010 laptop.
