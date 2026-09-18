# A2 — utilities: hex_editor, logs, typst_editor, terminal, snippets, monitor, disks, calculator

Base `fd1f84391` (Unit 0, `ux1x/unit0`), branch `ux1x/a2-utilities`, worktree
`/Users/xaver/dev/qos/.claude/worktrees/agent-a2f7bf5d21abdc40a`. Evidence under
`target/ux1x/a2/<app>/{before,after}/{dark,light}/<state>.png` (+ `matrix/` for the four
responsive viewports), geometry under `target/ux1x/a2/lint/resume/*.{inspection,audit}.json`
(final; `lint/after/` is the pre-resume run), every edit spec under `target/ux1x/a2/edit-*.py`.
204 BEFORE and 204 AFTER PNGs; every 1280×800 state PNG named below was opened with the Read
tool in both themes, twice — once by the first worker and again on resume.

## Six-rule checklist

1. **Nothing touches.** Every neighbouring pair on every screen keeps a token step (pair
   table below; measured from the AFTER inspections where the app exposes semantics, from
   the layout constants otherwise). Fixed: monitor process rows/sort headers (0 → `s1`,
   `apps/monitor/src/app.rs` `ROW_H`/`TABLE_HEADER_H`, row band inset), logs list rows
   (0 → `s1`, `apps/logs/src/app.rs` `draw_list` band), disks SMART and partition rows
   (0/2 → `s1`, `apps/disks/src/app.rs`), hex inspector zebra (2 → `s1`,
   `apps/hex_editor/src/view.rs`), calculator segmented segments (0 → `s1`,
   `apps/calculator/src/app.rs` `SEG_GAP`), typst compact toolbar `›` **overlapping** the
   Edit/Preview switch at phone width (overlap → `s3`, `apps/typst_editor/src/app.rs`
   `draw_toolbar_compact`/`draw_view_switch`).
2. **Every surface has a derived edge or a contrast step.** All app-painted card/row/pill
   edges now call `ui::surface::{card, pill, edge_on, edge_on_with, edge_outline,
   shadow}` directly at every site — the five pure per-app forwards this batch first
   introduced (calculator/snippets `draw::{card,edge}`, logs `card`/`edge`, disks
   `Self::{card,edge}`, monitor `charts::card`) were deleted per the plan's "delete the
   private duplicate once it is a pure wrapper"; the app helpers that remain are not pure
   forwards (`calculator::draw::wash`, `snippets::draw::stroke_edge`, `monitor::charts::{panel,
   inner_rule}`). Floating panels (typst kebab menu, logs dropdown list, snippets fill-in modal,
   disks confirm sheet, typst diagnostic card) use `ui::chrome::{panel_ground,
   panel_border}`; every Bar-role hairline (calculator/typst/hex/logs/disks top bars, typst
   problems panel) uses `ui::chrome::chrome_hairline`. `border_standard`/`border_strong`:
   **0 remaining** production uses in the eight apps (was 66); `Color::WHITE/BLACK`
   literals in paint paths: 0 (was 27). **On-fill ink uses the two toolkit contracts and
   nothing else**: `Colors::on_accent(ON_FILL_MIN_CONTRAST)` on an accent-primary fill
   (calculator segments/keys, logs live pill, typst active pills, hex cursor/kind chips) and
   `Colors::on_surface(fill).ink` on every other fill (logs severity chips, disks kind badge /
   partition segments / danger hover, typst stale banner, hex title pills). Both private
   `contrast_on` copies (hex, logs) are deleted — the review caught that my first rewrite to
   `hairline_on(bg, 255)` had dropped the contrast floor (white on amber at 2.1:1); the
   toolkit ink is measured against the fill and steps out until it clears 4.5:1.
3. **One rhythm per screen.** Calculator: keys `s2` (8), key groups `s3` (12), history rows
   `s2`, panel gutter `s4` (16, also in `framework_history.rs`, which had 16/70/82
   literals). Monitor: cards 14 (existing `GAP`), rows `s1`, sort headers `s1`. Snippets:
   pills `s2`, rows `s3` inset. Disks: cards `CARD_GAP` 12, tabs 6, rows `s1`. Logs: controls
   `s2`/`s3`, rows `s1`. Typst: toolbar pills 6, compact clusters `s3`. No relationship uses
   two different steps on one screen.
4. **Content centred on measured line boxes.** Unchanged and verified: calculator
   `draw::text_in` (measured line box), snippets `draw::pill` (measured), hex pills
   (measured `th`), typst pills (`measure_weighted`), monitor `row_text_top`, logs
   `m.vcenter`. The monitor row band is centred inside its pitch (`inset = s1/2` both sides).
5. **No authored-pixel exemption.** Every kept offset that produced a defect became a token
   expression: `SEG_GAP = Spacing::BASE.s1`, `ROW_BAND_H = 24`, `ROW_H`/`TABLE_HEADER_H =
   ROW_BAND_H + Spacing::BASE.s1` (1× seed) re-derived per frame as `Density::row_gap =
   spacing.s1` so pitch and inset move together under `ui_scale`, band insets `row_gap / 2`,
   hex inspector pitch `line + s1 + 3` with band `pitch − s1` from the same scaled token,
   history gutter `spacing.s4` / row gap `spacing.s3`, compact-toolbar clearances `spacing.s3`,
   segment padding `SEG_PAD_X` squeezing no further than `spacing.s2`.
6. **Evidence is visual.** Before/after PNGs at 1× dark and light for every touched surface
   (tables below), all opened; overlap/touching lint run over 20 inspections (table below).

## Capture harness notes

- `target/ux1x/a2/capture.sh <phase> [apps]` — every declared scenario / shots flag / env
  state, dark and light, isolated `HOME`/`XDG_*`. Env-driven states are run in subshells:
  a first pass leaked `QOS_CALC_EMPTY=1` into every later calculator run (POSIX `VAR=x fn`
  persists after a function call) and rendered all twelve calculator states identical —
  caught by md5, recaptured.
- Compact-only states need a compact viewport: snippets `narrow` and calculator `compact`
  at 480×800, typst `preview`/`menu` at 640×800 (`COMPACT_W = 760`).
- Calculator's custom shot renderer honoured `QOS_SHOTS_THEME=light` before (its own match);
  it now routes through `ui::launch::shot_theme(args)` (`apps/calculator/src/main.rs`), so
  theme, text scale and motion policy come from the one shared place. Light PNGs prove it.

## Surface table (BEFORE → AFTER, dark and light)

### calculator (`apps/calculator/src/{draw,keypad,app,graph2d,graph3d,framework_history,main}.rs`)

| surface | BEFORE defect (rule) | AFTER | dark / light PNG |
|---|---|---|---|
| Topbar | bottom rule `border_standard` on `raised` invisible (2) | `chrome_hairline` | `default.png`, `history-open.png` |
| Mode segmented + RAD/DEG track | bare `neutral[2]` fill, segments abut (1, 2); `Color::WHITE` labels | `draw::card` (edge_on) + `SEG_GAP` `s1`; `on_accent` ink | same |
| Display | `neutral[0]` + `border_standard` (2) | `draw::card` | `history-open.png` |
| Memory buttons, 46 keys, Clear history | bare fills (2); hover/press mixed literal white/black | `ui::surface::card`; `draw::wash` (polarity-correct) | `history-open.png`, `error.png` |
| History panel + recorded rows | left rule `border_standard` (2); rows bare `neutral[1]` (2) | `draw::edge(panel_bg)`; rows `draw::card` | `history-open.png`, `history-closed.png` |
| Graph 2D/3D function panel, rows, `+ Add`, info card, `×`, chips | panel rule, canvas outline `border_standard` (2); add/info/chip bare (2); `Color::WHITE` trace ring; `Color::BLACK` scrim | `draw::edge`/`draw::card`; `hairline_on(canvas,255)`; `surfaces.scrim` | `default.png`, `graph3d.png`, `long.png`, `selection.png`, `compact.png` |
| History-mode preview (`framework_history.rs`) | literal 16/70/82 gutter/row/step (3, 5) | `spacing.s4` gutter, `s3` row gap; rows via `draw_recorded_row` → card | in-app path, no shots route (limit) |

### snippets (`apps/snippets/src/{draw,app,expand}.rs`)

| surface | BEFORE defect | AFTER | PNG |
|---|---|---|---|
| Sidebar↔list and list↔detail dividers | `border_standard` (2) | `draw::edge(surfaces.base)` | `default.png`, `detail.png` |
| Hamburger / close toggle, empty tile, "New snippet" | raised + `border_standard` (2) | `draw::card` | `detail.png`, `empty.png`, `narrow.png` |
| Search field | base + 1.5 `border_standard` (2) | `draw::card` + focus ring only when focused | all |
| Language / folder / tag pills | bare tinted fills (2) | `ui::surface::pill` with `edge_on(bg)` | `default.png` |
| Code card | `neutral[0]` + `border_standard`; gutter painted over the edge | `draw_code` owns fill, gutter, then `draw::stroke_edge` (a 1 px stroke on `ui::surface::edge_outline`) on top; history preview gets the same card. The review caught my first version stroking `card.inset(0.5)` — kurbo's positive inset *grows* a rect, so the edge sat half a pixel outside the card | `default.png`, `detail.png` |
| Fill-in modal (`expand.rs`): panel, inputs, preview card | raised + `border_standard` ×3 (2) | `panel_ground` + `panel_border` on `edge_outline` (same inset-sign fix as the code card); `ui::surface::card` | not shot-reachable (no scenario); verified by build + tests |

### typst_editor (`apps/typst_editor/src/{app,preview,diagnostics,editor}.rs`)

| surface | BEFORE defect | AFTER | PNG |
|---|---|---|---|
| Toolbar bottom rule | `border_standard` on `raised` (2) | `chrome_hairline` | `default.png` |
| Toolbar buttons, document chip, Edit/Preview track | `lift(0.35)` + `border_standard` / bare base chip (2); `Color::WHITE` on accent | `ui::surface::card`; `on_accent` | `default.png`, `menu.png` |
| Kebab menu panel | overlay halo + raised + `border_standard` (2) | `surface::shadow(e3)` + `panel_ground` + `panel_border` | `menu.png` (640) |
| Compact toolbar at phone width | `›` **overlapped** the switch (1) | chip cap accounts for arrows + both `s3` gaps + the switch and may elide down to a lone ellipsis; the switch then squeezes `SEG_PAD_X` (11) toward `s2` before it may yield right. Test `compact_toolbar_controls_never_overlap_at_phone_width` (390/480/560 px, four documents, problems badge armed): no two `toolbar_hits` rects overlap | `matrix/dev.qos.typst-editor-phone.png` |
| Problems panel top rule | `border_standard` (2) | `chrome_hairline` | `problems.png`, `errors.png` |
| Preview page border, thumb strip rule, thumbnail border | `border_standard` ×3 (2) | `edge_on(neutral[1])` / `edge_on(raised)` | `default.png` |
| Stale banner text | `Color::from_rgba8(20,18,10)` | `on_surface(semantic.warning).ink` | `stale.png` |
| Diagnostic hover card (`editor.rs`) | raised, no edge (2) | `panel_border` on `edge_outline` | interaction-only (limit) |
| Divider grip dots | `Color::WHITE` on accent | `on_accent(NON_TEXT)` | `default.png` |

### terminal (`apps/terminal/src/main_body.rs`)

| surface | BEFORE defect | AFTER | PNG |
|---|---|---|---|
| "Session ended" status card | bare `lift(0.3)` fill (2) | `ui::surface::card` | `ended.png` |
| Scrollbar track/thumb, selection band, cursor, empty state | contrast steps present; unchanged | — | `scrollback.png`, `shell-select.png`, `empty.png` |
| Context menu | shared `ContextMenu` (edge present; separators full-bleed) | Open — needs F2 | `empty-menu.png` |

### hex_editor (`apps/hex_editor/src/view.rs`)

| surface | BEFORE defect | AFTER | PNG |
|---|---|---|---|
| Title bar bottom rule | `border_standard` on `raised` (2) | `chrome_hairline` | `default.png` |
| OVR/BE/24-row pills | bare fills (2); private `contrast_on` with literal white / `from_rgb8` | `ui::surface::pill` + `edge_on`; ink `on_surface(fill).ink` (pills) / `on_accent` (cursor cells, kind chip); private fn deleted | `default.png` |
| Column-header rule, hex/ASCII separators, inspector left edge | `border_standard` on base (faint on dark) | `edge_on(surfaces.base)` | `default.png`, `search.png` |
| Inspector zebra bands | 1 px seam (1) | row pitch `line + s1 + 3`, band `pitch − s1`, one scaled token (the review caught the first version mixing an unscaled 7 px pitch with the scaled `s1`) | `default.png` |
| Search / go-to panels, status bubble | focus ring on the focused panel; bubble is shared `status_bubble` | unchanged / Open — needs F4 | `search.png`, `error.png` |

### logs (`apps/logs/src/app.rs`)

| surface | BEFORE defect | AFTER | PNG |
|---|---|---|---|
| Header bottom rule, filter band rule | `border_standard` (2) | `chrome_hairline` / `edge_on(base)` | `default.png` |
| Refresh, Live-tail pill, Actions trigger, back pill | lift fill + `border_standard` (2); `Color::WHITE` on accent | `ui::surface::card`; `on_accent` | `default.png`, `actions.png` |
| Search box, Unit/Boot dropdown triggers | raised + `border_standard` (2) | `ui::surface::card` + focus/active ring only when so | `default.png` |
| Table card + header rule, detail card + header rule | `border_standard` ×4 (2) | `ui::surface::card`, `edge_on(raised)` | `default.png`, `error.png`, `empty.png` |
| List rows | zebra `tint(WHITE,6)` theme-blind; bands touching (1) | `hairline_on(raised,6)`; band inset `s1/2` | `default.png`, `filter-errors.png` |
| Scrollbar thumb | `tint(WHITE,70)` | `hairline_on(raised,70)` | `default.png` |
| Dropdown popup (hand-rolled, L5) | `tint(BLACK,80)` shadow + overlay + `border_standard` (2) | `surface::shadow(e3)` + `panel_ground` + `panel_border`; widget swap recorded as Open | `actions.png` (Actions menu) |
| Severity chip ink | private `contrast_on` with `from_rgb8(20,20,25)` / `WHITE` | `theme.tokens.colors.on_surface(col).ink` (near-black on amber, white on the dark reds); private fn deleted | `default.png` |

### monitor (`apps/monitor/src/{charts,app}.rs`)

| surface | BEFORE defect | AFTER | PNG |
|---|---|---|---|
| `charts::panel` (every card) | raised + `border_standard` (2) | `ui::surface::card` | `default.png`, `resources.png` |
| Chart grid, gauge tracks, scrollbar grooves, sparkline box | `with_alpha(border_standard, 40..150)` | `charts::inner_rule(theme, α)` = `edge_on_with(raised, α)` | `resources.png`, `detail.png` |
| Interval pill, pill buttons, toolbar buttons, back button, toggles, search | `border_standard` strokes (2) | `ui::surface::card` / `edge_on(fill)` | `default.png`, `interval-popover.png` |
| Table header rule, detail rules, action-bar hairline | `border_standard` (2) | `edge_on(base/raised)` | `default.png`, `detail.png` |
| Process rows and sort headers | 22 px targets, rows touching, headers touching (1, lint: 48 failures) | pitch = `ROW_BAND_H` (24) + `Density::row_gap` (this frame's `spacing.s1`), band and hit rect inset `row_gap / 2`; sort headers inset the same. Verified at 1× and `ui_scale` 2 (`table_rows_and_sort_headers_keep_an_s1_gap_at_1x` runs both) (lint: 0) | `default.png`, `tree-expanded.png` |
| Detail sparkline well | `with_alpha(border_standard, 40)` fill | `charts::inner_rule(theme, 12)` fill — a contrast step inside the card, deliberately no second edge (`edge_on` cannot derive against a translucent wash) | `detail.png` |

### disks (`apps/disks/src/app.rs`)

| surface | BEFORE defect | AFTER | PNG |
|---|---|---|---|
| Top bar rule | `border_standard` (2) | `chrome_hairline` | `default.png` |
| Drive cards, header card, action bar, SMART area, tiles, empty card | raised + `border_standard` (2) | `ui::surface::card` | `default.png`, `tab-smart.png` |
| Action buttons (enabled), whole-disk pill | `border_standard` (2); `Color::WHITE` on danger hover | `edge_on(fill)`; `on_surface(danger).ink` | `default.png` |
| Action buttons (disabled) | `with_alpha(neutral[2],120)` + `with_alpha(border_standard,120)` | rest face **and** derived edge under `widget::DISABLED_ALPHA` on `edge_outline` — the review caught my first version giving the inert control a full-strength edge | `default.png` (Mount/Unmount/Eject) |
| Partition map + segment labels | bare `neutral[2]` (2); `Color::WHITE`/`BLACK` washes | `ui::surface::card`; `on_surface(col).ink`, `hairline_on(col,70)` | `default.png` |
| Kind badge ink | `Color::WHITE` | `on_surface(badge_fill).ink` | `default.png` |
| SMART rows / partition rows | `with_alpha(WHITE, 8)` zebra; bands touching (1) | `lift(0.35)`; `s1` gap | `tab-smart.png`, `tab-partitions.png` |
| Confirm modal | raised + `border_standard` (2) | `panel_ground` + `panel_border` | `pending-format.png` |
| Overlay flap scrim + edge | `Color::BLACK` scrim; `border_standard` edge | `surfaces.scrim`; `edge(base)` | `narrow.png` |

Light did not regress on any of the above: light `default.png` for all eight apps plus
`history-open.png` (calculator), `menu.png` (typst), `actions.png` (logs),
`pending-format.png` (disks), `narrow.png` (snippets) were opened and show the same
derived edges (black hairline on light grounds) with no doubled or missing rules.

## Pair table (1×, dark; light identical geometry)

Measured from `target/ux1x/a2/lint/after/*.inspection.json` via `target/ux1x/a2/pairs.py`
where the app exposes semantic rects; otherwise from the layout constants named.

| surface (PNG) | element A | element B | axis | gap px before → after | relationship / step | OK |
|---|---|---|---|---|---|---|
| calculator `history-open.png` | key | key (row) | x | 8 → 8 | siblings `s2` (`cell_gap`) | ✓ |
| calculator `history-open.png` | key | key (column) | y | 8 → 8 | siblings `s2` | ✓ |
| calculator `history-open.png` | memory button | first key row | y | 12 → 12 | section `s3` (`gap`) | ✓ |
| calculator `history-open.png` | display | memory row | y | 12 → 12 | section `s3` | ✓ |
| calculator `history-open.png` | history row | history row | y | 8 → 8 | related rows `s2` | ✓ |
| calculator `history-open.png` | keypad | history panel edge | x | 16 → 16 (+1 px derived edge) | gutter `s4` | ✓ |
| calculator `default.png` | Calculator | Graph 2D segment | x | 0 → 4 (lint/resume: 4.0; keys 8.0, memory row → keys 12.0) | segments `s1` (`SEG_GAP`) | ✓ |
| calculator `default.png` | RAD | DEG segment | x | 0 → 4 | segments `s1` | ✓ |
| calculator `default.png` | topbar | body | y | rule invisible → 1 px `chrome_hairline` | Bar hairline | ✓ |
| monitor `default.png` | process row | process row | y | 0 → 4 (lint/resume: 4.0 on every consecutive pair) | rows `s1` | ✓ |
| monitor `default.png` | Sort by PID | Sort by name | x | 0 → 4 | siblings `s1` | ✓ |
| monitor `default.png` | sort header | first row | y | 0 → 4 | `s1` | ✓ |
| monitor `default.png` | summary card | summary card | x | 14 → 14 | `GAP` (existing rhythm) | ✓ |
| monitor `default.png` | search | table header | y | 16 → 16 | section | ✓ |
| monitor `default.png` | Processes | Resources tab | x | 6 → 6 | selector pills | ✓ |
| disks `default.png` | Overview | SMART & Health tab | x | 6 → 6 (lint/resume: 6.0) | selectors | ✓ |
| disks `default.png` | SMART & Health | Partitions tab | x | 6 → 6 (lint/resume: 6.0) | selectors | ✓ |
| disks `default.png` | drive card | drive card | y | 12 → 12 | `CARD_GAP` `s3` | ✓ |
| disks `tab-partitions.png` | partition row band | partition row band | y | 2 → 4 | rows `s1` | ✓ |
| disks `tab-smart.png` | SMART row band | SMART row band | y | 0 → 4 | rows `s1` | ✓ |
| logs `default.png` | list row band | list row band | y | 0 → 4 | rows `s1` | ✓ |
| logs `default.png` | search | Unit dropdown | x | 12 → 12 | siblings `s3` | ✓ |
| logs `default.png` | severity chip | severity chip | x | 6 → 6 | siblings | ✓ |
| logs `default.png` | table card | detail card | x | 16 → 16 | `PAD` `s4` | ✓ |
| hex `default.png` | inspector band | inspector band | y | 2 → 4 | rows `s1` | ✓ |
| hex `default.png` | OVR | BE pill | x | 8 → 8 | siblings `s2` | ✓ |
| snippets `default.png` | pill | pill | x | 8 → 8 | siblings `s2` | ✓ |
| snippets `default.png` | action button | action button | x | 8 → 8 | siblings `s2` | ✓ |
| snippets `default.png` | sidebar rail | list | x | divider invisible → 1 px `edge_on(base)` | rule on base | ✓ |
| typst `default.png` | toolbar pill | toolbar pill | x | 6 → 6 | siblings | ✓ |
| typst phone matrix | `›` | Edit/Preview switch | x | **−26 (overlap)** → 12 | `s3` | ✓ |
| typst `menu.png` | menu row | menu row | y | 0 (transparent rows, hover lift only) | list rows: none by policy | ✓ |
| terminal `ended.png` | status card | grid edge | x/y | `s2` → `s2` | inset `s2` | ✓ |

## Overlap / touching lint (`usability.geometry_audit`, `--headless-drive`, 1280×800)

Final run on the committed binaries: `target/ux1x/a2/lint/resume/*.{inspection,audit}.json`
(`target/ux1x/a2/lint-resume.log`); the earlier `lint/after/` set is the same geometry before
the disks container bounds were added. `measured` is the audit's own count of bounded
actionable controls it could place; `pairs` the peer pairs it compared for touching/overlap.

| inspection | semantic nodes | measured controls | peer pairs | failures (touching / overlap / target / label) | passed |
|---|---|---|---|---|---|
| calculator-dark / -light | 61 | 1 | 0 | 0 / 0 | ✓ / ✓ |
| disks-dark / -light | 8 | **0 → 4** | **0 → 6** | 0 / 0 | **✗ → ✓** (Toolbar / List / TabBar nodes now carry bounds, so the audit can descend) |
| hex_editor-dark / -light | 3 | 1 | 0 | 0 / 0 | ✓ / ✓ |
| logs-dark / -light | 16 | 13 | 66 | 0 / 0 | ✓ / ✓ |
| monitor-dark / -light (+ `--scenario resources`) | 36 | 35 | 595 | **48 → 0** / **48 → 0** | ✓ / ✓ |
| snippets-dark / -light (+ `--scenario detail`) | 2 | 1 | 0 | 0 / 0 | ✓ / ✓ |
| typst_editor-dark / -light (+ `--scenario problems`) | 5 | 1 | 0 | 0 / 0 | ✓ / ✓ |

Monitor's 48 were 19 × `target_size_24` (22 px rows) and 29 × `touching_peers` (rows and
sort headers at gap 0), measured on the pre-fix geometry; after `ROW_H`/`TABLE_HEADER_H`
and the band insets they are 0 in both themes. The other apps' geometry did not change, so
their BEFORE equals AFTER (0). Terminal is not linted: its `--headless-drive` root spawns a
live PTY (limit below). The `--headless-drive` root at this base is the *live* root, so disks
measures its empty-topology controls (Rescan + three tabs) and calculator/hex/snippets/typst
expose one bounded control each; the screenshot states are the demo roots, so rule 1 for
their rows rests on the pair table and the PNGs.

## Resume after the machine reboot

The first worker was killed mid-run; its worktree, edits, captures and this draft survived
and nothing was discarded. On resume the checkpoint was re-verified from scratch rather than
trusted:

- `cargo build` of all eight binaries was a no-op against the surviving `target/` (only
  `hex_editor` recompiled, for the whitespace fix below), so the AFTER PNGs and inspections
  were produced by the committed sources; typst_editor's last edit (17:12) had been
  recaptured by the final chain (`capture-after-typst.log`).
- Every 1280×800 AFTER PNG in both themes (142), the compact-viewport states, the typst phone
  matrix in both themes and the BEFORE PNGs of every surface with a rule-1 or rule-2 defect
  were opened again with the Read tool; the surface table above matches what they show.
- The geometry lint was re-run over all twenty inspections into `lint/resume/` (table
  above): zero `touching_peers`, zero `semantic_bounds_overlap_unresolved`, zero
  `target_size_24`.
- Two small edits on top of the checkpoint: `apps/hex_editor/src/view.rs` lost the three
  blank lines the deleted `contrast_on` left behind, and `apps/disks/src/app.rs` gives its
  Toolbar, List and TabBar access nodes their painted bounds (top bar, list header ∪ body,
  union of the tab rects) — without them the audit could not descend to the Rescan button or
  the tabs and reported disks as unobservable (`passed: false`, 0 measured); now 4 controls,
  6 peer pairs, 0 failures in both themes, and the disks lib tests (62) still pass.
- Tests, `cargo check --tests` and the ratchets were re-run after those edits (Tests and
  Ratchets below quote the resumed logs).

## Code review (`/code-review high`)

Ten findings, nine confirmed; all were regressions or gaps in my own diff and are fixed, not
recorded (evidence recaptured afterwards):

| # | finding | fix |
|---|---|---|
| 1 | typst compact switch could still overlap the kebab/problems pill on a ~360 px bar with 2+ documents; hit precedence went to the pill | chip cap floors at a measured ellipsis, segments squeeze `SEG_PAD_X → s2` before yielding; new test over 390/480/560 px with a problems badge asserts pairwise non-overlap of every toolbar hit |
| 2 | `contrast_on → hairline_on(bg,255)` had no contrast floor (white on amber 2.1:1) in logs and hex | `on_surface(fill).ink` / `on_accent`; both private fns deleted |
| 3 | monitor `ROW_H` used compile-time `BASE.s1` while the band inset used the scaled `s1` → band under 24 px at `ui_scale` > 1 | `Density::row_gap` = scaled `s1`; pitch = `ROW_BAND_H + row_gap + text growth`; test runs at `ui_scale` 2 |
| 4 | sparkline well `card(..)` on a translucent wash derives the wrong edge polarity | plain `fill_rrect` with `inner_rule` (contrast step, no second edge) |
| 5 | disabled disks button got a full-strength edge around a 47 % face | face and `edge_on` both under `widget::DISABLED_ALPHA` |
| 6, 7 | snippets `card.inset(0.5)` / `panel.inset(0.5)` — kurbo's positive inset grows the rect | `draw::stroke_edge` over `ui::surface::edge_outline` |
| 8 | hex inspector band mixed an unscaled 7 px pitch with the scaled `s1` | pitch `line + s1 + 3`, band `pitch − s1` |
| 9 | four on-fill ink idioms | two toolkit contracts (`on_accent`, `on_surface(fill).ink`), see rule 2 |
| 10 | five pure wrappers; toolkit chips/segments still `border_standard` | wrappers deleted (76 call sites now call `ui::surface::*` directly); toolkit items recorded under Open (F1, F4) |

## Code review on resume (`/code-review medium`)

The skill's forked finders returned only interim status on both invocations (their final
ranked list never arrived), so the medium pass was completed by hand over the full diff read
at resume, with every claim below checked by grep or by reading the surrounding code rather
than taken from the draft:

| check | result |
|---|---|
| `border_standard` / `border_strong` / `overlay_border_strong` in the eight apps' `src` | 0 (grep) |
| `Color::WHITE` / `Color::BLACK` in the eight apps' `src` | 0 (grep) |
| `edge_on(ground)` picks the ground it actually sits on | monitor search box is filled `surfaces.base` and stroked `edge_on(base)`; disks flap edge on `base`; logs header/detail rules on `raised` inside `raised` cards; typst page border on the `neutral[1]` pane; all consistent |
| disks partition band `ry − 6 .. ry − 6 + 38 − s1` matches the caller's pitch | `ry += 38.0` at both callers (lines 763, 2529) |
| hex inspector `row_h = line + s1 + 3`, `band_pad = 1.5` → band `line + 3`, gap `s1` | arithmetic checked; pitch at 1× unchanged (line + 7) |
| calculator `render_shot_to_png` → `ui::launch::shot_theme(args)` | `shot_theme` resolves `QOS_SHOTS_THEME`, then `shot_text_scale(args.text_scale, QOS_SHOTS_TEXT_SCALE)`, then the motion preference — the CLI `--text-scale` precedence the old private code had is preserved |
| snippets `draw_code` now owns the card fill + edge; its second caller (`framework_history.rs:221`) paints nothing of its own first | no double edge; the history preview gains the same derived edge |
| calculator `draw::wash` makes hover (0.10) and press (0.18) two strengths of one polarity-correct move instead of white-then-black literals | intentional per HIG §9.3 ("same kind of change at two strengths"); both states stay distinct |
| monitor sort-header hit rects shrink `row_gap / 2` per side | 28 → 24 px at 1×, exactly the lint floor; the test asserts `≥ 24` at 1× and 2× |
| typst `draw_view_switch` now painted after the document cluster | z-order only matters when they overlap, which the new test forbids at 390/480/560 px |

No regressions found in the diff; the two edits made on resume (hex blank lines, disks
container bounds) are described above.

## Ratchets

`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`: 7 passed, 2 failed —
both failures are not A2 growth:

- `app_and_shell_theme_escape_hatches_do_not_grow`: only the pre-existing
  `apps/browser/src/browser_tabs.rs` and `shell/src/shell/frame.rs` `grew` lines. **No A2 file
  grew.** Reductions for the coordinator's re-baseline (`UPDATE_APP_SHELL_DESIGN_BASELINE=1`):
  `apps/hex_editor/src/view.rs` literal_color 1→0, white_or_black 1→0;
  `apps/logs/src/app.rs` literal_color 1→0, white_or_black 6→0;
  `apps/typst_editor/src/app.rs` white_or_black 3→0; `apps/typst_editor/src/preview.rs`
  literal_color 1→0; `apps/disks/src/app.rs` white_or_black 8→0; `apps/calculator/src/app.rs`
  white_or_black 3→0; `apps/calculator/src/graph2d.rs` white_or_black 2→0;
  `apps/calculator/src/graph3d.rs` white_or_black 1→0; `apps/calculator/src/keypad.rs`
  white_or_black 3→0.
- `f1_spacing_token_audit::spacing_literals_do_not_grow` — stale (expected, coordinator
  lowers it): `apps/monitor/src/app.rs [pixel_const]: now 14, baseline still 15 — lower it
  to 14` (`ROW_H`/`TABLE_HEADER_H` became token expressions; `ROW_BAND_H` is the one new
  pixel constant, the lint's 24 px floor).
- `declared_borders_in_chrome_do_not_grow`, `theme_escape_hatches_do_not_grow`,
  `no_hardcoded_style_literals_in_tree`, pattern self-checks: pass.

## Tests

`cargo test --no-default-features --locked -p calculator -p snippets -p typst_editor
-p hex_editor -p logs -p monitor -p disks -p terminal --no-fail-fast`
(`target/ux1x/a2/app-tests-resume.log`, re-run after the resume; disks again after its
container-bounds change, `disks-tests-resume.log`):

| crate | result |
|---|---|
| calculator | lib 152 passed; main 1 passed |
| disks | lib 62 passed |
| hex_editor | lib 55 passed; `tests/open_save.rs` 3 passed |
| logs | lib 38 passed, **1 failed** — `journal::tests::export_preserves_a_non_utf8_portal_path` (known pre-existing: byte-0xFF filename on APFS) |
| monitor | lib 100 passed (includes the new `table_rows_and_sort_headers_keep_an_s1_gap_at_1x` and the updated 2× density test asserting band = pitch − `s1` and an `s1` gap) |
| snippets | lib 76 passed; main 1 passed |
| terminal | lib 109 passed; `tests/shots_smoke.rs` 2 passed |
| typst_editor | lib 59 passed (includes the new `compact_toolbar_controls_never_overlap_at_phone_width`); main 1 passed |

`cargo check -p <app> --tests` for all eight: clean (`target/ux1x/a2/check-tests-resume.log`,
disks re-checked after its last edit in `check-disks-resume.log`). `cargo build -p <app>` for
all eight: clean (`build-resume.log`; only the pre-existing `ui` warnings and the `__eh_frame`
linker note). The logs failure `journal::tests::export_preserves_a_non_utf8_portal_path` is
in `apps/logs/src/journal.rs`, which this branch does not touch (byte-0xFF filename on APFS).

## Open — needs another unit

- **F4** `ui/src/status_bubble.rs:164` — `draw_pill` rest edge is declared; used by typst
  (`Ln 1, Col 1`, `Compiled in …`) and hex (`0x00000010 (16)`) status pills. PNG:
  `target/ux1x/a2/typst_editor/after/dark/default.png` (bottom corners),
  `target/ux1x/a2/hex_editor/after/dark/default.png` (bottom right).
- **Open — needs coordinator (closed on `ux1x/integration`, F2)** `ui/src/widget/context_menu.rs:522,600-608`
  — terminal's and monitor's context menu: declared panel edge and full-bleed separators. F2
  landed ContextMenu as a Panel role on the integration branch; this base cannot see it. PNG:
  `target/ux1x/a2/terminal/after/dark/empty-menu.png`, `target/ux1x/a2/monitor/after/dark/context-menu.png`.
- **F4** `ui/src/surface.rs:463` `chip_colors` still returns `Some(border_standard)` for the
  quiet chip (used by `ui/src/widget/window_group.rs` tab/restore/close chips on every dark
  desktop) — the review's "right altitude" fix is `Some(edge_on(surfaces.raised))`; not an app
  file.
- **Open — needs coordinator (closed on `ux1x/integration`, F1)** `ui/src/widget/toggle_group.rs:469,477,681,697`
  and `ui/src/widget/segmented.rs:211,579` stroke `border_standard` on raised tracks — the shared
  segmented controls this batch's apps do not use but every other app inherits. F1 landed
  `edge_on(track)` on the integration branch. Same for the shared `Button` Neutral rest edge
  behind hex's `Open file…`, monitor's `Pause`/`Refresh`/`Stop`, disks' `Rescan` and the shared
  `ModalDialog` behind monitor's kill confirm (F1/F2, closed on integration).
- **Coordinator / history-mode owner** — the `FrameHarness` history-mode tab strip
  (`History | Snippet library`) paints a white bar over the dark theme for every
  history-enabled app capture. PNG: `target/ux1x/a2/snippets/after/dark/default.png` (top
  32 px). Not an app surface (`ui/src/history_mode.rs` / headless harness).
- **Open — needs coordinator (closed on `ux1x/integration`)** — at this base `--headless-drive`
  accepts `--scenario` only through `LaunchSpec::screenshot_root`, which for apps with a
  `shot_handler` (monitor, typst_editor, disks, calculator, terminal) falls back to the live
  root, so named states have no geometry dump; the integration branch's `--headless-drive`
  opens state through the spec's own opener.
- **Unit 0 tooling (`scripts/ux-bench/usability.py::geometry_audit`)** — the walk only descends
  into a projected semantic child that has `bounds`, so controls under an unbounded grouping
  node (Toolbar, List, TabBar, Group) are never measured and the audit reports `passed: false`
  with zero failures. Disks hit this (8 nodes, 0 measured) until its three containers were given
  their painted bounds (this branch); hand-painted apps that keep unbounded containers will
  keep reading as unobservable. Not an app file.
- **L5 (logs)** — the Unit/Boot dropdown stays hand-painted (now Panel-role edges); swapping
  it for the shared `Dropdown` is a retained-widget migration inside an immediate-mode
  painter and is recorded, not done.

## Limits

- The `code-review` skill could not deliver its final list in this session (forked finders
  reported "still running" twice and never returned); the medium review above is manual.
- Hand-painted apps expose few semantic nodes (hex 1, snippets 1, typst 1, logs 2 actionable),
  so the touching lint observes little there; rule 1 for those rests on the pair table
  (constants + PNG inspection), not on the lint.
- The three `framework_history.rs` previews (calculator, snippets, typst) have no shots route;
  their surfaces are painted by the same `draw_recorded_row` / `draw_code` / pane painters
  fixed above, so the edge follows, but no history-mode PNG exists to show it.
- Snippets' fill-in modal (`expand.rs`) and typst's diagnostic hover card have no scenario;
  fixed by the same helpers, verified by build and tests only.
- Terminal is a content grid with one own surface (the status card); its pointer/menu
  surfaces are the shared widgets above.
- `Metrics::BASE.min_pointer_target` is 32; the lint's floor is 24. Monitor rows target the
  24 px floor (28 px pitch at 1×) — a 36 px pitch would halve the visible rows on a laptop.
- typst compact toolbar: below roughly 340 px (two documents plus a problems badge) even an
  ellipsis chip and fully squeezed segments cannot seat three clusters, and the switch yields
  on the right; the narrowest declared viewport (phone, 390 px) is covered by the new test.
