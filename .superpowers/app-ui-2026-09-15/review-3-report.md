# App UI review 3 — fresh visual review

Read-only review of exactly 16 assigned apps. No app/framework edits or builds. Controller built supported binaries fresh and captured the current worktree. No historical captures substituted. All image paths below are under `.superpowers/app-ui-2026-09-15/captures/`; manifests record exact commands, binary SHA256, source HEAD and working-diff SHA256. Only explicitly listed images were viewed. Windows was subsequently built/captured on Fedora44 ARM64 from a controller-verified immutable source snapshot; all 16 assigned apps now have actual fresh images reviewed.

## apps/settings

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/settings/wide.png`, `captures/settings/narrow.png`, `captures/settings/large-text.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/settings`; `captures/settings/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Main flow:** Choose category → read/change grouped preferences.

**Source reviewed:** apps/settings/src/settings_app.rs:297,390; apps/settings/src/sidebar.rs:44.

**Composition/ownership:** AppShell navigation and scrolling are shared; category heading spacing and 1040px cap limited to External Accounts are app-authored.

**Findings:**

1. **P2 — excessive vertical preamble in settings pages.** Wide External Accounts starts its first actionable Email row at y464; narrow Region & Formats starts its form at y291 despite only a short heading/subtitle. `apps/settings/src/settings_app.rs:297` adds 14px/28px spacer blocks around a heading plus outer container padding; account card/status copy adds further spacing. Desired change: tighten page-level/preamble spacing while keeping related preference rows grouped, bringing first action above the fold. Ownership: app composition plus shared container spacing.
2. **P1 — inconsistent 200% scaling.** In large-text.png page headings/help enlarge, while sidebar names, search, account card titles and Add account remain small. Desired change: consistent readable text scale for navigation and controls. Ownership: mixed custom sidebar/control rendering and framework text widgets; root framework audit must establish one scale contract before local fixes. References `apps/settings/src/sidebar.rs:210`, `apps/settings/src/settings_app.rs:297`.

**Needed verification:** Default selected category, narrow selected content and sidebar; long account form at 200%.

## apps/sheets

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/sheets/wide.png`, `captures/sheets/narrow.png`, `captures/sheets/large-text.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/sheets`; `captures/sheets/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Supplemental images reviewed:** `captures/sheets/editing-wide.png`, `captures/sheets/editing-narrow.png`. Commands/environment/status recorded in `supplement-results.json`; same fresh binaries as initial captures. Normal-scale main-flow follow-ups were inspected individually.

**Main flow:** Select cell → enter value/formula → format → switch sheet/export.

**Source reviewed:** apps/sheets/src/app.rs:50,5381; apps/sheets/src/grid.rs:2288.

**Composition/ownership:** 44px menu + 34px formula + 34px tabs prioritize grid; manually positioned app chrome requires text-scale verification.

**Findings:**

No normal-scale composition defect identified in the viewed sample: menus/formula/tabs leave the grid most of the window; horizontal overflow at 480px is appropriate spreadsheet behavior. **P1 validation gap:** large-text.png still uses normal-size text because the custom shot handler ignores scale (see capture audit); no 200% pass claimed.

**Needed verification:** Default sample, narrow grid, editing/pointmode and format menu at 200%.

## apps/slides

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/slides/wide.png`, `captures/slides/narrow.png`, `captures/slides/large-text.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/slides`; `captures/slides/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Supplemental images reviewed:** `captures/slides/inspector-wide.png`, `captures/slides/inspector-narrow.png`. Commands/environment/status recorded in `supplement-results.json`; same fresh binaries as initial captures. Normal-scale main-flow follow-ups were inspected individually.

**Main flow:** Select slide → edit text/object → inspect formatting → present.

**Source reviewed:** apps/slides/src/app.rs:248,3593,3662; apps/slides/src/toolbar_tools.rs:14.

**Composition/ownership:** 212px rail and 296px inspector use shared flap behavior, app-owned geometry and 56px toolbar.

**Findings:**

No normal-scale composition defect identified: responsive toolbar/rail/inspector give the slide canvas room. Portrait letterboxing follows slide aspect ratio and is workspace, not unnecessary chrome. **P1 validation gap:** large-text.png ignores requested scale (see capture audit).

**Needed verification:** Default selected object, narrow_closed, inspector overlay and toolbar_menu at 200%.

## apps/snippets

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/snippets/wide.png`, `captures/snippets/narrow.png`, `captures/snippets/large-text.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/snippets`; `captures/snippets/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Supplemental images reviewed:** `captures/snippets/detail-wide.png`, `captures/snippets/detail-narrow.png`. Commands/environment/status recorded in `supplement-results.json`; same fresh binaries as initial captures. Normal-scale main-flow follow-ups were inspected individually.

**Main flow:** Choose folder/search → select snippet → read/edit/copy/expand.

**Source reviewed:** apps/snippets/src/app.rs:46,783,1091.

**Composition/ownership:** Three-pane layout reserves 232px nav + 340px list; app switches to push detail below 560px and overlay nav below 820px.

**Findings:**

1. **P1 — 200% list/detail overlap.** large-text.png shows title/subtitle/counts colliding in virtually every list row, title chips overlaid on Meeting note, and action row extending beyond the viewport. `apps/snippets/src/app.rs:46` fixes list/row widths and heights; detail paint starts at `:1091`. Desired change: scale-aware row heights, measured title/metadata placement and action overflow; switch to push detail when effective content width is insufficient. Ownership: app geometry; shared text measurement may contribute.
2. **P2 — narrow nav toggle overlays search text.** narrow.png hamburger covers the beginning of the search placeholder. Search always starts at area.x0+12 (`apps/snippets/src/app.rs:1732`), while toggle is painted on top (`:877`). Desired change: reserve a leading slot or move toggle outside search. Ownership: app.

**Needed verification:** Default library, empty, narrow detail and long snippet/title at 200%.

## apps/store

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/store/wide.png`, `captures/store/narrow.png`, `captures/store/large-text.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/store`; `captures/store/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Supplemental images reviewed:** `captures/store/detail-wide.png`, `captures/store/detail-narrow.png`. Commands/environment/status recorded in `supplement-results.json`; same fresh binaries as initial captures. Normal-scale main-flow follow-ups were inspected individually.

**Main flow:** Browse/filter/search → inspect application → install/update.

**Source reviewed:** apps/store/src/app.rs:1000,1014,3889; apps/store/src/shots.rs.

**Composition/ownership:** App explicitly stacks catalogue at text scale >1.25; retained widgets provide shared layout. Need actual catalogue/detail evidence before judging density.

**Findings:**

1. **P2 — wide card actions exceed card boundary.** Terminal Details/Open extend below its card into the gap, while adjacent Files/VLC action baselines differ. `apps/store/src/app.rs:2680` fixes cards to 180px; `apps/store/src/widgets/card.rs:145` combines variable description height, gaps, spacer and 40px actions. Desired change: intrinsic or sufficiently measured row height with aligned action baselines and internal bottom padding. Ownership: app card constraints; framework intrinsic sizing should also be checked.
2. **P2 — browse header consumes half narrow viewport.** narrow.png first item begins near y445 after title/subtitle/search/utilities/source count/filters/Your apps/count/Ready to use. `apps/store/src/app.rs:2216,2650` assembles these layers. Desired change: merge redundant counts/headings, reduce section gaps, keep search/filter hierarchy compact. Ownership: app.
3. **P1 shared scale inconsistency:** large-text.png card text enlarges but search/filter/action buttons remain small. See shared framework review; app already stacks cards at large text (`:1014`).

**Needed verification:** Default catalogue, app detail, narrow catalogue and 200% cards/decision controls.

## apps/terminal

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/terminal/wide.png`, `captures/terminal/large-text.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/terminal`; `captures/terminal/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Supplemental images reviewed:** `captures/terminal/shell-wide.png`, `captures/terminal/shell-narrow.png`. Commands/environment/status recorded in `supplement-results.json`; same fresh binaries as initial captures. Normal-scale main-flow follow-ups were inspected individually.

**Main flow:** Enter shell command → read output → scroll/select/copy.

**Source reviewed:** apps/terminal/src/main_body.rs:2095,2113,3561.

**Composition/ownership:** Terminal derives font/gutter from theme and gives grid available area. Sparse cold start is intentional; output scenario is needed to judge density.

**Findings:**

No density defect identified in the viewed cold-start screen. It contains only the idle terminal mark; this does not establish populated shell output/selection behavior. Environment-based 200% capture visibly enlarges the mark. Populated `shell-wide.png` and `shell-narrow.png` subsequently reviewed: clear command/output hierarchy, no identified composition issue.

**Needed verification:** Fresh cold start plus populated VT output, narrow/200% cell hit alignment.

## apps/themes

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/themes/wide.png`, `captures/themes/narrow.png`, `captures/themes/large-text.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/themes`; `captures/themes/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Supplemental images reviewed:** `captures/themes/controls-wide.png`, `captures/themes/controls-narrow.png`, `captures/themes/live-wide.png`, `captures/themes/live-narrow.png`. Commands/environment/status recorded in `supplement-results.json`; same fresh binaries as initial captures. Normal-scale main-flow follow-ups were inspected individually.

**Main flow:** Select theme → preview → change color/font/radius → apply.

**Source reviewed:** apps/themes/src/app.rs:47,54,832; apps/themes/src/customizer.rs:546.

**Composition/ownership:** 272px gallery + 344px customizer flank preview; narrow stack places a fixed 560px preview before customizer.

**Findings:**

1. **P1 — 200% gallery/preview overlap.** large-text.png preset names exceed272px gallery width and overlap subtitles; preview typography and badge/list samples overlap. Fixed58px row at `apps/themes/src/widgets.rs:33,281` and fixed preview sample geometry (`apps/themes/src/preview.rs:140`) do not reflow. Desired change: measured row heights/truncation or wrapping, scale-aware specimen geometry, and a wider/stacked gallery as needed. Ownership: app geometry.
2. **P2 — narrow customization begins below a 560px specimen.** narrow.png shows controls beginning around y650 behind the open fixture sidebar; fixed `NARROW_PREVIEW_H` at `apps/themes/src/app.rs:54,832` pushes primary customization below fold. Desired change: compact preview or preview/customizer switch so controls are immediately reachable. Ownership: app. Closed-gallery `controls-narrow.png` subsequently confirms long stacked specimen/customizer; controls are reachable by scrolling. Do not treat intentionally open overlay as a bug.

**Needed verification:** Default and narrow closed-gallery state, scroll to customizer, 200% labels and apply.

## apps/totp

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/totp/wide.png`, `captures/totp/narrow.png`, `captures/totp/large-text.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/totp`; `captures/totp/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Supplemental images reviewed:** `captures/totp/add-wide.png`, `captures/totp/add-narrow.png`. Commands/environment/status recorded in `supplement-results.json`; same fresh binaries as initial captures. Normal-scale main-flow follow-ups were inspected individually.

**Main flow:** Find account → copy current code; Add account → enter/scan secret.

**Source reviewed:** apps/totp/src/app.rs:48,876,1105; apps/totp/src/list.rs:36.

**Composition/ownership:** Fixed 150px header separates count/actions from search; 76px rows (108px narrow) are app-owned.

**Findings:**

1. **P1 — 200% account/action collisions.** large-text.png Edit selected/Add account text overlap their button boundaries; account names and addresses touch/overlap and8-digit code crowds its identity. Fixed action widths/header layout (`apps/totp/src/app.rs:48,1111`) and row layout (`apps/totp/src/list.rs:36`) lack scale-aware measurement. Desired change: measured action sizing or wrapped header, stacked account/code layout at effective narrow widths, and rows sized to scaled text. Ownership: app.
2. **P2 — wide list makes matching identity to code harder than necessary.** wide.png stretches5 accounts edge to edge with large empty span between identity and code. Desired change: readable maximum list width or bounded identity/code columns without shrinking codes. Ownership: app list layout. Reference `apps/totp/src/app.rs:1145`.

**Needed verification:** Default vault, narrow list, add and add-advanced, 200% account/action labels.

## apps/tube

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/tube/wide.png`, `captures/tube/narrow.png`, `captures/tube/large-text.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/tube`; `captures/tube/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Supplemental images reviewed:** `captures/tube/search-wide.png`, `captures/tube/search-narrow.png`. Commands/environment/status recorded in `supplement-results.json`; same fresh binaries as initial captures. Normal-scale main-flow follow-ups were inspected individually.

**Main flow:** Browse/search videos → select video → watch → choose next video.

**Source reviewed:** apps/tube/src/app.rs:43,2080; apps/tube/src/screens.rs:354,1682.

**Composition/ownership:** 300px minimum responsive card grid with 18px gaps; watch up-next panel is 360px app-authored.

**Findings:**

1. **P1 — 200% card metadata overlaps and clips into next row.** large-text.png titles, channel names and view counts paint over one another and next thumbnails; Home heading is partly behind thumbnails. `apps/tube/src/screens.rs:222,238,248,354` uses intrinsic Flex composition, suggesting shared Flex/Text measurement must include draw scale. Desired change: measured scaled text heights, coherent card metadata blocks, enough row height before next thumbnail; rail labels also need reflow. Ownership: likely framework Flex/text measurement first, app card minimum widths/rail density second. Normal-scale wide/narrow feed has clear grouping.

**Needed verification:** Default feed, narrow closed rail, watch/search, 200% long titles.

## apps/typst_editor

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/typst_editor/wide.png`, `captures/typst_editor/narrow.png`, `captures/typst_editor/large-text.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/typst_editor`; `captures/typst_editor/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Supplemental images reviewed:** `captures/typst_editor/errors-wide.png`, `captures/typst_editor/errors-narrow.png`. Commands/environment/status recorded in `supplement-results.json`; same fresh binaries as initial captures. Normal-scale main-flow follow-ups were inspected individually.

**Main flow:** Edit source → inspect compiled page/errors → export.

**Source reviewed:** apps/typst_editor/src/app.rs:103,928,1088.

**Composition/ownership:** 46px toolbar, adjustable editor/preview split, compact pane switch under 760px; diagnostics conditional.

**Findings:**

1. **P2 — Export PDF disappears in medium-width toolbar.** wide.png has Export PDF;800px large-text.png (actually normal-scale) has neither Export nor overflow. `apps/typst_editor/src/app.rs:1128` hides Export below 1120px claiming overflow, but full toolbar `:1111-1186` has no menu; compact mode starts below 760px (`:940`). Desired change: overflow access at 760–1119px or keep Export visible. Ownership: app. Verify pointer export route at 760,800,1119px.
2. **P1 validation gap:** custom shot ignores200% (capture audit). No other normal-scale density defect identified in wide/narrow editor.

**Needed verification:** Default compilation, narrow source/preview switch, diagnostics and 200% toolbar.

## apps/video-player

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/video-player/wide.png`, `captures/video-player/narrow.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/video-player`; `captures/video-player/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Supplemental images reviewed:** `captures/video-player/library-wide.png`, `captures/video-player/library-narrow.png`. Commands/environment/status recorded in `supplement-results.json`; same fresh binaries as initial captures. Normal-scale main-flow follow-ups were inspected individually.

**Main flow:** Open media → play/pause/seek → select queued/recent item.

**Source reviewed:** apps/video-player/src/app.rs:2138,2165.

**Composition/ownership:** Edge-to-edge player with floating Open and Library buttons; library is full width below 640px, otherwise 360px. Player widget is shared.

**Findings:**

No normal-scale composition defect identified: open/library are compact and video is edge to edge with aspect-preserving letterboxing; transport remains accessible at480px. **P1 validation gap:** custom shot ignores200%, so large-text behavior remains unverified.

**Needed verification:** Default clip, empty/open, library, narrow and 200% controls.

## apps/video_editor

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/video_editor/wide.png`, `captures/video_editor/compact.png`, `captures/video_editor/narrow.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/video_editor`; `captures/video_editor/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Main flow:** Import → select/trim/arrange timeline clips → adjust properties → export.

**Source reviewed:** apps/video_editor/src/app.rs:58,2015,2112.

**Composition/ownership:** 48px toolbar + fixed 304px timeline; 268px bin/300px inspector flap; transport 58px. App owns vertical allocation.

**Findings:**

No normal-scale collision identified in wide/800px compact/480px narrow captures. Dense timeline and inspector are related editing controls; collapsed bin/inspector recover preview width. Fixed304px timeline should additionally be checked at short heights, but no defect asserted from current evidence. **P1 validation gap:** custom shot ignores200%.

**Needed verification:** Default project, narrow closed panes, inspector, empty and export; short-height/200% timeline versus preview.

## apps/vpn

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/vpn/wide.png`, `captures/vpn/narrow.png`, `captures/vpn/large-text.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/vpn`; `captures/vpn/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Supplemental images reviewed:** `captures/vpn/detail-wide.png`, `captures/vpn/detail-narrow.png`. Commands/environment/status recorded in `supplement-results.json`; same fresh binaries as initial captures. Normal-scale main-flow follow-ups were inspected individually.

**Main flow:** Import configuration → select tunnel → connect/disconnect → inspect status.

**Source reviewed:** apps/vpn/src/app.rs:110,1242,1576,1942.

**Composition/ownership:** 440px list; at <720px single pane and header doubles to 144px. Fixed app geometry with shared dialog.

**Findings:**

1. **P1 — 200% empty-state text is oversized and cropped horizontally.** large-text.png No VPN connections becomes roughly4× baseline and subtitle runs off both edges. App invokes shared `EmptyState::paint` (`apps/vpn/src/app.rs:2031`); desired change: apply scale once and wrap message inside available content width. Ownership: framework EmptyState/text contract, app should consume corrected widget.
2. **P2 — narrow header reserves144px for controls ending at88px.** narrow.png leaves a56px blank trailing band. `apps/vpn/src/app.rs:1245` doubles header unconditionally below720px. Desired change: derive height from occupied rows and notices. Ownership: app. `detail-wide.png` and `detail-narrow.png` subsequently reviewed: connection/status groups are readable at normal scale; the narrow header band remains unnecessary.

**Needed verification:** Default connections, narrow detail, empty/import and error banner, 200% controls.

## apps/weather

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/weather/wide.png`, `captures/weather/narrow.png`, `captures/weather/large-text.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/weather`; `captures/weather/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Supplemental images reviewed:** `captures/weather/detail-wide.png`, `captures/weather/detail-narrow.png`. Commands/environment/status recorded in `supplement-results.json`; same fresh binaries as initial captures. Normal-scale main-flow follow-ups were inspected individually.

**Main flow:** Select/add city → read current/hourly/daily conditions.

**Source reviewed:** apps/weather/src/app.rs:59,745; apps/weather/src/core.rs:57.

**Composition/ownership:** 340px overview (68px collapsed rail), shared NavShell push transition; hero/cards/gaps app-owned.

**Findings:**

1. **P1 — 200% weather surfaces become unreadable.** large-text.png city/date/region overlap, saved-city names/subtitles/temperatures collide, hero temperature covers condition, tile values collide with units, hourly labels clip and daily values run together. Fixed62px chips/168px hero/78px metric tiles (`apps/weather/src/core.rs:57-70`) and custom header/hero paint (`:2090,2195,2367`) do not measure scaled text. Desired change: scale-aware card/row heights, stacked header and hero details, responsive effective-width breakpoint for340px city pane. Ownership: app custom geometry and shared text contract. Normal-scale wide grouping is clear; narrow defaults to city list. `detail-narrow.png` subsequently confirms the pushed forecast and card stack remain readable at normal scale.

**Needed verification:** Default overview/detail, narrow detail, scrolled daily forecast, 200% tiles.

## apps/windows

**Status:** visual-reviewed (fresh Fedora44 ARM64 captures; macOS build limitation overcome with native Linux build).

**Capture/provenance:** Viewed `captures/windows/wide.png`, `captures/windows/narrow.png`, `captures/windows/large-text.png`. `captures/windows/manifest.json` records Fedora44 ARM64, source manifest `linux-source-hashes.json`, binary SHA256 `012cbd869e276bf83814ee18169eaac7cc88495c1535606435cdcd9ed2ae0885`; controller verified current 3,614 files before build. No stale binary/capture used.

**Main flow:** Choose Windows application/VM → launch/install → inspect state/recovery.

**Source reviewed:** apps/windows/src/app.rs:28,1451,1493.

**Composition/ownership:** 296px sidebar; below 860px push detail; app paints geometry and scroll.

**Findings:**

1. **P1 — 200% header/sidebar collisions.** large-text.png title overlaps subtitle/Add button, appliance heading/help/status overlap, and runtime badges collide with app names. `apps/windows/src/app.rs:28,1493` uses fixed sidebar/row heights and painted offsets. Desired change: measured scaled text height, distinct groups, badges that size to labels, scale-aware push/detail threshold. Ownership: app custom geometry.
2. **P2 — VM appliance administration displaces app launching.** wide/narrow.png puts Add app, appliance status/error, explanatory text and Choose ISO above the application list; first app begins near y347. Detailed runner/capsule/access/management data occupies most of the wide selection page. Desired change: separate appliance setup/status as a secondary group or route; emphasize app list/Open action and disclose technical capsule details on demand. Ownership: app composition at `apps/windows/src/app.rs:1493`.

**Needed verification:** Default, narrow detail, running, VM detail and error recovery at 200%.

## apps/word-processor

**Status:** visual-reviewed (listed fresh captures; main-flow follow-ups noted).

**Capture/provenance:** Viewed `captures/word-processor/wide.png`, `captures/word-processor/narrow.png`. Fresh controller build, binary `/Users/xaver/dev/qos-macos-shell/target/debug/word-processor`; `captures/word-processor/manifest.json` records source HEAD `47fc2425a643a07ebf9c4f127fb5579ba42f77a7`, worktree diff hash and binary SHA256. Requested-scale fidelity follows audit below.

**Main flow:** Open/create document → write/style → page layout → save/export.

**Source reviewed:** apps/word-processor/src/widget.rs:233,248,715,8734.

**Composition/ownership:** Shared toolbar height; app owns 24px ruler, 32px layout row, optional 250px styles/230px outline and page canvas.

**Findings:**

1. **P2 — default narrow writing surface fits paper at55%, making body/caret tiny.** narrow.png shows an almost unreadable Start writing placeholder despite ample vertical space. `apps/word-processor/src/widget.rs:5352` derives zoom from full paper width. Desired change: readable narrow editing mode (reflow or fit text area) with explicit page-fit option; preserve document page geometry. Ownership: app. Verify actual paragraph editing/selection at480px before choosing implementation.
2. **P1 validation gap:** custom shot ignores200%. Normal wide blank page/chrome is appropriately focused.

**Needed verification:** Default document, blank, narrow compact toolbar, styles/history and 200%.

## Shared patterns

Most of these apps draw fixed logical-pixel geometry themselves; shared token changes alone cannot establish 200% fitness. Source uses shared flaps/NavShell in several apps, but app-authored thresholds/column widths remain distinct. Editor whitespace must be assessed as workspace, not automatically removed.

## Capture fidelity audit (confirmed from source)

1. **P1 — eight custom screenshot handlers do not apply the requested CLI text scale.** Slides (`apps/slides/src/main.rs:56`), Sheets (`apps/sheets/src/main.rs:165`), Typst (`apps/typst_editor/src/main.rs:91`), Video Player (`apps/video-player/src/main.rs:131`), Video Editor (`apps/video_editor/src/main.rs:180`), Word Processor (`apps/word-processor/src/main.rs:510`) construct unscaled themes; Terminal (`apps/terminal/src/main.rs:166`) reads only `QOS_SHOTS_TEXT_SCALE`. Store likewise reads only that environment variable (`apps/store/src/shots.rs:31`). Thus captures invoked only with `--text-scale 2` cannot establish large-text fitness for these apps. Desired visible change: actual enlarged type and corresponding layout in acceptance images. Ownership: custom app shot handlers bypass the shared launcher's `shot_theme` (`ui/src/launch.rs:822`). Verify true scale from render metrics / image changes, then rerun layout captures. This is a validation defect, not evidence of live UI overlap.
2. **Capture-state caveat:** Themes default narrow fixture opens the gallery overlay (`apps/themes/src/app.rs:275`); use `QOS_THEMES_SHOT=scroll` to close it and expose scrolled customizer (`apps/themes/src/lib.rs:61`). Default demo is deliberately editable; `QOS_THEMES_SHOT=live` is actual presets-only catalog.
3. **Capture-state caveat:** Settings viewport override takes the responsive path (`apps/settings/src/main.rs:40`) and does not select filename-specific category fixtures. Category screenshot filenames are interpreted only by the no-viewport path (`apps/settings/src/shots.rs:249`).

## Primary-flow follow-up commands (controller-owned capture execution)

All take `--shots PATH --viewport WxH` unless noted. Actual package binary names come from the inventory.

| App | Follow-up selector |
|---|---|
| settings | Filename-specific category fixture without `--viewport`; responsive with viewport is its own selected category |
| sheets | `--scenario editing`, `--scenario format` |
| slides | `--scenario narrow_closed`, `--scenario inspector`, `--scenario toolbar_menu` |
| snippets | `--scenario detail`, `--scenario empty` |
| store | `--scenario detail`; `QOS_SHOTS_TEXT_SCALE=2` for large text |
| terminal | `--scenario shell`; `QOS_SHOTS_TEXT_SCALE=2` for large text |
| themes | `QOS_THEMES_SHOT=scroll`, `QOS_THEMES_SHOT=live` |
| totp | `--screen add`, `--screen add-advanced` |
| tube | `--route search:rust`, `--play-demo` |
| typst_editor | `--scenario errors`, `--scenario preview` |
| video-player | `--scenario library`, `--scenario empty` |
| video_editor | `--empty`, `--scenario filters`, `--scenario format` |
| vpn | `--scenario detail`, `--scenario import` |
| weather | `--scenario detail`, `--scenario scrolled` |
| windows | `--scenario vm-detail`, `--scenario error-compact` |
| word-processor | `--styles`, `--history`, `--comments` |

## Final review gate / remaining evidence limits

- Exactly 16 assigned apps marked visual-reviewed after actual fresh image inspection, including Windows native Linux evidence. No implementation, builds, commits, or subagents were performed by this reviewer.
- Main-route supplements cover Sheets editing, Slides inspector, Snippets detail, Store detail, Terminal populated shell, Themes live/customizer, TOTP add, Tube search, Typst errors, Video Player library, VPN detail, and Weather pushed forecast.
- True 200% acceptance remains missing for six custom-handler apps: Sheets, Slides, Typst, Video Player, Video Editor, Word Processor. Terminal and Store scale successfully through the environment used by controller.
- Windows VM detail/recovery, Tube watch/playback, TOTP advanced add, document-rich Word Processor, and short-height Video Editor are additional verification states, not claimed reviewed. Listed static captures do not prove live pointer/keyboard interaction.
- Highest-leverage order: repair shared scale/measurement contract and custom capture fidelity first; then app-owned measured geometry for Snippets/Themes/TOTP/Weather/Windows; then reduce browse/settings/connection preambles and repair Typst medium-width Export route.
