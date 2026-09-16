### Spec Compliance

- ❌ Issues found. The RichTextLine/base-font correction, shaped multiline measurement, pointer origin refresh, scaled gutter metrics, and numbered-prefix alignment are implemented, but the required CodeEditor wrapper parity and real undo/redo invalidation are incomplete (`ui/src/widget/code_editor.rs:440`, `ui/src/widget/text_input.rs:3538`).
- ⚠️ Cannot verify from diff: controller-owned Notes, Calendar, and Typst captures remain the integration check for ordinary, narrow, and 200% layouts. Per the review context, these are not a worker defect.

### Strengths

- `ui/src/widget/rich_text_line.rs:437` preserves the base font override and resolves only the wrapper row height; the regression at `ui/src/widget/rich_text_line.rs:1015` checks actual shaped size, caret/selection height, and pointer inversion at 1x and 2x.
- `ui/src/widget/text_input.rs:3625` measures unmasked multiline content from a `RichTextLayout` with the same family, size, wrap width, and line-height basis used by drawing. `ui/src/widget/text_input.rs:2283` also refreshes the retained paragraph origin after placement, covering pointer geometry before first draw.
- `ui/src/widget/text_input.rs:1641` measures gutter width from the painted number style, and the CodeEditor regression at `ui/src/widget/code_editor.rs:1938` checks scaled gutter, wrapped selection/caret rows, hit-testing, and pointer-event placement.
- `ui/src/widget/block_editor.rs:3396` ties numbered-prefix placement to the resolved line-height band, while `ui/src/widget/block_editor.rs:5625` checks heading, paragraph, inline-code selection, and numbered-prefix geometry.
- Focused compatibility check: `ui/src/text_render.rs:1377` still converts resolved styles back to the renderer-relative legacy contract by dividing size and letter spacing by the renderer scale; the new gutter measurement uses that boundary correctly.

### Issues

#### Critical (Must Fix)

None.

#### Important (Should Fix)

1. `ui/src/widget/code_editor.rs:440` — `CodeEditor::build_layout` discards `theme`, then `CodeEditor::measure` delegates directly to the inner input at `ui/src/widget/code_editor.rs:460`. The new multiline measurement falls back to `text_renderer.text_scale()` whenever `layout_text_scale` is unset (`ui/src/widget/text_input.rs:3625`), so the required `build_layout(theme 2x) -> measure(renderer 1x) -> draw(theme 2x)` sequence still measures CodeEditor text and gutter at 1x and draws them at 2x. The wrapper test at `ui/src/widget/code_editor.rs:1938` bypasses both build and measure with `set_rect`, so it cannot catch this. It also means a same-instance theme-scale change does not mark the wrapped input measure-dirty through `layout_text_scale`. Forward the layout scale through a dedicated inner method (or otherwise share the TextInput layout-scale capture) and add a CodeEditor wrapper regression that performs build, measure, apply, and draw with a stale renderer, including 2x back to 1x.

2. `ui/src/widget/text_input.rs:3538` — the real document synchronization path assigns scalar/Text-object contents and clears removed values without setting `model_layout_dirty` when the text changes. Once `dirty` has been consumed by `take_dirty` (`ui/src/widget/text_input.rs:2271`), undo, redo, or a remote model update can change wrapping and line count while `layout_needs_measure` remains false (`ui/src/widget/text_input.rs:3916`); draw reshapes the paragraph, but Taffy's cached height is stale. The claimed undo/redo regression calls `set_text` at `ui/src/widget/text_input.rs:7021` and `ui/src/widget/text_input.rs:7036`, which explicitly takes the separate invalidating path at `ui/src/widget/text_input.rs:900`, so it is not equivalent to history replay through `sync`. Compare the adopted model text with the previous value and mark multiline measurement dirty (preferably through one model-adoption helper), then test an actual CRDT text change/undo/redo through TextInput or CodeEditor sync.

#### Minor (Nice to Have)

1. `ui/src/widget/text_input.rs:1584` and `ui/src/widget/text_input.rs:2430` — comments still claim `ensure_shaped_layout` returns a `TextStyle` and gutter width is a pure function of line count/font size. The function now returns `()` and gutter width preferentially uses retained measured state. Update the comments so future geometry changes do not rely on obsolete contracts.

### Assessment

**Task quality:** Needs fixes

**Reasoning:** The core base/resolved-scale and shaped-geometry work is clean and well targeted, but the production CodeEditor lifecycle still misses the authoritative theme scale and real model history changes can bypass height invalidation. Both are explicit Task 2 contracts and can produce measured-versus-painted divergence despite the reported green tests.

**Checks:** No test suites were rerun, per the review instructions and supplied green evidence. Focused out-of-diff checks were limited to two named risks: CodeEditor theme-scale propagation (`ui/src/widget/code_editor.rs:431`) and actual model/undo synchronization invalidation (`ui/src/widget/text_input.rs:3514`, `ui/src/widget/block_editor.rs:3589`).
