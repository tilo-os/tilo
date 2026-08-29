//! Lightweight, dependency-free syntax highlighter.
//!
//! This is a per-line tokenizer, not a full parser: it classifies runs
//! of a line into [`TokenKind`]s using language-specific keyword sets
//! and a handful of generic lexical rules (strings, numbers, comments,
//! `${placeholder}` variables). That's plenty for the read-only preview
//! pane, where the goal is legibility, not a compiler-grade AST.
//!
//! Colours are resolved against the [`ui::theme::Theme`] at draw time —
//! [`TokenKind::color`] maps each kind onto a theme token so the
//! highlighter tracks whatever palette the OS hands the app.

use ui::theme::Theme;
use vello::peniko::Color;

use crate::model::Language;

/// The lexical class of a highlighted span. Each maps to a theme colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// Default foreground — identifiers, operators, punctuation.
    Plain,
    /// Language keyword (`fn`, `SELECT`, `if`, …).
    Keyword,
    /// String / char literal.
    StringLit,
    /// Numeric literal.
    Number,
    /// Line or block comment.
    Comment,
    /// A `${name}` template placeholder — the headline feature, so it
    /// gets the accent colour to stand out in the body.
    Placeholder,
    /// Function-call-ish identifier (identifier immediately before `(`).
    Function,
    /// Common builtin / type names.
    Builtin,
}

impl TokenKind {
    /// Resolve this kind to a concrete colour from the active theme.
    /// Every branch reads a theme token — no hard-coded literals — so a
    /// theme swap recolours the whole preview.
    pub fn color(self, theme: &Theme) -> Color {
        let c = &theme.tokens.colors;
        match self {
            TokenKind::Plain => c.text_primary,
            TokenKind::Keyword => c.accent[6],
            TokenKind::StringLit => c.semantic.success,
            TokenKind::Number => c.semantic.warning,
            TokenKind::Comment => c.text_dim,
            TokenKind::Placeholder => c.accent[5],
            TokenKind::Function => c.semantic.info,
            TokenKind::Builtin => c.accent[7],
        }
    }
}

/// A contiguous run of text sharing one [`TokenKind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub kind: TokenKind,
}

/// Highlight one line of `lang` source into colour spans. `${...}`
/// placeholders are always recognised regardless of language so the
/// expand-variables affordance reads the same everywhere.
pub fn highlight_line(line: &str, lang: Language) -> Vec<Span> {
    // Comment lines short-circuit: the whole line is one comment span.
    if let Some(prefix) = comment_prefix(lang)
        && line.trim_start().starts_with(prefix)
    {
        return vec![Span {
            text: line.to_string(),
            kind: TokenKind::Comment,
        }];
    }

    let mut spans = lex(line, lang);
    // Merge adjacent same-kind spans so the draw loop emits fewer runs.
    coalesce(&mut spans);
    spans
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

/// Keyword set for a language. Empty for data/markup formats that lean
/// on the generic lexer (strings / numbers / placeholders).
fn keywords(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::Rust => &[
            "fn", "let", "mut", "const", "static", "struct", "enum", "trait", "impl", "for",
            "while", "loop", "if", "else", "match", "return", "pub", "use", "mod", "crate", "self",
            "super", "as", "in", "ref", "move", "where", "type", "dyn", "async", "await", "unsafe",
            "extern", "break", "continue",
        ],
        Language::Shell => &[
            "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac",
            "function", "in", "return", "export", "local", "set", "echo", "cd", "exit",
        ],
        Language::Sql => &[
            "SELECT",
            "FROM",
            "WHERE",
            "INSERT",
            "INTO",
            "VALUES",
            "UPDATE",
            "SET",
            "DELETE",
            "CREATE",
            "TABLE",
            "ALTER",
            "DROP",
            "JOIN",
            "LEFT",
            "RIGHT",
            "INNER",
            "OUTER",
            "ON",
            "GROUP",
            "BY",
            "ORDER",
            "HAVING",
            "LIMIT",
            "OFFSET",
            "AND",
            "OR",
            "NOT",
            "NULL",
            "AS",
            "DISTINCT",
            "COUNT",
            "INDEX",
            "PRIMARY",
            "KEY",
            "FOREIGN",
            "REFERENCES",
            "RETURNING",
        ],
        Language::Python => &[
            "def", "class", "import", "from", "as", "if", "elif", "else", "for", "while", "return",
            "yield", "with", "try", "except", "finally", "raise", "lambda", "pass", "break",
            "continue", "in", "is", "and", "or", "not", "None", "True", "False", "async", "await",
        ],
        Language::JavaScript => &[
            "function",
            "const",
            "let",
            "var",
            "if",
            "else",
            "for",
            "while",
            "return",
            "class",
            "extends",
            "new",
            "this",
            "import",
            "export",
            "from",
            "default",
            "async",
            "await",
            "try",
            "catch",
            "finally",
            "throw",
            "typeof",
            "instanceof",
            "of",
            "in",
            "null",
            "undefined",
            "true",
            "false",
        ],
        Language::Git => &[
            "git",
            "add",
            "commit",
            "push",
            "pull",
            "fetch",
            "merge",
            "rebase",
            "checkout",
            "branch",
            "status",
            "log",
            "diff",
            "reset",
            "stash",
            "tag",
            "remote",
            "clone",
            "cherry-pick",
            "switch",
            "restore",
        ],
        Language::Dockerfile => &[
            "FROM",
            "RUN",
            "CMD",
            "LABEL",
            "EXPOSE",
            "ENV",
            "ADD",
            "COPY",
            "ENTRYPOINT",
            "VOLUME",
            "USER",
            "WORKDIR",
            "ARG",
            "ONBUILD",
            "HEALTHCHECK",
            "SHELL",
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
        Language::Sql => &[
            "INT",
            "INTEGER",
            "VARCHAR",
            "TEXT",
            "BOOLEAN",
            "TIMESTAMP",
            "SERIAL",
            "UUID",
        ],
        _ => &[],
    }
}

/// The generic per-character lexer. Recognises `${placeholder}`,
/// strings, numbers and identifiers; classifies identifiers against the
/// language keyword / builtin sets.
fn lex(line: &str, lang: Language) -> Vec<Span> {
    let kws = keywords(lang);
    let bis = builtins(lang);
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        // ${placeholder}
        if c == '$'
            && i + 1 < chars.len()
            && chars[i + 1] == '{'
            && let Some(end) = find_char(&chars, i + 2, '}')
        {
            let text: String = chars[i..=end].iter().collect();
            spans.push(Span {
                text,
                kind: TokenKind::Placeholder,
            });
            i = end + 1;
            continue;
        }

        // string literal
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            if i < chars.len() {
                i += 1; // include the closing quote
            }
            let text: String = chars[start..i].iter().collect();
            spans.push(Span {
                text,
                kind: TokenKind::StringLit,
            });
            continue;
        }

        // number
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            spans.push(Span {
                text,
                kind: TokenKind::Number,
            });
            continue;
        }

        // identifier (letters, digits, _, -)
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '-')
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let next_non_ws = chars[i..].iter().find(|c| !c.is_whitespace()).copied();
            let kind = if kws.iter().any(|k| *k == word) {
                TokenKind::Keyword
            } else if bis.iter().any(|b| *b == word) {
                TokenKind::Builtin
            } else if next_non_ws == Some('(') {
                TokenKind::Function
            } else {
                TokenKind::Plain
            };
            spans.push(Span { text: word, kind });
            continue;
        }

        // everything else: punctuation / whitespace — Plain.
        let start = i;
        i += 1;
        let text: String = chars[start..i].iter().collect();
        spans.push(Span {
            text,
            kind: TokenKind::Plain,
        });
    }
    spans
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == target)
}

/// Merge runs of identical-kind spans into one so the renderer emits
/// fewer text calls per line.
fn coalesce(spans: &mut Vec<Span>) {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans.drain(..) {
        if let Some(last) = out.last_mut()
            && last.kind == s.kind
        {
            last.text.push_str(&s.text);
            continue;
        }
        out.push(s);
    }
    *spans = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(spans: &[Span]) -> Vec<TokenKind> {
        spans.iter().map(|s| s.kind).collect()
    }

    #[test]
    fn placeholder_is_recognised_in_any_language() {
        let spans = highlight_line("echo ${name}", Language::PlainText);
        assert!(
            spans
                .iter()
                .any(|s| s.kind == TokenKind::Placeholder && s.text == "${name}")
        );
    }

    #[test]
    fn rust_keyword_is_highlighted() {
        let spans = highlight_line("fn main() {}", Language::Rust);
        assert_eq!(spans[0].kind, TokenKind::Keyword);
        assert_eq!(spans[0].text, "fn");
    }

    #[test]
    fn function_call_identifier_is_function_kind() {
        let spans = highlight_line("println(x)", Language::Rust);
        assert_eq!(spans[0].kind, TokenKind::Function);
    }

    #[test]
    fn whole_comment_line_is_one_span() {
        let spans = highlight_line("# a shell comment", Language::Shell);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, TokenKind::Comment);
    }

    #[test]
    fn sql_keywords_upper_and_string() {
        let spans = highlight_line("SELECT * FROM users WHERE name = 'x'", Language::Sql);
        assert!(kinds(&spans).contains(&TokenKind::Keyword));
        assert!(
            spans
                .iter()
                .any(|s| s.kind == TokenKind::StringLit && s.text == "'x'")
        );
    }

    #[test]
    fn number_literal_is_number_kind() {
        let spans = highlight_line("let x = 42", Language::Rust);
        assert!(
            spans
                .iter()
                .any(|s| s.kind == TokenKind::Number && s.text == "42")
        );
    }

    #[test]
    fn coalesce_merges_same_kind_runs() {
        let spans = highlight_line("a b c", Language::PlainText);
        // "a b c" is all plain text — should collapse into a single span.
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "a b c");
    }

    #[test]
    fn placeholder_uses_accent_color_from_theme() {
        let theme = Theme::dark();
        let c = TokenKind::Placeholder.color(&theme);
        assert_eq!(c, theme.tokens.colors.accent[5]);
    }
}
