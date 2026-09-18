# A9 — ebook, pdf_viewer, file-explorer: 1× visible edges, no touching, one rhythm

Branch `ux1x/a9-documents`, cut from Unit 0 (`fd1f84391`). Edits confined to
`apps/ebook/**`, `apps/pdf_viewer/**`, `apps/file-explorer/**` and this report.
Evidence under `target/ux1x/a9/<app>/{before,after}/{dark,light}/` (every
declared scenario at 1280×800, 1× text, plus the four-viewport responsive
matrix); geometry dumps and `geometry_audit` output under `…/geom/`.

## Six-rule checklist

| # | Rule | Evidence |
|---|---|---|
| 1 | No two elements touch or overlap | AFTER `geometry_audit`: zero `touching_peers`, zero `semantic_bounds_overlap_unresolved` on every ebook scenario (36 dark+light runs) and every pdf_viewer scenario (16 runs). BEFORE had `touching_peers=1` (ebook settings-colours: rename hit shared an edge with the apply hit) and 4–13 overlaps per reader/popover scenario. Pair tables below; `file_view.rs::neighbouring_faces_keep_a_token_gap` and `places_sidebar.rs::rows_and_sections_keep_token_gaps` assert the `s1` gaps in Files. |
| 2 | Every surface has a visible derived edge or contrast step | Every app-painted card/row/pill/menu/sheet edge now comes from `ui::surface::{card, edge_on, edge_on_with, edge_outline, separator}` or `ui::chrome::{panel_ground, panel_border, panel_shadow, selection_tier, fill_scrim}`; `border_standard` is gone from all three apps' chrome (`grep border_standard apps/{ebook,pdf_viewer,file-explorer}/src` → 0 paint sites; one comment). `apps/ebook/src/chrome.rs::every_chrome_edge_registers_on_every_preset` and `apps/pdf_viewer/src/chrome.rs::every_chrome_edge_registers_on_every_preset` assert Δ luminance ≥ `HAIRLINE_MIN_DELTA` for every chrome fill on dark, light and high-contrast. |
| 3 | One spacing rhythm per screen | `apps/ebook/src/chrome.rs::rhythm` and `apps/pdf_viewer/src/chrome.rs::rhythm` (`TIGHT=s1, RELATED=s2, SIBLING=s3, SECTION=s4, GROUP=s6`) replace the per-site literals; Files uses `Spacing::BASE` consts (`ROW_HL_INSET_*`, `ICON_LABEL_GAP`, `PAD/TOP/SECTION_GAP/ROW_CONTENT_INSET`). Rhythm lists per screen below. |
| 4 | Content centred in its container on measured line boxes | Unchanged where already measured (Files rows/header, reader pills); newly measured: ebook menu row labels (`draw_menu`, `draw_default_font_menu`), the notes "add" row label, the reading-settings pills (`label_h` instead of the nominal 12.5), the annotation card header (dot, kind, locator, delete on the band's centre line), the pdf placed-signature × (centred on the row centre line), the ebook shortcut binding pill (centred in its row). |
| 5 | No authored-pixel exemption | Every gap that produced a defect was replaced by a token expression; every new module-level constant is a token expression (`MENU_ROW_H = min_pointer_target + s1`, `NOTES_ADD_ROW_H = min_pointer_target`, `SETTINGS_SHORTCUT_ROW_H = min_pointer_target + 2·s3`, `NOTE_CARD_CONTROL = icon_lg`, `SIG_ROW_H = panel_row_height`); the pre-existing row heights and widths that remain (34/64/68, `TOC_W`, `NOTES_W`, card widths) are named consts the `f1_spacing` baseline already carries. |
| 6 | Evidence is visual | 65 BEFORE + 65 AFTER scenario PNGs per theme (130 dark + 130 light) plus matrices; the ones cited below were opened with the Read tool and are described one line each. |

## Surfaces — before → after (dark and light, 1×)

### ebook (`apps/ebook/src/{app,chrome,shelf_controls,reading_settings,comment_overlay,input_controls,color_picker}.rs`)

New module `apps/ebook/src/chrome.rs`: `panel` (Panel role: `panel_shadow` + `panel_ground` + `panel_border`), `card`/`card_on_raised` (`ui::surface::card`), `edge` (derived edge on an arbitrary fill), `row` (the shared `selection_tier`, optional quiet floor), `quiet_pill` (chip-quiet rest), `bar_edge`, `rail`, `sheet_scrim`, `pointer_target`, `rhythm`, `MENU_ROW_H`. Every one of the 39 scenarios was captured before/after in both themes.

| Surface (PNG) | Before | After |
|---|---|---|
| Library top bar (`library`, `shelf`) | `border_standard` hairline on `raised` — invisible on dark; Sort button `overlay` + `border_standard`; Hide-read ↔ Sort 18 px, Sort ↔ Open 16 px | `bar_edge` = `edge_on(raised)` (visible dark + light); Sort is a quiet pill (`raised` + `edge_on`), focus ring only while its menu is open; Hide-read ↔ Sort ↔ Open all `s3` (12) |
| Book cards (`library`) | no rest edge; literal `rgba(0,0,0,60)` slab shadow; hover `border_focus` | `ui::surface::shadow(e2)` following the cover outline; generated boards get `edge_on(spine)`; real cover art is seated by `chrome::seat` — the edge is derived from the shelf ground and stroked just *outside* the art on that ground, so a dark cover on a light shelf still frames; focus `border_focus` 2 px, hover `panel_border` 1.5 px; grid gap 28 → `s6` (24), margin `s6+s4` |
| Shared/Local badge, "N notes" badge, Remove disc | Local `rgba(0,0,0,150)` literal, notes badge bare `overlay`, Remove `rgba(0,0,0,180)` | Local = `overlay` + `edge_on`, ink `on_surface`; notes badge `quiet_pill(raised)`; Remove = `overlay` disc + derived edge, `text_primary` glyph; Shared keeps the accent (live collaborative state) |
| Progress track | `neutral[3]` ramp slot | `lift(1.0)` + `edge_on` |
| Add-a-book tile | `overlay` fill + dashed accent 0.55 | `raised` + dashed `edge_on_with(PANEL_BORDER_ALPHA)`; accent only on hover (live drop target) |
| Sort menu (`shelf_controls::draw_menu`) | literal black shadow, `raised` + `border_standard`, selected row accent 0.16 wash, row pitch 34 | `chrome::panel`; rows `MENU_ROW_H` 36 + `s2`; selected/hover = `selection_tier`; label centred on measured line box |
| Reader top bar (`read`, `type`, …) | `border_standard` hairline; icon toggles active = accent fill with literal white ink; Back↔TOC 6 px, Notes↔Bookmark 4 px, Bookmark↔Aa 8 px; Aa pill bare `overlay` | `bar_edge`; toggles = `selection_tier` (lift + hairline + glow), `text_primary` ink; every gap `s3`, edge insets `s4`; Aa pill quiet (`raised` + edge), lifted while open; title bounds use the same steps |
| Contents rail + rows (`read`, `long-toc`) | rail = `base` on `base` + invisible `border_standard` line; active row accent 0.16 wash + accent bar | `chrome::rail`: stays on `base` (the shared tier lifts from `base`, so a lifted rail would swallow its hover step) with an `edge_on(base)` hairline that registers; active row `selection_tier`, hover `lift(0.5)`; row inset `s3`, number inset `s4` |
| Annotations rail, cards, add row, count badge (`read`, `long-annotations`) | rail base-on-base; cards `raised` no edge, 12 px apart; add row bare `overlay`; badge bare `overlay`; delete 20×20 at 4 px from the locator; card hit under the edit hit (overlap) | `chrome::rail`; cards `ui::surface::card` (hover → `panel_border`, composing → focus ring); add row `quiet_pill` (`min_pointer_target` tall); badge `quiet_pill`; header band = `icon_lg` + 2·`s1` with dot, kind label, locator and delete all centred on its centre line; delete painted `icon_lg` and hit at `pointer_target` (≥ `min_pointer_target`), `s4` in, locator ends `s2` before it; jump/edit hits are disjoint bands an `s1` apart |
| Selection HUD (`highlight`) | `raised` + `border_standard`, ring = reading-palette accent | `panel_ground` + `panel_border`, ring = OS `accent_primary` (chrome follows the OS theme; the palette stays on the page) |
| Reading-settings popover (`type`) | `raised` + `border_standard`; pills bare `overlay`; dismiss hit = whole viewport under every pill (13 overlaps) | `chrome::panel`; pills chip-quiet (`raised` + `edge_on`), chosen = accent with `on_surface` ink, label centred on measured box; dismiss registered as four bands around the panel, one node in the tree |
| Note composer (`note`) / comment overlay (`comments`) | literal shadow + `raised` + always-on `border_focus` 1.4; comment rows bare `overlay`; delete/edit glyph boxes 2 px apart; Add-comment accent 0.14 wash | `chrome::panel`; rows `card_on_raised`; controls painted 24×24, `s3` apart, `s1` in, hit at `pointer_target`; Add-comment = shared Neutral `Button::paint` with `Icon::Plus` |
| Load/error toast (`import-error`) | `raised` + semantic-tinted `border_standard` | `chrome::panel` + 3 px semantic severity stripe on the leading edge |
| Settings top bar + tabs (`settings-*`) | two `border_standard` rules | `bar_edge(raised)` and `bar_edge(base)` |
| Shortcut rows | `raised` + `border_standard`; binding pill bare `overlay` 32 px at a fixed `y+13` | `ui::surface::card` (hover lifts), accent ring only while capturing (live); pill `card_on_raised` centred on the row; pitch 58 + `s3` |
| Library-settings card, watched-folder rows | `raised` no edge, 12 px gaps | `chrome::card`; gaps `s3`; card → CTA `s4` |
| Default-font dropdown + menu (`settings-fonts`) | `raised` + `border_standard` (focus swap); menu literal shadow + `border_standard`, selected row accent wash, row 40 | field = `chrome::card` + focus ring while open; menu `chrome::panel`, rows `MENU_ROW_H`+`s2`, `selection_tier`; dropdown → section `s6` |
| Font rows + Add-a-system-font | `raised` no edge, 10 px gaps; pill `overlay` + `border_standard` r18 | `chrome::card`, gaps `s3`; button = shared Neutral `Button::paint` with `Icon::Plus` |
| Colour-scheme rows + swatches + actions (`settings-colours`) | `raised` no edge, 10 px gaps, selected = accent 1.6 ring; swatches `border_standard`; Edit/Duplicate bare `overlay` pills 8 px apart; rename hit touching apply hit | `chrome::row(floor)`: idle / hover / chosen are three points on one `lift` ramp (`FLOOR_IDLE_T` 0.35, `FLOOR_HOVER_T` 0.65, 1.0) and the derived edge stays on in every state (HIG §9.3's caveat; asserted by `chrome::tests::floored_rows_keep_their_order_on_every_preset`); swatch edges `edge_on(swatch)`; cluster gaps all `s3`; pills chip-quiet; rename/apply hits an `s1` apart; New-scheme = shared Primary button |
| Scheme editor (`scheme-editor`), font picker (`font-picker`), colour picker (`picker`) | scrim `rgba(0,0,0)` ×0.4/0.45; `raised` + `border_standard` panels; Done/Cancel hand-painted (Cancel bare `overlay`); chips/preview `border_standard`; SV/hue frames `border_standard` | `sheet_scrim` (`fill_scrim` 0.45, one value); `chrome::panel`; Done = Primary, Cancel = Neutral `Button::paint`, `s3` apart; chip/preview/swatch edges `edge_on(colour)`; SV/hue frame `hairline_on(base, PANEL_BORDER_ALPHA)`; picker inner inset `s6` |
| Empty library (`onboarding`) | plus glyph ink literal white | `on_surface(accent).ink` |
| Framed page shadow, rail scrim | literal black slab / literal black scrim | `ui::surface::shadow(e2)`; `fill_scrim` following the reveal |

Light did not regress: `after/light/{library,read,settings-fonts,settings-colours,type,scheme-editor}.png` opened — hairlines register as grey on white, chosen rows are white lifts with a hairline, the scrim tones the desktop instead of blackening it.

### pdf_viewer (`apps/pdf_viewer/src/{app,app_draw,chrome}.rs`)

New module `apps/pdf_viewer/src/chrome.rs` (`panel`, `card` with a chosen state, `row`, `quiet_pill`, `bar_edge`, `divider`, `rhythm`). All 9 scenarios captured before/after in both themes.

| Surface (PNG) | Before | After |
|---|---|---|
| Toolbar (`*`) | `border_standard` 1 px fill; icon buttons active = accent fill + `on_accent` ink; Zoom−/label/Zoom+ placed with **gap 0**; cluster gaps 6/10/14 | `bar_edge(raised)`; active = `selection_tier` (lift + hairline + glow) with `text_primary` ink; in-cluster gap `s2`, between clusters `s3`; faces inset `s1/2` |
| Find bar (`search`) | `border_standard` hairline; field always `border_focus`; Case/Whole-word pills `lift(0.72)` no edge | `bar_edge(glass)`; field `card(base)`, focus ring only when the editor is focused; pills chip-quiet, active = accent |
| Sidebar + tab strip | `border_standard` divider and hairline | `divider(raised)` and `bar_edge(raised)` |
| Comment cards (`edit-comment`) | `base` fill (darker than the panel) + `border_standard`; selected = accent 28 wash + accent 1.5 ring; body focus ring overlapped the × by 4 px; Resolve chip `lift(0.72)` no edge; reply rail `border_standard` | `card(overlay)` + `edge_on`; selected = same face, `PANEL_BORDER_ALPHA` edge + glow; `comment_body_top` = header + control + 2·`s1` (shared by layout and metrics) so the ring clears the ×; × hit at `pointer_target`; Resolve chip `raised` + edge; rail `edge_on_with(overlay, PANEL_BORDER_ALPHA)`; "+ Reply" hover = tier |
| Outline rows | accent 30 wash + 2.5 px accent bar; pitch 32 (2 px gap), unscaled insets | current page = the same chosen face the comment cards use (`chrome::card(overlay, selected)`: one tier up, Panel-strength edge + glow — the shared tier's `lift(1.0)` coincides with the raised sidebar), hover = tier `lift(0.5)`; pitch 30 + `s1`; insets `s2`/`s3`, indent `s4` |
| Signatures (`sign`) | compose card / draw pad `base` + `border_standard`; Place button `lift(0.72)` no edge; placed rows `base` + `border_standard`, selected accent wash; × 18 px; signer text unbounded | fields = `card(base)`; pad focus ring while drawing; Place = shared `Button::paint` (Primary while arming); rows `card(overlay)` with chosen edge+glow; × painted 24 px, hit at `pointer_target`, `s3` in, signer/meta bounded `s2` before it; toggle→card `s3`, card→button `s4`, button→list `s4`, rows `SIG_ROW_H` (= `Metrics::BASE.panel_row_height`) + `s3` (layout + scroll model share the constants) |
| Context menu | literal shadow + `raised` + `border_standard`; hover `lift(0.72)` | `chrome::panel` (radius md) + `selection_tier` hover; insets `s1` |
| Empty/error state (`error`, `loading`) | glyph card `raised` + `border_standard` 1.5, rules `border_standard`; button hover = `lift` over accent; progress track `border_standard` | `ui::surface::card(raised)`; rules `edge_on_with(raised, PANEL_BORDER_ALPHA)`; button through `face_colors(Primary)`; track `card(lift(1.0))` |
| Selection HUD, overlay scrim | `raised` + `border_standard`; scrim `rgba(0,0,0,120)` | `panel_ground` + `panel_border`; `fill_scrim` 0.45 |

### file-explorer (`apps/file-explorer/src/{views/file_view,places_sidebar,files_app,breadcrumb_bar}.rs`)

All 17 scenarios captured before/after in both themes with a seeded fixture home (`target/ux1x/a9/file-explorer/home`: 6 folders + 9 files so the list/grid/selection scenarios show real rows).

| Surface (PNG) | Before | After |
|---|---|---|
| List rows (`select`, `rename`, `sync`) | selected = accent wash + `Color::WHITE` text; hover `lift(0.72)`; highlight inset 6/2 literal; header rule `hairline_on(raised)` on a `base` band | `selection_tier` (hover `lift(0.5)`, selected `lift(1.0)` + hairline + glow), ordinary ink; face inset `s2` / `s1/2` (`list_row_face`, shared by highlight, focus ring and rename field); icon ↔ name `s3` so the rename field clears the icon by `s1`; header rule `ui::surface::separator(base)` inset `pad_x` |
| Grid cells + type badges (`grid`, `thumbnails`) | selected accent wash; badge white veneer when selected; thumbnail frame `lift(0.72)` | `selection_tier`; badge keeps its tint (edge via `edge_outline`); thumbnail seated by an `edge_on(base)` hairline stroked just outside the image on the row ground, so light and dark photos both frame |
| Places sidebar (`*`) | active row accent wash + white ink; hover `lift(0.72)`; ghost accent 0xe0; panel `lift(0.35)` + `hairline_on(raised)` edge; TOP 10, SECTION_GAP 10, insets 10 | active = `selection_tier`, ghost = raised card + `panel_shadow` + edge; panel stays on `base` (the tier lifts from `base`) with an `edge_on(base)` hairline; `PAD/TOP/SECTION_GAP/ROW_CONTENT_INSET = s3`, icon↔label `s2`, pill inset `s1/2` |
| Collab sheet (`remote-account`) | `raised` + `border_standard`; action button hand-painted accent/danger wash | Panel role (`panel_shadow` + `panel_ground` + `panel_border`, `ChromeRole::Panel.radius`); action = shared `ButtonFace` (`primary` / `danger_outline`) |
| Drag ghost (`pindrag`) | `raised` + `border_standard` | `panel_shadow` + `ui::surface::card(raised)` |
| Drop highlight (`dropmove`) | accent wash, literal insets | accent kept (live drop target, HIG §9.4), insets `s2`/`s1/2` from tokens, radius `radii.md` |
| Breadcrumb bar | `border_standard × 0.6` rule | `edge_on(base)` |

## Pair tables (px, 1×; from `--headless-drive` inspections via `target/ux1x/a9/pairs.py`, or from the geometry consts where the surface has no semantic node)

### ebook

| Surface | A | B | axis | before → after | relationship / step | OK |
|---|---|---|---|---|---|---|
| library bar | Hide read | Sort | x | 18 → 12 | siblings `s3` | ✓ |
| library bar | Sort | Open book | x | 16 → 12 | siblings `s3` | ✓ |
| library bar | Open book | bar edge | x | 22 → 16 | gutter `s4` | ✓ |
| shelf | card | card | x | 28 → 24 | next group `s6` | ✓ |
| shelf | progress bar | pct label | x | 8 → 8 | related `s2` | ✓ |
| reader bar | Back | Contents | x | 6 → 12 | siblings `s3` | ✓ |
| reader bar | Bookmark | Annotations | x | 4 → 12 | siblings `s3` | ✓ |
| reader bar | Aa | Bookmark | x | 8 → 12 | siblings `s3` | ✓ |
| TOC | row | row | y | 4 → 4 | `s1` (`toc_row_gap`) | ✓ |
| notes rail | card | card | y | 12 → 12 | siblings `s3` | ✓ |
| notes card | locator | delete | x | 4 → 8 | related `s2` | ✓ |
| notes card | reveal hit | edit hit | y | overlap → 4 | `s1` | ✓ |
| popover | Close (dismiss) | any pill | — | overlap (13) → disjoint bands | — | ✓ |
| popover | pill | pill | x | 8 → 8 | related `s2` (`pill_gap`) | ✓ |
| settings | row | row (fonts / schemes / shortcuts) | y | 10 / 10 / 10 → 12 | siblings `s3` | ✓ |
| settings | watched-folder row | row | y | 12 → 12 | siblings `s3` | ✓ |
| settings | dropdown | READING FONTS | y | 26 → 24 | next group `s6` | ✓ |
| scheme row | Duplicate | Edit | x | 8 → 12 | siblings `s3` | ✓ |
| scheme row | Edit | Delete | x | 8 → 12 | siblings `s3` | ✓ |
| scheme row | Delete | toggle | x | 16 → 12 | siblings `s3` | ✓ |
| scheme row | swatch | swatch | x | 6 → 8 | related `s2` | ✓ |
| scheme row | Rename | apply (status line) | y | **0** → 4 | `s1` | ✓ |
| scheme editor | swatch tile | tile | x | 16 → 16 | section `s4` | ✓ |
| colour picker | Cancel | Done | x | 10 → 12 | siblings `s3` | ✓ |
| comment overlay | edit glyph | delete glyph | x | 2 → 12 | siblings `s3` | ✓ |
| menus | row face | row face | y | 0 (34 pitch, 30 face) / 0 → 8 | related `s2` | ✓ |

### pdf_viewer

| Surface | A | B | axis | before → after | relationship / step | OK |
|---|---|---|---|---|---|---|
| toolbar | Zoom− | zoom label | x | **0** → 8 | in-cluster `s2` | ✓ |
| toolbar | zoom label | Zoom+ | x | **0** → 8 | in-cluster `s2` | ✓ |
| toolbar | Zoom+ | Fit | x | 10 → 12 | clusters `s3` | ✓ |
| toolbar | Fit | Find | x | 14 → 12 | clusters `s3` | ✓ |
| toolbar | Find | Sidebar | x | 6 → 8 | in-cluster `s2` | ✓ |
| toolbar | Open | Save | x | 6 → 8 | in-cluster `s2` | ✓ |
| toolbar | tool | tool | x | 6 → 8 | in-cluster `s2` | ✓ |
| comment card | × bottom | body focus ring top | y | **−4** (overlap) → 4 | `s1` | ✓ |
| comment card | card | card | y | 12 → 12 | siblings `s3` | ✓ |
| outline | row | row | y | 2 → 4 | `s1` | ✓ |
| signatures | toggle | compose card | y | 10 → 12 | siblings `s3` | ✓ |
| signatures | compose card | Place | y | 14 → 16 | section `s4` | ✓ |
| signatures | Place | PLACED header | y | 16 → 16 | section `s4` | ✓ |
| signatures | placed row | placed row | y | 10 → 12 | siblings `s3` | ✓ |
| placed row | signer text | × | x | unbounded → ≥ 8 | related `s2` | ✓ |
| context menu | item face | panel edge | all | 4 → 4 | `s1` | ✓ |

### file-explorer

| Surface | A | B | axis | before → after | relationship / step | OK |
|---|---|---|---|---|---|---|
| list | row face | row face | y | 4 → 4 | `s1` (asserted in `neighbouring_faces_keep_a_token_gap`) | ✓ |
| list | icon | rename field | x | 2 → 4 | `s1` | ✓ |
| list | icon | name | x | 10 → 12 | siblings `s3` | ✓ |
| grid | cell | cell | x / y | 16 / 16 → 16 / 16 | `GRID_GAP` (asserted ≥ `s1`) | ✓ |
| places | pill | pill | y | 4 → 4 | `s1` (asserted in `rows_and_sections_keep_token_gaps`) | ✓ |
| places | last favourite | SYSTEM header | y | 10 → 12 | section `s3` | ✓ |
| places | icon | label | x | 10 → 8 | related `s2` | ✓ |

## Geometry lint (`scripts/ux-bench/usability.py::geometry_audit`)

BEFORE (dark = light): ebook `settings-colours touching_peers=1`; `read`/`long-toc` overlap 1–4, `long-annotations` 7, `type` 13, `page-bubble`/`highlight` 4; pdf_viewer clean; file-explorer could not be driven (see limits).

AFTER:

```
ebook      dark : library shelf settings-library settings-fonts settings-colours settings-shortcuts
                  scheme-editor picker font-picker read long-toc long-annotations comments note type
                  page-bubble highlight onboarding                      — all 18 `clean`
ebook      light: the same 18 scenarios                                — all `clean`
pdf_viewer dark : edit-comment selection-hud page-bubble search sign reorder loading error — all 8 `clean`
pdf_viewer light: the same 8 scenarios                                 — all `clean`
file-explorer   : `--headless-drive` refused (`ui/src/launch.rs:730`, custom `open_state` opener) — no inspection; see Open
```
`clean` = zero `touching_peers`, zero `semantic_bounds_overlap_unresolved`, zero `target_size_24`
(`target/ux1x/a9/lint-after.log`; per scenario `…/<phase>/<theme>/geom/<scenario>.audit.json`).
No `target_size_24` remains either: every small painted control (toggle tracks, annotation and
comment delete/edit glyphs, pdf × buttons) registers its hit through `chrome::pointer_target`,
which grows the rect symmetrically to `metrics.min_pointer_target` (32) without painting bigger.

## Ratchets

`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit` → 7 passed, 2 failed:

- `f1_spacing_token_audit::spacing_literals_do_not_grow` — **no `grew` line**. One `stale` line,
  this unit's, for the coordinator's re-baseline:
  ```
  apps/file-explorer/src/places_sidebar.rs [pixel_const]: now 4, baseline still 8 — lower it to 4
  ```
  (An earlier run of this branch reported `grew` for `apps/ebook/src/app.rs` 13→17,
  `apps/ebook/src/chrome.rs` 0→1 and `apps/pdf_viewer/src/app.rs` 15→16; those constants are
  now token expressions — `Metrics::BASE.min_pointer_target + 2·s3`, `Metrics::BASE.icon_lg +
  2·s1`, `Metrics::BASE.panel_row_height` — so every count is at or under its baseline.)
- `design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow` — the pre-existing
  `apps/browser/src/browser_tabs.rs` / `shell/src/shell/frame.rs` `literal_color` /
  `white_or_black` growth named in the brief; not this unit's.
- `declared_borders_in_chrome_do_not_grow`, `theme_escape_hatches_do_not_grow`,
  `no_hardcoded_style_literals_in_tree`, `product_apps_use_the_shared_responsive_launcher` and
  the three pattern self-checks pass.

## Tests

All with the inline `RUSTFLAGS`/`sccache` env, `--no-default-features --locked`
(`target/ux1x/a9/final-tests.log`, `tests-fe-serial-all.log`):

| Command | Result |
|---|---|
| `cargo check -p ebook -p pdf_viewer -p file-explorer --tests` | clean (no warnings in the three apps) |
| `cargo build -p ebook -p pdf_viewer -p file-explorer` | ok |
| `cargo test -p pdf_viewer --lib` / `--tests` | **169 passed**, 0 failed (was 168; + `chrome::tests` ×3, one merged) |
| `cargo test -p ebook --lib` / `--tests` | **214 passed, 2 failed** — the two pre-existing Linux-only typography goldens (`serif_reader_matches_typography_baseline`, `mathematical_reader_matches_shared_typesetting_baseline`; host-rendering difference recorded by rounds one and two). New: `chrome::tests` ×4; `shelf_controls::tests::open_menu_owns_rows_and_gap_swallowing` updated to the token pitch and now also asserts the `s1` gap above the menu belongs to nobody. |
| `cargo test -p file-explorer --lib -- --test-threads=1` | **251 passed**, 0 failed (new: `views::file_view::tests::neighbouring_faces_keep_a_token_gap`, `places_sidebar::tests::rows_and_sections_keep_token_gaps`) |
| `cargo test -p file-explorer --lib` (parallel) | 234 passed, 17 failed — every failure is the pre-existing `fixture_io_guard` race ("ambient guard already armed", `model.rs:2189/2207`); the same set passes single-threaded |
| `cargo test -p file-explorer --tests` | the `src/lib.rs` target as above; the other targets were not re-run here beyond compile (pre-existing German-locale asserts in `tests/it.rs`, recorded by rounds one and two) |
| `cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit` | 7 passed, 2 failed as recorded under Ratchets |

## Open — needs another unit

- **F1** `ui::Toggle` track is 22 px tall (the apps now grow their own hit rects around it via `pointer_target`); `ToggleGroup`/`SegmentedFace` segments share edges (pdf sidebar tabs and Type/Draw report gap 0 in the BEFORE pairs) and the Files view switcher paints an accent wash for the chosen segment (`file-explorer/after/dark/select.png`, top right).
- **F2** `ui::widget::ContextMenu` separators run full-bleed and the highlighted item is an accent wash (`file-explorer/after/dark/context-menu.png`, `ui/src/widget/context_menu.rs:522,600-608`).
- **F3** `ui::widget::page_sidebar::PageSidebar` (pdf thumbnail rail) paints its own panel; not re-checked here beyond the stills.
- **F4** `ui::status_bubble::draw_pill` (ebook page bubble, pdf page bubble, Files status bubble) still strokes `border_standard` (`ui/src/status_bubble.rs:164`; `ebook/after/dark/page-bubble.png`).
- **Unit 0 / launch** `--headless-drive` refuses apps with a custom `open_state` opener (`ui/src/launch.rs:730` "History requires explicit storage provenance for a custom opener"), so Files has no inspection JSON; Files' gaps are asserted by the two new `#[cfg(test)]` geometry tests instead.
- **Toolkit promotion** `apps/ebook/src/chrome.rs` and `apps/pdf_viewer/src/chrome.rs` carry the same `panel`/`row`/`quiet_pill`/`bar_edge`/`pointer_target` painters; they belong in `ui::surface`/`ui::chrome` next to `card`/`separator` so the next app does not copy them a third time. Likewise `selection_tier` lifts from `surfaces.base` only — a ground-aware `selection_tier_on(theme, ground, state)` would let rows on a raised panel (the pdf sidebar) use the tier directly instead of the `card(.., selected)` stand-in, and would let a rail or sidebar sit on a lifted ground again.

## Limits and known pre-existing

- ebook Linux-only visual baselines and the file-explorer fixture race / German-locale asserts are pre-existing and untouched (see test output).
- Places-sidebar labels are not ellipsised; a long favourite name (`file-explorer/after/dark/history.png`) runs to the pill's inset. Pre-existing; not a touching pair between two controls.
- The reader's page surface, highlights and cover art keep the reading palette / cover tints (content, not chrome); the colour picker's SV/hue cursors keep their neutral white/black marks.
- pdf `render.rs` (page card, thumbnails, note markers) is document rendering and was not changed.
