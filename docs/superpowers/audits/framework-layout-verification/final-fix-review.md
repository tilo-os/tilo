# Final scoped fix re-review

## Scope and verdict

Reviewed `5381612849d749c8cc0bb497c85689936f1c9fd1..7bebe26b6` using `final-fix-review.diff`, the three scoped commits, `final-fix-brief.md`, the original findings in `final-review.md`, and `final-fix-report.md`. Inspected adjacent host/measurement contracts only where needed to assess these fixes. This is the scoped re-review, not a repeat of the whole-series review.

**Scoped source verdict: PASS. All three findings are ADDRESSED; no new fix-local issue found.** The corrected full UI run is green. Final acceptance still depends on the controller-owned refreshed visual/starter gates; their in-progress status is not a new code finding.

## Per-finding verdicts

### 1. Mutable resource-operation admission — ADDRESSED

Commit `313ba237a`; `ui/src/widget/composition_parts.rs:77`.

The shared macro now calls each child's `start_resource_operation_with_document` with the original invocation, operation kind, payload, document and path. It returns immediately on the first handled result, propagates errors, and returns `None` after declining children without a second read-only traversal. This supplies the existing transparent-container contract to both macro forms, including Manual's custom-apply variant.

The direct Manual and nested Section → Manual regressions (`ui/src/widget/composition_parts.rs:274`) invoke the composition root, distinguish mutable from read-only admission, assert one mutable call, and verify the supplied nested document path is mutated only on acceptance. Both accepting and declining cases are covered. The code itself also preserves first-result/error semantics. These tests do not run the complete exported-action dispatcher, but they exercise the exact root hook that the established dispatcher invokes; that is sufficient for the identified forwarding defect.

### 2. Manual policy inputs after a clean same-viewport frame — ADDRESSED

Commit `a7c6da63c`; `ui/src/widget/manual.rs:31`, `:93`, `:102`, `:111`, `:353`, `:429`; documentation `ui/MANUAL_LAYOUT.md:63`.

The public, cloneable `ManualInvalidation` handle makes captured-input invalidation explicit. Requested versus applied revisions feed `layout_needs_measure`, so an ordinary scheduled layout frame reaches the existing solver even when children are clean and viewport width is unchanged. Actual application records the successfully evaluated revision. The revision is sampled before policy execution; failed evaluation does not record success, and a newer invalidation is distinguishable from the revision evaluated. Existing parent-before-child policy traversal restores actual descendant decisions after ancestor probes. The factored Parts placement helper preserves the former child/origin behavior.

The documented requirement to call `invalidate()` **and schedule an ordinary host layout frame** matches the controller-approved API. It does not promise observation of arbitrary captured cells or a window wakeup from the handle. Retained children need not be rebuilt.

The new App regression (`ui/src/app/manual_composition_tests.rs:126`) paints and calls `mark_drawn`, asserts the tree is clean, then verifies an unchanged layout frame does not call the policy. It changes the captured offset, invalidates, and asserts an actual 105.5 px displacement, unchanged node references/focus/text/selection, updated accessibility bounds, and clicks rejected at the old location and accepted at the new one. It covers direct Manual, Form inside Manual, and Manual inside Form; a final clean frame again performs no policy evaluations. The additional lifecycle regression (`ui/src/widget/manual/tests.rs:706`) checks pending state after hypothetical solve, failure, and invalidation during policy evaluation. This closes the original test's missing clean-frame and moved-coordinate evidence.

### 3. Default NumberInput at 200% text — ADDRESSED

Commit `7bebe26b6`; `ui/src/widget/number_input.rs:525`, `:555`, `:632`; shared internal font constant `ui/src/widget/text_input.rs:118`.

Inspected the supplied original starter screenshot: the field truncates `15 min` and the stepper glyphs sit low. The correction scales the automatic minimum footprint with theme text scale, then uses renderer measurement of the formatted committed value plus the actual embedded field padding, two square steppers and the existing gaps. Width rounds upward for host pixel rounding. This uses content measurement rather than a fixture-specific replacement constant. Glyph placement now centers the renderer's measured line-box height, consistent with the renderer's scale ownership.

`with_size` disables automatic measurement/scaling and retains the pre-existing interaction target-height minimum. The measurement hook preserves authored min/max constraints, allowing exact Manual allocations to win. The embedded field still receives the final actual rectangle. No input/commit behavior was replaced.

Regressions cover 100% and 200% value fit, bounded fixture width, explicit dimensions, retained edit/commit behavior, exact fractional Manual placement, and the actual emitted stepper glyph transforms/font sizes. The App SetValue regression (`ui/src/app/number_input_layout_tests.rs:5`) checks that the real action path schedules layout, widens the retained control for a longer committed value, and preserves its root node and focus. This establishes the source correction; the controller's refreshed generated-starter capture remains the final visual acceptance gate.

## New fix-local issues

None found. No severity/file-line defect is reported.

## Evidence checked and its limits

- Read the recorded RED evidence: mutable routing failures in `01-admission-red.log`; zero movement in `03-manual-red.log`; measured value clipping, glyph-height failure and unchanged retained App width in `05-number-measured-red.log`.
- The reported focused GREEN runs are consistent with the inspected tests and code: admission 2 passed, Manual filter 29 passed, NumberInput filter 21 passed. The report explicitly identifies which historical runs included the superseded unrelated default hook; those runs are TDD history, not final isolated-source proof.
- `final-source-evidence.json` records final composed tree `eeb640b08d05b44f6c4748d16f26cb7796a56a87`, fix head `7bebe26b6`, 39 framework file hashes and 26 unchanged Store file hashes. This is the controller's source-provenance record, not a new independent manifest generated by this reviewer.
- `10-isolated-clippy.log` records successful completion on corrected capture-source. `12-isolated-fmt.log` has no diagnostics; the controller/fix report records exit 0.
- The completed corrected `11-isolated-full-ui.log` records **3041 passed, 0 failed, 2 ignored**, including the new regressions. Its final summary was read directly. `09-full-ui.log` is explicitly superseded and is not treated as final evidence.
- No suites were rerun. No production, Store, index or checkout mutations were made. This report is the only review-owned file written.

## Out-of-scope observations and residual gates

No additional out-of-scope defect was investigated or raised. The documented explicit scheduling contract and unconstrained automatic width for arbitrarily long values are accepted API limits, not failures of these bounded fixes. Store migration, unrelated runtime changes, and broad authoring/architecture review remain outside this re-review.

Controller-owned remaining gates: verify refreshed generated-starter execution/captures and final visual/source provenance; close the overall acceptance/status documents. This report approves the scoped fixes without claiming those separate gates are complete.
