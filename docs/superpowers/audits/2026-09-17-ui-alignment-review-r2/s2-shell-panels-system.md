# S2 — Shell panels, sheets, transient and system surfaces (round two)

Round two of the UI alignment, spacing and layout review for the shell's
floating and system chrome. Round one is merged at `381eff6a8`; its report is
`../2026-09-16-ui-alignment-review/s2-shell-panels-system.md` and the ids
below (`S2-nn`) refer to it. Branch `ui-review/r2-s2-shell-panels-system`,
cut from `381eff6a8`; delivery is local only.

## Scope

`shell/src/shell/{workspace_settings, workspace_settings_overlay, room_activity,
quick_settings_root, quick_settings_drill_in, collaborators_overlay,
collaboration_launchpad, peer_sync_overlay, projection_overlay, portal_dialog,
power_menu, file_picker_overlay, notification, osd, remote_cursors, peer_view,
peer_join_toast, desktop_backdrop, notes_app, editor_app}.rs`,
`shell/src/compositor/{lock_screen, drag_feedback}.rs`, `shell/src/onboarding_view.rs`,
`scripts/ux-bench/s2/*.json`. `overlay_host.rs` and `ui/src/history_mode/*`
were not touched (user WIP); the one change to `overlay_host.rs` on this branch
is the coordinator's `f11e61da6`, cherry-picked as `280e9b58b` (see Tests).

Governing text: `ui/HIG.md` §7a and §9. Every cargo command ran as
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig cargo … --locked --no-default-features`.

## Method

1. Re-verify round one's fixes on the merged tree: 48-run matrix, the three
   `s2/*.json` scenarios × six variants, `usability.py --mode deterministic`;
   captures read with the Read tool (`target/ux-bench/s2-r2-before*`).
2. Work the thirteen round-one open items owned by this slice and the reuse
   findings deferred there, then fresh eyes at 200 % and 800×700 on the
   surfaces the bench cannot open — through new inline `#[cfg(test)]` PNG
   fixtures rendered from the surfaces' own paint paths
   (`target/ui-review/s2-r2/{lock-screen,osd,power-menu,notifications}/`).
3. Fixes expressed through `theme.tokens.{metrics,spacing,radii,motion}`,
   `ui::chrome::{ChromeRole, selection_tier, shell::{panel_ground,
   panel_border, panel_shadow}}`, the shared `Button::paint` / `Toggle::paint`
   faces, and measured text; pinned geometry changed only after reading the
   test and recording it below.
4. AFTER: the same matrix, scenarios and lint; the fixture PNGs; scoped unit
   tests; the compile gate.

## Round-one items, resolved this round

| id | surface | round-one state | this round | where |
|---|---|---|---|---|
| S2-05 | Quick Settings sliders | 30 px faces on a 28 px pitch (< 32, and the two faces overlapped by 2 px); "needs re-spacing" | **fixed** — the 72 px block is `2·s1 + 2·min_pointer_target`: each slider is one 32 px pointer band, track and glyph centred on the band, hit-test is the painted face (the rect the access tree publishes). `s2-quick-settings.json` now asserts `target_size ≥ 32` and `no_overlap` for Brightness/Volume | `quick_settings_root.rs` `SLIDER_*`, `slider_geometry_for`, `slider_face_rect`, `hit_test` |
| S2-06 | QS drill-in | 12 px row inset kept because `quick_settings_wifi.rs` restates it | **recorded for the pane owner** — `ROW_INSET` is now `pub const` with the reason; the pane should read it. Rows themselves: hover via `selection_tier`, glyph `icon_md` on `s3`, idle floor on the same `lift` ramp (asserted per theme) | `quick_settings_drill_in.rs` |
| S2-11 | Room Activity chips | active chip was an accent fill | **fixed** — view tabs, scope pills and filter chips take `selection_tier(active)` (lift + hairline), idle on `lift(0.35)`; ordering asserted on dark/light/high-contrast | `room_activity.rs` `paint_selector_pill` |
| S2-14 | Settings lens | `radii.lg`, `surfaces.base`, `soft_shadow(40,16,0.62)`, bespoke 220 ms and a 48 px x-slide | **fixed** — Panel radius, `panel_ground`, `panel_border`, `panel_shadow` fading with the sheet, `durations.base` from the motion tokens, the shared `modal_transition_curve` entrance (opacity, rise, 0.95→1 about the centre). This file's own `MODAL_OPEN_ANIM_MS` (220) had no consumer left and is deleted; the power menu now opens over the motion token too. **Open — needs S1**: `overlay_host::MODAL_OPEN_ANIM_MS` (220) still times global search and the other host modals, so two Panel surfaces animate over 210 ms and the rest over 220 until that constant derives from the token | `workspace_settings_overlay.rs` `begin_draw`, `power_menu.rs` `panel_open_duration` |
| S2-15 | "At a glance" tiles, high contrast | tiles vanished where `base == raised` | **fixed** — each tile carries a hairline derived against the card (`stat_tile_face`); the composite of edge over card is asserted to move ≥ 0.008 in luminance on every palette | `workspace_settings_overlay.rs`; test `stat_tiles_keep_an_edge_on_every_palette` |
| S2-16 | Settings sidebar (tests-only path) | `neutral[1]/[3]`, identity radius 16, nav text at `+10` | **fixed, with a caveat** — `lift(0.35)` ground, `panel_border` seam, Control radius, active row via `selection_tier`, labels and the footer centred on measured lines, `icon_md` on `s3`, footer `bar_height`. The code review is right that `draw_body`/`draw_sidebar`/`sidebar_rect`/`nav_rows` have no production caller (the live lens paints through `WorkspaceSettingsOverlay` and a tab strip; `shell/src/shell/mod.rs:180` only calls `rename()`), so this polishes a painter no user sees. **Decision for the owner**: wire the sidebar into the overlay or delete the painter and its tests; recorded rather than deleted here because removing a tested module is a product decision, not an alignment fix | `workspace_settings.rs` `draw_sidebar` |
| S2-20 | Notifications | `CARD_PADDING_X 14` kept without evidence | **resolved as deliberate** — it is the shared toast card's own `CARD_PAD_X` (14), so the pill's text starts on the cards' line; documented at the constant | `notification.rs` |
| S2-29 | Peer-sync sheet | hand-rolled 30 px pills, 400–520 wide | **fixed** — `Button::paint` faces at `min_pointer_target` with the Control radius (`.primary()` where the action commits), `s3` label padding, `s2` gaps; sheet width `panel_width_sheet` (560) inside `s4` | `peer_sync_overlay.rs` `draw_buttons`, `card_rect` |
| S2-30 | Projection sheet | 20 px paddings | **kept, recorded** — the sheet's internal layout is authored on 20 and every hit rect (`audio_rect`, chips, credential field) shares it; moving to `s4` is a full sheet re-layout with no bench reach. Fixed instead: Panel edge → `panel_border`, the hand-rolled audio switch → `Toggle::paint` (its off track was `neutral[3]`) | `projection_overlay.rs` |
| S2-31 | Editor tile hint | reported repaired in WIP | **verified from source** — `header_height` measures title + hint (`editor_app.rs:488–502`); also its gutter divider was `Color::from_rgba8(0,0,0,24)` → `hairline_on(palette.gutter_bg, 26)` | `editor_app.rs` |
| S2-38 | Collaborators, projection, peer-sync, power menu, OSD at 200 % | still 4× (renderer scales the already-scaled tokens) | **fixed in every owned file, one idiom** — each immediate-mode surface resets the renderer to 1× for its own pass and restores it (the reset the lens pass already makes): collaborators, projection, peer-sync, power menu, OSD, notification stack (which also corrects the shared toast card at 2×), remote cursors, drag chip and lock screen; `Button::paint` faces inside those passes are handed the theme-scaled size explicitly. Round one's per-file `type_size` division in QS root, drill-in and Room Activity is left as it is. Fixture evidence: `power-menu/dark-2x-*.png`, `osd/dark-2x-*.png`, `notifications/dark-2x-*.png`, `lock-screen/dark-2x-*.png` | see files |
| S2-39 | Room Activity at 200 % | fixed 76 px header: title/subtitle/"Latest" and the scope pills overprinted; catch-up card lines collided | **fixed (reflow)** — `header_h()` = `HEADER_TOP + lg line + 2·sm line + HEADER_BOTTOM` at the user's text scale (equals 76 at 100 %); `draw` re-lays the sheet out when the theme's scale changes; scope pills are painted first and centred on the title's line, the title is elided to the room left of them; subtitle and "Latest" follow measured lines; `BriefCardMetrics` places and sizes every catch-up card line from measured heights (`s2` pad, `s1` gaps), the narrative card's label is `xs` (was an invented `sm − 1`). BEFORE `target/ux-bench/s2-r2-before-s2/011-…/activity.png` → AFTER `target/ux-bench/s2-r2-after-s2/011-…/activity.png` | `room_activity.rs` |
| `soft_shadow(neutral[0])` | lens, member sheet, onboarding | three sprite shadows off a ramp slot | **fixed** — `panel_shadow` (elevation token `e4`, follows the corner, silenced by the HC ladder); member-sheet scrim → `surfaces.scrim`; onboarding card → `panel_ground` + `panel_border` | `workspace_settings_overlay.rs:492,803`, `onboarding_view.rs:1706` |
| 26/30/78/256 | `workspace_settings.rs` | off-scale heights and sidebar width | **fixed** — `SECTION_HEADER_HEIGHT` = `bar_control_height` (28), `CONTROL_HEIGHT` and `REVOKE_HEIGHT` = `min_pointer_target` (32), `COMPOSER_HEIGHT` = `s3 + s6 + control + s3` (80), compact composer adds `control + caption band + s1` (54) so the "Until" caption clears the row above (it would have overlapped by 2 px at 32), default-app wide-mode controls centred at 32; `SIDEBAR_WIDTH` = `sidebar_width` (280). Pinned test `sidebar_rect_is_256_px_wide` read and renamed `sidebar_rect_is_the_token_sidebar_width`; the permissions-layout tests assert containment and ordering only and pass | `workspace_settings.rs` |

Hand-offs unchanged from round one (not owned): S2-25 (portal sheet paint,
S1), S2-33/34 (overview panel, QS width, S1), S2-35 (density consts, F1 —
now `Tokens::metrics`, adopted here).

## Reuse decisions revisited

| painter | site | decision |
|---|---|---|
| `Button::paint` | launchpad "Open an app" | **adopted** — `ButtonFace` with the Control radius, renderer-relative `base`, focus and hover from the launchpad's own state; the hand-painted fill/stroke/label are gone. Hint pixels unchanged (`navigation-layout.json` probes still pass) |
| `Button::paint` | peer-sync sheet buttons | **adopted** (S2-29 above) |
| `Toggle::paint` | projection "Share audio" | **adopted** |
| `Chip::draw_at` | DND pill | **declined** — the chip is a Selector meant to sit inside a Panel: its quiet fill is `surface_white_5`, a veneer that over the wallpaper and whatever window is under the stack would show through a status line; it also still sizes its label from the already-scaled `sm` token. The pill is instead the same material as the toast cards it heads (`surfaces.raised` + hairline, matching the locked card) and grows with its measured label. Reason recorded at `draw_dnd_pill` |
| `Chip::draw_at` | drag chip | **declined** — the carried item is an Object off the surface over arbitrary app content: it needs an opaque ground, an elevation shadow and the Control radius; nothing hit-tests it. Recorded at `draw_chip` |
| `selection_tier` | drill-in hover | **adopted** — hover is the tier's fill; the idle floor stays on the same `lift` ramp and the order is asserted per palette against the ramp's origin (`surfaces.base`) |
| `header_rect().y1` | QS root | **adopted as `header_bottom(panel)`** — one definition for the tiles, slider block, edit grid, edit caption and the header itself (the compat helpers run before `self.panel` is laid out, so it takes the panel explicitly) |

## Fresh eyes at 200 % and 800×700

| surface | finding | status |
|---|---|---|
| Power menu | 420 px panel, rows 50 tall on a 58 pitch, title at (22, 20), icon 22 at +14, label at +8/+28 authored: at 200 % label and detail overprinted; no 200 % room | **fixed** — list-Panel width (360), `PowerMenuMetrics`: Control rows (44) on the `s2` pitch inside `s4` padding, `row_h` = max(44, `s2` + label line + detail line + `s2`) at the user's scale, `icon_md` on `s3`, label/detail stack centred on the row and elided to it; the menu remembers the theme's text scale so clicks hit the rows where the last frame painted them (asserted). Where the viewport caps the panel (3× on 1280×720, 4× on 1280×800 — the code review's case) the rows shrink to tile the capped panel, never below the pointer target, and a row squeezed under its two lines drops the detail instead of painting through its neighbour (asserted). Anchor test (`x1 = viewport − 24`, `y0 = 60`) kept: `EDGE_INSET = s6`, `TOP_INSET = bar_height + s4 + s1`. Fixtures `power-menu/*.png` |
| Power menu | no accessibility node — reachable by the QS "Power" tile, not present in the tree | **open — needs S1** (the tree is assembled in `overlay_host.rs`) |
| OSD | caption 4× at 200 %; slab fill/border declared (`surfaces.overlay`, `hairline_on(overlay)`) | **fixed** — divide once; `panel_ground` + `panel_border`. At 200 % the stack is 88 of the 96 px slab (4 px margins) — fits; `rect_in` is pinned at 96 and left |
| Lock screen | literal 56/18/20/15/14/13/12 px sizes; status row changed size per state (14 / 13 / 12); panel fill `bg_primary`; renderer scale ignored | **fixed** — date `lg`, title `xl`, placeholder `base`, status and count `sm` (one role for every state), clock kept as a named display constant (`CLOCK_PX` 56 — no token that large), panel `panel_ground`, sizes divided by the renderer scale. Existing containment/large-text tests pass; fixtures `lock-screen/*.png` incl. 800×700 at 2× and the error state |
| Notifications | DND pill 28 px fixed at 200 % (label line 30); locked card / pill type 4× | **fixed** — pill = max(Selector 28, label + `s2`); divide once. Fixtures `notifications/*.png` (live with an error toast, DND with held items, locked). Toast card internals remain F4's (`toast::paint_card`) |
| Portal dialog | choice rows 38 tall on a 48 pitch (< Control row 44) | **fixed** — `panel_row_height` (44) on the same pitch; the painter (`draw_filter_chip`, S1) centres on the rect. Test `action_buttons_share_the_choice_rows_gutter` read: pins buttons only |
| Launchpad | `usability.py` `hit_geometry`: "Make something together" overlaps the wheel disc's `Switch to Home` / `Add a workspace` (baseline and compact-large-text) | **open — structural, needs the host layout (S1)**. The lint takes a labelled widget's *layout* rect (`usability.py:442`), and the launchpad's layout rect is the whole content tile, which by design extends under the floating workspace rail (§9.10); only the launchpad's *content* clears the rail (`card_geometry`). The Group node now publishes `content_bounds()` (heading band ∪ cards ∪ footer button) so the access tree describes what it presents, but that does not change what the lint measures — the tile rect would have to stop at the rail, or the lint would have to treat a container Group as a non-control. BEFORE and AFTER lint results are identical |
| Onboarding | sprite shadow, `raised` + `border_standard` on a floating card | **fixed** (see `soft_shadow` row); the existing 1.0/1.3/2.0 render tests pass |
| Editor tile, desktop backdrop | literal colours in draw paths (`from_rgba8(0,0,0,24)` divider; `from_rgba8(128,128,128,18)` grid) | **fixed** — hairlines derived against the surface they sit on (`hairline_on`) |
| Remote cursor chip | label 4× at 200 % | **fixed** — divide once. `radii.sm` kept: the label chip is a detail, not a pressable body |

## Off-grid metrics kept, and why

- `room_activity.rs` `VIEW_ROW_H 34`, `CHIP_ROW_H 32`, `HEADER_H 76` (now the
  1× floor of `header_h()`); the sheet's 8/12/16 insets. Moving them is a
  sheet redesign; the header and cards now grow with type instead.
- `quick_settings_drill_in.rs` `HEADER_HEIGHT 48 / LIST_TOP 56 / ROW_HEIGHT 56`
  (rows 52 tall vs the 44 Control row): `quick_settings_wifi/bluetooth/vpn.rs`
  (not owned) paint their glyphs at `row.y0 + 16..36` for a 52-tall row —
  **needs the QS pane owner** with `ROW_INSET`.
- `projection_overlay.rs` 20 px paddings (S2-30 above).
- `portal_dialog.rs` sheet `190 + 48·rows`, buttons 92/84×34, gutter 24: pinned
  by `action_buttons_share_the_choice_rows_gutter`; the paint is S1's.
- `notification.rs` `CARD_PADDING_X 14` (matches the shared card), `DOT_RADIUS 5`.
- `power_menu.rs` `TOP_INSET 60` — derived (`bar_height + s4 + s1`) and pinned
  by the anchor test.
- `collaborators_overlay.rs` mixed 8/12/16 insets (round one's S2-27) — untouched
  again; the known-red `collaborator_actions_are_absent_without_the_invite_capability`
  depends on this file's layout, which was not changed (only paint and the
  renderer reset). Its result is reported below, not "fixed".

## Code review (`/code-review high`) — findings and dispositions

| finding | disposition |
|---|---|
| Power menu laid rows on the uncapped pitch when the viewport capped the panel (3× on 720 px, 4× on 800 px): bottom rows painted past the panel and a click there closed the menu | **fixed** — `PowerMenuMetrics::row_height_in(panel)` tiles the rows inside the capped panel; detail line dropped when a row cannot hold two lines; `a_capped_panel_still_tiles_every_row_inside_it` |
| Onboarding comment claimed the high-contrast elevation ladder "silences" the shadow; `Theme::high_contrast()` builds on the dark ladder | **fixed (comment)** — the shadow is the one every Panel casts; on the high-contrast palette it is a black shadow on a black ground (`surfaces.base = BLACK`) and contributes nothing, the edge separates (§9.3). Behaviour kept consistent with every other Panel |
| Two modal durations (lens 210 from the token, host modals 220 from `overlay_host::MODAL_OPEN_ANIM_MS`); dead duplicate constant in the lens file | **fixed here** — the lens's dead duplicate is deleted, the power menu reads the motion token; **open — needs S1** for the host's constant (see S2-14) |
| Sidebar restyle and its test target a painter with no production caller | **recorded** — see S2-16; deletion or wiring is the owner's decision |
| Fixture tests asserted only that their own file exists, needed a GPU, and built a device per variant | **fixed** — pixel assertions on the production path, one device per output size, renderer-scale restoration asserted; the GPU requirement is kept deliberately (fail, not skip) |
| Six new per-file copies of the "divide by renderer scale" helper next to three files using a reset wrapper | **fixed** — every site this round uses the reset-for-the-pass idiom; the helpers are gone. Round one's `type_size` copies in QS root, drill-in and Room Activity remain (a shared `TextRenderer` helper is the F-unit fix recorded in the round-one README) |
| Second `#[path]` include of the offscreen harness | **recorded** — the host's include is private and `overlay_host.rs` is off limits this round; a single `pub(crate)` include in `shell/src/shell/mod.rs` is the right home when that file is free |
| `ROW_INSET` made `pub` while `quick_settings_wifi.rs` still hard-codes 12 | **fixed (honesty)** — the doc now says the pane still restates the literal at `:88,90,109,149` and that the constant is there for the pane owner to adopt |
| `#[cfg(test)]` free-function wrappers and `pub` metric fields nobody reads | **fixed** — wrappers deleted, tests call `PowerMenu::default().panel_rect/action_rect`, `label_h`/`detail_h` private |
| Fourth copy of `small_count_to_f64`, a second `text_width`, a tuple parameter to dodge the argument lint | **partly fixed** — the tuple is gone (the fn already carried the `too_many_arguments` allow); the two small helpers stay per file because the shared geometry module they belong in is not in this slice |

## Bounds and access tree

- Launchpad Group node carries its content bounds.
- Quick Settings sliders publish the same 32 px face the pointer presses.
- Power menu: rows are hit-tested at the painted (text-scale-aware) geometry.

## Tests

| check | command | result |
|---|---|---|
| Scoped shell unit tests | `cargo test -p shell --lib -- workspace_settings room_activity quick_settings notification lock_screen osd collaborators launchpad portal power_menu peer_sync projection onboarding drag_feedback remote_cursors editor_app desktop_backdrop` | **591 passed, 0 failed, 1 ignored** (1839 filtered out; final run after the code-review fixes); the ignored test is the pre-existing opt-in visual harness. The test build carries three pre-existing dead-code warnings in `collab_broker_capnp.rs` / `search/providers/live_app_actions.rs` (not owned) and the usual `__eh_frame` linker note. The known-red `overlay_host::tests::collaborator_actions_are_absent_without_the_invite_capability` is outside this filter; run on its own it still fails at the same assertion as at the base (`overlay_host.rs:22892`, "a guest is not offered the room's membership controls") — `collaborators_overlay.rs`'s layout, its input, is unchanged here (paint and the renderer reset only); not "fixed" |
| Bench schema tests | `cargo test -p ui --lib -- ux_bench` | **18 passed, 0 failed** |
| Python bench suite | `python3 scripts/ux-bench/test_suite.py` | **4 passed** (`OK`) |
| Compile gate | `cargo check -p shell --lib --bin shell --bin ux-bench` | **exit 0, no warnings** (`MODAL_OPEN_ANIM_MS` in `workspace_settings_overlay.rs` became test-only and is now `#[cfg(test)]`) |
| Integration target | `cargo check -p shell --tests` | not run: `shell/tests/it.rs` is Linux-only (macOS) |

Base note: at `381eff6a8` the shell lib test target did not compile —
`overlay_host.rs:20875` referenced an undefined `CHIP_PAD_PX` (the merge left
that line's fix unstaged). The coordinator committed the one-line fix as
`f11e61da6` on `codex/macos-shell`; it is cherry-picked here as `280e9b58b`
and is the only change to `overlay_host.rs` on this branch.

New inline tests (all in the files they cover):
`quick_settings_drill_in::tests::{row_floor_stays_below_the_hover_tier, discovery_row_glyph_is_the_token_glyph_centred_on_the_row}`,
`osd::tests::{caption_type_is_scaled_once_at_large_text, osd_review_fixtures_are_written}`,
`power_menu::tests::{rows_are_control_rows_on_the_token_pitch_at_default_text, large_text_grows_the_rows_and_the_panel_holds_them, a_capped_panel_still_tiles_every_row_inside_it, clicks_hit_the_rows_where_a_large_text_frame_painted_them, power_menu_review_fixtures_are_written}`,
`workspace_settings_overlay::tests::stat_tiles_keep_an_edge_on_every_palette`,
`room_activity::tests::{header_band_follows_the_text_scale_and_relayouts_on_draw, selector_pills_are_lifted_and_ordered_on_every_palette, brief_card_lines_never_overlap_at_large_text}`,
`lock_screen::tests::lock_screen_review_fixtures_are_written`,
`notification::tests::notification_stack_review_fixtures_are_written`.
Renamed: `workspace_settings::tests::sidebar_rect_is_256_px_wide` →
`sidebar_rect_is_the_token_sidebar_width` (asserts the token). No test was
ignored, weakened or deleted. The four fixture tests render through the same
offscreen wgpu harness `overlay_host.rs` uses (`shell/tests/support/offscreen.rs`,
declared once more as `osd::review_offscreen` because the host's include is
private and its file is off limits), one device per output size, and assert
on the production path — the rendered pixel of each surface's interior is the
Panel ground (OSD, power menu title band, lock card), the row's lift floor
(power menu) or the raised surface (notification pill, locked card, toast
card) — and that each pass hands the renderer's scale back. Like the host's
existing `directory_palette_fixture_writes_normal_and_narrow_pngs` they fail,
not skip, where no wgpu adapter exists (CLAUDE.md: a check that cannot observe
its subject must fail).

## Results

### 48-run matrix (`run-suite.py --matrix`)

| run | pass | fail | failing journeys |
|---|---|---|---|
| BEFORE `target/ux-bench/s2-r2-before` | 30 | 18 | Files scroll ×6 and Open Files ×6 (`semantic:Files` missing/ambiguous — the 12 known), plus browse-file ×6 (`semantic:Document-000.txt` missing) — all pre-existing at `381eff6a8` before any change here |
| AFTER `target/ux-bench/s2-r2-after` | **30** | **18** | exactly the same 18 pre-existing failures; every journey this slice touches (launchpad in `navigation-layout`/`desktop`, settings lens, overview) passes in all six variants |

### S2 scenarios (`scripts/ux-bench/s2/*.json` × six variants)

| run | quick settings | room activity | settings lens | total |
|---|---|---|---|---|
| BEFORE `s2-r2-before-s2` | 6/6 | 6/6 | 6/6 | **18/18** — round one's fixes survived the merge |
| AFTER `s2-r2-after-s2` (quick settings now also asserts the slider targets and their non-overlap) | 6/6 | 6/6 | 6/6 | **18/18** |

### `usability.py --mode deterministic` (`target/ux-bench/s2-r2-overlap*`)

| run | `hit_geometry` | other properties |
|---|---|---|
| BEFORE | fail ×2: `Make something together` overlaps `Switch to Home` / `Add a workspace` | `exact_document_outcome` fail ×2 (pre-existing Files/Document ambiguity); 7 properties pass ×2 |
| AFTER `s2-r2-overlap-after` | fail ×2, identical pair — structural (launchpad tile rect under the rail; see Fresh eyes) | `exact_document_outcome` fail ×2 (pre-existing); 7 properties pass ×2 — unchanged |

### Visual comparison (Read-tool inspected)

- Room activity, 200 %: `s2-r2-before-s2/011-s2-room-activity-large-text/activity.png`
  (scope pills over the title, subtitle through "Latest", card lines colliding)
  → `s2-r2-after-s2/011-…/activity.png` (title "Since you look…" elided before
  the pills, subtitle and "Latest" on their own lines, the view tabs below
  them, each catch-up card's byline / provenance / affordance on its own line).
- Quick settings, 1× and 200 %: `s2-r2-before-s2/000…/quick-settings.png`,
  `005…/quick-settings.png` — round-one state confirmed; AFTER slider faces
  are two 32 px bands (`s2-r2-after-s2/000…`, `005…`).
- Power menu: `target/ui-review/s2-r2/power-menu/dark-{1x,2x}-1280x800.png`,
  `dark-2x-800x700.png`, `light-…`, `high-contrast-…` — rows hold label and
  detail at both scales; HC ground clears the black desktop.
- Lock screen: `lock-screen/dark-{1x,2x}-1280x800.png`, `dark-2x-800x700.png`,
  `dark-2x-1280x800-error.png`.
- OSD: `osd/{dark,light,high-contrast}-1x-*.png`, `dark-2x-*.png` — caption
  2× not 4×; slab without a bar centred.
- Notifications: `notifications/{dark-1x,dark-2x,light-1x}-{live,dnd,locked}.png`.

## Limits

- macOS: `shell/tests/it.rs` (`--tests`) is Linux-only; `--shots`/`qos-control`
  unavailable. The lock screen, OSD, power menu, notifications and portal
  sheet cannot be opened by `ux-bench`; the first four are photographed from
  their paint paths by the fixture tests, the portal sheet's paint is S1's.
- The collaborators popover, peer-sync and projection sheets need peers or
  hardware the bench does not have; their changes are covered by the existing
  inline tests (layout unchanged) and by source review.
- Pre-existing matrix failures (Files ×12, browse-file ×6) and the
  `exact_document_outcome` lint failure are outside this slice.
