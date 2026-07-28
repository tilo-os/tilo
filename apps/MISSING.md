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

## monitor (system monitor) — CLEAR (verified, committed 1571bc5)
- [x] Re-architected: Processes view = a compact CPU/Memory/Network/Disk summary
      strip above a full-width process table (PID/name/user/CPU%/mem/threads +
      selected-process detail + Kill); Resources view = Processor donut + load
      avg, RAM/Swap bars, System info (real host/CPU/cores), Per-Core bars, and
      large CPU/Mem/Net/Disk graphs. No fake sidebar — reads as content → verified.

## pdf_viewer — CLEAR (verified on a real loaded PDF, committed ba0c8ab)
- [x] Zoom adopted onto ui::ViewportState: verified on the 3-page sample —
      page width 306/612/1224px at 50/100/200% (exact 2×), Ctrl+wheel focal zoom,
      h-pan, +/-/Fit + scrollbars. (The old "zoom broken" was a static empty-state
      proof; the real regression was the verification — now driven on a loaded doc.)
- [x] File open via the XDG portal (ashpd, off-thread); bespoke OpenDialog removed.
- [x] Drive-reviewed on the loaded doc: zoom/pan/highlight/search(whole-word+case)/
      sidebar/delete all work; removed the demo ellipse over whitespace → markup
      anchored to real text.

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
- [x] Week-view period title no longer clips — reads "29 Jun – 5 Jul" in full at
      1280px with the detail panel open → verified.

## email (mail) — CLEAR (verified, committed f917913)
- [x] Adopted ui::NavShell three-panel: accounts+folders genuine sidebar (chevron
      collapse, not X) + message-list overview + threaded-message detail, three
      abreast on desktop; narrow = overlay drawer + list full-screen → message
      pushes in with ‹ Back → fixed (verified wide 3-panel + narrow pushed detail;
      workflow's verify agent crashed on an API drop, so I rendered + checked the
      shots myself).

## chat — UI defects CLEAR; deeper FEATURE gaps below (audit 2026-07-04)
- [x] Call-overlay tiles packed into a bounded region (grid picks the column count
      that fits N; partial last row centred) — no right-edge overflow at any width
      (verified 1280/800/560) → fixed.
- [x] Remotes are honest — every tile shows an avatar + caption ("Camera off" /
      "No video"), no fabricated gradient video (HIG §4) → fixed (verified).
- [x] Attachment chip shows only "TYPE · SIZE" (e.g. "MD · 18.5 KB") — no
      /var/workspace path (test asserts no '/', 'workspace', 'shared via') → fixed.
- [x] Stale `tint` gradient stand-in comment removed → fixed.
- [x] Emoji reactions — REAL (model + Automerge CRDT + UI, cross-peer convergence
      tested). Image attachments — REAL (ui::ImageStore + real FUSE fs.read; the
      synthesized `qos-demo://` images are shots-only). No action.
- [ ] CALLING IS UI-ONLY (not real): the overlay is view-state + a join/leave beep.
      NO camera/mic capture, NO opus/H.264 codec, NO transport — IrohTransport
      never binds an endpoint (media.rs:232), connect()=false, send() loops back
      in-process, so no peer ever receives a frame. request_screencast() is a None
      stub (media.rs:189). To make it real: (a) capture via portal ScreenCast +
      a pipewire/GStreamer mic source; (b) opus (audio) + H.264 (video) codec;
      (c) transport — EITHER reuse the compositor's REAL H.264 pipeline
      (shell/src/network/encoder.rs + peer_view.rs over gossipsub, already has
      host-wins arbitration + signed invite tokens) OR bind the iroh QUIC seam —
      pick ONE and wire it end-to-end.
- [ ] P2P-by-default + APPOINTED-HOST routing does not exist. User's model: direct
      P2P by default, but route a workspace through an appointed host (e.g. a home
      server also running qOS) when one exists. Foundation exists in the compositor
      (session-host designation + "host always wins" for surface sharing); needs a
      persistent self-hosted workspace-host / relay concept built on top.
- [ ] Message EDITING has no user entry point: the CRDT writer `edit_message` +
      the read-only "(edited) · view history" popover exist, but nothing calls
      edit_message outside tests/seed (doc.rs:1143). Add a context-menu "Edit" /
      ArrowUp-to-edit-last-message that calls ChatDoc::edit_message (doc.rs:593).
- [ ] VIDEO attachments ABSENT: Attachment is only Image|File (model.rs:140); a
      shared video renders as a plain file chip — no poster, no inline playback.
      Add a Video variant + inline player (depends on the ui video core below).

## video playback (ui layer) — REAL but SILOED (audit 2026-07-04)
- [ ] The working decode (NoFlush single persistent decoder) lives ONLY in tube
      (apps/tube/src/playback.rs). The shared ui::widget::VideoPlayer still flushes
      after every decode → drops ~95% of a real stream's frames (playback.rs:13).
      Extract tube's robust decode into a shared `ui` video core (or fix
      ui::VideoPlayer) so ANY app (chat, a general player, file preview) gets real
      playback, not just tube.
- [ ] ui::VideoPlayer is not production-grade: no container demux (caller must feed
      Annex-B NALUs), no fetch, no audio, placeholder 60s scrub with the thumb
      pinned to start (video_player.rs:118,336,462). Needs a media-length setter +
      a playhead clock for real seek.
- [ ] No general video-player app; no inline video in chat or file-explorer preview.
      Tube's watch page is the only video UI and it's `--features live`-gated +
      network-dependent (offline/hermetic build shows posters only —
      OfflineBackend::resolve_video_stream errors, backend/mod.rs:166).
- [ ] No AUDIO anywhere in the video stack (tube is video-only by design) — a real
      player needs an audio track + A/V sync. No hardware decode (openh264/CPU only;
      OK for a 144p probe, insufficient for real playback resolutions).

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
- [x] Source-editor cut/copy/paste used an app-local buffer, so text couldn't
      move to/from other apps or the rendered preview → now goes through the
      OS system clipboard (paste normalises CRLF + strips stray control codes).
- [x] No word-wise editing → Ctrl+←/→ hop by word and Ctrl+Backspace/Delete
      remove a word (shared `ui` word-boundary helpers).
- [x] A failed recompile blanked the preview and lost the reader's place →
      the last successful render stays on screen under a "showing last
      successful render" banner until the next clean compile.

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

## camera
- [x] Killed the left desktop nav rail (Camera/Scan/Gallery/Settings + app
      aperture logo). The camera is now an immersive, full-bleed app: the
      viewfinder fills the window edge-to-edge and every control floats over it —
      capture-setting pills (MP/HDR/Timer/grid) top-left, a settings gear
      top-right, a Photo/Video/Scan switcher bottom-centre, the shutter, a
      last-shot thumbnail bottom-left (tap → Gallery), and a lens flip
      bottom-right. Gallery is a push with a ‹ back button; Settings is an
      overlay sheet — neither is persistent rail chrome. Verified on the camera,
      scanner, gallery and settings shots (1280×800).
- [x] No fake content in the live path: the disk-backed library starts EMPTY
      ("No photos yet — Take a photo and it'll show up here"); the 9 sample
      photos exist only behind `--shots`/`--emit-photos` (Storage::Sample).
      Verified: live empty-dir gallery shot shows the empty state, hermetic shot
      shows the grid + detail laying out. (`disk_source_starts_empty_and_seeds_no_content`.)
- [x] Settings is a proper centred overlay sheet (scrim + close + Escape),
      full-width grouped rows (Capture / Composition / Privacy & camera) on a
      base-surface sheet with raised cards — no longer a narrow centred column
      stranded in an empty pane. Scrolls on short windows (verified 900×560).
- [x] Live v4l2 capture unchanged: `capture.rs` untouched; the
      `#[cfg(feature = "live-capture")]` poll/enumerate/lens-selector path in
      `app.rs` references only stable APIs and was not modified. (Could not
      compile-check the `live-capture` feature in this box — gstreamer/glib
      devel not provisioned — but the code path is byte-for-byte unchanged.)
- [x] Codified the lesson in `ui/HIG.md` §1e: an immersive / full-bleed app
      class (camera, video player, maps, games) that does NOT get the content-app
      sidebar — edge-to-edge surface, floating/overlay controls, sheet-based
      secondary screens.

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
- [x] SWEEP done: every app that drew its own OS file picker now uses the XDG
      portal (ashpd) — pdf_viewer, slides, diffmerge, totp migrated; chat + maps
      already portal. No app-drawn OpenDialog/ui::FilePicker for OS file open/save
      remains (scout confirmed only those 4 offenders).
- [ ] FOLLOW-UP (dedup, not a bug): the 6 apps each carry a near-identical copy of
      `portal.rs`. Promote a shared `ui::portal` open/save helper and collapse the
      copies onto it (same "recurring pattern → framework" move as ui::doc).

---

## Open issues from a live desktop review (2026-07-04)

### ui (general, cross-app)
- [ ] Scroll bars aren't draggable: thumb + track don't respond to click/drag
      (seen in monitor's data table). Make scrollbars fully interactive (thumb
      drag + track paging) in the shared ui scrollbar/DataTable so ALL apps get it.

### maps
- [ ] Live tiles never load — always "Map imagery unavailable offline". Root cause:
      rustls 0.23 with BOTH ring + aws-lc-rs and no installed default CryptoProvider
      → every HTTPS fetch fails silently. (curl 200 from the same env proves the
      network is fine.) Fix started (install ring provider in main + rustls dep) but
      UNVERIFIED — needs a REAL-NETWORK tile-fetch test; unit tests hit a local http
      mock + shots use sample data, so neither caught it. Likely workspace-wide
      (weather/tube/music use reqwest+rustls too — audit + fix all).
- [ ] UX: fold Transit INTO Directions as a travel MODE (Drive/Cycle/Walk/Transit),
      not a separate tab; make clear what "Explore" is (search/browse) vs Directions.

### monitor
- [ ] Overlapping elements at very low width.
- [ ] The top resource-metric strip doesn't grow/reflow with the window width.

### weather
- [ ] Search UX is bad: only a "Searching…" text label appears below the bar —
      needs a real results dropdown you can pick from.
- [ ] The city list isn't collapsible — feels off; make it collapsible.
- [ ] Polish across widths (review at several widths).

### sheets
- [ ] No right-click context menu on the sheet TABS at the bottom.
- [ ] Implement a large subset of Excel/LibreOffice FORMULAS (needn't be 1:1; can
      improve where sensible).
- [ ] Reference-coloring only shows while EDITING a cell — should also show when a
      formula cell is just SELECTED.
- [ ] The top formula edit bar has no caret / text selection — give it a real
      editable field (shared caret core).

## monitor — remaining (overlap + responsive strip already fixed)
- [ ] "Copy PID" context-menu action is a no-op stub — reports "PID N copied" but
      writes to NO system clipboard. Wire a real clipboard write (or remove the lie).
- [ ] Narrow (≤680px) Processes detail push-pane wastes ~70% vertical height: the
      96px-strip draw_detail is handed a full-height rect (name/PID top, chips+Kill
      bottom, huge void between). Give it a proper full-height narrow layout.
- [ ] Narrow 560 THR column header clipped to "T" (needs h-scroll to read) — a core
      column; widen/prioritise it at narrow.

## chat — remaining (call-overlay void + first-run flash already fixed)
- [ ] Floating call-huddle title ("Design Huddle") overlaps the presence dot +
      participant-count badge at narrow width (docked huddle PiP ~164px wide) —
      elide the title to fit (the header topic is already elided; the huddle title
      isn't).
- [ ] Minor perf: ChatApp::needs_redraw() returns true unconditionally → a full
      re-render on every CursorMoved even when no hover/selection changed. Gate it
      on actual state change (not an idle spin, but wasteful under pointer motion).
