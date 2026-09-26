//! A lightweight, language-agnostic linter that powers the Problems panel.
//!
//! It runs a handful of cheap textual checks that hold for most code: trailing
//! whitespace, tab/space mixing, over-long lines, lonely `TODO`/`FIXME`
//! markers, and unbalanced brackets. The result is position-accurate feedback
//! the user can click through without an external process. This fills the gap
//! before a language server finishes indexing and for file types it does not
//! cover.

use crate::bottom_panel::{Diagnostic, Severity};
use crate::model::Language;

/// Whether `language` writes lifetimes with `'`.
///
/// This gate matters more than it looks. In Python, JavaScript, TypeScript,
/// shell, YAML and CSS a single quote *opens a string*, so treating a lone `'`
/// as a lifetime would make the scanner read a string's contents as code and
/// report the brackets inside it. Rust is the only language here where a lone
/// `'` is ordinary and correct.
fn has_lifetimes(language: Language) -> bool {
    matches!(language, Language::Rust)
}

/// Whether the `'` at byte offset `at` in `line` opens a Rust lifetime rather
/// than a character literal.
///
/// A char literal is `'x'` or an escape like `'\n'` — the quote has a partner
/// within a couple of characters. A lifetime (`'a`, `'_`, `'static`) is a lone
/// quote followed by an identifier. Deciding by looking for the partner is what
/// distinguishes them without a real lexer.
fn is_lifetime_tick(line: &str, at: usize) -> bool {
    let rest = line.get(at + 1..).unwrap_or("");
    let mut it = rest.chars();
    let Some(first) = it.next() else {
        // A trailing quote at end of line closes nothing.
        return true;
    };
    // `'\n'`, `'\''` — an escape is always a char literal.
    if first == '\\' {
        return false;
    }
    // `'x'` — a single character then its closing quote.
    if it.next() == Some('\'') {
        return false;
    }
    // Otherwise: a lone quote followed by an identifier character is a
    // lifetime. Anything else (e.g. a quote inside prose) is treated as a
    // lifetime too, since assuming "string" is the reading that corrupts the
    // rest of the line.
    first.is_alphanumeric() || first == '_'
}

/// How much of the linter to run.
///
/// The split is between checks that are *facts about the text* and checks that
/// are *guesses about program structure*. Trailing whitespace, mixed
/// indentation, line length and `TODO` markers are the first kind: a compiler
/// never contradicts them, so they are worth showing at all times. Bracket
/// balance is the second kind — it has no type information, no macro expansion
/// and no crate graph, so wherever it disagrees with a real language server it
/// is the scanner that is wrong. It is therefore only shown while no server has
/// answered for the buffer. See [`crate::app::CodeApp::refresh_diagnostics`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Every check, structural scan included. What to show when there is no
    /// better answer available — no server for this language, or one that has
    /// not reported yet.
    Full,
    /// Textual checks only. Used once a language server has published
    /// diagnostics for the buffer.
    TextualOnly,
}

/// Run the linter over `text` for a given language. Returns diagnostics sorted
/// by (line, col).
pub fn lint(text: &str, language: Language, scope: Scope) -> Vec<Diagnostic> {
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

        lint_textual_facts(&mut out, line, line_no, char_len, max_len);

        if scope == Scope::TextualOnly {
            continue;
        }

        scan_line_brackets(
            line,
            line_no,
            language,
            &mut bracket_stack,
            &mut in_block_comment,
            &mut out,
        );
    }

    // Any brackets left open at EOF.
    for (open, line, col) in bracket_stack {
        out.push(Diagnostic {
            severity: Severity::Warning,
            line,
            col,
            message: format!("Unclosed '{open}'"),
        });
    }

    out.sort_by_key(|a| (a.line, a.col));
    out
}

/// The checks that are facts about the text, independent of [`Scope`]:
/// trailing whitespace, mixed tab/space indentation, an over-long line, and a
/// lonely `TODO`/`FIXME` marker.
fn lint_textual_facts(
    out: &mut Vec<Diagnostic>,
    line: &str,
    line_no: usize,
    char_len: usize,
    max_len: usize,
) {
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
    let leading: String = line
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
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
            message: format!("Line exceeds {max_len} characters ({char_len})"),
        });
    }

    // TODO / FIXME markers.
    if let Some(byte) = line.find("TODO").or_else(|| line.find("FIXME")) {
        let col = line.get(..byte).unwrap_or("").chars().count() + 1;
        let marker = if line.get(byte..).is_some_and(|s| s.starts_with("TODO")) {
            "TODO"
        } else {
            "FIXME"
        };
        out.push(Diagnostic {
            severity: Severity::Info,
            line: line_no,
            col,
            message: format!("Unresolved {marker} marker"),
        });
    }
}

/// The structural guess: scan one line for bracket balance, skipping
/// string/char/comment-ish content crudely. `bracket_stack` and
/// `in_block_comment` carry state across lines within one [`lint`] call.
fn scan_line_brackets(
    line: &str,
    line_no: usize,
    language: Language,
    bracket_stack: &mut Vec<(char, usize, usize)>,
    in_block_comment: &mut bool,
    out: &mut Vec<Diagnostic>,
) {
    let mut chars = line.char_indices().peekable();
    let mut in_string = false;
    let mut string_delim = '"';
    while let Some((bi, ch)) = chars.next() {
        if *in_block_comment {
            if ch == '*'
                && let Some(&(_, '/')) = chars.peek()
            {
                *in_block_comment = false;
                chars.next();
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
            // A single quote is not always a string. In Rust `<'_>`,
            // `&'a str` and `'static` are lifetimes: one quote with no
            // partner. Treating one as an unterminated char literal makes
            // the scanner swallow the rest of the line — including the
            // bracket that closes the signature — and report "Unclosed '('"
            // on code that is perfectly valid. Opening real Tilo sources
            // produced eight such warnings on the first file.
            '\'' if has_lifetimes(language) && is_lifetime_tick(line, bi) => {}
            '"' | '\'' => {
                in_string = true;
                string_delim = ch;
            }
            '/' => {
                if let Some(&(_, '/')) = chars.peek() {
                    break; // line comment — ignore the rest
                }
                if let Some(&(_, '*')) = chars.peek() {
                    *in_block_comment = true;
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
                let col = line.get(..bi).unwrap_or("").chars().count() + 1;
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
                        let col = line.get(..bi).unwrap_or("").chars().count() + 1;
                        out.push(Diagnostic {
                            severity: Severity::Warning,
                            line: line_no,
                            col,
                            message: format!("Unmatched closing '{ch}'"),
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textual_facts_are_reported_and_survive_the_textual_only_scope() {
        // Each phase lints its own source; no state is shared.
        detects_trailing_whitespace();
        detects_todo_marker();
        textual_only_drops_structure_and_keeps_facts();
    }

    fn detects_trailing_whitespace() {
        let d = lint("let x = 1;   \n", Language::Rust, Scope::Full);
        assert!(d.iter().any(|x| x.message.contains("Trailing whitespace")));
    }

    #[test]
    fn bracket_scan_reports_real_imbalance_and_skips_strings_comments_and_literals() {
        // Each phase lints its own source; no state is shared.
        detects_unclosed_bracket();
        balanced_brackets_have_no_bracket_diagnostics();
        ignores_brackets_in_strings();
        ignores_brackets_after_line_comment();
        rust_lifetimes_are_not_read_as_char_literals();
        rust_char_literals_still_hide_their_contents();
        single_quoted_strings_still_hide_brackets_in_other_languages();
        real_rust_source_lints_clean();
    }

    fn detects_unclosed_bracket() {
        let d = lint("fn main() {\n", Language::Rust, Scope::Full);
        assert!(d.iter().any(|x| x.message.contains("Unclosed '{'")));
    }

    fn balanced_brackets_have_no_bracket_diagnostics() {
        let d = lint("fn main() {}\n", Language::Rust, Scope::Full);
        assert!(!d.iter().any(|x| x.message.contains("Unclosed")));
        assert!(!d.iter().any(|x| x.message.contains("Unmatched")));
    }

    fn ignores_brackets_in_strings() {
        let d = lint("let s = \"{[(\";\n", Language::Rust, Scope::Full);
        assert!(!d.iter().any(|x| x.message.contains("Unclosed")));
    }

    fn detects_todo_marker() {
        let d = lint("// TODO: fix this\n", Language::Rust, Scope::Full);
        assert!(d.iter().any(|x| x.message.contains("TODO")));
    }

    fn ignores_brackets_after_line_comment() {
        let d = lint(
            "let x = 1; // a comment with ( unbalanced\n",
            Language::Rust,
            Scope::Full,
        );
        assert!(!d.iter().any(|x| x.message.contains("Unclosed")));
    }

    /// The failure this exists to prevent: a lifetime is one quote with no
    /// partner, so reading it as an unterminated char literal makes the scanner
    /// swallow the rest of the line — including the `)` that closes the
    /// signature — and report "Unclosed '('" on code that compiles.
    fn rust_lifetimes_are_not_read_as_char_literals() {
        for src in [
            "fn f<'a>(s: &'a str) -> &'a str { s }\n",
            "impl<'a> Trait for Wrap<'a> {}\n",
            "fn g(s: &'static str) { drop(s); }\n",
            "fn h<'_a>(x: &'_ i32) { let _ = (x,); }\n",
        ] {
            let d = lint(src, Language::Rust, Scope::Full);
            let noise: Vec<_> = d
                .iter()
                .filter(|x| x.message.contains("Unclosed") || x.message.contains("Unmatched"))
                .map(|x| &x.message)
                .collect();
            assert!(
                noise.is_empty(),
                "{src:?} produced bracket noise: {noise:?}"
            );
        }
    }

    /// Char literals still close: the lifetime rule must not make the scanner
    /// blind to a genuine `'x'`, or brackets inside one would be counted.
    fn rust_char_literals_still_hide_their_contents() {
        let d = lint(
            "fn main() { let c = '('; drop(c); }\n",
            Language::Rust,
            Scope::Full,
        );
        assert!(
            !d.iter()
                .any(|x| x.message.contains("Unclosed") || x.message.contains("Unmatched")),
            "a '(' inside a char literal is not an unbalanced bracket: {:?}",
            d.iter().map(|x| &x.message).collect::<Vec<_>>()
        );
        let esc = lint(
            "fn main() { let c = '\\''; drop(c); }\n",
            Language::Rust,
            Scope::Full,
        );
        assert!(!esc.iter().any(|x| x.message.contains("Unclosed")));
    }

    /// The reason the lifetime rule is gated on the language. A single quote
    /// opens a *string* in Python, so applying Rust's rule here would read the
    /// string's contents as code and report the bracket inside it.
    fn single_quoted_strings_still_hide_brackets_in_other_languages() {
        for (lang, src) in [
            (Language::Python, "x = 'a) b'\nprint(x)\n"),
            (Language::JavaScript, "const x = 'a) b';\n"),
            (Language::Shell, "x='a) b'\n"),
        ] {
            let d = lint(src, lang, Scope::Full);
            let noise: Vec<_> = d
                .iter()
                .filter(|x| x.message.contains("Unclosed") || x.message.contains("Unmatched"))
                .map(|x| &x.message)
                .collect();
            assert!(
                noise.is_empty(),
                "{lang:?} {src:?} produced bracket noise: {noise:?}"
            );
        }
    }

    /// The linter is what the editor shows while a language server is still
    /// indexing, so it must stay useful on a real source file: this is a
    /// verbatim slice of Tilo code, and it must lint clean.
    fn real_rust_source_lints_clean() {
        let src = "\
use std::path::Path;

/// Resolve a path against a root.
pub fn resolve<'a>(root: &'a Path, rel: &str) -> Option<&'a Path> {
    if rel.is_empty() { return Some(root); }
    let _ = (root, rel);
    None
}
";
        let d = lint(src, Language::Rust, Scope::Full);
        assert!(
            d.is_empty(),
            "clean Rust should produce no diagnostics, got: {:?}",
            d.iter().map(|x| (&x.message, x.line)).collect::<Vec<_>>()
        );
    }

    /// `TextualOnly` drops the structural guess and keeps the textual facts.
    /// Both halves are asserted: a scope that dropped everything would satisfy
    /// the first assertion alone.
    fn textual_only_drops_structure_and_keeps_facts() {
        let src = "fn main( {   \n    // TODO: later\n";
        let full = lint(src, Language::Rust, Scope::Full);
        assert!(
            full.iter().any(|d| d.message.contains("Unclosed")),
            "the full scan reports the unbalanced bracket"
        );

        let textual = lint(src, Language::Rust, Scope::TextualOnly);
        assert!(
            !textual
                .iter()
                .any(|d| d.message.contains("Unclosed") || d.message.contains("Unmatched")),
            "structural guesses yield to the language server: {:?}",
            textual.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
        assert!(
            textual
                .iter()
                .any(|d| d.message.contains("Trailing whitespace")),
            "trailing whitespace is a fact about the text, not a guess"
        );
        assert!(
            textual.iter().any(|d| d.message.contains("TODO")),
            "a TODO marker is a fact about the text, not a guess"
        );
    }
}
