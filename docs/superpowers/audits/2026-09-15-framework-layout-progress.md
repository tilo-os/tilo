# Framework layout implementation progress

Status: in progress. Store source remains unchanged. This report separates completed changes from remaining work.

## Change 1: reliable intrinsic column gaps

Commits: `f14fdb98f`, `050c5653e`, `bab974dc8`.

Intrinsic Columns now use a single-column Grid with natural row sizes and explicit gaps. The host tree retains exactly one node per widget child. Vertical Scrollable owns the non-stretch behavior needed for natural content growth; normal Row centering and horizontal scroll stretching remain intact. Framework Card title/body spacing remains 12 pixels after removing its spacer workaround.

Two independent review/integration findings were corrected before proceeding: a child alignment override initially broke Row centering, and a stretched parent initially expanded implicit grid rows and inflated effective gaps. Tests now cover both configurations.

Verification: 15 Column tests pass, including literal gap, margin, padding, nesting, parent-alignment, horizontal-scroll, and vertical-scroll expectations. Card 11/11 and Scrollable 33/33 regressions pass; resize/reflow and deep-scroll integration checks passed. An independent raw-Taffy probe measures a requested 20-pixel gap as 20 pixels under a stretched 300-pixel parent. The macOS linker emits a pre-existing `__eh_frame` warning.

Visual result: mobile cards have separation; detail sections no longer touch. Correct gaps use more vertical space. Desktop Terminal actions extend beyond Store's explicitly fixed 180-pixel card height; this limitation remains visible because Store code and fixed-height semantics are unchanged. The artwork overflow is also Store-owned and remains outside scope.

Final capture directory: `/Users/xaver/.codex/visualizations/2026/09/15/01a0a50b-c5e5-7e63-bc71-bf4e2b8ac8cd/framework-layout-changes/01-column-gaps-final/`.

Baseline directory: `/Users/xaver/.codex/visualizations/2026/09/15/01a0a50b-c5e5-7e63-bc71-bf4e2b8ac8cd/framework-layout-changes/00-baseline/`.

Captures use a clean detached source snapshot because unrelated tasks changed the shared checkout during verification. Store hashes match the initial manifest; captured framework hashes match the task commits. Tests/captures do not include unrelated unfinished changes.

## Change 2: CardGrid participates in host layout

Commit: `b6ba757ef`.

CardGrid now places children during host layout, and Card has a matching host wrapper node. Buttons receive standard geometry, identities, focus, and input before painting. A shared bounded layout solver supports narrower-than-minimum allocations without rebuilding widget identities. Explicit fixed heights remain fixed; raw custom Taffy hosts use the new helper for that narrow fallback.

Verification: final Card 12/12 and App grid 10/10 tests pass. The broader library run passed 2,966 tests with two ignored before final compatibility cleanup; affected tests were rerun afterward. Independent review approved the change, with minor follow-up coverage for solver rejection/non-settlement.

The unchanged Store probe shows host registration increasing from 7 to 19 focusable widgets and real App focus traversal from 6 to 18. Previously 12 controls had zero rectangles before painting; now none do. Painting previously changed 90 widget rectangles; now it changes none. These counts describe the fixture and do not assert every app in the repository is fixed.

All five Store PNGs are byte-identical to Change 1. This is a lifecycle and keyboard-access improvement, not a visual redesign. Capture directory: `/Users/xaver/.codex/visualizations/2026/09/15/01a0a50b-c5e5-7e63-bc71-bf4e2b8ac8cd/framework-layout-changes/02-card-grid-host/`.

A separate VirtualList post-build synchronization bug was reproduced and assigned to Change 3: retained rows lose node IDs before applying geometry. Ordinary dynamic Card-body geometry is independently tested and passes.

## Remaining

Resize-dependent composition, coherent text scaling, content-aware card patterns, automatic page/form composition, guided overrides, and complete manual composition/inspection remain in progress or planned. Completion of Change 1 does not establish completion of the overall framework goal.
