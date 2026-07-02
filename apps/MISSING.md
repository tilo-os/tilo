# MISSING — open per-app issues to fix before showing again

Rule: **never present an app to the user while it has an unchecked entry here.**
Each item is confirmed fixed (self-verified: build + shot + driven interaction,
graded against `ui/HIG.md`) before its box is checked. Governed by the qOS HIG.

**Review discipline (do not skip):** an item is only checked after DRIVING the
real interaction that exercises it — never off a static empty-state shot. For a
document app that means OPEN a document and actually zoom / pan / annotate; for a
control that means click it and confirm it does the thing. A screenshot of an
empty state proves nothing. (I violated this by showing pdf_viewer from an empty
"No document open" shot without ever driving zoom or open — never again.)

---

## calculator
- [ ] Graph 2D/3D: the per-formula card must **grow in height to fit the formula
      up to a max**, then scroll vertically **within** the card. Vertical
      scrolling exists but does NOT replace an expandable card, and overflow
      detection is unreliable — the **3D-mode default formula is not recognised
      as overflowing** so no scrolling is enabled there. Never clip to hide
      overflow. (HIG §2)
- [ ] Formula editor must be a **real CRDT text editor** via the shared ui core:
      Ctrl+Z/Y undo-redo (currently broken), selection (shift+arrows, drag,
      ctrl+word, select-all), clipboard (ctrl+C/X/V), collaborative multi-cursor.
      Stop hand-rolling the buffer. (HIG §3)

## sheets (spreadsheet)
- [ ] Cell-reference visualisation: for `=A1+B1`, colour `A1` and `B1`
      **differently**, both in the in-cell formula text AND by highlighting the
      referenced cells on the grid in the matching colours (Excel/LibreOffice).
- [ ] Formula-building by click: while editing a cell and after typing `=`,
      clicking another cell inserts its reference (`=A1`), and dragging inserts a
      range (`=A1:B4`); further clicks/drags append more refs.
- [ ] Re-confirm the Excel-style selection (drag-range, shift-extend, ctrl
      multi-range) actually works live — user was unsure what changed.

## weather
- [ ] Move the units (°C/°F) selection to **Settings** — it must not live in the
      weather app UI (raised repeatedly).
- [ ] Layout per HIG §1b: the city list is a **content panel** (contentful), not
      a sidebar. Desktop: city list + forecast side by side. Mobile: full-screen
      city list → tap a city → full-screen forecast with a back button top-left.
      Remove the web-like overlay sidebar here.

## monitor (system monitor)
- [ ] The left column (Processor / Memory / Net / Disk stat cards + host info) is
      **actual content, not a sidebar** — rendering it as a left rail is wrong.
      Re-architect the layout so this is a proper dashboard: e.g. a compact
      summary strip above a full-width process table on the Processes view, with
      the detailed graphs living in the Resources view — no fake sidebar. Think
      it through; make it read as content.

## pdf_viewer
- [ ] ZOOM DOES NOT WORK (raised before, and I failed to record it — now recorded).
      The +/- / Fit buttons and ctrl+wheel must actually change the page scale on
      a LOADED document; plus horizontal pan. Adopt the shared ui DocumentViewport
      (zoom/pan) instead of the app's hand-rolled scale so it behaves like the
      other document apps. VERIFY by opening the sample PDF and driving zoom in/out
      + pan (not an empty-state shot).
- [ ] The open flow draws the app's OWN bespoke `OpenDialog` — WRONG. Open/save
      must go through the **XDG Desktop Portal** (`org.freedesktop.portal.FileChooser`
      via `ashpd`) so the HOST desktop provides the picker: on GNOME the user sees
      GNOME's file picker; on real qOS the compositor's portal backend provides
      qOS's picker (and enforces the sandbox/file-access prompt). The app must NOT
      draw a picker itself (neither the bespoke dialog nor an app-drawn
      ui::FilePicker). Remove the bespoke OpenDialog; call the portal.
- [ ] Re-review ALL previously-claimed pdf features by DRIVING them on a real
      document: zoom, pan, markers/annotations, tooltips, search (whole-word/case),
      sidebar focus, delete — several were "verified" only via static shots.

## calendar
- [ ] Adopt the HIG §1 three-panel model: **calendars** = genuine sidebar
      (collapsible, proper collapse button, overlay-drawer on mobile);
      **overview grid** + **event detail** = content panels. Desktop: all three
      abreast, only the sidebar collapses. Mobile: overview full-screen → event
      slides in from the right + back; only one of overview/detail at a time.
- [ ] Responsive reflow bug: responsive on first launch but STOPS reflowing after
      a resize (breakpoint cached from first frame). Re-derive from current width
      each layout.
- [ ] Narrow view-switcher (Month/Week/Day/Agenda) is pointer-unreachable
      (<980px folds it away; only keyboard works). Give it a pointer path.
- [ ] Week-view period title clips to `Mon 29 Jun –` at 1280px when the detail
      panel is open (the default).

## email (mail)
- [ ] Adopt the HIG §1 three-panel model: **accounts+folders** = genuine sidebar;
      **message list** + **message** = content panels. Desktop three abreast,
      sidebar collapsible; mobile sidebar = overlay drawer, list full-screen →
      message slides over from the right + back.

## chat
- [ ] Expanded call overlay: remote participant tiles run off the RIGHT edge at
      every width (grid sizes each remote to the full host-tile width and starts
      at host.x1). Fix so all tiles fit within the window at every width.
- [ ] Camera-on remote participants get a fabricated linear-gradient "video feed"
      presented as live, while the host honestly shows "Camera off". Make remotes
      honest — no fabricated gradient video. (HIG §4)
- [ ] Attachment chip leaks an internal mount path: `NN KB · shared via
      /var/workspace`. Remove implementation detail from user-facing text.
- [ ] Stale dead comment on `Attachment::Image` (`tint` gradient stand-in) — the
      render path ignores it; remove the misleading comment.

## maps
- [ ] Unbounded failed-tile refetch loop: a failed tile is dropped with no
      negative-cache, so it re-requests every frame and the loop never idles —
      continuous CPU/repaint spin + hammering tile.openstreetmap.org (ban risk),
      even in the offline scenario. Negative-cache failures with backoff. (HIG §5)

## typst_editor
- [ ] Source editor has no horizontal scroll and no word-wrap: typing past the
      pane's right edge pushes caret+text off-screen with no way back. Add
      horizontal scroll (keep caret visible) or word-wrap.
- [ ] Idle spin: redraws every vsync while the editor pane is focused (just to
      blink a caret). Settle to ~0 idle frames. (HIG §5)

## contacts
- [ ] Toasts never auto-dismiss (an Instant is stored but never read for expiry).
      Implement timed dismissal.
- [ ] Detail-pane heading truncates the contact NAME (`Ada Lovela…`) at 800px —
      the primary heading shouldn't truncate at a moderate width.

## totp
- [ ] "Scan a QR image" CTA opens no file picker (only reads an env var) — a
      functional dead-end. Wire a real file picker (portal/compositor capability).
- [ ] Advanced-options section sits below the modal fold with no auto-scroll —
      scroll the revealed fields into view when toggled.
- [ ] TOTP secrets persisted as plaintext JSON — implement encryption-at-rest.

## slides (presentation)
- [ ] Slide thumbnails render body text garbled/overlapping/illegible (min-6px
      font clamp oversizes paragraphs vs the tiny thumbnail; lines collide). Fix
      thumbnail body-text scaling.
- [ ] Present mode (narrow): the slide-counter pill overlaps the "Esc exit" hint.
      Separate them.
- [ ] Overview slide-sorter: the number badge overlaps the slide title. Offset.
- [ ] Present mode redraws the full scene every frame just to tick a 1 Hz clock —
      tick without full-scene re-render. (HIG §5)
- [ ] Keep the crate named `slides` (an agent renamed it to `presentation`;
      reverted — do not re-rename).

---

## toolkit (ui) — enablers
- [ ] Build the HIG §1 navigation primitive (NavShell): a genuine collapsible
      sidebar (docked on desktop with a proper collapse button, overlay-drawer on
      mobile) + a content master/detail region (side-by-side on desktop, push /
      slide-over on mobile with a back affordance). Replace the disliked
      web-style overlay-everywhere sidebar. Apps adopt this, not hand-rolled
      layouts.
- [ ] Shared CRDT caret/selection text-editing core adopted by fields that today
      hand-roll editing (calculator formula field, etc.). (HIG §3)
- [ ] Sweep the O(n²) `doc::store()` CRDT churn across the ~19 text apps. (HIG §5)
- [ ] File open/save is via the **XDG Desktop Portal** (`ashpd`), NOT an
      app-drawn picker: the host desktop provides the UI (GNOME's picker on GNOME;
      the qOS compositor's portal backend on qOS). Provide a small shared helper
      (`ui`) that wraps the portal open/save call so every app uses it uniformly,
      and SWEEP all apps that currently draw their own dialog (pdf_viewer's
      OpenDialog; audit sheets/ebook/code_editor/etc.) onto it. `ui::FilePicker`
      is only for the qOS compositor's own portal-backend rendering (or non-file
      in-cluster pickers), never drawn by an app for OS file open/save.
