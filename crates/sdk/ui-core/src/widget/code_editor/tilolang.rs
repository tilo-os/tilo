//! tilolang, coloured by its own lexer (`tilolang::lexer`): what the compiler reads as a
//! keyword, a string, a number, a reference literal (`@board.Card:c1`) or a comment is what the
//! editor colours as one, so the colours never disagree with the language.

use tilolang::lexer::{self, CONTEXTUAL, Tok};

use super::{Highlighter, HlState, LineSpans, Span, TokenKind};

/// Colours tilolang source a line at a time. A tilolang string or comment never spans lines,
/// so no state carries from one line to the next.
pub struct TilolangHighlighter;

impl Highlighter for TilolangHighlighter {
    fn spans(&self, line: &str, _carry: HlState) -> LineSpans {
        LineSpans {
            spans: spans_of(line),
            carry: 0,
        }
    }
}

/// The coloured runs of one line of tilolang, in order and not overlapping.
fn spans_of(line: &str) -> Vec<Span> {
    let (tokens, _) = lexer::lex(line);
    let mut spans: Vec<Span> = Vec::with_capacity(tokens.len());
    let mut first = true;
    for (at, token) in tokens.iter().enumerate() {
        let next = tokens.get(at + 1).map(|token| &token.tok);
        let kind = match &token.tok {
            Tok::Kw("true" | "false") => Some(TokenKind::Constant),
            Tok::Kw(_) => Some(TokenKind::Keyword),
            // A contextual keyword is one only where a declaration starts.
            Tok::Ident(name) if first && CONTEXTUAL.contains(&name.as_str()) => {
                Some(TokenKind::Keyword)
            }
            Tok::Ident(_) if next == Some(&Tok::LParen) => Some(TokenKind::Function),
            Tok::Ident(name) if name.starts_with(|c: char| c.is_ascii_uppercase()) => {
                Some(TokenKind::Type)
            }
            Tok::Int(_)
            | Tok::Float(_)
            | Tok::Length(..)
            | Tok::Duration(..)
            | Tok::Calendar(_) => Some(TokenKind::Number),
            Tok::Str(_) => Some(TokenKind::StringLit),
            Tok::Ref(_) => Some(TokenKind::Attribute),
            Tok::Doc(_) => Some(TokenKind::Comment),
            Tok::Arrow
            | Tok::FatArrow
            | Tok::Assign
            | Tok::PlusAssign
            | Tok::MinusAssign
            | Tok::EqEq
            | Tok::NotEq
            | Tok::Lt
            | Tok::Le
            | Tok::Gt
            | Tok::Ge
            | Tok::Plus
            | Tok::Minus
            | Tok::Star
            | Tok::Slash
            | Tok::Percent
            | Tok::QDot
            | Tok::QQ
            | Tok::Pipe
            | Tok::DotDot => Some(TokenKind::Operator),
            _ => None,
        };
        if !matches!(token.tok, Tok::Newline | Tok::Eof) {
            first = false;
        }
        let (start, end) = (
            token.span.start.min(line.len()),
            token.span.end.min(line.len()),
        );
        if let Some(kind) = kind
            && start < end
        {
            spans.push(Span::new(start..end, kind));
        }
    }
    for comment in lexer::plain_comments(line) {
        let (start, end) = (comment.start.min(line.len()), line.len());
        if start < end {
            spans.retain(|span| span.range.end <= start);
            spans.push(Span::new(start..end, TokenKind::Comment));
        }
    }
    spans.sort_by_key(|span| span.range.start);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str) -> Vec<(String, TokenKind)> {
        TilolangHighlighter
            .spans(line, 0)
            .spans
            .into_iter()
            .map(|span| (line[span.range].to_owned(), span.kind))
            .collect()
    }

    #[test]
    fn tilolang_is_coloured_as_its_lexer_reads_it() {
        let line = r#"let open = board.cards.filter(.column.title != "Done")"#;
        let found = kinds(line);
        assert!(
            found.contains(&("let".into(), TokenKind::Keyword)),
            "{found:?}"
        );
        assert!(
            found.contains(&("filter".into(), TokenKind::Function)),
            "{found:?}"
        );
        assert!(
            found.contains(&("\"Done\"".into(), TokenKind::StringLit)),
            "{found:?}"
        );
        assert!(
            found.contains(&("!=".into(), TokenKind::Operator)),
            "{found:?}"
        );

        let answer = r#"@board.Card:root/cards:c1 "Write the release notes" points: 3"#;
        let found = kinds(answer);
        assert_eq!(
            found.first(),
            Some(&("@board.Card:root/cards:c1".into(), TokenKind::Attribute)),
            "a reference literal is one run: {found:?}"
        );
        assert!(
            found.contains(&("3".into(), TokenKind::Number)),
            "{found:?}"
        );

        let found = kinds("todo.check(@todo.Todo:t4, done: true) // the last one");
        assert!(
            found.contains(&("true".into(), TokenKind::Constant)),
            "{found:?}"
        );
        assert_eq!(
            found.last(),
            Some(&("// the last one".into(), TokenKind::Comment)),
            "{found:?}"
        );
        assert_eq!(
            kinds("// 3 items"),
            vec![("// 3 items".into(), TokenKind::Comment)]
        );

        let found = kinds("view Release(board: Board) -> 2rem");
        assert!(
            found.contains(&("view".into(), TokenKind::Keyword)),
            "{found:?}"
        );
        assert!(
            found.contains(&("Board".into(), TokenKind::Type)),
            "{found:?}"
        );
        assert!(
            found.contains(&("2rem".into(), TokenKind::Number)),
            "{found:?}"
        );
        assert!(
            found.contains(&("->".into(), TokenKind::Operator)),
            "{found:?}"
        );

        // Runs are in order and never overlap, so the editor can paint them as they come.
        let spans = TilolangHighlighter
            .spans(r#"if x > 1 { "a" } // b"#, 0)
            .spans;
        assert!(
            spans
                .windows(2)
                .all(|pair| pair[0].range.end <= pair[1].range.start),
            "{spans:?}"
        );
        // An unfinished line still colours what it can.
        assert!(!kinds(r#"board.cards.filter(.title == "Wri"#).is_empty());
    }
}
