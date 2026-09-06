# Opaque full-tile command pruning candidate

Goal unchanged: ordinary release defaults, native 3840×2160 and 1,000 real
actions/redraws. UI p99 ≤30 ms; shell mean ≤25 ms and p99 ≤50 ms. Verified
success precedes polish, commit and push; the user merges.

## Evidence and previous-turn classification

The preceding turn made progress: implementation and exact GPU validation of
quad-skip, followed by all 24 planned unprofiled invocations, ruled out a general
default enablement. Combined bounds+quad shell median mean/p99 was
24.003/62.126 ms, but UI p99 was 34.306 ms (+10.63% versus contemporary default)
and peak RSS rose. One quad-only shell run failed its one-second map fence.
All outcomes remain preserved in the preceding report.

Read-only source inspection shows current coarse processing appends all paint
commands even when a later opaque solid completely covers the tile. The next
candidate addresses that overdraw. No speedup or goal success is assumed.

## Implementation contract

Exact opt-in `VELLO_SPARSE_OPAQUE_PRUNE=1`, off by default. Dedicated GPU
coarse variants cover normal dense-grid coarse, legacy sparse coarse and active
native-bin sparse coarse, but are selected only through sparse-fine rendering.
Ordinary full render/recovery, CPU, unsupported paths and production shared-
coarse diagnostics remain unchanged.

Prune only before a color fill with no active/zero clip or rendered blend,
zero segments in this native tile, non-clear backdrop for its actual fill rule,
and exactly alpha255 packed premultiplied color. Restart this tile's PTCL cursor
after the original preserved header and restore its initial command-block limit,
then retain later command order. Do not change native geometry, AA arithmetic,
visible segment order, or output bounds.

Old dynamic PTCL chunks and segments remain valid allocated resources even if
their old commands become unreachable. Never reclaim/decrement global bump
counters or erase an earlier failure. Initially retain historical maximum blend
depth; resetting it is not necessary for this first experiment and is not a
memory-saving claim. No production buffer, binding, GPU counter or readback is
added.

Requested, available and actually selected coarse-prune shaders must be
distinguished. Selection counts are **not** counts of tiles or commands pruned;
the GPU evaluates that predicate. Establish actual pruning via bounded test-
only PTCL inspection, not invented CPU metrics or production readbacks.

## Validation and measurement protocol

1. Terra authors source changes. Freeze every source owner before building or
   testing. Pure/recording tests cover exact opt-in, shader selection and
   unchanged bindings/fallbacks. Preserve public API signatures with helpers.
2. Real GPU checks inspect pruned PTCL heads and compare exact requested pixels
   with dense reference. Cover lower complex paint, later translucent paint,
   completed deep clip/blend groups, later clips, dynamic command jumps and
   partial native edges. Negative alpha254, crossing segments, clear/even
   backdrop and active-clip cases must not prune. Keep overflow/failure recovery
   and every existing exact assertion. No tolerance relaxation.
3. Run renderer tests, shell/UI accounting and benchmark tests, native-4K action
   pixel oracles, strict production/example lint, test-inclusive changed-line/
   compiler lint, formatting and diff checks. Preserve every failed invocation.
4. Build with ordinary locked release options and no profile/driver overrides.
   Hash sources/configuration and preserve/hash both executable artifacts.
5. Compare the same binaries in four modes: default (D), adaptive+tile-bounds
   (B), opaque prune alone (P), and adaptive+bounds+opaque prune (BP).
   Quad skip remains unset in all these runs because the previous experiment
   failed its general default regression guard.
6. Use three rounds per workload: D/B/P/BP, BP/P/B/D, B/D/BP/P. Alternate
   shell/UI, UI/shell, shell/UI workload order, for 24 invocations. Clear every
   inherited renderer/benchmark option including the new flag.
7. Keep all 1,000 native-4K samples and existing animations. Retain the one-second
   map fence, four-attempt render recovery, 15-minute process cap plus 30-second
   termination grace. No compiler, test or other benchmark may overlap a timed
   invocation. Do not replace failures or discard outliers.
8. Report every run and medians of completed runs, with completion denominators,
   mean/p99/worst, process peak RSS, available tracked GPU snapshots and actual
   selected-shader counts. Instrumented diagnostics remain separate.

Relative guard for BP versus contemporary D: shell median mean at least 10%
lower, no shell median-p99 regression, UI mean/p99 no worse by more than 3%/5%.
Compare P/D and BP/B to isolate the new candidate. Report memory increases.

Relative improvement alone is not acceptance: all absolute goals must be met.
Any proposed default enablement requires correctness, a new ordinary release
build and fresh unflagged acceptance measurements. This software-Vulkan host
does not certify physical 2010-laptop performance, KMS presentation, physical
GPU-bus traffic or constant 60 FPS.

## Completed validation before timing

All eleven planned commands passed. Raw logs and command/result metadata are
retained in `validation-results.json` and their named logs.

| Check | Result | Tests passed / check |
|---|---|---|
| vendor-tests | PASS | 77 |
| shell-example-tests | PASS | 19 |
| ui-example-tests | PASS | 8 |
| ui-accounting | PASS | 1 |
| ui-adaptive-bounds-parity | PASS | 2 |
| ui-forced-bounds-edges | PASS | 1 |
| shell-forced-bounds-overflow | PASS | 1 |
| shell-default-native-oracle | PASS | 1 |
| shell-bounds-native-oracle | PASS | 1 |
| shell-prune-native-oracle | PASS | 1 |
| strict-clippy | PASS | strict lint |

The vendor suite ran 77 tests with no skipped tests. Shell benchmark tests
passed 19; five explicit diagnostic tests remain ignored, and the native-4K
oracle excluded from that command was run separately in all three modes.
Each native oracle used 11 sparse and 2 full submissions. Default selected no
opaque-prune shader; both requested variants selected it for all 11 sparse
submissions. The existing exact pixel comparisons and overflow checks passed.

Strict test-inclusive lint additionally found zero changed-line/compiler
diagnostics across the ten modified Rust files. `cargo fmt --all -- --check`
and `git diff --check` passed. Source/configuration hashes remained unchanged
through validation. `production-review.md` records the pre-build review.

## Unprofiled comparison outcome

All 24 planned invocations finished. Twenty-three completed their 1,000 samples.
The first default shell invocation failed during initial desktop paint with
`wgpu buffer mapping failed: fence wait: Timeout`; it has no sample summary.
Its entire log and peak process RSS (551952 KiB) remain recorded. It was not
replaced, treated as zero latency, or silently discarded. Root cause beyond
the fence timeout is not established.

**The goal is not achieved. No default enablement, commit, push or merge.**
BP shell median mean meets 25 ms, but its p99 is 64.042 ms, above 50 ms.
BP UI p99 is 31.245 ms, above 30 ms. Against bounds alone, adding opaque prune
increased median mean, p99 and peak process RSS in both workloads.
The new candidate therefore stays exact-opt-in only.

### Medians of completed runs

D/default is the current ordinary-release default; bounds means adaptive
coarse plus native tile bounds; prune means opaque prune alone; bounds-prune
combines both. Quad skip was unset in every run.
Worst is the largest measured frame from completed runs, not a median.
Pass counts include all three attempted invocations in the denominator.

| Workload | Mode | Complete | Mean ms | p99 ms | Worst ms | Peak RSS KiB | Passes |
|---|---|---|---|---|---|---|---|
| shell | default | 2/3 | 37.780 | 92.052 | 554.261 | 560456 | 0/3 |
| shell | bounds | 3/3 | 23.830 | 60.057 | 557.076 | 565208 | 0/3 |
| shell | prune | 3/3 | 38.887 | 98.058 | 628.573 | 563984 | 0/3 |
| shell | bounds-prune | 3/3 | 23.985 | 64.042 | 598.517 | 572660 | 0/3 |
| ui | default | 3/3 | 7.864 | 33.264 | 83.732 | 378168 | 0/3 |
| ui | bounds | 3/3 | 7.777 | 28.801 | 136.503 | 386064 | 2/3 |
| ui | prune | 3/3 | 8.056 | 32.989 | 153.609 | 381872 | 1/3 |
| ui | bounds-prune | 3/3 | 7.882 | 31.245 | 150.608 | 393980 | 1/3 |

### Relative changes

Negative latency means faster; positive RSS means heavier. These are ratios
of the medians above, not paired causal estimates or hardware-independent
speedups. D shell has only two completed runs and one preserved failure.

| Workload | Candidate / reference | Mean change | p99 change | Peak RSS change |
|---|---|---|---|---|
| shell | prune / default | 2.93% | 6.53% | 0.63% |
| shell | bounds-prune / bounds | 0.65% | 6.64% | 1.32% |
| shell | bounds-prune / default | -36.51% | -30.43% | 2.18% |
| ui | prune / default | 2.44% | -0.83% | 0.98% |
| ui | bounds-prune / bounds | 1.35% | 8.49% | 2.05% |
| ui | bounds-prune / default | 0.23% | -6.07% | 4.18% |

BP passes the predeclared relative guard against D, but that does not make it
acceptable: the absolute target still fails, and BP/B shows no useful
incremental improvement. Bounds alone is the better measured candidate in
this batch, but shell p99 still misses the target and it is not a promoted
default.

### Actual path and memory accounting

- Every completed shell run used 1000 attempts: 999 sparse, 1 full, 0 recovery.
  B/BP applied native tile bounds for all 999 sparse submissions, with the same
  summed 799536 native bounding-box tiles; D/P applied none.
- P/BP selected an opaque-prune shader for all 999 shell sparse submissions and
  all 1000 UI sparse submissions. D/B selected it zero times. This is shader
  selection evidence, **not** a count of tiles or commands actually pruned.
- UI applied no path tile bounds in any mode. B active-bin submissions were
  83/82/72; BP were 70/81/76. Animation-driven adaptive activity may vary.
- Quad-skip selections and its counters were zero in every invocation.
- Every UI run ended with the same tracked known-GPU snapshot: 62282124 bytes.
  It is not a peak GPU-memory measurement. No new production GPU buffer,
  binding or copy was introduced by opaque prune.
- Peak process RSS increased with BP versus B by 1.32% shell and 2.05% UI.
  RSS is host process memory, not physical GPU VRAM or bus traffic.
- Full native shell output still requires 33177600 bytes per destination
  buffer. Damage-only destination copying is not correct without buffer-age
  tracking for rotating DRM buffers.

### Every invocation

All raw logs and exact commands are retained alongside
`comparison-plan.json` and `comparison-results.json`.

| Run | Samples | Mean ms | p99 ms | Worst ms | Peak RSS KiB | Result |
|---|---|---|---|---|---|---|
| shell-default-round-1 | incomplete | — | — | — | 551952 | startup fence timeout |
| shell-bounds-round-1 | 1000 | 24.054 | 59.170 | 513.602 | 603072 | FAIL |
| shell-prune-round-1 | 1000 | 39.200 | 103.681 | 444.193 | 609960 | FAIL |
| shell-bounds-prune-round-1 | 1000 | 24.161 | 70.913 | 499.727 | 618936 | FAIL |
| ui-default-round-1 | 1000 | 7.864 | 33.407 | 83.732 | 378908 | FAIL |
| ui-bounds-round-1 | 1000 | 7.777 | 28.801 | 136.503 | 385824 | PASS |
| ui-prune-round-1 | 1000 | 8.056 | 32.989 | 153.609 | 381940 | FAIL |
| ui-bounds-prune-round-1 | 1000 | 8.159 | 32.337 | 71.185 | 393336 | FAIL |
| ui-bounds-prune-round-2 | 1000 | 7.753 | 26.813 | 150.608 | 393980 | PASS |
| ui-prune-round-2 | 1000 | 7.675 | 25.595 | 65.420 | 381872 | PASS |
| ui-bounds-round-2 | 1000 | 7.690 | 26.686 | 116.626 | 386508 | PASS |
| ui-default-round-2 | 1000 | 7.820 | 33.264 | 70.424 | 377560 | FAIL |
| shell-bounds-prune-round-2 | 1000 | 23.316 | 56.434 | 466.054 | 572660 | FAIL |
| shell-prune-round-2 | 1000 | 37.064 | 92.010 | 588.257 | 554860 | FAIL |
| shell-bounds-round-2 | 1000 | 23.830 | 60.057 | 557.076 | 562504 | FAIL |
| shell-default-round-2 | 1000 | 36.399 | 94.514 | 438.755 | 561068 | FAIL |
| shell-bounds-round-3 | 1000 | 23.168 | 60.772 | 510.982 | 565208 | FAIL |
| shell-default-round-3 | 1000 | 39.161 | 89.589 | 554.261 | 559844 | FAIL |
| shell-bounds-prune-round-3 | 1000 | 23.985 | 64.042 | 598.517 | 571212 | FAIL |
| shell-prune-round-3 | 1000 | 38.887 | 98.058 | 628.573 | 563984 | FAIL |
| ui-bounds-round-3 | 1000 | 7.949 | 35.583 | 71.758 | 386064 | FAIL |
| ui-default-round-3 | 1000 | 7.878 | 32.604 | 49.322 | 378168 | FAIL |
| ui-bounds-prune-round-3 | 1000 | 7.882 | 31.245 | 65.358 | 394164 | FAIL |
| ui-prune-round-3 | 1000 | 8.592 | 40.081 | 135.435 | 381156 | FAIL |

## Instrumented follow-up

Two additional bounds / bounds-prune stage-timing invocations were declared
after the entire unprofiled batch. They reuse the same preserved shell binary,
native dimensions and 1000 actions, with the same process and fence caps.
Their timings are diagnostic, never acceptance evidence. The benchmark
intentionally returns exit 1 when diagnostic gates are disabled; completion
is determined from samples and timestamp validity, not exit code alone.
Results are recorded separately in the diagnostic logs and metadata.

Both diagnostic invocations completed all 1000 actions and matched all 1000
timestamp submissions, with zero missing/stale/invalid/map-failed/overflowed
samples. Both returned the intentional `DisabledDiagnostic` exit1, not a
fence/runtime failure. Only 999 samples use sparse fine/coarse; the one ordinary
full render remains included in workload totals.

| Stage | B mean ms | BP mean ms | B p99 ms | BP p99 ms |
|---|---|---|---|---|
| fine_sparse_area | 12.389 | 12.895 | 36.468 | 40.771 |
| draw_leaf_prefix | 1.658 | 1.682 | 22.793 | 28.811 |
| coarse_sparse_active | 1.593 | 1.543 | 10.841 | 7.659 |
| path_count_direct | 1.285 | 1.390 | 5.793 | 7.207 |
| path_tiling_direct | 1.068 | 1.145 | 5.629 | 6.512 |

These two instrumented runs do not establish a causal speed difference;
they show no obvious sparse fine-stage reduction from this candidate on the
measured workload. Fine remains the largest measured steady-state stage.
CPU copy/swizzle means were 2.104/2.106 ms; native destination output remained
33177600 bytes/sample. Staging payload means were 808636/807591 bytes/sample,
not total physical GPU-bus traffic. Diagnostic end-to-end means/p99 were
33.856/93.105 ms (B) and 34.479/93.334 ms (BP); do not substitute these for the
unprofiled results above.

All source/configuration hashes remained unchanged through validation,
release build and the 24 unprofiled comparisons. The diagnostic source and
binary are the same. This turn made implementation/validation/measurement
progress and ruled out default opaque-prune enablement. The performance goal
remains active; no user files were staged, committed, pushed or merged.

The next read-only audit is in `next-blur-cache-audit.md`. Blur-invariant
caching is deferred pending command-distribution telemetry on actual native
damage; fine-stage dominance alone does not establish blur cost.
