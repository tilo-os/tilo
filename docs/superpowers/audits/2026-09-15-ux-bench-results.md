# Desktop UX bench: first measured results

## Result

35 native shell scenario/variant runs: **6 passed, 29 failed required UX assertions, 0 execution errors**. Report: `target/ux-bench/final-matrix/index.html`. Each run records its executable hash and scenario in `run.json`; these results are a snapshot of this dirty development worktree, not a released build.

| Journey | Evidence | Interpretation |
| --- | --- | --- |
| Navigation geometry | All six variants passed: baseline, compact, light, high contrast, Retina input scale, 200% text | Authored topbar spacing/target checks pass; this does not establish whole-screen quality. |
| Contrast | Workspace name 9.71:1; shortcut hint 3.44:1 against a 4.5:1 requirement | Hint fails the explicit rendered-pixel probe. |
| Find/open app | No exposed “Open an app” target; Files palette route did not establish a Files outcome after seven key activations | Launch discoverability and outcome need work. |
| Browse Files | Seeded list is visibly populated; row semantics absent; click/Enter did not establish the requested document tab | Keep semantic coverage and the failed outcome visible; do not infer dispatch success from the screenshot. |
| Scroll Files | List pixels unchanged after a 60-logical-pixel gesture; header stayed fixed | No content displacement established. Reproduces in all four eligible variants. |
| Workspaces | New workspace count reached two; return step left active workspace “Untitled” | Returning Home failed. Overview controls lack required semantic targets. |
| Rename workspace | Two clicks/eight key activations renamed to “Bench”; saved/reopened names remain “Bench” | State outcome passes; overall journey fails because settings panel lacks required semantic visibility. |

## Bench and toolkit work

The bench uses existing headless input actions and exports the shell permission-filtered action catalogue. Each checkpoint retains native state, resources, inspection data, PNGs and expected/observed checks. HTML provides target highlights and route counts. Input failures, assertion failures and execution errors remain distinct.

Shared Scrollable fixes normalize physical pixel delta by display scale, give inner scroll regions first opportunity to consume the wheel, and stop clipped inner content from falsely inflating a parent's scroll extent. Regression tests establish these shared-widget changes. The hosted Files journey still fails, so these fixes do not establish that desktop scrolling is repaired.

Independent review found and corrected false-positive risks: absence inferred from missing geometry, a scrolling-container false pass without a stationary anchor, mismatched capture bounds, and raw coordinates reused across incompatible layouts. Matrix skips are explicit coverage gaps, not passes.

## Verification and limits

Verification passed: 14 bench tests, 33 Scrollable tests, 5 native shell tests and 3 Python suite tests (55 total). Standard Clippy encounters unrelated worktree lint failures in `ui/src/widget/layout.rs` and `runtime-fs/src/operations.rs`; the bench target passed with only the affected lint classes allowed on the command line (`doc-markdown`, `implicit-hasher`, `too-many-lines`). This is not a claim that the unrestricted repository lint check is green.

A concurrent app layout test could not compile because it moved a field out of an App implementing Drop; a minimal `mem::replace` correction allowed the UI test harness to compile without changing its intended assertions. A concurrent runtime-fs edit also required removing a redundant mutable reborrow to restore compilation.

Live hardware event delivery, macOS gesture arbitration, animation smoothness and subjective usability remain unverified. Visibility checks declared clipping/hidden state, not sibling occlusion. Contrast probes cover two baseline pixel pairs. Compact/large-text coordinate journeys and non-baseline contrast require separately authored probes. These limitations appear in the reports and usage documentation.

Final rebuilt-binary smoke run: navigation geometry passes and Files scroll retains its expected failure, with no execution errors (`target/ux-bench/final-smoke/index.html`). All 35 matrix runs retain executable identity, action catalogue and complete checkpoint evidence. Report JavaScript syntax and local HTTP delivery were also checked.
