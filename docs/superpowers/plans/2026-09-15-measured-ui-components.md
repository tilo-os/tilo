# Measured UI Components Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Repair shared sizing and interaction contracts so app authors get readable, balanced layouts with fewer manual measurements.

**Architecture:** Add one resolved-text normalization boundary while preserving existing relative APIs. Repair EmptyState, rich editor, opt-in scrolling editor fill and measured actionable Toolbar overflow. App composition migrations consume existing Card/Actions/FormPage rather than inventing another layout system.

**Tech Stack:** Rust, Taffy, cosmic-text, Vello and Tilo Widget lifecycle.

**Spec:** `docs/superpowers/specs/2026-09-15-app-ui-balance.md`

## Global Constraints

- Reuse the repository Cargo target cache; do not rebuild all dependencies into a new target directory without a concrete build-isolation need. Isolate runtime captures with task-specific HOME and XDG_CONFIG_HOME/XDG_DATA_HOME/XDG_CACHE_HOME/XDG_STATE_HOME, separately from build caches.

- Prioritize ordinary100% and narrow layout as well as200% text.
- Preserve primary actions, keyboard access, state and explicit manual dimensions.
- Keep legacy renderer-relative font APIs compatible; theme-resolved fonts must scale once.
- Do not globally change primitive padding or reinterpret explicit fixed sizes.
- Preserve unrelated working changes and untracked Scratchpad ownership.
- No pushes, merges, publishing or dependency upgrades.
- Existing linked worktree /Users/xaver/dev/tilo-macos-shell; sequential implementers and scoped reviews.
- Focused RED/GREEN behavior regressions before implementation; run relevant existing UI tests once before scoped commit. CARGO_BUILD_JOBS=4, CARGO_PROFILE_DEV_DEBUG=0, CARGO_PROFILE_TEST_DEBUG=0.
- Every task report includes exact commands/results, self-review, screenshot paths if requested, and remaining gaps. No subagents from implementers.

### Task 1: Resolved text sizing and bounded EmptyState

**Files:** `ui/src/text_render.rs`, `ui/src/widget/text.rs`, focused module tests.

Verified current source: `TextRenderer::scaled_style` (1367) multiplies size and letter spacing; `measure_style` (1470) and `draw_style` (1833) use renderer scaling. `Theme::with_text_scale` already changes typography tokens (`theme.rs` around 736). `Text::renderer_input_style` (375) privately divides resolved size by renderer scale. No `FontSize` enum found in current non-vendor Rust; recheck before editing because other work may add one.

- [ ] Add `pub fn renderer_input_style(&self, resolved: &TextStyle) -> TextStyle` on TextRenderer, returning a clone with `size` and `letter_spacing` divided by sanitized `self.text_scale()`. Document that input is final logical pixels; theme typography is already resolved. Leave `measure`, `measure_style`, `draw_text`, `draw_style` semantics intact. If a concurrent explicit FontSize API exists, consume it instead of adding this method.
- [ ] For immediate drawing use one normalized style for both calls: `let style = tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm)); let (w, h) = tr.measure_style(label, Some(width), &style); tr.draw_style(scene, label, color, transform, Some(width), &style);` Do not mutate global renderer scale around a draw.
- [ ] Keep Text's explicit/base/fixed semantics and cache invalidation. Reuse the public normalization for fully resolved styles only; examine letter-spacing semantics before replacing the existing private size-only adjustment wholesale.
- [ ] Tests: legacy14px at renderer2 measures as28px; resolved28px measures identically at renderer1 and2; resolved letter spacing also stays identical; theme ui1.25/text2 scales once; same resolved style produces identical measurement and shaped glyph bounds. Existing `text::tests::font_size_ownership_is_resolved_once_by_its_owner` (case `theme_resolved_sizes_are_independent_of_renderer_scale`) is a useful regression anchor.

**Gate:** `cargo test -p ui --lib text_scale`; `cargo test -p ui --lib theme_resolved_sizes`. Add/run the new focused test filter. Do not broad-rewrite callers whose literals are intentionally base sizes.

#### EmptyState consumer

**Files:** `ui/src/widget/empty_state.rs`; tests in that module.

`paint_empty_state` (137 onward) takes already-scaled title/message tokens through relative APIs, measures all text with no width, uses34px action height, and centers potentially wider-than-pane content. `Fit::choose` removes icon then message but can still produce negative top when title+action exceed height. VPN review-3 documents the visible4× symptom; both retained and immediate entry points already share this function.

- [ ] Normalize title, message and button styles once using the resolved-style helper in this same task; measure with inner width `max(rect.width()-2*s4, 1)` and draw with the same width. Center the measured wrapped block, not an unbounded single line.
- [ ] Compute action height from measured label line height plus vertical padding, min existing34px; wrap long labels within inner width. Preserve returned action rectangle as the sole paint/hit/focus geometry.
- [ ] Retain icon shedding. Prefer keeping explanatory message when it fits after wrapping. If essential content exceeds available height, anchor at pane top and expose a scrolling retained route rather than silently deleting the action. Immediate paint must document its minimum essential height; app parent supplies scroll for physically insufficient panes. Do not shrink fonts or return an invisible action hit target.
- [ ] Tests: width240/480, scales1/2, long localized title/message/action; glyph and action bounds fit width, action grows, painted action equals pointer target; Tab/Enter existing action route remains functional; tiny heights shed decoration first and do not place hit geometry above pane.

### Task 2: Rich editor measurement and scale

**Files:** `ui/src/widget/rich_text_line.rs`, `ui/src/widget/text_input.rs`, `ui/src/widget/block_editor.rs`; Scrollable fill is separately owned by Task3; do not edit it in Task2.

Confirmed double scaling: RichTextLine::build_layout at442 calls `inner.set_font_size(self.font_size * theme.text_scale)`, but TextInput::ensure_shaped_layout at1581 again computes `size * theme.text_scale`. Its comment claiming the first multiplier is needed is misleading. BlockEditor constructs RichTextLine from base `BlockKind::font_size`; list-prefix painting uses base fonts, so do not globally divide BlockEditor fonts.

- [ ] Set RichTextLine inner override to base `self.font_size`. Keep row height resolved once. Test actual shaped run font size, cursor position and selection bounds after layout+draw, not row-height-only assertions. Heading, paragraph, inline code and numbered prefix must align at1 and2. Cover scale changes on the same editor instance, undo/redo and pointer caret placement.
- [ ] Include CodeEditor/TextInput gutter number sizing in the same scale contract: corrected Typst200% capture shows oversized numbers overlapping wrapped lines. Measure number width/line height once; keep logical line numbers aligned to visual text rows, including wrapped lines, pointer caret and selection. Add regression through the existing CodeEditor wrapper.
- [ ] Audit TextInput multiline measurement: `measure` (3546) uses build_lines/content_height while draw uses shaped layout. Establish a single height basis for measured rows and the shaped draw path; add long wrapping line tests where content heights and caret line indexes agree at2.

### Task 3: Opt-in full writing surface

**Files:** `ui/src/widget/scrollable.rs`, scoped tests; minimal `apps/scratchpad/src/lib.rs` opt-in only after framework test passes.

**Interfaces:** Produces `Scrollable::with_content_min_viewport_height()` builder, default false, preserving intrinsic overflow and existing explicit child dimensions. Consume existing measured layout hooks. For explicit fixed-height children, preserve fixed contract rather than force viewport min; document this distinction.

- [ ] Scratchpad's38px editor is **not** inherently a wrong multiline minimum: TextInput::build_layout uses auto height and measures content; Scrollable::fill_parent_height grows viewport only (scrollable.rs972/995). Empty short fields should stay compact in forms. Add opt-in `Scrollable::with_content_min_viewport_height()` (or equivalent existing API if discovered) setting the direct child's minimum height to available viewport during measured layout, while retaining intrinsic height for overflowing content. Default false. The Widget prepaint hook explicitly permits changing only its own node style: do not mutate the child node through that hook. Implement the minimum via an owned wrapper with its own hook/inherited viewport decision, or a bounded layout-protocol extension reviewed separately. This is preferable to globally expanding multiline inputs.
- [ ] Prove empty editor fills viewport, long editor exceeds viewport and scrolls, short form multiline remains content-sized, footer stays visible, clicking lower empty area focuses/positions caret. Then one-line Scratchpad opt-in on its existing Scrollable chain (`apps/scratchpad/src/lib.rs:116–124`) preserves original source and all action bindings. Coordinate that app edit with controller.

- [ ] Preserve original Scratchpad file backed up at `.superpowers/app-ui-2026-09-15/scratchpad-original/lib.rs`; minimal diff only, do not stage pre-existing untracked app/manifests. Save the app-only patch in this task's report workspace; framework changes may be committed independently. The user explicitly authorized improving every app; owner task is finished, so no permission pause.
- [ ] Recapture Scratchpad wide/narrow/200%, inspect empty writing area, edit and save via existing action binding, then reopen isolated profile and verify persisted body. Confirm field remains scrollable for long content and footer stays reachable.


- [ ] Integration-discovered first-wheel symptom: the Scratchpad capture harness fails to move the outer scroll area. A visible final-line fixture now reproduces1079.375px content inside1079px bounds, consuming the first wheel in0.375px internal overflow. Earlier trailing-empty-line fixtures did not reproduce the same content extent. Keep exact behavioral evidence and normal theme construction. Scope includes a narrowly verified intrinsic multiline measurement correction in `ui/src/widget/text_input.rs` (round-up containment is a candidate, not a substitute for diagnosis). Add a real first-wheel regression and preserve explicit/manual heights, single-line behavior and current shaped caret/selection/measurement guarantees. Do not globally alter scroll event consumption. Report the source addition and exact RED/GREEN evidence.


- [ ] Verified second-click focus defect: Scrollable currently preassigns its own focus before child dispatch; an already-focused TextInput consumes the press without assigning focus again, so typing is lost. Defer background focus fallback until a child leaves the press unhandled; retain scrollbar focus, explicit child focus changes and outside-pane behavior. Test repeated click/typing with opt-in and default Scrollable plus existing pane/bar focus cases.

### Task 4: Measured Toolbar with reachable overflow

**Files:** `ui/src/widget/toolbar.rs`, scoped additions in `ui/src/widget/context_menu.rs`; reuse existing Widget overlay/focus hooks (`ui/src/widget/mod.rs`) without changing that trait. Do not confuse widget Toolbar with `ui/src/chrome/toolbar.rs`.

**Preflight:** `.superpowers/sdd/2026-09-15-measured-ui-components/task-4-preflight.md` maps the existing APIs and concrete gaps. ContextMenu currently has point-only placement, full unbounded height and no scrolling. Extend it with a bounded anchored route, measured row/label geometry, clipping, scrolling and offset-aware hit testing; retain legacy callers' placement behavior unless they opt into the new route. This is shared menu infrastructure, not a second Toolbar-specific popup. Keep host focus on Toolbar and route internal focus into/out of the owned menu through existing hooks. Synthetic menu accessibility must include action targets and painted bounds, not just nested labels. Tests must cover the existing legacy menu behavior as well as the new route.

Three separate approximate sizing paths currently disagree: `measure_item_axis`147, `decide_visible_count`214 and `measure_visible_axes`252. apply_layout loses theme and falls back to character count and16px labels. draw uses actual renderer measurement. `is_overflow_open`202 explicitly says host paints the popover; the widget supplies no reachable hidden command route, and its accessibility node is only Role::Toolbar.

- [ ] Cache theme font/padding during build_layout, then actual label width/height during `adjust_layout_with_measure` (Widget hook, mod.rs602; Form/Field demonstrate usage). Use one cached item extent vector for visibility, placement and intrinsic sizing. Reserve measured overflow control before fitting commands. Invalidate on theme/scale/label changes. Retain public recompute_visible compatibility, but avoid using its approximate result as authoritative final layout.
- [ ] Implement overflow using existing ContextMenu selection drain: menu maps each visible menu entry back to original ToolbarItem index; use `take_selection()` to call the original FnMut, preserving command ownership. Include only hidden buttons and meaningful separators. Widget draws it through draw_overlay and routes popup input before underlying buttons. Reuse dismissal/positioning infrastructure; do not create another floating-window system.
- [ ] Add focusable command/overflow semantics, keyboard opening, arrow navigation, Enter activation, Escape close and focus return. Popup must remain within host bounds and scroll if taller than viewport. A toggled boolean is not acceptance.
- [ ] Tests with `WWW` versus `iii`, non-Latin text, long translations, both orientations, widths less than chip width, scale changes and resize after opening. Every original callback reachable exactly once through pointer and keyboard; invisible item rectangles zero; separator-only overflow prohibited; accessibility exposes commands.
- [ ] Retain actual rendered test-fixture images at ordinary width and narrow2×, overflow open and final hidden command reachable. Reuse the tested widget/renderer path rather than adding an app; record source/binary/theme/viewport provenance for controller visual inspection.


### Task 5: Intrinsic card and section minimum propagation

**Execution order ruling:** Run before Tasks1–4, immediately after App Composition Task3 has saved its independent app edits. Fresh visual review exposed this missing dependency; App Task3 acceptance waits for this shared fix.

**Files:** `ui/src/widget/card.rs`, `ui/src/widget/card_composition.rs`, focused framework/harness tests. Inspect but preserve unrelated `column.rs` formatting. Do not change application files in this task.

**Diagnosis:** Read `.superpowers/app-ui-2026-09-15/intrinsic-card-diagnosis.md`. This is a source-backed hypothesis, not verified implementation. Card, ContentCard and CardGrid explicitly set min-height zero; enclosing intrinsic Column uses automatic Grid rows, which can allocate less than content in a definite short viewport. Current Store narrow capture has Discover more overlapping Files despite individual card containment. Do not compensate with app pixel heights.

- [ ] First reproduce RED with actual measured layout in a short definite Scrollable: intrinsic Column containing at least eight standalone ContentCards and trailing action; repeat legacy Card containing six form rows. Assert each surface contains descendants, following siblings clear previous content plus requested gap, and content exceeds fixed viewport. Test geometry before paint.
- [ ] Add nested section regression: Ready heading, content-sized CardGrid of six cards, spacer, Discover heading, second grid. Cover one-column narrow and nested multi-column wide paths, each grid allocated bottom beyond last card bottom, following section below preceding content. Small fixture under MaxContent does not establish this contract.
- [ ] First candidate restore automatic minimum height on Card, ContentCard and CardGrid while retaining min-width zero for wrapping. Keep internal min-content/flexible tracks, fixed row lengths, default Column fill/flex contracts, surface caps, padding and gap behavior unless a failing test proves another change necessary. If candidate fails, inspect first broken node's min-size, allocation and content contribution rather than layering speculative styles.
- [ ] Verify authored `CardGrid::with_card_height(180)` stays exact with oversized prose, existing fixed buttons remain exact, legacy fixed grid rows remain unchanged. Do not replace automatic minima with unconditional measured pixel minimums. Long automatic content must scroll; viewport/header do not grow.
- [ ] Verify 1×/2× and wide→narrow→wide produce same geometry as fresh layout, bottom action remains reachable by scrolling, pointer and keyboard invokes it exactly once. Run relevant Card, CardGrid, ContentCard, intrinsic Column and scroll regressions; broad UI suite once after focused passes.
- [ ] Build Store/Settings with corrected shared source and capture narrow/short/wide and scaled routes using isolated profiles. In full catalogue, Discover starts below all installed cards; in Region, Preview starts below final control. No application workarounds or capture-path changes in this shared commit. Coordinate exact route fixtures from App Task3 report.

**Report:** `.superpowers/sdd/2026-09-15-measured-ui-components/task-5-report.md`; include RED/GREEN evidence, candidate result, fixed/manual compatibility, capture provenance and remaining limitations. Scoped commit, no subagents or pushes.


## App migration guidance (not additional framework tasks)

## 5. Normal-size composition: use existing defaults first

**Existing interfaces to reuse:**

- `Card::composed(body)` → ContentCard `with_title`, `with_header`, `with_footer` (`card_composition.rs20–64`). Grid body takes flexible remainder; footer bottoms align, header/body/footer have one card gutter.
- `Actions::new(primary).secondary(button)` (72–92): measured unspecified Buttons, wrapping, trailing alignment, secondaries before primary in focus order. Explicit sizes remain manual. This is already the correct default for small card/footer action groups; use Toolbar overflow for long command strips.
- `FormPage::new(title, form).with_description(...)` (form_composition/page.rs16–71) already owns scroll, one gutter, default720px readable width, compact s2 title/description grouping, s6 separation before form (129–153).
- `Form::section`, `Section::field/content`, `Field::with_help/with_error`, density/spacing/label overrides (form_composition.rs77–253); measured label-placement hook at593. LayoutInfo records effective choices. Do not add a duplicate page/card/form abstraction.

**Migration examples (app worker owns these):**

1. Store: replace fixed180px card and custom40px footer with intrinsic `Card::composed(description).with_title(title).with_footer(Box::new(Actions::new(open).secondary(details)))`; let grid establish equal row height, remove redundant browse headings. Keep action identities and navigation bindings. Existing `ui/src/app/card_composition_tests.rs` and card screenshot example are regression starting points.
2. Settings/account forms: FormPage title+description, Section heading once, Field label/help; preserve editor/canvas full width elsewhere. Do not apply720px cap to all apps.
3. Chat/Backups/Feeds immediate headers: first try retained Text group with zero internal padding, s2 related-row gap and s6 section gap. If two real migrations cannot reasonably retain widgets, add a narrowly scoped measured header geometry helper returning title_rect, description_rect and remaining_rect from parent bounds and resolved TextStyles. It must only compute geometry; actions remain existing widgets. Avoid speculative general layout DSL.
4. API/DB/Code Editor flexible remainder and empty dock defaults are app composition decisions. Do not globally compress every gap or redefine fixed split proportions. App worker uses existing split/manual resize route and stores user resize state.

**Verification:** compare100% wide and480px captures before2×; primary content appears sooner, action footer lies inside card, related rows read as a group. Add geometry tests for card footer containment and FormPage compact heading/separate section gaps; preserve explicit Button sizes and override provenance. Avoid tests that merely assert every magic constant.

## 6. Correct ownership for grid, merge, Flex

- DB's alleged shared DataGrid is actually `apps/db_client/src/grid.rs::GridView`. bind(result,tr) measures columns once (13px base), then caches widths; row/header geometry uses fixed constants. App worker should share measured row/header metrics across draw, row_hit, header_hit and scrolling, invalidate width metrics on renderer/font generation changes WITHOUT calling bind (bind resets sorting/scroll). Preserve sort/selection/scroll during live text-size changes.
- Merge is `apps/diffmerge/src/merge_view.rs`, with ROW_H19 at31 used for hunk height366/711, merged lines844 and paint1261. Derive one measured base-font line height and use it in layout/paint/hit tests. Narrow multi-pane collapse remains app work. Test hunk hit regions and scrolling after reflow.
- Tube Flex is app-local `apps/tube/src/widgets/flex.rs`, whose build_layout491 delegates child layout to Taffy and retains explicit gap/pad values. No confirmed Flex algorithm defect from source. First recapture with renderer-aware layout from capture worker; then test Text child intrinsic height at2 in app's grid. Change app-local Flex only if measured child heights fail to reach parent. Do not change shared Taffy defaults because a screenshot was made with scale1 measurement.

### Task 6: Single shaped source for rich inline marks

**Dependency:** Tasks2 and7 accepted before dispatch. Preserve Task7 final-empty-line normalization and its regressions when consuming shape_styled. Confirmed ordinary-size Notes defect, not a speculative typography redesign.
**Files:** `ui/src/widget/rich_text_line.rs`, scoped `ui/src/widget/text_input.rs` attributed single-line support, and narrowly `ui/src/widget/block_editor.rs` current-frame link-hit refresh plus regression (see ruling below). Only extend `ui/src/richtext.rs` or `ui/src/text_render.rs` if existing range decoration support cannot preserve behavior. Report architectural need before widening production scope.
**Evidence:** `.superpowers/app-ui-2026-09-15/rich-mark-diagnosis.md`, actual Notes images in `task2-after-20260916/captures/notes/`. Inner plain text is painted then marked substrings painted again from a separate shape with different advances.

- [ ] Feed complete mark-derived RichSpan runs plus underline/strike information into TextInput's authoritative unmasked single-line RichTextLayout, using existing shape_styled/draw_rich and rich_text_editor.rs conversion as examples. Normal TextInput remains plain by default. No new editor implementation and no opaque eraser/repaint workaround.
- [ ] Paint glyphs once. Caret, selection, horizontal scrolling, pointer-byte inversion, peer presence and link hit geometry all consume the same styled layout. Keep existing code span sans face/color/chip appearance for this repair; don't add a separate product font change. Derive decoration bounds from actual shaped ranges, with backgrounds before glyphs and rules after.
- [ ] Make attributed styles available before relevant measurement and painting, not only after plain measurement. Mark-only changes, theme/renderer scale, text edits and actual model/undo/redo adoption invalidate correct caches. Preserve complete UTF-8 spans and IME preedit behavior; any reused signature must cover all accepted style/decor inputs.
- [ ] Focused RED/GREEN: glyph-count/weights show no duplicate plain run; add/remove bold on unchanged text refreshes layout; marked end caret/selection/pixel-byte inversion agree at1×/2×; overlapping bold/link click at true edge; UTF-8/preedit adjacent to marked text; code background and underline/strike follow shaped ranges. Reuse existing tests and don't mirror implementation without an observable invariant.
- [ ] Actual Notes link route: BlockEditor currently refreshes live_link_hits before draw_block shapes current line geometry, while peer presence intentionally runs after. Move the narrow link-hit refresh to after current-frame block draw and test first draw plus style-only/scale change at the true overlapping bold/link edge. Derive hit bands from the existing authoritative selection rectangles, including reversed/mixed-direction ranges, rather than two caret endpoints; preserve the existing full-line vertical hit target. Test actual link segments and exclusion of unrelated gaps. Preserve existing BlockEditor behavior; no wider redesign. This source-backed scope ruling is necessary for the existing actual-link acceptance, not an additional app feature.
- [ ] Run affected TextInput/RichTextLine/BlockEditor module tests, relevant rich-text tests and scoped check. Fresh Notes normal/narrow/2× screenshot acceptance plus actual caret/selection/link route; root independently inspects pixels. Preserve ordinary writing density.

### Task 7: Preserve the final empty line in attributed text

**Execution order:** after Task4, before Task6. Shared2/3 accepted; this separate defect was discovered during writing-surface verification.
**Files:** `ui/src/richtext.rs`, focused regressions in `ui/src/widget/text_input.rs`; do not modify Scrollable or app source.
**Evidence:** `.superpowers/app-ui-2026-09-15/trailing-line-diagnosis.md` and its direct-rustc public API probe. cosmic-text 0.19.0 set_rich_text drops the final empty paragraph after LF, while set_text retains it. Metadata still contains the line, producing an EOF caret on row zero and wrong pointer mapping.

- [ ] Establish RED for plain versus styled `"x\n"`: equal two-line height, EOF caret on the final row, visual-line index, logical-line band and below-content hit returns EOF. Controls: `"x"`, `"x\ny"`, `"\n"`, multiple trailing LF and split attributed spans, at 1× and 2×.
- [ ] Normalize at the RichTextLayout attributed shaping boundary before shaping: final empty BufferLine, no artificial visible character or byte offset change, final effective run attrs/metrics and alignment preserved. Confirm actual cosmic-text APIs before implementation. Do not switch measurement to the broken styled height or special-case Scrollable.
- [ ] TextInput integration: measure then draw a multiline trailing-LF value; content height agrees within documented pixel ceiling, EOF caret remains at final row, clicking that row maps to `text.len()`, and caret-follow scroll stays at the end. Preserve attributed styling, current-line/gutter geometry and default/manual dimensions.
- [ ] Run focused richtext/TextInput/CodeEditor regressions and Scrollable integration suite, scoped check and format. Record exact RED/GREEN; no broad suite/dependency upgrade. Report any final-empty-line style ambiguity before expanding scope. Selection LF-only decoration redesign is excluded.

**Report:** `.superpowers/sdd/2026-09-15-measured-ui-components/task-7-report.md`. No subagents, no push. Commit only exact owned paths using `git commit --only` and verify the resulting file list.
