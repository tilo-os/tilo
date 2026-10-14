# Tilo Human Interface Guidelines

## Purpose & philosophy

The toolkit should make balanced, adaptive layouts easy to author. Shared
patterns encode useful defaults for spacing, hierarchy, readable width, and
interaction. When the same correction recurs across apps, improve the shared
pattern so applications inherit the better default.

Automatic, guided, and fully manual composition are all supported. An application
may position standard controls with authored dimensions and responsive rules,
including on ordinary forms and app chrome, without exception approval. Shared
control behavior, accessible names, reachable actions, and actual clipping still
matter. A custom gap or rectangle alone does not establish poor quality.

Use [Form composition](FORM_COMPOSITION.md) for the automatic/guided path and
[Manual layout](MANUAL_LAYOUT.md) for shared child measurement, exact placement,
mixed regions, and existing inspection tools.

---

## 1. Navigation & layout: sidebars vs. content panels

There are **two distinct kinds of panel**. Do not conflate them. A single
generic "overlay drawer with a scrim, everywhere" is wrong — it turns *content*
into a menu and clutters small screens.

**One shell does both.** Every window with navigation is a
`ui::widget::NavShell`: `.sidebar(..)` for §1a, `.overview(..)` and
`.content(..)` for §1b, `.inspector(..)` for the properties of the
selection at the trailing edge. Its sidebar lists destinations with a
`ui::widget::NavList`. The shell owns the header row, the sidebar toggle,
the widths, the breakpoints, the drawer, the push stack, the regions and the
motion; the app passes none of them. A painted app that cannot host widgets
uses the headless `ui::widget::SidebarDrawer` with the same metrics and
paints its rows with `ui::chrome::paint_nav_row`. No other sidebar, split or
row painter exists.

**The header row.** Windows have no title bar. The shell draws one header
row (`metrics.header_height`, 56) across the window and each column's header
sits in it — `.sidebar_header(title, actions)`, `.overview_header(..)`,
`.content_header(..)`, `.inspector_header(..)` — so the row reads as one
unified toolbar: a `Heading` title in sentence case after the shell's
leading button, the column's actions at its trailing end, and one hairline
under the content columns. An app never draws a band of its own above a
column. The title starts at the content gutter (`rhythm.group`) or after
the shell's leading button; an optional count follows it as a quiet
`Caption.numeric` pill; the actions start after the title's measured
width. Where they do not fit, labelled buttons show their icons alone,
then the rest move into a trailing More menu. A path bar or a mode
switcher with no title is the header's leading fill
(`content_header_leading`). An app's own page stack (browse history, a
drill-down) gets the shell's Back chip (`with_back`), the same chip a
narrow window's push stack uses. A painted app lays the same row out with
`chrome::HeaderRowFace` and takes the window's History control through
`widget::WindowHeader`; there is one header-row geometry.

**History is the window's, at the trailing edge of the header row.** Where
the app has History, the host gives the shell its History control: one Plain
icon button with `Icon::History`, named and tooltipped "History" with its
shortcut (`HostShortcut::History`), in the window's top-trailing corner after
the column's actions and the inspector toggle. While History shows it reads
as on (the selection lift) and closes History; the History pane or bar shows
the mode itself. It is the same door as the shortcut, the palette and agents.
An app never adds a History button, text or glyph, of its own. A host whose
root has no header slot keeps the shortcut, palette and resource action;
it never inserts a second toolbar above the app. Where the
host's chrome presents History (the Tilo shell, a native menu bar) the row
shows none.

### 1a. Genuine sidebar / menu  (pure navigation)

Chrome that lists destinations. Examples: a calendar's list of *calendars*; an
email client's accounts + folders.

- **Desktop / wide:** solid, **docked** beside the content on the
  `Region::Sidebar` tier, at `metrics.sidebar_width` (280). Shown and hidden
  by the **sidebar toggle** — a Plain icon button with the `Icon::PanelLeft`
  glyph that the shell draws at the sidebar's top-leading corner, and at the
  leading edge of the next column's header while the sidebar is hidden —
  **never an `X`**, never a hamburger. One system shortcut toggles it in
  every app (`HostShortcut::Sidebar`, Ctrl+Shift+L / ⌘⇧L), and the toggle
  publishes "Show sidebar" / "Hide sidebar" to the palette and to agents.
  Dragging the sidebar's trailing edge resizes it between
  `sidebar_min_width` and `sidebar_max_width`; dragging it below the minimum
  hides it. The person's choice is remembered per window size class.
- **Mobile / narrow:** below the width where the content beside it would be
  squeezed (`sidebar_width` + the content's minimum), it **stays a genuine
  sidebar** as a **floating drawer** over the content — the floating
  material, `radii.panel`, the floating shadow, `surfaces.scrim` behind —
  summoned and dismissed by the same toggle, a scrim press or Esc. It opens
  under the header row, which stays live (the toggle does not move, History
  stays reachable); the columns behind it stay in place under the scrim
  and take no input. Choosing
  a destination in it closes it. It remains a menu; it does **not** turn
  into a full-screen page. (`NavMode::Push` is the exception for a list of
  *categories* such as Settings: there the sidebar is the first screen of
  the push stack and choosing pushes the content. `NavMode::Docked` is for a
  compact icon rail, `NavList::rail`, which fits beside content at every
  width.)
- A docked sidebar is never kept beside columns that no longer fit: at 800 px
  a window with a sidebar, an overview and a detail shows the drawer.

### 1b. Content panels  (overview + detail)

Panels that are themselves content the user reads and acts on. Examples: a
calendar's month/week grid (overview) + the event (detail); an email message
list (overview) + the message (detail).

- **Desktop / wide:** shown **side by side** (alongside the genuine sidebar, if
  any). Only the sidebar collapses; the content panels stay abreast.
- **Mobile / narrow:** when the overview and the detail no longer fit side by
  side (`overview_width` + `detail_min_width`), show **only one of
  {overview, detail} at a time**. The overview is the default
  **full-screen** view; selecting an entry **slides the detail in from the
  right**, over the overview, with a **Back button top-left** in the header
  row. Back (or Esc) returns to the overview. This is **push navigation** —
  never a scrim-overlay of content over content. A `NavList` in the overview
  pushes by itself; any other overview pushes with `shell.show(NavPane::Content)`
  or a `LocalState<NavPane>` bound with `with_pane_state`.
- The sidebar collapses first, then the split: at 800 px a mail window shows
  the drawer and keeps the list beside the message; at 560 px it pushes.

### 1c. How to decide which a panel is

- Is it *pure navigation* — a list of destinations / accounts / calendars? →
  **genuine sidebar** (§1a).
- Is it *content the user reads or acts on* — a list of messages, events, or
  cities-with-info? → **content panel** (§1b), **even if it sits on the left and
  looks sidebar-ish**. Where the content panel has an icon form (places as
  a rail), give it `overview_rail`: the window narrows to rail and detail
  before it pushes.
- Is it *a stack of pages inside the content* — a dashboard drilling into a
  queue, browse history? → the content's own page stack, with the shell's
  Back (`with_back`), never a hand-placed Back button.
- Is it *properties of the selection*? → the **inspector**; it docks while
  the content keeps its room, floats as a drawer otherwise, and keeps the
  person's docked or closed choice per window size class.

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
- **Controls float over the surface.** Capture/playback controls sit on the
  floating material (`Material::Floating`, §9.2) over the media itself,
  anchored to the edges (top strip for capture settings, bottom-centre for
  the primary action + mode switch, corners for secondary actions). Never
  unblurred translucency. When idle they may fade; they never box the
  surface in.
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

Editable text is a **CRDT-backed editor**, via the shared `ui` caret /
selection core:

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
- Never present fabricated content as live data (no synthesized "video feed"
  gradients, no invented data on an empty state).

### 4a. Search, filter and find

Search belongs to the platform. An app never draws a search field, a result
list or its own palette. There are three different jobs, and each has one
answer:

| The person wants to… | The app provides | The person sees |
|---|---|---|
| **find something the app holds** (a message, a video, a note) | a `SearchProvider` per findable kind, rows that say what a hit is (`SearchRow`), scopes (`ScopeDecl`), facets (`Facet`) | the one platform palette, opened by Ctrl+K, `/`, or the app's magnifier button (`search::search_button`) |
| **narrow the list already on screen** | `Filterable` on the list | `SearchField::filter()` in the navigation shell's filter slot (or `Filtered`), with a "3 of 120" count; Enter or "Search everywhere" escalates to the platform search |
| **find in the document on screen** | `Findable` on the document view | the `FindBar` band at the top of the content region (`FindScope`), opened by Ctrl+F |

- **The launcher is a button, not a field.** The toolbar's magnifier is
  `search::search_button(SearchRequest::this_app())`; a part of the app gets
  its own (`SearchRequest::within(ScopeDecl::new(anchor, "Inbox"))`, the scope
  pill reading "Only Inbox"). A pill that looks like a field but opens the
  palette is gone.
- **Opening search is an action.** Every opener — the button, `/`, Ctrl+K,
  Enter in a filter, an agent — invokes the host's `tilo.host:search` `open`
  action. A standalone window opens its palette; inside the shell the shell
  opens the system palette scoped to the app's tab. The app does the same
  thing either way.
- **A hit opens the app at that item.** A framework app shows the item in
  `ViewCx::on_open`; the same handler answers the resource's `open` action,
  so an agent opens the app where a person's search hit would.
- **Slow sources do not block.** A provider answering from a server returns
  what it has with `SearchPage::pending` and the rest from `poll_page`; the
  palette appends it under a "Searching…" row.
- **A filter field is a control.** Regular control height, `radii.control`,
  the raised field fill, the person's own colour as the focus ring, the
  placeholder visible while the field is focused and empty and elided when it
  does not fit, and a reserved slot for the count so text never runs under it.
  Narrow, it is the magnifier icon button until it is used.
- **Find marks spans, not blocks.** Every match is painted with
  `paint_find_match` over the matched glyphs; the current one is stronger and
  outlined. The bar shows "n of m", Match case and Whole word, previous/next,
  and replace or a text/hex mode when the view supports them.

---

## 5. System integration (file access, host services)

- **File open/save goes through the XDG Desktop Portal**
  (`org.freedesktop.portal.FileChooser`, via `ashpd`) — an app **never draws its
  own file picker**. The *host desktop* provides the picker: on GNOME the user
  sees GNOME's file chooser; on Tilo the **compositor implements the portal
  backend** and renders Tilo's picker (which is also where the per-cluster
  sandbox / file-access prompt lives — the portal is the security boundary). A
  bespoke in-app "OpenDialog", or an app-drawn `ui::FilePicker` used for OS file
  open/save, is a bug. (`ui::FilePicker` exists only for the compositor's own
  portal-backend rendering, or non-filesystem in-cluster pickers.)
- Prefer host/portal services generally (screenshot, sharing, notifications)
  over reinventing them in-app, so a Tilo app behaves like a native app on
  whatever desktop it runs.

## 6. Performance (felt responsiveness)

- **No idle redraw spin.** An app at rest requests **no** frames; animate only
  while something is animating. (A caret blink or a 1 Hz clock is not a
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

Every screen must meet the spacing, hierarchy, responsive layout, state
visibility, motion, interaction, and theming requirements below. Functional
completeness does not exempt a screen from these requirements.

- **Layout breathes.** Deliberate spacing on the `Theme` scale; content is not
  jammed edge-to-edge, controls are not scattered. A clear reading order and a
  single primary action per screen. Groups are separated by space and hierarchy,
  not by hairlines and boxes-within-boxes.
- **Nothing touches.** Every neighbouring pair keeps a token-step gap (`s1`+). A
  fixed offset that produces a touch, an overlap or a clipped glyph at 1× is a
  defect regardless of how it was authored — the screenshot decides, not the
  byte-identical frame. Content is centred in its container: icon + label
  blocks in tiles, leading icons and trailing controls on the row's centre
  line, text on its measured line box.
- **An edge you draw is derived.** Where an app paints its own card, row or
  pill edge, it comes from `ui::surface::{card, edge_on, separator}` or
  `ui::chrome::panel_border`, measured against the surface it sits on;
  `border_standard` is a rule on `surfaces.base`, never a stroke on `raised`
  or `overlay` (on the dark preset the two are three shades apart and the
  edge vanishes).
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
  stripe), not only a text label. Empty states are composed and inviting (a
  zero-state, per §4 — never fabricated content), loading and error states are
  designed, not raw.
- **Motion is purposeful and quiet.** Transitions clarify a change (a panel
  sliding in, a value settling); they never gate interaction or spin at idle
  (§6). Reduced-motion is honored.
- **Interactive things look interactive** and give feedback: hover, press, focus,
  and disabled are all distinct; a control that does nothing yet is a defect
  (§ functionality), and a control that *looks* dead is a craft defect.
- **Theme-driven, pixel-clean.** Every color and metric comes from the `Theme`
  (§ toolkit) so light/dark and every temperature look intentional; no hard-coded
  colors, no 1px seams from mismatched fills, no clipped glyphs or overlapping
  text.

Validate these requirements with `--shots` for the primary screens (populated,
empty, and a mid-interaction state), then inspect the output. The toolkit's
own review grid is `cargo run -p ui-examples --example ui-shots --
widget_states OUT_DIR`: every control, input, row, navigation and content
composite in rest, hover, pressed, focused, disabled, selected and error, at
each `ControlSize`, and every overlay over a busy page with and without the
blurred material, dark and light. A change to a widget's look is checked
there first. When a pattern
recurs (a media/participant grid, a stat tile, a huddle overlay, a segmented
control), promote it to a `ui` primitive so the shared version is the default.

Promoted so far, each as a retained widget and a `*Face` painter with the
same geometry (`docs/ui-migration.md`, "Content widgets", maps the app code
each replaces):

| Pattern | Primitive |
|---|---|
| A row of a content list (message, contact, request, setting) | `ListRow` / `ListRowFace`, listed in a `Section` |
| Labelled values (details, metadata, fields) | `PropertyList` / `PropertyListFace` |
| A titled group with a count and one action | `Section` + `SectionHeader` |
| Settings rows | `FormSection::grouped()` |
| An object (app, document, comment, connection) | `Card` / `CardFace` in a `CardGrid` |
| A number and what it measures | `StatTile` / `StatTileFace` in a `StatGroup` |
| Covers, thumbnails, album art, photos | `MediaCard` / `MediaCardFace` in a `MediaGrid` (`justified_rows` for photos) |
| A file drop target | `DropZone` / `DropZoneFace` |
| Content still loading | `Skeleton` / `SkeletonFace` |
| A countdown, progress or level ring | `ProgressRing` / `ProgressRingFace` |
| Wizard steps | `Stepper` / `StepperFace` |
| Nothing here yet, or something failed | `EmptyState` (`EmptyState::error`) |
| A person's picture or initials | `Avatar` / `AvatarFace` |
| A whole tile that opens something | `Container::interactive` |
| Tabular data | `DataTable` |
| A mark that means a status or a person | `Tone` |

A card is only for an object: settings groups, form sections and stats sit on
the ground in a `Section`, separated by space.

### 7a. Let content determine ordinary layout

Choose a shared composition by what the region contains:

| Region | Starting point | Decisions supplied by the framework |
|---|---|---|
| Account or settings form | `FormPage`, `Form`, `FormSection`, `Field` | One page gutter, readable width on the leading edge, one label column for the whole form, related-field spacing, measured label placement, help under the label, wrapping, scrolling and pinned actions |
| A page of sections (facts, lists, settings) | `Page` with `head(..)` items and `section(..)`s | The readable column on the leading edge, sections a group apart, scrolling with a persisted offset, actions pinned under it; the shell's header titles it |
| A readable column inside a custom layout | `Container::readable()` | `metrics.readable_width` on the leading edge; `.centered()` only for a page that stands alone (a wizard) |
| A titled group of content | `Section` with a `SectionHeader` | Heading role, count, one trailing action, `related` gap to the items and `sibling` between them |
| A list of things | `ListRow`s in a `Section` | Measured row heights, leading column alignment, inset separators hidden beside the selection, hover actions |
| An object with a title, content and actions | `Card::new(body).with_title(..).with_footer(Actions)` in a `CardGrid` | Content height, consistent card gutter, footer containment and alignment, rows as tall as their tallest card |
| Statistics | `StatTile`s in a `StatGroup` | Label, value and unit roles, value elision, 4 → 2 → 1 reflow |
| Small primary/secondary action group | `Actions` | Measured unspecified button sizes, wrapping and trailing alignment |
| Long command strip | `Toolbar` | Measured plain commands, toggles as lifts, separators only between shown commands, a More menu that keeps every command reachable, keyboard navigation and accessible command bounds |
| Main writing surface | `Scrollable::with_content_min_viewport_height()` | Short auto-height content fills the viewport; long content retains its intrinsic scroll extent |
| Empty list or first-use pane | `EmptyState` | Wrapped title/message, measured action height, centering and decorative shedding |
| Authored canvas or custom arrangement | `Manual` or existing layout primitives | Shared child measurement and ordinary control behavior; the author chooses geometry |

Give primary content the remaining workspace. Empty optional docks should not
reserve a large area before they have useful content. A short query or request
can start compact and retain a visible resize route. Bound prose and forms to a
readable width; keep document, image, map and editing canvases useful at full
workspace width. These are different content needs, so a single global maximum
width or larger global gap cannot produce good balance.

Keep related title, explanation and action rows close enough to read as a group,
with a larger separation before the next group. Avoid repeating a page title
inside its first section or wrapping each grouping in another padded surface.
When commands or panes no longer fit, use wrapping, a labelled disclosure, or
the existing overview/detail navigation. Preserve selected state and the return
route through the transition.

For custom text painting, measure and draw with the same style and width:
`theme.text(role)` (or the `PaintCtx` helpers `text_role`, `measure_role`,
`line_box`, `text_role_in_band`). A role style is already scaled by the UI
and text scale and the renderer paints it as given. Never build a
`TextStyle::new(px)` for UI text and never multiply or divide a size by the
text scale; do not compensate for a scale mismatch with smaller fonts or
offsets. Text that is sized by its content (a canvas, a slide) uses
`with_content_size` instead. See `docs/typography.md`.

An empty state's title and action can exceed a physically small pane even after
decoration is removed. Retained callers can use `EmptyState::into_scrollable()`;
immediate callers use `minimum_essential_height` and provide their own scroll
owner. Check the action after scrolling, including pointer and keyboard use.

For a primary editor, give the Scrollable a viewport and opt into a minimum
content height:

```rust
Scrollable::new_vertical()
    .fill_parent_height()
    .with_content_min_viewport_height()
    .with_child(Box::new(editor))
```

`fill_parent_height()` sizes the viewport. The additional opt-in lets short
auto-height content fill that viewport while long content scrolls. Ordinary
multiline form fields remain content-sized by default, and a child's explicit
fixed height stays authoritative. Scratchpad uses this composition with its
existing editor and Save action.

For inline rich text, use `RichTextLine` marks so measurement, glyphs, caret,
selection and links share one shaped layout. Do not paint styled words over a
plain text field: their different advances create doubled glyphs and misplaced
interaction targets. Code chips and rules are decorations around those shaped
ranges, not separately positioned text.

For a long command strip, let `Toolbar` choose the visible commands from
renderer measurements and put the remainder in its owned overflow menu. The
same original callbacks remain available through pointer, keyboard and semantic
activation. Use the ordinary Widget measurement and overlay lifecycle;
`recompute_visible` is a compatibility estimate for callers without a renderer,
not the final measured layout. A small form footer should still use `Actions`
when wrapping its few buttons gives a clearer result.

Explicit dimensions remain authored choices. Use guided overrides or fully
[manual composition](MANUAL_LAYOUT.md) wherever the default does not express the
intended layout. Evaluate the resulting reading order, useful content area,
containment and action reachability rather than counting custom pixel values.

### 7b. Spacing tokens, insets and density

Every gap and inset is a step on `theme.tokens.spacing` (`s1`–`s16` =
4 / 8 / 12 / 16 / 24 / 32 / 48 / 64); every control height, hit target, panel
width and icon size is a field of `theme.tokens.metrics` (§9.6). Both scale
with `ui_scale`; a custom theme's `spacing_px` rescales the whole spacing scale
proportionally and leaves metrics alone. That is also why a hit target must
read `metrics.min_pointer_target`, never a spacing step: a control that sizes
its minimum from `spacing.s8` shrinks with a dense gutter preference.
`Spacing::BASE` and `Metrics::BASE` are the unscaled reference values for the
few chrome constants that must be compile-time.

Rhythm inside a group versus between groups. Each relationship is a named
token, `theme.tokens.spacing.rhythm.*`, scaled with the spacing scale; a
painted layout reads the rhythm by relationship instead of picking a step:

| Relationship | Token (step) | Typical use |
|---|---|---|
| Label ↔ its control, icon ↔ its label; a control's face inset | `rhythm.tight` (`s1`) / `metrics.icon_label_gap` | stacked field label, button icon |
| Controls inside one cluster; related rows that read as one group | `rhythm.related` (`s2`) | a bar's tool buttons, help copy under a field |
| Siblings of equal weight | `rhythm.sibling` (`s3`, `gap_standard`, the `Row`/`Column` default) | clusters on a bar, list rows, cards |
| Section padding, gap between Panel sections | `rhythm.section` (`s4`) | panel body, card gutter |
| Before the next group; the content gutter | `rhythm.group` (`s6`, the `Column` default inset) | form sections, page edge |
| Between major regions | `s8`+ | header block ↔ content |

One rhythm per panel or screen: the same relationship always gets the same
step. A section gap of 30 px beside one of 18 px for the same relationship is
a defect even when both are "authored".

The framework supplies the values so callers state the relationship, not the
number:

- `Column::with_rhythm(Rhythm::Section)` / `Row::with_rhythm(..)` space the
  children by relationship (`Tight`, `Related`, `Sibling`, `Section`,
  `Group`), resolved from the theme at layout time;
  `with_padding_rhythm(horizontal, vertical)` insets them the same way.
  `Row::with_gap(px)` / `Column::with_gap(px)` remain for a gap that is not a
  relationship; `Row::with_justify(..)` and `Row::with_align_items(..)` place
  children along and across the row. Defaults (gap `s3`, children centred,
  start-packed) are unchanged.
- A child that is absent takes no gap: `optional(None)`, a `When` whose
  condition is false, bound text (`Text::from_state`) that renders nothing, a
  `ModelView` that builds nothing, a zero spacer, a `ModalDialog` or a
  `Popover`. Never add a spacer to make up or cancel a gap.
- `Spacer::token(SpacingStep::S4)` is a fixed gap that resolves from the Theme
  at layout time and therefore follows `ui_scale`; `Spacer::fixed(px)` is the
  authored-pixel form. Neither grows. `Spacer::new()` still grows to push
  siblings apart.
- `insets_all(px)`, `insets_xy(horizontal, vertical)` and
  `insets(top, right, bottom, left)` build a taffy padding `Rect` from token
  values in one expression instead of four `length(..)` fields.

`tests/spacing_token_audit.rs` ratchets module-level pixel constants,
literal `length(N)` values, literal container gaps and padding, and
`Spacer::fixed(N)` in widget, chrome, shell and app production code: a file
may never carry more than its shard under `tests/baselines/spacing/` records.
Every ratchet runs on one engine (`tests/support/ratchet.rs`), so all audits
see exactly the same production lines; §9.13 lists them.

A theme's density (`ThemeDocument::density.spacing`) is built inside
`ThemeDensity::USABLE` (0.75–1.5) even though the schema admits 0.5–2: the
whole scale is proportional to it, and half the scale would put `s1` at two
pixels.

### 7c. Overlays, status and captures

**One modal, one popover, one top layer.** A modal is a `ModalDialog`; an
anchored transient (an option menu, a small form next to its control) is a
`Popover`. Both are floating Panels (§9.1) and both paint only in the window's
top layer (`Widget::draw_overlay`), which the host runs after the app *and*
after the window's own chrome. A modal's `surfaces.scrim` dims the background
throughout the window, including sidebars, while excluding its foreground.
Its floating material must not sample its own scrim or shadow. A popover is
never clipped by the pane it was opened from. On compact viewports (either
axis below `panel_width_sheet`) a modal fills the window as an opaque page,
without rounded floating edges, shadow or scrim; its body scrolls while its
footer stays reachable. On larger viewports a sheet follows its content
height. Its title is a `Title`, and its actions are Regular buttons anchored
to its footer in the platform's order.
Immediate-mode hosts use the same chrome through `modal_dialog::layout`,
`paint_backdrop` and `paint_actions`.

**Status has two homes.** What a person reads while working and expects to
stay put (Ln/Col, a word count, unresolved conflicts) is persistent status in
`chrome::status_bar`, which reserves its band (`status_bar::split`) so it
never covers content. A change worth noticing once (a page turn, a finished sync) is
the transient `StatusBubble`: it shows on change, hides by itself and floats
in its area's bottom-trailing corner. An error the person must act on is a
`Banner` (persistent, in the flow) or a `Toast` (transient, with an icon close
button). A document's save state is `SaveStatusSurface`: autosave is the
system's job, so "Saving…" and "Saved" never show (they stay in the
accessibility status); a failed save is an error card with Try again that
stays until the save succeeds, and that success is a one-time "Saved" in the
transient bubble. An app never pokes or pins a save bubble or paints its own
save pill.

**Chrome over media uses the material on the media.** Video controls are a
floating Panel over the picture; text drawn straight onto media sits on
`overlay::scrim::vertical_fade` in `colors.media.ink`; a letterbox is
`colors.media.letterbox` in every theme. No stepped bands, no theme surface
behind a frame.

**The History bar is the window's, only where the app has no header row.**
An app with a `NavShell` header row gets the History control in that row
(§1) and no bar. Any other app gets a bar on the content region with one
hairline under it, holding the same one History glyph at its trailing edge,
on while History shows. A framework History that is open keeps its bar,
with the control on, because the live app beside it is inert.

**Captures are the live window, exactly.** `--shots` writes the frame on
`surfaces.content` after it has settled (`FrameHarness::settle_for_capture`:
animations finished, image loads drained, within a bound), with the wall
clock pinned to 30 June 2026, 10:42 local (`ui::clock::local_now`). Stage a
state through the framework scenario hooks (`ResourceApp::scenario`,
`scenario_viewport`, `ResourceSurface::stage`), never through a separate
capture root.

---

## 8. Never hand-roll an interactive primitive

Manual positioning of standard widgets is supported. The guidance below concerns
reimplementing their interaction and editing behavior, not choosing their geometry.


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
  anything that is not a widget, so hand-rolled controls are unreachable by
  keyboard and invisible to focus-follow.
- **Accessibility** — the AT-SPI / a11y tree is built from the widget tree; a
  painted rect has no role, name, or state, so it does not exist to a screen
  reader.
- **Theming** — shared widgets read every color and metric from the `Theme`
  (hover/press/focus/disabled included), so light/dark, high-contrast, the
  workspace temperature and the person's own colour all track automatically. A hand-rolled control with `Color::rgb(...)`
  literals breaks all three (see §7's "Theme-driven" clause).

This generalises §3 (text editing) to **all** interactive primitives. Concretely:

- Editable field → `TextInput` (or `NumberInput` / `EditableLabel`). Never keep
  a per-app cursor index and splice characters into a `String` on
  `Key::Character` / `Backspace`.
- Narrowing a list → `SearchField::filter()` bound to `Filterable`; find in a
  document → `FindBar` over `Findable`; searching the app → the platform
  palette (§4a). Never a hand-drawn search box.
- Clickable → `Button` (a canvas host paints `ButtonFace` and hit-tests
  `ButtonFace::contains`). Never `draw_*_button` + a `contains()` hit-test.
  A glyph that does something is `Button::icon_only(id, icon, name)`, never a
  text glyph.
- Confirm / prompt / sheet → `ModalDialog` (scrim, focus capture, Esc, a11y are
  built in; it is presented in the window's top layer, §7c). Never paint your
  own overlay + scrim rect.
- Select / choice → `Dropdown`; on/off → `Toggle` / `Checkbox`; range →
  `Slider` / `NumberInput`; peer views → `TabBar` / `ToggleGroup`; right-click →
  `ContextMenu`; hover help → `tooltip`; anchored transient → `Popover`.
- Sidebar, master/detail, drawer, push screens, column headers → `NavShell`;
  a list of destinations → `NavList` (a painted app: `SidebarDrawer` +
  `chrome::paint_nav_row` / `paint_nav_section`); a selected or hovered row →
  `chrome::paint_row_selection`. Never a private sidebar row painter,
  selection fill or collapse chip.

**If the shared widget is missing a capability you need, extend the widget — do
not fork it.** Add the feature to the one `ui` primitive so every app gets it and
the accessible / themed / clipboard-correct version stays the default. Forking a
private copy re-earns every bug above and guarantees drift. The only sanctioned
"hand-rolled" text surfaces are genuine document engines whose buffer *is* the
app (the terminal grid, a code/rich-text/cell editor) — and even those must route
their **side** fields (find bar → `FindBar`, rename box, cell entry, command
line) through the shared widgets, and are themselves candidates for a shared editor primitive
(§ toolkit gaps) rather than N private forks.

> Enforcement: a new painted control is a toolkit gap, not an app feature. Grep
> your diff for the patterns in §9.13 plus `draw_*_button` and
> `Key::Character => *.push` — each hit is either a shared-widget call you
> skipped or a widget you should be extending.

**Grep for the spelling the code actually uses.** This rule used to say "grep
for `Color::rgb`", and that pattern is why hard-coded colour accumulated under
it for months: `Color::rgb8(…)` is the *old* peniko constructor and this tree
does not call it. The one automated check that implemented the rule
(`crates/sdk/ui-core/tests/design_token_audit.rs`) was green over 125 literal
`Color::from_rgb8(…)` colours in `crates/sdk/ui-core/src` alone, because it was looking for a
string that appears nowhere. Two more spellings were invisible for the same
reason:

- **`Color::WHITE` / `Color::BLACK`** — a label on an inverted fill is
  `intent.on_inverted`, and the inverted fill is near-white on a dark
  material, so a hard-coded white label there is invisible.

`design_token_audit.rs` now covers all three in the toolkit, the shell and
every app, ratcheted against the shards under
`crates/sdk/ui-core/tests/baselines/theme/`: a file may never gain one. The
count only goes down. Those shards, not this document, are the authority on
how many are left (§9.13).

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
surfaces of the same role are automatically consistent. The values come from
`docs/design-language.md` ("Shape", "Colour", "Material").

| Role | What it is | Radius | Surface | Border | Elevation |
|---|---|---|---|---|---|
| **Ground** | the desktop behind everything | — | `surfaces.base` | — | none |
| **Bar** | docked chrome fixed to a screen edge (top bar) | — (edge to edge) | `chrome::shell::panel_ground` (opaque) | bottom hairline only | none |
| **Panel** | a floating surface that opens over content (quick settings, palette, menus, popovers, sheets) | `radii.panel` (18) | the floating material, `Material::Floating` (§9.2) | full hairline, plus `elevation.floating_edge` inside it on a dark material | `elevation.floating` |
| **Control** | something inside a Bar or Panel that you press (action tile, list row, slider, field) | `radii.control` (8) | selection tiers (§9.3) | per tier | none |
| **Selector** | one of a set you choose between (a tab, a segment, a tray or scope chip, a workspace chip, a strip segment) | `radii.control` for tabs and segments (not pills); `radii.full` for chips, pill-shaped by meaning | selection tiers (§9.3) | per tier | none |
| **Object** | a window tile | `radii.tile` (14) | `surfaces.content` | `Colors::window_edge()` | none; `elevation.floating` only while dragged |

Inside an Object, the window's regions (§9.2) are the only grounds, and the
framework paints them.

The radius scale is semantic (`tokens::Radii`):

| Field | Value | For |
|---|---|---|
| `tile` | 14 | a window tile |
| `panel` | 18 | a floating panel (menu, popover, palette, sheet, quick settings) |
| `control` | 8 | a button, field, segmented track, a tile inside a panel |
| `item` | 6 | an item inside a control or list: a selected row, a segment thumb |
| `detail` | 4 | a detail inside a control: progress track, badge, sparkline clip; also the floor |
| `full` | pill | chips, presence pills, tags, avatars — pill-shaped by meaning |

**Radii are concentric.** Something inset inside a rounded container takes
`radii.concentric(outer, inset)` — the container's radius minus the inset,
never below `radii.detail` — rather than a token of its own, so its corners
run parallel to the container's. The palette's query field and its selected
row are concentric with the palette card; the panel's inner light edge is
concentric with the panel. Buttons and tabs are not pills, and a radius
literal in a chrome draw path is a defect.

### 9.2 Surfaces come from the semantic tier, never from a ramp slot

Chrome reads `colors.surfaces.*`. It does **not** reach into
`colors.neutral[n]`. A ramp slot is a colour; a surface token is a *promise
about contrast* that every material keeps, at every temperature.

The neutral material is defined in OKLCH and shares one hue
(`colors.palette.hue`, chroma at most 0.010), so a theme or a workspace can
shift its temperature without moving contrast (`Theme::with_neutral_hue`;
each workspace takes the hue of its wallpaper, `ui::workspace_hue`). A
palette without a hue (`Palette::hue == None`, the light preset) is pure
grey until the window's workspace gives it one; nothing falls back to a
default hue.

| Surface | Dark L | Light L | Use |
|---|---|---|---|
| `surfaces.base` | 0.16 | 0.90 | Ground: the desktop behind tiles |
| `surfaces.content` | 0.215 | 0.995 | Object: every app's window ground, owned by the framework |
| `surfaces.sidebar` | 0.195 | 0.965 | the navigation region of a window |
| `surfaces.inspector` | 0.235 | 0.975 | the detail / inspector region of a window |
| `surfaces.raised` | 0.265 | 1.0 | fields, wells and objects inside content |
| `surfaces.overlay` | 0.285 | 1.0 | Bar and Panel |
| `surfaces.scrim` | — | — | behind a modal Panel, faded in with it |

The ink tiers `text_primary` / `text_dim` / `text_tertiary` are defined on
`surfaces.content` (dark L 0.96 / 0.76 / 0.62, light 0.18 / 0.43 / 0.56).
`tokens::contrast::FLOORS` holds every pairing to WCAG AA for its role (AAA on
the high-contrast material) and is swept at every hue in tests.

#### One ground per region, painted by the framework

An app never picks or paints a ground. A window is made of regions,
`ui::surface::Region::{Content, Sidebar, Inspector}`, and each has exactly
one ground:

- **Content** is `surfaces.content`, the Object's surface. The framework
  paints it in every path an app can be seen through: the native runner
  clears to it, `ResourceRoot` fills it under the view, `TileHost` fills it
  under a hosted tile, and `--shots` captures land on it
  (`FrameHarness::render_png`). An app that paints nothing (a clock, a
  print queue) stands on it; floating surfaces stay transparent around their
  material.
- **Sidebar** (navigation, leading edge) and **Inspector** (detail,
  trailing edge) are their own tiers. `NavShell` paints its sidebar slot;
  a painted app calls
  `ui::surface::region(scene, transform, rect, Region::Sidebar, theme)`,
  which paints the tier and returns the ground. `Theme::region(region)`
  reads it without painting.
- A region is told from content by **tone**, not by a line: the tiers sit at
  least `tokens::colors::REGION_MIN_STEP` (OKLCH ΔL 0.015) from content in
  every material and at every temperature. Where a material gives them the
  same ground (high contrast) `surface::region` draws the hairline on the
  edge the region shares with content, and nowhere else.
- What sits in a region is drawn with that region's theme:
  `theme.in_region(region)` (or `theme.on_ground(ground)` for any other
  ground) re-points `surfaces.content` to the ground, re-measures the ink
  tiers against it and steps `intent.selection` / `selection_hover` from it,
  so the same widget is right on content, sidebar and inspector. `NavShell`
  hands its sidebar child that theme.
- An app's header or toolbar band sits on its region with an `edge_on`
  hairline of that region's ground under it; it is never a raised band.
- `surfaces.raised` is for fields, wells, cards and controls *on* a region;
  `surfaces.base` is the desktop only; `neutral[n]` is never a ground.
- A control's hover and pressed fills are lifts of what it stands on: of its
  own rest face for a filled control (`colors.lift_on(face, 0.5)` /
  `lift_on(face, 1.0)`), of the region ground for a transparent one
  (`intent.selection_hover` / `intent.selection`).

A Bar is opaque: it is docked and nothing meaningful is behind it.

**A Panel is the floating material.** Floating chrome — the palette, menus,
popovers, quick settings, sheets, notifications, and chrome over media — is
drawn over a blur of what is behind it, tinted with the panel ground at 72 %
(`docs/design-language.md`, "Material and transparency"). A surface asks for
it by name and never for a radius or an opacity:

```rust
ui::floating_surface::paint_material(scene, transform, &shape, Material::Floating, theme);
// or, for the whole Panel row (fill + edge):
ui::surface::panel(scene, transform, rect, radius, theme);
```

Vello cannot sample what it has already drawn, so the compositor blurs in a
pass before the frame (`ui::backdrop`): while it paints, each floating surface
draws a placeholder image under the tint and records what is below it; the
renderer renders that backdrop at half resolution, halves it until the blur
fits a short kernel (`surfaces.material_blur_px` is the standard deviation),
blurs it separably and substitutes it for the placeholder. The live DRM,
headless and nested compositors, an app's own window (`ui::App`'s render
thread), the headless `FrameHarness` and `ShotFrame` behind every app's
`--shots`, and the offscreen `shots` path all run the same pass, so a menu
over content blurs it identically live and in a capture. A floating surface
painted in the main pass inside a `Column` or `Row` blurs too: their subtree
caches carry it (`backdrop::nest_scene`, `backdrop::append_nested`) and are
redrawn while they hold one.

The material falls back to opaque `panel_ground` whenever the blur is not
there: under the reduced-transparency setting
(`appearance.accessibility.reduce_transparency`), with forced colours or a
theme without blur (the high-contrast track), in a renderer without the pass
(a raw scene rendered without its recorded backdrop), for a surface drawn
into a scratch scene no host composes, and for surfaces past the per-frame
budget (`MAX_BACKDROP_REGIONS`). The placeholder's own pixels are that ground,
so a missed substitution is the opaque material, never a hole. Never paint
unblurred translucency: a translucent fill without the blur is not glass but
a loss of contrast that changes with what the user happens to have open.
`Material::Opaque` is the explicit opaque choice for a surface whose backdrop
must not show through.

**A Panel separates itself by value and by edge.** `panel_ground` walks the
overlay tier away from *both* the desktop and the window ground until it
clears each of them, because a Panel floats over app content and a Bar does
not, and the two are one material. `panel_border` supplies the edge.

### 9.3 Selection is a lift, and only a lift

A selected row, tab or segment is a **lift**: one surface step of the ground
it sits on, with primary ink, and no hue. On the window ground that is
`colors.intent.selection` (hover `selection_hover`); on any other ground
`colors.lift_on(ground, 1.0)` (hover `0.5`). In chrome, every "which one is
chosen" state — active tab, active workspace, active strip, selected row,
chosen scope chip, current power profile — uses the **same four tiers**:

| State | Fill | Border | Inner glow | Scale |
|---|---|---|---|---|
| idle | transparent | none | none | 1.0 |
| hover | `colors.lift(0.5)` | none | none | 1.0 |
| **active** | `colors.lift(1.0)` | `hairline_on(ground, 26)` | `hairline_on(ground, 51)` | 1.0 |
| pressed | as active | as active | as active | 0.97 |

This is `ui::chrome::selection_tier`; call it, do not restate it. On any other
ground use `chrome::selection_tier_on(theme, state, ground)`: the active fill
is one surface step of that ground (`colors.lift_on(ground, 1.0)`, hover
`0.5`), stepping the other way where the ground has no room (a white well on
a dark palette), so it stays at least `SELECTION_MIN_STEP` (OKLCH ΔL 0.06)
from the ground on content, sidebar, inspector, raised and panel grounds in
every material.

**One content edge.** Inside a column, everything that is text starts on the
column's content edge: a `ListRow`'s title, a `Section` or `SectionHeader`
title, a `PropertyList`'s labels, a `StatTile`'s label, a `DataTable`'s first
column, a `StatusLine`, an `ObjectHeader`. A section header's action ends its
*label* on the trailing edge. What a row paints behind its text (the hover
and selection lift, a zebra stripe, a header action's hover) **bleeds
`chrome::row_bleed` (`s2`) outward past that edge** on both sides, so text
never moves when a row is selected and a list lines up with the headings
around it. A retained row grows its own layout box by the bleed when it
paints; a face (`ListRowFace`, `PropertyListFace`) takes the row's outer
rect, the content edge grown by the bleed. The column keeps the room for the
bleed in its gutter; an app never insets a list, a header or a sentence by
hand to make them line up. A list that clips its rows (`VirtualList`) stands
them one bleed in from its own edges, so their lift fills it.

**A selected list row is the lift and nothing else.** Every list paints its
rows' hover and selection through `ui::chrome::paint_row_selection(scene,
transform, rect, state, ground, theme)`: the row's outer rect (the content
edge grown by the bleed), at `radii.item` (6), primary ink, no border, no
hue, no leading bar or stripe. A run of selected rows is one lift
(`paint_row_selection_joined` with a `RowJoin`): the corners two lifted rows
share are square, so a multi-selection has no notch. `TreeView`, `NavList`,
`paint_nav_row`, `ListRow`, `VirtualList` and
`DataTable` use it; a painted list calls it. A private `selected_on` / `row_tier_on` / `selection_fill` is a
defect.

**The armed row of a menu is a lift too, one step stronger.** The row Enter
or a click will activate in a menu, popover, dropdown, slash menu or palette
result list is the panel's **overlay lift**: `colors.lift_on(panel_ground,
OVERLAY_LIFT)` with primary ink, no hue, concentric inside the panel
(`widget::floating_surface::armed_row` / `paint_armed_row`). A floating panel
takes one and a half surface steps instead of one, because a single step
measured ΔL* 11.6 on the dark panel and read as "no highlighting"; the
stronger step clears ΔL* 15 on every
material and stays a quiet grey on a light panel, the weight of the sidebar
lift rather than an inverted slab. Pointer hover and the keyboard arm the
same row, so a menu has one highlight. Every menu paints it with
`paint_armed_row`, its rows inset `floating_surface::menu_inset` (`s2`) from
the panel on every side and `metrics.menu_row_height` tall, so popover
menus, dropdown lists and context menus are one shape. A state row ("Copy on
select", a sort order) is `MenuItem::with_checked(bool)`, and the applied
option of a popover menu or a dropdown list carries a check; the row a
context menu acts on is ringed in your colour while it is open
(`ContextMenu::anchor_row`).

**A menu opens at its trigger, in the top layer.** Every anchored Panel
(context menu, popover, dropdown list) is placed by
`floating_surface::place_anchored`: under its trigger with the leading edges
aligned (at the pointer for a right-click), flipped above when there is more
room there, aligned to the trigger's trailing edge when it would run off the
window, then shifted along the trigger into the window. A menu paints only
in `Widget::draw_overlay`, like a popover or a modal, so no later sibling,
pane or the window's chrome covers it; a painted host draws its menu from
its own `draw_overlay` or returns it from `children_mut` while it is open.

**A control with its own floor** (a filled rest face rather than a
transparent one) takes all of its states from one ordered ramp against the
ground: `chrome::selection_floor_ramp(theme, state, ground)` — rest, hover
and active each a further step from the ground on every material. Do not mix
a floor with the transparent tier.

An active thing never gets *more* prominent on hover. The contrast gain
belongs to the inactive things brightening toward the active tier.

**Elevation and selection are different vocabularies and must not be mixed:**

> **A shadow means "off the surface". A lift means "chosen on the surface".**

A selected pill casts no shadow. A dragged object casts `elevation.floating`
— it is lifted and will land somewhere.

**Only floating things cast a shadow, and they all cast the same one.**
`elevation.floating` is one soft, wide shadow (blur 32, offset a quarter of
that); on a dark material the floating surface also strokes
`elevation.floating_edge`, a 1 px light edge just inside its outline
(`ui::surface::floating_edge`, painted for you by `ui::surface::panel`).
Window tiles, bars, cards and selected controls have no shadow: a tile is
separated by the ground and its window edge.

**A shadow is the shape of the thing casting it, and comes from the token.**
One painter, `surface::shadow(scene, transform, rect, radius, token)`: it
blurs the surface's *own rounded outline*, so a shadow is equally dense the
whole way round and its corners follow the surface's corners. Pass the
radius. Do not hand a shadow a bare `Color`. `soft_shadow` is the older
sprite painter for effects with no outline to follow — the wheel disc's
bloom, a glow around a dragged card — and is not for drop shadows.

### 9.4 There is no accent: intents and your colour

The interface is a quiet, near-colourless material; **colour means that
someone is there or that something is happening.** Nothing in the theme is a
brand hue, and an app cannot supply one. Every former accent use is one of
these intents (`colors.intent`):

| Intent | Token | For |
|---|---|---|
| inverted | `intent.inverted` / `inverted_hover` + `intent.on_inverted` | a primary action, a switch that is on, a checked box, a slider's filled track, a progress fill |
| selection | `intent.selection` / `lift_on(ground, t)` | a selected row, tab, segment or item, and a menu's armed row at the overlay step (§9.3) |
| your colour | `intent.you` (= `border_focus`) | the keyboard focus ring (never focus a click gave), the text caret, selection handles, a drop or insertion indicator |
| selection tint | `intent.selection_tint` | the text-selection highlight (your colour, translucent) |
| status | `semantic.{danger, warning, success, info}` | status only, muted (chroma at most 0.13), never decoration. A destructive button is danger ink on a neutral control, filled only inside the confirmation step |
| presence | `presence.of_rgb(user_color(id))`, `presence.get(slot)`, `presence.series(i)` | a participant's cursor, selection, avatar and attribution; chart and tag series |

Links are primary ink with an underline. An "active" icon is primary ink, an
inactive one secondary ink. Which window has keyboard input is state, not
you: the focused window's edge (`window_edge_focused`) and its active tab's
keyline are neutral ink, so your colour is not painted around a window all
day. A resting readout's labels are ink too; a status hue is not a label.

**Your colour** is the local person's presence colour — one of the twelve
presence hues (`PresenceHue`), the colour their cursor has for
collaborators. It is identity, not theme: stored as `identity.colour` or
derived from the stable identity, never read by `Theme::configured`. The
host that shows a window applies it with the workspace's temperature
(`ui::WindowAppearance`, `Theme::in_window`): the shell resolves both and
sends each app window its own over the shell-ui channel
(`FloatingSurfaceHostMessage::Appearance`, applied by
`App::set_window_appearance`); a standalone app starts from the stored
choice, or the hue nearest the host accent, and stays neutral. The theme
sets only its lightness and chroma.

**Emphasis** is the one theme role that may colour controls: under
`Emphasis::Tinted { hue }` the `intent.inverted` fill (primary actions, on
switches, filled tracks) and the selection lift take that hue. Widgets read
the intents and never ask which emphasis is active; the label of the fill is
always `intent.on_inverted`.

**The presence palette** is twelve hues at one OKLCH lightness and chroma
(dark L 0.74 C 0.14, light L 0.58 C 0.15), so no participant looks more
important than another. Identities travel as colours
(`ui::presence::user_color`, `PEER_PALETTE`); paint them through
`colors.presence.of(..)`, which carries their hue at the active material's
lightness and chroma.

### 9.5 Borders are derived, never declared

Chrome borders call `Colors::hairline_on(ground, alpha)`, which picks white or
black by whichever actually moves against that ground. A hairline appears
only where two equal surfaces meet; groups are separated by space and type
weight, not boxes, and a card exists only for an *object* (something you can
open, move, share or drag).

- hairline on a control: alpha 26 (`/10`)
- inner glow on an active control: alpha 51 (`/20`)
- separator inside a Panel: alpha 26, inset from the panel edge by
  `spacing.s3`, never running full-bleed into the corner radius

The state veneers are `overlay_subtle` / `overlay_medium` / `overlay_strong`
and `overlay_border_subtle` / `overlay_border_strong`: directional washes that
lighten a dark ground and darken a light one. The declared strength is
still a declaration, so chrome prefers `lift` and `hairline_on`, which measure
against the ground the surface actually lands on.

#### Rest edges (toolkit and shell)

At rest (idle, enabled) every control, card and popup has a **visible derived
edge or a clear contrast step** against the surface it sits on, at 1× in dark
and light alike. "Edge" is `ui::surface::edge_on(fill)` painted as a crisp
inset 1 px stroke (`ui::surface::card`, the chip family's `ChipFace` /
`BadgeFace`, or the same geometry inline); chosen states use `selection_tier`
(Controls) or `glass_selector` (shell Selectors); focus is `intent.you` (your
colour); disabled is the rest face under `widget::DISABLED_ALPHA`, with its
edge kept at full strength, so rest and disabled never look the same.

| Control | Rest fill | Rest edge |
|---|---|---|
| Button `Secondary` / `Destructive` / disabled, Dropdown field, NumberInput steppers | `surfaces.raised` (hover `lift_on(raised, 0.5)`, press `1.0`) | `edge_on(raised)`; `Destructive` letters it in danger ink |
| Button `Primary` | `intent.inverted` (hover `inverted_hover`, press `Colors::inverted_pressed`, the hover step taken twice) | none; label `intent.on_inverted` |
| Button `Plain`, toolbar commands, icon buttons | transparent (hover `intent.selection_hover`, press `selection`) | none; a toggle that is on (`Button::toggled(true)`, a toolbar toggle) is `intent.selection` |
| Destructive confirmation (a `ModalDialog` danger action only) | `semantic.danger` | none |
| Card / Container (objects only) | `surfaces.raised`; a pressable one lifts on hover and press | `edge_on(fill)`; a section inside a card gets space, not a second edge |
| TextInput / NumberInput / SearchField | field fill (hover `lift_on(fill, 0.5)`, focus `1.0`) | `edge_on(fill)`; focus `border_focus`; a value `Field::with_error` rejected edges the field in danger ink (`Widget::set_invalid`) and its message is danger ink |
| Popover, ModalDialog, Toast, StatusBubble, video controls | the floating Panel body (`widget::floating_surface::paint`): `radii.panel`, the blurred material, `elevation.floating`; what stands on it is drawn with `theme.on_ground(panel_ground)`, whose `surfaces.raised` stays off the panel (`Colors::raised_on`) | `panel_border`, `floating_edge` on dark; a toast's severity is its leading icon's colour, as on a banner |
| Dropdown list, ContextMenu, SlashMenu, Autocomplete, LinkPopover, CommandPalette, FilePicker, PermissionPrompt | `panel_ground` (or `surfaces.overlay` where already used) | `panel_border`, `floating_edge` on dark; separators `separator(.., s3, ..)`, never full-bleed. The dropdown list and context menu take `radii.panel`, rows `metrics.menu_row_height`, content inset `spacing.s3`, and a separator only between two rows |
| Banner | `surfaces.raised` | `edge_on(raised)`; the severity is the icon's colour, never a tinted wash |
| Tooltip | the blurred material (`surface::panel`) at `radii.control`, ink measured on the panel ground | `panel_border`, `floating_edge` on dark |
| Segmented / ToggleGroup track (`SegmentedStyle::Segmented`) | `surfaces.raised`, `radii.control` | `edge_on(track)`; the chosen segment is a thumb `selection_tier_on(raised)` inset concentrically (`radii.item`); separators only between two unchosen segments. `Tabs`: no track, a 2 px primary-ink underline. `Quiet`: no track, a lift of the ground |
| Chip / Badge `Neutral` | `surfaces.raised`, `radii.full` | `edge_on(raised)`; selected is a lift, no edge |
| Chip / Badge `Tinted(tone)` | the tone washed 15 % into the ground | the tone at 38 %; label neutral ink measured on the wash |
| Chip / Badge `Live` | `semantic.danger` | none; label measured on it |
| Toggle | off `intent.selection`, on `intent.inverted` | `edge_on(track)`; on, the knob is `on_inverted`; off, a raised white knob with a ring that clears 3:1 on a light material and `text_dim` on a dark one |
| Slider | unfilled `intent.selection`, filled `intent.inverted` | `edge_on(unfilled)`; the thumb is a raised white object with a ring on a light material and primary ink on a dark one, clearing both halves; never collapses below six knob widths |
| Checkbox | unchecked `surfaces.raised`, checked `intent.inverted`; always the largest square in its rect, `radii.item` | `edge_on(raised)`, kept at full strength when disabled (as a switch's) |
| ProgressBar | track `intent.selection` (a lift of the ground, as a slider's unfilled half), fill `intent.inverted`, `radii.detail` | none |
| TimePicker | value cells `surfaces.raised` at `radii.control`; step arrows are `Icon::ChevronUp` / `ChevronDown`, lifted under the pointer | `edge_on(raised)`; the focused cell `border_focus` |
| Scroll thumb | `text_tertiary` at rest (a third lighter on a light ground), `text_dim` active stepped to 3:1 on the ground; `metrics.scroll_thumb` thick, centred in its lane | none; a `Scrollable` (or a painted `ScrollRegion`) keeps `metrics.scrollbar_gutter` (thumb plus `s1` each side) free on its trailing edge while its content overflows, and on its bottom edge for a horizontal bar |
| DataTable header and rules | the region's ground (no fill of its own) | `edge_on(ground)` under the header and, with `.ruled()`, between columns |
| StatTile, PropertyList, Section | the ground | none: separated by space |
| MediaCard art | the art | a seated `hairline_on(ground)` inside the art's outline; badges over the art use the floating material |
| DropZone | the ground | dashed `edge_on(ground)`; your colour only while a drag hovers |
| TreeView / NavList / VirtualList rows | transparent | none; selection is a lift inside the `s2` inset at `radii.item` (`paint_row_selection`) |
| ListRow | transparent | as the rows above; separators are inset `edge_on(ground)`, hidden beside the selection |
| Divider | — | `border_standard` only between two equal surfaces; `Divider::on(surface)` elsewhere |
| App chrome Toolbar band (`toolbar::paint_band`) | none: the region's ground | `chrome_hairline` along the bottom |
| StatusBar, History bar | none: the region's ground | `edge_on(ground)` where the bar meets the content |
| Selectors (tab pills, tray pill, scope chip) | idle none | none (`glass_selector`) |

Forced-colour accessibility is the field-edge exception: when
`Theme::colors_are_forced()` collapses surface levels, the shared field painter
uses `border_standard` for its idle outline so its boundary retains non-text
contrast, including disabled fields under the existing disabled group opacity.
Enabled focus and error outlines keep their own roles.

Outside that forced-colour field branch, a declared border or ramp token
(`border_standard`, `border_strong`, `overlay_border_strong`,
`colors.neutral[..]`) on a chrome surface is a defect;
`tests/design_token_audit.rs::declared_borders_in_chrome_do_not_grow` ratchets
their count against the shards under `tests/baselines/chrome-border/`.

**One painter per row of that table.** `ui::surface::card(fill)` is the edged
opaque surface; `ui::surface::panel(theme)` is the Panel row (the floating
material + `panel_border` + `floating_edge`) so no app spells the set;
`ui::surface::card_on(fill, ground)` is for a translucent wash — the edge
derives from what the eye sees, the wash composited over its ground. `edge_on`
assumes an opaque fill. All three snap their rect to whole logical pixels and
`separator` lands on one device-pixel row (`separator_rect`): a 1 px edge on a
fractional coordinate is two half-covered rows that fall under
`HAIRLINE_MIN_DELTA` even when the colour registers.

### 9.6 Type and density

Chrome uses the same type roles as apps (`docs/typography.md`), from the
compact end of the scale — chrome labels a thing, it does not host prose.
There is no all-caps or letter-spaced chrome text; section headers are
sentence case.

| Use | Role | Colour |
|---|---|---|
| Panel title | `Heading` | `text_primary` |
| Section header inside a Panel | `Subheading`, sentence case | `text_dim` |
| Control label | `Label` (a control's `ControlSize`) | `text_primary` |
| Control sub-line / value | `Caption` | `text_dim` |
| Bar label | `Label` | `text_primary` |
| Numeric readout | `Label.numeric()` | `text_primary` |

Density, so two panels built by two people come out the same size. Each value
is a field of `theme.tokens.metrics` (`ui::tokens::Metrics`), scaled by
`ui_scale` like spacing; read it from there rather than restating the number:

- Bar height 40 (`bar_height`); Bar control height 28 (`bar_control_height`).
- Controls take a `ControlSize`, never a height: `Small` 28, `Regular` 32
  (the default for `Button`, `Dropdown`, `NumberInput`, `ToggleGroup`),
  `Large` 40, each grown with the label's line at large text. A `Small`
  control keeps its 28 px face and reaches the 32 px target through its hit
  rect. A button is as wide as its label (at least twice its height; an
  icon-only button is a square) and never elides while its parent has room.
- Menu row 32 (`menu_row_height`); scroll bar gutter 16 (`scrollbar_gutter`),
  thumb 8 (`scroll_thumb`), one wheel line 60 (`wheel_line`), a committing
  swipe 88 (`swipe_commit`).
- Readable width 720 (`readable_width`) for forms and pages of facts; reading
  measure 640 (`reading_measure`) for long-form text; a chooser beside its
  label 224 (`chooser_width`).
- Panel padding `spacing.s4` (16); gap between Panel sections `spacing.s4`.
- Panel row / Control height 44 (`panel_row_height`); Selector height 28
  (`selector_height`).
- Minimum pointer target 32×32 (`min_pointer_target`) — a 20 px glyph
  (`icon_md`) gets a 32 px hit rect. Glyphs: `icon_sm` 14, `icon_md` 20,
  `icon_lg` 24; icon-to-label gap `icon_label_gap` 6.
- Panel width: 320 for a utility Panel (quick settings,
  `panel_width_utility`; an overlay drawer is the same width,
  `drawer_width`), 360 for a roster or list Panel (`panel_width_list`), 560
  for a sheet with a form in it (`panel_width_sheet`).
- **The window shell (`NavShell`) reads every size from the metrics**; an app
  passes none:

  | Metric | 1× | For |
  |---|---|---|
  | `header_height` | 56 | the one header row: `min_pointer_target` 32 in an `s3` inset |
  | `sidebar_width` | 280 | a docked sidebar (drag range `sidebar_min_width` 200 – `sidebar_max_width` 420) |
  | `drawer_width` | 320 | the floating drawer, `s2` in from the window edges |
  | `rail_width` | 76 | a docked icon rail (`NavMode::Docked`, `NavList::rail`) |
  | `overview_width` | 340 | the overview column |
  | `detail_min_width` | 360 | narrowest the detail beside an overview may get before they push |
  | `content_min_width` | 560 | the room the content beside a sidebar needs, a readable column the width of a sheet; narrower, the sidebar becomes a drawer (`compact_breakpoint` 840 − `sidebar_width`) |
  | `inspector_width` | 300 | the inspector; it floats from the trailing edge when it does not fit |
  | `nav_row_height` | 32 | a navigation row (`NavList`, `paint_nav_row`) |

  The breakpoints follow from them: the sidebar docks while
  `sidebar_width + overview_width + detail_min_width` (980) — or
  `sidebar_width + content_min_width` (840) without an overview — fits, so
  collapse follows the room the content needs: an 800 × 600 window shows its
  sidebar as a drawer rather than squeeze a document to 520; the
  overview and detail stand side by side while `overview_width +
  detail_min_width` (700) fits. The shell's buttons (sidebar toggle, Back,
  inspector toggle, the window's History control) are Plain icon buttons of
  `min_pointer_target` in the `s3` inset.
  Chat's channel list and conversation form a master/detail composition:
  its conversation reserves `detail_min_width`, rather than the form-sheet
  `content_min_width`. With its rail, the initial docked breakpoint is 716.
  Adaptive sidebar resizing reserves the declared content minimum, so
  enlarging a docked panel cannot hide its own resize control.
- **One text-scale rule (`Theme::responsive_scale`).** The shell grows its
  text-sized widths and breakpoints by `ui_scale × text_scale.max(1.0)`
  (`Metrics::text_growth`): at 200 % text the panes stop fitting side by side
  at twice the width, and never shrink below 1×. A rail is sized by icons and
  follows `ui_scale` alone. The header row and a one-line control (the
  filter field) grow by `widget::text_control_height(authored, theme)` — the
  authored height until the body line outgrows it — never by the whole
  scale. A painted app's `SidebarDrawer` applies the same rule to the widths
  it authors once the host calls `drawer.scale_for(theme)` from its
  `build_layout`.
- **The shell places its children through the host's layout pass.**
  `NavShell` is a manual layout policy (`MANUAL_LAYOUT.md`): every column,
  header action, the filter and the shell's buttons are children in the
  host tree, measured with the host's renderer and placed at the rects the
  responsive plan gives them, so focus, routing and semantics see the real
  geometry. A column the shell is not showing (a hidden sidebar, a closed
  drawer, a screen pushed away) is not in the host tree; behind an open
  drawer the other columns stay laid out and painted but take no input,
  and only the parts laid out this frame paint. A slot or header changed
  while an event is routed takes effect at the next build. A part whose
  overlay holds the pointer (a popover a header button owns) hears pointer
  events before the columns.
- **The header row is one face** (`chrome::HeaderRowFace`): `header_height`;
  leading chips (toggle, Back) and trailing chips (inspector toggle,
  History) of `min_pointer_target` in the `s3` inset, `s2` apart; the title
  at the content gutter (`rhythm.group`) when no chip leads; the rule under
  content columns. `NavShell` and every painted header row use it.
- **Every column's header shares the one header row**, and every body starts
  under it, so the columns' first rows share one top edge.

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

Every look rule that can be seen in source is a **ratchet** in
`crates/sdk/ui-core/tests/`. A ratchet counts each pattern per production
file (comments and `#[cfg(test)]` items are skipped) in the toolkit, the shell
and every app, and the count may only fall. A change that adds a hit fails. A
change that removes one passes, and the run notes the lower count so the
baseline can be lowered in that change or a later one. Apps are scanned
(`apps/*/src`, so a new app is covered the day it lands), and every root must
yield files or the ratchet fails rather than passing empty.

| Audit (baseline dir) | Scans | Keys | Test |
|---|---|---|---|
| `theme` | ui-core `src`, shell `src`, `apps/*/src`; not `theme.rs`, `tokens/`, `presence/render.rs` | `literal_color`, `white_or_black`, `theme_blind_white` | `design_token_audit.rs::theme_escape_hatches_do_not_grow` |
| `app-paint` | shell `src`, `apps/*/src`; same exemptions | `literal_color_new`, `ramp_slot`, `legacy_alias`, `desktop_ground`, `literal_radius`, `literal_alpha`, `raw_paint`, `glyph_icon`, `transparent_shot` | `design_token_audit.rs::app_and_shell_paint_knobs_do_not_grow` |
| `chrome-border` | ui-core `src/widget`, `src/chrome`, `surface.rs`, `status_bubble.rs`, shell `src` | `declared_border` | `design_token_audit.rs::declared_borders_in_chrome_do_not_grow` |
| `spacing` | ui-core `src/widget`, `src/chrome`, shell `src`, `apps/*/src` | `pixel_const`, `taffy_length`, `literal_gap`, `literal_spacer` | `spacing_token_audit.rs::spacing_audit_sees_what_it_polices_and_literals_do_not_grow` |
| `typography` | ui-core `src/widget`, `src/chrome`, shell `src`, `apps/*/src`; not `tokens/typography.rs`, `text_render*`, `text_metrics.rs` | `literal_font_size`, `legacy_size_api`, `font_size_const`, `type_step_arithmetic`, `raw_type_step`, `renderer_scale_helper` | `typography_token_audit.rs::typography_literals_do_not_grow` |
| `copy` | `apps/*/src` | `all_caps_label`, `middle_dot_join`, `to_uppercase` | `copy_audit.rs::copy_rules_do_not_grow` |

What each key counts (the test file pins every key against lines that must
and must not match):

- `literal_color`: `Color::from_rgb8(` / `from_rgba8(` with a literal first
  argument. Match both constructor spellings (see §8: grepping only for
  `Color::rgb` is how these accumulated under a green check).
- `white_or_black`: `Color::WHITE` / `Color::BLACK`. A colour is a colour even
  when it has a name; the theme has ink and scrim tokens.
- `theme_blind_white`: `surface_white_*` / `border_white_*`, a declared
  strength never measured against its ground (§9.5).
- `literal_color_new`: `Color::new([<literal>, …])`.
- `ramp_slot`: `.neutral[…]` or `accent[<digit>]`; use the semantic tier.
- `legacy_alias`: `.bg_app`, `.bg_primary`, `.bg_hover`, `.bg_active`, the
  removed theme aliases (they no longer compile; the key names them for
  migration).
- `desktop_ground`: `surfaces.base` / `surfaces.window_card` outside the
  desktop. The framework paints the window ground; apps declare regions.
- `literal_radius`: a numeric literal as a corner radius
  (`to_rounded_rect(8.0)`, `RoundedRect::from_rect(r, 6.0)`,
  `fill_round(s, r, 4.0, …)`, `.radius(12.0)`).
- `literal_alpha`: `with_alpha(0.5)`, `with_alpha_factor(x * 0.4)`,
  `multiply_alpha(0.6)`: a fractional literal opacity.
- `raw_paint`: `scene.fill(` / `scene.stroke(`. Chrome is composed from
  widgets; only genuine content drawing paints, from theme values.
- `glyph_icon`: a string or char literal containing `✕ × ⟳ ↻ ⌫ ⇅ « » ‹ › ▾ ▸ ▴
  ◍ ◎ ◷ ⚓ ➤ ⏰` or an emoji (U+1F300–U+1FAFF); use `ui::icons`.
- `transparent_shot`: `render_png(…TRANSPARENT…)`; shots render on the
  framework ground, like the live window.
- `declared_border`: `.border_standard`, `.border_strong`,
  `overlay_border_strong` or `colors.neutral[` on a chrome surface (§9.5).
- `pixel_const`: a module-level `const NAME: f32|f64 = <whole number ≥ 1>;`.
- `taffy_length`: a non-zero literal in taffy's `length(…)`.
- `literal_gap`: `.with_gap(` / `.with_padding(` / `.with_row_gap(` /
  `.with_column_gap(` with a literal.
- `literal_spacer`: `Spacer::fixed(<literal>)`.
- `literal_font_size`: a literal size passed to `TextStyle::new`,
  `.with_size`, `.with_scaled_size`, `.with_fixed_size`, `.text_size`,
  `.with_font_size` or `.set_font_size`.
- `legacy_size_api`: the pixel-size text calls `.draw_text(`,
  `.draw_text_weighted(`, `.measure(`, `.measure_weighted(`,
  `.measure_styled(`, `.elide_to_width(`, `.elide_to_width_weighted(`.
- `font_size_const`: an `f32` constant whose name contains `FONT`, `TEXT`,
  `SIZE`, `TITLE`, `LABEL`, `CAPTION` or `HEADING` (`*CONTENT_PX` document
  sizes excepted).
- `type_step_arithmetic`: arithmetic on `typography.scale.<step>`.
- `raw_type_step`: any `typography.scale.` read or `TypeScale::default()`.
- `renderer_scale_helper`: `renderer_input_style` / `renderer_type_size`.
- `all_caps_label`: a string literal opening with an upper-case word of three
  or more letters (not an acronym such as `PDF`, `HTTP`, `GET`) on a line
  that hands text to a label, title, caption, heading, header, placeholder,
  tooltip, button, `Text` or a draw/measure/elide call.
- `middle_dot_join`: a string literal containing ` · ` (an `A · B · C`
  metadata chain).
- `to_uppercase`: `.to_uppercase()`.

Hard failures, with no inherited debt, in `crates/sdk/ui-core/src`:
`cosmic_text::Color::rgb(<literal>…)`, `.to_rounded_rect(<literal>)` and a
literal shadow `blur:` (`design_token_audit.rs::no_hardcoded_style_literals_in_tree`).

**Baselines are sharded per unit** so app changes never conflict:
`crates/sdk/ui-core/tests/baselines/<audit>/<unit>.txt`, where the unit is
`apps-<name>`, `ui-core` or `shell`. Each line is `<path relative to the
repository root> <key> <count>`. A missing shard allows zero, so a unit that
reaches zero deletes its shard and is locked at zero from then on.

Updating a shard:

- With cargo (CI, or a checkout with a local toolchain):
  `UPDATE_RATCHET_BASELINES=1 RATCHET_SCOPE=apps/mail cargo test -p tilo-ui --test it do_not_grow`
  rewrites only that unit's shards (`RATCHET_SCOPE` also takes
  `crates/session/shell`, `crates/sdk/ui-core` or a unit name). Without
  `RATCHET_SCOPE` every shard is rewritten. An update refuses to raise any
  count unless `RATCHET_ALLOW_GROWTH=1` is also set; set it only after
  reviewing why each count grew.
- With a cargo drop-in that runs tests on a remote host, which neither
  forwards the environment nor brings a test's writes back:
  `cargo test -p tilo-ui --test it do_not_grow --success-output immediate`.
  A ratchet with progress prints the stale entries and the full text each
  affected shard should now hold (or `delete <path>` when the unit reached
  zero). Write exactly that.

**Inlining to dodge a ratchet is a defect.** Moving a literal into a local,
renaming a constant so its name no longer matches, splitting a call over
lines, or inlining "whole-number sizes at their call sites because this file
carries no `pixel_const` to spare" (the justification once written above two
font constants in `apps/git-client/src/app.rs`) hides the debt from the
ratchet without paying any of it. The fix is the semantic API that removes
the knob: a `TextRole`, a spacing token, a region, a widget. Never write a
comment that explains why a value was shaped to avoid a ratchet; a hit that
is genuinely correct (a document colour, a chart's data ink) stays in the
shard with its count.

Not automated yet; grep a diff for these:

- an accent of any spelling: there is none; pick the intent (§9.4)
- `Duration::from_millis(` with a literal in an animation (use
  `tokens::motion`)
- a `Command::new` / blocking read reachable from `draw`

`docs/ui-migration.md` maps each key to the API that replaces it.

---

*This document is versioned with the toolkit. When a guideline here becomes a
`ui` primitive, link the primitive from the relevant section so the rule and its
enforcement stay together.*

## Form composition

For content-first account/settings forms, use `FormPage`, `Form`, `FormSection` and `Field`; see [automatic and guided forms](FORM_COMPOSITION.md) for the API, independent override/reset rules and runnable primitive comparison. Manual composition remains supported with the same standard controls.
