# A8 — Media: music, photos, video-player, camera, tube

UI element alignment / spacing / layout review, 2026-09-16 (worker A8 of 18).
Branch `ui-review/a8-media`, base `codex/macos-shell` @ `087d5a62c` (+ the
one-line `Cargo.lock` libc fix, `ui-review/lockfix`). Delivery is local only.

Captures live under `target/ui-review/<app>/{before,after}/` in this worktree;
`before` is the corrected base, `after` is this branch. Every PNG named below
was opened and inspected.

## Method

1. Read `CLAUDE.md`, `ui/HIG.md` §1e/§7/§7a, `ui/MANUAL_LAYOUT.md`,
   `docs/hand-rolled-ui-audit.md`, review-1/2/3, corrected-review-1/2, the
   closure register and plan `2026-09-15-app-text-geometry.md` tasks 4/5/16/19.
2. Source pass over each app's layout files: private spacing consts, literal
   offsets, font-size centring (`size × k`), sibling gutter mismatches, split
   geometry descriptions (paint vs hit vs a11y).
3. BEFORE captures: `--responsive-matrix` dark+light, 800×900 @ `--text-scale 2`,
   480×900, 800×700, `--pseudo-locale`, every named scenario at 1200×800
   (`target/ui-review/cap.sh`, driven by `all.sh`).
4. Fixes with measured text, shared widgets and one geometry per screen;
   regressions with the real five-pass frame; AFTER captures compared.

## Framework finding (Open — needs F1) — the tube 200% P1

review-3's P1 ("card titles, channel names and view counts paint over one
another at 2×") reproduces on this base (`tube/before/large-text.png`,
`large-search.png`). Plan task 16 asked for a measure-first trace before any
layout code moved. Trace (tests in `apps/tube/tests/large_text_layout.rs`,
run with `--nocapture`):

| Probe (800×900, text scale 2) | laid-out height | renderer measures |
|---|---|---|
| bare `Text("Low Level Learning", 12.5)` inside app-local `Flex` | 31 px | 31.25 px ✓ |
| same lines inside `Flex::grid(300)` (CSS grid, as `screens::grid`) | 109 / 32 px | 108.75 / 31.25 ✓ |
| that grid inside shared `Scrollable` (as `screens::home`) | 109 / 31 px | ✓ |
| the real card inside `TubeApp` (hosted by shared `NavSplit`) | **37 / 15 / 16 px — identical to 1×** | 181.25 / 31.25 ✗ |

The frame's renderer is at scale 2 in every probe (`Harness::text_scale()`),
so the 1× rects come from a different renderer: `ui/src/widget/nav_split.rs`
`NavSplit::apply_layout` → `reapply_children` → `place_children(&Theme::dark(),
&mut TextRenderer::new())` (lines ≈506–527, 625–640) lays out both panes with an
**unscaled, default-theme renderer** whenever the split geometry changes, and
`self_tick` only re-places while the flap animates. Every `Text` under the split
is therefore measured at 1× and painted at 2× — the overlap in the PNGs.
`Text`, `Flex`, the grid and `Scrollable` are all correct; tube cannot fix this
inside `apps/tube/`. Repair belongs to F1 (NavSplit must place children with the
host's theme/renderer, e.g. defer `place_children` to the first `draw` or take a
renderer-aware layout hook). The tests above stay green as the measure-first
evidence; the "2× lines taller than 1×" acceptance check is written up here and
belongs with the F1 fix.

The same root cause explains the 2× rail labels overlapping the icons
(`nav_item` is inside the split) and the search chips' labels overflowing
(`search_type_chips`: chips 30 px tall at both scales).

## Findings

Status: **fixed** (this branch), **open** (owner named), **noted** (accepted /
judged by outcome per HIG §7a).

### music (`apps/music/src/app.rs`, `apps/music/src/app/render.rs`)

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| M1 | header vs body | page title at `r.x0+24`, search field to `r.x1-24`; every body gutter (`pad = 28.0`: section labels, chips, grids, track table) is 28 → title 4 px left of the content it names, field 4 px outside | 1200×800 dark (`sidebar.png`, `album-page.png`, `artist-page.png`), 800×700, 480×900 | render.rs:2695, :2769, :2750 | `music/before/sidebar.png` → `music/after/sidebar.png` | fixed — `CONTENT_GUTTER = 28` shared by header lead, pill, compact button; body `pad` values name the same distance (recorded as the pane's authored off-scale gutter) |
| M2 | track table | `TIME` heading left-aligned at `cols.dur` while values right-align to `cols.dur+48` → heading 23 px left of its digits | 1200×800 (`nowplaying.png`) | render.rs:3762 | `nowplaying.png` → `after/nowplaying.png` | fixed — heading right-aligned to `DURATION_SLOT_W` edge from measured width |
| M3 | durations / timecodes | proportional digits: `m:ss` columns and the ticking elapsed time jitter; rows centred with `vy()` (`size×1.25`) | all | render.rs:4258, :5844, :7581 | same | fixed — `numeric_style()` = `TextStyle` with `NumericStyle::TabularLining`, measured **and** drawn with one style, centred on measured line height (track table, queue, now-playing scrubber) |
| M4 | header search field / compact search button | filled with `c.panel` = `surfaces.base`, which equals the header background in the light theme → invisible until hover/focus | 1366×768 light (`light/dev.qos.music-laptop.png`) | render.rs:2773, :2753 | `before/light/…laptop.png` → `after/light/…laptop.png` | fixed — raised tier `c.card` (`surfaces.raised`, same as chips/cards) |
| M5 | track context menu | hand-rolled "nudge to stay in app rect": guessed `220×(rows·28+24)`, laid out against `Theme::dark()` regardless of live theme (`clamp_menu_origin`, `layout_context_menu`) | `context.png` | app.rs:4256, :4267 | n/a (geometry) | fixed — `ContextMenu::open_anchored(pointer, self.rect)`; the shared menu measures with the frame renderer/theme, flips and clamps. Regression `track_context_menu_stays_inside_the_app_rect_from_every_corner` |
| M6 | large-text frame (≥1.35) | header/search/player at 28, rows at 16+18 = 34 → row text 6 px in from the title, durations 6 px in from the search button; player art at 20; title/search `y` literals assumed `r.y0 == 0` | 800×900 @2× (`large-text.png`) | render.rs:2236, :2243, :2301, :2341, :2371, :2399 | `large-text.png` → `after/large-text.png` | fixed — one gutter; tabular durations; header title/search centred on measured height relative to `header.y0` |
| M7 | lyrics | fixed 34 px pitch with the 21 px active line top-anchored → next line crowded by ~4 px | `lyrics.png` | render.rs:6414 | `lyrics.png` → `after/lyrics.png` | fixed — each line centred in its slot on measured height |
| M8 | large-text route parity | at ≥1.35 the frame drops navigation and search content (corrected-review-2 P1, plan task 4) | 2× | render.rs:2090 | — | open — not an alignment defect; task 4 pending, app-owned |
| M9 | now-playing bar | transport buttons at `y0+30`, scrubber `y0+64`, art/queue/volume on the midline `y0+46` | 1200×800 | render.rs draw_nowplaying* | — | noted — a two-row transport block whose composite centre sits on the bar midline; reads as intended in the PNGs |
| M10 | `vy()` | centres on `size × 1.25`; exact at 1× (renderer default line height 1.25), drifts only in 1.0–1.35 where the normal frame is still used | — | render.rs:207 (28 call sites) | — | noted — replaced with measured centring where touched (durations, timecodes, lyrics, large-text title); remaining sites are literal-size labels whose 1× geometry is exact |
| M11 | strings | music has no fluent bundle; every label is a literal (pseudo-locale only localises the shared `TextInput` placeholder) | `pseudo.png` | — | — | noted — pre-existing, out of alignment scope |

### photos (`apps/photos/src/app.rs` — WIP file in the user's checkout; edits kept local and commented)

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| P1 | toolbar at 2× | shared `Button` faces fixed at `BTN_H = 34`; 28 px labels collide with the capsule edge ("More") | 800×900 @2× (`large-text.png`) | app.rs:57, :3870 | `photos/before/large-text.png` → `photos/after/large-text.png` | fixed — `btn_h(text_scale)` = authored 34 at 1×, else label line (14 × scale × 1.25) + 2·8; strip height follows (`BAR_H − BTN_H` preserved); `refit_toolbar` rebuilds on scale change; icon-only items stay square |
| P2 | status band at 2× | `BAR_H·0.6 = 28.8` band, 13 px text top-anchored at +4 → filename clipped (plan task 19) | 800×900 @2× | app.rs:2559, :4373 | same | fixed — `status_h(text_scale)`; text centred on measured line height |
| P3 | library card footer | Details inset 8 from `card.x0`, favourite/remove flush to `card.x1` → faces 12 vs ~5 px from the edges; geometry duplicated in paint, pointer hit, a11y tree and `library_card_action_rects` (4 copies) | 1200×800 (`library.png`) | app.rs:2918–2936, :826, :1917–1959, :4711 | `library.png` → `after/library.png` | fixed — one `card_action_controls(card)` (`CARD_ACTION_EDGE_INSET = 8`) consumed by all four; regression `library_card_actions_share_one_edge_inset` |
| P4 | library header pager | "1–5 / 5" left-anchored at `x1 − PAD − 100` → ends ~60 px short of the grid's right edge | `library.png` | app.rs:2649 | same | fixed — measured width, right-aligned to `bounds.x1 − PAD` |
| P5 | album editor | "N selected" left-anchored at `panel.x1 − 116`; row-mate Next button ends at `panel.x1 − 16` | `album-manage.png` | app.rs:3246 | `album-manage.png` → `after/album-manage.png` | fixed — measured, right-aligned to the sheet inset |
| P6 | library card | title at `x + 9` vs thumbnail at `+8` (1 px) | `library.png` | app.rs:2939 | — | noted — sub-pixel at 1×, judged by outcome |
| P7 | metadata sheet | label/field/Save all on the sheet's 16 px inset | `metadata-edit.png` | — | — | no defect |

### video-player (`apps/video-player/src/app.rs`)

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| V1 | floating top strip | title centred on `title_size` in a 44 px scrim (centre ≈24) while Library/Open sit at `OVERLAY_MARGIN..+OPEN_BTN_H` (centre 30); buttons overhang the scrim by 2 px; at 2× the 30 px title leaves the scrim and 28 px labels jam 32 px capsules | 1200×800 (`playing.png`), 480×900, 800×900 @2× (`large-text.png`) | app.rs:1612, :1631, :2280–2289 | `video-player/before/playing.png` → `after/playing.png`; `before/large-text.png` → `after/large-text.png` | fixed — `TopStrip::at(y0, text_scale)`: one `center_y`, capsule = authored 32 until the label line needs more, scrim contains the buttons; title centred on **measured** height; widths scale with text. Regression `top_strip_is_one_centre_line_that_contains_its_buttons` (1× and 2×) |
| V2 | library panel | three left insets (title 16, tabs 12, row text 24) and two right (Play 14, Close 12); `panel_text` offset the top by `size` as a hand-rolled baseline and **discarded the requested weight** (title floated 17 px low, painted Regular); geometry split between `apply_layout` and paint | `library.png` | app.rs:162–181, :1843–1884, :1937–1944, :2301–2318 | `library.png` → `after/library.png` | fixed — `LibraryPanelGeometry::for_panel` (gutter 16, row inset 8+8) read by `apply_layout` and paint; `panel_text` centres on measured line height and honours weight. Regression `library_panel_chrome_shares_one_gutter` |
| V3 | transport (shared `VideoPlayer`) | at 2× the "0:08 / 2:00" label sits ~10 px below the control centre line | `large-text.png` | `ui/src/widget/video_player.rs` | — | open — needs F5 |
| V4 | settings popover (shared) | rows aligned | `settings.png` | — | — | no defect |

### camera (`apps/camera/src/app.rs`, `apps/camera/src/paint.rs`)

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| C1 | top setting pills | `ctrl_pill` height fixed 30, label at `centre − 8` (font-size centring) → 24 px labels spill ~7 px below the pill at 2× (corrected-review-1 P2, plan task 5) | 800×900 @2× (`large-text.png`) | app.rs:3320–3321, :3365 | `camera/before/large-text.png` → `camera/after/large-text.png` | fixed — height = max(30, measured label + 2·7.5); label centred on measured height |
| C2 | mode switcher | 34 px segments, `text_in` centred on `size × 1.2` → "Photo" below its selected background at 2× | same | app.rs:2550, paint.rs:196 | same | fixed — `text_in` centres on measured height (12 callers); switcher grows **upward** from a fixed bottom edge (the shutter's top, unchanged at 1×) |
| C3 | corner controls | top pills/settings on `EDGE = 18`; gallery thumb at `EDGE + 8`; lens switch right edge at `x1 − 34` | 1366×768 (`dark/…laptop.png`) | app.rs:3500, :3611 | `dark/dev.qos.camera-laptop.png` → `after/dark/…laptop.png` | fixed — all four corners on `EDGE` (hit regions follow, they are the same rects) |
| C4 | gallery / detail panel | 24 px gutter shared by back button, grid and panel; label/value rows right-aligned to the panel inset | `gallery.png` | — | — | no defect |
| C5 | top pill row `y = y0 + 20` vs `EDGE = 18` | 2 px | — | app.rs:3380 | — | noted — settings button shares the row's centre line; not visible |

Regression: `viewfinder_capsules_contain_their_labels_at_accessibility_scale`
(1× keeps 30/34 and the shutter anchor; 2× capsules contain measured labels;
corner centres on `EDGE`). Hit regions are the painted rects grown to the 44 px
touch minimum, so the test reads centres at 1× and heights at 2×.

### tube (`apps/tube/src/screens.rs`, tests)

| id | surface | defect | viewport/theme | file:line (before) | before → after | status |
|---|---|---|---|---|---|---|
| T1 | home/section headers | heading icon at x=110 and trailing label to 1162 while card content spans 116…1156 (cards pad 6 inside the grid) | 1200×800 (`home.png`) | screens.rs:222, :278 | `tube/before/home.png` → `tube/after/home.png` | fixed — `CARD_PAD = 6` shared by cards and `section_header` (`pad_xy(CARD_PAD, 0)`) |
| T2 | everything under `NavSplit` at 2× | 1× measured heights, 2× glyphs (see framework finding) | 800×900 @2× (`large-text.png`, `large-search.png`) | ui/src/widget/nav_split.rs | — | open — needs F1 |
| T3 | app-local `Flex` gaps/pads (3, 5, 9, 10, 18) | off the 4 px scale | — | screens.rs video_card/grid | — | noted — authored values, even gaps (30/31 px) and clean 1× grid in the PNGs; judged by outcome, left in place |
| T4 | top bar | search field, Offline pill and avatar share centre y=33 | `home.png` | — | — | no defect |

## AFTER verification (PNGs opened)

- `music/after/nowplaying.png`: "Songs" at x=264 on the body gutter, search
  field ends at 1172, `TIME` ends with the durations, digits tabular.
- `music/after/light/dev.qos.music-laptop.png`: search field visible (raised
  white surface), right edge on the grid edge.
- `music/after/large-text.png`: "Home", row titles and "Nothing playing" share
  x=28; durations end at 772 with the search button.
- `photos/after/library.png`: star faces 12 px from the card edge like Details;
  "1–5 / 5" ends on the grid's right edge (1188).
- `photos/after/large-text.png`: 51 px capsules hold "More"; status band
  contains the filename line.
- `video-player/after/playing.png`: Library / title / Open on one centre line
  (y≈30) inside a 60 px scrim. `after/library.png`: title, subtitle, tabs and
  row text at x=856; Play and Close end at 1184; title Semibold on Close's line.
  `after/large-text.png`: strip grows to 72 px, labels contained; the shared
  transport's low time label remains (F5).
- `camera/after/large-text.png`: pills and the switcher contain their labels;
  "Photo" inside its selected background; bottom corners on `EDGE`.
  `after/dark/dev.qos.camera-laptop.png`: 1× heights unchanged (30/34), thumb
  at x=18, lens ends at x=1348. A second pass (after the code review) made the
  icon-only grid pill and the settings button share the row height/centre; the
  camera set was recaptured after that change.
- `tube/after/home.png`: heading icon at x=116 and trailing label ending at
  1156 — the card content's edges.

## Code review (`/code-review medium`)

Interim findings, addressed before the first commit: `TopStrip` doc named a
consumer it does not have plus a no-op comment; photos compared two outputs of
one pure function with an epsilon; camera measured a literal `"Photo"` instead
of the labels it draws (now the max over `items`); music duplicated the tabular
duration block three times (`draw_duration_right_aligned`).

Final findings (six), addressed in the follow-up commit:

1. photos `toolbar_width`/`faces_for_width` still sized items at the 1×
   `BTN_H` while the strip was built at `btn_h(scale)` → for scales in
   (1.0, 1.25] the fit decision could pick a face set the strip cannot hold.
   Fixed: `toolbar_width_at(faces, btn_h)` / `faces_for_width(width, btn_h)`;
   the public `toolbar_width(faces)` keeps its 1× contract for the tests.
2. video-player library rows elided the title at Regular but `panel_text` now
   draws Medium (wider) → a just-fitting title could run into Remove/Play.
   Fixed: `elide_to_width_weighted(…, TextWeight::Medium)`.
3. tube `card_meta_lines_stack_without_overlap_at_2x` was named for the 2×
   overlap it cannot observe (NavSplit measures at 1×). Renamed to what it
   asserts (`card_meta_rects_stay_stacked_in_order_at_2x_layout_pass`); the
   two bisection probes became permanent, plainly-named regressions for the
   app-local `Flex`/grid/`Scrollable` measuring at renderer scale; all
   `eprintln!`s removed — the numbers live in this report. The red-until-F1
   acceptance check ("2× lines taller than 1×") is stated in the framework
   section above.
4. video-player's strip test re-derived the label line with the strip's own
   formula. Fixed: it measures `"Library"` with a real 2× `TextRenderer`. Both
   apps' local `RENDERER_LINE_HEIGHT = 1.25` copies replaced by the public
   `TextStyle::new(size × scale).line_height_px()`.
5. `panel_text` drew with `max_width: None`, so the 360 px panel's empty-state
   copy ran past the panel edge (pre-existing, touched). Fixed: measure and
   draw bounded to the rect width (wraps); the empty-state band is tall enough
   for the wrapped block.
6. Open/Library capsule widths scaled the whole authored 84/88 px by the text
   scale, starving the title on narrow surfaces at 2×, and the Library rect
   could invert. Fixed: `capsule_w()` scales only the label part beyond the
   shared Button's 30 px horizontal padding; Library's `x1` is clamped to
   `>= x0`. A measured-width variant needs a renderer in layout (F2/F1).

Kept as-is with reasons: `#![allow(dead_code)]` on tube's shared test harness
(each test binary uses a subset); `min(budget)` on the measured photos labels
(word-wrap can exceed `max_width`); music's remaining `vy()` sites (exact at
1×, literal sizes). Not done: retiring photos' `library_details_cards` state in
favour of `card_action_controls` — a larger refactor of a file with live WIP in
the user's checkout. The reviewer could not run `cargo check` on its host
(`ring`); every finding was re-verified here by running the suites.

## Conventions observed

- One geometry description per screen where it was split: video-player
  `TopStrip` / `LibraryPanelGeometry`; photos `card_action_controls`; music
  `CONTENT_GUTTER` / `DURATION_SLOT_W`; camera `EDGE` for all corners.
- Vertical centring from the renderer's measured line height (`measure`,
  `measure_weighted`, `measure_style`) — never `size × k`.
- Digits that line up use `NumericStyle::TabularLining`, measured and drawn
  with the same `TextStyle`.
- Scale-aware capsules keep the authored 1× size and grow only when the
  measured label needs it (photos toolbar/status, camera pills/switcher,
  video-player strip). Where a layout pass has a theme but no renderer
  (photos `build_layout`, video-player `apply_layout`), the line height is
  derived as `label size × theme.text_scale × 1.25` (the renderer's default
  line height) and the draw pass centres on the actually measured height.
- Shared widgets over hand-rolled copies: music's menu placement now uses
  `ContextMenu::open_anchored`; the `Theme::dark()` layout shim is gone.
- Deliberately off-scale, named and kept: music `CONTENT_GUTTER = 28`,
  `NOWPLAYING_H = 92`, `HEADER_H = 72`; camera `EDGE = 18`; video-player
  `OVERLAY_MARGIN = 14`; tube `CARD_PAD = 6`, grid gap 18.

## Tests

Environment: macOS aarch64. The worktree sits under `/Users/xaver/dev/qos/`,
whose `.cargo/config.toml` cargo merges and which appends `-C target-cpu=generic`
(a `ring` const-assert then fails). Per the coordinator, every cargo command was
prefixed with `RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig RUSTC_WRAPPER=sccache
SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1` and `--no-default-features --locked`;
no cargo config was edited.

New tests (all run, all pass):

| package | test | result |
|---|---|---|
| camera | `app::tests::viewfinder_capsules_contain_their_labels_at_accessibility_scale` | 1 passed |
| photos | `app::tests::toolbar_capsules_and_status_band_contain_their_text_at_any_scale`, `app::tests::library_card_actions_share_one_edge_inset` | 2 passed |
| video-player | `app::tests::top_strip_is_one_centre_line_that_contains_its_buttons`, `app::tests::library_panel_chrome_shares_one_gutter` | 2 passed |
| music | `tests::track_context_menu_stays_inside_the_app_rect_from_every_corner` | 1 passed |
| tube | `tests/large_text_layout.rs`: `card_meta_lines_stack_without_overlap_at_1x`, `card_meta_rects_stay_stacked_in_order_at_2x_layout_pass`, `text_in_flex_measures_at_renderer_scale`, `text_in_grid_and_scrollable_measures_at_renderer_scale`, `search_header_chips_and_summary_do_not_collide_at_2x` | 5 passed |

Compile gates: `cargo check -p {music,photos,video-player,camera,tube} --tests`
— all five clean (re-run after `rustfmt`).

Full per-package suites (`cargo test -p <pkg> --no-default-features --locked`),
FINISH run, all green, no pre-existing failures:

| package | binaries | passed |
|---|---|---|
| music | lib (+2 empty) | 211 |
| photos | lib 90, `end_to_end` 20, `one_photograph_one_address` 5 | 115 |
| video-player | lib (+3 empty) | 51 |
| camera | lib (+2 empty) | 90 |
| tube | lib 134, two pre-existing integration binaries 1 + 23, `large_text_layout` 5 | 163 |

## Limits

- **Base correction.** The worktree was first cut from `24ee08c66`
  (`origin/main`'s line); the coordinator had it rebased onto
  `codex/macos-shell` @ `087d5a62c` before any edits. Captures and findings
  from the wrong base were discarded and redone.
- macOS: `ring`/linker flags as above; `--no-default-features` (no GStreamer),
  so video-player's real decode path was not exercised — the review is about
  chrome geometry, which the synthesized engine renders identically.
- Camera has no `--mode video` / `--drawer` shot flags on this base; video mode
  and the timer overlay were reviewed in code only.
- Tube `--live-shots` is network-bound and was not used; routes came from the
  offline sample fixtures.
- Text scale on this base composes theme tokens *and* renderer scale; the
  review judged containment at whatever the resulting glyph size is.
- Not visually re-verified after the change: music `context` near the viewport
  edges (proved by test instead), camera video/scan modes at 2×.
