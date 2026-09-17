# UI alignment review, round two — A4 Communication (feeds, chat, mail, totp)

Branch `ui-review/r2-a4-communication`, cut from the round-one merge tip
`381eff6a8` ("Merge ui-review/integration"). Worker A4 of the 19-way second
round; scope is exactly `apps/feeds/`, `apps/chat/`, `apps/mail/`, `apps/totp/`.
Nothing under `ui/`, `shell/` or `crates/` was touched; `.cargo/config.toml`,
`Cargo.toml` and `Cargo.lock` are unchanged in this worktree (`git status`
shows nothing under `.cargo` — the reviewer's report of a modified config came
from another tree).

## 1. Feeds merge verification (first)

The round-one `apps/feeds/src/app.rs` conflict (my `list_layout::{star_rect,
article_row_layout}` vs. the user's dynamic `self.list_row_h` and
`two_line_elide(…, weight)`) was resolved by the coordinator to: rows use
`self.list_row_h`; star, marker, time, title and meta come from
`article_row_layout` over that row rect; `two_line_elide` keeps the weight
parameter. Verified by hand on the merge tip **before any edit**:

| check | result |
|---|---|
| `draw_list` reads (`app.rs`) | one `icons::dot`/`icons::ring` marker per row on `row.marker_cy`; one `draw_text` of the time at `row.time_y`; title at `row.title_y`, meta at `row.meta_y`; star at `(star_rect.center().x, row.star_cy)`; `two_line_elide(tr, &a.title, 15.0, title_w, title_weight)` — no duplicated draw, no leftover `y0 + 22` marker |
| `cargo test -p feeds --lib` on the merge tip | **107 passed** (matches the post-merge count) |
| `target/ui-review/feeds/before/detail.png` (1200×800, 1×) | one marker + one time per row, star on the row centre, title/meta grouped with the small gap and the larger gap between articles |
| `target/ui-review/feeds/before/narrow.png` (480×900) | same; meta line stops before the star |
| `target/ui-review/feeds/before/large-text.png` (800×900 @2×) | rows grow with the measured lines (two-line titles sit above their meta line), sidebar rows grow, marker/time on the first title line, no overlap |

Verdict: the resolution is correct; nothing in it needed fixing. The one
refinement made in this round (below, F2) moves the row-height *formula* into
`list_layout::row_height` so the row is sized from the same metrics that lay it
out, with the user's WIP test contracts (`sidebar row ≥ line + 8`, `article row
≥ 2·title + meta + 30`) kept as-is and passing.

## 2. Method

1. Setup as instructed; BEFORE captures for all four apps with the given
   `cap()` matrix (`target/ui-review/<app>/before/…`, 78 PNGs + three 200 %
   scenario shots added for mail `setup`/`compose` and chat `call`), every PNG
   opened with Read. Helper scripts: `target/ui-review/cap.sh`, `one.sh`.
2. Source pass over each open item, then the fixes below. Rule applied
   throughout: the geometry a surface paints is the geometry it hit-tests, both
   derived from **one measured-metrics struct** computed with the paint styles
   (`CallOverlayMetrics`, `PanelMetrics`, `SetupMetrics`, `ComposeMetrics`,
   `ListRowMetrics` extensions); the authored 1× rules are that struct's
   `Default`, so 1× geometry does not move and the geometry tests (which never
   paint) exercise the same code. Where the layout entry has no renderer
   (`AddAccountPanel::layout`, `MailApp::rebuild_overlay_hits`,
   `ComposeWindow::layout`, `ChatApp::overlay_controls_layout`) the paint pass
   measures first and re-runs the layout when the metrics changed, so a single
   headless frame paints measured rects.
3. `code-review` (medium) on the diff; 14 review findings were fixed before
   commit (section 6), none recorded as open.
4. Tests and gates; AFTER captures on the same matrix; side-by-side.

Coordinator constraint honoured: no new app-side text-scale multiplication on
NavShell inputs — mail's `narrow` breakpoint, the `CHIP_INSET`/`CHIP_SIZE` chip
band and `ACCOUNT_HEADER_H` are untouched (`list_header_layout` only receives
the measured title box for the existing chip-centring rule).

## 3. Findings and fixes

Status legend: **fixed** — in this branch; open — recorded with owner.

### chat

| id | surface | defect | viewport | file | before → after | status |
|---|---|---|---|---|---|---|
| C11 | category header rows | fixed 24 px rows with `y + 5` text: "CHANNELS"/"VOICE" painted over the first channel row at 200 %, caret and "+" off the centre line, "+ Category" chip a fixed 106×22 box its label outgrew | 800×900 @2× | `render_chat.rs` `draw_sidebar_tree`, `draw_sidebar_header` | `chat/before/large-text.png` → `chat/after/large-text.png` | **fixed** — `category_row_h` (measured name + `s1` each side, ≥ 24), caret/name/"+" on `line_top(cy, measured)`, "+" a square the row's height, name budgeted to stop `s2` before it; chip from `add_category_chip_size` (label + `s2`/`s1` pads, ≥ 106×22), label centred; tree starts under the measured chip |
| C11 | identity bar | fixed 56 px bar, avatar r 16, name at `cy − 14`, status at `cy + 2`: at 200 % "You" and "Available" stacked onto each other and the initials outgrew the disc | 800×900 @2× | `draw_sidebar_identity_bar`, `draw_identity_row`, `draw_call_controls` | same | **fixed** — `identity_bar_h` (name + status lines + `2·s3`, ≥ 56) shared by the bar and the tree's bottom bound; disc radius × `text_scale`; name+status one block centred on the bar; mute/deafen squares from the measured glyph (≥ 32), centred |
| C10 | thread composer | fixed 48 px box vs. the 76 px main composer for the same control; the thread body stopped at a fixed `panel.y1 − 70` | `thread-narrow.png` | `draw_thread`, `draw_thread_body`, `app.rs` | `chat/before/thread-narrow.png` → `chat/after/thread-narrow.png` | **fixed** — `thread_composer_rect` follows the main composer's rule (`COMPOSER_MIN_H`, grows with the measured draft to `COMPOSER_MAX_H`); the body ends `s2` above it |
| C8 | expanded call controls | `overlay_button_width` = `chars × 8 + 26`: at 200 % "Mute Deafen Audio only Collapse Leave" ran together; title over subtitle; name plates overflowed | 800×900 @2× (`call-large.png`) | `render_call.rs` | `chat/before/call-large.png` → `chat/after/call-large.png` | **fixed** — `CallOverlayMetrics` (button widths for **all seven** labels incl. both halves of each toggle, button height, header line boxes, `s1/s2/s3/s6`) measured in the paint pass, cached on `(renderer scale, theme scale, catalogue revision)`, consumed by `overlay_controls_layout`/`overlay_content_region`; header stacks from measured boxes; `ctl` and `name_plate` centre on measured boxes; `request_relayout()` when the metrics change so the host tile's surface node follows on the next frame (`build_layout` has no renderer; the placeholder is painted from live geometry) |
| C12 (new) | inbox sidebar | fixed 36 px rows, `row.y0 + 9` text, 22 px workspace label rows | 800×900 @2× | `draw_inbox_sidebar` | — | **fixed** — `inbox_row_h`/`inbox_label_h`; badge measured and reserved before the name is truncated; all centred on `line_top` |
| C9 | strings | literals | — | — | — | **partly fixed** — every string touched this round is a key (`chat-identity-*`, `chat-call-*`, `chat-sidebar-add-category`); the shared faces receive keys, not resolved text. Remaining literals in untouched surfaces stay open (convention) |

Tests: `cargo test -p chat --lib` — see section 5. New: `render_call::tests::{overlay_metrics_fall_back_to_the_authored_rules_before_the_first_paint, measured_button_widths_replace_the_estimate_label_by_label, a_scaled_header_pushes_the_tile_band_down_by_its_extra_lines}`; `i18n::tests` covers every new key.

### feeds

| id | surface | defect | viewport | file | before → after | status |
|---|---|---|---|---|---|---|
| F2 | article row height | the merged `list_row_h` was `12 + 2·title + 8 + meta + 10`, a second formula next to `article_row_layout`'s | 800×900 @2× | `app.rs` `paint`, `list_layout.rs` | `feeds/before/large-text.png` → `feeds/after/large-text.png` | **fixed** — `list_layout::row_height(title_line, meta, gap: s2, pad: s3, min: 88)`: the row reserves `s3` above/below and an `s2` gap, the block is then grouped inside it with the tighter `s1` (HIG §7a); sidebar rows = line + `2·s1` + `SIDE_FEED_ROW_INSET` (nested feed rows are that much shorter), section rows = line + `2·s1`; 1× unchanged (88/34/24); the user's WIP contract `large_text_rows_contain_their_measured_content` kept and passing |
| F3 | sidebar `0.62` baseline | `cy − size × 0.62` | — | `draw_sidebar` | — | **resolved by the merge** — the merged tree already centres on the measured line box (`cy − measured/2`); nothing left to do |
| F4 | strings | literals; no `locales/` | — | — | — | open — no feeds string was touched this round (geometry only), so no catalogue was started; convention |

Tests: `cargo test -p feeds --lib` — see section 5 (two new `list_layout` tests: `one_x_row_keeps_the_authored_height`, `a_scaled_row_makes_room_for_two_title_lines_and_the_meta_line`).

### mail

| id | surface | defect | viewport | file | before → after | status |
|---|---|---|---|---|---|---|
| M7 | list header search box | fixed 32 px box with a hand-drawn 13 px "Search" over the shared `TextInput`; at 200 % the placeholder overflowed the box, the refresh square stayed 32 px, rows started under the fixed 56 px header | 800×900 @2× | `pane.rs` | `mail/before/large-text.png` → `mail/after/large-text.png` | **fixed** — `ListRowMetrics::{ctl_h, header_h}`: control = search line + `2·s2` (≥ 32), header = controls + inset (≥ 56); rows and the empty state start at `header_h`; the search/clear glyphs are squares the box's height with the `icon_sm` token scaled; the placeholder is the field's own (`with_placeholder("mail-search-placeholder")` — the key, the field resolves it); `list_header_layout` takes the measured title box |
| M7 | avatars | initials outgrew the fixed 17 px discs | 800×900 @2× | `pane.rs` list rows + reader card | same | **fixed** — `ListRowMetrics::avatar_r = 17 × text_scale` (row height keeps the disc inside); reader card `avatar_r` scaled likewise; text column one `s3` past the disc |
| M5 | account setup sheet | 34 px fields, 120 px label column, 168/92/120×38 buttons, labels at `y0 + 9`, 500 px card: at 200 % heading over subtitle, labels cut ("Display nan"), field text overflowing, pill/button labels overflowing, footer into the card edge; at 200 %×720 Save/Cancel would cover the SMTP row (and win the click, since the footer resolves last) | `setup-large.png` (800×900 @2×) | `app.rs` | `mail/before/setup-large.png` → `mail/after/setup-large.png` | **fixed** — `SetupMetrics` (heading/subtitle/label/caption/hint/note line boxes, field height from the input line + `2·s2`, label column from the widest resolved label + `s3`, pill/OAuth/footer widths from measured labels, `s1/s4/s6`, resolved labels) measured in `draw_setup`, cached on `(scales, revision, OAuth label)`, then `rebuild_overlay_hits` runs from it; the card is `panel_h()` (504 at 1×) clamped to the window; **the row band scrolls** when clamped (`setup_scroll`, wheel + focus-into-view), rows outside the band get no hit rect and are clipped, the hint anchors to the footer — hit rects never overlap; pills on the shared `SegmentedFace`, OAuth/Save/Cancel/Remove on the shared `ButtonFace` (keys handed over; the OAuth label is interpolated so it is resolved once), TLS toggle scaled and centred; strings through the new catalogue |
| M5 | compose window | 32 px fields, labels at `y0 + 8`, hand-drawn "recipients" over the `To` input, chip/action labels at fixed offsets, 30 px toolbar squares, 46/30 px prompt bar painted over the body, attachment chips at `r.y1 − 52 − 30`: at 200 % rows/labels stacked, toolbar glyphs collided, footer labels overflowed | `compose-large.png` (800×900 @2×) | `compose.rs` | `mail/before/compose-large.png` → `mail/after/compose-large.png` | **fixed** — `ComposeMetrics` (title/label/field/toolbar/button heights, chip/toggle/button/prompt widths) measured in `draw`, then `layout` re-runs; the default window follows the scale, and a later scale change rescales the window **in place** (placement is never reset); header rows, toolbar squares, button bar, attachment-chip band and prompt band are all reserved by `relayout_regions` and read back by the painters (`chips_band`, `prompt_band`); `To` gets the field's own placeholder (key); Send/Save draft/Discard and the prompt's Add/Cancel on `ButtonFace` with keys; labels centred on measured boxes; strings through the catalogue |
| M4 | compact inbox header | empty 60 px hamburger band above the list header | 480×900, 800×700 | NavShell hosting | `mail/before/narrow.png`, `compact.png` | open — needs **F1/F6** (coordinator: F1 is dropping the chip band above a docked NavShell sidebar and will switch mail to `nav.mode()` on the integration branch); deliberately not touched here |
| M6 | strings | literals | — | — | — | **partly fixed** — `apps/mail/locales/en-US.ftl` + `src/i18n.rs` created (same shape as chat's); every string touched (header placeholder, whole setup sheet, compose labels/chips/buttons/prompt) is a key; untouched surfaces stay literal (convention) |

Tests: `cargo test -p mail --lib` — see section 5. New: `pane::tests::list_header_and_avatar_keep_their_one_x_sizes_before_the_first_paint`, `app::tests::{setup_sheet_one_x_metrics_reproduce_the_authored_sheet, setup_sheet_scaled_metrics_keep_labels_fields_and_buttons_apart, setup_sheet_in_a_short_window_at_200_percent_never_overlaps_its_footer}`, `compose::tests::{one_x_metrics_reproduce_the_authored_bands, scaled_metrics_grow_the_header_rows_and_the_buttons_hold_their_labels}`, `i18n::tests` (every key + the interpolated OAuth label). `list_title_is_centered_on_and_spaced_from_the_collapse_chip` additionally asserts a 200 % title box still centres on the chip.

### totp

| id | surface | defect | viewport | file | before → after | status |
|---|---|---|---|---|---|---|
| T6 | add-account panel | every band a fixed pixel offset (`card_h 56`, `label_gap 18`, `INPUT_H 42`, title `y0 + 24`/`y0 + 50`, labels `y0 − 16`, buttons `min(140)`): at 200 % title over subtitle, card titles over descriptions, labels hidden behind fields, "Advanced options" clipped, "Add account" overflowing its button | `add-large.png` (800×900 @2×) | `add_panel.rs` | `totp/before/add-large.png` → `totp/after/add-large.png` | **fixed** — `PanelMetrics` (title/subtitle (wrapped to the card width)/label/card/divider/advanced/input/button line boxes, label widths, `s1/s2/s3/s6`) measured in `draw` (skipped until the window is known, cached on `(scales, revision, flow, content width)`), then `layout` re-runs; every band = measured text + token pads ≥ the authored 1× band; footer pair shares one width — the authored 140 px, grown only when the wider label needs it; card text single-line and elided before the chevron, short description chosen at `400 × scale`; strings through the new catalogue (`totp-add-*`), faces receive keys |
| T8 | account avatars | initials outgrew the fixed 24 px disc | 800×900 @2× | `list.rs` | `totp/before/large-text.png` → `totp/after/large-text.png` | **fixed** — `RowMetrics::avatar_r = 24 × text_scale` in the fit width, row heights and layout; stacked rows keep the disc's 1× one-`gap` overhang (`stacked_identity_band`) so 1× does not move |
| T9 (new) | countdown ring label | `size = radius × 0.95` renderer-relative while the radius is already text-scaled → 4× digits at 200 % spilling past the ring | 800×900 @2× | `ring.rs` | same | **fixed** — the design size passes through `renderer_input_style` once |
| T7 | strings | literals | — | — | — | **partly fixed** — `apps/totp/locales/en-US.ftl` + `src/i18n.rs`; every add-panel string is a key; the list's untouched literals stay (convention) |

Tests: `cargo test -p totp --lib` — see section 5. New: `add_panel::tests::{one_x_metrics_reproduce_the_authored_bands (pins the 140/140 footer), scaled_metrics_grow_every_band_and_the_form_still_lays_out}`, `list::tests::the_stacked_identity_band_is_unchanged_at_one_x_and_grows_with_the_disc`, `i18n::tests`.

## 4. Fresh eyes (800×700, 480×900, 200 %)

- chat 800×700 and 480×900: clean. 200 %: C11/C8/C12 above.
- mail 800×700 and 480×900: clean except the M4 band (F1/F6). 200 %: M7/M5.
- totp 800×700 and 480×900: clean. 200 %: T8/T9; add screen T6.
- feeds 800×700, 480×900, 200 %: clean after the merge; the fixed 256 px
  sidebar truncates long feed titles at 200 % ("Quanta Maga…") — a sidebar
  width that does not follow the text scale. Open — needs the F1 NavShell
  scaling the coordinator described; not changed here.

## 5. Tests run

All commands prefixed with the inline environment from the instructions
(`RUSTFLAGS="-C force-frame-pointers=yes --remap-path-prefix=/build=."
RUSTC_WRAPPER=sccache SCCACHE_CACHE_SIZE=200G CARGO_BUILD_JOBS=1
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig`), `--locked
--no-default-features`.

| command | result |
|---|---|
| `cargo test -p feeds --lib` (merge tip, before any edit) | 107 passed |
| `cargo check -p chat -p mail -p feeds -p totp --tests` (gate) | ok; one pre-existing warning (`mail::engine::report_engine_error` dead code) |
| `cargo test -p feeds --lib` | 109 passed, 0 failed (107 at the merge tip + 2 `list_layout` tests) |
| `cargo test -p chat --lib` | 231 passed, 0 failed (228 + 3 `render_call` metrics tests; `geometry` 9 unchanged) |
| `cargo test -p mail --lib` | 228 passed, 0 failed (`pane::tests` 19 incl. the new header/avatar test; 3 new `app` setup-sheet tests, 2 new `compose` tests, 2 `i18n` tests) |
| `cargo test -p totp --lib` | 107 passed, 0 failed (103 + `add_panel` 2, `list` 1, `i18n` 1) |
| `cargo build --bins -p feeds -p chat -p mail -p totp` | ok |

No test was ignored, weakened or deleted. The user's feeds WIP test
`large_text_rows_contain_their_measured_content` failed once during the work
(my first sidebar pad was `2·s1`, exactly at the test's `line + 8` bound while
nested rows are 2 px shorter); the fix was in the geometry (reserve the nested
inset), not in the test.

## 6. Code review

`/code-review medium` plus the coordinator's relayed finder reports (Angles A,
B, C, Efficiency) surfaced 14 diff-introduced findings; all fixed before commit:

1. totp `layout` hard-coded `(4, 8, 12, 24)` while paint used `theme.tokens.spacing` → the steps ride in `PanelMetrics` (measured from the theme; base tokens before the first paint). Same for mail `SetupMetrics::fields_top/panel_h` (`s1/s4/s6` fields).
2. compose attachment chips painted at `r.y1 − 52 − 30` while layout reserved a measured band → `chips_band` stored by layout, read by the painter.
3. totp `draw` measured the wrapped subtitle with a zero window before the first layout → measurement skipped until the window is known.
4. totp card description drawn with no width → elided to the width before the chevron.
5. chat metrics measured in `draw` but consumed by `build_layout` earlier → `request_relayout()` on change (no renderer is available in `build_layout`; the shots frame is consistent because the placeholder paints from live geometry).
6. `ensure()` in the new `i18n.rs` files called `I18nManager::init`, replacing a host-installed manager/locale → the catalogue is added to the active locale and a manager is created only when none exists (revision-delta probe, pseudo-locale paused for the fallback lookup); chat's copy got the same fix.
7. Efficiency: `overlay_control_specs`/`SetupMetrics::measure`/`ComposeMetrics::measure`/`PanelMetrics::measure` and the setup label `Vec<String>` re-resolved and re-measured every frame → cached on `(renderer scale, theme scale, catalogue revision, state)`, labels stored in the metrics; constant glyph measures hoisted out of the chat row loops.
8. chat `button_width` fell back to the estimate for a label that flipped since the measurement → all seven labels (both halves of each toggle) are measured.
9. totp footer widths became 96/132 at 1× → equal widths, the authored 140 cap, grown only when the wider label needs it; pinned in the 1× test.
10. compose prompt bar kept fixed 46/30 px and literal "Add"/"Cancel" → on `ComposeMetrics`, its band reserved in the body, labels via the catalogue on `ButtonFace`.
11. mail sheet clamped to the window while rows were measured → scrollable row band with clipped hits and paint; 200 %×720 test asserts no hit overlaps and the last row is reachable by wheel and by focus.
12. `ButtonFace::new`/`with_placeholder` were handed resolved strings (double translation / pseudo expansion) → keys are passed; the interpolated OAuth label is the documented exception.
13. compose reset `placed` on a scale change → the window rescales in place; placement is never discarded.
14. "`.cargo/config.toml` modified" → not in this worktree (`git status` clean there); nothing staged from it.

Review findings recorded rather than fixed (need `ui`):

- The three `i18n.rs` files are copies (chat's shape); the manager-probe
  logic belongs in `ui::i18n` as one helper. Open — needs **ui/i18n**.
- `X × theme.text_scale.max(1.0)` and the `measure("Ag", …)` line-box probe
  recur across the four apps; one `Theme`/`TextRenderer` helper should own
  the `.max(1.0)` decision and the probe. Open — needs **F2/F3** (same as
  round one's "measured-line centring has no home").
- `SearchField` has no `apply_layout_external`, so mail's canvas header keeps
  its plain-chrome `TextInput`. Open — needs **F2**.
- `Form`/`Field`/`Actions` are retained composites that own their controls;
  hosting them inside the hand-painted setup sheet / add panel would move the
  eight `TextInput`s out of `MailApp` and re-plumb tab order, a11y and the
  overlay-hit tests. This round adopted the shared *faces* instead
  (`ButtonFace`, `SegmentedFace`, `ToggleFace`, `TextInput::with_placeholder`)
  and one measured-metrics struct per sheet. Open — needs **F3** (an external
  layout hook or a paint-face `FieldFace`) before the retained form can be
  hosted.

## 7. AFTER captures

Same matrix as BEFORE under `target/ui-review/<app>/after/…` (81 PNGs), every
PNG opened with Read; the three 200 % scenario shots (mail `setup-large`,
`compose-large`, chat `call-large`) and mail `large-text` were retaken after
the last fixes.

| capture | verdict |
|---|---|
| `chat/after/large-text.png` | category headers clear the first row; caret, name and "+" on one centre line; "+ Category" chip fits its label; identity bar stacks "You" over "Available" beside a disc that holds the initials |
| `chat/after/call-large.png` | five controls hold their labels with even pads; title above "4 in the call"; name plates wrap their text; tile band starts under the measured header |
| `chat/after/thread-narrow.png` | thread "Reply…" composer is the 76 px control on the main composer's baseline; body stops above it; 1× sidebar unchanged |
| `feeds/after/detail.png`, `narrow.png`, `large-text.png` | 1× identical to the merge tip; 2× rows reserve the `s2` gap (a little more air per block); sidebar rows unchanged |
| `mail/after/large-text.png` | search box holds its placeholder, refresh square matches, rows start under the measured header; sender / subject / snippet share one column past the scaled disc |
| `mail/after/setup-large.png` | heading over subtitle, full labels, sized fields, pills/buttons hold their labels, TLS toggles scaled, "Port" caption clear of the password row, footer inside the card |
| `mail/after/compose-large.png` | rows stack, labels centre on fields, toolbar glyphs fit their squares, button bar holds its labels, the window followed the scale so a full body remains |
| `mail/after/setup.png`, `compose.png` | 1× geometry kept (34 px fields, 120 px column, 504 px card; 32 px rows, 560×460 window). Two deliberate style changes from adopting the shared faces: the sign-in pills are the `SegmentedFace` pill look, and Remove is the shared danger face |
| `totp/after/large-text.png` | ring seconds label inside the ring; discs follow the text scale; row heights hold the discs |
| `totp/after/add-large.png` | wrapped subtitle has room, card titles over short descriptions, labels clear their fields, divider caption centred, "Advanced options" unclipped, both footer buttons hold their labels |
| `totp/after/add.png` | 1× authored geometry kept, including the 140/140 footer pair |
| `*/after/compact.png`, `narrow.png` | unchanged from BEFORE (clean) apart from mail's M4 band |

Residual visible at 200 % in chat, mail and totp: the avatar **monogram** still
crowds its disc. `Avatar::draw_at` sizes the initials at 0.4 × the disc
*through the renderer*, which applies the text scale a second time, so at
200 % the monogram is 0.8 × the disc whatever radius the app passes; scaling the
disc (done here) halves the overflow, the rest is round one's systemic
finding 1 (double text scaling) inside `ui`. Open — needs **F2**:
`Avatar::draw_at` must pass its text size through `renderer_input_style`.

## 8. Limits

- macOS: no compositor, `shots` binary or `qos-control`; evidence is the
  apps' own `--shots`/`--responsive-matrix` output plus the pure-geometry unit
  tests on the metrics structs (hit == paint proofs).
- `build_layout` has no `TextRenderer`; chat's host video *surface node* still
  follows the measured overlay one frame late after a scale change
  (`request_relayout` closes it); the painted placeholder never lags.
- The headless host settles layout before one draw, so every metrics cache
  re-runs its layout inside `draw` when it changes; a `Widget` measure-phase
  hook for canvas apps would remove that pattern. Open — needs **ui**.
