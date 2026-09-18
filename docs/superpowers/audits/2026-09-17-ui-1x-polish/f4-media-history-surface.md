# F4 — Media, history, collaboration widgets, surface helpers (1× polish)

Branch `ux1x/f4-media-history-surface`, base `fd1f84391` (Unit 0 tip). Evidence under
`target/ux1x/f4/{before,after}/…` in the worktree
`/Users/xaver/dev/qos/.claude/worktrees/agent-a5b2ef1248e740fd4`. Every PNG named below was
opened and inspected at 1× in dark and light.

Files edited (16 + this report): `ui/src/widget/{history_scrubber,activity_timeline,
channel_browser,search_results,video_player,capture_studio,screenshot_markup,window_group,
image}.rs`, `ui/src/surface.rs` (`chip_colors` quiet branch, `pill` stroke geometry, one test),
`ui/src/status_bubble.rs`, `ui/examples/{activity_timeline,channel_browser,history_cases,
soft_shadow_shots}.rs`, `ui/tests/window_group_visual_journey.rs`. Not touched: `history.rs`,
`history_group.rs`, `image_viewer.rs`, `strip_view.rs`, `workspace_view.rs`, `tile_host.rs`,
`qr.rs`, `spinner.rs` (no declared edge, veneer or touching pair found in them — `tile_host`
already paints `Colors::window_edge()`), and the four owned tests other than the journey.

## Six-rule checklist

| # | Rule | Evidence |
|---|---|---|
| 1 | Nothing touches or overlaps | Pair table below; `capture_studio` mode/source buttons `TOP_BUTTON_GAP` 2 → **4 px (s1)** (`after/{dark,light}/capture_studio.png`); lifted list rows in `activity_timeline` and `search_results` inset `ROW_LIFT_INSET` = s1/2 top and bottom so two lifted neighbours keep **4 px** instead of fusing (`after/history_cases/{dark,light}/05-selection.png`, before: one grey block); filter chips `CHIP_GAP`/`PILL_GAP` 6 → **8 px (s2)**; `video_player` menu selection lift keeps 4 px between rows. Lint: `video_player`, `channel_browser`, `history_scrubber` inspection pass with 0 failures; `activity_timeline` reports 9 `touching_peers` that are consecutive **list-row hit bounds** tiling the list (a list has no dead zones); the painted rows keep s1 (PNG). |
| 2 | Every surface has a visible derived edge or contrast step | `theme_blind_white` veneers: `history_scrubber` 10 → 0, `channel_browser` 6 → 0, `activity_timeline` 6 → 0, `search_results` 1 → 0. `declared_border` (border_standard/neutral[]/border_focus-as-edge): `capture_studio` 8 → 0, `activity_timeline` 4 → 0, `video_player` 4 → 0, `channel_browser` 3 → 0, `history_scrubber` 2 → 0, `search_results` 1 → 0, `surface.rs` 1 → 0, `status_bubble.rs` 1 → 0. `literal_color`: `image.rs` 1 → 0, `window_group.rs` 1 → 0 (ratchet output below). Edges come from `surface::{card, pill, edge_on, edge_outline, separator, chip_colors}` and `chrome::shell::{panel_ground, panel_border}`; six in-crate fixtures assert Δ relative luminance ≥ 0.008 on dark **and** light. |
| 3 | One rhythm per surface | `capture_studio`: buttons s1, groups `TOP_GROUP_GAP` s2, group padding s2; `activity_timeline`/`channel_browser` chips s2; lifted rows s1 (`ROW_LIFT_INSET` = s1/2 each side); `channel_browser` footer rule inset = `PADDING` (12, the sheet's own inset) — see Open (12 is not a token step). |
| 4 | Content centred on measured line boxes | Unchanged from base for these widgets — every label already uses `measure(…)` and centres on `(h − lh)/2` (`status_bubble::centered_text_top`, chip/pill labels, menu rows); the × glyph fills the square chip via `draw_icon`'s centred fit. No fixed text offsets were introduced. |
| 5 | No "authored pixel" exemption | New offsets are token expressions (`Spacing::BASE.s1/s2`, `ROW_LIFT_INSET`); radii in `capture_studio` (10/9) and `video_player` menu (6) are inherited literals, unchanged — see Open. 1× frames are **not** byte-identical: 70 AFTER PNGs regenerated. |
| 6 | Evidence is visual | 26 PNG pairs (surfaces) + 50 history cases + 8 journey frames + 14 soft-shadow cards + 2 gallery frames, all opened; before/after table below. |

## Surface table (before → after, dark and light)

| Surface (PNG under `target/ux1x/f4/{before,after}/`) | Before (defects, rule) | After |
|---|---|---|
| `{dark,light}/activity_timeline.png`, `…/activity_timeline_example.png`, `history_cases/*/01–15` | Pane: `border_standard` on raised → no edge on dark (2). Bands `surface_white_5/10` blocks, no edge (2). Quiet chips `surface_white_10` + `border_white_20` (2; invisible on light). Hover `neutral[3]`; selected rows `surface_white_20` touching each other → **one fused block** (1). Selection bar wore `border_focus` (§9.4). Chip gap 6 (3). | Pane, bands, quiet chips, selection bar via `surface::card`/`pill` with `edge_on`; selected/hovered rows lifted on one wash ramp against `surfaces.raised`, inset s1/2 → 4 px apart; accent chip ink `on_accent`; chips s2 apart. Both presets show closed edges. |
| `history_cases/*/16–25` (strip), `{dark,light}/history_scrubber.png` | Strip `border_standard` on raised → invisible on dark (2). Unnamed segments/ticks/buttons/popover/handle ring: 10 `*_white_*` veneers (2). Track `neutral[3]`. Primary button wore `border_focus`. | Strip = `card(raised)`; segments one ordered wash ramp; neutral buttons Button-Neutral rest (`raised` + `edge_on`), primary accent with `on_accent` ink and no edge; popover `card(overlay)`; handle ring `edge_on(handle)`; track wash + **outline stroked last so it encloses played and unplayed range** (light PNG: dark 1 px ring around the blue track). |
| `{dark,light}/channel_browser.png` (example, PNG + inspection) | Sheet raised + `border_standard` (2); field `border_standard` rest; pills `surface_white_5` (2, bare text on light); role chips/key caps `surface_white_5` + `border_white_10`; footer rule **full-bleed** `border_standard`; selected row `surface_white_10`; pill gap 6 (3). | Panel role: `panel_ground` + `panel_border`; field `card(base)` at rest, `border_focus` 2 px on focus; pills/chips/key caps `chip_colors` quiet through `surface::pill`; selection = wash step + `edge_on(panel)`; footer rule `surface::separator` inset from the sheet edge; pills s2 apart. |
| `{dark,light}/search_results.png` | Panel raised + `border_standard` (2); hover `surface_white_5` (invisible on light). | Panel role (`panel_ground`/`panel_border`); hover = `hairline_on(panel_ground, HAIRLINE_ALPHA)`; selection and hover lifts inset s1/2 (never fuse). |
| `{dark,light}/video_player.png` | Bar stroked `border_standard` as a half-covered ring on all four sides (2); unfilled scrub/volume track `border_standard` (light: near-invisible); buffered range `text_dim` (vanished on light scrim); menu raised + `border_standard`; selected option = accent wash (§9.3). | Bar-role top hairline via `surface::separator(.., 0, scrim)` on the bar's top pixel row; tracks in the bar's own dim ink at 0.35; menu Panel role; selected option = wash step + `edge_on(panel)`. |
| `{dark,light}/capture_studio.png` | Groups/bottom bar bare `raised` fills over the scrim (2); light: shadow tinted `neutral[0]` (that palette's paper) → **no shadow**; resting buttons `lift(0.35)` with no edge, 2 px apart (1, 2). | Groups/bar `surface::card`; resting buttons `surface::card(fill)` (active faces keep no edge); shadows via `surface::shadow` with elevation tokens (`faded()` for the animated pills); buttons s1 apart. |
| `{dark,light}/window_group_close_chip_{idle,hover}.png`, `journey/window_group_*{,_light}.png` | Maximized × chip idle = literal `rgba(255,255,255,38)`; hover glyph literal white (fails on light danger red); the journey's maximized frames never showed the Restore/× chips (group collapsed to 0 px as a taffy root). | Idle = `chip_colors` quiet face with derived edge; hover glyph = `on_surface(danger).ink`; journey lays the group out inside a sized flex parent so the chips are on-canvas in both themes. |
| `{dark,light}/soft_shadow/*.png` (7 cards × 2) | Light: shadow tint `neutral[0]` = paper → white cards on off-white with **no shadow and no edge**. | Tint from the elevation token scaled by `shadow_scale`; cards through `surface::card` → visible shadow and edge on both presets. |
| `harness/gallery-{dark,light}.png` | Toolkit gallery (F1/F3 surfaces) — captured for the plan's gallery lint. | Unchanged by F4; lint: Bold↔Italic, Link↔Code `touching_peers` = Toolbar (F3, fixed there), 8 `accessible_label` misses (F1/F3 widgets). |
| `status_bubble::draw_pill` | Overlay pill + `border_standard` (within rounding of dark overlay). | `surface::card(overlay)`. No dedicated fixture (consumers are shell/apps); asserted by the ratchet drop and covered by `surface::card`'s own tests. |
| `screenshot_markup` | Shadows tinted `neutral[0]`; image failure mark from `Theme::dark()` per frame. | `surface::shadow` with elevation tokens; `Image::paint_at_on(.., theme)`. |

## Pair table (1×, both themes; from widget geometry, fixture asserts, or measured on the PNG)

| Surface | Element A | Element B | Axis | Gap before → after | Relationship / step | OK |
|---|---|---|---|---|---|---|
| capture_studio | mode button 1 | mode button 2 | x | 2 → **4** | siblings in a group, s1 (`TOP_BUTTON_GAP`; asserted ≥ s1 in fixture) | ✓ |
| capture_studio | source buttons (×3 pairs) | | x | 2 → **4** | s1 (asserted) | ✓ |
| capture_studio | mode group | source group | x | 8 → 8 | groups, s2 (`TOP_GROUP_GAP`; asserted ≥ s1) | ✓ |
| capture_studio | group edge | first button | x/y | 8 → 8 | padding s2 (`TOP_GROUP_PAD`) | ✓ |
| capture_studio | bottom-bar buttons | | x | 8 → 8 | s2 (PNG) | ✓ |
| activity_timeline | filter chip | filter chip | x | 6 → **8** | siblings, s2 (`CHIP_GAP`) | ✓ |
| activity_timeline | selected row lift | next selected row lift | y | 0 → **4** | related rows, s1 (2 × `ROW_LIFT_INSET`) | ✓ |
| activity_timeline | band backdrop | band backdrop (15-thousand-bands) | y | 4 → 4 | s1 (row inset 2 each) | ✓ |
| activity_timeline | pane edge | first chip / row | x | 8 → 8 | `PADDING` s2 | ✓ |
| channel_browser | space pill | space pill | x | 6 → **8** | siblings, s2 (`PILL_GAP`) | ✓ |
| channel_browser | selected row | next row | y | 0 → 0 (rows are text on ground; only one row is lifted at a time) | list rows | ✓ (n/a) |
| channel_browser | footer rule | sheet edge | x | 0 (full-bleed) → **12** | inset = sheet `PADDING` | ✓ (see Open: 12 is not a token step) |
| channel_browser | key cap | key cap / label | x | 8/6 → 8/6 | unchanged | ✓ |
| search_results | selected row lift | hovered row lift | y | 0 → **4** | s1 (2 × `ROW_LIFT_INSET`) | ✓ |
| search_results | panel edge | row | x | 8 → 8 | `ROW_INSET` s2 | ✓ |
| video_player | menu row lift | next row lift | y | 4 → 4 | (`y+1 .. y+row_h−3`) | ✓ |
| video_player | bar hairline | first control | y | — | bar padding s2 | ✓ |
| history_scrubber | neutral button | neutral button | x | 8 → 8 | siblings s2 in value, authored as a literal `8.0` in `button_layout` (measured on `16-strip-versions.png`: Preview x1 112 → Rename x0 120) | ✓ (see Open) |
| history_scrubber | band | track | y | 8 → 8 | `BAND_Y_FROM_TOP`+`BAND_H` → track (unchanged) | ✓ |
| history_scrubber | track outline | accent fill | — | outline stopped where the accent began → **encloses whole track** | edge integrity | ✓ |
| window_group (maximized) | Restore chip | × chip | x | 4 → 4 | s1 (asserted in `maximized_chrome_chips_share_one_row_and_a_token_gap`) | ✓ |
| window_group (maximized) | chip row | card edge | x/y | 6 → 6 | `TAB_PILL_INSET_Y` | ✓ |

## Geometry lint (`usability.geometry_audit`) on AFTER inspection JSON

- `video_player` dark/light: 8 controls, 21 pairs, **0 failures**, passed.
- `channel_browser` (fixture + example) dark/light: 1 control, 0 failures, passed.
- `history_scrubber` dark/light: 1 control, 0 failures, passed.
- `activity_timeline` dark/light: 11 controls, 45 pairs, 9 `touching_peers` — each pair is two
  consecutive **list rows** whose semantic hit bounds share an edge (gap `[-504, 0]`: full-width
  rows stacked with no dead zone). The painted rows are inset (`ROW_LIFT_INSET`) so lifted
  neighbours keep 4 px; keeping the hit bounds contiguous is deliberate for a list. Recorded, not
  "fixed" by shrinking accessibility bounds.
- `capture_studio`, `search_results`: the widgets expose no actionable semantic children
  (`measured_controls: 0`) — nothing for the lint to pair; gaps asserted in the fixture instead.
- `harness/gallery-{dark,light}`: Bold↔Italic and Link↔Code `touching_peers` = Toolbar items,
  F3 (already fixed on `ux1x/integration`); 8 `accessible_label` = F1/F3 widgets. Not F4.

## Ratchet output (`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`)

`f1_spacing_token_audit`: pass (2/2). `design_token_audit`: 3 of 8 fail, all as expected — no
`grew` line names an F4 file:

```
src/widget/activity_timeline.rs [theme_blind_white]: now 0, baseline still 6 — lower it to 0
src/widget/channel_browser.rs [theme_blind_white]: now 0, baseline still 6 — lower it to 0
src/widget/history_scrubber.rs [theme_blind_white]: now 0, baseline still 10 — lower it to 0
src/widget/image.rs [literal_color]: now 0, baseline still 1 — lower it to 0
src/widget/search_results.rs [theme_blind_white]: now 0, baseline still 1 — lower it to 0
src/widget/window_group.rs [literal_color]: now 0, baseline still 1 — lower it to 0
ui/src/status_bubble.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/surface.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/widget/activity_timeline.rs [declared_border]: now 0, baseline still 4 — lower it to 0
ui/src/widget/capture_studio.rs [declared_border]: now 0, baseline still 8 — lower it to 0
ui/src/widget/channel_browser.rs [declared_border]: now 0, baseline still 3 — lower it to 0
ui/src/widget/history_scrubber.rs [declared_border]: now 0, baseline still 2 — lower it to 0
ui/src/widget/search_results.rs [declared_border]: now 0, baseline still 1 — lower it to 0
ui/src/widget/video_player.rs [declared_border]: now 0, baseline still 4 — lower it to 0
```

`app_and_shell_theme_escape_hatches_do_not_grow` reports `grew` for
`apps/browser/src/browser_tabs.rs` (literal_color 1, white_or_black 1) and
`shell/src/shell/frame.rs` (literal_color 1, white_or_black 2). Both files are byte-identical
to base `fd1f84391` in this worktree (`git diff fd1f84391 -- <file>` empty) — pre-existing at
the base, not F4's.

## Tests run

- `cargo test -p ui --lib -- widget::history_scrubber widget::activity_timeline
  widget::channel_browser widget::search_results widget::video_player widget::capture_studio
  widget::window_group widget::image widget::screenshot_markup widget::tile_host widget::strip_view
  widget::workspace_view surface:: status_bubble widget::history_group widget::history::
  widget::image_viewer widget::qr widget::spinner` → **362 passed, 0 failed** (361 + the
  rewritten `maximized_close_chip_uses_danger_token_on_hover`, re-run green after the layout fix).
  New fixtures (all assert Δ luminance ≥ 0.008 on dark and light and write PNG + inspection
  under `$UX1X_F4_OUT`): `history_scrubber::ux1x_strip_buttons_and_track_edges_register_on_both_presets`,
  `activity_timeline::ux1x_pane_chips_and_bands_keep_a_derived_edge_on_both_presets`,
  `channel_browser::ux1x_sheet_pills_field_and_selection_register_on_both_presets`,
  `search_results::ux1x_panel_edge_and_hover_register_on_both_presets`,
  `video_player::ux1x_controls_bar_and_menu_edges_register_on_both_presets`,
  `capture_studio::ux1x_groups_and_resting_buttons_keep_a_derived_edge_on_both_presets`,
  `surface::chip_colors_quiet_edge_registers_on_every_preset`.
- `cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit
  window_group_visual_journey strip_view_test window_group_test workspace_view_test
  tile_app_widget` → **20 passed, 3 failed** (the three ratchet `stale`/pre-existing-`grew`
  cases above). The journey now has a light variant; both journeys **fail** (not skip) without a
  wgpu adapter, per CLAUDE.md.
- `cargo check -p ui --tests --examples` → clean (two pre-existing warnings in `record.rs`,
  `animation_schedule.rs`).
- `cargo build -p ui --examples` → clean; captures run from `target/debug/examples/{ui-shots,ui-lab}`.

## Coordinator directives — status

- Hand-rolled fill+stroke → `surface::card`/`pill` where geometry matches: done for
  `status_bubble::draw_pill`, the four `capture_studio` resting-button sites, `history_scrubber`
  strip/popover, `activity_timeline` pane/chips/selection bar, `channel_browser` field/pills/
  chips/key caps. Sites whose edge derives from the **ground** rather than the fill (wash lifts:
  `activity_timeline` selected row/band, `channel_browser` selected row, `video_player` menu
  selection, `history_scrubber` track and hovered button) keep the inline geometry — they are
  `card_on(.., fill, ground)` calls once that helper is on the base (Open).
- `video_player` controls-bar `Line` stroke → `surface::separator(.., 0.0, scrim)`: done.
- `Shadow::with_alpha_scaled`: **not reachable** (`fn with_alpha_scaled` in
  `tokens/elevation.rs:24` is private). `capture_studio` uses a local `faded(token, fade)` for
  the two animated pills (Open: make it `pub` and delete `faded`).
- Wash alpha ladders kept per file (`history_scrubber` 20/36/48/64, `activity_timeline`
  26/40/56, `channel_browser` 56, `video_player` 56, `search_results` `HAIRLINE_ALPHA`) — Open,
  needs Unit 0 shared wash ramp.
- `Image::paint_at` + `&Theme`: `paint_at` has 19 callers outside F4 (word-processor, typst,
  camera ×8, terminal, shell `overlay_host` ×6) that F4 may not edit, so the signature stays
  source-compatible: the fallback ink is a once-resolved `LazyLock` (no `Theme::dark()` per
  frame), and the new `Image::paint_at_on(.., &Theme)` is the theme-threaded entry
  (`screenshot_markup` and `Widget::draw` use it). Open: switch the 19 callers to `paint_at_on`
  and retire the fallback.
- Light journey must **fail**, not skip: done (both journeys).
- Idle-chip assertion on the **production draw path**: the two `#[cfg(test)]` colour helpers are
  deleted; the test lays the group out, draws through `Widget::draw`, composites offscreen and
  samples the chip (face = quiet fill within 1.02:1, rest edge Δ ≥ 0.008, hover face red-led
  and nearer `danger`, × glyph ≥ 3:1 on the hot face), on both presets, writing
  `window_group_close_chip_{idle,hover}.png`.
- Scrubber track outline encloses the whole track: done (stroke after accent fill, band, ticks).
- Gallery `touching_peers` Bold↔Italic / Link↔Code: Toolbar, F3 — recorded, untouched.

## Open — needs …

- **Unit 0 (shared wash ramp):** the per-file alpha ladders above should be one
  `surface::wash(surface, step)` ramp.
- **coordinator (closed on `ux1x/integration`):** `surface::card_on(scene, t, rect, radius,
  fill, ground)` — replace the inline fill + `edge_on(ground)` stroke at the six wash-lift sites;
  `surface::separator` device-pixel snapping and `card` rect snapping — the `video_player`
  hairline and every `card` call pick these up on merge; `ui::launch::shot_theme_from_env()` —
  the four owned examples each parse `QOS_SHOTS_THEME` locally
  (`activity_timeline.rs`, `channel_browser.rs`, `history_cases.rs`, `soft_shadow_shots.rs`);
  switch them after merge. `chrome::selection_tier_on(theme, state, ground)` could replace the
  `channel_browser`/`video_player` menu selection wash once available.
- **tokens (Unit 0):** make `Shadow::with_alpha_scaled` `pub`; delete `capture_studio::faded`.
- **A-units / S1 / S2:** 19 `Image::paint_at` callers → `paint_at_on(.., theme)` (apps
  word-processor, typst_editor, camera, terminal; shell `overlay_host.rs:420-459`).
- **F3 (`tabs.rs`):** in `journey/window_group_post_close{,_light}.png` the narrow "File" pill's
  × glyph overlaps its truncated label — pre-existing (identical in BEFORE).
- **channel_browser (F4 follow-up):** `PADDING` 12 and the row/section metrics are literals off
  the spacing scale (12 = s3 in name only); the footer rule inset equals that 12. A rhythm pass
  on this sheet should restate them as `Spacing::BASE.s3` etc. — kept out of this change to
  avoid moving every row.
- **history_scrubber (F4 follow-up):** `button_layout` spaces the buttons with a literal `8.0`
  (= s2) and widths 78/88/132; restate as `Spacing::BASE.s2` — value unchanged, so kept out of
  this change.
- **capture_studio / video_player:** radii 10/9/6 are inherited literals (HIG §9.1 wants
  `radii.lg`/`radii.sm`); not changed here ("must not change radii defaults" applied by analogy).
- **ratchet baselines (coordinator):** lower the 14 `stale` entries listed above.
- **`app_and_shell` `grew`:** `apps/browser/src/browser_tabs.rs`, `shell/src/shell/frame.rs` —
  at base, owned by A1/S-units.

## Limits

- The code-review skill was invoked three times at `medium`; each fork returned before its
  finder agents reported (and the first two ran against the primary checkout's diff, naming
  files not in this tree). No findings were delivered. In its place I diffed this session's edits
  against the checkpoint copies in `target/ux1x/f4/after-src/` and re-read every hunk: the
  changes are the ones listed above and compile warning-free.
- The activity-timeline `touching_peers` lint result is a property of list-row hit bounds, not
  paint; see the lint section.
- `status_bubble` has no F4-owned capture; its consumers are shell/app surfaces (S1/S2/A-units'
  PNGs will show it).
- `$UX1X_F4_OUT` and `QUICK_UI_SCREENSHOT_DIR` must be **absolute**: cargo runs `ui` tests from
  `ui/`, so a relative path lands under `ui/target/`.
- `soft_shadow_shots` is an example of the sprite painter, kept as such; only its tint, scale and
  card edge changed.
