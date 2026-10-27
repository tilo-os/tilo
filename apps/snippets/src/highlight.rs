//! Snippet syntax highlighting: the toolkit's grammar-backed highlighter plus
//! the one thing specific to snippets, `${placeholder}` variables, which are
//! marked in every language so the expand-variables affordance reads the same
//! everywhere.
//!
//! Colours come from the theme's shared syntax palette
//! (`theme.tokens.syntax`) at draw time, so the preview tracks whatever
//! palette the OS hands the app and matches every other code surface.

use std::ops::Range;

use ui::widget::code_editor::{
    Highlighter, HlState, LineSpans, Span as KindSpan, SyntaxHighlighter, TokenKind,
};

use crate::model::Language;

/// Placeholders borrow the markup-heading role, which the syntax palette
/// paints in the primary accent in both the editable body and the preview.
pub const PLACEHOLDER: TokenKind = TokenKind::Heading;

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

    /// Placeholder recognition, splitting, unclosed text, editor marking and palette role.
    #[test]
    fn placeholders_are_highlighted_in_every_language() {
        {
            for lang in Language::ALL {
                let spans = line("echo ${name} done", lang);
                assert!(has(&spans, PLACEHOLDER, "${name}"), "{lang:?}: {spans:?}");
                let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
                assert_eq!(joined, "echo ${name} done", "{lang:?} drops text");
            }
        }

        {
            let spans = line("git commit -m \"${message} now\"", Language::Git);
            assert!(has(&spans, PLACEHOLDER, "${message}"));
            assert!(has(&spans, TokenKind::StringLit, " now\""));
        }

        {
            assert!(placeholders("echo ${name").is_empty());
            assert_eq!(placeholders("${a}${b}"), [0..4, 4..8]);
        }

        {
            let hl = EditorHighlighter::new(Language::Shell);
            let spans = hl.spans("echo ${name}", 0).spans;
            assert!(
                spans
                    .iter()
                    .any(|s| s.kind == PLACEHOLDER && s.range == (5..12))
            );
        }

        {
            use ui::ColorCompat;
            let theme = ui::theme::Theme::dark();
            let rgb = |c: vello::peniko::Color| [c.r(), c.g(), c.b()];
            assert_eq!(
                rgb(theme.tokens.syntax.color(PLACEHOLDER)),
                rgb(theme.tokens.syntax.markup)
            );
        }
    }

    /// Rust, comments, SQL, TOML/Dockerfile, multi-line constructs and plain text.
    #[test]
    fn language_grammars_highlight_their_tokens() {
        {
            let spans = line("let x = 42; let s = \"hi\";", Language::Rust);
            assert_eq!(spans[0].kind, TokenKind::Keyword);
            assert_eq!(spans[0].text, "let");
            assert!(has(&spans, TokenKind::Number, "42"));
            assert!(has(&spans, TokenKind::StringLit, "\"hi\""));
        }

        {
            let spans = line("# a shell comment", Language::Shell);
            assert_eq!(spans.len(), 1);
            assert_eq!(spans[0].kind, TokenKind::Comment);
        }

        {
            let spans = line("SELECT * FROM users WHERE name = 'x'", Language::Sql);
            assert!(has(&spans, TokenKind::Keyword, "SELECT"));
            assert!(has(&spans, TokenKind::StringLit, "'x'"));
        }

        {
            let spans = line("name = \"tilo\" # note", Language::Toml);
            assert!(has(&spans, TokenKind::StringLit, "\"tilo\""));
            assert!(has(&spans, TokenKind::Comment, "# note"));
            let spans = line("FROM fedora:44", Language::Dockerfile);
            assert!(has(&spans, TokenKind::Keyword, "FROM"));
        }

        {
            let lines: Vec<_> = highlight_lines("/* open\nstill\n*/", Language::Rust).collect();
            assert!(lines[1].iter().all(|s| s.kind == TokenKind::Comment));
        }

        {
            let spans = line("a b c", Language::PlainText);
            assert_eq!(spans.len(), 1);
            assert_eq!(spans[0].text, "a b c");
        }
    }
}
