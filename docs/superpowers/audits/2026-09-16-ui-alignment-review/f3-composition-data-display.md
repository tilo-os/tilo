# F3 — Composition and data display: alignment and spacing review (2026-09-16)

Branch `ui-review/f3-composition-data-display`, cut from `codex/macos-shell` at `2d7e819b6`
(plus the one-line `Cargo.lock` fix `f9a579c9c`).

## Scope

Owned files: `ui/src/widget/{card,card_composition,composition,composition_parts,form,
form_composition,empty_state,data_table,table,tree_view,virtual_list,icon_grid,stat_tile,chart,
image,qr,banner,conflict_banner,conflict_picker,merge_strategy,search_results,resource_actions}.rs`,
`ui/src/widget/form_composition/`, `ui/src/app/{card_composition_tests,form_composition_tests}.rs`.

Out of scope and reported as "needs <unit>": controls (F2), popups (F4), shell chrome (S1/S2),
apps (A1–A10), the `Text` widget and layout containers (F1).

## Method

1. Source pass against the HIG (§7, §7a, §9.6), `ui/FORM_COMPOSITION.md`, `ui/MANUAL_LAYOUT.md`
   and the 2026-09-15 framework-layout audits (Change 5/6 accepted behaviour was treated as
   fixed ground).
2. BEFORE captures, inspected PNG by PNG with the image reader:
   - widget fixtures via the registered examples (`card-composition-shots`,
     `form-composition-shots`, `ui-shots --scenario table_shots|data_table|gallery_shots|
     denied_state_shots|search_showcase|widget_showcase_snapshot`) into
     `target/ui-review/ui/<scenario>/before/`;
   - the immediate-mode widgets the gallery does not exercise at 2× (DataTable, TreeView,
     SearchResultsView, Banner, StatTile, Chart, IconGrid, ConflictBanner/Picker, legacy
     `Card::titled`) via a new `FrameHarness` fixture, `ui/tests/f3_composition_fixtures.rs`,
     rendering 1× and 2× into `target/ui-review/ui/f3_fixture/before/`;
   - representative apps (Store = Card/CardGrid/Actions consumer, Settings = Form and legacy
     Card consumer, Notes) with `--responsive-matrix` dark/light, `--shots` at 480×900 and at
     800×900 with `--text-scale 2`, into `target/ui-review/<app>/before/`.
3. Fixes expressed through `Theme` tokens with measured text, defaults unchanged.
4. Regression per fix (module tests or `FrameHarness` tests), AFTER captures, comparison.

Text-placement rule applied throughout: `TextRenderer::measure*` returns
`lines × size × line_height` (1.25 for `TextStyle::new`), and `draw_text` places the top of that
line box at the transform origin. Any `(row_h - font_size) / 2` centring therefore sits the line
box `0.125 × size` too low at 1× and drifts further at 2× (the renderer scales the measured box
but not the literal).

## Build-environment notes (macOS host, 18 parallel workers)

- Cargo merges `/Users/xaver/dev/qos/.cargo/config.toml` (parent of the worktree directory) into
  this worktree's config; its aarch64 block injects `-C target-cpu=generic`, which makes
  `ring 0.17.14` (pulled in only by `ui`'s default `rustls` feature) fail its static-feature
  assertions on Apple Silicon. Every cargo command in this review used **both** workarounds the
  coordinator listed: `--no-default-features` (the documented macOS mode) and
  `RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."` (the branch's own
  macOS block, verbatim) so the merged flag does not apply; plus `RUSTC_WRAPPER=sccache
  CARGO_BUILD_JOBS=1`. This is an environment quirk of running worktrees under the main
  checkout, not a code change.
- The `ui` package has `autotests = false`; every integration test file is a `#[path]` module of
  `tests/it.rs`. Registering `tests/f3_composition_fixtures.rs` required two lines in
  `ui/tests/it.rs` (outside the owned list; reported here).

## Findings

Capture roots: `B` = `target/ui-review/ui/f3_fixture/before/`, `A` = `.../after/`; app roots
`target/ui-review/<app>/{before,after}/`; example fixtures `target/ui-review/ui/<scenario>/`.
Viewports are logical px at device scale 1; "2×" is `Theme::with_text_scale(2.0)` with the
renderer scale set to match, exactly as `App::rebuild_theme` does on the desktop.

| id | surface | defect | viewport / theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| F3-01 | DataTable, TreeView, SearchResultsView, Banner, StatTile, Chart, IconGrid, ConflictBanner, ConflictPicker, QrWidget error text | **Double text scaling.** Every immediate-mode widget passed the already-scaled `theme.tokens.typography.scale.*` into `draw_text`/`measure`, which the renderer scales again: at 2× text scale glyphs rendered at 4× and overran their rows (table/tree rows unreadable, banner titles clipped, picker cards overlapping). EmptyState had been fixed in `17110d403`; these had not. | 2×, dark | `data_table.rs:127,1213,1314,1324`; `tree_view.rs:2135`; `search_results.rs:332-333,388,481`; `banner.rs:93-94`; `stat_tile.rs:121-123`; `chart.rs:257,265`; `icon_grid.rs:313`; `conflict_banner.rs:212,304,308`; `conflict_picker.rs:348-349,496,726,762`; `qr.rs:215` | `B/data_table-2x.png`, `B/tree_view-2x.png`, `B/search_results-2x.png`, `B/banners-2x.png`, `B/chart-2x.png`, `B/icon_grid-2x.png`, `B/conflict-2x.png`, `B/stat_tiles-2x.png` → same names under `A/` | **fixed** — sizes go through `renderer_type_size` (`data_table.rs`, the size-only twin of `TextRenderer::renderer_input_style`), shared by all ten widgets |
| F3-02 | DataTable header labels, sort glyph, default `with_text` cells, Loading/Error rows | Vertical centring used the font size, not the measured line box: labels sat 1.5 px low in the 28 px header and 0.5 px low in 24 px rows; the sort glyph hung at a literal `x1 - 16` instead of the cell padding; cell x padding was a literal 8. | 1× and 2×, dark | `data_table.rs:129,1218,1237,1316,1326` | `B/data_table-1x.png` → `A/data_table-1x.png` | **fixed** — measured heights via `centered_text_top`, trailing padding `spacing.s2` for the glyph, `spacing.s2` cell padding; clipped rows keep their text where the full row would |
| F3-03 | TreeView row labels and disclosure glyphs | `text_y = (row_h - font_size) / 2` — labels 1.5 px low in 22 px rows; glyph shared the drift. | 1×, dark | `tree_view.rs:2221,2306` | `B/tree_view-1x.png` → `A/tree_view-1x.png` | **fixed** — one measured line box per draw centres label and glyph |
| F3-04 | SearchResultsView rows and group captions | Single-line titles 1.75 px below the icon centre; two-line rows used literals (`+8`, `y1 - size - 9`) so the title/subtitle block was not centred on the icon; subtitle at `sm - 1` (11 px, off the type scale); caption at `y1 - size - 6` with literal 4 px insets. | 1×, dark | `search_results.rs:333,342-346,363,481-503` | `B/search_results-1x.png` → `A/search_results-1x.png` | **fixed** — measured block (title + `s1` + subtitle) centred on the row; subtitle `scale.sm`; caption by measured line box with `s1` insets. Row heights (38/50) unchanged |
| F3-05 | Banner | Title/message block centred by font sizes (`size + 4 + size`): block sat ~1.75 px low against the icon and dismiss glyph; gap literal 4. | 1×, dark | `banner.rs:95-100,115` | `B/banners-1x.png` (transparent bg, see note) → `A/banners-1x.png` | **fixed** — measured title/message heights, `spacing.s1` gap; `content_height` (54/44) unchanged so no app layout shifts |
| F3-06 | StatTile | Trailing icon centred at `PAD + ICON_D/2` (23 px) while the caption line centre is at 21.5 px — icon 1.5 px below the caption. | 1×, dark | `stat_tile.rs:139` | `B/stat_tiles-1x.png` (gallery `Metrics` card) → `A/stat_tiles-1x.png` | **fixed** — icon centred on the caption's measured line; `tile_height` and row stepping unchanged |
| F3-07 | Chart header | Title at `y0 + 2`, legend at `y0 + 4`, dot at `y0 + 4 + size/2`: three different centre lines in a 22 px header (9.5 / 10.25 / 9); bottom axis label at `y1 - 12` overhung the plot by 0.5 px; legend gaps literal 8/12. | 1×, dark | `chart.rs:259,278,283,322` | `B/chart-1x.png` → `A/chart-1x.png` | **fixed** — title, legend labels and dots share the band centre; `s2`/`s3` legend gaps; low axis label bottom-aligned by measured height |
| F3-08 | IconGrid cells | Glyph pinned at `y0 + 10`, label at `+8` below: block top margin 10 vs bottom 15 in an 88 px cell; literals 8/10. | 1×, dark | `icon_grid.rs:332,351,354` | `B/icon_grid-1x.png` → `A/icon_grid-1x.png` | **fixed** — glyph + `s2` + measured label block centred in the cell (12.5/12.5); label still centred under the glyph; hit rect unchanged |
| F3-09 | ConflictPicker cards, ConflictBanner chips | Card text stacked by font sizes with literal 4/8/6 gaps; chip text `sm - 1` (11 px) and glyph `sm + 2` (14 px) off the type scale. | 1×, dark | `conflict_picker.rs:359,369,380`; `conflict_banner.rs:212,308,313` | `B/conflict-1x.png` → `A/conflict-1x.png` | **fixed** — measured stacking with `s1`/`s2` gaps; chip text `scale.sm`, glyph `scale.base` |
| F3-10 | `Card::titled` / `titled_with_action` | Title `Text::with_size(15.0)` — off the type scale (sm 12 / base 14 / lg 16) and renderer-relative rather than theme-driven, so it stayed at 1× in capture paths that scale only the theme (Settings `--text-scale 2`: `target/ui-review/settings/before/large-text.png`, "Email" title vs 2× body). Title/body gap was a literal 12. | 1× and 2× | `card.rs:40-42,120,128` | `B/legacy_cards-1x.png` → `A/legacy_cards-1x.png`; settings `before/large-text.png` → `after/large-text.png` | **fixed** — `TextRole::Title` (lg 16, semibold, follows theme text scale; matches `Section` titles); gap is the Column default `theme.gap_standard` (= s3 = 12, identical) |
| F3-11 | QrWidget | Square code anchored at the rect's top-left inside a non-square allocation (white surround piled on one side); error text literal 12 px / 8 px inset. | any | `qr.rs:211,215,239-240` | — (no fixture consumer captured) | **fixed** — code centred in the allocation; `scale.sm` (renderer-relative) and `spacing.s2` |
| F3-12 | Card/ContentCard/CardGrid/Actions | Header/body/footer gutters agree on all four sides (`padding_standard`), footer buttons share one bottom edge, trailing alignment pixel-exact, wrap row gap = `gap_standard`; natural min-height propagation intact. | wide 1080, narrow 340, 2× | `card_composition.rs:126-134` | `target/ui-review/ui/card_composition/before/{wide,narrow,large-text}.png`; Store `before/dark/*.png` | no defect |
| F3-13 | FormPage/Form/Section/Field | Label line centred on the first control (`top_inset`), help/error left edge = control edge, field gap s6/s3, section gap s8/s6, page gutter s6, readable 720. | wide 1080, narrow 340, 2× | `form_composition.rs`, `page.rs` | `target/ui-review/ui/form_composition/before/*.png` | no defect |
| F3-14 | EmptyState | Measured styles via `renderer_input_style`; centring by measured block. | — | `empty_state.rs:240-320` | gallery `Empty State` card | no defect (accepted 2026-09-16 work retained) |
| F3-15 | Table (editable grid) | Header band and cells share the same TextInput inset, so header/row x-parity holds; `CELL_FONT = 15.0` is off the type scale, but the cells are `TextInput`s whose intrinsic 40 px height is tied to it. | 1× | `table.rs:73` | `target/ui-review/ui/table_shots/before/table-base.png` | **open — needs F2** (TextInput owns the cell metric) |
| F3-16 | DataTable / TreeView / SearchResultsView / ConflictPicker / ConflictBanner | Fixed structural heights (24/28, 22, 38/50, 160 px cards, 64 px chips) do not follow the text scale; at 2× the now correctly sized text fills or overruns them. Virtualised row heights flow through `SparseVirtualViewport` and public `with_viewport(height, row_height)` callers. | 2× | `data_table.rs:1680-1681`; `tree_view.rs:668`; `search_results.rs:36-38`; `conflict_picker.rs:43-44,56`; `conflict_banner.rs:34,40-41` | `A/data_table-2x.png`, `A/tree_view-2x.png`, `A/search_results-2x.png`, `A/conflict-2x.png` | **open — F3 follow-up** (structural; 1× metrics are test-pinned, scaling them is a viewport/hit-test change beyond an alignment pass) |
| F3-17 | Plain `Button` inside `Card::titled_with_action` | At 2× the button background stays 1× wide while its label doubles (`A/legacy_cards-2x.png`, "Add account"). `Actions` buttons (`with_action_sizing`) are correct. | 2× | `button.rs` default sizing | `B/legacy_cards-2x.png` | **open — needs F2** |
| F3-18 | Settings shots path | `apps/settings/src/shots.rs::render_to_path_with_text_scale` scales the theme but not `Button`/renderer-relative text, so its 2× captures do not match the live `App` (buttons stay 1×). | 2× | `apps/settings/src/shots.rs:548` | `target/ui-review/settings/before/large-text.png` | **open — needs A-unit (settings)**; note `QOS_SHOTS_PANEL=region` is not a valid slug (falls back to External Accounts) |
| F3-19 | StatTile value/delta stacking | Rows advance by font size (`size + 6`), so line boxes overlap by ~3 px; ink does not collide at 1× or 2× and `tile_height` is a pinned default. | 1×/2× | `stat_tile.rs:145,155` | `A/stat_tiles-2x.png` | accepted — documented, no visible defect |
| F3-20 | Gallery banners at 1× in `B/banners-1x.png` | Titles look invisible only because the first fixture rendered on a transparent background; `render_window_png` is now used. | — | fixture only | `B/banners-1x.png` vs `A/banners-1x.png` | capture artefact, not a defect |

| F3-21 | 32 other `ui` widget files | The same double text scaling (F3-01) is live wherever a widget passes `theme.tokens.typography.scale.*` to `draw_text`/`measure`: `activity_timeline, autocomplete, badge, breadcrumb, capture_studio, card (legacy non-Title paths none left), channel_browser, chip, color_picker, command_palette, date_picker, file_picker, floating_window, gauge, history_scrubber, input_hint, link_popover, modal_dialog, modal_frame, native_view, permission_prompt, popover, screenshot_markup, segmented, shortcut_recorder, slash_menu, tabs, tile_host, time_picker, toast, tooltip, window_group` (grep: file uses `typography.scale.` and a size-based renderer call without `renderer_input_style`). The size-only helper `renderer_type_size` currently lives in `data_table.rs`; the right home is `TextRenderer` next to `renderer_input_style` (F1 owns `text_render.rs`). | 2× | see list | — | **open — needs F1 (renderer helper) + F2/F4/F5/F6 (call sites)** |

Not changed on purpose: `DEFAULT_CARD_MIN_WIDTH` 260 / `DEFAULT_CARD_HEIGHT` 160 (authored
public defaults, no token equivalent), `Banner::content_height`, `StatTile::tile_height`,
DataTable/TreeView/SearchResults row heights, `ConflictPicker` card geometry, `Table` gutters
(used by hit-testing without a theme). `ContentCard::with_title` keeps the `Heading` role
chosen in Change 5 even though `Section` and (now) `Card::titled` use `Title`; worth a
follow-up decision, not an alignment defect.

## AFTER comparison

Rebuilt `store`, `settings`, `notes` and the three shot examples on the fixed tree and re-ran
every capture with the `after` tag (62 app PNGs + the fixture set).

- Unchanged, as intended: Store desktop/laptop/tablet/phone dark+light, `card_composition/*`,
  `form_composition/*`, `table_shots/*`, Notes — the Card/CardGrid/Actions/Form defaults were
  not touched.
- Changed, as intended: Settings card titles (`Card::titled*`) are now `Title`-role headings —
  16 px semibold at 1× (`settings/after/dark/dev.qos.settings-desktop.png`) and 32 px at 2×
  (`settings/after/large-text.png`, previously stuck at 15 px); `data_table/after/*` header and
  rows centred with the partially visible last row clipped by the body layer;
  `search_showcase/after/*` rows centred with 12 px subtitles; gallery `Metrics` tile icons on
  the caption line and the chart header on one centre line (`gallery_shots/after/gallery-dark.png`).
- Fixture set `f3_fixture/after/*` at 2×: every widget renders 2× text (was 4×); residual
  overflow is the fixed-band item F3-16.

## Code review outcome

`/code-review high` on the working tree returned nine findings. Acted on:

- DataTable default cell text centred on the *clipped* cell rect could slide while a tall
  row scrolls in → `draw_rows` now paints inside the same body clip layer as mounted cells and
  hands `render_cell`/`text_render` the whole-row rect; centring uses the true row height.
- StatTile delta arrow was a 12 px square pinned to the line-box top → centred on the delta
  text's measured line, like the icon on the caption.
- `dominant()` in the fixture used `HashMap` iteration to break ties → deterministic tie-break.
- `legacy_cards_render_at_both_scales` asserted only a non-zero height → now asserts the Title
  role line box, the `gap_standard` title/body gap and the 1×→2× title ratio via inspection.

Recorded, not changed here:

- The fixed structural heights (F3-16) and the renderer-level home for `renderer_type_size`
  (F3-21) — both cross-unit; the clamp in `centered_text_top` is the correct local behaviour
  until bands grow with the text.
- StatTile rows still step by theme font size because `tile_height` is a pinned default;
  measured stepping would need the tile to grow (F3-19).
- Pre-existing literals in rewritten lines with no token equivalent: StatTile `6.0` row gap
  and `2.0` arrow gap (both inside `tile_height`'s reservation), Chart legend dot radius `3.0`.
- `"Loading…"` in DataTable/TreeView placeholder rows is a pre-existing literal; `ui/locales`
  has no `loading` key and the locale files are outside this unit (needs F1/i18n).
- The fixture pins private row/header constants as copies; DataTable/TreeView rows are not
  inspection nodes, so there is no geometry to read them from without adding test hooks.

## Tests

All runs used `RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."
RUSTC_WRAPPER=sccache CARGO_BUILD_JOBS=1 cargo … --locked --no-default-features -p ui`.

| Command | Result |
|---|---|
| `cargo test --test it f3_composition_fixtures` (BEFORE, structural asserts only) | 9 passed (captures `f3_fixture/before/`) |
| `cargo test --test it f3_composition_fixtures` (AFTER, alignment + scale asserts) | 9 passed, 0 failed (captures `f3_fixture/after/`) |
| `cargo test --lib -- widget::card:: widget::card_composition:: widget::form_composition:: widget::form:: widget::composition widget::empty_state:: widget::table:: widget::data_table:: widget::tree_view:: widget::virtual_list:: widget::icon_grid:: widget::banner:: widget::stat_tile:: widget::chart:: widget::search_results:: widget::conflict_banner:: widget::conflict_picker:: widget::qr:: widget::image:: app::card_composition_tests app::form_composition_tests` | 246 passed, 0 failed, 0 ignored |
| `cargo test --test it f3_composition_fixtures` (after the review fixes) | 9 passed, 0 failed |
| `cargo test --lib -- widget::data_table:: widget::stat_tile:: widget::card:: widget::card_composition::` (after the review fixes) | 73 passed, 0 failed, 0 ignored |
| `cargo check -p ui --tests` | clean, no warnings |
| `cargo clippy -p ui --lib --tests` (run by the review) | clean |

New tests: `ui/tests/f3_composition_fixtures.rs` (9: DataTable, TreeView, SearchResultsView,
Banner, StatTile, Chart, IconGrid, ConflictBanner/Picker, legacy Card — each asserting 1×
centring on painted ink and the 1×→2× ink ratio) and
`card::tests::titled_card_title_is_a_title_role_heading_that_follows_the_text_scale`.
Updated: `card::tests::titled_card_keeps_title_gap_after_intrinsic_column_gap_fix` now compares
against `Theme::dark().gap_standard` (same value, token origin).

No pre-existing failures were observed in the scoped runs.

## Limits

- Captures are dark theme at device scale 1 (plus light for the app responsive matrix); the
  fixture asserts on the dark theme's luminance contrast, so a light-theme regression in these
  widgets would need a second pass of the same assertions.
- The 2× assertions prove single scaling and containment where the band allows; they cannot
  prove good layout at 2× because the bands themselves do not scale (F3-16).
- `ui-shots --scenario conflict_picker` is an interactive `App::run()` scenario with no
  headless output; the conflict surfaces were covered by the new fixture instead.
- Store/Settings/Notes app captures exercise Card/CardGrid/Actions/Form/legacy Card; none of
  the sampled apps mounts a DataTable, TreeView or SearchResultsView, so app-level evidence for
  F3-01 is the fixture, not an app screenshot.
- `QOS_SHOTS_PANEL=region` does not select the Settings Region panel (not a slug); the
  "region" captures show External Accounts.
