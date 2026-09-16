# Corrected screenshot review 1 — 2026-09-15

## Evidence and scope

All nine assigned apps are **visual-reviewed at actual requested 2× text scale**, compared individually with the same 800×900 1× viewport. Eighteen images opened individually using view_image; no contact sheets, builds, production edits, commits, or subagents. Baseline entries in review-1.md and review-2.md were read. Source anchors below refer to `ef5f08db5`; captured binaries additionally include the unchanged recorded baseline working diff. Current implementation changes are not captured acceptance evidence.

The capture-fidelity defect is **closed for the inspected standard CLI route for every app below**: large text visibly differs from baseline. Files' corrected viewport evidence also closes the former ignored-viewport evidence problem (800×900 comparison is now truthful; controller reports all requested dimensions matched). This does not mean large-text layouts pass. Imager's separate standalone environment route is outside these images; its concurrent fix does not invalidate this standard-route evidence.

## Exact individually viewed images

- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/camera/scale-baseline.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/camera/large-text.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/chat/scale-baseline.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/chat/large-text.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/code_editor/scale-baseline.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/code_editor/large-text.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/contacts/scale-baseline.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/contacts/large-text.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/disks/scale-baseline.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/disks/large-text.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/file-explorer/scale-baseline.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/file-explorer/large-text.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/maps/scale-baseline.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/maps/large-text.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/monitor/scale-baseline.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/monitor/large-text.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/imager/scale-baseline.png`
- `.superpowers/app-ui-2026-09-15/capture-corrected/captures/imager/large-text.png`

Each corresponding `manifest.json` records command, requested scale, source HEAD, diff and binary hashes. No additional narrow image was necessary to establish the observed 800px defects; no new narrow-flow coverage is claimed.

## Per-app findings

### camera — reviewed; fidelity closed; P2 layout

2× labels extend below the 30px top pills (12 MP/HDR/Timer), and Photo extends below its selected mode background. The mode labels remain distinct, shutter reachable, and preview usefully large; no failure of capture interaction is proven. Owner: app `apps/camera/src/app.rs:2529` mode switcher, `:3275` ctrl_pill and `:3429` bottom controls. Size control heights and label centering from measured scaled text, preserving the current 1× preview/manual geometry. Do not enlarge the preview margins. Still verify gallery, no-device, Scan/Video and pointer hit regions.

### chat — reviewed; fidelity closed; P1 layout

The existing workspace-title/presence collision worsens at 2×. Voice rows crowd names/icons; bottom identity status runs offscreen. In the conversation, attachment denial caption and filename collide, and edited/view-history text runs into reaction controls. Main message paragraphs themselves wrap and remain readable; top clipped message is consistent with the bottom-anchored scroll position, not a new clipping defect. Owner: app `apps/chat/src/render_chat.rs:536` sidebar header, `:1175` voice rows, `:781` identity row, `:2045` footer, `:2187` reactions and `:2665` gated embeds. Use measured row/metadata heights and reserve presence width; retain message workspace and 1× density. Still verify long draft, thread, readable attachment and keyboard/scroll interactions.

### code_editor — reviewed; fidelity closed; P1 layout

Code and line numbers enlarge while retaining the old row spacing; adjacent source lines overlap throughout the buffer. Problems tabs paint below their strip; oversized empty-state heading consumes the panel and its explanatory line is lost. Owner: app editor `apps/code_editor/src/editor_view.rs:94` LINE_HEIGHT=20, used for paint at `:1399`, `:1516` and hit testing at `:1319`; app panel `apps/code_editor/src/bottom_panel.rs:23` / `:31` and `:587` shared EmptyState. Update the single line-metric contract for drawing, scrolling, caret, selection and hit testing together. Repair shared single-scaling before sizing empty-state geometry. Existing baseline P2 empty dock default remains; it is not the cause of buffer overlap. Preserve 1× metrics and manually resized dock. Still verify selection/caret, diagnostics, completion, search and terminal at 2×.

### contacts — reviewed; fidelity closed; P1 layout

Sort/Group labels become oversized truncated fragments; list names/subtitles collide; selected contact's large name collides with its role and group chips. Phone/email labels sit against values, although address and notes correctly wrap onto extra lines. Owner: app `apps/contacts/src/app.rs:87` fixed ROW_H=62, `:2785` list row, `:3028` detail header and `:2944` detail composition; shared token text scaling also implicated by disproportionate Sort/Group labels. Fix single-scaling, then measure identity rows and value-label separation; allow scale-aware push layout where necessary. Preserve baseline two-pane density and user divider position. Still verify long names, pushed detail, form, notes scrolling and all field hit targets.

### disks — reviewed; fidelity closed; P1 layout

Drive names wrap beyond card bounds and collide with device/capacity text and usage bars. The visible dimmed detail also has header, legend and SMART metric collisions. Owner: app `apps/disks/src/app.rs:987` drive rows, `:1109` header, `:1647` legend and `:1941` SMART body. Measure card/name/metadata heights and reserve distinct value rows at 2× while retaining 1× sizing. The open drawer intentionally obscures the detail in both images; do not classify the overlay itself as a regression or infer the full closed-drawer responsive layout. Still verify closed drawer, SMART/partitions tabs and action confirmation.

### file-explorer — reviewed; fidelity and viewport evidence closed; P1 layout

Sidebar and column headers render roughly 4× baseline at requested 2×; sidebar text runs beyond the rail and crowded rows; Modified clips at the right. List/Grid and empty-state text are similarly oversized, while breadcrumb/status are closer to 2×. This mixed scaling is a concrete single-scaling contract failure, not a reason to globally expand all manual dimensions. Owners: `apps/file-explorer/src/places_sidebar.rs:44` ROW_H=34 and `:383` theme token passed to draw_text; shared `ui/src/theme.rs:728` scales typography tokens and `ui/src/text_render.rs:1368` scales draw styles again. Correct scale once, then re-evaluate rail/column/row measurements at true 2×; preserve 100% density and user dimensions. Empty directory whitespace is appropriate. Populated list/grid, name editing, drag/drop and narrow drawer remain unverified at 2×.

### maps — reviewed; fidelity closed; P1 layout

Route duration and arrival time overlap, and step labels crowd distance labels in the fixed bands. Mode/Leave controls overflow vertically, while navigation instruction and distance nearly touch. Directions, map and End remain visible; no lost route is asserted. Owner: app `apps/maps/src/app.rs:3636` draw_directions, `:3861` fixed 46px route-step bands, `:3957` time picker and `:324` navigation surface. Derive summary/step heights from measured scaled lines and give distances a separate measured row; size mode controls without sacrificing the map at 1×. Still verify collapsed drawer, search/results, transit and map recovery after dismissal.

### monitor — reviewed; fidelity closed; P1 layout

Most typography is roughly 4× baseline: clock and uptime overlap, summary labels/values cover charts, toolbar labels leave their pills, process rows are clipped into unreadable slices, and bottom process metadata/actions collide. Owner: shared token+renderer double-scale contract (`ui/src/theme.rs:728`, `ui/src/text_render.rs:1368`) plus app `apps/monitor/src/app.rs:171` ROW_H=22, `:1369` header, `:1681` summary, `:2289` table rows and `:2684` detail pane. Fix single-scaling first, then use coherent measured row/card/tool heights and hit metrics. Preserve 1× process density and manually set dimensions; don't redesign based on erroneous 4× output. Still verify Resources, filtering/sorting, selected detail, scrolling and stop confirmation.

### imager — reviewed; standard-route fidelity closed; P1 layout

Heading/subtitle overlap; selected image title/path/configuration status collide and extend beyond the card; release and recent-item names collide with secondary metadata. Continue remains visible and legible, and numbered steps remain distinct; source-page failure is readability rather than proven blocked progression. Owner: app `apps/imager/src/app.rs:1718` title, `:1774` drop zone, `:1870` selected card, `:2057` release row and `:2156` recents. Derive heights from scaled wrapped content and advance each subsequent section from its measured bottom; retain 1× wizard spacing and user dimensions. Still verify empty source, Drive, Customize, confirmation, progress/result and footer reachability at 2×.

## Boundaries and priority

Highest leverage: shared single-scale contract (Files/Monitor and shared empty states/controls), then code editor's coherent line metrics, then app-owned measured rows and headers. All nine capture fidelity issues are closed only for the exercised command routes. These static frames do not establish pointer/keyboard operability, alternate routes, scrolling completion or live recovery. Existing baseline findings remain unless explicitly closed above. No broad whitespace reduction or global manual-dimension scaling is recommended.
