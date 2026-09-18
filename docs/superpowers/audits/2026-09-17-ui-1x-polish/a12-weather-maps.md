# A12 — weather, maps: 1× dark + light polish

Base `fd1f84391` (Unit 0). Branch `ux1x/a12-weather-maps`. Edited only `apps/maps/src/**`,
`apps/weather/src/**` and this file. Evidence under
`target/ux1x/a12/<app>/{before,after}/{dark,light}/` (163 PNGs: every declared weather
scenario ×10 + default, every maps shots flag ×16 + `--panel` explore/directions/transit +
`--edit-from --panel transit`, plus the four-viewport `--responsive-matrix` per theme) and
`…/geom/*.inspection.json` for the touching lint, plus `weather/after/{dark,light}/scale/detail-{1.5,2.0}x.png`
for the scaled hourly strip. Every PNG listed below was opened with the Read tool.

## Six-rule checklist

| # | Rule | Evidence |
|---|---|---|
| 1 | No two elements touch or overlap | maps zoom +/− shared an edge (`touching_peers` gap `[-40, 0]` in `maps/before/{dark,light}/geom/default.inspection.json`) → `s2` apart, lint clean after (`maps/after/*/geom`: 21 pairs, 0 failures). Weather tablet 7-day row: precip dot 2.2 px off the glyph → ≥ `s3` (`day_row_geometry_tests`); hourly strip value↔now-dot 1.15 px and glyph↔precip line 2 px → `s1` on measured lines. Pair table below. |
| 2 | Every surface has a visible derived edge or contrast step | `apps/maps/src/draw.rs::card` now = shadow + `ui::surface::card` (edge derived from the fill); border argument deleted from all 17 callers. Weather `core.rs::card` = `ui::surface::card`; chips, refresh, collapse/expand chips through `Button::paint`; context menu on `chrome::panel_ground` + `chrome::panel_border`; all rules through `ui::surface::separator`. Zero `border_standard`/`border_strong`/`*_white_*`/`neutral[]` strokes left on a raised or overlay ground in either app (remaining `neutral[]`/`Color::WHITE` uses are the weather icon palette, the night-sky gradient and moon dot, the nav-card shadow tint, the map-marker cartography palette, `ink_on` tone inputs and the armed Delete zone's ink on a danger fill — none is an edge or a rest face). |
| 3 | One spacing rhythm per screen | maps `GAP` 10 → `Spacing::BASE.s3`; panel inset 10 → `s3`; controls column `s2` pair / `s3` siblings / `s4` margin (`column_height()`); POI card, Explore panel, offline card, departures and step list restated as `s1`/`s2`/`s3`/`s4` (tables below). Weather gutters `PAD`/`RP` 28/20 → `s6`/`s6`; search block `s4`; rail `s3`; 7-day row columns `s2`/`s3`/`s4`. Tests: `controls_keep_token_gaps_and_never_touch`, `floating_chrome_keeps_one_sibling_gap`, `explore_rows_keep_a_related_row_gap`, `day_row_columns_keep_token_gaps_at_every_width`, `tablet_width_keeps_precipitation_clear_of_the_glyph`. |
| 4 | Content centred in its container on measured line boxes | maps place/POI/search/endpoint rows: name+subtitle block centred on the row, distance chip on the name line's centre; POI card close ✕ centred on the title line, rows advance by measured line + `s2`; offline card is the exact sum of its stack; weather refresh button centred on the city line; compact sunrise/sunset pairs stacked on measured lines from the card's `s3` inset (was 5.5 px past the card bottom); sun-arc label/value on measured lines. |
| 5 | No authored-pixel exemption | Every kept fixed offset is a `Spacing::BASE.sN`/`theme.tokens.spacing` expression, a measured line, or a named authored width on the geometry struct (`DayRowGeometry::{temp_w,precip_w,dot_r}`, `StripGeometry::{icon_edge,now_dot_r,…}`). `f1_spacing_token_audit` shows only `stale` (lower) counts for owned files, no `grew`. |
| 6 | Evidence is visual | before/after PNGs at 1× dark and light for every surface, opened and described below. |

## Surface table — maps (`apps/maps`)

All 1280×800; `before/<t>/<name>.png` → `after/<t>/<name>.png`, dark and light.

| Surface (PNG) | Defects before (rule) | After (dark) | After (light) |
|---|---|---|---|
| `default`, `panel-directions` — docked panel, search bar, controls, nav card | Panel/search/nav card: `border_standard` stroke invisible on dark `raised` (2). Zoom + and − share an edge (1). GAP 10 off the scale, panel inset 10, mixed 8/10/12/14/18 steps (3). Step distance `y + instr_h + 2` fixed (4). | Panel, search bar and nav card carry a visible derived hairline; + and − separated by 8 px; locate/traffic/⋯/‹/« are Neutral `ButtonFace`s with the same edge; step list rule derived and inset. | Same geometry; edges read as black-at-alpha on white, no regression (light `border_standard` already registered — now identical treatment both themes). |
| `search` — search dropdown | Dropdown card bare on dark; selected row accent wash `alpha(accent, 0.12)` (2, §9.3); rows pitched 46 with 2 px gap (1). | Card edged; selected row is `Colors::hairline_on(raised, 40)` (one ladder step above the hover hairline) + `edge_on(raised)` on `edge_outline` — a lift distinct from any hover wash, its edge judged against the panel since the wash is translucent; rows 42 px with `s1` between hit rects, name/address block centred. | Lift is a black-at-alpha step on the white card with its edge; row rhythm identical. |
| `pois`, `panel-explore` — POI card, POI rows, selected-place card | POI card bare edge; close ✕ 26×26 hit rect at `card.y0+12` with glyph at `y0+14` off-centre; rows 24 px fixed (2, 4). Selected-place card bare `surfaces.base` fill on raised; action buttons hand-painted with `border_standard` (2); Saved ✓ accent wash (§9.3). Section labels `+20`, trailing `+8`/`+14` (3). | POI card edged, ✕ centred on title line, rows on measured lines + `s2`, Directions = primary `ButtonFace`. Place card `edged` base tier; three `ButtonFace` actions `s3` apart on the `s4` gutter; Saved ✓ = `selection_tier(active)` lift + derived edge; labels advance line+`s2`, sections `s4`. | Same; base-in-raised reads as a grey step on white with a visible edge. |
| `menu`, `attribution` — ⋯ button, overflow menu, info sheet | ⋯ button bare on dark; accent border when active (2). | ⋯ is a Neutral face; accent ring only while the menu is open/download runs (§9.4). Menu and sheet are toolkit (`ContextMenu`, `ModalDialog`) — F2. | Same. |
| `offline` — offline maps card | Card bare edge; zoom stepper, row trash, Clear all bare fills / `border_standard` (2); 20/22/24/30/34/42/44 px steps (3); card height `350 + list` not the stack (4). | Card edged; stepper/trash/Download/Clear are `ButtonFace` (Neutral / Primary / DangerOutline); saved-area rows `edged` base tier `s2` apart; progress track edged; height = exact stack (`s6` gutter, `s2` label→control, `s4` sections, `s3` between the two buttons). | Same. |
| `traffic` — legend card + `No traffic data` pill | Bare edge on dark (2). | Edged (`card` at alpha 0.96 fill). | Same. |
| `transit-arrive`, `panel-transit`, `edit-from-transit` — mode segment, Leave/Arrive, stepper, Reset, departures, endpoint dropdown | Mode/time tracks bare `surfaces.base`; journey cards bare; Reset chip `border_standard`; stepper bare (2); `+8/+10/+12/+14/+18/+26` steps (3); active segment `inflate(-3)` literal (5). | Tracks `edged`, active segment inset `s1`, ink via `ink_on`; journeys `edged` with accent ring when chosen, `s3` apart; Reset and ± are `ButtonFace`; departures footer rule derived; endpoint fields `edged` with 2 px focus ring; dropdown rows as search rows. | Same. |
| `floating-navigation{,-attached,-hover,-returned,-returning}` — nav card, End pill | Card `border_standard` stroke invisible on dark (2); End pill `neutral[3]` at 0.62 + `border_standard` (2); rule full-bleed `border_standard` (§9.5); 16 px literal gutter (5). | Card edge `edge_on(raised)`; End pill `edged` base tier at rest, danger wash on hover; rule `separator` inset to the `s4` gutter. | Same. |
| `empty`, `error`, `nav-closed` — first-run panel, `Loading map…`/offline pill, status pill | Pills bare on dark (2); empty-state copy at `+28/+46/+78` (3). | Pills edged; copy on measured lines, `s2` under the title, `s4` after. | Same. |
| `matrix/dev.qos.maps-{phone,tablet,laptop,desktop}` | Phone: menu button bare, zoom pair touching. | Menu button Neutral face (12 px to the search bar = `s3`), pair `s2`, panel `s3` under the search bar. | Same. |

## Surface table — weather (`apps/weather`)

| Surface (PNG) | Defects before (rule) | After (dark) | After (light) |
|---|---|---|---|
| `default`, `detail` — saved-place chips, search, hero, tiles, hourly, 7-day, AQ | Chips `border_standard` at 0.6 alpha (invisible on dark `raised`), hover `neutral[2]` (2); every card `border_standard` at 0.7 (2); refresh/collapse chips hand-painted (2); gutters 28/20, search block 16/22 (3); hourly value↔dot 1.15 px, glyph↔precip 2 px, and at ≥1.5× text the hour label ran into the precipitation line because the band came from `authored_h`, not the stack (1); refresh top-pinned `hy-2` (4). | Chips/cards/tiles via `ui::surface::card` — visible hairline; hover a derived wash; refresh/collapse/expand `ButtonFace`s; gutters `s6`; hourly strip stacked `s1` on measured lines by `chart::strip_rows`, and the card's height is that same stack (`chart::strip_height` + `s6` scrollbar zone), so the label clears the precipitation line at every scale; refresh centred on the city line; header/day/arc rules `separator`. | Edges black-at-alpha; nothing regressed (the light `#DEDEDB` edge is now the same derived treatment). |
| `scrolled` — 7-day + AQ | Day-row rules `border_standard` at 0.5; temp track `neutral[2]` (2); hi-temp column a fixed 30 px (a three-digit or negative reading ran past it) (1). | Rules `separator`; range track and hourly scrollbar track `Colors::hairline_on(raised, 56)` (`track_fill` — two ladder steps above the hover hairline, black over the white card and white over the dark one; `glass_film` was tried first and vanished white-on-white on light); temperature column measured from the widest lo/hi label this frame. | Same. |
| `collapsed` — icon rail | Expand chip hand-painted, separator `border_standard` at 0.6 full-bleed, hover `neutral[2]` (2, §9.5). | `ButtonFace` chip, `separator` inset `s3`, derived hover wash; `s3` rhythm. | Same. |
| `drag` — reorder ghost, insertion caret, Delete zone | Chips as above. | Chips edged; ghost carries its edge. (Lint `semantic_bounds_overlap_unresolved` San Francisco/Tokyo etc. is the dragged ghost's bounds overlapping the reflowed list — the scenario's intent, not a rest defect.) | Same. |
| `search`, `search_loading`, `search_empty` — Autocomplete overlay | Toolkit `Autocomplete` (F1/F2). | Unchanged. | Unchanged. |
| `offline`, `error` — status line under the region | Status line drawn at `sub_y + sub_h` with no gap; header rule ran through it (1). | Third line `s1` under the region on its measured box; header grows by that line so the rule sits below. | Same. |
| `onboarding` — empty overview + zero state | Search field toolkit; no owned raised surfaces. | Unchanged. | Unchanged. |
| `matrix/dev.qos.weather-tablet` (768) — compact hero fallback, 7-day columns | Sunrise/Sunset stack at `hero.y1 − 60/46/26/12`: last line box 5.5 px past the card (4); precip dot 2.2 px from the glyph (1); label↔value pairs 1.5 px apart. | Pairs stacked from the `s3` inset with `s1` inside a pair, `s2` between; precip clear of the glyph by ≥ `s3` (kept at tablet width by narrowing the label/temperature columns to their measured widths). | Same. |
| `matrix/dev.qos.weather-phone` (390) — overview only | Chips as above. | Edged. | Edged. |

## Pair table (gap px before → after, 1×; from `Layout::new`/`DayRowGeometry`/the inspection JSON)

| Surface | Element A | Element B | Axis | Before → after | Relationship / step |
|---|---|---|---|---|---|
| maps controls | Zoom in | Zoom out | y | 0 → 8 | related pair `s2` |
| maps controls | Zoom out | Locate | y | 10 → 12 | siblings `s3` |
| maps controls | Locate | Traffic | y | 10 → 12 | siblings `s3` |
| maps controls | window edge | Zoom in | y/x | 16 → 16 | margin `s4` |
| maps chrome | search bar | panel | y | 10 → 12 | siblings `s3` |
| maps chrome | panel | map controls column (docked) | x | 10 → 12 | siblings `s3` |
| maps chrome (phone) | menu button | search bar | x | 10 → 12 | siblings `s3` |
| maps panel | panel edge | header / content | — | 10 → 12 | inset `s3` |
| maps panel | header | content | y | 10 → 12 | `s3` |
| maps panel | back ‹ | title | x | 10 → 8 | icon ↔ label `s2` |
| maps explore | place row | place row (hit rects) | y | 8 → 8 | related rows `s2` (asserted) |
| maps explore | section label | first row | y | 20 → line(11)+8 = 21.75 | label ↔ rows `s2` |
| maps explore | selected-place card | RESULTS label | y | 14 → 16 | sections `s4` |
| maps explore | action button | action button | x | 4 → 12 | siblings `s3` |
| maps explore | card edge | name / buttons | x | 16/8 → 16 | gutter `s4` |
| maps directions | Use my location | mode track | y | 14 → 16 | sections `s4` |
| maps directions | mode track | time row | y | 12 → 12 | `s3` |
| maps directions | time row | next group | y | 18 → 16 | `s4` |
| maps directions | Leave/Arrive track | − stepper | x | 8 → 8 | `s2` |
| maps directions | active segment | track edge | inset | 3 → 4 | `s1` |
| maps directions | summary | rule | y | 0 → 0 (rule) then 14 → 16 | `s4` after rule |
| maps directions | instruction line | distance line | y | 2 → 0 (stacked line boxes) | measured lines |
| maps departures | journey card | journey card | y | 10 → 12 | siblings `s3` |
| maps departures | leg detail row | leg detail row | y | 2 → 4 | `s1` |
| maps search/endpoint dropdown | field | dropdown card | y | 6 → 8 | `s2` |
| maps search/endpoint dropdown | row | row (hit rects) | y | 2 → 4 | `s1` |
| maps search/endpoint dropdown | card edge | row | x/y | 6 → 8 | pad `s2` |
| maps endpoint fields | origin | destination | y | 8 → 8 | `s2` |
| maps POI card | title | category line | y | 26−line(18)=3.5 → 4 | `s1` |
| maps POI card | detail row | detail row | y | 24−15=9 → 8 | `s2` |
| maps POI card | last row | Directions | y | 6 → 12 | `s3` |
| maps POI card | card edge | content | — | 16/14 → 16 | gutter `s4` |
| maps offline | card edge | content | — | 22/20 → 24 | gutter `s6` |
| maps offline | label | field | y | 22−16.25=5.75 → 8 | `s2` |
| maps offline | section | section | y | 30/24 → 16 | `s4` |
| maps offline | area row | area row | y | 8 → 8 | `s2` |
| maps offline | Download | Clear all | x | 10 → 12 | `s3` |
| maps nav card | card edge | Navigation / rule / summary | x | 16 → 16 (= `s4`) | gutter |
| weather overview | pane edge | search field / chips | x | 20 → 24 | gutter `s6` |
| weather overview | top | search field | y | 16 → 16 | `s4` |
| weather overview | search field | collapse chip | x | 10 → 12 | `s3` |
| weather overview | search field | SAVED PLACES | y | 16 → 16 | `s4` |
| weather overview | SAVED PLACES | first chip | y | 22 → 21 (line(10)+8) | `s2` |
| weather overview | chip | chip | y | 8 → 8 | `s2` |
| weather rail | expand chip | separator | y | 12 → 12 | `s3` |
| weather rail | separator | first slot | y | 12 → 13 | `s3` |
| weather detail | pane edge | header / cards | x | 28 → 24 | gutter `s6` |
| weather detail | region line | status line (offline/error) | y | 0 → 4 | `s1` |
| weather hero (arc) | horizon rule | Sunrise label | y | 6 → 8 | `s2` |
| weather hero (arc) | Sunrise label | 06:12 | y | 1.5 → 4 | `s1` |
| weather hero (compact) | 20:47 line box | card bottom | y | −5.5 → 12 | inset `s3` |
| weather hero (compact) | Sunrise pair | Sunset pair | y | 1.5 → 8 | `s2` |
| weather hourly | value line box | now-dot top | y | 1.15 → 4 | `s1` |
| weather hourly | now-dot bottom | glyph top | y | 3.5 → 4 | `s1` |
| weather hourly | glyph bottom | precip line | y | 2 → 4 | `s1` |
| weather hourly | precip line box | hour label box | y | 6.25 (1×) / −1.9 (1.5×) → 4 / 4 | `s1` (top-down stack; card grows) |
| weather hourly | precip dot | precip read-out | x | 4.9 (1×) / 1.1 (1.5×) → 4 / 4 | `s1` (centred cluster) |
| weather hourly | band | scrollbar track | y | 8 → 8 | `s2` (track `s2` tall, `s2` to the edge = `s6` zone) |
| weather hourly | card edge | band | x | 14 → 16 | gutter `s4` |
| weather 7-day | hi label (measured) | card gutter | x | 30−w (negative at 3 digits) → ≥ 0 | measured `temp_w` |
| weather 7-day (768) | condition glyph | precip dot | x | 2.2 → 14.2 | `s3` |
| weather 7-day | glyph | condition text | x | 10 → 8 | icon ↔ label `s2` |
| weather 7-day | lo label | bar | x | 10 → 8 | `s2` |
| weather 7-day | bar | hi label | x | 8 → 8 | `s2` |
| weather 7-day | card edge | row content | x | 18/16 → 16 | gutter `s4` |

## Rhythm per screen

- maps floating chrome: `s4` window margin, `s3` between every floating sibling, controls
  pair `s2`.
- maps panel: `s3` inset, `s3` header→content, `s1` under a title, `s2` related rows and
  label→rows, `s3` siblings, `s4` sections.
- maps POI card / offline card: `s4` / `s6` gutter, `s1` title→meta, `s2` rows and
  label→control, `s3` before the action / between the two actions, `s4` sections.
- weather overview + rail: `s6` gutter, `s4` search block, `s2` chips, `s3` rail.
- weather detail: `s6` gutter, `s3` between cards (unchanged), `s4` card gutter, `s2`
  value↔bar / icon↔label, `s3` between columns, `s1` inside a label/value pair.

## Ratchets (`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`)

```
apps/maps/src/app.rs [pixel_const]: now 8, baseline still 9 — lower it to 8
apps/maps/src/map_controls.rs [pixel_const]: now 1, baseline still 3 — lower it to 1
apps/weather/src/core.rs [pixel_const]: now 11, baseline still 13 — lower it to 11
```

No `grew` line for owned files. The pre-existing `app_and_shell_theme_escape_hatches_do_not_grow`
failure (`apps/browser/src/browser_tabs.rs`, `shell/src/shell/frame.rs`) is not this unit's.
`declared_borders_in_chrome_do_not_grow`: ok.

## Tests

- `cargo test -p maps --lib`: 187 passed, 0 failed, 1 ignored (pre-existing live-network test).
  New: `map_controls::tests::controls_keep_token_gaps_and_never_touch`,
  `app::tests::floating_chrome_keeps_one_sibling_gap`, `app::tests::explore_rows_keep_a_related_row_gap`.
- `cargo test -p weather --lib`: 90 passed, 0 failed. New: `day_row_geometry_tests::{day_row_columns_keep_token_gaps_at_every_width, tablet_width_keeps_precipitation_clear_of_the_glyph}` (now also at 38/42 px three-digit temperature columns), `chart::strip_row_tests::precipitation_and_hour_label_never_touch_at_any_scale` (1×, 1.25×, 1.5×, 2×).
  Updated: `collapsing_the_city_list_reclaims_forecast_width` (chip x for the `s6` gutter),
  `wheel_scrolls_the_forecast_body` (pointer moved off the hourly band, which owns the wheel for
  its own horizontal scroll and now sits where the old pointer landed).
- `cargo check -p maps -p weather --tests`: ok. `cargo build -p maps -p weather`: ok.
- Touching lint (`usability.geometry_audit`) on every `after/*/geom/*.inspection.json`:
  maps 21 peer pairs, 0 `touching_peers`, 0 overlap (was 1 touching pair per theme); weather
  0 failures on 10 of 11 scenarios; `drag` reports the dragged ghost overlapping the reflowed
  list (the scenario's intent; before and after identical).

## Code review (`code-review` skill, high)

Fixed before commit: the hourly strip's hour label overprinting the precipitation line at
≥1.5× text scale (`chart::strip_rows` now drives both the painter and the body layout; test
above); the 7-day hi-temperature column too narrow for three-digit / negative readings
(measured `temp_w`); the temperature range track and the hourly scrollbar track fading to a
26-alpha wash (`track_fill` = `hairline_on(raised, 56)`, visible on both themes — the first
attempt, `Colors::glass_film()`, lifts toward white on a paper palette and disappeared on the
white card in `weather/after/light/default.png`, caught on inspection); the maps selected row
reading like a hover wash (`draw::selected_row` is now `hairline_on(raised, 40)` + the panel's
derived edge).
Also fixed while re-checking at 1.5×/2× (`weather/after/{dark,light}/scale/detail-{1.5,2.0}x.png`):
the precipitation dot sat at a fixed `cx − 12` while the scaled "7%" read-out grew into it
(≈1 px at 1.5×) — dot, `s1` and text are now one cluster centred on the column.
Also fixed: two translucent accent washes (the `Locating…` chip, the chosen transit journey)
were passed to `ui::surface::card` as the fill, so their hairline was judged against the
accent's RGB and vanished on light — they now fill the wash and stroke
`ui::surface::edge_on(surfaces.raised)` on `edge_outline`, the panel being the surface actually
seen. Not mine — recorded below: panel `Button::paint` Neutral faces raised-on-raised with the
toolkit's declared `border_standard` rest edge.

## Open — needs other units

- **coordinator (Unit 0)** — `ui::surface::card_on(scene, t, rect, radius, fill, ground)` is
  being added on `ux1x/integration` for translucent fills; once merged, move the two inline
  sites `apps/maps/src/app.rs` (`Locating…` chip in `draw_directions`, chosen journey in
  `draw_departures`; `maps/after/{dark,light}/transit-arrive.png`) to it.

- **F1 (closed on `ux1x/integration` by `8027e8e47`)** — every Neutral `ButtonFace` this unit
  routed panel buttons through (`maps/after/dark/panel-directions.png` ‹ and «,
  `maps/after/dark/default.png` locate / traffic / ⋯, `maps/after/dark/offline.png` zoom
  stepper and row trash, `maps/after/dark/transit-arrive.png` ± and Reset to now,
  `weather/after/dark/default.png` refresh and collapse chip, `weather/after/dark/collapsed.png`
  expand chip) paints `surfaces.raised` on a raised panel with the base toolkit's
  `border_standard` rest edge (`ui/src/widget/button.rs::face_colors`, Neutral arm) — invisible
  on dark. F1 changes that rest edge to `ui::surface::edge_on(raised)`; no local fill or edge
  workaround was added here, so the merged toolkit fixes all of these at once.

- **F1** — `Autocomplete`/`TextInput` field faces in weather search, maps offline search and the
  maps search/endpoint inputs (`set_plain_chrome`) keep their toolkit rest edge; the weather
  suggestion overlay's selected row is an accent fill (`weather/after/dark/search.png`,
  `ui/src/widget/autocomplete.rs`).
- **F2** — maps overflow `ContextMenu` and attribution `ModalDialog` edges
  (`maps/after/dark/menu.png`, `maps/after/dark/attribution.png`).
- **F3** — weather `NavShell` back affordance / pane divider (`weather/after/dark/detail.png`,
  `ui/src/widget/nav_shell.rs`).

## Limits

- `--headless-drive` for maps runs the live root (no scenarios); only the default frame's
  seven controls are lint-checked. The panel's immediate-mode rows are covered by the new
  `explore_rows_keep_a_related_row_gap` test instead.
- Weather's overview list exports chips as semantic children (5 controls) — the inspection
  has no nodes for hero/tiles/rows, so those pairs are asserted in unit tests and measured on
  the PNGs.
- Pixel gaps in the pair table are computed from the layout code at 1×; no image-diff tooling
  exists on this machine.
