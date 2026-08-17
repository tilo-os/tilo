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
- **Scrolling is smooth for free.** The shared `Scrollable` primitive eases
  wheel/trackpad scrolling toward its target every frame (refresh-rate adaptive,
  fast-flick-aware) and draws a slim overlay progress bar that fades in while
  scrolling and out when idle. Don't hand-roll per-app scroll physics or a
  bespoke scrollbar — wrap content in `Scrollable` and inherit both. It honors
  reduced motion (instant, no momentum) and can opt back to 1:1 stepping via
  `with_smooth_scroll(false)`.

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
> your diff for `Color::rgb`, `draw_*_button`, and `Key::Character => *.push` — each
> hit is either a shared-widget call you skipped or a widget you should be
> extending.

---

*This document is versioned with the toolkit. When a guideline here becomes a
`ui` primitive, link the primitive from the relevant section so the rule and its
enforcement stay together.*
