# Editing workspace preflight

Date: 2026-09-16  
Scope: source-only preflight for API Tester, DB Client, and Code Editor composition repairs

## Evidence boundary

Controller update: shared Toolbar Task4 is now accepted (`1b3eac1b4` + `c7b2d576b`). Historical references below to it being in progress describe the preflight time. Current task briefs govern reuse; consuming its verified interface is allowed when existing command ownership fits, without requiring a wholesale migration.

This note was prepared after reading `../sdd/2026-09-15-app-composition/task-1-brief.md` and the current application, widget, test, and capture-entry source. The three application source trees were clean in the working tree at review time. `ui/src/widget/toolbar.rs` had an unrelated in-progress modification; it was deliberately not inspected and no recommendation below depends on that work being accepted.

No application or test source was edited, no binary was rebuilt, and no capture was made during this preflight. References to the baseline review summarize `.superpowers/app-ui-2026-09-15/review-1-report.md`; they are not new pixel judgments.

## Recommendation: split by application, then split scale work

The task should not be implemented as one cross-application patch. The smallest reviewable sequence is:

1. **Code Editor normal-size pass:** start with the bottom dock collapsed and add a visible retained control that uses the existing `ToggleBottomPanel` command path. Keep every existing command, shortcut, and programmatic opener.
2. **API Tester normal-size pass:** reorganize sidebar actions behind disclosures and implement the request/response divider in one pass. Those two changes share `ChildSig`, `Layout`, child enumeration, placement, drawing, and event ordering, so splitting them would duplicate fragile focus-tree work.
3. **DB Client normal-size pass:** implement the query/result divider and its local controls without touching result binding.
4. **Code Editor scale pass, after the shared text/measurement contract is accepted:** replace the editor's fixed font/line geometry and bottom-panel tab/row geometry with one effective measured metric model.
5. **DB Client scale pass, after that same gate:** replace fixed grid and query-chrome metrics while preserving sort and scroll state.

This gives each app an independently testable normal-size improvement and keeps the higher-risk scale rewrites out of the interaction patches. The API pass should use geometry helpers that can consume scaled metrics later; it should not introduce a parallel set of 2x constants.

## Shared implementation constraints

- Keep disclosure, split override, drag, and panel visibility state as local view state. Do not add it to the CRDT or persisted session models.
- Store a user's requested split height separately from its currently clamped height. Relayout may clamp the effective height for a short window, but must not overwrite the stored preference; resizing larger must restore it.
- Derive drawing, hit testing, child placement, and accessibility bounds from the same layout result. A painted divider without a real focus/action target does not meet the brief.
- Begin divider capture before routing a press to the editor or result surface. Continue the drag outside the separator and release capture on mouse-up.
- A Reset action must set the override back to `None` so content-aware sizing resumes. It must not merely copy the current automatic pixel height into the manual preference.
- Keep existing button IDs and command sinks for existing actions. New disclosure and sizing controls need stable names for `Driver::click_named` and accessibility tests.
- Do not move any of these controls into the shared Toolbar in these passes. That component is under concurrent implementation and was not part of this source audit.

## API Tester

### Current integration points

| Concern | Current source anchor | Consequence |
| --- | --- | --- |
| Local view state | `apps/api_tester/src/app.rs:266-338` | Add disclosure, request-height override, and drag state beside the existing sidebar/tab view state, not in request data. |
| Child-tree invalidation | `apps/api_tester/src/app.rs:203-217` (`ChildSig`) | Disclosure state must participate in the signature or the retained focus/accessibility tree can stay stale. |
| Layout model | `apps/api_tester/src/app.rs:219-257` (`Layout`) | Add a divider rect and represent visible management controls explicitly. The current `manage: Vec<Rect>` has no identity. |
| Stable management actions | `apps/api_tester/src/app.rs:654-708` | Preserve the ten existing IDs and command bindings: new/save/rename/delete request and collection plus new/rename/delete environment and new/delete variable. |
| Navigation order | `apps/api_tester/src/app.rs:991-1029` | `compute_nav_meta` already preserves collection, request, and history ordering; reuse it rather than rebuilding saved-object semantics. |
| Child enumeration | `apps/api_tester/src/app.rs:1044-1137` | It currently exposes all ten management buttons whenever the sidebar is visible and no form is open. Gate children by the same disclosure inventory used for layout and draw. |
| Child placement | `apps/api_tester/src/app.rs:1141-1182` | The current positional `zip(manage_buttons, l.manage)` becomes incorrect as soon as a subset is visible. Map rects to explicit button indices or IDs and zero every hidden button. |
| Request/response split | `apps/api_tester/src/app.rs:1196-1376` | `compute_layout` fixes the editor to 42% with only a 150 px lower bound (`:1248-1256`) and starts the response at its bottom. Replace this with automatic/manual split geometry plus a divider and a lower-pane reserve. |
| Sidebar composition | `apps/api_tester/src/app.rs:1296-1373` | All ten management rows are laid out before saved requests/history; short-window rows may receive zero rects while remaining focusable. Saved objects must be laid out first and hidden actions removed from the child inventory. |
| Sidebar paint | `apps/api_tester/src/app.rs:1412-1484` | Drawing is currently unconditional. It must consume the same explicit visible-action inventory as layout and children. |
| Parameter rows | `apps/api_tester/src/app.rs:1646-1814` | `draw_kv_table` gives a usable automatic-height source: header/top region, nonblank rows, the add row, and bottom breathing room. Replace raw `ROW_H` use with the accepted effective row metric when scale work lands. |
| Auth/body geometry | `apps/api_tester/src/app.rs:1887-2087` | Auth types have materially different field counts, and Body owns the retained `TextInput`. Automatic height must use the active mode rather than one universal 42% value. |
| Canonical mutations | `apps/api_tester/src/app.rs:2413-2477` | Keep `drain_commands` as the single mutation sink; disclosure and divider actions are view commands only. |
| Retained layout | `apps/api_tester/src/app.rs:2752-2805` | `build_layout` and `apply_layout` are the points where the updated child list and one computed geometry snapshot must meet. |
| Event ordering | `apps/api_tester/src/app.rs:2838-2944`, `:3129-3154` | Start/continue/end divider handling before the Body editor or other children consume pointer selection. Existing painted chrome routing only handles scrim and JSON folds. |

### Reuse already available

- `apps/api_tester/src/navrow.rs:38-330` is a retained, focusable, accessible row with pointer activation, Enter/Space activation, and external layout. Keep it for collections, requests, and history.
- `apps/api_tester/src/app.rs:385-388` keeps Send on its existing canonical binding; the Send button is built at `:602-605`.
- `apps/api_tester/src/responsive.rs:16-21` and the existing `SidebarDrawer` already own rail/drawer switching. The disclosure design should remain inside that content rather than create another narrow-mode surface.
- `apps/api_tester/tests/a11y.rs:26-34` already drives the real app with `ui::headless::Driver`. `Driver::resize`, `click_named`, and `key` are available at `ui/src/headless.rs:742`, `:2409`, and `:2430`.

### Proposed shape

Keep one explicit ordered sidebar inventory, for example an enum containing navigation rows, primary actions, disclosure triggers, and disclosed action-button indices. Use it in child enumeration, layout, placement, and draw. At normal size the order should make the environment selector and saved collections/requests/history visible before maintenance actions. Keep **New collection** and **Save request** adjacent to the corresponding saved-object region. Group rename/delete actions under Collection, Request, and Environment disclosures, with labels that include the selected target where a target is required.

For request sizing, compute an automatic preferred height from the active request mode:

- Params/Headers: table header + visible nonblank rows + add row + bottom allowance.
- Auth: chip row + the field count for None/Bearer/Basic/API key + bottom allowance.
- Body: chip row + a useful editor minimum.

Then clamp the automatic or stored manual height against the actual space after URL bar and request tabs while reserving the divider, response status/content, and a small lower-pane minimum. Put the resulting editor and response rectangles in `Layout`. Use a focusable divider/control path for pointer drag, keyboard increase/decrease, and Reset; all paths should mutate the same local override.

### Source-backed pitfalls

- `children_ref` currently lists all ten management buttons even when their rect is zero. Merely hiding paint would leave invisible Tab stops and accessibility nodes.
- The positional `zip` in `position_children` will assign a visible rect to the wrong command if controls are filtered without adding identity.
- The brief mentions Params/Auth editing, but those surfaces are currently painted, read-only chrome; comments and event routing do not provide retained editors or hit paths. This composition task should not silently expand into implementing new parameter/auth editors. Preserve Body `TextInput`, method selection, Send, and all current form commands.
- The current 150 px request minimum has no matching upper bound for very short windows. Clamp both panes from the available height; do not allow the response origin below the viewport.
- Opening a contextual form replaces normal sidebar children. Disclosure state must survive that temporary form without exposing hidden controls behind it.

### Focused verification to add/run

- Extend `apps/api_tester/tests/a11y.rs:115-133` so collapsed management actions are absent, disclosures are reachable, and opening one reveals only its controls.
- Keep and update the full Tab-order test at `apps/api_tester/tests/a11y.rs:219-264`; test both closed and open disclosure states.
- Use the existing keyboard activation coverage at `apps/api_tester/tests/a11y.rs:266-314` to prove disclosed actions reach the same `Cmd` path.
- Add library geometry tests beside `apps/api_tester/src/app.rs:3351-3409` for automatic sizing by Params/Auth/Body content, manual drag, keyboard adjustment, Reset, short-window clamping, and restoration after resizing larger.
- Add a real pointer test proving a divider drag does not select/edit Body text and that capture continues outside the separator.

Commands after implementation:

```sh
cargo test -p api_tester --lib
cargo test -p api_tester --test a11y
```

## DB Client

### Current integration points

| Concern | Current source anchor | Consequence |
| --- | --- | --- |
| Local state | `apps/db_client/src/app.rs:99-185` | Add query-height override, divider/drag state, and divider geometry here. Do not put them in `Session`. |
| Initial state | `apps/db_client/src/app.rs:241-300` | Initialize the override to automatic and keep the current query text/results construction. |
| Query layout | `apps/db_client/src/app.rs:552-628` | `relayout` fixes the editor at 42%, clamped to 120..360 (`:595-616`), then leaves a fixed 30 px gap before the grid. Replace that branch with one query split computation. Leave the Data tab branch (`:617-624`) independent. |
| Connection panel | `apps/db_client/src/app.rs:630-644` | Its height grows with connection count. Test the existing starvation risk, but do not fold a sidebar redesign into the query split pass. |
| Result binding | `apps/db_client/src/app.rs:1212-1273` | `GridView::bind` runs only when result generations change. A scale/layout refresh must not call it. |
| Event routing | `apps/db_client/src/app.rs:1275-1380`, `:1810-1828` | Begin divider press before editor focus/caret and grid header handling. Route cursor/release while captured independently of ordinary click handling. |
| Pointer capture | `apps/db_client/src/app.rs:1507-1511` | `wants_pointer_capture` currently delegates only to the editor; include an active divider drag. |
| Keyboard commands | `apps/db_client/src/app.rs:1754-1797` | Preserve Ctrl+Enter Run, Ctrl+N, Escape, snippets, and editor routing. Resize keys should be consumed only while the divider/control owns focus. |
| Query drawing | `apps/db_client/src/app.rs:2455-2576` | The toolbar is reconstructed at `:2463-2537`, the editor draws at `:2539-2540`, and the result strip derives from `editor.rect().y1 + 4` at `:2542-2564`. Draw and hit the divider from the relayout result instead of reconstructing another split. |
| Accessibility | `apps/db_client/src/app.rs:1468-1505` | The connected Query view currently exposes the editor only; toolbar/snippet/tab chrome is immediate-mode. Named resize controls require a retained or explicitly semantic control path. Root narration is insufficient. |

### Reuse already available

- `ui::widget::CodeEditor::line_count`, `line_height`, and `gutter_width` are available at `ui/src/widget/code_editor.rs:345-399`. After the shared measurement contract lands, line count × effective line height plus a small allowance is a direct content-aware default.
- `apps/db_client/src/app.rs:3025-3046` has a real synthetic `WindowEvent` helper in `behavior_tests`; use it for pointer drag, release, and keyboard paths.
- `apps/db_client/src/app.rs:2781-2796` has a real Taffy layout helper in `responsive_tests`.
- `click_query_toolbar` at `apps/db_client/src/app.rs:1918-1986` is the canonical route for Run, snippets, confirmation, and grid header sorting; preserve its priority and mutations.

### Proposed shape

Compute a query layout containing toolbar, editor, divider, result strip, and grid rects. The automatic editor height should follow the current query's visible line count and effective editor line height, then clamp to the viewport while reserving a useful grid region (at least header plus one row at constrained height). Store manual height separately and use the same clamping rule as API Tester.

The source has no existing accessible generic overflow menu. Implement the divider as a small app-local retained/focusable control, or add named controls to an existing app-owned action surface; do not claim that the immediate-mode Snippets popup supplies keyboard/accessibility semantics today. Pointer drag, keyboard increase/decrease, and Reset must all feed the same override. Do not add a permanent vertical stack of resize buttons.

### Grid scale pass

`apps/db_client/src/grid.rs:15-18` fixes header height 30, row height 26, padding 10, and font size 13. These constants drive visible-row calculation (`:122-130`), hit testing (`:149-180`), header/body drawing (`:237-360`), and cell text (`:395-443`). Replace them together with a cached effective `GridMetrics` after the shared contract lands.

`GridView::bind` at `apps/db_client/src/grid.rs:64-95` resets both scroll axes at `:92-93` and recomputes row order. Metric refresh needs a separate method that preserves `scroll_row`, `scroll_x`, sort state, order, and hover. A new result may still use `bind` and reset as it does now. There is no selected-row state in the current grid; the brief's “selection” language must not be used to justify inventing one.

Scale the query tab/toolbar/result strip constants (`TABBAR_H` and `TOOLBAR_H` near `apps/db_client/src/app.rs:58`) in the same DB scale pass so text and chrome stay coherent. Use one metric generation for draw, hit testing, and visible-row calculations.

### Source-backed pitfalls

- Calling `GridView::bind` after a text-scale change will destroy scroll state; it is not a harmless metric refresh.
- Relayout currently sets only rectangles and preserves editor/grid models. Keep that separation.
- The Run/snippet toolbar and tab strip are immediate-mode. Adding painted labels without semantic children will fail named-control and keyboard requirements.
- The many-connections sidebar issue is a baseline hypothesis, not part of this split implementation. Add a compact-height regression case, but keep the pass focused unless that case proves the split unusable.

### Focused verification to add/run

- Add behavior tests for content-aware default height with one and many query lines, manual drag, arrow adjustment, Reset, resize clamp/restoration, and no interference with Ctrl+Enter or editor selection.
- Seed sort and nonzero vertical/horizontal scroll, refresh metrics/relayout, and assert they survive. Then verify header and row hit tests at 1x and 2x use the same geometry as drawing.
- Test Query, Data, and Structure switching: the split affects Query only and a stored Query preference survives returning from another tab.
- Test 800×480 with many connections to catch starvation without broadening the implementation.
- Extend accessibility coverage near `apps/db_client/src/app.rs:3127-3137` for the divider/control names and actions.

Command after implementation:

```sh
cargo test -p db_client --lib
```

## Code Editor

### Normal-size dock pass

| Concern | Current source anchor | Consequence |
| --- | --- | --- |
| Default | `apps/code_editor/src/app.rs:570-658` | `from_session` currently sets `panel_visible: true` at `:607`; change the default view state to collapsed. There is no preference load/write to update. |
| Command routing | `apps/code_editor/src/app.rs:2297-2397` | Reuse `apply_command`: Search Project and Go to Search open Search, Toggle Bottom Panel toggles, Problems opens Problems, and Terminal opens Terminal plus `ensure_terminal`. |
| Programmatic openers | `apps/code_editor/src/app.rs:2619-2620`, `:3015-3019`, `:4478-4498` | Preserve project-search, screenshot-terminal, and resource-reveal paths. Diagnostics refresh should not force the panel open or closed. |
| Keyboard route | `apps/code_editor/src/app.rs:3469-3529` | Ctrl/Cmd+J already maps to Toggle Bottom Panel. Ctrl/Cmd+Shift+P opens the command palette. |
| Palette commands | `apps/code_editor/src/command_palette.rs:25-78`, `:94-147` | Toggle, Problems, Search, and Terminal commands already have stable IDs and labels. Keep these as canonical keyboard/action routes. |
| Layout | `apps/code_editor/src/app.rs:3267-3329`, `:3925-4052` | The panel node is already conditionally inserted. When hidden, editor flex growth naturally consumes the space. |
| Event/child gating | `apps/code_editor/src/app.rs:3627-3727`, `:4102-4119`, `:4375-4397` | Routing, drawing, and children already exclude the panel when hidden. Preserve this agreement. |
| Accessibility | `apps/code_editor/src/app.rs:4305-4350` | The root describes the panel but exposes no toggle. Add a retained, named trigger to the real child tree and update description/state as needed. |
| Status surface | `apps/code_editor/src/status_bar.rs` | The old status bar is now a noninteractive status bubble. It cannot serve as the visible dock trigger. |

Starting collapsed is a one-line state change, but it would remove the only visible pointer path to the dock. Add a compact retained control in existing editor chrome and bind it to `apply_command(Command::ToggleBottomPanel)`. Its visual state and accessible name/pressed or expanded state should reflect `panel_visible`. Avoid the shared Toolbar and do not create a second panel mutation path.

The brief says to preserve “existing manual dock sizing,” but current source has none: `BottomPanel::height` returns fixed heights, there is no panel-height/drag field in `App`, and no pointer resize path. This pass should preserve the fixed panel behavior rather than invent resizing. If dock resizing is desired later, it needs a separate requirement and interaction design.

`apps/code_editor/src/bottom_panel.rs:23-35` defines fixed panel/tab/row heights; `height` is at `:157-164`, tab drawing at `:471-534`, conditional build height at `:711-733`, and pointer-only tab handling at `:804-873`. `BottomPanel` does not expose an accessibility node or keyboard focus for its tab row. The existing palette commands provide keyboard routes to Problems/Search/Terminal; the new visible trigger supplies pointer reachability while collapsed. Do not imply that the internal tab row itself is keyboard accessible unless that is separately implemented and tested.

### Code Editor scale pass

`apps/code_editor/src/editor_view.rs:93-101` fixes font size 14, line height 20, fallback cell advance 8.4, and gutter padding 12. The fixed line height is reused for viewport counts/scroll clamping (`:707-730`), point-to-caret (`:1316-1325`), row backgrounds/selections and text (`:1399-1600`), diagnostics/caret (`:1628-1701`), popup anchors (`:1749`, `:1868`), wheel conversion (`:2017`), page movement (`:2188-2192`), caret queries (`:2465`), and test geometry (`:2658`). Replacing only text draw size would leave caret and hit geometry wrong.

Create one effective editor metric snapshot containing font size, line height, cell advance, and gutter padding. All drawing, scroll conversion, caret/selection geometry, hit testing, completion positioning, and accessibility bounds must read that snapshot. Measurement can refine the cell advance, but interaction before the first draw still needs a deterministic scale-derived fallback. If measured metrics change, invalidate layout without modifying caret or scroll model state.

Use the same accepted scale/measurement contract for `BottomPanel` tab height, row height, and text baselines. The panel has no manual height preference today, so there is no resize value to preserve in this source version.

### Existing test harness and focused verification

- `apps/code_editor/src/editor_view.rs:2771-2870` has geometry construction and synthetic pointer/key helpers; extend it for scaled caret hit testing, drag selection, line scroll, page movement, completion placement, and before/after-scale state preservation.
- `apps/code_editor/src/app.rs:5061-5088` already proves a project search opens the Search panel; keep it green after changing the default.
- Terminal tests immediately after `:5090` already opt into the Terminal panel; keep those explicit openers.
- Add app tests for launch collapsed, visible trigger click, Ctrl/Cmd+J, palette Problems/Search/Terminal, explicit open surviving diagnostics refresh, and the trigger's accessibility name/state.

Command after each Code Editor pass:

```sh
cargo test -p code_editor --lib
```

## Capture matrix for implementation review

Build once after the focused tests, then capture each app with isolated runtime directories so user state cannot change the result. Review normal, compact, narrow, short, and true 2x output; record the binary hash/source revision alongside the images.

```sh
cargo build -p api_tester -p db_client -p code_editor

run_root=$(mktemp -d)
HOME="$run_root/home" XDG_CONFIG_HOME="$run_root/config" XDG_DATA_HOME="$run_root/data" XDG_CACHE_HOME="$run_root/cache" XDG_STATE_HOME="$run_root/state" \
  target/debug/api_tester --shots "$run_root/api-normal" --viewport 1200x800
HOME="$run_root/home" XDG_CONFIG_HOME="$run_root/config" XDG_DATA_HOME="$run_root/data" XDG_CACHE_HOME="$run_root/cache" XDG_STATE_HOME="$run_root/state" \
  target/debug/api_tester --shots "$run_root/api-compact" --viewport 800x700
HOME="$run_root/home" XDG_CONFIG_HOME="$run_root/config" XDG_DATA_HOME="$run_root/data" XDG_CACHE_HOME="$run_root/cache" XDG_STATE_HOME="$run_root/state" \
  target/debug/api_tester --shots "$run_root/api-narrow" --viewport 480x900
HOME="$run_root/home" XDG_CONFIG_HOME="$run_root/config" XDG_DATA_HOME="$run_root/data" XDG_CACHE_HOME="$run_root/cache" XDG_STATE_HOME="$run_root/state" \
  target/debug/api_tester --shots "$run_root/api-short" --viewport 800x480
HOME="$run_root/home" XDG_CONFIG_HOME="$run_root/config" XDG_DATA_HOME="$run_root/data" XDG_CACHE_HOME="$run_root/cache" XDG_STATE_HOME="$run_root/state" \
  target/debug/api_tester --shots "$run_root/api-2x" --viewport 800x900 --text-scale 2
```

Repeat the five viewport commands with `db_client` and `code_editor`. Useful existing staging inputs are:

- API Tester: `--rail-open`, `--empty`, `--error`, `--focus-url`, and `--form NAME`.
- DB Client: `--empty`, `--status`, plus `DB_CLIENT_TAB=data|structure`, `DB_CLIENT_TABLE`, `DB_CLIENT_MODAL`, `DB_CLIENT_ERROR`, and `DB_CLIENT_CONFIRM`.
- Code Editor: existing scenarios include terminal, selection, completion, find, search, conflict, indexing, goto, close-prompt, and status; `--sidebar-open` is also available.

Do not edit any `main.rs` or capture path to make a screenshot pass. Capture both automatic and manually resized split states through the existing runtime interaction/test harness where possible, and treat 2x capture as valid only after the shared measurement contract is accepted and the app-local scale pass is complete.

## Completion gate for each pass

A pass is ready for review when focused tests prove command equivalence, hidden controls are absent from focus/accessibility traversal, manual split state survives relayout and restores after clamping, and drawing/hit/accessibility geometry comes from one layout result. Only then run the relevant capture matrix and inspect it at the five required sizes. The scale passes additionally need state-preservation tests across a metric refresh and true 2x captures with the same accepted shared contract.

## Controller follow-up: Code Editor trigger integration

Source-only check after preparing Tasks10–14: `build_editor_column` at app.rs3267 currently inserts tabs, Find, editor and conditional panel in one column. A compact trigger can share existing tab chrome; avoid adding a dedicated permanent toolbar row solely for this action. Retained child inventory at app.rs4375 and control binding forwarding at4352 need to include the trigger alongside its actual node placement/event route. The app already has canonical `CodeEditorOperation::Command { id, argument }` classified as a view operation in actions.rs86–108; `add_catalogue_actions` at1300 builds `command-toggle-bottom-panel` from the same Command catalogue. Reuse that existing operation/binding pipeline into apply_command rather than adding an independent document mutation or panel-state shortcut. These anchors guide implementation; no new source/test/capture was changed in this follow-up.

## Controller follow-up: DB query split source anchors (2026-09-16)

Current `DbApp::relayout` in `apps/db_client/src/app.rs`552 computes query editor height from42% and120–360clamp at601, then places grid below an independently assumed30px result strip. Task12 must make one query-region geometry result for editor/divider/result strip/grid; Task14 will replace the fixed chrome metrics consumed there. Shared `CodeEditor::line_height` is public at `ui/src/widget/code_editor.rs:393`; use the editor's authoritative metrics rather than introducing another font-height constant.

DB is currently immediate-mode for populated content: `children`1493 enumerates only the retained empty state when empty, while `accessibility`1465 and `perform_action`1479 directly delegate the populated Query editor. Thus the new separator needs deliberate focus/semantic integration; adding painted geometry or an invisible button list alone does not establish keyboard access. Keep the existing editor semantics and state available while adding the scoped divider route. No source edits or tests performed for this follow-up.
