//! A pragmatic, single-pass Typst syntax highlighter.
//!
//! This is intentionally not a full Typst parser — it is a per-line lexer that
//! recognises the constructs that matter visually in a source pane: headings,
//! comments, strings, `#`-functions / keywords, numbers, raw code (inline and
//! fenced), maths, and `*strong*` / `_emph_` runs. It tracks just enough cross
//! -line state (fenced code blocks) to stay coherent.
//!
//! Every colour is resolved from the OS [`Theme`] at draw time — the lexer
//! emits *categories*, never colours.

use ui::text_render::TextWeight;
use ui::theme::Theme;
use vello::peniko::Color;

/// Visual category a run of characters falls into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Plain,
    Comment,
    Heading,
    StringLit,
    Function,
    Number,
    Strong,
    Emph,
    Raw,
    Math,
}

/// A contiguous run of characters on one line sharing a [`Category`].
#[derive(Clone, Debug)]
pub struct Token {
    /// Column of the first character (0-based, in chars).
    pub start: usize,
    pub text: String,
    pub cat: Category,
}

/// Resolve a category to a concrete draw style: colour + weight + italic.
/// All colours come from theme tokens so the pane tracks the active theme.
pub fn style(cat: Category, theme: &Theme) -> (Color, TextWeight, bool) {
    let c = &theme.tokens.colors;
    match cat {
        Category::Plain => (c.text_primary, TextWeight::Regular, false),
        Category::Comment => (c.text_dim, TextWeight::Regular, true),
        Category::Heading => (c.accent[ui_accent::PRIMARY], TextWeight::Bold, false),
        Category::StringLit => (c.semantic.success, TextWeight::Regular, false),
        Category::Function => (c.semantic.info, TextWeight::Medium, false),
        Category::Number => (c.semantic.warning, TextWeight::Regular, false),
        Category::Strong => (c.text_primary, TextWeight::Bold, false),
        Category::Emph => (c.text_primary, TextWeight::Regular, true),
        Category::Raw => (c.accent[ui_accent::RAW], TextWeight::Regular, false),
        Category::Math => (c.accent[ui_accent::MATH], TextWeight::Regular, true),
    }
}

/// Accent-ramp indices used by the highlighter, named for clarity.
mod ui_accent {
    /// indigo-500 — headings.
    pub const PRIMARY: usize = 5;
    /// indigo-300 — raw / code.
    pub const RAW: usize = 7;
    /// indigo-400 — maths.
    pub const MATH: usize = 6;
}

/// Highlight an entire source string into per-line token lists. The returned
/// outer vec has one entry per line (split on `\n`), and the tokens within a
/// line are contiguous and cover every character (gaps are `Plain`).
pub fn highlight(source: &str) -> Vec<Vec<Token>> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in source.split('\n') {
        out.push(highlight_line(line, &mut in_fence));
    }
    out
}

fn highlight_line(line: &str, in_fence: &mut bool) -> Vec<Token> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut cats = vec![Category::Plain; n];

    let trimmed = line.trim_start();

    // Fenced code blocks: a line whose first non-space run is ``` toggles the
    // fence. The fence delimiter line itself renders as raw.
    if trimmed.starts_with("```") {
        for c in cats.iter_mut() {
            *c = Category::Raw;
        }
        *in_fence = !*in_fence;
        return coalesce(&chars, &cats);
    }
    if *in_fence {
        for c in cats.iter_mut() {
            *c = Category::Raw;
        }
        return coalesce(&chars, &cats);
    }

    // A heading line: optional indent then one or more '='.
    if trimmed.starts_with('=') {
        for c in cats.iter_mut() {
            *c = Category::Heading;
        }
        return coalesce(&chars, &cats);
    }

    let mut i = 0usize;
    while i < n {
        let c = chars[i];
        // Line comment.
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            for cc in cats.iter_mut().skip(i) {
                *cc = Category::Comment;
            }
            break;
        }
        // String literal.
        if c == '"' {
            let end = find_close(&chars, i + 1, '"').unwrap_or(n - 1);
            mark(&mut cats, i, end, Category::StringLit);
            i = end + 1;
            continue;
        }
        // Inline raw code.
        if c == '`' {
            let end = find_close(&chars, i + 1, '`').unwrap_or(n - 1);
            mark(&mut cats, i, end, Category::Raw);
            i = end + 1;
            continue;
        }
        // Maths span.
        if c == '$' {
            let end = find_close(&chars, i + 1, '$').unwrap_or(n - 1);
            mark(&mut cats, i, end, Category::Math);
            i = end + 1;
            continue;
        }
        // `#` function / keyword: hash plus an identifier (allowing dots for
        // method-style `table.header`).
        if c == '#' {
            let mut j = i + 1;
            while j < n && (chars[j].is_alphanumeric() || chars[j] == '_' || chars[j] == '.') {
                j += 1;
            }
            mark(&mut cats, i, j.saturating_sub(1).max(i), Category::Function);
            i = j.max(i + 1);
            continue;
        }
        // *strong* run.
        if c == '*' {
            if let Some(end) = find_close(&chars, i + 1, '*') {
                if end > i + 1 {
                    mark(&mut cats, i, end, Category::Strong);
                    i = end + 1;
                    continue;
                }
            }
        }
        // _emph_ run.
        if c == '_' {
            if let Some(end) = find_close(&chars, i + 1, '_') {
                if end > i + 1 {
                    mark(&mut cats, i, end, Category::Emph);
                    i = end + 1;
                    continue;
                }
            }
        }
        // Number (with optional unit suffix like 12pt, 1.5cm, 80%).
        if c.is_ascii_digit() {
            let mut j = i;
            while j < n && (chars[j].is_ascii_digit() || chars[j] == '.') {
                j += 1;
            }
            while j < n && (chars[j].is_ascii_alphabetic() || chars[j] == '%') {
                j += 1;
            }
            mark(&mut cats, i, j - 1, Category::Number);
            i = j;
            continue;
        }
        i += 1;
    }

    coalesce(&chars, &cats)
}

/// Find the index of the next `target` char at or after `from`.
fn find_close(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&k| chars[k] == target)
}

/// Mark `cats[lo..=hi]` with `cat`, clamping `hi` into range.
fn mark(cats: &mut [Category], lo: usize, hi: usize, cat: Category) {
    let hi = hi.min(cats.len().saturating_sub(1));
    for slot in cats.iter_mut().take(hi + 1).skip(lo) {
        *slot = cat;
    }
}

/// Collapse a per-character category array into contiguous tokens.
fn coalesce(chars: &[char], cats: &[Category]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let cat = cats[i];
        let start = i;
        let mut text = String::new();
        while i < chars.len() && cats[i] == cat {
            text.push(chars[i]);
            i += 1;
        }
        tokens.push(Token { start, text, cat });
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_line_is_one_heading_token() {
        let lines = highlight("= Title");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0][0].cat, Category::Heading);
    }

    #[test]
    fn string_and_function_classified() {
        let lines = highlight(r#"#set text("hi")"#);
        let cats: Vec<_> = lines[0].iter().map(|t| t.cat).collect();
        assert!(cats.contains(&Category::Function));
        assert!(cats.contains(&Category::StringLit));
    }

    #[test]
    fn fenced_block_spans_lines() {
        let lines = highlight("```rust\nlet x = 1;\n```");
        // Middle line is entirely raw.
        assert!(lines[1].iter().all(|t| t.cat == Category::Raw));
    }

    #[test]
    fn tokens_cover_every_char() {
        let src = "hello *world* 42";
        let lines = highlight(src);
        let total: usize = lines[0].iter().map(|t| t.text.chars().count()).sum();
        assert_eq!(total, src.chars().count());
    }
}
