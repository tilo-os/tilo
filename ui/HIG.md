# qOS Human Interface Guidelines

## Purpose & philosophy

These guidelines are **as strict and detailed as the GNOME HIG**, but with one
deliberate difference: they are meant to be **auto-enforcing at the code level**.

GTK's age and history let you build applications that badly violate the GNOME
HIG while still "working". qOS avoids that by making the right thing the *default*
thing in the `ui` toolkit, and by having the toolkit **nudge or refuse** the
wrong thing. When a rule here recurs across apps, the correct response is to
promote a `ui` primitive that bakes the rule in — not to reimplement it (and
re-violate it) per app.

> Rule of thumb: if an app can express a bad-UX layout without effort, that is a
> toolkit bug, not just an app bug.

---

## 1. Navigation & layout: sidebars vs. content panels

There are **two distinct kinds of panel**. Do not conflate them. A single
generic "overlay drawer with a scrim, everywhere" is wrong — it turns *content*
into a menu and clutters small screens.

### 1a. Genuine sidebar / menu  (pure navigation)

Chrome that lists destinations. Examples: a calendar's list of *calendars*; an
email client's accounts + folders.

- **Desktop / wide:** solid, **docked** beside the content. Collapsible via a
  proper **collapse / hamburger button** — **never an `X`**. It reads as part of
  the window frame, not a floating overlay. Smooth collapse/expand animation.
- **Mobile / narrow:** it **stays a genuine sidebar** — a web-style **overlay
  drawer** over the content (scrim behind), summoned and dismissed by the *same*
  proper collapse/hamburger button (**not** an `X`). It remains a menu; it does
  **not** turn into a full-screen page.

### 1b. Content panels  (overview + detail)

Panels that are themselves content the user reads and acts on. Examples: a
calendar's month/week grid (overview) + the event (detail); an email message
list (overview) + the message (detail).

- **Desktop / wide:** shown **side by side** (alongside the genuine sidebar, if
  any). Only the sidebar collapses; the content panels stay abreast.
- **Mobile / narrow:** show **only one of {overview, detail} at a time**. The
  overview is the default **full-screen** view; selecting an entry **slides the
  detail in from the right**, over the overview, with a **back button top-left**.
  Back returns to the overview. This is **push navigation** — never a
  scrim-overlay of content over content.

### 1c. How to decide which a panel is

- Is it *pure navigation* — a list of destinations / accounts / calendars? →
  **genuine sidebar** (§1a).
- Is it *content the user reads or acts on* — a list of messages, events, or
  cities-with-info? → **content panel** (§1b), **even if it sits on the left and
  looks sidebar-ish**.

### 1d. Concrete app mappings

- **Weather** — two **content panels**: a *contentful* city list (each row shows
  quick per-city info) as the **overview**, and the forecast as the **detail**.
  There is **no** genuine sidebar here — the city list is content, so it follows
  §1b. Mobile: full-screen city list → tap a city → full-screen forecast + back.
  Desktop: the two side by side. (Units belong in **Settings**, not in the app.)
- **Calendar / Email** — a **genuine sidebar** (calendars / accounts+folders,
  §1a) **plus two content panels** (overview grid/list + detail event/message,
  §1b).
  - Desktop: `[ sidebar | overview | detail ]` three abreast; **only the sidebar
    collapses**.
  - Mobile: the sidebar is an overlay drawer summoned by the collapse button; the
    overview is full-screen and the detail slides over from the right with a back
    button.

### 1e. Immersive / full-bleed apps  (no sidebar at all)

Some apps are **not** content-and-navigation apps — their primary surface *is*
the window. A **camera** viewfinder, a **video/media player**, **maps**, a
**game**: the live surface must fill the window edge-to-edge, uninterrupted.
These apps are a distinct class and **do not get the content-app sidebar** (§1a)
or the desktop overview/detail split (§1b).

- **Full-bleed surface.** The viewfinder / video / map fills the window to every
  edge. Do **not** dock a rail, sidebar, or app-title header beside it, and do
  **not** draw the app's own logo/brand mark — the OS owns window chrome
  (see "OS owns window chrome").
- **Controls float over the surface.** Capture/playback controls are translucent
  glass overlays anchored to the edges (top strip for capture settings,
  bottom-centre for the primary action + mode switch, corners for
  secondary actions), kept legible with a soft scrim rather than an opaque panel.
  When idle they may fade; they never box the surface in.
- **Secondary screens are pushes or sheets, not rail destinations.** A library /
  gallery is **pushed** over the surface with a **back button top-left**
  (§1b push navigation); settings and pickers are **overlay sheets** with a
  scrim and a close affordance. They are summoned *from* the surface (a
  thumbnail, a gear) and dismissed back *to* it — they are never persistent
  navigation chrome.
- **A mode switch is not a sidebar.** If the app has peer modes (photo / video /
  scan; play / browse), express them as a compact bottom-centre segmented
  switcher over the surface — not as a vertical list of destinations.

> How to tell the classes apart: if the window's job is to *show a live/edge-to-
> edge surface the user acts through*, it is immersive (§1e). If the window's job
> is to *navigate between and read destinations/content*, it is a content app
> (§1a/§1b). A camera with a Camera/Gallery/Settings rail down the left is the
> canonical mistake — it turns an immersive app into a content app and crowds the
> viewfinder.

---

## 2. Expandable rich-field cards

A per-item card hosting a rich field (e.g. a graph formula in the calculator)
**grows in height to fit its content, up to a maximum height**. Beyond that
maximum it becomes **vertically scrollable within the card**.

- Never **clip** content behind the card's edges to "hide" overflow — that is
  not a solution.
- Overflow detection must be **reliable**: any content taller than the card's max
  (e.g. a fraction, a 3D surface's multi-line default) must be recognised as
  overflowing and get scrolling.

---

## 3. Text editing (everywhere)

Editable text is a **real CRDT-backed editor**, via the shared `ui` caret /
selection core — not a per-app buffer:

- Undo / redo (Ctrl+Z / Ctrl+Y).
- Selection: shift+arrows, drag-select, word-wise (ctrl+arrows), select-all.
- Clipboard: Ctrl+C / X / V.
- Collaborative multi-cursor.

If an app hand-rolls text editing, it will get these wrong — use the shared core.

---

## 4. Chrome & copy

- No marketing slop in placeholders or badges ("Search privacy-respecting
  videos" → "Search"; no "no ads" pill).
- No internal / implementation labels in user-facing chrome (e.g. "CRDT-synced",
  "Stored in the document CRDT", a leaked `/var/workspace` path).
- Settings toggles live in **Settings**, not duplicated as always-on top-bar
  chips.
- Never fabricate content and present it as real (no synthesized "video feed"
  gradients, no invented data on an empty state).

---

## 5. System integration (file access, host services)

- **File open/save goes through the XDG Desktop Portal**
  (`org.freedesktop.portal.FileChooser`, via `ashpd`) — an app **never draws its
  own file picker**. The *host desktop* provides the picker: on GNOME the user
  sees GNOME's file chooser; on qOS the **compositor implements the portal
  backend** and renders qOS's picker (which is also where the per-cluster
  sandbox / file-access prompt lives — the portal is the security boundary). A
  bespoke in-app "OpenDialog", or an app-drawn `ui::FilePicker` used for OS file
  open/save, is a bug. (`ui::FilePicker` exists only for the compositor's own
  portal-backend rendering, or non-filesystem in-cluster pickers.)
- Prefer host/portal services generally (screenshot, sharing, notifications)
  over reinventing them in-app, so a qOS app behaves like a native app on
  whatever desktop it runs.

## 6. Performance (felt responsiveness)

- **No idle redraw spin.** An app at rest requests **no** frames; animate only
  while something is genuinely animating. (A caret blink or a 1 Hz clock is not a
  license to re-render the whole scene every vsync.)
- **No unbounded per-frame work.** CRDT writes are **diffed** — never rewrite the
  whole document (or delete+reinsert a whole text) per keystroke; that is
  O(n²) tombstone churn and it freezes the app. Failed network work
  (e.g. a map tile) is **negative-cached with backoff**, never retried every
  frame.
- The compute path (event → laid-out frame) should stay well under one frame
  (~16 ms); if an app feels slow, measure event/layout/draw/present separately
  before blaming app code.

---

## 7. Visual craft & polish (the bar for *every* app)

Working, honest, and consistent is the floor, not the goal. **Every app ships at
a "genuinely nice to look at and use" bar** — the same craft whether it is chat,
the calculator, or a system pane. An app that is functionally complete but looks
cramped, unbalanced, or unfinished is **not done**. Grade every screen against
this section the way you grade correctness against the tests.

- **Layout breathes.** Deliberate spacing on the `Theme` scale; content is not
  jammed edge-to-edge, controls are not scattered. A clear reading order and a
  single primary action per screen. Groups are separated by space and hierarchy,
  not by hairlines and boxes-within-boxes.
- **Real typographic hierarchy.** Size / weight / color establish
  title → section → body → caption; digits that line up use tabular figures. No
  wall of same-size text; no illegible low-contrast label on a colored fill
  (the permissions-pane and camera regressions were exactly this).
- **Content reflows by size, not just shrinks.** Honor the responsive rule (§1):
  card grids re-column, participant/media grids re-tile by count, panels become
  separate screens when they no longer fit — measured against window *area*, and
  **recomputed on resize**, never laid out once.
- **State reads at a glance.** Selection, focus, active/live, speaking, syncing,
  error — each has a *visible form* (an emphasis, a ring, a pill, a severity
  stripe), not only a text label. Empty states are composed and inviting (a real
  zero-state, per §4 — never fabricated content), loading and error states are
  designed, not raw.
- **Motion is purposeful and quiet.** Transitions clarify a change (a panel
  sliding in, a value settling); they never gate interaction or spin at idle
  (§6). Reduced-motion is honored.
- **Interactive things look interactive** and give feedback: hover, press, focus,
  and disabled are all distinct; a control that does nothing yet is a defect
  (§ functionality), and a control that *looks* dead is a craft defect.
- **Theme-driven, pixel-clean.** Every color and metric comes from the `Theme`
  (§ toolkit) so light/dark and OS accent both look intentional; no hard-coded
  colors, no 1px seams from mismatched fills, no clipped glyphs or overlapping
  text.

The proof is visual, not a green build: `--shots` the primary screens (populated,
empty, and a mid-interaction state), **open the PNGs and look critically** — if a
screen would embarrass you next to a best-in-class native app, it is not done.
When a polish pattern recurs (a media/participant grid, a stat tile, a huddle
overlay, a segmented control), promote it to a `ui` primitive so the *nice*
version is the default and no app has to re-earn it.

---

## 8. Never hand-roll an interactive primitive

**Use the shared `ui` widget for every interactive primitive — text field,
button, dialog, dropdown, checkbox, toggle, slider, tabs, menu, tooltip,
popover. Never paint your own.** A "clickable rect + a manual hit-test" or a
"`String` field you `push`/`pop` on raw key events" is not a lightweight
shortcut — it is a broken control that silently drops the behaviours the shared
widget already gets right:

- **Text selection** — shift+arrows, drag-select, word-wise, select-all
  (a hand-rolled `String` field has no selection at all).
- **Clipboard** — Ctrl+C / X / V, and paste from other apps.
- **Keyboard** — a painted button cannot be focused or activated with
  Space/Enter; a hand-rolled field has no caret navigation or IME.
- **Focus** — the 2D directional focus ring (`App::focusable_order`) skips
  anything that is not a real widget, so hand-rolled controls are unreachable by
  keyboard and invisible to focus-follow.
- **Accessibility** — the AT-SPI / a11y tree is built from the widget tree; a
  painted rect has no role, name, or state, so it does not exist to a screen
  reader.
- **Theming** — shared widgets read every color and metric from the `Theme`
  (hover/press/focus/disabled included), so light/dark, high-contrast, and OS
  accent all track automatically. A hand-rolled control with `Color::rgb(...)`
  literals breaks all three (see §7's "Theme-driven" clause).

This generalises §3 (text editing) to **all** interactive primitives. Concretely:

- Editable field → `TextInput` (or `SearchField` / `NumberInput` /
  `EditableLabel`). Never keep a per-app cursor index and splice characters into
  a `String` on `Key::Character` / `Backspace`.
- Clickable → `Button`. Never `draw_*_button` + a `contains()` hit-test.
- Confirm / prompt / sheet → `ModalDialog` (scrim, focus capture, Esc, a11y are
  built in). Never paint your own overlay + scrim rect.
- Select / choice → `Dropdown`; on/off → `Toggle` / `Checkbox`; range →
  `Slider` / `NumberInput`; peer views → `TabBar` / `ToggleGroup`; right-click →
  `ContextMenu`; hover help → `tooltip`; anchored transient → `Popover`.

**If the shared widget is missing a capability you need, extend the widget — do
not fork it.** Add the feature to the one `ui` primitive so every app gets it and
the accessible / themed / clipboard-correct version stays the default. Forking a
private copy re-earns every bug above and guarantees drift. The only sanctioned
"hand-rolled" text surfaces are genuine document engines whose buffer *is* the
app (the terminal grid, a code/rich-text/cell editor) — and even those must route
their **side** fields (find bar, rename box, cell entry, command line) through the
shared widgets, and are themselves candidates for a shared editor primitive
(§ toolkit gaps) rather than N private forks.

> Enforcement: a new painted control is a toolkit gap, not an app feature. Grep
> your diff for the patterns in §9.13 plus `draw_*_button` and
> `Key::Character => *.push` — each hit is either a shared-widget call you
> skipped or a widget you should be extending.

**Grep for the spelling the code actually uses.** This rule used to say "grep
for `Color::rgb`", and that pattern is why hard-coded colour accumulated under
it for months: `Color::rgb8(…)` is the *old* peniko constructor and this tree
does not call it. The one automated check that implemented the rule
(`ui/tests/design_token_audit.rs`) was green over 125 literal
`Color::from_rgb8(…)` colours in `ui/src` alone, because it was looking for a
string that appears nowhere. Two more spellings were invisible for the same
reason:

- **`Color::WHITE` / `Color::BLACK`** — most are a defensible ink on an accent
  fill or a scrim; the defect is that nothing was looking, so nobody ever had to
  say which.
- **`surface_white_*` / `border_white_*`** — the values are no longer literally
  white: `Colors::themed` washes them toward the contrasting extreme, so they
  flip to black on a light palette (the fix landed after every unselected filter
  chip lost its pill at 1.01:1 on light). What survives is the reason §9.5
  deprecates them anyway — the *name* still says white, which is how a reader
  picks one while reasoning about the wrong colour, and the tier is a
  **declared** alpha that never measures itself against the ground it lands on.
  `Colors::lift()` and `Colors::hairline_on()` do measure.

`design_token_audit.rs` now covers all three, ratcheted against
`ui/tests/design_token_baseline.txt`: a file may never gain one, and a file that
sheds one lowers its record in the same commit. The count only goes down. That
file, not this document, is the authority on how many are left.

---

## 9. The shell chrome design language

Sections 1–8 govern **apps**. This section governs the **shell** — the surfaces
the OS itself paints around and between apps: the top bar, window decorations
and tab pills, the workspace disc, the strip indicator, quick settings, the
command palette, and the collaborator invite/roster surfaces.

It exists because those surfaces drifted into separate design systems. Each was
transliterated independently from its own mockup component, carrying that
mockup's literal Tailwind values inward — `bg-black/80` for the bar,
`bg-slate-900/80` for quick settings, `rounded-3xl` here and `rounded` there,
`text-green-400` for a CPU label. The toolkit's token vocabulary was already
good, and the chrome simply did not read from it. The result was four surfaces
that share a screen without sharing a language.

The rule that prevents the drift returning is the same one §8 applies to apps:

> **Shell chrome does not invent appearance. It selects a role from this
> section, and the role resolves to tokens.** A literal colour, a literal
> radius, or a bespoke duration in a chrome draw path is a defect, the same way
> a hand-rolled text field is.

### 9.1 Roles, not pixels

Every chrome surface is exactly one of six **roles**. Pick the role first; the
role fixes radius, surface, border, elevation and motion together, so two
surfaces of the same role are automatically consistent.

| Role | What it is | Radius | Surface | Border | Elevation |
|---|---|---|---|---|---|
| **Ground** | the desktop behind everything | — | `surfaces.base` | — | none |
| **Bar** | docked chrome fixed to a screen edge (top bar) | — (edge to edge) | `chrome::shell::panel_ground` (opaque) | bottom hairline only | `e1`, downward only |
| **Panel** | a floating surface that opens over content (quick settings, palette, popovers, sheets) | `radii.xl` (24) | `chrome::shell::panel_ground` (opaque) | full hairline | `e4` |
| **Control** | something inside a Bar or Panel that you press (action tile, list row, slider, field) | `radii.lg` (12) | selection tiers (§9.3) | per tier | none |
| **Selector** | one of a set you choose between (tab pill, tray pill, workspace chip, strip segment, scope chip) | `radii.full` | selection tiers (§9.3) | per tier | none |
| **Object** | a thing that holds app content (window card, tile) | `radii.xl` (24) | `surfaces.window_card` | `Colors::window_edge()` | `e3`; `e4` while dragged |

Two consequences worth stating outright, because both were being violated:

- **`radii.sm` (4) and `radii.md` (8) are not for interactive bodies.** They are
  for details drawn *inside* a control — a progress track, a badge, a sparkline
  clip. A pressable thing is `radii.lg` if it is a Control and `radii.full` if
  it is a Selector, never a 4 px rectangle.
- **16 px is not on the scale.** The radius scale is 4 / 8 / 12 / 24 / full and
  deliberately omits 16 (see `tokens::radii`). A literal `16.0` in a chrome
  draw path is off-scale by construction.

### 9.2 Surfaces come from the semantic tier, never from a ramp slot

Chrome reads `colors.surfaces.*`, which every theme defines for itself. It does
**not** reach into `colors.neutral[n]`. A ramp slot is a colour; a surface token
is a *promise about contrast* that each theme keeps in its own way, and only the
latter survives a theme the author never saw.

- Bar and Panel: `surfaces.overlay`.
- Ground: `surfaces.base`. Object: `surfaces.window_card`.
- Scrim behind a modal Panel: `surfaces.scrim`, faded in with the panel.

One shared constant, `CHROME_SURFACE_ALPHA`, sets the opacity of every Bar and
Panel, so the whole chrome is one material. Per-surface alpha literals are what
made quick settings read as a different substance from the bar it hangs off.

**That material is opaque, and chrome does not fake glass.** Translucency is
only worth its cost when the backdrop is blurred, and this renderer cannot blur
a backdrop — vello can blur a *shape*, which is what a drop shadow is made of,
but there is no backdrop filter short of rendering the scene to a texture and
convolving it. Unblurred, a translucent panel simply adds a fraction of whatever
is behind it to itself: not glass, but a loss of contrast that changes with what
the user happens to have open. Measured at the old 0.80, one quick-settings
panel's own body ran `#ffffff` over a document and `#f7f7f7` over the desktop —
one flat surface reading as two, with a seam across the middle of it.

**A Panel separates itself by value and by edge.** `panel_ground` walks the
declared overlay tier away from *both* the desktop and the window card until it
clears each of them, because a Panel floats over app content and a Bar does not,
and the two are one material. `panel_border` supplies the edge. Get those right
and elevation is a finishing touch; get them wrong and the shadow is asked to do
their job, which is how it ends up too big to look like a shadow.

### 9.3 Selection is a lift, and only a lift

Every "which one is chosen" state in the shell — active tab, active workspace,
active strip, selected row, chosen scope chip, current power profile — uses the
**same four tiers**:

| State | Fill | Border | Inner glow | Scale |
|---|---|---|---|---|
| idle | transparent | none | none | 1.0 |
| hover | `colors.lift(0.5)` | none | none | 1.0 |
| **active** | `colors.lift(1.0)` | `hairline_on(ground, 26)` | `hairline_on(ground, 51)` | 1.0 |
| pressed | as active | as active | as active | 0.97 |

This is `ui::chrome::selection_tier` (promoted out of `widget::tabs`, where it
was `pub(crate)` and therefore unreachable from the shell — which is precisely
why the shell grew its own incompatible answers). Call it; do not restate it.

**One caveat the tier cannot absorb for you.** `Colors::lift` interpolates from
`surfaces.base` toward `surfaces.raised`, and where those two are close it
falls back to a derived step against the ground. That fallback is by design and
is what the default dark palette already uses. But it is *weak*, and on the
high-contrast palette — where `base` and `raised` are both pure black — it is
weaker than a fixed floor. A control that gives its unselected state a floor of
its own (because it has no row around it to supply context, as the strip ribbon
does) can therefore end up painting the **selected** state less prominently
than the unselected one. If a control needs its own floor, derive all of its
states from one ordered ramp against the ground and assert the ordering per
theme; do not mix a floor with the tier.

Why lift rather than an accent fill:

- `Colors::lift` measures its move against the chrome's *own ground* by relative
  luminance, so it lifts correctly on dark, light and high-contrast without a
  per-theme branch. A fixed white wash only lifts over a dark ground, and a
  fixed accent fill collides with whatever the accent is being used for.
- It leaves the **accent free to mean one thing**: see §9.4.
- Hover and active are the same *kind* of change at two strengths, so the
  control reads as one continuous affordance rather than two unrelated looks.

An active thing never gets *more* prominent on hover. The contrast gain belongs
to the inactive things brightening toward the active tier.

**Elevation and selection are different vocabularies and must not be mixed:**

> **A shadow means "off the surface". A lift means "chosen on the surface".**

A selected pill casts no shadow — it is not floating, it is picked. A dragged
object casts `e4` — it is genuinely lifted and will land somewhere. Rows of
idle pills each laying down a soft shadow is what made the tab strip read as
muddy rather than as chrome.

**A shadow is the shape of the thing casting it, and comes from a token.** One
painter, `surface::shadow(scene, transform, rect, radius, token)`: it blurs the
surface's *own rounded outline*, so a shadow is equally dense the whole way
round and its corners follow the surface's corners. Pass the radius. A surface
that hands its shadow a square field gets a box around a rounded panel, with
corners several times lighter than the flat edges — "boxy and uneven" is what
that looks like from the outside.

The token is the whole description: `offset_y`, `blur`, and a `color` whose
**alpha is the strength and is honoured**. Do not hand a shadow a bare `Color` —
a caller given a colour and no offset or blur has to invent the other two, which
is how four surfaces in this toolkit each grew a different shadow, three of them
a solid rounded rect nudged down a few pixels and called soft. `offset_y` stays
a quarter of `blur` across the ramp; a larger ratio slides the falloff onto the
bottom edge and the surface stops looking lifted and starts looking like it has
a dark bar under it.

`soft_shadow` is the older sprite painter and is **not** for drop shadows any
more. It survives for effects with no outline to follow — the wheel disc's
bloom, a glow around a dragged card.

### 9.4 Accent means live, not selected

`colors.accent` is reserved for **state the user did not choose and should
notice**: recording, screen-sharing, a peer joining, a live collaborative
session, an unread count, a sync in flight. Selection is a lift (§9.3); focus is
`border_focus`; danger/warning/success are `colors.semantic.*`.

This is the rule that kills per-surface palettes. A CPU label is not "green" —
if a metric strip needs to distinguish its columns, it distinguishes them with
`semantic.success` / `semantic.info` / `semantic.warning`, which every theme
defines and high-contrast redefines. Literal `0x4a,0xde,0x80` is Tailwind
green-400 wearing a costume, and it ignores the user's theme completely.

Per-peer identity colours are the single sanctioned exception: they come from
`ui::presence::user_color`, are derived from the peer id, and are *identity*,
not decoration.

### 9.5 Borders are derived, never declared

Chrome borders call `Colors::hairline_on(ground, alpha)`, which picks white or
black by whichever actually moves against that ground. A declared
`border_standard` stroked onto a translucent Panel does not track the surface
under it and disappears on one theme or glares on another.

- hairline on a control: alpha 26 (`/10`)
- inner glow on an active control: alpha 51 (`/20`)
- separator inside a Panel: alpha 26, inset from the panel edge by
  `spacing.s3`, never running full-bleed into the corner radius

`surface_white_5` / `surface_white_10` / `border_white_10` were literal white at
low alpha in **every** theme, including light ones where a white veneer over a
white panel is invisible — the faint tier measured 1.01:1 on light, which is why
every unselected filter chip there read as bare text. `Colors::themed` now
washes them toward the contrasting extreme instead, so they flip to black on a
light palette and a tier means the same thing on both sides of the line.

They stay **deprecated in chrome** regardless, for the half that derivation
cannot fix: the name says white while the value is a veneer, and the strength is
*declared* rather than measured against the ground the surface actually lands
on. `lift` and `hairline_on` take that measurement, so a hairline stays a
hairline on a palette their author never saw. Reach for those.

### 9.6 Type and density

Chrome type is a **compressed** version of the app scale — chrome labels a thing,
it does not host prose.

| Use | Size | Weight | Colour |
|---|---|---|---|
| Panel title | `scale.base` (14) | `semibold` | `text_primary` |
| Section header inside a Panel | `scale.xs` (10) | `medium` | `text_tertiary` |
| Control label | `scale.sm` (12) | `medium` | `text_primary` |
| Control sub-line / value | `scale.sm` (12) | `regular` | `text_dim` |
| Bar label | `scale.sm` (12) | `medium` | `text_primary` |
| Numeric readout | `scale.sm` (12) | `semibold`, tabular | `text_primary` |

Density, so two panels built by two people come out the same size:

- Bar height 40; Bar control height 28.
- Panel padding `spacing.s4` (16); gap between Panel sections `spacing.s4`.
- Panel row / Control height 44; Selector height 28.
- Minimum pointer target 32×32 — a 20 px glyph gets a 32 px hit rect.
- Panel width: 320 for a utility Panel (quick settings), 360 for a roster or
  list Panel, 560 for a sheet with a form in it.

### 9.7 Motion

All chrome animation reads `tokens.motion.durations` and nothing else:

- `fast` (130 ms) — hover, press, chip toggle, drop-zone hint, pip elongation.
- `base` (210 ms) — a Panel opening or closing, a tab in/out, a strip change,
  a reflow.
- `slow` (400 ms) — whole-view changes: overview, workspace switch, window
  spawn.

Panels open with the **one** entrance: opacity 0→1, y −10→0, scale 0.95→1,
`Curve::EaseOut` over `base`, and reverse to close. Bespoke per-surface
constants (the shell previously carried eight near-but-unequal ones) are a
defect. Reduced-motion collapses every duration to 0 and keeps the end state.

### 9.8 Chrome may not block the frame

Chrome is the OS's own UI, so a stall in it looks like the machine is broken
rather than like an app being slow. §6 applies with one extra, non-negotiable
rule:

> **No chrome draw path performs I/O.** No process spawn, no D-Bus round trip,
> no filesystem scan, no network call — not on open, not on hover, not per
> frame.

State that comes from the OS is read on a worker and delivered to the frame
asynchronously. The surface opens *immediately* with the last known values and
fills in when the read lands; a value that has never been read shows its own
"reading" state, never an empty panel and never a delayed one.

Reads are also **tiered by what the surface actually shows**. A panel fetches
what is on screen now, and defers what is only visible after a drill-in. Quick
settings was the worked example: opening it ran seven blocking shell-outs, one
of which (`nmcli device wifi list`) forces a Wi-Fi scan — 4.5 s of frozen
compositor to populate a list the user had not yet asked to see.

Every shell-out gets a timeout. An OS tool that hangs must degrade to a stale
value, never to a hung desktop.

### 9.9 The shell obeys §8

Everything in §8 applies to chrome, and chrome has been the worst offender: the
quick-settings sliders, its action tiles and its list rows are painted rects
with manual hit-tests, which is why they have no keyboard path, no focus ring
and no a11y node. New chrome uses `ui::widget::*`; a chrome control that the
toolkit does not yet have is a toolkit gap to fill, not a rect to paint.

### 9.10 Each level of the model owns one part of the screen

The model is `Workspace → Strip → WindowGroup → Tab`: Niri-like scrollable
strips inside collaborative workspaces. Three of those four levels are things
the user moves between, and each gets a fixed, non-overlapping part of the
screen so the control for a level is always in the same place:

- **Left edge of the content — which workspace.** The workspace disc. The
  outermost level, and the one changed least often.
- **Right edge of the content — which strip.** The strip ribbon (§9.11), within
  the workspace already in view.
- **Horizontal, inside the content — which group, which tab.** Groups scroll
  horizontally within a strip; tabs tile within a group. Nothing else may claim
  horizontal scrolling of the content area.

Keep these separate. Folding strip switching into the disc would mean one
control moving between two different levels of the model, and nothing on the
control would tell the user which of the two a given drag was about to do.
Workspace switching already owns Ctrl/Super + Up/Down, so a strip binding must
be distinct from it rather than a variant of it.

### 9.11 A switcher must answer "which one am I on" without being asked

Any control that moves between N things shows, at rest and without interaction:

1. **how many** there are,
2. **which one** is current,
3. **which direction** the others lie in.

A switcher that only appears once you already know the gesture is not a
switcher; it is a shortcut for people who read the source. Strips failed all
three — a workspace could hold four strips with nothing on screen indicating
that a second one existed.

The **strip ribbon** is the answer here: a slim vertical ribbon on the
content's right edge, one segment per strip, ordered to match the order the
strips are stacked in. The focused segment takes the active tier from §9.3 and
elongates; the others sit at the idle tier and thicken on hover. It is
`radii.full` (Selector role), it is click- and keyboard-addressable, and it
animates at `base`.

Note what it indicates. Every strip in a workspace is rendered at once, sharing
the content height between them, so the ribbon does not say "this is the strip
you can see" — it says **which strip has focus**, which is the thing that
decides where a new window lands and what the keyboard is aimed at, and which
had no representation on screen at all. It idles quiet enough to ignore and is
never absent, so a second strip is discoverable by looking rather than by being
told.

### 9.12 Collaboration chrome is chrome

Inviting a peer, showing who is here, and joining someone else's workspace are
**shell** surfaces and take shell roles — a Panel with Control rows and a
Selector for scope, not a bespoke dialog per entry point. Specifically:

- Presence is not a toast that vanishes. Who is in this workspace is legible
  from a persistent roster in the Bar; a toast may *announce* a change but never
  be the only place the fact is available.
- Every asynchronous collaboration action has a **designed pending, failure and
  empty state**. "Waiting for peer", "peer refused", "no peers found" are
  states, not log lines.
- A peer address the user has to type is a `TextInput` with validation and a
  paste path, never a painted field, and never the only way in — discovery and
  a share code are the primary routes.
- Ordinary prose `TextInput`s spell-check by default. Fields whose contents are
  structured opt out with semantic intent: `TextInputKind::Search`, `Name`,
  `Username`, `Email`, `Url`, `Path`, `Code`, `Number`, or `Password`. Shared
  search, number, code, secret, and editable-name widgets set that intent for
  their callers automatically. `with_spellcheck(true)` explicitly opts a
  structured field back in. Set `with_spell_language` when a field's content
  language differs from the session locale. High-confidence autocorrection
  happens only at a word boundary, respects Settings and personal vocabulary,
  and must be immediately reversible with Backspace.
- Per-peer colour comes from `presence::user_color` and is used consistently in
  every surface that mentions that peer (roster, cursor, tab badge, invite row).

### 9.13 Enforcement

Grep a chrome diff for these; each hit is a role you skipped:

- `Color::from_rgb8(` / `Color::from_rgba8(` / `Color::rgb8(` / literal hex in a
  draw path — match **both** constructor spellings, not just the one you happen
  to have typed lately (see §8: grepping only for `Color::rgb` is how these
  accumulated under a green check)
- `Color::WHITE` / `Color::BLACK` — a colour is a colour even when it has a
  name; the theme has an on-accent ink and a scrim token
- `surface_white_` / `border_white_` (any suffix) — a declared strength, never
  measured against its ground (§9.5)
- `colors.neutral[` in chrome (use `surfaces.*`)
- a numeric literal passed as a corner radius
- `Duration::from_millis(` with a literal in an animation
- a `Command::new` / blocking read reachable from `draw`

One regex covering the colour half, so it can be pasted rather than remembered:

```bash
git diff -U0 | grep -nE 'Color::(from_)?rgba?8\(|Color::(WHITE|BLACK)|(surface|border)_white_'
```

The first three are enforced for `ui/src` by
`ui/tests/design_token_audit.rs::theme_escape_hatches_do_not_grow`, ratcheted
against `ui/tests/design_token_baseline.txt` so the count can only fall. Apps
are not scanned yet; until they are, the grep above is what stands between an
app and a colour the user's theme cannot reach.

---

*This document is versioned with the toolkit. When a guideline here becomes a
`ui` primitive, link the primitive from the relevant section so the rule and its
enforcement stay together.*
