# Rich mark rendering diagnosis

## Conclusion

The doubled/misaligned `bold` in the Notes captures is caused by painting the same glyphs twice from two independently shaped layouts.

- `RichTextLine::draw` first asks its inner `TextInput` to paint the complete line as regular text (`ui/src/widget/rich_text_line.rs:474-477`).
- It then calls `paint_mark_segment` for each marked range (`:525-526`). That helper shapes and paints the marked substring again with `TextRenderer::draw_text_styled` (`:728-763`).
- The overlay starts at an x position taken from the inner input's **regular** shaped layout (`:717-724`). Bold and italic glyphs do not necessarily have the same advances as regular glyphs, so the first glyph can start correctly while later glyphs drift. The regular glyph remains visible underneath throughout. This is why the last letters of `bold` split visibly in `scale-baseline.png`, and why the error is larger in `large-text.png`.

The problem is not text scale itself. Scaling makes an existing shaping/compositing error easier to see.

This path also means the visual text and interaction geometry disagree: `TextInput` derives caret, selection, pointer hit-testing, scrolling, and `char_x_positions` from its plain `single_layout`, while `RichTextLine` paints a second styled layout above it. Link lookup ultimately consumes the byte returned by that plain geometry. Hiding the lower glyphs would conceal the screenshot symptom without repairing those semantics.

## Evidence and working reference

`TextInput` already has the correct single-source geometry architecture for unmasked single-line text:

- `build_single_line_metrics` creates `single_layout` (`ui/src/widget/text_input.rs:4312-4386`).
- `draw_single_line_text` paints that same layout with `draw_rich` (`:4477-4482`).
- caret, selection, and hit-testing read that layout (`:1083-1191`, plus the shaped hit path).

The missing piece is attributed input for that single-line layout. It currently calls `RichTextLayout::shape_plain` at `ui/src/widget/text_input.rs:4350`, so `RichTextLine` cannot put its marks into the authoritative layout.

The existing rich-text API is sufficient:

- `RichTextLayout::shape_styled` accepts complete `RichSpan` runs and underline/strike ranges (`ui/src/richtext.rs:312-390`).
- `RichSpan` already carries bold, italic, family, color, and background (`ui/src/richtext.rs:87-141`).
- `TextRenderer::draw_rich` paints backgrounds first, glyphs once, then decorations (`ui/src/text_render.rs:2100-2117`).
- `RichTextEditor::shape` is an in-tree working example that converts `marks::segments_for` into `RichSpan`s and underline/strike ranges, then shapes once (`ui/src/widget/rich_text_editor.rs:502-544`).
- `TextInput::highlighted_display_spans` and `ensure_shaped_layout` already solve attributed-span caching and IME preedit insertion for multiline code (`ui/src/widget/text_input.rs:727-823`, `:1584-1626`). That machinery is internal and currently tied to syntax highlighting; there is no public single-line style-span setter to use unchanged.

## Smallest sound fix

Keep `TextInput` as the editor and geometry owner, but let its unmasked single-line path receive attributed spans from `RichTextLine`.

1. Add a narrow `TextInput` API/state for a complete set of single-line `RichSpan`s plus underline/strike ranges. Validate that concatenating span text equals the committed display text. Generalize the existing preedit insertion helper so IME text is inserted into those spans before shaping.
2. In `build_single_line_metrics`, call `RichTextLayout::shape_styled` when attributed spans are present; retain `shape_plain` for normal fields. Store the result in the existing `single_layout` and continue populating `char_x_positions` from its caret stops. No parallel layout should remain.
3. In `RichTextLine::draw`, convert `marks::segments_for` to complete spans before calling `inner.draw`:
   - bold/italic map directly to `RichSpan` flags;
   - links and code set the accent color;
   - underline/strike ranges go into the shaped layout;
   - code can use `RichSpan::family = Some(FontFamily::Mono)` once the product wants the documented monospace behavior. The authoritative layout makes that safe, unlike the current forced `mono = false` workaround at `rich_text_line.rs:750-751`.
4. Remove marked-glyph painting from `paint_mark_segment`. Any remaining decoration must use rectangles from the authoritative layout (`selection_rects`), not start/end positions from a different font. A code-chip background is an intentional pre-glyph decoration; represent it as a rich-layout background or a small pre-glyph decoration range in `TextInput`. Link/underline/strike rules can be emitted by `draw_rich` or from the same layout rectangles.
5. Include all style inputs in layout invalidation. `TextInput::attributed_signature` currently hashes only color/bold/italic (`text_input.rs:714-724`) because syntax highlighting needs only those. Reuse for rich marks must also hash family, size, background, baseline, underline/strike ranges, and any code-chip decoration data used by the layout.

This keeps paint, caret, selection, scrolling, pointer-to-byte conversion, peer presence, and link hit-testing on one shaped buffer. It also avoids an opaque background used as an eraser. Filling the base surface over the regular word and repainting bold would break selection/presence backgrounds, transparent surfaces, and theme-dependent composition while leaving regular-layout hit geometry in place.

For the smallest behavior-only patch, preserve the current sans treatment of code spans while moving all glyphs into the one layout. Restoring the documented monospace code face is then a separate, now-safe product choice. If the existing rounded/padded chip appearance must remain exact in the same patch, add a pre-glyph ranged decoration using `single_layout.selection_rects`; do not keep the post-paint mask/overpaint scheme.

## Focused regression tests

1. **One glyph paint:** draw `aBz` with only `B` bold through `RichTextLine`; inspect `Scene::encoding().resources`. The scene must contain three glyphs total, with shaped weights regular/bold/regular. The current implementation emits the three regular glyphs plus a second bold `B`.
2. **Style-only invalidation:** draw a line, add/remove a bold mark without changing its text, and draw again. The stored layout/run weights must change each time; this catches signatures that hash text but omit mark style.
3. **Authoritative geometry at 1x and 2x:** use `Owner in bold.` with the exact bold range. After draw, assert the end-of-mark caret x, selection right edge, `pixel_x_for_byte`, and the authoritative layout caret stop are identical. Assert `byte_for_pixel_x` maps points immediately inside/outside the styled range to the expected bytes.
4. **Link hit geometry:** combine bold + link on a width-changing string such as `WWW`; click near the styled right edge and assert the resolved byte remains inside the link, while a point beyond it does not. This verifies actual painted advances feed `link_at_byte`/`mark_at_byte` routing.
5. **IME and UTF-8:** insert preedit text adjacent to a marked multi-byte grapheme and assert span concatenation, caret, and selection remain on valid byte/grapheme boundaries.
6. **Decorations:** verify code background and link/underline/strike bounds equal `single_layout.selection_rects` for their ranges and are painted in the intended layer order (background, glyph, rule). If code switches to monospace, assert the following unmarked caret begins at the mono run's true end.
7. **Visual acceptance:** recapture the Notes baseline and large-text fixtures. `bold` must have one crisp set of glyphs; the link and code sample must retain color/decorations; selection/caret/link clicks should be exercised at both scales.

No production source was changed and no test suite was run for this read-only diagnosis.
