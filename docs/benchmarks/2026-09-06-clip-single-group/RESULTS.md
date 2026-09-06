# Clip single-group: ordinary release results

The goal remains unmet. All 30 predeclared attempts completed the original 1,000 actions at native 3840×2160 and passed the evidence audit. No shell attempt passed both mean ≤25 ms and p99 ≤50 ms. One of 12 UI attempts passed p99 ≤30 ms; it was not repeatable. The candidate stays default-off and is not promoted. No commit or push has occurred.

## Current-state matrix

Three runs per row. Mean and p99 are medians of complete, eligible runs—not the best run and not pooled frame percentiles. Worst sample is the maximum observed across the three runs.

| Workload / configuration | Complete | Mean ms | p99 ms | Worst sample ms | Peak RSS KiB¹ | Gate passes |
|---|---:|---:|---:|---:|---:|---:|
| shell / Default | 3/3 | 38.509 | 87.070 | 468.216 | 597004 | 0/3 |
| shell / + clip | 3/3 | 40.180 | 94.013 | 502.744 | 604488 | 0/3 |
| shell / Bounds + XRGB | 3/3 | 23.225 | 65.297 | 464.794 | 605852 | 0/3 |
| shell / Bounds + XRGB + clip | 3/3 | 23.255 | 64.096 | 549.736 | 601680 | 0/3 |
| shell / Bounds + XRGB + PPT8 | 3/3 | 21.584 | 62.838 | 458.031 | 623328 | 0/3 |
| shell / Bounds + XRGB + PPT8 + clip | 3/3 | 21.812 | 68.767 | 423.947 | 626976 | 0/3 |
| ui / Default | 3/3 | 8.765 | 41.510 | 107.811 | 377556 | 0/3 |
| ui / + clip | 3/3 | 8.677 | 39.557 | 111.236 | 378764 | 0/3 |
| ui / PPT8 | 3/3 | 8.547 | 43.671 | 110.986 | 406788 | 0/3 |
| ui / PPT8 + clip | 3/3 | 8.410 | 43.323 | 118.957 | 406896 | 1/3 |

¹ Median of each process's observed peak RSS; not peak GPU memory. Shader/compiler/driver memory is included in process RSS. Build peak RSS (10,763,580 KiB) is unrelated to application memory.

## Same-binary incremental comparison

Ratio-of-medians changes below; negative means less observed time or RSS. All three matched pairs are present. See comparison-summary.json for the per-round deltas, which matter because medians can mask inconsistent paired behavior.

| Add clip to | Mean change | p99 change | RSS change |
|---|---:|---:|---:|
| shell / Default | 4.34% | 7.97% | 1.25% |
| shell / Bounds + XRGB | 0.13% | -1.84% | -0.69% |
| shell / Bounds + XRGB + PPT8 | 1.06% | 9.44% | 0.59% |
| ui / Default | -1.00% | -4.70% | 0.32% |
| ui / PPT8 | -1.60% | -0.80% | 0.03% |

Clip worsened shell mean in all three default pairs and all three Bounds+XRGB+PPT8 pairs. The Bounds+XRGB pair has mixed timing effects. UI median times improve slightly, but paired p99 with clip alone is worse in two of three rounds. PPT8+clip UI p99 was 43.323, 27.257 and 49.035 ms; its one pass cannot establish the goal. No consistent tail-latency win is established.

## Copies, memory and scope

Clip was actually selected for all 15,000 attempts in its 15 enabled runs; disabled runs selected none. This confirms path coverage, not speed. The candidate retains original GPU buffer allocation/lifetimes and workgroup arrays; no GPU-copy or GPU-buffer reduction is claimed.

All UI runs report the same 62,282,124 known GPU bytes and zero CPU readback. This is endpoint accounting, not measured peak VRAM or driver memory. Shell default/C both write 33,177,600,000 CPU XRGB destination bytes per 1,000 actions. The Bounds+XRGB controls and combined candidates are around 807–809 million bytes through the earlier XRGB damage history, not a new clip optimization. Small variation in those writes is not credited to clipping.

## Reproducibility and limitations

Source v6 passed all 20 validation commands, including the new exact native-4K full/sparse clip tests and existing native shell oracles. The 839 harness passes include 13 explicit configuration-guard returns; 18 ignored executions are separately documented. Earlier v1–v5 failed attempts remain in this directory. Passing the current native oracle does not declare the prior independent default pixel instability resolved.

Ordinary release: configured opt-level 3, fat LTO, one codegen unit; compiler/profile overrides absent/cleared. Release build passed in 13:58.87. Binaries are preserved at unique paths and hashed in v6-release-binaries.sha256. All 34 source/config/evidence inputs and both binaries were unchanged after the 30 runs. All 30 raw logs and their metadata were verified; raw-artifacts.sha256 covers 90 artifacts. Parser and aggregation synthetic checks passed (13 and 8 respectively); they are not benchmark evidence.

This is aarch64 Vulkan llvmpipe on the current host, not a physical 2010 laptop, hardware GPU, DRM/KMS scanout or constant-60-FPS demonstration. The current x86-64-v3 build floor is unchanged. The next investigation is a read-only feasibility review of exact geometry-only reuse; reuse hits and speed are not yet measured.
