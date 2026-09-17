# A10 — Large editors & browser chrome: round two

Unit A10 of the 2026-09-17 UI alignment review, round two: `video_editor`
(`apps/video_editor`, one full-bleed custom widget) and the browser chrome
(`apps/browser`). Branch `ui-review/r2-a10-large-editors-browser`, cut from the
round-one integration merge `381eff6a8`; two local commits, nothing pushed.
Round one's report is `../2026-09-16-ui-alignment-review/a10-large-editors-browser.md`;
its ids (`VE-*`, `BR-*`) are referenced below.

Capture root (not committed): `target/ui-review/<app>/{before,after}/` in the
worktree. `before` was rendered from binaries built at `381eff6a8` and kept in
`target/ui-review/bin-before/`; `after` from the final tree. Every PNG named
below was opened and inspected; the full matrix (dark/light responsive matrix,
800×900 at 2×, 480×900, 800×700, and every scenario at 1200×800) was captured
for both apps before and after.

## 1. Browser merge verification

The coordinator resolved `apps/browser/src/browser_widget.rs` between round
one's `roomy_chrome` tier and the other session's download-status toolbar item
(`4a23e862c`) and fullscreen flow (`7ead72f25`). Checked by hand at
`381eff6a8`:

| check | result |
|---|---|
| `cargo build --bins -p video_editor -p browser` | clean |
| the resolution's shape | the download-status `push` sits between Library and PiP; bookmark, history pause and PiP are gated on `if self.roomy_chrome {`; `build_composition` starts with the fullscreen early return; nothing from either side was dropped |
| browser lib suite | 124 passed, 0 failed, 2 ignored (round one: 112; the twelve new tests are the other session's download / fullscreen / exit-flow tests, e.g. `fullscreen_events_and_renderer_loss_drive_tab_state`, `exiting_content_fullscreen_is_idempotent_and_routes_native_command_once`) |
| `tests/chrome_alignment.rs` | 3 passed at the base; 6 passed after this round's additions |
| omnibox row keeps one control height with the download status present | no `--shots` route can show a download, so this is proved by a new harness test that feeds `WebViewEvent::DownloadRequested` through a `WebViewEngine` wrapper: `browser.download-status` is 40 tall on the address field's centre line (`the_download_status_shares_the_toolbar_rows_height_and_centre_line`) |
| the roomy tier still drops the right items at 1000 px | `shots-999x700.png` / `shots-1000x700.png` (bookmark, history pause and PiP absent at 999, present at 1000) and `the_roomy_tier_starts_at_a_thousand_pixels` |
| the find bar does not wrap | `the_find_bar_controls_share_the_find_fields_height_and_centre_line` still passes |

Two things the resolution did not get wrong but the merged row did:

- **BR2-1 (fixed).** With a download status showing at 1200 px, the row kept
  every roomy command *and* the 152 px status, leaving the address field about
  140 px — the first version of the new test failed on exactly that. The
  optional trio (bookmark, history pause, PiP — each with an action route)
  now yields to the status first, so the field keeps ≥ 200 px while a
  render reports; Find and the zoom readout keep their tier regardless.
- The other session's `browser_tabs::tests::crash_recovery_waits_for_explicit_choice_and_survives_prompt_close`
  failed once (panic at `browser_tabs.rs:1737`, the second `SessionStore::open`
  on the same temp dir after `drop(tabs)`) and passed on the two reruns — a
  lock-file release race, not touched here; recorded for its owner.

`--shots-live` was not run: it boots Chromium against `https://example.org`,
which this review must not do. Page content is the headless placeholder in
every capture.

## 2. video_editor

### Findings

| id | surface | defect | viewport / theme | file:line (after) | before → after | status |
|---|---|---|---|---|---|---|
| VE2-1 (VE-O2, VE-O3) | export dialog header and footer | filename, mix words and the RESOLUTION caption at fixed offsets off the card top (~3 px between the mix line and the caption); the failure reason shared its slot with the flow notice and its second line ended 1 px above the buttons; "Rendering…" ran into the buttons | 1200×800 dark/light, 480×700 | `app.rs` `export_layout` (6832), `draw_export_modal` (18389), `ExportLayout` (8109) | `video_editor/before/exportdialog.png` (red reason y 505–525 against buttons y 530) → `after/exportdialog.png`, `after/exportdialog-light.png`, `after/exportdialog-narrow.png` | fixed — the body is a stack of measured line boxes and spacing steps (`s1`/`s2`/`s3`/`s4`), the status slot is sized for its tallest occupant (progress bar + line, or two failure lines) and the notice sits under it; option rows take `metrics.min_pointer_target` |
| VE2-2 (VE-O7) | export, unsaved-work, shortcut sheet | hand-drawn cards (own scrim, radius 14, title at +20/+22, buttons at hand offsets) | all | `app.rs` `export_layout`, `discard_layout` (14681), `help_layout` (18145); `modal_layout_with_stacking_actions` (18651) | `before/discard-x2.png`, `before/help-x2.png` → `after/discard-x2.png`, `after/help-x2.png`, `after/help-narrow.png`, `after/discard-narrow.png` | fixed — `ModalFrame::layout` / `paint_backdrop` / `paint_actions` (scrim, card, title band, close control, `Button::paint` actions); the unsaved-work question keeps `closable(false)` / `dismiss_on_scrim(false)` as the code editor's conflict dialog does; three verbs stack full-width when the row is wider than the card (the word-processor's pattern) |
| VE2-3 (VE-O1, dialogs) | all four dialogs at 2× | title over the filename, mix words through the caption, Cancel/Open anyway/Save… drawn through one another, the go-to timecode through its hint, shortcut rows through the next group | 800×900 `--text-scale 2` | as above plus `goto_layout` (12471), `HelpMetrics::measure` (11120) | `before/exportdialog-x2.png`, `before/discard-x2.png`, `before/help-x2.png`, `before/goto-x2.png` → `after/*-x2.png` | fixed — every vertical position is a measured line at the theme type step (`dialog_type`, `line_height`, 18622–18640), so the cards grow with the text; the go-to field keeps its anchor above the transport and grows the same way (76 → three measured lines + steps) |
| VE2-4 | ruler zoom chip | "34.0 px/s" painted after the playhead, over the playhead's time tag whenever the playhead is near the ruler's right edge — the whole time on a phone-width window | 480×900, 390×844, 800×900 at 2× | `app.rs` `draw_timeline` tag placement, `handle_press` chip branch, `playhead_head_rect` | `before/narrow.png`, `before/dark/dev.qos.video-editor-phone.png` (tag hidden under the chip) → `after/narrow.png`, `after/dark/dev.qos.video-editor-phone.png` | fixed — the tag stops short of the chip so the two never overlap (paint and hit agree); a press on the playhead's head over the chip scrubs; the chip's hit rect and keyboard route (Z) are unchanged |
| VE2-5 (VE-O4) | side-pane insets | `PAD` 14 (inspector controls, "INSPECTOR", "MEDIA BIN", the bin's clip count) beside `TOPBAR_EDGE` 12, the track heads' 12 and `padding_standard` (`s3` = 12) | all | `app.rs:84`, `draw_bin` "MEDIA BIN" / count | `before/dark/dev.qos.video-editor-desktop.png` (bin heading x 15, inspector x 915) → `after/…desktop.png` (x 13 / 913) | fixed — one `s3` inset for the topbar, both pane headings, every inspector control and every lane name; `TOPBAR_GAP` 10 stays as round one documented (at `s3` the compact topbar cannot hold both pane toggles beside Import and Export at 420 px) |
| VE2-6 | go-to caret | caret placed from a regular-weight measure of digits drawn bold, so it sat inside the last digit | all | `draw_goto` (18292) | `before/goto.png` → `after/goto.png` | fixed — measured at the drawn weight; caret width from `theme.caret_width` |
| VE2-7 | dialog type sizes | 18 / 17 / 12.5 / 12 / 11 / 10.5 / 10 — five of them off the type scale | all | `dialog_type` | — | fixed — the dialogs draw at `scale.sm` / `scale.xs` / `scale.xl` through `renderer_input_style` once; the segmented option labels at `sm` (`draw_segmented_sized`); the modal title and buttons are the frame's own |
| VE2-O1 (VE-O6) | track-head M / L / S pills, PiP corner buttons | hand-rolled toggle faces | — | `draw_timeline` 17300 area; inspector PiP block | — | recorded — no fitting shared face. `ButtonFace` offers Neutral / Primary / Danger / Ghost and a toolkit `Icon`; the pills need an *on* fill in a semantic tone (danger / warning / info) and the PiP buttons a caller-drawn glyph (a miniature frame with the inset in the named corner; `Icon::PictureInPicture2` shows one corner only). Ask for F2: `ButtonFace::toggle(on: bool, on_fill: Color)` or a `ButtonStyle::Toned(Color)`, and a glyph slot (`ButtonFace::glyph(&dyn Fn(&mut Scene, Rect, Color))`) |
| VE2-O2 (VE-O1) | whole editor at 2× | transport readout over the scrubber, inspector rows and track-head lines collide, ruler labels overlap (and now sit under the zoom chip) | 800×900 at 2× | `TOPBAR_H`, `TRANSPORT_H`, `RULER_H`, `PANE_HEADING_H`, `VIDEO/AUDIO_LANE_H`, `INSPECTOR_HEADER_H`, `regions()` (2052), `inspector_layout` (5909), `lane_rects`, `transport_buttons`, `history_buttons`, `mute/lock/solo_pill_rect`, `bin_item_rects` | `after/large-text.png`, `after/large-text-effects.png` unchanged from before | open — the blocker is that `regions()` and every hit-test run without a `TextRenderer`. The path this round proved on the dialogs applies: cache the measured line heights (`sm`, `xs`, `base`, the 18 px timecode) from the draw pass in a `ChromeMetrics` next to `dialogs`, derive each height from them with the 1× value as the exact result (e.g. `TRANSPORT_H = line(18) + line(13) + 2·s2`, `RULER_H = line(10) + 2·s2 + tick`), and let the by-field fixtures in `chrome_geometry_tests` read the cache. Not attempted here: it touches ~20 geometry functions and 670 tests |
| VE2-8 | shortcut sheet at 2× on 800×900 | two columns decided from the raw window width, so every description elided ("Open a pr…"); one column did not fit the viewport | 800×900 at 2× | `help_layout`, `help_scroll`, the wheel branch of `handle_event` | `after/help-x2.png` (first pass) → recaptured `after/help-x2.png` | fixed — one column when the text-scaled width has no room for two, and a scrolling body with a "scroll for more" band when the table is taller than the window (the pattern `apps/notes/src/shortcuts_overlay.rs` uses); `the_shortcut_sheet_falls_back_to_one_column_and_scrolls_at_large_text` |
| VE2-O4 (VE-O5) | `--pseudo-locale` | icon-only `ButtonFace` labels bracketed to "[]" | `after/pseudo.png` | `ui/src/widget/button.rs` | unchanged | open — F2 |
| VE2-O5 (VE-O8) | segmented rows | `ToggleGroup::paint` Solid face over 3–4 px gapped rects (inspector) / `s1`-gapped (export dialog) | — | — | unchanged | open — F2 |

### Conventions kept

- One geometry description per dialog feeds paint, pointer and cursor shape:
  `ExportLayout`, `DiscardLayout`, `HelpLayout`, `GotoLayout`. The frame
  measures its title and action labels, so a layout needs a theme and a
  renderer; the event path keeps the theme of the last frame
  (`chrome_theme`) and a renderer of its own at that theme's text scale
  (`hit_text`) and resolves the dialog from the *current* state on every
  press and cursor query (`with_dialog_layout`) — never from a cache of
  what an earlier frame painted, which the code review showed a shortcut or
  a resize can make stale. Same inputs, same arithmetic: hit rect ==
  painted rect. A press before the first frame lands on nothing and the
  scrim keeps it, as on screen.
- Every action, keyboard route (Esc / Enter / `?` / Ctrl+E), selection and
  undo path is unchanged; the export dialog's accessible tree is unchanged
  (its close control was already a "Close export" button to a reader). New
  pointer behaviour follows the fleet's modal contract: the card swallows
  clicks, the close control and — where the spec allows — the scrim dismiss;
  the unsaved-work question dismisses on neither.
- Explicit manual sizes are kept as named constants: `EXPORT_CARD_W` 460,
  `DISCARD_CARD_W` 420, `HELP_CARD_W` 700, `HELP_TWO_COLUMN_MIN_W` 780,
  `GOTO_CARD_W` 260.
- Timeline time-to-pixel mapping and clip geometry untouched.

## 3. browser

### Findings

| id | surface | defect | viewport / theme | file:line (after) | before → after | status |
|---|---|---|---|---|---|---|
| BR2-1 | omnibox row with a download status | address field ≈ 140 px at 1200 px (see §1) | 1200×800 | `browser_widget.rs` `build_composition` (`optional_w`, `roomy`) | test evidence only (`the_download_status_shares_the_toolbar_rows_height_and_centre_line`) | fixed — the optional trio (bookmark, history pause, PiP; each with an action route) yields to the status first and returns once the row has both widths to spare beyond the roomy breakpoint; DevTools, Find and the zoom readout keep their tier whatever is downloading |
| BR2-2 | labelled toolbar controls at 2× | "Site", "DevTools", "Find", "100%", "Private" painted past their 56 / 82 / 72 / 72 / 92 px faces (`Button::with_size` pins the face: `flex_shrink: 0`), and every 1× command stayed on a 1200 px row that is a 600 px row to the eye, squeezing the address field | 1200×800 and 480×900 at 2×, 800×900 at 2× | `browser_widget.rs` `text_scale` (478), `labelled_width` (535), `chrome_width` (542), `build_layout` (3966); `daily_controls.rs` `button` (154) | `browser/before/shots-1200x800-x2.png`, `before/large-text.png` → `after/shots-1200x800-x2.png`, `after/large-text.png`, `after/shots-480x900-x2.png` | fixed — a labelled control keeps its explicit 1× width and grows with the text scale (icon-only controls do not); the tiers are judged on `width / text_scale`, so 1200 at 2× lays out as 600 (Forward kept, optional and developer commands gone, address ≥ 200 px). Test `labelled_toolbar_controls_grow_with_the_text_and_the_tiers_follow` |
| BR2-3 (BR-O2) | chrome notice bands, find count, site panel copy and title | `Text::with_size(13.0)` / `with_size(18.0)` off the type scale | — | `browser_widget.rs:1855–1883`, `daily_controls.rs:196`, `site_controls.rs:339, 371` | `after/history-paused.png`, `after/history-paused-x2.png`, `after/site-panel/*.png` | fixed — `TextRole::Body` for bands and panel copy, `TextRole::Title` for the panel title, `TextRole::Label` for fact labels. The other session's `browser_tabs.rs:143/166/168`, `capabilities.rs:307–374` and `settings.rs` sizes are recorded, not touched |
| BR2-4 (BR-O1) | site information facts | "Certificate: …", "Issuer: …", "camera: ask" as single sentences | 390×700 | `site_controls.rs` `panel`, `fact`, `FACT_LABELS`, `fact_labels`; `browser_widget.rs` `fact_label_w` (measured in `draw`, applied in `build_layout`) | round one's `narrow-site-info.png` → `after/site-panel/narrow-site-info.png` | fixed — label / value rows on one column edge (`Container::with_width` label, `with_flex_grow` value), the column sized from the widest label at the label role's size at the current text scale; the connection line becomes a "Connection" fact |
| BR2-5 (BR-O4) | toolbar glyphs | four identical `Icon::Shield` | desktop | `site_controls.rs` `launcher`, `browser_widget.rs` history-pause button | `before/shots-1200x800.png` → `after/shots-1200x800.png`, `after/history-paused.png` | fixed for the icon-only pair that sat side by side (site information → `Info`, protection keeps `Shield`) and for history pause (`Pause` while recording, `Play` while paused — the glyph names the press, as the label does); Private keeps `Shield` beside its label — the set has no incognito glyph (`Eye` says the opposite); icon-set ask recorded |
| BR2-6 | Library heading and section switcher at 2×; site-panel buttons at 2× | "Resume history" 116 px face ran under "Done"; "Bookmarks" / "Downloads" / "Extensions" touched across 132 px faces; the site panel's Done (64), action (160) and origin-toggle (210) faces kept 1× widths | 1200×800 at 2× (`--shots-history-paused`) | `browser_widget.rs` Library heading / section buttons; `site_controls.rs` `button`, `text_scale` | `after/history-paused-x2.png` (first pass, before this fix) → recaptured `after/history-paused-x2.png` | fixed — same labelled-width rule; the desktop section row is a wrapping `Row`, so at 2× it wraps rather than overlaps |
| BR2-7 | download status / Private buttons | height as the literal `40.0` beside `TOOLBAR_CONTROL_HEIGHT` | — | `browser_widget.rs:1785, 1731` | — | fixed — the named constant |
| BR2-O1 (BR-O3) | tab pills | `TabBar` 14 px icon slot at x0+10 vs the strip's 16 px favicon at x0+9 | `after/tabs-1200x800.png`, `after/tabs-x2.png` | `ui/src/widget/tabs.rs` `ICON_W`; `browser_tab_strip.rs:331` | unchanged | open — F6 |
| BR2-O2 | Picture-in-Picture feedback card at 2× | the card's title and caption scale, its two action labels ("Float page", "Select area") do not | `after/pip-empty-x2.png` | `ui/src/widget/toast.rs` `paint_card` actions | — | open — F4/F3 (shared `ToastCard`) |
| BR2-O3 | Library title at 2× | `Text::new("Library").with_size(24.0)` renders at its 1× size while the row's buttons double | `after/history-paused-x2.png` | `browser_widget.rs:1957` | — | open — not understood in this round (`with_size` is documented as renderer-relative and scales elsewhere); the Library pages are under the other session's active development |
| BR2-O4 | `Button::with_size` | an explicit width pins the face at 1×, so every labelled control in the fleet needs a per-call-site `× text_scale` (BR2-2, BR2-6) | — | `ui/src/widget/button.rs` `build_layout` (`flex_shrink: 0`, `max_size: auto`) | — | open — F2: treat an explicit width as a minimum and grow to the measured label at the theme's text scale, so apps stop restating the rule |
| BR2-O5 | `browser_tabs.rs:1737` | `crash_recovery_waits_for_explicit_choice_and_survives_prompt_close` failed 1 of 3 runs on a `SessionStore::open` lock race | — | — | — | recorded for the other session |

### Conventions kept

- Shared widgets only; the browser's own drawing is still limited to the tab
  strip and the suggestions overlay.
- Tier flags remain stored state updated in `apply_layout` (now from
  `chrome_width()`), seeded by `debug_set_viewport_width` and corrected in
  `build_layout` once the theme is known — the harness's first observable
  layout already has the right tier.
- Private-window semantics, `--private`, the fullscreen early return and the
  download-status binding are untouched; every explicit manual size is kept
  (they scale, they are not replaced).

## 4. Code review

`code-review high` over `381eff6a8..HEAD` (two angles; the coordinator relayed
the ranked findings). Every user-visible regression the round's edits had
introduced was fixed before the final commit; one item is recorded.

| finding | disposition |
|---|---|
| export dialog: on a window too short for the natural stack the frame clamps the card and the body clip cut the status, notice and progress rows away — a failure reason vanished exactly when the window was small | **fixed** — the footer (size line, status slot, notice) is anchored to the body's foot and the stack gives up, in order, the gaps between groups, the header's gap, the option rows' height down to their labels, the levels' verdict and the gap over the status slot (`export_layout`); `the_export_footer_stays_on_the_card_when_the_window_cannot_hold_the_dialog` proves 1280×720 at 2× and 1280×440 at 1× |
| shortcut sheet: two columns decided from the raw window width, so at 2× every description elided | **fixed** — decided from the text-scaled content width; one column at 800 px / 2×, the body scrolls when the table is taller than the window (`help_scroll`, wheel over the sheet, a "scroll for more" band at the body's foot) and every row stays reachable; `the_shortcut_sheet_falls_back_to_one_column_and_scrolls_at_large_text` asserts the description column holds the widest description and the last row is reachable |
| `press_export` / `cursor_shape` / discard / help / go-to hit-tested against dialog geometry cached from the last paint — stale after a shortcut opened a dialog or a resize | **fixed** — the paint cache is gone; the event path resolves the layout from the current state with the theme of the last frame and its own renderer at that theme's text scale (`chrome_theme`, `hit_text`, `with_dialog_layout`); `the_export_dialog_answers_a_click_the_frame_after_the_shortcut_opened_it` opens the dialog with Ctrl+E and clicks an option with no frame in between, then resizes and clicks again |
| browser: Find and the zoom readout were gated on whether a download was running | **fixed** — the developer commands keep their tier; the optional trio (bookmark, history pause, PiP — each with an action route) yields to the download status first and returns once the row has both the status's and their own width to spare beyond the roomy breakpoint; the download-status test asserts DevTools / Find / zoom present and the trio absent at 1200 px |
| unsaved-work question: the two body lines could paint under the stacked verbs | **fixed** — the body is clipped to `layout.body` as the other dialogs are; the stacking layout already grows the card by the extra rows |
| modal scrim lighter than the bin flap's | **recorded** — `ModalFrame` paints `MODAL_SCRIM_ALPHA` 0.45 through `chrome::fill_scrim` (soft-light) and takes no alpha; the bin flap paints `SCRIM_ALPHA` 0.55 as a plain fill of `surfaces.scrim`; the old export card painted 0.72. The two models are not the same operation, so the numbers are not directly comparable; the fleet's modal scrim is what every other app's dialog gets and the export-dims test still holds. An alpha parameter on `ModalSpec` is the F4 ask |
| a disabled Export offered a pointer cursor | **fixed** — `cursor_shape` offers the pointer only for an enabled action; `a_disabled_export_button_offers_no_pointer` |
| Ctrl+E opened the export dialog under the shortcut sheet | **fixed** — the sheet owns the keyboard like the dialogs (Esc / `?` close it, nothing else fires); `ctrl_e_does_not_open_the_export_dialog_under_the_shortcut_sheet` |
| site-panel fact label column sized from a spacing constant | **fixed** — measured in `draw` from the widest label (certificate facts plus the origin's permission names) at the label role's size at the current text scale and applied on the next layout pass (`fact_label_w`, `SiteControls::fact_labels`); the two-`s16` value is only the first-frame fallback |
| the "explicit width × text scale" rule spelled twice | **fixed** — one `pub(crate) fn labelled_width` in `browser_widget.rs`, used by the toolbar, the find bar and the Library heading |
| export option rows the body clip hides on a very short window stayed hit-testable (and offered a pointer) | **fixed** — `press_export` and `cursor_shape` accept an option only where its rect intersected with the body contains the point |
| zoom chip painted under the playhead tag but still claimed the press first, so pressing the visible tag zoomed to fit | **fixed** — the tag now stops short of the chip (`tagx.min(chip.x0 − tw − 8)`) so the two never overlap, and a press on the playhead's head at the ruler's end scrubs rather than zooming (`playhead_head_rect`) |
| the export card grew and re-centred when the transient flow notice appeared on the Export click | **fixed** — the notice line is always reserved, like the status slot (`notice_top: f64`) |
| history-pause showed `Pause` while paused, on a control whose label says "Resume" | **fixed** — `Play` while paused, `Pause` while recording |
| site-panel labelled buttons (Done 64, actions 160, the origin toggle 210) kept 1× faces at 2× | **fixed** — `SiteControls::text_scale` (set in `build_layout`) feeds `labelled_width` |
| `modal_layout_with_stacking_actions` copies the word-processor's stacking without its `height.min(available / n)` guard, so on a clamped card the stacked verbs could consume the whole body | **fixed** — the guard is in; moving the stacking into `ModalFrame` itself (`ModalSpec::stack_actions_when_wide`) is the F4 ask, recorded |
| `capabilities.rs`: `terminal_notice` is never cleared, so `toolbar_status()` stays `Some` for the session after the first finished download | **recorded** — the other session's file |

## 5. Tests

All runs with `--locked --no-default-features`, `CARGO_BUILD_JOBS=1`,
`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."`
(the parent checkout's `.cargo/config.toml` otherwise breaks `ring`; nothing
in-repo changed).

| command | result |
|---|---|
| `cargo build --bins -p video_editor -p browser` at the base and after | clean |
| `cargo check -p video_editor -p browser --tests` (final tree) | clean, no warnings (3 m 52 s) |
| `cargo test -p video_editor --lib -- chrome_geometry_tests export help discard goto shortcut scrim modal dialog transport topbar inspector bin readout shuttle walk section marker title mix cursor ctrl_e zoom chip` | 174 passed, 0 failed, 0 ignored, 503 filtered out (round one's 147, the 6 new `chrome_geometry_tests`, the 3 new `mod tests`, and the wider filter) |
| `cargo test -p browser --lib --test chrome_alignment --no-fail-fast -- crash_recovery site_panel narrow toolbar pip library history private address tab suggestions find compact chrome download roomy labelled fullscreen exit` | lib 124 passed, 0 failed, 2 ignored (pre-existing `#[ignore]` child-process entry points); bin unit tests 1 passed; `chrome_alignment` 6 passed |
| `QOS_SITE_RENDER_EVIDENCE=… cargo test -p browser --lib populated_site_panels_render_at_narrow_width` | passed; wrote `target/ui-review/browser/after/site-panel/{narrow-protection,narrow-site-info}.png` |

New regression tests:

- `apps/video_editor/src/app/chrome_geometry_tests.rs`:
  `the_export_dialog_stacks_its_body_without_overlap_at_every_text_scale`
  (1280×800 at 1×, 800×900 at 2×, 480×700 at 1×),
  `the_export_footer_stays_on_the_card_when_the_window_cannot_hold_the_dialog`
  (1280×720 at 2×, 1280×440 at 1×),
  `the_discard_question_keeps_its_verbs_inside_the_card_on_a_phone_and_at_large_text`
  (incl. 390×844 and 480×900 at 2×),
  `the_go_to_field_stacks_its_three_lines_above_the_transport`,
  `the_shortcut_sheet_rows_grow_with_the_text`,
  `the_shortcut_sheet_falls_back_to_one_column_and_scrolls_at_large_text`.
- `apps/video_editor/src/app.rs` (`mod tests`):
  `the_export_dialog_answers_a_click_the_frame_after_the_shortcut_opened_it`,
  `ctrl_e_does_not_open_the_export_dialog_under_the_shortcut_sheet`,
  `a_disabled_export_button_offers_no_pointer`.
- `apps/video_editor/src/app.rs` tests moved from replicated offsets to the
  production layout: `a_long_export_failure_wraps_above_the_footer_and_buttons`,
  `the_shortcut_sheet_has_room_for_every_shortcut_it_lists`,
  `the_export_dialog_header_never_collides_with_the_first_section`,
  `the_export_button_is_only_offered_when_export_can_actually_run`,
  `the_export_modal_dims_the_editor_rather_than_deleting_it`,
  `the_export_dialog_stays_inside_the_window` — each now renders (or lays out
  with a renderer) and reads `dialogs.*` / `export_layout` back.
- `apps/browser/tests/chrome_alignment.rs`:
  `the_roomy_tier_starts_at_a_thousand_pixels`,
  `the_download_status_shares_the_toolbar_rows_height_and_centre_line`,
  `labelled_toolbar_controls_grow_with_the_text_and_the_tiers_follow`.

## 6. Limits

- macOS: no `shots` binary, no headless compositor; app `--shots` routes and
  `FrameHarness` are the evidence. `--shots-live` deliberately not run.
- The download status has no capture route; its row geometry is test evidence.
- The other session's browser files (`browser_tabs.rs`, `capabilities.rs`,
  `settings.rs`, `chromium/`) were not edited; their off-scale sizes are
  recorded in BR2-3.
- VE2-O2 (whole editor at 2×) is scoped, not done; the dialogs prove the
  approach.
- Capture evidence for the review fixes that only show on windows the
  capture matrix does not include (export at 1280×440, the help sheet's
  scrolled state) is test evidence (§5); the 800×900 at 2× help capture
  shows the one-column sheet with its scroll band.
