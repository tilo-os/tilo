# A4 — communication apps (feeds, chat, mail, totp): 1× dark + light polish

Branch `ux1x/a4-communication`, cut from Unit 0 `fd1f84391`. Files edited: `apps/{feeds,chat,mail,totp}/**` and this report only.

The unit was interrupted by a machine reboot mid-way and resumed from its checkpoint (16 modified files, report draft, captures). The resume re-read the whole diff, ran the code review, acted on its findings (below), rebuilt, and **re-captured every AFTER surface against the final binaries**; the checkpoint's AFTER set is kept as `after-prev/` for comparison, nothing was discarded.

Evidence lives under `target/ux1x/a4/<app>/{before,after,after-prev}/{dark,light}/` — every declared `--shots` scenario at 1280×800 plus the responsive matrix (phone / tablet / laptop / desktop), an isolated `HOME` per app, `QOS_SHOTS_TEXT_SCALE=1.0`. Geometry: `after/<theme>/lint/<scenario>.{inspection,targets,audit}.json` from `--headless-drive` + `scripts/ux-bench/usability.py::geometry_audit`. BEFORE = `fd1f84391` binaries; AFTER = this branch at its final commit (`recapture-*.sh`, `finalize2.sh`, `finalize2.log`). Every AFTER PNG named below was opened with the Read tool after the final capture.

## Six-rule checklist

| # | Rule | Evidence |
|---|---|---|
| 1 | No two elements touch or overlap | Pair tables below; `geometry_audit` reports **0** `touching_peers` / `semantic_bounds_overlap_unresolved` / `target_size_24` for every feeds (live + 10 scenarios × 2 themes), mail (19 × 2), totp (8 × 2) inspection and the chat live root (× 2) — 78 inspections, `finalize2.log`. Fixed painted touches: feeds time-label ↔ star (−4 px → +4), totp banner ↔ Retry (0 → 8), mail compose toolbar glyphs (2 → 4) and grip ↔ Discard (0 → 4), chat filmstrip caption ↔ name plate (0.2 → ~16), chat category toggle ↔ `+` (0 → 4). |
| 2 | Every surface has a visible derived edge or contrast step | Every app-painted card / row / pill / chip goes through `ui::surface::card` (fill + `edge_on(fill)` inset 1 px), `ui::surface::pill` (semantic borders: focus / danger), `ui::surface::separator` / `edge_on` (rules) or `ui::chrome::panel_border` (floating panels: totp add sheet, feeds popover, mail compose window / setup sheet / context menu, chat huddle / call bars / emoji panel / attach picker / message menu). Every app-painted **button** is now the shared face — `Button::paint` / `face_colors` (Neutral, Primary, DangerOutline, disabled) — and totp's unsaved-vault banner is the shared `Banner::paint` (HIG §8), so their rest edges are the toolkit's one rule. Floating shadows come from the elevation ladder (`ui::surface::shadow`, `e1`–`e4`) instead of solid offset silhouettes that read as grey rings on light. `border_standard` is gone from all four apps (`grep -rn border_standard apps/{feeds,chat,mail,totp}/src` → nothing). mail's private `fill_round`/`stroke_round` (`draw.rs`, `compose.rs`) and totp's private `tint_over` are deleted; chat keeps `paint.rs::{fill_round,stroke_round,legible_on,band_at_least}` for plain fills and contrast steps and routes every edge through `ui::surface`. |
| 3 | One rhythm per screen | totp list: cards `s3`, list pad `s2`, banner `s2`/`s4`, header buttons `s3`; totp add sheet: cards `s3`, field rows `s3`, chips `s2`, footer `s6`; mail compose: header rows `s2`, toolbar glyphs `s1` + group `s3`, footer/grip `s1`; mail setup: pills `s2`, rows `s3`; mail reader chips `s3`; chat huddle controls `s2`, reaction chips `s2`, filmstrip block gaps `s2`; feeds trailing column `s1`, popover segments `s2`, row separators inset `s3`. All spelled as `Spacing::BASE.sN` / `theme.tokens.spacing.sN`; the `f1_spacing_token_audit` line for `compose.rs`, `add_panel.rs`, `list.rs` went **down** (stale lines pasted below). |
| 4 | Content centred on measured line boxes | feeds `#`, stepper glyphs and toolbar glyphs centred by `Button::paint` / the face's ink; feeds trailing column (time over star) centred on the row (`list_layout::trailing_column`, tests; its 1× line box asserted equal to `ui::text_metrics::line_height_px(12.0, 1.0)`); chat avatar + caption block centred in the band above the name plate (`render_call::avatar_caption_block`, tests; plate height from `text_metrics::line_height_px`); totp banner title centred by `Banner::paint`; totp Edit/Add/Retry and every chat/mail/feeds button label centred by the shared face painter. |
| 5 | No authored-pixel exemption | Replaced: totp `ROW_GAP 10`, `HEADER_BTN_GAP 10`, `CARD_GAP 10`, `ROW_GAP 14`, `FOOTER_GAP 18`, chip gaps `6`, banner `+8/+40`, Retry `+23`; mail `HEADER_ROW_GAP 6`, toolbar `+2/+6`, grip `x1-14..x1-4`, pill gap `10`, chip gap `10`; chat huddle gap `6`, reaction gap `6`, caption `cy + r + 8`, chip min-h `22`, shadow silhouettes `inflate(5|6)`; feeds `alpha(border_standard, N)` washes, `y0 + 7.0` / `x - 4.5` glyph offsets. Kept fixed offsets are token expressions or measured. |
| 6 | Visual evidence | Surface table below; every AFTER PNG listed was opened after the final capture and compared to its BEFORE in dark and light. |

## Surface table (BEFORE → AFTER, dark and light)

PNG names are relative to `target/ux1x/a4/<app>/<phase>/<theme>/`.

### totp (scenarios added: `list add add-advanced add-picker empty provision persistence-error missing-secret`; `--screen` still accepted)

| PNG | BEFORE defects (rules) | AFTER |
|---|---|---|
| `list.png` (was `default`) dark+light | (2) account cards a 55 % raised wash with no edge — five rows read as one block; (3) card gap 10 px, first card 5 px under the header rule; (2) Edit button `border_standard` on `raised` invisible, Add label literal white; (2) header rule `border_standard@0.5` invisible; (2) ring track `border_standard` orphaned the arc on dark | `ui::surface::card(raised)` per row, `bg_hover`/`bg_active` states; `s3` between cards, `s2` list pad; Edit/Add are `Button::paint` faces; header rule `edge_on(base)`; ring track `edge_on_with(raised, PANEL_BORDER_ALPHA)` |
| `persistence-error.png` dark+light | (1) banner and Retry share an edge; (2) banner a 14 % translucent wash, no edge, hand-painted danger text; (3) banner → first card 21 px; Retry hand-painted | banner is the shared `Banner::paint(BannerKind::Error)` — semantic tint, edge, leading bar and icon from the toolkit — stopping `s2` short of Retry, which is a primary `ButtonFace` of the same height (the Banner's 44 px 1× band); banner `s2` under the rule, first card `s4` under the banner |
| `add.png`, `provision.png`, `add-picker.png` dark+light | (2) modal `border_standard` on `raised` invisible; (2) "Paste a setup link" card `border_standard` on `base` on `raised`, invisible on dark; (3) card gap 10, field rows 14, footer 18; (5) primary card ink literal white; (2) divider `border_standard@0.6` | modal `panel_border` on `edge_outline`; method card `ui::surface::card(base)`, accent card `on_surface(accent).ink`; cards `s3`, rows `s3`, footer `s6`; divider `edge_on(raised)`, caption gap `s2` |
| `add-advanced.png` dark+light | as `add`; (3) algorithm/digit chips 6 apart, algorithm row → digits 8 | chips `s2`, rows `s3`; unselected segment track still bare — shared `ToggleGroup` pill face (Open, coordinator) |
| `empty.png`, `missing-secret.png` dark+light | header defects as `list` | as `list` |

### feeds

| PNG | BEFORE defects | AFTER |
|---|---|---|
| `default.png`/`detail.png`/`sidebar-open.png`/`post-import.png`/`long-titles.png` dark+light | (1) relative-time label overlapped the star glyph by ~4 px on one-line titles; (2) toolbar buttons `neutral[2]@160` + `border_standard@120` — five glyphs with no control shape on dark; (2) toolbar/list/sidebar rules `border_standard@120`, row rules `@70` (27 % ghost); (2) `#`/gear header buttons and hamburger/close/back chrome buttons `border_standard@150`; (2) "Atom" kind tag and `#rust` chips bare `code_bg`; (2) dark reader palette `page_edge`/`rule` = `border_standard`; (4) `#` glyph at `x−4.5, y0+7` | trailing column = time over star, `s1` apart, centred on the row (`list_layout::trailing_column`, star hit rect built on the same centre); toolbar buttons, `#`/gear and chrome buttons are the shared `Button::paint` / `face_colors` Neutral face (gear Primary while its popover shows), glyphs in the face's ink; every rule `edge_on(base|neutral[0])`, row rules `separator(.., s3, base)`; tag/chips `card(code_bg)`; dark palette edges `edge_on(raised)` |
| `reader-settings.png` dark+light | (2) popover `border_standard@180`; (2) steppers `neutral[1]` + `border_standard@140`; (2)(8) hand-rolled segmented cells with `border_standard@120`, 4 px apart; (5) glyph offsets `y0+3`, `y0+6` | popover `panel_border`; steppers `Button::paint` Neutral faces with the glyph as label; Font/Theme rows are the shared `ToggleGroup::paint(SegmentedFace::pill)` with `s2` gaps (unselected track bare — Open, coordinator) |
| `refresh.png` dark+light | (2) strip rule `border_standard@120` | `edge_on(base)` |
| `add-subscription.png`, `tags.png`, `context-menu.png` dark+light | shared `ModalDialog` / `ContextMenu` frames — dark modal edge faint, Cancel a bare Neutral button | unchanged here (Open, coordinator: F2/F1 landed on integration); no app-owned defect |
| `empty.png` dark+light | rules as above | as above |

### mail

| PNG | BEFORE defects | AFTER |
|---|---|---|
| `wide.png`/`narrow*.png`/`mid.png`/`search.png`/`preview.png`/`loading.png`/`offline.png`/`empty.png` dark+light | (2) active account card a 12 % accent wash, no edge; (2) folder selection and selected list row accent washes (§9.3); (2) search box / refresh `border_standard`; (2) row rule `neutral[2]` full to the right edge; (2) reader heading rule `border_standard` on `raised`; reader action bar / attachment chip / remote toggle hairline-on-`raised` regardless of fill | account card `card(bg_active)`, folders and the selected list row `bg_active` lift + accent bar; search `card(raised)` (focus keeps `border_focus`); refresh and the action bar are `Button::paint` faces (Delete = `DangerOutline`, glyphs in the face's ink; starred star keeps its warning fill); row rules `separator(.., s3, base)`; heading rule `edge_on(raised)`; chips / toggle `card(fill)`; attachment chips `s3` apart |
| `html_mail.png` dark+light | (2) fallback body `base` + `border_standard` | `card(base)` |
| `compose.png`/`compose_narrow.png` dark+light | (1) toolbar glyph squares 2 px apart; (1) resize grip shares Discard's corner; (2) `Cc/Bcc`/`Attach` chips and toolbar squares bare `lift()`; (2) title-bar rule and field underlines `border_standard`; (3) header rows 6 px; window shadow a solid rect nudged 2/4 px (a grey bar on light) | glyphs `s1` apart, group gap `s3`; grip inside the padding ring `s1` clear of Discard; chips/squares `card(lift)`, active states accent (no edge); rules `edge_on(base|raised)`; header rows `s2`; window `panel_border` + `ui::surface::shadow(e3)`, prompt panel `panel_border`/danger |
| `setup*.png`/`error.png`/`firstrun.png` dark+light | (3) sign-in pills 10 px apart; sheet edge hairline-on-`raised`; sheet shadow a solid offset rect | pills `s2`; sheet + context menu `panel_border`, shadows `ui::surface::shadow(e4 / e2)`; unselected pill track bare — shared `ToggleGroup` (Open, coordinator) |

### chat

| PNG | BEFORE defects | AFTER |
|---|---|---|
| `default.png`/`compact.png`/`narrow.png`/`starter.png`/`collaboration.png`/`edit.png`/`composer.png` dark+light | (2) rail tiles bare `neutral[2]`; (2) rail/sidebar/header/date/thread rules `border_standard` (invisible on dark); (2) presence pill, `+ Category`, header `6`/`Call` pills, reaction chips bare `lift()` fills; composer `+`/send and editor Cancel hand-painted; (2) composer rest edge `border_standard`; (1) category toggle hit shares an edge with `+`; `+ Category` 22 px tall (< pointer target); (3) reaction chips 6 apart | every chip/pill `card(fill)`; composer `+`, send (Primary / disabled) and editor Cancel are `Button::paint` faces; accent states keep no edge; rules `edge_on(base|raised|neutral[0])` or `separator(.., s3, ..)`; composer `edge_on(neutral[2])`; toggle stops `s1` before `+`; chip ≥ `metrics.min_pointer_target`; chips `s2` |
| `call.png`/`share.png`/`floating-call*.png` dark+light | (1) filmstrip "Camera off"/"No video" caption baseline on the name plate's top edge (0.2 px); (2) control buttons bare `lift(0.72)`, hand-painted danger/disabled faces; light: muted name in danger red on the 59 % black plate ≈2:1; huddle/overlay panel shadows solid 5–6 px silhouettes (grey rings on light) | avatar + caption one block centred in the band above the plate (`avatar_caption_block`, ≥ `s2` clear; plate height from `text_metrics::line_height_px`); controls are `Button::paint` faces (Neutral / Primary while on / `DangerOutline` for Leave / toolkit disabled face); plate ink `legible_on(tone, scrim-over-tile, 4.5)`; bars/panels `panel_border` + `ui::surface::shadow(e1 / e2)`; huddle controls `s2` |
| `people.png`/`thread-narrow.png`/`context.png`/`msgmenu.png` dark+light | (2) flap leading edge and header rule `border_standard`; (2) close `×` / `← Back` bare; members reaction chips bare; dead zero-length `hline`; message menu hairline declared against `raised` on a `neutral[1]` body, solid offset shadow | flap edge `edge_on(raised)` on the first pixel column, rules `separator`, close/back `Button::paint` faces, chips `card`; dead call removed; message menu `panel_border` on `edge_outline` + `ui::surface::shadow(e2)` |
| `attach*.png` dark+light | (2) picker header rule `border_standard`; panel hairline declared against `raised` while filled `neutral[1]`; solid offset shadow | `separator(.., s3, neutral[1])`, `panel_border`, `ui::surface::shadow(e2)` |

## Pair tables (gap px BEFORE → AFTER, relationship / step)

### totp `list.png`, `persistence-error.png`
| element A | element B | axis | before → after | step |
|---|---|---|---|---|
| header rule | first card | y | 5 → 8 | list pad `s2` |
| card | card | y | 10 → 12 | siblings `s3` |
| Edit | Add | x | 10 → 12 | siblings `s3` |
| header rule | banner | y | 8 → 8 | `s2` |
| banner | Retry | x | 0 → 8 | related `s2` |
| banner | first card | y | 21 → 16 | section `s4` |
| code | ring | x | 16 → 16 | `s4` (unchanged) |

### totp `add*.png`
| element A | element B | axis | before → after | step |
|---|---|---|---|---|
| method card | method card | y | 10 → 12 | siblings `s3` |
| field | next label band | y | 14 → 12 | siblings `s3` |
| divider rule | caption | x | 10 → 8 | `s2` |
| algorithm chip | chip | x | 6 → 8 | `s2` |
| algorithm row | digits label band | y | 8 → 12 | `s3` |
| form | footer buttons | y | 18 → 24 | next group `s6` |
| Cancel | Add account | x | 12 → 12 | `s3` |

### feeds `default.png` etc.
| element A | element B | axis | before → after | step |
|---|---|---|---|---|
| time label box | star glyph box (one-line title) | y | −4 (overlap) → 4 | icon ↔ label `s1` |
| toolbar button | button | x | 8 → 8 | `s2` |
| `#` | gear | x | 8 → 8 | `s2` |
| list row a11y bounds | next row | y | 0 → 1 (rule pixel) | rows contiguous by design; separator inset `s3` |
| sidebar row a11y bounds (pill) | next row | y | 0 → 4 | `s1` |
| popover segment | segment | x | 4 → 8 | `s2` |

### mail `compose.png`, `setup.png`, `wide.png`
| element A | element B | axis | before → after | step |
|---|---|---|---|---|
| toolbar glyph | glyph | x | 2 → 4 | `s1` |
| U | H1 (group gap) | x | 8 → 12 | `s3` |
| To row | Subject row | y | 6 → 8 | related `s2` |
| Discard | resize grip | x | 0 → 4 | `s1` |
| Password pill | OAuth pill | x | 10 → 8 | `s2` |
| action-bar button | button | x | 8 → 8 | `s2` |
| attachment chip | chip | x | 10 → 12 | `s3` |
| field row | field row (setup) | y | 12 → 12 | `s3` |

### chat `share.png`, `call.png`, `default.png`
| element A | element B | axis | before → after | step |
|---|---|---|---|---|
| tile caption | name plate | y | 0.2 → ≥ 8 (16 at 1×) | `s2` reserve, block centred |
| huddle control | control | x | 6 → 8 | `s2` |
| category toggle hit | `+` hit | x | 0 → 4 | `s1` |
| reaction chip | chip | x | 6 → 8 | `s2` |
| members pill | Call pill | x | 8 → 8 | `s2` |
| composer `+` | text band | x | 12 → 12 | `s3` |

## Geometry lint (AFTER, final binaries, `usability.geometry_audit` — `target/ux1x/a4/finalize2.log`)

- totp: 8 scenarios × dark/light — `failures=none` for all 16 (7–14 controls each).
- feeds: live + 10 scenarios × dark/light — `failures=none` for all 22 (up to 47 controls / 1073 peer pairs). BEFORE-state binaries reported 40–43 `touching_peers` per scenario: contiguous sidebar/list row bands and the pane-divider column; rows now report the painted pill (`sidebar_pill`) / the band less the rule pixel and divider column (`list_layout::row_semantic_bounds`) while their pointer hit rects stay full-band.
- mail: 19 scenarios × dark/light — `failures=none`; the list/reader panes expose 0 actionable nodes to the audit (their `Pane` semantics are not in the audit's role set), compose/setup expose 7–18.
- chat: live root × dark/light — `failures=none` (13 controls / 78 pairs) after: `+ Category` 22 px → `min_pointer_target`, toggle ↔ `+` gap, composer nodes report the `TextInput` text band (buttons sit beside it). Chat has no `shot_root`, so `--headless-drive --scenario` drives the live (empty) root only — scenarios are PNG-only for chat.

## Code review (medium) — acted on

Findings that concerned code this diff wrote, fixed before commit:

- `apps/feeds/src/app.rs`: the new `sidebar_pill` fn had been inserted between `#[allow(clippy::too_many_lines, reason = ..)]` and `draw_sidebar`, binding the allow to the wrong fn — moved above the attribute.
- Private helpers duplicating the toolkit: totp `tint_over` = `ui::tokens::colors::over` (deleted; the banner then became `Banner::paint`); mail `fill_round`/`stroke_round` (deleted, `ui::surface::{pill,card}`).
- Hand-rolled buttons → the shared face (HIG §8): `Button::paint` at feeds toolbar (`app.rs:3958`), `#`/gear (`2422`, `2441`; gear Primary while open, glyph in `face.colors(theme).2`), steppers (`2637`); `face_colors` for the renderer-less chrome button (`3996`); chat call controls (`render_call.rs:1406`, Neutral / Primary / `DangerOutline` / disabled), editor Cancel (`render_chat.rs:2380`), composer `+` (`3048`), send (`3071`, Primary / disabled), thread close (`3242`), members close (`3559`); mail refresh (`pane.rs:1096`), action bar (`1508`, Delete = `DangerOutline`).
- Hand-rolled banner → `Banner::paint(BannerKind::Error)` (`apps/totp/src/app.rs:1224`).
- `tr.measure("Ag", 12.0, ..)` line-box probes → `ui::text_metrics::line_height_px(12.0, theme.text_scale)` (chat `plate_height`, feeds `list_trailing.time_h`; new test `list_layout::one_x_time_line_is_the_toolkit_line_height`).
- Solid offset "shadows" → `ui::surface::shadow(.., &theme.tokens.elevation.eN)`: chat huddle (`render_call.rs:361`, `e1`), emoji picker (`1448`) and control sheet (`1622`), message menu (`render_chat.rs:1738`), attach picker (`3683`) — `e2`; mail context menu (`app.rs:2015`, `e2`), setup sheet (`2103`, `e4`), compose window (`compose.rs:2018`, `e3`).
- Chat message context menu stroked `hairline_on(raised, 26)` over a `neutral[1]` body → `panel_border` on `edge_outline` (`render_chat.rs:1743`).
- Mail selected list row was an `accent[5]@0.16` wash → `theme.bg_active` lift with the accent kept to the leading bar (`pane.rs`, §9.3 / §9.4).
- Unused imports left behind by the above (`Color`, `ColorCompat`, `RoundedRect`) removed; `draw_toolbar` takes `&mut TextRenderer` for the face painter.

Not acted on (coordinator's instruction): the "floating Panel idiom repeated" finding — `ui::surface::panel` landed on `ux1x/integration` (`1541e2fd3`); the 12 sites stay as fill + `panel_border` on `edge_outline` here (correct output) and are listed under Open.

## Ratchets (`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`)

Final run: `target/ux1x/a4/tests-ratchet-final2.log` — `7 passed; 2 failed`, both failures pre-existing growth outside this unit plus my stale lines.

- `grew`: **none in owned files**.
- `stale` (mine — pixel constants became token expressions; the coordinator refreshes the baseline):
  - `apps/mail/src/compose.rs [pixel_const]: now 15, baseline still 16 — lower it to 15`
  - `apps/totp/src/add_panel.rs [pixel_const]: now 19, baseline still 22 — lower it to 19`
  - `apps/totp/src/list.rs [pixel_const]: now 15, baseline still 16 — lower it to 15`
- Pre-existing, not mine (`app_and_shell_theme_escape_hatches_do_not_grow`): `apps/browser/src/browser_tabs.rs` `literal_color` 1 / `white_or_black` 1 and `shell/src/shell/frame.rs` `literal_color` 1 / `white_or_black` 2, all against a baseline of 0.

## Tests

- `cargo test -p feeds -p chat -p mail -p totp --lib` (`target/ux1x/a4/tests-apps-final2.log`) — chat **233**, feeds **113**, mail **228**, totp **107** passed; 0 failed.
- `cargo check -p feeds -p chat -p mail -p totp --tests` (`check-tests-final2.log`) — exit 0; only the pre-existing `ui` (`record.rs`, `animation_schedule.rs`) and `mail::engine::report_engine_error` warnings.
- `cargo build -p feeds -p chat -p mail -p totp` (`build-final2.log`) — exit 0.
- New unit tests: `list_layout::{time_label_clears_the_star_by_the_token_gap_and_the_pair_is_centred, a_scaled_time_line_keeps_the_star_clear_of_it, stacked_rows_report_bounds_that_do_not_touch, star_rect_is_centred_on_the_glyph_and_square, one_x_time_line_is_the_toolkit_line_height}`, `render_call::{avatar_caption_block_clears_the_name_plate_by_the_token_gap, a_short_tile_reads_the_block_from_the_band_top}`.
- Earlier in the checkpoint `mail::oauth::tests::loopback_server_captures_code_from_a_real_local_client` failed once under two concurrent capture jobs (a loopback-port test in a file this branch does not touch); it passes in every later run including the final one.

## Open — needs another unit

- **coordinator (closed on ux1x/integration: route the 12 sites through `ui::surface::panel` after merge)** — floating Panels painted as fill + `panel_border` on `edge_outline`: `apps/feeds/src/app.rs:2470`, `apps/chat/src/render_call.rs:374,953,1455,1629`, `apps/chat/src/render_chat.rs:1743,3690`, `apps/mail/src/app.rs:2022,2110`, `apps/mail/src/compose.rs:2025,2422`, `apps/totp/src/add_panel.rs:1201`.
- **coordinator (closed on ux1x/integration: F1 Button Neutral/Disabled rest edge derived)** — every `Button::paint` / `face_colors` Neutral site above paints the toolkit's declared `border_standard` rest edge on this base (visible on the `base` grounds, faint on the `raised` ones: mail reader action bar `mail/after/dark/wide.png`, chat flap close `chat/after/dark/people.png`, totp Cancel/Add account `totp/after/dark/add.png`); the merged toolkit derives it.
- **coordinator (closed on ux1x/integration: F1 Segmented/ToggleGroup)** — `ui/src/widget/toggle_group.rs` unselected segments have no track/edge: `totp/after/*/add-advanced.png` (SHA256/SHA512, `8`), `mail/after/*/setup*.png` (OAuth), `feeds/after/*/reader-settings.png` (Sans/Mono, Light/Dark).
- **coordinator (closed on ux1x/integration: F1 TextInput edges)** — the shared field's rest edge (totp search / add-panel fields, feeds dialog field, mail setup fields): `totp/after/dark/list.png`, `mail/after/dark/setup.png`.
- **coordinator (closed on ux1x/integration: F2 ModalDialog / ContextMenu as Panel role)** — feeds add-subscription / tags dialog and feed context-menu frames on dark: `feeds/after/dark/{add-subscription,tags,context-menu}.png`.
- **coordinator (closed on ux1x/integration: `separator` snaps to one device-pixel row)** — chat passes integer `y` (`header.y1`, `head.y1`, `head_y1`, attach-picker rule), which on this base paints the rule across two half-covered rows; the merged `separator` snaps it.

## Limits

- Chat: `--headless-drive` cannot take a scenario (no `shot_root`; the custom `shot_handler` renders PNG only), so the lint covers the live root only; all 21 scenarios are PNG-checked.
- Mail: the list/reader panes expose no audit-visible actionable nodes; those surfaces are PNG-checked.
- The totp ring's seconds/colour and mail's relative timestamps are wall-clock dependent, so BEFORE/AFTER PNGs differ there by design (compare against `after-prev/` before calling a diff a regression).
- The `+ Category` chip grew from 22 to 32 px (`metrics.min_pointer_target`), pushing the channel tree down 10 px; the label is 11 px, so the chip reads slightly airy at 1× — the toolkit's target minimum, kept.
- totp's banner band grew from 32 to 44 px: the shared `Banner`'s 1× floor (icon + leading bar + title) — Retry matches it.
- BEFORE geometry lints were not captured (the BEFORE binaries were rebuilt in place); BEFORE evidence is the PNG set plus the constant values quoted in the pair tables.
- totp `add-picker` renders identically to `add` under `--shots` (before and after): the shared file picker it opens is a floating surface the single-frame shot does not paint, so that scenario proves only the panel beneath it.
- Evidence directory `target/ux1x/a4/` is 75 MB (before / after-prev / after, three sets); left in place for the coordinator.
