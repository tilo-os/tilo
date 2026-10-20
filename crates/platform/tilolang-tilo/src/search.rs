//! What search finds of a room: the text of every record and item of every package.
//!
//! Every `Text` field of a type is searchable by default; a hit is the record or item that holds
//! it, found by its text, and it opens the host at that resource (its `open` view action, see
//! [`crate::effects::OpenEffect`]). A package will be able to say which fields to index; until
//! then all of them are, the most useful default for a notes app and a chat alike.

use tilolang_runtime::meta::Meta;

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
