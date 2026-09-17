# A2 — Developer tools II & utilities: UI alignment and spacing review

Unit A2 of the 2026-09-16 all-app alignment review: `hex_editor`, `logs`,
`typst_editor`, `terminal`, `snippets`, `monitor`, `disks`, `calculator`.
Branch `ui-review/a2-devtools-2-utilities`, rebased onto `codex/macos-shell`
(`087d5a62c`) plus the shared `ui-review/lockfix` cherry-pick.

Capture root: `target/ui-review/<app>/{before,after}/` (dark/light responsive
matrix at 390/768/1366/1920, `narrow.png` 480×900, `compact.png` 800×700,
`wide.png` 1200×800, `large-text.png` 800×900 at `--text-scale 2`, plus every
`--scenario`). Every PNG referenced below was opened and inspected.

## Method

1. Read `ui/HIG.md` §1, §7, §7a, `ui/MANUAL_LAYOUT.md`, `ui/FORM_COMPOSITION.md`,
   `docs/hand-rolled-ui-audit.md`, the 2026-09-15 review-1/2/3 + corrected
   reviews, the closure register and the text-geometry / composition plan tasks
   for these apps (8, 11, 14, 15, 17, 21; composition 6).
2. Source pass over every layout file: private spacing constants, fudge
   offsets, header/cell parity, text centred on font size vs measured line
   height, primitive geometry duplicated in an app.
3. BEFORE captures of all eight apps on the corrected base; visual pass.
4. Fixes with measured text, shared-primitive geometry and one description per
   surface; one regression test per fix; `cargo check -p <pkg> --tests` gate.
5. AFTER captures of the five edited apps; side-by-side inspection.

## Findings

Status legend: **fixed** (in this branch, AFTER capture inspected) · **open**
(recorded, with owner) · **ok** (checked, no defect).

### monitor (`apps/monitor/src/app.rs`, `layout.rs`)

| id | surface | defect | viewport/theme | source | before → after | status |
|---|---|---|---|---|---|---|
| M1 | process table header | In tree mode every NAME cell starts after a 16px twisty gutter but the `NAME` header sat at the column edge, 16px left of the cell text. | 1200×800 dark, all widths | `app.rs` `draw_table_header` | `monitor/before/tree-expanded.png` (header x=89, cells x=105) → `monitor/after/tree-expanded.png`, `after/wide.png`, `after/narrow.png` (both x=105) | fixed |
| M2 | Resources › Per-Core Utilisation | 8 threads on a 7-column-wide card painted as 7 + 1 orphan gauge; 24 threads as 7/7/7/3. | 1200×800 dark | `layout.rs` `per_core_grid` | `before/resources.png`, `before/resources-many.png` → `after/resources.png` (4×2), `after/resources-many.png` (6×4) | fixed |
| M3 | row context menu | App mirrored `ContextMenu`'s private constants (`CONTEXT_MENU_W/ROW_H/SEP_H/PAD`) for its own on-screen nudge, item hit rects and a throwaway taffy pass; any change in the primitive would silently desync paint and hit. | 1200×800 dark, `--scenario context-menu` | `app.rs` `open_context_menu`, `context_menu_item_rects` | `before/context-menu.png` (220px mirrored sheet) → `after/context-menu.png` (primitive's own anchored, measured sheet) | fixed — now `ContextMenu::open_anchored` + `item_rect`; constants deleted |
| M4 | table cells, toolbar count label, detail placeholder | Text centred on the nominal font size with `-1.0` / `-8.0` nudges instead of the measured line height (header row already measured). | all | `app.rs` `draw_table_row`, `draw_table_row_name`, `draw_toolbar_count_label`, `draw_detail` | `before/wide.png` → `after/wide.png`, `after/empty.png` (sub-pixel at 1×; scale-proof) | fixed |
| M5 | summary strip | Label (xs) and bold value (sm) aligned by top with a `-1.0` nudge; now share one measured centre line. | all | `app.rs` `draw_summary_strip` | `before/wide.png` → `after/wide.png` | fixed |
| M6 | whole app at 2× | Token type sizes are passed to the legacy renderer API, which scales again (≈4× glyphs) — rows, pills and clock collide. | 800×900 `--text-scale 2` | every `draw_text*` call with `theme.tokens.typography.*`; needs `TextRenderer::renderer_input_style` per text-geometry task 11 | `before/large-text.png` = `after/large-text.png` | open — text-geometry task 11 (app) on top of F2's scale contract |
| M7 | `OUTER_PAD`/`GAP` = 14, `CORE_GAP` = 14, `TOOLBAR_PILL_GAP` = 10 | Off the 4px scale but named and used consistently; the reference layout is drawn to them. | — | `app.rs:168-190`, `layout.rs:33,130` | — | ok — recorded as deliberate off-scale values, unchanged |
| M8 | kill-confirm dialog | Large empty band between message and buttons is the shared `ModalDialog` minimum height. | 1200×800 | `ui::widget::ModalDialog` | `before/kill-confirm.png` | open — needs F4 |

### hex_editor (`apps/hex_editor/src/view.rs`)

| id | surface | defect | viewport/theme | source | before → after | status |
|---|---|---|---|---|---|---|
| H1 | byte grid (offset gutter, hex cells, ASCII cells) | Cell pitch, gutter width and 20px row height were constants derived from an assumed 0.6 em advance; at 2× the renderer doubled the glyphs while layout, paint and hit-testing kept the 1× pitches — overlapping bytes and clicks landing on the wrong byte. | 800×900 `--text-scale 2` (and any scale) | `GridMetrics` consts, `fit_bytes_per_row`, `hit_test`, `draw_grid` | `hex_editor/before/large-text.png` → `hex_editor/after/large-text.png`; 1× `before/wide.png` = `after/wide.png` (20px rows, same pitch) | fixed — `CellMetrics::measure` (one measured advance + line height) feeds one `GridMetrics` used by layout, paint and hit-test |
| H2 | column header (`Offset`, `00 01 …`, `ASCII`) | Labels used the `xs` token (double-scaled) while the cells they annotate used the base cell font, so the header drifted off the byte pitch at 2×; header band fixed at 20px. | 2× | `draw_grid_headers`, `GridMetrics::new` | `before/large-text.png` → `after/large-text.png` | fixed — caption size is a ratio of the cell font; band height from the measured line height (still 20px at 1×) |
| H3 | header pills `OVR` `BE` `24/row`, title, byte count | Text centred on font size with a `+0.5` nudge. | all | `draw_pill`, `draw_header` | `before/wide.png` → `after/wide.png` | fixed — measured line box |
| H4 | title bar at 2× | Title (`lg` token) and pills (`xs` token) double-scale into each other; the 44px bar is fixed. | 2× | `draw_header`, `GridMetrics::new` (`header_bottom`) | `after/large-text.png` | open — text-geometry task 14 (token normalisation) |
| H5 | inspector rows | Zebra band drawn at `y - 3 .. y + row_h - 5` (fudged offsets around a 22px row), `label_w` 72 fixed. Reads correctly at 1×. | 1× | `draw_inspector` | `before/wide.png` | open — cosmetic; would follow the same measured-row pattern as H1 |

### snippets (`apps/snippets/src/app.rs`)

| id | surface | defect | viewport/theme | source | before → after | status |
|---|---|---|---|---|---|---|
| S1 | list search field vs floating hamburger | With the sidebar collapsed/overlaid the 34px toggle painted over the first 32px of the search field and its placeholder (`≡arch · try language:rust`); the shared `TextInput`'s external layout and the `FocusSearch` hit rect started under it. Toggle centre was 1px above the field's. | 800×700, 700×800, 480×900, 2× | `draw_list` search rect, `draw_toggle` | `snippets/before/compact.png`, `before/large-text.png`, `before/narrow-700.png` → `snippets/after/compact.png`, `after/large-text.png`, `after/narrow-700.png` | fixed — one `toggle_rect(band)`; the search rect (and therefore `apply_layout_external` + hit region) starts past it and both centre on `SEARCH_ROW_H` |
| S2 | list rows / detail at 2× | Title, subtitle and `n vars` collide (fixed 64px rows). | 2× | `ROW_H`, `draw_list` rows | `after/large-text.png` | open — composition task 6 (snippets row metrics) |
| S3 | `--scenario narrow` | The scenario ignores `--viewport`; it needs a sub-breakpoint viewport passed explicitly (captured as `narrow-700.png`/`narrow-480.png`). | — | `main.rs` | — | ok — documented behaviour |
| S4 | docked sidebar collapse/expand animation | The floating toggle pops in/out at `reveal() ≥ 0.999` (pre-existing); with S1 the search field's leading inset now follows the toggle, so it steps 44px on the same frame instead of easing. Code-review finding. | wide, during the sidebar ease | `show_toggle`, `draw_list` | — | open — the toggle's own appearance needs an eased reveal (F1/F6 sidebar transition); the field should ease with it |

### disks (`apps/disks/src/app.rs`)

| id | surface | defect | viewport/theme | source | before → after | status |
|---|---|---|---|---|---|---|
| D1 | identity header vs action band vs drive rows | Header text inset 20px and radius 14; the action band directly below inset 14 / radius 12; drive rows inset 14 / radius 12 — three stacked/adjacent cards, two insets, two radii. | 1200×800 dark & light | `draw_header`, `draw_action_bar`, `draw_drive_row`, `compute_action_bar_layout` | `disks/before/wide.png` (title x=376, WHOLE DISK x=369) → `disks/after/wide.png`, `after/light/dev.qos.disks-laptop.png` (both x=370) | fixed — one named `CARD_INSET` (14, the drive rows' existing value; off-scale and recorded) and `theme.tokens.radii.lg`. Code review notes the inset is still a constant, not a `spacing` token, so it will not follow `ui_scale`; disks' `Layout` is computed without a `Theme`, and every other disks metric (`PAD` 18, `CARD_H` 96, …) is literal too — the token migration belongs to D4. |
| D2 | drive cards / header at 2× | Names wrap over the meta line and usage bar; header meta and chips overlap. | 800×900 `--text-scale 2` | fixed 96px card rows (`CARD_H`), `HEADER_H` | `before/large-text.png` | open — text-geometry task 8 |
| D3 | 2× capture fidelity | `main.rs` now applies `--text-scale`; the closure register's fidelity note is closed on this base. | — | `main.rs` | `before/large-text.png` | ok |
| D4 | text sizes | Every label is a literal size (`22.0`, `13.0`, `10.5`, …) rather than a type token. | — | throughout `app.rs` | — | open — token migration, out of this unit's alignment scope |

### calculator (`apps/calculator/src/{app.rs,draw.rs,mathfield/mod.rs}`)

| id | surface | defect | viewport/theme | source | before → after | status |
|---|---|---|---|---|---|---|
| C1 | Graph 3D surface list | Inactive rows scrolled to their caret (parked at the end), so long formulas showed a left-clipped tail (`)·05 · (x² + y²) − 3`). | 1200×800 | `MathField::draw`, `relayout` | `calculator/before-3d/wide.png` → `calculator/after-3d/wide.png` | fixed — only the focused field follows its caret; a row that loses focus snaps to its start, an unfocused row keeps (clamps) its wheel scroll. The first cut zeroed the scroll on every unfocused frame, which would have undone `on_wheel` on inactive rows — caught in code review, fixed, and asserted in the test. |
| C2 | topbar mode switcher + RAD/DEG | Segment widths clamped to 46–128px and the toggle to 100/124px regardless of label width; at 2× `Calculator`/`Graph 2D`/`Graph 3D` overprinted each other. | 800×900 `--text-scale 2` | `relayout`, `draw_topbar` | `calculator/before/large-text.png`, `before-empty/large-text.png` → `after/large-text.png`, `after-empty/large-text.png` | fixed — labels measured on the paint path each frame; clamps grow with them; 1× geometry unchanged (asserted) |
| C3 | keypad grid | Equal 8px gaps, equal cell widths per column count, labels centred on the 1.25 line box via `draw::text_in`. | 480 / 800 / 1200 | `keypad.rs` | `before-empty/{narrow,compact,wide}.png` | ok |
| C4 | display | Expression and result right-aligned with a consistent 16px inset; status line left/right on one row. | all | `keypad.rs` `draw_display*` | `before-empty/wide.png` | ok |
| C5 | Graph 2D/3D function rows at 2× | `y =` prefix and formula field overlap; card widths fixed. | 2× | `graph2d.rs:275` region, `graph3d.rs` | `after/large-text.png` | open — text-geometry task 17 |

### logs (`apps/logs/src/app.rs`)

| id | surface | defect | viewport/theme | source | before | status |
|---|---|---|---|---|---|---|
| L1 | table header vs rows | Header and rows both use `column_x`; TIME/LEVEL/UNIT/MESSAGE align with cells at every width. | 1200 / 800 / 480 | `draw_list_frame_and_header`, `column_x` | `logs/before/{wide,compact,narrow}.png` | ok |
| L2 | filter bar | Search, Unit/Boot dropdowns and the severity chips share one 34px centre line; chips stretch evenly when wrapped. | all | `filter_bar_regions` | `before/{wide,compact,narrow}.png` | ok |
| L3 | `--actions` in a wide window | The Actions overflow menu anchors at the window origin because its trigger only exists in the narrow header. Shots-only state, unreachable by a user. | 1200×800 | `dropdown_regions` | `before/actions.png` | ok — recorded |
| L4 | whole app at 2× | Fixed 30px rows and token sizes double-scaled; rows and chips collide. | 2× | `ROW_H`, filter bands | `before/large-text.png` | open — text-geometry task 15 |

### typst_editor (`apps/typst_editor/src/app.rs`)

| id | surface | defect | viewport/theme | source | before | status |
|---|---|---|---|---|---|---|
| T1 | toolbar | Pills 28px on a 46px bar, labels centred on the 1.25 line box; zoom cluster and Problems on the same line. | 1200 / 800 / 480 | `draw_toolbar_*`, `paint_pill` | `typst_editor/before/{wide,compact,narrow}.png` | ok |
| T2 | split divider, status bubbles, Problems panel | Divider grip centred; bottom-corner bubbles inset 14px; diagnostics rows aligned (`5:28` column, message column). | 1200×800 | `draw_divider`, `draw_status_bubbles`, `diagnostics.rs` | `before/{wide,problems,errors}.png` | ok |
| T3 | toolbar at 2× | Left action group runs into the zoom cluster; Export PDF drops without an overflow. | 800×900 `--text-scale 2` | `draw_toolbar_full` (fixed 1120/760 thresholds) | `before/large-text.png` | open — composition task 6 (typst overflow) |

### terminal (`apps/terminal/src/main_body.rs`)

| id | surface | defect | viewport/theme | source | before | status |
|---|---|---|---|---|---|---|
| X1 | cell grid vs selection vs cursor | `char_w` is the measured monospace advance and `char_h = ceil(font × 1.25)`; rows, the selection band and the block cursor all floor to the same row grid. | 1200×800, 480×900 | `draw` (measure), `draw_cell_rows`, `draw_selection_highlight`, `draw_block_cursor` | `terminal/before/{shell,shell-select,tui,colors,scrollback}.png` | ok |
| X2 | context menu | Shared `ContextMenu` with measured rows. | `--scenario empty-menu` | — | `before/empty-menu.png` | ok |
| X3 | 2× | Glyphs are 4× (theme token + renderer scale); cell metrics follow the glyphs so the grid stays coherent, but the magnitude is wrong. | 2× | `main_body.rs:3591` region | `before/large-text.png` | open — text-geometry task 21 / F2 |
| X4 | `--scenario ended` | Fails on macOS: it spawns `/bin/true`, absent on Darwin. | — | `main.rs` | `terminal/before/capture.log` | open — platform limit, not a layout defect |

## Conventions observed

- Every app in this unit is one full-bleed custom widget; alignment lives in
  private `const` blocks and `Rect::new` arithmetic in `draw`. Header/cell
  column parity is generally good (logs, monitor except M1, hex).
- The dominant 2× defect across all eight apps is the same one: theme
  typography tokens (already resolved logical sizes) are passed to the legacy
  renderer API, which scales them again. `TextRenderer::renderer_input_style`
  exists on this base but none of these apps use it yet — that is the
  text-geometry plan's per-app work, not an alignment fix, so it is recorded
  as open above rather than patched around.
- Text was widely centred on the nominal font size (`(h - size) / 2`) with
  ±0.5/1px nudges; the toolkit's `Button` now centres on the measured line
  box. Where this unit touched a surface it moved to measured centring; the
  remaining call sites are listed per app.
- Off-scale spacing that was kept: monitor 14px pad/gap and 10px pill gap,
  disks 14px card inset, snippets 10px toggle inset — each is now (or was
  already) a named constant.

## Tests

Focused regressions added (all pass):

- `monitor`: `layout::tests::per_core_grid_balances_rows_instead_of_orphaning_the_last_core`
- `hex_editor`: `view::tests::grid_pitches_follow_the_measured_mono_metrics_and_hit_their_cells`
- `snippets`: `app::tests::collapsed_sidebar_toggle_and_search_field_share_a_row_without_overlap`
- `calculator`: `app::tests::topbar_controls_seat_their_measured_labels_at_large_text`,
  `mathfield::tests::unfocused_field_shows_its_start_while_the_focused_field_follows_the_caret`
- `disks`: `app::tests::detail_cards_share_one_left_edge_and_inner_inset`

Focused runs (`cargo test --locked -p <pkg> --lib <filter>`): monitor
`per_core` 4/4, `context_menu` 5/5, `toolbar` 4/4; hex_editor `grid_pitches`
1/1; snippets `collapsed_sidebar` 1/1; calculator `topbar` 4/4,
`unfocused_field` 1/1; disks `detail_cards` 1/1.

Full-suite and compile-gate results are recorded in the "Final gate" section.

Every cargo invocation ran as
`RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." cargo <…> --locked`.
The `RUSTFLAGS` prefix is required on this host: the worktree lives under the
main checkout, whose `.cargo/config.toml` cargo merges in, appending
`-C target-cpu=generic` for aarch64 and breaking `ring`'s CPU const-assert.
The env var replaces the merged flag list with the repo's own macOS set; no
cargo config was edited. The tests were executed, not only type-checked.

Code review (`/code-review medium`) raised five findings: two fixed in code
(the unfocused `MathField` scroll reset that would have undone wheel
scrolling; a dead `.clamp` in `per_core_grid`), one test assertion dropped as
tautological (disks D1 test), and two recorded as open (S4; the `CARD_INSET`
constant vs. a spacing token, see D1/D4).

## Final gate

Full suites on the final tree (`cargo test --locked -p <pkg>`):

| package | lib tests | integration | result |
|---|---|---|---|
| monitor | 97 passed | — | ok |
| hex_editor | 55 passed | `open_save` 3 passed | ok |
| snippets | 68 passed | — | ok |
| disks | 62 passed | — | ok |
| calculator | 143 passed | — | ok |

Compile gate (`cargo check --locked -p <pkg> --tests`): hex_editor, logs,
typst_editor, terminal, snippets, monitor, disks, calculator — all exit 0.
Pre-existing warning, not from this diff: `terminal` (lib test) `function
`send_middle_press` is never used`.

logs, typst_editor and terminal were not modified; their suites were not run
beyond the compile gate.

## Limits

- **Base correction.** The worktree was first cut from `origin/main`
  (`24ee08c66`); the coordinator redirected it onto `codex/macos-shell`
  (`087d5a62c`). All captures and findings above are from the corrected base;
  the earlier captures were discarded.
- **macOS build environment.** The parent checkout's `.cargo/config.toml`
  leaks `target-cpu=generic` into this worktree's cargo config (cargo merges
  parent-directory configs), which breaks `ring`; every cargo command in this
  review therefore ran with `RUSTFLAGS="-C force-frame-pointers=yes
  --remap-path-prefix=/build=."` (the worktree's own macOS flags) and
  `CARGO_BUILD_JOBS=1`. No config file was edited.
- **Terminal** was reviewed from captures and source only; its `ended`
  scenario cannot run on macOS (X4). No terminal source was changed.
- **2× evidence** is truthful on this base (every shot handler applies the
  requested scale) but confounded by the shared double-scale contract (M6, H4,
  L4, S2, T3, X3, C5, D2): the collisions are real, their magnitude is 4×
  rather than 2×.
- Static captures prove geometry, not live pointer routing; the snippets and
  hex regressions assert the production hit/layout rects instead.
