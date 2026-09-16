# Task 11 independent review

## Spec Compliance — FAIL

- ❌ The actual-visible-text measurement requirement is incomplete: parameter rows measure unconstrained `Ag`, while keys paint with a wrapping width (`apps/api_tester/src/app.rs:1957`); saved request/history labels likewise measure without the width supplied to paint (`apps/api_tester/src/navrow.rs:522`, `:540`, `:566`, `:584`). See Important finding below.
- ✅ Scope and ownership match the brief: the ordered sidebar inventory controls visible children and placement, hidden maintenance rectangles are cleared, and local commands preserve document heads (`apps/api_tester/src/app.rs:1189`, `:1251`, `:4005`). Shared ToggleGroup normalization is narrowly within the controller's explicit authorization (`ui/src/widget/toggle_group.rs:702`). `responsive.rs` needs no mandatory edit because existing rail setters are consumed in `app.rs:3151`.
- ⚠️ Saved-request content adoption remains expressly outside this gate. The report and staged capture harness accurately distinguish it from selection, editing an already displayed Body, and Send (`task-11-report.md:57`; `apps/api_tester/src/app.rs:4038`). No successful ordinary saved-request-to-bound-draft loading claim is accepted.

## Strengths

- Explicit `SidebarItem` identities avoid filtered positional association mistakes; the same inventory supplies children and placement, and removed controls get empty rectangles (`apps/api_tester/src/app.rs:1189`, `:1251`). The disclosure regression verifies removal and Escape focus return through the host (`apps/api_tester/tests/a11y.rs:312`).
- Manual height is independent of effective short-window geometry. Tests exercise actual pointer drag, arrow adjustment, Reset, Body/Auth changes and restoration after shrinking (`apps/api_tester/tests/a11y.rs:336`, `:418`; `apps/api_tester/src/app.rs:3972`).
- Native child operation forwarding restores the existing Button command path without introducing document mutation alternatives (`apps/api_tester/src/app.rs:3575`, `:3608`). Real form interactions and loopback HTTP Send tests check resulting behavior (`apps/api_tester/tests/a11y.rs:459`, `:563`).
- Shared ToggleGroup uses the same normalized style for measurement and paint; its regression checks actual glyph-run font sizes at 1× and 2× (`ui/src/widget/toggle_group.rs:702`, `:1063`).

## Issues

### Critical

- None found in the bounded diff.

### Important — must fix for this task

- **[P2] Measure wrapped content using its paint width and allocate matching row geometry.** `apps/api_tester/src/app.rs:1957` derives every Params/Headers row from `measure_style("Ag", None, ...)`; `automatic_request_height` at `:1320` consequently reserves `count × uniform row_height`. The key drawing path supplies a constrained column width. Similarly, `apps/api_tester/src/navrow.rs:522` and `:566` measure a label without width, then draw it with bounded widths at `:540` and `:584`. `TextRenderer` wraps constrained text, rather than ellipsizing it (`ui/src/text_render.rs:1545`, `:1911`). A long query/header key or saved request/history name therefore paints multiple lines into a one-line row, crossing separators or the following clickable row; automatic request height also undercounts the content. This violates the brief's explicit same-font/weight/width and actual visible text metrics requirements. Compute the painted row measurements at the actual column/label width and share those row heights with total height, placement, paging/hit targets and painting. If a single-line navigation design is intended, explicitly truncate/clip the painted label within its row while retaining its full accessible name. Add focused long-key and long-saved-name cases at narrow 1× and true 2× that assert text remains within its owning row and navigation still invokes the displayed item. Existing geometry tests use short demo strings and only target rectangles, so they do not detect this failure.

### Minor

- **[P3] Vertically center Params/Headers check indicators in measured rows.** `apps/api_tester/src/app.rs:1966` and `:2030` keep a 16 px indicator at `y + 4`, while measured rows and labels expand. Independent inspection of `task11-api-tester/captures/params-response-800x900-2x.png` confirms the roughly 12 px center mismatch identified by the controller. Center the indicator and text from the same measured row geometry, including the placeholder. This is visible polish rather than lost interaction because these rows remain read-only.
- **[P3] Shared test output contains an acknowledged linker warning.** `task11-api-tester/logs/toggle-green.log:3` reports `__eh_frame section too large`, with possible exception-handling performance impact. It does not invalidate the 13 passing tests or justify expanding this task into linker work; retain it as an existing warning rather than describing all validation as pristine.

## Checks and limits

- Read the supplied owned diff in chunks (the first tool response truncated and the omitted segment was recovered), the brief, report, controller visual review and final test logs. No git commands, production edits, subagents, suite reruns or new captures.
- **Named risk: wrapping contract.** Checked `ui/src/text_render.rs:1545`, `:1761`, `:1911`: bounded paint shapes wrapped text and measurement supports the same width. The long-string failure follows from the source; no additional long-string runtime fixture was executed.
- **Named risk: child operation ownership and completion after disclosure changes.** Checked `ui/src/widget/button.rs:1201` and the default lifecycle adapter in `ui/src/widget/mod.rs:1471`, `:1530`. Button operations validate their control ID and complete synchronously; forwarding through the current visible inventory does not leave these management operations waiting for a hidden child. Existing app Send lifecycle remains separate.
- **Named risk: hidden focus and action/geometry correspondence.** Traced changed inventory, stale-rectangle clearing, ChildSig, Escape mapping, drawer pointer gate and operation routing in the supplied diff; checked the real-host regressions at `apps/api_tester/tests/a11y.rs:312`, `:459`, `:493`. No blocking issue found in those changed paths. This is source/headless evidence, not a live OS accessibility audit.
- **Named risk: enlarged row visual alignment.** Independently inspected only the final Params-with-response 800×900 2× PNG. Relied on the controller's documented individual inspection of the other 20 final PNGs and source/hash verification; did not repeat that capture gate.
- Logs substantiate 67 passing library tests with one ignored capture harness, 13 passing a11y tests, and 13 passing shared ToggleGroup tests (`task11-api-tester/logs/final-tests.log`, `task11-api-tester/logs/toggle-green.log`). The shared warning above is the observed validation noise. No additional test execution was necessary to establish the width mismatch.

## Assessment

**Task quality: Needs fixes.** Interaction ownership, disclosure inventory and preference restoration are sound within the reviewed scope. Correct the measurement/paint-width mismatch before accepting the composition task; the checkbox alignment is a smaller visual follow-up and the saved-request loading defect remains the separately authorized follow-up.
