# A6 — calendar, notes, contacts: 1× polish (visible edges, nothing touches, one rhythm)

Branch `ux1x/a6-calendar-notes-contacts`, base `fd1f84391` (Unit 0 tip). Files edited: `apps/calendar/src/**`,
`apps/notes/src/**`, `apps/contacts/src/**` only. Evidence under `target/ux1x/a6/<app>/{before,after}/{dark,light}/`
(PNG per scenario + responsive matrix; `geometry/<scenario>.{inspection,targets,audit}.json` from `--headless-drive`).
Capture scripts: `target/ux1x/a6/capture.sh` (AFTER: every declared scenario, the matrix, geometry + lint),
`capture-calendar-legacy.sh` (BEFORE calendar through the legacy flags — the base had no `--scenario` list),
`geometry.sh`, `audit_detail.py` (lint failures joined with rects), `pair_table.py` (nearest-neighbour gaps).
Environment for every capture: `QOS_SHOTS_TEXT_SCALE=1.0`, `QOS_SHOTS_THEME=dark|light`, `QUICK_UI_VELLO_CPU=1`,
a scratch `HOME`/`XDG_*` profile.

This worker resumed the unit after the previous agent was killed mid-review; its worktree, edits and captures were
kept and finished, not restarted.


## What changed

**calendar** (`apps/calendar/src/{app,draw_util,main,event_popovers,mini_month,sidebar,timeline_interaction,topbar}.rs`)
- Every self-painted edge is derived: `draw_util::{panel, selected_row, row_fill, row_ground, selection_fill, hairline}`
  compose `ui::surface::{card, separator, edge_on, edge_outline, shadow}` and `ui::chrome::{panel_ground, panel_border}`.
  Month grid rules, timeline hour/column rules, the sidebar's right edge and the detail pane's left edge are
  `hairline` (one device-pixel row, `edge_on(ground)`); event chips, timeline blocks, field buttons, the status
  pill, the empty-state plate, the icon buttons and the metadata chips are `ui::surface::card`; event/date/time/
  calendar popovers, the task pickers and the modal editor card are Panel role (`shadow(e3)` + `panel_ground` +
  `panel_border`). No `border_standard`, `neutral[]`, `Color::WHITE` or literal black remains in a calendar draw path.
- Selection is a lift, never an accent wash: sidebar smart-list/project rows, both segmented view switchers, the
  calendar-selector popover rows (`selected_row` measured against the ground they sit on).
- Rhythm: stacked month chips keep `s1` (pitch `chip_h + s1`, was 3 px); the status pill is inset `s4`, padded `s3`,
  Retry sits `s2` beside it as a real `Button::paint`; segmented tracks are inset `s3`; popover rows keep `s1`.
- Timed month chips composite their wash onto the cell's *actual* fill (weekend / anchor lift), the count badge picks
  the tier furthest from the row it sits on (idle raised row → base pill; selected row → raised pill), so the badge
  body is visible on both.
- Capture surface: `.scenarios(&[..])` with 23 names (views, editor + every popover, task detail + three pickers,
  empty ×3, four transport states, collapsed sidebar, two narrow presentations); `--shots` goes through
  `ui::launch::shot_theme(args)` so `QOS_SHOTS_THEME=light` / `QOS_SHOTS_TEXT_SCALE` are honoured (proved by the light
  captures below); `--headless-drive --scenario` drives the same fixture. Malformed shot options are errors, never a
  silent fallback (documented in `main.rs`); `--date-popover` wins over `--calendar-popover` as before; the
  unreachable `--shots-narrow` alias is gone (the narrow states are the `narrow_drawer` / `narrow_editor` scenarios).
- Drawer over narrow content casts the theme's `e3` elevation, painted once.

**notes** (`apps/notes/src/{app,framework_history,main,main_panel,shortcuts_overlay,shots,sidebar}.rs`)
- Sidebar: selected page is a lifted pill (`selection_fill_on_raised` — the tier furthest from the raised sidebar,
  `edge_on` hairline, inner glow) inset `ROW_PILL_MARGIN` and half an `s1` top/bottom so neighbouring pills keep `s1`;
  nesting guides and the sidebar divider are `edge_on(raised)`; guides paint before the pill so a rail never crosses
  the chosen row. Narrow top bar: `chrome_hairline` under the bar; the open-drawer toggle is the same lift with its
  derived edge (not `raised` on `raised`).
- History page tree (`framework_history.rs`): a raised pane with a derived right edge, `TreeRowMetrics` (pane
  `sidebar_width`, docks from `compact_breakpoint`, rows `selector_height` or measured line + `s2`, pill inset `s2`
  / gap `s1`, labels centred on their line box, indent `s4`) replacing the 600/200/24/28/12/16-px literals.
- Tag chips under the title: one `TagChipBand` (title ↔ chips `s2` — title + explanation — which also clears the
  focused title's 2 px ring; chip = measured label line + `s2`, padded `s2`, gap `s2`; quiet chip = raised +
  `edge_on`) shared by painter and test; the kbd chips in the shortcuts overlay are one derived hairline step off the
  `ModalFrame` ground (`over(raised, edge_on(raised))`) with a crisp `card` edge, visible in light where `lift(0.72)`
  vanished.
- `shots.rs` takes the resolved `ui::launch::shot_theme(args)` instead of re-deriving `QOS_SHOTS_THEME` privately.

**contacts** (`apps/contacts/src/{app,export_dialog,framework_history,framework_history_tests}.rs`)
- Selected row is a lifted pill (`row_tier_on(base, active)` — one registered hairline step off the ground, derived
  edge, inner glow) inset `s2` / half `s1`, with the row separators dropped on *both* sides; idle separators are
  `ui::surface::separator(.., PAD, base)`; the favourite star sits on the row centre line; toolbar hairline is
  `chrome_hairline`; back capsule, favourite toggle (`over(base, accent, 0.18)` so the wash is opaque), remove and
  calendar buttons are `card`; the splitter is one derived step off base.
- Menus (`menu_panel`) are Panel role (`shadow(e3)`, `panel_ground`, `panel_border`); hovered/selected menu rows and
  export-dialog hover rows are `row_tier_on(panel_ground, ..)` because `selection_tier`'s lift lands within 0.01 of
  the dark and high-contrast Panel grounds.
- History preview: page gutter `s6`, avatar `s16`, avatar ↔ first field `s3` (was 20/64/76 px);
  `framework_history_tests.rs::history_preview_renders_on_the_token_rhythm_in_dark_and_light` renders both themes with
  `render_rgba_resilient`, asserts the gutter/gap pixels and writes `after/<theme>/history_preview.png`.

## Six-rule checklist

| Rule | Evidence |
|---|---|
| 1 Nothing touches | Pair tables below (geometry JSON + code-derived); `notes/sidebar.rs::neighbouring_selection_pills_keep_an_s1_gap`, `notes/framework_history.rs::neighbouring_selection_pills_keep_a_token_gap`, `contacts/app.rs::neighbouring_selection_pills_keep_a_token_gap`, `calendar/app.rs::month_metrics_hold_scaled_text_and_keep_the_one_x_design` (chip pitch `= chip_h + s1`), `notes/main_panel.rs::high_text_title_tags_and_first_block_have_separate_bands`; PNGs `calendar/after/*/month.png` (chips), `contacts/after/*/new.png` (pill vs neighbours), `notes/after/*/drawer.png` (pill, chips), `notes/after/*/title_edit.png` (ring ↔ chips). |
| 2 Visible derived edge / contrast step | `grep -c 'border_standard\|neutral\[\|Color::WHITE\|from_rgba8' apps/{calendar,notes,contacts}/src/*.rs` on draw paths: 0 new, see "declared tokens left" below; `notes/sidebar.rs::selection_fill_moves_off_the_raised_sidebar_on_every_preset`, `notes/framework_history.rs::tree_pane_edge_and_selection_register_on_every_preset`, `contacts/app.rs::panel_row_lift_registers_on_every_preset` (dark, light, high-contrast, Δ ≥ `HAIRLINE_MIN_DELTA`); every AFTER PNG opened (lists below) — reviewer-sampled greys in the notes/contacts sections. |
| 3 One rhythm | calendar: chips `s1`, popover rows `s1`, status pill `s4`/`s3`/`s2`, tracks `s3`; notes: `TreeRowMetrics`, `TagChipBand`, pill `s1`/margin; contacts: pill `s2`/`s1`, separators `PAD`, history preview `s6`/`s16`/`s3`. Two rhythm defects remain in calendar and are listed under Open (task detail NOTES band, mini-month dot). |
| 4 Centred content | notes labels centred on measured line boxes (`labels_sit_on_their_measured_line_centred_in_the_row`), tag chip text centred in its band; contacts avatar, name/subtitle block and star on the row centre; calendar badge/status text centred on measured height (`center_text`, `hmeas`). |
| 5 No authored-pixel exemption | Every fixed offset this unit kept is a token expression or a measured value; the remaining literals in these files are pre-existing (`f1_spacing` baseline unchanged or lower — see ratchets). |
| 6 Visual evidence | 27 + 23 + 15 scenarios per theme (plus `narrow_*`, `drawer_phone`, `edit_phone`, `history_preview`), before and after, dark and light, every PNG opened with the Read tool (one line each below); geometry JSON + lint for 8 calendar, 3 notes, 6 contacts states per theme. |

## Surfaces (before → after, dark + light)

| App | Surface | Before PNG | After PNG | Defects fixed (rule) |
|---|---|---|---|---|
| calendar | month grid, cells, chips | `before/*/month.png` | `after/*/month.png`, `collapsed_sidebar.png`, `empty.png` | grid rules invisible in dark (2); chips edgeless, 3 px apart (1, 2); weekend/anchor timed chips composited onto base (2) |
| calendar | week / day timelines | `before/*/{week,day,empty_week}.png` | `after/*/{week,day,empty_week}.png` | hour/column rules half-alpha `border_standard` (2); event blocks and all-day strips edgeless tints (2); `Color::WHITE` strip text (2) |
| calendar | agenda | `before/*/agenda.png` | `after/*/{agenda,empty_agenda}.png` | sidebar/segment accent washes (2); agenda rows themselves are text lines (no surface) — unchanged |
| calendar | sidebar lists, badges, segmented switcher, mini-month rule | `before/*/month.png` | `after/*/month.png` | accent-wash selection (2/§9.4); badge body invisible on selected row (2); `Color::WHITE` today number (2); `border_standard` rule (2); segmented `accent[5]` fill + `border_standard` track (2) |
| calendar | top bar + segmented | any before | any after | `border_standard` bar rule → `chrome_hairline`; track → `card`, segment → lift (2) |
| calendar | event editor (modal card) | `before/*/{month,new_event}.png` | `after/*/{event_editor,new_event}.png` | card had no edge in dark, heavy dark ring in light (2); rules `border_standard` on Panel ground (2); field buttons/chips `neutral[2]` + `border_standard` (2) |
| calendar | date / time / calendar popovers | `before/*/{date_popover,calendar_popover}.png` | `after/*/{date_popover,time_popover,calendar_popover}.png` | `raised` + `border_standard` backing (2); calendar rows `lift(0.72)` wash touching (1, 2); time popover anchored under the date field (4) |
| calendar | task detail + pickers | `before/*/{task_detail,task_picker_*}.png` | `after/*/{task_detail,task_*_picker}.png` | picker panel `border_focus` ring instead of a Panel edge (2); card edge (2) |
| calendar | status pill + Retry | `before/*/{sync_error,sync_offline}.png` | `after/*/sync_{error,offline,syncing,conflict}.png` | `neutral[2]` pill, `neutral[9]` text, no edge (2); Retry hand-rolled `accent[4]` box 8 px from the pill (2, 3) |
| calendar | narrow drawer / narrow editor | `before/*/narrow_flap.png`, `phone.png` | `after/*/{narrow_drawer,narrow_editor}.png`, `dev.qos.calendar-phone.png` | drawer's literal black band → `e3` elevation, `hairline` edge (2) |
| notes | sidebar rows, divider, guides | `before/*/drawer.png` (+ every page scene) | `after/*/drawer.png` (+ every page scene) | accent wash selection touching neighbours (1, 2); `border_standard` divider/guides (2); guide through the pill (1) |
| notes | tag chips | `before/*/{drawer,recipes,title_edit}.png` | `after/*/{drawer,recipes,title_edit}.png` | `neutral[3]` fill, no edge, 4/22/6 px literals (2, 3, 5); ring ↔ chips 3 px (1) |
| notes | shortcuts overlay kbd chips | `before/*/shortcuts.png` | `after/*/shortcuts.png` | `lift(0.72)` fill invisible on the light frame (2) |
| notes | narrow top bar + drawer toggle | `before/*/dev.qos.notes-phone.png` | `after/*/dev.qos.notes-phone.png`, `drawer_phone.png` | `border_standard` bar rule (2); toggle accent wash / raised-on-raised (2) |
| notes | History page tree | — (History mode not capturable through `--shots`) | geometry asserted by the three `framework_history.rs` tests | raised pane without edge, 600/200/24/28/12/16 literals (2, 5) |
| contacts | list rows + selection | `before/*/{new,edit,editfocus,groups,context}.png`, matrix | `after/*/…` | accent wash + left bar touching neighbours, separator hairline on the wash edge (1, 2); separators nearly invisible in dark (2); star off the row centre (4) |
| contacts | toolbar, back capsule, splitter | `before/*/dev.qos.contacts-phone.png`, tablet | `after/*/edit_phone.png`, tablet | `border_standard` bar rule / capsule / splitter (2) |
| contacts | detail: favourite toggle, remove, calendar buttons | `before/*/{edit,new}.png` | `after/*/{edit,new}.png` | translucent wash + `border_standard` (2); border-only minus buttons (2) |
| contacts | menus (context, groups, export rows) | `before/*/{context,groups,export}.png` | `after/*/{context,groups,export}.png` | literal black halo, `raised` + `border_standard` (2); accent wash selected row, `Color::WHITE` hover text (2); hover lift invisible on Panel ground (2) |
| contacts | History preview | — | `after/*/history_preview.png` (test-rendered) | 20/64/76 px offsets (3, 5) |

## Pair tables

Geometry-derived (from `after/<theme>/geometry/*.inspection.json`; dark and light rects are identical — verified per
file). Only actionable controls carry semantic bounds, so calendar's tables are dominated by the two structures that
share edges by design: the month grid (`Open <day>` cells, hairline-separated) and the hand-rolled segmented view
switcher (one control, painted segments inset `s1/2 + 1` = 3 px each, so painted pills are 6 px apart while the hit
cells touch). Notes exposes the tree region and two buttons; contacts exposes a single window node (see Limits).

| surface (PNG) | element A | element B | axis | gap px before → after | relationship / step | OK |
|---|---|---|---|---|---|---|
| calendar month (`month.png`) | chip row n | chip row n+1 | y | 3 → 4 | stacked chips, `s1` | yes |
| calendar month | chip | cell edge | x | 6 → 6 | `MONTH_CHIP_INSET` (authored, unchanged) | yes |
| calendar month | today pill | first chip | y | 4 → 4 | `s1` | yes |
| calendar month | day cell | day cell | x/y | 0 → 0 | grid tiles, separated by the painted `hairline` | by design (lint: `touching_peers`) |
| calendar top bar | `Previous period` | `Next period` | x | 4 → 4 | siblings (authored) | yes |
| calendar top bar | `Next period` | `Go to today` | x | 10 → 10 | — | yes |
| calendar top bar | `Month view` | `New event` | x | 10 → 10 | — | yes |
| calendar top bar | segment hit cell | segment hit cell | x | 0 → 0 | one control; painted pills 6 px apart | by design (lint) |
| calendar sidebar | `Export calendar` | `Import calendar` | x | 8 → 8 | `s2` | yes |
| calendar sidebar | list row pill | list row pill | y | 4 → 4 | rows 34 high at 38 pitch, `s1` | yes |
| calendar sidebar | badge | row edge | x | 18 − w/2 | authored | yes |
| calendar status | pill | Retry | x | 8 → 8 | `s2` | yes |
| calendar status | pill | content edge | x/y | 20/16 → 16/16 | `s4` | yes |
| calendar popover | calendar row pill | row pill | y | 0 → 4 | `s1` | yes |
| calendar popover | picker backing | field | y | 6 → 6 | authored (unchanged) | yes |
| calendar agenda | task row hit band | next row hit band | y | 2 → 2 | 30 px bands at 32 px pitch; rows are text lines, no painted surface | unchanged (agenda.rs untouched) |
| notes sidebar (`drawer.png`) | selection pill | neighbour pill | y | 2 → 4 | `s1` | yes |
| notes sidebar | pill | pane side | x | 6 → 6 | `ROW_PILL_MARGIN` | yes |
| notes header | `New note` chip | `New note` chip (semantic dup) | x | 8 → 8 | `s2` | yes |
| notes header | `History` | `Pages` (tree region) | y | 0 → 0 | bar ↔ pane, separated by the bar hairline | region, see Limits |
| notes page | title | tag chips | y | 4 → 8 | title + explanation, `s2` (ring-safe: 6 px from a focused ring) | yes |
| notes page | chip | chip | x | 6 → 8 | related siblings, `s2` | yes |
| notes page | chips | first block | y | ≥ 8 → ≥ 11 | `TITLE_TO_EDITOR_GAP` slack (test asserts) | yes |
| notes shortcuts | kbd row | kbd row | y | 5 → 5 | `ROW_HEIGHT` (authored, unchanged) | yes |
| notes History tree | pill | pill | y | — → 4 | `s1` (test) | yes |
| contacts list (`new.png`) | selection pill | neighbour row content | y | 0 (wash touched) → 2 to the band edge, 4 to the next pill | `s1` | yes |
| contacts list | pill | pane side | x | 0 → 8 | `s2` | yes |
| contacts list | separator | pill | y | 0 (on the wash edge) → none on either side | replaced by the pill's edge | yes |
| contacts list | avatar | name block | x | 12 → 12 | `s3` | yes |
| contacts list | star | row centre | y | −8 → 0 | centred | yes |
| contacts menu | row pill | row pill | y | 0 → 4 | `s1` | yes |
| contacts history preview | edge | avatar | x/y | 20 → 24 | `s6` | yes |
| contacts history preview | avatar | first field | y | 12 → 12 | `s3` (test asserts the gap is unpainted) | yes |

## Geometry lint (`usability.geometry_audit`, every AFTER inspection, dark = light)

| app / state | controls | `touching_peers` | `semantic_bounds_overlap_unresolved` | `target_size_24` | classification |
|---|---|---|---|---|---|
| calendar month, empty, event_editor, task_detail, calendar_popover | 53–73 | 134 | 9 / 0 / 9 / 85 / 107 | 10–12 | touching: 131 month-grid cell pairs + 3 segmented pairs (structures that share edges by design, separated by painted rules / inset pills); overlaps: 9 event chips inside their day cell (flat hit list, chip wins the reverse scan), plus everything under the modal scrim's `Back to calendar` dismiss region in `task_detail` / `calendar_popover`; targets: 17 px month chips |
| calendar week, day, agenda | 17–30 | 8 / 3 / 3 | 0 / 0 / 5 | 3 / 2 / 1 | touching: segmented + week day headers; agenda overlaps: `Open task` row vs its `Mark … complete` checkbox (nested targets in a flat list); `Sync calendars` link 44×20 |
| notes live, first_note, menu | 4 | 2 | 0 | 0 | the Tree region's bounds share an edge with the History bar above and the `New note` chip beside it |
| contacts (6 states) | 1 | 0 | 0 | 0 | the app exposes one window node — the lint cannot observe rows/menus; rows are asserted in code instead |

`event_editor` does not settle on a fresh profile (first `--headless-drive` run: `CompositionDidNotSettle`, 0 controls;
the same command on the warmed profile settles with 63 controls, identical to `month`): the demo fixture's first sync
persists its model into the empty document inside the driver's rebuild budget. `capture.sh` re-drives once on the warm
profile and records the note in `geometry/event_editor.drive.log`.

## Visual review, one line per PNG

Every PNG under `target/ux1x/a6/{calendar,notes,contacts}/{before,after}/{dark,light}/` was opened with the Read tool
(the three per-app passes below were run as read-only review agents with the six rules; the notes and contacts passes
sampled pixel values, quoted as 0–255 greys or hex). Follow-up fixes after those passes — narrow calendar captures,
notes guide order / kbd fill / title-to-chips step, contacts star / splitter — were re-captured and re-opened by the
worker (`after/*/{narrow_drawer,narrow_editor}.png`, `notes/after/*/{drawer,shortcuts,title_edit,drawer_phone}.png`,
`contacts/after/*/{new,export,edit_phone}.png`, `contacts/after/light/dev.qos.contacts-tablet.png`).

### calendar — before/dark (18)
- before/dark/agenda.png: Agenda list with event editor overlay; defects vs rules 1–4: sidebar "Today" is an accent wash not a lifted pill (2); "Agenda" segment is accent fill (2); editor card has no border against the backdrop (2); agenda rows have no separators (2, mild).
- before/dark/calendar_popover.png: Month + editor + calendar dropdown; defects: month grid lines nearly invisible (2); month chips ("9:30 Team standup", "Company offsite") have no edge (2); stacked chips in cell 21 nearly touch (1, borderline); Month segment accent fill (2); sidebar accent wash (2); dropdown panel itself has a faint border (ok).
- before/dark/collapsed.png: Collapsed sidebar, month + editor; defects: grid lines nearly invisible (2); chips edgeless (2); editor card no border (2); Month segment accent fill (2).
- before/dark/date_popover.png: Date popover (blue focus ring) over editor; defects: popover edge is only the focus ring; same base defects (grid, segment, sidebar wash, editor no border) (2).
- before/dark/day.png: Day timeline + editor; defects: hour rules very faint (2); event blocks are flat tints with no edge (2); sidebar wash / segment accent fill (2); editor no border (2).
- before/dark/empty_week.png: Empty week, mini-calendar in sidebar; defects: hour rules and column separators faint (2); "No events yet" card is contrast-only, no edge (2, mild); Week segment accent fill (2); sidebar wash (2).
- before/dark/empty.png: Empty month; defects: grid lines faint (2); Month segment accent fill (2); sidebar wash (2); empty card contrast-only (2, mild).
- before/dark/month.png: Month + editor (Design review); defects: same as collapsed plus sidebar wash (2).
- before/dark/narrow_flap.png: 720x900 sidebar drawer over an empty black content area; defects: drawer has no visible edge against the content (2); content area is blank.
- before/dark/new_event.png: New event editor, title focused; defects: editor card no border (2); base defects (grid, segment, sidebar) (2).
- before/dark/phone.png: 390x844 phone editor with Back; defects: field and chip borders very faint (2, mild); otherwise none seen.
- before/dark/sync_error.png: Month + editor + "Sync failed" pill + Retry; defects: status pill has no edge (2); Retry sits under the editor's bottom-left corner, near-touching (1, borderline); base defects (2).
- before/dark/sync_offline.png: Same with "Offline" pill; defects: pill no edge (2); base defects (2).
- before/dark/task_detail.png: Task editor "Prepare quarterly report"; defects: card no border (2); ~80 px void between "Project" row and NOTES vs 8 px between dropdowns (3); base defects (2).
- before/dark/task_picker_due.png: Due-date picker with blue ring; defects: panel edge is only the focus ring, no shadow (2, mild); base defects (2).
- before/dark/task_picker_priority.png: Priority picker; defects: panel edge only the focus ring (2); selected row "P1 · High" is a full accent fill (2, same wash idiom as sidebar); base defects.
- before/dark/task_picker_project.png: Project picker; defects: as priority picker ("Work" accent fill) (2).
- before/dark/week.png: Week + editor; defects: hour rules faint (2); "Quarterly planning" block on Sat has no edge (2); base defects (2).

### calendar — before/light (18)
- before/light/agenda.png: Agenda + editor overlay; defects: sidebar "Today" pale-blue accent wash (2); "Agenda" segment accent fill (2); agenda rows no separators (2, mild); editor card has a very thick dark-grey ring (edge present but heavy, 3).
- before/light/calendar_popover.png: Month + editor + dropdown; defects: month chips are filled tints with no crisp edge (2, mild); Month segment accent fill (2); sidebar wash (2); grid lines visible (ok).
- before/light/collapsed.png: Collapsed month + editor; defects: heavy editor ring (3); segment accent fill (2); chips edgeless (2, mild).
- before/light/date_popover.png: Date popover blue ring, white body; defects: base defects only (segment, sidebar wash) (2).
- before/light/day.png: Day + editor; defects: event blocks are filled tints without edge (2, mild); hour rules faint but visible; segment/sidebar accent (2).
- before/light/empty_week.png: Empty week; defects: Week segment accent fill (2); sidebar wash (2); empty card white with shadow (ok).
- before/light/empty.png: Empty month; defects: segment accent fill (2); sidebar wash (2).
- before/light/month.png: Month + editor; defects: as calendar_popover (2).
- before/light/narrow_flap.png: 720x900 drawer with grey divider against blank white content; defects: none seen for the drawer edge; content blank.
- before/light/new_event.png: New event, focused title (grey fill + blue ring); defects: base defects (2).
- before/light/phone.png: Phone editor; defects: none seen (field borders faint but visible).
- before/light/sync_error.png: Month + editor + "Sync failed" pill + Retry; defects: pill no edge (2); Retry near-touching editor corner (1, borderline); base defects (2).
- before/light/sync_offline.png: "Offline" pill; defects: pill no edge (2); base defects (2).
- before/light/task_detail.png: Task editor; defects: void between Project row and NOTES (3); base defects (2).
- before/light/task_picker_due.png: Due picker blue ring; defects: base defects (2).
- before/light/task_picker_priority.png: Priority picker; defects: selected row full accent fill (2); base defects.
- before/light/task_picker_project.png: Project picker; defects: as priority (2).
- before/light/week.png: Week + editor; defects: Quarterly planning block edgeless tint (2, mild); base defects (2).

### calendar — after/dark (27)
- after/dark/agenda.png: Agenda list, no overlay; sidebar "Today" is a lifted pill with grey outline, badge "3" visible on selected and idle rows; "Agenda" segment lifted with edge; defects: agenda rows still have no separators (2, mild, pre-existing — rows are text lines). Before defects (sidebar wash, segment fill) gone.
- after/dark/calendar_popover.png: Month + editor + calendar dropdown; grid lines visible; chips have outlines and clear gaps; dropdown is a bordered, shadowed panel with lifted-pill selected row + check; editor card has a border; defects: none seen. Before defects gone.
- after/dark/collapsed_sidebar.png: Collapsed sidebar, full month (no overlay); grid lines visible; stacked chips (cell 24: Pay rent / standup / Gym) have edges and ~4 px gaps; weekend chips (19, 20) have visible bodies; Month segment lifted; defects: none seen. Before defects gone.
- after/dark/date_popover.png: Date popover over editor; panel has blue ring plus darker body and shadow; defects: popover edge treatment (blue focus ring painted by the toolkit DatePicker) differs from the task_due_picker's neutral border (3, minor, toolkit widget). Before defect (edge) gone.
- after/dark/day.png: Day timeline, no overlay; hour rules visible; all four event blocks have outlines; now-line visible; Day segment lifted; defects: none seen. Before defects gone.
- after/dark/dev.qos.calendar-desktop.png: 1920x1080 month + centred editor, sidebar with Today tasks and mini-calendar with event dots; grid/chips/editor edges all present; defects: mini-calendar event dots sit tight under the date numbers (1, borderline). AFTER-only.
- after/dark/dev.qos.calendar-laptop.png: 1366x768 month + editor; edges present, chips outlined; defects: none seen. AFTER-only.
- after/dark/dev.qos.calendar-phone.png: 390x844 phone editor; identical layout to before/phone; defects: field/chip borders faint (2, mild, derived `edge_on(raised)` at the policy's alpha).
- after/dark/dev.qos.calendar-tablet.png: 768x1024 month + modal editor with hamburger; truncated chips outlined; editor bordered; defects: none seen. AFTER-only.
- after/dark/empty_agenda.png: Empty agenda "Nothing scheduled" centred text; sidebar pill lifted; Agenda segment lifted; defects: none seen. AFTER-only.
- after/dark/empty_week.png: Empty week; hour rules and column lines clearly visible; Week segment lifted; defects: "No events yet" card is contrast-only with a subtle derived edge (2, mild). Grid-line defect gone.
- after/dark/empty.png: Empty month; grid lines visible; Month segment lifted; defects: empty card as above (2, mild). Grid/segment/sidebar defects gone.
- after/dark/event_editor.png: Month + Design review editor; editor card bordered; chips in editor outlined; defects: none seen. AFTER-only (matches before/month overlay; its defects gone).
- after/dark/month.png: Full month, no overlay; grid, chip edges, gaps, weekend chip bodies, lifted segment/sidebar all present; defects: none seen. Before defects gone.
- after/dark/narrow_drawer.png (re-captured at 720x900 after the first pass found it rendered wide): drawer with lifted "Today" pill, badges, calendars list, Today task, `e3` elevation and derived right edge over the (blank, as before) content; defects: content behind the drawer is blank as in before/narrow_flap (pre-existing).
- after/dark/narrow_editor.png (re-captured at 720x900): modal editor card over the month, Panel border + shadow, fields and chips edged, NOTES rule; defects: none seen.
- after/dark/new_event.png: New event editor, title focused; card bordered; defects: none seen. Before defects gone.
- after/dark/sync_conflict.png: Month + "1 change also changed on the server" pill; pill has a visible edge; clear of the row-6 chips; defects: none seen. AFTER-only.
- after/dark/sync_error.png: Month + "Sync failed" pill + Retry; pill bordered, Retry is a distinct blue button with ~8 px gap; defects: none seen. Before defects (pill edge, near-touch) gone.
- after/dark/sync_offline.png: "Offline" pill with edge; defects: none seen. Before defect gone.
- after/dark/sync_syncing.png: "Syncing calendars..." pill with edge; defects: none seen. AFTER-only.
- after/dark/task_detail.png: Task editor; card bordered; dropdowns outlined; defects: ~80 px void between Project row and NOTES vs 8 px sibling gaps (3, pre-existing — see Open). Border defect gone.
- after/dark/task_due_picker.png: Due picker as a grey-bordered, shadowed panel; "Clear due date" outlined; defects: a sliver of the anchor "Project" row's top border peeks above the panel (1, cosmetic, pre-existing). Before ring-only edge gone.
- after/dark/task_priority_picker.png: Priority picker panel bordered + shadow; defects: selected "P1 · High" remains a full accent fill inside the toolkit Dropdown list (2/3, toolkit widget); anchor sliver (1, cosmetic).
- after/dark/task_project_picker.png: Project picker; defects: as priority picker ("Work" accent fill, toolkit Dropdown) (2/3).
- after/dark/time_popover.png: Time popover with hour/minute steppers, bordered + shadow, hour field focused, anchored under the start-time field; defects: none seen. AFTER-only.
- after/dark/week.png: Week, no overlay; hour rules and column lines visible; all blocks outlined, incl. "Quarterly planning" on tinted Sat; defects: none seen. Before defects gone.

### calendar — after/light (27)
- after/light/agenda.png: Agenda list; sidebar lifted pill with outline, badges visible; Agenda segment lifted white; defects: agenda rows no separators (2, mild, pre-existing). Before wash/fill gone.
- after/light/calendar_popover.png: Month + editor + dropdown; chips now have a darker edge; dropdown panel bordered + shadow with lifted selected row; editor border neutral (thinner than before's heavy ring); defects: none seen.
- after/light/collapsed_sidebar.png: Collapsed full month; grid lines visible; chips edged with gaps; weekend chips visible; Month segment lifted; defects: none seen.
- after/light/date_popover.png: Date popover blue ring, white body; defects: edge treatment differs from task_due_picker's grey border (3, minor, toolkit DatePicker).
- after/light/day.png: Day timeline; blocks tinted with borders; rules visible; defects: none seen.
- after/light/dev.qos.calendar-desktop.png: 1920x1080 month + editor + mini-calendar with dots; defects: mini-calendar dots tight against the date glyphs (1, borderline). AFTER-only.
- after/light/dev.qos.calendar-laptop.png: 1366x768 month + editor; defects: none seen. AFTER-only.
- after/light/dev.qos.calendar-phone.png: Phone editor, same as before/phone light; defects: none seen.
- after/light/dev.qos.calendar-tablet.png: 768x1024 month + modal; defects: none seen. AFTER-only.
- after/light/empty_agenda.png: Empty agenda centred text; defects: none seen. AFTER-only.
- after/light/empty_week.png: Empty week; grid visible; card white with derived edge; Week segment lifted; defects: none seen.
- after/light/empty.png: Empty month; defects: none seen.
- after/light/event_editor.png: Month + editor; card bordered (neutral, not heavy); defects: none seen.
- after/light/month.png: Full month; chips edged, gaps, weekend bodies, lifted segment/sidebar; defects: none seen.
- after/light/narrow_drawer.png (re-captured at 720x900): drawer with grey divider and elevation over blank white content; lifted "Today" pill; defects: blank content as before (pre-existing).
- after/light/narrow_editor.png (re-captured at 720x900): modal editor card over the month, neutral border + shadow, edged fields/chips; defects: none seen.
- after/light/new_event.png: New event, focused title; defects: none seen.
- after/light/sync_conflict.png: Conflict pill with edge; defects: none seen. AFTER-only.
- after/light/sync_error.png: "Sync failed" pill bordered + distinct blue Retry with gap; defects: none seen. Before defects gone.
- after/light/sync_offline.png: "Offline" pill with edge; defects: none seen.
- after/light/sync_syncing.png: "Syncing calendars..." pill with edge; defects: none seen. AFTER-only.
- after/light/task_detail.png: Task editor; defects: Project-to-NOTES void (3, pre-existing — see Open).
- after/light/task_due_picker.png: Due picker grey-bordered panel + shadow; defects: anchor-row sliver above panel (1, cosmetic, pre-existing).
- after/light/task_priority_picker.png: Priority picker; defects: "P1 · High" full accent fill (toolkit Dropdown) (2/3).
- after/light/task_project_picker.png: Project picker; defects: "Work" accent fill (toolkit Dropdown) (2/3).
- after/light/time_popover.png: Time popover bordered + shadow; defects: none seen. AFTER-only.
- after/light/week.png: Week; rules and blocks all edged, Sat block visible; defects: none seen.

Light-vs-dark (calendar): light was largely acceptable before (grid lines visible, chips readable as tints, a heavy editor ring); the batch mostly repaired dark, and after both themes share one treatment (the light editor ring went from a thick dark-grey ring to the thin derived Panel border). No light regressions seen. Scenario drift: the before set had the editor overlaid on agenda/day/week/month/collapsed (legacy flags); the after set captures the raw views and the editor as its own `event_editor` scenario.


### notes — before/dark (23)
- before/dark/first_note.png: empty Pages sidebar + centred empty state (icon disc, title, subtitle, New note); defects vs rules 1–4: rule 2 sidebar/page divider is a 1-px `2b2c33` line on `1f2025`/`16171b` — barely a contrast step.
- before/dark/drawer.png: wide layout, Launch checklist page; defects: rule 2 selected row is a translucent blue wash (`2b3950`) with no edge; rule 2 tag chips are filled grey (`313234`) with no edge; rule 2 nesting guide (`2b3241`) nearly invisible inside the wash; divider faint as above.
- before/dark/drag.png: sidebar drag in progress, ghost "Launch checklist" row with blue outline; defects: rule 1 ghost top edge sits 1–2 px under the Inbox glyphs (transient); rule 3 ghost spans the full sidebar width while rows are inset; chips as drawer.
- before/dark/block_drag.png: blue drop indicator under "Finalise the onboarding copy"; defects: wash and chips as drawer.
- before/dark/menu.png: context menu over the Projects row (New child/Rename/Duplicate/Delete); defects: wash and chips as drawer; menu itself (toolkit widget) has a proper edge — none.
- before/dark/rename.png: inline rename of Projects with caret; defects: rule 2 rename editor indistinguishable from the wash (no input edge); chips.
- before/dark/rename_commit.png: renamed "Projects v2" row rendered detached near y≈400 with a full-width blue outline, original slot empty, nesting guide orphaned; defects: rule 1/3 displaced row breaks list rhythm and touches both sidebar edges.
- before/dark/slash.png: slash palette popover (filter input, block list, Paragraph highlighted); defects: popover fine (toolkit widget); wash and chips as drawer.
- before/dark/slash_applied.png: new Untitled page selected, body "/Overview"; defects: wash and chips.
- before/dark/heading.png: Untitled page, "Roadmap" H1 + "Ship by Q3", + button focus ring; defects: wash and chips.
- before/dark/title_edit.png: full-width blue-outlined title input "Launch plan"; defects: rule 1 focus-ring bottom (y≈76) to chip top (y=80) ≈3 px; wash and chips.
- before/dark/recipes.png: Recipes page with bullets and numbered list; defects: wash and chips.
- before/dark/formula.png: three formula cards (TeX/Typst/Typst cases) with Source/Visual toggles; defects: rule 1 status pill overlaps the third card's bottom-right; rule 2 segmented control has no outer edge, unselected segment has no surface; rule 2 TeX/Typst label is a barely-visible inset; wash and chips.
- before/dark/status.png: standard page, "Saved" pill; defects: wash and chips.
- before/dark/saving.png: same, "Saving..." pill; defects: wash and chips.
- before/dark/save_error.png: same, "Save failed" pill in neutral colour; defects: wash and chips.
- before/dark/presence.png: Alice selection + cursor labels, Bob viewport bar at the right edge, Inbox row tinted orange; defects: rule 1 "Alice" label covers the end of "Cut the release branch"; rule 1 Bob bar overlaps the code block's right edge; wash and chips.
- before/dark/shortcuts.png: Keyboard shortcuts modal, 4 groups; defects: rule 2 kbd chips filled `2f3033` on `1f2025` with no edge; rule 3 large empty band at modal bottom (minor).
- before/dark/tag_palette.png: tag palette alone on a blank dark screen; defects: none seen.
- before/dark/dev.qos.notes-phone.png: 390-wide, top bar with hamburger + title, drawer closed; defects: rule 2 top-bar hairline `2b2c33` barely visible; chips no edge; menu toggle is a bare glyph.
- before/dark/dev.qos.notes-tablet.png: 768x1024 wide layout; defects: wash and chips.
- before/dark/dev.qos.notes-laptop.png: 1366x768; defects: wash and chips.
- before/dark/dev.qos.notes-desktop.png: 1920x1080; defects: wash and chips.

### notes — before/light (23)
- before/light/first_note.png: empty state; defects: none seen (divider `dededb` visible).
- before/light/drawer.png: wide layout; defects: rule 2 selected row is a light-blue wash (`d1dff6`) with no edge; chips filled `d0d0ce` — contrast step ok, no edge; guide `d8dfe8` invisible inside the wash.
- before/light/drag.png: drag ghost; defects: rule 1 ghost touches Inbox label; rule 3 full-width ghost (as dark).
- before/light/block_drag.png: drop indicator; defects: wash.
- before/light/menu.png: context menu; defects: wash; menu (toolkit) fine.
- before/light/rename.png: inline rename; defects: rule 2 no editor edge.
- before/light/rename_commit.png: displaced "Projects v2" row at y≈400; defects: rule 1/3 as dark.
- before/light/slash.png: slash palette; defects: wash.
- before/light/slash_applied.png: Untitled selected; defects: wash.
- before/light/heading.png: Roadmap H1; defects: wash.
- before/light/title_edit.png: title input; defects: rule 1 ≈3 px to chips; wash.
- before/light/recipes.png: Recipes; defects: wash.
- before/light/formula.png: formula cards; defects: rule 1 status pill overlaps third card; rule 2 segmented control no outer edge; wash.
- before/light/status.png, saving.png, save_error.png: standard page with the three pill states; defects: wash.
- before/light/presence.png: presence overlays; defects: rule 1 Alice label over text, Bob bar over code block; wash.
- before/light/shortcuts.png: shortcuts modal; defects: rule 2 kbd chips have effectively no surface at all (`fcfcfb` on `ffffff`, no edge) — worse than dark.
- before/light/tag_palette.png: palette on white; defects: none seen.
- before/light/dev.qos.notes-phone.png: narrow layout, drawer closed; defects: rule 2 top bar hairline `dededb` present but faint; chips filled.
- before/light/dev.qos.notes-tablet.png, -laptop.png, -desktop.png: wide layouts; defects: wash.

### notes — after/dark (23 + drawer_phone)
- after/dark/first_note.png: empty state; defects: none seen. Divider now `36373b` (brighter than before's `2b2c33`).
- after/dark/drawer.png: wide layout; selected row is an outlined pill (border `616164`, fill `393a3d`, 26 px tall, inset 6 px, 4–5 px clear of neighbours); chips outlined (`36373b`), equal height, well clear of title and first block; guide `434447` visible; re-captured after the guide-order fix: the rail no longer runs through the pill. Before wash/chip/guide/divider defects gone.
- after/dark/drag.png: drag ghost; defects: rule 1 ghost still 1–2 px under Inbox glyphs, full-width — pre-existing (toolkit `DragController` ghost), see Open.
- after/dark/block_drag.png: drop indicator; defects: none seen. Before defects gone.
- after/dark/menu.png: Projects row as outlined pill with context menu; defects: none seen. Before gone.
- after/dark/rename.png: rename editor now sits in the outlined pill; defects: none seen. Before gone.
- after/dark/rename_commit.png: displaced full-width "Projects v2" row at y≈400, empty slot above; defects: rule 1/3 — pre-existing NoteTreeView behaviour, see Open.
- after/dark/slash.png: slash palette over outlined pill / outlined chips; defects: none seen. Before gone.
- after/dark/slash_applied.png: Untitled as outlined pill; defects: none seen. Before gone.
- after/dark/heading.png: Roadmap H1; defects: none seen. Before gone.
- after/dark/title_edit.png (re-captured after the `s2` step): focused title ring to chip top now ≈6 px, rest gap 8 px; defects: none seen. Before 3 px gone.
- after/dark/recipes.png: Recipes with outlined pill and chips; defects: none seen. Before gone.
- after/dark/formula.png: formula cards; defects: rule 1 status pill still overlaps third card; rule 2 Source/Visual segmented control no outer edge (toolkit Segmented), TeX label faint inset (toolkit formula block) — pre-existing, see Open.
- after/dark/status.png, saving.png, save_error.png: standard page; defects: none seen (code block `1f2025` on `16171b` with only the blue bar as edge — toolkit block editor, see Open).
- after/dark/presence.png: presence overlays; defects: rule 1 "Alice" label (`ec4899`) covers "h" of "branch"; Bob bar over code block — pre-existing toolkit presence overlay, see Open.
- after/dark/shortcuts.png (re-captured after the kbd fill fix): kbd chips are a clearly lighter step off the frame with a crisp edge, 5 px between rows; defects: none seen. Before kbd defect gone.
- after/dark/tag_palette.png: unchanged palette; defects: none seen.
- after/dark/dev.qos.notes-phone.png: narrow layout, drawer closed; top-bar hairline now `36373b`; chips outlined; defects: none seen.
- after/dark/drawer_phone.png (AFTER-only, 390x844, drawer open): drawer over dimmed content with the open-state toggle as a lifted square with a derived edge at the top-left; drawer edge visible; defects: none seen.
- after/dark/dev.qos.notes-tablet.png, -laptop.png, -desktop.png: wide layouts with outlined pill/chips; defects: none seen. Before gone.

### notes — after/light (23 + drawer_phone)
- after/light/first_note.png: empty state; defects: none seen. Divider is `e5e5e5` (`edge_on(raised)` — the policy's black at alpha 26 over white; fainter than the declared `dededb`, ≥ HAIRLINE_MIN_DELTA).
- after/light/drawer.png: pill outlined (border `c2c2c1`, fill `f3f3f1` on white), 4–5 px from neighbours; chips white with `e5e5e5` outline, equal height, even gap; guide `e7e7e5` present and, after the order fix, not through the pill; defects: rule 2 borderline — quiet chips (raised + `edge_on`) are lower contrast than the old `d0d0ce` fill; policy outcome, noted for the coordinator.
- after/light/drag.png: drag ghost; defects: rule 1 ghost touches Inbox label — pre-existing (toolkit).
- after/light/block_drag.png, menu.png, rename.png, slash.png, slash_applied.png, heading.png, recipes.png, status.png, saving.png, save_error.png: same scenes as dark with outlined pill and chips; defects: chip contrast note as drawer; otherwise none seen. Before wash gone.
- after/light/rename_commit.png: displaced "Projects v2" row; defects: rule 1/3 — pre-existing, see Open.
- after/light/title_edit.png (re-captured): ring ↔ chips ≈6 px; defects: none seen.
- after/light/formula.png: defects: rule 1 status pill over third card; rule 2 segmented control (`d5e2f7` selected, unselected text-only, no outer edge) — toolkit, pre-existing.
- after/light/presence.png: defects: rule 1 Alice label over text, Bob bar over code block — toolkit, pre-existing.
- after/light/shortcuts.png (re-captured): kbd chips are a visible grey step (`over(raised, edge_on(raised))`) with a crisp edge on the white frame; before they had no surface at all. Defects: none seen.
- after/light/tag_palette.png: unchanged; defects: none seen.
- after/light/dev.qos.notes-phone.png: narrow layout, drawer closed; top-bar hairline `e5e5e5`; chips outlined; defects: rule 2 borderline hairline (policy outcome).
- after/light/drawer_phone.png (AFTER-only): drawer open over dimmed content, toggle a lifted light-grey square with a derived edge; defects: none seen.
- after/light/dev.qos.notes-tablet.png, -laptop.png, -desktop.png: outlined pill and chips; defects: chip contrast note; otherwise none seen.

Light-vs-dark (notes): dark improved on every targeted item (divider/top-bar hairline `2b2c33` → `36373b`, pill `616164` border, chips and kbd chips outlined, guide `434447`). Light gained the pill edge and a visible guide; three light surfaces are legible but lower-contrast than before because the derived edge (`e5e5e5`) replaces the declared `dededb` and a quiet chip is the raised tier with an edge rather than a solid grey fill — the policy's outcome, recorded for the coordinator rather than worked around. Toolkit widgets (context menu, slash palette, tag palette, title input, checkboxes, status pill, empty state) are pixel-identical before/after in both themes.


### contacts — before/dark (14)
- before/dark/new.png: split view, empty "New Contact" form, Ada selected in list; defects vs rules 1–4: R1 selected row is a full-bleed accent wash touching the rows above/below, with the idle-row separator hairline sitting directly on the wash's top edge (y≈401 vs wash 402) and bottom edge (464); R2 idle-row separators nearly invisible (5 levels above bg), toolbar hairline faint (37 vs toolbar 31), minus buttons have transparent fill with an ~11-level border, favourite toggle border ~8 levels above fill, Import/Export/Cancel/"+ Add" buttons border only ~6 levels above fill (toolkit widget); R4 star sits on the name line (~8 px above row centre); avatar and text block centred.
- before/dark/edit.png: same layout, Ada's data filled, "Favorited" toggle tinted; defects: same as new.png (R1 wash touching, R2 faint separators/minus/toolbar hairline, R4 star).
- before/dark/editfocus.png: as edit.png with First-name focused (blue ring, toolkit widget); defects: same as edit.png.
- before/dark/export.png: "Export contacts" modal on black scrim, 7 checked names, Select all/none pills, Cancel/Export 3.0/Export 4.0; defects: R2 panel edge 37 on fill 31 and Cancel/Export 3.0 borders ~6 levels above fill (toolkit widget); Select all/none pills only ~11 levels above panel, no border.
- before/dark/unsaved.png: "Save changes?" modal on scrim, Cancel focused, Discard, Save; defects: R2 panel edge and Cancel/Discard borders faint (toolkit widget).
- before/dark/groups.png: Group dropdown open under "Group: All", "All" selected with focus ring; detail view of Ada; defects: R2 menu panel fill only 9 levels above list bg with 1 px border (41) and a hard 1–2 px shadow; selected row is an accent wash inside the ring, not a lift; list R1/R2/R4 as new.png.
- before/dark/context.png: context menu on Tim's row (Edit focused, Remove from favorites, Export, Delete in red); defects: same faint panel as groups.png; list defects as new.png.
- before/dark/confirm.png: "Delete contact?" dialog over undimmed detail pane, Cancel + red Delete; defects: R2 panel edge 37 on 31, Cancel border ~6 levels above fill (toolkit widget); list defects as new.png.
- before/dark/import-conflict.png: "Review imported contact" dialog, Keep existing / red Import as new; defects: same as confirm.png.
- before/dark/empty.png: "No contacts" toolbar with Import, centred empty state + Create contact; defects: R2 Import button border faint (toolkit widget); otherwise none seen.
- before/dark/dev.qos.contacts-phone.png: 390-wide list only, icon toolbar (down/up arrows, +); defects: R1 wash touching and separator on wash edge, R2 faint separators, R4 star; no back capsule in view.
- before/dark/dev.qos.contacts-tablet.png: 768-wide split, subtitles truncated with ellipsis; defects: same list defects; splitter visible (40–43 on 22).
- before/dark/dev.qos.contacts-laptop.png: 1366-wide split; defects: same list defects.
- before/dark/dev.qos.contacts-desktop.png: 1920-wide split; defects: same list defects.

### contacts — before/light (14)
- before/light/new.png: same as dark counterpart in light; defects: R1 wash touching with separator on its top edge (y 401/402); R2 idle separators nearly invisible (238 on 243), minus buttons transparent with ~11-level border; R4 star on name line; toolbar hairline present (239 between 255 and 243).
- before/light/edit.png: filled form; defects: same as new.png.
- before/light/editfocus.png: First-name focused, grey fill + blue ring (toolkit widget); defects: same as new.png.
- before/light/export.png: white modal on white backdrop, grey border (165); defects: none seen beyond pills relying on a 15-level fill step (toolkit widget).
- before/light/unsaved.png: white modal, grey border; defects: none seen.
- before/light/groups.png: white dropdown with dark-grey border (157), light shadow, "All" tinted with ring; defects: list R1/R2/R4 as new.png; menu itself fine.
- before/light/context.png: white context menu with dark-grey border; defects: list defects as new.png.
- before/light/confirm.png: white dialog over grey scrim, no panel border but strong step; defects: Cancel border 238 on white, faint (toolkit widget); list defects as new.png.
- before/light/import-conflict.png: as confirm.png; defects: same.
- before/light/empty.png: white toolbar with hairline, grey body, empty state; defects: none seen.
- before/light/dev.qos.contacts-phone.png: list only; defects: R1 wash touching + separator on edge, R2 faint separators, R4 star.
- before/light/dev.qos.contacts-tablet.png: split, truncated subtitles, darker splitter band (222–226 on 243); defects: same list defects.
- before/light/dev.qos.contacts-laptop.png: split; defects: same list defects.
- before/light/dev.qos.contacts-desktop.png: split; defects: same list defects.

### contacts — after/dark (15 + edit_phone)
- after/dark/new.png (re-captured after the star fix): Ada is an inset lifted pill (8 px from list edges, border 98/79 AA, fill 59 on bg 22), no accent bar; no separator on either side of the pill; idle separators 59 on 22, toolbar hairline 54, minus buttons filled (31) with 54 border, calendar border 54, favourite toggle border 48, splitter a derived step; star now on the row centre line with the avatar; remaining: Import/Export/Cancel/"+ Add" borders ~6 levels above fill (toolkit Button, pre-existing).
- after/dark/edit.png: filled form, pill selection; before defects gone as in new.png; remaining: toolkit button edges.
- after/dark/editfocus.png: focused First-name (toolkit ring); same as edit.png.
- after/dark/export.png: modal panel lighter (51) with clear border (125) and soft shadow; Cancel/Export 3.0 darker filled buttons; before panel-edge defect gone; remaining: Select all/none quiet chips ~10 levels above panel, no border (toolkit Chip quiet, pre-existing).
- after/dark/unsaved.png: panel 51 with border 125, Discard a dark filled button; before defects gone; remaining: none seen.
- after/dark/groups.png: dropdown is a Panel (fill 51 on 22, border 95–125, gradient shadow ~9 px), "All" a lifted row (fill 72, border 91) inside the focus ring; before menu defect gone; list before defects gone.
- after/dark/context.png: context menu with the same Panel treatment; before defect gone.
- after/dark/confirm.png: dialog identical to before (toolkit ModalDialog: panel 31 with 37 edge on the undimmed pane, Cancel border ~6 levels above fill) — pre-existing, toolkit, and now a different surface style from the restyled export/unsaved panels (R3); list defects gone.
- after/dark/import-conflict.png: identical to before; same as confirm.png (toolkit).
- after/dark/empty.png: identical to before apart from the stronger toolbar hairline (54); remaining: Import button edge faint (toolkit Button).
- after/dark/dev.qos.contacts-phone.png: list only, pill inset with border, separators 59, toolbar hairline 54; before R1/R2 gone; star centred.
- after/dark/edit_phone.png (AFTER-only, 390x844): narrow edit screen with the Back capsule (raised, derived edge) under the icon toolbar; favourite toggle, remove buttons, fields edged; defects: none seen.
- after/dark/dev.qos.contacts-tablet.png: split, pill inset, splitter a derived step on 22; before defects gone.
- after/dark/dev.qos.contacts-laptop.png: split, pill; before defects gone.
- after/dark/dev.qos.contacts-desktop.png: split, pill; before defects gone.
- after/dark/history_preview.png (AFTER-only, test-rendered 900x600): "SL" avatar 64 px at (24,24) with equal 24 px gutters, label column flush with the avatar, PHONE/EMAIL section headers; defects: none for R1/R2/R4; minor R3: address/birthday/website/notes rows do not carry the PHONE/EMAIL section rhythm (pre-existing field mapping).

### contacts — after/light (15 + edit_phone)
- after/light/new.png (re-captured): pill selection (fill 218, border 174/196 on 243), separators 218, toolbar hairline 229, minus buttons white with 229 border, calendar 229 border; no separator either side of the pill; star on the row centre; splitter a darker derived step (was a white band after the first pass, `lift(1.0)` = white on 243); nothing regressed.
- after/light/edit.png: filled form, pill; same as new.png.
- after/light/editfocus.png: focused First-name (toolkit); same as new.png.
- after/light/export.png: panel light grey (239) with border 153 and soft shadow, buttons white; Select all/none quiet chips 225 on 239, no border — toolkit Chip quiet (pre-existing weakness).
- after/light/unsaved.png: grey panel 239, border 153, white buttons; none seen.
- after/light/groups.png: dropdown panel fill 239 on page 243 (`panel_ground`), edge carried by border 153–188 and gradient shadow; "All" a darker lifted row (215, border 193) inside the ring; list before defects gone; panel body contrast weaker than the old white panel though rule 2 is met via the edge (policy outcome, noted).
- after/light/context.png: same Panel treatment as groups.png; same notes.
- after/light/confirm.png: identical to before (toolkit ModalDialog; Cancel border 238 on white); style mismatch with export/unsaved (R3, toolkit).
- after/light/import-conflict.png: identical to before; same as confirm.png.
- after/light/empty.png: identical to before except the stronger toolbar hairline (229); none seen.
- after/light/dev.qos.contacts-phone.png: list only, pill inset with border; before R1/R2 gone; star centred.
- after/light/edit_phone.png (AFTER-only): narrow edit screen, white Back capsule with derived edge, edged controls; defects: none seen.
- after/light/dev.qos.contacts-tablet.png (re-captured): split, pill; splitter a visible grey step between list and detail; none seen.
- after/light/dev.qos.contacts-laptop.png, -desktop.png: split, pill; same notes.
- after/light/history_preview.png (AFTER-only): white bg, avatar 64 px at (24,24), even gutters, label column flush with avatar; defects: none for R1/R2/R4; same minor R3 note as dark.

Light-vs-dark (contacts): light gained the same pill/separator/toolbar/minus/calendar fixes with equivalent contrast (218/229 on 243). Two light-only softenings are the policy's outcome: the menu panel fill is the Panel ground (239 vs page 243, edge + shadow carry it) and the splitter became a derived step (first pass: white `lift(1.0)` band, fixed to one hairline step off base). Toolkit dialogs (confirm/import-conflict) and secondary buttons are identical before/after in both themes.


## Ratchets (`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`, baselines untouched)

```
test design_token_audit::forbidden_patterns_match_the_shapes_they_name ... ok
test design_token_audit::declared_border_pattern_matches_the_shapes_it_names ... ok
test design_token_audit::product_apps_use_the_shared_responsive_launcher ... ok
test f1_spacing_token_audit::spacing_patterns_match_the_shapes_they_claim ... ok
test design_token_audit::no_hardcoded_style_literals_in_tree ... ok
test design_token_audit::theme_escape_hatches_do_not_grow ... ok
test design_token_audit::declared_borders_in_chrome_do_not_grow ... ok
test design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow ... FAILED
test f1_spacing_token_audit::spacing_literals_do_not_grow ... FAILED

apps/browser/src/browser_tabs.rs [literal_color]: 1 occurrence(s), baseline 0
apps/browser/src/browser_tabs.rs [white_or_black]: 1 occurrence(s), baseline 0
shell/src/shell/frame.rs [literal_color]: 1 occurrence(s), baseline 0
shell/src/shell/frame.rs [white_or_black]: 2 occurrence(s), baseline 0

apps/calendar/src/app.rs [pixel_const]: now 17, baseline still 18 — lower it to 17
test result: FAILED. 7 passed; 2 failed; 0 ignored; 0 measured; 345 filtered out
```

- **stale (this unit, expected):** `apps/calendar/src/app.rs pixel_const 18 → 17` — `MONTH_CHIP_GAP` (3 px) is
  gone; the chip pitch is `chip_h + spacing.s1`. For the coordinator's baseline refresh. No other owned file moved
  (notes `main_panel.rs` stays at 5: the tag chip label size lives on `TagChipBand`, not as a module constant).
- **grew (not this unit — red at base):** `apps/browser/src/browser_tabs.rs` and `shell/src/shell/frame.rs` are
  byte-identical to `fd1f84391` in this worktree (`git diff fd1f84391 -- <file>` is empty); the
  `app_shell_design_token_baseline.txt` entries for them are lower than the tree at the base itself.
- Declared tokens left in owned files after this unit (all on `surfaces.base` grounds or non-draw uses):
  calendar `mini_month.rs` `neutral[4]`/`neutral[8]` day-number *ink* (text colour, not an edge), `app.rs`
  `neutral[8]`/`neutral[9]`/`neutral[4]` text inks, `agenda.rs`/`timeline_viewport.rs` untouched (no edges painted
  there); no `border_standard`, `border_strong`, `*_white_*`, `Color::WHITE` or `from_rgba8` remains on a draw path
  in `apps/{calendar,notes,contacts}/src`.


## Tests run

All with the mandated inline environment and `--no-default-features --locked`, one heavy job at a time.

| Command | Result |
|---|---|
| `cargo build -p calendar -p notes -p contacts` | ok (10 pre-existing dead-code warnings in `calendar/{caldav,metrics,timeline_viewport}.rs` under `--no-default-features`, none in touched code) |
| `cargo test -p calendar --lib` | 215 passed, 0 failed |
| `cargo test -p calendar --bin calendar` | 5 passed (`every_declared_scenario_resolves_to_a_state`, `scenarios_cover_every_view_the_editor_and_the_empty_and_error_states`, `the_date_grid_wins_when_both_popover_flags_are_given`, `malformed_shot_options_are_errors_not_silent_fallbacks`, `wxh_parses_both_separators_and_rejects_garbage`) |
| `cargo test -p notes --lib` | 131 passed, 0 failed (incl. `sidebar::{neighbouring_selection_pills_keep_an_s1_gap, selection_fill_moves_off_the_raised_sidebar_on_every_preset}`, `framework_history::tree_geometry_tests::{neighbouring_selection_pills_keep_a_token_gap, labels_sit_on_their_measured_line_centred_in_the_row, tree_pane_edge_and_selection_register_on_every_preset}`, `main_panel::high_text_title_tags_and_first_block_have_separate_bands` updated to the production band) |
| `cargo test -p contacts --lib` | 184 passed, 0 failed (incl. `neighbouring_selection_pills_keep_a_token_gap`, `panel_row_lift_registers_on_every_preset`, `history_preview_renders_on_the_token_rhythm_in_dark_and_light` — writes `after/<theme>/history_preview.png`) |
| `cargo check -p calendar -p notes -p contacts --tests` | ok |
| `cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit` | 7 passed, 2 failed as pasted above (one stale line owned here; growth outside the unit, red at base) |

Not run: whole-crate `shell`/`ui` suites (out of scope; per the shell gating memory).


## Open — needs …

Routing per the coordinator's triage; nothing below is worked around locally beyond what is noted.

**Open — needs coordinator (closed on ux1x/integration)**
- route to `ui::chrome::selection_tier_on(theme, state, ground)`: calendar `draw_util.rs::{selection_fill, row_fill, row_ground, selected_row}` (callers: `app.rs` sidebar smart-list/project rows ~3590/3645, `sidebar.rs` segmented ~278, `topbar.rs` segmented ~258, `event_popovers.rs` calendar rows ~360); notes `sidebar.rs::selection_fill_on_raised` (callers `sidebar.rs::draw_one_row`, `app.rs` ~649 narrow drawer toggle, `framework_history.rs` ~280 tree pill); contacts `app.rs::row_tier_on` (list rows, `menu_item`, `export_dialog.rs` hover rows, splitter). The local rules stay until the merge; their registration on dark, light and high-contrast is asserted by the tests listed above.
- route to `ui::surface::panel(scene, t, rect, radius, theme)`: calendar `draw_util.rs::panel`, contacts `app.rs::menu_panel` — both cast `surface::shadow(&elevation.e3)` where the shell uses `panel_shadow`; recorded, not changed.
- `ui::surface::separator` now snaps to one device-pixel row: calendar `draw_util.rs::hairline` (horizontal branch) should become `separator` after merge; the `separator(.., y - 0.5, ..)` callers in `contacts/app.rs::draw_list_row`, `calendar/app.rs` (sidebar section rule) and `calendar/mini_month.rs` pass a centred `y` today and should pass the row after merge.
- route to `ui::surface::card_on(scene, t, rect, radius, fill, ground)`: calendar timed month chips (`app.rs::draw_month_cell_chips`, `over(cell_fill, col, 0.18)` + `card`) and timeline blocks (`timeline_interaction.rs`, `over(base, color, 0.22)` + `card`).
- `ui::launch::parse_viewport` is private: calendar `main.rs::parse_wxh` duplicates it for `--resize-from`; make it public (or a `LaunchArgs` accessor) and delete the copy.
- F1 (edges landed on integration): Button Neutral rest edge — contacts Import/Export/Cancel/"+ Add …" (`export.png`, `new.png`: border ~6 levels above fill in dark), calendar top-bar Today/New; TextInput/SearchField edge — contacts search + form fields, notes title input, calendar title field; quiet Chip edge — contacts export "Select all/none" and sort/group chips (`chip()` → `Chip::draw_at`), notes sidebar "+" chip; Dropdown list selected row is an accent fill — calendar `task_priority_dropdown`/`task_project_dropdown` (`task_priority_picker.png`, `task_project_picker.png`); DatePicker paints `border_focus` around itself over the Panel — calendar `date_popover.png` reads differently from `task_due_picker.png`; Segmented — notes formula Source/Visual (`formula.png`: no outer edge, unselected segment no surface).
- F2: ModalDialog — contacts `confirm.png`, `import-conflict.png` (panel edge 37 on 31 in dark, Cancel edge faint; a different surface style from the app's Panel-role menus); ContextMenu/SlashMenu/Popover (notes) — already derived, no defect seen; Panel ground in light is 239 on a 243 page (`groups.png`, `context.png`) — the edge and shadow carry it; noted for judgement.
- F3: block editor code block edge — notes dark `1f2025` on `16171b` with only the accent bar (`status.png`, `drawer.png`); formula block TeX/Typst label chip contrast (`formula.png`).
- F4: presence overlay — notes `presence.png`: "Alice" caret label covers the tail of the previous line, Bob's viewport bar overlaps the code block (rule 1); `DragController` ghost row 1–2 px under the Inbox label, full sidebar width (`drag.png`).

**Open — needs calendar follow-up (own files, not fixed in this pass)**
- Hand-rolled segmented view switcher (`topbar.rs`, `sidebar.rs`) → `ui::widget::Segmented`; its hit cells share edges (lint `touching_peers` ×3 per state) while the painted pills are 6 px apart.
- Semantics: the modal editor's scrim (`app.rs` ~4474 `Action::CloseDetail` over `content_rect`) leaves the covered controls in the a11y tree (85–107 `semantic_bounds_overlap_unresolved` in `task_detail`/`calendar_popover`); the flat `hits` list nests nothing (event chip inside its day cell ×9, `Mark task … complete` inside `Open task …` ×5 in agenda). A modal-aware, nested `accessibility()` is the fix.
- Task detail rhythm (rule 3, `task_detail.png` both themes): `NOTES_TOP_OFFSET` (296) reserves a fixed band for the subtasks list, so a task without subtasks shows ~80 px between the Project row and NOTES against 8 px between rows. Needs a measured layout: last subtask row (or Project row) + `s6` + `notes_header_h()`, the way the event form computes `f.notes_top`.
- Mini-month event dot (`mini_month.rs` ~193: `cell.y1 - 2.5`, r 1.6 in a 22 px cell) sits ~1.4 px under the 11 px date glyph (rule 1, `dev.qos.calendar-desktop.png`); needs a 24 px cell or the dot beside the number.
- Task picker popover starts 1 px below the anchor row's top edge (`app.rs` ~5142, `detail_rect.y0 + 176` literal): cosmetic sliver (`task_*_picker.png`).
- `--headless-drive --scenario event_editor` does not settle on a fresh profile (first sync persists the demo model within the driver's rebuild budget); `task_detail`/`new_event` settle. Geometry is taken from the warmed profile (see the lint section).
- Month chips are 17 px tall (`target_size_24` ×10–12 per state) — the authored `MONTH_CHIP_MIN_H`; the row's full height is the pointer target in practice (the cell hit wins outside the chip).
- Narrow drawer over blank content (`narrow_drawer.png`, both themes, same as before/`narrow_flap.png`): the month is not painted behind the open drawer at 720 px.
- 10 dead-code warnings under `--no-default-features` (`caldav.rs`, `metrics.rs::sanitized`, `timeline_viewport.rs::geometry`) — pre-existing.

**Open — needs notes follow-up**
- `rename_commit` scenario renders the renamed row detached near y≈400, full sidebar width, with the original slot empty (rule 1/3, pre-existing `NoteTreeView` behaviour after a rename commit — likely a real bug, both themes).
- Status pill (`Saved · …`) floats over the third formula card without elevation when the page is long (`formula.png`, rule 1).
- The page tree's semantic bounds are the pane rect: they share an edge with the History bar above and the `New note` chip beside them (lint `touching_peers` ×2 per state); exposing the rows area as the Tree's bounds would make the lint pass and is semantically truer.
- History mode (`framework_history.rs`) is not reachable through `--shots`; its geometry is covered by tests only.

**Open — needs contacts follow-up**
- `accessibility()` exposes one window node: rows, menus and dialogs are invisible to the geometry lint and to assistive tech (should expose `ListItem`s, menu items, dialog controls).
- Local `draw_checkbox`, `chip`, `add_button`, `draw_avatar`, `draw_star` painters mirror toolkit controls (pre-existing hand-rolling; the export list checkbox especially).
- History preview field mapping: address/birthday/website/notes rows lack the PHONE/EMAIL section rhythm and label casing is mixed (`history_preview.png`, minor rule 3).

**Pre-existing red at base (not this unit)**
- `design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow`: `apps/browser/src/browser_tabs.rs`, `shell/src/shell/frame.rs` (see Ratchets).


## Limits

- **Lint blind spots.** contacts exposes a single semantic node, so `geometry_audit` cannot observe its rows/menus; the
  row geometry is asserted in code (`neighbouring_selection_pills_keep_a_token_gap`, `history_preview_renders_…`) and
  by the PNGs. calendar's `touching_peers`/`semantic_bounds_overlap_unresolved` counts are by-design shared edges
  (grid, segmented) and flat-hit nesting (chip in cell, checkbox in row, everything under the modal scrim); no painted
  overlap was seen in any AFTER PNG beyond the items listed under Open. A check that cannot observe its subject is
  recorded as such here, not skipped silently.
- **`event_editor` geometry** comes from the second `--headless-drive` run on a warmed profile (the first, on a fresh
  profile, returns `CompositionDidNotSettle`); the capture script records the re-drive in the log.
- **Scenario drift (calendar).** BEFORE was captured through the legacy flags (the base had no scenarios), with the
  editor overlaid on every view; AFTER captures the raw views and the editor as its own scenario. Before→after pairs
  compare the underlying view.
- **History mode** (notes page tree, contacts preview) cannot be rendered through `--shots`; evidence is the
  test-rendered `contacts/after/<theme>/history_preview.png` and the notes geometry tests.
- **Light softenings reported by the reviewers** (notes divider/top-bar hairline `dededb` → `e5e5e5`, quiet chips as
  white + `e5e5e5` edge, kbd chips `efefee` before the fix, contacts menu panel 239 on 243) are the derived-edge
  policy's outcome (`edge_on`, `panel_ground`, each ≥ `HAIRLINE_MIN_DELTA`). Recorded for the coordinator's judgement,
  not worked around with declared tokens.
- **Code review.** The `code-review` skill's forked runs reported on the primary checkout's diff (imager, photos,
  weather, `toolbar.rs`), not this worktree, so none of their findings applied; this diff was reviewed against the
  previous agent's verified findings (a–f, all applied), the coordinator's triage notes (applied: `over()` for the
  three private tint helpers, `row_ground` + `Point` for the badge with the `too_many_arguments` allow removed,
  `#[derive(Default)]` for `ShotState`; judged and kept: the three independent `LaunchSpec` hook calls to
  `ShotState::from_args`; recorded: `parse_wxh` vs the private `ui::launch::parse_viewport`) and by hand.
- **Evidence size.** `target/ux1x/a6/` also keeps the previous agent's `after-draft/` captures and edit scripts
  (`edit_*.py`, `edit_review_fixes.py` — the unapplied review batch this worker reconciled) for provenance.


