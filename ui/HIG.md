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

*This document is versioned with the toolkit. When a guideline here becomes a
`ui` primitive, link the primitive from the relevant section so the rule and its
enforcement stay together.*
