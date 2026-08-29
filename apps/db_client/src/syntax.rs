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

/// Tokenize `src` into colour spans covering every byte exactly once.
pub fn tokenize(src: &str) -> Vec<Span> {
    let chars: Vec<char> = src.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    let n = chars.len();

    while i < n {
        let c = chars[i];

        // Line comment `-- ...`
        if c == '-' && i + 1 < n && chars[i + 1] == '-' {
            let start = i;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            spans.push(Span {
                text: chars[start..i].iter().collect(),
                kind: Tok::Comment,
            });
            continue;
        }

        // Block comment `/* ... */`
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(n);
            spans.push(Span {
                text: chars[start..i].iter().collect(),
                kind: Tok::Comment,
            });
            continue;
        }

        // String literals: single-quoted (with '' escape) or double-quoted
        // identifiers — we colour both as strings for simplicity.
        if c == '\'' || c == '"' {
            let quote = c;
            let start = i;
            i += 1;
            while i < n {
                if chars[i] == quote {
                    // Doubled quote = escaped quote, stay inside the literal.
                    if i + 1 < n && chars[i + 1] == quote {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            spans.push(Span {
                text: chars[start..i].iter().collect(),
                kind: Tok::String,
            });
            continue;
        }

        // Whitespace run.
        if c.is_whitespace() {
            let start = i;
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            spans.push(Span {
                text: chars[start..i].iter().collect(),
                kind: Tok::Whitespace,
            });
            continue;
        }

        // Numbers (integers, decimals).
        if c.is_ascii_digit() {
            let start = i;
            while i < n && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            spans.push(Span {
                text: chars[start..i].iter().collect(),
                kind: Tok::Number,
            });
            continue;
        }

        // Words: identifiers / keywords / functions / types.
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let lower = word.to_ascii_lowercase();
            spans.push(Span {
                text: word,
                kind: classify_word(&lower),
            });
            continue;
        }

        // Operators.
        if "+-*/%<>=!|&".contains(c) {
            let start = i;
            while i < n && "+-*/%<>=!|&".contains(chars[i]) {
                i += 1;
            }
            spans.push(Span {
                text: chars[start..i].iter().collect(),
                kind: Tok::Operator,
            });
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
