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
- [x] Cards grow-to-fit (up to 96px max) then scroll; fractions + 3D default show
      in full, list reflows, never clips → fixed.
- [x] Real CRDT editor: Text-backed diff-splice buffer, undo/redo via ui::State,
      selection + clipboard → fixed (drive-verify undo live before showing).
- [ ] Minor: in Graph 3D the `sin` function name renders oversized relative to
      its `(√x²+y²)` argument — legible + un-clipped but typographically off. Make
      the function name and its parenthesised argument consistent size.
- [ ] Remaining: remote multi-cursor is not rendered inside the math field (local
      structured caret only) — peer carets need mapping into the 2D layout.

## sheets (spreadsheet) — CLEAR (verified, committed b0fe3e2)
- [x] Cell-reference visualisation: each distinct ref in a formula gets a stable
      colour, tinted in the in-cell text + formula bar AND outlined on the grid in
      the matching colour → fixed (verified: `=B2+C2` → B2 blue + C2 red in text
      and as grid outlines; `=SUM(B2:D2)` → blue range rectangle).
- [x] Formula-building by click (point mode): while editing a formula, click a
      cell to insert its ref, drag to insert a range, click after an operator to
      append → fixed (verified: C10 `=B2+C2` built by clicking).
- [x] Excel-style multi-range selection → confirmed live (ctrl-selected B2:B7 +
      E2:E7 = 11 cells, status bar shows Sum $14,750 · Avg $1,340.91 · Count 11).

## weather — CLEAR (verified, committed b0fe3e2)
- [x] Units (°C/°F, km/h/mph) removed from the app; read from the shared Settings
      store (locale.measurement) → fixed.
- [x] HIG §1b content panels via ui::NavShell: contentful city list + forecast
      side-by-side on desktop; narrow = full-screen list → tap city → forecast
      pushes in with a ‹ Back button (never an X). Old overlay-flap removed →
      fixed (verified wide + narrow-pushed-detail). Real Open-Meteo data.

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
- [x] HIG §1 three-panel model: genuine sidebar (calendars/smart-lists/projects,
      chevron collapse never an X) + month/week/day grid overview + event detail;
      three abreast on desktop, only the sidebar collapses; narrow = overlay
      drawer + overview→detail push. Hand-rolled (not NavShell) but well-reasoned,
      HIG-correct, and richer than a generic fill — kept per judgement; its
      collapse behaviour is a candidate to fold into ui. Committed f917913.
- [x] Responsive reflow re-derives the breakpoint each layout pass (test) → fixed.
- [x] Narrow view-switcher (Month/Week/Day/Agenda) is pointer-reachable inside the
      overlay drawer → verified.
- [ ] NOT re-verified this round: week-view period title clipping at 1280px with
      the detail panel open. Check the Week view + fix if it still clips.

## email (mail) — CLEAR (verified, committed f917913)
- [x] Adopted ui::NavShell three-panel: accounts+folders genuine sidebar (chevron
      collapse, not X) + message-list overview + threaded-message detail, three
      abreast on desktop; narrow = overlay drawer + list full-screen → message
      pushes in with ‹ Back → fixed (verified wide 3-panel + narrow pushed detail;
      workflow's verify agent crashed on an API drop, so I rendered + checked the
      shots myself).

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
- [x] Unbounded failed-tile refetch loop → fixed.
- [x] Swap-ends (separate origin/dest + labels) → fixed.
- [x] Transit tab triggers the departures fetch → fixed.
- [x] First-run/offline shows a grid + "Loading map…" affordance → fixed.
- [x] Typeable From/To origin fields + "Use my location" via the location portal
      (org.freedesktop.portal.Location) + transit time picker → fixed (6a9ad41).
      (Location's real coords depend on the host location service + permission —
      wiring is real + unit-tested; live fix unverifiable headlessly.)

## typst_editor
- [x] Horizontal scroll / keep caret in view → fixed.
- [x] Idle spin while editor focused → fixed.

## contacts
- [x] Toasts auto-dismiss → fixed.
- [x] Detail-pane name no longer truncates at 800px → fixed.

## totp
- [x] QR-image import wired to a real picker → fixed. (Follow-up: it must go
      through the XDG portal per HIG §5 — audit in the picker sweep.)
- [x] Advanced options scroll into view → fixed.
- [x] Vault encrypted at rest → fixed.

## slides (presentation) — CLEAR (verified, committed 6c2e54b)
- [x] Thumbnail body-text garble, present counter/hint overlap, overview badge
      overlap → fixed.
- [x] Deck-theme palette pills fit their full labels + wrap to a 2nd row
      (Indigo/Emerald/Amber · Rose/Sky) — no truncation → fixed.
- [x] Responsive toolbar: overflow ⋯ menu keeps every insert tool
      pointer-reachable at 1280/800/560 (verified: clicked ⋯ at 560, all 6 tools
      shown) → fixed.
- [x] Selection handles anchored outside the content rect (+8px) — never cover
      the first glyph → fixed.
- [x] Rail + overview vertical scroll for large decks; action buttons never
      overlapped → fixed.
- [x] Image insert goes through the XDG portal (ashpd FileChooser off-thread),
      renders a real raster; app-drawn FilePicker + mountain-glyph removed → fixed.
- [x] Present mode rests when idle → fixed (caveat: ui::App has no timed-wake
      hook, so present returns false = wait-for-input rather than an autonomous
      1 Hz tick; correct per "not every vsync").
- [x] Crate stays named `slides`.

---

## toolkit (ui) — enablers
- [x] Build the HIG §1 navigation primitive (NavShell) → DONE (ui 13e2928; genuine
      collapsible sidebar docked/overlay-drawer + content master/detail
      side-by-side/push, proper collapse chevron never an X). NEXT: apps must
      ADOPT it — weather, calendar, email, monitor. (Adoption is tracked per app.)
- [ ] (superseded — see the DONE item above) Build the HIG §1 navigation
      primitive (NavShell): a genuine collapsible
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
