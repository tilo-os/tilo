//! What a change wrote to the fields of a resource, before and after (`host.Version.changes`):
//! which fields from the change itself, and what each held on either side, read in place
//! through the platform's history ([`tilo_resource::history::wrote`] and
//! [`tilo_resource::history::held_at`]), at the cost of the values and never of the record. A
//! change never changes, so each is read once.
//!
//! A rich text field's two sides are the paragraphs the change typed into, read in place at
//! the change's dependencies and at the change ([`tilo_resource::history::spliced_texts`]),
//! with the runs it inserted marked and the runs it deleted struck ([`RichValue::sides`]).
//! Each paragraph is a text object of its own, so the cost is the paragraph's, never the
//! note's: a keystroke in one paragraph of a long note reads that paragraph twice.

use automerge::{ChangeHash, ReadDoc as _};
use tilolang_runtime::ids::FieldId;
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::store::{FieldSpec, FieldValue, StoreError};
use tilolang_runtime::value::ResourceRef;

use super::{TiloStore, backend, not_found};
use crate::reads::{field_of_json, locate_ref};
use crate::schema::MergeKind;

/// What one change wrote to the fields of one resource: each field, before and after.
pub(super) type Written = Vec<(FieldId, FieldValue, FieldValue)>;

/// Of what a change wrote, the fields among `fields`, in their order.
fn select_written(held: &Written, fields: &[FieldSpec]) -> Written {
    fields
        .iter()
        .filter_map(|field| held.iter().find(|(id, ..)| *id == field.id).cloned())
        .collect()
}

impl TiloStore {
    /// [`tilolang_runtime::Store::written`] — what the change `change` wrote to each of `fields`
    /// of `at`, before and after.
    pub(super) fn written_by(
        &self,
        at: &ResourceRef,
        fields: &[FieldSpec],
        change: &str,
    ) -> Result<Vec<(FieldId, FieldValue, FieldValue)>, StoreError> {
        let key = (change.to_owned(), at.id_string());
        if let Some(held) = self.inner.borrow().written.borrow().get(&key) {
            return Ok(select_written(held, fields));
        }
        self.load_record()?;
        let inner = self.inner.borrow();
        let (app, ty) = inner.locate(at)?;
        let mirror = inner.held_doc(&at.doc)?;
        let hash: ChangeHash = change
            .parse()
            .map_err(|_| not_found(format!("change {change}")))?;
        let located = locate_ref(mirror, Some(&inner.chunks), (&app, &ty), at, &inner.nested)?;
        let Some(obj) = located.obj.as_ref() else {
            // An item stored as its order key alone: no change wrote a field of it.
            return Ok(Vec::new());
        };
        // The change is the item's: it is in the chunk that holds the item's map.
        let mirror = inner.chunks.mirror(located.chunk).unwrap_or(mirror);
        let committed = mirror.committed_document();
        let made = committed
            .get_change_by_hash(&hash)
            .ok_or_else(|| not_found(format!("change {change}")))?;
        let keys = tilo_resource::history::wrote(committed, &hash, obj).map_err(backend)?;
        let mut held = Vec::with_capacity(keys.len());
        for prop in keys {
            let Some(spec) = located.schema.field(&prop) else {
                continue;
            };
            if matches!(spec.kind, MergeKind::Collection | MergeKind::Record) {
                continue;
            }
            if spec.kind == MergeKind::RichText {
                if let Some((before, after)) = Self::rich_sides(committed, obj, &prop, &hash)? {
                    held.push((spec.id.clone(), before, after));
                }
                continue;
            }
            let side = |heads: &[ChangeHash]| {
                let json = tilo_resource::history::held_at(committed, obj, &prop, heads)
                    .map_err(backend)?;
                field_of_json(&json, spec)
            };
            let before = side(made.deps())?;
            let after = side(&[hash])?;
            if before != after {
                held.push((spec.id.clone(), before, after));
            }
        }
        let out = select_written(&held, fields);
        inner.written.borrow_mut().insert(key, held);
        Ok(out)
    }

    /// What the change `hash` did to the rich text the key `prop` of `obj` holds, as its two
    /// sides show it: each paragraph it typed into, with the runs it inserted marked and the
    /// runs it deleted struck, cut to the change and a few words of context; `None` when it
    /// typed into none of them (it only marked a run, or wrote what was there).
    ///
    /// Which paragraphs, from the change's own splices; each read in place at the change's
    /// dependency and at the change ([`tilo_resource::history::spliced_texts`]), never the
    /// whole rich text, so the cost is the paragraph's.
    fn rich_sides(
        committed: &automerge::Automerge,
        obj: &automerge::ObjId,
        prop: &str,
        hash: &ChangeHash,
    ) -> Result<Option<(FieldValue, FieldValue)>, StoreError> {
        let Some(field) =
            tilo_resource::history::object_at(committed, obj, prop, &[*hash]).map_err(backend)?
        else {
            return Ok(None);
        };
        let edits =
            tilo_resource::history::spliced_texts(committed, hash, &field).map_err(backend)?;
        if edits.iter().all(tilo_resource::history::TextEdit::is_empty) {
            return Ok(None);
        }
        let (mut was, mut now) = (RichValue::default(), RichValue::default());
        for edit in edits {
            // The paragraph this text sits in, by the block its path names: its kind and
            // marks read in place, its text from the edit, which read it at both sides
            // already. A text no block of the rich text holds (one the change made) reads as
            // a paragraph of its own.
            let block =
                tilo_resource::history::object_at(committed, &field, edit.block(), &[*hash])
                    .map_err(backend)?
                    .and_then(|paragraph| read_block_heads(committed, &paragraph, &[*hash]))
                    .unwrap_or_else(|| plain_paragraph(&edit.before));
            // A text the change wrote into: what it wrote is marked, what it took away is
            // struck. A text it only spliced (a structure of paragraphs, the mark of a
            // paragraph) has neither, and is no part of what the reader sees.
            if edit.inserted.is_empty() && edit.deleted.is_empty() {
                continue;
            }
            let (b, a) = RichValue::sides(
                &block,
                &edit.before,
                &edit.after,
                &edit.inserted,
                &edit.deleted,
            );
            was.blocks.extend(b.related(CONTEXT_WORDS).blocks);
            now.blocks.extend(a.related(CONTEXT_WORDS).blocks);
        }
        Ok(Some((FieldValue::Rich(was), FieldValue::Rich(now))))
    }
}

/// How many words of a paragraph stand around a change on either side: a few, so a change
/// reads in its sentence and not with the whole paragraph.
const CONTEXT_WORDS: usize = 6;

/// A paragraph of plain text: its text, no marks, no attributes.
fn plain_paragraph(text: &str) -> tilo_doc::snapshot::BlockView {
    tilo_doc::snapshot::BlockView {
        id: String::new(),
        kind: "paragraph".to_owned(),
        text: text.to_owned(),
        marks: Vec::new(),
        attrs: std::collections::BTreeMap::new(),
        children: Vec::new(),
    }
}

/// The paragraph a block's map holds as of `heads`: its kind and the marks of its text,
/// read in place, with `text` empty (the caller has the text at both sides already); `None`
/// when the object is no block of a rich text.
fn read_block_heads(
    committed: &automerge::Automerge,
    block: &automerge::ObjId,
    heads: &[ChangeHash],
) -> Option<tilo_doc::snapshot::BlockView> {
    tilo_doc::model::block_at(committed, block, heads)
        .ok()
        .flatten()
}
