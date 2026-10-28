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
//! its reference ([`REFERENCE_SCHEME`]). Search, notifications and backlinks read the marks,
//! never the prose.
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

/// What a `mention` mark refers to: a person, or a resource by the catalogue's reference
/// (`tilo_reference::embed`), the one form native apps write too.
pub use tilo_reference::Embed as Reference;

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

    /// The text, a line per paragraph: what search indexes and a plain-text reader reads. A
    /// reference reads as its run (`@Ada`).
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// What a row shows of it: its first line with text, as plain text.
    #[must_use]
    pub fn excerpt(&self) -> String {
        self.blocks
            .iter()
            .flat_map(|b| b.text.lines())
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .to_owned()
    }

    /// Every reference it holds, in the order the text says them, each once.
    #[must_use]
    pub fn references(&self) -> Vec<Reference> {
        let mut out: Vec<Reference> = Vec::new();
        for b in &self.blocks {
            let mut marks: Vec<&MarkSpan> = b.marks.iter().filter(|m| m.name == MENTION).collect();
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

    /// Append `other` with its formatting: its first paragraph continues the last one, the
    /// rest follow it.
    pub fn push_rich(&mut self, other: &Self) {
        let mut blocks = other.blocks.iter();
        if let (Some(first), Some(last)) = (blocks.next(), self.blocks.last_mut()) {
            let shift = last.text.chars().count();
            last.text.push_str(&first.text);
            last.marks.extend(first.marks.iter().map(|m| MarkSpan {
                start: m.start.saturating_add(shift),
                end: m.end.saturating_add(shift),
                ..m.clone()
            }));
        } else if let Some(first) = other.blocks.first() {
            self.blocks.push(BlockView {
                id: String::new(),
                ..first.clone()
            });
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
///
/// # Errors
/// A message when the document cannot be written.
pub fn store_under(doc: &mut AutoCommit, obj: &ObjId, value: &RichValue) -> Result<(), String> {
    if tilo_doc::model::open(doc, obj).is_err() {
        tilo_doc::model::init(doc, obj);
    }
    tilo_doc::reconcile_body_under(doc, obj, &value.blocks)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Make the field `key` of the map `parent` hold rich text, empty, when it holds none yet.
///
/// # Errors
/// A message when the document cannot be written.
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

    #[test]
    fn the_excerpt_is_the_first_line_with_text() {
        assert_eq!(
            RichValue::from_text("\n  \nHello there\nmore").excerpt(),
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
        assert_eq!(v.references(), [ada, ch]);
        assert_eq!(v.plain_text(), "Ask @Ada in #general, thanks @Ada");
    }
}
