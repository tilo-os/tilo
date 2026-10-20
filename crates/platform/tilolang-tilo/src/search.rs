//! What search finds of a room: the text of every record and item of every package.
//!
//! Every `Text` field of a type is searchable by default; a hit is the record or item that holds
//! it, found by its text, and it opens the host at that resource (its `open` view action, see
//! [`crate::effects::OpenEffect`]). A package will be able to say which fields to index; until
//! then all of them are, the most useful default for a notes app and a chat alike.

use std::collections::{BTreeMap, BTreeSet};

use tilolang_runtime::ids::DocId;
use tilolang_runtime::meta::{FieldKind, Meta};
use tilolang_runtime::store::{ANY_FIELD, FieldSpec, FieldValue, Store as _};
use tilolang_runtime::value::ResourceRef;

use crate::host::Host;
use crate::publish::resource_id;
use crate::record::{DynRecord, DynValue};
use crate::schema::{MergeKind, TypeSchema, lookup};
use crate::store::{TiloStore, Watch};

/// One searchable resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The id the resource is published under (`tl:<app>:<path>`): what its `open` action is
    /// addressed to.
    pub resource: String,
    /// The app the resource belongs to, as people call it.
    pub app_title: String,
    /// The type of the resource.
    pub type_name: String,
    /// The first of its texts: what the hit reads as.
    pub title: String,
    /// Every text it holds, in field order.
    pub texts: Vec<String>,
}

/// The searchable resources of the record `doc` and the items below it.
#[must_use]
pub fn hits(meta: &Meta, record: &DynRecord, doc: &str) -> Vec<Hit> {
    let mut out = Vec::new();
    collect(meta, record, doc, &mut out);
    out
}

fn collect(meta: &Meta, record: &DynRecord, path: &str, out: &mut Vec<Hit>) {
    let mut texts = Vec::new();
    for field in &record.schema().fields {
        if let Some(DynValue::Text(text)) = record.get(&field.id)
            && !text.trim().is_empty()
        {
            texts.push(text.clone());
        }
    }
    if let Some(title) = texts.first().cloned() {
        out.push(Hit {
            resource: resource_id(&meta.app, path),
            app_title: meta.title.clone(),
            type_name: record.schema().name.clone(),
            title,
            texts,
        });
    }
    for field in &record.schema().fields {
        if let Some(DynValue::Collection(items)) = record.get(&field.id) {
            for (item_id, item) in items.iter() {
                collect(
                    meta,
                    item,
                    &format!("{path}/{}:{}", field.id, item_id.as_str()),
                    out,
                );
            }
        }
    }
}

/// The kind a package's types are published under, for grouping hits by app.
#[must_use]
pub fn group_of(meta: &Meta) -> String {
    meta.title.clone()
}

/// What search holds of a room, kept up to date by redoing only the resources that changed.
///
/// A keystroke in one note re-reads that note's texts; an item added or removed reads that
/// collection's membership and indexes the new item. Only a resource that appeared, or a
/// document that changed as a whole (the mirror copied anew), is read in full.
pub struct SearchIndex {
    watch: Watch,
    /// Hits by resource path (`doc`, then `/field:item` per step).
    hits: BTreeMap<String, Hit>,
    /// The items of each collection indexed, by the collection's path (`resource/field`).
    items: BTreeMap<String, BTreeSet<String>>,
    /// The documents indexed, with their app.
    docs: BTreeMap<DocId, String>,
}

/// What an update of a [`SearchIndex`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchDelta {
    /// Hits added or whose texts changed.
    pub upserted: Vec<Hit>,
    /// The published ids ([`Hit::resource`]) of hits gone.
    pub removed: Vec<String>,
}

impl SearchDelta {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.upserted.is_empty() && self.removed.is_empty()
    }
}

impl SearchIndex {
    /// An empty index following `host`'s store; the first [`Self::update`] indexes everything.
    #[must_use]
    pub fn new(host: &Host) -> Self {
        Self {
            watch: host.store().watch(),
            hits: BTreeMap::new(),
            items: BTreeMap::new(),
            docs: BTreeMap::new(),
        }
    }

    /// Every hit, by resource path.
    pub fn hits(&self) -> impl Iterator<Item = &Hit> {
        self.hits.values()
    }

    /// Bring the index up to date with the room: what changed since the last update.
    pub fn update(&mut self, host: &Host) -> SearchDelta {
        let changes = self.watch.take();
        let store = host.store();
        let mut delta = Delta::default();
        let mut now: BTreeMap<DocId, (Meta, &'static TypeSchema)> = BTreeMap::new();
        for (package, type_name, doc) in host.records() {
            let schema = package
                .meta
                .types
                .iter()
                .find(|t| t.name == type_name)
                .and_then(|t| lookup(&t.id));
            if let Some(schema) = schema {
                now.insert(doc, (package.meta.clone(), schema));
            }
        }
        let gone: Vec<DocId> = self
            .docs
            .keys()
            .filter(|doc| !now.contains_key(*doc))
            .cloned()
            .collect();
        for doc in gone {
            self.forget(&doc, &mut delta);
            self.docs.remove(&doc);
        }
        for (doc, (meta, schema)) in &now {
            let whole =
                changes.everything || changes.whole.contains(doc) || !self.docs.contains_key(doc);
            if whole {
                self.forget(doc, &mut delta);
                self.docs.insert(doc.clone(), meta.app.clone());
                // A whole document is read once, as a record, rather than field by field.
                match store.record(doc) {
                    Ok(record) => self.index_record(meta, &record, doc, &mut delta),
                    Err(_) => self.index(store, meta, schema, doc, &mut delta),
                }
                continue;
            }
            let Some(fields) = changes.docs.get(doc) else {
                continue;
            };
            for key in fields {
                let Some(at) = schema_at(schema, &key.resource) else {
                    continue;
                };
                if key.field == ANY_FIELD {
                    self.forget(&key.resource, &mut delta);
                    self.index(store, meta, at, &key.resource, &mut delta);
                    continue;
                }
                let Some(field) = at.field(&key.field) else {
                    continue;
                };
                match field.kind {
                    MergeKind::Text => self.retext(store, meta, at, &key.resource, &mut delta),
                    MergeKind::Collection => {
                        self.membership(store, meta, at, &key.resource, &field.id, &mut delta);
                    }
                    _ => {}
                }
            }
        }
        delta.finish(&self.hits)
    }

    /// Drop the hits of the resource at `path` and of everything below it.
    fn forget(&mut self, path: &str, delta: &mut Delta) {
        let below = format!("{path}/");
        let hits: Vec<String> = std::iter::once(path.to_owned())
            .chain(
                self.hits
                    .range(below.clone()..)
                    .take_while(|(p, _)| p.starts_with(&below))
                    .map(|(p, _)| p.clone()),
            )
            .collect();
        for p in hits {
            if let Some(hit) = self.hits.remove(&p) {
                delta.removed.insert(p, hit.resource);
            }
        }
        let items: Vec<String> = self
            .items
            .range(below.clone()..)
            .take_while(|(p, _)| p.starts_with(&below))
            .map(|(p, _)| p.clone())
            .collect();
        for p in items {
            self.items.remove(&p);
        }
    }

    /// Index `record`, read whole, at `path`.
    fn index_record(&mut self, meta: &Meta, record: &DynRecord, path: &str, delta: &mut Delta) {
        for hit in hits(meta, record, path) {
            let at = hit
                .resource
                .strip_prefix(&resource_id(&meta.app, ""))
                .unwrap_or_default()
                .to_owned();
            delta.upserted.insert(at.clone());
            delta.removed.remove(&at);
            self.hits.insert(at, hit);
        }
        members(record, path, &mut self.items);
    }

    /// Index the resource at `path` and everything below it.
    fn index(
        &mut self,
        store: &TiloStore,
        meta: &Meta,
        schema: &TypeSchema,
        path: &str,
        delta: &mut Delta,
    ) {
        self.retext(store, meta, schema, path, delta);
        for field in schema
            .fields
            .iter()
            .filter(|f| f.kind == MergeKind::Collection)
        {
            self.membership(store, meta, schema, path, &field.id, delta);
        }
    }

    /// Read the texts of the resource at `path` again.
    fn retext(
        &mut self,
        store: &TiloStore,
        meta: &Meta,
        schema: &TypeSchema,
        path: &str,
        delta: &mut Delta,
    ) {
        let Some(at) = ResourceRef::parse_id(path, schema.id.clone()) else {
            return;
        };
        let texts: Vec<String> = schema
            .fields
            .iter()
            .filter(|f| f.kind == MergeKind::Text)
            .filter_map(|f| match store.read(&at, &spec(&f.id, FieldKind::Text)) {
                Ok(FieldValue::Text(text)) if !text.trim().is_empty() => Some(text),
                _ => None,
            })
            .collect();
        let hit = texts.first().cloned().map(|title| Hit {
            resource: resource_id(&meta.app, path),
            app_title: meta.title.clone(),
            type_name: schema.name.clone(),
            title,
            texts,
        });
        match hit {
            Some(hit) => {
                if self.hits.get(path) != Some(&hit) {
                    delta.upserted.insert(path.to_owned());
                    delta.removed.remove(path);
                    self.hits.insert(path.to_owned(), hit);
                }
            }
            None => {
                if let Some(old) = self.hits.remove(path) {
                    delta.removed.insert(path.to_owned(), old.resource);
                }
            }
        }
    }

    /// Read which items the collection `field` of the resource at `path` holds: index the new
    /// ones, forget the gone ones.
    fn membership(
        &mut self,
        store: &TiloStore,
        meta: &Meta,
        schema: &TypeSchema,
        path: &str,
        field: &str,
        delta: &mut Delta,
    ) {
        let Some(item_schema) = schema
            .field(field)
            .and_then(|f| f.record.as_deref())
            .and_then(lookup)
        else {
            return;
        };
        let Some(at) = ResourceRef::parse_id(path, schema.id.clone()) else {
            return;
        };
        let now: BTreeSet<String> = match store.read(&at, &spec(field, FieldKind::Collection)) {
            Ok(FieldValue::Items(ids)) => ids.into_iter().collect(),
            _ => BTreeSet::new(),
        };
        let collection = format!("{path}/{field}");
        let before = self.items.remove(&collection).unwrap_or_default();
        for gone in before.difference(&now) {
            self.forget(&format!("{collection}:{gone}"), delta);
        }
        for new in now.difference(&before) {
            self.index(
                store,
                meta,
                item_schema,
                &format!("{collection}:{new}"),
                delta,
            );
        }
        self.items.insert(collection, now);
    }
}

/// The hits an update touched, by path, settled against the index at the end.
#[derive(Default)]
struct Delta {
    upserted: BTreeSet<String>,
    removed: BTreeMap<String, String>,
}

impl Delta {
    fn finish(self, hits: &BTreeMap<String, Hit>) -> SearchDelta {
        let upserted: Vec<Hit> = self
            .upserted
            .iter()
            .filter_map(|path| hits.get(path).cloned())
            .collect();
        let removed = self
            .removed
            .into_iter()
            .filter(|(path, _)| !hits.contains_key(path))
            .map(|(_, resource)| resource)
            .collect();
        SearchDelta { upserted, removed }
    }
}

/// The items of every collection of `record` (at `path`) and below.
fn members(record: &DynRecord, path: &str, out: &mut BTreeMap<String, BTreeSet<String>>) {
    for field in &record.schema().fields {
        if let Some(DynValue::Collection(items)) = record.get(&field.id) {
            let collection = format!("{path}/{}", field.id);
            let mut ids = BTreeSet::new();
            for (id, item) in items.iter() {
                ids.insert(id.as_str().to_owned());
                members(item, &format!("{collection}:{}", id.as_str()), out);
            }
            out.insert(collection, ids);
        }
    }
}

fn spec(id: &str, kind: FieldKind) -> FieldSpec {
    FieldSpec {
        id: id.to_owned(),
        kind,
    }
}

/// The type of the resource at `path` below a document of type `root`.
fn schema_at(root: &'static TypeSchema, path: &str) -> Option<&'static TypeSchema> {
    let mut schema = root;
    for step in path.split('/').skip(1) {
        let (field, _) = step.split_once(':')?;
        schema = schema.field(field)?.record.as_deref().and_then(lookup)?;
    }
    Some(schema)
}
