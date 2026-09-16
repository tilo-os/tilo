# Task 2 report: rich editor measurement and scale

## Scope

Implemented the shared rich editor measurement work in:

- `ui/src/widget/rich_text_line.rs`
- `ui/src/widget/text_input.rs`
- `ui/src/widget/block_editor.rs`
- `ui/src/widget/code_editor.rs`

No `Scrollable` production code or `apps/code_editor` files were changed. No screenshots were requested for this task.

## RED evidence

All regression tests were written and observed failing before the corresponding production change.

1. `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui base_font_override_scales_once_across_relayout_caret_selection_and_pointer -- --nocapture`
   - Failed because a base 16px `RichTextLine` shaped at 64px under a 2x theme instead of 32px.
   - This confirmed the `RichTextLine::build_layout` multiplier and the inner `TextInput` multiplier were both active.

2. `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui multiline_measure_uses_the_same_scaled_shaped_rows_as_draw -- --nocapture`
   - Failed with measured height `208.5` while the shaped draw path required `401`.
   - This confirmed `measure` used the approximate `build_lines` model while draw used `RichTextLayout`.

3. `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui scaled_gutter_and_wrapped_rows_share_the_shaped_editor_geometry -- --nocapture`
   - Failed because gutter width remained `39.84` at both 1x and 2x.
   - This confirmed gutter width used an unscaled digit-ratio estimate while line-number glyphs scaled.

4. `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib measured_multiline_pointer_geometry_tracks_applied_origin_before_draw -- --nocapture`
   - Failed with byte `74` instead of the expected second-line start at byte `68`.
   - This confirmed the measured shaped paragraph retained a zero origin after external layout and before first draw.

5. `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib multiline_measure_uses_build_layout_theme_scale_when_renderer_is_stale -- --nocapture`
   - Failed because `build_layout(theme 2x) -> measure(renderer 1x) -> draw(theme 2x)` measured and drew different heights.
   - This confirmed measurement could not rely on the renderer's retained scale when the layout theme was authoritative.

## Implementation

- `RichTextLine` now passes its base font override to `TextInput`; its wrapper row height remains resolved once from the theme scale.
- Single-line caret and selection bands now read the retained shaped layout's actual line metric after draw. This keeps heading, paragraph and inline-code geometry aligned with the shaped glyph run.
- Multiline `TextInput::measure` now shapes a `RichTextLayout` using the same size, family, wrap width and line height used by draw. `content_height`, caret rows, selection rows and pointer hit-testing therefore share one paragraph model.
- `TextInput::build_layout` retains the theme text scale for measurement. Direct measurement without a prior themed layout pass falls back to the renderer scale. A scale change on the same widget marks multiline layout dirty.
- Applying layout refreshes the shaped paragraph origin, including the gutter offset, so pointer placement is correct before first draw.
- Code-editor gutter width now comes from a measured line-number label plus scaled padding. The resolved line-number style is converted through `TextRenderer::renderer_input_style`, avoiding a second renderer scale during paint.
- Numbered block prefixes use an extracted origin calculation tied to the same resolved line-height band as their editable text.

## GREEN verification

Focused regressions passed:

- `cargo test -p ui --lib base_font_override_scales_once_across_relayout_caret_selection_and_pointer -- --nocapture`
- `cargo test -p ui --lib multiline_measure_uses_the_same_scaled_shaped_rows_as_draw -- --nocapture`
- `cargo test -p ui --lib measured_multiline_pointer_geometry_tracks_applied_origin_before_draw -- --nocapture`
- `cargo test -p ui --lib multiline_measure_uses_build_layout_theme_scale_when_renderer_is_stale -- --nocapture`
- `cargo test -p ui --lib scaled_multiline_geometry_refreshes_across_model_undo_redo_replay -- --nocapture`
- `cargo test -p ui --lib scaled_gutter_and_wrapped_rows_share_the_shaped_editor_geometry -- --nocapture`
- `cargo test -p ui --lib heading_paragraph_inline_code_and_numbered_prefix_scale_once -- --nocapture`

The final current-source library test executable was produced by the last focused Cargo run, after all Task 2 source changes. Running its relevant module filters directly avoided rebuilding unrelated concurrent test edits:

- `target/debug/deps/ui-98aece3f3d10fdcd widget::text_input::tests --nocapture` — 99 passed.
- `target/debug/deps/ui-98aece3f3d10fdcd widget::rich_text_line::tests --nocapture` — 25 passed.
- `target/debug/deps/ui-98aece3f3d10fdcd widget::code_editor::tests --nocapture` — 22 passed.
- `target/debug/deps/ui-98aece3f3d10fdcd widget::block_editor::tests --nocapture` — 69 passed.

Build and formatting checks:

- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo check -p ui` — passed.
- `rustfmt --edition 2024 --check ui/src/widget/rich_text_line.rs ui/src/widget/text_input.rs ui/src/widget/block_editor.rs ui/src/widget/code_editor.rs` — passed.
- `git diff --check -- ui/src/widget/rich_text_line.rs ui/src/widget/text_input.rs ui/src/widget/block_editor.rs ui/src/widget/code_editor.rs` — passed.

The test link emits the existing macOS `__eh_frame section too large` warning. It does not fail the build or tests.

## Self-review

- Verified base-versus-resolved ownership: `RichTextLine` and `TextInput` overrides remain base values; only the shaped layout and wrapper geometry resolve the active theme scale.
- Verified renderer-relative conversion is limited to gutter painting and measurement; direct `RichTextLayout` shaping receives resolved sizes.
- Verified the same widget instance transitions 1x -> 2x and 2x -> 1x, and model replay equivalent to undo/redo invalidates and restores wrapped geometry.
- Verified actual caret, selection, wrapped logical rows, pointer events, inline-code marks, heading/paragraph sizes and numbered-prefix alignment rather than row-height constants alone.
- Preserved all unrelated dirty and untracked files. Only the four task widget files and this report are intended for the scoped commit.

## Remaining gaps

- No application capture was produced in this implementer task; the controller owns Notes, Calendar and corrected Typst integration captures.
- The full UI suite was intentionally not repeated because the task brief directs focused editor filters and records unrelated Chromium churn. All relevant editor module tests and `cargo check -p ui` are green.

## Review fix round 1

### RED evidence

The two review findings were reproduced with production-lifecycle regressions before their fixes:

1. `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::code_editor::tests::wrapper_layout_uses_theme_scale_with_a_stale_renderer_and_reverses_to_normal -- --exact --nocapture`
   - Failed at `wrapper measure and draw must share the 2x paragraph height`.
   - The test drives the wrapper through Taffy `build_layout`, measured layout computation, `apply_layout`, and `draw` with a renderer left at 1x. This confirmed that `CodeEditor::build_layout` discarded the 2x theme scale before delegating measurement.

2. `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::text_input::tests::crdt_sync_undo_redo_invalidates_multiline_measurement -- --exact --nocapture`
   - Failed at `CRDT text growth invalidates retained measurement` after `take_dirty()` returned `None`.
   - The test creates a real Automerge `Text`, commits a wrapping replacement through `State`, synchronizes the bound input, and then performs real `State::undo` and `State::redo`. This confirmed that path-bound model adoption changed text without setting `model_layout_dirty`.

### Implementation

- `CodeEditor::build_layout` now forwards the resolved theme text scale through a dedicated `TextInput::capture_layout_text_scale` method before registering the wrapper leaf. The existing direct-measure fallback to the renderer remains intact for callers that have no themed build pass.
- Scalar values, Automerge `Text` values, removed paths, and direct bound-text adoption now share `TextInput::adopt_model_text`. A changed multiline value sets `model_layout_dirty`; path synchronization remains a model update rather than a user edit, while direct bindings preserve their existing reconciliation dirty signal.
- Corrected the `ensure_shaped_layout` comment to describe its actual unit return and rebuild inputs.
- Corrected the gutter-width comment to distinguish retained glyph measurement from the pre-measure estimate.
- Replaced the earlier `set_text` undo/redo surrogate with the real CRDT commit/undo/redo regression above.

### GREEN verification

Focused regressions on the final implementation:

- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::code_editor::tests::wrapper_layout_uses_theme_scale_with_a_stale_renderer_and_reverses_to_normal -- --exact --nocapture` — 1 passed, 0 failed.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::text_input::tests::crdt_sync_undo_redo_invalidates_multiline_measurement -- --exact --nocapture` — 1 passed, 0 failed.

Affected module tests on the formatted final source:

- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::code_editor::tests -- --nocapture` — 23 passed, 0 failed.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::text_input::tests -- --nocapture` — 99 passed, 0 failed.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo check -p ui` — passed.
- `rustfmt --edition 2024 ui/src/widget/code_editor.rs ui/src/widget/text_input.rs` — passed.
- `git diff --check -- ui/src/widget/code_editor.rs ui/src/widget/text_input.rs` — passed.

The test link continues to emit the existing macOS `__eh_frame section too large` warning; it does not fail compilation or the test executable.

### Self-review and remaining gaps

- Confirmed the wrapper test catches both the original 2x mismatch and same-instance 2x-to-1x invalidation while the renderer remains stale at 1x throughout.
- Confirmed the CRDT regression consumes initial measurement, keeps model sync out of `take_dirty`, and requires a new measurement after replacement, undo, and redo.
- Confirmed unchanged model text does not mark measurement dirty, and removed paths use the same change comparison before invalidation.
- No app files or `Scrollable` production code changed. Controller-owned image review and separately queued app geometry remain outside this fix round.
