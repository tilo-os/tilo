# S1 — Top bar, workspace wheel disc, strips/gutters, palette & overview host

Branch `ui-review/s1-shell-topbar-wheel-strips`, cut from `codex/macos-shell`
(`2d7e819b6`) plus the one-line `Cargo.lock` fix (`f9a579c9c`). Review of
`shell/src/shell/{topbar_chrome,sidebar_wheel_disc,sidebar_wheel_chrome,overlay_host,global_search,chrome_state}.rs`
and `shell/src/shell/search/` against `ui/HIG.md` §7, §7a and §9 (roles, tokens,
density, measured text).

## Method

1. Source pass over every owned file: literals in draw paths, roles vs.
   `ui::chrome::ChromeRole`, density vs. §9.6, measured vs. fixed text placement,
   which chrome publishes semantic bounds.
2. BEFORE evidence on the untouched binary: the 48-run `ux-bench` matrix
   (`target/ux-bench/s1-before`), exploratory palette captures in dark, light,
   compact and 200 % text (`target/ux-bench/s1-explore-palette-*`), the three
   new S1 scenarios (`target/ux-bench/s1-before-<scenario>`), and pixel
   measurements of glyph ink boxes with a dependency-free PNG reader
   (`target/ux-bench/png.py`; Pillow is not installed on this host).
3. Fixes expressed through `theme.tokens.*` and chrome roles; text placed by
   its own measured line height; every geometry change guarded by an inline
   `#[cfg(test)]` test in the file and, where the surface is addressable, by an
   `scripts/ux-bench/s1/*.json` bench check.
4. AFTER evidence: the same matrix, the same scenarios and the same crops on
   the rebuilt binary.

Pinned geometry that existing tests assert (`a_full_width_window_stops_short_of_both_screen_edges`,
`room_rows_advance_downward_without_overlapping`,
`compact_shortcut_help_keeps_content_inside_a_comfortable_footer_inset`, the
topbar centre-lane tests) was left unchanged; every existing test in the touched
files still passes unmodified except for one signature adaptation noted below.

## Findings

| id | surface | defect | theme / viewport | where (before) | evidence | status |
|---|---|---|---|---|---|---|
| S1-01 | Overview panel | Fixed row heights (`HERE 84`, `CAPTION 26`, `SHARE_ROW 28`, `ROOM_ROW 60`) do not follow the type scale: at 200 % text "Private to you", "Shared out of here" and the share row paint on top of one another | dark, 1280×800, `text_scale 2` (also compact/light/HC at 2×) | `overlay_host.rs` overview consts + `overview_*` geometry fns | `s1-before/041-workspace-overview-large-text/overview.png`, crop `s1-crops/before-overview-large-text-2x.png` → `s1-after-s1-final/…overview-density…/overview.png`, crop `s1-crops/after-overview-large-text-2x.png` | **fixed** — `OverviewMetrics::for_theme` (measured line heights + `s1/s2/s3/s4/s6`), cached per layout pass so hit-testing and paint agree |
| S1-02 | Overview panel | Panel drawn at `radii.lg` (12) and shadowed with a square field; HIG §9.1 Panel = `radii.xl`, shadow follows the outline | all | `draw_overview` | before/after `overview.png` | **fixed** — `radii.xl`, `ui::surface::shadow(panel, radius, e4)` |
| S1-03 | Overview panel | Off-scale metrics: `PAD 20`, `SECTION_GAP 18`, `ROW_RADIUS 10`, `ROOM_ROW_GAP 6`, row inner pad `14`, literal `+6`/`+2`/`+10`/`-16` offsets; caption text placed by a fixed `+6` | all | overview consts and `draw_overview_*` | source | **fixed** — `s4` pad/section gap (§9.6), Control radius `lg`, `s2` row gap, `s4` row pad, captions/lines placed by `s1`/`s2` around measured heights |
| S1-04 | Overview panel | Here-block faces centred on a fixed `y0 + 22`, chip `22 px` fixed | all | `draw_overview_here_room` | source | **fixed** — faces centred on the name's measured line; chip = `sm` line + `s2` |
| S1-05 | Palette filter band | Chips and scope switch `24 px` tall (§9.6 Selector 28) and set at `scale.xs` regular; at 200 % text the 20 px labels sit outside their pills | dark/light/compact/200 % | `FILTER_CONTROL_HEIGHT_PX`, `draw_filter_chip`, `draw_search_banner_scope_switch` | `s1-explore-palette-lt/palette-query.png` (before) → `s1-after-explore-palette-lt-final2/palette-query.png`; crops `s1-crops/before-palette-band-large-text-2x.png` → `after-palette-band-large-text-2x.png`, `before-palette-band-2x.png` → `after-palette-band-2x.png` | **fixed** — `filter_control_height`: Selector 28, grows with the measured `sm` medium label + `s2`, stopping `s1` short of the band row so two rows keep a gutter; labels `scale.sm` medium measured and drawn at the same weight |
| S1-06 | Palette filter band | `SCOPE_SEGMENT_WIDTH_PX 88` fixed → "W…"/"Ev…" at 200 %; `CHIP_GAP 6`, `CHIP_PAD 18`, `track.x0 - 10`, `seg.width - 10` off the 4-px grid | 200 % text | `search_scope_track_rect`, `draw_search_banner` | same captures | **fixed** — segment width measured from the widest scope label + `2·s3`; gap and pad are the *live* `s2` / `s3` tokens passed into `lay_out_chip_strip` (not pinned literals, so a custom or `with_ui_scale` theme moves them with every other chip); gutters `s3`/`s2`. Scope radios publish and hit-test the segment's full-height slice of the track, so they share the chips' edges |
| S1-07 | Palette | Card shadow via the deprecated `soft_shadow` sprite with `colors.neutral[0]` (§9.13: neutral ramp in chrome) and bespoke `44/14/0.55` | all | palette draw in `draw_top_overlays` | source | **fixed** — `ui::surface::shadow(rect, surface_radius, e4)`, alpha follows the open transition |
| S1-08 | Palette filter band | Chips and scope segments published no semantic node: not addressable by AT, scripts or the bench | all | `accessibility()` CommandPalette arm | `s1-before-palette-filters` (16 "missing target" failures) | **fixed** — "Search filters" group with `Filter: <kind>` toggles (`palette:filter:<slug>`, `checked` + `selected`) and `Scope: <label>` radios (`palette:scope:<slug>`), bounds = painted rects, routed through `synthetic_accessibility_rect`. The band's index is derived from the palette's own child count (`search_filters_child_index`), never a literal; slugs are `[a-z0-9-]` only and de-duplicated with an ordinal so two custom kinds can never share an address |
| S1-09 | Top bar | Centre selector dots: 24 × 32 hit rects (§9.6 floor 32×32) | all | `draw_center_band` `dot_gap 24`, slot `±12` | `s1-before-topbar-alignment` (`[24,32]` for all three) | **fixed** — `CENTER_DOT_PITCH_PX` 32 (= `s8`); the content lane starts after the third pitch; `activity_content_width(theme)` derives from the same lane fn |
| S1-10 | Top bar | Centre band 34 px tall while every other bar control is 32: the band's edges sat 1 px outside the pills either side | all | `band_h = 34.0` | ink measurement (`png.py ink`): band top 7 vs controls 8 | **fixed** — `BAR_CONTROL_PX` (= `s8`) shared by nav cluster, pill, band, tray |
| S1-11 | Top bar | Workspace pill asymmetric: 6 px before the label, 10 px after the gear (`x-6 … x+w+28`, gear at `+6/-2/+12`) | all | `draw_workspace_pill` | crop `s1-crops/before-topbar-left-4x.png`; ink box x0 138 vs pill 131, x1 205 vs 216.5 | **fixed** — `s2` before the label, `s2` label→gear, `s2` after the gear, gear centred on the pill's line. The label's x is deliberately unchanged (137 at 1280): `navigation-layout.json` and `run-suite.py`'s `VARIANT_PIXEL_PROBES` sample its glyphs at authored pixels, and an earlier draft that shifted the text 4 px right failed that probe in all six variants — so the capsule moved, not the text, and the cluster→pill gutter is `s1` (`navigation-layout.json`'s 4..12 gap check reads 4) |
| S1-12 | Top bar | Bar shadow: literal `Color::from_rgba8(0,0,0,46)` and `28 px` (§9.13 literal colour; §9.1 Bar = `e1`) | all, wrong on light/HC | `draw_backdrop` | source | **fixed** — `elevation.e1.color` (alpha honoured) fading over `s6` |
| S1-13 | Top bar | System-monitor readouts placed at fixed `x` offsets (`+26`, `-96`, `-70`, `-40`, `-18`) and fixed `y` (`-6`, `-7`) instead of measured advances/line heights; sparkline range `+70 … -100`. At the 800 px viewport the fixed offsets already painted RAM over the CPU value (lane 128 px, RAM at `x1 - 96`) | all, worse at 200 % and compact | `draw_system_monitor_widget`, `draw_metric_label`, `draw_cpu_sparkline` | source | **fixed** — `monitor_columns()` positions every run from its measured width at its own weight/size, gaps `s1`/`s3`, right cluster anchored to the lane end, each run centred on its own line height; in a narrow lane the pairs drop from the inside out (RAM, then TMP) instead of overlapping, and the sparkline hides when < `s12` of room |
| S1-14 | Top bar | Calendar note nudged `ty + 1.0` instead of centred on its own smaller line | all | `draw_calendar_widget` | source | **fixed** — centred on the `xs` line height; gaps `s2` |
| S1-15 | Top bar | Unread badge: `9 px` font off the type scale, `Color::WHITE` ink, `8.0` radius, `-1/+9/20` offsets | all; ink wrong on light | `draw_tray_notification_badge` | source | **fixed** — `scale.xs`, `colors.ink_on(danger, text_primary, 4.5)`, Selector radius, size = label + `s1` (≥ `s4`), width reserved from the same measurement |
| S1-16 | Top bar | Media indicator pill 22 px tall is also its pointer target | with active capture | `media_indicator_rect` | source | **fixed** — `media_hit_rect()` = 32 px tall control target for hit-test and semantics; painted mark unchanged |
| S1-17 | Top bar | Tray pad/gap/slot literals (`8`, `4`, `24`, `32`) | all | `draw_tray_pill_body` | source | **fixed** — `s2`, `s1`, `s6`, `BAR_CONTROL_PX` (identical pixels) |
| S1-18 | Wheel disc | Labels truncate to "H…" at 200 % text: both chip sizes (48/64) map to the same 1.6× label factor, so stepping down did nothing | 200 % | `active_chip_height`, fixed inactive `SECTOR_CHIP_H` | `s1-crops/before-disc-label-large-text-3x.png` → `s1-after/011-desktop-large-text/desktop.png` | **fixed (partially at the shallowest slot)** — `chip_height_for` steps both sizes down to the shared Chip resting height (28, label exactly `scale.sm`) before truncating; the resting ring steps down *as one* (`inactive_chip_height`) so it stays uniform. In the live 1280×800 slot the band is only 220×343 and at 200 % not even the floor holds "Home", so the name is elided *at the floor* — a 24 px "Hom…" at the person's own size — instead of the family size's 38 px "H…". Fitting the full name there needs a deeper band (the disc geometry constants `BAND_TANGENT_FRACTION`/`SLOT_VERTICAL_INSET_FRAC`, a design decision recorded as open). Tests `large_text_steps_a_chip_down_before_it_truncates_a_name`, `a_name_that_fits_nowhere_is_elided_at_the_persons_text_size`, `a_shallow_slot_at_large_text_elides_at_the_floor_not_the_family_size` |
| S1-19 | Wheel disc | Zero token references; `LABEL_RIM_GAP/INNER_INSET/BAND_LEFT_INSET 12`, chip 48/64 | all | consts | source | **fixed (parity)** — documented as `s3`/`s12`/`s16` and pinned by `label_metrics_sit_on_the_spacing_scale`; kept `const` because the band geometry is queried without a theme |
| S1-20 | Wheel disc | No semantic nodes for sectors or the add button (only `test_id = wheel_disc`) | all | — | `s1-before-topbar-alignment` (`wheel:*` missing) | **fixed** — "Workspace wheel" group: `Switch to <name>` buttons (`wheel:workspace:<i>`, bounds = painted hit rect, `selected`), `Add a workspace` (`wheel:add`). The production a11y dispatcher route is implemented (`perform_semantic_action` on the `wheel:*` targets turns the wheel / adds; `synthetic_accessibility_rect` gives the click fallback the sector, not the disc centre), plus the key-path route `perform_action_at` |
| S1-21 | Strip gutters | `WHEEL_TO_STRIP_GAP_PX 28`, `STRIP_TRAILING_GUTTER_PX 28` off the grid | all | `overlay_host.rs` | source | **open — kept** (see below) |
| S1-22 | Palette rows | At 200 % text every result row overlaps the next: `ROW_HEIGHT 32 / TWO_LINE 42 / FILTER_STRIP 36 / HEADER 24` are fixed in `ui::widget::command_palette` | 200 % | `ui/src/widget/command_palette.rs` | `s1-explore-palette-lt/palette-query.png` | **open — needs F4** (also limits S1-05: the band row is 36 px, so a 200 % chip is capped at 36) |
| S1-23 | Home launcher | Compact (800×700) heading "Make something together" starts under the top bar | compact | launcher, not shell | `s1-before/007-desktop-compact/desktop.png` | **open — needs A/S2** (today's WIP repair per coordinator; not in this tree) |
| S1-24 | Top bar | Icons (`18`, `14`, `17`, `12` px) do not follow text scale; the 14 px gear beside a 24 px label reads small at 200 % | 200 % | `topbar_chrome.rs` | `s1-crops/before-topbar-left-large-text-4x.png` | **open** — icon sizing policy belongs to F1/F2 (glyphs are not text); noted, not changed |
| S1-25 | Top bar | `center_band_width` lane budget literals (`580`, `220`, `260`, `360`, `16`) | all | `center_band_width` | source | **open — kept**: tuned by `narrow_topbar_releases_center_width_for_workspace_identity` and the large-text lane tests |
| S1-26 | ws-settings / quick settings / shortcut help | Same `soft_shadow(neutral[0])` pattern as S1-07 | all | `overlay_host.rs` other overlays | source | **open — needs S2** |
| S1-27 | Overview room row | Idle row fill reads `colors.neutral[1]` (§9.2 says `surfaces.*`) | all | `draw_overview_room_row` | source | **open** — colour semantics, not alignment; left for the theme owner (F1/S2) to pick the surface token |
| S1-28 | Filter chips | Chip pill is 28 px tall while §9.6 also sets a 32×32 pointer floor; the two §9.6 rows conflict for Selectors | all | — | — | **open — needs F1 (HIG)**: decide whether Selectors get an inflated hit rect |
| S1-29 | Palette scope switch | At 200 % text two segments need more than the 55 % share of the band the track is capped at, so "Workspace"/"Everywhere" still elide to "Works…"/"Every…" (with `s2` padding kept) | 200 % text | `search_scope_track_rect` cap | `s1-after-explore-palette-lt-final/palette-query.png` | **open — kept**: the cap protects the first chip row; letting the switch wrap to its own row at large text is a layout change for F4/S1 together |
| S1-31 | Wheel disc | At 200 % text in the live 1280×800 slot (band 220×343) no chip step holds "Home"; the label is now elided at the person's size ("Hom…") but a full name needs a deeper band | 200 % text, 1280×800 | `sidebar_wheel_disc.rs` band geometry (`BAND_TANGENT_FRACTION 0.93`, `SLOT_VERTICAL_INSET_FRAC 0.16`, `LABEL_MIN_W 62`) | `s1-after-disc-lt-final/desktop.png`, crop `s1-crops/after-disc-label-large-text-3x.png` | **open — design**: deepening the band trades the third visible workspace (`every_workspace_stays_on_the_disc_at_every_size`) for label room; needs the disc's owner to choose |
| S1-30 | Shell right-click menu | Coordinator hand-off (from F4): the shell's tile context menu never draws or receives events. Verified — and it is not bounded. (1) `shell/src/shell/mod.rs:183` `ctx_menu: ContextMenu` belongs to `Shell`, a struct with **no production constructor** anywhere in `shell/src` (only fixtures); `open_default_tile_context_menu` (`:759`) is `#[cfg(any(test, debug_assertions))]`, reachable only from `simulate_right_click_on_active_tile`, and its items are no-op closures. (2) The live desktop's right-click path is `ui/src/widget/tabs.rs:2149` → `pending_context_menu` → `WindowGroup::take_tab_context_menu` (`ui/src/widget/window_group.rs:709`), which has **zero consumers** in `shell/src` or `ui/src`. So no shell-owned right-click menu exists at runtime; whatever menu the user saw without a hover highlight is an app-level `ui::ContextMenu` (F4's widget). Wiring the tab menu means consuming the request in the frame/group-row layer (F6, `frame.rs` — off-limits here), defining the real tab actions, drawing in the overlay pass and routing input — a feature, not an alignment fix | all | `mod.rs:183,252,719-769`; `tabs.rs:2149`; `window_group.rs:709` | source (grep counts above) | **open — needs F6 + F4** |

### Off-grid metrics kept, and why

- **Strip gutters 28 px** (`WHEEL_TO_STRIP_GAP_PX`, `STRIP_TRAILING_GUTTER_PX`):
  `a_full_width_window_stops_short_of_both_screen_edges` pins the trailing gutter
  to the leading one within 1 px, and both values feed
  `workspace_rail_clearance_for_viewport`, consumed by `collaboration_launchpad.rs`
  and every group-row span. Moving to 24 or 32 shifts every window on every
  display; that is a frame/strip decision (F6/S2) and not an alignment defect, so
  the values stay and are recorded here.
- **`TOPBAR_HEIGHT_PX 48`** vs §9.6 "Bar 40": the file documents why (32 px
  targets need 8 px of air); 48 *is* `spacing.s12`, now pinned by
  `bar_density_sits_on_the_spacing_scale`. Every bar control is 32 (`s8`).
- **`SEGMENT_INSET_PX 2`** and **`CHIP_MIN_W_PX 34`**: a bezel detail inside a
  control and a degenerate-case floor; neither positions anything a user reads.
- **`SIDEBAR_WIDTH_PX 320`** in `sidebar_wheel_chrome.rs` duplicates
  `SIDEBAR_WIDTH_MAX_PX` in `overlay_host.rs`; the chrome file is documented dead
  code ("not mounted") but `foreign.rs`, `mod.rs` and `shots/scenario.rs` still
  read the constant. Consolidating is a cross-file change outside this unit's
  files; left as is.
- **`LABEL_MIN_W 62`, `RIM_SHADOW_REACH 26`** in the disc: empirical geometry
  bounds, not spacing.
- **`OVERVIEW_PANEL_WIDTH_PX 720`**: matches the workspace-settings modal (S2);
  §9.6 lists 320/360/560 but the two panels must move together.

## Bounds and addresses added

- Wheel disc: `semantic:wheel` (group), `semantic:wheel:workspace:<i>`
  (`Switch to <name>`, selected state, bounds = painted sector), `semantic:wheel:add`
  (`Add a workspace`). Activation via AccessKit `Activate` drives `click_to_spoke`
  / `add_workspace`. Names are verbs so they cannot collide with the overview's
  `semantic:Home` / `semantic:New workspace` used by `workspace-overview.json`.
- Palette filter band: `semantic:palette:filters`, `semantic:palette:filter:all`,
  `semantic:palette:filter:<kind-slug>` (`Filter: <Kind>`, `checked`),
  `semantic:palette:scope:<workspace|everywhere|this-app>` (`Scope: <label>`,
  radio, `checked`). Geometry routed through `synthetic_accessibility_rect`
  path `[palette, 2, group, i]`.
- Top bar: the eleven `semantic:topbar:*` nodes already existed; their bounds
  now share one height (`BAR_CONTROL_PX`) and the three centre dots are 32×32.

## Tests

New inline tests (all in the touched files, all green):

- `topbar_chrome.rs`: `bar_controls_share_one_height_and_centre_line`,
  `workspace_pill_pads_its_label_and_gear_symmetrically`,
  `monitor_columns_drop_pairs_from_the_inside_out_in_a_narrow_lane`,
  `bar_density_sits_on_the_spacing_scale`.
- `sidebar_wheel_disc.rs`: `label_metrics_sit_on_the_spacing_scale`,
  `large_text_steps_a_chip_down_before_it_truncates_a_name`,
  `a_name_that_fits_nowhere_is_elided_at_the_persons_text_size`,
  `a_shallow_slot_at_large_text_elides_at_the_floor_not_the_family_size`,
  `every_sector_is_a_bounded_semantic_target_that_activates` (drives the
  production `perform_semantic_action` route and `synthetic_accessibility_rect`).
- `overlay_host.rs`: `overview_metrics_sit_on_the_spacing_scale`,
  `overview_rows_grow_with_the_text_scale_instead_of_stacking`,
  `filter_controls_grow_with_their_label_and_stay_inside_the_row`,
  `filter_chips_and_scope_segments_are_bounded_semantic_targets` (also
  covers slug uniqueness for hostile custom labels).

Code review (`/code-review high`) on the diff raised seven findings; six were
fixed before commit (dispatcher-reachable disc activation; live `s2`/`s3`
tokens instead of pinned chip literals; a compact-lane guard for the monitor;
palette band index by identity instead of a positional constant; slug
collisions; uniform resting-ring height). The seventh — the new semantic names
are English literals — follows the file's existing precedent (`"Open overview"`,
`"New workspace"`, …): `shell/src` has no fluent/i18n path at all today, so it
is recorded as open for the shell i18n owner rather than solved with a
one-off.

Existing tests: unchanged except `large_text_activity_stays_single_line_inside_center_content_lane`
and `compact_large_text_preserves_the_full_activity_sentence_and_detail_route`,
which now pass the theme to `activity_content_width(&theme)` (the lane is derived
from the same function the band draws with); their assertions are untouched and
still pass with the narrower lane.

Runs (exact counts in "Runs" below).

## Scenario files

The three S1 scenarios live in `scripts/ux-bench/s1/` rather than directly in
`scripts/ux-bench/`: `scripts/ux-bench/test_suite.py` pins the directory's
`*.json` glob to exactly eight journeys, and `run-suite.py`'s default (no
positional arguments) runs that same glob as "the 48-run matrix". Dropping
new files beside them would have failed the suite's own test and silently
turned the 48-run matrix into a 66-run one — which is exactly what happened
on the first AFTER pass (`target/ux-bench/s1-after`, 66 runs) before the
files were moved. Run them explicitly:

```sh
python3 scripts/ux-bench/run-suite.py --matrix --out target/ux-bench/<dir> \
  scripts/ux-bench/s1/s1-topbar-alignment.json \
  scripts/ux-bench/s1/s1-palette-filters.json \
  scripts/ux-bench/s1/s1-overview-density.json
```

## Runs

Every cargo command was run as
`RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." cargo … --no-default-features`
(the `RUSTFLAGS` value is this worktree's own macOS flag set; see Limits for why
it had to be set inline).

Tests:

- `cargo test -p shell --lib --no-default-features -- topbar_chrome sidebar_wheel_disc overlay_host::overview_panel_tests overlay_host::overlay_event_absorb_tests overlay_host::palette_dispatch_tests`
  → **163 passed, 0 failed, 1 ignored** (the ignored test is the pre-existing
  `render_notification_history_accessibility_matrix_at_1x`, marked
  "visual review: needs an offscreen wgpu adapter"). 2199 filtered out.
- `cargo test --locked -p ui --lib --no-default-features ux_bench` → **18 passed, 0 failed**.
- `python3 scripts/ux-bench/test_suite.py` → **4 tests OK** (with the S1
  scenarios in `scripts/ux-bench/s1/`; in the parent directory they fail its
  eight-journey count assertion — see "Scenario files").
- Compile gate: `cargo check -p shell --lib --bin shell --bin ux-bench --bin qos-control --no-default-features`
  → clean. `cargo check -p shell --lib --bins --no-default-features` fails on
  macOS in two binaries this unit does not touch and that are Linux-only:
  `click_test_client` (`E0432/E0433`: `smithay_client_toolkit` is not a
  dependency on this target, 6 errors) and `shots` (`E0432` unresolved import).
  `cargo check -p shell --tests` cannot run here because `shell/tests/it.rs`
  is Linux-only, as the brief anticipated.

Bench:

| run | directory | runs | pass | fail | notes |
|---|---|---|---|---|---|
| BEFORE matrix | `target/ux-bench/s1-before` | 48 | 36 | 12 | the 12 are `files-scroll` ×6 and `open-files` ×6, all `missing or ambiguous target: semantic:Files` — pre-existing at base |
| BEFORE S1 scenarios | `target/ux-bench/s1-before-{topbar-alignment,palette-filters,overview-density}` | 3 | 0 | 3 | topbar: 3× dot targets `[24,32]`, cluster gap 6, `wheel:*` missing (8 failures); palette: 16 missing-target failures; overview: `semantic:Home` block `[680,84]` under the text it must hold |
| BEFORE exploratory | `target/ux-bench/s1-explore-palette-{dark,lt,light,compact}` | 4 | — | — | captures only (Super+K, type `n`) |
| first AFTER matrix | `target/ux-bench/s1-after` | 66 | 48 | 18 | ran while the S1 files still sat in the glob (48 + 18); 12 pre-existing + **6 regressions** in `navigation-layout`'s authored contrast probe caused by the 4 px label shift — fixed by keeping the label's x (S1-11), then rerun |
| AFTER matrix | `target/ux-bench/s1-after-final` | 48 | 36 | 12 | identical failure set to BEFORE (the 12 `semantic:Files` runs); `navigation-layout` passes in all six variants |
| AFTER matrix (final binary) | `target/ux-bench/s1-after-final2` | 48 | 36 | 12 | rerun after the disc floor-fallback change; the same 12 pre-existing `semantic:Files` failures and nothing else — no regression against BEFORE |
| AFTER S1 scenarios | `target/ux-bench/s1-after-s1-final2` | 18 | 18 | 0 | 3 scenarios × baseline/compact/light/high-contrast/retina/large-text |
| AFTER exploratory | `target/ux-bench/s1-after-explore-palette-{dark,lt}-final2`, `s1-after-disc-lt-final` | 3 | — | — | captures for the crops below |

Crops for side-by-side reading (all under `target/ux-bench/s1-crops/`):
`before-topbar-left-4x.png` / `after-topbar-left-4x.png` (pill padding),
`before-topbar-center-3x.png` / `after-topbar-center-3x.png` (dot pitch),
`before-overview-large-text-2x.png` / `after-overview-large-text-2x.png`
(row collisions), `before-palette-band-2x.png` / `after-palette-band-2x.png`
(28-px chips, `sm` medium labels), `before-palette-band-large-text-2x.png` /
`after-palette-band-large-text-2x.png` (labels inside their pills, `s1` row
gutter), `before-disc-label-large-text-3x.png` / `after-disc-label-large-text-3x.png`
("H…" at 38 px → "Ho…" at the person's 24 px).

## Limits

- macOS host: no `shots`, no `--backend headless`, no `qos-control`; `ux-bench`
  is the only driven-evidence path. `shell/tests/it.rs` (`--tests` gate) is
  Linux-only and does not compile here; the compile gate used is
  `cargo check -p shell --lib --bins --no-default-features`.
- `scripts/ux-bench/usability.py` and `--interactive` mode named in the brief do
  not exist at this base commit (they are in the user's uncommitted WIP), so the
  whole-tree overlap lint could not be run; overlap coverage comes from the
  `no_overlap` checks in the three `scripts/ux-bench/s1/*.json` scenarios and
  the inline tests.
- Pillow is not installed; crops and ink measurements use
  `target/ux-bench/png.py` (pure Python, not committed).
- The build host's parent checkout `/Users/xaver/dev/qos/.cargo/config.toml`
  adds `-C target-cpu=generic` for non-Linux aarch64, which cargo merges into
  worktree builds and which breaks `ring`'s static-feature asserts. Every cargo
  command here was run with `RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."`
  (this worktree's own macOS flags) to neutralise it; nothing in the repo was
  changed for this.
- Pre-existing at base: the `files-scroll` and `open-files` journeys fail in
  every variant with `missing or ambiguous target: semantic:Files` (12 of 48
  matrix runs); unrelated to this unit and unchanged by it.
