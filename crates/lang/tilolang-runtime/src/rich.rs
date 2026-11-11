//! `RichText` values: tilo-doc's rich text, the one rich-text model of the platform.
//!
//! A value is the body's paragraphs as tilo-doc reads them ([`BlockView`]: kind, text, marks,
//! attributes and, once stored, the paragraph's id). A field of type `RichText` is stored as
//! tilo-doc's rich text under the field ([`store_under`]), and writing a value over it brings
//! the stored text to read as the value by splices and marks ([`tilo_doc::reconcile_body_under`]),
//! so a concurrent edit by someone else merges instead of being replaced.
//!
//! A reference inside the text is a run marked `mention`, as native apps mark one: the run is
//! what the text says (`@Ada`, `#general`), the mark's value names what it refers to. A person
//! is named by their participant id, as native Chat always named one; any other resource by
//! its reference ([`Reference`]). Search, notifications and backlinks read the marks,
//! never the prose.
//!
//! The run keeps the title its resource had when the text was written. Plain text shown to a
//! reader is read for them ([`RichValue::read_text`]): a resource they hold by its title now,
//! one they cannot read as only being there, as the reference's chip reads; the run as written
//! ([`RichValue::written_text`]) is never shown.
//!
//! Paragraphs are cited by anchor, `¶2`, or `¶2.1` for the first item of the list that is the
//! second top-level paragraph ([`RichValue::anchors`]).

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjId, ObjType};
use serde::{Deserialize, Serialize};
use tilo_doc::document::{MarkSpan, MarkValue};
use tilo_doc::model::Region;
use tilo_doc::snapshot::BlockView;

/// The mark a reference's run carries, valued with what it refers to (`tilo_doc`'s and
/// ui-core's `mention`).
pub const MENTION: &str = tilo_reference::embed::MARK;

/// The mark a paragraph projecting another resource carries: one level deep, what a card of it
/// shows of its own content (`tilo_doc`'s and ui-core's `Mark::Transclusion`).
pub const PROJECTION: &str = tilo_doc::schema::MARK_PROJECTION;

/// What a `mention` mark refers to: a person, or a resource by the catalogue's reference
/// (`tilo_reference::embed`), the one form native apps write too.
pub use tilo_reference::Embed as Reference;

/// How a reference reads in plain text for one reader (`tilo_reference::embed::Reading`).
pub use tilo_reference::embed::{Reading, UNSHARED, UNSHARED_PHRASE};

/// The catalogue's id of the tilolang resource `id` of `app`, as its app publishes it
/// (`tilolang_tilo::publish::resource_id`).
#[must_use]
pub fn published_id(app: &str, id: &str) -> String {
    format!("tl:{app}:{id}")
}

/// The catalogue's resource kind of the tilolang type `ty` of `app`
/// (`tilolang_tilo::meta::MetaExt::kind_of`).
#[must_use]
pub fn published_kind(app: &str, ty: &str) -> String {
    format!("tl.{app}.{ty}")
}

/// The reference to the tilolang resource `target`, of the type named `ty`, of the app `app`:
/// the catalogue's reference to it, as its app publishes it.
#[must_use]
pub fn resource_reference(app: &str, ty: &str, target: &crate::value::ResourceRef) -> Reference {
    Reference::resource(
        app,
        published_kind(app, ty),
        published_id(app, &target.id_string()),
    )
}

/// The resource id (`ResourceRef::id_string`) a reference names, when it names a tilolang
/// resource.
#[must_use]
pub fn resource_id(reference: &Reference) -> Option<&str> {
    let at = reference.as_resource()?;
    at.reference
        .id
        .strip_prefix("tl:")?
        .strip_prefix(at.app.as_str())?
        .strip_prefix(':')
        .filter(|id| !id.is_empty())
}

/// A `RichText` value (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RichValue {
    pub blocks: Vec<BlockView>,
}

fn paragraph(text: &str) -> BlockView {
    BlockView {
        id: String::new(),
        kind: "paragraph".to_owned(),
        text: text.to_owned(),
        marks: Vec::new(),
        attrs: std::collections::BTreeMap::new(),
        children: Vec::new(),
    }
}

/// Whether `block` is one projection of a resource (an embed) and nothing else: a card of it
/// where the text is drawn.
fn projects(block: &BlockView) -> bool {
    let len = block.text.chars().count();
    block
        .marks
        .iter()
        .any(|mark| mark.name == PROJECTION && mark.start == 0 && mark.end == len)
}

/// The paragraph `b` as one reader reads it ([`RichValue::read_text`]).
fn read_block(
    b: &BlockView,
    read: &mut dyn FnMut(&Reference) -> Reading,
    unshared: &str,
) -> String {
    let runs = b.marks.iter().filter_map(|m| match &m.value {
        MarkValue::Str(value) if m.name == MENTION || m.name == PROJECTION => {
            Some((m.start, m.end, value.as_str()))
        }
        _ => None,
    });
    tilo_reference::embed::read_text(&b.text, runs, read, unshared)
}

impl RichValue {
    /// `RichText("…")`: the text, a paragraph per line, unformatted.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        if text.is_empty() {
            return Self::default();
        }
        Self {
            blocks: text.split('\n').map(paragraph).collect(),
        }
    }

    /// Whether it says nothing: no paragraph holds text, and none is something other than
    /// text (an image, a divider).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks
            .iter()
            .all(|b| b.text.trim().is_empty() && b.kind == "paragraph" && b.children.is_empty())
    }

    /// The text as written, a line per paragraph: each reference as its run, which keeps the
    /// title its resource had when it was written. Never what a reader is shown: they read
    /// [`Self::read_text`].
    #[must_use]
    pub fn written_text(&self) -> String {
        self.blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The text as one reader reads it, a line per paragraph: each reference as `read` says it
    /// reads for them (a resource they hold by its title now, one they cannot read as
    /// `unshared`), as its chip does (`tilo_reference::embed::read_text`). What search, rows,
    /// notifications and agents read of it.
    #[must_use]
    pub fn read_text(&self, read: &mut dyn FnMut(&Reference) -> Reading, unshared: &str) -> String {
        self.blocks
            .iter()
            .map(|b| read_block(b, read, unshared))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// What a row shows of it to one reader: the first line with text of
    /// [`Self::read_text`].
    #[must_use]
    pub fn read_excerpt(
        &self,
        read: &mut dyn FnMut(&Reference) -> Reading,
        unshared: &str,
    ) -> String {
        for b in &self.blocks {
            if b.text.trim().is_empty() {
                continue;
            }
            let text = read_block(b, read, unshared);
            if let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty()) {
                return line.to_owned();
            }
        }
        String::new()
    }

    /// Whether any run of it is a `mention` mark naming `person` (`tilo_reference::embed::MARK`):
    /// what a mention count counts, read the way the system's mention notices read it.
    #[must_use]
    pub fn mentions(&self, person: &str) -> bool {
        self.blocks.iter().any(|b| {
            b.marks.iter().any(|m| {
                m.name == MENTION && matches!(&m.value, MarkValue::Str(value) if value == person)
            })
        })
    }

    /// Every reference it holds, in the order the text says them, each once: a run mentioning
    /// one, and a paragraph projecting one (an embed), which links it here as a mention does.
    #[must_use]
    pub fn references(&self) -> Vec<Reference> {
        let mut out: Vec<Reference> = Vec::new();
        for b in &self.blocks {
            let mut marks: Vec<&MarkSpan> = b
                .marks
                .iter()
                .filter(|m| m.name == MENTION || m.name == PROJECTION)
                .collect();
            marks.sort_by_key(|m| m.start);
            for m in marks {
                if let MarkValue::Str(v) = &m.value
                    && let Some(r) = Reference::of_mark(v)
                    && !out.contains(&r)
                {
                    out.push(r);
                }
            }
        }
        out
    }

    /// The references the text projects (an embed): what a card of it shows of itself, one
    /// level deep. Empty for text that only mentions and links.
    #[must_use]
    pub fn projections(&self) -> Vec<Reference> {
        let mut out: Vec<Reference> = Vec::new();
        for block in &self.blocks {
            for mark in &block.marks {
                if mark.name != PROJECTION {
                    continue;
                }
                if let MarkValue::Str(value) = &mark.value
                    && let Some(reference) = Reference::of_mark(value)
                    && !out.contains(&reference)
                {
                    out.push(reference);
                }
            }
        }
        out
    }

    /// Append `label` as a run referring to `to`, after the text so far, in the last paragraph
    /// (a new one when there is none).
    pub fn push_reference(&mut self, label: &str, to: &Reference) {
        if self.blocks.is_empty() {
            self.blocks.push(paragraph(""));
        }
        let Some(last) = self.blocks.last_mut() else {
            return;
        };
        let start = last.text.chars().count();
        last.text.push_str(label);
        let end = last.text.chars().count();
        if start < end {
            last.marks.push(MarkSpan {
                start,
                end,
                name: MENTION.to_owned(),
                value: MarkValue::Str(to.mark_value()),
            });
        }
    }

    /// Append the moment `at` (RFC 3339) as a run every reader reads in their own zone and
    /// language (`tilo_doc::inline_time`); the run is the moment as written, for a reader that
    /// shows text as it is.
    pub fn push_time(&mut self, at: &str) {
        if self.blocks.is_empty() {
            self.blocks.push(paragraph(""));
        }
        let Some(last) = self.blocks.last_mut() else {
            return;
        };
        let start = last.text.chars().count();
        last.text.push_str(at);
        let end = last.text.chars().count();
        if start < end {
            last.marks.push(MarkSpan {
                start,
                end,
                name: tilo_doc::inline_time::MARK.to_owned(),
                value: MarkValue::Str(
                    tilo_doc::inline_time::InlineTime {
                        at: at.to_owned(),
                        show: tilo_doc::inline_time::Show::default(),
                    }
                    .mark_value(),
                ),
            });
        }
    }

    /// Append `label` as a paragraph of its own projecting `to` (an embed): what a card of it
    /// shows where the text is drawn, opening it.
    pub fn push_projection(&mut self, label: &str, to: &Reference) {
        self.blocks.push(BlockView {
            id: String::new(),
            kind: "paragraph".to_owned(),
            text: label.to_owned(),
            marks: vec![MarkSpan {
                start: 0,
                end: label.chars().count(),
                name: PROJECTION.to_owned(),
                value: MarkValue::Str(to.mark_value()),
            }],
            attrs: std::collections::BTreeMap::new(),
            children: Vec::new(),
        });
    }

    /// Append `other` with its formatting: its first paragraph continues the last one, the
    /// rest follow it. A paragraph that is one projection (an embed) is a block of its own, so
    /// it never continues the paragraph before it.
    pub fn push_rich(&mut self, other: &Self) {
        let mut blocks = other.blocks.iter();
        let first = blocks.next();
        match (first, self.blocks.last_mut()) {
            (Some(first), Some(last)) if !projects(first) => {
                let shift = last.text.chars().count();
                last.text.push_str(&first.text);
                last.marks.extend(first.marks.iter().map(|m| MarkSpan {
                    start: m.start.saturating_add(shift),
                    end: m.end.saturating_add(shift),
                    ..m.clone()
                }));
            }
            (Some(first), _) => self.blocks.push(BlockView {
                id: String::new(),
                ..first.clone()
            }),
            (None, _) => {}
        }
        self.blocks.extend(blocks.map(|b| BlockView {
            id: String::new(),
            ..b.clone()
        }));
    }

    /// Append `text`, unformatted, to the last paragraph; a line break starts a new one.
    pub fn push_text(&mut self, text: &str) {
        let mut lines = text.split('\n');
        if let Some(first) = lines.next() {
            match self.blocks.last_mut() {
                Some(last) => last.text.push_str(first),
                None => self.blocks.push(paragraph(first)),
            }
        }
        for line in lines {
            self.blocks.push(paragraph(line));
        }
    }

    /// Each paragraph's anchor and its block: `¶1`, `¶2`, …, with the items of a list counted
    /// under the list's own number (`¶2.1`, `¶2.2`), as agents and quotes cite them.
    #[must_use]
    pub fn anchors(&self) -> Vec<(String, &BlockView)> {
        let mut out = Vec::with_capacity(self.blocks.len());
        let mut top = 0usize;
        let mut item = 0usize;
        let mut in_list = false;
        for b in &self.blocks {
            if b.kind == "list_item" {
                if !in_list {
                    top = top.saturating_add(1);
                    item = 0;
                    in_list = true;
                }
                item = item.saturating_add(1);
                out.push((format!("¶{top}.{item}"), b));
            } else {
                in_list = false;
                top = top.saturating_add(1);
                out.push((format!("¶{top}"), b));
            }
        }
        out
    }

    /// The paragraph `anchor` (`¶2`, `¶2.1`, with or without the `¶`) names; a list's own
    /// number names its first item.
    #[must_use]
    pub fn at_anchor(&self, anchor: &str) -> Option<&BlockView> {
        let wanted = anchor.trim().trim_start_matches('¶');
        let anchors = self.anchors();
        anchors
            .iter()
            .find(|(a, _)| a.trim_start_matches('¶') == wanted)
            .or_else(|| {
                let first = format!("{wanted}.1");
                anchors
                    .iter()
                    .find(|(a, _)| a.trim_start_matches('¶') == first)
            })
            .map(|(_, b)| *b)
    }

    /// The anchor of the paragraph whose id is `block`.
    #[must_use]
    pub fn anchor_of(&self, block: &str) -> Option<String> {
        self.anchors()
            .into_iter()
            .find(|(_, b)| !b.id.is_empty() && b.id == block)
            .map(|(a, _)| a)
    }
}

/// Write `value` as the rich text under the map `obj` of `doc`, making the rich text there
/// first when there is none: as splices and marks of the paragraphs that differ.
pub fn store_under(doc: &mut AutoCommit, obj: &ObjId, value: &RichValue) -> Result<(), String> {
    if tilo_doc::model::open(doc, obj).is_err() {
        tilo_doc::model::init(doc, obj);
    }
    tilo_doc::reconcile_body_under(doc, obj, &value.blocks)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Make the field `key` of the map `parent` hold rich text, empty, when it holds none yet.
pub fn ensure_field(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Result<ObjId, String> {
    use automerge::ReadDoc as _;
    if let Ok(Some((automerge::Value::Object(ObjType::Map), obj))) = doc.get(parent, key) {
        return Ok(obj);
    }
    let obj = doc
        .put_object(parent, key, ObjType::Map)
        .map_err(|e| e.to_string())?;
    tilo_doc::model::init(doc, &obj);
    Ok(obj)
}

/// The rich text under the map `obj`, as a value; empty when there is none.
#[must_use]
pub fn read_under(doc: &AutoCommit, obj: &ObjId) -> RichValue {
    match tilo_doc::model::open(doc, obj) {
        Ok(c) => RichValue {
            blocks: tilo_doc::model::materialize_region(doc, &c, Region::Body),
        },
        Err(_) => RichValue::default(),
    }
}

/// The mark a run the change inserted carries (`tilo_doc`'s and ui-core's `strong`, drawn
/// bold) and the one a run it deleted carries (`strike`, drawn struck): the two ways a
/// diff marks its runs, in the one form the editor and every reader of rich text know.
pub const INSERTED: &str = "strong";
pub const DELETED: &str = "strike";

impl RichValue {
    /// What a change made of one paragraph, as its two sides show it: `before` and `after`
    /// are the paragraph's text at the two sides of the change, and `inserted`/`deleted`
    /// are the ranges of each the change wrote and removed, as it recorded them.
    ///
    /// The before side reads the paragraph as it read and strikes what the change deleted;
    /// the after side reads it as it reads and marks what the change inserted. Each is one
    /// paragraph of its own: a note of a hundred paragraphs with one word typed in the
    /// fortieth shows the fortieth, never the other ninety-nine.
    #[must_use]
    pub fn sides(
        block: &BlockView,
        before: &str,
        after: &str,
        inserted: &[(usize, usize, String)],
        deleted: &[(usize, usize, String)],
    ) -> (Self, Self) {
        (
            Self {
                blocks: vec![marked_block(block, before, deleted, DELETED)],
            },
            Self {
                blocks: vec![marked_block(block, after, inserted, INSERTED)],
            },
        )
    }

    /// This text with each paragraph cut to what its own marking leaves: the marked runs,
    /// with `words` words of the paragraph around each, so a change deep in a long paragraph
    /// reads with its context and not with the paragraph. A paragraph with no marked run is
    /// no part of the change and is left out.
    #[must_use]
    pub fn related(&self, words: usize) -> Self {
        let blocks = self
            .blocks
            .iter()
            .map(|block| {
                let mut out = block.clone();
                let chars: Vec<char> = block.text.chars().collect();
                let marked: Vec<(usize, usize)> = block
                    .marks
                    .iter()
                    .filter(|m| m.name == INSERTED || m.name == DELETED)
                    .map(|m| (m.start, m.end))
                    .collect();
                if marked.is_empty() {
                    out.text.clear();
                    out.marks.clear();
                    return out;
                }
                let (mut from, mut to) = (
                    marked.iter().map(|(s, _)| *s).min().unwrap_or(0),
                    marked.iter().map(|(_, e)| *e).max().unwrap_or(0),
                );
                for _ in 0..words {
                    from = from.saturating_sub(1);
                    while from > 0 && chars.get(from).is_some_and(|c| c.is_whitespace()) {
                        from = from.saturating_sub(1);
                    }
                }
                for _ in 0..words {
                    to = to.saturating_add(1).min(chars.len());
                    while to < chars.len() && chars.get(to).is_some_and(|c| !c.is_whitespace()) {
                        to = to.saturating_add(1);
                    }
                }
                let mut window: String = String::new();
                if from > 0 {
                    window.push('…');
                    window.push(' ');
                }
                window.extend(chars.get(from..to).unwrap_or_default());
                if to < chars.len() {
                    window.push(' ');
                    window.push('…');
                }
                let prefix: usize = if from > 0 { 2 } else { 0 };
                out.text = window;
                out.marks = block
                    .marks
                    .iter()
                    .filter(|m| m.start >= from && m.end <= to)
                    .map(|m| MarkSpan {
                        start: m.start.saturating_sub(from).saturating_add(prefix),
                        end: m.end.saturating_sub(from).saturating_add(prefix),
                        ..m.clone()
                    })
                    .collect();
                out
            })
            .filter(|block| !block.text.is_empty())
            .collect();
        Self { blocks }
    }
}

/// `block` as one side of a change reads it: its text `text`, with the runs `ranges` names/// covered by the mark `name` (`strong` for an insertion, `strike` for a deletion) and the
/// paragraph's own marks beside them.
fn marked_block(
    block: &BlockView,
    text: &str,
    ranges: &[(usize, usize, String)],
    name: &str,
) -> BlockView {
    let mut marks: Vec<MarkSpan> = block
        .marks
        .iter()
        .filter(|mark| mark.end <= text.chars().count())
        .cloned()
        .collect();
    for (start, end, _) in ranges {
        if start < end {
            // Runs of one kind that meet read as one mark, so a word typed one character
            // after another is one run of marked text.
            match marks.last_mut() {
                Some(last) if last.name == name && last.end == *start => last.end = *end,
                _ => marks.push(MarkSpan {
                    start: *start,
                    end: *end,
                    name: name.to_owned(),
                    value: MarkValue::Bool(true),
                }),
            }
        }
    }
    marks.sort_by_key(|span| (span.start, span.end));
    BlockView {
        id: block.id.clone(),
        kind: block.kind.clone(),
        text: text.to_owned(),
        marks,
        attrs: block.attrs.clone(),
        children: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_number_paragraphs_and_the_items_of_a_list_under_it() {
        let mut v = RichValue::from_text("Intro\nfirst\nsecond\nOutro");
        for b in v.blocks.get_mut(1..3).unwrap_or_default() {
            b.kind = "list_item".into();
        }
        let names: Vec<String> = v.anchors().into_iter().map(|(a, _)| a).collect();
        assert_eq!(names, ["¶1", "¶2.1", "¶2.2", "¶3"]);
        assert_eq!(v.at_anchor("¶2.2").map(|b| b.text.as_str()), Some("second"));
        assert_eq!(v.at_anchor("2").map(|b| b.text.as_str()), Some("first"));
        assert_eq!(v.at_anchor("¶3").map(|b| b.text.as_str()), Some("Outro"));
        assert!(v.at_anchor("¶9").is_none());
    }

    /// The runs of `v`'s first paragraph covered by the mark `name`, as text.
    fn runs(v: &RichValue, name: &str) -> Vec<String> {
        v.blocks[0]
            .marks
            .iter()
            .filter(|m| m.name == name)
            .map(|m| {
                v.blocks[0]
                    .text
                    .chars()
                    .skip(m.start)
                    .take(m.end.saturating_sub(m.start))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_change_shows_its_insertions_marked_and_its_deletions_struck() {
        let block = RichValue::from_text("Groceries and milk").blocks[0].clone();
        // What the change recorded: the word it removed and the one it wrote.
        let deleted = [(14, 18, "milk".to_owned())];
        let inserted = [(14, 19, "bread".to_owned())];
        let (was, now) = RichValue::sides(
            &block,
            "Groceries and milk",
            "Groceries and bread",
            &inserted,
            &deleted,
        );
        assert_eq!(was.blocks[0].text, "Groceries and milk");
        assert_eq!(now.blocks[0].text, "Groceries and bread");
        assert_eq!(runs(&was, DELETED), ["milk"]);
        assert!(
            runs(&was, INSERTED).is_empty(),
            "before marks nothing written"
        );
        assert_eq!(runs(&now, INSERTED), ["bread"]);
        assert!(runs(&now, DELETED).is_empty(), "after strikes nothing");
    }

    #[test]
    fn a_word_replaced_reads_as_the_word_not_its_letters() {
        // "sourdough" and "bread" share letters, which a character diff of the two texts
        // would mark as untyped; what the change recorded is the word it wrote, whole.
        let block = RichValue::from_text("Oat milk and bread").blocks[0].clone();
        let deleted = [(13, 18, "bread".to_owned())];
        let inserted = [(13, 22, "sourdough".to_owned())];
        let (was, now) = RichValue::sides(
            &block,
            "Oat milk and bread",
            "Oat milk and sourdough",
            &inserted,
            &deleted,
        );
        assert_eq!(runs(&now, INSERTED), ["sourdough"]);
        assert_eq!(runs(&was, DELETED), ["bread"]);
    }

    #[test]
    fn characters_typed_one_after_another_are_one_marked_run() {
        let block = RichValue::from_text("a").blocks[0].clone();
        let inserted = [
            (1, 2, "x".to_owned()),
            (2, 3, "y".to_owned()),
            (3, 4, "z".to_owned()),
        ];
        let (_, now) = RichValue::sides(&block, "a", "axyz", &inserted, &[]);
        assert_eq!(runs(&now, INSERTED), ["xyz"], "one run, not one per key");
    }

    #[test]
    fn a_long_paragraph_cuts_to_the_change_and_a_few_words_of_context() {
        let block = RichValue::from_text("x").blocks[0].clone();
        let before =
            "the quick brown fox jumps over the lazy dog and then runs far away into the wood";
        let after =
            "the quick brown fox leaps over the lazy dog and then runs far away into the wood";
        let at = after.find("leaps").unwrap_or(0);
        let inserted = [(at, at + 5, "leaps".to_owned())];
        let deleted = [(at, at + 5, "jumps".to_owned())];
        let (was, now) = RichValue::sides(&block, before, after, &inserted, &deleted);
        let cut = now.related(2);
        assert_eq!(cut.blocks.len(), 1);
        assert!(
            cut.blocks[0].text.starts_with('…'),
            "{:?}",
            cut.blocks[0].text
        );
        assert!(
            cut.blocks[0].text.ends_with('…'),
            "{:?}",
            cut.blocks[0].text
        );
        assert!(
            cut.blocks[0].text.contains("leaps"),
            "{:?}",
            cut.blocks[0].text
        );
        assert!(
            cut.blocks[0].text.chars().count() < now.blocks[0].text.chars().count() / 2,
            "a few words, not the paragraph: {:?}",
            cut.blocks[0].text
        );
        let struck = was.related(2);
        assert!(struck.blocks[0].text.contains("jumps"), "{struck:?}");
    }

    #[test]
    fn the_excerpt_is_the_first_line_with_text() {
        assert_eq!(
            RichValue::from_text("\n  \nHello there\nmore")
                .read_excerpt(&mut |_| Reading::AsWritten, ""),
            "Hello there"
        );
        assert!(RichValue::from_text("\n  ").is_empty());
        assert!(!RichValue::from_text("x").is_empty());
    }

    #[test]
    fn references_are_read_from_the_marks_in_order_each_once() {
        let mut v = RichValue::from_text("Ask ");
        let ada = Reference::Person("u_ada".into());
        let ch = Reference::resource("so.tilo.chat", "chat.channel", "chat.channel:general");
        v.push_reference("@Ada", &ada);
        v.push_text(" in ");
        v.push_reference("#general", &ch);
        v.push_text(", thanks ");
        v.push_reference("@Ada", &ada);
        assert_eq!(v.references(), [ada, ch.clone()]);
        assert_eq!(v.written_text(), "Ask @Ada in #general, thanks @Ada");
        // A reader reads each reference as it reads for them, never by the run alone.
        let mut read = |r: &Reference| {
            if *r == ch {
                Reading::Unshared
            } else {
                Reading::Title("@Ada Lovelace".into())
            }
        };
        assert_eq!(
            v.read_text(&mut read, "Not shared with you"),
            "Ask @Ada Lovelace in Not shared with you, thanks @Ada Lovelace"
        );
    }
}
