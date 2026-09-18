# A3 — store, settings, themes: 1× polish (visible edges, no touching, one rhythm)

Batch: 1×-first UI fix pass (`/Users/xaver/.claude/plans/enchanted-forging-pancake.md`).
Base: `fd1f84391` (Unit 0, `ux1x/unit0`). Branch: `ux1x/a3-store-settings-themes`.
Files edited: `apps/settings/**`, `apps/themes/**`, this report. `apps/store/**` was
verified, not edited (every store PNG is byte-identical before → after, 26/26).

Evidence root: `target/ux1x/a3/<app>/{before,after}/{dark,light}/` (1× = `QOS_SHOTS_TEXT_SCALE=1.0`,
`QUICK_UI_VELLO_CPU=1`, isolated `HOME`/`XDG_*`). Capture and lint scripts that produced it:
`target/ux1x/a3/{capture.sh,geometry.sh,final.sh,rects.py}` (evidence dir, not committed).
Every declared scenario was captured in both themes: store 13 scenarios + responsive matrix;
settings 22 `QOS_SHOTS_PANEL` panes + 8 filename scenarios + matrix; themes 7 `QOS_THEMES_SHOT`
states × wide (1280×800) and narrow (440×820) + two tall frames (440×1500 `menu`, 1280×1500
default) + matrix. 114 PNGs per phase, plus the two rendered fixtures the new tests write
(`themes/after/<theme>/history-panel.png`, `themes/after/<theme>/specimen-<id>.png`).

## Six-rule checklist

1. **No two elements touch or overlap** — `geometry_audit` on the `--headless-drive`
   inspection of every app, dark and light, before and after: `touching_peers: []`,
   `overlaps: []` (`target/ux1x/a3/*/after/*/geometry/default.inspection.json`,
   `final-geometry.log`). The pair tables below show every adjacent pair ≥ `s1`. Only
   non-app-owned exception: the store's floating `ui::StatusBubble` (an overlay forced visible
   for shots) sits over the last card row while the catalogue is scrolled — recorded below.
2. **Every surface has a visible derived edge or a contrast step** — settings sidebar
   selection is now `ui::chrome::selection_tier` (lift + `hairline_on` edge + inner glow), the
   time-zone map well is `ui::surface::card`; in themes every app-painted edge goes through
   `ui::surface::{card, edge_on, edge_outline}` (gallery rows, swatches, both pane dividers, the
   top-bar hairline, the token chips, the history panel, and the specimen's card, list, chart
   panels, Neutral/Disabled buttons, slider track and thumb) — `border_standard` no longer
   appears in a paint path in either app (`grep -rn border_standard apps/{settings,themes}/src`
   → 0 strokes; `model.rs:370` is a data export, `time_date.rs` comments only).
3. **One rhythm per screen** — settings: cards `s6`, heading→body `s4`, rows `s3`, captions
   `s1`, sidebar rows `s1`/pills `s3`, headers `s2`, sections `s6`; the off-scale `CARD_GAP =
   20` became `Spacing::BASE.s6` (`apps/settings/src/widgets.rs:61`), asserted by
   `pane_cards_stack_on_the_s6_step_and_never_touch`. Themes: gallery rows `s2` inset `s4`
   (`gallery_rows_stack_on_the_s2_step_and_never_touch`), customizer gap = `s1 + sm line box +
   s1` for every control (`customizer.rs::gap()`), history panel `s3` siblings / `s4` text inset.
4. **Content centred on measured line boxes** — unchanged where already measured (sidebar
   `RowGeometry`, `ThemeRow`, specimen `SpecimenLayout`); the specimen's selected list row is
   now inset `s1` inside its list so the lift never touches the panel edge or the row rules.
5. **No authored-pixel exemption** — `CARD_GAP` (20), customizer `GAP` (22 "deliberately off
   the token scale"), `PAD` (16), `CHIP_GAP` (8), `CONTROL_LABEL_OFFSET` (18), `SLIDER_H` (30,
   stale), `DROPDOWN_H`/`ACTIONS_H` (40), history panel literals (12/70/80/90/18/700) are now
   token or metric expressions (`Spacing::BASE.*`, `Metrics::BASE.*`,
   `text_metrics::line_height_px`). Remaining fixed values in touched files are text-driven
   1× heights the specimen scales (`TITLE_BAR_H` etc.), `PICKER_H = 220` mirroring
   `color_picker.rs:28`, and the specimen's own demo geometry.
6. **Evidence is visual** — every PNG named below was opened with the Read tool; one line per
   PNG in the surface tables.

## Surface table — settings

| Surface (PNG, dark / light) | Before | After |
|---|---|---|
| `panel-network` | Active sidebar row = full accent fill (`accent_primary`), §9.3/§9.4 violation; cards 19–21 px apart (off-scale 20 ± rounding); page top gutter 20. Cards/fields/buttons already edged (Unit 0). | Active row = `selection_tier` lift with derived hairline + inner glow, primary ink; cards 23–25 (`s6` ± 1 px text rounding); top gutter 24 (`s6`); heading→card 16 (`s4`). Light: row lifts to white on the grey sidebar with a visible edge. |
| `panel-appearance`, `search` | Same accent fill (Appearance row; filtered "RESULTS" list) | Same lift; filtered list keeps `s2` header gap. |
| `panel-time_date` | Map well: `surfaces.base` fill + `border_standard` 1 px stroke on the rect edge (edge faint on dark, half-pixel blurred), graticule `from_rgba8(border_standard, 100)`. | `ui::surface::card(rect, radii.md, surfaces.base)` — crisp inset derived edge, graticule = `edge_on(base)`, marker alpha via `multiply_alpha`. Focus ring unchanged (2 px `border_focus`). Visible in both themes. |
| `panel-region_formats`, `narrow-content` | Cards edged, 52 px choice rows, readable width. Chooser values render in placeholder ink (dim) — see Open. | Unchanged apart from the `s6` card step. |
| `panel-about`, `panel-users`, `panel-external_accounts`, remaining 16 panes, `narrow-list`, `netdemo`, `audiodemo`, `accountdemo*`, matrix | Edged cards, no touching; card gap 20. | Card gap `s6`; nothing else changed. |

## Surface table — themes

| Surface (PNG, dark / light) | Before | After |
|---|---|---|
| `default-wide`, `default-tall` | Gallery rows: `border_standard × 0.6` stroke (near-invisible on dark); gallery/preview and preview/customizer dividers and the top-bar rule `border_standard` (invisible on dark); token chip rings `border_standard`; scroll track `border_standard × 0.35`. Specimen (Sunset): card, list, chart panels, Secondary button stroked with the *previewed* theme's `border_standard`; Disabled = bare fill; slider track = `border_standard` fill; toggle knob / slider thumb `Color::WHITE`; selected list row = accent wash + accent bar. | Rows `ui::surface::card` (derived rim, visible on dark); dividers and top-bar rule `edge_on(fill)` on a whole pixel; chip rings `edge_on(chip)`; scroll track `lift(0.5)`. Specimen: card/list/charts/Secondary via `card`; Disabled = rest face + edge under `DISABLED_ALPHA`; track `lift(1.0)` + edge; one `ink_on_any` control ink for knob and thumb (+ derived thumb edge); selected row = `selection_tier` lift + hairline + glow, inset `s1`. Light: no regression (rows, dividers, rule all still register). |
| `live-narrow` (qOS Dark specimen — the designer's proof) | Card, Secondary, Disabled, list and chart edges = qOS dark `border_standard` on its own raised ground: invisible. | All edges visible on the qOS Dark palette; selected row is a lift. Light host: same. |
| `menu-tall` (Sunset selected) | Selected row: accent-tinted fill (`lerp(bg_app, accent, 0.18)`) + 1.5 px accent ring; unselected rows' faint edge; black OLED/High-Contrast swatches with no visible rim. | Accent *wash* removed (§9.4); the row keeps its card surface with one 1.5 px accent ring (§7 "a ring"; a `lift(1.0)` on an already-raised card is the card's own colour). Every swatch has a rim derived from its own base colour. |
| `customize-narrow`, `scroll-narrow`, `font-wide` | Chip rings faint on dark; ~44 px void between the colour picker and "Corner radius" (see Open — F1); Dracula selection ring. | Chip rims visible; customizer gap is `s1 + sm line box (15) + s1 = 23` for every control (was 22 with a back-fitted 14 px line); the picker void is inside the shared `ColorPicker`'s declared 220 px (unchanged, recorded). |
| `history-panel` (rendered fixture; no `--shots` route into history mode) | `LibraryPreview::draw`: bare `bg_primary` fill over the host ground, literals 12/70/80/90/18/700. | `ui::surface::card(bounds, radii.md, bg_primary)`; row/specimen/token list `s3` siblings inset `s3`, token text inset `s4`; the test asserts Δ luminance ≥ `HAIRLINE_MIN_DELTA` at the panel's edge column vs its fill in dark and light. |
| `specimen-<qos-dark|oled|beige|sunset>` (rendered fixture) | — | `specimen_paints_visible_derived_edges_on_dark_and_light_palettes` samples the frame: card edge vs fill and list edge vs fill ≥ `HAIRLINE_MIN_DELTA` on a dark, a pure-black, a light and the custom palette. |

## Surface table — store (verification only)

| Surface (PNG, dark / light) | Finding |
|---|---|
| `wide`/`narrow` (identical at 1280) | Cards (`Card::composed`), Details buttons, dropdowns, search field all carry the Unit 0 derived edge; sections `s3`; `6 apps available.` status bubble overlays the Chess card's bottom edge while scrolled (transient `ui::StatusBubble`, forced visible for shots; landing zone only applies at scroll end). |
| `detail`, `remotes`, `moderation*`, `error`, `progress`, `empty`, `offline` | Edged section containers (`Container::with_background`), 12 px (`s3`) between sections, fields edged, EmptyState centred, error banner clears the search field by 10 px. No app-painted surface lacks an edge; the only own painting is the artwork fallback tile (`tinted_chip_on` wash, a contrast step). |
| matrix `phone` | Sticky "Ready to use · 3" band clears the first card by 4 px (`s1`). |

## Pair tables (px, before → after; from the inspection JSON unless noted "PNG")

Settings, `panel-network` 1200×760 (identical in dark and light):

| Element A | Element B | Axis | Before → After | Relationship / step | OK |
|---|---|---|---|---|---|
| search band | CONNECTIVITY header | y | 20 → 20 | `SHELL_FIELD_INSET + s2` | ✓ |
| section header | first row | y | 8 → 8 | `s2` | ✓ |
| sidebar row | next row | y | 4 → 4 (pills 12) | `s1` (pills `s3`) | ✓ |
| Bluetooth row | DEVICES header | y | 24 → 24 | `s6` | ✓ |
| selection pill | sidebar edge | x | 12 → 12 | `s3` (= search field inset) | ✓ |
| pill edge | pill inner glow | — | none → 1 px hairline + glow at 1.5 px inset | `selection_tier` | ✓ |
| page top | SETTINGS eyebrow | y | 20 → 24 | `s6` page gutter | ✓ |
| heading subtitle | first card | y | 16 → 16 | `s4` | ✓ |
| card | next card | y | 19 / 21 → 23 / 25 | `s6` ± 1 px (taffy rounds each text-driven height) | ✓ |
| card edge | field / row inside | — | edged (Unit 0) → unchanged | `edge_on(fill)` | ✓ |
| time-zone map well | card (PNG) | edge | faint declared → crisp derived | `card(base)` | ✓ |

Themes, `default-wide` 1280×800:

| Element A | Element B | Axis | Before → After | Relationship / step | OK |
|---|---|---|---|---|---|
| gallery row | next row | y | 8 → 8 | `s2` | ✓ |
| gallery row | gallery edge | x | 16 → 16 | `s4` | ✓ |
| gallery | preview pane | edge | `border_standard` → `edge_on(bg_app)` at x1 − 0.5 | derived divider | ✓ |
| preview | customizer | edge | `border_standard` → `edge_on(bg_app)` at x0 + 0.5 | derived divider | ✓ |
| top bar | panes | edge | `border_standard` → `edge_on(bg_primary)` | Bar hairline | ✓ |
| chip | chip | x/y | 8 → 8 | `s2` | ✓ |
| control | next label | y | 4 → 4 | `s1` | ✓ |
| label line box | its control | y | 4 → 4 (line 14 → 15 measured) | `s1` | ✓ |
| colour picker box | Corner-radius slider | y | 22 → 23 | `gap()` (void is inside the picker box — F1) | ✓ |
| specimen selected row | list edge / row rules | — | 0 (full-bleed wash) → 4 | `s1` | ✓ |
| specimen card edge | card fill | Δ lum | < floor on dark → ≥ `HAIRLINE_MIN_DELTA` | test-asserted | ✓ |
| history panel row | panel edge | x/y | 12 → 12 | `s3` | ✓ |
| history row | specimen band | y | 10 → 12 | `s3` | ✓ |
| specimen band | token list | y | 10 → 12 | `s3` | ✓ |
| token list text | panel edge | x | 18 → 16 | `s4` | ✓ |

Store, `wide` 1280×800 (PNG; unchanged):

| Element A | Element B | Axis | Gap | Step | OK |
|---|---|---|---|---|---|
| card | card | x / y | 12 / 12 | `s3` | ✓ |
| section heading | cards | y | 12 | `s3` | ✓ |
| Details | Open | x | 12 | `s3` | ✓ |
| status bubble | Chess card | — | overlaps (overlay) | by design; recorded | see Open |

## Ratchets (`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`)

- `app_and_shell_theme_escape_hatches_do_not_grow`: **FAILED** on pre-existing `grew` lines
  only — `apps/browser/src/browser_tabs.rs` (literal_color 1, white_or_black 1) and
  `shell/src/shell/frame.rs` (literal_color 1, white_or_black 2) — the failure the brief
  excludes. No `grew` in `apps/{store,settings,themes}`. This diff *lowers* the tree: `time_date.rs`
  literal_color −2, `preview.rs` white_or_black −2, `app.rs` white_or_black −1 (the stale
  lines are not printed because the grew assertion fires first).
- `spacing_literals_do_not_grow`: **FAILED** on stale-high entries that are mine to report:
  ```
  apps/settings/src/widgets.rs [pixel_const]: now 3, baseline still 4 — lower it to 3
  apps/themes/src/customizer.rs [pixel_const]: now 6, baseline still 12 — lower it to 6
  ```
  Final run (`target/ux1x/a3/final-test-ratchets.log`), after the review fixes:
  ```
  apps/settings/src/widgets.rs [pixel_const]: now 3, baseline still 4 — lower it to 3
  apps/themes/src/customizer.rs [pixel_const]: now 5, baseline still 12 — lower it to 5
  ```
  (coordinator re-baselines; counts only went down). 7 passed, 2 failed for the two reasons above.
- `declared_borders_in_chrome_do_not_grow`, `no_hardcoded_style_literals_in_tree`,
  `theme_escape_hatches_do_not_grow`, pattern self-checks: ok.

## Tests

Final run at the committed tree (`target/ux1x/a3/final-*.log`; inline env,
`--no-default-features --locked`, `QUICK_UI_VELLO_CPU=1`):

| Command | Result |
|---|---|
| `cargo build -p store -p settings -p themes` | ok, no warnings in `apps/{store,settings,themes}` |
| `cargo check -p store -p settings -p themes --tests` | ok (`final-check.log`, EXIT 0) |
| `cargo test -p themes` | lib 66 passed, bin 1 passed, `alignment_review` 8 passed, 0 failed |
| `cargo test -p settings` | lib 301 passed, `prefs_persistence` 1 passed, `it` 41 passed / **1 failed** — `sidebar_rows_seat_the_pointer_target_at_1x_and_grow_with_large_text`, pre-existing (fails identically at `fd1f84391`, `base-verify.log`) |
| `cargo test -p store` | lib 103 passed, bin 1 passed, `alignment_review` 6 passed, 0 failed |
| `cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit` | 7 passed, 2 failed: pre-existing `grew` (browser/frame) and my `stale` lines — see Ratchets |
| `geometry.sh after` (store/settings/themes × dark/light) | `touching_peers: []`, `overlaps: []` everywhere |

New / updated tests:
- `apps/settings/src/sidebar.rs::active_row_ink_meets_text_contrast_on_the_lifted_fill`
  (replaces `…on_the_accent_fill`; asserts the tier fill is opaque, carries edge + glow, ink ≥ 4.5:1
  on it, hover has no edge, press carries scale, idle paints nothing; dark/light/high-contrast).
- `apps/settings/tests/alignment_review.rs::pane_cards_stack_on_the_s6_step_and_never_touch`
  (About/Network/Users wide, Region narrow: `CARD_GAP == s6`, every card pair ≥ `s1` and `s6 ± 1`,
  heading→first card `s4 ± 1`).
- `apps/themes/tests/alignment_review.rs::gallery_rows_stack_on_the_s2_step_and_never_touch`.
- `apps/themes/src/preview.rs::specimen_paints_visible_derived_edges_on_dark_and_light_palettes`
  (renders `draw_specimen` for qos-dark, oled, beige, sunset; edge-pixel Δ ≥ floor; writes PNGs).
- `apps/themes/src/framework_history_tests.rs::library_panel_paints_a_visible_derived_edge_in_dark_and_light`
  (renders the real `HistoryPreview` over the host ground; edge-pixel Δ ≥ floor; writes PNGs).

Pre-existing red, verified at the base commit in a throwaway worktree
(`target/ux1x/a3/base-verify.log`: `sidebar_rows_seat_the_pointer_target_at_1x_and_grow_with_large_text`
FAILED at `fd1f84391` with the same message "the sidebar is still docked at 1600 px / 200 %").
Not touched by this diff; recorded, not fixed.

## Open — needs another unit

| Item | Owner | Evidence |
|---|---|---|
| `ColorPicker` declares 220 px (`ui/src/widget/color_picker.rs:28,749`) but paints ≈200 px, leaving a ~40 px void above the next control in the Themes customizer (and everywhere it is stacked). | F1 | `themes/{before,after}/dark/customize-narrow.png`; inspection `Customizer/ColorPicker [952,278,260,220]` vs `Slider y=520`. |
| `Dropdown` has `with_placeholder` but no initial-selection API (`ui/src/widget/dropdown.rs:222`); Settings' Region choosers therefore show the *current* preference in placeholder (`text_dim`) ink until edited (`apps/settings/src/panes/region.rs:121-166`) — reads as a disabled control at 1×. Fixing needs the API, then a settings follow-up to seed the draft. | F1 (+ settings follow-up) | `settings/after/{dark,light}/panel-region_formats.png`, `narrow-content.png`. |
| `Scrollable` exposes no accessible label — the geometry audit's only failures on Settings (`SettingsApp/AppShell/Container[0..1]/Scrollable`). | F3 | `settings/after/*/geometry/default.inspection.json`. |
| No shared painter for a `SelectionTier` (fill + `edge_outline` stroke + glow); the sidebar and the specimen restate the three strokes (as `overlay_host.rs` does three times). A `ui::chrome::paint_selection(scene, transform, rect, radius, &tier)` beside `selection_tier` would remove the copies. | coordinator / toolkit | `apps/settings/src/sidebar.rs` draw, `apps/themes/src/preview.rs::draw_list_row`. |
| `--headless-drive` at this base rejects `"scenario"` (`error: unknown field "scenario"; expected viewport, actions, audit, audit_report, dump, targets, or inspect`), so store scenarios cannot be linted by geometry; the plan's Unit 0 item (4) is not in `fd1f84391`. | coordinator (Unit 0) | `target/ux1x/a3/store/before/dark/geometry/*.drive.log`. |
| Store `ui::StatusBubble` overlays the last card row while the catalogue is scrolled (forced visible in shots). Transient overlay by design; if rule 1 is to apply to overlays, the bubble needs a reserved band or auto-hide-on-scroll in the toolkit. | F4 (`status_bubble.rs`) / record | `store/after/dark/wide.png`, `matrix/dev.qos.store-phone.png`. |
| Region choosers (above) aside, `apps/settings/src/panes/{power,mouse_keyboard}.rs` seed a `Theme::dark().accent_primary` at build time (theme-blind accent in two sparklines). Not a rule 1–4 defect; noted for a settings follow-up. | settings (later) | — |
| The launcher's history strip ("History · Theme library (Apply separately)") above the Themes top bar is shared launcher chrome, not app paint. | — | `themes/after/*/default-wide.png` top 32 px. |

## Limits

- The customizer's `SLIDER_H` mirrors the Slider's fine-pointer target (`Metrics::BASE.min_pointer_target`);
  under a coarse-pointer environment the Slider lays out taller and the narrow stacked panel's
  intrinsic height (a static function) would under-reserve. Same class of limit as before, now
  documented at the constant.
- The `menu` shot state does not render its open actions menu in a single headless frame (the
  selected Sunset row and its ring are captured in `menu-tall.png`).
- `geometry_audit` sees only the default root of each app (no `"scenario"` at this base); the
  store's detail/remotes/moderation states were checked visually only.
- Store: verification only; its edges come from Unit 0's shared widgets.
