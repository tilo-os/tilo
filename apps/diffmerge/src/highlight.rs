//! Lightweight, theme-aware token highlighter for code in the diff/merge
//! panels. This is deliberately language-agnostic and conservative: it tints
//! a small set of universally-recognisable token classes (keywords, strings,
//! comments, numbers, punctuation) so code reads as code without pulling in a
//! full grammar engine. Every colour is derived from the [`ui::theme::Theme`]
//! so highlighting respects the OS palette.

use ui::theme::Theme;
use vello::peniko::Color;

/// A classified run of characters within a single line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub class: TokenClass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenClass {
    Plain,
    Keyword,
    Str,
    Number,
    Comment,
    Punct,
    Ident,
}

impl TokenClass {
    /// Resolve a colour from the theme. Tints are pulled from the accent and
    /// neutral ramps + semantic colours so they harmonise with any theme.
    pub fn color(self, theme: &Theme) -> Color {
        let c = &theme.tokens.colors;
        match self {
            TokenClass::Plain | TokenClass::Ident => c.text_primary,
            TokenClass::Keyword => c.accent[6], // lighter accent
            TokenClass::Str => c.semantic.success,
            TokenClass::Number => c.semantic.warning,
            TokenClass::Comment => c.text_dim,
            TokenClass::Punct => c.neutral[6],
        }
    }
}

/// Tokens recognised as keywords across the languages we ship samples for.
const KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "pub", "use", "impl", "struct", "enum", "trait", "for", "while", "loop",
    "if", "else", "match", "return", "self", "Self", "true", "false", "as", "in", "mod", "const",
    "static", "ref", "move", "where", "type", "dyn", "async", "await", "crate", "super", "break",
    "continue", "and", "or", "not", "def", "class", "import", "from", "None", "True", "False",
    "null", "var", "function", "new", "this",
];

/// Classify one line into a sequence of spans covering every character.
///
/// Operates on a `Vec<char>` view of the line (not raw byte offsets), so
/// every span boundary this returns is a char index, safe to use with
/// `chars().collect()` on the same line — see [`segments`].
pub fn classify(line: &str) -> Vec<Span> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut spans: Vec<Span> = Vec::new();
    let mut i = 0;

    while let Some(&c) = chars.get(i) {
        // Line comments: // … or # … (toml/python/shell) or ; … (ini)
        let next = chars.get(i + 1).copied();
        if (c == '/' && next == Some('/')) || c == '#' || (c == '-' && next == Some('-')) {
            // `--` only as a comment when it starts a token boundary; cheap
            // heuristic — treat the rest of the line as a comment.
            spans.push(Span {
                start: i,
                end: n,
                class: TokenClass::Comment,
            });
            break;
        }

        // Strings: "…" or '…' with naive escape skipping.
        if c == '"' || c == '\'' {
            let quote = c;
            let start = i;
            i += 1;
            while let Some(&cur) = chars.get(i) {
                if cur == '\\' && chars.get(i + 1).is_some() {
                    i += 2;
                    continue;
                }
                if cur == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            spans.push(Span {
                start,
                end: i,
                class: TokenClass::Str,
            });
            continue;
        }

        // Numbers.
        if c.is_ascii_digit() {
            let start = i;
            while chars
                .get(i)
                .copied()
                .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '_')
            {
                i += 1;
            }
            spans.push(Span {
                start,
                end: i,
                class: TokenClass::Number,
            });
            continue;
        }

        // Identifiers / keywords.
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while chars
                .get(i)
                .copied()
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
            {
                i += 1;
            }
            let word: String = chars.get(start..i).unwrap_or(&[]).iter().collect();
            let class = if KEYWORDS.contains(&word.as_str()) {
                TokenClass::Keyword
            } else {
                TokenClass::Ident
            };
            spans.push(Span {
                start,
                end: i,
                class,
            });
            continue;
        }

        // Whitespace stays plain.
        if c.is_whitespace() {
            let start = i;
            while chars.get(i).copied().is_some_and(char::is_whitespace) {
                i += 1;
            }
            spans.push(Span {
                start,
                end: i,
                class: TokenClass::Plain,
            });
            continue;
        }

        // Punctuation run.
        let start = i;
        while chars.get(i).copied().is_some_and(|ch| {
            !ch.is_alphanumeric() && ch != '_' && !ch.is_whitespace() && ch != '"' && ch != '\''
        }) {
            i += 1;
        }
        spans.push(Span {
            start,
            end: i,
            class: TokenClass::Punct,
        });
    }

    spans
}

/// Slice the original line into `(text, color)` segments using the theme.
pub fn segments(line: &str, theme: &Theme) -> Vec<(String, Color)> {
    let chars: Vec<char> = line.chars().collect();
    classify(line)
        .into_iter()
        .map(|s| {
            let end = s.end.min(chars.len());
            let text: String = chars.get(s.start..end).unwrap_or(&[]).iter().collect();
            (text, s.class.color(theme))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_is_classified() {
        let spans = classify("pub fn foo() {");
        // First span "pub" is a keyword.
        assert_eq!(spans[0].class, TokenClass::Keyword);
    }

    #[test]
    fn string_literal_spans_quotes() {
        let spans = classify("let s = \"hi\";");
        assert!(spans.iter().any(|s| s.class == TokenClass::Str));
    }

    #[test]
    fn comment_consumes_rest_of_line() {
        let spans = classify("x = 1 // trailing");
        let comment = spans
            .iter()
            .find(|s| s.class == TokenClass::Comment)
            .unwrap();
        // Comment runs to end of line.
        assert_eq!(comment.end, "x = 1 // trailing".chars().count());
    }

    #[test]
    fn number_is_classified() {
        let spans = classify("port = 8080");
        assert!(spans.iter().any(|s| s.class == TokenClass::Number));
    }

    #[test]
    fn segments_cover_full_line() {
        let theme = Theme::dark();
        let line = "let x = 42;";
        let segs = segments(line, &theme);
        let joined: String = segs.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, line);
    }
}
