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
| `TextStyle::numeric()` / `Text::numeric()` | Tabular lining figures for counters, clocks, sizes and columns. |
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

### The UI face

The system UI face is Instrument Sans; Noto Sans is its fallback for Greek,
Cyrillic and Vietnamese. Code never names a family: `TextRole` picks sans,
serif (`Reading`) or mono (`Code`), and `FontChoice::{SansSerif, Serif,
Monospace}` remains for the rare explicit choice.

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
| `data_table::paint_selected_row(..)` (crate-private) | `ui::widget::paint_row_fill(scene, transform, theme, rect, selected, hovered)` |
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
| A `DataTable` column painted with `with_text_render` that only right-aligns a number | `.numeric().with_text(..)` |
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

## Window chrome & shots

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
