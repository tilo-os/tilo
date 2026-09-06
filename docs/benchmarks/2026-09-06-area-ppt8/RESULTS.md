# Area-PPT8 v5 — measured result

The goal remains unmet. No default promotion, commit, push or merge.

Adding PPT8 to the same B+X shell configuration reduces the ratio-of-medians
mean by **9.50%** and p99 by **22.90%**, but increases median peak process RSS
by **5.07%**. The resulting 20.645 ms mean passes the mean target; 54.805 ms
median p99 still exceeds the 50 ms target. All three BXP runs fail the p99 gate.

UI D→P median mean improves 2.36%, median p99 only 1.74%, while peak RSS rises
7.30%. One P run passes 30 ms, two do not; median p99 is 38.105 ms.
These results do not establish repeatable goal success or a lighter renderer.

## Primary matrix

Three predeclared attempts per row; medians use completed, eligibility-checked
1,000-sample reports. Failed numeric gates remain included. N=2 rows have a
retained incomplete attempt, not a replacement. Worst is the largest sample
among completed runs; it does not include the aborted fence waits.

| Workload | Variant | Eligible complete | Mean ms | p99 ms | Worst ms | Peak RSS KiB | Numeric passes |
|---|---|---:|---:|---:|---:|---:|---:|
| shell | D | 3/3 | 37.625 | 86.600 | 586.492 | 582,692 | 0/3 |
| shell | P | 2/3 | 35.676 | 81.557 | 419.424 | 612,052 | 0/3 |
| shell | B | 3/3 | 24.385 | 71.690 | 483.365 | 595,092 | 0/3 |
| shell | BP | 2/3 | 22.257 | 58.101 | 420.371 | 615,648 | 0/3 |
| shell | BX | 3/3 | 22.812 | 71.082 | 611.129 | 588,980 | 0/3 |
| shell | BXP | 3/3 | 20.645 | 54.805 | 415.820 | 618,844 | 0/3 |
| ui | D | 3/3 | 8.476 | 38.780 | 118.520 | 377,864 | 0/3 |
| ui | P | 3/3 | 8.276 | 38.105 | 119.111 | 405,440 | 1/3 |

D = current default. P = Area-PPT8. B = adaptive sparse coarse + native tile
bounds requested. X = partial CPU XRGB destination writes. All binaries use
the ordinary release build settings. Each report is native 3840×2160 with
1,000 actions. Profiling/timestamps are disabled.

The P and BP shell rows have only two completed runs. Their ratios of medians
therefore compare unequal completed cohorts; the per-round comparisons in
comparison-summary.json are preferable. BXP/BX and UI P/D have all three pairs.
For BXP/BX, paired mean reductions are 6.68%, 7.35%, 11.04%; paired p99 reductions
are 1.39%, 27.80%, 9.75%. The variation is retained, not hidden by the median.

## All attempts and failures

All 30 planned attempts ran, with 28 full 1,000-sample reports. Two P-enabled
shell attempts timed out at the existing one-second fence:

- shell-ppt8-round-1: initial desktop paint, before measured samples.
- shell-bounds-ppt8-round-1: **measured sample 1**, not initial startup.

The earlier commentary grouping both as startup failures was imprecise; the
raw logs establish the distinction above. The association with first full and
sparse use suggests a cold shader-cache hypothesis, but does not prove it.
No fence, recovery count, warm-up, timer or workload was changed.

Six UI B/BP reports are complete but fail the predeclared positive-bounds
eligibility check: requested/available is true, actual applied bounds is zero.
They are excluded from the primary bounds comparison, not discarded:

| UI configuration | Reports | Descriptive mean ms | Descriptive p99 ms | Peak RSS KiB | Applied bounds |
|---|---:|---:|---:|---:|---:|
| B requested | 3/3 | 8.381 | 36.928 | 385,512 | 0 |
| BP requested | 3/3 | 8.422 | 39.065 | 410,852 | 0 |

These are no-bounds-path observations, not evidence of bounds performance.
Adaptive active submissions are nonzero (66–90), and P is actually selected
on all 1,000 sparse attempts in each BP run. Primary eligibility remains
unchanged; no-path-descriptive-summary.json preserves these secondary numbers.

## Copies, memory and scope

PPT8 introduces no intended new output-copy path. BXP and BX have the same
median CPU XRGB payload, 807,591,936 bytes per 1,000 actions versus
33,177,600,000 for full CPU destination writes. That earlier XRGB reduction is
not a new PPT8 saving. The small first-round payload difference reflects
observed damage coverage; it is not attributed to a new copy algorithm.

All 12 UI reports retain known GPU bytes = 62,282,124 and CPU readback bytes = 0.
Known-GPU accounting is not measured peak VRAM or physical GPU bus traffic and
does not capture private-register/scratch spilling. RSS is whole-process peak
resident memory, including initialization; it increased with PPT8.

This host is aarch64 Vulkan llvmpipe (LLVM 22.1.8, 128 bits), device type Cpu.
There is no physical 2010-laptop, hardware-GPU, DRM/KMS scanout or constant-60FPS
proof. First measured frames and all outliers remain in every complete report.

## Validation and reproducibility

All 20 v5 validation commands pass. The harness reports 824 passing executions,
including 13 explicit configuration-guard returns recorded separately; 16
ignored executions are documented, with the required native proof run explicitly.
The new four-way odd-edge comparison and native 33,177,600-byte P4/P8 full-output
proof are exact. Existing UI tolerance policies were not changed. Original
independent native shell comparisons pass D/B/P/BP with actual P selection
counters. Prior independent default-GPU rounding instability is not declared
resolved by these successful runs.

Ordinary release build passed in 13m17.91s. Its 10,758,288 KiB build peak RSS is
not app memory. Both copied binaries, all 33 source hashes, all 30 saved raw
logs and all 30 reparsed metadata artifacts were verified. Sixty artifact
hashes are preserved. No failed attempt or outlier was replaced.

Run `node docs/benchmarks/2026-09-06-area-ppt8/summarize-saved.mjs` from the
repository to reparse the saved raw logs, check metadata against the active
plan, and reproduce the primary summary. See comparison-plan-v2.json,
comparison-summary.json, postcomparison-audit.json and comparison-artifacts.sha256.

