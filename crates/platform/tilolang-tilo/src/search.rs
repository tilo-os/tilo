//! What search finds of a workspace: the text of every record and item of every package.
//!
//! Every `Text` and `RichText` field of a type is searchable by default (a rich text by its text
//! as the person searching reads it: a reference to a resource they hold by what it is called
//! now, one they do not hold as not shared, never by the title it was written with,
//! [`crate::reading`]); a hit is the record or item that holds it, found by its text, and it opens the host at that resource (its `open` view action, see
//! [`crate::effects::OpenEffect`]). A package will be able to say which fields to index; until
//! then all of them are, the most useful default for a notes app and a chat alike.
//!
//! Search is the system's: a window publishes every hit to the session's search index, which
//! finds them as every Tilo surface finds things, and keeps them after the app closes. The hits
//! are read from each package's index ([`crate::index`]), the one the store's lists read: one
//! pass over its fields, nothing kept beside it.

use tilolang_runtime::meta::Meta;

use crate::host::Host;
use crate::publish::resource_id;
use crate::record::{DynRecord, DynValue};
use tilo_resource::search_index::{
    IndexAction, IndexDelta, IndexEntry, IndexState, IndexWeight, Publisher,
};

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
    /// The resources its rich text refers to, in the one form (`tilo_reference::embed`): the
    /// system's index finds it by what they are called now, and keeps its backlinks.
    pub references: Vec<String>,
}

/// The searchable resources of the record `doc` and the items below it, read from the record
/// whole, each text as the person `reader` holds resources for reads it: what the index's hits
/// are held to.
#[must_use]
pub fn hits(
    meta: &Meta,
    record: &DynRecord,
    doc: &str,
    reader: &dyn crate::reading::Holdings,
) -> Vec<Hit> {
    let mut out = Vec::new();
    collect(meta, record, doc, reader, &mut out);
    out
}

fn collect(
    meta: &Meta,
    record: &DynRecord,
    path: &str,
    reader: &dyn crate::reading::Holdings,
    out: &mut Vec<Hit>,
) {
    let mut texts = Vec::new();
    let mut references: Vec<String> = Vec::new();
    for field in &record.schema().fields {
        let text = match record.get(&field.id) {
            Some(DynValue::Text(text)) => text.clone(),
            Some(DynValue::RichText(rich)) => {
                let value = tilolang_runtime::rich::RichValue {
                    blocks: rich
                        .state()
                        .map(|state| {
                            state
                                .body()
                                .all()
                                .filter_map(|id| state.block(id))
                                .map(|block| (**block).clone())
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                for reference in value.references() {
                    let mark = reference.mark_value();
                    if reference.as_resource().is_some() && !references.contains(&mark) {
                        references.push(mark);
                    }
                }
                crate::reading::said(
                    reader,
                    path,
                    tilolang_runtime::store::FieldValue::Rich(value),
                )
                .unwrap_or_default()
            }
            _ => continue,
        };
        if !text.trim().is_empty() {
            texts.push(text);
        }
    }
    if let Some(title) = texts.first().cloned() {
        out.push(Hit {
            resource: resource_id(&meta.app, path),
            app_title: meta.title.clone(),
            type_name: record.schema().name.clone(),
            title,
            texts,
            references,
        });
    }
    for field in &record.schema().fields {
        if let Some(DynValue::Collection(items)) = record.get(&field.id) {
            for (item_id, item) in items.iter() {
                collect(
                    meta,
                    item,
                    &crate::index::derive::item_path(path, item_id.as_str()),
                    reader,
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

/// Every hit of the workspace, by package and path, read from the indexes now: what a window
/// publishes to the session's search index, which is what search finds things through. One pass
/// over each package's index. Each text is as the window's person reads it: the index matches a
/// query against what they read, never against the title of a resource they do not hold.
#[must_use]
pub fn every_hit(host: &Host) -> Vec<Hit> {
    hits_of(&host.store().snapshot(), &catalogue(host))
}

/// The apps the host runs a package of, each with the title people call it: what a hit is
/// grouped by and what it opens through. What a read off the frame takes with it, beside the
/// snapshot it reads.
#[must_use]
pub fn catalogue(host: &Host) -> Vec<(String, String)> {
    host.packages()
        .iter()
        .map(|package| (package.meta.app.clone(), package.meta.title.clone()))
        .collect()
}

/// [`every_hit`], read from `snapshot` and the apps `catalogue` names: what a thread of its own
/// reads in one pass, a workspace of ten thousand included.
fn hits_of(snapshot: &crate::store::Snapshot, catalogue: &[(String, String)]) -> Vec<Hit> {
    let _schemas = snapshot.scoped();
    let reader: &crate::store::Snapshot = snapshot;
    let mut out = Vec::new();
    for (app, title) in catalogue {
        let Some(view) = snapshot.view(app) else {
            continue;
        };
        let Ok(texted) = view.every_text() else {
            continue;
        };
        out.extend(texted.into_iter().filter_map(|held| {
            let texts: Vec<String> = held
                .texts
                .into_iter()
                .filter_map(|text| reader.said(&held.path, text))
                .collect();
            Some(Hit {
                resource: resource_id(app, &held.path),
                app_title: title.clone(),
                type_name: held.schema.name.clone(),
                title: texts.first()?.clone(),
                texts,
                references: held.references,
            })
        }));
    }
    out
}

/// The action that opens a resource at its anchor (`open(r)`), as every published resource
/// offers it bound to itself.
const OPEN_ACTION: &str = "open";

/// Every hit of the workspace as the session's persistent search index keeps it: one [`IndexEntry`]
/// per resource with text, short metadata only (never a prose body), keyed by the resource's
/// published id and opening it through its record's `open` action. This is what
/// a live tilolang instance publishes on its connection, so a Mail message or a note is found by
/// a word of its body through the system's one search, whether or not the app runs.
fn entries_of(
    snapshot: &crate::store::Snapshot,
    catalogue: &[(String, String)],
) -> Vec<IndexEntry> {
    hits_of(snapshot, catalogue)
        .into_iter()
        .filter_map(|hit| {
            entry(
                &hit.resource,
                &hit.title,
                &hit.type_name,
                &hit.app_title,
                &hit.texts,
                &hit.references,
            )
        })
        .collect()
}

/// The entry the session's persistent search index keeps of the resource at `path` of the
/// package `meta`, read now: none when it holds no text, or when it is gone. What an instance
/// publishes for the resources a change reached, instead of reading the workspace again.
#[must_use]
pub fn index_entry_at(host: &Host, meta: &Meta, path: &str) -> Option<IndexEntry> {
    let snapshot = host.store().snapshot();
    let _schemas = snapshot.scoped();
    let reader: &crate::store::Snapshot = &snapshot;
    let held = snapshot.view(&meta.app)?.texts_at(path).ok()??;
    let texts: Vec<String> = held
        .texts
        .into_iter()
        .filter_map(|text| reader.said(&held.path, text))
        .collect();
    let resource = resource_id(&meta.app, &held.path);
    entry(
        &resource,
        texts.first()?,
        &held.schema.name,
        &meta.title,
        &texts,
        &held.references,
    )
}

/// The index's entry of the resource `resource` with `texts`: short metadata first (a sender, a
/// subject), then long prose (a body), so the index keeps a bounded number of fields
/// ([`IndexEntry::bounded`]) with the metadata that ranks highest. A hit opens through the `open`
/// of the record that holds the resource, which every window showing anything of the record
/// publishes: the caller names the resource as the item to show, which no window may show yet.
fn entry(
    resource: &str,
    title: &str,
    type_name: &str,
    app_title: &str,
    texts: &[String],
    references: &[String],
) -> Option<IndexEntry> {
    let mut fields: Vec<(String, IndexWeight)> = texts
        .iter()
        .skip(1)
        .filter(|text| text.chars().count() <= SHORT_TEXT)
        .map(|text| (text.clone(), IndexWeight::Metadata))
        .collect();
    fields.extend(
        texts
            .iter()
            .skip(1)
            .filter(|text| text.chars().count() > SHORT_TEXT)
            .map(|text| (text.clone(), IndexWeight::Body)),
    );
    let entry = IndexEntry {
        key: resource.to_owned(),
        resource: resource.to_owned(),
        action: Some(IndexAction {
            resource: record_of(resource).to_owned(),
            action: OPEN_ACTION.to_owned(),
        }),
        title: title.to_owned(),
        subtitle: Some(type_name.to_owned()),
        group: Some(app_title.to_owned()),
        icon: None,
        boost: 0,
        fields,
        secret: false,
        references: references.to_vec(),
        ..IndexEntry::default()
    };
    entry.bounded()
}

/// The published record that holds the published resource `resource`: an item's path goes on
/// from its record's id after a `/`.
fn record_of(resource: &str) -> &str {
    resource
        .split_once('/')
        .map_or(resource, |(record, _)| record)
}

/// The longest text an entry keeps as a short field: a name, a subject, a sender.
const SHORT_TEXT: usize = 80;

/// The whole of a workspace's search index, read on a thread of its own: every resource with
/// text, one pass over the indexes, as publication's first trees are read. A frame after it ends
/// takes it in and sends what it holds; until then the window shows.
pub struct WholeIndex {
    building: crate::background::Pending<Published>,
}

/// What [`WholeIndex`] read.
pub struct Published {
    /// The publisher, holding the whole set as what it has sent.
    pub publisher: Publisher,
    /// One entry per resource with text, by the resource's published id.
    pub entries: std::collections::BTreeMap<String, IndexEntry>,
    /// The deltas that publish them, each as it crosses the broker.
    pub deltas: Vec<Vec<u8>>,
}

impl WholeIndex {
    /// Read the index of `host` off the frame, publishing every entry through `publisher` at
    /// `state`: what a first publication, a new route or a change the journal no longer reaches
    /// sends. `wake` is called once it is read.
    #[must_use]
    pub fn start(
        host: &Host,
        publisher: Publisher,
        state: IndexState,
        wake: Option<crate::background::Waker>,
    ) -> Self {
        let snapshot = host.store().snapshot();
        let catalogue = catalogue(host);
        Self {
            building: crate::background::Pending::start(
                move || read(&snapshot, &catalogue, publisher, state),
                wake,
            ),
        }
    }

    /// What it read, once it has; [`crate::background::Poll::Lost`] when the thread that read it
    /// ended without an answer.
    pub fn poll(&self) -> crate::background::Poll<Published> {
        self.building.poll()
    }
}

/// [`entries_of`] and the deltas that publish them, read from the snapshot `snapshot` and the
/// apps `catalogue` names.
#[must_use]
pub fn read(
    snapshot: &crate::store::Snapshot,
    catalogue: &[(String, String)],
    mut publisher: Publisher,
    state: IndexState,
) -> Published {
    let entries = entries_of(snapshot, catalogue);
    let kept = entries
        .iter()
        .map(|entry| (entry.resource.clone(), entry.clone()))
        .collect();
    let deltas = publisher
        .publish(state, entries)
        .iter()
        .filter_map(IndexDelta::encode)
        .collect();
    Published {
        publisher,
        entries: kept,
        deltas,
    }
}
