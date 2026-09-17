# A10 — video_editor: 1× visible edges, no touching, one rhythm

Branch `ux1x/a10-video-editor`, cut from Unit 0 `fd1f84391`. Files edited:
`apps/video_editor/src/app.rs`, `apps/video_editor/src/layout.rs` (new),
`apps/video_editor/src/app/chrome_geometry_tests.rs`, `apps/video_editor/src/lib.rs`.
Nothing outside `apps/video_editor` was touched.

Evidence: `target/ux1x/a10/{before,after}/{dark,light}/<scenario>.png` — every one of
the 36 declared `--shots` scenarios plus `live` and `empty`, at 1280×800, 1× text scale,
plus the four-viewport `--responsive-matrix` and `--headless-drive` geometry dumps
(`geo/live.inspection.json`), rendered under an isolated `$HOME`/XDG profile by
`target/ux1x/a10/capture.sh`. Every PNG below was opened with the Read tool.

## Six-rule checklist

| # | Rule | Evidence |
|---|---|---|
| 1 | No two elements touch or overlap | Track head ↔ lane stripe: 0 → 4 px (`layout::track_head_card`); kind line ↔ meter on a sound lane: 1 → 4 px (`AUDIO_LANE_H` 46 → 54 = `layout::MIN_LANE_H`); master strip readout ↔ meter 2 → 4 px; bin name ↔ metadata line 3 → 4 px; effect-row reorder pair 1 → 4 px; marker-row icons 2 → 4 px; anchor grid rows 3 → 4 px. Tests: `layout::tests::*`, `chrome_geometry_tests::{every_track_head_clears_its_lane_and_its_neighbours_by_a_token_step, the_bin_rows_and_the_row_controls_keep_token_gaps, the_topbar_commands_and_side_toggles_keep_one_sibling_step_at_every_width}`. PNGs: `after/{dark,light}/{live,mix,effects,markers,geometry}.png`. |
| 2 | Every surface has a visible derived edge or contrast step | 13 `border_standard` strokes → 0 (`pane_hairline` = `ui::surface::edge_on(raised)` for the five pane seams and the ruler ticks; `ui::surface::card` for lane stripes, track heads, swatches, meters, wells; `ui::chrome::{panel_ground,panel_border}` for the go-to card). 13 `neutral[3]` ramp fills → 0 (one `neutral[5]` remains: a clip *label* palette slot, content not chrome). Hand-painted buttons → `Button::paint` (transport discs, S/L/M pills, zoom chip, +Video/+Audio, PiP presets, hamburger, inspector toggle). Bin selection accent wash → `selection_tier`. PNGs: `after/{dark,light}/live.png` (pane seams, heads, stripes, meters), `after/light/live.png` (sound lanes now register on white). |
| 3 | One rhythm per screen | `layout.rs` `S1..S4` from `Spacing::BASE`; `TOPBAR_GAP` 10 → `S3`, `TOPBAR_EDGE`/`PAD` = `S3`, `LANE_GAP` = `S1`, bin row pitch = body + `S2`, add-track buttons `S2` apart and `S2` under the last lane, pills `S1` apart, PiP presets `S2` apart, key swatches `S1` apart, effect/marker row controls `S1` apart, anchor rows `S1` apart, segmented rows abut (one control, separators drawn by `ToggleGroup`) instead of 3 px slots inside one outline. |
| 4 | Content centred on measured line boxes | Bin row: name + metadata block measured (`line_height`) and centred in the row; the “Not on device” chip sits on the metadata line’s band. Track head: name and pills share one `HEAD_PILL` band (`text_baseline`), kind line and meter on their own bands. Master strip: `MIX`/peak readout and meter centred as one block (`layout::master_strip`). Segment labels centred by the shared face. |
| 5 | No authored-pixel exemption | Every changed offset is a token expression (`S1..S4`, `Metrics::BASE.icon_md`, `radii.sm/md/lg`) or derived from a measured band; `TIMELINE_H` 304 → 320 so the taller sound lanes and the add-track row still fit at 800 px; `pixel_const` count in `app.rs` fell 53 → 48 (spacing ratchet reports it stale, see below). |
| 6 | Visual evidence | 39 PNGs × 2 themes before and after, all opened; table below. Lint on the geometry dumps: 0 `touching_peers`, 0 `semantic_bounds_overlap_unresolved` (see limits — the canvas publishes one measured control). |

## Surfaces: before → after (dark and light)

| Surface (PNG) | Before — defects (rules) | After |
|---|---|---|
| Panes and seams (`live`) | Topbar/bin, topbar/inspector, ruler/side-pane seams stroked in `border_standard` — invisible on dark raised-on-raised (2); bin right edge, transport top, inspector left the same | `pane_hairline` (derived on `raised`), each on its own pixel row; visible dark and light |
| Track heads (`live`, `mix`) | Head flush against the lane stripe (1); name at +8, kind at +26, meter at y1−8 → 1 px kind↔meter on 46 px sound lanes (1); pills `neutral[3]` bare (2); heads flat `raised` with no edge (2) | `ui::surface::card(raised, radii.sm)` inset `S1` from the stripe; bands from `layout::track_head`; S/L/M off = `Button::paint` Neutral, on = semantic fill; kind↔meter 4 px |
| Lane stripes (`live` light) | Sound lanes `success @ 5 %` — invisible on light (2); picture lanes `lift(0.35)` no edge | `card(lift(0.5), 0)` with derived edge; sound lanes keep a green tint via `tinted` |
| Clips (`live`, `split`, `linked`) | Rest outline `surfaces.base @ 55 %` — a light ring on light (2) | `edge_on(body)`, crisp inset; primary/selected/linked outlines unchanged |
| Bin rows (`live`, `focusbin`, `rename`) | Selected = accent wash + accent outline (§9.3, 2); name/meta 3 px apart (1); row↔row 6 px | `selection_row` (`selection_tier`: lift + hairline + glow), hover = lift(0.5); measured block centred; row↔row `S2` |
| Bin “Not on device” chip (`live`) | Fixed `y0+28..45` off the metadata line (4) | On the metadata line’s band |
| Transport (`live`, `focustransport`, `shuttle`) | Discs `neutral[3]` bare (2); scrubber `neutral[3]` bar + local knob (2) | `Button::paint` discs (Primary play), `SliderFace` scrubber with hover/drag |
| Master strip (`mix`) | `MIX`/peak at +5, meter at y1−12 → 2 px apart (1); meter bars `neutral[3]` (2) | `layout::master_strip` block centred, `S1` apart; meter well `card(lift(0.35))` |
| Ruler ticks / grid (`live`) | `border_standard` ticks, `border_standard @ 40 %` grid | `pane_hairline` ticks; grid `edge_on_with(base, HAIRLINE_ALPHA/2)` |
| Zoom chip, +Video/+Audio (`live`) | `neutral[3]` + `border_standard` (2); buttons 6 px apart | `Button::paint`; `S2` apart, `S2` under the last lane |
| Inspector: title words box (`title`) | `lift(0.35)` + `border_standard` (2) | `well` (card + derived edge), focus ring while editing |
| Inspector: swatches (`title`, `keymask`, `filters`) | Label/key/title swatches bare fills; title plate `neutral[3]` + `text_primary @ 25 %` | `card(swatch)` — edge derived from the swatch itself; plate `lift(0.35)` |
| Inspector: segmented rows (`filters`, `geometry`, `format`, `mix`) | 3 px slots between segments inside one outline (1, 3); anchor rows 3 px apart | Segments abut (one `ToggleGroup`); anchor rows `S1` apart |
| Inspector: PiP presets (`geometry`) | `neutral[3]` + `border_standard`, 6 px apart | `Button::paint` (Primary when in force), `S2` apart |
| Inspector: effect rows (`effects`) | `lift(0.35)` bare (2); ↑/↓ 1 px apart (1) | `well`; controls `S1` apart, hover via `selection_row` |
| Inspector: section headers, marker rows, key controls (`markers`, `keyframes`) | Hover `lift(0.35)`/`lift(0.72)` bare | `selection_row` hover tier |
| Inspector: curve editor (`curves`) | `lift(0.35)` + `border_standard` | `well` |
| Inspector: “N clips selected” pill (`multiselect`) | Accent wash | `well` capsule, accent ink |
| Inspector: level meter (`mix`) | `neutral[3]` track and flag | `card(lift(0.35))` track and idle flag |
| Go-to card (`goto`) | `raised` + accent stroke | `panel_ground` + `panel_border` + focus ring (Panel role) |
| Export progress bar (`exporting` dialog state) | `neutral[3]` | `well` |
| Topbar toggles (`matrix/phone`, `matrix/tablet`) | Hamburger/inspector toggle `neutral[3]` bare | `Button::paint` via `paint_icon_toggle` (Primary when open) |
| Topbar (`live`) | Gap 10 (between `s2` and `s3`) (3); hamburger at +8 | `S3` everywhere; test at 390/480/768/1280 px |
| Empty preview canvas (`empty`) | `border_standard` outline, white safe-area guide on a white canvas | `edge_on(base)` outline and guide when no footage is shown |

Menus (`clipmenu`, `keymenu`, `projectmenu`, `menu`), dialogs (`discard`, `help`,
`exporting`/`exportfailed` chip) and the inline rename field are the shared `ContextMenu`,
`ModalFrame` and `EditableLabel`; they were inspected in both themes and read correctly
(visible panel edges, separators inset). Their edges are F2’s to derive; nothing here paints
them.

## Pair table (1280×800, 1×; px before → after; from the constants and the layout model)

| Surface | A | B | Axis | Before → after | Step |
|---|---|---|---|---|---|
| Topbar | project button | Import | x | 12 → 12 | `S3` |
| Topbar | Import | Split | x | 10 → 12 | `S3` |
| Topbar | Split | Export | x | 10 → 12 | `S3` |
| Topbar | hamburger (390 px) | inspector toggle | x | 10 → 12 | `S3` |
| Topbar | window edge | first / last control | x | 8·12 → 12·12 | `S3` |
| Bin | row | row | y | 6 → 8 | `S2` |
| Bin | thumb | row edge | x/y | 8 → 8 | `S2` |
| Bin | thumb | name | x | 12 → 12 | `S3` |
| Bin | name line box | metadata line box | y | 3 → 4 | `S1` |
| Timeline | lane | lane | y | 4 → 4 | `S1` |
| Timeline | track head | lane stripe | x | 0 → 4 | `S1` |
| Timeline | head | head | y | 4 → 4 | `S1` |
| Track head | name band | kind line | y | 1 (46 px lane) → 4 | `S1` |
| Track head | kind line | meter | y | 1 → 4 | `S1` |
| Track head | S | L | x | 4 → 4 | `S1` |
| Track head | L | M | x | 4 → 4 | `S1` |
| Track head | M | head edge | x | 8 → 8 | `S2` |
| Track head | name | first pill | x | ≥0 (unelided) → 4 | `S1` |
| Timeline | last lane | +Video/+Audio row | y | 8 → 8 | `S2` |
| Timeline | +Video | +Audio | x | 6 → 8 | `S2` |
| Master strip | readout | meter | y | 2 → 4 | `S1` |
| Clip | clip | lane edge | y | 2 → 2 | inset (content) |
| Clip | clip | clip (adjacent lanes) | y | 8 → 8 | `S2` |
| Inspector | anchor row | anchor row | y | 3 → 4 | `S1` |
| Inspector | PiP preset | PiP preset | x | 6 → 8 | `S2` |
| Inspector | key swatch | key swatch | x | 5 → 4 | `S1` |
| Inspector | key swatches | Sample button | x | 5 → 8 | `S2` |
| Effect row | key | ↑ | x | 4 → 4 | `S1` |
| Effect row | ↑ | ↓ | x | 1 → 4 | `S1` |
| Effect row | slider | ✕ | x | 8 → 8 | `S2` |
| Marker row | note | rename | x | 2 → 4 | `S1` |
| Marker row | rename | delete | x | 2 → 4 | `S1` |

Contiguous clips on one lane (`split`, `linked`) share an edit point by definition; each now
carries its own derived edge so the seam reads as two clips (2 hairlines), not as a 4 px gap in
the programme. Recorded as the one deliberate non-`S1` adjacency.

## Ratchets

`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`:

```
apps/video_editor/src/app.rs [pixel_const]: now 48, baseline still 53 — lower it to 48
```
(stale — the count went down; the coordinator refreshes `ui/tests/f1_spacing_token_baseline.txt`.)

`app_and_shell_theme_escape_hatches_do_not_grow` fails only on the pre-existing
`apps/browser/src/browser_tabs.rs` / `shell/src/shell/frame.rs` literals named in the brief;
no `grew` line names `apps/video_editor`. `white_or_black` in `app.rs` stays at its baseline
12 hits (the safe-area guide is now white only over footage, otherwise derived).
`declared_borders_in_chrome_do_not_grow`, `theme_escape_hatches_do_not_grow`,
`no_hardcoded_style_literals_in_tree`, `spacing_patterns_match_the_shapes_they_claim`,
`declared_border_pattern_matches_the_shapes_it_names`, `forbidden_patterns_match_the_shapes_they_name`,
`product_apps_use_the_shared_responsive_launcher`: pass.

## Tests

- `cargo test -p video_editor --lib`: 684 passed, 0 failed, 0 ignored (682 at the base; +4
  `layout::tests`, +3 `chrome_geometry_tests`, and the base's `the_topbar_is_inset_equally…`
  still passes), including `shot::tests::*` (scenario determinism) and
  `app::tests::every_window_size_renders_without_panicking`.
- `cargo test -p video_editor --tests`: `headless_action_no_window` 1 passed,
  `two_participants` 8 passed, `export_fidelity` 0 (feature-gated).
- `cargo check -p video_editor --tests`: clean (no video_editor warnings).
- `cargo build -p video_editor`: ok.
- Touching lint (`usability.geometry_audit`) on `after/{dark,light}/geo/live.inspection.json`:
  `failures: []`, `passed: true`.

## Code review

`code-review` (medium) raised four items; three fixed in this diff: the lane name now has its
own band ending `S1` before the first pill and is elided to it (a collaborator-named
“Voiceover” lane no longer runs under S/L/M); the head hit-tests read one `TrackHead` instead
of rebuilding it per pill (wrappers kept `#[cfg(test)]` for the existing tests); the
`the_steps_are_the_theme_scale` change-detector was dropped. The fourth — `S1..S4` are
compile-time `Spacing::BASE` and do not follow a custom theme’s `spacing_px`/`ui_scale` — is
recorded under limits.

## Open — needs another unit

- **F1** — `ui/src/widget/button.rs::face_colors` Neutral/Disabled rest edge is still
  `border_standard`; every `Button::paint` face here (Import, Split, +Title, S/L/M, zoom chip,
  +Video/+Audio, transport discs, PiP presets, inspector buttons) inherits it. It registers on
  the dark topbar/heads in the PNGs today but is declared, not derived. `after/dark/live.png`.
- **F1** — `ui/src/widget/slider.rs::Slider::paint` track `neutral[3]` + `border_standard`,
  knob `surface_white_10`/`border_white_20`: every inspector slider and the transport scrubber.
  `after/{dark,light}/filters.png`.
- **F1** — `ui/src/widget/toggle_group.rs` Solid segment separators and outline in
  `border_standard`: every segmented row and the export dialog’s option groups.
  `after/dark/format.png`.
- **F2** — `ContextMenu` (`clipmenu`, `keymenu`, `projectmenu`, `menu`), `ModalFrame`
  (`discard`, `help`, `exporting`), `EditableLabel` (`rename`): Panel role edges and inset
  separators per the toolkit edge policy. Read fine in both themes at this base; listed for
  completeness since this unit does not paint them.
- **Unit 0 tooling** — `--headless-drive` at `fd1f84391` rejects a `"scenario"` field
  (`unknown field "scenario"; expected viewport, actions, audit, audit_report, dump, targets,
  or inspect`), so the geometry lint could only run on the live fixture. The scenario dumps
  (`before/*/geo/<scenario>.out`) record the refusal.

## Limits

- The editor is one canvas `Widget`; its accessibility tree publishes actions without bounds,
  so `geometry_audit` measures one control and the touching lint is machine-checked only
  through the `layout.rs` and `chrome_geometry_tests.rs` assertions, not the inspection JSON.
- `S1..S4`, `HEAD_PILL` and the rest of `app.rs`’s chrome constants are compile-time
  (`Spacing::BASE`/`Metrics::BASE`); `regions()` runs in the event path with no `Theme`, so the
  editor’s own gaps do not rescale with a custom theme’s `spacing_px` or `ui_scale` — the same
  standing limit the file’s other constants carry (measured-controls work).
- `Color::WHITE`/`Color::BLACK` uses (12) are unchanged: HUD chips over footage, the pending
  dots on a thumbnail, and `on_fill`’s preferred ink; the safe-area guide is now derived when
  no footage is shown.
- Contiguous clips share an edit point (see pair table).
