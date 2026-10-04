//! Syntax colouring for the diff and merge panels. Tokenizing is the
//! toolkit's grammar-backed highlighter
//! ([`ui::widget::code_editor::SyntaxHighlighter`]), chosen from the file name
//! a panel shows; colours come from the theme's shared syntax palette, so code
//! reads exactly as it does in every other code surface. What stays here is
//! specific to diffing: each side of a comparison is tokenized as one
//! continuous document (so a block comment or a multi-line string colours
//! correctly across aligned rows), and a line paints as coloured runs at a
//! panel's own origin and size.

use std::path::Path;

use ui::paint::PaintCtx;
use ui::tokens::{SyntaxPalette, TextRole};
use ui::widget::code_editor::{Span, SyntaxHighlighter, TokenKind};
use vello::kurbo::Point;
use vello::peniko::Color;

/// A panel label as the file it names and the revision it shows: the label
/// `rate_limiter.rs (working tree)` is `("rate_limiter.rs", Some("working tree"))`.
pub fn split_label(label: &str) -> (&str, Option<&str>) {
    let label = label.trim();
    match label.rsplit_once(" (") {
        Some((name, rest)) if rest.ends_with(')') => {
            (name.trim_end(), rest.strip_suffix(')').map(str::trim))
        }
        _ => (label, None),
    }
}

/// The file a panel label names: the label without its revision.
pub fn file_name(label: &str) -> &str {
    split_label(label).0
}

/// The grammar for a panel label such as `"rate_limiter.rs (working tree)"`
/// or a path: its file extension, else its whole file name (`Dockerfile`),
/// else plain text.
pub fn grammar_for_label(label: &str) -> SyntaxHighlighter {
    let path = Path::new(file_name(label));
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(SyntaxHighlighter::for_token)
        .or_else(|| {
            path.file_name()
                .and_then(|file| file.to_str())
                .and_then(SyntaxHighlighter::for_token)
        })
        .unwrap_or_else(SyntaxHighlighter::plain_text)
}

/// Spans for each line of one continuous document, in order.
pub fn document<'a>(
    grammar: &SyntaxHighlighter,
    lines: impl IntoIterator<Item = &'a str>,
) -> Vec<Vec<Span>> {
    let mut state = grammar.start();
    lines
        .into_iter()
        .map(|line| state.highlight_line(line))
        .collect()
}

/// The colour a token paints in. The theme's syntax palette sets attributes,
/// macros, tags and keys (a TOML `host = …`) in the danger hue, which reads as
/// an error on a line that has none; they take the type colour instead.
fn token_colour(syntax: &SyntaxPalette, kind: TokenKind) -> Color {
    match kind {
        TokenKind::Attribute | TokenKind::Preprocessor => syntax.type_name,
        other => syntax.color(other),
    }
}

/// Paint `text` at `origin` (the top of its line box) in the `Code` role as
/// runs coloured by `spans`. A line with no spans (its side was never
/// tokenized) paints as body text.
pub fn draw_line(ctx: &mut PaintCtx<'_>, origin: Point, text: &str, spans: &[Span]) {
    let syntax = &ctx.theme.tokens.syntax;
    let style = ctx.theme.text(TextRole::Code);
    if spans.is_empty() {
        if !text.is_empty() {
            ctx.text(text, &style, syntax.text, origin, None);
        }
        return;
    }
    let mut at = origin;
    for span in spans {
        let Some(run) = text.get(span.range.clone()) else {
            continue;
        };
        if run.is_empty() {
            continue;
        }
        ctx.text(run, &style, token_colour(syntax, span.kind), at, None);
        let (w, _) = ctx.text.measure_style(run, None, &style);
        at.x += f64::from(w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::code_editor::TokenKind;

    fn kinds(line: &str, spans: &[Span], kind: TokenKind) -> Vec<String> {
        spans
            .iter()
            .filter(|span| span.kind == kind)
            .filter_map(|span| line.get(span.range.clone()))
            .map(str::to_owned)
            .collect()
    }

    /// Syntax highlighting: the grammar follows the file a label names, token kinds
    /// cover a line, and a side is highlighted as one document. Each phase is a
    /// former test with its own inputs.
    #[test]
    fn highlighting_picks_grammar_by_label_and_spans_whole_documents() {
        the_grammar_follows_the_file_a_label_names();
        keywords_strings_and_comments_take_their_kinds();
        a_side_is_one_document_so_block_comments_span_rows();
    }

    fn the_grammar_follows_the_file_a_label_names() {
        assert_eq!(
            grammar_for_label("rate_limiter.rs (working tree)").name(),
            "Rust"
        );
        assert_eq!(grammar_for_label("config.toml (ours)").name(), "TOML");
        assert_eq!(
            file_name("rate_limiter.rs (working tree)"),
            "rate_limiter.rs"
        );
        assert_eq!(file_name("plain.txt"), "plain.txt");
        assert_eq!(
            grammar_for_label("/srv/app/Dockerfile").name(),
            "Dockerfile"
        );
        assert_eq!(grammar_for_label("notes (draft)").name(), "Plain Text");
        assert_eq!(grammar_for_label("").name(), "Plain Text");
    }

    fn keywords_strings_and_comments_take_their_kinds() {
        let grammar = grammar_for_label("a.rs");
        let line = "pub fn f() { let s = \"hi\"; } // done";
        let spans = &document(&grammar, [line])[0];
        assert!(kinds(line, spans, TokenKind::Keyword).contains(&"pub".to_owned()));
        assert_eq!(kinds(line, spans, TokenKind::StringLit), ["\"hi\""]);
        assert_eq!(kinds(line, spans, TokenKind::Comment), ["// done"]);
        let covered: usize = spans.iter().map(|span| span.range.len()).sum();
        assert_eq!(covered, line.len(), "spans cover the whole line");
    }

    fn a_side_is_one_document_so_block_comments_span_rows() {
        let grammar = grammar_for_label("a.rs");
        let lines = ["/* opened", "still inside", "*/ let x = 1;"];
        let spans = document(&grammar, lines);
        assert_eq!(
            kinds(lines[1], &spans[1], TokenKind::Comment),
            ["still inside"],
            "the middle row continues the comment"
        );
        assert!(kinds(lines[2], &spans[2], TokenKind::Keyword).contains(&"let".to_owned()));
    }
}
