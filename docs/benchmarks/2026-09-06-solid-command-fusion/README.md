# Sparse SOLID-command fusion candidate

Goal: ordinary release defaults, native3840x2160 and1000 real actions/redraws;
UI p99<=30ms; shell mean<=25ms andp99<=50ms. Success must be verified before
final polish, commit and push. The user merges.

## Evidence and scope

Expanded test-only command telemetry passed26 example tests, lint checks and
11 explicit native captures, after one preserved compile failure. Each capture
retained exact same-capture requested pixels. Every reachable SOLID command
had an eligible immediate paint/END_CLIP successor; sparse scenes showed
9302..9427 pairs among31643..32131commands, about29.3..29.4%. Root-only opaque
pruning removed zero commands in those sparse scenes. Clip-local dry-run
predicted12.3..12.6% removal; row masking predicted about60% clear segment rows,
but both have larger semantic/rounding risks. This preceding work is progress,
not a repeated blocker. See ../2026-09-06-fine-command-distribution/README.md.

Fusion removes one command word/interpreter iteration per eligible pair by
moving the exact four area=1.0 assignments into the following paint command.
It does not delete paint or change geometry, AA, floating arithmetic, clip state,
ancestor background, output coverage or the benchmark workload. Counts are
potential work reduction, not measured latency savings.

Exact opt-in VELLO_SPARSE_SOLID_FUSE=1 remains off by default. A private high
opcode bit pairs matching GPU dense-grid/legacy/active coarse variants with a
sparse Area fine decoder. write_path returns an explicit bool to the immediate
paint writer; no pending state may leak. Dynamic JUMP may move, but neither
JUMP nor BEGIN/END/FILL is tagged. END_CLIP is an eligible paint consumer.
Missing either partner keeps original shaders of the SAME mode/grid/bounds.
Full, CPU, recovery and shared-coarse diagnostics stay original. Requests for
quad-skip or opaque-prune conservatively disable fusion; no mixed permutations.

No new GPU buffers, bindings, copies or production readbacks. Actual selected
submission counts differ from requested/available and do not count fused words.
GPU PTCL tests must establish actual fused output. Paint work and the shell's
rotating-buffer destination copy are unchanged; no memory/copy saving is claimed
without measurements. Extra decode cost may negate command reduction.

## Predeclared validation and measurements

1. Terra owns source changes; root reviews and orchestrates. Freeze all affected
   owners before any build/test. Keep unrelated user work untouched.
2. Exact env/parser and recording tests cover paired selection, all coarse modes,
   bounds preservation, unavailable fine/coarse partners, incompatible feature
   precedence, default/CPU/full/shared isolation and unchanged resource lifetimes.
3. GPU tests cover actual flagged PTCL, shorter lists, dynamic JUMPs, unchanged
   segmented FILL, all paint types, deep clips including END_CLIP, disjoint and
   partial native edge damage, overflow and recovery. Dense/original-sparse/fused
   pixels must remain exact. No tolerance relaxation or geometry simplification.
4. Run vendor tests, shell/UI benchmark/accounting tests, native4K action oracles,
   existing edge/overflow tests, strict production/example lint, test-inclusive
   changed-line/compiler lint, format and diff checks. Retain all failures.
5. Build ordinary locked release options (no flags/profile/driver override).
   Hash frozen sources/configuration; preserve and hash both executable artifacts.
6. Same-binary modes: D=default, B=adaptive+tile-bounds, F=fusion alone,
   BF=adaptive+tile-bounds+fusion. Quad-skip and opaque-prune unset throughout.
   Three rounds D/B/F/BF, BF/F/B/D, B/D/BF/F; workload order shell/UI,
   UI/shell, shell/UI. All24 invocations are predeclared in comparison-plan.json.
7. Keep1000 native4K samples, real actions/animations, one-second map fence,
   four render attempts,15min process cap plus30s TERM grace. No compiler/test/
   competing benchmark overlaps measurements. Retain failed attempts and outliers.
8. Report all runs, medians of completed runs with completion denominators,
   mean/p99/worst, peak process RSS, available knownGPU snapshots and actual
   selected submission counts. Instrumented runs cannot pass acceptance.

Relative guard BF/contemporaryD: shell medianmean at least10%lower, no shell
medianp99 regression, UI mean/p99 no worse than3%/5%. Compare F/D and BF/B to
isolate the candidate, including memory increases. Relative improvement alone
is insufficient: all absolute goals must pass. Promotion requires a fresh
ordinary release build and fresh unflagged acceptance runs; experimental flags
cannot satisfy the default-build goal. The software-Vulkan CPU adapter does not
certify physical2010-laptop/KMS/physicalGPU-bus behavior or constant60FPS.

Implementation and exploratory measurements are complete; see the results below.
No default promotion, commit, push, merge or goal completion at this stage.

## Implemented validation state (before any release timing)

The paired SOLID-fuse production experiment and returned-stat accounting are
implemented, still off by default. Fifteen owned source files include the shared
conditional private opcode constant. The first vendor compile attempt failed
before GPU execution; vendor-tests.log retains all7 errors and1 warning.
Those issues were corrected without changing the experiment's render contract.

V2 ran all13 planned validation commands:12 passed and1 failed. Renderer tests
passed82/82, including actual GPU PTCL proof, all paint types, depth9 spill clips,
Multiply, partial/disjoint edges, pair fallback and CPU/default isolation.
Shell example25 tests and UI example8 passed; six shell diagnostics stayed
explicitly ignored and the native oracle ran separately. Accounting, adaptive
parity, native-edge and overflow/recovery tests passed. Strict production/example
and strict vendor-test lint passed; test-inclusive changed-line/compiler lint
reported0 diagnostics. Formatting/diff checks passed under the v2 source hashes.

| Native4K action oracle | V2 outcome | Fusion-selected sparse submissions |
|---|---|---|
| default | pass | 0/11 |
| adaptive + bounds | pass | 0/11 |
| adaptive + bounds + fusion | FAIL at candidate5 | selected=true on failed frame |
| fusion alone | pass | 11/11 |

The combined-mode failure had3 differing color bytes, maximumdelta1, first at
(1894,14,channel2,234vs233), outside damage(3496,32,336,576). The exact failure
remains in shell-fuse-native-oracle-v2.log and native-oracle-failure-v2.json.
The same-scene failure diagnostics reported:

- Shared-coarse full/sparse requested pixels:0 mismatches (inside damage only).
- Fresh retained-bridge dense/original dense:3 mismatches.
- Repeated oracle dense/original dense:3 mismatches.
- Fresh retained dense/repeated oracle dense:0 mismatches.

Two ordinary full renders on the oracle bridge therefore differed. This supports
full-render run/history variation, not a proven cause or a guarantee that the
original retained bytes were correct. Source audit found no obvious comparison,
buffer-length/pitch or scene-lifetime defect. Do not infer atomic/float ordering
from this log alone, or erase the failure as harmless.

A separately predeclared ordinary dense-repeatability diagnostic, with all
experimental flags cleared, passed13 scenes *8 repetitions *2 independent
contexts=208 full paints, exact same/cross-context pixels and packed-scene checks.
Its173.09second whole-test duration is NOT a latency result. It did not reproduce
or explain the earlier discrepancy; its log is ordinary-dense-repeatability-
diagnostic-v2.log.

V3 changes only cfg(test) failure reporting in the shell example. It borrows the
original retained CPU output without a33MiB snapshot, compares it with a fresh
same-bridge dense repaint, and snapshots/compares original/repeated oracle packed
scene bytes. Existing assertions, tolerances and production timing/rendering are
unchanged. V3 example tests(25 pass,6 ignored,1 separately filtered native oracle)
and strict example lint passed. The three predeclared combined-mode diagnostic
replays all passed with11/11 actual fusion-selected sparse submissions each.
They did not trigger the richer failure report, replace the original failure,
or qualify acceptance. See the separately named v3 replay logs and input hashes.

### Timing classification amendment, declared before execution

The original protocol required correctness validation before acceptance timing.
That acceptance gate remains closed by the unresolved native discrepancy.
To determine whether this experiment merits further engineering, the already
planned same-binary24-run release comparison may proceed strictly as exploratory
unprofiled measurement. Commands, modes, order,1000-action/native4K workload,
caps, outlier handling and quantitative budgets are unchanged; plan metadata now
marks every invocation acceptance_eligible=false. No such exploratory result,
even if its numeric latency budget passes, may justify default promotion, goal
completion, commit or push. The unresolved pixel failure must remain beside any
reported timing matrix, not be hidden or silently replaced.

## Completed exploratory comparison

The ordinary locked release build passed in 14m 08s. The unchanged v3 source
and configuration hashes were verified before and after the timing batch and
after the two subsequent stage diagnostics. Both executable copies are preserved
under target/ui-perf-comparison; see release-artifacts.sha256 and
release-artifact-sizes.txt. No old artifact was overwritten.

All 24 predeclared invocations terminated: 23 produced all 1,000 samples.
shell-fuse-round-1 failed at sample 1 with the unchanged one-second map-fence
timeout, exit 1, and no complete sample summary. Its peak RSS was 620,780 KiB.
It remains a failed attempt, was not replaced, and is excluded only from
completed-run latency/RSS medians. This result does not establish the timeout's
cause. The later F runs were already scheduled, not replacement attempts.

All results below remain exploratory, acceptance_eligible=false. The unresolved
native pixel discrepancy above remains a correctness blocker; no later pass
erases it. Every completed run retained all samples/outliers. Means and p99s
below are medians of the reported per-run statistics, not pooled quantiles.
Worst is the maximum over completed runs; RSS is median completed-run peak
process RSS, not GPU memory. Numeric passes are not acceptance passes.
The shell binary evaluates its mean gate using the exact total/count, not the
rounded display mean.

| Workload / mode | Completed | Mean ms | p99 ms | Worst ms | Peak RSS MiB | Numeric budget passes |
|---|---:|---:|---:|---:|---:|---:|
| shell default | 3/3 | 38.474 | 98.652 | 524.988 | 556.9 | 0/3 |
| shell bounds | 3/3 | 25.023 | 66.315 | 546.537 | 556.4 | 0/3 |
| shell fuse | 2/3 | 37.5115 | 96.059 | 513.533 | 566.5 | 0/3 |
| shell bounds-fuse | 3/3 | 24.207 | 65.163 | 478.538 | 577.7 | 0/3 |
| ui default | 3/3 | 8.259 | 26.660 | 81.073 | 369.0 | 2/3 |
| ui bounds | 3/3 | 8.331 | 38.328 | 106.910 | 377.3 | 1/3 |
| ui fuse | 3/3 | 8.590 | 38.155 | 110.007 | 382.3 | 0/3 |
| ui bounds-fuse | 3/3 | 8.028 | 38.385 | 142.794 | 392.1 | 0/3 |

D = default; B = adaptive coarse plus native tile bounds; F = SOLID fusion;
BF = both. None of these experiments was promoted to default.

Negative latency changes mean lower measured time; positive RSS means heavier.
These are descriptive medians from three attempts, not statistical confidence
intervals or a demonstrated hardware-independent speedup. F shell has only
two completed runs versus three default runs.

| Workload / comparison | Mean change | p99 change | Peak RSS change |
|---|---:|---:|---:|
| shell fuse/default | -2.50% | -2.63% | +1.73% |
| shell bounds-fuse/bounds | -3.26% | -1.74% | +3.84% |
| shell bounds-fuse/default | -37.08% | -33.95% | +3.74% |
| ui fuse/default | 4.01% | 43.12% | +3.62% |
| ui bounds-fuse/bounds | -3.64% | 0.15% | +3.92% |
| ui bounds-fuse/default | -2.80% | 43.98% | +6.26% |

BF passed the relative shell mean/p99 guard versus D but failed the UI p99
regression guard (+43.98%, above the allowed +5%). It also failed the absolute
shell p99 limit in all three runs; its third run missed the mean limit too.
Every fused UI run missed the 30 ms p99 limit. Even default UI passed only
two of three numeric gates. The default-build goal is not achieved.

Against B alone, fusion's shell mean reduction was only 3.26% and its median
p99 reduction only 1.74%, accompanied by 3.84% more peak RSS. Fusion alone
also had an incomplete run and worse UI mean/p99 than D. The large BF/D shell
difference therefore must not be attributed solely to fusion. This experiment
does not justify default promotion or additional command-fusion complexity
as the main route to the target.

### Accounting and evidence limits

Every completed shell run had 1,000 attempts: 999 sparse, one full, zero
recovery. F/BF selected fusion on all 999 sparse shell submissions; D/B never
selected it. B/BF applied bounds on all 999 sparse shell submissions. B's
native bbox sum was 799,536 each round; BF's was 807,864 / 799,536 / 807,864.
Real wall-clock animation and workload state remain unchanged.

All UI runs reported 1,000 sparse submissions; F/BF selected fusion 1,000 times
and D/B zero times. B's adaptive-active counts were 91 / 94 / 96; BF's were
92 / 94 / 74. No UI run actually applied path tile bounds. Quad skip and opaque
pruning selected zero submissions in every completed run.

All UI runs reported the same end-of-run known GPU allocation snapshot:
62,282,124 bytes, including the 33,177,600-byte native output. This is neither
peak VRAM nor a physical-GPU measurement. No allocation/copy saving is shown.
The larger process RSS includes costs outside that known-allocation snapshot.

Every shell run and both stage diagnostics explicitly reported Vulkan llvmpipe
LLVM 22.1.8, device_type=Cpu. **Protocol reporting gap:** this UI executable did
not log the selected adapter, despite the expectation in environment.md.
Its per-run adapter identity cannot be retroactively established from the shell
log. The results are retained with that missing provenance; a separate, untimed
adapter-reporting correction is required before future acceptance builds.
Nothing in this batch certifies a physical GPU, a 2010 laptop, KMS presentation,
scanout, or constant 60 FPS.

### Post-batch stage diagnostics

The two separately predeclared B/BF profiles ran only after all unprofiled
invocations terminated. Each had 1,000 matched timestamp records and zero
missing, stale, invalid, map-failed, overflowed or recovery records. Exit 1
was the intentional DisabledDiagnostic result, not a runtime failure.
Each used a 2,048-byte diagnostic timestamp buffer. These profiles cannot
pass latency acceptance and cannot be mixed into the table above.

| Stage / phase | B mean / p99 ms | BF mean / p99 ms |
|---|---:|---:|
| Sparse fine shading (999 samples) | 12.521185 / 33.329116 | 12.237258 / 34.815380 |
| Active sparse coarse (999 samples) | 1.492987 / 4.752857 | 1.439593 / 5.126611 |
| Draw leaf prefix (1,000 samples) | 1.448014 / 10.328795 | 1.524387 / 20.796550 |
| Direct path count (1,000 samples) | 1.295304 / 4.213842 | 1.215168 / 4.246030 |
| Direct path tiling (1,000 samples) | 1.126593 / 7.829970 | 1.117074 / 6.912461 |

Sparse fine mean decreased only about 2.27%; its p99 increased about 4.46%.
The approximately 29% SOLID command-count opportunity did not translate into
a comparable fine-stage latency reduction. Command counts are not time shares.

CPU copy/swizzle means were 2.038 ms and 2.101 ms. Each still wrote all
33,177,600 destination bytes per frame. Mean GPU readback payloads were
807,591 and 808,636 bytes per frame, reflecting damage workload variation,
not a fusion copy saving. Instrumented end-to-end mean/p99 was
32.712 / 82.009 ms and 32.843 / 85.765 ms; do not compare these as acceptance
timings. See dispatch-diagnostic-results.json for all stages and validity fields.

### Decision and next work

Keep fusion, opaque pruning, quad skip and native bounds off by default.
No goal completion, final polish, staging, commit, push or merge is authorized
by this batch. The user retains the merge.

The candidate evaluation made measurable progress and is not an external
blocker: it rules out command-count reduction as a sufficient improvement here.
UI adapter provenance is now repaired for future builds, as recorded below.
Next, evaluate the already audited conservative segment-row mask as a separate
opt-in candidate. It must prove omissions against
the original AA equations and actual GPU captured metadata/pixels; it must not
relax tolerances, reorder segments or simplify the scene. The unresolved native
full-render discrepancy also still needs causal resolution before promotion.

### Post-measurement adapter reporting (v4, not timed release results)

Terra changed only ui/src/offscreen.rs and ui/examples/dashboard_perf_bench.rs
after every v3 comparison/profile process had terminated. A doc-hidden accessor
snapshots metadata from the selected shared-context adapter without changing
selection or doing GPU work. The dashboard prints name, backend, device type
and driver before warm-up, or explicitly reports unavailable metadata.

All eight example tests, strict UI library/example clippy, scoped formatting
and diff checks passed. A separately labelled one-action native-4K debug smoke
printed exactly one header before warm-up: Vulkan llvmpipe LLVM 22.1.8, Cpu.
The smoke's duration, throughput and allocations are NOT performance results
and are excluded from every comparison above. It does not retroactively identify
the adapters of earlier UI processes.

Current frozen source/configuration hashes are validation-inputs-v4.sha256;
the preserved release executables and exploratory timings remain v3. The v3
binary hashes are unchanged. See validation-summary-v4.json and the four v4 logs.
No v4 release build or new acceptance batch was run. All compiler, test, smoke,
benchmark and diagnostic processes are terminal; nothing is staged or committed.
