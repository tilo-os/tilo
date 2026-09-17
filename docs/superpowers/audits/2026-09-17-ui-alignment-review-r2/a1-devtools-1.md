# UI alignment review, round two — A1: Developer tools I

Unit: api_tester, db_client, code_editor, containers, diffmerge, git_client.
Branch `ui-review/r2-a1-devtools-1`, cut from `381eff6a8` (`Merge ui-review/integration`,
the merged tip carrying round one). Round-one report: `../2026-09-16-ui-alignment-review/a1-devtools-1.md`.

## Method

1. `git reset --hard 381eff6a8`; `cargo build --bins` for the six apps (clean).
2. Merge-survival check: `git log`/`git diff` of every A1 file against both merge parents;
   BEFORE captures of all six apps (dark + light responsive matrix, 200 % at 800×900,
   480×900, 800×700, every scenario, git client `--shots-narrow`) — 74 PNGs, every one
   opened. Paths `target/ui-review/<app>/before/…`.
3. Source pass for the nine round-one open items and for every app-side double-scaling
   site (theme type token → legacy renderer API).
4. Fixes: one measured metrics snapshot per surface (`Metrics`/`Bands`/`BarMetrics`),
   measured with the exact painted style, shared by paint, hit rects, scroll clamps and
   the accessibility bounds; 1× reproduces the historical constants exactly, larger text
   scales grow the bands. Regression per fix (1×-exact + 2× growth, paint == hit).
5. `/code-review high`; its findings fixed or recorded (see "Code review").
6. AFTER captures with the same matrix, every PNG opened: `target/ui-review/<app>/after/…`.

Every cargo command ran as
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=." RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1 PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig cargo … --locked --no-default-features`
(the parent checkout's cargo config otherwise breaks `ring`). No cargo config, `ui/`,
`shell/` or `crates/` file was edited. Capture helper: `/tmp/a1r2/cap.sh` (isolated
`$HOME`/XDG dirs, `QUICK_UI_VELLO_CPU=1`), mirroring the E2E recipe.

## Merge-survival check

| app | round-one files | user WIP merged in `0b08f8d2a` | overlap | result |
|---|---|---|---|---|
| code_editor | `bottom_panel.rs` (dock tab strip, Problems rows) | `app.rs` (diagnostics cache, native fixture), `editor_view.rs` (incremental highlighting), `highlight.rs`, `lib.rs`, new test files | **none** — disjoint files, no function changed by both sides | dock tabs still centred on the strip (`after/terminal.png`, `after/compact.png`); Problems rows unchanged; no duplicated or contradictory path |
| git_client, diffmerge, containers, db_client, api_tester | round-one files | no WIP in these crates | none | BEFORE captures identical to round-one AFTER |

Two pre-existing failures surfaced in whole-crate runs, both reproduced at the merged base
with this unit's sources swapped back to `381eff6a8`:

- `code_editor app::tests::a_document_from_a_previous_session_still_stats_the_file` (merged
  WIP test) compares a persisted temp path with its canonicalised form (`/var/…` vs
  `/private/var/…` on macOS). Not a geometry test, not touched; recorded for the owner of
  `app.rs`.
- `db_client app::behavior_tests::host_routes_named_divider_focus_keyboard_and_pointer_reset`
  fails only inside the full `db_client` binary (also with `--test-threads=1`) and passes
  alone or inside `-- behavior_tests::`; the divider does not move on ArrowDown
  (`195.25` vs `219.25`). Identical with the base sources (83 passed / 1 failed), so it is
  process-state or timing dependent and predates this branch; recorded for the owner of
  db_client's behaviour tests.

## Findings

Status legend: **fixed** (this branch, with a regression), **open** (recorded, with owner).
Before/after PNGs: `target/ui-review/<app>/{before,after}/<variant>.png`; matrix cells
`<theme>/dev.qos.<app-id>-{phone,tablet,laptop,desktop}.png`.

### git_client

| id | surface | defect | viewport | file | status |
|---|---|---|---|---|---|
| A1-GC-9 (r1 open) | unified diff hunk header | `@@ -12,9 +12,11 @@ impl Scene {` ran under the `Unstage hunk` button at 390/480 | phone, `narrow.png` | `diffview.rs` | **fixed** — `draw_unified_row_background` returns the x the row's text must end before (leftmost button − `s2`); the header is elided with `draw::elide_mono` (mono counterpart of `TextRenderer::elide_to_width`, trailing space trimmed). Test `hunk_header_elides_before_the_unstage_button` |
| A1-GC-7 (r1 open) | diff rows | fixed 18 px pitch under 31 px glyphs at 200 % | `large-text.png` | `diffview.rs` | **fixed** — `diffview::Metrics { row_h, text_dy, old_col_right, new_col_right, gutter_w, inset }` measured per draw with the painted mono style; returned to the app (`DiffDraw`) so `content_height`/wheel clamp use the same pitch; gutter columns widen with the widest line number (last numbered line per column, O(1)); split view shares the metrics. 1× == constants (test) |
| A1-GC-7 | changes rows / section headers / empty hint / commit box | fixed 30/30/22/150 px bands under 2× text | `large-text.png` | `changes.rs` | **fixed** — `changes::Metrics` (row, section, hint, author row, input, button h/w, commit box, pads) measured per draw; `RowLayout` takes the pitch; chevron widens with the row; commit box stacks author → input → button from measured heights (1× = 150) |
| A1-GC-7 | history rows | 68 px pitch with three fixed baselines (9 / 30 / +18) | `large-text.png`, laptop | `history.rs` | **fixed** — `history::Metrics` (pill, summary, meta heights + the row's rhythm) measured per draw, returned with the row rects (`ListDraw`) for the scroll clamp; the first ref pill always paints (elided to the room before the hash) so a long branch name never leaves an empty first line |
| r2-GC-1 | section header | count badge painted over `Unstage all` at 2×; title had no limit | `large-text.png` | `changes.rs` | **fixed** — `HeaderLayout::measure`: action anchored first, title elided to the room left minus badge and two `s2` gaps. Test `section_header_title_badge_and_action_never_collide` (1× and 2×) |
| r2-GC-2 | `draw::pill` | label size derived from a fixed pill height (`height − 6`) and then scaled again by the renderer → 2× glyphs in a 1× pill everywhere (section badges, ref pills, branch pill, card pills, tag pills) | `large-text.png` | `draw.rs` + 5 callers | **fixed** — `pill` takes the label size; `pill_height`/`pill_width` derive the geometry (10/12/14 px labels → exactly 16/18/20 px at 1×, leading 2.5) |
| r2-GC-3 | app bar | title (`y0+9`) and subtitle (`y0+30`) overlapped at 2×; 32 px controls held 26 px labels | `large-text.png` | `app.rs` | **fixed** — `BarMetrics` (row, tab row, control heights from measured title/subtitle/tab lines); the title + subtitle stack centres in its row (1× moves 1.5 px: the stack was 9 px from the top and 12 px from the bottom); hamburger repaint over the open flap reuses the placed rect instead of a fixed `APPBAR_H` row |
| r2-GC-4 | diff pane header | fixed 36 px; title/summary at `y0+9`/`y0+10`; a long title wrapped into the first diff row | all | `app.rs` | **fixed** — measured header height (1× = 36) shared with the scroll clamp; title elided (not wrapped) and centred with the `+adds −dels` summary and the Discard button label |
| r2-GC-5 | changes rows | a dir hint reduced to a lone `…` crowded the counts at 2× | `large-text.png` | `changes.rs` | **fixed** — dir hint skipped below one `s6` (24 px) of room |
| r2-GC-6 | panels tag list | 18 px pills on a 28 px pitch; hash/message at `y + 2` | source | `panels.rs` | **fixed** — pitch from `pill_height` (1× = 28), hash and message centred on the pill |
| A1-GC-8 (r1 open) | narrow settings disclosure | Task 6 disclosure vs two-row bar | — | — | open — composition Task 6 |
| r2-GC-7 | hunk buttons | 14 px tall pointer targets inside 18 px rows (min target 32) | 1× | `diffview.rs` | open — deliberate: a hunk button is row-bound; a 32 px target needs a row-level affordance (composition Task 6) |
| r2-GC-8 | strings | every label is a literal (no `locales/` in the crate) | — | — | open — pre-existing, crate-wide i18n gap, not an alignment change |

### db_client

| id | surface | defect | viewport | file | status |
|---|---|---|---|---|---|
| A1-DB-7 (r1 open) | tab bar / toolbar / result strip / toggle | fixed 38/40/26/26 px bands: the 26 px `Query` pill touched the bar edges, `Run` overflowed its 28 px button at 200 % | `large-text.png` | `app.rs` | **fixed** — `Bands { tabbar_h, toolbar_h, strip_h, toggle, inset }` measured at the top of `draw`; `relayout()` re-runs in the same frame when they change, so paint, hit rects and the empty state agree; tab pill pad = `s1`, toolbar/strip pads and the 6 px inset are the panel's own 1× rhythm (1× == constants) |
| A1-DB-7 | results grid | 26 px rows / 30 px header under 32 px glyphs | `large-text.png` | `grid.rs` | **fixed** — `GridView::measure_bands` (called by `bind` and by the app before each draw); `visible_rows`, `header_hit`, `row_hit`, header/body paint read the two fields. Test `bands_match_the_constants_at_1x_and_hits_follow_the_scaled_pitch` |
| A1-DB-7 | schema tree | 24/26 px rows/header; text at `y + 4/5/6` nudges; wrapped names ran into the next row | source, laptop | `schema_tree.rs` | **fixed** — bands measured in `draw`, read by `visible_rows`/`hit`; every text centred on the row via `text_top`; names and columns elided instead of wrapped; type chip = one `s4` square centred on the row |
| A1-DB-7 | structure tab | 26/28 px rows/header; 30/18/18 px title/panel bands | source | `structure.rs` | **fixed** — `Bands` snapshot threaded through the three panels, `panel_header` and `cell`; chip = `s4`. Test `bands_match_the_constants_at_1x_and_follow_the_text_scale` |
| r2-DB-1 | data tab toolbar | info text at `toolbar.y0 + 11/12` | source | `app.rs` | **fixed** — centred on the measured toolbar |
| r2-DB-2 | result strip | message at `x0 + 12` while the grid starts at `x0 + 8` and its cells at `x0 + 18` | compact, laptop | `app.rs` | open — 4 px; pick one left edge for strip and grid text (composition Task 14) |

### code_editor

| id | surface | defect | viewport | file | status |
|---|---|---|---|---|---|
| A1-CE-4 (r1 open) | editor body | fixed `LINE_HEIGHT` 20 under 35 px glyphs at 200 %; hit-testing, caret, selection, diagnostics and scroll all on the same fixed pitch | `large-text.png` | `editor_view.rs` | **fixed** — `line_h` measured with `cell_w` every draw (`line_pitch`: 1× = 20, else line + 1.25 px leading); the 23 pitch sites read it (rows, gutter, hits, caret, selection, diagnostics, scroll steps, popups); glyphs centre on the measured line (`text_dy`, 1.25 px at 1× — replaces the `+2`/`+3`/`+3.5` nudges). Test `line_pitch_matches_the_constant_at_1x_and_follows_the_text_scale` (paint == hit at both scales) |
| r2-CE-1 | completion popup / hover tooltip | rows on a local 20 px const; tooltip lines on `FONT_SIZE + 4` | source | `editor_view.rs` | **fixed** — popup rows on `line_h`, detail centred on the row; tooltip pitch from the measured line (1× = 18) |
| A1-CE-3 (r1 open) | editor tab chrome | `TabBar` pills top-aligned in the 44 px chrome | 1× | `ui::widget::TabBar` | **resolved upstream** — pills now inset 6 px in the allocated 44 px band (`TAB_PILL_INSET_Y`), centred with `+` and the dock toggle (`after/terminal.png`) |
| r2-CE-2 | editor tab pill at 200 % | `m…` pill squeezed | `large-text.png` | `ui::widget::TabBar` | open — needs F6 (pill width from the scaled label) |

### containers

| id | surface | defect | viewport | file | status |
|---|---|---|---|---|---|
| A1-CO-6 (r1 open) | header search launcher | `Search containers…` under the ⌘K cap at 768 and 800 | tablet, `compact.png` | `panels.rs` | **mitigated app-side** — `fit_launcher_placeholder` measures the placeholder at the input's 16 px against the room the field leaves (34 px icon gutter, cap label at 11 px + 2×7 pad + 10 inset, one `s2` gap — mirrored from `ui::widget::search_field`) and falls back to `Search`. Root fix stays **open — needs F2**: `SearchField::build_layout` should reserve the cap in its right padding when `opens_palette()`; then the fallback and the mirrored constants go |
| r2-CO-1 | header title | centred on `centre − 14` (a 1× font-size guess) beside the 38 px launcher | all | `panels.rs` | **fixed** — centred on the measured title line |
| r2-CO-2 | strings | `Search containers…` literal (no `locales/` in the crate) | — | — | open — pre-existing crate-wide i18n gap |

### diffmerge

No open alignment items remained: `merge_view`/`diff_view` `Metrics` from round one survived
the merge (`after/large-text.png`: rows on the scaled pitch, toolbar and chips scaled,
merged band gutter right-aligned). Fresh eyes at 800×700 and 480×900 found the panes,
chips and status labels level. `cargo check --tests` clean; suites re-run (table below).

### api_tester

Nothing open from round one. Fresh eyes at 800×700 / 480×900 / 200 %: toolbar controls on
one centre line, KV rows level, response chrome centred, segments wrap at 480. Every theme
token goes through `renderer_input_style` once (`navrow.rs`, `app.rs`); the one raw
`TextStyle::new(self.text_sm)` feeds `RichTextLayout`, which takes resolved metrics.
Minor: the 1× hamburger bars are not scaled inside the 52 px slab at 200 % (glyph, not
alignment) — recorded.

## App-side double-scaling sweep

`grep typography\.` over the six crates: api_tester (all via `renderer_input_style`),
containers (all via `draw::px`), none in db_client / code_editor / diffmerge / git_client
(literal sizes into the legacy API, scaled once). The one remaining app-side double-scale
was structural, not a token: git_client's `draw::pill` derived a label *size* from a fixed
*height* and then let the renderer scale it (r2-GC-2, fixed).

## Conventions observed

- **One metrics snapshot per surface**, measured with the exact painted style at the top
  of every draw, returned to the owner where scrolling needs it: `diffview::Metrics` →
  `DiffDraw`, `history::Metrics` → `ListDraw`, `changes::Metrics`, `app::BarMetrics`,
  db_client `Bands` (+ same-frame `relayout()`), `GridView::measure_bands`,
  `SchemaTree::measure_bands`, structure `Bands`, code_editor `line_h`/`text_dy`.
- **1× exact by construction**: every measured band is `(measured + padding).max(constant)`
  with the padding chosen from the real 1× line heights (12.5 px mono → 15.625; 13 px →
  16.25; 14 px mono → 17.5; size × 1.25), and every snapshot has a test asserting
  `measure(1×) == Default` plus growth at 2×.
- **Tokens where the value is on the grid** (`spacing.s1/s2/s3/s4/s6`) and a `Theme` is in
  scope; deliberately off-grid 1× rhythm values live in each snapshot's `Default` (or inline
  where the legacy code had them inline), not as new named consts — see "Ratchet".
- Labels are elided, never wrapped, inside a band (`elide_to_width*`, `draw::elide_mono`).

## Ratchet (`ui/tests/f1_spacing_token_audit.rs`)

The F1 ratchet counts named whole-number `const X: f32|f64 = N;` per file against
`ui/tests/f1_spacing_token_baseline.txt` and fails in both directions (growth, and shedding
without lowering the ledger). A local replica (`/tmp/a1r2/ratchet.py`, same regex, same
`#[cfg(test)]` exemption) over the 13 changed files after the pass:

| file | now | baseline |
|---|---:|---:|
| `apps/git_client/src/changes.rs` | 8 | 9 (**lower to 8** — `BADGE_H` replaced by the measured pill) |
| `apps/code_editor/src/editor_view.rs` | 4 | 5 (**lower to 4** — the completion popup's local `ROW_H` is now the measured pitch) |
| all other changed files | = baseline | |

Ground truth — the real test runs on this host (`cargo test -p ui --test it -- f1_spacing`,
`/tmp/a1r2/finish.log`): **no entry grew**; `spacing_literals_do_not_grow` fails only on
four stale (below-baseline) entries, two of them mine and two already stale at the merged
base `381eff6a8` (shed by the merged WIP, ledger not lowered):

```
apps/code_editor/src/editor_view.rs [pixel_const]: now 4, baseline still 5 — lower it to 4   (this unit)
apps/git_client/src/changes.rs      [pixel_const]: now 8, baseline still 9 — lower it to 8   (this unit)
apps/word-processor/src/zoom_control.rs [pixel_const]: now 1, baseline still 2 — lower it to 1   (pre-existing)
shell/src/shell/editor_app.rs       [pixel_const]: now 3, baseline still 4 — lower it to 3   (pre-existing)
```

**Open — needs F1**: those four one-line baseline lowerings (the ledger is in `ui/tests/`,
outside this unit's files). Also for F1: the ratchet has no count-neutral way to *name* a
deliberate off-grid value (a `const` counts, an inline literal or a struct default does
not), and it counts text sizes as spacing; this pass therefore carries some 1× rhythm in
`Default` impls and inline literals where a named const would read better.

## Code review

`/code-review high` over the working tree; disposition:

- ratchet would go red (11 files over) → **fixed** as above; two ledger lowerings recorded for F1.
- containers placeholder measured at 14 px while `TextInput` paints 16 px → **fixed** (measured at 16 with the legacy API, as the widget does).
- launcher fit at the wrong altitude (mirrors `SearchField` constants) → **recorded, needs F2**; kept as the app-side fallback with the mirrored values documented in one place.
- history: first ref pill dropped → empty first line → **fixed** (first pill elides; `painted_any` drives the summary line).
- nine hand-rolled "measure + pad + max" band helpers → **recorded, needs F2/X1**: one `TextRenderer::line_height(size, weight)` + `band_height(line, pad, floor)` in `ui`; this branch keeps the copies but each has a 1×/2× test.
- `elide_mono` dropped the trailing-space trim → **fixed**; mono elide belongs in `TextRenderer` (F2).
- `Metrics::measure` walked every diff line per frame → **fixed** (last numbered line per column, O(1)); test `gutter_digits_come_from_the_widest_column_tail`.
- `pill_height` 14 px → 20.5 → **fixed** (`PILL_LEADING` 2.5: 16/18/20 exact).
- `HeaderLayout` test-only fields → **fixed** (removed; the test derives them from the same helpers).

## Tests

Final run (`/tmp/a1r2/finish.log`, plus the targeted re-runs noted). Every new test was
executed, not only type-checked; `cargo check -p <pkg> --tests` is clean for all six
crates (api_tester, db_client, code_editor, containers, diffmerge, git_client).

| package | command | result |
|---|---|---|
| git_client | `cargo test --lib` (whole crate) | **42 passed**, 0 failed — new: `diffview::tests::{metrics_match_the_constants_at_1x_and_follow_the_text_scale, hunk_header_elides_before_the_unstage_button, gutter_digits_come_from_the_widest_column_tail}`, `changes::tests::{metrics_match_the_constants_at_1x_and_grow_with_the_text_scale, section_header_title_badge_and_action_never_collide}`, `history::tests::row_metrics_match_the_constant_at_1x_and_follow_the_text_scale`; round-one `appbar_*`, `row_layout_*`, `wrapped_appbar_*` still green |
| db_client | `cargo test --lib` (whole crate) | 85 passed, **1 failed — pre-existing**: `app::behavior_tests::host_routes_named_divider_focus_keyboard_and_pointer_reset` fails identically (`left: 195.25, right: 219.25`) with the base `apps/db_client/src` restored (83/1), passes alone and inside `-- behavior_tests::` (20 passed), and with `--test-threads=1`; not a geometry assertion of this unit. New: `grid::tests::bands_match_the_constants_at_1x_and_hits_follow_the_scaled_pitch`, `structure::tests::bands_match_the_constants_at_1x_and_follow_the_text_scale`; round-one `grid::` tests green |
| containers | `cargo test --lib` (whole crate) | **52 passed**, 0 failed |
| diffmerge | `cargo test --lib` (whole crate) | **56 passed**, 0 failed (unchanged crate; gate) |
| api_tester | `cargo test --lib` (whole crate) | **73 passed**, 0 failed, 2 ignored (pre-existing `#[ignore]` capture routes needing `API_TESTER_CAPTURE_DIR`) |
| code_editor | `cargo test --lib -- editor_view:: bottom_panel::` | **28 passed**, 0 failed — new: `editor_view::tests::line_pitch_matches_the_constant_at_1x_and_follows_the_text_scale`; round-one `bottom_panel::` (10) green |
| code_editor | `cargo test --lib` (whole crate) | 226 passed, 1 ignored, **1 failed — pre-existing**: `app::tests::a_document_from_a_previous_session_still_stats_the_file` (macOS `/var` vs `/private/var` path canonicalisation in the merged WIP test; fails identically with the base `editor_view.rs`) |
| ui | `cargo test -p ui --test it -- f1_spacing` | 4 passed, 1 failed — `spacing_literals_do_not_grow`: no growth; 4 stale ledger entries (2 this unit, 2 pre-existing), see "Ratchet" |

Whole-crate runs were used here (the coordinator's memory constraint from round one did
not apply on this host); the two failures are the same tests at the merged base.

## Verification (AFTER captures)

74 PNGs under `target/ui-review/<app>/after/` (dark + light matrix, `large-text.png`
800×900 @ 2×, `narrow.png` 480×900, `compact.png` 800×700, scenarios at 1200×800), every
one opened. BEFORE counterparts under `…/before/`.

- **git_client** — `after/large-text.png`: diff rows on the measured pitch with no
  overprint; `Unstage hunk` buttons as tall as their rows with the `@@ … @@` headers elided
  before them; `renderer` + `main` pill + `8 pending changes` stacked without overlap;
  section badges sized from their labels and clear of `Unstage all`/`Stage all`; commit box
  author → input → button rows separated. `after/dark/dev.qos.git-client-phone.png` (390),
  `after/light/…-phone.png`, `after/narrow.png` (480): hunk headers end in `…` before the
  button; two-row bar unchanged. `after/shots-narrow.png` (680, flap open): hamburger
  repainted above the flap at its placed rect. `after/dark|light/…-{tablet,laptop,desktop}.png`,
  `after/compact.png`: identical to BEFORE at 1× (title block within 1.5 px, badges 16 px,
  rows 30/18/68 px) in both themes.
- **db_client** — `after/large-text.png`: tab bar, Run/Snippets toolbar and result strip
  grown with their labels, grid rows with leading, toggle square in the bar.
  `after/dark|light/…-laptop.png`, `…-desktop.png`, `after/compact.png`, `after/narrow.png`,
  phone/tablet cells: 1× unchanged (38/40/26 px bands, 26/30 px grid, tree rows 24 px).
- **code_editor** — `after/large-text.png`: editor rows on the measured pitch (A1-CE-4
  closed), gutter numbers on the same rows. `after/terminal.png`: dock tabs centred on the
  strip (round one survived), `main.rs` pill centred in the 44 px chrome. `after/selection.png`,
  `after/find.png`, `after/status.png`: selection band = row pitch; find-bar controls on one
  centre line; status pill unchanged. Phone/tablet/laptop cells in both themes and
  `after/narrow.png`/`after/compact.png`: 1× glyphs sit 0.75 px higher (centred on the
  measured line) — otherwise identical to BEFORE. Desktop cells absent (rasteriser limit).
- **containers** — `after/dark|light/…-tablet.png` (768) and `after/compact.png` (800):
  `Search` beside a clear ⌘K cap; title centred on the launcher's line.
  `after/dark|light/…-{laptop,desktop}.png`: full `Search containers…` placeholder retained.
  `after/large-text.png`, `after/narrow.png`, phone cells: compact detail page unchanged.
  `after/confirm.png`, `after/prompt.png`: shared dialogs unchanged.
- **diffmerge** — all 14 identical to BEFORE (unchanged crate): `after/large-text.png`
  scaled rows/toolbar/chips, `after/compact.png` chips in-pane, `after/narrow.png` compact
  source switcher, `after/diff.png` gutter right-aligned, `after/merge.png`/`after/status.png`
  status pill; both themes at all four widths.
- **api_tester** — all 11 identical to BEFORE (unchanged crate) in both themes:
  `after/large-text.png` square hamburger + scaled checkbox column; `after/narrow.png`
  wrapped response segments; laptop/desktop sidebar rows level.

## Limits

- code_editor's 1920×1080 matrix cell still panics in the vendored vello CPU rasteriser
  (`tile_alloc.rs:52`) in both themes, BEFORE and AFTER — evidence limit, not an app defect;
  the other three cells and the 1200/800/480 shots cover the app.
- The `ui` `it` target's `f1_spacing` result is reported from the run in `/tmp/a1r2/finish.log`
  (see Tests); the two baseline lowerings it asks for are outside this unit's files.
- db_client's 200 % capture is the 800 px overlay layout (sidebar hidden); the schema tree
  and structure changes are verified at 1× (laptop) and by their 1×/2× unit tests.
- `git_client`'s `--shots-narrow` (680, flap open) and the phone cell verify the hunk-header
  elision; the Panels screen is verified by source and its pill/pitch helpers' tests.
