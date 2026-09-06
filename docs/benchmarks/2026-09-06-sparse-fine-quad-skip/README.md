# Sparse Area-AA quad-skip candidate

Goal unchanged: ordinary release defaults, native 3840×2160 and 1,000 real
actions/redraws. UI p99 ≤30 ms; shell mean ≤25 ms and p99 ≤50 ms. Final polish,
commit and push follow verified success; the user merges.

The preceding turn made concrete progress: native tile bounds reduced shell
median mean versus adaptive-only from 29.869 to 24.914 ms, but p99 remained
64.664 ms. UI median p99 regressed beyond the predeclared guard and no default
promotion occurred. All 18 invocations, including one default-shell setup fence
timeout, are preserved in the preceding native-tile-bounds report. Separate
instrumentation identifies sparse fine rasterization at 13.058 ms mean as the
largest remaining stage; instrumented timings are not acceptance measurements.

## Implementation contract

Exact opt-in `VELLO_SPARSE_FINE_QUAD_SKIP=1`, off by default. A distinct
`fine_sparse_area_quad_skip` permutation may return before its command
interpreter if the four native pixels owned by that invocation all have zero
damage mask bits. It uses the existing nine-word sparse tile records, native
coordinates and dispatch grid; no new uniform, storage record or GPU copy is
required. Keep the uniform prior-stage failure check before this branch.

This is analytic Area-AA only. MSAA has workgroup barriers, so it must never
take the divergent early return. Do not reorder segments, change AA arithmetic,
translate geometry, alter clipping, skip requested pixels or relax any exact
assertion. Preserve ordinary dense, CPU, unsupported, shared-coarse diagnostic
and recovery paths. Deep blend-spill indexing must remain disjoint per pixel.

Returned applied-path stats must distinguish requested, available and actually
selected. Count total quad invocations and active nonzero-mask quads only when
the new shader is selected. Report evaluated native pixels correctly, clipping
partial edge groups to the native extent; the old full-tile raster metric must
not be presented as actual work after skipping. Counters are not allocated
memory or physical GPU transfer measurements. Use bounded integer counting
without extra per-frame allocations.

## Validation and measurement protocol

1. Pure tests cover exact opt-in, mask nibbles, word/tile boundaries, disjoint
   damage, native partial edges, disabled/unavailable paths and accounting.
   Recording checks prove shader selection and unchanged bindings/lifetimes.
2. Real GPU checks assert actual use and nonzero skipped quads, compare requested
   pixels exactly with dense reference and preserve untouched output. Exercise
   unaligned masks, nested/deep clips, blend spill and bounded overflow recovery.
   Keep the existing native-4K action oracle and its exact assertions. Preserve
   baseline nondeterminism separately; never erase failures by rerunning.
3. Freeze all source owners before compilation. Run strict production/example
   clippy, test-inclusive changed-line/compiler lint and formatting checks.
4. Build both examples with ordinary locked release settings and no profile
   overrides. Preserve source/build-configuration and executable hashes.
5. Compare four modes from the same binaries: default (D), adaptive+tile-bounds
   (B), quad-skip alone (Q), and adaptive+tile-bounds+quad-skip (C). Use three
   unprofiled rounds per workload, orders D/B/Q/C, C/Q/B/D, B/D/C/Q. Alternate
   shell-first, UI-first, shell-first workload order: 24 invocations total.
6. Each invocation keeps all 1,000 samples, native size, existing animations,
   one-second map fence, four-attempt render recovery and 15-minute process cap
   with 30-second termination grace. Clear every inherited renderer/benchmark
   override, including the new flag. No compiler, test or other benchmark may
   overlap a timed process. Preserve setup failures, timeouts and outliers.
7. Report all runs, mean/p99/worst, whole-process peak RSS, available tracked
   GPU snapshots, actual skipped-quad counts and actual selected submissions.
   Use per-run medians; incomplete controls are not silently replaced or treated
   as successful controls. Instrumented diagnostics are separate.

The combined candidate must retain the previous relative guard: shell median
mean at least 10% below contemporary default, no shell median-p99 regression,
and UI median mean/p99 no worse by more than 3%/5%. Compare Q with D and C with B
to isolate whether this new path adds value. Report all memory increases.
These exploratory comparisons do not establish default success: promotion
requires correctness plus all absolute goals, then a new default-enabled release
build and fresh unflagged acceptance runs. No 2010-laptop, physical-GPU-bus or
KMS/constant-60-FPS certification is implied by the software-Vulkan host.

The protocol above preceded all timing runs. The candidate is implemented by
Terra, remains off by default, and adds no GPU buffer, binding or transfer.
Root review corrected a compile-time endpoint check before compilation and
strengthened the GPU fixture so full and sparse fine passes share one immutable
coarse result. Five fractional translucent layers exercise deep blend storage;
unaligned disjoint damage is checked against full rendering, every untouched
pixel is checked, and requested output must contain a nontransparent pixel.

## Correctness validation

All 11 planned validation invocations passed (`validation-results.json` and raw
logs). Vello: 70 tests. Shell example: 19 tests, with 5 explicit diagnostic tests
ignored and the native oracle run separately. UI example: 8 tests. UI accounting:
1 test. Adaptive and edge GPU parity: 3 tests. Real sparse-overflow recovery: 1
test. Default, quad-only and combined native-4K action oracles each passed with
11 sparse and 2 full frames; these short exact-pixel checks are not the 1,000
action performance measurement. Strict production/example clippy passed.
Test-inclusive lint found 0 changed-line/compiler diagnostics. Formatting and
`git diff --check` passed. Existing separately documented baseline LSB
nondeterminism has not been erased or declared fixed.

Sources and build configuration were hashed before validation and verified
unchanged before starting the ordinary release build. No candidate speedup or
goal success is claimed until the planned unprofiled measurements finish.

## Unprofiled comparison result

All 24 predeclared invocations finished; 23 completed all 1,000 native-4K
actions. **The goal is not met and neither candidate is promoted to default.**
The combined candidate fails the UI relative p99 guard (+10.63% versus default,
limit +5%) as well as the absolute UI and shell p99 goals. All shell runs fail
their absolute gate; each UI mode passes only one of three runs. A faster shell
median is not default-build acceptance.

D = unchanged defaults, B = adaptive coarse plus native tile bounds,
Q = quad skip alone, C = adaptive coarse plus bounds plus quad skip.
All modes use the exact same preserved release binaries. Table values are
medians of completed per-run statistics; worst is the largest individual frame.
Peak RSS is whole-process high-water memory, including startup—not GPU memory.

| Workload | Mode | Complete/attempted | Mean ms | p99 ms | Worst ms | Median peak RSS MiB | Passing runs |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| shell | D | 3/3 | 37.238 | 81.406 | 601.529 | 544.8 | 0/3 |
| shell | B | 3/3 | 24.883 | 69.198 | 627.439 | 541.4 | 0/3 |
| shell | Q | 2/3 | 38.386 | 94.460 | 611.884 | 548.0 | 0/3 |
| shell | C | 3/3 | 24.003 | 62.126 | 586.560 | 553.6 | 0/3 |
| ui | D | 3/3 | 7.877 | 31.009 | 143.780 | 369.0 | 1/3 |
| ui | B | 3/3 | 7.803 | 31.711 | 175.905 | 377.0 | 1/3 |
| ui | Q | 3/3 | 8.516 | 40.594 | 170.397 | 378.2 | 1/3 |
| ui | C | 3/3 | 7.890 | 34.306 | 73.473 | 385.6 | 1/3 |

The Q shell medians describe **only two completed runs**. Its first invocation
failed at sample 1 with `wgpu buffer mapping failed: fence wait: Timeout` under
the unchanged one-second fence limit. The failed process peaked at 604,456 KiB
RSS and its entire log is retained; it is not replaced or entered as a zero-time
or successful run. No cause beyond the reported timeout is established.

### Changes from contemporary controls

Negative timing percentages mean less time. Positive RSS percentages mean
more memory, not a saving.

| Workload | Comparison | Mean change | p99 change | Peak RSS change |
| --- | --- | ---: | ---: | ---: |
| shell | C versus D | -35.54% | -23.68% | +1.61% |
| shell | C versus B | -3.54% | -10.22% | +2.26% |
| shell | Q versus D (2 versus 3 completions) | 3.08% | 16.04% | +0.58% |
| ui | C versus D | 0.17% | 10.63% | +4.50% |
| ui | C versus B | 1.11% | 8.18% | +2.30% |
| ui | Q versus D | 8.11% | 30.91% | +2.48% |

Combined versus bounds-only saves 3.54% shell mean and 10.22% shell p99 in this
batch, but adds 2.26% peak RSS. UI mean/p99 and RSS all regress against bounds-only.
Quad alone does not improve either workload's median timing against default.
The experiment therefore does not justify a general default enablement.

### Actual work and memory

- All completed shell runs have 999 sparse and 1 full submission, with no
  recovery submissions. B and C apply native tile bounds to all 999 sparse
  frames, each summing to 799,536 native bounding-box tiles over the workload.
- Each C shell run reports 50,709,888 quad groups, 48,397,872 active and
  2,312,016 skipped: **4.56% skipped**. Q's two completed shell runs skip 4.57%.
  These are invocation counts, not GPU memory or transfer savings.
- Q and C UI runs apply quad skip to all 1,000 sparse submissions, with median
  skipped fractions of 22.66% and 22.57% respectively. Native tile bounds apply
  to **zero UI submissions** in every mode. Adaptive active-bin use varies
  with the live workload and is recorded in every raw log.
- Every UI mode ends with the same **62,282,124 B tracked GPU snapshot**
  (59.4 MiB) and zero CPU readback in timing-only mode. This is not peak GPU
  memory, driver allocations or physical GPU-bus traffic. Shell peak GPU
  allocation/physical transfer measurements are unavailable in this harness.
- The new shader adds no storage buffer, binding or copy. It does not remove
  the shell's existing complete retained-image CPU swizzle into the selected
  output buffer; rotating-buffer correctness still requires that operation.

These are end-to-end live-action comparisons, not identical-pose per-stage
experiments. The observed timing variation and all outliers remain in the data.
The host adapter is Vulkan llvmpipe (LLVM 22.1.8, 128 bits), a CPU device.
No physical 2010-laptop or constant-60-FPS claim follows from these measurements.

### All invocations

| Run | Mean ms | p99 ms | Worst ms | Peak RSS MiB | Exit | Gate |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| shell-default-round-1 | 35.812 | 75.463 | 310.067 | 546.2 | 1 | FAIL |
| shell-bounds-round-1 | 23.552 | 59.094 | 489.111 | 541.3 | 1 | FAIL |
| shell-quad-round-1 | — | — | — | 590.3 | 1 | FENCE TIMEOUT at sample 1 |
| shell-combined-round-1 | 23.708 | 60.613 | 453.718 | 553.6 | 1 | FAIL |
| ui-default-round-1 | 8.238 | 34.610 | 143.780 | 369.0 | 2 | FAIL |
| ui-bounds-round-1 | 8.398 | 38.707 | 175.905 | 377.0 | 2 | FAIL |
| ui-quad-round-1 | 8.516 | 40.594 | 60.812 | 379.2 | 2 | FAIL |
| ui-combined-round-1 | 7.914 | 34.306 | 73.274 | 385.9 | 2 | FAIL |
| ui-combined-round-2 | 7.418 | 22.118 | 66.416 | 385.6 | 0 | PASS |
| ui-quad-round-2 | 7.696 | 28.060 | 170.397 | 377.7 | 0 | PASS |
| ui-bounds-round-2 | 7.689 | 26.728 | 77.091 | 376.3 | 0 | PASS |
| ui-default-round-2 | 7.708 | 29.351 | 51.658 | 368.9 | 0 | PASS |
| shell-combined-round-2 | 25.987 | 75.264 | 586.560 | 554.5 | 1 | FAIL |
| shell-quad-round-2 | 37.222 | 83.732 | 561.415 | 543.7 | 1 | FAIL |
| shell-bounds-round-2 | 24.883 | 69.198 | 612.304 | 548.1 | 1 | FAIL |
| shell-default-round-2 | 37.238 | 81.406 | 485.478 | 544.8 | 1 | FAIL |
| shell-bounds-round-3 | 25.090 | 81.431 | 627.439 | 541.4 | 1 | FAIL |
| shell-default-round-3 | 39.287 | 94.643 | 601.529 | 535.9 | 1 | FAIL |
| shell-combined-round-3 | 24.003 | 62.126 | 505.849 | 549.3 | 1 | FAIL |
| shell-quad-round-3 | 39.550 | 105.189 | 611.884 | 552.4 | 1 | FAIL |
| ui-bounds-round-3 | 7.803 | 31.711 | 76.405 | 377.0 | 2 | FAIL |
| ui-default-round-3 | 7.877 | 31.009 | 65.583 | 369.2 | 2 | FAIL |
| ui-combined-round-3 | 7.890 | 36.850 | 73.473 | 384.9 | 2 | FAIL |
| ui-quad-round-3 | 8.850 | 43.405 | 109.207 | 378.2 | 2 | FAIL |

Machine-readable summaries are in `comparison-results.json` (all invocations),
`comparison-summary.json` and `comparison-changes.json`. Source/configuration
hashes remained unchanged through the complete batch. No compiler, test or
other benchmark overlapped any timed process. No commit or push is claimed;
the user's requested polish, commit and push remain conditional on verified
goal success.
