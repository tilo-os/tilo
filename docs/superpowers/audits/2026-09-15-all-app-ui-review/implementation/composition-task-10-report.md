# Task 10 report: Code Editor initial dock composition

## Result

Fresh Code Editor windows now start with the empty bottom dock collapsed. A retained 40×44 disclosure at the trailing edge of the existing 44px tab chrome opens the dock through the catalogue's `command-toggle-bottom-panel` action. It shows an up chevron while collapsed and a down chevron while expanded. Problems and Terminal commands still select and open their requested tabs; closing and reopening preserves the selected tab and live Terminal object.

The trigger exports `ToggleButton` semantics with `checked`, `selected`, and `expanded` state plus dynamic Show/Hide names. Pointer activation, Enter, Ctrl+J, assistive activation, and Escape dismissal all reach the existing command path. Escape returns focus to the retained trigger.

## TDD and verification

All commands used the repository target cache and `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` unless shown otherwise.

### RED / GREEN

1. Added `fresh_editor_collapses_the_empty_bottom_panel`.
   - RED: `cargo test -p code_editor --lib 'app::tests::fresh_editor_collapses_the_empty_bottom_panel' -- --exact` failed at `a fresh editor must give its height to code` while launch state was visible.
   - GREEN: the same exact command passed after changing the fresh default to collapsed.
2. Added `explicit_problems_open_survives_an_empty_diagnostics_refresh` and `dock_commands_open_the_requested_content_and_toggle_without_clearing_it`.
   - GREEN: both scoped tests passed. They cover authoritative explicit opening, empty diagnostics refresh, requested tab selection, and retained Terminal state.
3. Added `retained_dock_trigger_shares_tab_chrome_and_uses_pointer_and_keyboard_routes`.
   - RED: the first run failed because `code.bottom-panel-toggle` did not exist.
   - GREEN: after adding the action-bound retained trigger, pointer, Enter, Ctrl+J, and accessibility activation passed at 480×900.
   - RED: the added Escape assertion failed at `Escape closes the dock disclosure`.
   - GREEN: after adding Escape dismissal and focus return, the same exact test passed.
4. Review changed the ambiguous grid glyph to stateful up/down chevrons. The final exact trigger test passed: `1 passed; 0 failed; 206 filtered out`.

### Final checks

- `cargo fmt --package code_editor -- --check` — passed.
- `cargo test -p code_editor --lib` — `207` run, `206` passed, `1` failed.
  - Failure: `app::tests::a_document_from_a_previous_session_still_stats_the_file`, at the assertion that the buffer path equals `crate::disk::canonical(&file)`: the stored temporary path used `/var/...` while canonicalization returned `/private/var/...` on macOS.
  - This was not rerun on an unchanged baseline checkout because the checkout is shared. Its baseline classification is an inference from the platform path alias and the unchanged disk/session code covered by that test; Task 10 does not touch that path. No unrelated fix was made.
- `cargo build -p code_editor` — passed. Final `target/debug/code_editor` SHA-256: `3be37c17b6e8ec0490d53a0cc688a59cd86353170bc220a97d30293fe1dee74f`.
- `git diff --check -- apps/code_editor/src/app.rs` — passed.

## Capture evidence

Evidence is in `.superpowers/app-ui-2026-09-15/task10-code-editor/`. The committed harness uses `ui::headless::Driver` with the production `CodeApp` and writes a PNG plus retained-tree inspection JSON for each route and viewport.

Harness build/run:

```sh
cp Cargo.lock .superpowers/app-ui-2026-09-15/task10-code-editor/harness/Cargo.lock
CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  CARGO_TARGET_DIR="$PWD/target" cargo run --offline \
  --manifest-path .superpowers/app-ui-2026-09-15/task10-code-editor/harness/Cargo.toml -- \
  .superpowers/app-ui-2026-09-15/task10-code-editor/captures
find .superpowers/app-ui-2026-09-15/task10-code-editor/harness/Cargo.lock -delete
```

The final capture run invoked the built harness directly with task-local `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, and `XDG_STATE_HOME`; those temporary runtime directories were removed afterward. Capture harness SHA-256: `acbae60272a804d59d4c15c616b5219c175e60ac23d30912f7e4634ee35b0747`. Source/capture hashes and per-PNG hashes are in `capture-manifest.json`.

Fresh, Problems, and Terminal were captured at 800×700 and 480×900. Problems was opened by clicking `code.bottom-panel-toggle`, proving the pointer route used the canonical bound action. Inspections show the trigger at `[760, 0, 40, 44]` and `[440, 0, 40, 44]`, aligned and disjoint from the tab bar. Fresh captures omit `bottom_panel` and export `Show bottom panel`, `expanded=false`; open routes include the panel and export `Hide bottom panel`, `expanded=true`.

All six PNGs were visually inspected. Fresh frames reclaim the former 160px dock height. Problems and Terminal remain usable at both viewports, and the chevrons communicate disclosure direction. The Terminal screenshot proves route and geometry only; Terminal state preservation is established by the behavioral regression, not by terminal text in a still frame.

Root reported its final default matrix from the same application binary as a normal/short visual pass. The known 200% row/text overlap remains assigned to Task 13.

## Self-review and limitations

- Only `apps/code_editor/src/app.rs` changes production behavior. `bottom_panel.rs`, shared UI, `main.rs`, and screenshot entry points remain untouched.
- The implementation keeps the existing fixed 160px dock and does not add preference persistence or resize behavior.
- The app-local wrapper delegates retained interaction and canonical action emission to the shared `Button`; its custom layer only supplies dynamic disclosure semantics and the stateful chevron.
- The 200% font-metric finding is intentionally unresolved here and must not be counted as closed by Task 10.

## Review fix round 1

Review found that Ctrl+J closed a focused Terminal before Terminal event routing, but the canonical toggle only flipped `panel_visible`. That left `terminal_focused` set while the dock was hidden. Reopening could therefore return the next ordinary key to Terminal even though the retained disclosure was the visible focus target. Escape had separate cleanup, which caused the inconsistent lifecycle.

The close transition is now centralized in `close_bottom_panel`: both the canonical Toggle Bottom Panel command and Escape hide the dock, clear hidden Terminal focus, update disclosure state, and preserve the selected panel tab plus the live Terminal object. The global keyboard shortcut passes through existing focus plumbing and returns focus to the retained trigger when it closes the dock. Opening Terminal explicitly keeps its existing semantics.

### RED / GREEN

- Added `keyboard_closing_a_focused_terminal_returns_to_the_trigger_without_refocusing_terminal`, which opens the real Terminal route, clicks its content to give it keyboard ownership, closes and reopens with Ctrl+J, and presses Enter on the retained trigger.
  - RED: the exact test failed at `keyboard close returns focus to the retained disclosure`; actual focused node was `Code editor`, expected `Show bottom panel`.
  - GREEN: after centralizing close cleanup and forwarding the keyboard focus target, the exact test passed: `1 passed; 0 failed; 207 filtered out`.
- Extended `dock_commands_open_the_requested_content_and_toggle_without_clearing_it` to assert the same live Terminal allocation and grid content remain across close/reopen.
- Extended `retained_dock_trigger_shares_tab_chrome_and_uses_pointer_and_keyboard_routes` to resize the same open driver from 480×900 to 800×700 and back. It asserts the panel remains open at the fixed height, panel bounds remain inside the viewport, the trigger stays aligned and disjoint from tabs, and expanded/selected semantics survive the resize.

### Focused verification

The following exact tests passed individually with `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p code_editor --lib TEST -- --exact`:

- `app::tests::fresh_editor_collapses_the_empty_bottom_panel`
- `app::tests::explicit_problems_open_survives_an_empty_diagnostics_refresh`
- `app::tests::dock_commands_open_the_requested_content_and_toggle_without_clearing_it`
- `app::tests::retained_dock_trigger_shares_tab_chrome_and_uses_pointer_and_keyboard_routes`
- `app::tests::keyboard_closing_a_focused_terminal_returns_to_the_trigger_without_refocusing_terminal`

`cargo fmt --package code_editor -- --check` and `git diff --check -- apps/code_editor/src/app.rs` passed after formatting. Per the fix brief, the 207-test package suite was not repeated; its existing `/var` versus `/private/var` caveat above remains. Paint and layout behavior did not change, so the previously accepted six screenshots and their binary/source provenance remain the visual evidence; the new same-instance resize behavior is covered by the focused retained-driver regression. Task 13's 200% metric finding remains out of scope.
