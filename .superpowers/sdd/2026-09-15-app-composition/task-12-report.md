# Task 12 — DB Client automatic/manual query pane

## Result

Implemented the DB Client query editor/result split in `apps/db_client/src/app.rs`. The Query tab now derives its automatic editor height from the shared editor's shaped `content_height` and authoritative `line_height`, adds two rows of editing allowance, and clamps only the effective height so a local manual request survives short windows, query runs, and tab changes.

The toolbar, editor, divider, result strip, and grid use one `QueryLayout`. Data and Structure geometry were left on their existing branches. The current fixed grid/chrome reservation remains intentionally unchanged for Task 14.

The shared editor measure pass now runs at the real embedded width before the first drawable frame. A retained, absolute measuring leaf participates in the existing bounded composition settlement without joining visual flow. When the embedded width changes, the app requests one bounded rebuild; the same editor instance survives, preserving caret and scroll state. This repaired a rejected first candidate where the capture path read the editor's one-row pre-shape fallback and clipped the four-line query.

## Interaction and semantics

- Pointer drag uses the painted divider rectangle and keeps capture until release.
- `ArrowUp`/`ArrowDown` change height by 24 logical pixels.
- Accessibility `Increment`/`Decrement` route through the same setter.
- `Home` resets automatic sizing.
- `Enter`, `Space`, right click, or accessibility `ShowContextMenu` opens a bounded context menu.
- The menu exposes Increase, Decrease, and Reset with real pointer bounds and semantic action targets.
- `Escape` closes the menu and returns focus to the divider.
- The divider is a named slider (`db.query-divider`) with value/description text documenting its current Automatic/Manual state and keyboard affordances.

No permanent resize button stack was added. Existing Run and snippet command priority remains ahead of divider/global handling.

## State preservation

The manual request is app-local and is not written into the query CRDT. Behavioral coverage verifies that the requested height survives Run, Query/Data/Structure switching, and a temporary 800×480 clamp. The same regression verifies editor selection plus the grid's actual sort column/direction, horizontal/vertical scroll, and hover row. GridView has no selected-row state, so none was invented.

Retained editor/divider instances are never replaced during events. Width refinement requests composition through `take_structure_dirty`; the host clears stale raw maps before rebuilding, matching the retained-widget lifetime ruling.

## TDD and verification

The initial focused RED was:

```text
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p db_client --lib demo_query_automatic_height_follows_its_content -- --nocapture
```

After fixing the fixture sync, the four-line query failed the new expected-height assertion against the old fractional split. It passed after the first split implementation.

Normal captures then exposed a second RED that the helper-level test did not cover: the editor was only about 66px high because `TextInput::content_height` correctly returns a one-row fallback before shaping. This real-host regression failed before the settlement repair and passed afterward:

```text
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p db_client --lib host_first_frame_measures_query_content_before_painting_the_split -- --nocapture
```

Focused GREEN runs:

```text
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p db_client --lib app::responsive_tests:: -- --nocapture
# 8 passed

CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p db_client --lib divider_ -- --nocapture
# 4 passed
```

Final verification:

```text
rustfmt --edition 2024 apps/db_client/src/app.rs

CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p db_client --lib -- --nocapture
# 79 passed; 0 failed

CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo check -p db_client --lib
# passed

CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo build -p db_client
# passed
```

The final test/check transcripts are in `task12-db-client/cargo-test-db-client-lib.log` and `task12-db-client/cargo-check-db-client-lib.log`.

One earlier focused test attempt encountered a transient external compile error while concurrently owned `ui/src/webview/mod.rs` had three new `WebViewCommand` variants and `ui/src/webview/test_engine.rs` had not yet received its matching arms. No shared file was edited for Task 12. The concurrent owner completed those arms; the Driver regression and final 79-test suite then passed.

## Visual evidence and provenance

Final normal binary SHA-256:

```text
b6034bc385b14fe7d7bb3a8ee63ce94b7c463961568ad6832eb14087f0d94172  target/debug/db_client
```

Final source SHA-256 before commit:

```text
4d78b83def6b8753f3d308da596b2cce17c12477d62a127fe2bdda736ae66b40  apps/db_client/src/app.rs
```

Named captures were made with isolated task-local `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, and `XDG_STATE_HOME`:

```sh
export HOME="$PWD/.superpowers/app-ui-2026-09-15/task12-db-client/runtime/home"
export XDG_CONFIG_HOME="$PWD/.superpowers/app-ui-2026-09-15/task12-db-client/runtime/config"
export XDG_DATA_HOME="$PWD/.superpowers/app-ui-2026-09-15/task12-db-client/runtime/data"
export XDG_CACHE_HOME="$PWD/.superpowers/app-ui-2026-09-15/task12-db-client/runtime/cache"
export XDG_STATE_HOME="$PWD/.superpowers/app-ui-2026-09-15/task12-db-client/runtime/state"

DB_CLIENT_TAB=query target/debug/db_client --shots "$PWD/.superpowers/app-ui-2026-09-15/task12-db-client/query-populated-1200x800.png" --viewport 1200x800
DB_CLIENT_TAB=query target/debug/db_client --shots "$PWD/.superpowers/app-ui-2026-09-15/task12-db-client/query-populated-480x900.png" --viewport 480x900
DB_CLIENT_TAB=data target/debug/db_client --shots "$PWD/.superpowers/app-ui-2026-09-15/task12-db-client/data-populated-1200x800.png" --viewport 1200x800
DB_CLIENT_TAB=structure target/debug/db_client --shots "$PWD/.superpowers/app-ui-2026-09-15/task12-db-client/structure-populated-1200x800.png" --viewport 1200x800
```

All four PNGs were individually inspected. Wide Query shows all four demo lines plus editing allowance and a result grid beginning around y231 rather than the old roughly y392. Narrow Query accounts for the first line wrapping and shows all logical lines. Populated Data and Structure remain unchanged in composition. The wide fixtures show three connections; the existing narrow drawer open/close regression remained green.

Manual resize and reset are proven through the production Driver route rather than a screenshot-only hook: the test focuses the named divider, resizes by keyboard, opens the semantic context menu, uses the menu's published pointer bounds, and clicks Reset. A separate regression covers raw pointer drag and state preservation. Capture entry points were intentionally left unchanged.

The controller's six final standard captures are in `.superpowers/app-ui-2026-09-15/task12-db-settled-20260916/`. The controller independently inspected wide, compact, narrow, short, scale baseline, and true 2× output from the same binary and accepted Task 12 split pixels. The earlier `task12-db-final-20260916` set is explicitly rejected evidence from the pre-settlement binary.

Exact artifact hashes are in `task12-db-client/SHA256SUMS`.

## Known limits and scope

- Task 14 still owns measured result-grid/header/chrome geometry. At true 2×, the existing result-strip/grid/chrome collisions remain visible and are not closed by this task.
- The automatic editor height is bounded by a three-line minimum and by the current fixed 56px grid reservation plus result strip and bottom padding. Very long queries remain scrollable instead of consuming the whole window.
- Manual resize and drawer interaction evidence is behavioral/semantic; no capture-only state hook was added because `main.rs` and shot entry points are outside Task 12 ownership.
- No `session.rs`, shared UI component, capture entry, dependency, Data layout, or Structure layout file changed.

## Fix round 1 — menu integration and mounted acceptance

The scoped review found two menu integration defects. An open resize menu received modified Enter before the app's reserved shortcut path, so `Ctrl`/`Cmd`+`Enter` could select Increase instead of running the query. An open menu also retained the divider anchor and viewport from the moment it opened. The fix routes the existing reserved shortcut handler ahead of menu dispatch and calls the shared `ContextMenu::reanchor` API from Query relayout with the newly computed divider and main viewport. The canonical query run/confirmation path is unchanged, and reanchoring preserves the menu's highlighted item.

The mounted long-query acceptance test exposed one related retained-child focus defect. `DbClientApp::set_focus(false)` cascaded blur into the editor and divider when the host moved focus between individually mapped child nodes. In shared `CompositionFocus::restore`, mapped nodes are visited in `HashMap` iteration order and each node receives its own `set_focus(should_focus)` call; a root entry visited after the target child could therefore blur the child that restore had just focused. Native and headless explicit focus setters likewise target individual mapped nodes. Removing the app root's cascade leaves those host-owned child transitions authoritative and lets a real editor pointer focus remain active, so typing, caret placement, and wheel scrolling work while the automatic pane is clamped. No shared UI or session code changed.

### RED/GREEN evidence

Focused RED runs established the defects before their fixes:

```text
CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p db_client --lib modified_enter_runs_once_ahead_of_the_open_resize_menu -- --nocapture
# RED: query generation did not advance while the open menu consumed modified Enter

CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p db_client --lib open_resize_menu_reanchors_and_keeps_every_action_mapped_after_resize -- --nocapture
# RED: after resize, menu x0 remained 276 while the recomputed divider x0 was 8

CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p db_client --lib mounted_clamped_long_query_remains_editable_and_scrollable -- --nocapture
# RED: real wheel input left scroll_y at 0 because the retained editor had lost focus
```

The final regressions cover:

- An open resize menu plus real `Ctrl`+`Enter`: query generation advances exactly once, requested and effective heights remain unchanged, and no resize item is selected.
- Wide to narrow to short relayout while the menu stays open: the menu and all three semantic rows remain bounded and anchored; the preserved highlighted Decrease row, pointer Increase row, and semantic Reset each invoke the matching production action.
- Eight connection profiles at 480×900: the real drawer trigger opens and closes the overlay, the query split stays stable, and a lower connection row remains bounded and activates the correct profile.
- Sixty SQL lines at 800×480: a real editor click, selection, text input, and wheel event preserve the edit and caret and produce positive editor scroll while keeping at least the 56px usable result region. A manual request then survives 800×700 to 800×480 clamping.

Final verification from the fix source:

```text
cargo fmt -p db_client -- --check
# passed

CARGO_BUILD_JOBS=4 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p db_client --lib -- --nocapture
# 83 passed; 0 failed; 0 ignored

CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo check -p db_client --lib
# passed

CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo build -p db_client
# passed
```

The fix transcripts are `task12-db-client/cargo-test-db-client-lib-fix1.log`, `task12-db-client/cargo-check-db-client-lib-fix1.log`, and `task12-db-client/cargo-build-db-client-fix1.log`.

### Fix visual evidence and provenance

`task12-db-client/menu-open-short-800x480.png` is Driver-rendered evidence of the real open menu after resizing to 800×480. It shows three bounded, legible rows with Decrease highlighted. The connection drawer remains open from the resize sequence, and its scrim dims the query area; this image is therefore an interaction-state capture, not the default collapsed short-window view. The semantic/pointer regression validates all three action mappings in the same route. Capture entry points were not changed.

Fix artifact SHA-256 values (also recorded separately in `task12-db-client/SHA256SUMS-fix1`; the original `SHA256SUMS` remains the provenance manifest for the original candidate):

```text
5b0521ca4b866acb285e0b5d54adc19b6323a8df43fb26eabebaa92a231c8bcb  target/debug/db_client
863bb54b9445d5fbfe2632e8e652a902807971109fbed842508d8e8a7a07cdbb  apps/db_client/src/app.rs
e741540b58d6f3fb283e23ebd89cd9f0d97d55f3839f8873029ea2936c656544  task12-db-client/menu-open-short-800x480.png
5613006f1d0bae3d8ec50619bf849d8dd11f56752db54847d9db82e387af9c40  task12-db-client/cargo-test-db-client-lib-fix1.log
5e699bd569f5b1a0f53ec8b23e3380f5e23dd3ca1191115e56ec1618ac69a269  task12-db-client/cargo-check-db-client-lib-fix1.log
9cbcb1d591c3d83af5b4bfa55a68e6937e70185c907fcdf7e068f5701903d7db  task12-db-client/cargo-build-db-client-fix1.log
```

The controller captured all six default views from the fix binary under a stable expected-binary guard and found them byte-identical to the previously inspected settled defaults. Visual acceptance therefore carries to the fix binary. The known true-2× result-strip/grid/header/chrome collisions remain assigned to Task 14.
