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
   needs to say something new, the schema (`CustomThemeSpec`,
   `CUSTOM_THEME_SCHEMA_VERSION` in `crates/sdk/ui-core/src/theme.rs`) gains a
   role and a version. It never grows CSS-like access to widget internals.

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
surface one step above the ground, which primary and secondary text must read
on". The current role set lives in `crates/sdk/ui-core/src/tokens/`:

| Family | Roles | Where |
|---|---|---|
| Surfaces | `base`, `raised`, `overlay`, `scrim`, `glass`, `window_card`, `window_card_border` | `tokens/colors.rs` (`Surfaces`) |
| Text | `text_primary`, `text_dim`, `text_tertiary` | `tokens/colors.rs` (`Colors`) |
| Accent | accent ramp, `ACCENT_PRIMARY_IDX` / `ACCENT_HOVER_IDX`, `border_focus` | `tokens/colors.rs` |
| Semantic | `success`, `warning`, `danger`, `info` | `tokens/colors.rs` (`SemanticColors`) |
| State veneers | `overlay_subtle` / `medium` / `strong`, `overlay_border_*` | `tokens/colors.rs` |
| Shape | radii `sm` / `md` / `lg` / `xl` / `full` | `tokens/radii.rs` |
| Density | spacing `s0`–`s16`, `Metrics` (pointer targets, panel widths) | `tokens/spacing.rs`, `tokens/metrics.rs` |
| Type | families, scale `xs`–`x4l`, `font_size_base` | `tokens/typography.rs` |
| Motion | durations and easings, `reduce_motion` | `tokens/motion.rs` |
| Depth | elevation `e0`–`e4` | `tokens/elevation.rs` |
| Derived palettes | terminal ANSI palette, syntax palette, categorical series | `tokens/terminal_palette.rs`, `tokens/syntax_palette.rs`, `Colors::categorical` |

A theme sets the authored roles. Everything else is **derived** in one place
(`Colors::themed`, `Theme::from_tokens`): ramps, veneers whose polarity follows
the ground, the terminal palette, ink measured against whatever surface a
widget paints on (`Colors::on_surface`, `ink_on`, `legible_fill`). Derivation
is what lets a ten-colour theme cover forty apps without a theme author ever
seeing them.

### Data, versioned

The authored form is `CustomThemeSpec`, and it is the document for built-in
defaults, distribution defaults, and the user theme. Schema 1 is an id, a name,
eleven colours, `radius_px`, `spacing_px` and a font. Schema 2 adds
`motion_scale`, `material_scale` and `icon_scale`. Schema 3 adds
`chrome_weight` (the rule under a toolbar and along a status bar; `1` is the
hairline, `0` draws none), `motion_character` (the shared UI springs, from
calm through stock to lively; drag tracking stays put) and `material_tint`
(how far glass carries the accent; `0` is the stock film). A missing scale or
chrome weight or motion character is `1.0`, and a missing tint is `0`, so a
schema 1 or schema 2 file still opens. It is
stored in `appearance.custom_theme` of the shared settings record, and Theme
Studio is its only writer (`docs/settings-and-studios.md`, Themes). Built-in
presets are the same contract expressed as `PaletteSpec` constructors.

New roles arrive as new schema versions. A reader accepts every version it
knows and fills new roles by derivation, so a file written by an older build
still opens.

### Validation: reject structure, repair legibility

Validation has two halves, and they fail differently on purpose:

- **Structure** (`CustomThemeSpec::validate`): schema version, id/name/font
  shape, finite in-range radius and spacing. A file that fails is not a theme;
  it is rejected and the user's previous or default theme stays.
- **Legibility**: the contrast floors in
  `crates/sdk/ui-core/src/tokens/contrast.rs` (`FLOORS`). Theme Studio refuses
  to save or export a theme that misses one (`contrast_findings`), and shows
  which pair and by how much. A theme that reaches the runtime anyway — a file
  written by an older build, hand-edited, or produced by a future tool — is
  **repaired, not rejected**: each failing foreground is stepped toward the
  contrasting extreme until it clears its floor (`Theme::from_custom_spec`),
  the same "clamp, don't drop the user's theme" rule `spacing_px` already
  follows (`CustomThemeSpec::SPACING_PX_MIN`). `validate` stays ranges-only on
  purpose, so a structurally valid legacy file still opens.

The floors are WCAG 2.x ratios, applied to the pairs apps actually rely on:

| Foreground | Ground(s) | Floor | Why |
|---|---|---|---|
| `text_primary` | `base`, `raised`, `overlay` | 4.5:1 | body text (WCAG AA) |
| `text_dim` | `base`, `raised`, `overlay` | 4.5:1 | captions and labels are still body text |
| `text_tertiary` | `base` | 3:1 | quietest tier; the token's documented contract is large-text AA |
| accent primary | `base`, `raised` | 3:1 | active states: toggle on, slider track (WCAG 1.4.11) |
| `border_focus` | `base` | 3:1 | focus indicator (WCAG 2.4.11) |
| `success`, `warning`, `danger`, `info` | `base` | 3:1 | status dots and icons |

Labels drawn *on* an accent or semantic fill are not part of the floor table
because they are always derived (`Colors::on_accent`, `legible_fill`,
`tinted_chip_on`) and cannot be authored wrong. One rule table serves the
shipped presets, Theme Studio and the runtime, and
`every_theme_that_materializes_keeps_the_contrast_floors` in `theme.rs` runs
every preset through it.

## Layers and precedence

The theme a person sees is resolved from layers, lowest to highest:

| # | Layer | Who writes it | Typical contents |
|---|---|---|---|
| 1 | Built-in defaults | the toolkit, in code | [`Theme::dark()`](../../crates/sdk/ui-core/src/theme.rs): the stock dark palette, shape, density, type and motion. This is the code default. It is not a file |
| 2 | **Distribution defaults** (`distribution` / `distribution_defaults`) | whoever ships the OS image | the same document as the user theme. Pins may name any of its roles. |
| 3 | User theme | the person, through Settings and Theme Studio, or an agent through the same actions | the same document: colours, radius, spacing, font, motion scale and character, material scale and tint, icon scale, chrome weight |
| 4 | Workspace tint (optional) | the person, per workspace | an accent or identity hue for one workspace, on their own screen |
| 5 | Accessibility | the person or assistive policy | high contrast / forced colours, text scale, UI scale, reduce motion, thick caret |

**Accessibility always wins.** Nothing below layer 5 can lower contrast,
shrink text, or re-enable motion that the person turned off, and distribution
defaults cannot pin a value that layer 5 touches.

**Built-in defaults are `Theme::dark()`.** `resolve_theme_layers` uses
that constructor when no higher layer supplies a palette. There is no
defaults file, and nothing about this layer is signed.

**Distribution defaults** (identifiers: `distribution`,
`distribution_defaults`) are the layer for whoever ships the Tilo OS image —
an agent-sandbox provider, a hardware vendor, an organisation's managed
fleet. The layer sets the same fields a user theme sets, and may **pin** any of
them (`palette`, `accent`, `shape`, `density`, `type`, `motion`, `material`,
`icons`, `chrome`). A pin beats the user theme only for that field.
Accessibility fields are not pinnable. Wallpaper, greeter artwork and boot artwork are image files, not roles on
this document, so they are not an option on any of the three layers.

Distribution defaults, the user theme, the workspace tint and accessibility
live in the settings document as **versioned JSON**. The settings store does
not sign appearance records, and this design does not add a signature. Each
of those layers carries a `schema_version`. `validate` runs before the layer
is applied: a schema this build does not admit is left unused, and the theme
stays as the layers below it resolved. A finite value inside an admitted
schema that falls outside the usable band — a custom theme's `spacing_px`,
an accessibility text or UI scale — is clamped when the layer is
materialized, before it changes the theme. A non-finite accessibility scale
rejects that payload the same way an unknown schema does.

Resolution is one deterministic function over the stack, in the style of the
default-application resolver in `docs/settings-and-studios.md`. Layers
contribute individual roles, not whole themes: distribution defaults that
set only an accent leave the user's choice of light or dark alone.

### What users get

Far more than an accent colour, because every one of these is a token and not
an implementation detail: palette (every surface, text tier, accent and
semantic colour), shape (radius scale), density (spacing scale, kept
proportional), type (family and scale), motion (durations and the character
of the UI springs), materials (glass, elevation, and how far the film carries
the accent), icons, and chrome weight (a hairline rule or a heavier one).
Each is safe to expose because apps consume only the role, and the validator
guarantees the role keeps its promise.

A person and an agent author the same document. Theme Studio's sliders and
the Themes actions (`SetCustomThemeChrome`, `SetCustomThemeMotionCharacter`,
`SetCustomThemeMaterialTint`, and the scale actions beside them) write the
same fields. An agent does not invent a widget. Individuality is which
values fill the roles; the widget vocabulary stays the one the toolkit draws.
When a look still does not fit, the schema gains a role. It does not gain a
selector.

## App developers

What an app may do:

- **Supply a brand accent.** An app may declare one accent colour. The
  toolkit checks it against the active theme's ground with the accent floor
  above and falls back to the OS accent where it fails, so an app's identity
  colour never costs a user legibility. The user can turn app accents off.
- **Own its content surfaces.** A canvas, a document page, a video, a game or
  a map is content, and the app chooses how it looks. The reference pattern is
  `apps/ebook/src/reader_theme.rs`: all chrome (toolbars, drawers, buttons)
  reads strictly from `Theme`, while the page uses a palette the reader picks
  (light, sepia, dark, high contrast), with on-page affordances tinted from
  the OS accent.

What an app may not do:

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

A workspace may carry an **identity colour** — a hint so that "the design
review workspace" is recognisable at a glance. It is a hint, never an
override: the viewer's theme decides whether and how it is shown (layer 4),
and accessibility can suppress it. Collaborator colours work the same way:
the room settles a colour per person for cursors and presence, and each viewer
renders it through their own theme's legibility derivation.

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

- **Contrast validation has landed.** `CustomThemeSpec::validate` is still
  ranges-only (schema version, id, name, font, radius, spacing) so a
  structurally valid legacy file opens. The legibility half lives in
  `tokens/contrast.rs` (`FLOORS`, `audit`, `repair`). Theme Studio refuses to
  save a custom theme with findings (`apps/themes/src/store.rs`,
  `ensure_legible`). `Theme::from_custom_spec` repairs a file that reaches the
  runtime anyway. Every shipped preset is swept against the same floors
  (`every_theme_that_materializes_keeps_the_contrast_floors`).
- **Layer resolution has landed.** [`theme::resolve_theme_layers`](../../crates/sdk/ui-core/src/theme_layers.rs)
  resolves built-in defaults (`Theme::dark()` in code, not a file),
  distribution defaults (`distribution` / `distribution_defaults`), the user
  theme, an optional workspace tint, and accessibility, lowest to highest.
  Distribution defaults, the workspace tint and accessibility are versioned
  JSON in settings. Each has `schema_version` and `validate`, and a rejected
  schema is not applied. Policy pins live on `DistributionPolicy.pins` and
  name `palette`, `accent`, `shape`, `density`, `type`, `motion`, `material`,
  `icons`, or `chrome`. A pin beats the user theme for that
  field; an admitted accessibility layer still replaces the colour track and
  applies text scale, UI scale and reduced motion last. Out-of-range
  accessibility scales are clamped in `AccessibilityLayer::materialize`
  before that apply. `Theme::configured` reads the same function. Wallpaper,
  greeter and boot artwork are not part of this layer. Appearance records
  are unsigned.
- **Forced colours.** `Theme::colors_are_forced` and `Theme::content_surface`
  answer "am I forced?" with the theme's page, ink and accent tokens. A
  content palette keeps its own page when the flag is clear. Apps still have
  to honour the flag; the toolkit does not restyle them.
- **Brand accent.** `Theme::brand_accent` takes one `[u8; 3]`. It must clear
  the 3:1 accent floor on the active base and raised surfaces. A miss, a
  cleared `appearance.app_accents` switch, or forced colours rejects it and
  the theme accent stays. The call cannot replace surfaces, text, chrome or
  accessibility.
- **Hard-coded colours in apps.** `rg -c "Color::from_rgb8\(|Color::rgb8\(|Color::new\(\["`
  still counts 155 sites in `apps/`. 122 are legitimate (document and media
  colours, stored user choices, colour-math helpers, test fixtures) and 33
  are chrome that should come from `Theme`; see the appendix. That regex
  misses `Color::from_rgba8` (71 sites) and `Color::WHITE`/`BLACK` (150
  sites). The HIG §9.13 `theme` and `app-paint` ratchets now count all of
  these per app, and the counts may only fall.
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
   the appendix. Add a shared role wherever an app had to invent one (a
   distinct-hue series for spreadsheet references, a forced-colours flag for
   content surfaces) rather than hard-coding it locally. Each app empties
   its shards under `crates/sdk/ui-core/tests/baselines/` as it goes.
2. **Layer resolution.** Landed: `theme::resolve_theme_layers`.
3. **Forced colours and brand accent.** Landed: `Theme::content_surface` and
   `Theme::brand_accent`. Still open: a compositor-drawn trusted-UI marker no
   theme or app can reproduce, and wallpaper / greeter / boot artwork on the
   distribution layer.

## Appendix: hard-coded colours in `apps/`

Classification of the 155 `from_rgb8` / `rgb8` / `Color::new([` sites, by
file. **Content** means the colour belongs to the document, the media or the
person: page and paper palettes, a stored or user-picked colour, overlays that
keep controls legible over live video, colour-math helpers that convert rather
than choose, and test fixtures. **Chrome** means the colour is a UI role the
theme should supply.

| File | Sites | Content | Chrome | Notes |
|---|---:|---:|---:|---|
| `ebook/src/reader_theme.rs` | 27 | 27 | 0 | page palettes (the reference pattern) |
| `ebook/src/app.rs` | 4 | 2 | 2 | scheme swatches are content; badge ink should be measured (`readable_on`) |
| `ebook/src/model.rs`, `input_controls.rs` | 3 | 3 | 0 | user-picked scheme and highlight colours |
| `word-processor/src/widget.rs` | 19 | 16 | 3 | paper, paper ink, table and placeholder tints and stored run colours are content; caret, active-band edge and spelling squiggle are affordances that should come from the accent and `danger`, made legible on paper |
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
