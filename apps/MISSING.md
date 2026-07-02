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
- [x] Unbounded failed-tile refetch loop → fixed (negative-cache + backoff; loop
      idles once tiles resolve).
- [ ] Directions "swap ends" (⇅) is broken: it sets origin = the selected place,
      but destination is still read from `selected_place()`, so origin==dest and
      it routes the destination to itself (0 min / 0 m); the "from" label stays
      stale. Give directions a proper separate origin AND destination so swap
      actually swaps.
- [ ] Transit tab: clicking the Transit tab only switches the view and never
      triggers a departures fetch, so it always shows "No itineraries found" as if
      an empty result — actually run the lookup when the tab is selected.
- [ ] First-run shows a fully blank dark map (no basemap/grid, no loading
      affordance) until OSM raster tiles arrive; if egress is blocked it's a black
      void. Show a basemap/grid + a loading/imagery affordance.

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

## slides (presentation)
- [x] Thumbnail body-text garble, present counter/hint overlap, overview badge
      overlap → fixed in the last pass.
- [ ] NO SCROLLING anywhere (rail + overview): handle_event ignores MouseWheel and
      neither the rail-thumbnail loop nor the overview applies a scroll offset — a
      deck with more slides than fit vertically loses access to them (overview at
      800x600 cuts off slide 5; 560px cuts off slides 3-5, on the 5-slide sample).
      Add scrolling to the rail and the overview.
- [ ] Thumbnail rail overflows for larger decks: thumbnails lay out unbounded
      while the Add/Dup/Del actions are pinned to the bottom, so a deck beyond ~6
      slides overlaps the pinned buttons and runs off-screen — the rail is the
      primary edit-mode nav, so this breaks real decks. Make the rail scroll.
- [ ] The "Image" toolbar tool is a non-functional placeholder (no asset pipeline
      / picker; only paints a framed box with a mountain glyph). Wire real image
      insertion via the XDG portal (HIG §5), or remove the tool until it works —
      do not ship a primary insert tool that can never show a real picture.
- [ ] Present mode never rests (needs_animation_frame always true in Present) —
      re-renders at vsync just to tick a ~1 Hz clock. Wake at 1 Hz, don't spin.
      (HIG §5)
- [ ] Keep the crate named `slides` (an agent renamed it to `presentation`;
      reverted — do not re-rename).

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
