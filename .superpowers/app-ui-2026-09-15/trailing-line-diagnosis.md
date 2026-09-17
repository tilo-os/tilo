# Trailing empty-line diagnosis

## Result

Confirmed at runtime. A string ending in LF is represented differently by
`RichTextLayout::shape_plain` and `RichTextLayout::shape_styled`. The plain
path retains the empty logical line after the LF. The styled path drops its
cosmic-text visual run, even though `RichTextLayout`'s own byte/line map still
contains that final line.

This is separate from the fractional-height first-wheel and repeated-focus
work in Scrollable.

## Minimal fixture and runtime proof

Minimal input:

```text
x\n
```

Style: 16px font, 1.25 line-height (20px), width 300px.

Observed through the public `RichTextLayout` API:

| Geometry | `shape_plain` | `shape_styled` |
|---|---:|---:|
| height | 40px | 20px |
| EOF caret `(x, top, height)` | `(0, 20, 20)` | `(0, 0, 20)` |
| EOF logical line | 1 | 1 |
| EOF visual line | 1 | 0 |
| EOF logical-line bands | `[(20, 20)]` | `[]` |
| hit below content | byte 2 (EOF) | byte 1 (before LF) |

Controls behave correctly: `"x"` is 20px in both paths and `"x\ny"` is
40px in both. The mismatch is specifically the empty line after a terminal
LF. `"\n"` likewise measures 40px plain and 20px styled.

The long fixture already used by `ui/tests/scrollable_test.rs`,
`"A line of the document.\n".repeat(80)`, is 1620px plain (81 rows) and
1600px styled (80 rows), before TextInput's 16px vertical padding. The
reported 1079px versus 1056.75px observation is the same one-line loss at
that fixture's effective line height.

Proof artifacts:

- Source: `.superpowers/app-ui-2026-09-15/trailing-line-probe.rs`
- Source SHA-256: `95c2760eb5e044e31a0cbfc9c2b7d9fe27fe1e0bfdd3f45384562ee26cf47b57`
- Binary: `.superpowers/app-ui-2026-09-15/trailing-line-probe-ui-537f77485e6f899f`
- Binary SHA-256: `e3ce7dd3b40fa445a7228d667db58962e5888f6e8a13aca2ce15700d7256a1f6`
- UI dependency: `target/debug/deps/libui-537f77485e6f899f.rlib`
- UI rlib SHA-256 at the final probe compile:
  `abf42a1f332c81d5549eaf72ae4c62c1ad3f420801109cf70d4bc333f6047b37`
- cosmic-text rlib resolved alongside it:
  `target/debug/deps/libcosmic_text-4d1adcb6605e3653.rlib`
- cosmic-text rlib SHA-256: `1d957b3f134185cf9d38770b65cc9f7c8fa0ed4bb9a0ca02357d057b6c8892e5`
- Repository HEAD at probe time:
  `fba9ea50f0e0547237ac4c3d7151c04b5e6ec145`
- Compiler: `rustc 1.97.1 (8bab26f4f 2026-07-14)`

Build command (direct `rustc`; no Cargo suite was run):

```sh
rustc --edition=2024 \
  .superpowers/app-ui-2026-09-15/trailing-line-probe.rs \
  -L dependency=target/debug/deps \
  --extern ui=target/debug/deps/libui-537f77485e6f899f.rlib \
  -o .superpowers/app-ui-2026-09-15/trailing-line-probe-ui-537f77485e6f899f
```

## Source cause

TextInput first measures an unmasked multiline value with
`RichTextLayout::shape_plain` (`ui/src/widget/text_input.rs:3652-3664`) and
uses that layout's height plus padding (`:3669-3686`). On draw,
`ensure_shaped_layout` converts the same display bytes to attributed spans
and replaces the cached layout with `shape_styled`
(`ui/src/widget/text_input.rs:1615-1644`). `highlighted_display_spans`
preserves the terminal LF; the loss occurs below TextInput.

The two richtext entry points use different cosmic-text setters:

- `shape_plain` calls `Buffer::set_text`
  (`ui/src/richtext.rs:219-252`).
- `shape_styled` calls `Buffer::set_rich_text`
  (`ui/src/richtext.rs:317-370`).

In cosmic-text 0.19.0, `set_text_impl` iterates `LineIter`, then explicitly
adds an empty `LineEnding::None` line when the preceding line has an ending
(`cosmic-text-0.19.0/src/buffer.rs:865-922`).

`set_rich_text_impl` instead derives line ranges from `BidiParagraphs`
(`buffer.rs:949-979`). `BidiParagraphs` intentionally does not emit a final
paragraph when the text ends in LF
(`cosmic-text-0.19.0/src/bidi_para.rs:18-44`). The rich setter has an
empty-whole-input fallback, but no equivalent to `set_text_impl`'s terminal
empty-line insertion (`buffer.rs:995-1076`). It therefore creates only the
line before the terminal LF.

Meanwhile `RichTextLayout::line_map` always pushes a start and zero length
for the text after every LF (`ui/src/richtext.rs:201-213`). The styled layout
therefore contains metadata for logical line 1 but no cosmic-text run for
line 1. `height` counts runs (`:466-472`), and `caret_geometry_at` falls back
to `(0, 0, line_height)` when it cannot find a run (`:543-561`). This explains
both the one-row height loss and the EOF caret at the top.

## User-visible effect

For a multiline TextInput whose value ends in LF:

- measurement reserves the correct final row, but drawing reports content
  one line shorter;
- the caret at EOF is drawn on the first row instead of the final empty row;
- clicking in the reserved final row maps to the byte before the LF rather
  than EOF;
- current-line highlighting and line-number origins have no band/origin for
  the final logical line;
- scrolling code that follows the EOF caret receives `top = 0`, so a
  caret move to EOF can pull an internally scrolling editor toward the top.

Selection rectangles are not a plain-versus-styled height divergence here.
For `"x\n"`, both paths return the glyph band for selecting `[0, 2)` and no
positive-width rectangle for selecting only the LF `[1, 2)`. The styled-path
bug still makes the selection's EOF caret/endpoint and hit-testing wrong.
If a visible marker for selected hard breaks is desired, that is a separate
existing shaped-selection behavior and should not be bundled into this fix.

## Smallest appropriate follow-up

Fix the compatibility boundary once in `RichTextLayout::shape_styled`, not
in Scrollable or only in TextInput. After `Buffer::set_rich_text` and before
shaping, ensure a terminal LF has a final empty `BufferLine` with
`LineEnding::None`, the final effective run's metrics (falling back to base
attrs), and the requested alignment. This mirrors `set_text_impl` while
preserving attributed layouts for highlighted editors. An upstream
cosmic-text fix would be preferable long term; the local normalization can
be removed after upgrading to a version whose rich setter preserves terminal
empty lines.

Add focused regressions:

1. In `ui/src/richtext.rs`, assert that plain and styled `"x\n"` have equal
   two-line height, EOF caret top, visual-line index, logical-line band, and
   below-content hit result. Include `"x"`, `"x\ny"`, `"\n"`, and split
   rich spans as controls.
2. In TextInput, measure then draw a trailing-LF multiline fixture and assert
   that post-draw `content_height` matches intrinsic measurement within only
   the documented pixel-ceiling difference, EOF caret remains on the last
   row, and clicking that row returns `text.len()`.

Changing TextInput measurement to use the styled path would only hide the
measure/draw delta while retaining the incorrect missing row, caret, hit,
gutter, and current-line geometry. The final empty run must exist.

No production source, production test, commit, or broad Cargo suite was
changed/run during this diagnosis.
