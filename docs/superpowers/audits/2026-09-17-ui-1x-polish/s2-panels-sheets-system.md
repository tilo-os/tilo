# S2 — Shell panels, sheets and system surfaces at the 1× bar

Branch `ux1x/s2-panels-sheets-system`, cut from Unit 0 `fd1f84391`. Files edited (owned
list only): `shell/src/shell/{workspace_settings_overlay,workspace_settings,room_activity,
collaborators_overlay,collaboration_launchpad,peer_sync_overlay,projection_overlay}.rs`,
`shell/src/onboarding_view.rs`, `shell/src/bin/shots/driver.rs`,
`scripts/ux-bench/s2/s2-workspace-settings-rhythm.json` (new). Not touched: `overlay_host.rs`,
`power_menu.rs`, `osd.rs`, `lock_screen.rs`, `drag_feedback.rs`, `portal/**`, panel widths,
keyboard/focus routing, portal and lock-screen logic.

Evidence: `target/ux1x/s2/{before,after}/{dark,light}/…png` (shots, 1280×800, 1×; scenario
frames under `<theme>/<scenario>/NN-step.png`), `target/ux1x/s2/before/<theme>/extra/` (BEFORE
frames of the six new scenes, painted with the Unit 0 paint files and the new driver),
`target/ux1x/s2/{before,after}/{ws-dark,ws-light,rhythm-dark,rhythm-light,matrix}/` (ux-bench
PNG + inspection + report). Every AFTER PNG (51 dark + 51 light) was opened with the Read tool
after the pre-review rebuild; after the code-review fixes the whole set was regenerated once
more (`after/`), every S2-owned surface re-opened in both themes, and the frames untouched by
the fixes compared byte-for-byte against the pre-review set (`after/changed-vs-prev.txt`; the
differences outside S2 surfaces are the top-bar clock). The interrupted session's captures
are kept under `target/ux1x/s2/checkpoint-after/`, the pre-review set under `after-prev/`.

## Six-rule checklist

1. **Nothing touches or overlaps.** Apps pane empty state no longer paints across the
   section header and column labels (`workspace_settings.rs::draw_default_apps_pane`,
   `default_app_rows_top`); onboarding eyebrow no longer sits on the panel's top edge and the
   progress track no longer runs into the corner radius (`onboarding_view.rs`); invite-field
   caret no longer overlaps its placeholder (`peer_sync_overlay.rs::draw_naming`). Pair table
   below: every adjacent pair ≥ `s1`.
2. **Every surface has a derived edge or contrast step.** All 21 `border_standard` strokes on
   raised/lift/base grounds in the five owned files are gone (ratchet: `now 0` for each).
   Cards → `ui::surface::card` (`edge_on(fill)`), fields → `edge_on(base)`, separator →
   `ui::surface::separator(.., s3, panel_ground)`, Panel edges → `panel_border` on
   `panel_ground` (room-activity sheet joins the other Panels), chosen states →
   `selection_tier`, capsule tabs → Selector radius. Accent washes used for *selection*
   (room-activity lens row, detail chips, projection cells/rows/chips, invite recipient/role
   pills, launchpad hover) → lift tier (HIG §9.3/§9.4). Every card class has a
   `*_keep_an_edge_on_every_palette` test (dark/light/high-contrast, Δ luminance ≥ 0.008).
3. **One rhythm per panel.** Workspace-settings lens header: 14 / 10 / 22 (General) / 26
   (panes) → `s4` / `s4` / `s4` / `s4` (`identity_rect_for`, `nav_rect_for`, `body_rect_for`,
   `PANE_PAD_TOP = s2` completing the body's `s2` lead-in; the pane's bottom inset is its own
   `PANE_PAD_BOTTOM = s3`, so the row-overflow guard and footer copy keep their authored step).
   Cards inside stay `s4`, rows `s2`, intro → section header `s3`. Collaborators footer:
   separator inset `s3`, band gaps `s1`/`s2`. Onboarding header: `s3` pad, `s3` track inset,
   labels on the body's `s6` gutter. Every 1 px tier or idle edge S2 strokes sits on the
   shape's outermost pixel row (`ui::surface::edge_outline`), the same row `ui::surface::card`
   uses, so one sheet's hairlines never land half a pixel apart.
4. **Centred on measured boxes.** Close chip centred on the identity card's centre line
   (was pinned 14 px above it); tab pills one `min_pointer_target` tall inside an `s1` track;
   identity mark = `min_pointer_target + s4` tile with `s4` around it, so the 76 px text column
   is `s4 + mark + s3`. Power menu, OSD, lock screen verified: icons/labels centred on rows and
   the slab, in light as well (unchanged).
5. **No authored-pixel exemption.** Every kept offset in the header is a token expression
   (`Spacing::BASE.sN`, `Metrics::BASE.*`); the two new consts flagged by
   `f1_spacing_token_audit` were rewritten as token expressions (count stays at the baseline 3).
6. **Evidence is visual.** Before/after PNGs at 1× dark and light for every touched surface
   (table below), opened and described; ux-bench PNG + inspection + `geometry_audit` for the
   lens; the `shots` binary itself had to be unbroken first (see Limits).

## Surface table (1×, 1280×800; `B` = before, `A` = after)

| Surface (PNG under `target/ux1x/s2/<phase>/<theme>/`) | Defects in B (rule) | A dark | A light |
|---|---|---|---|
| `ws-settings-general.png` | 5 cards/fields with `border_standard` on raised/base — invisible on dark (2); header 14/10/22 (3); close chip top-pinned (4) | every card, the name field, the invite pill and the stat tiles carry a hairline; 16/16/16; chip centred; active tab a lifted capsule | edges keep the light look (`border_standard`-like contrast), same rhythm |
| `ws-settings-members.png` | row cards edge-less (2); role cyclers declared edge (2) | row cards and cyclers edged; intro→header `s3`, rows `s2` | same |
| `ws-settings-permissions.png` | intro, composer and row cards edge-less; cycler tracks declared (2) | all edged; composer card edged | same |
| `ws-settings-apps.png` | empty state painted over "DEFAULT APPLICATIONS" and the column labels (**1**) | empty state starts under the label line, `s2`+ below it | same |
| `ws-settings-agents.png` (new scene) | intro card edge-less (2) | edged; empty state under the Add-agent button | same |
| `room-activity.png`, `-all`, `-named` | sheet `surfaces.raised` + `border_standard` (2) | Panel role: `panel_ground` + `panel_border`; entry cards read as cards | white sheet with visible border |
| `collaboration_product_proof/05-history-comparison.png`, `room_activity_explore_detail_compare/*` | "Everything" lens row accent-filled (§9.3); Compare/Undo chips declared edge (2); radius 6 off-scale | lens row = lift + hairline; chips edged at the Control radius; unavailable-comparison card edged | same |
| `room_activity_replay/*` | transport chips declared edge; canvas `border_standard`, radius 10; group cards declared edge (2) | chips, canvas (`radii.lg`), group cards (`radii.md`) edged; Pause keeps the live accent | same |
| `collaborators.png` (new scene), `…/02-invite-pending.png` | separator `border_standard`, inset 8 (2, §9.5); activity row `radii.sm`, no edge | separator derived on `panel_ground`, inset `s3`; row at the Control radius with an edge; Invite/Join band present | same |
| `peer-sync.png` (new scene) | recipient/role choice pills accent-filled (§9.3); caret on top of the placeholder (**1**); rows/mini pills bare lift | chosen pill = lift + hairline; caret 1.5 px + `s1` before the placeholder; rows and Copy/Cancel pills edged | same |
| `projection.png` (new scene) | selected cell accent wash + 1.5 px accent stroke, idle cells/chips/rows bare lift, radii 12/9 literals (2, §9.3) | cells/chips/rows edged at the Control radius; selected = lift + hairline | same |
| `launchpad.png` (new scene) / bench `initial.png` | card edge `border_standard·0.72` invisible on the dark grid; hover accent wash (2, §9.4) | idle cards edged from their floor; hover/keyboard = lift + hairline; focus ring kept | edged on the light grid |
| `host_desktop_first_login/01-…08` | eyebrow 3 px under the panel edge; progress rule full-bleed into the corner radius (1, 3) | header `s3` off the edge, track `s3` inside both edges, labels on the `s6` gutter | same |
| `portal-permission.png` (new scene) | — (paint lives in `overlay_host.rs` / `ui::widget::PermissionPrompt`) | captured for S1/F2; see Open | same |
| `power-menu*.png`, `media_key_osd/*`, `lock_screen_states/*`, `wifi_password_prompt/*`, `file_chooser_dialog/*` | reference / verify only | unchanged, rules 1/3/4 hold in light too | unchanged |

Descriptions of the key frames (opened after the final rebuild): dark `ws-settings-general` B
shows five flat raised blocks with no outline, the close chip 14 px above the identity card's
centre line and a 10 px gap above the tab strip beside a 22 px gap below it; A shows a
hairline around every card and field, three equal 16 px steps and the close chip on the
identity card's centre line. Dark `ws-settings-apps` B shows the LayoutGrid glyph and "No file
handlers installed" painted across the intro card, the "DEFAULT APPLICATIONS" header and the
"Workspace choice / Your choice" labels; A shows the labels alone and the empty state below
them. Dark `peer-sync` B (extra) shows two solid accent pills and the caret touching "Their
name"; A shows a lifted pill with a hairline and a caret one `s1` before the placeholder.
Dark `projection` B (extra) shows a blue-washed "Duplicate" cell with a blue ring and a blue
"Auto" chip; A shows a lifted cell and chip with a hairline and edged idle cells. Dark
`collaborators` B (extra) shows the divider ending 8 px from the panel edge and an edge-less
footer row; A shows the `s3`-inset divider and an edged row. Dark `room-activity` B shows a
raised sheet with an invisible declared border; A shows the Panel ground with its derived
border and edged entry cards. Dark `launchpad` B shows six cards with no visible edge on the
grid; A shows a light hairline around each. Light frames of all of the above keep their edges
and show no regression (white cards on the `#EFEFEF` panel ground with a black-at-α hairline;
lock screen, OSD slab, Wi-Fi prompt and file chooser unchanged in both themes).

## Pair table (gap px, before → after, 1×)

| Surface | Element A | Element B | Axis | Before → After | Relationship / step | OK |
|---|---|---|---|---|---|---|
| ws-settings lens | panel top | identity card | y | 14 → 16 | section pad `s4` | ✓ |
| ws-settings lens | identity card | tab strip | y | 10 → 16 | section `s4` | ✓ |
| ws-settings lens | tab strip | first content card (General) | y | 22 → 16 | section `s4` | ✓ |
| ws-settings lens | tab strip | intro card (People/Agents/Privacy/Apps) | y | 26 → 16 | section `s4` | ✓ |
| ws-settings lens | close chip centre | identity card centre | y | −14 → 0 | centred (rule 4) | ✓ |
| ws-settings lens | close chip | identity card trailing edge | x | 12 → 12 | `s3` | ✓ |
| ws-settings lens | identity mark | identity card edge | x/y | 14 → 16 | `s4` | ✓ |
| ws-settings lens | tab pill | track edge | x/y | 3 → 4 | `s1`; pill = `min_pointer_target` (36 → 32) | ✓ |
| ws-settings lens (ux-bench `ws-*`, `rhythm-*`) | dialog edge | `Workspace settings content` | x / y | 16 / 160 → 16 / 160 | panel pad `s4`; header block | ✓ |
| ws-settings lens (Rust `general_name_field_sits_on_the_card_pad_and_clears_the_summary_card`) | `Workspace settings content` | `Workspace name` field | x | 16 → 16 (both sides) | card pad `s4` | ✓ |
| General | identity card | sharing card | y | 16 → 16 | `s4` | ✓ |
| General | sharing card | summary card | y | 16 → 16 | `s4` | ✓ |
| General (Rust test above; ux-bench `ws-*`/`rhythm-*` inspections) | Workspace name field | At a glance card | y | 172 → 172 (≥ `s1` asserted from `name_rect_for` / `summary_card_rect_for`; 172 read back from the dark and light inspections) | rows/cards apart | ✓ |
| Panes | row list / footer copy | lens body bottom | y | 12 → 12 (`PANE_PAD_BOTTOM = s3`; the WIP had shrunk it to 8 with the top lead-in) | bottom inset `s3` | ✓ |
| General (ux-bench `rhythm-*`) | content group | At a glance card | x | Δ 0 → 0 (`aligned`, tol. 1, all six variants) | one left edge | ✓ |
| Panes | intro card | section header | y | 12 → 12 | related `s3` | ✓ |
| Panes | row card | row card | y | 8 → 8 | related rows `s2` | ✓ |
| Apps pane | header label line | empty-state glyph | y | **−76 (overlap)** → ≥ 8 | rows region | ✓ |
| Collaborators | Invite/Join band | separator | y | 4 → 4 | `s1` | ✓ |
| Collaborators | separator | activity row | y | 4 → 4 | `s1` | ✓ |
| Collaborators | activity row | panel bottom | y | 8 → 8 | `s2` | ✓ |
| Collaborators | separator ends | panel edges | x | 8 → 12 | `s3` (HIG §9.5) | ✓ |
| Onboarding | panel top | eyebrow row | y | 0 → 12 | `s3` | ✓ |
| Onboarding | progress track ends | panel edges | x | 0 → 12 | `s3` | ✓ |
| Onboarding | eyebrow row | progress track | y | 12 → 12 | `s3` | ✓ |
| Onboarding | eyebrow label | panel edge | x | 12 → 24 | body gutter `s6` | ✓ |
| Peer sync | caret | placeholder | x | 0 → 5.5 | caret + `s1` | ✓ |
| Launchpad | card | card | x/y | 12 → 12 | `s3` | ✓ |
| Projection | mode cell | mode cell | x | 8 → 8 | `s2` | ✓ |
| Power menu (verify) | row | row | y | 8 | `s2`; icon centred on row | ✓ |
| Room activity (verify, `matrix/*room-activity*/activity.inspection.json`) | entry card | entry card | y | 8 | `s2` (F4 widgets) | ✓ |

`geometry_audit` on the fresh `after/{ws,rhythm}-{dark,light}/{lens,summary}.inspection.json`
(20 controls, 175 pairs each) and the light matrix checkpoints: the only failures are the three
top-bar `touching_peers` (Previous/Next workspace, Show clock / system monitor / calendar,
gap 0) — S1's — and `semantic_bounds_overlap_unresolved` between the lens or the room-activity
sheet and the launchpad they float over (a modal over content; same set as before). No
touching or overlap inside any S2 surface.

## Ratchets (`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`)

```
test design_token_audit::declared_borders_in_chrome_do_not_grow ... FAILED   (stale only, no grew)
5 entr(ies) in tests/chrome_declared_border_baseline.txt are higher than the tree; the ratchet only moves down:
shell/src/shell/collaboration_launchpad.rs [declared_border]: now 0, baseline still 1 — lower it to 0
shell/src/shell/collaborators_overlay.rs [declared_border]: now 0, baseline still 1 — lower it to 0
shell/src/shell/room_activity.rs [declared_border]: now 0, baseline still 6 — lower it to 0
shell/src/shell/workspace_settings.rs [declared_border]: now 0, baseline still 5 — lower it to 0
shell/src/shell/workspace_settings_overlay.rs [declared_border]: now 0, baseline still 8 — lower it to 0
test f1_spacing_token_audit::spacing_literals_do_not_grow ... FAILED   (stale only, no grew)
1 baseline entr(ies) in tests/f1_spacing_token_baseline.txt are higher than the tree.
shell/src/shell/workspace_settings.rs [pixel_const]: now 16, baseline still 17 — lower it to 16
test design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow ... FAILED   (pre-existing, not S2)
apps/browser/src/browser_tabs.rs [literal_color] 1 / [white_or_black] 1; shell/src/shell/frame.rs [literal_color] 1 / [white_or_black] 2
test result: FAILED. 6 passed; 3 failed; 0 ignored; 0 measured; 345 filtered out
```

`declared_borders_in_chrome_do_not_grow` reports no `grew`; the `stale` lines are the five
owned files going to 0 (coordinator lowers the baseline). `app_and_shell_theme_escape_hatches`
fails only on the pre-existing `browser_tabs.rs` / `frame.rs` entries, which are present at the
base commit and untouched by S2.

## Tests

`cargo test -p shell --lib -- workspace_settings room_activity collaborators launchpad peer_sync projection portal power_menu osd onboarding lock_screen drag_feedback editor_app notes_app file_picker_overlay bluetooth`:
`test result: ok. 543 passed; 0 failed; 0 ignored; 0 measured; 1952 filtered out; finished in 6.76s`
(final code; the OffscreenRenderer fixtures for power menu, OSD and lock screen re-wrote their
dark/light/high-contrast PNGs under `target/ui-review/s2-r2/`).

New tests: `workspace_settings_overlay::{lens_cards_and_fields_keep_an_edge_on_every_palette,
lens_header_keeps_one_rhythm_and_centres_the_close_chip,
general_name_field_sits_on_the_card_pad_and_clears_the_summary_card}`,
`workspace_settings::rest_edge_tests::{pane_cards_and_footer_keep_an_edge_on_every_palette,
default_apps_empty_state_starts_below_the_header_block, pane_lead_in_completes_the_section_step}`,
`room_activity::sheet_and_card_edges_register_on_every_palette`,
`collaborators_overlay::{footer_separator_and_row_edge_register_on_every_palette,
footer_keeps_a_token_gap_around_the_activity_row}`,
`collaboration_launchpad::launchpad_cards_keep_an_edge_on_every_palette`,
`peer_sync_overlay::invite_rows_and_panel_edge_register_on_every_palette`,
`projection_overlay::sheet_cells_keep_an_edge_on_every_palette`.

`cargo check -p shell --lib --bin shell --bin ux-bench --bin shots`: `Finished dev profile`
(exit 0; the only warnings are the pre-existing `ui` `unused_mut` in `record.rs:1327` and
dead `animation_schedule::next_wake`, plus the macOS linker's `__eh_frame` note).
`cargo check -p shell --tests` (macOS): three test targets fail to resolve Linux-only crates —
`click_test_client` (test), `qos-loadclient` (test) and `tests/it` (164 errors, all
`wayland_client` / `smithay` / `wayland_protocols` / `smithay_client_toolkit` imports and the
`shell::compositor::state` / `shell::portal::ScreenCastStreamProvisionerHandle` paths behind
them); none of those files is touched by S2 and no error names an S2 symbol. With
`--keep-going` every other shell test target checks (`check-shell-tests-keepgoing.log`).
Recorded as a platform limit of the macOS review build, not a regression.
`cargo build -p shell --bin ux-bench --bin shots`: exit 0.

ux-bench (final binaries): `after/ws-dark`, `after/ws-light` PASS; `after/rhythm-dark`,
`after/rhythm-light` PASS; `run-suite.py --matrix` over `workspace-settings.json`,
`s2/s2-room-activity.json`, `s2/s2-workspace-settings.json`,
`s2/s2-workspace-settings-rhythm.json`: **24/24 PASS** (`after/matrix/summary.json`,
`passed: true`, no coverage gaps).

### `s2-workspace-settings-rhythm.json`

The scenario asserts what the accessibility tree can carry at every matrix variant:
checkpoint `lens` — the lens and the name field are visible and the field is one whole pointer
target (`target_size` ≥ 32×32); then a wheel over the body (a no-op at 1×, where General fits;
at 200 % text it brings the summary card under the fold) and checkpoint `summary` — `At a
glance` visible, ≥ 32×32 and `aligned` with the body's left edge to 1 px. The 1× pair the user
actually sees — name field ↔ "At a glance" card, and the field's `s4` inset inside the body —
cannot be a ux-bench `gap` check: the runner resolves a target only while it is in the visible
tree, and at 200 % text on an 800 px body the two are never co-visible (drafts that asserted
the gap failed the `large-text` variant: `after/matrix-attempt{1,2,3}.log`; a `aligned` check
with tolerance 16 was tried and dropped as tautological). The pair is asserted instead from the
production geometry in
`workspace_settings_overlay.rs::general_name_field_sits_on_the_card_pad_and_clears_the_summary_card`
(`name_rect_for`, the new shared `summary_card_rect_for`), and read back from the checkpoint
inspections (172 px, dark and light).

## New capture scenes (`shell/src/bin/shots/driver.rs`)

`ws-settings-agents`, `collaborators` (shared room + invited member, peer sync on so the
Invite/Join band shows), `launchpad` (window groups cleared → launchpad mounts through the
production `resync_workspace_content` path), `peer-sync` (Invite button clicked through the
event path), `projection` (Win+P sheet, 20 frames settled), `portal-permission` (camera prompt
through the real `PromptQueueHandle`). Also `shots` now builds on macOS: the driver imported
`compositor::capture::CaptureExecutor`, which is `cfg(target_os = "linux")`; it is gated the
same way (this blocked *every* `shots` capture at the base commit).

## Code review

Three `code-review` passes were requested (the first two reviewed the wrong tree — the
primary checkout — and were discarded; the third was scoped to `fd1f84391..HEAD` in this
worktree). Findings and what was done:

- **Regression, fixed.** `PANE_PAD_TOP` (12 → `s2` = 8) also served as the pane's *bottom*
  inset at five sites (row-overflow guards, footer copy, permissions footer band), so those
  shrank as a side effect. Split: `PANE_PAD_BOTTOM = s3` keeps the authored 12 at the bottom
  sites; only the top lead-in is `s2`.
- **Fixed.** `shots` scene `peer-sync` guarded the Invite click with `if invite.area() > 0.0`,
  so a roster without an Invite button would have captured the wrong overlay under a green
  result; it now asserts (a check that cannot observe its subject fails).
- **Fixed.** The rhythm scenario's `aligned` check with tolerance 16 was tautological; replaced
  by the Rust geometry test above, which also carries the name↔summary gap the bench cannot.
- **Fixed.** The default-apps empty state, now confined to the row region, would have painted
  past the body at the lens's 320 px height floor (negative region); `draw_empty_state`
  returns when its stack does not fit the region instead of overflowing.
- **Fixed.** `draw_ws_settings_collaborators` had a half-migrated body rect (literal 16 px
  insets beside `body_rect(rect).y0`); it takes `Self::body_rect(rect)` wholesale, and the
  summary card geometry is one shared `summary_card_rect_for` for hit-testing and painting.
- **Fixed.** The palette tests asserted Δ ≥ 0.008 while `edge_on` enforces
  `HAIRLINE_MIN_DELTA` (0.02); every S2 probe now asserts the toolkit bar (all pass, including
  `panel_border` over `panel_ground` and `edge_on(lift(0.35))` on high-contrast).
- **Fixed (crispness).** The inline tier/idle strokes stroked the raw shape while
  `ui::surface::card` strokes `edge_outline`; every S2 hairline now uses `edge_outline`, and
  `lift(0.35)` / `edge_on(floor)` are hoisted out of the draw loops. `paint_selector_pill` and
  `paint_control_chip` share one `paint_tiered`.
- **Done.** `cargo check -p shell --tests` run and recorded (see Tests): only the Linux-only
  Wayland client test targets fail on this Mac, untouched by S2.
- **Recorded, not done here** (toolkit-side, see Open): one `ui::surface::selectable` helper for
  the tier/idle recipe that projection, launchpad, peer-sync, room-activity and the tab strip
  restate; folding the per-file `edge_on` palette probes into
  `ui/src/surface.rs::edge_on_registers_on_every_preset_surface` with the two fills it lacks
  (`lift(0.35)`, `bg_primary`).

## Open — needs another unit

- **Coordinator (closed on `ux1x/integration`)** `ui::surface::card_on(scene, t, rect, radius,
  fill, ground)`: `collaboration_launchpad.rs:391` paints its idle cards on a translucent floor
  (`bg_primary.with_alpha_factor(0.78)` over the wallpaper) and derives the edge from the
  opaque `bg_primary`; once `card_on` is in the base the fill + edge should go through it with
  the desktop ground.
- **Coordinator (closed on `ux1x/integration`)** `ui::chrome::selection_tier_on(theme, state,
  ground)`: every S2 chosen state measures its lift against `surfaces.base` via
  `selection_tier`; the rows actually sit on `panel_ground` or a lift floor — replace at
  `workspace_settings_overlay.rs:1330` (tab pill on the nav track), `room_activity.rs:2751`
  (lens row on `panel_ground`), `room_activity.rs:6191/6213` (`paint_selector_pill` /
  `paint_control_chip` on the sheet), `peer_sync_overlay.rs:1138` (recipient/role pills on
  `panel_ground`), `projection_overlay.rs:705/852/1058` (mode cells, stream chips, device rows
  on `panel_ground`), `collaboration_launchpad.rs:390` (tiles on the desktop ground).
- **Coordinator (closed on `ux1x/integration`)** `separator` snapping to one device-pixel row
  and `card` snapping its rect: no S2 change needed; the collaborators separator and every
  `card` call pick it up on merge.
- **Coordinator / F-units** `ui/src/surface.rs`: a `selectable(scene, transform, rect, radius,
  theme, floor, active)` painter beside `card` — the HIG §9.3/§9.5 recipe (chosen =
  `selection_tier`, idle = floor + `edge_on(floor)`, edge on `edge_outline`) is restated in
  `projection_overlay.rs` ×3, `collaboration_launchpad.rs`, `peer_sync_overlay.rs::draw_choice`,
  `room_activity.rs::paint_tiered` and the tab strip; S2 could not add it (toolkit file).
- **Coordinator** fold the seven shell `*_edge_on_every_palette` probes into
  `ui/src/surface.rs::edge_on_registers_on_every_preset_surface` by adding `colors.lift(0.35)`
  and `theme.bg_primary` to its fill list; the shell copies (now at `HAIRLINE_MIN_DELTA`) can
  then go, keeping only the geometry tests.
- **S1** `shell/src/shell/overlay_host.rs:15234 draw_portal_dialog_sheet` and the
  `PermissionPrompt` host: `after/dark/portal-permission.png` — banner edge is faint on dark.
- **S1** top bar: `touching_peers` Previous/Next workspace and Show clock/system monitor/
  calendar (gap 0) in every lens inspection.
- **F2** `ui/src/widget/permission_prompt.rs`, `ui/src/widget/file_picker.rs`:
  `file_chooser_dialog/01-open.png` dark — dialog and filter/button rest edges near-invisible.
- **F3** `ui/src/widget/badge.rs`: tinted badge pills ("4 active grants", "Once/Always/
  Session") have low-contrast text in light (`after/light/ws-settings-permissions.png`).
- **F4** `ui/src/widget/history*`: room-activity "At a glance" card is an accent-tinted wash
  (§9.4) — `after/{dark,light}/room-activity.png`.

## Limits

- Replay transport chips and peer-sync pending rows keep their authored 6 px gaps (≥ `s1`,
  not a scale step); projection stream chips likewise. Not changed this round.
- Lock screen: the credential card reserves a bottom band for the notification row, so
  states 01–04 read bottom-heavy (rule 4); a content-driven height belongs to
  `LockLayout::for_output` and was left alone (security-adjacent layout, out of scope).
- Launchpad subtitle is hidden in the dark `shots` scene at 1280×800 but shown in the bench
  desktop and in light — the `HEADING_BAND` measurement path predates this round.
- `shots --mode offscreen` renders on transparent; the desktop behind panels is the fixture
  wallpaper. ux-bench frames are composited.
- Room-activity entry names carry the local login name, so no ux-bench `gap` check could be
  written for them; the card gaps are asserted from the inspection (8 px) instead.
- The session that produced the WIP commit was killed by a reboot; every AFTER capture was
  regenerated from the final binaries on resume (the checkpoint set is kept under
  `target/ux1x/s2/checkpoint-after/`, 46 MB; `target/ux1x/s2` totals 143 MB).
