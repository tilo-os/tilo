# Shared Task 4 — measured Toolbar and reachable overflow

## Scope and source

- Verified starting HEAD `fba9ea50f`; worked in the existing `codex/macos-shell` checkout. Concurrent docs/other work continued; no unrelated source was edited or staged.
- Owned production files: `ui/src/widget/toolbar.rs`, `ui/src/widget/context_menu.rs`. No Widget trait changes, dependency changes, pushes, merges, or app migrations.
- Used the supplied brief/preflight and executing-plans, test-driven-development, verification-before-completion workflow. No worker subagents.

## Implementation

Toolbar now caches resolved theme typography, padding, target size and actual renderer-measured item/chip extents. Intrinsic measurement, final fitting and placement share that cache. Renderer input normalization applies theme text scale once. The final lifecycle ignores the public compatibility estimate from `recompute_visible`; narrow allocations remain authoritative and clip glyph paint. Hidden item rectangles are zero, spacers share residual space, and decoration-only suffixes never create overflow.

The owned ContextMenu maps action rows back to original ToolbarItem indices and drains selection exactly once into the original FnMut. Separators are trimmed/collapsed, spacers omitted. The opt-in anchored menu route measures labels/row heights, clamps to viewport, clips paint, scrolls on wheel, adjusts hit testing for scroll, and scrolls keyboard/accessibility focus into view. Existing `open_at` point placement/fixed geometry is retained and covered.

Toolbar remains the retained host focus owner. It handles orientation arrows, Enter/Space, internal menu focus, Escape return, pointer/modal capture and focus trapping. Pointer presses survive unchanged layout passes. Synthetic accessibility supplies named buttons/menu commands, stable original-index action routes, expanded/focus state, and clipped painted row bounds. Resize reconciles hidden indices and preserves a still-hidden focused command; eliminating overflow closes the menu and chooses a reachable focus target.

## RED / GREEN evidence

All Cargo commands used the repository target cache and:

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
```

1. `cargo test -p ui --lib widget::toolbar::tests -- --nocapture`
   - Initial test helper compile error (`Size::MAX_CONTENT` trait not imported) corrected before behavioral RED.
   - RED: 4 passed, 2 failed as expected: equal heuristic widths for `WWW`/`iii`, and Toolbar not focusable.
   - First GREEN: 6 passed.
2. `cargo test -p ui --lib widget::toolbar::lifecycle_tests -- --nocapture`
   - Corrected the new host test helper to rebuild `BoundNodeMap` admission; an empty map intentionally suppresses callbacks.
   - Behavioral RED after helper correction: 5 passed, 2 failed (initial focus incorrectly last; press lost across layout).
   - Fixed focus initialization and preserved reachable hover/press targets across layout.
3. Final `cargo test -p ui --lib widget::toolbar -- --nocapture`: **15 passed, 1 manual GPU fixture ignored**.
4. `cargo test -p ui --lib widget::context_menu -- --nocapture`: **13 passed**.
5. `cargo test -p ui --lib context_menu -- --nocapture`: **15 passed**, including existing TextInput copy/cut menu routing.
6. `cargo test -p ui --lib widget::layout -- --nocapture`: **7 passed**, shared host solver/admission/focus tests.
7. `cargo test -p ui --test it context_menu -- --nocapture`: **1 passed**, existing legacy integration test.
8. `cargo check -p ui --lib`: **passed**.
9. `rustfmt --edition 2024 --check ui/src/widget/toolbar.rs ui/src/widget/context_menu.rs`: **passed**.
10. `git diff --check -- ui/src/widget/toolbar.rs ui/src/widget/context_menu.rs`: **passed**.

The focused cases include Unicode/Arabic/CJK/combining/emoji labels, long translations, `WWW` versus `iii`, both orientations, exact/under/tiny allocations, 1×/2× resolved themes, resize while open, all original callbacks once through pointer/keyboard/accessibility, clipped accessibility geometry, focus return, outside-dismiss click suppression, meaningful separators, wheel limits and offset-aware hit testing.

The macOS lib-test link emitted the existing `__eh_frame section too large (max 16MB)` warning; all reported test/check exits were successful. The first RED invocation briefly waited for an unrelated shared build lock; no separate target cache was created.

## Rendered evidence and provenance

Manual fixture `widget::toolbar::lifecycle_tests::rendered_toolbar_overflow_fixtures` was executed separately using the compiled library test binary, `--ignored --nocapture`, and existing `crate::offscreen::OffscreenRenderer`. It uses the real build → admitted shared solver → apply → draw → keyboard menu navigation → draw_overlay path. Last hidden command activation is asserted once after each capture. Result: **1 passed**.

Runtime isolation:

```text
HOME=/tmp/qos-task4-runtime/home
XDG_CONFIG_HOME=/tmp/qos-task4-runtime/config
XDG_DATA_HOME=/tmp/qos-task4-runtime/data
XDG_CACHE_HOME=/tmp/qos-task4-runtime/cache
XDG_STATE_HOME=/tmp/qos-task4-runtime/state
QUICK_UI_SCREENSHOT_DIR=/Users/xaver/dev/qos-macos-shell/.superpowers/sdd/2026-09-15-measured-ui-components/task-4-captures
```

Command (environment above):

```sh
/Users/xaver/dev/qos-macos-shell/target/debug/deps/ui-98aece3f3d10fdcd widget::toolbar::lifecycle_tests::rendered_toolbar_overflow_fixtures --ignored --nocapture
```

- `task-4-captures/toolbar-ordinary-100.png`: 720×340 viewport, 530px Toolbar, `Theme::dark().with_text_scale(1.0)`, matching renderer scale. Popup below the chip; labels and selected final command fit.
- `task-4-captures/toolbar-narrow-200.png`: 320×260 viewport, 290px Toolbar, `Theme::dark().with_text_scale(2.0)`, matching renderer scale. Menu fills available height, is scrolled to the final command, and stays within viewport. Selected final command is fully readable.
- Worker opened and inspected both images. Controller separately inspected both and confirmed the visual fixture gate passes pending independent code review.
- Captures remain retained in the task evidence directory; they are not app screenshots or live macOS window/focus proof.

SHA-256 at validation/capture:

```text
ff901324b3fb50e313dfe58495b34277d13147f40e71930814ed32dc97f5fd72  ui/src/widget/toolbar.rs
b9b87e5052b19b95137fc0cdfadbd982730c44164685e50a3f730696cbaa5e36  ui/src/widget/context_menu.rs
6fbcc5817a1453721d2075b15492d1e7598f79920c3cdde6b596539f8b547dcd  target/debug/deps/ui-98aece3f3d10fdcd
29808354e98a80611d1b941d6411bfae0071c85acfd9998096a4483e28cd43ac  task-4-captures/toolbar-ordinary-100.png
3c5bb675d1396823df5a286288b9ea3a9caa29772a8e60450fa5be8f22229b68  task-4-captures/toolbar-narrow-200.png
```

## Self-review and limits

- Confirmed only the existing ContextMenu paints/handles the popup; Toolbar owns neither a new overlay stack nor copied callbacks.
- Confirmed `Widget` remains unchanged and existing ContextMenu/TextInput route tests pass.
- Confirmed actual shared host admission/settlement is exercised, not just manually seeded geometry.
- Confirmed explicit narrow allocations and menu viewport containment with ordinary and 200% rendered evidence.
- App-specific consumer migrations and live OS focus/assistive-technology acceptance are outside this shared-widget task. No full workspace suite, app startup, or end-user app screenshot claims are made.
- `open_anchored` is an owned-menu route; callers should run `layout_within` before paint (Toolbar does this in normal draw, then uses the same geometry in overlay/input). Existing retained legacy ContextMenu sizing remains unchanged.
- No known blocking issue remains from scoped tests/self-review. Independent controller code review remains the next gate.

## Commit discipline

Commit only `ui/src/widget/toolbar.rs`, `ui/src/widget/context_menu.rs`, and this report with `git commit --only`. Verify the resulting commit file list; preserve all unrelated working-tree and index content. The returned task status records the resulting commit ID.
