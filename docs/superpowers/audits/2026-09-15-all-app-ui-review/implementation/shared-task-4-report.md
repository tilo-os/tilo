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

## Fix round 1 — accepted review findings

Verified starting HEAD `2aff2e6c7` and read `task-4-fix-1.md` plus `task-4-review.md`. Used receiving-code-review/systematic-debugging to check the named event branches and AccessKit export contract before implementing the accepted fixes. Production scope for this round is **Toolbar only**; ContextMenu, the Widget trait and AccessKit bridge are unchanged. Concurrent non-owned work was preserved.

### Resolutions

1. **Exported accessibility geometry:** visible command/overflow nodes now use `with_bounds` from their authoritative painted rects. The menu node exports its popup rect; menu items export the clipped ContextMenu row rect. Scrolled-out actions export `Rect::ZERO`, remain semantically focusable/visible, and receive real clipped bounds when semantic focus reveals them. The regression asserts the emitted AccessNode fields themselves, including a partially clipped row, zero-area offscreen row and focus-to-reveal transition.
2. **Stale pointer targets after popup routing:** Toolbar stores the current logical pointer position even while ContextMenu consumes events. Opening clears its old hover/armed target; closing and relayout re-hit-test the stored position. Press/release also re-hit-test before arming/activating. Regression covers keyboard opening from hovered A and pointer opening from hovered Overflow, selection of hidden B, a same-position second click with no CursorMoved, and variants with/without intervening layout. Only B fires once and Overflow stays closed.
3. **Non-left release suppression:** only a left press routed through the popup arms its matching release suppression. Right/middle press-release followed by Escape leaves no pointer capture. A regression uses a real Row with two retained Toolbars, admitted shared layout and Row event dispatch; the subsequent sibling left click reaches its callback once. Existing outside-left-dismiss suppression remains covered by the full Toolbar group.

### Focused RED / GREEN and final verification

Every Cargo command retained the repository target cache and used `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`.

- `cargo test -p ui --lib widget::toolbar::lifecycle_tests::review_ -- --nocapture`
  - **RED: 0 passed, 3 failed** for the exact accepted defects: exported bounds `None` versus painted rect; callback counts `[1,1,0,…]` versus `[0,1,0,…]`; stuck capture after a right press-release and Escape.
  - **GREEN: 3 passed** after the fixes. All requested variants are contained in those tests, including real Row sibling routing.
- `cargo test -p ui --lib widget::toolbar -- --nocapture`: **18 passed, 1 existing manual GPU fixture ignored**.
- `cargo test -p ui --lib context_menu -- --nocapture`: **15 passed**, including legacy TextInput copy/cut routes.
- `cargo test -p ui --lib widget::layout -- --nocapture`: **7 passed**.
- `cargo check -p ui --lib`: **passed**.
- `rustfmt --edition 2024 --check ui/src/widget/toolbar.rs`: **passed**.
- `git diff --check -- ui/src/widget/toolbar.rs`: **passed**.

The known macOS `__eh_frame` lib-test linker warning remains. Shared-target/package-cache lock waits and recompilation of concurrent vendor changes occurred; no build cache isolation or dependency changes were introduced by this worker. The current lib-test binary path is `target/debug/deps/ui-c93bc6d5c202339c`.

Validated Toolbar source SHA-256: `ce4f839a59fb02e1ba0ddf0785722af0c25b1476dd5ffe9d10c17b03a618585e`.

### Self-review and limits

Checked that platform-exported bounds now come from AccessNode output, while fallback click bounds and semantic action routes remain intact. Checked that popup cursor events update the same pointer used after close/layout, and every left press/release activation revalidates geometry. Verified that right/middle events cannot create unmatched release suppression and that the host regression actually reaches the sibling callback through Row.

No layout, typography, popup paint or callback ownership changes were made in this fix round; the accepted rendered captures/provenance above remain retained and were not regenerated. No broad suite or live macOS accessibility/focus acceptance claim is added. Independent scoped re-review remains the next gate.

Round-1 commit is restricted with `git commit --only` to `ui/src/widget/toolbar.rs` and this report; verify its exact file list after committing. Returned worker status records the commit ID.

## Controller archive note

The two original fixture images and their hashes are archived outside the SDD workspace at `.superpowers/app-ui-2026-09-15/task4-candidate-captures/`. They were accepted visually and carried through fix round1 because it changed neither layout nor paint. See `task4-controller-visual.md` and the independent fix-round review.
