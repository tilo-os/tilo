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
      themes, video_editor, tube, disks, firewall, logs, totp —
      keeping functional toolbars and promoting any real status line (drive
      summary / firewall state / account count) to primary.
- [x] video-player: edge-to-edge immersive; the persistent 48px bar is replaced by
      a floating title+Open overlay that auto-hides with the controls.
- [ ] Follow-up: route each app's title into OS chrome via `AppWidget::app_title`
      (blocked: the standalone `ui::App` runner doesn't consume it yet; only the
      compositor does, and these apps don't implement `AppWidget`).
- [ ] Follow-up: tube keeps a small play-glyph brand at the nav-rail head. Minor,
      rail-local — revisit if tightening further. The redundant `software_store`
      app was consolidated into `store`, whose chrome uses shared widgets.

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

## weather — CLEAR (verified, committed b0fe3e2; honesty + §7 empty-state pass)
- [x] Units (°C/°F, km/h/mph) removed from the app; read from the shared Settings
      store (locale.measurement) → fixed.
- [x] HIG §1b content panels via ui::NavShell: contentful city list + forecast
      side-by-side on desktop; narrow = full-screen list → tap city → forecast
      pushes in with a ‹ Back button (never an X). Old overlay-flap removed →
      fixed (verified wide + narrow-pushed-detail). Real Open-Meteo data.
- [x] Live path is real, verified end-to-end: the geocoding + forecast endpoints
      return live data matching the parser (Berlin 29.8 °C / code 0), the net
      worker fetches on its own tokio+reqwest thread, and parse/URL building are
      unit-tested. `--shots` renders bundled sample data offline (never the net).
- [x] Honesty: `live()` no longer seeds a curated set of world cities (SF / Tokyo
      / Reykjavík / Cairo …) into the user's doc on first run — it starts with the
      user's OWN saved places only (empty on a fresh vault), and the bundled
      sample cities/readings are `new()`/`--shots`-only. Errors surface on the
      status line; a place-less start shows no phantom "Loading…" spinner.
      (`live_starts_empty_with_no_fabricated_places_or_readings`.)
- [x] §7 empty state: the detail pane's bare "No place selected" on a big empty
      gradient is replaced by a composed, centred zero-state — a partly-cloudy
      weather glyph + "No cities yet" + "Search for a city to see its current
      conditions and 7-day forecast." (an existing roster with nothing selected
      reads "Pick a city"). Verified on the `onboarding` shot; the populated wide
      view is byte-unchanged.

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
- [x] There is now a general `video-player` app on the shared `media::MediaEngine`
      (real GStreamer `playbin`→`appsink` under `--features decode`). It opens a
      real local/`/var/workspace` file via the XDG portal (or `--open`), demuxes
      the container, decodes, and plays with real play/pause/seek/volume/mute and
      a real duration+playhead. Proven: `--features decode` decodes a synthesized
      real file with advancing PTS and lands a frame on the app's surface
      (`real_decode.rs`, ran green — not skipped — against GStreamer 1.26), and a
      `--open realclip.ogv` capture shows genuine decoded SMPTE bars, not the
      synth gradient. (Inline video in chat / file-explorer preview: still open.)
- [ ] No AUDIO anywhere in the video stack (tube is video-only by design) — a real
      player needs an audio track + A/V sync. No hardware decode (openh264/CPU only;
      OK for a 144p probe, insufficient for real playback resolutions).
      (Note: the `media` crate now carries an `audio` module + PipeWire device
      enumeration; end-to-end A/V-synced playback in the app is still unproven.)

## video-player (local) — CLEAR (real GStreamer decode proven, immersive §1e)
- [x] Plays a REAL local file: `media::MediaEngine` demuxes + decodes through a
      GStreamer `playbin`→`appsink` (`--features decode`); the hermetic default
      build uses a deterministic synthesized backend so `--shots`/tests stay
      offline. Verified NOT skipped on GStreamer 1.26: `engine_decodes_a_real_
      file_and_the_pts_advances` + `player_app_pulls_a_real_decoded_frame_onto_
      its_surface` both bite (real RGBA frames, advancing PTS, 160×120 at the
      encoded size); a `--open realclip.ogv` capture renders genuine SMPTE bars
      with a real "0:00 / 0:02" duration — a real decoded frame, not a gradient.
- [x] Immersive (§1e): edge-to-edge surface, NO persistent chrome bar; the media
      title + Open pill float over a soft scrim and auto-hide with the controls.
      Real glass control bar: play/pause, seek scrubber, time, volume + mute,
      priority setting icons + a gear menu, fullscreen. Open goes through the XDG
      FileChooser portal (HIG §5), never an app-drawn picker.
- [x] Honest empty state: the LIVE path (`app_root(None)`) starts on "No media"
      with no frame — the demo clip is `--shots`/`for_shot` only (unit test
      `fresh_player_shows_a_clean_empty_state_with_no_frame`). Real controls
      driven + asserted: seek holds mid-play, volume/mute reach the engine, Space
      toggles playback through the app's own event path.

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

## typst_editor — CLEAR (re-verified: every interaction driven + shot-graded)
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
- [x] Preview zoom / pan / scroll / selection all drive the real shared
      viewport kernel (Ctrl+wheel zooms toward the cursor, ± and Fit, drag-pan
      when zoomed past fit, plain/tilt wheel scroll, glyph-precise text select +
      Ctrl+C copy) — verified by shots (100%/190% zoom, horizontal-scroll caret,
      heading selection) and the preview::/app:: interaction tests.
- [x] Compiler diagnostics UX is real: red/orange squiggles under the offending
      spans, gutter dots, a Problems panel listing `line:col` + severity swatch,
      and clicking a row jumps the editor caret to that diagnostic
      (`diagnostic_click_jumps_editor_for_both_severities` on the production
      `goto_line` path). Failed compile shows an honest "Compilation failed"
      preview instead of blanking.
- [x] PDF export is REAL (the earlier "no export" gap is closed): the toolbar
      "Export PDF" writes a real `.pdf` next to the source via `typst_pdf::pdf`
      over the same `World` the preview uses (`export_pdf_writes_a_real_pdf_file`
      asserts the file opens with the `%PDF` magic).

## git_client — CLEAR (real git2 end-to-end: rendered + mutated a live repo)
- [x] Every surface reads a real repository, not stubs: opened a prepared temp
      repo via `GIT_CLIENT_REPO` and the Changes/History/Branches views rendered
      its actual state — staged/unstaged/untracked with real M/A/R/D badges and
      `+/-` counts, the real commit graph (HEAD/main/branch refs, real hashes),
      the real branch/tag/remote/stash lists, and the author read from the repo
      config. Honest empty state ("No repository open" + an Open-path input) when
      no repo is reachable — never a fabricated repo.
- [x] Mutating operations are real `git2`, driven end-to-end: `--stage-commit`
      staged all four pending changes (new/deleted/modified) and wrote a real
      commit to the temp repo (working tree went clean). The 26-test `repo_ops`
      integration suite drives stage/unstage (file + hunk), commit (rejects empty
      message / missing identity), branch create/checkout/delete (refuses HEAD),
      discard file/hunk, stash save+apply, and fetch/pull against a local remote —
      all through the production actions the UI buttons invoke. Push is real
      `git2` (credentials + refspec).
- [x] Dead-label fix (HIG §7): the top-right diff option toggled syntax
      highlighting but was labelled with the cryptic fragment "Syn" — replaced
      with the universal `</>` code glyph, centred in the pill.

## disks — CLEAR (reads real block devices; mutations real + honestly gated)
- [x] Reads REAL storage, never fabricated: `--live` probes this machine via
      lsblk/sysfs and rendered its actual drives (both NVMe SSDs, the real
      partition label, zram devices, real capacities/usage); SMART shows an
      honest "Unknown — needs administrator access" rather than inventing values.
      The offline sample topology is used only for `--shots`/tests and is labelled
      as such (`probe_returns_none_or_real_devices_never_fabricates`,
      `drives_real_probed_topology_end_to_end`).
- [x] Mount / unmount / format / eject / benchmark are real and correctly gated:
      the live binary runs udisks2 over D-Bus (feature `live`) → `udisksctl` →
      raw `mount`/`umount`/`mkfs.<fs>` fallback, plus a genuine read/seek
      benchmark; every executing path is runtime-gated on `DISKS_LIVE` so
      `--shots`/tests stay inert and state only what *would* run. Destructive
      Format is behind a type-the-device-name confirm gate.
- [x] Honesty fix: the Format confirm body unconditionally claimed
      "(Demo build — no changes are written to disk.)" — but the shipping live
      binary sets `DISKS_LIVE` and really erases the device, so the reassurance
      was false and dangerous there. Now the clause appears ONLY in the hermetic
      demo build; the live confirm just warns the erase is permanent
      (`format_confirm_body_is_honest_about_live_vs_demo`).
- [ ] Follow-up (pre-existing, fleet-wide, not a defect): two taffy
      `percent()` deprecation warnings (rust #154024) in the lib — swept when the
      toolkit's taffy usage is normalised across apps.

## contacts — CLEAR (verified live: driven add persists to the real CRDT)
- [x] Toasts auto-dismiss → fixed.
- [x] Detail-pane name no longer truncates at 800px → fixed.
- [x] Live-verified end-to-end: driving the real widget (open + New, type a
      first/last name, Save) writes a real contact into the CRDT with the full
      schema (first/last/org/title/phones+labels/emails+labels/address/birthday/
      website/notes/favorite), and it survives a restart (second process reloads
      `dev.qos.contacts/state.automerge` and still shows it). List/detail render
      real state; empty state is honest ("No contacts yet"); narrow width uses
      the §1b push flow (full-screen list → detail slides in with a Back button).

## osk (on-screen keyboard) — verified real; §7 tofu fix
- [x] Real key output: taps/swipes emit `KeyAction`s that a real
      `virtual-keyboard-v1` injector types into the focused surface (synthetic
      XKB keymap over a memfd), or commit via `input-method-v2` in auto-show
      mode. Verified: the generated keymap compiles with libxkbcommon and each
      keycode resolves to the intended keysym; pointer tap → 'q' typed; shift /
      caps / symbols / backspace / space all drive the real text path (not a
      dead grid). Injection degrades to a null recorder only when no compositor
      protocol is present.
- [x] Real FUTO swipe: the geometric SHARK² decoder ships as the default and a
      real `FutoSwipeModel` (ExecuTorch over the `swipe` crate FFI) is used
      automatically when its libs + models are present — same `SwipeModel`
      seam, honest fallback. Next-word + completion suggestions are a real
      offline n-gram predictor (the bar shows cold-start "the/of/and/to").
- [x] §7 key grid: the ⇧ / ⌫ / ⏎ modifier keys rendered as `.notdef` tofu boxes
      (their Unicode symbols have no glyph in the shipped UI font). Replaced
      with vector lucide icons (`arrow-big-up` / `delete` / `corner-down-left`,
      added to `ui::icons`); the Shift icon fills solid while Caps-Lock is
      latched so a sticky upper-case reads differently from a one-shot shift.
      Verified on the QWERTY/phone/pin shots (icons crisp, no tofu); asserted by
      `modifier_and_edit_keys_carry_icons` on the production layout data.

## totp — CLEAR (verified real RFC 6238 + real vault + §7 token list)
- [x] QR-image import wired to a real picker → fixed. QR scan + paste-otpauth
      cards both go through the XDG FileChooser portal (HIG §5) — asserted by
      `clicking_the_scan_qr_card_emits_pick_image` +
      `portal_chosen_qr_image_fills_the_add_panel_via_frame_drain`.
- [x] Advanced options scroll into view → fixed.
- [x] Vault encrypted at rest → fixed. Real persistence verified:
      `save_then_load_round_trips_the_vault` + `plaintext_vault_is_migrated_and_
      removed` (XChaCha20-Poly1305), and `doc::sync_vault_removes_deleted_accounts`.
- [x] Real RFC 6238 codes from real stored secrets: HOTP dynamic truncation on
      RustCrypto HMAC + SHA1/256/512, RFC 4648 base32 decode, key zeroized after
      derivation. Proven by the RFC 6238 Appendix B test vectors
      (`rfc6238_sha1_vectors`: 94287082 / 07081804 / 89005924 / 69279037) and
      `code_is_stable_within_a_period_and_rotates_after`. Invalid secrets show a
      masked •••••• row, never a fabricated code.
- [x] Add / remove / copy / reorder driven on the production path (not seeded
      shots): `adding_an_account_through_the_panel_grows_the_vault`,
      `delete_event_removes_account`, `mouse_click_in_the_copy_zone_emits_a_copy_
      event`, `mouse_clicks_in_the_action_column_emit_reorder_and_delete`. Ctrl+N
      opens Add. Live path loads the persisted vault only — never fabricates
      accounts (sample is `--shots`/demo).
- [x] §7 token list: per-account colour monogram, name + username hierarchy, the
      grouped code in tabular figures, and a live per-row countdown ring
      (seconds-remaining + colour arc) — state reads at a glance. Empty state is a
      centred "No accounts yet — press + or Ctrl+N". Verified on the list / empty /
      add / add-advanced shots.

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
- [x] Permissions & Roles now renders the **real** per-workspace app grants the
      compositor persists to `~/.config/qos/state.json`, not a hand-mirrored
      description. The pane depends on the shareable `collab-types` types crate
      (NOT the compositor) to deserialise each workspace's `capabilities`
      (`CapOverride`) and groups them per workspace → app → capability, with a
      meaning-carrying tinted pill per kind (Files/Camera/Microphone/Screen
      sharing/Remote control/Network) and a trailing lifetime chip
      (Always / Until log out / Once / Temporary). Read-only by design (granting
      is the compositor's consent prompt; revoking is done in the owning
      workspace) so it still ships zero dead buttons. Honest empty state when no
      app holds an out-of-sandbox grant (or the compositor hasn't written its
      state file yet) — never a fabricated permission. Verified on populated +
      empty shots; asserted by the projection/render tests (real `CapOverride`
      fixtures, `no dead buttons`, `a pill per grant`, `honest empty`).
- [ ] Follow-up (unchanged): in-pane granting/revoking is still deferred — that
      needs a write path back to the compositor's grant model, out of scope for a
      read-only Settings pane.

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
- [x] On-canvas text + speaker-notes editing adopt the shared `ui`
      caret/selection core (`ui::text_selection`): shift+arrow extends,
      ctrl+arrow is word-wise, Ctrl+A selects all, Ctrl+C/X/V use the shared
      clipboard, Delete forward-deletes, typing/paste replace the selection —
      all splicing the real CRDT (no per-app text buffer). Click-to-place caret,
      double-click word-select and drag-select on the canvas text box; selection
      highlight rendered. Undo/redo already handled by the toolkit. Tests assert
      each on the real key path into the CRDT.
- [x] Drag-to-delete: a red dashed "Drop to delete" zone pinned to the bottom of
      the slide rail, live only while a thumbnail is dragged; dropping there
      removes that slide from the CRDT (test drives on_press → drag → release).
      Slide reorder + context menu (add/duplicate/move/delete) already via the
      shared `PageSidebar`.
- [x] Present mode is genuine borderless fullscreen via `ui::fullscreen`
      (`FullscreenRequest::distinct(Largest)`); the App drives
      `set_fullscreen(Borderless)` and drops present mode on a compositor-side
      un-fullscreen. Verified in shots (present / presenter / notes / white-out).
- [x] §7 visual pass on the editor, filmstrip and present surfaces (spacing,
      typographic hierarchy, visible selection/active state, honest empty state).

## camera — CLEAR (live-verified: honest gating, real library + QR decode, live controls)
- [x] Live-verified end-to-end: with no capture device the viewfinder shows an
      honest "No camera connected" card and the shutter is visibly disabled — no
      fabricated frames. The disk-backed library is honestly empty ("No photos
      yet"); the sample grid + metadata detail render only under `--shots`. The
      scanner really decodes a QR (the sample Wi-Fi poster parses into a
      "qOS Lounge / WPA2" credential card with Connect/Copy/Save). Driving the
      bottom mode switcher flips the real CRDT `screen` field
      (camera → scanner), so the immersive controls are live, not decorative.
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

## snippets — real CRDT snippet manager; live now starts empty (§7 CLEAR)
- [x] Live launch was fabricating a 15-snippet starter library in
      `SnippetsApp::new()` and mirroring it into the real persisted CRDT store
      (`ui::persist::open("dev.qos.snippets")`) — a fresh install wrote sample
      content it kept. Fixed: `new()` starts on an **empty** library; the starter
      set moved behind `new_demo()` (used by `for_shot`/`app_root_for_shot`), and
      `sync_library` now writes nothing into the shared doc when local is empty.
      Asserted by `live_app_starts_empty` + `empty_live_app_does_not_seed_the_shared_doc`;
      the seeding path is proven by `demo_seeds_and_opens_a_snippet` /
      `sync_seeds_into_empty_doc` on `new_demo()`.
- [x] Composed honest empty state: a genuinely empty library renders an inviting
      onboarding (glyph tile, "No snippets yet", a primary **New snippet** CTA, and
      a search-grammar tip) instead of the bare "No snippets match." line; a
      filtered-empty result gets a "No snippets match" note with a Clear-search
      button. Detail pane reads "Your snippets will appear here." when empty.
- [x] §7: fixed the top-left toggle overlapping the title in the narrow layouts —
      the overlay sidebar's "Snippets" title and the push-detail's snippet title
      now indent past the close-X / Back chevron so glyphs never collide.
- Everything else was already real and strong: three-pane search/list/editor,
      syntax-aware highlighted body with a line gutter, language sigil badges +
      per-language tints, favorites/most-used smart folders, `language:`/`tag:`
      search, `${placeholder}` fill-and-copy expand flow, real copy-to-clipboard
      (bumps usage), duplicate/delete/star, diff-spliced Text CRDT persistence
      (bounded-churn tests), and a shared responsive flap sidebar. Verified on
      populated + empty + narrow-overlay + push-detail shots.

## themes — real OS theme studio that actually applies (§7 CLEAR)
- [x] Apply is real: it persists the selected `ThemeSpec` into the shared
      `~/.config/qos/settings.json` `appearance` section — the same file the
      compositor and every system app read at session start — via a
      non-destructive merge that preserves foreign sections. Proven end-to-end
      by `apply_command_writes_the_active_theme_to_the_shared_store` (drives
      Apply through the real sync, then reads the id + full spec back off disk).
- [x] Honesty: the live catalog no longer seeds a fabricated "Sunset (custom)"
      user theme. `default_catalog()` is now the built-in **presets only** (the
      OS's shipped themes — real product content, not user content); the editable
      custom theme moved to `demo_catalog()` used by `--shots`/tests. Live focuses
      the first preset (read-only customizer, "Duplicate to edit"); the user
      authors their own via New/Duplicate and they persist in the CRDT. Asserted
      by `live_seed_is_presets_only_and_selects_a_preset` +
      `live_default_catalog_is_presets_only_no_fabricated_custom`.
- Everything else was already strong and stayed: a live preview pane rendering
      real widgets (typography specimen, buttons, controls, presence list,
      badges, charts) under the selected theme; a two-way-reconciled customizer
      (accent hue/RGB picker, radius/spacing sliders, font family) editing real
      tokens with bounded-churn CRDT persistence; New/Duplicate/Delete/Reset/
      Export JSON; the shared responsive flap gallery. Verified on live (8
      presets), demo (editable custom), font, and sidebar-menu shots.

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
- [x] software_store (retired into `store`) — the constructor always loaded `sample_catalog()`
      (GIMP/Blender with fabricated star ratings), shown live, while the real
      popular-collection fetch landed unused in `live_apps`. A live worker now
      starts with an empty catalog and Browse renders the real Flathub popular
      set (ordered) with a "Loading apps from Flathub…" state; the sample stays
      offline/`--shots` only. (bd2f320c)

## video_editor — a real editor: GPU compositing, hardware decode, real audio

**This section previously described the export as working, and it was wrong in a
way worth recording.** It claimed export pushed "the composited timeline frames
(the same grade/fade math the preview shows)" and cited as proof "a real export
that writes a valid `ftyp` MP4". Both statements were literally true and
completely misleading: export rendered a *synthesized gradient* built from a
seven-entry palette table, never the user's footage, under every feature
combination — and a container magic-byte check passes happily on a valid MP4 of
a gradient. That check is precisely why the bug survived. Everything below is
now proven by decoding the encoded file back and comparing pixels.

- [x] **Export encodes the real footage.** Preview and export share ONE wgpu
      compute compositor (`gpu/compositor.rs` + `composite.wgsl`), so they cannot
      disagree — the export reads that compositor back rather than re-deriving a
      frame. Proven per container by decoding the output and comparing against
      the source: horizontal detail and mean-absolute-difference, plus assertions
      that a brightness change moves the *encoded* pixels and that the four
      transitions reach the file as different pictures. The WebM chain is decoded
      back too; it had never been until recently (every fidelity test was MP4).
- [x] **The colour grade reaches real video.** It used to be computed on the
      palette colour and then discarded — `let _ = decoded;` — so brightness,
      contrast, saturation and every effect only tinted the fallback gradient and
      did nothing to actual footage.
- [x] **Multi-layer compositing.** `top_video_clip_at` picked one clip and
      discarded every lower track, so picture-in-picture and real dissolves were
      architecturally impossible. The compositor now walks every visible track
      bottom-to-top; a transition is the incoming layer's alpha rather than a
      special case, which is what makes a dissolve sample both clips.
- [x] **Real transitions.** Cross-dissolve, fade, dip and wipe were one flat
      white-or-black flash; three of the four were byte-identical.
- [x] **Hardware decode**, multi-vendor: `decodebin3` autoplugs `nvh264dec`
      (NVIDIA) or `vah264dec` (AMD/Intel incl. iGPU), falling back to
      `avdec_h264`. Reported as the element actually plugged, never asserted.
      Seeks carry ACCURATE — keyframe-only seeking landed up to 4166 ms away.
      The preview takes planar NV12 (3.1 MB/frame) instead of a CPU colour
      convert (8.3 MB/frame).
- [x] **Hardware encode selection**, same shape: VA → NVENC → QSV → v4l2 →
      x264 → openh264, with a `QOS_MEDIA_HWENC` override that falls back to
      *software* rather than another vendor's GPU. **Not provable on this
      machine**: Fedora strips H.264/H.265 from Mesa's VA driver and nvcodec
      exposes 0 features without libcuda, so H.264 hardware selection is proven
      only against injected registries. The kilobit bitrate family *is* proven on
      real hardware via `vaav1enc` (400 kbit/s → 202 KB vs 8 Mbit/s → 4.0 MB).
- [x] **Real audio**, which the export simply did not have: mixing, per-clip
      volume and fades, track mute/solo, monitoring on the transport with a
      sample-count clock, and AAC/Opus muxed into the file. Proven with a
      generated stereo fixture (440 Hz left, 880→1760 Hz right at a zero
      crossing) that validates itself before any test may use it; channels are
      neither swapped nor summed, proven by injecting a swap. A/V sync measured:
      Opus lands the landmark at +0.0 ms (it is delay-compensated), MP4's +45 ms
      is AAC priming rather than an export error.
- [x] **Editing depth**: transform (crop/scale/rotate/position, PiP presets),
      speed with reverse, title clips rendered through the toolkit's own text
      stack, keyframed parameters (linear/hold/ease, clip-local so a split keeps
      the value on both sides), markers, multi-select with marquee, cross-track
      moves, per-track solo/lock/mute.
- [x] **Project files** (`.qvp`): magic + `version` + `min_reader` + payload
      length, so a writer that only adds fields stays readable by older builds
      and an interrupted write is refused rather than loading a convincing
      partial timeline. The implicit autosave is kept as the "work is never
      lost" guarantee; files are a second, explicit copy. Recents are stored
      outside the CRDT deliberately — that document replicates, and local file
      paths must not ride along to a collaborator.
- [x] **Accessibility**: the app contributed *zero* nodes — no `accessibility()`
      anywhere in the crate, so a screen reader saw an empty window and a
      keyboard-only user could scrub the playhead and nothing else. It now
      exposes bin rows, tracks, clips (position/duration/track/selection),
      transport buttons with real names, inspector sliders with values, and the
      export dialog as a focus-trapping dialog. In the timeline, Up/Down walk
      clips across tracks while **Left/Right stay nudge/step** — taking them for
      navigation would break the tool to make it navigable.

**Honesty notes.** The size estimate withholds its megabyte figure wherever the
chosen encoder demonstrably ignores its bitrate target — measured here as
openh264 writing 59 Mbit/s for a 1 Mbit/s request, vp8 14.4 and vp9 8.9 — which
on this machine is every container. A source whose bytes are absent says "Not on
this device"; one that is present but is not media (an HTML page saved as
`.mp4`, which is how this was found) says its size and length are unknown rather
than printing `0p · 0:00` as fact.

**Not done**: colour curves/LUT and masks/chroma key (compositor supports them;
UI and shader work not started), and an in-app undo affordance — undo lives on
`ui::State`/`UndoLog` while a widget only receives `&mut AutoCommit`, so no app
in the fleet can render an Undo control. That needs a toolkit API.

## terminal — CLEAR (real PTY + real shell; bulk-output O(n²) freeze fixed)
- [x] Live-verified end-to-end: the `shell` shot drives a real login-shell PTY
      (real prompt, `$0` → `/bin/bash`, printed output, real SGR-green "done"),
      proving a real shell runs through the reader → `sync` → vte → CRDT path.
      Full VT fidelity confirmed on shots: 16/256/truecolor SGR + style bits,
      the alternate screen (a vim-like redraw), scrollback with a live scrollbar,
      and an honest first-run empty state ("qOS Terminal").
- [x] Fixed a real defect: the VT print path wrote **every printed cell** to the
      CRDT on its own — a `splice_text` for the glyph plus a full-`attrs`-blob
      `put` for the line — and both are O(document) per call in Automerge, so a
      screenful of output was O(n²). It froze a debug build outright (a coloured
      80-line burst was ~45 min; the `scrollback` shot could not even render) and
      was the per-keystroke document rewrite HIG §6 forbids. `print` now batches a
      run of cells landing on one line into one in-memory buffer and commits it
      once (a diffed text splice + a single `attrs` put) when the line changes,
      any other emulator action runs, or the feed ends; every non-print callback
      flushes first so it never sees stale grid state. Regression test
      `bulk_styled_output_stays_linear_and_correct` feeds 80 styled lines through
      the real `feed` path and asserts correct content within a bound the
      quadratic path could never meet. All 52 terminal tests pass.

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

## firewall — responsive reflow + §7 CLEAR (verified wide/medium/narrow, shots)

Was a fixed three-rail immediate-mode painter (214px zone rail + rules table +
form + a fixed ~300px services panel) with zero width reactivity — crushed at
phone widths.

- [x] The zone rail is now the shared `ui::SidebarDrawer`: docked beside the
      content on wide windows (with an in-rail collapse toggle), a summoned
      overlay drawer with a dismiss scrim when narrow (hamburger in the header).
- [x] Progressive breakpoints so content reflows, not just the rail: the
      services panel collapses below 1000px (the rules table takes the whole
      centre); below 720px the rules table switches to two-line rows, the
      add-rule form stacks its controls onto four lines, and the summary stat
      chips wrap. The header status line + backend chip drop at narrow (the
      enable pill + red "Panic ON" button already carry that state) so nothing
      wraps over the summary.
- [x] Honesty preserved: the live app still reads the real firewalld config and
      shows an honest "firewalld unavailable" card when it can't — never a
      fabricated ruleset. Sample ruleset stays behind `--shots`/`new_demo()`.
- [x] Verified by offscreen shots at 1280 / 900 / 440 (populated) + the drawer
      open + the live unavailable state; 32 tests green.

## backups — responsive reflow + §7 CLEAR (verified every view wide + narrow, shots)

Was a fixed 256px floating-card rail (Timeline) plus fixed multi-column tile /
card grids that crushed at narrow widths.

- [x] The Timeline source rail is now `ui::SidebarDrawer` (docked wide, overlay
      drawer + scrim when narrow; hamburger in the top bar, in-rail collapse
      when docked).
- [x] Tile/card grids reflow by width: the summary tiles re-column 4→2→1 via the
      shared `card_columns`/`card_column_width`; the destination repository
      cards reflow into an aligned grid and become compact single-column cards
      when narrow (no crushed ring gauge); the schedules editor + its summary
      stack vertically when narrow; the distribution legend wraps; the recent-
      activity feed folds to two lines.
- [x] Top-bar chrome compacts (icon-only primary action, tabs clamp + abbreviate
      before they collide, wordmark drops); the restore sheet clamps to the
      window and stacks its columns; the status bar's three texts no longer
      overlap.
- [x] Honesty preserved: live still starts on an empty catalog with the "No
      backups configured yet" onboarding; the populated sample stays behind
      `--shots`/`new_demo()`.
- [x] Verified by offscreen shots of Timeline / Sources / Destinations /
      Schedules / Restore at wide + narrow (+ the empty onboarding + the overlay
      rail open); 12 tests green.

## containers — real zero-state + §7 CLEAR (verified list/images/empty shots, live drive)

Reviewed this cycle. The manager already drives real rootless podman (containers,
images, volumes, distroboxes via `podman ps/images/volume ls/stats` + real
start/stop/restart/pause/rm, `rmi`/tag/inspect, streaming pull, `exec` attach).
One real defect fixed; the rest verified fine.

- [x] No fabricated state: `podman::probe()` fell back to the bundled
      `sample_engine()` whenever podman was unreachable, so the *live* app showed
      seven fake containers you could "stop". It now returns an empty engine
      flagged not-live; the sample is reserved for `--shots`.
- [x] Honest, composed zero-state: the container list shows "No container engine"
      (podman absent) or "No containers yet" (engine up, none created) — a title
      over an explanatory line, not a lone grey sentence.
- [x] Verified: list + images + volumes populated shots read genuinely nice
      (status pills, inline lifecycle actions, tabbed detail, live logs); the
      empty-engine shot is honest; a live headless drive stands the app up against
      the real backend and a section switch persists. Real `podman ps --all
      --format json` confirmed returning real containers. 35 tests green.

## logs — real zero-state + §7 CLEAR (verified stream/empty shots, live journal drive)

Reviewed this cycle. The viewer already parses real `journalctl -o json` (severity
colouring, unit/boot/priority filters, free-text search, live-tail, a full-field
detail pane). One real defect fixed.

- [x] No fabricated logs: the live app fell back to the bundled `sample_entries()`
      whenever the journal read came back empty/unreadable, surfacing invented log
      lines. It now shows the real journal only; empty/unreadable renders an honest
      "No journal entries" zero-state (title + explanation). Sample stays behind
      `--shots`.
- [x] Verified: the populated stream reads nicely (tabular time, severity dots +
      tags, detail grid, legend); the empty shot is honest; a live headless drive
      against **real journalctl** loaded real entries and a typed search filter
      persisted to the CRDT (`search: "shell"`). 12 tests green.

## hex_editor — real open/save + honest empty + width-fit grid (verified shots + e2e)

Reviewed this cycle. Had real byte editing into the CRDT but three gaps: it opened
onto a fabricated sample buffer by default, had no in-app Open (only a CLI path)
and **no Save to disk at all**, and stranded 16 columns in a band of dead space.

- [x] Real file lifecycle: Open (Ctrl+O / empty-state button) and Save (Ctrl+S)
      go through the host XDG file portal (`ui::portal`); Ctrl+S on a path-backed
      buffer writes straight back and clears the dirty markers, with a status flash.
- [x] Honest empty state: with no file the live app shows a composed zero-state
      (title, one-line explanation, a single Open action) — never a fake buffer;
      an unreadable path yields the same. Sample buffer reserved for `--shots`.
- [x] §7 grid: the byte columns auto-fit the viewport width (24/row at 1280) and
      the data inspector sits directly beside the ASCII column, so the surface
      fills the window instead of stranding a fixed 16 columns.
- [x] Proven end-to-end (`tests/open_save.rs`): a real file opens through the
      portal drain path, and an edit saved with Ctrl+S lands on disk (dirty
      cleared). Verified shots: sample (width-fit), a real ELF opened live, and
      the empty state. 45 tests green.
