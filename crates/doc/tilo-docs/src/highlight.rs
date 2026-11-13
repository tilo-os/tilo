//! Syntax colouring for the languages the docs show: a small lexer per
//! family, enough to set strings, comments, keywords and numbers apart. It
//! never changes the text: concatenating its spans gives the input back.

use std::fmt::Write as _;

use crate::html::escape;

/// The kinds of token that get a colour; everything else is plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Keyword,
    String,
    Comment,
    Number,
    /// `true`, `None`, `null`.
    Literal,
    /// A name called as a function, or a shell line's command.
    Function,
    /// An object key (JSON, TOML) or a shell option (`--name`).
    Key,
    /// A shell variable (`$TILO_API_KEY`).
    Variable,
}

impl Token {
    fn class(self) -> &'static str {
        match self {
            Self::Keyword => "k",
            Self::String => "s",
            Self::Comment => "c",
            Self::Number => "n",
            Self::Literal => "l",
            Self::Function => "f",
            Self::Key => "a",
            Self::Variable => "v",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Python,
    Script,
    Shell,
    Json,
    Toml,
    Plain,
}

impl Family {
    fn of(language: &str) -> Self {
        match language {
            "python" | "py" => Self::Python,
            "typescript" | "ts" | "javascript" | "js" | "tsx" | "jsx" => Self::Script,
            "sh" | "bash" | "shell" | "console" | "curl" | "zsh" | "fish" => Self::Shell,
            "json" | "jsonc" => Self::Json,
            "toml" => Self::Toml,
            _ => Self::Plain,
        }
    }

    fn keywords(self) -> &'static [&'static str] {
        match self {
            Self::Python => &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def",
                "del", "elif", "else", "except", "finally", "for", "from", "global", "if",
                "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
                "try", "while", "with", "yield",
            ],
            Self::Script => &[
                "as",
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "default",
                "delete",
                "do",
                "else",
                "export",
                "extends",
                "finally",
                "for",
                "from",
                "function",
                "if",
                "import",
                "in",
                "instanceof",
                "interface",
                "let",
                "new",
                "of",
                "return",
                "switch",
                "throw",
                "try",
                "type",
                "typeof",
                "var",
                "void",
                "while",
                "yield",
            ],
            Self::Shell => &[
                "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function",
                "if", "in", "then", "while",
            ],
            Self::Json | Self::Toml | Self::Plain => &[],
        }
    }

    fn literals(self) -> &'static [&'static str] {
        match self {
            Self::Python => &["True", "False", "None", "self"],
            Self::Script => &["true", "false", "null", "undefined", "this"],
            Self::Json | Self::Toml => &["true", "false", "null"],
            Self::Shell | Self::Plain => &[],
        }
    }

    fn line_comment(self) -> Option<&'static str> {
        match self {
            Self::Python | Self::Shell | Self::Toml => Some("#"),
            Self::Script | Self::Json => Some("//"),
            Self::Plain => None,
        }
    }

    fn quotes(self) -> &'static [char] {
        match self {
            Self::Script => &['"', '\'', '`'],
            Self::Python | Self::Shell | Self::Toml => &['"', '\''],
            Self::Json => &['"'],
            Self::Plain => &[],
        }
    }
}

/// `code` as HTML, its tokens wrapped in `<span class="…">`.
#[must_use]
pub fn highlight(language: &str, code: &str) -> String {
    let family = Family::of(language);
    let mut out = String::with_capacity(code.len().saturating_mul(2));
    if family == Family::Plain {
        out.push_str(&escape(code));
        return out;
    }
    Lexer {
        family,
        rest: code,
        line_start: true,
        previous: None,
        out: &mut out,
    }
    .run();
    out
}

/// Reads tokens off the front of what is left of the code.
struct Lexer<'a> {
    family: Family,
    rest: &'a str,
    /// No word has been read on this line yet (a shell line's first word is
    /// its command).
    line_start: bool,
    /// The last character written.
    previous: Option<char>,
    out: &'a mut String,
}

impl Lexer<'_> {
    /// Writes the next `len` bytes as `token`.
    fn emit(&mut self, token: Option<Token>, len: usize) {
        let (text, rest) = self.rest.split_at_checked(len).unwrap_or((self.rest, ""));
        match token {
            Some(token) => {
                let _ = write!(
                    self.out,
                    "<span class=\"{}\">{}</span>",
                    token.class(),
                    escape(text)
                );
            }
            None => self.out.push_str(&escape(text)),
        }
        self.previous = text.chars().last().or(self.previous);
        self.rest = rest;
    }

    /// The length of the longest prefix whose characters all `keep`.
    fn prefix_len(text: &str, keep: impl Fn(char) -> bool) -> usize {
        text.find(|c: char| !keep(c)).unwrap_or(text.len())
    }

    fn run(&mut self) {
        while let Some(c) = self.rest.chars().next() {
            if c == '\n' {
                self.line_start = true;
                self.emit(None, 1);
            } else if c.is_whitespace() {
                self.emit(
                    None,
                    Self::prefix_len(self.rest, |c| c.is_whitespace() && c != '\n'),
                );
            } else if self.at_comment() {
                self.emit(
                    Some(Token::Comment),
                    self.rest.find('\n').unwrap_or(self.rest.len()),
                );
            } else if self.family.quotes().contains(&c) {
                let len = self.string_len(c);
                let after = self.rest.get(len..).unwrap_or_default();
                let key = self.family == Family::Json
                    && after.trim_start_matches([' ', '\t']).starts_with(':');
                self.emit(Some(if key { Token::Key } else { Token::String }), len);
                self.line_start = false;
            } else if c.is_ascii_digit() {
                self.emit(
                    Some(Token::Number),
                    Self::prefix_len(self.rest, |c| {
                        c.is_ascii_alphanumeric() || matches!(c, '.' | '_')
                    }),
                );
                self.line_start = false;
            } else if self.family == Family::Shell {
                self.shell_word(c);
            } else if c.is_alphabetic() || c == '_' || c == '$' {
                self.word();
            } else {
                self.line_start = false;
                self.emit(None, c.len_utf8());
            }
        }
    }

    /// A comment starts here; in a shell only where a word could start.
    fn at_comment(&self) -> bool {
        self.family
            .line_comment()
            .is_some_and(|prefix| self.rest.starts_with(prefix))
            && (self.family != Family::Shell
                || self.line_start
                || self.previous.is_none_or(char::is_whitespace))
    }

    /// The length of the string starting here: triple quotes in Python,
    /// backslash escapes, and up to the line's end for an unclosed one-line
    /// string.
    fn string_len(&self, quote: char) -> usize {
        let triple: String = std::iter::repeat_n(quote, 3).collect();
        if self.family == Family::Python
            && let Some(body) = self.rest.strip_prefix(triple.as_str())
        {
            return body
                .find(triple.as_str())
                .map_or(self.rest.len(), |offset| offset.saturating_add(6));
        }
        let escapes = quote != '\'' || self.family != Family::Shell;
        let multiline = quote == '`' || self.family == Family::Shell;
        let mut chars = self.rest.char_indices().skip(1);
        while let Some((offset, c)) = chars.next() {
            if c == '\\' && escapes {
                chars.next();
            } else if c == quote {
                return offset.saturating_add(c.len_utf8());
            } else if c == '\n' && !multiline {
                return offset;
            }
        }
        self.rest.len()
    }

    fn word(&mut self) {
        let len = Self::prefix_len(self.rest, |c| c.is_alphanumeric() || c == '_' || c == '$');
        let word = self.rest.get(..len).unwrap_or_default();
        let next = self
            .rest
            .get(len..)
            .unwrap_or_default()
            .trim_start_matches([' ', '\t']);
        let token = if self.family.keywords().contains(&word) {
            Some(Token::Keyword)
        } else if self.family.literals().contains(&word) {
            Some(Token::Literal)
        } else if self.family == Family::Toml
            && self.line_start
            && (next.starts_with('=') || next.starts_with('.'))
        {
            Some(Token::Key)
        } else if next.starts_with('(') {
            Some(Token::Function)
        } else {
            None
        };
        self.line_start = false;
        self.emit(token, len);
    }

    /// A shell word: the line's command, an option, a variable, or plain.
    fn shell_word(&mut self, c: char) {
        if c == '$' {
            let name = self.rest.get(1..).map_or(0, |after| {
                Self::prefix_len(after, |c| {
                    c.is_alphanumeric() || matches!(c, '_' | '{' | '}')
                })
            });
            self.line_start = false;
            self.emit(Some(Token::Variable), name.saturating_add(1));
            return;
        }
        let len = Self::prefix_len(self.rest, |c| {
            !c.is_whitespace() && !matches!(c, '"' | '\'' | ';' | '|' | '&' | '(' | ')')
        })
        .max(c.len_utf8());
        let word = self.rest.get(..len).unwrap_or_default();
        let token = if self.line_start
            && !matches!(c, '\\' | '|' | '&' | ';' | '(' | ')')
            && !word.contains('=')
        {
            Some(if self.family.keywords().contains(&word) {
                Token::Keyword
            } else {
                Token::Function
            })
        } else if word.starts_with('-') && word.len() > 1 {
            Some(Token::Key)
        } else {
            None
        };
        // After a pipe or `&&` the next word is a command again.
        self.line_start = matches!(word, "|" | "||" | "&&" | ";" | "sudo" | "env");
        self.emit(token, len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(html: &str) -> String {
        let mut text = String::new();
        let mut in_tag = false;
        for c in html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => text.push(c),
                _ => {}
            }
        }
        text.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    #[test]
    fn colouring_never_changes_the_text() {
        let samples = [
            (
                "python",
                "from tilo import Tilo\n\nws = tilo.workspaces.create(\"Jane's \\\"form\\\"\")  # one per customer\nprint(f'{ws.id}', 3.5, None)\n\"\"\"doc\nstring\"\"\"\n",
            ),
            (
                "typescript",
                "import { Tilo } from \"@tilo/sdk\";\nconst ws = await tilo.workspaces.create(`Jane ${n}`); // ok\nif (a < b && c > d) {}\n",
            ),
            (
                "sh",
                "export TILO_API_KEY=tilo_sk_… # yours\ncurl -fsSL https://tilo.sh | sh\ntilo -w jane do 'pdf-viewer#read' && echo \"$HOME\"\n",
            ),
            (
                "json",
                "{\"id\": \"ws_1\", \"n\": 2, \"ok\": true, \"x\": null}",
            ),
            ("toml", "[[page]]\nslug = \"x\" # c\norder = 3\n"),
            ("text", "<b>&</b>"),
        ];
        for (language, code) in samples {
            assert_eq!(plain(&highlight(language, code)), code, "{language}");
        }
    }

    #[test]
    fn tokens_get_their_classes() {
        let html = highlight("python", "def f(x):\n    return \"s\"  # c\n");
        assert!(html.contains("<span class=\"k\">def</span>"), "{html}");
        assert!(html.contains("<span class=\"f\">f</span>"), "{html}");
        assert!(
            html.contains("<span class=\"s\">&quot;s&quot;</span>"),
            "{html}"
        );
        assert!(html.contains("<span class=\"c\"># c</span>"), "{html}");
        let shell = highlight("sh", "tilo keys create \"Visa\" --expires-in 60 # note\n");
        assert!(shell.contains("<span class=\"f\">tilo</span>"), "{shell}");
        assert!(
            shell.contains("<span class=\"a\">--expires-in</span>"),
            "{shell}"
        );
        assert!(shell.contains("<span class=\"c\"># note</span>"), "{shell}");
        let json = highlight("json", "{\"id\": \"x\"}");
        assert!(
            json.contains("<span class=\"a\">&quot;id&quot;</span>"),
            "{json}"
        );
    }

    #[test]
    fn a_hash_inside_a_shell_word_is_not_a_comment() {
        let shell = highlight("sh", "tilo do pdf-viewer#read-form-fields\n");
        assert!(!shell.contains("class=\"c\""), "{shell}");
    }
}
