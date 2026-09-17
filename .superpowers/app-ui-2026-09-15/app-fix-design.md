# Normal-size app UI balance implementation plan

> **For agentic workers:** Execute task-by-task with the executing-plans skill. This is a read-only design handoff; production implementation and reviews run sequentially in the shared checkout under the controller's ownership. Checkboxes describe future work, not completed fixes.

**Goal:** Repair confirmed normal-100% density, grouping, overflow and action-reachability findings without reducing useful workspace or removing manual control.

**Architecture:** Keep each app's existing state/action model. Change composition through existing Card, Actions, Form, Toolbar, NavSplit, SidebarDrawer and measured text paths where they fit; for immediate-mode apps compute one geometry description and consume it in drawing, pointer events, keyboard navigation and accessibility. Do not create another layout framework.

**Tech stack:** Rust, ui Widget/Taffy/Vello, TextRenderer, Automerge content state and qos_actions.

**Spec:** `docs/superpowers/specs/2026-09-15-app-ui-balance.md`.

## Evidence and boundaries

Source inspected on 2026-09-15; baseline evidence is `docs/superpowers/audits/2026-09-15-all-app-ui-review/review-{1,2,3}.md`. This planning pass read reports/source; it did not recapture or independently inspect the baseline PNGs. Line references below are discovery anchors; functions are the durable reference if preceding shared work moves lines.

- Ordinary 100%, 1200×800, 800×700 and 480×900 are first-class targets. Add 800×480 to reveal height starvation. Finish with true 200% at 800×900 after the shared scale/capture contract lands.
- Preserve every action, keyboard access, selection, draft/edit state and explicitly chosen dimensions. Disclosure needs a visible pointer trigger, keyboard activation, Escape dismissal/focus return, and accessible name/state.
- Measure with the same font, weight and width constraint used to paint. Never shrink fonts to disguise overlap.
- No global padding changes, reinterpretation of explicit sizes, dependency updates, pushes or merges. No Scratchpad edits. Do not modify capture-fidelity-owned main.rs/shots.rs files during these app tasks.
- Large-text/double-scale investigation remains a separate shared task. These changes consume its final measurement API; they must not compensate locally with reciprocal scale factors.
- View-only disclosure, split position and compact route state stay out of replicated documents. Existing document mutations continue through canonical actions.

## Existing primitives: use with honest limits

`ui/src/widget/resizable_split.rs` exposes `ResizableSplit::new(left, right, SplitOrientation::Vertical)`, `with_split_position(f32)` and `split_position()`. It keeps view state and supplies pointer dragging with 0.1–0.9 clamps. **It currently has no keyboard/accessibility implementation.** Reuse it when app child ownership fits; complete semantics in the shared widget in a separately scoped dependency, or give the app named Increase/Decrease/Reset pane-height buttons using the same setter. Do not claim pointer dragging alone meets acceptance. Immediate-mode API/DB should avoid a wholesale rewrite solely to wrap their painted halves; reuse the split interaction policy with a small local state/geometry object and accessible controls.

`ui/src/widget/toolbar.rs` has overflow, but `accessibility()` only emits `Role::Toolbar`, without item children, and width calculation is not the required renderer measurement contract. Do not migrate accessible custom controls into it until the shared toolbar work includes action identity, keyboard navigation and semantic children. Existing app Button/menu paths are the safer immediate migration.

`Container::with_centered_max_width`, intrinsic-height Columns, CardGrid and Button::fit_content are available. Store can use these existing paths; confirm intrinsic measurement actually reaches CardGrid before removing its fixed height.

## Task group A — reclaim editing height

### A1. API Tester: saved objects first; compact adjustable request region

**Files:** `apps/api_tester/src/app.rs`, `responsive.rs`, `navrow.rs`; extend `tests/a11y.rs` and app tests.

**Current:** `build_widgets` (~655) creates ten `manage_buttons` before saved collections. `compute_layout` (~1196) reserves 42% of remaining height, minimum 150, for editor regardless of content. Body field, response status/switcher and scroll body derive from those rectangles.

- [ ] Keep environment selector and saved collections/requests high in the rail; keep New collection/Save request near their targets. Put rename/delete and environment-variable maintenance behind clearly labelled Collection/Request/Environment actions. Reuse the existing `Cmd::OpenForm(FormMode::...)`, `Cmd::DeleteRequest`, `Cmd::DeleteEnvironment` sink and Buttons; do not implement alternative document mutations.
- [ ] Add local disclosure state and expose only visible buttons through layout, `draw_sidebar`, `handle_event`, `children`/`children_mut`, test rectangles and focus traversal. A closed menu's old rectangles must be cleared. Retain the ten stable `api.*` identifiers; selected-object actions announce their target and retain existing availability rules.
- [ ] Add local `request_height_override: Option<f64>` plus drag state and one divider rect to `Layout`. Automatic height fits the active Params/Headers row block including its add-row, Auth fields, or a useful Body editor viewport, bounded so a response region remains usable. Compute from actual visible row/text metrics; do not grow on every keystroke once manually resized.
- [ ] Apply the override in `compute_layout`, update body input placement and `draw_editor`/`draw_response`, handle divider events before editor selection, and add named Increase request area / Decrease request area / Reset request area controls to the same command path. Clamp effective geometry for short windows without overwriting the stored user preference; restore when the window grows.
- [ ] Keep Send, all request tabs and response modes reachable with disclosure open/closed. `active_send_binding`, outgoing variable interpolation and existing canonical-action tests stay unchanged.

**Acceptance:** Default rail shows saved requests before maintenance; three parameter rows no longer leave a large void above the response. Pointer select/edit/Send and keyboard Send work. Test drag, keyboard resize, Reset, Body/Auth tab switches, narrow rail open/Escape, 800×480 shrink/grow, and resize preference survival. Capture Params with response, Body and expanded management at 1200/800/480.

### A2. DB Client: query/result divider

**Files:** `apps/db_client/src/app.rs`, only if needed `session.rs`; tests in app.rs.

**Current:** `relayout` (~552/602) uses `(avail * 0.42).clamp(120,360)` then a 30px result strip. `editor` and `grid` own text/selection; `draw_query_tab`, `click_query_toolbar`, `on_key_pressed`, `handle_event`, `accessibility`, `perform_action` are integration paths.

- [ ] Introduce local automatic/manual query height using the A1 policy, with default based on query line metrics plus a small editing allowance (not the whole content fraction). Reserve toolbar and result-message height first. Keep Data/Structure tab geometry unchanged.
- [ ] Add a separator below editor; one computed rectangle controls paint and drag. Named keyboard height controls/Reset route through the same setter. Keep Run/snippets/transaction commands and editor shortcut handling ahead of unrelated global keys.
- [ ] Store the manual preference on the app, not query CRDT; preserve it across result updates and Query/Data/Structure switching. Preserve editor caret and result selection on relayout.

**Acceptance:** Four-line demo query yields substantially more visible result rows; long query remains editable/scrollable. Drag and keyboard resizing survive Run and tab changes. Short-height view keeps Run, a usable editor and result strip reachable. Test connection drawer and many connections to ensure no regression outside query layout.

### A3. Code Editor: empty dock initially collapsed

**Files:** `apps/code_editor/src/app.rs`, `bottom_panel.rs` only if required for toggle placement.

**Current:** constructor (~607) initializes `panel_visible: true`; `build_editor_column` (~3267), draw and accessibility conditionally include panel. Existing action dispatch (~2358–2391), terminal paths and status controls already set visibility.

- [ ] Default `panel_visible` to false when no restored explicit preference exists. Do not continuously auto-close the panel whenever Problems count reaches zero; explicit user opening is authoritative.
- [ ] Audit existing Toggle panel, Problems and Terminal actions, command palette/status entry points and `accessibility` (~4305) so opening is still visible to pointer and keyboard users. Keep existing manual dock sizing and terminal-open behavior.
- [ ] Update tests that assume launch-visible; retain tests that intentionally open Problems/Terminal. Do not invent preference persistence if the app currently has no storage for it.

**Acceptance:** Fresh empty Problems dock consumes no code height; click/keyboard toggle opens it, explicit open remains through empty diagnostics updates, terminal commands open the correct dock and closing/reopening preserves contents. Capture fresh/Problems/Terminal at 800×700 and 480×900.

## Task group B — medium-width workspace routing

### B1. Containers

**Files:** `apps/containers/src/app.rs`, `panels.rs`; `tests/interaction.rs`.

**Current:** `COMPACT_VIEWPORT_WIDTH` is 600; `compact_layout_for`/`uses_compact_layout` (~2657) permit three columns at 800. `draw_containers`, `draw_container_row_layout`, `draw_row_stats`, `draw_row_actions`, `draw_container_detail_pane` all depend on route/layout mode. `compact_layout_active`, selected resource, sidebar flap and existing compact detail behavior already exist.

- [ ] Decide feasibility from content width after rail plus measured list/status/action and detail minimums. At 800 normal size use the existing full-page list→detail route when those panes cannot fit. Keep wide three-column view when feasible; do not merely change a window-width constant without accounting for rail state.
- [ ] Use the same decision in `uses_compact_layout`, draw/event routing, `accessibility`, `perform_semantic_action`, `control_action_bindings`; reuse existing Back and selection transitions. Reserve running badge before row action positions and SearchField shortcut badge before placeholder.
- [ ] Replace the test `compact_breakpoint_preserves_tablet_desktop_but_protects_large_text` expectation that 800 always preserves desktop with a measured-feasibility assertion. Retain `compact_selection_is_a_full_page_drill_in` and verify resize does not lose selected container/detail tab.

**Acceptance:** At 800 no Running/Stop/Restart overlap and search hint is distinct. List→detail→Back by pointer/keyboard/accessibility preserves selection; lifecycle commands dispatch once. Capture list/detail at 800, selected detail at 1200, disconnected engine and 480.

### B2. Diff/Merge

**Files:** `apps/diffmerge/src/app.rs`, `merge_view.rs`, `diff_view.rs` only where shared compact decision is needed.

**Current:** MergeView::is_compact, source_selector_rects, draw_hunk_action_chips (~569), draw_hunks and merged band have a working narrow source switcher. Conflict chip widths currently estimate `label.len()` (~649). App toolbar has explicit semantic/focus mappings.

- [ ] Measure the full conflict choice group and require it to fit the actual pane width. If three panes cannot fit, select existing compact source view earlier, including 800 where required. Share that compact decision among draw, hit rectangles, focus/accessibility and source selector; preserve selected source when widening/narrowing.
- [ ] Replace character-count chip widths with measured labels. Keep Take ours/theirs/both actions bound to `HunkChoice` and existing pending `DiffmergeAction` route; don't substitute a purely visual chip.

**Acceptance:** All conflict choices fully inside their pane at 800; pointer, Tab/Enter and semantic activation resolve identical hunks and remain undoable. Extend `compact_source_switcher_has_pointer_keyboard_and_a11y_parity`, toolbar focus order and root resolution tests. Capture a multi-conflict merge at 800 and 480; also a two-file diff at 1200.

## Task group C — browse/form preambles and bounded cards

### C1. Store

**Files:** `apps/store/src/app.rs`, `widgets/card.rs`; app tests. Shared CardGrid changes only as a separately reviewed dependency if intrinsic support is absent.

- [ ] In `store_card` (~40), retain measured identity, description and Button action row, with internal bottom padding. In catalogue `make_section` (~2650), remove unsupported `.with_card_height(180.0)` and choose intrinsic/measured row height. Equal-height siblings must share the row maximum so Details/primary actions align; longer descriptions may wrap and increase the row, never paint outside it.
- [ ] Tighten `build_header` (~2209) and catalogue section assembly: one title, search, compact filter/source controls and one availability/count heading. Remove repeated Your apps/count/Ready to use layers while preserving meaningful installed/discovery distinctions and source/filter controls.
- [ ] Preserve `item_action_binding`, `drain_click_keys`, `drain_primary_action_click`, operation locks and existing accessible Button children. A disabled install/update remains disabled through reflow.

**Acceptance:** Terminal/Files/VLC bottom actions all inside card bounds with aligned baselines at 1200; narrow first result appears materially above baseline y445. Test long description/title, progress label and all filters. Click Details/Open and keyboard primary action in both grid/stacked layouts; record detail return selection.

### C2. Settings

**Files:** `apps/settings/src/settings_app.rs`, `widgets.rs`, `panes/accounts.rs` and `panes/region.rs` only as needed.

- [ ] `build_content` (~297) currently adds a 14px spacer, page heading, 28px spacer, outer Container padding and category-specific card gaps. Replace redundant spacer layers with explicit related-title/subtitle/body spacing; keep the Scrollable and ExternalAccounts readable width.
- [ ] Inspect the Accounts first card/status introduction and Region heading so the category title isn't repeated by the pane. Keep explanatory account state and Add account visible without another large blank band. No global Container padding change.
- [ ] Reuse existing widget semantics/action dispatch and selection/search state; adjust first-action geometry tests, including `narrow_large_text_keeps_the_destination_in_the_initial_viewport`, only after shared scaling lands.

**Acceptance:** External Accounts Email action and Region first field move upward at normal scale; category identity is still clear, back/search/category selection works. Capture these exact categories (filename-specific no-viewport fixtures differ from responsive viewport fixtures; controller must deliberately select each route).

### C3. Themes

**Files:** `apps/themes/src/app.rs`; customizer/preview only for compact specimen mode.

- [ ] `build_panes_layout` (~809) stacks a fixed `NARROW_PREVIEW_H` 560px specimen before controls. Prefer a compact specimen summary followed by the existing customizer, with a named Show full preview disclosure, rather than cropping arbitrary sample controls. Alternatively use two existing Button tabs Preview/Customize if the summary cannot represent the specimen honestly.
- [ ] Add local view state and propagate through `build_content_layout`, `apply_content_layout`, `apply_narrow_stack_layout`, draw and event routing. Preserve `push_view_state`, chosen preset, unsaved customization and scroll; do not reset on viewport changes. A full preview opens with keyboard and returns focus to its trigger. Expose the new control through Widget children/accessibility (app has no custom accessibility override to silently extend).

**Acceptance:** Editable controls appear in first narrow viewport with gallery closed; full preview remains reachable; change accent then open/close preview preserves edit. Capture `QOS_THEMES_SHOT=scroll` and `live` in addition to default overlay fixture.

### C4. VPN

**Files:** `apps/vpn/src/app.rs`.

- [ ] `layout` (~1242) unconditionally reserves 144px narrow header, plus optional 48px notice. Derive base header bottom from actual title/control rows in `draw_header`; keep notice rect and Dismiss/Retry rects after that occupied band. Use the same `LayoutRects` in `handle_event`, `accessibility`, `perform_semantic_action` and `handle_key_press`.
- [ ] Preserve Add/import route and connection list/detail push semantics. Do not touch EmptyState text-scale internals in this task.

**Acceptance:** Blank trailing 56px disappears at 480; header, notice and first row do not overlap with import progress or errors. Pointer/keyboard Retry/Dismiss and import remain reachable. Capture empty, detail and import/error at 480/800.

## Task group D — management disclosure

### D1. Firewall

**Files:** `apps/firewall/src/app.rs`; existing app tests.

**Current:** `layout_center` (~545) fixes narrow summary at 188px and bottom form at `FORM_H_NARROW` (226px), `draw_filter` repeats Rules beside Rules tab. `Layout`, `relayout`, `draw_form`, `handle_key`, `accessibility`, `perform_semantic_action` and existing form action dispatch are relevant.

- [ ] Compact summary to measured title/chips/status groups; keep zone active-state action. Remove redundant Rules label when the tab already identifies the section.
- [ ] Add `rule_form_open` view state and a clearly named Add rule Button. When closed, table fills the released space; when open use existing form in a bounded panel/modal with Save/Add and Cancel. Retain partially entered draft on temporary close if existing behavior retained it; Escape never applies a rule.
- [ ] One form geometry drives paint and hit targets. Hide closed form controls from traversal and accessibility; focus first field on open and trigger on close. Route submit to existing rule action and retain Apply/backend confirmation behavior.

**Acceptance:** More than baseline four narrow rules visible; all remaining rows scroll. Add rule by click and keyboard, cancel without mutation, submit through canonical action, Rules/Services switching and active zone setting. Capture closed/open form and backend unavailable state at 480 and 800×480.

### D2. Windows

**Files:** `apps/windows/src/app.rs`, `strings.rs`/localization resources for visible labels; no provider behavior changes.

- [ ] In `WindowsApp::draw` (~1485), place application catalogue and Add app before secondary appliance details. Keep a compact meaningful appliance status plus Manage Windows disclosure; setup/error states offer their corrective action without dumping all maintenance copy before apps.
- [ ] Add local `appliance_details_open` and `application_details_open` state. Move runner/capsule/access/management metadata to explicit details section; primary selected-app Open remains prominent. Reuse all existing Button instances and signals, `apply_control_signals`, `queue_live_control`, `invoke_appliance_operation` and application operation paths.
- [ ] Update `sidebar_content_bottom`, `clamp_sidebar_scroll`, `deactivate_selection_controls`, `children`/`children_mut`, `test_rects`, `handle_event`, `accessibility`, `perform_semantic_action` together. Closed branches clear old button rectangles and aren't focusable. Preserve `compact_detail`, selection and pending confirmation/error recovery.
- [ ] Enumerate existing maintenance commands in the test (Choose ISO, recovery desktop, start/restart/suspend/resume/shutdown/connect/update/revoke, capsule/VM confirmation, retry/escalation/uninstall/delete) and verify each is still reachable in its applicable state. Disclosure must not bypass destructive confirmation or execute VM operations on opening.

**Acceptance:** First catalogue entry moves above baseline y347; choose/Open are primary. Open disclosure by pointer/keyboard, traverse all applicable commands, close/Escape and recover focus. Test offline/provider recording fixture; capture normal, vm-detail and error-compact. Controller owns Linux VM run and restoration to initial suspended state.

## Task group E — related text grouping

### E1. Chat

**Files:** `apps/chat/src/render_chat.rs`.

- [ ] `draw_sidebar_header` (~536) elides title to side.width−60 before measuring presence pill. Compute pill first with its actual collaborators/people label and reserve its full measured width plus gap; then elide title into the remainder. If title would become meaningless, stack pill below title and return the actual header bottom.
- [ ] Preserve category affordance and `Hit` rectangles; if header height changes ensure `draw_sidebar_tree` starts below the returned rect. Keep full workspace identity accessible.

**Acceptance:** Long workspace and plural collaborators do not collide at 800/1200; category trigger remains clickable and keyboard accessible. Main/thread route heights remain usable.

### E2. Backups

**Files:** `apps/backups/src/app.rs`, `paint.rs` only if a shared measured-bold helper is needed.

- [ ] In timeline header (~1110), `paint::text_bold` renders title but `tr.measure` measures regular weight. Use the exact bold measurement; reserve right-side newest-first width; place scope after a real gap, ellipsizing or stacking scope when necessary. Return measured header height to timeline list geometry.

**Acceptance:** Snapshot timeline / All sources remain distinct; long selected source cannot collide with newest-first or first snapshot. Source selection, snapshot keyboard focus, backup/restore actions unchanged. Capture all-source and long-source at 480/1200.

### E3. Feeds

**Files:** `apps/feeds/src/app.rs`.

- [ ] `build_list_rows` (~2001) allocates fixed 88px rows and `draw_list` (~3087) positions separate title/source/snippet bands. Add a measured row layout (title rect, source rect, snippet rect, star rect, total height); use tighter intra-article gaps and a larger interarticle gap. One-line titles must not reserve a phantom second line. Two-line titles must still fit.
- [ ] Sum row heights for `list_content_h` and scroll clamp; all drawing, selection, star hit testing and accessibility bounds consume the cached rows, not `index * LIST_ROW_H`. Update keyboard scroll-to-selection and refreshed article rebuilding accordingly.

**Acceptance:** Title/source/snippet read as one unit; adjacent article is visibly distinct. Mixed one/two-line titles and long source names work at 480/800. Pointer star, keyboard selection/open/back, semantic star and scrolling act on the correct article after row heights differ.

## Task group F — preserve narrow tool access

### F1. Git Client

**Files:** `apps/git_client/src/app.rs` and `settings.rs` if reusing setting controls inside disclosure.

- [ ] `draw_appbar` (~843) reserves fixed 230px settings and right-anchored full tabs. Measure available width after hamburger; keep repository identity bounded and Changes/History/Panels navigation visible or in an explicit screen chooser. Move secondary presentation preferences to a Settings disclosure when the complete group cannot fit.
- [ ] Geometry/state must feed `draw_appbar_tab_group`, `draw_settings_cluster`, `handle_click`, `handle_press`, `handle_key`, `accessibility_targets`, `perform_semantic_action`. Reuse existing setting operations; clear hidden tab/settings rectangles. No command reachable only by undocumented shortcut.

**Acceptance:** At 480 no negative tab positions or hamburger overwrite; pointer and Tab/Enter can reach each screen and every preference. Stage a file, type commit message, inspect diff and return without lost selection. Verify measured settings menu and repository names at 800.

### F2. PDF Viewer

**Files:** `apps/pdf_viewer/src/app_draw.rs`, `app_events.rs`, `app.rs`.

- [ ] `toolbar_layout` (~102) places five tools after left Open/Save, but does not check against `right_start`. Prefer wrapping the tool group to a second measured toolbar row at insufficient width; this preserves all direct tools and avoids another bespoke menu. Derive toolbar height and canvas top from occupied rows. A shared accessible overflow can replace wrapping only after its semantic dependency is ready.
- [ ] Extend `ToolbarLayout` and toolbar rect computation; all `draw_toolbar_*`, toolbar hit testing, `ToolbarHit`, `on_left_press_*`, `on_key*`, `accessibility` and semantic routes consume the same rectangles. Keep zoom, Fit, search, rail/sidebar and annotation swatches reachable.

**Acceptance:** At 480 Note/Sign never intersect zoom buttons. Activate every Tool by pointer and keyboard, annotate populated sample, zoom/Fit and search. Test same tool hit before/after 480↔800 resize. Separate sample text rendering defect in render.rs is outside this toolbar fix and stays open in closure register.

### F3. Snippets

**Files:** `apps/snippets/src/app.rs`.

- [ ] `draw_list` (~1719) places search at area.x0+12 while `draw_toggle` (~1155) overlays leading slot. Reserve `draw_toggle` actual width plus gap in a shared search/header rect calculation. Apply that rect through `search.apply_layout_external` and `route_search_event`; do not fix paint only.
- [ ] Preserve `sidebar_layout`, `is_push_layout`, detail back route and `dispatch_event`/`handle_keyboard_input`. Header title/count and detail action overflow remain measured after the shared scale dependency; don't widen permanent sidebar to fix the narrow toggle.

**Acceptance:** Hamburger and search placeholder/input/selection have disjoint bounds at 480. Click search, type filter, toggle sidebar and Escape; keyboard return to search works. Capture list, `--scenario detail`, empty.

### F4. Typst Export

**Files:** `apps/typst_editor/src/app.rs`.

**Current:** `draw_toolbar_full` (~1111) drops Export below 1120; compact menu only below 760. Crucially `layout_in` (~928) clears `menu_open` whenever not compact.

- [ ] In full toolbar, reserve a measured overflow button whenever an action is omitted (including Export); reuse `TbAction::ToggleMenu`, `draw_menu`, `hit_menu`, `do_action(TbAction::ExportPdf)` and existing keyboard menu handling. Treat overflow availability independently from editor/preview compact pane mode.
- [ ] Update `layout_in` so it closes the menu only when overflow is no longer available, not simply when `!compact`. Compute omitted actions before rendering; derive `toolbar_hits`, menu items, `accessibility`, `perform_semantic_action` and `control_action_bindings` from the same visible/overflow action list. Keep split_ratio/manual divider unchanged.

**Acceptance:** Export reachable by click, Tab/Enter and semantic action at 759,760,800,1119,1120. Opening overflow survives the next layout frame. Export invokes existing portal path once; cancellation preserves source. Test resize while menu open and last-good-preview/error scenarios.

## Task group G — reading width and compact editor chrome

### G1. TOTP

**Files:** `apps/totp/src/app.rs`, `list.rs`.

- [ ] `apply_layout` (~1094) spans search/list across viewport. Center a readable content column, initially 760 logical px maximum at normal type, with fluid width at smaller sizes; align header actions, search and list edges. Treat 760 as app composition choice to validate with 8-digit codes/long identities, not a new framework size rule.
- [ ] Pass the bounded list rect into AccountList::set_viewport; `row_rect`, `row_layout`, copy/action hit zones then inherit the correct geometry. Keep banner/retry and modal outside this cap only where they need viewport overlay bounds. Don't shrink codes or change countdown behavior.
- [ ] Preserve `apply_list_event`, `handle_event`, root `accessibility`, `perform_semantic_action`; semantic account names must not expose secret codes (existing tests enforce this).

**Acceptance:** Identity/code can be matched without scanning across 1200px. Pointer copy/Edit/Delete/Reorder, keyboard selection/copy, search, add dialog and scroll all work; last account remains reachable. Capture 5 accounts, long identity, 8-digit code at 1200/480 and add-advanced.

### G2. Mail

**Files:** `apps/mail/src/app.rs`, `pane.rs`; `ui/src/widget/nav_shell.rs` for a narrowly scoped, default-preserving shared option.

- [ ] `NavShell::content_child_rect` (~593) always reserves the chip band; no existing public option disables this reservation. Add an explicit opt-in overview-header ownership option (proposed `with_overview_header_owns_affordance(bool)`), default false, used by Mail only. With it true, pass the full overview rect while retaining the shell-owned chip, event routing and semantic control. Trace `layout_nav` (~570) and `publish_shell_flags`; Mail reserves the chip horizontal slot in its own inbox header. Do not paint a second independent hamburger. Keep detail/sidebar reservation unchanged. Test opt-out apps retain current geometry and drawer modality. `Pane::list_header_layout`, `header_left_inset`, `draw_list_header` (~734), refresh/search rectangles and list content top must agree on one reserved band.
- [ ] Preserve compact Reader/Back, drawer scrim, account setup and compose routes. Update shell flags only for list header ownership; ensure a drawer-open transition doesn't recreate the duplicate band. `accessibility`/`perform_semantic_action` and existing compact-pane tests use the same shell control.

**Acceptance:** One compact inbox header band, with hamburger/title/count/search/refresh all reachable; if two rows are necessary they are occupied, not a near-empty 60px strip. Test drawer open/Escape, search, read→Back, reply and compose. Capture narrow inbox/reader/drawer; wide retains its useful three panes.

### G3. Word Processor

**Files:** `apps/word-processor/src/widget.rs`, `zoom_control.rs` only as needed.

**Current:** `draw_frame_chrome` (~5344) applies `auto_fit_page` from `(available−32)/MAX_PAGE_W`, resulting in ~55%. `set_zoom`, `zoom_by` and existing zoom hit/caret tests preserve explicit manual zoom. `LayoutMode` is only None/Margins/Spacing guide editing, not a reader/reflow mode; do not repurpose it. `zoom_control::Action::Reset` means 100%, not Fit page.

- [ ] First capture a populated paragraph and exercise insertion/selection at 480 to distinguish fixture placeholder from real editing. Add a distinct view-only fitting mode (proposed `ViewFit::{TextWidth, Page, Manual}`) using text-area width rather than paper width, with horizontal viewport offset accounting for page margins. Add explicit Fit page/Fit text choices beside the existing zoom control or its accessible menu; retain Reset as 100%. A full document reflow engine is not justified by this bounded task.
- [ ] Use one content transform and inverse in page paint, selection/caret hit testing, `ime_cursor_rect`, ruler and scroll bounds. Do not change stored page size/margins or exported PDF geometry. If text-width fitting needs horizontal scrolling, provide a reachable scroll route and ensure-caret-visible behavior; do not simply clamp zoom and leave the caret offscreen.
- [ ] A chosen manual zoom disables automatic fitting; viewport/panel changes may adjust only an active automatic mode. Update `narrow_first_open_fits_the_page_until_the_user_zooms` to new intended default and retain `click_maps_to_the_same_caret_under_zoom`, scroll range and IME accessibility checks.

**Acceptance:** At 480 actual paragraph/caret readable; click/type/select at first and last line edges, horizontal scroll if required, undo, resize, styles/outline open and close. Explicit Fit page and 100% persist. Compare export before/after view-mode switch to ensure page geometry unchanged. Capture populated paragraph plus blank page at 480/800/1200. This bounded investigation may end with a documented recommendation before implementation if existing layout modes cannot safely support text fit; do not silently ship a zoom clamp.

## Execution / review / evidence checklist for every group

- [ ] Read affected crate package names from Cargo.toml; run focused existing tests and new behavioral regression(s) only for changed semantics/geometry. No heavyweight workspace build during parallel planning.
- [ ] For geometry behavior assert visible targets are inside their owning pane, are pairwise disjoint unless deliberately overlaying, and correspond to the action actually invoked. Tests that merely restate constants are insufficient.
- [ ] For disclosures test closed/open/keyboard/semantic paths, hidden-focus removal, Escape/focus return and resize while open. For resize test manual state restoration after temporary clamping. For variable list rows test click/keyboard mapping after scroll.
- [ ] Capture fresh standard sizes plus named routes above using corrected controller binaries. Inspect PNGs, record binary/source provenance and before/after findings; screenshots alone do not prove events or accessibility.
- [ ] Have a scoped reviewer reject or accept each bounded group before next production worker. Update controller closure register per app/finding; carry large-text and fixture gaps explicitly. All 48 apps still need final corrected-capture verification, including those not changed here.

## Shared dependencies and intentionally unresolved items

1. Capture fidelity and single-scale text/measurement contract land first; coordinate any renderer-aware layout signatures with that owner. This plan contains no broad font audit.
2. Shared accessible measured Toolbar / resize semantics may eliminate local code only once verified. No app may hide an action while waiting for that dependency.
3. API/DB, variable-height Feeds and Word's view transform are the higher-risk behavioral tasks; isolate their reviews from small title-gap/preamble fixes.
4. Other baseline findings in Calendar, Hex, Logs, Music, Weather, etc. remain under their assigned shared/app work packages; omission here is not closure. PDF sample-renderer title/caption defect also remains separate.
5. No tests/builds or visual recaptures were run for this design-only artifact. No production file was changed.
