# A11 — imager: 1× dark + light polish

Branch `ux1x/a11-imager`, cut from `ux1x/unit0` at `fd1f84391`. Files edited: `apps/imager/src/{app,customize,lib,main}.rs`,
`apps/imager/src/setup/{mod,view,import_view}.rs` and this report. Evidence under
`target/ux1x/a11/{before,after}/{dark,light}/` (29 scenario PNGs + 4 responsive-matrix PNGs per theme per phase,
`geometry/*.inspection.json` + `geometry/audit.jsonl` per theme per phase). The unit was interrupted by a machine
reboot after a first code review; the captures that review saw are kept as `target/ux1x/a11/after-checkpoint/`, and
`after/` is the re-capture of the committed tree (the "Resume" section below lists what changed between the two).
Every PNG named below was opened with the Read tool and is described in one line.

## Renderer check

`--shots` and `--responsive-matrix` go through `ui::launch::shot_theme` (`main.rs::render_shot`); `--shots-dir`
uses `standalone_shot_theme`, which reads `QOS_SHOTS_THEME` the same way. `QOS_SHOTS_THEME=light` produced light
frames for all 29 scenarios before any change (`before/light/*.png`); nothing to fix in the renderer.

One capture gap was real: `--headless-drive --scenario NAME` drove the blank live root, because the imager declared
a `shot_handler` but no `shot_root`. `main.rs` now declares `shot_root` (the same `app_root_for_shot` the
screenshot renders), and `open_state` — the opener `--demo` already seeds through — answers a `--scenario` with an
in-memory `State::new()` posed by `imager::seed_scenario` (the same call `render_scenario` makes for `--shots`) and
`Autosave::disabled`, so a geometry dump describes the screenshot and can never write sample content into the user's
persisted store for the live app to load (the first review caught that hazard in the seed-into-the-store version).
**Dependency:** the launcher hands `--headless-drive` through `open_state_with_args` since `ux1x/integration`; this
base's launcher only does so for apps with a history definition and otherwise drives the fixture on `State::new()`
via `headless::run_with_theme`, so at `fd1f84391` a wizard geometry dump runs over an *unposed* document: the local
drive inventory, releases and posed step come from `seed_local_shot_state` and are present, the document-borne
selected-source card, recent rows and Confirm / Done values are not. The checkpoint worked around that by seeding
inside `sync` (a `stage_shot_seed` hook, since removed). The dumps under `after/*/geometry/` were taken at this base;
their control and pair counts are identical to the checkpoint's sync-seeded dumps for all 29 scenarios in both themes,
because those document-borne blocks are painted text and rows the accessibility tree does not expose as controls
(`a11y_source_controls` maps the browse zone and the release rows; the recent rows, the selected card and the review
values have no nodes), so the lint verdicts below are the same under either opener — but they cover the controls,
not those painted blocks, whose spacing is evidenced by the PNGs and the `app.rs` layout tests. `drive-internal`
had no posing arm and rendered the Source step; it now poses the Drive step with nothing selected (the drive card at
rest).

## Inventory

| Surface | Painter | Rest edge before | Rest edge after |
|---|---|---|---|
| Step rail pills, discs, connectors | app (`draw_header`) | current: accent ring; others none (selector) | unchanged (glass-selector idle = none); disc + label on the measured centre line |
| Footer rule | app | `border_standard` on `surfaces.base` | `edge_on(surfaces.base)` |
| Back / Rescan / Preview | app → `ui::widget::Button::paint` (`ButtonFace` Neutral) | `neutral[2]` fill + `border_standard` (invisible dark, edgeless light) | the toolkit's neutral face (`face_colors`): `raised` / `bg_hover`, ink resolved against both; its rest edge is whatever `face_colors` returns — `border_standard` at this base, `edge_on(raised)` once F1 (`ux1x/integration`) is merged (`Open — needs coordinator` below) |
| Primary disabled | app → `Button::paint` (`.disabled(true)`) | `neutral[3]` fill, `neutral[6]` ink | the toolkit's disabled face: `raised` + `on_surface_at(..).ink_dim`; rest edge as above |
| Primary / destructive enabled | app → `Button::paint` (`.primary()` / `.danger()`) | accent / danger fill, literal white ink | the toolkit's Primary / Danger faces: `accent_primary` / `semantic.danger`, ink via `ink_on_any` / `ink_on`, hover shifts the fill away from the ink |
| Drop zone | app | accent-tinted fill, dashed accent (visible) | unchanged colours; measured stack, token insets |
| Selected-source card | app | `raised` + success 1.4 stroke (visible) | unchanged edge; measured bands |
| Release rows | app | `raised` + `border_standard` | `ui::surface::card(raised)`; selected keeps accent ring |
| Recent rows | app | none (bare list rows) | none (TreeView/VirtualList rule); hover `bg_hover` |
| "No releases" / "Insert a drive" zero cards | app | `raised` + dashed `border_standard` | `raised` + dashed `edge_on(raised)` |
| Drive card | app | `raised` + `border_standard` | `ui::surface::card(raised)`; selected/danger rings unchanged |
| Confirm review card, Done summary card, plain-ISO card | app | `raised` + `border_standard` | `ui::surface::card(raised)` |
| Confirm field, Customize text fields | app (plain-chrome `TextInput` inside) | `neutral[2]` fill + `border_standard` / semantic strokes | `raised` + `edge_on(raised)` (focus: `bg_active` + `border_focus` 2 px); the confirm gate keeps its danger/success stroke |
| Kind badges (`qOS`, `ISO`, `USB`) | app → `ui::widget::Badge::paint_at` | `radii.sm` rectangles, 11–12 pt Bold, literal white ink | the toolkit badge: `radii.full` pill, `scale.xs` label, ink `ink_on_any(tint)`; as wide as the label needs, on the row's badge band |
| Chips (`REMOVABLE`, `SYSTEM DISK`), glyph discs | app | semantic tint at alpha 40 + semantic ink; literal white glyphs | unchanged tint chip; glyph ink `ink_on(fill, NON_TEXT_MIN_CONTRAST)` |
| Progress ring track, phase-bar tracks, connectors | app | `neutral[3]` on `surfaces.base` (allowed) | unchanged; bar gets `edge_on(track)` via `card` |
| Setup / import pages: `Card`, `Button`, `TextInput`, `Dropdown`, `Scrollable`, `ModalDialog` | shared widgets | `border_standard` on `bg_primary` (container.rs:310) | unchanged — `Open — needs F2 / F1 / F3` below |

## BEFORE — one line per PNG (1280×800, 1×)

Wizard (`app.rs`, `customize.rs`), both themes unless noted:

- `source`: release rows `raised`+`border_standard` — edge invisible in dark (rule 2); block gaps 16 / 24 / 10 / 12 / 8 for
  sibling relationships (rule 3); "installable" tag on a hand offset (rule 4).
- `source-empty`: same row edges; disabled Continue is a grey `neutral[3]` pill (rule 2 policy: rest ≠ disabled face).
- `drive`: card lines `/dev/rsdb · 30.8 GB` (12.5 pt at +72) and `⚠ Erases …` (11.5 pt at +88) touch — gap 0 px
  (rule 1); model → device line 1 px; Rescan and Back `neutral[2]` faces with no visible edge (rule 2, both themes;
  light shows a grey pill with no edge); heading → card 9 px vs Source's 12 px (rule 3).
- `drive-empty`: zero card dashed `border_standard` barely visible in dark (rule 2); card top at +60 while the list
  starts at +52 (rule 3).
- `drive-internal`: rendered the Source step (posing bug), so the rest drive card was never captured.
- `customize`: caption → field 3 px (rule 1 / 3, label ↔ control is `s1` = 4); field boxes `neutral[2]` +
  `border_standard`: invisible edge in dark, edgeless grey box in light next to white edged Dropdowns (rule 2);
  field → next caption 23 px, section → label 26 px, export 30 px (rule 3); export label at a +8 offset (rule 4).
- `customize-timezone-open`: as `customize`; the open list is the shared Dropdown (F1).
- `export`: shared ModalDialog over the same customize defects (F2 for the dialog edge).
- `confirm`: review card `border_standard` invisible in dark (rule 2); caption → field 3 px (rule 1); card → banner 16,
  banner → caption 16+17, field → note 8 (rule 3); rows on fixed 34 px pitch, field 46 px.
- `progress`: bars inset 40, label → bar 20, bar → label 18, foot +22 (rule 3); ring stack on hand offsets (rule 4).
- `done` / `done-failed`: summary card `border_standard` invisible in dark (rule 2); headline → sub 0 px by line box
  (rule 1); card inset 80, rows 30 px pitch vs Confirm's 34 (rule 3).

Setup / import (`setup/view.rs`, `setup/import_view.rs`), both themes:

- `setup-experience*`: header title → description 12 px = same as card ↔ card (rule 3); shared Card and Button edges
  near-invisible in dark (F2/F1).
- `setup-folders-*`: mode cards title ↔ explanation 4 px vs 12 px in every `Card::titled` (rule 3); "Suggested
  folders" heading 12 px from "Help me choose", the same as sibling cards (rule 3, needs `s6`); page padding 24 / 18.
- `setup-review`, `setup-desktop-installed`: title → first card 12 px (rule 3); F2 edges.
- `import-first-login*`: per-folder `Card::new` nested inside the continuation card — box in box (rule 2, HIG §7);
  page padding 24 / 18 and a trailing empty `Row` (rule 3).
- `import-progress`, `-retry-progress`, `-partial`, `-conflict`, `-complete`, `-issue-pages`: "Imports" heading 12 px
  from the description (rule 3); `Details` / `Continue copy` stacked as one-button lines.
- Responsive matrix (phone / tablet / laptop / desktop, Source step): same row-edge and gap findings; no clipping.

## Fixes

Rhythm model (`app.rs::rhythm`, `customize.rs`, `setup/{view,import_view}.rs`): every gap is a `Spacing::BASE` step
named by relationship — `LABEL` s1 (caption ↔ field, lines of one block), `RELATED` s2 (title ↔ explanation,
caption ↔ list, help under a field), `SIBLING` s3 (rows, cards, card ↔ banner, footer actions), `SECTION` s4 (card /
row inset, compact gutter), `GROUP` s6 (next group, page gutter), `REGION` s8 (rail ↔ body, ring ↔ bars), `HERO` s16.
Control heights are `Metrics::BASE` fields (`panel_row_height` 44 footer buttons and confirm field,
`min_pointer_target` 32 step stations / Rescan / Preview, `bar_height` 40 form fields, `icon_lg` 24 glyph box).
Radii are `theme.radius_standard` (surfaces) and `theme.tokens.radii.sm` (badges).

Measured bands (`Measure` trait over `TextRenderer`, with an `Estimate` for the unmeasured first frame):
`SourceMetrics` (drop zone, selected card, release / recent rows, captions, zero card), `DriveMetrics` (Rescan
width, badge band, model / device / erase line tops, card, zero card), `ConfirmMetrics` (key column, per-row tops
from the wrapped values, banner, caption, note), `WriteMetrics`, `DoneMetrics`, `CustomizePanel::CzAnchors`
(heading band, caption line box, Preview width). Every step's heading is measured (`step_heading` now covers
Drive, Customize and Write); the a11y heading / description bounds follow it.

Rule 1: drive card lines `SECTION` / badge / `RELATED` / model / `LABEL` / device / `LABEL` / erase / `SECTION`;
captions `LABEL` above their fields; Done headline ↔ sub `RELATED`; every list block starts at the previous block's
bottom plus its step.

Rule 2: all app-drawn card / row / control faces go through `ui::surface::card` (`edge_on(fill)`), zero cards
through a dashed `edge_on`, the footer rule through `edge_on(surfaces.base)`; no `neutral[]` fill remains on a
raised ground (the three left — ring track, bar track, connector — sit on `surfaces.base`); literal white ink is
resolved with `ink_on` / `ink_on_any` (text) and the non-text bar for glyph discs; disabled primary follows
`Button::face_colors`. Setup / import: the nested per-folder card became a block separated by `GROUP`.

Rule 3: one step per relationship on every screen (pair tables below); setup / import pages are `page(header)`
+ sections (`GROUP` between groups, `SIBLING` inside, `RELATED` title ↔ explanation), gutter `s6` on all sides,
scrollbar gutter `s4`, footer inset `s1`.

Rule 4: text tops come from `line_top` (measured line box centred on the control / row centre) — station labels
and numbers, Back / primary / Rescan / Preview labels, footer hint, badge and chip labels, "installable" and
"✓ armed" tags, Done card rows, ring percentage + caption stack.

Rule 5: no fixed pixel offset survives in the layout or the painters except dimensions (content width cap 900,
station width 132, footer button widths, ring radius / stroke, glyph body 54, the status card's 56 px cap).

### Resume — the first review's findings, applied

The review that ran before the reboot found six things in the checkpoint diff; all are in the committed tree:

- (a) The `sync`-time scenario seed (`shot_seed` / `stage_shot_seed`) is gone; `main.rs::open_state` answers
  `args.scenario` with a posed in-memory document and disabled persistence (dependency recorded under "Renderer
  check").
- (b) `Estimate::line` is `ui::text_metrics::line_height_px(size, 1.0)` — the toolkit's 1.25 × line, what
  `TextRenderer::measure` returns — instead of a private `size × 1.3`; `line_top` / `txt_center_v` / `txt_right_v`
  take the band (`y0`, `h`) and centre through `ui::text_metrics::centered_text_top`, so every line box is centred in
  its container (row, badge, disc, footer band) rather than on a caller-computed midpoint.
- (c) One `pub(crate) mod rhythm` in `lib.rs` (`LABEL … HERO` as `f64`, and `rhythm::px::{RELATED, SIBLING, GROUP,
  PAGE_GUTTER, SCROLLBAR_GUTTER, FOOTER_INSET}` as `f32` for taffy) replaces the four copies in `app.rs`,
  `customize.rs`, `setup/view.rs`, `setup/import_view.rs`; `stack()` / `page()` live once in `setup/mod.rs`.
- (d) Back, the primary / destructive action, Rescan and Preview are `ui::widget::Button::paint` faces
  (`ButtonFace::new(rect, label).radius(theme.radius_standard)` + `.primary()` / `.danger()` / `.disabled()` /
  `.hovered()` / `.text_size()` / `.weight()`); `draw_primary`'s hand-derived faces and `control_face` are deleted
  (the remaining `row_face` is the hover face of list rows and drive cards, painted through `ui::surface::card`).
  The three kind badges go through `ui::widget::Badge::paint_at` (`ImagerApp::kind_badge`). The keyboard focus ring
  is still the wizard's single inflated stroke over the focused hit; the Preview button's ring is `ButtonFace::focused`.
- (e) `measure_done` measures the success copy (`done_ok_copy`, shared with the painter) wrapped to `content_w`
  in the wide or compact form; the step headings return `Option<&'static str>` descriptions (`write_heading`,
  plain-ISO `customize_heading` → `None`); `WriteMetrics` is `pct_h` / `meta_h` / `small_h`; `DriveMetrics` lost its
  dead `max`; `a11y_group` uses `AccessBounds::to_rect`; the `ImagerApp::card` pass-through is gone — callers use
  `ui::surface::card` directly.
- (f) The six bands are one `Bands { source, drive, confirm, write, done, plain_card_h }` (`PartialEq`), measured by
  `measure_bands` from the renderer or the 1× `Estimate`; `adjust_layout_with_measure` compares and assigns the
  struct once, and `plain_card_h` is estimated in `new()` instead of starting at 0.

A second `code-review` (medium) over the resumed diff found eight things; disposition:

1. `open_state` seeding `--scenario` into the persisted store would have left sample content for the live app —
   **fixed**: the fixture is an in-memory `State::new()` with `Autosave::disabled` (above).
2. The report still described the removed `stage_shot_seed` hook and over-read the lint's coverage — **fixed** in
   "Renderer check" and "Lint" (the dumps cover the controls; the painted blocks are evidenced by PNGs and tests).
3. The a11y `step-description` paragraph collapsed to a 0.25 px band on steps whose painted heading has no
   description (Write, plain-ISO Customize, Done) once descriptions became `Option` — **fixed**: its band is at least
   one line of description type (`line_height_px(13.0, text_scale)`) below the title, so AT and the lint always get a
   real target.
4. `operation_card` had moved the action row ahead of the expanded issue list, putting "Next issue page" and the copy
   action before the list they act on in reading / Tab order — **fixed**: the details precede the row again;
   `import_view::tests::expanded_details_precede_the_operation_commands` pins summary → details → toggle → paging →
   copy by walking the widget tree.
5. `customize.rs` stacked its fields below `panel_width_sheet` (560, unscaled) while the wizard went compact below
   520 × text scale — **fixed**: one `crate::compact_content(width, text_scale)` decides for both (the panel caches
   the renderer's text scale in its anchors); `COMPACT_BELOW` is gone.
6. Three rewritten tests had become transcriptions of the production formula — **fixed**: the compact-station test
   floors on `Metrics::BASE.min_pointer_target` (the HIG token, not `CRUMB_H`); the Source test pins the 1× bands the
   PNG shows (drop zone 91.625, release row 52.5, recent row 50), checks every measured band equals its `Estimate`
   (the renderer / `line_height_px` agreement finding (b) relies on) and floors rows at the pointer target; the
   customize test adds token-anchored checks (field = `bar_height`, Preview = `min_pointer_target`, caption band +
   `s1` above every field, ≥ `s4` between rows) beside the formula ones.
7. `draw_drive_card_details` wrapped the model / device / erase lines to the column while `DriveMetrics` measures
   single lines, so a long `lsblk` model would overprint the device line — **fixed**: each line is elided
   (`elide_to_width_weighted`) to the column and drawn unwrapped, as the source card's name already was.
8. `a11y_group`'s children-union bounds duplicate a fold the toolkit should own (`group_row.rs` carries the same one)
   — **recorded** below as the Unit 0 item, with the suggested `AccessNode::with_children_bounded`.

## AFTER — one line per PNG

- `source` (dark, light): rows carry a visible hairline in dark and a grey edge in light; drop zone → card 12,
  card → caption 24, caption → rows 8, rows 12, → RECENT 24, recent rows 12; `qOS` kind badges are the toolkit pill on
  the badge band in the selected card and in every release row; Continue is the toolkit primary face. `source-empty`:
  the disabled Continue is the toolkit disabled face (raised, dim ink; edge as the coordinator item) and the footer
  hint is centred on the footer band.
- `drive` (dark, light): card 121 px with model / device / erase lines 4 px apart; Rescan and Back are the
  toolkit's neutral faces (dark: raised face, the declared rest edge is faint — coordinator item; light: grey edge);
  "1 removable" on the Rescan band; `USB` is the toolkit badge pill on the badge band. `drive-internal`: the same
  card at rest, edged. `drive-empty`: dashed derived edge, glyph / title / lines on measured bands.
- `customize`, `customize-timezone-open` (dark, light): field boxes match the shared Dropdowns (raised + hairline
  in dark, white + grey edge in light); caption 4 px above each field; IDENTITY → REGION 24; Preview is the 32 px
  toolkit neutral face with its label on the measured line.
- `export`: unchanged modal over the fixed form.
- `confirm` (dark, light): review card edged; rows on measured tops; banner 12 below the card; caption 24 below the
  banner, field 4 below it, note 8 below the field; "Erase & write" is the toolkit Danger face (opaque danger fill,
  ink resolved against it).
- `progress` (dark, light): ring stack centred; meta 12 under the ring; bars 32 in from the column, label → bar 4,
  bar → label 12, foot 24; "Cancel write" (no job in a fixture) is the toolkit disabled face.
- `done`, `done-failed` (dark, light): headline 24 under the disc, sub 8 under the headline, card 24 under the sub,
  edged, rows on one pitch; glyph ink resolved per fill.
- `setup-*` (dark, light): title → description 8, header → cards 24, cards 12, "Suggested folders" 24 after
  "Help me choose", mode-card title ↔ explanation 8, page gutter 24 all round.
- `import-*` (dark, light): groups 24 apart, no nested card, one action row per operation card (the row precedes the
  expanded details so "Hide details" stays in reach), bottom gutter 24.
- Responsive matrix (both themes): the Source rhythm holds at 390 / 768 / 1366 / 1920; compact stations 32 px.

## Surface table

| Surface | Before dark | Before light | After dark | After light | Defect fixed |
|---|---|---|---|---|---|
| Release row | `before/dark/source.png` faint | `before/light/source.png` | `after/dark/source.png` hairline | `after/light/source.png` | rule 2, 3 |
| Drive card | `before/dark/drive.png` touching lines | `before/light/drive.png` | `after/dark/drive.png` | `after/light/drive.png` | rule 1, 2, 3 |
| Drive card at rest | not captured (posing bug) | — | `after/dark/drive-internal.png` | `after/light/drive-internal.png` | rule 2 |
| Zero cards | `before/dark/drive-empty.png`, `source-empty` | light | `after/dark/drive-empty.png` | `after/light/drive-empty.png` | rule 2, 3 |
| Rescan / Back / Preview / primary | `before/dark/drive.png`, `customize.png` | light: edgeless grey pills | `after/dark/drive.png`, `customize.png` (toolkit `Button::paint` faces) | light | rule 2 (dark rest edge: coordinator item), 4 |
| Kind badges | `before/dark/source.png`, `drive.png` (rectangles, white ink) | light | `after/dark/source.png`, `drive.png` (toolkit `Badge` pills) | light | rule 2, 4 |
| Customize fields | `before/dark/customize.png` | `before/light/customize.png` grey vs white dropdowns | `after/dark/customize.png` | `after/light/customize.png` | rule 1, 2, 3 |
| Confirm card / field | `before/dark/confirm.png` | light | `after/dark/confirm.png` | `after/light/confirm.png` | rule 1, 2, 3 |
| Done card / hero | `before/dark/done.png`, `done-failed.png` | light | `after/dark/done.png`, `done-failed.png` | light | rule 1, 2, 3 |
| Progress | `before/dark/progress.png` | light | `after/dark/progress.png` | light | rule 3, 4 |
| Setup pages | `before/dark/setup-*.png` | light | `after/dark/setup-*.png` | light | rule 3 (edges: F1/F2) |
| Import pages | `before/dark/import-*.png` | light | `after/dark/import-*.png` | light | rule 2 (box in box), 3 |

## Pair tables (1280×800, 1×; before from the fd1f84391 constants, after from `after/dark/geometry/*.inspection.json` and the rhythm constants)

Source (`source.png`):

| A | B | axis | before → after | step |
|---|---|---|---|---|
| station | station | x | 14 → 16 | `CRUMB_GAP` s4 (connector inset 4 each side) |
| rail | heading | y | 30 → 32 | `REGION` s8 |
| title | description | y | 2 → 8 | `RELATED` s2 |
| description | drop zone | y | 12 → 12 | `heading_gap` s3 |
| drop zone | selected card | y | 16 → 12 | `SIBLING` s3 |
| selected card | "OR CHOOSE" caption | y | 24 → 24 | `GROUP` s6 |
| caption | first row | y | 8 → 8 | `RELATED` s2 |
| release row | release row | y | 10 → 12 | `SIBLING` s3 |
| last row | "RECENT" caption | y | 22 → 24 | `GROUP` s6 |
| recent row | recent row | y | 8 → 12 | `SIBLING` s3 |
| row title line | row detail line | y | 3 → 4 | `LABEL` s1 |
| body | footer rule | y | 10 → 12 | `SIBLING` s3 |
| Back | primary (compact) | x | 12 → 12 | `SIBLING` s3 |

Drive (`drive.png`, `drive-empty.png`):

| A | B | axis | before → after | step |
|---|---|---|---|---|
| description | first card | y | 9 → 12 | `heading_gap` s3 |
| card badge row | model line | y | 8 → 8 | `RELATED` s2 |
| model line | device line | y | 1 → 4 | `LABEL` s1 |
| device line | erase line | y | **0 → 4** | `LABEL` s1 |
| erase line | card bottom | y | 9 → 16 | `SECTION` s4 |
| card | card | y | 12 → 12 | `SIBLING` s3 |
| "N removable" | Rescan | x | 12 → 12 | `SIBLING` s3 (now on one centre line) |
| Rescan (compact) | first card | y | 12 → 12 | `SIBLING` s3 |
| zero card top | description | y | 20 → 12 | `heading_gap` s3 |
| glyph | title / title | line 1 / line 1 | line 2 | y | 20 / 8 / 10 → 24 / 8 / 8 | `GROUP` / `RELATED` / `RELATED` |

Customize (`customize.png`, from `CzGeom`):

| A | B | axis | before → after | step |
|---|---|---|---|---|
| section caption | field caption | y | 9 → 8 | `RELATED` s2 |
| field caption | field | y | **3 → 4** | `LABEL` s1 |
| field | next field caption | y | 23 → 16 | `SECTION` s4 |
| left column | right column | x | 16 → 16 | `SECTION` s4 |
| last identity field | REGION caption | y | 26 → 24 | `GROUP` s6 |
| keyboard field | Preview | y | 30 → 24 | `GROUP` s6 |
| field height / Preview height | — | — | 38 / 32 → 40 / 32 | `bar_height` / `min_pointer_target` |

Confirm (`confirm.png`):

| A | B | axis | before → after | step |
|---|---|---|---|---|
| card padding | first row | y | 20 → 16 | `SECTION` s4 |
| row | row | y | 34 pitch (16 gap) → line + 12 | `SIBLING` s3 |
| key column | value column | x | 96 fixed → widest key + 24 | `GROUP` s6 |
| card | banner | y | 16 → 12 | `SIBLING` s3 |
| banner | caption | y | 16 → 24 | `GROUP` s6 |
| caption | field | y | **3 → 4** | `LABEL` s1 |
| field | note | y | 8 → 8 | `RELATED` s2 |
| field height | — | — | 46 → 44 | `panel_row_height` |

Write / Done (`progress.png`, `done.png`):

| A | B | axis | before → after | step |
|---|---|---|---|---|
| title | ring | y | 44 → 32 | `REGION` s8 |
| ring | rate line | y | 20 → 12 | `SIBLING` s3 |
| rate line | first bar label | y | 36 → 24 | `GROUP` s6 |
| bar label | bar | y | 5 → 4 | `LABEL` s1 |
| bar | next label | y | 18 → 12 | `SIBLING` s3 |
| bars | source → target line | y | 22 → 24 | `GROUP` s6 |
| bars | content edge | x | 40 → 32 | `REGION` s8 |
| hero disc | headline | y | 20 → 24 | `GROUP` s6 |
| headline | sub | y | **0 → 8** (line boxes) | `RELATED` s2 |
| sub | card | y | 22 → 24 | `GROUP` s6 |
| card row | card row | y | 30 pitch → line + 12 | `SIBLING` s3 |
| card | content edge | x | 80 → 64 | `HERO` s16 |

Setup / import (`setup-folders-copy.png`, `import-first-login.png`; `Card`/`Button` rects from the inspection):

| A | B | axis | before → after | step |
|---|---|---|---|---|
| page edge | content | x / y | 24 / 18 → 24 / 24 | `PAGE_GUTTER` s6 |
| title | description | y | 12 → 8 | `RELATED` s2 |
| header | first card / section | y | 12 → 24 | `GROUP` s6 |
| card | card (one section) | y | 12 → 12 | `SIBLING` s3 |
| "Help me choose" | "Suggested folders" | y | 12 → 24 | `GROUP` s6 |
| mode card title | explanation | y | 4 → 8 | `RELATED` s2 |
| "Add folder…" | "Selected folders" | y | 12 → 24 | `GROUP` s6 |
| continuation card | copy card | y | 12 → 24 | `GROUP` s6 |
| saved folder block | next block | y | 12 (nested cards) → 24 | `GROUP` s6 |
| status card | page bottom | y | 18 + empty row → 24 | `PAGE_GUTTER` s6 |

## Six-rule checklist

1. Nothing touches — pair tables above, every pair ≥ 4 px; `geometry_audit` reports 0 `touching_peers` and 0
   `semantic_bounds_overlap_unresolved` on all 29 scenarios in dark and light (`after/*/geometry/audit.jsonl`);
   `app.rs::source_blocks_follow_the_text_scale_without_overlapping` asserts ≥ `LABEL` between Source blocks at 1×,
   `customize.rs::captions_fields_and_sections_keep_their_rhythm` asserts the caption / field / section steps.
2. Edges derived — `app.rs` and `customize.rs` no longer reference `border_standard`; all app-painted card / row
   faces go through `ui::surface::card` / `edge_on`, buttons through `Button::paint`. The remaining `neutral[..]`
   uses are fills, not edges: the ring track, the bar track and the rail connector (`neutral[3]`) and the not-yet-
   reached station disc (`neutral[4]`), all on `surfaces.base`; and the `kind_color` tints of the non-semantic drive
   kinds (`Hdd` → `neutral[5]`, `Other` → `neutral[4]`), badge fills whose label ink `Badge::paint_at` resolves
   against them.
3. One rhythm — `rhythm` module (`app.rs`), `S.s*` constants in `customize.rs`, `view.rs`, `import_view.rs`.
4. Centred content — `line_top` / `txt_center_v` / `txt_right_v` in `app.rs` centre a measured line box in its
   band through `ui::text_metrics::centered_text_top`; badge labels are centred by `Badge::paint_at`, chip labels on
   their band; Back / primary / Rescan / Preview labels are centred by `Button::paint`.
5. No authored offsets — every gap is a `Spacing::BASE` step, every control height a `Metrics::BASE` field.
6. Evidence — 58 + 8 BEFORE and 58 + 8 AFTER PNGs opened; described above.

## Lint

`lint.sh after` (`lint-resume.out`, `after/*/geometry/audit.jsonl`): 29 scenarios × dark + light, all `exit=0`,
0 `touching_peers`, 0 `semantic_bounds_overlap_unresolved`. Remaining `accessible_label` failures (setup-folders-*,
setup-review, setup-desktop-installed, import-first-login-waiting, import-issue-pages) name
`…/Column[0]/Scrollable[0]`: the shared `Scrollable` is focusable without a name — `Open — needs F3`. BEFORE lint
measured 0 controls everywhere: the base binary had no `shot_root`, so the drive hit the blank live root, and
`geometry_audit` does not descend into semantic `Group`s without bounds (the imager's groups now carry their
children's union bounds via `ImagerApp::a11y_group`). `export` still measures 0 controls: the detached `ModalDialog`
does not project bounded semantics — `Open — needs F2`. The dumps ran over the unposed document this base's
launcher opens (see "Renderer check"); every scenario's control and pair count equals the checkpoint's sync-seeded
dump (`after-checkpoint/*/geometry/audit.jsonl`).

## Ratchets

`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit` (`target/ux1x/a11/ratchet-resume.log`, the
committed tree; `ratchet2.log` is the checkpoint's identical result): 7 passed, 2 failed, neither on a `grew` line of
this unit.

- `design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow` — only the pre-existing growth in
  `apps/browser/src/browser_tabs.rs` (`literal_color` 1, `white_or_black` 1) and `shell/src/shell/frame.rs`
  (`literal_color` 1, `white_or_black` 2); no imager line.
- `f1_spacing_token_audit::spacing_literals_do_not_grow` — stale entry to be lowered by the coordinator (pasted):
  `apps/imager/src/app.rs [pixel_const]: now 8, baseline still 10 — lower it to 8`. The first run also reported
  `apps/imager/src/customize.rs [pixel_const]: 1, baseline 0` for the new `COMPACT_BELOW = 520.0`; it is now the
  token expression `Metrics::BASE.panel_width_sheet` (560: a form narrower than a sheet stacks its fields), so the
  file is back at 0 (`ratchet2.log`).
- `declared_borders_in_chrome_do_not_grow`, `theme_escape_hatches_do_not_grow`, `no_hardcoded_style_literals_in_tree`
  and the pattern self-checks pass.

## Tests

- `cargo test -p imager --lib` on the committed tree — 453 passed, 0 failed, 3 ignored (the three pre-existing
  `requires root / actual NTFS image` ignores in `setup::migration*`), 189 s (`target/ux1x/a11/test-lib-resume.log`;
  the checkpoint's `test-lib.log` had the same counts). Includes the rewritten
  `app::tests::{step_content_starts_below_the_measured_heading_group, source_blocks_follow_the_text_scale_without_overlapping,
  confirm_field_follows_the_danger_banner, compact_layout_keeps_every_step_station_inside_the_viewport}` and the new
  `customize::tests::captions_fields_and_sections_keep_their_rhythm`.
- `cargo test -p imager --lib -- app::tests customize::tests setup::view::tests setup::import_view::tests main`
  after the resume edits — 81 passed, 0 failed, 1 ignored (`test-ui-modules-resume.log`; the `main` filter also
  matched a few `setup::migration*` / `store` tests by substring); the same set after the review fixes —
  `test-ui-modules-review.log` (counts below).
- `cargo check -p imager --tests` — exit 0, no warnings in the edited files (`check7.log`; the only imager warnings
  are the pre-existing `setup/connection/store*` ones present at the base, `check5.log`).
- `cargo build -p imager` — exit 0 (`build-resume.log`), the binary every `after/` capture and geometry dump used.
- `cargo fmt -p imager` ran last (`fmt-check.log` lists the 27 sites; 13 of them pre-date this unit in `app.rs`),
  followed by `cargo check -p imager --tests` — exit 0 (`check8.log`). The pass is formatting-only, so the captures
  and test runs above describe the committed code.
- `apps/imager` has no `tests/` directory; the setup / import suites live in the lib (`setup::view::tests` 20,
  `setup::import_view::tests` 14, all passing).

## Open — needs another unit

- F2 `ui/src/widget/container.rs:310/449` — `Card` (`Container::with_background`) strokes `border_standard` on
  `bg_primary`: every setup / import card edge is invisible in dark (`after/dark/setup-experience.png`,
  `after/dark/import-first-login.png`). Also the `ModalDialog` edge in `export.png` and its missing bounded
  semantics (`export` measures 0 controls).
- F1 `ui/src/widget/button.rs:520/538`, `dropdown.rs:774/875` — Neutral / disabled Button and Dropdown field rest
  edges are declared: `after/dark/setup-folders-copy.png` (Choose / Help me choose), `after/dark/customize.png`
  (Time zone / Language / Keyboard fields).
- F3 `ui/src/widget/scrollable.rs` — focusable `Scrollable` without an accessible name (lint `accessible_label`
  on every setup / import scenario with a scrollbar).
- Unit 0 / `ui::a11y` + `scripts/ux-bench/usability.py::geometry_audit` — the lint does not descend into projected
  semantic nodes without bounds, so any app whose a11y groups are unbounded lints as 0 controls; `ImagerApp::a11y_group`
  works around it by giving groups their children's union bounds (also correct for AT), and `group_row.rs::
  collect_accessibility` carries the same fold. One owner is wanted: `AccessNode::with_children_bounded` (or the
  inspection serialiser deriving group bounds, or the lint descending with the parent rect), after which
  `a11y_group` collapses to that call.
- `Open — needs coordinator (closed on ux1x/integration)`: the wizard's Back, primary / destructive, Rescan and
  Preview faces are `Button::paint`, so their rest edge is `face_colors`' — `border_standard` at `fd1f84391`
  (faint on `raised` in dark: `after/dark/drive.png` Rescan / Back, `after/dark/customize.png` Preview,
  `after/dark/source-empty.png` disabled Continue), `edge_on(raised)` once F1 is merged. Likewise
  `--headless-drive` posing the document through `open_state` (`ux1x/integration` launcher), and F2's `Card` /
  `ModalDialog` edges and F3's `Scrollable` name on the setup / import pages.

## Limits

- The wizard's footer buttons, Rescan and Preview are painted by the toolkit (`Button::paint`) over the wizard's
  own hit rects (the immediate-mode form HIG §8 provides for canvas hosts); the confirmation field box is still a
  hand-painted face (`raised` + `edge_on`, `bg_active` + `border_focus` while editing) around the shared plain-chrome
  `TextInput` — replacing it with the retained widget is a larger refactor of the hit layout and out of scope.
- `Badge::paint_at` paints kind badges at the theme's `scale.xs` on a `12 pt + s2` band measured by the layout; the
  band is taller than the pill's label needs, which keeps the badge, the chip beside it and the row's first line on
  one band at every text scale.
- Button widths (120 / 170 / 190 / 250) and the 900 px content cap are authored dimensions, not offsets, and were
  kept.
- Glyph ink on the success disc resolves to dark on both themes (white is 2.4:1 on that green); this is the
  toolkit's measured answer, not a stylistic choice.
- Compact-width PNGs come only from the responsive matrix's phone shot of the Source step; the other steps' compact
  layouts are covered by `app.rs` layout tests, not screenshots.
- The customize panel now stacks its fields below `panel_width_sheet` (560) while the wizard chrome goes compact
  below 520 × text scale, as before; the two agree at every captured viewport (content widths 358, 720, 900), and
  the last rebuild after that one-line change was not re-captured because no captured width falls in 520–560.
- Coordinator completion: the resumed worker went silent after its last verification pass (transcript idle from
  18:16, no process left running). The coordinator verified the checkpoint tree as it stood — `cargo check -p imager
  --tests` clean apart from two pre-existing dead-code warnings in the Linux-only native module, `cargo test -p imager
  --lib` 454 passed / 0 failed / 3 ignored — and committed it unchanged. Ratchet `stale` lines for `apps/imager` are
  recorded in the batch README with the coordinator's re-baseline.
