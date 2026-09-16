### Spec Compliance

- ❌ Issues found: the fresh dock default, retained trigger, canonical pointer/keyboard/assistive action routes, Problems/Terminal selection, explicit-open retention through an empty diagnostics refresh, fixed 160px dock, and requested narrow captures are present (`apps/code_editor/src/app.rs:906`, `apps/code_editor/src/app.rs:2677`, `apps/code_editor/src/app.rs:3575`, `apps/code_editor/src/app.rs:3638`, `apps/code_editor/src/app.rs:5484`, `apps/code_editor/src/app.rs:5500`, `apps/code_editor/src/app.rs:5547`; `.superpowers/sdd/2026-09-15-app-composition/task-10-report.md:53`). The disclosure lifecycle is incomplete because closing a focused Terminal with the existing Toggle Bottom Panel command leaves hidden terminal focus behind (`apps/code_editor/src/app.rs:2677`, `apps/code_editor/src/app.rs:3932`), and the mandated resize-while-open regression is absent from the single-viewport interaction test (`apps/code_editor/src/app.rs:5560`).

### Strengths

- The dock is absent from both layout and retained child enumeration when collapsed, and `panel_node` is explicitly cleared, so the fresh editor reclaims the dock height without leaving a hidden panel child (`apps/code_editor/src/app.rs:3638`, `apps/code_editor/src/app.rs:3644`, `apps/code_editor/src/app.rs:4766`).
- The new trigger delegates emission, availability, and invocation draining to the shared action-bound `Button`; its binding targets the existing `command-toggle-bottom-panel` catalogue action rather than creating a second mutation path (`apps/code_editor/src/app.rs:110`, `apps/code_editor/src/app.rs:306`, `apps/code_editor/src/app.rs:351`).
- Disclosure semantics are explicit and stateful, with dynamic Show/Hide naming and `ToggleButton` checked/expanded state; the test exercises pointer, Enter, Ctrl+J, Escape, focus return, and assistive activation (`apps/code_editor/src/app.rs:282`, `apps/code_editor/src/app.rs:5580`).
- Empty diagnostics replacement only updates panel contents and analysis, so it does not revoke an explicit Problems open (`apps/code_editor/src/app.rs:1469`, `apps/code_editor/src/app.rs:5500`).
- Escape ordering is scoped after palette, close/conflict prompts, overlay-sidebar handling, and the find bar, then returns focus to the retained trigger (`apps/code_editor/src/app.rs:4560`, `apps/code_editor/src/app.rs:4617`).
- Focused outside check, canonical-action risk: the constructor binding and the live resource inventory use the same stable `project_id`, so later changes to file/folder snapshot fields do not orphan this action address (`apps/code_editor/src/app.rs:351`, `apps/code_editor/src/app.rs:1647`).
- Focused outside check, child-lifecycle risk: hidden panels are removed from layout, event routing, and child enumeration, while the always-visible trigger remains in all three (`apps/code_editor/src/app.rs:3638`, `apps/code_editor/src/app.rs:4026`, `apps/code_editor/src/app.rs:4766`).
- Focused outside check, narrow-geometry risk: the 480px regression asserts the trigger shares the tab row and is disjoint from the tab bar, and the recorded 480×900/800×700 inspections place it inside the trailing 40px (`apps/code_editor/src/app.rs:5560`, `.superpowers/sdd/2026-09-15-app-composition/task-10-report.md:53`).

### Issues

#### Critical (Must Fix)

- None.

#### Important (Should Fix)

- `apps/code_editor/src/app.rs:2677`: `Command::ToggleBottomPanel` only flips `panel_visible`. If the Terminal owns keyboard focus, Ctrl+J is handled before terminal routing, closes the dock, and leaves `terminal_focused` true (`apps/code_editor/src/app.rs:3916`, `apps/code_editor/src/app.rs:4590`). A second Ctrl+J reopens Terminal with that stale focus, so the next key is routed into the terminal even though focus was hidden during the close. Escape correctly clears this flag and returns focus (`apps/code_editor/src/app.rs:4633`), which makes the inconsistency explicit. Centralize the close transition so every close clears terminal/panel focus; for keyboard-triggered closure, return focus to `panel_toggle_node`. Add a regression that focuses Terminal, closes and reopens through Ctrl+J, and verifies hidden focus was removed while Terminal contents remain intact.

#### Minor (Nice to Have)

- `apps/code_editor/src/app.rs:5560`: the disclosure interaction test starts and remains at 480×900. Separate fresh app captures at two sizes establish static geometry, but they do not satisfy the controller contract's resize-while-open path. Resize the same open driver between the required narrow/standard sizes and assert the panel remains open, its state/contents survive, and the trigger/panel remain inside their owning panes.
- `.superpowers/sdd/2026-09-15-app-composition/task-10-report.md:30`: the package test run is not clean: 206 of 207 tests passed. The `/var` versus `/private/var` failure is plausibly unrelated to Task 10, but its pre-existing status is inferred rather than reproduced (`.superpowers/sdd/2026-09-15-app-composition/task-10-report.md:32`). Keep this as an explicit verification caveat until the path-alias test is fixed or independently baselined; the focused Task 10 regressions and build did pass.

### Assessment

**Task quality:** Needs fixes

**Reasoning:** The composition and action wiring are otherwise strong, but the canonical keyboard close path can preserve focus in hidden Terminal content, violating the disclosure lifecycle requirement. The live-resize test gap and non-pristine package run are secondary verification issues.
