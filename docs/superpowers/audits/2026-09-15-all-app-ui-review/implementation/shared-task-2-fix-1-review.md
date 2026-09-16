### Spec Compliance

- ✅ Spec compliant for fix round 1. Both Important findings and both stale comments from `task-2-review.md` are addressed, with no new fix-diff defect found.
- ⚠️ Not assessed in this scoped re-review: controller-owned Notes, Calendar, and Typst captures. The review context records their remaining app issues under Shared Task 6, Text Geometry 23, and Composition 6 rather than Task 2.

### Original Finding Verdicts

1. **ADDRESSED — CodeEditor authoritative-scale lifecycle.** `ui/src/widget/code_editor.rs:446` now captures `theme.text_scale` before the wrapper leaf is measured, using the same helper as direct TextInput layout at `ui/src/widget/text_input.rs:489` and `ui/src/widget/text_input.rs:3596`. The regression at `ui/src/widget/code_editor.rs:2035` exercises the real wrapper build→measure→apply→draw path with a renderer held at 1x, verifies measured/drawn height, gutter, and shaped font at 2x, then verifies same-instance invalidation and return to 1x.

2. **ADDRESSED — real undo/redo model invalidation.** `ui/src/widget/text_input.rs:479` centralizes model adoption and marks changed multiline values measure-dirty without setting the user-edit signal. Scalar, Automerge Text, and removal paths use it at `ui/src/widget/text_input.rs:3563`, `ui/src/widget/text_input.rs:3568`, and `ui/src/widget/text_input.rs:3576`; direct bound text retains its existing reconciliation `dirty` behavior at `ui/src/widget/text_input.rs:468`. The replacement regression at `ui/src/widget/text_input.rs:7021` uses a real document Text plus State commit/undo/redo, consumes the prior measurement, asserts `take_dirty() == None`, and requires fresh geometry after replacement, undo, and redo.

3. **ADDRESSED — shaped-layout comment.** `ui/src/widget/text_input.rs:1603` now describes the actual rebuild inputs and no longer claims the unit-returning helper returns a `TextStyle`.

4. **ADDRESSED — gutter-width comment.** `ui/src/widget/text_input.rs:2448` now distinguishes retained measured glyph width from the pre-measure estimate.

### Strengths

- The fix shares small internal boundaries rather than duplicating scale or model-adoption logic.
- `adopt_model_text` preserves single-line behavior by setting measurement invalidation only for multiline inputs, and preserves unchanged-value behavior by returning early.
- The regressions reproduce the exact production lifecycle gaps identified in the first review rather than testing surrogates.
- Scope remains limited to `ui/src/widget/code_editor.rs`, `ui/src/widget/text_input.rs`, and the task report; no app, Scrollable, or global renderer contract changed.

### New Issues in Fix Diff

#### Critical (Must Fix)

None.

#### Important (Should Fix)

None.

#### Minor (Nice to Have)

None.

### Assessment

**Task quality:** Approved

**Reasoning:** The fix closes both measured-versus-painted lifecycle gaps at their shared boundaries, preserves compatibility and dirty/action semantics, and adds focused regressions that directly exercise the previously missing paths. No new correctness or maintainability issue is evident in the scoped diff.

**Checks:** No tests were rerun, per the re-review instructions. Supplied final-source evidence reports the two exact regressions passing, CodeEditor 23/23, TextInput 99/99, `cargo check -p ui`, rustfmt, and `git diff --check` passing; the known macOS `__eh_frame` linker warning remains outside this fix.
