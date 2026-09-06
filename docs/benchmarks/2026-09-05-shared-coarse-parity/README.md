# Shared-coarse fine-rendering isolation

Goal remains ordinary release defaults, 1,000 real native-3840×2160 redraws,
UI p99 ≤30 ms, shell mean ≤25 ms and p99 ≤50 ms. This is a correctness
diagnostic required before timing/promoting the adaptive coarse candidate,
not a smaller substitute for those performance targets.

The previous turn made progress: adaptive selection and actual-path accounting
were implemented, and native shell testing exposed intermittent one-level byte
differences under both default dense coarse and forced-active coarse. On the
captured default failure, the mismatching pixel was inside updated damage; two
fresh dense renders matched the original dense oracle exactly. That rules out
an AUTO-only explanation but does not isolate atomic segment ordering versus
fine shader arithmetic. All earlier failures remain in the adaptive-coarse logs.

## Isolation contract

An explicitly called hidden diagnostic encodes one ordinary dense coarse/path
pipeline, followed by full Area fine and sparse Area fine against the exact same
immutable config, segments, PTCL, info, gradient and atlas resources. Outputs
and mutable blending scratch are distinct. Shared resources live through both
fine dispatches. No normal rendering path, shader arithmetic, antialiasing,
workgroup setting, benchmark action, fence or acceptance threshold changes.

Only pixels in the requested exact damage rectangles are compared. Sparse
pixels outside those rectangles are not meaningful comparison evidence. Both
native output copies finish and bump overflow settles before comparing; retain
the existing one-second map fence and four-attempt recovery bound. Diagnostic
allocations and additional GPU/CPU copies are explicit and never performance
acceptance measurements.

An ignored test, invoked explicitly, will drive the same 13 native shell
candidates and named click sequence as the existing oracle. Missing capability
is a failure when this test is invoked, not a successful skip. Candidates with
no bounded damage use a full-native rectangle. The existing independent
retained-versus-dense oracle stays intact. On its original mismatch it will
also invoke this shared-coarse diagnostic for that exact candidate scene and
still fail its original zero-difference assertion.

## Predeclared first checks

1. Renderer unit/recording tests: one dense coarse, one full fine, one sparse
   fine; identical immutable binding IDs; distinct output/blend IDs; shared
   resources freed only after both fine passes.
2. Explicit shared-coarse native-4K test twice with renderer overrides cleared.
   Keep every outcome. Neither run is performance acceptance.
3. The unchanged independent shell oracle in default, forced-active, AUTO order,
   with the new failure-only diagnostic enabled through the test itself. Keep
   its original exact comparison and every failure.

A mismatch between fine outputs from one shared geometry snapshot rules out
different segment ordering *between those two fine passes* as its cause. A
shared-pair pass alongside an independent-render failure instead points toward
earlier-stage, recording or retained-state differences; it is not by itself
proof of atomic ordering. Do not claim a fix from an intermittent repeat pass.

Before these edits, test-inclusive changed-line/compiler lint reported zero
diagnostics. Unrelated security/OS edits in this shared worktree are out of
scope and remain untouched.

## Recording validation

The initial renderer test build found Debug-dependent assertions on resource
IDs and one unnecessary qualification. After fixing those test/source issues,
all 55 renderer tests passed. The pair test now checks actual image IDs (not
vacuous `None` buffer comparisons), one dense coarse dispatch, ordered full and
sparse fine dispatches, shared immutable bindings, distinct writable resources,
and immediate/deferred bump lifetimes. Both the failed build and passing run
are retained here.

## First native checks and diagnostic lifecycle correction

Both first paired native runs failed before comparing pixels: wgpu rejected
`Buffer::buffer_unmap` because the primary staging buffer had not been mapped.
A separate backtrace run confirms the diagnostic's unconditional pre-render
unmap was the call site. Terra removed that single call; the existing mapped
success, partial-map failure and overflow-retry cleanup is unchanged. The
13-candidate pair test starts with a fresh bridge and then reuses it, covering
the failing lifecycle directly. All three failed raw logs are retained.

The first fixed-order independent native oracle controls (default, forced
active, AUTO) each passed their 13 scenes, reporting 11 sparse and two full
attempts. Those passes do not erase the earlier intermittent pixel failures
and do not validate the paired diagnostic. Two corrected paired runs are the
next predeclared check. No adaptive candidate timing or promotion has occurred.

Both corrected paired runs passed all 13 native candidates exactly, including
full-frame damage and compact sparse dispatch. This provides no evidence for
changing the fine shader permutation; it does not prove that upstream segment
ordering causes the intermittent independent-render mismatch.

The next fixed controls use the already-built test executable (SHA-256
`c0a3015cb7059426d029fd7c97b1629d5446dd794b9f42980d2246e72162a2a7`):
two rounds of the independent oracle in default / forced-active / AUTO order.
Preserve all six results rather than retrying until a pass.

An additional explicitly ignored dense-only diagnostic captures each of the
same 13 candidate scenes once, then performs eight ordinary full renders on
each of two independent bridges without changing the scene between repeats.
It requires exact native bytes both within each context and between contexts,
reports the first failing candidate/repetition/comparison, and remains outside
normal rendering and benchmark paths. Run it with renderer overrides cleared.
Its purpose is to test the stability of the independent reference itself, not
to replace or weaken the original sparse/full oracle.

All six post-pair independent controls passed, so none invoked the failure-only
paired diagnostic. The dense-only test also passed all 13 scenes: 208 ordinary
full renders, eight per scene in each of two contexts, with exact within-context
and cross-context comparisons. These observations do not demonstrate dense
instability or an atomic-ordering cause. The diagnostic rectangle policy test
passed, and strict affected-library/example Clippy passed. No runtime shader
or performance default has changed.

## Fixed animation-pose diagnostic

The next probe will sweep quick-settings openness through 0, 25, 50, ..., 1000
permille (41 fixed poses). Each pose uses a fresh FrameBuilder, one closed
baseline scene, then a real quick-settings-open state and one captured candidate
scene. Capture reuses a constant zero-duration `Transition`; no extra per-frame
field or draw-path branch is required. Normal rendering and the 1,000-action
performance benchmark do not call this diagnostic hook.

For each captured scene, two full/sparse pairs share identical coarse resources
within each pair. Compare exact bytes only inside the actual candidate damage
regions, using full-native damage only if no bounded regions are supplied.
Use the existing independent-output scratch vector to retain the first pair's
full result, then compare the second pair's full result against it before
overwriting that scratch with the independent dense render. Also compare the
full output against that independent dense render of the same scene, labeling
each comparison separately. Stop at the first discrepancy and preserve it;
do not retry until a pass. This covers known, replayable subpixel transforms
without changing the production animation, action count, target size or pixel
tolerance. It is correctness evidence only, not performance acceptance.

The four capture-transition unit tests passed. The first fixed-pose sweep
stopped at 225 permille (22.5% openness): both full/sparse pairs matched
exactly, and both pair-full outputs matched each other. The second pair's
full output differed from the independent ordinary dense output by one byte,
maximum delta 1, at `(3770, 95, channel 0, pair-full 49, independent 48)`.
This is a full-fine versus full-fine difference across independent coarse/
context runs, not evidence for changing sparse fine shader arithmetic.
Neither atomic ordering nor a resolver/resource-context cause is yet proven.

All damage in this first sweep was full-native. That still validates these
full-output comparisons, but does not establish compact-dispatch pose coverage.
The next fixture revision will paint two closed baselines before opening QS,
and will require actual compact-dispatch coverage before a successful sweep
can pass; it will not fabricate narrower damage. A separate explicitly ignored
test will capture only the failing 225-permille pose and compare eight solely
ordinary dense renders in each of two contexts. No paired or sparse API may be
called in that dense-only probe. Preserve its first failure; no tolerance change.

The first dense-only 225-permille run failed at the first cross-context
comparison: two differing bytes, maximum delta 1, first again
`(3770, 95, channel 0, first-context 49, second-context 48)`. The second fixed
run passed all eight repeats per context. This reproduces the discrepancy
without any sparse or paired API calls; the pass does not erase it.

The two-baseline sweep later failed at 325 permille, this time between full
outputs from repeated coarse builds in the same renderer context: two bytes,
maximum delta 1, first `(3783, 95, channel 0, second-full 54, first-full 55)`.
Each shared-coarse full/sparse comparison still matched exactly. Every visited
pose still requested full-native damage, so compact pose coverage remains
unproven and its new success assertion must stay in place.

Next compare exact resolved CPU scene uploads, including availability and first
different byte, before attributing the variation to GPU geometry. Inspection
found that `Renderer.packed_scene` is recycled/emptied staging, not the retained
submitted payload; the diagnostic accessor instead borrows the most recent
`Recording::Command::Upload` named `vello.scene`. Accessors and test-only copies
add no work to normal rendering. Equal scene-upload bytes alone do not prove
equality of separate configuration, atlas/gradient resources, or GPU-generated
geometry.

The first build of this input logger failed because matched optional byte
values shadowed the input slice names in a length-reporting branch. Terra
renamed those bindings; the failed build log is retained. No GPU test ran in
that failed build. Repeat the two fixed dense-only controls and the pose sweep
with the corrected logger, then strict affected-code and test-inclusive
changed-line lint checks. No timing or default-promotion decision follows from
an intermittent pixel-test pass.

### Possible next GPU snapshot, not implemented

If differing outputs have identical CPU scene uploads, a bounded diagnostic
could capture native tile PTCL and only its referenced segments while retaining
the exact coarse resources. The two observed pixels fall in native tiles 1435
and 1436 at 240 tiles per row. Their initial 64-word PTCL blocks begin at byte
offsets 367,360 and 367,616. The first word is blend storage metadata; command
parsing starts after it. `CMD_FILL` supplies segment offset/count/rule/backdrop;
`CMD_JUMP` supplies an absolute word address and must be followed only under an
explicit bounded-chain policy, otherwise reporting an incomplete capture.

Segments have 24-byte stride, not 32. Compare the five meaningful f32 bit fields
(20 bytes), excluding padding, first in order and then as a multiset. Do not
index the path-local Tile buffer using a native framebuffer tile index. Existing
debug-layer capture does not include PTCL/segments, so buffer lifetime, bounded
range readback, cleanup and unsupported-capture handling need an explicit design
before implementation. This is a diagnostic proposal, not an established cause
or an authorization to change AA arithmetic or sort production geometry.

## CPU-input comparison results

The corrected input diagnostics reproduced three failures with **exactly equal,
nonempty 187,976-byte submitted CPU scene streams**:

| Probe | Comparison / location | Pixel difference |
| --- | --- | --- |
| Fixed 225, run 1 | Same second context, repeat 1; `(3770,95,channel 0)` | 1 byte, delta 1; current 48 / first 49 |
| Fixed 225, run 2 | Same first context, repeat 3; `(3770,95,channel 0)` | 1 byte, delta 1; current 48 / first 49 |
| Pose sweep, 125 | Repeated pair-full outputs; `(3771,95,channel 0)` | 2 bytes, maximum delta 1; current 42 / previous 43 |

Each full/sparse pair in the sweep continued to match exactly. This moves the
investigation beyond differing resolved CPU scene bytes, without establishing
whether GPU-generated segment order, other geometry, configuration or bound
image resources cause the output difference. The failures are preserved, and
neither the exact comparisons nor production AA arithmetic has been relaxed.
The pose sweep still used full-native damage before failing; compact pose
coverage is not established.

Strict affected-library/example Clippy passed after the logger correction.
Test-inclusive Clippy also completed with zero changed-line/compiler diagnostics
across the nine performance-owned files; unrelated baseline warnings are filtered,
not claimed fixed. Workspace/vendor formatting and diff-whitespace checks passed.
The exact-pixel failures above remain failing evidence, not a clean correctness
claim. All validation processes are terminal at this checkpoint.

No new adaptive release binary, performance result, default promotion, commit
or push has been made. The user goal remains active and unmet. The next useful
isolation is GPU-generated geometry/bound-resource comparison from the failing
full renders, with bounded diagnostic copies and native coordinates preserved.
Unrelated security, OS and workflow edits remain untouched.

## Same-render GPU geometry capture

The next bounded diagnostic copies the PTCL and segment buffers associated with
the actual paired fine outputs, in the same command submission. It does not
rerun coarse work to obtain a substitute snapshot. It also retains the exact
CPU-uploaded uniform configuration; that uniform buffer is not a GPU readback.
Ordinary painting and the existing capture-free paired diagnostic must make no
additional geometry copies.

Use whole-buffer downloads through the existing recording mechanism, with a
checked 64 MiB cap covering both logical buffer lengths plus configuration.
The native initial PTCL reservation is 49,766,400 bytes and the segment
reservation is 6,220,800 bytes, so a 32 MiB cap would reject this workload.
Pool allocations can be larger than their logical payload: map/copy only the
declared payload lengths. Oversize, missing, malformed, or incomplete capture
must be explicit failure/unavailability, never evidence of equality. Preserve
the ordinary one-second mapping fence and four-attempt overflow recovery.

Predeclared checks, outside performance acceptance:

1. Recording tests check exact buffer identities, both fine dispatches before
   downloads, downloads before frees, and zero geometry downloads for the
   existing paired API. Pure cap tests cover boundaries and integer overflow.
2. Parser tests cover the 24-byte segment stride, ignored padding, bounded PTCL
   jump chains, cycles, truncated commands, and invalid segment ranges.
3. Run the fixed-225-permille native-4K scene twice in separate diagnostic
   invocations, with renderer overrides cleared. Each invocation permits eight
   same-context render repetitions and preserves its first exact pixel failure.
   Record config/CPU-input equality and ordered versus multiset segment equality
   for native tiles 1435 and 1436 from the two frames being compared.
4. Recheck the existing shared-coarse oracle and strict affected-source lint.

PTCL physical allocation offsets and jump locations are not logical geometry.
Compare logical command arguments, fill rules/backdrops, and the five meaningful
u32 segment fields. Equal segment multisets with different order would narrow
the remaining explanation, but would not by itself prove that ordering caused
the pixel difference. Gradient/atlas resources and indirect info data are not
captured by this probe. No sorting or antialiasing changes are authorized by
the diagnostic, and no extra readback time counts toward acceptance results.

### Implemented capture and observed results

The bounded capture is implemented, including owned download handles, cleanup,
and pre-submit abort recovery for resolver/atlas metadata. The native probe
copies 49,766,400 PTCL bytes plus 6,220,800 segment bytes and retains the 88-byte
uploaded configuration (55,987,288 bytes total). It confirms a nonempty
187,976-byte CPU scene upload. Parsing reads PTCL in place and retains only the
two decoded tiles as the baseline, not a second complete raw geometry copy.

Both predeclared native invocations completed; neither was a performance run:

| Probe | Pixel result | Geometry evidence |
| --- | --- | --- |
| `gpu-geometry-native-1.log` | All eight full/sparse pairs and repeated full outputs exact | Several recorded fills changed segment order while retaining identical segment multisets |
| `gpu-geometry-native-2.log` | Repeat 1 fails full/full: two bytes, maximum delta 1; first `(3770,95,channel 0,current49,baseline48)` | Tile 1435, logical command 29: 49 segments, identical rule/backdrop and meaningful segment multiset, different order |

The failed invocation's full/sparse pair still matched exactly. Its submitted
CPU scene was identical. Configuration first differs at byte 68, inside
`tiles_size`, an allocation-capacity field; the fine shader's directly read
configuration fields are earlier and unchanged. Only the first differing fill
per tile is expanded in these logs: they do not establish equality of every
other command/resource, nor capture gradients, atlas pixels, or indirect info.
The known pixel discrepancy remains a failing exact assertion, not a fixed bug.

As supporting analysis, a CPU emulation that rounds each arithmetic operation
to f32, using the two captured 49-segment orders and the shader's native local
pixel group, gives coverage at `(3770,95)` of `0.6837971210479736` versus
`0.6837971806526184` (bits 1060048212 versus 1060048213). The values and adjacent
pixels are saved in `gpu-geometry-cpu-area-evidence.json`. This is **not** a GPU
replay or a reconstruction of final pixel blending; fused arithmetic and other
resources remain caveats. It narrows the investigation to order-sensitive
accumulation without authorizing production sorting or changed AA arithmetic.

Final validation: 57 renderer tests passed; both parser tests passed (including
the adjacent-allocation zero regression); the existing 13-scene shared-coarse
control passed, including compact damage. Strict affected-library/example
Clippy passed. Test-inclusive changed-line/compiler lint reported zero
diagnostics across all nine performance-owned files, with unrelated baseline
warnings still filtered rather than claimed fixed. All these processes are
terminal. The failed native run remains preserved alongside the passing run.

No new release performance trial, default promotion, commit, or push occurred.
The absolute user targets remain unmet. The next correctness step is to compare
all logical command arguments and all fill multisets for the failing captures,
then isolate the final-pixel effect of ordering if needed. The native integer
tile-bbox restriction opportunity is recorded in the adaptive-coarse notes;
it is not implemented and has no speedup claim.

## Follow-up: complete tile comparison (2026-09-06)

The follow-up logs are preserved in
`docs/benchmarks/2026-09-06-native-tile-bounds/geometry-all-commands-native-{1,2}.log`.
One run passed all repetitions; the second reproduced the ordinary full-render
two-byte/one-level variation at (3770,95), channel 0. The expanded comparison
now checks every decoded command and fill, not just the first differing fill:
tile 1435 had 33 commands/five fills and tile 1436 had 35 commands/six fills.
Both tiles retained equal command metadata and complete fill segment multisets.
Only tile 1435 changed segment order in the failing capture. Packed CPU scene
bytes were equal; configuration first differed at byte 68. Existing zero-error
pixel assertions remain unchanged. This narrows the evidence but does not prove
GPU final-pixel causality or resolve nondeterminism.
