# A7 — Office apps (word-processor, sheets, slides): 1× dark + light polish

Branch `ux1x/a7-office`, cut from Unit 0 `fd1f84391`. Files edited: `apps/word-processor/src/**`,
`apps/sheets/src/**`, `apps/slides/src/**` and this report. Evidence under
`target/ux1x/a7/<app>/{before,after}/{dark,light}/` (all `--shots` scenarios / flags at 1280×800,
the `--responsive-matrix` set, and — for slides — the six viewport-specific scenarios at their own
viewport as `<scenario>-narrow.png`). Every PNG named below was opened with the Read tool.

## Root cause, in one line per app

- **word-processor** — `ChromeColors::for_theme` mapped every chip to `neutral[2]` and every rule
  to `border_standard`; on the dark preset both sit within a shade of the raised bar, so every
  button, toggle, stepper, layout chip and bar rule vanished at 1×. Light was fine, which is why it
  shipped.
- **sheets** — every chrome edge was `border_standard` (chips, name box, formula field, bar rules,
  header-band rules, cards, three menus/dialog); menus stacked `raised`+`overlay` under a literal
  black shadow; row hover/active was an accent wash; annotation cards floated over the column
  header band and 2 px into the scrollbar lane.
- **slides** — panel dividers were `border_standard` strokes centred on integer coordinates (2 px
  blur, invisible on dark); every hand-painted button, swatch, pill, notes box, help card, menu and
  presenter panel stroked a *declared-alpha* ink veneer (`text_primary/26..60`,
  `Color::BLACK/50`, `Color::WHITE/40`); menus and the fill-picker popover used literal black
  shadows; the fill-picker caption shared the picker's top edge.

## Six-rule checklist

1. **No touching / overlap** — WP: review comment card cleared the scrollbar lane
   (`review_overlay.rs` `CARD_RIGHT_INSET = s3`, before: 2 px overlap with the thumb), Resolve /
   Reply buttons sized to their measured labels (before: "✓ Auflösen" overran its 56 px face),
   stepper cells `s1` apart (before 2 px), card stack gap `s2` (before 6). Sheets: annotation cards
   `s3` below the column-header band and `s3` left of the scrollbar lane (before: card over the
   header labels, 2 px into the rail), menu-row seam `s1` (before 2 px). Slides: fill-picker caption
   on its own measured row (before: caption line box overlapped the picker by ≈6 px). PNGs:
   `word-processor/after/dark/threaded-comments.png`, `sheets/after/{dark,light}/charts.png`,
   `slides/after/{dark,light}/inspector_widgets-narrow.png`.
2. **Every surface has a derived edge or contrast step** — all owned chips, cards, fields and rules
   route through `ui::surface::{card, edge_on, edge_on_with, separator, edge_outline}`; floating
   panels (sheets File/Format menus, tab menu, Document settings; WP comment thread, colour-picker
   popover, compact swatch palette; slides overflow menu, fill-picker popover, shortcuts dialog)
   through `ui::chrome::{panel_ground, panel_border, panel_shadow}`; scrims through
   `ui::chrome::fill_scrim`. `border_standard` remains only where it is a rule on `surfaces.base`
   (sheets body grid lines, WP paper content). Owned `Color::WHITE` chrome ink is now
   `Colors::on_accent` / `ink_on_any`; owned `neutral[..]` chrome slots are gone.
3. **One rhythm per screen** — per-app tables below; the off-scale seams that survived earlier
   rounds (`TOGGLE_SEAM 5`, `ACTION_SEAM 6`, `PANEL_ROW_SEAM 6`, `PANEL_STEP_SEAM 2`,
   `MENU_ROW_SEAM 2`, review card stack 6, thread panel 12/18/38/39/48/52/82/96, annotation card
   14/10) are token steps now.
4. **Centred on measured line boxes** — WP recovery-banner labels (before: `chars × 7.4`
   estimate), review card buttons, thread-panel title→root advance (measured title line box),
   slides fill-picker caption row (measured `lh_caption`). Everything else already centred on
   `centred_top` / `centred_line_top`.
5. **No authored-pixel exemption** — every fixed offset kept is a control height / radius
   (`PANEL_CONTROL_H`, `BUTTON_H`, `REVIEW_BTN`, radii) or a token expression; the ones that
   produced 1–4 were replaced (list in rule 3).
6. **Visual evidence** — before/after PNGs per surface below, dark and light, all opened.

## Surface tables (before → after)

### word-processor (`--shots` flags, 1280×800, 1×)

| surface (PNG) | before dark | before light | after dark | after light |
|---|---|---|---|---|
| toolbar + layout bar (`plain.png`) | zoom `−`/`+` and `Ränder`/`Abstände` chips invisible (`neutral[2]` on raised); layout-bar top rule invisible | chips grey, rule faint | chips: raised face + derived edge (`surface::card`); rule `edge_on(bar)` | chips white + hairline on white bar; rule visible |
| styles panel (`styles.png`) | every chip, stepper, value box invisible; panel left edge invisible; dividers faint | visible grey chips | all controls edged; value boxes `card(field)`; dividers `surface::separator`; rows `s2` apart, steppers `s1` | same, white faces with hairlines |
| image inspector (`image-object.png`) | Smaller/Larger/Horizontal/Vertical/Crop/Reset/Free/Left/Right invisible | visible | edged chips | edged chips |
| find bar (`find.png`) | `Aa W .*` toggles and `↑ ↓ Ersetzen Alle ×` bare text | grey chips | edged chips; toggles `s1` apart, actions `s2` (before 5 / 6) | same |
| comment thread panel (`threaded-comments.png`) | bar-toned card, `border_standard` stroke faint, reply bubbles invisible, fixed 12/18/38/39/48/52/82/96 | bubbles grey | Panel role (`panel_ground` + `panel_border` + `panel_shadow`), bubbles `card(chip)`, `s3`/`s2` rhythm, measured buttons | same |
| review cards (`comments.png`, `threaded-comments.png`) | comment card 2 px under the scrollbar thumb; "✓ Auflösen" overran its button | same | card column `s3` inside the canvas edge (clears the 8 px lane + `s1`); buttons = measured label + `s1` pad, centred | same |
| history strip (`history.png`) | shared `HistoryScrubber` (F4) + preview card already `lift(0.72)` + `hairline_on` | — | unchanged (not owned) | — |
| error toast (`error.png`) | shared `paint_transient` (F2) | — | unchanged | — |
| responsive matrix (phone/tablet/laptop/desktop) | two-row toolbar; chips invisible in dark | — | chips edged; nothing touches | — |

Not capturable by flag, fixed by code and covered by `cargo test -p word-processor --lib`:
recovery banner (`recovery_banner.rs`: `s3` inset, `s2` between buttons, measured labels,
`card(chip)`), compact swatch palette (`compact_swatch_palette.rs`: Panel role), outline pane
(`outline_pane.rs`: active row `card(chip)`), colour-picker popover (`style_color_picker.rs`:
Panel role, `s3` inset), toolbar swatch strips (neutral swatch `card(chip)`, selection ring
`text_primary` instead of `Color::WHITE`).

### sheets (19 scenarios + plain + matrix, 1280×800, 1×)

| surface (PNG) | before dark | before light | after dark | after light |
|---|---|---|---|---|
| menu bar chips (`plain.png`) | `border_standard` stroke ≈ invisible; active label `Color::WHITE` | visible | `surface::card(raised)`; AutoSum wash via `accent_chip_on`; active label `on_accent` | white chips + hairline |
| menu bar bottom rule / tabs strip top rule | invisible (`border_standard` stroke straddling the boundary) | faint | 1 px `edge_on(raised)` on the bar's own pixel row | visible |
| name box / formula field | faint edges | visible | `card(raised)` / `card(base)`; editing keeps the accent ring | same |
| header bands (`grid.rs`) | rules invisible | faint | 1 px `edge_on(raised)` on the band's last row / column | visible |
| scrollbar thumb | `border_standard` stroke | same | `card(thumb)` inset edge | same |
| sheet tabs | inactive tab: `raised` on `raised` (no-op fill) | same | selector: idle bare label (HIG §9.5), active `base` lift + accent underline; `+` button `card(base)` | same |
| File / Format menus (`file.png`, `format.png`) | `raised`+`overlay` fill, `border_standard`, literal black shadow; matching row = accent wash | edge visible | `panel_ground` + `panel_border` + `panel_shadow`; matching row `selection_tier(active)` (lift + hairline); hover `lift(0.5)` | same |
| Document settings (`docsettings.png`) | scrim `rgba(0,0,0,110)`, same panel recipe, chips `border_standard`, selected `accent/0.85` + stroke, label `Color::WHITE` | same | `fill_scrim(0.45)`, Panel role, quiet chips `card(raised)`, selected accent fill no edge, `on_accent` ink | same |
| tab menu (`tabmenu.png`) | same panel recipe; hover accent wash; none-swatch `border_standard`; selected ring `Color::WHITE` | same | Panel role; hover `lift(0.5)`; `card(base)`; ring `text_primary` | same |
| chart / comment cards (`charts.png`, `comments.png`) | over the column-header band (`y0+14`), 2 px into the scrollbar rail (`x1-14` vs 16 px lane), `border_standard` / accent frame, 10 px stack gap | same | `s3` below the header band, `s3` left of the lane, `s3` apart, `card(raised)`; accent kept for live labels only | same |
| status bubble / toast (`plain.png`, `error.png`, `conflict.png`) | shared `ui::status_bubble::draw_pill` (`border_standard` at `status_bubble.rs:164`) | — | unchanged — **Open, needs F4** | — |
| command palette (`commandpalette.png`) | shared `CommandPalette` (F2) | — | unchanged | — |

### slides (27 scenarios + plain + matrix at 1280×800, six scenarios at their own viewport)

| surface (PNG) | before dark | before light | after dark | after light |
|---|---|---|---|---|
| rail / inspector / toolbar dividers (`plain.png`) | `border_standard` 1 px stroke on integer x/y → 2 px blur, invisible on dark | faint | crisp `chrome::hairline_v/h` in `edge_on(raised)` / `edge_on(toolbar_fill)` on the panel's own pixel row | visible |
| toolbar / inspector buttons, segments, hamburger, palette pills | `base` fill + `text_primary/26..30` declared veneer; active ink `Color::WHITE` | same | `surface::card(base)`; active accent fill, ink `on_accent(pal)`; segments per-cell `edge_on(bg)` inset ½ px; danger = `tinted_chip_on` wash + derived edge | same |
| swatches | `Color::BLACK/50` frame | same | `card(col)` — edge derived per swatch | same |
| speaker notes box (`plain.png`, `notes_edit.png`) | `text_primary/26` | same | `card(base)`; editing: accent wash + ring on `edge_outline` | same |
| overflow menu (`toolbar_menu-narrow.png`) | raised + `text_primary/48` + literal black shadow | same | `chrome::panel` (panel ground/border/shadow) | same |
| fill-picker popover (`inspector_widgets-narrow.png`) | same recipe; caption at `panel.y0+8` overlapping the picker's top edge | same | `chrome::panel`; caption on its own `s2 + lh_caption + s2` row; picker inset `s3` | same |
| shortcuts dialog (`help.png`) | `Color::BLACK/150` wash + raised card + `text_primary/30` | same | `fill_scrim(0.6)` + `chrome::panel` | same |
| overview grid (`overview.png`, `overview_end.png`) | header divider `border_standard` stroke (blur); thumb frames `text_primary/30` | same | crisp `edge_on(base)` rule; frames `edge_on(base)`; current keeps accent ring | same |
| presenter / notes views (`presenter.png`, `present_notes.png`) | notes panel top rule `border_standard`; frames `text_primary/22..30`, `Color::WHITE/40` | — | rule `edge_on(raised)`; frames `edge_on(STAGE)`; panels `card(raised)` | — |
| element menu / slide menu (`element_menu.png`, `slide_menu.png`) | shared `ContextMenu` (F2) / `PageSidebar` menu (F3) | — | unchanged | — |
| responsive matrix | fine | — | fine | — |

## Pair tables (1×; gap px before → after; token step)

### word-processor — styles panel (`styles.png`)

| element A | element B | axis | before → after | step |
|---|---|---|---|---|
| panel edge | content column | x | 14 → 16 | `s4` (`PANEL_PAD`, Panel section padding) |
| control row | next control row | y | 6 → 8 | `s2` (`PANEL_ROW_SEAM`) |
| `L` `C` `R` `J` cells | each other | x | 6 → 8 | `s2` |
| stepper `−` | value box | x | 2 → 4 | `s1` (`PANEL_STEP_SEAM`) |
| value box | stepper `+` | x | 2 → 4 | `s1` |
| swatch | swatch | x | 4 → 4 | `s1` |
| swatch row | custom chip row | y | 8 → 8 | `s2` |
| divider | next section | y | 12 → 12 | `s3` |
| caption | first control | y | `caption_adv` = lh + 4 → same | `s1` |

### word-processor — comment thread panel (`threaded-comments.png`)

| element A | element B | axis | before → after | step |
|---|---|---|---|---|
| panel | window right edge | x | 12 → 12 | `s3` |
| panel | layout bar | y | 6 → 8 | `s2` |
| panel edge | content | x/y | 12 → 12 | `s3` |
| title | root comment | y | 39 − (12 + lh) ≈ 10 → 8 | `s2` (measured title box) |
| root comment | first reply bubble | y | 9 → 12 | `s3` |
| bubble | bubble | y | 5 → 8 | `s2` |
| bubble text | bubble edge | x / y | 7 / 4 → 8 / 4 | `s2` / `s1` |
| last bubble | reply field | y | ≥ 14 → ≥ 12 | `s3` |
| reply field | action row | y | 12 → 12 | `s3` |
| Abbrechen | Senden | x | 8 → 8 | `s2` |

### word-processor — find bar, layout bar, review cards

| surface | element A | element B | axis | before → after | step |
|---|---|---|---|---|---|
| find bar | bar top | field row | y | 8 → 8 | `s2` |
| find bar | find field | toggles | x | 8 → 8 | `s2` |
| find bar | toggle | toggle | x | 5 → 4 | `s1` (segmented trio) |
| find bar | action | action | x | 6 → 8 | `s2` |
| layout bar | bar left | first chip | x | 12 → 12 | `s3` |
| layout bar | chip | chip | x | 8 → 8 | `s2` |
| layout bar | chip | bar top / bottom | y | 4 / 4 → 4 / 4 | `s1` |
| review | comment card | canvas right edge | x | 6 (−2 vs thumb) → 12 | `s3` |
| review | change card | comment card | x | 6 → 8 | `s2` |
| review | card | next card | y | 6 → 8 | `s2` |
| review | Reply button | Resolve button | x | ≥ 8 → ≥ 8 (measured widths) | — |

### sheets

| surface | element A | element B | axis | before → after | step |
|---|---|---|---|---|---|
| menu bar | button | button | x | 8 → 8 | `s2` |
| menu bar | bar edge | first button | x | 8 → 8 | `s2` |
| formula bar | field | bar top / bottom | y | 4 → 4 | `s1` |
| formula bar | name box | fx slot | x | 0 (fx slot is padding) → same | fx slot = glyph + 2·`s2` |
| tabs strip | tab | tab | x | 4 → 4 | `s1` |
| tabs strip | `+` | last tab | x | 4 → 4 | `s1` |
| menus | row lift | row lift | y | 2 → 4 | `s1` (`MENU_ROW_SEAM`; hit rects stay contiguous, only the painted lift is inset) |
| menus | panel edge | rows | x/y | 8 → 8 | `s2` |
| menus | anchor button | panel | y | 4 → 4 | `s1` |
| settings | panel edge | title | x/y | 24 → 24 | `s6` |
| settings | chip | chip | x/y | 8 → 8 | `s2` |
| settings | section | section | y | 16 → 16 | `s4` |
| settings | last row | footer | y | 24 → 24 | `s6` |
| cards | column-header band | card top | y | −12 (over the band) → 12 | `s3` |
| cards | card right | scrollbar lane | x | −2 (over the rail) → 12 | `s3` |
| cards | card | card | y | 10 → 12 | `s3` |
| cards | card edge | content | x | 12 / 14 → 12 | `s3` |
| comment card | row-header band | card left | x | −42 (over the band) → 12 | `s3` |

### slides

| surface | element A | element B | axis | before → after | step |
|---|---|---|---|---|---|
| fill picker | caption line box | picker top | y | −6 (overlap) → 8 | `s2` |
| fill picker | panel edge | caption | y | 8 → 8 | `s2` |
| fill picker | panel edge | picker | x / bottom | 12 / 16 → 12 / 12 | `s3` |
| overflow menu | item | item | y | 8 → 8 | `s2` (`GAP`) |
| overflow menu | panel edge | items | x/y | 8 → 8 | `s2` |
| inspector | panel edge | content | x | 16 → 16 | `INSPECTOR_PAD` = `s4` |
| inspector | swatch | swatch | x/y | `s2` → `s2` | `s2` |
| inspector | row | row | y | `s2`/`s3` (measured `ChromeMetrics`) → same | `s2`/`s3` |
| toolbar | pill | pill | x | `s2` → `s2` | `s2` |
| panels | divider | neighbour | — | 1 px stroke straddling 2 px → 1 px on the panel's own row/column | crisp |

## Lint

`--headless-drive` at Unit 0 rejects a `"scenario"` field (`unknown field "scenario"; expected
viewport, actions, audit, audit_report, dump, targets, or inspect`), so only the plain state of each
app could be inspected. All three apps are single hand-painted widgets whose inspection tree holds
one `Document` node (`measured_controls=1, peer_pairs=0, passed=True`, dark and light, all three
apps: `target/ux1x/a7/<app>/after/<theme>/lint/plain.audit.json`); the lint therefore has nothing
to measure here and the pair tables above are the machine-readable substitute (geometry read from
the layout code; PNGs confirm).

## Ratchets

`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit` (final run,
`target/ux1x/a7/tests/ui-ratchets-3.log`): 7 passed, 2 failed — neither failure is a `grew` line
in an owned file.

```
test design_token_audit::declared_borders_in_chrome_do_not_grow ... ok
test design_token_audit::theme_escape_hatches_do_not_grow ... ok
test design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow ... FAILED
  apps/browser/src/browser_tabs.rs [literal_color]: 1 occurrence(s), baseline 0     (pre-existing, not A7)
  apps/browser/src/browser_tabs.rs [white_or_black]: 1 occurrence(s), baseline 0    (pre-existing, not A7)
  shell/src/shell/frame.rs [literal_color]: 1 occurrence(s), baseline 0             (pre-existing, not A7)
  shell/src/shell/frame.rs [white_or_black]: 2 occurrence(s), baseline 0            (pre-existing, not A7)
test f1_spacing_token_audit::spacing_literals_do_not_grow ... FAILED
  apps/word-processor/src/find_bar.rs [pixel_const]: now 7, baseline still 9 — lower it to 7   (stale: A7 reduction)
```

`stale` for the coordinator's re-baseline: `apps/word-processor/src/find_bar.rs [pixel_const]`
9 → 7 (`TOGGLE_SEAM` / `ACTION_SEAM` are token expressions now). Owned-file counts that went
*down* without tripping the stale check: `literal_color` in `apps/sheets/src/app.rs` (three
`Color::from_rgba8(0,0,0,..)` shadows gone), `white_or_black` in `apps/sheets/src/app.rs`,
`apps/slides/src/app.rs`, `apps/slides/src/help_overlay.rs`, `apps/slides/src/toolbar_tools.rs`,
`apps/slides/src/presentation.rs`, `apps/word-processor/src/widget.rs`; `declared_border` is
scoped to `ui`/`shell` so the ~30 `border_standard` sites removed here are not on that ratchet.

## Tests

All with the inline env and `--no-default-features --locked`; logs under `target/ux1x/a7/tests/`.

| command | result |
|---|---|
| `cargo check -p word-processor -p sheets -p slides --tests` | clean (only the two pre-existing `ui` warnings) |
| `cargo build -p word-processor -p sheets -p slides` | ok |
| `LC_ALL=en_US.UTF-8 cargo test -p word-processor --tests --no-fail-fast` | lib 329 passed / 0 failed / 1 ignored; main 2 passed; `tests/it.rs` 14 passed |
| `cargo test -p word-processor --lib` (system locale, no hunspell dictionary) | 324 passed / 5 failed — the five spellcheck tests listed under Limits; identical at the base |
| `cargo test -p sheets --lib` | 244 passed / 0 failed / 1 ignored |
| `cargo test -p slides --lib` | 171 passed / 1 failed — `app::tests::notes_typing_writes_through_and_up_down_move_the_caret` ("ArrowUp moved the notes caret onto the first shaped line"): the known pre-existing notes-caret failure named in the unit brief; caret navigation in the shared multiline `TextInput`, untouched here |
| `cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit` | 7 passed / 2 failed, see Ratchets (no `grew`) |

Both red rows were verified red at the base `fd1f84391` with this branch's sources swapped for
the base ones (`target/ux1x/a7/tests/base-slides-notes.log`: 0 passed / 1 failed;
`base-wp-spell.log`: 0 passed / 5 failed under the same system locale), then the branch sources
restored (`git status --short` empty).

Tests updated with the new values (never weakened): `word-processor/widget.rs`
`chrome_colors_are_sourced_from_theme_tokens` (border = `edge_on(bar)`, chip = `raised`, edge Δ ≥
`HAIRLINE_MIN_DELTA`, scrollbar thumb ≥ `NON_TEXT_MIN_CONTRAST`); `widget/r2_chrome_metrics.rs`
1× row advance 30 → 32 (`PANEL_CONTROL_H + s2`); `sheets/app/r2_annotation_cards.rs` card top =
`grid_rect.y0 + col_header_h() + s3` (computed independently of the painter's helper).

## Code review (`code-review` skill, high)

Ten findings; the six regressions my own diff introduced were fixed before commit:

1. slides fill-picker dismiss hit-test still used `get_rect().inflate(12, 16)` while the painted
   panel grew a caption row → one `fill_picker_panel_rect` now feeds both the painter and the
   pointer test; `fill_picker_popover_rect` clamps the whole panel `s1` inside the body.
2. WP review-card Reply / Resolve faces could overlap at larger text scales → Resolve is placed
   first (trailing), Reply is bounded at `resolve.x0 − s2` and elided; labels measured once per
   frame (`CardButtonLabels`), which also closes the per-card `t()` + `measure` finding.
3. WP recovery buttons took `metrics.bar_control_height` inside the fixed 40 px band → clamped to
   `bar.height() − 2·s1`.
4. WP canvas scrollbar thumb at `edge_on_with(base, 92)` measured ≈2.5:1 on the light desk →
   `ink_on(base, text_dim, NON_TEXT_MIN_CONTRAST)`; the test asserts the 3:1 floor, not a
   hairline delta.
5. sheets `MENU_ROW_SEAM` was subtracted from the rows' hit rects → hit rects are the full row,
   only the painted lift is inset `seam/2`.
6. sheets annotation test derived its expectation from the production helper → now computed
   independently from `grid_rect.y0 + col_header_h() + s3`.

Two are recorded rather than fixed here: the floating-panel recipe (`panel_shadow` +
`panel_ground` + `panel_border`) is composed once per app (`SheetsApp::paint_panel`,
`slides::chrome::panel`, `word_processor::widget::paint_panel`) because the composition belongs
in `ui::chrome` (not an A-unit file); and the chip painters are one-call wrappers over
`ui::surface::card` rather than `Button::paint`, because `face_colors`'s Neutral rest edge at this
base is still `border_standard` (F1's fix) — routing through it today would re-hide every chip in
dark. Both are listed below.

## Open — needs another unit

- **Unit 0 / F2 `ui::chrome::panel(scene, transform, rect, radius, theme)`** — the shadow +
  ground + border composition is written three times in this diff (one per app); the toolkit
  exposes the parts, not the whole.
- **F1 `Button::paint` rest edge** — once `face_colors` gives Neutral an `edge_on(raised)` rest
  edge, `word_processor::widget::chip_face`, `SheetsApp::draw_chip` / `draw_setting_chips`, the
  find-bar toggles, the recovery buttons and slides `draw_button_*` should call `Button::paint`
  with a `ButtonFace` instead of `ui::surface::card`; the swap is mechanical (same rect, same
  label, same state split).
- **F1 `ColorPicker`** — its hex / rgb readouts paint past `PICKER_WIDTH` (260): in
  `slides/after/{dark,light}/inspector_widgets-narrow.png` the `rgb(…)` line runs out of the
  popover onto the inspector's Justify button, exactly as the A7 round-two note recorded
  (`ui/src/widget/color_picker.rs`, `apps/slides/src/app.rs:fill_picker_popover_rect`). The popover
  frame is not widened to hide it.
- **F4 `ui::status_bubble::draw_pill`** — `border_standard` stroke at `ui/src/status_bubble.rs:164`
  on the sheets status pill / toasts (`sheets/after/dark/plain.png`, bottom right).
- **F2 `ContextMenu`** (slides `element_menu.png`, WP style menu / template picker) and **F3
  `PageSidebar` menu** (slides `slide_menu.png`), **F3 `ui::chrome::toolbar`** (WP toolbar bottom
  hairline) — shared widgets, unchanged here; their AFTER PNGs show the Unit 0 state.

## Limits

- BEFORE for the six slides viewport-specific scenarios exists only at 1280×800 (the menu /
  popover need the narrow viewport, which I only captured AFTER; the base binary was gone by then).
- No BEFORE lint run (the binaries had been rebuilt); the lint is uninformative for these apps
  anyway (one semantic node).
- WP review cards keep their authored paper-relative colours (`COMMENT_CARD_*`, `CHANGE_CARD_*`):
  they are document annotations next to the always-light paper, not chrome; only their geometry
  (scrollbar clearance, measured buttons, `s2` stack) changed.
- The isolated `HOME` renders the word-processor in German (system locale fallback); the geometry
  is the same.
- `cargo test -p word-processor --lib` under this Mac's system locale fails five spellcheck tests
  (`misspelled_word_yields_a_squiggle_span_at_its_x_range`, …): `spellcheck::default_language()`
  follows `LANG` (German here) and there is no hunspell dictionary installed, so nothing is
  flagged. The same binary passes 329/329 under `LC_ALL=en_US.UTF-8`; none of these tests touch
  the chrome this unit changed.
- Panel / button recipes are composed per app (see Code review) until `ui::chrome::panel` and
  the F1 rest edge exist.
