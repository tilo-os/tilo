# Application performance coverage

The [app suite inventory](../scripts/perf/app-suite.json) records 47 GUI applications with native Rust entrypoints under `apps/`, plus the excluded headless `tilo-agent` CLI. Every GUI app is **unqualified**. This is a source inventory, not evidence that every app builds or runs on macOS. Package and binary names come from each Cargo manifest, including `windows-app` / `windows` and the implicit `tilo-scratchpad` binary.

Implementation follows the [application performance plan](superpowers/plans/2026-09-16-app-performance.md). Improve measured shared-framework bottlenecks first; move to app-specific optimization when controlled measurements show the remaining dominant work is app-specific or shared experiments no longer yield repeatable gains.

## Qualification contract

- Scope every GUI app, each applicable workload, and the measured OS/hardware configuration explicitly. Missing, failed, or unsupported cases remain incomplete; do not infer platform parity.
- Use optimized executables with recorded pre-launch hashes and a post-run consistency check. Retain fixture sizes, viewport and scale, hardware/OS metadata, instrumentation settings, warmup policy, and raw evidence for successful and failed runs. Filesystem cache state is uncontrolled unless explicitly established.
- The 60 Hz budget is **16,666,667 ns**. Collect at least 1,000 measured active-workload frames per case after warmup, across repeated runs. Report p99 and missed/skipped/dropped outcomes. Do not dilute active-frame distributions with idle rebuilds or discard failed attempts.
- Exercise real scroll, typing/editing, resize, timed animation, and applicable large-document/media operations. Check resulting state or pixels so a missed target, ineffective input, empty fixture, or stalled animation cannot pass.
- Measure main-thread work, render queue/worker work, GPU completion, and presentation cadence separately. CPU frame construction and the duration of `present()` are diagnostic metrics, not proof of displayed 60 fps. Do not add independent stage percentiles. Startup and action completion latency are separate measurements.
- Preserve functionality, output quality, accessibility, security checks, responsiveness, and idle efficiency. Reducing fixture sizes, disabling features, or hiding missing coverage does not qualify an app.

## Reusable machinery

| Existing facility | Reuse and limitation |
| --- | --- |
| [`ui::launch`](../ui/src/launch.rs) | Common app lifecycle and `--headless-drive`. The inventory marks enabled paths unverified. `osk` and `scratchpad` explicitly disable generic headless driving. |
| [`ui::headless`](../ui/src/headless.rs) | Real widget event dispatch, named targets, bounded waits, semantic assertions, and component hosts. It does not establish native presentation cadence. |
| [`ui::profiling`](../ui/src/profiling.rs) | `PerfScenario`, scroll/type/key/click/frame steps, warmup exclusion, and CPU frame capture. Reuse this instead of duplicating app benchmark loops. |
| [`ui::ux_bench`](../ui/src/ux_bench.rs) | Structured input, wheel phases, checkpoints, and semantic/visual checks. Existing [`scripts/ux-bench`](../scripts/ux-bench) scenarios cover desktop journeys, not all app workloads. |
| [`ui::offscreen`](../ui/src/offscreen.rs) | GPU measurement primitives and image validation; offscreen completion alone does not establish displayed cadence. |
| [`perf_scenario`](../ui/examples/perf_scenario.rs) | Synthetic list-scroll and gallery controls for shared-framework regressions. |
| [`runtime_benchmark`](../shell/examples/runtime_benchmark/mod.rs) and its [reporter](../scripts/perf/runtime_benchmark.py) | Live Photos/Word Processor action workload and shared timing summaries. This is only part of the full workload matrix. |
| [`frame-loop-bench.py`](../scripts/perf/frame-loop-bench.py) | Compositor frame-loop instrumentation; Linux compositor backends do not establish macOS app presentation. |
| [`benchmark-host.py`](../scripts/browser/benchmark-host.py) | Browser-specific host measurements; preserve engine coverage separately from native widget chrome. |

Calculator, Monitor, Music, and Tube already have app-specific benchmark flags, recorded verbatim in the inventory. Many apps register named screenshot scenarios. Those are starting points for fixture construction, not performance passes; the inventory deliberately does not invent scenario IDs or assume screenshot roots match live roots.

## Remaining coverage work

1. Extend shared launch/scenario handling to run per-app input workloads using existing app preparation. Keep fixture data and expected outcomes in declarative scenarios, with an explicit suite runner tracking every app and failure.
2. Seed representative content. Empty lists, short documents, static thumbnails, and screenshot-only poses do not cover sustained scroll/edit/media work. Network, device, PTY, remote-frame, and media apps need controlled inputs plus native integration checks.
3. Require observable input effects. `PerfStep::ClickNamed` now fails on a missing or empty target instead of discarding that failure; a regression test also verifies delivery to a real control. Per-app state/pixel assertions remain necessary. Headless `Frames(n)` runs without a real-time schedule, so it cannot stand in for timed animation.
4. Correlate active inputs, frame identities, worker outcomes, and presentation evidence. Preserve skipped/acquire-failed frames and distinguish occlusion/background behavior from foreground qualification.
5. Add explicit OSK and Scratchpad adapters. OSK requires an input-method client; generic simulated typing is not equivalent to that integration. Browser, media, camera, and terminal fixtures likewise need their actual engines/providers where applicable.

The inventory's `existing_hooks` are source-backed facilities. `proposed_workloads` are work still to implement and validate, not executable fixture identifiers. Empty `qualification_evidence` arrays are intentional until the full contract has supporting run artifacts.

## Initial optimized diagnosis

`target/runtime-proof/20260916-085258/benchmark.json` is a successful diagnostic
run on Apple M5 Max/macOS27 using `thin-app` optimized binaries. Fifty measured
actions per app followed ten warmups; this is not the sustained-frame gate.

| App | CPU build p99 | Worker wall p99 | Worker outcomes |
|---|---:|---:|---|
| Photos | 3.038ms | 1.188ms | 50 Presented, zero Skipped |
| Word Processor | 10.925ms | 0.768ms | 50 Presented, zero Skipped |

The earlier development-build Word Processor redraw anomaly did not recur in
this run. No scheduling optimization is justified by that anomaly alone.
Word Processor's sync p99 was4.932ms and draw p99 was0.423ms. The remaining frame
cost needs attribution: existing total time includes scene-pool waits and
resource/semantic work outside the named passes. Separate percentiles cannot
be subtracted to derive a per-frame residual.

A sampling run at `target/runtime-proof/profile-shared-20260916-085420/` failed
the presentation-evidence gate (Word Processor had no presented frames in the
measurement window). It is retained as a failed diagnostic, not qualification.
Its stripped executable also prevented useful native stack symbolization.

## Shared collaboration wake experiment

The SDK inbox now wakes its owning UI loop on the first event in a queued
burst, rather than waiting for the next 60 Hz collaboration poll. Queue wakes
do not force model synchronization; the existing event pump decides whether an
event changed state. Scope checks, bounded queues, and the fallback poll remain.
Targeted validation passed 32 wake tests and 26 collaboration tests.

Two optimized foreground runs, `target/runtime-proof/20260916-091617/` and
`target/runtime-proof/20260916-091727/`, passed the signed cross-app proof and
all measured actions with zero skipped worker frames. Each has 50 actions per
app; these remain diagnostics, not sustained-frame qualification.

| Action completion | Before median / p99 | After run 1 median / p99 | After run 2 median / p99 |
|---|---:|---:|---:|
| Photos rotate | 9.952 / 17.145 ms | 0.282 / 0.736 ms | 0.244 / 0.345 ms |
| Word Processor insert | 13.704 / 24.931 ms | 4.678 / 7.248 ms | 4.528 / 7.130 ms |

Before is the optimized `20260916-085258` run. The later runs also include
additional CPU attribution and explicit foreground window placement; this is
not an isolated binary-only A/B test. CPU build p99 remained similar: Photos
3.038 / 3.118 ms and Word Processor 10.968 / 10.646 ms in the two new runs.
In run 1, Word Processor resource availability cost was 3.210 ms median and
5.619 ms p99; scene-pool wait was at most 0.003 ms. This identifies action
resource discovery/projection as the next shared path to investigate.

Failed intermediate runs `20260916-090550` and `20260916-090657` are retained.
They completed actions but lacked Word Processor presentation samples, so no
frame-performance claim uses them. The foreground hook addresses benchmark
visibility; it is opt-in and leaves ordinary application window ordering alone.

## Rejected shared allocation experiment

The borrowed declared-resource address collector passed eight action-palette
tests and independent review, but two optimized runs did not show a repeatable
frame improvement. Runs `20260916-092514` and `20260916-092541` both passed
the correctness proof with 50 Presented and zero Skipped frames per app.
Word Processor CPU build median/p99 was 6.479/10.769 ms and 6.365/10.692 ms;
resource availability median/p99 was 3.310/5.442 ms and 3.247/5.402 ms.
These overlap the preceding runs. The collector change was reverted; its source
and regression test are archived in
`target/runtime-proof/borrowed-address-experiment-source.json`.

Next, split the resource-availability envelope into declaration generation,
semantic construction/validation, permission projection, and control refresh
before choosing another optimization. This rejected experiment is not evidence
that the whole shared layer has stalled.

## Resource-stage diagnosis

The optimized instrumented run `target/runtime-proof/20260916-093337/` passed
the signed proof and all actions, with 50 Presented and zero Skipped frames per
app. Word Processor declaration generation dominates resource refresh:

| Word Processor stage | Median | p99 |
|---|---:|---:|
| Declaration generation | 2.994 ms | 5.229 ms |
| Semantic construction (includes declaration clone) | 0.075 ms | 0.137 ms |
| Surface validation | 0.058 ms | 0.089 ms |
| Local permission projection | 0.140 ms | 0.237 ms |
| Control refresh | 0.008 ms | 0.014 ms |

Each work stage has 50 samples. Resource and local caches each recorded 50 hits
and 50 misses; control availability recorded 50 misses. These are per-call
diagnostics, not per-frame accounting, and percentiles must not be summed.
Word Processor CPU build p99 was 11.291 ms; Photos was 2.930 ms. Extra logging
means this run locates costs rather than proving a speedup.

Word Processor's provider materializes the document before building structural
resources. The small projection/validation costs explain why the previous
shared allocation experiment had no measurable benefit. Shared-layer stall is
still unproven: next measure other widget families using the existing list and
gallery diagnostic workloads before selecting app-specific changes.

## Verified shared-list CPU workload

The `ui-perf perf_scenario --count 1200 --json` list case now checks the real
Scrollable's persisted offset after every wheel event. The pointer is inside
the viewport, the list retains 200 rows, and direction reverses every eight
inputs before reaching an edge. Missing movement, wrong direction, zero count,
or a mismatch between moving steps and captured frames fails the workload.
This verifies scroll state, not rendered pixels or presentation.

Two optimized runs of the same executable SHA-256
`518753b442c2a6fe21de95f80275e99972d2ad5b9842b06c06a4d3b4640c059c`
retain raw samples and metadata:

| Evidence directory under `target/runtime-proof/` | Moving frames | CPU median | CPU p99 |
|---|---:|---:|---:|
| `verified-scroll-20260916-100327` | 1200 | 0.640 ms | 0.729 ms |
| `verified-scroll-20260916-100458` | 1200 | 0.646 ms | 0.956 ms |

p99 is recomputed from raw `total_us` samples using nearest rank. Layout
dominates this workload. Gallery steady measurements in the same output are
explicitly idle CPU diagnostics and do not count toward active-frame coverage.
Neither case has native GPU/display evidence, so no app qualification changes.

## Live CPU/worker identity validation

`20260916-100852` matched all 50 CPU samples per app to 50 Presented worker
outcomes; `20260916-101012` matched 69 Photos samples and 63 Word Processor
samples. Neither had unmatched, unprofiled or skipped measured worker frames.
This verifies CPU-to-worker identity, not action-to-frame or display timing.
The additional frames in the second run are not attributed to workload inputs.

CPU p99 varied substantially: Photos 10.710 / 6.100 ms, Word Processor
17.016 / 14.963 ms. The first Word Processor run exceeds the CPU budget and is
retained. These are not controlled evidence of a regression caused by identity
instrumentation, nor evidence of a speedup in the repeat. Active native input
correlation and repeatable presentation measurement remain necessary.

The correlation Rust regression, all 20 Python statistics/coverage tests, and
all three verified-scroll example tests passed. Independent correlation review
found no blocker; invalid CPU IDs and unmatched-worker checks were added after
its parser-hardening suggestions.

## Coverage validator

Run `python3 scripts/perf/check-app-coverage.py` from the repository. This checks exact entrypoint coverage, duplicate/stale records, Cargo package and binary identifiers, exclusion reasons, qualification status, and evidence file references. It exits nonzero while any GUI app is unqualified or lacks evidence. The initial expected result is **47 unqualified apps**, not a passing gate.

`qualification_evidence` entries are paths to existing files, relative to the repository root or absolute. The checker validates their existence only. It does not validate their contents, review exclusion decisions, or certify 60 fps; the actual performance gate remains pending. Focused CLI tests run with `python3 scripts/perf/check_app_coverage_test.py`.

### Shared render admission (2026-09-16)

Primary frame construction now defers before consuming state when the fixed
scene/damage pool is busy; worker completion asynchronously wakes the relevant
window. Finite pool and request queue capacities are unchanged. Auxiliary
acquisition is nonblocking too, and failed surfaces use per-window retry delays.
Native scrolling input-dispatch p99 improved from 9.000/10.649 ms to
4.165/4.168 ms over repeated 1200-input samples with verified movement and
matching worker outcomes. CPU samples now exclude deferred GPU backpressure;
these samples cannot establish displayed 60 fps. Worker completion-gap p99
varied from 8.980 to 16.494 ms, and its repeat maximum was 20.601 ms. All 47 apps
remain unqualified. See `docs/native-input-workloads.md` for hashes, failed
experiments, idle validation, and inconclusive occlusion checks.

### Actual Metal presentation evidence

The optional `presentation-feedback` build now records Metal drawable
`presentedTime`, distinct from render-worker completion. Two complete native
scroll runs show full active-frame presentation p99 of 8.3334 ms, but
input-associated presentation p99 of 25 ms. A preceding failed run has 123
explicit zero timestamps despite matching worker-Presented outcomes. Therefore
neither worker submission nor overall animation cadence can qualify visible
scroll updates. All app qualifications remain incomplete. See
`docs/native-presentation-feedback.md` for implementation, raw evidence,
clock/observer limitations and exact cohorts.

## Development hot reload (2026-09-29)

`tilo-app dev --timings` prints where each in-place Rust reload spent its time
and, with `--json`, returns every reload's phases when `dev` stops.
`scripts/perf/dev-reload-bench.py APP` drives it against a headless compositor:
it edits one line of `src/lib.rs` per run and times the save to the first frame
the reloaded code presents. `--budget-ms` makes it fail when the median is over
a budget. The phases are file-change detection (`notify`), `debounce`, `cargo`
startup and fingerprint walk, rustc `compile` and `link`, `stage`, `signal`,
the app's `checkpoint`, `dlopen`, `state_handoff` and `first_frame`, and
`present`.

A scaffolded standalone app (`tilo-app new`, 179-line `src/lib.rs`), built
locally in the Fedora 44 dev container on stable 1.97.1 with `rust-lld`, medians
of five or six saves after a warm-up:

| Phase | Before | After |
|---|---:|---:|
| detection + debounce | ~250 ms | 94 + 31 ms |
| Cargo startup and fingerprint walk | ~1300 ms | 0 |
| rustc front end and codegen | ~2300 ms (with link) | 1911 ms |
| link | | 82 ms |
| stage, signal, checkpoint, dlopen, handoff | ~20 ms | 7 ms |
| first frame + present | | 12 ms |
| **save → first frame** | **4322 ms** | **2138 ms** |

`apps/hot-reload-demo`, inside the workspace, same container:

| Path | Before | After |
|---|---:|---:|
| edit to `src/lib.rs` (rustc replay) | 94.1 s | 2185 ms (compile 1926, link 116) |
| edit Cargo must see (`tests/app.rs`), no app code to compile | ~75 s | 3.5 s |

Before, every Cargo reload in the workspace rebuilt `runtime-dylib` twice: a
step of its own built it with `-C prefer-dynamic`, and the host and library
steps rebuilt it without. That step is gone; a Rust dylib links `libstd`
dynamically without the flag. The remaining 3.5 s is Cargo's startup and
fingerprint walk over the toolkit's ~900 units, which the rustc replay skips.

What changed:

- After Cargo builds the reload library once, `tilo-app` replays its recorded
  rustc command directly for edits confined to files in that library's
  dep-info. Manifests, build scripts, toolkit sources, new files, or a rustc
  failure outside the app's code still go through Cargo.
- The replayed link leaves out the Rust dylibs the running app already has
  loaded. `lld` spent ~390 of ~470 ms parsing the 961,000 exported symbols of
  `libruntime_dylib.so`; the library now resolves them from the host process
  at `dlopen`. Linux only.
- The default debounce is 30 ms instead of 150 ms. A newer save already
  cancels an older build.

Measured and rejected, all on the replayed rustc command: `-C incremental`
(slower: the first run pays the cache, later runs gained nothing measurable),
`-C split-debuginfo=unpacked`, `-C debuginfo=0`, `-C codegen-units=16`, and
`-C opt-level=s`. `-Z share-generics=no` changed nothing either.
`-Z time-passes` shows the remaining floor: an empty crate with the same
`--extern`s spends ~550 ms resolving the toolkit's metadata, macro expansion
takes ~550 ms, coherence plus type checking ~400 ms, and the monomorphization
collector ~600 ms for only ~2,100 items. All of this is the size of the crate
universe that `ui` re-exports, not the app's code, and the parallel front end
that could split it is nightly-only.

Two environment notes. When another program holds most of
`fs.inotify.max_user_watches` (an editor watching a large tree), `dev` falls
back to a 200 ms stamp poll, which is the ~94 ms detection above. On a host
whose `cargo` forwards to a remote builder, Cargo and rustc run remotely, so
the fast path cannot record a local command and every reload pays the remote
round trip (~6 s for a no-op). Sub-second reloads need a local toolchain.
