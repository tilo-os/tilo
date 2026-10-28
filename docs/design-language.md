# Tilo design language

This is the single source of truth for how Tilo looks. `crates/sdk/ui-core/HIG.md`
says how to build screens; this document says what they look like and why.
Where the two disagree, this document wins and the HIG is corrected.

## The idea

Tilo is shared live state: people, agents and devices working in one
workspace. The interface is a quiet material with a cool temperature, and
**colour has a role or it is not there**: the person's accent marks what
they act on, green marks what is on, presence hues mark other people, muted
status tones mark status, and the user's own content (photos, album art,
calendars) carries its own. Nothing else gets colour.

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

### Roles, and nothing else gets colour

| Role | Colour | Used for | Never used for |
|---|---|---|---|
| On state | green `intent.on_state` (OKLCH 0.62/0.15/150 dark, 0.58/0.15/150 light), white knob or mark | a switch's track, a checked checkbox or radio, an active Quick Settings tile, a "connected" dot | sliders, selection, buttons |
| Accent | one hue, by default **the person's own colour** (`Emphasis::You`), settable in Appearance (`Emphasis::Tinted`) | a primary button's fill (`intent.accent`, white label), the caret, the text selection, the keyboard focus ring, links (`intent.accent_ink`), the selected row's lift tint, a value's fill (`intent.value_fill`: a slider's filled track, a progress bar, a gauge) | hover, resize grips, window edges, icons at rest, borders |
| Presence | the twelve presence hues | other people: avatars, cursors, their selection, what they are doing | anything that is not a person |
| Status | muted danger, warning, success, info (C ≤ 0.13) | status text, badges, a destructive button's label | decoration |
| Neutral | everything else | surfaces, ink, icons, hairlines | — |

- **Focus ring:** keyboard focus only (`ui::focus_visible`), on every
  focusable control. A pointer press never shows it. A text field's focus is
  its caret.
- **Window edge resize:** the resize cursor first; resting on an edge for
  a moment lights the window's outline along that whole edge in secondary
  ink, primary ink while dragging, with the window's size in a chip beside
  it that fades once let go. Nothing marks an edge at rest; never the accent.
- **Slider and progress:** the value is the accent as a mark
  (`intent.value_fill`) on a lift of the ground, never the text colour. A
  value is not an on state.
- **Buttons:** primary is the accent fill with a white label; secondary the
  raised level; quiet (`ButtonRole::Plain`) has no fill and lifts on hover;
  destructive is secondary with a danger label, filled only inside the
  confirmation step, where Return is still the safe answer.
- **Lifts:** hover and press are neutral lifts of the ground they stand on;
  only a *selected* row, tab or segment carries the accent's faint tint
  (`Colors::selected_on`). A menu's armed row is the neutral lift.
- **Monochrome:** `Emphasis::Inverted` keeps the old colourless look for
  those who want it: primary ink as the primary fill, colourless selection.
  High contrast uses it.

### Surfaces: five levels with real separation

All neutrals are OKLCH lightnesses at one hue, so a theme or a workspace can
shift their temperature without moving contrast. The stock temperature is
hue 260 at chroma 0.006: cool, not grey-blue. Chroma never exceeds 0.010.

| Level | Token | Dark L | Light L | Edge | Shadow |
|---|---|---|---|---|---|
| Ground (desktop) | `surfaces.base` | 0.14 | 0.90 | — | — |
| Window (app content) | `surfaces.content` | 0.19 | 0.995 | the window edge hairline | soft: y 2, blur 16, 0.28 dark / 0.10 light |
| Region (sidebar, inspector) | `surfaces.sidebar`, `surfaces.inspector` | 0.165 (recessed) | 0.965 | none: the step separates | — |
| Raised (field wells, segmented tracks, secondary buttons, objects) | `surfaces.raised` | 0.235 | 0.95 (a darker well on light) | none | — |
| Floating (menu, popover, palette, sheet, Quick Settings, tooltip) | `surfaces.overlay` | 0.225, material when blurred | 1.0 | 1 px inner hairline, white 7 % dark / black 8 % light | two layers: y 1 blur 2 (0.30 / 0.08) and y 12 blur 32 (0.38 / 0.14) |

Ink on the window: primary L 0.96 / 0.18, secondary 0.76 / 0.43, tertiary
0.62 / 0.56 (dark / light). Every pairing meets WCAG AA for its role, and
the contrast tests (`tokens::contrast`) enforce it.

A child never paints a surface of its parent's level, and never a raised
surface inside a raised one; `ui::widget::Surface` refuses to.

**Dark presets.** Cool (255 / 0.008), Warm graphite (60 / 0.006) and Soft
(270 / 0.003, lifted: base 0.19, content 0.25, region 0.228, raised 0.30,
floating 0.32; its accent is light with a dark label) keep the contract.

**Workspace temperature.** Each workspace derives the neutral hue (never
more chroma) from its wallpaper, so switching workspaces feels like entering
a different room. A grey wallpaper keeps the stock temperature.

**Your colour.** The local person's presence colour is what collaborators
see their cursor in; by default it is also their accent. Until they choose
one, it derives from their stable identity, so every machine shows the same.
Each collaborator's and agent's colour marks their presence, cursor,
selection and attribution. Unread counts and mentions are filled in the
person's colour too.

**Presence palette.** Twelve hues at equal OKLCH lightness and chroma (dark
L 0.74 C 0.14; light L 0.58 C 0.15) that differ in hue only, so no
participant looks more important than another.

**Status.** Danger, warning and success exist only for status. Danger red
means an error or a failure, never unread, new or live.

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

**Elevation.** A window tile casts one soft shadow onto the ground; a
floating thing casts two (a tight contact layer and a wide one) and draws a
1 px inner hairline. Bars, cards and selected controls cast none.

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
- The one exception is chrome over pixels the person is choosing: the capture
  bar and its selection controls use the **see-through material**: the
  blurred backdrop under `surfaces.overlay` at `surfaces.see_through_opacity`
  (50 %), so the screen under it stays visible
  (`floating_surface::Material::SeeThrough`). Where there is no backdrop pass
  it is the half-transparent ground alone. It keeps the floating hairline and
  casts its shadow only outside itself. The bar moves away from what is
  chosen, so the blur never hides it. Reduced transparency, forced colours
  and high contrast make it opaque.
- The window group's docked tab strip is a floating-material surface too: one
  frosted body behind the whole strip (the desktop behind it, blurred and
  tinted), with the active tab an opaque lift on it, lighter than the strip
  on a light theme too, and a hovered one a translucent film of that lift.
  Tabs inside a window stand on the opaque window ground with the same tiers
  and no strip. High contrast and reduced transparency make the strip
  solid.

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

**Rail or list.** A navigation sidebar reads by its width. Narrow, it is a
rail: each destination a tile, its icon over a short caption. Wider, it is
a list: the icon beside the label. The person drags the sidebar's edge
(pointer, finger, or the arrow keys on its separator) and the width follows
one to one, from the rail's width to the sidebar's maximum; the rows change
form where every label stands whole beside its icon, never past the
sidebar's minimum, so nothing jumps under the pointer. Dragged past the
rail it hides, as the sidebar toggle hides it. The width is remembered per
app on this device. Rail tiles never stand in a wide sidebar, and list rows
never in a rail.

Which form a sidebar starts in follows the destinations it can hold, not
the app and not what has loaded so far: a sidebar whose places come under
headings (a contact book's groups, a mail account's folders) declares so
and starts as a list even before any heading arrives. The rule runs once,
when a window first shows its sidebar; places that load afterwards never
change its form or width in that window's life. Only the person's drag
does.

| Starts as | When |
|---|---|
| Rail | One flat set of at most seven destinations, each with an icon and a label that stands whole in a tile (two caption lines, broken only at spaces), with no headings, folding, "New" action or filter, no actions in the sidebar's header, and no row that says more than its label (a second line, a figure, a check, a status, a colour, people) or nests or renames |
| List | Anything else: headings, many or growing sets of places, nesting, long labels, rows with more to say |

Today that makes these sidebars rails: Tube (six places), Clocks (four
screens), and Containers (four places; their counts show once widened); and these
lists: Settings, Mail, Files, Contacts (its groups), Feeds (its header adds a subscription), Passwords,
Snippets, Shortcuts, Backups, Themes, Database client, Disks (sizes and
health), Firewall (no icons, the active zone's label). Remote's rail is
always a rail (`NavMode::Docked`). A sidebar that is not a navigation list
alone (API tester's environment picker over its lists, Notes' notebook
column) keeps its list width. Apps that paint their own sidebar (Photos,
Music, Chat, Calendar) do not have the rail yet.

## Space and density

- Spacing is the 4 px rhythm used by role, never by number
  (`spacing.rhythm`): `tight` (4) inside a control, `related` (8) within a
  cluster, `sibling` (12) between rows and clusters, `section` (16) as card
  and panel padding and between sections, `group` (24) between groups and as
  the page gutter.
- Control heights are 28 (Small: bar controls, window controls), 32
  (Regular: buttons, fields, segmented) and 40 (Large: primary actions in
  sheets, Quick Settings tiles' minimum).
- Rows are 36 (one line), 52 (title and secondary line) and 64 (led by a
  40 px avatar or icon); icon-to-text is `sibling`.
- The top bar is 32 high; the window bar follows `window bar`; 4 px separate
  them.
- **Density** is a token mode (`tokens::Density`, `Theme::with_density`) set
  per window or per region: compact scales the rhythm by 0.75 and rows to
  28 / 44 / 52; control heights and text stay. Data-heavy regions (Monitor,
  the Files list view, tables) choose compact. Apps do not cram by hand.
- Reading and forms are bounded to a readable width. Canvases, tables, maps
  and media use the full width.

## Theming

Everything in this document is a theme's default, not a hard-coded value.
`docs/architecture/theming.md` defines the contract: a validated, versioned
theme document that sets semantic roles, resolved through the built-in,
distribution, user, workspace and accessibility layers. A theme can set:

- **Palette:** neutral hue and chroma, the surface lightness ladder, ink
  tiers, and status colours.
- **Emphasis:** which hue the accent takes. The default is the person's own
  colour; a theme may choose one hue (*tinted*) or none (*inverted*, the
  monochrome look). The on state is green whatever the emphasis.
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

One icon set (`ui::icons`, Lucide), 1.5 px stroke, in ink colours: 16 in rows
and controls, 20 as a bar's leading app icon. Icons rest in secondary ink and
turn primary on hover or while active. No emoji or Unicode glyphs as icons
(arrows, check marks, crosses, dots, stars); `tools/policy/check_primitives.py`
and `check_copy_language.py` refuse them.

## Primitives

Each primitive owns its padding, radius, colour and motion; callers pass
content, never geometry or colour (`ui::widget::primitives`).

| Primitive | Is |
|---|---|
| `Surface(level)`, `paint_surface` | one of the five levels with its edge and shadow; children ask `SurfaceLevel::inner_radius` for a concentric radius |
| `FloatingPanel`, `FloatingPanelFace` | header slot, body, footer slot; a field in the header renders embedded; a hairline under the header only while the body holds something |
| `TextStack` | title (Body or Label, medium), secondary (Caption, secondary ink), tertiary; fixed gaps |
| `Facts` | separate facts (icon and text) with space between, dropping the lowest priority first; never a joined string |
| `PropertyList` | label and value pairs |
| `KeyHintsFace` | drawn keycaps and what they do |
| `Breadcrumb` | drawn chevrons between segments |
| `Button(role)` | primary, secondary, quiet, destructive |
| `IconButton` | square, small or default; its constructor requires the accessible name, which is its tooltip |
| `BarControlFace` | the top bar's and window bars' control: plain at rest, lifts on hover and while open, 28 high |
| `Chip` | people, tags and filters only |
| `Toggle`, `Checkbox`, `Checkbox::radio` | the on-state green, keyboard-only focus ring |
| `ToggleGroup` (segmented) | raised track, item-radius thumb |
| `Slider` | the value fill on a lifted track |
| `ListRow` | leading icon 16 / avatar 24 / avatar 40, a text stack, a trailing value, control or chevron; hover lifts, selection is the accent-tinted lift |
| `DisclosureRow` | a row whose chevron turns and whose body opens in place on `motion::Disclose`; one open at a time in a `DisclosureGroup` |
| `Section` | an optional heading and rows, separated from the next by space, never boxed |
| `EmptyState`, `Banner`, toast | on the tokens |

A card is only for an object (a person, a file, a device), never for a group
of settings. `tools/policy/check_primitives.py` refuses, in
`crates/session/` and `apps/`, hand-painted rounded rects with text,
separator-glyph joins and symbol icons in string literals, and raw colour
literals.

## Motion

- Motion answers an action (open, expand, confirm, move) and shows what changed.
- Every animation is one of the standard motions in `ui::motion` (appear,
  drill, disclose, panel slide, cross-fade, list change, snap, zoom, window
  motion, ambient, feedback, attention; HIG §9.7), so the same relationship
  always moves the same way. No literal durations, curves or springs.
- Motion never delays the person: input acts at once, nothing bounces, a
  keyboard shortcut's result shows on the next frame, and repeated actions
  collapse into one motion.
- Nothing animates at idle.
- Reduced motion: movement (slides, scales, springs, snaps) lands at once; a
  change of what is shown may cross-fade, at most `fast`; looping indicators
  stand still. Progress, status and new content keep updating. Read it
  through `Theme::motion()` (`tokens::EffectiveMotion`); the same rule is in
  HIG §9.7.

## Emoji and mentions

A text field offers both by default, from the toolkit: `:name:` becomes the
emoji the catalogue names, `@name` becomes a person or an agent in the room.
The catalogue belongs to `ui`, so an app never links one to have emoji; a
workspace with custom emoji adds to the toolkit's catalogue rather than
replacing it.

Opting out is a property of the field, not of the app: a search field, a
password, a code editor, a terminal, a hex view and a URL are plain (UTF-8
only), because `:` and `@` are syntax there. Say which and why at the call
site — a field with completion switched off for no stated reason is
indistinguishable from one where it was forgotten.

## Words and structure

These rules are the ones that keep generated filler out.

- No taglines or subtitles under headings. Compositions have no subtitle slot;
  a description exists only when it tells the user something they need to act.
- An app never titles its own window. The OS chrome names the app.
- No all-caps labels and no facts joined with "·", "•", "|", "—", "–" or
  " / ". Use a secondary line, `Facts`, a `PropertyList`, key hints or
  columns.
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
