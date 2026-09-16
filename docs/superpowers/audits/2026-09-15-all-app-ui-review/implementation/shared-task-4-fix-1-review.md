### Finding Verdicts

- **Export actual synthetic accessibility bounds — ADDRESSED.** `ui/src/widget/toolbar.rs:860` exports visible command/overflow bounds, `ui/src/widget/toolbar.rs:887` exports the clipped menu-row bounds, and `ui/src/widget/toolbar.rs:898` exports the popup bounds. These are AccessNode fields consumed by the previously checked AccessKit export path, rather than only fallback-click geometry. Scrolled-out rows export zero area while remaining available to semantic focus; focus reveals them and updates bounds. `review_emitted_accessibility_bounds_match_paint_and_scrolling` (`ui/src/widget/toolbar.rs:1691`) checks those emitted fields, including partial clipping, offscreen zero area, and focus-to-reveal.
- **Refresh pointer hover around popup routing — ADDRESSED.** `ui/src/widget/toolbar.rs:700` records the logical pointer before popup dispatch. `refresh_hover` (`ui/src/widget/toolbar.rs:399`) derives the target from that position and current geometry, clears it while the menu is open, and is called after closing, layout, and immediately before arming/activating a left click (`ui/src/widget/toolbar.rs:393`, `ui/src/widget/toolbar.rs:308`, `ui/src/widget/toolbar.rs:736`, `ui/src/widget/toolbar.rs:749`). Opening/closing also clears the armed target. `review_popup_close_rehits_pointer_before_second_click` (`ui/src/widget/toolbar.rs:1736`) reproduces both keyboard-open stale-command and pointer-open stale-overflow cases, with and without intervening layout; its callback and closed-menu assertions cover the original incorrect activation.
- **Match release suppression to the mouse button — ADDRESSED.** The popup branch now arms suppression only for `MouseButton::Left` (`ui/src/widget/toolbar.rs:718`), matching its existing left-release clearing branch. `review_nonleft_popup_press_does_not_capture_next_sibling_click` (`ui/src/widget/toolbar.rs:1784`) dispatches right and middle press/release followed by Escape through an actual Row, asserts capture has ended, then dispatches a sibling click and asserts its callback fires once.

### New Breakage in the Fix Diff

- None identified. The fix changes pointer bookkeeping and semantic geometry without changing measured placement, popup painting, callback ownership, or ContextMenu behavior.

### Out-of-Scope Observations

- None.

### Checks and Evidence

- Read `task-4-fix-1.md`, the appended fix report in `task-4-report.md`, and only the scoped fix package `review-task4-fix1-scoped-c7b2d576b.md` (effective base `c66ec996c`, head `c7b2d576b`). The original fix base `2aff2e6c7` and non-overlapping intervening commit are controller-provided facts. The unscoped package was not inspected.
- Compared each reported resolution against its production hunk and regression. The report names the three `review_` regressions and records their exact failures before the fix: missing bounds, wrong callback counts, and stuck capture. Reported focused results are RED 0 passed/3 failed, then GREEN 3 passed.
- Reported amended-code validation: Toolbar 18 passed/1 existing manual fixture ignored; context-menu filter 15 passed; layout 7 passed; library check, scoped formatting, and diff whitespace checks passed. The known baseline macOS `__eh_frame` warning remains; no new warning is reported.
- No tests, git commands, or additional external-source checks were needed for this scoped verification. Only this requested review report was written; no production/index/branch changes or subagents.
- The controller's accepted fixture gate carries forward because this fix does not change typography, layout, or popup paint. Live OS accessibility/focus remains outside the established evidence; this review adds no live-platform claim.

### Verdict

**Fix round:** All findings addressed, no new Critical/Important breakage.

**Final spec verdict:** ✅ Spec compliant within the Task 4 review scope and stated evidence limits; all three previously blocking gaps are resolved.

**Final task quality:** Approved.
