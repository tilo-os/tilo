# A7 — Office editors: UI alignment and spacing review

Unit A7 of the 2026-09-16 all-OS alignment review: **word-processor**,
**sheets**, **slides**. Branch `ui-review/a7-office-editors`, based on
`codex/macos-shell` at `087d5a62c` (see *Limits* for the base correction).
Delivery is local only.

The document canvas of each editor (page, cells, slide) is authored geometry
and was not reviewed; the chrome around it was: toolbars, formula bar, tab
strip, status pill, settings panel, menus, rail, inspector, layout bar, ruler.

## Method

1. Read `CLAUDE.md`, `ui/HIG.md` §1/§7/§7a/§8, `ui/MANUAL_LAYOUT.md`,
   `ui/FORM_COMPOSITION.md`, `docs/hand-rolled-ui-audit.md`, the prior
   `2026-09-15-all-app-ui-review` (review-3, corrected-review-2, closure
   register), the `2026-09-16-template-menu-ux` audit, and plan tasks
   `app-composition` 9 and `app-text-geometry` 1–2.
2. Source pass over each app's chrome layout for private spacing literals,
   fudge offsets, sibling gap mismatches, text centred on font size, estimated
   label widths, and hit-rect versus painted-rect drift.
3. BEFORE captures for every app: `--responsive-matrix` (dark and light),
   2× `--text-scale 2` at 800×900, 480×900, 800×700, and every listed
   `--scenario` / route at 1200×800; every PNG opened and inspected.
4. Fixes driven by one measured metric per chrome region (renderer-resolved
   line boxes and label widths plus `theme.tokens.spacing`), with 1× floors
   that reproduce the classic density exactly; hit rects come from the same
   geometry that is painted.
5. Focused regressions per fix; AFTER captures; comparison.

Capture directories (worktree-relative):

- `target/ui-review/sheets/{before,after}/` — `dark/`, `light/` matrices,
  `large-text.png`, `narrow.png`, `compact.png`, `<scenario>.png`
- `target/ui-review/slides/{before,after}/` — as above plus
  `viewport-narrow.png` (480×900), `scn-<scenario>.png` (native narrow
  viewports), `scn-inspector-2x.png`, `scn-many_rail-2x.png`,
  `pseudo-locale.png`
- `target/ui-review/word-processor/before/` — matrices, `large-text.png`,
  `narrow.png`, `compact.png`, `history.png`, `styles.png`, `comments.png`,
  `threaded-comments.png`, `find.png`, `error.png`, `image-object.png`
  (no AFTER set: no word-processor code changed — see below)

## Sheets

**Scope.** `apps/sheets/src/app.rs` (menu bar, formula bar, sheet tabs,
status pill, dropdowns, tab menu, document-settings panel, chart/comment
cards, loading veil) and `apps/sheets/src/grid.rs` (column/row headers,
default row height, cell text, in-cell editor).

**Root cause shared by most findings.** Theme typography tokens are already
accessibility-scaled and the shot harness / live host also sets the
renderer's text scale, so every hand-drawn chrome string that passed
`theme.tokens.typography.scale.*` straight to `TextRenderer::measure` /
`draw_text` rendered at 4× under a 2× theme while every band kept its 1×
height (the corrected-review-2 P1). Fixed by resolving a `ChromeMetrics` /
`GridMetrics` once per frame from the renderer that paints
(`renderer_input_style`), and deriving every band, control and label slot
from the measured line boxes plus spacing tokens.

| id | surface | defect | viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| S1 | formula bar `fx` | glyph placed at `x0 + NAME_BOX_W + 8`, `y0 + 8` — not in its own `fx_rect`; at 2× inside the name box | 800×900 2× dark | `app.rs:3826-3827` | `sheets/before/large-text.png` → `sheets/after/large-text.png` | fixed — centred in `fx_rect`, slot = measured glyph + 2·s2 |
| S2 | all chrome and grid text | token sizes fed to a scaled renderer → 4× glyphs, unreadable sheet | 800×900 2× | `app.rs` 49 sites, `grid.rs` 19 sites | same PNGs | fixed — sizes converted once via `renderer_input_style` |
| S3 | menu bar / formula bar / tab strip / headers / default rows | fixed 44 / 34 / 34 / 28 / 26 px while text scales | 800×900 2× | `app.rs:50-53`, `grid.rs:26-29` | same PNGs | fixed — `menubar_h()`, `formula_h()`, `tabs_h()`, `GridMetrics::{col_header_h,default_row_h,row_header_w}` with 1× floors; manual row/column sizes byte-for-byte |
| S4 | status / summary pill | `text_size = 12.0` literal (never scales); bottom `tabs_rect.y0 - 8` lands inside the grid's horizontal scrollbar rail | every viewport, both themes (e.g. 1366×768 light, 390×844) | `app.rs:4081-4086` | `sheets/before/light/dev.qos.sheets-laptop.png`, `before/summary.png` → `after/summary.png`, `after/dark/dev.qos.sheets-phone.png` | fixed — `sm` renderer-input size, `s3` margin, bottom = rail top − s2 (`status_bubble_bottom()`) |
| S5 | menu bar buttons | per-label width table (56/70/66/104), gaps 6, inset 10 | all | `app.rs:3269-3305` | `before/dark/…-desktop.png` → `after/dark/…-desktop.png` | fixed — measured label + 2·s3, one `s2` gap, one table (`MENU_BAR_BUTTONS`) for layout, paint and measurement |
| S6 | name box, formula content, chips, tabs, menu rows, dropdown rows | text centred on font size (`(h − size)/2`) | all | `app.rs:3742, 3796, 3614, 4026, 4530, 4773` | as above | fixed — `ChromeMetrics::centred_top(rect, line_h)` |
| S7 | document-settings panel | section labels drawn at `row_y + 4` (3–4 px above chip centre); chip widths `chars × 8 + 24`; 6 px chip gap, 18 px section gap, fixed 268 px height | 1200×800 dark | `app.rs:3482-3577, 4604` | `before/docsettings.png` → `after/docsettings.png` | fixed — labels centred on their chip row, measured chips, `s2` / `s4` rhythm, content-sized panel |
| S8 | sheet-tab menu | "Colour" caption at `first.y0 − 16` literal; swatch gap 6; width 188 literal | 1200×800 | `app.rs:3427-3460, 4797` | `before/tabmenu.png` → `after/tabmenu.png` | fixed — caption block = `lh_xs + s1`, swatch gap `s1`, width from measured rows (floor 188) |
| S9 | chart / comment card actions | labels drawn at `button.center().y + 4` (≈8 px low in a 44 px band) | 1200×800 | `app.rs:4242, 4438` | `before/charts.png` → `after/charts.png` | fixed — centred on measured `xs` line box; 44 px touch bands kept |
| S10 | add-sheet "+" | glyph nudged `center.x − 4`, `y0 + 4` | all | `app.rs:4057-4058` | desktop PNGs | fixed — centred on measured glyph box; button is a square of the tab height |
| S11 | name-box `TextInput`, tab-rename `EditableLabel` | fed the theme-scaled `sm` (double scale in the shared editor) | 2× | `app.rs:3747, 4866` | — | fixed — renderer-input size |
| S12 | grid headers / cells / editor | header & cell text centred on font size; `COL_HEADER_H` / `ROW_HEADER_W` / `DEFAULT_ROW_H` constants | 2× | `grid.rs:2270, 2356, 2429, 2550` | `large-text.png` | fixed — `GridMetrics` line boxes; prefix sums rebuilt on metric change |
| S13 | shared `ui::status_bubble::{pill_size,draw_pill}` | pill height and label centring derive from the renderer-input `text_size`, not the measured line box, so a 2× pill is 30 px around 30 px of text | 2× | `ui/src/status_bubble.rs:118-160` | `sheets/after/large-text.png` | open — needs F2; sheets compensates for the height only (measured `sm` line box + 2·s2, shared floor kept) |
| S14 | columns at 2× | default column width does not follow text (autofit widens only for content) | 800×900 2× | `grid.rs:28` | `after/large-text.png` (E clipped by viewport) | open — content geometry (plan task 1 leaves manual/default widths authored); documented, not a chrome defect |
| S15 | sheet tabs (found in code review) | measured tab width had no floor/ceiling, so a long sheet name could push "+" and later tabs off the strip; names were never elided | any width, long name | `app.rs` `tab_w` | — (regression `sheet_tabs_keep_their_floor_and_ceiling_at_every_scale`) | fixed — `TAB_MIN_W`/`TAB_MAX_W` (64/180, grown with the text) and paint-time `elide_to_width_weighted` |
| S16 | grid scroll (found in code review) | `set_metrics` rebuilt geometry without clamping `scroll_x/y`; shrinking text after a deep scroll left an empty body until the next input | scale change | `grid.rs` `set_metrics` | — | fixed — `clamp_scroll()` after the rebuild |
| S17 | inline tab rename (found in code review) | the rename editor kept the rect captured when it opened; a relayout (resize, collaborator renaming a sheet, scale change) detached it from its tab | live collab / resize | `app.rs` `begin_tab_rename` | — | fixed — `tab_rename_rect(idx)` shared by `begin_tab_rename` and `relayout`; editor room `TAB_RENAME_MIN_W` grown with the text |

Conventions observed: `MENUBAR_H`, `MENU_BUTTON_H`, `FORMULA_FIELD_H`,
`TAB_H`, `SETTING_CHIP_H`, `MENU_ROW_H` are the 1× floors; deliberately
off-scale values are named (`TAB_STRIP_BOTTOM_INSET = 2`,
`TAB_UNDERLINE_H = 3`, `MENU_ROW_SEAM = 2`, `TAB_COLOUR_SWATCH = 20`).
Spacing uses `s1`/`s2`/`s3`/`s4`/`s6`. `refresh_chrome_metrics` runs at the
top of `draw`, re-runs `relayout` in the same frame when metrics change, so
paint and the stored hit rects always agree.

## Slides

**Scope.** `apps/slides/src/app.rs` (toolbar, title block, rail header and
actions, inspector sections, drop zone, label painter) and
`apps/slides/src/toolbar_tools.rs` (insert pills and overflow menu).

**Root cause shared by most findings.** `compute_chrome()` is renderer-free
(event handlers call it too) and sized every slot with `chrome::est_width`
at literal base fonts, while painting through the scaled renderer; every row
height was a literal (20/26/30/34/56). Fixed with a `ChromeMetrics` struct
refreshed at the top of `draw` (measured line boxes and label widths in final
pixels), read by `compute_chrome`, and paint-time elision for the title.

| id | surface | defect | viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| L1 | toolbar title | title slot bounded by the nominal tools slot; the overflow "⋯" is pulled left of it when nothing fits, so the elided title paints under the button | 480×900 dark | `app.rs:1445-1451`, `toolbar_tools.rs:95` | `slides/before/viewport-narrow.png` → `slides/after/viewport-narrow.png` | fixed — title yields to the leftmost placed control (`title_yields_to_the_leftmost_placed_toolbar_control_at_480`) |
| L2 | toolbar pills, title, subtitle | `est_width` layout at 13/17/11.5 px vs scaled painted glyphs → title under "+ Slide", "Present" clipped at 2× (corrected-review-2 P1) | 800×900 2× | `app.rs:1617, 1627, 1722, 1741`, `toolbar_tools.rs:212-216` | `before/large-text.png` → `after/scn-inspector-2x.png`, `after/scn-many_rail-2x.png` | fixed — measured pill widths, `elide_to_width_weighted` at paint |
| L3 | toolbar | fixed `TOOLBAR_H = 56` while the two-line title block scales; subtitle spilled below the bar | 2× | `app.rs:248, 1419, 1715, 1740` | same | fixed — `toolbar_h()` from measured title + subtitle + s2 above/below (floor 56) |
| L4 | inspector | caption advance 20, controls 26/30, gaps 6/8/10/14/18 → captions under swatches/controls at 2×, uneven section rhythm at 1× (17 vs 22 px) | 820×800 2×, 1920×1080 1× | `app.rs:1872-1881, 1911-1938, 1989, 2010, 2057, 2088, 2124-2141, 2164, 2177` | `before/scn-inspector-2x.png` → `after/scn-inspector-2x.png`; `before/dark/…-desktop.png` → `after/dark/…-desktop.png` | fixed — `caption_adv`, `heading_adv`, `control_h`, `small_pill_h`; rows `s2` apart, sections `s4` |
| L5 | ALIGN segmented control | quarter-width cells clip "Center/Right/Justify" at 2× | 2× | `app.rs:2062-2087` | same | fixed — wraps to two connected rows when the widest label + 2·s2 exceeds a cell |
| L6 | TYPE row | font input reserved at 136 px ("56 pt" clipped at 2×); B/I toggles then collide with the widened input | 2× | `app.rs:2036-2047` | same | fixed — reserve = 2 steppers + measured "999 pt" + 2·s2 (floor 136); toggles wrap below when the row is too narrow |
| L7 | rail header / Add·Dup·Del | fixed 30/34 px rows, 14 px pad; 2× labels overflow | 2× | `app.rs:1320-1326, 1769-1818` | `before/scn-many_rail-2x.png` → `after/scn-many_rail-2x.png` | fixed — `rail_header_h()`, `control_h()`, `s4` pad, `s2` gaps |
| L8 | layout / background pills | width estimated at 12 px while painted at 13 px semibold | all | `app.rs:2293` | desktop PNGs | fixed — measured at the painted style |
| L9 | drop-to-delete zone | label width estimated; text at `cy − 8` | drag | `app.rs:4007, 4024` | — | fixed — measured cluster, centred on the measured line box |
| L10 | rail thumbnails | `PageSidebar` viewport handed out before the metrics refresh → thumbnails painted over the rail header at 2× | 2× | `app.rs:5578-5583` (draw order) | intermediate AFTER → `after/scn-many_rail-2x.png` | fixed — refresh runs before flap/rail sync |
| L11 | inspector at 2× on an 800 px-tall window | sections run past the viewport; no scroll container | 820×800 2× | `app.rs:1841` | `after/scn-inspector-2x.png` (ARRANGE clipped at the bottom) | open — needs F1/F6 (scrollable inspector); not a spacing defect |
| L12 | slide canvas text | canvas glyphs follow the UI text scale (document contract) | 2× | `render.rs:260-331` | `before/large-text.png` | open — plan `app-text-geometry` task 2 bullets 1–2, out of A7 scope |
| L13 | thumbnail rail | 9 px thumbnail gap (off-scale) inside the shared `PageSidebar` | all | `ui/src/widget/page_sidebar.rs` | matrices | open — needs F6 |
| L14 | all chrome strings | literals, not fluent keys (`--pseudo-locale` has no effect) | — | `app.rs` labels | `after/pseudo-locale.png` | open — i18n, out of alignment scope |
| L15 | speaker-notes box (found in code review) | with measured rows, a 2× inspector in a short window made `notes_top + caption` exceed `inspector.y1 − pad`: an inverted rect (`contains` always false) hid the notes hit target | 2× ≤ 800 px tall | `app.rs` `build_inspector_notes` | — (regression in `two_x_chrome_rows_follow_measured_text`) | fixed — the box is at least one control tall and always starts under the sections; content flows below the panel until L11 lands |
| L16 | drop-to-delete zone (found in code review) | `rail.y1 − 54 .. − 10` literals no longer covered the taller 2× action row | 2× drag | `app.rs` `slide_delete_zone` | — | fixed — sized from `control_h`, `s4`, `s2` |

Rail at 1×: the thumbnail list now starts 16 px lower (`y0 + s2 + 34 + s2`
instead of `y0 + 34`). This is a deliberate outcome change, not a floor
violation: the overlay's Close button (`y0 + 5 .. 35`) previously overlapped
the first thumbnail row; the header row now clears the list at every scale
(`before/scn-narrow.png` → `after/scn-narrow.png`).

Conventions observed: `TOOLBAR_MIN_H`, `TOOLBAR_PILL_MIN_H`,
`CONTROL_MIN_H`, `SMALL_PILL_MIN_H` are the 1× floors; `SWATCH_H = 26`,
`NOTES_MIN_H = 188`, `FONT_INPUT_W = 136`, `TOOLBAR_ICON = 18`,
`PALETTE_DOT_SLOT = 26` are named authored sizes; `TITLE_LINE_GAP = 2` and
`MENU_ROW_SEAM = 4` are the deliberately off-scale seams. Base fonts are
named (`FONT_CAPTION … FONT_TITLE`) and passed unscaled to the renderer,
which applies the accessibility scale once.

## Word-processor

**No code changed.** `apps/word-processor/src/{widget,zoom_control,
style_menu,template_picker,…}.rs` carry uncommitted user work in the main
checkout (template-menu routing, measured zoom/layout-bar controls, pinch
zoom, virtualisation — per `docs/superpowers/audits/2026-09-16-template-menu-ux.md`),
so every finding below is recorded, not edited.

| id | surface | defect | viewport / theme | file:line | evidence | status |
|---|---|---|---|---|---|---|
| W1 | toolbar rows | `toolbar_height` and `draw_toolbar` pin `TOOLBAR_H = 50` per row while `ui::chrome::toolbar::draw` paints 2× glyphs — labels overflow the row at 2× | 800×900 2× | `widget.rs:11205-11290, 11329-11345` | `word-processor/before/large-text.png` | open — WIP zone; use `ui::chrome::toolbar::metrics(text_scale).row_height` in both places |
| W2 | shared `ui::chrome::toolbar::draw_item` | label at `cy − 9.0` with a literal 15 px font (font-size centring, unscaled offset) | 2× | `ui/src/chrome/toolbar.rs:131-133` | same | open — needs F2 |
| W3 | layout bar | "Ränder/Abstände" chips, hint and word/char counts collide at 2× (corrected-review-2 P1) | 800×900 2× | `widget.rs:3714` | same | open — WIP-owned (template-menu audit: "zoom and bottom layout controls reserve measured label dimensions") |
| W4 | zoom control | "94% +" clipped at the right edge at 2× | 800×900 2× | `widget.rs:11050`, `zoom_control.rs` | same | open — WIP-owned |
| W5 | page fit | 480 px first open fits the paper at 55 %, body unreadable | 480×900 | `widget.rs:5344-5361` | `before/narrow.png` | open — plan `app-composition` task 9 (not A7) |
| W6 | styles panel colour row | the extra (custom) swatch sits under the label column while the swatch row starts at the control column | 1200×800 | `widget.rs` styles panel | `before/styles.png` | open — minor, WIP zone |

1× observations that pass: toolbar labels sit on the 25 px centre line
(`before/dark/…-desktop.png`); the ruler band is aligned to the paper edges
at every zoom (`desktop`, `laptop`, `tablet`, `narrow`, `find`); the layout
bar's text is centred and the optional hint is dropped when it cannot fit
(`narrow`, `compact`); find bar controls share one centre line
(`before/find.png`); history band buttons are on one row with measured labels
(`before/history.png`); the side modules (`find_bar`, `history_panel`,
`outline_pane`, `recovery_banner`, `review_overlay`, `style_menu`,
`template_picker`, `ruler`, `zoom_control`, palettes) contain no font-size
centring or literal chrome sizes — they compose shared widgets. The recent
template-menu fixes were not touched.

## Code review

`/code-review medium` over the diff returned eight findings. Fixed: the
inverting notes box (L15), the unbounded sheet-tab width (S15), the stale
delete-zone literals (L16), the missing `"999 pt"` in the slides label set
and the sheets fallback estimate not being in final pixels (both now
measured/scaled), the in-flight tab-rename rect (S17), the unclamped grid
scroll after a metrics change (S16), and the no-op metrics comparison in
slides (plain assignment now). Acknowledged rather than changed: the rail's
1× list top moving 16 px (deliberate, see the note under the slides table)
and the status pill's label centring (S13, shared widget, needs F2 — the
height half is compensated in sheets). Not taken: a per-frame invalidation
key for the label re-measurement (the renderer caches measurements; ~45
short strings per frame) — recorded as a follow-up.

## Tests

All cargo commands ran with
`RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1` and
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."`
inline. The `RUSTFLAGS` override is required because this worktree sits
under `/Users/xaver/dev/qos/`, whose `.cargo/config.toml` cargo merges in and
which appends `-C target-cpu=generic` for aarch64 macOS — `ring` then fails
its CPU const-assert. The value is the repository's exact macOS flag set; no
cargo config was edited.

New regressions (all fail on the pre-change geometry):

- sheets `app::tests`: `chrome_metrics_keep_the_one_x_density`,
  `chrome_bands_and_labels_follow_measured_text_at_two_x`,
  `status_bubble_floats_above_the_scrollbar_rail`,
  `settings_panel_labels_align_with_their_chip_rows`,
  `tab_menu_reserves_room_for_its_caption_and_swatches_at_two_x`,
  `sheet_tabs_keep_their_floor_and_ceiling_at_every_scale`
- sheets `grid::tests`:
  `measured_metrics_drive_automatic_geometry_but_never_manual_sizes`
- slides `app::tests`:
  `title_yields_to_the_leftmost_placed_toolbar_control_at_480`,
  `one_x_chrome_density_is_unchanged`, `two_x_chrome_rows_follow_measured_text`

Runs:

- `cargo test --locked -p sheets --lib` — **242 passed, 0 failed, 1 ignored**
  (the ignore is pre-existing; this diff adds none).
- `cargo test --locked -p slides --lib` — **143 passed, 1 failed, 0 ignored**.
  The failure is pre-existing on the base:
  `app::tests::notes_typing_writes_through_and_up_down_move_the_caret` —
  `panicked at apps/slides/src/app.rs:…: ArrowUp moved the notes caret onto
  the first shaped line`. Verified by checking out the base
  `apps/slides/src/{app,toolbar_tools}.rs`, running that single test
  (FAILED at the same assertion, base line 9785), and restoring this diff.
  Not touched.
- `cargo test --locked -p slides --lib -- title_yields one_x_chrome
  two_x_chrome toolbar_tools` — 5 passed.
- Compile gates: `cargo check --locked -p sheets --tests`,
  `cargo check --locked -p slides --tests`,
  `cargo check --locked -p word-processor --tests` — all `Finished`.
- `cargo clippy --locked -p sheets -p slides --tests` does not reach either
  app on this base: it stops on pre-existing deny-level lints in
  `ui/src/webview/chromium/{mod,protocol}.rs` and `ui/src/state/owned_restore.rs`
  (`unreadable_literal`, `arithmetic_side_effects`, `or_fun_call`,
  `assigning_clones`). Those files are outside A7; the app crates' own
  `#![deny]` attributes are enforced by the `cargo check --tests` gates above.

## Limits

- **Base correction.** The worktree was cut from `24ee08c66` (a lineage
  266 commits off `codex/macos-shell`). On the coordinator's instruction it
  was rebased with `git rebase --onto codex/macos-shell 24ee08c66` (no own
  commits existed; two environment cherry-picks were dropped as already
  upstream), `ui-review/lockfix` was cherry-picked for the `--locked` build,
  binaries were rebuilt and every BEFORE capture was redone on the corrected
  base. Findings from the first pass were re-verified against the new source;
  the sheets 2× and slides 2× P1s are more severe here than on the old base
  (no text-scale plumbing at all).
- **Word-processor WIP.** No edits in word-processor; six findings are open
  and attributed above. `QOS_WP_HEADER_SHOT` / `QOS_WP_REVIEW_SHOTS` do not
  exist on this base; the header/review surfaces were covered through
  `--comments`, `--threaded-comments` and `--image-object`.
- **Not covered.** Slides presenter / help / picker / menus were captured and
  inspected at 1× (no spacing defects found in the shared menus); their 2×
  behaviour was not separately captured. Sheets `commandpalette`, `file`,
  `format` menus are shared widgets and were only visually confirmed.
- Static shots prove geometry; they do not prove live pointer/keyboard
  interaction beyond the module tests above.
