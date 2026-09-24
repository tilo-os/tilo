//! Snippet syntax highlighting: the toolkit's grammar-backed highlighter plus
//! the one thing specific to snippets, `${placeholder}` variables, which are
//! marked in every language so the expand-variables affordance reads the same
//! everywhere.
//!
//! Colours are resolved against the [`ui::theme::Theme`] at draw time, so the
//! preview tracks whatever palette the OS hands the app.

use std::ops::Range;

use ui::theme::Theme;
use ui::widget::code_editor::{
    Highlighter, HlState, LineSpans, Span as KindSpan, SyntaxHighlighter, TokenKind,
};
use vello::peniko::Color;

use crate::model::Language;

/// Placeholders borrow the heading role, which both the editable body and the
/// preview paint in the accent colour.
pub const PLACEHOLDER: TokenKind = TokenKind::Heading;

/// The preview's colour for a token kind; every branch reads a theme token.
pub fn token_color(kind: TokenKind, theme: &Theme) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        TokenKind::Keyword => c.accent[6],
        TokenKind::StringLit | TokenKind::CharLit => c.semantic.success,
        TokenKind::Number | TokenKind::Constant => c.semantic.warning,
        TokenKind::Comment => c.text_dim,
        PLACEHOLDER => c.accent[5],
        TokenKind::Function => c.semantic.info,
        TokenKind::Type => c.accent[7],
        TokenKind::Attribute | TokenKind::Preprocessor => c.semantic.danger,
        _ => c.text_primary,
    }
}

/// A contiguous run of preview text sharing one [`TokenKind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub kind: TokenKind,
}

fn grammar(lang: Language) -> SyntaxHighlighter {
    let token = match lang {
        Language::PlainText => return SyntaxHighlighter::plain_text(),
        // Git snippets are shell command lines.
        Language::Shell | Language::Git => "sh",
        Language::Rust => "rs",
        Language::Sql => "sql",
        Language::Python => "py",
        Language::JavaScript => "js",
        Language::Json => "json",
        Language::Yaml => "yaml",
        Language::Markdown => "md",
        Language::Html => "html",
        Language::Css => "css",
        Language::Toml => "toml",
        Language::Dockerfile => "Dockerfile",
    };
    SyntaxHighlighter::for_token_or_plain(token)
}

/// The editable body's highlighter: grammar spans with placeholders on top.
pub struct EditorHighlighter(SyntaxHighlighter);

impl EditorHighlighter {
    pub fn new(lang: Language) -> Self {
        Self(grammar(lang))
    }
}

impl Highlighter for EditorHighlighter {
    fn spans(&self, line: &str, carry: HlState) -> LineSpans {
        let mut result = self.0.spans(line, carry);
        result.spans = overlay_placeholders(line, result.spans);
        result
    }
}

/// Highlight `text` line by line (split like [`str::lines`]), carrying the
/// grammar state across lines so multi-line strings and comments hold.
pub fn highlight_lines(text: &str, lang: Language) -> impl Iterator<Item = Vec<Span>> + '_ {
    let mut state = grammar(lang).start();
    text.lines().map(move |line| {
        overlay_placeholders(line, state.highlight_line(line))
            .into_iter()
            .filter_map(|span| {
                line.get(span.range).map(|text| Span {
                    text: text.to_owned(),
                    kind: span.kind,
                })
            })
            .collect()
    })
}

/// `${name}` ranges: `$` immediately followed by `{`, closed by the next `}`.
/// An unclosed `${` is ordinary text.
fn placeholders(line: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut from = 0;
    while let Some(start) = line.get(from..).and_then(|rest| rest.find("${")) {
        let start = from + start;
        let Some(close) = line.get(start + 2..).and_then(|rest| rest.find('}')) else {
            break;
        };
        let end = start + 2 + close + 1;
        ranges.push(start..end);
        from = end;
    }
    ranges
}

/// Replace whatever the grammar said about each placeholder's bytes with
/// [`PLACEHOLDER`], splitting the spans it overlaps.
fn overlay_placeholders(line: &str, spans: Vec<KindSpan>) -> Vec<KindSpan> {
    let marks = placeholders(line);
    if marks.is_empty() {
        return spans;
    }
    let mut out = Vec::with_capacity(spans.len() + marks.len() * 2);
    for span in spans {
        let mut start = span.range.start;
        for mark in &marks {
            if mark.end <= start || mark.start >= span.range.end {
                continue;
            }
            if mark.start > start {
                out.push(KindSpan::new(start..mark.start, span.kind));
            }
            start = mark.end.min(span.range.end);
        }
        if start < span.range.end {
            out.push(KindSpan::new(start..span.range.end, span.kind));
        }
    }
    out.extend(
        marks
            .into_iter()
            .map(|mark| KindSpan::new(mark, PLACEHOLDER)),
    );
    out.sort_by_key(|span| span.range.start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, lang: Language) -> Vec<Span> {
        highlight_lines(text, lang).next().unwrap_or_default()
    }

    fn has(spans: &[Span], kind: TokenKind, text: &str) -> bool {
        spans.iter().any(|s| s.kind == kind && s.text == text)
    }

    #[test]
    fn placeholder_is_recognised_in_any_language() {
        for lang in Language::ALL {
            let spans = line("echo ${name} done", lang);
            assert!(has(&spans, PLACEHOLDER, "${name}"), "{lang:?}: {spans:?}");
            let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(joined, "echo ${name} done", "{lang:?} drops text");
        }
    }

    #[test]
    fn placeholder_splits_the_string_it_sits_in() {
        let spans = line("git commit -m \"${message} now\"", Language::Git);
        assert!(has(&spans, PLACEHOLDER, "${message}"));
        assert!(has(&spans, TokenKind::StringLit, " now\""));
    }

    #[test]
    fn unclosed_placeholder_is_text() {
        assert!(placeholders("echo ${name").is_empty());
        assert_eq!(placeholders("${a}${b}"), [0..4, 4..8]);
    }

    #[test]
    fn rust_keyword_number_and_string() {
        let spans = line("let x = 42; let s = \"hi\";", Language::Rust);
        assert_eq!(spans[0].kind, TokenKind::Keyword);
        assert_eq!(spans[0].text, "let");
        assert!(has(&spans, TokenKind::Number, "42"));
        assert!(has(&spans, TokenKind::StringLit, "\"hi\""));
    }

    #[test]
    fn comment_line_is_one_span() {
        let spans = line("# a shell comment", Language::Shell);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, TokenKind::Comment);
    }

    #[test]
    fn sql_keywords_and_string() {
        let spans = line("SELECT * FROM users WHERE name = 'x'", Language::Sql);
        assert!(has(&spans, TokenKind::Keyword, "SELECT"));
        assert!(has(&spans, TokenKind::StringLit, "'x'"));
    }

    #[test]
    fn toml_and_dockerfile_keep_highlighting() {
        let spans = line("name = \"tilo\" # note", Language::Toml);
        assert!(has(&spans, TokenKind::StringLit, "\"tilo\""));
        assert!(has(&spans, TokenKind::Comment, "# note"));
        let spans = line("FROM fedora:44", Language::Dockerfile);
        assert!(has(&spans, TokenKind::Keyword, "FROM"));
    }

    #[test]
    fn multi_line_constructs_carry_between_lines() {
        let lines: Vec<_> = highlight_lines("/* open\nstill\n*/", Language::Rust).collect();
        assert!(lines[1].iter().all(|s| s.kind == TokenKind::Comment));
    }

    #[test]
    fn plain_text_is_one_run() {
        let spans = line("a b c", Language::PlainText);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "a b c");
    }

    #[test]
    fn editor_highlighter_marks_placeholders() {
        let hl = EditorHighlighter::new(Language::Shell);
        let spans = hl.spans("echo ${name}", 0).spans;
        assert!(
            spans
                .iter()
                .any(|s| s.kind == PLACEHOLDER && s.range == (5..12))
        );
    }

    #[test]
    fn placeholder_uses_accent_color_from_theme() {
        let theme = Theme::dark();
        assert_eq!(
            token_color(PLACEHOLDER, &theme),
            theme.tokens.colors.accent[5]
        );
    }
}
