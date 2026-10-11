# UI migration guide

How an app moves onto Tilo's unified design system. `docs/design-language.md`
says what Tilo looks like; `crates/sdk/ui-core/HIG.md` says how screens are
built. This guide is the mechanical part: what to delete, what replaces it,
and how to prove the app is done. Where this guide and the design language
disagree, the design language wins.

Apply the checklist to one app per change. Each section after it maps an old
API or hand-drawn pattern to its replacement.

## App migration checklist

Work through every step for the app (`apps/<name>`). Do not touch other apps
or the toolkit; a missing capability is reported, not forked into the app.

1. **Read first.** `docs/design-language.md`, then the sections of this guide
   for every API the app uses.
2. **Roles, not appearance.** Every piece of text gets a `TextRole`
   ([Typography](#typography)); every button a `ButtonRole` (`Primary` for the
   one main action of a view, `Destructive` for anything that deletes or
   revokes, `Secondary` for the rest, `Plain` for inline text actions) and a
   `ControlSize`; every list a list style (`Sidebar`, `Inset`, `Plain`); every
   area of the window a region (`Content`, `Sidebar`, `Inspector`). Remove
   every colour, font size, radius, padding and border width the app passes
   to a widget.
3. **No own ground.** Delete any fill of the whole window or of a whole pane
   (`surfaces.base`, `surfaces.window_card`, `bg_app`, `bg_primary`, a literal
   colour). The framework paints the window ground and each region's tier
   ([Ground & regions](#ground--regions)).
4. **Navigation and selection.** Sidebars, master/detail and drawers come from
   the shared shell and list ([Navigation](#navigation)). A selected row, tab
   or segment is the system lift, never an accent fill and never painted by
   the app.
5. **Tokens for what remains.** Genuine content drawing (charts, documents,
   maps, media, canvases) keeps its painting code but reads every value from
   the theme: `theme.tokens.spacing.s1`–`s16`, `theme.tokens.metrics.*`,
   `theme.tokens.radii.*`, theme colours. Delete module-level pixel and size
   constants.
6. **Icons.** Replace every Unicode glyph and emoji used as an icon (`✕`, `×`,
   `⟳`, `▾`, `🔍`, …) with `ui::icons`.
7. **Copy.** Sentence case everywhere. No all-caps labels, no `A · B · C`
   metadata chains, no taglines or subtitles under headings, no window title
   drawn by the app, no developer notes. A button names what happens
   ("Delete message", not "OK"). An empty state is one sentence and at most
   one action. Demo and fixture content is realistic and complete.
8. **Empty your shards.** Every audit keeps a shard per app at
   `crates/sdk/ui-core/tests/baselines/<audit>/apps-<name>.txt` for `theme`,
   `app-paint`, `spacing`, `typography` and `copy`
   ([Ratchets](#ratchets-what-each-key-asks-you-to-replace) maps every key to
   its fix). The goal is to delete every one of them. An entry may remain
   only for genuine content drawing or for a hit that is not UI (a volume
   label in fixture data, a protocol token), and it keeps its true count.
   Never reshape code so a pattern stops matching (HIG §9.13). Lower the
   shards as described in [Updating shards](#updating-shards).
9. **Shots.** Render the app and look at every screen it has, dark, light
   and narrow:

   ```bash
   cargo run -p <pkg> --bin <bin> -- \
     --shots target/look/<dir>/x.png --viewport 1280x800
   TILO_SHOTS_THEME=light cargo run -p <pkg> --bin <bin> -- \
     --shots target/look/<dir>/x-light.png --viewport 1280x800
   cargo run -p <pkg> --bin <bin> -- \
     --shots target/look/<dir>/x-narrow.png --viewport 800x600
   ```

   When a cargo drop-in renders on a remote host, bring `target/look/<dir>`
   back, and give each app its own `<dir>` so fetches never overwrite each
   other. Open the PNGs and compare them with the design language: one
   ground per region, no accent outside presence and status, sentence-case
   copy, no glyph icons, a sidebar that collapses at 800 px.
10. **Verify once.** `cargo clippy -p <pkg> --all-targets`,
    `cargo test -p <pkg>`, and
    `cargo test -p tilo-ui --test it do_not_grow --success-output immediate`
    (every ratchet passes and prints the shard text to commit).

### Updating shards

A shard lists `<path relative to the repository root> <key> <count>` lines for
one unit (`apps-<name>`, `ui-core` or `shell`). A missing shard allows zero.

- **A remote cargo drop-in** neither forwards the environment nor brings a
  test's writes back. Through it run
  `cargo test -p tilo-ui --test it do_not_grow --success-output immediate`.
  Every ratchet whose tree dropped below its shard prints a `notice:` with
  the stale entries and then either `<shard path> now reads:` followed by the
  complete new shard text, or `delete <shard path>` when the unit reached
  zero. Write exactly that text to the file (or delete it). Do not edit
  other units' shards.
- **cargo** (CI, or a local toolchain):
  `UPDATE_RATCHET_BASELINES=1 RATCHET_SCOPE=apps/<name> cargo test -p tilo-ui --test it do_not_grow`
  rewrites only that app's shards and deletes the ones that reached zero.
  It refuses to raise a count unless `RATCHET_ALLOW_GROWTH=1` is set, and
  an app migration never sets it.

A ratchet test that fails lists each file that grew, the key, and what to use
instead. Fix the code; do not raise the shard.

## Ratchets: what each key asks you to replace

Find the key in the failing test or in the app's shard, then apply the
replacement. "Delete" means the line goes away because a widget or the
framework now does the job.

### `theme` and `app-paint` (colour, ground, shape, paint)

| Key | Matches | Replace with |
|---|---|---|
| `literal_color` | `Color::from_rgb8(12, …)`, `Color::from_rgba8(0x…, …)` | A theme colour. Status: `danger`/`warning`/`success` ink. Presence: the participant's presence colour. User content (album art, calendar colours, photo data) stays and keeps its count. |
| `white_or_black` | `Color::WHITE`, `Color::BLACK` | The theme's ink tiers, the inverted primary label, or the scrim token. |
| `theme_blind_white` | `surface_white_*`, `border_white_*` | `Colors::lift()` for a raised fill, `Colors::hairline_on(ground, …)` for an edge; in app chrome, delete and use the widget. |
| `literal_color_new` | `Color::new([0.2, …])` | Same as `literal_color`. |
| `ramp_slot` | `.neutral[3]`, `accent[5]` | `surfaces.content` / `sidebar` / `inspector` / `raised` / `overlay` and the text tiers. Selection is the lift: delete the fill and use the list or segmented widget. |
| `legacy_alias` | `.bg_app`, `.bg_primary`, `.bg_hover`, `.bg_active` | Removed from `Theme`; map each use with [Ground & regions](#ground--regions). Ground fills: delete. Hover and active states: delete when a widget paints the control, else the lift of its ground. |
| `desktop_ground` | `surfaces.base`, `surfaces.window_card` | Delete the fill and declare the region; `surfaces.base` is the desktop behind tiles. |
| `literal_radius` | `.to_rounded_rect(8.0)`, `RoundedRect::from_rect(r, 6.0)`, `fill_round(s, r, 4.0, …)`, `.radius(12.0)` | Controls and rows: delete; the widget's role gives the radius (control 8, item 6, chip full). Content drawing: `theme.tokens.radii.*`, concentric (inner = outer − inset, minimum 4). |
| `literal_alpha` | `with_alpha(0.5)`, `with_alpha_factor(x * 0.4)`, `multiply_alpha(0.6)` | The theme's state, scrim or material token; hover/press washes come from the widget. |
| `raw_paint` | `scene.fill(`, `scene.stroke(` | Chrome (bars, rows, buttons, chips, cards, separators, backgrounds): compose `ui` widgets and delete the painter. Content drawing (plots, documents, maps, canvases, media) stays, reading theme values. |
| `glyph_icon` | a string or char literal with `✕ × ⟳ ↻ ⌫ ⇅ « » ‹ › ▾ ▸ ▴ ◍ ◎ ◷ ⚓ ➤ ⏰` or an emoji | The matching `ui::icons` icon (an icon-only button carries an accessible label). A `×` that is real text ("1280×800") stays. |
| `transparent_shot` | `render_png(…, …TRANSPARENT…)` | Render shots through the framework so they land on the window ground (see [Window chrome & shots](#window-chrome--shots)). |

### `spacing`

| Key | Matches | Replace with |
|---|---|---|
| `pixel_const` | `const PAD: f64 = 12.0;` | Delete the constant. Gaps and insets: `theme.tokens.spacing.sN` (4 → `s1`, 8 → `s2`, 12 → `s3`, 16 → `s4`, 24 → `s6`, 32 → `s8`, 48 → `s12`, 64 → `s16`; round to the nearest step). Control, bar and row heights, icon sizes, panel widths: `theme.tokens.metrics.*`, or delete when the widget sizes itself. |
| `taffy_length` | `length(12.0)` in a `Style` | `insets_all(spacing.sN)` / `insets_xy(h, v)` or `length(spacing.sN)`. |
| `literal_gap` | `.with_gap(12.0)`, `.with_padding(16)`, `.with_row_gap(…)`, `.with_column_gap(…)` | `.with_gap(spacing.sN)` etc., or delete it when the composition (form, section, list) already spaces its children. |
| `literal_spacer` | `Spacer::fixed(12.0)` | Delete it when the gap belongs to the container; otherwise `Spacer::fixed(spacing.sN)`. |

Gaps by relationship replace the reads and hacks apps used to get them:

| Old | New |
|---|---|
| `.with_gap(Spacing::BASE.sN)`, `.with_gap(Spacing::BASE.rhythm.group)`, an app `mod rhythm { px::GROUP .. }` | `.with_rhythm(Rhythm::Group)` (or `Tight`, `Related`, `Sibling`, `Section`) on the `Column` / `Row`; it follows density, `ui_scale` and a custom spacing scale |
| `.with_padding(h, v)` / `with_horizontal_padding` / `with_vertical_padding` with `Spacing::BASE` steps | `.with_padding_rhythm(Rhythm::Sibling, Rhythm::Tight)` (`None` for no inset on an axis) |
| `Spacer::token(SpacingStep::S0)` between two children to double the column's gap | Delete it: a zero spacer is out of the flow now. Say the gap: `.with_rhythm(..)` on the column, or split the column in two |
| Pushing a child only when it has content, or a spacer that "cancels" the gap of an empty one | `ui::widget::optional(Some(child))` / `optional(None)`, or `When::new(child, || shown)` / `When::from_state(&state, |s| ..)`; an absent child takes no gap. `Text::from_state` that renders `""` and a `ModelView` that builds `optional(None)` collapse the same way |
| A `Decorator` around a `Column` that sets its gap from the theme in `build_layout` (snippets `Gapped`) | `Column::with_rhythm` |

### `typography`

| Key | Matches | Replace with |
|---|---|---|
| `literal_font_size` | `TextStyle::new(13.0)`, `.with_size(20.0)`, `.text_size(14.0)`, `.with_font_size(…)`, `.set_font_size(…)` | A `TextRole` ([Typography](#typography)). By size: 34 → `Display`, 26–28 → `LargeTitle`, 20–22 → `Title`, 16–18 → `Heading`, 14–15 semibold → `Subheading`, 14 → `Body`, 13 → `Label` (`Code` for monospace), 12 and below → `Caption`; long-form reading → `Reading`. Bold or semibold emphasis inside a role → `.strong()`, tabular figures → `.numeric()`. |
| `legacy_size_api` | `.draw_text(`, `.draw_text_weighted(`, `.measure(`, `.measure_weighted(`, `.measure_styled(`, `.elide_to_width(`, `.elide_to_width_weighted(` | A `Text` widget, or the role-based measure/draw/elide calls ([Typography](#typography)). |
| `font_size_const` | `const TITLE_FONT: f32 = 15.0;` | Delete the constant; name the role where the text is made. |
| `type_step_arithmetic` | `typography.scale.sm - 1.0` | The role whose size fits; never a size between roles. |
| `raw_type_step` | `typography.scale.*`, `TypeScale::default()` | A `TextRole`. |
| `renderer_scale_helper` | `renderer_input_style`, `renderer_type_size` | Delete; a role is scaled once, by the theme. |

### `opentype`

| Key | Matches | Replace with |
|---|---|---|
| `raw_opentype_feature` | `.with_numeric_style(…)`, `.with_font_feature(…)`, `.with_kerning(…)`, `.with_discretionary_ligatures(…)`, `.with_small_caps(…)`, `OpenTypeTag::new(…)`, `FeatureTag::new(…)`, a byte-string tag such as `b"tnum"` or `b"ss01"` | The text's `TextKind` ([Text kinds](#text-kinds)); the toolkit picks the features the face has. |

### `copy`

| Key | Matches | Replace with |
|---|---|---|
| `all_caps_label` | a displayed literal opening with an all-caps word (`"COLLECTIONS"`, `"REC  {}"`) | Sentence case (`"Collections"`). A section header uses the `Subheading` role (`Heading` for a top-level section). Acronyms (`PDF`, `HTTP`, `GET`) are not counted. A hit that is data rather than UI (a volume label in fixtures) stays. |
| `middle_dot_join` | a literal containing ` · ` (`format!("{a} · {b}")`, `.join(" · ")`) | The first fact as the row title and the rest as a `Caption` secondary line; a `PropertyList` for label/value facts; table columns for repeated facts. |
| `to_uppercase` | `.to_uppercase()` | Displayed text: delete the call and write the source string in sentence case. Comparisons: `eq_ignore_ascii_case` or `to_lowercase`. Content whose form is upper case (hex digits, a licence plate) stays. |

### `chrome-border` (toolkit and shell only)

`declared_border` counts `.border_standard`, `.border_strong`,
`overlay_border_strong` and `colors.neutral[` in toolkit and shell chrome.
Derive the edge from the ground: `tilo_ui::surface::{edge_on, card,
separator}` or `tilo_ui::chrome::{panel_border, chrome_hairline,
selection_tier}` (HIG §9.5).

## Colour, shape and elevation tokens

The colour tokens are now the design language's neutral material, intents
and presence palette. There is no accent colour any more: nothing in
`theme.tokens.colors` is a brand hue, and an app cannot set one.

### Removed APIs and their replacements

| Old | New |
|---|---|
| `theme.accent_primary`, `colors.accent[5]`, `colors.accent[ACCENT_PRIMARY_IDX]`, `colors.accent.stops[..]` | pick by intent, see "Choosing the intent" below |
| `theme.accent_hover`, `colors.accent[4]`, `colors.accent[ACCENT_HOVER_IDX]` | `colors.intent.inverted_hover` (the hover/pressed step of the same intent) |
| `colors.accent[0..=3]`, `colors.accent[6..=10]` (tints and shades) | a lift (`colors.lift_on(ground, t)`) for a chosen/hovered body; `colors.text_dim` for quiet ink; delete if decorative |
| `colors.on_accent(min)` | `colors.intent.on_inverted` (label on an inverted fill), or `colors.text_primary` when the fill became a lift |
| `colors.accent_chip_on(ground, min)` | `colors.tinted_chip_on(ground, tint, 0.18, min)` with `tint` = `colors.intent.you` (yourself), a presence colour (someone else) or a `colors.semantic.*` colour (status) |
| `AccentChip` | `TintedChip` |
| `ACCENT_PRIMARY_IDX`, `ACCENT_HOVER_IDX` imports | delete |
| `colors.categorical(i)` | `colors.presence.series(i)` (chart/tag series) or `colors.presence.get(slot)` (a person) |
| `Theme::with_accent(rgb)` | nothing in an app: the person's own colour is identity, applied by the host (`Theme::with_your_colour(PresenceHue)`) |
| `Theme::brand_accent`, `Theme::with_brand_accent`, `BrandAccentRejection`, `Theme::allow_app_accents`, `appearance.app_accents` | delete: an app has no colour of its own. Brand hues an app needs for *content* (a calendar's event colour, a map pin) come from `colors.presence` |
| `WorkspaceTint`, `appearance.workspace_tint` | delete; a workspace's temperature comes from its wallpaper (`ui::workspace_hue(pixels)` → `Theme::with_neutral_hue`, applied by the shell; `ThemeLayerStack::workspace_hue` for a host resolving layers) |
| `PIN_ACCENT`, `PIN_SHAPE`, `DistributionDefaults::accent` | delete |
| `surfaces.window_card` | `surfaces.content` |
| `surfaces.window_card_border` | `surfaces.content_edge` (read it through `colors.window_edge()`) |
| `surfaces.glass` | `surfaces.material` (overlay at 72 %, only behind a compositor blur); paint `surfaces.overlay` where nothing blurs the backdrop |
| `surfaces.glass_blur_px` | `surfaces.material_blur_px` |
| `theme.material_glass()` | `theme.tokens.colors.surfaces.overlay` (opaque) |
| `theme.material_film()` | `theme.tokens.colors.glass_film()` |
| `tokens.material_tint`, `CustomThemeSpec::material_tint`, `MATERIAL_TINT_*` | delete |
| `theme.on_card()`, `colors.on_card()` | `theme.on_content()`, `colors.on_content()` |
| `theme.on_window_card()` | `theme` itself (the theme's text tiers are already ink on content); `.clone()` where an owned theme was needed |
| `OnCard` | `OnSurface` |
| `ON_CARD_SMALL_TEXT_CONTRAST` | `SMALL_TEXT_MIN_CONTRAST` |
| `surface_white_5` / `_10` / `_20` | `overlay_subtle` / `overlay_medium` / `overlay_strong` |
| `border_white_10` / `_20` | `overlay_border_subtle` / `overlay_border_strong` |
| `PaletteSpec`, `Colors::themed`, `Colors::set_accent`, `Colors::set_overlays` | delete; colours come from a theme document (`Theme::from_document`, `Theme::preset`, `Colors::of_palette`) |
| `Colors::tilo_blue()`, `Theme::{tilo_blue, dracula, nord, gruvbox, gruvbox_light, catppuccin_mocha, catppuccin_latte, solarized, oled, warm_light, rose_pine}` | `Theme::dark()` / `Theme::light()` (`Theme::ALL` is now `Dark`, `Light`, `High Contrast`); a different temperature is `theme.with_neutral_hue(h)` |
| `CustomThemeSpec` (every schema), `Theme::from_custom_spec`, `appearance.theme`, `appearance.custom_theme` | `ThemeDocument` schema 6 (`Theme::from_document`); see [Theming](#theming) |
| `ContentSurface::accent` | delete |
| `QueryInk::accented(text, placeholder, accent)` | `QueryInk::of(&theme.tokens.colors)` (caret and selection in your colour) |
| `palette.surface_radius_px()` (`CommandPalette`) | `CommandPalette::surface_radius_px(theme)` (the panel radius) |
| `ui::presence::PEER_PALETTE` (24 entries), `user_color` returning Tailwind colours | 12 entries, the dark presence palette; `user_color` is an identity, paint it with `colors.presence.of_rgb(user_color(id))` |
| `Theme::configured()` reading the person's colour | the window's host applies it (`Theme::in_window`, see [Window appearance](#window-appearance)); an app does nothing |
| `elevation.e0` … `elevation.e4` | `elevation.floating` for anything floating (menu, popover, palette, sheet, quick settings, toast, tooltip, dragged object); **no shadow** for a window tile, a card, a bar or a selected control: delete the `surface::shadow` call |
| `radii.sm` (4) | `radii.detail` (progress track, badge, sparkline clip, caret) |
| `radii.md` (8), `radii.lg` (12) | `radii.control` (8) for a button, field, segmented track or a tile inside a panel; `radii.item` (6) for a selected row or a segment thumb |
| `radii.xl` (24) | `radii.panel` (18) for a floating panel; `radii.tile` (14) for a window tile or an object card |
| a radius literal for something inset inside a rounded container | `radii.concentric(outer, inset)` |
| `theme.radius_standard` | unchanged name, now `radii.control` |

### Choosing the intent

Every former accent use is exactly one of these. Decide by what the pixel
*means*, not by what it looked like:

| The accent was… | Now |
|---|---|
| the fill of a primary button, a switch that is on, a checked checkbox, a slider's filled track, a progress fill, a selected-count badge | `colors.intent.inverted` (fill) + `colors.intent.on_inverted` (label/glyph on it); hover/pressed `colors.intent.inverted_hover` |
| the armed row of a menu, popover, dropdown, slash menu or result list (the row Enter or a click activates) | the panel's overlay lift: `ui::widget::floating_surface::armed_row(theme)` (fill and ink), or `paint_armed_row(scene, t, row, inset, theme)`; a lift one and a half surface steps off the panel (HIG §9.3), never the inverted fill |
| the background of a selected row, tab, segment, chip or grid cell (a list, table, sidebar, segmented control) | a lift of the surface it sits on: `colors.lift_on(ground, 1.0)` (hover `0.5`) with `colors.text_primary` ink. On the window ground use `colors.intent.selection` / `selection_hover`; inside a floating panel `ground = colors.surfaces.overlay`; on a segmented track `surfaces.raised` |
| a translucent accent wash (`accent.with_alpha_factor(a)`) used as a hover/selected film | `colors.text_primary.with_alpha_factor(a)` or the matching veneer (`overlay_subtle` ≤ 0.15, `overlay_medium` ≤ 0.3, else `overlay_strong`) |
| the text caret, the focus ring, a drop/insertion indicator, selection handles | `colors.intent.you` (`theme.border_focus` is the same colour) |
| the text-selection highlight | `colors.intent.selection_tint` (already translucent; do not add alpha) |
| a link | `colors.text_primary` with an underline |
| an "active" icon or label tint | `colors.text_primary` (inactive stays `text_dim`) |
| live / syncing / in-flight state | `colors.semantic.info`; an unread count or anything that wants attention `colors.intent.you` (`ChipStyle::Live`); `colors.semantic.danger` only for an error, a failure or a destructive action |
| a person, collaborator, agent, avatar | `colors.presence.of_rgb(ui::presence::user_color(id))` for an identity colour, `colors.presence.get(slot)` for a slot |
| a chart or data series, a tag colour | `colors.presence.series(index)` (or `ChartPalette::from_theme(theme).categorical(index)`) |
| decoration (a glow, a gradient, a coloured header, a tinted empty-state disc) | delete it, or use `colors.text_dim` / a lift |

A label that was hard-coded `Color::WHITE` on an accent fill must become
`colors.intent.on_inverted`: the inverted fill is near-white on dark themes.

### New APIs

- `Colors::of_palette(&palette, emphasis, presence_tone, you)`,
  `Colors::{dark, light, high_contrast}()`, `Tokens::{dark, light}()`;
  themes from `Theme::preset(Preset::…)` / `Theme::from_document`.
- `surfaces.{base, content, sidebar, inspector, raised, overlay, scrim,
  material, material_blur_px, content_edge}`. Paint the window ground with
  `surfaces.content`, a navigation column with `surfaces.sidebar`, a detail
  column with `surfaces.inspector`, fields and wells with `surfaces.raised`.
- `colors.intent.{inverted, inverted_hover, on_inverted, selection,
  selection_hover, you, selection_tint}` and `colors.lift_on(ground, t)`.
- `colors.presence` (`PresencePalette`): `get(slot)`, `series(index)`,
  `of(color)`, `of_rgb((r, g, b))`; `PRESENCE_LEN` = 12.
- `Theme::with_neutral_hue(h)`, `Theme::with_your_colour(PresenceHue)`,
  `ui::workspace_hue(rgba_pixels)`, `ui::color::{Oklch, oklch, dominant_hue}`.
- `Radii { tile, panel, control, item, detail, full }` and
  `Radii::concentric(outer, inset)`.
- `Elevation { floating, floating_edge, glass_inner, glass_outer,
  shadow_scale }`: draw `floating` with `ui::surface::shadow(scene,
  transform, rect, radius, &elevation.floating)`, and on a dark material
  stroke `floating_edge` 1 px inside the panel outline.

### Grounds

- An app's background is `surfaces.content` (the framework owns it); a
  navigation column is `surfaces.sidebar`, a detail column
  `surfaces.inspector`. `surfaces.base` is the *desktop* ground. The
  framework paints all of them; [Ground & regions](#ground--regions) says
  what to delete and what each removed alias becomes.

### What to delete in an app

- Any accent picker, "app accent" switch or workspace-tint control; the
  appearance controls Settings keeps are listed in [Theming](#theming).
- Hard-coded per-app palettes (`const ACCENT_*`, Tailwind literals) that
  stood in for the accent ramp.
- Shadows under window content, cards and selected controls.

## Typography

Text names a role; the theme decides size, weight, leading and family, scaled
once by the UI and text scale. The ten roles, their sizes and when to use each
are in `docs/design-language.md` and `docs/typography.md`; pick the role from
the [`typography` ratchet table](#typography) when the old code only has a
pixel size. There is no all-caps, overline or letter-spaced role: write the
string in sentence case and use `Subheading` (or `Heading` for a top-level
section).

### New API

| API | Use |
|---|---|
| `ui::TextRole::{Display, LargeTitle, Title, Heading, Subheading, Body, Label, Caption, Reading, Code}` | What a piece of text is. |
| `theme.text(role) -> TextStyle` | The complete, already-scaled style for custom measuring, painting and eliding. |
| `TextStyle::strong()` / `Text::strong()` | Emphasis inside the role: one weight step up (Regular/Medium → Semibold, Semibold → Bold). |
| `TextStyle::numeric()` / `Text::numeric()` | Shorthand for `with_kind(TextKind::Numeric)`: tabular figures for counters, clocks, sizes and columns. |
| `ui::TextKind::{Prose, Numeric, Secret, Identifier, Url, Code}`; `Text::with_kind`, `TextStyle::with_kind`, `ColumnDef::with_kind` | What text is, for the OpenType features the toolkit picks against the face ([Text kinds](#text-kinds)). Links, email addresses, mentions and room addresses inside prose are found and set apart with no kind declared. |
| `ui::text_detect::identifier_spans(text)` | The links, email addresses, mentions and room addresses in a string, as the byte ranges the text is drawn with, for an app that acts on them. |
| `TextRenderer::security_surface(..)`, `TextStyle::on_security_surface()` | A custom-drawn security surface (link preview, verification screen): every look-alike letter set apart in all its text. Toolkit prompts and identity sheets do this themselves. |
| `RichSpan::kind(TextKind)` | A rich-text run that knows what it is; mention marks are `Identifier` and external link marks `Url` already. |
| `TextInputKind::Secret`, `TextInputKind::text_kind()` | A code or key typed in the clear; every field kind sets its text kind while typing. |
| `ui::UiFace::{Default, Hyperlegible}`, `Typography::set_face(name)`, `Typography::face_name()`, `UI_HYPERLEGIBLE_FAMILY` | Theme authors: the UI face by the name a theme document stores. |
| `ui::ReadingFace::{Default, Libertinus, Literata, Garamond, SourceSerif}`, `ReadingFace::{ALL, name, family, from_name}`, `Typography::set_reading_face(name)` / `with_reading_face(name)` / `reading_face_name()` | The `Reading` role's face, by name ([Reading faces](#reading-faces)). |
| `ui::TextGround::{Light, Dark}`, `TextGround::of(colour)`, `TextStyle::with_ground` | The ground text is set on. Theme roles carry their theme's; a surface that paints its own page (a sepia book page in a dark theme) sets it, so the reading face keeps its sturdier cut on a dark page. |
| `TextStyle::with_content_size(px)` / `Text::with_content_size(px)` / `TextStyle::content(px)` | Text sized by its content (a canvas, a slide, a document's own type): the size is used as given and never grown by the text scale. |
| `PaintCtx::text_role`, `measure_role`, `line_box`, `text_role_in_band` | Custom painters: paint, measure, size a row, or centre one line in a band in a role. |
| `ui::widget::ControlSize::{Small, Regular, Large}` | A control's size: label role (`Label`, `Label`, `Body`) and height (`bar_control_height`, `min_pointer_target`, `bar_height`). |
| `with_text_role(role)` on `TextInput`, `Autocomplete`, `EditableLabel`, `Link`, `RichTextLine`, `RichTextEditor`; `set_text_role` on `TextInput`, `EditableLabel`, `RichTextLine`; `TextInput::text_role()`, `BlockEditor::text_role()` | The role a text widget sets its text in. The role sets the family too: `Reading` is serif and `Code` mono in a `TextInput`, its placeholder and a `RichTextEditor`, and an `EditableLabel` sets its idle text in the role's whole style (weight and leading included). |
| `TextInput::with_content_size(px)` / `set_content_size(Some(px))` | Editing text in place at a document's own size (a note on a zoomed page): never grown by the text scale. |
| `Typography::with_base_size(body_px)`, `typography.size(role)`, `ui::tokens::REFERENCE_BODY_PX` (14) | Theme authors and geometry only: the body size every role derives from, and a role's resolved size. Not for building a style. |
| `ui::TextScaling::{Renderer, Resolved, Content}` (field `TextStyle::scaling`) | What a style's size is relative to. Role styles are `Resolved`, `content(px)` is `Content`; `TextStyle::new(px)` is `Renderer`, kept for the few renderer-relative callers in the toolkit. |

### Replace

| Old | New |
|---|---|
| `Text::new(s).with_size(px)` / `.with_scaled_size(px)` / `.with_fixed_size(px)` | `Text::new(s).with_role(role)`; for content-sized text `.with_content_size(px)` |
| `Text::new(s).with_style(TextStyle::new(px).with_weight(w)…)` | `.with_role(role)` plus the modifiers that remain: `.strong()`, `.numeric()`, `.with_weight(w)`, `.with_italic(b)`, `.with_font_family(f)`, `.with_line_height(m)`, hyphenation and OpenType builders. Modifiers survive theme changes. |
| `Text::with_letter_spacing`, `with_tracking`, `with_small_caps` | Delete (no tracked or small-caps UI text). |
| `TextStyle::new(theme.tokens.typography.scale.sm)` (any `scale.xs`…`scale.x4l`) | `theme.text(role)` |
| `theme.tokens.typography.scale.<step>` used as a size number for layout | `theme.tokens.typography.size(role)`, or better a measured line: `ctx.line_box(role)` |
| `theme.tokens.typography.font_size_base` | `theme.tokens.typography.size(TextRole::Body)` |
| `typography.weights.{regular,medium,semibold,bold}` | `TextRole::weight()` via the role, or `.strong()` |
| `typography.line_height_{tight,normal,relaxed}` | The role's leading (in `theme.text(role)`); `with_line_height(m)` only for a genuine exception |
| `TypeScale { .. }` literal, `TypeScale::BASE`, `TypeScale::default()` | Delete; `Typography::with_base_size(px)` sets the base |
| `tr.renderer_input_style(&style)`, `text_metrics::renderer_type_size(tr, px)`, `size / theme.text_scale` | Delete; pass `theme.text(role)` straight to `measure_style` / `draw_style` / `elide_to_width_style` |
| `tr.draw_text(scene, s, px, color, t, w)` / `draw_text_weighted(…, px, …, weight)` | `tr.draw_style(scene, s, color, t, w, &theme.text(role))` or `ctx.text_role(s, role, color, origin, w)` |
| `tr.measure(s, px, w)` / `measure_weighted` / `measure_styled` | `tr.measure_style(s, w, &theme.text(role))` or `ctx.measure_role(s, role, w)` |
| `tr.elide_to_width(s, px, max)` / `elide_to_width_weighted` | `tr.elide_to_width_style(s, max, &theme.text(role))` |
| `tr.clamp_to_lines(s, px, max_w, n)` | `tr.clamp_to_lines(s, &theme.text(role), max_w, n)` |
| `TextInput::with_font_size(px)` / `set_font_size(px)` | `with_text_role(role)` / `set_text_role(role)` (default `Body`); a document's own size: `with_content_size(px)` |
| `TextInput::with_text_role(TextRole::Reading).with_font_family(FontChoice::Serif)` (and `Code` + `Monospace`) | `with_text_role(role)` alone: the role sets the family |
| `EditableLabel::with_text_role(role).with_font_family(..)` for a title | `with_text_role(role)`: the idle label draws and centres from `theme.text(role)` |
| `Autocomplete::with_font_size`, `Link::with_font_size`, `EditableLabel::with_font_size` / `set_font_size` | `with_text_role(role)` / `set_text_role(role)` |
| `RichTextLine::with_size(px)` / `set_size(px)` / `font_size()` | `with_text_role(role)` / `set_text_role(role)`; a block kind's role comes from `BlockKind` (paragraphs `Body`, H1 `LargeTitle`, H2 `Title`, H3+ `Heading`, code and tables `Code`) |
| `BlockEditor::font_size()` | `BlockEditor::text_role()` |
| `RichTextEditor::with_font_size(px)` | `with_text_role(TextRole::Body / Reading / Code)` |
| `CodeEditor::with_font_size(px)` | Delete; a code editor is `Code` |
| `ButtonFace::text_size(px).weight(w)`, `Button` label size | `.control_size(ControlSize::Small / Regular / Large)` |
| `SegmentedFace::text_size(px)` / `.weight(w)` | Delete; segments are `Label`, the selected one `.strong()` |
| `Chip::label_size_for_height(h, theme)` | Delete; chip labels are `Label` |
| `tooltip::chip_size(label, tr, px)` | `tooltip::chip_size(label, tr, theme)` (`Caption`) |
| `status_bubble::pill_size(text, tr, px, control)`, `pill_height(px, scale)`, `draw_pill(…, text, px)` | `pill_size(text, tr, theme, control)`, `pill_height(theme)`, `draw_pill(…, text)` (`Label`) |
| `name_field::fit(tr, s, px, budget)` / `fit_tail`, `NameField::draw(…, px)` | Same with `&TextStyle` in place of `px` |
| `data_table::type_line_height(px)` | `type_line_height(&theme.tokens.typography, role)` |
| `filters::filter_label_size(theme, tr)` | `filters::filter_label_style(theme)` |
| `"SECTION"` drawn as an overline (small, bold, tracked, upper case) | `"Section"` in `Subheading` |
| ui-graph `UiNode::Text { size: Some(px), .. }`, property `size` | `role: <role>` (`display`, `large-title`, `title`, `heading`, `subheading`, `body`, `label`, `caption`, `reading`, `code`); the `size` field and the `TextSize` editable property are gone |

### Delete

- Module constants holding a font size (`const FONT: f32 = 13.0`,
  `const TITLE_SIZE: f32 = …`): name the role where the text is made.
- Any arithmetic on a type size (`scale.sm - 1.0`, `size * 0.9`,
  `.max(11.0)`): a size between roles does not exist.
- `TextStyle::new(px)` for UI text. It remains only for renderer-relative
  toolkit internals and is counted by the `literal_font_size` ratchet.
- Uppercasing of labels (`.to_uppercase()`, `"SETTINGS"`).

### Text kinds

An app never sets an OpenType feature. Say what the text is where a widget
cannot know it; widgets that know their content (stat values, clocks, row
timestamps, table columns declared numeric, inputs by their `TextInputKind`)
set it themselves.

| Old | New |
|---|---|
| `.with_numeric_style(NumericStyle::TabularLining)`, `.with_font_feature(OpenTypeTag::TABULAR_NUMERALS, 1)` on times, counts, prices, sizes, progress | `.numeric()` (`TextKind::Numeric`) |
| Monospace or a hand-picked face for verification codes, TOTP, recovery codes, API keys, hashes, revealed passwords, Wi-Fi and licence keys | `.with_kind(TextKind::Secret)`; an input: `TextInputKind::Secret` (in the clear) or `Password` (revealed) |
| Usernames, handles, email addresses, invite codes, room names, other people's file names shown on their own | `.with_kind(TextKind::Identifier)`; an input: `TextInputKind::Username` / `Email` / `Path` |
| A URL or origin shown on its own | `.with_kind(TextKind::Url)`; an input: `TextInputKind::Url` |
| Hand-rolled link or email highlighting only to make an address legible | delete it: identifiers in prose are found and set apart by the toolkit |
| `.with_font_family(FontChoice::Monospace)` on source or terminal text | `.with_kind(TextKind::Code)`, or the `Code` role |
| A numeric `DataTable` column drawn by hand to line digits up | `ColumnDef::with_text(…).with_kind(TextKind::Numeric)` |
| `.with_font_feature(*b"ss01", 1)` or any stylistic set for a look | Delete; the face is the theme's |
| `.with_numeric_style(NumericStyle::ProportionalOldstyle)` on `Reading` text | Delete; the `Reading` role sets old-style proportional figures itself, and `.numeric()` turns them tabular lining |

### The UI face

The system UI face is Tilo Sans, which sets Latin, Vietnamese and Greek;
Cyrillic falls back to Onest, matched to its x-height and weight
([Script fallbacks](typography.md#script-fallbacks)). A theme or the
accessibility layer may choose the built-in `Hyperlegible` face instead. Code
never names a family: `TextRole` picks sans, serif (`Reading`) or mono
(`Code`), and `FontChoice::{SansSerif, Serif, Monospace}` remains for the rare
explicit choice.

### Reading faces

The `Reading` role is set in New Computer Modern, the way TeX sets it: the
toolkit picks its 8 pt, Book or Regular cut for the size each run is set at
(`docs/typography.md`, Reading faces). Noto Serif is gone; `FontChoice::Serif`
is New Computer Modern. A theme document's `type.reading` names the face, and
an app that lets a reader choose (Books) passes the chosen face by name:

| Old | New |
|---|---|
| `FontChoice::Named("Noto Serif".into())`, any serif family literal for long-form text | `theme.text(TextRole::Reading)` |
| A reader's serif choice stored as a family or file name | `ui::ReadingFace::name()` stored, `ReadingFace::from_name(stored)` read back; set with `.with_family(FontChoice::Named(face.family().to_owned()))` on the `Reading` style |
| `"Noto Serif"` / `"New Computer Modern Book"` / a cut's family in app code | Delete; name the face (`ReadingFace`), the toolkit picks the cut |
| `.with_small_caps(true)` on a book's opening words or a chapter label | Keep in document engines (Books, the word processor): small caps, ligatures and kerning reach every reading face through the shaping path |

The reading faces a theme or reader can choose are `ReadingFace::ALL`: New
Computer Modern (default), Libertinus, Literata, EB Garamond and Source Serif.
Books lists each as an entry of its font registry (`FontKind::Reading(name)`,
id `reading-<name>`), so the typography popover offers them beside the system
reading face, the UI face, mono and families the reader added.

## Ground & regions

A window has one ground per region and the framework paints it. The window
ground is `surfaces.content`: the native runner clears to it, the framework
root fills it under the view, a hosted tile fills it, and `--shots` captures
land on it. Regions are `Content`, `Sidebar` (navigation) and `Inspector`
(detail). An app declares regions and paints nothing underneath them.

### Removed APIs

`Theme::{bg_app, bg_primary, bg_hover, bg_active}` are gone. Map each use by
what the colour *did* at that line, first match wins:

| Old use | New |
|---|---|
| `bg_app` (or `surfaces.base`, `surfaces.content`, `window_card`, a literal) filling the whole window, the root widget or a whole pane: a `scene.fill` over the root/pane rect, `Container::with_fill(theme.bg_app)` around the root or a pane | Delete the fill (the whole `scene.fill` call, or the `.with_fill(..)`). |
| `bg_app` as the background a frame is rendered on (`OffscreenRenderer::render(&scene, theme.bg_app)`, `render_png(&scene, theme.bg_app, out)`, `ShotFrame::with_background(theme.bg_app)`) | `theme.tokens.colors.surfaces.content` |
| `bg_app` as "the ground this sits on" in a contrast, ink or edge measurement (`contrast_ratio(ink, theme.bg_app)`, `edge_on(theme.bg_app)`, `ink_on_any(&[theme.bg_app, ..])`) | The ground actually under it: `theme.tokens.colors.surfaces.content` on the window, `theme.region(Region::Sidebar)` / `theme.region(Region::Inspector)` in those columns. |
| `bg_app` cut out of a shape to show the ground (a keyhole, an outline stroke around a glyph) | The ground under it, as in the row above. |
| `bg_primary` filling a navigation column (folders, mailboxes, channels, collections, places, a library rail) | Delete the fill. With `NavShell`, pass the column to `.sidebar(..)` ([Navigation](#navigation)); in a painted app call `ui::surface::region(scene, transform, rect, Region::Sidebar, theme)` once for the column. |
| `bg_primary` filling a detail/inspector column (properties, metadata, an info panel beside the main view) | Delete the fill; `ui::surface::region(scene, transform, rect, Region::Inspector, theme)`. |
| `bg_primary` filling a header, toolbar or status band | Delete the fill. The band stands on its region; draw one hairline under it with `ui::surface::separator(scene, transform, x0, x1, band.y1, 0.0, ground)` where `ground` is the region's ground. |
| `bg_primary` as a card, field, well, key cap, chip, avatar ring or a button's rest face inside content | `theme.tokens.colors.surfaces.raised` (an avatar ring takes the ground it sits on instead). |
| `bg_hover` / `bg_active` as the hover / pressed (or open, focused, selected) fill of a control with a filled rest face | Use the toolkit control (`Button`, `Checkbox`, `Dropdown`, `TextInput`, `SegmentedControl`, `NavShell`, `Table`) and delete the painter. Where the painter stays: `colors.lift_on(rest, 0.5)` for hover, `colors.lift_on(rest, 1.0)` for pressed/open/selected, with `rest` the face's own fill (normally `surfaces.raised`). |
| `bg_hover` / `bg_active` under a transparent control or row (a ghost button, an icon button, a list row, a grid cell) on the window ground | `colors.intent.selection_hover` / `colors.intent.selection`. Draw the row with the region's theme (below) and the same tokens are right in a sidebar or inspector too. |
| the same inside a floating panel (menu, popover, picker) | `colors.lift_on(ui::chrome::shell::panel_ground(theme), 0.5)` / `lift_on(.., 1.0)`; a menu's armed row is the stronger overlay lift, see [Choosing the intent](#choosing-the-intent). |
| `bg_hover.with_alpha_factor(a)` as a wash (current-line band, gutter tint) | `colors.lift_on(ground, 0.5).with_alpha_factor(a)`, with `ground` the surface under the wash. |
| `Theme { bg_app, bg_primary, bg_hover, bg_active, .. }` struct literal | Delete the four fields; better, build the theme with `Theme::dark()` / `light()` and change tokens. |
| `theme.bg_app != other.bg_app` as "the theme changed" | `theme.tokens.colors.surfaces.content != other.tokens.colors.surfaces.content` |

### New API

| API | Use |
|---|---|
| `ui::surface::Region::{Content, Sidebar, Inspector}` | The region an area of the window is. |
| `ui::surface::region(scene, transform, rect, region, theme) -> Color` | Paint a region's ground over `rect` (plus the shared-edge hairline where the material gives region and content one ground) and return the ground. For painted apps; `NavShell` calls it for its sidebar. |
| `Theme::region(region) -> Color` | A region's ground, without painting (to measure against). |
| `Theme::in_region(region) -> Theme`, `Theme::on_ground(ground) -> Theme` | The theme for what is drawn on that ground: `surfaces.content` re-pointed to it, ink tiers re-measured, `intent.selection` / `selection_hover` stepped from it. On a ground of the other polarity (`media.letterbox` under a light theme) the inverted fill is re-derived too, so a `Primary` button over media stays legible. Pass it to the children's `draw`. |
| `Colors::wash_on(ground, Tone, WashStrength::{Rest, Selected}) -> Wash` | A status wash (an added line, a failed row) composited on its ground with `ink`, `ink_dim` and `mark` that read on it. `Selected` keeps the hue on the selection lift (`lift_on` alone drops it). Replaces app `WASH` alphas and `ChipStyle::Tinted(..).colors(..).fill` used as a row wash. |
| `colors.media.guide` | A composition guide over a live picture (thirds, a level line): translucent media ink. |
| `Text::tone(Tone)` | Status text ("Running", "Stopped", a person's name) in its tone's ink, resolved when drawn against the ground under it. Replaces `.with_color(semantic.success)`. |
| `ui::surface::region_needs_edge(ground, content)`, `ui::tokens::colors::REGION_MIN_STEP` | Whether a tier is too close to content to separate by tone (only the high-contrast material). |
| `theme.tokens.spacing.rhythm.{tight, related, sibling, section, group}` (`ui::tokens::RhythmScale`; `rhythm.get(Rhythm::Group)` by step) | Gaps by relationship (4 / 8 / 12 / 16 / 24 at 1×), scaled with `ui_scale` and a custom spacing. Containers take the step itself: `Column::with_rhythm(Rhythm::Group)`. |
| `FrameHarness::render_png` / `render_rgba` | The frame over the window ground; `--shots` and `--responsive-matrix` write this (see Window chrome & shots). |
| `ShotFrame::new(viewport, theme)` | Now clears to the window ground by default; delete a `.with_background(theme.bg_app)` / `.with_background(surfaces.content)`. |

### Choosing the region

- **Sidebar**: the column that navigates the rest of the window: folders,
  mailboxes, channels, playlists, collections, notebooks, settings
  categories. At most one per window, at the leading edge.
- **Inspector**: the column that shows properties or details of the
  selection beside the main view (file info, layer properties, contact
  details beside a conversation). At the trailing edge.
- **Content**: everything else, including a master list that *is* the
  content (the message list, the file grid, the track list) and the detail
  view that is the subject of the window (HIG §1c).

### What to delete in an app

1. The window's own background: the first `scene.fill` in the root
   widget's `draw` whose rect is the root rect, whatever colour it uses;
   `.with_fill(..)` on the root `Container`.
2. Each pane's own background fill (`bg_primary`, `surfaces.raised`,
   `surfaces.base`, `neutral[n]` or a literal over a whole column). Declare
   the region instead (table above).
3. Header and toolbar band fills; keep one `separator` hairline under the
   band.
4. A hand-drawn divider between a sidebar and content where the app now uses
   `NavShell` or `surface::region`: the tone separates them (and the region
   draws the line where it is needed).
5. App-local rhythm modules: `mod rhythm { TIGHT, RELATED, SIBLING, SECTION,
   GROUP }` built from `Spacing::BASE` (pdf-viewer, ebook). Delete the module
   and read `theme.tokens.spacing.rhythm.{tight, related, sibling, section,
   group}` where a `Theme` is in hand (`f64::from(..)` where an `f64` is
   needed). The values are the same at 1× and now follow `ui_scale`. A
   compile-time constant derived from the rhythm (`MENU_ROW_H = … +
   rhythm::TIGHT`) becomes a function of the theme.

### Painted app with regions

```rust
use ui::surface::{self, Region};

// No window fill: the framework painted surfaces.content already.
surface::region(scene, transform, sidebar_rect, Region::Sidebar, theme);
let sidebar_theme = theme.in_region(Region::Sidebar);
self.sidebar_rows.draw(scene, transform, text, &sidebar_theme, presence);

self.content.draw(scene, transform, text, theme, presence);

surface::region(scene, transform, inspector_rect, Region::Inspector, theme);
let inspector_theme = theme.in_region(Region::Inspector);
self.inspector.draw(scene, transform, text, &inspector_theme, presence);
```

## Navigation

Every sidebar in Tilo looks and behaves the same because there is one of
each: one window shell (`NavShell`), one navigation list (`NavList`), one row
painter for painted apps (`chrome::paint_nav_row`) and one selection painter
(`chrome::paint_row_selection`). `AppShell`, `NavSplit`, `PushNav` and
`SidebarNav` are gone. The shell and the list are semantic: there is no
width, breakpoint, colour, size, padding or radius to pass.

### Choosing the structure

Decide what each area of the window *is* (HIG §1c), then use the matching
slots. First match wins.

| The app is | Build |
|---|---|
| One canvas, document, map, camera or player with no navigation (HIG §1e) | No `NavShell`. Keep the view as the root. |
| Destinations + one content area (files, settings pages, notes, music library, feeds, containers, disks, firewall, backups) | `NavShell::new().sidebar(Box::new(nav_list)).content(content)` |
| Destinations + a list the person reads + the item (mail, calendar, contacts, logs, chat channels + messages) | `NavShell::new().sidebar(..).overview(list).content(detail)` |
| A list the person reads + the item, no destinations (weather, monitor processes) | `NavShell::new().overview(list).content(detail)` |
| A list of *categories* that, on a phone, should be its own first screen (Settings, Shortcuts, Themes) | add `.mode(NavMode::Push)` |
| Workspaces / channels / sections shown as an icon rail beside content (tube, remote, chat workspaces, weather's content rail) | `.sidebar(Box::new(NavList::new(..).rail())).mode(NavMode::Docked)` |
| Properties of the selection beside the main view (file info, layer properties, contact details beside a conversation) | add `.inspector(panel)` |

The default `NavMode::Overlay` is right for every other sidebar: below the
width where the content would be squeezed it becomes a floating drawer.

### Removed APIs and their replacements

`AppShell` (settings, shortcuts, themes):

| Old | New |
|---|---|
| `AppShell::new(nav, content)` | `NavShell::new().mode(NavMode::Push).sidebar(nav).content(content)` |
| `.with_search(field)` | Delete it. A filter of the sidebar's list goes in `.with_filter(field)` (the field is a `SearchField` in filter mode, see [Search](#search)); searching the app's content is the platform search. |
| `.with_sidebar_width(..)`, `.with_breakpoint(..)`, `.with_width_hint(..)` | Delete. |
| `.with_narrow_screen(NarrowScreen::List)` / `Content` | Delete; the narrow window opens on the first screen. To open elsewhere call `shell.show(NavPane::Content)`. |
| `.with_narrow_screen_state(&LocalState<NarrowScreen>)` | `.with_pane_state(&LocalState<NavPane>)`; `NarrowScreen::List` → `NavPane::Sidebar`, `NarrowScreen::Content` → `NavPane::Content`. |
| `.with_id(id)`, `.with_back_accessible_name(..)` | `.with_id(id)`; delete the Back name (the shell names it "Back"). |
| `shell.notify_selection()` | Delete when the sidebar is a `NavList` (choosing a destination pushes by itself); otherwise `shell.show(NavPane::Content)`. |
| `shell.back()` | `shell.back()` |
| `shell.set_nav(nav)`, `shell.set_content(c)` | `shell.set_sidebar(Some(nav))`, `shell.set_content(c)` |
| `shell.is_narrow()`, `shell.layout()`, `ShellLayout` | `!shell.geometry().stack.is_empty()`; usually delete the branch. |
| `shell.narrow_screen()` | `shell.front_pane()` |
| `shell.sidebar_open()` | `shell.is_sidebar_open()` |
| `shell.back_rect()`, `sidebar_panel_rect()`, `content_area_rect()` | `shell.geometry().back`, `.sidebar_column`, `.content_column`; only tests read these. |
| `ui::AppShell`, `ui::NarrowScreen`, `ui::ShellLayout`, `DEFAULT_SHELL_SIDEBAR_WIDTH`, `DEFAULT_SHELL_BREAKPOINT` | Delete the imports. |

`NavSplit` (containers, disks, feeds, tube):

| Old | New |
|---|---|
| `NavSplit::new(sidebar, content)` | `NavShell::new().sidebar(sidebar).content(content)` |
| `.with_mode(SplitMode::Adaptive)` | Delete (the default). |
| `.with_mode(SplitMode::Push)` with `show_detail()` / `show_master()` | The two children are an overview and a detail: `NavShell::new().overview(list).content(detail)`, then `shell.show(NavPane::Content)` / `shell.show(NavPane::Overview)`. A `NavList` overview pushes by itself. |
| `.with_mode(SplitMode::AlwaysDocked)` | An icon rail: `.mode(NavMode::Docked)` with `NavList::rail()`. A text sidebar always docks only where it fits; delete the mode. |
| `.with_breakpoint(..)`, `.with_sidebar_width(..)`, `.with_icon_sidebar_width(..)` | Delete. |
| `.with_sidebar_closed()` | `.with_sidebar_hidden()` |
| `toggle_sidebar()`, `open_sidebar()`, `close_sidebar()`, `is_sidebar_open()` | The same names on `NavShell`. |
| `layout_mode()`, `SplitLayout::Docked` / `Overlay` | `shell.geometry().sidebar` (`SidebarPlacement::Docked` / `Drawer`). |
| `push_pane()`, `PushPane::Master` / `Detail` | `shell.front_pane()`, `NavPane::Overview` / `NavPane::Content`. |
| `snap_open(open)` | `shell.open_sidebar()` / `close_sidebar()` then `shell.snap()` (shots and tests only). |
| `sidebar_rect()`, `content_panel_rect()`, `flap()` | `shell.geometry().sidebar_column` / `.content_column`; delete `flap()` uses. |
| `DEFAULT_SIDEBAR_WIDTH`, `DEFAULT_BREAKPOINT`, `SplitMode` | Delete. |

`PushNav` (contacts, logs, monitor):

| Old | New |
|---|---|
| A painted app holding `PushNav` and positioning its own list and detail from `push.slide()` | Split the painted list and the painted detail into two widgets (each owns its rows, hit-testing and paint) and give them to `NavShell::new().overview(list).content(detail)`. Delete the `PushNav` field, the app's breakpoint (`PUSH_BREAKPOINT`, `layout_at`), the slide arithmetic, the Back button painter and its hit-test. |
| `push.show_detail()` / `push.show_master()` | `shell.show(NavPane::Content)` / `shell.show(NavPane::Overview)` (or bind `.with_pane_state(&state)` and set the state). |
| `push.pane() == PushPane::Detail` | `shell.front_pane() == NavPane::Content` |
| `push.tick(reduce_motion)`, `push.is_animating()`, `push.snap(..)` | Delete; the shell animates (motion tokens) and snaps for shots. |
| `PushPane` | `NavPane` |

The previous `NavShell` API (file-explorer, mail, maps, weather):

| Old | New |
|---|---|
| `NavShell::new(overview, detail)` | `NavShell::new().overview(overview).content(detail)` |
| `NavShell::sidebar_over_content(sidebar, content)` | `NavShell::new().sidebar(sidebar).content(content)` |
| `.with_sidebar(s)` | `.sidebar(s)` |
| `.with_breakpoint(..)`, `.set_breakpoint(..)`, `.with_sidebar_width(..)`, `.with_icon_sidebar_width(..)`, `.with_narrow_sidebar_width(..)`, `.with_overview_width(..)`, `.with_icon_overview_width(..)` | Delete. An icon-width sidebar is a rail: `.mode(NavMode::Docked)` with `NavList::rail()`. |
| `.hide_toggle_when_docked()` | Delete. The toggle is always there; a person hides a sidebar the same way in every app. |
| `.with_sidebar_collapsed()` | `.with_sidebar_hidden()` |
| `set_overview(e)`, `set_detail(e)`, `set_content(e)`, `set_sidebar(o)` | `set_overview(Some(e))`, `set_content(e)`, `set_content(e)`, `set_sidebar(o)` |
| `show_detail()`, `show_overview()` / `show_master()`, `hint_active_pane(p)` | `show(NavPane::Content)`, `show(NavPane::Overview)`; delete `hint_active_pane` (the shell tracks the pane the person works in). |
| `snap_sidebar(open)`, `snap_pane(p)` | `open_sidebar()`/`close_sidebar()` or `show(p)`, then `snap()`. |
| `mode()`, `ShellMode`, `content_layout()`, `ContentLayout`, `sidebar_layout()`, `top_left_affordance()`, `Affordance` | `shell.geometry()` (`sidebar`, `side_by_side`, `stack`, `toggle`, `back`). |
| `pane()`, `is_detail_shown()` | `front_pane()`, `front_pane() == NavPane::Content` |
| `breakpoint()`, `sidebar_width()`, `overview_width()` | `geometry().breakpoint`, `geometry().sidebar_column.width()`, `geometry().overview_column.width()` |
| `sidebar_rect()`, `overview_rect()`, `detail_rect()`, `scrim_rect()`, `collapse_button_rect()`, `back_button_rect()` | `geometry().sidebar_column`, `.overview_column`, `.content_column`, `.scrim`, `.toggle`, `.back` |
| `sidebar_content_rect()`, `overview_content_rect()`, `detail_content_rect()` | Delete the reads: children are laid out under the header row already. |
| A header band drawn at the top of a pane (title + buttons), a `CHIP_CLEARANCE` indent before the title, a title indented past the chip | `.sidebar_header(title, actions)`, `.overview_header(..)`, `.content_header(..)`, `.inspector_header(..)`; delete the band, its fill, its hairline and the indent. Change a title in place with `set_content_title` / `set_overview_title`. |
| `DEFAULT_NAVSHELL_*` | Delete. |

`SidebarNav` (passwords) and hand-painted sidebar rows:

| Old | New |
|---|---|
| `SidebarNav::new(rect, sections).render(..)`, `.hit_test(p)`, `.accessibility(..)` | In a widget app: a `NavList` in `NavShell::sidebar`. In a painted app: `chrome::paint_nav_section` + `chrome::paint_nav_row` per row, hit-tested with `chrome::nav_row_layout` / `nav_section_layout`. |
| `SidebarNavSection::new(label, items)` | `NavSection::new(id, label, items)` (sentence-case label), `NavSection::untitled(id, items)` for no header. |
| `SidebarNavItem::new(id, label).badge(text).selected(b)` | `NavItem::new(id, label)` with `.count(n)` or `.live_count(n)`; selection is `NavList::selected(id)` / `bind_selection`. |

`SidebarDrawer` (painted apps) keeps its API with these changes:

| Old | New |
|---|---|
| `drawer.tick(reduce_motion)` | `drawer.tick(theme)` (takes the motion duration and reduced motion from the theme). |
| `drawer.draw_scrim(scene, transform)` | `drawer.draw_scrim(scene, transform, theme)` (`surfaces.scrim`, no literal black, no stepped shadow bands). |
| Filling the sidebar rect yourself (`surfaces.sidebar`, `raised`, `bg_primary`, a literal), or `surface::region(.., Region::Sidebar, ..)` under a drawer | `let sidebar_theme = drawer.paint_panel(scene, transform, theme);` and draw the drawer's contents with `sidebar_theme`. Docked it is the region tier with no radius; as an overlay it is the floating material with `radii.panel` and the floating shadow, floating `s2` inside the window. |
| `drawer.draw_toggle(scene, transform, theme, rect, hovered)` | `drawer.draw_toggle(scene, transform, text, theme, rect, hovered)`: the same Plain `Icon::PanelLeft` button `NavShell` draws. Put it at the top-leading corner (`rect` = `min_pointer_target` square in an `s3` inset). |
| Default docked width 320, default breakpoint 760 | 280 (`sidebar_width`) and 840 (`compact_breakpoint`, the sidebar plus the 560 the content needs); delete `.docked_width(..)` / `.breakpoint(..)` calls that restate the old defaults. |
| A collapse that the drawer forgot after the window crossed the breakpoint | Remembered per size class now; delete app code that re-applies it. |
| An app holding a `FlapState` for its own sidebar or side panel, with its own breakpoint, scrim and slide arithmetic | A `SidebarDrawer` (`.right()` for a trailing panel). `FlapState::tick(layout, dt)` is now `tick(layout, dt, theme.tokens.motion.durations.base())` and `slide()` is gone: `reveal()` is already eased. |
| A hand-placed toggle rect, its hover tracking, the sidebar shortcut and the toggle's semantic node | `let face = drawer.header(area, theme);` (`header_with_history(area, theme, opener.is_shown())` with History): the window's `HeaderRowFace` with the toggle in it. Route events with `drawer.handle_event(event, ctx)` (hit-test, hover, `HostShortcut::Sidebar`, Escape and a scrim press), paint with `drawer.paint_toggle(..)`, publish `drawer.toggle_accessibility()` and route its `Activate` with `perform_toggle_action`. The floating drawer opens under the row. |
| `.breakpoint(f64::INFINITY)` | `.overlay_only()`. |
| A `snap` re-applied after the first `resolve` | Delete: a snap before the first `resolve` waits for the layout. |
| Two drawers side by side for an icon rail and a list | `.with_rail()`: one drawer, `rail_rect()` and `list_rect()`. |
| A floating panel painted by hand at the host's own rect | `SidebarDrawer::paint_panel_in(scene, transform, rect, theme) -> Theme`. |

`TreeView`:

| Old | New |
|---|---|
| `.inset_rows()` | Delete; every tree insets its rows `s2`, so the depth-0 caret is never on the pane edge. |
| `.with_trailing_edge()` | Delete; regions are told apart by tone (`NavShell`, `surface::region`). |
| The current row's accent rail (`set_current`) | Unchanged call; the current row is now its label in the heavier weight. |
| A tree in a sidebar listing destinations (code-editor explorer, db-client schema, notes pages) | A list of places, nested or not, is a `NavList`: nested items, rename, drag into and between items, Delete, row menus and per-row semantic keys are all its own. Keep `TreeView` only for a hierarchy the list cannot hold (a file tree that loads its children lazily); it is a `NavShell::sidebar` child and paints the same lift. |
| `.with_ground(|t| t.tokens.colors.surfaces.sidebar)` (or another region tier) | Delete: the tree stands on the ground of the region its container hands it. Keep `with_ground` only for a tree on a raised well. |

Selection forks:

| Old | New |
|---|---|
| A private `selected_on(ground)`, `row_tier_on(ground, state)`, `selection_fill(theme, ground)`, `row_fill(..)`, `selection_fill_on_raised(theme)`, a `scene.fill` of `intent.selection` / `lift_on(..)` / `selection_tier(..).fill` under a row | `chrome::paint_row_selection(scene, transform, row.inset((-s2, -hairline)), SelectionState::new(selected, hovered, pressed), ground, theme)` where `ground` is the surface under the list (`theme.tokens.colors.surfaces.content` of the theme you draw the list with). Delete the private function. |
| A control with its own rest face whose states are `colors.lift(floor_ramp(state))` (ebook `floor_ramp`) | `chrome::selection_floor_ramp(theme, state, ground)`; delete `floor_ramp` and its constants. |
| An accent bar, left stripe, outline or coloured fill on the selected row | Delete it; the lift is the whole selection. |

### Rows: what each part of a `NavItem` means

A sidebar row is a `NavItem` (in a painted app, a `NavRowFace` with the same
builder names; both paint through `chrome::paint_nav_row`). A `ListRow` is a
row of content the person reads, never a sidebar row: when a sidebar needs a
part that is not here, report the gap instead of reaching for `ListRow`.

Leading to trailing:

- **Check**: `.checked(on)` with `NavList::on_check(|id, on| ..)`: a checkbox
  in the leading slot for a place the person shows or hides (a calendar).
  The whole row is its target (a click anywhere, Space, Enter); a checkable
  row is not a destination and is never selected.
- **Icon**: one `ui::icons` icon per row, or none for the whole section. Use
  the same icon family as the app's other chrome. Never an emoji or a
  coloured dot.
- **Swatch**: `.swatch(color)` in the icon's place for the person's own
  content colour (a calendar's, a project's). Never a status or a decoration.
- **Badge**: `.badge("GET")` for a short leading code that classifies the row
  (an HTTP method, a file kind). The codes of one section share a column, so
  their labels line up.
- **Label**: sentence case, what the destination is called. It elides; it
  never wraps (a rail's caption wraps to a second line, then elides). The
  selected row's label is set in the heavier weight; an elided label shows
  in full as the row's tooltip.
- **Secondary**: `.secondary("db.example.net:5432")`: one `Caption` line
  under the label (a host and port, a schedule). It elides; the row grows to
  hold it.
- **Level**: `.level(fraction)`: a usage bar under the text, on the progress
  bar's track and fill (a backup destination's space).
- **Presence**: `.presence(identity)`, once per person: a mark in their
  presence colour for someone on that place (a page a collaborator has open).
- **Status label**: `.status_label("Active", Tone::Success)`: a quiet tinted
  badge naming the row's state (a firewall zone).
- **Meta**: `.meta("1.2 TB")`: a tertiary figure at the trailing edge (a size).
- **Count**: `.count(n)` for how many items a place holds (messages in a
  folder, tracks in a playlist) — quiet tabular figures in tertiary ink.
  `.live_count(n)` only for things waiting for the person (unread, mentions,
  failed jobs): the one filled badge, in the person's own colour. Never a
  filled badge for a plain count, never a status colour.
- **Status**: `.status(RowStatus::Active / Warning / Error / Idle)`: the dot
  at the trailing edge (a drive's health, a connection's state, an account
  that needs attention); `RowStatus::Presence(id)` for a person.
- **Children**: nested places (`.children(..)`), indented one `s4` step, with
  a trailing chevron. `.expanded()` to start open; `.ensure_expanded()` to
  open whatever the person folded (the database a connection just opened).
  `NavList::on_expand(|id, open| ..)` reports what the person folds so the
  app can keep it; `set_expanded(id, open)` changes it without a report.
- **Actions**: actions that apply to every row (rename, more) go in
  `NavList::item_actions(..)`; they show on hover and focus at the trailing
  edge, in place of the trailing parts. `.without_actions()` on rows where
  none apply (Inbox, Trash). The row's More is
  `NavAction::new("more", Icon::MoreHorizontal, "More").opens_context_menu()`.
- **Menu**: `NavList::on_context_menu(|id, lift, at| ..)` is called for a
  right click, a long press, the Menu key (Shift+F10) and a More action that
  opens the menu. Open the app's `ContextMenu` there:
  `menu.anchor_row(lift); menu.open_at([at.x as f32, at.y as f32])`. Outside
  the callback, `row_lift_rect(id)` and `item_action_rect(id, "more")` give
  the same geometry; never hit-test the list's rows yourself.
- **Section**: `NavSection::new(id, "Sentence case title", items)`; never
  upper case or letter-spaced. `.collapsible()` when the person may fold it,
  `.collapsed()` to start folded,
  `.trailing_action(NavAction::new("new", Icon::Plus, "New playlist"))` for
  the one "new …" action of the section, `.empty_caption("No drafts")` for
  what it says while empty, `.filterable()` when the window's filter
  (`NavShell::with_filter`) narrows it. A section that does not opt in (the
  connections above the tables) stays whole while the person filters. An
  elided title shows in full as its tooltip.
- **Editing**: `.editable_label()` plus `NavList::on_rename`; `.drop_target()`
  plus `NavList::on_move` for places that hold other places;
  `NavList::on_delete(|id| ..)` for the Delete key (never Backspace).
  `NavList::stage_drag(id, before)` shows a drag in progress for a capture.
- **Addressing**: `.semantic_key(key)` when agents and tests address the row
  by the app's own id (a page); the default is `nav:item:{id}`.
- **Selection**: `set_selected`, `.selected` and `bind_selection` bring the
  chosen row into view, also before the list's first layout.

### New API

| API | Use |
|---|---|
| `NavShell::new()` with `.sidebar(e)`, `.overview(e)`, `.content(e)`, `.inspector(e)` | The window's columns. |
| `.sidebar_header(title, actions)`, `.overview_header(..)`, `.content_header(..)`, `.inspector_header(..)` | Each column's header in the one header row: a sentence-case title and the column's actions (`Button`s, a `ToggleGroup`). The title starts at the content gutter (after the toggle or Back when the column holds one); the actions start after the title's measured width. Labelled buttons collapse to their icons when the row is short, then the rest move into a trailing More menu built from their names; nothing to configure. |
| `set_sidebar_header(title, actions)`, `set_overview_header(..)`, `set_content_header(..)`, `set_inspector_header(..)` | Change a header in place. |
| `set_sidebar_title`, `set_overview_title`, `set_content_title`, `set_inspector_title` | Change only the title. |
| `.content_header_bound(&LocalState<String>, actions)` | The content's title follows a `LocalState` a view writes from resource state (as `NavList::bind_selection`). |
| `.sidebar_count(n)`, `.overview_count(n)`, `.content_count(n)`; `set_*_count(Option<usize>)` | How many items the column holds, a quiet `Caption.numeric` pill after the title (`SectionHeader::count`). |
| `.content_header_leading(e)`, `set_content_header_leading(Option<e>)` | The fill between the toggle or Back and the actions: a path bar or breadcrumb, a mode or tab switcher with no title. It stands at its own width and gets what is left when it needs more. |
| `.with_back(&LocalState<NavStack<T>>)`; `.on_back(can_go_back, go_back)` + `set_can_go_back(bool)` | The content's Back for the app's own page stack (browse history, a drill-down from a dashboard). The shell draws its `ChevronLeft` chip at the leading edge of the content's header and pops the stack on a press; a narrow window's push stack shares the chip. |
| `.overview_controls(e)`, `set_overview_controls(Option<e>)` | Controls that act on the overview list (its sort order), under the overview's header at the shell inset. |
| `.overview_rail(e)` | A `NavList::rail()` the overview collapses into where the list no longer fits beside the content but the rail does; narrower still, the overview pushes. |
| `sidebar_mut::<T>()`, `overview_mut::<T>()`, `content_mut::<T>()`, `inspector_mut::<T>()` | The column as the widget the app gave it (an explorer, an editor, a tree), the one waiting to go in if a swap is pending. |
| `.with_sidebar_state(&LocalState<bool>)`, `.with_inspector_state(..)`; `open_inspector()`, `close_inspector()` | Show or hide from state (`ViewCx::on_stage`, a scenario opening a drawer for a shot); the shell writes the person's choice back. |
| `.with_filter(SearchField::filter())`, `filter()`, `filter_mut()` | The one filter field of the window, under the sidebar's header (the overview's without a sidebar, else the content's); Ctrl+F / ⌘F focuses it. The shell carries its query to the column's `Filterable` after every event (`sync_filter`); read the query and the count back from `filter()`. |
| `.mode(NavMode::{Overlay, Push, Docked})` | What the sidebar becomes in a narrow window. |
| `.with_pane_state(&LocalState<NavPane>)`, `show(NavPane)`, `back()`, `front_pane()` | The front screen of a narrow window's push stack. |
| `toggle_sidebar()`, `open_sidebar()`, `close_sidebar()`, `is_sidebar_open()`, `with_sidebar_hidden()`; the same for the inspector | Show and hide; remembered per window size class. |
| `geometry()` (`NavGeometry`) | What the shell laid out, for tests and shots (`overview` is `OverviewPlacement::{None, List, Rail}`). |
| Changing a slot or header from an event | Safe: `set_content`, `set_inspector`, `set_*_header`, … take effect when the host next builds the tree, never while it routes the event. |
| An open drawer | Opens under the header row, which stays live (the toggle, History, the columns' header actions); the other columns stay laid out and painted under the scrim, and take no input. |
| `chrome::HeaderRowFace::new(area, theme)` with `.with_toggle()`, `.with_back()`, `.with_history()`, `.with_history_if(shown)`, `.with_trailing()`, `.without_rule()`, `.with_inset_title()`; `row`, `toggle`, `back`, `title`, `actions`, `history`, `rule`, `split(title_width)`, `body_of(area)`, `history_rect()`, `paint_rule(..)`, `paint_title(..)`; `HeaderRowMetrics` | The header row's geometry for a painted app: the same face `NavShell` lays its row out with. |
| `widget::WindowHeader` (`set_history_control(Option<HistoryControl>)`), `widget::HistoryOpener`, `chrome::paint_history_control(..)` | A painted app's header row takes the window's History control: provide `WindowHeader`, keep a `HistoryOpener` (`set_control`, `handle_event(event, face.history_rect(), scale)`, `paint`, `accessibility(rect)`, `perform_action`), and return `opener.take()` from `ActionControls::take_control_invocation`. A header row that hosts library controls hosts `opener.button_mut()` instead (the same History button, bound to the History door, at `widget::HISTORY_CONTROL_ID`; `take_rebuilt` / `tree_rebuilt` when it is replaced). The window then draws no History bar above the app. |
| `NavList::new(sections)` with `NavSection`, `NavItem`, `NavAction`, `NavDrop` | The one navigation list (keyboard, disclosure, semantics, rename, drag, row actions). |
| `.selected(id)`, `.bind_selection(&LocalState<Option<String>>)`, `.on_select(..)`, `set_sections(..)`, `set_selected(..)`, `selected_id()` | Selection and updates. Choosing a destination in a narrow window closes the drawer or pushes the content by itself. |
| `.item_actions(actions, on_action)`, `.on_section_action(..)`, `.on_rename(..)`, `.on_move(..)` | What the person can do to rows. |
| `NavItem::{secondary, meta, status, status_label, badge, swatch, checked, level, presence, ensure_expanded, semantic_key}` | The parts of a row ([Rows](#rows-what-each-part-of-a-navitem-means)). |
| `NavSection::{collapsed, empty_caption, filterable}` | A section that starts folded, says when it is empty, and narrows under the window's filter. |
| `.on_check(..)`, `.on_expand(..)`, `.on_delete(..)`, `.on_context_menu(..)`, `NavAction::opens_context_menu()` | Checks, folding, Delete and row menus. |
| `set_expanded(id, open)`, `is_expanded(id)`, `row_lift_rect(id)`, `item_action_rect(id, action)`, `stage_drag(id, before)`, `set_filter(query)` | Folding without a report, menu geometry, a staged drag for captures, filtering by hand. |
| `.rail()` | Icon over caption, for a docked rail; captions wrap to two lines before they elide. |
| `chrome::paint_nav_row(scene, transform, text, theme, &NavRowFace)`, `chrome::nav_row_layout`, `nav_row_height(theme)`, `nav_rail_row_height(theme)` | One navigation row in a painted app, identical to a `NavList` row. |
| `NavRowFace::{secondary, meta, status, status_label, badge, badge_column, swatch, checked, level, presence}`, `face.height(theme)` / `chrome::nav_item_height(theme, secondary, level)`, `chrome::nav_badge_column(theme, text, codes)`, `chrome::nav_rail_item_height(..)` | The same parts in a painted row: size the row with `height`, share a section's badge column. |
| `chrome::paint_nav_section(.., &NavSectionFace)`, `nav_section_layout`, `nav_section_height(theme, first)` | One section header in a painted app. |
| `chrome::paint_row_selection(scene, transform, rect, state, ground, theme)` | The hover and selection lift of any list row. |
| `chrome::selection_floor_ramp(theme, state, ground)` | States of a control with its own rest face. |
| `chrome::selection_tier_on(theme, state, ground)` | Now one surface step of `ground` (`SELECTION_MIN_STEP`, ΔL 0.06, on every ground). |
| `SidebarDrawer::paint_panel(scene, transform, theme) -> Theme` | A painted app's sidebar or inspector panel, docked or floating. |
| `HostShortcut::Sidebar` | Ctrl+Shift+L / ⌘⇧L: show or hide the sidebar in every app. |
| The window's History control in the header row | Put there by the host where the app has History: `Icon::History` at the trailing edge, on while History shows. `NavGeometry::history` is its rect. |
| `Button::toggled(on)`, `set_toggled(on)`, `is_toggled()`; `ButtonFace::toggled(on)` | A toggle button: on, a Plain glyph stands on the selection lift and is announced as a pressed toggle. |
| `Icon::PanelLeft`, `Icon::PanelRight` | The sidebar and inspector toggles. |
| `theme.tokens.metrics.{header_height, nav_row_height, rail_width, inspector_width, sidebar_min_width, sidebar_max_width, content_min_width, detail_min_width}` | The shell metrics (HIG §9.6). |

### What to delete in an app

1. The sidebar's background fill, its trailing divider, and any hairline the
   app draws between columns: the shell paints the regions and the rules.
2. Every header band above a column: its fill, its title painter, its
   hairline, the indent that cleared the old chip. Move the title and the
   buttons into the column's `*_header(..)`.
3. Collapse chips, hamburger buttons, `X` close buttons on sidebars, Back
   buttons on narrow screens, their hit-tests and their geometry constants.
4. Drawer scrims (`Color::from_rgba8(0, 0, 0, ..)`, stepped shadow bands),
   drawer slide arithmetic, app-owned breakpoints (`BREAKPOINT`,
   `NAV_BREAKPOINT`, `PUSH_BREAKPOINT`, `layout_at`), sidebar and overview
   width constants (`SIDEBAR_W`, `SIDEBAR_WIDTH`, `OVERVIEW_W`), and the
   `× text_scale` multiplications applied to them.
5. App-drawn sidebar rows: row height and padding constants, icon
   placement, selection and hover fills, accent bars, count badges, section
   header painters (and their upper-case labels), and the row hit-testing.
6. Private selection functions (`selected_on`, `row_tier_on`,
   `selection_fill`, `row_fill`, `selection_fill_on_raised`, `floor_ramp`).
7. App-owned search fields at the top of a sidebar (see
   [Search](#search)); a filter of the list becomes `with_filter`.
8. `std::mem::take(&mut shell).content_header(..)` rebuilds, `Decorator`s
   that redraw a title or a count over a header, and app-placed Back
   buttons (`ControlSize::Small` "Back"): use `set_content_header`,
   `set_content_title`, `set_*_count` and `with_back`.

### Example: a mail window

```rust
use ui::icons::Icon;
use ui::widget::{NavAction, NavItem, NavList, NavSection, NavShell};

let mailboxes = NavList::new(vec![
    NavSection::untitled("favourites", vec![
        NavItem::new("inbox", "Inbox").icon(Icon::Inbox).live_count(unread),
        NavItem::new("drafts", "Drafts").icon(Icon::FileText).count(drafts),
        NavItem::new("sent", "Sent").icon(Icon::Send),
    ]),
    NavSection::new("folders", "Folders", folder_items)
        .collapsible()
        .trailing_action(NavAction::new("new-folder", Icon::Plus, "New folder")),
])
.bind_selection(&ui.mailbox)
.with_accessible_name("Mailboxes");

let shell = NavShell::new()
    .sidebar(Box::new(mailboxes))
    .overview(Box::new(message_list))
    .content(Box::new(message_view))
    .overview_header("Inbox", vec![compose_button])
    .content_header(subject, vec![reply_button, delete_button]);
```

### Example: a painted app

```rust
use ui::chrome::{self, NavCount, NavRowFace, NavSectionFace, SelectionState};

// In the draw path, after the content:
self.drawer.tick(theme);
let content = self.drawer.resolve(rect);
self.draw_content(scene, transform, text, theme, content);
self.drawer.draw_scrim(scene, transform, theme);
let local = self.drawer.paint_panel(scene, transform, theme);
let row_h = chrome::nav_row_height(&local);
let mut y = self.drawer.sidebar_rect().y0 + f64::from(local.tokens.metrics.header_height);
for row in &self.rows {
    let rect = Rect::new(self.drawer.sidebar_rect().x0, y, self.drawer.sidebar_rect().x1, y + row_h);
    chrome::paint_nav_row(scene, transform, text, &local, &NavRowFace::new(rect, &row.label)
        .icon(row.icon)
        .count(NavCount::Neutral(row.count))
        .state(SelectionState::new(row.id == self.selected, Some(row.id) == self.hovered, false)));
    y += row_h;
}
```

## Controls

A control takes what it is and how big it is, never how it looks. Buttons
take a `ButtonRole` and a `ControlSize`; segmented strips a `SegmentedStyle`
and a `ControlSize`; chips and badges a `ChipStyle`. Colour, radius, height,
padding and label size come from the theme. Apply the steps below in order.

### Choosing a button role

| The button… | Role |
|---|---|
| is the one main action of the view or sheet ("Save", "Send", "Create") | `ButtonRole::Primary` (at most one per view) |
| deletes, removes, revokes, leaves or discards something | `ButtonRole::Destructive` |
| is any other action ("Cancel", "Edit", "Add account") | `ButtonRole::Secondary` (the default) |
| is an inline text action, a toolbar glyph, a close or overflow glyph, a link-like action in a row | `ButtonRole::Plain` |

A destructive button is a neutral control lettered in danger ink. The filled
danger face exists only in the confirmation step, and the toolkit gives it
there: `ModalDialog::with_destructive(true)` and a painted modal action made with
`ModalAction::danger(label)`. An app never asks for a filled danger button.
As on macOS, Return in that confirmation is the safe answer: Enter fires
Cancel, the sheet opens with the keyboard on Cancel, and the destructive
action takes a click, or Tab and Space. In any sheet, Space operates the
focused control (a switch, a check box) and Enter is the sheet's default
answer unless the focused control reads Enter itself (a multiline field, an
open list, a focused button).

### Choosing a size

| Old height | `ControlSize` |
|---|---|
| ≤ 30 (toolbars, dense rows, inline row actions) | `Small` (28) |
| 31–38, or the old default (40) | `Regular` (32), the default: write nothing |
| the one prominent action of onboarding, a lock or login screen, a sheet's confirmation | `Large` (40) |

### Buttons: replace

| Old | New |
|---|---|
| `ButtonStyle` (type) | `ButtonRole` |
| `.primary()` | `.role(ButtonRole::Primary)` |
| `.danger()`, `.danger_outline()` | `.role(ButtonRole::Destructive)` |
| `.ghost()` | `.role(ButtonRole::Plain)` |
| `.with_style(ButtonStyle::Neutral)` / `.style(ButtonStyle::Neutral)` | delete (Secondary is the default) |
| `.with_style(ButtonStyle::X)`, `ButtonFace::style(ButtonStyle::X)`, `set_style` | `.role(ButtonRole::Y)`, `set_role` (Neutral → Secondary, Primary → Primary, Danger / DangerOutline → Destructive, Ghost → Plain) |
| `.fit_content(h)`, `.content_sized()` | delete; a button is as wide as its label. `h ≤ 30` → `.control_size(ControlSize::Small)` |
| `.with_size(w, h)` | delete; for a button that should span its column `.fill_width()`; `h ≤ 30` → `ControlSize::Small` |
| `.fill_width(h)` | `.fill_width()` |
| `.with_min_height(h)` | delete; pick the `ControlSize` |
| `ButtonFace::radius(r)`, `ButtonFace::wrap_label()` | delete |
| `Button::new(id, "").with_icon(i).with_accessible_name(n)` (+ `.tooltip(n)`, `.ghost()`, `.with_size(s, s)`) | `Button::icon_only(id, i, n)` (square, named, tooltip, Plain) |
| `ButtonFace::new(r, "").icon(i).accessible_name(n)` | `ButtonFace::icon_only(r, i, n)` |
| a label with a shortcut glued on ("Run ⌘↵", "Send (Ctrl+Enter)") | the label alone plus `.shortcut_hint(Shortcut::command("Enter"))`; the hint is spelled in the platform's keycaps, never `⌘` off macOS |
| `UndoButton::with_style(s)`, `UndoButton::with_size(w, h)` | `UndoButton::role(r)`, `UndoButton::control_size(size)` |
| ui-graph `UiButtonStyle::{Neutral, Primary, Danger, Ghost}`, field `style` | `UiButtonRole::{Secondary, Primary, Destructive, Plain}`, field `role` |

### Buttons: hand-drawn patterns

| Hand-drawn | Replace with |
|---|---|
| `draw_*_button(...)` + `rect.contains(point)` in a retained view | a `Button` (role, size, `on_click` / `on_action`) |
| a painted button in a canvas app | `Button::paint(scene, t, text, theme, &ButtonFace::new(rect, label).role(..).hovered(..).pressed(..).focused(..).disabled(..))`; size the rect with `ButtonFace::height(size, theme)` and `face.intrinsic_width(text, theme)`; hit-test with `face.contains(theme, point)` (grown to the pointer target) |
| a text glyph used as a button (`"×"`, `"+"`, `"‹"`, `"⋯"`) | `Button::icon_only` / `ButtonFace::icon_only` with `ui::icons::Icon::{X, Plus, ChevronLeft, MoreHorizontal}` |
| a fixed-width button that truncates its label | delete the width; buttons size to their label and never elide while the parent has room |
| a solid red "Delete" next to "Cancel" | `ButtonRole::Destructive`; the filled face appears only in the `ModalDialog` that confirms it |
| an accent/blue primary | `ButtonRole::Primary` (inverted: primary ink fill, content-surface label) |
| keypad and on-screen keyboard keys painted by hand, a one-letter square toggle ("B", "I") | `ButtonRole::Key` (a faint lift at rest, the label in `Title`); a toggle with `.toggled(on)` |
| a star or pin glyph swapped for a filled one when on | nothing: a toggled button with `Icon::Star` / `Icon::Pin` draws it filled |
| a plain text button nudged so its text lines up with the content edge ("Back", "Show") | nothing: a labelled `Plain` button's ink sits on its layout edge; its lift and hit area reach outward |
| `set_enabled` called from a state watcher | `Button::enabled_from(&state)` |
| a `Link` whose activation runs an app action through a callback | `Link::action(label, binding)` / `Link::on_action(binding)` |

### Segmented controls and tabs

| Old | New |
|---|---|
| `ToggleGroup::with_flexible_width()` | `.fill()`; without it the strip is as wide as its widest label needs |
| `ToggleGroup::underline()`, `SegmentedStyle::Underline` | `.with_style(SegmentedStyle::Tabs)` / `.style(SegmentedStyle::Tabs)` |
| `SegmentedStyle::Solid` | delete (`SegmentedStyle::Segmented` is the default) |
| `SegmentedStyle::Pill`, `SegmentedFace::pill()` | `SegmentedStyle::Quiet` (a refinement under a primary choice) |
| `SegmentedFace::radius(r)`, `SegmentedFace::left_labels(pad)` | delete |
| a segment label that does not fit ("Wednesday" in a narrow week strip) | `Segment::new("Wednesday").short("Wed")` |
| segment rects sized by hand | `SegmentedFace::height(size, theme)` × `SegmentedFace::intrinsic_width(labels, size, text, theme)`, split with `SegmentedFace::partition(rect, labels, size, text, theme)` (equal while every label fits an equal share, otherwise sized to the labels) |
| accent-filled selected segment, a selected segment painted by the app | `ToggleGroup` / `SegmentedFace`: the chosen segment is a lift thumb |
| hand-drawn tab row with an accent underline | `ToggleGroup::new(..).with_style(SegmentedStyle::Tabs)` (2 px primary-ink underline) |
| `TriStateControl` | `ToggleGroup` with three segments |
| a strip with nothing chosen yet, `selected = 0` as a stand-in | `ToggleGroup::unchosen(segments)` / `set_selection(None)`; `selection() -> Option<usize>`; a painted strip `SegmentedFace::selection(None)` |
| `on_change` that only mirrors the index into a `LocalState` | `.bind_selection(&state)` |
| a row of hand-drawn weekday toggles | `ToggleGroup::multiple(segments, chosen).on_chosen_change(f)`: each its own toggle, as wide as its label, wrapping |
| an icon drawn over a segment, an icon-only segment with an empty label | `Segment::icon(Icon::List, "List")` (the name is spoken) |
| a strip greyed out by hand | `ToggleGroup::with_enabled(false)` / `set_enabled`; `SegmentedFace::disabled(true)` |
| a label stacked over a strip | `ToggleGroup::caption("Repeat")` |
| `SegmentedFace::new(rects, labels, i)` with a hand-picked short label | `SegmentedFace::of_segments(rects, &segments, i)` (short forms where the full label does not fit), split with `SegmentedFace::partition_segments(rect, &segments, style, size, ..)` |
| tabs inset from the content edge, equal-width tabs, a long tab strip that elides | nothing: `SegmentedStyle::Tabs` packs tabs from the leading edge (first label and underline flush) and scrolls sideways when longer than its room |
| `use ui::widget::toggle_group::Segment` | `use ui::widget::Segment` |

### Switches, sliders, checkboxes

| Old | New |
|---|---|
| `ToggleFace::on_color(c)` | delete; on is always the inverted fill |
| `toggle_face_colors(..)` / `face.colors(theme)` returning `(track, knob, border, width)` | returns `ToggleColors { track, knob, knob_edge, edge, edge_width }` |
| `CheckboxFace::transparent_unchecked()` | delete; an unchecked box always has its raised face and edge |
| a non-square checkbox rect | nothing: the face paints the largest square in it |
| a slider squeezed to a knob in a row | nothing: a `Slider` keeps six knob widths of track |
| a `0..1` slider scaled into the app's units by hand, a percentage spoken for seconds | `Slider::range(min, max).step(s).value_text(f)`; the value, binding and callbacks are in those units; arrow, Page Up/Down and Home/End keys work |
| a slider inset so its knob does not leave the column | nothing: the track runs flush with the slider's rect and the knob overhangs it; a painted host hit-tests with `SliderFace::hit_rect` and maps with `SliderFace::value_at` (`SliderFace::overhang()` is the room it reaches out) |
| `ToggleFace` sized with a literal 44×24 | `ToggleFace::size(theme)` |
| a labelled checkbox spanning its row next to other controls | `Checkbox::with_label(..).intrinsic_width()` |
| a checkbox edge strengthened by hand on light grounds | nothing: the unchecked edge reaches 3:1 on its ground |

### Menus and dropdowns

| Old | New |
|---|---|
| `MenuItem::checked_action(label, checked, f)` | `MenuItem::action(label, f).with_checked(checked)` |
| a "✓ " prefixed menu label | `.with_checked(true)` |
| leading, trailing or doubled `MenuItem::separator()` | leave them; the menu shows a separator only between two rows. Delete any code that trims them |
| a row highlighted by hand while its context menu is open | `menu.anchor_row(row_rect)` before `open_*` |
| `menu.draw(scene, t, text, theme, presence)` in a painted widget's `draw` | delete it: a `ContextMenu` paints only in the window's top layer. Call `menu.draw_overlay(..)` from the widget's own `draw_overlay`, or return the menu from `children_mut` while it is open so the default `draw_overlay` reaches it |
| `floating_surface::paint_armed_row(scene, t, row, theme)` | `paint_armed_row(scene, t, row, inset, theme)`, `inset` the row's distance from the panel edge (`menu_inset(theme)` for a menu on it) |
| a menu placed by hand next to its button | `ContextMenu::open_anchored(button_rect, window)`; it opens under the button, aligns to its trailing edge at the window's edge and flips above near the bottom |
| a hand-drawn caption above a dropdown ("Level" over a filter) | `Dropdown::new(..).caption("Level")` and delete the caption text; placed by hand, give it `DropdownFace::outer_height` and `apply_layout_external` keeps the caption above the field |
| a dropdown that spans its row by default | nothing: a dropdown is as wide as its widest option; `.fill()` where it should span (a form column) |
| trailing dropdowns of one section pinned to a common width by hand (`compact_control_w`, a fixed `with_width`) so their edges line up | nothing: a `Section` or `FormSection` gives its dropdowns one width, the widest option among them capped at `metrics.chooser_width`; a captioned, `fill` or menu dropdown keeps its own |
| a closed dropdown painted by a canvas host (camera, reader settings) | `DropdownFace::new(rect, value).caption(..).leading_icon(..).open(b).hovered(b).paint(..)`, sized with `DropdownFace::height(size, theme)` / `outer_height` × `intrinsic_width(options, ..)`, hit-tested with `contains`; the list is an option `Popover` opened at `field_rect(theme)` |
| a "Type to filter…" prompt, a filter on a short list | nothing: the value stays while open, the query shows once the person types, and lists under `FILTER_MIN_OPTIONS` (8) never filter (a typed letter arms the next match) |
| `Dropdown::with_accessible_name(already_localized)` | `with_resolved_accessible_name(name)` |
| a hand-drawn `▾` / chevron path on a dropdown-like field | `Dropdown` |
| `Dropdown` at a fixed 40 px | nothing: it is a `Regular` control; `.control_size(ControlSize::Small)` in a toolbar |
| `NumberInput::with_size(w, h)` | delete; `.control_size(size)`. The width is measured to hold every value in range |
| a hand-drawn `− value +` stepper (zoom, page, point size, copies) | `NumberInput::new(v).with_range(..).with_step(..).with_suffix("%").compact()` |
| a button whose click opens a `ContextMenu` placed from its rect (header More, a Sort menu), the rect walked out of a `NavShell` header | `Button::icon_only(id, Icon::MoreHorizontal, "More").with_menu(items)` (a `MenuButton`): it anchors the menu to its own rect wherever it stands, also in `NavShell` header actions. Add `.aligned(PanelAlign::Trailing)` at the end of a row. Poll `menu_mut().take_selection()` or use the row callbacks |
| a `Toolbar` one control wide so every command lands in its More menu | `Button::with_menu(items)`; delete the one-wide container |
| a button that opens a `Popover` placed by hand | `Button::new(..).with_popover(content)` (a `PopoverButton`); `popover_mut()` reaches the content |
| a second `ContextMenu` opened beside a row as a flyout ("New file ›"), a row label ending in `…` or `›` | `MenuItem::submenu(label, items)`: a chevron, opens on hover, click, Enter or the arrow towards it; `take_submenu_selection()` reports `(row, nested row)` |
| a shortcut painted beside a menu row, or in its label ("Save (Ctrl+S)") | `MenuItem::with_shortcut(Shortcut::command("S"))`; a key on its own is `Shortcut::key("Delete")` |
| `Shortcut::key()` (the legend getter) | `Shortcut::key_name()`; `Shortcut::key(k)` now builds a modifier-free shortcut |
| a popover or menu aligned to a trailing anchor by hand | `Popover::aligned(PanelAlign::Trailing)`, `ContextMenu::set_align(PanelAlign::Trailing)`; it flips to the leading edge only when it would leave the window |
| a viewport inset by hand so a menu or popover keeps off the window edge | delete: every anchored panel keeps `floating_surface::viewport_margin(theme)` from the window's edges |
| a form wrapper around one `TextInput` in a popover (caption, field, hint, Enter submits) | `Popover::prompt(caption, placeholder, hint).with_field_chars(n).with_field(\|input\| input.with_kind(..))`; `open_prompt(anchor, initial)`, `take_submitted()` |

### Text fields and pickers

| Old | New |
|---|---|
| a 40 px field beside 32 px buttons, `TextInput` placed in a hand-sized band | nothing: a single-line field is a Regular control (32); `.control_size(ControlSize::Small / Large)` to match its row |
| a multiline field given a hand-made minimum height or inset | `TextInput::with_min_lines(n)`: never below the single-line band, the same text inset |
| `take_submit()` polled by a tree-owned field, Enter caught by the parent | `.on_submit(\|text\| ..)` or `.on_submit_action(binding)` |
| a clear "×" drawn over a field, a reveal or picker glyph beside it | `.with_clear_button()`; any other: `.with_trailing_accessory(icon, name, AccessoryAction::Run(f))` |
| a plain field padded by hand inside a pane that already has its gutter | nothing: a plain-chrome field has no horizontal inset |
| the caret moved to the end after `set_text` | nothing: `set_text` (and `SearchField::set_query`, `Autocomplete::set_text`) parks the caret at the end |
| a focused field filled darker | nothing: focus keeps the rest fill; the ring marks focus |
| a placeholder in a hand-picked dim ink or the sans face of a serif field | nothing: placeholders draw in the field's own style, in the tertiary ink |
| `TextInput::with_persistent_placeholder()`, a placeholder hidden while the field has focus | delete: every placeholder (`TextInput`, `SearchField`, `Autocomplete`, `RichTextEditor`, a block editor's empty hint) stays while the field is empty, focused or not, and the first character replaces it |
| a formula or query coloured by repainting over the field | `TextInput::with_span_highlighter(\|text, theme\| ranges)`; a field painted by a canvas host: `FieldFace::new(rect, text).focused(b).invalid(b).paint(..)` / `.paint_chrome(..)`, `FieldFace::height(size, theme)`, `text_rect(theme)` |
| `NumberInput` showing a value through a wrapper string ("Now", "10:57") | `NumberInput::with_value_format(format, parse)` |
| `"50 %"` spelled in English | nothing: the unit sits where the locale puts it (`50%`, `50 %` in German) |
| a suggestion popup placed, flipped and scrolled by hand under a caret | `SuggestionList` (`set_anchor`, `set_viewport`, `set_suggestions`, `set_highlight`, `row_at`, `paint`); `Autocomplete` is built on it |
| an autocomplete status line with a hand-drawn spinner | `Autocomplete::set_busy(true)` with `set_status(Some(..))` |
| `Autocomplete::new()` reconfigured through private fields | `Autocomplete::with_input(TextInput::local().with_placeholder(..))` |
| a month calendar framed by hand in a sidebar, event dots painted over it | `DatePicker::new(day).navigator().marked(days)`; `intrinsic_height(width)` for a painted host |
| a shortcut recorder whose keycaps sit off the `InputHint` column | nothing: its keycaps start on its leading edge like an `InputHint`'s |
| a "Search everywhere" label in dim text, a filter count that disappears at zero | nothing: it is a plain button and "0 of N" stays |

### Chips and badges

One family, styled by `ChipStyle`: `Neutral` (quiet, raised; a selected chip
is a lift), `Tinted(Tone)` (a status or a person, a faint wash with neutral
ink) and `Live` (the one filled pill, in your colour: an unread count, a
mention, something live). Danger red is only a status: `Tinted(Tone::Danger)`.
`Tone` is the shared `ui::widget::Tone`: `Neutral`, `Info`, `Success`,
`Warning`, `Danger`, or a person with `Tone::of_identity(id)` /
`Tone::Presence(hue)` (yourself: `Tone::Presence(colors.you)`).

| Old | New |
|---|---|
| `Chip::draw_at(scene, t, rect, label, lead, style, theme, text)` | `ChipFace::new(rect, label).lead(lead).style(style).selected(b).hovered(b).paint(scene, t, text, theme)` |
| `Chip::natural_width_at(label, h, lead, theme, text)` | `ChipFace::intrinsic_width(label, lead, removable, text, theme)`; the height is `ChipFace::height(theme)` |
| `ChipStyle::quiet()`, `ChipStyle::default()` | `ChipStyle::Neutral` |
| `ChipStyle::tinted(colors.intent.inverted)` (a chosen chip) | `ChipStyle::Neutral` plus `.selected(true)` |
| `ChipStyle::tinted(colors.intent.you)` | `ChipStyle::Tinted(Tone::Presence(colors.you))` |
| `ChipStyle::tinted(<a person's colour>)` | `ChipStyle::Tinted(Tone::of_identity(id))` |
| `ChipStyle::tinted(colors.semantic.*)` | `ChipStyle::Tinted(Tone::Success / Warning / Danger / Info)` |
| `ChipStyle::ghost()` | delete the empty-slot chip; show the action as a `Button` |
| `ChipStyle::bare(..)` | `ChipFace::bare()` |
| `ChipStyle::measured(TintedChip { .. })` | the matching `ChipStyle`; delete the colour computation |
| `.with_alpha(a)` on a chip style | `ChipFace::opacity(a)` (transitions only) |
| `Chip::with_dot(color)` | `Chip::with_lead(ChipLead::Dot(color))` |
| `Badge::pill(label, color)`, `Badge::text(s).with_tint(color)` | `Badge::text(label).style(ChipStyle::Tinted(tone))` |
| `Badge::semantic_pill(label, BadgeTone::Success / Warning / Neutral / Accent)` | `Badge::text(label).style(ChipStyle::Tinted(Tone::Success / Warning / Neutral))`; `Accent` → `ChipStyle::Neutral` |
| `Badge::count(n)`, `Badge::dot()` | unchanged (`Live` by default) |
| `Badge::paint_at(scene, t, rect, label, tint, text, theme)` | `BadgeFace::new(rect, label).style(style).paint(scene, t, text, theme)`; size with `BadgeFace::intrinsic_width(label, text, theme)` × `BadgeFace::height(theme)` |
| `surface::pill(scene, t, rect, fill, edge, radius)` + a label drawn on it | `ChipFace` (interactive or a tag) or `BadgeFace` (a status) |
| `surface::chip_colors(theme, selected)` | `ChipStyle::Neutral.colors(theme, ground, selected, hovered)` |
| a hand-drawn status pill (tint fill, tint edge, coloured label) | `BadgeFace` / `Badge::text(..)` with `ChipStyle::Tinted(tone)` |
| a chip whose label size follows its rect | nothing: chip labels are `Label`, badge labels `Caption`, and pills grow with the text scale |

### Toolbars

| Old | New |
|---|---|
| `ui::chrome::toolbar::{ToolbarItem, LabelCue, item_rects, draw, draw_with_metrics, metrics, hit, ToolbarMetrics, TOOLBAR_HEIGHT}`, `ui::{LabelCue, ToolbarItem}` | `ui::widget::Toolbar` (retained), or `Toolbar::lay_out_in(rect, text, theme)` inside a painted widget, then its ordinary `draw` / `handle_event` |
| `ToolbarItem::new(label, w).active(true)` | `Toolbar::push_toggle(label, active, f)`; `set_active(index, on)` from the model |
| `ToolbarItem::new(label, w).group_start(true)` | `.push_separator()` before the group |
| `ToolbarItem::new(label, w).cue(..)` | delete (labels are `Label`) |
| an icon-only toolbar command | `.push_icon(icon, name, f)` |
| an icon-only toggle (bold, italic, compare) | `.push_icon_toggle(icon, name, on, f)` |
| `chrome::paint_bar(scene, t, theme, rect)` | `ui::widget::toolbar::paint_band(scene, t, theme, rect)` (no fill, a bottom hairline) |
| `chrome::bar_padding(theme)` | `ui::widget::toolbar::band_padding(theme)` |
| a toolbar that wraps or clips commands on a narrow window | `Toolbar`: commands that do not fit go to its More menu |
| an icon command that is on or off, painted by hand next to the toolbar | `.push_icon_toggle(icon, name, active, f)` |
| a command left out (or hidden) while it cannot run | `.set_enabled(index, false)` (or `.with_enabled(false)` after pushing): dimmed, announced as disabled, inert |
| a toolbar rebuilt to change one label ("Problems (3)") | `.set_label(index, text)` |
| items ordered by importance, or a re-layout so a pair (zoom out / in) is not split | `.with_priority(p)` after pushing: higher stays visible longer, equals give way from the end; `.joined()` after the second of a pair keeps it with the one before, so they fold together |
| a toggle whose visible label is not its name ("Aa") | `.with_accessible_name("Match case")` after pushing, or `set_accessible_name(index, name)` |
| a ContextMenu anchored to a toolbar item by hand, or reading private item rects | `.push_menu(label, items)`; `item_rect(index)` for anything else anchored to an item |
| `Rc<Cell<..>>` flags set by command closures | `take_activated() -> Option<usize>` after routing each event |
| `lay_out_in(Rect::ZERO, ..)` then `measure` to learn the toolbar's size | `intrinsic_size(text, theme)` |
| placing a hosted toolbar only in `draw` (no renderer in `apply_layout`) | `lay_out_at(rect, theme)`: the cached measurements place it, measured again only when a label or the text scale changed |
| a stage hook that cannot show the More menu | `open_overflow()` |
| a `ToggleGroup`, `NumberInput` or colour well drawn beside the toolbar | `.push_element(Box::new(control))`: it stands in the row and never goes to the More menu |
| a badge or dot painted over a toolbar item | `.set_dot(index, true)` |

### Scrolling

| Old | New |
|---|---|
| `Scrollable::with_content_gutter(px)` | delete; a vertical `Scrollable` reserves `metrics.scrollbar_gutter` on its trailing edge |
| content padded by hand so the scroll bar does not cover it | delete the padding |
| a hand-drawn scroll thumb | `Scrollbar::draw(scene, t, track, metrics, theme, focused)` (thumb ink from `scrollbar::thumb_color`) |

### Swatches

| Old | New |
|---|---|
| `ColorSwatchPicker::new()`, `with_palette(Vec<u32>, cols)`, `DEFAULT_PALETTE`, `packed_color` | `ColorSwatchPicker::new(swatches, columns)` with `ColorSwatch::named(color, "Blue")` (content colours: `colors.presence.series(i)` or the document's own) |
| `with_accessible_labels(..)` | the swatch names |
| `picker.color_at(i)` | `picker.swatch(i).map(|s| s.color)` |
| `ui::widget::color_picker::ColorSwatch` | `ui::widget::ColorSwatch` (same type, now in `color_swatch`) |
| a padding added so swatches line up with text, a selected swatch ringed in your colour | nothing: the first swatch sits on the content edge, and the chosen one is a concentric primary-ink ring with a gap of the ground (focus is that ring in your colour) |
| `ColorPicker::rgb_string()` drawn beside the picker, the picker clipped by hand | nothing: the picker adapts to the width it is given, its hex field and every part lie inside `preferred_size()` / `preferred_height(w)`, and its channel sliders are the shared `Slider` |

### Delete

- Every colour, radius, size, padding and border width passed to a control.
- `ButtonStyle`, `TriStateControl`, `SegmentTone`, `BadgeTone`, the
  `ChipStyle` struct fields, `chrome::toolbar`, `surface::pill`,
  `surface::chip_colors`.
- Module constants for button, chip, badge, menu-row or toolbar geometry.
- Text glyphs used as button icons and "✓" prefixes in menu labels.

### New API

| API | Use |
|---|---|
| `ButtonRole::{Primary, Secondary, Destructive, Plain}`, `Button::role`, `set_role`, `ButtonFace::role` | What a button does |
| `Button::icon_only(id, icon, label)`, `ButtonFace::icon_only(rect, icon, label)` | Icon-only buttons |
| `Button::shortcut_hint(Shortcut)`, `ButtonFace::shortcut_hint` | Trailing keyboard shortcut |
| `Button::fill_width()` | Span the parent's width |
| `ButtonFace::{height, intrinsic_width, hit_rect, contains}` | Canvas hosts |
| `ToggleGroup::{fill, control_size}`, `Segment::short`, `SegmentedStyle::{Segmented, Tabs, Quiet}`, `SegmentedFace::{focused, control_size, height, intrinsic_width}` | Segmented strips |
| `ToggleColors` | A switch face's resolved colours |
| `MenuItem::with_checked`, `ContextMenu::anchor_row` | State rows; the row a menu acts on |
| `Dropdown::{caption, control_size, fill, leading_icon, with_resolved_accessible_name}`, `DropdownFace::{height, outer_height, intrinsic_width, field_rect, contains, paint}`, `FILTER_MIN_OPTIONS` | Stacked caption; size; span; glyph; a resolved name; the painted closed control |
| `NumberInput::{compact, control_size, with_value_format}` | Inline stepper; size; the app's own words for a value |
| `ButtonRole::Key`, `Button::enabled_from`, `Link::{action, on_action}` | Keys; enabled from a state; links that run actions |
| `ToggleGroup::{unchosen, multiple, on_chosen_change, bind_selection, caption, with_accessible_name, with_enabled, selection, set_selection}`, `Segment::icon`, `SegmentedFace::{of_segments, partition_segments, selection, chosen, disabled}`, `NONE_CHOSEN` | No choice yet; many choices; bound selection; icon segments; painted strips |
| `TextInput::{control_size, with_min_lines, on_submit, on_submit_action, with_clear_button, with_trailing_accessory, with_content_size, with_span_highlighter}`, `AccessoryAction`, `FieldFace` | Fields |
| `Slider::{range, step, value_text}`, `SliderFace::{hit_rect, value_at, overhang}`, `ToggleFace::size`, `Checkbox::intrinsic_width` | Ranged sliders; painted switches; a hugging checkbox |
| `SuggestionList`, `Autocomplete::{with_input, set_busy, list_rect}` | The floating suggestion list |
| `DatePicker::{navigator, marked, set_marked, intrinsic_height}` | A sidebar month |
| `FormControl` for `ToggleGroup` and `Autocomplete` | Both take a `labeled_field` label as their accessible name |
| `ChipStyle::{Neutral, Tinted(Tone), Live}`, `ChipFace`, `BadgeFace`, `ChipColors`, `Chip::{style, with_lead}`, `Badge::style` | Chips and badges |
| `Toolbar::{push_icon, push_toggle, set_active, lay_out_in}`, `toolbar::{paint_band, band_padding}` | Toolbars |
| `Toolbar::{push_icon_toggle, push_menu, push_element, with_priority, joined, with_enabled, with_accessible_name, set_enabled, set_label, set_dot, set_menu, item_rect, intrinsic_size, lay_out_at, take_activated, open_overflow}` | Toolbar commands with state, menus, hosted controls and overflow priority |
| `Button::with_menu(items) -> MenuButton`, `Button::with_popover(content) -> PopoverButton` | A button that opens a menu or a popover at its own rect |
| `MenuItem::{submenu, with_shortcut, shortcut, submenu_items}`, `ContextMenu::{with_items, set_items, set_align, open_submenu, close_submenu, take_submenu_selection}` | Nested menus, shortcuts, alignment |
| `floating_surface::{PanelAlign, place_aligned, viewport_margin, panel_room, control_inset, single_row_radius, paint_rounded, paint_armed_row_in}` | Panel alignment, edge margin and concentric insets |
| `Popover::{prompt, aligned, set_align}`, `PopoverPrompt` | A one-field prompt next to its trigger |
| `Shortcut::key(k)`, `Shortcut::key_name()` | A shortcut without the command modifier; its legend |
| `TabBar::{closable, without_new_tab, set_mark, mark, mark_rect, take_mark_click}`, `TabMark::{Busy, Playing, Muted}` | A strip of fixed views; what a tab's content is doing |

| `Metrics::{menu_row_height, scrollbar_gutter}` | Menu rows; scroll bar gutter |
| `ColorSwatch`, `ColorSwatchPicker::new(swatches, columns)`, `ColorSwatchFace::{of, focused}` | Swatches |
| `InputHint::keycap_height(theme)` | Centring a row of keycaps |
| `Colors::inverted_pressed()` | A pressed inverted control: the hover step taken twice, so press reads past hover |
| `Colors::raised_on(ground)` | `surfaces.raised` as it stands on `ground`; `Theme::on_ground` applies it, so controls in a panel never sink into it |
| `widget::floating_surface::{menu_inset, armed_row, paint_armed_row, OVERLAY_LIFT}` | A painted menu's row inset and its armed row: the panel's overlay lift, concentric with the panel |
| `widget::floating_surface::{place_anchored, anchored_room}` | Place a painted floating Panel at its trigger: flip, align and shift at the window edges |
| `SegmentedFace::partition(rect, labels, size, text, theme)` | A painted segmented strip's segment rects, sized to their labels |
| `Widget::set_invalid(bool)` | A form's verdict on a control; `Field::with_error` sets it. `TextInput` edges itself in danger ink and reports `invalid` to assistive technology |

Visible changes that need no code: a popover menu's rows are menu rows
(`metrics.menu_row_height`, 32) with the lifted armed row and a small
check; the dropdown list insets its rows like every menu and checks the
applied value instead of lifting it; text fields lift under the pointer;
tooltips are the floating material; a toast shows its severity as a leading
icon instead of a stripe; the tree's disclosure, the breadcrumb separator,
the date picker's month steps and the time picker's arrows are chevrons from
`ui::icons` instead of typed glyphs. A screenshot test pinned to the old
pixels is regenerated, not worked around.

## Icons

One set, `ui::icons::Icon`: Lucide glyphs stroked at 1.5 px on the 24-unit
grid, in ink colours. Every icon an app needs is in it; an app never draws a
glyph by hand, uses a Unicode character or an emoji, or borrows a nearby
glyph that means something else. A missing icon is reported and added here.

| Old | New |
|---|---|
| `IconWidget::new(icon, 20.0)` (or `Metrics::BASE.icon_md`, any pixel size) | `IconWidget::new(icon)` (Regular) or `.sized(IconSize::Small / Regular / Large)`, resolved from `metrics.icon_sm` / `icon_md` / `icon_lg` |
| A hand-drawn heart, flag, pin, bookmark or star filled when on | `icon.draw_filled(scene, transform, rect, ink)` for the on state, `icon.draw(..)` for off |
| `"..."` or `…` for hidden path segments | `Breadcrumb` paints `Icon::MoreHorizontal` itself |
| A stand-in glyph (a `Circle` for a status, `Monitor` for a USB stick, `FileText` for a quote) | The icon that names the thing (below) |
| A text toolbar command that has a glyph (Bold, Italic, Link, Table) | `Toolbar::push_icon(icon, name, f)` / `push_icon_toggle(icon, name, on, f)`; the name stays its tooltip, accessible name and overflow row |

Added to the set, by what they are for:

| Use | Icons |
|---|---|
| People and calls | `User`, `UserPlus`, `Phone`, `PhoneOff`, `Video`, `MessageSquare`, `Smile`, `SmilePlus`, `Hand`, `Hash` |
| Places and travel | `MapPin`, `Navigation`, `LocateFixed`, `Landmark`, `Hospital`, `Coffee`, `Utensils`, `Beer`, `BedDouble`, `ShoppingBag`, `TrainFront`, `Plane`, `Trees`, `PawPrint`, `Flag`, `Trophy` |
| Turns | `CornerUpLeft`, `CornerUpRight`, `ArrowUpLeft`, `ArrowUpRight`, `ArrowDownLeft`, `ArrowDownRight` |
| Weather and time | `Cloud`, `CloudRain`, `CloudSnow`, `Wind`, `Droplet`, `Moon`, `Gauge`, `Clock`, `AlarmClock`, `Hourglass` |
| Media | `Shuffle`, `Repeat`, `Repeat1`, `Heart`, `ListMusic`, `Speaker`, `Headphones`, `Disc3` |
| Text and documents | `Type`, `Bold`, `Italic`, `Underline`, `Strikethrough`, `Superscript`, `Subscript`, `Heading1`, `Heading2`, `ListOrdered`, `ListTree`, `Quote`, `AlignLeft`, `AlignCenter`, `AlignRight`, `AlignJustify`, `Baseline`, `Highlighter`, `Table`, `Columns2` |
| Canvas and images | `Box`, `BringToFront`, `SendToBack`, `Palette`, `Contrast`, `Crop`, `ScanLine`, `QrCode`, `SwitchCamera` |
| Mail and sharing | `MailOpen`, `ReplyAll`, `Forward`, `Link`, `Upload`, `Rss`, `Bookmark`, `Tag` |
| Devices and state | `Usb`, `SdCard`, `Lock`, `EyeOff`, `CircleCheck`, `Lightbulb` |

Filled variants are not separate icons: a glyph whose on state is filled
(`Heart`, `Bookmark`, `Flag`, `Star`, `Pin`) paints with `draw_filled`.

## Content widgets

The patterns every app drew by hand are library widgets. Each comes in two
forms with one geometry: a retained widget for widget trees, and a `*Face`
immediate painter (`ListRowFace`, `PropertyListFace`, `CardFace`,
`StatTileFace`, `MediaCardFace`, `DropZoneFace`, `SkeletonFace`,
`ProgressRingFace`, `StepperFace`, `AvatarFace`) for apps that paint into a
`Scene`. A face is built per frame, sized with `height(...)` and painted
with `paint(&mut PaintCtx, rect)`. Every widget is semantic: none takes a
colour, font size, radius, padding or pixel size. Where an app needs a mark
with meaning it passes a `Tone`.

### Removed APIs and their replacements

Apply the first row that matches each call.

| Old | New |
|---|---|
| `Card::composed(body)`, `ContentCard` | `Card::new(body)` |
| `.with_header(element)` on a card | `.with_title(title)`, or `.with_title_action(title, action)` when the header held one control. A custom header element has no replacement: put the content in the body. |
| `.with_footer(Box::new(actions))` | `.with_footer(actions)` (an `Actions`, unboxed) |
| `Card::titled(title, body)` holding settings, a form section, statistics or explanatory text | `Section::titled(title, vec![body])` |
| `Card::titled(title, body)` for an object (something the person opens, moves, shares or drags) | `Card::new(body).with_title(title)` |
| `Card::titled_with_action(title, action, body)` | Not an object: `Section::new(vec![body]).with_header(SectionHeader::heading(title).action(button))`. An object: `Card::new(body).with_title_action(title, action)`. |
| `Container::new().with_background().with_child(..)` used as a card | The same two rows above. |
| `CardGrid::content_sized(cards)` | `CardGrid::new(cards)` (rows are always content-sized) |
| `CardGrid::with_card_height(px)`, `DEFAULT_CARD_HEIGHT` | Delete. A row is as tall as its tallest card. |
| `CardGrid::with_min_card_width(px)`, `DEFAULT_CARD_MIN_WIDTH` | `.with_card_width(CardWidth::Compact)` for px ≤ 220, `CardWidth::Regular` (the default; delete the call) for ≤ 300, `CardWidth::Wide` above. |
| `CardGrid::with_gap(px)` | Delete (`rhythm.sibling`). |
| `Section::with_gap(items, gap)` | `Section::new(items)` |
| `DEFAULT_SECTION_GAP` | `theme.tokens.spacing.rhythm.sibling` |
| `FormSection` titles | Unchanged call; the title is now `Heading`, not `Title`. |
| `ActionRowFace::new(rect, name)…paint(..)` | `ListRowFace::new(name)…paint(&mut ctx, rect)`; hit-test the rect you laid out; `face.accessibility(rect, target)` for the node. |
| `ActionRowFace::…paint_focus(..)` alone (a painted control's focus) | `ui::widget::paint_focus_ring(scene, transform, rect, radius, theme)` |
| `ActionRowFace::accented(true)` / `.outlined(true)` | Delete: rows are not accented. The one recommended choice goes first. |
| `ChoiceCardFace::new(rect, icon, title, description)` | `ListRowFace::new(title).leading(RowLeading::Icon(icon)).secondary(description).disclosure()` |
| `data_table::paint_selected_row(..)` (crate-private) | `ui::chrome::paint_row_selection(scene, transform, rect, state, ground, theme)` ([Navigation](#navigation)) |
| `StatTile::with_icon(icon)` | Delete. |
| `StatTile::with_value_color(c)` | `.tone(Tone::Success / Warning / Danger / Info)` by what the colour meant; a colour that meant nothing: delete. |
| `StatTile::with_delta(text, trend)` | `.change(text, trend)`, with text that says against what ("12% this week"). The arrow is grey; the meaning is the tile's tone. |
| A value with its unit in one string (`"48 ms"`, `"1.2 TB"`) | `StatTile::new(label, "48").unit("ms")` |
| `StatTile::paint(ctx, rect, &StatTileContent { .. })` | `StatTileFace::new(label, value)…paint(ctx, rect)` (size it with `.height(text, theme)`) |
| A `Row` of `StatTile`s | `StatGroup::new(tiles)` (reflows 4 → 2 → 1) |
| `Avatar::from_initials(initials, size)` | `Avatar::person(name, identity).size(AvatarSize::…)`: size ≤ 24 `Small`, ≤ 40 `Regular` (default; omit), ≤ 56 `Large`, else `Hero`. `identity` is a stable id (a peer id, an address, an account key), never a list index. |
| `Avatar::new(image, size)` | `Avatar::person(name, identity).with_image(image).size(..)` |
| An app widget that loads an avatar picture (tube `ChannelAvatar`) | Delete it; `Avatar::person(name, id).with_image_url(store, url)` |
| `Avatar::draw_at(scene, t, rect, initials, colour, theme, text)` | `AvatarFace::new(name, identity).paint(&mut PaintCtx::new(scene, t, text, theme), rect)`. When you hold the person's presence colour (a collaborator's chosen colour), add `.presence(colour)` instead of hashing. |
| `Avatar::draw_ringed_at(ctx, rect, initials, colour, AvatarRing { color: ground, .. })` | `AvatarFace::new(..).presence(colour).ringed_on(ground).paint(ctx, rect)` |
| `Avatar::draw_overflow_at(ctx, rect, n, ring)` | `AvatarFace::paint_overflow(ctx, rect, n, Some(ground))` |
| `Avatar::color_for_initials(..)`, `Avatar::background_color` | `AvatarFace::new(name, identity).colour(theme)` |
| `Avatar::initials_for_name(name)` | `ui::widget::initials_for_name(name)` (first and last word) |
| `Avatar::initial_ink(fill)` | `ui::widget::avatar::initial_ink(fill, theme)` |
| `AvatarRing` | Deleted. |
| A square account/site icon drawn as a rounded rect with a letter | `AvatarFace::new(..).square()` / `Avatar::person(..).square()` |
| `RingGauge::new(f).with_label(value, caption)` | `ProgressRing::new(f).value(value).caption(caption)` |
| A ring that colours by threshold (CPU, disk) | add `.level(GaugeThresholds::default())` |
| `RingGauge::with_color(c)` | `.tone(Tone::…)` by meaning; delete for plain progress (the inverted fill). |
| `RingGauge::with_size(px)` | `.size(ProgressRingSize::Small)` (no readout, inline), `Regular` (default), `Large` (the screen's hero) |
| `RingGauge::with_thickness`, `with_style`, `GaugeStyle` on a ring | Delete. |
| `RingGauge::draw_at(ctx, circle, thickness, &reading)` | `ProgressRingFace::new(frac).value(..).caption(..).paint(ctx, rect)` |
| `EmptyStateContent { icon, title, message, action_label }` | Add `illustration: None, error: false`, or write `EmptyStateContent { icon, message, ..EmptyStateContent::new(title) }`. |
| A hand-drawn error placeholder ("Couldn't load …" with a red or accent icon) | `EmptyState::error(title, detail).with_icon(icon)`; immediate: `error: true`. |
| An empty state with an app-drawn picture | `.with_illustration(|ctx, rect| …)` (immediate: `illustration: Some(&painter)`), painting from theme values. |
| `Container::with_fill(colour)` | Over a whole pane or the window: delete it. A well inside content: `.with_background()`. |
| A `Container` (or painted rect) with its own hit-test and hover fill | `Container::new()…interactive(id, || …)` (or `.interactive_action(id, binding)`) |
| `ui::widget::Table`, `widget::table::*` | `DataTable` with `ColumnDef`s. The editable grid is the block editor's own document table. |
| A `DataTable` column painted with `with_text_render` that only right-aligns a number | `.with_kind(TextKind::Numeric).with_text(..)`: numeric columns align cells and header to the trailing edge |
| A cell painting `NULL`, `—` or an empty string for a missing value | `.with_optional_text(|row| …, "NULL")` (the absent label is painted in tertiary ink and read out) |
| App code painting a table header band, a sort triangle, zebra rows or a selected-row fill | Delete it; `DataTable` paints all four. `.ruled()` for rules between columns (query results, spreadsheets). |
| `FunctionPlane::paint_grid(scene, t, text, theme, ground, label_size)` | Drop `label_size`: labels are `Caption`, culled when they would collide and kept whole inside the canvas. |
| A legend painted next to a `PieChart` | `PieChart::new(slices).with_legend()` |
| `PageSidebar` markup dots drawn by the app | `sidebar.set_marked(page_indices)` |
| `PageSidebar::with_header("PAGES")` | Delete the call (the default is "Pages"), or pass sentence case. |
| A header, a sentence or a status set in by hand (`with_horizontal_padding`, an `inset(S3, ..)`, a `text_edge` helper) so it meets a list's text | Delete it: rows, headers, property labels and table columns share the column's content edge (HIG §9.3). |
| A `Row` of a `Text` title and a `Toggle` / `Button` built to look like a list row | `ListRow::new(id, title).trailing(Box::new(control))` |
| A `Row` of title, value text and spacer for a figure at the trailing edge (a clock's time, a code) | `ListRow::…value(text)` / `.title_value(text)` |
| `ListRow::on_activate(f)` for an open or select | `.on_view(f)`; for an action that changes something, `.on_effect(f)` |
| A row's hover action routed to an exported action by hand | `.action_binding(icon, label, binding)` |
| A hand-built count pill, star or tag beside a row's title | `.count(n)`, `.mark(Icon::Star)`, `.tags(&[(label, style)])` |
| `PropertyRow { action, .. }` | `PropertyRow::new(label, value).action(icon, label)` (up to two); the field is `actions` |
| Two `PropertyList`s whose label columns differ in one section | Put them in the same `Section` (they share one column), or `PropertyList::sections(..)` |
| A `Text` + `Icon` button pair that folds a group | `Section::…collapsible()` |
| A dot and a `Caption` built as a status footer | `StatusLine::active(text)` (or `warning`, `error`, `idle`) |
| An avatar beside a `Title` and a `Caption` heading a detail view | `ObjectHeader::new(name).avatar(name, id).caption(text)` |
| A plain `Chip` beside an invisible button that navigates | `Chip::new(label).with_id(id).on_click(f)` |
| `CellRender` closures `|row, scene, theme, text, rect|` | `|row, scene, transform, theme, text, rect|`, painting through `transform` |
| `CellTextRender` closures `|row, scene, text, rect, theme, selected|` | `|row, scene, transform, text, rect, theme, selected|` |
| A cell painter that draws a severity or status pill | `.with_badge(|row| (label, tone))` |
| A cell painter that draws a tree's indent and chevron, with app hit-testing | `DataTable::tree(depth, has_children, expanded, on_toggle)` |
| `DataTable` row selection on release of a drag-scrolling body | A mouse press selects at once; add `.drag_scroll()` only where a mouse drag should scroll |
| A painted track list or a hand-drawn header band | `data_table::DataTableFace` |
| A `Button` placed beside a `Banner` (Retry, Restore, Open settings) | `Banner::warning(..).with_action(button)` (and `.with_secondary_action(button)`); immediate: `BannerContent::new(..).action("Try again")`, hit-test `BannerHit::action`. Size a painted banner with `Banner::height(&content, width, text, theme)`. |
| `Banner::paint(..)` returning the dismiss `Rect` | `Banner::paint(..)` returns a `BannerHit { rect, action, secondary_action, dismiss }`; `dismiss` is `None` when not dismissible. |
| A banner kept in the tree with `is_dismissed()` checks and an app flag for "show it" | `Banner::from_state(&LocalState<Option<BannerNotice>>)`: `None` collapses it with its gap. `.on_dismiss(f)` runs when the person closes it. |
| `BannerKind::Info` for a notice where nothing is wrong (a private window, a read-only copy) | `BannerKind::Neutral` (`Banner::neutral(..)`, `BannerNotice::neutral(..)`): primary ink, no hue. |
| `toast::paint_transient` with a hand-placed action button or a second card for the body | `ToastCard::new(title, kind).body(body).action(label, hovered)`; the card sizes itself to its wrapped text. `ToastAnchor::TopCenter` / `TopRight` under a HUD. |
| `when.elapsed() > TOAST_TTL` around an immediate toast | `toast::transient_expired(shown_at, ttl)`: holds under the capture clock. |
| `toasts.tick(dt)` and a `toasts_ticked` instant in the app | Delete both: `ToastStack` advances itself from `ui::time`, pauses on hover and under the capture clock, and wakes the host at the next expiry. The app only pushes. |
| `EmptyState::with_action(..)` + `take_action()` polled after events | `.with_action(label).on_view(f)` (or `.on_effect(f)`, `.on_action(binding)`); give it `.with_action_id(id)` to address it. The drain channel stays for painted hosts. |
| A small "nothing here yet" stack hand-built under a heading inside a page | `EmptyState::new(title).inline()`: a `Heading`, hugging its content. |
| A spinner placed over or beside an empty state while something loads | `EmptyState::new(title).busy()` (`set_busy(bool)`): a spinner in the hero slot. |
| `EmptyState::minimum_essential_height` plus app math to find the action | `EmptyState::layout(text, theme, rect, &content)` returns `natural_height`, `hero` and `action` without painting. |
| `ProgressBar` painted in a status colour by the app | `ProgressBar::new(f).tone(Tone::Danger)` or `.level(GaugeThresholds::default())`; painted: `ProgressBarFace::new(f).tone(..).paint(ctx, rect)`, sized by `ProgressBarFace::height(theme)`. |
| `BarGauge::with_color(theme.tokens.colors.intent.inverted)` | `BarGauge::new(f).neutral()` |
| A small ring with the seconds painted beside it (one-time codes) | `ProgressRingFace::new(f).size(ProgressRingSize::Small).value("23")`: a small ring holds a numeric `Caption`. |
| A `ProgressRing` rebuilt on every tick | `ProgressRing::from_state(&state, \|s\| RingReading::new(frac, value).caption(..))`; `set_caption(..)` changes the caption in place. |
| A wizard's own "not started" state or clickable step labels | `Stepper::new(steps).current(None)`; `.on_station(\|index\| ..)` lets the person go back to a done step; `station_rects()` / `StepperFace::station_rects(..)` for painted hosts. |
| A qualifier appended to a stat's value ("1.2 TB of 2 TB") | `StatTile::new("Used", "1.2").unit("TB").note("of 2 TB")` |
| A `StatGroup` rebuilt to change one reading | `group.tile_mut(i).set_value(..)` (`set_note`, `set_change`, `set_tone`), or `set_tiles(..)` for a different set. |
| A dot painted for "unsaved changes" | `Badge::modified()` / `BadgeFace::modified(rect)`; a new-item dot is `BadgeFace::dot(rect)`. |
| A presence dot painted on an avatar's corner | `AvatarFace::new(name, id).status(PresenceStatus::Online)` (`Away`, `Busy`, `Offline`). |
| A rounded-rect letter tile for an app without an icon | `AvatarFace::new(name, id).neutral()` (`Avatar::person(..).neutral()`). |
| A circular artist picture clipped by the app | `MediaCard::new(..).aspect(MediaAspect::Circle)` |
| A 2×2 cover collage, a kind icon, or app-painted art on a media tile | `MediaSource::Mosaic(images)`, `MediaSource::Kind(icon)`, `MediaSource::Paint(Box::new(\|ctx, rect\| ..))`; painted: `MediaArt::Mosaic`, `MediaArt::Kind`, `MediaArt::Paint`. |
| A tag painted over art ("New", "Explicit") | `.badge(OwnedBadge::Tag(text, ChipStyle::…))` / `MediaBadge::Tag(..)` |
| A verified glyph after a channel name | `.meta(channel).meta_mark(Icon::…)` |
| A loading grid of hand-sized grey boxes | `MediaGrid::loading(count, aspect)` |
| `Card` with a hand-painted selected outline | `Card::new(..).selected(true)` / `CardFace::new().selected(true)` |
| `VideoPlayer` with an app-painted still before playback | `VideoPlayer::new().with_poster(MediaSource::Url { store, url })` |
| A page outline with a stripe and a shadow (print and scan queues) | `PagePreview::new(label, page, count)`: flat, sized by the theme. |
| A PDF reader's rail that ignores drags and menus by hand | `PageSidebar::new(aspect).read_only()`; `set_drop_target_armed(true)` while a file is dragged over it. |
| Edge handles or a rotation-less frame hand-drawn around a crop | `SelectionHandles::new(bounds, metrics).with_edges().without_rotation()`; `resize_for(handle, from, to)` for one-axis resizes. |
| A camera shutter painted from circles | `CaptureButtonFace::new(CaptureKind::Photo / Video / Recording).live(..).paint(ctx, center)`; `accessibility(rect)` names it. |
| `PieChart::with_height(px)` to make room for a legend | Delete it: the chart takes the room it is given and centres the donut and legend as one unit. `PieSlice::with_legend_detail("12 GB")` adds a size column. |
| Axis labels drawn inside a `Chart`'s plot | Nothing to do: `Chart` sets them in a gutter outside the plot. |
| `format!("-{}", ..)` for a negative tick on a function plane | `function_plane::format_tick(value, step)` (uses U+2212). |
| `math::layout` at `size * text_scale` with glyphs that outgrow their bars | Unchanged call: math glyphs are content-sized now. `math::layout_wrapped(row, text, size, max_width)` breaks before top-level operators. |

### Pages, forms and readable columns

| Old | New |
|---|---|
| `Container::new().with_centered_max_width(720.0)` (or a `READABLE_WIDTH` constant) around a form, settings or a page of facts | `Container::new().readable()`: `metrics.readable_width` on the leading edge. Keep `with_centered_max_width` (or `.readable().centered()`) only for a page that stands alone, like a wizard. |
| `Scrollable` + `Column` + max-width `Container` + a footer `Row` hand-built per screen | `Page::new().head(item).section(section)…` with `.with_persisted_offset(cell)` and `.with_pinned_actions(Actions::new(..))`. A `Page` has no title; the shell's header titles it. |
| `FormPage::new(title, form)` with a `Heading` title, centred | Unchanged call; the title is `LargeTitle` and the column sits on the leading edge. `FormPage::untitled(form)` when the header already says it; `.with_header_action(button)`, `.with_persisted_offset(cell)`, `.with_pinned_actions(actions)`. |
| `FormSection::new("")` or a section whose title repeats the page's | `FormSection::untitled()` (no heading, visible or accessible) |
| A `Field` faked with a `Row` of a label `Text` and a value | `Field::custom(label, element)`: a read-only value, a `Row` of controls or buttons, lined up with the other fields |
| `.content(Box::new(Row::new().push(button)…))` for a row of buttons in a grouped section | `FormSection::control_row(element)`: a row of the section, with its height and separators |
| Help copy under the control beside its label, measured label offsets, a second label node for screen readers | Delete: help reads under the label while the label is beside the control, the label is the control's name (read once), a beside label sits on a tall control's first line, and every section of a `Form` shares one label column |
| Settings row helpers (label growing to the control, description under the label) | `Field::new(label, control).with_help(description)` in a `FormSection::grouped()`; switches end on the trailing edge, and narrow windows stack rows with full-width controls |
| A private 16-em chooser width | `metrics.chooser_width` |
| `Container::new().with_no_padding()` around a `Form` "so it keeps its height in a scroll view" | Delete the wrapper: form grids keep an automatic minimum height |
| A tile or link that should line up with the text beside it but lift on hover | `Container::new().hug().interactive(id, f)`: content-sized, no inset, the lift bleeds past the rect |
| A column of buttons sized to the widest | `Column::with_intrinsic_width()` |
| A `ModelView` wrapped in a `Container::filling()` so a `Scrollable::fill_parent_height()` inside gets a height | Delete the wrapper: `ModelView` passes percent heights and flex growth through |
| An app `Swap` / slot whose content is replaced from outside the tree | `ui::widget::Slot::new(element)` (or `Slot::empty()`) and its `SlotHandle::set(element)`, adopted at the next sync |
| A `ModelView` rebuilding a tabbed or wizard part each time it shows again | `Switch::from_state(&state, |s| key, |key| build)`: each part is built once and kept alive, scroll and drafts included |
| A main column and an inspector-like aside laid out by hand, or a `Row` that never stacks | `Columns::new(main).aside(aside)`: the aside keeps `metrics.inspector_width` beside the main column and goes under it below the themed breakpoint |
| A dashboard of cards in a centred `with_centered_max_width(1120.0)` | `Page::new().full_width()`: the page's gutter, reflowing across the column |
| A footer of a status and actions pinned under a form by hand (a second `Scrollable` sharing the gutter) | `FormPage::with_pinned(element)` / `Page::with_pinned(element)` (`with_pinned_actions` for `Actions` alone) |
| A block document (a note) whose editor starts `gutter - text_inset` in, so its drag handles share the page gutter and touch the sidebar | Place the editor's rect on the page gutter and line the title and metadata up at `gutter + BlockEditor::handle_gutter(theme)`: the handles hang in their own gutter outside the text column, a full page gutter clear of the sidebar |
| A `Page` or `FormPage` rebuilt every frame | `.with_semantic_key(key)`, `.with_cursor(point)` and `.with_persisted_offset(cell)` as on the `Scrollable` |
| A `Following` decorator that rebuilds a `ResizableSplit` when its divider moves, or forwards `update` to its panes | Delete it: the split relays out its panes and forwards `update` itself. `.with_content_sized_first()` for a first pane at its content size until moved; `left()` / `right()` (and `_mut`) reach the panes |

### Painted hosts

A widget that paints its own content uses the same parts the retained
widgets use, so it behaves like them:

| Need | API |
|---|---|
| Pressable rects (keys, markers, clips): hover, press, focus ring, Tab between them, Enter/Space, accessible buttons | `PaintedTargets`: `begin_frame()`, `target(id, rect, label)` per frame, `handle_event(event, ctx)`, `interaction(id)` to paint, `paint_focus(..)`, `take_activated()`; forward `wants_tab_input`, `set_focus` (`set_host_focus`), `accessibility_children()` and `perform_action` / `perform_semantic_action` |
| A scrolling viewport (a transcript, a history list, a modal body from `ModalLayout::body`) | `ScrollRegion`: `set_viewport(rect, theme)`, `set_content_size(w, h)`, `handle_event(event, ctx, theme)`, `paint(scene, t, theme, |scene, t| ..)` (clip, offset, edge fades, bar), `to_content(point)` for hit-testing, `primary_vertical_scroll()` / `apply_primary_vertical_scroll(..)` for assistive scrolling, `with_persisted_offset(cell)` |
| A scroll bar of its own | `Scrollbar::track_in(lane, theme)` for the track in a `metrics.scrollbar_gutter` lane, `Scrollbar::paint_marks(.., &[TrackMark])` for search hits and changes, `BarFade` for the idle fade, `metrics.wheel_line` per wheel notch |
| Fading content out at an edge (`DestOut` masks, `vertical_fade` over non-media, hand-made gradients) | `ui::surface::content_edge_fade(scene, t, rect, ContentEdge::Bottom, ground)`, only while more can scroll in from there; `Scrollable::with_edge_fade()` / `ScrollRegion::with_edge_fade()` do it for you |
| A divider between two painted panes | `SplitHandle` with a `SplitRatio`: `set_rect`, `pane_rects()`, `handle_event`, `paint`, `accessibility()` (a separator with its value), `cursor_shape`; the same model `ResizableSplit` uses |
| A column rule; crisp edges | `ui::surface::vertical_separator(..)`; `ui::paint::snap_rect(rect)` before edging a surface |
| A swipe that commits | `metrics.swipe_commit` |
| A retained control placed by hand that should take keyboard focus when pressed | `Embedded::take_focus_request()` after routing an event; take the host's focus in response |

### Which widget replaces which hand-drawn pattern

| Widget (face) | Use it for | Replaces |
|---|---|---|
| `ListRow` (`ListRowFace`) | One row of a content list: a message, a contact, a request, a file, a device, a setting that opens a page | mail list rows and swipe (mail `pane.rs:1466-1800`), contacts rows (`app.rs:4449-4565`), snippets/feeds rows (snippets `app.rs:90-160`, feeds `list_layout.rs`), remote hosts (`app.rs:4672`), api-tester method rows (`navrow.rs`) |
| `PropertyList` (`PropertyListFace`) | Labelled values: details, metadata, fields | contacts detail (`app.rs:4895-5010`), passwords fields with copy (`showcase.rs:3050-3112`), hex-editor inspector (`view.rs:1295-1420`), file info panes |
| `Section` + `SectionHeader` | A titled group on the ground; a list of `ListRow`s | git-client change groups with count and "Stage all" (`changes.rs:420-520`), every "Title + count + Clear all" row |
| `FormSection::grouped()` | Settings rows: label leading, control trailing, separators | camera settings (`app.rs:6346-6770`) |
| `Card` (`CardFace`) + `CardGrid` | Objects: apps, documents, comments, saved connections | store cards (`app.rs:2000`), word-processor review cards (`.leading_stripe(Tone::Presence(..))`), clocks quick starts |
| `StatTile` (`StatTileFace`) + `StatGroup` | One number and what it measures | backups summary tiles (`app.rs:2200-2295`), monitor tiles with live sparkline and click-to-select (`app.rs:2236-2408`), vpn 2×2 (`app.rs:2308-2376`) |
| `DataTable` | Tabular data | db-client grid (`grid.rs`), logs (`app.rs:980-1060`), api-tester headers/params tables |
| `MediaCard` (`MediaCardFace`) + `MediaGrid` / `justified_rows` | Covers, thumbnails, album art, photos, app art | ebook covers (`app.rs:7824-8180`), tube video tiles (`widgets/large_video_meta.rs`), music albums (`render.rs:784`, generated art `art.rs:120-127`), photos justified rows (`app.rs:11069-11200`), store artwork (`artwork.rs:42-62`) |
| `DropZone` (`DropZoneFace`) | A drop target for files | ebook import (`app.rs:8182-8296`), imager source (`app.rs:2858-2958`) |
| `Skeleton` (`SkeletonFace`) | Content still loading | music loading rows (`render.rs:8702`) |
| `ProgressRing` (`ProgressRingFace`) | A countdown, progress or measured level in a ring | imager progress (`app.rs:3882-3942`), totp countdown (`ring.rs`), speed-test dial (`app.rs:527-575`) |
| `Stepper` (`StepperFace`) | Where a wizard is | imager stations (`app.rs:2533-2657`) |
| `EmptyState` | Nothing here yet, or something failed | weather illustration (`core.rs:2218-2276`), pdf error (`app_draw.rs:1199-1210`) |
| `Avatar` (`AvatarFace`) | A person's or account's picture or initials | mail avatars (`draw.rs:108-150`), passwords site icons (`.square()`), tube `channel_avatar.rs` |
| `Container::interactive` | A whole tile that opens something | tube clickable tiles (`widgets/flex.rs`) |

### Choosing

- **Card, Section or StatTile.** Can the person open, move, share or drag
  it? It is a `Card`. Is it a number? A `StatTile` in a `StatGroup`. Is it
  settings, a form, or a group of facts? A `Section` (with a `PropertyList`,
  `ListRow`s or `FormSection` inside). Never put a section, a form or a stat
  inside a card.
- **Section header.** `SectionHeader::heading` for a section of a page,
  `SectionHeader::group` for a group inside one. `.count(n)` when the number
  of items matters (unread, changes); `.action(button)` for one text action
  on the whole group (its label ends on the trailing edge). The header is
  one height with or without an action. No subtitle: if the person must know
  something to act, put one sentence in the content. A painted host uses
  `SectionHeaderFace` with the same geometry (`height`, `action_rect`,
  `paint`).
- **A section that folds** (a window group, an archive): `Section::…
  .with_header(..).collapsible()` (or `.collapsed()`), `.on_toggle(f)`. The
  header gets a chevron, toggles with a pointer, Enter, Space and the arrow
  keys, and reads as expanded or collapsed. Never build a disclosure out of a
  button and a flag.
- **Status and detail heads.** A non-interactive status ("Engine running",
  "3 stopped") is a `StatusLine::active(text)` (`warning`, `error`, `idle`,
  `presence`): a dot and a `Caption` on the content edge. The head of a detail
  view is an `ObjectHeader::new(name).avatar(name, id)` (or `.icon(icon)`,
  `.square()`) `.caption(text)`: never a hand-stacked `Title` and `Caption`.
- **ListRow leading column.** `.avatar(name, identity)` for people
  (`.square()` for accounts and sites), `.icon(icon)` for kinds of things
  (with `.leading_tooltip(name)` when the glyph is the only place the row
  says its kind, a connection's protocol for one; never a text prefix),
  `.badge(code)` for a short classifying code (`GET`, `PDF`, `WARN`, with
  `.leading_tooltip(text)` when the code needs explaining; the code is part of
  the row's name), `.thumbnail(image)` for a picture,
  `.media(source, aspect)` for media art (the title's initials stand in until
  it loads), `.checkbox(checked, f)` for multi-select, `.swatch(colour)` for a
  colour that is content, `.leading_paint(f)` for anything else (a font
  sample). In a `Section`, consecutive rows share one badge column on their
  own; with faces, compute `badge_column_width(text, theme, &all_badges)`
  once per list and pass it to every row's `.leading_column(..)`.
- **ListRow text.** Title in `Body` (`.emphasized(true)` for unread,
  `.two_line_title()` for long names, `.title_family(font)` only in a font
  list), `.mark(icon)` after it for starred or pinned, `.tags(&[(label,
  style)])` for labels; `.subtitle(text)` for a `Body` line under the title (a
  message's subject), `.secondary(text)` for the one secondary fact
  (`.two_line_secondary()` for a message preview), `.meta(text)` for a time,
  size or count on the title's line (`.meta_spans(&[(text, tone)])` for toned
  figures such as `+2.4%`), `.count(n)` for how many items the row stands
  for. More than one secondary fact: a `PropertyList` in the detail view, or
  columns in a `DataTable`. Never join facts with `·`.
- **ListRow trailing.** `.value(text)` (or `.title_value(text)`) for the
  row's own figure (a time, a code, a temperature): hover actions never
  replace it. `.trailing(element)` for one persistent control (a `Toggle`, a
  `Button`, a `ToggleGroup`, a small `ProgressRing`, a `Badge`): the row
  keeps its width, it stays reachable by keyboard and assistive technology,
  and it moves under the text when the row is narrow (faces:
  `.trailing_width(w)` and `trailing_rect(..)`). `.checked(bool)` for the
  chosen row of a single-choice list; `.active()`, `.warning()`, `.error()`,
  `.idle()` for a status dot on the title's line; `.presence(identity)` for a
  person's presence; `.disclosure()` when activating opens a page.
  `.action(icon, label, f)` (or `.action_binding(icon, label, binding)` for an
  exported action the host confirms) for actions shown on hover and focus
  (they replace the meta; the arrow keys reach them, and a touch pointer
  shows them on the selected row); `.swipe_action(icon, label, destructive,
  f)` for touch. `.with_enabled(false)` dims a row and takes it out of focus.
- **ListRow activation.** `.on_view(f)` when activating opens or selects
  something, `.on_effect(f)` when it does something; both publish the row's
  action so it is discoverable and driveable, as a `Button`'s do. `.on_action
  (binding)` for an exported resource action. `.on_activate(f)` remains for
  a closure the native audit may report.
- **Chip.** A tag or a backlink that navigates is a `Chip::new(label)
  .with_id(id).on_click(f)` (or `.on_action(binding)`): a button with a focus
  ring that Enter and Space press. `.menu()` adds a trailing chevron for a
  chip that opens choices. A painted keyboard key is a `ChipFace` wider than
  its label: the label centres in it.
- **PropertyList.** One label column per section: lists in one `Section`
  share it on their own, and `PropertyList::sections(vec![(title, rows)])`
  groups related facts under titles in one list (`PropertyColumn` shares a
  column across stacks; faces take `.label_column(property_label_column(..))`).
  `Property::with_kind(TextKind::Identifier / Secret / Numeric)` for values
  that never break (they elide before the actions); prose wraps at a readable
  measure. `.tone(Tone)` for a value that carries a status; `.copyable()`
  and `.action(icon, label, f)` for up to two trailing actions;
  `.with_accessible_value(text)` when the painted value is not what should
  be read (a masked secret). `.compact()` for an inspector beside a canvas;
  `.with_id(id)` for tests and the accessibility tree. A `Field` that shows a
  value it cannot edit hosts a `PropertyValue`.
- **Tone.** `Success`, `Warning`, `Danger` and `Info` for status,
  `Tone::of_identity(id)` / `Tone::Presence(hue)` for a person. `Neutral` is
  no colour. A tone is never decoration: a card stripe or a stat ticker
  without a meaning is deleted.
- **Media.** `MediaAspect::Square` for albums and app art, `Wide` for video,
  `Portrait` for books and posters, `Ratio(w / h)` for photos (with
  `MediaGrid::justified`). `MediaSource::Url { store, url }` loads in the
  background; until it lands, and for missing art, the generated placeholder
  shows. Badges: `Duration`, `Progress`, `Live`, `Downloaded`. Up to two
  `.meta(..)` lines.
- **Progress ring.** Plain progress or a countdown: no fill call. A measured
  level: `.level(thresholds)`. A status: `.tone(..)`.
- **Banner actions.** A banner holds what resolves it: one action
  (`.with_action(Button)`) and at most one secondary. They sit inside the
  box at the trailing edge and wrap under the message when the banner is
  narrow. Never place a button beside a banner.
- **Empty states.** A pane with nothing in it: `EmptyState::new(..)`. A
  section of a page with nothing in it: `.inline()`. Something on its way:
  `.busy()`. The one action is a published button: give it
  `.with_action_id(..)` and `.on_view(..)` / `.on_effect(..)`.

### Lists

- **One content edge.** A row's text, a section's title, a property list's
  labels and a table's first column all start on the column's content edge;
  the lift and the stripes bleed `chrome::row_bleed` (`s2`) past it (HIG
  §9.3). Give the column its gutter and nothing else: no inset on a header to
  meet a row's text, no padding on a sentence to meet a list.
- **Rows in a section.** Put `ListRow`s in a `Section`: consecutive rows
  become one list, edge to edge, with inset separators that hide beside the
  selected row, one shared badge column, and one lift across a run of
  selected rows (a multi-selection). Selection is `.selected(true)`; never
  paint a row fill.
- **Painted rows.** Paint each `ListRowFace` into its outer rect (the
  content edge grown by `row_bleed`), call `.separator(next_selected)` on
  every row but the last and `.joined(RowJoin::new(above, below))` on
  selected rows. `paint_row_selection_joined` is the lift for anything else a
  host paints as a row.
- **Long lists.** A `VirtualList` stands its rows one bleed in from its
  edges, so their lift fills it, and keeps the shared scroll bar in its
  gutter. `.on_activate(f)` runs on Enter and on a double click,
  `.on_context(f)` on a right click (the row is selected first, and `f` gets
  the row's rect to anchor a menu), `reveal(key)` scrolls a row into view.
  Build it with `VirtualList::with_selection_delegate(..)` when the rows
  paint their own selection: each row receives a `RowSelection` with its
  neighbours' (a `ListRow` row takes it in place, with its joined lift and
  separators).
- **Tables.** A `DataTable`'s rows are 40 high, 32 with `.compact()`. Columns:
  `ColumnWidth::Auto` measures header, text, badges and controls, and shares
  the leftover when no column is flexible; `.priority(n)` lets a column give
  way when the table is narrow (higher first); what still does not fit
  scrolls sideways (shift-wheel or the bar). `.wrapping(lines)` lets a cell
  wrap and its row grow. `.with_badge(|row| (label, tone))` for a level or a
  status, `ColumnDef::checkbox(..)` with `.on_check(f)` for a column of
  boxes, `.with_add_row(label, f)` for the blank row that adds one. Painters
  (`with_render`, `with_text_render`) receive the table's transform; never
  paint text a column can declare. A table that holds a hierarchy is
  `.tree(depth, has_children, expanded, on_toggle)`: the chevron, the indent,
  Right and Left to fold, level and expansion read out. `.fill_height()`
  takes the parent's height and scrolls inside; `.on_row_context_menu(f)`
  selects the row and hands its rect; `.drag_scroll()` lets a mouse drag
  scroll the body (a finger always does), otherwise a drag past the slop runs
  `.on_row_drag_begin(f)` and `row_rect` / `row_at_point` find drop targets.
  Edit rows in place with `rows_mut()` and `apply_changes(&changes)` so cell
  controls stay mounted by row key. `set_sort_indicator(key, ascending)` shows
  a sort the host applied. A pressable row lifts under the pointer;
  `with_text_render` gets the row's `CellState { selected, hovered }`, and a
  widget column `.revealed_on_hover()` is a row's actions, mounted only for
  the row under the pointer (the selected row on touch) over the quiet state
  its text render paints. `.without_header()` leaves the header band out and
  `.with_row_height(px)` sets a taller row. A painted host draws
  `data_table::DataTableFace`, or embeds the table itself (`Embedded`) in a
  page it scrolls as a whole: `.host_scrolled()`, the table's rect on the part
  of it in view, `set_host_window(offset, body_height)` for the rows that
  is, `content_height()` for the room it takes of the page; the header then
  stays at the top of the part in view.

### What to delete in an app

1. Hand-drawn list rows: per-row hover/selection fills, separators, avatar
   circles, timestamp placement, unread dots, status dots, hover-action
   hit-testing. Use `ListRow` / `ListRowFace`.
2. Key/value grids drawn with two `Text` columns or measured label offsets.
   Use `PropertyList`.
3. Cards around settings, forms or stats (`Card::titled`,
   `Container::with_background` used as a group). Use `Section`.
4. Stat tiles painted as cards with a value colour or an accent ticker. Use
   `StatTile` / `StatGroup` with a tone.
5. Table headers, sort triangles, zebra stripes, selected-row fills and
   column measurement around a `DataTable`, and any padding added to its
   first or last column or for its scroll bar: the table insets its
   columns from the lift and reserves the bar's column itself.
10. A gutter left of a `BlockEditor` for its drag handles: the editor keeps
    the handles in its own leading column and starts every block (text,
    checklist boxes, quotes, code, embeds, tables, images) at one content
    edge.
6. Cover and thumbnail painters, generated-art gradients made of stacked
   rects, duration pills, progress bars and live badges over media, and
   column math for media grids (`card_columns` stays for custom layouts).
7. Drop-target dashed boxes, loading placeholder bars, progress rings,
   wizard step indicators, and empty or error placeholders.
8. App avatar widgets and avatar colour tables indexed by position.
9. The status enums, colour tables and pixel constants these painters used
   (`ROW_H`, `AVATAR_D`, `TILE_PAD`, …).

### Painted app with content faces

```rust
use ui::paint::PaintCtx;
use ui::widget::{AvatarFace, ListRowFace, RowLeading, StatTileFace, Tone};

let mut ctx = PaintCtx::new(scene, transform, text, theme);
let mut y = list.y0;
for (i, message) in messages.iter().enumerate() {
    let face = ListRowFace::new(&message.sender)
        .leading(RowLeading::Avatar(AvatarFace::new(&message.sender, &message.address)))
        .secondary(&message.preview)
        .meta(&message.time)
        .emphasized(message.unread)
        .selected(Some(i) == selected)
        .interaction(row_interaction(i));
    let face = match messages.get(i + 1) {
        Some(_) => face.separator(Some(i + 1) == selected),
        None => face,
    };
    let h = face.height(ctx.text, ctx.theme);
    let rect = Rect::new(list.x0, y, list.x1, y + h);
    face.paint(&mut ctx, rect);
    row_rects.push(rect);
    y += h;
}

let tile = StatTileFace::new("Protected", "1.2").unit("TB").tone(Tone::Success);
let h = tile.height(ctx.text, ctx.theme);
tile.paint(&mut ctx, Rect::new(x, y, x + w, y + h));
```

## Search

Search belongs to the platform. An app draws no search field, no result list
and no palette of its own. Sort every search-like thing in the app into one
of three jobs and apply that job's mapping:

| The person wants to… | The app provides | The person sees |
|---|---|---|
| find something the app holds (a message, a video, a store item, a place) | a `SearchProvider` per findable kind (framework apps: `Resource::search`), `SearchRow`s, scopes, facets | the platform palette: Ctrl+K, `/`, or the app's magnifier button |
| narrow the list already on screen | `Filterable` on that list | `SearchField::filter()` in the navigation shell's filter slot, or `Filtered` |
| find in the document on screen | `Findable` on that view | the `FindBar` band at the top of the content (`FindScope`), Ctrl+F |

If a field does two of these (a sidebar box that filters the list *and*
queries a server on Enter), it is a filter whose Enter escalates — the filter
mapping covers both.

### Removed APIs

| Removed | Replacement |
|---|---|
| `SearchField::opens_palette()` | `ui::search::search_button(SearchRequest::this_app())` in the toolbar. |
| `SearchField::is_palette_launcher()`, `SearchField::take_palette_request()` | Nothing to poll: the button's action opens search. Delete the polling code and whatever it called (`open_container_palette`, a private `CommandPalette`). |
| `SearchProvider::search_page(&self, query: &str, …)` | `search_page(&self, query: &SearchQuery, …)`. Use `query.text()` where you used `query`; see steps 4 (scopes) and 5 (facets) under Replace for the rest. Providers that only implement `search`/`search_limited` change nothing. |
| `SearchPage { results, next, inspected }` | Add `pending: None`, or build it with `SearchPage::of(results, next)` (finished) / `SearchPage::pending(results, token)` (more coming). |
| The 38 px pill `SearchField` | Nothing: the field is a regular control now (32 px, `radii.control`). Delete any height, radius or padding the app set around it. |

### New API

| API | Use |
|---|---|
| `ui::search::search_button(request) -> Button` | The search launcher: an icon-only magnifier button. The only search affordance in an app's chrome. |
| `ui::search::SearchRequest::{this_app(), within(ScopeDecl), with_query(q)}` | What the launcher opens: this app, or one part of it, optionally pre-filled. |
| `ui::search::ScopeDecl::new(anchor, label)` (also `tilo_resource::ScopeDecl`) | A part of the app search can be narrowed to. `label` fills the pill "Only Inbox". |
| `ui::search::open_search_binding(&request)` | Bind any other action control to open search (a menu item, a "Search in this folder" link). |
| `ui::search::SearchOpener` / `open_search_invocation(&request)` | A painted widget opening search from its own input handler: call `opener.open(request)`, return `opener.take()` from `ActionControls::take_control_invocation`. |
| `ViewCx::search_button(request)`, `ViewCx::open_search(&request)`, `Dispatcher::open_search(&request)` | The same in a framework app. |
| `ViewCx::on_open(|anchor| …)` | Show the item a search hit (or an agent's `open` call) points to. |
| `Resource::search_scopes(state)`, `#[resource(scopes = path)]`, `SearchProvider::search_scopes()` | Declare the parts search can be narrowed to; the palette lists each as a "Search in" row. |
| `ui::search::SearchQuery` | What a provider receives: `text()`, `filters()` / `filter_values(key)` / `admits(key, values)`, `scope()` / `in_scope(anchor)`. |
| `ui::search::{Facet, FacetValue, FacetFilter}`, `SearchProvider::facets()`, `CatalogSearchProvider::{facets, admits}` | Filter dimensions: chips in the palette, typed `key:value` tokens parsed by the platform. |
| `SearchPage::pending(results, PendingToken)`, `SearchProvider::poll_page(&token) -> PagePoll` | A slow source (IMAP, YouTube, a store catalogue, a geocoder): return what you have now, answer the rest when it arrives. |
| `ui::search::SearchRow::{message, media, file_hit}` + `SearchResult::with_row` | Say what a hit is; the platform draws the row. |
| `SearchResult::within(anchor)` | Scope a hit addressed by a flat id (`contact:42`, `sheets.cell:B4`) by the part of the app it lies in (`group:family`, `sheet:2`), without renaming the resource. |
| `SearchEntry { within, row, ..SearchEntry::default() }`, `tilo_resource::{SearchRow, SearchRowShape}` | The same for a framework app: the scope a flat-id entry lies in, and its row (subtitle, a named icon from `Icon::name`, a message, media or file-hit shape). |
| `CatalogSearchProvider::search_scopes()` | A catalogue provider's scopes; the blanket `SearchProvider` impl forwards them. |
| `ui::icons::AppImage::from_image_store(&store, url)` | A media row's thumbnail or a cover from the app's `ImageStore`. |
| `ui::icons::Icon::{name(), from_name(name)}` | An icon by its lucide kebab name, for data that cannot name the enum. |
| `SearchField::filter()`, `.escalates_within(scope)`, `.with_placeholder(..)`, `set_count(Some(FilterCount::new(shown, total)))` | The in-content filter field. |
| `ui::widget::{Filterable, FilterCount, apply_filter, sync_filter, Filtered}` | Bind a filter field to a list. |
| `ui::widget::{Findable, FindOptions, FindMode, FindMatch, FindBar, FindScope, paint_find_match, with_findable}` | Find in a document. |

### Replace

Work through these in order. "The field" is the app's own search/filter/find
input and everything drawn around it.

1. **A field that searches the app's content** (mail's search box, tube's and
   music's YouTube search, the store search, the maps place search, a
   snippets/notes search box, containers' launcher pill):
   - Delete the field, its result list or overlay, any private
     `CommandPalette`/`ActionPalette`, and the code that runs the query.
   - Move the query into a provider. A framework app returns its findable items
     from `Resource::search` as `SearchEntry { anchor, title, text }`; any
     other app implements `SearchProvider` (prefer `CatalogSearchProvider`
     over a `SearchCatalog` for local data) and returns it from
     `SearchSource::search_providers`.
   - Put `search_button(SearchRequest::this_app())` where the field was in the
     toolbar or header (framework app: `cx.search_button(..)`).
   - A source behind the network: `search_page` starts the request, returns
     the hits it already has (cache, local index) with
     `SearchPage::pending(hits, PendingToken::new(id))`, and `poll_page`
     answers `PagePoll::Pending` until the reply is in, then
     `PagePoll::Ready(SearchPage::of(more, None))`. Never block in either; a
     token you no longer know answers `Ready(SearchPage::empty())`.
2. **Hits the app drew itself.** Pick the row shape by what the hit is and
   fill it with the same values the app drew:
   - a message, mail, chat line → `SearchRow::message(sender, subject).snippet(..).date(..).unread(..)`;
   - a video, track, episode → `SearchRow::media(title).thumbnail(AppImage).byline(channel).duration(..)`;
   - a match in a file or a code search hit → `SearchRow::file_hit(path, line_text).line(n).matched(byte_range)`;
   - anything else → no row: `title`, `with_subtitle`, `with_icon` (the generic row).
   Dates and durations are passed already formatted. Delete the app's row
   painting.
3. **Opening a hit.** A framework app registers `cx.on_open(|anchor| …)` in
   the view that shows items and sets the `LocalState` that selects / scrolls
   to the item, exactly as a click in the list would. The framework then
   answers both a picked hit and the resource's `open` action (agents) with
   it. Other apps keep `SearchSource::activate_search_result(action_id, ..)`.
   Anchors are paths: give items inside a part of the app the part's anchor
   as prefix (`folder:inbox/msg:12`). An item whose id is flat (`kind:uuid`)
   keeps it and says where it lies instead: `SearchResult::within(anchor)`,
   or `SearchEntry { within: Some(anchor), .. }` in a framework app.
4. **Scopes** ("Search this folder", "Search in playlist",
   per-mailbox search): declare each part as a `ScopeDecl { anchor, label }`
   — framework apps with `#[resource(scopes = my_scopes)]` (or
   `Resource::search_scopes`), others with `SearchProvider::search_scopes`.
   A button that searched one part becomes
   `search_button(SearchRequest::within(ScopeDecl::new(anchor, label)))`.
   Providers that can narrow cheaply read `query.scope()`; the palette drops
   hits outside the scope (`query.in_scope(hit.anchor())`) either way. For the
   shell's drill-down the anchor must also be a published resource id (a
   `Resource::children` entry).
5. **Facets** (snippets `language:`/`tag:`, store
   source/category, a mail `from:`): return them from
   `SearchProvider::facets()` as `Facet::new("language", "Language").value("rust", "Rust")`.
   Read the applied values in `search_page` with `query.filter_values("language")`
   or test a hit with `query.admits("language", &[hit_language])`
   (`CatalogSearchProvider::admits` for catalogue providers). Delete the app's
   own token parsing and filter dropdowns next to its search field.
6. **A field that narrows the visible list** (sidebar filters, a table filter,
   containers' panel filter, a settings search over the visible settings):
   - Implement `Filterable` on the list: `set_filter(query)` narrows (match
     with `ui::search::ListIndex` so it agrees with search), `filter_count()`
     returns `FilterCount::new(shown, total)`, and `search_scope()` returns the
     `ScopeDecl` the list shows (or `None` for the whole app).
   - Place the field: in a `NavShell` sidebar it is the shell's filter slot
     (see [Navigation](#navigation)); anywhere else wrap the list:
     `Filtered::new(Box::new(list)).with_placeholder("Filter people")`.
     A painted host keeps a `SearchField::filter()` and calls
     `sync_filter(&mut field, &mut list)` after routing each event.
   - Delete the app's count text ("3 results"), its key-cap or count
     workaround drawn over the field, its own Escape-to-clear and its "no
     matches, search online" button: the field shows "3 of 120", clears on
     Escape, and offers "Search everywhere" when nothing matches.
   - Placeholder copy names what is filtered ("Filter people"), sentence case,
     no ellipsis, no marketing.
7. **Find in a document** (pdf-viewer, hex-editor, ebook, terminal
   scrollback, text and code views):
   - Implement `Findable` on the document view: `find(query, options)`
     searches its own content and keeps the matches, `reveal(i)` scrolls match
     `i` into view and makes it current, `clear_find()` stops painting. Offer
     `supports_replace`/`replace`/`replace_all` only where the view edits
     (replacements go through the view's own actions, so they are undoable),
     and `supports_hex` in a byte view (`FindMode::Hex`).
   - Paint each match with `paint_find_match(scene, transform, span_rect, is_current, theme)`
     over the matched glyphs only — one call per line a match covers. Never
     tint a whole block, page or row.
   - Wrap the content: `FindScope::new(Box::new(view))` (configure with
     `.with_bar(FindBar::new().with_replace())` if needed). A painted app that
     lays out its own chrome hosts a `FindBar`: `layout(band_rect, theme, text)`,
     `paint(..)`, `route(event, ctx)`, then `drive(&mut view)`, with the band
     `FindBar::height(theme)` tall at the top of the content.
   - Delete the app's find field, counter, case/word toggles, next/previous
     buttons, its Ctrl+F / F3 / Enter handling and its match colour.
   - A hosted bar, with what used to need a workaround:

     | Workaround | Instead |
     |---|---|
     | replacing the `FindBar` to clear a sticky `close_requested()` | `take_close_requested()` after `drive`; the bar keeps its query and options for the next open |
     | `set_query(query)` again to make the counter follow content that changed under an open bar (terminal output, a reloaded file, PDF text arriving page by page) | `refresh()`: the next `drive` finds again and keeps the current match |
     | a byte view whose bar opens on Text | `FindBar::with_mode(FindMode::Hex)` / `set_mode`; `set_options(options)` restores a view's last find |
     | an engine that finds on its own (a web page) faking `Vec<FindMatch>` | return no matches from `find`, report with `FindBar::set_results(total, current)`, and step in `Findable::step(forward) -> true` |
     | a regex toggle drawn beside the bar | `Findable::supports_regex() -> true` (or `FindBar::with_regex()`); `FindMode::Regex` reaches `find` |
     | a second field focused by hand for Ctrl+H | `focus_replace()` |
     | insetting the band and repainting its hairline so the field lines up with the content | `with_content_inset(leading, trailing)` / `set_content_inset` |
     | a bare `FindBar` hosted because `FindScope` binds plain Ctrl+F (a terminal) | `FindScope::new(view).with_shortcut(Shortcut::command("F").shift())`, or `.without_shortcut()` and `FindScope::open()` from the app's own key |

     The bar's labels come from the toolkit's catalogue (`find-bar-*`), its
     Match case / Whole word / Regular expression options are toggle buttons,
     Text / Hex is one segmented strip, and its only X is the bar's close
     control.
8. **Keys.** Delete every app handler for Ctrl+K, `/` and Ctrl+F that opens

   search or find: Ctrl+K and `/` open the platform search (a focused text
   field or terminal still types `/`), Ctrl+F belongs to the `FindScope` or
   `Filtered` around the content.

### Delete

- Every app-drawn search field, search pill, magnifier-in-a-box, result list,
  search overlay and private palette.
- `open_container_palette` and any other path that builds a `CommandPalette`
  or `ActionPalette` in the app.
- Hand-drawn match counters, find toolbars, case/word toggles, highlight
  colours and whole-block match tints.
- App handlers for Ctrl+K, `/`, Ctrl+F, F3.
- App shard entries these leave behind (lower them as in
  [Updating shards](#updating-shards)).

### What each host shows

The app does the same thing in every host; what the host does with it:

- **A standalone window** opens its own palette: scope pill and "Search in"
  rows, facet chips, slow pages under a loading row, row shapes.
- **The Tilo shell** opens the system palette scoped to the app's tab with the
  request's query, and drills into a scope the app publishes as a browsable
  resource. App hits reach it through the action directory. Implement rows,
  facets and slow pages anyway: the provider contract is the same in both.

## Window chrome & shots

Overlays, status and captures behave the same in every app: one modal, one
popover, both presented in the window's top layer; persistent status in the
status bar and transient status in the bubble; chrome over media on the
floating material; and shots that are the live window, settled, on a pinned
clock. HIG §7c is the rule; this section is the mechanical part.

### Removed APIs and their replacements

| Removed | Replacement |
|---|---|
| `ui::widget::ModalFrame` (namespace) | functions of `ui::widget::modal_dialog`: `layout`, `paint_backdrop`, `paint_actions`, `card_width`, `body_width`, `action_height`, `ink` |
| `ModalFrame::layout(viewport, &spec, tr, theme)` | `modal_dialog::layout(window, &spec, tr, theme)`; `window` is the whole window (see "Painted apps"), never a pane |
| `ModalFrame::paint_backdrop(..)` / `ModalFrame::paint_actions(..)` | `modal_dialog::paint_backdrop(..)` / `modal_dialog::paint_actions(..)`, same arguments |
| `ModalFrame::card_width(viewport, &spec, theme)` / `ModalFrame::body_width(viewport, &spec, theme)` | `modal_dialog::card_width(window, spec.max_width, theme)` / `modal_dialog::body_width(window, spec.max_width, theme)` |
| `ModalFrame::action_height(theme)` / `ModalFrame::ink(theme)` | `modal_dialog::action_height(theme)` / `modal_dialog::ink(theme)` |
| `ModalFrame::card_fill(theme)` | delete; a colour derived from the sheet is `ui::chrome::panel_ground(theme)` |
| `ui::widget::modal_frame::{ModalAction, ModalEmphasis, ModalHit, ModalLayout, ModalSpec}` | same names in `ui::widget::modal_dialog` (also re-exported from `ui::widget`) |
| `MODAL_SCRIM_ALPHA` (`ui::widget::modal_frame`) | `theme.tokens.colors.surfaces.scrim_strength`; better, `ui::overlay::scrim::modal(scene, transform, window, openness, theme)` |
| `MODAL_PAD`, `MODAL_TITLE_ROW_H`, `MODAL_ACTION_ROW_H`, `MODAL_CLOSE_SIZE`, `MODAL_VIEWPORT_MARGIN` | delete; read geometry from the `ModalLayout` |
| `ModalSpec::body_height(h / text_scale)` | `ModalSpec::body_height(h)`: the body height in the same logical pixels as everything else; never divide by the text scale |
| `ModalDialog::with_max_size(w, h)` / `set_max_size(w, h)` | `ModalDialog::with_max_width(w)` / `set_max_width(w)`; the height always follows the content (`shortcuts`' `with_max_size(560.0, 520.0)` becomes `with_max_width(560.0)`) |
| `ModalDialog::layout_in(rect, theme, tr)` and `ModalDialog::with_viewport(rect)` | delete; the top layer places the dialog over the window (see "Mounting a dialog") |
| `ui::widget::modal_dialog::{MAX_DIALOG_WIDTH, MAX_DIALOG_HEIGHT}` | `ui::widget::modal_dialog::MODAL_MAX_WIDTH`; no height cap |
| `ui::widget::PopoverWidget::new(anchor, content, side)` | `ui::widget::Popover::with_content(content).anchored(anchor).side(side)` |
| `ui::widget::popover_widget::PopoverSide` | `ui::widget::PopoverSide` |
| `ui::overlay::{Modal, Popover, OverlayStack, OverlayEntry, Scrim}` | `ModalDialog`; `ui::widget::Popover`; nothing (the host owns the top layer); `ui::overlay::scrim::{modal, vertical_fade}` |
| `ui::chrome::STATUS_BAR_HEIGHT` | `ui::chrome::status_bar::height(theme)`, and reserve the band with `status_bar::split` |
| `ui::status_bubble::draw_chevron(..)` | `ui::icons::Icon::ChevronLeft` / `ChevronRight` `.draw(scene, transform, rect, ink)` |
| `FrameHarness::render_window_png(path)` / `render_window_rgba()` | `FrameHarness::render_png(path)` / `render_rgba()` (now on `surfaces.content`) |
| the old transparent `FrameHarness::render_png` / `render_rgba` | `render_png_transparent` / `render_rgba_transparent`, only where a test needs alpha 0 |
| `ShotFrame::with_background(colour)` | delete; `ShotFrame::write_png` clears to `surfaces.content`, `write_png_transparent` for alpha |
| `chrono::Local::now()` | `ui::clock::local_now()`; `Local::now().date_naive()` is `ui::clock::today()` |

### New API

| API | Use |
|---|---|
| `ModalDialog::with_max_width(w)` | Widest the sheet is drawn (design pixels, default 520). |
| `ModalDialog::close_rect()` | The close control; shown when the dialog has `on_dismiss` and no secondary action. |
| `ui::widget::Popover::with_content(c)` + `.anchored(rect)`, `.side(PopoverSide)`, `.open(bool)`, `.on_dismiss(f)`, `.with_viewport(rect)`, `.set_anchor(rect)`, `.set_open(bool)`, `.content()`, `.content_mut()` | An anchored Panel holding any widget. |
| `ui::widget::Popover::new()` + `.with_label`, `.with_items`, `open_at`, `item_at`, `item_rect`, `move_highlight`, `select`, `take_selection`, `place` | An option menu (unchanged API). |
| `ui::widget::floating_surface::{paint, paint_faded, radius}` | The body of any floating Panel: `radii.panel`, the blurred material, `elevation.floating`, the panel edge. |
| `ui::chrome::status_bar::{height, split, draw}` | Persistent status, in a band the content does not cover. |
| `StatusBubble::show(text, now)`, `pointer_moved(area, p, tr, theme, now)`, `paint(scene, t, tr, theme, area, now)`, `rect_in(area, tr, theme)`, `text()` | The transient bubble owns its text, timing, placement and paint. |
| `ui::overlay::scrim::modal(scene, t, window, openness, theme)` | The scrim behind a modal Panel (`surfaces.scrim`). |
| `ui::overlay::scrim::vertical_fade(scene, t, rect, FadeEdge::Top / Bottom, strength, theme)` | One smooth gradient under text on media. |
| `theme.tokens.colors.media.{letterbox, scrim, ink, ink_dim}` | Media colours, the same in every theme. |
| `ui::icons::Icon::TriangleAlert` | Warning icon (the `BannerKind::Warning` default). |
| `ui::clock::{local_now, today, CAPTURE_LOCAL_TIME}` | Local date and time; pinned to 30 June 2026, 10:42 in captures. |
| `FrameHarness::settle_for_capture(bound)` | Run frames until no animation runs and no image load is in flight. |
| `ui::image_store::loads_in_flight()` | Image loads in flight across every store. |
| `ResourceApp::scenario_viewport(name) -> Option<(u32, u32)>` | The viewport a named scenario is captured at. |

### Mounting a dialog or a popover

`ModalDialog::draw` and `Popover::draw` paint nothing. Both paint in
`Widget::draw_overlay`, which the host calls after the app and the window's
chrome, with the window as the presence viewport. So:

1. **Retained apps** (a `Column`/`Row`/framework view holds the dialog):
   nothing to do beyond deleting any `dialog.draw(..)`/`popover.draw(..)` call
   the app makes itself. A dialog anywhere in the tree covers the window.
2. **Painted apps** (a custom `Widget` that owns `dialog: ModalDialog<_>` or
   `popover: Popover` as a field):
   - Return it from `children()`, `for_each_child()` and `children_mut()`
     while it is open, so the default `draw_overlay` reaches it. If the app
     overrides `draw_overlay`, call `self.dialog.draw_overlay(scene,
     transform, text, theme, presence)` there instead.
   - Delete `dialog.layout_in(..)`, `.with_viewport(..)` and the
     `dialog.draw(..)`/`popover.draw(..)` call in `draw`.
   - Keep routing events with `dialog.handle_detached(event, scale, mods,
     doc, path)` or `popover.handle_event(..)` while it is open.
3. **Immediate-mode sheets** that keep `ModalSpec`/`ModalLayout` (no
   `ModalDialog`): move the three paint calls into the app's
   `draw_overlay`, and lay out against the window:
   `let window = presence.viewport.get().unwrap_or(self.rect);`. Delete any
   scrim the app paints itself and any opaque "overlay" fill used as a scrim
   (vpn, mail).

Scrims that covered only a pane (calendar, shortcuts, containers,
word-processor) cover the window once the dialog is mounted as above; delete
any per-pane scrim rect.

A dialog (or a popover with content) mounted in a retained tree lays its
content out in the window's own tree, out of flow: its node takes no room
from the column it stands in, a click on a field focuses that field and
blurs the one before, opening it focuses its first control, and Tab runs
from the content into the footer actions and around inside the trap. A
painted host that routes events through `handle_detached` keeps its content
in the dialog's own tree; mount the dialog as a child node instead when its
fields need the window's focus.

What used to need a workaround around `ModalDialog`:

| Workaround | Instead |
|---|---|
| translating a field's focus request to the window's node (`field_under`, `host_field_nodes`), a decorator that makes the dialog's node absolute | delete: the content is in the window's tree |
| a `Scrollable` with a computed `max_height` as the body, MouseWheel routed to it by the app, a `fit` pass per window size | delete: a body taller than the window allows scrolls inside the sheet, with the wheel, a scroll thumb and hairlines at the scrolled edges, and ends on a row boundary |
| rebuilding or re-measuring the dialog after the content changed size | `invalidate_layout()`; a structure change the content reports does it by itself |
| `ModalSpec`/`paint_backdrop`/`paint_actions` to ask a question with three answers | `.primary_action("Save", ..).tertiary_action("Don't save", ..).with_destructive_tertiary(true).secondary_action("Cancel", ..)`: the third answer stands apart, Enter never fires it, Escape is Cancel |
| a type-to-confirm gate painted by hand, or a primary action that looks enabled while locked | `set_primary_enabled(bool)` / `with_primary_enabled`, or `primary_enabled_if(\|content\| ..)`; `set_primary_label(text)` for a label that follows the content |
| a typed-phrase confirmation body built by hand | `ModalDialog::new(title, PromptBody::confirm_phrase(message, phrase)).primary_enabled_if(PromptBody::confirms)` |
| a title elided on one line | nothing: the title wraps to two lines before it elides |
| a sheet that covers the window's header row | nothing: a sheet that fits below the header row stands below it; only one too tall for that room takes the whole window (inside the gutter) |
| a dropdown inside a dialog whose list closes the dialog when picked below the sheet | nothing: an overlay the content opened gets the press before the scrim |
| tests that could not find the footer buttons by name | `click_named("modal.primary")` (or `"<dialog test id>.primary"`, `.secondary`, `.tertiary`) |

An immediate-mode host that asks a question from a painted widget uses
`ModalSheet`, which keeps the layout of its last paint so the event path can
hit-test without a renderer:

```rust
// draw_overlay
let window = presence.viewport.get().unwrap_or(self.rect);
let actions = [
    ModalAction::primary("Save"),
    ModalAction::alternate_danger("Don't save"),
    ModalAction::secondary("Cancel"),
];
let spec = ModalSpec::new("Save changes to notes.md?").actions(&actions);
self.sheet.paint_message(scene, transform, text, theme, window, &spec, "Your changes will be lost.");
// handle_event
match self.sheet.handle(event, ctx.scale_factor) {
    Some(SheetResponse::Action(0)) => self.save(),
    Some(SheetResponse::Action(1)) => self.discard(),
    Some(SheetResponse::Action(_) | SheetResponse::Dismiss) => self.cancel(),
    None => {}
}
*ctx.handled = true;
```

Delete the cached `ModalLayout` field, the hover index and the hit-testing
around it. `ModalSheet::paint` takes a closure for a body that is not a
message.

### Choosing modal or popover

- Confirms a destructive or irreversible step, asks for a value, or blocks
  until answered → `ModalDialog`. Title says what is being decided; the
  primary action names what happens ("Delete 3 photos"); `with_destructive`
  for deleting, revoking or discarding; a `secondary_action("Cancel", ..)` or
  an `on_dismiss` (which shows the close control).
- Picks one value next to the control that owns it (an interval, a unit, a
  sort order) → `Popover::new().with_items(..)`. Delete a hand-drawn list
  with a solid accent row: the highlight is the panel's lift and the applied
  option has a check.
- Shows a small, dismissible form or detail next to its trigger →
  `Popover::with_content(widget)`.
- Never a modal for information that needs no decision: use a `Banner` or a
  `Toast`.

### Status: which surface

| What | Surface |
|---|---|
| Ln/Col, word or character count, selection size, unresolved conflict count, zoom level that the person checks while working | `chrome::status_bar`: `let (content, bar) = ui::chrome::status_bar::split(region, theme);` lay the region's content out in `content`, then `ui::chrome::status_bar::draw(scene, t, tr, theme, bar, left, right)` |
| A change worth noticing once: "Synced", a page turn, a zoom step | `StatusBubble` |
| A readout that does something when clicked (a zoom level, an offset that opens Go to) | `ui::chrome::status_bar::draw_readouts(scene, t, tr, theme, bar, &left, &right, hovered)` with `StatusReadout::action(text)`; it returns each readout's rect to hit-test. `status_bar::height` grows with the `Caption` line at a large text scale |
| An error or warning the person has to act on, that stays until resolved | `Banner` (in the flow, above the content it concerns), with the action that resolves it inside it (`.with_action(button)`) |
| A standing notice where nothing is wrong (a private window, a read-only copy) | `Banner::neutral(..)` |
| A transient result or a recoverable error ("Copied", "Upload failed, retrying") | `Toast` (`ToastStack`, or `toast::paint_transient` for a painted app) |
| A document's save state: "Saving…", "Saved", "Save failed" | `ui::SaveStatusSurface`: `set(SaveState::from(&record_save_status), now)` every frame, `paint(scene, t, tr, theme, area, now)` from `draw_overlay`, route clicks with `click(p)` (`SaveAction::Retry` / `Dismiss`). Autosave's saving and saved never show (expose them in the app's accessibility status via `state()`); a failure is an error card with Try again that stays until the save succeeds, and that success is a one-time "Saved" bubble. Delete any save pill, pinned bubble or hand-drawn failure text |

StatusBubble, replacing the hand-rolled pattern (`pill_size` + a margin
rect + a reveal zone + `push_layer(alpha)` + `draw_pill` + `poke` on change):

```rust
// state: status_bubble: ui::StatusBubble
let now = ui::time::now();
self.status_bubble.show(&label, now);              // every frame is fine; pokes on change only
// in CursorMoved:
self.status_bubble.pointer_moved(self.rect, p, tr, theme, now);
// in draw (or draw_overlay):
self.status_bubble.paint(scene, transform, tr, theme, self.rect, now);
// in needs_animation_frame:
self.status_bubble.needs_frame(ui::time::now())
```

Delete the app's `status_shown` copy of the text, its `margin`/zone
constants, `text_size` literals and the alpha layer. Delete
`arm_status_bubble_for_shots`, `shots_status` and any forced-visible flag: a
plain `--shots` waits until the bubble has faded. A screenshot scenario that
wants the bubble in frame stages it in its stage hook with
`status_bubble.show(text, now)` followed by `status_bubble.set_hover(true,
now)`. A bubble that hosts controls (ebook's page arrows) keeps `pill_size`
and `draw_pill`, and draws its arrows with `ui::icons::Icon::ChevronLeft` /
`ChevronRight`.

### Scrims and media

- Any scrim colour literal (`Color::from_rgba8(0, 0, 0, a)`, `BLACK.with_alpha`)
  or app-painted modal dim → delete; the modal paints its own.
- Stepped bands (several rects of increasing alpha, a hard 64 px band) under
  text on media → one call:
  `ui::overlay::scrim::vertical_fade(scene, t, rect, FadeEdge::Bottom, 0.7, theme)`,
  and draw that text in `theme.tokens.colors.media.ink` (secondary:
  `media.ink_dim`).
- Controls over media (capture toolbars, editor transport, photo info) → a
  `ui::widget::floating_surface::ControlBar`: `ControlBar::bottom_centre(area,
  count, theme)` (or `size`), `bar.paint(scene, t, text, theme, rect,
  &[BarControl::new(icon, name).toggled(on).enabled(ok)])`, and
  `bar.handle(event, scale)` returns the control a click activated. It owns
  the panel, the `control_inset(theme)` around the buttons (panel radius less
  control radius, so their corners run concentric), the gaps and the hit
  rects; delete the app's constants for them. Anything else over media is a
  floating Panel shaped to it:
  `ui::widget::floating_surface::paint(scene, t, rect, theme)`, inked with
  `let ink = theme.on_ground(ui::chrome::panel_ground(theme));`
  (`ink.text_primary`, `ink.text_dim`).
- A floating surface attached to an app (a call, picture in picture) that
  must keep clear of the window's header row →
  `FloatingPlacement::set_safe_insets([0.0, header, 0.0, 0.0])`; keep
  `set_attached_chrome` for the surface's own decoration.

- The area around a frame (letterbox, an empty viewfinder) →
  `theme.tokens.colors.media.letterbox`, never `surfaces.base`.
- An idle or error state over media → text only; never draw a play glyph
  or a ring under it.

### History

History is one window control: a Plain `Icon::History` button named
"History", which the framework puts at the trailing edge of the `NavShell`
header row and which reads as on while History shows. Apps place nothing.

| Before | After |
|---|---|
| `ui::widget::history_button()` in an app's toolbar | Delete it. Build the window with `NavShell`; its header row holds History. |
| A "History" text button, menu item or glyph an app or shell draws itself | Delete it; the shortcut, the palette and the header row's control are the doors. |
| Top padding or a band an app added under the window's History bar | Delete it. An app with a `NavShell` has no bar; any other app's bar is the framework's. |

- An app without a header row (a single canvas or form) keeps the window's
  History bar: the same glyph at its trailing edge, nothing else.
- A painted app with a header row of its own (an editor's file name and
  mode strip, a viewer's toolbar) lays the row out with
  `chrome::HeaderRowFace`, provides `widget::WindowHeader` and keeps a
  `widget::HistoryOpener`; the History control then stands at
  `face.history_rect()` and the window draws no bar above it. A media app
  may place the control in its floating strip over the media instead
  (`chrome::paint_history_control` at any rect, presses through the
  opener).

| Painted app before | After |
|---|---|
| A History bar drawn by the window above the app's own header | `impl WindowHeader for App { fn set_history_control(&mut self, c) { self.history.set_control(c) } }` and `ui::provides!(.., dyn ui::widget::WindowHeader)`. |
| Space reserved under that bar | Delete it; the app's header row is the window's top edge. |
- The header control is bound to the window's History door. The App, the
  headless harness and `native_app_audit` all resolve that door (and the
  search door) as the window's, so delete any audit filter for
  `nav-shell-history` / `history-button` `stale-action-binding` issues.
- A framework app whose resource keeps no document state (its state lives on
  the device or in a service: a speed test) sets
  `const DOCUMENT_HISTORY: bool = false` on its `ResourceApp`: no History
  control, no History actions.
- A view is built again for every past version History shows. Such a build
  must start no work: check `cx.is_past()` (`SurfaceCx::is_past()` in a
  surface) and build the surface without its worker, network or file
  watches, presenting what the document holds.
- A scope without History (`HistoryPresentation::External { reason }`) shows
  its reason as an empty state where the timeline would stand; write the
  reason as a sentence a person reads ("Forecasts are not recorded.").
- `HistoryScopeDefinition::title` is a short noun ("Notebook", "Saved
  places"). Delete descriptions and developer notes from it: "(not
  recorded)", "(forecasts are not recorded)", "Process output (not
  recorded)".

### Shots

1. Delete the `standalone_capture()` branch in `main.rs` that builds a
   separate `LaunchSpec` or root for `--shots` (clocks, maps, settings,
   themes, typst-editor, sheets, video-player, weather). `main` is
   `ui::framework::run::<App>()`. Move each scenario:
   - resource state → `ResourceApp::scenarios()` names it and
     `ResourceApp::scenario(Some(name))` returns the actions that stage it;
   - view state (a selection, an open dialog, a scroll offset, a pushed
     detail page) → `ViewCx::on_stage` or `ResourceSurface::stage`;
   - a narrow or phone-sized scenario → `ResourceApp::scenario_viewport`:
     ```rust
     fn scenario_viewport(name: &str) -> Option<(u32, u32)> {
         name.ends_with("-narrow").then_some((520, 820))
     }
     ```
2. Replace `chrono::Local::now()` with `ui::clock::local_now()` everywhere
   (live and capture code alike) and delete per-app fixed demo clocks (maps'
   `demo_clock_base`): a capture already shows 30 June 2026, 10:42.
3. Delete `std::thread::sleep`, manual frame loops and
   `ImageStore::block_until_idle` before a capture: the launcher settles
   animations and image loads (`CAPTURE_SETTLE_BOUND`).
4. Capture on the content ground: `render_window_png` → `render_png`; never
   pass `Color::TRANSPARENT` to a shot.
5. A shot handler that drives `ShotFrame` itself draws the top layer too:
   `let mut scene = frame.draw(&mut root); frame.draw_overlay(&mut root, &mut scene); frame.write_png(&scene, out);`
   (`ui::launch::render_widget_png` does all three).
6. Light shots through a remote cargo drop-in: pass `--theme light` (the
   environment is not forwarded). A shot is neutral, as a window without a
   shell is; `--workspace-hue DEGREES` and `--your-colour NAME` show it as
   the shell would in a workspace ([Window appearance](#window-appearance)).
7. History mode: pass `--history` with `--shots` or `--responsive-matrix`;
   the capture opens History first (`FrameHarness::open_history`), as the
   History shortcut does.
8. Search: pass `--search QUERY` with `--shots`; the capture opens this
   window's search with the query typed (`FrameHarness::open_search`), the
   palette an app's search launcher opens, over the app's own providers.

### What to delete in an app

- Every `ModalFrame`, `PopoverWidget`, `ui::overlay::Modal`/`Popover`/`Scrim`
  use, per the tables above.
- Hand-drawn option lists, menus and pickers with an accent-filled selected
  row → `Popover::new()`.
- App-painted scrims, dims and "overlay" fills behind dialogs.
- Status strips painted as a raised band → `chrome::status_bar`.
- Error text drawn in red over content → `Banner` or `Toast`.
- `arm_status_bubble_for_shots` and every forced-visible shot flag.
- Separate capture roots, demo clocks and pre-capture sleeps.

## Floating material

Floating chrome is drawn over a blur of what is behind it, tinted with the
panel ground at 72 %, and falls back to the opaque panel ground wherever the
blur is unavailable. Surfaces name the material; they never choose a blur
radius, an opacity or a tint.

### New API

| API | Use |
|---|---|
| `ui::floating_surface::Material::Floating` | The material of every floating surface: menus, popovers, the palette, sheets, toasts/notifications, chrome over media (video controls, photo info). Default. |
| `ui::floating_surface::Material::Opaque` | A floating surface whose backdrop must not show through (a secret or credential prompt over content the user does not own). |
| `ui::floating_surface::paint_material(scene, transform, &shape, material, theme)` | Fill a floating surface's body. `shape` is any `kurbo::Shape`, normally `RoundedRect::from_rect(rect, radius)`. |
| `ui::surface::panel(scene, transform, rect, radius, theme)` | The whole Panel row: `Material::Floating` body plus the `panel_border` edge and, on a dark material, the `floating_edge`. Unchanged signature; its body is now the material. |
| `Theme::reduce_transparency` / `Theme::with_reduce_transparency(bool)` | The reduced-transparency accessibility setting. Read only by the toolkit; apps do not branch on it. |
| `AccessibilityLayer::reduce_transparency` | Settings document field `appearance.accessibility.reduce_transparency` (bool, default `false`). |

### Replace

Apply in this order. "Body fill" means the first `scene.fill` that paints a
floating surface's background, before its content.

1. Body fill + `panel_border` stroke on `ui::surface::edge_outline(rect, radius)`
   (or on the same rounded rect) → one call:
   ```rust
   ui::surface::panel(scene, transform, rect, radius, theme);
   ```
   Delete both the fill and the stroke.
2. Any remaining body fill of a floating surface → `paint_material`, keeping the
   caller's own edge/shadow/content calls:
   ```rust
   // before
   scene.fill(Fill::NonZero, transform, ui::chrome::panel_ground(theme), None, &shape);
   // after
   ui::floating_surface::paint_material(
       scene, transform, &shape, ui::floating_surface::Material::Floating, theme,
   );
   ```
   This applies whatever the old colour was: `chrome::panel_ground(theme)`,
   `chrome::shell::panel_ground(theme)`, `colors.surfaces.overlay`,
   `colors.surfaces.glass`, `theme.material_glass()`, `colors.glass_film()`,
   `ChromeRole::Panel.surface(theme)`, or any of those with `.with_alpha(..)` /
   `.with_alpha_factor(..)`.
3. A body painted translucent to "look like glass" (a fill with alpha below 1,
   a translucent `surfaces.raised`/`surfaces.overlay`, a scrim drawn inside the
   panel to keep text legible) → step 2 with `Material::Floating`. Delete the
   alpha and the in-panel scrim.
4. Chrome over media (player controls, photo/info overlays, capture toolbars)
   → step 2 with `Material::Floating`, shaped to the control cluster. Delete
   the gradient or scrim that kept them legible.
5. Do **not** use the material for anything that is not floating: window
   grounds, sidebars, inspectors, cards, rows and fields inside content stay
   opaque surface tokens.

### Delete

- `ui::chrome::shell::CHROME_SURFACE_ALPHA` is gone. Remove any
  `.with_alpha_factor(CHROME_SURFACE_ALPHA)`; the colour it multiplied is the
  opaque `panel_ground`.
- Shell-only constants `POPOVER_SURFACE_ALPHA`, `MODAL_SURFACE_ALPHA` and
  `panel_surface(theme, alpha)` are gone; use `ui::chrome::shell::panel_ground(theme)`
  for colours derived from the panel, `paint_material` for the body.
- Any app-side blur radius, `material_blur_px` read, or "fake blur" (a blurred
  rounded rect behind a panel, a lowered-alpha copy of the content) — the
  material owns it.

### Where the blur actually renders

Everywhere a frame is presented: the shell's own scene (quick settings, the
palette, shell menus and sheets), an app's own window, and every capture
(`FrameHarness::render_png`/`render_rgba`, `ShotFrame::write_png`, so every
`--shots`). A menu, popover, dropdown list, context menu, tooltip, sheet,
toast or the action palette over content shows the same blur live and in a
shot; with reduced transparency or forced colours it is the opaque panel
ground in both. Nothing to do in an app: the call is the same. Do not
special-case either path.

A surface stays opaque only where nothing composes what is behind it: a
scratch `Scene` a widget draws into and never appends, or a cache it appends
without `backdrop::append_nested` (below).

### Settings app

The reduced-transparency setting needs a switch next to "Reduce motion" in
Accessibility. Persist it as `reduce_transparency` in the accessibility layer
(`ui::theme::AccessibilityLayer { reduce_transparency, .. }`); label it
"Reduce transparency" with the description "Make menus, panels and sheets
opaque".

### Renderer hosts (toolkit and shell only)

A host that draws floating surfaces and renders the result itself:

1. Paint inside a recording:
   `let rec = ui::backdrop::BackdropRecording::begin(); /* draw */ let surfaces = rec.finish();`
   Name every scene you draw into first with `ui::backdrop::layer_scene(&scene, n)`;
   a surface drawn into an unnamed scene stays opaque.
2. Single scene (named layer 0): `ui::backdrop::BackdropFrame::from_single_scene(surfaces)`.
   Several scenes composed afterwards: give each its own layer and rebuild each
   region's `below` from the scenes composed under its layer (see
   `ShellSceneSections::below_layer`). The scenes must not move while the
   recording is active.
3. Renderer: `OffscreenRenderer::set_backdrop(Some(Arc::new(frame)))`, or with a
   raw vello `Renderer`, `BackdropBlur::prepare(..)` before the frame's render
   and `BackdropBlur::release(..)` after it.
4. Shell: `FrameBuilder::paint_with_damage` puts it on `FramePaint::backdrop`;
   after `FrameBuilder::paint` take it with `FrameBuilder::take_backdrop()` and
   hand it on with `FramePaint::with_backdrop` or `OffscreenRenderer::set_backdrop`.
5. A frame painted in passes (a main pass, then a top layer drawn later into
   the same scene): finish the first recording and resume it with
   `BackdropRecording::continuing(surfaces)` for the next pass, naming the
   scene again, so the frame's regions keep distinct placeholders
   (`ShotFrame::draw` then `ShotFrame::draw_overlay` do this).
6. A widget that draws children into its own cache scene and appends it
   later (`Column`, `Row`): `backdrop::nest_scene(&cache)` before drawing
   into it, then `backdrop::append_nested(scene, &cache, transform)` instead
   of `scene.append(..)`. It returns how many floating surfaces the cache
   carried; while it carried any, redraw the cache every frame instead of
   reusing it, or the reused surfaces show the opaque ground.
7. App window (`ui::App`, done by the framework): `RenderRequest::Frame`
   carries the frame's `backdrop`; the render thread prepares the blur on
   its own renderer before rasterising and redraws in full while a backdrop
   is present and on the frame after.

## Theming

A theme is one versioned document (`ui::theme::ThemeDocument`, schema 6) of
semantic roles; `docs/architecture/theming.md` is the contract. Apps read
tokens and never a theme's name, and Settings and Theme Studio are the only
writers of the appearance state.

### Removed APIs and their replacements

| Old | New |
|---|---|
| `CustomThemeSpec`, `CUSTOM_THEME_SCHEMA_VERSION`, `CUSTOM_THEME_SCHEMA_MIN`, `Theme::from_custom_spec` | `ThemeDocument`, `THEME_SCHEMA_VERSION`, `Theme::from_document` (`None` for a document this build does not admit) |
| `CustomThemeSpec::{spacing_px, font, motion_scale, material_scale, icon_scale, chrome_weight, motion_character, dark_base, neutral_hue}` | `density.spacing` (a multiple of the 4 px scale), `type.{face, reading, base_size, distinct_letters}`, `motion.character`, `material.{blur, tint_opacity}`, `palette.{dark, hue, chroma}`; icon scale, chrome weight and the duration scale are not theme roles |
| `tokens::Material`, `Colors::of_material`, `Tokens::of_material`, `colors.material`, `colors.neutral_hue` | `tokens::Palette` (`Palette::dark()` …), `Colors::of_palette` / `Colors::stock`, `Tokens::around`, `colors.palette`, `colors.palette.hue` |
| `Theme::high_contrast()` as the accessibility track | `theme.with_high_contrast()` (the high-contrast palette of the theme's own polarity); `Theme::high_contrast()` is the dark preset |
| `Theme::ALL = ["Dark", "Light", "High Contrast"]`, the name `"High Contrast"` | `Preset::ALL` / `Theme::ALL` (seven presets, sentence case: `"High contrast"`, `"High contrast light"`). Never compare `theme.name`; ask `theme.colors_are_forced()` |
| `Theme::with_your_colour([u8; 3])`, `Colors::set_you(Color)`, `DEFAULT_YOU_SLOT` | `Theme::with_your_colour(PresenceHue)`, `Colors::set_you(PresenceHue)`, `PresenceHue::Blue`; a host colour becomes `PresenceHue::nearest(colour)` |
| `PIN_ICONS`, `PIN_CHROME`, `DistributionDefaults::{palette, custom}`, distribution schema 1 | `PIN_EMPHASIS`, `PIN_PRESENCE`, `PIN_SHAPE` beside `PIN_PALETTE`, `PIN_DENSITY`, `PIN_TYPE`, `PIN_MOTION`, `PIN_MATERIAL`; `DistributionDefaults::{theme, document}`, schema 2 |
| `ThemeLayerStack::user_theme: Option<Theme>` | `Option<ThemeDocument>`, plus `your_colour: Option<PresenceHue>` |
| `appearance.theme` (`"Dark"`, `"Light"`, `"Automatic"`, `"custom:<id>"`), `appearance.custom_theme`, `appearance.studio`, `appearance.app_accents`, `appearance.workspace_tint` | `appearance.mode`, `dark_theme`, `light_theme`, `custom_themes`, `density`, `workspace_temperatures`; `identity.colour` |
| `day_schedule::AUTOMATIC_THEME` (`"Automatic"`) | `day_schedule::AUTOMATIC_MODE` (`"automatic"`), read from `appearance.mode` |
| `backdrop::FLOATING_TINT_ALPHA` | `surfaces.material_opacity` (a theme role) |
| A primary label resolved from `Color::WHITE` | `colors.intent.on_inverted` |
| `DEFAULT_NEUTRAL_HUE` (hue 255 as every palette's fallback) | none: `Palette::hue` is `Option<f32>`; `None` (the light, dark and high-contrast presets) is pure grey until the window's workspace gives it a hue |
| `ui::workspace_hue(..) -> f32` | `-> Option<f32>`, `None` for a wallpaper without a hue |
| `ThemeLayerStack::{workspace_hue, your_colour}` | `ThemeLayerStack::window: WindowAppearance`, set by the host that shows the window |
| `Theme::configured` applying `identity.colour` | the host applies the colour (`Theme::in_window`); the shell sends it to each app window, a standalone app reads the stored choice once |
| `collaboration::Collaborator::color: [f32; 3]` | `Collaborator::colour: Option<PresenceHue>` (packed on the roster wire with `Collaborator::packed_colour`) |

### Window appearance

Where and for whom a window is shown is not part of its theme:
`ui::WindowAppearance { workspace_hue, your_colour }`, applied over any
theme with `Theme::in_window`. Apps do nothing to receive it. The shell
sends it over the app's shell-ui connection
(`FloatingSurfaceHostMessage::Appearance`) and `App` applies it on every
rebuild (`App::set_window_appearance`, `App::window_appearance`), so the
temperature and the person's colour survive settings and contrast changes.
A host that embeds windows itself (a test harness, another shell) calls
`App::set_window_appearance` directly.

A capture shows a window without a shell, neutral and in the default
colour. To show it in a workspace, pass what the shell would send:

```bash
cargo run -p <pkg> --bin <bin> -- --shots target/look/<dir>/warm.png \
  --viewport 1280x800 --theme light --workspace-hue 60 --your-colour violet
```

### Settings > Appearance

The Settings app exposes these and nothing else about appearance. Every
control writes the shared settings document through the same actions an
agent calls; labels are sentence case.

| Control | Writes | Values |
|---|---|---|
| **Appearance** (segmented: Light, Dark, Automatic) | `appearance.mode` | `"light"`, `"dark"`, `"automatic"`. Automatic shows the schedule controls already in Settings (`schedule`, `sunrise`, `sunset`) |
| **Dark theme** (a row of rendered previews, the four dark presets and the person's dark custom themes) | `appearance.dark_theme` | `"dark"` (default), `"dark-cool"`, `"dark-graphite"`, `"dark-soft"`, or `"custom:<id>"` for a document in `appearance.custom_themes` |
| **Light theme** (previews: Light and the person's light custom themes) | `appearance.light_theme` | `"light"` (default) or `"custom:<id>"` |
| **Your colour** (the twelve presence hues as swatches, with names for screen readers) | `identity.colour` | `"red"`, `"orange"`, `"amber"`, `"olive"`, `"green"`, `"teal"`, `"cyan"`, `"sky"`, `"blue"`, `"violet"`, `"magenta"`, `"pink"` (`PresenceHue::id`); absent means the colour derived from the identity, which the control shows selected |
| **Density** (Compact, Regular, Roomy) | `appearance.density` | `0.85`, `1.0`, `1.2` (`ThemeDensity::{COMPACT, REGULAR, ROOMY}`) |
| **Text size** | `appearance.accessibility.text_scale` | 0.5–4, stock steps 1, 1.15, 1.3, 1.5 |
| **Reduce transparency** — "Make menus, panels and sheets opaque" | `appearance.accessibility.reduce_transparency` | bool |
| **High contrast** | `appearance.accessibility.high_contrast` | bool; keeps the mode's polarity |
| **Open Theme Studio** | — | launches `so.tilo.themes` to write a theme |

The three accessibility rows write the versioned `appearance.accessibility`
object (`AccessibilityLayer`, `schema_version` 1) as a whole; once it is
present the flat `appearance.high_contrast` / `text_scale` flags are not read.

Delete from Settings: the old theme list (`Dark`, `Light`, `High Contrast`,
`Automatic` as one choice), the custom-theme summary, the "app accents"
switch and any workspace tint control. Settings writes no theme document
itself; Theme Studio writes `custom_themes`.

### Theme Studio

`apps/themes` is the reference for authoring: its resource stores each
theme role by role (`themes::Studio`), every edit is an action, the gallery
renders each preset with `themes::preview::ThemePreview`, and applying goes
through `themes::apply` (refused while `ThemeDocument::findings` is not
empty). `--shots --scenario gallery|editor|tinted|illegible` with
`--theme <preset>` renders it in any preset.

## Library changes awaiting app reconciliation

These toolkit changes landed while app migrations were in flight. Apps
merged before them were updated with them; every app below still has to
take them when its migration is reconciled.

- **Menus paint in the top layer.** `ContextMenu::draw` paints nothing; the
  menu paints in `draw_overlay`. A painted app that calls `menu.draw(..)`
  draws its menu from its own `draw_overlay` (or returns the open menu from
  `children_mut`), and a host that owns a `TreeView` routes `draw_overlay`
  to it. Apps that hold a `ContextMenu`: browser (`browser_tabs.rs`
  `tab_menu.draw`, `page_menu.rs`, `browser_widget.rs` `back_menu`), chat
  (`events.rs`), contacts, db-client, feeds (`app.rs` `feed_menu.draw`),
  file-explorer (`context_menu`, `submenu`), mail, maps, monitor, music,
  notes (`app.rs` `note_menu.draw`), osk, pdf-viewer, slides, terminal,
  video-editor (`app.rs` `bin_menu.draw`), weather, word-processor. Apps
  that own a `TreeView`: code-editor, db-client, notes, settings.
- **Menus open at their trigger.** A header or toolbar button opens its menu
  with `open_anchored(button_rect, window)`; the toolkit flips and shifts
  it. Delete any hand placement (notes' note menu, every `open_at` of a
  button's menu).
- **The armed row is a lift.** `paint_armed_row` takes the row's `inset`;
  painted menus (none in apps today) pass `menu_inset(theme)`.
- **Unread is your colour.** `ChipStyle::Live` and `Badge::count` fill in
  your colour. An app that painted an unread or attention count in
  `semantic.danger` uses `Badge::count` / `NavCount::Live` instead; one that
  used `Live` for an error uses `Tinted(Tone::Danger)`.
- **Segments size to their labels.** A painted segmented strip splits its
  rect with `SegmentedFace::partition` instead of equal shares: backups,
  chat, diffmerge, disks, feeds, firewall, mail, osk, passwords, pdf-viewer,
  photos, remote, totp, video-editor, vpn.
- **Save status is a framework surface.** Notes replaces its save pill and
  pinned bubble with `SaveStatusSurface`; any other app that shows save
  state (code-editor, sheets, typst-editor, word-processor) does the same.
- **The block editor owns its handle column.** Hosts of a `BlockEditor`
  (chat, ebook, mail, notes, pdf-viewer, slides, word-processor) delete the
  gutter they reserved for its drag handles.
- **A scroll bar takes its lane only while it is needed.** A `Scrollable`
  reserves `metrics.scrollbar_gutter` (now 16: the thumb plus `s1` each
  side) on its trailing edge only while its content overflows, and on its
  bottom edge for a horizontal bar; a horizontal `Scrollable` lays its
  content out at max-content width. Rows painted beside a scroll view to
  "end where the content ends" drop their own gutter padding.
- **Absent children take no gap.** A zero spacer (`Spacer::token(S0)`) is out
  of the flow, so it no longer doubles a column's gap; bound text that
  renders nothing and an empty `ModelView` collapse too. Say the gap with
  `with_rhythm` instead.
- **Forms read once and line up once.** A `Field`'s label is the control's
  accessible name and is not a second node; all sections of a `Form` share
  one label column; help reads under a beside label; `FormPage` titles in
  `LargeTitle` on the leading edge and no longer fills its own ground.
- **`ModalDialog` and `Popover` take no room in the flow.** Delete spacers or
  wrappers that made room for them.
- **Rows stand where they are laid out.** A `NavList` (or any `TreeView`)
  handed new sections before its first layout no longer animates its rows
  from the stock 28 px row, and a finished transition always leaves rows at
  their laid-out geometry. Delete workarounds that rebuilt the list until it
  had been laid out, or held `set_sections` back until then (mail and feeds
  are done).
- **The lifted row floats beside the pointer.** A dragged tree row now
  hangs below and beside the pointer, and the drop line paints above it, so
  the place it lands is never hidden; nothing to change in an app.
- **The sidebar collapses for the content's room.** `compact_breakpoint` is
  840 (`sidebar_width` 280 + `content_min_width` 560): tests or shots that
  pinned a docked sidebar at 720–839 px wide now see the drawer.
- **The header row is a capability.** Every `NavShell` app replaces its
  `std::mem::take(&mut self.shell).content_header(..)` /
  `inspector_header(..)` rebuilds with `set_content_header` /
  `set_inspector_header`, and any `Decorator` drawn over a header with a
  title, `set_*_count` or `content_header_leading`: backups, clocks,
  contacts, containers, feeds, firewall, logs, mail, monitor, passwords,
  snippets, vpn, windows. Hand-placed Back buttons become `with_back`
  (disk-usage, print-centre, store, tube). `with_filter` takes a
  `SearchField` (logs). Painted apps with a header row of their own take
  the window's History control through `WindowHeader` + `HistoryOpener`
  and lay the row out with `HeaderRowFace` (hex-editor, photos,
  video-editor, typst-editor, calculator, music, git-client, maps,
  sheets); `SidebarDrawer` hosts move to `drawer.header(..)` and
  `drawer.handle_event(..)` (code-editor, music, chat, video-player,
  word-processor, video-editor).
- **An open drawer leaves the window in place.** A `NavShell` drawer opens
  under the header row; tests that expected only the drawer and its toggle
  among the exposed children now also see the header row's controls, and
  the toggle stays in the row instead of moving into the drawer.
