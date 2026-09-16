# App UI review group 2 — all 16 apps marked

Read-only review. No fixes, builds, commits or subagents. All16 apps visually inspected from fresh controller captures; OSK additionally reviewed from a fresh source-verified Fedora44 ARM64 build.

## Provenance and scope

Fresh supported macOS build completed in `app-build-macos-2.log`; each capture directory has `manifest.json` with command, binary SHA256 and source provenance. Source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`; working diff SHA256 `7f762ea1f3d802de77724201200fa2412477d420d59c0bebe82fe0571b479797`. OSK provenance: `captures/osk/manifest.json`, binary SHA256 `917fdee39859d97b1e12f0a2361453e07ac3c3616d99003b8dc62e5b9c692012`, source hashes in `linux-source-hashes.json` (controller verified immutable3614-file source). Follow-up native commands/environment/status are in `supplement-results.json`; actual dimensions in `capture-dimensions.json`. Images listed below were individually opened with view_image. No historic image substituted. Requested wide1200×800, compact800×700, narrow480×900, large800×900@200%; fidelity exceptions explicitly marked. This is composition review, not a claim that clicks were exercised.

## file-explorer

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/file-explorer/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/file-explorer/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/file-explorer/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/file-explorer/actual-size-wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/file-explorer/actual-size-narrow.png`
- Main flow: Browse places → navigate folders → select/open or operate on files.

1. **P2 — capture fidelity, app harness:** `apps/file-explorer/src/main.rs:645` accepts `--size`, while controller used `--viewport`; both inspected PNGs are the same 1200×720 default. `:779` also drops text scale. Desired result: faithful narrow/200% evidence before judging reflow. Ordinary empty-folder composition is calm and workspace-oriented; no extra spacing defect established.

**Needed verification:** Supplement actual-size-wide/narrow confirms readable empty-folder layout at1200×800 and480×900. Narrow has an additional near-empty drawer-toggle row beneath navigation (`apps/file-explorer/src/files_app.rs:7995` NavShell composition), a P2 opportunity to combine compact controls and preserve file-list height. Still verify populated directory and actual200%.

## firewall

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/firewall/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/firewall/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/firewall/compact.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/firewall/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/firewall/large-text.png`
- Main flow: Select zone → inspect rules/services → add or edit a rule → Apply.

1. **P1 — app geometry:** at large text header wraps through zone rail, summary chips spill into filters, Rules/Services collide, and add-rule labels crowd controls. `apps/firewall/src/app.rs:478`, `:545`, `:704` reserve unscaled regions. Size rows/cards from measured text and reflow groups.
2. **P2 — app hierarchy:** narrow summary uses 188px despite one compact line of chips, and the permanent 226px add-rule form leaves only four rules visible; the Rules label is duplicated beside selected Rules tab. `apps/firewall/src/app.rs:579`, `:607`, `:2702`. Compact summary and place new-rule editing behind a clear Add rule action to prioritize inspection.

**Needed verification:** Check all rules reachable, service/rules switching, add/apply main flow, 480px and true200% after shared scaling review.

## git_client

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/git_client/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/git_client/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/git_client/compact.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/git_client/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/git_client/large-text.png`
- Main flow: Open repository → select changed file/diff → stage → enter message → commit.

1. **P1 — app toolbar:** narrow screenshot clips Changes off the left edge and overwrites hamburger/repository with the right-anchored tabs/settings. `apps/git_client/src/app.rs:874–888` reserves 230px settings plus full measured tabs without compact alternative. Move secondary preferences to overflow and retain visible navigation.
2. **P1 — app + shared scale audit:** large-text change rows, diff lines, line numbers, hunks and commit footer overlap. Fixed 30px rows/150px commit box (`apps/git_client/src/changes.rs:20`) and appbar56px (`apps/git_client/src/app.rs:84`) need measured scaled geometry after renderer fidelity is fixed.

**Needed verification:** 480px list access, staging, message entry/commit, scrolling diff, actual200% without overlapping lines.

## hex_editor

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/hex_editor/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/hex_editor/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/hex_editor/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/hex_editor/large-text.png`
- Main flow: Open file → inspect/select byte → edit or search → save.

1. **P1 — app + shared scale audit:** large-text header collides with pills and grid degenerates into overlapping byte/offset/ASCII rows. `apps/hex_editor/src/view.rs:519`, `:2452–2508` use fixed 14px font/20px rows and unscaled column pitches. Scale grid measurement/painting consistently and choose bytes per row from scaled metrics. Standard narrow8-byte layout is useful; preserve its density.

**Needed verification:** Verify 200% byte hit targets, row/ASCII alignment and search; do not treat empty remainder of small73-byte fixture as wasted layout.

## imager

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/imager/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/imager/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/imager/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/imager/large-text.png`
- Main flow: Choose source → choose drive → customize → confirm → write → result.

1. **P2 — app harness:** nominal200% remains ordinary text: `apps/imager/src/main.rs:191` changes Theme but `:196` never sets TextRenderer scale. Fix evidence plumbing, then evaluate wizard.
No additional actionable layout defect established at normal scale: source choice, selected image, releases, recent choices and Continue have readable grouping; narrow number-only steps preserve space.

**Needed verification:** Actual200%, source-empty first run, Drive, confirmation and footer reachability.

## logs

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/logs/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/logs/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/logs/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/logs/large-text.png`
- Main flow: Search/filter journal → select entry → inspect detail → refresh/follow/export.

1. **P1 — app + shared scale audit:** at large text filter labels/chips paint through one another, table columns and consecutive rows overlap, and detail message consumes entire pane width with unreadable clipping. `apps/logs/src/app.rs:591`, `:649`, `:1744`, `:2059` keep fixed bands/row geometry; `apps/logs/src/responsive.rs:24` chooses split solely by width. Make split/rows respond to effective text space; keep main entries readable. Normal-scale narrow push list is well composed.

**Needed verification:** Actual200% list/detail Back route and all filters; include long log entries.

## mail

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/mail/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/mail/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/mail/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/mail/large-text.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/mail/compose-wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/mail/compose-narrow.png`
- Main flow: Add account → choose folder → choose/read message → compose/reply.

1. **P1 — app geometry:** large-text inbox keeps normal sender/subject/snippet offsets, so the three lines touch/overlap and avatars contain oversized initials. `apps/mail/src/pane.rs:383`, `:1020`, `:1050` use fixed list-row geometry. Scale or measure vertical text bands and avatar typography, then reduce preview detail when needed.
2. **P2 — app/host composition:** normal narrow inbox spends a full near-empty60px hamburger strip above a second60px inbox/search strip. `apps/mail/src/app.rs:570` plus `apps/mail/src/pane.rs:383`. Merge compact navigation and inbox tools when space permits.

**Needed verification:** Compose supplement reviewed at1200/480: recipients, subject, formatting, message body and Send/Save draft/Discard remain clearly grouped; no new normal-scale composition defect. Verify open/read/reply and compose/setup at200%; baseline wide layout otherwise clear.

## maps

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/maps/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/maps/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/maps/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/maps/large-text.png`
- Main flow: Search place → select result → Directions → choose route → navigate.

1. **P2 — app harness:** custom renderer `apps/maps/src/main.rs:195` ignores requested text scale; nominal large-text image still normal typography. Must provide truthful200% evidence. Normal navigation panel has readable grouped origins/modes/timing/steps; narrow drawer intentionally covers map and has visible dismiss control. No further confirmed composition issue.

**Needed verification:** Actual200% navigation and collapsed drawer, search/result route, map recovery after panel dismissal.

## monitor

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/monitor/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/monitor/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/monitor/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/monitor/large-text.png`
- Main flow: Inspect resource overview/process list → sort/filter/select process → inspect details or stop.

1. **P2 — app harness:** `apps/monitor/src/main.rs:107` drops requested scale and `:140` constructs default renderer/theme. Nominal large-text is normal text. Normal narrow process table and four summary cards remain readable; no forced density redesign warranted from these views.

**Needed verification:** Real200%, select narrow process detail and stop confirmation, Resources tab.

## music

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/music/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/music/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/music/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/music/large-text.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/music/search-wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/music/search-narrow.png`
- Main flow: Browse/search library → select album/track → play → manage queue.

1. **P1 — app main-flow loss:** large-text view is visually clean but replaces Home/library navigation with undifferentiated tracks, omits drawer and queue controls, and makes search field geometry ZERO. `apps/music/src/app/render.rs:2194`, `:2236`, `:2247` implement this alternate surface. Retain accessible navigation/search and playback/queue actions while using readable rows; do not solve overlap by dropping main routes. Wide and narrow normal-scale grouping is clear.

**Needed verification:** Normal-scale search supplement reviewed at1200/480: query, type filters and track rows are readable, with clear transport. At200% navigate albums/playlists, enter search, choose result, play and open queue. QOS_SHOTS_TEXT_SCALE=2 is used by controller and respected (`apps/music/src/main.rs:524`).

## notes

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/notes/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/notes/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/notes/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/notes/large-text.png`
- Main flow: Create/select page → rename/title → edit blocks → navigate pages.

1. **P1 — shared editor with app integration:** large-text body is disproportionately larger than title, clips checklist/prose at right edge and link overlay is misaligned. Normal wide/narrow is calm and readable. `apps/notes/src/main_panel.rs:64` owns page geometry; `ui/src/widget/block_editor.rs:3660` delegates measured line layout and `:3347` painting. Root should investigate common text-scale ownership/line wrapping before app padding changes. Desired change: one scale application and readable wrapping/inline alignment in the editing surface.

**Needed verification:** Actual200% prose/checklist/code/link editing, caret/hit geometry, scrolling, note navigation.

## pdf_viewer

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/pdf_viewer/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/pdf_viewer/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/pdf_viewer/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/pdf_viewer/large-text.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/pdf_viewer/sample-wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/pdf_viewer/sample-narrow.png`
- Main flow: Open PDF → read/navigate pages → annotate/search → save.

1. **P1 — app toolbar:** narrow screenshot visibly merges Note/Sign tools with zoom minus/plus. `apps/pdf_viewer/src/app_draw.rs:102–164` places five194px tools atx138–332 while right cluster beginsx276 at480px. Overflow/reflow tools while keeping individual action hit targets.
Supplement populated --sample confirms wide thumbnail/document/comments hierarchy and narrow document-focused collapse are useful. Toolbar overlap remains visible in sample-narrow. Sample title wraps onto its caption in both samples (`apps/pdf_viewer/src/render.rs:84`, `apps/pdf_viewer/src/model.rs:578`); P2 sample-renderer line-height/wrapping defect, separate from real-PDF rendering and global app layout.

**Needed verification:** Populated sample reviewed at480/1200; still verify200% populated document and open/save/annotate/zoom hit targets.

## photos

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/photos/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/photos/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/photos/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/photos/large-text.png`
- Main flow: Open image or browse library → edit/rotate/crop → save/export.

1. **P2 — app status geometry:** large-text file metadata is clipped by bottom edge; `apps/photos/src/app.rs:2551` reserves a fixed status band while text grows. Enlarge or remove optional metadata band at accessibility scale so filename is legible. Main image canvas and toolbar adapt well at480px; letterboxing is appropriate.

**Needed verification:** Status text200%, More→Save/Crop and library route (not visually reviewed yet).

## remote

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/remote/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/remote/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/remote/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/remote/large-text.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/remote/sample-wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/remote/sample-narrow.png`
- Main flow: Find saved machine or create connection → connect → interact with session.

1. **P1 — app geometry:** large-text title overlaps count, New connection label overflows its button, empty-state heading/subtitle touch. `apps/remote/src/app.rs:4206`, `:4365`, `:4740` use fixed heights/button widths. Measure headline/subtitle/button clusters and reflow. Normal narrow empty state has clear primary action.

**Needed verification:** Supplement QOS_REMOTE_SAMPLE=1 roster reviewed at1200/480: connection identity, readiness and Edit/Connect remain clearly grouped. Verify New connection form/session and200% roster; scale issue remains in empty-state evidence.

## scratchpad

- Status: **visual-reviewed** (limitations below).
- Capture provenance: fresh controller binary; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/scratchpad/manifest.json`.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/scratchpad/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/scratchpad/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/scratchpad/large-text.png`
- Main flow: Write a note → Save → observe saved status.

1. **P1 — shared multiline sizing / app composition:** primary editor is a one-line-height strip despite nearly600px unused body; at200% placeholder is vertically cut in half. `apps/scratchpad/src/lib.rs:103–124` wraps multiline TextInput in fill-height Scrollable but does not give the editor viewport a usable writing area. Root should check shared TextInput/Scrollable sizing. Desired change: an editor that visibly occupies available writing space with scale-aware minimum line height.
2. **P2 — app hierarchy:** title/tagline plus persistent bottom save instruction use more hierarchy than the actual writing field. Reduce helper chrome once editor size is fixed. Untracked work belongs to another task: review only.

**Needed verification:** Multi-paragraph typing, wrapping/scrolling, Save state and200%; do not edit until controller resolves ownership.

## osk

- Status: **visual-reviewed** (fresh Linux evidence; native macOS limitation resolved for review).
- Capture provenance: Fedora44 ARM64, immutable current source checked by controller; `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/osk/manifest.json` records binary hash and successful commands.
- Exact viewed captures:
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/osk/wide.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/osk/compact.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/osk/narrow.png`
  - `/Users/xaver/dev/qos-macos-shell/.superpowers/app-ui-2026-09-15/captures/osk/large-text.png`
- Main flow: focus text → type/swipe → select prediction → dismiss.

1. **P1 — app scaled text placement:** large-text letters sit below key centers and cross key boundaries; prediction words spill below pills. `apps/osk/src/widget.rs:1078` and `:1146` center using nominal font size instead of measured/scaled text height, while `:1293` keeps fixed intrinsic row/suggestion heights. Use scaled measured line height for centering and sufficient row height. Normal480px keyboard is readable and balanced.
2. No whitespace defect: black remainder of tall test viewport is outside intrinsic312px keyboard surface; this keyboard is a bottom overlay, not an editor that should fill900px.

**Needed verification:** After shared scale audit, actual200% keyboard with contained text and valid hit targets; alphabetic/numeric layouts, suggestions and live input-method overlay integration. No interaction execution claimed.

## Shared patterns and sequencing

1. **Prioritize shared scale fidelity first.** Notes, hex and logs show disproportionate text growth; root owns identifying whether Theme/type tokens and renderer both apply scale. Do not compensate by shrinking individual fonts.
2. **Then fix app-authored rectangles.** Firewall/Git/Logs/Mail/Remote fixed rows, chips and headers must respond to measured text. Retained Row/Column gap changes alone cannot fix these custom-painted surfaces.
3. **Capture truth before claims.** File-explorer ignores --viewport and scale; Maps/Monitor ignore scale; Imager changes Theme without renderer scale; Music requires env scale (controller provides it).
4. **Main task space differs by app.** Preserve image/map/editor canvas and dense-but-readable tables. Firewall can prioritize existing rules over permanently expanded creation form.
5. **No invented defects.** Imager normal-scale source wizard and Monitor normal-scale process list need no generic spacing rewrite from current evidence. Photos canvas letterboxing and tiny Hex fixture blank remainder are expected.
6. **Missing main-flow evidence:** OSK and PDF/Remote/Music/Mail main-route supplements now reviewed. Remaining evidence limits: Photos library; Remote form/session; populated PDF200%; actual scale for four custom handlers; interaction checks. These limits do not leave any assigned app unmarked.
