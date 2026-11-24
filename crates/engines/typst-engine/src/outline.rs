//! A source's headings, read from its syntax rather than its lines: a heading in raw text, a
//! comment or a string is not one. Headings a script makes exist only once compiled, and are
//! not guessed at.

use typst::syntax::{LinkedNode, Source, SyntaxKind, SyntaxNode, ast};

/// A heading written in the source. Lines are counted from 1, as an editor goes to them.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Heading {
    pub line: usize,
    pub level: usize,
    pub title: String,
}

/// The headings `source` writes.
#[must_use]
pub fn headings(source: &str) -> Vec<Heading> {
    headings_in(&Source::detached(source))
}

fn headings_in(source: &Source) -> Vec<Heading> {
    let mut result = Vec::new();
    collect_headings(&LinkedNode::new(source.root()), source, &mut result);
    result
}

fn collect_headings(node: &LinkedNode<'_>, source: &Source, result: &mut Vec<Heading>) {
    if matches!(
        node.get().kind(),
        SyntaxKind::Raw | SyntaxKind::LineComment | SyntaxKind::BlockComment
    ) {
        return;
    }
    if let Some(heading) = node.get().cast::<ast::Heading<'_>>()
        && let Some(line) = source.lines().byte_to_line(node.range().start)
    {
        use typst::syntax::ast::AstNode as _;
        let mut title = String::new();
        collect_title(heading.body().to_untyped(), &mut title);
        result.push(Heading {
            line: line + 1,
            level: heading.depth().get(),
            title: title.split_whitespace().collect::<Vec<_>>().join(" "),
        });
    }
    for child in node.children() {
        collect_headings(&child, source, result);
    }
}

/// A heading's readable text: its markup without delimiters, labels or comments.
fn collect_title(node: &SyntaxNode, title: &mut String) {
    match node.kind() {
        SyntaxKind::Text | SyntaxKind::MathText | SyntaxKind::SmartQuote => {
            title.push_str(node.leaf_text());
        }
        SyntaxKind::Space | SyntaxKind::Parbreak | SyntaxKind::Linebreak => title.push(' '),
        SyntaxKind::Escape => {
            if let Some(escape) = node.cast::<ast::Escape<'_>>() {
                title.push(escape.get());
            }
        }
        SyntaxKind::Shorthand => {
            if let Some(shorthand) = node.cast::<ast::Shorthand<'_>>() {
                title.push(shorthand.get());
            }
        }
        SyntaxKind::Raw => {
            if let Some(raw) = node.cast::<ast::Raw<'_>>() {
                for line in raw.lines() {
                    title.push_str(line.get());
                }
            }
        }
        SyntaxKind::LineComment | SyntaxKind::BlockComment | SyntaxKind::Label => {}
        _ => {
            for child in node.children() {
                collect_title(child, title);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_are_read_from_syntax_with_one_based_lines() {
        let found = headings("intro\n= Café λ\n\n=== Deep\n== Sibling\n");
        assert_eq!(
            found,
            vec![
                Heading {
                    line: 2,
                    level: 1,
                    title: "Café λ".into()
                },
                Heading {
                    line: 4,
                    level: 3,
                    title: "Deep".into()
                },
                Heading {
                    line: 5,
                    level: 2,
                    title: "Sibling".into()
                },
            ]
        );
        let ignored = "// = Comment\n/*\n== Block comment\n*/\n```typ\n= Raw example\n```\n#let example = \"= String\"\n= Real\n";
        assert_eq!(
            headings(ignored),
            vec![Heading {
                line: 9,
                level: 1,
                title: "Real".into(),
            }]
        );
        let styled = headings("= *Bold* _italic_ `code` \\# café <intro>\n");
        assert_eq!(
            styled.first().map(|heading| heading.title.as_str()),
            Some("Bold italic code # café")
        );
        assert!(headings("").is_empty());
    }
}
