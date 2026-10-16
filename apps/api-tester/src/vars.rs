//! Variable substitution and JSON presentation helpers.
//!
//! Two concerns live here:
//!
//! 1. `{{variable}}` interpolation — the active environment's variables
//!    are spliced into the URL, headers, params, and body just before a
//!    request goes out. [`substitute`] is deliberately tolerant: an
//!    unknown variable is left verbatim so the user can see exactly which
//!    placeholder didn't resolve.
//! 2. JSON presentation — pretty-printing plus a flat tokenizer that the
//!    response viewer renders as syntax-highlighted, collapsible lines.

use std::collections::HashMap;

use crate::model::KeyValue;

/// Replace every `{{name}}` occurrence in `input` with the matching value
/// from `vars`. Whitespace inside the braces is tolerated (`{{ name }}`).
/// Unknown names are left untouched so the placeholder stays visible.
pub fn substitute(input: &str, vars: &HashMap<String, String>) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes.get(i) == Some(&b'{')
            && bytes.get(i + 1) == Some(&b'{')
            && let Some(rest) = input.get(i + 2..)
            && let Some(close) = rest.find("}}")
        {
            if let Some(raw) = input.get(i + 2..i + 2 + close) {
                let name = raw.trim();
                if let Some(val) = vars.get(name) {
                    out.push_str(val);
                } else if let Some(verbatim) = input.get(i..i + 2 + close + 2) {
                    // Keep the placeholder verbatim — unresolved is visible.
                    out.push_str(verbatim);
                }
            }
            i += 2 + close + 2;
            continue;
        }
        // Push the next char (UTF-8 safe).
        let Some(&b) = bytes.get(i) else { break };
        let ch_len = utf8_len(b);
        if let Some(chunk) = input.get(i..i + ch_len) {
            out.push_str(chunk);
        }
        i += ch_len;
    }
    out
}

/// Byte length of a UTF-8 char given its leading byte.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// Collect the distinct `{{name}}` references in a string, in first-seen
/// order. Powers the "unresolved variable" hinting in the URL bar.
pub fn referenced_vars(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = input;
    while let Some(open) = rest.find("{{") {
        let Some(after) = rest.get(open + 2..) else {
            break;
        };
        if let Some(close) = after.find("}}") {
            let Some(name_str) = after.get(..close) else {
                break;
            };
            let name = name_str.trim().to_string();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
            let Some(next) = after.get(close + 2..) else {
                break;
            };
            rest = next;
        } else {
            break;
        }
    }
    out
}

/// Build a name→value lookup from a list of enabled variable rows.
pub fn var_map(vars: &[KeyValue]) -> HashMap<String, String> {
    vars.iter()
        .filter(|v| v.enabled && !v.key.trim().is_empty())
        .map(|v| (v.key.trim().to_string(), v.value.clone()))
        .collect()
}

/// Pretty-print a JSON string with two-space indentation. Returns `None`
/// if the input isn't valid JSON (the caller then shows it raw).
pub fn pretty_json(input: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(input).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

/// A single coloured token kind within a rendered JSON line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// Object key (before the colon).
    Key,
    /// String value.
    StringValue,
    /// Number value.
    Number,
    /// `true` / `false` / `null`.
    Keyword,
    /// Structural punctuation: braces, brackets, commas, colons.
    Punctuation,
    /// Indentation / whitespace.
    Plain,
}

/// A coloured run of text on a rendered line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub kind: TokenKind,
}

/// One physical line of pretty-printed JSON, decomposed into tokens for
/// syntax highlighting, plus structural metadata used for the collapse
/// affordance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonLine {
    pub tokens: Vec<Token>,
    /// Nesting depth (number of open containers above this line).
    pub depth: usize,
    /// `true` if this line opens a container (`{` or `[` is the last
    /// significant char) — the viewer shows a fold caret here.
    pub opens_block: bool,
    /// Index of the matching closing line for an opener, else `None`.
    pub close_line: Option<usize>,
}

/// Tokenize pretty-printed JSON text into highlighted, fold-aware lines.
/// The input is expected to already be pretty-printed (see
/// [`pretty_json`]); each physical line becomes one [`JsonLine`].
pub fn tokenize_json(pretty: &str) -> Vec<JsonLine> {
    let mut lines: Vec<JsonLine> = pretty.lines().map(tokenize_line).collect();

    // Second pass: match each opener line to its closer using a stack of
    // line indices, so the viewer can collapse a block to its first line.
    let mut stack: Vec<usize> = Vec::new();
    for idx in 0..lines.len() {
        if lines.get(idx).is_some_and(|l| l.opens_block) {
            stack.push(idx);
        }
        let closes = lines.get(idx).is_some_and(line_closes_block);
        if closes
            && let Some(open_idx) = stack.pop()
            && open_idx != idx
            && let Some(opener) = lines.get_mut(open_idx)
        {
            opener.close_line = Some(idx);
        }
    }
    lines
}

/// A closing line starts with a closer, possibly followed by a comma.
/// Inline empty containers must not close an enclosing multiline block.
fn line_closes_block(line: &JsonLine) -> bool {
    line.tokens
        .iter()
        .find(|token| !token.text.trim().is_empty())
        .is_some_and(|token| {
            token.kind == TokenKind::Punctuation && matches!(token.text.as_str(), "}" | "]")
        })
}

/// Tokenize a single physical line of pretty JSON.
fn tokenize_line(raw: &str) -> JsonLine {
    let indent_len = raw.len() - raw.trim_start().len();
    let depth = indent_len / 2;
    let mut tokens = Vec::new();
    push_indent_token(&mut tokens, raw, indent_len);
    let content = raw.get(indent_len..).unwrap_or("");
    tokenize_content(content, &mut tokens);

    // A line "opens a block" if its last non-whitespace structural token
    // is an opening brace/bracket.
    let opens = line_opens_block(&tokens);

    JsonLine {
        tokens,
        depth,
        opens_block: opens,
        close_line: None,
    }
}

fn push_indent_token(tokens: &mut Vec<Token>, raw: &str, indent_len: usize) {
    if indent_len > 0
        && let Some(indent) = raw.get(..indent_len)
    {
        tokens.push(Token {
            text: indent.to_string(),
            kind: TokenKind::Plain,
        });
    }
}

/// Walk `content`'s characters, pushing one [`Token`] per string / structural
/// character / run of whitespace or plain text.
fn tokenize_content(content: &str, tokens: &mut Vec<Token>) {
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;
    // Track whether the next string we hit is a key (a colon follows it).
    while let Some(&c) = chars.get(i) {
        match c {
            '"' => tokenize_quoted_string(&chars, &mut i, tokens),
            '{' | '}' | '[' | ']' | ':' | ',' => {
                tokens.push(Token {
                    text: c.to_string(),
                    kind: TokenKind::Punctuation,
                });
                i += 1;
            }
            ' ' => {
                tokens.push(Token {
                    text: " ".to_string(),
                    kind: TokenKind::Plain,
                });
                i += 1;
            }
            _ => tokenize_word(&chars, &mut i, tokens),
        }
    }
}

/// Consume a full quoted string (handling escapes) starting at `chars[*i]`,
/// classifying it as a key (a colon follows, after optional spaces) or a
/// plain string value.
fn tokenize_quoted_string(chars: &[char], i: &mut usize, tokens: &mut Vec<Token>) {
    let start = *i;
    *i += 1;
    while let Some(&cc) = chars.get(*i) {
        if cc == '\\' {
            *i += 2;
            continue;
        }
        *i += 1;
        if cc == '"' {
            break;
        }
    }
    let end = (*i).min(chars.len());
    let s: String = chars.get(start..end).unwrap_or_default().iter().collect();
    let mut j = *i;
    while chars.get(j) == Some(&' ') {
        j += 1;
    }
    let is_key = chars.get(j) == Some(&':');
    tokens.push(Token {
        text: s,
        kind: if is_key {
            TokenKind::Key
        } else {
            TokenKind::StringValue
        },
    });
}

/// Number, keyword, or anything else: consume up to the next structural
/// delimiter starting at `chars[*i]`.
fn tokenize_word(chars: &[char], i: &mut usize, tokens: &mut Vec<Token>) {
    let start = *i;
    while chars
        .get(*i)
        .is_some_and(|&ch| !matches!(ch, '{' | '}' | '[' | ']' | ':' | ',' | ' ' | '"'))
    {
        *i += 1;
    }
    let word: String = chars.get(start..*i).unwrap_or_default().iter().collect();
    let kind = if word == "true" || word == "false" || word == "null" {
        TokenKind::Keyword
    } else if word
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
        && !word.is_empty()
    {
        TokenKind::Number
    } else {
        TokenKind::Plain
    };
    tokens.push(Token { text: word, kind });
}

fn line_opens_block(tokens: &[Token]) -> bool {
    tokens
        .iter()
        .rev()
        .find(|token| !token.text.trim().is_empty())
        .is_some_and(|token| {
            token.kind == TokenKind::Punctuation && matches!(token.text.as_str(), "{" | "[")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn placeholders_substitute_and_are_referenced_in_order() {
        // Case "known and unknown": known keys are replaced, unknown kept.
        let v = vars(&[("base_url", "https://api.example.com"), ("ver", "v2")]);
        let out = substitute("{{base_url}}/{{ver}}/users/{{missing}}", &v);
        assert_eq!(
            out, "https://api.example.com/v2/users/{{missing}}",
            "case known and unknown"
        );

        // Case "inner whitespace": tolerated inside the braces.
        let v = vars(&[("token", "abc")]);
        assert_eq!(
            substitute("Bearer {{ token }}", &v),
            "Bearer abc",
            "case inner whitespace"
        );

        // Case "unicode": text around placeholders survives.
        let v = vars(&[("name", "wörld")]);
        assert_eq!(
            substitute("héllo {{name}} ☃", &v),
            "héllo wörld ☃",
            "case unicode"
        );

        // Case "referenced vars": deduped in first-seen order.
        let refs = referenced_vars("{{a}}/{{b}}/{{a}}");
        assert_eq!(
            refs,
            vec!["a".to_string(), "b".to_string()],
            "case referenced vars"
        );
    }

    #[test]
    fn json_is_pretty_printed_and_tokenized_with_matched_blocks() {
        // Case "pretty": valid input is indented, invalid input rejected.
        let pretty = pretty_json(r#"{"a":1,"b":[2,3]}"#).unwrap();
        assert!(pretty.contains("\n  \"a\": 1"), "case pretty");
        assert!(pretty_json("not json").is_none(), "case pretty invalid");

        // Case "tokenize": classifies keys, values, numbers and keywords.
        let pretty = pretty_json(r#"{"name":"Ada","age":36,"active":true}"#).unwrap();
        let lines = tokenize_json(&pretty);
        // First line opens the object.
        assert!(lines[0].opens_block, "case tokenize");
        assert_eq!(lines[0].close_line, Some(lines.len() - 1), "case tokenize");
        // Find the key token "name".
        let key_line = &lines[1];
        assert!(
            key_line
                .tokens
                .iter()
                .any(|t| t.kind == TokenKind::Key && t.text.contains("name")),
            "case tokenize key"
        );
        assert!(
            key_line
                .tokens
                .iter()
                .any(|t| t.kind == TokenKind::StringValue && t.text.contains("Ada")),
            "case tokenize string value"
        );
        // The age line carries a number.
        let has_number = lines.iter().any(|l| {
            l.tokens
                .iter()
                .any(|t| t.kind == TokenKind::Number && t.text == "36")
        });
        assert!(has_number, "case tokenize number");
        // The active line carries the `true` keyword.
        let has_keyword = lines.iter().any(|l| {
            l.tokens
                .iter()
                .any(|t| t.kind == TokenKind::Keyword && t.text == "true")
        });
        assert!(has_keyword, "case tokenize keyword");

        // Case "nested blocks": every opener resolves a matching closer.
        let pretty = pretty_json(r#"{"outer":{"inner":[1,2]}}"#).unwrap();
        let lines = tokenize_json(&pretty);
        let openers: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.opens_block)
            .map(|(i, _)| i)
            .collect();
        for o in openers {
            assert!(
                lines[o].close_line.is_some(),
                "nested: opener {o} has no closer"
            );
        }
    }
}
