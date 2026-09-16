# Task 5 review — Intrinsic card and section minimum propagation

## Verdicts

- **Spec verdict: NEEDS FOLLOW-UP.** The intrinsic minimum propagation, fixed/manual compatibility, scroll ownership, resize stability, and focus reachability requirements are covered. The explicit requirement that the trailing action be invoked by keyboard exactly once is not actually exercised.
- **Quality verdict: PASS WITH CONCERN.** No production defect was found in the scoped change. The implementation is minimal and the layout regressions are substantive. The concern is limited to the interaction test's claim and does not invalidate the card sizing fix.

## Finding

### [P2] The new regression does not verify keyboard activation

`automatic_content_cards_expand_a_short_scroll_stack` proves that the final action is reachable after scrolling, that pointer activation fires once, and that Tab traversal ends on the final action. It then calls `perform_named("final-action", Action::Activate)` and describes the result as focused activation. That API dispatches an accessibility semantic action directly; it does not send Enter or Space. The live keyboard behavior is a separate host fallback in `app.rs` that activates the focused widget when an unconsumed Enter/Space press arrives.

This leaves the brief's explicit “pointer and keyboard invokes it exactly once” requirement unverified. Add a host-level regression that scrolls to the final action, tabs to it, sends Enter or Space through the live application event route, and asserts one additional invocation. If the current `FrameHarness` cannot represent that route, place the focused activation assertion in the existing application-host harness and keep the geometry/scroll assertions here. The existing semantic activation check may remain as accessibility coverage, but it should not be labeled as keyboard coverage.

## Spec assessment

- `Card`, `ContentCard`, and `CardGrid` change only the block-axis minimum from zero to automatic. Their automatic outer height and zero minimum width remain intact, so the change targets intrinsic Grid contribution without removing narrow wrapping.
- The automatic-card and legacy-card regressions measure descendant containment, sibling clearance, and scroll content extent before paint. The nested narrow and wide fixtures ensure each grid encloses its final card and the following section starts below prior content.
- `content_cards_keep_explicit_fixed_rows` asserts an authored 180 px row stays exactly 180 px with oversized prose. Existing fixed CardGrid row coverage and the exact manual Button size assertions preserve the stated compatibility contracts.
- The short viewport test keeps the viewport at 360 px while content grows beyond it and confirms that the final action becomes reachable after scrolling. The resize test compares wide→narrow→wide geometry against a fresh wide layout at 1× and 2× text scale.
- The supplied controller review independently inspected all eight captures and found the Store and Region overlaps resolved. Its remaining Settings balance observations belong to the later app task and are outside this review.
- The three broad-suite failures are baseline-confirmed in the supplied isolated comparison and are unrelated to this task; they do not change either verdict.

## Quality assessment

The production diff is three symmetric one-line behavior changes with comments explaining the Grid sizing reason. It introduces no measured pixel workaround, application change, dependency change, or unrelated formatting. The focused tests reproduce the original overlap at both component and dependent-app levels, cover direct and nested compositions, and retain explicit-size behavior. Aside from the keyboard-path gap above, the tests make the intended contract observable and guard the named regression risks.
