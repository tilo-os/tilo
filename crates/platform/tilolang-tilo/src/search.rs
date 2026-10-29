//! What search finds of a room: the text of every record and item of every package.
//!
//! Every `Text` and `RichText` field of a type is searchable by default (a rich text by its text,
//! where a reference reads as its run: the referenced resource's title, the person's name); a hit is the record or item that holds
//! it, found by its text, and it opens the host at that resource (its `open` view action, see
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
/// whole: what the index's hits are held to.
#[must_use]
pub fn hits(meta: &Meta, record: &DynRecord, doc: &str) -> Vec<Hit> {
    let mut out = Vec::new();
    collect(meta, record, doc, &mut out);
    out
}

fn collect(meta: &Meta, record: &DynRecord, path: &str, out: &mut Vec<Hit>) {
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
                rich.text()
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

/// Every hit of the room, by package and path, read from the indexes now: what a window
/// publishes to the session's search index, which is what search finds things through. One pass
/// over each package's index.
#[must_use]
pub fn every_hit(host: &Host) -> Vec<Hit> {
    let snapshot = host.store().snapshot();
    let mut out = Vec::new();
    for package in host.packages() {
        let Some(view) = snapshot.view(&package.meta.app) else {
            continue;
        };
        let Ok(texted) = view.every_text() else {
            continue;
        };
        out.extend(texted.into_iter().filter_map(|held| {
            Some(Hit {
                resource: resource_id(&package.meta.app, &held.path),
                app_title: package.meta.title.clone(),
                type_name: held.schema.name.clone(),
                title: held.texts.first()?.clone(),
                texts: held.texts,
                references: held.references,
            })
        }));
    }
    out
}
