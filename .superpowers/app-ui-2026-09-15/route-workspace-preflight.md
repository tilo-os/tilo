# Task 2 route-workspace source preflight

Read-only preflight at `94f161255`. Scope was limited to Task 2 plus Global Constraints in `docs/superpowers/plans/2026-09-15-app-composition.md`, the Containers and Diff/Merge findings in the group-1 review, and the source/tests named below. No build or test was run. The listed production files had no staged or unstaged changes before this artifact was written; the `.superpowers/app-ui-2026-09-15/` directory already contained unrelated untracked evidence.

## Execution ruling

Execute Containers and Diff/Merge as **two separate bounded tasks**.

- Containers has replicated `ViewDoc` section/selection/detail-tab state, a local animated rail, and a draw-cached compact flag. Its key unresolved question is how a local list/detail route coexists with the selected CRDT resource.
- Diff/Merge keeps `MergeView::selected_source` and `DiffView::compact_show_right` local, while only merged text/resolutions cross the canonical `DiffmergeAction` path. Its key unresolved work is one measured geometry/focus model shared by paint, pointer, keyboard, and accessibility.
- They share no production files. Their test seams and state-transition risks are materially different, so a combined worker/commit would make failures harder to attribute.

## Controller rulings needed before production work

1. **Containers Back semantics:** the plan says Back preserves selection and Global Constraints say compact route state stays out of replicated documents. Current Back/Escape clears `ViewDoc::selected` (`panels.rs:3182-3184`, `3362-3369`), and `uses_single_pane_detail` derives the route only from `selected.is_some()`. If preservation means the selected row remains selected after Back, authorize a local `compact_detail_open`/route state and define its initial resize behavior. Reusing the current transition cannot satisfy that acceptance statement.
2. **Search shortcut reservation is outside the listed Containers files:** `SearchField` lays its input out with a fixed 30px trailing gutter, then paints a separately measured shortcut chip (`ui/src/widget/search_field.rs:34`, `196-215`, `274-277`, `346-351`). Increasing the app's search width or shortening “Search containers…” does not actually reserve the chip. Either add a small shared `SearchField` prerequisite/scope item with a focused launcher geometry test, or explicitly reduce acceptance to an app-local shorter placeholder. The former matches “reserve ... before placeholder” and does not require a global token/padding change.
3. **“Last log selectable” needs a definition:** Containers logs are painted strings with wheel scrolling only (`panels.rs:1357-1409`); there is no log-line hit, selection state, text-selection widget, or log accessibility child. If “selectable” means user text selection, this is new behavior and should be separately scoped. If it means visible/reachable after scrolling, amend the verification wording and test scroll-to-tail geometry.
4. **Diff/Merge Tab/Enter is missing, not just untested:** root Tab is consumed by a toolbar-only six-action focus list and clamps at its end (`app.rs:772-824`). Merge chips have pointer hits and semantic targets, but no keyboard focus state or Enter handler. The task therefore needs an explicit focus handoff/order from toolbar to visible source selectors and visible resolution chips, plus focus retention after reflow. Merely extending the named tests cannot meet the acceptance text.

## Containers

### Current compact, geometry, and action seams

- `COMPACT_VIEWPORT_WIDTH` remains 600 and `compact_layout_for` is only `viewport_width < 600 || base_text_size >= 20` (`app.rs:62-63`, `2654-2663`). It does not use rail width, measured row/status/action minimums, detail minimums, or the actual content rect.
- At 800px normal size with the expanded 200px rail, content is 600px. With a selected container, current geometry produces `max(600 - 420, 240) = 240px` for the list and only 360px for detail (`app.rs:55-57`; `panels.rs:531-536`). This is the reviewed failing three-column case. With the rail manually collapsed to 56px, content is 744px, yielding a 324px list plus the intended 420px detail. A feasibility function must therefore consume the live rail state as the plan requires; replacing 600 with another window constant is insufficient.
- A running row reserves three 32px icon buttons plus inter-button gaps and trailing inset at the far right (`panels.rs:26`, `982-1018`). In the 240px selected-list geometry, `draw_container_row_layout` also puts the status pill in that trailing region while `draw_container_row` stays on the noncompact branch because the viewport is 800 (`panels.rs:779-843`). That split between `list_compact` and `uses_compact_layout` is the direct overlap seam.
- Draw stores `compact_layout_active`; wheel routing uses that cached value but also falls back to the old 600px threshold (`app.rs:277-281`, `2640-2651`). `draw_containers`, row layout, detail layout, accessibility, semantic activation, and control bindings all ultimately depend on the last painted hit list. A single computed mode must be installed before hits are produced and then reused for event routing and semantic/control exports.
- Selection and tab are replicated `ViewDoc` state. Selecting a row toggles selection; Back and Escape clear it (`panels.rs:3140-3143`, `3178-3184`, `3362-3369`). There is no separate compact route state today. The current integration test encodes this clearing behavior.
- Pointer and accessibility already share `Hit`/`apply_hit`: accessibility nodes come from painted hits, and `perform_semantic_action` resolves the same target back to a hit. `control_action_bindings` also derives action addresses from those hits. Preserve this seam rather than adding a second lifecycle path.
- Lifecycle controls already queue an inert `ControlInvocation`; they do not run Podman or mutate the engine before host authorization. Existing coverage drains the invocation once but does not assert that a second drain is empty.
- Scale geometry remains partly fixed: compact detail header 76px, tab strip 40px, toolbar 48px, and fixed y offsets (`panels.rs:1081-1090`, `1111-1176`, `1193-1220`). Log rows already derive line height from the same token size used to paint (`panels.rs:1373-1396`). `draw.rs` now measures and paints with the same supplied size and uses the base transform only for host DPI, so the plan's old `draw.rs:79` double-scale suspicion is stale in current source. Remaining app-local work is measured vertical geometry, not a reciprocal scale correction.
- Compact row text already reserves 138px at the trailing edge (`panels.rs:779-814`), but the desktop/detail-open row does not use that branch at 800. The measured feasibility decision should prevent that mixed geometry rather than adding another ad hoc reservation.

### Missing semantics / plan-source mismatches

- The plan says to replace `compact_breakpoint_preserves_tablet_desktop_but_protects_large_text`; the current test still asserts 768px normal text is desktop (`app.rs:2797-2811`). This is an explicit stale expectation.
- `compact_selection_is_a_full_page_drill_in` verifies the selected compact detail and large-text row height, but not 800px measured feasibility, rail-expanded versus rail-collapsed feasibility, selection/tab retention through resize, or local route state (`app.rs:2815-2833`).
- `compact_container_detail_is_a_readable_drill_in_and_escape_returns_to_list` asserts Escape clears the selected CRDT id. That conflicts with a literal reading of “Back ... preserves selection” and must change only after ruling 1.
- There is no pointer/keyboard/accessibility parity test for Back, no geometry assertion that Running/status and row actions are disjoint at 800, and no SearchField placeholder/chip bounds test.
- There is no disconnected-engine compact route test, no 800x2 scroll-to-tail test, and no test that lifecycle controls dispatch exactly once.

### Focused existing tests to retain/extend

- `app::tests::compact_breakpoint_preserves_tablet_desktop_but_protects_large_text` — replace the 768 desktop invariant with measured content feasibility and include rail state.
- `app::tests::compact_selection_is_a_full_page_drill_in` — extend across 800 normal, 480 normal, and 800x2 while retaining selected id/detail tab.
- `tests/interaction.rs::compact_container_detail_is_a_readable_drill_in_and_escape_returns_to_list` — adapt to the chosen Back/selection semantics and exercise pointer, Escape, and semantic activation through the same hit.
- `tests/interaction.rs::painted_controls_have_named_semantics_and_activation_reuses_hit_dispatch` — retain as the semantic/hit-path guard.
- `tests/interaction.rs::canonical_log_resource_opens_collaborative_detail_state` — retain for canonical selected id + Logs tab adoption.
- `app::tests::lifecycle_control_emits_the_exported_effect_instead_of_running_podman_inline` — add a second-drain-is-None assertion and exercise a real painted compact action.
- `tests/interaction.rs::sidebar_collapses_and_expands_via_shortcut` — combine with feasibility assertions so rail state is an input, not a visual side effect.

## Diff/Merge

### Current compact, geometry, and action seams

- Merge, two-file diff, and toolbar each independently use a fixed 640px breakpoint (`merge_view.rs:37-40`, `183-205`; `diff_view.rs:28-34`, `193-195`; `app.rs:527`). Neither pane decision sees theme text scale or measured labels.
- At 800px merge width, each source pane is about 266.7px. Even the current character estimate for the desktop conflict group needs about 268.8px for the three chips plus 8px pane inset, before any font-metric correction (`merge_view.rs:589-650`, gaps advanced at `685`). Thus the source itself proves the reviewed “Take both” failure: 800 cannot satisfy the current three-pane action group.
- Chip widths are estimated from UTF-8 byte count (`label.len()`) while paint uses `TextRenderer` at 11.5 (`merge_view.rs:649-677`). Replace that with measured text plus the same horizontal padding and gaps, and use the full group width as an input to the pane-mode decision.
- `selected_source` is local and is not reset by `set_inputs` (`merge_view.rs:116`, `149`, `158-177`, `187-191`). `DiffView::compact_show_right` is likewise local (`diff_view.rs:58`, `80`, `127-136`). This already satisfies the nonreplicated compact-source-state constraint and naturally preserves the choice across widening/narrowing; add tests rather than moving either into `MergeDoc`.
- One fixed `is_compact` currently drives merge headers, separators, source rects, painted source, keyboard arrows, accessibility, and synthetic source activation. Preserve that single decision. Because measured text is available during draw while event/accessibility have no renderer, cache a compact/layout geometry result before producing hits and reuse it everywhere; do not reintroduce separate width checks.
- `action_hits` are the painted chip rects and carry `(hunk, HunkChoice)`. Pointer calls `resolve`, the parent drains one pending `DiffmergeAction`, and semantic resolution routes through the same `resolve`/drain path. Existing tests cover one undo boundary and byte-identical palette/chip envelopes. Keep these action bindings intact.
- Resolution accessibility already exports the action target and painted bounds from `action_hits`. Compact source accessibility uses hashed child keys plus `source_selector_rects`; any cached layout decision must feed both paths.
- The confirmed scale work is still source-visible: merge uses fixed `ROW_H=19`, `HEADER_H=30`, `ACTION_H=26`, and raw font constants; diff uses fixed `ROW_H=20`, `HEADER_H=34`; toolbar is a fixed 48px row with character-estimated button widths. A scale-aware pane decision alone will not close the 200% row/toolbar finding. Row/header/action heights must derive from the actual painted font metrics or the accepted shared metric API, and those same heights must drive hunk spans, culling, scroll, hit rects, and merged rows.

### Missing semantics / plan-source mismatches

- `phone_merge_uses_a_full_width_source_switcher` explicitly asserts the selector disappears at 800. This is the opposite of Task 2's measured-feasibility requirement for the current conflict fixture and must be replaced with a bounds/feasibility assertion.
- `compact_source_switcher_has_pointer_keyboard_and_a11y_parity` covers source selection only at 390px. It does not test preserving Theirs across 390→1200→800, nor a scale-triggered compact mode.
- Toolbar focus tests cover six toolbar actions only. Root Tab never reaches source selectors or resolution chips, so the required Tab/Enter resolution path does not exist.
- `root_accessibility_includes_active_hunks_and_uses_the_crdt_resolution_path` proves semantic activation mutates the CRDT but not that it creates the same undo boundary as pointer/keyboard. Reuse the existing undo test helper for all three activation routes.
- `clicking_a_resolution_chip_resolves_that_hunk_through_the_pointer_path` runs at 1200 only and does not assert every chip rect stays inside its owning pane/panel at 800.
- There is no direct test that `hunk_spans`, click rects, scroll/culling, and merged row baselines use one scale-derived row metric.

### Focused existing tests to retain/extend

- `phone_merge_uses_a_full_width_source_switcher` — rename/generalize to measured feasibility; assert compact at the current 800 conflict fixture and wide at a width where the measured group plus source minima fit.
- `compact_source_switcher_has_pointer_keyboard_and_a11y_parity` — retain and add resize/source-retention coverage.
- `phone_toolbar_keeps_every_control_visible_without_overlap` — retain; add 800x2 with measured toolbar metrics.
- `toolbar_controls_have_semantic_names_focus_geometry_and_activation` and `toolbar_keyboard_tab_and_enter_follow_the_painted_control_order` — extend the root focus order into the active comparison controls.
- `root_accessibility_includes_active_hunks_and_uses_the_crdt_resolution_path` — retain semantic resolution and add undo parity.
- `clicking_a_resolution_chip_resolves_that_hunk_through_the_pointer_path` — run the same hunk/choice at 800 compact and assert all chip bounds are within the panel.
- `chip_dispatch_records_one_undo_boundary_and_reverts` and `chip_and_palette_encode_identically` — preserve as the canonical action/undo invariants; use them as the common assertion for pointer, Enter, and semantic activation.
- `phone_diff_uses_an_accessible_one_source_switcher` — retain if the shared scale-sensitive compact decision is applied to `DiffView`; add selected-side retention across resize.

## Bounded task boundaries

1. **Containers route/geometry task:** `apps/containers/src/app.rs`, `panels.rs`, `tests/interaction.rs`, plus `draw.rs` only for measured geometry if evidence requires it. It must first resolve Back/local-route semantics. Do not change replicated selection/tab schema or lifecycle action authority.
2. **SearchField launcher reservation prerequisite:** `ui/src/widget/search_field.rs` and its focused tests, if controller accepts the exact Task 2 wording. Keep it separate from Containers production ownership because it is a shared widget.
3. **Diff/Merge measured route/focus task:** `apps/diffmerge/src/app.rs`, `merge_view.rs`, and `diff_view.rs` only where the same compact/scale decision is consumed. Preserve local source selection and the pending `DiffmergeAction` route.

These boundaries avoid a whole-repository pass and keep the reviewed normal 1200px compositions intact while addressing the proven 800/480/200% failures.
