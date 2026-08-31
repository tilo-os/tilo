//! Minimal but real XHTML → [`Block`] converter.
//!
//! EPUB chapter content *is* XHTML, so this is the actual rendering front-end
//! for the format. We parse with `roxmltree` (a strict, pure-Rust XML reader),
//! walk `<body>`, and lower the structural elements we care about into flow
//! [`Block`]s with inline [`Span`] styling. Unknown elements are transparently
//! descended into so their text is not lost.

use roxmltree::{Document, Node};
use ui::math::{MathExpr, Node as MathNode, Row};

use crate::model::{Block, Span};

/// Parse a chapter's XHTML source into a flat list of flow blocks.
///
/// Resilient by design: malformed input yields an empty chapter rather than a
/// panic, and a handful of common HTML named entities are normalised to
/// numeric form first (XML, unlike HTML, only predefines five entities).
pub fn parse_xhtml(src: &str) -> Vec<Block> {
    let sanitized = sanitize_entities(src);
    let Ok(doc) = Document::parse(&sanitized) else {
        return Vec::new();
    };
    let Some(body) = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case("body"))
    else {
        return Vec::new();
    };

    let mut blocks = Vec::new();
    for child in body.children() {
        lower_block(child, &mut blocks);
    }
    blocks
}

/// Replace HTML named entities XML doesn't predefine with numeric refs, so the
/// strict XML parser doesn't reject real-world chapters.
fn sanitize_entities(src: &str) -> String {
    // Order matters only for `&` itself, which we never rewrite here (it is one
    // of XML's five predefined entities when written as `&amp;`).
    const SUBS: &[(&str, &str)] = &[
        ("&nbsp;", "&#160;"),
        ("&mdash;", "&#8212;"),
        ("&ndash;", "&#8211;"),
        ("&hellip;", "&#8230;"),
        ("&ldquo;", "&#8220;"),
        ("&rdquo;", "&#8221;"),
        ("&lsquo;", "&#8216;"),
        ("&rsquo;", "&#8217;"),
        ("&copy;", "&#169;"),
        ("&deg;", "&#176;"),
        ("&eacute;", "&#233;"),
    ];
    let mut out = src.to_string();
    if out.contains('&') {
        for (from, to) in SUBS {
            if out.contains(from) {
                out = out.replace(from, to);
            }
        }
    }
    out
}

/// Lower one block-level node, appending zero or more blocks.
fn lower_block(node: Node, out: &mut Vec<Block>) {
    if !node.is_element() {
        return;
    }
    let tag = node.tag_name().name().to_ascii_lowercase();
    match tag.as_str() {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = match tag.as_str() {
                "h1" => 1,
                "h2" => 2,
                "h3" => 3,
                "h4" => 4,
                "h5" => 5,
                _ => 6,
            };
            let spans = collect_inline(node);
            if !spans_empty(&spans) {
                out.push(Block::Heading { level, spans });
            }
        }
        "p" => {
            if let Some(math) = sole_math_child(node) {
                push_math_block(math, out);
                return;
            }
            let spans = collect_inline(node);
            if !spans_empty(&spans) {
                out.push(Block::Paragraph { spans });
            }
        }
        "blockquote" => {
            // A blockquote usually wraps <p>s; flatten each into a Quote block.
            let mut had_child = false;
            for child in node.children() {
                if child.is_element() && child.tag_name().name().eq_ignore_ascii_case("p") {
                    let spans = collect_inline(child);
                    if !spans_empty(&spans) {
                        out.push(Block::Quote { spans });
                        had_child = true;
                    }
                }
            }
            if !had_child {
                let spans = collect_inline(node);
                if !spans_empty(&spans) {
                    out.push(Block::Quote { spans });
                }
            }
        }
        "ul" | "ol" => {
            let ordered = tag == "ol";
            let mut idx = 0usize;
            for child in node.children() {
                if child.is_element() && child.tag_name().name().eq_ignore_ascii_case("li") {
                    idx += 1;
                    let spans = collect_inline(child);
                    if !spans_empty(&spans) {
                        out.push(Block::ListItem {
                            spans,
                            ordered,
                            index: idx,
                        });
                    }
                }
            }
        }
        "math" => push_math_block(node, out),
        "hr" => out.push(Block::Separator),
        // Structural wrappers we descend into (div/section/article/main/etc.).
        _ => {
            for child in node.children() {
                lower_block(child, out);
            }
        }
    }
}

fn sole_math_child<'a, 'input>(node: Node<'a, 'input>) -> Option<Node<'a, 'input>> {
    let mut meaningful = node.children().filter(|child| {
        child.is_element() || child.text().is_some_and(|text| !text.trim().is_empty())
    });
    let only = meaningful.next()?;
    if meaningful.next().is_some() {
        return None; // more than one meaningful child
    }
    (only.is_element() && only.tag_name().name().eq_ignore_ascii_case("math")).then_some(only)
}

fn push_math_block(node: Node, out: &mut Vec<Block>) {
    let root = mathml_row(node);
    if root.is_empty() {
        return;
    }
    let expr = MathExpr { root };
    let source = ui::math::to_linear(&expr);
    let display = node
        .attribute("display")
        .is_some_and(|value| value.eq_ignore_ascii_case("block"));
    out.push(Block::Math {
        expr,
        source,
        display,
    });
}

/// Concatenated text of `node`'s text-node descendants — MathML tokens
/// (`<mi>`/`<mn>`/`<mo>`/`<mtext>`) carry their content as flat text
/// regardless of nesting depth.
fn node_text(node: Node) -> String {
    node.descendants()
        .filter(Node::is_text)
        .filter_map(|child| child.text())
        .collect()
}

/// The `<mtr>` rows of an `<mtable>`, each a row of `<mtd>` cell contents.
fn mathml_table_rows(element_children: &[Node]) -> Vec<Vec<Row>> {
    element_children
        .iter()
        .filter(|row| row.tag_name().name().eq_ignore_ascii_case("mtr"))
        .map(|row| {
            row.children()
                .filter(|cell| {
                    cell.is_element() && cell.tag_name().name().eq_ignore_ascii_case("mtd")
                })
                .map(mathml_children)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The first non-annotation child of a `<semantics>` wrapper — MathML's
/// presentation form, skipping the parallel `<annotation>`/`<annotation-xml>`.
fn mathml_semantics(element_children: Vec<Node>) -> Row {
    element_children
        .into_iter()
        .find(|child| {
            !matches!(
                child.tag_name().name().to_ascii_lowercase().as_str(),
                "annotation" | "annotation-xml"
            )
        })
        .map(mathml_row)
        .unwrap_or_default()
}

/// Lower the presentation subset used by EPUB `MathML` into the same semantic
/// tree the calculator edits. Unsupported wrappers remain transparent, so an
/// unfamiliar styling element never makes its mathematical children vanish.
fn mathml_row(node: Node) -> Row {
    if node.is_text() {
        return node
            .text()
            .unwrap_or_default()
            .trim()
            .chars()
            .map(MathNode::Char)
            .collect();
    }
    if !node.is_element() {
        return Vec::new();
    }
    let tag = node.tag_name().name().to_ascii_lowercase();
    let element_children: Vec<_> = node.children().filter(Node::is_element).collect();
    let child_row = |index: usize| {
        element_children
            .get(index)
            .map(|child| mathml_row(*child))
            .unwrap_or_default()
    };
    match tag.as_str() {
        "mi" | "mn" | "mo" => {
            let text = node_text(node);
            let text = text.trim();
            if tag == "mi"
                && (text.chars().count() > 1 || node.attribute("mathvariant") == Some("normal"))
            {
                vec![MathNode::Text(text.to_string())]
            } else {
                text.chars().map(MathNode::Char).collect()
            }
        }
        "mtext" => vec![MathNode::Text(node_text(node).trim().to_string())],
        "mfrac" => vec![MathNode::Frac {
            num: child_row(0),
            den: child_row(1),
        }],
        "msqrt" => vec![MathNode::Sqrt {
            index: None,
            radicand: mathml_children(node),
        }],
        "mroot" => vec![MathNode::Sqrt {
            radicand: child_row(0),
            index: Some(child_row(1)),
        }],
        "msup" => {
            let mut row = child_row(0);
            row.push(MathNode::Sup { exp: child_row(1) });
            row
        }
        "msub" => {
            let mut row = child_row(0);
            row.push(MathNode::Sub { sub: child_row(1) });
            row
        }
        "msubsup" => {
            let mut row = child_row(0);
            row.push(MathNode::SubSup {
                sub: child_row(1),
                sup: child_row(2),
            });
            row
        }
        "mover" => vec![MathNode::UnderOver {
            base: child_row(0),
            under: None,
            over: Some(child_row(1)),
        }],
        "munder" => vec![MathNode::UnderOver {
            base: child_row(0),
            under: Some(child_row(1)),
            over: None,
        }],
        "munderover" => vec![MathNode::UnderOver {
            base: child_row(0),
            under: Some(child_row(1)),
            over: Some(child_row(2)),
        }],
        "mtable" => vec![MathNode::Table {
            rows: mathml_table_rows(&element_children),
        }],
        "mfenced" => vec![MathNode::Delimited {
            open: mathml_delimiter(node.attribute("open"), '('),
            close: mathml_delimiter(node.attribute("close"), ')'),
            body: mathml_children(node),
        }],
        "semantics" => mathml_semantics(element_children),
        "annotation" | "annotation-xml" => Vec::new(),
        _ => mathml_children(node),
    }
}

fn mathml_children(node: Node) -> Row {
    node.children().flat_map(mathml_row).collect()
}

fn mathml_delimiter(value: Option<&str>, default: char) -> Option<char> {
    match value {
        Some("" | ".") => None,
        Some(value) => value.chars().next(),
        None => Some(default),
    }
}

fn spans_empty(spans: &[Span]) -> bool {
    spans.iter().all(|s| s.text.trim().is_empty())
}

/// Collect the inline content of a block element into styled spans, collapsing
/// runs of whitespace and merging adjacent spans of identical style.
fn collect_inline(node: Node) -> Vec<Span> {
    let mut b = SpanBuilder::default();
    walk_inline(node, Style::default(), &mut b);
    b.finish()
}

#[derive(Clone, Copy, Default)]
struct Style {
    bold: bool,
    italic: bool,
    code: bool,
}

fn walk_inline(node: Node, style: Style, b: &mut SpanBuilder) {
    for child in node.children() {
        if child.is_text() {
            if let Some(t) = child.text() {
                b.push_text(t, style);
            }
        } else if child.is_element() {
            let tag = child.tag_name().name().to_ascii_lowercase();
            if tag == "math" {
                let expr = MathExpr {
                    root: mathml_row(child),
                };
                if !expr.is_empty() {
                    let source = ui::math::to_linear(&expr);
                    b.push_math(expr, source, style);
                }
                continue;
            }
            let mut s = style;
            match tag.as_str() {
                "strong" | "b" => s.bold = true,
                "em" | "i" | "cite" => s.italic = true,
                "code" | "kbd" | "samp" | "tt" => s.code = true,
                "br" => {
                    b.push_text(" ", style);
                    continue;
                }
                _ => {}
            }
            walk_inline(child, s, b);
        }
    }
}

/// Accumulates spans while normalising whitespace across text-node boundaries.
///
/// Whitespace within a same-style run stays as a single literal space; a break
/// at a *style* boundary is lifted onto the next span's `space_before` flag
/// (leaving the span text trimmed). This keeps runs clean — `.` after `</em>`
/// hugs the emphasised word instead of picking up a spurious gap.
#[derive(Default)]
struct SpanBuilder {
    spans: Vec<Span>,
    /// A word break is pending (collapsing any run of whitespace to one).
    pending_space: bool,
    /// Any non-whitespace has been emitted (suppresses a leading block space).
    started: bool,
}

impl SpanBuilder {
    fn push_math(&mut self, expr: MathExpr, source: String, style: Style) {
        let space_before = self.pending_space && self.started;
        self.pending_space = false;
        self.spans.push(Span {
            text: source,
            math: Some(expr),
            bold: style.bold,
            italic: style.italic,
            code: false,
            space_before,
        });
        self.started = true;
    }

    fn push_text(&mut self, raw: &str, style: Style) {
        for ch in raw.chars() {
            if ch.is_whitespace() {
                if self.started {
                    self.pending_space = true;
                }
                continue;
            }
            let continues_last = self.spans.last_mut().filter(|l| {
                l.math.is_none()
                    && l.bold == style.bold
                    && l.italic == style.italic
                    && l.code == style.code
            });
            if let Some(last) = continues_last {
                // Continuation of the current run: a pending break is an
                // internal space (word separation within one style).
                if self.pending_space {
                    last.text.push(' ');
                    self.pending_space = false;
                }
                last.text.push(ch);
            } else {
                // New run: the pending break becomes its `space_before` flag.
                let space_before = self.pending_space;
                self.pending_space = false;
                self.spans.push(Span {
                    text: ch.to_string(),
                    math: None,
                    bold: style.bold,
                    italic: style.italic,
                    code: style.code,
                    space_before,
                });
            }
            self.started = true;
        }
    }

    fn finish(self) -> Vec<Span> {
        self.spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_heading_and_paragraph() {
        let src = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
            <h1>Title</h1>
            <p>Hello <strong>bold</strong> and <em>italic</em>.</p>
        </body></html>"#;
        let blocks = parse_xhtml(src);
        assert_eq!(blocks.len(), 2);
        match &blocks[0] {
            Block::Heading { level, spans } => {
                assert_eq!(*level, 1);
                assert_eq!(spans[0].text, "Title");
            }
            _ => panic!("expected heading"),
        }
        match &blocks[1] {
            Block::Paragraph { spans } => {
                let bold = spans.iter().find(|s| s.bold).unwrap();
                assert_eq!(bold.text, "bold");
                let ital = spans.iter().find(|s| s.italic).unwrap();
                assert_eq!(ital.text, "italic");
            }
            _ => panic!("expected paragraph"),
        }
    }

    #[test]
    fn collapses_whitespace() {
        let src = "<body><p>one\n   two\t three</p></body>";
        let blocks = parse_xhtml(src);
        assert_eq!(blocks[0].plain_text(), "one two three");
    }

    #[test]
    fn parses_lists_quotes_and_hr() {
        let src = r"<body>
            <ul><li>a</li><li>b</li></ul>
            <blockquote><p>quoted</p></blockquote>
            <hr/>
        </body>";
        let blocks = parse_xhtml(src);
        let li = blocks
            .iter()
            .filter(|b| matches!(b, Block::ListItem { .. }))
            .count();
        assert_eq!(li, 2);
        assert!(blocks.iter().any(|b| matches!(b, Block::Quote { .. })));
        assert!(blocks.iter().any(super::super::model::Block::is_separator));
    }

    #[test]
    fn presentation_mathml_lowers_to_shared_structured_math() {
        let src = r#"<body xmlns="http://www.w3.org/1999/xhtml">
          <math xmlns="http://www.w3.org/1998/Math/MathML" display="block">
            <mfrac>
              <mrow><msup><mi>x</mi><mn>2</mn></msup><mo>+</mo><mn>1</mn></mrow>
              <msqrt><mrow><mi>y</mi><mo>-</mo><mn>3</mn></mrow></msqrt>
            </mfrac>
          </math>
        </body>"#;
        let blocks = parse_xhtml(src);
        let Block::Math {
            expr,
            source,
            display,
        } = &blocks[0]
        else {
            panic!("expected a mathematical block");
        };
        assert!(*display);
        assert!(matches!(expr.root.first(), Some(MathNode::Frac { .. })));
        assert_eq!(source, "(x^(2)+1)/(sqrt(y-3))");
    }

    #[test]
    fn mathml_subscripts_and_superscripts_share_one_script_node() {
        let src =
            r"<body><p><math><msubsup><mo>∑</mo><mi>i</mi><mi>n</mi></msubsup></math></p></body>";
        let blocks = parse_xhtml(src);
        let Block::Math { expr, .. } = &blocks[0] else {
            panic!("expected wrapped MathML to remain mathematical");
        };
        assert!(matches!(expr.root.get(1), Some(MathNode::SubSup { .. })));
    }

    #[test]
    fn inline_mathml_remains_structured_inside_prose() {
        let src = r"<body><p>Einstein wrote <math><mi>E</mi><mo>=</mo><mi>m</mi><msup><mi>c</mi><mn>2</mn></msup></math> in 1905.</p></body>";
        let blocks = parse_xhtml(src);
        let Block::Paragraph { spans } = &blocks[0] else {
            panic!("expected prose paragraph");
        };
        let math = spans
            .iter()
            .find(|span| span.math.is_some())
            .expect("inline formula remains a structured span");
        assert_eq!(math.text, "E=mc^(2)");
        assert!(math.space_before);
        assert_eq!(blocks[0].plain_text(), "Einstein wrote E=mc^(2) in 1905.");
    }

    #[test]
    fn mathml_limits_and_matrices_remain_two_dimensional() {
        let src = r#"<body><math display="block"><mrow>
          <munderover><mo>∑</mo><mrow><mi>i</mi><mo>=</mo><mn>1</mn></mrow><mi>n</mi></munderover>
          <mfenced><mtable>
            <mtr><mtd><mi>a</mi></mtd><mtd><mi>b</mi></mtd></mtr>
            <mtr><mtd><mi>c</mi></mtd><mtd><mi>d</mi></mtd></mtr>
          </mtable></mfenced>
        </mrow></math></body>"#;
        let blocks = parse_xhtml(src);
        let Block::Math { expr, .. } = &blocks[0] else {
            panic!("expected display math");
        };
        assert!(matches!(expr.root[0], MathNode::UnderOver { .. }));
        let MathNode::Delimited { open, close, body } = &expr.root[1] else {
            panic!("matrix fence retained");
        };
        assert_eq!((*open, *close), (Some('('), Some(')')));
        let MathNode::Table { rows } = &body[0] else {
            panic!("matrix remains a table");
        };
        assert_eq!((rows.len(), rows[0].len()), (2, 2));
        assert_eq!(rows[0][0], vec![MathNode::Char('a')]);
        assert_eq!(rows[0][1], vec![MathNode::Char('b')]);
        assert_eq!(rows[1][0], vec![MathNode::Char('c')]);
        assert_eq!(rows[1][1], vec![MathNode::Char('d')]);
    }

    #[test]
    fn sanitizes_named_entities() {
        let src = "<body><p>a&nbsp;b&mdash;c</p></body>";
        let blocks = parse_xhtml(src);
        // nbsp (U+00A0) is whitespace and collapses; mdash survives.
        assert!(blocks[0].plain_text().contains('\u{2014}'));
    }

    #[test]
    fn empty_on_garbage() {
        assert!(parse_xhtml("<<<not xml").is_empty());
    }
}
