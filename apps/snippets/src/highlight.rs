//! Per-language tokenizer, expressed as the shared [`Highlighter`] the
//! [`CodeEditor`](ui::widget::code_editor::CodeEditor) drives.
//!
//! The shared editor owns rendering, caret, selection, gutter and the
//! token→theme-colour mapping ([`token_color`]); an app supplies only the
//! per-line lexing. This module is that lexer for snippet bodies: a
//! dependency-free classifier that recognises keywords, builtins/types,
//! strings, numbers, line comments and — the headline feature — `${name}`
//! template variables, across the languages a snippet library holds. Every
//! run is emitted as a byte-range [`Span`] tagged with a shared [`TokenKind`],
//! so a snippet body highlights with exactly the same code surface (and the
//! same light/dark palette) the code editor and SQL console use — no private
//! render path, no forked colour table.

use std::ops::Range;

use ui::widget::code_editor::{Highlighter, HlState, LineSpans, Span, TokenKind};

use crate::model::Language;

/// A [`Highlighter`] for one snippet [`Language`]. Line-local (block comments
/// are not tracked across lines — snippet bodies are short), so it ignores the
/// cross-line [`HlState`] carry and always returns `0`.
pub struct SnippetHighlighter {
    lang: Language,
}

impl SnippetHighlighter {
    /// A highlighter for `lang`.
    pub fn new(lang: Language) -> Self {
        Self { lang }
    }

    /// The highlighter boxed for [`CodeEditor::with_highlighter`].
    ///
    /// [`CodeEditor::with_highlighter`]: ui::widget::code_editor::CodeEditor::with_highlighter
    pub fn boxed(lang: Language) -> Box<dyn Highlighter> {
        Box::new(Self::new(lang))
    }
}

impl Highlighter for SnippetHighlighter {
    fn spans(&self, line: &str, _carry: HlState) -> LineSpans {
        // A whole-line comment short-circuits to one span.
        if let Some(prefix) = comment_prefix(self.lang) {
            let trimmed = line.trim_start();
            if trimmed.starts_with(prefix) {
                return LineSpans::line_local(vec![Span::new(0..line.len(), TokenKind::Comment)]);
            }
        }
        let mut spans = lex(line, self.lang);
        coalesce(&mut spans);
        LineSpans::line_local(spans)
    }
}

/// The single-line comment prefix for a language, if it has one.
fn comment_prefix(lang: Language) -> Option<&'static str> {
    match lang {
        Language::Rust | Language::JavaScript | Language::Css => Some("//"),
        Language::Shell
        | Language::Python
        | Language::Yaml
        | Language::Toml
        | Language::Dockerfile
        | Language::Git => Some("#"),
        Language::Sql => Some("--"),
        _ => None,
    }
}

/// Keyword set for a language. Empty for data/markup formats that lean on the
/// generic lexer (strings / numbers / placeholders).
fn keywords(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => &[
            "fn", "let", "mut", "const", "static", "struct", "enum", "trait", "impl", "for",
            "while", "loop", "if", "else", "match", "return", "pub", "use", "mod", "crate",
            "self", "super", "as", "in", "ref", "move", "where", "type", "dyn", "async", "await",
            "unsafe", "extern", "break", "continue",
        ],
        Language::Shell => &[
            "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac",
            "function", "in", "return", "export", "local", "set", "echo", "cd", "exit",
        ],
        Language::Sql => &[
            "SELECT", "FROM", "WHERE", "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE",
            "CREATE", "TABLE", "ALTER", "DROP", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "ON",
            "GROUP", "BY", "ORDER", "HAVING", "LIMIT", "OFFSET", "AND", "OR", "NOT", "NULL", "AS",
            "DISTINCT", "COUNT", "INDEX", "PRIMARY", "KEY", "FOREIGN", "REFERENCES", "RETURNING",
        ],
        Language::Python => &[
            "def", "class", "import", "from", "as", "if", "elif", "else", "for", "while", "return",
            "yield", "with", "try", "except", "finally", "raise", "lambda", "pass", "break",
            "continue", "in", "is", "and", "or", "not", "None", "True", "False", "async", "await",
        ],
        Language::JavaScript => &[
            "function", "const", "let", "var", "if", "else", "for", "while", "return", "class",
            "extends", "new", "this", "import", "export", "from", "default", "async", "await",
            "try", "catch", "finally", "throw", "typeof", "instanceof", "of", "in", "null",
            "undefined", "true", "false",
        ],
        Language::Git => &[
            "git", "add", "commit", "push", "pull", "fetch", "merge", "rebase", "checkout",
            "branch", "status", "log", "diff", "reset", "stash", "tag", "remote", "clone",
            "cherry-pick", "switch", "restore",
        ],
        Language::Dockerfile => &[
            "FROM", "RUN", "CMD", "LABEL", "EXPOSE", "ENV", "ADD", "COPY", "ENTRYPOINT", "VOLUME",
            "USER", "WORKDIR", "ARG", "ONBUILD", "HEALTHCHECK", "SHELL",
        ],
        _ => &[],
    }
}

/// Builtin / type names, coloured distinctly from keywords.
fn builtins(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => &[
            "String", "Vec", "Option", "Result", "Box", "Some", "None", "Ok", "Err", "u8", "u16",
            "u32", "u64", "usize", "i32", "i64", "f32", "f64", "bool", "str", "char",
        ],
        Language::Sql => &["INT", "INTEGER", "VARCHAR", "TEXT", "BOOLEAN", "TIMESTAMP", "SERIAL", "UUID"],
        _ => &[],
    }
}

/// The generic per-character lexer, emitting byte-range spans into `line`.
/// Recognises `${placeholder}`, strings, numbers and identifiers; classifies
/// identifiers against the language keyword / builtin sets.
fn lex(line: &str, lang: Language) -> Vec<Span> {
    let kws = keywords(lang);
    let bis = builtins(lang);
    let bytes = line.as_bytes();
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut i = 0;
    // Byte offset one past the last character, for end ranges.
    let end_byte = line.len();
    let byte_at = |ci: usize| -> usize { chars.get(ci).map(|(b, _)| *b).unwrap_or(end_byte) };

    while i < chars.len() {
        let (start_b, c) = chars[i];

        // ${placeholder}
        if c == '$' && i + 1 < chars.len() && chars[i + 1].1 == '{' {
            if let Some(end_ci) = (i + 2..chars.len()).find(|&j| chars[j].1 == '}') {
                let end_b = byte_at(end_ci + 1);
                spans.push(Span::new(start_b..end_b, TokenKind::Attribute));
                i = end_ci + 1;
                continue;
            }
        }

        // string literal
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            i += 1;
            while i < chars.len() && chars[i].1 != quote {
                if chars[i].1 == '\\' {
                    i += 1;
                }
                i += 1;
            }
            if i < chars.len() {
                i += 1; // include the closing quote
            }
            let end_b = byte_at(i);
            spans.push(Span::new(start_b..end_b, TokenKind::StringLit));
            continue;
        }

        // number
        if c.is_ascii_digit() {
            while i < chars.len()
                && (chars[i].1.is_ascii_alphanumeric() || chars[i].1 == '.' || chars[i].1 == '_')
            {
                i += 1;
            }
            spans.push(Span::new(start_b..byte_at(i), TokenKind::Number));
            continue;
        }

        // identifier (letters, digits, _, -)
        if c.is_alphabetic() || c == '_' {
            let word_start = i;
            while i < chars.len()
                && (chars[i].1.is_alphanumeric() || chars[i].1 == '_' || chars[i].1 == '-')
            {
                i += 1;
            }
            let end_b = byte_at(i);
            let word = &line[start_b..end_b];
            let next_non_ws = chars[i..].iter().map(|(_, c)| *c).find(|c| !c.is_whitespace());
            let kind = if kws.iter().any(|k| *k == word) {
                TokenKind::Keyword
            } else if bis.iter().any(|b| *b == word) {
                TokenKind::Type
            } else if next_non_ws == Some('(') {
                TokenKind::Function
            } else {
                TokenKind::Variable
            };
            let _ = word_start;
            spans.push(Span::new(start_b..end_b, kind));
            continue;
        }

        // everything else: punctuation / whitespace — plain body text.
        let next_b = byte_at(i + 1).min(end_byte).max(start_b + 1);
        // Guard against multi-byte punctuation edge (use the char's byte len).
        let step = c.len_utf8();
        let end_b = (start_b + step).min(bytes.len());
        spans.push(Span::new(start_b..end_b, TokenKind::Text));
        i += 1;
        let _ = next_b;
    }
    spans
}

/// Merge adjacent same-kind, contiguous spans so the editor emits fewer runs.
fn coalesce(spans: &mut Vec<Span>) {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans.drain(..) {
        if let Some(last) = out.last_mut() {
            if last.kind == s.kind && last.range.end == s.range.start {
                last.range.end = s.range.end;
                continue;
            }
        }
        out.push(s);
    }
    *spans = out;
}

/// A byte range paired with the kind the highlighter assigned it — used by the
/// small static preview renderer that paints the *filled* body in the copy
/// affordance (the live editor renders the *template*).
pub fn spans_for(lang: Language, line: &str) -> Vec<(Range<usize>, TokenKind)> {
    SnippetHighlighter::new(lang)
        .spans(line, 0)
        .spans
        .into_iter()
        .map(|s| (s.range, s.kind))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lang: Language, line: &str) -> Vec<(String, TokenKind)> {
        SnippetHighlighter::new(lang)
            .spans(line, 0)
            .spans
            .into_iter()
            .map(|s| (line[s.range].to_string(), s.kind))
            .collect()
    }

    #[test]
    fn placeholder_is_recognised_in_any_language() {
        let out = kinds(Language::PlainText, "echo ${name}");
        assert!(out.iter().any(|(t, k)| *k == TokenKind::Attribute && t == "${name}"));
    }

    #[test]
    fn rust_keyword_is_highlighted() {
        let out = kinds(Language::Rust, "fn main() {}");
        assert_eq!(out[0], ("fn".to_string(), TokenKind::Keyword));
    }

    #[test]
    fn function_call_identifier_is_function_kind() {
        let out = kinds(Language::Rust, "println(x)");
        assert_eq!(out[0].1, TokenKind::Function);
    }

    #[test]
    fn whole_comment_line_is_one_span() {
        let ls = SnippetHighlighter::new(Language::Shell).spans("# a shell comment", 0);
        assert_eq!(ls.spans.len(), 1);
        assert_eq!(ls.spans[0].kind, TokenKind::Comment);
    }

    #[test]
    fn sql_keywords_upper_and_string() {
        let out = kinds(Language::Sql, "SELECT * FROM users WHERE name = 'x'");
        assert!(out.iter().any(|(_, k)| *k == TokenKind::Keyword));
        assert!(out.iter().any(|(t, k)| *k == TokenKind::StringLit && t == "'x'"));
    }

    #[test]
    fn number_literal_is_number_kind() {
        let out = kinds(Language::Rust, "let x = 42");
        assert!(out.iter().any(|(t, k)| *k == TokenKind::Number && t == "42"));
    }

    #[test]
    fn spans_cover_the_line_without_gaps_or_overlap() {
        // Every byte of the line falls in exactly one span, in order — the
        // invariant the editor's renderer relies on.
        let line = "kubectl -n ${ns} logs -f ${pod}";
        let spans = SnippetHighlighter::new(Language::Shell).spans(line, 0).spans;
        let mut cursor = 0;
        for s in &spans {
            assert_eq!(s.range.start, cursor, "span starts where the previous ended");
            assert!(s.range.end > s.range.start, "no empty spans");
            cursor = s.range.end;
        }
        assert_eq!(cursor, line.len(), "spans reach the end of the line");
    }

    #[test]
    fn multibyte_line_stays_on_char_boundaries() {
        // A non-ASCII char must not split a span mid-codepoint.
        let line = "echo café ${x}";
        let spans = SnippetHighlighter::new(Language::Shell).spans(line, 0).spans;
        for s in &spans {
            assert!(line.is_char_boundary(s.range.start));
            assert!(line.is_char_boundary(s.range.end));
        }
    }
}
