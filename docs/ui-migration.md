# Migrating to the unified design system

Each section below is one framework change and the exact edits an app makes
to follow it. Apply them mechanically; where a rule asks you to choose, it
says how. `docs/design-language.md` is the reason behind every rule here.

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
| `Theme::with_accent(rgb)` | `Theme::with_your_colour(rgb)` (the person's own colour: focus ring, caret, selection) |
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
| `PaletteSpec`, `Colors::themed`, `Colors::set_accent`, `Colors::set_overlays`, `contrast::repair` | delete; build colours with `Colors::of_material(material, hue)` |
| `Colors::tilo_blue()`, `Theme::{tilo_blue, dracula, nord, gruvbox, gruvbox_light, catppuccin_mocha, catppuccin_latte, solarized, oled, warm_light, rose_pine}` | `Theme::dark()` / `Theme::light()` (`Theme::ALL` is now `Dark`, `Light`, `High Contrast`); a different temperature is `theme.with_neutral_hue(h)` |
| `CustomThemeSpec` colour fields (`accent`, `accent_hover`, `surface_base`, `surface_raised`, `text_primary`, `text_dim`, `border`, `success`, `warning`, `danger`, `info`), `radius_px`, `material_tint`, `contrast_findings()` | delete; a custom theme is `dark_base` + `neutral_hue` + spacing/font/scales, schema 4 |
| `ContentSurface::accent` | delete |
| `QueryInk::accented(text, placeholder, accent)` | `QueryInk::of(&theme.tokens.colors)` (caret and selection in your colour) |
| `palette.surface_radius_px()` (`CommandPalette`) | `CommandPalette::surface_radius_px(theme)` (the panel radius) |
| `ui::presence::PEER_PALETTE` (24 entries), `user_color` returning Tailwind colours | 12 entries, the dark presence palette; `user_color` is an identity, paint it with `colors.presence.of_rgb(user_color(id))` |
| `Theme::configured()` reading the person's colour | the shell applies it (`with_your_colour`); an app does nothing |
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

- `Colors::of_material(Material::{Dark, Light, HighContrast}, hue)`,
  `Colors::{dark, light, high_contrast}()`, `Tokens::of_material(material)`.
- `surfaces.{base, content, sidebar, inspector, raised, overlay, scrim,
  material, material_blur_px, content_edge}`. Paint the window ground with
  `surfaces.content`, a navigation column with `surfaces.sidebar`, a detail
  column with `surfaces.inspector`, fields and wells with `surfaces.raised`.
- `colors.intent.{inverted, inverted_hover, on_inverted, selection,
  selection_hover, you, selection_tint}` and `colors.lift_on(ground, t)`.
- `colors.presence` (`PresencePalette`): `get(slot)`, `series(index)`,
  `of(color)`, `of_rgb((r, g, b))`; `PRESENCE_LEN` = 12.
- `Theme::with_neutral_hue(h)`, `Theme::with_your_colour(rgb)`,
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
  `surfaces.inspector`. `surfaces.base` and `theme.bg_app` are the *desktop*
  ground: in an app, replace every `surfaces.base` / `bg_app` used as the
  window or pane background with `surfaces.content`, and every
  `surfaces.base` used as "the ground this widget sits on" (a contrast
  measurement, a hairline) with the surface it actually sits on.

### What to delete in an app

- Any theme picker, accent picker, "app accent" switch or workspace-tint
  control (Settings, Themes).
- Hard-coded per-app palettes (`const ACCENT_*`, Tailwind literals) that
  stood in for the accent ramp.
- Shadows under window content, cards and selected controls.
