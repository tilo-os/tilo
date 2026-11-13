//! A build whose field changed from `Text` to `RichText`: the records keep their words.
//!
//! A running app keeps its records in the workspace, and a new build of it may keep a field as rich
//! text where the old one kept plain text (`crate::upgrade::compare` lets that one type change
//! take over in place). The text a record holds there is not lost: when the workspace's origin opens
//! the record, every record of such a type — and every item of it — has the plain text it holds
//! there turned into rich text: the same characters, a paragraph per line, no marks, as an
//! ordinary change of the record. The record keeps its id and its items keep theirs; a value
//! that was not text stays exactly as it was, and a record that already holds rich text there
//! is not written at all, so a second open writes nothing.
//!
//! The change is a [`DynEdit::Edit`] under the field, applying as the splices and marks that
//! make the rich text read the words ([`tilo_document::RichTextAction::SyncClips`]), which a
//! field still holding plain text turns first ([`tilo_document::convert_field`]). It is
//! committed under the package version's migration principal, as a `migration action` is.

use automerge::{ObjType, ReadDoc as _};
use serde_json::Value as Json;
use tilolang_runtime::value::ResourceRef;

use super::Inner;
use crate::action::{Authorship, DynAction, DynEdit, EDIT_TEXT_ACTION};
use crate::items::Items;
use crate::schema::{MergeKind, TypeSchema, lookup};

impl Inner {
    /// Turn the plain text each record of `app` holds in a field its type now keeps as rich
    /// text, and write each as a change of the record: one committed action per record changed.
    /// `by` is the invoker the change is recorded under (the package version's migration
    /// principal). Returns how many records changed.
    ///
    /// A field is one to turn when its type keeps it as a [`MergeKind::RichText`] and a record
    /// holds a text object there: a record the new build made holds rich text from the start,
    /// so nothing is written for it, and a second open finds nothing to write.
    pub(super) fn upgrade_plain_text(&mut self, app: &str, module: &str, by: &str) -> usize {
        let docs: Vec<(String, String)> = self
            .index
            .iter()
            .filter(|(_, (a, _))| a == app)
            .map(|(doc, (_, ty))| (doc.clone(), ty.clone()))
            .collect();
        let mut changed = 0usize;
        for (doc, ty) in docs {
            if self.upgrade_record(app, module, by, &ty, &doc) {
                changed = changed.saturating_add(1);
            }
        }
        changed
    }

    /// [`Self::upgrade_plain_text`] of one record `doc` of the type `ty`: every field the type
    /// now keeps as rich text that the record — or an item of it — holds as plain text, turned
    /// and written, then committed as one action of the record. Whether it wrote anything.
    fn upgrade_record(&mut self, app: &str, module: &str, by: &str, ty: &str, doc: &str) -> bool {
        let Some(schema) = lookup(ty) else {
            return false;
        };
        let Ok(mirror) = self.held_doc(doc) else {
            return false;
        };
        let Some(root) = crate::reads::resource_obj(mirror, app, doc) else {
            return false;
        };
        let targets = self.upgrade_targets(app, doc, schema, &root);
        // What each record and item of it holds as plain text, read before any of it is written.
        let plans = self.upgrade_plans(targets, mirror);
        let mut edits: Vec<DynEdit> = Vec::new();
        let mut args = serde_json::Map::new();
        for (at, chunk, plain) in plans {
            let Ok(prefix) = self.prefix_of(&at) else {
                continue;
            };
            for (field, plain) in plain {
                let mut path = prefix.clone();
                path.push(tilo_resource::Segment::Field(field.clone()));
                let seed = format!("upgrade:{doc}:{field}").into_bytes();
                let edit = DynEdit::routed_at(
                    &self.chunks,
                    DynEdit::Edit {
                        id: doc.to_owned(),
                        edit: tilo_resource::Edit::new(path, plain_text_edit(&plain)),
                    },
                    chunk,
                );
                let key = tilolang_runtime::store::FieldKey::new(&at, field.clone());
                if self
                    .write(
                        app,
                        vec![edit.clone()],
                        &seed,
                        &[key],
                        (Vec::new(), Vec::new()),
                    )
                    .is_err()
                {
                    continue;
                }
                args.insert(field, Json::String(plain));
                edits.push(edit);
            }
        }
        if edits.is_empty() {
            return false;
        }
        self.enqueue(DynAction {
            app: app.to_owned(),
            kind: format!("tl.{app}.{}", schema.name),
            action_id: EDIT_TEXT_ACTION.to_owned(),
            args: Json::Object(args),
            authorship: Authorship {
                invoker: by.to_owned(),
                module: module.to_owned(),
                ..Authorship::default()
            },
            edits,
        });
        true
    }

    /// The record `doc` and every item of it whose own type keeps a field as rich text, each with
    /// the chunk its map is stored in.
    fn upgrade_targets(
        &self,
        app: &str,
        doc: &str,
        schema: &'static TypeSchema,
        root: &automerge::ObjId,
    ) -> Vec<Target> {
        let mut targets = Vec::new();
        let own = rich_fields(schema);
        if !own.is_empty() {
            targets.push((
                ResourceRef::root(doc.to_owned(), schema.id.clone()),
                self.chunks.chunk(doc),
                root.clone(),
                own,
            ));
        }
        let items = Items::across(&self.chunks, app, doc, schema);
        for (id, found) in &items.found {
            let (Some(obj), Some(item_schema)) =
                (found.obj.clone(), found.ty.as_deref().and_then(lookup))
            else {
                continue;
            };
            let rich = rich_fields(item_schema);
            if rich.is_empty() {
                continue;
            }
            targets.push((
                ResourceRef::root(doc.to_owned(), schema.id.clone())
                    .item(id, item_schema.id.clone()),
                found.chunk,
                obj,
                rich,
            ));
        }
        targets
    }

    /// What each target holds as plain text, read before any of it is written.
    fn upgrade_plans(&self, targets: Vec<Target>, record: &tilo_resource::Doc) -> Vec<Plan> {
        targets
            .into_iter()
            .map(|(at, chunk, obj, fields)| {
                let source = self.chunks.mirror(chunk).unwrap_or(record);
                let plain = fields
                    .into_iter()
                    .filter_map(|field| {
                        let plain = plain_text_at(Some(source), &obj, &field)?;
                        Some((field, plain))
                    })
                    .collect();
                (at, chunk, plain)
            })
            .collect()
    }
}

/// A field holder a build's text upgrade writes: the record or item, the chunk its map is
/// stored in, its map there, and the fields it holds as rich text now.
type Target = (
    ResourceRef,
    crate::chunk::ChunkNo,
    automerge::ObjId,
    Vec<String>,
);

/// What an upgrade writes of one [`Target`]: the plain text each of its fields holds.
type Plan = (ResourceRef, crate::chunk::ChunkNo, Vec<(String, String)>);

/// The fields of `schema` it keeps as rich text.
fn rich_fields(schema: &'static TypeSchema) -> Vec<String> {
    schema
        .fields
        .iter()
        .filter(|field| field.kind == MergeKind::RichText)
        .map(|field| field.id.clone())
        .collect()
}

/// The plain text the map `obj` of `doc` holds under `field`, when it holds a text object (a
/// field the build before this one kept as `Text`) rather than rich text.
fn plain_text_at(
    doc: Option<&tilo_resource::Doc>,
    obj: &automerge::ObjId,
    field: &str,
) -> Option<String> {
    let doc = doc?;
    match doc.get(obj, field).ok()? {
        Some((automerge::Value::Object(ObjType::Text), text)) => doc.text(&text).ok(),
        _ => None,
    }
}

/// The edit that makes a field read `plain` as rich text: the field's body brought to those
/// words, a paragraph per line and no marks ([`tilo_document::ClipBlock`]).
fn plain_text_edit(plain: &str) -> tilo_resource::EditOp {
    let blocks = tilo_document::blocks_of_plain(plain);
    let blocks = blocks
        .iter()
        .map(tilo_document::ClipBlock::from_view)
        .collect();
    tilo_failure::ignored!(
        "a `RichTextAction` always serializes",
        super::rich_edit(blocks, None, 0)
            .unwrap_or(tilo_resource::EditOp::Rich { rich: Json::Null })
    )
}
