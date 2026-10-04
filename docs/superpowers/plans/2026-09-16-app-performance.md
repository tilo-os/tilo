# Application p99 frame performance implementation plan

> **For agentic workers:** Use subagent-driven-development for independent bounded work and review.

**Goal:** Improve the shared application framework until measurable gains stall, then optimize individual apps until every GUI app sustains the 60fps p99 target under representative interaction workloads.

**Architecture:** Keep optimization and measurement in shared UI/runtime paths. Establish optimized baselines before changing scheduling or rendering. Retain raw frame outcomes and workload correctness evidence; CPU-only or offscreen-only timing is not evidence of displayed 60fps.

**Tech stack:** Rust, winit, wgpu/Vello, shared ui launch/headless/profiling, Python runners.

**Spec:** User objective in this task; initial acceptance contract below.

## Acceptance contract and constraints

- Scope is all workspace GUI apps, not just Photos and Word Processor. Inventory is generated from app entrypoints; headless agent CLI is recorded separately.
- Begin on available macOS hardware; platform release gates are being clarified. Do not claim unmeasured OS/hardware parity.
- 60Hz frame budget is 16,666,667ns. Require at least 1,000 measured active-workload frames per case after warmup and repeated runs; record every failed run and dropped/skipped frame. Do not pool idle rebuilds into active-workload percentiles.
- Validate real scroll, typing/editing, resize, animation and applicable app-specific large-document/media workflows with resulting state/pixels. Startup is reported separately.
- Measure main-thread work, render queue/worker work and GPU completion/cadence separately. Final acceptance needs presentation evidence, not sums of independent percentiles or successful action acknowledgements.
- Maintain behavior, image correctness, security checks, accessibility, input responsiveness and idle efficiency. No disabling features or shrinking fixture sizes to make a gate pass.
- Use optimized code for acceptance, pin executable hashes and workload settings, retain raw evidence and environmental metadata.
- Shared improvements precede app-specific optimizations. Stall means repeated controlled profiling shows remaining dominant costs are app-specific or proposed shared changes yield no repeatable improvement; record rejected experiments.

## Task 1: Reliable baseline and coverage inventory

- [x] Build existing production runtime proof and real apps with `cargo build --profile thin-app -p shell -p photos -p word-processor --no-default-features --example shared-runtime-proof --bin photos --bin word-processor`.
- [x] Extend opt-in worker evidence in `ui/src/render_thread.rs` to record every outcome, including skipped frames, without introducing fences or behavior changes.
- [x] Test parser/phase selection in `scripts/perf/runtime_benchmark_test.py`; extend `runtime_benchmark.py` to expose outcome counts and wall-time distributions instead of accepting cheap skipped-frame loops as smooth output.
- [x] Run `python3 scripts/shared-runtime-proof.py --benchmark --bin-dir target/thin-app`; retain successful and failed evidence.
- [x] Inventory every GUI entrypoint, reusable fixtures and unsupported cases in `docs/app-performance.md`. Missing coverage stays explicitly incomplete.

## Task 2: Shared scheduling and frame pipeline

- [ ] Investigate the 1440 Word Processor CPU builds versus43 successful worker presents in the existing dev run using new outcome evidence.
- [ ] If skipped/acquire retries cause hot redraw loops, add a failing scheduling regression test before changing retry policy. Preserve retry and first-present recovery, ensure background/occluded apps become bounded, and test restore visibility.
- [ ] Profile main-thread sync/layout/draw, worker acquire/encode/submit, queue backpressure and actual presentation cadence in optimized builds. Attribute long frames before choosing changes.
- [ ] For each measured shared bottleneck, write a regression reproduction, make one optimization, rerun correctness and identical workload samples, and record before/after. Revert experiments that do not help or break behavior.
- [ ] Continue this loop across representative widget families until evidence supports a shared-layer stall.

## Task 3: Full app workload coverage and app optimizations

- [ ] Extend shared live/headless input driving with per-app synthetic fixtures and independently asserted outcomes, using existing launch paths; no alternate app implementation.
- [ ] Build and run all GUI apps with applicable scroll/type/resize/animation cases; hardware/network-dependent cases need controlled providers and separate native verification.
- [ ] Fix dominant per-app costs in measured order after shared-layer work stalls. Initial Photos lead: synchronous pixel transformation in `PhotosApp::refresh`; retain output-equivalence tests and viewport/zoom semantics.
- [ ] Repeat full-app regression measurements after shared changes and affected cases after app-specific changes. Keep every app in the coverage matrix until it has passing evidence.

## Task 4: Final qualification

- [ ] Run repeated >=1000 active-frame cases on agreed platforms/hardware and verify p99 within budget with actual presentation cadence and workload outcomes.
- [ ] Review tests and benchmark validity independently; resolve blockers.
- [ ] Audit every app/case/platform against raw evidence. Missing or indirect evidence means goal remains active.

## Progress ledger

- Optimized baseline build started. Existing dev observations are diagnostic only.
- Read-only app inventory delegated; opt-in worker-outcome instrumentation delegated independently.
- Completed optimized diagnostic baseline: `target/runtime-proof/20260916-085258/`, zero skipped measured frames; Photos CPU p99 3.038ms, Word Processor10.925ms. No60fps qualification claimed.
- Inventory and coverage-only validator now explicitly track47GUIapps; all remain unqualified. Parser and coverage tests pass (14 at this checkpoint).
- Sampling diagnostic failed presentation gate and lacked symbols; retained failed evidence. Adding main-thread cost attribution before selecting resource/cache optimizations.
- Shared SDK queue wake passes 32 wake and 26 collaboration tests. Foreground optimized runs `20260916-091617` and `20260916-091727` both pass the signed proof and all actions with zero skipped frames. Action latency improves repeatedly; CPU frame cost is unchanged. Exact figures and confounders are in `docs/app-performance.md`.
- Main-thread attribution identifies Word Processor resource availability (run 1 median 3.210ms, p99 5.619ms), not scene-pool waiting, as the next investigation. The live root is unbound; deep-copying declared resources solely to collect allowed addresses is avoidable without changing generation keys.
- Borrowed-address experiment: failing missing-method regression observed, then 8 action-palette tests pass, including nested public actions below a denied parent, permission restoration, and unchanged handling of unavailable actions. Optimized runs `20260916-092514` and `20260916-092541` both pass correctness/presentation checks but show no repeatable frame gain; reverted the experiment and archived its source/test. Next split the resource-generation/projection timing envelope. This does not establish shared-layer stall.
- Resource-stage tracing now separates declaration generation, semantic construction, validation, permission projection, bound-resource work, control refresh and cache lookups. Opt-in only; reporter groups hits/misses separately. Eleven profiling tests and seventeen Python tests pass. Optimized run `20260916-093337` passes proof/actions with50Presented and zeroSkipped perapp. WP declaration generation median2.994ms/p995.229ms dominates; local permission projection median0.140ms/p990.237ms. See `docs/app-performance.md` for boundaries and complete results.
- Workload validity: reproduced `PerfStep::ClickNamed` returning a successful report for a missing target. It now fails with scenario/step/target context. A second regression confirms actual control activation. This closes one failure mode; per-app observable state/pixel checks remain required.
- Broader shared-widget diagnosis started: optimized `ui-perf` build and bounded correction of list-scroll workload. One-way scrolling beyond the bottom cannot count as1200activeframes; pointer targeting, direction reversal and independent movement assertions are required before accepting its timing samples. Gallery steady remains explicitly an idle CPU diagnostic.
- Verified list scrolling completed twice with1200moving CPU frames/run: `verified-scroll-20260916-100327` median0.640ms/p990.729ms; `verified-scroll-20260916-100458` median0.646ms/p990.956ms. Raw samples and executable hashes retained. No GPU/display coverage and no app qualified by these diagnostics.
- Added optional originating CPU frame ID to worker requests/logs, preserving independent worker counters and null IDs for auxiliary/unprofiled frames. Regression covers presented/skipped/reconfigured/surface-gone identity; parser preserves unmatched IDs and rejects duplicate/missing/invalid identities. Rust correlation test and20Python tests pass; independent review has no blocker. Live run `20260916-100852` matches all50CPUframes/app to50Presented outcomes with no unmatched or unprofiled frames. CPU cost regressed versus earlier runs (Photos p9910.710ms, WP17.016ms); repeatability unresolved, do not hide the over-budget run or claim a speedup. Three verified-scroll example tests pass.

## Native workload implementation notes

- Implemented first shared native timeline/input-frame provenance layer in
  `ui/src/native_scenario.rs` and `AppHandler`, with absolute deadlines, typed
  hover/click/line-wheel input, validation, first-worker-presentation startup,
  and explicit warmup/input/frame IDs. It does not certify effects or displays.
- Native button delivery probe `native-input-20260916-101949` independently
  records1260activations including60warmups, with1200measured input-associated
  CPU frames matched toPresented outcomes. Full stream preserves one startup
  skip and a warmup coalesced batch. Five Rust scheduler and24Python tests pass;
  independent review has no blocker after malformed-time/order hardening.
- Next extend typed native inputs and independent semantic/pixel checks, seed
  representative per-app fixtures, and add actual presentation feedback.
  Probe callback delivery is a foundation, not substitute app qualification.
  Usage and limitations: `docs/native-input-workloads.md`.

Correlation repeat `20260916-101012` matches all69Photos/63WP CPU samples to
Presented outcomes, with no unmatched/skipped frames. CPU p99 varies (Photos
6.100ms, WP14.963ms); extra frames are not associated with workload inputs.
No controlled speed claim follows. Next implementation priority is scheduled
native workload input plus observable effect checks and workload/frame IDs,
then native presentation feedback; all47apps remain unqualified.

Source review identifies `App::run_with_setup` / `AppHandler` as the shared native
entry point. Reuse existing `UserEvent` hover/click/wheel routing and factor the
synthetic keyboard-event builder from `headless.rs`; extend wheel handling to
pixel deltas/phases. Reuse `ux_bench` scenario and semantic-check vocabulary,
but do not pretend its current headless runner provides native presentation.
Screenshot scenario preparation is currently screenshot-only in `launch.rs`.

The native runner must use independent scheduled input deadlines, not delay the
next input until the previous frame finishes. Record lateness, coalescing,
workload phase and frame IDs through main/worker queues, then independently
assert state/pixels. Keep this separate from actual presentation feedback.

wgpu 29 `SurfaceTexture::present` schedules presentation and exposes no displayed
timestamp. The Metal HAL owns the drawable privately; native `MTLDrawable`
presented callbacks/times require a backend-edge extension to correlate with
frame IDs. GPU timestamp queries or submission completion callbacks alone do
not qualify display cadence. Any macOS implementation must be described as
OS-reported presentation, not physical scanout or input-to-photon latency.
- Native scroll effect probe added with six passing Rust tests and 25 Python
  tests, plus independent source review. Two optimized 200-row native scroll
  runs verify all 1260 wheel movements; 1200/1198 measured CPU frames match
  Presented outcomes. CPU p99 is 10.201/10.540 ms; the repeat retains a 31.285 ms
  tail and two coalesced batches. First-run scene-pool wait p99 is 9.972 ms.
  Investigate blocking primary scene-pool receive / redraw scheduling next.
  Evidence and the failed partial-log collector attempt are documented in
  `docs/native-input-workloads.md`. All 47 apps remain unqualified.
- Retained shared nonblocking buffer admission: atomic scene/damage pairs,
  cached reservation before state consumption, asynchronous window-specific
  completion events, and gating periodic redraw requests while deferred.
  Initial direct-worker redraw deadlocked on macOS; failed run and sampled
  stacks retained, fixed with EventLoopProxy delivery. Occlusion exposed a
  retry storm; all three window paths now apply a 16.667 ms failure backoff.
  Four render-thread tests and one retry test pass; reviews found no blocker.
- Final optimized native scroll repeats `110113` / `110140` verify 1260 moving
  steps and 1200 measured Presented frames each. Input p99 4.165 / 4.168 ms
  versus baseline 9.000 / 10.649 ms. CPU p99 0.199 / 0.163 ms partly reflects
  moving GPU waiting outside the sample. Worker completion p99 8.980 / 16.494 ms;
  no displayed-cadence claim. Static idle check passes. Intentional occlusion
  attempts were inconclusive; auxiliary retry/recovery still needs native
  coverage. All 47 app qualifications and physical-presentation proof remain
  incomplete. See native-input-workloads.md for every failed and final run.
- Added opt-in macOS Metal drawable presentation feedback through a narrow
  feature-gated wgpu-hal 29.0.3 vendor patch (only three upstream files changed).
  Callback samples are queued nonblocking and serialized off-thread, tied to
  worker generation/frame and CPU IDs. Enabled/disabled cargo checks pass;
  optimized native runs execute the actual drawable callback. Strict parser
  covers zero/missing/provenance/duplicate/nonmonotonic feedback; 29 Python
  tests pass. Review found no ownership blocker. See native-presentation-feedback.md.
- New evidence invalidates worker-only qualification: first run 111507 has
  123 zero presentation timestamps among 1131 input-associated worker-Presented
  frames. Repeats 111600/111704 have complete positive feedback. Full active
  presentation cadence p99 is 8.3334 ms, but frames associated with new scroll
  input have p99 25 ms in BOTH runs. No scroll or app qualification claimed.
  Investigate scheduled-send/main-dispatch/admission timing without substituting
  intervening animation frames for visible scroll changes.
- Next actual-app workload identified by source review: Word Processor normal
  start_live opens a generated 200-paragraph Markdown file through open_document.
  Extend native input with characters/named keys using existing headless
  KeyEvent::synthetic constructor (not IME commit; WP does not handle it).
  Add fixture-only read-only snapshots of real scroll_y, caret and block text;
  run independently asserted 1200-step scroll and bounded character/backspace
  edit workloads with Metal presentation feedback. No app source edited yet.


Clipped rich-text candidate checkpoint: shared exact-pixel regressions pass;
Word Processor 281 tests pass and the explicit reference-noise-envelope GPU
oracle passes twice. Native repeats 144952/145020 reduce worker mean encoding
to ~0.34 ms but increase CPU p99 to 6.77/7.31 ms; display p99 is 25/16.667 ms.
Keep qualification open. Sample 145051 points to repeated ink-bounds key
hashing/traversal; optimize that before broadening this candidate to other apps.
Full evidence and oracle limitations are in docs/native-input-workloads.md.


Grouped ink-cache follow-up implemented: font/variation key hashing occurs once
per batch, with a bounded per-font glyph cache and generation invalidation.
RED regression observed 15 keys instead of one; all 13 shared renderer tests
then pass, including strict pixel comparisons. The WP full-document GPU oracle
passes, and optimized native build succeeds. Benchmark executable copied into
Writer Performance Probe.app. Native comparison deferred while another task's
Fedora VM dependency build and queued local builds are active; observed prl_vm_app
at ~500% CPU. No performance improvement is claimed for this revision yet.
Next: wait for build completion notification, verify host load, then run two
scroll-large collector repeats and compare with 144952/145020 and the direct-bind
baseline. Goal remains all 47 apps, all required workloads and platform evidence.


Native grouped-cache repeats now recorded: CPU p99 5.171/5.262 ms in two
full samples, display p99 16.667/25 ms; third failed with 431 frames and
183.333 ms display p99 (dispatch delay unexplained). No qualification. Cache
capacity/eviction regression passes. A new strict rotated-pixel mismatch
reproduces after reverting an experimental glyph-vector reservation: three
RGB bytes at (117,24), 6 versus 7, reference repeat identical in that attempt.
All experimental reservation production edits reverted. Next priority is
isolating clipping/encoding versus cache grouping; keep exact pixel assertions.
No live parent process remains at this checkpoint. Other task's build pause
released; coordinate again before clean native measurements.


Rotated-pixel investigation resolved the observed mismatch: repeated unchanged
reference scenes also alternate RGB6/7 at exactly the affected pixel; cold/warm
glyph streams match. Atomic GPU segment ordering plus f32 area accumulation is
the source-supported mechanism. Added explicit test-only preprocessing selection
and dual oracles: stable preprocessing with exact repeated RGBA, plus production
GPU preprocessing with measured same-byte reference envelope. All15 renderer
regressions pass in final verified run. Production renderer arithmetic/clipping
unchanged; no pixel-tolerance shortcut in the exact oracle. Next work resumes
CPU clipping cost reduction and diagnosis of the failed151024 dispatch-delay
sample. Native executable0498030c… is still the grouped-cache benchmark binary;
all47 app qualifications remain open. No live parent process at checkpoint.


Fx integer glyph-cache candidate added and verified with15 renderer regressions,
native build and three comparable runs (two candidate, one prior binary). CPU
p99 5.151/5.710ms versus6.161ms in adjacent old-binary run; display p99 still
16.667/25ms. Evidence is limited and no app qualifies. Late sampling and a50ms
conditional dispatch profiler are now available in the prototype collector;
167ms dispatch stall did not recur. No live parent processes. Probe bundle now
contains Fx candidateaf092a21…, grouped prior0498030c… preserved separately.
Next shared work: reduce glyph-batch allocation/traversal overhead using the
now-stable dual pixel oracle; investigate event-loop stalls if trigger catches
one. All47 apps, required workloads and platform evidence remain in scope.


Batch-reservation candidate verified:15 renderer regressions pass, native
build succeeds, two coordinated idle-window scroll runs show CPU p994.868/4.737ms
but display p9925ms. Bundle/binary4e6df4a4… saved; no live parent processes.

A parallel runtime proof obligation is now active: the Photos
export-edited-content contract is published. Read-only audit found old proof inserts original bytes and agent
broker result could fall back to local Photos product. Implementation owner is
fixing production session/surface fallback; our subagent photos_handoff_audit
owns NEW test-only session sequence regressions using existing local Unix
production broker, no model/external network. Preserve concurrent production
hunks. Parent owns new shell/examples/edited_handoff_proof and opt-in existing
proof wiring/runner. Independent pure-Rust clockwise pixel oracle added in
pixels.rs; explicit2x3→3x2 expected fixture RED then2tests GREEN via rustc --test.
It is not yet wired to actual runtime proof. Next integrate typed production
rotate/export→Writer, retained edited PNG/hash, exact route/signature checking,
negative authority/unavailable cases and headless proof. No Photos implementation
ownership here. Performance goal remains all47 apps and allrequiredplatforms.


Borrowed uniform glyph-run candidate d74a7c42…: all16 renderer regressions and
optimized native build pass. Plain large-scroll runs154417/154459 each retain
1200 measured frames and1260 verified movements; CPU p994.568/4.978ms and
display p9916.666708ms, maxima25ms. Styled old/new comparison is next; no shared
stall or app qualification established. All47apps remain open.


Styled comparison now complete: alternating candidate/prior1200-frame runs
155004/155038/155108/155141 show CPU p99 about10.7–10.9ms and display p99
16.67–25ms. No repeatable styled gain from uniform borrowing. Short diagnostic
155218 identifies app-specific `block_runs_markup` as dominant sampled draw
work (310inclusive stacks vs118 shared rich drawing); no all-widget shared
stall claim. Next bounded experiment: evaluate mark-derived styles at mark
endpoints instead of every character, retaining exact resolution/merging and
reference regression coverage before native repeats. Parent collectors terminal,
probe bundle restored to d74a7c42… borrowed candidate. All47apps remain open.
Companion native proof currently cannot rebuild due concurrent WebViewCommand
exhaustiveness error in ui/src/webview/test_engine.rs; controller routed to
browser owner. No proof success claimed and stale app binaries are prohibited.


Mark-boundary optimization implemented in both WP style-run builders, retaining
per-character reference in widget/mark_boundary_tests.rs. RED missing helper,
2targetedtests pass,283librarytests pass/1GPUoracleignored, strengthened exact
field/float-bit tests pass, independent review no blocker, native build succeeds.
Candidate d2670a83… retained after native styled CPU p996.185/7.276/6.136ms vs
prior~10.8ms; repeated plain comparison gives no established tail regression or
speedup. Display remains unqualified (25ms p99,100ms max, one506zero-feedback
failure; lateststyled16.667p99/50max). All failed artifacts retained and no
shared-wideplateau/all-app claim. Parent collectors/builds terminal; next
investigate frame scheduling/presentation tails and remaining shared-widget
families, then all47app workload coverage. Prior turn and this turn are progress.

Companion native edited-content proof passed with frozen26artifact manifest;
controller informed of independent pixel/hash/exactroute audit. Headless broker
adapter missing and standalone runtime persist_path unset prevent headless and
durable receipt claims. Controller's implementation owner assessing shared fixes;
parent changed none of their production ownership. Proof agent holds all activity.


Main-thread gap diagnosis continuation: prior turn was progress (tested mark
optimization and repeated native measurements). Current offline analysis of six
saved runs identifies87.786/90.464ms outside timed CPU builds in two100ms gaps;
worker start spacing tracks CPU commit spacing, but exact cause remains unknown.
Results in target/runtime-proof/mark-boundary-gap-diagnosis.json. Added opt-in
input-origin timestamps at native redraw entry, timed build start, frame commit
and submit attempt in ui/src/app.rs; no normal scheduling change. Seven shared
native_scenario tests pass; diff check clean. Optimized diagnostic build started
(native-redraw-stage-build.log); live stage ordering/runtime capture pending.
All47app/workload/platform qualifications remain open, no completion claimed.


Stage diagnostic build757ecd5b… succeeds; native160918 validates1260same-origin
stage sequences, no long stall, CPU p995.938ms/displayp9916.666750/max25ms.
Repeat161010 fails missingdisplayrequests:503worker skips explicitly occluded.
Retain failed evidence, no fabricatedpercentile/no inferredcause for earlier
100ms stalls. Parent build94177 and collector24781/85197 terminal. Probe bundle
currently diagnostic757ecd5b…; prior optimized candidate d2670a83… saved. Next
foreground stage capture or broader shared-widget workload to distinguish app-
specific versus event-loop tail behavior. All47applications remain unqualified.


Continuation progress: repeated typing/scroll measured with diagnostic757ecd5b…
after revalidating no live parent jobs and unchanged binary. Typing161159 fully
occluded; retained failed diagnostic, not performance evidence. Added prototype
activation override and partial failure-report retention. Activated typing
161325/161411 validates1260edits each and complete feedback, displayp9916.666708ms,
CPU p991.933/1.916ms. Activated styled161444 remains25msdisplayp99. Same-origin
stages show one56.119ms warmup gap before redraw entry, not inside timed build;
earlier100ms measured stalls remain unexplained. All collectors terminal.
Next shared investigation: native frame pacing/event-loop delay and extra
non-input-associated typing redraws, plus coverage beyond WP; all47apps and
requiredworkloads/platforms remain unqualified.


### Prepaint commit experiment rejected; shared history work identified

The candidate described above was tested and then removed. WP tests passed
(283 passed, one opt-in GPU test ignored), but native typing runs
`wp-native-typing-20260916-162723` and `162755` both measured display p99
33.333292 ms and max 41.666625 ms. Input-associated CPU p99 was 8.211/8.019 ms.
The first run overlapped a CPU-heavy db_client test process; the repeat still
showed the display regression. Fewer paints did not make visible updates more
consistent. Source was reverted and archived in
`target/runtime-proof/rejected-prepaint-commit.json`; the probe bundle was
restored to the prior commit-tracing executable. Opt-in timing traces remain.

The short diagnostic profile `wp-native-typing-20260916-162839` identifies
173 samples in one App::commit_action -> service_history -> State::undo_status
-> history_source::patch_summary -> AutoCommit::diff path, plus 17 in another
event path. This points to repeatedly deriving undo labels from full-document
diffs. It is diagnostic evidence, not a qualifying 1,000-frame run. The next
candidate reuses a status only for identical selected historical spans and
preserves the existing transaction-flush behavior. All 47 apps remain
unqualified.


### Undo status summary reuse

The shared State path now retains one undo/redo status keyed by the resolved
historical spans and text encoding. Every referenced hash must exist in the
current document; document adoption clears the cache. Reading a nonempty
status still closes an open Automerge transaction, preserving the previous
diff side effect. Bound record components keep their existing status cache.

The repeated-read regression first failed with 21 diffs for 21 identical
queries. The replacement-encoding regression then reproduced stale deletion
counts (one Unicode scalar versus two UTF-16 units). Both are fixed. Final
`undo-status-cache-green5.log` reports66 tests matching state:: passing,
including missing-graph replacement/restoration, author selection, undo/redo,
and pending-transaction checks. Some matched tests belong to widget state
modules; this is not a claim of66 history-specific tests. Independent source
review has no remaining actionable finding. Optimized native measurement
remains pending; no display-performance improvement is claimed yet.

An intermediate replacement-plus-merge test encountered Automerge's existing
diff cache retaining the prior missing-graph summary after merge. The final
test covers direct replacement/restoration and explicitly verifies missing
graphs are not cached by State. This change does not claim to fix upstream
diff-cache behavior.


### Undo cache candidate removed after native investigation

The cache candidate above was removed; its exact source/test patch is archived
at `target/runtime-proof/undo-status-cache-experiment.patch`, and the saved
optimized executable remains `word-processor-undo-status-cache`. The probe
bundle again contains the prior `word-processor-commit-trace` executable.

Native candidate200450 failed edit continuity: the viewport moved and the
caret jumped from paragraph0 to44 between scheduled inputs1136/1137. Cause
is not established. Subsequent baseline200533 and candidate200602/200728
validated1260 edits each, but positive presentation feedback was incomplete
(zero timestamps), so no display p99 comparison is claimed.

Diagnostic profiles200638(candidate) and200656(baseline) retain187 and220
samples respectively under patch_summary. This is an uncontrolled short
profile comparison, not a demonstrated speedup. The expensive historical
diff remains substantial despite removing repeat calls. Source inspection
finds no UndoButton or set_undo_status override in Word Processor; the host
still derives labels for its default no-op consumer. Next investigate lazy
status delivery while preserving existing consumers, author/bound-history
routing and transaction/commit semantics. No shared-layer plateau or app
qualification is established. Machine-readable experiment accounting is in
`target/runtime-proof/undo-status-cache-experiment-summary.json`.


### Lazy undo-status delivery candidate

Word Processor has no status consumer, but the host eagerly generated history
summaries for its default no-op Widget callback. Change the callback to accept
a mutable provider. Default widgets do not invoke it; UndoButton and the video
editor toolbar explicitly consume it. Migrate native/headless delivery,
examples and tests together so custom overrides fail to compile until migrated
rather than silently losing their status. Keep unbound transaction flushing at
the same publication boundary. Keep bound status preparation eager: its
completed-action label cache intentionally survives pending edits and dynamic
control mounting. This avoids changing canonical record history semantics.

RED provider-signature regressions recorded;7 direct tests and49 tests matching
undo pass. Additional flush/bound tests and video-editor all-target compilation
are running; optimized native build/measurement pending. No speedup claimed.


### Lazy undo delivery retained; display benefit not yet established

Default Widget::set_undo_status now ignores an unevaluated provider. UndoButton
and video-editor toolbar consume the provider; native/headless delivery,
observers and examples migrate together. Unbound transaction flushing stays at
the previous boundary. Bound status is still prepared eagerly to preserve its
completed-action label cache during pending edits and dynamic control mounting.
This is a callback signature change: external Widget overrides must migrate.

Validation:7 direct provider tests,49 undo-related tests,1 explicit no-consumer
transaction-boundary test and2 prepared-driver tests pass. Video editor
all-target cargo check and optimized WP build pass. The dormant
ui/examples/undo_controls_shots.rs source was migrated; it is not a declared
Cargo target, so the attempted example check correctly reports no such target.
No claim that this dormant example was compiled.

Saved executable `word-processor-lazy-undo`; SHA and capture accounting in
`target/runtime-proof/lazy-undo-summary.json`. Short diagnostic201439 validates
120 exact edits with viewport fixed at0 and contains no patch_summary samples
(the earlier baseline200656 contains220). The short run has incomplete display
feedback and is not qualification evidence or a controlled speedup estimate.

Full candidate201502 validates1260 text edits but viewport moves0→798pixels.
Baseline201540 fails edit continuity. These cannot compare typing performance.
Added optional expected_offset_y validation for controlled fixed-view typing:
RED missing argument, then5 Python tests pass, including valid text edits with
extraneous scrolling. Collector now requires offset0 for this fixture and
retains effect-failure reports. Retrospective audit rejects201502. No-activation
candidate201728 validates1260 fixed-view edits but fails for missing display
requests. All failures and raw traces remain. No app is qualified; the goal
continues across all47apps.


### Scratchpad native workload adapter and shared empty-commit fix

Added a1000-line66000-byte fixture seeded through Scratchpad's production Save
path into an exclusively created directory. Existing directories/symlinks are
refused. Root snapshots expose BLAKE3 body identity/length, focus, logical
bounds and persisted scroll offsets, without full note text. Hashing and tree
traversal are diagnostic overhead; their cost has not been isolated.

The real fixture exposed an intermittent second-edit Automerge
PatchLogMismatch. A deterministic qdoc regression reproduced the cause with
actor IDs80→10(empty stamped commit)→20(edit). The stamped-commit helper now
abandons empty transactions and resets the patch log instead of opening and
committing an empty transaction. This also resets the incremental diff cursor,
documented on the public helpers; current explicit-head diff consumers remain
valid. Both fresh and already-open empty-transaction cases assert unchanged
heads and correct subsequent explicit-head patches/timestamps.

Validation:265 qdoc tests pass (`empty-stamped-green2.log`), and all4 Scratchpad
native-proof Driver tests pass after the fix
(`scratchpad-native-proof-fixed.log`,116.31s debug). They prove real Save/reload,
overwrite refusal, actual scrolling0→180→0, focused insertion and Backspace
restoring exact fixture hashes. Tab does not focus this editor; at800×600 the
validated logical click is40,130 then Home. Native physical scaling must be
checked, not assumed.

The collector and validators live in `scripts/perf/collect-scratchpad-native.py`
and `scratchpad_effects.py`. Ten Python tests validate exact changes,
post-draw state, fixed viewport, missing/malformed evidence, subprocess failure
cleanup and executable-removal metadata retention. Native input diagnostics
now emit initial actual window scale and physical dimensions. Optimized build
and native capture remain pending; these checks do not qualify Scratchpad or
any other app. Code Editor is the next identified file-based fixture candidate.


Scratchpad optimized build completed (`scratchpad-native-build.log`,2m42s);
fixture example build completed (`scratchpad-seed-build.log`,22.53s). Saved
bundle/tool identities are in `scratchpad-native-binaries.json`. Prepared
typing and scroll scenarios remain diagnostic candidates. Their initial
physical80,260 target corresponds to tested logical40,130 at scale2; native
window metadata/focus/hash assertions must verify the real run. GUI capture
is deferred while the separately requested rebuilt cross-app proof runs, so
no simultaneous foreground workload is introduced.


### Scratchpad native shaping baseline (2026-09-16 continuation)

Native runs `scratchpad-native-ayl5w1_p` and `scratchpad-native-stcza2i5`
validate all1260 edit effects and fixed viewport, with actual2x window scaling.
The first, unprofiled run has662 measured input-associated frames, CPU build
p99 44.275ms and display interval p99 50ms, with complete positive display
feedback. It fails the minimum1000 distinct-frame evidence gate; coalesced
inputs are not extra frames. The second run includes a5-second sampling profile
and must not serve as an uninstrumented performance comparison. Both preserve
raw evidence and executable identities; summary: `scratchpad-shaping-baseline.json`.

Profile:2022/2965 main-thread sample stacks include
`RichTextLayout::shape_styled`;1957 include cosmic `shape_until_scroll`.
Source inspection confirms `TextInput::ensure_shaped_layout` rebuilds a fresh
rich layout for a changed text signature, and `shape_styled` constructs a fresh
Buffer then shapes with unbounded height. This establishes shared shaping as
the next optimization target; no claim of a framework plateau is supported.

Next implementation: reuse unchanged paragraph shape/layout only when all
relevant text, attributes, line endings, font metrics, width, wrap/alignment and
shaping settings match. Preserve complete document geometry, not only visible
text. Add differential checks against fresh layouts for start/middle/end edits,
inserted/deleted paragraphs, trailing LF, Unicode/bidi, style and width changes,
selection/caret/hit testing and IME. Test actual reuse as well as output equality.
Then repeat the identical native workload with rebuilt, hashed binaries.
All47 apps remain unqualified.


### Shared paragraph reuse: implemented and measured

`RichTextLayout::shape_styled_reusing` now transfers unchanged prefix/suffix
paragraph caches only when all cosmic Buffer layout settings and exact text,
line ending, attributes and alignment match. Fresh construction still derives
document offsets/decorations. TextInput uses this path for multiline paint.
A renderer-owned Arc epoch invalidates widget layouts across different renderers
and font/locale generations. The font-change regression failed before the guard
(caret endpoint150.256 vs fresh163.200) and passes after it.

Validation:22 richtext tests and106 TextInput tests pass, including full glyph
layout/caret/selection/hit comparison over32 edit/style/width/bidi/newline cases,
an allocation-retention test, and existing IME/decorations/measurement tests.
Independent review found no remaining correctness blocker after the epoch fix.
The separate measurement path still has plain-vs-attributed signature churn;
profile before deciding its next optimization.

Saved optimized binary and SHA are in `scratchpad-paragraph-reuse-binary.json`.
Native typing `scratchpad-native-ih8abqi_` verifies1260 effects and1178 measured
input-associated frames, complete positive presentation feedback. CPU build
p99 is8.962ms vs baseline44.275ms; input-associated display interval p99 remains
33.333ms. Native scroll `scratchpad-native-j2bqcva1` verifies1260 effects and1198
frames: CPU p99 3.136ms, input-associated display p99 25ms. All-active intervals
are recorded separately, never substituted for input-associated results.
Other tasks compiled during these optimized captures; host process inventories
are retained, so these are diagnostic comparisons, not clean qualification.
Summary: `paragraph-reuse-native-comparison.json`. All47 apps remain unqualified.

The independently requested rebuilt Photos→Writer final2 proof passed actual
binaries, pixels/route/denials and persisted receipt redaction. Its manifest
records concurrent source changes during build and source_snapshot_stable=false;
it must not be presented as a stable-current-tree proof. Earlier failure kept.


### Caret search pruning and native verification

The optimized paragraph-reuse profile `scratchpad-native-zjze4y1e` identified
`caret_geometry_at` rebuilding/sorting grapheme stops through unrelated
paragraphs when only a byte fallback exists for the requested affinity. The
profile capture itself fails positive-display completeness (9 active zero-time
callbacks); it is hotspot evidence only. Phase analysis of earlier unprofiled
runs is retained in `paragraph-reuse-phase-timing.json`, on the CPU input clock,
not input-to-photon latency.

A deterministic regression observed1001 caret-stop paragraph visits for byte0
in a1000-paragraph document. The fix skips runs whose mapped inclusive source
range cannot contain that byte; empty glyph runs use the exact prior fallback
mapping. It preserves exact-match priority and first-fallback order without
assuming cursor line indices match bidi paragraph splitting. Full-scan oracle
tests cover every byte/affinity for CR/LF, bidi, emoji, combining text, Thai,
Khmer, narrow wrapping and soft hyphens.24 richtext tests and106 TextInput tests
pass; read-only review found no blocker. Compile interruptions from concurrent
History work are retained as failed logs, not counted as test RED evidence.
Actual RED evidence is `caret-pruning-red3.log`; validation summary and source
hashes are in `caret-pruning-validation.json`.

Saved optimized executable SHA742acaca6385220f2aa2243b89bc7312f6e880080a50fabcfed7fa55a578685e
is recorded in `scratchpad-caret-pruning-binary.json`. Native typing capture
`scratchpad-native-zt360sfe` validates1260 edits,1200 measured frames, zero
coalescing and complete positive feedback. CPU build p99 3.984ms, down from
8.962ms with paragraph reuse alone (original baseline44.275ms). Input-associated
display interval p99 remains25ms; all-active display interval p99 8.333ms is
recorded separately and cannot substitute for it. Source/host variation and
single-run limits remain. No qualification or framework plateau claimed.
Next investigate input cadence/presentation phase and true input-to-display
latency before changing scheduling; unsynchronized60Hz injection on a120Hz
display is a hypothesis, not an established cause.

Code Editor now has an app-local2000-line Rust fixture and durable-buffer
snapshot adapter (SHA256, explicit scroll units); four authored tests remain
uncompiled pending the coordinated final3 native-proof source/build window.
The seed example has a distinct `seed-code-editor-fixture` name to avoid
overwriting Scratchpad's helper executable. All47 apps remain unqualified.


### Code Editor capture preparation and actual input cadence

The Code Editor optimized build completed successfully (code-editor-native-build.log),
and its executable is preserved in Code Editor Native Probe.app with identity in
code-editor-native-binary.json. Source stability during this diagnostic build is
not established. The four native fixture/observer Rust tests pass; the shared
Python collector, delivery and effects suite passes 41 tests
(code-editor-python-final.log). The shared collector now supports Code Editor's
production --path opening and retains Scratchpad's --data-dir path. Code Editor
pointer validation converts physical scenario positions using actual native
window scale before comparing logical editor bounds. Missing or invalid scale
is rejected. Native Code Editor timing is still pending.

Actual input cadence is now reported separately from processing and display
intervals, with monotonic timestamps checked. Saved Scratchpad capture
scratchpad-native-zt360sfe had scheduled 16.667ms intervals, actual sent p99
20.769ms and dispatch p99 20.773ms (cadence-diagnostic.json). This establishes
sender jitter, not causation of the 25ms input-associated display p99. No
input-to-photon calibration or performance qualification is claimed. All 47
GUI apps remain unqualified. Code Editor fixture build/capture waits behind
the coordinated final4 Photos/Writer proof; no new Cargo or GUI started here.

Read-only collector review found two false-success risks: an app exiting after
its final evidence could escape health checks, and setup/hover frames could
satisfy the measured-frame gate. Reproductions are retained in
native-capture-gates-red.log. The collector now checks process health through
drain/shutdown and requires every measured input to be an actual edit or wheel
movement, after validating effects and postdraw state. All 43 combined Python
tests pass (native-capture-gates-green.log); independent re-review found both
blockers closed. Existing saved typing and scroll scenarios satisfy the new
workload gate. The 1000-frame and positive-presentation requirements remain.

The separately requested final4 Photos/Writer proof passed with all1122 relevant
inputs and actual binaries stable. That result proves the tested handoff, not
performance. Holds released; the unique Code Editor fixture helper build is
now queued/running as session35920. Clock-correlation follow-up design is in
target/runtime-proof/input-display-clock-design.md; not implemented or measured.


### First native Code Editor evidence

Saved fixture helper completed; hash added to code-editor-native-binary.json.
Geometry probe code-editor-native-ac3xrxyl confirms scale2, editor logical bounds
[0,44,800,600], fixture57390bytes/2000lines. Probe intentionally has insufficient
timing evidence and remains a failed diagnostic.

Typing code-editor-native-3es3jod0 timed out after681/1262effects. Initial body
had an unexpected b atbyte0 before the first input; correctness validation
rejects it. Five-second process sample code-editor-baseline-sample.txt identifies
full-document rehighlighting inside EditorView::rebuild as dominant event work.
This is hotspot evidence, not a valid performance baseline. An exact incremental
parse-state cache with fullfresh equivalence/work-count regressions is in progress.
No framework-wide plateau is claimed; this particular dominant cost is app-local.

Scroll code-editor-native-k0o7ourj validates1260movements with1200measuredinputs
and997measuredCPUframes (CPUbuildp993.528ms). It fails display completeness:
659workerframes report occluded;365input-associatedframe mappings lack display
requests. No qualification; retry with noactivation to reduce desktop interference.
Raw failures and diagnostic.json files are retained.

Noactivation scroll retry code-editor-native-fxugpn_s completes with1260verified
movements and1177measuredframes; positivepresentationcomplete. CPUbuildp99
4.205ms, input-associateddisplayp9925ms,max366.666ms; queue-delayp99111.181ms.
This fails60FPS cadence and is not qualified; saved summary
code-editor-scroll-baseline.json. Unprofiled noactivation typing retry runs
against the same preserved baselinebinary (session51244) while the incremental
highlight regression has now demonstrated2000parsedlines versusexpected1.

Clean noactivation typing retry code-editor-native-67wg9f46 also times out:707
effects,705 exact edits verified against fixture/caret state; partial dispatch
interval mean84.633ms,p9994.273ms. This is an incomplete workload, not a full
latency/throughput qualification. It independently confirms the typing backlog
without the previous initial-character contamination or process sampling.

Scroll phase diagnostic code-editor-native-fxugpn_s/phase-diagnostic.json locates
the largest357.972ms gap between CPU submit and next redraw entry, rather than
inside timed frame construction. Worker totalp998.455ms,max59.085ms; redraw
build-start-to-submitp996.489ms. Do not add independent percentiles or call this
input-to-photon evidence. A separate120Hz scroll supply diagnostic uses twice
the events at half wheel delta, preserving nominal scroll speed, duration and
reversal period. It tests rendering capacity under denser input; it cannot
replace or erase the failed original60Hz cadence case.

The first120Hz scroll diagnostic code-editor-native-d91uwfx_ fails strict effect
validation: input66 moves no content, with no proven cause. Retained unchanged.
The repeat code-editor-native-9blh9opz validates2520moves/2391measuredframes with
complete positive feedback: CPUbuildp993.712ms, displayedintervalp998.333417ms,
max50ms. Nominal scrollspeed/duration/reversalperiod equal the60Hz scenario.
This demonstrates120Hz rendering capacity in this case and supports investigating
input supply as a contributor to25ms60Hz cadence; it does not erase the original
case, explain the dropped movement, or establish whole-app qualification.
Summary in code-editor-scroll-supply-diagnostic.json.


### Incremental Code Editor highlighting checkpoint

IncrementalHighlights retains per-line outgoing ParseState and ScopeStack;
unchanged prefix/suffix reuse requires same language and exact incoming state
equality. Changed text still receives the existing full grammar parser; cached
spans/states move rather than clone. Text splitting/scanning remain O(n). The
actual EditorView regression goes from2000parsedlines to1, with fullfresh span
oracles for multiline syntax, Unicode, newlines, insertion/deletion/reverts and
language changes. Independent review found no introduced correctness blocker.

Original fullsuite218pass/1 macOS /var versus /private/var temporarypath failure
is preserved. With canonical TMPDIR=/private/tmp all219 tests pass; logs and
sourcehashes live in target/runtime-proof/code-editor-incremental-highlight/.
No native speedup yet claimed. Optimized rebuild started as parent session7921,
log code-editor-incremental-native-build.log; next save distinctbinary and repeat
the identical1260edit60Hz noactivation workload. Originalbaseline must remain.


Optimized incremental binary saved separately (SHA9dfd04eb1ac2d1f16b933bff16703d4dc041b59fd67c70c64bbde4919f3c51fc);
code-editor-native-w8k_qfyi completes1260verifiededits1195measuredframes with
completepositivepresentation. CPUbuildp998.921ms; input-associateddisplayp9925ms.
Baseline clean run only completed705editsbefore60stimeout. This is a meaningful
completion/backlog improvement, not a ratio of comparable fullrun percentiles.
Noqualification or sharedframeworkplateau claimed. Profiled repeat underway
for remainingcost attribution. Summary code-editor-incremental-native-result.json.

Collector now isolates XDG_CONFIG_HOME and XDG_CACHE_HOME in addition to data.
A subprocess assertion demonstrated missingconfigenv beforefix; all43 combined
checks pass afterward (native-config-isolation-red/green.log). The firstcandidate
run above predates this config/cache isolation; future runs record it by source
and explicit environmentdirectories, so comparisons must retain this distinction.

Profiled incrementaltyping repeat code-editor-native-jt776dbk completes1260edits
and1032measuredframes withpositivefeedback, butCPU12.421ms/display41.666625ms
p99; samplingperturbsrun, retainedashotspotevidenceonly. Full-document text
materialization remains under diagnostics/resourceavailability and diagnostic
snapshot reads. Read-only invalidation review is in progress before selecting
a cache/refresh change; action freshness and pending/remote edits must survive.
FileExplorer app-local fixture/observer tests nowinprogress after parentGUI
released. Productionroute/remainingSettingsTerminal/resizegaps recordedin
next-native-coverage.md; inventorystillall47unqualified.


### Next cache and File Explorer integration in progress

Read-only review rejects weakening shared resource invalidation: widgets may
change actions without CRDT changes, and replace-text resource payloads must
capture current optimistic bases. Narrow CodeEditor local-diagnostics cache
is being implemented with heads/buffer/language/lint-scope keys only when
pending_ops()==0. Pending edits always bypass/evict, never call get_heads;
currentLSPdiagnostics/status still merge everycall. Scopedtests queued, no
correctness/nativeimprovement claim yet. InitialREDmissingfields was a compile
failure, not behavioralregression evidence; agent notified to distinguishit.

FileExplorerfixture/observer3scopedtests pass. Fullparallelapp suite230pass9fail
with sharedambient-I/Oguardinterference; serialrerunqueued. Newsharedcollector
branch usesactualdirectorypositional, >=1000entries>=100images, isolateddata/
config/cache. Itsnewsubprocessroute testawaits Filesvalidator module;7 existing
collectorregressionspass. All nativequalification gates remain unchanged.

Files Pythonvalidators nowimplemented; combined52collector/delivery/effecttests
pass (file-explorer-native-adapter/collector-integration.log), including actual
directorypositional subprocess route. Validatorallows async thumbnail progress
and lagging immediate contentorigin, while requiring directedoffsetmovement and
postdrawcontenttranslation plus completeorderedinput mappings.

FourCodeEditordiagnosticscache regressionspass: identical11refreshes read once
versus11uncached withsamepaneloutput; pendingoperationsnevercommit/cache,
includingequalcountdifferentcontent; remote/historical/buffer/language/scope
changes invalidate;LSPpayload/statusalwaysfresh. Fullapp suiteandindependent
review stillpending. No nativecache performanceclaim.

FileExplorerfullserial239tests pass andactualhelperCLI verifiedoneJSONmanifest,
1200diskentriesandrefusalwithoutoverwritingexistingPNG. Uniquehelperhash recorded
infile-explorer-native-adapter/helper-cli-verification.json. Optimizednativebuild
68510remainslive; geometryprobe prepared.

Diagnosticscache review found pending_ops()==0 permits an open emptytransaction.
Newbehavioralregression reproduces PatchLogMismatch afteractor80→10empty→20write
(diagnostics-transaction-red.log). Original get_heads-on-livedoc candidate is
NOT safe. Clone-onlyrevisionwitness is being tested, withrepresentativehistory
costcomparison required before retention. No pendingcache speedup claimed.


FileExploreroptimizedbinary saved separately, SHA
ff27db9431c1dc99f1b0e82ce375830695b7fd8b1dbbecf8aaa1d4be0383ca76.
Geometryprobe file-explorer-native-_rs4if9v confirmsloaded1200entries/order,
scale2,viewport[212,56,788,596],Listmode; no startupreadinessdelayneeded.
Fullscroll file-explorer-native-ecqdn8xr deliversall1261effects butstrictchecks
reject146unmovingwheels. Firstmissingblock249..262 atoffset1440; subsequent
upwardclamping followslostdownwardmotion. Causeunproven; rootpointerdiagnostic
requested before changing productionrouting. Failedrawrun retained.

Diagnosticsclone-witness cache passes225tests (1manualcostbenchmarkignored),
independentre-reviewclear. Originaldocument immutableborrow and exactDebug
comparisons coveropenemptytransaction/patchcursor preservation. Earlier raw
rollback+activecursor PatchLogMismatch reproducesWITHOUTdiagnostics too, so
thatcombinedfixturefailureisnotattributedtoclonemutation. Clone testprofile
cost2000lines/history0,100,1000 is238/319/838us total50refreshes versus
119767/123849/118299us uncached; notnativeproof. ParentoptimizedCodeEditor
diagnosticscandidatebuild40189 nowrunning, logcode-editor-diagnostics-native-build.log.


Filespointerobserver addslogical last_cursor only;4nativeadaptertests pass
(pointer-green.log). ActualCursorMoved scale2 maps[1000,400]→[500,200];
productionList→GridScrollable replacement retainswheelrouting, andoutside
pointerstopsmovement. No productionbehaviorfix. Pointeroptimizedbuild8915
queued behindCodeEditordiagnosticsbuild40189; preservepreviousFilesbinary.
Settings app-local observer/tests preparation started forproductionsearch,
retainedrowIDs/focus andimmutablecloned effectiveheads witness; no screenshot
hardwareoverride or nativequalification.

CodeEditordiagnosticscandidatebuild40189passed; savedbinarySHA
00aa7751834786a89557a621bbb4186a4d42880b1bcabeefad4abd475fe400e6.
Nativecapture code-editor-native-4g3_a48d completes1260verifiededits and
1199measuredframes withpositivepresentationcomplete. CPUbuildp998.083ms
(earlierincrementalhighlightonly8.921ms), dispatchqueuep992.527ms, displayed
intervalp9925.000042ms. NotcontrolledA/B: source/hostconditionsdiffer;
FEpointerbuildwasactive. Savedresult code-editor-diagnostics-native-result.json.
No60fpsqualificationorframeworkplateau claimed.
SettingsobserverGREENtests cleared; FEpointeroptimized8915stilllive.

### Pointer isolation verification continuation

Previous user-facing timing explanation made no implementation progress; resumed
from the existing failing acknowledgement regression. Reproduced all four RED
subcases, added literal-boolean confirmation enforcement, and ran all 53 native
collector/effect/delivery tests GREEN (`pointer-isolation-green.log`). The old
File Explorer native binary is rejected for missing acknowledgement in capture
`file-explorer-native-ev3gffjd`, recorded in `pointer-isolation-native-red.log`.
Added opt-in no-activate scripted-window `set_cursor_hittest(false)` with actual
success metadata and backend-error exit. Independent review found no blocker;
actual cursor stability still requires a new native capture. Optimized build
is pending; this is measurement infrastructure, not a claimed speedup.

Settings agent completed query-only navigation refresh, retaining the content
pane instead of rebuilding/reseeding it on each filter edit. Parent inspected
production sync path. Agent reports 45 passing targeted tests including strict
document-head preservation and existing category/narrow navigation coverage;
no Settings native timing or qualification yet.

Optimized pointer-isolation build completed (session69888 exit0). Saved bundle
`File Explorer Isolated Pointer Probe.app`, identity in
`file-explorer-native-adapter/isolated-pointer-binary.json`. Capture
`file-explorer-native-0ho93yu5` confirms actual backend acknowledgement, one
scripted cursor position [350,200] throughout,1260 verified scroll movements,
and1246 verified postdraw states.1185 measured CPU frames: p99 13.84ms.
Display gate FAILED: parent command omitted `--features ui/presentation-feedback`,
and app logged the corresponding explicit display error. Retain failed report;
CPU-only evidence is not displayed performance. Corrected optimized feedback
build started as session61526; not yet terminal.

Settings exact search validators added in `settings_effects.py` with nine tests.
Parent reran the combined suite:62passed in `native-collectors-with-settings.log`.
Independent app-local review found no blocker in query-only refresh or clone
witness, but current wide/default-pane tests do not directly exercise transient
content identity and narrow search focus; agent is authoring those regressions
without Cargo/GUI during the native build/capture window. Settings has no native
timing yet. Catalogue result expectations are exact (22empty,3forz), not merely
"some results narrowed"; unresolved lazy backend mutations fail document gates.

Corrected feature-enabled build61526 completed exit0 (2m01s). Saved immutable
`File Explorer Isolated Feedback Probe.app`, hash/command recorded in
`file-explorer-native-adapter/isolated-feedback-binary.json`. Capture34071
exit0, `file-explorer-native-nlnz60ei`:1260 verified wheel movements,1252
postdraw states,1191measuredCPUframes, all1191input-associated positive display
callbacks complete. Cursor fixed at [350,200]. CPU p99 14.191ms; input-associated
presentation interval p99 33.333208ms/max58.333208ms; dispatch queue p99 12.698ms.
All-active interval p99 16.666750ms is separate and cannot replace inputgate.
See capture `isolation-diagnostic.json`. Valid baseline, NOT qualification;
source snapshot stability and repeated-run evidence remain absent. Actual
input-to-display clock calibration still unimplemented.

All parent builds/GUI captures terminal; Settings agent cleared to run three
new pane-retention/category-replacement/narrow-focus regressions. Next profile
File Explorer CPU/dispatch costs under this corrected same workload and finish
Settings native runner coverage. All47GUIapps remain unqualified, no shared
framework plateau claim.

### Shared NavShell hotspot located in native scroll profile

Prior goal turn made progress: nativepointergate+validbaseline. Current phase
breakdown of `file-explorer-native-nlnz60ei` in `phase-diagnostic.json` shows
apply-layout mean6.398ms/p99 9.46ms and resource-availabilitymean3.398ms/p99
3.691ms. Native sample run`80iaqdex` failed movement correctness after physical
cursor ingress atinput1122 despitebackendhittestack;139wheelsunmoving. Keep
failedcapture. This disproves the assumption that backendack alone isolates
allmacOSevents. Explicit origin-aware benchmark-onlyroute filtering is being
regression-tested byframe_evidence agent; normal/lifecycle routing mustremain.

Initialsample stripped, so parent built optimizedsymbolized binary via cargo
rustc --profile thin-app -p file-explorer --no-default-features --features
ui/presentation-feedback --bin file-explorer -- -C strip=none (8366exit0).
Saved `File Explorer Symbolized Probe.app`, identity
`file-explorer-symbolized-binary.json`. Nativecapture`file-explorer-native-tqrqv3yy`
exit0, all1260movementsvalidated;5s sample `file-explorer-symbolized-sample.txt`
(sample25263exit0) is hotspot evidence, not unperturbedtiming.1214/2808main
samples in NavShellapply_layout,1207configured_font_system,1197FontSystem::new,
1146load_system_fonts. Source confirms newTextRenderer atnav_shell.rs:872 on
everyapply_layout. Sharedframework optimization assignedparagraph_reuse_review
with REDbehavioral regression and geometry/fontcorrectness checks. No plateau.

Settings additionalcoverage:26app-localtests + existingnarrowBackintegration
passed after testharness fixes matchingactualhost startupsettling/controlaction
dispatch. Earlier2harnessfailures retained insettings-native-adapter logs.
Productionquery-onlynavfix unchanged; directpaneidentity,localtext/caret and
narrowsearchfocus nowcovered. Parentread terminalGREENlog.

Parent strengthened collector acknowledgement after backend-only isolation
failed: requestedisolatedruns nowrequire both literal`pointer_passthrough:true`
and`input_route_isolation:true`. Fournewrouteacksubcases reproduceRED;62combined
testsGREEN in `input-route-ack-green.log`. Oldsavedbinaries intentionallyfail
newrequestedroutegate; newnativecapture awaits originroutingbuild.

SharedNavShell candidate refined: reusesolvedchildtree fortranslation-only
frames onlywhenpaneextents unchanged anddescendantsneednomeasurement; full
paths retainedforgeometry/style/contentchanges. AgentauthoringRED+geometry
regressions; productionoptimization notyetverified. Secondaryreview identifies
fullsemanticcatalogueconstruction insidecontrolavailability (167samplebranch)
as a sharedprojection opportunity, plus app-localICUformatterconstruction as
a latercandidate. Do not bypass localwidgetresourceinvalidation/permissions.

### Origin routing verified; NavShell behavioral RED pending

Previous goalturnprogress: actionable sharedfontscanprofile and stronger
collectorackgate. Originrouting3behavioral regressions nowGREEN3192 after
actual3failureRED53482. Parentreadgreen-retry.log: scriptedScrollable/TextInput
effects preserved, physicalsameDeviceId events suppressed, lifecycleadmitted.
Independentreview no blocker; isolation scopesplatformWindowEvent input, not
AccessKit requests or externalapplicationevents. Syntheticinputtests16692
queued/live underagent; no optimizednativevalidation yet.

NavShellproduction remains unchanged while regression fixture corrected:
set_offset_y doesnotmarkdirty; useactualscroll_by. CompileerrorsfromtestText
borrow/concurrentHistoryReadDocimportfixedbyowners. NavRED3session39109
verifiedqueuedbehindsharedCargolock, no duplicate started. Suiteinventory
updatedwithactualFilesbaseline andSettingsobservercoverage; all47statuses
andemptyqualificationevidence remain unchanged.

NavRED3 completed with meaningful failure:4font-system constructions across
4real scroll translations, expected0; geometryassertionspassed. Sharedreuse
implementation then39NavtestsGREEN (`nav-shell-layout-green.log`), including
theme/resize/replacement. Review identified modal traversal omits dirtybackdrop
panes; agent authored regression91371 and is correctingguard toinspectall
retainedpanes. Do not claimcandidatecomplete beforethatcheck.

ActualFilesFileView/PlacesSidebar leaves retain defaultalwaysdirtycontract, so
Navfastpath alone maynotimprove thisnativeworkload. Parentinspected immutable
FileView entries/mode/scale builders, geometry-onlymeasure, sidebarpercentfill.
Preciseappmeasurementdirty integration still needed with meaningful no-remeasure
andresize/theme/model replacementtests; noappcontracteditmade yet.

Originroute syntheticinput16692terminal0(2tests), isolation3testsGREEN; no
optimizednativevalidationyet. Sharedsecondarylazy-control-projection work
started byframe_evidence with countedsemanticconstructionRED; permission,
widgetgeneration, fullcombinedvalidation andinvocation mustremainunchanged.
Review confirmscurrentallowedaddresses derivefromdeclaredresources evenwhen
combinedvalidationclearscomplete; preservebaselinepresentation+secureinvocation.

### Files integration of retained layout

ParentactualFileView+PlacesSidebar+Scrollable+NavShell1200-entry regression
RED33187 measured12FileViewmeasurecalls for4scrolls; geometry80pxtranslation
passed. Addedpreciseis_layout_dirty=false toimmutableFileView andpercent-fill
PlacesSidebar, documentedassumptions; GREEN17526 eliminatedall12calls. Expanded
List+Grid/widthresize/textscale/directoryreplacement oracle initiallyfailed
becausefreshGridexpectedobject columns wereuninitialized (content_height uses
resolvedcolumns); correctedexpectedcolumns_for_width, noassertionremoved.
FullserialFiles23370exit0:241tests passed38.27s (`file-explorer-measure-full.log`).
Reviewerfound no contractblocker. NavShellmodalfixfinal40testsGREEN; parentreadlog.

Lazycontrolprojection3newtestsGREEN andindependentreviewnoblocker. Broader
actionpalette/semantic/boundauthoritytests agent81075queued/live; do notclaim
thosepasseduntilterminal. CombinedoptimizedsymbolizedFilesbuild24349live
(`files-shared-candidate-build.log`), sourcebeforehash1123knowninputs captured
in`files-shared-candidate-source-before.json` (notexhaustivedependencyproof).
CandidatewillincludeNavreuse, Filesleafcontracts, lazycontrolprojection and
benchmarkoriginisolation; anynativegainmustbecreditedtocombination, notone
isolatedchange. All47apps remainunqualified.

Combinedcandidate build24349exit0 (3m20s), saved `File Explorer Shared Layout
Probe.app`, SHAaa3f4eee6ea68c2b0921fce4fb2c7dd0ca55543e67f6bcc9e0ed99928aa09a6e.
Known1123sourceinputs checked:onlyui/src/widget/webview.rs changedduringbuild;
notanexhaustivedependency/stable-sourceclaim. Binaryidentity manifest
`files-shared-candidate-binary.json`. Lazyprojectionbroader81075exit0:12action
palette+31semantictree+18boundcomponenttests=61GREEN; independentreviewclear.

Nativecapture75403exit0 `file-explorer-native-mkpmmqe4`:1260validatedmovements,
1185measuredframes,completepositivefeedback,bothinputisolationacks true,
cursorstable[350,200]. CPUmean2.957ms/p99 7.462ms vsprior10.058/14.191ms.
Applylayoutmean0.003608ms/p99 0.005ms vsprior6.398/9.46ms; resourceavailability
mean2.562ms/p99 2.842ms vsprior3.398/3.691ms. Inputdispatchqueuep99 4.013ms
vsprior12.698ms. Displayintervalp99 STILL33.333292ms,max74.999875ms.

Repeatedidenticalsavedbinary/scenario77156exit0 `file-explorer-native-tqpg7v99`:
1260movements,1199measuredframes,completepositivefeedback; CPUmean2.963ms/p99
7.418ms; dispatchqueuep99 3.574ms; displayp99 16.666833ms,max33.333375ms.
Nearbudgetdisplayinterval inrepeat doesnoterase firstcandidate33.33msp99failure.
Comparisonartifact `files-shared-candidate-comparison.json`; firstcandidate
phase/cursorchecks inits`shared-candidate-diagnostic.json`. RepeatCPUreduction
observed, butcombinedchanges andconcurrenthost/sourcevariations preventisolated
attribution; noallworkload/appqualification. AllparentGUI/buildterminal and
HistorynotifiedGUIholdreleased. Next profile remaining~2.56msresourceavailability
anddeliverytail/inputcadence, addGrid/nativeSettings coverage; all47GUIapps
remainunqualified, sharedframeworkplateau notestablished.

- Follow-up scheduling investigation: two valid Files candidates contain1183/1195 CPU frames without new input between first/last measured input frames, costing3604/3667ms CPU out of7109/7220ms total. Counts alone do not prove redundancy; scrollbar hold animation and unconditional about_to_wait redraw explain the self-pumping source path. Saved diagnostic: target/runtime-proof/animation-scheduling/baseline-submissions.json. New bounded scheduling fix/regressions are in progress; no native gain claimed.
- Profiled Files run rdp1ambn is a retained failure: all1260 effect steps verified, but measured CPU639 has zero presentedTime; all-active CPU639/640 have zero timestamps. No missing callbacks. CPU p998.804ms is perturbed sampling evidence only; presentation gate correctly rejects this run.
- Clock review replaces endpoint-origin calibration proposal with direct per-input host brackets at send/dispatch. This avoids extrapolation after sender completion and preserves coalesced-input identities. Design updated at target/runtime-proof/input-display-clock-design.md; not implemented, no absolute latency claim. All47 apps remain unqualified.

- Shared animation request pacing implemented in ui/src/animation_schedule.rs and narrow AppHandler integration. Persistent deadline prevents repeated callback redraws; idle/reduced-motion reset; no late catch-up burst. Review found deadline expiry/min-selection defects, reproduced both and fixed with independent next_wake selection. Five integrated Cargo tests pass (green-cargo-final.log); standalone policy RED retained, not a full native AppHandler RED. Input/exposure/retry routes remain outside the gate. Independent follow-up review found both issues resolved.
- Optimized scheduler-candidate build started, session72713, target/runtime-proof/animation-scheduling/build.log; verified live waiting on shared Cargo build lock held by concurrent browser compilation. Do not restart on observation timeout. Native before/after remains pending: animation-only pacing may still interleave with immediate input redraws, so no submission reduction or display p99 gain claimed. Coverage checker still reports47 unqualified apps.

- Scheduler candidate build72713 completed exit0 and saved as File Explorer Animation Pacing Probe.app; SHA5585181ae5e49b44490d8e929c9604182552b444f0f5bd8e249caa4c49a41e8e. Known checked source changes before UI compile ended: headless.rs and history_mode/host_tests.rs. New host-clock instrumentation was wired after UI compilation, so this binary does not include it. No exhaustive stable-source attribution claimed.
- Two same-workload scheduler candidate captures pass all1260 effects and positive feedback: 9l1mu3no and __xff2nq. CPU p992.975/2.748ms; input-associated display p9925ms in BOTH, hence target still missed.1197/1200 input-associated frames and1119/1133 other active frames; timer pacing alone retains most extra work. Saved comparison target/runtime-proof/animation-scheduling/comparison.json. No shared-layer plateau or all-app qualification. Next measured fix: stop opaque-scrollbar hold redraws while preserving fade wake, terminal clearing and momentum.
- Direct native host-clock brackets now wired at send+dispatch with unchanged Instant diagnostics. Helper3 tests and focused native_scenario10 tests pass. Python input_latency analyzer retains every coalesced measured input, explicitly reports unavailable old/nonmac clocks, rejects invalid brackets/feedback/mappings/negative bounds, reports uncertain zero crossings without clamping. Review found noncontiguous batch admission and overflow defects; behavioral RED then fixes retained. Combined Python suite75 passed, then extended latency/presentation/delivery suite26 passed. Collector integrates optional latency result; repeated scheduler capture correctly reports unavailable. Native latency instrumentation still requires rebuilt binary and valid live capture before any latency claim.

- Scrollbar hold follow-up in progress: behavioral RED exit101 proves unchanged opaque hold pumped animation and independent momentum demand was absent. Candidate splits actual fading/expired terminal clearing from hold, preserves momentum demand and schedules fade-start. NavShell all-retained-pane and Files nav/toolbar deadline forwarding regressions are pending agent-owned sessions37968/72031, logs target/runtime-proof/scrollbar-hold/red-nav.log and red-files.log. No end-to-end green/native claim yet. Audit found no other immediate toolkit-Scrollable deadline-drop roots: Monitor/TOTP/Camera use custom scrolling; DeviceMesh has none below its override; Browser/component/resource wrappers already forward.
- Latency analyzer independent review defects were reproduced and fixed: ordered contiguous batches/strict CPU order and finite derived bounds required. Final scoped suite26 passed; optional collector path exercised by repeat scheduler capture, reporting clocks unavailable as intended. No parent build or GUI/capture session remains; clock-instrumented native build awaits completed scrollbar change and tests.

- Opaque-hold follow-up now GREEN and reviewed: Scrollable38, NavShell41, Files focuseddeadline1 tests pass (sessions63000/10457/34195 terminal0). RED specifically established holdselfpumping, momentum demand, modaldeadline omission and Filesmetadata-only deadline. Root update call verified currentapp.rs; reviewer retracted a stale-comment concern, stale NavShell comment corrected. Candidate preserves activefade and terminaldamage, pinned/reducedmotion behavior, childdeadline minimum, momentum movement+settlement, and metadatawake precedence. Native performance still unproven.
- Optimized nextcandidate build session48860 running with automatic saved-bundle/hash/source snapshots: target/runtime-proof/scrollbar-hold/build_candidate.py, build.log. Includes holdoptimization and directhostclock diagnostics, with pacing retained. Source known set before/after collected; not exhaustive dependency proof. No native capture active at this checkpoint.

- Evidence precision correction: Cargo may pipeline Files compilation while UI code generation is still running. The earlier source-after-ui-compile filename is only a log-stage snapshot, not proof of UI compiler termination. Scheduler candidate captures independently show missing host fields and unavailable latency; no precise source-read boundary is inferred. Candidate manifest corrected. Upcoming hold capture uses a saved Python runner snapshot (scrollbar-hold/capture-scripts plus hashes) to avoid concurrent Settings adapter changes during launch.

- Hold/clock candidate build48860 terminal0, saved bundle File Explorer Scrollbar Hold Probe.app SHA5f272fb7207b4554be1015532964007274cc34ea4152cfada54bfc6ee0ba6bfd.1131 known sources checked; history_mode/host.rs changed duringbuild; no exhaustive stable-source claim. Two native captures53269/11339 terminal0: e1bj0bmj/yzv25qln each1260effects,1200/1199 measured frames, complete positive feedback and directhost latency for every1200measuredinput. Extra active frames67/68 versus prior1119/1133; totalCPU2631/2747ms versus5322/5121. CPU p993.920/3.864ms; displayinputp9925ms BOTH, goal stillmissed. Send-to-present median24.063/27.268ms,p9932.801/35.416ms; dispatchp9932.731/34.265ms. Synthetic nextpresentedframe metric, notphysicalinput-to-photon. Rawcomparison scrollbar-hold/comparison.json; all47unqualified, noframeworkplateau.
- Settings native capture adapter added with normal no-argument isolatedlaunch and1200measuredz/backspace searchsteps. Existingfeedback/1000frame/identity/latencygates retained, asyncNetworkdocumentchanges duringwarmup remain failures; firstpresentation doesnotprovequiescence. Agent21runnerchecks and parent80combinedPythonchecks pass. Native Settings build+savedbundle pipeline running session66526, settings-native-adapter/build_candidate.py/build.log. No Settings native timing yet. No parent GUI/capture active; both Files holdsreleased.

- Settings build66526 terminal101 due concurrent History Box<HistoryHost> inspectioncast. Currentheadless.rs alreadycorrected to history.as_ref(); failedlog/source snapshots preserved in settings-native-adapter/build-attempt1-failed, ownernotified, retry23976running. Do notrestart on timeout. Renderpipeline read-only findings: effective source requestAutoVsync/FIFO, shared ui/src/gpu.rs desiredlatency2, Metaldrawablecapacity3; no proof of fixed2refreshdelay. Measured CPU worker durations do not coverGPUcompletion/queueresidence/compositorwait. Next instrumentation/experiment scoped in animation-scheduling/e1bj0bmj-render-pipeline.md; no configchange made.

- Settings retry23976 terminal0, saved Settings Native Search Probe.app SHA0a2f31872fceacf4889652871f05146ed65fb15337d250f043778a2d20a8c51e. Known1141sources checked; concurrentHistoryheadless/host files changed. Nativecaptures42065/21494 terminal0: yenvppb7/uz2vhh_1 both1260edits+stableeffectiveheads+completepositivefeedback,1200/1195measuredframes, only9extraactiveframes each. CPU p991.966/2.785ms, displayp9916.666750/25.000000ms. Send-to-presentp9929.584/33.850ms. Repeatmissesbudget; Settings remainsunqualified, otherworkloads missing. Comparison settings-native-adapter/comparison.json.
- Opt-in renderboundaries now instrument mainchannel send, workerreceive/render, actualpresentcall and requested/effectiveMetalconfig, samehostclock+CPU/workeridentities. Defaultpathdoesnotreadclock. Parser recognizes receive-before-sendreturn and display-before-presentreturn; validates explicitnullauxiliaryidentity withoutaliasing and earlySurfaceGone shape outsideexpectedPresentedframes. BehavioralRED and87combinedPythonchecks pass. FeatureenabledRust8pass; nofeaturetest initially hitconcurrentwindowcloseAPI RED, nowdefinitionspresent and agentrerunpending. Disabledproductioncheck71873live. No GPUcompletionclaim; workerdurationincludeslogging/device.poll.
- Diagnostic Files nativebuild96837 running with auto savedbinary/sourcehashes and copiedPythonrunner: target/runtime-proof/render-boundaries/build_candidate.py/build.log. No presentmode/latencyconfiguration change. SettingsGUIholdreleased; no activecapture. All47stillunqualified, sharedplateauunproven.

- Disabled-feature productioncheck71873 terminal0; focused nofeaturetests retry1260running after concurrentwindowcloseAPI completion. Originalfailure84809 preserved; noprofiling-source fault identified. Boundary nativebuild96837 verifiedlive, noactiveGUI/capture; nextturn must pollsamehandle and inspect manifest before nativecapture. Python boundary/parser integration87tests green (collector-suite.log), allstage/nullable/SurfaceGone regressions included. Remaining all-app objective unchanged.

- Renderboundary nofeaturefocused1260 terminal0,8pass; feature8pass+disabledlibcheck+standalone7 alreadygreen. Nativebuild96837 terminal0, savedRenderBoundaryProbe SHA9f7bc83552c237639b8636db4cb076a17d470df8168de47e0808b25400f47574;1134knownsourceschecked, concurrentHistory/Chromium/widget/tilehost changes notedmanifest. Diagnosticcapture92910 terminal0: dm5x9wza validates1260effects,1200measuredframes, completepositivepresentation+host/renderboundaries. CPU p993.777ms, displayp9925.000042ms. Runtimeconfig confirmsdesired2/maximumdrawables3/syncenabledtrue/transactionfalse. Main send→receive median0.012625ms,p990.2465ms; present-return→display median19.038542ms,p9928.051042ms. NotGPUcompletion attribution. baseline-summary.json retainsallstages andlimits.
- Single-variable parametertrial nowinprogress: ui/src/gpu.rs desired_maximum_frame_latency temporarily2→1, AutoVsync unchanged. Nativebaselineabove providesbehavioral evidence; no meaninglessconstantunitassertion added. Design andoriginalGPUsource preserved at target/runtime-proof/frame-latency-one/design.md andgpu-before.rs. Build25411live withauto savedbinary/sourcehashes. MUSTmeasure/repeat effectiveMetalcount2,latency/cadence/backpressure/effects/feedback before retaining; revertonlyscopedvalue ifno repeatablegain orregression. Concurrent source changes meaninitialruns exploratory, not perfectlyisolatedA/B. Noactivecapture; GUIholdreleased. All47remainunqualified.

- Lower-buffering build25411 completed exit0; saved candidate SHA03f2655283916d683f675c01324c12087f78d3ddad60fadabf0af12b640deab7. Native runs 8cghv6n8/hn7yjjty both validate1260 effect steps, completepositivefeedback and all1200 inputlatencies;1158/1198 measuredframes. Both confirm requested1/effectiveMetal drawables2/synctrue/transactionfalse. Displayp99 33.333/25.000ms; input-to-display medians27.097/27.070ms,p99 138.092/36.930ms versusbaseline24.488/33.649ms. WorkerCPU p99 6.948/6.323ms versus2.372ms. Concurrenthostcompilation andchangedbuildsources preventcausal attribution; no repeatablebenefit demonstrated. Reverted ONLY gpu.rs desired_maximum_frame_latency to2. Saved comparison frame-latency-one/comparison.json andallrawcaptures. GUIholdsreleased; all47remainunqualified, sharedplateauunproven. Next: investigate presentation scheduling/input-phase tails withoriginalbudget; avoid assumingdrawablecapacity explains delay.

- Native gap decomposition completed across originalbudget dm5x9wza andbothlowerbudgetruns, allpair telescoping identitieschecked. Findings/scripts/rawderivedrows: target/runtime-proof/presentation-gap-stages/. Worst233.333ms displaygap isframe1293/input1248: send/dispatch intervals17.600ms, render-send236.029ms. ExistingInstant diagnostics narrow218.028ms to framecommit→submit, AFTER profilercommit(total2.073ms). This region includes native snapshot/logging,damage/markdrawn,a11y,optionalHUD; no individualcause proven. Baseline/repeat worstpostcommit7.868/4.293ms. Nexttargeted instrumentation mustseparate these stages,retainallcorrectnesssnapshots,reportfullpresubmitcost separately; donot tuneinputscheduler to manufacturepass. All47remainunqualified; originalGPUbudget2 restored.

- Post-draw main-stage diagnostics now wired narrowly around profilercommit,nativeframe snapshot/log,debugoutlines,damage/markdrawn,a11y,HUD/request,andnativeredrawlog; singleextra rowafterchannelsend. Existingoptin gate anddisablednoclock behavior preserved. RustbehavioralRED2expectedfail/5pass; featuregreen17759currentlycompiling (agentframe_evidence). Parentstrictparser+collector5newchecks andexisting87 passed; reviewcaught drainrace wherepresentation precedespostsendlogging, reproduced thenfixed boundeddiagnosticdrain, combined93pass. Existing3captures correctlymain-stagesunavailable. Parserreviewfollowup pending. SavedPythonrunner+hashes andoptimizedbuild92724nowlive at target/runtime-proof/main-stages/. No nativecapture yet; no newperformanceclaim. NoGUIhold active. Goal/all47unqualified unchanged.

- Producerfeaturetests17759terminal0:11pass(new3+prior8), no followupsourcechanges. Build92724stilllive onlastpoll. Nativevalidationpending; donotrestart buildonobservationtimeout.

- Main-stagebuild92724terminal0, SHA6f2d5caab18c8b9a15d4adce42ab680e3710b10ef07e592300d27262e4f530d8.1144knownsourceschecked; changesntfshelperlimits/reader,Historyhosttests,Chromiumtests. Frozen35Pythonrunnerhashesverified. Captures19629/23394terminal0:azaxem6g/t8p2snbi both1260effects+completepositivefeedback+mainstages+hostlatency;1199/1094measuredframes. CPU p992.916/2.544ms;displayp9925.000/41.667ms(max33.333/449.999). Postdraw p992.323/6.378ms,max11.037/20.439; native snapshot/log dominates thatregion,damage/a11y tiny. Prior218ms postdrawstall didnotrecur. Repeat449ms spanscoalesced695–722,donotinterpretlastinputinterval463ms asonesenderpause. Comparisonmain-stages/comparison.json;gapdecompositionupdated. Drainfixindependentlyreviewedclear. AllGUIholdsreleased,no parentbuild/capture. Agentframe_evidence readonly investigation of directJSONformatting vs serializedsingle-rowwrite; no causalclaim orproductionchangeyet. All47unqualified,no sharedplateau.

- Next sharedhotspot quantified fromlatestnative measuredframes: resource_availability consumes2196.763/2525.471ms(87.0%) and1938.874/2223.413ms(87.2%) ofCPUframebuild; draw289.676/251.768ms. Scopeexcludespostdraw/input/GPU, notwholeframe. Artifactcontrol-presence/current-cost.json. Currentpresencecheck atApp refreshcontrolavailability buildsfullcontrolinventory; explicitcontrol_action_coverage_for isseparateaudit andmustremainfull. Readonlyaudit confirmsFiles→NavShell→Column→Scrollable→FileView pathneedsauditedoverrides withconservativeWidgetdefaultfallback preservingcustomownemitters. No productionfastpathimplemented yet. Loggingwritecountdiagnostic andfinalminimaltestscope inprogress onagents; noparentbuild/GUI.

- Currenttree controlpresence designaudit completed. ConservativeWidgetdefault fullinventory.any(Action) preservescustomownemitters; auditedfastpaths requiredalongFilesroot→NavShell→Column→Scrollable→FileView,Buttonchecksownemitter. Modal/collaborationexposure mirrors existingfor_each_child; boundwrappersretainfallback/admissionguard. Explicitcoverageaudit remainsfull. Approvedtests-firstimplementation byparagraph_reuse_review withcustomemitterpermissionrevocation,nestedcontainers,modalexposure,realFilesa11ybuildcounter,subtreereplacement/stampreset regressions. Nooptimizedbuild/GUI authorizedatthisstage. Diagnosticwritecountinvestigation remainsreadonlypendingevidence.

- Controlpresencecandidate implemented conservativedefault+auditedButton/Column/Scrollable/NavShell/Filesroot/FileView overrides; onlyApprefreshpresencequeryreplaced, explicitcoverage/bounddiscovery/permission logicunchanged. BehavioralRED host2inventoriesvs0 andrealFiles8a11ybuildsvs0; fixedcandidatefocusedUI4/Files2green. Custominventoryonlyemitters/nestedcontainers/permissionrevocation/modalexposure/replacementstamp/explicitcoverage covered. Temporarymodalfixture()Action compileerror corrected withoperationbinding, Browser/Historyownersnotified. Broaderactionpalettetests30081running, independentframe_evidencereviewpending. Optimizedbuild49469nowlive control-presence/build_candidate.py, baselinePythonrunnercopied+hashed; loggingunchangedforcomparison. NoGUIcapture yet.
- Readonlyloggingprobe proven exactrecordbytesincludingnewline: directValueformatting frame215/mainstages113/display41writer calls, pre-serializedbody3, completelinewrite_all1 each. Artifactslog-write-counts/results.jsonl,count.rs,findings.md,provenance.json. Currentstdstderr holdslockacrossformatting butforwardstinyfragmentsunbuffered; syscallcount qualification/partialwrite caveats documented. Doesnotprove449ms cause. Deferproductionloggingchange untilcontrolpresencecomparisonsoeffectsnotconflated. All47remainunqualified,nosharedplateau.

- Controlpresenceverificationcomplete: UIaction15+focused4,Filesfullserial244+focused2 pass. InitialparallelFiles236pass/8fail dueprocessglobalambientguard; serialrerun62355terminal0 preservedalllogs. Independentreviewclear(customemitters,modalvisibility,admission,permissions,audits). Build49469terminal0,savedSHA4e30a6d42acfb36babac83979a19b8af154424641adf1f3be48c1da7c68b5fa2;1146knownsourceschecked,onlyFilesnative_proof_tests changed(test-only), notexhaustiveproof.
- Two nativecontrolpresencecaptures68523/52718terminal0:dsewokz8/ago6gvst validateall1260effects+positivepresentation+host/mainstageevidence. CPU p991.289/1.412ms versusbaseline2.916/2.544; meanresource0.917/0.940ms versus1.832/1.772. RetainrepeatableCPUgain. Displayp9925.000/41.667ms remainsfailed; no all-appFPSclaim. Savedcontrol-presence/comparison.json+compare.py. GUIholdreleased,noparentbuild/capture. Sharedframeworkplateaunotestablished,all47unqualified.
- Separateloggingoptimization nowdelegatedtoframe_evidence: completenativeJSONline serializedbeforestderrlock andwrite_all, preservesbytes/fields/timestamporder andexistingpanicvsignoreerrors; partialwrite/Interrupted/error testsrequired. No asyncqueue/droppedrecords; controlcandidatealreadysaved so diagnosticchange willbeseparatecomparison. No optimizedloggingbuildyet.

- frame_evidenceloggingimplementation stoppedmodelcapacityafterstandaloneRED; parentinspectedexistingartifacts andcompletedlocally. Nativehelper serializescompletelinebeforestderrlock,write_all; App10JSONsites+presentation4+callbackconverted. Schemas/gates/perthreadtimestamporder/errorpoliciespreserved,noasyncqueue/drops. SixstandaloneGREEN andintegratedfeature6GREEN93083exit0; nofeatureproductioncheck34324exit0. Independentreviewclear; libteststderr-capture distinction documented native-log/design.md. Optimizedcandidatebuild85620live(native-log/build_candidate.py), exactbaselinePythonrunnercopied+hashed; noGUIcaptureyet. Mustpollsamebuildhandle, thenmeasuretwicebeforetimingclaims. All47unqualified,noframeworkplateau.

- Loggingbuild85620terminal0,savedSHA53280c26aa57dc08c3bc1ce791c9492f4661bc5869ece78f586f000fbc95f2f2;1147knownsourceschecked, concurrentntfshelpertest/broker andTileHost changesnotedmanifest. Capture1670terminal1,cjakfa4q: effectdeliverytimeout,nofirstpresent/noinputs,all3310renderattemptsSkipped/occluded. PostfailureIOConsoleUsersreadback ScreenIsLocked=Yes; no unlockattempt. Savednative-log/failed-capture-summary.json/rawlogs; GUIholdreleased,noparentbuild/capture. Nativecomparisonpendinguntildesktopunlocked, do notrerunidenticalcapturewhilelocked. Sourceinspectionrender_retry_deadline schedulesallSkipped/Reconfigured16.667ms; persistentocclusion yields~55attempts/sec. Nextsourceinvestigation canreduceoccludedretrywork preservingfirstpresent/reconfigure/inputrecovery. Overallgoalnotblocked:meaningfulsourceworkavailable,all47unqualified.

- Desktoplockflagabsent onfreshcheck; coordinatedsavedloggingcapture46096terminal0,p6jrngre verifies1260effects/1200measuredframes/completefeedback+host/mainstageevidence. Executable+35runnerhashesverified. CPU p993.914ms,displayp9925.000ms,inputlatencyp9934.468ms. Nativeframelogmedian0.140ms,postdrawp990.405ms(singlevalidrun only). Repeat77593terminal1,758_j4ok:all1260effects/1199measuredframes but latencygate correctlyrejects warmupCPU4zero presentedTime;1260mappedframes/1259positive,nocallbackmissing. Preservefailure;donotwaivewarmupgateorclaimrepeatablebenefit. Savednative-log/unlocked-summary.json. GUIholdreleased,noliveparentbuild/capture; Historyownerinformedvisibilitynowpermitscapture. All47unqualified,goalactive.

- Occlusionbackoff workstarted aftercurrentAppinspection/Historycoordination(no schedulingownershipoverlap). Agentparagraph_reuse_review ownsRustpolicy/per-surfaceintegration+REDtests; parentPythonoutcomesupport done. BehavioralRED showedmissingoccludedfraction/invalidauxoutcome, then107PythonchecksGREEN. Occluded contributes skipped_fraction andseparateoccluded_fraction; expectedmeasuredframesstillPresented; occluded-onlywork cannotpasspresentationstats. NoRust/nativebackoffgainclaimed yet. Coverageinventoryrechecked, all-appqualificationstillmissing. No parentGUI/buildactive.

- Occlusionpolicy/wakeselection behavioralRED preserved: policy-red2.log provesmissinggrowingdelay+renderdemandgate; wake-red.log provesselectedWait16.667msinstead ofblocked250ms evenafterpolicypasses. Parentreviewflaggedactualnext_wake_deadline filesystem/statussync contract, notonlyanimation; agentaddingrealdeadline/admissionintegration regressions. No nativebackoffresult or completedRustintegration claimed. Agent stillowns narrowRustpaths, no parentGUI/build.

- Occlusioninitialintegration compiles (69137 integrated.log terminal1legacytestpass), standalonepolicy+waitselector6GREEN. RealBufferPool/prepare_frame regression added toretainpayload/probelatchacrossbackpressure; finalnewtestcoveragepending. Parentreviewfoundnormalinputhook unconditionallyrequestingredrawandtypingfadeclassifiedmodeldemand: askednarrowrecovery-onlyforcedredrawandrender-onlytypinggate regressions. Independentframe_evidencereviewnowactive; explicitlytoldnotresumeoldloggingtask(alreadycompleted). Nooptimizedbackoffbuild/nativecapture yet. NoGUIhold.

- OcclusionfocusedCargo51272terminal0:4tests pass(eventclassification,policyreset/growth,realBufferPool/probelatch/payloadadmission). Standalone7green. Wider render-thread/startup suites pending. FreshAppinitializer includes typingfield aftertransientpeercompilefailure; Historyowner notifiedcoherentcheckpoint. Independentreviewblockednativebuild: primaryretrygateprecedesroot.update/sync butvisibleauxhandlers onlydraw, so siblingstatecanlag250ms despite60Hzdrawcadence. Implementeraddingactualsharedprojectionregression+fix; no buildclearanceyet. Preparedocclusion-backoff/build_candidate.py andPythonrunnersnapshot only, notlaunched. NoGUIhold.

- Occlusion sibling regression reproduced stale retained content at the normal frame deadline (sibling-red2.log). Candidate now caps primary retries at one frame while an auxiliary surface is not known occluded, retains occlusion history across admission, and probes primary on explicit auxiliary input. Default render-thread suite: 21 passed; standalone policy: 8 passed. Independent follow-up review and feature suites remain pending. Optimized build started with handle 49905; no native capture or GUI hold yet. This is correctness/idle-efficiency progress, not evidence of displayed 60fps. All 47 apps remain unqualified.

- Independent follow-up source review found no remaining occlusion sibling blocker. Feature occlusion suite passed 5 tests; feature render-thread suite remains queued. Actual native restore behavior and idle retry-rate reduction are still unmeasured. Optimized build handle 49905 was re-polled and remained live; do not restart it because an observation times out.

- Occlusion candidate build 49905 completed successfully; saved binary SHA 2a6010d50d49067abb02ba1abe6aefc25bb2f4a950c671bb23cf56fb7862c153. Known source manifest covers 1,179 files; retry/App/render-thread remained stable, concurrent rich-text/TileHost and test changes recorded. Default and presentation-feedback render-thread suites each passed 21 tests; feature occlusion 5, startup 4, policy 8 passed. Saved 35 runner hashes verified.
- Native captures 84739/81840 both terminal success: gmw1ir27 and jmrwywam validate all 1,260 effects, with 1,178/1,192 measured frames and complete positive presentation/latency evidence. CPU p99 1.795/1.977 ms, displayed interval p99 33.333/33.333 ms, input-to-display p99 81.909/58.626 ms. Target still fails; visible workload does not establish hidden retry savings or restore behavior. One startup Occluded in first capture recovered. GUI hold released; no parent build/capture remains live. Summary under occlusion-backoff/capture-summary.json.
- Reproducible cadence-phase analysis of earlier p6jrngre separates numeric two-refresh deviations from descriptive refresh-slot tails without changing acceptance. A 22 ms send-to-worker interval occurs after prior measured render body ended. Unmeasured worker logging/recycle/outcome publication versus OS wake delay remains unresolved; proposed next diagnostic is narrow opt-in worker epilogue and pre-receive brackets. All 47 apps remain unqualified; shared plateau not established.

- Dedicated restore diagnostic completed (handle57829 exit0, childSIGTERM normal cleanup). Native minimize AX action/shortcut did not establish sustained occlusion; 3 transient Occluded outcomes followed by presentation do not prove hidden retry-rate savings or restoration. Raw evidence and limitation saved in occlusion-backoff/restore-probe; GUI hold released.
- Next measured gap investigation: agent implementing opt-in worker handoff/previous epilogue timestamps in narrow Rust paths. Parent added strict worker_handoff parser, regression tests, and collector drain/report integration; RED imports/drain behavior preserved, then 69 combined Python tests passed (59876 exit0). Rust implementation and review pending; no optimized worker-handoff build or native capture yet. Preserve last outgoing epilogue as unavailable without a successor; no GPU or pure OS-latency inference.
- User clarified proposed MCP runtime design must follow YAGNI, maximize shared core, and keep platform-specific code at edges. No MCP runtime implementation started; performance objective remains active.

- Worker-handoff producer/parser independent review clear: schema, explicit null auxiliary identities, exact copied brackets, previous-frame continuity, disabled clocks/JSON, shutdown and failed-resize cleanup checked. Added an auxiliary drain behavioral RED and fix; 70 Python tests pass. Rust handoff focused3 green observed; broader agent suites/checkpoint pending. Optimized build handle5729 started from coherent source; runner snapshot/hash and build identity script prepared under worker-handoff. No GUI/native capture active. No performance gain or all-app qualification claimed.
