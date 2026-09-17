# S2 — Shell panels, sheets, transient and system surfaces

UI alignment, spacing and layout review of the shell's floating and system
chrome. One of eighteen parallel slices of the whole-OS review; this slice
owns the files listed under **Scope**. Branch `ui-review/s2-shell-panels-system`.

## Scope

`shell/src/shell/{workspace_settings, workspace_settings_overlay, room_activity,
quick_settings_root, quick_settings_drill_in, collaborators_overlay,
collaboration_launchpad, peer_sync_overlay, projection_overlay, portal_dialog,
power_menu, file_picker_overlay, notification, osd, remote_cursors, peer_view,
peer_join_toast, desktop_backdrop, notes_app, editor_app}.rs`,
`shell/src/compositor/{lock_screen, drag_feedback}.rs`, `shell/src/onboarding_view.rs`.

Governing text: `ui/HIG.md` §7, §7a and §9 (roles, density, type, motion).
Method: `docs/ux-testing-bench.md`. Two documents named in the brief —
`docs/ux-evidence-method.md` and
`docs/superpowers/audits/2026-09-16-ux-findings-repairs.md` — are not in the
committed tree at the base commit and were not available to this slice.

## Environment notes (affect every worker under `/Users/xaver/dev/qos/.claude/worktrees/`)

- The worktree was cut from `24ee08c66`, which is **not** on `codex/macos-shell`
  (196 commits diverged; no `ux-bench`, no `scripts/ux-bench/`). It was reset to
  `codex/macos-shell` HEAD `2d7e819b6` before any work, then `ui-review/lockfix`
  (`f9a579c9c`) was cherry-picked as instructed.
- Cargo merges `.cargo/config.toml` from every parent directory. The main
  checkout at `/Users/xaver/dev/qos/.cargo/config.toml` adds
  `-C target-cpu=generic` for non-Linux aarch64, which strips the AES/SHA2
  static features `ring 0.17.14` asserts at compile time (`E0080` in
  `cpu/arm/darwin.rs`). Every cargo invocation in this slice therefore sets
  `RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."`
  (exactly this tree's own macOS list) so the merged flags are replaced, not
  extended. Nothing in the repository was changed for this. (The coordinator
  later confirmed the same diagnosis and offered `--no-default-features` alone
  as an alternative; this slice used both together on every cargo command,
  with `RUSTC_WRAPPER=sccache CARGO_BUILD_JOBS=1` inline.)
- `scripts/ux-bench/usability.py` (overlap/target-size lint named in the brief)
  does not exist at this base; that step was not run.
- The committed `ux-bench` has no `--interactive` mode; exploratory captures
  were taken through throwaway scenario files under `target/ux-bench/s2-scn/`
  (`click_named` + `checkpoint`).

## Method

1. Source pass over every owned file against HIG §9 roles/density/type and the
   measured-text rule (`ui/src/widget/button.rs:739` pattern).
2. BEFORE evidence: the 48-run matrix (`target/ux-bench/s2-before`) plus four
   exploratory captures (quick settings root at baseline / 800×700 / 200 % text;
   the centre-clock control, which opens Room Activity). PNGs inspected with the
   Read tool; `*.inspection.json` used for rects.
3. Fixes expressed through `theme.tokens.*`, `ui::chrome::ChromeRole`,
   `selection_tier`, `panel_ground/panel_border/panel_shadow`, and measured text;
   pinned geometry left alone unless recorded below.
4. Inline `#[cfg(test)]` geometry tests per fix; new `scripts/ux-bench/s2-*.json`
   scenarios for the surfaces the bench can reach on macOS.
5. AFTER evidence: matrix, s2 scenarios across the six variants, comparison.

## Findings

Status: **fixed** (in this branch), **open** (recorded, not changed here),
**needs <unit>** (lives in a file another slice owns).

| id | surface | defect | theme / viewport | where | before / after | status |
|---|---|---|---|---|---|---|
| S2-01 | Quick Settings root | Title drawn from a top-left origin at `PAD+23` ("baseline"), landing 13 px below the Edit button's centre and touching the action tiles at y=104 | dark 1280×800 | `quick_settings_root.rs` `header()` | `target/ux-bench/s2-explore-qs-root/qs-root.png` → `target/ux-bench/s2-after-qs/*/quick-settings.png` | fixed: title measured and centred in `header_rect()` (the 40 px band the Edit button is centred in) |
| S2-02 | Quick Settings root at 200 % text | Chrome geometry clamps at 1.2× while type grows to 2×: title painted under the tiles, tile captions overflow, "Night Light"/"Off" and "Balanced"/"AC power" overlap each other and the panel bottom | dark 1280×800, `text_scale` 2 | `quick_settings_root.rs` row constants `ACTIONS/CARD/POWER` | `target/ux-bench/s2-explore-qs-root-large/qs-root.png` → `s2-after-qs/…-large-text/quick-settings.png` | fixed: `action_height/card_height/power_height` = max(chrome constant, label offset + measured line heights from `TextStyle::line_height_px` at the user's scale); sub-lines follow the measured label; `fit_root_layout_to_height` walks the chrome scale down until the text-aware total fits |
| S2-03 | Quick Settings root | Panel fill radius literal `18.` (off-scale) painted over the host's Panel-role card | all | `quick_settings_root.rs` `draw()` | — | fixed: `ChromeRole::Panel.radius` |
| S2-04 | Quick Settings root | No access node carries bounds (`bounds: None` for Edit/tiles/sliders/switches); the power-profile row is absent from the tree although it takes keyboard focus | all | `quick_settings_root.rs` `accessibility_snapshot` | `s2-explore-qs-root/qs-root.inspection.json` | fixed: every node `.with_bounds(...)` from the same rects paint and hit-testing use; "Change power profile" added; bounds attached only once laid out |
| S2-05 | Quick Settings root | Slider faces are 30 px tall on a 28 px pitch (< 32 minimum target) | all | `quick_settings_root.rs` `slider_face_rect` | — | open: raising to 32 makes the two faces overlap; needs the slider block re-spaced (design) |
| S2-06 | Quick Settings drill-in | Rows inset 12 px while the root page's cards inset 16 px (`PAD`); row fill from `neutral[2]/[3]`; radius literal 12 | all | `quick_settings_drill_in.rs` | — | radius → `ChromeRole::Control.radius`, fills → `colors.lift(...)` fixed; the 12 px inset kept because `quick_settings_wifi.rs:88,109` (not owned) draws its hotspot row at `panel.x0 + 12` and would misalign — **needs the QS pane owner** |
| S2-07 | Room Activity | Subtitle and chip labels at `scale.sm - 1.0` (an invented 11 px size) | all | `room_activity.rs` (4 sites) | `target/ux-bench/s2-explore-clock/clock.png` | fixed: `scale.sm` |
| S2-08 | Room Activity | Wrapped subtitle at `y0+32` collides with "Latest ·" pinned at `y0+51` inside the fixed 76 px header | narrow sheets / long subtitles | `room_activity.rs` `draw_title_and_subtitle` | source | fixed: subtitle measured with its wrap width; "Latest" follows it; falls back to one elided line when the stack cannot clear the view tabs. Tasks branch: both lines elided, second follows the first's measured height |
| S2-09 | Room Activity | View tabs painted 18 px tall, scope pills 22 px, filter chips 22 px — under the 28 px Selector height and the 32 px minimum target; hit rect = painted rect | dark 1280×800 | `room_activity.rs` `draw_chip_row*`, `draw_scope_chips`, `draw_view_tabs` | `s2-explore-clock/clock.png` → `s2-after-ra/*/activity.png` | fixed: pills painted at `SELECTOR_H` 28, hit rects are the full 32 px row (`MIN_TARGET`), `CHIP_ROW_H` 30 → 32; "More…" centred by measurement |
| S2-10 | Room Activity | Access nodes carry no bounds | all | `room_activity.rs` `accessibility_control_node` | `s2-explore-clock/clock.inspection.json` | fixed: `.with_bounds(rect_for_focus)`; dialog carries the sheet |
| S2-11 | Room Activity | Active chip is an accent fill (`accent[5]`), §9.3 says selection is a lift | all | `room_activity.rs` | — | open (colour semantics, not alignment; recorded for the design pass) |
| S2-12 | Workspace settings lens, 200 % text | Tab strip switches to compact labels on lens width alone, decides icons per pill, and splits the strip into equal fifths: active "General" elided while its neighbours had slack and only "Apps" kept an icon | dark 1280×800, `text_scale` 2 | `workspace_settings_overlay.rs` tab strip | `target/ux-bench/s2-before/047-workspace-settings-large-text/settings.png` → `s2-after3/047-…/settings.png` (uniform plan, "General" still elided in equal fifths) → `s2-after5-s2/017-…/lens.png` | fixed: `tab_strip_plan()` measures the whole strip once and picks full+icons → compact+icons → full → compact, uniformly; `tab_pill_widths()` keeps equal fifths while every label fits and otherwise gives each pill its measured share, so no tab is elided while another has slack. 1× geometry unchanged |
| S2-13 | Workspace settings lens | Close button 30×30 (< 32 minimum target) | all | `workspace_settings_overlay.rs` `close_rect_for` | — | fixed: 32×32 on the same centre line (deliberate geometry change; the only test touching it clicks the centre) |
| S2-14 | Workspace settings lens | Panel role drift: `radii.lg`, `surfaces.base`, `soft_shadow(40,16,0.62)`, bespoke `MODAL_OPEN_ANIM_MS` 220 and a 48 px x-travel entrance instead of §9.7's | all | `workspace_settings_overlay.rs` `begin_draw` | — | open: WIP file, edits kept minimal by instruction |
| S2-15 | Workspace settings lens, high contrast | "At a glance" stat tiles lose their surface (`raised == base`) so the numbers float | high-contrast | `workspace_settings_overlay.rs` | `s2-before/045-…/settings.png` | open (§9.3 caveat; needs a hairline floor) |
| S2-16 | Workspace settings sidebar | `neutral[1]/[3]` in chrome, identity radius literal 16 (off-scale), nav text at fixed `+10` | — | `workspace_settings.rs` `draw_sidebar` | — | open: `draw_body` is only reached from tests; not on the live path |
| S2-17 | Home launchpad | Keyboard hint at `last.y1+22` while the "Open an app" button starts at `last.y1+14` with a ≥36 px height: centres ~4 px apart; button label at an authored (16, 8) offset | dark 1280×800, 800×700 | `collaboration_launchpad.rs` | `s2-before/036-workspace-overview-baseline/overview.png` (hint y≈598 vs button centre ≈601) | fixed: hint centred on the button's centre line; label centred by measurement |
| S2-18 | Home launchpad | Card radius literal 16 (off-scale), button radius literal 8 | all | `collaboration_launchpad.rs` | — | fixed: `ChromeRole::Control.radius` (12) for both |
| S2-19 | Notifications | Locked card and DND pill centre text by font size, not measured line; pill 30 px (off-grid) | all | `notification.rs` | source | fixed: measured; pill = `SELECTOR_HEIGHT` 28 |
| S2-20 | Notifications | `CARD_PADDING_X` 14 (off-grid); toast card internals | all | `notification.rs`; card painted by `ui::widget::toast::paint_card` | — | open (14 kept: changing it moves the DND dot/label without evidence); card internals **need F4** |
| S2-21 | Remote cursors | Name chip height = font size + 2·2; the measured text height was discarded, so the label rides onto the 2 px underline | all | `remote_cursors.rs` | source | fixed: chip wraps the measured line |
| S2-22 | Drag chip | Literal 13 px font, centring by `size × 1.3`, radius literal 8, shadow `Color::from_rgba8(0,0,0,90)`, height 30 | compositor | `compositor/drag_feedback.rs` | source-only | fixed: `scale.sm`, measured centring, `ChromeRole::Control.radius`, `panel_shadow`, `CHIP_H` 28 |
| S2-23 | OSD slab | Radius literal 18 (off-scale), `soft_shadow(36,14,0.5)`; glyph/bar/caption at authored offsets so a slab without a level bar is top-heavy (glyph 12–40, caption 69–84 in 96) | compositor | `osd.rs` | source-only | fixed: `OsdStack::layout` centres the stack with `spacing.s3` gaps and `spacing.s6` insets; Panel radius + `panel_shadow`. With a bar the rhythm is within 1 px of the old one |
| S2-24 | Portal dialog | Choice rows inset 24 px but Cancel/Confirm end at `sheet.x1 - 20`: right edges 4 px apart | compositor / Linux portals | `portal_dialog.rs` `layout` | source-only | fixed: buttons end at −24 (sizes 92/84, gap 12 unchanged) |
| S2-25 | Portal dialog | Sheet radius literal 16, literal 20/14 px fonts, detail text unwrapped | — | `overlay_host.rs` `draw_portal_dialog_sheet` | — | **needs S1** |
| S2-26 | Power menu | `neutral[1]` panel with literal radius 24, rows `neutral[2]` with literal radius 14, selected row an accent wash; detail at a fixed +28 | compositor / QS → Power | `power_menu.rs` | source-only | fixed: `panel_ground` + `panel_border` + Panel radius; rows `Control.radius`, idle `lift(0.35)`, selected `selection_tier(active)`; detail follows the measured label |
| S2-27 | Collaborators popover | Follow/Stop, Invite, Join buttons 28 px tall (< 32 target) with `radii.sm` bodies (§9.1 reserves sm for details); mixed 8/12/16 insets | — | `collaborators_overlay.rs` | source-only (no peers on the bench) | fixed: 32 px targets centred in their bands via one `row_action_rect`; `Control.radius`. Insets left (would move every row without evidence) |
| S2-28 | Lock screen | Panel radius `24.0 * scale` literal; border `border_standard` | compositor | `compositor/lock_screen.rs` | source-only | fixed: `radii.xl * scale`, `panel_border`. Literal strings and 56/18/20/13 px display sizes kept (compositor has no fluent; the existing layout tests pin containment) |
| S2-29 | Peer-sync sheet | Hand-rolled 30 px pill buttons (§9.9 wants `Button::paint`), sheet 400–520 wide (form sheet is 560) | — | `peer_sync_overlay.rs` `draw_buttons`, `card_rect` | source-only | open |
| S2-30 | Projection sheet | Panel role adopted; 20 px paddings (off-grid) | — | `projection_overlay.rs` | source-only | open (minor) |
| S2-31 | Editor tile | Hint centred by font-size difference; fixed 48 px header at 200 % would reach the body | — | `editor_app.rs` | source-only | not changed: the coordinator reports this repaired in the user's uncommitted WIP; verified the committed stack is correct at 1× |
| S2-32 | Onboarding | `ui` widgets, 41 token refs; existing render tests at 1.0/1.3/2.0 scales | — | `onboarding_view.rs` | tests | no defect found from source; tests run below |
| S2-33 | Workspace overview panel | Panel radius 12, "You are here" chip vs title | dark 1280×800 | `overlay_host.rs` | `s2-before/036-…/overview.png` | **needs S1** (not owned) |
| S2-34 | Quick Settings panel width | 400 px; §9.6 says 320 for a utility Panel | all | `overlay_host.rs` `quick_settings_rect` | — | **needs S1** |
| S2-35 | Density tokens | No shared constants for Selector 28 / Control 44 / min target 32 exist in `ui::chrome`; each file declares its own `SELECTOR_H`/`MIN_TARGET` | — | `ui/src/chrome` | — | **needs F1** |
| S2-36 | Room Activity at 200 % text | The view tabs did not fit the 435 px sheet and `draw_chip_row` dropped Explore/Replay behind a "+N" marker | `text_scale` 2, 1280×800 | `room_activity.rs` `draw_view_tabs` | `target/ux-bench/s2-after2-s2/011-…/activity.png` → `s2-after4-s2/011-…/activity.png` | fixed as a consequence of S2-38: at the correct 2× the four tabs fit and the 200 % scenario variant passes |
| S2-39 | Room Activity at 200 % text | With type at its correct 2×, the fixed 76 px header (`HEADER_H`) cannot hold title, subtitle and "Latest" (subtitle overlaps the title's descenders; the view row overlaps "Latest"), and the catch-up cards' lines ("xaver · just now / 1 underlying action / Show affected…") sit at authored offsets and collide | `text_scale` 2 | `room_activity.rs` `chrome_height`, `draw_title_and_subtitle`, `draw_catch_up` | `target/ux-bench/s2-after4-s2/011-s2-room-activity-large-text/activity.png` | open: `layout()` runs without a renderer, so a text-aware header needs the pane to cache measured line heights and relayout during `draw` — a reflow change, recorded for follow-up |
| S2-37 | Home launchpad hint probe | `navigation-layout.json` samples the hint glyph at fixed pixels `[553,599]` (and `run-suite.py` re-authors them for compact / large-text); moving the hint's line breaks every variant of that journey | all | `scripts/ux-bench/navigation-layout.json`, `run-suite.py` | first AFTER matrix (`target/ux-bench/s2-after`): 18 fails incl. 6 new contrast fails | resolved without touching probes: the **button** is centred on the hint's line instead, so the hint pixels are unchanged |

| S2-38 | Every overlay pass except the lens/overview one | `overlay_host.rs:12743` resets the renderer's text scale to 1.0 only around the workspace-settings and overview overlays. Quick settings, its drill-ins, room activity, collaborators, projection, peer-sync, the power menu and the OSD are painted with the renderer still at the widget scale while their theme sizes are already scaled, so at 200 % text their type renders at **4×** (`target/ux-bench/s2-after2-s2/005-…/quick-settings.png`, `011-…/activity.png`) | `text_scale` 2, all themes | `quick_settings_root.rs`, `quick_settings_drill_in.rs`, `room_activity.rs`; the pass itself in `overlay_host.rs` | 4× → 2×: `target/ux-bench/s2-after4-s2/005-…/quick-settings.png`, `011-…/activity.png` | fixed in the three files with bench evidence by dividing each resolved size by the renderer scale once (`type_size`, the HIG §7a rule; identity at 1× and in a pass that resets). Collaborators, projection, peer-sync, power menu and OSD still double at large text: **needs S1** (reset the renderer in those passes as the lens pass does) or the same per-file division |

### Deviations from the brief

- The three new scenarios live in `scripts/ux-bench/s2/`, not at
  `scripts/ux-bench/s2-*.json`: `scripts/ux-bench/test_suite.py` pins the
  top-level directory at exactly eight scenario files, and `run-suite.py`'s
  default matrix globs that directory. Top-level placement turned the suite
  red (`11 != 8`) and would have grown the pinned 48-run matrix. The
  subdirectory keeps both honest; the s2 scenarios are run explicitly.
- The compile gate `cargo check -p shell --lib --bins --no-default-features`
  cannot pass on macOS at this base: `shell/src/bin/shots/driver.rs:24`
  imports `shell::compositor::capture`, which is `#[cfg(target_os = "linux")]`
  (`shell/src/compositor/mod.rs:19`). Verbatim:
  `error[E0432]: unresolved import shell::compositor::capture`. The gate run
  here is `--lib --bin shell --bin ux-bench`.

### Reuse review

The code-review pass asked for `Button::paint` / `Chip::draw_at` /
`selection_tier` in place of hand-painted copies. Applied: `selection_tier`
for the power menu's chosen row; `Button::paint` already carries the
collaborators' buttons. Declined for the Room Activity chips: `Chip::draw_at`
paints `surface_white_5` (deprecated in chrome by §9.5) with a full-strength
border and `radii.full` on a 28 px rect, and returns no hit rect, so adopting
it would change the chips' colour tier and lose the paint/hit coupling this
review just tightened. They keep token colours and measured text; the
tier question is S2-11.

## Bounds and test ids added

- Quick Settings root: Dialog, Edit controls, Settings/Privacy/History/Power,
  Brightness/Volume, every control switch/button, and the new
  "Change power profile" node all carry the rect their paint and hit-test use.
- Room Activity: the dialog carries the sheet; every focusable chip/tab/button
  carries `rect_for_focus`.

## Off-grid metrics kept, and why

- `workspace_settings.rs` `SECTION_HEADER_HEIGHT 26`, `CONTROL_HEIGHT 30`,
  `COMPOSER_HEIGHT 78`, `SIDEBAR_WIDTH 256`: the composer and rows are pinned by
  `permissions_pane_layout` consumers and the sidebar by tests at :3671/:3698;
  changing them is a layout redesign, not an alignment fix.
- `notification.rs` `CARD_PADDING_X 14`, `DOT_RADIUS 5`: moving them needs a
  toast render to judge; the shared toast card owns the visible layout.
- `quick_settings_root.rs` action/card/power constants 58/70/50: the text-aware
  minimum equals them at 100 % (`default_text_scale_keeps_the_authored_row_heights`),
  so 1× geometry is unchanged to within 0.5 px.
- `room_activity.rs` `HEADER_H 76`, `VIEW_ROW_H 34`: the header stack was made
  to fit inside them instead of moving them.
- `power_menu.rs` rows 50 tall on a 58 pitch (Control row is 44): pinned by the
  anchor test's containment; visually generous rather than wrong.

## Surfaces reviewed from source only

Lock screen, OSD, drag chip, portal dialog, power menu, collaborators popover,
peer-sync and projection sheets, remote cursors, notifications, file picker,
peer view, desktop backdrop, peer-join toast, notes/editor tiles, onboarding.
None can be reached by `ux-bench` on macOS (compositor input, peers, or
Linux-only portals); their fixes are covered by the inline geometry tests below.

## Tests and bench runs

Every cargo command ran as
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 cargo … --locked --no-default-features`
(workaround (b) from the coordinator's note, plus the documented macOS mode).

| check | command | result |
|---|---|---|
| Scoped shell unit tests | `cargo test -p shell --lib --no-default-features -- quick_settings room_activity workspace_settings notification lock_screen osd collaborators portal_dialog power_menu launchpad onboarding drag_feedback remote_cursors` | **346 passed, 0 failed, 1 ignored** (2013 filtered out); `target/s2-test-shell6.log`. The ignored test is the pre-existing opt-in visual harness. |
| Bench schema tests | `cargo test -p ui --lib ux_bench --no-default-features` | **18 passed, 0 failed**; `target/s2-test-ui.log` |
| Python bench suite | `python3 scripts/ux-bench/test_suite.py` | **4 passed** (`OK`); `target/s2-pytest2.log` |
| Compile gate | `cargo check -p shell --lib --bin shell --bin ux-bench --no-default-features` | **exit 0**; `target/s2-gate2.log`. `--bins` fails on the Linux-only `shots` binary (see Deviations). |
| Integration target | `shell/tests/it.rs` | not compiled: Linux-only |

New inline tests (all in the files they cover):
`quick_settings_root::tests::{large_text_rows_grow_to_hold_their_type_and_the_title_stays_clear_of_the_tiles, default_text_scale_keeps_the_authored_row_heights, accessibility_nodes_carry_the_rects_paint_and_hit_testing_use}`,
`room_activity::tests::chips_are_full_pointer_targets_that_publish_their_bounds`,
`workspace_settings_overlay::tests::{tab_strip_plan_compacts_every_label_before_it_elides_any, tab_pills_take_measured_widths_only_when_equal_shares_cannot_hold_the_widest_label, close_button_is_one_pointer_target}`,
`osd::tests::slab_stack_is_centred_with_token_gaps_with_and_without_a_level_bar`,
`collaborators_overlay::tests::every_button_is_at_least_one_pointer_target_and_centred_in_its_band`,
`portal_dialog::tests::action_buttons_share_the_choice_rows_gutter`.
No existing test was ignored, weakened or deleted.

## Results

### 48-run matrix (`run-suite.py --matrix`, the eight pinned journeys × six variants)

| run | pass | fail | failing journeys |
|---|---|---|---|
| BEFORE `target/ux-bench/s2-before` | 36 | 12 | Scroll Files ×6 (`step 1: missing or ambiguous wheel target semantic:Files`), Open Files ×6 (`files-open: Visible { target: "semantic:Files" }: missing or ambiguous target`) — pre-existing, outside this slice |
| AFTER pass 1 `s2-after` | 30 | 18 | the 12 above + "Desktop navigation geometry and contrast" ×6: the hint glyph left its probe pixel (S2-37) — **rejected**, fix re-done via the button |
| AFTER pass 2 `s2-after2` | 35 | 13 | the 12 above + navigation contrast — compact only: the button's new top changed the launchpad overflow guard and shifted the grid 4 px (S2-37) — **rejected**, footer band introduced |
| AFTER pass 3 `s2-after3` | **36** | **12** | exactly the 12 pre-existing Files failures; every S2 change visible in the matrix (launchpad, settings lens) passes in all six variants |

The divide-once change (S2-38) landed after pass 3; it touches only quick
settings, its drill-ins and room activity, none of which the eight matrix
journeys open, so pass 3 remains the AFTER matrix.

### S2 scenarios (`scripts/ux-bench/s2/*.json` × six variants = 18 runs)

| run | quick settings | room activity | settings lens | total |
|---|---|---|---|---|
| `s2-after2-s2` (before divide-once) | 6/6 | 5/6 (200 %: Explore/Replay dropped, "All workspaces" 28.5 px wide) | 6/6 (after adding the scroll step; 200 % hid "At a glance" unscrolled) | 17/18 |
| `s2-after4-s2` (after divide-once) | 6/6 | 6/6 | 6/6 | 18/18 |
| `s2-after5-s2` (final: + measured tab pill widths) | **6/6** | **6/6** | **6/6** | **18/18** |

Final gate for this state: `target/s2-gate3.log` (exit 0, no warnings);
final scoped tests: `target/s2-test-shell6.log` (346 / 0 / 1).

Checks exercised per checkpoint: `visible`, `target_size` ≥ 32×32 on tiles,
switches, chips and the power row, `aligned` (tolerance 1) across tile rows,
slider column and chip rows, `gap` on the tile and chip pitch and the card →
power row spacing, `no_overlap` between title band, tiles, switch and power
row and between chips.

### Visual comparison (Read-tool inspected)

- Quick settings, dark 1280×800: `s2-explore-qs-root/qs-root.png` (title 13 px
  below the Edit centre, touching the tiles) → `s2-after4-s2/000-…/quick-settings.png`
  (title centred on the Edit line, clear of the tiles).
- Quick settings, 200 %: `s2-explore-qs-root-large/qs-root.png` (4× type,
  title under the tiles, labels overflowing and overlapping) →
  `s2-after4-s2/005-…/quick-settings.png` (2× type, every row holds its lines).
- Room activity, dark: `s2-explore-clock/clock.png` (18 px tabs, 22 px pills,
  11 px subtitle) → `s2-after4-s2/006-…/activity.png` (28 px pills in 32 px
  targets, `sm` subtitle, "Latest" seated below it).
- Room activity, 200 %: `s2-after2-s2/011-…/activity.png` (4×, unreadable) →
  `s2-after4-s2/011-…/activity.png` (2×; tabs reachable; header/card stack
  still collides — S2-39).
- Settings lens, 200 %: `s2-before/047-…/settings.png` (active "General"
  elided, one stray icon) → `s2-after3/047-…/settings.png` (uniform plan,
  "General" still elided in equal fifths) → `s2-after5-s2/017-…/lens.png`
  (all five labels intact on measured pill widths).
- Launchpad: `s2-before/036-…/overview.png` (hint/button centres ~4 px apart)
  → `s2-after3/036-…/overview.png` (one centre line; hint pixels unchanged).

## Limits

- `shell/tests/it.rs` (the `--tests` integration target) is Linux-only and was
  not compiled; the compile gate here is `cargo check -p shell --lib --bins
  --no-default-features`.
- `target/debug/shell --shots` is dark-only at 1280×800 and was not used as
  theme evidence.
- Pre-existing matrix failures (Files scroll ×6, Open Files ×6) are outside
  this slice and reproduced verbatim below.
