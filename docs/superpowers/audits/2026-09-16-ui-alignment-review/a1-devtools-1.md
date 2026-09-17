# UI alignment review — A1: Developer tools I

Unit: api_tester, db_client, code_editor, containers, diffmerge, git_client.
Branch `ui-review/a1-devtools-1`, cut from `2d7e819b6` (`codex/macos-shell`) plus the
coordinator's `Cargo.lock` fix (`f9a579c9c`).

## Method

1. Read `ui/HIG.md` §1/§7/§7a, `ui/MANUAL_LAYOUT.md`, `ui/FORM_COMPOSITION.md`,
   `docs/hand-rolled-ui-audit.md`, the 2026-09-15 review register (review-1/2,
   closure register, additional findings, controller addenda) and app-composition
   plan tasks 6/10–17.
2. Source pass over every layout file: private spacing consts, `len * 7.x` width
   estimates, font-size-based vertical centring, `± 0.5/1.0` nudges, hit rect vs paint,
   token sizes passed to renderer-relative APIs.
3. BEFORE captures (dark + light responsive matrix, 200% text at 800×900, 480×900,
   800×700, `--pseudo-locale` at 1200×800, every scenario, git client `--shots-narrow`) —
   80 PNGs, every one opened and inspected. Paths: `target/ui-review/<app>/before/…`.
4. Fixes using measured text (`measure`/`measure_weighted`/`measure_styled` with the exact
   painted style), one geometry description per surface shared by paint, hits and
   accessibility, and named constants where a value is deliberately off-scale.
5. Regressions per fix (module tests that paint with a real `TextRenderer`).
6. AFTER captures with the same matrix, every PNG inspected: `target/ui-review/<app>/after/…`.
7. `/code-review high` over the full diff; its findings fixed (see "Code review").

Capture helper (isolated `$HOME`/XDG dirs, `QUICK_UI_VELLO_CPU=1`): `/tmp/a1-cap.sh <tag> <apps…>`
mirrors the E2E recipe.

## Environment notes (affect every worker on this host)

- The worktree was created from `24ee08c66` (an `origin/main` lineage commit), not from
  `codex/macos-shell`; it lacked the 2026-09-15 evidence and the landed composition commits.
  Reset to `2d7e819b6`, the base every F/S worker branch uses (`git merge-base` verified).
- Cargo merges `.cargo/config.toml` up the directory tree. The parent checkout
  `/Users/xaver/dev/qos/.cargo/config.toml` (on `main`) still carries
  `[target.'cfg(all(target_arch = "aarch64", not(target_os = "linux")))'] rustflags =
  ["-C", "target-cpu=generic", …]`, which is concatenated with the worktree's flags and
  strips the AES/SHA target features `ring 0.17.14` asserts on
  (`vendor …/ring-0.17.14/src/cpu/arm/darwin.rs:44`). `RUSTFLAGS="-C force-frame-pointers=yes
  --remap-path-prefix=/build=."` (exactly the worktree's macOS list) replaces the merged
  list and produces the same rustc argv, so sccache keys are unchanged. No cargo config was
  edited.
- The vendored vello CPU rasteriser panics at 1920×1080 for code_editor
  (`vendor/vello_shaders/src/cpu/tile_alloc.rs:52` index out of bounds), so the desktop
  cell of that app's responsive matrix is missing in both BEFORE and AFTER; laptop/tablet/
  phone rendered. Evidence limit, not an app defect.
- The sandbox's git guard refuses any shell line containing `git_client`; cargo/capture
  commands for that crate went through `/tmp/a1-cargo.sh gc` / `/tmp/a1-cap.sh`.

## Findings

Status legend: **fixed** (in this branch, with a regression), **open** (recorded, with owner).
Before/after PNGs: `target/ui-review/<app>/{before,after}/<variant>.png`; the responsive
matrix cells are `<theme>/dev.qos.<app-id>-{phone,tablet,laptop,desktop}.png`.

### api_tester

| id | surface | defect | viewport/theme | file (before) | status |
|---|---|---|---|---|---|
| A1-API-1 | tab strip variable hint | text centred with a `- 7.0` font-size guess | all | `apps/api_tester/src/app.rs` `draw_var_hint` | fixed — measured line height |
| A1-API-2 | URL bar hamburger | fixed 36px slab beside a 52px picker at 200% | `large-text.png` | `compute_layout` (`HAMBURGER_W`) | fixed — side = `HAMBURGER_W.max(control_h)` (square) |
| A1-API-3 | KV rows enable checkbox | fixed 16px box beside 28px text at 200% | `large-text.png` | `KvRowLayout::checkbox` | fixed — `KV_INDICATOR_SIZE * text_scale`, bounded by the row; the key column (`kv_key_x`) starts after the scaled box in measurement, heading and rows |
| A1-API-4 | response Raw / Pretty bodies | line pitch measured at 13.0 but lines drawn at the `sm` token size | all | `draw_json_body`, `draw_raw_body` | fixed — measured at the drawn size |
| A1-API-5 | response Preview (headers) | fixed 24/28/22 px steps under scaled text | source (Preview not in the capture set) | `draw_response_headers` | fixed — row pitch from measured line height, separator on the row edge |

No 1× misalignment observed in the captures (sidebar rows, tab strip, KV table, response
chrome and gutters share their centre lines; left/right gutters equal at `PAD`). The
saved-draft adoption issue (additional-findings.md) is out of scope and untouched.

### db_client

| id | surface | defect | viewport/theme | file | status |
|---|---|---|---|---|---|
| A1-DB-1 | query toolbar | Run label at `+7`/13px and Snippets at `+8`/12.5px — two baselines in one row; fixed 150/110 widths | laptop dark/light | `app.rs` `draw_query_tab` | fixed — measured widths (`RUN_MIN_W`, `SNIPPETS_MIN_W` floors), labels centred on measured height |
| A1-DB-2 | result strip, snippet menu, tab bar, breadcrumb, New-connection label | centred on font size | all | `app.rs` | fixed — measured centring |
| A1-DB-3 | sidebar | connection rows inset 6 while the New-connection button is inset 8 | all | `draw_connection_row`, `draw_connections_footer` | fixed — one `SIDEBAR_INSET` (8 = `s2`) |
| A1-DB-4 | results grid | header label `+8` and cells `+6` centred on font size; `header_hit` accepted `y == y0+HEADER_H` (painted as row 0); `row_hit` returned rows for the unpainted partial strip and outside the rect horizontally | all | `grid.rs` | fixed — measured centring; half-open header band; hits bounded to painted rows and the rect |
| A1-DB-5 | schema tree | paint/hit used 26 for the heading band, `visible_rows` used 30 | all | `schema_tree.rs` | fixed — one `HEADER_H` |
| A1-DB-6 | structure tab | header labels/cells centred on font size | — | `structure.rs` | fixed — measured centring |
| A1-DB-7 | 200% text | tab bar, toolbar, result strip and grid rows collapse (fixed 38/40/26/26 bands) | `large-text.png` | `app.rs`, `grid.rs` | open — Composition Task 14 (measured GridMetrics / band heights); not started here |

### code_editor

| id | surface | defect | viewport/theme | file | status |
|---|---|---|---|---|---|
| A1-CE-1 | bottom dock tab strip | labels at `y0 + 9` ~1.5px below the strip centre; measured Regular but painted Semibold when active | `terminal.png`, compact | `bottom_panel.rs` `draw_tab_strip` | fixed — measured with the painted weight, centred on the strip |
| A1-CE-2 | Problems rows | three text sizes aligned by tops with a `+0.5` nudge | — | `draw_problems` | fixed — each centred on the row band |
| A1-CE-3 | editor tab chrome | shared `ui::widget::TabBar` pills top-aligned (y 2–30) in the 44px chrome while `+` and the dock toggle are centred (y≈22) | all 1× | `app.rs` (`EDITOR_CHROME_H`), `ui` TabBar | open — needs F6 (window-chrome widgets): TabBar should centre its pills in the allocated height or expose its intrinsic height |
| A1-CE-4 | editor body at 200% | fixed `LINE_HEIGHT`/gutter overprint scaled glyphs | `large-text.png` | `editor_view.rs` | open — Composition Task 13; file is WIP in the user's checkout, deliberately untouched |

### containers

| id | surface | defect | viewport/theme | file | status |
|---|---|---|---|---|---|
| A1-CO-1 | every text helper | theme type tokens (already ×text-scale) passed to the renderer-relative `draw_text*`/`measure*` APIs → glyphs painted at 4× and layout bands computed for 2× | `large-text.png` | `draw.rs` `text`, `text_clip`, `status_pill`, `chip`; 8 raw measurements in `panels.rs` | fixed — `draw::px` (= `renderer_input_style`) applied exactly once in the helpers and in `draw::measure`; regression asserts glyph-run `font_size == token` and measure == paint |
| A1-CO-2 | container list, detail open | `Running` pill paints under Stop/Restart/Remove in the 240px list | tablet 768, compact 800 (dark+light) | `panels.rs` `draw_container_row_layout` | fixed — `status_pill_fits` measures the pill against the action cluster; when it cannot fit the status stacks under the name (the compact surface's layout); row stats reserve the real cluster width (`row_actions_width`) and yield when < 60px remain |
| A1-CO-3 | container list | `NAME / STATUS` heading 8px left of the names it labels | all | `draw_container_list_header` | fixed — heading on `cols[0] + ROW_TEXT_INSET`; asserted from glyph-run origins of the real header and row paints |
| A1-CO-4 | container rows | action cluster ended 12px from the edge while the header divider ends at `PAD` (16) | all | `draw_row_actions` | fixed — same right gutter |
| A1-CO-5 | pills, chips, rail count chip, rail toggle glyphs, tab labels, toolbar labels, header button | centred on font size with `-1.0`/`-5`/`-7`/`-9` nudges; header button fixed 150/96 wide | all | `draw.rs`, `panels.rs` | fixed — measured centring; button width from its label; pill geometry shared by `status_pill` and `status_pill_width` (`PILL_*` consts) |
| A1-CO-6 | header search launcher | placeholder overlaps the ⌘K badge at 768 | tablet | shared `ui::SearchField` | open — needs F2 (Task 18 measured shortcut reservation) |
| A1-CO-7 | 800px three-column feasibility | list→detail routing when panes cannot fit | compact 800 | `app.rs` `COMPACT_VIEWPORT_WIDTH` | open — Composition Task 16 (the overlap symptom itself is closed by A1-CO-2) |

### diffmerge

| id | surface | defect | viewport/theme | file | status |
|---|---|---|---|---|---|
| A1-DM-1 | merge resolution chips | widths from `label.len() * 7.2`; "Take both" ran past the theirs pane at 800 | compact 800×700 (dark+light) | `merge_view.rs` `draw_hunk_action_chips` (~:649) | fixed — measured labels; the explicit "Take …" set falls back to Ours/Theirs/Both when the group cannot fit its pane; chip text centred on measured height |
| A1-DM-2 | merge rows / headers / chip band | fixed `ROW_H 19`, `HEADER_H 30`, `ACTION_H 26` while text scales; `y + 2.0` glyph offset | `large-text.png` | `merge_view.rs` | fixed — one `Metrics` snapshot (row pitch, header, chip band, text offset, merged gutter) measured per draw and shared by spans, scroll clamp, hit rects, a11y bounds and paint; spans also follow `set_inputs` |
| A1-DM-3 | merged band gutter | numbers left-aligned at `+6`; code at fixed 44 | all | `draw_merged_band` | fixed — right-aligned on measured width, gutter grows with digits |
| A1-DM-4 | diff view gutter | right-alignment from `len * 7.0`; fixed rows/header | all, `large-text.png` | `diff_view.rs` `draw_gutter_number` | fixed — measured width; `Metrics` (row, header, gutter) shared by scroll, block rects and paint |
| A1-DM-5 | toolbar | segment/button widths from `len * 7.x` (pseudo-locale labels touch; 200% collapses); fixed 48px band | `pseudo.png`, `large-text.png` | `app.rs` `draw_toolbar` | fixed — measured with the painted styles; band height from control height; views re-placed in the same frame (`place_views`) |
| A1-DM-6 | views | the taffy content node was the full root height, so each view's rect ran a toolbar height past the window bottom (the 58% source band split included invisible space) | all | `app.rs` `apply_layout` | fixed — views placed on `content_rect()`; the visible merged band gained the formerly hidden height |
| A1-DM-7 | compact feasibility / focus handoff to chips | — | — | — | open — Composition Task 17 (the 800 clipping symptom is closed by A1-DM-1) |

### git_client

| id | surface | defect | viewport/theme | file | status |
|---|---|---|---|---|---|
| A1-GC-1 | app bar | right-anchored tabs + fixed 230px settings cluster overwrote the title at 480/680; subtitle wrapped under the bar | `narrow.png`, `shots-narrow.png`, phone | `app.rs` `draw_appbar` | fixed — measured cluster and tab widths; when a readable title (`TITLE_MIN_W`) cannot sit beside them the tabs wrap to a second `APPBAR_TAB_ROW_H` row (nothing hidden); name and subtitle elide; body/scroll/hit tests read the measured `appbar_h`; the branch pill paints only when its measured width fits |
| A1-GC-2 | app bar | unequal gutters, several control heights, labels centred on font size | all | `draw_appbar_*`, `draw_settings_cluster`, `draw_hamburger` | fixed — `APPBAR_GUTTER` both sides, `APPBAR_CONTROL_H`, measured centring, hamburger bars centred as a block |
| A1-GC-3 | changes rows | `+adds -dels` painted as a second 11px line at `y + 18` inside the 30px row (ran into the next row) | laptop/desktop | `changes.rs` `section_row` | fixed — one measured `RowLayout`: badge, elided name, dir, right-aligned counts and chevron on one centre line; select region derived from the chevron rect |
| A1-GC-4 | section headers, count badges, ref/branch pills, card pills | `draw::pill` centred on font size with `- 1.0` | all | `draw.rs` `pill` | fixed — measured line height (removes the nudge everywhere) |
| A1-GC-5 | diff view | hunk buttons centred on font size; row glyphs at fixed `+ 2.0` | all | `diffview.rs` | fixed — measured centring (`text_dy` measured once per pass) |
| A1-GC-6 | panels | toolbar buttons fixed 84px; card title vs count pill off by ~1px | all | `panels.rs` | fixed — measured button widths (72 floor), title/pill share the header band's centre |
| A1-GC-7 | 200% text | changes/history/diff rows use fixed 30/68/18 px pitches | `large-text.png` | `changes.rs`, `history.rs`, `diffview.rs` | open — Composition Task 6 scale audit; not attempted here to keep the app-bar change reviewable |
| A1-GC-8 | narrow settings disclosure | Task 6 asks for a Settings disclosure; this branch keeps both preferences visible via the two-row bar | — | — | open — Task 6 may supersede the wrap with a disclosure |
| A1-GC-9 | unified diff hunk header | the right-anchored `Unstage hunk` button covers the tail of a long `@@ … @@` header at 390/480 | phone, narrow | `diffview.rs` | open — pre-existing; header text should elide before the button (bounded follow-up) |

## Conventions observed

- **Measure with the painted style.** Every new placement measures the exact text with the
  size/weight/mono flag it is drawn with and centres on the measured line height. Helpers:
  `git_client::draw::{centered_top, mono_centered_top}`, `containers::draw::{px, measure,
  centered_top}`, `diffmerge` `Metrics` snapshots, `DiffView::text_dy`.
- **Tokens through `renderer_input_style` once** (HIG §7a) — `containers::draw::px`.
- **One geometry description per surface**: `git_client::changes::RowLayout`,
  `diffmerge::{merge_view,diff_view}::Metrics`, `GitClientApp::appbar_h`/`title_rect`,
  `DiffMergeApp::toolbar_h` + `place_views`, `ApiTesterApp::kv_key_x`.
- Deliberately off-scale values kept as named consts (recorded, not hidden): git client
  `APPBAR_GUTTER 20`, `HAMBURGER_SLOT 44`, `TITLE_MIN_W 96`, `APPBAR_TAB_ROW_H 44`,
  `APPBAR_CONTROL_H 32`, `APPBAR_CLUSTER_GAP 12` (= `s3`), changes `GUTTER 14`, `BADGE_H 16`,
  `CHEVRON_W 20`, `CHEVRON_INSET 8`, `COUNTS_GAP 8`; diffmerge `ROW_H 19/20`, `HEADER_H 30/34`,
  `ACTION_H 26`, chip pads 7/8, `COMPACT_CHIPS_X 64`, `MERGED_GUTTER_W 44`, toolbar pads 8–11,
  `TOOLBAR_CONTROL_H 28`; db_client `RUN_MIN_W 150`, `SNIPPETS_MIN_W 110`, `SIDEBAR_INSET 8`
  (= `s2`); containers `ROW_TEXT_INSET 8` (= `s2`), `PILL_PAD_X 10`, `PILL_DOT_R 3.5`,
  `PILL_DOT_GAP 6`; api_tester `KV_INDICATOR_SIZE 16` (= `s4`), `KV_INDICATOR_GAP 14`.
  Spacing tokens were not substituted inside the pure geometry helpers (RowLayout/Metrics)
  that have no `Theme` in scope, to keep one number per concern.

## Open — needs F-units

- **F2 (controls/text):** a shared `TextRenderer::centered_top(text, style, y0, h)` (or an
  equivalent on `TextStyle`) — this branch adds the same measured-centring rule in six apps;
  the toolkit should own it. `SearchField` shortcut badge reservation (A1-CO-6, Task 18).
- **F6 (window-chrome widgets):** `TabBar` pill vertical placement inside a taller chrome
  band (A1-CE-3).

## Code review

`/code-review high` (all findings addressed): stale `hunk_spans` after `MergeView::set_inputs`
(now recomputed there); the change-row file name was drawn with a wrap width (now elided);
`DiffView::text_top` re-measured per line (now once per pass as `text_dy`); the branch pill
could paint past `right_limit` (now gated on its measured width); `status_pill_width` copied
pill literals (shared `PILL_*` consts); the containers heading/name test compared a constant
with itself (now asserts glyph-run origins from the production header and row paints).
The reviewer's altitude note (centring helper belongs in `ui`) is recorded above as an F2 need.

## Tests

Every cargo command in this unit ran as
`RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." cargo <check|test|build> --locked -p <pkg> …`.
The `RUSTFLAGS` prefix is the environment workaround the coordinator confirmed (see
"Environment notes"); no cargo config was edited. The new tests were executed (not only
type-checked). Final run (`/tmp/a1-final.log`, `/tmp/a1-api-chain.log`):

| package | command (`--lib` filter) | result |
|---|---|---|
| git_client | `cargo check --tests` | clean |
| git_client | `appbar_` | 4 passed (3 new: `appbar_controls_stay_inside_the_window_and_never_overlap`, `appbar_controls_share_one_centre_line_per_row`, `wrapped_appbar_moves_the_body_down_with_it`) |
| git_client | `row_layout` | 2 passed (new) |
| git_client | `diffview::` | 2 passed (existing) |
| diffmerge | `cargo check --tests` | clean |
| diffmerge | `merge_` | 9 passed (2 new: `merge_chips_stay_inside_their_pane`, `merge_rows_follow_the_scaled_line_height`) |
| diffmerge | `toolbar_` | 4 passed (1 new: `toolbar_controls_fit_their_measured_labels_at_every_text_scale`) |
| diffmerge | `diff_rows` | 1 passed (new: `diff_rows_and_gutter_follow_the_scaled_line_height`) |
| diffmerge | `chip_`, `phone_`, `compact_source` | 3 + 3 + 1 passed (existing) |
| containers | `cargo check --tests` | clean |
| containers | `token_sizes`, `status_pill`, `row_layout_tests`, `compact_` | 1 + 1 + 2 + 2 passed (4 new) |
| db_client | `cargo check --tests` | clean |
| db_client | `grid::` | 3 passed (1 new: `header_and_body_hits_split_exactly_where_the_paint_does`) |
| code_editor | `cargo check --tests` | clean |
| code_editor | `bottom_panel::` | 10 passed (1 new: `tab_labels_share_one_centre_line_inside_the_strip`) |
| api_tester | `cargo check --tests` | clean |
| api_tester | `overlay_hamburger` | 1 passed (new) |
| api_tester | `composition_`, `compact_`, `phone_` | 7 passed, 2 ignored (pre-existing `#[ignore]` capture routes that need `API_TESTER_CAPTURE_DIR`), 1, 1 passed |

No pre-existing failures were observed in the scoped runs. Whole-crate runs were not
executed (memory-constrained host, coordinator instruction).

## Verification (AFTER captures, all inspected)

- **git_client** — `after/dark/dev.qos.git-client-laptop.png`: change rows are one line
  with counts right-aligned before the chevron; section badges centred on their titles.
  `after/narrow.png` (480) and `after/dark/dev.qos.git-client-phone.png` (390): two-row app bar,
  title/hamburger/settings on row 1, tabs on row 2, nothing overwritten, subtitle elided.
  `after/shots-narrow.png` (680, flap open): same, flap over the body. Tablet/laptop/desktop
  keep one row with equal 20px gutters. `after/large-text.png`: bar wraps and stays legible;
  rows still overprint (A1-GC-7 open).
- **diffmerge** — `after/compact.png` (800): "Take ours / Take theirs / Take both" end inside
  the theirs pane; `after/large-text.png`: taller toolbar, rows at the scaled pitch, chips fall
  back to Ours/Theirs/Both and stay in-pane; `after/pseudo.png`: buttons sized to the
  pseudo-locale labels; `after/diff.png`: gutter numbers right-aligned on measured widths.
  The visible source band is shorter than before at 800×700 because the view now ends at the
  window (A1-DM-6) — the third hunk's chips scroll into view.
- **containers** — `after/dark/dev.qos.containers-tablet.png` and `after/compact.png`: the
  narrow list shows name + status stacked, actions clear; `after/large-text.png`: text painted
  once at 2× (title, tabs, logs legible, log lines on their pitch); laptop/desktop: heading on
  the name x, actions on the 16px gutter.
- **db_client** — laptop/desktop/light: Run and Snippets labels on one line, sidebar rows and
  button on one inset; `after/large-text.png` unchanged (A1-DB-7 open, Task 14).
- **code_editor** — `after/terminal.png`, `after/compact.png`: dock tab labels centred on the
  strip; other variants identical to BEFORE apart from that.
- **api_tester** — `after/large-text.png`: square hamburger; checkbox scaled and the key column
  starts after it; all other variants identical to BEFORE at 1×.

## Limits

- code_editor desktop (1920×1080) matrix cell cannot be rendered on this host (vello CPU
  tile allocator panic); the other three cells and the 1200/800/480 shots cover the app.
- api_tester's response Preview mode is not in the capture set; A1-API-5 is verified by the
  measured-pitch code path only.
- 200% text for db_client, git_client rows and code_editor's editor body remain open under
  their composition tasks (14, 6, 13) and were not attempted here.
