# UI alignment review — A6 Content I (calendar, notes, contacts, weather, maps)

Branch `ui-review/a6-content-1`, cut from `codex/macos-shell` @ `087d5a62c` (plus the
`ui-review/lockfix` cherry-pick `cd67a888c`). Worktree
`/Users/xaver/dev/qos/.claude/worktrees/agent-aa0c082321abe39da`. Captures under
`target/ui-review/<app>/{before,after}/` in that worktree (BEFORE = base commit binaries,
AFTER = this branch). Every PNG cited below was opened and inspected.

## Method

1. Read `ui/HIG.md` §1/§7/§7a/§8, `ui/MANUAL_LAYOUT.md`, `ui/FORM_COMPOSITION.md`,
   `docs/hand-rolled-ui-audit.md`, the 2026-09-15 all-app review (review-1/2/3,
   corrected-review-1, closure register) and text-geometry plan tasks 7/10/13/23.
2. Source pass over each app's layout files for private spacing constants, literal offsets,
   font-size centring, sibling inset mismatches and split paint/hit geometry.
3. BEFORE captures: responsive matrix (dark + light), 2× at 800×900, 480×900, 800×700, every
   listed scenario, plus each app's bespoke routes (`cal-routes.sh`, `cal-grid.sh`,
   `maps-routes.sh`, `shot.sh` in `target/ui-review/`).
4. Fixes derive every row/chip/header height from the measured text line
   (`TextRenderer::measure(...).1`, or the identical `size × text_scale × 1.25` where a layout
   pass has no renderer), keep the authored 1× value as the floor (`max(authored, measured)`),
   and keep one geometry description per surface (the same struct feeds paint, hit rects,
   scroll extent and accessibility).
5. Regressions per fix (crate tests + FrameHarness-style layout tests), AFTER captures,
   comparison.

Conventions observed: theme spacing/radii tokens for every new metric; deliberately off-scale
authored values are named constants with the reason in their doc comment (e.g.
`MONTH_CHIP_GAP = 3`, `MONTH_NUM_TOP = 6`, `ROW_GAP = 2`, `TOP_PAD = 18`); base text sizes are
named constants passed to the renderer (which applies the text scale itself — theme
typography tokens are already pre-scaled, so they must not be fed to the renderer, see
`ui/src/widget/modal_frame.rs:361` for the tree's convention).

## calendar

Scope: month grid, week/day timeline, agenda, event form, topbar, empty state.
Files: `apps/calendar/src/{app.rs, agenda.rs, timeline_viewport.rs, timeline_interaction.rs,
topbar.rs, lib.rs}`, new `apps/calendar/src/metrics.rs`.

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| C1 | month event chips | chip fill 17 px / pitch 20 px fixed; 2× text spills out and stacks onto the next chip | 800×900 dark 2× | `app.rs:4004-4050` (`cy + 17.0`, `cy += 20.0`) | `calendar/before/grid-month-2x.png` → `after/grid-month-2x.png` | fixed — `MonthMetrics` (chip_h/pitch from measured 10 pt line) |
| C2 | month today pill | radius 12 fixed, "16" overflows at 2×; today number sat ~3.5 px lower than siblings at 1× | 1200×800 + 2× | `app.rs:3936-3945` | same images; 1× `before/grid-month.png` → `after/grid-month.png` | fixed — pill radius from label line; every day number centred on the pill axis |
| C3 | month weekday header | 28 px fixed row, 2× labels touch day numbers; header x-inset 10 vs day-number 8 | 2× | `app.rs:3834,3873-3888` | as C1 | fixed — header_h = max(28, line + s3); shared `s2` inset |
| C4 | month "+N more" | overflow line drawn in a slot that no longer fits → clipped; was not a click target | 2× | `app.rs:4001-4013` | `after/grid-month-2x.png` ("+3 more" fully inside cell) | fixed — last slot reserved when overflowing, ≥1 chip always drawn, overflow line opens the day |
| C5 | event form header | "EVENT · SAVES AUTOMATICALLY" row fixed 22 px; at 2× label abuts title field, dot and close/trash icons off the label's centre | 800×900 2× | `app.rs:2846,4237-4270,4297-4312` | `before/new-event-2x.png` → `after/new-event-2x.png` | fixed — `detail_header_h()` from measured label; dot, label and controls share the row centre |
| C6 | event form chips | 28 px pill fixed, text at `center-7`; 2× text overflows (plan task 23) | 2× | `app.rs:6751-6779` | as C5 | fixed — `chip_with_lead`, height = max(28, line + s3), measured centring |
| C7 | colour chip | label spelled "●" and a circle was painted at `x0+12` → two dots at 2×, smudge at 1× | 1× and 2× | `app.rs:4756-4776` | `before/new-event.png` → `after/new-event.png` | fixed — label without glyph, measured lead reserves the swatch |
| C8 | NOTES section header | label at `notes_top-26`, rule at `label+18`; 2× rule crosses the label (plan task 23) | 2× | `app.rs:2891-2893,4482-4507` | as C5 | fixed — `notes_header_h()` reserved in `form_layout`, painter walks back the same terms |
| C9 | form labels | "All day", "Starts", "Ends" centred on font size | 2× | `app.rs:4535-4541,4617-4624` | as C5 | fixed — measured line height |
| C10 | agenda rows | 30 px rows, 26 px headings, location wrapped to a 126 px column → rows overprint at 2×; locations left-aligned while priority flags right-aligned at 1× | 800×900 2× / 1200×800 | `agenda.rs:117-170,214-400,421-442` | `before/grid-agenda-2x.png` → `after/grid-agenda-2x.png`; `before/grid-agenda.png` → `after/grid-agenda.png` | fixed — `AgendaMetrics` shared by extent, paint and hits; single-line elided location right-aligned with the flag |
| C11 | agenda task toggle rect | (introduced by C10, caught in review) toggle region ran to the flag column | — | `agenda.rs` `draw_task_row` | — | fixed — bounded by `TASK_OPEN_MIN_W` (120 px open-task region kept) |
| C12 | week/day timeline | header 56 px / all-day strip 26 px / block subtitle at `+18` fixed; at 2× weekday and day number touch, all-day text overflows, subtitle overprints title | 800×900 2× | `timeline_viewport.rs:13-15,110`, `timeline_interaction.rs:491-541,666-690,769-784` | `before/grid-week-2x.png` → `after/grid-week-2x.png` | fixed — `TimelineViewport::geometry_for(area, text_scale)`; header/strip/min-block from measured lines; subtitle starts at measured title height |
| C13 | empty onboarding | title/hint block sized by font size, no backing; hint lands on the middle row's day numbers | 390×844 phone | `app.rs:4992-5028` | `before/dark/dev.qos.calendar-phone.png` → `after/dark/dev.qos.calendar-phone.png` | fixed — measured block on a raised plate (radius lg, s6/s4 padding) |
| C14 | topbar insets | leading 16 vs trailing 14; `switcher_fits()` used 14 | 1200×800 | `topbar.rs:213,270,392` | `before/grid-month.png` → `after/grid-month.png` | fixed — 16 both, fit rule in sync |
| C15 | `center_text` | centred on `size/2`, off by half the scale delta at 2× (segments, compact Today "•", today number) | 2× | `app.rs:6375-6395` | `before/grid-month-2x.png` (blank-looking Today button) → `after/grid-month-2x.png` | fixed — measured line box |
| C16 | timeline initial scroll | first visible hour label clipped under the all-day band (pre-existing, both scales) | 1200×800 | `timeline_viewport.rs::layout` seeding | `after/grid-week.png` | open — app (scroll offset should land a label height above the seeded hour) |
| C17 | modal backdrop | editor overlay hides the calendar completely although the code comment intends a still-visible grid | 1200×800 | `app.rs draw_detail_frame` | `after/new-event.png` | open — app, design question |
| C18 | notes drag handles (in calendar's BlockEditor) | 1× size at 2× | 2× | shared `BlockEditor` gutter | `after/new-event-2x.png` | open — needs F2 |

Tests: `apps/calendar/src/metrics.rs` (4), `agenda::tests` (+2:
`one_x_metrics_reproduce_the_authored_rows`, `scaled_rows_hold_their_text_and_grow_the_scroll_extent`),
`app::tests` (+2: `month_metrics_hold_scaled_text_and_keep_the_one_x_design`,
`event_form_reserves_measured_header_and_notes_rows_at_two_x`); existing
`large_text_desktop_keeps_modal_form_controls_in_bounds` still green.

## notes

Scope: sidebar/drawer, editor page geometry, status bubble, slash menu, presence.
Files: `apps/notes/src/{app.rs, sidebar.rs}`, `apps/notes/tests/responsive_drawer.rs`.

| id | surface | defect | viewport/theme | file:line | before → after | status |
|---|---|---|---|---|---|---|
| N1 | overlay drawer | `DRAWER_WIDTH × text_scale` (528 px) wider than a 480 px window: "+" button and selection pill run off-screen; `SidebarDrawer` clamps its own rect but the taffy node did not | 480×900 dark 2× | `app.rs:2392-2394,2816` | `notes/before/drawer-narrow-2x.png` → `after/drawer-narrow-2x.png` | fixed — one clamped width (window − s12 reveal band) feeds both the drawer and the sidebar node |
| N2 | slash menu | popup keeps 1× box (200×28-px rows) while items render ~4× (theme token fed to the renderer = double scale) | 800×900 2× | `ui/src/widget/slash_menu.rs:68-76,523-564` | `before/slash-2x.png` | open — needs F4 (also F2: `BlockEditor` owns the anchor) |
| N3 | block drag handles | 1× glyphs at 2× | 2× | shared `BlockEditor` gutter | `before/large-text.png` | open — needs F2 |
| N4 | sidebar header | "Pages" at x=12, rows at 28, "+" chip hand-drawn (audit item) | 1200×800 | `sidebar.rs` header | `before/status.png` | open — cosmetic; audit already lists the chip → `Button` |
| N5 | 1× composition | title, tag chips, body and code block share x=272; drawer scrim, status bubble, empty state all clean | all | — | `before/dark/dev.qos.notes-desktop.png`, `before/narrow.png`, `before/first_note.png`, `before/drawer-narrow.png` | no defect |

Tests: `responsive_drawer::scaled_drawer_stays_inside_a_narrow_window` (new; `settle_layout`
gained a themed variant). Accessor `NotesApp::sidebar_rect_for_test` /
`Sidebar::rect_for_test` added for it.

## contacts

Scope: list rows, toolbar, Sort/Group pills, detail header, detail fields, push breakpoint.
Files: `apps/contacts/src/{app.rs, responsive.rs}`.

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| K1 | list rows | `ROW_H = 62` fixed at nine sites; 2× name and subtitle collide (plan task 7 / corrected-review-1 P1) | 800×900 2× | `app.rs:87,1476-1485,2027-2050,2785-2870` | `contacts/before/large-text.png` → `after/large-text.png` | fixed — `ContactRowMetrics` shared by row rects, scroll extent, ensure-visible and paint |
| K2 | list row insets | avatar/separator inset 14 vs search/filter fields 16 | 1920×1080 | `app.rs:2812-2816,2864` | `before/dark/dev.qos.contacts-desktop.png` → `after/dark/dev.qos.contacts-desktop.png` | fixed — `PAD` everywhere; star centred on the name line |
| K3 | push breakpoint | 760 px at every scale, so 2× keeps two cramped panes | 800×900 2× | `responsive.rs:27,71-79`, `app.rs:1956` | as K1 | fixed — `PushNav::layout_at/is_narrow_at` scale the breakpoint (never below 1×) |
| K4 | detail header | name/role/chips at fixed 88/36/26/28 offsets; 2× name overprints role, chip pills stay 20 px | 2× | `app.rs:3028-3130` | as K1 (detail reachable via push) | fixed — measured stacking, chips sized from their line and wrapping |
| K5 | detail fields | 78 px label column with a 64 px label cap; 2× labels run under values | 2× | `app.rs:4132-4155` | — | fixed — column from the widest label (`"mobile"`) + s4, label centred on the value's first line |
| K6 | toolbar | 48 px bar / 32 px buttons / 520 px compact rule fixed; 2× "7 people" clipped | 800×900 2× | `app.rs:86,1997-2012,2071,2667-2720` | as K1 | fixed — `toolbar_h()`, `toolbar_button_h()`, widths and compact rule scale together |
| K7 | Sort/Group pills | labels oversized and truncated ("So…") — shared `Chip::label_size_for_height` multiplies the pre-scaled `typography.scale.sm` by the renderer scale | 2× | `ui/src/widget/chip.rs:265-267` | `after/large-text.png` | open — needs F3 (pill height deliberately kept at `FILTER_H`; growing it enlarges the label further) |
| K8 | avatar initials | shared `Avatar::draw_at` initials double-scale inside the disc | 2× | `ui/src/widget/avatar.rs` | `after/large-text.png` | open — needs F3 |
| K9 | edit form | `FormPage`/`Field` not adopted (brief suggestion); form fields are already the shared `TextInput` | — | `app.rs draw_form*` | `before/edit.png` | open — composition migration, out of alignment scope |

Tests: `app::tests::row_and_toolbar_metrics_follow_the_text_scale` (new);
`responsive::tests` extended for `layout_at`/`is_narrow_at`.

## weather

Scope: city rows, detail header, NavShell breakpoint, metric tiles.
Files: `apps/weather/src/{core.rs, app.rs}`.

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| W1 | saved-place rows | `CHIP_H = 62` fixed at six sites; 2× name/condition/temperature collide (plan task 13) | 800×900 2× | `core.rs:57,909,1213-1237,1271,1554,1290-1380` | `weather/before/large-text.png` → `after/large-text.png` | fixed — `ChipMetrics` shared by extent, slots, reorder `ListMetrics`, hit test and paint; identity text elides before the temperature column |
| W2 | shell breakpoint | 700 px / 340 px overview at every scale → 2× keeps two panes | 800×900 2× | `app.rs:59,194-202` | as W1 (list now full-width) | fixed — shell rebuilt with scaled breakpoint/overview width on a scale change, active pane preserved |
| W3 | detail header | title/region/date/time at fixed 32/2/24 offsets, `body_top = hy + 74`; 2× title overprints the date column, region overprints the hero | 800×900 2× | `core.rs:2108-2190,1970` | `before/large-text.png` → `after/detail-2x.png` | fixed — measured stacking, title elides before the right column, header height returned as body top |
| W4 | hero, metric tiles, hourly strip, daily rows, AQ tiles | `HERO_H/TILE_H/HOURLY_CARD_H/DAY_ROW_H` fixed in the renderer-less `DetailBodyLayout::measure` | 2× | `core.rs:3595-3632` and the section painters | `after/detail-2x.png` (below the header) | open — app (plan task 13 remainder: the body layout needs the scale threaded like the chips) |
| W5 | metric tiles equal widths | suspected 5 px narrower last tile | 1920×1080 | `core.rs:2424-2430` | `before/dark/dev.qos.weather-desktop.png` | no defect — `(1520 − 4·13)/5 = 293.6` per tile; reading error |
| W6 | NavShell Back pill | label clipped inside the pushed page's Back affordance at 2× | 800×900 2× | shared `NavShell` | `after/detail-2x.png` | open — needs F1 |

Tests: `core::chip_metrics_tests` (2 new).

## maps

Scope: floating controls, directions panel, navigation card.
Files: `apps/maps/src/app.rs`.

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| M1 | map controls cluster | zoom/locate/traffic column at the top-right corner is covered by the Navigation card while a route is active (§1e insets collide) | 1200×800 | `app.rs:2320` (`set_viewport(self.rect)`), `map_controls.rs:40-63` | `maps/before/default.png` (controls hidden under the card) → `after/default.png` | fixed — `map_controls_viewport()` lowers the cluster beneath a visible, overlapping card; paint, hit-test and a11y rects follow |
| M2 | `vmid` | text centred on `size × 1.25` without the renderer scale | 2× | `app.rs:2334` and 19 call sites | `before/directions-2x.png` → `after/directions-2x.png` | fixed — `text_vmid(..., text_scale)` via `MapsApp::vmid` |
| M3 | directions panel rows | "Use my location" 30 px, mode control 40 px, time picker 36 px fixed; 2× labels overflow (corrected-review-1 P1) | 800×900 2× | `app.rs:3666,3728,3760,4030` | as M2 | fixed — each row = max(authored, measured line + pad) |
| M4 | route summary | duration/distance/Clear and the arrival clock at fixed offsets → 2× collision | 2× | `app.rs:3813-3850` | as M2 | fixed — measured line box, Clear hit rect sized to it |
| M5 | route step bands | 46 px fixed, distance at `+18` overprints the instruction at 2× (plan task 10) | 2× | `app.rs:3861-3910` | as M2 | fixed — band = max(46, instruction + distance lines + 12); distance on its own line; connector follows the badge |
| M6 | navigation card body | instruction/distance/rule/summary at fixed 47/70/98/114 | 2× | `app.rs:448-525` | as M2 | partly fixed — stack follows measured lines; the card's own height comes from the floating-surface placement and still clips the summary row at 2× |
| M7 | directions panel width | `PANEL_MAX_W = 384` at every scale → every row elides at 2× | 2× | `app.rs:68-69` | `after/directions-2x.png` | open — app (panel width should follow the scaled content like the weather overview) |
| M8 | overflow button inset | bottom inset 12 vs 16 elsewhere | 1200×800 | `app.rs` bottom-right overflow | `after/default.png` | open — cosmetic |
| M9 | committed reference PNGs | `apps/maps/shots/*.png` predate the current panel design (tab strip, palette) | — | `apps/maps/shots/` | compare `apps/maps/shots/default.png` with `after/default.png` | open — not regenerated (not a like-for-like baseline) |

Tests: `app::tests::map_controls_clear_a_visible_navigation_card`,
`app::tests::text_vmid_centres_the_scaled_line` (new).

## Framework items (Open — needs F*)

- F4 (+F2): `ui/src/widget/slash_menu.rs` — fixed 200×28 menu, label size from the pre-scaled
  typography token → ~4× glyphs at 2× (N2).
- F3: `ui/src/widget/chip.rs:265` `label_size_for_height` multiplies the pre-scaled
  `typography.scale.sm` by the renderer scale (K7); `Avatar::draw_at` initials likewise (K8).
- F2: `BlockEditor` gutter drag handles stay 1× at 2× (N3, C18).
- F1: `NavShell` Back affordance clips its label at 2× (W6); the floating-surface placement
  should size the maps navigation card from its measured content (M6).

## Tests run

All cargo commands used `RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1
RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."` plus
`CFLAGS_aarch64_apple_darwin=-fstack-protector-strong` (the repo's macOS flag set, replacing
the merged Linux-only `-z relro`/`noexecstack` target flags from the parent checkout) and
`--locked --no-default-features`.

| package | command | result |
|---|---|---|
| calendar | `cargo test -p calendar --lib` | 210 passed, 0 failed (202 at the base + 4 `metrics`, 2 `agenda`, 2 `app` new) |
| notes | `cargo test -p notes` (full) | lib 126 passed; `it` 122 passed, **10 failed**, 1 ignored — the 10 (`canonical_actions::*` ×6, `canonical_views::*` ×4) fail identically with all notes changes reverted (verified by patch/revert/re-apply), i.e. pre-existing at `087d5a62c` |
| notes | `cargo test -p notes --test it -- responsive_drawer` | 2 passed (incl. new 2× clamp test) |
| contacts | `cargo test -p contacts --lib` | 168 passed, 0 failed |
| weather | `cargo test -p weather --lib` | 84 passed, 0 failed |
| maps | `cargo test -p maps --lib` | 180 passed, 0 failed, 1 ignored (pre-existing `#[ignore = "hits the public OSM tile server"]` in `tiles.rs:1477`) |
| all five | `cargo check -p <pkg> --tests` | Finished for calendar, notes, contacts, weather, maps |
| calendar, contacts | `cargo clippy --no-deps -p … --lib` | clean apart from pre-existing `caldav.rs` dead-code warnings under `--no-default-features`; workspace clippy cannot run because `ui/src/webview/chromium` has deny-level lint errors at the base |

Exact final counts are in the branch's last verification run (see the final message).

## Limits

- Base correction: the worktree was first cut from `origin/main` (`24ee08c66`); all work was
  rebased onto `codex/macos-shell` `087d5a62c` before any commit, the wrong-base captures were
  discarded and every BEFORE capture was retaken from the correct base.
- macOS build environment: `.cargo/config.toml` at the base passes Linux-only linker flags for
  aarch64; they were overridden through `RUSTFLAGS`/`CFLAGS_aarch64_apple_darwin` in the
  environment only (no config edits). `crates/runtime/runtime-identity/src/backup.rs` at the
  corrected base already gates `renameat2` to Linux.
- The `code-review` skill's forked run terminated before its finder agents returned (subagent
  concurrency cap shared with 17 sibling workers); its findings were relayed by the coordinator
  and fixed (C4 at-least-one-chip, C11 toggle rect, K6 compact rule, C14 fit rule, agenda 1×
  whole-pixel heights).
- Weather hero/tiles/hourly/daily (W4), the maps panel width (M7) and navigation card height
  (M6) remain open: they need the text scale threaded into renderer-less layout code or the
  floating-surface placement, which is a larger change than this alignment pass.
- `--pseudo-locale` runs were not captured; elision paths were exercised through the 2× shots
  and the demo data's long strings instead.
