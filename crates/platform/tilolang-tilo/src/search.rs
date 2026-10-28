//! What search finds of a room: the text of every record and item of every package.
//!
//! Every `Text` field of a type is searchable by default; a hit is the record or item that holds
//! it, found by its text, and it opens the host at that resource (its `open` view action, see
//! [`crate::effects::OpenEffect`]). A package will be able to say which fields to index; until
//! then all of them are, the most useful default for a notes app and a chat alike.
//!
//! Search keeps nothing of its own: it reads each package's index ([`crate::index`]), the one
//! the store's lists read, whose search index (Tilo's search layer, `tilo-search`) finds
//! resources by their texts as every Tilo surface finds things, and whose fields give a hit its
//! texts. Listing every hit (what a system catalogue takes in) reads the same search index.

use tilolang_runtime::meta::Meta;

use crate::host::Host;
use crate::index::search::Found;
use crate::index::view::View;
use crate::publish::resource_id;
use crate::record::{DynRecord, DynValue};
use crate::schema::lookup;

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

/// The searchable resources of the record `doc` and the items below it, read from the record
/// whole: what the index's hits are held to.
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

/// The hit of the resource at `path` of the package `meta`, as `view` holds it: none when it
/// has no text.
fn hit_at(view: &View<'_>, meta: &Meta, path: &str, ty: &str) -> Option<Hit> {
    let schema = lookup(ty)?;
    let texts = view.texts(path, schema).ok()?;
    let title = texts.first()?.clone();
    Some(Hit {
        resource: resource_id(&meta.app, path),
        app_title: meta.title.clone(),
        type_name: schema.name.clone(),
        title,
        texts,
    })
}

/// Every hit of the room, by package and path, read from the indexes now: what a catalogue
/// that keeps its own copy takes in.
#[must_use]
pub fn every_hit(host: &Host) -> Vec<Hit> {
    let snapshot = host.store().snapshot();
    let mut out = Vec::new();
    for package in host.packages() {
        let Some(view) = snapshot.view(&package.meta.app) else {
            continue;
        };
        let Ok(found) = view.snapshot.search().resources() else {
            continue;
        };
        for path in found {
            let Some(ty) = view.type_of(&path).ok().flatten() else {
                continue;
            };
            out.extend(hit_at(&view, &package.meta, &path, &ty));
        }
    }
    out
}

/// The resources whose texts hold `query`, folded as everywhere in Tilo: inside a word or at its
/// start, its words spread over the texts, the last as typed so far. The best first: a match in
/// what names a resource ranks above one in its other texts. The first `limit` of them, read
/// from the indexes' search indexes.
#[must_use]
pub fn find(host: &Host, query: &str, limit: usize) -> Vec<Hit> {
    let snapshot = host.store().snapshot();
    let mut found: Vec<(i32, Hit)> = Vec::new();
    for package in host.packages() {
        let Some(view) = snapshot.view(&package.meta.app) else {
            continue;
        };
        let Ok(hits) = view.snapshot.search().find(query, limit) else {
            continue;
        };
        for Found { resource, rank } in hits {
            let Some(ty) = view.type_of(&resource).ok().flatten() else {
                continue;
            };
            if let Some(hit) = hit_at(&view, &package.meta, &resource, &ty) {
                found.push((rank, hit));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.resource.cmp(&b.1.resource)));
    found.truncate(limit);
    found.into_iter().map(|(_, hit)| hit).collect()
}
