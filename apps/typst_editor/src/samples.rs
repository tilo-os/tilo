//! Bundled Typst documents shown when the editor first opens and cycled
//! through by the toolbar's document switcher. Each one is valid Typst 0.14
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

/// Index of the document shown on first launch and in `--shots`.
pub const DEFAULT_INDEX: usize = 0;

const WELCOME: &str = r#"#set page(width: 16cm, height: auto, margin: 1.6cm)
#set text(font: "Libertinus Serif", size: 11pt)
#set par(justify: true)

= Typst, live in qOS

Welcome to the *Typst editor*. Type on the left, watch the document
re-typeset itself on the right. Everything you see here is compiled by the
real `typst` engine and rasterised page-by-page.

== What you can write

You get headings, _emphasis_, *strong* text, and `inline code` for free.
Bullet lists are a dash:

- Collaborative editing, CRDT-backed
- A real compiler, not a preview hack
- Diagnostics with line numbers

Numbered lists use a plus:

+ Edit the source
+ Hit #raw("Ctrl+Enter") to recompile
+ Read the rendered pages

== Mathematics

Inline maths like $a^2 + b^2 = c^2$ flows with the text, and block maths
gets its own centred line:

$ sum_(k=1)^n k = (n (n + 1)) / 2 $

== Code

```rust
fn main() {
    println!("compiled by typst");
}
```

Switch documents from the toolbar to explore more examples.
"#;

const REPORT: &str = r#"#set page(width: 16cm, height: auto, margin: 1.6cm)
#set text(font: "Libertinus Serif", size: 11pt)
#set heading(numbering: "1.1")
#set par(justify: true)

#align(center)[
  #text(18pt, weight: "bold")[Quarterly Engineering Report]

  #text(10pt)[Prepared for the qOS steering group]
]

= Summary

This quarter focused on the collaborative compositor and the CRDT-native UI
toolkit. Throughput improved across every measured surface.

= Metrics

#table(
  columns: 3,
  align: (left, right, right),
  table.header([*Area*], [*Before*], [*After*]),
  [Frame time], [12.4 ms], [7.5 ms],
  [Cold build], [49 G], [24 G],
  [Sync latency], [180 ms], [60 ms],
)

= Model

The convergence guarantee follows from the merge being commutative:

$ A union B = B union A $

#pagebreak()

= Appendix

The second page exists to demonstrate multi-page rendering and the page
thumbnail strip on the right-hand side of the editor.

- Each page is rasterised independently.
- Click a thumbnail to jump to that page.
- Zoom affects only display, not the render resolution.
"#;

const LETTER: &str = r#"#set page(width: 16cm, height: auto, margin: 2cm)
#set text(font: "Libertinus Serif", size: 11pt)

#align(right)[
  qOS Project \
  Spatial Computing Lab \
  #datetime.today().display()
]

#v(1cm)

Dear Reader,

Thank you for trying the Typst editor inside qOS. This letter is a minimal
document: a couple of paragraphs of justified prose and a closing.

The point is to show how little markup it takes to produce a clean,
typeset page. There is no front matter to fight, no template to wrestle —
just text, with the occasional bit of _emphasis_ where it helps.

#v(0.5cm)

Yours, \
The qOS team
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
