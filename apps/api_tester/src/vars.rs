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
        if i + 1 < bytes.len()
            && bytes[i] == b'{'
            && bytes[i + 1] == b'{'
            && let Some(close) = input[i + 2..].find("}}")
        {
            let raw = &input[i + 2..i + 2 + close];
            let name = raw.trim();
            if let Some(val) = vars.get(name) {
                out.push_str(val);
            } else {
                // Keep the placeholder verbatim — unresolved is visible.
                out.push_str(&input[i..i + 2 + close + 2]);
            }
            i = i + 2 + close + 2;
            continue;
        }
        // Push the next char (UTF-8 safe).
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&input[i..i + ch_len]);
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
        let after = &rest[open + 2..];
        if let Some(close) = after.find("}}") {
            let name = after[..close].trim().to_string();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
            rest = &after[close + 2..];
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
        if lines[idx].opens_block {
            stack.push(idx);
        }
        if line_closes_block(&lines[idx])
            && let Some(open_idx) = stack.pop()
            && open_idx != idx
        {
            lines[open_idx].close_line = Some(idx);
        }
    }
    lines
}

/// Whether a tokenized line's last structural char closes a container.
fn line_closes_block(line: &JsonLine) -> bool {
    line.tokens
        .iter()
        .rev()
        .find_map(|t| {
            let trimmed = t.text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.starts_with('}') || trimmed.starts_with(']'))
            }
        })
        .unwrap_or(false)
}

/// Tokenize a single physical line of pretty JSON.
fn tokenize_line(raw: &str) -> JsonLine {
    let indent_len = raw.len() - raw.trim_start().len();
    let depth = indent_len / 2;
    let mut tokens = Vec::new();
    if indent_len > 0 {
        tokens.push(Token {
            text: raw[..indent_len].to_string(),
            kind: TokenKind::Plain,
        });
    }
    let content = &raw[indent_len..];

    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;
    // Track whether the next string we hit is a key (a colon follows it).
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                // Consume a full quoted string (handling escapes).
                let start = i;
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\\' {
                        i += 2;
                        continue;
                    }
                    if chars[i] == '"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                let s: String = chars[start..i.min(chars.len())].iter().collect();
                // A key is a string immediately followed (after optional
                // spaces) by a colon.
                let mut j = i;
                while j < chars.len() && chars[j] == ' ' {
                    j += 1;
                }
                let is_key = j < chars.len() && chars[j] == ':';
                tokens.push(Token {
                    text: s,
                    kind: if is_key {
                        TokenKind::Key
                    } else {
                        TokenKind::StringValue
                    },
                });
            }
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
            _ => {
                // Number, keyword, or anything else: consume up to the
                // next structural delimiter.
                let start = i;
                while i < chars.len()
                    && !matches!(chars[i], '{' | '}' | '[' | ']' | ':' | ',' | ' ' | '"')
                {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
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
        }
    }

    // A line "opens a block" if its last non-whitespace structural token
    // is an opening brace/bracket.
    let opens = tokens
        .iter()
        .rev()
        .find_map(|t| {
            let trimmed = t.text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.ends_with('{') || trimmed.ends_with('['))
            }
        })
        .unwrap_or(false);

    JsonLine {
        tokens,
        depth,
        opens_block: opens,
        close_line: None,
    }
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
    fn substitute_replaces_known_keeps_unknown() {
        let v = vars(&[("base_url", "https://api.example.com"), ("ver", "v2")]);
        let out = substitute("{{base_url}}/{{ver}}/users/{{missing}}", &v);
        assert_eq!(out, "https://api.example.com/v2/users/{{missing}}");
    }

    #[test]
    fn substitute_tolerates_inner_whitespace() {
        let v = vars(&[("token", "abc")]);
        assert_eq!(substitute("Bearer {{ token }}", &v), "Bearer abc");
    }

    #[test]
    fn substitute_handles_unicode_around_placeholders() {
        let v = vars(&[("name", "wörld")]);
        assert_eq!(substitute("héllo {{name}} ☃", &v), "héllo wörld ☃");
    }

    #[test]
    fn referenced_vars_are_deduped_in_order() {
        let refs = referenced_vars("{{a}}/{{b}}/{{a}}");
        assert_eq!(refs, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn pretty_json_indents_valid_input() {
        let pretty = pretty_json(r#"{"a":1,"b":[2,3]}"#).unwrap();
        assert!(pretty.contains("\n  \"a\": 1"));
        assert!(pretty_json("not json").is_none());
    }

    #[test]
    fn tokenize_classifies_keys_values_numbers_keywords() {
        let pretty = pretty_json(r#"{"name":"Ada","age":36,"active":true}"#).unwrap();
        let lines = tokenize_json(&pretty);
        // First line opens the object.
        assert!(lines[0].opens_block);
        assert_eq!(lines[0].close_line, Some(lines.len() - 1));
        // Find the key token "name".
        let key_line = &lines[1];
        assert!(
            key_line
                .tokens
                .iter()
                .any(|t| t.kind == TokenKind::Key && t.text.contains("name"))
        );
        assert!(
            key_line
                .tokens
                .iter()
                .any(|t| t.kind == TokenKind::StringValue && t.text.contains("Ada"))
        );
        // The age line carries a number.
        let has_number = lines.iter().any(|l| {
            l.tokens
                .iter()
                .any(|t| t.kind == TokenKind::Number && t.text == "36")
        });
        assert!(has_number);
        // The active line carries the `true` keyword.
        let has_keyword = lines.iter().any(|l| {
            l.tokens
                .iter()
                .any(|t| t.kind == TokenKind::Keyword && t.text == "true")
        });
        assert!(has_keyword);
    }

    #[test]
    fn nested_blocks_track_their_closers() {
        let pretty = pretty_json(r#"{"outer":{"inner":[1,2]}}"#).unwrap();
        let lines = tokenize_json(&pretty);
        // The opener line(s) should each resolve a matching closer.
        let openers: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.opens_block)
            .map(|(i, _)| i)
            .collect();
        for o in openers {
            assert!(lines[o].close_line.is_some(), "opener {o} has no closer");
        }
    }
}
