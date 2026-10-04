# Theming — a token contract the OS owns

Tilo applications do not own their pixels. An app declares its interface
through `ui`, and the toolkit and compositor draw it. Theming follows from that
one decision: a theme is a **typed, validated set of tokens owned by the OS**,
not a stylesheet an application author writes and a user overrides.

Four rules generate the rest of this document:

1. **Themes target semantic roles, never implementation.** A theme says what
   "the raised surface", "secondary text" or "danger" look like. It never says
   what a `Button`'s third child looks like, and there is no selector language
   in which it could.
2. **Themes are data, never code.** A theme is a document of numbers and
   strings that the OS validates and then applies. Nothing in it executes.
3. **A theme that passes validation cannot break an app.** Validation covers
   legibility, not only ranges, so "this theme broke my app" is not a bug class
   an app developer can receive.
4. **Expressiveness grows by adding roles to a versioned schema.** When a theme
   needs to say something new, the schema (`ThemeDocument`,
   `THEME_SCHEMA_VERSION` in `crates/sdk/ui-core/src/theme_document.rs`) gains
   a role and a version. It never grows CSS-like access to widget internals.

It builds on the HIG (`crates/sdk/ui-core/HIG.md`, §7 and §9: "roles, not
pixels"), on `docs/settings-and-studios.md` (who owns which theme state), and
on `docs/architecture/distribution.md` §6 (Profile layers), which the layer
model below reuses rather than reinvents.

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
| `shape` | `window_radius` | `radii.{tile, panel, control, item, detail}` derived concentrically (`Radii::from_window_radius`) | `tokens/radii.rs` |
| `density` | `spacing`, a multiple of the 4 px scale | `spacing.s0`–`s16`; `Metrics` (pointer targets, control heights) stay put | `tokens/spacing.rs` |
| `type` | `face` (a built-in face by name, `Default` or `Hyperlegible`, or any family name), `base_size` | `typography.sans_family`, the body size every `TextRole` derives from (`Theme::text(role)`) | `tokens/typography.rs` (`UiFace`, `Typography::set_face`) |
| `motion` | `character`: 0 calm, 1 stock, 2 lively | the shared UI springs, `motion_character` | `tokens/motion.rs` |
| `material` | `blur`, `tint_opacity` of floating chrome | `surfaces.material_blur_px`, `surfaces.material_opacity`, `surfaces.material` | `tokens/colors.rs`, `backdrop.rs` |

Not theme roles, by design: there is no accent and no app colour (an app
has no brand colour in this language), no per-widget colour, no way to
remove the selection lift or the focus ring, and no chrome weight or icon
size. A person's own colour is identity, not theme ([Your
colour](#your-colour)). Derived palettes — the terminal ANSI palette, the
syntax palette, chart series (`PresencePalette::series`) — follow the
colour tokens.

Everything is derived in one place (`Colors::as_authored`, then
`Theme::from_document`): every surface and ink tier is an OKLCH lightness
at the palette's hue and chroma, so a hue never moves contrast.

### Data, versioned

The document is `ThemeDocument`
(`crates/sdk/ui-core/src/theme_document.rs`, `THEME_SCHEMA_VERSION` = 5):

```json
{
  "schema_version": 5,
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
  "shape": { "window_radius": 14.0 },
  "density": { "spacing": 1.0 },
  "type": { "face": "Noto Sans", "base_size": 14.0 },
  "motion": { "character": 1.0 },
  "material": { "blur": 24.0, "tint_opacity": 0.72 }
}
```

`palette.hue` may be absent: the palette then has no hue of its own and is
pure grey until the workspace it is shown in gives it one.

Built-in presets, distribution defaults, the user's themes and every file
Theme Studio exports are this document. Unknown fields are refused, and a
document of any other schema version is not admitted: the layer it sits on
contributes nothing and the layers below it apply. There is no migration
from older schemas.

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

### Validation and legibility

`ThemeDocument::validate` checks structure: the schema, id/name/face, and
every number finite and inside the band its role admits (chroma ≤ 0.010,
ink on the ink side of every surface, status chroma ≤ 0.13, presence
lightness 0.35–0.90 and chroma 0.04–0.20, window radius 0–40, density
0.5–2, base size 10–24, motion 0–2, blur 0–64, tint opacity 0.6–1). A value
admitted but outside the usable band (radius above 28, density outside
0.75–1.5, base size outside 12–18, blur above 48) is clamped when the theme
is built.

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

## Layers and precedence

The theme a person sees is resolved from layers, lowest to highest:

| # | Layer | Who writes it | Contents |
|---|---|---|---|
| 1 | Built-in defaults | the toolkit, in code | `Preset::Dark` |
| 2 | **Distribution defaults** (`appearance.distribution_defaults` / `appearance.distribution`) | whoever ships the OS image | a preset id or a whole document, and pins |
| 3 | User theme | the person, through Settings and Theme Studio, or an agent through the same actions | the theme for the current mode, and the person's density |
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

A section with none of `mode`, `dark_theme`, `light_theme` is no user
theme, so distribution defaults still show.

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
does not). Collaborators see the same colour: every presence plane carries
it (`NeighbourMsg::PresenceMoved` and `AppPresenceV2` as the hue's
`PresenceHue::id`, a live frame's `LiveParticipant::color`), and each
receiving shell paints that peer in it and announces it to its apps in the
roster (`Collaborator::colour`). It is their focus ring, text caret, text-selection tint and unread
mark (`intent.you`, `selection_tint`, `border_focus`), painted at the
current theme's presence lightness and chroma, so it reads on every theme.
The theme sets only that lightness and chroma.

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
ceremony (`crates/session/shell/src/onboarding_view.rs`), the capability
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

- **The document and its presets.** `ThemeDocument` schema 5 with the
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
- **The material.** Blur and tint opacity are theme roles read by the
  backdrop pass; reduce transparency, forced colours and the high-contrast
  palettes keep floating chrome opaque.
- **Theme Studio** (`apps/themes`) edits the document role by role with
  live findings and applies or exports only legible themes. Settings does
  not yet expose the appearance controls; `docs/ui-migration.md`
  ("Theming") lists what it must offer.
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

1. **Hard-coded chrome colours onto tokens**, one app per change, following
   the appendix and the ratchet shards under
   `crates/sdk/ui-core/tests/baselines/`.
2. **Settings > Appearance** exposes the controls listed in
   `docs/ui-migration.md` ("Theming").
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
