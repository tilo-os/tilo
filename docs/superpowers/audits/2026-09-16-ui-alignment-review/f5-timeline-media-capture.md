# F5 — Timeline, history, media and capture widgets: alignment and spacing review

Branch `ui-review/f5-timeline-media-capture`, cut from `codex/macos-shell` @ `2d7e819b6`
(plus the documented one-line `Cargo.lock` fix `f9a579c9c`). All paths below are relative
to the worktree root unless absolute.

## Scope

Files owned and reviewed: `ui/src/widget/{activity_timeline,history,history_group,
history_scrubber,channel_browser,video_player,capture_studio,screenshot_markup,
document_view,document_viewport,image_viewer}.rs` and `ui/src/presence/{geom,interaction,
key,render,mod}.rs`. These are single-leaf widgets that hand-compute geometry in `draw`;
the goal was not to move them onto taffy but to give each one geometry description that
paint, hit-testing and accessibility read alike, with text centred on measured line height
and every pointer target at the HIG §9.6 minimum.

Consumers checked: `apps/video-player` (VideoPlayer), `apps/notes` presence shots
(`presence::render`), `apps/word-processor --history` (HistoryScrubber), `apps/chat`
collaboration shot, `apps/photos` (ImageViewer), `apps/pdf_viewer`, `apps/video_editor`
(DocumentViewport). `ui-lab activity_timeline --shots`, `ui-shots history_cases`,
`widget_snapshot`, `widget_showcase_snapshot`, `gallery_shots`. Nothing consumes
`ChannelBrowser`; `CaptureStudio`/`ScreenshotMarkup` are shell overlays (`shell/src/shell/
overlay_host.rs`), rendered here through `FrameHarness`/`OffscreenRenderer`.

## Method

1. Source pass over every file for whole-pixel literals, font-size-derived offsets,
   hard-coded colours and hit rects that differ from painted rects (counts in the table).
2. BEFORE captures (dark, light, 2× text) for every consumer and fixture above, opened and
   inspected; defects recorded with viewport, theme and coordinates.
3. Fixes expressed through theme tokens and *measured* text metrics, one geometry
   description per widget for paint + hit-test + a11y; no shared primitive's default
   metric changed (the presence pill is still 18 px at 1×, the controls bar still 44).
4. Regression tests per fix (unit tests in each widget module; one integration file
   `ui/tests/f5_immersive_geometry.rs`, registered in `ui/tests/it.rs` because the crate
   aggregates integration tests into one binary with `autotests = false`).
5. AFTER captures, compared with BEFORE.

Capture roots: `target/ui-review/<app>/{before,after}/`, `target/ui-review/ui/<scenario>/
{before,after}/`, `target/ui-review/ui/f5/{before,after}/` (harness renders).

## Findings

| id | surface | defect | viewport / theme | file:line (pre-fix) | before / after evidence | status |
|---|---|---|---|---|---|---|
| F5-01 | capture_studio: docked recording pill | Label centred with a hand-tuned fraction of the font size (`center.y − scale.sm × 0.52`, plus `+0.5`); every other pill/receipt label used a per-size literal offset (`−6.0` xs, `−7.0` sm, `−10.5` lg) instead of the measured line box (1.25 × size). At 2× type the 25 px line box sat 6.5 px low and the label ran out of the pill. | any / any; visible at 2× | `capture_studio.rs:3252, 1705, 2389, 2802, 3021, 3413, 3524, 3724–3740, 3921, 3996, 4291, 4314, 4537, 4572, 4756` | after: `target/ui-review/ui/f5/after/capture-studio-recording-pill-{1,2}x.png` | fixed — `line_height`/`centered_text_top` helpers; 15 sites |
| F5-02 | capture_studio: pill/chip labels | Labels painted with the legacy `max_width` argument, which *wraps*; a label that did not fit folded onto a second line below the pill (the unit test at 2× caught rows 17..=56 against a pill of 7..41). | wide bar ≥1000 / any; any scale where the label exceeds its budget | `capture_studio.rs:1731, 2421, 3065, 3323, 3358, 3378, 3593, 3805, 3815, 4007, 4647` | test `docked_recording_label_stays_inside_its_pill_at_two_x` | fixed — measured, weight-aware `fit_label` (ellipsis) then painted unwrapped; 11 sites |
| F5-03 | capture_studio: bottom-bar action buttons | Icon rect `y0+13..y0+29` in a 44 px button (centre 21 vs 22), label at `y0+14` — icon and label on different lines from the button centre. | any | `capture_studio.rs:4740–4756` | covered by F5-01 evidence | fixed — icon and label centred on `r.center().y` |
| F5-04 | activity_timeline: rows | Author, hash, op glyph, summary, time and the selection-bar label centred by `cy − size/2`; the painted line box is 1.25 × size, so text sat 1.5 px below the avatar's centre line at 1× and 3 px at 2×. | 520×640 dark: `history_cases/before/01-bands-default.png`; 860×660 `activity_timeline/before/frame.png` | `activity_timeline.rs:1811, 1823, 1866, 1896, 1974, 2571` | after: `target/ui-review/ui/history_cases/after/01-bands-default.png`, `target/ui-review/ui/activity_timeline/after/frame.png` | fixed — `text_top` measured helper |
| F5-05 | activity_timeline: band header vs rows | Header time right-aligned at `x1 − 8`, rows at `x1 − 12`: the time column was 4 px ragged (816 vs 820 in the lab frame). "Still open" dot and author dots placed at `y + size/2` rather than the line box centre. | 860×660 dark: `activity_timeline/before/frame.png` (x=816 vs 820) | `activity_timeline.rs:2270, 2280–2285, 2210` | after: `target/ui-review/ui/activity_timeline/after/frame.png` | fixed — shared `TRAILING_INSET` (= `spacing.s3`, token-parity test) |
| F5-06 | history_scrubber: popover | Popover clamped to an 8 px gutter while track and buttons use `TRACK_INSET` (16): pushed to either end it overhung the content edge (x1 = 992 vs track 984 in word-processor; 828 vs 820 in the lab frame). `clamp` could also panic when the strip was narrower than the popover floor. | 1000×700 dark: `word-processor/before/history-dark.png`; `ui/activity_timeline/before/frame.png` | `history_scrubber.rs:457–465` | after: `target/ui-review/word-processor/after/history-dark.png`, `…/ui/activity_timeline/after/frame.png` | fixed — clamped to `TRACK_INSET`, guarded; 2 tests |
| F5-07 | history_scrubber: type below the scale floor | Band captions, popover sub-line and the naming hint were `sm − 3 = 9 px` / `(size − 2).max(9)` — below `scale.xs` (10), the type scale's floor (HIG §9.6). Two-line popover used fixed `+5`/`+21` offsets. | any | `history_scrubber.rs:768, 1062–1063, 1140` | before/after `history_cases/{before,after}/16-strip-versions.png` | fixed — `scale.xs`; two-line stack centred on measured heights |
| F5-08 | video_player: hit rects | Hit rect == painted rect: scrub and volume tracks were 5 px tall targets, mute 24×24, play/fullscreen/gear/setting icons 30×30 — all under the 32 px minimum (HIG §9.6). Accessibility bounds and tooltips read the same rects. | 1200×800 dark: `video-player/before/playing-dark.png` | `video_player.rs:1005–1013, 1084–1093, 1112, 1189, 1274–1284, 1354–1362` | after: `target/ui-review/video-player/after/playing-dark.png`; a11y bounds asserted in `f5_immersive_geometry` | fixed — `ControlsLayout::{hit_rect,track_hit_rect}`; glyph rects unchanged; 3 unit tests + 1 integration test |
| F5-09 | video_player: light theme legibility | Controls scrim was a fixed `rgba(0,0,0,170)` while glyphs and time used `text_primary` — near-black on black in light themes: play, time, gear and fullscreen almost invisible. | 1200×800 light: `video-player/before/playing-light.png` | `video_player.rs:997, 1014, 1130, 1149, 1258, 1290` | after: `target/ui-review/video-player/after/playing-light.png` | fixed — `surfaces.scrim` at `CONTROLS_SCRIM_ALPHA`, ink from `theme.on_surface(scrim)`; pixel test |
| F5-10 | video_player: time label and menu type | `time_size = 12.0` literal centred by `size × 0.5`; at 2× the label dropped ~9 px below the bar's centre line. Settings menu used `12.0`/`13.0` literals with `−7`/`−8` offsets. | 800×900 2×: `video-player/before/playing-large.png` | `video_player.rs:1042–1043, 1258, 1507–1512, 1601–1604, 1622–1631, 1699–1719` | after: `target/ui-review/video-player/after/playing-large.png` | fixed — `scale.sm` + measured height |
| F5-11 | presence: name pills, caret chips, edge-mist tags | Pill height = `size + 2·pad` and chip height = `size + 2·pad` with the measured height discarded (`_th`); the label box was pinned to the top pad. At 1× the 15 px box sat flush to the 18 px pill's bottom and the 12.5 px box overran the 13 px chip by 1 px; at 2× the labels spilled far below their chips ("Alice", "Bob"). | 800×900 2×: `notes/before/presence-large.png` (chip 537–567, ink to 595) | `presence/render.rs:418–426, 740–747, 835–842, 858–867, 995–1000` | after: `target/ui-review/notes/after/presence-large.png`, `…/presence-dark.png` | fixed — sizes from the measured line box (`PILL_PAD_Y` 3 → 1.5 keeps the 18 px pill at 1×); 3 tests |
| F5-12 | channel_browser: rows, header, time | Same `cy − size/2` centring as F5-04; footer band a fixed `rgba(0,0,0,51)`. | no consumer; harness only | `channel_browser.rs:514, 522, 636, 691, 726` | — | fixed — measured `text_top`; `colors.overlay_subtle` |
| F5-13 | screenshot_markup: header | Caption placed at a fixed `x0 + 92` after the title and at a `+3` offset; a wider title (or 2× type) runs the two together. Zoom hint at `x1 − 210` with a 190 px cap rather than the canvas gutter. | any | `screenshot_markup.rs:589–617` | after: `target/ui-review/ui/f5/after/screenshot-markup-wide.png` | fixed — measured title width + `HEADER_GAP`, one measured centre line, `CANVAS_INSET` both ends |
| F5-14 | screenshot_markup: toolbar | Swatches were 30×36 targets on a 30 px stride (under 32); on a toolbar at its 420 px floor the swatch run (ending x≈358) slid under the action cluster (starting x≈258) — overlapping hit rects. Badge and picker positions re-derived from literal sums (`8 + 204 + 48`, `8 + 3·40 + 20`). Tooltip width estimated from `chars × 7`. | ≤452 px wide surface | `screenshot_markup.rs:294–386, 881–884` | after: `target/ui-review/ui/f5/after/screenshot-markup-{wide,narrow}.png` | fixed — one `toolbar_layout()`; 32 px swatches inline when they fit, otherwise collapsed behind a `colors` disclosure whose palette pill keeps every `color-*` target reachable (review R4); redact badge clamped to the room left (R3); anchors derived; measured tooltip; 6 tests |
| F5-15 | video_player / VideoPlayer in `widget_snapshot` | Play glyph painted at the cell origin over the label. | `widget_snapshot/before/gallery.png` | `ui/examples/widget_snapshot.rs:449–470` | — | open — example harness places a flex-grow leaf with no available size; needs F3 (examples) |
| F5-16 | pdf_viewer page title wraps into its meta line; highlight band offset from text | `pdf_viewer/before/page-bubble-dark.png` (301–310 vs 160) | app renderer | — | — | open — needs A-unit (pdf_viewer); already on the 2026-09-15 register (P2) |
| F5-17 | video-player app top bar | "Library" button, "Demo clip" title and "Open" sit on three different centre lines (y≈30/24/30). | 1200×800 dark: `video-player/before/playing-dark.png` | `apps/video-player/src/app.rs:1645–1660` | — | open — needs A-unit (video-player) |
| F5-18 | chat sidebar | Server title "Acme Design channe" clipped under the "3 people online" pill; at 2× the sidebar and huddle card collide throughout. | `chat/before/collaboration-{dark,large}.png` | app-owned sidebar (not `ChannelBrowser`) | — | open — needs A-unit (chat) |
| F5-19 | capture_studio: 8 px / 7 px glyph text | Shortcut hint drawn at `8.0` px and the delay digit at `7.0` px — below the type scale floor; kept as-is because they are glyph decorations inside 40 px buttons, and enlarging them changes the button composition. | any | `capture_studio.rs:2263, 2621` | — | open — design decision (F2 controls / S2 shell overlay) |
| F5-20 | capture_studio: fixed pill widths at 2× | Pill widths (252/360/304) and bar heights do not scale with type; labels now truncate rather than wrap, but a full 2× layout needs pill growth. | 2× | `capture_studio.rs:1790–1835` | `target/ui-review/ui/f5/after/capture-studio-recording-pill-2x.png` | open — needs S2 shell overlay layout decision |
| F5-21 | document_viewport | `DEFAULT_PADDING = 24` (= `spacing.s6`) and the 8/3 px scrollbar metrics are constants because the viewport is theme-less at construction; the pdf and video_editor shots show the 24 px gutter correct. | — | `document_viewport.rs:40, 47, 49` | `pdf_viewer/before/page-bubble-dark.png`, `video_editor/before/default-dark.png` | no defect; documented |
| F5-22 | image_viewer, document_view, history, history_group | No geometry literals on the draw path (contain-fit is edge to edge; the others are data/taffy modules). The 9 px letterbox in Photos is the app's layout. | — | — | `photos/before/opened-dark.png` | no defect |
| F5-23 | capture_studio: top bar source group | The source group pill spans `b.x0 + 188 .. b.x1` (300 px) but holds four 56 px buttons on a 64 px stride (ends at +250): ~50 px of dead space at its right end (x≈893→944 in the 1400 px render); the mode group has 2 px gaps, the source group 8 px. | 1400×900 dark: `ui/f5/after/capture-studio-selector.png` | `capture_studio.rs:top_bar/source_group_rect/source_rects` | — | open — top-bar composition; re-sizing it moves every hover/hit fixture (32 tests) — needs an S2 decision |
| F5-24 | video-player app at 2× | The time label now holds the bar's centre line, but it paints at 48 px: this app's shot path scales the theme *and* the renderer (`ui/src/app.rs:1240` `Host` branch sets both), so legacy `draw_text` callers get 2× twice. Same root cause as the 4× chip text in `notes --text-scale 2`. | 800×900 2×: `video-player/after/playing-large.png` | `ui/src/app.rs:1211–1256` | before/after `video-player/{before,after}/playing-large.png` | open — needs F1/F3 (App text-scale plumbing; not in this unit's files) |

Literal counts (whole-pixel floats outside tests, `awk`/`grep`, before → after): capture_studio
681 → see diff (fudge factors removed, 15 sites now measured); activity_timeline 125;
history_scrubber 63; channel_browser 55; video_player 104; screenshot_markup 126;
presence/render 55. The remaining literals are the bars' internal insets (4/8/12/16 — the
spacing scale — kept as named constants where hit-testing has no theme, with token-parity
tests where they were introduced).

## Tests

All cargo invocations used `RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1`
inline, `--locked --no-default-features`, and `RUSTFLAGS="-C force-frame-pointers=yes
--remap-path-prefix=/build=."` (the repo's macOS flag set) because cargo merges the parent
checkout's `.cargo/config.toml` under `/Users/xaver/dev/qos/` and its stale aarch64
`target-cpu=generic` breaks `ring`'s const-assert — not this diff.

| command | result |
|---|---|
| `cargo test -p ui --lib -- capture_studio:: activity_timeline:: history_scrubber:: channel_browser:: video_player:: screenshot_markup:: presence:: document_view image_viewer::` | **329 passed, 0 failed, 0 ignored** (2904 filtered out). Base had 286 in the seven edited modules; 18 tests are new (video_player 4, presence 3, capture_studio 2, history_scrubber 2, screenshot_markup 5, activity_timeline 2) and the remaining modules are unchanged. |
| `cargo test -p ui --test it -- f5_immersive_geometry` | **3 passed, 0 failed** (305 filtered out); writes `target/ui-review/ui/f5/after/*.png` (7 files). After the review round the video-player case observes seven controls (transport, seek, mute, volume, promoted setting, gear, fullscreen) through a synthesized `media::MediaEngine`. |
| `cargo test -p ui --test it -- design_token_audit` | **2 passed, 2 failed — pre-existing at the base commit, not this diff.** `theme_escape_hatches_do_not_grow`: `src/text_render/renderer_regressions.rs [white_or_black]: 5 occurrence(s), baseline 0`. `app_and_shell_theme_escape_hatches_do_not_grow`: `apps/browser/src/browser_tab_strip.rs [literal_color]: 1 occurrence(s), baseline 0`, `shell/src/shell/frame.rs [literal_color]: 1 occurrence(s), baseline 0`, `shell/src/shell/frame.rs [white_or_black]: 2 occurrence(s), baseline 0`. None of those files is touched here. This diff's own effect on the ratchet is a tightening: the `channel_browser.rs` and `video_player.rs` `literal_color 1` entries dropped to 0 and were removed from `ui/tests/design_token_baseline.txt`. |
| `cargo check -p ui --tests` | clean. |
| `rustfmt --check` on every changed file | clean; all eight source files were rustfmt-clean at HEAD, so formatting touched only these hunks. |

New tests (all assert on the production paint/hit path, none `#[ignore]`d):

- `video_player`: `every_bar_control_meets_the_minimum_pointer_target`,
  `bar_controls_share_one_centre_line_and_never_overlap`,
  `a_widened_scrub_target_still_seeks_where_the_track_is_painted`,
  `controls_ink_reads_over_the_scrim_in_the_light_theme` (pixel-sampled).
- `presence::render`: `pill_and_chip_heights_follow_the_measured_label_at_every_text_scale`,
  `the_default_pill_height_is_unchanged_at_one_x`, `pill_label_stays_inside_its_pill_at_two_x`
  (pixel-sampled).
- `capture_studio`: `label_centring_tracks_the_measured_line_box_at_every_text_scale`,
  `docked_recording_label_stays_inside_its_pill_at_two_x` (pixel-sampled; this is the test
  that exposed F5-02 — it failed against wrapping labels and passes after `fit_label`).
- `history_scrubber`: `popover_never_overhangs_the_track_gutter`,
  `popover_survives_a_strip_narrower_than_its_floor_width`.
- `screenshot_markup`: `toolbar_controls_meet_the_minimum_target_and_never_overlap`,
  `narrow_toolbar_drops_swatches_rather_than_overlapping_actions`,
  `badge_and_picker_follow_the_placed_tool_group`.
- `activity_timeline`: `gutter_constants_equal_the_spacing_tokens`,
  `row_text_centring_tracks_the_measured_line_box_at_every_text_scale`.
- `ui/tests/f5_immersive_geometry.rs`: `video_player_controls_are_reachable_targets_on_one_centre_line`
  (a11y bounds ≥ 32 px, one centre line, dark + light), `screenshot_markup_renders_wide_and_narrow`,
  `capture_studio_renders_selector_and_docked_recording_pill`.

Pre-existing failures: none observed in the scoped runs.

## Evidence index

BEFORE (dark unless noted): `target/ui-review/video-player/before/{playing,paused,fullscreen}-{dark,light}.png`,
`…/playing-large.png`; `target/ui-review/notes/before/{presence,status}-{dark,light}.png`,
`…/presence-large.png`; `target/ui-review/chat/before/collaboration-{dark,light,large}.png`;
`target/ui-review/pdf_viewer/before/page-bubble-{dark,light,large}.png`;
`target/ui-review/photos/before/opened-{dark,light,large}.png`;
`target/ui-review/word-processor/before/history-dark.png`;
`target/ui-review/video_editor/before/{default,filters}-dark.png`;
`target/ui-review/ui/history_cases/before/01..25*.png`; `target/ui-review/ui/activity_timeline/before/frame.png`;
`target/ui-review/ui/widget_snapshot/before/gallery.png`; `target/ui-review/ui/widget_showcase_snapshot/before/showcase.png`;
`target/ui-review/ui/gallery_shots/before/gallery-{dark,light}.png`.

AFTER: `target/ui-review/video-player/after/{playing,paused,fullscreen}-dark.png`, `playing-light.png`,
`playing-large.png`; `target/ui-review/notes/after/presence-{dark,light,large}.png`;
`target/ui-review/chat/after/collaboration-dark.png`; `target/ui-review/word-processor/after/history-dark.png`;
`target/ui-review/ui/history_cases/after/*.png`; `target/ui-review/ui/activity_timeline/after/frame.png`;
`target/ui-review/ui/widget_snapshot/after/gallery.png`; `target/ui-review/ui/gallery_shots/after/gallery-{dark,light}.png`;
`target/ui-review/ui/f5/after/{video-player-controls-{dark,light},screenshot-markup-{wide,narrow},capture-studio-selector,capture-studio-recording-pill-{1,2}x}.png`.

Compared pairs called out: video-player light (F5-09: ink now light over the scrim), video-player
large (F5-10: time label on the centre line), notes presence-large (F5-11: chips enclose labels),
activity_timeline frame (F5-05/F5-06: time column and popover edges aligned at x = 816 / 820),
history 16-strip (F5-06/F5-07), capture-studio pill 2× (F5-01/F5-02: centred, ellipsised, inside
the pill), screenshot-markup narrow (F5-14: one swatch survives, no overlap).

## Code-review round (`/code-review high`)

Ten findings; six real defects fixed, two in-scope cleanups done, two logged as open because
their right home is outside this unit's files.

| # | finding | resolution |
|---|---|---|
| R1 | Markup header caption still painted with `Some(max_width)` (wraps) and a `max(0.0)` budget, so at 2× / narrow it folded into a column over the canvas gap and the zoom hint. | fixed — `TextRenderer::elide_to_width_weighted` then painted unwrapped. |
| R2 | Removing the last literal colours from `channel_browser.rs` and `video_player.rs` left their `literal_color 1` entries in `ui/tests/design_token_baseline.txt`, which the ratchet reports as stale. | fixed — both entries removed (a missing key is 0). The audit test is red at the base commit for unrelated files (see Tests). |
| R3 | With the redact tool active on a ≤ 452 px surface the 96 px "Opaque" badge ran under the Open/Copy buttons; the new no-overlap tests only enumerated `controls`. | fixed — badge width clamps to the room before the actions, hidden under 44 px; label elided; `narrow_redact_badge_stays_clear_of_the_actions`. |
| R4 | Dropping swatches that did not fit removed four colours from `controls()` — the only pointer, tooltip and `markup.color-*` control-socket route — with no keyboard or overflow alternative. Reachability regression. | fixed — when five 32 px targets do not fit, a single `colors` button (showing the current colour) opens a palette pill above the toolbar, the way the tool button does; every `color-*` target is present while it is open, closes on choice / Escape / click-outside. `narrow_toolbar_collapses_swatches_behind_a_colours_button`. |
| R5 | The a11y integration test saw only "Enter fullscreen": no media (transport/seek/mute/volume withheld), no settings (no gear), and the role filter excluded `ToggleButton`. It could not fail if the scrub hit rect regressed. | fixed — synthesized `media::MediaEngine` with an open source, a promoted toggle and a gear choice; roles `Button | Slider | ToggleButton | ComboBox`; asserts ≥ 7 controls, each ≥ 32 px on one centre line. |
| R6 | `fit_label` re-implemented `TextRenderer::elide_to_width_weighted` with a different edge contract. | fixed — thin wrapper over the renderer's helper (one ellipsis policy crate-wide). |
| R7 | Light-theme test used a hand-rolled, non-gamma luma and a `> 128` threshold; ink is chosen against the opaque scrim rather than the 0.67-α composite. | fixed (test) — `a11y::contrast_ratio(glyph, painted scrim) ≥ 4.5`. Ink choice against the opaque scrim is deliberate and documented in code: the composite depends on the frame under it and the opaque scrim is the conservative surface. |
| R8 | `TRAILING_INSET` comment claimed "hit-test path has no theme" though all uses are in `draw_*`; markup's four `Equals spacing.sN` constants had no parity test. | fixed — comment corrected; `layout_constants_equal_the_spacing_tokens` added to screenshot_markup. |
| R9 | The "measure `Ag`, halve it" centring exists as five private copies (activity_timeline, channel_browser, capture_studio, inline in presence/history_scrubber/video_player); one `TextRenderer::line_height`/`centered_top` would give one definition. | open — needs F1/F2 (`ui/src/text_render.rs` is not this unit's file). Each copy is a two-line wrapper over the renderer's own measurement, so behaviour is already single-sourced. |
| R10 | HIG §9.6's 32 px minimum is a private constant in `screenshot_markup.rs`, `video_player.rs` and the integration test (plus pre-existing `CLOSE_HIT_PX`/`RIBBON_HIT_W`/`RAIL_HIT_H`); a `tokens` home would let one audit cover every node. | open — needs F1 (`ui/src/tokens`). The integration test's `32.0` is the HIG number it checks against, not a copy of production. |

## Limits

- The shell `shots` binary (which has the canonical `capture_studio` / `capture_*` /
  `ScreenshotMarkupTransition` scenarios) was not built here (memory-constrained host,
  `CARGO_BUILD_JOBS=1`); capture-studio and markup evidence is from `FrameHarness` /
  `OffscreenRenderer` renders in `ui/tests/f5_immersive_geometry.rs`, which paint the same
  widget code the shell composes.
- BEFORE renders for capture_studio/screenshot_markup exist only as source line references
  (the widgets are not reachable from any app `--shots`); AFTER PNGs are listed above.
- `ChannelBrowser` has no consumer; its fixes are covered by its unit tests only.
- Text-scale behaviour: the renderer and the theme can both carry the text scale (see
  `ui/src/app.rs:1240` vs `Owned`); every fix here measures with the renderer it draws with,
  so it is correct under either convention, but the double-scaling seen in `notes
  --text-scale 2` (chip text at 4× the label size) is an `App`/`ui/src/app.rs` matter —
  needs F1/F3 (not in this unit's files).
- Two files outside the listed set were touched, both test plumbing: `ui/tests/it.rs`
  (two-line module registration — the only way a new `ui/tests/f5_*.rs` file is compiled
  at all, since the crate aggregates its integration tests with `autotests = false`) and
  `ui/tests/design_token_baseline.txt` (two ratchet entries lowered to their new count of
  zero, which the audit test requires once a file's last literal colour is gone).
