# Task 4 management-disclosure source preflight

Read-only preflight on 2026-09-16. Scope was the Task 4 and Global Constraints sections of `docs/superpowers/plans/2026-09-15-app-composition.md`, the current Firewall/Windows app sources and inline tests, and their baseline review findings. No build, test, VM action, provider action, or app/framework edit was performed.

## Dispatch and source ownership

Firewall and Windows are independent implementation/review passes. They share no app state or source files and should not be assigned as one worker-owned diff. Keep each app's normal composition and scaled geometry in the same app pass; both consume the shared measurement contract without changing it.

| Pass | Owned source | Do not absorb |
| --- | --- | --- |
| Firewall | `apps/firewall/src/app.rs` and its inline tests | Backend/model/action semantics, shared widgets/text measurement |
| Windows | `apps/windows/src/app.rs`, visible labels in `strings.rs` / `locales/en-US.ftl`, and inline tests | `provider.rs`, appliance-owner behavior, `actions.rs` confirmation contracts, Linux/VM state |

Both app trees are clean in the current worktree. The repository is broadly dirty, including root Cargo files and shared `ui/src/app.rs`, `ui/src/text_render*`, and `ui/src/widget/*`; workers must stage only their owned app files and must not sweep shared changes into either commit. The baseline findings remain open: Firewall P1 measured geometry/P2 permanent form (`review-2.md:35-36`), Windows P1 measured geometry/P2 appliance-first composition (`review-3.md:303-304`).

## Firewall ruling

### Actual ownership and behavior

- `FormState` owns the complete local draft (`is_service`, service index, protocol, action, port/source `TextInput`s, and `FocusField`). It is not replicated. `presentation_changes_do_not_create_document_heads` already proves selection/filter/drawer/draft focus remain view state.
- The draft is currently prefilled with port `8080` and source `10.13.0.0/24`, and `FormState::new` focuses Source immediately. Temporary disclosure close must retain these values and choices, while clearing field focus.
- `layout_center` always reserves `FORM_H`/`FORM_H_NARROW`; `build_form` produces the same rectangles consumed by `draw_form`, `handle_click_form`, and `handle_click_text_field`. Closed form geometry does not exist yet.
- `build_table_rows` takes only `max_rows`; there is no rule-scroll state and `handle_event` has no `MouseWheel` path. Rows after the viewport are currently unreachable, not merely offscreen.
- Keyboard precedence is unsafe for a closed disclosure: `handle_key` handles Escape/Tab/Enter before forwarding a focused field, and Enter always calls `add_rule_from_form`. Escape only drops field focus/cancels pending deletion. A closed form would still submit unless this is gated.
- Canonical submit is already correct: `add_rule_from_form` validates, then calls `perform_action` with `actions::add_rule_action`. Keep that route. The header Apply path and effect operation remain separate backend confirmation/application paths and must not be folded into form submit.
- Current custom accessibility exposes retry, service/panic/apply shells, zones, compact route, services, and a rules-table shell. It has no rule-row actions, Add-rule disclosure, form fields, segmented controls, Add, or Cancel nodes. `perform_semantic_action` handles only `firewall.retry`.
- Status text is painted inside `draw_form` (with an accessibility live node at the root). Closing the form must not make validation, delete confirmation, Apply failure, or backend acknowledgement disappear visually; the measured summary/status group is the correct persistent owner.

### Narrow implementation ruling

1. Add local-only `rule_form_open` and rule-table scroll state beside `compact_view`/`form`. The default is closed. Toggling either state must not touch the Automerge document.
2. Extend `Layout` with explicit Add-rule disclosure, Cancel, status, table viewport/content, and open-form rectangles. In the closed state, all form rectangles, including both external `TextInput` rectangles, are `Rect::ZERO`; do not forward cursor, release, keyboard, or semantic events to them.
3. Replace fixed summary/form assumptions with one measured geometry result. Measure the exact title/description/chips/status labels at their painted font/weight/width, then store the resulting rectangles in `Layout`; draw, pointer hit testing, keyboard routing, accessibility bounds, and scrolling consume that same stored layout. Remove the extra painted `Rules` label whenever the Rules/Services route already names the section.
4. Opening by pointer, retained keyboard activation, or semantic activation sets `rule_form_open = true` and focuses Port for a port draft or Source for a service draft. Closing by Cancel or Escape retains draft values, performs no action, clears both `TextInput` focus flags, and returns focus to Add rule. Enter submits only while open; after successful canonical submit, closing is permissible, while validation failure keeps the form open and focuses the invalid field.
5. Add a clamped rule scroll offset based on filtered row count, measured row height, and table body height. Wheel/keyboard scrolling changes local view only. `build_table_rows` must build the visible slice with stable rule IDs so paint, toggle/delete hits, selection, and semantics address the same rows.
6. Add explicit semantics/action targets for Add rule, Cancel, submit, form kind/protocol/action/service controls, and visible rule actions. Closed form nodes are absent, not disabled placeholders. Preserve Rules/Services switching, zone selection/set-active, delete confirmation, and backend-unavailable retry.

### Firewall test seams

- Closed at 480x900 and 800x480: form/input rectangles are zero, semantics contain Add rule and no form controls, table height increases, and scrolling reaches the final filtered stable rule ID.
- Open/close: pointer and semantic/keyboard activation expose identical nonzero geometry; first editable field owns focus; Escape/Cancel leaves document heads and draft values unchanged and returns disclosure focus.
- Submit: closed Enter is inert; open valid Enter/click dispatches the same encoded `add_rule_action`; invalid submit mutates neither config nor document and remains open.
- Route/state: Rules/Services and set-active still work with the form closed/open; unavailable backend exposes retry but no draft controls. At 2x, measured summary/status/form groups do not overlap and all form fields remain reachable.

## Windows ruling

### Actual ownership and behavior

- `WindowsApp` owns local selection, `compact_detail`, sidebar scrolling, hit rows, notice/confirmation state, retained Buttons/signals, and the current destructive delete modal. There is no disclosure state today.
- `draw` paints Add app, an optional capsule/VM destination confirmation, the full appliance management block, and only then catalogue rows. `sidebar_content_bottom` also forces the old appliance minimum (`appliance_y + 440`), so simply hiding paint without fixing extent/clamping will retain blank/scrollable space.
- Selected-app Open is already the prominent primary control. Runner/capsule/storage/access/grants and all install-management actions are always composed below route controls. The detail surface has no working scroll state: wheel handling only changes `sidebar_scroll`, which does not translate the selected-app detail. An open application-details section therefore needs its own bounded/scrollable geometry to keep every applicable command reachable at 480/short heights.
- `handle_event` forwards every retained Button on every frame. `children`, `children_mut`, and `test_rects` also enumerate every command unconditionally. `deactivate_selection_controls` disables many selected-app/appliance controls and clears signals, but does not zero their rectangles; it deliberately leaves global install/ISO controls out. Entering compact detail can therefore leave hidden sidebar buttons with stale rectangles/children.
- Current app-level keyboard handling covers delete-modal Escape/Enter, compact Back on Escape, and catalogue arrow/Home/End selection. Accessibility exposes catalogue rows, compact Back, a description-only selected-app group, notice, and delete modal. It has no appliance/application disclosure nodes or expanded state; `perform_semantic_action` only handles Back, delete modal actions, and catalogue selection.
- Opening a disclosure must be a pure local state transition. It must not set any existing command signal, call `apply_control_signals`, queue a live control, invoke an appliance/application operation, alter selection, or clear `confirm_escalate_to_vm`, `confirm_vendor_uninstall`, or `confirm_delete_data`.

### Command and confirmation preservation

Keep the existing retained Buttons, signals, IDs, and operation routes for every applicable command:

- catalogue/pending install: Add app, private-capsule confirmation, Windows-appliance confirmation;
- appliance: Choose ISO, recovery desktop, start, restart, suspend, resume, shut down, connect for activation, connect for update, revoke connection;
- selected app: Wine/Proton/VM route, reset recommendation, Open app, Open file, Open in VM, publish VM candidate, retry setup, escalate to VM, remove launcher, vendor uninstall, delete data.

Do not change `actions.rs` contracts or bypass `queue_live_control`. Live/native controls must continue to re-resolve through the host. Current confirmation policy includes Confirm for both pending-install choices, all appliance lifecycle/recovery/network commands, VM fallback, retry/escalate, remove launcher, and uninstall; delete-data requires the application-name phrase. The current local second-click/modal safeguards for escalation, uninstall, and delete also remain intact.

### Narrow implementation ruling

1. Add local-only `appliance_details_open` and `application_details_open`. Add clearly named retained disclosure controls and localized open/close labels. Expose named expanded/collapsed state through the custom semantic tree and route semantic activation through `perform_semantic_action`.
2. Sidebar order is title/catalogue identity, Add app, pending destination confirmation when applicable, catalogue rows, then a compact appliance summary. Unprovisioned/setup/error summary keeps its applicable corrective action visible (for example Choose ISO); provisioned secondary lifecycle, recovery, activation/license detail, connection, and maintenance copy live behind Manage Windows.
3. Selected-app identity/status/Open and applicable Open file/Open in VM stay outside the disclosure. Keep route/recommendation controls in the primary app area. Put runner/capsule/storage/access/grants, installation detail, and publish/retry/escalate/remove/uninstall/delete controls in Application details.
4. Give open application details a real local scroll/bounded-panel path; do not reuse `sidebar_scroll` while the sidebar is not painted. One geometry description must drive detail paint, wheel/keyboard movement, retained button rectangles, and semantic bounds.
5. At the start of composition, clear conditional button layouts to `Rect::ZERO` and disable them, then lay out/enable only the visible applicable branch. `children`/`children_mut` must return only currently visible controls; `test_rects` may retain stable IDs but hidden controls must report zero. This includes the whole hidden sidebar during compact detail, closed appliance commands, closed application commands, and commands made inapplicable by a selection/provider refresh.
6. Escape closes the innermost open disclosure before compact Back and restores focus to its trigger. Modal delete handling keeps first priority. Opening/closing preserves selection, `compact_detail`, notice, and pending confirmation state.
7. Recompute `sidebar_content_bottom` from the actual final visible catalogue/summary/details bottom and clamp after every disclosure, selection, provider refresh, and resize. Remove the unconditional old appliance-height floor when details are closed.

### Windows test seams

- Extend the existing stable-ID coverage into a visibility/applicability matrix. For closed appliance/app details, relevant `test_rects` are zero, the controls are absent from `children()`/accessibility, and no signal/control invocation/provider recording occurs merely from opening.
- Open each disclosure by pointer, retained-button keyboard activation, and semantic action; assert expanded state, applicable child order, Escape close, and focus return. In compact detail, hidden Add/ISO/appliance controls must not activate through stale rectangles.
- Use an offline/recording provider or queued native-control fixture to traverse every command listed above and assert the unchanged action address/operation path. Assert the existing confirmation contracts through `app.resources()`, including phrase confirmation for delete; do not execute VM operations in this unit pass.
- Preserve and extend current tests for selection deactivation, delete modal, escalation second click, compact list/detail, failed-setup recovery, and sidebar scrolling. Add application-detail scrolling to the last applicable management command at 480x900 and 800x480.
- Linux visual/interaction evidence is controller-owned. Query the Linux VM's fresh power state immediately before validation and restore only a power transition this Task 4 validation itself made. Never infer Linux state from macOS and never blindly suspend a VM started by concurrent work.

