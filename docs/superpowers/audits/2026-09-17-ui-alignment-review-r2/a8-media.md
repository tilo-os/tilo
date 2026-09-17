# A8 — Media, round two: music, photos, video-player, camera, tube

UI element alignment / spacing / layout review, 2026-09-17 (worker A8 of 19,
round two). Branch `ui-review/r2-a8-media`, base `381eff6a8` (the merged
round-one integration tip). Delivery is local only.

Captures live under `target/ui-review/<app>/{before,after}/` in this worktree;
`before` is the merged base, `after` is this branch. Every PNG named below —
68 scenario shots + 34 responsive-matrix shots before, the same after, plus
the extra camera/video-player captures added mid-review — was opened and
inspected.

## Method

1. Read round one's index and A8 report, F1's and F5's notes on the two
   cross-unit items (`NavSplit` 1× measurement; the transport time label).
2. Merge verification: each round-one geometry helper (`card_action_controls`,
   `btn_h`/`status_h`, `TopStrip`, `LibraryPanelGeometry`, `CONTENT_GUTTER`,
   `DURATION_SLOT_W`, `ctrl_pill_height`, `CARD_PAD`) is present in the merged
   tree; the five round-one suites (211 / 115 / 51 / 90 / 163) run green on
   the base; the round-one AFTER geometry reproduces in the BEFORE captures
   (photos card action inset, pager, capsules; video-player strip centre line;
   camera corner insets; music gutter and tabular durations).
3. BEFORE captures with the shared `cap()` helper (`target/ui-review/cap.sh`),
   plus 2× variants of the key scenarios, tube's offline `--route`s, camera's
   `--screen`s, and three extra camera poses (480 px gallery, 1200×800 2×
   gallery and scanner) added once the first pass showed where to look.
4. Source pass over the 2× paths: literal offsets that only fit one scale,
   fixed heights under measured lines, hand-rolled widths.
5. Fixes with measured line boxes and one geometry description per surface;
   regressions with the real five-pass frame; `/code-review medium`; AFTER
   captures compared.

## Round one survived the merge

photos carried the user's WIP (`0b08f8d2a wip: checkpoint`) into the merge on
top of A8's `b4701f794`. All round-one helpers are intact and the round-one
regressions pass (`library_card_actions_share_one_edge_inset`,
`toolbar_capsules_and_status_band_contain_their_text_at_any_scale`). The
BEFORE captures show the round-one geometry: card Details/star both 12 px in
from the card edge, pager ending on the grid edge, 51 px toolbar capsules at
2×. Nothing to redo.

## Cross-unit items verified (not this unit's files)

| item | owner | state on this base |
|---|---|---|
| tube: everything under `NavSplit` measured at 1× and painted at 2× | F1 (`ui/src/widget/nav_split.rs`) | **still present** (`tube/before/home-2x.png`, `after/home-2x.png` identical). Tube's own `Flex`/grid/`Scrollable` measure at renderer scale: `apps/tube/tests/large_text_layout.rs` 5/5 green before and after this round. |
| video-player: transport time label at 2× | F5 (`ui/src/widget/video_player.rs`) | the label holds the bar's centre line (F5-10 fixed) but paints at 4× (48 px) — F5-24, the `--shots` host scaling theme and renderer both (`video-player/after/playing-2x.png`). Recorded; not in this unit's files. |

## Findings

Status: **fixed** (this branch), **open** (owner named), **noted** (accepted).

### camera (`apps/camera/src/app.rs`, `apps/camera/src/main.rs`)

| id | surface | defect | viewport/theme | file:line (after) | before → after | status |
|---|---|---|---|---|---|---|
| C-r2-1 | Photo / Video / Scan switcher | capsule `clamp(150, 240)` split into three equal segments at every scale → 80 px segments crowd 2× labels ("Photo" touches "Video") | 800×900 @2× | app.rs:2658 (`segmented_min_w`), `draw_mode_switcher` | `camera/before/large-text.png` → `after/large-text.png` | fixed — capsule = max(authored, n × (widest measured label + 2·12)); 1× stays 240/3 |
| C-r2-2 | scanner title card (`draw_glass_header`) | fixed 52 px box, title at +12 / subtitle at +32 → 2× subtitle paints below the card; at 1× the block sat 3 px low | 800×900 @2× (`scanner-2x.png`) | app.rs:3925 | `before/scanner-2x.png` → `after/scanner-2x.png` | fixed — measured title + subtitle block centred, box grows past 52 when needed (1× block now centred: title top 30 vs 28) |
| C-r2-3 | "QR Code detected" tag | fixed 20 px tag under a 27 px 2× line | same | app.rs:4100 | same | fixed — height = max(20, line + 2·3) |
| C-r2-4 | scan result card | fixed 122 px: title at +16, summary at +39, actions at y1−42 → 2× title over summary, buttons over summary | same | app.rs:4251 | same | fixed — measured title/summary/action stack with the authored gaps; card = max(122, stack); actions = max(30, line + 14) |
| C-r2-5 | recent-scans panel | rows 44 px with summary at cy−14 and time at cy+2; empty state's second line at a fixed +20 → 2× lines overlap | 1200×800 @2× (`scanner-wide-2x.png`) | app.rs:4358 | `before/scanner-wide-2x.png` → `after/scanner-wide-2x.png` | fixed — measured block centred in max(44, block + 12) rows; empty-state second line follows the first's measured height |
| C-r2-6 | gallery header | "Library" 20 px at +16 and count at +44 → 2× title paints over "9 items"; grid at a literal +64/+104 | 800×900 @2× (`gallery-2x.png`) | app.rs:4512 (`GalleryChrome`) | `before/gallery-2x.png` → `after/gallery-2x.png` | fixed — one measured description (`GalleryChrome::measure`) read by header, filter and grid: count under the measured title, grid under the taller of header and filter; 1× values 16 / 44 / 64 unchanged |
| C-r2-7 | gallery filter | `available.min(320)` split four ways → 2× "Videos" runs into "Favorites"; the beside/below decision was `width < 460` regardless of the measured title, so at 480 px the filter sat on the title's row and touched it | 800×900 @2×; 480×900 (`gallery-narrow.png`) | same | `before/gallery-narrow.png` → `after/gallery-narrow.png` | fixed — capsule ≥ measured labels; beside the title only when measured title + gap + capsule fit, else on its own row under the count (480 px: row 64..96, grid 112 vs 104 — the 1× narrow filter used to start 3.6 px into the count's line box) |
| C-r2-8 | gallery grid scrollbar | 3 px thumb painted 3–6 px inside the grid edge, over the last column's cards | 480×900 | app.rs:4878, :105 | same | fixed — when the roll overflows, cards give up a 10 px gutter; layout re-solved at the narrower width |
| C-r2-9 | gallery card captions | 44 px strip, label at y1−34 and time at y1−18 → 2× lines overlap and leave the card | 800×900 @2× | app.rs:4959 | `before/gallery-2x.png` → `after/gallery-2x.png` | fixed — measured label + time block centred in max(44, block + 16) |
| C-r2-10 | gallery card tags ("Unavailable", duration) | fixed 22 px, "Unavailable" a fixed 83 px wide | same | same | fixed — height = max(22, line + 6); width measured |
| C-r2-11 | gallery detail info | title at +0, time at +24, key/value rows on a 26 px pitch → 2× lines overlap (`gallery-detail-2x.png`) | 1200×800 @2× | app.rs:5198 | `before/gallery-detail-2x.png` → `after/gallery-detail-2x.png` | fixed — each line advances by its measured height plus the authored 1× gap (2.75 / 13 / 10.4) |
| C-r2-12 | detail actions | 38 px buttons, label at cy−7 (font-size centring) | same | app.rs:5258 | same | fixed — max(38, line + 16); label centred on its measured line beside the 16 px glyph |
| C-r2-13 | video mode | no shot route on the base; reviewed in code only in round one | — | main.rs `--mode`, app.rs:2091 | `after/video-mode.png`, `after/video-mode-2x.png` | fixed — `--mode photo|video` (`CAMERA_MODE`) poses the capture sub-mode; unknown values ignored like `CAMERA_SCREEN`; captured at 1× and 2×: red shutter core, "Video" segment selected, switcher contains its labels |
| C-r2-14 | scan result card close glyph | a faint mark left of the X (`scanner.png`) | 1200×800 | — | `before/scanner-crop-x.png` | noted — the scan window's bracket showing through the 92 % card; not a defect |

Regressions: `mode_switcher_segments_hold_their_measured_labels_at_2x`
(1× keeps 240/3 = 80 px segments; 2× segments ≥ widest measured label + 24;
read from the accessibility tree's `camera.mode:*` / `camera.nav:scanner`
bounds), `gallery_chrome_keeps_title_count_filter_and_grid_apart` (1×: 16 /
44 / 64, filter 16..48 × 320 beside the title; 480 px: filter below the
count's line box, grid below the filter; 2×: count under the measured title,
filter segments hold the widest label, grid under both).

### video-player (`apps/video-player/src/app.rs`)

| id | surface | defect | viewport/theme | file:line (after) | before → after | status |
|---|---|---|---|---|---|---|
| V-r2-1 | floating capsules (Library / Open) | widths = authored label part × theme factor (round-one interim) | all | app.rs:77 (`capsule_w`), :89 (`StripLabels`), :1861, :1883 | `video-player/before/playing.png` → `after/playing.png` | fixed — `draw` measures both labels with the frame's renderer and caches them per text scale; `place_top_strip` (the only writer of both rects) re-places when the cache changes, so hit rect == painted rect. The capsule is the shared Button's fit width (15 px a side) around the measured label: Open 66 px at 1× (authored 84 was wider than its label needed) — a deliberate 1× change |
| V-r2-2 | library panel at 2× | `LibraryPanelGeometry` literals (28 / 38..54 / 66..96 / 108 / 62 px rows / 48 & 50 px controls) → title over subtitle, subtitle over tabs, "Remove" over "Play", 1× pitch under 2× glyphs; 360 px panel | 800×900 @2× (`library-2x.png`) | app.rs:177 (`PanelLabels`), :194, :1900, :1927 | `before/library-2x.png` → `after/library-2x.png` | fixed — every part keeps its authored 1× size as a floor and grows from the measured line; measured label widths cached per (scale, panel width) by paint and re-placed through `place_library_panel`; panel = 360 × text scale (≥ 640 px surfaces). 1× geometry unchanged (asserted) |
| V-r2-3 | strip over the video, light theme | scrim a literal `rgba(0,0,0,90)`, title a literal near-white, and the ghost "Library" face draws `text_primary` — near-black on black in light themes (`light/dev.qos.video-player-laptop.png`) | 1366×768 light | app.rs:166, `paint_overlay` | `before/light/…laptop.png` → `after/light/…laptop.png` | fixed — scrim = `surfaces.scrim` at the authored alpha (the shared transport bar's token, so both window edges agree), title ink = `theme.on_surface(scrim).ink`, Library = the shared Button's neutral face (its own raised fill carries `text_primary`, legible on the scrim in every theme). Two literal colours removed |
| V-r2-4 | transport time label at 2× | 48 px glyphs (4×) | 800×900 @2× | `ui/src/widget/video_player.rs` | `after/playing-2x.png` | open — F5-24 (shots host scaling theme + renderer); verified present |

Regressions: `capsules_take_their_measured_label_widths_once_painted` (1× and
2×: after `refresh_strip_labels` both capsules are `BUTTON_PAD_X + measured`,
on the strip's centre line with the overlay margins; a second measurement
changes nothing), `library_panel_geometry_stacks_measured_lines_at_2x` (1×:
28 / 38 / 66 / 30 / 108 / 58 / 44 / 48 / 50; 2× with measured labels: header
holds the title line, subtitle under the header with its wrapped height, tabs
under the subtitle, rows under the tabs, rows hold two measured lines,
controls hold their labels and fit the row).

### photos (`apps/photos/src/app.rs`)

| id | surface | defect | viewport/theme | file:line (after) | before → after | status |
|---|---|---|---|---|---|---|
| P-r2-1 | photo-details sheet at 2× | `panel_h = min(408 × scale, viewport − 24)` with a `60 × scale` field pitch and a bottom-anchored footer → on 800×900 the content (788 px) outgrows the capped panel (752 px) and Cancel / Save paint over "Captured" | 800×900 @2× (`metadata-edit-2x.png`) | app.rs:62 (`SHEET_*`), :627, :1592, :1608, :3756 | `photos/before/metadata-edit-2x.png` → `after/metadata-edit-2x.png` | fixed — one measured geometry (title, wrapped subtitle, four measured labels + 38·scale inputs, footer): the panel asks for that height (408 at 1×, unchanged), and when the viewport caps it the field list scrolls in a clipped body under the pinned footer. Wheel over the body scrolls (pixel deltas divided by `scale_factor`), Tab/focus reveals the focused field, presses outside the body never reach a scrolled-away input |
| P-r2-2 | library card "Details" control at 2× | label top-anchored at +14 in a 44 px face → 27 px line runs into the face's bottom edge while the pencil sits 8 px higher | 800×900 @2× (`library-2x.png`) | app.rs:3155 | `before/library-2x.png` → `after/library-2x.png` | fixed — label and pencil centred on the face's centre line (1×: label top 15.1 vs 14) |
| P-r2-3 | sheet footer labels | `rect.y0 + 14 × scale` | 2× | `draw_metadata_editor` | same | fixed — centred on the measured line |
| P-r2-4 | library grid gap | 8 px at ≤ 2 columns, 12 px otherwise | 480×900, 2× | `draw_library` | — | noted — authored density rule, consistent across the two narrow layouts |
| P-r2-5 | metadata label/value alignment | labels, inputs, Save all on the 16 px sheet inset | 1200×800, 480×900 | — | — | no defect |

Regression: `metadata_sheet_footer_never_covers_a_focused_field` (1200×800 1×
and 800×900 2×, three real frames of the `metadata-edit` scenario with focus
moved to "Captured": the field's rect is inside the visible body, Cancel and
Save start below the body, 1× needs no scroll and 2× on 800×900 does).

### music (`apps/music/src/app/render.rs`)

| id | surface | defect | viewport/theme | file:line (after) | before → after | status |
|---|---|---|---|---|---|---|
| M-r2-1 | large-text (≥ 135 %) rows | `row_h = 88`, title at +15, artist at +49 — literals that fit 2× exactly | 800×900 @2× | render.rs:124 (`LARGE_*`), :145 (`LargeTextBands`), :2345 | `music/before/large-text.png` → `after/large-text.png` (identical at 2× by design) | fixed — bands measured from their lines with the authored sizes as floors (header 96, rows 88, player 116); title + artist block centred on the row |
| M-r2-2 | large-text now-playing bar | art (18..86), text block (28..85) and play button (28..88) on three centre lines — 52 / 56.5 / 58 px from the bar's top | 800×900 @2× (`nowplaying-2x.png`) | same | `before/nowplaying-2x.png` → `after/nowplaying-2x.png` | fixed — art (a square as tall as the text block, ≥ 68), text block and the 60 px play circle all centred on the bar's midline; "Nothing playing" centred on its measured line; text budget derived from the play button's edge instead of a literal 310 |
| M-r2-3 | large-text header | 96 px fixed | — | same | fixed — max(96, measured title + 24) |
| M-r2-4 | large-text route parity (navigation and search content) | corrected-review-2 P1, plan task 4 | 2× | render.rs `render_large_text_frame` | — | open — not an alignment defect; app-owned |
| M-r2-5 | 1× track table | durations end 8 px inside the header search field / album grid edge | 1200×800 (`nowplaying.png`), 480×900 | — | — | noted — rows keep an 8 px inner inset inside their hover surface (the row highlight spans the full content width); consistent across every list |

Regression: `large_text_bands_keep_their_authored_size_at_2x_and_grow_beyond`
(2×: 96 / 88 / 116 with the blocks and padding inside; 4×: every band grows
and contains its measured block).

### tube (`apps/tube/src/screens.rs`, tests)

| id | surface | defect | viewport/theme | file:line (after) | before → after | status |
|---|---|---|---|---|---|---|
| T-r2-1 | app-local `Flex` gap / pad values | literals | — | screens.rs:26–46 | 1× PNGs pixel-identical before/after | fixed — values on the 4 px scale read `theme.tokens.spacing` (`s1` nav gaps, `s2` list/chip gaps, `s4` pill and bar insets, `s6` page padding); control sizes on a HIG metric read `theme.tokens.metrics` (`bar_height` 40 px icon/menu buttons, `icon_md`, `icon_label_gap`). Authored off-scale values are named constants (`PILL_PAD_Y` 9, `CARD_META_GAP` 3, `CARD_META_LINE_GAP` 5, `CARD_BODY_GAP` 9, `CARD_AVATAR_GAP` 10, `GRID_GAP` 18, `HEADING_GAP` 10, chip 14/7, status pill 10/6, top bar 66/20, search field 42/10). No pixel changed (`tube/after/*` vs `before/*`); 163 tests green |
| T-r2-2 | everything under `NavSplit` at 2× | 1× measured heights, 2× glyphs | 800×900 @2× | `ui/src/widget/nav_split.rs` | `before/home-2x.png` = `after/home-2x.png` | open — F1; `large_text_layout` bisection (5 tests) stays green: `Text`, `Flex`, grid and `Scrollable` measure at renderer scale |
| T-r2-3 | History route trailing label | "Stored locally — never uploaded" ends at x=876 while Home/Trending end at 1156 | 1200×800 (`history.png`) | `history()` `.max_w(820)` | — | noted — the history list is a reading-width column (820 px max); heading and rows share that column's right edge |
| T-r2-4 | card grid, channel rows, search chips | gaps even (18), heading on the card content edges (round one T1), chips 8 apart | 800×700, 480×900 | — | — | no defect |

## Fresh-eyes pass (800×700, 480×900, 200 %)

- music: track-list columns and tabular durations hold at all three
  (`nowplaying-compact.png`, `nowplaying-narrow.png`, `nowplaying-2x.png`).
- photos: grid gaps 12 (≥ 3 columns) / 8 (≤ 2); metadata labels and values on
  the sheet inset at 1200 and 480; the 2× sheet was the one defect (P-r2-1).
- camera: viewfinder clusters hold at all three; the gallery and scanner 2×
  routes were where the defects were (C-r2-2 … C-r2-12), plus the 480 px
  gallery filter and scrollbar (C-r2-7, C-r2-8).
- tube: card grid, channel rows and chips hold at 800×700 and 480×900; 2× is
  F1's.

## AFTER verification (PNGs opened)

- `camera/after/large-text.png`: switcher 268..532, "Photo | Video | Scan"
  with air between them. `after/scanner-2x.png`: header 16..108 holds both
  lines; tag holds "QR Code detected"; result card 664..824 stacks title,
  summary, three contained action pills. `after/scanner-wide-2x.png`: the
  empty state's two lines stacked. `after/gallery-2x.png`: "Library" 24..62
  and "9 items" 74..96; filter 290..776 with all four labels; captions two
  lines inside their strips; "0:07" tag contains its digits.
  `after/gallery-detail-2x.png`: title / "2m ago" / four key–value rows on a
  measured pitch; Favorite / Share / Delete hold their labels.
  `after/gallery-narrow.png`: filter row 64..96 under "9 items", grid from
  112, cards end at 446 clear of the thumb at 450..456. `after/video-mode.png`,
  `after/video-mode-2x.png`: red core, "Video" selected. `after/scanner.png`,
  `after/gallery.png`, `after/settings.png`, the eight matrices: 1× unchanged
  except the centred scanner header block.
- `video-player/after/library-2x.png`: 720 px panel; Library / subtitle /
  Queue · Recent / three two-line rows with Remove and Play sized to their
  labels. `after/library.png`: 1× identical. `after/playing.png`: Library
  14..84 and Open 1122..1188 (fit widths), title at 96, one centre line.
  `after/light/dev.qos.video-player-laptop.png`: dark scrim with light title
  and a filled neutral Library capsule — legible; the bottom bar and the top
  strip now share the scrim token. `after/playing-2x.png`: capsules hold 2×
  labels; the 4× time label remains (F5).
- `photos/after/metadata-edit-2x.png`: fields clipped at the body's edge,
  footer below it (no overlap); `after/library-2x.png`: "Details" and the
  pencil on one centre line; every 1× shot identical to BEFORE.
- `music/after/nowplaying-2x.png`: art 808..876, "Glasslight" block and the
  pause circle all centred on y=842; `after/large-text.png`: rows unchanged at
  2×; every 1× shot identical.
- `tube/after/*`: pixel-identical to BEFORE at 1×; 2× unchanged (F1).

## Code review (`/code-review medium`)

Confirmed and fixed: photos wheel `PixelDelta` used raw physical pixels while
every pointer position in the file divides by `scale_factor` (now divided);
video-player's strip hand-rolled `surfaces.base` + `text_primary` while `ui`
provides `surfaces.scrim` + `on_surface(scrim)` for over-media chrome and the
shared transport bar already uses them (now the same token and ink, Library as
the neutral face). Plausible and fixed: `CAMERA_MODE` parsed any unknown
string as Photo (now ignored, like `CAMERA_SCREEN`). Simplifications applied:
`refresh_panel_labels` no longer re-derives geometry the placer already
produces; `segmented_min_w` takes a slice. Refuted by the reviewer (not
changed): row hit rects diverging from paint (pushed during paint, as before);
tube tokens changing values; stale sheet body eating wheel events after close;
the scan-library grid sharing the gallery gutter. The reviewer could not run
`cargo check` on its host (`ring`); every finding was re-verified here by
running the suites and recapturing.

## Conventions observed

- One geometry description per surface, derived from measured lines with the
  authored 1× value as a floor: camera `GalleryChrome`, `segmented_min_w`;
  video-player `TopStrip` + `StripLabels` / `LibraryPanelGeometry` +
  `PanelLabels`; photos `SHEET_*`; music `LargeTextBands`.
- Where a layout pass has no renderer (`apply_layout`), paint measures with
  the frame's renderer, caches per text scale, and re-places through the same
  single writer — hit rects and faces always agree within a frame.
- Over-media chrome reads `surfaces.scrim` and `on_surface(scrim)`; shared
  `Button` faces over it use a style with its own fill.
- Tokens: spacing steps for on-scale gaps, `metrics` for HIG control sizes;
  authored off-scale values are named and documented.
- No app in this unit passes `tokens.typography.scale.*` to the legacy
  renderer APIs (all sizes are renderer-relative literals), so there was no
  `renderer_input_style` migration to do.

## Tests

Environment as round one (macOS aarch64; every cargo command prefixed with
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."
RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig` and
`--locked --no-default-features`; no cargo config edited).

New tests (all pass):

| package | test |
|---|---|
| camera | `app::tests::mode_switcher_segments_hold_their_measured_labels_at_2x`, `app::tests::gallery_chrome_keeps_title_count_filter_and_grid_apart` |
| video-player | `app::tests::capsules_take_their_measured_label_widths_once_painted`, `app::tests::library_panel_geometry_stacks_measured_lines_at_2x` |
| photos | `app::tests::metadata_sheet_footer_never_covers_a_focused_field` |
| music | `app::render::large_text_tests::large_text_bands_keep_their_authored_size_at_2x_and_grow_beyond` |

Full per-package suites (`cargo test -p <pkg>`), FINISH run, all green:

| package | passed (round one) |
|---|---|
| music | lib 212 (211) |
| photos | lib 93 + `end_to_end` 20 + `one_photograph_one_address` 5 = 118 (115) |
| video-player | lib 53 (51) |
| camera | lib 92 (90) |
| tube | lib 134 + main 1 + `interaction` 23 + `large_text_layout` 5 = 163 (163) |

Gate: `cargo check -p music -p photos -p video-player -p camera -p tube
--tests` clean after `rustfmt`.

## Limits

- macOS, `--no-default-features`: no GStreamer; video-player's chrome
  geometry is what the synthesized engine renders.
- The photos sheet scrolls the field list; the 2× screenshot shows the
  "Captured" field clipped at the body edge with Folder focused — the honest
  state for content taller than the viewport. There is no scroll affordance
  (thumb) yet; keyboard focus and the wheel reveal the rest.
- Camera recording state (`Video` while recording) has no shot pose; the
  square stop glyph was reviewed in code only.
- Video-player, light theme, letterboxed media: the strip's `surfaces.scrim`
  (dark in every theme, like the shared transport bar's) reads mid-grey at
  35 % over the light page, with light `on_surface(scrim)` ink — legible, the
  same trade F5 made for the bottom bar (`after/playing-light.png`).
- Tube 2× remains F1's `NavSplit` item; this unit changed no tube pixel.
