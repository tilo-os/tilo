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
