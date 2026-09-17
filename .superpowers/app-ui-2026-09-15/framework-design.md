# App UI framework implementation plan

> **For agentic workers:** Use executing-plans to implement these independently reviewable tasks sequentially. Do not delegate or commit without the controller's authorization.

**Goal:** Measured, pleasant normal-size composition with consistent accessible text and reachable commands.

**Architecture:** Preserve existing renderer-relative APIs and manual geometry. Add a small public normalization boundary, repair confirmed component defects, and migrate apps onto existing measured composition before introducing new layout abstractions.

**Tech stack:** Rust, Taffy, cosmic-text, Vello; existing Widget measurement/overlay/focus contracts.

**Spec:** `docs/superpowers/specs/2026-09-15-app-ui-balance.md`; baseline review reports in `docs/superpowers/audits/2026-09-15-all-app-ui-review/`.

## Constraints and evidence limits

No pushes, merges, publishing, upgrades, global padding changes, or reinterpretation of explicit fixed sizes. Preserve working changes and untracked Scratchpad source. This report is source inspection, not executed verification. Capture fidelity is a separate preceding task. Review findings identify symptoms, not always correct ownership: there is no shared DataGrid or shared Flex implementation for the cited DB/Tube findings.

## 1. One text-size contract (shared prerequisite)

**Files:** `ui/src/text_render.rs`, `ui/src/widget/text.rs`, focused module tests.

Verified current source: `TextRenderer::scaled_style` (1367) multiplies size and letter spacing; `measure_style` (1470) and `draw_style` (1833) use renderer scaling. `Theme::with_text_scale` already changes typography tokens (`theme.rs` around 736). `Text::renderer_input_style` (375) privately divides resolved size by renderer scale. No `FontSize` enum found in current non-vendor Rust; recheck before editing because other work may add one.

- [ ] Add `pub fn renderer_input_style(&self, resolved: &TextStyle) -> TextStyle` on TextRenderer, returning a clone with `size` and `letter_spacing` divided by sanitized `self.text_scale()`. Document that input is final logical pixels; theme typography is already resolved. Leave `measure`, `measure_style`, `draw_text`, `draw_style` semantics intact. If a concurrent explicit FontSize API exists, consume it instead of adding this method.
- [ ] For immediate drawing use one normalized style for both calls: `let style = tr.renderer_input_style(&TextStyle::new(theme.tokens.typography.scale.sm)); let (w, h) = tr.measure_style(label, Some(width), &style); tr.draw_style(scene, label, color, transform, Some(width), &style);` Do not mutate global renderer scale around a draw.
- [ ] Keep Text's explicit/base/fixed semantics and cache invalidation. Reuse the public normalization for fully resolved styles only; examine letter-spacing semantics before replacing the existing private size-only adjustment wholesale.
- [ ] Tests: legacy14px at renderer2 measures as28px; resolved28px measures identically at renderer1 and2; resolved letter spacing also stays identical; theme ui1.25/text2 scales once; same resolved style produces identical measurement and shaped glyph bounds. Existing `text::tests::theme_resolved_sizes_are_independent_of_renderer_scale` is a useful regression anchor.

**Gate:** `cargo test -p ui --lib text_scale`; `cargo test -p ui --lib theme_resolved_sizes`. Add/run the new focused test filter. Do not broad-rewrite callers whose literals are intentionally base sizes.

## 2. EmptyState wrapping and recovery target

**Files:** `ui/src/widget/empty_state.rs`; tests in that module.

`paint_empty_state` (137 onward) takes already-scaled title/message tokens through relative APIs, measures all text with no width, uses34px action height, and centers potentially wider-than-pane content. `Fit::choose` removes icon then message but can still produce negative top when title+action exceed height. VPN review-3 documents the visible4× symptom; both retained and immediate entry points already share this function.

- [ ] Normalize title, message and button styles once using task1; measure with inner width `max(rect.width()-2*s4, 1)` and draw with the same width. Center the measured wrapped block, not an unbounded single line.
- [ ] Compute action height from measured label line height plus vertical padding, min existing34px; wrap long labels within inner width. Preserve returned action rectangle as the sole paint/hit/focus geometry.
- [ ] Retain icon shedding. Prefer keeping explanatory message when it fits after wrapping. If essential content exceeds available height, anchor at pane top and expose a scrolling retained route rather than silently deleting the action. Immediate paint must document its minimum essential height; app parent supplies scroll for physically insufficient panes. Do not shrink fonts or return an invisible action hit target.
- [ ] Tests: width240/480, scales1/2, long localized title/message/action; glyph and action bounds fit width, action grows, painted action equals pointer target; Tab/Enter existing action route remains functional; tiny heights shed decoration first and do not place hit geometry above pane.

## 3. Rich editor scale and Scratchpad fill are distinct repairs

**Files:** `ui/src/widget/rich_text_line.rs`, `ui/src/widget/text_input.rs`, `ui/src/widget/block_editor.rs`; `ui/src/widget/scrollable.rs` only for an opt-in fill-content behavior.

Confirmed double scaling: RichTextLine::build_layout at442 calls `inner.set_font_size(self.font_size * theme.text_scale)`, but TextInput::ensure_shaped_layout at1581 again computes `size * theme.text_scale`. Its comment claiming the first multiplier is needed is misleading. BlockEditor constructs RichTextLine from base `BlockKind::font_size`; list-prefix painting uses base fonts, so do not globally divide BlockEditor fonts.

- [ ] Set RichTextLine inner override to base `self.font_size`. Keep row height resolved once. Test actual shaped run font size, cursor position and selection bounds after layout+draw, not row-height-only assertions. Heading, paragraph, inline code and numbered prefix must align at1 and2. Cover scale changes on the same editor instance, undo/redo and pointer caret placement.
- [ ] Audit TextInput multiline measurement: `measure` (3546) uses build_lines/content_height while draw uses shaped layout. Establish a single height basis for measured rows and the shaped draw path; add long wrapping line tests where content heights and caret line indexes agree at2.
- [ ] Scratchpad's38px editor is **not** inherently a wrong multiline minimum: TextInput::build_layout uses auto height and measures content; Scrollable::fill_parent_height grows viewport only (scrollable.rs972/995). Empty short fields should stay compact in forms. Add opt-in `Scrollable::with_content_min_viewport_height()` (or equivalent existing API if discovered) setting the direct child's minimum height to available viewport during measured layout, while retaining intrinsic height for overflowing content. Default false. The Widget prepaint hook explicitly permits changing only its own node style: do not mutate the child node through that hook. Implement the minimum via an owned wrapper with its own hook/inherited viewport decision, or a bounded layout-protocol extension reviewed separately. This is preferable to globally expanding multiline inputs.
- [ ] Prove empty editor fills viewport, long editor exceeds viewport and scrolls, short form multiline remains content-sized, footer stays visible, clicking lower empty area focuses/positions caret. Then one-line Scratchpad opt-in on its existing Scrollable chain (`apps/scratchpad/src/lib.rs:116–124`) preserves original source and all action bindings. Coordinate that app edit with controller.

## 4. Toolbar measurement AND reachable overflow

**Files:** `ui/src/widget/toolbar.rs`; reuse `ui/src/widget/context_menu.rs` and Widget overlay/focus hooks (`ui/src/widget/mod.rs`). Do not confuse widget Toolbar with `ui/src/chrome/toolbar.rs`.

Three separate approximate sizing paths currently disagree: `measure_item_axis`147, `decide_visible_count`214 and `measure_visible_axes`252. apply_layout loses theme and falls back to character count and16px labels. draw uses actual renderer measurement. `is_overflow_open`202 explicitly says host paints the popover; the widget supplies no reachable hidden command route, and its accessibility node is only Role::Toolbar.

- [ ] Cache theme font/padding during build_layout, then actual label width/height during `adjust_layout_with_measure` (Widget hook, mod.rs602; Form/Field demonstrate usage). Use one cached item extent vector for visibility, placement and intrinsic sizing. Reserve measured overflow control before fitting commands. Invalidate on theme/scale/label changes. Retain public recompute_visible compatibility, but avoid using its approximate result as authoritative final layout.
- [ ] Implement overflow using existing ContextMenu selection drain: menu maps each visible menu entry back to original ToolbarItem index; use `take_selection()` to call the original FnMut, preserving command ownership. Include only hidden buttons and meaningful separators. Widget draws it through draw_overlay and routes popup input before underlying buttons. Reuse dismissal/positioning infrastructure; do not create another floating-window system.
- [ ] Add focusable command/overflow semantics, keyboard opening, arrow navigation, Enter activation, Escape close and focus return. Popup must remain within host bounds and scroll if taller than viewport. A toggled boolean is not acceptance.
- [ ] Tests with `WWW` versus `iii`, non-Latin text, long translations, both orientations, widths less than chip width, scale changes and resize after opening. Every original callback reachable exactly once through pointer and keyboard; invisible item rectangles zero; separator-only overflow prohibited; accessibility exposes commands.

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

## Order and review boundaries

1. Capture fidelity (other worker), then text normalization + focused regressions.
2. EmptyState and editor fixes independently reviewable; Scratchpad fill opt-in separately reviewable because it changes layout semantics.
3. Toolbar is one measured sizing + interaction deliverable; don't land a visual-only overflow fix.
4. Existing composition migrations demonstrate normal-size value; only then approve any tiny immediate header helper.
5. App-owned grid/merge/Tube fixes consume the agreed text contract.

For each completed boundary: focused tests, fresh affected wide1200×800/compact800×700/narrow480×900/true200%800×900 captures, actual image inspection and pointer/keyboard checks. Record closure separately from immutable baseline reports. No finding is closed by this plan or by an exit-code-only capture.
