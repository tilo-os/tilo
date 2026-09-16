# Final review — Shared Measured UI Tasks 1–7

## Assessment

**Spec compliance: PASS. Shared-plan quality: Approved with minor lint debt. Ready to integrate this shared scope: Yes.**

No Critical or Important functional defect was found in the owned cumulative changes. The normalization, shaping, intrinsic-height and overflow contracts compose consistently, and the retained focused evidence covers their material interactions. This verdict does not close the separate all-app implementation/acceptance plan, certify a clean whole-workspace suite, or authorize a push/merge.

## Scope and method

Reviewed the authoritative `final-owned-review.md` in bounded passes: commits `cec5c10c7`, `93b56dc1f`, `17110d403`, `3f13fb4eb`, `83c269dac`, `fba9ea50f`, `1b3eac1b4`, `c7b2d576b`, `fd7b0964f`, and `73dde03c6`. Requirements were the measured-components plan, its referenced app-balance spec, and the recorded scope rulings. Read production hunks, relevant surrounding final source, behavioral regressions, task reports and previous review/fix verdicts. The diff from `73dde03c6` to current source was empty for the reviewed shared component/test paths; unrelated concurrent app changes were excluded. Source/index/HEAD remained untouched; this report is the only write.

No test suites were rerun merely to duplicate retained evidence. No new uncovered functional risk required a focused execution.

## Strengths and integration checks

- **Scale boundaries and compatibility:** `TextRenderer::renderer_input_style` converts resolved size and letter spacing once (`ui/src/text_render.rs:1377`); Text deliberately retains its legacy renderer-relative letter spacing. EmptyState and Toolbar use normalized styles for measurement and paint, while RichTextLine passes base sizes to TextInput. CodeEditor's theme-scale propagation survives a stale renderer and a return to 1×.
- **Measurement → drawing → scrolling:** unmasked multiline measurement uses shaped geometry and rounds intrinsic height upward (`ui/src/widget/text_input.rs:3748`, `:3790`); Task 7 repairs attributed final-LF geometry at its shaping boundary (`ui/src/richtext.rs:361`). These fixes preserve the first-wheel behavior established by Task 3 rather than masking excess internal scroll. Attributed single-line measurement uses the same styled authority later painted.
- **Manual dimensions/defaults:** the viewport minimum is opt-in, lives on an owned wrapper, and does not rewrite the child's style (`ui/src/widget/scrollable.rs:248`, `:1010`, `:1200`). Card changes restore automatic block minima while preserving zero minimum width and fixed rows (`ui/src/widget/card.rs:180`, `:421`; `ui/src/widget/card_composition.rs:123`). Explicit 72px content, 180px grid rows, fixed buttons and short default multiline inputs have compatibility coverage.
- **Text interactions:** complete mark runs, cache signatures and IME range remapping feed one shaped layout. Chips precede selection/glyphs; rules follow glyphs. Current-frame link hits consume shaped selection bands, including reversed ranges and unrelated gaps (`ui/src/widget/rich_text_line.rs:357`; `ui/src/widget/text_input.rs:880`, `:4476`; `ui/src/widget/block_editor.rs:1560`). Model sync/undo/redo and style-only changes have observable geometry tests.
- **Overflow interactions:** measured extents drive Toolbar fitting and placement; hidden commands retain original callback indices through the shared bounded ContextMenu. Reviewed resize reconciliation, scrolling, Escape/outside dismissal, pointer release ownership, emitted accessibility bounds, and the three accepted fix regressions. ContextMenu legacy point placement remains separately supported.
- **Host keyboard route:** the extracted Enter/Space fallback preserves the original live handler behavior; host regressions close the earlier semantic-activation-only gap for scrolled card/EmptyState actions.

## Issues

### Critical

None found.

### Important

None found within the shared-plan scope.

### Minor

1. **Some retained Clippy debt belongs to this cumulative plan.** The optional lint log is not wholly unrelated baseline: it identifies new owned code at `ui/src/widget/text_input.rs:3755` (narrowing cast), `:3779` (eager `map_or` fallback), `ui/src/widget/scrollable.rs:1085` (literal return lifetime), `ui/src/widget/toolbar.rs:1`, `:452`, `:687` (documentation, guarded remainder, method length), and `ui/src/widget/empty_state.rs:634` (narrowing cast). Additional nonfatal warnings flag owned helper code. See `.superpowers/app-ui-2026-09-15/task6-after-20260916/verification/task6-clippy.log:1111` onward. These are nonfunctional/style diagnostics, and optional Clippy was not a plan acceptance gate, so this does not block shared acceptance. Apply narrow cleanups or justified local allowances in a follow-up; describe these as shared-plan lint debt. The “untouched/unrelated” wording in `docs/superpowers/audits/2026-09-15-all-app-ui-review/implementation/shared-task-6-report.md:65` is valid only relative to Task 6, not the whole plan.

## Verification evidence and remaining limits

- Independently read the final retained result lines: TextInput **105**, RichTextLine **29**, BlockEditor **72**, richtext **20**, rich_text_editor **16**, CodeEditor **23** — **265 passed, zero failures** — plus the successful scoped compiler check. Logs are under `.superpowers/app-ui-2026-09-15/task6-after-20260916/verification/`.
- Earlier accepted gates cover Card/ContentCard/Column, resolved text/EmptyState and real host activation, Scrollable's **12 integration + 34 module** tests, and Toolbar's **18 tests**, ContextMenu **15**, layout **7**. Prior Task 2 and Task 4 findings were repaired and independently re-reviewed. These counts describe the recorded task gates, not one fresh cumulative suite.
- Controller-owned visual acceptance records cover Store/Settings, VPN, editor captures, Scratchpad, Toolbar fixtures and final Notes interaction routes. Read the Notes final rebuild correspondence: six standard and twelve interaction PNGs were byte-identical to the accepted captures, with final binary/source provenance. I did not independently rerender or reinspect every image in this final source review.
- Broad testing is **not entirely green**. Task 5 records three isolated-baseline integration failures: `bound_component_dispatch::legacy_bound_mutable_admission_edits_only_its_record_and_refuses_missing_or_busy`, `design_token_audit::app_and_shell_theme_escape_hatches_do_not_grow`, and `typography_visual_regression::multilingual_gallery_matches_scale_matrix` (`shared-task-5-report.md:46`). Task 1's broad run stopped after a concurrent Chromium library failure and did not reach integration binaries (`shared-task-1-report.md:49`). Those historical results do not establish current full-tree status.
- The macOS compact-unwind linker warning remains disclosed. Optional Clippy ends with **100 errors/12 warnings**, mixing outside-scope debt and the minor owned diagnostics above. Neither clean lint nor warning-free builds is claimed.
- Live OS accessibility behavior, Linux verification, and the final all-app route/UX matrix remain separate acceptance evidence. Complete those in their owning work; do not reopen all shared suites solely to repeat their recorded passes.

## Recommendation

Accept Tasks 1–7 as the shared foundation, retain the lint follow-up with correct ownership, and continue the separate app work and final matrix. Keep the documented broad-suite failures distinct from this shared functional verdict.
