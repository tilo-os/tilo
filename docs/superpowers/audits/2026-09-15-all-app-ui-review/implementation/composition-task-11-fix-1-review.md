# Task 11 fix round 1 — scoped re-review

## Finding Verdicts

- **P2: wrapped content measured without its paint width — ADDRESSED.** `apps/api_tester/src/app.rs:1918` shapes the actual key/value strings at their actual column widths and retains both glyph layouts and cell/row rectangles. Automatic height consumes the retained table height (`:1363`); drawing paints those exact layouts inside cell and row clips (`:2067`). Params, Headers and Form share this path, including Form's corrected vertical offset. Navigation explicitly elides to one line, measures and paints the same result without wrapping, and clips to its own cell (`apps/api_tester/src/navrow.rs:445`, `:479`). Full accessibility names and callbacks are preserved. The added regressions check wrapped geometry, glyph containment, manual preference, full names and displayed-item activation at narrow 1× and true 2× (`apps/api_tester/src/app.rs:4067`, `:4180`, `:4253`; `apps/api_tester/src/navrow.rs:685`).
- **P3: checkbox/placeholder center mismatch — ADDRESSED.** `KvRowLayout::checkbox` derives its center from the owning measured row (`apps/api_tester/src/app.rs:282`); key and value blocks use the same center in `measure_kv_table` (`:1918`). This covers enabled, disabled and placeholder rows. The geometry assertions at `:4180` and independent inspection of the final default Params 2× and long Headers 1× PNGs confirm the repair without reduced font size.
- **P3: acknowledged shared linker warning — ADDRESSED as the required retention of the limitation.** The fix report explicitly retains the original `__eh_frame section too large` warning and makes no linker-repair claim (`.superpowers/sdd/2026-09-15-app-composition/task-11-report.md`, Fix round 1 / RED-GREEN section). The underlying warning remains an authorized, non-blocking limitation; shared production code was not changed in this fix.

## Authorized Lifetime Dependency

- **NavRow reclamation boundary — ACCEPTED.** `rebuild_nav` moves each replaced Vec into `retired_nav_rows`, preserving the allocation addressed by the host's old raw pointers (`apps/api_tester/src/app.rs:1121`). It explicitly marks replacement dirty even when row counts and visible signatures are unchanged (`:1126`, `:3670`). Retired generations are excluded from the current inventory and reclaimed at the beginning of the next `build_layout` (`:3123`).
- **Named risk check: use-after-free during host focus capture.** Independently checked `ui/src/headless.rs:1043`: focus capture occurs before update/sync, and both binding/node maps are cleared before root `build_layout`. Independently checked `ui/src/app.rs:6026` and `:6082`: initialization and structural rebuild clear both maps before the same call; structural rebuild captures preceding focus state first. These boundaries support the app-local reclamation point. The same-count regression verifies allocation/name survival across replacements and reclamation on rebuild (`apps/api_tester/src/app.rs:4102`).
- **Named risk check: measured geometry versus painted rich-text scaling.** Checked `ui/src/richtext.rs:317` and `ui/src/text_render.rs:2161`: `shape_styled` consumes resolved metrics and `draw_rich` consumes the shaped runs without a second text-scale/wrapping pass. This matches the new retained row design and its 1×/2× evidence.

## New Breakage in the Fix Diff

- **None found.** The changed row geometry, single-line navigation rendering and authorized lifetime repair introduce no identified Critical/Important issue within this scoped re-review. The observed intermediate unused-import warning is removed by the final diff, and the final affected-test log contains no compiler warning.

## Checks and Evidence Limits

- Read the supplied fix diff once in consecutive chunks, the fix brief and appended report. No git commands, production edits, new agents, suite reruns or new captures.
- Read `task11-fix1/logs/red.log` and `green.log`: the original two containment regressions fail before the fix, then the four long-content regressions pass.
- Read `task11-fix1/logs/lifetime-focused.log`, `lifetime-pointer.log`, `lifetime-paging.log` and the supplied crash stacks: the same-count lifetime regression and both formerly crashing host routes pass after repair. The intermediate pointer log's unused import is disclosed above, not hidden.
- Read `task11-fix1/logs/final-tests.log`: **72 library tests passed, two capture tests ignored; all 13 a11y tests passed**. No additional test run was needed to answer a remaining concrete risk.
- Independently inspected `task11-fix1/captures/params-response-800x900-2x.png`, `long-headers-480x900-1x.png`, and `long-navigation-800x900-2x.png`: ordinary rows remain compact, long table text fits its row, indicators align, and navigation elides within its target. Relied on the controller's individual inspection of the other final captures and verification of source/evidence hashes and normal binary `6cd8cb6db12c2cd1cceef4950391724ef040c333a82f3c08fbadfaf53cde206b`; did not repeat that complete gate.
- The lifetime conclusion is supported by both host code paths and headless regressions; it is not a claim of a sanitizer run or live OS accessibility audit. Long-content fixtures cover the stated cases, not every possible Unicode/font input.

## Out-of-Scope Observations

- **Existing saved-draft adoption gap remains open and non-blocking for this fix round.** The new tests honestly distinguish displayed saved-row selection/history callback identity from full draft adoption (`apps/api_tester/src/app.rs:4253`). Staged capture fixtures do not claim to repair or verify that separate transition.

## Verdict

**Fix round: All required findings addressed, no new Critical/Important breakage.**

**Task spec: PASS within the controller's bounded scope. Task quality: Approved.** The existing linker warning and separately tracked saved-draft adoption issue remain explicitly open.
