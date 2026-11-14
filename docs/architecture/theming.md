# Theming: how every part looks, moves and responds

Tilo applications do not own their pixels. An app (and the shell) declares its
interface through `ui` components; the toolkit and compositor draw it. A theme
decides how those components look, move and respond, for every app and the
shell at once. It can be as small as an accent colour or as large as a
playful, dithered, pixel-edged desktop with Bauhaus-shaped buttons that squish
when pressed.

The rules:

1. **Views say what is there; the theme says how it looks and behaves.**
   Structure (which components, where, the shell's arrangement) belongs to
   views, chosen by the person separately from the theme. A theme reaches
   every component *part* a component declares, never an app's view tree,
   and there is no selector language over app structure.
2. **Two levels.** Most people stay at the **token level**: data
   (`ThemeDocument`, `THEME_SCHEMA_VERSION` in
   `crates/sdk/ui-core/src/theme_document.rs`): roles, density, motion
   presets, built-in painter sets (Default, Glass, Block). The **full level**
   adds programs: part painters, state transitions, scroll feel and assets
   (§ "Part painters").
3. **Safety comes from the sandbox, not from being data.** A theme's
   programs run as wasm in the UI host with no I/O, no secrets and a measured
   frame budget; they draw parts and nothing else. The danger KDE's 2024
   store theme showed was authority (a script that deleted files), which a
   painter never holds.
4. **A theme that passes validation cannot break an app.** Legibility is
   checked on rendered output, for every material a painter draws, so "this
   theme broke my app" is not a bug class an app developer receives.

Live surfaces (an interactive background, a lock-screen animation) are a third
kind, beside views and themes: a program of its own the person chooses. A
theme may ship one in the same bundle; it is not part of the theme.

It builds on the HIG (`crates/sdk/ui-core/HIG.md`, §7 and §9).

## What other platforms teach

- **The web** has no semantics to theme: everything is a `div` with author
  styles, so a user has no lever except overriding everything. The one durable
  user win was Windows High Contrast, which became CSS `forced-colors`: a
  small set of system colours the browser forces onto every page.
- **GNOME**: in 2019 app developers published *Please don't theme our apps*
  (#dontthememyapp), asking distributions to stop restyling GTK widgets, and
  libadwaita followed. The real complaint was not that users wanted colour; it
  was that themes targeted implementation details (GTK widget CSS), broke apps
  in ways the theme author never saw, and the bug reports landed on app
  developers. GNOME 47 later brought accent colours back — a single role,
  safely themable.
- **KDE** is the most themable desktop: global themes from the KDE Store
  restyle the whole session, and Union is an in-progress effort toward one
  style engine across toolkits. In 2024 a global theme from the store ran a
  script that deleted a user's files. A theme that can execute is a package
  with authority, and should be treated as one; ours cannot execute.
- **Windows** locks `.msstyles` behind a signature, so enthusiasts patch
  `uxtheme.dll`; meanwhile Win32, WinForms, WPF, UWP and WinUI each theme
  differently, so no theme reaches everything.
- **macOS** is coherent because apps use stock controls and semantic system
  colours (`labelColor`, `controlAccentColor`), and gives users little control
  beyond appearance, accent and contrast.
- **Android** went from OEM reskins through runtime resource overlays (which
  broke apps the same way GTK themes did) to Material You dynamic colour: a
  token contract apps consume, filled from the user's wallpaper.

Theming works when it targets semantic roles and fails when it targets
implementation. "Light, dark and an accent colour" is what remains when a
platform cannot guarantee that anything richer will not break apps. Tilo can
offer more because it controls the drawing: every pixel of chrome comes from a
role, and every role is validated.

## The contract

### Roles

A role is a named promise. `surfaces.raised` is not "a grey"; it is "the
surface one step above the window ground, which primary and secondary text
must read on". The roles are the ones `docs/design-language.md` describes,
and a theme sets them through one document:

| Theme role | What it sets | Tokens it produces | Where |
|---|---|---|---|
| `palette` | Neutral hue and chroma (≤ 0.010), the lightness of `base`, `content`, `sidebar`, `inspector`, `raised`, `overlay`, three ink tiers, four muted status tones, light or dark, high contrast | `surfaces.*`, `text_primary` / `text_dim` / `text_tertiary`, `semantic.*`, `border_standard`, the neutral ramp, state veneers, `content_edge` | `tokens/palette.rs` (`Palette`), `tokens/colors.rs` (`Colors::as_authored`) |
| `emphasis` | `inverted` (default) or `tinted { hue }` | `intent.inverted`, `inverted_hover`, `on_inverted` (primary actions, on switches, filled tracks), `intent.selection`, `selection_hover`, `Colors::lift_on` (the selection lift) | `tokens/palette.rs` (`Emphasis`) |
| `presence` | Lightness and chroma of the twelve presence hues (hues stay 30° apart) | `presence` (`PresencePalette`), and through it `intent.you`, `selection_tint`, `border_focus` | `tokens/colors.rs` |
| `shape` | `window_radius`; `border`: the outline every surface carries, its `width` and `ink` (`hairline`, `standard` or `strong`) | `radii.{tile, panel, control, item, detail}` derived concentrically (`Radii::from_window_radius`), the surface outline | `tokens/radii.rs`, `tokens/finish.rs` (`Border`) |
| `density` | `spacing`, a multiple of the 4 px scale | `spacing.s0`–`s16`; `Metrics` (pointer targets, control heights) stay put | `tokens/spacing.rs` |
| `type` | `face` (a built-in face by name, `Default` or `Hyperlegible`, or any family name), `reading` (a reading face by name, `New Computer Modern`, `Libertinus`, `Literata`, `EB Garamond` or `Source Serif`, or any family name), `base_size`, `distinct_letters` (optional; absent follows the face: on with Hyperlegible), `contrast` (the spread of role sizes, 0.5–1.5, stock 1), `weights` of the `body`, `label` and `heading` tiers | `typography.sans_family`, `typography.serif_family` (the `Reading` role's face, set in the optical cut its size calls for), the body size every `TextRole` derives from (`Theme::text(role)`), `typography.distinct_letters` (every look-alike set apart in all text, `docs/typography.md`) | `tokens/typography.rs` (`UiFace`, `ReadingFace`, `Typography::set_face`, `Typography::set_reading_face`), `theme_document.rs` (`ThemeType::distinguishes_letters`) |
| `motion` | `character`: 0 calm, 1 stock, 2 lively; `curve` (`spring`, `eased` or `none`); `duration_scale` | the shared UI springs, `motion_character` | `tokens/motion.rs`, `theme_document.rs` (`ThemeMotion`) |
| `material` | For each surface role (`window`, `control`, `floating`, `bar`) the painter set that draws it and its parameters: `block` (flat, square, borders only), `default` (opaque, or translucent given `blur`, `tint`, `saturation`), `glass` (translucent plus `refraction` at the rim, `edge_highlight`, `specular`), `pixelated` (what lies below shown as square cells of `cell` logical pixels, softened first by `blur`, under the ground at `cover`, with the cell grid itself drawn over the body as a dither of the fill towards its ink at `grid`), `liquid` (a lens: within `band` of the rounded rim what lies below is bent inward by up to `refraction`, red and blue set apart by `dispersion`, the flat middle straight and softened only by `blur`, under the ground at `tint`, with a thin specular rim of `edge_highlight` lit from `light_angle` degrees) or `shader` (the theme's own surface shader, [below](#a-surface-shader)); `selector`: what the window group's tab strip is made of (`floating` or `opaque`), the active selector's lit rim (`rim_span`, `rim_alpha`) and the hover film's opacity (`hover_alpha`, admitted only where a hover clears the selection step) | `RoleFinishes`, `surfaces.material`, `tokens.selector` | `tokens/finish.rs` (`Finish`: a painter set and its parameters; `RoleFinishes`), `part_paint/builtin.rs`, `tokens/selector.rs`, `backdrop.rs`, `chrome/selector.rs` |
| `depth` | `shadow { strength }`, `outline` or `none`: how a raised surface stands off what is below it | the elevation shadows | `tokens/finish.rs` (`Depth`) |
| `icons` | `stroke`: how wide an icon's outline is drawn, 1–2.5 logical pixels (1.5 at stock). Sizes are the metrics' (`Metrics::icon_sm`/`_md`/`_lg`), not a theme role: a layout is built against them | `tokens.icon_stroke_px`, read by `widget::IconWidget` | `tokens/metrics.rs`, `widget/icon.rs`, `theme_document.rs` (`ThemeIcons`) || `response` | How the shared components answer the pointer: for `hover` and `press`, how far the fill `lift`s toward the next surface step (0–2), how much of the accent it carries (`tint`, 0–0.25), the control's `scale` about its centre (0.7–1.15) and its lit inner edge (`glow`, 0–0.5). Optional: absent is the stock response (hover half a step; press a step, 0.97, a 0.2 edge), or with `motion.curve: none` the still one (the same lifts, nothing scaling, no edge) | `tokens.response`, read by `selection_tier`, `selection_tier_on`, `selector_tier` | `tokens/response.rs` (`Response`) |

Not theme roles, by design: there is no accent and no app colour (an app
has no brand colour in this language), no per-widget colour, no way to
remove the selection lift or the focus ring, and no icon **size** (its
weight is the `icons` role). A person's own colour is identity, not theme
([Your colour](#your-colour)). Derived palettes — the terminal ANSI
palette, the syntax palette, chart series (`PresencePalette::series`) —
follow the colour tokens.

Everything is derived in one place (`Colors::as_authored`, then
`Theme::from_document`): every surface and ink tier is an OKLCH lightness
at the palette's hue and chroma, so a hue never moves contrast.

### Data, versioned

The document is `ThemeDocument`
(`crates/sdk/ui-core/src/theme_document.rs`, `THEME_SCHEMA_VERSION` = 8):

```json
{
  "schema_version": 8,
  "id": "ember",
  "name": "Ember",
  "palette": {
    "dark": true, "high_contrast": false, "hue": 60.0, "chroma": 0.006,
    "surfaces": { "base": 0.16, "content": 0.215, "sidebar": 0.195,
                  "inspector": 0.235, "raised": 0.265, "overlay": 0.285 },
    "ink": { "primary": 0.96, "secondary": 0.76, "tertiary": 0.62 },
    "status": { "danger": { "lightness": 0.70, "chroma": 0.13 }, "warning": { … },
                "success": { … }, "info": { … } }
  },
  "emphasis": { "kind": "tinted", "hue": 40.0 },
  "presence": { "lightness": 0.74, "chroma": 0.14 },
  "shape": { "window_radius": 14.0, "border": { "width": 1.0, "ink": "hairline" } },
  "density": { "spacing": 1.0 },
  "type": { "face": "Noto Sans", "reading": "Literata", "base_size": 14.0,
            "contrast": 1.0, "weights": { "body": "regular", "label": "medium", "heading": "semibold" } },
  "motion": { "character": 1.0, "curve": "spring", "duration_scale": 1.0 },
  "material": {
    "window": { "painter": "default" },
    "control": { "painter": "default" },
    "floating": { "painter": "default", "blur": 24.0, "tint": 0.72, "saturation": 1.0 },
    "bar": { "painter": "glass", "blur": 24.0, "tint": 0.25, "saturation": 1.7,
             "refraction": 10.0, "edge_highlight": 0.35, "specular": 0.12 },
    "selector": { "strip": "floating", "rim_span": 0.55, "rim_alpha": 0.22, "hover_alpha": 0.75 }
  },
  "depth": { "kind": "shadow", "strength": 1.0 }
}
```

`palette.hue` may be absent: the palette then has no hue of its own and is
pure grey until the workspace it is shown in gives it one.

Each `material` role is a **finish**: `painter` names a painter set and the
other keys are that set's parameters. A set declares its parameters
(`ParamSpec`): each has a name, an optional default, an **admitted** band
that a document may state it in, and a **usable** band, no wider, that the
theme is built at. A document that leaves out a parameter with no default,
states one the set does not declare, or states a value outside the admitted
band is refused. A value outside the usable band is clamped when the theme
is built. `default`, `glass`, `block`, `pixelated`, `liquid` and `shader` are the toolkit's built-in sets
(`part_paint::builtin`), and those names are reserved. A theme's own set
registers under any other name (`register_painter_set`), and a set named by
a document but not installed is refused like an unknown field.

#### A surface shader

The `shader` set paints a role with the theme's own colour arithmetic: a
WGSL function `fs_surface` that takes exactly the documented inputs, in
order, and returns `vec4<f32>` (`tokens/shader.rs`, `INPUTS`): `backdrop`,
`uv` (0–1 across the part), `size` and `position` (logical pixels),
`fill`, `ink`, `ground`, `raised`, `accent`, `light` (linear sRGB),
`params` (the finish's `tint`, `saturation`, `scale`, `phase`) and `time`
(the static seconds the finish states, never a clock). The finish carries
the source as `shader` and the value as `time`:

```json
"window": { "painter": "shader", "shader": "fn fs_surface(...) -> vec4<f32> { ... }" }
```

It declares no entry point, binding or global; the toolkit owns every
input, so a shader reads nothing but what it is handed. The source is
checked when the document is read (`Source::validate`: at most 16 KiB,
parses with the WGSL front end wgpu uses, the signature above). A document
whose shader fails is refused with the compiler's message and position
(`Theme::refusal`, "material.window: surface shader does not parse: …
(line 3, column 9)"); Theme Studio's import shows it, and a saved theme
that fails is logged and does not apply. A shader is evaluated once per
settled part into an image the part cache replays, so an idle surface
draws nothing; `backdrop` is the part's ground today (no backdrop pass
feeds it yet).

Built-in presets, distribution defaults, the user's themes and every file
Theme Studio exports are this document. Unknown fields are refused. A new
schema version only adds fields with stock values, so a document of an
older version this build lists as read (schema 7 and 8 today) is admitted,
read with stock values for what it does not say, and written back at the
current version when it is next saved. A document of any other version is
not admitted: the layer it sits on contributes nothing and the layers below
it apply.

The shipped presets (`Preset`, `Theme::ALL`):

| Preset | Hue / chroma | Ladder |
|---|---|---|
| Dark (default) | – / 0 (pure neutral) | the design language's dark ladder |
| Dark cool | 255 / 0.008 | dark ladder |
| Dark graphite | 60 / 0.006 | dark ladder |
| Dark soft | 270 / 0.003 | base 0.19, content 0.25, sidebar 0.228, inspector 0.27, raised 0.30, overlay 0.32; tertiary ink 0.65 |
| Light | – / 0.010 (neutral until a workspace gives it a hue) | the light ladder |
| High contrast | – / 0 | black surfaces, ink 1.0 / 0.94 / 0.88, AAA |
| High contrast light | – / 0 | white surfaces, ink 0 / 0.30 / 0.40, AAA |

Glass, Glass light, Block, Block light, Pixelated, Liquid Glass and Liquid
Glass light are plain
documents in `crates/sdk/ui-core/themes/`.

### Validation and legibility

`ThemeDocument::validate` checks structure: the schema, id/name/face, and
every number finite and inside the band its role admits (chroma ≤ 0.010,
ink on the ink side of every surface, status chroma ≤ 0.13, presence
lightness 0.35–0.90 and chroma 0.04–0.20, window radius 0–40, density
0.5–2, base size 10–24, motion 0–2, and every material parameter inside the
admitted band its painter set declares; the built-in sets admit blur 0–64,
tint 0–1, saturation 0–2, refraction 0–32, band 0–64, dispersion 0–0.5,
light angle 0–360, edge highlight and specular 0–0.5, cell 1–64, cover 0–1
and dither (`grid`) 0–0.5). A value admitted but
outside the usable band (radius above 28, density outside 0.75–1.5, base
size outside 12–18, blur above 48, band above 48) is clamped when the theme
is built.

**What shows through.** A translucent surface (any finish that samples the
backdrop: Glass, Pixelated, Liquid) shows what lies below compressed into
the material's band (`material_band`): on the side of the ground its ink
stands on, as far as the theme's inks can afford (`material_spread`, at
most 1.5:1); on the other side, to the end of the neutral ramp. The
backdrop pass maps each sampled colour's relative luminance into that
range evenly in contrast and keeps its hue (`backdrop::Legible`): dimmed,
a colour's channels scale with its luminance; lifted, they keep how far
they stand off its grey, so a dark page under light glass lifts without a
cast. A white page under a dark theme's glass shows as a dimmed page with
its text still on it, and a photo in its own colours, never one flat
grey. The band's ends are the worst grounds the audit
measures ink on. Backdrops the pass does not sample (one the compositor
provides, the published desktop picture) and a painter's glaze are held in
the band channel by channel instead.

Legibility is measured on the materialized colours against the floors in
`tokens/contrast.rs` (`FLOORS`). They are WCAG 2.x ratios, and text floors
rise to AAA on a high-contrast palette:

| Foreground | Ground(s) | Floor | Why |
|---|---|---|---|
| `text_primary`, `text_dim` | every surface, `intent.selection`, `intent.selection_hover` | 4.5:1 | body text, also on a selected or hovered row |
| `text_tertiary` | every surface; `content` | 3:1; 4.5:1 | quietest tier: large-text AA everywhere, body text on the window ground |
| `intent.selection` | `content` | 1.15:1 | a selected row is a visible step with no hue doing the work |
| `intent.on_inverted` | `intent.inverted`, `intent.inverted_hover` | 4.5:1 | the label of a primary action, at rest and under the pointer |
| `intent.inverted` | `content` | 3:1 | an on switch, a filled track (WCAG 1.4.11) |
| `intent.you` | `content`, `sidebar`, `inspector`, `raised`, `overlay` | 3:1 | your focus ring (WCAG 2.4.11) |
| every presence colour | `content`, `raised`, `overlay` | 3:1 | a collaborator's caret, cursor, pill |
| `success`, `warning`, `danger`, `info` | `content`; `base` | 4.5:1; 3:1 | status ink on a control; status dots |

- `ThemeDocument::findings` audits a document as authored, for every
  presence hue as the wearer's colour. Theme Studio shows the findings live
  and refuses to apply or export a document that has any.
- `contrast::repair` runs on every theme the runtime builds
  (`Colors::of_palette`): a foreground that misses a floor is stepped in
  OKLCH lightness, at its own hue, until it holds; grounds never move, and
  the presence palette moves as one so every participant stays equally
  loud. A hand-edited file therefore still renders legibly.
- Tests sweep every preset at every temperature for every presence hue,
  and every preset with every tinted emphasis hue (5° steps), and require
  no findings at all (`every_preset_keeps_every_floor_at_every_temperature_for_everyone`,
  `every_tinted_emphasis_hue_keeps_every_floor_on_every_preset`).

## Part painters

Every `ui` component declares its **parts** (a `Button`: container, label,
icon, focus ring; a `Tab`: container, label, close, indicator; a scroll
container: track, thumb, edge) and their **states** (rest, hover, pressed,
focused, selected, disabled, dragging, changed-by-someone).

- **A painter** is Rust compiled to wasm against the theme SDK. It is a pure
  function `(part, size, state, progress, tokens) -> scene`: a vello scene of
  kurbo paths and peniko brushes (the renderer already draws with them), plus
  an optional WGSL fill shader for materials such as ordered dithering,
  pixelation, hatching or refractive glass.
- **The painted shape is the hit area.** A triangle button takes clicks inside
  the triangle only; focus rings and accessibility bounds follow the shape.
- **Motion** belongs to the UI host's clock. A theme declares how each state
  changes (spring, stepped, duration, curve, or none); the painter only draws
  a given moment. Reduce motion replaces every transition with its end state.
- **Scroll feel** is a part too: overscroll (rubber band or hard stop), snapping,
  momentum, parallax of nested layers, the edge effect and the indicator.
- **Assets**: fonts (a bitmap font draws unhinted at integer scale), an icon
  set covering every semantic icon name, cursors, system sounds.
- **Cost**: painted parts are cached by part, size and state; only animating
  parts repaint. Each painter's time is measured; one that exceeds the frame
  budget is reported to the theme's author and drawn by the built-in painter
  for that part until its author fixes it.
- **Window effects** (open, close, minimise, focus) and window frames use the
  same painter interface in the compositor (`WindowEffect`, `WindowFrame`).

**The SDK and the host.** `tilo-theme-sdk` (`crates/sdk/theme-sdk`) is the
interface: `PartPainter` (`paint`, `params`, `transition`), the scene a
painter returns (fills, strokes, layers; solid, gradient, pattern and
backdrop brushes) and the wire bytes a wasm painter exchanges. The host is
`part_paint` in `ui-core`. It resolves a part's tokens, draws the painted
scene, fills a backdrop brush from the backdrop pass, which compresses
what it samples into the material's band, caches settled parts and runs
transitions. A backdrop brush takes a glaze: the painter's own ops, drawn
over the sample; the host holds a glaze in the band after it, channel by
channel.

**Built-in sets.** Default, Glass and Block (`part_paint::builtin`)
implement the same `PartPainter` and draw through the same host as a
theme's own set. Each draws the four surface parts (window ground, floating
panel, bar body, control face) and leaves the rest to the toolkit.

A part's tokens carry the light a surface catches (`PartTokens::light`,
from `Colors::light`): daylight at no chroma, white under every palette and
temperature, so it is not a shade of what it lands on. Glass draws its
specular sheen and its lit rim in it; how strongly a body catches the light
stays the role's finish (`specular`, `edge_highlight`), so a theme that
wants a warmer or cooler light has a token to name rather than a literal
in a painter. The wire encoding puts it after the parameter list and reads
bytes that end before it as the stock white, so an older encoding still
reads; a field added to `PartTokens` goes on the end the same way.

`PartTokens::flush` names the sides of a part that stand on the display's
own edge (`Sides`, from the component: a square chrome body at the `full`
fit has three). A body there has no ground behind that side and no
neighbour to separate from, so a painter draws none of its edge, rim or
hairline on it: the body reaches the screen's edge in its own colour, where
otherwise a pixel of another colour shows between it and the bezel. Glass
leaves its lit rim off those sides; Block, Pixelated and Shader draw no
outline at all. It is encoded last, for the same reason as the light.

**Sets loaded from wasm.** A theme's set is a module built against the SDK
(`export_painter!`). `part_paint::wasm` loads it:

- The module may import nothing, so it has no clock, no I/O and no host to
  ask; a module that imports anything is refused.
- It runs on wasmi.
- Each call has a fuel budget (`FUEL_PER_CALL`) and the memory is capped
  (`MAX_MEMORY`).
- Its answers are decoded as untrusted bytes, bounded by `MAX_ANSWER`.
- A call that traps, runs out of fuel or answers malformed bytes leaves the
  part to the Default set, and the set is not called again.

The module's declared parameters are read once, at load.

**Theme bundles.** A theme with its own set ships as a bundle
(`theme_bundle`): a directory holding `theme.json` and one `<set>.wasm`
for each set the document names that is not built in. Loading it loads
and registers each module under its name, then reads the document like any
other. Set names are lowercase letters, digits, `-`, `_` and `.`. A
bundle that is missing a module, or carries one the host refuses, is
refused. The material bench (`ui-perf material_bench DIR`) and
`tilolang layout-check --theme DIR` take a bundle directory where they
take a theme's name.

**Writing a set.** A set is a crate that depends on `tilo-theme-sdk`
alone, built as a `cdylib` for `wasm32-unknown-unknown`, exporting its
painter with `export_painter!`. Its tests run the painter natively through
`guest::answer`, over the same wire bytes the host exchanges with the
module. Bauhaus (`crates/sdk/themes/bauhaus`) is the worked example:
- square surfaces and an accent rule down floating chrome;
- surfaces shading towards their ink in ordered-dither bands, where
  `levels`, `depth` and `grain` set the number of bands, how deep they
  go, and the texel size;
- buttons on a hard offset shadow (`shadow`) that sink onto it in a
  stepped transition when pressed.

It is built with
`cargo build -p tilo-theme-bauhaus --target wasm32-unknown-unknown --release`.
Its bundle is that module, renamed `bauhaus.wasm`, beside the crate's
`theme.json`.

## Live surfaces

A live surface is a program that draws a layer the shell places: the desk
background below the windows, the lock screen's backdrop. It uses the same kit
(Rust compiled to wasm, vello and wgpu) and gets read-only inputs: time, the
pointer, the windows' rectangles and motion, notifications, the audio level. It
does no work while nothing it reads moves, and pauses when covered or on
battery. It holds no authority and cannot read window content.

## Layers and precedence

The theme a person sees is resolved from layers, lowest to highest:

| # | Layer | Who writes it | Contents |
|---|---|---|---|
| 1 | Built-in defaults | the toolkit, in code | `Preset::Dark` |
| 2 | **Distribution defaults** (`appearance_device.distribution_defaults` / `appearance_device.distribution`) | whoever ships the OS image | a preset id or a whole document, and pins |
| 3 | User theme | the person, through Settings and Theme Studio, or an agent through the same actions | the theme for the current mode, the person's density, and their finish over it (`FinishLayer`: the interface face, the accent and how much of the theme's translucency they keep) |
| 4 | Window | the shell, per window | the neutral hue of the workspace the window is in, and the person's colour (`WindowAppearance`) |
| 5 | Accessibility | the person or assistive policy | high contrast / forced colours, text scale, UI scale, reduce motion, reduce transparency, thick caret, the Hyperlegible face |

**Accessibility always wins.** Nothing below layer 5 can lower contrast,
shrink text, re-enable motion or make chrome translucent once the person
asked otherwise, and a distribution cannot pin a value layer 5 touches.

**Distribution defaults** (`DistributionDefaults`, schema 2) name a preset
(`theme`) or carry a `document`, and may pin any of `palette`, `emphasis`,
`presence`, `shape`, `density`, `type`, `motion`, `material`
(`PINNABLE`). A pin beats the user's theme for that role only;
accessibility names in the pin list are ignored, and any other name, an
unknown preset or another schema rejects the layer.

**The user theme** is read from the settings document's `appearance`
section (`theme_layers::user_document`):

| Key | Value |
|---|---|
| `mode` | `"dark"`, `"light"` or `"automatic"` (light by day, dark by night, on the schedule night light shares) |
| `dark_theme` | the theme dark mode shows: a preset id (`"dark-soft"`) or `"custom:<id>"`; default `"dark"` |
| `light_theme` | the theme light mode shows; default `"light"` |
| `custom_themes` | the documents a `custom:<id>` names (Theme Studio writes them when it applies one) |
| `density` | the person's spacing over whichever theme is shown (Compact 0.85, Regular 1, Roomy 1.2) |
| `ui_face` | the face the person reads best, over the theme's (`Default`, `Hyperlegible`, any family name); absent or `null` follows the theme |
| `accent` | the accent they chose, by a name the presence palette gives a hue (`violet`, `teal`); absent or `null` follows the theme's emphasis |
| `translucency` | how much of the theme's translucency they keep: 1 as the theme sets it, 0 opaque floating chrome; absent or `null` follows the theme |

None of `ui_face`, `accent` or `translucency` is a new role: each names a
role the theme already has (`type`, `emphasis`, `material`), so a person who
cannot read the theme's face, wants their own accent colour or wants menus
opaque need not write a theme of their own. A distribution that pins `type`,
`emphasis` or `material` keeps its choice, a high-contrast palette has no
accent to give, and the accessibility layer is applied after all three (high
contrast and "make panels opaque" still win).

A section with none of `mode`, `dark_theme`, `light_theme` is no user
theme, so distribution defaults still show.

**What an appearance draws** is `theme::drawn`, and every surface asks it:
`dark` or `light` alone, Automatic's day/night schedule as both in turn, and
an appearance that names no mode as the host's own answer. A page answers its
browser's light or dark, which `Theme::configured_from` merges in before the
layers resolve, so a page is light on a light system from its first frame; a
computer running Tilo answers nothing and starts dark. The Theme Studio's "in
use" is that same reading, so a card cannot name a polarity the surfaces do
not draw.

**The workspace temperature** is a hue only: the person's override for that
workspace (`appearance.workspace_temperatures`, OKLCH degrees by workspace
id, `workspace_temperature_override`) or the dominant hue of its wallpaper
(`ui::workspace_hue`, `None` for a grey one). It never adds chroma, so the
default dark (chroma 0) shows no temperature and Light (0.010) shows the
most. Light has no hue of its own (`Palette::hue` is `None`): without a
temperature — a grey wallpaper, no shell, a capture — it is pure neutral.
The high-contrast palettes carry none.

**The window layer** (`ThemeLayerStack::window`, `ui::WindowAppearance`) is
set by the host that shows a window, never by a settings document: the
temperature of the workspace the window is in and the person's colour,
applied with `Theme::in_window`. The shell keeps every workspace's
temperature and the person's colour (`shell::window_appearance`), dresses
its own chrome in the active workspace's, and sends each app window the
appearance of the workspace holding it over the app's authenticated
shell-ui connection (`FloatingSurfaceHostMessage::Appearance`, on connect
and whenever a temperature, the window's workspace or the colour changes).
The app keeps it across theme rebuilds (`App::set_window_appearance`), so
a workspace switch, a new wallpaper or a new colour reaches focus ring,
caret, selection and unread marks live. Captures take it from
`--workspace-hue DEGREES` and `--your-colour NAME`.

**Accessibility** high contrast or forced colours swaps the palette for the
high-contrast palette of the theme's own polarity (`Theme::with_high_contrast`):
shape, density, type and motion stay, emphasis becomes inverted and the
material turns opaque. Reduce transparency keeps floating chrome opaque
(`backdrop::blurs`).

Distribution defaults, the user theme and accessibility live in the
settings document as versioned JSON; the workspace temperature is derived
on the device from the wallpaper unless overridden. The settings store does
not sign appearance records. Resolution is one deterministic function over
the stack (`theme::resolve_theme_layers`), and `Theme::configured` reads it.

### Your colour

A person's colour is part of their identity, not of a theme: one of the
twelve presence hues (`PresenceHue`: red, orange, amber, olive, green, teal,
cyan, sky, blue, violet, magenta, pink), stored as `identity.colour` in the
settings document and roaming with their other personal settings. Until
they choose, it is derived from their stable identity
(`PresenceHue::of_identity` of the person's id, never the session's
handle). It reaches windows through the window layer, not the theme:
`Theme::configured` does not read `identity.colour`; the shell sends it to
each app window, and a standalone app that may read the person's settings
starts from the stored choice (a private app with an ephemeral identity
does not). Collaborators see the same colour: a shell's presence frame says
its face's colour (`ShellPresence::colour`), a live frame carries
`LiveParticipant::color`, and a remote cursor the toolkit draws takes its hue
from the publisher's identity (`PresenceHue`); each receiving shell paints
that peer in it and announces it to its apps in the roster
(`Collaborator::colour`). It is their focus ring, text caret, text-selection tint and unread
mark (`intent.you`, `selection_tint`, `border_focus`), painted at the
current theme's presence lightness and chroma, so it reads on every theme.
The theme sets only that lightness and chroma. Settings' Appearance page
offers the twelve hues beside "from your identity", which writes none.

### What users get

Every one of these is a role and not an implementation detail: light, dark
or automatic; which preset or own theme each mode shows; the temperature
and chroma of the neutral material and its whole lightness ladder; inverted
or tinted emphasis; the presence tone; the window radius; density; the
interface face and text size; the character of motion; and how floating
chrome blurs. Colour itself belongs to people: each person's colour marks
their presence, and each workspace has a temperature.

A person and an agent author the same document. Theme Studio's controls
and its actions (`new_theme`, `set_hue`, `set_chroma`, `set_lightness`,
`set_tinted`, `set_tint_hue`, `set_presence_lightness`, … on the
`so.tilo.themes.studio` resource) write the same fields. When a look still
does not fit, the schema gains a role. It does not gain a selector.

## App developers

What an app may do:

- **Own its content surfaces.** A canvas, a document page, a video, a game or
  a map is content, and the app chooses how it looks. The reference pattern is
  `apps/ebook/src/reader_theme.rs`: all chrome (toolbars, drawers, buttons)
  reads strictly from `Theme`, while the page uses a palette the reader picks
  (light, sepia, dark, high contrast).

What an app may not do:

- Bring a colour of its own. There is no brand accent; an app's content
  colours (a calendar's event colours, a chart's series) come from the
  presence palette.
- Draw or restyle window chrome (title bars, tab pills, decorations).
- Restyle the internals of shared controls. A `Button` is the system's
  button; an app that needs something different needs a new role or a new
  widget in `ui`, not a local copy (HIG §8).
- Override accessibility. High contrast and forced colours reach into content
  surfaces too: a page palette must offer a forced-colours rendering and switch
  to it when the person asks for high contrast, the way the ebook's high
  contrast page already exists.

The trade for an app developer: dark mode, high contrast, text and UI scaling,
RTL and accessibility come for free, and there is no "a theme broke my app"
bug class, because any theme that passes validation renders every app
correctly.

## Theming is per viewer

Collaborators sync state, not pixels. A shared workspace is drawn natively on
each machine from the same CRDT state, so each person sees it in their own
theme, at their own text scale, with their own accessibility settings. A
collaborator's high-contrast preference never reaches anyone else's screen,
and a host's dark theme never reaches a guest.

A workspace carries a **temperature** on each viewer's device (layer 4):
it moves hue only, never contrast. Collaborator colours are identities: a
person's presence hue is painted through each viewer's own theme, at that
theme's presence lightness and chroma (one tone for everybody).

## Trusted UI

A themable OS gives an attacker a palette. Permission prompts, the identity
ceremony (`crates/session/onboarding/src/ceremony.rs`), the capability
indicators of `distribution.md` §5 and the lock screen must carry a marker
that **no theme and no app can reproduce**:

- It is drawn by the compositor, in a region no client surface can cover.
- Its appearance is not a theme role. Themes can recolour the prompt's body;
  they cannot reach the marker, so a theme cannot make app content look like
  it, nor make the real one look like app content.
- It is tied to the session (for example a personal secure image or phrase set
  at onboarding), so a pixel-perfect screenshot of someone else's prompt does
  not carry it.

## Current state

Verified against the code at the time of writing:

- **The document and its presets.** `ThemeDocument` schema 8 with the
  roles above; `Preset` ships Dark (default, chroma 0), Dark cool, Dark
  graphite, Dark soft, Light and both high-contrast palettes;
  `Theme::from_document` builds a theme, `Theme::named` finds a preset.
- **Contrast.** `FLOORS`, `audit`, `ThemeDocument::findings` and
  `contrast::repair` as above; the preset, temperature, presence-hue and
  tinted-emphasis sweeps pass with no findings.
- **Layer resolution.** `theme::resolve_theme_layers` resolves built-in
  defaults, distribution defaults (schema 2, pins on the new roles), the
  user theme (`mode`, `dark_theme`, `light_theme`, `custom_themes`,
  `density`), the workspace temperature, the person's colour and
  accessibility. `Theme::configured` reads the same function.
- **Forced colours.** `Theme::colors_are_forced` and `Theme::content_surface`
  answer "am I forced?" with the theme's page and ink tokens; apps still
  have to honour the flag.
- **Workspace temperature** is applied by the shell to its own chrome and
  sent to every app window for the workspace that window is in, over the
  shell-ui channel. Without one (a grey wallpaper, no shell, a capture) the
  light palette is pure neutral.
- **Your colour.** The shell resolves `identity.colour` or the
  identity-derived hue, paints its chrome and the person's own avatar in it,
  and sends it to every app window; collaborators receive it with the
  person's presence and paint them in it.
- **The material.** Each surface role names its painter set (`block`,
  `default`, `glass`, `pixelated`, `liquid`, `shader`) in `material`, and one path (`fill_role`,
  `control_face`) paints windows, menus, buttons, fields, dropdowns, toggle
  groups and the top bar from it. Glass adds a lens refraction, a rim
  light and a specular sheen; Pixelated runs the backdrop pass at one texel
  per cell and draws it unsmoothed; Liquid bends the backdrop along the
  rounded rim like a convex lens, leaves the middle sharp and lights a thin
  rim from the theme's light angle; Liquid Glass ships nearly clear (tint
  0.1 on floating chrome, no blur) and its window ground is glass too, the
  only finish whose windows sample. A lens (Glass's or Liquid's) is evaluated
  in the backdrop pass itself: its last blur pass takes its kernel about the
  displaced point (`backdrop::Refraction`), so a bent surface costs one
  displaced lookup and a settled one draws nothing new. The pass keeps
  each region's sample with what it was taken of (the scene below, the
  rect, the lens, the range) and shows it again, running nothing, while
  that is unchanged; a region whose sample another region shows through
  itself is taken anew with it.
  Backdrops the
  compositor provides and a strip over the published desktop are shown
  unbent today and held in the band channel by channel, not compressed. A
  window shows what lies below only where its scene is layered over it (a
  shell surface, a shot) or the compositor provides a backdrop; an app's
  own window over the desktop shows its fallback ground. On a dark theme
  the band's light end is bound by the light ink, so a white page shows as
  a mid-dark grey; Liquid Glass light is the bright glass, dark ink over
  what shows through lifted toward white. Glass, Glass light, Block, Block
  light, Pixelated, Liquid Glass and Liquid Glass light ship as documents in `crates/sdk/ui-core/themes/`. Legibility is checked
  against the worst backdrop each material can show (`audit_materials`), and
  `tilolang layout-check --theme NAME` lays a view out under a theme. Reduce
  transparency, forced colours and the high-contrast palettes keep floating
  chrome opaque. The selector finish
  (`material.selector`) is read by `chrome::paint_selector`; the same three
  make the tab strip solid and drop the rim.
- **Theme Studio** (`apps/themes`) edits the document role by role with
  live findings and applies or exports only legible themes. A studio
  theme keeps the document it was made from (`Theme::origin`), so every
  role it does not edit (each finish, border, depth, reading face, motion
  curve, response) round-trips; editing the floating blur or tint keeps its
  painter. Settings'
  Appearance page offers the theme, density, interface size, face, accent,
  translucency and colour; none of them needs a theme of one's own.
- **Hard-coded colours in apps.** The HIG §9.13 `theme` and `app-paint`
  ratchets count literal colours, `Color::WHITE`/`BLACK`, ramp slots and
  app-painted grounds per app, and the counts may only fall; see the
  appendix for the classification of the remaining sites.
- **Trusted-UI frame for permission prompts and portal sheets.** The shell
  paints a seal band on those surfaces and publishes it as
  `tilo.shell.trusted-frame/permission-prompt` or
  `tilo.shell.trusted-frame/portal-dialog`. The widget walk drops that
  namespace, so an app widget cannot mint the same target. The band is not a
  theme token; the active theme may recolour it. A session-tied personal
  image is still absent, and onboarding, the lock screen, and capability
  indicators do not carry the frame yet.

## Roadmap

0. **Part painters and live surfaces**: the theme SDK, the part and state
   declarations of every `ui` component, Glass and Block as built-in painter
   sets, and a full-level reference theme (dithered, pixel-edged, Bauhaus
   shapes, the brand palette) across every app and the shell at native refresh.

1. **Hard-coded chrome colours onto tokens**, one app per change, following
   the appendix and the ratchet shards under
   `crates/sdk/ui-core/tests/baselines/`.
2. **Settings > Appearance** exposes the appearance controls.
3. **A compositor-drawn trusted-UI marker** no theme or app can reproduce,
   and wallpaper / greeter / boot artwork on the distribution layer.

## Appendix: hard-coded colours in `apps/`

Classification of the 155 `from_rgb8` / `rgb8` / `Color::new([` sites, by
file, as counted before the token migration (the Themes row no longer
applies: Theme Studio holds no colours of its own). **Content** means the colour belongs to the document, the media or the
person: page and paper palettes, a stored or user-picked colour, overlays that
keep controls legible over live video, colour-math helpers that convert rather
than choose, and test fixtures. **Chrome** means the colour is a UI role the
theme should supply.

| File | Sites | Content | Chrome | Notes |
|---|---:|---:|---:|---|
| `ebook/src/reader_theme.rs` | 27 | 27 | 0 | page palettes (the reference pattern) |
| `ebook/src/app.rs` | 4 | 2 | 2 | scheme swatches are content; badge ink should be measured (`readable_on`) |
| `ebook/src/model.rs`, `input_controls.rs` | 3 | 3 | 0 | user-picked scheme and highlight colours |
| `word-processor/src/widget.rs` | 19 | 16 | 3 | paper, paper ink, table and placeholder tints and stored run colours are content; caret, active-band edge and spelling squiggle are affordances that should come from `intent.you` and `danger`, made legible on paper |
| `word-processor/src/review_overlay.rs` | 14 | 0 | 14 | comment and change cards, accept/reject buttons, insertion/deletion ink |
| `word-processor/src/widget/tests.rs`, `style_color_picker.rs` | 4 | 4 | 0 | fixtures; seed from a stored style |
| `feeds/src/reader_theme.rs` | 14 | 14 | 0 | page palettes |
| `pdf-viewer/src/render.rs` | 9 | 8 | 1 | page ink and thumbnails are content; the page-card outline is chrome (`ui::surface::edge_on`) |
| `pdf-viewer/src/app.rs` | 1 | 1 | 0 | shots harness clear colour |
| `tube/src/contrast.rs`, `util.rs` | 9 | 9 | 0 | colour-math helpers and their tests |
| `tube/src/widgets/thumbnail.rs` | 2 | 0 | 2 | LIVE badge and watch-progress bar |
| `tube/src/screens.rs` | 2 | 2 | 0 | segment-category identity colours |
| `terminal/src/main_body.rs`, `tests.rs` | 10 | 10 | 0 | ANSI/truecolor conversion; fixtures |
| `sheets/src/grid.rs` | 7 | 0 | 7 | formula reference colours; needs a shared distinct-hue role |
| `sheets/src/app.rs` | 1 | 1 | 0 | stored sheet-tab colour |
| `camera/src/app.rs` | 6 | 5 | 1 | white controls and dark washes over the viewfinder, document paper are content; the settings-sheet scrim is chrome (`surfaces.scrim`) |
| `video-editor/src/**` | 8 | 8 | 0 | title colours rendered into the video, stored swatches, blend helpers, test |
| `themes/src/**` | 5 | 4 | 1 | spec conversion and tests are content; the customizer's seed colour should be the active accent |
| `mail/src/draw.rs` | 2 | 0 | 2 | avatar palette entries outside the theme (`Colors::categorical`) |
| `chat/src/**` | 3 | 3 | 0 | participant colour (made legible downstream), blend helper, scrim derivation over video |
| `slides/src/model.rs` | 1 | 1 | 0 | the authored "Deep" slide treatment |
| `music/src/art.rs` | 1 | 1 | 0 | generated cover art |
| `db-client`, `remote`, `osk` | 3 | 3 | 0 | blend helper; test fixtures |
| **Total** | **155** | **122** | **33** | |
