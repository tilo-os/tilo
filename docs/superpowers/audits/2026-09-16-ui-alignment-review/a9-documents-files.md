# UI alignment review — A9 Documents & files (ebook, pdf_viewer, file-explorer)

Date: 2026-09-16. Branch `ui-review/a9-documents-files`, base `codex/macos-shell` @
`087d5a62c` plus the one-line `Cargo.lock` fix from `ui-review/lockfix` (`778aa10c5`).
Scope: the *chrome* of the three document/file apps — shelf/library grid, TOC and
annotation rails, settings sheets, reader HUD, toolbars, find bar, comment/sign
panels, page bubbles, places sidebar, list/grid views, status bubble, rename
editors. Document canvases (EPUB page text, PDF page content, file thumbnails)
are authored content and were not judged.

Conventions applied throughout: every new metric reads a `Theme` token or a
`TextRenderer` measurement; deliberately off-scale values that were kept are
named and listed under *Conventions observed*; hit rect == painted rect; one
geometry description per view (paint, hit-testing and editors read the same
function); no action, keyboard route, selection, rename or drag state was
removed or renamed.

## Method

1. Read `ui/HIG.md` §1/§7/§7a, `ui/MANUAL_LAYOUT.md`, `ui/FORM_COMPOSITION.md`,
   `docs/hand-rolled-ui-audit.md`, the 2026-09-15 all-app reviews (review-1
   ebook, review-2 file-explorer + pdf_viewer, corrected-review-2), the
   closure register, `2026-09-16-ux-findings-repairs.md`, and plan tasks
   text-geometry 9/18 and composition 6.
2. Source pass over the chrome layout code of each app (literal metrics, font-size
   centring, sibling inset mismatches, hit-vs-paint divergence, breakpoints),
   partly delegated to two read-only sub-audits (ebook, pdf_viewer) and
   re-anchored to the corrected base by hand.
3. BEFORE captures for every app at the responsive matrix (dark + light),
   800×900 @ 2× text, 480×900, 800×700 and every listed scenario; every PNG opened
   and inspected. Captures live under `target/ui-review/<app>/before/`.
4. Fixes with tokens, measured text and shared helpers; a regression test per fix.
5. AFTER captures under `target/ui-review/<app>/after/`, compared pairwise.

Capture recipe: `target/ui-review/cap.sh` (ebook, pdf_viewer via `ui::launch`
`--responsive-matrix` / `--shots --viewport`) and `target/ui-review/fe-cap.sh`
(file-explorer takes `--size`, has no matrix, and is pointed at an isolated
`HOME` plus a seeded fixture directory so no real user file is ever captured).

## pdf_viewer

Files: `apps/pdf_viewer/src/app_draw.rs`, `app_events.rs`, `app.rs` (tests).
Viewports inspected: 1920×1080, 1366×768, 768×1024, 390×844 (dark + light),
800×900 @ 2×, 480×900, 800×700, scenarios selection-hud, page-bubble, search,
edit-comment, sign, reorder, error, import, loading.

| id | surface | defect | viewport/theme | file:line (after) | before → after | status |
|---|---|---|---|---|---|---|
| P1 | every custom label (Fit, zoom %, find placeholder/counter/toggles/hint, Delete, Open a PDF…, chips, + Reply, outline rows, sign hint/Clear, Place signature) | `vcenter(rect, size)` centred the *nominal* font size, not the measured line box (size×1.25×scale) — labels sat ¼ line low at 1×; at 2× the "Fit" label spilled out of its 34px button | 800×900 @2× dark: `before/large-text.png` (Fit clipped, x=657–695 y=11–45) | `app_draw.rs:82` (`vcenter` now takes the measured height); 15 call sites each use the `(w, h)` of the measurement they already make | `before/large-text.png` → `after/large-text.png` | fixed |
| P2 | empty-state title, Open button label, Place-signature label, author name | measured Regular, painted Semibold — centring and truncation used the wrong advance | all | `app_draw.rs:89` (`truncate_to_width` takes the paint weight), 1006, 1086, 2374, 3127 | — | fixed |
| P3 | comments sidebar | hit rects of cards scrolled under the tab strip were pushed unclipped; `apply_sidebar` picks the smallest containing rect, so a hidden 24px delete button beat the "Contents" tab | any scrolled thread list | `app_draw.rs:2135` (all body hits intersected with the painted body clip, empties dropped) | n/a (interaction) | fixed — test `sidebar_hits_are_clipped_to_the_scrolled_body` |
| P4 | comment card body / replies / composer | wrap widths mixed unscaled and scaled insets (`inner_w − 16` vs text at `12·s`): 4px right inset at 1×, −8px (past the border) at 2×; reply measured `−36` but painted `−36·s`; composer field `compose_h − 8.0` vs metrics `8·s` | 2× | `app_draw.rs:2248–2262` (`comment_body_wrap` / `comment_reply_wrap` / `comment_compose_wrap` feed both `comment_metrics` and the paint pass), 2703 | `before/edit-comment.png` → `after/edit-comment.png` | fixed — test `comment_text_wraps_keep_a_symmetric_inset_at_every_scale` |
| P5 | sidebar scroll model | `sidebar_item_layout` content height subtracted an unscaled origin; the Signatures reveal origin was a stale additive chain 66–104px off `sig_list_top`; `38·text_scale` tab height copied in three places | 2× / sign | `app_draw.rs:414` (`sidebar_tab_h`), 421 (`comments_list_top`), `prepare_sidebar_scroll` uses `sig_list_top` | — | fixed |
| P6 | toolbar | first control inset 12 from the left edge, last control 16 from the right | all widths | `app.rs` `PdfApp::toolbar_inset` (cached from `theme.tokens.spacing.s3` in `build_layout`, next to the cached `text_scale`, because `toolbar_layout` is pure geometry without a `Theme`); `app_draw.rs:127/156` | `before/dark/…desktop.png` → `after/dark/…desktop.png` | fixed — test `toolbar_edge_insets_are_symmetric` |
| P11 | comments sidebar inline editor | `edit_rect` (routed before any tab/toolbar hit) was published unclipped, so an editor scrolled under the tab strip still swallowed the press meant for the tab | scrolled thread while editing | `app_draw.rs` `draw_sidebar` (clipped with the body like every other body hit) | n/a | fixed (code-review finding) |
| P7 | toolbar at 480px | tool cluster (`tools_w` 194) is only clamped against `left_end`, never against `right_start`; Note/Sign overlap −/+ | 480×900: `before/narrow.png` | `app_draw.rs:152` | — | open — plan `2026-09-15-app-composition.md` task 6 (wrap to a second toolbar row); not duplicated here |
| P8 | page bubble | chevron hit slot 34px vs painted 16px glyph at inset 8 | all | `app_draw.rs:3320` | — | kept — larger touch target, does not reach the label; noted |
| P9 | sidebar internals | `PageSidebar` menu metrics (`MENU_W 184`, `MENU_ROW_H 30`, `NUM_GUTTER 22` unscaled) and `ui::SelectionHud` / `status_bubble::pill_size` (`text_size + 16` height from font size) | 2× | `ui/src/widget/page_sidebar.rs:89–97`, `ui/src/status_bubble.rs:118` | — | open — needs F6 / F5 |
| P10 | typography literals | 31 font-size literals; off-scale 9.5, 10.5, 11, 11.5, 12.5, 13 used for chips, hints, tab labels | — | `app_draw.rs` | — | open — recorded, not changed (changing sizes is a type decision, not an alignment fix) |

Conventions observed (pdf_viewer): `vcenter` is the single centring chokepoint
and now only accepts a measured height; every `tr.measure*` height that used
to be discarded as `_` is consumed. Kept off-scale, named: toolbar `bw/bh 34`,
`gap 6`, cluster gaps 14/10, `VIEW_PAD 28`, `SEARCH_H 46`, `RAIL_W 144`,
`SIDEBAR_W 324`, `OVERLAY_BREAKPOINT 900` (raw width, ignores text scale).

## ebook

Files: `apps/ebook/src/app.rs`, `input_controls.rs`. Viewports inspected:
1920×1080, 1366×768, 768×1024, 390×844 (dark + light), 800×900 @ 2×, 480×900,
800×700, scenarios shelf, library, settings-fonts, settings-fonts-rename,
settings-colours, settings-library, settings-shortcuts, scheme-editor,
font-picker, page-bubble, comments, long-toc, long-annotations, onboarding,
note, highlight, import-error, import-loading.

| id | surface | defect | viewport/theme | file:line (after) | before → after | status |
|---|---|---|---|---|---|---|
| E1 | reader top bar "Aa" pill | fixed 32px pill, label centred on its nominal 15px size — at 2× the label sat on the bar's bottom edge and was clipped | 800×900 @2×: `before/large-text.png` (pill x=637–700, label y=28–50) | `app.rs:8951` (pill height = measured label + s3, floored at s8 and then capped at TOP_BAR − s2 — cap applied last so a large UI scale cannot invert the bounds; width = label + 2·s3; label centred on its measured box) | `before/large-text.png` → `after/large-text.png` | fixed — test `reader_aa_pill_contains_its_label_at_2x_text` |
| E2 | Settings › Fonts size stepper | fixed 52px value slot clipped "0.96 ×" → "0.96 ›" and "1.08 ×" → "1.08 ›" (Mono, DejaVu rows) | 1200×800 dark: `before/settings-fonts.png` (x=845–898) | `app.rs:6194` (`size_stepper_slot_w` measures the field's widest value through `renderer_input_style` so the theme token is scaled once, + 2·`padding_standard`), `input_controls.rs:37` (`NumberField::widest_displays`) | `before/settings-fonts.png` → `after/settings-fonts.png` | fixed — test `size_stepper_slot_fits_the_widest_value`; **open, needs F2**: the slot assumes the shared field's type size equals `typography.scale.lg` (true today); `NumberInput` should expose its preferred width |
| E3 | Settings › Fonts inline rename | editor width was `w − 210` and ran under the visibility toggle (field x1=770 vs toggle x0=757); its left edge (`name_x − 8`) did not match the shared field's 12px text inset, so the name jumped 4px on entering rename | `before/settings-fonts-rename.png` | `app.rs:10328` (field ends s3 before `font_row_controls_x0`, the same cluster edge `draw_font_row_controls` lays out from — `debug_assert`ed equal), 10332 (`name_x − padding_standard`) | `before/settings-fonts-rename.png` → `after/settings-fonts-rename.png` | fixed — test `font_rename_editor_stops_before_the_controls_and_starts_on_the_name` |
| E4 | Settings › Colours inline rename | same `name_x − 6` inset mismatch | — | `app.rs:10937` | — | fixed |
| E5 | Settings › Shortcuts rows | label top-pinned at `y+14` (visually 6px above the row centre) while the binding pill was centred; pill text at a fixed `y+22` | `before/settings-shortcuts.png` | `app.rs:9847`, 9891 (both centre their measured boxes) | → `after/settings-shortcuts.png` | fixed |
| E6 | Settings tabs | label at fixed `tabs_y + 11` in a 38px tab | — | `app.rs:9734` | — | fixed |
| E7 | segmented `pill()` (quick settings, tags) | text centred on nominal size | — | `app.rs:6254`; pad reads `padding_standard` | — | fixed |
| E8 | Annotations rail header | "ANNOTATIONS" inset 22 vs cards/add-row inset 16; count badge pinned at `x0 + 110` (overruns the caption at large text) | `before/dark/…desktop.png` (header x=1616 vs cards x=1610) | `app.rs:9258` (caption on the cards' `s4` edge; badge follows the measured caption by `s2`, sized from its measured count, centred on the caption) | → `after/…desktop.png` | fixed |
| E9 | Contents rail header | "CONTENTS" at inset 22 while the chapter numbers start at 24 | all | `app.rs:9018` (`row_inset + number_inset`, shared with the rows) | — | fixed |
| E10 | TOC rows / reader pagination at 2× | fixed 44px rows and page line geometry overlap at 2× text | `before/large-text.png` | `app.rs` draw_toc / reader_page_geometry | — | open — plan `2026-09-15-app-text-geometry.md` task 18 (reading reflow) |
| E11 | running header vs rule on the framed page | header at `paper_y + 22`, rule at `+42` fixed; touches at 2× | `before/large-text.png` | `app.rs:7775–7790` | — | open — part of E10's reflow (moving the rule alone would collide with the first line) |
| E12 | shelf grid | `margin 40`, `gap 28`, card 212/196, breakpoints 960/560 raw px | — | `app.rs` shelf_grid_geometry | — | open — recorded; gaps are equal today (28/28) so no visible defect |
| E13 | reader_bubble | `TEXT_SIZE 12.5`, `CHEVRON_SLOT 26`; pill height from `status_bubble::pill_size` | — | `reader_bubble.rs:16` | — | open — needs F6 (pill_size measured height) |

Conventions observed (ebook): `TOP_BAR 54` stays fixed and the controls inside
it are now sized from measurements, so 2× labels fit without changing the bar;
`SIZE_STEPPER_BTN 28` and `FONT_ROW_TOGGLE_W 40` were named so the rename editor
and the control cluster derive from one description. The stepper slot measures
with `typography.scale.lg`, which is the shared field's type size today —
**open, needs F2**: `NumberInput` should expose an intrinsic width for its
widest value so callers do not have to know that.

## file-explorer

Files: `apps/file-explorer/src/views/file_view.rs`, `places_sidebar.rs`,
`files_app.rs` (kept minimal: these three carry uncommitted work in the live
checkout). Viewports inspected: 1200×800, 800×700, 480×900 (dark + light),
800×900 @ 2×, 1200×800 @ 2×, scenarios places, grid, rename, status,
context-menu, remote-account, thumbnails, select, menu, dropmove, pindrag,
reorder, history, sync, error, loading, grid @ 2×.

| id | surface | defect | viewport/theme | file:line (after) | before → after | status |
|---|---|---|---|---|---|---|
| F1 | grid / thumbnails | folder glyph (46px) and file badge (60px) each hung their label from their own preview, so labels in one row sat 14px apart (`Documents` y=139 vs `a-very-long…` y=153) | `before/grid.png`, `before/thumbnails.png` | `file_view.rs:323` (`grid_cell_slot`: one preview slot per cell, folder glyph centred in it, one label baseline; label height measured with the label's own style) | → `after/grid.png`, `after/thumbnails.png` | fixed — test `grid_labels_share_one_baseline_and_stay_inside_the_cell` |
| F2 | list rename editor | field left edge `name_x − 4` vs the shared `TextInput`'s `padding_standard` inset → the name jumped 8px right on entering rename (254 → 262) | `before/rename.png` | `file_view.rs:835` | → `after/rename.png` | fixed |
| F3 | list column header | header labels centred on font size, rows on the shared optical rule (`text_layout::centered_text_y`) | all | `file_view.rs:650` | — | fixed |
| F4 | places sidebar section captions | caption at `row_x0 + 2` (aligned with neither icon nor label) and centred on font size | all | `places_sidebar.rs:50` (`ROW_CONTENT_INSET` shared by icon and caption), 414 | → `after/dark/wide.png` | fixed |
| F5 | status bubble | `12.0f32` and `margin 14` literals | all | `files_app.rs:1419–1421` (`typography.scale.sm`, `spacing.s4`) | — | fixed |
| F6 | list rows / sidebar rows / columns at 2× | rows, headings and columns overlap (`before/large-text.png`, `before/grid-large-text.png`) | 800×900 @2× | `file_view.rs` `row_h`, `places_sidebar.rs` `ROW_H` | — | **not touched** — this is today's accepted repair ("Files rows/headings/columns overlap at 200 %", "Expanded Files sidebar heading overlaps Root") which lives uncommitted in the live checkout and is not on `codex/macos-shell`; re-implementing it here would conflict |
| F7 | error state | retry button at a fixed `center.y + 86` under `EmptyState::paint` (7px under the detail line at 1×; would overlap at 2×) | `before/error.png` | `files_app.rs` draw_state_surface | — | open — pass the action label to `EmptyState::paint` so the shared widget places it (F3 composition) |
| F8 | context menu / view menu | anchor correct; row metrics inside `ContextMenu` | — | `ui/src/widget/context_menu.rs` | — | needs F4 (nothing app-side) |
| F9 | narrow (480) hamburger band | a near-empty 44px row under the toolbar | `before/narrow.png` | NavShell composition | — | open — composition (review-2 P2), not alignment |

Conventions observed (file-explorer): `text_layout::centered_text_y` is the
one optical rule for every icon+label row and now also for the list header and
sidebar captions; `ROW_CONTENT_INSET 10` and `CONTENT_PAD_X 10` remain
off-scale but named (changing them re-flows the accepted 100 % layout).

## Tests run

Every cargo command was prefixed with
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1`
(the repo's exact macOS flag set; the env var replaces the `-C target-cpu=generic`
that cargo otherwise merges in from the parent checkout's `.cargo/config.toml`
and that makes `ring` fail its CPU const-assert). No cargo config was edited.
All new tests were executed, not only compiled.

New regression tests (9):

| crate | test | proves |
|---|---|---|
| pdf_viewer | `app::event_tests::sidebar_hits_are_clipped_to_the_scrolled_body` | every body hit lies inside the painted body, none reaches a tab |
| pdf_viewer | `app::event_tests::toolbar_edge_insets_are_symmetric` | first/last toolbar controls share one edge inset |
| pdf_viewer | `app::tests::vcenter_centres_the_measured_line_box` | `vcenter` centres a measured (2×-scaled) line box |
| pdf_viewer | `app::tests::comment_text_wraps_keep_a_symmetric_inset_at_every_scale` | body/reply/composer wraps end 12·s inside the card at 1×, 1.5×, 2× |
| ebook | `app::interaction_tests::font_rename_editor_stops_before_the_controls_and_starts_on_the_name` | real rename via `dispatch`; editor ends s3 before the toggle hit rect, text origin == static name x |
| ebook | `app::interaction_tests::reader_aa_pill_contains_its_label_at_2x_text` | real 2× frame (theme + renderer scaled); pill contains its measured label and stays inside the top bar |
| ebook | `app::interaction_tests::size_stepper_slot_fits_the_widest_value` | slot ≥ widest value + 2·padding for the field |
| file-explorer | `views::file_view::tests::grid_labels_share_one_baseline_and_stay_inside_the_cell` | one label anchor per row; label inside the cell at 1× and 2× line heights |

Results (final code):

- `cargo test -p pdf_viewer --lib`: **165 passed, 0 failed** (155 pre-existing + the 4 new; the
  10 layout-relevant tests were also run scoped: 10 passed).
- `cargo test -p ebook --lib`: **208 passed, 2 failed**. The two failures are
  `app::interaction_tests::serif_reader_matches_typography_baseline` (105 762 px differ)
  and `app::interaction_tests::mathematical_reader_matches_shared_typesetting_baseline`
  (40 714 px differ). **Pre-existing**: with my ebook edits reverted at the same base the
  same two fail with 105 090 / 40 042 px — a host-rendering mismatch against the
  Linux-generated `tests/fixtures/typography/*.png` baselines, not this change (the ~700 px
  delta is the 2px caption/pill shifts). Scoped run of the changed areas: 22 passed.
- `cargo test -p file-explorer --no-fail-fast` (every target, final code):
  - lib `src/lib.rs` (parallel): **223 passed, 10 failed** — all ten are
    `files_app::{injected_startup_tests,command_path_tests,open_with_tests}::*` panicking
    inside `model::fixture_io_guard` ("ambient guard already armed" / "ambient I/O
    denied"). **Pre-existing and nondeterministic**: the guard is a process-global static
    that races when those tests share threads. At the base with my three file-explorer
    edits reverted the same run gave 224 passed / 8 failed with identical panics; the same
    lib run with `--test-threads=1` gives **233 passed, 0 failed** (my new grid test
    included).
  - `src/main.rs` unit tests: **3 passed**.
  - `tests/it.rs` (parity, scroll, double-click, drag-drop, shots smoke): **39 passed,
    3 failed** — `parity::breadcrumb_component_click_navigates_to_that_component`,
    `parity::file_explorer_widget_implements_app_widget_and_defaults_correctly`,
    `parity::sidebar_shows_fixed_seven_shortcuts_when_home_present`. **Pre-existing,
    locale-driven**: the host locale is German (`"Persönlicher Ordner"` / `"Dateien"`
    vs the asserted `"Home"` / `"Files"`); the same three pass under
    `LC_ALL=en_US.UTF-8` (3 passed). Today's layout net (`scroll`, `parity` geometry,
    `shots_smoke`) is green.
  - `tests/live_collaborative_mount.rs`: **1 passed**. Doc-tests: 0.
- Compile gates (`--tests`): `cargo check -p ebook`, `cargo check -p pdf_viewer`,
  `cargo check -p file-explorer` — all three clean, no errors or unused-variable warnings.
- Code review (`/code-review`, medium) raised five findings; all five were fixed before
  the final runs above: the `aa_h` `clamp` that would panic under a large UI scale
  (max/min ordering), the stepper slot measuring a theme-resolved size through a scaling
  renderer (now `renderer_input_style`), the unclipped `edit_rect` (P11), the
  `TOOLBAR_EDGE_INSET` literal (now cached from the theme), and the duplicate
  cluster-edge derivation guarded by a `debug_assert` (now one derivation).

Captures: `target/ui-review/{ebook,pdf_viewer,file-explorer}/before/` and `…/after/`
(76 PNGs each set, all from the corrected base; AFTER from the final binaries). Every
PNG was opened and inspected.

## Limits

- **Base correction.** The worktree was first created from `24ee08c66` (= `main`),
  not `codex/macos-shell`; the coordinator ordered a rebase. This branch was
  reset onto `codex/macos-shell` @ `087d5a62c` (no unique commits existed yet;
  the three build-enabler cherry-picks were already on the target) and the
  `ui-review/lockfix` commit re-applied. All BEFORE/AFTER captures, findings and
  line numbers are from the corrected base; the wrong-base captures were
  discarded.
- **Build host.** Cargo merges `/Users/xaver/dev/qos/.cargo/config.toml` into
  this worktree's config (the worktree sits beneath that checkout); its
  aarch64-non-Linux `target-cpu=generic` breaks `ring`. Every cargo command was
  run with `RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."`
  to override it; nothing was changed in either config file.
- The file-explorer 200 % repairs are not on the base; the 2× file-explorer
  captures therefore still show the overlap and were not judged.
- Static captures cannot prove pointer/keyboard routes; those are covered by the
  existing interaction tests plus the new hit-clipping test.
- The pdf_viewer narrow toolbar overlap and the ebook 2× reading reflow are owned
  by their planned tasks and were left untouched.
