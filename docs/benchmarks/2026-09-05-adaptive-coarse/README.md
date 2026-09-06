# Adaptive coarse-selection investigation

The updated user objective is native 3840×2160, 1,000 actual redraws,
ordinary release settings, UI p99 ≤30 ms and shell mean ≤25 ms / p99 ≤50 ms.
The user changed the shell limits from 8/30 ms on this turn; historical logs
retain their original limits and failure labels.
The prior goal turn made progress through a tested, committed candidate and
measurements that reject enabling it globally. Production UI now has three
passing p99 observations, but production shell still fails both gates.

`1acdb59f8` remains the baseline. Its forced active-bin option reduces shell
mean by 24.72% while increasing UI p99 by 16.22%. Before changing selection,
run two fixed preserved-UI diagnostics: default coarse, then forced active
bins. Both retain 1,000 native-4K redraws and all animations, enable GPU
timestamps plus dispatch timestamps, and clear all acceptance budgets. Inspect
coarse-prefix coverage and stage cost; instrumented results are diagnostic,
not acceptance. No compiler, test or other benchmark overlaps these runs.
Use the unchanged one-second fence and 15-minute process cap / 30-second grace.

Preserved UI executable: `ui-perf-active-native-coarse-bins-20260905`, SHA-256
`6e24e4c70434e9b1c08e9da698b6e51b5c1a650c3b5060377f52da191afeb0a4`.

A possible next selector must depend on generic measured damage/bin coverage,
not workload names. When sparse coarse is not worthwhile, its fallback should
be ordinary dense coarse, not the already-regressing legacy sparse path.
No new heuristic or default promotion is authorized by source intuition alone;
actual-path accounting, exact-pixel tests and fresh release trials are required.
This is an intermediate step toward retaining unchanged scene work. Passing
a comparative promotion criterion does not by itself complete the user goal.

## Preserved-binary diagnosis

Both fixed UI diagnostics finished with 1,000 redraws, zero skipped frames,
518 widgets, zero CPU readback, and 1,000 matched dispatch-timestamp samples.
Default coarse averaged 668,070 prefix pixels (versus the shell's full native
8,294,400-pixel target). Its coarse stage averaged 1.453035 ms. Forced active
bins averaged 761,538 prefix pixels and 1.277137 ms in the active coarse stage.
Animations depend on elapsed time, so independent runs do not have identical
damage coverage. Whole-run means were 19.846 / 20.983 ms and p99 values
60.065 / 81.117 ms, respectively. These heavily instrumented measurements
are diagnostics, not acceptance or a comparison with unprofiled 8 ms UI runs.

## Candidate and predeclared trial protocol

The first candidate is opt-in through `VELLO_SPARSE_COARSE_AUTO=1`, with the
forced coarse flags absent. Default rendering is unchanged. Adaptive selection
uses active coarse only after successful bounded selector packing, when selected
native bins are at most half the total and at least 32 native bins can be omitted.
Otherwise it uses ordinary dense coarse, never legacy sparse coarse. Targets
with at most 32 native bins skip selector packing entirely. No benchmark-name,
action, resolution, animation, quality, fence, or sampling exceptions are added.

Before candidate timing, require pure policy/recording tests, exact GPU pixel
oracles for adaptive dense and active choices, unchanged forced-active and
overflow tests, native shell oracle tests, formatting and affected-code lint
checks. Build UI and shell together with the repository's ordinary locked
release settings, preserve both executables, and record their SHA-256 hashes.

Fixed execution order, declared before candidate timing:

1. Two adaptive diagnostics: shell then UI, both with pass and dispatch GPU
   timestamps. Report actual adaptive/active/dense-fallback counts and selected
   versus native bins. Diagnostic results cannot qualify acceptance.
2. Three unprofiled rounds, shell then UI in each round. Within each workload,
   use default then adaptive in rounds 1 and 3, adaptive then default in round 2.
   Compare the same new executables, clearing renderer and workload overrides
   before adding only the candidate switch and explicit benchmark settings.
3. Repeat the preserved `1acdb59f8` shell and UI defaults once, in that order,
   as unchanged-binary drift controls. The old shell executable still reports
   its old 8/30 ms gates; also assess its raw values against the new 25/50 limits.

All runs retain 1,000 native-4K actions/redraws, all samples and outliers, the
one-second renderer fence, and the 15-minute process cap with 30-second grace.
No compilation, test, or other benchmark may overlap a timed run. Record every
terminal outcome, peak process RSS, raw timing, and available copy accounting.

This geometry-adaptive candidate has a different, explicitly predeclared
promotion test from the previous globally forced candidate: exact correctness;
shell median mean at least 10% lower without a median-p99 regression; all three
UI p99 observations at or below 30 ms; UI median mean regression at most 3% and
median-p99 regression at most 5%. Report any memory increase. These comparisons
permit a shell improvement with effectively unchanged UI; they do not weaken
the user's absolute limits. An opt-in result never qualifies as default-build
acceptance. Promotion would require a source change, a fresh ordinary release
build, and new unflagged acceptance runs for both workloads.

## Validation checkpoint: correctness investigation before timing

The 54 renderer tests passed. Both adaptive GPU exact-pixel tests passed,
including timestamp proof of dense stage 17 for the small fixture and active
stage 28 for the larger native-edge fixture. Forced-active edge/overflow tests,
legacy resource parity, and four UI benchmark accounting tests also passed.
The old resource test skips when only AUTO is set; that log is not parity proof.
The extra no-default-feature checks did not compile: workspace feature selection
rejects the excluded package, while its standalone manifest requires its own
updated lockfile. No dependency or lockfile change was made for these checks.

The first adaptive native-4K shell oracle failed after candidate 2: three byte
differences, maximum channel delta 1, first at `(1894, 14, channel 2)`.
Predeclared serial follow-up controls produced:

| Native shell oracle control | Outcome |
| --- | --- |
| Default dense coarse | Pass, all 13 scenes |
| Forced active bins | Fail, candidate 2; two bytes, maximum delta 1; first `(3742, 287, channel 1)` |
| Unchanged AUTO repeat | Pass, all 13 scenes |

These passes do not erase the failures. Active-path exactness is not reliably
established; no candidate release timings or promotion are allowed at this
checkpoint. Failure-only diagnostics will compare the original retained and
oracle pixels with repeated dense renders of the very same candidate scene.
They must preserve the original failure and exact zero-difference requirement.

Next correctness-only order, declared before execution: two rounds of the
failure-diagnostic oracle, default / forced-active / AUTO then AUTO /
forced-active / default. Keep all six outcomes, including any intermittent
success. Extra dense renders occur only after the original mismatch and cannot
replace its comparison or make that test pass.

All six failure-diagnostic repetitions passed all 13 scenes. None triggered the
extra same-scene dense repeats, so these runs establish no new evidence about
dense determinism and do not explain or erase the two earlier failures. The
atomic-segment/floating-point accumulation explanation remains a hypothesis
supported by code inspection, not an experimentally isolated cause. Candidate
timing and default promotion remain on hold.

After removing a duplicate successful-path diagnostic byte scan, the complete
AUTO shell test target passed 14/14 tests. The following complete default target
failed its native oracle after candidate 4, with one byte difference at
`(3747, 282, channel 0, retained 223, dense 222)`. Actual returned stats showed
dense coarse (`coarse_active_bins=false`, `coarse_adaptive=false`), 792 fine
tiles and 193,536 written pixels. The difference was inside the changed
quick-settings rectangle `(3496, 32, 336, 576)`, not outside retained damage.

All three additional comparisons had zero differences:
`freshRetainedDense/originalDense`, `repeatedOracle/originalDense`, and
`freshRetainedDense/repeatedOracle`. Thus the failure is not confined to AUTO
or active-bin coarse, and these particular dense repeats were deterministic.
This does not yet distinguish upstream atomic segment ordering from arithmetic
differences in the sparse/full fine shader permutations. A useful next
diagnostic would compare both fine passes against the exact same coarse and
segment buffers, without rerunning upstream geometry.

Strict affected-code lint initially found one new function-length violation in
the bridge's duplicated dense-recovery branches. Refactoring those branches
back into one shared fallback preserves submission stats and error behavior;
the initial failing lint log is retained. Formatting and whitespace checks
passed. The shell limits alone were updated to the user's explicit 25/50 ms
request; none of the native resolution, actions, sample counts, original
pixel comparisons, renderer fences, or production rendering defaults changed.

After the shared-fallback refactor, strict affected-library/example Clippy
passed, as did both formatting checks and `git diff --check`. Test-inclusive
changed-line lint and further correctness isolation remain follow-up work.
No adaptive release build, performance trial, default promotion, commit, or
push was made in this checkpoint. Unrelated security/OS worktree changes were
left untouched. The latest performance matrix therefore remains the committed
`1acdb59f8` measurements, not an estimate of the unbenchmarked adaptive code.

## Next allocation-work candidate: native tile bounding-box restriction

Read-only shader inspection identifies a narrower implementation than rebuilding
the complete native grid with local indexing: an optional sparse tile-allocation
permutation could intersect each path's integer tile bounding box with the
bounding union of requested damage tiles. Native viewport/configuration,
flattened floating-point coordinates, bin IDs, PTCL tile IDs and fine pixel
coordinates would remain unchanged. This is a proposal, not implemented or
measured performance evidence.

`tile_alloc.wgsl` currently intersects only with the full native viewport and
zeros all allocated path tiles. The actual selected backdrop shader is
`backdrop_dyn.wgsl`, which iterates those path-local bounding-box rows and
widths. `path_count.wgsl` already clips lines in tile-index space to each path
bounding box and explicitly accumulates left-of-box winding into its first
column. `path_tiling.wgsl` derives native tile origins from the original line
and subsegment index; it need not translate the scene. Coarse uses the resulting
path bbox/stride to enumerate tiles within native bins.

Important proof obligations before any promotion: preserve winding from edges
left of the restricted box, clip begin/end behavior, zero-area paths, disjoint
damage, viewport-edge partial tiles, negative/offscreen geometry, overflow and
fallback behavior. Bounds must only restrict work after a successful sparse
selection; the ordinary dense fallback must receive its original full path
allocations. Narrowing tile storage does not by itself reduce the native PTCL
structural reservation or establish a memory saving. Segment allocation order
may change and needs the ongoing exact-geometry/pixel investigation. No shader
change or benchmark claim has been made for this proposal.
