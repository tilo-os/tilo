# Conservative sparse segment-row-mask candidate

Goal unchanged: ordinary release defaults, 1,000 real actions at native
3840x2160. UI p99 <= 30 ms; shell exact mean <= 25 ms and p99 <= 50 ms.
Only after verified completion: final polish, scoped commit and push; user merges.

## Previous turn classification and evidence

The previous turn was progress, not a repeated blocker: it completed the
24-run SOLID-fusion comparison (23 complete sample sets, one retained timeout),
two GPU profiles, and a tested untimed UI-adapter reporting repair. Fusion
remains off by default. Against bounds alone it reduced median shell mean by
3.26% and p99 by 1.74%, while peak RSS rose 3.84%. Sparse fine GPU time improved
only about 2.27% and its p99 worsened. The relative UI guard and absolute
latency requirements failed. See ../2026-09-06-solid-command-fusion/README.md.

At this turn's start, all v4 source/config hashes matched, no compiler/test/
benchmark process was live, and nothing was staged. HEAD was
1acdb59f8ca126e4e3a7e936e78c11b7a6182929 on codex/clippy-runtime-ui-recovery.
Unrelated OS/security/workflow changes remain user-owned and untouched.

## Candidate contract

Exact opt-in VELLO_SPARSE_SEGMENT_ROW_MASK=1 stays disabled by default.
Use the existing trailing 32-bit padding of the 24-byte Segment as a
conditional 16-row contribution mask. No new production buffer, binding,
allocation, GPU copy or readback. Added metadata stores/loads and computation
are costs, not free bandwidth; there is no speed or memory-saving claim yet.

Compute the mask from final tile-relative endpoints/y_edge AFTER path tiling's
clipping, robustness adjustments and orientation swap. Only omit a segment
when both original Area-AA dy and y_edge contributions are proven zero on the
current native row. Preserve segment order, winding rules, clip/blend state,
coordinates and all remaining arithmetic. Use conservative endpoint/edge
margins and all-row fallback for nonfinite, out-of-range or uncertain values.
A horizontal segment can still have a nonzero y_edge contribution.

Actual selection requires valid, applied Active native path bounds and both
matching GPU path-tiling producer (direct/indirect as selected) and sparse Area
fine reader. Missing partners, absent/invalid bounds, CPU/full/recovery and
ordinary shared-coarse diagnostics keep their original shaders/resources/grid.
Requests for quad skip, opaque prune or SOLID fusion preempt row masking; no
cross-product permutations. Requested/available/actually-selected are distinct.

Terra owns source changes. Root audits and orchestrates evidence. Freeze every
affected owner before compilation, tests or runtime execution. Preserve all
old logs/artifacts and the original unresolved native full-render discrepancy.
Never relax tolerances, replace failed runs, drop outliers or simplify workloads.

## Required validation before candidate timing

- Exact flag parsing, 24-byte layout/stride, actual selection versus request,
  direct/indirect mode, missing writer/reader, missing/invalid/unapplied bounds,
  old-feature precedence, full/CPU/recovery/shared isolation and resource lifetimes.
- Independent CPU clear-bit oracle uses the ORIGINAL stepwise equations:
  y=p0.y-row, delta_y=p1.y-p0.y, clamp(y+delta_y), not p1.y-row.
  Broad deterministic boundaries, horizontal/left-edge, signed-zero, nonfinite
  and finite uncertainty cases; the prior telemetry model is not GPU proof.
- Actual GPU-produced nontrivial mask words, explicit meaningful segment fields
  and mask validation (old captures ignored padding), exact requested pixels
  for original sparse versus masked fine over the SAME captured geometry when
  feasible, plus ordinary full/retained oracles. Keep existing shared diagnostics
  ordinary; any new same-geometry mask helper is cfg(test), bounded and frees
  buffers only after both readers/captures complete.
- Real GPU nonzero/evenodd winding, clips/translucency, negative/offscreen paths,
  partial native edges, direct/indirect writers, overflow and recovery.
- Vendor tests, UI/shell example/accounting tests, native-4K default/B/BR oracles,
  existing parity/edge/overflow tests, strict relevant lint, changed-line/compiler
  test lint, formatting and diff checks. Retain failures under versioned logs.

The candidate can only advance with those checks. The previous native
same-scene full-render discrepancy remains unresolved and must be causally
resolved before promotion. Planned measurements are exploratory from the
outset, not acceptance-eligible; they cannot erase that failure.

## Predeclared performance comparison

Build with ordinary locked workspace release options: no profile/driver/CPU
override. Freeze/hash source plus configuration, preserve/hash both executables.
D = default, B = adaptive coarse + tile bounds, BR = B + row mask.
R alone cannot select this bounds-dependent optimization and is covered by
fallback tests rather than redundant performance runs.

Three balanced orders: D/B/BR, BR/D/B, B/BR/D. Workload order shell/UI,
UI/shell, shell/UI. All 18 invocations and exact cleared environments are in
comparison-plan.json. Quad/opaque/fusion flags remain unset. Each invocation
uses 1,000 native-4K samples, unchanged one-second map fence, four render
attempts, and 15-minute process cap plus 30-second TERM grace.
No compiler, tests, diagnostics or competing benchmark overlaps timed runs.

Keep every attempted run; no replacement for timeouts or budget failures.
Report completion denominators, per-run results, medians of completed-run
mean/p99, worst sample, peak process RSS, known GPU allocation snapshots,
native extent, each process's actual adapter and selected submission counts.
No GPU-memory/copy claim from reduced row arithmetic or segment metadata.

Relative guard BR/D: shell median mean at least 10% lower, no shell median-p99
regression; UI mean/p99 no worse than 3%/5%. BR/B isolates the added row-mask
effect. No relative win substitutes for all absolute gates, correctness, fresh
ordinary-release default rebuild and fresh unflagged acceptance runs.

This ARM/software-Vulkan host cannot certify a 2010 laptop, physical GPU-bus
copies, DRM/KMS presentation or constant 60 FPS. The original hardware ambition
remains unverified; the current explicit goal uses the stated benchmark gates.

## Validation checkpoint (v4)

All 16 planned validation commands and both additional checks passed on the
same frozen source: 134 test passes, strict production and vendor-test Clippy,
test-inclusive changed-line/compiler Clippy with zero diagnostics, rustfmt and
diff checks. Source/configuration hashes match after validation. Exact command
lines, results, and the earlier compile/harness failures are retained in the
versioned validation plans, summaries and logs.

The GPU mask fixture validates direct/indirect same-geometry sparse readers
against dense pixels and checks actual captured clear bits (see source-audit.md).
All six native-4K shell oracle configurations pass: default, bounds, bounds plus
row mask, row mask without bounds, forced bounded Active, and indirect bounded
Active. Mask-selected sparse submissions were 0/0/11/0/11/11 respectively;
each fixture made 11 sparse attempts and two full attempts including setup.
Overflow recovery, adaptive fallback, partial edges, clipping and translucency
also pass without tolerance changes. These fixtures are not 1,000-action timings.

## Completed ordinary-release comparison

The unchanged workspace release build finished successfully in 13m43s.
Preserved binaries and SHA-256 hashes are in release-artifacts.sha256; sizes
are in release-artifact-sizes.txt. Source/configuration hashes match after
validation, release compilation and all timings. Both executable hashes
match after timing. No compiler/test/diagnostic overlapped the timed runs.

All 18 predeclared invocations completed their 1,000-action sample sets; none
were replaced or discarded. Every process explicitly reported native
3840x2160 and llvmpipe (LLVM 22.1.8, 128 bits), Vulkan, Cpu. Results remain
exploratory, not acceptance-eligible. These are medians of three completed
run means/p99s; Worst is the largest individual sample across those runs.
RSS is the median of each process's peak RSS, not GPU memory.

| Workload / mode | Complete | Mean ms | p99 ms | Worst ms | Median peak RSS MiB | Numeric budget passes |
|---|---:|---:|---:|---:|---:|---:|
| shell / default | 3/3 | 38.007 | 90.912 | 436.331 | 556.0 | 0/3 |
| shell / bounds | 3/3 | 25.074 | 72.712 | 492.601 | 556.9 | 0/3 |
| shell / bounds-row-mask | 3/3 | 25.700 | 70.334 | 976.797 | 569.9 | 0/3 |
| ui / default | 3/3 | 8.190 | 34.406 | 80.031 | 369.0 | 0/3 |
| ui / bounds | 3/3 | 8.216 | 36.177 | 78.728 | 377.8 | 0/3 |
| ui / bounds-row-mask | 3/3 | 8.239 | 32.894 | 141.716 | 386.7 | 1/3 |

Against bounds alone, the added row mask makes median shell mean **2.50%
slower**, lowers median p99 **3.27%**, and raises median peak RSS **2.33%**.
It does not close the target gap. Against default, the combined mode lowers
shell mean 32.38% and p99 22.64%, but that comparison includes the existing
bounds optimization and must not be attributed to the mask alone.

The predeclared relative BR/default guard passes (including UI mean/p99),
but absolute gates fail. The mask is **not promoted** and remains disabled
by default. Goal completion, final polish, commit and push have not occurred.

### Actual work and resource evidence

Every shell timing made exactly 1,000 render attempts: 999 sparse, one full,
zero recovery. Bounds applied 999 times in B and BR, zero in D. Row masking
was selected 999 times only in shell BR. Quad skip, opaque prune and SOLID
fusion were selected zero times in every run. No reduced-quality, scaled or
replacement workload was used. The largest BR frame was 976.797 ms; this is
not constant 60 FPS even apart from the failed p99 gate.

Every UI run made 1,000 sparse submissions. UI bounds and row-mask selections
were both zero; B/BR sometimes selected active coarse bins, which is distinct
from narrowed path bounds. UI timing variation is therefore not evidence of
executed row-mask savings. UI CPU readback bytes remained zero. Known GPU
allocation at the end remained 62,282,124 bytes in all nine UI runs; this is
not peak VRAM or a physical-bus measurement. Shell's full destination-copy
implementation and 33,177,600-byte frame size are unchanged; this unprofiled
batch does not add a new measured copy-bandwidth figure.

The experiment adds no production buffer/binding/copy/readback; it uses the
existing segment padding, but adds mask stores, reads, shader pipelines and
computation. The measured RSS increase is retained rather than labelled a
memory win. The GPU-proof downloads are test-only, not in the timings.

Current native pixel checks pass, but the prior dense/full-render discrepancy
in the SOLID-fusion/native-bounds evidence remains unresolved. This batch
cannot erase it, and no default promotion is justified by timing alone.

See comparison-results.json for every run, run-integrity.json for exact
selection/adapter/extent checks, and candidate-decision.json for the decision.
