# Task 4 report: coherent Text scaling and shared capture configuration

## Status

DONE. `Text` now distinguishes theme-resolved, renderer-relative, and fixed logical font sizes without changing the low-level renderer contract or Store source.

## Implementation

- Theme role styles and `Text::with_scaled_size` convert their resolved logical font size using the actual `TextRenderer::text_scale()`. Theme 2× with renderer 1× and theme 2× with renderer 2× both emit the intended 28px glyph for a 14px base.
- Measurement and drawing call the same renderer-input conversion, including the `TextLayoutEngine` path.
- `Text` observes retained renderer scale during measure and draw. A change invalidates layout and the cached glyph scene, including a draw reached before another measurement.
- Added `Text::with_fixed_size(size)`, an exact logical font-size API. Font size does not follow theme or renderer scaling. Letter spacing remains renderer-relative.
- Preserved default `Text`, `Text::with_size`, and `Text::with_style` as renderer-relative. Their font size and letter spacing continue to follow `TextRenderer`.
- Documented `TextLayoutEngine`'s renderer-relative style contract. Delegating engines pass the provided style to `TextRenderer` unchanged.
- Kept direct custom-canvas `TextRenderer` calls unchanged and covered scales 1, 1.25, 1.5, and 2.
- Reused `FrameHarness::new_with_theme`. It already synchronizes renderer and theme, so no duplicate capture pipeline or edit to dirty `headless.rs` was needed.

## Ownership and modifier precedence

| API | Font-size owner | Letter-spacing owner |
| --- | --- | --- |
| Default `Text`, `with_size`, `with_style` | Renderer-relative input; `TextRenderer` applies its scale | `TextRenderer` |
| Semantic role, `with_scaled_size` | `Text` resolves the final theme-scaled logical size, then compensates for renderer scale | `TextRenderer` |
| `with_fixed_size` | `Text` preserves the exact final logical size | `TextRenderer` |
| Direct custom-canvas renderer call | `TextRenderer` | `TextRenderer` |

Later size modifiers take ownership. Tests cover role → fixed, fixed → role, fixed ↔ scaled, fixed ↔ base size, and fixed ↔ complete style. A visual role overridden by an explicit size retains its accessibility semantics, matching the established contract.

## TDD evidence

### RED: mismatched theme and renderer

Command:

```text
cargo test -p ui theme_resolved_sizes_are_independent_of_renderer_scale -- --nocapture
```

Relevant failure before implementation:

```text
role mismatch measure: expected 19.6, got 39.2
test widget::text::tests::theme_resolved_sizes_are_independent_of_renderer_scale ... FAILED
```

This was expected: theme 1× with renderer 2× exposed the inverse mismatch; the table also included the reported theme 2× / renderer 1× case.

### GREEN: mismatched theme and renderer

Same command after converting with the actual renderer scale:

```text
running 1 test
test widget::text::tests::theme_resolved_sizes_are_independent_of_renderer_scale ... ok
test result: ok. 1 passed; 0 failed
```

### RED: explicit fixed logical size

Command:

```text
cargo test -p ui fixed_size_is_independent_of_theme_and_renderer_scales -- --nocapture
```

Relevant failure before the API existed:

```text
error[E0599]: no method named `with_fixed_size` found for struct `widget::text::Text`
error: could not compile `ui` (lib test) due to 1 previous error
```

### GREEN: explicit fixed logical size

```text
running 1 test
test widget::text::tests::fixed_size_is_independent_of_theme_and_renderer_scales ... ok
test result: ok. 1 passed; 0 failed
```

### RED: retained glyph cache after renderer-scale change

Command:

```text
cargo test -p ui renderer_scale_change_invalidates_a_retained_base_size_cache -- --nocapture
```

Relevant failure before renderer observation:

```text
assertion failed: Widget::needs_redraw(&text)
test widget::text::tests::renderer_scale_change_invalidates_a_retained_base_size_cache ... FAILED
```

The old cached scene remained at 18px after changing the renderer to 2× while fresh measurement used 36px metrics.

### GREEN: retained glyph cache after renderer-scale change

```text
running 1 test
test widget::text::tests::renderer_scale_change_invalidates_a_retained_base_size_cache ... ok
test result: ok. 1 passed; 0 failed
```

## Final verification

```text
cargo test -p ui widget::text::tests:: -- --nocapture
running 30 tests
test result: ok. 30 passed; 0 failed; 0 ignored

cargo test -p ui custom_canvas_styles_remain_renderer_relative_at_supported_scales -- --nocapture
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored

rustfmt --edition 2024 --check ui/src/widget/text.rs ui/src/text_render.rs
git diff --check -- ui/src/widget/text.rs ui/src/text_render.rs
```

Both formatting checks exited 0 with no output. Test linking emits the documented pre-existing macOS `__eh_frame section too large` warning; no related code changed.

The controller-owned isolated glyph probe also passed: theme 2× role/scaled text emitted 28px with renderer 1× and renderer 2×; measured heights were 39.2px for Body and 35px for explicit 14px scaled text. Existing renderer-relative base text emitted 14px at renderer 1× and 28px at renderer 2×.

## Capture boundary and limitations

The raw Store capture deliberately keeps its app-local renderer at 1× while constructing a 2× theme. This change makes Store's unchanged framework `Text` role/scaled content genuinely larger under that mismatch. Non-`Text` controls and custom canvas code remain renderer-relative and depend on the capture host configuring `TextRenderer` coherently. That is a separate configuration boundary; `FrameHarness::new_with_theme` is the shared configured entrypoint for faithful framework previews and captures.

No file under `apps/store` was edited. The controller owns the unchanged Store normal/large screenshots and source-hash verification.

## Files changed

- `ui/src/widget/text.rs`
- `ui/src/text_render.rs`
- `.superpowers/sdd/2026-09-15-framework-layout-improvements/task-4-report.md`

The pre-existing unrelated edits in `ui/src/headless.rs`, `ui/src/widget/mod.rs`, and the rest of the shared working tree were preserved.

## Self-review

- Confirmed measure and draw use the identical ownership conversion.
- Confirmed renderer-scale invalidation happens in both paths and the cache test exercises draw before remeasurement.
- Confirmed plugin delegation sees renderer-relative input and produces intended glyph sizes for role, base-style, and fixed-size cases.
- Confirmed all four required scales and matching/mismatching configurations are covered.
- Confirmed later size modifiers clear or replace prior ownership flags.
- Confirmed direct custom-canvas scaling remains unchanged.
- Confirmed no Store, headless, widget module, or app source changed for this task.

No unresolved implementation concerns.
