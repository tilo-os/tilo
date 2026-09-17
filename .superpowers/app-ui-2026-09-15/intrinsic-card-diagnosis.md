# Intrinsic Card minimum-height diagnosis

Date: 2026-09-15. Read-only production investigation; no builds, tests, production edits, or commits performed by this investigator. Line anchors describe the current shared checkout and may move as the sole production worker continues.

## Conclusion

The strongest source-backed hypothesis is a defective **automatic block-axis minimum contribution** on Card, ContentCard, and CardGrid. Each explicitly sets `min_size.height = 0`, even when its height is automatic. An intrinsic Column uses Grid with implicit automatic rows. Under a definite short viewport, those rows can accept a zero minimum, allocate less height than the card/section needs, and place the next sibling over descendants that retain their natural content height.

This is a shared standalone/nested composition contract defect, not a reason for each app to calculate card heights. `flex_shrink: 0` does not protect a Grid item from Grid track sizing. A CardGrid workaround fixes its own card rows but still exposes a zero minimum to an enclosing intrinsic Column, explaining the latest section-level overlap.

This investigation establishes the code path and a high-confidence hypothesis, not a test-verified fix. A failing short-viewport fixture must precede the implementation.

## Evidence and source anchors

- `ui/src/widget/column.rs:244`: intrinsic Columns use `Display::Grid`, retain automatic implicit row sizing, and pack at start. Default Columns remain flex/fill-height. There is no explicit content-minimum row policy on the intrinsic path.
- `ui/src/widget/card_composition.rs:119`: ContentCard sets both minimum dimensions to zero; `:103` gives its internal header/footer min-content rows and body `minmax(min-content, 1fr)`. Those internal row rules do not restore its minimum contribution to its parent.
- `ui/src/widget/card.rs:176`: legacy Card also sets minimum height zero; `:154` caps the inner surface at 100% of a definite allocated row. Thus an underallocated outer row can visibly cap the surface while descendants overflow.
- `ui/src/widget/card.rs:415`: CardGrid likewise sets minimum height zero. Its `content_sized` mode uses min-content rows (`:423`), while fixed mode uses authored row lengths. This protects rows *inside* the grid, but not the grid's own allocation in an automatic outer row.
- `ui/src/widget/card.rs:472`: CardGrid's prepaint hook resolves columns at allocated width. This is a real difference from standalone Card, but is not sufficient to explain the latest whole-section overlap. Zero block-axis minima are the stronger common cause.
- `ui/src/widget/layout.rs:96`: the shared solver measures and runs widget adjustment hooks until stable, before applying geometry. `ui/src/widget/mod.rs:574` and `:595` restrict hooks to self-node styles and prohibit topology mutation. No hook currently repairs Card/CardGrid minimum height.
- `ui/src/widget/scrollable.rs:983`: vertical Scrollable avoids stretching its child and retains a definite viewport; `:1048` walks descendant bottom edges for scroll extent. That defensive extent calculation cannot repair sibling placement already computed from an undersized section.
- `ui/src/widget/container.rs:223`: ordinary Container already defaults to automatic height and has no blanket zero minimum height. Do not change all Containers speculatively.

The locally installed dependency corroborates the mechanism:

- `/Users/xaver/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/taffy-0.10.1/src/compute/grid/track_sizing.rs:572`: min-content row minima take measured min-content contribution. `:590` automatic row minima under definite available space take `minimum_contribution`; indefinite MinContent/MaxContent requests take the stronger content contribution.
- The same file `:1141`: track maximization distributes available free space up to growth limits. A zero lower bound allows tracks to stop below natural content size in a short definite space.
- `.../compute/grid/types/grid_item.rs:459`: minimum contribution resolves preferred/minimum style sizes first; explicit zero prevents the automatic content-based fallback. `:533` clamps contributions to a fixed spanned track limit.

## Observed reproduction, coordinated with app worker

### Store standalone cards

`.superpowers/app-ui-2026-09-15/task3-after-20260915/captures/store/narrow.png` is 480×900. Topology: filling Container → fill-remaining-height Column → header + fill-parent-height vertical Scrollable → intrinsic Column (no padding, gap 8) → ContentCards. A card contains header Row (artwork and identity), Text body, and Actions footer. The worker reports approximately 71px painted card heights with descriptions/actions extending beyond them. Wide content-sized CardGrid worked. A three-item test under MaxContent or a generous definite 900px height did **not** reproduce the full-catalogue failure.

### Store grid-section propagation

After switching each availability group to one-column `CardGrid::content_sized`, `.superpowers/app-ui-2026-09-15/task3-final-20260915/captures/store/narrow.png` still places “Discover more” around y588 across Files, hiding VLC (controller observation). Current topology in `apps/store/src/app.rs:2544`, `:2624`, `:2647` is an intrinsic outer Column containing heading, content-sized grid, fixed spacer, next heading, next grid. The grid's zero minimum height explains why individual cards improve while the next section begins prematurely. Wide nested section Columns are composed at `:2660` and require coverage too.

### Settings legacy Card

`apps/settings/src/widgets.rs:48` stacks Cards in an intrinsic gap-zero Column with explicit 20px spacers. `:227` similarly stacks row controls with explicit 12px spacers. The Region Formats Card has six dropdown rows. The worker's 1200×760 regression placed the final first-day field at y434..450 and the following Preview heading at y436..455. A one-column content-sized CardGrid made that focused test green, but the Store result shows this workaround is not a complete shared fix.

## Minimal shared fix task

1. Add failing regressions for the standalone-card and nested-grid-section paths below; record actual Taffy allocations and semantic widget rectangles before changing production code.
2. First candidate: restore the **automatic minimum height** for Card, ContentCard, and CardGrid, retaining zero minimum width for narrow wrapping. Do not author pixel heights or grow measured rectangles after layout. Limit CardGrid behavior changes as needed to preserve explicitly authored fixed-row behavior.
3. Keep existing internal min-content/body-flex tracks, fixed row lengths, legacy surface cap, normal Column fill/flex defaults, padding, and gap semantics unless a failing regression proves a separate change necessary.
4. Confirm automatic minimum content propagates through each enclosing intrinsic Column to the scroll content root. If this candidate does not settle the full fixture, collect per-node size/min-size, allocated height, content height and child bottom at the first broken boundary before trying another policy.
5. Use existing prepaint settlement only if a width-dependent measurement problem remains after the minimum-height correction. Do not add app-level height formulas or mutate descendant styles from parent hooks.

### Automatic versus explicit sizing

For automatic height in an intrinsic stack, each Card and section should contribute enough natural height to keep its content and following sibling separated; excess height belongs to Scrollable content, while the viewport stays fixed. Fixed author dimensions are a separate contract: `CardGrid::with_card_height(180)` must remain exactly 180 even with excessive prose. Taffy's fixed-track contribution clamp suggests an automatic item minimum can coexist with that requirement, but tests must establish this. Do not replace automatic minima with unconditional measured `min-height` lengths, which could override manual constraints.

## Regression and verification plan

### Shared fixture A — direct standalone Cards

Use FrameHarness at 480×360 (and 480×900 with enough items), with a filling Container → no-padding fill-remaining-height Column → fixed-height header → vertical Scrollable.fill_parent_height → intrinsic no-padding Column.gap(8). Add at least eight ContentCards; each has a fixed 40px artwork/header Row, identity text, a wrapping multi-line description, and Details/Open Actions. Add a trailing heading and final actionable control. Repeat with legacy Card::titled containing an intrinsic Column of six fixed-height control rows separated by 12px gaps. Use distinct semantic keys.

Assert each automatic card surface contains all owned regions, every next card/heading starts after the preceding card's last descendant plus requested gap, and the content root exceeds the viewport. Assert the viewport/header sizes remain unchanged. Scroll to bottom and click/keyboard-activate the final control exactly once. Inspect layout geometry before paint; do not pass merely because Scrollable's descendant walk makes the last control technically reachable.

### Shared fixture B — nested CardGrid sections

Under the same short Scrollable/outer intrinsic Column, add “Ready” heading → single-column content-sized CardGrid with six cards → fixed 16px spacer → “Discover” heading → second content-sized CardGrid with four cards. Mirror the wide branch with an extra intrinsic section Column containing title/caption/grid. Compare each grid's allocated height with its last card bottom; assert following headings start below all preceding descendants. Test single-column and multi-column widths. This specifically detects the remaining Store section overlap.

### Compatibility and resize

- Repeat 1× and 2× text scales and wide → narrow → wide; fresh and resized geometry must match, including section/card bounds and scroll ownership.
- Retain `ui/src/widget/card_composition.rs:416` exact 180px fixed-row regression and fixed manual button-size tests at `:384`.
- Retain legacy Card fixed grid row-height coverage in `ui/src/widget/card.rs:753`, and add an oversized legacy Card body under a fixed row if not covered elsewhere. Retain `card_keeps_intrinsic_height_in_a_short_flex_parent` (`:856`) and add the corresponding intrinsic-grid parent case because that existing coverage only tests flex.
- Run the existing Column intrinsic gap/padding/explicit-stretch/scroll tests and the default fill-height Column test. The fix should not require changing those expectations.
- Re-run app Store full catalogue grouping/section-order/last-action tests and Settings Region final-field-to-Preview test, then capture 480×900, 1200×760/800 and elevated-scale routes. Do not accept only a small three-item catalogue fixture.

No verification commands were run here because the active production worker owns the build/test slot.
