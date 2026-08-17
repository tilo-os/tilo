//! Shared "visual markdown" input detection.
//!
//! Both the notes [`BlockEditor`](crate::widget::BlockEditor) and the
//! word-processor live-transform markdown-ish input as the user types:
//! `# ` becomes a heading, `**bold**` becomes a real bold run, and so on.
//! The *decision* about whether a keystroke completes such a shortcut is
//! identical across editors — only the way each editor *renders* the result
//! differs (the notes editor uses inline [`Mark`](crate::widget::marks::Mark)
//! spans on a `RichTextLine`; the word-processor writes automerge run marks on
//! its `qdoc` model).
//!
//! This module owns that shared detection so neither app re-implements it. It
//! is pure, rendering-agnostic logic returning byte ranges + a neutral kind;
//! each editor maps the result onto its own formatting model.
//!
//! # What is detected
//!
//! - **Block prefixes** ([`detect_block_prefix`]) — the marker typed at the
//!   very start of an (otherwise empty) line, completed by a space:
//!   `#`/`##`/`###` → headings, `-`/`*`/`+` → bullet list, `1.`/`1)` →
//!   ordered list.
//! - **Inline spans** ([`detect_inline`]) — a delimiter pair completed by the
//!   just-typed closing keystroke: `**b**`/`__b__` → bold, `*i*`/`_i_` →
//!   italic, `` `c` `` → inline code, `~~s~~` → strikethrough.
//!
//! Detection fires on the *completing* keystroke: the closing `*`/`` ` ``/`~`
//! of an inline pair, or the space after a block marker. The caller strips the
//! markers and applies the real formatting run.

use std::ops::Range;

/// Neutral inline-format kind produced by [`detect_inline`]. Rendering-
/// agnostic: each editor maps it onto its own inline-formatting model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineKind {
    Bold,
    Italic,
    Code,
    Strikethrough,
}

/// A detected inline markdown span within the text before the caret.
///
/// All three ranges are byte offsets into the `&str` passed to
/// [`detect_inline`]. To apply the transform, delete the [`close`](Self::close)
/// and [`open`](Self::open) delimiter bytes and format the surviving
/// [`content`](Self::content) with [`kind`](Self::kind). `content` is
/// contiguous and sits between the two delimiters, so after deleting `open`
/// the content lands at `open.start .. open.start + content.len()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineMatch {
    pub kind: InlineKind,
    /// Opening delimiter bytes (e.g. the leading `**`) — deleted.
    pub open: Range<usize>,
    /// Inner text bytes — kept and formatted.
    pub content: Range<usize>,
    /// Closing delimiter bytes (e.g. the trailing `**`, ending at the caret) —
    /// deleted.
    pub close: Range<usize>,
}

/// Neutral block-prefix kind produced by [`detect_block_prefix`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockPrefix {
    /// Heading level 1..=3.
    Heading(u8),
    /// Unordered (bullet) list item.
    BulletList,
    /// Ordered (numbered) list item.
    OrderedList,
}

/// Detect a block-level markdown prefix.
///
/// `marker` is the run of marker characters typed at the start of the line,
/// **without** the completing space (e.g. `"#"`, `"##"`, `"###"`, `"-"`,
/// `"*"`, `"+"`, `"1."`, `"1)"`). The caller is responsible for only calling
/// this when that marker is the entire line content before the caret and the
/// completing keystroke was a space — see the editors' handlers.
pub fn detect_block_prefix(marker: &str) -> Option<BlockPrefix> {
    match marker {
        "#" => Some(BlockPrefix::Heading(1)),
        "##" => Some(BlockPrefix::Heading(2)),
        "###" => Some(BlockPrefix::Heading(3)),
        "-" | "*" | "+" => Some(BlockPrefix::BulletList),
        "1." | "1)" => Some(BlockPrefix::OrderedList),
        _ => None,
    }
}

/// True for the delimiter characters that open/close inline spans.
fn is_delim(b: u8) -> bool {
    matches!(b, b'*' | b'_' | b'`' | b'~')
}

/// Detect an inline markdown span completed by the last character of
/// `before_caret`.
///
/// `before_caret` is the text of the current line/block from its start up to
/// the caret, **including** the just-typed closing delimiter. Returns the
/// span to transform, or `None` when the keystroke doesn't complete a
/// recognised pair.
///
/// Rules (kept deliberately conservative so a stray delimiter never eats
/// text): the closing delimiter is the maximal run of the trigger character
/// ending at the caret, and it must be exactly the width the kind expects
/// (`` ` `` → 1, `~` → 2, `*`/`_` → 1 for italic or 2 for bold). The opening
/// delimiter is the nearest run of the same character to the left, and it must
/// match that width exactly. The content between them must be non-empty, must
/// not itself contain the delimiter, and must not begin or end with
/// whitespace. For `_` the opener must not sit inside a word (so `snake_case`
/// is left alone).
pub fn detect_inline(before_caret: &str) -> Option<InlineMatch> {
    let bytes = before_caret.as_bytes();
    let caret = bytes.len();
    if caret == 0 {
        return None;
    }
    let ch = bytes[caret - 1];
    if !is_delim(ch) {
        return None;
    }

    // Closing delimiter: the maximal run of `ch` ending at the caret.
    let mut cs = caret;
    while cs > 0 && bytes[cs - 1] == ch {
        cs -= 1;
    }
    let close_run = caret - cs;

    // Required delimiter width + resulting kind. A run wider than the kind
    // expects (e.g. ``` ``` ```, `***`) is left untouched.
    let (n, kind) = match ch {
        b'`' if close_run == 1 => (1usize, InlineKind::Code),
        b'~' if close_run == 2 => (2, InlineKind::Strikethrough),
        b'*' | b'_' if close_run == 1 => (1, InlineKind::Italic),
        b'*' | b'_' if close_run == 2 => (2, InlineKind::Bold),
        _ => return None,
    };

    let close = (caret - n)..caret;
    let content_end = close.start;

    // Opening delimiter: the nearest `ch` strictly left of the content. Because
    // the content may not contain `ch`, the first `ch` we meet scanning left
    // bounds the content on the right edge of the opener run.
    let mut j = content_end;
    while j > 0 && bytes[j - 1] != ch {
        j -= 1;
    }
    if j == 0 {
        return None; // no opener
    }
    let open_end = j;
    let mut open_start = open_end;
    while open_start > 0 && bytes[open_start - 1] == ch {
        open_start -= 1;
    }
    if open_end - open_start != n {
        return None; // opener width must match the closer exactly
    }

    let content = open_end..content_end;
    if content.start >= content.end {
        return None; // empty content
    }
    // Content must not begin or end with whitespace (matches markdown emphasis
    // flanking; also stops `* ` bullets and `a * b` from being read as spans).
    if bytes[content.start].is_ascii_whitespace()
        || bytes[content.end - 1].is_ascii_whitespace()
    {
        return None;
    }
    // Intra-word guard for `_`: the opener must not follow an alphanumeric, so
    // identifiers like `foo_bar_baz` don't italicise.
    if ch == b'_' && open_start > 0 && bytes[open_start - 1].is_ascii_alphanumeric() {
        return None;
    }

    Some(InlineMatch {
        kind,
        open: open_start..open_end,
        content,
        close,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline(s: &str) -> Option<InlineMatch> {
        detect_inline(s)
    }

    /// Extract `(kind, kept-content, stripped-result)` for readable asserts.
    fn applied(s: &str) -> Option<(InlineKind, String, String)> {
        let m = detect_inline(s)?;
        let content = s[m.content.clone()].to_string();
        let mut out = String::new();
        out.push_str(&s[..m.open.start]);
        out.push_str(&s[m.content.clone()]);
        out.push_str(&s[m.close.end..]);
        Some((m.kind, content, out))
    }

    #[test]
    fn bold_double_star() {
        let (k, c, out) = applied("**bold**").unwrap();
        assert_eq!(k, InlineKind::Bold);
        assert_eq!(c, "bold");
        assert_eq!(out, "bold");
    }

    #[test]
    fn bold_double_underscore() {
        let (k, c, _) = applied("__bold__").unwrap();
        assert_eq!(k, InlineKind::Bold);
        assert_eq!(c, "bold");
    }

    #[test]
    fn italic_single_star_and_underscore() {
        assert_eq!(applied("*i*").unwrap().0, InlineKind::Italic);
        assert_eq!(applied("_i_").unwrap().0, InlineKind::Italic);
        assert_eq!(applied("*i*").unwrap().1, "i");
    }

    #[test]
    fn inline_code_backticks() {
        let (k, c, out) = applied("`code`").unwrap();
        assert_eq!(k, InlineKind::Code);
        assert_eq!(c, "code");
        assert_eq!(out, "code");
    }

    #[test]
    fn strikethrough_double_tilde() {
        let (k, c, _) = applied("~~gone~~").unwrap();
        assert_eq!(k, InlineKind::Strikethrough);
        assert_eq!(c, "gone");
    }

    #[test]
    fn bold_only_completes_on_second_closing_star() {
        // Mid-typing "**bold*" must NOT fire (would leave a stray `*`).
        assert!(inline("**bold*").is_none());
        // The next `*` completes bold.
        assert_eq!(inline("**bold**").unwrap().kind, InlineKind::Bold);
    }

    #[test]
    fn single_tilde_does_not_strike() {
        assert!(inline("~s~").is_none());
    }

    #[test]
    fn triple_backtick_is_not_inline_code() {
        assert!(inline("```").is_none());
    }

    #[test]
    fn opener_needs_matching_width() {
        // A single `*` closer against a `**` opener isn't a match.
        assert!(inline("**bold*").is_none());
    }

    #[test]
    fn empty_content_is_ignored() {
        assert!(inline("****").is_none());
        assert!(inline("``").is_none());
        assert!(inline("**").is_none());
    }

    #[test]
    fn whitespace_flanks_are_ignored() {
        assert!(inline("* not bold *").is_none());
        assert!(inline("*trailing *").is_none());
        assert!(inline("* leading*").is_none());
    }

    #[test]
    fn underscore_does_not_break_snake_case() {
        // `foo_bar_` — the opener `_` follows `o`, so no italic.
        assert!(inline("foo_bar_").is_none());
        // But a word-boundary `_x_` italicises.
        assert_eq!(inline(" _x_").unwrap().kind, InlineKind::Italic);
    }

    #[test]
    fn span_after_existing_text() {
        // "a *b*" italicises just "b", leaving "a " intact.
        let (k, c, out) = applied("a *b*").unwrap();
        assert_eq!(k, InlineKind::Italic);
        assert_eq!(c, "b");
        assert_eq!(out, "a b");
    }

    #[test]
    fn non_delimiter_last_char_is_none() {
        assert!(inline("plain text").is_none());
        assert!(inline("# heading").is_none());
    }

    #[test]
    fn content_with_spaces_inside_is_kept() {
        let (_, c, out) = applied("**two words**").unwrap();
        assert_eq!(c, "two words");
        assert_eq!(out, "two words");
    }

    #[test]
    fn unicode_content_ranges_are_byte_aligned() {
        // Multibyte content must not panic and must slice cleanly.
        let (k, c, out) = applied("*café*").unwrap();
        assert_eq!(k, InlineKind::Italic);
        assert_eq!(c, "café");
        assert_eq!(out, "café");
    }

    // ---- block prefixes ---------------------------------------------------

    #[test]
    fn heading_prefixes() {
        assert_eq!(detect_block_prefix("#"), Some(BlockPrefix::Heading(1)));
        assert_eq!(detect_block_prefix("##"), Some(BlockPrefix::Heading(2)));
        assert_eq!(detect_block_prefix("###"), Some(BlockPrefix::Heading(3)));
    }

    #[test]
    fn list_prefixes() {
        assert_eq!(detect_block_prefix("-"), Some(BlockPrefix::BulletList));
        assert_eq!(detect_block_prefix("*"), Some(BlockPrefix::BulletList));
        assert_eq!(detect_block_prefix("+"), Some(BlockPrefix::BulletList));
        assert_eq!(detect_block_prefix("1."), Some(BlockPrefix::OrderedList));
        assert_eq!(detect_block_prefix("1)"), Some(BlockPrefix::OrderedList));
    }

    #[test]
    fn non_prefixes_are_ignored() {
        assert_eq!(detect_block_prefix("####"), None);
        assert_eq!(detect_block_prefix("2."), None);
        assert_eq!(detect_block_prefix("hello"), None);
        assert_eq!(detect_block_prefix(""), None);
    }
}
