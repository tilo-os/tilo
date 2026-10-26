//! What search finds of a room: the text of every record and item of every package.
//!
//! Every `Text` field of a type is searchable by default; a hit is the record or item that holds
//! it, found by its text, and it opens the host at that resource (its `open` view action, see
//! [`crate::effects::OpenEffect`]). A package will be able to say which fields to index; until
//! then all of them are, the most useful default for a notes app and a chat alike.

use std::collections::{BTreeMap, BTreeSet};

use tilolang_runtime::ids::DocId;
use tilolang_runtime::meta::{FieldKind, Meta};
use tilolang_runtime::store::{ANY_FIELD, FieldSpec, FieldValue, Splice, Store as _};
use tilolang_runtime::value::ResourceRef;

use crate::background::{Pending, Poll, Waker};
use crate::host::Host;
use crate::publish::resource_id;
use crate::record::{DynRecord, DynValue};
use crate::schema::{MergeKind, TypeSchema, lookup};
use crate::store::{Snapshot, TiloStore, Watch};

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
///
/// Reading a whole document (every one, the first time) happens on a thread of its own, from a
/// snapshot of the store: the frame does not wait for it. Until it ends, updates change nothing
/// and the changes since wait in the index's watch; the update after it ends takes the documents
/// in and then those changes. A host passes a [`Waker`] to get that frame.
pub struct SearchIndex {
    watch: Watch,
    /// Hits by resource path (`doc`, then `/field:item` per step).
    hits: BTreeMap<String, Hit>,
    /// The items of each collection indexed, by the collection's path (`resource/field`).
    items: BTreeMap<String, BTreeSet<String>>,
    /// The documents indexed, with their app.
    docs: BTreeMap<DocId, String>,
    /// Documents being read whole off the frame.
    reading: Option<(Vec<DocId>, Pending<Vec<Read>>)>,
    wake: Option<Waker>,
}

/// One document read whole off the frame: its hits by path and its collections' items, or the
/// document to index field by field on the frame when it could not be read as a record.
enum Read {
    Indexed {
        doc: DocId,
        app: String,
        hits: Vec<(String, Hit)>,
        items: BTreeMap<String, BTreeSet<String>>,
    },
    Unread {
        doc: DocId,
    },
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
            reading: None,
            wake: None,
        }
    }

    /// Have `wake` called (from another thread) when documents read off the frame are ready
    /// for the next [`Self::update`] to take in.
    pub fn set_waker(&mut self, wake: Waker) {
        self.wake = Some(wake);
    }

    /// Whether documents are being read off the frame: the next update after they are ready
    /// takes them in.
    #[must_use]
    pub fn is_reading(&self) -> bool {
        self.reading.is_some()
    }

    /// [`Self::update`], waiting first for documents being read off the frame: for a host
    /// without frames (a test, the bench, an agent's headless room).
    pub fn settle(&mut self, host: &Host) -> SearchDelta {
        let mut delta = self.update(host);
        while let Some((docs, reading)) = self.reading.take() {
            let mut out = Delta::default();
            let reads = reading.wait();
            self.take_in(host, &docs, reads, &mut out);
            let first = out.finish(&self.hits);
            let rest = self.update(host);
            delta = merged(delta, first, rest);
        }
        delta
    }

    /// Every hit, by resource path.
    pub fn hits(&self) -> impl Iterator<Item = &Hit> {
        self.hits.values()
    }

    /// Bring the index up to date with the room: what changed since the last update.
    pub fn update(&mut self, host: &Host) -> SearchDelta {
        let mut delta = Delta::default();
        if let Some((docs, reading)) = self.reading.take() {
            match reading.poll() {
                Poll::Running => {
                    self.reading = Some((docs, reading));
                    return SearchDelta::default();
                }
                read => self.take_in(host, &docs, read, &mut delta),
            }
        }
        let changes = self.watch.take();
        let store = host.store();
        let mut whole_docs = Vec::new();
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
                self.docs.remove(doc);
                whole_docs.push((doc.clone(), meta.clone()));
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
                        let spliced = changes.listing(key).is_some_and(|splices| {
                            self.splice_membership(
                                (store, meta, at),
                                (&key.resource, &field.id),
                                splices,
                                &mut delta,
                            )
                        });
                        if !spliced {
                            self.membership(store, meta, at, &key.resource, &field.id, &mut delta);
                        }
                    }
                    _ => {}
                }
            }
        }
        if !whole_docs.is_empty() {
            // A whole document is read once, as a record, rather than field by field, and off
            // the frame. Changes from now on stay in the watch until it is taken in.
            let snapshot = host.store().snapshot();
            let docs = whole_docs.iter().map(|(doc, _)| doc.clone()).collect();
            self.reading = Some((
                docs,
                Pending::start(
                    move || read_whole(&snapshot, &whole_docs),
                    self.wake.clone(),
                ),
            ));
        }
        delta.finish(&self.hits)
    }

    /// Take in documents read off the frame; a document that could not be read as a record,
    /// or all of them if the reading was lost, is indexed field by field here.
    fn take_in(&mut self, host: &Host, docs: &[DocId], read: Poll<Vec<Read>>, delta: &mut Delta) {
        let reads = match read {
            Poll::Ready(reads) => reads,
            Poll::Running | Poll::Lost => docs
                .iter()
                .map(|doc| Read::Unread { doc: doc.clone() })
                .collect(),
        };
        let store = host.store();
        for read in reads {
            match read {
                Read::Indexed {
                    doc,
                    app,
                    hits,
                    mut items,
                } => {
                    self.forget(&doc, delta);
                    for (at, hit) in hits {
                        delta.upserted.insert(at.clone());
                        delta.removed.remove(&at);
                        self.hits.insert(at, hit);
                    }
                    self.items.append(&mut items);
                    self.docs.insert(doc, app);
                }
                Read::Unread { doc } => {
                    let Some((package, schema)) = host
                        .records()
                        .into_iter()
                        .find(|(_, _, d)| *d == doc)
                        .and_then(|(package, type_name, _)| {
                            let schema = package
                                .meta
                                .types
                                .iter()
                                .find(|t| t.name == type_name)
                                .and_then(|t| lookup(&t.id))?;
                            Some((package, schema))
                        })
                    else {
                        continue;
                    };
                    self.forget(&doc, delta);
                    self.index(store, &package.meta, schema, &doc, delta);
                    self.docs.insert(doc, package.meta.app.clone());
                }
            }
        }
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

impl SearchIndex {
    /// Apply the steps of the listing of the collection `field` of the resource at `path` to
    /// the items it is known to hold: index the items that arrived, forget those that left, and
    /// leave one moved as it is. False, with nothing changed, when the steps do not fit what
    /// it holds or would leave it with another count than the listing's: its items are then
    /// read again.
    fn splice_membership(
        &mut self,
        (store, meta, schema): (&TiloStore, &Meta, &TypeSchema),
        (path, field): (&str, &str),
        splices: &[Splice],
        delta: &mut Delta,
    ) -> bool {
        let Some(item_schema) = schema
            .field(field)
            .and_then(|f| f.record.as_deref())
            .and_then(lookup)
        else {
            return false;
        };
        let Some(at) = ResourceRef::parse_id(path, schema.id.clone()) else {
            return false;
        };
        let Ok(listing) = store.listing(&at, &spec(field, FieldKind::Collection)) else {
            return false;
        };
        let collection = format!("{path}/{field}");
        let Some(held) = self.items.get_mut(&collection) else {
            return false;
        };
        // Whether each item the steps name is held once they are taken.
        let mut after: BTreeMap<&str, bool> = BTreeMap::new();
        for splice in splices {
            let (item, arrives) = match splice {
                Splice::Insert { item, .. } => (item.as_str(), true),
                Splice::Remove { item, .. } => (item.as_str(), false),
            };
            let held_now = after
                .get(item)
                .copied()
                .unwrap_or_else(|| held.contains(item));
            if held_now == arrives {
                return false;
            }
            after.insert(item, arrives);
        }
        let arrived: Vec<String> = after
            .iter()
            .filter(|(item, arrives)| **arrives && !held.contains(**item))
            .map(|(item, _)| (*item).to_owned())
            .collect();
        let left: Vec<String> = after
            .iter()
            .filter(|(item, arrives)| !**arrives && held.contains(**item))
            .map(|(item, _)| (*item).to_owned())
            .collect();
        let count = held
            .len()
            .saturating_add(arrived.len())
            .saturating_sub(left.len());
        if count != listing.len() {
            return false;
        }
        for item in &left {
            held.remove(item);
        }
        held.extend(arrived.iter().cloned());
        for item in left {
            self.forget(&format!("{collection}:{item}"), delta);
        }
        for item in arrived {
            self.index(
                store,
                meta,
                item_schema,
                &format!("{collection}:{item}"),
                delta,
            );
        }
        true
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

/// Read `docs` whole from `snapshot`: on a thread of its own.
fn read_whole(snapshot: &Snapshot, docs: &[(DocId, Meta)]) -> Vec<Read> {
    let mut out = Vec::new();
    for (doc, meta) in docs {
        let Ok(record) = snapshot.record(doc) else {
            out.push(Read::Unread { doc: doc.clone() });
            continue;
        };
        let prefix = resource_id(&meta.app, "");
        let hits = hits(meta, &record, doc)
            .into_iter()
            .map(|hit| {
                let at = hit
                    .resource
                    .strip_prefix(&prefix)
                    .unwrap_or_default()
                    .to_owned();
                (at, hit)
            })
            .collect();
        let mut items = BTreeMap::new();
        members(&record, doc, &mut items);
        out.push(Read::Indexed {
            doc: doc.clone(),
            app: meta.app.clone(),
            hits,
            items,
        });
    }
    out
}

/// Three deltas one after the other, as one.
fn merged(first: SearchDelta, second: SearchDelta, third: SearchDelta) -> SearchDelta {
    let mut upserted: BTreeMap<String, Hit> = BTreeMap::new();
    let mut removed: BTreeSet<String> = BTreeSet::new();
    for delta in [first, second, third] {
        for gone in delta.removed {
            upserted.remove(&gone);
            removed.insert(gone);
        }
        for hit in delta.upserted {
            removed.remove(&hit.resource);
            upserted.insert(hit.resource.clone(), hit);
        }
    }
    SearchDelta {
        upserted: upserted.into_values().collect(),
        removed: removed.into_iter().collect(),
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
