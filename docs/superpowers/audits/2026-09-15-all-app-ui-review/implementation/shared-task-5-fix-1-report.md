# Task 5 fix 1 report — Native keyboard activation coverage

Status: **DONE**. The short intrinsic card stack now has app-host coverage for a real Enter key event activating its final action exactly once.

The fix began from `cec5c10c7`. The unrelated browser commit `73d21e5fc` advanced the shared branch during validation and is outside this fix's diff.

## Review finding addressed

The first Task 5 regression proved pointer activation, Tab reachability, and accessibility semantic activation. It did not exercise the native host's separate Enter/Space fallback. This fix adds that missing keyboard-path evidence while retaining the original semantic coverage under its accurate accessibility label.

## Implementation

- Added `short_card_stack_scroll_and_tab_activate_final_action_once_from_enter` to `ui/src/app/card_composition_tests.rs`.
- The fixture uses a 360px viewport, a vertical Scrollable, an intrinsic Column, eight automatic ContentCards, and a trailing action.
- The test verifies the action begins below the viewport, sends a real wheel event to reach it, moves host focus to it through Tab order, and sends a synthetic winit Enter press.
- The test asserts the Enter press adds exactly one invocation. It also verifies that the release and an already handled Enter press add no invocation.
- Extracted the existing live `AppHandler` Enter/Space fallback into private `App::handle_unconsumed_activation_key`. The live handler calls that helper at the same point in its event precedence. Its conditions and effects are unchanged: only unhandled Enter/Space presses with a focused actionable widget activate, then mark the event handled and set sync/layout dirty.
- No card sizing, widget behavior, public API, application source, or unrelated routing changed.

## RED/GREEN evidence

RED after adding the host regression first:

- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui app::card_composition_tests::short_card_stack_scroll_and_tab_activate_final_action_once_from_enter --lib`
- Compilation failed with `E0599`: `App` had no `handle_unconsumed_activation_key` test seam for the live fallback.

GREEN after extracting the unchanged fallback and completing the duplicate guards:

- `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui app::card_composition_tests --lib`
- 2 passed: the new short-stack Enter regression and the existing `content_card_host_geometry_focus_click_and_resize_are_coherent_before_paint` regression.
- `rustfmt --edition 2024 --check ui/src/app.rs ui/src/app/card_composition_tests.rs` — passed.
- `git diff --check -- ui/src/app.rs ui/src/app/card_composition_tests.rs` — passed.

Per the fix brief, the broad UI suite and visual captures were not repeated. Task 5's production sizing change remains unchanged from `cec5c10c7`.

## Self-review

- The test does not duplicate the activation fallback: both the live native handler and test route call the same private helper.
- The test sends a real `WindowEvent::KeyboardInput` with `NamedKey::Enter`, `Pressed`, through widget dispatch before the host fallback.
- Release and pre-handled-event assertions protect the exactly-once contract.
- No push or merge was performed.
