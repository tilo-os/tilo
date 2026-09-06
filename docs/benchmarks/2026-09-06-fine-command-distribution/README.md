# Native fine-command distribution diagnostic

## Goal and previous-turn classification

Goal unchanged: ordinary release defaults, native3840x2160,1000 real actions;
UI p99<=30ms, shell mean<=25ms and p99<=50ms. Verified goal success precedes
polish, commit and push; user merges. This telemetry cannot itself satisfy
those latency requirements.

Previous turn made progress: exact opaque-prune implementation was validated
and all24 planned unprofiled invocations finished (23 completed samples), plus
2 valid instrumented diagnostic runs. Opaque pruning provided no incremental
win versus adaptive native bounds. Bounds shell medianmean/p99=23.830/60.057ms;
UI=7.777/28.801ms, still offdefault and shellp99 abovegoal. The full failure and
results are preserved in ../2026-09-06-opaque-tile-prune/README.md.

The next blur-cache audit is deferred: dominant fine-stage time does not prove
blur is responsible. Establish actual command distribution before adding a
GPU cache or changing arithmetic.

## Predeclared implementation scope

Terra edits only cfg(test) helpers/tests in shell/examples/redraw_bench.rs.
No production renderer, shader, ABI, buffer, copy, readback or timed benchmark
change. Existing exact pixel assertions remain unchanged. All other source
owners remain frozen; freeze this owner before any build/test.

A new explicitly ignored diagnostic uses the existing shared-coarse geometry
capture and native PTCL parser. Capture sequentially: initial native frame,
six real benchmark target clicks with normal dispatch/relayout/damage, then
fixed quick-settings poses0,225,500,1000. These are representative scenes,
not the same wall-clock animation trajectory as the1000-action benchmark.
No latency or speedup claim follows from counts alone.

For each scene report original draw-tag counts (including blur), damage mode,
capture byte sizes and unique native16x16 tiles intersecting clipped nonempty
damage. None damage means full; an explicit empty list means zero. Deduplicate
overlap. Count only reachable commands for those tiles, not all screen tiles
unless actual damage is full. Parse/drop one tile at a time.

Report selected-tile count, command tag histogram (excluding END/JUMP control
records), total reachable commands, blur commands, tileswithblur, maximum
blurcommands/tile, unique blur info offsets, fillcommands and segment counts.
Tile coverage must agree with returned sparse capture stats where applicable.
Requested full/sparse pixels from that same capture remain byte-exact.
Geometry and destination readbacks use existing bounded diagnostic paths;
no additional production readback is introduced.

## Validation/execution protocol

1. Review source and pure tests for native clipping/union, overflow/zero/edge
   bounds and histogram aggregation. Capture frozen source/config hashes.
2. Run shell example non-diagnostic tests (existing five plus the new explicit
   diagnostic stay ignored). Run strict example lint, changed-line/compiler
   test-inclusive lint, formatting and diff checks; preserve all failures.
3. Invoke the new ignored diagnostic alone, single-threaded, with all inherited
   renderer/benchmark flags cleared. Keep existing one-second fence, four
   render attempts and64MiB geometry cap. Process cap15minutes plus30s TERM
   grace. No compiler/test/benchmark overlaps the diagnostic execution.
4. Save exact command/output/exit, immutable hashes and parsed per-case data.
   Preserve failure and incomplete cases, never substitute zeros or silently
   replace a failed run. Counts are measured work distribution, not timings.
5. Rank the next optimization from this evidence. Only add a blur cache if
   repeated blur work plausibly outweighs extra storage/bandwidth and exactness
   risk; otherwise investigate the actually dominant command types.

No commit, push, merge or goal completion is authorized by telemetry passing.
Those remain conditional on the complete latency goal and final polish.

## First diagnostic results

All22 non-ignored shell example tests passed;6 explicit diagnostic tests were
ignored by that command. Strict example lint and test-inclusive changed-line/
compiler lint passed (zero diagnostics); formatting/diff checks passed.
Only cfg(test) shell example code changed: the production prefix hash is
6bfeda6a11e7659e7ded7aaeca56ed612dece0a6493840cdaa14b37b0b34a02b.
All other14-source-manifest entries stayed unchanged.

The explicitly enabled diagnostic completed11 captures with exact same-capture
requested full/sparse pixels and matching selected tile counts. Test exit0,
17.23seconds for the test as a whole; that elapsed time is not a latency
benchmark. Each geometry download was55987288bytes, below the existing64MiB
cap, and was not retained by ordinary rendering. See native-command-distribution.log,
command-distribution.json and validation-inputs.sha256 for the exact command,
output, parsed fields and frozen inputs.

| Scene | Damage | Tiles | Commands | Segment references | Blur commands | Clip begin+end |
|---|---|---|---|---|---|---|
| framework_initial | full | 32400 | 949207 | 97946 | 0 | 376330 |
| framework_click_1_topbar_tray_pill | full | 32400 | 964276 | 109708 | 196 | 380568 |
| framework_click_2_topbar_overview_button | provided | 792 | 32131 | 12126 | 182 | 11006 |
| framework_click_3_topbar_tray_pill | provided | 792 | 31643 | 11941 | 196 | 11022 |
| framework_click_4_topbar_overview_button | provided | 792 | 32131 | 12127 | 182 | 11006 |
| framework_click_5_topbar_tray_pill | provided | 792 | 31643 | 11940 | 196 | 11022 |
| framework_click_6_topbar_overview_button | provided | 792 | 32123 | 12082 | 182 | 11006 |
| fixed_quick_settings_pose_0 | full | 32400 | 963380 | 109525 | 196 | 380568 |
| fixed_quick_settings_pose_225 | full | 32400 | 964183 | 109570 | 216 | 380602 |
| fixed_quick_settings_pose_500 | full | 32400 | 964205 | 109860 | 198 | 380574 |
| fixed_quick_settings_pose_1000 | full | 32400 | 962857 | 109768 | 210 | 379106 |

The five sparse action scenes each selected792native tiles. They contained
31643..32131 commands, including9302..9442 SOLID area-setup commands,
4557..4875 colors,3764..3887 fills and5503..5511 clip entries plus the same
number of exits. Blur appeared182..196times, one blur draw/unique info offset,
and no more than once per tile. Segment references were11940..12127.

These are ordinary shared-coarse fine-command counts for representative scenes,
not a replay of the timing trajectory or an isolated stage-time attribution.
An expensive command can matter despite a small count. The data justify
examining general interpreter/clip/area-setup overhead before assuming a blur
cache is the best change. Fixed-pose helpers currently report full damage;
those full32400-tile counts must not be mixed with sparse792-tile counts.

## Follow-up telemetry refinement (declared before execution)

Keep the first invocation and source manifest unchanged. A second version of
the same cfg(test)-only diagnostic will add clip-depth/end-mode counts,
root/clip-local opaque-prune dry-run reductions, conservative CPU segment-row
mask potential, and SOLID/next-paint adjacency counts. These predictions are
not actual production pruning/masks, GPU exactness proof or speedup results.
Use separate v2 validation/input/diagnostic logs. Preserve the same11 scenes,
clipping semantics, caps and exact pixel assertions. Choose a production
prototype only after this evidence and a source-level correctness review.

## Follow-up results and next candidate

The v2 compile attempt failed with E0282 (the clip stack needed an explicit
Vec<ClipFrame> type). Its command, failure log and source manifest remain
preserved; it did not execute a GPU capture. V3 fixes that type and improves
the omitted-row check to independently mirror the fine shader's f32 dy and
y-edge arithmetic, including explicit signed-zero handling. These remain CPU
checks, not a GPU rounding or row-mask correctness proof.

V3 validation:26 passed,0 failed,6 explicitly ignored example tests, including
the existing native-4K exact pixel oracle; strict example lint and test-inclusive
changed-line/compiler lint passed with zero diagnostics. Formatting and diff
checks passed. The v3 source manifest matched immediately before capture.
All11 explicit v3 captures passed exact same-capture requested pixels and
selected-tile agreement; test exit0,19.33seconds total test duration (not a
performance measurement). Each geometry capture remained55987288bytes.

| Sparse action scene | Commands | Fusable SOLID pairs | Command reduction potential | Root prune removed | Clip-local prune prediction | Potential clear segment rows |
|---|---|---|---|---|---|---|
| framework_click_2_topbar_overview_button | 31973 | 9410 | 29.43% | 0 | 4027 (12.60%) | 60.77% |
| framework_click_3_topbar_tray_pill | 31643 | 9302 | 29.40% | 0 | 3911 (12.36%) | 59.72% |
| framework_click_4_topbar_overview_button | 32131 | 9427 | 29.34% | 0 | 3947 (12.28%) | 60.52% |
| framework_click_5_topbar_tray_pill | 31643 | 9302 | 29.40% | 0 | 3911 (12.36%) | 59.72% |
| framework_click_6_topbar_overview_button | 32131 | 9427 | 29.34% | 0 | 3947 (12.28%) | 60.52% |

Every reachable SOLID in all11 captures had an eligible immediate successor;
none were unmatched. All captured tiles had maximum clip depth2. For sparse
scenes most SOLID successors were END_CLIP (5167..5196), so fusion must include
clip-end coverage setup, not only color/gradient painting. Root-only pruning
removed no commands in those sparse scenes. Clip-local pruning predicted only
about12.3..12.6% reduction and carries ancestor-background/NaN/register risks.
The segment-row mask predicts about60% clear rows, but needs a separate robust
GPU exactness and producer-cost proof.

Choose paired SOLID-command fusion next: about29% fewer interpreter commands
without deleting paint or changing arithmetic, allocating buffers or adding
copies. This is only work-reduction potential; extra flag decoding can still
make the implementation slower. It will remain opt-in until exact pixels and
repeated native-4K ordinary-release latency measurements justify promotion.
See solid-command-fusion-audit.md for the reviewed contract. V3's second click
has31973 commands versus32131 in v1, consistent with wall-clock animation
trajectory being representative rather than a deterministic benchmark replay.
No latency result, default change, commit, push or goal completion follows
from this diagnostic.
