# Whole-series independent review

Reviewed owned ranges `9e3eb2e68..838b13acd` and `82afabfb7..538161284`, using `final-review.diff`. Unrelated runtime/desktop commits and shared working-tree changes are excluded. Authority is the accepted seven-task plan, its binding spec, and `final-review-context.md`'s explicit scope rulings.

## Spec compliance verdict

**Needs fixes before final acceptance.** The seven implementation areas are present and form a coherent automatic/guided/manual authoring model. Two cross-cutting lifecycle gaps below remain: transparent composition can lose a child's mutable action route, and the same-viewport manual-policy update claim is not established for a clean painted frame. Neither calls for Store migration or expansion into the architecture's optional roadmap.

The implementation otherwise meets the bounded source requirements:

- Intrinsic Column gaps use host Grid geometry, retain child identities, and preserve Row alignment by scoping the anti-stretch behavior to vertical Scrollable.
- CardGrid children participate in host layout/input/focus before paint. Fixed rows remain exact; content-sized cards and actions are additive. Content-card body expansion and footer alignment use retained tracks.
- App and FrameHarness share bounded measurement/style settlement and viewport recomposition. Failed settlement skips paint and invalidates the successful-viewport marker. Unique-key focus restoration preserves selection without sending state to ambiguous or ineligible targets; native attachment occurs after successful final composition.
- Text explicitly distinguishes renderer-relative, theme-resolved, and fixed logical font sizes. Measurement, painting, plugins, and Checkbox normalization use consistent ownership.
- FormPage owns readable width, gutters and scroll; Form/Section/Field provide hierarchy, measured label placement, help/validation and independent supported overrides with reset and real origins. The automatic fixture has no authored layout pixels or viewport branches.
- Manual exposes child-indexed real retained-subtree measurement and exact logical placement. Its owned policy descriptor ends widget borrows before probing and reuses original admission and owner execution leases. Allocation and content bounds stay distinct, including the reviewed nested-negative-overflow fix. The pure manual pilot places individual labels, controls, support text and actions; both mixed directions exist.
- Existing inspection receives optional policy metadata through the single `Widget::layout_info` hook. Docs retain the low-level SDK, disclose precision/probe/host limits, and the starter adopts real automatic widgets while retaining its controls and bindings.

## Code quality verdict

**Needs fixes — two P2 findings.** No critical memory-safety or geometry-ownership defect was found. The implementation generally uses existing retention, dispatch, renderer and inspection mechanisms rather than parallel systems. Behavioral tests cover literal geometry, emitted glyph metrics, real App input/focus, recovery, owner refusal and mixed subtrees. The missing lifecycle cases below are material despite the passing full suite.

### P2 — Forward mutable resource-operation admission through new transparent containers

**Source:** `ui/src/widget/composition_parts.rs:54-67` (`retained_parts!`, used by Manual, Form, Section, ContentCard and the other new composition wrappers).

The macro forwards update/sync/events but omits `start_resource_operation_with_document`. Its users therefore inherit `Widget`'s default at `ui/src/widget/mod.rs:1500`, which calls the read-only `start_resource_operation` route. That route invokes descendants' read-only hooks, never their mutable admission hook. A child implementing only the mutable hook is discovered but cannot execute; a child with a legacy read-only adapter can instead execute the wrong path and miss its document follow-up or invocation lifecycle.

This is reachable in the real action path: `bound_dispatch::invoke_resource_action` passes the component root to `execute_resource_operation` (`ui/src/bound_dispatch.rs:1101`), which calls its mutable hook (`:1010`). Baseline Column, Row and Container explicitly forward this method, and the baseline Widget contract requires transparent containers to do so. Existing Camera and Settings implementations use the hook (`apps/camera/src/app.rs:1667`, `apps/settings/src/settings_app.rs:912`); this is an existing capability, not a new requested feature.

**Reproduction:** Put a retained child whose `start_resource_operation_with_document` writes a marker/returns `Accepted`, and whose read-only hook declines, under `Manual::child` or `Section::content`. Invoke its published effect through the root. The mutable child callback is not reached and the operation is reported unknown. The same child under Column receives the mutable call.

**Required correction:** Add the existing transparent-container forwarding behavior to the shared macro, preserving the first handled result and avoiding a second read-only fallback traversal. Add a focused regression that routes through the composition root and proves the mutable child receives the document/invocation exactly once. Exercise a nested composition path as well as a direct Manual child. No application or Store edits are needed.

### P2 — Make same-viewport manual policy changes work after a clean painted frame

**Source:** `ui/src/widget/manual.rs:45-79`; interaction with `ui/src/app.rs:6856-6860`. The current acceptance test is `ui/src/app/manual_composition_tests.rs:60-91`.

Manual stores an arbitrary policy closure but has no policy revision/invalidation signal, and inherits `layout_needs_measure` from its children. After the first real draw and `mark_drawn`, an ordinary fixed Button reports no layout dirtiness (`ui/src/widget/button.rs:1076-1088`). Changing an `Rc<Cell<_>>` captured by the manual policy therefore leaves the subtree clean. Even if the host schedules a layout-dirty frame at the same viewport, `run_layout_pass` skips `compute_widget_layout`; it reapplies the old Taffy placement and never calls the policy. FrameHarness's unconditional rebuilding can conceal the live-host difference.

The supplied local-offset regression never draws/marks the tree clean and never asserts the changed input X coordinate. Its Save button has a constant rectangle, so successful clicks there do not prove the offset changed. Thus it cannot establish the documented live closure-input/local-override behavior.

**Reproduction:** Create Manual with one fixed Button and a captured X offset. Lay out, draw and mark the frame drawn; change only the offset from `0.25` to `5.75`; request an otherwise ordinary same-viewport layout frame. With scroll optimization enabled and no structural rebuild, the policy is not called and the Button retains its old X coordinate.

**Required correction:** Provide and document a usable policy-input invalidation/revision path that causes the live host to solve again while retaining children. If the intended contract is instead an explicit structural rebuild for every such input change, make that requirement concrete in the public manual authoring path and test that supported route; merely scheduling redraw/layout is insufficient. Extend the regression to paint and clear dirtiness before each change, then assert the moved rectangle, corresponding hit/accessibility geometry, and retained focus/selection at unchanged width. Keep unchanged scroll/paint frames free of needless full solves.

## Complete-series review and named cross-cutting checks

Reviewed the production deltas across all seven tasks, the examples/scaffold, relevant tests, and documentation; per-task verdicts were supporting evidence, not substitutes for reviewing their interactions. Particular checks included Card/Column/Scrollable sizing, Form hooks during Manual probes, final policy metadata after hypothetical measurement, fit Button min/max precedence, owner lifetime/admission, focus recovery and native mounting, negative overflow and clipping, and generated starter dependencies/semantics.

Outside owned code was inspected only for these concrete risks:

- `Widget` dirty/default routing contracts and Button/TextInput postpaint dirtiness, to evaluate manual policy invalidation.
- `bound_dispatch`'s actual root invocation path and existing Column/Row/Container forwarding, plus two existing mutable-handler examples, to establish the action compatibility finding. The forwarding contract already exists at the original base.
- Existing child drawing/culling methods, to check how the Manual descendant-bounds fix participates in parent rendering.

No production/index/checkout changes, subagents, or repeated test suites were used. This report is the only review-owned written artifact. The two findings are derived from concrete source control flow; no newly executed regression result is claimed.

## Verification evidence and residual gates

Read the durable final evidence available during this review:

| Evidence | Recorded result |
| --- | --- |
| `final-checks/full-ui.log` | 3033 passed, 0 failed, 2 ignored |
| `final-checks/manual.log` | 26 passed, 0 failed |
| `final-checks/starter-test.log` | Actual external generated starter: 2 passed, 0 failed |
| `final-checks/clippy.log` | Completed successfully |
| `final-checks/capture-comparison.json` | 37 framework files and 26 Store files verified; 5 Store, 16 Form and 6 Card regression PNGs unchanged; Manual fresh/resized PNG/JSON equality true |

These are controller-produced evidence, not reruns or independent pixel inspection by this reviewer. Their presence closes the earlier missing-log/capture-regeneration concern; it does not cover the newly identified lifecycle cases.

Before claiming final completion:

1. Address or concretely resolve the two findings, run their focused regressions, and obtain a scoped re-review of those fixes. Refresh affected final evidence if source changes.
2. Close the controller-owned acceptance matrix, plan/spec status and public progress audit. At review time those documents still contain Task7 pending entries despite source and recovered evidence now existing.
3. Complete the final source-provenance/visual assessment and user-facing comparison/notification with usable links, attributing the configured capture pipeline and retaining the limitations below.

Approved limits remain binding: Store retains its authored fixed 180px cards and existing artwork issue; additive Card/Form APIs require adoption. Raw Store `--shots` bypasses shared own-style settlement, while configured FrameHarness is the demonstrated host. Manual probes measure current retained topology, not every hypothetical legacy AppShell topology. Metadata reports available facts, not invented source locations. Source-decision counts do not prove elapsed productivity savings, equal baseline polish, or human pleasantness acceptance. No universal workspace policy, visual editor, serialization-wide migration or human study is required to close this seven-task implementation.
