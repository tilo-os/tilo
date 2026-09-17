# UI alignment review, round two — A6 Content I (calendar, notes, contacts, weather, maps)

Branch `ui-review/r2-a6-content-1`, cut from `381eff6a8` (the merged round-one tip,
"Merge ui-review/integration"). Worktree
`/Users/xaver/dev/qos/.claude/worktrees/agent-aa3c5ce2a4a244554`. Captures under
`target/ui-review/<app>/{before,after}/` in that worktree: BEFORE = the merged-tip binaries
(this worktree's first build; the three 2× views that round one never captured were taken
from round one's worktree binaries, which for contacts, weather and maps are the merged tip
— `target/ui-review/shot-r1.sh`), AFTER = this branch. Every PNG cited below was opened.

## Method

1. Read the round-one index and A6 report, `ui/HIG.md`, the merged toolkit APIs this round
   leans on (`Tokens::metrics`, `Chip::draw_at`/`ChipLead`, `Button::paint`,
   `chrome::fill_scrim`, `TextRenderer::renderer_input_style`, `FloatingPlacement::
   set_preferred_size`) and the `f1_spacing_token_audit` ratchet.
2. Re-checked round one's late fixes against the merge, then worked the 14 open items and
   fresh-eyes passes at 800×700, 480×900, 1280×360 and 200 %.
3. Layout rule used everywhere this round: a row is the theme token for that control (or its
   authored 1× value) **or** the measured line plus the same theme pad the painter leaves,
   rounded up to a whole pixel — `at_least_whole(minimum, line + pad)` (one copy per crate,
   beside the crate's single `line_h`). Reserve and paint read one set of tokens, 1× is the
   design exactly, and every height lands on a whole pixel at every scale. Where the design
   is a hand-authored card (weather's body, maps' navigation card) the card grows by exactly
   its lines' extra height (`authored_h`) and painters read the offsets from the layout.
4. `code-review` (high) ran on the diff; the coordinator relayed its findings plus five
   reviewer-found regressions, all fixed before the final suites (see "Review findings").

## Round-one survival check

| item | state at `381eff6a8` | evidence |
|---|---|---|
| month cell keeps ≥ 1 clickable chip when crowded | intact (`draw_month_cell_chips`, `slots.clamp(1, 3)`) | `calendar/after/grid-compact.png`, `grid-month-short.png` |
| contacts toolbar compact breakpoint scales with the buttons | intact (`toolbar_is_compact`, `520 × scale`) | `contacts/after/large-text.png` (icon faces at 2×) |
| agenda task toggle rect bounded | intact (`TASK_OPEN_MIN_W`) | `calendar/after/grid-agenda.png` |
| `switcher_fits` vs painted trailing inset | intact (16 both) | `calendar/after/grid-month.png` |
| 1× rows on whole pixels | now **asserted**, not eyeballed: `metrics::tests`, `agenda::tests::one_x_metrics_reproduce_the_authored_rows` / `scaled_rows_land_on_whole_pixels`, `timeline_viewport::tests::geometry_reproduces_the_authored_design_at_one_x_and_grows_whole`, `app::tests::month_metrics_hold_scaled_text_and_keep_the_one_x_design`, contacts `row_and_toolbar_metrics_follow_the_text_scale` / `edit_form_rows_are_authored_at_one_x_and_hold_scaled_text`, weather `chip_metrics_tests::*`, maps `authored_rows_and_navigation_card_are_exact_at_one_x_and_whole_at_two_x` | — |

## calendar

Files: `apps/calendar/src/{metrics.rs, app.rs, agenda.rs, timeline_viewport.rs,
timeline_interaction.rs}`.

| id | surface | defect | viewport | file | before → after | status |
|---|---|---|---|---|---|---|
| R2-C1 | week/day timeline | first visible hour label clipped under the all-day strip: the seed hour is `now − 1 h`, fractional, so the 07:00 rule sat 10 px above the body top; at 2× in a 900 px window the same clip came back because the seed lay past the scroll range and the `Scrollable` clamped it to a fractional maximum | 1200×800; 800×900 2× | `timeline_viewport.rs::layout` | `calendar/before/grid-week.png` → `after/grid-week.png`; `after/grid-week-2x.png` | fixed — the seed floors to a whole hour and caps at the last whole hour the range reaches; `seeded_scroll_lands_on_a_whole_hour`, `seeded_scroll_caps_at_the_last_whole_hour_the_range_allows` |
| R2-C2 | event editor backdrop | modal scrim painted with `surfaces.overlay` (the opaque popover surface) — the grid vanished | 1200×800, 800×700 | `app.rs::draw_detail_frame` | `before/new-event.png` → `after/new-event.png`, `after/compact.png` | fixed — `ui::chrome::fill_scrim` at `modal_frame::MODAL_SCRIM_ALPHA` |
| R2-C3 | timeline min block | `at_least(16, 13.75 + 4) = 17.75` at 1× — fractional | 1× | `timeline_viewport.rs::geometry_for` | — | fixed — `at_least_whole(16, title + s1)` = 18 at 1×, 32 at 2×; asserted ≥ title + `s1` at both scales (the coordinator kept the pad: 16 would clip descenders) |
| R2-C4 | NOTES section header | reserve `line + s1 + s2 = 25.75` at 1× | 1× | `app.rs::notes_header_h` | — | fixed — `ceil` → 26; painter walks back with the same `s1`/`s2` |
| R2-C5 | month grid `chips_top` | round one's brief listed 34 vs an authored 30; the coordinator's review established the `s1` gap under the today pill is part of the design | 1× | `app.rs::MonthMetrics::new` | `after/grid-month.png` | kept at `6 + 2·pill_r + s1 = 34`; test asserts that expression, not a literal |
| R2-C6 | timeline hour gutter (fresh eyes) | fixed 56 px; the 2× "08:00" ran ~10 px into Monday's column | 800×900 2× | `timeline_viewport.rs` | `before/grid-week-2x.png` → `after/grid-week-2x.png` | fixed — gutter grows with the hour label; the test measures the real 2× label and asserts the gutter holds it after its inset |
| R2-C7 | restated tokens | `TIMELINE_HEAD_PAD`/`BLOCK_PAD` (s2/s1), agenda `SIDE_PAD`/`BOTTOM_PAD` (s6/s2), `TIMELINE_BAR_LANE` (s3), `EVENT_FORM_CHIP_MIN_H` (`metrics.selector_height`) | — | `timeline_viewport.rs`, `agenda.rs`, `app.rs` | — | fixed — `geometry_for(area, scale, &Spacing)`; `MonthMetrics::new(&Theme, …)`; `CalendarApp` caches `tokens_metrics` in `build_layout`; `event_form_chip_h(selector_h, s3, scale)` |
| R2-C8 | colour literals (coordinator) | `Color::WHITE` ×3, `from_rgba8(0,0,0,90)` shadow in production paint | — | `app.rs` 3745/4078/4212/4380 | — | fixed — `on_accent(4.5)` on the today pill, `Avatar::initial_ink(fill)` on chips/swatches, `surfaces.scrim.with_alpha(0.35)` for the card shadow |
| — | `line_h` copies | one per crate: `metrics.rs::line_h` (calendar) | — | — | — | done; the `authored`/`line_growth` helpers added mid-round were removed again when every reserve moved to `at_least_whole` |

## notes

Files: `apps/notes/src/sidebar.rs`.

| id | surface | defect | viewport | file | before → after | status |
|---|---|---|---|---|---|---|
| R2-N1 | sidebar header "+" | hand-rolled square with a typeset "+" | 1200×800 | `sidebar.rs::draw_header` | `notes/before/status.png` → `after/status.png` | fixed — `Chip::draw_at(rect, "", ChipLead::Glyph(Icon::Plus), quiet/tinted)`; hit rect is the same `new_page_btn_rect` |
| R2-N2 | header / rows / drag ghost | labels centred on the font size (1.75 px low at 1×, more at 2×) | all | `sidebar.rs` 1034/1187/1344 | as above, `after/status-2x.png` | fixed — one `line_h(size, scale)` probe (the crate's copy) for every row; no per-row measuring (reviewer 8) |
| N2 | slash menu | fixed 200×28 menu, double-scaled label | 2× | `ui/src/widget/slash_menu.rs` | — | open — F4 |
| N1' | overlay drawer clamp | notes re-clamps `(DRAWER_WIDTH × scale).min(width − s12)` (`app.rs:2400-2409`) because `SidebarDrawer` clamps only `overlay_width.min(o.width())` (`ui/src/widget/sidebar.rs:340`) and not a reveal band | 480×900 2× | — | — | open — F1 (move the reveal-band clamp into `SidebarDrawer`) |
| N3 | block drag handles 1× at 2× | — | 2× | `BlockEditor` | `after/status-2x.png` | open — F2 |

## contacts

Files: `apps/contacts/src/app.rs`.

| id | surface | defect | viewport | file | before → after | status |
|---|---|---|---|---|---|---|
| R2-K1 | list header search field | `(17.5 + 24) ∨ 38 = 41.5` at 1× | 1× | `app.rs` layout | `contacts/before/edit.png` → `after/edit.png` (field back at 38) | fixed — `at_least_whole(FIELD_H, line + 2·s2)` |
| R2-K2 | toolbar faces | widths `104 × grow` | 2× | `layout_toolbar_buttons` | `before/large-text.png` → `after/large-text.png` | fixed — authored width + the label's **measured** growth (`LabelGrowth::measure`, once per paint; a change requests a relayout for the next frame — paint never runs layout, reviewer 10); `toolbar_faces_grow_by_their_measured_labels` |
| R2-K3 | edit form at 2× (round one's K9) | fixed 18/38/32/28 rows and 78/130/… faces: section labels overprinted fields, type faces and add buttons overflowed, Save/Cancel clipped | 800×900 2× | `layout_form*`, `draw_form*` | `before/edit-2x.png` → `after/edit-2x.png`, `after/edit-wide-2x.png` | fixed — `FormMetrics` (identity row = `metrics.panel_row_height`, actions = `min_pointer_target`, add faces = `selector_height`, gaps `s3`/`s4`, authored label band 18 and face widths + measured label growth), solved once per `layout_form` and cached (`form_m`); type faces keep their authored height and centre in the row (reviewer 11); `edit_form_rows_are_authored_at_one_x_and_hold_scaled_text` proves 1× exactness and 2× non-overlap |
| R2-K4 | "+ Add …" buttons | hand-rolled stroke + text at `center − 7` | — | `add_button` | as R2-K3 | fixed — `Button::paint` (Neutral) |
| R2-K5 | restated tokens | `TOOLBAR_H` (s12), `TOOLBAR_BUTTON_H`/`FORM_ACTION_H` (`min_pointer_target`), `FORM_ADD_H` (`selector_height`), `FORM_TITLE_H` (`panel_row_height`), `FORM_TITLE_TOP`/`FORM_ROW_GAP` (s4/s3), `ROW_TEXT_PAD` (s3), text sizes 12/14/20 (`TypeScale::default().sm/base/xl`) | — | `app.rs` | — | fixed — `ContactsApp` caches `spacing`/`metrics` in `build_layout`; `FormMetrics::new(&Spacing, &Metrics, scale, &LabelGrowth)`, `ContactRowMetrics::new(&Spacing, scale)` |
| K7/K8 | Sort/Group `Chip::draw_at` labels and `Avatar::draw_at` initials 4× at 2×; the form's type faces elide to nothing for the same reason | 2× | `ui/src/widget/{chip.rs:274, avatar.rs:123}` | `after/large-text.png`, `after/edit-2x.png` | **still present** — X1 (verified after the merge) |
| K9 | `FormPage` adoption | the form now scales but is still hand-laid | — | — | — | open — composition migration (repeating `[type][value][−]` rows, add buttons, focus routing); not bounded this round |

## weather

Files: `apps/weather/src/{core.rs, app.rs}`.

| id | surface | defect | viewport | file | before → after | status |
|---|---|---|---|---|---|---|
| R2-W1 | detail header | `32.5 + 4 + 16.25 + 24 = 76.75` at 1× (authored 74) | 1× | `core.rs::detail_header_h` | `weather/before/detail.png` → `after/detail.png` | fixed — `at_least_whole(74, name + s1 + region + s4)`; `detail_header_is_authored_at_one_x` |
| R2-W2 | hero / tiles / hourly / daily / AQ (round one's W4) | `DetailBodyLayout::measure` was renderer-less with fixed card heights and inner offsets; at 2× the condition ran over the temperature, units over values, hour labels over each other, day rows overprinted | 800×900 2× | `core.rs::DetailBodyLayout`, the section painters | `before/detail-2x.png` → `after/detail-2x.png` | fixed — `measure(mw, snap, &Spacing, &Metrics, scale)`: card title rows = `metrics.bar_height` ∨ line + s3 + s2, day rows = 42 ∨ label + 2·s3, gaps `s3`, hourly band `s8`, AQI hero `s16`; the hand-authored cards live in one `BODY: BodyDesign` table (heights, inner offsets, off-scale text sizes) grown by their lines, and the painters read every offset and text size from the layout (`layout.text.*`) so no number is stated twice |
| R2-W3 | restated tokens | `CHIP_TEXT_PAD` (s3), `CHIP_GAP` (s2), `SECTION_GAP` (s3), text 12/24/10/14 (`TypeScale` sm/x2l/xs/base) | — | `core.rs` | — | fixed — `Core::set_layout_tokens(&Theme)` from `WeatherApp::build_layout`; `ChipMetrics::new(&Spacing, scale)` carries `gap` |
| R2-W4 | sun arc labels (fresh eyes) | "Sunrise 06:12 / Sunset 20:47" collide at 2× (fixed −60/−46/−26/−12 offsets in `draw_hero_sun` / `draw_sun_arc`) | 2× | `core.rs` ~2400-2480 | `after/detail-2x.png` | open — app (next pass: stack from measured lines like the hero) |
| R2-W5 | hourly chart internals | temperature labels touch the "Now" column at 2× | 2× | `apps/weather/src/chart.rs` | `after/detail-2x.png` | open — app (chart band content) |
| W6 | `NavShell` Back pill clips its label at 2× | 2× | shared `NavShell` | `after/detail-2x.png` | open — F1 (per the coordinator, F1 now scales shell breakpoints itself; weather's `grow` on the NavShell inputs was left untouched for the integration branch to remove) |

## maps

Files: `apps/maps/src/app.rs`.

| id | surface | defect | viewport | file | before → after | status |
|---|---|---|---|---|---|---|
| R2-M1 | directions panel width (round one's M7) | `PANEL_MAX_W = 384` at every scale — every 2× row elided | 1600×900 2× | `app.rs::panel_w` | `maps/before/default-wide-2x.png` → `after/default-wide-2x.png` | fixed — docked bounds scale with the text (≤ 60 % of the window); the 1× drawer width is unchanged and only a *scaled* drawer's extra width stops short of the controls column (reviewer 6); bounds combine with `max`/`min`, never `clamp` (reviewer 9) |
| R2-M2 | drawer / dock breakpoint | — | — | `overlay_mode` | — | reads `metrics.compact_breakpoint` (720); **not** scaled by the app — breakpoint scaling is the shell family's policy (coordinator) |
| R2-M3 | navigation card (round one's M6) | fixed 360×156: at 2× the summary row floated outside, "End" clipped, the title ran into the instruction | 2× | `navigation_card_size`, `draw_navigation_surface` | `before/nav-closed-2x.png` → `after/nav-closed-2x.png`; `before/default-wide-2x.png` → `after/default-wide-2x.png` | fixed — intrinsic size (`metrics.panel_width_list` × grow, 156 + lines' growth) handed to `FloatingPlacement::set_preferred_size` in `build_layout`; End pill = 50 + measured label growth (cached per scale, reviewer 4) × `s6` ∨ line + 2·s1; the instruction stack shifts by the title line's growth |
| R2-M4 | map controls cluster | lowered beneath the card it ran off the bottom of a short window and under the ⋯ button; beside the card it could cover the docked panel | 1280×360, 720×400, 380×330 | `map_controls_viewport` | `before/default-short.png` → `after/default-short.png`; `before/default-720x400.png` → `after/default-720x400.png` | fixed — below the card when it clears the overflow button, beside it when that clears the docked panel, otherwise HEAD's corner (reviewers 4 and 7); `map_controls_fit_a_short_window_beside_the_navigation_card`, `map_controls_never_land_under_the_panel_or_off_screen` |
| R2-M5 | directions rows | `(16.25 + 12) ∨ 28 = 28.25` at 1× and four siblings of the same shape | 1× | `app.rs` 3720-4043 | — | fixed — `authored_h` (whole pixels, exact at 1×); `text_vmid` now reads the crate's single `line_h` instead of its own `1.25` |
| R2-M6 | restated tokens | `MARGIN` (s4), `SEARCH_H` (s12), `PANEL_HEADER_H` (`bar_height`), `NAV_OVERLAY_BELOW` (`compact_breakpoint`), `NAV_CARD_W` (`panel_width_list`), `NAV_END_H` (s6), text 12/16 (`TypeScale` sm/lg) | — | `app.rs` | — | fixed — `MapsApp` caches `spacing`/`metrics` in `build_layout` (`margin()`, `search_h()`, `panel_header_h()`) |
| M8 | "12 vs 16 overflow inset" | not reproduced: the ⋯ button, the panel and the scale bar all sit at `s4`; the scale bar's own baseline is `clip.y1 − 26` (`canvas.rs:1283`) | 1200×800 | — | `after/default.png` | closed — no defect at this tree |
| M9 | `apps/maps/shots/*.png` | six committed PNGs predate the current panel design; unreferenced from code, docs or a generator | — | `apps/maps/shots/` | — | open — not regenerated: no route list produces them and nothing consumes them; recommend deleting or adding a `--shots` route script beside them |

## Review findings (coordinator-relayed; all fixed, none recorded)

Token derivation and the ratchet: every new module-level pixel constant I had added (contacts
21→37, maps 9→18, weather 21→44, calendar +1/+1) would have failed
`ui/tests/f1_spacing_token_audit.rs`. Reworked as above; counts now at or under baseline for
every touched file and the ratchet lines lowered: `calendar/src/agenda.rs 14→12`,
`calendar/src/app.rs 19→18`, `weather/src/core.rs 21→13`, plus two lines that were already
stale at `381eff6a8` and block the test for everyone until lowered —
`word-processor/src/zoom_control.rs 2→1` and `shell/src/shell/editor_app.rs 4→3` (not my
unit's files; only the baseline line moved, no code touched).

Removed-behaviour regressions restored: (1) timeline min block ≥ title + `s1` (18, not 16);
(2) `notes_header_h`/`detail_header_h` reserve with the same `s1`/`s2` the painter uses;
(3) `geometry_for` takes the theme spacing; (4) controls "beside" branch avoids the docked
panel; (5) `chips_top` keeps the `s1` gap under the today pill; (6) 1× drawer width in maps
unchanged; (7) controls fallback keeps HEAD's corner instead of laying out off-screen; (8)
notes rows share one probe; (9) `panel_w` cannot panic on a degenerate rect; (10) contacts
requests a relayout from paint instead of running one; (11) contacts type faces keep their
authored height. Tests pin token expressions (`spacing.s6`), not numbers; the gutter test
measures the real label.

## Tests

All cargo commands: `RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."
RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig cargo … --locked --no-default-features`.

| package | command | result |
|---|---|---|
| calendar | `cargo test -p calendar --lib` | 215 passed, 0 failed (210 at the base + `timeline_viewport::{geometry_reproduces_the_authored_design_at_one_x_and_grows_whole, seeded_scroll_lands_on_a_whole_hour, seeded_scroll_caps_at_the_last_whole_hour_the_range_allows}`, `agenda::scaled_rows_land_on_whole_pixels`, `metrics::at_least_whole_snaps_a_grown_row_up_to_a_pixel`) |
| notes | `cargo test -p notes` (full) | lib 126 passed; `it` 122 passed, **10 failed**, 1 ignored — `canonical_actions::{bound_first_type_undo_to_empty_retypes_once, bound_rich_split_preserves_both_halves_in_one_history_entry, bound_rejected_editor_delta_after_last_page_delete_shows_empty_cta, bound_first_typing_split_and_block_move_keep_schema_and_one_history_entry, bound_tree_reorder_is_one_logical_history_entry, bound_peer_undo_to_empty_retypes_once}`, `canonical_views::{parseable_block_spelling_is_preserved_and_shape_errors_install_nothing, stale_local_view_first_real_edit_uses_current_text_marks_and_one_owned_action, owned_close_restores_each_instances_selected_page_from_latest_record, stale_page_block_and_tag_fall_back_and_empty_page_has_no_stable_caret}` — the same ten round one reproduced at the base |
| contacts | `cargo test -p contacts --lib` | 170 passed, 0 failed (168 + `toolbar_faces_grow_by_their_measured_labels`, `edit_form_rows_are_authored_at_one_x_and_hold_scaled_text`) |
| weather | `cargo test -p weather --lib` | 87 passed, 0 failed (84 + `scaled_rows_land_on_whole_pixels`, `detail_header_is_authored_at_one_x`, `detail_body_layout_is_authored_at_one_x_and_grows_whole`) |
| maps | `cargo test -p maps --lib` | 184 passed, 0 failed, 1 ignored (pre-existing OSM tile test; 180 + `authored_rows_and_navigation_card_are_exact_at_one_x_and_whole_at_two_x`, `map_controls_fit_a_short_window_beside_the_navigation_card`, `map_controls_never_land_under_the_panel_or_off_screen`, `panel_width_follows_the_text_scale_and_the_drawer_keeps_its_one_x_width`) |
| ui | `cargo test -p ui --test it -- f1_spacing_token_audit` | 5 passed, 0 failed (no touched file above its baseline; stale lines lowered, see "Review findings") |
| all five | `cargo check -p calendar -p notes -p contacts -p weather -p maps --tests` | Finished |

## Limits

- macOS: no compositor, `shots`, `qos-control` or `ux-bench`; evidence is `--shots` /
  `--responsive-matrix` with the vello CPU rasteriser.
- `ui::widget::text::centered_text_top` is not reachable from apps (`mod text` is private,
  `ui/src/widget/mod.rs:1898`), so the apps centre with `(band − line_h)/2` inline, using the
  crate's one `line_h`; consolidating onto the toolkit remains an integration follow-up.
- The shared `Chip::draw_at` / `Avatar::draw_at` double-scale (K7/K8) is X1's; it is still
  visible in every 2× contacts capture and in the notes "+" chip's glyph sizing is unaffected
  (glyph lead, no label).
- Weather's remaining 2× defects (sun-arc labels, hourly chart labels) and the maps `shots/`
  PNGs are recorded above rather than fixed.
