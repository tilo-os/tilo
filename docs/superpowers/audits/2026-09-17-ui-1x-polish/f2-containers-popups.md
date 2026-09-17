# F2 — Containers and popups: Panel-role popups, derived card edges, inset separators

Branch `ux1x/f2-containers-popups`, cut from Unit 0 (`fd1f84391`). Base commit for all
BEFORE captures: `fd1f84391`. Evidence lives under `target/ux1x/f2/{before,after}/`
(PNG + inspection JSON) — every PNG named below was opened and inspected at 1×.

Scope: `ui/src/widget/{container,card_composition,divider,context_menu,popover,popover_widget,
tooltip,toast,modal_dialog,modal_frame,slash_menu,link_popover,page_palette,command_palette,
permission_prompt,file_picker,floating_window,conflict_banner,conflict_picker}.rs`,
`ui/src/widget/identity_flow/sheet.rs`; examples `card/form/manual_composition_shots.rs`;
tests `file_picker_render_test, toast_test, overlay_test, command_palette_test,
f3_composition_fixtures`. `card.rs`, `composition*.rs`, `form*.rs`, `group_row.rs`,
`banner.rs`, `empty_state.rs`, `modal_prompt.rs`, `merge_strategy.rs`, `resource_actions.rs`
were inventoried and paint no declared border (Banner and EmptyState paint a semantic
accent tint + accent stroke, which is a contrast step, not a declared border) — unchanged.

## Six-rule checklist

| # | Rule | Result | Evidence |
|---|---|---|---|
| 1 | No two elements touch or overlap | **Met.** Two real defects found and fixed: the PermissionPrompt label ran under the button column on a narrow strip (`permission_prompt.rs` label block now ends `s3` before the leftmost button and is elided); the Toast severity stripe started at `x0 + 1.5` and touched the 1 px edge (now `x0 + 1 + s1`, vertical inset `s2`). FilePicker idle rows painted `raised` — invisible on the old `raised` panel, a stack of touching bands once the panel took `panel_ground` — now transparent (HIG §9.3). `geometry_audit` on six inspections (cards/forms/manual × dark/light): `failures: 0`, no `touching_peers`. | pair table below; `after/fixtures/permission_prompt_{dark,light}.png`, `toast_card_{dark,light}.png`, `file_picker_open_{dark,light}.png` |
| 2 | Every surface has a visible derived edge | **Met** for every owned surface (table below, Δ relative luminance measured on the PNG ≥ `HAIRLINE_MIN_DELTA` 0.02 in both themes). No `border_standard` / `border_strong` / `*_white_*` / `neutral[]` / literal-colour edge remains in owned files except `Divider`'s default, which HIG §9.5 keeps as a rule on `surfaces.base` (`Divider::on(surface)` added for any other ground). | surface table; `declared_border` ratchet now 0 for 18 owned files |
| 3 | One rhythm per panel | **Met, layout unchanged** (the brief forbids padding/layout changes). The only spacing token change is the context-menu separator inset `s2` → `s3` (HIG §9.5). Every remaining fixed offset inside a popup is listed in the pair table with its step; none produces a touch. | pair table |
| 4 | Content centred on measured line boxes | **Met / unchanged.** Context menu, tooltip, toast, modal, permission prompt and file-picker rows already centre labels on `measure()` line boxes; the new captures show it in both themes. | fixture PNGs |
| 5 | No authored-pixel exemption | **Met.** Offsets that produced a defect were replaced by token expressions (toast stripe `s1`/`s2`, permission label `s3`); half-pixel placement that smeared the tooltip hairline across two columns at half strength is snapped to the grid before painting; the palette footer rule is centred on `+0.5` so it fills one pixel row. | measurements below |
| 6 | Visual evidence, dark and light | **Met.** 84 BEFORE and 190 AFTER PNGs on disk (48 dark+light fixture pairs); the ones listed below were each opened with the Read tool and described. Composition examples honour `QOS_SHOTS_THEME` through `Theme::named` (light, high-contrast, every preset — not a `light`-only branch, per review); file picker, toast, modal, popover, command palette, permission prompt and conflict surfaces gained dark+light capture tests that also assert the edge registers. | `target/ux1x/f2/**` |

## Surface table (before → after, 1×)

Δ = |relative luminance(edge pixel) − relative luminance(fill)| measured on the PNG with
`target/ux1x/f2/pngsample.py`; the HIG floor is 0.02. "smeared" = the old centred stroke
straddled two pixel columns, each at partial coverage.

| Surface | Defect (file:line at fd1f84391) | Fix | Dark Δ before → after | Light Δ before → after | PNGs (before / after) |
|---|---|---|---|---|---|
| Container (`has_background`) | `container.rs:310/449` `bg_primary` fill + `border_standard` | `surface::card(fill)` — edge derived from the fill, crisp inset | `#25262c`/`#1f2025` 0.005 → `#36373b` 0.024 | n/a (example was dark-only) → `#e5e5e5`/`#ffffff` 0.217 | `before/dark/manual/manual-wide.png` → `after/{dark,light}/manual/manual-wide.png` |
| ContentCard (`Card::composed`) | `card_composition.rs:165` | `surface::card(bg_primary)`; header/body/footer inside get space, not a second edge | 0.005 → 0.024 (`#36373b`) | → 0.217 (`#e5e5e5`) | `before/dark/cards/wide.png` → `after/{dark,light}/cards/wide.png` |
| ContextMenu | `context_menu.rs:522` `border_standard` on `overlay`; separator `600-608` full-bleed-ish `border_standard` inset `s2` | Panel role: `panel_border(theme)` on `edge_outline`; `surface::separator(.., s3, overlay)` | smeared `#212227`/`#1c1d22` → crisp `#646567` on `#0d0e11` 0.126 | smeared `#e9e9e6`/`#efefed` → `#a3a3a3` on `#ffffff` 0.634 | `before/fixtures/f4_context_menu_hover_{dark,light}.png` → `after/fixtures/…` |
| Popover (list) | `popover.rs:386` `raised` + `border_standard` | `panel_ground` + `panel_border` inset | by tokens: 0.011 → panel edge (alpha 92) | by tokens | no PNG (see Limits); unit tests `popover` pass |
| PopoverWidget | `popover_widget.rs:399` | `panel_border` inset; fill stays `overlay` | test asserts ≥ 0.02 both themes | " | — → `after/fixtures/popover_widget_{dark,light}.png` (new) |
| Tooltip | `tooltip.rs:113` | `surface::card(overlay)` + snap chip to whole pixels | `#282a31` 0.019 (2-px smear) → `#333436` 0.030 crisp | `#e0e0de` → `#e5e5e5` 0.217 | `before/fixtures/tooltip_over_icon_button_{dark,light}.png` → `after/fixtures/…` |
| Toast card | `toast.rs:469` `raised` + `border_standard`; stripe at `x0+1.5` touching the edge | `panel_ground` + `panel_border` inset, both fading with `alpha`; stripe `x0 + 1 + s1`, inset `s2` | — → `#7d7d7f` on `#333436` 0.171 | — → `#999999` on `#efefef` 0.545 | — → `after/fixtures/toast_card_{dark,light}.png` (new) |
| ModalDialog | `modal_dialog.rs:581` | `panel_ground` + `panel_border` inset | test asserts ≥ 0.02 | " | — → `after/fixtures/modal_dialog_{dark,light}.png` (new) |
| ModalFrame (identity sheets) | `modal_frame.rs:540`; `card_fill` = `raised` | `card_fill` → `panel_ground`; `panel_border` | visible rim in `identity_recovery_verify_dark.png` | `…_light.png` | `after/fixtures/identity_recovery_verify_{dark,light}.png` |
| SlashMenu | `slash_menu.rs:492`; filter field had no edge | `panel_ground` + `panel_border`; filter `edge_on(base)` inset | visible | visible | `before/fixtures/f4_slash_menu_hover_dark.png` → `after/fixtures/f4_slash_menu_hover_{dark,light}.png` |
| LinkPopover | `link_popover.rs:368`; filter field no edge | same as SlashMenu | — | — | no PNG (see Limits); `link_popover_renders` pass |
| PageJumpPalette | `page_palette.rs:285` | `panel_ground` + `panel_border` inset (stroke scales with text) | visible | visible | `before/fixtures/f4_page_palette_cursor_dark.png` → `after/fixtures/f4_page_palette_cursor_{dark,light}.png` |
| CommandPalette | `command_palette.rs:2060` card `border_standard`; `2083` input rest ring; `1433` footer rule `border_standard·0.6` inset 4 px; `1467/1474` `surface_white_5`/`border_white_10` key chips; `1315` literal `white/5` idle scope chip; `1346/1347` Tailwind emerald | `panel_ground` + `panel_border` inset; input `edge_on(base)`; `surface::separator(.., s3, ground)` on the half pixel; idle scope/filetype chips and key chips: transparent fill + 1 px `edge_on(panel_ground)` (Selectors idle none, HIG §9.5 — a base-relative `lift(1.0)` read as a well on the darker dark-preset ground, per review); active accent, no edge; FULL TEXT `semantic.success` | `#2b2c33`/`#1f2025` 0.011 → `#7d7d7f`/`#333436` 0.171; footer rule 0.014 (2 rows) → `#48494a` 0.032 (1 row); chip hairline = `edge_on(#333436)` ≥ 0.02 | → `#999999`/`#efefef` 0.545; chips outlined on the light ground | — → `after/fixtures/command_palette_{dark,light}.png` (new) |
| PermissionPrompt | `284/286` button `border_standard`; `391` panel `border_standard`; label overlapped buttons on a narrow strip | button `edge_on(face)` (disabled at the face's 0.4), panel `panel_border`; label elided to `allow_rect.x0 − s3` | test asserts ≥ 0.02 | " | — → `after/fixtures/permission_prompt_{dark,light}.png` (new) |
| FilePicker | `1024` frame; `1564` filename field; `1631` filter field; idle rows `raised` | `panel_ground` + `panel_border`; fields `edge_on(fill)`; rows idle transparent / hover `hairline_on(panel_ground, HAIRLINE_ALPHA)` wash (measured against the panel the row sits on — a base-relative `lift` lands below `panel_ground` on dark and would darken the row, per review) / selected accent | test asserts ≥ 0.02 | " | `before/fixtures/file_picker_open.png` (dark) → `after/fixtures/file_picker_{open,save}_{dark,light}.png` |
| FloatingWindow | `floating_window.rs:764` `border_standard·α` on the organic outline | Object role: `Colors::window_edge()·α` | `#27282e` 0.007 → `#3c3d41` 0.032 | `#e9e9e7` 0.186 → `#eaeaea` 0.177 | `before/{dark,light}/floating-surfaces.png` → `after/{dark,light}/floating-surfaces.png` |
| ConflictPicker | `411` card `border_standard`; keep button `817/819/831` white veneers | `surface::card(base)` (hover keeps the 2 px focus ring); keep button `raised`/`bg_hover` + `edge_on` | visible (`conflict_dark.png`) | chips/cards now edged on light | — → `after/fixtures/conflict_{dark,light}.png` (new, f3 fixture) |
| ConflictBanner | `238/240/246` chip veneers | `raised`/`bg_hover` + `edge_on` via `surface::card` | f3 test asserts chip face/edge registers | " | same |
| Divider | `divider.rs:125` default `border_standard` | default kept (rule on `surfaces.base`); `Divider::on(surface)` → `edge_on(surface)`; the resolved colour is `Divider::rule_color(theme)`, which `draw` paints, so the test asserts the production path (default / `.on` / `.with_color` precedence) rather than re-deriving the helper | — | — | unit tests `rule_color_follows_with_color_then_on_then_default`, `on_surface_still_emits_exactly_one_fill` |
| Identity code cell | `identity_flow/sheet.rs:741` | `edge_on(fill)` at rest, `border_focus` focused | visible | visible | `after/fixtures/identity_recovery_verify_{dark,light}.png` |

## Pair table (gap px before → after, relationship / step)

Measured on the AFTER PNGs unless marked "geometry".

| Surface (PNG) | Element A | Element B | Axis | Gap before → after | Relationship / step | OK |
|---|---|---|---|---|---|---|
| context menu (`f4_context_menu_hover_dark`) | panel edge stroke (x=40) | armed-row fill (x=44) | x | 4 → 4 (edge now 1 px, 3 px ground between) | highlight inset `s1` | yes |
| context menu | panel top edge (y=40) | row 0 top (y=46) | y | 6 → 6 | `PADDING` 6 (authored, ≥ `s1`) | yes |
| context menu | armed row fill | neighbouring row | y | 1 px inset each side; one row armed at a time | row stack, `ROW_HEIGHT` 28 | yes |
| context menu (geometry) | separator rule | panel edge | x | 8 (`s2`) → 12 (`s3`) | HIG §9.5 separator inset | yes |
| context menu (geometry) | separator rule | adjacent rows | y | 4.5 above / 4.5 below (`SEPARATOR_HEIGHT` 9 centred) | unchanged | yes |
| context menu (geometry) | panel edge | label glyphs | x | 10 → 10 (`s1` + `LABEL_GAP` 6) | unchanged | yes |
| cards (`after/dark/cards/wide.png`) | card 1 right edge (x=355) | card 2 left edge (x=368) | x | 12 → 12 | siblings `s3` (`CardGrid` gap) | yes |
| command palette (`command_palette_dark`) | input field ring (y=68) | filter chips (y=80) | y | 11 → 11 | `PADDING` 8 + chip inset (authored) | yes |
| command palette | filter chips (y=107) | first result row (y=119) | y | 11 → 11 | siblings ≈ `s3` | yes |
| command palette | footer separator (y=364, 1 row) | key chip | y | chip centred in the 28 px footer, ≥ 6 above/below | related rows `s2` | yes |
| command palette | key chip edge (x=59) | its label (x=64) | x | 4 → 4 | icon↔label `s1` | yes |
| command palette (geometry) | footer separator ends | panel edge | x | 4 → 20 (`PADDING` 8 + `s3` 12) | separator inset `s3` | yes |
| toast (`toast_card_dark`) | card edge (x=20) | severity stripe | x | 0.5 (touching) → 4 (`s1`) | edge ↔ decoration | yes |
| toast (geometry) | stripe end (`x0+9`) | title glyphs (`x0+14`) | x | 8.5 → 5 | ≥ `s1` | yes |
| toast (geometry) | stripe | card top/bottom | y | 8 → `s2` (8) | related rows `s2` | yes |
| permission prompt (`permission_prompt_dark`, 600 px strip) | label block | leftmost button | x | overlapped (label ran under Allow) → 12 (`s3`, elided) | control ↔ label `s3` | yes |
| permission prompt (geometry) | icon | label | x | `s2` → `s2` | icon↔label | yes |
| file picker (`file_picker_open_dark`) | idle row | idle row | y | 0 (touching `raised` bands) → n/a (idle rows transparent) | list rows | yes |
| file picker (geometry) | panel edge | first row / header | x | `PADDING` 12 (`s3`) unchanged | section padding | yes |
| popover widget (`popover_widget_dark`) | panel edge | label | x | content inset unchanged (`s2`) | unchanged | yes |
| conflict (`conflict_dark`) | banner chip `Review` | chip `Dismiss` | x | 8 → 8 | siblings (authored `s2`) | yes |
| conflict | picker card | picker card | x | 12 → 12 | siblings `s3` | yes |

## Geometry lint

`python3 -c 'import sys,json; sys.path.insert(0,"scripts/ux-bench"); import usability; …geometry_audit…'`
over `after/{dark,light}/{cards/wide,forms/automatic-wide,manual/manual-wide}.json`:
all six report `failures: 0, passed: true` (5 measured controls, 10 peer pairs each; no
`touching_peers`, no `semantic_bounds_overlap_unresolved`).

## Captures opened (1×)

BEFORE (`target/ux1x/f2/before/`): `palette/ctrlk-{dark,light}-browse.png`,
`palette/ctrlk-dark-query.png`, `{dark,light}/floating-surfaces.png`, `dark/cards/wide.png`,
`dark/forms/automatic-wide.png`, `dark/manual/manual-wide.png`,
`fixtures/{f4_context_menu_hover_{dark,light},tooltip_over_icon_button_{dark,light},
file_picker_open,f4_slash_menu_hover_dark,f4_page_palette_cursor_dark}.png`.

AFTER (`target/ux1x/f2/after/`): the same names in both themes plus
`{dark,light}/{cards,forms,manual}/…` (light is new), `fixtures/{command_palette,
permission_prompt,toast_card,modal_dialog,popover_widget,conflict,file_picker_open,
file_picker_save,identity_recovery_verify}_{dark,light}.png`,
`fixtures/f4_{slash_menu_hover,page_palette_cursor}_{dark,light}.png`.

One-line findings per BEFORE PNG: palette dark — card edge invisible (0.011), input ring
accent (focused) fine; palette light — fine (light `border_standard` registers); floating dark —
window rim invisible (0.007); floating light — fine; cards dark — edge 0.005, invisible; forms —
no owned surface (fields/buttons are F1); manual dark — page Container edge invisible; context
menu dark — edge a 2-px smear, separator would be `s2`-inset `border_standard`; tooltip dark —
2-px smear 0.019; file picker dark — frame edge invisible, idle rows painted `raised` (hidden
only because the panel was also `raised`); slash/page palette dark — edge invisible.

Note on `ctrl_k_palette`: that example renders `ui::ActionPalette`, whose card is painted by
`SearchResultsView` (`search_results.rs:509/516`, unit F4) and `SearchField` (F1) — its pixels
are byte-identical before/after and are recorded as Open items below. `CommandPalette` itself
is captured by the new `command_palette_test` fixture.

## Ratchets

`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`:

- `f1_spacing_token_audit::spacing_literals_do_not_grow` — ok (no `grew`).
- `design_token_audit::declared_borders_in_chrome_do_not_grow` — no `grew`; **stale** (to be
  lowered by the coordinator, all now 0): `card_composition.rs 1`, `command_palette.rs 3`,
  `conflict_picker.rs 1`, `container.rs 2`, `context_menu.rs 2`, `file_picker.rs 3`,
  `floating_window.rs 1`, `identity_flow/sheet.rs 1`, `link_popover.rs 1`, `modal_dialog.rs 1`,
  `modal_frame.rs 1`, `page_palette.rs 1`, `permission_prompt.rs 3`, `popover.rs 1`,
  `popover_widget.rs 1`, `slash_menu.rs 1`, `toast.rs 1`, `tooltip.rs 1`. `divider.rs 1`
  stays (the `surfaces.base` default HIG §9.5 permits).
- `design_token_audit::theme_escape_hatches_do_not_grow` — no `grew`; **stale**:
  `command_palette.rs literal_color 3 → 0`, `command_palette.rs theme_blind_white 2 → 0`,
  `conflict_banner.rs theme_blind_white 3 → 0`, `conflict_picker.rs theme_blind_white 3 → 0`.
- `design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow` — pre-existing failure
  on `apps/browser/src/browser_tabs.rs` / `shell/src/shell/frame.rs` (not this unit).

## Tests

- `cargo test -p ui --lib -- container card context_menu popover tooltip toast modal slash_menu
  link_popover permission_prompt command_palette file_picker floating_window conflict divider
  identity_flow`: **461 passed, 0 failed, 1 ignored** (pre-existing `file_picker.rs:2478`
  developer visual audit).
- `cargo test -p ui --test it -- context_menu_test toast_test tooltip_test tooltip_render_test
  command_palette_test overlay_test file_picker_render_test f4_menu_highlight_render
  f3_composition_fixtures f6_identity_flow_layout link_popover_renders group_row_visual_journey
  container_update_forwarding design_token_audit f1_spacing_token_audit`:
  **57 passed, 3 failed** — the three failures are exactly the ratchet lines above
  (`theme_escape_hatches_do_not_grow` 4 stale, `declared_borders_in_chrome_do_not_grow` 18 stale,
  `app_and_shell_theme_escape_hatches_do_not_grow` pre-existing `browser_tabs.rs`/`frame.rs`);
  every owned functional test passes and no `grew` line was printed. Final run pasted:

  ```
  == lib set
  test result: ok. 462 passed; 0 failed; 1 ignored; 0 measured; 3206 filtered out
  == it set
  test design_token_audit::theme_escape_hatches_do_not_grow ... FAILED
  test design_token_audit::declared_borders_in_chrome_do_not_grow ... FAILED
  test design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow ... FAILED
  src/widget/command_palette.rs [literal_color]: now 0, baseline still 3 — lower it to 0
  src/widget/command_palette.rs [theme_blind_white]: now 0, baseline still 2 — lower it to 0
  src/widget/conflict_banner.rs [theme_blind_white]: now 0, baseline still 3 — lower it to 0
  src/widget/conflict_picker.rs [theme_blind_white]: now 0, baseline still 3 — lower it to 0
  ui/src/widget/{card_composition 1, command_palette 3, conflict_picker 1, container 2,
    context_menu 2, file_picker 3, floating_window 1, identity_flow/sheet 1, link_popover 1,
    modal_dialog 1, modal_frame 1, page_palette 1, permission_prompt 3, popover 1,
    popover_widget 1, slash_menu 1, toast 1, tooltip 1}.rs [declared_border]: now 0 — lower it to 0
  apps/browser/src/browser_tabs.rs [literal_color] 1 / [white_or_black] 1, baseline 0   (pre-existing)
  shell/src/shell/frame.rs [literal_color] 1 / [white_or_black] 2, baseline 0            (pre-existing)
  test result: FAILED. 57 passed; 3 failed; 0 ignored; 0 measured; 300 filtered out
  ```
- New tests: `divider::tests::on_surface_derives_a_visible_edge_in_every_preset`,
  `toast_test::toast_card_edge_registers_in_both_themes`,
  `overlay_test::{modal_dialog,popover_widget,permission_prompt}_panel_edge_registers_in_both_themes`,
  `command_palette_test::command_palette_panel_edge_registers_in_both_themes`,
  `f3_composition_fixtures::conflict_surfaces_keep_a_visible_chip_edge_in_both_themes`;
  `file_picker_render_test` now renders dark and light and asserts the panel edge.
- `cargo check -p ui --tests --examples`: clean (two pre-existing lib warnings in `record.rs`,
  `animation_schedule.rs`).

## Open — needs another unit

| Item | File:line | Needs | Evidence |
|---|---|---|---|
| Ctrl+K `ActionPalette` card: `raised` fill + `border_standard` (0.011 on dark) | `ui/src/widget/search_results.rs:509/516` | F4 | `before/palette/ctrlk-dark-browse.png` = `after/…` (byte-identical) |
| Ctrl+K query field chrome | `ui/src/widget/search_field.rs:228/235/361` | F1 | same PNGs |
| Form/manual composition fields, buttons, checkbox rest edges | `text_input.rs:4458/4462`, `button.rs:520/538`, `checkbox.rs:328` | F1 | `after/{dark,light}/forms/automatic-wide.png` |
| Card footer `Actions` buttons (Neutral face edge) | `button.rs:538` | F1 | `after/dark/cards/wide.png` |
| `surface::card` / `edge_outline` paint a centred-on-half-pixel stroke; a caller with a fractional rect gets a two-column smear (tooltip needed a local snap) — consider snapping in the helper | `ui/src/surface.rs:546-575` | Unit 0 / F4 (surface.rs) | tooltip measurements above |
| `surface::separator` centres on `y`; callers must pass `y + 0.5` for a one-row rule — document or snap | `ui/src/surface.rs:577` | Unit 0 / F4 | palette footer measurements |
| No shared `surface::panel(scene, transform, rect, radius, theme, alpha)`: the Panel sequence (fill `panel_ground`, 1 px `panel_border` on `edge_outline`) is now restated in 11 widgets with small drifts (toast fades with alpha, page_palette strokes `1·scale`, modal_frame strokes the full card) — review finding; needs a helper beside `surface::card` | `ui/src/surface.rs` | Unit 0 / F4 | this diff |
| Test helpers (`luminance_of`, PNG `dump`, edge-vs-fill assertion) are repeated in `toast_test`, `overlay_test`, `command_palette_test`, `file_picker_render_test`; the shared home is `ui/tests/support/` (not owned here) — review finding | `ui/tests/support/offscreen.rs` | coordinator | this diff |
| Children that fill `surfaces.raised` (shared Neutral `Button`, `TextInput`) sit *below* a `panel_ground` sheet on the dark preset (raised lum 0.015 vs ground 0.056) and *above* it on light; `panel_ground` for ModalFrame/ModalDialog follows the plan's Panel table, so the direction flip belongs to the control fills — controls inside a Panel should derive their floor from the panel ground (S1's `lift(0.35)` + `edge_on(panel_ground)` pattern), review finding | `button.rs:538`, `text_input.rs:4458` | F1 (and plan follow-up) | `after/fixtures/identity_recovery_verify_dark.png` (cells and buttons read as wells) |
| `chrome_declared_border_baseline.txt` / `design_token_baseline.txt` stale lines listed under Ratchets — refresh with `UPDATE_CHROME_BORDER_BASELINE=1` / `UPDATE_DESIGN_BASELINE=1` | `ui/tests/*baseline*.txt` | coordinator (workers must not edit) | ratchet output |

## Code review

`code-review high` (fork) returned nine findings. Fixed in this diff: clippy `cast_*` allows on the
four capture tests (matching every other ui render test) and the redundant `as u32`; rustfmt on
`permission_prompt.rs:289/418`, `overlay_test.rs`, `divider.rs` (remaining rustfmt diffs in
touched files are import-order lines that pre-exist at HEAD); `shot_theme()` in the three
examples resolves via `Theme::named` instead of a `light`-only branch; the Divider test now
asserts `Divider::rule_color` (the colour `draw` paints) instead of re-deriving the helper;
idle palette chips and key chips are transparent with an `edge_on(panel_ground)` hairline
instead of `lift(1.0)` (which is `raised`, a well on the dark ground); FilePicker hover is a
hairline wash on the panel ground instead of a base-relative `lift`. A follow-up
`cargo clippy -p ui --all-targets -- --cap-lints warn` (the uncapped run stops on pre-existing
`deny` errors in `ui/src/app.rs` and `history_mode/` before reaching these files) showed four
introduced sites, all fixed: `SlashMenu::draw_overlay` over 100 lines (chrome painting
extracted to `draw_chrome`), `theme.name.to_string()` → `.clone()` in f3, and the
`x_edge ± 6` probes → `saturating_add/sub` in the capture tests; the remaining hits in
touched files (`context_menu.rs:340`, `sheet.rs:673/1021`, `card_composition.rs:319-739`,
f3 helpers 118-204) pre-exist at HEAD. Whole-file `rustfmt` on `slash_menu.rs` also moved
one pre-existing `use` line. Recorded, not fixed (files outside this unit or a plan-level
decision): stale baselines, a `surface::panel` helper, shared test helpers, and the
raised-child inversion on `panel_ground` sheets (all above).

## Limits

- No PNG for `Popover` (list) and `LinkPopover`: neither has an offscreen fixture and both need
  a `BlockEditor`/anchor host to open; their fills and edges are the same code path as
  SlashMenu/PageJumpPalette (verified visually) and their unit tests pass.
- Context-menu separator: the f4 fixtures carry no `MenuItem::Separator`, so the `s3` inset is
  verified by geometry (`separator_inset = spacing.s3`) rather than a PNG.
- `panel_ground` on the dark preset is a mid grey (`#333436`) rather than the old `raised`
  `#1f2025`; this is the shared Panel material every shell panel already uses (power menu,
  quick settings) and is deliberate per HIG §9.2 — it changes the look of the palette, toast,
  modal, file picker and identity sheets more than a hairline would.
- The permission-prompt capture uses a 600 px strip to prove elision; on the shell's full-width
  strip the label is not elided.
- The menu-row hover state: ContextMenu/Popover/SlashMenu/PageJumpPalette keep the accent
  armed row (pinned by `f4_menu_highlight_render` to the exact theme accent); no separate
  hover-vs-armed tier was introduced, so "hover `lift(0.5)`" applies only to FilePicker rows.
