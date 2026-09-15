# Software Store: layout and framework review

Reviewed the Store implementation and newly rendered screenshots on 2026-09-15. Focus: pleasant layout with minimal developer effort, while retaining manual control.

## Evidence and scope

Captured the existing deterministic fixture scenarios at 1200×760, 920×720, and 390×844, plus detail and the Store's requested 200% text mode. These are fixture-backed renders of Store widgets, not a live remote catalogue. Artwork uses the Store's fallback rendering.

The current workspace build failed in unrelated, unfinished `runtime-fs` code: `Arc<OperationOwner>` has no `reconcile` method at `native.rs:1138`. Captures were built in a temporary source snapshot with the committed filesystem dependency and the needed current macOS configuration. All Store and framework layout files cited below were verified byte-for-byte against the workspace. No application/framework source was changed in the workspace.

Screenshot and probe directory:

`/Users/xaver/.codex/visualizations/2026/09/15/01a0a50b-c5e5-7e63-bc71-bf4e2b8ac8cd/store-review/`

- `wide.png`: fresh 1200×760 browse view.
- `medium.png`: fresh 920×720 browse view.
- `narrow.png`: fresh 390×844 browse view.
- `detail.png`: fresh 920×720 detail view.
- `large-text.png`: existing screenshot path with `QOS_SHOTS_TEXT_SCALE=2`.
- `resized-920.png`: widget tree built at 1200, then laid out at 920 without an unrequested structural rebuild; reproduces the resize contract failure.
- `before-draw.json`, `after-draw.json`, `resized-920.json`: widget geometry/semantics snapshots from the probe.

## Findings

### 1. [P1] Responsive composition does not invalidate across all of its breakpoints

**Observed:** shrinking an existing 1200-pixel view to 920 keeps three columns, clips the third column, and cuts off header actions. A freshly constructed 920-pixel view instead uses two columns and a stacked header.

**Cause:** `StoreApp::apply_layout` only compares `compact_layout()` before/after resize, which checks 620 pixels. Composition also depends on 980 pixels for the header, 1180 pixels for centering/grouping, and 900 pixels for detail-page width. Those changes do not request rebuilding the composition. The framework resize event requests layout, not a structural rebuild.

**Proof:** the probe reports `RESIZE 1200->920 structure_dirty=false`. The resized grid spans x=12 to x=1132 in a 920-pixel viewport.

**Sources:** `apps/store/src/app.rs:995`, `:1280`, `:1367`, `:2209`, `:3916`; `ui/src/app.rs:9051`.

**Framework implication:** adaptive composition needs a shared dependency/invalidation mechanism. Authors should not maintain a separate list of breakpoints whose changes rebuild the tree. Explicit manual responsive policies need this mechanism too.

### 2. [P2] Content-sized columns silently discard their requested gaps

**Observed:** narrow cards touch one another, detail surfaces run together, and labels/descriptions/actions become cramped despite `.with_gap(...)` calls.

**Cause:** `Column::with_intrinsic_height()` selects Taffy block layout, while `with_gap` still writes the Flexbox-style gap property. Block layout does not apply that gap. `Card::vgap` documents this and works around it with empty fixed-height containers.

**Proof:** a two-child intrinsic Column with a requested gap of 20 has an actual gap of 0. The Store uses this exact combination in card identity, narrow cards, headers, and detail sections.

**Sources:** `ui/src/widget/column.rs:151`, `:225`; `ui/src/widget/card.rs:133`; `apps/store/src/widgets/card.rs:60`, `:120`; `apps/store/src/app.rs:282`, `:1408`.

**Framework implication:** a public spacing property must retain its meaning across sizing modes. Content-sized vertical layout should work with real gaps. This is a primitive-contract defect, not a missing design guideline.

### 3. [P2] Fixed-height grids do not guarantee balanced card contents

**Observed:** on the wide screen, Terminal's action buttons sit nine pixels lower than Files' and VLC's. Bottom padding is three pixels in the first card versus twelve in the others. Card descriptions and actions compete inside a fixed-height box.

**Cause:** Store chooses `with_card_height(180.0)` and manually assembles identity, description, spacer, and actions. `CardGrid::measure` computes its height from the fixed row height and card count, without measuring child content. There is no shared card-body/footer arrangement with content-aware row height. The Store switches the whole catalogue to a list above text scale 1.25 as a workaround.

**Proof:** the first row ends at y=514; Terminal buttons end at y=511, while Files/VLC buttons end at y=502. All cards start at y=334 and have the same declared 180-pixel height.

**Sources:** `ui/src/widget/card.rs:267`, `:331`, `:439`; `apps/store/src/widgets/card.rs:156`; `apps/store/src/app.rs:1014`, `:2680`.

**Framework implication:** add an automatic card composition with measured body height and aligned footer actions. Preserve explicitly fixed card heights for manual authors. Avoid making a caller guess a height that happens to fit the current descriptions.

### 4. [P2] Artwork overflows its allocated layout box

**Observed:** Files' fallback icon intrudes into its name; VLC and Notes artwork extend outside their tiles. The large Notes icon crowds the Back control in the detail screenshot.

**Cause:** `Artwork::draw` calculates a positive inset and calls `self.rect.inset(inset)`. Kurbo defines positive insets as expansion. A 52-pixel tile therefore produces a 74.88-pixel drawing region, instead of a smaller inner region. The fallback drawing is not clipped to the tile.

**Sources:** `apps/store/src/widgets/artwork.rs:150`, especially `:166`; Kurbo's local `Rect::inset` documentation confirms the expansion behavior.

**Ownership:** the sign error is Store-specific. The framework opportunity is a standard artwork component or explicit inner-padding helper with optional clipping, so manually drawn content can fit reliably within its measured region. Do not universally clip every widget: shadows/popovers need declared overflow.

### 5. [P2] Screenshot text scaling does not exercise real large text

**Observed:** requesting 200% text changes the catalogue from grid to list, but text remains normal-sized.

**Cause:** Store's screenshot driver sets `Theme::text_scale` but leaves `TextRenderer` at its default scale of 1. Scaled Text prepares a base-size style for the renderer, expecting the renderer to apply the multiplier. The two settings are inconsistent.

**Proof:** with theme scale 2, the same 14-pixel Text emits a 14-pixel glyph run when renderer scale is 1, and a 28-pixel run when renderer scale is 2.

**Sources:** `apps/store/src/shots.rs:275`, `:295`; `ui/src/widget/text.rs:340`; `ui/src/text_render.rs:1347`.

**Framework implication:** screenshot/preview rendering should use the same configured layout/render context as the live application. Maintaining an app-local copy of the pipeline makes visual acceptance misleading. This finding concerns the screenshot path; it does not establish that the live application's text scaling is broken.

### 6. [P1] CardGrid's private layout tree loses standard keyboard traversal

**Evidence:** the wide fixture contains 19 focusable widget objects. Its host node map registers only 7, excluding the 12 card buttons. Before drawing, those 12 controls have zero-sized rectangles. The real `App::init_layout` focus traversal produces only 6 entries for this fixture.

**Cause:** CardGrid appears as a leaf in the host Taffy tree. It builds and measures card descendants in an isolated tree inside `draw`. App focus traversal zips widget children with host Taffy children, so it cannot descend into those cards. Painting supplies geometry later, but does not register them in the host focus order.

**Sources:** `ui/src/widget/card.rs:376`, `:421`, `:472`; `ui/src/app.rs:4982`.

**Framework implication:** custom/manual layout must integrate its children with the host lifecycle. Standard buttons must retain standard focus behavior when placed in a grid. Final geometry should exist after layout, rather than being created as a side effect of paint. This is separate from the aesthetic findings but matters to a first-class manual path.

## Composition findings and developer effort

The fresh screenshots also show problems that are primarily Store composition choices:

- At 920 pixels, the catalogue count is repeated above and beside the filters.
- Header content and result content use different left edges: x=28 versus x=12 at medium width, and x=56 versus x=36 at wide width.
- The first card starts at y=382 on a 720-pixel-tall medium view. Much of the first screen is masthead, utility controls, summaries, and spacing rather than apps.
- In the detail view, moderation/reporting gets a prominent filled action beside the main app action elsewhere on the page. That is an information-hierarchy choice, not a Taffy defect.

The framework does not need to dictate Store's copy or forbid custom headers. It should offer a pleasant page composition with shared gutters, header/search/filter regions, section rhythm, and a compact utility-action area. Today the Store assembles these with repeated `with_no_padding`, intrinsic-height switches, spacer widgets, width constants, text sizes, and breakpoint branches. The comments throughout the source document repeated repairs for overflow and unexpected container behavior.

## Recommended order of framework work

1. Repair primitive contracts: intrinsic column spacing and coherent child geometry/focus registration.
2. Provide reliable adaptive composition invalidation.
3. Add content-aware cards with footer alignment, preserving manual sizing.
4. Add an automatic page layout with shared gutters and coordinated regions.
5. Unify preview/screenshot and live rendering configuration; test both fresh layout and resize of an existing tree.

The Store is strong evidence that polished widgets alone are insufficient. QoS needs predictable composition primitives and useful automatic layouts. More styling restrictions would not resolve the measured defects above.
