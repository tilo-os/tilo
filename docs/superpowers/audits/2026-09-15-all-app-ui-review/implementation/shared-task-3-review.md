### Spec Compliance

- ✅ Spec compliant for the scoped commit `fba9ea50f0e0547237ac4c3d7151c04b5e6ec145` and supplied one-line Scratchpad patch. The builder is opt-in, documents explicit-height preservation, supports either builder order, and avoids duplicate wrappers (`ui/src/widget/scrollable.rs:232–256`). Only the wrapper's own minimum style changes; Scrollable retains an inherited viewport decision without changing child styles or topology (`ui/src/widget/scrollable.rs:980–1023`, `ui/src/widget/scrollable.rs:1200–1215`).
- ✅ Default compact fields, explicit child heights, retained-tree convergence, short/long/short transitions, lower and repeated clicks, first-wheel outer scrolling, and pane/scrollbar focus have behavioral coverage (`ui/tests/scrollable_test.rs:75`, `:98`, `:107`, `:125`, `:158`, `:174`, `:244`, `:269`, `:281`). The shaped multiline ceiling is confined to intrinsic measurement; single-line and externally applied layout paths remain outside it (`ui/src/widget/text_input.rs:3624–3639`, `:3683–3686`).
- ✅ The supplied app integration adds only the requested builder call (`apps/scratchpad/src/lib.rs:121`). The saved real-app harness uses existing action registration, Save, isolated profile reopening, Unicode input, a single downward wheel, and byte-for-byte persistence checks (`.superpowers/sdd/2026-09-15-measured-ui-components/task-3-scratchpad-check.rs:24–119`).
- ⚠️ Controller's independent baseline-matched PNG inspection is outside this diff review; complete that visual gate. The reported trailing-newline whole-row discrepancy is explicitly an unresolved separate investigation, not represented as fixed by this patch (`.superpowers/sdd/2026-09-15-measured-ui-components/task-3-report.md:125`).

### Strengths

- The presentational wrapper preserves real child traversal and pointer capture, delegates the ordinary container event/layout/presence lifecycle, and does not introduce an accessible control (`ui/src/widget/scrollable.rs:969–1105`). The retained-tree regression compares the complete child style, exercising the actual shared solver rather than only fresh-layout snapshots (`ui/tests/scrollable_test.rs:174–241`).
- Both interaction fixes are tied to recorded behavioral failures: fractional inner overflow consumed the first wheel and repeated clicks displaced editor focus. The final implementations address the source of each failure without changing wheel-consumption policy (`ui/src/widget/text_input.rs:3683–3686`, `ui/src/widget/scrollable.rs:1467–1486`; RED evidence in `task-3-logs/task3-fractional-red4.log:14–29` and `task3-focus-red.log:8–11`).
- Focused out-of-diff check — risk: the wrapper could suppress action, commit, accessibility, redraw, or presence propagation. Checked Container delegation and Widget traversal defaults (`ui/src/widget/container.rs:164–194`, `:316–405`, `:421–468`; `ui/src/widget/mod.rs:855`, `:1037–1085`, `:1603–1634`). The real child remains traversable and the captured accessible editor retains its role and focusability (`task-3-captures-final/narrow-empty.json:212–247`). No missing forwarding found.
- Focused out-of-diff check — risk: `focus_target.is_none()` could confuse current focus with a newly requested target. The Driver creates an empty request for each event and only replaces focus when a target is returned (`ui/src/headless.rs:2006–2027`), consistent with the new deferred fallback.
- Expanded only the cut-off changed-function context needed to judge dispatch ordering and measurement scope (`ui/src/widget/scrollable.rs:1329–1487`, `ui/src/widget/text_input.rs:3592–3703`). No production edits, git-state changes, subagents, or test reruns performed.
- Read the existing final logs: 12 integration, 34 Scrollable, 99 TextInput, 23 CodeEditor, 7 Scratchpad tests passed; the four-layout capture harness passed, and `cargo check -p ui` succeeded (`task-3-logs/task3-final-integration.log:19`, `task3-existing-ui-final.log:46`, `task3-text-input-final.log:103`, `task3-code-editor-final.log:27`, `task3-existing-app-final.log:11`, `task3-captures-final.log:9`, `task3-check-final.log:2`). Log paths in this report are relative to `.superpowers/sdd/2026-09-15-measured-ui-components/`.

### Issues

#### Critical (Must Fix)

- None found.

#### Important (Should Fix)

- None found.

#### Minor (Nice to Have)

- Existing validation noise: `task-3-logs/task3-existing-ui-final.log:2` contains the macOS linker warning that `__eh_frame` exceeds compact-unwind encoding capacity. The tests pass; no evidence ties it to this patch. Track the test-binary linker configuration separately so validation output becomes clean.

### Assessment

**Task quality:** Approved.

**Reasoning:** The change follows the owned-node measured-layout contract and retains compact/manual sizing behavior. Real event regressions, retained-tree settlement checks, and recorded app persistence runs support the implementation; no task-blocking defect was found.
