//! Notebooks written before Notes stored a [`Notebook`] record.
//!
//! Until the resource model, a notebook kept its notes at
//! `pages/<id>/{title, parent, order, body}` (`title` a Text, `parent` the
//! parent's id or `""`, `order` a fractional key), its tags at
//! `tags/<id>/name`, a note's tags as `tag_assignments/"<page>:<tag>" =
//! true`, and its pictures at `assets/<digest>` (the bytes, or `{len, mime}`
//! naming them in the content store), stamped `notes_schema_version = 3`.
//! A [`Notebook`] keeps its notes in a [`Collection`](ui::tilo_resource::Collection),
//! `pages/<id>/{k: order, v: note}`, so reading an old notebook as one finds
//! every note's id and nothing else: every note untitled, without a body,
//! and nothing an edit could land in. [`migrate`] rewrites such a notebook
//! in the record's layout once, when it opens; a notebook already in it is
//! left as it is.

use std::collections::{HashMap, HashSet};

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::tilo_resource::{Atom, Bytes, Doc, Fields, Id, ModelError};

use crate::notebook::{self, Notebook, Page, Tag};

/// The root key only an old notebook has.
const LEGACY_VERSION_KEY: &str = "notes_schema_version";

/// The root keys an old notebook keeps its data under, all replaced.
const LEGACY_KEYS: [&str; 5] = [
    LEGACY_VERSION_KEY,
    "pages",
    "tags",
    "tag_assignments",
    "assets",
];

/// Bring a notebook stored at version `from` up to the current one: an old
/// notebook is rewritten as a [`Notebook`] record with the same notes (ids,
/// titles, tree, order and bodies), tags and pictures; one already stored as
/// a record is only checked.
///
/// # Errors
/// Refuses another `from`, or an old notebook whose data does not read.
pub fn migrate(doc: &mut Doc, from: u32) -> Result<(), ModelError> {
    if from != 1 {
        return Err(ModelError::invalid(format!(
            "Notes has no migration from storage version {from}"
        )));
    }
    if doc.get_all(ROOT, LEGACY_VERSION_KEY)?.is_empty() {
        Notebook::read_fields(doc, &ROOT)?;
        return Ok(());
    }
    let notebook = read(doc)?;
    for key in LEGACY_KEYS {
        if doc.get(ROOT, key)?.is_some() {
            doc.delete(ROOT, key)?;
        }
    }
    notebook.write_fields(doc, &ROOT)
}

/// The notebook an old document holds.
fn read(doc: &Doc) -> Result<Notebook, ModelError> {
    let mut notebook = Notebook::default();
    let assigned = tag_assignments(doc)?;
    let tags = read_tags(doc, &mut notebook)?;
    if let Some(pages) = map(doc, &ROOT, "pages")? {
        let mut notes = Vec::new();
        for key in doc.keys(&pages) {
            let Some(page) = map(doc, &pages, &key)? else {
                continue;
            };
            notes.push((order(doc, &page)?, key, page));
        }
        // Siblings were shown by order key, then id.
        notes.sort();
        let ids: HashSet<&str> = notes.iter().map(|(_, key, _)| key.as_str()).collect();
        for (_, key, page) in &notes {
            let parent = string(doc, page, "parent")?;
            // A note whose parent is gone sits at the top, as it showed.
            let parent =
                (!parent.is_empty() && ids.contains(parent.as_str())).then(|| Id::from(parent));
            let mut note = Page {
                title: text(doc, page, "title")?,
                parent,
                ..Page::default()
            };
            if let Some(body) = map(doc, page, "body")? {
                let blocks = tilo_rich_text::blocks_under(doc, &body)
                    .map_err(|error| error.at("body").at(key))?;
                note.body = notebook::written(&blocks);
            }
            for tag in assigned.get(key.as_str()).into_iter().flatten() {
                if tags.contains(tag.as_str()) {
                    note.tags.insert(Id::from(tag.as_str()));
                }
            }
            notebook.pages.push(Id::from(key.as_str()), note);
        }
    }
    read_images(doc, &mut notebook)?;
    Ok(notebook)
}

/// The old notebook's tags, added to `notebook`; their ids.
fn read_tags(doc: &Doc, notebook: &mut Notebook) -> Result<HashSet<String>, ModelError> {
    let mut ids = HashSet::new();
    let Some(tags) = map(doc, &ROOT, "tags")? else {
        return Ok(ids);
    };
    let mut keys: Vec<String> = doc.keys(&tags).collect();
    keys.sort();
    for key in keys {
        let Some(tag) = map(doc, &tags, &key)? else {
            continue;
        };
        let name = text(doc, &tag, "name")?.trim().to_owned();
        if name.is_empty() {
            continue;
        }
        notebook
            .tags
            .push(Id::from(key.as_str()), Tag { name: Atom(name) });
        ids.insert(key);
    }
    Ok(ids)
}

/// Each note's tags, from the old `"<page>:<tag>" = true` entries.
fn tag_assignments(doc: &Doc) -> Result<HashMap<String, Vec<String>>, ModelError> {
    let mut assigned: HashMap<String, Vec<String>> = HashMap::new();
    let Some(assignments) = map(doc, &ROOT, "tag_assignments")? else {
        return Ok(assigned);
    };
    for key in doc.keys(&assignments) {
        let on = matches!(
            doc.get(&assignments, &key)?,
            Some((Value::Scalar(value), _)) if value.as_ref() == &ScalarValue::Boolean(true)
        );
        if let (true, Some((page, tag))) = (on, key.split_once(':')) {
            assigned
                .entry(page.to_owned())
                .or_default()
                .push(tag.to_owned());
        }
    }
    for tags in assigned.values_mut() {
        tags.sort();
    }
    Ok(assigned)
}

/// The old notebook's pictures, added to `notebook` under their digests: a
/// reference stays one; bytes kept in the document stay there when they are
/// small enough for a record, and move to the content store otherwise.
fn read_images(doc: &Doc, notebook: &mut Notebook) -> Result<(), ModelError> {
    let Some(assets) = map(doc, &ROOT, "assets")? else {
        return Ok(());
    };
    for digest in doc.keys(&assets) {
        let Ok(hash) = blake3::Hash::from_hex(&digest) else {
            continue;
        };
        let stored = match doc.get(&assets, &digest)? {
            Some((Value::Object(ObjType::Map), reference)) => Bytes::Blob {
                hash: *hash.as_bytes(),
                len: integer(doc, &reference, "len")?,
            },
            Some((Value::Scalar(value), _)) => match value.as_ref() {
                ScalarValue::Bytes(bytes) => {
                    let stored = Bytes::of(bytes);
                    if stored.needs_store()
                        && let Err(error) = notebook::content().put(bytes)
                    {
                        tracing::warn!(%digest, %error, "notes: an old notebook's picture was not kept");
                    }
                    stored
                }
                _ => continue,
            },
            _ => continue,
        };
        notebook.images.insert(digest, stored);
    }
    Ok(())
}

fn map(doc: &Doc, parent: &ObjId, key: &str) -> Result<Option<ObjId>, ModelError> {
    Ok(match doc.get(parent, key)? {
        Some((Value::Object(ObjType::Map), id)) => Some(id),
        _ => None,
    })
}

fn text(doc: &Doc, parent: &ObjId, key: &str) -> Result<String, ModelError> {
    Ok(match doc.get(parent, key)? {
        Some((Value::Object(ObjType::Text), id)) => doc.text(&id)?,
        Some((Value::Scalar(value), _)) => value.as_str().unwrap_or_default().to_owned(),
        _ => String::new(),
    })
}

fn string(doc: &Doc, parent: &ObjId, key: &str) -> Result<String, ModelError> {
    Ok(match doc.get(parent, key)? {
        Some((Value::Scalar(value), _)) => value.as_str().unwrap_or_default().to_owned(),
        _ => String::new(),
    })
}

fn integer(doc: &Doc, parent: &ObjId, key: &str) -> Result<u64, ModelError> {
    Ok(match doc.get(parent, key)? {
        Some((Value::Scalar(value), _)) => match value.as_ref() {
            ScalarValue::Uint(n) => *n,
            ScalarValue::Int(n) => u64::try_from(*n).unwrap_or_default(),
            _ => 0,
        },
        _ => 0,
    })
}

/// A note's order key; the oldest notebooks numbered their notes, which
/// sort as the fixed-width keys they were read as.
fn order(doc: &Doc, page: &ObjId) -> Result<String, ModelError> {
    Ok(match doc.get(page, "order")? {
        Some((Value::Scalar(value), _)) => match value.as_ref() {
            ScalarValue::Str(key) => key.to_string(),
            ScalarValue::Uint(n) => format!("i{n:08x}1"),
            ScalarValue::Int(n) => format!("i{:08x}1", u64::try_from(*n).unwrap_or_default()),
            _ => String::new(),
        },
        _ => String::new(),
    })
}
