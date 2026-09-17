# S1 (round two) — Top bar, wheel disc, strips/gutters, palette & overview host

Branch `ui-review/r2-s1-shell-topbar-wheel-strips`, cut from the merged round-one tip
`381eff6a8` ("Merge ui-review/integration") plus the coordinator's one-line test-target fix
`f11e61da6` (cherry-picked as `280e9b58b`; `overlay_host.rs` test read `CHIP_PAD_PX`, which
the merge left unstaged — without it `cargo test -p shell --lib` does not compile at the base).

Files owned this round: `shell/src/shell/{topbar_chrome,sidebar_wheel_disc,sidebar_wheel_chrome,
overlay_host,global_search,chrome_state}.rs`, `shell/src/shell/search/`, `shell/src/shell/mod.rs`
(context-menu item only), `shell/src/bin/ux_bench_support/mod.rs`, new `scripts/ux-bench/s1/*.json`.
`frame.rs`, `ui/src/history_mode/*` and every `ui` widget were not touched.

## Method

1. Round one's report re-read; its S1 changes re-verified on the merged tip (the coordinator's
   conflict resolutions in `overlay_host.rs` and `topbar_chrome.rs` are intact — the palette
   band still measures `search_filter_control_h` / `search_scope_segment_w`, the workspace pill
   still budgets through the token chrome with `workspace_label`). BEFORE evidence on the
   untouched binary: the 48-run matrix, the three round-one S1 scenarios, the deterministic
   overlap lint.
2. The one feature item — the shell's right-click menu — wired end to end on the production
   host and pinned by inline tests that drive `ShellOverlayHost::handle_event` itself.
3. Round one's open items closed where they lived in S1 files; each geometry or role change
   guarded by an inline test and, where addressable, a bench check.
4. AFTER evidence on the rebuilt binary: the same matrix, the S1 scenarios (now four), the
   overlap lint, and an `--interactive` JSONL capture of the menu through the new
   `context_click` op.

## Findings

| id | surface | defect | theme / viewport | where (before) | evidence | status |
|---|---|---|---|---|---|---|
| R2-01 | Shell right-click menu | No runtime context menu: `WindowGroup::take_tab_context_menu` had no consumer; `Shell::ctx_menu` (fixture only) held no-op items | all | `ui/src/widget/window_group.rs:729`; `shell/src/shell/mod.rs` `open_default_tile_context_menu` | BEFORE: nothing opens (`tab-menu-before`); AFTER: `s1-r2-after-tabmenu/0001.png`…`0004.png` | **fixed** — `ShellOverlayHost::tab_menu` (`TabContextMenu`): the tab bar's request is drained synchronously inside `dispatch_pointer_and_content_event` (and as an `update` fallback), the shared `ui::widget::ContextMenu` opens anchored to the pointer (`open_anchored`, flips/clamps inside the host), `route_tab_context_menu_event` owns pointer + keyboard while up (hover arms, Up/Down, Enter, Escape, click-in selects, click-out dismisses and is absorbed, Super chords close-and-pass, a second right-click outside closes-and-passes so another tab can open its own), painted in `draw_top_overlays` with the live theme and an `e4` shadow along the widget's own radius, published as `AccessibleOverlay::TabContextMenu` (Menu → MenuItem children with bounds, `focused`, `tabmenu:*` targets), `perform_semantic_action("tabmenu:<slug>", Activate)` fires the same operation, `synthetic_accessibility_rect`/`focus_synthetic_accessibility` route the rows. Items come from one list, `shell::tab_context_menu_items` (`TabMenuAction::{CloseTab, SplitRight, MoveToNewGroup}`), shared with the fixture `Shell` so `shell_chrome_test::right_click_on_tile_opens_default_context_menu` keeps its labels. Every row runs a `window_actions` function that the action registry runs too (R2-20): Close → `window_actions::close_tab` (document, tree, prune, flush, surface close, bump — `commit_tab_close` now delegates to it); Split Right / Move to New Group → `window_actions::split_tab`, **tree-first** (`WorkspaceTree::split_tab_into_new_group`: cache and replicated document together through `doc_move_tab_across_strips` with the strip as both ends, then `bump()`), after which the host moves the live tab host into a group carrying the tree's new id so the resync keeps the window state instead of building a cold host. The action runs first; a refusal keeps the menu up with the row still armed and is surfaced as a warning notification. The press a menu absorbs owes a release, and that release is swallowed even after the press closed the menu. A single-tab group is offered Close only, and every route refuses `OnlyTab` |
| R2-02 | Strip gutters | `WHEEL_TO_STRIP_GAP_PX` / `STRIP_TRAILING_GUTTER_PX` 28 px, off the spacing scale (round one S1-21, kept) | all | `overlay_host.rs:1434,1447` | `a_full_width_window_stops_short_of_both_screen_edges` (updated) | **fixed** — `s8` (32): the disc's rim shadow reaches `RIM_SHADOW_REACH` = 26 px past its footprint, so `s6` (24) would lay it over the first window; 32 clears it and is the pointer floor every bar control sits on. The pinning test now asserts the scale step, that it exceeds the rim reach, and that both gutters agree; the trailing gutter is defined as the leading one. Every window moves 4 px right / narrows 8 px on every display — the deliberate change |
| R2-03 | Fixture default `SIDEBAR_TOP_OFFSET_PX` | 40 px beside a 48 px bar ("matches `TOPBAR_HEIGHT_PX`" — stale): bare-host fixtures placed the wheel, room-activity sheet, permission prompt and notification stack 8 px under the bar; production passes 48 through `with_sidebar_top` | tests only | `overlay_host.rs:1414` | inline tests unchanged | **fixed** — the default *is* `TOPBAR_HEIGHT_PX`, so fixtures observe production geometry |
| R2-04 | Room-activity sheet, quick settings, shortcut help, portal sheet | `soft_shadow(neutral[0], 40/34/40/36, …)` sprite shadows on a ramp slot with bespoke strengths (round one S1-26, S2's four) | all; wrong on light/HC | `overlay_host.rs` four sites | source; AFTER captures | **fixed** — `ui::surface::shadow(rect, radius, e4)` following each surface's own outline (`radii.lg` for the room-activity sheet as `room_activity.rs` paints it, `ChromeRole::Panel.radius` for QS, help and the portal sheet), alpha following the open transition where one exists; `quick_settings_settled_bounds` now derives damage from the `e4` token alone |
| R2-05 | Shortcut help panel | 18 px radius (off the 4/8/12/24 scale) on `neutral[0]`, no edge | all | `draw_shortcut_help` | AFTER `shortcut-help` capture | **fixed** — Panel role: `radii.xl`, `ChromeRole::Panel.surface`, `panel_border` hairline |
| R2-06 | Portal review sheet | 16 px radius (off-scale); choice chips painted with the filter-chip helper at a literal `14.0` with an `accent[5]@0.26` "selected" fill; Cancel/Confirm buttons painted as chips (`surfaces.base` / `accent[5]`) | all | `draw_portal_dialog_sheet` | source | **fixed (shadow, radius, chips, buttons)** — `Panel.radius`, chips on `selection_tier` at `scale.base` (kept 14 px; §9.6 says `sm` — the size change is the sheet owner's call), Cancel/Confirm through `Button::paint` (`ButtonFace`, Confirm `.primary()`, Control radius). Left for S2: title at `20.0`, sub-lines at `14.0`/`11.0`, `+24/+50/+74/+10` offsets, `surfaces.raised` body |
| R2-07 | Overview room row | Selected row: 16 % accent fill + accent stroke (§9.4: accent means live); idle row `neutral[1]` + `border_standard@0.55` (§9.2; round one S1-27); hover `lift(0.35)` | all | `draw_overview_room_row` | AFTER `overview` captures | **fixed** — `selection_tier` (idle transparent, hover `lift(0.5)`, selected `lift(1.0)` + hairline + inner glow); idle rows keep a derived `hairline_on(ground, 26)` edge so the destination still reads as a target |
| R2-08 | Help button, QS password field | `neutral[1]` bodies | all | `draw_shortcut_help_button`, QS Wi-Fi password field | source | **fixed** — help pill: Panel surface + `panel_border` + `ink_on`; pad literals `24/12/16` → `s6/s3/s4` (140 × 36 floors kept as degenerate minimums). Field: Control radius (`radii.lg`, was 10), body `lift(1.0)` (opaque, replaces `neutral[1]` + `lift(0.35)`); its 1.5 px focus stroke and `+12` text inset left for S2 |
| R2-09 | Palette filter band | Chosen chip/segment `accent[5]`, active label `text_primary` on it (unmeasured ink); idle chip `lift(0.35)`; track `lift(0.35)` + `hairline_on(raised, 26)`. Also: labels double-scaled — `scale.sm` (already text-scaled) through the legacy renderer (which scales again) painted 48 px glyphs at 200 % and the pills grew to 64 px to hold them (round one's S1-05 grew the pill rather than fixing the size) | all; 200 % | `draw_filter_chip`, `draw_search_banner`, `draw_search_banner_scope_switch`, `filter_label_size` | BEFORE `s1-r2-before-s1/011-s1-palette-filters-large-text/palette.png` (blue "All"/"Everywhere", 48 px labels, two chip rows) → AFTER `s1-r2-after-final-s1/011-s1-palette-filters-large-text/palette.png` (lift-selected, 24 px labels matching the result rows, all eight chips and the switch on one row) | **fixed** — Selectors on `selection_tier`; ink `ink_on(under, tone, 4.5)`; the track is a `hairline_on(panel_ground, HAIRLINE_ALPHA)` outline with no body; `filter_label_size` is renderer-relative (`scale.sm / text_renderer.text_scale()`, the cure `draw_shortcut_help_button` already used) and every measure/draw in the band goes through it; `filter_band_paints_selection_as_a_lift_never_as_accent` pins the tiers on dark/light/HC |
| R2-10 | Palette filter band | Selector 28 px was also its pointer target (round one S1-28) | all | chip/scope hit-tests, a11y bounds | `s1-palette-filters.json` (`target_size min [32,32]`) | **fixed** — `selector_hit_rect`: painted pill grown about its centre to `metrics.min_pointer_target`; hit-tests, `search_filters_accessibility` bounds, `search_filter_rect_for_path` and the bench read the target, paint reads the pill; two rows' targets never overlap (`row_h − s1 ≥ 32`). `SELECTOR_HEIGHT_PX` now derives from `Metrics::BASE.selector_height`; `filter_control_height` reads the live `metrics.selector_height` |
| R2-11 | Palette scope switch | At 200 % on a narrow card the two-segment track exceeded its 55 % share and elided to "Works…"/"Every…" (round one S1-29) | 200 % text, compact | `search_scope_track_rect` | `a_scope_switch_too_wide_to_share_the_first_row_takes_its_own` (800 × 700, 2×) | **fixed** — `scope_switch_has_own_row`: past the share the switch takes the first row alone at its full measured width and the chips start on the next row; the band reserves one more row; draw and hit-test read the same predicate. At 1280 × 800 the switch already fit (F4's 72 px rows), so the matrix's large-text variant shows no change there |
| R2-12 | Top bar | `TOPBAR_HEIGHT_PX 48` / `BAR_CONTROL_PX 32` typed while `tokens.metrics` exists (round one "adopt metrics") | all | `topbar_chrome.rs:34,53` | `bar_density_sits_on_the_spacing_scale` (extended) | **fixed (derived, pixels unchanged)** — `BAR_CONTROL_PX = Metrics::BASE.min_pointer_target`; `TOPBAR_HEIGHT_PX = min_pointer_target + 2·s2`. Decision recorded: the shell's bar hosts pointer-floor controls, so it is 48, not `metrics.bar_height` (40, whose controls are `bar_control_height` 28); the centre-lane tests pin the height |
| R2-13 | Home launcher, compact | Heading under the bar (round one S1-23) | compact 800×700 | launchpad | `s1-r2-before/007-desktop-compact/desktop.png` | **resolved before this round** — verified on the merged tip: heading at y≈185, bar ends at 48 |
| R2-14 | Launchpad × wheel | Overlap lint `hit_geometry`: "Make something together" (the launchpad *widget*, focusable, rect spanning the content row from x = 0) vs the wheel's `Switch to Home` (44–208 × 392–456) and `Add a workspace` | baseline, compact-large-text | `collaboration_launchpad.rs:566,582` (`is_focusable`, `get_rect`) | `s1-r2-overlap/summary.json` | **open — needs the launchpad owner (S2/A)**: the widget paints inside `workspace_rail_clearance_for_viewport` but its rect and its `Group` node (no bounds) start at the row's left edge; publish bounds = the painted card, or start the widget's rect at the clearance. Pre-existing at the base; unchanged by this round (the gutter change moves the card 4 px, not the widget rect) |
| R2-15 | Shell a11y names | English literals (`"Close Tab"`, `"Workspace overview"`, …) | all | `shell/src/shell` | source | **open — recorded**: `ui::i18n::I18nManager` is initialised (`compositor/macos.rs:147`) but `shell/` has no `.ftl` catalogue and no `lookup` call; the path is `I18nManager::install_system(locale, catalogs)` + `lookup_plain(key)`. The menu's labels follow the file's precedent and carry stable `tabmenu:*` keys so a catalogue can slot in |
| R2-16 | Bench | No secondary-button input: the JSON action set (`click_named`/`click_at`/`tap_*`/`key`/`type`/`perform`) cannot open a context menu, so no `s1/*.json` scenario can exercise the menu | — | `ui/src/headless.rs:3359` | — | **fixed in the interactive half** — `ux_bench_support::Command::ContextClick { x, y }` (move, right press, right release through `Driver::send`, counted as a click, bounds-validated like `click`). **Open for F4/F6**: a `{"context_click_at": [x, y]}` / `context_click_named` action in `Driver::apply_action` so a JSON scenario can pin the menu too |
| R2-17 | Wheel chrome | `soft_shadow(accent[5], 14, 0, 0.35)` in `sidebar_wheel_chrome.rs:417` | all | — | — | **open — kept**: an accent glow ring on the active card, not a drop shadow, in code documented as unmounted (`foreign.rs`, `mod.rs`, `shots/scenario.rs` still read its width constant) |
| R2-18 | Portal sheet chips | `draw_filter_chip` served two surfaces; the sheet's chips used their own colours | — | — | — | **fixed** with R2-06 (one painter, one tier model) |
| R2-19 | In-strip drag-out split | Found while pinning R2-01: `harvest_widget_only_groups_into_tree` mirrored a widget-side split by *inserting* the new group only, leaving the moved tab listed in its tree source group; at the next chrome revision `resync_strips_from_tree`'s tab reconcile saw "tree has it, row does not" and re-created the tab in the source as a second, hostless card (and revived a source group the row had pruned). Every in-strip drag-out split duplicated its tab at the next bump | all | `overlay_host.rs` `harvest_widget_only_groups_into_tree`; `resync_strips_from_tree` reconcile block | first version of `split_right_by_keyboard_moves_the_tab_into_a_new_group_beside_it` failed with `[8651, 8652, 8653, 8654]` in the source | **fixed** — the mirror now *moves*: the new group's tab ids are removed from every other tree group in the strip (active tab taken from the live bar, else the bar's own rule — the neighbour that slid into the slot, else the last; an emptied source pruned; `active_group` re-pointed at the new group when the source was active) before the insert, and the tree is flushed like a width change. The menu no longer relies on this path (R2-01 is tree-first); it remains the drag-out gesture's mirror and is still cache-only — the drag-out's own tree-first form is `GroupRow` handing its detach to `WorkspaceTree::split_tab_into_new_group`, which is the frame owner's (F6) |
| R2-20 | Action registry | The tab menu's capabilities existed only as right-click rows — "a capability reachable only by clicking is not finished" | — | `search/providers/actions.rs` | `search::providers::actions` tests (contract + searchability) | **fixed** — `tab.close`, `tab.split-right`, `tab.move-to-new-group` registered beside `tab.duplicate`, on the focused tab, calling the same `window_actions::{close_active_tab, split_active_tab}` the menu rows call (`close_tab`/`split_tab` by id); contracts Write, splits reversible, close not; refusals surfaced with `push_warning_notification` and `ActionError::Refused`, like `tab.duplicate` |
| R2-21 | Bench pixel probes | `run-suite.py` `VARIANT_PIXEL_PROBES` "App shortcut hint" (compact `[275,591]`, large-text `[320,598]`) are authored pixels on the launchpad's hint glyph; the card sits at `workspace_rail_clearance_for_viewport`, so R2-02 moved it and both probes sampled background (AFTER matrix 28/20 before the pin followed) | compact, large-text | `scripts/ux-bench/run-suite.py:35,38` | measured glyph runs on row 598: 319..321 → 321..323 (large text, +2: the card is centred in its budget there), compact +4 | **fixed (deliberate change to a shared bench file — outside this unit's list, flagged for the coordinator)** — compact `[279,591]/[276,590]`, large-text `[322,598]/[316,598]`; `navigation-layout.json`'s baseline probe at `[553,599]` still lands on the glyph run and is untouched |
| R2-22 | `files` bench fixture, compact | The `files` fixture at 800 × 700 paints nothing at all — no bar, no wheel, no window (`s1-tab-context-menu` compact fails `visible(semantic:topbar:overview)` with zero bounds) | compact | `compositor/macos.rs` `prepare` + fixture | BEFORE `s1-r2-before/001-browse-file-compact/initial.png` (untouched binary, same fixture) is the same blank frame | **open — pre-existing, needs the fixture/compositor owner**: `browse-file` and `files-scroll` (matrix known-red) share it. Reported verbatim, not masked |
| R2-23 | Palette filter band | The pointer floor is unscaled while the palette's row is `36·text_scale`: below 100 % text targets grown to 32 would overlap neighbouring rows' and the first row's would protrude above the band onto the query field | text scale < 1 | `selector_hit_rect` | `selector_targets_never_overlap_or_leave_the_band_at_small_text` (0.75) | **fixed** — floor clamped to `row_h − s1` (never below the pill) at draw, every target clipped to the band; targets never share more than a pixel and stay inside the band at every scale |
| R2-24 | Palette filter band | The frame on which the scope switch first claims the first row: the palette has reserved one row, so the chips' band has no height, yet a chip row was laid out and published over the first result | 200 %, narrow card, one frame | `draw_search_banner` | source; `clicking_a_kind_chip_filters_without_dismissing_the_palette` (a first draft that cleared the chips on that frame broke this round-one test, whose single paint runs against a not-yet-settled band) | **fixed** — two predicates: `scope_switch_wants_own_row` (measured track exceeds its share; drives the row request) and `scope_switch_has_own_row` (wants, and the band already holds two rows; drives layout and hit-tests). On the flip frame the switch shares the first row as before and the chips stay in it; the granted row is taken on the next layout |
| R2-25 | Shared helpers (recorded) | Three copies of the same shape in this unit alone | — | — | — | **open — needs X1/F1/F4**: (X1/F1) a scalar `TextRenderer::renderer_input_size(size)` so the shell's `size / text_renderer.text_scale()` copies (`filter_label_size`, `draw_shortcut_help_button`, the type-size twins round one listed) collapse into one; (F1) a shared `pointer_target(rect, theme)` for the strip ribbon, strip rail and selector hit rects, which each grow a painted rect to `min_pointer_target` by hand; (F4) `MenuItem` ids on the shared `ContextMenu` so a host does not carry a parallel `Vec<Option<TabMenuAction>>` beside `menu.items()` |

### Deliberate geometry changes (pinned tests updated, with rationale in the test)

- `a_full_width_window_stops_short_of_both_screen_edges`: gutter 28 → 32 (`s8`); asserts the
  scale step and the rim-shadow clearance.
- `filter_chips_and_scope_segments_are_bounded_semantic_targets`: published bounds are pointer
  targets (pill grown to the floor), pills share edges with the track, targets share edges with
  the scope targets; rest height read from `metrics.selector_height`.
- `scripts/ux-bench/s1/s1-palette-filters.json`: `target_size` floors 28 → 32 for chips and
  scopes; the cross-family `aligned(all, scope:everywhere, y)` check replaced by `no_overlap`
  (the switch may now sit on its own row at large text on narrow cards).
- `bar_density_sits_on_the_spacing_scale`: additionally asserts derivation from
  `metrics.min_pointer_target` and that the bar exceeds `metrics.bar_height`.
- `scripts/ux-bench/run-suite.py` `VARIANT_PIXEL_PROBES`: the two shortcut-hint probes
  followed the launchpad card (+4 compact, +2 large text, measured) — R2-21; a shared file,
  flagged.

### Off-grid metrics kept, and why

- `TOPBAR_HEIGHT_PX` 48 vs §9.6 Bar 40 — see R2-12 (derived, decided, pinned).
- `WHEEL_TO_STRIP_GAP_PX` is `Spacing::BASE.s8` with a `const` assertion that it exceeds
  `RIM_SHADOW_REACH`; `CHIP_MIN_W_PX` is `Metrics::BASE.min_pointer_target`; the help pill's
  height floor is `min_pointer_target + s1` and its 140 px width floor is gone (the measured
  label sizes it).
- `OVERVIEW_PANEL_WIDTH_PX 720`, `SIDEBAR_WIDTH_*`, `center_band_width` budgets,
  `LABEL_MIN_W 62`, `RIM_SHADOW_REACH 26`, `SEGMENT_INSET_PX 2` — unchanged from round one's
  reasoning.
- `ContextMenu`'s own metrics (`ROW_HEIGHT 28`, `PADDING 6`, `SEPARATOR_HEIGHT 9`, label inset
  `10`, `radii.md`) are F4's open item F4-16; the host shadows the widget's painted radius.
- Portal sheet: option chips kept at `scale.base` (14 px, their previous literal) rather than
  §9.6's `sm` — a size decision for the sheet owner (S2); the sheet's other literals are listed
  under R2-06.

## Bounds and addresses added

- `semantic:tabmenu` (Menu, name `<tab title> tab menu`, `expanded`), children
  `semantic:tabmenu:close` / `semantic:tabmenu:split-right` / `semantic:tabmenu:new-group`
  (MenuItem, `focusable`, `focused` = armed row, bounds = the shared widget's row rect,
  `pos_in_set`/`set_size`). Activation: `perform_semantic_action` on the host, key path
  `[0, i]` while open (the menu is the first overlay in `accessible_overlay_order`).
- Palette band: unchanged addresses; bounds are now the pointer targets.

## Tests

New inline tests (all green — see Runs):

- `shell/src/shell/mod.rs` `tab_menu_item_tests`: `a_single_tab_group_is_offered_only_close`,
  `a_multi_tab_group_gets_close_then_the_split_pair_after_a_separator`,
  `semantic_keys_round_trip_and_are_unique`.
- `overlay_host.rs` `tab_context_menu_tests` (production `handle_event` over
  `build_nirios_overlay_host_with_chrome(&WorkspaceTree::bootstrap_demo())`):
  `right_click_on_a_tab_opens_the_menu_arrows_move_the_highlight_and_escape_closes`,
  `a_click_outside_the_menu_dismisses_it_and_goes_no_further`,
  `clicking_close_tab_removes_that_tab_from_the_tree`,
  `split_right_by_keyboard_moves_the_tab_into_a_new_group_beside_it`,
  `the_semantic_route_performs_the_same_action_as_the_pointer`,
  `the_registry_split_action_lands_in_the_tree_and_the_resync_builds_the_group`
  (`window_actions::split_active_tab` with no menu and no live host; `OnlyTab` refusal),
  `a_single_tab_group_is_offered_close_only`, `the_open_menu_paints_and_the_closed_one_does_not`.
- `overlay_host.rs` `overlay_event_absorb_tests`:
  `a_scope_switch_too_wide_to_share_the_first_row_takes_its_own`,
  `filter_band_paints_selection_as_a_lift_never_as_accent`,
  `selector_targets_never_overlap_or_leave_the_band_at_small_text` (0.75 text scale).
- `ux_bench_support/tests.rs`: `context_click` accepted inside the viewport, rejected outside.
- New production code outside the round-one file list, at the coordinator's direction:
  `WorkspaceTree::split_tab_into_new_group` (`state.rs`), `window_actions::{TabActionError,
  SplitPlacement, active_tab_id, close_tab, close_active_tab, split_tab, split_active_tab}`,
  and the three `tab.*` registrations in `search/providers/actions.rs`.

Updated: the four pinned tests listed under "Deliberate geometry changes".

Notes on the fixtures: a hand-built `WorkspaceTree` carries no Automerge mirror, and a close is
a document deletion first (`tab_binding::remove_tab` → `doc_remove_tab`), so the closing tests
use `bootstrap_demo()`; the host settles its chrome revision once (`take_structure_dirty`) the
way the first frame's `update` does, or `harvest_widget_only_groups_into_tree` skips the split
and the resync evicts it — both are how production behaves, not test scaffolding.

## Runs

Every cargo command was run as
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig cargo … --locked --no-default-features`.

Tests:

- `cargo test -p shell --lib -- topbar_chrome sidebar_wheel_disc overlay_host::tab_context_menu_tests overlay_host::overview_panel_tests overlay_host::overlay_event_absorb_tests overlay_host::palette_dispatch_tests overlay_host::roster_rows overlay_host::maximize_harvest_tests overlay_host::overview_transition_tests overlay_host::two_workspace_host_tests overlay_host::resync_strips_tests overlay_host::tests overlay_host::empty_workspace_tests overlay_host::wifi_password_tests overlay_host::peer_cursor_tests tab_menu_item_tests collaboration_launchpad window_actions search::providers::actions state::tests`
  → **353 passed, 1 failed, 1 ignored** (2075 filtered out). The one failure is the brief's
  known-red `overlay_host::tests::collaborator_actions_are_absent_without_the_invite_capability`
  ("a guest is not offered the room's membership controls", left `(Rect { x0: 1272.0, y0: 164.0,
  x1: 1420.0, y1: 196.0 }, Rect { x0: 1428.0, … })` vs right two zero rects), reproduced at the
  base and untouched. The ignored test is the pre-existing
  `render_notification_history_accessibility_matrix_at_1x` (needs an offscreen adapter).
- First pass (before the fixture fixes) also selected `search`: `393 passed, 8 failed, 1 ignored`
  — four of the eight were `search::action_api::tests::{a_nullary_action_still_invokes_with_no_arguments, enumerate_select_invoke_mutates_state_with_no_rendering}` ("current workspace authority no longer permits settings.open") and `search::visibility::tests::{acting_on_the_room_requires_the_matching_verb, the_gate_hides_acts_from_a_member_without_capabilities}` ("peer.invite must be gated" / "Write must not confer Invite"). **Verified pre-existing at the base**: in a throwaway worktree at `381eff6a8` (which does not compile its lib tests without `f11e61da6` — `E0425 CHIP_PAD_PX`; the fix was applied there too) those four plus the brief's known-red `collaborator_actions_are_absent_without_the_invite_capability` fail identically (`0 passed; 5 failed`). They are the user's in-progress capability-gating work and were not touched; the worktree was removed afterwards.
- `cargo test -p ui --lib -- ux_bench` → **18 passed, 0 failed**.
- `python3 scripts/ux-bench/test_suite.py` → 4 tests OK; `python3 scripts/ux-bench/usability_test.py` → 26 tests OK.
- Compile gate: `cargo check -p shell --lib --bin shell --bin ux-bench --bin qos-control` →
  **clean** (`Finished dev`, no warnings). `cargo check -p shell --tests` cannot run on macOS:
  `shell/tests/it.rs` is the Linux-only integration target (as in round one), and the
  `code-review` skill's own attempt failed inside `ring`'s const-eval for the same reason.
- `code-review high` (the shared skill) on the diff: its findings were fixed before commit —
  the tree-first split (R2-01), the three registry actions (R2-20), `WHEEL_TO_STRIP_GAP_PX`
  from `Spacing::BASE` with a const assertion, the help-button floors on tokens, dead
  `widget_handled`/`widget_focus` locals documented as the trait's shape; the Angle A items —
  pointer-floor clamp/clip (R2-23), the paired release swallowed after the press closed the
  menu, deterministic harvest of the request nearest the pointer plus the harvest on the
  pointer-capture path, the harvest's active-tab rule from the live bar, action-first with the
  menu kept up on refusal, and the flip-frame chips (R2-24). Recorded, not fixed (needs
  X1/F1/F4): R2-25.

Bench:

| run | directory | runs | pass | fail | notes |
|---|---|---|---|---|---|
| BEFORE matrix | `target/ux-bench/s1-r2-before` | 48 | 30 | 18 | the brief expected 36/12; the merged tip fails `browse-file` ×6 as well (`missing or ambiguous target: semantic:Document-000.txt`, both checks of its `document-open` checkpoint) beside the 12 known `semantic:Files` runs (`files-scroll` ×6, `open-files` ×6). All 18 are Files/Editor semantics, none in S1 files |
| BEFORE S1 scenarios | `target/ux-bench/s1-r2-before-s1` | 18 | 18 | 0 | round one's three scenarios survived the merge |
| BEFORE overlap lint | `target/ux-bench/s1-r2-overlap` | 2 variants | — | 2 `product_property_failure` | `exact_document_outcome` (Files/Editor, pre-existing) and `hit_geometry` (R2-14: launchpad widget × wheel sectors) in both variants |
| AFTER matrix (first binary) | `target/ux-bench/s1-r2-after` | 48 | 28 | 20 | the 18 pre-existing plus `navigation-layout` at compact and large-text: the `VARIANT_PIXEL_PROBES` contrast probes on the launchpad's hint glyph sampled background after the card moved with the gutter (R2-21). Not masked — the pins followed the subject by the measured shift |
| AFTER matrix (final binary) | `target/ux-bench/s1-r2-after-final` | 48 | 30 | 18 | **identical failure set to BEFORE**: `browse-file` ×6 (`semantic:Document-000.txt`), `files-scroll` ×6 and `open-files` ×6 (`semantic:Files`) — all Files/Editor semantics, none in S1 files; `navigation-layout` passes in all six variants against the moved probes |
| AFTER S1 scenarios | `target/ux-bench/s1-r2-after-final-s1` | 24 | 23 | 1 | four scenarios × six variants. The 18 round-one runs and `s1-palette-filters` with its 32 px floors all pass; the one failure is `s1-tab-context-menu` at compact, where the `files` fixture paints a blank frame on the untouched binary too (R2-22: `visible(semantic:topbar:overview)` → zero bounds). Reported, not masked |
| AFTER overlap lint | `target/ux-bench/s1-r2-after-final-overlap` | 2 | — | 2 | identical to BEFORE: `exact_document_outcome` (Files/Editor) and `hit_geometry` with only the launchpad × wheel pair (R2-14); no `target_size_24`, `finite_bounds` or `accessible_label` failures |
| AFTER interactive | `target/ux-bench/s1-r2-after-final-tabmenu` | 6 frames | 6 | 0 | `fixture: files` at 1280 × 800: `{"op":"context_click","x":292,"y":124}` on the Files pill → frame 1 publishes `Files tab menu` (Menu, bounds 292..361 × 124..164) with one `MenuItem` `Close Tab` (bounds 292..361 × 130..158, not focused — a single-tab group); `ArrowDown` → frame 2 `Close Tab` focused, the accent highlight visible in `0002.png`; `ArrowDown` → frame 3 unchanged (one row); `Escape` → frame 4 no Menu; `finish` ok, counts `clicks 1, keys 3`. Driven by `target/ux-bench/s1-r2-tools/tab_menu_interactive.py` (not committed; those are the five JSONL lines). The tab bar publishes no `Tab` nodes, so the driver locates the pill from the `GroupRow` widget rect and the first bounded content node — recorded for F6 |

## Limits

- macOS host: no `shots`, `--backend headless` or `qos-control`; `ux-bench` is the driven
  evidence path; `shell/tests/it.rs` (where `shell_chrome_test::right_click_on_tile_opens_default_context_menu` lives) is Linux-only and could not be run here — its labels are preserved by construction (the fixture builds from the same list).
- The JSON scenario format has no secondary button (R2-16), so the menu's bench evidence is the
  `--interactive` capture; `Driver::apply_action` is `ui`'s and was not changed.
- `git cherry-pick` needed an identity: this worktree has none configured, so the pick was
  completed with `-c user.name/user.email` matching the tree's recent commits on the command
  line only; nothing in git config was written.
- The two splits' durability rests on `harvest_widget_only_groups_into_tree` exactly as the
  drag-out split's does; a tree-side `split_tab_into_new_group` with a document mirror (the
  `move_tab_across_strips` shape) would make both paths first-class — recorded for the
  state/frame owner, not attempted here.
