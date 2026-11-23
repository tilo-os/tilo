# A theme is one resource, and its materials run on the GPU

A person asks their agent for a liquid-glass desktop, and the agent writes one: a glass
that bends what lies behind it, catches the light physically and follows the pointer,
fast on the GPU, plus the type, icons, colours, curvature and background that go with it.
A person without an agent changes the same theme in Settings, or from the shell's style
panel without leaving the shell. Both write the same fields of the same resource through
the same actions, with history and undo, and every window shows the change live.

This note says which of today's theming pieces survive, which go, and where each
responsibility ends up. `docs/architecture/theming.md` stays the reference for the
roles themselves.

## The resource

A theme is a typed record in the person's appearance resource, `settings/appearance`:

- `themes` is a collection of themes keyed by id. Each theme is the `ThemeDocument`
  roles as typed fields: palette, emphasis, presence, shape (curvature, borders),
  density, type, motion, icons, materials, depth and response. Each field has its own
  address (`settings/appearance/themes/<id>/palette/hue`), so a grant, a search result or
  an agent can name it.
- `dark_theme` and `light_theme` name the theme each mode shows: a preset id or a theme
  in `themes`.
- The person's own choices that sit over whichever theme is shown (density, interface
  face, accent, translucency, text and control size, the accessibility comforts) stay
  fields of the same record, as they are today.

Changing a theme is a typed action or a field edit on the one dispatch: `new_theme`,
`duplicate_theme`, `import_theme`, `delete_theme`, `switch_polarity`, and binding a
control to a field. The record's check is the theme's admission: a value outside a
role's admitted band, a material shader that does not validate, or a palette under a
contrast floor is refused with the reason, so an agent reads the same refusal a person
sees. `Theme` (ui-core) is what the record resolves to and is drawn with; it holds no
state of its own.

Sharing a theme is exporting its document and importing it elsewhere, which round-trips
every role.

## Materials with a GPU stage

A material (`material.window`, `.floating`, `.bar`, `.control`) is a painter set and its
parameters, as today. The `shader` set's WGSL becomes a real material stage:

- The theme writes one function, `fs_surface(s: Surface) -> vec4<f32>`. `Surface` holds
  the fragment's position across the part, the part's size, its signed distance to the
  outline and the normal of the slab the outline describes, the pointer (or none), the
  theme's light (colour and direction), the roles' colours (fill, ink, ground, raised,
  accent) and the finish's four parameters. `backdrop(offset)` returns what lies behind
  the surface, blurred by the finish's blur, displaced by `offset` logical pixels.
- It is validated with naga when the theme is read: one function, no globals, no
  bindings, no entry points, no loops, at most eight `backdrop` lookups. With no loops
  and a bounded number of lookups, its cost per pixel is bounded. It has no I/O: it
  reads only what the toolkit hands it.
- It runs as the last pass of the backdrop pass (`backdrop.rs`), the wgpu pass that
  already blurs and bends what lies below every floating surface, bar and window of the
  shell and of apps. Its output is the surface's body.
- The CPU raster path evaluates the same function with the WGSL interpreter in
  `part_paint::shader`. There `backdrop(offset)` reads the part's ground, so the surface
  stays legible without the GPU.

The `glass` and `liquid` sets stay as the built-in, parameter-only versions of the same
look. The liquid-glass example theme (`crates/ui/themes/liquid-glass/theme.json`, a theme bundle with no module of its own)
shows what a theme's own stage does: a rounded slab whose rim refracts by its normal,
splits colour slightly at the edge, reflects the light by Fresnel, and lights a
highlight where the pointer is.

## What survives, what goes

| Piece | Fate |
|---|---|
| `ThemeDocument`, `ui-core` `theme_document.rs` | Survives as the typed record of one theme. |
| `struct Theme`, `theme.rs`; `tokens/` | Survive: the resolution of a theme into what is drawn. |
| `floating_surface.rs` `enum Material` | Survives: it names which of the theme's materials a surface takes. It defines no material. |
| `backdrop.rs` | Survives, and runs a material's GPU stage. |
| `tokens/shader.rs`, `part_paint/shader.rs` | Survive, widened to the material stage; the interpreter is the CPU fallback. |
| `theme-sdk`, `theme-painter-wasm`, `themes/bauhaus`, `engines/loadable/theme-painter` | Survive as the one way a theme's own part painter loads: the shapes, hit areas and stepped transitions a material stage cannot express. A painter's shader brush uses the material stage's contract. |
| `tilo-appearance` | Survives as a platform reading of the host OS's own preference, used only when the person's record names no mode. It stores nothing. |
| `settings/appearance` `custom_themes` (JSON copies of theme documents) | Goes: replaced by the typed `themes` collection. |
| `apps/themes` (Theme Studio, its `themes.studio` resource) | Goes. A second resource holding the same themes, copied into the appearance record on apply. Its editor becomes Settings' Themes page, a tilolang view over `settings/appearance`. |
| The shell's style settings | Read and write the theme the shell shows through the same fields. Layout choices (dock edge, which material a bar takes) stay in the layout's own document: they choose among the theme's materials and define none. |
| The compositor's frames and window effects | Stay in the compositor; they take the window radius and depth from the theme the session sends. Moving the compositor's composition onto wgpu is not part of this. |

## Order

1. The material stage in the backdrop pass, the CPU fallback on the same contract, and
   the liquid-glass example theme, shown headless in its dark and light sides.
2. Themes as typed records of `settings/appearance`, Settings' Themes page and the
   shell's theme section over them, and `apps/themes` deleted in the same push.
