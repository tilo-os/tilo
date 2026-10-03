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

The system UI face is Tilo Sans, which sets Latin, Vietnamese and Greek;
Cyrillic falls back to Onest, matched to its x-height and weight
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
| `bg_primary` filling a navigation column (folders, mailboxes, channels, collections, places, a library rail) | Delete the fill. With `NavShell`, pass the column to `.sidebar(..)` ([Navigation](#navigation)); in a painted app call `ui::surface::region(scene, transform, rect, Region::Sidebar, theme)` once for the column. |
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
| Default docked width 320, default breakpoint 760 | 280 (`sidebar_width`) and 720 (`compact_breakpoint`); delete `.docked_width(..)` / `.breakpoint(..)` calls that restate the old defaults. |
| A collapse that the drawer forgot after the window crossed the breakpoint | Remembered per size class now; delete app code that re-applies it. |
| An app holding a `FlapState` for its own sidebar or side panel, with its own breakpoint, scrim and slide arithmetic | A `SidebarDrawer` (`.right()` for a trailing panel). `FlapState::tick(layout, dt)` is now `tick(layout, dt, theme.tokens.motion.durations.base())` and `slide()` is gone: `reveal()` is already eased. |

`TreeView`:

| Old | New |
|---|---|
| `.inset_rows()` | Delete; every tree insets its rows `s2`, so the depth-0 caret is never on the pane edge. |
| `.with_trailing_edge()` | Delete; regions are told apart by tone (`NavShell`, `surface::region`). |
| The current row's accent rail (`set_current`) | Unchanged call; the current row is now its label in the heavier weight. |
| A tree in a sidebar listing destinations (code-editor explorer, db-client schema, notes pages) | Keep `TreeView` for real hierarchies the person edits (files, schema, pages); it is a `NavShell::sidebar` child and paints the same lift. A flat or two-level list of places is a `NavList`. |
| `.with_ground(|t| t.tokens.colors.surfaces.sidebar)` (or another region tier) | Delete: the tree stands on the ground of the region its container hands it. Keep `with_ground` only for a tree on a raised well. |

Selection forks:

| Old | New |
|---|---|
| A private `selected_on(ground)`, `row_tier_on(ground, state)`, `selection_fill(theme, ground)`, `row_fill(..)`, `selection_fill_on_raised(theme)`, a `scene.fill` of `intent.selection` / `lift_on(..)` / `selection_tier(..).fill` under a row | `chrome::paint_row_selection(scene, transform, row.inset((-s2, -hairline)), SelectionState::new(selected, hovered, pressed), ground, theme)` where `ground` is the surface under the list (`theme.tokens.colors.surfaces.content` of the theme you draw the list with). Delete the private function. |
| A control with its own rest face whose states are `colors.lift(floor_ramp(state))` (ebook `floor_ramp`) | `chrome::selection_floor_ramp(theme, state, ground)`; delete `floor_ramp` and its constants. |
| An accent bar, left stripe, outline or coloured fill on the selected row | Delete it; the lift is the whole selection. |

### Rows: what each part of a `NavItem` means

- **Label**: sentence case, what the destination is called. It elides; it
  never wraps. The selected row's label is set in the heavier weight.
- **Icon**: one `ui::icons` icon per row, or none for the whole section. Use
  the same icon family as the app's other chrome. Never an emoji or a
  coloured dot.
- **Count**: `.count(n)` for how many items a place holds (messages in a
  folder, tracks in a playlist) — quiet tabular figures in tertiary ink.
  `.live_count(n)` only for things waiting for the person (unread, mentions,
  failed jobs): the one filled badge, in the person's own colour. Never a
  filled badge for a plain count, never a status colour.
- **Children**: nested places (`.children(..)`), indented one `s4` step, with
  a trailing chevron. `.expanded()` to start open.
- **Actions**: actions that apply to every row (rename, more) go in
  `NavList::item_actions(..)`; they show on hover and focus at the trailing
  edge. `.without_actions()` on rows where none apply (Inbox, Trash).
- **Section**: `NavSection::new(id, "Sentence case title", items)`; never
  upper case or letter-spaced. `.collapsible()` when the person may fold it,
  `.trailing_action(NavAction::new("new", Icon::Plus, "New playlist"))` for
  the one "new …" action of the section.
- **Editing**: `.editable_label()` plus `NavList::on_rename`; `.drop_target()`
  plus `NavList::on_move` for places that hold other places.

### New API

| API | Use |
|---|---|
| `NavShell::new()` with `.sidebar(e)`, `.overview(e)`, `.content(e)`, `.inspector(e)` | The window's columns. |
| `.sidebar_header(title, actions)`, `.overview_header(..)`, `.content_header(..)`, `.inspector_header(..)` | Each column's header in the one header row: a sentence-case title and the column's actions (icon `Button`s, a `ToggleGroup`). |
| `.with_filter(e)` | The one filter field of the window, under the sidebar's header (the overview's without a sidebar); Ctrl+F / ⌘F focuses it. |
| `.mode(NavMode::{Overlay, Push, Docked})` | What the sidebar becomes in a narrow window. |
| `.with_pane_state(&LocalState<NavPane>)`, `show(NavPane)`, `back()`, `front_pane()` | The front screen of a narrow window's push stack. |
| `toggle_sidebar()`, `open_sidebar()`, `close_sidebar()`, `is_sidebar_open()`, `with_sidebar_hidden()`; the same for the inspector | Show and hide; remembered per window size class. |
| `geometry()` (`NavGeometry`) | What the shell laid out, for tests and shots. |
| `NavList::new(sections)` with `NavSection`, `NavItem`, `NavAction`, `NavDrop` | The one navigation list (keyboard, disclosure, semantics, rename, drag, row actions). |
| `.selected(id)`, `.bind_selection(&LocalState<Option<String>>)`, `.on_select(..)`, `set_sections(..)`, `set_selected(..)`, `selected_id()` | Selection and updates. Choosing a destination in a narrow window closes the drawer or pushes the content by itself. |
| `.item_actions(actions, on_action)`, `.on_section_action(..)`, `.on_rename(..)`, `.on_move(..)` | What the person can do to rows. |
| `.rail()` | Icon over caption, for a docked rail. |
| `chrome::paint_nav_row(scene, transform, text, theme, &NavRowFace)`, `chrome::nav_row_layout`, `nav_row_height(theme)`, `nav_rail_row_height(theme)` | One navigation row in a painted app, identical to a `NavList` row. |
| `chrome::paint_nav_section(.., &NavSectionFace)`, `nav_section_layout`, `nav_section_height(theme, first)` | One section header in a painted app. |
| `chrome::paint_row_selection(scene, transform, rect, state, ground, theme)` | The hover and selection lift of any list row. |
| `chrome::selection_floor_ramp(theme, state, ground)` | States of a control with its own rest face. |
| `chrome::selection_tier_on(theme, state, ground)` | Now one surface step of `ground` (`SELECTION_MIN_STEP`, ΔL 0.06, on every ground). |
| `SidebarDrawer::paint_panel(scene, transform, theme) -> Theme` | A painted app's sidebar or inspector panel, docked or floating. |
| `HostShortcut::Sidebar` | Ctrl+Shift+L / ⌘⇧L: show or hide the sidebar in every app. |
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
| `Button::new(id, "", None).with_icon(i).with_accessible_name(n)` (+ `.tooltip(n)`, `.ghost()`, `.with_size(s, s)`) | `Button::icon_only(id, i, n)` (square, named, tooltip, Plain) |
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

### Segmented controls and tabs

| Old | New |
|---|---|
| `ToggleGroup::with_flexible_width()` | `.fill()`; without it the strip is as wide as its widest label needs |
| `ToggleGroup::underline()`, `SegmentedStyle::Underline` | `.with_style(SegmentedStyle::Tabs)` / `.style(SegmentedStyle::Tabs)` |
| `SegmentedStyle::Solid` | delete (`SegmentedStyle::Segmented` is the default) |
| `SegmentedStyle::Pill`, `SegmentedFace::pill()` | `SegmentedStyle::Quiet` (a refinement under a primary choice) |
| `SegmentedFace::radius(r)`, `SegmentedFace::left_labels(pad)` | delete |
| a segment label that does not fit ("Wednesday" in a narrow week strip) | `Segment::new("Wednesday").short("Wed")` |
| segment rects sized by hand | `SegmentedFace::height(size, theme)` × `SegmentedFace::intrinsic_width(labels, size, text, theme)`, split equally |
| accent-filled selected segment, a selected segment painted by the app | `ToggleGroup` / `SegmentedFace`: the chosen segment is a lift thumb |
| hand-drawn tab row with an accent underline | `ToggleGroup::new(..).with_style(SegmentedStyle::Tabs)` (2 px primary-ink underline) |
| `TriStateControl` | `ToggleGroup` with three segments |

### Switches, sliders, checkboxes

| Old | New |
|---|---|
| `ToggleFace::on_color(c)` | delete; on is always the inverted fill |
| `toggle_face_colors(..)` / `face.colors(theme)` returning `(track, knob, border, width)` | returns `ToggleColors { track, knob, knob_edge, edge, edge_width }` |
| `CheckboxFace::transparent_unchecked()` | delete; an unchecked box always has its raised face and edge |
| a non-square checkbox rect | nothing: the face paints the largest square in it |
| a slider squeezed to a knob in a row | nothing: a `Slider` keeps six knob widths of track |

### Menus and dropdowns

| Old | New |
|---|---|
| `MenuItem::checked_action(label, checked, f)` | `MenuItem::action(label, f).with_checked(checked)` |
| a "✓ " prefixed menu label | `.with_checked(true)` |
| leading, trailing or doubled `MenuItem::separator()` | leave them; the menu shows a separator only between two rows. Delete any code that trims them |
| a row highlighted by hand while its context menu is open | `menu.anchor_row(row_rect)` before `open_*` |
| a hand-drawn caption above a dropdown ("Level" over a filter) | `Dropdown::new(..).caption("Level")` and delete the caption text |
| a hand-drawn `▾` / chevron path on a dropdown-like field | `Dropdown` |
| `Dropdown` at a fixed 40 px | nothing: it is a `Regular` control; `.control_size(ControlSize::Small)` in a toolbar |
| `NumberInput::with_size(w, h)` | delete; `.control_size(size)`. The width is measured to hold every value in range |
| a hand-drawn `− value +` stepper (zoom, page, point size, copies) | `NumberInput::new(v).with_range(..).with_step(..).with_suffix("%").compact()` |

### Chips and badges

One family, styled by `ChipStyle`: `Neutral` (quiet, raised; a selected chip
is a lift), `Tinted(Tone)` (a status or a person, a faint wash with neutral
ink) and `Live` (the one filled pill: live, recording, an unread count).
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
| `chrome::paint_bar(scene, t, theme, rect)` | `ui::widget::toolbar::paint_band(scene, t, theme, rect)` (no fill, a bottom hairline) |
| `chrome::bar_padding(theme)` | `ui::widget::toolbar::band_padding(theme)` |
| a toolbar that wraps or clips commands on a narrow window | `Toolbar`: commands that do not fit go to its More menu |

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
| `Dropdown::{caption, control_size}` | Stacked caption; size |
| `NumberInput::{compact, control_size}` | Inline stepper; size |
| `ChipStyle::{Neutral, Tinted(Tone), Live}`, `ChipFace`, `BadgeFace`, `ChipColors`, `Chip::{style, with_lead}`, `Badge::style` | Chips and badges |
| `Toolbar::{push_icon, push_toggle, set_active, lay_out_in}`, `toolbar::{paint_band, band_padding}` | Toolbars |
| `Metrics::{menu_row_height, scrollbar_gutter}` | Menu rows; scroll bar gutter |
| `ColorSwatch`, `ColorSwatchPicker::new(swatches, columns)`, `ColorSwatchFace::{of, focused}` | Swatches |
| `InputHint::keycap_height(theme)` | Centring a row of keycaps |
| `Colors::inverted_pressed()` | A pressed inverted control: the hover step taken twice, so press reads past hover |
| `Colors::raised_on(ground)` | `surfaces.raised` as it stands on `ground`; `Theme::on_ground` applies it, so controls in a panel never sink into it |
| `widget::floating_surface::{menu_inset, paint_armed_row}` | A painted menu's row inset and its inverted, concentric armed row |
| `Widget::set_invalid(bool)` | A form's verdict on a control; `Field::with_error` sets it. `TextInput` edges itself in danger ink and reports `invalid` to assistive technology |

Visible changes that need no code: a popover menu's rows are menu rows
(`metrics.menu_row_height`, 32) with the inverted armed row and a small
check; the dropdown list insets its rows like every menu and checks the
applied value instead of lifting it; text fields lift under the pointer;
tooltips are the floating material; a toast shows its severity as a leading
icon instead of a stripe; the tree's disclosure, the breadcrumb separator,
the date picker's month steps and the time picker's arrows are chevrons from
`ui::icons` instead of typed glyphs. A screenshot test pinned to the old
pixels is regenerated, not worked around.

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
  on the whole group. No subtitle: if the person must know something to act,
  put one sentence in the content.
- **ListRow leading column.** `.avatar(name, identity)` for people and
  accounts, `.icon(icon)` for kinds of things, `.badge(code)` for a short
  classifying code (`GET`, `PDF`, `WARN`). With faces, compute
  `badge_column_width(text, theme, &all_badges)` once per list and pass it to
  every row's `.leading_column(..)` so titles line up.
- **ListRow text.** Title in `Body` (`.emphasized(true)` for unread),
  `.secondary(text)` for the one secondary fact (`.two_line_secondary()` for a
  message preview), `.meta(text)` for a time, size or count. More than one
  secondary fact: a `PropertyList` in the detail view, or columns in a
  `DataTable`. Never join facts with `·`.
- **ListRow trailing.** `.active()`, `.warning()`, `.error()`, `.idle()` for a
  status dot; `.presence(identity)` for a person's presence; `.disclosure()`
  when activating opens a page; `.action(icon, label, f)` for actions shown on
  hover and focus (they replace the meta); `.swipe_action(icon, label,
  destructive, f)` for touch.
- **Lists.** Put `ListRow`s in a `Section`: consecutive rows become one list,
  edge to edge, with inset separators that hide beside the selected row.
  With faces, call `.separator(next_selected)` on every row but the last.
  Selection is `.selected(true)`; never paint a row fill.
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
   column measurement around a `DataTable`.
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
   as prefix (`folder:inbox/msg:12`).
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
