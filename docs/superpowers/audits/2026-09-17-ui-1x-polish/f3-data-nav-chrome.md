# Unit F3 — Data display, navigation shells, app chrome bars (1× polish)

Branch `ux1x/f3-data-nav-chrome`, cut from Unit 0 `fd1f84391`. Owned files:
`ui/src/widget/{table,data_table,tree_view,virtual_list,sidebar,page_sidebar,nav_shell,nav_split,app_shell,resizable_split,resize_handle,scrollable,scrollbar,tabs,breadcrumb,toolbar,icon_grid,stat_tile,chart,gauge,progress_bar,badge,avatar,block_editor,rich_text_editor,document_view,native_view}.rs`,
`ui/src/chrome/{toolbar,status_bar,page}.rs`, the owned examples and tests.

Evidence lives under `target/ux1x/f3/{before,after}/` (BEFORE captured at `fd1f84391`,
AFTER at this branch's tip), every PNG opened with the Read tool; pixel numbers below
come from `target/ux1x/f3/png_probe.py` (a dependency-free PNG reader — no PIL on
this machine) and are relative luminance (`L`) with the step against the ground (`ΔL`).
The HIG §9.5 floor for a hairline to register is `HAIRLINE_MIN_DELTA` = 0.02.

## Six-rule checklist

| Rule | Status | Evidence |
|---|---|---|
| 1 No two elements touch or overlap | met for every owned surface in the gallery harness | `after/harness/lint-{dark,light}.json`: 0 `touching_peers`, 0 overlap (BEFORE: Bold↔Italic and Link↔Code touching in both themes). Toolbar rects from `after/harness/gallery-dark.inspection.json` in the pair table. |
| 2 Every surface has a visible derived edge or contrast step | met; 0 `border_standard` / `border_strong` / `*_white_*` / `neutral[]` on a raised or overlay ground remain in owned files | Surface table below; `grep` of owned files finds only `tabs.rs` test-only literals and `avatar.rs` identity ink (see Limits). |
| 3 One spacing rhythm per panel | met where F3 owns the rhythm | Toolbar siblings `s3`, group boundary `s3 + 1 px rule + s3`; Table/DataTable rows separated by a derived rule, zebra as a contrast step; scrollbar tiers shared by Scrollable, DataTable, VirtualList, PageSidebar. |
| 4 Content centred on measured line boxes | unchanged, already met (`centered_text_top`, `measure_weighted`) in DataTable header/body, TreeView rows, StatusBar, Toolbar, PageSidebar | No geometry regressions in the AFTER PNGs; Toolbar labels remain centred in their (now edged) faces. |
| 5 No authored-pixel exemption | Toolbar's contiguous `cursor += extent` (a fixed 0 gap) replaced by `spacing.s3`; separator's private `s2` margins removed; Table rules snapped to pixel centres; 1× is not byte-identical and was not meant to be | Diff of `ui/src/widget/toolbar.rs`, `table.rs`. |
| 6 Visual evidence, dark and light | all touched surfaces captured 1× dark+light and opened | `after/gallery-{dark,light}.png`, `after/harness/gallery-{dark,light}.png`, `after/{dark,light}/{table,data_table,nav_shell}/*.png`, `after/scrollbar/*.png`, `after/rte/*.png`. BEFORE light did not exist for table/data_table/nav_shell (dark-only examples — fixed, see Examples). |

## Surface table (before → after, dark and light)

| Surface (file:line at base) | Defect | Before | After | Dark evidence | Light evidence |
|---|---|---|---|---|---|
| Toolbar widget buttons (`widget/toolbar.rs` `place`, `draw_button`) | faces `surfaces.raised` on a raised card — invisible; rects contiguous (rule 1) | bare labels, gap 0 | `surface::card(raised)` face with derived edge; `s3` gap | `after/gallery-dark.png` Navigation & Toolbar card: four edged chips | `after/gallery-light.png` same |
| Toolbar widget separator (`:673`) | `border_standard` | invisible in dark | `chrome_hairline` | same PNG, 1 px rule between Italic and Link | same |
| Table grid + header rule (`table.rs:678/705`) | `border_white_10` rules; `border_standard` 1.5 px header rule; every rule centred on an integer coordinate (two half-alpha columns) | header rule ΔL 0.007 (invisible); grid rule ΔL 0.013 smeared over 2 px | `edge_on(raised)` rules snapped to pixel centres inside the body; header rule 1 px at `GLOW_ALPHA` | `after/dark/table/table-base.png`: header rule (102,103,106) ΔL 0.097 at y=92, one row; grid rule (64,65,69) ΔL 0.029, one column (x=52, x=204) | `after/light/table/table-base.png`: header rule (174) ΔL 0.36; grey rules and band on white body |
| Table header band / zebra (`table.rs:1193/1195`) | `surface_white_10` / `surface_white_5` (white veneer on a light table) | light: white on white | `edge_on_with(raised, HAIRLINE_ALPHA)` / `ZEBRA_ALPHA` (13) — flips polarity | header band lighter than body | header band darker than body |
| Table empty-state box (`table.rs:627`) | `border_standard` | — | `edge_on(box fill)` | (not captured: no empty scenario in the example) | — |
| DataTable header rule (`data_table.rs:1304`) | `border_standard` | ΔL 0.011 | `edge_on(raised)` at `y1 − 0.5` | `after/dark/data_table/data_table-top.png` col x=300 y=27: (37,38,44) → (54,55,59), ΔL 0.011 → 0.024 | `after/light/data_table/data_table-top.png` |
| DataTable zebra (`:1402`) | `neutral[5]` ramp slot — glaring band on dark | (91,92,95) L 0.107 on 0.0086 | `edge_on_with(base, ZEBRA_ALPHA)` | (34,35,39) ΔL 0.008 — a stripe, not a wall | subtle grey stripes |
| DataTable selected row (`:1394`) | accent primary fill (accent = live, §9.4) | accent wash, `on_accent` text | `data_table::selection_fill` = `selection_tier(active).fill` (`lift(1.0)`), no ring — rows stack, a ring would double up between neighbours (§7); on a near-black ground (high-contrast, OLED) the lift collapses (§9.3 caveat) so the row takes `hairline_on(base, 110)`, the HC preset's own middle tier (4.1:1, white text 5.1:1); text stays `text_primary` | pinned by `tier_tests::selected_row_*` (dark ΔL 0.032, light 0.105, HC 0.156); no selected-row scenario in an owned example | — |
| DataTable scrollbar (`:1218/1226`) | `surface_white_5` track, `border_standard` thumb | — | `scrollbar::{track_color, thumb_color}` | shared with Scrollable below | — |
| TreeView selection (`tree_view.rs:2274/2375`) | `accent.with_alpha_factor(0.3)` wash | accent wash | `paint_selected_row` (selection tier, faded by transition opacity); focus ring unchanged | not in an owned example — see Limits | — |
| VirtualList selection / scrollbar (`virtual_list.rs:1244/1285`) | accent wash; `border_standard` thumb | — | selection tier; shared scrollbar tiers | not in an owned example — see Limits | — |
| Scrollable overlay bar (`scrollable.rs:770/798`) | `surface_white_5` track; `border_standard` idle thumb | dark idle thumb (43,44,51) ΔL 0.017 < 0.02 | `thumb_color`: `hairline_on(base, 72)` idle, `text_dim` active; `track_color`: `hairline_on(base, 13)` | `after/scrollbar/scrollbar-dark-idle.png` col x=456: (88,89,91) ΔL 0.091 | `after/scrollbar/scrollbar-light-idle.png`: (174,174,173) ΔL 0.47 |
| PageSidebar rail edge (`page_sidebar.rs:656`) | `border_standard` at `x1` | — | `edge_on(raised)` at `x1 − 0.5` | not in an owned example — see Limits | — |
| PageSidebar add button (`:857`) | `border_standard`; accent hover wash | — | `surface::card(base)`; hover `bg_hover`, label `text_primary` | — | — |
| PageSidebar page menu (`:890/897`) | raised + `border_standard`; accent hover wash | — | Panel role: `panel_ground` + `panel_border` (inset outline); row hover `selection_tier(hover)` | — | — |
| PageSidebar scrollbar (`:766/773`) | private `text_primary` α 0.07/0.35 | — | shared `scrollbar::{track_color, thumb_color}` | — | — |
| NavShell back/collapse chip (`nav_shell.rs:973`) | `bg_primary` + `border_standard` | faint edge | `surface::card(bg_primary)` | `after/dark/nav_shell/nav_shell-wide.png` top-left chip edged | `after/light/nav_shell/nav_shell-wide.png` |
| NavShell pane dividers (`:1577/1586`) | `border_standard` on integer x (two half-alpha columns) | (38,39,45)/(41,42,49) ΔL 0.012/0.009 | sidebar divider `edge_on(bg_primary)` (the `fill_panel` surface it sits on), overview divider `edge_on(surfaces.base)`, both at `x1 − 0.5` | row y=400: sidebar (55,56,61) ΔL 0.025, overview (74,75,80) ΔL 0.056, one column each | (209,209,207) ΔL 0.26 |
| AppShell sidebar (`app_shell.rs:576/578`) | `neutral[0]` ground; `border_standard` divider | — | `surfaces.raised` (§9.2) + `edge_on(raised)` at `x1 − 0.5`; narrow back band `edge_on(base)` | not in an owned example — see Limits | — |
| ResizableSplit divider (`resizable_split.rs:407/600`) | `border_standard` 4 px band | — | `edge_on(surfaces.base)`; accent kept for live drag | — | — |
| Sidebar toggle pill (`sidebar.rs:516/518/525`) | `neutral[2]/[3]` faces, `border_standard` | — | idle `surfaces.raised`, hover `bg_hover`, `edge_on(bg)`; open keeps accent (toggle "on") | — | — |
| StatTile (`stat_tile.rs:117`) | `border_standard` on raised | dark (37,38,44) ΔL 0.005 over 2 px | `surface::card(raised)` | `after/gallery-dark.png` row y=870: (54,55,59) ΔL 0.024, 1 px | light (238) 2 px → (229) ΔL 0.22 crisp |
| RingGauge / BarGauge track (`gauge.rs:181/449`) | `border_standard` α150 | ring track barely visible in dark | `hairline_on(raised, 40)` | `after/gallery-dark.png` 72 % ring: grey track visible | unchanged look in light |
| Chart grid (`chart.rs:333`) | `border_standard` α90 | ΔL 0.0015–0.004 (invisible) | `hairline_on(raised, HAIRLINE_ALPHA)` | col x=300 y=1290: ΔL 0.024 | grid visible as before |
| App chrome Toolbar bar (`chrome/toolbar.rs:278/300`) | `border_standard` bottom rule and group separator | — | `chrome_hairline(theme)` (Bar role) | not in an owned example — see Limits | — |
| App chrome StatusBar (`chrome/status_bar.rs:54`) | `border_standard` top rule | — | `chrome_hairline(theme)` | — | — |
| RichTextEditor frame (`rich_text_editor.rs:868`) | `border_standard` on `bg_primary` | faint | `edge_on(bg_primary)`; focus ring unchanged | `after/rte/rte-dark.png` | `after/rte/rte-light.png` |
| BlockEditor divider / todo box (`block_editor.rs:3416/3546`) | `border_standard` | — | `edge_on(base)` rule; box outline `edge_on_with(base, PANEL_BORDER_ALPHA)` | — | — |
| NativeView placeholder (`native_view.rs:439`) | `border_standard` | — | `edge_on(base)` on `edge_outline` | — | — |
| TabBar drop indicator (`tabs.rs:2917`) | literal white 85 % — invisible on light | — | `hairline_on(base, α)` | not capturable without a drag; `pill_appearance` and geometry untouched | — |
| TabBar close-chip test mirror (`tabs.rs:1780`) | literal white 28 | — | `chip_hover_tint(theme, CHIP_HOVER_TINT_ALPHA)` — mirrors the draw path | — | — |

## Pair table (gaps in px, from `after/harness/gallery-dark.inspection.json`; light is identical)

| Surface (PNG) | Element A | Element B | Axis | Gap before → after | Relationship / step | OK |
|---|---|---|---|---|---|---|
| Toolbar (`harness/gallery-*.png`) | Bold | Italic | x | 0 → 12 (Bold x1 654.7, Italic x0 666.7; dark and light) | siblings / `s3` — **closes F4 Open (Toolbar Bold↔Italic, Link↔Code)** | yes |
| Toolbar | Italic | Link | x | 17 (1 + 2×`s2`) → 25 (`s3` + 1 + `s3`) | group boundary / `s3` each side of the rule | yes |
| Toolbar | Link | Code | x | 0 → 12 (Link x1 783.8, Code x0 795.8; dark and light) | siblings / `s3` — **closes F4 Open** | yes |
| Toolbar | button face | card body | y | 39 px face centred in the 39 px row (unchanged) | — | yes |
| Table (`*/table/table-base.png`) | header row | body row 1 | y | 0 → 0, separated by a 1 px rule at `GLOW_ALPHA` (ΔL 0.007 → 0.097 dark, 0.36 light) | rows / derived rule, §9.5 "Table rules" | yes |
| Table | row ↔ row | | y | 0 → 0, separated by a 1 px `edge_on(raised)` rule (2 × ΔL 0.013 smear → 1 × ΔL 0.029 dark) | rows / derived rule | yes |
| Table | cell ↔ cell | | x | 0 → 0, separated by a 1 px rule (same numbers, x=204) | cells / derived rule | yes |
| Table | body | outer frame ticks | x/y | 24 / 24 (`LEFT_GUTTER`/`TOP_GUTTER`, unchanged) | gutter | yes |
| DataTable (`*/data_table/*.png`) | header | body | y | rule at `y1 − 0.5`, ΔL 0.011 → 0.024 dark | header ↔ body / derived rule | yes |
| DataTable | row ↔ row | | y | zebra step ΔL 0.098 (glare) → 0.008 (stripe) | rows / contrast step | yes |
| NavShell (`*/nav_shell/nav_shell-wide.png`) | sidebar pane | overview pane | x | 1 px rule, two half-alpha columns (ΔL 0.012 / 0.009) → one crisp column on the sidebar's `bg_primary` fill: (55,56,61) ΔL 0.025 dark, (209) ΔL 0.26 light | rail ↔ content / `edge_on(bg_primary)` | yes |
| NavShell | overview pane | detail pane | x | same → (74,75,80) ΔL 0.056 dark | content ↔ content / `edge_on(surfaces.base)` | yes |
| Scrollable (`scrollbar/*-idle.png`) | thumb | viewport edge | x | 2 px inset (unchanged); thumb ΔL 0.017 → 0.091 dark | bar ↔ content | yes |
| StatTile (`gallery-*.png`) | tile | tile | x | 12 (`s3`, unchanged) | siblings / `s3` | yes |

Lint: `after/harness/lint-{dark,light}.json` — `failures` 10 → 8; the 8 remaining are
`accessible_label` findings (Scrollable, Checkbox, Slider, ToggleGroup, TextInput,
NumberInput, TabBar, Toolbar) which are not rule 1–4 findings; the TabBar/Toolbar two are
recorded under Limits. `touching_peers`: 2 → 0 in both themes; overlap 0 → 0.

## Rhythm list

- Toolbar widget: `s3` between siblings (`Row` default, HIG §7b "toolbar buttons"); a group
  boundary is `s3 + 1 px rule + s3` (the separator no longer carries its own `s2` margins,
  so the relationship is stated once); label padding `s3` inside each face (unchanged).
- Table / DataTable: rows and cells are separated by one derived rule; header band and zebra
  are contrast steps on the body surface (`HAIRLINE_ALPHA`, `ZEBRA_ALPHA`), never a second
  rule.
- Scrollbars: one tier pair for Scrollable, DataTable, VirtualList and PageSidebar
  (`scrollbar::{THUMB_IDLE_ALPHA, TRACK_ALPHA, thumb_color, track_color}`).
- Selection: one painter for DataTable, TreeView and VirtualList rows
  (`data_table::paint_selected_row` = `selection_tier(active)`).

## Examples

`table_shots`, `data_table` and `nav_shell_shots` were dark-only; they now read
`QOS_SHOTS_THEME` (`Theme::named`, default dark). `scrollbar_shots` and
`rich_text_editor_shots` already wrote both palettes; `QOS_SHOTS_THEME` now narrows them to
one when set. `target/ux1x/f3/capture.sh <phase>` is the capture sequence used for both
phases.

## Ratchet output

`cargo test -p ui --test it -- data_table_test scrollable_test scrollbar_render_test tabs_test icon_grid_test design_token_audit f1_spacing_token_audit`
→ 28 passed, 3 failed. No `grew` line anywhere; `f1_spacing_token_audit` passes (no new
pixel constants — the alpha steps added are `u8`, outside its regex). The three failures are
the expected ratchet outcomes, pasted verbatim for the coordinator's re-baseline
(`UPDATE_DESIGN_BASELINE=1`, `UPDATE_CHROME_BORDER_BASELINE=1`):

```
---- design_token_audit::theme_escape_hatches_do_not_grow ----
4 baseline entr(ies) in tests/design_token_baseline.txt are higher than the tree.
src/widget/data_table.rs [theme_blind_white]: now 0, baseline still 1 — lower it to 0
src/widget/scrollable.rs [theme_blind_white]: now 0, baseline still 1 — lower it to 0
src/widget/table.rs [theme_blind_white]: now 0, baseline still 3 — lower it to 0
src/widget/tabs.rs [literal_color]: now 1, baseline still 2 — lower it to 1

---- design_token_audit::declared_borders_in_chrome_do_not_grow ----
18 entr(ies) in tests/chrome_declared_border_baseline.txt are higher than the tree:
ui/src/chrome/status_bar.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/chrome/toolbar.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/app_shell.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/block_editor.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/chart.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/widget/data_table.rs [declared_border]: now 0, baseline still 3 — lower it to 0
ui/src/widget/gauge.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/native_view.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/widget/nav_shell.rs [declared_border]: now 0, baseline still 3 — lower it to 0
ui/src/widget/page_sidebar.rs [declared_border]: now 0, baseline still 3 — lower it to 0
ui/src/widget/resizable_split.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/rich_text_editor.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/widget/scrollable.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/widget/sidebar.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/widget/stat_tile.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/widget/table.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/toolbar.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/widget/virtual_list.rs [declared_border]: now 0, baseline still 1 — lower it to 0

---- design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow ----
apps/browser/src/browser_tabs.rs [literal_color] 1 / [white_or_black] 1, baseline 0
shell/src/shell/frame.rs [literal_color] 1 / [white_or_black] 2, baseline 0
```

The third failure is the pre-existing `browser_tabs.rs` / `frame.rs` one named in the brief;
neither file is F3's. Every owned file in `chrome_declared_border_baseline.txt` is now at 0
(28 → 0 declared borders across the 18 files).

## Tests

`cargo test -p ui --lib -- table data_table tree_view virtual_list sidebar nav_shell nav_split app_shell resizable_split scrollable scrollbar tabs breadcrumb toolbar icon_grid stat_tile chart gauge status_bar`
→ `test result: ok. 491 passed; 0 failed; 1 ignored` (the ignored test is pre-existing and
not F3's; the count includes the four new `widget::data_table::tier_tests`). First run had one
failure, `resize_reconciles_original_indices_and_meaningful_separators`, whose width formula
pinned the gap-less packing model; updated as described below and green.

Final `cargo test -p ui --test it -- data_table_test scrollable_test scrollbar_render_test
tabs_test icon_grid_test design_token_audit f1_spacing_token_audit` → `28 passed; 3 failed`:
the three ratchet outcomes pasted above (22 stale entries in owned files + the pre-existing
`browser_tabs.rs` / `frame.rs` growth). `f1_spacing_token_audit` is green: an intermediate
`NEAR_BLACK_TIER_GAIN: f32 = 3.0` matched its `pixel_const` regex (`data_table.rs` 6 → 7,
a `grew`) and was retyped as `u32 = 3` — an alpha multiplier, not a pixel value.

`cargo check -p ui --tests --examples` — clean apart from two pre-existing warnings in
`ui/src/record.rs` and `ui/src/animation_schedule.rs` (not owned).

Test updated with the new geometry: `widget::toolbar::lifecycle_tests::
resize_reconciles_original_indices_and_meaningful_separators` — its "width that fits the
first five items plus the chip" formula now includes the five sibling gaps the packing
model sums (four between items, one before the chip); the assertion on which items
overflow is unchanged.

## Code review (`/code-review high`) — findings and dispositions

| # | Finding | Disposition |
|---|---|---|
| 1, 2 | `chrome_declared_border_baseline.txt` (18 entries) and `design_token_baseline.txt` (4 entries) are stale-high after this diff | Expected; workers may not edit baselines. Pasted under Ratchet output for the coordinator's `UPDATE_*_BASELINE=1` pass. |
| 3 | Fixed-alpha `hairline_on`/`edge_on_with` tiers collapse on the pure-black high-contrast/OLED grounds (the HC preset re-solves its veneers to ~3× the dark tiers) | **Fixed** for every fill tier this unit paints: `data_table::tier_on(surface, alpha)` applies the preset's gain (×3, floor 51) when the ground's relative luminance is below 0.005 (sRGB compression near black; the dark preset's base is 0.0086 and is untouched). Used by Table header band/zebra, DataTable zebra, scrollbar thumb/track (all four owners), gauge tracks. `tier_tests::tier_on_is_plain_hairline_on_ordinary_grounds` pins dark/light byte-identical. *Edges* keep Unit 0's `edge_on`/`chrome_hairline`, whose near-black weakness is recorded below as Open. |
| 4 | Selected row via `lift(1.0)` is 1.39:1 on HC | **Fixed**: `selection_fill` takes the HC preset's middle tier on a near-black ground; `tier_tests::selected_row_on_a_black_ground_reads_as_a_control_and_carries_text` asserts ≥ 3:1 step and ≥ 4.5:1 text, `selected_row_fill_registers_on_every_preset` asserts the register floor on dark/light/HC. |
| 5 | `QOS_SHOTS_THEME=high-contrast` silently produced no PNGs (scrollbar/rte); unknown names silently fell back to dark (table/data_table/nav_shell) | **Fixed**: `shot_themes()` shoots the named theme (tag = the name) or both defaults; an unknown name exits 2 with a message; `shot_theme()` likewise exits 2 and now applies `QOS_SHOTS_TEXT_SCALE` through `ui::launch::shot_text_scale`. |
| 6 | VirtualList paints no ground; on a raised host its `lift(1.0)` selection equals the host on light | Latent (no production host places a VirtualList on `raised`); recorded under Limits. A fix needs the host's ground, which the widget does not know. |
| 7 | NavShell sidebar divider derived against `base` but stroked on `fill_panel`'s `bg_primary` | **Fixed**: sidebar divider `edge_on(theme.bg_primary)`, overview divider `edge_on(surfaces.base)` (the shell's ground under a content pane). Dark: (67,68,73) → (55,56,61), still ΔL 0.025; light unchanged. |
| 8 | `paint_selected_row` stroked the tier's hairline on every row → doubled rules inside a multi-selection | **Fixed**: rows take the tier's fill only (documented on the painter). |
| 9 | Table rule snapping assumed integer edges; fractional widths after a drag could land a rule outside the body | **Fixed**: `draw` rounds the painted `xs`/`ys`/`gr`/`gb` to whole pixels (hit-testing keeps exact edges; ≤ 0.5 px apart) and the snap is `±0.5` from those integers. |
| 10 | Three examples carry a private `shot_theme()` copy instead of `ui::launch::shot_theme(&LaunchArgs)` | `LaunchArgs` has private fields and no constructor, so examples cannot call it; the copies now mirror its theme + text-scale halves and fail loudly. Recorded as Open — needs Unit 0 (an arg-less `ui::launch::shot_theme_from_env()`). |

## Open — needs another unit

- **Unit 0 / `ui/src/surface.rs`, `ui/src/chrome/shell.rs`**: `edge_on` stops at alpha 40 on a
  pure-black ground (Δ 0.021, 1.4:1) and `chrome_hairline` is fixed at 26 (1.2:1) there, while
  `Theme::high_contrast` re-solves `border_standard` to white. Every unit's edges inherit this;
  F3's fill tiers work around it with `data_table::tier_on`, which belongs next to `edge_on` in
  `surface.rs` once Unit 0 can take it (with `NEAR_BLACK_*` constants).
- **Unit 0 / `ui/src/launch.rs`**: expose an arg-less `shot_theme_from_env()` so examples stop
  copying the theme/text-scale halves of `shot_theme(&LaunchArgs)`.
- Gallery harness `accessible_label` findings on Checkbox, Slider, ToggleGroup, TextInput,
  NumberInput → F1; on Scrollable[0] (the harness page) → coordinator/Unit 0
  (`ui/examples/gallery_harness_shots.rs`). Not rule 1–4; recorded from
  `after/harness/lint-dark.json`.
- `ui/examples/common/gallery_scene.rs` — no touching/overlap remained after the Toolbar fix,
  so the scene is unchanged; Button/Toggle/Chip/Card edges visible in it belong to F1/F2.
- **Closes F4's Open** (Toolbar Bold↔Italic, Link↔Code touching under
  `Scrollable[0]/Column[0]/Row[2]/Container[1]/…/Toolbar[0]`, `gallery_scene.rs:630`): the
  AFTER inspections report 12 px between Bold↔Italic and Link↔Code and 25 px across the group
  separator in dark and light, `touching_peers` empty in both (`after/harness/lint-{dark,light}.json`).

## Limits

- Surfaces without an owned capture path (PageSidebar, AppShell, ResizableSplit, Sidebar
  toggle, chrome Toolbar/StatusBar, BlockEditor, NativeView, TreeView/VirtualList selection,
  TabBar drop indicator): fixed per the §9.5 table with the same helpers as the captured
  surfaces; their colours are computed by the same `edge_on`/`selection_tier`/
  `chrome_hairline` calls whose output is measured above. `widget_snapshot` (F1) is the
  example that mounts TreeView/Toolbar and will show them once F1's light path lands.
- `avatar.rs::initial_ink` keeps its white / near-black ink literal: it is ink on a per-peer
  identity colour (§9.4's sanctioned exception), not a surface edge; the tests pin the
  values. `tabs.rs` close glyph stays white on the danger chip for the same reason.
- `sidebar.rs`, `nav_shell.rs`, `nav_split.rs` drawer scrims are literal black at low alpha
  (`literal_color` baseline entries, unchanged): a scrim darkens by design on every theme;
  moving them onto `surfaces.scrim`/`chrome::fill_scrim` is a follow-up, not a rule 1–4
  defect.
- `page_sidebar.rs` keeps the accent 2 px ring on the *current* page thumbnail: it is the
  page rail's only "which page am I on" signal and re-designing it as a lift is beyond a
  polish pass; recorded here rather than changed silently.
- `AppShell`'s sidebar moved from `neutral[0]` to `surfaces.raised` (§9.2). An app that
  painted raised rows inside that sidebar now has raised-on-raised rows; none of the owned
  examples does, and the divider still separates the panes.
- Toolbar hover/active faces in the gallery are not captured (no interaction in the shot);
  hover uses `bg_hover` with the same derived edge.
- VirtualList selection on a `raised` host (review finding 6): `lift(1.0)` equals `raised` on
  the light preset, so a VirtualList placed on a raised panel would paint its selected row in
  the panel colour. No owned example or production host does this today; the widget would
  need its host's ground to derive against.
- Zebra stripes stay a sub-threshold stripe by design (ΔL 0.008 dark); on a near-black ground
  `tier_on` lifts them to the HC preset's floor (white@51, ΔL 0.033) — Table rows are also
  separated by rules, DataTable rows by the stripe and header rule only.
- `edge_on`-derived rules (Table grid, chart grid, stat tile, dividers, `surface::card`) and
  `chrome_hairline` bars are 1.2–1.4:1 on the pure-black high-contrast/OLED grounds — Unit 0's
  helpers, listed under Open above; not re-solved locally so every unit's edges stay one
  vocabulary.
