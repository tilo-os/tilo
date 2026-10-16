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
//! `_italic_`, `~~struck~~`, `` `code` `` and `[text](url)`. A line break
//! inside a block ends the line with a backslash. Blocks Markdown cannot
//! carry (a table, an image) read as `[kind] ^id` and are kept as they are.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use tilo_doc::document::{MarkSpan, MarkValue};
use tilo_doc::snapshot::{AttrVal, BlockView};

/// The fewest characters of an id a reader shows.
pub const MIN_ID_CHARS: usize = 4;

/// The deepest list indent the editor draws, and the deepest this writes.
pub const MAX_INDENT: i64 = 5;

/// The deepest heading level.
pub const MAX_HEADING: i64 = 6;

/// How a block is stored, in the terms the editor draws: what a line of
/// Markdown says a block is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
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

/// The callouts a quote can be, by the name its `alert` attribute stores:
/// GitHub-flavoured Markdown's alerts.
pub const CALLOUTS: [&str; 5] = ["note", "tip", "important", "warning", "caution"];

/// The stored attribute that makes a quote a callout.
const CALLOUT_ATTR: &str = "alert";

/// The callout named `name`, in any case.
pub(crate) fn callout_named(name: &str) -> Option<&'static str> {
    // Lower-cased once, then compared exactly: a case-insensitive comparison
    // per callout is unrolled into a loop for each name.
    let name = name.trim().to_ascii_lowercase();
    CALLOUTS.into_iter().find(|callout| *callout == name)
}

/// What to do with one stored attribute when a block takes a shape.
pub(crate) enum Want {
    Set(AttrVal),
    Clear,
    Keep,
}

/// The stored kind of a shape and what its attributes must be.
pub(crate) struct Stored {
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
    pub(crate) fn of_view(view: &BlockView) -> Option<Self> {
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
    pub(crate) fn stored(&self) -> Stored {
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
    pub(crate) fn is_code(&self) -> bool {
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
///
/// # Errors
/// Names why `token` matches no block, or several.
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

/// A mark the Markdown dialect can carry.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tag {
    Link(String),
    Strong,
    Em,
    Strike,
    Code,
}

impl Tag {
    fn of(mark: &MarkSpan) -> Option<Self> {
        match (mark.name.as_str(), &mark.value) {
            (_, MarkValue::Bool(false)) => None,
            ("strong", _) => Some(Self::Strong),
            ("em", _) => Some(Self::Em),
            ("strike", _) => Some(Self::Strike),
            ("fontFamily", MarkValue::Str(family)) if family == "mono" => Some(Self::Code),
            ("link", MarkValue::Str(url)) if !url.is_empty() => Some(Self::Link(url.clone())),
            _ => None,
        }
    }

    /// Where the tag sits in a nest: outermost first.
    fn rank(&self) -> u8 {
        match self {
            Self::Link(_) => 0,
            Self::Strong => 1,
            Self::Em => 2,
            Self::Strike => 3,
            Self::Code => 4,
        }
    }
}

/// The marks of `marks` the dialect carries, as `(start, end, tag)` over
/// the `len` characters of the text.
fn tags(marks: &[MarkSpan], len: usize) -> Vec<(usize, usize, Tag)> {
    marks
        .iter()
        .filter_map(|mark| {
            let end = mark.end.min(len);
            (mark.start < end)
                .then(|| Tag::of(mark).map(|tag| (mark.start, end, tag)))
                .flatten()
        })
        .collect()
}

fn is_alphanumeric(c: Option<&char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric())
}

/// `text` with its marks as Markdown.
fn inline(text: &str, marks: &[MarkSpan]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let spans = tags(marks, chars.len());
    let mut cuts: Vec<usize> = vec![0, chars.len()];
    for (start, end, _) in &spans {
        cuts.push(*start);
        cuts.push(*end);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = String::new();
    let mut open: Vec<Tag> = Vec::new();
    for pair in cuts.windows(2) {
        let (Some(&from), Some(&to)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let mut wanted: Vec<Tag> = spans
            .iter()
            .filter(|(start, end, _)| *start <= from && to <= *end)
            .map(|(_, _, tag)| tag.clone())
            .collect();
        wanted.sort_by_key(Tag::rank);
        wanted.dedup_by_key(|tag| tag.rank());
        let keep = open
            .iter()
            .zip(&wanted)
            .take_while(|(have, want)| have == want)
            .count();
        while open.len() > keep {
            if let Some(tag) = open.pop() {
                close_tag(&mut out, &tag);
            }
        }
        for tag in wanted.iter().skip(keep) {
            open_tag(&mut out, tag, chars.get(from.wrapping_sub(1)), from > 0);
            open.push(tag.clone());
        }
        let piece: Vec<char> = chars.get(from..to).unwrap_or_default().to_vec();
        if open.contains(&Tag::Code) {
            let raw: String = piece.iter().collect();
            out.push_str(&raw);
        } else {
            escape_into(&mut out, &piece);
        }
    }
    while let Some(tag) = open.pop() {
        close_tag(&mut out, &tag);
    }
    out
}

fn open_tag(out: &mut String, tag: &Tag, before: Option<&char>, has_before: bool) {
    match tag {
        Tag::Link(_) => out.push('['),
        Tag::Strong => out.push_str("**"),
        // `_` cannot open inside a word, `*` can.
        Tag::Em => out.push(if has_before && is_alphanumeric(before) {
            '*'
        } else {
            '_'
        }),
        Tag::Strike => out.push_str("~~"),
        Tag::Code => out.push('`'),
    }
}

fn close_tag(out: &mut String, tag: &Tag) {
    match tag {
        Tag::Link(url) => {
            out.push_str("](");
            for c in url.chars() {
                match c {
                    ' ' => out.push_str("%20"),
                    ')' => out.push_str("%29"),
                    '(' => out.push_str("%28"),
                    _ => out.push(c),
                }
            }
            out.push(')');
        }
        Tag::Strong => out.push_str("**"),
        Tag::Em => out.push('_'),
        Tag::Strike => out.push_str("~~"),
        Tag::Code => out.push('`'),
    }
}

/// The characters that mean something inline, escaped; a line break is a
/// backslash at the end of the line.
fn escape_into(out: &mut String, chars: &[char]) {
    for &c in chars {
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '~' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\\n"),
            _ => out.push(c),
        }
    }
}

/// A paragraph's text must not read as a block marker.
fn guard_paragraph(rendered: String) -> String {
    let chars: Vec<char> = rendered.chars().collect();
    let first = chars.first().copied();
    let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
    let numbered = digits > 0 && matches!(chars.get(digits), Some('.' | ')'));
    match first {
        Some('#' | '>' | '-' | '+') => format!("\\{rendered}"),
        _ if numbered => {
            let head: String = chars.iter().take(digits).collect();
            let tail: String = chars.iter().skip(digits).collect();
            format!("{head}\\{tail}")
        }
        _ => rendered,
    }
}

/// One non-code block as a line, without its id.
pub(crate) fn render_line(shape: &Shape, text: &str, marks: &[MarkSpan]) -> String {
    let body = || inline(text, marks);
    let pad = |indent: &i64| "  ".repeat(usize::try_from(*indent).unwrap_or(0));
    match shape {
        Shape::Paragraph => guard_paragraph(body()),
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

/// What makes two blocks the same line, ids aside: its shape, its text and
/// the marks the dialect carries.
pub(crate) fn line_key(shape: &Shape, text: &str, marks: &[MarkSpan]) -> String {
    match shape {
        Shape::Code(lang) => format!("```{}\n{text}", lang.as_deref().unwrap_or("")),
        _ => render_line(shape, text, marks),
    }
}

/// The fence that holds `lines` of code.
fn fence_for(lines: &[&str]) -> String {
    let longest = lines
        .iter()
        .map(|line| {
            let line = line.trim_start();
            let run = line.chars().take_while(|c| *c == '`').count();
            if run >= 3 { run } else { 0 }
        })
        .max()
        .unwrap_or(0);
    "`".repeat(longest.saturating_add(1).max(3))
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
            .flat_map(|block| block.text.split('\n'))
            .collect();
        let fence = fence_for(&lines);
        out.push_str(&fence);
        out.push_str(lang.as_deref().unwrap_or(""));
        out.push('\n');
        for block in &run {
            let mut parts = block.text.split('\n').peekable();
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
pub(crate) enum Ids<'a> {
    /// A `^id` names the block, when `known` says it is one.
    Known(&'a dyn Fn(&str) -> Option<String>),
    /// Ids are dropped: the lines are new blocks.
    Discard,
}

/// One block of parsed Markdown.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Parsed {
    /// A block of the page, left as it is (`[table] ^id`).
    Keep(String),
    Block(Item),
}

/// One block a line of Markdown says.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    pub shape: Shape,
    pub text: String,
    pub marks: Vec<MarkSpan>,
    /// The block this line names, a full id.
    pub id: Option<String>,
    /// The fenced code this line is part of, by order of appearance.
    pub fence: Option<usize>,
}

impl Item {
    pub(crate) fn key(&self) -> String {
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
///
/// # Errors
/// Names a line the dialect cannot carry (a table) or an id that names no
/// block of the page.
pub(crate) fn parse(markdown: &str, ids: &Ids<'_>) -> Result<Vec<Parsed>, String> {
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

fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation()
}

/// The inline marks of `source`, and its text without their delimiters.
pub(crate) fn parse_inline(source: &str) -> (String, Vec<MarkSpan>) {
    let chars: Vec<char> = source.chars().collect();
    let mut text: Vec<char> = Vec::new();
    let mut spans = Vec::new();
    inline_into(&chars, &mut text, &mut spans);
    spans.sort_by_key(|span: &MarkSpan| (span.start, span.end));
    (text.into_iter().collect(), spans)
}

/// Whether `source` says anything in inline Markdown: a mark (**bold**,
/// _italic_, `code`, ~~strike~~, [link](url)) or a backslash escape. Text with
/// neither reads the same as plain text, so writing it plain keeps the marks
/// it is written into; text with either means what the Markdown says, as
/// the read that showed it to the writer does.
pub(crate) fn has_inline_markdown(source: &str) -> bool {
    let (text, marks) = parse_inline(source);
    !marks.is_empty() || text != source
}

fn run_of(chars: &[char], from: usize, c: char) -> usize {
    chars
        .iter()
        .skip(from)
        .take_while(|candidate| **candidate == c)
        .count()
}

/// Where the code span opened at `from` (a run of `ticks` backticks) closes.
fn code_end(chars: &[char], from: usize, ticks: usize) -> Option<usize> {
    let mut at = from.saturating_add(ticks);
    while at < chars.len() {
        if chars.get(at) == Some(&'`') {
            let run = run_of(chars, at, '`');
            if run == ticks {
                return Some(at);
            }
            at = at.saturating_add(run);
        } else {
            at = at.saturating_add(1);
        }
    }
    None
}

/// The `]` that closes the `[` at `from`, when `](url)` follows it.
fn link_end(chars: &[char], from: usize) -> Option<(usize, usize, String)> {
    let mut depth = 0_usize;
    let mut at = from;
    while let Some(c) = chars.get(at) {
        match c {
            '\\' => at = at.saturating_add(1),
            '`' => {
                let run = run_of(chars, at, '`');
                match code_end(chars, at, run) {
                    Some(end) => at = end.saturating_add(run).saturating_sub(1),
                    None => at = at.saturating_add(run).saturating_sub(1),
                }
            }
            '[' => depth = depth.saturating_add(1),
            ']' => {
                if depth == 0 {
                    break;
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
        at = at.saturating_add(1);
    }
    let close = at;
    if chars.get(close) != Some(&']') || chars.get(close.saturating_add(1)) != Some(&'(') {
        return None;
    }
    let url_start = close.saturating_add(2);
    let url_len = chars
        .iter()
        .skip(url_start)
        .take_while(|c| **c != ')')
        .count();
    let end = url_start.saturating_add(url_len);
    if chars.get(end) != Some(&')') {
        return None;
    }
    let url: String = chars
        .get(url_start..end)?
        .iter()
        .collect::<String>()
        .replace("%20", " ")
        .replace("%28", "(")
        .replace("%29", ")");
    Some((close, end, url))
}

/// Where the emphasis `delim` opened before `from` closes.
fn emphasis_end(chars: &[char], from: usize, c: char, len: usize) -> Option<usize> {
    let mut at = from;
    while let Some(&current) = chars.get(at) {
        if current == '\\' {
            at = at.saturating_add(2);
            continue;
        }
        if current == '`' {
            let run = run_of(chars, at, '`');
            at = code_end(chars, at, run)
                .map_or_else(|| at.saturating_add(run), |end| end.saturating_add(run));
            continue;
        }
        if current == c {
            let run = run_of(chars, at, c);
            let previous = at.checked_sub(1).and_then(|before| chars.get(before));
            let next = chars.get(at.saturating_add(run));
            let closes = run >= len
                && at > from
                && !previous.is_some_and(|p| p.is_whitespace())
                && (c != '_' || !is_alphanumeric(next));
            if closes {
                // Of a longer run, the last delimiters close; the rest are inside.
                return Some(at.saturating_add(run).saturating_sub(len));
            }
            at = at.saturating_add(run);
            continue;
        }
        at = at.saturating_add(1);
    }
    None
}

fn mark_span(start: usize, end: usize, name: &str, value: MarkValue) -> MarkSpan {
    MarkSpan {
        start,
        end,
        name: name.to_owned(),
        value,
    }
}

fn inline_into(chars: &[char], text: &mut Vec<char>, spans: &mut Vec<MarkSpan>) {
    let mut at = 0;
    while let Some(&c) = chars.get(at) {
        let next = at.saturating_add(1);
        match c {
            '\\' if chars.get(next).copied().is_some_and(is_punctuation) => {
                text.extend(chars.get(next));
                at = next.saturating_add(1);
            }
            '`' => {
                let run = run_of(chars, at, '`');
                if let Some(end) = code_end(chars, at, run) {
                    let inner = chars.get(at.saturating_add(run)..end).unwrap_or_default();
                    let start = text.len();
                    text.extend_from_slice(inner);
                    spans.push(mark_span(
                        start,
                        text.len(),
                        "fontFamily",
                        MarkValue::Str("mono".to_owned()),
                    ));
                    at = end.saturating_add(run);
                } else {
                    text.extend(std::iter::repeat_n('`', run));
                    at = at.saturating_add(run);
                }
            }
            '[' => {
                if let Some((close, end, url)) = link_end(chars, next)
                    && crate::action::valid_hyperlink_target(&url)
                {
                    let start = text.len();
                    inline_into(chars.get(next..close).unwrap_or_default(), text, spans);
                    spans.push(mark_span(start, text.len(), "link", MarkValue::Str(url)));
                    at = end.saturating_add(1);
                } else {
                    text.push('[');
                    at = next;
                }
            }
            '*' | '_' | '~' => {
                let run = run_of(chars, at, c);
                let len = if run >= 2 { 2 } else { 1 };
                let before = at.checked_sub(1).and_then(|before| chars.get(before));
                let after = chars.get(at.saturating_add(len));
                let opens = !after.is_none_or(|a| a.is_whitespace())
                    && (c != '_' || !is_alphanumeric(before))
                    && (c != '~' || len == 2);
                let closing = opens
                    .then(|| emphasis_end(chars, at.saturating_add(len), c, len))
                    .flatten();
                if let Some(end) = closing {
                    let start = text.len();
                    inline_into(
                        chars.get(at.saturating_add(len)..end).unwrap_or_default(),
                        text,
                        spans,
                    );
                    let name = match (c, len) {
                        ('~', _) => "strike",
                        (_, 2) => "strong",
                        _ => "em",
                    };
                    spans.push(mark_span(start, text.len(), name, MarkValue::Bool(true)));
                    at = end.saturating_add(len);
                } else {
                    text.push(c);
                    at = next;
                }
            }
            _ => {
                text.push(c);
                at = next;
            }
        }
    }
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
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

    #[test]
    fn an_unknown_id_stays_the_text_it_is() {
        let none: [&str; 0] = [];
        let parsed = parse("see ^caret\n", &Ids::Known(&known(&none))).unwrap();
        let Parsed::Block(item) = &parsed[0] else {
            panic!("a block")
        };
        assert_eq!(item.text, "see ^caret");
        assert_eq!(item.id, None);
    }
}
