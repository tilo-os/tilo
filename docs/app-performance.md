# Application performance coverage

Improve measured shared-framework bottlenecks first; move to app-specific optimization when controlled measurements show the remaining dominant work is app-specific or shared experiments no longer yield repeatable gains.

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
| [`ui::launch`](../ui/src/launch.rs) | Common app lifecycle and `--headless-drive`. `osk` and `scratchpad` explicitly disable generic headless driving. |
| [`ui::headless`](../ui/src/headless.rs) | Real widget event dispatch, named targets, bounded waits, semantic assertions, and component hosts. It does not establish native presentation cadence. |
| [`ui::profiling`](../ui/src/profiling.rs) | `PerfScenario`, scroll/type/key/click/frame steps, warmup exclusion, and CPU frame capture. Reuse this instead of duplicating app benchmark loops. |
| [`ui::ux_bench`](../ui/src/ux_bench.rs) | Structured input, wheel phases, checkpoints, and semantic/visual checks. Existing [`scripts/ux-bench`](../scripts/ux-bench) scenarios cover desktop journeys, not all app workloads. |
| [`ui::offscreen`](../ui/src/offscreen.rs) | GPU measurement primitives and image validation; offscreen completion alone does not establish displayed cadence. |
| [`perf_scenario`](../ui/examples/perf_scenario.rs) | Synthetic list-scroll and gallery controls for shared-framework regressions. |
| [`runtime_benchmark`](../shell/examples/runtime_benchmark/mod.rs) and its [reporter](../scripts/perf/runtime_benchmark.py) | Live Photos/Word Processor action workload and shared timing summaries. This is only part of the full workload matrix. |
| [`frame-loop-bench.py`](../scripts/perf/frame-loop-bench.py) | Compositor frame-loop instrumentation; Linux compositor backends do not establish macOS app presentation. |
| [`benchmark-host.py`](../scripts/browser/benchmark-host.py) | Browser-specific host measurements; preserve engine coverage separately from native widget chrome. |

Calculator, Monitor, Music, and Tube already have app-specific benchmark flags. Many apps register named screenshot scenarios. Those are starting points for fixture construction, not performance passes.

## Remaining coverage work

1. Extend shared launch/scenario handling to run per-app input workloads using existing app preparation. Keep fixture data and expected outcomes in declarative scenarios, with an explicit suite runner tracking every app and failure.
2. Seed representative content. Empty lists, short documents, static thumbnails, and screenshot-only poses do not cover sustained scroll/edit/media work. Network, device, PTY, remote-frame, and media apps need controlled inputs plus native integration checks.
3. Require observable input effects. `PerfStep::ClickNamed` now fails on a missing or empty target instead of discarding that failure; a regression test also verifies delivery to a real control. Per-app state/pixel assertions remain necessary. Headless `Frames(n)` runs without a real-time schedule, so it cannot stand in for timed animation.
4. Correlate active inputs, frame identities, worker outcomes, and presentation evidence. Preserve skipped/acquire-failed frames and distinguish occlusion/background behavior from foreground qualification.
5. Add explicit OSK and Scratchpad adapters. OSK requires an input-method client; generic simulated typing is not equivalent to that integration. Browser, media, camera, and terminal fixtures likewise need their actual engines/providers where applicable.

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

### Pointer motion keeps the published tree

Every input event used to drop the resource cache, and for a framework app
every mutable record loan (each event dispatch takes one) advanced the bound
owner's admission generation, which keys the bound resource cache. Each pointer
move therefore rebuilt the whole publication and reprojected every control's
availability. Now bare pointer motion (no button held, nothing handled, no
focus change) keeps the tree when neither the app's document nor any bound
owner's record moved and no widget reported a presentation change
(`widget::note_resource_presentation_changed`, which the context menu calls
when hover opens or closes a nested menu). The bound snapshot is carried to the
owner's new generation. Hover also skips the pre-dispatch availability
projection, since hover activates nothing and every frame refreshes
availability again before it paints. Every other event republishes as before
(key releases and modifiers keep the tree too since #4148, below).

`cargo run --release -p tilo-app --features test-support --example
framework-scaling` mounts a checklist of N tasks (title and checkbox per row) in
a real `App` and sends 100 `CursorMoved` events through `App::surface_input`.
Run on an Apple M5 Max, macOS 27, release build. Publication p50 is one full
`resources_for` of the root:

| Tasks | Resources | Publication p50 | Move p50 before | Publications per 100 moves before | Move p50 after | Publications per 100 moves after |
|---:|---:|---:|---:|---:|---:|---:|
| 10 | 11 | 0.010 ms | 0.046 ms | 100 | 0.007 ms | 0 |
| 250 | 261 | 0.83 ms | 1.08 ms | 100 | 0.091 ms | 0 |
| 1,000 | 1,251 | 17.5 ms | 12.7 ms | 100 | 0.37 ms | 0 |

With `TILO_PROFILE_RESOURCES=1` at 1,000 tasks, the resource work left per move
is the two `input_carry` spans: 8 µs before dispatch and 8 µs after it, p50,
with all 100 decisions `hit`. Before the change each move spent 11.4 ms in
`bound_discovery` and 0.55 ms in `bound_control_projection`. The 0.37 ms that
remains is widget dispatch and hit-testing, which also grows with the number of
rows. Each frame still runs the full availability refresh in
`draw_frame_content`. With an unchanged tree that refresh hits every cache, but
discovery still walks the bound owners and projection still visits every
control.

### A keystroke publishes twice

Traced through the hosted path (`App::surface_input` for the key's press and
release, each followed by the host's frame, `App::surface_frame`, and its
accessibility walk, as `EmbeddedApp` runs them), one keystroke into a bound
field built the whole published tree four times: the bound-action door's
broker-routing pre-check resolved the edit in a tree of its own, the door
resolved it again, the frame after the edit republished, and the key's release
dropped the tree so the frame after it republished once more. Now the door
resolves the action once and asks the host, before anything runs, whether it
goes through the broker (`ResourceActionRequest::route`); a broker-bound action
comes back unexecuted and takes the broker path as before. A key's release, a
modifier key's press and a modifier change are inert input like hover: unless
consumed, and when no document, bound record, focus or reported presentation
moved, they keep the tree. A local invocation of a bound action (a control,
the palette, an agent) therefore builds the tree once.

The `framework-scaling` example types 20 keys into the draft of the same
checklist, press → host frame → release → host frame, and reports p50 from
press to the end of the frame showing the key, the most publications one
keystroke ran, and p50 of the time one keystroke spent publishing. Apple M5
Max, macOS 27, release build, median of three interleaved runs on a shared,
loaded machine:

| Tasks | Publications before | after | Publishing per keystroke before | after | Keystroke→frame before | after |
|---:|---:|---:|---:|---:|---:|---:|
| 10 | 4 | 2 | 0.027 ms | 0.019 ms | 0.47 ms | 0.47 ms |
| 250 | 4 | 2 | 0.248 ms | 0.194 ms | 1.67 ms | 1.65 ms |
| 1,000 | 4 | 2 | 1.045 ms | 0.793 ms | 5.42 ms | 5.11 ms |

The first keystroke after pointer input publishes once more: it moves the
window to keyboard modality, which may change what it presents. The headless
`FrameHarness` still counts three, because its undo-status discovery after
every relayout is uncached. Publication is 0.8 ms of the 5 ms a keystroke
takes at 1,000 tasks; the rest of the frame was not profiled here.

The hosted input-cost regression scopes monotonic time to one instant while
it drives the real press/frame/release/frame path. The record-save status
poll is an independent 100 ms idle deadline; under CI load it can otherwise
fall inside that observation and add a publication unrelated to input. A
separate regression advances that scoped clock to the deadline and requires
the real save-status frame to publish while preserving the typed draft. The
clock override is test-support only, thread-local, and restored on unwind;
production save sampling and clock readings keep their existing behaviour.

### A frame that changes nothing no longer walks the published tree

`sample` on the keystroke loop at 10,000 tasks
(`TILO_SCALING_KEYSTROKES=400`) put layout and draw at 0.4 % of the samples.
92 % went to two consumers of the bound resource snapshot that ran on every
frame, changed or not: control availability (before each non-hover input and
in every `surface_frame`) deep-copied the cached snapshot, re-filtered every
action by permission and built a descriptor per action to learn which
addresses are available; the host's accessibility refresh copied it again and
rebuilt and diffed a semantic node per resource. Now the snapshot's tree is
shared (`Rc`), so a cache hit copies nothing; the controls' allowed addresses
are derived once per snapshot, permission witness and host doors, read in
place; and the semantic tree keeps each component's resource nodes while the
component hands over the same snapshot. A frame that changes nothing costs
the same at 10,000 tasks as at 10.

The bench now also prints p99 from press to frame and p50 from the key's
release to the end of the frame after it. Same machine and method as above:

| Tasks | Keystroke→frame p50 before | after | p99 before | after | Release→frame p50 before | after |
|---:|---:|---:|---:|---:|---:|---:|
| 10 | 0.42 ms | 0.39 ms | 0.54 ms | 0.54 ms | 0.23 ms | 0.18 ms |
| 250 | 1.37 ms | 1.01 ms | 1.56 ms | 1.15 ms | 0.79 ms | 0.22 ms |
| 1,000 | 4.60 ms | 3.10 ms | 4.93 ms | 3.35 ms | 2.63 ms | 0.25 ms |
| 10,000 | 59.9 ms | 42.9 ms | 64.2 ms | 45.8 ms | 35.5 ms | 0.40 ms |

`publication_per_keystroke::a_hosted_frame_that_changes_nothing_costs_the_same_whatever_the_list`
holds the release frame at 1,000 tasks within 2× of 100 (it measured 4.2×
before). The frame that shows a key still grows with the list: the edit
changes the snapshot, so it publishes twice, rebuilds the
accessibility tree's resource nodes and the controls' allowed addresses once,
and `ResourceRoot::nested_views` builds every child once more. Making that
flat needs publication that shares unchanged children between snapshots.

### The frame that shows a key stays flat: a shared published tree

After #4176 the frame that shows a typed key still grew with the list,
although the key edits the draft, which no item depends on. `sample` at
10,000 tasks: 58 % rebuilding and diffing a semantic node per item, 25 %
rebuilding, copying and permission-filtering every item resource on the
frame's cache miss, 12 % the same in the door that resolves the draft's edit,
14 % `nested_views` building every item once more. `tilo_actions::Resource`
owns its children inline, so no consumer could tell an unchanged item from a
changed one.

`ui_core::published::SharedResource` holds the same tree with its children
behind `Rc`s, beside `tilo_actions::Resource` (whose shape does not change):

* **Publication.** `ResourceRoot` builds the document tree from
  `Resource::children_parts` (`#[resource]` splits its item collections from
  its other children) and keeps each item child while its collection field,
  id and label are the ones it was built with; collections unchanged since
  the last version reuse the whole list. It answers
  `PublishesResources::shared_resources_for` when nothing in the view
  decorates the children (no surface, no content resource joining the root),
  and the ordinary tree is materialised from the shared one.
* **Snapshot and filtering.** A bound component's cached snapshot is the
  shared forest; the ordinary trees are materialised on first use for the
  palette, agents and search. Filtering keeps its result on each input node
  and list of children under a token that stands for the component's
  permission witness, so a subtree an edit did not touch is not filtered
  again. A new witness (a grant changing or expiring) takes a new token.
* **Controls and accessibility.** The controls' allowed addresses (counted per
  address) and a component's semantic nodes move from one forest to the next
  by a pointer diff: shared subtrees are skipped, nodes whose own part changed
  are rebuilt and compared, added and removed subtrees are visited. A
  repeated id rebuilds the component, as the full walk reports it.
* **The door.** A local document edit resolves its node in the shared tree,
  exactly the one the filtered tree would give, without building or
  filtering the rest; every check after resolution runs as before. Other
  kinds, participants' trees and the unbound fallbacks take the whole tree.

M5 Max, release, `TILO_SCALING_KEYSTROKES=40`; after is the median of three
runs (machine load 15–20 during them, about 3 for before):

| Tasks | Key press→frame p50 before | after | p99 before | after |
|---:|---:|---:|---:|---:|
| 10 | 0.40 ms | 0.41 ms | 0.50 ms | 0.58 ms |
| 100 | 0.63 ms | 0.45 ms | 0.72 ms | 0.62 ms |
| 250 | 1.00 ms | 0.46 ms | 1.13 ms | 0.61 ms |
| 1,000 | 3.01 ms | 0.53 ms | 3.34 ms | 0.72 ms |
| 10,000 | 37.5 ms | 1.44 ms | 40.8 ms | 1.75 ms |

`publication_per_keystroke::a_hosted_keystroke_costs_the_same_whatever_the_list`
holds key press→frame at 1,000 tasks within 2× of 100 (4.7× before; 1.2× now
in release, 1.1× in a debug test build). What remains at 10,000 is linear with
a small constant: each of the two publications per keystroke still reads every
item's id and label from the state (`Fields::item_collections`) to compare the
collections, 0.54 ms of publishing per keystroke in all.

### Opening a long list

`framework-scaling -- 5000,50000 open` reopens the checklist over a record
already holding N tasks, five times, to its first laid-out frame. Each size
also prints a `FRAMEWORK-OPEN` line, which the scheduled lane's render job
records as `render.framework_open.{ms,allocations,heap_per_item}.<size>`
(`tools/perf/adapters.py framework-open`). Apple M5 Max, macOS 27, release,
three interleaved runs of each build, p50 of five opens:

| Tasks | Open before | Open after | Heap held before | Heap held after |
|---:|---:|---:|---:|---:|
| 5,000 | 155–156 ms | 120–122 ms | 28.5 MB (5,472 B/task) | 24.7 MB (4,698 B/task) |
| 50,000 | 1,820–1,836 ms | 1,163–1,174 ms | 638 MB (12,739 B/task) | 224 MB (4,454 B/task) |

Two storage costs were behind most of it. Appending an item gave it an order
key halfway between the last key and the end, so keys grew by a character
every six appends: the 50,000th task's key was 8,334 characters, 208 MB of
order keys in all, which the record load re-encoded and hashed, the read
copied and the sort compared. Keys toward an open end now step instead
(`tilo_resource::order::between`): 50,000 appends need at most seven
characters, and a key minted the old way keeps its length. And the collection
read looked up every key of every item separately (about six Automerge
lookups per item); it now walks each map once (`tilo_resource::read_slots`).

What remains at 50,000, from `sample`: `AutoCommit::load` 40 % (Automerge
rebuilds and hashes every change to learn the change graph; the titles alone
are 2.75 million text ops), the collection read 21 % (one object lookup each
for the entry, the item and its title text), layout and the bound-control
refresh 18 %, and closing the previous window 10 %.

### Typing in a note repaints its line

`apps/notes/tests/typing_damage.rs` drives the real Notes window (its record
and saver included) the way the desktop runs an in-process tile: input through
`App::surface_input`, frames through `App::surface_frame_with_damage`, and the
app's scene appended to a host part hinted with its damage and compared part
by part (`ui::scene_parts`). Every frame is presented sparsely over the one
before it and compared with a full render, in dark, light and high contrast.
Pixels repainted per frame of a 1280×900 app (1,152,000 px):

| Change | Before | After |
|---|---:|---:|
| A key after the previous key's save landed | 1,152,000 (a rebuilt tree) | ≈ 48,000 (the line, the caret, the save bubble) |
| The save landing between keys | 1,152,000 (a rebuilt tree) | ≈ 25,000 (the bubble) |
| A key a minute after the last edit ("Edited" turns over) | 1,152,000 (a rebuilt tree) | ≈ 81,600 (the line, the bubble, the dates line) |
| A key in the title | ≈ 816,000 (the page panel) | ≈ 62,500 (the title's band) |
| The pointer onto a toolbar button | ≈ 1,131,000 (the app below the header) | ≈ 1,200 (the button) |

The desktop's 4K Notes tile repainted 7,022,592 of 8,294,400 px per key
before (`frame_retained_tests::typing_in_notes_repaints_what_changed_not_the_tile`
now holds a key and a title letter under 5 % of the output).

What was behind it:

- A record's save status flipping (Saved → Saving on a key, back when the
  saver lands) marked a framework root's *structure* dirty, so the next frame
  rebuilt the widget tree and repainted the window. The root now syncs again
  with a changed status in the same pass, and an `App` presents it with a relayout
  (`component_lifecycle::take_save_status_presentation`); a component mounted
  straight in a `TileHost` still sees it as a structure change.
- Notes drew its shell, page tree, save bubble and toasts as one opaque scene
  beside the note, so the bubble or a hover in the shell repainted the app.
  Each is now a damage part with its own proof; the shell's empty column
  slots no longer claim their columns as needing a redraw.
- The page's title was outside its parts. It is a part of its own now.
- The "Created … · Edited …" line rebuilt the header's widgets (and so the
  tree) whenever its words changed. Same-length changes now retext the built
  line (`NoteHeader::retext_captions`).
- A text field's correction chip floats outside the field, where neither the
  block editor's per-paragraph damage nor the title's covered it: sparse
  frames left it out or left it behind. Fields report it (`TextInput::overhang`)
  and the editor and the title damage it where it is and was.

Headings, quotes, list and checklist items and rules now damage their row of
the editor too.

### Code blocks, tables, tags and page renames repaint what changed

The same test drives a code block (in a note of four lines of prose and a
two-line run), a table, tags and renames (same window, same three themes, same
pixel comparison). Largest frame per change, of the 1,152,000 px app (most
include the save bubble, ≈ 25,000 px):

| Change | Before | After |
|---|---:|---:|
| A key in a code run's first line | 183,252 (the editor) | ≈ 59,300 (the line, the bubble) |
| A key in a later code line | 213,898 (the editor) | ≈ 58,500 |
| A key in prose in a note holding code | 213,898 (the editor) | ≈ 48,600 (as in any note) |
| A key in a table cell | 1,152,000 (a rebuilt tree) | ≈ 57,500 (the cell, its editor, the bubble) |
| A second tag joining the header's row | 1,152,000 (a rebuilt tree) | ≈ 92,200 (the header, the bubble) |
| The first tag (the header grows, the page moves) | 1,152,000 (a rebuilt tree) | ≈ 817,800 (the page panel) |
| A key in the page tree's rename field | 229,577 (the tree's column) | ≈ 12,300 (the field) |
| Naming the note in its title (Enter) | 315,041 (the title, the tree's column) | ≈ 94,200 (the title, the row, the caret's line, the bubble) |
| Committing the rename | 318,105 (the tree's column, the title) | ≈ 101,400 (the row, the title, the caret's line, the bubble) |

What was behind it:

- The block editor kept per-block parts only while *no* block was code or a
  table, so one code block anywhere made every key repaint the editor. A code
  run's ground (drawn by its first block) is now a part of its own; each code
  line damages its line. A table is a frame part, one part per cell (each
  row's card when the cells stack), the focused cell's editor and the controls.
  Formula runs still repaint the editor.
- A key in a table cell set the editor's *structure* dirty, rebuilding the
  window's tree per key. The table's line measures the table from its text, so
  the key now asks for a relayout.
- A header that rebuilt its widgets (a tag, a caption line) rebuilt the
  window's tree, and a rebuilt tree repainted the window. A widget can now say
  its structural change is described (`StructureChange::Described`): its own
  damage covers what its new children change. The window still rebuilds the
  tree, but keeps the frame's damage, adding every widget box that came, went
  or moved, matched by identity across the rebuild. Any other reason to
  rebuild in that frame (`LayoutRuntime::request_rebuild`, a
  `StructureChange::Rebuild`, a history or viewport recomposition) repaints
  the window as before; a decorator or container forwards `Described` only by
  overriding `take_structure_change`.
- The page tree (`TreeView`) had no damage of its own: anything in it repainted
  its column. A settled tree now records one part per visible row and one for
  the rename field; motion, a drag, an open menu, delegated rows and navigation
  tiles still repaint the tree. Each scene it keeps opens and closes its own
  layers: splitting the body's clip layer across two appended scenes rendered
  stray pixels at a row's corner on Metal.

Still open: a native window repaints whole while a blurred floating surface
(the save bubble) shows.

### Enter and Backspace repaint the blocks that moved

The same test presses Enter and Backspace in a note of four paragraphs (and a
list), each key after the previous edit's save landed and the bubble faded, in
the three themes, every frame compared with a full render. Largest frame per
key, of the 1,152,000 px app (each includes the save bubble, ≈ 25,000 px):

| Key | Before | After |
|---|---:|---:|
| Enter splitting a paragraph | 1,152,000 (a rebuilt tree) | ≈ 114,400 (the rows from the split down, the drag handle, the bubble) |
| Enter ending a paragraph | 1,152,000 (a rebuilt tree) | ≈ 92,900 |
| Enter adding a list item | 1,152,000 (a rebuilt tree) | ≈ 213,700 (the rows from the new item down) |
| Backspace joining two paragraphs | 1,152,000 (a rebuilt tree) | ≈ 91,800 |
| Backspace removing an empty paragraph | 1,152,000 (a rebuilt tree) | ≈ 114,400 |

What was behind it:

- A block that came, went or changed its kind set the editor's *structure*
  dirty, so the window rebuilt its whole tree. The block stack is one measured
  leaf and blocks are not widgets, so the editor now *restacks*: it prepares
  its lines again (`BlockEditor::prepare_blocks`, what a rebuild did for them)
  and asks for a relayout. A block that brings or takes an image's button
  still rebuilds. A collaborator's block arrives the same way
  (`present_blocks`).
- `PaintDamage` matched parts by position and gave up when one came, went or
  moved. Parts are now matched by key; a part whose damage covers everything
  it paints (`PaintPart::placed`: a block's row, a cell, the drag handle, a
  code run's chrome) repaints where it was and where it is. Parts that only
  certify where their scene changes (`PaintPart::new`) still repaint whole
  when they come, go or move.
- The editor's frame geometry was its rect, which a new paragraph grows, and
  the drag handle beside the caret's block and a code run's chrome were drawn
  outside its parts. The geometry is now the editor's top edge and width, and
  the handle and chrome are placed parts.
- A window repaints where a relaid widget's box was and is; the editor's box
  grew with every paragraph. A widget can say its own damage covers changes to
  its box (`Widget::damage_covers_own_box`, after its draw); the editor does
  whenever it recorded a paint. The note panel keys the editor's part by its
  top edge and width, and the scrollbar, whose thumb moves with the note's
  height, is a part of its own.

The rows below the edit still repaint (the region moved): Enter near the top
of a long note repaints the visible note, not the window. A copy of the moved
pixels in the renderer would avoid that.

### A scroll copies what stays on screen

`framework-scaling -- 5000 scroll` scrolls the checklist of 5,000 tasks down
120 frames of 40 px (and of 40.5 px), the pointer resting over the list, each
frame presented on an offscreen GPU target as a window presents it and the
last checked against a full render. Release, M-series Mac (Metal), 1280×800:

| | Repainted px p50 (max) | Copied px p50 | Whole-window frames | CPU frame p50 | GPU present p50 (full render) |
|---|---:|---:|---:|---:|---:|
| Before | 1,024,000 (1,024,000) | 0 | 120 / 120 ("history mode") | 0.44 ms | 1.65 ms (1.64 ms) |
| After, a host that does not copy | 983,616 (983,616) | 0 | 0 | 0.51 ms | 1.68 ms (1.67 ms) |
| After, the native window (copies) | 99,792 (115,872) | 756,288 | 0 | 0.52 ms | 1.72 ms (1.67 ms) |
| After, 40.5 px steps | 843,168 (843,168) | 0 (refused) | 0 | 0.52 ms | 1.68 ms (1.67 ms) |

A long note (`apps/notes/tests/scroll_damage.rs`, 120 paragraphs, 1280×900)
repaints 114,084 px per frame scrolling down by 40 px and 92,602 scrolling up
by 32, copying every frame, in all three themes; before, every frame repainted
the window (1,152,000 px).

What was behind it:

- A scroll moved every row, and the native window repaints whole for moved
  geometry. A relayout whose only moves are scrolls now stays sparse: a
  scroller offers the move as a copy (`ui::scroll_copy`), the window plans it
  against the tree it drew and checks every other box (`moves_besides`).
- A framework window's History toolbar was a "layer" in every frame the
  window drew without scene parts, so every native frame of a framework app
  repainted whole. History now draws into its own scene, compared with the
  presented frame's.
- `Each` damaged its whole list when any row was dirty and every `Checkbox`
  damaged itself on every frame; a remount during a scroll was an undescribed
  rebuild. The list now unions its rows' damage, a checkbox repaints when what
  it paints changed, and a remount is described.
- Notes' page scrolls by hand: the panel offers its body (paragraphs and
  headings; other blocks' hover chrome is not followed), and the app repaints
  its save bubble, which floats over the note, on every copied frame.

On this GPU the raster stays about as long as a full render: the frame's
fixed stages dominate at this size, and only the fine raster shrinks with the
damage. The CPU rasterizer a software adapter (lavapipe, VMs) uses
rasterizes only the damage, so there the frame shrinks with the pixels. The
compositor still repaints where the copies landed: the client's pixels there
changed. The nightly render job records `render.framework_scroll.*` and
enforces the pixels.

Since then the shell's in-process apps copy too (below), and so do
`VirtualList` (rows a scroll brings near are mounted in a rebuild the frame's
damage describes; rows move without a rebuild), a painted host scrolling a
`ScrollRegion` that says only its offset changed (`ScrollRegion::offer_copy`;
Mail's message list), `DocumentViewport` around a widget, and Notes pages
with lists, to-dos, code runs, tables and images: what a block paints for the
pointer rather than the text (`BlockEditor::pointer_chrome`) is repainted
where it is and was. A note of eight sections of a paragraph, a list, a to-do
and a code run repaints 119,560 px per frame scrolling down and 96,402
scrolling up (1280×900, all three themes, pixel-identical); before, it
repainted the window. `Toggle` repaints only when what it paints changed.

### In-process apps copy too

The shell's in-process apps (Notes, Files, Messages in a tile) hand the
compositor what a scroll moved, and the DRM presenter copies those pixels in
its retained target before it rasterises the rest (`ui-rendering-internals.md`).
`framework-scaling -- 5000 scroll` also shows the checklist in a host's scene
as the shell shows a tile (`FRAMEWORK-SCROLL-HOSTED`): 985,280 px per frame for
a host that does not copy, 158,040 px p50 (174,838 max) with copies, 756,288 px
copied, pixel-identical (M-series Mac, 1280×800 region). The nightly render job
enforces `render.framework_scroll.hosted_pixels.5k`. The shell's own test
(`frame_scroll_copy_tests.rs`) scrolls a 60-paragraph note on the 1600×1000
desktop at 100 % and 200 % with a peer's pointer resting over it and checks
every presented frame against a whole render.

Not copied: several displays at once (each draws the desktop at its own place
and scale), a desktop under materials or HDR layers, a strip pan, and the CPU
presenter some boards use.

### Copies go on while collaborators are on screen

A native window used to refuse every copy while a collaborator's pointer,
name or activity was painted over it, so a shared document scrolled at the
no-copy cost. The presence layer's marks are now their own damage: where the
copy took the marks of the frame on screen, and where this frame paints them,
repaint (`scroll_copy::marks_over_copies`); the content beneath still copies.
`framework-scaling -- 5000 scroll` scrolls the window with a peer's pointer
and name resting over the list (`FRAMEWORK-SCROLL-PEER`; release, M-series
Mac, 1280×800, pixel-identical):

| | Repainted px p50 (max) | Copied px p50 | Whole-window frames |
|---|---:|---:|---:|
| A peer on screen, before (every copy refused) | 983,616 (983,616) | 0 | 0 |
| A peer on screen, after | 109,168 (125,248) | 756,288 | 0 |
| Alone, for comparison | 99,792 (115,872) | 756,288 | 0 |

The nightly render job enforces `render.framework_scroll.peer_pixels.5k`. A
peer's dot also claimed 96 px square of damage for a 23 px mark; its bounds now
cover what it paints, which trims every peer-pointer frame too. In the shell,
the Notes tile with a peer's pointer over it now rasterises 215,456 of
1,112,672 px per frame scrolling down (19.4 %, was 223,648) and 180,480 up.

An in-process app planned its copies in whole logical pixels, so on a 200 %
display a step of half a logical pixel repainted the tile. The compositor now
says how many device pixels a unit is (`HostCopies::begin_scaled`) and the app
plans in those: half-pixel steps at 200 % rasterise 527,850 of 4,450,688 px
per frame (11.9 %), and whole steps 693,328 down and 552,832 up (were about
725,000 and 584,832).

### Notes input latency on a 27-note notebook

`cargo run --release -p notes --example notes-latency` opens the real Notes
window over a notebook it writes with Notes' own actions (27 notes, 838
blocks: headings, paragraphs, list items; the note under test holds 136) and
sends each input through `App::surface_input`, then draws the host's frame
(`surface_frame_with_damage` until at rest, then the accessibility walk) and
rasterises that frame's damage on an offscreen GPU target, waiting for
completion. Each sample is checked through the Notes editor observer: the
canonical body and the drawn body agree and changed by exactly the input, or
the run fails. It stops at the GPU; the compositor, KMS and the panel are the
installed-desktop measurement's (`scripts/perf/device_notes_latency.py`,
[native typing measurement](native-typing-measurement.md)). The nightly render
job records it as `render.notes_latency.*` (reported against the 60 Hz frame).

M5 Max, macOS 27, release, 1280×900 at scale 1, three runs of 200 keys back
to back on a shared machine (load 11–16), ms:

| Input | To frame p50 | To frame p99 | To raster p50 | To raster p99 |
|---|---:|---:|---:|---:|
| A character | 4.2–5.4 | 7.2–8.8 | 7.2–8.5 | 13.6–17.3 |
| Enter (splits a block) | 4.8–5.3 | 8.3–12.4 | 8.2–8.8 | 12.6–19.1 |
| Backspace (joins it back) | 5.3–5.8 | 8.4–11.8 | 8.9–9.3 | 12.7–18.2 |
| A 40 px wheel step | 2.7–2.9 | 4.6–4.9 | 5.1–5.4 | 7.6–8.4 |
| Opening a note (`open` action) | 4.7–5.2 | 6.4–7.9 | 8.3–8.6 | 10.2–13.8 |

Keys 100 ms apart (`interval=100`, the host drawing frames between them)
measure the same: 4.2 ms to frame p50 typing. Every workload fits a 60 Hz
frame at p99 to frame; to raster p99 reaches the frame on a loaded machine.

What a keystroke costs, from `sample` during 1,500 keys typed back to back
(shares of the time from key press to rasterised frame):

* Input handling, 36 %: the rich-text splice (10 %) and the author mark over
  the typed character (21 %, `tilo_document::action::Written::attribute`):
  three Automerge `query_insert_at` seeks per key, two of them the mark's
  begin and end. The mark expands after, so a key typed right after the same
  author's text is already covered by it.
* The frame, 40 %: the hosted model pump (14 %), in which `Following::apply`
  clones the whole `Notebook` (every note's body) and `NotesApp::adopt` drops
  the previous projection, a cost that grows with the notebook rather than the
  edit; the accessibility walk (11 %); layout and draw for the rest.
* Rasterising the damage, 24 %, mostly waiting for the GPU.

Typing 1,500 keys into one block raised to frame p50 from about 4.3 to
8.4 ms: the block reshapes as it grows, and each key leaves three operations
in the document.

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

### Shared render admission (2026-09-16)

Primary frame construction now defers before consuming state when the fixed
scene/damage pool is busy; worker completion asynchronously wakes the relevant
window. Finite pool and request queue capacities are unchanged. Auxiliary
acquisition is nonblocking too, and failed surfaces use per-window retry delays.
Native scrolling input-dispatch p99 improved from 9.000/10.649 ms to
4.165/4.168 ms over repeated 1200-input samples with verified movement and
matching worker outcomes. CPU samples now exclude deferred GPU backpressure;
these samples cannot establish displayed 60 fps. Worker completion-gap p99
varied from 8.980 to 16.494 ms, and its repeat maximum was 20.601 ms. See `docs/native-input-workloads.md` for hashes, failed
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

A small app inside the workspace, same container:

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
