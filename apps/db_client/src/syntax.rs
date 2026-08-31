//! A small SQL tokenizer used to colour the editor. It is intentionally
//! lexical (no parsing): it walks the source once and emits typed spans the
//! editor maps to theme colours. Good enough to make queries readable; it
//! never rejects input, so partial/invalid SQL still highlights gracefully.

/// The lexical class of a highlighted span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    Keyword,
    Function,
    Type,
    Number,
    String,
    Comment,
    Operator,
    Punct,
    Identifier,
    Whitespace,
}

/// One contiguous run of source text with a single class.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub kind: Tok,
}

const KEYWORDS: &[&str] = &[
    "select",
    "from",
    "where",
    "join",
    "inner",
    "left",
    "right",
    "full",
    "outer",
    "on",
    "as",
    "group",
    "by",
    "order",
    "having",
    "limit",
    "offset",
    "insert",
    "into",
    "values",
    "update",
    "set",
    "delete",
    "create",
    "table",
    "view",
    "index",
    "drop",
    "alter",
    "add",
    "column",
    "primary",
    "key",
    "foreign",
    "references",
    "unique",
    "not",
    "null",
    "default",
    "and",
    "or",
    "in",
    "like",
    "between",
    "is",
    "distinct",
    "all",
    "union",
    "case",
    "when",
    "then",
    "else",
    "end",
    "asc",
    "desc",
    "exists",
    "pragma",
    "begin",
    "commit",
    "rollback",
    "with",
    "using",
    "constraint",
    "cascade",
    "check",
    "autoincrement",
];

const FUNCTIONS: &[&str] = &[
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "round",
    "coalesce",
    "ifnull",
    "length",
    "lower",
    "upper",
    "substr",
    "substring",
    "trim",
    "replace",
    "abs",
    "cast",
    "date",
    "time",
    "datetime",
    "strftime",
    "now",
    "group_concat",
    "total",
    "nullif",
    "instr",
];

const TYPES: &[&str] = &[
    "integer",
    "int",
    "bigint",
    "smallint",
    "text",
    "varchar",
    "char",
    "blob",
    "real",
    "float",
    "double",
    "numeric",
    "decimal",
    "boolean",
    "bool",
    "date",
    "datetime",
    "timestamp",
    "time",
    "serial",
];

fn classify_word(lower: &str) -> Tok {
    if KEYWORDS.contains(&lower) {
        Tok::Keyword
    } else if FUNCTIONS.contains(&lower) {
        Tok::Function
    } else if TYPES.contains(&lower) {
        Tok::Type
    } else {
        Tok::Identifier
    }
}

/// `chars[idx]`, but a scan that walks past the end reports "no more input"
/// instead of panicking.
fn char_at(chars: &[char], idx: usize) -> Option<char> {
    chars.get(idx).copied()
}

/// `chars[start..end]` collected into a `String`; an out-of-range span (which
/// the scan loops below never actually produce) collects as empty rather than
/// panicking.
fn slice_text(chars: &[char], start: usize, end: usize) -> String {
    chars.get(start..end).unwrap_or_default().iter().collect()
}

/// Line comment `-- ...`, up to (not including) the newline or end of input.
fn scan_line_comment(chars: &[char], start: usize, n: usize) -> (usize, Span) {
    let mut i = start;
    while i < n && char_at(chars, i) != Some('\n') {
        i += 1;
    }
    (
        i,
        Span {
            text: slice_text(chars, start, i),
            kind: Tok::Comment,
        },
    )
}

/// Block comment `/* ... */`; an unterminated comment runs to end of input.
fn scan_block_comment(chars: &[char], start: usize, n: usize) -> (usize, Span) {
    let mut i = start;
    i += 2;
    while i + 1 < n && !(char_at(chars, i) == Some('*') && char_at(chars, i + 1) == Some('/')) {
        i += 1;
    }
    i = (i + 2).min(n);
    (
        i,
        Span {
            text: slice_text(chars, start, i),
            kind: Tok::Comment,
        },
    )
}

/// String literal opened by `quote` (single- or double-quoted); a doubled
/// quote is an escaped quote and stays inside the literal.
fn scan_string(chars: &[char], start: usize, n: usize, quote: char) -> (usize, Span) {
    let mut i = start;
    i += 1;
    while i < n {
        if char_at(chars, i) == Some(quote) {
            if char_at(chars, i + 1) == Some(quote) {
                i += 2;
                continue;
            }
            i += 1;
            break;
        }
        i += 1;
    }
    (
        i,
        Span {
            text: slice_text(chars, start, i),
            kind: Tok::String,
        },
    )
}

fn scan_whitespace(chars: &[char], start: usize, n: usize) -> (usize, Span) {
    let mut i = start;
    while i < n && char_at(chars, i).is_some_and(char::is_whitespace) {
        i += 1;
    }
    (
        i,
        Span {
            text: slice_text(chars, start, i),
            kind: Tok::Whitespace,
        },
    )
}

/// Integers and decimals.
fn scan_number(chars: &[char], start: usize, n: usize) -> (usize, Span) {
    let mut i = start;
    while i < n && char_at(chars, i).is_some_and(|ch| ch.is_ascii_digit() || ch == '.') {
        i += 1;
    }
    (
        i,
        Span {
            text: slice_text(chars, start, i),
            kind: Tok::Number,
        },
    )
}

/// Identifiers / keywords / functions / types.
fn scan_word(chars: &[char], start: usize, n: usize) -> (usize, Span) {
    let mut i = start;
    while i < n && char_at(chars, i).is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
        i += 1;
    }
    let word = slice_text(chars, start, i);
    let lower = word.to_ascii_lowercase();
    let kind = classify_word(&lower);
    (i, Span { text: word, kind })
}

fn scan_operator(chars: &[char], start: usize, n: usize) -> (usize, Span) {
    let mut i = start;
    while i < n && char_at(chars, i).is_some_and(|ch| "+-*/%<>=!|&".contains(ch)) {
        i += 1;
    }
    (
        i,
        Span {
            text: slice_text(chars, start, i),
            kind: Tok::Operator,
        },
    )
}

/// Tokenize `src` into colour spans covering every byte exactly once.
pub fn tokenize(src: &str) -> Vec<Span> {
    let chars: Vec<char> = src.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    let n = chars.len();

    while i < n {
        let Some(c) = char_at(&chars, i) else { break };

        if c == '-' && char_at(&chars, i + 1) == Some('-') {
            let (next_i, span) = scan_line_comment(&chars, i, n);
            i = next_i;
            spans.push(span);
            continue;
        }

        if c == '/' && char_at(&chars, i + 1) == Some('*') {
            let (next_i, span) = scan_block_comment(&chars, i, n);
            i = next_i;
            spans.push(span);
            continue;
        }

        // Single-quoted (with '' escape) or double-quoted identifiers — we
        // colour both as strings for simplicity.
        if c == '\'' || c == '"' {
            let (next_i, span) = scan_string(&chars, i, n, c);
            i = next_i;
            spans.push(span);
            continue;
        }

        if c.is_whitespace() {
            let (next_i, span) = scan_whitespace(&chars, i, n);
            i = next_i;
            spans.push(span);
            continue;
        }

        if c.is_ascii_digit() {
            let (next_i, span) = scan_number(&chars, i, n);
            i = next_i;
            spans.push(span);
            continue;
        }

        if c.is_alphabetic() || c == '_' {
            let (next_i, span) = scan_word(&chars, i, n);
            i = next_i;
            spans.push(span);
            continue;
        }

        if "+-*/%<>=!|&".contains(c) {
            let (next_i, span) = scan_operator(&chars, i, n);
            i = next_i;
            spans.push(span);
            continue;
        }

        // Everything else is punctuation (parens, commas, dots, semicolons).
        spans.push(Span {
            text: c.to_string(),
            kind: Tok::Punct,
        });
        i += 1;
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        tokenize(src).into_iter().map(|s| s.kind).collect()
    }

    #[test]
    fn spans_cover_input_exactly() {
        let src = "SELECT id FROM t WHERE x = 1; -- note";
        let joined: String = tokenize(src).into_iter().map(|s| s.text).collect();
        assert_eq!(joined, src);
    }

    #[test]
    fn keywords_and_functions_classified() {
        let spans = tokenize("SELECT count(*) FROM t");
        assert_eq!(spans[0].kind, Tok::Keyword); // SELECT
        assert!(
            spans
                .iter()
                .any(|s| s.kind == Tok::Function && s.text == "count")
        );
    }

    #[test]
    fn strings_and_comments_consume_to_close() {
        let spans = tokenize("'a''b' /* c */ x");
        assert_eq!(spans[0].kind, Tok::String);
        assert_eq!(spans[0].text, "'a''b'");
        assert!(spans.iter().any(|s| s.kind == Tok::Comment));
    }

    #[test]
    fn numbers_classified() {
        assert!(kinds("3.14").contains(&Tok::Number));
    }
}
