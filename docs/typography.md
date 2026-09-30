# Typography

Tilo text has one central rule: measurement and painting must use the same
`TextStyle`. A style contains the font size, line-height multiplier, weight,
italic state, and family. Passing one value through the whole layout pipeline
prevents wrapping, clipping, and baseline drift.

The product font policy is deterministic: Noto Sans is the UI face, Noto Serif
is the reading serif, and Noto Sans Mono is the code face. The renderer pins
all three generic families to those names, while the OS image includes Noto
fallbacks for symbols, emoji, CJK, Arabic, Hebrew, Devanagari, Bengali, and
Thai. Application code should use semantic roles or generic `FontChoice`
variants rather than repeating these family names.

## Application text

Prefer a semantic role from the active theme:

```rust
use ui::tokens::TextRole;
use ui::widget::Text;

let body = Text::new("A readable paragraph").with_role(TextRole::Body);
let heading = Text::new("Documents").with_role(TextRole::Heading);
```

The available roles are `Caption`, `Label`, `Body`, `Reading`, `Title`,
`Heading`, `Display`, and `Code`. Roles resolve size, leading, weight, style,
and family together. A retained `Text` resolves its role again during layout,
so changing the active theme or text/UI scale updates it without rebuilding
the application widget tree. Hyphenation, OpenType, alignment, wrapping, and
fragmentation modifiers compose with a role. An explicit core typography
modifier such as `with_size`, `with_weight`, `with_font_family`, or
`with_style` opts out of live role styling and keeps that text custom without
discarding its semantic intent.

`Title` and `Display` expose level-1 headings to accessibility tools;
`Heading` exposes level 2. Nested sections can retain the visual role while
setting their document outline explicitly:

```rust
let subsection = Text::new("Keyboard shortcuts")
    .with_role(TextRole::Heading)
    .with_heading_level(3);
```

Body, label, caption, reading, and code roles remain static text. This makes
screen-reader heading navigation match the visible hierarchy by default.

Use an explicit style only when text has a specific typographic requirement that
does not match a role:

```rust
use ui::{FontChoice, TextStyle, TextWeight};

let pull_quote = Text::new("Make the important thing unmistakable.")
    .with_style(
        TextStyle::new(22.0)
            .with_line_height(1.35)
            .with_weight(TextWeight::Semibold)
            .with_italic(true)
            .with_family(FontChoice::Serif),
    );
```

`Text::with_size` and `TextRenderer::draw_text` remain available for existing
code. New code should prefer complete styles.

Every application crate is kept behind this public boundary by
`tools/policy/check_app_text_contract.py`, which runs in the fast lint workflow and
the pre-commit hook. Applications may compose `Text`, the shared input/editor
widgets, `RichTextLayout`, or `TextRenderer` for a custom-painted
surface. They may not depend on the private shaping crates, instantiate a
second font system, emit raw Vello glyphs, or repeat the product font-family
names. This keeps fallback, OpenType features, variable axes, wrapping and
interaction geometry in the one implementation even when an application needs
canvas-like chrome instead of a widget tree.

## OpenType and paragraph controls

`TextStyle` carries advanced typography through the same measurement, shaping,
painting, hit-testing, and cache path as its basic metrics. Feature tags use
four-byte [`OpenTypeTag`] values, so common controls have convenient builders
without closing the API to a font's less common features:

```rust
use ui::{HangingPunctuation, NumericStyle, OpenTypeTag, OpticalSizing,
         ParagraphAlign, TextStyle, TextWrapMode};

let figures = TextStyle::new(16.0)
    .with_kerning(true)
    .with_discretionary_ligatures(true)
    .with_small_caps(true)
    .with_numeric_style(NumericStyle::TabularOldstyle)
    .with_font_feature(OpenTypeTag::new(*b"ss03"), 1);

let display = TextStyle::new(42.0)
    .with_tracking(0.025) // em; with_letter_spacing accepts logical pixels
    .with_font_variation(OpenTypeTag::WEIGHT, 675.0)
    .with_font_variation(OpenTypeTag::WIDTH, 92.0)
    .with_optical_sizing(OpticalSizing::Auto);

let paragraph = TextStyle::new(18.0)
    .with_align(ParagraphAlign::Justified)
    .with_wrap_mode(TextWrapMode::Balance)
    .with_hanging_punctuation(HangingPunctuation::BOTH)
    .with_fragmentation(2, 2);
```

Explicit feature settings are passed to OpenType substitution and positioning;
this includes disabling defaults such as kerning with `with_kerning(false)`.
Variable axes are expressed in their design-space units. `wght` also selects
the matching shaped font instance, while all axes are normalized against the
chosen face and passed to the outline renderer. Automatic optical sizing sets
`opsz` to the logical font size when the face provides that axis; `None` leaves
the face at its default, and `Value` chooses an explicit design size.

Full justification expands eligible rows but leaves the paragraph's final row
ragged. Balanced wrapping retains the greedy line count and chooses a tighter
measure to avoid a conspicuously short final line. Hanging punctuation applies
optical half-width protrusion to supported opening and closing punctuation and
the shaped caret geometry follows the ink. Paginated and multi-column
containers call `RichTextLayout::fragment_line_count` with their remaining
line capacity; it returns zero when placing the paragraph would violate its
configured widow or orphan minimum.

## Custom-painted surfaces

Custom widgets must reuse the exact same style for both calls:

```rust
let style = theme.tokens.typography.style(TextRole::Label);
let size = text_renderer.measure_style(label, Some(width), &style);
text_renderer.draw_style(
    scene,
    label,
    color,
    transform,
    Some(width),
    &style,
);
```

Do not independently reconstruct font size, leading, weight, or family in the
draw path. `TextRenderer` caches measurement and shaped glyph geometry using
the complete style.

## Editing contract

Caret movement and character deletion use Unicode extended grapheme clusters.
Combining sequences, emoji modifiers, flags, and emoji ZWJ sequences therefore
behave as single user-perceived characters. Document/storage offsets remain
UTF-8 byte offsets at the widget boundary and Automerge character offsets at
the CRDT boundary.

`RichTextLayout` is the authoritative shaped paragraph. It translates
cosmic-text's per-line cursors into global UTF-8 offsets and supplies painting,
wrapping, click hit-testing, caret geometry, visual Home/End and Left/Right
movement, and disjoint selection bands for mixed bidirectional text. Ordinary
single- and multiline `TextInput` fields, syntax-highlighted `CodeEditor`
instances, and `RichTextEditor` use that same layout, so ligatures and RTL text
cannot disagree between ink and interaction. Code-editor gutters and
current-line bands are overlays derived from the shaped paragraph's logical and
visual line geometry; they do not maintain a competing row model.

Attributed custom text should use `RichTextLayout::shape_styled`. It accepts
`RichSpan`s plus one complete block `TextStyle`, keeping leading, named font
families, wrapping, token colours, and interaction geometry together. Layouts
are retained until display text, width, style, attributes, or IME composition
changes, so blinking and caret-only movement do not reshape a paragraph.

Text widgets also publish their shaped caret rectangle to the application host.
The host enables the platform IME only for a focused text widget and positions
the candidate window at that rectangle. Preedit is inserted into the display
layout, underlined, and kept out of the document, CRDT, clipboard, and
accessibility value until a single atomic commit arrives. Composite controls
that embed a `TextInput`—including code editors, prompts, autocomplete, numeric
fields, editable labels, search, rich-text lines, and colour entry—forward that
same candidate rectangle rather than losing IME support at the wrapper boundary.
The lightweight history `NameField` follows the same contract without exposing
the full selection API: it retains an unwrapped shaped layout, clips and scrolls
the ink to keep the visual end caret visible in both LTR and RTL names, deletes
extended grapheme clusters, and keeps underlined preedit display-only until
commit.

Transient search and command surfaces should keep their application-owned
query plus a `QueryComposition`. The helper retains one unwrapped
`RichTextLayout` for ink, selection, BiDi-aware caret motion, horizontal
scrolling, preedit underline, and the platform candidate rectangle. Filtering
must run only after `QueryEventChange::Edited`; `Display` changes repaint the
composition without exposing uncommitted text to search state. Its shared
event editor also owns selection replacement, clipboard shortcuts, visual and
word movement, Home/End, and grapheme-safe Backspace/Delete, so popup authors
do not recreate a partial text editor. The command palette and argument editor,
Notes page-jump palette, channel browser, searchable dropdown, slash menu, and
link picker use this contract; `Autocomplete` delegates to the fuller
`TextInput`. Composite document editors forward candidate-window geometry from
their focused shaped block just as single-field wrappers do.

Inline slash-command and page-link menus use the same model. Their active
filter is a visible shaped field above the result rows; IME events are consumed
by the menu rather than leaking committed query text into the document. The
menus publish their composition caret through the parent `BlockEditor`, delete
extended graphemes atomically, expose only committed filter values to
accessibility, and keep a keyboard highlight visible in long result sets.

Text boundaries are Unicode data, not ASCII heuristics. `TextInput`,
`TextRenderer::clamp_to_lines`, and custom row metadata consume the public
`line_break_opportunities` API: UAX #14 rules are augmented with ICU's compiled
dictionary/LSTM model for Khmer, Lao, Myanmar, and Thai, while NBSP and emoji
ZWJ sequences remain unbreakable. Word selection, word deletion, and
Ctrl/Command+Arrow use ICU word segmentation as well, including dictionary
boundaries in Thai and CJK plus UAX behavior for apostrophes, combining marks,
and decimal numbers. The segmenters are retained per UI event thread because
ICU4X 1.5's baked payload is intentionally `Rc`-backed.

`RichTextLayout` feeds ICU-only opportunities into both plain and attributed
paragraph shaping as invisible U+200B hints. A central offset map translates
between that private shaping string and the unchanged document UTF-8 byte
space, so hit testing, caret geometry, selection, decorations, IME ranges, and
styled-span boundaries never expose or persist a virtual byte. The hints have
zero visible advance; CJK, NBSP, and emoji continue to follow native UAX #14
behavior while Thai and the other complex-context scripts can wrap at word
boundaries in the real renderer.

Long-form copy can opt into language-aware hyphenation with
`TextStyle::with_auto_hyphenation("en-US")` or the matching
`Text::with_auto_hyphenation` builder. The language tag is reduced to its
primary BCP 47 language and selects an embedded TeX/Liang pattern set with the
language's own minimum word-edge rules. Unknown or malformed languages fall
back to ordinary wrapping, and hyphenation remains off by default for short UI
labels and text whose language is unknown. Discretionary breaks live only in
the private shaping string: unused breaks are invisible, while a taken break
paints U+2010 in the exact run font and reserves its advance during line fit.
The offset map keeps every public range and caret in the original document byte
space.

For example, reading text can opt in without constructing a custom shaper:

```rust
Text::new("Internationalization demonstrates typography.")
    .with_role(TextRole::Reading)
    .with_auto_hyphenation("en-US")
```

## Book composition

The ebook reader's default **Book** composition goes beyond ordinary widget
wrapping. It fits each complete paragraph with a Knuth–Plass-style global
search, adjustable inter-word glue, TeX/Liang hyphenation selected from the
EPUB language, consecutive-hyphen penalties, line-fitness transitions, and
small glyph expansion. Opening and closing punctuation can protrude into the
margin; CJK and Southeast Asian text use Unicode/ICU break opportunities; and
mixed-direction paragraphs preserve visual BiDi order while retaining logical
selection offsets. Pagination keeps headings with their following text and
enforces two-line widow and orphan minima.

Readers can switch the typography popover to **Ragged** composition when they
prefer natural spacing. That mode deliberately disables automatic hyphenation,
glyph expansion, justification, and punctuation protrusion. The preference is
stored with the other reader settings and participates in layout cache
identity.

The actual 1280×800 serif reading surface is a committed visual regression at
`apps/ebook/tests/fixtures/typography/serif-reader.png`. After visually
approving an intentional change, regenerate it through the build container:

```sh
UPDATE_EBOOK_TYPOGRAPHY_BASELINE=1 cargo test -p ebook \
  serif_reader_matches_typography_baseline -- --nocapture
```

## Mathematical composition

Structured formulas use the single compositor in `ui::math`. Its semantic tree,
recursive 2D box layout, insertion-gap geometry, and painter are shared by the
calculator's live editor and the ebook reader's immutable MathML content. The
calculator alone owns mutation, caret-navigation, clipboard, and collaborative
state; these policies are not duplicated in read-only document rendering.

The compositor uses the shipped Noto Sans Math face and mathematical italic
alphabet, with math-class spacing for binary and relation operators. It lays
out fractions, superscripts, subscripts, paired scripts, radicals and indexed
roots, scalable fences, stacked under/over limits, and rectangular tables.
EPUB presentation MathML lowers directly into that tree for both inline and
display formulas. A linear fallback remains attached to each formula for
search, selection, and accessibility.

The same tree has source front ends for practical TeX math and Typst math.
Notes treats fenced `tex`/`latex`/`math` and `typst`/`typst-math` blocks as
formula cards with an explicit syntax badge and a persisted Source/Visual mode.
Source mode is editable text with a live preview; Visual mode reuses the
calculator's structured editor core, promoted into `ui::math::edit`. Switching
modes is lossless until a structured edit occurs, at which point the shared
tree serializes to canonical native TeX or Typst. Both parsers are total: package-specific
or unsupported input is preserved visibly and reported as a preview diagnostic
rather than discarded. Their shared coverage includes scripts, fractions,
indexed roots, large operators and limits, constructed delimiter families,
matrices, arrays, cases, aligned rows, accents, binomials, common mathematical
alphabets, Greek, relations, sets, and arrows.

The real ebook integration is pinned by
`apps/ebook/tests/fixtures/typography/math-reader.png`. Regenerate it only after
visual approval:

```sh
UPDATE_EBOOK_MATH_BASELINE=1 cargo test -p ebook \
  mathematical_reader_matches_shared_typesetting_baseline -- --nocapture
```

Editable shaped text retains caret affinity as well as its UTF-8 document
offset. `TextPosition { byte, affinity }` represents the two legitimate visual
positions that can share one logical byte at a mixed-BiDi run boundary (and at
a soft wrap). `RichTextLayout::hit_position`, `caret_geometry_at`, and
`visual_motion_position` preserve that distinction; `TextInput` and
`RichTextEditor` carry it through clicks, arrow motion, painting, IME anchoring,
and ephemeral rebuild/undo state. Existing byte-only methods remain convenient
wrappers for unambiguous positions, so ordinary application code does not need
to manage affinity.

Editable controls expose the same model to platform accessibility. Each
non-secret field publishes an AccessKit `TextRun` whose characters are Unicode
extended grapheme clusters, plus a direction-preserving caret or selection.
Single- and multiline fields use their corresponding platform roles, and
`SetValue` / `SetTextSelection` actions flow through the same edit and CRDT
paths as keyboard input. Composite editors forward those actions to their
inner text surface. Protected fields publish neither their value nor selection
length, so accessibility does not become a side channel for secrets.

The normal component gallery includes a `Typography & Scripts` card in both
light and dark offscreen renders. It is the visual review corpus for semantic
hierarchy, Latin ligatures/diacritics, Arabic, Hebrew, Devanagari, Bengali,
Thai, CJK, mixed BiDi, and multi-codepoint emoji. Run
`cargo run -p ui-examples --example ui-shots -- gallery_shots` through the repository build wrapper
to regenerate it.

That same card is also a committed visual regression corpus. The integration
suite compares light and dark reference PNGs at 1×, fractional 1.25× display
scale, and combined 1.5× display/1.25× text scale. Comparisons tolerate only
the renderer's measured one-channel antialiasing wobble; a real mismatch writes
the actual frame and a high-contrast pixel diff under the test target's
`CARGO_TARGET_TMPDIR`, in `typography-visual-regression/`. The references are
Mesa llvmpipe renders, and the comparison refuses other adapters.
`typography_baseline_geometry` checks the same layouts against the references'
dimensions without a GPU, so a card that changed size fails every test run.

After visually approving an intentional change, regenerate the references with
the "Render visual baseline candidates" workflow
(`.forgejo/workflows/dev-visual-baselines.yml`), which pins the builder
image and llvmpipe, and review its candidates. Inside that image the
equivalent command is:

```sh
UPDATE_TYPOGRAPHY_BASELINES=1 \
VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json \
WGPU_ADAPTER_NAME=llvmpipe WGPU_BACKEND=vulkan \
  cargo test --locked -p tilo-ui --test it \
  typography_visual_regression::multilingual_gallery_matches_scale_matrix -- --exact
```

## Multi-paragraph editing

`BlockEditor` models a selection as two `DocumentPosition` endpoints rather
than treating each painted line highlight as an independent selection. This
gives mouse drag, keyboard extension, Ctrl+A, plain-text clipboard operations,
IME/text replacement, undo, and remote presence one ordered document range.
Crossed block boundaries serialize as newlines. Replacement preserves the
unselected prefix/suffix and their inline marks, uses the same continuation
kind rules as Enter, and reconciles the complete structural edit through one
host undo boundary.
