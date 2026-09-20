//! Unified-diff parsing and a lightweight, language-agnostic syntax
//! tokenizer.
//!
//! [`parse_unified`] turns standard `git diff` text into a structured
//! [`FileDiff`] with per-line old/new numbering and hunk boundaries, so
//! both the unified and side-by-side renderers can address lines without
//! re-scanning the raw text. The tokenizer is intentionally small: it
//! classifies spans as keyword / string / comment / number / punctuation
//! so the diff pane can colour code without pulling a multi-megabyte
//! grammar set. It errs toward "good enough to read", not perfect parsing.

use crate::model::{DiffHunk, DiffLine, DiffLineKind, FileDiff};

/// Parse a single-file unified diff (the body git emits for one path) into
/// a [`FileDiff`]. `path` and `language` are supplied by the caller since
/// the headers may be ambiguous (renames, new files).
pub fn parse_unified(path: &str, language: &str, text: &str) -> FileDiff {
    let mut diff = FileDiff {
        path: path.to_string(),
        old_path: None,
        language: language.to_string(),
        ..Default::default()
    };

    let mut state = HunkState::default();

    for raw in text.lines() {
        if raw.starts_with("@@") {
            state.open_hunk(&mut diff, raw);
            continue;
        }

        if is_meta_line(raw) {
            push_meta_line(&mut diff, raw, state.hunk_idx);
            continue;
        }

        if !state.in_hunk {
            continue;
        }

        state.push_content_line(&mut diff, raw);
    }

    state.close_hunk(&mut diff);
    diff
}

/// Running position while walking a unified diff: which hunk is open and
/// the next old/new line numbers to hand out.
#[derive(Default)]
struct HunkState {
    old_no: u32,
    new_no: u32,
    hunk_idx: usize,
    in_hunk: bool,
}

impl HunkState {
    /// Close the currently-open hunk's line range, if one is open.
    fn close_hunk(&self, diff: &mut FileDiff) {
        if self.in_hunk
            && let Some(h) = diff.hunks.last_mut()
        {
            h.line_range.1 = diff.lines.len();
        }
    }

    /// Close the previous hunk (if any) and open a new one at `raw`'s
    /// `@@ … @@` header.
    fn open_hunk(&mut self, diff: &mut FileDiff, raw: &str) {
        self.close_hunk(diff);
        let (old_start, new_start, header) = parse_hunk_header(raw);
        self.old_no = old_start;
        self.new_no = new_start;
        self.hunk_idx = diff.hunks.len();
        let hunk_start_line = diff.lines.len();
        diff.hunks.push(DiffHunk {
            header,
            old_start,
            new_start,
            line_range: (hunk_start_line, hunk_start_line),
        });
        diff.lines.push(DiffLine {
            kind: DiffLineKind::Hunk,
            old_lineno: None,
            new_lineno: None,
            content: raw.to_string(),
            hunk: self.hunk_idx,
        });
        self.in_hunk = true;
    }

    /// Classify and push one `+`/`-`/` ` content line, advancing the
    /// relevant line counters.
    fn push_content_line(&mut self, diff: &mut FileDiff, raw: &str) {
        let (kind, body) = match raw.chars().next() {
            Some('+') => (DiffLineKind::Addition, raw.get(1..).unwrap_or("")),
            Some('-') => (DiffLineKind::Deletion, raw.get(1..).unwrap_or("")),
            Some(' ') => (DiffLineKind::Context, raw.get(1..).unwrap_or("")),
            None => (DiffLineKind::Context, ""),
            _ => (DiffLineKind::Context, raw),
        };

        let (old_l, new_l) = match kind {
            DiffLineKind::Addition => {
                let n = self.new_no;
                self.new_no += 1;
                (None, Some(n))
            }
            DiffLineKind::Deletion => {
                let o = self.old_no;
                self.old_no += 1;
                (Some(o), None)
            }
            _ => {
                let o = self.old_no;
                let n = self.new_no;
                self.old_no += 1;
                self.new_no += 1;
                (Some(o), Some(n))
            }
        };

        diff.lines.push(DiffLine {
            kind,
            old_lineno: old_l,
            new_lineno: new_l,
            content: body.to_string(),
            hunk: self.hunk_idx,
        });
    }
}

/// Whether `raw` is a file-level metadata line (`diff --git`, `index`,
/// `--- a/…`, rename/similarity notes, binary markers, …) rather than hunk
/// content.
fn is_meta_line(raw: &str) -> bool {
    raw.starts_with("diff ")
        || raw.starts_with("index ")
        || raw.starts_with("--- ")
        || raw.starts_with("+++ ")
        || raw.starts_with("new file")
        || raw.starts_with("deleted file")
        || raw.starts_with("rename ")
        || raw.starts_with("similarity ")
        || raw.starts_with("Binary ")
}

/// Record a metadata line's file-level flags and append it to `diff.lines`.
fn push_meta_line(diff: &mut FileDiff, raw: &str, hunk_idx: usize) {
    if raw.starts_with("new file") {
        diff.is_new = true;
    }
    if raw.starts_with("deleted file") {
        diff.is_deleted = true;
    }
    if raw.starts_with("Binary ") {
        diff.binary = true;
    }
    diff.lines.push(DiffLine {
        kind: DiffLineKind::Meta,
        old_lineno: None,
        new_lineno: None,
        content: raw.to_string(),
        hunk: hunk_idx,
    });
}

/// Parse an `@@ -a,b +c,d @@ trailing @@` header into (`old_start`, `new_start`,
/// remaining-section-text).
fn parse_hunk_header(line: &str) -> (u32, u32, String) {
    // Expected shape: @@ -12,7 +12,9 @@ fn foo()
    let mut old_start = 0;
    let mut new_start = 0;
    if let Some(rest) = line.strip_prefix("@@ ") {
        for tok in rest.split_whitespace() {
            if let Some(neg) = tok.strip_prefix('-') {
                old_start = neg
                    .split(',')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            } else if let Some(pos) = tok.strip_prefix('+') {
                new_start = pos
                    .split(',')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            }
        }
    }
    // The section heading after the closing `@@`.
    let header = line
        .splitn(3, "@@")
        .nth(2)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    (old_start.max(1), new_start.max(1), header)
}

/// Map a file extension to a coarse language label the tokenizer keys off.
pub fn language_for(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rust",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "json" => "json",
        "js" | "ts" | "jsx" | "tsx" => "js",
        "py" => "python",
        "c" | "h" | "cpp" | "hpp" | "cc" => "c",
        "sh" | "bash" => "shell",
        "yaml" | "yml" => "yaml",
        _ => "text",
    }
    .to_string()
}

// === Lightweight syntax tokenizer =====================================

/// A classified span of a source line. The renderer maps each class to a
/// theme colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenClass {
    Plain,
    Keyword,
    Str,
    Comment,
    Number,
    Type,
    Punct,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub text: String,
    pub class: TokenClass,
}

const RUST_KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "pub", "use", "mod", "struct", "enum", "impl", "trait", "for", "while",
    "loop", "if", "else", "match", "return", "self", "Self", "crate", "super", "as", "in", "ref",
    "where", "const", "static", "move", "async", "await", "dyn", "type", "unsafe", "extern",
    "true", "false", "continue", "break", "default",
];

const GENERIC_KEYWORDS: &[&str] = &[
    "function", "var", "const", "let", "return", "if", "else", "for", "while", "class", "import",
    "from", "export", "def", "elif", "try", "except", "True", "False", "None", "null", "true",
    "false", "new", "this", "public", "private", "static", "void", "int", "string", "bool",
    "double",
];

/// Punctuation / operator characters, coalesced into runs by the tokenizer.
const PUNCT_CHARS: &str = "+-*/%=<>!&|^~?:;,.(){}[]@#";

/// Tokenize one source line. Comment detection is single-line only (`//`,
/// `#`); block comments are treated as plain to avoid carrying state across
/// lines (the diff pane shows discontiguous slices anyway).
pub fn tokenize(line: &str, language: &str) -> Vec<Token> {
    let keywords: &[&str] = match language {
        "rust" => RUST_KEYWORDS,
        _ => GENERIC_KEYWORDS,
    };
    let comment_lead: &[&str] = match language {
        "python" | "shell" | "yaml" | "toml" => &["#"],
        _ => &["//", "#"],
    };

    let mut out: Vec<Token> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while let Some(&c) = chars.get(i) {
        // Single-line comment to end of line.
        if let Some(rest) = comment_at(&chars, i, comment_lead) {
            out.push(Token {
                text: rest,
                class: TokenClass::Comment,
            });
            break;
        }

        // String literal (single or double quoted).
        if c == '"' || c == '\'' {
            let (text, next) = read_string_literal(&chars, i, c);
            i = next;
            out.push(Token {
                text,
                class: TokenClass::Str,
            });
            continue;
        }

        // Number.
        if c.is_ascii_digit() {
            let (text, next) = read_number(&chars, i);
            i = next;
            out.push(Token {
                text,
                class: TokenClass::Number,
            });
            continue;
        }

        // Identifier / keyword / type.
        if c.is_alphabetic() || c == '_' {
            let (text, next) = read_identifier(&chars, i);
            i = next;
            let class = if keywords.contains(&text.as_str()) {
                TokenClass::Keyword
            } else if text.chars().next().is_some_and(char::is_uppercase) {
                TokenClass::Type
            } else {
                TokenClass::Plain
            };
            out.push(Token { text, class });
            continue;
        }

        // Punctuation / operators — coalesce runs of symbol chars.
        if PUNCT_CHARS.contains(c) {
            let (text, next) = read_punct(&chars, i, comment_lead);
            i = next;
            out.push(Token {
                text,
                class: TokenClass::Punct,
            });
            continue;
        }

        // Whitespace and everything else — pass through as plain.
        let (text, next) = read_plain(&chars, i);
        i = next;
        out.push(Token {
            text,
            class: TokenClass::Plain,
        });
    }

    out
}

/// If a single-line comment starts at `i`, return the rest of the line as
/// its text.
fn comment_at(chars: &[char], i: usize, comment_lead: &[&str]) -> Option<String> {
    let rest: String = chars.get(i..).unwrap_or_default().iter().collect();
    comment_lead
        .iter()
        .any(|p| rest.starts_with(p))
        .then_some(rest)
}

/// Consume a quoted string literal starting at `i` (the opening quote).
/// Returns the literal text (quotes included) and the index just past it.
fn read_string_literal(chars: &[char], start: usize, quote: char) -> (String, usize) {
    let mut s = String::new();
    s.push(quote);
    let mut i = start + 1;
    while let Some(&ch) = chars.get(i) {
        s.push(ch);
        let escaped = i.checked_sub(1).and_then(|p| chars.get(p)) == Some(&'\\');
        i += 1;
        if ch == quote && !escaped {
            break;
        }
    }
    (s, i)
}

/// Consume a run of digits/`.`/`_` starting at `i`.
fn read_number(chars: &[char], start: usize) -> (String, usize) {
    let mut s = String::new();
    let mut i = start;
    while let Some(&ch) = chars.get(i) {
        if !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_') {
            break;
        }
        s.push(ch);
        i += 1;
    }
    (s, i)
}

/// Consume an identifier (alphanumeric/`_`) starting at `i`.
fn read_identifier(chars: &[char], start: usize) -> (String, usize) {
    let mut s = String::new();
    let mut i = start;
    while let Some(&ch) = chars.get(i) {
        if !(ch.is_alphanumeric() || ch == '_') {
            break;
        }
        s.push(ch);
        i += 1;
    }
    (s, i)
}

/// Consume a run of punctuation/operator characters starting at `i`,
/// stopping before a comment lead-in that begins mid-run.
fn read_punct(chars: &[char], start: usize, comment_lead: &[&str]) -> (String, usize) {
    let mut s = String::new();
    let mut i = start;
    while let Some(&ch) = chars.get(i) {
        if !PUNCT_CHARS.contains(ch) {
            break;
        }
        let look: String = chars.get(i..).unwrap_or_default().iter().collect();
        if !s.is_empty() && comment_lead.iter().any(|p| look.starts_with(p)) {
            break;
        }
        s.push(ch);
        i += 1;
    }
    (s, i)
}

/// Consume a run of characters that are none of alphanumeric/`_`/quote/
/// punctuation — plain whitespace and stray symbols. Always advances by at
/// least one character.
fn read_plain(chars: &[char], start: usize) -> (String, usize) {
    let mut s = String::new();
    let mut i = start;
    while let Some(&ch) = chars.get(i) {
        if ch.is_alphanumeric() || ch == '_' || ch == '"' || ch == '\'' || PUNCT_CHARS.contains(ch)
        {
            break;
        }
        s.push(ch);
        i += 1;
    }
    if s.is_empty() {
        // Safety: ensure progress even if none of the above ever matched.
        if let Some(&ch) = chars.get(i) {
            s.push(ch);
            i += 1;
        }
    }
    (s, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hunk_line_numbering() {
        let text = "\
@@ -1,3 +1,4 @@ fn main()
 context
-removed
+added one
+added two
 tail";
        let d = parse_unified("src/main.rs", "rust", text);
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.additions(), 2);
        assert_eq!(d.deletions(), 1);
        // First context line is old 1 / new 1.
        let ctx = d.lines.iter().find(|l| l.content == "context").unwrap();
        assert_eq!(ctx.old_lineno, Some(1));
        assert_eq!(ctx.new_lineno, Some(1));
        // Addition has only a new line number.
        let add = d.lines.iter().find(|l| l.content == "added one").unwrap();
        assert_eq!(add.old_lineno, None);
        assert_eq!(add.new_lineno, Some(2));
    }

    #[test]
    fn detects_new_and_deleted_files() {
        let new = parse_unified(
            "a.txt",
            "text",
            "new file mode 100644\n@@ -0,0 +1,1 @@\n+hi",
        );
        assert!(new.is_new);
        let del = parse_unified(
            "b.txt",
            "text",
            "deleted file mode 100644\n@@ -1,1 +0,0 @@\n-bye",
        );
        assert!(del.is_deleted);
    }

    #[test]
    fn tokenizer_classifies_rust() {
        let toks = tokenize("    let x = \"hi\"; // note", "rust");
        assert!(
            toks.iter()
                .any(|t| t.text == "let" && t.class == TokenClass::Keyword)
        );
        assert!(
            toks.iter()
                .any(|t| t.class == TokenClass::Str && t.text.contains("hi"))
        );
        assert!(toks.iter().any(|t| t.class == TokenClass::Comment));
    }

    #[test]
    fn language_detection() {
        assert_eq!(language_for("src/main.rs"), "rust");
        assert_eq!(language_for("Cargo.toml"), "toml");
        assert_eq!(language_for("README.md"), "markdown");
    }
}
