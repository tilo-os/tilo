# A8 — media apps at 1×: music, photos, video-player, camera, tube

Branch `ux1x/a8-media`, cut from Unit 0 (`fd1f84391`). Files edited: `apps/music/src/app/render.rs`,
`apps/photos/src/{app,main}.rs`, `apps/video-player/src/app.rs`, `apps/camera/src/app.rs`,
`apps/tube/src/{screens.rs,widgets/mod.rs,widgets/flex.rs}`. Nothing outside `apps/`.

Evidence: `target/ux1x/a8/<app>/{before,after}/{dark,light}/<scenario>.png` (1280×800, 1×) plus
`matrix/dev.qos.<app>-{phone,tablet,laptop,desktop}.png` — 168 PNGs per phase, every one opened
with the Read tool. Inspection dumps + lint output: `target/ux1x/a8/lint/after/<app>/<theme>/`.
Capture script: `target/ux1x/a8/capture.sh`; lint: `target/ux1x/a8/lint.sh`.

## Six-rule checklist

| # | Rule | Status | Evidence |
|---|---|---|---|
| 1 | No two elements touch or overlap | Done for owned surfaces | Pair table below; `geometry_audit` on 30 dumps: `touching_peers=0` for photos, tube, video-player; camera's shutter/switcher semantic overlap (5 px) fixed → `overlaps=0`; the two camera `touching_peers` left are segments of one segmented control sharing an edge (the audit's own scope note). Music: `--headless-drive` exports 0 controls (hand-painted widget, no accessibility export) — the lint **cannot observe** music, so its gaps are proven by geometry constants + PNGs only (see Limits). |
| 2 | Every surface has a derived edge / contrast step | Done | `border_standard` on a raised ground: music 8 strokes → 0, photos 11 filled rules → 0, camera 13 → 0, tube 11 → 0, video-player 1 → 0. All card/pill/row edges via `ui::surface::{card, pill, separator, edge_on, edge_outline}` (music 29 sites, photos 25, camera 8, tube 5, video-player 2); floating panels via `ui::chrome::{panel_ground, panel_border}` (music queue preview + volume popover, photos compact menu, camera chooser + delete dialog + settings sheet, tube command palette); Bar-role rules via `chrome_hairline` (photos toolbar, tube top bar + rail). `Palette.border` in music now derives against `surfaces.base`. |
| 3 | One rhythm per screen | Done | Rhythm tables below: every relationship maps to one step (`s1` row surface gap / label↔glyph, `s2` related rows & paired controls, `s3` siblings & sections, `s4` panel padding, `s6` next group). |
| 4 | Content centred on measured line boxes | Kept / improved | Camera: top pills, gear and header now share one centre line (`EDGE + 16`) instead of 35/34/18 px; video-player subtitle box is bounded by the row pad again, not the inter-row gap; music download status label is clamped to the glyph. Pre-existing `vy()` centring in music unchanged. |
| 5 | Kept fixed offsets are token expressions or gone | Done for gaps | Constants converted to `Spacing::BASE.sN`: video-player 11, camera 9, photos 8 (+3 new `LIB_*`), music 4 new. Ratchet `f1_spacing_token_audit` reports only `stale` (counts fell: camera 31→22, photos 28→20, video-player 36→25). Remaining authored *sizes* (row heights, capsule widths, pill min heights) are metrics, listed under Limits. |
| 6 | Visual evidence, dark and light | Done | 168 BEFORE + 168 AFTER PNGs opened; every touched surface described in the tables below; light checked for regressions after each fix (the review found two `lift(1.0)`-on-`raised` collapses in light and one kurbo `inset` sign error; all three are fixed and re-captured). |

## Surface table (before → after, dark and light)

### music (`apps/music/src/app/render.rs`)

| Surface (PNG) | Before | After |
|---|---|---|
| Sidebar active nav row (`sidebar`, `queue`, `downloading`) | accent wash, no edge, rows 2 px apart | lift (`white10`) via `surface::card`, derived edge, marker bar keeps the accent; rows `s1` apart |
| Sidebar playlist row (`membership-open`) | bare `white5`, 0 px to next row | `surface::card` lift, `s1` gap |
| Home playlist chips (`sidebar`, `collapsed`, matrix) | bare `raised` fill, cover flush with chip | `surface::card`; cover inset `s1` inside the edge (`.inset(-CHIP_ART_INSET)`) |
| Search pill (all screens) | bare `raised`, edge only on hover | `surface::card`; focus ring unchanged |
| Search type tabs (`search-*`) | 1 px `border_standard` stroke, 10 px apart | `surface::card` edge, `s3` apart |
| Top-result hero + "Artist" kind pill (`search-tabs`) | bare `raised` / `white10` | `surface::card` |
| Play/Shuffle/Add-to-Library/Follow pills (`album-page`, `artist-page`) | outline = `border_standard` (invisible dark) | `surface::card` on `raised`; saved / following = opaque wash `over(base, accent|success, α)` with derived edge |
| Track rows (all lists) | hover/selection fill spans the full 52 px pitch | row surface 48 px, `s1` gap; revealed-membership row is a lift, not an accent wash |
| Queue "Now playing" row (`queue`) | translucent accent wash, no edge | opaque `accent_wash` + `surface::card` edge |
| Downloads banner, Liked "Keep offline" hint (`downloading`, `offline-*`) | bare `raised` / `border_standard` outline | `surface::card` |
| Download status label (`offline-error`) | "Retry · Network co…" painted **over** the download glyph | elided to `icon.x − 8 − s2` |
| Playlist management pills (`membership-open`) | `border_standard` outline, 10 px apart | `surface::card`, `s3` apart |
| Lyrics SYNCED badge (`lyrics`) | accent wash | `accent_wash` + edge |
| Queue preview, volume popover | `panel` fill + `border_standard` | `panel_ground` + `panel_border` (Panel role, crisp inset stroke) |
| Scrubber / volume tracks | `white10` (invisible in light) | `edge_on(base)` — visible in both themes |
| Header rule (large-text frame), sidebar divider, now-playing top rule | `border_standard` | `edge_on(base)` / `edge_on(bg)` |

Light: checked `sidebar`, `nowplaying`, `queue`, `search-tabs`, `album-page`, `artist-page`,
`downloading`, `offline-error`, `lyrics`, `membership-open`, `collapsed`, matrix ×4 — no regression,
tracks now visible.

### photos (`apps/photos/src/app.rs`, `main.rs`)

| Surface (PNG) | Before | After |
|---|---|---|
| Shot renderer (`main.rs`) | own `QOS_SHOTS_THEME` match, ignored `QOS_SHOTS_TEXT_SCALE` | `ui::launch::shot_theme(args)` — light/text-scale/motion resolve like every app |
| Toolbar rule + group rules (`opened`, all) | `border_standard` on the raised plate | `chrome_hairline` (Bar role); group rules `edge_on(raised)`; status rule `edge_on(base)` |
| Album chips (`library`) | raised fill + 1 px bottom rule; selected `neutral[3]` | `surface::card` (radius `sm`); selected = accent fill with `on_surface` ink |
| Metadata filter chips (`library`) | same bottom rule | `surface::card`; selected accent via `surface::pill` |
| Library card (`library`, `album-manage`) | square `window_card` + `neutral[2]` tray + raised footer + 1 px rule; no closed edge | one rounded card (`radii.md`), tray/footer clipped inside, edge `edge_on(card_fill)`; tray = `control_face(card_fill)` |
| Card footer controls Details / Remove / Star | `neutral[3]` squares | `surface::card(control_face(raised))`, inset `s1`, radius `sm` |
| New album / Edit album (`library`) | raised + bottom rule; disabled `neutral[2]` | `surface::card`; disabled = face at `DISABLED_ALPHA` over base |
| Album editor sheet (`album-manage`, large-text) | raised + top rule | `surface::card` (`radii.lg`) |
| Sheet Cancel / Save / Prev / Next | `neutral[3]` | `control_face(raised)` + edge; Save (metadata) accent `surface::pill` |
| Metadata sheet (`metadata-edit`) | raised + top rule | rounded panel, edge stroked over the scrolled body |
| Compact "More" menu (phone matrix) | raised + full-bleed `border_standard` rules | `panel_ground` + `panel_border`; `surface::separator(.., s3, ..)` |

Light: `library`, `album-manage`, `metadata-edit`, `opened`, `empty`, `error`, `loading`,
`cropping`, `edited`, matrix ×4 — selected album chip now distinguishable (was identical to its
neighbours after the first pass; caught in review).

### video-player (`apps/video-player/src/app.rs`)

| Surface (PNG) | Before | After |
|---|---|---|
| Library panel left edge (`library`, `recent`, `library-focus`) | `border_standard` on `raised` (invisible dark) | `edge_on(raised)` |
| Library rows | bare text on the panel, hover `neutral[2]`, 4 px pitch gap | `surface::card` on `surfaces.base` (step down from the panel) with derived edge; hover `bg_hover`; `s3` gap |
| Panel rhythm | header top 10, button gap 6, row gap 4 | `s3`, `s2`, `s3` (see rhythm table) |
| Transport pills (`playing`, `paused`, `fullscreen`) | shared `ButtonFace` | unchanged (toolkit-owned) |

Light: `library`, `recent`, `library-focus`, `playing`, `empty`, matrix ×4 — rows read as
light-grey cards on the white panel.

### camera (`apps/camera/src/app.rs`)

| Surface (PNG) | Before | After |
|---|---|---|
| Mode switcher (`camera`, `camera-video`, `scanner`) | `alpha(border_standard, .5)` stroke; bottom flush on the shutter; 44 px tap boxes overlapped the shutter's by 5 px | `edge_on(neutral[2])`; bottom `s3` above the shutter → semantic gap 7 px |
| Top pills + gear (`camera`) | pills at y 20 (centre 35), gear centre 34, header at 18, `EDGE` 18 | one corner inset `EDGE = s4`; pills, gear and REC pill on the `EDGE + 16` centre line; pill gap `s2` |
| Recovery buttons (`no-device`, `permission`, `cam-error`) | `alpha(border_standard, .55)` | `edge_on(face)` (accent or glass) |
| Camera chooser | raised + `border_standard` | `panel_ground` + `panel_border` |
| Delete dialog (`gallery-confirm-delete`) | raised + `border_standard`; Cancel `neutral[2]` + `border_standard` | `panel_ground` + `panel_border`; Cancel = hairline lift over the panel with `edge_on`; buttons `s3` apart |
| Gallery back button, detail panel (`gallery*`) | `alpha(border_standard, .5)` | `edge_on(raised)` |
| Gallery card selection ring | `alpha(border_standard, 0)` phantom stroke | stroke only when selected |
| Settings sheet + 3 group cards + row rules (`settings`) | `alpha(border_standard, .6/.5/.35)` | sheet `panel_border`; cards `surface::card`; rules `surface::separator(.., s3, raised)` |

Light: all 12 flag states + matrix ×4 checked; separators and group edges now visible in light
too (before they depended on the declared token).

### tube (`apps/tube/src/screens.rs`, `widgets/{mod,flex}.rs`)

| Surface (PNG) | Before | After |
|---|---|---|
| Card surface (`settings`, device-sync) | raised + `border_standard` | `edge_on(raised)` |
| Dividers inside cards / palette | `border_standard` fill | `divider_on(ground)` → `edge_on(ground)`; page dividers `edge_on(base)` |
| Subtle pills (Offline pill, stat pills, search field, quality menu, mode pills, device actions, status pill) | `border_standard` or none | `subtle_edge` = `edge_on(subtle_fill)`; hover pill edge derives from the hover fill |
| Top bar + rail (`home`, all) | `border_standard` | `chrome_hairline` (Bar role) |
| Command palette (`palette`) | raised + `border_standard` | `panel_ground` + `panel_border`; divider on the panel ground |
| `Flex` surface border (all) | stroked on the rect (half a pixel bled outside) | crisp inset stroke (`rect.inset(-w/2)`) |

Light: `home`, `settings`, `palette`, `play-demo`, `search`, `subs`, `history`, `trending`,
`empty`, `loading`, `device-sync`, `narrow`, matrix ×4 — no regression.

## Pair / rhythm tables (gap px before → after, step)

### music

| Surface | Element A | Element B | Axis | Before → After | Step |
|---|---|---|---|---|---|
| sidebar | nav row surface | next nav row surface | y | 2 → 4 | `s1` (`ROW_GAP`) |
| sidebar | playlist row surface | next playlist row | y | 0 → 4 | `s1` |
| track table / queue / large-text | row surface | next row surface | y | 0 → 4 (pitch 52 kept) | `s1` |
| search-tabs | tab pill | next tab pill | x | 10 → 12 | `s3` (`SIBLING_GAP`) |
| search-tabs | tab row | Top result heading | y | 12 → 12 | `s3` |
| membership-open | Set cover… | Rename / Delete | x | 10 → 12 | `s3` |
| home chips | cover art | chip edge | all | 0 → 4 | `s1` (`CHIP_ART_INSET`) |
| downloads row | status label end | download glyph | x | −? (overlap) → ≥ 8 | `s2` (`LABEL_GLYPH_GAP`) |

### photos

| Surface | Element A | Element B | Axis | Before → After | Step |
|---|---|---|---|---|---|
| library | album chip | next album chip | x | 8 → 8 | `s2` (`LIB_CHIP_GAP`) |
| library | album chip row | filter chips | y | 12 → 12 | `s3` (`LIB_ROW_GAP`) |
| library | filter chip | first card row | y | 6 → 8 | `s2` (`LIB_ROW_GAP_TIGHT`) |
| library | card | neighbouring card | x/y | 12 (8 on ≤2 cols) → same | `s3` / `s2` |
| library card | control hit rect | painted face | all | 4 → 4 (sign fixed) | `s1` |
| library (phone) | filter chips | New/Edit buttons | y | 9 → 8 | `s2` |
| library (phone) | New/Edit buttons | grid | y | 6 → 8 | `s2` |
| album editor | New album | Edit album | x | 8 → 8 | `s2` |
| metadata sheet | title | subtitle | y | 9.5 → 8 | `s2` |
| metadata sheet | subtitle | first field label | y | 27 → 24 | `s6` |
| metadata sheet | field label | its input | y | 2 → 4 | `s1` |
| metadata sheet | field | next field | y | 5 → 8 | `s2` |
| metadata sheet | body | footer | y | 8 → 8 | `s2` |
| compact menu | row | next row | y | full-bleed rule → rule inset 12 | `s3` |

### video-player (library panel, 1×)

| Element A | Element B | Axis | Before → After | Step |
|---|---|---|---|---|
| panel edge | header | y | 10 → 12 | `s3` (`PANEL_HEADER_TOP`) |
| Clear | Close | x | 6 → 8 | `s2` (`PANEL_BUTTON_GAP`) |
| Queue tab | Recent tab | x | 6 → 8 | `s2` |
| subtitle | tabs | y | 12 → 12 | `s3` (`PANEL_SECTION_GAP`) |
| tabs | first row | y | 12 → 12 | `s3` |
| row card | next row card | y | 4 → 12 | `s3` (`PANEL_ROW_GAP`) |
| row text | Remove / Play | x | 8 → 12 | `s3` (`STRIP_TITLE_GAP`) |
| Remove | Play | x | 6 → 8 | `s2` |
| Library capsule | title | x | 12 → 12 | `s3` |
| capsules | video edge | x/y | 14 → 16 | `s4` (`OVERLAY_MARGIN`) |
| last row | panel bottom | y | 14 → 16 | `s4` (`PANEL_GUTTER`) |

Test `library_panel_geometry_stacks_measured_lines_at_2x` re-pinned: subtitle 38→40, tabs 66→68,
rows 108→110 (the PNGs `video-player/after/*/library.png` are the justification).

### camera

| Surface | Element A | Element B | Axis | Before → After | Step |
|---|---|---|---|---|---|
| camera | mode switcher (visual) | shutter | y | 0 → 12 | `s3` (`ACTION_GAP`) |
| camera | switcher tap box | shutter tap box | y | −5 (overlap) → 7 | ≥ `s1` (asserted in `viewfinder_capsules_contain_their_labels_at_accessibility_scale`) |
| camera | setting pill | next pill | x | 9 → 8 | `s2` (`PILL_GAP`) |
| camera | pills / gear / REC / header | window edge | x, y | 18 / 20 / 18 / 34 → 16 (centre line `EDGE + 16`) | `s4` (`EDGE`) |
| delete dialog | Cancel | Delete | x | 10 → 12 | `s3` |
| recovery card | button | button | x | 8 → 8 | `s2` |
| settings card | row rule inset | card edge | x | 16 → 12 | `s3` (`SEPARATOR_INSET`) |
| scan card | pad / line gap / section gap / action inset | — | — | 16 / 4 / 12 / 12 → same as tokens | `s4` / `s1` / `s3` / `s3` |

### tube

No gap changed (the retained `Flex` layouts already sat on `theme.tokens.spacing`); this unit
only replaced declared borders with derived ones and made the stroke crisp.

## Geometry lint (`scripts/ux-bench/usability.py::geometry_audit`, AFTER, dark and light)

```
photos/{dark,light}/{library,album-manage,metadata-edit,opened}: controls=9 pairs=36 touching_peers=0 overlaps=0
video-player/{dark,light}/{library,recent,playing}: controls=3 pairs=3 touching_peers=0 overlaps=2   (see below)
camera/{dark,light}/camera: controls=8 pairs=28 touching_peers=2 overlaps=0   (was overlaps=1 before the switcher move)
tube/{dark,light}/home: controls=7 pairs=21 touching_peers=0 overlaps=0
music/*: controls=0 — not observable (see Limits)
```

- video-player `overlaps=2`: the floating Library / Open capsules sit **on** the `VideoPlayer`
  widget by design (an overlay strip over the media, not a neighbouring element); the audit
  reports any sibling intersection. Not a rule-1 defect; left as is.
- camera `touching_peers=2`: `photo mode | video mode | Open Scan` are the three segments of one
  segmented control and share edges by construction — the audit's `scope` string names exactly
  this case.
- `--headless-drive` on this base takes `--scenario` as a CLI flag (not a JSON field); camera and
  tube shot flags are `ShotsOnly`, so only their live roots were linted.

## Ratchet output

`cargo test -p ui --test it -- design_token_audit f1_spacing_token_audit`:

```
test design_token_audit::declared_borders_in_chrome_do_not_grow ... ok
test design_token_audit::theme_escape_hatches_do_not_grow ... ok
test design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow ... FAILED   (pre-existing, not this unit)
    apps/browser/src/browser_tabs.rs [literal_color] 1 / [white_or_black] 1, baseline 0
    shell/src/shell/frame.rs        [literal_color] 1 / [white_or_black] 2, baseline 0
test f1_spacing_token_audit::spacing_literals_do_not_grow ... FAILED   (stale only — coordinator re-baselines)
    apps/camera/src/app.rs       [pixel_const]: now 22, baseline still 31 — lower it to 22
    apps/photos/src/app.rs       [pixel_const]: now 20, baseline still 28 — lower it to 20
    apps/video-player/src/app.rs [pixel_const]: now 25, baseline still 36 — lower it to 25
```

No `grew` line anywhere. No change to any baseline file.

## Tests run

| Command | Result |
|---|---|
| `cargo test -p music --lib` | 212 passed |
| `cargo test -p photos --lib` / `--tests` | 94 passed; end_to_end 20, one_photograph_one_address 5 |
| `cargo test -p video-player --lib` | 53 passed |
| `cargo test -p camera --lib` | 92 passed (switcher-placement test re-pinned to `s3` above the shutter + a `≥ s1` tap-box gap assertion) |
| `cargo test -p tube --lib` / `--tests` | 134 passed; interaction 23, large_text_layout 5, main 1 |
| `cargo check -p music -p photos -p video-player -p camera -p tube --tests` | clean |
| `cargo build -p music -p photos -p video-player -p camera -p tube` | clean |

All with the mandated inline env and `--no-default-features --locked`.

## Code review (`code-review high`)

Ten findings; seven fixed before commit: kurbo `Rect::inset(+)` grows (photos footer faces, music
chip art — both flipped to `inset(-…)`); `lift(1.0)` collapsing onto `raised` in light (photos:
selected album chip → accent + `on_surface` ink; neutral faces → `control_face(ground)` = one
control hairline composited over the ground); video-player subtitle box bounded by
`PANEL_ROW_PAD_Y`; photos card edge derives from `card_fill`; tube `divider_on` dead parameter and
stale doc; camera vertical corner insets moved onto `EDGE`. Two recorded, not changed: the
`Spacing::BASE` compile-time constants are what the plan prescribes for painter geometry
(scaled `theme.tokens.spacing` would drift against the apps' unscaled authored sizes), and the
four-crate `step()` copy needs a `ui` change (below).

## Open — needs other units

- **needs F2** — music `new-playlist` sheet and `context` menu are the shared `ModalDialog` /
  `ContextMenu`: no rest edge on the dialog box, Cancel a bare face (`music/before/dark/new-playlist.png`,
  `context.png`; `ui/src/widget/modal_dialog.rs:581`, `context_menu.rs:522`).
- **needs F2** — music / photos / tube empty states are the shared `EmptyState` (`music/*/empty.png`,
  `photos/*/empty.png`, `tube/*/empty.png`); nothing app-owned to fix.
- **needs F1** — the shared `TextInput` inside music's search pill and photos' sheets, and the
  `Toggle` faces in camera settings, keep their toolkit rest edge (`text_input.rs:4458`, `toggle.rs:266`).
- **needs coordinator / ui** — a `pub const fn Spacing::px(step: f32) -> f64` (or `f64` accessors)
  in `ui/src/tokens/spacing.rs` would delete the four identical `const fn step()` copies
  (camera 61, music 126, photos 62, video-player 46).
- **needs coordinator** — re-baseline `ui/tests/f1_spacing_baseline.txt` for camera / photos /
  video-player (stale counts above).

## Limits

- Music exports no accessibility nodes from its hand-painted root, so `geometry_audit` sees zero
  controls: rule 1 for music rests on the constants (`ROW_GAP`, `SIBLING_GAP`, `CHIP_ART_INSET`,
  `LABEL_GLYPH_GAP`) and the opened PNGs, not on the lint. Adding an accessibility export is out
  of this unit's scope (behaviour, not paint).
- BEFORE lint dumps were not produced (the BEFORE binaries were overwritten by the AFTER build);
  BEFORE gaps in the pair tables come from the source constants at `fd1f84391` and the PNGs.
- Camera: `CTRL_PILL_PAD_Y 7.5`, `SCAN_ACTION_PAD_Y 7`, `SCAN_ROW_PAD_Y 6`, `BADGE_PAD_Y 3`,
  `GLASS_HEADER_PAD_Y 10`, `DETAIL_ROW_GAP 10.4` stay literal: they are measured line-box paddings
  tuned to the renderer's line heights, not sibling gaps. Music's 8.4k-line painter still holds many
  authored offsets (`28.0` gutters, `52.0` rows); this unit converted the ones that produced
  touching or double rhythms and left the rest as metrics.
- Tube `--device-sync` renders the home grid under `--no-default-features` (the `grayjay-sync`
  feature is off), so that state was captured only as its fallback.
- Video-player `tests/real_decode.rs` was compiled (`cargo check --tests`) but not run (needs a
  decoder fixture); the lib suite covers the panel geometry.
