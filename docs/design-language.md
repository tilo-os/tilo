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

**Workspace temperature.** Each workspace derives the neutral hue (never more
chroma) from its wallpaper. Switching workspaces feels like entering a
different room. The fallback hue is 255.

**No brand accent.**
- A primary action is an *inverted* control: primary ink as its fill and the
  content surface as its label.
- A selected row, tab or segment is a *lift* (HIG §9.3): one surface step plus
  primary ink, with no hue.
- A switch that is on uses the inverted fill.
- Links are primary ink with an underline.

**Your colour.** The local user's presence colour is their focus ring, text
caret and text-selection tint, the same colour their cursor has for
collaborators. Each collaborator's and agent's colour marks their presence,
cursor, selection and attribution.

**Presence palette.** Twelve hues at equal OKLCH lightness and chroma (dark
L 0.74 C 0.14; light L 0.58 C 0.15) that differ in hue only, so no participant
looks more important than another.

**Status.** Danger, warning and success exist only for status. They are muted
(C ≤ 0.13) and never used as decoration. A destructive button uses danger ink
on a neutral control and is filled only inside the confirmation step.

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

Pills are for things that are pill-shaped by meaning (people, tags, filters).
Buttons and tabs are not pills.

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

## Type

The UI face is chosen once for the whole system (see the evaluation in
`fw-type-core`). Noto remains the fallback for scripts the UI face lacks.
Reading text is serif and code is mono, both from the font catalog.

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

- Sizes derive from one base (Body = base); text scale changes everything at once.
- Modifiers keep the role: `.strong()`, `.numeric()` (tabular figures), `.italic()`.
- **No all-caps or letter-spaced labels.** Section headers are sentence case
  in `Subheading` or `Heading`.
- Hierarchy comes from size and weight contrast plus ink tier, never from
  colour.

## Space and density

- The 4 px spacing scale (`s1`–`s16`) is unchanged.
- The content gutter is 24 and the gap between groups is 24–32.
- Rows are 40 high by default (Regular) and 32 in Compact density.
- Control heights are 28 (Small), 32 (Regular) and 40 (Large).
- Density is a system or per-app preference that the toolkit applies. Apps do
  not cram by hand.
- Reading and forms are bounded to a readable width. Canvases, tables, maps
  and media use the full width.

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
