//! Bundled Typst documents shown when the editor first opens and listed in
//! its document picker. Each one is valid Typst 0.14
//! and is deliberately kept simple so it always compiles cleanly — they double
//! as the `--shots` fixture and as a feature tour of the highlighter.

/// A bundled, named document.
pub struct Sample {
    pub name: &'static str,
    pub source: &'static str,
}

pub const SAMPLES: &[Sample] = &[
    Sample {
        name: "welcome.typ",
        source: WELCOME,
    },
    Sample {
        name: "report.typ",
        source: REPORT,
    },
    Sample {
        name: "letter.typ",
        source: LETTER,
    },
    Sample {
        name: "cheatsheet.typ",
        source: CHEATSHEET,
    },
];

/// Index of the demo document shown first in `--shots` and the example gallery.
pub const DEFAULT_INDEX: usize = 0;

/// A document carrying exactly one **warning** (an unknown font family, on the
/// first line) and one **error** (a reference to an undefined variable, further
/// down) — both with source spans the compiler can resolve to a line/column.
/// Used by the `errors` `--shots` scenario and the diagnostics tests so the
/// Problems panel, the inline squiggles, and click-to-jump are exercised against
/// real compiler output spanning both severities.
pub const ERROR_WARNING: &str = r#"#set text(font: "Brunhilde Sans", size: 11pt)

= Draft with problems

This paragraph uses #undefined_value, which is unknown.

The heading is fine; the font and variable are wrong.
"#;

/// An optional tutorial sample. The live editor starts with empty source.
pub const STARTER: &str = r"#set page(width: 16cm, height: auto, margin: 1.6cm)
#set text(size: 11pt)

= Untitled

Start writing here. The document on the right updates
as you type.

- A dash makes a bullet
- `Ctrl+S` saves, `Ctrl+Enter` recompiles
";

const WELCOME: &str = r#"#set page(width: 16cm, height: auto, margin: 1.6cm)
#set text(font: "Libertinus Serif", size: 11pt)
#set par(justify: true)

= Getting started with Typst

Type on the left; the rendered document updates on the
right. Typst is a markup language for typesetting. You
write plain text with a few marks and it lays out a
finished page.

== Text and markup

Headings start with `=`. Inline marks give you
_emphasis_, *strong* text, and `code`. Paragraphs are
separated by a blank line, and lists come in two flavours.

A dash makes a bullet list:

- One item
- Another item
- A third

A plus makes a numbered list:

+ First step
+ Second step
+ Third step

== Mathematics

Inline maths such as $a^2 + b^2 = c^2$ flows with the
text, while block maths gets its own centred line:

$ sum_(k=1)^n k = (n (n + 1)) / 2 $

== Code

Fenced blocks keep their monospace formatting:

```rust
fn main() {
    println!("hello");
}
```

Use the document list above to open the other examples.
"#;

const REPORT: &str = r#"#set page(width: 16cm, height: auto, margin: 1.6cm)
#set text(font: "Libertinus Serif", size: 11pt)
#set heading(numbering: "1.1")
#set par(justify: true)

#align(center)[
  #text(18pt, weight: "bold")[Quarterly Engineering Report]

  #text(10pt)[Prepared for the Tilo steering group]
]

= Summary

This quarter focused on the collaborative compositor and
the CRDT-native UI toolkit. Throughput improved across
every measured surface.

= Metrics

#table(
  columns: 3,
  align: (left, right, right),
  table.header([*Area*], [*Before*], [*After*]),
  [Frame time], [12.4 ms], [7.5 ms],
  [Cold build], [49 s], [24 s],
  [Sync latency], [180 ms], [60 ms],
)

= Model

The convergence guarantee follows from the merge
being commutative:

$ A union B = B union A $

#pagebreak()

= Appendix

The second page exists to demonstrate multi-page
rendering and the page thumbnails beside the preview.

- Each page is rasterised independently.
- Click a thumbnail to jump to that page.
- Zoom affects only display, not the render resolution.
"#;

const LETTER: &str = r#"#set page(width: 16cm, height: auto, margin: 2cm)
#set text(font: "Libertinus Serif", size: 11pt)

#align(right)[
  Tilo Project \
  Spatial Computing Lab \
  #datetime.today().display()
]

#v(1cm)

Dear Reader,

Thank you for trying the Typst editor inside Tilo. This
letter is a minimal document: a couple of paragraphs of
justified prose and a closing.

The point is to show how little markup it takes to
produce a clean, typeset page. There is no front matter
to fight and no template to wrestle, just text, with the
occasional bit of _emphasis_ where it helps.

#v(0.5cm)

Yours, \
The Tilo team
"#;

const CHEATSHEET: &str = r#"#set page(width: 16cm, height: auto, margin: 1.4cm)
#set text(font: "Libertinus Serif", size: 10pt)

= Typst syntax cheatsheet

== Markup
- `= Heading` and `== Subheading`
- `*strong*` and `_emphasis_`
- `` `inline code` `` and raw blocks
- `- item` bullets, `+ item` numbered

== Set & show rules
```typ
#set text(size: 12pt)
#set par(justify: true)
#show heading: set text(navy)
```

== Math
Inline $e^(i pi) + 1 = 0$ and display:

$ integral_0^1 x^2 dif x = 1/3 $

== Figures
#figure(
  rect(width: 4cm, height: 1.2cm, fill: luma(230)),
  caption: [A boxed figure],
)
"#;
