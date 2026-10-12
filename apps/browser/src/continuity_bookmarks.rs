//! Private observed-remove bookmark import ledger. The shared typed Browser is unchanged.
//!
//! Navigation and the visit log remain local. Bookmarks and preferences are
//! durable decisions, so every visible control, Ctrl+K and agent uses this
//! deterministic document-action path.

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ROOT, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionError, Doc, ExecutionLocus};

use crate::continuity_library::{Bookmark, Bookmarks};

const ADD_PREFIX: &str = "browser.bookmarks.v2.add:";
const REMOVE_PREFIX: &str = "browser.bookmarks.v2.remove:";
const URL: &str = "url";
const TITLE: &str = "title";
const MAX_BOOKMARK_TITLE_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersonalLibraryEdit {
    /// `tag` is minted when the action descriptor is made, never during
    /// replay. This makes the per-bookmark observed-remove set merge-safe.
    AddBookmark {
        url: String,
        title: String,
        tag: String,
    },
    /// Only observed tags are tombstoned, so a concurrent add is never lost.
    RemoveBookmark {
        url: String,
        observed_tags: Vec<String>,
    },
    /// Add a reviewed library snapshot without replacing existing bookmarks.
    ImportBookmarks {
        batch: crate::continuity::ImportBatch,
    },
    RemoveImportedNode {
        source: [u8; 32],
        id: String,
    },
    ImportBookmarkBundle {
        bundle: crate::continuity::ImportBundle,
    },
}

pub fn normalized_bookmark_url(url: &str) -> Option<String> {
    crate::actions::normalized_bookmark_url(url)
}

/// Adds and tombstones live directly below the shared V2 map. Tags are
/// globally unique per add, so this avoids a contested per-URL map object
/// when two replicas bookmark the same new URL concurrently.
fn add_key(tag: &str) -> String {
    format!("{ADD_PREFIX}{tag}")
}

fn remove_key(tag: &str) -> String {
    format!("{REMOVE_PREFIX}{tag}")
}

fn put_add(
    doc: &mut Doc,
    root: &ObjId,
    tag: &str,
    url: &str,
    title: &str,
) -> Result<(), ActionError> {
    let key = add_key(tag);
    if doc.get(root, &key)?.is_none() {
        let add = doc.put_object(root, key, ObjType::Map)?;
        doc.put(&add, URL, url)?;
        doc.put(&add, TITLE, title)?;
    }
    Ok(())
}

fn read_string(doc: &Doc, parent: &ObjId, key: &str) -> Option<String> {
    doc.get(parent, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_scalar().ok())
        .and_then(|value| value.as_str().map(str::to_string))
}

/// Project the CRDT OR-set into the compact view model used by the chrome.
/// `None` means the document holds no bookmark yet, so callers may retain the
/// local cache until the first bookmark action.
pub fn bookmarks_from_doc(doc: &Doc) -> Option<Bookmarks> {
    if !doc.keys(&ROOT).any(|key| key.starts_with(ADD_PREFIX)) {
        return None;
    }
    let mut rows = Vec::new();
    for key in doc.keys(&ROOT) {
        let Some(tag) = key.strip_prefix(ADD_PREFIX) else {
            continue;
        };
        let tag = tag.to_string();
        if doc.get(&ROOT, remove_key(&tag)).ok().flatten().is_some() {
            continue;
        }
        let Some((Value::Object(ObjType::Map), add)) = doc.get(&ROOT, key).ok().flatten() else {
            continue;
        };
        let Some(url) = read_string(doc, &add, URL) else {
            continue;
        };
        let Some(title) = read_string(doc, &add, TITLE) else {
            continue;
        };
        rows.push(Bookmark {
            url,
            title,
            tags: vec![tag],
        });
    }
    rows.sort_by(|a, b| {
        let imported = |row: &Bookmark| {
            row.tags
                .first()
                .is_some_and(|tag| tag.starts_with("import:"))
        };
        a.url
            .cmp(&b.url)
            .then_with(|| imported(a).cmp(&imported(b)))
            .then_with(|| a.title.cmp(&b.title))
    });
    let mut merged: Vec<Bookmark> = Vec::new();
    for row in rows {
        if let Some(last) = merged.last_mut()
            && last.url == row.url
        {
            last.tags.extend(row.tags);
            last.tags.sort();
            last.tags.dedup();
        } else {
            merged.push(row);
        }
    }
    rows = merged;
    rows.truncate(crate::continuity::MAX_BUNDLE_ENTRIES);
    Some(Bookmarks::from_entries(rows))
}

fn import_bookmark_bundle(
    doc: &mut Doc,
    bundle: &crate::continuity::ImportBundle,
) -> Result<(), ActionError> {
    let current = doc
        .keys(&ROOT)
        .filter(|k| k.starts_with(ADD_PREFIX))
        .count();
    crate::continuity::preflight_bundle(doc, "bookmarks", bundle, current)?;
    if let Some(tree) = &bundle.imported_tree {
        tree.preflight(doc)?;
    }
    for chunk in &bundle.chunks {
        import_bookmarks(doc, chunk)?;
    }
    if let Some(tree) = &bundle.imported_tree {
        tree.apply(doc)?;
    }
    Ok(())
}

fn import_bookmarks(
    doc: &mut Doc,
    batch: &crate::continuity::ImportBatch,
) -> Result<(), ActionError> {
    if !batch.history.is_empty() {
        return Err(ActionError::Refused(
            "History belongs to its private history record".into(),
        ));
    }
    let Some((key, digest)) = crate::continuity::receipt(doc, "bookmarks", batch)? else {
        return Ok(());
    };
    let current = doc
        .keys(&ROOT)
        .filter(|k| k.starts_with(ADD_PREFIX))
        .count();
    if current.saturating_add(batch.bookmarks.len()) > crate::continuity::MAX_BUNDLE_ENTRIES {
        return Err(ActionError::Refused("Too many imported bookmarks".into()));
    }
    for row in &batch.bookmarks {
        put_add(
            doc,
            &ROOT,
            &crate::continuity::tag(&batch.source, &row.url),
            &row.url,
            &row.title,
        )?;
    }
    doc.put(&ROOT, key, digest)?;

    Ok(())
}

impl Action for PersonalLibraryEdit {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::RemoveImportedNode { source, id } => {
                crate::imported_folders::remove(doc, source, id)?;
            }
            Self::ImportBookmarkBundle { bundle } => import_bookmark_bundle(doc, bundle)?,
            Self::ImportBookmarks { batch } => import_bookmarks(doc, batch)?,
            Self::AddBookmark { url, title, tag } => {
                let url = normalized_bookmark_url(url).ok_or_else(|| {
                    ActionError::Refused("bookmark must be an absolute http(s) URL".into())
                })?;
                if tag.trim().is_empty() {
                    return Err(ActionError::Refused("bookmark tag is required".into()));
                }
                if tag.len() > 256 || title.len() > MAX_BOOKMARK_TITLE_BYTES {
                    return Err(ActionError::Refused(
                        "bookmark metadata exceeds the safety limit".into(),
                    ));
                }
                let title = if title.trim().is_empty() {
                    url.clone()
                } else {
                    title.trim().to_string()
                };
                put_add(doc, &ROOT, tag, &url, &title)?;
            }
            Self::RemoveBookmark { url, observed_tags } => {
                normalized_bookmark_url(url).ok_or_else(|| {
                    ActionError::Refused("bookmark must be an absolute http(s) URL".into())
                })?;
                for tag in observed_tags.iter().filter(|tag| !tag.trim().is_empty()) {
                    doc.put(&ROOT, remove_key(tag), true)?;
                }
            }
        }
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}
