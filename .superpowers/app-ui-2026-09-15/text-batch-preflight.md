# App text geometry batching preflight

Date: 2026-09-16  
Inputs: `docs/superpowers/plans/2026-09-15-app-text-geometry.md` and `.superpowers/sdd/2026-09-15-app-text-geometry/progress.md`

## Ruling

All app Tasks 1–23 remain unstarted; shared Tasks 1–7 are accepted. The SDD rule permits one implementer to handle a small same-shape batch sequentially, while retaining a separate RED/GREEN result, capture set, interaction result, and ledger disposition for every task. It does not permit parallel production work or a shared abstraction pass.

Only these cross-task batches are small enough now:

1. **Tasks 5 + 6 — bounded control rows:** Camera pills/mode selector and OSK key/suggestion rows.
2. **Tasks 19 + 20 — small status/header groups:** Photos status band and Remote roster header/empty-state group.

Task 22 is already a plan-defined verify-first multi-app task; keep it self-contained rather than adding it to another batch. Tasks 16, 17, and 23 are relatively small, but their contracts are different enough to remain short standalone tasks.

## Batch A: Tasks 5 + 6 — bounded control rows

**Owned paths**

- `apps/camera/src/app.rs`
- `apps/osk/src/widget.rs`
- `apps/osk/src/toolbar.rs`

**Common implementation shape**

- Normalize the resolved token style through `TextRenderer::renderer_input_style` for both measure and paint.
- Compute each automatic row/control height as `max(current normal minimum, measured line height + existing vertical padding)`.
- Center from measured height and publish the same rectangles to pointer and accessibility routes.
- Preserve control identity/order and ordinary-size minima.

**Concrete owners**

- Camera: `draw_mode_switcher`, `ctrl_pill`, `draw_camera_top_controls`, and `draw_capture_cluster`; `push_tap_region` already expands the painted rect for pointer use, and `accessibility`/`perform_semantic_action` already derive from the resulting `regions` map. The change should therefore stay in the control geometry and its existing regions, without a second semantic layout.
- OSK: `build_layout` owns requested overlay height; `apply_layout` owns `toolbar_geom`, `suggestion_area`, `suggestion_geom`, and `key_geom`; `draw_cell` and `draw_suggestion_chip` paint those rects; `key_at`, the suggestion hit path, and `accessibility` consume the cached geometry. `toolbar_rects` remains the toolbar splitter. Row minima must be resolved before `build_layout`, not enlarged only during `draw`.

**Substantive difference / stop rule**

Camera is immediate-mode and rebuilds semantic regions during paint. OSK is a retained widget whose intrinsic height is requested before paint. Keep separate metric helpers and tests. Split Task 6 out if it requires compositor/input-method changes outside these three files. Controller-owned Linux overlay/capture validation remains separate evidence and must not inject user input.

## Batch B: Tasks 19 + 20 — small status/header groups

**Owned paths**

- `apps/photos/src/app.rs`
- `apps/remote/src/app.rs`

**Common implementation shape**

- Replace one fixed text group height with a measured minimum.
- Keep the existing canvas/session/provider state and existing action dispatch.
- Fit or stack the local label/action group only when its actual remaining width fails.

**Concrete owners**

- Photos: `build_status_node` allocates the status band, `apply_layout` stores `status_rect`, and `draw` paints `self.status` into that rect. Measure the painted 13px regular style before assigning the Taffy leaf height; retain the zero-height empty status and preserve `canvas_rect`, viewer zoom, toolbar actions, and the status accessibility bounds.
- Remote: `draw_list` owns `HEADER_H`, the title/stat/Add/search grouping, list viewport start, `list_scroll_max`, and row placement; `draw_empty_state` owns the onboarding cluster; `draw_button` and `push_hit` publish action rectangles; `accessibility` consumes that hit map. Measure only the heading/action/empty clusters named by Task 20.

**Substantive difference / stop rule**

Photos has a Taffy allocation boundary; Remote calculates geometry while drawing. Split Task 20 out if the fix expands into connection row heights, form/session layout, provider state, or scroll identity. Terminal is excluded from the ledger's original `Photos+Remote+Terminal` candidate because its mono cell grid is a different rendering contract.

## Ledger candidates that should not be batched

| Candidate | Current ruling | Exact paths and concrete reason |
|---|---|---|
| Tasks 7 + 8, Contacts + Disks | Separate | `apps/contacts/src/app.rs`: `layout`, `ROW_H` list virtualization, `layout_detail_actions`, `draw_detail_header`, detail/form scroll, divider ratio. `apps/disks/src/app.rs`: `Layout`, `draw_drive_row`, `draw_header`, `content_height`, `draw_legend`, `draw_smart_card_body`, partitions/actions/confirmations. These are multi-surface stateful layouts, not small rows with the same owner. |
| Tasks 9 + 11, Files + Monitor | Do not dispatch Task 9; keep Task 11 independent | Task 9 directly collides with dirty `apps/file-explorer/src/places_sidebar.rs` and `apps/file-explorer/src/views/file_view.rs`; the active diff already changes `ROW_H`, `ListMetrics`, `row_h`, `header_h`, hit math, and renderer-relative styles. It also depends on dirty `ui/src/widget/nav_shell.rs`. Task 11 separately couples `ROW_H` to `draw_table_rows`, scrollbars, PID selection, keyboard ensure-visible, accessibility, summary/header/detail composition in `apps/monitor/src/app.rs` plus `apps/monitor/src/responsive.rs`. |
| Tasks 10 + 12, Maps + Imager | Separate; defer Task 12 while Imager ownership is active | `apps/maps/src/app.rs`: `draw_directions`, route-step bands, time picker, route scroll/selection, and map viewport. `apps/imager/src/app.rs`: `compute_layout`, `content_extent`, `layout_source`, `layout_drive`, `layout_confirm`, `Hit`, footer/a11y. Imager's target file is clean, but `apps/imager/src/model.rs` and multiple `apps/imager/src/setup/*` files are dirty, so fixtures/state and package validation are not stable ownership boundaries. |
| Tasks 14 + 15, Hex + Logs | Separate | `apps/hex_editor/src/view.rs`: `GridMetrics`, `fit_bytes_per_row`, `hit_test`, `apply_layout`, byte/caret/ASCII equivalence. `apps/logs/src/app.rs` and `responsive.rs`: `ROW_H`, `draw_list_rows`, `row_at`, `column_x`, detail wrapping, push layout, scroll/a11y. Hex is a mono character-cell transform; Logs is a virtual information table with a wrapping detail pane. |
| Tasks 19 + 20 + 21 | Batch only 19 + 20 | `apps/terminal/src/main_body.rs` already has cached `char_w`/`char_h`, exact grid rect, cursor, selection, and scrollback invariants. Task 21 must verify populated VT output and normalize only the idle UI mark; it must not inherit ordinary UI row logic. |

## Standalone task map

| Task | Exact production scope | Why independent |
|---|---|---|
| 1 Sheets | `apps/sheets/src/grid.rs`: `row_h`, `row_top`, `visible_rows`, `has_layout`, cells/headers; `apps/sheets/src/app.rs`: `FORMULA_H`, menus/formula/tabs | Manual row/column sizes, prefix offsets, scroll thumbs, editor/fill/point-mode hit routing, and app chrome must move together. Dirty `ui/src/widget/tabs.rs` is unrelated; keep sheet tabs app-local. |
| 2 Slides | `apps/slides/src/render.rs`: `draw_text_element` and all document text paths; `apps/slides/src/app.rs`: toolbar plus selection/edit consumers | Authored document pixels, canvas transform/inverse, wrapping, caret, thumbnails, Present, and export parity form a unique fixed-document contract. |
| 3 Video Editor | `apps/video_editor/src/app.rs`: `InspectorLayout`, `inspector_metrics`, `inspector_layout`, `inspector_max_scroll`, `inspector_row_rect`, `a11y_inspector*`, `lane_rects`, `clip_rect`, `transport_buttons` | Large coupled inspector/timeline change; seconds-to-x and lane state must remain invariant. |
| 4 Music | `apps/music/src/app/render.rs`: `render_large_text_frame`, real search input/results, route-specific body, sidebar/queue actions and semantics | This is route parity and state preservation, not a small geometry repair. |
| 10 Maps | `apps/maps/src/app.rs`: directions summary, route steps, time picker, content extent/hits | Route state and scroll must coexist with an unchanged pan/zoom viewport. |
| 13 Weather | `apps/weather/src/core.rs`: `ListMetrics`, chip/reorder/hit geometry, hero/tiles/day rows and content height; `apps/weather/src/app.rs`: shell choice only | Reorder insertion, scroll extent, and forecast section accounting are coupled. Dirty `ui/src/widget/nav_shell.rs` changes child typography propagation; settle and recapture it first, then keep Task 13 app-local. |
| 16 Tube | `apps/tube/src/screens.rs`; inspect `apps/tube/src/widgets/flex.rs::build_layout` only if the failure remains | Verification-first closure. If retained Text/Flex is correct, close with evidence; any remaining cache/intrinsic defect needs its own diagnosis. |
| 17 Calculator | `apps/calculator/src/app.rs::relayout`; `apps/calculator/src/graph2d.rs` formula cards/action reservation | Small, but graph canvas preservation and math-field measurement are distinct from ordinary label rows. |
| 18 Ebook | `apps/ebook/src/paginate.rs::layout_chapter`/pagination data; `apps/ebook/src/app.rs` reader geometry, `draw_toc`, reading-position anchor | Full reading reflow and anchor preservation. Dirty `apps/word-processor/*` has no direct path overlap; do not move this task into Word/shared rich-editor code. |
| 21 Terminal | `apps/terminal/src/main_body.rs` idle mark plus existing cell/grid measurement, cursor, selection, copy, scroll | Verification-first character-cell contract; split any production defect from UI token normalization. |
| 22 Settings/Themes/VPN | Settings: `apps/settings/src/sidebar.rs`, `settings_app.rs`, `panes/region.rs`; Themes: `apps/themes/src/widgets.rs`, `preview.rs`, `app.rs`; VPN: `apps/vpn/src/app.rs` only for verified residuals | Retain its prescribed recapture-first internal batch. `apps/settings/tests/category_nav.rs` and `control_interaction.rs` are dirty, so coordinate before changing/extending those tests. |
| 23 Calendar | `apps/calendar/src/app.rs`: `FormRects`/`chips_y`, notes positioning, `draw_detail_notes_header`, `draw_event_form_chips`, `chip` | Small but standalone: one cached form geometry must feed paint, hit/focus, embedded editor placement, and compact tail scrolling. The editor is `ui::widget::BlockEditor`; no Word source edit is needed. |

Tasks 5/6 and 19/20 are covered by the two batches above. Tasks 7–9, 11–12, and 14–15 are covered by the rejected-candidate table.

## Active ownership and collision rules

- **Hard collision:** Task 9 owns the exact dirty Files sources. Do not dispatch it until those changes are integrated or ownership is explicitly transferred. Reassess the resulting behavior first because the current diff already changes the task's row/header/style contract.
- **Shared-shell dependency:** dirty `ui/src/widget/nav_shell.rs` now propagates theme text scale into its isolated child layout. This can change Files and Weather results. Tasks 9 and 13 should consume the settled behavior and must not edit NavShell in their app passes.
- **Shared Tabs:** dirty `ui/src/widget/tabs.rs` changes renderer-relative title styles and elision. No task here should claim it. Sheets, Disks, and Calculator use app-local/semantic tabs in the named paths.
- **Imager package:** Task 12's `app.rs` is clean, but model/setup ownership is active and substantial. Defer the package pass; do not mix measured layout with migration/import work.
- **Word:** dirty `apps/word-processor/*` does not overlap Task 18 or 23. Preserve that boundary: Ebook reflows through `apps/ebook`, and Calendar's notes editor is the shared `ui::widget::BlockEditor` without changing its implementation.
- **Browser:** dirty `apps/browser/*` and Chromium files are outside Tasks 1–23. They create no reason to widen a text-geometry task into browser/tab-strip work.
- **No collision now:** the five files in Batch A and Batch B are clean in the current worktree.

## Dispatch order

1. Batch A (5+6) or Batch B (19+20); they are clean and app-local.
2. Short standalone 17 or 23; run 16 as a verification-only task when capture capacity is available.
3. Independent clean tasks according to severity/dependencies.
4. Task 22 after its existing Settings test ownership clears.
5. Tasks 9, 12, and 13 only after Files/Imager/NavShell ownership settles and the accepted shared behavior is recaptured.

For every batch, commit and ledger each task separately (or use exact-path commits with clearly separated evidence); stop the batch when a task crosses its listed path/API boundary.
