# UI migration guide

Mechanical old → new mappings for moving app code onto the unified design
system (`docs/design-language.md`). Each section is self-contained: apply it
top to bottom, then build with `rcargo clippy -p <app> --all-targets`.

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
| `ui::surface::panel(scene, transform, rect, radius, theme)` | The whole Panel row: `Material::Floating` body plus the `panel_border` edge. Unchanged signature; its body is now the material. |
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
- Any app-side blur radius, `glass_blur_px` read, or "fake blur" (a blurred
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
