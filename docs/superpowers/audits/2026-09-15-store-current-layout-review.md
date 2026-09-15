# Store review after the framework layout changes

Reviewed Store source and the five existing shared-host fixture captures on 2026-09-15. This is a review of the isolated, verified framework series through `538161284`; unrelated working-tree changes are excluded. No Store source was changed. The historical audit describes the original baseline, not the current implementation.

## Current visual findings

1. **P2: fixed cards overflow their content.** In the wide capture, Terminal's buttons extend below its card, while adjacent card actions sit higher. Store explicitly requests 180px rows (`apps/store/src/app.rs:2680`) and builds its own body/spacer/action arrangement (`apps/store/src/widgets/card.rs:156`). Correct spacing makes the insufficient allocation visible. The new content-sized CardGrid and composed Card address automatic authoring, but this Store has not adopted them. Preserve fixed sizes; offer an overflow diagnostic and content-sized default rather than silently overriding manual intent.
2. **P2: artwork escapes its allocated tile.** Files artwork crowds its title, and the detail Notes artwork extends beyond its intended region. `Artwork::draw` uses positive `Rect::inset` (`apps/store/src/widgets/artwork.rs:167`), which expands the rectangle. This is an app defect. A framework inner-padding helper or standard artwork widget would reduce the opportunity for this error; universal clipping would incorrectly hide intentional paint overflow.
3. **P2: page regions lack a shared alignment and density policy.** The narrow header starts around x=28 while filters and catalogue start around x=12. The first app appears only around y=445 in an 844px viewport. Multiple source/result summaries and nested headings consume space. Store individually assembles header padding, spacers, filters and content regions (`apps/store/src/app.rs:2209`). Automatic forms now exist, but they are not a catalogue-page composition. A reusable catalogue page should coordinate title, search, filters, results, gutters and utility actions.
4. **P2: large text exposes fixed control allocations.** The configured 200% capture has real enlarged text, but fixed-width filter labels truncate and the header consumes much of the viewport. Treat text growth, wrapping and control measurement as a whole-screen contract. Preserve explicitly authored dimensions and surface conflicts. The unchanged legacy Store screenshot driver is not equivalent to the shared App/FrameHarness settlement path; use the shared host for acceptance captures.

## Remaining framework code findings

Independent whole-series review found two additional P2 issues that are not inferred from pixels:

- `ui/src/widget/composition_parts.rs:54`: shared retained-child forwarding omits `start_resource_operation_with_document`. New transparent containers can prevent a descendant's document-changing action from executing through the root.
- `ui/src/widget/manual.rs:45`: a changed captured manual-policy input has no explicit invalidation signal. After a painted, clean frame, same-viewport layout can reuse stale geometry. The current acceptance test does not assert moved geometry after paint. The manual path needs a documented, tested update mechanism while retaining child interaction state.

Follow-up: `313ba237a` corrects forwarding and `a7c6da63c` adds explicit Manual invalidation. New regressions cover both defects, and the corrected isolated full UI suite passes 3,041 tests with two ignored. Final scoped review and visual acceptance are tracked in the implementation progress audit; these paragraphs preserve the review findings rather than claiming they remain unfixed.

## Earlier defects already addressed

- Intrinsic Column gaps now take effect.
- CardGrid descendants receive host geometry and keyboard traversal before painting.
- Shared-host viewport resizing produces the same Store layout as fresh construction at the destination size.
- Themed Text scaling and fit-content Button measurement were corrected.

Do not list these as current unfixed bugs. Their source evidence and verification are in the framework progress audit.

## Conclusion

Pleasant default layout requires measured content, coordinated page regions, reliable updates, and diagnostics when explicit constraints cannot fit. Good individual widgets alone do not deliver this. Automatic composition should cover common screens; selective overrides and fully manual measurement/placement should use the same input, focus, layout and rendering lifecycle.

## Evidence

Capture directory: `/Users/xaver/.codex/visualizations/2026/09/15/01a0a50b-c5e5-7e63-bc71-bf4e2b8ac8cd/framework-layout-changes/07-store-host/`.

The recovery comparison verifies 37 framework files and 26 unchanged Store files. All five Store PNGs and full inspection JSON match the preceding accepted capture set. These are deterministic fixture renders, not a live remote catalogue or a user usability study.
