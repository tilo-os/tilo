# A7 — Office editors, round two: alignment and spacing

Unit A7 of the 2026-09-17 round-two alignment review: **word-processor**,
**sheets**, **slides**. Branch `ui-review/r2-a7-office-editors`, cut from the
merged round-one tip `381eff6a8` (`Merge ui-review/integration`). Delivery is
local only: nothing pushed, no PR.

Round one left word-processor untouched (uncommitted user work) and recorded
six findings; that work is now committed, so this round edits the editor with
small, local changes (token substitutions, measured centring, fixed gaps) and
puts every new regression in a new file. Sheets and slides were rebuilt and
re-run to prove round one survived the merge, then their open items were
finished and all three chrome sets were looked at again at 800×700, 480×900
and 200 %.

The document canvas of each editor (page, cells, slide) is authored geometry
and was not touched; the chrome around it was.

## Method

1. `git reset --hard 381eff6a8`, `cargo build --bins -p word-processor -p
   sheets -p slides` (4 m 13 s, sccache), `cargo test -p sheets --lib` and
   `-p slides --lib` to confirm round one's counts.
2. BEFORE captures per app (isolated `HOME`/XDG profile, vello CPU): dark and
   light `--responsive-matrix`, 2× `--text-scale 2` at 800×900, 480×900,
   800×700, and every `--scenario` / route at 1200×800 plus its 2× twin at
   800×900. Every PNG opened.
3. Source pass over the chrome files for literal row advances, per-glyph
   width estimates, font-size centring and fixed slot widths.
4. Fixes from one measured metric per chrome region (renderer line boxes +
   `theme.tokens.spacing`), with the classic 1× values as floors so a 1×
   frame keeps its density; hit rects come from the geometry that is painted.
5. New regressions in new files; AFTER captures; `code-review medium`; the
   findings that were real were fixed.

Capture directories (worktree-relative), `before/` and `after/` each:

- `target/ui-review/word-processor/{before,after}/` — `dark/`, `light/`
  matrices, `large-text.png`, `narrow.png`, `compact.png`, and for every
  route (`history`, `styles`, `comments`, `threaded-comments`, `find`,
  `error`, `image-object`) a 1200×800 shot and a `-2x` 800×900 shot.
- `target/ui-review/sheets/{before,after}/` — as above with the scenarios
  `format docsettings editing summary tabmenu fbarselect scrollbars charts`.
- `target/ui-review/slides/{before,after}/` — as above with `overview
  many_rail narrow inspector inspector_widgets notes_edit presenter picker
  toolbar_menu`.

## Word-processor

**Scope.** `apps/word-processor/src/widget.rs` (toolbar packer, layout bar,
styles panel, image inspector, numeric rows, `draw_btn`), `find_bar.rs`. The
user's WIP modules (`widget/` split, `unsaved_changes.rs`, `style_menu.rs`,
`template_picker.rs`, `zoom_control.rs`, locales) were not edited.

**Root cause shared by most findings.** The styles panel, the find bar and
the layout bar laid their rows out at literals (30 px row, 24/26 px chip, 26
px field, 5 px label offset, 7.2 / 6.6 px per glyph) while the text inside
followed the accessibility scale. Fixed with three small resolved structs —
`ChromeTokens` (spacing/radius tokens next to `ChromeColors`),
`PanelMetrics` (measured panel line boxes, 1× floors) and `FindMetrics`
(measured bar geometry) — refreshed once per frame in `draw_frame_chrome`,
plus one `ToolbarModel` stating the toolkit's row model.

| id | surface | defect | viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| W1 | toolbar rows | `TOOLBAR_H` per row while the toolkit paints scaled labels | 800×900 2× | `widget.rs` `toolbar_row_height` | `word-processor/before/large-text.png` → `after/large-text.png` | fixed upstream by the WIP (`toolbar_row_height` is measured); this round consolidates its 14/6/14/18 pad-gap-sep-margin, the `label + 12` item and the `label + 16` row into one `ToolbarModel` (`TOOLBAR_MODEL`) mirroring `ui::chrome::toolbar::draw` — to be replaced by F6's `ToolbarMetrics`/`item_rects` when published; regression `toolbar_packer_mirrors_the_toolkit_row_model` draws each packed row through the toolkit and checks it lands inside the packer's width |
| W2 | shared `chrome::toolbar::draw_item` `cy − 9` | label at a literal offset | 2× | `ui/src/chrome/toolbar.rs` | `after/large-text.png` | fixed upstream (F2): labels now centre on the measured line box; recorded, no app change |
| W3 | layout bar | chips/hint/counts collide at 2× | 800×900 2× | `widget.rs:3714` | `before/large-text.png` → `after/large-text.png` | fixed by the WIP's measured hint drop; this round: chip = label + 2·`s2`, chip height = measured line + `s1` (floor 22), centred on the bar, gaps `s2`/`s3`, radius `radii.md`, bar = chip + 2·`s1` (floor 32); `layout_bar_status_rect` takes its margin as a token |
| W4 | zoom control | "94 % +" clipped at 2× | 800×900 2× | `zoom_control.rs` | `after/large-text.png`, `after/find-2x.png` | fixed upstream by the WIP (measured `ZoomControl`); verified at 2× / 480 / 800×700, not edited |
| W5 | page fit at 480 px | paper fits at 55 % | 480×900 | `widget.rs` `auto_fit_page` | `after/narrow.png` | alignment part verified: the sheet is centred with equal gutters, the ruler band tracks the paper edges and the page caption sits under the sheet; the fit policy itself stays with plan `app-composition` task 9 — open |
| W6 | styles panel colour row | custom chip under the label column while the swatch strip starts at the control column | 1200×800 | `widget.rs` `draw_panel_colour` | `before/styles.png` → `after/styles.png` | fixed — chip at the strip's `sx0`, strip = `inner_w − measured label − s3` clamped to the classic 120..196 (grown with the text), swatch gap `s1`, label centred on the swatch row |
| W7 | styles panel at 2× (new) | fixed 250 px column and 30/24/22/18/16 px rows: captions under chips, chips clipped, value boxes overrun | 800×900 2× | `widget.rs` `draw_styles_panel` + sections | `before/styles-2x.png` → `after/styles-2x.png` | fixed — `PanelMetrics`: `control_h = lh_label + s2` (floor 24), `row_adv = control_h + 6 px seam`, captions/name/hint/dims advances from their line boxes (floors 18/24/22/26), chip widths measured (floors 96/60/28/62), `panel_w = 250 × text scale`; `draw_btn` and the numeric value box centre on the measured label instead of a per-glyph estimate |
| W8 | image inspector at 2× (new) | same literal rows: "Position" heading under "Crop 5 %", "Smalle." / "Horizon." clipped | 800×900 2× | `widget.rs` `draw_image_inspector`, `draw_panel_image_flip` | `before/image-object-2x.png` → `after/image-object-2x.png` | fixed — `wide_control_h` (floor 26), rows `ch + s2` / `ch + s3`, button widths grown with the text |
| W9 | find bar at 2× (new) | fixed 188 px fields, 30/26/30 toggles, 28/72/40 actions, `7.2 px × chars` centring: "Keine Treffer" under the arrows, "Ersetzen" over "Alle", "×" off the window | 800×900 2× | `find_bar.rs` `draw_*` | `before/find-2x.png` → `after/find-2x.png` | fixed — `FindMetrics` measured against the canvas width before the host reserves the band: fields = measured line + `s2` tall, labels measured + 2·`s2` wide, count slot = widest readout; when one row would run off the window the bar takes a second row (find + toggles + count above, replace + actions below), fields yielding towards the classic width first |
| W10 | version-history band (new) | band labels, buttons and tooltip at 4× (double-scaled token sizes) | 800×900 2× | shared `ui` scrubber via `history_panel.rs` `draw` | `before/history-2x.png` (= `after/history-2x.png`) | open — the band is the shared history scrubber in `ui/src/history_mode/*`, the user's active WIP; recorded for its owner (systemic finding 1 of round one) |
| W11 | review cards / comment thread panel (new) | 2× header text in 1×-sized cards, 1× body text; "Abbrechen" under "Senden" | 800×900 2× | `widget.rs` review card painter, `review_overlay.rs` | `before/threaded-comments-2x.png` (= `after/…`) | open — out of this round's small-edit budget; the cards need a measured header/body rhythm like the panel's |
| W12 | ruler labels (new) | 9 px inch labels and the "in" caption keep a 24 px band at 2× | 800×900 2× | `ruler.rs` `draw` | `after/large-text.png` | open — the band is a document-space ruler aligned to the paper; its label metric should come from one measured base-font metric (plan `app-text-geometry`), recorded |

Conventions observed: `PANEL_CONTROL_H` 24, `PANEL_WIDE_CONTROL_H` 26,
`PANEL_CAPTION_ADV` 18, `PANEL_NAME_ADV` 24, `PANEL_HINT_ADV` 22,
`PANEL_DIMS_ADV` 26, `PANEL_HEADING_ADV` 20, `PANEL_SWATCH_H` 22,
`LAYOUT_CHIP_H` 22, `LAYOUT_BAR_H` 32, `find_bar::HEIGHT` 42, `FIELD_H` 26,
`FIELD_W` 188 are the 1× floors; `PANEL_ROW_SEAM` 6, `PANEL_STEP_SEAM` 2,
`TOGGLE_SEAM` 5, `ACTION_SEAM` 6 and `PANEL_PAD` 14 are the named
deliberately off-scale values. Type roles (`PANEL_LABEL_FONT` 12.5,
`PANEL_CAPTION_FONT` 10.5, `PANEL_HINT_FONT` 11, `PANEL_NAME_FONT` 18,
`PANEL_DIMS_FONT` 15, `LAYOUT_BAR_FONT` 13, `LAYOUT_BAR_STATUS_FONT` 12.5,
find `LABEL_FONT` 12.5 / `ACTION_FONT` 13) are passed unscaled to the
renderer, which applies the accessibility scale once.

1× deltas (intentional, all ≤ 2 px): the "PARAGRAPH STYLE" caption advances
18 instead of 16 (one caption metric for the panel), the "IMAGE" caption 22
stays 22 via `caption_adv + s1`, panel labels centre 0.8 px higher (measured
line box instead of `y + 5`), `draw_btn` labels centre on their measured box.

Not changed (recorded): `draw_btn`'s 6 px radius and `panel_divider` remain
literals — `draw_btn` is a free function without a theme handle and is
called from ~25 sites; the local `centred_line_top` duplicates
`ui::widget::text::centered_text_top`, which `ui/src/widget/mod.rs` does not
export (this unit may not edit `ui/`; F2 should add `pub use
text::centered_text_top;` and this copy goes).

## Sheets

Round one survived the merge: `cargo test -p sheets --lib` — 242 passed,
1 ignored (pre-existing) before any change.

| id | surface | defect | viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| S13 | shared `ui::status_bubble::draw_pill` | label top at `y0 + (h − text_size)/2` with the renderer-input size: the label sits `(lh − text_size)/2` below centre — ≈ 1.6 px at 1× (`sm` 13 → 16.25 line), ≈ 9.75 px at 2× | 2× | `ui/src/status_bubble.rs:157` | `after/large-text.png` | open — F2/X1; sheets keeps the height compensation from round one; recorded with the numbers |
| S14 | default column widths at 2× | do not follow text | 800×900 2× | `grid.rs` | `after/large-text.png` | open — content geometry (plan task 1); recorded, not a chrome defect |
| S18 | chart cards at 2× (new) | title at `y0 + 21`, actions at `y0 + 26`: a 2× title paints under Open · Edit · Delete; plot at `y0 + 86` fixed | 800×900 2× | `app.rs:4553-4600` | `before/charts-2x.png` → `after/charts-2x.png` | fixed — `ChromeMetrics::chart_card_rows`: the action band starts where its centred `xs` label clears the `sm` title by `s1` (floor: classic 5 px seam), plot `s4` below, `s4` bottom; the 1× card is the classic 176 px; title elided to the card |
| S19 | comment card at 2× (new) | heading 22 / body 48 / detail 84 / actions 112 literals: body runs out of the card, detail under the body | 800×900 2× | `app.rs:4725-4805` | same | fixed — `comment_card_rows`: each advance is the line box + token, floored at the classic (26/36/28), card = classic 178 px at 1×; heading and body elided to the card; the resolve/reopen target is a full 44 px band centred on its detail line that ends where the action band begins (it used to start 26 px above the line) |

Regressions (new file `apps/sheets/src/app/r2_annotation_cards.rs`):
`annotation_card_rows_clear_each_other_at_every_scale` (rows, floors,
classic sizes at 1×, growth at 2×) and
`painted_annotation_targets_follow_the_measured_rows` (production painters:
the registered hit targets sit under the 2× title and the toggle ends at the
action band). `seeded_app` / `measured_app` became `pub(super)` for reuse.

## Slides

Round one survived the merge: `cargo test -p slides --lib` — 143 passed,
1 failed (the pre-existing `notes_typing_writes_through_and_up_down_move_the_caret`,
same assertion) before any change.

| id | surface | defect | viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| L11 | inspector at 2× on an 800 px-tall window | sections run past the viewport; ARRANGE and the notes unreachable | 820×800 2× | `app.rs` `compute_chrome` / `draw_editor_inspector_layer` | `before/inspector-2x.png` → `after/inspector-2x.png` (scroll lane at the panel edge; the scrolled state is proven by the regression) | fixed — the panel drives the shared `Scrollable` by hand: `compute_chrome` measures the content extent, shifts every inspector item (buttons, labels, notes box, font field) by the clamped offset and culls the buttons, labels and font field whose centre lies outside the viewport (a centre test, so `in_inspector` stays a centre test and nothing scrolled off paints over the toolbar; the notes box is last in the content and can only lie below the viewport, so it keeps its shifted, clipped rect); `draw` syncs viewport/content size and paints the bar (`always_visible`, idle thumb = toolkit border tone); the wheel over the panel scrolls it and is consumed only when it moved; a culled font field is parked at `Rect::ZERO` so its stale rect cannot swallow toolbar presses |
| L13 | `PageSidebar` 9 px thumbnail gap | off-scale | all | `ui/src/widget/page_sidebar.rs:100` | `after/many_rail-2x.png` | fixed upstream (F6): `GAP = 12.0 (= s3)`; recorded |
| L14 | chrome strings | literals; `--pseudo-locale` had no effect | — | `app.rs`, `toolbar_tools.rs`, `presentation.rs`, `overview_grid.rs` | `after/dark/…-desktop.png` | fixed — `apps/slides/locales/en-US.ftl` + `src/strings.rs` (`tr`, `tr_args`, per-thread install like photos/chat) over `ui::i18n` (no Cargo change: `fluent` is already `ui`'s); 78 sites in `app.rs` (toolbar, rail, inspector captions and buttons, context menu, tooltips, toasts, portal titles, accessible names), the six insert tools, the presenter and overview captions; `tr` memoises per `I18nManager::revision()` + pseudo-locale so the per-frame/per-press `compute_chrome` costs a hash probe, not a fluent resolve. Model display names (`Layout`/`Background`/`PaletteKind`/`ElementKind::label`) and `help_overlay.rs` remain literals — content names and a shortcut table, recorded |
| L17 | presenter view at 2× (new) | "PRESENTER VIEW" under the title, "ELAPSED · slide 2 / 5" under the clock, "CURRENT"/"NEXT" captions over their frames, key hints off the right edge | 800×900 2× | `presentation.rs:728-800, 839-1030` | `before/presenter-2x.png` → `after/presenter-2x.png` | fixed — `PresenterMetrics` (measured title/caption/clock/position/footer line boxes + `s1`/`s3`/`s4`): caption under the title line, position under the clock's digits, header = the lower column (floor 60), section captions advance `lh + s1` (floor 20), footer band `lh + 2·s3` (floor 40) with the hints centred and elided; the title elides to the room left of the clock |
| L18 | overview header at 2× (new) | title over the "5 slides · click to open" hint; divider at `y0 + 60`, grid at `+64` fixed | 800×900 2× | `overview_grid.rs:32, 101-129` | `before/overview-2x.png` → `after/overview-2x.png` | fixed — header rows measured at draw (`s4`, `lh_title`, `lh_sub + s1`, floors 22/22), remembered in a `Cell` so `geometry` (hit-test, wheel, clamp) starts the grid under the divider the painter drew (`HEADER_H_1X` 64 until the first draw); title and hint elided |
| L19 | fill-colour popover at 2× (new) | the shared `ColorPicker`'s hex / rgb readouts run past the 260×220 popover and off the window | 820×800 2× | `app.rs` `fill_picker_popover_rect`, `ui/src/widget/color_picker.rs` | `before/inspector_widgets-2x.png` → `after/inspector_widgets-2x.png` | partly — the popover's width now grows with the text scale (the picker spans its sliders across the rect); its readouts still overrun because the widget lays them out beyond its rect at 2× — a `ColorPicker` defect for F4/F2, recorded |

Regressions (new file `apps/slides/src/app/r2_inspector_scroll.rs`):
`two_x_inspector_scrolls_instead_of_running_past_the_viewport` (overflow in
the 2× scenario, every kept control inside the panel, scrolling to the end
brings ARRANGE and the notes box in, culled swatches leave the hit list and
nothing leaks into the toolbar band, the wheel is consumed over the panel
and not over the canvas, the culled font field is parked),
`one_x_inspector_that_fits_neither_shifts_nor_scrolls`,
`chrome_labels_are_fluent_resolved_text`; `strings::tests`
(`every_key_the_chrome_uses_resolves`,
`formatted_keys_interpolate_their_arguments`). `app_sized` / `measured_app`
became `pub(super)`.

Accessibility note: the accessibility tree already filtered controls by
`self.rect.contains`; culled inspector items are the ones that were
unreachable anyway, so nothing reachable before the change is lost.

## Fresh eyes at 800×700, 480×900 and 200 %

- Word-processor: 800×700 and 480×900 pass (the layout bar drops its hint
  cleanly, the toolbar wraps to two rows at 480 with the zoom on the second);
  200 % findings W7–W12 above.
- Sheets: 800×700 and 480×900 pass; 200 % passes except the cards (S18,
  S19) — the menu bar, formula bar, settings panel, tab menu, summary pill,
  dropdown and scrollbars all keep round one's measured layout.
- Slides: 800×700 and 480×900 pass; 200 % findings L11, L17–L19 above; the
  toolbar overflow menu, rail, notes editing and toasts pass.

## Code review

`/code-review medium` over the working tree returned four findings. Fixed:
the shifted font field / notes box were not culled, so a scrolled-off field
in the toolbar band would have swallowed presses (culled by the same centre
rule, and a culled field is parked at `Rect::ZERO`; regression extended);
`compute_chrome` resolved ~40 fluent messages on every call (memoised per
revision in `strings::tr`); the misplaced `rail_rect` doc comment. Not taken:
exporting `centered_text_top` from `ui/src/widget/mod.rs` — a `ui/` edit
outside this unit's files; the local `centred_line_top` is marked for
deletion once F2 exports it. The reviewer's own `cargo check` failed on
`ring`'s CPU const-assert because it ran without the inline `RUSTFLAGS`
(see Limits); every gate below was run with them.

## Tests

All cargo commands ran with `RUSTFLAGS="-C force-frame-pointers=yes
--remap-path-prefix=/build=." RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G
CARGO_BUILD_JOBS=1 PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig`
inline and `--locked --no-default-features`.

- `cargo test -p sheets --lib` — **244 passed, 0 failed, 1 ignored**
  (242 + 2 new; the ignore is pre-existing).
- `cargo test -p slides --lib` — **148 passed, 1 failed, 0 ignored**
  (143 + 5 new). The failure is the pre-existing
  `app::tests::notes_typing_writes_through_and_up_down_move_the_caret`
  (`ArrowUp moved the notes caret onto the first shaped line`), the same
  assertion as at the base; not touched.
- `cargo test -p word-processor --lib` — **325 passed, 5 failed** (319 + 6
  new). The five failures are spell-check tests
  (`document_autocorrect_is_reversible_at_a_space_boundary`,
  `misspelled_word_yields_a_squiggle_span_at_its_x_range`,
  `right_click_on_misspelled_word_offers_add_to_dictionary_and_clears_squiggle`,
  `right_click_suggestion_replaces_the_word`,
  `spell_cache_reuses_unchanged_blocks_and_rechecks_after_edit`) — nothing is
  flagged on this macOS host (no dictionary). **Verified at the base**: a
  throwaway worktree of `381eff6a8` (`git worktree add --detach
  /tmp/a7-base-check 381eff6a8`, removed afterwards) fails the same five
  with the same assertions. Not touched.
- New regressions run alone: `cargo test -p word-processor --lib --
  r2_chrome_metrics find_bar` — 11 passed; `-p sheets --lib --
  r2_annotation_cards` — 2 passed; `-p slides --lib -- r2_inspector_scroll
  strings` — 5 passed.
- Gate: `cargo check -p word-processor -p sheets -p slides --tests` —
  `Finished`, no warnings.

## Limits

- **Base spell tests** fail on macOS regardless of this diff (verified above);
  a Linux run with a dictionary is the honest check.
- **`ui/` was not edited**, so three findings this unit can see are recorded
  for their owners: `status_bubble::draw_pill` label centring (F2), the
  `ColorPicker` readouts overrunning its rect at 2× (F4/F2), the
  `centered_text_top` export (F2). The version-history band's 4× glyphs sit
  in the user's active `ui/src/history_mode/*` WIP and were left alone.
- **Static shots prove geometry**; the inspector's scrolled state, the wheel
  routing and the culled hit targets are proven by the module tests, not by
  a capture (the shot harness has no scroll input).
- macOS: `shots`, `qos-control` and `--backend headless` are Linux-only;
  the `picker` scenario's file chooser fails through ZBus and shows its
  toast — expected here.
- The temporary capture helper (`target/ui-review/cap.sh`) and the two
  replacement scripts live under `target/` and are not part of the commit.
