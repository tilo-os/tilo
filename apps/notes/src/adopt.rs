//! Bringing the notebooks of earlier Notes windows into the room's notebook.
//!
//! Notes used to keep a notebook per window: each window the launcher opened
//! got a record of its own (`derived:default@tab:<window>`), so a new window
//! showed an empty notebook and the pages written in another were reachable
//! only by restoring that exact window. Notes now keeps one notebook per
//! room ([`ResourceApp::ROOM_RECORD`](ui::framework::ResourceApp::ROOM_RECORD)).
//! When a room's notebook is first created, its seed copies the pages of
//! every per-window notebook this device holds in the room into it, through
//! one [`NotesAction::AdoptWindowNotebooks`].
//!
//! The copy is deterministic and idempotent: notebooks are taken in record
//! id order, pages keep their ids (so links between them still resolve),
//! and the notebook records which window notebooks it adopted. Applying the
//! same adoption again, or adopting a notebook whose pages are already
//! there, adds nothing. The per-window records are only read; they stay on
//! disk as they were, so the migration can be undone by hand.
//!
//! Why a copy and not [`RecordCheckout::formerly`](ui::record::RecordCheckout::formerly):
//! a former identity hands the new record *one* earlier document, history
//! and all, and claims it so no other successor gets it. A room usually has
//! several window notebooks with pages (each window someone wrote in), and
//! independent Automerge documents cannot be merged into one: each created
//! its own `pages` map, so merging would keep one window's pages and hide
//! the others' as conflicts. Copying the pages, by id, keeps all of them;
//! what is lost is their per-keystroke history, which stays readable in the
//! untouched old records.

use std::collections::HashSet;

use automerge::{
    AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use serde::{Deserialize, Serialize};

use crate::actions::{NotesAction, PageBlock, TagData};
use crate::doc::NotesDoc;
use crate::model::{PageId, PageMeta, TagId};
use crate::notebook::StoredAsset;

/// Root map of the window notebooks this notebook adopted: record id → true.
pub const ADOPTED_WINDOW_NOTEBOOKS: &str = "adopted_window_notebooks";

/// One earlier window's notebook, as an adoption carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptedNotebook {
    /// The window's record id (`derived:default@tab:…`).
    pub record: String,
    /// Every page, each after its parent, siblings in their order.
    pub pages: Vec<AdoptedPage>,
    pub tags: Vec<TagData>,
    /// `(page, tag)` memberships.
    pub assignments: Vec<(String, String)>,
    pub assets: Vec<AdoptedAsset>,
}

/// One page of an adopted notebook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptedPage {
    pub id: String,
    pub title: String,
    /// Parent page id, or `""` for a root page.
    pub parent: String,
    /// Its order among its siblings in the old notebook. A root page is
    /// placed after the room notebook's own roots instead.
    pub order: String,
    pub blocks: Vec<PageBlock>,
}

/// One image of an adopted notebook: its bytes, or the reference to bytes
/// already in this device's content store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdoptedAsset {
    Inline {
        digest: String,
        bytes: Vec<u8>,
    },
    Reference {
        digest: String,
        len: u64,
        mime: String,
    },
}

/// The adoption of `records` (a room's window notebooks, as
/// [`SeedCx::window_records`](ui::framework::SeedCx::window_records) reads
/// them), or `None` when none holds a page or a tag. A notebook that does
/// not read as a Notes notebook is left out and logged.
pub fn adoption_action(records: &[(ui::record::RecordId, AutoCommit)]) -> Option<NotesAction> {
    let notebooks = records
        .iter()
        .filter_map(|(id, doc)| match adopted_notebook(id.as_str(), doc) {
            Ok(notebook) => notebook,
            Err(error) => {
                tracing::warn!(record = %id, %error, "notes: not adopting an unreadable window notebook");
                None
            }
        })
        .collect::<Vec<_>>();
    (!notebooks.is_empty()).then_some(NotesAction::AdoptWindowNotebooks { notebooks })
}

/// What `doc`, window record `record`'s notebook, holds; `None` when empty.
///
/// # Errors
/// A document that is not a valid Notes notebook.
pub fn adopted_notebook(record: &str, doc: &AutoCommit) -> Result<Option<AdoptedNotebook>, String> {
    let workspace = crate::notebook::project(doc, None)?;
    if workspace.pages().is_empty() && workspace.tags().is_empty() {
        return Ok(None);
    }
    let mut pages = Vec::new();
    let mut pending = vec![None];
    while let Some(parent) = pending.pop() {
        let children = workspace.children_of(parent);
        // Depth-first, parents first; children pushed reversed so they come
        // out in their order.
        for child in children.iter().rev() {
            pending.push(Some(child.id));
        }
        for child in children {
            pages.push(AdoptedPage {
                id: child.id.to_string(),
                title: child.title.clone(),
                parent: child.parent.map(|p| p.to_string()).unwrap_or_default(),
                order: child.order.clone(),
                blocks: NotesDoc::page_blocks(doc, child.id)
                    .iter()
                    .map(PageBlock::of)
                    .collect(),
            });
        }
    }
    let mut assignments = workspace
        .data
        .tag_assignments
        .iter()
        .map(|edge| (edge.page.to_string(), edge.tag.to_string()))
        .collect::<Vec<_>>();
    assignments.sort();
    let mut tags = workspace
        .tags()
        .iter()
        .map(TagData::from_meta)
        .collect::<Vec<_>>();
    tags.sort_by(|a, b| a.id.cmp(&b.id));
    let assets = crate::notebook::stored_assets(doc)?
        .into_iter()
        .map(|(digest, asset)| match asset {
            StoredAsset::Inline(bytes) => AdoptedAsset::Inline { digest, bytes },
            StoredAsset::Reference { len, mime } => AdoptedAsset::Reference { digest, len, mime },
        })
        .collect();
    Ok(Some(AdoptedNotebook {
        record: record.to_owned(),
        pages,
        tags,
        assignments,
        assets,
    }))
}

/// Whether this notebook already adopted window notebook `record`.
pub fn has_adopted(doc: &AutoCommit, record: &str) -> bool {
    doc.get_all(ROOT, ADOPTED_WINDOW_NOTEBOOKS)
        .unwrap_or_default()
        .into_iter()
        .any(|(value, map)| {
            value == Value::Object(ObjType::Map) && matches!(doc.get(&map, record), Ok(Some(_)))
        })
}

/// Apply [`NotesAction::AdoptWindowNotebooks`]: each notebook not adopted
/// yet, in the order given. A page whose id the notebook already has is
/// kept as it is; so is a tag. Root pages go after the notebook's roots.
/// Lenient by design: this runs while the room's notebook is created, so a
/// part of an old notebook that no longer validates (a page id, a mark, an
/// image) is left out rather than failing the notebook everyone opens.
///
/// # Errors
/// Only a document write failure.
pub fn adopt(doc: &mut AutoCommit, notebooks: &[AdoptedNotebook]) -> Result<(), String> {
    for notebook in notebooks {
        if has_adopted(doc, &notebook.record) {
            continue;
        }
        adopt_one(doc, notebook)?;
        mark_adopted(doc, &notebook.record)?;
    }
    Ok(())
}

fn adopt_one(doc: &mut AutoCommit, notebook: &AdoptedNotebook) -> Result<(), String> {
    for asset in &notebook.assets {
        let (digest, stored) = match asset {
            AdoptedAsset::Inline { digest, bytes } => (digest, StoredAsset::Inline(bytes.clone())),
            AdoptedAsset::Reference { digest, len, mime } => (
                digest,
                StoredAsset::Reference {
                    len: *len,
                    mime: mime.clone(),
                },
            ),
        };
        if let Err(error) = crate::notebook::adopt_asset(doc, digest, &stored) {
            tracing::warn!(record = %notebook.record, %digest, %error, "notes: an adopted image was left out");
        }
    }
    let mut existing = NotesDoc::list_pages(doc);
    let mut adopted = HashSet::new();
    for page in &notebook.pages {
        let Some(id) = page.id.parse::<PageId>().ok().filter(|id| !id.is_nil()) else {
            tracing::warn!(record = %notebook.record, page = %page.id, "notes: an adopted page id is invalid");
            continue;
        };
        if existing.iter().any(|known| known.id == id) {
            continue;
        }
        // A page whose parent is not in the notebook (it was left out)
        // becomes a root rather than dangling.
        let parent = page
            .parent
            .parse::<PageId>()
            .ok()
            .filter(|parent| existing.iter().any(|known| known.id == *parent));
        let order = match parent {
            Some(_) if !page.order.is_empty() => page.order.clone(),
            _ => crate::model::key_after(existing.iter().filter(|known| known.parent == parent)),
        };
        let blocks = page.blocks.iter().map(lenient_block).collect::<Vec<_>>();
        let meta = PageMeta {
            id,
            title: page.title.clone(),
            parent,
            order,
        };
        NotesDoc::upsert_page_meta(doc, &meta);
        NotesDoc::fill_page_body(doc, id, &blocks)?;
        existing.push(meta);
        adopted.insert(id);
    }
    let mut tags = NotesDoc::list_tags(doc)
        .iter()
        .map(|tag| tag.id)
        .collect::<HashSet<TagId>>();
    for tag in &notebook.tags {
        let Some(meta) = tag.to_meta() else {
            continue;
        };
        if tags.insert(meta.id) {
            NotesDoc::upsert_tag(doc, &meta);
        }
    }
    for (page, tag) in &notebook.assignments {
        let (Ok(page), Ok(tag)) = (page.parse::<PageId>(), tag.parse::<TagId>()) else {
            continue;
        };
        if adopted.contains(&page) && tags.contains(&tag) {
            NotesDoc::set_tag_assignment(doc, page, tag, true);
        }
    }
    Ok(())
}

/// The block as it was; without its marks if one no longer validates; as
/// plain text if its kind does not either.
fn lenient_block(block: &PageBlock) -> ui::widget::BlockContent {
    block
        .content()
        .or_else(|| {
            PageBlock {
                marks: Vec::new(),
                ..block.clone()
            }
            .content()
        })
        .unwrap_or_else(|| ui::widget::BlockContent::paragraph("", block.text.clone()))
}

fn mark_adopted(doc: &mut AutoCommit, record: &str) -> Result<(), String> {
    let map = match doc
        .get_all(ROOT, ADOPTED_WINDOW_NOTEBOOKS)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|(value, _)| *value == Value::Object(ObjType::Map))
    {
        Some((_, map)) => map,
        None => doc
            .put_object(ROOT, ADOPTED_WINDOW_NOTEBOOKS, ObjType::Map)
            .map_err(|e| e.to_string())?,
    };
    doc.put(&map, record, ScalarValue::Boolean(true))
        .map_err(|e| e.to_string())
}
