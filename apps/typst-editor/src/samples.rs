//! Bundled Typst documents for captures and tests, using Typst 0.15.
//! They exercise real typesetting and the highlighter without seeding a live
//! workspace. The tutorial remains available alongside a compact research note.

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
    Sample {
        name: "motion-notes.typ",
        source: MOTION_NOTES,
    },
];

/// Index of the demo document shown first in `--shots` and the example gallery.
pub const DEFAULT_INDEX: usize = 4;

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

const MOTION_NOTES: &str = r##"#set page(width: 18cm, height: auto, margin: (x: 1.2cm, y: 0.8cm))
#set text(font: "Libertinus Serif", size: 10pt)
#set par(justify: true, leading: 0.6em)
#show heading: set text(fill: rgb("#245568"))

#text(8pt, tracking: 1.5pt)[MOTION SYSTEMS / RESEARCH NOTE 01]
= Smooth motion, by design
_Leah Chen · Interaction Lab · October 2026_

*Abstract.* A good transition makes a change
easy to follow without drawing attention to
itself. We derive one smooth trajectory from
six boundary conditions.

== From constraints to a curve
Let $tau = t / T$ be normalized time. A
minimum-jerk path starts and ends at rest,
with zero acceleration:

$ s(tau) = 10 tau^3 - 15 tau^4 + 6 tau^5 $

#block(fill: rgb("#eef4f6"), inset: 9pt, radius: 3pt)[
  *Boundary conditions.* For $tau in {0, 1}$,
  $s'(tau) = s''(tau) = 0$. Among paths with these
  endpoints, this curve minimizes the squared jerk:
  $ J = integral_0^T (dif^3 x / dif t^3)^2 dif t $
]

== A gentler handoff
The curve covers little distance near either endpoint
and moves fastest halfway through. Unlike linear
motion, it has no sudden jump in velocity.

#figure(
  table(columns: (1.4fr, 1fr, 1fr, 1fr),
    stroke: none, inset: 5pt,
    fill: (x, y) => if y == 0 { rgb("#eef4f6") },
    align: (left, right, right, right),
    table.header([*Time*], [*25%*], [*50%*], [*75%*]),
    [Linear], [25.0%], [50.0%], [75.0%],
    [Minimum jerk], [10.4%], [50.0%], [89.6%],
  ),
  caption: [Distance traveled at three points in time.],
)

== Design implication
Use this profile for short, deliberate transitions.
Keep the destination stable, allow interruption, and
respect the reader's reduced-motion preference.
"##;

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

Use Open to work on a .typ file of your own.
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
