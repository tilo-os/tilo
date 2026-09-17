# A10 — Large editors & browser chrome: alignment and spacing review

Unit A10 of the 2026-09-16 all-app UI alignment review: `video_editor`
(`apps/video_editor`, one full-bleed custom widget) and `browser`
(`apps/browser`, a Tier A composer of shared widgets around a Chromium
content area). Branch `ui-review/a10-large-editors-browser`, cut from
`codex/macos-shell` at `ad222b830` (see "Environment" for the base
correction), plus the `ui-review/lockfix` cherry-pick.

Capture root (not committed): `target/ui-review/<app>/{before,after}/` in the
worktree. `before` was rendered from binaries built at the base commit and
preserved in `target/ui-review/bin-before/`; `after` from the reviewed tree.
Every PNG named below was opened and inspected.

## video_editor

### Scope

`apps/video_editor/src/app.rs` chrome regions: topbar, transport strip, media
bin heading and rows, inspector heading and control rows, timeline ruler and
track heads, dialogs (discard, export, help, go-to), export chip and project
notice. Timeline time-to-pixel mapping, clip geometry, the curve editor and the
waveform/filmstrip content were treated as content, not chrome.

### Method

1. Source pass over every `draw_*` region and its geometry helpers
   (`regions`, `transport_buttons`, `history_buttons`, `inspector_layout`,
   `add_title_rect`, `export_layout`, `readout_fit`), looking for per-size
   fudge offsets, sibling rows with different gaps or sizes, text centred on
   a font size rather than a measured line box, and hand-rolled control faces
   beside shared ones.
2. `--responsive-matrix` (desktop/laptop/tablet/phone) in dark and light,
   `--text-scale 2` at 800×900, 480×900, 800×700, and 30 `--scenario`
   captures at 1200×800 (`filters menu effects keyframes markers curves
   focusinspector focusbin focustransport shuttle rename discard format
   collaborators help goto title mix locked projectmenu clipmenu exporting
   exportfailed composed scrolled keymask range`, the export dialog through
   `QOS_VE_EXPORT=1 --scenario exportfailed` in dark/light/480×700, and
   `--pseudo-locale`).
3. Fixes kept small and local (token substitutions, one shared geometry
   description per region, shared faces); a child test module reads the
   production geometry back.

### Findings

| id | surface | defect | viewport / theme | file:line (after) | before → after | status |
|---|---|---|---|---|---|---|
| VE-1 | transport readout | playhead time, total, motion words and format meta drawn at four hand-tuned tops (+8/+13/+13/+14): the shuttle readout sat ~2px above the total's baseline, and the parts diverge from the buttons' centre line at any other text scale | 1200×800 dark, `shuttle` | `app.rs` `TRANSPORT_CONTROL_CY` (const block), `transport_buttons`, `history_buttons`, `draw_transport`, `readout_tops` | `video_editor/before/shuttle.png` (readout at x 557–790, y 449–470) → `video_editor/after/shuttle.png` | fixed |
| VE-2 | bin + inspector heading rows, export chip, project notice | "+ Title" chip (y0+6..26, centre +16) floated ~5px above "MEDIA BIN"/"7 clips" (top +14, centre ≈+20.6); inspector's "N clips selected" pill (+4..22) likewise above "INSPECTOR"; the export chip and project-notice plate (+6..+32, centre +19) in the same strip on a third line | 1920×1080 dark/light, `exporting`, `saved` | `app.rs` `PANE_HEADING_Y0/H`, `add_title_rect`, `regions` (`export_chip`), `draw_project_notice`, `draw_bin`, `draw_inspector_body` | `before/dark/dev.qos.video-editor-desktop.png` (bin heading x 15–255, y 54–74), `before/exporting.png` (chip y 54–80) → `after/...desktop.png`, `after/exporting.png` | fixed — one band for headings, chips and plates |
| VE-3 | inspector control rows | sibling rows mixed label sizes 12 / 11.5 / 11 ("Fade In/Out" and "Transition length" read as a smaller kind of row than "Opacity"; geometry rows and captions at 11.5) | 1920×1080, `title`, `keymask` | `app.rs` `draw_inspector_body` (`label_size`), `draw_project_section`, `draw_marker_rows`, `base_size` | `before/.../desktop.png` (y 352, 436) → `after/.../desktop.png` | fixed — one size, `typography.scale.sm`, applied once via `base_size` |
| VE-4 | inspector "Transition" caption | 2px tighter to the fade knobs than every other caption to the row above it (touching) | 1920×1080 | `app.rs` `inspector_layout` (`y += row + 4.0`) | `before/.../desktop.png` (y 379–392) → `after/.../desktop.png` | fixed |
| VE-5 | inspector "+ Add effect" | hand-rolled fill/stroke with label pinned at `center().y - 8.0`, left inset 12 while every other button centres its label through the shared face | 1200×800, `effects`, light `effects-light` | `app.rs` effects block in `draw_inspector_body` | `before/effects.png` (x 914–1046, y 314–338) → `after/effects.png` | fixed — `draw_button` (shared `Button::paint`) |
| VE-6 | bin "+ Title" | hand-rolled chip face | 1920×1080 | `app.rs` `draw_bin` | `before/.../desktop.png` → `after/.../desktop.png` | fixed — `ButtonFace` / `Button::paint` |
| VE-7 | export dialog option groups | hand-rolled filled pills with labels at `center().y - 7.0`; the same rows in the inspector already use the shared segmented face, so the dialog spoke a second control language | 1200×800 dark/light, 480×700 | `app.rs` `draw_export_modal` (`seg_group`) | `before/exportdialog.png` (rows y 314–466) → `after/exportdialog.png` | fixed — `draw_segmented` (`ToggleGroup::paint`) |
| VE-8 | topbar | leading inset 12 vs trailing 14; Import 112 wide beside Split/Export 110 | all | `app.rs` `TOPBAR_EDGE`, `TOPBAR_BTN_W`, `regions` | matrix before → after | fixed |
| VE-O1 | whole editor at 2× text | transport readout overlaps the scrubber, inspector rows and track-head lines collide, ruler labels overlap — every chrome height is a fixed pixel count | 800×900 `--text-scale 2` | `app.rs` `TIMELINE_H`, `RULER_H`, `TRANSPORT_H`, `VIDEO/AUDIO_LANE_H`, `inspector_layout` row metrics | `before/large-text.png` → `after/large-text.png` (unchanged failure mode; the readout now clamps at the strip top instead of drifting) | open — plan `2026-09-15-app-text-geometry.md` task 3 (measured heights), not an alignment-only change |
| VE-O2 | export dialog footer | `export_notice` (card.y1−82) shares its slot with the failure reason (y1−80) and touches the info line (y1−96, 12px); "Rendering…" (y1−62, 10.5px) ends 1px above the buttons (y1−50) | `exportdialog.png` | `app.rs` `draw_export_modal` footer | `before/exportdialog.png` (y 486–530) | open — needs measured footer stacking; `a_long_export_failure_wraps_above_the_footer_and_buttons` replicates the offsets rather than reading them and must move with it |
| VE-O3 | export dialog header | mix words (y0+60, 10.5px) to "RESOLUTION" caption (y0+76): ~3px | `exportdialog.png` | `app.rs` `EXPORT_HEADER_H` | same | open — cramped, not overlapping |
| VE-O4 | topbar / inspector insets | `TOPBAR_GAP` 10 and `PAD` 14 are off the spacing scale | — | `app.rs` consts | — | recorded — deliberately retained and documented (`TOPBAR_GAP` doc comment); `regions()` has no `Theme`, so token equivalence is by comment (`TOPBAR_EDGE` = `s3`) |
| VE-O5 | transport undo/redo | `--pseudo-locale` brackets the empty label of an icon-only `ButtonFace` into a visible "[]" beside the glyph | `pseudo.png` | `ui/src/widget/button.rs` `Button::paint` (`i18n!(face.label)` on "") | `before/pseudo.png` (x 485–545, y 458) | open — needs F2 |
| VE-O6 | track-head M/L/S pills, PiP corner buttons | hand-rolled toggle faces (semantic on-colours; custom corner glyphs) — centred by measurement, but not the shared face | — | `app.rs` `draw_timeline`, PiP block | — | recorded — needs F2 (`ButtonFace` has no semantic-tone toggle or custom-glyph slot) |
| VE-O7 | discard / export / help / go-to cards | hand-drawn cards (shared `Button` faces inside) not on `ModalFrame`/`ModalDialog` | — | `app.rs` `draw_discard_dialog`, `draw_export_modal`, `draw_help_sheet`, `draw_goto` | — | recorded — hand-rolled audit already lists the export modal as DEFER; adoption is composition work (F3/F4), not alignment |

### Conventions observed

- One geometry description per region is honoured: `regions()`,
  `transport_buttons`, `history_buttons`, `add_title_rect`,
  `inspector_layout`, `export_layout` feed draw, hit-test and the
  accessibility tree alike; every fix changed those functions, never a
  draw-side copy.
- Text is centred from the measured line box (`text_baseline`, the pattern
  `ui/src/widget/button.rs` uses); `readout_tops` derives every readout top
  from measured heights around the cluster centre line.
- Theme type tokens reach the legacy `text` helpers through `base_size`
  (divide by the renderer's scale once), mirroring
  `TextRenderer::renderer_input_style`.
- Shared faces: `Button::paint`, `Slider::paint`, `ToggleGroup::paint`,
  `EmptyState::paint`, `ContextMenu`, `Scrollbar` — no new local control.
- All actions, keyboard routes, selection and undo state untouched; no
  explicit manual size changed except the two documented consts.

## browser

### Scope

`apps/browser/src/{browser_widget,browser_tab_strip,address_overlay,
site_controls,daily_controls}.rs`: toolbar (omnibox row), tab strip
decorations, suggestions dropdown, site information/protection panels, find
bar. `apps/browser/chromium/` and `blocker.rs` untouched. Page content is the
headless engine's placeholder in every capture.

### Method

Same matrix as above plus the bespoke routes: `--shots-suggestions` (1×, 2×,
light), `--shots-overlay`, and `--shots-{pip-empty,tabs,settings,library,
loading,error,history-paused}` at 1200×800 and 480×900, `--shots-tabs` at 2×
and light, `--shots-library --library-section bookmarks|downloads`. Geometry
proved through `ui::headless::FrameHarness::targets()` in
`apps/browser/tests/chrome_alignment.rs`.

### Findings

| id | surface | defect | viewport / theme | file:line (after) | before → after | status |
|---|---|---|---|---|---|---|
| BR-1 | omnibox row | control heights mixed on one row: Site / protection launchers 32, DevTools 36, Find / zoom 32 beside 40px navigation buttons and the 40px address field (Row centres them, so tops and bottoms stepped) | 1920×1080 dark/light | `browser_widget.rs` `build_composition` (site launcher, DevTools), `site_controls.rs` `launcher`, `daily_controls.rs` `button` | `browser/before/dark/dev.qos.browser-desktop.png` (Site x 180–236 y 16–48 vs Back y 12–52) → `after/dark/dev.qos.browser-desktop.png` | fixed — every control 40 tall; test `every_toolbar_control_shares_the_address_fields_height_and_centre_line` |
| BR-2 | omnibox row, 600–999px | every optional command stayed until the 600px compact tier, leaving a 768px window an address field ~56px wide ("https:/") and an 800px one ~88px | tablet 768×1024, 800×700, 800×900 @2× | `browser_widget.rs` `ROOMY_CHROME_MIN_WIDTH`, `roomy_chrome`, `apply_layout`, `build_composition`, Library heading | `before/dark/dev.qos.browser-tablet.png`, `before/compact.png`, `before/large-text.png` → `after/...` (address ≈292px at 768) | fixed — site info, bookmark, history pause and PiP leave the row below 1000px; each keeps its action/keyboard route, history pause moves to the Library heading exactly as in compact; test `a_tablet_width_window_keeps_a_readable_address_field` |
| BR-3 | suggestions dropdown rows | two-line block pinned at row_top+8 with the second line placed from the font size: 8px above / 4.5px below at 1×, 8px / 23px at 2×; theme-resolved sizes drawn through the legacy API (scaled a second time on the live desktop) | `suggestions.png`, `suggestions-x2.png` | `address_overlay.rs` `render` | `before/suggestions-x2.png` → `after/suggestions-x2.png` | fixed — `renderer_input_style` + `measure_style`/`draw_style`, block centred in the row, line gap `spacing.s1`; hit rows unchanged (`row_height`) |
| BR-4 | tab strip mute slash | `Color::from_rgba8(220, 65, 75, 255)` | `tabs-1200x800.png` | `browser_tab_strip.rs` `draw` | before → after (theme `semantic.danger`) | fixed |
| BR-5 | site information / protection panels | column gap `10.0` off the spacing scale; panel insets 12 / 64 / 24 / 112 as literals | 1000×700, 390×700 | `site_controls.rs` `panel`, `browser_widget.rs` `build_layout` | after only (see limits) | fixed — theme gap, `spacing.{s3,s16,s12}` |
| BR-O1 | site information rows | permissions and certificate facts are single "name: value" text lines — no label/value column alignment | 390×700 | `site_controls.rs` `panel` | — | open — composition (Field/label rows); left alone to avoid colliding with the in-flight site-panel work (`3af55beab`) |
| BR-O2 | chrome notice bands, panel lines | `Text::with_size(13.0)` throughout (private indicator, history paused, notices, site panel) — off the type scale | — | `browser_widget.rs`, `site_controls.rs` | — | recorded — a size change beyond alignment; candidates for `TextRole::Body` |
| BR-O3 | tab pills | `TabBar` reserves a 14px icon slot at x0+10 while the strip paints 16px favicons at x0+9 (centres 19 vs 17, 2px); at 2× the pill stays 44 tall with 14px glyphs beside 24px titles | `tabs-x2.png` | `ui/src/widget/tabs.rs` | — | open — needs F6 |
| BR-O4 | toolbar | four identical Shield glyphs (Site, protection, history pause, Private) | desktop | `browser_widget.rs` | — | recorded — icon identity, not alignment |

### Conventions observed

- Shared widgets only: `Button`, `TextInput`, `Row`, `Column`, `Container`,
  `Scrollable`, `ContextMenu`, `ToastCard`, `EmptyState`, `TabBar`; the
  browser's own drawing is limited to tab-strip decorations and the
  suggestions overlay, both now theme-toned and measured.
- Responsive tiers are stored flags updated in `apply_layout` with
  `structure_dirty`, seeded by `debug_set_viewport_width` for one-frame
  captures — the same mechanism `compact_chrome` already used.
- Private-window semantics, tab identity controls and site-panel behaviour
  are untouched; the `Private` button stays on every tier.

## Tests

All runs with `--locked --no-default-features`, `CARGO_BUILD_JOBS=1`,
`RUSTFLAGS="-C target-feature=-sha3"` (see Environment).

| command | result |
|---|---|
| `cargo check --tests -p video_editor -p browser` | clean (no warnings) |
| `cargo test -p video_editor --lib -- chrome_geometry_tests transport topbar inspector export bin readout shuttle walk section marker title mix` | 147 passed, 0 failed, 0 ignored (521 filtered out); includes the 5 new `app::chrome_geometry_tests::*` |
| `cargo test -p browser --test chrome_alignment --lib -- site_panel narrow toolbar pip library history private address tab suggestions find compact chrome` | lib: 112 passed, 0 failed, 2 ignored (pre-existing `#[ignore]`, listed below), 71 filtered out; bin unit tests: 1 passed; `tests/chrome_alignment.rs`: 3 passed, 0 failed |
| `QOS_SITE_RENDER_EVIDENCE=… cargo test -p browser --lib populated_site_panels_render_at_narrow_width` (in the run above) | passed; wrote `target/ui-review/browser/after/site-panel/{narrow-protection,narrow-site-info}.png` |

New regression tests:

- `apps/video_editor/src/app/chrome_geometry_tests.rs` (child module of `app`,
  reads private geometry): `the_transport_cluster_shares_one_centre_line`,
  `a_tall_readout_is_held_inside_the_strip_rather_than_pushed_above_it`,
  `the_bin_heading_chip_sits_on_the_heading_band_and_clear_of_the_list`,
  `the_transition_caption_keeps_the_clearance_every_other_row_has`,
  `the_topbar_is_inset_equally_at_both_edges_and_its_commands_share_one_width`.
- `apps/browser/tests/chrome_alignment.rs` (public API through
  `FrameHarness::targets()`):
  `every_toolbar_control_shares_the_address_fields_height_and_centre_line`,
  `a_tablet_width_window_keeps_a_readable_address_field`,
  `the_find_bar_controls_share_the_find_fields_height`.

Pre-existing ignored tests in the browser lib run (not touched; both are
`#[ignore]`d child-process entry points spawned by their parent tests, not
failing tests):
`browser_widget::tests::private_discovery_failure_filter_worker_does_not_recreate_root_child`,
`native_blocker::tests::private_refresh_does_not_recreate_root_after_helper_exit_child`.

## Limits

- **WIP constraint.** `app.rs`, `shot.rs`, `browser_widget.rs` and
  `blocker.rs` carry uncommitted changes in the user's checkout; edits here
  were kept to token substitutions, corrected centring expressions, fixed
  gaps and face swaps inside existing functions. New tests live in new files
  (`apps/video_editor/src/app/chrome_geometry_tests.rs`, declared with one
  `mod` line; `apps/browser/tests/chrome_alignment.rs`).
- **Chromium placeholder.** Every browser capture shows the headless engine's
  placeholder page; page-content alignment is out of scope.
- **Site panels.** No `--shots` route reaches the site panels; their evidence
  is the existing `populated_site_panels_render_at_narrow_width` test with
  `QOS_SITE_RENDER_EVIDENCE` (after only — the before tree is gone once the
  source changed). The panel change is a gap/inset token substitution and a
  launcher height.
- **Private window.** No offscreen route composes the private indicator
  (`--private` is a live launch flag); the indicator band was reviewed from
  source only.
- **2× text scale** is a pre-existing whole-editor failure (VE-O1); this
  review did not attempt the measured-height rework.

## Environment

- The worktree was cut at `24ee08c66`, a lineage that lacked the browser
  commits this unit must not regress (`5a30514e3`, `35f4cf2f2`, `09c998a2d`,
  `3af55beab`, `478634a8e`); the branch was reset to `codex/macos-shell`'s tip
  `ad222b830` before any work, then `ui-review/lockfix` (`f9a579c9c`) was
  cherry-picked for the browser `libc` lock entry.
- On this macOS host `ring 0.17.14` fails its `CAPS_STATIC ==
  MIN_STATIC_FEATURES` const assertion because rustc 1.97.1 statically enables
  `sha3` for `aarch64-apple-darwin`; every cargo command was run with
  `RUSTFLAGS="-C target-feature=-sha3"` (no Darwin rustflags exist in
  `.cargo/config.toml`, so nothing is dropped). `PKG_CONFIG_PATH` pointed at
  `ffmpeg@8`; `--no-default-features` for both crates.
