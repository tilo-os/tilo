### Finding Verdicts

- **Canonical Toggle Bottom Panel retains Terminal focus on keyboard close** — ADDRESSED. `Command::ToggleBottomPanel` now routes every close through `close_bottom_panel`, which hides the dock, clears `terminal_focused`, updates the disclosure state, and leaves the selected tab and Terminal object untouched (`apps/code_editor/src/app.rs:2677`, `apps/code_editor/src/app.rs:2708`). Ctrl+J detects the closing transition and assigns the retained trigger as `focus_target`, while Escape uses the same close helper and focus target (`apps/code_editor/src/app.rs:3878`, `apps/code_editor/src/app.rs:4651`). The new retained-driver regression focuses the real Terminal content, closes and reopens with Ctrl+J, and proves Enter remains with the focused disclosure rather than returning to Terminal; the preservation test also checks the live Terminal slot and grid content across close/reopen (`apps/code_editor/src/app.rs:5554`, `apps/code_editor/src/app.rs:5713`). The appended report records this exact regression passing.
- **Controller contract lacks a same-instance resize-while-open regression** — ADDRESSED. The existing retained-trigger driver now keeps the disclosure open while resizing the same app from 480×900 to 800×700 and back, checking panel containment and fixed height, trigger/tab alignment and separation, open semantics, and panel presence after restoration (`apps/code_editor/src/app.rs:5683`, `apps/code_editor/src/app.rs:5705`). The appended report records the extended exact test passing.

### New Breakage in the Fix Diff

- None. The fix centralizes close cleanup without changing open routes, panel selection, Terminal construction, paint, or layout behavior; the five named focused tests passed individually according to `.superpowers/sdd/2026-09-15-app-composition/task-10-report.md:80`.

### Out-of-Scope Observations

- None. The previously documented macOS `/var` versus `/private/var` package-test caveat remains outside this fix round and was not re-opened.

### Verdict

**Fix round:** All findings addressed, no new Critical/Important breakage.
