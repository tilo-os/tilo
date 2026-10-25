# UI and compositor performance controls

This is the source-checked reference for Tilo-owned rendering, redraw, cache,
copy, and performance-harness controls. It covers the retained UI, vendored
Vello, DRM bridge, and presentation loop, not unrelated application settings or
every graphics-driver environment variable. Defaults below mean **unset**.

The toolkit's knobs are `TILO_UI_*` (read through `ui::env`).

Use release binaries, record the exact commit, binary hash, adapter/driver,
dimensions, environment, and workload, and change one variable at a time.
Requested does not mean applied: unsupported paths fall back, and the harnesses
print selection/support counters. Shader selection is not evidence of fewer
copies, lower GPU memory, faster completion, or successful presentation.

The [benchmark guide](ui-performance-benchmark.md) contains commands. Historical
measurements and their limitations remain in Git history at `f780ae5c0`, under
`docs/benchmarks/2026-09-06-current-main/`. Those measurements predate the
latest-main integration; they are not fresh measurements of this merge. No flag
combination has established constant 60 fps on a 2010 laptop at 4K.

## Value parsing and lifetime

These controls do **not** share one Boolean parser:

| Notation below | Enabled values                               | How to disable            |
| -------------- | -------------------------------------------- | ------------------------- |
| Default-on     | Anything except the exact string `0`         | Set `0`                   |
| Exact-1        | Only the exact string `1`                    | Unset, or any other value |
| Present        | Variable exists, including empty or `0`      | Unset                     |
| Nonempty       | Any nonempty value, including `0` or `false` | Unset or empty            |

Where code uses `env::var`, a non-Unicode value is treated as unavailable;
`var_os` also accepts non-Unicode values. Use the documented ASCII values. Set
controls **before launching the process**. Renderer/device options are latched
at construction; application fast-path switches use `OnceLock`, and benchmarks
mostly read their configuration at startup. They are not live settings.
`TILO_UI_DASH_DAMAGE` is an exception: the harness checks it per redraw. Hidden
Rust setters used by differential tests can override renderer settings; they are
not additional environment flags.

## Default production optimizations

Leave these unset for a production-default measurement. Disabling one is an A/B
or troubleshooting control, not an optimization recommendation.

| Variable                     | Default / values   | Effect and limits                                                                                                                                                                                                          |
| ---------------------------- | ------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `TILO_UI_DAMAGE_GEOMETRIC`   | Default-on         | Reuses geometrically sized damage-target allocations in offscreen and window rendering. Disabling changes capacity policy, not native output resolution.                                                                   |
| `VELLO_COMPUTE_BATCH`        | `2`; `0`, `1`, `2` | `0`: separate dispatches; `1`: conservative compute batching; `2`: also batches across deferred frees. Unset/invalid values select `2`. CPU dispatches break GPU batches; profiler builds preserve their query boundaries. |
| `VELLO_SPARSE_FINE`          | Default-on         | Shades damaged native tiles with Area antialiasing on the GPU path. Full damage, unsupported modes, or invalid retained state use full rendering. Does not itself enable sparse coarse dispatch.                           |
| `VELLO_CPU_PATHTAG`          | Default-on         | Runs the supported path-tag prefix work on the CPU, avoiding small GPU dispatches. This is selective CPU work, not a full software renderer.                                                                               |
| `VELLO_CPU_PATHTAG_REUSE`    | Default-on         | Reuses exact compatible path-tag prefix results on the GPU renderer. Requires the applicable CPU path-tag path; full CPU rendering does not use this optimization.                                                         |
| `VELLO_CPU_METADATA`         | Default-on         | Selective CPU metadata work for compatible uploads on the GPU renderer, with the normal path as fallback.                                                                                                                  |
| `VELLO_CPU_DRAW_PREFIX`      | Default-on         | Selective CPU draw-prefix construction on the GPU renderer; unsupported work falls back.                                                                                                                                   |
| `VELLO_SPARSE_UPLOAD_REUSE`  | Default-on         | Retains compatible packed sparse-scene uploads. Requires supported sparse fine rendering; does not promise zero upload for changing scenes.                                                                                |
| `VELLO_DIRECT_PATH_DISPATCH` | Default-on         | Bounded direct GPU path-count/tiling dispatch, avoiding setup dispatches. Not used for full CPU rendering.                                                                                                                 |
| `VELLO_ZERO_GRADIENT_REUSE`  | Default-on         | Reuses an engine-owned zero-gradient texture for scenes without gradient ramps.                                                                                                                                            |

Sources: [offscreen.rs](../crates/sdk/ui-core/src/offscreen.rs),
[render_thread.rs](../crates/sdk/ui-core/src/render_thread.rs),
[wgpu_engine.rs](../vendor/vello/src/wgpu_engine.rs),
[shaders.rs](../vendor/vello/src/shaders.rs),
[Vello renderer](../vendor/vello/src/lib.rs).

## Optional and experimental rendering paths

These are **off by default**. Do not enable them all as a preset: some are
mutually exclusive, some add work, and benefits depend on workload and adapter.
Keep the matching full-render pixel oracle and recovery tests in the validation
plan. Sparse shader experiments below require the supported GPU/Area path.

| Variable                          | Values                   | Effect / dependencies                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| --------------------------------- | ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `TILO_UI_VELLO_CPU`               | Present                  | Selects Vello's CPU coarse pipeline in UI renderer construction; this does not remove every GPU operation. Disables the GPU-only sparse/metadata experiments. **`=0` still selects CPU mode.**                                                                                                                                                                                                                                                                        |
| `VELLO_SPARSE_COARSE`             | Exact-1                  | Forces sparse coarse dispatch. `1` selects legacy-mask mode unless active bins are also `1`. **Any explicit non-`1` value forces dense coarse and overrides AUTO.**                                                                                                                                                                                                                                                                                                   |
| `VELLO_SPARSE_COARSE_ACTIVE_BINS` | Exact-1                  | With `VELLO_SPARSE_COARSE=1`, dispatches native active-bin lists. Alone it does not select a coarse policy.                                                                                                                                                                                                                                                                                                                                                           |
| `VELLO_SPARSE_COARSE_AUTO`        | Exact-1                  | When `VELLO_SPARSE_COARSE` is **unset**, chooses the adaptive coarse policy. The policy may choose dense work; it is not an unconditional sparse switch.                                                                                                                                                                                                                                                                                                              |
| `VELLO_SPARSE_TILE_BOUNDS`        | Exact-1                  | Narrows path-tile allocation when active-bin dispatch supplies valid, non-full native tile bounds. Requires an actually selected compatible active-bin path, not merely a requested sparse flag.                                                                                                                                                                                                                                                                      |
| `VELLO_SPARSE_FINE_QUAD_SKIP`     | Exact-1                  | Selects sparse fine workgroup/quad skipping where supported. Conflicts with solid fusion and segment-row-mask selection; also prevents the sparse PPT8 variant.                                                                                                                                                                                                                                                                                                       |
| `VELLO_SPARSE_OPAQUE_PRUNE`       | Exact-1                  | Selects the matching sparse-render coarse opaque-prune permutation, including its dense-coarse variant. Prevents solid fusion and segment-row-mask selection. Selection counters do not count commands actually pruned.                                                                                                                                                                                                                                               |
| `VELLO_SPARSE_SOLID_FUSE`         | Exact-1                  | Uses paired coarse/fine shaders for a private fused-solid command stream. Requires both matching shaders and neither quad-skip nor opaque-prune request. Prevents segment-row-mask and sparse PPT8 selection.                                                                                                                                                                                                                                                         |
| `VELLO_SPARSE_SEGMENT_ROW_MASK`   | Exact-1                  | Paired segment writer/reader optimization. Requires active coarse bins, valid narrowed tile bounds, matching tiling/fine shaders, and no quad-skip, opaque-prune, or solid-fuse request.                                                                                                                                                                                                                                                                              |
| `VELLO_CANONICAL_SEGMENT_ORDER`   | Exact-1                  | Canonicalizes segment order with an extra GPU stage for supported Area rendering, including full and sparse variants. This is an experimental determinism/correctness path, not a guaranteed speedup; it prevents PPT8 selection.                                                                                                                                                                                                                                     |
| `VELLO_AREA_PPT8`                 | Exact-1                  | Eight-pixels-per-thread Area fine-shader experiment. Full rendering excludes canonical order; sparse rendering additionally excludes selected quad-skip, solid-fuse, and segment-row-mask paths.                                                                                                                                                                                                                                                                      |
| `VELLO_CLIP_SINGLE_GROUP`         | Exact-1                  | GPU clip-leaf specialization only for 1–256 clips and exactly one clip-leaf workgroup `(1,1,1)`. Other clip shapes use the normal shader.                                                                                                                                                                                                                                                                                                                             |
| `VELLO_DEFER_BUMP_READBACK`       | Exact-1                  | Moves the existing bump-counter readback after fine rasterization in synchronous GPU recording. Not full CPU or asynchronous rendering. Does not remove overflow validation or the readback bytes.                                                                                                                                                                                                                                                                    |
| `TILO_DRM_XRGB_DAMAGE`            | Exact-1                  | Opt-in damage-aware writes into DRM XRGB destinations, also exercised by `redraw_bench`. Per-buffer revision history must be valid; unknown/stale/invalid history and recovery boundaries require full refresh. This is distinct from GPU texture readback damage.                                                                                                                                                                                                    |
| `TILO_KMS_ATOMIC`                 | Unset: on; exact `0` off | `0` page-flips every output through the legacy `drmModePageFlip` instead of an atomic commit of the primary plane, without asking the driver for atomic (docs/architecture/compositor.md, "Page flips"). Restores the flips Tilo made before atomic presentation: the A/B baseline, or the switch-off for a driver whose atomic flips misbehave. Legacy flips cannot change format, so fullscreen `XBGR8888`/`ABGR8888` clients are composited. Latched at first use. |
| `TILO_CLIENT_SCANOUT`             | Unset: on; exact `0` off | `0` composites every frame instead of page-flipping a qualifying fullscreen client's own buffer (docs/architecture/compositor.md, "Fullscreen client scanout"). The composited baseline for an A/B measurement, or an escape hatch for a driver that misbehaves with client framebuffers. Latched at first use.                                                                                                                                                       |

Sources: [shader selection](../vendor/vello/src/render.rs),
[DRM scanout](../crates/session/shell/src/compositor/backend/drm/scanout.rs),
[KMS flips](../crates/session/shell/src/compositor/backend/drm/kms_flip.rs),
[XRGB history](../crates/session/shell/src/compositor/backend/drm/xrgb_history.rs).
For the recorded `bounds-xrgb-ppt8` experiment, the additional controls were
`VELLO_SPARSE_COARSE_AUTO=1 VELLO_SPARSE_TILE_BOUNDS=1 TILO_DRM_XRGB_DAMAGE=1 VELLO_AREA_PPT8=1`;
these are not production defaults.

## Memory-policy and renderer diagnostics

| Variable                        | Default / values         | Meaning and measurement caveat                                                                                                                                                                                                                                                                                                                                                                                   |
| ------------------------------- | ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `VELLO_BUMP_BUDGET`             | Unset: adaptive          | `upstream` pins all policy reservations to upstream constants. `lines`, `binning`, `tiles`, `seg_counts`, `segments`, `blend`, or `ptcl` seeds only that field from upstream; these single-field modes remain adaptive. `ptcl=N` seeds PTCL words (u32, capped at the policy ceiling), not bytes. Unknown/invalid strings warn and retain adaptive initialization. Scene/viewport structural floors still apply. |
| `VELLO_BUMP_LOG`                | Nonempty, off            | Logs allocator usage/recovery; adds output overhead. `=0` still logs.                                                                                                                                                                                                                                                                                                                                            |
| `VELLO_COMPUTE_BATCH_LOG`       | Exact-1, off             | Logs compute batching information; separate from timing queries.                                                                                                                                                                                                                                                                                                                                                 |
| `VELLO_DIRECT_PATH_WORKGROUPS`  | `128`; integer `1..=128` | Initialization-only dispatch-grid diagnostic. Invalid, zero, out-of-range, or non-Unicode values use `128`. Only affects the direct path. Nondefault grids require shell benchmark profile mode.                                                                                                                                                                                                                 |
| `VELLO_GPU_TIMESTAMPS`          | Exact-1, off             | Requests GPU timestamp instrumentation and query/resolve/readback resources. Requires adapter/device timestamp support; unsupported is reported, not a zero-time sample.                                                                                                                                                                                                                                         |
| `VELLO_GPU_DISPATCH_TIMESTAMPS` | Exact-1, off             | Per-dispatch GPU timing; requires `VELLO_GPU_TIMESTAMPS=1`, inside-pass query support, GPU execution, and no `wgpu-profiler` feature. The harness rejects the missing-parent-flag combination.                                                                                                                                                                                                                   |

Sources: [bump_budget.rs](../vendor/vello/src/bump_budget.rs),
[gpu.rs](../crates/sdk/ui-core/src/gpu.rs),
[wgpu_engine.rs](../vendor/vello/src/wgpu_engine.rs). Tracked GPU bytes exclude
driver-private allocations. CPU XRGB bytes written, GPU readback payload,
GPU-to-GPU copies, and physical VRAM are separate metrics. The completion/map
phase includes rendering and synchronization, not only copy time. Readback
payload counters exclude row-padding holes.

## Interactive UI troubleshooting and profiling

| Variable                | Default / values     | Effect                                                                                                                                                                                    |
| ----------------------- | -------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `TILO_UI_NO_SCROLL_OPT` | Nonempty, off        | Disables the scroll-reposition fast path; forces full layout solving on layout-dirty frames. Cached at first use.                                                                         |
| `TILO_UI_PERF`          | Nonempty, off        | Enables application performance logging. Cached at first use.                                                                                                                             |
| `TILO_UI_PRESENT`       | Present, off         | Render-thread encode/acquire/present timing logs; useful for costs absent from offscreen benchmarks. Latched when the render thread starts.                                               |
| `TILO_PROFILE`          | Nonempty, off        | Enables the in-app profiling HUD, including in release builds. The HUD itself adds UI work.                                                                                               |
| `TILO_PROFILE_OUT`      | Nonempty path, unset | Enables per-frame profiling without requiring the HUD. Appends CSV for `.csv`, JSON-lines otherwise. Choose a dedicated output path.                                                      |
| `TILO_WEBVIEW_PROFILE`  | Unset/empty/`0`: off | Other nonempty strings enable periodic WebView timing summaries; exact `trace` adds initial frame/draw detail. Includes SHM swizzle and input-to-paint diagnostics; latched at first use. |

`TILO_UI_DEBUG` is a presence-enabled **debug-build-only** UI debug mode, not a
release optimization. `TILO_A11Y_REDUCE_MOTION` / `TILO_REDUCE_MOTION` are
accessibility inputs: trimmed nonempty values other than `0` or case-insensitive
`false` request reduced motion. Record the effective theme setting when
comparing animation workloads; do not claim a speedup by silently removing
animation. Likewise keep UI scale, text scale, theme, and overlays constant.
`TILO_SHOTS_THEME` selects the offscreen launch fixture (`light`,
`high-contrast`, otherwise dark); it is not a renderer optimization switch.

Sources: [app.rs](../crates/sdk/ui-core/src/app.rs),
[nav_split.rs](../crates/sdk/ui-core/src/widget/nav_split.rs),
[profiling.rs](../crates/sdk/ui-core/src/profiling.rs),
[WebView profiling](../crates/sdk/ui-core/src/webview/profile.rs).

## Startup warmup and DMA-BUF routing

These affect startup or the actual pixel-transfer route, not just shader
selection. Keep their values identical across steady-state comparisons.

| Variable                    | Default / values                                                     | Effect and limits                                                                                                                                                                                                                                                                                                                                                                                   |
| --------------------------- | -------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `TILO_STARTUP_WARM`         | On; trimmed, case-insensitive `off`, `0`, `false`, or `no` disables  | Master background warmup switch, cached at first use. Empty/other values enable it. The shell warms a throwaway font system to populate OS/font caches; this is not a persistent GPU pipeline cache.                                                                                                                                                                                                |
| `TILO_STARTUP_WARM_VELLO`   | Off; trimmed empty/`0`/`off`/`false`/`no` disables, case-insensitive | Other supplied values additionally request a 4×4 Vello scratch render in shell and shot-harness startup. Requires master warmup enabled. Can contend with real renderer initialization; do not assume it improves first paint.                                                                                                                                                                      |
| `TILO_STARTUP_METRICS_PATH` | Unset: disabled; path                                                | Enables startup event capture at initialization. Explicit dump or scoped shutdown writes/replaces JSON at that path; parent directories are not created and write errors are ignored. Verify the file exists before reporting a result.                                                                                                                                                             |
| `TILO_DMABUF_DEVICE`        | Unset: automatic probe; path                                         | Selects the render node advertised to compositor clients. Empty explicitly disables DMA-BUF advertisement; an unusable override is rejected. Automatic selection requires an openable, known CPU-mappable driver; an override bypasses that driver allow-list. The compositor's foreign-surface route CPU-maps the buffer, so DMA-BUF advertisement does **not** imply a GPU-only zero-copy import. |

Sources: [warmup.rs](../crates/sdk/ui-core/src/warmup.rs),
[shell startup](../crates/session/shell/src/main.rs),
[startup metrics](../crates/system/tilo-startup-metrics/src/lib.rs),
[compositor render node](../crates/session/shell/src/compositor/render_node.rs).

## UI dashboard benchmark

Build `cargo build -p ui-examples --release --example ui-perf --locked`, then
invoke `target/release/examples/ui-perf dashboard_perf_bench`. All controls here
are harness-only; none sets a production frame-rate limit.

| Variable                       | Default / values         | Workload or report                                                                                                                                                                                                                                                                |
| ------------------------------ | ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `TILO_UI_DASH_WIDTH`           | `1800`; positive u32     | Native output width in pixels. Invalid/zero falls back. Set `3840` for UHD 4K.                                                                                                                                                                                                    |
| `TILO_UI_DASH_HEIGHT`          | `1200`; positive u32     | Native output height in pixels. Invalid/zero falls back. Set `2160` for UHD 4K.                                                                                                                                                                                                   |
| `TILO_UI_DASH_ITERS`           | `500`; usize             | Measured event iterations. Invalid falls back; zero parses but is not a valid performance sample. Use `1000` for redraws or `1000000` for the idle-event test.                                                                                                                    |
| `TILO_UI_DASH_STATIC`          | Present, off             | Omits animated spinners/indeterminate bars. Alone it does not guarantee no redraw.                                                                                                                                                                                                |
| `TILO_UI_DASH_FROZEN`          | Present, off             | Identical stationary cursor events. Combine with STATIC for the no-redraw workload; do not combine with SCROLL.                                                                                                                                                                   |
| `TILO_UI_DASH_SCROLL`          | Present, off             | Wheel event workload; tests scroll/layout/cache behavior.                                                                                                                                                                                                                         |
| `TILO_UI_DASH_DAMAGE`          | Present, off             | Prints damage rectangles on each redraw; contaminates timing with console I/O.                                                                                                                                                                                                    |
| `TILO_UI_DASH_RENDER_PROFILE`  | Nonempty, off            | Host render-phase breakdown. Also automatically enabled by GPU timestamp mode.                                                                                                                                                                                                    |
| `TILO_UI_DASH_FRAME_BREAKDOWN` | Nonempty, off            | Prints detailed frame breakdown.                                                                                                                                                                                                                                                  |
| `TILO_UI_DASH_ALLOC_PROFILE`   | Nonempty, off            | Counts allocations in the measured loop after harness storage preparation.                                                                                                                                                                                                        |
| `TILO_UI_DASH_ALLOC_BREAKDOWN` | Nonempty, off            | Enables allocation counting plus phase attribution; also enabled automatically by either per-redraw allocation budget.                                                                                                                                                            |
| `TILO_UI_DASH_VERIFY`          | Exact-1, off             | Retained-versus-full native pixel preflight. Adds reference textures/readbacks, suppresses performance reporting, rejects all acceptance budgets and the four profile-report flags above. Cannot combine with GPU timestamp mode.                                                 |
| `TILO_UI_DASH_VERIFY_FRAMES`   | `40`; integer `20..=100` | Number of frames required for verification; invalid values fail with exit 2. Used in verification mode; supply enough iterations/redraws (normally the same number).                                                                                                              |
| `TILO_UI_DASH_SAMPLES`         | Unset, or nonempty path  | Writes every redrawing frame's total (microseconds, one per line) after the loop, the population the frame budgets rank, for `tools/perf/record.py emit`. Fails with exit 2 when no frame redrew or the file cannot be written. Rejected in verification and GPU timestamp modes. |

### Optional acceptance budgets

All default to **disabled**, not to the example command's limits. Failures exit
with status 2. GPU timestamp diagnostics reject all these variables even if
their value would otherwise disable a budget. Allocation budgets enable the
corresponding instrumentation automatically.

| Variable                                    | Unit / statistic                               | Parsing                                                                    |
| ------------------------------------------- | ---------------------------------------------- | -------------------------------------------------------------------------- |
| `TILO_UI_DASH_FRAME_BUDGET_US`              | Redrawing-frame p99, microseconds              | Positive u128; invalid/zero ignored                                        |
| `TILO_UI_DASH_MEAN_FRAME_BUDGET_US`         | Exact redrawing-frame mean, microseconds       | Positive u128; an explicitly supplied invalid/zero/non-Unicode value fails |
| `TILO_UI_DASH_EVENT_BUDGET_US`              | Event p99, microseconds                        | Positive u128; invalid/zero ignored                                        |
| `TILO_UI_DASH_RASTER_PIXEL_BUDGET`          | Mean native pixels rasterized per redraw       | Positive u128; invalid/zero ignored; fails with no redraws                 |
| `TILO_UI_DASH_ALLOC_BUDGET_PER_1000`        | Allocator calls per 1,000 events               | u64; zero allowed, invalid ignored                                         |
| `TILO_UI_DASH_ALLOC_CALL_BUDGET_PER_REDRAW` | Allocator calls per redraw                     | u64; zero allowed, invalid ignored                                         |
| `TILO_UI_DASH_ALLOC_BYTE_BUDGET_PER_REDRAW` | Allocated bytes per redraw, not retained bytes | u64; zero allowed, invalid ignored                                         |
| `TILO_UI_DASH_RSS_BUDGET_KIB`               | Current process RSS after the loop, KiB        | Positive u64; invalid/zero ignored; not peak RSS                           |
| `TILO_UI_DASH_GPU_BUDGET_BYTES`             | Known retained GPU bytes                       | Positive u64; invalid/zero ignored; not total driver VRAM                  |
| `TILO_UI_DASH_RSS_GROWTH_BUDGET_KIB`        | Warm-to-final RSS increase, KiB                | u64; zero allowed, invalid ignored; negative growth treated as zero        |
| `TILO_UI_DASH_GPU_GROWTH_BUDGET_BYTES`      | Warm-to-final known GPU increase, bytes        | u64; zero allowed, invalid ignored; negative growth treated as zero        |

Always check that the expected gate actually printed a result: a misspelled
budget or an invalid value in an ignore-on-error parser is not a passing gate. A
16,667 µs p99 gate is a useful 60 Hz screen, not proof that every frame met its
deadline. The million-event frozen test measures **zero redraws**, not a million
rendered frames. Source:
[dashboard harness](../crates/dev/ui-examples/examples/dashboard_perf_bench.rs).

### Other UI performance scenarios

The same `ui-perf` executable also offers these separate workloads; their
results are not interchangeable with native-4K dashboard redraws:

| Scenario / control                           | Default / behavior                                                                                                                                                              |
| -------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `perf_scenario --count N` (also `--count=N`) | `120` usize iterations; missing/invalid count falls back. Runs CPU-only list-scroll and gallery-steady scenarios, without a GPU or window. Use a positive count.                |
| `perf_scenario --json`                       | Off; emits JSON reports instead of text.                                                                                                                                        |
| `widget_perf_bench` / `TILO_UI_PERF_ITERS`   | `20` usize iterations; invalid falls back. A separate 1100×800 click/layout/draw/offscreen-render workload; use a positive count.                                               |
| `ui-shots perf_hud_shot` / `PERF_HUD_FRAMES` | `90` usize frames; invalid falls back. Feeds real CPU pass timings into a HUD screenshot at 1280×800, not an acceptance run.                                                    |
| `PERF_HUD_OUT`                               | `/tmp/perf_hud/hud.png`; PNG output path for the HUD fixture, which creates parent directories and writes the image. Use a dedicated path to avoid replacing an existing image. |

Sources:
[perf_scenario.rs](../crates/dev/ui-examples/examples/perf_scenario.rs),
[widget_perf_bench.rs](../crates/dev/ui-examples/examples/widget_perf_bench.rs),
[perf_hud_shot.rs](../crates/dev/ui-examples/examples/perf_hud_shot.rs).

## Shell redraw and cold-scene harnesses

Build `cargo build -p shell --release --example redraw_bench --locked` and run
`target/release/examples/redraw_bench`. The workload is fixed at 1,000 actions,
native 3840×2160, with GPU completion, readback, and CPU XRGB conversion. There
is no environment override for the action count, resolution, or gate limits.
Current limits are **mean ≤25,000 µs and p99 ≤50,000 µs**; they are not a 60 fps
claim. Its CPU destination is reused memory, not a physical KMS mapping.

| Variable                           | Default / values        | Effect                                                                                                                                                                                                                                        |
| ---------------------------------- | ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `TILO_REDRAW_BENCH_PROFILE`        | Exact-1, off            | Phase timing and joined action diagnostics. Acceptance is disabled and the run deliberately returns nonzero even when rendering succeeds.                                                                                                     |
| `TILO_REDRAW_BENCH_PROFILE_OUTPUT` | Unset, or nonempty path | Profile-only all-action report, written after the loop to a newly created file; existing paths are not overwritten. Empty paths fail in profile mode. Ignored outside profile mode.                                                           |
| `TILO_REDRAW_BENCH_SAMPLES`        | Unset, or nonempty path | Writes the 1,000 action samples (microseconds, one per line, action order) before the gate decides, so a run that misses the gate is still recorded. Only a complete, valid sample set is written. Empty paths fail; rejected with PROFILE=1. |
| `TILO_REDRAW_BENCH_SCENE_REUSE`    | Exact-1, off            | Requires PROFILE=1. Compares/retains resolved upload bytes for **telemetry**, not production scene-reuse enablement.                                                                                                                          |
| `TILO_REDRAW_BENCH_GEOMETRY_REUSE` | Exact-1, off            | Requires PROFILE=1. Measures potential geometry reuse for **telemetry**, not an implemented production geometry cache.                                                                                                                        |
| `SHELL_DRAW_ITERS`                 | `500`; usize            | Only the separate `draw_bench` example: repeated CPU scene paint at 1600×900, no GPU/present. Invalid falls back; use a positive count.                                                                                                       |

Either GPU timestamp flag, scene/geometry reuse telemetry, or a nondefault
direct-path workgroup grid requires shell profile mode. Never count a diagnostic
nonzero exit as an ordinary acceptance failure or pass without reading its
explicit mode and result.

`retained_scene_probe` accepts exactly one positional mode: `legacy-dense`,
`legacy-versioned`, `segmented-dense`, or `segmented-versioned`. It renders one
**cold** native-4K frame with phase timing; it is not an acceptance benchmark.
Sources: [redraw_bench.rs](../crates/session/shell/examples/redraw_bench.rs),
[retained_scene_probe.rs](../crates/session/shell/examples/retained_scene_probe.rs).

## Presentation-loop controls

These measure/drive the actual shell loop. Keep physical-display measurements
separate from headless and nested-window results.

| Variable                            | Default / values                           | Effect                                                                                                                                                                                                                                                                                                                                                  |
| ----------------------------------- | ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `TILO_FRAME_TRACE`                  | Unset; path                                | Enables instrumentation and creates/truncates a per-tick CSV file.                                                                                                                                                                                                                                                                                      |
| `TILO_FRAME_SUMMARY`                | Unset; path                                | Enables instrumentation and writes a JSON summary at shutdown. Either path alone enables recording; use valid dedicated paths, not empty strings.                                                                                                                                                                                                       |
| `TILO_FRAME_WARMUP_MS`              | `1500`; trimmed u64                        | Excludes early samples from summary; zero allowed, invalid falls back. Only useful with instrumentation enabled.                                                                                                                                                                                                                                        |
| `TILO_PAINT_STATS`                  | Present (Unicode), off                     | Logs painted/idle tick counts, selected redraw causes, cadence, and presentation/backpressure counters about every two seconds. `=0` still logs. The cause counters are not an exhaustive partition of all redraw reasons.                                                                                                                              |
| `TILO_LOOP_TIMING`                  | Present (Unicode), off                     | Logs a phase breakdown for native-loop ticks with more than 100 ms of non-sleeping work. `=0` still enables it; no log does not establish a 16.67 ms deadline. On a running desktop, a [profile](#profiling-the-running-desktop) records every phase without a restart.                                                                                 |
| `TILO_RUN_SECONDS`                  | Unset: run indefinitely                    | Positive finite fractional seconds requests clean shutdown; malformed/nonpositive values mean no timeout.                                                                                                                                                                                                                                               |
| `TILO_REFRESH_HZ`                   | Backend refresh, or 60 Hz fallback         | Diagnostic pacing override, finite 20–1000 Hz including fractional rates (rounded to millihertz). Invalid values warn and are ignored. Does not reconfigure a physical monitor.                                                                                                                                                                         |
| `TILO_TEST_FRAME_STALL_MS`          | Unset; `80` or `50-130` (ms, at most 2000) | Developer override (debug or `dev-overrides` builds only; release builds ignore it): blocks the loop after every painted frame, a fresh delay from the range each frame, so layout springs, animations and configures run at a slow machine's frame times. Invalid values warn and are ignored. `os/dev/test-slow-frame-launch.sh` opens apps under it. |
| `TILO_TEST_PRESENT_DEVICE_DELAY_MS` | Unset; milliseconds                        | Developer override (debug or `dev-overrides` builds only; release builds ignore it): the renderer's device is published for client imports, and its GPU probe and dmabuf feedback sent, only that long after it opened, so a headless session meets its first clients without a presenting GPU, as a laptop does at session start.                      |
| `TILO_HEADLESS_SIZE`                | `1600x900`                                 | Initial headless output `WxH`, positive u32 dimensions; malformed/zero falls back.                                                                                                                                                                                                                                                                      |
| `TILO_HEADLESS_WATCH`               | Unset: unwatched                           | `1` rasterizes every headless frame from the start, as a streaming viewer needs; otherwise frames are rasterized only after the control socket's `watch` (snapshots render on demand regardless).                                                                                                                                                       |
| `TILO_WINIT_SIZE`                   | `1600x900`                                 | Initial nested window `WxH`, positive u32 dimensions; malformed/zero falls back.                                                                                                                                                                                                                                                                        |

The [frame-loop driver](../scripts/perf/frame-loop-bench.py) exposes:

| CLI option                         | Default / purpose                                                                                |
| ---------------------------------- | ------------------------------------------------------------------------------------------------ |
| `--seconds`                        | `8.0`, run length                                                                                |
| `--backend`                        | `headless`; also `winit` or `drm` (requires a free seat/TTY)                                     |
| `--load`                           | `none`; also continuously repainting `windowed` or `fullscreen` client                           |
| `--input-hz`                       | `0.0`; injected click rate                                                                       |
| `--width`, `--height`              | `1600`, `900`; request native dimensions                                                         |
| `--warmup-ms`                      | `1500`; summary warm-up exclusion                                                                |
| `--refresh-hz`                     | Unset; passes the pacing override above                                                          |
| `--target-us`                      | `0` means derive from refresh override, otherwise 16,667 µs; scoring target, not rendering speed |
| `--label`, `--out`                 | Optional run label; output directory defaults to `target/frame-bench`                            |
| `--shell-bin`, `--startup-timeout` | Optional existing binary; startup timeout `30.0` seconds                                         |
| `--nested`                         | Off; wraps winit in a headless `cage`                                                            |
| `--compare BEFORE AFTER`           | Compare existing `.stats.json` files                                                             |
| `--report STATS_JSON`              | Report existing results                                                                          |
| `--resummarize TRACE_CSV`          | Recompute **and rewrite** stats from an existing trace using selected warm-up/target             |

The driver uses `TILO_TEST_CONTROL` for its local input-control socket. That is
test infrastructure, not an optimization flag. `present_async` in traces
distinguishes input-to-queued from synchronous input-to-present; neither an
offscreen GPU fence nor a queue submission proves a physical page flip.

### Profiling the running desktop

The image sets `kernel.yama.ptrace_scope = 3` and
`kernel.perf_event_paranoid = 3` (`os/usr/lib/sysctl.d/60-tilo-hardening.conf`),
so neither `perf` nor a debugger can attach to the compositor, and that stays
so. Instead the device admin asks the compositor to profile itself, without a
restart or an environment variable, through the owner-only `session-diagnostics`
actions on the admin control socket:

```bash
sock="$XDG_RUNTIME_DIR/tilo-session/admin-control.sock"
tilo-control --socket "$sock" actions invoke session-diagnostics profile-start@session-diagnostics --args-json '{"seconds":30}'
# ... reproduce the problem ...
tilo-control --socket "$sock" actions invoke session-diagnostics profile-stop@session-diagnostics
tilo-control --socket "$sock" actions invoke session-diagnostics inspect@session-diagnostics   # "profile": state and last path
```

`seconds` defaults to 30 and may be 1 to 600; the profile ends by itself then,
or earlier at `profile-stop`. Only one records at a time. Like the other session
diagnostics, these actions answer only on the device admin's own unlocked
session socket: the development test-control socket, the agent socket, a
sandboxed or launched app, another account and an agent acting over the admin
socket are refused with `not_permitted`.

Each profile is a private (0700/0600) directory
`$XDG_STATE_HOME/tilo/profiles/compositor-<unix-seconds>/`; the newest eight are
kept. It reuses the loop's own instruments:

| File           | Contents                                                                                                                                                                                                                                                                                      |
| -------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `frames.csv`   | One row per loop tick, the `TILO_FRAME_TRACE` columns (`--resummarize` above reads it).                                                                                                                                                                                                       |
| `phases.csv`   | `seq,phase,elapsed_us,items`: every pump phase and checkpoint `TILO_LOOP_TIMING` times, the widget pump's parts (`widget-pump/*`), the overlay host's update steps and the retained scene build's (`scene/*`), joined to `frames.csv` on `seq`. `TILO_LOOP_TIMING` alone logs only slow ones. |
| `work.csv` | `seq,counter,items`: integer work counts joined to the same tick. Counter names contain no application or document identities. |
| `threads.csv`  | `t_us,tid,thread,cpu_us`: CPU each thread used since the previous sample, read from `/proc/self/task` every 50 ms by the profile's own `compositor-profile` thread (which appears in it). Presenter, GPU and worker threads show here, not only the loop.                                     |
| `summary.json` | Frame summary (as `TILO_FRAME_SUMMARY`), per-phase count/total/max, per-thread CPU totals, and work-counter samples/total/max (`work_schema: 1`). Includes target OS/architecture and assertion mode. Written last, only after all trace writers close successfully; missing means incomplete.                                                                                                                                              |

While no profile records, each recording site pays only an atomic load. Rows are
handed to writer threads once a second, so a profile never puts the loop in the
disk's queue. Nothing records application content: the trace names which app ids
committed frames, which is why the files are private. Completion establishes
that the recorded traces were written, not that every thread CPU clock was
observable: `/proc` sampling remains best-effort. Missing observations must not
be interpreted as zero CPU usage.

### Comparing work across computing environments

Treat elapsed time, amount of work, and workload identity as separate evidence.
A faster machine can hide redundant work; a lower operation count does not by
itself prove lower latency. Neither CPU cycles nor retired instructions are
fully portable: compiler, architecture, inputs and cache state affect them.

For reproducible comparisons:

1. Fix the workload: the same fixture, operation sequence and operation count,
   locale, theme, viewport, scale, source sizes and result limits. Warm up
   outside the recording, and measure cold start separately. A fresh process
   does not imply cold filesystem or shader caches; record their actual state.
   Use semantic
   actions where possible: pointer-event coalescing can differ across hosts.
2. Start the existing owner profile, replay the fixed operations, then stop it.
   Do not compare "as many operations as possible in 30 seconds" as equal work.
   Keep the binary hash, compiler, features and build flags with the results;
   assertion mode alone does not identify an optimisation profile.
3. Compare `work` totals and counts per replayed operation first. A recorded
   zero is observed zero work; a missing counter is unknown, not zero. Compare
   only compatible counter schemas and definitions. Counts include only the
   instrumented work, not an estimate of all CPU instructions.
4. For timing, alternate before/after runs on the **same** host, keep cold and
   warm samples separate, report median and tails, and retain the raw traces.
   CPU time helps distinguish computation from scheduling or I/O waits, but
   is not a hardware-independent score. Validate representative changes in
   both debug and release before making claims about their relative speed.
5. Use the owner profile's phase and thread CPU breakdowns to narrow the
   expensive area; it is not a stack sampler or a GPU timestamp capture. Do
   not weaken Tilo's ptrace or perf restrictions. On a host that permits it,
   launch a controlled CPU benchmark under a stack sampler to locate hot
   functions, or use Callgrind/Cachegrind instruction and simulated cache counts
   with a fixed binary and simulator configuration. Those counts describe
   that model, not actual GPU performance or panel latency.

Current work counters:

| Counter | Meaning |
| --- | --- |
| `layout/tree-builds` | Layout-tree rebuild attempts. |
| `layout/solves` | Layout solve attempts. |
| `search/provider-pages` | Provider page queries, including empty or stale replies. |
| `search/provider-rows` | Rows returned by those queries, before registry filtering. |
| `host-apps/dedup-inputs` | Host entries passed to alias deduplication. |
| `host-apps/dedup-candidates` | Previously retained apps examined for a duplicate, after name grouping. |

For example, 512 host entries with distinct names previously required 130,816
candidate comparisons. The grouped algorithm examines zero duplicate candidates
and retains all 512 entries; it still performs name hashing and output
construction. This is a portable algorithmic regression check, not a claim of
zero runtime or a measured speedup. Many distinct launch modes sharing one name
can still require multiple comparisons within that name's group.

### Projector capture workload

The capture profile is not a flag. The projector service sends it to the shell
as a `CaptureProfile` event on its control socket whenever it changes
(`tilo_projector::control`), and the capture tap drains that channel before each
frame; nothing is read from disk on the render thread. Its u32 fields are
`max_width`, `max_height`, `fps`, and `bitrate_kbps`. The initial
unknown-receiver ceiling is 1920×1080 at 60 fps and 10,368 kbps. Encoder
pressure can reduce the effective dimensions/bitrate below the ceiling. This
changes the streamed workload, not native display resolution; scaled projector
results are not native-4K redraw evidence.

`TILO_PROJECTOR_VIDEO_SOCKET` selects the capture worker's Unix socket,
defaulting to `video.sock` in the projector runtime directory
(`TILO_PROJECTOR_RUNTIME_DIR`, else `XDG_RUNTIME_DIR/tilo-projector`). It routes
output, not rendering speed. Sources:
[capture](../crates/session/shell/src/compositor/projection_capture.rs),
[profile schema and defaults](../crates/system/projector/src/lib.rs),
[native-loop diagnostics](../crates/session/shell/src/compositor/native.rs).

## Build and dependency controls

The standard release profile is `opt-level=3`, fat LTO, one codegen unit,
`panic=abort`, stripped symbols, line-table debug information,
`overflow-checks=false`, and `incremental=false`. `thin-app` changes
linking/unwinding; debug, demo, and test builds are not interchangeable with
release samples. Record any `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`,
`CARGO_PROFILE_*`, compiler, linker, or feature overrides. `RUSTFLAGS` replaces
target config flags; casually setting it can also drop hardening options.

**Hardware compatibility:** `.cargo/config.toml` sets the x86 CPU floor to
`x86-64-v3` (Haswell-era/2013+), while aarch64 uses `generic`. Most 2010 laptops
cannot execute the x86 release build as configured. A compatible CPU baseline,
working graphics backend, and physical 4K60 output need separate validation;
software-renderer measurements in this VM establish none of these.

`CARGO_BUILD_JOBS`, `RUSTC_WRAPPER`, `CARGO_TARGET_DIR`, and `CARGO_INCREMENTAL`
control build scheduling/caching. The optional Cargo shim also uses
`SCCACHE_CACHE_SIZE`, `SCCACHE_DIR`, and `SCCACHE_SERVER_PORT`; see
[build troubleshooting](development/troubleshooting.md#builds-repeat-work-or-storage-is-exhausted).
They are not runtime UI switches. Use a separate target directory per worktree.
Do not call a cache-hit build-time reduction a frame-time improvement.

The optional [PGO/BOLT scripts](../scripts/perf/README.md) are separate build
experiments, not part of the default release recipe or the preserved A/B runs.
`-C profile-generate=DIR`, `LLVM_PROFILE_FILE`, and `-C profile-use=FILE`
control Rust instrumentation, training output, and profile-guided codegen. The
scripts use fixed `target/pgo-*` directories and the named Distrobox
environment; merging profiles writes `os/pgo/tilo.profdata` and does not itself
commit it. The BOLT helper additionally requires per-binary `.fdata` and uses
`-reorder-blocks=ext-tsp`, `-reorder-functions=hfsort+`, `-split-functions`,
`-split-all-cold`, `-icf=1`, and `-use-gnu-stack`. Rust `.profraw` collection
does not automatically produce BOLT `.fdata`. These helpers were not validated
in this integration; record training data, tool versions, hardening flags, and
both final binaries before attributing any runtime gain to them.

Tilo passes wgpu environment configuration through at device initialization.
`WGPU_BACKEND` selects a comma-separated list of compiled/platform-supported
backends (`vulkan`/`vk`, `metal`/`mtl`, `dx12`/`d3d12`, `opengl`/`gles`/`gl`, or
`webgpu`; case-insensitive). Unset uses wgpu's default backend set. An
empty/invalid list can leave no usable backend. `WGPU_ADAPTER_NAME` selects a
case-insensitive substring match (a supplied name with no match can panic).
`WGPU_POWER_PREF` accepts `low`, `high`, or `none` when choosing a default
adapter. Keep them identical for A/B runs and record the **actual** adapter.
wgpu's `WGPU_DEBUG`, `WGPU_VALIDATION`, `WGPU_GPU_BASED_VALIDATION`,
`WGPU_VALIDATION_INDIRECT_CALL`, `WGPU_DISCARD_HAL_LABELS`, and
`WGPU_ALLOW_UNDERLYING_NONCOMPLIANT_ADAPTER` are dependency diagnostics, not
Tilo optimization presets. In the locked wgpu 29 implementation, exact `0`
clears a supplied Boolean instance flag and any other supplied string sets it.
Debug builds default to DEBUG, VALIDATION, and VALIDATION_INDIRECT_CALL; release
defaults to VALIDATION_INDIRECT_CALL. The other listed flags default off. Do not
disable correctness checks to manufacture benchmark gains. Backend-specific
controls are defined by the wgpu version in [Cargo.lock](../Cargo.lock), not by
Tilo's Boolean parsers. The `vello/wgpu-profiler` Cargo feature is build-time
profiling and excludes the custom per-dispatch timestamp path described above.

Related graphics/media Cargo features (defaults are for each crate itself; an
application can enable them through dependency feature unification):

| Feature                     | Default                    | Performance relevance / requirement                                                                                                                                                                             |
| --------------------------- | -------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ui/dmabuf-import`          | Off                        | Vulkan external-memory import of a compatible DMA-BUF; avoids a CPU pixel round-trip. Requires supported formats/modifiers/device sharing; enabling the feature does not guarantee successful zero-copy import. |
| `ui/gpu-video`              | Off                        | Enables DMA-BUF import plus NV12/P010-to-RGB GPU conversion. Conversion is still GPU work even when no CPU pixel copy occurs.                                                                                   |
| `media/realtime`            | Off (the shell enables it) | GStreamer real-time encoder/decoder: hardware first (VA-API, NVCODEC, V4L2), then royalty-free software, then Cisco openh264. Which hardware runs is decided at runtime from the registry.                      |
| `shell/hosted-test-control` | On                         | Deterministic test seams for `shots` and integration tests; remove from production feature selection when that test surface is unwanted. Not a frame-rate optimization.                                         |

See [UI features](../crates/sdk/ui-core/Cargo.toml) and
[shell features](../crates/session/shell/Cargo.toml). `--no-default-features`
changes the workload/capability set; document it in any benchmark recipe rather
than silently comparing different feature sets.

## Optimizations without environment flags

Dirty/animation gating, retained widget/resource caches, allocation-free child
traversal, retained scene sections, bounded queues/image caches/undo history,
shared image storage, GPU buffer/bind-group reuse, and DRM recovery checks are
code paths or fixed bounds, not hidden tunables. See the
[rendering internals](ui-rendering-internals.md). Do not invent flags for cache
sizes, disable recovery, or describe diagnostic geometry-reuse opportunities as
already implemented production reuse.
