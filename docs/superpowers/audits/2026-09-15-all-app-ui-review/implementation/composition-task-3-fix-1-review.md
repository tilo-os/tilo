# Task 3 fix round 1 scoped re-review — Themes disclosure

## Verdicts

- **Specification: PASS.** Both Themes findings from `task-3-review.md` are resolved. The controller reports the separately reviewed Task 5 Store/Settings dependency green, so Task 3 no longer has a pending dependency acceptance item.
- **Code quality: PASS.** Preview geometry now has one owner, view selection uses the shared selected-state control and canonical typed route, and the focused regressions exercise pointer, keyboard, accessibility, Escape/focus return, resize, hidden-pane routing, edit preservation, and short-height reachability. No new scoped breakage was found.

## Finding closure

### Full preview containment and reachable scroll — resolved

- `PreviewPane::SpecimenLayout` owns margins, title height, section positions, chart bounds, and the minimum complete pane height (`apps/themes/src/preview.rs:30-41`, `apps/themes/src/preview.rs:53-154`). `narrow_intrinsic_height` exposes that requirement to the app instead of duplicating section offsets (`apps/themes/src/preview.rs:213-222`).
- The narrow layout uses the preview-owned height (`apps/themes/src/app.rs:893-917`), and `apply_narrow_stack_layout` includes the resulting Taffy height in `scroll_content_h` (`apps/themes/src/app.rs:1128-1169`). Drawing consumes the same `SpecimenLayout` geometry (`apps/themes/src/preview.rs:290-333`).
- The short-height regression proves the pane contains the card, the card contains the final charts, a real scroll extent exists, and maximum wheel scroll brings the chart tail inside the viewport (`apps/themes/src/app.rs:2176-2216`).
- Fresh custom/live preview captures at 480×900 show both chart surfaces inside the enclosing card. The 480×480 and 800×480 maximum-scroll captures show the entire chart row plus the card's bottom padding and edge, confirming the tail is reachable at the requested short-height sizes.

### Visible and accessible selected view state — resolved

- The former plain buttons are now a two-segment `ToggleGroup` bound to the typed `qos.ui.segmented.select` view route (`apps/themes/src/app.rs:183-194`). `set_narrow_preview` synchronizes the selected segment before its same-value early return, so initial editable/live state and capture seeds cannot leave the selector stale (`apps/themes/src/app.rs:839-866`).
- The shared control paints the selected segment with the accent fill and exposes the active label plus checked state on each synthetic toggle-button child (`ui/src/widget/toggle_group.rs:656-712`, `ui/src/widget/toggle_group.rs:908-942`). The fresh 480×900 Customize/custom-preview/live-preview captures show the selected fill clearly.
- Pointer and focused keyboard selection emit the typed operation; Escape switches back to Customize and focuses the compound selector (`apps/themes/src/app.rs:1780-1811`). Hidden preview/customizer routing remains gated by the active node and widget subtree (`apps/themes/src/app.rs:1838-1866`, `apps/themes/src/app.rs:1876-1927`).
- The combined regression checks initial checked state, pointer switch, canonical operation routing, breakpoint persistence, Escape focus return, keyboard re-entry, unsaved edit preservation, and unchanged document heads (`apps/themes/src/app.rs:2218-2326`).
- The follow-up 16px horizontal margin aligns the selector with the narrow content gutter without adding vertical preamble (`apps/themes/src/app.rs:1570-1585`); the focused test asserts its x-origin (`apps/themes/src/app.rs:2123-2145`).

## Verification basis

Reviewed `task-3-fix-1.md`, `task-3-fix-1-report.md`, the authoritative `review-203032223-7231a8170.md` package, current lines only for the two named findings, and all seven fresh Theme captures under `.superpowers/app-ui-2026-09-15/task3-fix-1-20260916/themes/`. Per instruction, reported suites were not rerun. The report records 52 passing Themes tests and a passing scoped library Clippy run. Repository-wide all-target strict Clippy remains blocked by unrelated concurrent Chromium warnings and does not alter this scoped quality verdict. No production files were changed during review.
