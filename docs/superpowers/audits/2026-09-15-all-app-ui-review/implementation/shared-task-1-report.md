# Task 1 report — Resolved text sizing and bounded EmptyState

Status: **DONE_WITH_CONCERNS**. Resolved text styles now cross one public normalization boundary, while legacy renderer-relative styles keep their existing behavior. EmptyState measures and draws the same wrapped geometry, exposes an explicit retained scrolling route, and does not report a wholly invisible action as a usable target.

## Scope and implementation

- Added `TextRenderer::renderer_input_style`, which converts a fully resolved logical-pixel `TextStyle` into renderer-relative input by dividing both font size and letter spacing by the sanitized renderer text scale. Existing `measure`, `measure_style`, `draw_text`, and `draw_style` semantics remain unchanged.
- Reused that boundary for resolved `Text` sizes while restoring `Text`'s established renderer-relative letter-spacing behavior. Base and explicit legacy style paths are unchanged.
- Added opt-in `TextWrapMode::WordOrGlyph`. EmptyState uses it so a long localized word cannot exceed the measured pane; existing wrap and balance modes retain their word-wrap behavior.
- EmptyState now resolves title, message, and action styles once, measures and draws with the same bounded inner width, centers the measured block, and grows the action pill from its wrapped label height with the existing 34px minimum.
- Preserved decorative shedding order: icon first, then explanatory message. The action remains essential.
- Added `EmptyState::minimum_essential_height` for immediate parents and `EmptyState::into_scrollable()` for retained composition. Retained layout uses automatic height with a 100% minimum, so roomy panes keep centered content while a physically short viewport gets real overflow. Immediate paint anchors essential overflow at the pane top and returns no action rectangle when the action is wholly outside the supplied pane; this means the parent has supplied insufficient visible space and must use the documented scrolling contract, not that the action was removed.

The task was specified against `7231a8170`. The shared branch advanced through unrelated Chromium/browser and documentation/application commits while work was in progress; the last observed pre-commit HEAD was `f6ab9c6e7`. Those changes remain outside this task's scoped diff.

## RED evidence

Production code was changed only after these focused regressions failed:

- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib resolved_styles` — failed to compile because `TextRenderer::renderer_input_style` did not exist. After correcting the test's unsupported direct `Vec<vello::Glyph>` equality comparison, it remained RED solely on the missing API.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib long_localized_content_wraps_inside_a_narrow_scaled_pane` — failed because the old unbounded action measured approximately `Rect { x0: -492.75, x1: 772.75, .. }` in a 240px pane.
- The tiny-pane regression showed the old centered essential block and action outside the viewport, with no retained scrolling route.

Baseline compatibility checks before implementation were green:

- `cargo test -p ui --lib text_scale` — 14 passed.
- `cargo test -p ui --lib theme_resolved_sizes` — 1 passed.

## Focused verification

All final commands used the shared Cargo target cache.

- `rustfmt --edition 2024 --check ui/src/text_render.rs ui/src/widget/text.rs ui/src/widget/empty_state.rs ui/src/app/card_composition_tests.rs` — passed.
- `git diff --check -- ui/src/text_render.rs ui/src/widget/text.rs ui/src/widget/empty_state.rs ui/src/app/card_composition_tests.rs` — passed.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::empty_state::tests` — 16 passed.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib text_scale` — 15 passed.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib theme_resolved_sizes` — 1 passed.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib resolved_styles` — 1 passed.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib letter_spacing` — 2 passed.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib scrollable_empty_state_action_uses_the_real_tab_and_enter_host_route` — 1 passed.
- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo check -p ui --lib` — passed.

The EmptyState module covers 240px and 480px panes at 1× and 2× text, long localized title/message/action wrapping, growing action height, bounded glyph origins and action geometry, immediate tiny-pane minimum height, no wholly invisible hit target, ordinary centering, retained decoration, scrolling to a visible action, pointer activation, and semantic accessibility activation.

The app-level regression separately exercises the real host input path: it lays out a 50px retained viewport, sends a wheel event through widget dispatch, traverses host focus with Tab, then sends Enter through `App::handle_unconsumed_activation_key` and observes the document action. This is distinct from the lower-level `perform_action(Action::Activate)` semantic activation coverage in the EmptyState module.

The linker emitted the existing `__eh_frame section too large` warning during test links.

## Broad UI suite

`CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui` was run once after the main implementation. The library suite reported 3,090 passed, 1 failed, and 2 ignored; the failure stopped integration binaries from running:

- `webview::chromium::tests::rejected_navigation_is_visible_and_does_not_change_current_uri` failed because its concurrent Chromium expectation required a `LoadFailed` error containing `"not queued"`.

That test and its implementation were concurrently dirty outside the Task 1 files. The previously documented Task 5 integration baselines were not reached because the library failure stopped the command. Per controller direction, the broad suite was not repeated after the retained-only test adjustment.

## Visual evidence

`CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo build --locked -p vpn` passed using the shared target cache. Each VPN run used separate task-specific `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`, and `XDG_STATE_HOME` values and exited 0. The empty-state captures are:

- `.superpowers/app-ui-2026-09-15/task1-after-20260916/captures/normal.png` — 1200×800, SHA-256 `1921079a8f0bdf9335d1e3d65d85075c17d1e94dd27e8e2c4c299815bae8b669`.
- `.superpowers/app-ui-2026-09-15/task1-after-20260916/captures/narrow.png` — 480×900, SHA-256 `3f7de52a0b50bcb638fc1a376744520438ef08fe9af2bbf8842c2ee2c132d5b2`.
- `.superpowers/app-ui-2026-09-15/task1-after-20260916/captures/large-text.png` — 800×900 at 2× text, SHA-256 `6f918055933b14afc5b1c4281458769919b05da4d36131753e0dd71bb504ba98`.

The inspected binary SHA-256 was `531ca60b96bccab20beaa9e8e4c87facef320ad7340b8dbecdcf666183c142e4`. All three PNGs were inspected and their dimensions verified with `file`; EmptyState remained centered and bounded at ordinary, narrow, and 2× text sizes.

## Self-review and remaining concerns

- Measurement and draw share the same normalized styles and width. The returned action rectangle is the same rectangle used for painting, pointer handling, focus, and accessibility.
- The retained route is explicit and opt-in, leaving manual parent sizes compatible. A short immediate-mode parent must use `minimum_essential_height` to provide an actual scrolling owner.
- No broad caller migration, global padding change, font shrink, hidden action, dependency update, push, or merge was introduced.
- The 2× VPN capture also shows an existing top Tor status string exceeding its capsule bottom. The controller classified that as residual application Task 22 work, outside Task 1's EmptyState scope.
- Concurrent Chromium test churn remains the only observed broad UI blocker.
