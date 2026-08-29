//! Minimal but real XHTML → [`Block`] converter.
//!
//! EPUB chapter content *is* XHTML, so this is the actual rendering front-end
//! for the format. We parse with `roxmltree` (a strict, pure-Rust XML reader),
//! walk `<body>`, and lower the structural elements we care about into flow
//! [`Block`]s with inline [`Span`] styling. Unknown elements are transparently
//! descended into so their text is not lost.

use roxmltree::{Document, Node};

use crate::model::{Block, Span};

/// Parse a chapter's XHTML source into a flat list of flow blocks.
///
/// Resilient by design: malformed input yields an empty chapter rather than a
/// panic, and a handful of common HTML named entities are normalised to
/// numeric form first (XML, unlike HTML, only predefines five entities).
pub fn parse_xhtml(src: &str) -> Vec<Block> {
    let sanitized = sanitize_entities(src);
    let doc = match Document::parse(&sanitized) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
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
            let level = tag.as_bytes()[1] - b'0';
            let spans = collect_inline(node);
            if !spans_empty(&spans) {
                out.push(Block::Heading { level, spans });
            }
        }
        "p" => {
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
        "hr" => out.push(Block::Separator),
        // Structural wrappers we descend into (div/section/article/main/etc.).
        _ => {
            for child in node.children() {
                lower_block(child, out);
            }
        }
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
    fn push_text(&mut self, raw: &str, style: Style) {
        for ch in raw.chars() {
            if ch.is_whitespace() {
                if self.started {
                    self.pending_space = true;
                }
                continue;
            }
            let same_style = self.spans.last().is_some_and(|l| {
                l.bold == style.bold && l.italic == style.italic && l.code == style.code
            });
            if same_style {
                // Continuation of the current run: a pending break is an
                // internal space (word separation within one style).
                let last = self.spans.last_mut().unwrap();
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
        let src = r#"<body>
            <ul><li>a</li><li>b</li></ul>
            <blockquote><p>quoted</p></blockquote>
            <hr/>
        </body>"#;
        let blocks = parse_xhtml(src);
        let li = blocks
            .iter()
            .filter(|b| matches!(b, Block::ListItem { .. }))
            .count();
        assert_eq!(li, 2);
        assert!(blocks.iter().any(|b| matches!(b, Block::Quote { .. })));
        assert!(blocks.iter().any(|b| b.is_separator()));
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
