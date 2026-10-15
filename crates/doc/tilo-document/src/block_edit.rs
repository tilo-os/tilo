//! Edits of a body by block: insert blocks of any kind, set a block's text,
//! kind or done flag, indent, move, delete, and replace the body from
//! Markdown by diffing it against what is there.
//!
//! These are the [`RichTextAction`](crate::RichTextAction) variants an agent
//! or script reaches through the declared actions of
//! [`block_actions`](crate::block_actions). They name blocks by the ids
//! [`block_markdown`](crate::block_markdown) reads (a unique prefix is
//! enough), are checked against the document before anything is written, and
//! write through the same model as typing: each is one CRDT edit, one undo
//! step, and the text it writes is attributed to whoever invoked it.

use std::collections::{HashMap, HashSet};

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_doc::DocMut;
use tilo_doc::document::{MarkSpan, MarkValue};
use tilo_doc::model::{Containers, Region};
use tilo_doc::snapshot::{AttrVal, BlockView};
use tilo_resource::JsonSchema;

use crate::action::{MAX_ACTION_TEXT_CHARS, RichTextAction, Written};
use crate::block_markdown::{
    Ids, Item, MAX_HEADING, MAX_INDENT, Parsed, Shape, Want, parse, parse_inline, resolve,
};

/// The kinds of block an agent names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum BlockKind {
    #[default]
    Paragraph,
    Heading,
    Bullet,
    Numbered,
    Checklist,
    Quote,
    Code,
}

impl BlockKind {
    /// The names the declared actions take, in the order they are offered.
    pub const NAMES: [&'static str; 7] = [
        "paragraph",
        "heading",
        "bullet",
        "numbered",
        "checklist",
        "quote",
        "code",
    ];

    /// The kind named `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "paragraph" | "text" => Self::Paragraph,
            "heading" => Self::Heading,
            "bullet" | "list" => Self::Bullet,
            "numbered" | "ordered" => Self::Numbered,
            "checklist" | "todo" | "task" => Self::Checklist,
            "quote" => Self::Quote,
            "code" => Self::Code,
            _ => return None,
        })
    }
}

/// What kind of block, and what it needs of its kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlockShape {
    pub kind: BlockKind,
    /// A heading's level, 1 to 6 (default 1).
    #[serde(default)]
    pub level: Option<i64>,
    /// A list item's depth, 0 to 5 (default 0).
    #[serde(default)]
    pub indent: Option<i64>,
    /// A checklist item's state (default not done).
    #[serde(default)]
    pub done: Option<bool>,
    /// A code block's language.
    #[serde(default)]
    pub lang: Option<String>,
}

impl BlockShape {
    /// A shape of `kind` with nothing else said.
    #[must_use]
    pub fn of(kind: BlockKind) -> Self {
        Self {
            kind,
            level: None,
            indent: None,
            done: None,
            lang: None,
        }
    }

    pub(crate) fn shape(&self) -> Result<Shape, String> {
        let level = self.level.unwrap_or(1);
        let indent = self.indent.unwrap_or(0);
        if !(0..=MAX_INDENT).contains(&indent) {
            return Err(format!("indent is 0 to {MAX_INDENT}, not {indent}"));
        }
        Ok(match self.kind {
            BlockKind::Paragraph => Shape::Paragraph,
            BlockKind::Heading if (1..=MAX_HEADING).contains(&level) => Shape::Heading(level),
            BlockKind::Heading => {
                return Err(format!(
                    "a heading's level is 1 to {MAX_HEADING}, not {level}"
                ));
            }
            BlockKind::Bullet => Shape::Bullet(indent),
            BlockKind::Numbered => Shape::Numbered(indent),
            BlockKind::Checklist => Shape::Checklist(indent, self.done.unwrap_or(false)),
            BlockKind::Quote => Shape::Quote,
            BlockKind::Code => Shape::Code(self.lang.clone().filter(|lang| !lang.is_empty())),
        })
    }
}

/// A block to insert: its kind and its text, plain, with no Markdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewBlock {
    pub shape: BlockShape,
    pub text: String,
}

/// Where blocks go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Placement {
    /// Directly after the block with this id.
    After(String),
    /// First in the body.
    Start,
    /// Last in the body.
    End,
}

const MAX_BLOCKS: usize = 10_000;

fn body_order(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    tilo_doc::model::blocks_in_order(doc, c)
}

fn view_of(doc: &AutoCommit, c: &Containers, id: &str) -> Option<BlockView> {
    tilo_doc::model::materialize_blocks(doc, c, &[id.to_owned()]).pop()
}

fn block_id(order: &[String], token: &str) -> Result<String, String> {
    resolve(order.iter().map(String::as_str), token)
}

fn text_in_limits(text: &str) -> Result<(), String> {
    if text.chars().count() > MAX_ACTION_TEXT_CHARS {
        return Err(format!(
            "text is longer than {MAX_ACTION_TEXT_CHARS} characters"
        ));
    }
    Ok(())
}

/// The block `placement` puts blocks after: `None` is the start.
fn after_of(placement: &Placement, order: &[String]) -> Result<Option<String>, String> {
    match placement {
        Placement::After(token) => block_id(order, token).map(Some),
        Placement::Start => Ok(None),
        Placement::End => Ok(order.last().cloned()),
    }
}

/// Why characters `start..end` of `block` cannot be replaced with `text`.
fn check_range(
    doc: &AutoCommit,
    c: &Containers,
    order: &[String],
    block: &str,
    (start, end): (usize, usize),
    text: &str,
) -> Result<(), String> {
    let id = block_id(order, block)?;
    text_in_limits(text)?;
    if text.contains(['\n', '\r']) {
        return Err(
            "the text replaces characters inside one block, so it holds no line break; insert-markdown adds blocks"
                .to_owned(),
        );
    }
    let Some(view) = view_of(doc, c, &id).filter(|view| Shape::of_view(view).is_some()) else {
        return Err(format!(
            "block `{block}` is not a text block (a table or image)"
        ));
    };
    let len = view.text.chars().count();
    if start > end || end > len {
        return Err(format!(
            "characters {start}..{end} are not in block `{block}`, which holds {len}"
        ));
    }
    Ok(())
}

/// Why `action` cannot apply to the document, before anything is written.
///
/// # Errors
/// Names the stale or malformed target.
pub(crate) fn check(
    action: &RichTextAction,
    doc: &AutoCommit,
    c: &Containers,
) -> Result<(), String> {
    let order = body_order(doc, c);
    match action {
        RichTextAction::InsertBlocks { at, blocks } => {
            after_of(at, &order)?;
            if blocks.is_empty() || blocks.len() > MAX_BLOCKS {
                return Err(format!("insert 1 to {MAX_BLOCKS} blocks"));
            }
            for block in blocks {
                block.shape.shape()?;
                text_in_limits(&block.text)?;
            }
            Ok(())
        }
        RichTextAction::InsertMarkdown { at, markdown } => {
            after_of(at, &order)?;
            text_in_limits(markdown)?;
            let parsed = parse(markdown, &Ids::Discard)?;
            if parsed.is_empty() {
                return Err("the Markdown holds no blocks".to_owned());
            }
            if parsed.len() > MAX_BLOCKS {
                return Err(format!("the Markdown holds more than {MAX_BLOCKS} blocks"));
            }
            Ok(())
        }
        RichTextAction::SetBlockText { block, text, .. } => {
            let id = block_id(&order, block)?;
            text_in_limits(text)?;
            match view_of(doc, c, &id).as_ref().and_then(Shape::of_view) {
                Some(_) => Ok(()),
                None => Err(format!(
                    "block `{block}` is not a text block (a table or image)"
                )),
            }
        }
        RichTextAction::ReplaceBlockRange {
            block,
            start,
            end,
            text,
            ..
        } => check_range(doc, c, &order, block, (*start, *end), text),
        RichTextAction::SetBlockShape { block, shape } => {
            let id = block_id(&order, block)?;
            shape.shape()?;
            match view_of(doc, c, &id).as_ref().and_then(Shape::of_view) {
                Some(_) => Ok(()),
                None => Err(format!(
                    "block `{block}` is not a text block (a table or image)"
                )),
            }
        }
        RichTextAction::SetBlockDone { block, .. } | RichTextAction::ShiftIndent { block, .. } => {
            let id = block_id(&order, block)?;
            match view_of(doc, c, &id).as_ref().and_then(Shape::of_view) {
                Some(Shape::Bullet(_) | Shape::Numbered(_) | Shape::Checklist(..)) => Ok(()),
                _ => Err(format!(
                    "block `{block}` is not a list item; set its kind to bullet, numbered or checklist first"
                )),
            }
        }
        RichTextAction::MoveBlock { block, at } => {
            let id = block_id(&order, block)?;
            if let Placement::After(token) = at
                && block_id(&order, token)? == id
            {
                return Err("a block cannot be placed after itself".to_owned());
            }
            after_of(at, &order).map(|_| ())
        }
        RichTextAction::DeleteBlocks { blocks } => {
            if blocks.is_empty() {
                return Err("name the blocks to delete".to_owned());
            }
            for block in blocks {
                block_id(&order, block)?;
            }
            Ok(())
        }
        RichTextAction::SyncBody { markdown } => {
            text_in_limits(markdown)?;
            let known = |token: &str| block_id(&order, token).ok();
            let parsed = parse(markdown, &Ids::Known(&known))?;
            if parsed.len() > MAX_BLOCKS {
                return Err(format!("the Markdown holds more than {MAX_BLOCKS} blocks"));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Replace characters `start..end` of block `id` with `text` (inline
/// Markdown when `markdown`, outside a code block), and what was typed in.
/// Marks outside the range stay; inside it, the dialect marks are the ones
/// the Markdown says, and written text takes none from its neighbours.
fn replace_range(
    dm: &mut DocMut<'_>,
    id: &str,
    (start, end): (usize, usize),
    text: &str,
    markdown: bool,
) -> Option<Written> {
    let view = view_of(dm.doc, dm.c, id)?;
    let shape = Shape::of_view(&view)?;
    let len = view.text.chars().count();
    let start = start.min(len);
    let end = end.clamp(start, len);
    let inline = markdown && !shape.is_code();
    let (text, marks) = if inline {
        parse_inline(text)
    } else {
        (text.to_owned(), Vec::new())
    };
    let del = end.saturating_sub(start);
    if del == 0 && text.is_empty() {
        return None;
    }
    dm.splice_text(id, start, isize::try_from(del).unwrap_or(isize::MAX), &text);
    if !inline || text.is_empty() {
        return (!text.is_empty()).then(|| Written::body(id.to_owned(), start, &text));
    }
    let after = start.saturating_add(text.chars().count());
    // A mark that touched the replaced range may have stretched over what
    // was just written; the Markdown alone says what covers it.
    for name in DIALECT_MARKS {
        let touched = view.marks.iter().any(|mark| {
            mark.name == name && dialect_mark(mark) && mark.start <= end && mark.end >= start
        });
        if touched {
            dm.remove_mark(id, start, after, name);
        }
    }
    for mark in marks.iter().filter(|mark| dialect_mark(mark)) {
        dm.add_mark(
            id,
            start.saturating_add(mark.start),
            start.saturating_add(mark.end).min(after),
            &mark.name,
            mark_scalar(&mark.value),
        );
    }
    Some(Written::body(id.to_owned(), start, &text))
}

/// The text `new` is to `old`, as one splice: characters kept at the start
/// and the end are left alone, so what did not change keeps its marks,
/// authorship and the carets inside it.
fn splice_between(old: &str, new: &str) -> Option<(usize, usize, String)> {
    let old: Vec<char> = old.chars().collect();
    let new: Vec<char> = new.chars().collect();
    let prefix = old
        .iter()
        .zip(&new)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(old.len().min(new.len()).saturating_sub(prefix))
        .take_while(|(left, right)| left == right)
        .count();
    let del = old.len().saturating_sub(prefix).saturating_sub(suffix);
    let ins: String = new
        .iter()
        .skip(prefix)
        .take(new.len().saturating_sub(prefix).saturating_sub(suffix))
        .collect();
    (del > 0 || !ins.is_empty()).then_some((prefix, del, ins))
}

/// Make the stored block `id` (as `view` shows it, `None` for a new one)
/// take `shape`: its kind, and each attribute it governs, writing only what
/// differs.
fn apply_shape(dm: &mut DocMut<'_>, id: &str, view: Option<&BlockView>, shape: &Shape) {
    let stored = shape.stored();
    if view.map(|view| view.kind.as_str()) != Some(stored.kind) {
        dm.set_kind(id, stored.kind);
    }
    for (key, want) in stored.attrs {
        let have = view.and_then(|view| view.attrs.get(key));
        match want {
            Want::Set(value) if have != Some(&value) => match value {
                AttrVal::Bool(value) => dm.set_attr(id, key, value),
                AttrVal::Int(value) => dm.set_attr(id, key, value),
                AttrVal::Float(value) => dm.set_attr(id, key, value),
                AttrVal::Str(value) => dm.set_attr(id, key, value.as_str()),
            },
            Want::Clear if have.is_some() => dm.remove_attr_in(Region::Body, id, key),
            _ => {}
        }
    }
}

fn mark_scalar(value: &MarkValue) -> automerge::ScalarValue {
    match value {
        MarkValue::Bool(value) => automerge::ScalarValue::Boolean(*value),
        MarkValue::Int(value) => automerge::ScalarValue::Int(*value),
        MarkValue::Str(value) => automerge::ScalarValue::Str(value.as_str().into()),
    }
}

/// The marks the Markdown dialect carries, by the name they are stored under.
const DIALECT_MARKS: [&str; 5] = ["strong", "em", "strike", "link", "fontFamily"];

fn dialect_mark(mark: &MarkSpan) -> bool {
    match mark.name.as_str() {
        "fontFamily" => mark.value == MarkValue::Str("mono".to_owned()),
        name => DIALECT_MARKS.contains(&name),
    }
}

/// The value of the mark `name` over each character of the first `len`.
fn coverage(marks: &[&MarkSpan], name: &str, len: usize) -> HashMap<usize, MarkValue> {
    let mut cover = HashMap::new();
    for mark in marks.iter().filter(|mark| mark.name == name) {
        for at in mark.start..mark.end.min(len) {
            cover.insert(at, mark.value.clone());
        }
    }
    cover
}

/// Make the marks of the block `id` the dialect carries those of `wanted`,
/// leaving every other mark (a colour, a mention, an author) as it is.
/// Automerge splits a mark wherever an edit landed in it, so marks are
/// compared per character, and only a mark that differs is rewritten.
fn set_dialect_marks(dm: &mut DocMut<'_>, id: &str, wanted: &[MarkSpan]) {
    let Some(view) = view_of(dm.doc, dm.c, id) else {
        return;
    };
    let len = view.text.chars().count();
    let wanted: Vec<&MarkSpan> = wanted.iter().filter(|mark| dialect_mark(mark)).collect();
    let have: Vec<&MarkSpan> = view
        .marks
        .iter()
        .filter(|mark| dialect_mark(mark))
        .collect();
    for name in DIALECT_MARKS {
        let (had, wants) = (coverage(&have, name, len), coverage(&wanted, name, len));
        if had == wants {
            continue;
        }
        if !had.is_empty() {
            dm.remove_mark(id, 0, len, name);
        }
        let mut at = 0;
        while at < len {
            let Some(value) = wants.get(&at) else {
                at = at.saturating_add(1);
                continue;
            };
            let mut end = at.saturating_add(1);
            while wants.get(&end) == Some(value) {
                end = end.saturating_add(1);
            }
            dm.add_mark(id, at, end, name, mark_scalar(value));
            at = end;
        }
    }
}

/// Write `item` as a new block after `prev`; what was written.
fn insert_item(
    dm: &mut DocMut<'_>,
    prev: Option<&str>,
    item: &Item,
    groups: &mut HashMap<usize, String>,
    written: &mut Vec<Written>,
) -> String {
    let id = dm.insert_block(prev, item.shape.stored().kind);
    apply_shape(dm, &id, None, &item.shape);
    if item.shape.is_code() {
        let group = match item.fence {
            Some(fence) => groups.entry(fence).or_insert_with(|| dm.ids.mint()).clone(),
            None => dm.ids.mint(),
        };
        dm.set_attr(&id, "code_group", group.as_str());
    }
    if !item.text.is_empty() {
        dm.splice_text(&id, 0, 0, &item.text);
        written.push(Written::body(id.clone(), 0, &item.text));
    }
    let len = item.text.chars().count();
    for mark in &item.marks {
        if mark.start < mark.end && mark.end <= len {
            dm.add_mark(
                &id,
                mark.start,
                mark.end,
                &mark.name,
                mark_scalar(&mark.value),
            );
        }
    }
    id
}

fn plain_items(blocks: &[NewBlock]) -> Vec<Item> {
    blocks
        .iter()
        .filter_map(|block| {
            Some(Item {
                shape: block.shape.shape().ok()?,
                text: block.text.clone(),
                marks: Vec::new(),
                id: None,
                fence: None,
            })
        })
        .collect()
}

/// Insert `items` after `at`; a trailing empty paragraph a new body starts
/// with is replaced, as appending to it would.
fn insert_items(dm: &mut DocMut<'_>, at: &Placement, items: &[Item]) -> Vec<Written> {
    let order = body_order(dm.doc, dm.c);
    let Ok(after) = after_of(at, &order) else {
        return Vec::new();
    };
    let blank_end = (*at == Placement::End)
        .then(|| order.last())
        .flatten()
        .filter(|last| {
            view_of(dm.doc, dm.c, last).is_some_and(|view| {
                view.text.is_empty() && view.attrs.is_empty() && view.kind == "paragraph"
            })
        })
        .cloned();
    let mut written = Vec::new();
    let mut groups = HashMap::new();
    let mut prev = after;
    for item in items {
        prev = Some(insert_item(
            dm,
            prev.as_deref(),
            item,
            &mut groups,
            &mut written,
        ));
    }
    if let Some(blank) = blank_end {
        dm.tombstone_block_in(Region::Body, &blank);
    }
    written
}

/// Make the block `id`, shown as `view`, the line `item` says: its kind,
/// its text and its marks, writing only what differs.
fn update_block(
    dm: &mut DocMut<'_>,
    view: &BlockView,
    have: &Shape,
    item: &Item,
    written: &mut Vec<Written>,
) {
    let id = view.id.as_str();
    if *have != item.shape {
        apply_shape(dm, id, Some(view), &item.shape);
    }
    if let Some((at, del, ins)) = splice_between(&view.text, &item.text) {
        dm.splice_text(id, at, isize::try_from(del).unwrap_or(isize::MAX), &ins);
        if !ins.is_empty() {
            written.push(Written::body(id.to_owned(), at, &ins));
        }
    }
    if !item.shape.is_code() {
        set_dialect_marks(dm, id, &item.marks);
    }
}

/// The positions of the longest run of `values` that only grows.
fn longest_increasing(values: &[usize]) -> HashSet<usize> {
    let mut tails: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; values.len()];
    for (at, value) in values.iter().enumerate() {
        let place = tails.partition_point(|tail| values.get(*tail).is_some_and(|v| v < value));
        if let Some(before) = place.checked_sub(1).and_then(|index| tails.get(index))
            && let Some(slot) = previous.get_mut(at)
        {
            *slot = Some(*before);
        }
        match tails.get_mut(place) {
            Some(slot) => *slot = at,
            None => tails.push(at),
        }
    }
    let mut keep = HashSet::new();
    let mut at = tails.last().copied();
    while let Some(position) = at {
        keep.insert(position);
        at = previous.get(position).copied().flatten();
    }
    keep
}

/// Pairs `(i, j)` of a longest common subsequence of `a` and `b` under
/// `same`: the ends they share, then a table over what is between.
fn common<A, B>(a: &[A], b: &[B], same: impl Fn(&A, &B) -> bool) -> Vec<(usize, usize)> {
    let head = a.iter().zip(b).take_while(|(x, y)| same(x, y)).count();
    let tail = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(a.len().min(b.len()).saturating_sub(head))
        .take_while(|(x, y)| same(x, y))
        .count();
    let mut pairs: Vec<(usize, usize)> = (0..head).map(|at| (at, at)).collect();
    let a_mid = a
        .get(head..a.len().saturating_sub(tail))
        .unwrap_or_default();
    let b_mid = b
        .get(head..b.len().saturating_sub(tail))
        .unwrap_or_default();
    let width = b_mid.len().saturating_add(1);
    let mut table = vec![0_u32; a_mid.len().saturating_add(1).saturating_mul(width)];
    let cell = |i: usize, j: usize| i.saturating_mul(width).saturating_add(j);
    for i in (0..a_mid.len()).rev() {
        for j in (0..b_mid.len()).rev() {
            let value = match (a_mid.get(i), b_mid.get(j)) {
                (Some(x), Some(y)) if same(x, y) => table
                    .get(cell(i.saturating_add(1), j.saturating_add(1)))
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1),
                _ => {
                    let down = table
                        .get(cell(i.saturating_add(1), j))
                        .copied()
                        .unwrap_or(0);
                    let right = table
                        .get(cell(i, j.saturating_add(1)))
                        .copied()
                        .unwrap_or(0);
                    down.max(right)
                }
            };
            if let Some(slot) = table.get_mut(cell(i, j)) {
                *slot = value;
            }
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < a_mid.len() && j < b_mid.len() {
        if let (Some(x), Some(y)) = (a_mid.get(i), b_mid.get(j))
            && same(x, y)
        {
            pairs.push((head.saturating_add(i), head.saturating_add(j)));
            i = i.saturating_add(1);
            j = j.saturating_add(1);
        } else if table.get(cell(i.saturating_add(1), j)) >= table.get(cell(i, j.saturating_add(1)))
        {
            i = i.saturating_add(1);
        } else {
            j = j.saturating_add(1);
        }
    }
    let a_end = a.len().saturating_sub(tail);
    let b_end = b.len().saturating_sub(tail);
    pairs.extend((0..tail).map(|at| (a_end.saturating_add(at), b_end.saturating_add(at))));
    pairs
}

/// The most cells the table of a diff may fill: past it, blocks that are
/// not identical are replaced rather than matched by likeness.
const LIKENESS_CELLS: usize = 4_000_000;

fn words(text: &str) -> HashSet<String> {
    text.split_whitespace().map(str::to_lowercase).collect()
}

/// Whether an edited block is plausibly the same block as `old`.
fn alike(old: &BlockView, item: &Item) -> bool {
    if old.text == item.text {
        return true;
    }
    let (before, after) = (words(&old.text), words(&item.text));
    let shared = before.intersection(&after).count();
    let total = before.union(&after).count();
    total > 0 && shared.saturating_mul(2) >= total
}

/// One block of the new body: a block kept as it is, one already there and
/// edited, or a new one.
enum Entry {
    Kept(String),
    Edited(usize, Item),
    Fresh(Item),
}

/// Pair the blocks the Markdown does not name by id with the blocks there:
/// identical lines first, then the edited ones, in place.
fn pair_unnamed(
    old: &[(BlockView, Shape)],
    free_old: &[usize],
    free_new: &[(usize, &Item)],
) -> Vec<(usize, usize)> {
    let keys: Vec<String> = free_old
        .iter()
        .filter_map(|at| {
            old.get(*at).map(|(view, shape)| {
                crate::block_markdown::line_key(shape, &view.text, &view.marks)
            })
        })
        .collect();
    let exact = common(&keys, free_new, |key, (_, item)| *key == item.key());
    let mut matched: Vec<(usize, usize)> = exact.clone();
    let mut bounds = vec![(0, 0)];
    bounds.extend(
        exact
            .iter()
            .map(|(i, j)| (i.saturating_add(1), j.saturating_add(1))),
    );
    let mut stops: Vec<(usize, usize)> = exact.clone();
    stops.push((free_old.len(), free_new.len()));
    for ((from_old, from_new), (to_old, to_new)) in bounds.into_iter().zip(stops) {
        let olds: Vec<usize> = (from_old..to_old).collect();
        let news: Vec<usize> = (from_new..to_new).collect();
        if olds.is_empty() || news.is_empty() {
            continue;
        }
        if olds.len() == news.len() {
            matched.extend(olds.into_iter().zip(news));
            continue;
        }
        if olds.len().saturating_mul(news.len()) > LIKENESS_CELLS {
            continue;
        }
        let likeness = common(&olds, &news, |i, j| {
            let old_view = free_old.get(*i).and_then(|at| old.get(*at)).map(|(v, _)| v);
            let item = free_new.get(*j).map(|(_, item)| *item);
            matches!((old_view, item), (Some(view), Some(item)) if alike(view, item))
        });
        matched.extend(likeness.iter().map(|(i, j)| {
            (
                olds.get(*i).copied().unwrap_or_default(),
                news.get(*j).copied().unwrap_or_default(),
            )
        }));
        // What lies between the likenesses, if it is as many as it is many,
        // is the same blocks edited.
        let mut cuts = vec![(0, 0)];
        cuts.extend(
            likeness
                .iter()
                .map(|(i, j)| (i.saturating_add(1), j.saturating_add(1))),
        );
        let mut ends: Vec<(usize, usize)> = likeness;
        ends.push((olds.len(), news.len()));
        for ((a, b), (c, d)) in cuts.into_iter().zip(ends) {
            let (old_run, new_run) = (c.saturating_sub(a), d.saturating_sub(b));
            if old_run == new_run && old_run > 0 {
                matched.extend((a..c).zip(b..d).map(|(i, j)| {
                    (
                        olds.get(i).copied().unwrap_or_default(),
                        news.get(j).copied().unwrap_or_default(),
                    )
                }));
            }
        }
    }
    matched
}

/// Which block of `old` each line of `parsed` is: those it names by id, then
/// those whose line is unchanged, then those edited in place; and which of
/// `old` that took.
fn pair_lines(
    old: &[(BlockView, Shape)],
    at_of: &HashMap<&str, usize>,
    parsed: &[Parsed],
) -> (Vec<Option<usize>>, Vec<bool>) {
    let mut used = vec![false; old.len()];
    let mut paired: Vec<Option<usize>> = vec![None; parsed.len()];
    for (at, line) in parsed.iter().enumerate() {
        let id = match line {
            Parsed::Keep(_) => continue,
            Parsed::Block(item) => item.id.as_deref(),
        };
        if let Some(old_at) = id.and_then(|id| at_of.get(id)).copied()
            && let Some(taken) = used.get_mut(old_at)
            && !*taken
        {
            *taken = true;
            if let Some(slot) = paired.get_mut(at) {
                *slot = Some(old_at);
            }
        }
    }
    let free_old: Vec<usize> = (0..old.len())
        .filter(|at| !used.get(*at).copied().unwrap_or(true))
        .collect();
    let free_new: Vec<(usize, &Item)> = parsed
        .iter()
        .enumerate()
        .filter_map(|(at, line)| match line {
            Parsed::Block(item) if paired.get(at).copied().flatten().is_none() => Some((at, item)),
            _ => None,
        })
        .collect();
    for (i, j) in pair_unnamed(old, &free_old, &free_new) {
        if let (Some(old_at), Some((new_at, _))) = (free_old.get(i), free_new.get(j))
            && let Some(taken) = used.get_mut(*old_at)
            && let Some(slot) = paired.get_mut(*new_at)
            && !*taken
            && slot.is_none()
        {
            *taken = true;
            *slot = Some(*old_at);
        }
    }
    (paired, used)
}

/// Replace the body with `markdown`, as the fewest edits that make it so:
/// blocks named by id, or whose line is unchanged, are kept; edited ones are
/// edited in place; the rest are removed or inserted. Blocks Markdown cannot
/// carry stay where they are.
fn sync_body(dm: &mut DocMut<'_>, markdown: &str) -> Vec<Written> {
    let order = body_order(dm.doc, dm.c);
    let views = tilo_doc::model::materialize_region(dm.doc, dm.c, Region::Body);
    let known = |token: &str| block_id(&order, token).ok();
    let Ok(parsed) = parse(markdown, &Ids::Known(&known)) else {
        return Vec::new();
    };
    // The blocks Markdown can carry, in order: what the new body may reuse.
    let old: Vec<(BlockView, Shape)> = views
        .into_iter()
        .filter_map(|view| Shape::of_view(&view).map(|shape| (view, shape)))
        .collect();
    let at_of: HashMap<&str, usize> = old
        .iter()
        .enumerate()
        .map(|(at, (view, _))| (view.id.as_str(), at))
        .collect();
    let (paired, used) = pair_lines(&old, &at_of, &parsed);

    let entries: Vec<Entry> = parsed
        .into_iter()
        .zip(&paired)
        .map(|(line, pair)| match (line, pair) {
            (Parsed::Keep(id), _) => Entry::Kept(id),
            (Parsed::Block(item), Some(old_at)) => Entry::Edited(*old_at, item),
            (Parsed::Block(item), None) => Entry::Fresh(item),
        })
        .collect();

    // The blocks that must move: those out of the order the page has them in.
    let place: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(at, id)| (id.as_str(), at))
        .collect();
    let positions: Vec<usize> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Edited(at, _) => old
                .get(*at)
                .and_then(|(view, _)| place.get(view.id.as_str())),
            Entry::Kept(id) => place.get(id.as_str()),
            Entry::Fresh(_) => None,
        })
        .copied()
        .collect();
    let stay = longest_increasing(&positions);

    let mut written = Vec::new();
    for (at, (view, _)) in old.iter().enumerate() {
        if !used.get(at).copied().unwrap_or(true) {
            dm.tombstone_block_in(Region::Body, &view.id);
        }
    }
    let mut groups: HashMap<usize, String> = HashMap::new();
    for entry in &entries {
        if let Entry::Edited(at, item) = entry
            && let (Some(fence), Some((view, _))) = (item.fence, old.get(*at))
            && let Some(AttrVal::Str(group)) = view.attrs.get("code_group")
        {
            groups.entry(fence).or_insert_with(|| group.clone());
        }
    }
    let mut prev: Option<String> = None;
    let mut ordinal = 0_usize;
    for entry in entries {
        let id = match entry {
            Entry::Fresh(item) => {
                insert_item(dm, prev.as_deref(), &item, &mut groups, &mut written)
            }
            Entry::Kept(id) => {
                if !stay.contains(&ordinal) {
                    dm.move_block(&id, prev.as_deref());
                }
                ordinal = ordinal.saturating_add(1);
                id
            }
            Entry::Edited(at, item) => {
                let Some((view, shape)) = old.get(at) else {
                    continue;
                };
                if !stay.contains(&ordinal) {
                    dm.move_block(&view.id, prev.as_deref());
                }
                ordinal = ordinal.saturating_add(1);
                if item.key() != crate::block_markdown::line_key(shape, &view.text, &view.marks) {
                    update_block(dm, view, shape, &item, &mut written);
                }
                view.id.clone()
            }
        };
        prev = Some(id);
    }
    keep_a_paragraph(dm);
    written
}

fn keep_a_paragraph(dm: &mut DocMut<'_>) {
    if body_order(dm.doc, dm.c).is_empty() {
        dm.insert_block(None, "paragraph");
    }
}

/// Apply a block edit. The action was [`check`]ed against the same document.
/// What was typed in, for the invoker's authorship.
#[expect(
    clippy::too_many_lines,
    reason = "the dispatch over every block action, one arm each"
)]
pub(crate) fn apply(dm: &mut DocMut<'_>, action: &RichTextAction) -> Vec<Written> {
    let order = body_order(dm.doc, dm.c);
    let mut written = Vec::new();
    match action {
        RichTextAction::InsertBlocks { at, blocks } => {
            return insert_items(dm, at, &plain_items(blocks));
        }
        RichTextAction::InsertMarkdown { at, markdown } => {
            let Ok(parsed) = parse(markdown, &Ids::Discard) else {
                return written;
            };
            let items: Vec<Item> = parsed
                .into_iter()
                .filter_map(|line| match line {
                    Parsed::Block(item) => Some(item),
                    Parsed::Keep(_) => None,
                })
                .collect();
            return insert_items(dm, at, &items);
        }
        RichTextAction::SetBlockText {
            block,
            text,
            markdown,
        } => {
            let Ok(id) = block_id(&order, block) else {
                return written;
            };
            let Some(view) = view_of(dm.doc, dm.c, &id) else {
                return written;
            };
            let Some(shape) = Shape::of_view(&view) else {
                return written;
            };
            let (text, marks) = if *markdown && !shape.is_code() {
                parse_inline(text)
            } else {
                (text.clone(), Vec::new())
            };
            if let Some((at, del, ins)) = splice_between(&view.text, &text) {
                dm.splice_text(&id, at, isize::try_from(del).unwrap_or(isize::MAX), &ins);
                if !ins.is_empty() {
                    written.push(Written::body(id.clone(), at, &ins));
                }
            }
            if *markdown && !shape.is_code() {
                set_dialect_marks(dm, &id, &marks);
            }
        }
        RichTextAction::ReplaceBlockRange {
            block,
            start,
            end,
            text,
            markdown,
        } => {
            let Ok(id) = block_id(&order, block) else {
                return written;
            };
            if let Some(range) = replace_range(dm, &id, (*start, *end), text, *markdown) {
                written.push(range);
            }
        }
        RichTextAction::SetBlockShape { block, shape } => {
            let (Ok(id), Ok(shape)) = (block_id(&order, block), shape.shape()) else {
                return written;
            };
            if let Some(view) = view_of(dm.doc, dm.c, &id) {
                apply_shape(dm, &id, Some(&view), &shape);
            }
        }
        RichTextAction::SetBlockDone { block, done } => {
            let Ok(id) = block_id(&order, block) else {
                return written;
            };
            if let Some(view) = view_of(dm.doc, dm.c, &id) {
                let shape = match Shape::of_view(&view) {
                    Some(Shape::Checklist(indent, now)) => {
                        Shape::Checklist(indent, done.unwrap_or(!now))
                    }
                    Some(Shape::Bullet(indent) | Shape::Numbered(indent)) => {
                        Shape::Checklist(indent, done.unwrap_or(true))
                    }
                    _ => return written,
                };
                apply_shape(dm, &id, Some(&view), &shape);
            }
        }
        RichTextAction::ShiftIndent { block, by } => {
            let Ok(id) = block_id(&order, block) else {
                return written;
            };
            if let Some(view) = view_of(dm.doc, dm.c, &id) {
                let moved = |indent: i64| indent.saturating_add(*by).clamp(0, MAX_INDENT);
                let shape = match Shape::of_view(&view) {
                    Some(Shape::Bullet(indent)) => Shape::Bullet(moved(indent)),
                    Some(Shape::Numbered(indent)) => Shape::Numbered(moved(indent)),
                    Some(Shape::Checklist(indent, done)) => Shape::Checklist(moved(indent), done),
                    _ => return written,
                };
                apply_shape(dm, &id, Some(&view), &shape);
            }
        }
        RichTextAction::MoveBlock { block, at } => {
            let (Ok(id), Ok(after)) = (block_id(&order, block), after_of(at, &order)) else {
                return written;
            };
            if after.as_deref() != Some(id.as_str()) {
                dm.move_block(&id, after.as_deref());
            }
        }
        RichTextAction::DeleteBlocks { blocks } => {
            let mut ids: Vec<String> = blocks
                .iter()
                .filter_map(|token| block_id(&order, token).ok())
                .collect();
            ids.sort();
            ids.dedup();
            for id in &ids {
                dm.tombstone_block_in(Region::Body, id);
            }
            keep_a_paragraph(dm);
        }
        RichTextAction::SyncBody { markdown } => return sync_body(dm, markdown),
        _ => {}
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_splice_leaves_the_ends_the_text_keeps() {
        assert_eq!(
            splice_between("buy milk", "buy oat milk"),
            Some((4, 0, "oat ".into()))
        );
        assert_eq!(
            splice_between("buy milk", "buy"),
            Some((3, 5, String::new()))
        );
        assert_eq!(splice_between("same", "same"), None);
        assert_eq!(splice_between("aaa", "aa"), Some((2, 1, String::new())));
        assert_eq!(splice_between("é1", "é2"), Some((1, 1, "2".into())));
    }

    #[test]
    fn the_longest_increasing_run_stays_and_the_rest_moves() {
        let stay = longest_increasing(&[0, 3, 1, 2, 4]);
        let mut kept: Vec<usize> = stay.into_iter().collect();
        kept.sort_unstable();
        assert_eq!(kept, [0, 2, 3, 4]);
        assert!(longest_increasing(&[]).is_empty());
    }

    #[test]
    fn a_common_subsequence_pairs_what_both_hold() {
        let pairs = common(&["a", "b", "c", "d"], &["b", "x", "c", "d"], |a, b| a == b);
        assert_eq!(pairs, [(1, 0), (2, 2), (3, 3)]);
        assert!(common::<&str, &str>(&[], &["a"], |a, b| a == b).is_empty());
    }

    #[test]
    fn shapes_refuse_values_outside_their_range() {
        let mut shape = BlockShape::of(BlockKind::Heading);
        shape.level = Some(9);
        assert!(shape.shape().unwrap_err().contains("level"));
        let mut shape = BlockShape::of(BlockKind::Bullet);
        shape.indent = Some(6);
        assert!(shape.shape().unwrap_err().contains("indent"));
        assert_eq!(
            BlockShape::of(BlockKind::Checklist).shape().unwrap(),
            Shape::Checklist(0, false)
        );
        assert_eq!(BlockKind::parse("Checklist"), Some(BlockKind::Checklist));
        assert_eq!(BlockKind::parse("nope"), None);
    }
}
