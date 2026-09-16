# App UI review — group 1

Review-only. Source inspected 2026-09-15. Fresh controller captures individually inspected with view_image: all four variants for all 15 windowed apps plus four supplementary primary-flow images (64 images). No implementation changes or builds performed by reviewer. References are repository-relative.

## apps/agent

- Status: **windowless**
- Capture/provenance: none available at this review pass.
- Main flow: Join collaboration document or Task Center and execute instructions.
- Composition source: `apps/agent/src/main.rs:36`.
- Findings: No app window exists; primary-screen spacing review does not apply.
- Needed verification: Broker/task feedback belongs to hosting surfaces; no screenshot required.

## apps/api_tester

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/api_tester/wide.png`, `.superpowers/app-ui-2026-09-15/captures/api_tester/compact.png`, `.superpowers/app-ui-2026-09-15/captures/api_tester/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/api_tester/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/api_tester/manifest.json`.
- Main flow: Choose saved request → edit method/URL/parameters/body → Send → inspect response.
- Composition source: `apps/api_tester/src/app.rs:1209`.
- Findings:

1. **P1 — 200% text makes request/response unreadable.** In large-text.png the tabs run into one another, parameter rows overlap, status is clipped, JSON lines overlap heavily. `apps/api_tester/src/app.rs:1240`, `:1724` use fixed regions/row steps. Ownership: app geometry plus suspected shared double-scaling for token-sized text (root investigating). Desired: measured tab widths, scale-aware rows and response line metrics; verify all request tabs and response modes at 200%.
2. **P2 — maintenance actions dominate navigation.** wide.png shows ten full-width create/rename/delete buttons above actual collections, which only start around y=580. `apps/api_tester/src/app.rs:655`. Desired: put collection/request actions beside their objects or in menus; environment editing in its own section; keep saved requests high in rail. App-owned.
3. **P2 — blank request region displaces response.** Default three parameters end around y=235 but response starts y=391 (wide) or434 (narrow). `apps/api_tester/src/app.rs:1248`. Desired draggable/content-aware request-response divider. App-owned.

- Needed verification: Default, narrow, 200% text; Send response and Body/Auth tabs.

## apps/backups

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/backups/wide.png`, `.superpowers/app-ui-2026-09-15/captures/backups/compact.png`, `.superpowers/app-ui-2026-09-15/captures/backups/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/backups/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/backups/manifest.json`.
- Main flow: Choose source → inspect snapshot timeline → Back up now or restore snapshot.
- Composition source: `apps/backups/src/app.rs:1110`.
- Findings:

1. **P1 — 200% fixed rows collide.** large-text.png shows tab labels and Back up now clipped, snapshot time overlapping Auto badge, summary value clipping, metadata colliding. `apps/backups/src/app.rs:745`, `:1112`, `:1606`. Desired measured toolbar and taller/reflowed timeline rows; app-owned.
2. **P2 — title/scope lack separation.** “Snapshot timelineAll sources” visually runs together even at1× because scope is placed after unweighted title measurement while title is bold. `apps/backups/src/app.rs:1131`. Desired consistent measured bold title then a distinct gap. App-owned. Narrow summary tiles consume space but remain understandable; no additional defect asserted.

- Needed verification: Default timeline, narrow, 200%, restore and failure scenarios.

## apps/browser

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/browser/wide.png`, `.superpowers/app-ui-2026-09-15/captures/browser/compact.png`, `.superpowers/app-ui-2026-09-15/captures/browser/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/browser/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/browser/manifest.json`.
- Main flow: Enter address/search → navigate page → optionally bookmarks/history/settings.
- Composition source: `apps/browser/src/browser_widget.rs:1154`.
- Findings:

No confirmed chrome-layout defect in four inspected images; compact controls leave a useful address field. **Evidence limitation:** all show a blue placeholder instead of a rendered web page, so navigation/page-content flow is not visually reviewed. `apps/browser/src/main.rs:309` shot path versus `:460` live-shot path. Need live page capture. Ownership: screenshot fixture/engine evidence, not a claimed page bug.

- Needed verification: Default page with actual browser content, address overlay, narrow, 200%; confirm URL space after trailing actions.

## apps/calculator

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/calculator/wide.png`, `.superpowers/app-ui-2026-09-15/captures/calculator/compact.png`, `.superpowers/app-ui-2026-09-15/captures/calculator/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/calculator/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/calculator/manifest.json`.
- Main flow: Enter expression → evaluate → reuse history or switch to graph.
- Composition source: `apps/calculator/src/keypad.rs:215`.
- Findings:

1. **P1 — scaled function text overlaps and clips.** large-text.png shows graph function prefix/expressions colliding, leading coefficient of second formula obscured, toolbar mode labels touching/clipped. `apps/calculator/src/app.rs:158` fixed segment widths; `apps/calculator/src/graph2d.rs:275` fixed expression inset/action reservation. Desired measure scaled expressions and mode labels, reflow sidebar or overlay it. App-owned geometry; check shared math/text scaling. Graph canvas appropriately dominates at narrow1×. Supplementary `.superpowers/app-ui-2026-09-15/captures/calculator/live-wide.png` and `.superpowers/app-ui-2026-09-15/captures/calculator/live-narrow.png` individually viewed: calculator keypad route is clear at1× and adapts key columns; no additional baseline spacing defect. Provenance: controller current-build supplementary capture. Short-height and keypad200% still need verification.

- Needed verification: Default, graph, narrow, short viewport, 200%.

## apps/calendar

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/calendar/wide.png`, `.superpowers/app-ui-2026-09-15/captures/calendar/compact.png`, `.superpowers/app-ui-2026-09-15/captures/calendar/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/calendar/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/calendar/manifest.json`.
- Main flow: Select date/view → inspect event → create/edit event.
- Composition source: `apps/calendar/src/app.rs:2669`.
- Findings:

1. **P1 — event notes scale differently from form.** large-text.png form fields render ~2× but note heading/body ~4×, body clips horizontally and becomes unreadable. `apps/calendar/src/app.rs:2743` notes width integration; `:4468` notes header; underlying rich-text renderer needs shared ownership investigation. Desired one scale application and line wrapping within note area. App notes/form separator also needs enough height at2×. Wide/narrow1× event form has clear hierarchy. Supplementary `.superpowers/app-ui-2026-09-15/captures/calendar/calendar-wide.png` and `calendar-narrow.png` individually viewed: month grid clear wide; narrow event chips mostly truncate to times and need opening to identify events. Preserve access to agenda/day modes; do not treat dense month grid as a generic spacing failure. Provenance: controller current-build supplementary capture.

- Needed verification: Month/week, new-event, narrow, 200%; confirm dense month chips and editor scrolling.

## apps/camera

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/camera/wide.png`, `.superpowers/app-ui-2026-09-15/captures/camera/compact.png`, `.superpowers/app-ui-2026-09-15/captures/camera/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/camera/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/camera/manifest.json`.
- Main flow: Preview → choose photo/video → capture → open gallery; alternate scan.
- Composition source: `apps/camera/src/app.rs:3123`.
- Findings:

No confirmed spacing/hierarchy issue in1× wide, compact, narrow photo preview. Capture button is clear and preview maximized. **P1 verification defect:** large-text.png is actually1×, matching `apps/camera/src/main.rs:104` omission. Apply launcher scale in custom shot path before judging200%. App-owned. Other camera/gallery/recovery routes not visually exercised.

- Needed verification: Photo default, no-device recovery, gallery, narrow, 200%; inspect top-pill and bottom-mode collisions.

## apps/chat

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/chat/wide.png`, `.superpowers/app-ui-2026-09-15/captures/chat/compact.png`, `.superpowers/app-ui-2026-09-15/captures/chat/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/chat/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/chat/manifest.json`.
- Main flow: Choose workspace/channel → read messages → compose/send → open thread.
- Composition source: `apps/chat/src/app.rs:2330`.
- Findings:

1. **P2 — workspace title and online badge collide.** wide/compact show “Acme Design channe…” meeting/underlying the3people-online pill without clean gap. Sidebar header truncates title to width minus60 while presence pill needs more (`apps/chat/src/render_chat.rs:554`). Desired bounded/elided title and measured badge reservation. App-owned.
2. **P1 verification defect:** large-text.png is1×; `apps/chat/src/main.rs:141`. Thread/composer default geometry otherwise keeps content clear. Attachment denied placeholders are real fixture state, not labelled a layout defect.

- Needed verification: Main conversation, long draft, thread, narrow, 200%; inspect message text and sidebar headings.

## apps/code_editor

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/code_editor/wide.png`, `.superpowers/app-ui-2026-09-15/captures/code_editor/compact.png`, `.superpowers/app-ui-2026-09-15/captures/code_editor/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/code_editor/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/code_editor/manifest.json`.
- Main flow: Open file from tree → edit/search → diagnostics or terminal.
- Composition source: `apps/code_editor/src/app.rs:3267`.
- Findings:

1. **P2 — empty Problems panel consumes editing height on launch.** All screenshots show bottom~160px occupied by Problems(0) and “No problems in this file,” reducing visible code even on narrow/compact windows. `apps/code_editor/src/app.rs:607` sets `panel_visible: true`; `:3313` adds it to editor column. Desired start closed or collapse empty dock until user asks; retain manual toggle/persisted preference. App-owned.
2. **P1 verification defect:** custom screenshot ignores200%, `apps/code_editor/src/main.rs:142`. Actual scaled editor still needs capture.

- Needed verification: Default project, narrow, 200%, diagnostics/completion and terminal.

## apps/contacts

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/contacts/wide.png`, `.superpowers/app-ui-2026-09-15/captures/contacts/compact.png`, `.superpowers/app-ui-2026-09-15/captures/contacts/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/contacts/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/contacts/manifest.json`.
- Main flow: Find/select contact → inspect details → edit or create.
- Composition source: `apps/contacts/src/app.rs:1940`.
- Findings:

No confirmed baseline composition defect. Related fields form readable groups; narrow becomes contact list and wide shows detail. Compact address runs close to right edge but remains visible in fixture; test longer addresses before asserting overflow. **P1 verification defect:** custom screenshots ignore200%, `apps/contacts/src/main.rs:144`. Desired corrected capture scaling and form/detail large-text verification. App-owned.

- Needed verification: Default selected contact, narrow, 200%, long names and contact form.

## apps/containers

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/containers/wide.png`, `.superpowers/app-ui-2026-09-15/captures/containers/compact.png`, `.superpowers/app-ui-2026-09-15/captures/containers/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/containers/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/containers/manifest.json`.
- Main flow: Choose resource section → select container → inspect logs/stats → lifecycle actions.
- Composition source: `apps/containers/src/app.rs:2655`.
- Findings:

1. **P1 — 800px layout keeps three columns and overlaps row actions/status.** compact.png shows names heavily truncated; Stop/Restart controls paint over Running badges; search placeholder overlaps shortcut badge. `apps/containers/src/app.rs:62` compact threshold600; `apps/containers/src/panels.rs:731` columns. Desired switch to list/detail push based on available content width, earlier than600, or collapse navigation/detail. App-owned.
2. **P1 — 200% header/log overlap.** large-text.png title/image/tab text collide, Back clips at right, logs overlap. `apps/containers/src/panels.rs:1101` fixed header and scaled token sizes passed into `apps/containers/src/draw.rs:79`. Ownership shared suspected token+renderer double scale plus app geometry. Desired single scaling and measured line/header heights.

- Needed verification: Default selected container, compact list/detail, 200%, disconnected engine.

## apps/db_client

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/db_client/wide.png`, `.superpowers/app-ui-2026-09-15/captures/db_client/compact.png`, `.superpowers/app-ui-2026-09-15/captures/db_client/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/db_client/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/db_client/manifest.json`.
- Main flow: Connect → select schema/table or write query → Run → inspect results.
- Composition source: `apps/db_client/src/app.rs:602`.
- Findings:

1. **P1 — 200% result header and toolbar clipped.** large-text.png Query/Run and result column labels sit partly outside their backgrounds; result count clips against grid. `apps/db_client/src/app.rs:595` fixed tab/toolbar/result-strip geometry; shared DataGrid row/header sizing also implicated. Desired scale-aware tool/header heights and normal line-number scale. App + framework.
2. **P2 — short query reserves large empty block.** Four-line query leaves~230px blank before results wide and~250px narrow. `apps/db_client/src/app.rs:602` fixed42% split. Desired adjustable divider/default content-aware editor. App-owned. Many-connections overflow remains source hypothesis, not confirmed finding.

- Needed verification: Query default, Data tab, narrow, 200%, many connections.

## apps/diffmerge

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/diffmerge/wide.png`, `.superpowers/app-ui-2026-09-15/captures/diffmerge/compact.png`, `.superpowers/app-ui-2026-09-15/captures/diffmerge/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/diffmerge/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/diffmerge/manifest.json`.
- Main flow: Open two files or merge inputs → navigate differences → resolve/save.
- Composition source: `apps/diffmerge/src/app.rs:527`.
- Findings:

1. **P1 — 200% toolbar/panes unreadable.** large-text.png toolbar labels collide, lines and conflict choice buttons overlap; fixed3-pane layout persists at800px although text doubled. `apps/diffmerge/src/app.rs:527`, `:551` width-only compact logic and fixed metrics; merge widget shared row metrics need inspection. Desired scale-aware single-pane mode, measured row/action heights. App + underlying merge widget.
2. **P2 — compact800px Take both clips at right edge.** compact.png third pane cannot contain conflict action group. Desired compact conflict choices or earlier single-pane mode. App/widget ownership. Narrow480px1× single-pane mode is much clearer.

- Needed verification: Diff and merge scenarios, narrow, 200%; inspect conflict actions and pane text.

## apps/disks

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/disks/wide.png`, `.superpowers/app-ui-2026-09-15/captures/disks/compact.png`, `.superpowers/app-ui-2026-09-15/captures/disks/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/disks/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/disks/manifest.json`.
- Main flow: Select drive → inspect capacity/health/partitions → targeted action.
- Composition source: `apps/disks/src/app.rs:1516`.
- Findings:

Wide overview has clear drive→action→tabs→health hierarchy; no confirmed wide defect. **P1 verification defect:** `apps/disks/src/main.rs:143` ignores200%. Compact/narrow fixtures force drawer open, obscuring selected-drive content. Forced56/44 overview columns at `apps/disks/src/app.rs:1516` remain a source-only concern until closed-drawer narrow capture. Desired verify before implementing stack change.

- Needed verification: Default overview, narrow, 200%, SMART/partition tabs.

## apps/ebook

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/ebook/wide.png`, `.superpowers/app-ui-2026-09-15/captures/ebook/compact.png`, `.superpowers/app-ui-2026-09-15/captures/ebook/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/ebook/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/ebook/manifest.json`.
- Main flow: Import/select book → read/page → TOC, annotations, reading settings.
- Composition source: `apps/ebook/src/app.rs:7544`.
- Findings:

1. **P1 — 200% TOC rows overlap and reading lines collide.** large-text.png chapters wrap across adjacent fixed44px rows; page text touches/overlaps vertically and clips at bottom. `apps/ebook/src/app.rs:8982` fixed TOC row; `:7544` page geometry; `:8148` line rendering. Desired measure chapter-row heights and scale pagination/line advance with rendered font metrics. App-owned reading layout plus shared text scaling investigation. Narrow1× edge-to-edge reading works well; intentional readable width/paper margins are appropriate.

- Needed verification: Library and read scenarios, TOC+notes open, narrow, 200%, long titles.

## apps/feeds

- Status: **visual-reviewed**
- Capture/provenance: `.superpowers/app-ui-2026-09-15/captures/feeds/wide.png`, `.superpowers/app-ui-2026-09-15/captures/feeds/compact.png`, `.superpowers/app-ui-2026-09-15/captures/feeds/narrow.png`, `.superpowers/app-ui-2026-09-15/captures/feeds/large-text.png` individually viewed; exact command/binary hash/source provenance in `.superpowers/app-ui-2026-09-15/captures/feeds/manifest.json`.
- Main flow: Select feed/article → read → save/tag or refresh subscriptions.
- Composition source: `apps/feeds/src/app.rs:1719`.
- Findings:

1. **P1 — 200% titles overlap snippets.** large-text.png “Show HN…” second line paints over source/snippet, likewise “Ask HN…”; sidebar text meets counts/edges. `apps/feeds/src/app.rs:77`, `:78`, `:2012` fixed row heights and snippet offsets. Desired measured title block plus gap/snippet, scale-aware sidebar rows and responsive width. App-owned.
2. **P2 — article row related text is too far apart at1×.** wide/narrow list places one-line title near top and summary near bottom of88px row, with~30px empty between; unrelated next article title can appear closer than its own summary. `apps/feeds/src/app.rs:2018`. Desired tighter title+source grouping and separate interarticle padding, while allowing two-line titles. App-owned. Middle-tier fixed sidebar focus concern remains unverified until detail capture.

- Needed verification: Default, detail at 640/800px, 200%, long-titles and add-subscription.

## Shared patterns

0. Fresh capture provenance: source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, working diff SHA256 `7f762ea1f3d802de77724201200fa2412477d420d59c0bebe82fe0571b479797`; controller rebuilt binaries immediately before capture. Wide1200×800, compact800×700, narrow480×900, large800×900 requested2×.

1. App-authored fixed pixel regions and width-only responsive decisions need visual review at 200%; framework typography growth cannot repair those geometries automatically.
2. Fixed editor/result proportions in API tester and DB client can displace the user’s result despite very little request/query content.
3. Drawer/push navigation is widespread and mostly deliberate; distinguish app-owned breakpoints from primitive sizing defects.
4. Editor/canvas apps should preserve workspace; ebook reading width limits should remain intentional.

## Missing evidence

All15 windowed apps have baseline visual review (60images) plus4supplements for Calendar grid and Calculator keypad, with1windowless agent. Missing/limited: Browser actual web page (WebPE unavailable on macOS); Disks closed narrow detail (shot path forces drawer open); real200% for Camera, Chat, Code editor, Contacts, Disks. These are explicit evidence limits, not unreviewed app entries.

## Confirmed screenshot fidelity defects

The following custom `--shots` handlers construct a theme without applying the launcher `args.text_scale` or `QOS_SHOTS_TEXT_SCALE`. A file generated with `--text-scale 2` therefore cannot be labelled verified 200% evidence for these apps. This is an app-owned verification defect, separate from an actual visible layout defect:

1. Camera: `apps/camera/src/main.rs:104`.
2. Chat: `apps/chat/src/main.rs:141`.
3. Code editor: `apps/code_editor/src/main.rs:142`.
4. Contacts: `apps/contacts/src/main.rs:144`.
5. Disks: `apps/disks/src/main.rs:143`.

Desired change: thread launcher text scale through custom screenshot theme and renderer consistently; verify text dimensions differ at 1× and 2× before using the captures as accessibility evidence. Calendar, calculator, and browser explicitly apply scale in their custom shot path.
