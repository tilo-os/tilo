//! Pure functions to translate between a page's markdown body on
//! disk and the in-memory block list the [`ui::widget::BlockEditor`]
//! renders.
//!
//! Phase 3 extends this with four more block kinds: bullet lists,
//! checklists, fenced code blocks, and dividers. Phase 4 adds inline
//! marks (`**bold**`, `*italic*`, `` `code` ``, `[[Page Title]]`) and
//! propagates a per-block [`MarkSpan`] list alongside the plain-text
//! body. The grammar is:
//!
//! - A line beginning with `# `, `## `, `### ` is a heading.
//! - A line beginning with `- ` / `* ` (after optional 2-space
//!   indent groups, max 5 levels) is a bullet list item.
//! - A line beginning with `- [ ] ` / `- [x] ` (similarly indentable)
//!   is a checklist item.
//! - A line equal to `---` is a divider.
//! - A line beginning with ` ``` ` opens a fenced code block; the
//!   matching closing fence terminates it. **v0.1 limitation**: code
//!   block body is single-line per block.
//! - Inline marks (Phase 4):
//!   - `**text**` → bold
//!   - `*text*`  → italic (but: `**` never parses as italic-double-
//!     star; the bold tokenizer runs first)
//!   - `` `text` `` → inline code
//!   - `[[Page Title]]` → page link (target id = `Uuid::nil()`; the
//!     notes app resolves the title to an id at parse time)
//! - Any other non-empty line is a paragraph.
//! - Blank lines are block separators (consecutive blanks collapse).
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
    Paragraph(InlineText),
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
    Divider,
}

/// Same constant as [`ui::widget::block_editor::MAX_LIST_INDENT`].
pub const MAX_LIST_INDENT: u8 = 5;

impl BlockSpec {
    pub fn paragraph(s: impl Into<String>) -> Self {
        BlockSpec::Paragraph(InlineText::plain(s))
    }
    pub fn paragraph_marks(s: impl Into<String>, marks: Vec<MarkSpan>) -> Self {
        BlockSpec::Paragraph(InlineText::new(s, marks))
    }
    pub fn heading(level: u8, s: impl Into<String>) -> Self {
        BlockSpec::Heading {
            level: level.clamp(1, 3),
            text: InlineText::plain(s),
        }
    }
    pub fn heading_marks(level: u8, s: impl Into<String>, marks: Vec<MarkSpan>) -> Self {
        BlockSpec::Heading {
            level: level.clamp(1, 3),
            text: InlineText::new(s, marks),
        }
    }
    pub fn bullet(indent: u8, s: impl Into<String>) -> Self {
        BlockSpec::Bullet {
            indent: indent.min(MAX_LIST_INDENT),
            text: InlineText::plain(s),
        }
    }
    pub fn bullet_marks(indent: u8, s: impl Into<String>, marks: Vec<MarkSpan>) -> Self {
        BlockSpec::Bullet {
            indent: indent.min(MAX_LIST_INDENT),
            text: InlineText::new(s, marks),
        }
    }
    pub fn numbered(indent: u8, s: impl Into<String>) -> Self {
        BlockSpec::Numbered {
            indent: indent.min(MAX_LIST_INDENT),
            text: InlineText::plain(s),
        }
    }
    pub fn numbered_marks(indent: u8, s: impl Into<String>, marks: Vec<MarkSpan>) -> Self {
        BlockSpec::Numbered {
            indent: indent.min(MAX_LIST_INDENT),
            text: InlineText::new(s, marks),
        }
    }
    pub fn checklist(indent: u8, done: bool, s: impl Into<String>) -> Self {
        BlockSpec::Checklist {
            indent: indent.min(MAX_LIST_INDENT),
            done,
            text: InlineText::plain(s),
        }
    }
    pub fn checklist_marks(
        indent: u8,
        done: bool,
        s: impl Into<String>,
        marks: Vec<MarkSpan>,
    ) -> Self {
        BlockSpec::Checklist {
            indent: indent.min(MAX_LIST_INDENT),
            done,
            text: InlineText::new(s, marks),
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
/// 2. Page link (`[[Page Title]]`).
/// 3. Bold (`**...**`).
/// 4. Italic (`*...*`).
///
/// Unmatched markers fall through to the plain text — `*not closed`
/// stays `*not closed` so the round-trip stays byte-stable.
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
            // Shift inner marks by `start`.
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
        plain.push(c);
        i += c.len_utf8();
    }
    marks.sort_by_key(|m| m.range.start);
    InlineText { text: plain, marks }
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

/// Serialise an [`InlineText`] back to a markdown string, weaving
/// the inline markers back over the plain bytes. Output is
/// byte-stable for any text+marks that came out of [`parse_inline`].
///
/// Algorithm: walk `text` byte-by-byte; at each byte, open any mark
/// span whose `range.start == i`, append the byte, then close any
/// span whose `range.end == i+1`. Marks are emitted in this order
/// (most-nested last on open, first on close):
///
/// 1. Bold (outermost)
/// 2. Italic
/// 3. Code / Link (innermost — exclusive with one another)
fn open_marker(mark: &Mark) -> &'static str {
    match mark {
        Mark::Bold => "**",
        Mark::Italic => "*",
        Mark::Code => "`",
        Mark::Link(_) => "[[",
        Mark::Underline => "__",
        Mark::Strikethrough => "~~",
    }
}
fn close_marker(mark: &Mark) -> &'static str {
    match mark {
        Mark::Bold => "**",
        Mark::Italic => "*",
        Mark::Code => "`",
        Mark::Link(_) => "]]",
        Mark::Underline => "__",
        Mark::Strikethrough => "~~",
    }
}
// Sort marks for deterministic emission: by start ascending, then
// by priority (Bold < Italic < Underline < Strikethrough < Code < Link).
fn priority(m: &Mark) -> u8 {
    match m {
        Mark::Bold => 0,
        Mark::Italic => 1,
        Mark::Underline => 2,
        Mark::Strikethrough => 3,
        Mark::Code => 4,
        Mark::Link(_) => 5,
    }
}

pub fn serialize_inline(text: &InlineText) -> String {
    let mut out = String::with_capacity(text.text.len() + text.marks.len() * 4);
    // For each byte position we maintain a stack of currently-open
    // marks (in order of opening). We open marks at their `start`
    // byte (in priority order Bold → Italic → Code/Link) and close
    // them at their `end` byte in reverse order.
    let mut marks = text.marks.clone();
    marks.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
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
                out.push_str(close_marker(&top.mark));
                open.pop();
            } else {
                break;
            }
        }
        // Open any marks whose range.start == i.
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
            out.push(c);
            i += c.len_utf8();
        } else {
            i += 1;
        }
    }
    out
}

/// Parse a markdown string into a flat list of [`BlockSpec`]s.
///
/// Empty input → a single empty paragraph.
pub fn parse(md: &str) -> Vec<BlockSpec> {
    let mut blocks = Vec::new();
    let mut lines = md.split('\n').peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("```") {
            let lang = if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            };
            let mut body: Vec<&str> = Vec::new();
            while let Some(next) = lines.next() {
                if next == "```" {
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

/// Inverse of [`parse`]. Always emits a trailing newline.
pub fn serialize(blocks: &[BlockSpec]) -> String {
    let mut out = String::new();
    // Ordinal counters per indent level for numbered lists, mirroring the
    // renderer's `numbered_ordinals`: a numbered item increments its
    // level and resets deeper ones; any other block resets all counters.
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
            counters = [0usize; MAX_LIST_INDENT as usize + 1];
        }
        match b {
            BlockSpec::Paragraph(t) => out.push_str(&serialize_inline(t)),
            BlockSpec::Heading { level, text } => {
                let prefix = match *level {
                    1 => "# ",
                    2 => "## ",
                    _ => "### ",
                };
                out.push_str(prefix);
                out.push_str(&serialize_inline(text));
            }
            BlockSpec::Bullet { indent, text } => {
                push_indent(&mut out, *indent);
                out.push_str("- ");
                out.push_str(&serialize_inline(text));
            }
            BlockSpec::Numbered { indent, text } => {
                push_indent(&mut out, *indent);
                let n = counters
                    .get((*indent as usize).min(MAX_LIST_INDENT as usize))
                    .copied()
                    .unwrap_or(0);
                let _ = write!(out, "{n}. ");
                out.push_str(&serialize_inline(text));
            }
            BlockSpec::Checklist { indent, done, text } => {
                push_indent(&mut out, *indent);
                out.push_str(if *done { "- [x] " } else { "- [ ] " });
                out.push_str(&serialize_inline(text));
            }
            BlockSpec::CodeBlock { lang, text } => {
                out.push_str("```");
                if let Some(l) = lang {
                    out.push_str(l);
                }
                out.push('\n');
                out.push_str(text);
                out.push_str("\n```");
            }
            BlockSpec::Divider => {
                out.push_str("---");
            }
        }
    }
    out.push('\n');
    out
}

/// Convert a list of [`BlockSpec`]s into a list of
/// [`ui::widget::Block`]s suitable for handing to
/// [`ui::widget::BlockEditor::set_blocks`]. Mark spans on each block
/// land on the inner `RichTextLine`'s mark list.
pub fn specs_to_blocks(specs: &[BlockSpec]) -> Vec<ui::widget::Block> {
    use ui::widget::Block;
    let mut out = Vec::with_capacity(specs.len());
    for s in specs {
        match s {
            BlockSpec::Paragraph(t) => {
                let mut b = Block::paragraph(&t.text);
                b.line.set_marks(t.marks.clone());
                out.push(b);
            }
            BlockSpec::Heading { level, text } => {
                let mut b = Block::heading(*level, &text.text);
                b.line.set_marks(text.marks.clone());
                out.push(b);
            }
            BlockSpec::Bullet { indent, text } => {
                let mut b = Block::bullet(*indent, &text.text);
                b.line.set_marks(text.marks.clone());
                out.push(b);
            }
            BlockSpec::Numbered { indent, text } => {
                let mut b = Block::numbered(*indent, &text.text);
                b.line.set_marks(text.marks.clone());
                out.push(b);
            }
            BlockSpec::Checklist { indent, done, text } => {
                let mut b = Block::checklist(*indent, *done, &text.text);
                b.line.set_marks(text.marks.clone());
                out.push(b);
            }
            BlockSpec::CodeBlock { lang, text } => {
                // A multi-line fenced block maps to ONE code-block cell per
                // line — the editor's `RichTextLine` substrate is single-line,
                // and consecutive code cells render as one contiguous block.
                // An empty fence still yields a single empty cell so the user
                // has a place to type.
                if text.is_empty() {
                    out.push(Block::code_block(lang.clone(), ""));
                } else {
                    for line in text.split('\n') {
                        out.push(Block::code_block(lang.clone(), line));
                    }
                }
            }
            BlockSpec::Divider => out.push(Block::divider()),
        }
    }
    out
}

/// Inverse of [`specs_to_blocks`] — convert the editor's block list
/// back into the spec representation so [`serialize`] can emit
/// markdown that round-trips through the inline-marks layer.
pub fn blocks_to_specs(blocks: &[ui::widget::Block]) -> Vec<BlockSpec> {
    use ui::widget::BlockKind;
    let mut out = Vec::with_capacity(blocks.len());
    let mut i = 0usize;
    while let Some(b) = blocks.get(i) {
        match &b.kind {
            BlockKind::Paragraph => {
                out.push(BlockSpec::Paragraph(InlineText::new(
                    b.text(),
                    b.line.marks().to_vec(),
                )));
                i += 1;
            }
            BlockKind::Heading(level) => {
                out.push(BlockSpec::Heading {
                    level: *level,
                    text: InlineText::new(b.text(), b.line.marks().to_vec()),
                });
                i += 1;
            }
            BlockKind::Bullet { indent } => {
                out.push(BlockSpec::Bullet {
                    indent: *indent,
                    text: InlineText::new(b.text(), b.line.marks().to_vec()),
                });
                i += 1;
            }
            BlockKind::Numbered { indent } => {
                out.push(BlockSpec::Numbered {
                    indent: *indent,
                    text: InlineText::new(b.text(), b.line.marks().to_vec()),
                });
                i += 1;
            }
            BlockKind::Checklist { indent, done } => {
                out.push(BlockSpec::Checklist {
                    indent: *indent,
                    done: *done,
                    text: InlineText::new(b.text(), b.line.marks().to_vec()),
                });
                i += 1;
            }
            BlockKind::CodeBlock { lang } => {
                // Coalesce a run of consecutive code cells (with the same
                // language tag) back into ONE fenced block, inverting the
                // per-line split `specs_to_blocks` does. Distinct adjacent
                // fences (different lang) stay separate.
                let mut lines: Vec<String> = vec![b.text().to_string()];
                let run_lang = lang.clone();
                i += 1;
                while let Some(next) = blocks.get(i) {
                    if let BlockKind::CodeBlock { lang: next_lang } = &next.kind
                        && *next_lang == run_lang
                    {
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
            }
            BlockKind::Divider => {
                out.push(BlockSpec::Divider);
                i += 1;
            }
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
        let (BlockSpec::Paragraph(inline)
        | BlockSpec::Heading { text: inline, .. }
        | BlockSpec::Bullet { text: inline, .. }
        | BlockSpec::Numbered { text: inline, .. }
        | BlockSpec::Checklist { text: inline, .. }) = b
        else {
            continue;
        };
        for m in &mut inline.marks {
            if let Mark::Link(id) = &m.mark
                && id.is_nil()
                && let Some(title) = inline.text.get(m.range.clone())
                && let Some(new_id) = resolver(title)
            {
                m.mark = Mark::Link(new_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn parse_empty_yields_single_empty_paragraph() {
        let blocks = parse("");
        assert_eq!(blocks, vec![BlockSpec::paragraph("")]);
    }

    #[test]
    fn parse_heading_levels() {
        let blocks = parse("# H1\n\n## H2\n\n### H3\n");
        match &blocks[0] {
            BlockSpec::Heading { level, text } => {
                assert_eq!(*level, 1);
                assert_eq!(text.text, "H1");
            }
            _ => panic!("expected heading"),
        }
        assert_eq!(blocks.len(), 3);
    }

    #[test]
    fn parse_paragraph_with_punctuation() {
        let blocks = parse("Hello, world.\n");
        assert_eq!(blocks, vec![BlockSpec::paragraph("Hello, world.")]);
    }

    #[test]
    fn round_trip_typical_document() {
        let md = "# Title\n\nFirst paragraph.\n\nSecond paragraph.\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn round_trip_paragraphs_only() {
        let md = "alpha\n\nbeta\n\ngamma\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn lone_hash_is_paragraph_not_heading() {
        let blocks = parse("#\n");
        assert_eq!(blocks, vec![BlockSpec::paragraph("#")]);
    }

    #[test]
    fn parse_inline_bold() {
        let inline = parse_inline("**hello**");
        assert_eq!(inline.text, "hello");
        assert_eq!(inline.marks.len(), 1);
        assert_eq!(inline.marks[0].range, 0..5);
        assert_eq!(inline.marks[0].mark, Mark::Bold);
    }

    #[test]
    fn parse_inline_italic() {
        let inline = parse_inline("*hi*");
        assert_eq!(inline.text, "hi");
        assert_eq!(inline.marks.len(), 1);
        assert_eq!(inline.marks[0].mark, Mark::Italic);
    }

    #[test]
    fn parse_inline_code() {
        let inline = parse_inline("`foo`");
        assert_eq!(inline.text, "foo");
        assert_eq!(inline.marks[0].mark, Mark::Code);
    }

    #[test]
    fn parse_inline_link() {
        let inline = parse_inline("hello [[Welcome]] there");
        assert_eq!(inline.text, "hello Welcome there");
        let m = &inline.marks[0];
        assert!(matches!(m.mark, Mark::Link(_)));
        assert_eq!(inline.text.get(m.range.clone()), Some("Welcome"));
    }

    #[test]
    fn parse_inline_unmatched_star_falls_through() {
        let inline = parse_inline("*hello world");
        // Lone `*` with no partner stays as plain text.
        assert_eq!(inline.text, "*hello world");
        assert!(inline.marks.is_empty());
    }

    #[test]
    fn parse_inline_bold_inside_paragraph() {
        let inline = parse_inline("hello **world** bye");
        assert_eq!(inline.text, "hello world bye");
        assert_eq!(inline.marks.len(), 1);
        assert_eq!(inline.marks[0].range, 6..11);
        assert_eq!(inline.marks[0].mark, Mark::Bold);
    }

    #[test]
    fn serialize_inline_round_trip() {
        let cases = [
            "plain text",
            "**bold**",
            "*italic*",
            "`code`",
            "hello **world** bye",
            "hello [[Page]] there",
            "**bold *italic***",
            "a `b` c **d** e",
        ];
        for md in cases {
            let inline = parse_inline(md);
            let re = serialize_inline(&inline);
            assert_eq!(re, md, "round-trip failed for {md:?}");
        }
    }

    #[test]
    fn round_trip_paragraph_with_bold() {
        let md = "Hello **world** today.\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn round_trip_heading_with_italic() {
        let md = "# A *taste* of\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn round_trip_bullet_with_code() {
        let md = "- run `cargo build` first\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn round_trip_paragraph_with_link() {
        let md = "See [[Welcome]] for details.\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn round_trip_nested_bold_italic() {
        let md = "**bold *italic***\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn round_trip_mixed_corpus() {
        let md = "\
# Welcome **dear** friend

This paragraph has *italic* and **bold** and `code`.

- bullet with [[Welcome]] link

- [ ] todo with **bold**

Last paragraph.
";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn resolve_link_ids_replaces_placeholders() {
        let mut blocks = parse("See [[Welcome]] please.\n");
        let target = Uuid::from_u128(42);
        resolve_link_ids(&mut blocks, |title| {
            if title == "Welcome" {
                Some(target)
            } else {
                None
            }
        });
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
    }

    #[test]
    fn multiline_code_fence_splits_into_one_cell_per_line() {
        use ui::widget::BlockKind;
        let specs = parse("```rust\nlet a = 1;\nlet b = 2;\nlet c = 3;\n```\n");
        // One spec, three source lines.
        assert_eq!(specs.len(), 1);
        let blocks = specs_to_blocks(&specs);
        // Three single-line code cells so the editor's single-line substrate
        // and the contiguous renderer see one row per line.
        assert_eq!(blocks.len(), 3);
        for b in &blocks {
            assert!(matches!(b.kind, BlockKind::CodeBlock { .. }));
        }
        assert_eq!(blocks[0].text(), "let a = 1;");
        assert_eq!(blocks[1].text(), "let b = 2;");
        assert_eq!(blocks[2].text(), "let c = 3;");
    }

    #[test]
    fn code_cells_coalesce_back_into_one_fence() {
        let specs = parse("```sh\nfoo\nbar\nbaz\n```\n");
        let blocks = specs_to_blocks(&specs);
        let round = blocks_to_specs(&blocks);
        assert_eq!(
            round,
            vec![BlockSpec::CodeBlock {
                lang: Some("sh".into()),
                text: "foo\nbar\nbaz".into(),
            }],
            "consecutive code cells coalesce back into a single fenced block"
        );
    }

    #[test]
    fn multiline_code_block_round_trips_byte_stably() {
        // Full loop through the block layer (the notes save/load path) must
        // reproduce the source markdown exactly.
        let md = "# Title\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n";
        let blocks = specs_to_blocks(&parse(md));
        let re = serialize(&blocks_to_specs(&blocks));
        assert_eq!(re, md, "multi-line code block must round-trip byte-stably");
    }

    #[test]
    fn tex_and_typst_formula_fences_round_trip_as_editable_source() {
        let source = r#"```tex
\frac{x^2+1}{\sqrt{y}}
```

```typst visual
sum_(i=1)^n frac(1, i^2)
```
"#;
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
        assert_eq!(
            ui::math::FormulaSyntax::from_fence_language(match &blocks[0].kind {
                ui::widget::BlockKind::CodeBlock { lang } => lang.as_deref(),
                _ => None,
            }),
            Some(ui::math::FormulaSyntax::Tex)
        );
        assert_eq!(
            ui::math::FormulaDescriptor::from_fence_language(match &blocks[1].kind {
                ui::widget::BlockKind::CodeBlock { lang } => lang.as_deref(),
                _ => None,
            })
            .map(|descriptor| descriptor.mode),
            Some(ui::math::FormulaMode::Visual)
        );
        assert_eq!(serialize(&blocks_to_specs(&blocks)), source);
    }

    #[test]
    fn parse_numbered_list_items() {
        let blocks = parse("1. first\n\n2. second\n\n3. third\n");
        assert_eq!(
            blocks,
            vec![
                BlockSpec::numbered(0, "first"),
                BlockSpec::numbered(0, "second"),
                BlockSpec::numbered(0, "third"),
            ]
        );
    }

    #[test]
    fn parse_numbered_accepts_paren_and_arbitrary_start() {
        // `7) ` is a valid ordered marker; the stored spec drops the
        // number (it's recomputed), so it round-trips to `1. `.
        let blocks = parse("7) lucky\n");
        assert_eq!(blocks, vec![BlockSpec::numbered(0, "lucky")]);
    }

    #[test]
    fn round_trip_numbered_list() {
        let md = "1. first\n\n2. second\n\n3. third\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn serialize_renumbers_sequentially() {
        // Source that numbers everything `1.` is normalised to 1, 2, 3 on
        // save — ordinals track list position, not the literal digits.
        let blocks = parse("1. a\n\n1. b\n\n1. c\n");
        assert_eq!(serialize(&blocks), "1. a\n\n2. b\n\n3. c\n");
    }

    #[test]
    fn round_trip_nested_numbered_list() {
        // A nested level restarts at 1; returning to the outer level
        // continues its own count.
        let md = "1. one\n\n  1. inner a\n\n  2. inner b\n\n2. two\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn numbered_run_resets_after_paragraph() {
        // A paragraph between two numbered lists breaks the run: the
        // second list restarts at 1.
        let md = "1. a\n\n2. b\n\nbreak\n\n1. c\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn round_trip_numbered_with_inline_marks() {
        let md = "1. run `cargo build` and **ship**\n";
        assert_eq!(serialize(&parse(md)), md);
    }

    #[test]
    fn numbered_block_survives_editor_block_round_trip() {
        use ui::widget::BlockKind;
        let specs = parse("1. alpha\n\n2. beta\n");
        let blocks = specs_to_blocks(&specs);
        assert!(matches!(blocks[0].kind, BlockKind::Numbered { indent: 0 }));
        assert!(matches!(blocks[1].kind, BlockKind::Numbered { indent: 0 }));
        // Round-trips back through the block layer byte-stably.
        let re = serialize(&blocks_to_specs(&blocks));
        assert_eq!(re, "1. alpha\n\n2. beta\n");
    }

    #[test]
    fn serialize_clamps_heading_level() {
        let blocks = vec![BlockSpec::Heading {
            level: 99,
            text: InlineText::plain("x"),
        }];
        assert_eq!(serialize(&blocks), "### x\n");
    }
}
