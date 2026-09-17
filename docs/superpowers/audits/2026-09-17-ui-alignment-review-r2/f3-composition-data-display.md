# F3 — Composition and data display, round two (2026-09-17)

Branch `ui-review/r2-f3-composition-data-display`, cut from the merged round-one tip
`381eff6a8` ("Merge ui-review/integration"). Round one's report:
[`../2026-09-16-ui-alignment-review/f3-composition-data-display.md`](../2026-09-16-ui-alignment-review/f3-composition-data-display.md).

## Scope

Owned files: `ui/src/widget/{card,card_composition,composition,composition_parts,form,
form_composition,empty_state,data_table,table,tree_view,virtual_list,icon_grid,stat_tile,chart,
image,qr,banner,conflict_banner,conflict_picker,merge_strategy,search_results,resource_actions}.rs`,
`ui/src/widget/form_composition/`, `ui/src/app/{card_composition_tests,form_composition_tests}.rs`,
plus the round-one fixture `ui/tests/f3_composition_fixtures.rs`. One file outside the list was
edited on the coordinator's hand-off: `ui/examples/widget_snapshot.rs` (F5-15).

Not touched, per the round-two brief: the text-size double-scaling and text-centring expressions
(`renderer_type_size`, `centered_text_top` and every call that feeds `theme.tokens.typography.scale.*`
through them) — unit X1 is moving those into `ui/src/text_metrics.rs`. The two layout helpers this
unit adds (`type_line_height`, `text_band`) sit at the top of `data_table.rs`, away from that hunk,
and are exported so the fixture asserts against the production formula rather than a copy.

## Method

1. BEFORE captures on the merged tip, opened one by one: the round-one fixture at 1× and 2×
   (`target/ui-review/ui/f3_fixture/before/`), the registered examples (`card_composition`,
   `form_composition`, `table_shots`, `data_table`, `gallery_shots`, `denied_state_shots`,
   `search_showcase`, `widget_snapshot` under `target/ui-review/ui/<scenario>/before/`), and the
   isolated-profile app matrices for store (wide/narrow/detail/empty), settings
   (`region_formats`, `external_accounts`), db_client and monitor, dark + light + 2×
   (`target/ui-review/<app>/before-<tag>/`). Recipe: `target/ui-review/capture.sh`.
2. Every structural band is derived from the theme with the same rule: **band = max(pinned 1×
   default, measured line + token padding)**. `type_line_height(size)` is one line of
   `TextStyle::new(size)` — exactly what `TextRenderer::measure_style` returns for a single line
   (`lines × size × 1.25`), so a band decided at layout time agrees with the line the widget
   later measures and centres at paint time. At the default text scale every band resolves to
   the pinned constant, so 1× layouts are unchanged; explicit, authored sizes stay exact.
3. Regression per fix in the round-one fixture (now asserting the derived bands at 2× on painted
   ink, with an "air" check that tells a grown band from a clipped one) and in module tests; the
   `code-review` skill at `high`; AFTER captures.

## Findings

Capture roots: `B` = `target/ui-review/ui/f3_fixture/before/`, `A` = `.../after/`.

| id | surface | defect | viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| F3-R2-01 | `DataTable` rows and header | **F3-16.** `ROW_HEIGHT` 24 / `HEADER_HEIGHT` 28 fixed: at 2× the 30 px label line overran 24 px rows and the header text touched the first row. | 2×, dark | `data_table.rs:1700-1701` | `B/data_table-2x.png` → `A/data_table-2x.png` | **fixed** — header `max(28, sm line + s3)` (42 at 2×); the unauthored row estimate `max(24, line + s2)` (38); an estimate the host authored through `set_viewport` is honoured and raised only to `line + s1` (34). `header_h` is resolved in `build_layout`; every header consumer (hit-test, scrollbar, body clip, mounted-cell insets) reads it. 1× pixel-identical (`A/data_table-1x.png`) |
| F3-R2-02 | `TreeView` rows, disclosure column | **F3-16.** `ROW_HEIGHT` 22 and `DISCLOSURE_WIDTH` 16 fixed: at 2× rows overlapped and the 24 px glyph overran the label. Hit-testing (disclosure zone, reorder guard, drop preview, transition label) used the pinned constants while paint used `spacing.s4` — a latent hit/paint mismatch under `ui_scale`. | 2×, dark | `tree_view.rs:668-669, 1491, 1530-1533, 1634-1638, 2383` | `B/tree_view-2x.png` → `A/tree_view-2x.png` | **fixed** — row estimate `max(22, line + s1)` (34 at 2×), disclosure column `max(16, line)` (30); `indent_px` (`s4`) and `disclosure_w` are resolved once in `build_layout` and read by paint, hit-testing, reorder and transitions alike. Test `disclosure_hit_zone_is_the_painted_column_at_two_x` presses between the 1× and 2× column edges and expects the branch to toggle |
| F3-R2-03 | `SearchResultsView` rows, group captions | **F3-16.** 38/50/26 px bands fixed: at 2× two-line rows painted over the next caption ("Computing" under "APPS") and a third of the card was empty. | 2×, dark | `search_results.rs:37-39` | `B/search_results-2x.png` → `A/search_results-2x.png` | **fixed** — `RowBands::resolve(theme)` in `build_layout`: one-line `max(38, base line + s3)` (47), two-line `max(50, base + s1 + sm + s3)` (81), caption `max(26, xs line + 2·s1)` (33); `desired_height`, `plan` and `row_at` read the same bands, so overlay height, paint and hit-testing agree. A free-floating overlay that never lays out keeps the constants |
| F3-R2-04 | `ConflictPicker` cards, title, pick pills, keep button | **F3-16.** `CARD_H` 160 / `TITLE_H` 28 / `BUTTON_H` 32 / 24 px pill / 240 px keep button fixed: at 2× the summary line painted under the pick pill and "Pick neither (keep current)" overran its button. | 2×, dark | `conflict_picker.rs:41-57, 244, 273-275` | `B/conflict-2x.png` → `A/conflict-2x.png` | **fixed** — `PickerBands::resolve(theme)`: title `max(28, lg line + s1)`, pill `max(24, sm line + s1)`, button band `max(32, pill + s2)`, card `max(160, text stack + button + insets)`; the keep button's label is measured in `adjust_layout_with_measure` (layout, not paint), so `keep_current_rect` hit-tests the width that is drawn. The fixture's modal grows with the scale as a shell host would; a host that pins the modal still gets the capped-card behaviour |
| F3-R2-05 | `ConflictBanner` band and chips | **F3-16.** `BANNER_HEIGHT` 36 and 64 px chips fixed: at 2× "Review"/"Dismiss" spilled past their chips. | 2×, dark | `conflict_banner.rs:34, 41-42` | `B/conflict-2x.png` → `A/conflict-2x.png` | **fixed** — `ConflictBanner::banner_height(theme)` = `max(36, sm line + 16)` (46 at 2×); chip labels measured in `adjust_layout_with_measure`, chip width `max(64, label + 16)`; `draw_chip` centres on that same measurement. `BANNER_HEIGHT` stays the public 1× constant |
| F3-R2-06 | `Chart` header | **F3-16.** `HEADER_H` 22 fixed: at 2× the title clipped at the top edge and the "6.0" axis label collided with the legend. | 2×, dark | `chart.rs:48, 246-250` | `B/chart-2x.png` → `A/chart-2x.png` | **fixed** — header `max(22, sm line + s1)` (34 at 2×), computed in `draw_chart` from the theme it is handed |
| F3-R2-07 | `IconGrid` default cell | **F3-16.** 96×88 default cell fixed: at 2× every label elided ("Docum…"). | 2×, dark | `icon_grid.rs:77-80` | `B/icon_grid-2x.png` → `A/icon_grid-2x.png` | **fixed** — the *default* cell follows the label type: width `max(96, 8 em of sm)` (192 at 2×), height `max(88, glyph + s2 + sm line + 2·s3)` (102); gap is `spacing.s3`. A cell authored through `with_cell_size` is left exactly as written |
| F3-R2-08 | `Banner` band | `content_height` reserved type *sizes* (14 + 4 + 12) while paint centres measured *line boxes* (17.5 + 4 + 15): at 2× the two-line block ate the padding down to ~5 px. `measure()` answered with `Theme::dark()` — a 1× band inside a 2× layout. | 2×, dark | `banner.rs:300-307, 345-357` | `B/banners-2x.png` → `A/banners-2x.png` | **fixed** — band = `max(legacy size formula, 44, line-box block + 2·s2)`: 54/44 at 1× unchanged, 85/52 at 2×; `measure` returns the height and `metrics.panel_width_utility` cached at `build_layout` |
| F3-R2-09 | `Field` for settings choosers (A3 S9, A4 M5/T6, A5) | `choice_row` rows wrap independently on a phone: "Currency" stays beside its control while longer labels stack in the same card. `Section` already decides Beside/Above once for all its fields; what the toolkit lacked was a control that keeps its own width and trails. | phone 390×844 | `apps/settings/src/widgets.rs:207-229` (consumer) | `target/ui-review/settings/before-region_formats/dark/dev.qos.settings-phone.png` | **fixed (affordance)** — `Field::with_trailing_control()`: the body column is the section's shared control need (`justify_self: End` beside the label, `Start` once stacked), so every chooser in a section switches together and sibling controls share one column and one trailing edge. Test `trailing_controls_switch_with_their_section_and_share_the_trailing_edge` (wide + narrow). Migrating settings/mail/totp onto it is the A-units' call |
| F3-R2-10 | `widget_snapshot` VideoPlayer cell (F5-15) | The example's `stamp()` laid a `flex_grow` leaf out as a root with no flex parent, so it resolved to a zero rect and painted its play glyph over the caption. | example | `ui/examples/widget_snapshot.rs:61-93` | `target/ui-review/ui/widget_snapshot/{before,after}/gallery.png` | **fixed** — a root leaf that fills its slot (`flex_grow > 0`, auto size) is given the cell, as a Row/Column parent would |
| F3-R2-11 | `EmptyState` action label, `ConflictPicker` pick/merged labels | Label ink hard-coded (`Color::WHITE`) or resolved against the wrong ground (`text_primary` on the accent/success fill). | any theme | `empty_state.rs:314`; `conflict_picker.rs:462-476, 850` | — | **fixed** — ink resolved against the fill it sits on with the `Button` primary convention (`ink_on`/`ink_on_any`, white stepped until legible); pick fill and ink share `accent[ACCENT_PRIMARY_IDX]` |
| F3-R2-12 | `Table` (editable grid) `CELL_FONT` 15 | Round one F3-15: off the type scale. | 1× | `table.rs:73` | `target/ui-review/ui/table_shots/{before,after}/table-base.png` | **recorded as deliberate** — the review confirmed 15 px is this grid's authored default and must stay exact at 1× ("do not change shared primitives' default metrics"); `TextInput` already scales it with the renderer's text scale. Documented at the constant |
| F3-R2-13 | Plain `Button` inside `Card::titled_with_action` (F3-17) | At 2× the 120×40 default button keeps its background while its label doubles and overflows the card edge. | 2× | `button.rs` default sizing | `B/legacy_cards-2x.png`, `target/ui-review/settings/before-external_accounts/large.png` | **open — needs F2** (F2-15). The card only places the boxed element (`Row` + `Spacer`); it cannot resize it. Verified F2's round-two Button work is not in this tree |
| F3-R2-14 | `StatTile` without a delta in a `Row` of tiles | `tile_height` reserves only the rows it draws, so a tile without a delta is shorter and floats centred beside its siblings (gallery "Uptime"). | 1× and 2× | `stat_tile.rs:77-85` | `target/ui-review/ui/gallery_shots/before/gallery-dark.png` @ Metrics | open — a shared height would change the default; the host can pass the same shape to every tile |
| F3-R2-15 | `Banner` dismiss | Pointer-only: the × has no focus stop or key path. | — | `banner.rs:400-431` | — | open — reachability, needs focus plumbing + tests; beyond an alignment pass |
| F3-R2-16 | `Card::composed`, `FormPage`, `Field`, `EmptyState`, `Actions` at 2× | Footers share one bottom edge and trailing alignment across siblings; wrapped `Actions` keep the trailing edge; `FormPage` keeps `s6` gutters and the 720 readable width; `Field` labels stack by the measured decision; `EmptyState` centres its measured block. | 2×, dark | — | `target/ui-review/ui/card_composition/before/large-text.png`, `form_composition/before/automatic-{large-text,narrow-large-text}.png`, `store/before-wide/large.png` | no defect |

Round one's fixes re-verified on the merged tip: the ten widgets' type sizes and centring
(`f3_composition_fixtures` 9/9 on the BEFORE tag), the DataTable body clip
(`target/ui-review/ui/data_table/before/data_table-top.png`), the QR centring (`qr.rs:236-238`),
Settings card titles at the `Title` role (`settings/before-*/large.png`).

Not changed on purpose: `DEFAULT_CARD_MIN_WIDTH` / `DEFAULT_CARD_HEIGHT`, `StatTile::tile_height`,
`Table` gutters and `ROW_H` 40 (tied to `TextInput`'s intrinsic height), `EmptyState`'s 34 px
action pill floor and 72/34 px disc/glyph, `SearchResultsView` icon (18 px; icons follow
`ui_scale`, not the text scale), `ConflictPicker` `NARROW_THRESHOLD`/`MIN_CARD_W`,
`Banner` `PAD_X` 14 / `GAP` 10 (no token equivalent), the `"Loading…"` literals (no fluent key in
`ui/locales`; outside this unit).

## Code review outcome

`/code-review high` on the working tree. Acted on:

- Reuse #1 — DataTable/TreeView shared the row-estimate rule twice: one `row_estimate` +
  `RowType` in `data_table.rs`, and one owner of the viewport estimate per widget
  (`refresh_row_estimate`, called from `set_viewport` and from the theme pass). The reviewer's
  suggestion to use `SparseVirtualViewport::set_minimum_height` was tried first and rejected with
  evidence: that floor also applies to heights set per row (`virtual_viewport.rs:1004`), and
  `variable_row_heights_share_placement_and_hit_testing` authors a 10 px row that must stay 10.
- Reuse #2/#3 — the helpers stay in `data_table.rs` (X1 moves them); they are `pub` so the fixture
  imports the production formulas instead of copying them.
- Reuse #4 — chip and keep-button labels were measured twice per frame (once for width, once for
  centring): now measured once in `adjust_layout_with_measure`, and `draw_chip` /
  `draw_keep_button` centre on that measurement.
- Reuse #5 — pick-button fill and ink resolved against different grounds: one ground, the
  `Button` tone convention.
- Reuse #6 — `IconGrid` gap seed is a module-level `Spacing::BASE` const, per codebase convention.
- Angle A (1)+(2) — TreeView hit-testing/reorder/drop-preview/transition used the pinned
  `DISCLOSURE_WIDTH` while paint used the resolved column: all four sites read `disclosure_w`
  and `indent_px`; 2× test added.
- Angle A (3)+(4) — ConflictPicker/ConflictBanner widths were paint-owned and read by input:
  resolved in layout through `adjust_layout_with_measure`, so hit-testing never depends on a prior
  paint (a host that skips the hook keeps the pinned widths and paint measures locally).
- Angle A (5) — `Table` cells back to the authored 15 px at 1× (see F3-R2-12).
- Angle A (6A) — `set_viewport` and the theme pass no longer both write the estimate.

## Tests

All runs: `RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." RUSTC_WRAPPER=sccache
CARGO_BUILD_JOBS=1 cargo … -p ui --locked --no-default-features`.

| Command | Result |
|---|---|
| `cargo test --test it -- f3_` (BEFORE tag, round-one asserts) | 9 passed, 0 failed |
| `cargo test --test it -- f3_` (AFTER tag, derived 2× bands + air) | 9 passed, 0 failed, 0 ignored (captures `f3_fixture/after/`) |
| `cargo test --lib -- widget::card:: widget::card_composition:: widget::form_composition:: widget::form:: widget::composition widget::empty_state:: widget::table:: widget::data_table:: widget::tree_view:: widget::virtual_list:: widget::icon_grid:: widget::banner:: widget::stat_tile:: widget::chart:: widget::search_results:: widget::conflict_banner:: widget::conflict_picker:: widget::qr:: widget::image:: app::card_composition_tests app::form_composition_tests` | 248 passed, 0 failed, 0 ignored (round one: 246; +2 new) |
| `cargo check -p ui --tests --examples` | clean, no warnings |

New tests: `form_composition::tests::trailing_controls_switch_with_their_section_and_share_the_trailing_edge`,
`tree_view::tests::disclosure_hit_zone_is_the_painted_column_at_two_x`. The nine fixture tests now
assert, at 1× and 2×, that each band equals the theme-derived value (pinned constant at 1×) and
that the ink is centred *with air* inside it; the search fixture checks the two-line block and the
icon grid checks reflow into columns and an unelided 2× label; the conflict fixture checks the
banner's laid-out height and that the chips end inside the banner padding.

Intermediate failures, fixed rather than weakened: two new probes used x-height-only ("src") and
descender-bearing ("Alan Turing / Computing") text, whose ink centre sits below the line-box centre
— the fixture header requires descender-free probes, so the fixture data was changed to "Sources"
and "Ada Lovelace / Mathematics". The first DataTable/TreeView floor
(`set_minimum_height`) broke `variable_row_heights_share_placement_and_hit_testing` (an authored
10 px row) and was replaced by the estimate-only rule above.

## AFTER comparison

Rebuilt the three shot examples and `store`, `settings`, `db_client`, `monitor` on the final tree
and re-ran every capture with the `after` tag; every BEFORE/AFTER pair was byte-compared
(`target/ui-review/compare.sh`) and every differing pair pixel-diffed for its bounding box
(`target/ui-review/pngdiff.py`, dependency-free PNG decode).

Byte-identical, as intended (1× and 2× untouched by this unit's changes): store wide/narrow/detail
(9/9 each: dark + light matrices + 2× `large.png`), settings `region_formats` and
`external_accounts` (9/9 each), `card_composition` (6/6), `form_composition` (16/16),
`table_shots` (4/4 — `CELL_FONT` kept at 15), `data_table` (2/2), `denied_state_shots` (3/3),
`search_showcase` (4/4), and in the fixture set `data_table-1x`, `banners-1x`, `chart-1x`,
`stat_tiles-{1,2}x`, `legacy_cards-{1,2}x` (7/18).

Changed, as intended:

- `f3_fixture/after/{data_table,tree_view,search_results,conflict,banners,chart,icon_grid}-2x.png`
  — the F3-16 bands: 42/34 px DataTable header/rows, 34 px TreeView rows with a 30 px disclosure
  column, 47/81/33 px search rows/captions, 206 px picker cards with the summary above the pill
  and a 46 px conflict banner whose chips hold their labels, 85 px two-line banners with `s2` of
  air, a 34 px chart header clearing the top axis label, 192×102 icon cells with whole labels.
  `{conflict,icon_grid,search_results,tree_view}-1x.png` differ only by fixture data (the
  descender-free probes, the scale-sized modal, the Column-wrapped grid).
- `store/after-empty/dark/*` and `large.png` (5 of 9; light 4/4 identical) — bbox x 922–998,
  y 682–693 on the desktop shot: the "Refresh catalog" label glyphs, now the contrast-resolved
  ink on the dark accent that the primary `Button` already uses (F3-R2-11); on the light accent
  white already clears the threshold, hence identical.
- `ui/widget_snapshot/after/gallery.png` — the VideoPlayer cell paints its play glyph centred in
  the 320×160 cell instead of over the caption (F5-15).

Differing for reasons outside this unit, verified: `gallery_shots` dark/light differ only inside
x 609–1143, y 649–727 — the "Feedback & Loading" card's indeterminate progress bar and spinner,
which are animated; `monitor` (9/9) shows live process statistics and differs between two AFTER
runs as well (`monitor/after2`); `db_client` (3/9) differs in one 25×9 px element at y 214–222
that also differs between two AFTER runs (`db_client/after2`), so it is time-dependent.

## Limits

- Captures are dark theme at device scale 1 (light for the app matrices); the fixture asserts on
  the dark theme's luminance.
- None of the sampled apps mounts the toolkit's `DataTable`, `TreeView`, `IconGrid` or
  `SearchResultsView` (monitor, notes, music and the shell carry their own same-named types), so
  the app-level evidence for F3-R2-01…07 is the fixture and the `ui-shots` scenarios; the qos-dev
  templates and `format-host` are the in-repo consumers of `TreeView::with_viewport(…, 36.0)`.
- `FrameHarness`/`App` run `adjust_layout_with_measure`; a manual host that computes taffy layout
  directly (the `widget_snapshot` example) keeps the pinned chip/keep-button widths.
- `Dropdown` reports no intrinsic width, so a trailing `Field` control takes the comfortable
  16 em minimum (224 px at 1×) unless the control declares a width.
- macOS: `shell --shots`/`qos-control` paths are Linux-only; app evidence is the per-app
  `--responsive-matrix`/`--shots` hosts, whose `--shots` 2× path ignores `QOS_SHOTS_PANEL`
  (the settings 2× captures show External Accounts, as in round one).
