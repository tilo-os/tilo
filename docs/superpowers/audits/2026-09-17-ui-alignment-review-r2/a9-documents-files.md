# UI alignment review, round two — A9 Documents & files (ebook, pdf_viewer, file-explorer)

Date: 2026-09-17. Branch `ui-review/r2-a9-documents-files`, cut from the merged round-one tip
`381eff6a8` ("Merge ui-review/integration"). Scope as in round one: the *chrome* of the three
document/file apps. Document canvases (EPUB text, PDF pages, file thumbnails) are content and
were not judged. Every cargo command carried the inline macOS flag set
(`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."`, `--locked
--no-default-features`); no cargo config was edited.

Capture recipe: `target/ui-review/cap.sh APP BIN TAG scenarios…` (dark + light responsive
matrix, 800×900 @ 2× as `text-scale-2.png`, 480×900 `narrow.png`, 800×700 `compact.png`, every
listed scenario at 1200×800) with an isolated `HOME`/XDG profile per app and, for file-explorer,
`QOS_USER_HOME`/`QOS_WORKSPACE_ROOTS` pointed at a seeded fixture directory
(`target/ui-review/fixture`) so no real file is ever captured. Round one named its 2× chrome
capture `large-text.png`, which the ebook *scenario* of the same name overwrote; the 2× chrome
frame is now `text-scale-2.png`. Pixel positions below come from `target/ui-review/px.py`
(a dependency-free PNG probe: ink rows/columns of a region). Every PNG named here was opened.

## 1. File-explorer merge verification

The merge combined the user's 200 % repairs (row heights `× text_scale`, `HEADER_H *
text_scale`, sizes divided by the renderer scale) with round one's edits; the coordinator
resolved `places_sidebar.rs` and `views/file_view.rs` by hand. Verified against the merged
binary at 1920×1080, 800×900 @ 2× and 480×900, dark and light, plus the places / grid / rename /
status / context-menu / remote-account / thumbnails / select / error / loading scenarios.

What the resolution got right (measured, not eyeballed):

| surface | 1× | 2× | verdict |
|---|---|---|---|
| sidebar caption vs icon column | "FAVOURITES" ink x0 = 23, Home icon x0 = 23; caption glyph centre y 74.5 in a 61–87 band (centre 74) | caption x0 = 24 vs icon 23 (glyph side-bearing); glyph centre 88 in a 61–113 band (centre 87) | correct — `ROW_CONTENT_INSET` for x, the user's `HEADER_H × scale` centring for y |
| sidebar row icon vs label | icon 97–111 (centre 104), label 100–109 (104.5) | icon 140–154 (147), label 139–158 (148.5) | correct |
| list row icon vs label | icon 95–109 (102), text 98–108 (103) | icon 142–156 (149), text 141–160 (150.5) in a 116–184 row (150) | correct — rows are `LIST_ROW_H × scale × text_scale` |
| column x: header vs cells | Size 1635 / 1634, Modified 1741 / 1741 | same columns (`header_col_rect` is shared) | correct |
| rename field | field border x = 243 = `name_x − padding_standard`; edited text starts at x 255 where the static name starts (255) | — | correct (F2) |
| grid labels | one baseline per row (F1), preview slot shared | — | correct |

What it got wrong, and what was fixed here:

| id | surface | defect | evidence | fix | status |
|---|---|---|---|---|---|
| R2-F1 | list column header | round one centred the header on `centered_text_y(top, header_h, hsize)` with the *nominal* size; the user's rows centre the *measured* line box. At 2× the header sat 7 px low ("Name" glyphs 85–101 in a 56–116 band, centre 86 vs 93) and used a different rule from the cells | `before/text-scale-2.png` → `after/text-scale-2.png` (glyphs 78–94, centre 86 = band centre; cells 141–160 in 116–184) | `file_view.rs`: `FileView::centered_line_top(y0, h, measured_h)` is the one vertical rule for the list; header and `draw_list_row_text` both call it | fixed |
| R2-F2 | breadcrumb bar | `centered_text_y(rect, TEXT_SIZE)` positioned a 13 px request that the renderer paints at 26 px: label 8 px low at 2× (glyphs 24–43 vs icon 19–32) | `before/text-scale-2.png` → `after/text-scale-2.png` (text 15–33, icon 19–32) | `text_layout::centered_text_y(y0, h, size, text_scale)` now centres the *rendered* size; both breadcrumb call sites pass `text_renderer.text_scale()`; module doc rewritten to say what the two rules in the app are and that both scale | fixed — test `text_layout::tests::centres_the_rendered_size_at_every_text_scale` |
| R2-F3 | status bubble | **round-one regression (F5)**: `typography.scale.sm` is theme-scaled and `pill_size`/`draw_pill` hand it to a scaling renderer → 48 px glyphs at 2×, bubble clipped off the bottom-right (`before/text-scale-2.png`, glyphs 865–897) | → `after/text-scale-2.png` (glyphs 870–887 = 24 px type, bubble inside the frame at x 685–769) | `files_app.rs`: `scale.sm / tr.text_scale()` — the app's one scale rule | fixed |
| R2-F4 | empty-folder message, drag ghost | pre-existing double scaling: both passed `scale.base` straight to the renderer (4× at 2×); the ghost pill was a fixed 30 px | source | unscale once; ghost pill height = measured label + 2·`s1`, floored at 30; both centre through `centered_text_y(…, text_scale)` | fixed |
| R2-F5 | error state (round one F7) | retry button at a fixed `center.y + 86`: 7 px under the detail at 1×, over it at 2×; the icon/title/detail stack was centred without the button | `before/error.png` → `after/error.png` (button 16 px under the detail, whole stack centred) | `EmptyState::paint` receives the action label and returns the pill rect it laid out; the retained shared `Button` (hover, focus, keyboard, typed Refresh command) is laid out on that rect and painted over it — never narrower than four `metrics.min_pointer_target` (the existing ≥ 120 px test still holds) | fixed — F3's widget does the placing; nothing app-side is hand-rolled |
| R2-F6 | context menu (round one F8) | the `context-menu` scenario showed only the resting menu, so F4's dark-theme armed-row fix was unverifiable from this app | `after/context-menu.png`: "New Folder" carries the accent fill with on-accent ink | `main.rs`: the scenario arms the first row through the menu's real keyboard route (`KeyEvent::synthetic(ArrowDown)` via `handle_event`) | verified |
| R2-F7 | narrow (480) hamburger band | a 56 px band under the toolbar holding one 36 px chip (`before/narrow.png`) | — | `NavShell` reserves the band itself (`ui/src/widget/nav_shell.rs` `CHIP_INSET * 2 + CHIP_SIZE`, the comment in `files_app.rs` `view_col` explains why the app must not reserve it) | open — NavShell composition (`ui/`), not app-side |

Tests for the merged file-explorer, before and after these edits: lib **247 → 248 passed, 0
failed** with `--test-threads=1` (the `fixture_io_guard` race is unchanged in parallel runs),
`src/main.rs` **3 passed**, `tests/it.rs` **39 passed, 3 failed** — the same three German-locale
parity asserts as round one (`"Persönlicher Ordner"`/`"Dateien"` vs `"Home"`/`"Files"`),
`live_collaborative_mount` **1 passed**.

## 2. pdf_viewer

| id | surface | defect | evidence | fix | status |
|---|---|---|---|---|---|
| R2-P1 | toolbar at 480 px (round one P7) | the five-tool cluster (194 px) is clamped only against `left_end`; Note and Sign ran under − / + | `before/narrow.png` → `after/narrow.png`, `after/light/…-phone.png` | `toolbar_layout_for(tb)`: when `tools_w + 2·s2 > right_start − left_end` the tools (and swatches) wrap onto a second toolbar row centred across the bar; `ToolbarLayout::rows`; `recompute_regions` sizes `toolbar_rect` from the rows a one-row layout asks for, so search bar, rail, sidebar and canvas all start under the whole bar. Every action stays reachable; hit-testing, tooltips and painting read the same layout | fixed — test `toolbar_wraps_the_tool_cluster_onto_a_second_row_when_narrow` (1280 → 1 row, 480 → 2 rows; no two controls intersect; all inside the bar; canvas starts at the bar's bottom) |
| R2-P2 | empty state at 200 % (fresh eyes) | the whole canvas pass runs at renderer scale 1.0 (pages are fixed-layout — correct) but the *empty state* was drawn inside it, so its title/detail/button ignored the text scale (`before/text-scale-2.png`: title glyph rows identical to 1×) and its offsets (`cy + 34/58/78`) would have collided had it scaled | → `after/text-scale-2.png` (32 px title, 24 px detail, button grown to its label) | the 1.0 scale now wraps only the document; `draw_empty_state` measures title/detail/label at the chrome scale and stacks glyph → title → detail → button with `s4`/`s1`/`s1` gaps, centred as one group; the button is its measured group + `2·s6` / `2·s3`; the painted rect is cached in `empty_open_button: Cell<Rect>` and `empty_open_button_rect()` (the click handler) reads it back — hit == paint | fixed |
| R2-P3 | 31 off-scale font literals (round one P10) | 9.5 / 10.5 / 11 / 11.5 / 12 / 12.5 / 13 / 16 across toolbar, find bar, comments, outline, signatures, page bubble | `after/edit-comment.png`, `after/search.png`, `after/sign.png` | `TypePx { xs, sm, base, lg }` = theme type tokens ÷ `theme.text_scale`, cached in `build_layout` next to `text_scale`/`toolbar_inset` so the pure helpers without a `Theme` (`search_layout`, `comment_metrics`) size text exactly as the draw pass paints it. Mapping: 9.5, 10.5 → `xs`; 11, 11.5, 12, 12.5 → `sm`; 13 → `base`; 16 → `lg`. The three shared `TextInput`s that overlay measured text (find field, reply composer, signer name) take the same steps, so the editor's glyphs are the glyphs they replace | fixed — 50 sites; type decision recorded: replies and page numbers now share `sm` with the body/title they sit under (colour and weight still separate them) |
| R2-P4 | `PageSidebar` internals (round one P9) | `MENU_W 184`, `MENU_ROW_H 32`, `NUM_GUTTER 24` unscaled | source (`ui/src/widget/page_sidebar.rs:93–110`) | F6 named the constants against tokens (`= s6`, `= s8`) but they are still literals and do not follow the text scale | open — F6 |
| R2-P5 | `SelectionHud` internals | `13.0` label size, fixed `dims::HEIGHT` | source (`ui/src/reader_hud.rs:87–109`) | unchanged by round one | open — F5/F6 |
| R2-P6 | `status_bubble::pill_size` | height `text_size + 16` from the nominal size, `pad_x 14` | source (`ui/src/status_bubble.rs:118–124`) | unchanged; the pdf page bubble and the ebook reader bubble both inherit it | open — F5 |

Conventions observed (pdf_viewer): `TOOLBAR_H 56` per row, `bw/bh 34`, `gap 6`, cluster gaps
14/10, `OVERLAY_BREAKPOINT 900` kept as round one named them. `TypePx` divides by
`theme.text_scale` on purpose: it yields the *nominal* token, which whichever renderer scale is
in force then applies once — the chrome scale for bars and rails, 1.0 inside the page pass.
`renderer_input_style` would give the same result for the chrome pass but the wrong one for the
page pass, and the two pure layout helpers have no `TextStyle` path; the reviewer's suggestion
to migrate every `measure`/`draw_text` to `measure_style`/`draw_style` is recorded, not done.

## 3. ebook

| id | surface | defect | evidence | fix | status |
|---|---|---|---|---|---|
| R2-E1 | contents rail at 200 % (round one E10, alignment part) | fixed 44 px rows; a wrapped title ("I. The Tide-House" over two lines) ran into the next chapter's number | `before/text-scale-2.png` → `after/text-scale-2.png` | every row is as tall as its wrapped title needs (measured at the heavier active weight so the height does not jump on chapter change) floored at `metrics.panel_row_height − s1`, pitch adds an `s1` gap — one-line rows keep the 44 px panel-row pitch at 1× (`after/long-toc.png` unchanged); the number column comes from the measured "00" and the title starts an `s3` after it; number and title centre their measured boxes on the pill; hit rects, scroll extent, scroll-to-active and the rail's keyboard paging (`toc_row_pitch`) all read the same per-row heights | fixed — test `toc_rows_grow_with_wrapped_titles_at_2x_and_never_overlap` (1× and 2× frames: rows ordered, non-overlapping, ≥ floor, and the tallest 2× row is taller than any 1× row) |
| R2-E2 | contents / annotations captions | "CONTENTS" at `y0 + 18` with the list at `y0 + 44`: at 2× the caption's 27.5 px box reached the first row; "ANNOTATIONS" (E8) sat at the same literal | `after/long-toc.png` (both captions at y 77) | both captions drop an `s4`; the contents list starts an `s3` under the caption's measured box | fixed |
| R2-E3 | running header vs rule (round one E11) | header at `paper_y + 22`, rule at `+42`, content at `+50` fixed: at 2× the 27.5 px header box crossed the rule (`before/text-scale-2.png`) | → `after/text-scale-2.png` | `draw_reader` measures the header's line box at the frame's chrome scale (`running_header_h`); `reader_page_geometry` puts the header at `paper_y + s6`, the rule an `s1` under that box and the content an `s2` under the rule (rounded, so 1× keeps the rule at +42 and the content at +50 — no pagination change); the header is ellipsized to one line so a long title cannot wrap into the rule | fixed — test `running_header_fits_its_slot_above_the_rule_at_2x` (real 2× frame; an independent 2× measurement of the current chapter title fits above the rule; content starts under it). The first version of this test caught the `size × 1.25` estimate under-sizing the slot, which is why the height is measured, not estimated |
| R2-E4 | shelf grid literals (round one E12) | `margin 40`, `gap 28`, cards 212/196, breakpoints 960/560 raw px | source `shelf_grid_geometry` | no spacing token is 28 or 40, and composing `s6 + s1` to restate 28 would be token theatre; gaps are equal today and the 2026-09-15 composition plan owns the shelf re-flow | open — recorded, deliberately off-scale |
| R2-E5 | size stepper slot (round one E2) | the slot measures the field's widest value through `renderer_input_style` assuming the shared field's size is `scale.lg` | source | `NumberInput` still has only `with_size(w, h)` (`ui/src/widget/number_input.rs:297`); no intrinsic-width API landed | open — F2 |
| R2-E6 | page text at 200 % (reflow part of E10) | line pitch and hyphenation of the *content* at 2× (`after/text-scale-2.png`, "Trim the / wick", "Her / grand- / mother") | — | content typesetting; task 18 of `2026-09-15-app-text-geometry.md` | open — not chrome |

Conventions observed (ebook): the rail chrome type sizes stay the file's inline 11 / 12 / 13
(the annotations rail and settings use the same literals; retyping one rail alone would split
the app), and the running header's `11.0` is a single function (`running_header_px`) shared by
its measurement, its paint and its test. `TOP_BAR 54`, `TOC_W 244`, `row_inset 10`,
`number_inset 14` kept as named.

## 4. Fresh eyes at 800×700, 480×900 and 200 %

- 800×700: all three apps clean (`compact.png`); the file-explorer's columns, sidebar and bubble
  keep their 1200 px geometry.
- 480×900: pdf_viewer fixed (R2-P1). file-explorer: hamburger band recorded (R2-F7); the Size
  column and header keep their alignment. ebook: unchanged from round one.
- 200 %: file-explorer R2-F1…F4 fixed; pdf_viewer R2-P2 fixed; ebook R2-E1…E3 fixed. Remaining
  200 % items are `ui`-owned (R2-P4…P6) or content (R2-E6). Icons in the file-explorer list and
  sidebar do not scale with text (17/18 px next to 28 px labels) — consistent with the toolbar
  icons and the user's repair, so left as a density decision, noted only.

## 5. Tests run

New regression tests (5):

| crate | test | proves |
|---|---|---|
| file-explorer | `text_layout::tests::centres_the_rendered_size_at_every_text_scale` | a doubled row and doubled glyphs double the offset; nominal 13 @ 2× lands where rendered 26 @ 1× does |
| pdf_viewer | `app::event_tests::toolbar_wraps_the_tool_cluster_onto_a_second_row_when_narrow` | 1280 → 1 row, 480 → 2 rows; toolbar height follows rows; canvas starts under the bar; no two controls intersect; all controls inside the bar |
| ebook | `app::interaction_tests::toc_rows_grow_with_wrapped_titles_at_2x_and_never_overlap` | real 1× and 2× frames; rows ordered, non-overlapping, ≥ the token floor; a wrapped 2× title gets a taller row |
| ebook | `app::interaction_tests::running_header_fits_its_slot_above_the_rule_at_2x` | real 2× frame; the measured 2× header fits above the rule; content starts under it; the 2× slot is taller than the 1× slot |
| pdf_viewer (existing, still green) | `toolbar_edge_insets_are_symmetric`, `toolbar_tool_buttons_select_tools` | the wrapped layout keeps symmetric insets and every tool clickable |

Results (final code, after the code-review fixes):

- `cargo check -p ebook -p pdf_viewer -p file-explorer --tests`: clean, no warnings.
- `cargo test -p pdf_viewer --lib`: **166 passed, 0 failed** (165 + 1 new).
- `cargo test -p ebook --lib`: **210 passed, 2 failed** — the two pre-existing Linux-baseline
  typography goldens (`serif_reader_matches_typography_baseline`,
  `mathematical_reader_matches_shared_typesetting_baseline`), failing at the base for the same
  host-rendering reason round one recorded.
- `cargo test -p file-explorer --no-fail-fast -- --test-threads=1`: lib **248 passed, 0
  failed**; `src/main.rs` **3 passed**; `tests/it.rs` **39 passed, 3 failed** (pre-existing
  German-locale parity asserts, same three as round one; pass under `LC_ALL=en_US.UTF-8`);
  `live_collaborative_mount` **1 passed**.
- Spacing ratchet (`ui/tests/f1_spacing_token_audit.rs`, which this unit may not edit): the diff
  adds exactly one module-level constant, `LINE_BOX_FACTOR = 1.25`, which the ratchet does not
  count; every other new value is token arithmetic or a local binding, so no per-file count moves.

## 6. Code review

`/code-review` (medium) raised six findings; five were fixed, one is recorded:

1. New whole-number pixel consts would have raised the spacing ratchet (`RETRY_MIN_W`,
   `EMPTY_OPEN_MIN_W/H`, `TOOLS_EDGE_GAP`, six ebook `TOC_*`/`RUNNING_HEADER_PX`) — replaced by
   `4 × metrics.min_pointer_target`, measured group + token padding, `Spacing::BASE.s2`,
   `metrics.panel_row_height − s1`, and one shared `running_header_px()`.
2. Placeholders/composer probes moved to `type_px` while the shared `TextInput`s kept
   13.0/11.5/12.0 — the find field, reply composer and signer field now take the same steps.
3. `mem::zeroed()` `KeyEvent`/`DeviceId` in the shots scenario — `KeyEvent::synthetic` +
   `DeviceId::dummy()`, the toolkit's own pattern.
4. `TypePx` vs `renderer_input_style` — recorded with the reasoning above (the page pass needs the
   nominal token under a 1.0 renderer; the file-explorer keeps its one division rule as instructed).
5. `recompute_regions` assigned `toolbar_rect` twice to read `.rows` — `toolbar_layout_for(rect)`
   takes the rect explicitly; one derivation, no ordering trap.
6. `draw_comment_replies` recomputed `TypePx::from_theme` per reply — the size is passed in from
   the cached `type_px` the metrics were measured with.

## 7. Captures

`target/ui-review/{file-explorer,pdf_viewer,ebook}/{before,after}/`: dark + light responsive
matrix (desktop / laptop / tablet / phone), `text-scale-2.png` (800×900 @ 2×), `narrow.png`
(480×900), `compact.png` (800×700) and every scenario listed in the brief (file-explorer
additionally `error`, `loading`; pdf_viewer additionally `error`, `import`, `loading`). AFTER
sets are from the final binaries. Every PNG named in this report was opened; the pixel figures
come from `target/ui-review/px.py`.

## 8. Limits

- macOS host: `shots`, `qos-control` and headless compositor paths are Linux-only; all evidence
  is from each app's own `--shots`/`--responsive-matrix` renderer with the CPU rasteriser.
- The two ebook typography goldens and the three file-explorer locale asserts fail at the base
  and were left as they are (never `#[ignore]`d, never weakened).
- `NavShell`'s hamburger band, `PageSidebar`, `SelectionHud`, `status_bubble::pill_size` and
  `NumberInput`'s intrinsic width are `ui`-owned and only recorded.
- The ebook 2× page text (line pitch, hyphenation) is content typesetting owned by the
  text-geometry plan and was not touched.
