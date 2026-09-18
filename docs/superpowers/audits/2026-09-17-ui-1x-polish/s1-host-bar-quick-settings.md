# S1 — Host, Bar, wheel, Quick Settings, notifications (1× polish)

Branch `ux1x/s1-host-bar-quick-settings`, base `fd1f84391` (Unit 0 tip). Worktree
`/Users/xaver/dev/qos/.claude/worktrees/agent-a76f51eaa1bdd9611`. Evidence under
`target/ux1x/s1/` (paths below are relative to the worktree). Every PNG named here was
opened and inspected; the one-line verdicts are in the surface table.

Files changed (all in the S1 list): `shell/src/shell/{overlay_host, quick_settings_root,
quick_settings_drill_in, quick_settings_wifi, quick_settings_bluetooth, quick_settings_vpn,
notification, remote_cursors, sidebar_wheel_chrome, topbar_chrome}.rs`,
`scripts/ux-bench/s1/s1-quick-settings.json` (new). Not changed: `power_menu.rs`,
`shell/src/bin/shots/**`, panel widths (320/360/400 popover, 400 at ≥1200 px).

## Six-rule checklist

| # | Rule | Status | Evidence |
|---|---|---|---|
| 1 | Nothing touches or overlaps | Done | Pair table below (every adjacent pair ≥ `s2` = 8 px; the two slider bands went 0 → 8). `geometry_audit`: `touching_peers [Brightness, Volume]` present in `before/qs-dark/quick-settings.inspection.json`, gone in `after2/qs-dark/…` and `after2/qs-light/…`. `quick_settings_root.rs::tests::root_geometry_keeps_one_rhythm_at_one_x` asserts no pair of tiles/sliders/cards/power intersects. |
| 2 | Every surface has a visible derived edge or contrast step | Done | 0 `border_standard`/`border_strong`/`*_white_*`/`neutral[]` left in the ten files (ratchet output below: six files at `now 0`). Panel: `panel_border` on `edge_outline` (`overlay_host.rs` QS host + overview). Controls: `quick_settings_drill_in::paint_control` = `ui::surface::card(control_fill)` (lift 0.35 floor + `edge_on`), hover `selection_tier`. Pixel-asserted per preset in `quick_settings_drill_in::tests::control_surface_registers_on_the_panel_ground_on_every_preset` and in `overlay_event_absorb_tests::s1_review_fixtures_are_written_in_dark_and_light` (row interior = Control floor, padding = Panel ground, dark and light). |
| 3 | One rhythm per panel | Done | `quick_settings_drill_in.rs` is the one constant set for root, Wi-Fi, Bluetooth, VPN, notification history and the host: `PANEL_PAD`/`SECTION_GAP` = `s4`, `SIBLING_GAP`/`CONTROL_INSET` = `s3`, `RELATED_GAP`/`CONTROL_PAD` = `s2`, `ROW_HEIGHT` = `panel_row_height`, `GLYPH` = `icon_md`, `TARGET` = `min_pointer_target`. Root before: 8 / 18 / 10 / 8 between the four sections; after: 16 / 16 / 16 / 16. |
| 4 | Content centred on measured line boxes | Done | Tile = glyph + `icon_label_gap` + measured caption as one block centred in the tile; card glyph/switch/chevron on the icon row's centre line (`leading_glyph`/`trailing_toggle`/`trailing_glyph`); power row, VPN row, Hotspot row and edit rows centre their measured label+detail block (`centred_text_top`); drill-in title measured with the weight it is drawn in. |
| 5 | No authored-pixel exemption | Done | Every kept offset is `Spacing::BASE.sN`/`Metrics::BASE.*` or derived from a measured line (`quick_settings_drill_in.rs` constants; `compile_time_rhythm_equals_the_preset_tokens_at_one_x` pins them to the three presets). Removed literals: root `PAD/HEADER/ACTIONS/SLIDERS/CARD/GAP/POWER/EDIT_*/…_LABEL_TOP`, tile `±9`, `scaled(46.)`, card `14/11/32/29`, toggle `50/11/14/31`, chevron `30/13`, power `12/15/30/33/42/28/17`, slider `.radius(12.)`, drill-in `ROW_INSET 12`, `56/52/8/40`, Wi-Fi `12/56/108/116/44/22/39/48/16/36/62/4/3/14/5`, BT `64/42/18/14/5`, VPN `12/16/32/36/56/44/9/29/12.0`, notification history `70/76/12/6/8/3/24/44/88/78/52/10/9/22`, shortcut guide `neutral[2|3]` radii `5/6`, QS drill-in cap `552`. |
| 6 | Visual evidence, dark and light | Done | Surface table below; each PNG opened. |

## Surface table

Dark first, then light. "Before" for the root is the ux-bench capture at base; "before" for the
fixture surfaces is `target/ux1x/s1/fixtures-before/<theme>/` (same fixture test on the base
paint path).

| Surface | Before (dark / light) | After (dark / light) | What was wrong → what is there now |
|---|---|---|---|
| Quick Settings root | `before/qs-dark/quick-settings.png`, `before/qs-light/quick-settings.png` | `after2/qs-dark/quick-settings.png`, `after2/matrix/020-s1-quick-settings-light/quick-settings.png` (also `after2/qs-light/quick-settings.png`); 2× crops `after2/crops/qs-dark-2x.png`, `after2/crops/qs-light-2x.png` | Dark before: panel outline `border_standard` on `panel_ground` invisible, root re-filled `surfaces.raised` over the host's ground (two tokens, one panel), tiles were Neutral `ButtonFace`s whose declared edge vanished, glyph pinned 8 px from the tile top with the caption riding the bottom edge (caption line 46+15 > 58.5 tile), the two slider faces touched (gap 0), card glyph/switch top-pinned, section gaps 8/18/10/8. After: one `panel_ground` fill with `panel_border` on `edge_outline`; every tile, the slider block, the card and the power row are Control surfaces (`paint_control`: lift-0.35 floor + `edge_on`, hover tier) with a visible edge on both presets; icon+caption block centred in each tile; glyph/switch/chevron on the icon row centre line; sections `s4` apart, tiles/cards `s3`, slider bands `s2`. Light before already showed edges (light `border_standard` reads) but had the same touching/pinning; light after matches dark geometry exactly (pair table). |
| Wi-Fi drill-in | `fixtures-before/dark/qs-wifi-drillin.png`, `fixtures-before/light/qs-wifi-drillin.png` | `fixtures/dark/qs-wifi-drillin.png`, `fixtures/light/qs-wifi-drillin.png` | Before: rows inset 12 vs the root's 16, 52-px rows on a 56 pitch (4 px apart), Hotspot row a Neutral `ButtonFace` with glyph/toggle/label at literal offsets, signal bars at hard-coded 4-px pitch, back chevron in an 8-px corner. After: rows on `PANEL_PAD`, `ROW_HEIGHT` 44 on `s2` gaps, every row `paint_control` with a derived edge (visible on dark), Hotspot label+detail block and switch centred on the row, bars fill the trailing status slot symmetric about the centre line, back button is a `TARGET` square on `s2` of air shared with the host's hit rect (`DrillInList::back_rect`). Light: same geometry, edges read as light hairlines. |
| Bluetooth drill-in | `fixtures-before/{dark,light}/qs-bt-drillin.png` | `fixtures/{dark,light}/qs-bt-drillin.png` | Battery glyph at `x1-64..-42` (22 wide, off the token glyph) → `trailing_status_glyph` (`icon_md` on the inset, one `s2` before the connected dot, which is `STATUS_DOT` = half a glyph); rows on the shared rhythm with derived edges. |
| VPN drill-in | `fixtures-before/{dark,light}/qs-vpn-long-labels.png` | `fixtures/{dark,light}/qs-vpn-long-labels.png` | Rows were `neutral[2]`/`neutral[3]` washes at a literal radius 12 with the name at `y0+9` and the status at `y0+29` (top-pinned) and a 44×24 toggle at `x1-56`; now `paint_control` rows, name+status block centred on the row, `TOGGLE_W×TOGGLE_H` on the `s3` inset, label width measured to the toggle minus `s2`. Long label elides before the switch on both presets. |
| Notification history (QS) | `fixtures-before/{dark,light}/qs-notifications.png` | `fixtures/{dark,light}/qs-notifications.png` (12 records, pager) and `fixtures/{dark,light}/qs-notifications-short.png` (3 records, no pager) | Rows were `neutral[2]` at 62 % alpha, radius 12, 64 tall on a 70 pitch at inset 12 with text at literal `7/24/44`; header chips `lift(0.35)` at radius 10 and literal rects; footer `lift(0.22)`. Now one `NotificationHistoryLayout` built from the theme's resolved `xs`/`sm` (`qs_type_px`) drives paint, hit-testing, visible-row count and preferred height: rows are `surface::card(control_fill)` at the Control radius on `PANEL_PAD`, `s2` apart, source/title/body on the control padding; Back/Clear are `paint_control` chips `2×TARGET` wide; the pager band is reserved only when the history overflows (review finding), so a short history ends on the panel padding (`qs-notifications-short.png`). Panel height is the natural list height capped by the popover budget, not a third `552` cap. `qs-notifications-short.png` (dark and light): three rows, "3 notifications" position line, the panel ends `s4` under the last row with no pager band. |
| Shortcut guide (F1) | `fixtures-before/{dark,light}/shortcut-guide.png` | `fixtures/{dark,light}/shortcut-guide.png` | Keycaps `neutral[2]` at radius 5, reset chips `neutral[2]`/`neutral[3]` at radius 6, footer rule `neutral[3]` full width → keycaps `surface::card(control_fill)` at `radii.sm` (a detail inside the guide), reset chips `paint_control`, rule `surface::separator` derived on the Panel ground. Dark keycaps now have a visible edge; light unchanged in feel. |
| Workspace overview | `fixtures-before/{dark,light}/overview.png`; 200 % text via ux-bench `before/matrix/00{0,2}-s1-overview-density-*/overview.png` | `fixtures/{dark,light}/overview.png`; `after2/matrix/000-s1-overview-density-baseline/overview.png`, `after2/matrix/002-s1-overview-density-light/overview.png` | Panel outline `border_standard` → `panel_border` on `edge_outline`. Rows (S2's `workspace_settings`-style cards) untouched. |
| Top bar | `before/matrix/018-s1-topbar-alignment-baseline/topbar.png`, `before/matrix/020-…-light/topbar.png` | `after2/matrix/030-s1-topbar-alignment-baseline/topbar.png`, `after2/matrix/032-s1-topbar-alignment-light/topbar.png`; crop `after2/crops/topbar-centre-dark-3x.png` | Bottom hairline and the vertical divider were `border_standard` (invisible on dark) → `chrome_hairline`; disabled workspace arrows borrowed `border_standard` as an ink → `text_dim` under `DISABLED_ALPHA`; inactive page dots `neutral[7]` → `text_dim`. |
| Sidebar wheel | (collapsed wheel visible in every desktop PNG, e.g. `before/qs-dark/initial.png`) | `after2/qs-dark/initial.png`, `after2/qs-light/initial.png` | Backdrop edge `border_standard@0.35` → `edge_on(surfaces.base)@0.35`; pill highlight and rim were literal white (`from_rgba8(255,255,255,…)`) → `Colors::hairline_on(surfaces.base, a)`; plus card `neutral[0]`/`raised` + `border_standard` dashes → `control_fill` + `edge_on(fill)` dashes; idle cards `neutral[1]` → `ui::surface::card(control_fill)`. Expanded/hover states have no headless capture in this tree (see Limits). |
| Toasts (lock-suppressed card, DND pill) | none captured at base (fixture only exists post-change; before = `surfaces.raised` + `border_standard`, Δ luminance ≈ 0.007 on dark per the plan's measurement) | `target/ui-review/s2-r2/notifications/{dark-1x,light-1x}-locked.png`, `…-dnd.png` (written by `notification.rs::tests::notification_stack_review_fixtures_are_written`) | Both surfaces now `ui::surface::card(raised)`: a visible edge on dark and light. |
| Remote-cursor label chip | — | — (no fixture) | `border_standard` → `edge_on(surfaces.raised)`; same call site, one token. |
| Palette (global_search, presentation only) | `fixtures-before/{dark,light}/palette.png` | `fixtures/{dark,light}/palette.png` | Inspected, no S1 change needed; rows and filter pills read at 1× on both presets. |

## Pair table — Quick Settings root, 1×, dark and light (identical geometry; from `quick-settings.inspection.json`)

| element A | element B | axis | before → after (px) | relationship / step | OK |
|---|---|---|---|---|---|
| Settings tile | Privacy tile | x | 8 → 12 | siblings `s3` | yes |
| Privacy tile | History tile | x | 8 → 12 | siblings `s3` | yes |
| History tile | Power tile | x | 8 → 12 | siblings `s3` | yes |
| Title band (header rect bottom) | tiles top | y | 8 → 16 | section `s4` | yes |
| Edit button bottom | tiles top | y | 4 → 20 | button centred in the 40-px band + `s4` | yes |
| Tiles bottom | slider block top | y | 18 → 16 | section `s4` | yes |
| Power tile bottom | Brightness face top | y | 20 → 24 | `s4` + control pad `s2` | yes |
| Brightness face | Volume face | y | **0 → 8** | related rows `s2` | yes |
| Volume face bottom | slider block bottom | y | 4 → 8 | control pad `s2` | yes |
| Slider block bottom | Night Light card top | y | 8 → 16 | section `s4` | yes |
| Night Light card | Power row | y | 8 → 16 | section `s4` | yes |
| Power row bottom | panel bottom | y | 16 → 16 | panel pad `s4` | yes |
| Panel left | first tile / card / power row | x | 16 → 16 | panel pad `s4` | yes |
| Slider glyph | slider face | x | 8 → 12 | glyph slot inset `s3` (face starts at `s3+glyph+s3` = 44) | yes |
| Tile glyph (20) | tile caption | y | 20 unmeasured, caption clipped by ~2 px at the tile bottom → `icon_label_gap` 6, block centred (6.75 px air top and bottom) | icon↔label | yes |
| Card glyph | card switch | x | never met before or after; both now on the icon-row centre line at `y0+8..28` (before: glyph `y0+11..29`, switch `y0+11..31`) | leading/trailing on `s3` | yes |
| Card icon row | card label | y | label at literal `y0+40` → icon row bottom + `s2` | related `s2` | yes |

Drill-ins and notification history (from `quick_settings_drill_in.rs` constants, asserted in
`list_rows_keep_the_panel_rhythm_and_trailing_slots_do_not_touch` and
`notification_history_reserves_the_pager_only_when_it_overflows`):

| surface | element A | element B | axis | before → after | step | OK |
|---|---|---|---|---|---|---|
| Wi-Fi / BT / VPN | row | row | y | 4 → 8 | related `s2` | yes |
| Wi-Fi / BT / VPN | panel edge | row | x | 12 → 16 | panel pad `s4` | yes |
| Wi-Fi | back button bottom (40) | Hotspot row top (56) | y | 16 → 16 (`HEADER_HEIGHT` 48 + `s2`) | related `s2` | yes |
| Wi-Fi | Hotspot row | first discovery row | y | 8 → 8 | related `s2` | yes |
| all rows | leading glyph | label | x | 12 → 12 | inset `s3` | yes |
| BT / Wi-Fi | status glyph (battery / bars) | connected dot | x | 2 → 8 | related `s2` | yes |
| VPN | label (elided) | toggle | x | 12 → 8 (`s2`, label width measured to it) | related `s2` | yes |
| Notification history | Back chip | title | y | both centred in the 48-px title band | — | yes |
| Notification history | position line | first row | y | 24 − caption → 8 | related `s2` | yes |
| Notification history | row | row | y | 6 → 8 | related `s2` | yes |
| Notification history | last row | pager band | y | ≥ 6 → 8, and 0 (no band) when the history fits | related `s2` | yes |
| Notification history | pager band | panel bottom | y | 10 → 16 | panel pad `s4` | yes |

## Geometry lint (`usability.geometry_audit`)

`after2/qs-dark/quick-settings.inspection.json` and `after2/qs-light/…`: 28 controls, 363 peer
pairs, 0 clipped. Failures: 13 × `semantic_bounds_overlap_unresolved` — every one a Quick
Settings control against the desktop heading/cards *under* the popover (`Make something
together`, `Notes`, `Tasks`), identical before and after: the popover covers the desktop by
design. `touching_peers`: before had `[Brightness, Volume]` (the two slider faces, an S1
defect — fixed) plus three Bar hit-rect pairs; after has the same three Bar pairs
(`Previous workspace` 61..93 / `Next workspace` 93..125 and the centre pill's three 32-px
segments — contiguous pointer-floor hit rects whose painted glyphs are 14–20 px apart,
pre-existing at base, recorded under Open) and two cross-layer coincidences
(`Night Light` ↔ desktop `Notes`/`Tasks`: the popover's card bottom, y = 366, happens to equal
the desktop card row's top under it). Zero same-surface touches or overlaps in S1 surfaces.

ux-bench matrix (`after2/matrix/summary.json`, `scripts/ux-bench/s1/*.json` × 6 variants, run
while the since-removed `s1-quick-settings-light.json` duplicate still existed — its six runs
`012–017` are the same checks as `018–023` and also pass): 35 / 36 pass; the one failure (`s1-tab-context-menu — compact`) is the same at base
(`before/matrix/summary.json`: the 800×700 fixture has no visible `topbar:overview` and no
`wheel:workspace:0` target) — a scenario-fixture gap, not a paint defect.
`s1-quick-settings.json` (dark; the matrix derives the light/HC/retina/large-text variants)
carries `gap`/`aligned`/`no_overlap`/`target_size` checks with `min > 0`: tile↔tile 10–15,
Edit↔tiles 4–29, tiles→sliders 21–29, slider↔slider 7–10, sliders→card 21–29, card→power
14–20, all four tiles on one line, tiles/cards/power left-aligned — 6/6 variants pass.

## Ratchet (`cargo test -p ui --test it -- design_token_audit`)

`declared_borders_in_chrome_do_not_grow`: no `grew` lines. Stale lines (expected; baseline is
the coordinator's):

```
shell/src/shell/notification.rs [declared_border]: now 0, baseline still 2 — lower it to 0
shell/src/shell/overlay_host.rs [declared_border]: now 0, baseline still 7 — lower it to 0
shell/src/shell/quick_settings_vpn.rs [declared_border]: now 0, baseline still 2 — lower it to 0
shell/src/shell/remote_cursors.rs [declared_border]: now 0, baseline still 1 — lower it to 0
shell/src/shell/sidebar_wheel_chrome.rs [declared_border]: now 0, baseline still 4 — lower it to 0
shell/src/shell/topbar_chrome.rs [declared_border]: now 0, baseline still 5 — lower it to 0
```

`app_and_shell_theme_escape_hatches_do_not_grow` is red **at base** (not S1 files):
`shell/src/shell/frame.rs [literal_color] 1, [white_or_black] 2` and
`apps/browser/src/browser_tabs.rs [literal_color] 1, [white_or_black] 1`, all present in
`fd1f84391` and untouched here. The other five `design_token_audit` tests pass
(`forbidden_patterns_match_the_shapes_they_name`, `declared_border_pattern_matches_the_shapes_it_names`,
`product_apps_use_the_shared_responsive_launcher`, `no_hardcoded_style_literals_in_tree`,
`theme_escape_hatches_do_not_grow`).

## Tests run

All with the inline env and `--no-default-features --locked`.

| Command | Result |
|---|---|
| `cargo check -p shell --lib --bin shell --bin ux-bench` | ok |
| `cargo check -p shell --tests` | every target ok except `shots` (bin test): `shell::compositor::capture` is `#[cfg(target_os = "linux")]` — pre-existing, platform, same at base |
| `cargo test -p shell --lib quick_settings` | 48 passed |
| `cargo test -p shell --lib topbar` | 30 passed |
| `cargo test -p shell --lib sidebar_wheel` | 51 passed |
| `cargo test -p shell --lib notification` | 56 passed, 1 ignored (pre-existing opt-in visual matrix) |
| `cargo test -p shell --lib overview` | 50 passed |
| `cargo test -p shell --lib status` | 15 passed |
| `cargo test -p shell --lib system_monitor` | 1 passed |
| `cargo test -p shell --lib global_search` / `remote_cursors` | 0 tests match (no unit tests in those modules) |
| `cargo test -p shell --lib overlay_host` | see the line below the table |
| `cargo test -p shell --lib s1_review_fixtures_are_written` | 1 passed (writes `target/ux1x/s1/fixtures/{dark,light}/*.png`, pixel-asserts Control floor and Panel ground) |
| `cargo test -p shell --lib notification_history_reserves_the_pager` | 1 passed |
| `cargo test -p ui --test it -- design_token_audit` | 5 passed, 2 failed (stale-only for S1 files; escape-hatch failure is at base, see above) |

`overlay_host` filter: 230 passed, 1 failed, 1 ignored (pre-existing opt-in visual matrix). The
failure is `tests::collaborator_actions_are_absent_without_the_invite_capability` — the
collaborators dropdown's capability gate (`collaborators_overlay.rs`, an S2 file); no line of the
S1 diff touches it (see Open). Every S1-owned `overlay_host` test passes, including the new
fixture and layout tests.

New tests in this diff: `quick_settings_drill_in::tests::{control_surface_registers_on_the_panel_ground_on_every_preset,
list_rows_keep_the_panel_rhythm_and_trailing_slots_do_not_touch, compile_time_rhythm_equals_the_preset_tokens_at_one_x}`,
`quick_settings_root::tests::root_geometry_keeps_one_rhythm_at_one_x` (replaces
`default_text_scale_keeps_the_authored_row_heights`, which pinned the old 70/50/58 heights),
`overlay_host::overlay_event_absorb_tests::{s1_review_fixtures_are_written_in_dark_and_light,
notification_history_reserves_the_pager_only_when_it_overflows}`. Updated with the new values:
`root_fits_between_the_bar_and_the_bottom_inset` (now fits through
`fit_root_layout_to_height` at ≥ 1× chrome), `discovery_row_glyph_is_the_token_glyph_centred_on_the_row`
(44-px row), `render_notification_history_accessibility_matrix_at_1x` (reads the layout oracle).

## Review findings (code-review skill, medium) and what was done

1. `NotificationHistoryLayout::preferred_height` always reserved the pager band, leaving a blank
   40-px band under a history that fits — fixed: `visible_rows(total)` / `preferred_height(rows,
   total, ..)` reserve the band only on overflow; regression test + `qs-notifications-short.png`.
2. Keycap floor bypassed `control_fill` with `colors.lift(CONTROL_FLOOR_LIFT_T)` — fixed; the
   constant is private to `quick_settings_drill_in` now.
3. `scripts/ux-bench/s1/s1-quick-settings-light.json` duplicated the dark scenario — removed; the
   runner's `--matrix` derives the light variant.
4. `shell/tests/qs_actions_click_routing.rs:734` hard-codes the retired Wi-Fi rhythm — outside the
   S1 file list; recorded under Open with the exact replacement.
5. `control_fill` belongs in `ui::chrome` — recorded under Open (Unit 0).

Binding findings from the previous (killed) session, resolved: (b) one shell-local
`control_fill` (drill-in module) used by the root, drill-ins, sidebar wheel, shortcut guide and
notification history; (d) `NotificationHistoryLayout::new(panel, qs_type_px)` measures the
theme's resolved `xs`/`sm` captured per frame in `ShellOverlayHost::draw`; (e)
`QS_DRILL_IN_MAX_HEIGHT` removed — every page is capped by `quick_settings_rect`'s popover
budget (bar above, desktop inset below), and `draw_quick_settings` clips its page to the Panel
so a list longer than the budget ends at the panel edge; (f) `draw_card_background` calls
`ui::surface::card`. (c) kept as compile-time `Spacing::BASE`/`Metrics::BASE` constants on
purpose: hit-testing (`handle_click`, `preferred_height`) has no theme in hand and must agree
with paint to the pixel; the shell never applies `with_ui_scale` in production (only test
fixtures do: `overlay_host.rs` review matrix, `onboarding_view.rs`, `lock_screen.rs`), so the
constants equal the preset tokens at 1× — pinned by
`compile_time_rhythm_equals_the_preset_tokens_at_one_x`; the root's `layout_scale` (fit-to-height
0.88–1.2) still scales every constant through `scaled()`. A theme-resolved rhythm is listed
under Open for when the shell adopts `ui_scale`.

## Open — needs …

- `Open — needs Unit 0: ui::chrome::control_floor(theme)` — `quick_settings_drill_in::control_fill`
  (lift 0.35 floor / hover `selection_tier`) is now consumed by `sidebar_wheel_chrome.rs` (plus
  card, idle cards) and `overlay_host.rs` (shortcut guide keycaps and reset chips, notification
  history rows and chips) — a shell-wide Control floor living in a quick-settings module; it
  belongs beside `ChromeRole::Control`/`selection_tier`.
- `Open — needs coordinator (closed on ux1x/integration)`: `selection_tier_on(theme, state,
  panel_ground)` should replace `selection_tier` inside `control_fill` (hover measured on the
  Panel ground the rows sit on); `ui::surface::card_on` for the wheel backdrop's translucent
  edge (`sidebar_wheel_chrome.rs` `edge_on(surfaces.base).with_alpha_factor(0.35)`); the
  device-pixel-snapped `separator` for the shortcut-guide rule (`overlay_host.rs`, `pointer_y + 0.5`).
- `Open — needs Unit 0`: a theme-resolved `QsRhythm::from_theme(&Theme)` if the shell ever
  applies `ui_scale` (see (c) above).
- `Open — needs coordinator`: `shell/tests/qs_actions_click_routing.rs:734` restates the retired
  Wi-Fi rhythm (`116 + 56·i + 28`); it passes by coincidence against the new rows (`108 + 52·i`,
  44 tall: row 0 click at +144 ∈ 108..152, row 1 at +200 ∈ 160..204). Replace `row_y(i)` with
  `host.quick_settings_wifi_row_rect_for_tests(i).center().y` — the helper is in
  `overlay_host.rs` for exactly this and is otherwise uncalled.
- `Open — needs S2 (coordinator at merge)`: `scripts/ux-bench/s2/s2-quick-settings.json` pins the
  old rhythm — `gap Night Light → Change power profile max 12` (now 16 = `s4`) and
  `gap Settings → Privacy / Privacy → History max 12` (now 12 at 1×, 14.4 at the large-text
  variant's 1.2 chrome scale) — all six of its matrix variants fail against S1. Align with
  `scripts/ux-bench/s1/s1-quick-settings.json` (14–20 / 10–15) or retire the duplicate.
- `Open — needs coordinator`: `shell::overlay_host::tests::collaborator_actions_are_absent_without_the_invite_capability`
  fails in this tree alone and in the filter (`left: (Rect{1272..1420}, Rect{1428..1576})`, a
  guest is still offered the membership controls). The gate is
  `collaborators_overlay.rs::action_rects` (S2 file); the S1 diff has no line mentioning
  collaborators. Verify at `fd1f84391` (I could not check out base without a second worktree).
- `Open — needs coordinator (base)`: `design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow`
  red at base for `shell/src/shell/frame.rs` and `apps/browser/src/browser_tabs.rs`.
- `Open — needs coordinator`: `geometry_audit` `touching_peers` on the Bar — `Previous workspace`
  / `Next workspace` (32×32 hit rects sharing x = 93) and the centre pill's three 32-px
  segments. These are pointer-floor hit rects (HIG §9.6); the painted glyphs sit 14–20 px apart
  and nothing visible touches (`after2/crops/topbar-centre-dark-3x.png`). Pre-existing at base.
  If the lint is meant to cover hit rects, the Bar needs an `s1` between targets (`topbar_chrome.rs`);
  left as is here so the targets keep the 32-px floor.
- `Open — needs F2`: none — the palette reads correctly at 1× on both presets.

## Limits

- The `shots` binary cannot be built on this machine at all: `shell/src/bin/shots/driver.rs`
  imports `shell::compositor::capture`, which is `#[cfg(target_os = "linux")]`
  (`shell/src/compositor/mod.rs`). The plan's `shots --mode offscreen --scenes quick-settings,
  qs-compact,qs-wifi-drillin,qs-bt-drillin,notifications,notifications-dnd` capture is replaced
  by the in-tree `OffscreenRenderer` fixture (`s1_review_fixtures_are_written_in_dark_and_light`,
  same pattern as `power_menu.rs`), which renders the production draw path at 1× on dark and
  light and asserts pixels. `qs-compact` (root at 800×700) is covered by the ux-bench matrix's
  `compact` variant (`after2/matrix/019-s1-quick-settings-compact/quick-settings.png`, pass).
- Sidebar wheel expanded, hovered and idle-card states have no headless capture in this tree;
  the collapsed wheel with one active card appears in every desktop PNG. Their edges are
  derived by construction (`ui::surface::card`, `Colors::hairline_on`) but not photographed.
- The Wi-Fi passphrase prompt page keeps its authored `244.0` height in
  `quick_settings_preferred_height` (not among the listed defects; not captured here).
- Before-PNGs for the toast card/pill were not captured at base by the killed session and the
  fixture output directory is rewritten on every run; the before state is the declared
  `border_standard` stroked on `surfaces.raised` (the plan's measurement: Δ luminance ≈ 0.007
  on dark, i.e. invisible), the after state is the two `ui::surface::card` PNGs listed above.
- The tab context menu is painted from `overlay_host.rs` but was not in the defect list and had
  no declared-border sites at base; not re-captured (the previous session's `--interactive`
  frames under `after/tabmenu-{dark,light}/` show only the tab pill tooltip).
- `after/` (from the killed session, 16:29–16:43) predates the last edits; the evidence in this
  report is `after2/` (root, matrix, crops), `fixtures/` (rewritten by the final fixture run) and
  `target/ui-review/s2-r2/notifications/` (rewritten by the `notification` filter run).
