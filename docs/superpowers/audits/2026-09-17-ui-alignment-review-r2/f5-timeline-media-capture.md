# F5 — Timeline, history, media and capture widgets: round two

Branch `ui-review/r2-f5-timeline-media-capture`, cut from the merged round-one tip
`381eff6a8` ("Merge ui-review/integration"). Worktree
`/Users/xaver/dev/qos/.claude/worktrees/agent-aea1647a026a69bde`; paths below are relative to
it unless absolute. Round one's report: `../2026-09-16-ui-alignment-review/f5-timeline-media-capture.md`.

## Scope

Same files as round one: `ui/src/widget/{activity_timeline,history,history_group,
history_scrubber,channel_browser,video_player,capture_studio,screenshot_markup,document_view,
document_viewport,image_viewer}.rs`, `ui/src/presence/`, plus the unit's own integration file
`ui/tests/f5_immersive_geometry.rs`. Unit X1 owns every text-size double-scaling and
text-centring expression this round; none of those expressions was edited here (one was moved
verbatim in `video_player.rs`; see F5R-04).

This round's brief: close round one's open items that sit on the widget side (F5-15, F5-19,
F5-20, F5-21, F5-23, A8's V3 hand-off, the presence-width question), re-verify that round one's
fixes survived the merge, and look again at 2× and narrow widths for centre lines, insets, hit
rect == painted rect, and hidden overflow of chips and badges.

## Re-verification of round one after the merge

BEFORE captures were retaken at `381eff6a8` before any edit (paths under
`target/ui-review/<app>/before/` and `target/ui-review/ui/<scenario>/before/`) and compared with
round one's AFTER descriptions:

| round-one fix | evidence at the merged tip | status |
|---|---|---|
| F5-05/06 timeline band header time column and popover clamp | `ui/activity_timeline/before/frame.png`: header "just now" and row "just now" both right-aligned at x≈816; popover right edge at the track end (820) | survived |
| F5-06/07 scrubber popover gutter, type floor | `ui/history_cases/before/16-strip-versions.png`: two-line popover inside its box, captions at `xs` | survived |
| F5-08/09/10 video player 32 px targets, light scrim ink, measured time label | `video-player/before/playing-{dark,light}.png`, `playing-large.png`: light ink over the scrim; label on the bar's centre line at 2× | survived; the 2× label paints at 4× (F5-24, X1) |
| F5-11 presence pill/chip heights | `notes/before/presence-large.png`: "Alice"/"Bob" pills enclose their labels top and bottom | survived |
| F5-13/14 markup header and toolbar | `f5_immersive_geometry` (`screenshot_markup_renders_wide_and_narrow`) | survived — 3/3 f5 tests green at the base before edits |
| module tests | `cargo test -p ui --lib -- capture_studio:: activity_timeline:: history channel_browser:: video_player:: screenshot_markup:: document_view image_viewer:: presence::` | **567 passed, 0 failed** at the base |

## Findings

| id | surface | defect | viewport / theme | file:line (pre-fix) | evidence | status |
|---|---|---|---|---|---|---|
| F5R-01 | `VideoPlayer` in `widget_snapshot` (round one F5-15) | The leaf was `Dimension::auto()` with `flex_grow`; as the root of a tree (a gallery cell, a harness) that resolves to 0×0, so the placeholder glyph painted on the cell's origin over the caption. `ImageViewer` beside it uses `percent(1.0)` and fills its cell. The widget side, not the example's. | `ui/widget_snapshot/before/gallery.png` (row 7, glyph over "VideoPlayer (no frame)") | `video_player.rs:2043–2051` | after: `ui/widget_snapshot/after/gallery.png` | fixed — `percent(1.0)` leaf like `ImageViewer`; the video-player app already wraps the player in a 100 %×100 % column, chat and tube build their own leaf, so no consumer layout changes |
| F5R-02 | `VideoPlayer` time label width budget (A8 V3 hand-off) | The label's left edge was clamped to the play button (`max(play_btn.x1 + pad)`) but its right end was never checked: on a narrow bar, a long duration or large type the label painted under the volume control and starved the scrub track below `MIN_SCRUB_WIDTH`. The bar's one non-control was the one thing allowed to overlap controls. | 300–420 px bars at 1×; 960 px at 2× | `video_player.rs:1307–1321` | tests `time_label_yields_to_the_scrubber_rather_than_overlapping_controls`, `a_wide_bar_paints_the_full_time_label`; `ui/f5/after/video-player-controls-dark-2x{,-narrow}.png` | fixed — the label yields: full "m:ss / m:ss" → position only → nothing, keeping the track at its floor; `time_label_rect` recorded for the no-overlap contract; the centring expression `mid_y - height * 0.5` moved unchanged (X1's) |
| F5R-03 | `VideoPlayer` at 2× (A8 V3 / round-one F5-24) | The label sits on the centre line but paints at 4× in the app's `--shots` path (theme and renderer both scaled). | `video-player/before/playing-large.png` | `ui/src/app.rs` `Host` branch | — | open — X1 (double text scaling); the a11y contract now runs at 2× (`dark-2x`, `dark-2x-narrow`) so the bar's targets are checked under scale regardless |
| F5R-04 | presence pill / caret chip / name chip widths at 2× (round-one question) | Widths already follow the measured label (`layout_peer_pills` also clamps pills to the viewport); what did not scale were the horizontal insets (7 px pill, 5 px chip) and the agent glyph's 14 px room, so a 2× label sat in 1× pads. | `notes/before/presence-large.png` | `presence/render.rs:396–430, 700–760, 841–890` | test `pill_and_chip_side_insets_scale_with_the_label`; `ui/f5/after/presence-pills-{1,2}x.png` | fixed — `label_magnification` (the theme's `sm` step over the default `sm`, × the renderer's text scale; exactly 1 at 1×, so `with_text_scale`, `with_ui_scale` and a larger base all count) scales `PILL_PAD_X`, `CHIP_PAD_X` and the agent glyph; the 1× pill is byte-identical (width and height tests). Visible in `notes/after/presence-large.png` |
| F5R-05 | presence edge-mist name tag | The tag was as wide as the name and anchored 4 px inside the band, so a long name (or any name at 2×) hung out over content the band's hit rect (`mist_band_rect`, 44 px deep on a side edge) knows nothing about — a painted rect the pointer cannot reach. | any; worse at 2× | `presence/render.rs:1016–1040` | test `edge_tag_stays_inside_its_band_at_every_text_scale`; `ui/f5/after/presence-pills-{1,2}x.png` (right edge "Charlotte Okonkwo", top edge "Bob") | fixed — `mist_tag` elides the label to the band's inner width and keeps the tag inside the band both ways; a short 1× name is untouched; a tag that would keep no letter, or a line box deeper than the band, is withheld — the mist's colour still points |
| F5R-06 | presence `draw_peer_outline_labeled_within` | Only the vertical was clamped to `bounds`; a chip on an object near the surface's right edge ran past it and was clipped to its first letters — at 2× twice as often. | clipped surfaces (timeline lanes, grids) | `presence/render.rs:305–340` | test `a_name_chip_never_leaves_the_clipped_surface_sideways` (pixel) | fixed — `chip_fitting` elides to the surface width and the chip slides left to stay inside |
| F5R-07 | capture studio top bar (round-one F5-23) | Mode group `x0..x0+180` held three 56 px buttons with an 8 px leading and **no** trailing inset; the source group `x0+188..x1` (300 px) held four 58 px buttons on a 64 px stride with **no** leading inset and ~50 px of dead pill after the last button; gaps 2 px vs 6 px. | 1400×900 dark | `capture_studio.rs:827–843, 883–901` | tests `top_bar_groups_hug_their_buttons_with_one_inset`, `top_bar_buttons_are_one_bar_height_tall_inside_token_insets`; `ui/f5/after/capture-studio-selector.png` | fixed — one geometry derived from the tokens (the bar has no theme when it lays out): group inset and gap `Spacing::BASE.s2`, bar `Metrics::BASE.bar_height + 2·s2` = 56 so the buttons are exactly one `bar_height` (40) tall, a symbol button as wide as the bar is tall, neighbours on an `s1/2` hairline. Both groups hug their buttons; the bar is 442 px instead of 488, still centred; every hover/hit fixture reads `mode_rects()`/`source_rects()` so none moved by hand |
| F5R-08 | capture studio 8 px shortcut hint (round-one F5-19) | `symbol_button` painted a `shortcut` at 8 px — but every one of its seven callers passes `""`: dead code below the type floor. | — | `capture_studio.rs:2266–2320` | — | fixed — parameter and branch removed (with the renderer it alone used) |
| F5R-09 | capture studio 7 px delay count (round-one F5-19) | The armed-delay count was a 7 px Semibold glyph at two hand-picked offsets (`x1 − 12` / `x1 − 9.5`). | — | `capture_studio.rs:2680–2695` | test `delay_count_is_painted_at_the_type_floor_or_above` (glyph-run font size ≥ `scale.xs`) | fixed — `scale.xs`, right-aligned by measurement in the 36 px slot's corner |
| F5R-10 | capture studio docked recording pill at 2× (round-one F5-20, S2 hand-off — widget side) | Pill 252 / 360 px and Stop 64 px fixed; at 2× the timer label was elided to a few characters and "Stop" overran its button. | 2× | `capture_studio.rs:1838–1890, 3137–3300` | test `docked_recording_pill_grows_with_its_content_above_one_x`; `ui/f5/after/capture-studio-recording-pill-{1,2}x.png` | fixed on the widget side — the settled labels are built and measured before the geometry; above 1× (by the shared `label_magnification`) the pill grows to `43 + label + 8 + 34 + stop + 6` and Stop to `22 + caption + 8` (`remember_docked_content_widths`), as monotone floors for the life of the recording so digits and Paused/Resumed swaps never slide the buttons; at 1× both caches are zero so the designed widths — and the compact pill's match with the shell's 360 px clock capsule — are untouched. The shell's capsule (`shell/src/shell/topbar_chrome.rs:459`, `(width − 580).clamp(min, 360)`) does not scale with type, so at 2× on a compact bar the two no longer match: **S2** |
| F5R-11 | history scrubber popover at 2× | `POPOVER_H` is a fixed 36 px with the band at y0+50 and the handle's reach at y0+47 — there is no room to grow (35–38 px). The two-line stack (head `sm−1`, detail `xs`) is 28.75 px at 1× but 53.75 px at 2×, so the detail line ran down over the band. | 2× | `history_scrubber.rs:1067–1125` | test `popover_text_stays_inside_its_box_at_every_text_scale` (glyph runs + pixel rows) | fixed — one fitted, measured line stack; when it cannot keep `POPOVER_PAD_Y` inside the box the change line yields and the version name stays (the screen reader still names both); 1× unchanged |
| F5R-12 | `DocumentViewport` gutter (round-one F5-21) | `DEFAULT_PADDING = 24` was a constant because the viewport is theme-less at construction. | — | `document_viewport.rs:40, 102, 179, 952` | tests `default_gutter_and_scrollbar_thickness_equal_the_spacing_tokens`, `gutter_follows_the_theme_unless_a_host_pinned_it` | fixed — `build_layout` (the first place a theme reaches the widget) binds the gutter to `spacing.s6` unless a host pinned it with `with_padding`; the theme-less default stays 24. `SCROLLBAR_THICKNESS` 8 = `s2` under a parity test; `SCROLLBAR_INSET` 3 is off-grid and documented |
| F5R-13 | reader HUD insets | The brief's "HUD insets" are the shared `ui::SelectionHud` in `ui/src/reader_hud.rs`, which the pdf viewer composes (`apps/pdf_viewer/src/app_draw.rs:1416+`). Not an F5 file. | `pdf_viewer/before/page-bubble-large.png` (page bubble clipped at the bottom is the app's, F5-16) | `ui/src/reader_hud.rs` | — | open — hand-off (reader HUD owner / A9) |
| F5R-14 | activity timeline chips | Filter-strip chips are painted 28 px tall and hit-tested on the same rect; the author pill likewise; a band header's Name / Undo / Restore chips are 20 px. All under the 32 px minimum (HIG §9.6) while their containers (36 px strip, 52 px row) have room. | any | `activity_timeline.rs:2462, 2505, 1147, 1165` | test `chip_targets_meet_the_minimum_pointer_target_and_never_overlap` | fixed — `hit_rect_within` grows the *target* to `metrics.min_pointer_target` (cached from the theme each `draw`), sliding rather than shrinking at a container edge; paint unchanged; hit-testing and the public accessors read the target, painting reads the original |
| F5R-15 | channel browser role chip | `chip_w = min(label + 12, ROLE_CHIP_W)` capped the chip at 64 px but painted the label uncapped — a role name at 2× ran past the chip. | 2× (no consumer; harness only) | `channel_browser.rs:577–606` | tests `role_label_budget_is_the_chip_less_its_pads`, `role_chip_label_stays_inside_the_capped_chip_at_two_x` (pixel) | fixed — label elided to `ROLE_LABEL_BUDGET` (= chip − pads) before measuring |
| F5R-16 | `image_viewer`, `document_view`, `history`, `history_group`, `screenshot_markup` | Fresh pass at 2× and narrow: contain-fit is edge to edge (`photos/before/opened-large.png`), the data modules have no geometry, and the markup toolbar's round-one layout (32 px swatches, collapse behind `colors`) holds at 440 px and 1200 px. | — | — | `ui/f5/after/screenshot-markup-{wide,narrow}.png` | no defect |
| F5R-17 | video-player app top bar, pdf page title/bubble, chat sidebar and huddle card at 2× | Round one's F5-16/17/18: app-owned. Re-seen in `video-player/before/playing-dark.png`, `pdf_viewer/before/page-bubble-large.png`, `chat/before/collaboration-large.png`. | 2× | app files | — | open — A8 / A9 / A4 |

## Tests

Every cargo command carried `RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."
RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig` inline and `--locked --no-default-features`.

| command | result |
|---|---|
| `cargo test -p ui --lib -- capture_studio:: activity_timeline:: history channel_browser:: video_player:: screenshot_markup:: document_view image_viewer:: presence::` | **582 passed, 0 failed, 0 ignored** (2931 filtered out). Base at `381eff6a8`: 567 passed; the 15 new tests are listed below |
| `cargo test -p ui --test it -- f5_` | **4 passed, 0 failed** (340 filtered out); base: 3 passed. Writes `target/ui-review/ui/f5/after/*.png` (11 files) |
| `cargo check -p ui --tests` | clean — no errors, no warnings |
| `cargo test -p ui --test it -- f1_spacing` (F1's spacing-literal ratchet, run because this diff adds module constants) | **4 passed, 1 failed — pre-existing at the merged tip, not this diff.** `spacing_literals_do_not_grow` reports two *stale* entries whose baseline is higher than the tree: `apps/word-processor/src/zoom_control.rs [pixel_const]` (now 1, baseline 2) and `shell/src/shell/editor_app.rs [pixel_const]` (now 3, baseline 4). Neither file is touched here; the test also checks every file for counts *above* baseline and names none of this unit's — the token-derived constants (F5R-07/R1) are not literals to it. The two entries should be lowered on the integration branch (a tightening); not done here because both files belong to other units |
| `rustfmt --edition 2024 --check` on changed files | clean for `capture_studio.rs`, `history_scrubber.rs`, `channel_browser.rs`, `presence/render.rs`, `video_player.rs`, `document_viewport.rs`, `f5_immersive_geometry.rs`; `activity_timeline.rs` has five pre-existing unformatted spots at HEAD (378, 796, 2063, 2315, 2580) that were left alone so the diff stays this unit's |

New tests (all on the production paint/hit path; none `#[ignore]`d, none weakened):

- `video_player`: `time_label_yields_to_the_scrubber_rather_than_overlapping_controls`
  (300/420 px at 1×, 300/960 px at 2×), `a_wide_bar_paints_the_full_time_label`.
- `presence::render`: `pill_and_chip_side_insets_scale_with_the_label`,
  `pill_insets_follow_ui_scaled_type_too`, `edge_tag_stays_inside_its_band_at_every_text_scale`
  (1×/2×/3×), `a_name_chip_never_leaves_the_clipped_surface_sideways` (pixel).
- `capture_studio`: `top_bar_groups_hug_their_buttons_with_one_inset`,
  `top_bar_buttons_are_one_bar_height_tall_inside_token_insets`,
  `docked_recording_pill_grows_with_its_content_above_one_x`,
  `delay_count_is_painted_at_the_type_floor_or_above`.
- `history_scrubber`: `popover_text_stays_inside_its_box_at_every_text_scale` (glyph runs + pixel).
- `document_viewport`: `default_gutter_and_scrollbar_thickness_equal_the_spacing_tokens`,
  `gutter_follows_the_theme_unless_a_host_pinned_it`.
- `activity_timeline`: `chip_targets_meet_the_minimum_pointer_target_and_never_overlap`.
- `channel_browser`: `role_chip_label_stays_inside_the_capped_chip_at_two_x` (pixel).
- `ui/tests/f5_immersive_geometry.rs`: the video-player a11y contract now runs a matrix (dark,
  light, dark 2×, dark 2× at 320 px) with a per-case control floor; new
  `presence_pills_and_edge_tags_render_at_one_and_two_x` evidence render.

## Evidence index

BEFORE (dark unless noted), all at `381eff6a8`: `target/ui-review/video-player/before/{playing,paused,fullscreen}-{dark,light,large}.png`;
`target/ui-review/pdf_viewer/before/page-bubble-{dark,large}.png`; `target/ui-review/photos/before/opened-{dark,large}.png`;
`target/ui-review/notes/before/{presence,status}-{dark,large}.png`; `target/ui-review/chat/before/collaboration-{dark,large}.png`;
`target/ui-review/ui/{history_cases,gallery_shots,widget_snapshot,widget_showcase_snapshot,activity_timeline}/before/`.

AFTER: the same set under `…/after/`, plus `target/ui-review/ui/f5/after/{video-player-controls-{dark,light,dark-2x,dark-2x-narrow},presence-pills-{1,2}x,capture-studio-selector,capture-studio-recording-pill-{1,2}x,screenshot-markup-{wide,narrow}}.png`.

Compared pairs (every PNG opened):

- `ui/widget_snapshot/{before,after}/gallery.png` — row 7 "VideoPlayer (no frame)": the play
  glyph moves from the caption's origin to the centre of its 320×160 cell (F5R-01).
- `notes/{before,after}/presence-large.png` — the "Alice" pointer and caret pills gain side pads
  in proportion to their (4×, X1) labels instead of 7 px; heights unchanged (F5R-04). The
  off-screen "Bob" tag at the right edge no longer paints a name wider than its 44 px band; at
  this 4× magnification no letter fits, so the band carries the colour alone (F5R-05/R5).
- `ui/f5/after/capture-studio-selector.png` — both top-bar groups end one inset after their last
  button; no dead pill after the source group (F5R-07).
- `ui/f5/after/capture-studio-recording-pill-2x.png` — "0:00 · Selection" fully shown beside a
  Stop button that holds its caption; `…-1x.png` unchanged at 252 px (F5R-10).
- `ui/f5/after/video-player-controls-dark-2x-narrow.png` — 320 px bar at 2× type: play, scrub
  (≥ 48 px), mute, volume, gear, fullscreen on one centre line, no label (F5R-02); the a11y
  matrix asserts the same for `dark`, `light`, `dark-2x`.
- `ui/f5/after/presence-pills-{1,2}x.png` — "Alice", the agent "Ada" (glyph scaled with the
  label), "Bob" tag inside the top band, "Charlotte Okonkwo" elided inside the right band.
- `video-player/{before,after}/playing-large.png`, `…-dark.png`, `…-light.png` — the controls bar
  is unchanged at 1200 px: the full label fits, so the degrade never engages (F5R-02's "no
  change with room"). Measured with `target/ui-review/pngdiff.py` (stdlib PNG decoder, no
  ImageMagick on this host): `playing-dark` 48 of 960 000 pixels differ (max channel delta 21,
  anti-aliasing); `playing-light` differs across the *video frame* only — the synthesized demo
  clip's diagonal pattern advances with wall-clock time (its white seam sits at x≈80 before and
  x≈79 after) — with the bar pixel-identical.
- `notes/{before,after}/presence-dark.png` — 2 360 pixels differ at a maximum channel delta of 8,
  all inside the "Write the migration" typing highlight, whose fade alpha is wall-clock based
  (`draw_peer_typing`); the pills, caret chip and edge tag are pixel-identical at 1× (notes
  scales its theme with `with_text_scale(1.0)`, so the magnification is exactly 1).
- `ui/activity_timeline/{before,after}/frame.png`, `ui/history_cases/{before,after}/*.png`,
  `photos/…/opened-large.png`, `pdf_viewer/…/page-bubble-large.png` — byte-identical (`cmp`): the
  timeline change is hit-rect only (F5R-14), the popover keeps two lines at 1× (F5R-11), and the
  viewport gutter is the same 24 px under the default theme (F5R-12).
- `pdf_viewer`, `photos`, `chat` after-sets — unchanged; their remaining 2× defects are the
  apps' (F5R-13, F5R-17).

## Code-review round (`/code-review high`)

Seven findings; six fixed, one recorded as a hand-off. The reviewer could not compile in its
sandbox (the parent `.cargo/config.toml` `ring` const-assert), so every finding was re-verified
here by running the tests it named.

| # | finding | resolution |
|---|---|---|
| R1 | The new module-level whole-number constants (`TOP_*` ×4, `ROLE_LABEL_BUDGET`, `POPOVER_PAD_Y`) trip F1's spacing-literal ratchet (`ui/tests/f1_spacing_token_audit.rs`, `pixel_const`; counts may only fall), and a parity test restates a token instead of reading it. | fixed — the constants are the tokens' arithmetic: `TOP_GROUP_PAD`/`TOP_GROUP_GAP = Spacing::BASE.s2`, `TOP_BAR_H = Metrics::BASE.bar_height + 2·s2` (56 — the buttons come out exactly one `bar_height` tall), `TOP_BUTTON_W = TOP_BAR_H` (a symbol button is as wide as the bar is tall), `TOP_BUTTON_GAP = s1 / 2`, `ROLE_LABEL_BUDGET = ROLE_CHIP_W − 12`, `POPOVER_PAD_Y = s1 / 2`. The parity tests became `top_bar_buttons_are_one_bar_height_tall_inside_token_insets`; `f1_spacing` is green with no baseline change |
| R2 | `label_magnification` read `Theme::text_scale × renderer scale`, missing `with_ui_scale`, which doubles the type but leaves `text_scale` at 1 — the pads would stay 1× around a 2× label. | fixed — the magnification is the theme's `sm` step over the default `sm`, times the renderer scale; `pill_insets_follow_ui_scaled_type_too` |
| R3 | The capture studio's pill-growth gate restated the same formula, so under UI scale the pill never grew. | fixed — one `pub(crate) presence::label_magnification` shared by both |
| R4 | Above 1× the docked pill re-fitted itself to the live label every frame — "9:59"→"10:00", Paused/Resumed swaps — sliding Pause and Stop under a resting pointer. | fixed — both floors are monotone for the life of a recording and cleared by `reset_docked_content_widths` on the idle/receipt paths |
| R5 | `mist_tag` clamped the tag to the band horizontally but not vertically; a 3× line box (or the double-scaled shots path) still painted past a top/bottom band's 44 px depth. | fixed — a line box deeper than the band withholds the tag (the mist alone still points); the test now runs 1×/2×/3× and only a >2× scale may withhold. Seen in the notes 4× capture: a *scaled* tag pad left room for a lone ellipsis, so the tag's pad is the band's fixed inset and an ellipsis-only label withholds the tag too |
| R6 | `paint_settings_cluster` still reserved the full "m:ss / m:ss" width when deciding how many setting icons fit, folding reachable controls into the gear to protect a label that then yields. | fixed — the reserve is the position-only width (the label's shortest useful form); controls outrank the full label |
| R7 | The armed-delay count at `scale.xs` sits in a fixed 36 px slot beside a 16 px icon; at 2× the two overlap. | recorded — the bottom bar has no scale (every slot is 36 px, F5-20's sibling); at 1× badge and icon are disjoint. Needs the S2 bottom-bar scaling decision; noted in code |

## Limits

- macOS: the shell `shots` binary, `qos-control` and the headless compositor are Linux-only, so
  capture-studio and presence evidence is rendered through `FrameHarness`/`OffscreenRenderer`
  (the same widget code the shell composes). `channel_browser`'s lab scenario opens a window and
  has no `--shots`; its fix is covered by its pixel test.
- `ui-shots` scenarios have no text-scale switch; 2× evidence for the framework widgets comes
  from `f5_immersive_geometry` (theme-scaled and renderer-scaled cases) and the unit pixel tests.
- The app `--shots --text-scale 2` captures still show 4× type (F5R-03, X1); they were retaken
  so the alignment changes are visible, but their glyph size is not this unit's to judge.
