# Native-4K comparison: main snapshot 31e5afdde

All 21 predeclared runs completed with valid data on 2026-09-06. The tested branch is substantially faster in this VM, but median peak process memory is slightly higher. Constant 60 FPS and the full tail-latency goal remain unmet.

## Exact revision scope

Main was fetched and pinned at `31e5afdde9e86e004b71635bf34d847520d4a5ba` before adapters/builds. The final remote check found and fetched a newer tip, `84329100b1074d8b2adfc9312ceb9e1a3fc40350`. Eleven commits landed during this task, changing 53 files including UI block-editor/link-popover and multiple applications. **These results do not benchmark that newer tip.** No performance result has been relabeled, extrapolated or rerun silently.

The comparison branch is the hash-preserved clip-single-group v6 tested snapshot, not a new build of later uncommitted geometry diagnostics. Original branch `codex/clippy-runtime-ui-recovery` remains uncommitted and unpushed. The isolated benchmark checkout stays detached at the tested main SHA; no production optimization was ported into it.

## Small performance matrix

Three-run medians; time reduction is `1 − branch/main`. Frame rows use mean time per action/event. Idle uses complete process wall time, not frame time or isolated event-dispatch time.

| Workload/configuration | Tested main | Tested branch | Speedup | Less time | Peak process RSS change |
|---|---:|---:|---:|---:|---:|
| Branch: shell default | 333.245 ms | 40.253 ms | 8.28× | 87.9% | +1.7% |
| Branch: shell Bounds+XRGB+PPT8 (experimental) | 333.245 ms | 21.196 ms | 15.72× | 93.6% | +7.3% |
| Branch: animated UI default | 66.963 ms | 8.598 ms | 7.79× | 87.2% | +3.1% |
| Branch: 1M idle events (whole process) | 9.44 s | 3.49 s | 2.70× | 63.0% | +2.3% |

RSS increases are not GPU/VRAM measurements. Idle process time includes setup, two initial draws, loop execution, reporting and shutdown; different harness storage affects both wall time and RSS. Both revisions skipped all 1,000,000 idle events without redraw in every run.

## Frame latency and memory

All targets are native **3840×2160** (8,294,400 pixels), with 33,177,600 bytes for one 4-byte-per-pixel image—not a smaller target upscaled to 4K. Each frame workload contains 1,000 timed actions/events per run.

| Revision/configuration | Mean ms | p99 ms | Worst observed ms | Median peak RSS MiB | Numeric gate passes |
|---|---:|---:|---:|---:|---:|
| Main: shell | 333.245 | 451.836 | 575.231 | 523.07 | 0/3 |
| Branch: shell default | 40.253 | 93.309 | 505.147 | 531.98 | 0/3 |
| Branch: shell Bounds+XRGB+PPT8 (experimental) | 21.196 | 66.479 | 393.366 | 561.32 | 0/3 |
| Main: animated UI | 66.963 | 151.580 | 250.874 | 357.97 | 0/3 |
| Branch: animated UI default | 8.598 | 25.642 | 127.793 | 369.12 | 2/3 |

Each mean, p99 and peak-RSS entry is the median of three independently completed runs; worst is the largest observed maximum across the three. Percentiles are not pooled. Shell numeric gate: mean ≤25 ms and p99 ≤50 ms. Animated UI gate: p99 ≤30 ms. Main has no equivalent executable pass/fail gate, so its gate status is computed from recorded times. The branch UI third-run p99 was 43.230 ms: the two passing runs do not establish consistency. None of these outcomes proves all frames stay under the 16.67 ms budget required for constant 60 FPS.

## Copies and renderer validity

- Animated UI measurements on both revisions submit native-4K work, wait for GPU completion and perform **zero CPU target readback**. Main's benchmark-only no-readback adapter calls the existing renderer and settlement path; all three measured UI runs reported zero overflowed submissions.
- Shell measurements include each revision's production GPU texture readback and CPU XRGB output into a reused destination. No physical DRM/KMS page flip or scanout is timed.
- The six branch shell runs each completed exactly 1,000 render attempts (999 sparse, 1 full), with zero recovery attempts.
- Measured branch default CPU XRGB destination writes: 33,177,600,000 bytes per 1,000 actions. Experimental Bounds+XRGB+PPT8 median: 808,636,416 bytes, **97.6% fewer CPU destination bytes written**. This does not establish the same reduction in GPU copies, memory bandwidth or VRAM.
- Main's production shell bridge does not immediately check/retry overflow after each frame. Its nonuniform-pixel checks and final checksum detect blank output, not pixel equivalence. Shell ratios are timing comparisons, not proof that every frame has equivalent complete geometry.

## Method and limits

The host is the existing aarch64 VM using llvmpipe (LLVM22.1.8,128-bit) Vulkan **CPU/software rendering**. It is not a 2010 laptop, hardware-GPU measurement, or a complete OS presentation benchmark. Native target extent is verified, but no image asset is being claimed as a photographic 4K test input.

The frozen plan used three attempts per workload/configuration, rotated shell order and alternating UI/idle order. No builds, tests or benchmark runs overlapped one another. Runtime profiling, driver-thread overrides and cache-policy experiments were off. Existing GPU shader caches were retained; cold/warm state was not assumed. Every attempt, including numerical budget failures, is retained.

Revisions include independent shell/UI/dependency changes. Normal repository release optimization settings were preserved, but branch executables identify mold2.40.4 while main used its original build recipe with a transparent compiler-cache bypass. This is not a same-linker or single-patch causal experiment. See [adapter review](adapter-review.md).

## Full attempt record

All rows below are valid completions. Shell branch exit1 and UI branch exit2 indicate numerical budget failures, not missing samples or truncated output. Raw log sidecars `.started.json` and `.result.json` preserve commands, environment, exit status, timestamps, byte counts and SHA256.

| Raw log | Samples/events | Mean µs | p99 µs | Peak RSS KiB | Process wall s | Exit |
|---|---:|---:|---:|---:|---:|---:|
| [main-default-shell-round-1](main-default-shell-round-1.log) | 1000 | 335022.140 | 476438 | 582396 | 359.17 | 0 |
| [branch-default-shell-round-1](branch-default-shell-round-1.log) | 1000 | 44583.797 | 93309 | 543056 | 46.04 | 1 |
| [branch-bounds-xrgb-ppt8-shell-round-1](branch-bounds-xrgb-ppt8-shell-round-1.log) | 1000 | 22860.500 | 75631 | 574892 | 24.14 | 1 |
| [main-default-ui-round-1](main-default-ui-round-1.log) | 1000 | 67750.816 | 159234 | 366768 | 68.34 | 0 |
| [branch-default-ui-round-1](branch-default-ui-round-1.log) | 1000 | 8691.000 | 25193 | 377976 | 9.11 | 0 |
| [main-default-ui-frozen-round-1](main-default-ui-frozen-round-1.log) | 1000000 | 7.569 | 22 | 476820 | 9.44 | 0 |
| [branch-default-ui-frozen-round-1](branch-default-ui-frozen-round-1.log) | 1000000 | 1.000† | 2 | 488276 | 3.52 | 0 |
| [branch-default-shell-round-2](branch-default-shell-round-2.log) | 1000 | 39936.548 | 86234 | 544748 | 41.04 | 1 |
| [branch-bounds-xrgb-ppt8-shell-round-2](branch-bounds-xrgb-ppt8-shell-round-2.log) | 1000 | 21196.391 | 66479 | 574344 | 22.24 | 1 |
| [main-default-shell-round-2](main-default-shell-round-2.log) | 1000 | 333244.645 | 451836 | 535268 | 334.53 | 0 |
| [branch-default-ui-round-2](branch-default-ui-round-2.log) | 1000 | 8598.000 | 25642 | 378200 | 9.20 | 0 |
| [main-default-ui-round-2](main-default-ui-round-2.log) | 1000 | 66962.961 | 151580 | 366460 | 67.70 | 0 |
| [branch-default-ui-frozen-round-2](branch-default-ui-frozen-round-2.log) | 1000000 | 1.000† | 3 | 486796 | 3.49 | 0 |
| [main-default-ui-frozen-round-2](main-default-ui-frozen-round-2.log) | 1000000 | 7.766 | 23 | 476588 | 9.52 | 0 |
| [branch-bounds-xrgb-ppt8-shell-round-3](branch-bounds-xrgb-ppt8-shell-round-3.log) | 1000 | 21170.063 | 55394 | 574792 | 22.14 | 1 |
| [main-default-shell-round-3](main-default-shell-round-3.log) | 1000 | 321356.340 | 426890 | 535628 | 322.76 | 0 |
| [branch-default-shell-round-3](branch-default-shell-round-3.log) | 1000 | 40253.018 | 99473 | 547452 | 41.70 | 1 |
| [main-default-ui-round-3](main-default-ui-round-3.log) | 1000 | 61729.921 | 133506 | 366564 | 62.36 | 0 |
| [branch-default-ui-round-3](branch-default-ui-round-3.log) | 1000 | 8522.000 | 43230 | 377492 | 8.97 | 2 |
| [main-default-ui-frozen-round-3](main-default-ui-frozen-round-3.log) | 1000000 | 7.074 | 22 | 476048 | 8.76 | 0 |
| [branch-default-ui-frozen-round-3](branch-default-ui-frozen-round-3.log) | 1000000 | 1.000† | 2 | 487504 | 3.20 | 0 |

† Branch means are reported as truncated integer microseconds. That resolution is material for idle events, so **no precise idle mean-time percentage is published**. The idle wall-time comparison above is separately measured. The branch idle worst-event time is unavailable in these logs, not zero.

## Storage cleanup and integrity

At the user's authorization, the idle sccache server was stopped and only `/home/user/.cache/sccache` was permanently removed (14 GiB reported by du). The cache is regenerable. Guest available space increased from143 GiB to156 GiB immediately after cleanup and was155 GiB after rebuilding. Source, GPU shader caches, failed build targets and all benchmark evidence were preserved.

A 1 MiB flushed write matched a direct-I/O SHA256 reread. A fresh cache-bypassed build hit its declared15-minute cap; its separately recorded continuation succeeded in12m43s. All942 new rlib headers were valid. Both main binaries matched direct-I/O rereads after flushing. The observed Btrfs write-error counter stayed at20,769 throughout the successful build and all runs; the cache cleanup is not proof of the original host/backing-storage cause.

The final fetch also reported an existing empty loose Git object, `10f6023ceb985a5b2fa9c87d6beec41a00857a30` (0 bytes, mtime18:40:23+02:00, before cleanup). It was not deleted or repaired. A connectivity-only check of HEAD and the newly fetched origin/main passed; that is not a full integrity check of all repository objects/refs.

After measurement, all four binary hashes, 14 frozen main input hashes and 38 original working-tree input hashes still matched. No Cargo/rustc/benchmark processes remained. No production or benchmark-harness source edits, commits or pushes were made by this benchmarking continuation; only benchmark reporting artifacts were added/updated.

## Evidence and reporting correction

- [Frozen run plan](comparison-plan-after-cleanup.json), [binary hashes](binaries-before.sha256), [main input hashes](main-before-build.sha256).
- [Corrected full summary](comparison-summary-v2.json), [validation history](validation.md).
- [Successful release build log](ordinary-release-build-after-cleanup-v2.log). Earlier capped and failed build logs remain alongside it.
- The first generated `comparison-summary.json` is preserved but superseded: its reporter used0 for an unavailable idle maximum and calculated an over-precise ratio from truncated idle means. A reporting-only correction returns null for unavailable maxima and suppresses that ratio. Focused summary fixtures and all14 parser assertions passed. No raw log, metadata, benchmark input, run selection, frame timing, wall-time comparison or RSS result changed.
