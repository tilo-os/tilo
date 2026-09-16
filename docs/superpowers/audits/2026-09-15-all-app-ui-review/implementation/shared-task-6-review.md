### Spec Compliance

- ✅ Spec compliant for the scoped change: complete UTF-8 runs enter the inner attributed layout before layout/draw (`ui/src/widget/rich_text_line.rs:356`, `ui/src/widget/rich_text_line.rs:464`); styled shaping replaces plain shaping in the existing authority (`ui/src/widget/text_input.rs:4476`); the wrapper's duplicate glyph pass is removed (`ui/src/widget/rich_text_line.rs:522`).
- ✅ Mark, model, theme-scale, and style invalidation is present (`ui/src/widget/rich_text_line.rs:269`, `ui/src/widget/rich_text_line.rs:457`, `ui/src/widget/text_input.rs:490`, `ui/src/widget/text_input.rs:880`). The shared IME insertion path and UTF-8 splice correction preserve complete characters (`ui/src/widget/text_input.rs:835`, `ui/src/widget/rich_text_line.rs:707`).
- ✅ Link routing now consumes current-frame selection bands with the existing full-row vertical target (`ui/src/widget/block_editor.rs:1561`, `ui/src/widget/block_editor.rs:3941`); regressions exercise first draw, style/scale changes, RTL and an unrelated mixed-direction gap (`ui/src/widget/block_editor.rs:5623`).
- ⚠️ Visual acceptance and final binary correspondence are controller-owned evidence, not independently established by this source diff. The controller inspection and recapture record is present (`.superpowers/app-ui-2026-09-15/task6-controller-visual.md:3`); final interaction output records successful 1×/2× caret, selection and actual page navigation (`.superpowers/app-ui-2026-09-15/task6-after-20260916/interaction-run.log:2`). Whole-task shared invariants outside the three owned production files remain outside this scoped gate.

### Strengths

- One shaped source serves painting and interaction; the new tests assert observable glyph count, weight, changed advance, pointer inversion and scroll geometry rather than only checking attributes (`ui/src/widget/rich_text_line.rs:857`, `ui/src/widget/rich_text_line.rs:865`, `ui/src/widget/text_input.rs:5372`).
- Chips precede selection and glyphs, while link rules follow glyphs, avoiding the previous selection-erasing overlay (`ui/src/widget/text_input.rs:3862`, `ui/src/widget/text_input.rs:3890`, `ui/src/widget/text_input.rs:4581`).
- Preedit coverage exercises real IME events beside multibyte text, and model coverage uses actual sync/undo/redo (`ui/src/widget/text_input.rs:5423`, `ui/src/widget/text_input.rs:5591`).

### Issues

#### Critical (Must Fix)

- None found.

#### Important (Should Fix)

- None found.

#### Minor (Nice to Have)

- Validation output is not pristine: the test linker emits the compact-unwind warning (`.superpowers/app-ui-2026-09-15/task6-after-20260916/verification/task6-text-input-suite.log:3`), and optional clippy ends with 100 errors/12 warnings (`.superpowers/app-ui-2026-09-15/task6-after-20260916/verification/task6-clippy.log:1341`). The report discloses these; they do not demonstrate a defect introduced by this scoped diff. Track the existing linker/lint baseline separately rather than claiming a clean lint run.

### Checks

- Read the supplied diff once in three sequential slices; no git commands, production/index/HEAD edits, or test reruns. Changed-file helper reads were limited to functions cut off by diff context.
- Named risk—IME insertion and cached geometry: checked the existing signature, span insertion, byte mapping/selection and final draw helpers (`ui/src/widget/text_input.rs:734`, `ui/src/widget/text_input.rs:835`, `ui/src/widget/text_input.rs:1286`, `ui/src/widget/text_input.rs:4644`). Position is included in the signature and rich glyph paint consumes `single_layout`.
- Named risk—range rules and Task 7 normalization: checked existing styled shaping and decoration consumers (`ui/src/richtext.rs:317`, `ui/src/richtext.rs:816`, `ui/src/text_render.rs:1284`, `ui/src/text_render.rs:2102`). Final-empty-line normalization remains intact; underline/strike paint derives bounds from shaped selection bands.
- Named risk—new wrapper measurement reporting: checked the shared measurement callback and app measurement decision (`ui/src/widget/layout.rs:97`, `ui/src/app.rs:7001`, `ui/src/widget/mod.rs:1679`). No separate measurement authority was introduced.
- Inspected retained module-result lines: TextInput 105, RichTextLine 29, BlockEditor 72, richtext 20, rich_text_editor 16, code_editor 23; all zero failures (respectively `verification/task6-text-input-suite.log:118`, `task6-rich-line-suite.log:33`, `task6-block-editor-suite-final.log:84`, `task6-richtext-suite.log:24`, `task6-rich-editor-suite.log:20`, `task6-code-editor-suite.log:27`, under `.superpowers/app-ui-2026-09-15/task6-after-20260916/`). Scoped compiler check completed (`verification/task6-check.log:3` in that directory).

### Assessment

**Task quality: Approved.**

**Reasoning:** The repair consolidates styling, paint and geometry without creating another editor or changing unrelated production scope. Focused behavior tests and retained verification evidence cover the requested regression routes; no blocking defect was found in this scoped review.
