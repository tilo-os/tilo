# Native tile-bounds performance candidate

The goal remains 1,000 real redraws/actions at native 3840×2160 with ordinary
release build defaults: UI p99 ≤30 ms; shell mean ≤25 ms and p99 ≤50 ms.
The user wants final polish, commit and push only after verified success, and
will merge personally. Nothing in this candidate establishes success yet.

## Candidate contract

`VELLO_SPARSE_TILE_BOUNDS=1` is an exact, off-by-default experiment. After an
active sparse-coarse selection, compute the bounding union of its
requested native damage tiles. A separate tile-allocation permutation clamps
each path's integer tile bbox to that interval. A bounded, ordered interval
covering at most half the native tile grid is required; otherwise retain the
ordinary allocator. Dense, legacy-coarse, CPU, unsupported and fallback paths
must retain their original complete path allocations.

The comparison uses adaptive selection and its profitability thresholds.
Explicitly forced active-coarse mode may also apply bounds; the small edge and
overflow fixtures use that mode to exercise the shader independently of the
adaptive cutoff.

Keep native floating-point geometry, viewport configuration, bin IDs, PTCL IDs,
tile-origin calculations, fine coordinates and exact pixel masks unchanged.
Only the selected path uploads its 16-byte bounds uniform. No allocation/copy
reduction is claimed merely from bbox area: per-path overlaps, allocation
floors, pool retention and actual peak memory must still be measured.

The prior shared-geometry investigation reproduced ordinary full-render pixel
variation with identical CPU inputs. A captured 49-segment fill has identical
segment bits in a different order; stepwise CPU f32 analysis changes coverage
at the failing pixel by one ULP. That does not establish final-pixel causality.
The complete command/segment comparison is being strengthened separately.
Neither existing exact pixel checks nor AA arithmetic will be relaxed.

## Validation and decision

1. Pure selection/environment tests and recording tests must prove actual
   selection, the bounds upload/binding/lifetime, and untouched fallback paths.
2. Exercise native-coordinate winding/clip/edge cases, including geometry left
   of the restricted bbox, even-odd fills, nested clips, negative/offscreen
   geometry, disjoint damage, partial edge tiles, zero area and overflow recovery.
   Preserve every original exact-check failure and investigate candidate-specific
   differences before promotion.
3. Build both benchmark executables using the repository's ordinary `--release`
   settings, with no optimization-profile overrides. Preserve executable hashes.
4. Compare three modes from the same binaries: default (D), adaptive coarse (A),
   and adaptive coarse plus tile bounds (R). Run three unprofiled rounds per
   workload, rotating mode order D/A/R, A/R/D, R/D/A. Alternate workload order
   across rounds. Retain all 18 results, including failed gates and outliers.
5. Keep native size, 1,000 actions, animations, one-second map fence and bounded
   recovery unchanged. No build/test may overlap a timed run. Retain the existing
   15-minute process cap and 30-second termination grace. Clear all inherited
   renderer/benchmark overrides, including the new bounds flag, before setting
   a mode's explicit flags. Profiled runs are separate diagnostic evidence.
6. Report mean, p99, worst sample, whole-process peak RSS, tracked GPU bytes and
   actual applied-path counters. Bbox tiles are not allocated path tiles, and
   API copy payloads are not physical GPU-bus or display-transfer measurements.

Comparative timing is allowed after the candidate's bounded correctness checks
while the independently reproduced baseline nondeterminism remains tracked;
it is not default acceptance or a clean correctness claim. New candidate-only
pixel errors block promotion. A useful candidate must improve shell median mean
by at least 10% against its contemporaneous default control, not regress shell
p99, and not regress UI median mean/p99 by more than 3%/5%. Report any memory
increase explicitly. These relative checks do not replace the absolute goal.

Even a promising flagged comparison requires source-default promotion, a new
ordinary release build, and fresh unflagged acceptance runs meeting all absolute
targets. Physical 2010-laptop/KMS/constant-60-FPS qualification is not provided by
the current software-Vulkan host. All new code and results here are intermediate
until those requirements are actually verified.

## Initial validation (2026-09-06)

The first vendor test build exposed a test-only `ResourceId` formatting
requirement; `vendor-tests-compile-1.log` preserves that failure. The corrected
suite passed all 65 tests. This includes actual GPU bounds application at native
3840×2160 (negative geometry, nested NonZero/EvenOdd clips and two disjoint
damage tiles) and the 529×257 partial native edge. The references settle any
bump overflow within four attempts. The partial-edge check compares the changed
pixel exactly and separately checks untouched output.

Shell example tests passed 19 tests (five explicit diagnostics remain ignored;
the native action oracle is run separately). UI example tests passed eight,
and scalar submission-accounting reset/fallback coverage passed. Both adaptive
UI GPU comparisons and the forced-active edge/clip/translucency comparison
passed with tile bounds requested. The forced-active shell overflow fixture
really overflowed and recovered to exact dense output. The default and candidate
native-4K shell action oracles both passed, each reporting 11 sparse and two full
attempts for its 13-frame correctness sequence. These are not 1,000-action
performance runs. Strict production/example clippy passed.

The two expanded fixed-225 geometry captures produced one pass and one baseline
repeatability failure, preserved without changing assertions. In the failure,
the full/shared-coarse baseline differed by two bytes, maximum delta one, first
at (3770,95,channel 0), 48 versus 49. CPU packed scene bytes were identical;
configuration first differed at byte 68. All 33 commands/five fills in tile 1435
and all 35 commands/six fills in tile 1436 had equal command metadata and equal
complete fill segment multisets. Tile 1435 changed segment order; tile 1436 did
not. This strengthens the ordering evidence but is not a controlled GPU proof
of final-pixel causality or a fix for baseline nondeterminism.

Normal, unprofiled benchmark output now includes actual sparse, active, bounds-
applied and native-bbox-area counts. UI accounting uses scalar counters without
clocks, extra GPU copies or per-frame allocations and counts submitted work
before overflow recovery. Requested/available headers do not substitute for
these actual counts. Bbox area still does not measure allocated memory.

## Unprofiled comparison results

All 18 planned invocations are retained. The first default shell invocation
failed its unchanged one-second fence during initial desktop paint, before any
1,000-action statistics existed. It is an incomplete run, not a zero or a
replacement sample. The shell default medians below therefore describe only
two completed runs; they are **not a three-successful-run default baseline**.
Every other invocation completed all 1,000 required events/actions. No outlier
or failed gate was removed. Medians summarize per-run values, not pooled p99s.

| Workload | Mode | Completed/attempted | Median mean ms | Median p99 ms | Worst frame ms | Median peak RSS MiB | Passing runs |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| shell | default | 2/3 | 38.779 | 89.668 | 577.086 | 538.3 | 0/3 |
| shell | adaptive | 3/3 | 29.869 | 88.352 | 539.190 | 538.3 | 0/3 |
| shell | bounds | 3/3 | 24.914 | 64.664 | 531.229 | 545.7 | 0/3 |
| ui | default | 3/3 | 8.136 | 29.822 | 79.499 | 369.2 | 2/3 |
| ui | adaptive | 3/3 | 7.765 | 30.865 | 77.481 | 376.6 | 1/3 |
| ui | bounds | 3/3 | 7.868 | 32.620 | 98.573 | 377.2 | 1/3 |

Bounds versus adaptive-only provides the complete three-versus-three shell
comparison: mean latency fell 16.59%,
p99 fell 26.81%, and median peak RSS
**rose 1.39%**. Against the two
completed default controls, the combined candidate's median mean is
35.75% lower, with the missing-control
limitation above. This is useful experimental evidence, not goal success.

All three bounds shell runs applied the new allocator to all 999 sparse
submissions (one full submission, zero recovery), each reporting 807,864 summed
native bbox tiles. The three bounds UI runs applied it **zero times**; adaptive
coarse was active on only 66–97 of their 1,000 sparse submissions. Their UI
results cannot demonstrate a bounds-allocation speedup. End-of-run tracked UI
GPU allocation was 62,282,124 bytes in every mode, with zero CPU readback;
this is a tracked snapshot, not a physical GPU memory/bus measurement.

The candidate stays off by default. Shell never met both absolute limits in
one run; its means were 24.429, 24.914 and 25.144 ms, and p99 values were
77.978, 61.640 and 64.664 ms. UI bounds-mode median p99 was
9.38% above default, exceeding the predeclared
5% regression limit; only one of its three runs met 30 ms. Default UI itself
passed only two of three. No promotion, final acceptance, goal completion,
commit or push follows from this batch.

### Every invocation

| Run | Mean ms | p99 ms | Worst ms | Peak RSS MiB | Bounds applied | Gate |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| shell-default-round-1 | — (setup timeout) | — | — | 505.4 | — | INCOMPLETE |
| shell-adaptive-round-1 | 28.527 | 71.178 | 539.190 | 538.5 | 0 | FAIL |
| shell-bounds-round-1 | 24.429 | 77.978 | 440.965 | 566.6 | 999 | FAIL |
| ui-default-round-1 | 8.182 | 41.774 | 79.499 | 367.6 | 0 | FAIL |
| ui-adaptive-round-1 | 7.851 | 39.111 | 75.308 | 376.6 | 0 | FAIL |
| ui-bounds-round-1 | 7.576 | 20.815 | 82.822 | 377.3 | 0 | PASS |
| ui-adaptive-round-2 | 7.765 | 30.865 | 77.481 | 376.7 | 0 | FAIL |
| ui-bounds-round-2 | 7.868 | 32.620 | 74.885 | 377.2 | 0 | FAIL |
| ui-default-round-2 | 8.136 | 29.822 | 78.771 | 369.3 | 0 | PASS |
| shell-adaptive-round-2 | 30.289 | 88.352 | 404.122 | 538.3 | 0 | FAIL |
| shell-bounds-round-2 | 24.914 | 61.640 | 500.923 | 545.7 | 999 | FAIL |
| shell-default-round-2 | 39.112 | 82.366 | 577.086 | 535.2 | 0 | FAIL |
| shell-bounds-round-3 | 25.144 | 64.664 | 531.229 | 545.3 | 999 | FAIL |
| shell-default-round-3 | 38.445 | 96.969 | 565.910 | 541.4 | 0 | FAIL |
| shell-adaptive-round-3 | 29.869 | 91.572 | 531.454 | 537.6 | 0 | FAIL |
| ui-bounds-round-3 | 8.021 | 33.979 | 98.573 | 376.8 | 0 | FAIL |
| ui-default-round-3 | 7.857 | 26.366 | 78.818 | 369.2 | 0 | PASS |
| ui-adaptive-round-3 | 7.638 | 22.602 | 73.291 | 376.2 | 0 | PASS |

## Separate dispatch diagnostics

The three planned diagnostic runs completed 1,000 actions each, matching
1,000/1,000 timestamp samples with no missing, stale, failed, overflowed or
recovery submissions. Gates were explicitly DisabledDiagnostic; their nonzero
exit status is expected and cannot be treated as an acceptance pass.

| GPU stage | Adaptive mean ms | Bounds mean ms | Bounds p99 ms |
| --- | ---: | ---: | ---: |
| tile_alloc | 2.050 | 0.867 | 3.973 |
| backdrop_dyn | 2.805 | 1.053 | 6.149 |
| path_count_direct | 2.645 | 1.360 | 5.390 |
| path_tiling_direct | 1.737 | 1.247 | 10.296 |
| fine_sparse_area | 13.570 | 13.058 | 39.711 |

The bounds run also averaged 2.048 ms for the CPU XRGB copy and wrote 33,177,600
CPU destination bytes per action. GPU texture-to-staging payload averaged
807,591 bytes/action, versus 808,636 in the other two diagnostics. Instrumentation
changes elapsed animation timing and the observed damage union was 799,536 bbox
tiles, versus 807,864 in each unprofiled bounds run; these are not identical-pose
stage experiments. Fine sparse rasterization remains the largest measured
stage. Individual stage p99s cannot be summed to reconstruct total p99.

The diagnostic full-frame means/p99s were default 49.637/122.114 ms, adaptive
45.143/130.654 ms and bounds 35.026/96.660 ms. Do not mix those values with the
unprofiled matrix or cite them as default-release success. Physical GPU traffic,
KMS presentation and a 2010 laptop remain unmeasured.

A read-only follow-up is inspecting whether sparse Area-AA invocations whose
four output pixels are all unrequested can skip fine work safely, including
uniformity/shared-memory constraints. No such source change or speedup is
claimed here. The candidate remains off by default; goal targets are unmet.

The read-only audit found that this early return is feasible only for analytic
Area-AA: each invocation owns four adjacent pixels, whose mask nibble never
crosses a u32 boundary. Area fine rendering has no workgroup barriers/shared
state; blend spill is addressed per invocation/pixel. MSAA has workgroup
barriers and must not take a per-invocation early return. Existing counters do
not determine the count of zero nibbles, so exact skipped-quad instrumentation
and pixel/clip/overflow tests are prerequisites to a new candidate. No source
for that follow-up has been changed, and no speedup is assumed.
