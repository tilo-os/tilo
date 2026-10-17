# Tilo design language

This is the single source of truth for how Tilo looks. `crates/sdk/ui-core/HIG.md`
says how to build screens; this document says what they look like and why.
Where the two disagree, this document wins and the HIG is corrected.

## The idea

Tilo is shared live state: people, agents and devices working in one
workspace. The interface is a quiet, near-colourless material, and **colour
means that someone is there or that something is happening**. Collaborator
and agent presence, live/recording/syncing state, and the user's own content
(photos, album art, calendars) carry all the chroma. Nothing else does.

Coherence comes from the toolkit, not from discipline. Apps say what a thing
*is* (a list, a section, a form, a primary action, a destructive action) and
the system decides how it looks. An app that cannot express an appearance has
no appearance to get wrong.

## Semantic API, not stylistic API

- Widgets take intent: `TextRole`, `ButtonRole::{Primary, Secondary,
  Destructive, Plain}`, `ControlSize::{Small, Regular, Large}`, list styles
  (`Sidebar`, `Inset`, `Plain`), surface regions (`Content`, `Sidebar`,
  `Inspector`). They do not take colours, font sizes, radii, paddings or
  border widths from apps.
- Remove an app-facing appearance knob when its semantic replacement lands;
  do not keep both.
- Genuine content drawing (charts, documents, maps, media, canvases) keeps a
  painting surface. It reads every value from the theme, and the app audits
  track it. Chrome around content is never painted by the app.

## Colour

All neutrals are defined in OKLCH and share one hue, so a theme or a
workspace can shift their temperature without breaking contrast.

**Neutral material.** Chroma stays at or below 0.010.

| Token | Dark L | Light L | Use |
|---|---|---|---|
| `surfaces.base` | 0.16 | 0.90 | Desktop ground behind tiles |
| `surfaces.content` | 0.215 | 0.995 | The window ground: every app, owned by the framework |
| `surfaces.sidebar` | 0.195 | 0.965 | Navigation region of a window |
| `surfaces.inspector` | 0.235 | 0.975 | Detail/inspector region |
| `surfaces.raised` | 0.265 | 1.0 | Fields, wells and objects inside content |
| `surfaces.overlay` | 0.285 | 1.0 | Floating chrome (menus, popovers, palette, sheets) |

Ink on the content surface: primary L 0.96 / 0.18, secondary 0.76 / 0.43,
tertiary 0.62 / 0.56 (dark / light). Every pairing meets WCAG AA for its role,
and the existing contrast tests enforce it.

**Default chroma.** The default dark theme is pure neutral (chroma 0). The
default light theme has no hue of its own: it is pure neutral until the
workspace it is shown in gives it one, and then carries chroma 0.010 at that
hue.

**Dark presets.** Three more dark palettes ship as presets with the same
contract:

| Preset | Hue / chroma | Lightness |
|---|---|---|
| Cool | 255 / 0.008 | As in the table |
| Warm graphite | 60 / 0.006 | As in the table |
| Soft | 270 / 0.003 | Lifted: base 0.19, content 0.25, sidebar 0.228, raised 0.30, overlay 0.32 |

**Workspace temperature.** Each workspace derives the neutral hue (never more
chroma) from its wallpaper. Switching workspaces feels like entering a
different room. A palette with chroma 0 shows no temperature. There is no
fallback hue: a grey wallpaper, a window shown without a shell and a capture
stay pure neutral. The shell gives the temperature to its own chrome and to
every app window in that workspace.

**No brand accent.**
- A primary action is an *inverted* control: primary ink as its fill and the
  content surface as its label.
- A selected row, tab or segment is a *lift* (HIG §9.3): one surface step plus
  primary ink, with no hue. The armed row of a menu is the same lift, one
  and a half steps off its floating panel so it reads at a glance and stays
  quiet.
- A switch that is on uses the inverted fill.
- Links are primary ink with an underline.

**Your colour.** The local user's presence colour is their focus ring, text
caret and text-selection tint, the same colour their cursor has for
collaborators. Their presence carries it to every collaborator's machine;
until they choose one, it derives from their stable identity, so every
machine shows the same colour. Each collaborator's and agent's colour marks their presence,
cursor, selection and attribution. Unread counts, mentions and anything
else that wants the person's attention are filled in their colour too,
the same mark in a sidebar and on a badge.

**Presence palette.** Twelve hues at equal OKLCH lightness and chroma (dark
L 0.74 C 0.14; light L 0.58 C 0.15) that differ in hue only, so no participant
looks more important than another.

**Status.** Danger, warning and success exist only for status. They are muted
(C ≤ 0.13) and never used as decoration. Danger red means an error or a
failure, never unread, new or live. A destructive button uses danger ink
on a neutral control and is filled only inside the confirmation step, where
Return is still the safe answer (Cancel), never the destructive one.

## Shape

**Radii are concentric.** An inner radius equals its container's radius minus
the inset between them, and never goes below 4.

| Thing | Radius |
|---|---|
| Window tile | 14 |
| Floating panel (menu, popover, palette, sheet) | 18 |
| Control (button, field, segmented track) | 8 |
| Item inside a control or list (selected row, segment thumb) | 6 |
| Chip, presence pill, tag, avatar | full |
| A media transport's play/pause | full |

Pills are for things that are pill-shaped by meaning (people, tags, filters).
Buttons and tabs are not pills. The one round button is a media player's
play/pause (`TransportFace`): a filled circle in the primary action's
colours, a size up from previous and next, which stand beside it as solid
glyphs. Transport glyphs are solid, never outlines.

**Edges.** A hairline appears only where two equal surfaces meet. Groups are
separated by space and type weight, not boxes. A card exists only for an
*object*: something you can open, move, share or drag. Settings, form sections
and stats are not cards.

**Elevation.** Tiles have no shadow; their separation is the ground contrast
plus the window edge. Only floating things cast a shadow: one soft, wide
shadow plus, on dark themes, a 1 px light inner edge.

## Material and transparency

- Windows are opaque. Tiles never overlap, so there is nothing meaningful
  behind them.
- Floating chrome (palette, menus, popovers, quick settings, sheets,
  notifications) uses a **blurred material**: the compositor blurs what is
  behind the panel, then tints it with `surfaces.overlay` at 72 % opacity.
  Without backdrop blur it falls back to opaque `surfaces.overlay`. Never use
  unblurred translucency.
- Chrome over media (video controls, photo info) uses the same material on the
  media itself.
- The window group's docked tab strip is one body behind the whole strip,
  with the active tab an opaque lift on it and a hovered one a film of that
  lift. It is solid by default; a theme may make it a floating-material
  surface (frosted: the desktop behind it blurred and tinted), never an
  unblurred film. Tabs inside a window stand on the opaque window ground with
  the same tiers and no strip. High contrast and reduced transparency make
  the strip solid.

## Type

The UI face is Tilo Sans, one face for the whole system (why, and its
provenance: `crates/sdk/ui-core/assets/fonts/PROVENANCE.md`). It sets Latin,
Vietnamese and Greek; Cyrillic falls back to Onest, matched to its x-height
and stem weight, and every other script to Noto. A
theme or the accessibility layer may choose the built-in Hyperlegible face
(Atkinson Hyperlegible Next) instead.
Reading text is set in New Computer Modern, in the optical cut its size
calls for (the 8 pt design for footnotes, Book for body text, Regular for
titles), with old-style figures; a theme or a reader may choose Libertinus,
Literata, EB Garamond or Source Serif instead. Code is mono. All come from
the font catalog.

| Role | Size / weight / leading | Use |
|---|---|---|
| `Display` | 34 / 600 / 1.15 | One hero number or name per screen at most |
| `LargeTitle` | 26 / 600 / 1.2 | Top of a content page |
| `Title` | 20 / 600 / 1.25 | Detail title (a message subject, a contact name) |
| `Heading` | 16 / 600 / 1.3 | Section heading |
| `Subheading` | 14 / 600 / 1.35 | Group label inside a section, list section header |
| `Body` | 14 / 400 / 1.45 | Default text |
| `Label` | 13 / 500 / 1.3 | Control labels, row titles in dense lists |
| `Caption` | 12 / 400 / 1.35 | Secondary line, timestamps, metadata |
| `Reading` | 16 / 400 / 1.6, serif | Long-form documents and books |
| `Code` | 13 / 400 / 1.5, mono | Code and terminal |

- Sizes derive from one base (Body = base); text scale changes everything at once. Glyphs grow with it by `Metrics::icon_growth` (the square root of the text growth), and a timeline's hour by `TimelineDensity`.
- Modifiers keep the role: `.strong()`, `.numeric()` (tabular figures), `.italic()`.
- Apps never pick OpenType features: text says what it is (numeric, secret,
  identifier, link, code) and the toolkit applies what the face supports
  (`docs/typography.md`, Text kinds).
- **Confusable letters.** Prose and labels keep the face's character. Names
  people must not mistake — usernames, handles, email addresses, invite
  codes, room names, other people's file names — get a distinct I and l,
  also when they appear inside a message, note or mail, where the toolkit
  finds them itself. Codes and secrets (TOTP and recovery codes, revealed
  passwords, keys, hashes, IDs) and links add the slashed zero. Permission
  prompts, review sheets, invite and join confirmations and identity
  verification set every look-alike apart. "Always distinguish similar
  letters" (Appearance and Accessibility; on with Hyperlegible) does so
  everywhere.
- **No all-caps or letter-spaced labels.** Section headers are sentence case
  in `Subheading` or `Heading`.
- Hierarchy comes from size and weight contrast plus ink tier, never from
  colour.

## Windows and navigation

Windows have no title bar. The OS tab names the app, and the framework draws
one header row across the top of the window that holds each column's header:
sidebar, list title and detail toolbar together read as one unified toolbar.
The sidebar toggle sits at the top-leading corner of the sidebar; when the
sidebar is hidden it moves to the leading edge of the next column. One
system-wide shortcut toggles it in every app. History, where an app has it,
is one glyph at the row's trailing edge that reads as on while History
shows; no app draws a History button of its own. Narrow windows collapse the
sidebar into a floating drawer instead of squeezing content.

## Space and density

- The 4 px spacing scale (`s1`–`s16`) is unchanged.
- The content gutter is 24 and the gap between groups is 24–32.
- Rows are 40 high by default (Regular) and 32 in Compact density.
- Control heights are 28 (Small), 32 (Regular) and 40 (Large).
- Density is a system or per-app preference that the toolkit applies. Apps do
  not cram by hand.
- Reading and forms are bounded to a readable width. Canvases, tables, maps
  and media use the full width.

## Theming

Everything in this document is a theme's default, not a hard-coded value.
`docs/architecture/theming.md` defines the contract: a validated, versioned
theme document that sets semantic roles, resolved through the built-in,
distribution, user, workspace and accessibility layers. A theme can set:

- **Palette:** neutral hue and chroma, the surface lightness ladder, ink
  tiers, and status colours.
- **Emphasis:** how primary actions, selection and "on" states show. The
  default is *inverted*; a theme may choose *tinted* with one hue, which
  restores an accent for people who want one.
- **Presence:** lightness and chroma of the presence palette (hues stay evenly
  spaced so participants remain equal).
- **Shape:** the window radius; every other radius derives concentrically.
- **Density, type and motion:** spacing scale, UI face, reading face,
  base size and whether similar letters are always distinguished, and spring
  character.
- **Material:** blur strength and tint opacity of floating chrome.

A theme cannot set colours of individual widgets, remove the lift or focus
ring, or break contrast floors. "Your colour" is part of your identity, not
the theme: you pick it once and every device and collaborator sees the same.

## Icons

One icon set (`ui::icons`), 1.5 px stroke on a 20 px grid, in ink colours. No
emoji or Unicode glyphs as icons.

## Motion

- Motion answers an action (open, expand, confirm, move) and shows what changed.
- Springs come from `tokens::motion`, with no literal durations.
- Nothing animates at idle.
- Reduced motion replaces movement with a cross-fade.

## Words and structure

These rules are the ones that keep generated filler out.

- No taglines or subtitles under headings. Compositions have no subtitle slot;
  a description exists only when it tells the user something they need to act.
- An app never titles its own window. The OS chrome names the app.
- No all-caps labels and no `A · B · C` metadata chains. Use a secondary line,
  a `PropertyList` or columns.
- Sentence case everywhere.
- A button names what happens ("Delete message", not "OK").
- Empty states have one sentence and at most one action.
- Demo and fixture content is realistic and complete. A placeholder that looks
  broken ("Access not granted") is a defect.
- No developer notes in the UI.
- Search belongs to the platform. Apps do not draw search fields; they expose
  search scopes (see `docs/ui-migration.md`). Only genuine in-content filtering
  and find-in-page remain, through the framework's controls.

## Secret fields and capture

Secret inputs suppress their values from accessibility, review, presence,
primary selection and input-method surrounding text. Revealed input-method
preedit is secret content even before the committed field contains text.
A native host waits for the compositor's correlated capture-protection
acknowledgement before submitting a frame containing revealed secrets.

Capture protection is currently conservative: after a secret frame is
submitted, protection remains until native app teardown. This also covers
secret content in auxiliary windows and can withhold capture of other app
windows after the auxiliary window closes. A renderer's presentation result
does not prove that the compositor has displayed a clean replacement; early
release requires a compositor presentation barrier and settlement of pending
captures. Disconnecting the control transport keeps protection on displayed
surfaces until their destruction.
