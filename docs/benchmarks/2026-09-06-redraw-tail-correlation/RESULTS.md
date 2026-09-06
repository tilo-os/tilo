# Native-4K redraw tail correlation

These are **diagnostic captures, not acceptance results or a speed comparison**.
The same ordinary release executable was run with profiling/timestamps enabled.
Every capture retained all 1,000 real actions at native 3840×2160.

## All-action diagnostic matrix

| Capture | Mean ms (exact sum / 1,000) | p99 ms | Maximum ms | Exact GPU matches | Retries |
| --- | ---: | ---: | ---: | ---: | ---: |
| BX round 1 | 33.221480 | 95.322 | 510.633 | 1,000/1,000 | 0 |
| B round 1 | 34.832285 | 90.515 | 535.943 | 1,000/1,000 | 0 |
| B round 2 | 35.901130 | 86.764 | 530.050 | 1,000/1,000 | 0 |
| BX round 2 | 32.448365 | 91.414 | 414.308 | 1,000/1,000 | 0 |

B enables adaptive sparse coarse plus native tile bounds. BX additionally
enables partial CPU XRGB destination writes. All remain opt-in, not defaults.
Profiling adds work, so these times must not be compared as a regression or
improvement against the previous uninstrumented results.

Every attempt completed with the intentionally non-success diagnostic exit:
`diagnostic run completed: acceptance gates are intentionally disabled`.
No timeout, replacement, truncated output, missing/stale timestamp, or retry.
Every action row's wall value cross-checks with its run's exact total and
nearest-rank p99, and phase/write/attempt/timestamp aggregates also match.

## What the same-action records establish

- GPU-completion-plus-map dominates the observed wall phases: its all-action
  mean is 29.245–30.540 ms across captures. It includes rendering, queueing,
  copying, mapping and waiting, not merely a GPU memory copy.
- CPU copy/swizzle means are 2.044–2.118 ms in B and 0.188–0.189 ms in BX.
  This agrees with the earlier CPU-write opportunity, but is not a substitute
  for the separately measured uninstrumented comparison.
- Every run's maximum is action 0, the first measured full-frame redraw.
  It remains in every primary total. Later actions are 999 sparse redraws;
  their post-capture attribution is stored separately, not used for acceptance.
- The sparse cohort has **the same p99 value as the full 1,000-action report in
  all four captures**. Removing the initial full redraw would not solve p99.
- Between 84 and 131 of the 999 later sparse actions exceed 50 ms in each
  diagnostic run. Among these 414 actions combined, the largest timed GPU
  category is fine rasterization for 243, other renderer stages for 165, and
  coarse for 6. This is category size, not proof of the cause of each delay.
- For the 999-action sparse cohorts, correlation of wall time with map/completion
  is 0.904–0.926; with fine stages 0.479–0.550; with other stages 0.580–0.655;
  with submit 0.395–0.483. The initial dense frame inflated some all-action
  correlations. No single fine/coarse/copy bottleneck explains every tail row.
- Input, registry, layout and scene generation are comparatively small. Both
  quick-settings opening and its outside dismissal exhibit slow sparse actions.
  The overview-button action is dismissal, not an overview-opening workload.

Examples: BX round 2 action 174 spent 86.495 ms in submit while its fine
timestamp was 14.199 ms. Action 949 spent 125.981 ms in map/completion, with
10.215 ms fine and 110.949 ms other timed stages. BX round 1 action 187 had
102.569 ms coarse. These are joined rows, not subtraction of unrelated quantiles.

## Interpretation limits and next priority

The adapter is llvmpipe (LLVM 22.1.8, 128 bits), Vulkan, device type Cpu.
It is not a physical 2010 laptop, a hardware-GPU result, KMS presentation,
or proof of constant 60 FPS on a display.

CPU phases are elapsed wall intervals, not CPU execution time. GPU stages
overlap these intervals and cannot be added as separate wall costs.
A GPU timestamp on this software renderer does not distinguish useful work
from all scheduling effects. Unassigned wall time is not labelled scheduling.
Read-only source review confirmed that per-dispatch timestamps do not disable
normal compute batching: they write queries around dispatches within the same
prepared compute pass (wgpu_engine.rs, encode_compute_batch). Query writes,
resolve/map work and driver effects still add diagnostic overhead. These are
not measurements of uninstrumented per-stage production costs.

Next priority is reducing renderer work/dispatch cost across stages while
preserving pixels and Area AA. A further CPU-copy-only change is not sufficient
to explain or eliminate this distributed tail. No new renderer change or
default promotion is authorised by these diagnostic results alone.

The previous uninstrumented matrix remains the performance evidence:
[previous results](../2026-09-06-xrgb-damage-output/RESULTS.md).
The best tested shell candidate median was mean 22.165605 ms / p99 68.852 ms;
the required default shell mean ≤25 ms and p99 ≤50 ms remain unmet.
UI p99 ≤30 ms is also not reliably met. No commit or push yet.

## Verification and reproduction

Frozen v3 passed seven relevant validation commands (88 passing test executions).
Fourteen ignored executions are seven older opt-in diagnostics repeated twice;
the original native pixel oracle ran and passed separately in D and B.
Earlier independent GPU rounding instability is not declared resolved.

The ordinary release build passed; executable SHA-256:
`a748a6aa0fdc418ef58fb9bd2452e372798974eeaf3b57389618bc70cbea9a7a`.
All 33 source/config/fixture hashes and the binary remained unchanged.
The eight raw stdout/record artifacts are hashed in `capture-logs.sha256`.
All four primary analyses and four post-capture stratifications were independently
recomputed from disk and matched their saved JSON exactly.
The parser passed ten synthetic checks before runtime; those are not benchmarks.

Exact run commands/order are in `capture-plan-v2.json`; exits are in
`capture-runs.json`. Do not reuse the existing create-new output paths.
To reproduce analysis without running the renderer:

```sh
node docs/benchmarks/2026-09-06-redraw-tail-correlation/parser-fixtures.mjs
node docs/benchmarks/2026-09-06-redraw-tail-correlation/analyze-capture.mjs docs/benchmarks/2026-09-06-redraw-tail-correlation/capture-plan-v2.json bounds-xrgb-diagnostic-round-1 1
```
