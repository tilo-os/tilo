# F1 — Toolkit controls: derived rest edges, one knob colour, contrasting slider thumb

Unit F1 of the 2026-09-17 1×-first UI fix pass. Base `fd1f84391` (Unit 0 on
`ux1x/unit0`). Branch `ux1x/f1-controls`, local only; not pushed.

## Scope

Owned and edited: `ui/src/widget/{button, toggle, slider, checkbox, chip,
segmented, toggle_group, text_input, search_field, number_input, dropdown,
date_picker, time_picker, color_picker, autocomplete, input_hint}.rs`,
`ui/examples/widget_snapshot.rs` (light path). Owned, inspected, unchanged:
`shortcut_recorder.rs` (paints only hover/focus rings; its keycap comes from
`input_hint.rs`, fixed there), `editable_label.rs` (no edges), `undo_button.rs`
(paints through `ButtonFace`, inherits the Button fix), the other owned
examples and tests (`filled_control_labels_are_readable`,
`f2_control_row_alignment`, `textinput_local_mode`, `colorpicker_hex_input`
— run, not edited).

Not changed, per the brief: control heights, default radii, hit rects,
`face_colors` / `toggle_face_colors` / `segment_colors` signatures, builder
APIs, layout. `git diff --stat`: 17 files, +380/−169.

## Six-rule checklist

| # | Rule | Result | Evidence |
|---|---|---|---|
| 1 | No two elements touch or overlap | Pass for owned controls. Every peer pair in the gallery columns measures 12 px (`s3`) or 11 px (text line-box rounding), see the pair table. `touching_peers` in the harness audit names only `Bold/Italic`, `Link/Code` — Toolbar items, F3's file. | `after/harness/gallery-{dark,light}.inspection.json` + `geometry_audit` |
| 2 | Every surface has a visible derived edge | Pass. 0 `border_standard`/`border_strong`/`*_white_*`/`neutral[]`/literal-white hits remain in the 19 owned widget files (was 31 declared-border hits + 7 veneers + 1 literal white). Every rest edge is `ui::surface::edge_on(fill)`; the two list popups are `chrome::panel_ground` + `chrome::panel_border`. | `chrome_declared_border` ratchet: 14 owned files now 0 (all `stale`, none `grew`) |
| 3 | One spacing rhythm | Pass, unchanged: the gallery's Row/Column defaults (`s3` siblings) are the only rhythm on these surfaces; separators inside Segmented/ToggleGroup now stop `spacing.s1` short of the edge. | pair table |
| 4 | Content centred on measured line boxes | Unchanged and verified in the AFTER crops: labels in Button, Toggle, Checkbox, Segmented, ToggleGroup, TextInput, Dropdown, NumberInput sit on the row centre line. No fixed offsets were added. | AFTER crops |
| 5 | No "authored pixel" exemption | The slider's two literal `3.0` radii became `track_rect.height() / 2.0`; the separator inset is `theme.tokens.spacing.s1`. No new pixel constants (`f1_spacing_token_audit` clean). | `f1_spacing_token_audit` pass |
| 6 | Visual evidence | 30 BEFORE and 31 AFTER PNGs, dark and light, every one opened with the Read tool and described below. | `target/ux1x/f1-controls/{before,after}/` |

## BEFORE — defects per PNG (1×, opened and inspected)

| PNG | Defects (rule) |
|---|---|
| `before/gallery/gallery-dark.png` (crop `gallery-controls-dark`) | Save/Discard stroke `border_standard #2B2C33` on raised `#1F2025` — no visible edge (2). Wi-Fi knob dim blue-grey, Bluetooth knob light grey — the knob changes colour with the state (2, "interactive things look interactive"). Off track `neutral[3]` is a ramp slot, one grey per theme (2). Slider thumb `surface_white_10` = a grey blob on the dark track, ring `border_white_20` invisible (2). Day/Week/Month outer edge and separators `border_standard`, separators run full height into the corner radius (2). |
| `before/gallery/gallery-dark.png` (crop `gallery-inputs-dark`) | Text field, search pill, dropdown field, `− 12 px +` steppers all stroke `border_standard` on raised: edge barely registers (2). Search field's ⌘K keycap (not in this frame) is literal `rgba(255,255,255,16)`. |
| `before/gallery/gallery-dark.png` (crop `gallery-chips-dark`) | Quiet chips "Filter", "alice@example.com": `surface_white_5` + `border_standard` (2). |
| `before/gallery/gallery-light.png` (crop `gallery-controls-light`) | Buttons fine (light `border_standard` `#DEDEDB` registers). Toggle knob: near-white on blue, dark on grey — flips (2). Slider thumb: near-white with grey ring on a light-grey track — all but invisible (2). |
| `before/gallery/gallery-light.png` (crops `inputs-light`, `chips-light`) | Edges register on light; the defect is that they are declared, so they only work on one preset (2). |
| `before/harness/gallery-{dark,light}.png` | Same scene as the gallery via `FrameHarness`; identical findings. `geometry_audit`: 148 peer pairs, 0 overlaps, `touching_peers` only Toolbar `Bold/Italic`, `Link/Code` (F3). |
| `before/denied/denied-{dark,light}.png` | "Save draft" Neutral edge invisible on dark (2). Off toggle track `neutral[3]`; unchecked checkbox body `bg_active` (the pressed tier at rest) with invisible edge (2). The rectangular scrim around the denied checkbox/toggle is `draw_with_presence` in `widget/mod.rs`, not owned. |
| `before/search/search-dark-{empty,populated,selected}.png`, `search-light-populated.png` | Search pill edge `border_standard`, ⌘K keycap literal white (2). Result list panel is `search_results.rs` (F4). |
| `before/textinput/{multiline,secret,structured-fields,writing-assistance}.png` (dark only — the example has no theme switch) | Rest field edge `border_standard` on raised: the unfocused "Web address"/"File path" fields read as unbounded (2). |
| `before/undo/undo-{dark,light}-{empty,undoable,redoable,hovered}.png` | Undo/redo are Ghost faces (transparent at rest, `bg_hover` on hover): no rest edge by design (Selector-like); the "disabled" (empty) state was only a dimmer glyph — rest ≠ disabled held through the label alone. |
| `before/snapshot/widget-snapshot-dark.png` | ToggleGroup edge/separators declared; DatePicker a raised rectangle with no edge on a base ground (fine there, invisible on a raised card); TimePicker value cells no rest edge; ColorPicker hue strip, preview and RGB tracks `border_standard`, RGB tracks `raised`-on-`raised`. |
| `before/snapshot/widget-snapshot-light.png` | Written by the dark-only example (QOS_SHOTS_THEME ignored) — byte-for-byte the dark frame. Fixed in this unit. |

## Fixes (HIG §9.5 "Rest edges" table)

| Control | file:line (base) | Before | After |
|---|---|---|---|
| Button Neutral | `button.rs:487` | `border_standard` | `edge_on(raised)`, held across hover/press |
| Button disabled (all styles) | `button.rs:469`, `paint` 918, retained draw 1259 | raised + `border_standard` + `ink_dim`; **no** alpha layer, so rest ≈ disabled | neutral rest face (`edge_on(raised)`, primary ink) under `DISABLED_ALPHA` in both painters; cache carries the layer |
| Toggle track | `toggle.rs:241` | off `neutral[3]` | off `colors.lift(1.0)`, on `accent_primary` |
| Toggle knob | `toggle.rs:249-262` | resolved against the current track only → colour flips with state | `knob_ink`: one tone resolved against **both** tracks (`ink_across`), hover pushes the same tone; per-track fallback only where no single tone can clear 3:1 (see Limits) |
| Toggle face (shared + `on_color`) | `toggle.rs:228-268`, `319-339` | `toggle_face_colors` and `ToggleFace::colors` each spelled the track and knob resolution | one `face_for(theme, on_track, on, interact)`; `toggle_face_colors` = `face_for(.., accent_primary, ..)`, `ToggleFace::colors` = `face_for(.., on_color.unwrap_or(accent_primary), ..)`; public signatures unchanged |
| Toggle edge | `toggle.rs:266` | `border_standard` | `edge_on(track)`; focus `border_focus` 2 px |
| Slider track | `slider.rs:344-356` | `neutral[3]` + `border_standard`, radius literal `3.0` | unfilled `lift(1.0)`, edge `edge_on(unfilled)`, radius `track_rect.height()/2` |
| Slider thumb | `slider.rs:381-393` | five `surface_white_*`/`border_white_*` sites, fill changes per state | one thumb `ink_across([filled, unfilled], text_primary)`, edge `edge_on(thumb)`; hover = firmer edge (`PANEL_BORDER_ALPHA`) + radius; drag = derived halos (`hairline_on(unfilled, HAIRLINE_ALPHA/GLOW_ALPHA)`) |
| Checkbox | `checkbox.rs:313/328` | unchecked body `bg_active`, edge `border_standard` | unchecked body `surfaces.raised`, edge `edge_on(body)`; checked accent, no edge (self-defining, like Primary) |
| Chip quiet | `chip.rs:410`, `666-677` | `surface_white_5` + `border_standard` | immediate `paint`: `(raised, edge_on(raised))` (the ghost variant keeps its dashed stroke); retained draw: `ui::surface::card(.., raised)`; selected keeps accent tint + accent stroke, now on `edge_outline` |
| Segmented track / swatch panel | `segmented.rs:211/579` | `border_standard` on the rect boundary | `ui::surface::card(.., raised)` — fill + `edge_on(raised)` on the half-pixel-inset outline |
| Swatch hover ring | `segmented.rs:601` | `Color::WHITE` 2 px | `Colors::hairline_on(swatch, GLOW_ALPHA)` |
| ToggleGroup | `toggle_group.rs:469/477/681/697` | separators + outer `border_standard`, separators full height | `edge_on(raised)`; separators inset `spacing.s1` top and bottom |
| TextInput field | `text_input.rs:4457/4461` | rest and disabled both `border_standard`; disabled = `base` fill | rest `edge_on(raised)`; disabled = rest face under `DISABLED_ALPHA`; focus `border_focus` 2 px |
| TextInput gutter rule | `text_input.rs:1843` | `border_standard` | `edge_on(raised)` |
| SearchField pill | `search_field.rs:361` | `border_standard` | `edge_on(raised)` |
| SearchField ⌘K keycap | `search_field.rs:228/235` | `rgba(255,255,255,16)` + `border_standard` | `hairline_on(pill, HAIRLINE_ALPHA)` body + `edge_on(pill)` |
| Dropdown field | `dropdown.rs:774` | `border_standard` | `edge_on(raised)` |
| Dropdown list | `dropdown.rs:867-875` | `raised` + `border_standard` | Panel role: `panel_ground` + `panel_border` |
| NumberInput steppers | `number_input.rs:672` | `border_standard` | `edge_on(raised)` |
| Autocomplete list | `autocomplete.rs:680-687` | `raised` + `border_standard` | Panel role: `panel_ground` + `panel_border` |
| InputHint keycap / controller body | `input_hint.rs:69/614/618` | `neutral[2]`, `neutral[5]` | keycap `ui::surface::card(.., lift(1.0))`; controller body `lift(1.0)` |
| Stroke geometry (every owned rest edge and focus ring) | button, toggle, checkbox, slider, toggle_group, text_input, search_field, dropdown ×2, number_input, date_picker, time_picker, autocomplete | strokes centred on the rect boundary — half the hairline bled onto the ground | `ui::surface::edge_outline(rect, radius)`: rest edge and focus ring share one half-pixel-inset outline |
| ColorPicker | `color_picker.rs:536/584/627/683/702` | five `border_standard`; RGB tracks `raised` on `raised` | SV/hue `edge_on(raised)`, preview `edge_on(self.color)`, RGB tracks `lift(1.0)` + `edge_on(unfilled)`, knob `edge_on(text_primary)` |
| DatePicker | `date_picker.rs:352-367` | raised fill, edge only when focused | rounded fill + `edge_on(raised)` at rest |
| TimePicker cells | `time_picker.rs:517-530` | raised fill, edge only when focused | `edge_on(raised)` at rest |
| widget_snapshot | `examples/widget_snapshot.rs:594` | `Theme::dark()` always | honours `QOS_SHOTS_THEME` and `QOS_SHOTS_TEXT_SCALE` (`ui::launch::shot_text_scale`), reduce-motion on like every other capture |

Two tests in `button.rs` and two in `toggle.rs` pinned the old declared edge
and the `neutral[3]` track; each now asserts the derived value
(`edge_on(raised)`, `lift(1.0)`), justified by the AFTER PNGs below. One test
added: `toggle_knob_is_one_colour_for_both_states_where_a_tone_exists`.

## AFTER — per PNG (1×, opened and inspected; 36 PNGs under `target/ux1x/f1-controls/after/`)

| PNG | Result |
|---|---|
| `after/gallery/gallery-dark.png` (crop `gallery-controls-dark`) | Save/Discard carry a visible hairline on the raised card; Wi-Fi (on) and Bluetooth (off) knobs are the **same white**; off track is a lift with its own edge; slider thumb is white with a crisp edge on both halves; Day/Week/Month outer edge visible, separators stop `s1` short of the corners. Labels remain on the row centre line. |
| `after/gallery/gallery-dark.png` (crop `gallery-inputs-dark`) | Text field, search pill, dropdown field and the `−`/`+` steppers each show a 1 px derived edge inside their bounds; nothing bleeds onto the card. |
| `after/gallery/gallery-dark.png` (crop `gallery-chips-dark`) | "Filter" and "alice@example.com" chips are raised pills with a visible edge; selected "Rust" keeps its accent tint and accent stroke. |
| `after/gallery/gallery-light.png` (crop `gallery-controls-light`) | Buttons keep an edge (black at alpha 26 ≈ `#E5E5E5`); **both knobs near-black** — the on knob no longer flips to white; slider thumb near-black on the pale track with a visible edge; ToggleGroup edge and inset separators visible. No regression. |
| `after/gallery/gallery-light.png` (crops `inputs-light`, `chips-light`) | Every field and chip edge registers; identical geometry to BEFORE. |
| `after/harness/gallery-{dark,light}.png` + `.inspection.json` | Same frame via `FrameHarness`. `geometry_audit`: 148 peer pairs, 0 overlaps, `touching_peers` only Toolbar `Bold/Italic`, `Link/Code` (F3) — unchanged from BEFORE, as expected: this unit paints inside rects. |
| `after/denied/denied-dark.png` | Granted column: "Save draft" has a visible edge; off toggle is a lifted track with a white knob and edge; unchecked checkbox is raised with an edge. Denied column: same faces under the scrim, all still findable. (Scrim rectangle: `widget/mod.rs`, Open.) |
| `after/denied/denied-light.png` | Granted: white "Save draft" with visible edge, off toggle with near-black knob, edged checkbox. Denied: dimmed correctly. No regression. |
| `after/search/search-dark-{empty,populated,selected}.png`, `search-light-populated.png` | Search pill edge is derived (focused frames show `border_focus`; the pill's rest edge is exercised in the gallery). Result list panel unchanged (F4). |
| `after/textinput/{multiline,secret,structured-fields,writing-assistance}.png` | Unfocused "Web address"/"File path" fields now have a visible bound; focused field keeps its 2 px `border_focus`, now on the same inset outline. |
| `after/undo/undo-{dark,light}-{empty,undoable,redoable,hovered}.png` | Byte-identical to BEFORE: Ghost faces have no rest edge by policy (Selector-like); disabled glyph dimming unchanged. |
| `after/snapshot/widget-snapshot-dark.png` | ToggleGroup: edge + inset separators. DatePicker: rounded raised panel with a derived edge. TimePicker: each value cell edged. ColorPicker: hue strip, preview swatch, RGB tracks (now `lift(1.0)` + edge, distinct from the raised ground) and knobs all edged. |
| `after/snapshot/widget-snapshot-light.png` | **A real light frame** (was a copy of dark): white DatePicker/TimePicker cells with visible edges on the `#F3F3F1` ground, ToggleGroup edge, RGB tracks distinct from the panel. With motion reduced the TreeView fixture now shows its fully expanded `src/` (main.rs, lib.rs) — the still no longer catches an expansion mid-animation. |

## Pair table (gallery, from `harness/gallery-dark.inspection.json`; light identical)

Geometry is unchanged by this unit (edges are painted inside the rect), so
before = after for every pair.

| surface (PNG) | element A | element B | axis | gap px before → after | relationship / step | OK |
|---|---|---|---|---|---|---|
| gallery-controls | Button "Save" (36..156) | Button "Discard" (168..288) | x | 12 → 12 | siblings `s3` | ✓ |
| gallery-controls | Toggle Wi-Fi (36..80) | label "Wi-Fi" (92..) | x | 12 → 12 | control ↔ label, Row gap `s3` | ✓ |
| gallery-controls | Toggle Bluetooth (459..503) | label (515..) | x | 12 → 12 | `s3` | ✓ |
| gallery-controls | Checkbox (36..68) | label (80..) | x | 12 → 12 | `s3` | ✓ |
| gallery-controls | buttons row (bottom 221) | toggle row (top 233) | y | 12 → 12 | rows `s3` | ✓ |
| gallery-controls | toggle row (265) | checkbox row (277) | y | 12 → 12 | `s3` | ✓ |
| gallery-controls | "Volume" label (bottom 338) | Slider (top 349) | y | 11 → 11 | label ↔ control (Column `s3`, 17.x px line box) | ✓ |
| gallery-controls | Slider (381) | ToggleGroup (393) | y | 12 → 12 | `s3` | ✓ |
| gallery-controls | Toggle knob | track edge | inset | 2 → 2 | containment (`KNOB_EDGE_GAP`), not a peer pair; hit rect unchanged | ✓ |
| gallery-controls | ToggleGroup segment | segment | x | 0 → 0 (shared edge, separator inset `s1` = 4) | deliberate shared edge of one control | ✓ |
| gallery-inputs | label "Text field" (157) | TextInput (169) | y | 12 → 12 | label ↔ control `s3` | ✓ |
| gallery-inputs | TextInput (209) | label "Search field" (221) | y | 12 → 12 | `s3` | ✓ |
| gallery-inputs | SearchField (288) | label "Dropdown" (300) | y | 12 → 12 | `s3` | ✓ |
| gallery-inputs | Dropdown (368) | label (380) | y | 12 → 12 | `s3` | ✓ |
| gallery-inputs | NumberInput (440) | Breadcrumb (452) | y | 12 → 12 | `s3` | ✓ |
| gallery-inputs | NumberInput − (stepper) | value field | x | 0 → 0 | one control's segments share an edge (`radius` per side) | ✓ |
| gallery-chips | Chip ↔ Chip (110→122, 169→181, 230→242) | | x | 12 → 12 | siblings `s3` | ✓ |
| gallery-chips | Badge ↔ Badge | | x | 12 → 12 | `s3` | ✓ |
| gallery-chips | Avatar ↔ Avatar | | x | 12 → 12 | `s3` | ✓ |
| gallery-chips | Chip dot (leading) | Chip label | x | 6 → 6 | icon ↔ label (`icon_label_gap`) | ✓ |
| button (all) | icon | label | x | 6 → 6 | `ICON_LABEL_GAP` = `metrics.icon_label_gap` | ✓ |

## Code review (`/code-review medium`, reuse pass relayed by the coordinator)

Fixed in this diff: (a) the no-focus fill+stroke sites — Segmented track and
swatch panel, InputHint keycap, the unselected Chip — now go through
`ui::surface::card`, which strokes the half-pixel-inset outline (a stroke on
the rect boundary bleeds half the hairline onto the ground); (b) every other
owned rest edge *and* focus ring is stroked on `ui::surface::edge_outline(rect,
radius)` so both share one geometry (button both painters, toggle, checkbox,
slider track, toggle_group both painters, text_input, search_field, dropdown
field and list, number_input, date_picker, time_picker, autocomplete);
(c) the toggle face is one `face_for(theme, on_track, on, interact)` —
`toggle_face_colors` and `ToggleFace::colors` are one call each and `lift(1.0)`
is spelled once; (d) `widget_snapshot` honours `QOS_SHOTS_TEXT_SCALE` via
`ui::launch::shot_text_scale(None, env)` and reduces motion like every other
capture. The toggle geometry test that painted the expected pill by hand was
updated to the shared outline. Recorded for Unit 0 (see Open): `blend_over` vs
`composite`, the two alpha ladders, `edge_on_with`, and folding `ink_across`
into `Colors::ink_on_any`.

## Ratchets

`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`
(baselines are coordinator-only; no worker edits). **No `grew` line for any
owned file.** `f1_spacing_token_audit` passes (no new pixel constants). The
three failing audits are the expected stale entries plus the pre-existing
app/shell failure the brief excludes:

```
theme_escape_hatches_do_not_grow — 4 baseline entr(ies) in tests/design_token_baseline.txt are higher than the tree:
src/widget/chip.rs [theme_blind_white]: now 0, baseline still 2 — lower it to 0
src/widget/search_field.rs [literal_color]: now 0, baseline still 1 — lower it to 0
src/widget/segmented.rs [white_or_black]: now 1, baseline still 2 — lower it to 1
src/widget/slider.rs [theme_blind_white]: now 0, baseline still 5 — lower it to 0

declared_borders_in_chrome_do_not_grow — 14 entr(ies) in tests/chrome_declared_border_baseline.txt are higher than the tree:
ui/src/widget/autocomplete.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/widget/button.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/checkbox.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/widget/chip.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/color_picker.rs [declared_border]: now 0, baseline still 5 — lower it to 0
ui/src/widget/dropdown.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/input_hint.rs [declared_border]: now 0, baseline still 3 — lower it to 0
ui/src/widget/number_input.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/widget/search_field.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/segmented.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/slider.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/text_input.rs [declared_border]: now 0, baseline still 3 — lower it to 0
ui/src/widget/toggle.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/toggle_group.rs [declared_border]: now 0, baseline still 4 — lower it to 0

app_and_shell_theme_escape_hatches_do_not_grow — pre-existing at fd1f84391, not this unit:
apps/browser/src/browser_tabs.rs [literal_color] 1, [white_or_black] 1; shell/src/shell/frame.rs [literal_color] 1, [white_or_black] 2
```

Net: 31 declared-border hits → 0 across 14 owned files; 7 `theme_blind_white`
→ 0; 1 literal white → 0; `segmented.rs` `white_or_black` 2 → 1 (the remaining
hit is `Color::WHITE` as the *authored* label tone fed to `ink_on_any`, not a
paint colour).

## Tests

All with the inline env prefix and `--no-default-features --locked`.

```
cargo test -p ui --lib -- button toggle slider checkbox chip segmented toggle_group text_input search_field dropdown number_input color_picker
test result: ok. 441 passed; 0 failed; 1 ignored (pre-existing); 0 measured; 3226 filtered out

cargo test -p ui --test it -- filled_control_labels_are_readable f2_control_row_alignment textinput_local_mode colorpicker_hex_input design_token_audit f1_spacing_token_audit
test result: FAILED. 23 passed; 3 failed
  passed: filled_control_labels_are_readable ×5, f2_control_row_alignment ×6, textinput_local_mode ×2,
          colorpicker_hex_input ×4, f1_spacing_token_audit, design_token_audit (the non-ratchet checks)
  failed: design_token_audit::{theme_escape_hatches_do_not_grow, declared_borders_in_chrome_do_not_grow}
          — stale-only, pasted above; coordinator refreshes the baselines
          design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow — pre-existing browser_tabs.rs / frame.rs

cargo check -p ui --tests --examples   Finished
```

Tests updated to the new derived values (never weakened): `button.rs`
`face_colors_neutral_*` and `disabled_*` (edge = `edge_on(raised)`),
`toggle.rs` `toggle_face_colors_track_tracks_on_state` (off = `lift(1.0)`),
`toggle_face_colors_focus_swaps_border_and_weight` (rest = `edge_on(track)`),
`retained_draw_paints_the_documented_pill_geometry` (stroke on `edge_outline`).
Added: `toggle.rs::toggle_knob_is_one_colour_for_both_states_where_a_tone_exists`.

## Open — needs another unit

| Item | Where | Evidence |
|---|---|---|
| Toolbar items touch: `Bold/Italic`, `Link/Code` gap 0 px on x. **Not the ToggleGroup** (F4 recorded it as `Open — needs F1`): the inspection places all four targets under `Scrollable[0]/Column[0]/Row[2]/Container[1]/…/Toolbar[0]` — `Bold` x 608..654.7, `Italic` 654.7..701.9, `Link` 718.9..763.8, `Code` 763.8..815.2 at y 1208..1247 — i.e. `Toolbar::push_button` items in the "Navigation & Toolbar" panel (`gallery_scene.rs:630`). The gallery's ToggleGroup is `Day/Week/Month` (`gallery_scene.rs:419`) and reports no touching pair. | `ui/src/widget/toolbar.rs` / `ui/src/chrome/toolbar.rs` — **needs F3** | `after/harness/gallery-{dark,light}.inspection.json` `touching_peers`; `gallery-dark.png` Navigation & Toolbar panel |
| ToggleGroup / Segmented segments share an edge by design (edge-policy row "Segmented / ToggleGroup track": one track, segments tile it). Answer (a): the boundary between segments *is* the separator — painted `edge_on(raised)` inset `spacing.s1` top and bottom in both the retained draw and `paint_style_layer`, visible in `after/crops/gallery-controls-{dark,light}.png` and `after/snapshot/widget-snapshot-{dark,light}.png`. No `s1` gap is introduced; the group stays one control. Would report as `touching_peers` only if the lint were extended to segments, which it is not today. | `ui/src/widget/toggle_group.rs` — resolved in this unit, recorded for F4's item | pair table rows "ToggleGroup segment ↔ segment" |
| Gallery controls without accessible names (`Checkbox[0]`, `Slider[0]`, `ToggleGroup[0]`, `TextInput[0]`, `NumberInput[0]`, `TabBar`, `Toolbar`) | `ui/examples/common/gallery_scene.rs` — **needs F3** | `geometry_audit` `accessible_label` failures (both themes) |
| Denied-state scrim is a square darker rectangle around rounded controls | `ui/src/widget/mod.rs` `draw_with_presence` — **needs coordinator** (file off-limits to workers) | `before/denied/denied-dark.png` right column |
| Search result list panel edge | `ui/src/widget/search_results.rs` — **needs F4** | `after/search/search-dark-populated.png` |
| `textinput_ext_shots` and the search showcase render dark only / one light frame | examples are F1-owned but the theme switch is out of this unit's time; recorded so the coordinator's integration capture knows the light frames are missing | `before/textinput/*.png` |
| High-contrast `accent_primary` `#0099FF` measures 2.9995:1 against white | `ui/src/theme.rs:677` (tokens) — **needs coordinator**: nothing painted on that accent can reach `NON_TEXT_MIN_CONTRAST` (3.0) with light ink; the toggle falls back to a per-track knob there | `cargo test -p ui --lib -- toggle` output before the fallback |
| ToggleGroup / Segmented selected segment stays an accent fill (`segment_colors` is public API; `segment_colors_solid_selected_is_accent_and_readable` pins it) rather than `glass_selector` | `toggle_group.rs`, `segmented.rs` — policy note for the coordinator | — |
| `surface::blend_over` duplicates `tokens::colors::composite` | `ui/src/surface.rs` — **needs coordinator (Unit 0)** | reuse review |
| `surface::EDGE_ALPHA_STEPS` ladder and `chrome::shell::PANEL_GROUND_STEPS` / `PANEL_MIN_DELTA` are two spellings of one "walk alpha until it registers" rule | `ui/src/surface.rs`, `ui/src/chrome/shell.rs` — **needs coordinator (Unit 0)** | reuse review |
| `surface::edge_on_with(surface, alpha)` is a bare alias of `Colors::hairline_on`; kept in use here (slider hover edge) so the coordinator can fold or keep it in one place | `ui/src/surface.rs` — **needs coordinator (Unit 0)** | reuse review |
| `toggle::ink_across` (two-ground ink with polarity chosen over *all* grounds) belongs in `Colors::ink_on_any`, whose hardest-ground polarity walks a knob into the other track on light; kept `pub(crate)` in `toggle.rs` (slider calls it) for the coordinator to fold in at integration | `ui/src/tokens/colors.rs:598` — **needs coordinator (Unit 0)** | `toggle.rs` `ink_across` doc comment; light `#2F6FD6` case |
| ColorPicker's five strokes still ride the rect boundary (`RoundedRect::from_rect`) rather than `edge_outline`; their rects are private fields set in `apply_layout` and the SV square carries the picker's focus ring — left for a follow-up in this file | `ui/src/widget/color_picker.rs` | — |

## Limits

- **One knob colour** holds on the shipped dark and light presets (asserted).
  On high contrast the tokens admit no single tone (white is 2.9995:1 on the HC
  accent over a black off-track), and a bright semantic `on_color` over the
  dark off-track likewise straddles too far; there the knob resolves against
  the track it rides (the base behaviour) so it stays ≥ 3:1 — legibility beats
  constancy. Documented in `knob_ink`.
- Disabled buttons now dim the whole face (`DISABLED_ALPHA`), as every other
  control already did. On light the dimmed label composites to ≈ 2.9:1; WCAG
  exempts inactive controls and the face's own colours still clear
  `ON_FILL_MIN_CONTRAST` (asserted).
- The denied-state scrim and the Toolbar touching pairs are visible in the
  captures but belong to files this unit may not edit.
