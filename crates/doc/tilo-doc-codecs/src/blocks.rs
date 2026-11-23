//! A document's body as compact Markdown with stable block ids, and back.
//!
//! One line is one block, and each line ends in the id of its block:
//!
//! ```text
//! # Groceries ^3fa9
//! - [ ] oat milk ^c01e
//! - [x] **free-range** eggs ^77b2
//!   - [ ] a dozen ^9d40
//! > remember the bags ^e5a1
//! ```
//!
//! A reader or agent names a block by the id at the end of its line. Ids are
//! the shortest prefix, at least [`MIN_ID_CHARS`] characters, that no other
//! block's id starts with ([`short_ids`]); [`resolve`] takes it back to the
//! full id. A line without an id is a new block, so the same text is what
//! an agent writes to insert blocks or to replace a body.
//!
//! The dialect is small on purpose: headings (`#`..`######`), bullets,
//! numbered items, checklist items (`- [ ]`, `- [x]`), quotes (`>`),
//! callouts (`> [!WARNING] text`, a GitHub alert on one line), fenced
//! code, a `---` divider, and the inline marks `**bold**`, `*italic*` /
//! `_italic_`, `~~struck~~`, `` `code` `` and `[text](url)`, read and
//! written by the crate's one [`inline`](crate::inline) dialect. A line
//! break inside a block ends the line with a backslash. Blocks Markdown
//! cannot carry (a table, an image) read as `[kind] ^id` and are kept as
//! they are.
//!
//! It lives beside the document codec because both are Markdown: this is
//! the body's lines, with the ids a reader names a block by.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use tilo_doc::document::{MarkSpan, MarkValue};
use tilo_doc::snapshot::{AttrVal, BlockView};

use crate::inline;
use crate::markdown::fence_for;

pub use crate::markdown::CALLOUTS;
pub use crate::markdown::callout_named;
pub use crate::markdown::guard_paragraph;

/// The fewest characters of an id a reader shows.
pub const MIN_ID_CHARS: usize = 4;

/// The deepest list indent the editor draws, and the deepest this writes.
pub const MAX_INDENT: i64 = 5;

/// The deepest heading level.
pub const MAX_HEADING: i64 = 6;

/// How a block is stored, in the terms the editor draws: what a line of
/// Markdown says a block is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    Paragraph,
    Heading(i64),
    Bullet(i64),
    Numbered(i64),
    Checklist(i64, bool),
    Quote,
    /// A quote set apart as a GitHub alert: `note`, `tip`, `important`,
    /// `warning` or `caution` ([`CALLOUTS`]).
    Callout(&'static str),
    Code(Option<String>),
    Divider,
}

/// The stored attribute that makes a quote a callout.
const CALLOUT_ATTR: &str = "alert";

/// What to do with one stored attribute when a block takes a shape.
pub enum Want {
    Set(AttrVal),
    Clear,
    Keep,
}

/// The stored kind of a shape and what its attributes must be.
pub struct Stored {
    pub kind: &'static str,
    pub attrs: [(&'static str, Want); 7],
}

fn int_attr(view: &BlockView, key: &str) -> Option<i64> {
    match view.attrs.get(key) {
        Some(AttrVal::Int(value)) => Some(*value),
        _ => None,
    }
}

fn bool_attr(view: &BlockView, key: &str) -> Option<bool> {
    match view.attrs.get(key) {
        Some(AttrVal::Bool(value)) => Some(*value),
        _ => None,
    }
}

impl Shape {
    /// The shape a stored block has, or `None` for a block Markdown cannot
    /// carry: a table cell, a table kept as its source, an image.
    pub fn of_view(view: &BlockView) -> Option<Self> {
        if view.attrs.contains_key("table_source") {
            return None;
        }
        let indent = || int_attr(view, "level").unwrap_or(0).clamp(0, 9);
        match view.kind.as_str() {
            "heading" => Some(Self::Heading(
                int_attr(view, "level").unwrap_or(1).clamp(1, MAX_HEADING),
            )),
            "quote" | "blockquote" => Some(match view.attrs.get(CALLOUT_ATTR) {
                Some(AttrVal::Str(name)) => callout_named(name).map_or(Self::Quote, Self::Callout),
                _ => Self::Quote,
            }),
            "divider" => Some(Self::Divider),
            "list_item" => Some(match bool_attr(view, "checked") {
                Some(done) => Self::Checklist(indent(), done),
                None if bool_attr(view, "ordered") == Some(true) => Self::Numbered(indent()),
                None => Self::Bullet(indent()),
            }),
            "paragraph" if bool_attr(view, "code") == Some(true) => {
                Some(Self::Code(match view.attrs.get("lang") {
                    Some(AttrVal::Str(lang)) if !lang.is_empty() => Some(lang.clone()),
                    _ => None,
                }))
            }
            "paragraph" => Some(Self::Paragraph),
            _ => None,
        }
    }

    /// How this shape is stored.
    pub fn stored(&self) -> Stored {
        use AttrVal::{Bool, Int, Str};
        match self {
            Self::Paragraph | Self::Quote | Self::Callout(_) | Self::Divider => Stored {
                kind: match self {
                    Self::Quote | Self::Callout(_) => "quote",
                    Self::Divider => "divider",
                    _ => "paragraph",
                },
                attrs: [
                    ("level", Want::Clear),
                    ("ordered", Want::Clear),
                    ("checked", Want::Clear),
                    ("code", Want::Clear),
                    ("lang", Want::Clear),
                    ("code_group", Want::Clear),
                    (
                        CALLOUT_ATTR,
                        match self {
                            Self::Callout(name) => Want::Set(Str((*name).to_owned())),
                            _ => Want::Clear,
                        },
                    ),
                ],
            },
            Self::Heading(level) => Stored {
                kind: "heading",
                attrs: [
                    ("level", Want::Set(Int(*level))),
                    ("ordered", Want::Clear),
                    ("checked", Want::Clear),
                    ("code", Want::Clear),
                    ("lang", Want::Clear),
                    ("code_group", Want::Clear),
                    (CALLOUT_ATTR, Want::Clear),
                ],
            },
            Self::Bullet(indent) | Self::Numbered(indent) | Self::Checklist(indent, _) => Stored {
                kind: "list_item",
                attrs: [
                    ("level", Want::Set(Int(*indent))),
                    (
                        "ordered",
                        Want::Set(Bool(matches!(self, Self::Numbered(_)))),
                    ),
                    (
                        "checked",
                        match self {
                            Self::Checklist(_, done) => Want::Set(Bool(*done)),
                            _ => Want::Clear,
                        },
                    ),
                    ("code", Want::Clear),
                    ("lang", Want::Clear),
                    ("code_group", Want::Clear),
                    (CALLOUT_ATTR, Want::Clear),
                ],
            },
            Self::Code(lang) => Stored {
                kind: "paragraph",
                attrs: [
                    ("level", Want::Clear),
                    ("ordered", Want::Clear),
                    ("checked", Want::Clear),
                    ("code", Want::Set(Bool(true))),
                    (
                        "lang",
                        lang.as_ref()
                            .map_or(Want::Clear, |lang| Want::Set(Str(lang.clone()))),
                    ),
                    ("code_group", Want::Keep),
                    (CALLOUT_ATTR, Want::Clear),
                ],
            },
        }
    }

    /// Whether the block's text is code, read verbatim.
    pub fn is_code(&self) -> bool {
        matches!(self, Self::Code(_))
    }
}

// ---------------------------------------------------------------------------
// Ids
// ---------------------------------------------------------------------------

/// The ids `ids` are shown as: each the shortest prefix of at least
/// [`MIN_ID_CHARS`] characters that no other id in `ids` starts with.
pub fn short_ids<'a>(ids: impl IntoIterator<Item = &'a str>) -> BTreeMap<&'a str, String> {
    let mut sorted: Vec<&str> = ids.into_iter().collect();
    sorted.sort_unstable();
    sorted.dedup();
    let common = |a: &str, b: &str| {
        a.chars()
            .zip(b.chars())
            .take_while(|(left, right)| left == right)
            .count()
    };
    let mut shown = BTreeMap::new();
    for (index, id) in sorted.iter().enumerate() {
        let before = index
            .checked_sub(1)
            .and_then(|previous| sorted.get(previous))
            .map_or(0, |other| common(id, other));
        let after = sorted
            .get(index.saturating_add(1))
            .map_or(0, |other| common(id, other));
        let wanted = before.max(after).saturating_add(1).max(MIN_ID_CHARS);
        shown.insert(*id, id.chars().take(wanted).collect());
    }
    shown
}

/// The id `token` names among `ids`: an id itself, or the one id it is a
/// prefix of.
pub fn resolve<'a>(ids: impl IntoIterator<Item = &'a str>, token: &str) -> Result<String, String> {
    let mut found: Option<&str> = None;
    let mut ambiguous = false;
    for id in ids {
        if id == token {
            return Ok(id.to_owned());
        }
        if !token.is_empty() && id.starts_with(token) {
            ambiguous |= found.is_some();
            found = Some(id);
        }
    }
    match (found, ambiguous) {
        (Some(id), false) => Ok(id.to_owned()),
        (Some(_), true) => Err(format!(
            "block id `{token}` starts several blocks' ids; give more of it"
        )),
        (None, _) => Err(format!(
            "no block `{token}`; read the page for the ids it holds"
        )),
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// A block's text and marks as the one inline dialect writes them, a line break as the
/// backslash that reads back as the same break.
fn inline(text: &str, marks: &[MarkSpan]) -> String {
    crate::inline::render(text, marks, inline::Style::BLOCK)
}

/// One non-code block as a line, without its id.
pub fn render_line(shape: &Shape, text: &str, marks: &[MarkSpan]) -> String {
    let body = || inline(text, marks);
    let pad = |indent: &i64| "  ".repeat(usize::try_from(*indent).unwrap_or(0));
    match shape {
        Shape::Paragraph => guard_paragraph(&body()),
        Shape::Heading(level) => format!(
            "{} {}",
            "#".repeat(usize::try_from(*level).unwrap_or(1)),
            body()
        ),
        Shape::Bullet(indent) => format!("{}- {}", pad(indent), body()),
        Shape::Numbered(indent) => format!("{}1. {}", pad(indent), body()),
        Shape::Checklist(indent, done) => {
            format!(
                "{}- [{}] {}",
                pad(indent),
                if *done { 'x' } else { ' ' },
                body()
            )
        }
        Shape::Quote => format!("> {}", body()),
        Shape::Callout(name) => {
            format!("> [!{}] {}", name.to_ascii_uppercase(), body())
        }
        Shape::Divider => "---".to_owned(),
        Shape::Code(_) => text.to_owned(),
    }
}

/// The lines of a code block's text: a trailing newline ends its last line, and is not a line
/// of its own.
fn text_lines(text: &str) -> impl Iterator<Item = &str> {
    text.strip_suffix('\n').unwrap_or(text).split('\n')
}

/// What makes two blocks the same line, ids aside: its shape, its text and
/// the marks the dialect carries.
pub fn line_key(shape: &Shape, text: &str, marks: &[MarkSpan]) -> String {
    match shape {
        Shape::Code(lang) => format!(
            "```{}\n{}",
            lang.as_deref().unwrap_or(""),
            text.strip_suffix('\n').unwrap_or(text)
        ),
        _ => render_line(shape, text, marks),
    }
}

/// `blocks` as the lines `InsertMarkdown` writes back
/// as the same blocks, with their marks: what a caller that read Markdown its own way inserts so
/// the text is the invoker's. `None` when a block has no line of its own (a table, a picture) or
/// links somewhere a line cannot name (a page of an app, another resource).
#[must_use]
pub fn lines_of(blocks: &[BlockView]) -> Option<String> {
    blocks
        .iter()
        .all(|view| {
            Shape::of_view(view).is_some()
                && view.marks.iter().all(|mark| {
                    mark.name != "link"
                        || matches!(&mark.value, MarkValue::Str(target)
                            if inline::valid_hyperlink_target(target))
                })
        })
        .then(|| render(blocks, &BTreeMap::new(), false))
}

/// `blocks` as Markdown, one line per block. With `ids`, each line ends in
/// its block's id as `shown` gives it (`ids` empty omits them).
pub fn render(blocks: &[BlockView], shown: &BTreeMap<&str, String>, ids: bool) -> String {
    let id_of = |view: &BlockView| -> String {
        if !ids {
            return String::new();
        }
        let id = shown
            .get(view.id.as_str())
            .map_or(view.id.as_str(), String::as_str);
        format!(" ^{id}")
    };
    let mut out = String::new();
    let mut index = 0;
    while let Some(view) = blocks.get(index) {
        index = index.saturating_add(1);
        let Some(shape) = Shape::of_view(view) else {
            let kind = if view.attrs.contains_key("table_source") {
                "table"
            } else {
                view.kind.as_str()
            };
            let _ = writeln!(out, "[{kind}]{}", id_of(view));
            continue;
        };
        let Shape::Code(lang) = &shape else {
            out.push_str(&render_line(&shape, &view.text, &view.marks));
            out.push_str(&id_of(view));
            out.push('\n');
            continue;
        };
        // A run of code lines of one group is one fence.
        let group = view.attrs.get("code_group");
        let mut run = vec![view];
        while let Some(next) = blocks.get(index) {
            let same = group.is_some()
                && next.attrs.get("code_group") == group
                && matches!(Shape::of_view(next), Some(Shape::Code(_)));
            if !same {
                break;
            }
            run.push(next);
            index = index.saturating_add(1);
        }
        let lines: Vec<&str> = run
            .iter()
            .flat_map(|block| text_lines(&block.text))
            .collect();
        let fence = fence_for(&lines);
        out.push_str(&fence);
        out.push_str(lang.as_deref().unwrap_or(""));
        out.push('\n');
        for block in &run {
            let mut parts = text_lines(&block.text).peekable();
            while let Some(part) = parts.next() {
                out.push_str(part);
                if parts.peek().is_none() {
                    out.push_str(&id_of(block));
                }
                out.push('\n');
            }
        }
        out.push_str(&fence);
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// What a line's trailing `^id` means.
pub enum Ids<'a> {
    /// A `^id` names the block, when `known` says it is one.
    Known(&'a dyn Fn(&str) -> Option<String>),
    /// Ids are dropped: the lines are new blocks.
    Discard,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    /// A block of the page, left as it is (`[table] ^id`).
    Keep(String),
    Block(Item),
}

/// One block a line of Markdown says.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub shape: Shape,
    pub text: String,
    pub marks: Vec<MarkSpan>,
    /// The block this line names, a full id.
    pub id: Option<String>,
    /// The fenced code this line is part of, by order of appearance.
    pub fence: Option<usize>,
}

impl Item {
    pub fn key(&self) -> String {
        line_key(&self.shape, &self.text, &self.marks)
    }
}

fn id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-')
}

/// `line` without its trailing ` ^id`, and the id.
fn split_id(line: &str) -> (&str, Option<&str>) {
    let trimmed = line.trim_end();
    let Some(at) = trimmed.rfind('^') else {
        return (trimmed, None);
    };
    let (head, token) = trimmed.split_at(at);
    let token = token.trim_start_matches('^');
    let spaced = head.is_empty() || head.ends_with(char::is_whitespace);
    if spaced && !token.is_empty() && token.chars().all(id_char) {
        (head.trim_end(), Some(token))
    } else {
        (trimmed, None)
    }
}

/// The number of backslashes `line` ends in.
fn trailing_backslashes(line: &str) -> usize {
    line.chars().rev().take_while(|c| *c == '\\').count()
}

fn is_rule(rest: &str) -> bool {
    let mut chars = rest.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    matches!(first, '-' | '*' | '_')
        && rest.chars().count() >= 3
        && rest.chars().all(|c| c == first)
}

/// The marker a line starts with, its shape and where its text starts.
fn block_start(line: &str) -> (Shape, &str) {
    let spaces: usize = line
        .chars()
        .take_while(|c| matches!(c, ' ' | '\t'))
        .map(|c| if c == '\t' { 2 } else { 1 })
        .sum();
    let indent = i64::try_from(spaces / 2).unwrap_or(0).clamp(0, MAX_INDENT);
    let rest = line.trim_start();
    let after = |marker: usize| rest.get(marker..).unwrap_or_default();
    let hashes = rest.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && matches!(after(hashes).chars().next(), None | Some(' ')) {
        let level = i64::try_from(hashes).unwrap_or(1);
        return (Shape::Heading(level), after(hashes).trim_start());
    }
    if is_rule(rest) {
        return (Shape::Divider, "");
    }
    if let Some(quoted) = rest.strip_prefix('>') {
        let quoted = quoted.strip_prefix(' ').unwrap_or(quoted);
        if let Some((name, text)) = quoted
            .strip_prefix("[!")
            .and_then(|marker| marker.split_once(']'))
            && let Some(callout) = callout_named(name)
            && matches!(text.chars().next(), None | Some(' '))
        {
            return (Shape::Callout(callout), text.trim_start());
        }
        return (Shape::Quote, quoted);
    }
    if let Some(item) = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))
        .or_else(|| rest.strip_prefix("+ "))
    {
        for (box_text, done) in [("[ ]", false), ("[x]", true), ("[X]", true)] {
            if let Some(text) = item.strip_prefix(box_text)
                && matches!(text.chars().next(), None | Some(' '))
            {
                return (Shape::Checklist(indent, done), text.trim_start());
            }
        }
        return (Shape::Bullet(indent), item);
    }
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits > 0
        && matches!(after(digits).chars().next(), Some('.' | ')'))
        && matches!(
            after(digits.saturating_add(1)).chars().next(),
            None | Some(' ')
        )
    {
        return (
            Shape::Numbered(indent),
            after(digits.saturating_add(1)).trim_start(),
        );
    }
    (Shape::Paragraph, rest)
}

/// The code blocks the lines of one fence say: a line is a block, named by
/// the `^id` it ends in.
fn code_lines(body: &[&str], lang: &str, fence: usize, ids: &Ids<'_>) -> Vec<Parsed> {
    body.iter()
        .map(|line| {
            let (text, token) = split_id(line);
            let (text, id) = match ids {
                Ids::Known(known) => match token.and_then(known) {
                    Some(id) => (text, Some(id)),
                    None => (*line, None),
                },
                Ids::Discard => (text, None),
            };
            Parsed::Block(Item {
                shape: Shape::Code((!lang.is_empty()).then(|| lang.to_owned())),
                text: text.to_owned(),
                marks: Vec::new(),
                id,
                fence: Some(fence),
            })
        })
        .collect()
}

/// Parse `markdown` as the blocks it says.
pub fn parse(markdown: &str, ids: &Ids<'_>) -> Result<Vec<Parsed>, String> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out = Vec::new();
    let mut fences = 0_usize;
    let mut at = 0;
    while let Some(raw) = lines.get(at) {
        at = at.saturating_add(1);
        let trimmed = raw.trim_start();
        let ticks = trimmed.chars().take_while(|c| *c == '`').count();
        if ticks >= 3 {
            let lang = trimmed.get(ticks..).unwrap_or_default().trim();
            let fence = fences;
            fences = fences.saturating_add(1);
            let mut body: Vec<&str> = Vec::new();
            while let Some(line) = lines.get(at) {
                at = at.saturating_add(1);
                let line_ticks = line.trim().chars().take_while(|c| *c == '`').count();
                if line_ticks >= ticks && line.trim().chars().all(|c| c == '`') {
                    break;
                }
                body.push(line);
            }
            out.extend(code_lines(&body, lang, fence, ids));
            continue;
        }
        let mut joined = String::new();
        let mut line = *raw;
        loop {
            if trailing_backslashes(line) % 2 == 1
                && let Some(next) = lines.get(at)
            {
                at = at.saturating_add(1);
                joined.push_str(line.get(..line.len().saturating_sub(1)).unwrap_or_default());
                joined.push('\n');
                line = next;
                continue;
            }
            joined.push_str(line);
            break;
        }
        if joined.trim().is_empty() {
            continue;
        }
        let (content, token) = split_id(&joined);
        let id = match (ids, token) {
            (Ids::Known(known), Some(token)) => known(token),
            _ => None,
        };
        let content = if token.is_some() && (id.is_some() || matches!(ids, Ids::Discard)) {
            content
        } else {
            joined.trim_end()
        };
        if let Some(kind) = split_id(&joined)
            .0
            .trim()
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
            .filter(|kind| {
                !kind.is_empty() && kind.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            })
        {
            match id {
                Some(id) => {
                    out.push(Parsed::Keep(id));
                    continue;
                }
                None if matches!(ids, Ids::Known(_)) && token.is_some() => {
                    return Err(format!(
                        "`[{kind}]` names a block that is not on this page; leave it out or keep its id"
                    ));
                }
                None => {}
            }
        }
        if content.trim_start().starts_with('|') {
            return Err(
                "a table cannot be written as Markdown here; the page keeps the tables it has, as `[table] ^id` lines"
                    .to_owned(),
            );
        }
        let (shape, rest) = block_start(content);
        let (text, marks) = if matches!(shape, Shape::Divider) {
            (String::new(), Vec::new())
        } else {
            parse_inline(rest)
        };
        out.push(Parsed::Block(Item {
            shape,
            text,
            marks,
            id,
            fence: None,
        }));
    }
    Ok(out)
}

/// The inline marks of `source`, and its text without their delimiters.
pub fn parse_inline(source: &str) -> (String, Vec<MarkSpan>) {
    inline::parse(source, &inline::valid_hyperlink_target)
}

/// Whether `source` says anything in inline Markdown: a mark (**bold**,
/// _italic_, `code`, ~~strike~~, [link](url)) or a backslash escape. Text with
/// neither reads the same as plain text, so writing it plain keeps the marks
/// it is written into; text with either means what the Markdown says, as
/// the read that showed it to the writer does.
pub fn has_inline_markdown(source: &str) -> bool {
    let (text, marks) = parse_inline(source);
    !marks.is_empty() || text != source
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(id: &str, kind: &str, text: &str, attrs: &[(&str, AttrVal)]) -> BlockView {
        BlockView {
            id: id.into(),
            kind: kind.into(),
            text: text.into(),
            marks: Vec::new(),
            attrs: attrs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect(),
            children: Vec::new(),
        }
    }

    fn mark(start: usize, end: usize, name: &str, value: MarkValue) -> MarkSpan {
        MarkSpan {
            start,
            end,
            name: name.into(),
            value,
        }
    }

    fn known<'a>(ids: &'a [&'a str]) -> impl Fn(&str) -> Option<String> + 'a {
        move |token| resolve(ids.iter().copied(), token).ok()
    }

    #[test]
    fn ids_are_the_shortest_unique_prefix_and_resolve_back() {
        let ids = ["aaaa1111", "aaaa2222", "bbbbcccc", "cc"];
        let shown = short_ids(ids);
        assert_eq!(shown["aaaa1111"], "aaaa1");
        assert_eq!(shown["aaaa2222"], "aaaa2");
        assert_eq!(shown["bbbbcccc"], "bbbb");
        assert_eq!(shown["cc"], "cc");
        assert_eq!(resolve(ids, "aaaa1").unwrap(), "aaaa1111");
        assert_eq!(resolve(ids, "bbbbcccc").unwrap(), "bbbbcccc");
        assert!(resolve(ids, "aaaa").unwrap_err().contains("several"));
        assert!(resolve(ids, "zz").unwrap_err().contains("no block"));
    }

    fn blocks_read_one_line_each_with_their_ids() {
        let blocks = [
            view(
                "h1h1",
                "heading",
                "Groceries",
                &[("level", AttrVal::Int(1))],
            ),
            view(
                "c1c1",
                "list_item",
                "oat milk",
                &[
                    ("level", AttrVal::Int(0)),
                    ("ordered", AttrVal::Bool(false)),
                    ("checked", AttrVal::Bool(false)),
                ],
            ),
            view(
                "c2c2",
                "list_item",
                "eggs",
                &[
                    ("level", AttrVal::Int(1)),
                    ("ordered", AttrVal::Bool(false)),
                    ("checked", AttrVal::Bool(true)),
                ],
            ),
            view(
                "n1n1",
                "list_item",
                "first",
                &[("ordered", AttrVal::Bool(true))],
            ),
            view("q1q1", "quote", "bags", &[]),
            view("p1p1", "paragraph", "plain", &[]),
            view("d1d1", "divider", "", &[]),
            view(
                "t1t1",
                "paragraph",
                "| a |",
                &[("table_source", AttrVal::Bool(true))],
            ),
        ];
        let shown = short_ids(blocks.iter().map(|block| block.id.as_str()));
        assert_eq!(
            render(&blocks, &shown, true),
            "# Groceries ^h1h1\n- [ ] oat milk ^c1c1\n  - [x] eggs ^c2c2\n1. first ^n1n1\n> bags ^q1q1\nplain ^p1p1\n--- ^d1d1\n[table] ^t1t1\n"
        );
        assert!(!render(&blocks, &shown, false).contains('^'));
    }

    fn code_lines_of_one_group_are_one_fence_and_each_keeps_its_id() {
        let attrs = |group: &str| {
            [
                ("code", AttrVal::Bool(true)),
                ("lang", AttrVal::Str("rust".into())),
                ("code_group", AttrVal::Str(group.into())),
            ]
        };
        let blocks = [
            view("l1l1", "paragraph", "fn main() {", &attrs("g")),
            view("l2l2", "paragraph", "}", &attrs("g")),
            view("l3l3", "paragraph", "other", &attrs("h")),
        ];
        let shown = short_ids(blocks.iter().map(|block| block.id.as_str()));
        let markdown = render(&blocks, &shown, true);
        assert_eq!(
            markdown,
            "```rust\nfn main() { ^l1l1\n} ^l2l2\n```\n```rust\nother ^l3l3\n```\n"
        );
        let ids = ["l1l1", "l2l2", "l3l3"];
        let parsed = parse(&markdown, &Ids::Known(&known(&ids))).unwrap();
        let lines: Vec<_> = parsed
            .iter()
            .map(|line| match line {
                Parsed::Block(item) => (item.text.clone(), item.id.clone(), item.fence),
                Parsed::Keep(_) => panic!("a block"),
            })
            .collect();
        assert_eq!(
            lines,
            [
                ("fn main() {".into(), Some("l1l1".into()), Some(0)),
                ("}".into(), Some("l2l2".into()), Some(0)),
                ("other".into(), Some("l3l3".into()), Some(1)),
            ]
        );
    }

    fn what_is_rendered_parses_back_to_the_same_lines() {
        let mut bold = view("b1b1", "paragraph", "Deadline: *Friday* [soon]", &[]);
        bold.marks = vec![
            mark(0, 9, "strong", MarkValue::Bool(true)),
            mark(11, 17, "em", MarkValue::Bool(true)),
            mark(
                19,
                25,
                "link",
                MarkValue::Str("https://example.test/a b".into()),
            ),
        ];
        let mut code = view("b2b2", "list_item", "run cargo test now", &[]);
        code.marks = vec![
            mark(4, 14, "fontFamily", MarkValue::Str("mono".into())),
            mark(15, 18, "strike", MarkValue::Bool(true)),
        ];
        let tricky = [
            view("b3b3", "paragraph", "- not a list", &[]),
            view("b4b4", "paragraph", "2. not numbered # or > nor", &[]),
            view("b5b5", "paragraph", "snake_case and 2*3 and a\\b", &[]),
            view("b6b6", "paragraph", "two\nlines", &[]),
            view(
                "b7b7",
                "heading",
                "# not nested",
                &[("level", AttrVal::Int(2))],
            ),
            view(
                "b8b8",
                "quote",
                "heed this",
                &[("alert", AttrVal::Str("warning".into()))],
            ),
            view("b9b9", "quote", "[!NOTE] is only text here", &[]),
        ];
        let mut blocks = vec![bold, code];
        blocks.extend(tricky);
        let ids: Vec<&str> = blocks.iter().map(|block| block.id.as_str()).collect();
        let shown = short_ids(ids.iter().copied());
        let markdown = render(&blocks, &shown, true);
        let parsed = parse(&markdown, &Ids::Known(&known(&ids))).unwrap();
        assert_eq!(parsed.len(), blocks.len(), "{markdown}");
        for (line, block) in parsed.iter().zip(&blocks) {
            let Parsed::Block(item) = line else {
                panic!("a block: {line:?}")
            };
            assert_eq!(item.id.as_deref(), Some(block.id.as_str()), "{markdown}");
            let shape = Shape::of_view(block).unwrap();
            assert_eq!(
                item.key(),
                line_key(&shape, &block.text, &block.marks),
                "{markdown}"
            );
            assert_eq!(item.text, block.text, "{markdown}");
        }
    }

    fn a_callout_is_a_quote_with_its_alert() {
        let callout = view(
            "c1c1",
            "quote",
            "mind the gap",
            &[("alert", AttrVal::Str("caution".into()))],
        );
        let shape = Shape::of_view(&callout).unwrap();
        assert_eq!(shape, Shape::Callout("caution"));
        assert_eq!(
            render_line(&shape, &callout.text, &[]),
            "> [!CAUTION] mind the gap"
        );
        let parsed = parse("> [!tip] try this\n> [!NOPE] plain\n", &Ids::Discard).unwrap();
        let shapes: Vec<Shape> = parsed
            .into_iter()
            .map(|line| match line {
                Parsed::Block(item) => item.shape,
                Parsed::Keep(id) => panic!("kept {id}"),
            })
            .collect();
        assert_eq!(shapes, [Shape::Callout("tip"), Shape::Quote]);
        // Back to a plain quote clears the alert; every other shape too.
        for shape in [Shape::Quote, Shape::Paragraph, Shape::Bullet(0)] {
            assert!(
                shape
                    .stored()
                    .attrs
                    .iter()
                    .any(|(key, want)| *key == "alert" && matches!(want, Want::Clear))
            );
        }
    }

    #[test]
    fn blocks_render_one_line_each_and_parse_back() {
        let cases: [(&str, fn()); 4] = [
            (
                "blocks read one line each with their ids",
                blocks_read_one_line_each_with_their_ids,
            ),
            (
                "code lines of one group are one fence and each keeps its id",
                code_lines_of_one_group_are_one_fence_and_each_keeps_its_id,
            ),
            (
                "what is rendered parses back to the same lines",
                what_is_rendered_parses_back_to_the_same_lines,
            ),
            (
                "a callout is a quote with its alert",
                a_callout_is_a_quote_with_its_alert,
            ),
        ];
        for (case, run) in cases {
            eprintln!("case: {case}");
            run();
        }
    }

    fn written_markdown_is_blocks_with_marks() {
        let parsed = parse(
            "## Plan\n- [ ] buy **oat** milk\n  - [x] done _already_\n> a ~~quote~~ with `code`\nsee [docs](https://example.test)\n---\n",
            &Ids::Discard,
        )
        .unwrap();
        let items: Vec<Item> = parsed
            .into_iter()
            .map(|line| match line {
                Parsed::Block(item) => item,
                Parsed::Keep(_) => panic!("a block"),
            })
            .collect();
        let shapes: Vec<_> = items.iter().map(|item| item.shape.clone()).collect();
        assert_eq!(
            shapes,
            [
                Shape::Heading(2),
                Shape::Checklist(0, false),
                Shape::Checklist(1, true),
                Shape::Quote,
                Shape::Paragraph,
                Shape::Divider
            ]
        );
        assert_eq!(items[1].text, "buy oat milk");
        assert_eq!(
            items[1].marks,
            [mark(4, 7, "strong", MarkValue::Bool(true))]
        );
        assert_eq!(items[2].marks, [mark(5, 12, "em", MarkValue::Bool(true))]);
        assert_eq!(items[3].text, "a quote with code");
        assert_eq!(items[4].marks.len(), 1);
        assert_eq!(items[4].text, "see docs");
    }

    fn a_table_is_refused_and_an_unknown_kept_block_is_named() {
        assert!(
            parse("| a | b |\n| - | - |\n", &Ids::Discard)
                .unwrap_err()
                .contains("table")
        );
        let none: [&str; 0] = [];
        let error = parse("[table] ^gone\n", &Ids::Known(&known(&none))).unwrap_err();
        assert!(error.contains("not on this page"), "{error}");
        let ids = ["t1t1"];
        let kept = parse("[table] ^t1t1\n", &Ids::Known(&known(&ids))).unwrap();
        assert_eq!(kept, [Parsed::Keep("t1t1".into())]);
    }

    fn an_unknown_id_stays_the_text_it_is() {
        let none: [&str; 0] = [];
        let parsed = parse("see ^caret\n", &Ids::Known(&known(&none))).unwrap();
        let Parsed::Block(item) = &parsed[0] else {
            panic!("a block")
        };
        assert_eq!(item.text, "see ^caret");
        assert_eq!(item.id, None);
    }

    #[test]
    fn markdown_parses_to_blocks_keeps_named_tables_and_refuses_new_ones() {
        let cases: [(&str, fn()); 3] = [
            (
                "written markdown is blocks with marks",
                written_markdown_is_blocks_with_marks,
            ),
            (
                "a table is refused and an unknown kept block is named",
                a_table_is_refused_and_an_unknown_kept_block_is_named,
            ),
            (
                "an unknown id stays the text it is",
                an_unknown_id_stays_the_text_it_is,
            ),
        ];
        for (case, run) in cases {
            eprintln!("case: {case}");
            run();
        }
    }
}
