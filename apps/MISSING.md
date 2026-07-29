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

## chrome consistency — the OS owns window chrome (fleet, HIG)
The compositor draws the window title/app-name/logo; an app must not paint its own
app-name/logo strip (and video must be immersive, §1e). Swept this pass — each
verified by an offscreen shot (header gone, functional controls intact) + tests:
- [x] Dropped the app-drawn name/logo header from calculator, monitor,
      software_store, themes, video_editor, tube, disks, firewall, logs, totp —
      keeping functional toolbars and promoting any real status line (drive
      summary / firewall state / account count) to primary.
- [x] video-player: edge-to-edge immersive; the persistent 48px bar is replaced by
      a floating title+Open overlay that auto-hides with the controls.
- [ ] Follow-up: route each app's title into OS chrome via `AppWidget::app_title`
      (blocked: the standalone `ui::App` runner doesn't consume it yet; only the
      compositor does, and these apps don't implement `AppWidget`).
- [ ] Follow-up: software_store still shows an app tile+"Software" wordmark inside
      its overlay nav drawer; tube keeps a small play-glyph brand at the nav-rail
      head. Minor, drawer/rail-local — revisit if tightening further.

## calculator
- [x] OS owns window chrome: dropped the "=" logo + "Calculator" wordmark from the
      topbar; mode + RAD/DEG controls take the full width → fixed.
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
- [~] CALLING media pipeline is REAL behind `--features live-media`: GStreamer
      mic → Opus and camera/screen → H.264 (in-tree openh264 source), shipped over
      a bound iroh-QUIC transport; the ScreenCast portal (ashpd) is wired; remote
      NALUs decode into per-peer tiles. Join publishes each peer's dialable ticket
      into the shared voice-membership map (the signalling channel, riding the same
      collab transport as the messages), so peers dial each other with no extra
      round-trip. The default/`--shots` build carries none of that stack and runs
      the call UI against a simulated local bus. REMAINING: a dedicated MoQ-over-iroh
      framing (the transport is raw iroh QUIC today, not MoQ), and end-to-end proof
      of real frames flowing on a live install (unverifiable headlessly).
- [x] VOICE CHANNELS you JOIN (Discord-style): join/leave writes the local member
      into the channel's shared voice membership (CRDT); mute/deafen/audio-only
      toggle real state and mirror into that membership so peers see it; the
      expanded overlay is a responsive participant grid (host-first, reflows by
      count) with per-participant speaking rings, avatars, mic/camera state, and
      the live P2P-vs-host routing surfaced. Runs fully against the simulated bus
      in the default/`--shots` build; real mic flows behind `--features live-media`.
- [x] SCREEN SHARE is a first-class featured stage — REAL: a "Share screen" control
      requests a source through the `ScreenPortal` seam (real xdg-desktop-portal
      ScreenCast via ashpd behind `live-media`; honest `None` in the default build,
      so it never fakes a stream). A granted share flips `call.sharing` and mirrors
      into the shared voice membership (`voice_set_sharing`), so every peer promotes
      the overlay to a presentation stage: a large featured screen tile above a
      single-row participant filmstrip, with a presenter badge on the sharer's tile.
      Remote screen frames route to a dedicated `remote_screens` slot (separate from
      camera), and the featured tile shows a real decoded frame when present, an
      honest drawn-monitor "connecting" placeholder otherwise — never a fabricated
      picture. Proven by tests: `share_screen_gates_on_the_portal_and_mirrors_into_voice_membership`
      (inject a fake portal → share + CRDT mirror; system portal declines → honest
      gating), `a_remote_screen_share_promotes_the_overlay_to_the_presentation_stage`,
      `share_control_reads_the_live_share_state`, `voice_screen_share_round_trips_through_the_shared_membership`,
      and the `filmstrip_rects`/stage geometry tests. Visual proof: the `share`
      `--shots` scenario captures the featured stage + filmstrip + controls.
- [ ] P2P-by-default + APPOINTED-HOST routing does not exist. User's model: direct
      P2P by default, but route a workspace through an appointed host (e.g. a home
      server also running qOS) when one exists. Foundation exists in the compositor
      (session-host designation + "host always wins" for surface sharing); needs a
      persistent self-hosted workspace-host / relay concept built on top.
- [x] Message EDITING now has a user entry point — REAL: right-click your own
      message → "Edit message", or ArrowUp from an empty composer, opens an inline
      editor; commit writes through `ChatDoc::edit_message`, which archives the
      prior body into the message's history list and flags it `edited`. Only your
      own messages are editable. Covered by `edit_via_menu_writes_through_edit_message`,
      `arrow_up_in_empty_composer_edits_last_own_message`, `edit_escape_discards_without_writing`,
      `cannot_edit_another_authors_message`, and a two-peer convergence test
      (`an_edit_and_its_history_converge_across_two_participants`) that proves the
      edited body AND its archived history reach the other peer over the incremental
      delta the broker ships.
- [x] VIDEO attachments — REAL: `Attachment::Video { name, path, w, h, size }` is
      shared by reference (the FUSE path, never the bytes) and renders as an inline
      poster/play card with the real resolution + size. Full frame decode still
      needs the `live-media` media engine, but the share, persistence, and CRDT
      round-trip are real (`video_attachment_round_trips_through_the_crdt`,
      `attaching_a_video_posts_an_inline_video_card`).

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
- [x] Real-time traffic layer (product.md's "crowdsourced real-time traffic over
      OSM"): a togglable congestion overlay in the immersive chrome (a traffic-light
      control in the floating stack — NO sidebar) that colours road segments green →
      amber → orange → red by a `Congestion` signal, with a bottom-left legend
      (per-level counts + provenance). There is no turnkey free real-time-traffic
      feed to wire, so the model is honest: the LIVE app has no peer reports and
      shows "No traffic data" (never fabricated congestion); the `--shots` / sample
      cartography path colours its roads from a deterministic sample set (sub-
      segmented so congestion varies along a road, class-weighted so arterials run
      busy) purely to exercise the rendering, labelled "sample data". Verified on
      the populated 1280px overlay + the empty-state shots; asserted by the
      `traffic::` model tests + the app toggle / honest-empty / click-target tests.

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

## settings — CLEAR (every pane shot + inspected, controls driven against the real stores)
- [x] Sidebar is HIG §1 correct and stays that way: when it fits it is docked
      flush-left with the search field pinned on top and NO collapse button; when
      it doesn't fit (narrow) the list and content become SEPARATE full-screen
      screens with a top-left back arrow — never an overlay/scrim over content.
      Verified all 16 panes wide + the narrow list and narrow content screens.
- [x] Cards sit on one aligned grid in every pane (consistent left/right inset and
      gap) — no "weird rounded cards that don't line up". Verified across all panes.
- [x] Wi-Fi signal icon was a tofu box → fixed: the Unicode block-element bars
      (▁▂▄▆█) have no glyph in the shipped fonts and fell back to a `.notdef`
      box; replaced with the MDI `wifi-strength-{outline,1,2,3,4}` family the
      sidebar's own Network icon uses. Verified by re-render (real signal arcs, no
      tofu) + a unit test pinning the glyph mapping to the MDI range.
- [x] External Accounts no longer leaks the on-disk store path in the UI (the
      "Accounts store: ~/.config/qos/accounts/store.json" caption is removed) —
      HIG §4. Verified by re-render.
- [x] No fabricated sample data in the live app: Users reads the real /etc/passwd
      roster (no Ada-Lovelace fallback) and External Accounts starts empty (no
      illustrative ada@example.com rows) — honest empty states; the sample is kept
      only for tests/--shots. Verified: the live `new()` path over an empty store
      renders empty (unit test with a redirected config dir) and the production
      roster never returns the sample.
- [x] Every pane reads/writes real state, driven and asserted (no mocks):
      Appearance / Region & Formats / Power / Display / Mouse & Keyboard / Updates
      persist to the shared settings.json (prefs_persistence + per-pane sync
      tests); Network / Bluetooth / Sound / Time & Date drive real nmcli /
      bluetoothctl / wpctl / timedatectl through os-actions; External Accounts
      add/remove writes the shared AccountStore + SecretVault (accounts_mail_loop
      end-to-end); About / Users / Printers / Displays read real /proc, /etc, /sys,
      and DRM connectors.
- [ ] Follow-up (not a defect): the committed `apps/settings/shots/*.png` fixtures
      are stale (predate the search-on-top sidebar) — regenerate in a personal-data
      -free environment (they must not bake in a real box's Wi-Fi/BT/email).
- [ ] Follow-up: Permissions & Roles is an honest read-only overview; the embedded
      role/permission editor is still gated on the permissions service.

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
- [x] Right-click context menu on the sheet TABS: Rename / Duplicate / Insert
      sheet / Move left / Move right / Delete + a tab-colour palette → done
      (verified on the `tabmenu` shot; opens on the Budget tab with all rows +
      the colour swatches).
- [x] Excel-style multi-range cell selection: Ctrl+click / Ctrl+drag build a
      discontiguous selection, drag selects a rectangle, Shift+click extends; the
      status bar aggregates Sum/Avg/Count over the *whole* multi-range and the
      name box shows an "N cells" count → done (verified live: B2:B6 + E2:E7 =
      11 cells, status bar Sum $14,750.00 · Avg $1,340.91 · Count 11; tests
      `multi_range_selection_aggregates_numeric_cells_over_the_real_pointer_path`
      drives the real ctrl-drag pointer path and
      `multi_range_selection_summary_aggregates_both_ranges` asserts the
      status-bar summary).
- [ ] Implement a large subset of Excel/LibreOffice FORMULAS (needn't be 1:1; can
      improve where sensible). (The engine is already broad — `formula.rs` — but
      not yet audited for Excel/Calc parity; left open.)
- [x] Reference-coloring also shows when a formula cell is just SELECTED, not only
      while editing → done (verified: E2 selected shows `=SUM(B2:D2)` with `B2:D2`
      tinted in the formula bar AND the B2:D2 range outlined on the grid;
      `selected_formula_bar_shows_colored_body`).
- [x] The top formula edit bar is a real editable field with caret + text
      selection → done (verified on the `fbarselect` shot: the focused formula bar
      shows a selected `B2:D2` reference with the range highlighted on the grid).

## word-processor
- [x] Real Save / Save As / Open via the XDG portal (`ui::portal`). Ctrl+S saves
      to the current path, or — for an untitled document — opens the host Save-As
      picker instead of silently writing `~/Untitled.md`; Ctrl+Shift+S is always
      Save As; Ctrl+O opens a document in place. The chosen extension picks the
      qdoc codec (.docx/.odt/.md/.rtf/.html/…), so a docx opened → edited →
      saved-as writes a real .docx that round-trips → done (test
      `save_as_writes_a_real_docx_that_round_trips` proves the PK/OOXML signature
      + round-trip; `open_pick_replaces_the_document_in_place` proves in-place
      Open). Retired the bespoke image-only portal module for the shared
      `ui::portal` primitive (open + save + filters), dropping the app's direct
      ashpd/tokio/url deps.

## monitor — remaining (overlap + responsive strip already fixed)
- [x] "Copy PID" context-menu action writes the real PID to the OS clipboard via
      `ui::clipboard_copy` (arboard, wayland-data-control) — the toast reads "PID N
      copied to clipboard" and mirrors a genuine clipboard write (silent no-op only
      in a headless/locked-down session). Verified in `apply_action` + the context
      menu shot; asserted by `context_menu_copy_pid_records_pid` /
      `copy_pid_records_and_shows_expiring_toast`.
- [x] Narrow Processes detail push-pane fills the full height: the pushed pane
      renders the command line, an identity line, a large live CPU sparkline and a
      six-cell stat grid (Memory/Virtual/Threads/Open files/Status/Running-for) with
      Stop + Kill pinned to a reserved bottom bar — no void. Verified on the 560px
      `--scenario detail` shot.
- [x] Narrow THR column reads without a horizontal scroll: PID/NAME/CPU%/MEM/THR are
      the essential columns and USER (the droppable text column) is hidden once the
      table can't seat all six — so THR, a core numeric column, is never pushed off
      the right edge. Below even the five essentials the table still overflows and
      scrolls (honest fallback). Verified on the 560px table shot; asserted by
      `narrow_hides_user_so_threads_reads_without_hscroll` / `wide_shows_user_column`
      / `very_narrow_table_overflows_so_it_must_scroll_not_overlap`.

## chat — remaining (call-overlay void + first-run flash already fixed)
- [ ] Floating call-huddle title ("Design Huddle") overlaps the presence dot +
      participant-count badge at narrow width (docked huddle PiP ~164px wide) —
      elide the title to fit (the header topic is already elided; the huddle title
      isn't).
- [ ] Minor perf: ChatApp::needs_redraw() returns true unconditionally → a full
      re-render on every CursorMoved even when no hover/selection changed. Gate it
      on actual state change (not an idle spin, but wasteful under pointer motion).

---

## honesty pass — live path starts real/empty, samples gated to `--shots`

Live launches must show real (or honestly empty) data; fabricated sample
content is reserved for `--shots`/demo builders. Each item build + tested +
shot-inspected (empty state and demo).

- [x] store — the shopfront search box was a dead styled `Container` and the
      source-filter chips were an inert `ToggleGroup` (`on_change` no-op). The
      search box is now a real `TextInput` bound to a CRDT `Text` that filters
      the catalog; the chips are wired through a shared cell that `sync` adopts
      into `active_filter`. Removed the `[All]` bracket rendering on category
      chips and the stray tiny "update" label under cards. Tests drive both the
      typed query and a chip click through the real `sync`. (322a44f5)
- [x] tube — removed the dead "Invidious instance" control: the selected
      instance was discarded in `Request::Search` (real search runs through the
      yt-dlp/InnerTube backend; media resolution uses its own instance list).
      Dropped the row + its "a real control, not a dead ornament" comment, the
      cycle action, the `INSTANCES` table, the Settings "Source" card + field,
      the "sample data" wording on the offline label, and "across your instance"
      copy. Live already starts empty. (818ff6cc)
- [x] chat — live already seeds an honest minimal starter (My Workspace +
      #general + Lounge, local user only), never the fabricated "Acme Design"
      demo. Fixed the guardrail test to pin `QOS_CHAT_DATA` at a temp dir so it
      exercises the fresh first-run seed instead of adopting (or overwriting) an
      on-disk store. (7ab4fc18)
- [x] backups — the constructor loaded `Catalog::sample()` unconditionally
      (58 GB / Seagate / Backblaze / a week of snapshots on live launch). Live
      now starts on an empty catalog with a "No backups configured yet"
      onboarding; the sample moved behind `new_demo()`/`app_root_demo()` for
      `--shots`. Real btrfs/restic backend still a later task. (834b9f49)
- [x] calendar — live already starts empty on the real persisted store
      (`Model::default()`, `demo`-gated seed); the "Design review / Team standup"
      + Work/Personal/Health sample is `new_demo()`/`--shots` only. Verified the
      empty onboarding ("No events yet") renders. (no code change)
- [x] ebook — `load_library()` no longer falls back to the bundled sample book
      ("The Lantern-Keeper's Almanac") when the reader has no epubs; a fresh
      shelf is empty and lands on the existing "No books yet" onboarding. The
      bundled EPUB + multi-book sample stay in the `--shots`/test path. (e73ca2d8)
- [x] ebook — the floating text-selection HUD (already had highlight swatches +
      Note) now has a **Copy** action: it writes the FULL selected passage to the
      OS clipboard via `ui::clipboard_copy` (same primitive pdf_viewer uses), not
      the 160-char annotation excerpt. A bare `c` copies the live selection too
      (mirroring `h`=highlight / `m`=note), and Copy keeps the selection so the
      reader can still highlight/note it. Verified on the wide + narrow selection
      shots; asserted by `copy_action_copies_the_full_selected_passage` /
      `copy_key_shortcut_targets_the_active_selection`. (Text selection / drag-
      select / highlight / annotate / comments already existed; continuous-scroll
      and two-up spread remain — a large refactor of the single-page reader draw,
      deferred.)
- [x] software_store — the constructor always loaded `sample_catalog()`
      (GIMP/Blender with fabricated star ratings), shown live, while the real
      popular-collection fetch landed unused in `live_apps`. A live worker now
      starts with an empty catalog and Browse renders the real Flathub popular
      set (ordered) with a "Loading apps from Flathub…" state; the sample stays
      offline/`--shots` only. (bd2f320c)

## video_editor — real import / decode / export (a real editing engine, now with real media)

The timeline/CRDT editing engine (split/trim/ripple/snap/grade/fade, all
tested) was real, but it could not import real media, never showed real video,
and could not export. Those three seams are now real; each headless-gated part
is honestly gated and proven.

- [x] Real media import — added an "Import…" action (topbar + empty-bin button
      + Ctrl+O) that opens the XDG FileChooser portal (`ashpd`, `portal`
      feature) and brings picked video/audio/image files into the bin as real
      clips. A dependency-free container probe (`src/probe.rs`) reads **real**
      metadata with no decoder — ISOBMFF duration/dimensions/codec/fps (`mvhd`
      /`tkhd`/`stsd`/`stsz`/`hdlr`), WAV channels/rate/duration, PNG/JPEG dims —
      so the default build gets real numbers, not placeholders. Files under the
      `/var/workspace` FUSE mount store a workspace-relative reference (they
      replicate to peers); a local import stores an absolute path. `media::
      resolve` now handles absolute paths. Tests: crafted-byte probe tests
      (mp4/wav/png/jpeg + honest-degrade), and an app test that drives a real
      PNG through `sync` → a real `MediaItem` in the bin with real dims/duration,
      replicated into the CRDT. The old "Media import isn't wired up yet" leak
      is gone.
- [x] Real preview decode — replaced the always-synthesized gradient (and the
      broken openh264-fed-raw-bytes stub) with a `PreviewDecoder` on the shared
      `media::MediaEngine`: it seeks the clip's source time, plays a frame to
      the sink, and pauses to hold it. Behind `--features decode` (→
      `media/decode`) the engine is a real GStreamer `playbin`; the canvas blits
      the true frame (green "LIVE" badge) and falls back to the representative
      frame for an un-hydrated reference. Proven: a hermetic synth-backed seam
      test (top-clip → resolve → hydration-gate → seek/poll → frame), and — with
      `decode`+`gst` — a real round-trip that encodes an H.264 clip in-process
      and decodes a true 192×108 frame back. Visually verified: `--import` a real
      SMPTE clip and the preview shows the real colour bars, not a gradient.
- [x] Real export — `export.rs`'s `gst` path returned "not yet wired". It now
      builds a real `appsrc → videoconvert → encoder → mux → filesink` pipeline
      (`--features gst`), pushing the composited timeline frames (the same
      grade/fade math the preview shows) and writing a real file: MP4/MOV via
      `openh264enc`, WebM via `vp8enc`. It never reports success unless a
      non-empty file exists. The default build stays honest — a "needs the
      GStreamer build" status, no fake progress. Tests: pipeline-construction +
      even-aspect output dims (default build), and a real export that writes a
      valid `ftyp` MP4 (`gst` feature).
- Honesty: capabilities that are feature-gated say so plainly (the import
      notice, the "needs the GStreamer build" export status); nothing fabricates
      progress or a written file. Real gst dep: `gstreamer1-devel` +
      `-plugins-base-devel` + `-plugins-good` + `-plugin-openh264` +
      `-plugin-libav` (dev headers + codec/mux plugins) — installed in
      `my-distrobox`; the default build needs none of it.

## vpn — real WireGuard/OpenVPN transport (connect, status, throughput)

The app already started empty with real `.conf`/`.ovpn` parsers and a portal
import, but Connect was a gated no-op, status was a stored toggle, and the
throughput chart was a synthesized waveform. The transport is now real; the
mutating path stays behind `QOS_VPN_ALLOW_EXEC` and the UI never claims
Connected unless a real command succeeded and the kernel confirms the interface.

- [x] Real bring-up / teardown: importing a `.conf`/`.ovpn` persists the raw
      file (`0600`) into the app data dir and wires it as the run target, so
      Connect runs `wg-quick up <config>` / `openvpn --config <config> --daemon`
      on the exact user config and Disconnect runs the matching teardown. Connect
      only shows Connected after the command exits 0 AND the interface probes up.
- [x] Real status: each frame reconciles the stored status against the true
      interface/process state — `ip link show <iface>` (UP flag) for WireGuard,
      `pgrep -f <config>` for the OpenVPN daemon — throttled to ~1 Hz and gated
      on the exec opt-in, so a tunnel that drops out-of-band flips back to
      Disconnected on its own.
- [x] Real throughput: connected tunnels poll real kernel byte counters
      (`wg show <iface> transfer`, falling back to
      `/sys/class/net/<iface>/statistics/`) and the chart plots rates differenced
      from consecutive real samples. Removed the fabricated waveform generator;
      with no live data the chart is empty with an honest note (never a curve).
- [x] Proven: unit tests for the wg-quick/openvpn argv from a profile, the
      import→persist→bring-up wiring, `wg show transfer` / `ip link` / sysfs
      parsing on real command-output fixtures, real reads against the loopback
      interface, and a real dummy-interface bring-up (netlink status Connected →
      teardown → Disconnected) gated on the exec env var and honest when the box
      lacks the privilege/tools. (bbf44657, 335c9933, 078358e6)
- [ ] OpenVPN throughput is best-effort (kernel-assigned tun device); Tor
      chaining and a live management-socket status remain later work.

## remote — real SSH + VNC transport (Connect actually connects)

The honesty pass already made the live app start empty and show an honest "not
connected" card. The transport itself is now **real** — Connect opens a genuine
session, and on failure shows the real error, never a fabricated terminal or
desktop. A live connection stays gated behind `REMOTE_LIVE_SESSIONS`; the
default build never touches the network and keeps the honest surface.

- [x] Real SSH — `src/ssh.rs` drives a `russh` client on a background tokio
      runtime: real TCP connect, real key exchange, real auth (password via
      `REMOTE_SSH_PASSWORD`, or a private key via `REMOTE_SSH_KEY` /
      `~/.ssh/id_*`), and a real interactive PTY channel. Output is parsed by a
      compact VT/ANSI emulator (`src/vt.rs`, over `vte`) into the terminal grid
      the surface renders; keystrokes flow back into the PTY. Proven by a real
      round-trip test that stands up a `russh` **server** on a loopback socket
      and drives the production client through it (connect → password auth →
      shell banner into the grid → an upstream keystroke echoed back).
- [x] Real VNC — `src/rfb.rs` is a from-scratch RFB (RFC 6143) codec:
      version + security negotiation, `None` and `VNC Authentication` (single
      DES, cross-checked against openssl/pyDes), true-colour pixel formats, and
      the `Raw`/`CopyRect`/`RRE` encodings — decoders take any `Read`, so the
      same path runs on fixtures and on the live socket. `src/vnc.rs` drives it
      over TCP on a background thread into a shared RGBA framebuffer the surface
      paints, with pointer/keyboard injected back. Proven by a real round-trip
      against a loopback RFB server (handshake → decode a known 4×2 framebuffer,
      pixel-exact) plus an honest-failure test (refused connection → `Failed`).
- Honesty: connecting/failed/closed states each render the real status; the old
      fabricated sample transcript + synthetic desktop are deleted. Secrets are
      read from the environment at connect time and never stored in the CRDT.
      Deps (`russh`, `tokio`, `vte`, `des`) build in the default profile; a live
      session still requires the runtime opt-in. Cross-host verification against
      a system `sshd` / external VNC server is the remaining nice-to-have (the
      loopback round-trips already exercise the full real transport).

## imager (Image Writer) — a real media-writer wizard (source → drive → write → verify)

Was a dense three-column dashboard that conflated writing an ISO with installing
qOS. Rebuilt as a linear wizard (Fedora Media Writer / balenaEtcher shaped) with
clear labels: **Image Writer** writes a disk image to a USB drive; installing qOS
onto an internal disk stays the separate `qos-installer` job. Real enumeration,
real gated write, honest empty/progress/verify states.

- [x] Wizard flow with a persistent step breadcrumb (Source → Drive → Customize →
      Write) and a Back / single-primary footer per step. Verify is a phase
      within the Write step (write bar + read-back verify bar), so the crumb
      stays short. Each step graded against HIG §7 on offscreen shots (source
      pick, drive select, drive **empty**, customize, review+confirm, progress,
      verify/done) — spacing, typographic hierarchy, visible selected/armed/
      progress/error state, one clear primary action per screen.
- [x] Real drive enumeration: `lsblk -J -b` parsed into removable/fixed block
      devices (`RM`/`ROTA`/`TRAN`, size, model, partitions, mountpoint→system-disk
      flag). The Drive step shows removable drives only; internal disks require an
      explicit "Show internal disks (dangerous)" opt-in. No drives → an honest,
      composed empty state ("Insert a USB drive", Rescan) — never a fabricated
      list. Parser tested on real `lsblk` JSON fixtures (usb+nvme, string-flag
      variants, zero-size/non-disk skipping).
- [x] Source via the host XDG portal (`ui::portal`, HIG §5) — a picked `.iso`/
      `.img` is inspected for its real size (`stat`) and ISO-9660 volume label
      (PVD at 0x8000, `CD001` magic, volume id at offset 40) to populate the
      source card; no bespoke in-app file dialog. Portal drain + ISO-label parse
      tested (real PVD fixture, foreign-ISO fallback, non-image rejection).
- [x] Real, byte-for-byte write + read-back verify engine (`stream_copy` +
      `verify_readback` in 4 MiB chunks, fsync between). Doubly gated: a typed
      device-name confirmation unlocks the primary, and the *real* block-device
      backend is only selected with `QOS_IMAGER_ALLOW_WRITE=1` (VPN's
      `QOS_VPN_ALLOW_EXEC` precedent). With the gate off — tests, `--shots`,
      normal runs — a simulated worker drives the identical progress UI and
      **touches no device**. Engine proven against temp files (multi-chunk copy
      is byte-exact, verify flags the first differing offset, end-to-end
      Writing→Verifying→Success, bad-dest → WriteFailed); the gated-off planned
      path and the honest empty state are asserted, and no test ever writes a
      disk. (42 tests green: fd98087b, d784d187, b3d36768)
- [x] Guided customization reuses the existing `InstallProfile` (hostname, first
      user, region, options) which doubles as an exportable unattended-install
      config; a plain ISO skips the step and is written as-is.
- [ ] The live progress worker streams `Progress` over a channel drained per
      animation frame; the real `run_system_job` path needs a privileged run
      (root / a polkit helper) to exercise end-to-end on hardware — verified so
      far against files, not a physical stick. Fetching a not-yet-local qOS
      release (the "download" rows) is still a stub.
