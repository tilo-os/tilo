//! A lightweight, language-agnostic linter that powers the Problems panel.
//!
//! This is intentionally *not* a language server — real diagnostics from an
//! LSP are deferred (see the TODO in `app.rs`). Instead it runs a handful of
//! cheap, genuinely-useful textual checks that hold for most code: trailing
//! whitespace, tab/space mixing, over-long lines, lonely `TODO`/`FIXME`
//! markers, and unbalanced brackets. The result is real, position-accurate
//! feedback the user can click through, with zero external process.

use crate::bottom_panel::{Diagnostic, Severity};
use crate::model::Language;

/// Run the linter over `text` for a given language. Returns diagnostics sorted
/// by (line, col).
pub fn lint(text: &str, language: Language) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let max_len = match language {
        Language::Rust => 100,
        _ => 120,
    };

    let mut bracket_stack: Vec<(char, usize, usize)> = Vec::new();
    let mut in_block_comment = false;

    for (li, line) in text.split('\n').enumerate() {
        let line_no = li + 1;
        let char_len = line.chars().count();

        // Trailing whitespace.
        if line.ends_with(' ') || line.ends_with('\t') {
            let trimmed = line.trim_end();
            out.push(Diagnostic {
                severity: Severity::Hint,
                line: line_no,
                col: trimmed.chars().count() + 1,
                message: "Trailing whitespace".to_string(),
            });
        }

        // Mixed leading tabs + spaces.
        let leading: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        if leading.contains('\t') && leading.contains(' ') {
            out.push(Diagnostic {
                severity: Severity::Warning,
                line: line_no,
                col: 1,
                message: "Mixed tabs and spaces in indentation".to_string(),
            });
        }

        // Over-long line.
        if char_len > max_len {
            out.push(Diagnostic {
                severity: Severity::Info,
                line: line_no,
                col: max_len + 1,
                message: format!("Line exceeds {} characters ({})", max_len, char_len),
            });
        }

        // TODO / FIXME markers.
        if let Some(byte) = line.find("TODO").or_else(|| line.find("FIXME")) {
            let col = line[..byte].chars().count() + 1;
            let marker = if line[byte..].starts_with("TODO") { "TODO" } else { "FIXME" };
            out.push(Diagnostic {
                severity: Severity::Info,
                line: line_no,
                col,
                message: format!("Unresolved {} marker", marker),
            });
        }

        // Bracket balance, skipping string/char/comment-ish content crudely.
        // Toggle a naive block-comment tracker for C-family languages.
        let mut chars = line.char_indices().peekable();
        let mut in_string = false;
        let mut string_delim = '"';
        while let Some((bi, ch)) = chars.next() {
            if in_block_comment {
                if ch == '*' {
                    if let Some(&(_, '/')) = chars.peek() {
                        in_block_comment = false;
                        chars.next();
                    }
                }
                continue;
            }
            if in_string {
                if ch == '\\' {
                    chars.next();
                } else if ch == string_delim {
                    in_string = false;
                }
                continue;
            }
            match ch {
                '"' | '\'' => {
                    in_string = true;
                    string_delim = ch;
                }
                '/' => {
                    if let Some(&(_, '/')) = chars.peek() {
                        break; // line comment — ignore the rest
                    }
                    if let Some(&(_, '*')) = chars.peek() {
                        in_block_comment = true;
                        chars.next();
                    }
                }
                '#' if matches!(
                    language,
                    Language::Python | Language::Shell | Language::Toml | Language::Yaml
                ) =>
                {
                    break; // hash line comment
                }
                '(' | '[' | '{' => {
                    let col = line[..bi].chars().count() + 1;
                    bracket_stack.push((ch, line_no, col));
                }
                ')' | ']' | '}' => {
                    let expected = match ch {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    match bracket_stack.last() {
                        Some(&(open, _, _)) if open == expected => {
                            bracket_stack.pop();
                        }
                        _ => {
                            let col = line[..bi].chars().count() + 1;
                            out.push(Diagnostic {
                                severity: Severity::Warning,
                                line: line_no,
                                col,
                                message: format!("Unmatched closing '{}'", ch),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Any brackets left open at EOF.
    for (open, line, col) in bracket_stack {
        out.push(Diagnostic {
            severity: Severity::Warning,
            line,
            col,
            message: format!("Unclosed '{}'", open),
        });
    }

    out.sort_by(|a, b| (a.line, a.col).cmp(&(b.line, b.col)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_trailing_whitespace() {
        let d = lint("let x = 1;   \n", Language::Rust);
        assert!(d.iter().any(|x| x.message.contains("Trailing whitespace")));
    }

    #[test]
    fn detects_unclosed_bracket() {
        let d = lint("fn main() {\n", Language::Rust);
        assert!(d.iter().any(|x| x.message.contains("Unclosed '{'")));
    }

    #[test]
    fn balanced_brackets_have_no_bracket_diagnostics() {
        let d = lint("fn main() {}\n", Language::Rust);
        assert!(!d.iter().any(|x| x.message.contains("Unclosed")));
        assert!(!d.iter().any(|x| x.message.contains("Unmatched")));
    }

    #[test]
    fn ignores_brackets_in_strings() {
        let d = lint("let s = \"{[(\";\n", Language::Rust);
        assert!(!d.iter().any(|x| x.message.contains("Unclosed")));
    }

    #[test]
    fn detects_todo_marker() {
        let d = lint("// TODO: fix this\n", Language::Rust);
        assert!(d.iter().any(|x| x.message.contains("TODO")));
    }

    #[test]
    fn ignores_brackets_after_line_comment() {
        let d = lint("let x = 1; // a comment with ( unbalanced\n", Language::Rust);
        assert!(!d.iter().any(|x| x.message.contains("Unclosed")));
    }
}
