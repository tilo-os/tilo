//! Pure functions to translate between a page's markdown body on
//! disk and the in-memory block list the [`ui::widget::BlockEditor`]
//! renders.
//!
//! The grammar supports block structure and a per-block [`MarkSpan`] list:
//!
//! - A line beginning with `# `, `## `, `### ` is a heading.
//! - A line beginning with `- ` / `* ` (after optional 2-space
//!   indent groups, max 5 levels) is a bullet list item.
//! - A line beginning with `- [ ] ` / `- [x] ` (similarly indentable)
//!   is a checklist item.
//! - A line equal to `---` is a divider.
//! - A line beginning with `> ` is a block quote.
//! - A quote opened by an alert marker line, `> [!NOTE]`, `> [!TIP]`,
//!   `> [!IMPORTANT]`, `> [!WARNING]` or `> [!CAUTION]` (GitHub-flavoured
//!   Markdown's alerts), is a callout of that kind; its text is the quoted
//!   lines after the marker.
//! - A line beginning with ` ``` ` opens a fenced code block; the
//!   matching closing fence terminates it. **v0.1 limitation**: code
//!   block body is single-line per block.
//! - Inline marks:
//!   - `**text**` → bold
//!   - `*text*`  → italic (but: `**` never parses as italic-double-
//!     star; the bold tokenizer runs first)
//!   - `__text__` → underline
//!   - `~~text~~` → strikethrough
//!   - `` `text` `` → inline code
//!   - `[[Page Title]]` → page link (target id = `Uuid::nil()`; the
//!     notes app resolves the title to an id at parse time)
//!   - `[label](https://example.test)` → external link
//! - Any other non-empty line is a paragraph.
//! - Blank lines are block separators (consecutive blanks collapse).
//! - A paragraph line that is exactly `&nbsp;` is an empty paragraph.
//!
//! ## Escapes
//!
//! A backslash makes the next character literal, as in CommonMark: ASCII
//! punctuation, and also a space, tab or line break. The serialiser escapes
//! every character of a text that would otherwise read back as markup (inline
//! markers, a paragraph start that looks like another block, whitespace a
//! text starts or ends with), so export then import gives back the same text.
//! Code and link text is verbatim and never escaped.
//!
//! ## Marks model
//!
//! Each non-code block carries a `text: String` (no markdown chrome
//! — the inline markers are stripped) and a `marks: Vec<MarkSpan>`
//! sorted by `range.start`. The serialiser walks the text + marks
//! together and re-emits the chrome. **Nesting**: bold and italic
//! may share a range; the serialiser nests them deterministically
//! (bold first, then italic inside). Code + link don't nest with
//! other marks for v0.1 — if the parser sees `**[[Page]]**` the
//! double-bracket wins and the bold is dropped from that range.

use std::fmt::Write as _;

use ui::widget::marks::{Mark, MarkSpan, PageId};

/// Avoid rendering the note title twice when an imported Markdown document
/// starts with the same H1 as the title chosen by the importer. Only the exact
/// leading heading is removed; unrelated headings and body whitespace remain.
pub fn strip_matching_title_heading(body: &str, title: &str) -> String {
    let normalized = body.strip_prefix('\u{feff}').unwrap_or(body);
    let Some((first, rest)) = normalized.split_once('\n') else {
        return normalized.to_string();
    };
    if first.trim_end() != format!("# {}", title.trim()) {
        return normalized.to_string();
    }
    rest.strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest)
        .to_string()
}

/// Storage-friendly block representation. Carries the kind tag, the
/// raw text content (no inline markers), and any inline-mark spans.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InlineText {
    pub text: String,
    pub marks: Vec<MarkSpan>,
}

impl InlineText {
    pub fn new(text: impl Into<String>, marks: Vec<MarkSpan>) -> Self {
        Self {
            text: text.into(),
            marks,
        }
    }
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            marks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockSpec {
    /// Non-list content at an outline level, independent of its presentation kind.
    Nested {
        depth: u8,
        block: Box<BlockSpec>,
    },
    Paragraph(InlineText),
    Quote(InlineText),
    Callout {
        kind: ui::widget::CalloutKind,
        text: InlineText,
    },
    Heading {
        level: u8,
        text: InlineText,
    },
    Bullet {
        indent: u8,
        text: InlineText,
    },
    Numbered {
        indent: u8,
        text: InlineText,
    },
    Checklist {
        indent: u8,
        done: bool,
        text: InlineText,
    },
    CodeBlock {
        lang: Option<String>,
        text: String,
    },
    Table {
        source: String,
    },
    Divider,
}

/// Same constant as [`ui::widget::block_editor::MAX_LIST_INDENT`].
pub const MAX_LIST_INDENT: u8 = 5;

impl BlockSpec {
    fn indented(self, depth: u8) -> Self {
        match self {
            Self::Bullet { indent, text } => Self::Bullet {
                indent: (indent + depth).min(MAX_LIST_INDENT),
                text,
            },
            Self::Numbered { indent, text } => Self::Numbered {
                indent: (indent + depth).min(MAX_LIST_INDENT),
                text,
            },
            Self::Checklist { indent, done, text } => Self::Checklist {
                indent: (indent + depth).min(MAX_LIST_INDENT),
                done,
                text,
            },
            Self::Nested {
                depth: inner,
                block,
            } => Self::Nested {
                depth: (inner + depth).min(MAX_LIST_INDENT),
                block,
            },
            block if depth > 0 => Self::Nested {
                depth,
                block: Box::new(block),
            },
            block => block,
        }
    }
    pub fn paragraph(s: impl Into<String>) -> Self {
        BlockSpec::Paragraph(InlineText::plain(s))
    }
    pub fn numbered(indent: u8, s: impl Into<String>) -> Self {
        BlockSpec::Numbered {
            indent: indent.min(MAX_LIST_INDENT),
            text: InlineText::plain(s),
        }
    }
    pub fn code_block(lang: Option<String>, s: impl Into<String>) -> Self {
        BlockSpec::CodeBlock {
            lang,
            text: s.into(),
        }
    }
    pub fn divider() -> Self {
        BlockSpec::Divider
    }
    pub fn table(source: impl Into<String>) -> Self {
        BlockSpec::Table {
            source: source.into(),
        }
    }
}

fn table_separator_line(line: &str) -> bool {
    let trimmed = line.trim();
    let body = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let body = body.strip_suffix('|').unwrap_or(body);
    let cells = body.split('|').map(str::trim).collect::<Vec<_>>();
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let core = cell.trim_matches(':');
            core.len() >= 3 && core.chars().all(|ch| ch == '-')
        })
}

fn take_table_source<'a>(
    header: &'a str,
    lines: &mut std::iter::Peekable<std::str::Split<'a, char>>,
) -> Option<String> {
    if !header.contains('|') || !lines.peek().copied().is_some_and(table_separator_line) {
        return None;
    }
    let mut source = vec![header, lines.next().unwrap_or_default()];
    while let Some(next) = lines.peek().copied().filter(|next| next.contains('|')) {
        source.push(next);
        let _ = lines.next();
    }
    Some(source.join("\n"))
}

fn standalone_image_syntax(line: &str) -> bool {
    let Some(body) = line.trim().strip_prefix("![") else {
        return false;
    };
    let Some((_, destination)) = body.split_once("](") else {
        return false;
    };
    destination
        .strip_suffix(')')
        .is_some_and(|destination| !destination.trim().is_empty())
}

fn take_rich_block<'a>(
    line: &'a str,
    lines: &mut std::iter::Peekable<std::str::Split<'a, char>>,
) -> Option<BlockSpec> {
    if standalone_image_syntax(line) {
        return Some(BlockSpec::paragraph(line));
    }
    take_table_source(line, lines).map(BlockSpec::table)
}

fn parse_list_indent(line: &str) -> (u8, &str) {
    let mut bytes = 0usize;
    let mut level: u8 = 0;
    let b = line.as_bytes();
    while level < MAX_LIST_INDENT {
        if b.get(bytes..bytes + 2)
            .is_some_and(|s| s == b"  ".as_slice())
        {
            bytes += 2;
            level += 1;
        } else if b.get(bytes) == Some(&b'\t') {
            bytes += 1;
            level += 1;
        } else {
            break;
        }
    }
    (level, line.get(bytes..).unwrap_or(""))
}

/// Detect an ordered-list marker (`123. ` or `123) `) at the start of
/// `body`. Returns the text after the marker if present. The number
/// itself is discarded — ordinals are recomputed on render + serialise,
/// so a source file that numbers everything `1.` still displays 1, 2, 3.
fn strip_ordered_prefix(body: &str) -> Option<&str> {
    let bytes = body.as_bytes();
    let mut i = 0;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    // At least one digit, then `. ` or `) `.
    if i == 0 {
        return None;
    }
    match bytes.get(i..i + 2) {
        Some([b'.' | b')', b' ']) => body.get(i + 2..),
        _ => None,
    }
}

fn push_indent(out: &mut String, indent: u8) {
    for _ in 0..indent.min(MAX_LIST_INDENT) {
        out.push_str("  ");
    }
}

/// Parse one line of inline markdown into `(plain_text, marks)`.
/// Marks are stored as byte ranges over `plain_text` (no markdown
/// chrome in the text).
///
/// Tokeniser order:
/// 1. Inline code (backtick) — eats everything literally between
///    the matching backticks so `\*` inside code stays `\*`.
/// 2. Page transclusion (`![[Page Title]]`) or link (`[[Page Title]]`).
/// 3. Underline (`__...__`) or strikethrough (`~~...~~`).
/// 4. Bold (`**...**`).
/// 5. Italic (`*...*`).
///
/// Unmatched markers fall through to the plain text — `*not closed`
/// stays `*not closed` so the round-trip stays byte-stable. Outside code
/// and links a backslash before an [`escapable`] character yields that
/// character; [`serialize_inline`] writes those escapes.
pub fn parse_inline(line: &str) -> InlineText {
    let bytes = line.as_bytes();
    let mut plain = String::with_capacity(line.len());
    let mut marks: Vec<MarkSpan> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        // Inline code.
        if bytes.get(i) == Some(&b'`')
            && let Some(end) = find_unescaped(bytes, i + 1, b'`')
        {
            let start = plain.len();
            plain.push_str(line.get(i + 1..end).unwrap_or_default());
            let stop = plain.len();
            marks.push(MarkSpan::new(start..stop, Mark::Code));
            i = end + 1;
            continue;
        }
        // Page transclusion `![[Title]]`. Run before the ordinary link case.
        if bytes.get(i) == Some(&b'!')
            && bytes.get(i + 1) == Some(&b'[')
            && bytes.get(i + 2) == Some(&b'[')
            && let Some(end) = find_pair(bytes, i + 3, b']', b']')
        {
            let start = plain.len();
            plain.push_str(line.get(i + 3..end).unwrap_or_default());
            let stop = plain.len();
            marks.push(MarkSpan::new(
                start..stop,
                Mark::Transclusion(PageId::nil()),
            ));
            i = end + 2;
            continue;
        }
        // Page link `[[Title]]`.
        if bytes.get(i) == Some(&b'[')
            && bytes.get(i + 1) == Some(&b'[')
            && let Some(end) = find_pair(bytes, i + 2, b']', b']')
        {
            let title = line.get(i + 2..end).unwrap_or_default();
            let start = plain.len();
            plain.push_str(title);
            let stop = plain.len();
            // Use Uuid::nil() as a placeholder; the notes app
            // resolves titles to ids after parse via
            // `resolve_link_ids`.
            marks.push(MarkSpan::new(start..stop, Mark::Link(PageId::nil())));
            i = end + 2;
            continue;
        }
        // Standard external Markdown link. Relative destinations have no
        // stable base in a native Notes page, so only safe absolute web/mail
        // URLs become clickable marks; other syntax remains literal.
        if let Some(next) = parse_external_link(line, i, &mut plain, &mut marks) {
            i = next;
            continue;
        }
        // Underline `__...__` and strikethrough `~~...~~`. These are
        // emitted by `serialize_inline`, so recognizing them here is
        // required for lossless Markdown export -> import.
        if let Some((delimiter, mark)) = paired_mark_at(bytes, i)
            && let Some(next) = parse_paired_mark(line, i, delimiter, mark, &mut plain, &mut marks)
        {
            i = next;
            continue;
        }
        // Bold `**...**` (run before italic so `**` doesn't parse as
        // empty italic).
        if bytes.get(i) == Some(&b'*')
            && bytes.get(i + 1) == Some(&b'*')
            && let Some(end) = find_pair(bytes, i + 2, b'*', b'*')
        {
            // Body may contain nested italic via `*text*`. Recurse.
            let body = line.get(i + 2..end).unwrap_or_default();
            let body_parsed = parse_inline(body);
            let start = plain.len();
            plain.push_str(&body_parsed.text);
            let stop = plain.len();
            marks.push(MarkSpan::new(start..stop, Mark::Bold));
            for m in body_parsed.marks {
                marks.push(MarkSpan::new(
                    (m.range.start + start)..(m.range.end + start),
                    m.mark,
                ));
            }
            i = end + 2;
            continue;
        }
        // Italic `*...*`.
        if bytes.get(i) == Some(&b'*')
            && let Some(end) = find_unescaped(bytes, i + 1, b'*')
        {
            // Empty `**` body wouldn't reach here (bold path
            // above already claimed it). A lone `*` with no
            // closing partner falls through.
            if end > i + 1 {
                let body = line.get(i + 1..end).unwrap_or_default();
                let body_parsed = parse_inline(body);
                let start = plain.len();
                plain.push_str(&body_parsed.text);
                let stop = plain.len();
                marks.push(MarkSpan::new(start..stop, Mark::Italic));
                for m in body_parsed.marks {
                    marks.push(MarkSpan::new(
                        (m.range.start + start)..(m.range.end + start),
                        m.mark,
                    ));
                }
                i = end + 1;
                continue;
            }
        }
        // Fallback: copy the next char verbatim. The markers we
        // scan for are all ASCII, but the body may contain
        // multi-byte UTF-8 — pushing a single byte as `char` mangles
        // those into Latin-1 (e.g. an em-dash `—` becomes three
        // garbled glyphs). Walk by char to preserve the full
        // codepoint. `i` always sits on a char boundary here (it only
        // ever advances by whole `len_utf8()` steps or ASCII-delimiter
        // widths), so this lookup cannot fail in practice; bail out of
        // the loop rather than panicking if that invariant is ever
        // broken.
        let Some(c) = line.get(i..).and_then(|rest| rest.chars().next()) else {
            break;
        };
        let (literal, width) = unescaped(c, line.get(i + c.len_utf8()..).unwrap_or_default());
        plain.push(literal);
        i += width;
    }
    marks.sort_by_key(|m| m.range.start);
    InlineText { text: plain, marks }
}

fn paired_mark_at(bytes: &[u8], at: usize) -> Option<(u8, Mark)> {
    // Double-character toggles whose open delimiter is two identical ASCII
    // bytes (driven by [`mark_chrome`], not a second table).
    for mark in [Mark::Underline, Mark::Strikethrough] {
        let open = open_marker(&mark).as_bytes();
        let (Some(&a), Some(&b)) = (open.first(), open.get(1)) else {
            continue;
        };
        if open.len() == 2 && a == b && bytes.get(at) == Some(&a) && bytes.get(at + 1) == Some(&b) {
            return Some((a, mark));
        }
    }
    None
}

fn parse_external_link(
    line: &str,
    at: usize,
    plain: &mut String,
    marks: &mut Vec<MarkSpan>,
) -> Option<usize> {
    let bytes = line.as_bytes();
    (bytes.get(at) == Some(&b'[') && bytes.get(at + 1) != Some(&b'[')).then_some(())?;
    let label_end = find_unescaped(bytes, at + 1, b']')?;
    (bytes.get(label_end + 1) == Some(&b'(')).then_some(())?;
    let url_end = link_destination_end(bytes, label_end + 2)?;
    let label = line.get(at + 1..label_end)?;
    let url = line.get(label_end + 2..url_end)?;
    (!label.is_empty() && safe_external_url(url)).then_some(())?;
    let label = parse_inline(label);
    let start = plain.len();
    plain.push_str(&label.text);
    marks.push(MarkSpan::new(
        start..plain.len(),
        Mark::ExternalLink(url.to_string()),
    ));
    marks.extend(label.marks.into_iter().map(|inner| {
        MarkSpan::new(
            (inner.range.start + start)..(inner.range.end + start),
            inner.mark,
        )
    }));
    Some(url_end + 1)
}

// Parentheses inside a destination belong to the URL until their matching
// close. Stopping at the first ')' silently changes where a link opens.
fn link_destination_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = start;
    while let Some(&byte) = bytes.get(i) {
        match byte {
            b'\\' => i += 1,
            b'(' => depth += 1,
            b')' if depth == 0 => return Some(i),
            b')' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

fn parse_paired_mark(
    line: &str,
    at: usize,
    delimiter: u8,
    mark: Mark,
    plain: &mut String,
    marks: &mut Vec<MarkSpan>,
) -> Option<usize> {
    let end = find_pair(line.as_bytes(), at + 2, delimiter, delimiter)?;
    (end > at + 2).then_some(())?;
    let body = parse_inline(line.get(at + 2..end).unwrap_or_default());
    let start = plain.len();
    plain.push_str(&body.text);
    marks.push(MarkSpan::new(start..plain.len(), mark));
    marks.extend(body.marks.into_iter().map(|inner| {
        MarkSpan::new(
            (inner.range.start + start)..(inner.range.end + start),
            inner.mark,
        )
    }));
    Some(end + 2)
}

/// Search for `delim` in `bytes[start..]`, ignoring escaped
/// occurrences (`\delim`). Returns the byte index of the first
/// match or `None`.
fn find_unescaped(bytes: &[u8], start: usize, delim: u8) -> Option<usize> {
    let mut i = start;
    while i < bytes.len() {
        if bytes.get(i) == Some(&b'\\') && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes.get(i) == Some(&delim) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Search for an adjacent pair `delim1 delim2` (e.g. `**` or `]]`)
/// in `bytes[start..]`. Returns the byte index of the *first*
/// character of the pair or `None`.
fn find_pair(bytes: &[u8], start: usize, delim1: u8, delim2: u8) -> Option<usize> {
    let mut i = start;
    while i + 1 < bytes.len() {
        if bytes.get(i) == Some(&b'\\') && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes.get(i) == Some(&delim1) && bytes.get(i + 1) == Some(&delim2) {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub fn safe_external_url(value: &str) -> bool {
    url::Url::parse(value)
        .ok()
        .is_some_and(|url| matches!(url.scheme(), "http" | "https" | "mailto"))
}

/// Open/close delimiters and emission priority for a mark kind.
///
/// One table feeds parse ([`paired_mark_at`]) and serialize
/// ([`open_marker`] / [`close_marker`] / [`priority`]). External-link
/// closes are value-dependent and handled in [`close_marker`].
fn mark_chrome(mark: &Mark) -> (&'static str, Option<&'static str>, u8) {
    match mark {
        Mark::Bold => ("**", Some("**"), 0),
        Mark::Italic => ("*", Some("*"), 1),
        Mark::Underline => ("__", Some("__"), 2),
        Mark::Strikethrough => ("~~", Some("~~"), 3),
        Mark::Code => ("`", Some("`"), 4),
        Mark::Link(_) => ("[[", Some("]]"), 5),
        Mark::Transclusion(_) => ("![[", Some("]]"), 6),
        Mark::ExternalLink(_) => ("[", None, 7),
        // Markdown has no mention syntax: the label is the text.
        Mark::Mention(_) => ("", Some(""), 8),
    }
}

fn open_marker(mark: &Mark) -> &'static str {
    mark_chrome(mark).0
}

fn close_marker(mark: &Mark) -> std::borrow::Cow<'_, str> {
    match mark {
        Mark::ExternalLink(url) => format!("]({url})").into(),
        mark => match mark_chrome(mark) {
            (_, Some(close), _) => close.into(),
            // Only [`Mark::ExternalLink`] omits a static close; handled above.
            _ => "".into(),
        },
    }
}

fn priority(m: &Mark) -> u8 {
    mark_chrome(m).2
}

/// Serialise an [`InlineText`] back to a markdown string, weaving
/// the inline markers back over the plain bytes. Output is
/// byte-stable for any text+marks that came out of [`parse_inline`].
///
/// Algorithm: walk `text` byte-by-byte; at each byte, open any mark
/// span whose `range.start == i`, append the byte, then close any
/// span whose `range.end == i+1`. At a shared start the longest span
/// opens first, so contained formatting closes inside its parent. Equal
/// spans use the canonical bold, italic, code/link priority.
pub fn serialize_inline(text: &InlineText) -> String {
    let mut out = String::with_capacity(text.text.len() + text.marks.len() * 4);
    // For each byte position we maintain a stack of currently-open
    // marks (in order of opening). We open marks at their `start`
    // byte (longest first, then Bold → Italic → Code/Link) and close
    // them at their `end` byte in reverse order.
    let mut marks = text.marks.clone();
    marks.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then_with(|| b.range.end.cmp(&a.range.end))
            .then_with(|| priority(&a.mark).cmp(&priority(&b.mark)))
    });

    // Stack of currently-open marks (in emission order).
    let mut open: Vec<&MarkSpan> = Vec::new();
    let mut mi = 0usize; // index into marks for "the next mark to open"
    let bytes_len = text.text.len();
    // Walk byte by byte so opens/closes align with mark `range`
    // boundaries (which are byte indices). For non-ASCII chars
    // we append the multibyte codepoint at its start boundary and
    // skip ahead the rest of its bytes.
    let mut i = 0usize;
    while i <= bytes_len {
        // Close any marks whose range.end == i (deepest first).
        while let Some(top) = open.last() {
            if top.range.end == i {
                out.push_str(&close_marker(&top.mark));
                open.pop();
            } else {
                break;
            }
        }
        while let Some(m) = marks.get(mi).filter(|m| m.range.start == i) {
            open.push(m);
            out.push_str(open_marker(&m.mark));
            mi += 1;
        }
        if i < bytes_len {
            // Push the *char* starting at byte i, not the raw byte —
            // mark ranges are still byte indices, but the rendered
            // text uses full UTF-8 codepoints. `i` only ever advances
            // by whole `len_utf8()` steps, so it always sits on a char
            // boundary; bail out rather than panic if that ever breaks.
            let Some(c) = text.text.get(i..).and_then(|rest| rest.chars().next()) else {
                break;
            };
            let literal = open.iter().any(|span| literal_mark(&span.mark));
            if !literal && needs_escape(c, &out, text, i + c.len_utf8()) {
                out.push('\\');
            }
            out.push(c);
            i += c.len_utf8();
        } else {
            i += 1;
        }
    }
    out
}

/// Marks whose text the parser takes verbatim, so nothing inside is escaped.
fn literal_mark(mark: &Mark) -> bool {
    matches!(mark, Mark::Code | Mark::Link(_) | Mark::Transclusion(_))
}

/// Whether plain text character `c`, written after `out` and followed by
/// the text from byte `next`, would read back as markup.
fn needs_escape(c: char, out: &str, text: &InlineText, next: usize) -> bool {
    let doubled = |mark: Mark| {
        out.ends_with(c)
            || text
                .text
                .get(next..)
                .is_some_and(|rest| rest.starts_with(c))
            || text.marks.iter().any(|span| {
                span.mark == mark && (span.range.start == next || span.range.end == next)
            })
    };
    match c {
        '\\' | '*' | '`' | '[' | ']' => true,
        '_' => doubled(Mark::Underline),
        '~' => doubled(Mark::Strikethrough),
        _ => false,
    }
}

/// The plain character at `c`, followed by `rest`, and the bytes it takes:
/// a backslash before an [`escapable`] character is that character.
fn unescaped(c: char, rest: &str) -> (char, usize) {
    match rest.chars().next() {
        Some(next) if c == '\\' && escapable(next) => (next, 1 + next.len_utf8()),
        _ => (c, c.len_utf8()),
    }
}

/// The characters a backslash makes literal: ASCII punctuation, as in
/// CommonMark, the space and tab a text may start with, and a line break
/// (CommonMark's backslash hard break), which a text may start with too.
fn escapable(c: char) -> bool {
    c.is_ascii_punctuation() || matches!(c, ' ' | '\t' | '\n')
}

/// Parse a markdown string into a flat list of [`BlockSpec`]s.
///
/// Empty input → a single empty paragraph.
#[allow(
    clippy::too_many_lines,
    reason = "single-pass Markdown block parsing keeps lookahead and fence state local"
)]
pub fn parse(md: &str) -> Vec<BlockSpec> {
    let mut blocks = Vec::new();
    let mut lines = md.split('\n').peekable();
    while let Some(line) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        let (depth, body) = parse_list_indent(line);
        if depth > 0
            && !body.starts_with("- ")
            && !body.starts_with("* ")
            && strip_ordered_prefix(body).is_none()
        {
            // Parse a nested region with its structural indentation removed.
            // Code indentation inside the fence remains ordinary source text.
            // A continuation under `1. ` is indented three spaces in
            // CommonMark, one more than a level here: that odd space is
            // indentation too, not the text's first character.
            let mut nested = body.trim_start().to_string();
            while let Some(next) = lines.peek().copied() {
                if !next.is_empty() && parse_list_indent(next).0 < depth {
                    break;
                }
                let _ = lines.next();
                nested.push('\n');
                let mut content = next;
                for _ in 0..depth {
                    content = content
                        .strip_prefix("  ")
                        .or_else(|| content.strip_prefix('\t'))
                        .unwrap_or(content);
                }
                nested.push_str(content);
            }
            blocks.extend(
                parse(&nested)
                    .into_iter()
                    .map(|block| block.indented(depth)),
            );
            continue;
        }
        if line == EMPTY_PARAGRAPH {
            blocks.push(BlockSpec::paragraph(""));
            continue;
        }
        if let Some(block) = take_rich_block(line, &mut lines) {
            blocks.push(block);
            continue;
        }
        if let Some((ticks, rest)) = code_fence(line) {
            let lang = if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
            let mut body: Vec<&str> = Vec::new();
            for next in lines.by_ref() {
                if backticks_only(next).is_some_and(|closing| closing >= ticks) {
                    break;
                }
                body.push(next);
            }
            blocks.push(BlockSpec::code_block(lang, body.join("\n")));
            continue;
        }
        if line == "---" {
            blocks.push(BlockSpec::divider());
            continue;
        }
        if let Some(kind) = line
            .strip_prefix('>')
            .and_then(ui::widget::CalloutKind::from_marker)
        {
            let body = take_quoted_lines(&mut lines);
            blocks.push(BlockSpec::Callout {
                kind,
                text: parse_inline(&body),
            });
            continue;
        }
        let joined = join_hard_breaks(line, &mut lines);
        let line = joined.as_str();
        if let Some(rest) = line.strip_prefix("> ") {
            blocks.push(BlockSpec::Quote(parse_inline(rest)));
            continue;
        }
        let (indent, body) = parse_list_indent(line);
        if let Some(check_body) = body
            .strip_prefix("- [ ] ")
            .or_else(|| body.strip_prefix("* [ ] "))
        {
            let inline = parse_inline(check_body);
            blocks.push(BlockSpec::Checklist {
                indent: indent.min(MAX_LIST_INDENT),
                done: false,
                text: inline,
            });
            continue;
        }
        if let Some(check_body) = body
            .strip_prefix("- [x] ")
            .or_else(|| body.strip_prefix("* [x] "))
            .or_else(|| body.strip_prefix("- [X] "))
            .or_else(|| body.strip_prefix("* [X] "))
        {
            let inline = parse_inline(check_body);
            blocks.push(BlockSpec::Checklist {
                indent: indent.min(MAX_LIST_INDENT),
                done: true,
                text: inline,
            });
            continue;
        }
        if let Some(bullet_body) = body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")) {
            let inline = parse_inline(bullet_body);
            blocks.push(BlockSpec::Bullet {
                indent: indent.min(MAX_LIST_INDENT),
                text: inline,
            });
            continue;
        }
        if let Some(num_body) = strip_ordered_prefix(body) {
            let inline = parse_inline(num_body);
            blocks.push(BlockSpec::Numbered {
                indent: indent.min(MAX_LIST_INDENT),
                text: inline,
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix("### ") {
            blocks.push(BlockSpec::Heading {
                level: 3,
                text: parse_inline(rest),
            });
        } else if let Some(rest) = line.strip_prefix("## ") {
            blocks.push(BlockSpec::Heading {
                level: 2,
                text: parse_inline(rest),
            });
        } else if let Some(rest) = line.strip_prefix("# ") {
            blocks.push(BlockSpec::Heading {
                level: 1,
                text: parse_inline(rest),
            });
        } else {
            blocks.push(BlockSpec::Paragraph(parse_inline(line)));
        }
    }
    if blocks.is_empty() {
        blocks.push(BlockSpec::paragraph(""));
    }
    blocks
}

/// A code fence's backticks (three or more, as many as it starts with) and
/// the language after them.
fn code_fence(line: &str) -> Option<(usize, &str)> {
    let rest = line.trim_start_matches('`');
    let ticks = line.len() - rest.len();
    (ticks >= 3).then_some((ticks, rest))
}

/// How many backticks `line` is, when it is nothing else (trailing
/// whitespace aside): such a line closes a fence at least as long.
fn backticks_only(line: &str) -> Option<usize> {
    let line = line.trim_end();
    (!line.is_empty() && line.bytes().all(|byte| byte == b'`')).then_some(line.len())
}

/// The backticks a code block's fence takes: three, or one more than the
/// longest line of its text that is only backticks, so a fence the code
/// shows (Markdown about Markdown) stays code instead of ending the block.
/// Indentation is ignored: Markdown closes a fence on a line indented up
/// to three spaces, such as the fence of a list item the code shows.
fn fence_length(text: &str) -> usize {
    text.split('\n')
        .filter_map(|line| backticks_only(line.trim_start()))
        .max()
        .map_or(3, |longest| longest.saturating_add(1).max(3))
}

/// The quoted lines that follow an alert marker, as one text: a line ending
/// in a hard break (two spaces) or an empty one breaks the text there, any
/// other line continues it after a space, as GitHub wraps a quote.
fn take_quoted_lines(lines: &mut std::iter::Peekable<std::str::Split<'_, char>>) -> String {
    let mut body = String::new();
    let mut hard_break = None;
    while let Some(quoted) = lines.peek().and_then(|next| next.strip_prefix('>')) {
        let quoted = quoted.strip_prefix(' ').unwrap_or(quoted);
        let (content, breaks) = quoted
            .strip_suffix("  ")
            .map_or((quoted, quoted.is_empty()), |content| (content, true));
        if let Some(previous) = hard_break {
            body.push(if previous { '\n' } else { ' ' });
        }
        body.push_str(content);
        hard_break = Some(breaks);
        let _ = lines.next();
    }
    body
}

// Hard line breaks belong to a single text block. Continuation indentation is
// presentation syntax, not text, and is removed before parsing inline marks.
fn join_hard_breaks<'a>(
    line: &'a str,
    lines: &mut std::iter::Peekable<std::str::Split<'a, char>>,
) -> String {
    let (depth, body) = parse_list_indent(line);
    let prefix = if line.starts_with("> ") {
        "> ".to_string()
    } else {
        let marker = if body.starts_with("- [ ] ")
            || body.starts_with("- [x] ")
            || body.starts_with("- [X] ")
            || body.starts_with("* [ ] ")
            || body.starts_with("* [x] ")
            || body.starts_with("* [X] ")
        {
            6
        } else if body.starts_with("- ") || body.starts_with("* ") {
            2
        } else {
            strip_ordered_prefix(body).map_or(0, |rest| body.len() - rest.len())
        };
        " ".repeat(if marker == 0 {
            0
        } else {
            usize::from(depth) * 2 + marker
        })
    };
    let mut joined = line.to_string();
    while joined.ends_with("  ") && lines.peek().is_some() {
        let next = lines.next().unwrap_or_default();
        joined.truncate(joined.len() - 2);
        joined.push('\n');
        joined.push_str(next.strip_prefix(&prefix).unwrap_or(next));
    }
    joined
}

/// A text's source with its line breaks as hard breaks. Whitespace the text
/// starts with is escaped, since the parser takes leading whitespace as
/// indentation and a first line with nothing on it as a blank line, and so
/// are two trailing spaces, which would read back as a hard break into the
/// next line.
fn serialize_hard_breaks(text: &InlineText, continuation: &str) -> String {
    let mut inline = serialize_inline(text);
    if inline.ends_with("  ") {
        inline.insert(inline.len() - 1, '\\');
    }
    // A table separator after a hard break would make the first line a header.
    if let Some((first, rest)) = inline.split_once('\n')
        && first.contains('|')
        && let Some(separator) = rest
            .split('\n')
            .next()
            .filter(|line| table_separator_line(line))
    {
        let at = first.len() + 1 + (separator.len() - separator.trim_start().len());
        inline.insert(at, '\\');
    }
    if inline.starts_with([' ', '\t', '\n']) {
        inline.insert(0, '\\');
    }
    inline.replace('\n', &format!("  \n{continuation}"))
}

/// A paragraph's source line: its text, escaped where its start would read
/// back as another kind of block. An empty paragraph is [`EMPTY_PARAGRAPH`],
/// so it stays a block of its own.
fn serialize_paragraph(text: &InlineText) -> String {
    if text.marks.is_empty()
        && !text.text.starts_with(char::is_whitespace)
        && !text.text.contains('\n')
        && standalone_image_syntax(&text.text)
    {
        return text.text.clone();
    }
    if text.text.is_empty() {
        return EMPTY_PARAGRAPH.to_owned();
    }
    let mut out = serialize_hard_breaks(text, "");
    let block_marker = ["# ", "## ", "### ", "> ", "- "]
        .iter()
        .any(|marker| out.starts_with(marker));
    if block_marker || out == "---" || out == EMPTY_PARAGRAPH {
        out.insert(0, '\\');
    } else if let Some(rest) = strip_ordered_prefix(&out) {
        let marker = out.len() - rest.len() - 2;
        out.insert(marker, '\\');
    }
    out
}

/// The source of an empty paragraph that is not the whole document: a
/// non-breaking space, which other Markdown readers show as an empty
/// paragraph too.
const EMPTY_PARAGRAPH: &str = "&nbsp;";

/// A callout as a GitHub alert: its marker line, then its quoted text.
fn serialize_callout(out: &mut String, kind: ui::widget::CalloutKind, text: &InlineText) {
    out.push_str("> ");
    out.push_str(&kind.marker());
    if !text.text.is_empty() {
        out.push_str("\n> ");
        out.push_str(&serialize_hard_breaks(text, "> "));
    }
}

/// Inverse of [`parse`]. Always emits a trailing newline.
pub fn serialize(blocks: &[BlockSpec]) -> String {
    if let [BlockSpec::Paragraph(text)] = blocks
        && text.text.is_empty()
    {
        return "\n".to_owned();
    }
    serialize_blocks(blocks)
}

fn serialize_blocks(blocks: &[BlockSpec]) -> String {
    let mut out = String::new();
    // Ordinal counters per indent level for numbered lists, mirroring the
    // renderer's `numbered_ordinals`: a numbered item increments its
    // level and resets deeper ones; mixed list children retain shallower counters.
    let mut counters = [0usize; MAX_LIST_INDENT as usize + 1];
    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        if let BlockSpec::Numbered { indent, .. } = b {
            let lvl = (*indent as usize).min(MAX_LIST_INDENT as usize);
            if let Some(c) = counters.get_mut(lvl) {
                *c += 1;
            }
            for c in counters.iter_mut().skip(lvl + 1) {
                *c = 0;
            }
        } else {
            let depth = match b {
                BlockSpec::Bullet { indent, .. } | BlockSpec::Checklist { indent, .. } => {
                    usize::from(*indent)
                }
                BlockSpec::Nested { depth, .. } => usize::from(*depth),
                _ => 0,
            };
            for counter in counters.iter_mut().skip(depth) {
                *counter = 0;
            }
        }
        match b {
            BlockSpec::Nested { depth, block } => {
                let source = serialize_blocks(std::slice::from_ref(block.as_ref()));
                let source = source.strip_suffix('\n').unwrap_or(&source);
                for (index, line) in source.split('\n').enumerate() {
                    if index > 0 {
                        out.push('\n');
                    }
                    push_indent(&mut out, *depth);
                    out.push_str(line);
                }
            }
            BlockSpec::Paragraph(t) => out.push_str(&serialize_paragraph(t)),
            BlockSpec::Quote(t) => {
                out.push_str("> ");
                out.push_str(&serialize_hard_breaks(t, "> "));
            }
            BlockSpec::Callout { kind, text } => serialize_callout(&mut out, *kind, text),
            BlockSpec::Heading { level, text } => {
                let prefix = match *level {
                    1 => "# ",
                    2 => "## ",
                    _ => "### ",
                };
                out.push_str(prefix);
                out.push_str(&serialize_hard_breaks(text, ""));
            }
            BlockSpec::Bullet { indent, text } => {
                push_indent(&mut out, *indent);
                out.push_str("- ");
                out.push_str(&serialize_hard_breaks(
                    text,
                    &" ".repeat(usize::from(*indent) * 2 + 2),
                ));
            }
            BlockSpec::Numbered { indent, text } => {
                push_indent(&mut out, *indent);
                let n = counters
                    .get((*indent as usize).min(MAX_LIST_INDENT as usize))
                    .copied()
                    .unwrap_or(0);
                let _ = write!(out, "{n}. ");
                out.push_str(&serialize_hard_breaks(
                    text,
                    &" ".repeat(usize::from(*indent) * 2 + n.to_string().len() + 2),
                ));
            }
            BlockSpec::Checklist { indent, done, text } => {
                push_indent(&mut out, *indent);
                out.push_str(if *done { "- [x] " } else { "- [ ] " });
                out.push_str(&serialize_hard_breaks(
                    text,
                    &" ".repeat(usize::from(*indent) * 2 + 6),
                ));
            }
            BlockSpec::CodeBlock { lang, text } => {
                let fence = "`".repeat(fence_length(text));
                out.push_str(&fence);
                if let Some(l) = lang {
                    out.push_str(l);
                }
                out.push('\n');
                out.push_str(text);
                out.push('\n');
                out.push_str(&fence);
            }
            BlockSpec::Table { source } => out.push_str(source),
            BlockSpec::Divider => {
                out.push_str("---");
            }
        }
    }
    out.push('\n');
    out
}

/// Build a [`ui::widget::Block`] from a kind and plain text. Shared by
/// [`specs_to_blocks`]; marks/nesting/code-group are applied by the caller.
fn block_from_kind(kind: ui::widget::BlockKind, text: &str) -> ui::widget::Block {
    use ui::widget::{Block, BlockKind};
    match kind {
        BlockKind::Paragraph => Block::paragraph(text),
        BlockKind::Quote => Block::quote(text),
        BlockKind::Callout(kind) => Block::callout(kind, text),
        BlockKind::Heading(level) => Block::heading(level, text),
        BlockKind::Bullet { indent } => Block::bullet(indent, text),
        BlockKind::Numbered { indent } => Block::numbered(indent, text),
        BlockKind::Checklist { indent, done } => Block::checklist(indent, done, text),
        BlockKind::CodeBlock { lang } => Block::code_block(lang, text),
        BlockKind::Table { visual } => Block::table(text, visual),
        BlockKind::Divider => Block::divider(),
    }
}

/// Map a simple (non-fence) editor kind + inline text back to a [`BlockSpec`].
/// Fence rows need multi-line reassembly in [`blocks_to_specs`].
fn spec_from_kind(kind: &ui::widget::BlockKind, inline: InlineText) -> Option<BlockSpec> {
    use ui::widget::BlockKind;
    match kind {
        BlockKind::Paragraph => Some(BlockSpec::Paragraph(inline)),
        BlockKind::Quote => Some(BlockSpec::Quote(inline)),
        BlockKind::Callout(kind) => Some(BlockSpec::Callout {
            kind: *kind,
            text: inline,
        }),
        BlockKind::Heading(level) => Some(BlockSpec::Heading {
            level: *level,
            text: inline,
        }),
        BlockKind::Bullet { indent } => Some(BlockSpec::Bullet {
            indent: *indent,
            text: inline,
        }),
        BlockKind::Numbered { indent } => Some(BlockSpec::Numbered {
            indent: *indent,
            text: inline,
        }),
        BlockKind::Checklist { indent, done } => Some(BlockSpec::Checklist {
            indent: *indent,
            done: *done,
            text: inline,
        }),
        BlockKind::Table { .. } => Some(BlockSpec::Table {
            source: inline.text,
        }),
        BlockKind::Divider => Some(BlockSpec::Divider),
        BlockKind::CodeBlock { .. } => None,
    }
}

/// Convert a list of [`BlockSpec`]s into a list of
/// [`ui::widget::Block`]s suitable for handing to
/// [`ui::widget::BlockEditor::set_blocks`]. Mark spans on each block
/// land on the inner `RichTextLine`'s mark list.
pub fn specs_to_blocks(specs: &[BlockSpec]) -> Vec<ui::widget::Block> {
    let mut groups = std::collections::HashMap::new();
    expanded_rows(specs)
        .into_iter()
        .map(|(kind, inline, fence, nesting)| {
            let mut block = block_from_kind(kind, &inline.text);
            block.line.set_marks(inline.marks);
            block.nesting = nesting;
            block.code_group = fence.map(|fence| {
                groups
                    .entry(fence)
                    .or_insert_with(|| block.id.clone())
                    .clone()
            });
            block
        })
        .collect()
}

/// Data-only expansion shared by the legacy editor and canonical importer.
/// Fenced code has one durable row per line, including one row for an empty fence.
pub(crate) fn expanded_rows(
    specs: &[BlockSpec],
) -> Vec<(ui::widget::BlockKind, InlineText, Option<usize>, u8)> {
    use ui::widget::BlockKind;
    let mut rows = Vec::new();
    for (fence, spec) in specs.iter().enumerate() {
        let (kind, text) = match spec {
            BlockSpec::Nested { depth, block } => {
                for (mut kind, text, group, nesting) in
                    expanded_rows(std::slice::from_ref(block.as_ref()))
                {
                    let nesting = match &mut kind {
                        BlockKind::Bullet { indent }
                        | BlockKind::Numbered { indent }
                        | BlockKind::Checklist { indent, .. } => {
                            *indent = (*indent + depth).min(MAX_LIST_INDENT);
                            0
                        }
                        _ => (nesting + depth).min(MAX_LIST_INDENT),
                    };
                    rows.push((kind, text, group.map(|_| fence), nesting));
                }
                continue;
            }
            BlockSpec::Paragraph(text) => (BlockKind::Paragraph, text),
            BlockSpec::Quote(text) => (BlockKind::Quote, text),
            BlockSpec::Callout { kind, text } => (BlockKind::Callout(*kind), text),
            BlockSpec::Heading { level, text } => (BlockKind::Heading(*level), text),
            BlockSpec::Bullet { indent, text } => (BlockKind::Bullet { indent: *indent }, text),
            BlockSpec::Numbered { indent, text } => (BlockKind::Numbered { indent: *indent }, text),
            BlockSpec::Checklist { indent, done, text } => (
                BlockKind::Checklist {
                    indent: *indent,
                    done: *done,
                },
                text,
            ),
            BlockSpec::CodeBlock { lang, text } => {
                for line in text.split('\n') {
                    rows.push((
                        BlockKind::CodeBlock { lang: lang.clone() },
                        InlineText::plain(line),
                        Some(fence),
                        0,
                    ));
                }
                continue;
            }
            BlockSpec::Table { source } => {
                rows.push((
                    BlockKind::Table { visual: true },
                    InlineText::plain(source),
                    None,
                    0,
                ));
                continue;
            }
            BlockSpec::Divider => {
                rows.push((BlockKind::Divider, InlineText::plain(""), None, 0));
                continue;
            }
        };
        rows.push((kind, text.clone(), None, 0));
    }
    rows
}

/// Inverse of [`specs_to_blocks`] — convert the editor's block list
/// back into the spec representation so [`serialize`] can emit
/// markdown that round-trips through the inline-marks layer.
pub fn blocks_to_specs(blocks: &[ui::widget::Block]) -> Vec<BlockSpec> {
    use ui::widget::BlockKind;
    let mut out = Vec::with_capacity(blocks.len());
    let mut i = 0usize;
    while let Some(b) = blocks.get(i) {
        if let BlockKind::CodeBlock { lang } = &b.kind {
            // Reassemble the rows of one authored fence. Language alone
            // cannot distinguish adjacent fences using the same syntax.
            let mut lines: Vec<String> = vec![b.text().to_string()];
            let run_lang = lang.clone();
            i += 1;
            while let Some(next) = blocks.get(i) {
                if b.same_code_group(next) {
                    lines.push(next.text().to_string());
                    i += 1;
                    continue;
                }
                break;
            }
            out.push(BlockSpec::CodeBlock {
                lang: run_lang,
                text: lines.join("\n"),
            });
        } else {
            let inline = InlineText::new(b.text(), b.line.marks().to_vec());
            if let Some(spec) = spec_from_kind(&b.kind, inline) {
                out.push(spec);
            }
            i += 1;
        }
        if b.nesting > 0
            && let Some(spec) = out.pop()
        {
            out.push(spec.indented(b.nesting));
        }
    }
    out
}

/// Walk every block in `blocks` and replace `Mark::Link(Uuid::nil())`
/// with `Mark::Link(resolver(&title))` where `title` is the byte
/// slice the link covers. Used by the notes app after parse to bind
/// page-link marks to the workspace's current page ids.
pub fn resolve_link_ids<F>(blocks: &mut [BlockSpec], mut resolver: F)
where
    F: FnMut(&str) -> Option<PageId>,
{
    for b in blocks {
        let mut b = b;
        while let BlockSpec::Nested { block, .. } = b {
            b = block;
        }
        let (BlockSpec::Paragraph(inline)
        | BlockSpec::Quote(inline)
        | BlockSpec::Callout { text: inline, .. }
        | BlockSpec::Heading { text: inline, .. }
        | BlockSpec::Bullet { text: inline, .. }
        | BlockSpec::Numbered { text: inline, .. }
        | BlockSpec::Checklist { text: inline, .. }) = b
        else {
            continue;
        };
        for m in &mut inline.marks {
            let replacement = match &m.mark {
                Mark::Link(id) if id.is_nil() => Some(false),
                Mark::Transclusion(id) if id.is_nil() => Some(true),
                _ => None,
            };
            if let Some(transclusion) = replacement
                && let Some(title) = inline.text.get(m.range.clone())
                && let Some(new_id) = resolver(title)
            {
                m.mark = if transclusion {
                    Mark::Transclusion(new_id)
                } else {
                    Mark::Link(new_id)
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// A paragraph continuing an ordered item, indented the CommonMark way
    /// (three spaces under `1. `), is a line of that item without a stray
    /// leading space.
    #[test]
    fn a_three_space_continuation_is_a_line_of_the_item() {
        let blocks = specs_to_blocks(&parse(
            "1. Pack the books\n   The heavy ones first.\n2. Next\n",
        ));
        let texts: Vec<_> = blocks
            .iter()
            .map(|block| (block.text().to_owned(), block.indent()))
            .collect();
        assert_eq!(
            texts,
            [
                ("Pack the books".to_owned(), 0),
                ("The heavy ones first.".to_owned(), 1),
                ("Next".to_owned(), 0),
            ]
        );
    }

    #[test]
    fn document_markdown_round_trips_byte_stably_and_normalises_on_save() {
        // (case, markdown that must survive parse -> serialize unchanged)
        let round_trips = [
            (
                "typical document",
                "# Title\n\nFirst paragraph.\n\nSecond paragraph.\n",
            ),
            ("paragraphs only", "alpha\n\nbeta\n\ngamma\n"),
            ("paragraph with bold", "Hello **world** today.\n"),
            ("heading with italic", "# A *taste* of\n"),
            ("bullet with code", "- run `cargo build` first\n"),
            ("paragraph with link", "See [[Welcome]] for details.\n"),
            ("nested bold italic", "***italic* bold**\n"),
            (
                "mixed corpus",
                "\
# Welcome **dear** friend

This paragraph has *italic* and **bold** and `code`.

- bullet with [[Welcome]] link

- [ ] todo with **bold**

Last paragraph.
",
            ),
            ("numbered list", "1. first\n\n2. second\n\n3. third\n"),
            // A nested level restarts at 1; returning to the outer level
            // continues its own count.
            (
                "nested numbered list",
                "1. one\n\n  1. inner a\n\n  2. inner b\n\n2. two\n",
            ),
            // A paragraph between two numbered lists breaks the run: the
            // second list restarts at 1.
            (
                "numbered run resets after a paragraph",
                "1. a\n\n2. b\n\nbreak\n\n1. c\n",
            ),
            (
                "numbered with inline marks",
                "1. run `cargo build` and **ship**\n",
            ),
            (
                "gfm table",
                "| Name | Status |\n| :--- | ---: |\n| Search | Done |\n",
            ),
        ];
        for (case, md) in round_trips {
            assert_eq!(serialize(&parse(md)), md, "case {case}");
        }

        // Source that numbers everything `1.` is normalised to 1, 2, 3 on
        // save — ordinals track list position, not the literal digits.
        assert_eq!(
            serialize(&parse("1. a\n\n1. b\n\n1. c\n")),
            "1. a\n\n2. b\n\n3. c\n"
        );
        let blocks = vec![BlockSpec::Heading {
            level: 99,
            text: InlineText::plain("x"),
        }];
        assert_eq!(serialize(&blocks), "### x\n", "heading level clamps");

        // A matching import heading is not duplicated below the note title.
        assert_eq!(
            strip_matching_title_heading("# Project brief\n\nFirst paragraph\n", "Project brief"),
            "First paragraph\n"
        );
        assert_eq!(
            strip_matching_title_heading("# Different\n\nFirst paragraph\n", "Project brief"),
            "# Different\n\nFirst paragraph\n"
        );
    }

    #[test]
    fn block_parsing_recognises_each_block_kind() {
        // (case, markdown, exactly the parsed blocks)
        let exact = [
            (
                "empty yields one empty paragraph",
                "",
                vec![BlockSpec::paragraph("")],
            ),
            (
                "punctuation stays paragraph text",
                "Hello, world.\n",
                vec![BlockSpec::paragraph("Hello, world.")],
            ),
            (
                "a lone hash is a paragraph, not a heading",
                "#\n",
                vec![BlockSpec::paragraph("#")],
            ),
            (
                "numbered list items",
                "1. first\n\n2. second\n\n3. third\n",
                vec![
                    BlockSpec::numbered(0, "first"),
                    BlockSpec::numbered(0, "second"),
                    BlockSpec::numbered(0, "third"),
                ],
            ),
            // `7) ` is a valid ordered marker; the stored spec drops the
            // number (it's recomputed), so it round-trips to `1. `.
            (
                "paren marker with an arbitrary start",
                "7) lucky\n",
                vec![BlockSpec::numbered(0, "lucky")],
            ),
        ];
        for (case, md, want) in exact {
            assert_eq!(parse(md), want, "case {case}");
        }

        let blocks = parse("# H1\n\n## H2\n\n### H3\n");
        match &blocks[0] {
            BlockSpec::Heading { level, text } => {
                assert_eq!(*level, 1);
                assert_eq!(text.text, "H1");
            }
            _ => panic!("expected heading"),
        }
        assert_eq!(blocks.len(), 3);

        let parsed = parse("A | B | C\n:--- | ---: | :---:\nx | y | z\n");
        assert!(
            matches!(parsed.as_slice(), [BlockSpec::Table { .. }]),
            "every alignment separator is a table"
        );
        let parsed = parse("Use A | B when discussing alternatives.\n");
        assert!(
            matches!(parsed.as_slice(), [BlockSpec::Paragraph(_)]),
            "ordinary pipe text is not promoted to a table"
        );

        let md = "![Architecture diagram](diagram.png)\n";
        let parsed = parse(md);
        assert_eq!(serialize(&parsed), md);
        let [BlockSpec::Paragraph(inline)] = parsed.as_slice() else {
            panic!("standalone image remains one paragraph-backed image block");
        };
        assert!(
            inline.marks.is_empty(),
            "a standalone image is not a link mark"
        );
    }

    #[test]
    fn inline_markdown_parses_marks_and_round_trips() {
        // (source, visible text, exactly the marks)
        let cases = [
            ("**hello**", "hello", vec![MarkSpan::new(0..5, Mark::Bold)]),
            ("*hi*", "hi", vec![MarkSpan::new(0..2, Mark::Italic)]),
            ("`foo`", "foo", vec![MarkSpan::new(0..3, Mark::Code)]),
            (
                "__under__ and ~~gone~~",
                "under and gone",
                vec![
                    MarkSpan::new(0..5, Mark::Underline),
                    MarkSpan::new(10..14, Mark::Strikethrough),
                ],
            ),
            (
                "hello **world** bye",
                "hello world bye",
                vec![MarkSpan::new(6..11, Mark::Bold)],
            ),
            (
                "Read [the guide](https://example.test/docs?q=notes).",
                "Read the guide.",
                vec![MarkSpan::new(
                    5..14,
                    Mark::ExternalLink("https://example.test/docs?q=notes".into()),
                )],
            ),
            // A lone `*` with no partner stays as plain text.
            ("*hello world", "*hello world", vec![]),
            (
                "[script](javascript:alert(1))",
                "[script](javascript:alert(1))",
                vec![],
            ),
            ("[local](../private)", "[local](../private)", vec![]),
            (
                "[file](file:///etc/passwd)",
                "[file](file:///etc/passwd)",
                vec![],
            ),
        ];
        for (md, text, marks) in cases {
            let inline = parse_inline(md);
            assert_eq!(inline.text, text, "text of {md:?}");
            assert_eq!(inline.marks, marks, "marks of {md:?}");
        }
        assert_eq!(
            serialize_inline(&parse_inline(
                "Read [the guide](https://example.test/docs?q=notes)."
            )),
            "Read [the guide](https://example.test/docs?q=notes).",
            "an external link round-trips as a durable mark"
        );

        let inline = parse_inline("hello [[Welcome]] there");
        assert_eq!(inline.text, "hello Welcome there");
        let m = &inline.marks[0];
        assert!(matches!(m.mark, Mark::Link(_)));
        assert_eq!(inline.text.get(m.range.clone()), Some("Welcome"));

        for md in [
            "plain text",
            "**bold**",
            "*italic*",
            "`code`",
            "__underline__",
            "~~strikethrough~~",
            "__under__",
            "~~gone~~",
            "__under ~~and gone~~__",
            "hello **world** bye",
            "hello [[Page]] there",
            "[guide](https://example.test/guide)",
            "![[Page]]",
            "***italic* bold**",
            "a `b` c **d** e",
        ] {
            assert_eq!(
                serialize_inline(&parse_inline(md)),
                md,
                "round-trip failed for {md:?}"
            );
        }
        // The first `**` closes the bold, so the stars left over are text,
        // and they come back as text.
        let ambiguous = parse_inline("**bold *italic***");
        assert_eq!(ambiguous.text, "bold *italic*");
        assert_eq!(serialize_inline(&ambiguous), "**bold \\*italic**\\*");
        assert_eq!(parse_inline(&serialize_inline(&ambiguous)), ambiguous);
    }

    #[test]
    fn links_and_transclusions_resolve_to_stable_page_ids() {
        let mut blocks = parse("See [[Welcome]] please.\n");
        let target = Uuid::from_u128(42);
        resolve_link_ids(&mut blocks, |title| (title == "Welcome").then_some(target));
        match &blocks[0] {
            BlockSpec::Paragraph(t) => {
                assert_eq!(t.marks.len(), 1);
                match &t.marks[0].mark {
                    Mark::Link(id) => assert_eq!(*id, target),
                    _ => panic!("expected Link"),
                }
            }
            _ => panic!("expected paragraph"),
        }

        let mut blocks = parse("![[Roadmap]]\n");
        assert_eq!(serialize(&blocks), "![[Roadmap]]\n");
        let target = Uuid::from_u128(0xfeed);
        resolve_link_ids(&mut blocks, |title| (title == "Roadmap").then_some(target));
        let BlockSpec::Paragraph(inline) = &blocks[0] else {
            panic!("transclusion should remain a paragraph block")
        };
        assert_eq!(inline.text, "Roadmap");
        assert!(matches!(
            inline.marks[0].mark,
            Mark::Transclusion(id) if id == target
        ));
    }

    #[test]
    fn every_github_alert_round_trips_as_its_callout() {
        use ui::widget::{BlockKind, CalloutKind};
        let through_blocks = |md: &str| serialize(&blocks_to_specs(&specs_to_blocks(&parse(md))));
        for (marker, kind) in [
            ("NOTE", CalloutKind::Note),
            ("TIP", CalloutKind::Tip),
            ("IMPORTANT", CalloutKind::Important),
            ("WARNING", CalloutKind::Warning),
            ("CAUTION", CalloutKind::Caution),
        ] {
            let markdown = format!("> [!{marker}]\n> Mind the **gap**  \n> twice\n\nAfter\n");
            let parsed = parse(&markdown);
            let BlockSpec::Callout {
                kind: parsed_kind,
                text,
            } = &parsed[0]
            else {
                panic!("{marker} is a callout: {parsed:?}");
            };
            assert_eq!(*parsed_kind, kind);
            assert_eq!(text.text, "Mind the gap\ntwice");
            assert!(text.marks.iter().any(|mark| mark.mark == Mark::Bold));
            assert_eq!(parsed[1], BlockSpec::paragraph("After"));
            assert_eq!(serialize(&parsed), markdown, "{marker}");
            assert_eq!(
                specs_to_blocks(&parsed)[0].kind,
                BlockKind::Callout(kind),
                "{marker}"
            );
            assert_eq!(through_blocks(&markdown), markdown, "{marker}");
        }
        // An empty callout keeps its marker; GitHub's soft-wrapped lines
        // join with a space; a plain quote stays a quote.
        assert_eq!(through_blocks("> [!TIP]\n"), "> [!TIP]\n");
        let wrapped = parse("> [!note]\n> one\n> two\n");
        assert!(matches!(
            &wrapped[0],
            BlockSpec::Callout { kind: CalloutKind::Note, text } if text.text == "one two"
        ));
        assert!(matches!(parse("> [!NOPE] x\n")[0], BlockSpec::Quote(_)));
    }

    #[test]
    fn editor_blocks_round_trip_quotes_code_formulas_numbered_lists_and_tables() {
        use ui::widget::BlockKind;
        let through_blocks = |md: &str| serialize(&blocks_to_specs(&specs_to_blocks(&parse(md))));

        let markdown = "> A **useful** quotation\n";
        let parsed = parse(markdown);
        let BlockSpec::Quote(text) = &parsed[0] else {
            panic!("expected quote");
        };
        assert_eq!(text.text, "A useful quotation");
        assert!(text.marks.iter().any(|mark| mark.mark == Mark::Bold));
        assert_eq!(serialize(&parsed), markdown);
        assert!(matches!(specs_to_blocks(&parsed)[0].kind, BlockKind::Quote));
        assert_eq!(through_blocks(markdown), markdown);

        // One fenced spec, three source lines -> three single-line code cells
        // so the editor's single-line substrate and the contiguous renderer
        // see one row per line.
        let specs = parse("```rust\nlet a = 1;\nlet b = 2;\nlet c = 3;\n```\n");
        assert_eq!(specs.len(), 1);
        let blocks = specs_to_blocks(&specs);
        assert_eq!(blocks.len(), 3);
        for b in &blocks {
            assert!(matches!(b.kind, BlockKind::CodeBlock { .. }));
        }
        assert_eq!(blocks[0].text(), "let a = 1;");
        assert_eq!(blocks[1].text(), "let b = 2;");
        assert_eq!(blocks[2].text(), "let c = 3;");
        assert_eq!(
            blocks_to_specs(&specs_to_blocks(&parse("```sh\nfoo\nbar\nbaz\n```\n"))),
            vec![BlockSpec::CodeBlock {
                lang: Some("sh".into()),
                text: "foo\nbar\nbaz".into(),
            }],
            "consecutive code cells coalesce back into a single fenced block"
        );
        // Full loop through the block layer (the notes save/load path) must
        // reproduce the source markdown exactly.
        let md = "# Title\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n";
        assert_eq!(
            through_blocks(md),
            md,
            "multi-line code block must round-trip byte-stably"
        );

        let source = r"```tex
\frac{x^2+1}{\sqrt{y}}
```

```typst visual
sum_(i=1)^n frac(1, i^2)
```
";
        let specs = parse(source);
        assert_eq!(specs.len(), 2);
        assert!(matches!(
            &specs[0],
            BlockSpec::CodeBlock { lang: Some(lang), text }
                if lang == "tex" && text.contains("\\frac")
        ));
        assert!(matches!(
            &specs[1],
            BlockSpec::CodeBlock { lang: Some(lang), text }
                if lang == "typst visual" && text.contains("sum_")
        ));
        assert_eq!(serialize(&specs), source);
        let blocks = specs_to_blocks(&specs);
        let lang = |kind: &BlockKind| match kind {
            BlockKind::CodeBlock { lang } => lang.clone(),
            _ => None,
        };
        assert_eq!(
            ui::math::FormulaSyntax::from_fence_language(lang(&blocks[0].kind).as_deref()),
            Some(ui::math::FormulaSyntax::Tex)
        );
        assert_eq!(
            ui::math::FormulaDescriptor::from_fence_language(lang(&blocks[1].kind).as_deref())
                .map(|descriptor| descriptor.mode),
            Some(ui::math::FormulaMode::Visual)
        );
        assert_eq!(serialize(&blocks_to_specs(&blocks)), source);

        let blocks = specs_to_blocks(&parse("1. alpha\n\n2. beta\n"));
        assert!(matches!(blocks[0].kind, BlockKind::Numbered { indent: 0 }));
        assert!(matches!(blocks[1].kind, BlockKind::Numbered { indent: 0 }));
        assert_eq!(
            serialize(&blocks_to_specs(&blocks)),
            "1. alpha\n\n2. beta\n"
        );

        let md = "| A | B |\n| --- | --- |\n| 1 | 2 |\n";
        assert!(matches!(
            specs_to_blocks(&parse(md))[0].kind,
            BlockKind::Table { visual: true }
        ));
        assert_eq!(through_blocks(md), md);
    }

    fn export_import(blocks: &[ui::widget::Block]) -> Vec<ui::widget::Block> {
        specs_to_blocks(&parse(&serialize(&blocks_to_specs(blocks))))
    }

    fn shape(blocks: &[ui::widget::Block]) -> Vec<String> {
        blocks
            .iter()
            .map(|block| format!("{:?} {:?} {}", block.kind, block.text(), block.nesting))
            .collect()
    }

    /// Code that shows a fence (Markdown about Markdown) stays one code
    /// block through export and import: its own fence is longer.
    #[test]
    fn a_code_block_showing_a_fence_round_trips() {
        let specs = vec![
            BlockSpec::code_block(
                Some("md".into()),
                "```rust\nfn main() {}\n```\n\n````\n``` ",
            ),
            BlockSpec::paragraph("after"),
        ];
        let markdown = serialize(&specs);
        assert_eq!(parse(&markdown), specs, "markdown {markdown:?}");
        let blocks = specs_to_blocks(&specs);
        assert_eq!(shape(&export_import(&blocks)), shape(&blocks));
        // A longer fence written elsewhere reads as one block too, and a
        // fence closes on a line of at least its own backticks.
        assert_eq!(
            parse("````md\n```\nx\n```\n`````\n\nafter\n"),
            vec![
                BlockSpec::code_block(Some("md".into()), "```\nx\n```"),
                BlockSpec::paragraph("after"),
            ]
        );
        assert_eq!(
            serialize(&[BlockSpec::code_block(Some("sh".into()), "ls")]),
            "```sh\nls\n```\n"
        );
    }

    /// A fence the code shows indented, as under a list item, would close a
    /// three-backtick fence in any Markdown reader, so the exported fence
    /// is longer than it too.
    #[test]
    fn a_code_block_showing_an_indented_fence_exports_a_longer_fence() {
        let specs = vec![
            BlockSpec::code_block(Some("md".into()), "- step\n  ```sh\n  ls\n  ```\nmore"),
            BlockSpec::paragraph("after"),
        ];
        let markdown = serialize(&specs);
        assert!(
            markdown.starts_with("````md\n- step\n  ```sh\n  ls\n  ```\nmore\n````\n"),
            "markdown {markdown:?}"
        );
        assert_eq!(parse(&markdown), specs, "markdown {markdown:?}");
    }

    /// Text that reads like markup comes back as the same text, not marks.
    #[test]
    fn literal_markup_characters_round_trip_as_text() {
        use ui::widget::Block;
        let texts = [
            "*x*",
            "**x**",
            "a * b * c",
            "\\",
            "C:\\path\\",
            "ends with a backslash \\",
            "__x__",
            "~~x~~",
            "snake_case and a~b",
            "`code`",
            "[[Page]]",
            "[label](https://example.test)",
            "![[Page]]",
            "&nbsp;",
            "# not a heading",
            "> not a quote",
            "- not a bullet",
            "- [ ] not a task",
            "1. not a list",
            "2) not a list",
            "---",
            "```",
            "a | b\n--- | ---",
        ];
        for text in texts {
            let blocks = vec![
                Block::paragraph(text),
                Block::bullet(0, text),
                Block::heading(2, text),
            ];
            assert_eq!(
                shape(&export_import(&blocks)),
                shape(&blocks),
                "text {text:?}"
            );
        }
        assert_eq!(serialize(&[BlockSpec::paragraph("*x*")]), "\\*x\\*\n");
        assert_eq!(serialize(&[BlockSpec::paragraph("a\\b")]), "a\\\\b\n");
        // Markup that is meant as markup is left alone.
        assert_eq!(
            serialize(&parse("**bold** and `a*b`\n")),
            "**bold** and `a*b`\n"
        );
        // A backslash before a letter is not an escape.
        assert_eq!(parse_inline("a\\b").text, "a\\b");
    }

    /// A nested line keeps the space its text starts with, and an empty
    /// paragraph between others stays a paragraph.
    #[test]
    fn leading_space_and_empty_paragraphs_round_trip() {
        use ui::widget::Block;
        let mut nested = Block::paragraph(" indented by a space");
        nested.nesting = 1;
        let mut deeper = Block::paragraph("   three spaces");
        deeper.nesting = 2;
        let blocks = vec![
            Block::bullet(0, "item"),
            nested,
            deeper,
            Block::paragraph(""),
            Block::paragraph(" top level"),
            Block::paragraph("\tleading tab"),
            Block::paragraph("trailing spaces  "),
            Block::paragraph("\nstarts with a break"),
            Block::paragraph("ends with a break\n"),
            Block::paragraph(""),
            Block::paragraph("last"),
        ];
        assert_eq!(shape(&export_import(&blocks)), shape(&blocks));
        assert_eq!(serialize(&[BlockSpec::paragraph("")]), "\n");
        assert_eq!(
            serialize(&[
                BlockSpec::paragraph("a"),
                BlockSpec::paragraph(""),
                BlockSpec::paragraph("b")
            ]),
            "a\n\n&nbsp;\n\nb\n"
        );
    }

    /// A deterministic generator, so a failure names its seed.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            usize::try_from(self.next() % u64::try_from(n).unwrap_or(1)).unwrap_or(0)
        }

        fn text(&mut self) -> String {
            const CHARS: &[char] = &[
                'a', 'b', 'k', 'Z', '0', '7', ' ', ' ', ' ', '*', '_', '~', '`', '\\', '[', ']',
                '(', ')', '!', '#', '-', '>', '|', '.', ':', '&', ';', '\n', '\t', 'é', '—',
            ];
            let len = self.below(20);
            (0..len)
                .map(|_| CHARS.get(self.below(CHARS.len())).copied().unwrap_or('a'))
                .collect()
        }
    }

    /// Export then import is the identity on what a note holds: every
    /// block's kind, text and nesting, over notes-like content full of
    /// characters Markdown gives meaning to.
    #[test]
    fn export_then_import_keeps_every_block() {
        use ui::widget::Block;
        for seed in 1..=3000u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            let count = 1 + rng.below(10);
            let blocks: Vec<Block> = (0..count)
                .map(|_| {
                    let text = rng.text();
                    let indent = u8::try_from(rng.below(3)).unwrap_or(0);
                    match rng.below(8) {
                        0 => Block::heading(u8::try_from(1 + rng.below(3)).unwrap_or(1), &text),
                        1 => Block::bullet(indent, &text),
                        2 => Block::numbered(indent, &text),
                        3 => Block::checklist(indent, rng.below(2) == 1, &text),
                        4 => {
                            let mut quote = Block::quote(&text);
                            quote.nesting = indent.min(1);
                            quote
                        }
                        5 if text.is_empty() => Block::divider(),
                        _ => {
                            let mut paragraph = Block::paragraph(&text);
                            paragraph.nesting = indent;
                            paragraph
                        }
                    }
                })
                .collect();
            let markdown = serialize(&blocks_to_specs(&blocks));
            assert_eq!(
                shape(&export_import(&blocks)),
                shape(&blocks),
                "seed {seed}, markdown {markdown:?}"
            );
        }
    }
}
