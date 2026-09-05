# Active native coarse-bin dispatch

The objective remains native 3840×2160, 1,000 actual redraw actions, ordinary
release defaults, shell mean ≤8 ms and p99 ≤30 ms. Prior `ec4c2127a` evidence
shows roughly 12 ms sparse fine and 11 ms coarse means under per-dispatch
instrumentation; its unprofiled default is 37.252 ms / 97.533 ms, failing both
gates. No reduced resolution, discarded samples, modified animations/actions,
changed AA tolerance, or relaxed fence/gate limits are permitted.

## Baseline protocol declared before measurements

While Terra implements an initially opt-in active-native-bin coarse path, run
these preserved-binary checks in fixed order, without builds/tests/other
benchmarks overlapping:

1. `shell-redraw-dispatch-profile-default-release-20260905` with all timing and
   experimental flags cleared: ordinary default control, 1,000 actions.
2. The same binary with `VELLO_SPARSE_COARSE=1`, `QOS_REDRAW_BENCH_PROFILE=1`,
   `VELLO_GPU_TIMESTAMPS=1`, and `VELLO_GPU_DISPATCH_TIMESTAMPS=1`: legacy
   sparse-coarse stage-cost diagnostic, not acceptance.

The preserved SHA-256 is
`fe86c79768871ff2d479ca2b38a94e127088ca025db90e2b5e0a61720bf193ce`.
Use the established 15-minute process cap with 30-second TERM grace and the
unchanged one-second renderer fence. Preserve every failure. Source edits may
proceed against these immutable executables; source will be frozen before
compilation. Candidate timing and promotion criteria will be specified before
candidate measurement, after the bounded implementation and exact-pixel tests
establish which paths can actually be tested.

## Implementation boundaries

The candidate retains native bin/tile coordinates and per-tile draw/clip order.
A bounded bitmap deduplicates selected native bins, then packs their IDs after
the used tile-bit words in the existing 8 KiB uniform. A separate shader
permutation dispatches only those bins. Unsupported bounds or insufficient
packing space retain the existing sparse-coarse fallback; no extra storage
buffer or reduced target is allowed. The first physical dispatch must forward
failures even when native bin zero is inactive. Sparse coarse and the new
active-bin option remain off by default until correctness and performance
provide grounds for promotion.

## User-requested historical comparison

Before compiling the new source, answer the question about the earlier
20.176 ms UI mean with a fixed preserved-binary sequence: original
`ui-perf-final-stable-damage-bindings` (SHA-256
`20d39a8473631ba17a958dfe0ee75be5caf864626edbc5b697c96df01325afc9`),
then current `ui-perf-dispatch-profile-default-release-20260905` (SHA-256
`e3bf83357fbf4ac2554adc3efe366337aaa1dfed9ed06a3bf5640cd08059385d`).
Clear all renderer experimental/timing flags and all QUICK_UI_DASH flags;
set only ITERS=1000, WIDTH=3840, HEIGHT=2160 and FRAME_BUDGET_US=30000.
Run `dashboard_perf_bench` serially with the same process cap, no compiler,
test or other benchmark active. Preserve all samples and failures. This is
the UI workload, not the shell render/readback workload; renderer/harness
versions differ across these binaries, so inspect reported counters and do
not attribute differences to a specific change without additional evidence.

### Historical comparison results

| Preserved UI binary, same native-4K/1,000-frame configuration | Mean | p99 | 30 ms p99 gate |
| --- | ---: | ---: | --- |
| Original binary behind the historical 20.176 ms median | 19.789 ms | 65.914 ms | **Fail** |
| Current committed `ec4c2127a`-era binary | 8.195 ms | 26.868 ms | **Pass in this run** |

Both render all 1,000 frames, skip none, report 518 widgets, and perform zero
CPU readback. These single observations reproduce the old mean and show no
old-versus-current UI regression in this pair. They are not the shell action
workload: the previous 37.252 ms shell mean includes its own scene, input,
render/readback, and CPU XRGB destination handling. Earlier 66–69 ms UI runs
and their slow preserved previous-binary control remain recorded and are not
explained away by this pair; a specific host/driver/scheduling cause is still
unproven. Current inherited benchmark/renderer flags were checked and none
were present, though that does not establish the historical environment.

The original renderer also used translated rectangle-local rendering,
subsequently replaced after stronger text-edge pixel checks; the current
renderer retains native coordinates. No rollback to that old code is implied.
This new passing UI observation does not satisfy the separate ≤8 ms shell
mean or certify constant 60 FPS, and is not a measurement of the unbuilt
active-bin worktree changes.

### Active-bin baseline results

The preserved default shell control completes 1,000 actions at 42.996 ms mean /
124.846 ms p99 (Fail), with 999 sparse / one full attempt and no recovery.
The subsequent legacy sparse-coarse dispatch diagnostic completes 1,000 at
49.480 ms / 135.633 ms (DisabledDiagnostic), matching 1,000/1,000 GPU samples
with no missing/stale/failure/recovery counters. Sparse coarse itself averages
2.929 ms across 999 samples; sparse fine 14.531 ms across 999 samples. This is
not directly comparable to the unprofiled total, nor proof that active-bin
dispatch will meet the goal. All raw logs are preserved alongside this file.

## Validation and candidate protocol

Source review retained the public legacy low-level coarse API via a private
active-dispatch helper. It also strengthened the 257-pixel fixture to change
pixels beyond the actual native 256-pixel bin boundary, and uses exact bytes
for the two new simple fixtures. Existing AA tolerances and the native-4K
shell oracle are unchanged. The first vendor run passed 47 tests with a new
visibility warning; that warning was fixed without removing the public API.
The final vendor run passes all 50 tests without that warning. Initial and
final logs are both retained. Both new GPU tests pass without being skipped:
the edge/clip/translucency fixture is exact, with matched timestamp samples
and stage 28 present, and the bin-1 overflow fixture actually raises
`failed=0x2` before exact dense recovery. The existing disjoint masked
image/clip fixture passes with legacy and active paths. All 13 shell tests,
including the exact native-4K action oracle, pass with active flags and again
under defaults (each oracle: 11 sparse / two full attempts). Focused strict
runtime/example clippy passes. Test-inclusive changed-line clippy found one
new style warning for the 130-line fixture; its setup was extracted without
changing geometry or assertions. The final test-inclusive check reports zero
changed-line/compiler diagnostics (unrelated baseline warnings are filtered,
not claimed fixed). Both active GPU tests were rerun after this extraction
and pass exactly, including the actual overflow. Workspace/vendor formatting
and diff whitespace checks pass. No candidate timing or default promotion has
occurred at that preflight point. The subsequent ordinary release build
completed successfully in 14m 01s; hashes are preserved in `artifacts.md`.

Predeclare the candidate measurements now, before any candidate timing:

1. After actual-path pixel/overflow tests and native-4K shell parity pass,
   compile both examples with the ordinary locked release configuration.
   Preserve uniquely named binaries and their SHA-256 hashes.
2. Run two shell dispatch diagnostics, legacy sparse coarse then active bins,
   with both timestamp flags and explicit diagnostic mode. Require stage 28
   in the active run and account for every expected submission. These runs
   cannot pass acceptance, and their totals are not an unprofiled speedup.
3. Run three serial rounds for each workload, comparing the same candidate
   binary with all flags cleared against only `VELLO_SPARSE_COARSE=1` and
   `VELLO_SPARSE_COARSE_ACTIVE_BINS=1`. Round order is default/active,
   active/default, default/active; finish shell then UI within each round.
   UI sets only the established 1,000-frame native-4K configuration and
   30,000 us p99 budget after clearing every inherited DASH flag. Shell
   keeps all 1,000 actions and its 8/30 ms gates. An experimental flagged
   run is comparative evidence, never default-release acceptance regardless
   of the old harness's printed gate label.
4. Preserve every failed gate, timeout and outlier; no compiler, test or other
   benchmark may overlap a measured process. Keep the same process/fence caps.
   Report per-run results and medians, not just the best observation.
5. Consider default promotion only if correctness is unchanged and both
   workloads show at least 5% lower median mean with no median-p99 regression.
   Inspect memory/copy counters for regressions too. A qualifying comparison
   would still need a new default build and unflagged acceptance runs; it is
   not itself completion of the shell goal. If this criterion fails, retain
   the path as off-by-default experimental work and report that explicitly.

After those candidate rounds, run one additional fixed preserved-UI pair:
`ui-perf-zero-gradient-default-release-20260905` (the exact executable that
previously recorded 67.203 ms), then the committed dispatch-profile UI binary
used in the fresh 8.195 ms observation. Use the same fully cleared native-4K
configuration and retain both results. This pair is specifically a check for
repeatability of unchanged executables, not active-bin performance evidence.
Old logs show 71.05 CPU seconds / 70.68 wall seconds for the slow previous
binary; the fresh current control reports 21.60 CPU seconds / 8.81 wall
seconds. That demonstrates different measured CPU/wall behavior, but does
not by itself prove a quota, driver, clock, or scheduling cause.

## Candidate results and decision

All 16 predeclared candidate/control runs completed and are saved alongside this
file. Every workload is native 3840×2160 with 1,000 real redraws/actions.
The candidate remains **off by default**: it fails the predeclared promotion
criterion because UI mean and p99 regress. The shell goal is still unmet.

| Workload / round | Default mean / p99 | Active candidate mean / p99 | Default max | Active max |
| --- | ---: | ---: | ---: | ---: |
| shell / 1 | 39.611 / 81.920 ms | 30.036 / 74.137 ms | 401.503 ms | 403.567 ms |
| shell / 2 | 38.812 / 82.523 ms | 29.218 / 62.985 ms | 454.615 ms | 438.356 ms |
| shell / 3 | 36.758 / 87.645 ms | 28.110 / 70.040 ms | 462.343 ms | 397.337 ms |
| ui / 1 | 8.234 / 23.063 ms | 8.330 / 26.803 ms | 75.541 ms | 98.033 ms |
| ui / 2 | 8.209 / 21.353 ms | 8.618 / 41.179 ms | 55.125 ms | 80.472 ms |
| ui / 3 | 8.123 / 26.278 ms | 7.962 / 22.916 ms | 72.992 ms | 41.081 ms |

| Median of three unprofiled runs | Default | Active candidate | Change |
| --- | ---: | ---: | ---: |
| shell mean | 38.812 ms | 29.218 ms | -24.72% |
| shell p99 | 82.523 ms | 70.040 ms | -15.13% |
| ui mean | 8.209 ms | 8.330 ms | 1.47% |
| ui p99 | 23.063 ms | 26.803 ms | 16.22% |

Every shell run records 999 sparse / one full attempt, no recovery, and fails
both unchanged acceptance gates. All three default UI runs pass the 30 ms
p99 gate. Active UI round 2 fails (41.179 ms p99); its other printed passes
remain experimental observations, not production acceptance. Every UI run
renders all 1,000 events with zero CPU screenshot readback. No trial or maximum
was discarded. No constant-60-FPS or physical-2010-laptop claim follows.

The 24.72% shell mean reduction is **default dense coarse versus the combined
sparse-coarse + active-bin candidate**. It is not an isolated 24.72% benefit
from the new native-bin list alone: sparse coarse already existed as an
experimental option. The comparison below isolates that additional path
selection only under instrumentation.

### Dispatch diagnostics, not acceptance

Legacy sparse coarse and active bins both match all 1,000 GPU submissions,
with every missing/stale/unsupported/pending/failed/invalid counter zero and
no recovery. The active run records stage 28 on all 999 sparse submissions.
Its sparse-coarse mean is 2.350314 ms versus 3.145069 ms for legacy sparse
coarse (25.27% lower); their stage p99 values are 18.026980 / 24.766830 ms.
Sparse fine still averages 12.467919 / 13.370189 ms. Whole instrumented means
are 39.097 / 43.244 ms, but changes in other stages and instrumentation prevent
assigning that whole difference solely to the bin list. Both runs explicitly
print DisabledDiagnostic. Per-stage p99 values are not additive.

### Memory and copy accounting

| Metric | Default | Active candidate |
| --- | ---: | ---: |
| UI tracked GPU bytes, every trial | 62,282,124 | 62,290,316 |
| UI median whole-process peak RSS | 377,260 KiB | 385,664 KiB |
| Shell median whole-process peak RSS | 545,200 KiB | 551,020 KiB |

There is no memory-saving claim: the experimental selector uses 8,192 additional
tracked GPU bytes relative to default dense coarse, while reusing the same
selector storage as the existing sparse-coarse path. Measured peak RSS is
higher, not lighter. Native output/staging ownership is unchanged. The two
shell diagnostics still submit about 0.81 MB of texture-to-staging payload
per action (807,591 / 808,636 bytes, varying with scene timing), and both write
33,177,600 CPU destination bytes per action. These are API payload counters,
not measurements of physical GPU bus traffic or KMS display transfers.

### Unchanged-binary repeatability check

The exact `ui-perf-zero-gradient-default-release-20260905` executable which
reported 67.203 ms mean / 332.911 ms p99 earlier now reports **8.304 / 27.942 ms**
with the same SHA-256. The following unchanged committed UI control reports
7.972 / 20.834 ms. Both complete all 1,000 native-4K redraws and pass the p99
gate in these observations. The slow executable now uses 21.65 CPU seconds /
8.91 wall seconds, versus 71.05 / 70.68 previously. This demonstrates that the
large observed change is not explained solely by source revisions. It does
not establish the precise historical environment, driver, scheduling, or
clock cause. Earlier slow results remain valid recorded failures.

The original 20.176 ms result was also reproduced separately at 19.789 ms;
it was the UI renderer workload, not the shell's render/readback/XRGB workload.
