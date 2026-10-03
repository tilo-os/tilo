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
| `CustomThemeSpec` (every schema), `Theme::from_custom_spec`, `appearance.theme`, `appearance.custom_theme` | `ThemeDocument` schema 5 (`Theme::from_document`); see [Theming](#theming) |
| `ContentSurface::accent` | delete |
| `QueryInk::accented(text, placeholder, accent)` | `QueryInk::of(&theme.tokens.colors)` (caret and selection in your colour) |
| `palette.surface_radius_px()` (`CommandPalette`) | `CommandPalette::surface_radius_px(theme)` (the panel radius) |
| `ui::presence::PEER_PALETTE` (24 entries), `user_color` returning Tailwind colours | 12 entries, the dark presence palette; `user_color` is an identity, paint it with `colors.presence.of_rgb(user_color(id))` |
| `Theme::configured()` reading the person's colour | it reads `identity.colour`; an app does nothing |
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
| the armed row of a menu, popover, dropdown, slash menu or result list (the row Enter or a click activates) | `colors.intent.inverted` + `colors.intent.on_inverted`, exactly like a primary action |
| the background of a selected row, tab, segment, chip or grid cell (a list, table, sidebar, segmented control) | a lift of the surface it sits on: `colors.lift_on(ground, 1.0)` (hover `0.5`) with `colors.text_primary` ink. On the window ground use `colors.intent.selection` / `selection_hover`; inside a floating panel `ground = colors.surfaces.overlay`; on a segmented track `surfaces.raised` |
| a translucent accent wash (`accent.with_alpha_factor(a)`) used as a hover/selected film | `colors.text_primary.with_alpha_factor(a)` or the matching veneer (`overlay_subtle` ≤ 0.15, `overlay_medium` ≤ 0.3, else `overlay_strong`) |
| the text caret, the focus ring, a drop/insertion indicator, selection handles | `colors.intent.you` (`theme.border_focus` is the same colour) |
| the text-selection highlight | `colors.intent.selection_tint` (already translucent; do not add alpha) |
| a link | `colors.text_primary` with an underline |
| an "active" icon or label tint | `colors.text_primary` (inactive stays `text_dim`) |
| live / syncing / in-flight state | `colors.semantic.info`; recording/destructive state `colors.semantic.danger` |
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
| `ui::TextKind::{Prose, Numeric, Secret, Identifier, Code}`; `Text::with_kind`, `TextStyle::with_kind`, `ColumnDef::with_kind` | What text is, for the OpenType features the toolkit picks against the face ([Text kinds](#text-kinds)). |
| `TextInputKind::Secret`, `TextInputKind::text_kind()` | A code or key typed in the clear; every field kind sets its text kind while typing. |
| `ui::UiFace::{Default, Hyperlegible}`, `Typography::set_face(name)`, `Typography::face_name()`, `UI_HYPERLEGIBLE_FAMILY` | Theme authors: the UI face by the name a theme document stores. |
| `TextStyle::with_content_size(px)` / `Text::with_content_size(px)` / `TextStyle::content(px)` | Text sized by its content (a canvas, a slide, a document's own type): the size is used as given and never grown by the text scale. |
| `PaintCtx::text_role`, `measure_role`, `line_box`, `text_role_in_band` | Custom painters: paint, measure, size a row, or centre one line in a band in a role. |
| `ui::widget::ControlSize::{Small, Regular, Large}` | A control's size: label role (`Label`, `Label`, `Body`) and height (`bar_control_height`, `min_pointer_target`, `bar_height`). |
| `with_text_role(role)` on `TextInput`, `Autocomplete`, `EditableLabel`, `Link`, `RichTextLine`, `RichTextEditor`; `set_text_role` on `TextInput`, `EditableLabel`, `RichTextLine`; `TextInput::text_role()`, `BlockEditor::text_role()` | The role a text widget sets its text in. `RichTextEditor` maps `Reading` to the serif and `Code` to the mono family. |
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
| `TextInput::with_font_size(px)` / `set_font_size(px)` | `with_text_role(role)` / `set_text_role(role)` (default `Body`) |
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
| Usernames, handles, email addresses, invite codes, room names shown for verification, other people's file names, URLs in permission prompts and link previews | `.with_kind(TextKind::Identifier)`; an input: `TextInputKind::Username` / `Email` / `Url` / `Path` |
| `.with_font_family(FontChoice::Monospace)` on source or terminal text | `.with_kind(TextKind::Code)`, or the `Code` role |
| A numeric `DataTable` column drawn by hand to line digits up | `ColumnDef::with_text(…).with_kind(TextKind::Numeric)` |
| `.with_font_feature(*b"ss01", 1)` or any stylistic set for a look | Delete; the face is the theme's |

### The UI face

The system UI face is Instrument Sans; Greek and Cyrillic fall back to faces
matched to its x-height and weight (Geologica, Onest), Vietnamese to Noto Sans
([Script fallbacks](typography.md#script-fallbacks)). A theme or the
accessibility layer may choose the built-in `Hyperlegible` face instead. Code
never names a family: `TextRole` picks sans, serif (`Reading`) or mono
(`Code`), and `FontChoice::{SansSerif, Serif, Monospace}` remains for the rare
explicit choice.

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
| `bg_primary` filling a navigation column (folders, mailboxes, channels, collections, places, a library rail) | Delete the fill. With `NavShell`, pass the column to `.with_sidebar(..)`; in a painted app call `ui::surface::region(scene, transform, rect, Region::Sidebar, theme)` once for the column. |
| `bg_primary` filling a detail/inspector column (properties, metadata, an info panel beside the main view) | Delete the fill; `ui::surface::region(scene, transform, rect, Region::Inspector, theme)`. |
| `bg_primary` filling a header, toolbar or status band | Delete the fill. The band stands on its region; draw one hairline under it with `ui::surface::separator(scene, transform, x0, x1, band.y1, 0.0, ground)` where `ground` is the region's ground. |
| `bg_primary` as a card, field, well, key cap, chip, avatar ring or a button's rest face inside content | `theme.tokens.colors.surfaces.raised` (an avatar ring takes the ground it sits on instead). |
| `bg_hover` / `bg_active` as the hover / pressed (or open, focused, selected) fill of a control with a filled rest face | Use the toolkit control (`Button`, `Checkbox`, `Dropdown`, `TextInput`, `SegmentedControl`, `NavShell`, `Table`) and delete the painter. Where the painter stays: `colors.lift_on(rest, 0.5)` for hover, `colors.lift_on(rest, 1.0)` for pressed/open/selected, with `rest` the face's own fill (normally `surfaces.raised`). |
| `bg_hover` / `bg_active` under a transparent control or row (a ghost button, an icon button, a list row, a grid cell) on the window ground | `colors.intent.selection_hover` / `colors.intent.selection`. Draw the row with the region's theme (below) and the same tokens are right in a sidebar or inspector too. |
| the same inside a floating panel (menu, popover, picker) | `colors.lift_on(ui::chrome::shell::panel_ground(theme), 0.5)` / `lift_on(.., 1.0)`; a menu's armed row is inverted, see [Choosing the intent](#choosing-the-intent). |
| `bg_hover.with_alpha_factor(a)` as a wash (current-line band, gutter tint) | `colors.lift_on(ground, 0.5).with_alpha_factor(a)`, with `ground` the surface under the wash. |
| `Theme { bg_app, bg_primary, bg_hover, bg_active, .. }` struct literal | Delete the four fields; better, build the theme with `Theme::dark()` / `light()` and change tokens. |
| `theme.bg_app != other.bg_app` as "the theme changed" | `theme.tokens.colors.surfaces.content != other.tokens.colors.surfaces.content` |

### New API

| API | Use |
|---|---|
| `ui::surface::Region::{Content, Sidebar, Inspector}` | The region an area of the window is. |
| `ui::surface::region(scene, transform, rect, region, theme) -> Color` | Paint a region's ground over `rect` (plus the shared-edge hairline where the material gives region and content one ground) and return the ground. For painted apps; `NavShell` calls it for its sidebar. |
| `Theme::region(region) -> Color` | A region's ground, without painting (to measure against). |
| `Theme::in_region(region) -> Theme`, `Theme::on_ground(ground) -> Theme` | The theme for what is drawn on that ground: `surfaces.content` re-pointed to it, ink tiers re-measured, `intent.selection` / `selection_hover` stepped from it. Pass it to the children's `draw`. |
| `ui::surface::region_needs_edge(ground, content)`, `ui::tokens::colors::REGION_MIN_STEP` | Whether a tier is too close to content to separate by tone (only the high-contrast material). |
| `theme.tokens.spacing.rhythm.{tight, related, sibling, section, group}` (`ui::tokens::Rhythm`) | Gaps by relationship (4 / 8 / 12 / 16 / 24 at 1×), scaled with `ui_scale` and a custom spacing. |
| `FrameHarness::render_window_png` / `render_window_rgba` | The frame over the window ground; `--shots` and `--responsive-matrix` write this. |
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

## Controls

## Content widgets

## Search

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
| `MODAL_SCRIM_ALPHA` (`ui::widget::modal_frame`) | `ui::overlay::scrim::MODAL_STRENGTH`; better, `ui::overlay::scrim::modal(scene, transform, window, openness, theme)` |
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
| `ui::widget::history_button()` | The History button for an app's own toolbar; the window then draws no History bar. |
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
| A change worth noticing once: "Saved", "Synced", a page turn, a zoom step | `StatusBubble` |
| An error or warning the person has to act on, that stays until resolved | `Banner` (in the flow, above the content it concerns) |
| A transient result or a recoverable error ("Copied", "Upload failed, retrying") | `Toast` (`ToastStack`, or `toast::paint_transient` for a painted app) |

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
  floating Panel shaped to the controls:
  `ui::widget::floating_surface::paint(scene, t, rect, theme)`, inked with
  `let ink = theme.on_ground(ui::chrome::panel_ground(theme));`
  (`ink.text_primary`, `ink.text_dim`). Glyphs are `metrics.icon_md` in
  `metrics.min_pointer_target` hit rects that do not overlap.
- The area around a frame (letterbox, an empty viewfinder) →
  `theme.tokens.colors.media.letterbox`, never `surfaces.base`.
- An idle or error state over media → text only; never draw a play glyph
  or a ring under it.

### History

- Toolbar apps (logs, themes, weather and any app with its own header row):
  put `ui::widget::history_button()` into that row (trailing group). The
  window then draws no History bar; delete any top padding or band the app
  added under the bar.
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
   environment is not forwarded).

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

The compositor blurs floating surfaces drawn into the shell's own scene
(quick settings, the palette, shell menus and sheets, shell-hosted app
surfaces). In a separate app window the same call paints the opaque panel
ground: correct, never a hole, and it starts blurring with no code change once
that renderer runs the pass. Do not special-case either.

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

## Theming

A theme is one versioned document (`ui::theme::ThemeDocument`, schema 5) of
semantic roles; `docs/architecture/theming.md` is the contract. Apps read
tokens and never a theme's name, and Settings and Theme Studio are the only
writers of the appearance state.

### Removed APIs and their replacements

| Old | New |
|---|---|
| `CustomThemeSpec`, `CUSTOM_THEME_SCHEMA_VERSION`, `CUSTOM_THEME_SCHEMA_MIN`, `Theme::from_custom_spec` | `ThemeDocument`, `THEME_SCHEMA_VERSION`, `Theme::from_document` (`None` for a document this build does not admit) |
| `CustomThemeSpec::{spacing_px, font, motion_scale, material_scale, icon_scale, chrome_weight, motion_character, dark_base, neutral_hue}` | `density.spacing` (a multiple of the 4 px scale), `type.{face, base_size}`, `motion.character`, `material.{blur, tint_opacity}`, `palette.{dark, hue, chroma}`; icon scale, chrome weight and the duration scale are not theme roles |
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
