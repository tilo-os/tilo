# Native scheduled input diagnostics

`TILO_NATIVE_SCENARIO=/absolute/path/scenario.json` enables a shared native input
timeline in `App::run_with_setup`. Enable `TILO_PROFILE_OUT` to retain CPU frames
and `TILO_UI_PRESENT=1` to retain worker outcomes. The scheduler starts after
the first primary worker `Presented` outcome, uses absolute deadlines, and
never moves future deadlines to accommodate a late callback.

```json
{
  "name": "button delivery",
  "interval_us": 16667,
  "warmup_steps": 1,
  "steps": [
    {"kind": "hover", "x": 100.0, "y": 30.0},
    {"kind": "click", "x": 100.0, "y": 30.0}
  ]
}
```

Coordinates are physical window coordinates. The current typed subset is hover,
click, wheel line deltas (`dx`, `dy`), Unicode character input
(`{"kind":"character","value":"é"}`), and named keys
(`{"kind":"key","key":"backspace"}`), routed through existing native event
handlers. Each keyboard step emits a press/release pair using the constructor
shared with headless driving. Character steps contain exactly one Unicode scalar;
unknown key names and extra fields are rejected. Pixel-wheel phases, resize, named targeting, semantic/pixel
assertions and app fixture preparation remain required for the full suite.
Unknown fields, empty timelines, non-finite input, invalid warmup, over100000
steps and overflowing deadlines fail before scheduling. Without the environment
variable, no scheduling thread or native input logging is installed.

`tilo-native-input` records input IDs, requested deadlines, event-handler start
times, scheduler-send timestamps, and lateness relative to those deadlines.
The report separates `scheduler_lateness` (deadline to send) from
`delivery_to_dispatch` (send to handler entry); their percentiles must not be
added. Old evidence without send timestamps leaves both fields unavailable;
partially populated or chronologically invalid timestamps fail validation.
It records dispatch entry,
not input-to-photon latency. `tilo-native-frame` maps all inputs since the previous
CPU frame to the next committed CPU sample. Several inputs may share one frame;
they must not be counted as several rendered frames. Inputs that fail to produce
a frame remain missing in the evidence. Mixed warmup/measured frame batches are
excluded from measured CPU statistics.

`scripts/perf/native_delivery.py` validates input coverage, ordering, payloads,
timelines, frame mapping and numeric timing fields. It separates full-stream
CPU/worker correlation from the subset associated with measured inputs. Its
report explicitly does not certify effects or FPS; fixtures must independently
assert effects. Optional macOS drawable presentation evidence is documented in
[native-presentation-feedback.md](native-presentation-feedback.md).

## Native delivery probe

Build `cargo build --profile thin-app -p ui-examples --no-default-features --example
ui-native-input-probe`. The example uses an ordinary button callback to emit
`tilo-native-effect` activation counts. It runs until closed by its test harness
or the user. Repeated clicks verify input routing but do not represent sustained
visual animation or a substantive application workload.

Evidence in `target/runtime-proof/native-input-20260916-101949/` retains the
scenario, raw CPU/worker/input/effect logs, executable hash and report. At16667µs
intervals,1260scheduled clicks produced1260independent callback activations.
After60warmup inputs,1200input-associated CPU frames all matched Presented
worker outcomes. The full stream contains1269Presented and1Skipped outcome;
the skip was outside the measured-input subset. One warmup batch coalesced
inputs. Handler-entry lateness was3.260ms median and4.162ms p99. The trivial
button's CPU timing is not an app performance qualification.

Five Rust scheduler tests,24Python statistics/coverage/delivery tests, and an
independent source review support this delivery implementation. All47apps still
need representative native workloads, effect assertions and display evidence.

## Verified native scrolling

Run the same example with `--scroll` to construct a 200-row list using the real
`Column`, `Text`, and `Scrollable` widgets. A fixture-owned read-only observer
samples the existing persisted scroll offset before and after each scheduled
input. `validate_scroll_effects` rejects missing/reordered snapshots, non-finite
or malformed offsets, and wheel inputs that do not move in the expected direction.
Use an initial hover inside the viewport and reverse wheel direction periodically
to avoid reaching the content boundary. Include the hover in the warmup count.

The observer is optional and has no callback or logging cost for ordinary apps.
Its snapshot allocation and logging occur outside ordinary event-handler timing;
scheduling lateness still includes delays from earlier observer work. Verified
state changes are not pixel assertions. Multiple inputs can coalesce or reverse
before rendering, so the movement count must not be interpreted as a count of
visibly changed frames. CPU/frame/worker correlation remains required, and worker
`Presented` still does not establish actual display cadence.

Six Rust scheduler/probe tests and 25 Python reporting/coverage tests pass.
Independent review found no blocker for widget-state diagnostics; malformed
snapshot handling was subsequently hardened with a regression test.

Optimized native scrolling evidence (same executable SHA-256
`1d049fe9f51ec140ae1da9f2ee0099e539b3d0823260f0115a7680dccbec95d3`):

| Run under `target/runtime-proof/` | Verified wheel steps | Measured CPU frames | CPU p99 | Input lateness p99 |
| --- | ---: | ---: | ---: | ---: |
| `native-scroll-20260916-103252` | 1260 | 1200 | 10.201 ms | 9.000 ms |
| `native-scroll-20260916-103331` | 1260 | 1198 | 10.540 ms | 10.649 ms |

Both exclude the initial hover and 60 wheel warmups. Every measured CPU frame
matches a worker `Presented` outcome. The repeat has two coalesced input batches
and a 31.285 ms maximum CPU frame; preserve these tails. Each full stream includes
one startup skip and two unmatched CPU samples at collector termination. The
first run's scene-pool wait p99 is 9.972 ms, dominating the 10.201 ms total;
`phase-report.json` retains all phase distributions. These are separate
percentiles and must not be added. Next shared bottleneck investigation is the
blocking scene-pool receive and redraw scheduling, not list layout.

The initial `native-scroll-20260916-103223` attempt failed in the collector when
it parsed a partially written final log line; its evidence is retained. The
collector now polls only complete lines and validates the full log after exit.
No app or displayed-FPS qualification follows from these diagnostic runs.

## Render admission experiment

Primary redraw now checks for a reusable scene/damage pair before palette,
model, sync, layout, or dirty-state consumption. If the finite pool is busy,
it arms a completion wake and returns to the event loop. The worker publishes
both buffers atomically and its outcome before requesting the deferred redraw.
A reserved pair remains cached across layout aborts. Periodic primary redraw
demand is gated while deferred; failed worker outcomes wake even without a
waiter. The two-buffer pool and capacity-one request queue are unchanged.

This moves GPU backpressure outside the CPU frame sample. A lower CPU percentile
alone is therefore not an optimization result: compare scheduled input lateness,
coalescing, worker outcomes and cadence as well. The request-channel send may
still briefly wait for worker dequeue. Worker timings remain API timings, not
physical display feedback. Existing startup pumping remains, gated by admission.

The first admission attempt (`native-scroll-20260916-104713`) stalled after
approximately 3 seconds. `target/runtime-proof/admission-hang.sample.txt` records
the main-thread semaphore wait and render-worker synchronous dispatch to main.
Vendored winit's macOS `maybe_queue_on_main` calls `maybe_wait_on_main`, so a
worker `Window::request_redraw` can deadlock with UI submission backpressure.
The replacement callback sends `UserEvent::RenderReady(window_id)` through the
asynchronous event-loop proxy; only the UI handler calls `request_redraw` and
routes it to the corresponding primary, floating, or audience window. Removed
windows ignore stale notifications. Four render-thread tests pass and source
review found no blocker. Native reruns must validate the runtime fix.

The proxy attempt `native-scroll-20260916-105519` completed but was occluded:
64,204 skipped outcomes, 34 coalesced measured inputs, and 366.163 ms input p99
make it a failed performance run. The healthy repeat
`native-scroll-20260916-105547` verified all 1260 movements and 1200 measured
Presented outcomes, with 4.168 ms input p99 and 0.380 ms CPU p99. Preserve both.
The CPU decrease partly reflects removal of recorded pool waiting; dispatch
lateness is the separate responsiveness measurement.

Occlusion exposed an immediate-retry loop. Primary, audience, and floating
windows now delay Skipped/Reconfigured retries by one 60 Hz interval, preserve
pending state, and include the retry deadline in event-loop wake scheduling.
Floating buffer acquisition is also nonblocking. SurfaceGone remains terminal.
The retry unit test passes and review found no blocker; native pacing validation
is still required, including eventual auxiliary-window occlusion/recovery.

Final admission/pacing build SHA-256:
`725e9a36832ec5bcd168031e282c767db0b7c915162a1830104b16b104f82543`.

| Run | Measured inputs / frames | Measured Presented | Input p99 | CPU p99 | Worker completion gap p99 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `native-scroll-20260916-110113` | 1200 / 1200 | 1200 | 4.165 ms | 0.199 ms | 8.980 ms |
| `native-scroll-20260916-110140` | 1200 / 1200 | 1200 | 4.168 ms | 0.163 ms | 16.494 ms |

Both independently verify all 1260 wheel movements, have no coalesced batches,
and preserve one startup skip plus two unmatched shutdown CPU samples. The
repeat's worker completion maximum is 20.601 ms and input lateness maximum is
12.054 ms. Worker-gap distributions include animation frames between measured
inputs and do not measure display presentation. Input dispatch improved versus
the 9.000/10.649 ms p99 baselines; no display-cadence improvement is claimed.
Concurrent worktree changes and host activity limit attribution to an isolated
source patch. Raw phase/cadence reports and executable hashes are retained.

`native-idle-20260916-110210` remains at five worker frames across a two-second
idle observation after settling, ruling out an unconditional completion redraw
loop in the static probe. Off-screen placement (`native-occlusion-20260916-110218`)
and manual minimization of an identical bundled executable (`...-110326`) did
not reproduce sustained skipped outcomes. Those occlusion checks are explicitly
inconclusive; do not claim live retry-rate qualification from them. The bundle
had 202 frames, only one skipped, and stopped rendering after minimization.
The unbundled UI-control attempt (`...-110252`) was also inconclusive. All owned
probe processes have exited. Auxiliary occlusion/recovery remains untested.


## Word Processor native workload preparation

The shared root-widget observer defaults to no snapshot. An explicit App probe
still takes precedence; otherwise opt-in native scenario input samples the root
widget before/after delivery. There is no recursive traversal or ordinary-input
observation work. Word Processor exposes its actual scroll, focus, selection and
caret state. `caret_text` is the current body paragraph only; header/footer text
is omitted to avoid materializing an entire running region. Diagnostic logs can
contain document text, so run these workloads against controlled fixture files.

`scripts/perf/word_processor_effects.py` validates exact single-character and
backspace transitions, body focus, unchanged block identity, no selection,
complete ordered evidence and continuity between edits. This is state evidence,
not proof that pixels reached the display. Snapshot allocation and text logging
are diagnostic overhead; fixture paragraph sizes must remain representative.
Seven native scheduler tests and 34 Python reporting tests pass. Native app
measurements are still required before any Word Processor qualification.


The first optimized 200-paragraph typing workload failed:
`target/runtime-proof/wp-native-typing-20260916-123158`. It requested1260
alternating character/backspace steps at16667µs intervals, with60warmups,
but timed out after70seconds. Only439inputs entered dispatch;438completed
edits pass exact text/caret validation. The last input was41873.501ms late.
Adjacent dispatch-entry gaps have129.591ms p99 (not frame time or independent
handler duration). This is a real-app responsiveness failure, not qualification.
The original executable hash is
`1a659addb99ad6e7ceaed7e16a11f4ae2e5f8372bbeca199ec02e227338a0632`.
A sampled diagnostic repeat (`wp-native-typing-20260916-124231`) also timed out;
its stripped stack remains retained pending a symbol-bearing diagnostic repeat.

The default observer, root fallback, explicit-probe precedence and actual
Word Processor read-only snapshot tests pass (2sharedUItests,1WPtest).
The `tilo-app` facade forwards the optional `presentation-feedback` feature so
apps can enable diagnostics using their existing `ui` dependency alias.


A symbol-bearing optimized diagnostic (`wp-native-typing-20260916-124621`)
retains a3second main-thread sample. It deliberately uses120steps to locate
costs, not to qualify performance. All120edits pass exact state validation,
but CPU mappings mix warmup/measured input, so the normal report correctly
refuses to emit a measured CPU distribution. Raw input/frame/effect logs and
`diagnostic-report.json` remain available.

The sample has2168main-thread event-loop samples. Two sibling stacks under
`refresh_control_action_availability` account for747+758=1505samples (~69.4%).
Both lead through Word Processor resource generation and full qdoc
materialization. `refresh_blocks` accounts for307samples and
`insert_text_via_action` for272samples in the press-side branch. These are
inclusive sampled stacks, not independently measured per-event durations.
The largest observed candidate is shared pre-input resource rebuilding; do not
skip authority refresh or release events without preserving permission changes,
resource invalidation and activation semantics. App-local validation and block
refresh remain subsequent candidates, not grounds for declaring shared stall.


## Shared control-availability fast path

Pre-input refresh still discovers and refreshes bound components first. It now
consults the retained control-binding inventory before building the unbound
permission-filtered catalog: only `ControlBindingKind::Action` consumers need
that projection. Semantic, collaborative-document and legacy controls retain
their existing invocation paths. Explicit resource discovery is unchanged.
The inventory is checked each refresh; skipped projection clears its old stamp
so a later replacement emitter is initialized. Custom emitters must continue
to expose metadata as required by the existing Widget contract.

The no-action regression failed with3unnecessary materializations, then passed
with none and successful on-demand discovery. All29control-related tests pass,
including permission revocation/restoration and bound authority refresh without
an outer generation change. Independent review found no blocker after requiring
stamp reset. Native performance comparison remains necessary; the inventory
itself can construct accessibility state, so this is not assumed free.


Optimized fast-path executable SHA-256:
`d5c46770eadc5a7073662b942f5b54c82b50f52f12308b1d33bc71823364de60`.
Both full200paragraph/1260input repeats complete exact edit validation:

| Run | Dispatch-entry gap p99 | Mean gap | Last input lateness |
| --- | ---: | ---: | ---: |
| `wp-native-typing-20260916-125153` |52.094ms|35.744ms|24017.572ms|
| `wp-native-typing-20260916-125321` |50.889ms|35.224ms|23362.994ms|

The prior failed baseline had129.591ms p99 adjacent dispatch gaps and completed
only438edits before timeout. This is faster input processing, not a frame-time
claim: both new runs coalesce ALL1260inputs into one CPU frame. Normal reports
reject that mixed warmup/measured batch. Qualification remains failed.
The repeat overlapped an additional test compilation, recorded in its diagnostic
report; concurrent worktree and host activity limit controlled attribution.

The short sampled repeat `wp-native-typing-20260916-125301` shows only6samples
under control-availability refresh, compared with1505before. Of~2153main-thread
event samples,1000are under `refresh_blocks` and870under
`insert_text_via_action`. Thus the next measured costs for this workload are
app-local block materialization and edit validation. This establishes neither
all-widget-family shared stall nor all-app qualification.

The additional `app::action_palette_tests` attempt could not compile because
concurrently added Chromium tests reference `NavigationDirection` and `direction`
fields not yet present in webview types. Preserve those edits; the earlier29
control tests and optimized comparison builds passed before that concurrent
change. Log: `target/runtime-proof/control-refresh-action-tests.log`.


## Targeted Word Processor splice validation

After the shared control refresh reduction, text action validation remained a
measured cost. `SpliceText` now resolves only its region and block, verifies the
same byte-valued order membership as qdoc's snapshot, and counts that block's
Unicode characters. It retains empty text for missing/non-Text fields, region
fallback behavior, deletion bounds and the one-million-character insertion cap.
Other action validation and ID-generator seeding are unchanged.

The regression first failed because validation advanced the ordered-region scan
counter6→10. The new implementation matches the full-materialize oracle without
region scans across body/header/footer, malformed block/order/text, Unicode and
out-of-range offsets, oversized insertions and atomic refusal. All21action tests
pass. Independent source review found no blocking semantic or security issue.
The previously blocked shared action-palette regression was also rerun after
the concurrent Chromium definitions landed:8tests pass.
Native performance comparison is pending; this change alone does not qualify
Word Processor or establish a shared-layer stall across all widget families.


Optimized targeted-splice executable SHA-256
`7b891098ac101e1b3c9a2d4ba5bbb8ea8c528eb1ec5dbebd47c283347c22d28c`:

| Full200paragraph/1260input run | Mean dispatch gap | Dispatch-gap p99 | Last input lateness |
| --- | ---: | ---: | ---: |
| `wp-native-typing-20260916-130016` |22.673ms|55.818ms|7561.167ms|
| `wp-native-typing-20260916-130109` |21.761ms|53.081ms|6413.663ms|

All1260edits are independently verified in both runs. Mean processing spacing
improves from the prior35.2–35.7ms, but tail spacing remains above50ms. The first
run coalesces1260inputs into one frame; the repeat maps one initial input then
1259to one frame. Both fail native frame qualification. These gaps are input
processing diagnostics, not frame times. The first also contains a cluster of
114–130input stalls; its cause is not isolated from concurrent host activity.

The sampled120step diagnostic `wp-native-typing-20260916-130150` retains1202
inclusive samples under `refresh_blocks`,40under `insert_text_via_action`, and
106under `note_edit_for_autosave` (105of those in Markdown export). Slow gaps
also align with the24edit recovery cadence. Block materialization is the next
regular cost; recovery export remains a periodic tail candidate. Preserve
recovery functionality and full snapshot semantics when optimizing either.
No displayed60fps qualification follows from these processing improvements.


## Retained body views for known text-only edits

Word Processor now keeps unchanged body block views for ordinary alphanumeric
text insertion and within-block Backspace. The hint is issued only when the
pre-edit CRDT heads match the retained snapshot, the caret is in a retained body
block, there is no selection, and Backspace is not undoing autocorrection.
Unknown/structural operations, stale revisions and shifted virtual windows use
the full path. The edited block is rematerialized from qdoc; blobs, running
regions, notes and review metadata still refresh. Cached heads advance after
materialization. Action dispatch, undo, autosave cadence and dirty flags remain.

The regression first failed because an untouched block view was rebuilt. It
now verifies retention plus exact equality with full materialization after
Unicode insert/Backspace, another-block pending change, Enter, structural
Backspace and cross-block replacement. A second test verifies that a shifted
virtual window takes the full path. Both pass and source review found no blocker.

Optimized executable SHA-256
`a4e269ae4966a6e38dc935e408eefca0212b2c5b4fa1805a0f5f59495978985e`:

- `wp-native-typing-20260916-130935`: all1260edits verified;1200measuredinputs
  map to1133frames with complete positive display feedback. CPU build p99
  is4.543ms; input-associated presentation interval p99 is50.000ms. A recovery
  test compilation overlapped this diagnostic run. It fails60fps qualification.
- `wp-native-typing-20260916-131028`: all edits verified,1088measuredCPUframes,
  CPU p994.995ms;74worker skips and missing presentation requests. Failed
  display evidence; retain `failed-presentation-report.json`.
- `wp-native-typing-20260916-131112`: all edits verified,1006measuredCPUframes,
  CPU p995.734ms;266worker skips and missing presentation requests. Failed
  display evidence; do not infer cadence or an unverified cause for the skips.

Full-library testing also exposed a recovery identity bug: saving a new file
under macOS's /var alias canonicalized it to /private/var, changing the recovery
filename hash. Resolving existing ancestors before hashing a missing target
preserves identity; the new regression failed before the fix, and all10recovery
tests now pass including autosave cleanup. This separate fix is not included
in the executable above and does not change recovery frequency/export behavior.

Two pagination fixtures also failed: their400-word paragraph shaped to900.900px,
less than the917.176px content height. Enlarging those fixtures to800words makes
them exercise the intended page split rather than assuming host font metrics.
These test-only changes do not change production pagination.


Final broad test rerun after the recovery/fixture fixes could not compile due to
concurrent webview changes: missing `base64` dependency in Chromium and an
unhandled `WebViewCommand::SetAudioMuted` arm in the test engine. Log:
`target/runtime-proof/block-refresh-final-tests.log`. Do not claim full-suite
success: the2focused block-refresh tests and10recovery tests passed earlier;
the enlarged pagination fixtures still require execution once the shared UI
source compiles. Those unrelated edits were preserved.

### Recovery snapshot view reuse (2026-09-16)

The shared Markdown codec now exposes `export_views`, borrowing complete body
views, notes and properties through the same renderer as canonical export.
Its23Markdown tests pass, including front matter, Unicode marks, tables, code,
footnotes, lists and empty documents. Word Processor refreshes its edited view
before counting the recovery edit. At the existing24-edit threshold it reuses
views only when committed heads match, the window starts at zero, and every
body ID matches the complete ordered body index. Stale or virtualized caches
fall back to canonical export; recovery remains synchronous at the same cadence.

The new regression failed before the integration because a fresh complete cache
still caused4full container scans. It now passes without those scans and checks
byte equality for fresh, pending-edit and768-block virtualized documents. The
keyboard autosave test checks exact bytes including the threshold keystroke.
A recovery identity test also needed the existing environment mutex: another
parallel test could change the recovery base directory between comparisons.

The previous shared UI compile errors have been resolved in the current tree.
`target/runtime-proof/recovery-views-final-full.log` records **277passed,0failed**,
including the enlarged pagination fixtures and all recovery tests. This
supersedes the earlier pending full-suite status; it is not display qualification.

Native recovery-view executable SHA-256:
`d81d69b788edf0e39472ffeb85b860f6e8c121a319f450412577b4a3c884a7dd`.
Source review found no correctness blockers.

- `wp-native-typing-20260916-132314`:1260exact edits verified;1115measured
  CPUframes, CPU build p994.415ms, delivery-to-dispatch p993.683ms.
  Dispatch-entry gaps max22.291ms (diagnostic, not frame times); the earlier
  periodic >40ms processing gaps did not recur in this run.506worker skips
  explicitly report `occluded`, including226input-associated skips. Missing
  display requests fail qualification. A process check during the run found
  no rustc/cargo-clippy process; the desktop remains uncontrolled.
- `wp-native-typing-20260916-132402`:1200measuredCPUframes with complete
  positive input-associated display feedback; interval p9925.000ms,
  max33.333ms. However, exact editing continuity failed: unrequested characters
  appeared between injected inputs (first mismatch between input3and4).
  This run fails workload validation and cannot qualify performance.

Both directories retain `failed-presentation-report.json`, raw events and
binary identities. The strict validators were not relaxed. Prior skipped runs
131028and131112also explicitly logged74and266`occluded`surface results.
The next measurement work should address native window/focus handling and
unsolicited input without filtering away real application errors. CUA could
not select the unbundled executable by its path or process name in this session.
No all-app qualification or shared-layer stall is established.

### Nonactivating benchmark window and virtualized scrolling (2026-09-16)

`TILO_PROFILE_WINDOW_NO_ACTIVATE=1` opts the macOS event-loop builder out of
activating over another application. Normal launches are unchanged. The
collector uses this option and can select a binary with `WP_BINARY`. A local
Writer Performance Probe.app wrapper allows CUA inspection of the same binary.
The first bundled executable hash is
`6fbf9a9425a197681b8e1e9b5d4f661263ab75ac856f0ae40984a17b3ff2328c`.

- Typing132836:1260edits verified, complete feedback but only446measuredframes;
  input-associated interval p9975ms and delivery delay p9951.723ms. CUA read
  the accessibility tree during this run (the query took17.5s). Keep this as
  observed concurrent-accessibility workload evidence, not a clean baseline.
- Typing132926:1260edits verified,1200measuredframes, complete feedback;
  CPU p994.422ms, input delivery p992.239ms; input-associated display interval
  p9916.666750ms (Metal quantization), max25ms. All active-interval frames
  p998.333417ms. One clean run is not full app qualification or pixel proof.
- Large scroll132956:768paragraphs,1260wheels reversing every360events. It
  reached max_scroll46083px at step322, failing the moving-input validator.
  CPU p9928.352ms; high frames spend approximately24.6ms in sync. Retain raw
  logs and failed-workload-report.json; no qualification. A screenshot attempt
  after this process exited caused CUA to open a new empty app; that screenshot
  is not evidence about the workload. The empty inspection app was closed.

The previous200paragraph scroll case moves0–1152px and never virtualizes
(the threshold is384blocks). The768paragraph case moves across the192block
cache. Its repeat reverses every180events (25920px), retaining1260events and
all768paragraphs while avoiding stationary boundary inputs.

Unchanged-document scrolling now moves overlapping block views and materializes
only entering IDs. A preview-dirty flag gates reuse: ruler previews can mutate
cached attrs without changing document heads, and can outlive active drag state.
The flag also gates single-block edit reuse and recovery export, and clears
when canonical views are restored. The regression failed on rebuilt overlapping
text allocations before the fix and now checks forward/backward retention,
canonical equality and preview discard. Full WP library:278passed,0failed in
`target/runtime-proof/scroll-overlap-green.log`. Source review found no blockers.

Overlap-reuse measurement results:

- Pre-change `wp-native-scroll-large-20260916-133333`: all1260moving inputs
  verified,807measuredCPUframes, complete feedback; CPU p9931.305ms,
  delivery p9928.040ms, input-associated display interval p9941.666583ms.
- Post-change executable SHA-256
  `21966ef7519a46ee867ee4a3bdff72da55aa167b489f34fbcae22dbbba29dd38`,
  `wp-native-scroll-large-20260916-141334`: all1260moving inputs verified,
  1200measuredframes with complete display feedback; CPU p9912.859ms,
  delivery p990.038ms; display interval p9925.000ms. Improvement, still fails
  displayed60fps target. Sync averages9.649ms; draw averages2.265ms.
- Repeat141406: all movement effects verify,700measuredframes,954surface
  occlusion skips. Missing display requests fail qualification. Raw evidence
  and failed-presentation-report.json retained.
- Short sampled diagnostic141500:411main-thread samples in WordProcessor::bind
  (207active-region order and203body order), versus36in the next sync segment
  and8stylesheet samples. Sampling is diagnostic only, not qualification.

The follow-up bind fast path directly checks canonical Map+Bytes-order body
membership and clamps the Unicode caret offset. Initialization, empty bodies,
invalid/deleted targets and running regions retain existing fallback behavior.
RED recorded6order scans across3unchanged binds; GREEN records0.50model tests
cover remote shrink, malformed/deleted order/block/text and region fallback.
Full WP library now281passed,0failed (`body-bind-final-full.log`).

Native frame records now include the read-only workload observer's state after
drawing. `validate_frame_scroll_states` checks complete ordered mappings,
finite in-bounds offsets, and equality with the last effect in each coalesced
batch; a draw-time clamp cannot silently pass. Optional WP window-index bounds
and coverage are reported. This remains state evidence, not pixels/content
identity. All37reporting tests pass (`post-draw-reporting-tests.log`).

Direct-bind native repeats (executable SHA-256
`9e2a57054c53bf38faa9fe229cc180705ca93787d7915d511cabe803c9303b25`):

- Large scroll142539:1260movements,1200measuredframes; CPU p993.813ms,
  delivery p990.058ms. Complete positive presentation feedback, interval
  p9925.000ms/max25.000042ms. Post-draw state matches every last batched input;
  offset range0–25920px, window starts0–576, observed index union768blocks.
- Repeat142611: same verified counts and post-draw coverage; CPU p993.741ms,
  complete feedback, interval p9925.000ms/max25.000042ms. Both fail the strict
  displayed60fps target despite the repeatable CPU improvement.

The first repeat's worker diagnostic reports encode mean7.119ms/p998.553ms,
acquire mean0.030ms, submit0.072ms, present0.073ms. Scheduler-send lateness
p994.065ms is separately recorded; do not attribute display variation solely
 to scheduler jitter. A representative scene is1.159MB with856draw tags.
Earlier sampled renderer stacks show glyph resolution and buffer-copy work.
`draw_laid_text` still submits rich text for every retained laid item, including
 offscreen content. Next shared-layer candidate: conservative clipped rich-text
encoding, preserving actual clips and all document bookkeeping. Bounds must
include real glyph ink, baseline shifts, variable coordinates, fallback fonts,
hanging punctuation and discretionary hyphens; unknown bounds must render
normally. Exact offscreen RGBA equivalence and reduced glyph counts are required
before claiming a safe optimization. The candidate is now implemented; verification and native results follow below.

A separate live inspection run142928 holds its process after the workload so
CUA can inspect the actual window rather than relaunching an empty app. At the
endpoint, post-draw state is offset25920, window576–768. The screenshot shows
paragraphs599–602 and page24, with intact rendered text. This is a spot check,
not proof of full-document pagination accuracy or every frame's pixels. Large
layout uses estimated prefix extents; accurate pagination/visual continuity
across virtual-window transitions still needs explicit comparison.


### Conservative rich-text clipping candidate

The shared renderer now offers `draw_rich_in_clip`, which omits a glyph batch
only when actual unhinted outline bounds prove every glyph outside the clip.
The cache includes font identity, variation coordinates and size. Unknown,
color/bitmap and invalid bounds retain the reference drawing path. Word
Processor supplies its canvas/split-page clip without skipping document
bookkeeping or changing real clip layers.

All 12 shared renderer regressions pass, including exact RGBA comparisons for
transformed styled text, fallback, variable fonts and exercised discretionary
hyphens (`clipped-rich-regressions.log`). The full Word Processor suite passes
281 tests with the explicit GPU oracle ignored (`wp-clipped-text-full-tests.log`).

The full-document raster oracle found nondeterminism in the unchanged reference:
rendering the identical scene twice changed 16–17 channel bytes by one level.
Therefore this document test is not an exact-RGBA claim. It measures a per-byte
range over 64 reference renders, requires at most 256 variable bytes, each with
range at most one, and requires four candidate renders inside those same-byte
ranges. All stable bytes must match exactly. It also checks retained layout and
review state and requires more than threefold encoded glyph reduction across
three scroll/zoom positions. Both final and repeat runs pass
(`wp-clipped-text-final-pixels.log`, `wp-clipped-text-repeat-pixels.log`).
The earlier 16-reference-sample candidate failed on one byte, then passed
unchanged; those failures and identical-scene diagnostics remain retained.
This sampled envelope has finite coverage and is not proof of all document
pixels or fonts. Native results follow.


Native candidate executable SHA-256:
`af6793bedf9ebbf215897192c2e624ca5fbf15a055f429faa53bcde9c450c3e0`.
The optimized build succeeds (`clipped-text-native-build.log`). Same 768-block,
1260-movement scenario, with complete positive presentation feedback and verified
post-draw state in both runs:

| Run | Measured frames | CPU p99 | Display interval p99 | Display maximum |
| --- | ---: | ---: | ---: | ---: |
| 144952 | 1199 | 6.766 ms | 25.000 ms | 41.667 ms |
| 145020 | 1199 | 7.307 ms | 16.666750 ms | 41.667 ms |

Worker diagnostics after the first 60 logged worker frames: mean encode
0.335/0.339 ms, p99 0.640/0.591 ms; mean scene size about 30.2 KB, compared
with the previous representative 1.159 MB scene and mean encode 7.119 ms.
These diagnostic cohorts are distinct from the strict input-associated display
cohort. The clipping candidate removes substantial worker work, but raises CPU
p99 from 3.741–3.813 ms and does not yet deliver repeatable 60 fps p99.
No qualification is inferred from the better second run.

Short sampled diagnostic 145051 (`clipped-text-sample.log`, not qualification)
shows repeated `InkBoundsKey` hashing under `draw_rich_run`. Next work should
remove repeated font/variation-key hashing and glyph-bound traversal from the
main-thread clipping path while preserving conservative outline bounds and the
existing pixel regression suite. No app has been marked qualified.


The next shared candidate groups the outline cache by full font instance
(blob/index/face/weight/size/normalized coordinates), then caches glyph bounds
by glyph ID inside that entry. The full font key is hashed once per batch;
transform coefficient padding is also computed once per batch. Total retained
glyph bounds remain capped at 4096, including unknown bounds, and generation
changes clear both cache and entry count. The outline calculation, rounding
padding and unknown-font fallback are unchanged. A regression fails on the
old structure (15 font keys for 15 glyphs) and passes with one shared font key
(`ink-cache-group-red.log`). All 13 renderer regressions, including strict RGBA
comparisons, pass (`ink-cache-group-regressions.log`). Native performance of
this revision remains unmeasured at this checkpoint. The Word Processor
reference-envelope pixel oracle also passes (`ink-cache-group-wp-pixels.log`),
and the optimized native build succeeds (`ink-cache-group-native-build.log`).


Grouped-cache follow-up capacity regression passes
(`ink-cache-capacity.log`): more than 4096 distinct size/font-instance requests
exercise eviction, retain the global bound and reuse a repeated glyph after
clearing. This is a cache-lifecycle test, not a timing result.

The next short profile, `wp-native-scroll-large-20260916-150650`, uses executable
SHA-256 `0498030ca2ce2d595a30c878ddd417d14a023cc86ff9599f71b995e4eb0d8e16`.
It deliberately ran under concurrent VM compilation, with host process snapshots
in `ink-cache-group-loaded-host-{before,after}.txt`. It is NOT a comparable
performance run: only 100 measured inputs, 97 frames, scheduler p99 42.601 ms,
CPU p99 76.612 ms. The trace no longer shows old InkBoundsKey hashing, but still
shows glyph-ID hashing, transformed bounds, and repeated glyph-batch Vec growth.
This motivates a bounded initial reservation of min(run glyph count,128) in
shared rich-text batching. Pixel regressions must pass before native measurement.
The grouped-only executable is retained as `word-processor-grouped-cache` so the
additional reservation can be measured separately. Full idle native repeats
remain pending; none of these loaded results qualify any app.


Grouped-cache native repeats (same executable 0498030c…; 1260 verified moves,
complete positive display feedback in each):

| Run | Measured frames | CPU p99 | Display p99 | Display maximum |
| --- | ---: | ---: | ---: | ---: |
| 150926 | 1200 | 5.171 ms | 16.666750 ms | 16.666958 ms |
| 150953 | 1199 | 5.262 ms | 24.999958 ms | 41.666625 ms |
| 151024 | 431 | 18.849 ms | 183.332958 ms | 324.999417 ms |

First run launched with one queued local compiler active. The next two launched
with no observed compiler and VM CPU ~0.5–1.7%. Third run is a failed workload
sample, not omitted: dispatch-delivery p99 167.011 ms while scheduler lateness
p99 was 4.091 ms; only one initial occluded skip is logged. The cause is not
established. Retained post-draw states cover all 768 indices, but coalescing
leaves fewer than the required 1000 measured frames. No 60 fps qualification.

The batch-reservation follow-up failed a strict transformed-text pixel test
(`ink-cache-batch-regressions.log`, `ink-cache-batch-pixels-repeat.log`). A
diagnostic rebuild passed once, then failed again
(`ink-cache-batch-diagnostic-repeat-1.log`): rotated case at (117,24) has RGB
6 versus 7; repeated reference in that attempt is identical. This does NOT
establish reference noise as the explanation. Reservation is reverted to
isolate the issue; diagnostic output keeps the exact assertion and does not
introduce a tolerance. Previous one-shot exact-pixel passes are insufficient
for a stable correctness claim. Investigate before wider clipped-path rollout.

The exact same three-byte mismatch reproduces after reverting reservation
(`ink-cache-no-reservation-pixels.log`). This rules out that allocation change
as the necessary trigger. Next isolate clipped versus reference scene encoding
and flat versus grouped bounds lookup for this rotated fixture. Keep strict
pixel assertions; do not silently replace them with an AA tolerance.


### Rotated-pixel mismatch isolated to GPU ordering variation

Repeated-scene diagnostics `clipped-repeat-scene-4.log` and `-9.log` show the
unchanged reference itself alternating RGB6/7 at pixel(117,24) over 32 renders.
The clipped scene alternates over the same values. Cold and warm glyph streams
match. This is stronger evidence than the earlier single repeated reference
(which happened to match). Shader source explains the mechanism: path_count.wgsl
assigns segment slots with atomicAdd; fine.wgsl fill_path sums those contributions
in f32. Allocation/scheduling can change the accumulation order. No production
shader or clipping geometry is changed on this basis.

CPU preprocessing (`TILO_UI_VELLO_CPU=1`) makes this exact matrix pass
(`clipped-cpu-preprocess-pixels.log`); Vello still uses GPU fine rasterization.
The matrix now has two independent checks: stable CPU preprocessing requires
four exact reference and clipped renders per case; normal GPU preprocessing
measures min/max per byte over 65 reference renders then checks four candidates
against those exact same-byte ranges. It permits at most16 varying reference
bytes per220×160 case, each range at most1; all stable bytes remain exact.
This is a finite sampled GPU envelope, not universal exact-GPU equivalence.
All15 renderer regressions initially pass (`clipped-dual-oracle-regressions.log`).
A test-only constructor selects preprocessing explicitly without changing the
process environment; production constructor behavior is preserved.

The explicit-mode final revision passes all15 tests again
(`clipped-dual-oracle-verified.log`,6.72s). A preceding constructor call/signature
compile error is retained in `clipped-dual-oracle-final.log` and was corrected
before that verified run. No production tolerance or shader arithmetic changed.


### Glyph-ID hashing and late dispatch diagnostic

The inner bounded glyph-ID cache now uses rustc-hash FxHashMap; the outer
font/variation map retains randomized hashing. This addresses SipHash samples
on numeric u16 glyph IDs without changing bounds, entries, eviction, keys or
clipping decisions. rustc-hash2.1 already existed transitively; ui now declares
it directly through workspace dependencies. All15 renderer regressions pass
(`ink-cache-fx-regressions.log`), including both pixel oracles and capacity
coverage. Native benefit remains to be measured.

The previous failed151024 run is fast through input359, then dispatch delays
rise to ~83 ms mean/~167 ms p99 per120-input group. Scheduler remains timely.
A new diagnostic collector option WP_PROFILE_AFTER_INPUT profiles later in a
full1260-input run rather than truncating to120inputs. Late sample152038 at
input500 does NOT reproduce the stall: dispatch p990.271ms, CPU p996.093ms,
display p9925ms; it is an instrumented diagnostic, not qualification.
The native cause remains unresolved. WP_PROFILE_DISPATCH_DELAY_MS now allows
sampling to trigger only when one of the latest32inputs crosses that delay;
metadata records the setting and whether sampling actually occurred.


Fx glyph-cache native build succeeds (`ink-cache-fx-native-build.log`). Binary
SHA-256 `af092a2172f4850b82a17ccf7033a82fee9792380e4e7a7a8cfb841f15e284b6`.
All following runs have1200 measured frames,1260 verified movements, complete
positive presentation feedback, and no triggered sample. The collector watches
for dispatch delay>=50ms; that observation overhead is identical in the old/new
comparison. No compiler was observed at the first candidate launch; the other
task held heavy builds for the comparison window.

| Run / cache | CPU mean | CPU p99 | Display p99 | Display maximum |
| --- | ---: | ---: | ---: | ---: |
| 152331 / Fx | 3.699 ms | 5.151 ms | 16.666750 ms | 33.333250 ms |
| 152409 / Fx | 3.954 ms | 5.710 ms | 25.000000 ms | 25.000042 ms |
| 152430 / prior grouped SipHash | 4.117 ms | 6.161 ms | 16.666708 ms | 33.333250 ms |

The candidate has lower observed CPU costs in this comparison, but run-to-run
variation and the single adjacent old-binary run limit causal strength. Actual
display timing does not consistently improve. Retain as a bounded candidate,
not a 60fps qualification or proof of shared-layer performance stalling. The
prior grouped binary0498030c… and Fx binaryaf092a21… are both retained for further
comparison; the probe bundle is restored to the Fx candidate after the runs.
The earlier167ms dispatch-delay failure remains unresolved and was not reproduced
by the new automatic trigger. No change to OS activity policy or scheduling was
made to hide it.


### Bounded rich-glyph batch reservation

The shared rich-text batch vector reserves min(run glyph count,128) before
copying. This avoids repeated growth for ordinary lines while bounding the
initial allocation for huge heavily styled runs. Same glyph order, grouping,
positions and drawing behavior. All15 renderer regressions pass with the dual
pixel oracle (`rich-batch-reserve-regressions.log`). Native build succeeds
(`rich-batch-reserve-native-build.log`). Binary SHA-256:
`4e6df4a4995dd1ad164f0ffe6cfd76c84ae512b427470d2b3ff4567e72184df2`.

During a coordinated idle build window:

| Run | Measured frames | CPU mean | CPU p99 | Display p99 | Display max |
| --- | ---: | ---: | ---: | ---: | ---: |
| 153222 | 1198 | 3.388 ms | 4.868 ms | 25.000 ms | 41.667 ms |
| 153300 | 1200 | 3.331 ms | 4.737 ms | 25.000 ms | 25.000 ms |

Both have1260 verified movements and complete positive display feedback;
conditional50ms dispatch sampler did not trigger. Compared with the preceding
Fx-only runs (CPU mean3.699/3.954ms, p995.151/5.710ms), observed CPU cost is
lower, but displayed60fps p99 remains unmet. Keep the failed/slow intervals;
there is no shared-layer stall or all-app qualification claim.


### Borrow uniform shaped glyph runs

Shared rich-text painting now borrows a run's glyph slice when every glyph has
the same font identity/weight, color, exact size and baseline shift, and no
hanging punctuation or discretionary-hyphen marker needs position rewriting.
All other runs retain the existing copy/batch path. This eliminates the
transient glyph vector and copies for uniform runs; it still scans eligibility.
New regression directly compares old copied and borrowed glyph streams and
exact stable-preprocessing pixels, including normal/bold/superscript runs and
mixed-style/soft-hyphen fallback. RED missing methods then all16 regressions
pass (`borrowed-rich-red.log`, `borrowed-rich-regressions.log`). Optimized native
build succeeds (`borrowed-rich-native-build.log`).

Candidate SHA-256:
`d74a7c42ab90c01964750556390b60c64d010a3b2c6b4c3f3a34dc3b3d806a7d`.
The collector now has optional WP_STYLED_FIXTURE=1, which renders the same
paragraph text with bold/italic spans, records it in metadata and leaves the
default fixture unchanged. Its performance comparison remains pending; the
uniform fast path can add a failed eligibility scan to mixed runs, so normal
paragraph timing alone cannot establish absence of a styled-text regression.


Borrowed-run native observations (plain 768-paragraph scrolling):

| Run | Measured frames | CPU mean | CPU p99 | Display p99 | Display max |
| --- | ---: | ---: | ---: | ---: | ---: |
| 154417 | 1200 | 3.125 ms | 4.568 ms | 16.666708 ms | 25.000000 ms |
| 154459 | 1200 | 3.241 ms | 4.978 ms | 16.666708 ms | 24.999917 ms |

Both runs have 1260 verified movements, complete positive display feedback and
no triggered 50 ms dispatch-delay sample. Raw reports are in
`target/runtime-proof/wp-native-scroll-large-20260916-154417/` and the sibling
`154459` directory. No compiler was observed at host checks, but complete host
isolation was not established. The second CPU p99 exceeds the preceding
reservation candidate's 4.737 ms; these observations alone do not establish a
repeatable improvement. Display p99 is approximately the 60 Hz interval in
both runs, with occasional 25 ms intervals. This is not full app qualification:
styled text, repeated typing, resize, animation, full pagination/pixel continuity,
other applications and platform coverage remain open.


### Mixed-style comparison

Same 768 paragraphs and visible text, with repeated bold/italic spans imported
through the real Markdown codec. Alternating saved borrowed-run and prior
reservation binaries; identical profiler settings. Times below are milliseconds.

| Run / candidate | Frames | CPU mean | CPU p99 | Display p99 |
| --- | ---: | ---: | ---: | ---: |
| 155004 / borrowed-rich-styled1 | 1200 | 9.624 | 10.867 | 16.666708 |
| 155038 / reserve-rich-styled1 | 1200 | 9.494 | 10.679 | 25.000000 |
| 155108 / borrowed-rich-styled2 | 1200 | 9.744 | 10.751 | 24.999875 |
| 155141 / reserve-rich-styled2 | 1199 | 9.728 | 10.792 | 24.999958 |

All four runs have1260 verified movements, complete positive display feedback,
no conditional sample and matching pre/post executable hashes. No compiler was
observed during the initial run; other task was requested to hold heavy work.
Styled text does not establish a consistent60fps p99 result. Borrowed eligibility
adds work before fallback for mixed lines; this comparison does not establish a
repeatable styled-text speedup. Drawing averages approximately8.9ms versus2.4ms
for the earlier plain candidate, while sync averages remain under0.8ms. Next
profile this shared drawing path; do not infer the cause from timings alone.


Styled diagnostic sample `wp-native-scroll-large-20260916-155218` uses the
short120-step fixture and three-second macOS sampler; it is not qualification.
Of628 sampled `draw_frame_content` stacks,359 are in Word Processor pagination,
including310 in `block_runs_markup`;118 are in shared `draw_rich_in_clip`.
Counts are inclusive stack samples, not timings to add or CPU percentages for
the full run. Source confirms both `block_runs` and `block_runs_markup` resolve
a style tuple for each character by repeatedly scanning the full mark vector,
and `resolve_paragraph_shape` does this before testing its shaped-layout cache.
This supplies an app-specific lead rather than justification for another font
cache without profiling. Proposed next experiment: preserve existing mark
precedence/resolution, evaluate only at sorted character-index mark endpoints,
and merge equal adjacent intervals. Compare exact spans/decorations with the
old per-character reference for Unicode, overlapping/invalid/empty marks and
review on/off, then repeat styled/plain timing and layout regressions. No such
production change has been made at this checkpoint.


### Mark-boundary style preparation candidate

Word Processor's two style-run builders now resolve styles only at sorted,
clamped mark endpoints and merge adjacent equal results. Existing mark resolver
order, review precedence, character offsets, decoration ranges and empty runs
are retained. This changes repeated per-character mark scans into per-boundary
scans; dense mark sets still require O(B*M) work, so it is not a constant-time
cache or proof of a shared-framework plateau. Regression first failed for the
missing helper (`mark-boundary-red.log`), then two targeted tests passed and the
full library suite passed283tests with one opt-in GPU oracle ignored
(`mark-boundary-green.log`, `mark-boundary-full-tests.log`). Independent read-only
review found no blocker. The reference comparison was subsequently strengthened
from Debug text to explicit span fields/float bits; that test-only refinement
still needs its targeted rerun. Optimized build/native comparison pending.


Mark-boundary explicit-field reference tests pass2/2
(`mark-boundary-fields-tests.log`); optimized native build succeeds in2m01s.
Candidate SHA256 `d2670a83ab55a8f466733eaa9656c398d4155377417d4c48747593af13de658a`.
Saved as `target/runtime-proof/word-processor-mark-boundaries`; probe bundle
restored to that candidate after adjacent prior-binary comparison.

| Run / workload | Active frames | CPU mean ms | CPU p99 ms | Display p99 ms | Display max ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| 155947 / mark-boundary-styled1 | 1196 | 5.181 | 6.185 | 25.000000 | 100.000 |
| 160019 / mark-boundary-styled2 | 1136 | 5.302 | 7.276 | unproven | unproven |
| 160058 / mark-boundary-plain1 | 1200 | 3.579 | 5.715 | 25.000000 | 25.000 |
| 160130 / mark-boundary-plain2 | 1196 | 3.174 | 6.339 | 25.000000 | 100.000 |
| 160204 / mark-boundary-prior-plain | 1200 | 3.687 | 6.034 | 24.999958 | 25.000 |
| 160238 / mark-boundary-styled3 | 1198 | 5.065 | 6.136 | 16.666708 | 50.000 |

Every attempt retains1260 verified movement effects and stable binary hashes.
160019 has506 zero display timestamps out of1136 expected frames, so its display
percentiles are withheld. Do not discard this failed display attempt. Other
attempts have complete positive feedback. Conditional50ms dispatch sampler did
not trigger. No compilers were observed in initial/mid-run host snapshots, but
the controller could not guarantee isolation from other implementation work;
source changes in that worktree also mean this is not an isolated binary A/B.

Styled CPU mean drops from9.62–9.74ms before to5.07–5.30ms, p99 from10.75–10.87ms
to6.14–7.28ms. Draw mean drops from~8.9ms to~4.4ms, matching the sampled source
bottleneck. Plain means3.17/3.58ms compare with3.69ms in the adjacent prior binary;
p99 varies6.34/5.72ms versus6.03ms, so no plain-tail speedup is established.
Retain the candidate with exact span/reference regressions, but no app qualifies.

Long presentation intervals remain: styled155947 has~100ms between CPUframes
1217/1218 and five coalesced inputs1215..1219; frame1218 CPU build12.337ms and
worker683µs→7995µs do not explain the entire wall gap. Dispatch delay reached
27.824ms, below the50ms sampling trigger. Plain160130 similarly has~100ms
between frames261/262 with five coalesced inputs259..263. The timing origins
are not correlated to display host time, so these are bounded associations,
not an end-to-end latency attribution. Later styled160238 has display p99
16.666708ms but maximum50ms. Native scheduling/presentation tail diagnosis,
full visual/page continuity and all remaining app/workload/platform gates stay open.


### Main-thread gap localization

Offline analysis of consecutive CPU frame IDs (never bridging missing/zero
presentation feedback) is retained in `target/runtime-proof/mark-boundary-gap-diagnosis.json`.
In styled155947, the100ms display gap corresponds to100.123ms between CPU
commits, with only12.337ms in the next timed build:87.786ms lies outside that
build. Plain160130 similarly has99.343ms between commits,8.879ms timed build,
and90.464ms outside. Worker starts are100.386/102.272ms apart, respectively.
The50ms styled160238 gap has53.922ms between commits and9.034ms timed build.
Independent clock origins cancel for intervals within each stream; these
comparisons do not establish absolute input-to-photon latency. The unmeasured
interval includes event-loop work/wait, pre-profile redraw work, and the previous
frame's post-profile work. It is not evidence of a specific OS scheduling cause.

Opt-in native diagnostics now add the same input-origin Instant timestamps at
redraw entry, timed-build start, frame commit and submit attempt, so future
runs can separate those regions. No scheduling policy or normal-path clock
reads change. Targeted scheduler tests/build/runtime verification pending.


Native stage diagnostics verified: seven native_scenario tests pass and the
optimized build succeeds (`native-redraw-stage-tests.log`,
`native-redraw-stage-build.log`). Saved diagnostic binary
`word-processor-redraw-stages`, SHA256
`757ecd5bbb9fa70b24307ef328d24719b14143d0777ba24a3ec1a32fb6ecb9c9`.
160918 validates1260redraw rows and1260input-associated frame commits:
entry<=timed-build-start<=commit<=submit, and all associated inputs dispatch
before commit. CPU p995.938ms; display p9916.666750ms/max25ms with complete
feedback. Long stalls did not reproduce. Maximum entry-to-build14µs and
commit-to-submit812µs apply only to this run, not the earlier stalled captures.

Repeat161010 exits1 at the missing-display-request gate. Raw logs show503worker
Skipped outcomes with explicit `occluded` reasons, starting at frame788 after
the initial startup skip. No display percentile is claimed. Stage evidence is
retained separately in `redraw-stage-validation.json`: one28.629ms gap between
previous submit and next redraw entry; entry-to-build10µs and commit-to-submit
160µs for that frame. This occluded capture is not a substitute for reproducing
the prior100ms foreground gap. Both collectors terminal; all evidence retained.


### Foreground activation and repeated typing

Typing161159 applied1260 exact edits but all925 measured worker outcomes were
Skipped/occluded. CPU p992.248ms is not a foreground performance result. Raw
failure plus `cpu-only-diagnostic.json` retained. The prototype collector now
allows WP_WINDOW_NO_ACTIVATE=0 (default remains1), records the setting, and
retains a failed-report.json before rethrowing display validation failures.
Python syntax validation passes; qualification failures remain failures.

Using the same diagnostic757ecd5b… binary with activation enabled:

| Run | Workload | Active CPU frames | CPU p99 ms | Display p99 ms | Display max ms |
| --- | --- | ---: | ---: | ---: | ---: |
| 161325 | typing | 1200 | 1.933 | 16.666708 | 25.000 |
| 161411 | typing | 1199 | 1.916 | 16.666708 | 33.333 |
| 161444 | styled large scroll | 1200 | 6.502 | 25.000000 | 25.000 |

All have1260 validated effects, complete positive presentation feedback and
stable binary hashes. Host isolation was not guaranteed. These activation-mode
runs must remain distinct from no-activation experiments. They show that valid
foreground measurements are possible, not that activation universally fixes
occlusion or qualifies every app. Styled display p99 still exceeds60Hz budget.

Same-origin stage ordering validates2512/2511typing redraw rows and1260scroll
rows. Typing includes additional non-input-associated redraws; do not pool those
into input-associated distributions. In161325, frame42 has56.119ms from prior
submit to redraw entry,6µs entry-to-build and157µs commit-to-submit. This is a
warmup gap, not a measured-tail failure or proof of the earlier100ms cause.
No>25ms stage gap occurred in the other two runs. Captures and per-run
redraw-stage-validation.json retained; all parent processes terminal.


### Deferred commits cause a second typing paint

Diagnostic161958 (`native-commit-redraw-run.log`) records1260idle commit redraw
requests. For1258, the trace orders an input-associated submit, then the commit
request, then a second non-input-associated redraw. Total2518redraws; exact
summary in `commit-trace-analysis.json`. Winit's local macOS `AppState::cleared`
dispatches pending redraws before AboutToWait, matching the observed cause.
Earlier161411 extra paints averaged~1.394ms timed build each. These are measured
extra paints, not proof that all underlying work is visually redundant.

Candidate App::commit_pending_widget retains commit_action→on_commit→needs_sync
and performs it inside timed frame work before update/sync/draw; idle fallback
remains for pending edits without a successful paint. RED missing helper then
exactly-once acknowledgment/no-op/undo regression passes (`prepaint-commit-red.log`,
`prepaint-commit-green.log`);63tests matching state:: pass. Independent review
finds no confirmed regression, but record-switch/bound-history integration and
native removal of the second paint still require evidence. Record tick ordering
is unchanged. WP suite and optimized native build pending at this checkpoint.


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

Code Editor has an app-local 2000-line Rust fixture and durable-buffer
snapshot adapter (SHA256, explicit scroll units). Completed native captures
validate edits and presentation, but the 60Hz typing case still has a 25ms
input-associated display interval p99. See the performance plan ledger for
binary hashes, raw runs and the incremental highlighting/diagnostics changes.
All 47 apps remain unqualified.

### Physical pointer isolation for scripted captures

The native collectors accept `--pointer-passthrough --no-activate`. This opts
into `TILO_PROFILE_POINTER_PASSTHROUGH=1` only for a scripted native scenario.
The window disables physical cursor hit testing before being shown; scripted
inputs keep the existing event dispatch route. Unsupported backend errors stop
the app. The collector requires a literal `pointer_passthrough: true` in the
actual native window metadata. This acknowledges backend acceptance, not proof
that physical events were absent: inspect actual cursor/state observations too.
Normal application launches retain normal hit testing.

The older File Explorer binary was rejected by this gate in
`target/runtime-proof/file-explorer-native-ev3gffjd`. The preceding run with
physical hit testing had 641 unmoving wheel events while its cursor was outside
the viewport, and another 16 at the upper boundary after missed downward moves.
Those failures cannot establish a rendering-performance regression.

The feature-enabled isolated File Explorer capture
`target/runtime-proof/file-explorer-native-nlnz60ei` validates all1260wheel
movements with one stable scripted cursor position.1191measuredframes have
complete positive display callbacks: CPU p99 14.191ms, input-associated
display interval p99 33.333208ms. This valid single baseline fails the60fps
p99 target. Build native timing probes with `--features ui/presentation-feedback`;
the omitted-feature run is retained as an explicit failed capture.

Follow-up `file-explorer-native-80iaqdex` showed that macOS may still deliver
physical tracking events despite cursor-hit-test acknowledgement. Therefore
new requested isolated captures must also confirm `input_route_isolation:true`.
The collector rejects absent or non-boolean route acknowledgement. Backend
acknowledgement alone is insufficient; previously saved captures remain raw
evidence, and only runs with verified actual movement/cursor state were valid.

Shared layout reuse plus Files measurement contracts and lazy control-resource
projection were validated in two same-binary List captures (`mkpmmqe4`,
`tqpg7v99` under target/runtime-proof/file-explorer-native-*). Each validates
1260 movements and complete positive presentation feedback. CPU p99 is
7.462/7.418ms versus the previous14.191ms baseline; layout application mean
is0.003608ms versus6.398ms. Display p99 is33.333292/16.666833ms respectively.
The variable display tail still prevents a solid60fps claim. Source and host
comparison limitations are recorded in files-shared-candidate-comparison.json.

### Host-clock latency diagnostics

Native scenario packets now record direct macOS host-clock brackets around send and dispatch Instant reads. The collector's optional `input_to_presentation` report uses those bounds with Metal presentation timestamps; old binaries and unsupported platforms report unavailable. Each coalesced input is retained, but intermediate effects may have disappeared before the shared frame was drawn. This is synthetic input processing to next presented frame, not physical device-to-photon latency or proof of frame cadence. Numeric bounds cover sampling and floating-point representation, not compositor timestamp accuracy. Two Files captures from the saved clock-instrumented scrollbar-hold binary now have complete latency evidence (`e1bj0bmj`, `yzv25qln`): synthetic send-to-presentation median24.063/27.268ms and p9932.801/35.416ms. Both still have25ms input-associated display p99; these are not performance qualifications.

The animation-pacing candidate produced two valid1260-step Files captures (`9l1mu3no`, `__xff2nq`), both with25ms input-associated display p99. CPU p99 was2.975/2.748ms. These results remain unqualified despite complete presentation feedback. See `target/runtime-proof/animation-scheduling/comparison.json`.

The scrollbar-hold change reduces non-input active frames from1119/1133 to67/68 in repeated identical workloads. Total active CPU frame work drops from5322/5121ms to2631/2747ms. Each run verifies1260 scroll effects and complete positive presentation feedback. CPU frame p99 is3.920/3.864ms in the new runs; CPU-tail and display-tail claims remain separate. Saved comparison: `target/runtime-proof/scrollbar-hold/comparison.json`. Known-source snapshots record a concurrent History host change; the saved binaries and script hashes identify artifacts, not a perfectly isolated attribution.

### Settings native search

The saved Settings Native Search Probe binary now has two valid normal-launch search captures: `settings-native-yenvppb7` and `settings-native-uz2vhh_1`. Both verify1260 alternating edits, stable effective document heads, focus/caret/search results and complete positive display feedback. CPU p99 is1.966/2.785ms; input-associated display p99 is16.666750/25ms. The second run misses the target, so neither is qualification evidence for the app. Synthetic send-to-next-presented-frame p99 is29.584/33.850ms. Other Settings workloads remain unmeasured. See `target/runtime-proof/settings-native-adapter/comparison.json` and its binary/source manifest.

The new optional `render_boundaries` report distinguishes main channel-send duration, send-start-to-worker-receive, worker CPU/render duration, actual present-call CPU duration, and present-return-to-display. It uses common host-clock brackets and strict CPU/worker identities. Unprofiled auxiliary frames and early surface loss are validated without being substituted for measured frames. This does not measure GPU completion. Worker CPU duration includes diagnostic logging and device polling; negative present-return-to-display is valid when display precedes CPU return. Native Files capture `dm5x9wza` validated 1,200 measured frames with complete positive presentation and render-boundary evidence. Median send-to-worker delay was 0.013 ms; median present-return-to-display delay was 19.04 ms. These CPU boundaries do not distinguish GPU completion from compositor waiting.


### Post-draw main-thread diagnostic stages

The optional `main_stages` report measures work after drawing and before the render-channel send: profiler commit, native state snapshot/log output, debug outlines, damage collection and dirty-state clearing, accessibility, HUD/request construction, and redraw-log output. The producer records eight common-host-clock brackets and emits one row after sending the frame. Normal runs outside the native timing gate read no additional clocks.

These intervals supplement `active_cpu_frame_build`; that existing metric ends before this region. They do not include earlier layout/draw work, channel-send blocking, GPU completion, or physical input latency. Native snapshots/logging and host preemption remain part of the measured intervals. Older binaries explicitly report the stages as unavailable. Missing or malformed rows in an instrumented capture fail validation.

The runner waits within its existing drain timeout for observed main-stage and render-boundary diagnostics as well as display callbacks, avoiding premature shutdown before post-send logging completes. Final parsing still validates identities, finite chronological brackets, and complete positive presentation evidence. Native Files captures `azaxem6g` and `t8p2snbi` validate this report with 1,199 and 1,094 measured frames, complete presentation feedback, and all 1,260 expected effects. Post-draw-to-send p99 was 2.323 and 6.378 ms; display interval p99 remained 25.000 and 41.667 ms. These are diagnostic results, not performance qualification.


The control-presence fast path was measured with the same saved Files workload and logging in `dsewokz8` and `ago6gvst`. Resource-availability CPU work averaged 0.917/0.940 ms per measured frame, versus 1.832/1.772 ms in the preceding captures. CPU frame-build p99 fell to 1.289/1.412 ms, but display-interval p99 remained 25.000/41.667 ms. Both runs retained all 1,260 verified effects and complete presentation evidence; the optimization does not qualify Files or other apps.
