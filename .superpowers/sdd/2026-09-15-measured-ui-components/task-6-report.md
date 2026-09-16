# Task 6 — one shaped layout for inline marks

## Result and owned scope

Implemented the single-line rich-mark repair. `RichTextLine` no longer paints marked glyphs over plain glyphs. The inner `TextInput.single_layout` owns attributed shaping, glyph paint, caret stops, selection, scrolling, and pointer inversion. Normal TextInput remains plain unless the internal attributed API is supplied.

Owned production files: `ui/src/widget/rich_text_line.rs`, `ui/src/widget/text_input.rs`, plus the controller-approved narrow live-link fix in `ui/src/widget/block_editor.rs`. No changes to `richtext.rs`, `text_render.rs`, application production files, dependencies, or unrelated dirty files. Starting source base: `90e68a2e9`; unrelated commits continued on the shared checkout.

Controller approved the BlockEditor addition after inspection showed `live_link_hits` refreshed before line drawing, using previous-frame metrics. Its refresh now follows drawing and consumes TextInput's shaped selection rectangles, preserving the existing full-line vertical target. This also supports reversed ranges and separate link segments without joining an unmarked gap.

## Geometry, decorations, and invalidation

- Complete RichSpan runs come from valid UTF-8 mark segments, including unmarked text. Bold/italic shape with the run; code/link use the theme accent. Code retains the existing sans face, two-pixel horizontal chip padding, one-pixel vertical inset, three-pixel radius, and raised-surface color.
- `shape_styled` fills the existing `single_layout`; `draw_rich` emits its glyphs once. Underline/strike ranges use the same layout. Accent link rules and code chips use its `selection_rects`, including IME display remapping.
- Paint order is chip background → local selection → glyphs/underline/strike → accent link rules → local caret. BlockEditor peer presence remains after all lines. An opaque chip therefore cannot erase local selection or subsequent peer presence.
- Attributes refresh at mark mutations, model adoption/sync, local edit reconciliation, accessibility replacement, themed build, measurement, and draw. The wrapper supplies theme scale before measurement. Attributed single-line measurement shapes the current runs while retaining the existing fixed row sizing contract.
- Cache signatures cover span text/boundaries, bold, italic, size, family, foreground/background, baseline shift, underline/strike/chip/link ranges, base family/size, display text, and IME position. Theme scale is applied once; changes to attributed size/family/mono/model text invalidate measurement.
- Existing preedit insertion is shared with the multiline highlighter: preedit stays a default-style display-only run. Committed decorations split/shift around it. IME commit still goes through the real inner event path and mark reconciliation.
- `diff_splice` now retreats common-prefix/suffix boundaries to complete UTF-8 characters. Replacing `é` with `ê` no longer describes a splice inside their shared leading byte.
- Task 2's multiline resolved measurement/model invalidation and Task 7's terminal empty-line normalization were preserved. Their existing module regressions pass.

## RED evidence

All Cargo tests used the existing repository target cache and:

```sh
CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
```

Commands below include that environment prefix when using Cargo. Logs are retained under `.superpowers/app-ui-2026-09-15/task6-after-20260916/verification/`.

1. `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib rich_marks_ -- --nocapture`
   - `task6-red.log`: **0 passed, 2 failed**. `aBz` with bold B painted 4 glyphs instead of 3; adding bold to unchanged WWW left the authoritative caret advance unchanged.
2. `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib attributed_single_line_is_shaped_during_measure_before_draw -- --nocapture`
   - `task6-measure-red.log`: **0 passed, 1 failed**, no layout after measurement. An initial test compilation typo (`Size::MAX_CONTENT` trait import) was corrected before this behavioral RED.
3. `target/debug/deps/ui-c93bc6d5c202339c rich_marks_utf8_replacement_keeps_complete_boundaries --nocapture`
   - `task6-utf8-red.log`: **0 passed, 1 failed**; observed `(1, 1, 1)` instead of complete-character `(0, 2, 2)`.
4. `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib rich_link_hits_follow_current_draw -- --nocapture`
   - `task6-link-red.log`: **0 passed, 1 failed**; first-frame actual BlockEditor lookup at the final link sliver returned None.
5. `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib rich_link_hits_cover_the_shaped_rtl_range -- --nocapture`
   - `task6-link-rtl-red.log`: **0 passed, 1 failed**; endpoint-only bounds rejected the painted Hebrew link range.

## GREEN verification

The new focused tests cover glyph count, shaped weights 400/700/400 before draw, add/remove bold with unchanged text, caret/selection/inversion at 1× and 2× including horizontal scroll, actual IME preedit+commit beside multibyte text, full style inputs, chip/link path bounds, and real CRDT sync/undo/redo. BlockEditor tests exercise the actual first-frame and changed-style/scale link hit cache, RTL, and two link segments around an unmarked mixed-direction gap.

Exact final module commands and outcomes:

| Command | Result | Log |
| --- | --- | --- |
| `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::text_input::tests -- --nocapture` | 105 passed | `task6-text-input-suite.log` |
| `target/debug/deps/ui-c93bc6d5c202339c widget::rich_text_line::tests --nocapture` | 29 passed | `task6-rich-line-suite.log` |
| `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test -p ui --lib widget::block_editor::tests -- --nocapture` | 72 passed | `task6-block-editor-suite-final.log` |
| `target/debug/deps/ui-c93bc6d5c202339c richtext::tests --nocapture` | 20 passed | `task6-richtext-suite.log` |
| `target/debug/deps/ui-c93bc6d5c202339c widget::rich_text_editor::tests --nocapture` | 16 passed | `task6-rich-editor-suite.log` |
| `target/debug/deps/ui-c93bc6d5c202339c widget::code_editor::tests --nocapture` | 23 passed | `task6-code-editor-suite.log` |
| `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo check -p ui` | passed | `task6-check.log` |
| `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo build -p notes --bin notes --message-format=json` | passed | `final-notes-build.log`, `final-build-artifacts.jsonl` |
| `git diff --check -- ui/src/widget/text_input.rs ui/src/widget/rich_text_line.rs ui/src/widget/block_editor.rs` | passed | no output |

Total: **265 relevant module tests passed, zero failed**. The macOS test linker emits its existing `__eh_frame` compact-unwind warning.

A transient unrelated Chromium test edit first blocked compilation (missing NavigationDirection/direction fields). No Chromium source was touched; one retry after its writer supplied matching production edits compiled successfully.

Optional `CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 cargo clippy -p ui --lib` is **not green**: 100 existing errors/12 warnings across the shared tree, including untouched TextInput multiline measurement casts/map_or and unrelated tabs/scrollable code. No diagnostic points to Task 6 added logic. This is retained in `task6-clippy.log`; no blanket lint cleanup was attempted. The scoped compiler check is green.

## Actual Notes interaction and visual evidence

Evidence directory: `.superpowers/app-ui-2026-09-15/task6-after-20260916/`.

`notes-interaction.rs` is a standalone evidence harness, not an app production edit. It links the exact Notes/qos-app artifacts emitted by the Cargo build (`interaction-build-command.json`), creates throwaway workspaces, and drives real NotesApp Widget events against settled Taffy layouts. Runtime HOME and **all four XDG homes** are isolated in `interaction-profile`; Cargo's repository cache remains shared. `interaction-run-command.json` records exact executable arguments and environment. Rendering uses `Theme::with_text_scale` and CPU offscreen paint.

The final run (`interaction-run.log`, exit 0) at both 1× and 2× proves:

- Actual click at the painted bold end places caret at byte 13.
- Actual pointer drag selects exactly bytes 9..13 (`bold`).
- Actual pointer drag selects exactly bytes 18..22 (`code`), and its selection remains visible over the chip.
- A native `SetBlockMarks` action plus normal app sync applies overlapping bold and a valid page link. The actual hit route rejects edge+0.1px; clicking edge−0.1px navigates Notes to the expected Reading list UUID and body.

The fixture explicitly initializes the canonical document and native link action. Earlier exploratory runs retained a nil legacy Markdown placeholder link; their refused navigation correctly exposed an incomplete harness fixture, not a passing click test. Only `interactions-final-verified/` is acceptance evidence.

Fresh accepted screenshots (all 1200×800):

- `interactions-final-verified/caret-1x.png`, `caret-2x.png`
- `interactions-final-verified/selection-1x.png`, `selection-2x.png`
- `interactions-final-verified/code-selection-1x.png`, `code-selection-2x.png`
- `interactions-final-verified/bold-link-1x.png`, `bold-link-2x.png`
- `interactions-final-verified/link-opened-1x.png`, `link-opened-2x.png`

Implementer visually inspected 1× selection and 2× code-selection: single crisp bold, aligned selection, and visible code selection. Controller independently inspected all ten earlier accepted interaction images (`interactions-accepted/`; `interaction-image-comparison.json` compares the final rerun) and six standard actual Notes captures (wide, compact, narrow, large text, scale baseline, short), recording the visual pass in `.superpowers/app-ui-2026-09-15/task6-controller-visual.md`. Standard screenshot manifest: `.superpowers/app-ui-2026-09-15/task6-notes-final-20260916/capture-results.json`.

## Provenance, self-review, and limits

- Final Notes binary SHA256, stable through the final controller captures: `c1f09b8aede7e4a2021a9f0704818a13d0ae739dda8ba782d722bfd8eb655039`. The earlier candidate was `6b5d2cbbb5ed9f4b582606b0aa25686edca09015941e44707f9e8aba7fd45c97`. All six final standard PNG hashes are exactly identical to the earlier individually inspected images; the controller recorded this byte-identical proof.
- `provenance.json` records source HEAD, SHA256 for all three owned source files, standalone harness source/executable hashes, and each accepted PNG hash. `owned-working-diff.patch` records the scoped source changes; Cargo artifact JSON records the linked library graph.
- After the captured binary, only cfg(test) fixture refinement and restoring the original `too_many_arguments` annotation to its original draw helper changed. Neither changes runtime behavior. The final post-review Notes rebuild (`post-review-notes-build.log`) changed the executable hash; the controller therefore recaptured its matrix, and the implementer relinked/re-ran the actual interaction harness against the final library artifacts. Both final interaction scales passed again. All twelve final interaction PNGs (including initial-state images) are byte-for-byte identical to the earlier run; comparison is recorded in `interaction-image-comparison.json`.
- Self-review checked complete span coverage, signature inputs, theme scaling, UTF-8 edit boundaries, IME display/committed offsets, chip/selection/presence layering, unchanged ordinary single-line defaults, model adoption routes, and the final commit scope. The old overpaint context/helpers are removed; no eraser pass or second editor/layout exists.
- A trial styled mixed-direction range was visually contiguous; requiring two selection bands was an invalid fixture assumption. The final tests retain a reversed range and a real unmarked mixed-direction gap between separate link segments. Existing shaping/selection behavior was consumed unchanged; no typography/BiDi engine redesign was introduced.
- Full workspace tests and global clippy cleanliness are not claimed. Independent controller source review remains the acceptance gate. No push, merge, deployment, or unrelated source edits.
