# A2 — Developer tools II & utilities: alignment review, round two

Unit A2 of the 2026-09-17 round-two alignment review: `hex_editor`, `logs`,
`typst_editor`, `terminal`, `snippets`, `monitor`, `disks`, `calculator`.
Branch `ui-review/r2-a2-devtools-2-utilities`, cut from `381eff6a8` (the merged
round-one tip, "Merge ui-review/integration"). Only `apps/<app>/` trees were
edited; `ui/`, `shell/` and `crates/` are untouched.

Capture root: `target/ui-review/<app>/{before,after}/` — dark and light
responsive matrices, `large-text.png` (800×900, `--text-scale 2`),
`narrow.png` (480×900), `compact.png` (800×700), `wide.png` (1200×800), every
`--scenario` at 1× (`<s>.png`) and at 2× (`<s>-2x.png`), plus the flag-driven
variants (`logs/{after-actions,after-detail,after-error,after-filter-errors,
after-loading}`, `disks/after-narrow`, `calculator/{after-3d,after-2d,
after-empty}`). Every PNG referenced below was opened and inspected; the
intermediate `mid*/` directories are the per-app iterations.

## Round one survived the merge

Rebuilt at the merged tip and re-captured before touching anything:

| item | evidence | status |
|---|---|---|
| monitor per-core grid balances rows (M2) | `monitor/before/resources.png` 4×2, `resources-many.png` 6×4 | ok |
| monitor context menu is the shared primitive, anchored (M3) | `monitor/before/context-menu.png` | ok |
| monitor tree-mode NAME header shares the cells' edge (M1) | `monitor/before/tree-expanded.png` header x=105 = cells | ok |
| hex `CellMetrics` drives layout, paint and hit-test (H1/H2) | `hex_editor/before/large-text.png` 2× grid coherent; `grid_pitches_…` test green | ok |
| snippets toggle beside the search field, not over it (S1) | `snippets/before/compact.png`, `narrow.png` | ok |
| disks header/action band/rows share one inset and radius (D1) | `disks/before/wide.png` | ok |
| calculator 3D rows show their start when inactive (C1), topbar segments seat their labels (C2) | `calculator/before-3d/wide.png`, `calculator/before/large-text.png` | ok |

## The one defect under most of the 2× findings

Theme type tokens (`theme.tokens.typography.scale.*`) already include the
accessibility text scale, and the size-based `TextRenderer` APIs
(`measure*`, `draw_text*`, `ButtonFace::text_size`, `status_bubble::pill_size`)
multiply by it again. Every token an app fed to those APIs painted 4× glyphs
at 200 %. Each app in this unit now converts the token to the renderer's input
space exactly once (`TextRenderer::renderer_input_style(...).size`, via a
small per-app `ts()` helper), and every text-bearing band takes its pitch
from the *measured* line box of the lines it stacks — the 1× constants stay
the design, each band grows by exactly the extra height its lines gain.
Vertical steps that used `size × 1.25` or `f64::from(size)` were moved onto
measured line boxes. The helpers (`ts`, `line_box`, `line_growth`,
`paint_scale`) are copied per app on purpose: this unit may not edit `ui/`;
the coordinator consolidates them into `ui/src/text_metrics.rs` at
integration (see "Open — needs …").

## Findings

Status legend: **fixed** (in this branch, AFTER capture inspected) · **open**
(recorded, with owner) · **ok** (checked, no defect).

### monitor (`apps/monitor/src/{app.rs,charts.rs}`)

| id | surface | defect | viewport/theme | before → after | status |
|---|---|---|---|---|---|
| M6 | whole app at 2× | Tokens through the legacy API: 4× glyphs; rows, pills, header, strip and detail collide. | 800×900 2× | `monitor/before/large-text.png` → `monitor/after/large-text.png` | fixed — 45 sites through `charts::ts`; a per-frame `Density` (rows, table header, toolbar rows, header, detail strip, stat rows, strip cell, per-core row, `text_k`) measured in `draw` and read by layout, paint, `rows_per_page`, wheel-to-rows and the seeded menu anchor; `Density::measure` at 1× equals the constants (test) |
| M6a | header at 2× | Tabs clipped (`Process`/`Resource`), tool buttons over the tabs, interval pill measured at `xs` but drawn at `sm`, uptime on a fixed 22 px line. | 800×900 2× | `before/large-text.png` → `after/large-text.png` | fixed — segment/pill height grows with the `sm` line; pill measured at the size it draws; uptime stacks on the clock's measured line; tab segments grow by the labels' measured growth over 1× (the 1× 124/96 px thresholds are untouched) |
| M6b | summary strip at 2× | Stacked label/value overprinted the sparkline. | 800×900 2× | same | fixed — both branches step by measured lines; the cell grows by the two lines the tight branch stacks |
| M6c | process table columns at 2× | `622.0 M…`, `26…`: glyph-budget widths designed at 1×. | 800×900 2× | same | fixed — columns laid out in 1× space (`avail_w / k`) and scaled by `k`, so the width-dependent column set folds as a half-width 1× window would; header and cell hit rects read the scaled columns |
| M8 | kill-confirm dialog | Empty band between message and buttons: `ModalDialog` fills its `max_h` (`with_max_size(420, 190)`) instead of measuring its body. | 1200×800 | `monitor/before/kill-confirm.png` = `after/kill-confirm.png` | open — needs F4 (`ui/src/widget/modal_dialog.rs`: body-measured height); the app already passes the tightest sane max |
| M7 | 14 px pad/gap, 10 px pill gap | Off the 4 px scale, named, used consistently; the reference layout is drawn to them. | — | — | ok — decision: keep (recorded in round one; no change) |

Tests: `density_at_1x_is_the_design_constants`,
`density_at_2x_grows_rows_by_their_measured_lines_and_rows_stay_seated`
(paints a 2× frame; asserts the cached density, equal-pitch non-overlapping
row hit rects, and `rows_per_page × row_h ≤ viewport`).

### hex_editor (`apps/hex_editor/src/view.rs`)

| id | surface | defect | viewport/theme | before → after | status |
|---|---|---|---|---|---|
| H4 | title bar at 2× | Title (`lg`), byte count (`sm`) and pills (`xs`) double-scaled into each other in a fixed 44 px bar. | 800×900 2× | `hex_editor/before/large-text.png` → `hex_editor/after/large-text.png` | fixed — 18 token sites through `ts`; the bar is 44 px + the `lg` line's measured growth, measured inside `CellMetrics::measure` so the event path (`hit_test`, pill rects) and the painted frame share one `GridMetrics`; pills wrap their measured line |
| H5 | inspector rows | Zebra band at `y − 3 .. y + row_h − 5`, `label_w` 72 fixed, `y += 22/24` literals. | 1× / 2× | `before/wide.png` → `after/wide.png` | fixed — one measured pitch (22 px at 1×), band centred on the value's line box, label column = widest label + gutter |
| H6 | status bubble | Passed a double-scaled size; at 2× the pill was 4× and clipped. | 2× | `before/large-text.png` → `after/large-text.png` | fixed on the app side (renderer-relative size). The shared `status_bubble::pill_size` still sizes its height from `text_size + 16` rather than the measured line, so at 2× the 30 px pill is tight around a 30 px line: open — needs F4 (coordinator fixes `ui/src/status_bubble.rs` at integration) |
| H7 | empty state, search and go-to sheets | Stacked on font-size offsets (`cy − 78`, `cy − 40`, chip 48×22, sheet 56, input at `y0 + 19`). | 2× | `before/large-text.png` → `after/large-text.png` (`--shots` opens with a file; the sheets are exercised by the suite) | fixed — anchored on the button and stacked on measured lines; chip/sheet/caption heights grow with their lines |

Test: `grid_pitches_follow_the_measured_mono_metrics_and_hit_their_cells`
extended — paints a 1× and a 2× frame and asserts the event-path
`metrics()` (header bottom, grid top) equal the painted frame's, and that
`title_growth` is 0 at 1× and > 4 px at 2×.

### snippets (`apps/snippets/src/{app.rs,draw.rs}`)

| id | surface | defect | viewport/theme | before → after | status |
|---|---|---|---|---|---|
| S2 | list rows, sidebar rows, search band, detail header/pills/toolbar/code at 2× | Fixed 64/32/36/22/32/20 px bands; title, preview and `n vars` collide; code lines overprint. | 800×900 2× | `snippets/before/large-text.png` → `snippets/after/large-text.png` | fixed — per-frame `Metrics` (row, sidebar row, search band, toggle, button growth, title/preview/detail/body line boxes, pill growth) measured in `draw`; 1× exact (`Metrics::ONE`); badge sized from its sigil and centred on the title line; hint level with the title line |
| S2a | list row title vs trailing hint | Long titles ran under `n vars`; preview clipped by character count. | 2× (and long titles at 1×) | same | fixed — title elided with `TextRenderer::elide_to_width_weighted` (Semibold, the weight it draws) to the width beside the hint; monospace preview fitted from one measured advance |
| S2b | detail toolbar at 2× | Five buttons overflowed the pane; Delete clipped off. | 800×900 2× | same | fixed — buttons wrap onto measured rows when the next would cross the pane inset; every action stays reachable |
| S4 | docked sidebar collapse/expand | Toggle popped in/out at `reveal ≥ 0.999`; the search field's inset stepped 44 px on that frame. | wide, during the ease | (animation; not capturable in a still) | fixed — `toggle_presence()` = `1 − reveal` beside a docked rail (1.0 for overlay / pushed detail): the toggle fades in an alpha layer, the field's inset eases by the same factor, and its hit rect is recorded whenever it is painted (one predicate for paint and hit); dead `show_toggle` deleted |
| S5 | list empty state (`--scenario empty`) at 2× | Glyph tile, headline, wrapped sub-copy, CTA and tip stacked on fixed 26/19/36 px steps; the tip ran off the pane. | 800×900 2× | `snippets/before/empty-2x.png` → `snippets/after/empty-2x.png` | fixed — every line steps on its measured box, the CTA grows with its label, the tip is elided to the list width (shared `elide_to_width`) |
| S3 | `--scenario narrow` ignores `--viewport` | — | — | — | ok — documented behaviour, unchanged |

### disks (`apps/disks/src/app.rs`)

| id | surface | defect | viewport/theme | before → after | status |
|---|---|---|---|---|---|
| D4 | every label | 72 literal sizes (`10.5`, `11.0`, … `22.0`). | — | `disks/before/wide.png` → `disks/after/wide.png` | fixed — a renderer-relative `TypeRamp` (theme tokens ÷ renderer scale, refreshed every `draw`; paint-only) feeds every `txt*`/`measure_weighted` call: 10.5/11 → `xs`, 11.5–13 → `sm`, 13.5–15 → `base`, 17/18 → `lg`, 19 → `xl`, 22 → `x2l`. The two remaining `11.0` literals are radii. 1× layout matches BEFORE with the tokenised sizes |
| D2 | drive cards, detail header, action band, tabs, top bar at 2× | Names wrapped over the meta line and bar; header meta and chips overlapped; 96/94/40/60/30 px bands fixed. | 800×900 2× | `disks/before/large-text.png` → `disks/after/large-text.png` | fixed — the renderer's text scale `k` is a widget field set in `Widget::measure` and `draw` (the layout is computed without a theme on the event path) and passed into the associated painters; every 1×-designed band and its inner offsets scale by it; drive model and header meta elide (shared weighted elide) instead of wrapping; header chips are placed after the previous chip's measured width and wrap their measured line; the scope tag and its detail centre on the measured band; tab and action-button glyph budgets scale with the labels |
| D2a | header meta elides the serial | The full S/N must stay reachable. | 2× | — | fixed — the drive's accessibility list item now carries `S/N …` in its description (kind; size; status; S/N) |
| D5 | Overview / SMART / Partitions tab internals at 2× | Partition map, legend, filesystem rows and SMART tiles keep their 1× pitches (146/78/56/38/24 px). | 800×900 2× | `after/large-text.png` (behind the overlay flap) | open — same `k`-scaling pattern, tab by tab; out of this round's scope |
| D1 | `CARD_INSET` 14 | Off-scale, named; still a constant not a spacing token. | — | — | ok — decision: keep (round-one record stands) |

### calculator (`apps/calculator/src/{draw.rs,graph2d.rs,graph3d.rs,app.rs,keypad.rs}`)

| id | surface | defect | viewport/theme | before → after | status |
|---|---|---|---|---|---|
| C5 | Graph 2D/3D function rows at 2× | `y =` / `z =` prefix in a fixed 26 px slot overran the field. | 800×900 2× | `calculator/before-2d/large-text.png`, `before-3d/large-text.png` → `after-2d/…`, `after-3d/…` | fixed — prefix measured in `measure_cards` (paint path) and cached for `layout` (event path); the field starts past it |
| C6 | panel header and info block at 2× | Title/subtitle and the three read-out lines overprinted (fixed 24/20 px bands). | 2× | same | fixed — `header_band()` from measured lines feeds both the painter and `layout`'s `list_top`; info lines stack on measured boxes |
| C7 | custom-painted text on the live desktop | `draw::set_text_scale(theme.text_scale)` multiplied the literal ramp *and* the renderer multiplied again on the live `App` path (the shots renderer is left at 1×, which hid it). | live 2× | (shots could not show it; the round-one C2 test set both scales and only checked widths) | fixed — `draw::paint_scale(tr, theme)` = theme scale ÷ renderer scale (1.0 live, 2.0 headless), used by every `set_text_scale`/`measure_cards` call; `text_in` centres on the measured line, not `size × 1.25` |

### logs (`apps/logs/src/app.rs`)

| id | surface | defect | viewport/theme | before → after | status |
|---|---|---|---|---|---|
| L4 | whole app at 2× | Tokens through the legacy API: 4× glyphs; 30 px rows, 34 px controls, 26 px header row, 30 px dropdown rows fixed; dropdown caption over its value. | 800×900 2× | `logs/before/large-text.png` → `logs/after/large-text.png` | fixed — 45 sites through `ts`; per-frame `Metrics` (row, list header, control, header, item, `line_per_px`, `k`) measured theme-lessly in `Widget::measure` and `draw`, read by `regions()` (layout and hit-testing) and the painters (associated painters receive it); `vcenter`/`line_h` are `Metrics` methods on the measured line; glyph-budget widths (search, triggers, chips, header controls, detail key column ≤ 45 % of the pane) scale by `k`; one `narrow_at(w)` predicate in 1× space for layout, paint and the event path |
| L4a | row columns at 2× | `column_x` fixed px: TIME/LEVEL/MESSAGE overlapped. | 2× | same | fixed — `column_x(row, k)` scales the time/level/unit budgets and judges compact in 1× space; header and cells share it |
| L4b | detail pane at 2× | Title/timestamp/message steps on `size × 1.5/1.6`; 22 px field rows. | 2× | same | fixed — steps on measured lines |
| L3 | `--actions` in a wide window | Actions menu anchored at the window origin: the compact trigger does not exist wide (also reachable live by opening the menu narrow and widening). | 1200×800 | `logs/before-actions/wide.png` → `logs/after-actions/wide.png` | fixed — the sheet anchors where the trigger would sit, under the header's trailing controls |
| L6 | list empty / error state (`--error`) at 2× | "Journal unavailable", its subtitle and "Use Refresh to try again." stacked on fixed `cy − 24 / + 2 / + 26` offsets and overprinted. | 800×900 2× | `logs/before-error/large-text.png` → `logs/after-error/large-text.png` | fixed — the three lines stack on their measured boxes around the centre (the 1× offsets are exact) |
| L5 | dropdown rows, filter chips | Hand-rolled dropdown and chip row rather than the shared `ContextMenu`/`Dropdown`. | — | — | open — migration to the shared menu (as monitor did in round one) is a larger change than an alignment fix; its accessibility tree (`logs.*-choice` targets) would need re-homing |

### typst_editor (`apps/typst_editor/src/app.rs`)

| id | surface | defect | viewport/theme | before → after | status |
|---|---|---|---|---|---|
| T3 | toolbar at 2× | Left action group ran into the zoom cluster; Export dropped at `width < 1120` with no overflow; 28 px pills clipped 2× labels; 46 px bar fixed. | 800×900 2× | `typst_editor/before/large-text.png` → `typst_editor/after/large-text.png` | fixed — bar and pills grow by the pill label's measured growth (measured in `draw`, which re-runs `layout_in` when it changes, so the first frame is coherent); the compact breakpoint is judged in 1× space (`width / k`); in the wide bar the right group is placed first and the action group folds trailing actions into a `⋯` kebab only when they would collide — Export PDF is always a pill or a menu row; the menu anchors to the kebab that opened it and closes when no kebab is shown |
| T4 | status bubbles at 2× | Bottom-corner bubbles sit tight around their 2× line. | 2× | `after/large-text.png` | open — `ui::status_bubble::pill_size` height contract (needs F4, coordinator) — same as H6 |
| T1/T2 | toolbar pills, divider, Problems panel at 1× | — | 1200 / 800 / 480 | `after/{wide,compact,narrow}.png` = BEFORE | ok — 1× unchanged; Export PDF still a pill at 1200 |

### terminal (`apps/terminal/src/{main_body.rs,main.rs,tests.rs}`)

| id | surface | defect | viewport/theme | before → after | status |
|---|---|---|---|---|---|
| X3 | cell grid at 2× | `font_size_base` (already scaled) through the legacy API: 4× glyphs; `char_h = size × 1.25` pitched rows for 2× so glyphs overlapped rows. | 800×900 2× | `terminal/before/large-text.png` → `terminal/after/large-text.png`, `after/tui-2x.png`, `after/shell-2x.png` | fixed — one renderer-relative size for `measure_font`/`draw_*`; `char_h` is the measured line box (`cell_h`); `build_layout` caches the theme's base size so `Widget::measure` (renderer, no theme) converts it and sizes the first grid — the one the PTY is resized to — from the real 2× cell; `measure` skips while a test has `force_geometry`'d a synthetic cell |
| X7 | context menu at 2× (`--scenario empty-menu`) | The menu opened through the bare `open_at` path: a fixed 1× sheet (constant width, 1× rows) under 2× glyphs, so the labels overprinted and spilled past the card. | 800×900 2× | `terminal/before/empty-menu-2x.png` → `terminal/after/empty-menu-2x.png` | fixed — opened with the shared primitive's `open_anchored` inside the terminal's viewport, so it lays out from its measured labels on the next draw (2× sheet, 2× rows) and still flips/clamps at the edges; 1× unchanged (`after/empty-menu.png`) |
| X5 | empty-state watermark, disconnected status | Token sizes double-scaled; status card height `size × 1.7`. | 2× | `after/ended-2x.png` | fixed — renderer-relative sizes; the card wraps the measured line plus padding |
| X4 | `--scenario ended` | Spawned `/bin/true`; `portable-pty` spawns the literal path and macOS ships `true` only at `/usr/bin/true`, so it failed with ENOENT. | — | `terminal/before/capture.log` (error) → `terminal/after/ended.png` | fixed — `/bin/sh -c "exit 0"`, the one POSIX-guaranteed path; the same change makes `exited_pty_publishes_a_persistent_disconnected_accessibility_state` (which spawned `/bin/true`) pass on this host |
| X6 | canned `tui` scenario at 2× | The canned VT bytes position a fixed 28-column status bar; the glyphs and pitch are consistent, the content is the scenario's own. | 2× | `after/tui-2x.png` | ok — scenario content, not layout |

## Fresh eyes at 800×700, 480×900 and 200 %

Every app's `compact.png`, `narrow.png` and `large-text.png` were inspected
before and after. Beyond the items above, no new alignment defect was found
at 800×700 or 480×900; at 200 % the remaining collisions are the two `ui`
contract items (M8, H6/T4) and the disks tab internals (D5).

## Open — needs another unit

- **F4 (coordinator at integration)** — `ui/src/widget/modal_dialog.rs`
  fills `max_h` instead of measuring its body (M8);
  `ui/src/status_bubble.rs::pill_size` sizes its height from
  `text_size + 16` rather than the measured line (H6, T4). Call sites in
  this unit pass renderer-relative sizes and are ready for the fix.
- **Consolidation (coordinator at integration)** — `ts()` (monitor `charts::ts`,
  hex, logs, disks `TypeRamp::resolve`, terminal inline), `line_box` /
  `line_growth` (monitor, hex, snippets, logs `Metrics::line_h`), and
  calculator's `paint_scale` all patch the same toolkit-level double
  scaling; one `TextRenderer::renderer_input_size` / `line_box` /
  `line_growth` in `ui` is the right depth (`ui/src/text_metrics.rs`).
- **Ratchets** — the new module-level whole-number pixel constants
  (calculator `PREFIX_X`/`PREFIX_GAP`/`PREFIX_W_1X`/`HEADER_H_1X`, snippets
  `ROW_TITLE_FONT`/`ROW_SUB_FONT`/`CONTROL_FONT`/`BUTTON_FONT`) will move
  `ui/tests/f1_spacing_token_baseline.txt`; this unit may not edit `ui/tests`,
  so the coordinator takes the baseline delta at integration.

## Tests

Full suites on the final tree (`cargo test --locked --no-default-features -p <pkg>`):

| package | result |
|---|---|
| monitor | 99 passed (97 + 2 new) |
| hex_editor | 55 lib (1 extended) + 3 `open_save` passed |
| snippets | 68 passed |
| disks | 62 passed |
| calculator | 143 passed |
| typst_editor | 45 passed |
| terminal | 109 lib (1 previously failing on macOS now passes) + 2 + 1 integration passed |
| logs | 38 passed, 1 failed: `journal::tests::export_preserves_a_non_utf8_portal_path` — pre-existing, platform: it writes a filename containing byte `0xff`, which APFS rejects; `journal.rs` is untouched by this branch |

Compile gate (`cargo check --locked --no-default-features -p <pkg> --tests`):
all eight exit 0; the only warning is the pre-existing terminal
`send_middle_press is never used`.

Code review (`/code-review medium`) and the coordinator's three review
angles raised fourteen findings; all were fixed before the commit (kebab
anchoring and menu reset, disks tab/chip/tag geometry, one logs narrow
predicate, monitor strip growth and the 1× fold threshold, dead
`show_toggle`, hex event-path `title_growth`, terminal forced-cell guard and
comment rationale, field-based scale instead of thread-local layout state in
disks and logs, shared `elide_to_width_weighted` instead of private loops,
the disks serial in the accessibility description). The reuse and ratchet
items are recorded above for the coordinator.

Every cargo invocation ran as `RUSTFLAGS="-C force-frame-pointers=yes
--remap-path-prefix=/build=." RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G
CARGO_BUILD_JOBS=1 PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig
cargo … --locked --no-default-features`; no cargo config was edited.

## Limits

- macOS: the `shots` binary, `qos-control` and the headless compositor are
  Linux-only; evidence is each app's own `--shots`/`--responsive-matrix`.
- Terminal `shell`/`shell-select` scenarios spawn the host's real shell, so
  their content is host-dependent; the canned `tui`, `colors`, `scrollback`
  and `ended` scenarios are the deterministic evidence.
- Static captures prove geometry, not live pointer routing; the monitor and
  hex regressions assert the production hit/layout rects against painted
  frames instead.
- The `calculator` live-path 4× (C7) is by construction invisible to the
  headless shot (its renderer is left at 1×); the fix is asserted by the
  ratio in `paint_scale` and inspected on the paint path in review, not in
  a capture.
