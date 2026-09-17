# Template selection and related interaction audit

Scope: the reported Word Processor dark-mode template highlight, its pointer/keyboard routes, related Word Processor menus, and shared menu consumers including Feeds and Slides. This is a targeted source/interaction review, not an exhaustive all-app visual certification. Pre-existing working-tree changes were preserved.

## Repaired: template feedback and input routing

The picker drew a shared `ContextMenu` but received only manually hit-tested clicks. Cursor movement never reached the shared menu, so its hovered row stayed unset in both themes. Keyboard input reached the document beneath it.

`WordProcessorWidget::handle_event` now forwards input to the open template menu before editor dispatch, consumes editing/navigation input while it is open, and maps the shared menu's selection to the existing template action. Reopening resets old hover/selection state.

Verification:

- Two regression tests failed before the fix: missing painted hover and Enter failing to choose/close the menu.
- Three focused tests pass: light/dark hover with 2× pointer scaling; keyboard selection and blocked Backspace; pointer selection, Escape without editor focus, outside-click consumption, and clean reopening.
- Word Processor library suite: 286 passed, 1 ignored, 0 failed.
- Word Processor integration suite: 14 passed, 0 failed.
- A fresh dark-mode offscreen rendering was visually inspected: the Report row has a visible filled highlight. Evidence: `.superpowers/template-menu-audit/screenshots/template-hover-dark.png`.
- Independent scoped source review found no actionable correctness regressions. Whitespace validation passed.

## Additional findings, repaired in the follow-up

The following findings were initially source-reviewed and have since been reproduced with regression tests and repaired. The descriptions below retain the original failure mechanism for context; the suggested repairs have been implemented.

### P1: right-click loses selected text and removes Cut/Copy

Select text, then right-click inside it. `open_context_menu` remembers `had_selection` but calls `caret_from_pointer(content_pt, false)`, which clears the anchor. The previous selection prevents the fallback `select_word`, so `push_clipboard_rows` no longer offers Cut/Copy.

Source: `apps/word-processor/src/widget.rs`, `open_context_menu` around line 10648, `caret_from_pointer` around line 3121, and `push_clipboard_rows` around line 10798.

Suggested repair: preserve the selection for a right-click within it; test selection retention and the resulting Cut/Copy actions.

### P1: context and paragraph-style menus let keys edit the document

Open either menu, then press an arrow key, Enter, or type. Only Escape is intercepted by `handle_key_press_guards`; other keys reach the editor. Cursor movement also never reaches these shared menus, so their hovered rows do not highlight.

Source: `apps/word-processor/src/widget.rs`, `handle_key_press_guards` around line 8438, `handle_cursor_moved` around line 7626, and `handle_left_press_menu_routing` around line 7777; `apps/word-processor/src/style_menu.rs`.

Suggested repair: forward complete menu input and consume editing input while a menu is open, retaining typed action dispatch.

### P2: menus can hide actions beyond the window edge

Right-click near the bottom/right edge. Word Processor's context menu and paragraph-style menu use legacy `open_at`; shared layout places a fixed-width menu below/right of the point without viewport constraints. Some rows can become unreachable. The template picker also uses this legacy geometry and does not measure large-text row heights.

Source: `apps/word-processor/src/widget.rs` around line 10675; `apps/word-processor/src/style_menu.rs` around line 44; `apps/word-processor/src/template_picker.rs`; `ui/src/widget/context_menu.rs`, `layout_at_position` around line 292.

Suggested repair: use existing `open_anchored` / `layout_within` support; verify bottom/right placement, short windows, scrolling, and enlarged text.

### P2: Feeds menu shortcuts act on articles behind it

Open a feed's context menu. Pointer movement updates only the app cursor; there is no menu hover feedback. Down moves the article selection, and `m` toggles the selected article's read state while the menu remains visible.

Source: `apps/feeds/src/app.rs`, cursor handling around line 4326 and `handle_key` around lines 4866–4924. Slides' `route_context_menu` provides an existing example of forwarding input to the shared widget.

Suggested repair: give the open menu priority over article navigation and shortcuts, with tests that article state remains unchanged during menu navigation.


## Follow-up repairs and evidence

- Context and style menus now receive shared pointer/keyboard routing and block background document edits, including host Undo. Viewport constraints keep large menus reachable by scrolling.
- Right-click inside an existing selection retains it, including the selected endpoint, keeping Cut/Copy available.
- Feeds now routes menu input first, requests repaint on hover/dismissal, and prevents article shortcuts behind the menu. Enlarged text uses measured row heights, clipped/scrollable sidebar content and reserved header action space. Its 102-test suite passed.
- Invalid DOCX/ODT imports validate before replacing live content. Failed Open reports a visible error; failed Save As preserves the old document identity and modified state.
- New documents clear the previous path. New/Open with a modified document offer Save, Discard changes and Cancel; Save As failure/cancellation keeps both the draft and pending choice. Resource operations report their terminal result exactly once.
- Native Shift–Option–Arrow and Shift–Command–Arrow select words and visual lines using existing text/layout helpers, including wrapped header/footer lines. Host-level tests select and delete; Unicode and trailing-space regressions exercise boundary behavior.
- At enlarged text sizes, zoom and bottom layout controls reserve measured label dimensions and use those same sizes for hit testing. Optional status/hint text is omitted when it cannot fit.

Evidence is under `.superpowers/ux-repairs/`: regression RED logs, library verification logs, and offscreen screenshots in light/dark palettes and 2× text. This remains a targeted audit of Writer and Feeds interactions; it is not a claim that every application and workflow has been exhaustively inspected.


## Trackpad zoom and disappearing text

Native macOS `PinchGesture` now smoothly changes document zoom within the existing limits. It preserves the vertical reading position under the pointer, uses the existing centered-page horizontal alignment, and ignores cancelled/nonfinite events. Menus and dialogs suppress background zoom. Two gesture regression tests pass; these exercise native event routing in the widget, not a physical trackpad session.

The disappearing-text report reproduced in a 768-paragraph document:

1. At scroll offset zero, the materialized window incorrectly followed a distant caret. Scroll position now chooses the render window even at the top, without moving the caret.
2. Deep scrolling selected a window using continuous text-flow pixels while pagination included page margins/gaps. Shared forward/inverse mapping now accounts for the page stack. Tests cover 12 combinations of scroll position and 0.5×/1×/2× document zoom.

Returning to a prior viewport previously rebuilt 192 shaped paragraph layouts. A bounded recent-layout cache now reuses them: the regression goes from 192 new shapes to zero. Canonical shape keys still invalidate changed words/styles/wrapping; an offscreen edit causes exactly one new shape. The retained cache prioritizes the previous viewport, excludes current duplicate IDs, and keeps total current/recent shapes bounded at 384. This measures eliminated shaping work, not a claimed end-to-end latency percentage. Initial cold layout and variable-height pagination estimates remain areas for further performance work.


## Final verification for this repair pass

- Word Processor: 321 library tests passed (1 ignored), 2 binary tests passed, and 14 integration tests passed, including the non-test library build.
- Shared toolbar: 2 tests passed; shared context menu: 13 tests passed; Feeds: 102 tests passed.
- Focused regressions observed failing before repair cover native word boundaries, pinch routing, missing top/deep text, and repeated shaping.
- Fresh light/dark/large-text renderer screenshots inspected; the enlarged Save prompt, toolbar, zoom control, bottom controls and bounded context menu remain legible.
- Independent scoped source review found no actionable regressions in the file-decision flow, navigation, zoom, coordinate mapping or bounded cache. Scoped whitespace check passed.
