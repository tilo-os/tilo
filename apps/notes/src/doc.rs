//! Automerge document model for the notes workspace.
//!
//! This replaces the Phase-1 "JSON-in-a-single-Text-blob" storage shim
//! (`storage::encode_workspace`) with a **live CRDT structure** so two
//! peers can edit the same workspace concurrently and converge:
//!
//! ```text
//! root
//! └── "pages" (Map, keyed by PageId string)
//!     └── <page-id> (Map)
//!         ├── "title"  (Text)   # collaborative title
//!         ├── "parent" (Str)    # PageId string, or "" for a root page
//!         ├── "order"  (Int)
//!         └── "blocks" (List)
//!             └── [i] (Map)
//!                 ├── "kind" (Str)   # paragraph / heading1.. / bullet / ...
//!                 └── "text" (Text)  # the block body (carries author marks)
//! ```
//!
//! Everything here operates on a borrowed `&mut AutoCommit` — the app's
//! shared `State::doc`. "Which page I'm viewing" is deliberately NOT in
//! this document: it is per-peer presence (see `NotesApp`), so two peers
//! can read different pages while sharing one workspace.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

use crate::model::{PageId, PageMeta};

/// Namespace for the workspace document helpers. Stateless — every
/// function takes the doc explicitly.
pub struct NotesDoc;

impl NotesDoc {
    /// Get-or-create the top-level `pages` Map.
    pub fn pages_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "pages") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "pages", ObjType::Map)
                .expect("put_object pages on root never fails"),
        }
    }

    /// Get-or-create the per-page Map for `id`. Returns the page Map's
    /// ObjId. Does not touch the page's fields.
    pub fn ensure_page(doc: &mut AutoCommit, id: PageId) -> ObjId {
        let pages = Self::pages_map(doc);
        let key = id.to_string();
        match doc.get(&pages, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), pid))) => pid,
            _ => doc
                .put_object(&pages, key.as_str(), ObjType::Map)
                .expect("put_object page map never fails"),
        }
    }

    /// Insert or update a page's metadata (title / parent / order).
    /// Title is stored as a `Text` object so concurrent renames merge;
    /// we only re-splice it when it actually changed to avoid churn.
    pub fn upsert_page_meta(doc: &mut AutoCommit, meta: &PageMeta) {
        let page = Self::ensure_page(doc, meta.id);

        // title (Text)
        let title_id = match doc.get(&page, "title") {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => doc
                .put_object(&page, "title", ObjType::Text)
                .expect("put_object title never fails"),
        };
        let cur = doc.text(&title_id).unwrap_or_default();
        if cur != meta.title {
            let cur_chars = cur.chars().count() as isize;
            let _ = doc.splice_text(&title_id, 0, cur_chars, &meta.title);
        }

        // parent (string; "" == root)
        let parent_str = meta.parent.map(|p| p.to_string()).unwrap_or_default();
        let _ = doc.put(&page, "parent", parent_str);

        // order (int)
        let _ = doc.put(&page, "order", meta.order as i64);
    }

    /// Read every page's metadata out of the document, sorted by
    /// `order`. Pages with an unparseable id key are skipped.
    pub fn list_pages(doc: &AutoCommit) -> Vec<PageMeta> {
        let pages = match doc.get(&ROOT, "pages") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Vec::new(),
        };

        let mut out = Vec::new();
        for key in doc.keys(&pages) {
            let Ok(id) = key.parse::<PageId>() else { continue };
            let Ok(Some((Value::Object(ObjType::Map), page))) =
                doc.get(&pages, key.as_str())
            else {
                continue;
            };

            let title = match doc.get(&page, "title") {
                Ok(Some((Value::Object(ObjType::Text), tid))) => {
                    doc.text(&tid).unwrap_or_default()
                }
                _ => String::new(),
            };

            let parent = match doc.get(&page, "parent") {
                Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                    automerge::ScalarValue::Str(s) if !s.is_empty() => {
                        s.parse::<PageId>().ok()
                    }
                    _ => None,
                },
                _ => None,
            };

            let order = match doc.get(&page, "order") {
                Ok(Some((Value::Scalar(v), _))) => {
                    v.to_i64().unwrap_or(0) as u32
                }
                _ => 0,
            };

            out.push(PageMeta { id, title, parent, order });
        }
        out.sort_by_key(|p| p.order);
        out
    }

    /// Get-or-create the `blocks` List for an existing page. Returns
    /// `None` if the page Map doesn't exist yet (caller must
    /// `upsert_page_meta` first).
    pub fn page_blocks_list(doc: &mut AutoCommit, id: PageId) -> Option<ObjId> {
        let pages = Self::pages_map(doc);
        let key = id.to_string();
        let page = match doc.get(&pages, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), pid))) => pid,
            _ => return None,
        };
        Some(match doc.get(&page, "blocks") {
            Ok(Some((Value::Object(ObjType::List), lid))) => lid,
            _ => doc
                .put_object(&page, "blocks", ObjType::List)
                .expect("put_object blocks never fails"),
        })
    }

    /// The `Text` ObjId for block `index` in a `blocks` List, if that
    /// block exists and has a Text body.
    pub fn block_text_obj(
        doc: &AutoCommit,
        blocks: &ObjId,
        index: usize,
    ) -> Option<ObjId> {
        let block = match doc.get(blocks, index) {
            Ok(Some((Value::Object(ObjType::Map), m))) => m,
            _ => return None,
        };
        match doc.get(&block, "text") {
            Ok(Some((Value::Object(ObjType::Text), t))) => Some(t),
            _ => None,
        }
    }

    /// The `kind` string for block `index`, defaulting to `"paragraph"`.
    pub fn block_kind(doc: &AutoCommit, blocks: &ObjId, index: usize) -> String {
        let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(blocks, index)
        else {
            return "paragraph".to_string();
        };
        match doc.get(&block, "kind") {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                automerge::ScalarValue::Str(s) => s.to_string(),
                _ => "paragraph".to_string(),
            },
            _ => "paragraph".to_string(),
        }
    }

    /// Append a new block `{ kind, text }` to a `blocks` List and return
    /// the new block's `text` ObjId (already seeded with `text`).
    pub fn push_block(
        doc: &mut AutoCommit,
        blocks: &ObjId,
        kind: &str,
        text: &str,
    ) -> ObjId {
        let index = doc.length(blocks);
        let block = doc
            .insert_object(blocks, index, ObjType::Map)
            .expect("insert block map never fails");
        let _ = doc.put(&block, "kind", kind);
        let text_id = doc
            .put_object(&block, "text", ObjType::Text)
            .expect("put_object block text never fails");
        if !text.is_empty() {
            let _ = doc.splice_text(&text_id, 0, 0, text);
        }
        text_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PageMeta;
    use uuid::Uuid;

    #[test]
    fn pages_round_trip() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta { id, title: "Hello".into(), parent: None, order: 0 },
        );
        let pages = NotesDoc::list_pages(&doc);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, id);
        assert_eq!(pages[0].title, "Hello");
        assert_eq!(pages[0].parent, None);
        assert_eq!(pages[0].order, 0);
    }

    #[test]
    fn parent_and_order_round_trip() {
        let mut doc = AutoCommit::new();
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta { id: root, title: "Root".into(), parent: None, order: 0 },
        );
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta { id: child, title: "Child".into(), parent: Some(root), order: 0 },
        );
        let pages = NotesDoc::list_pages(&doc);
        let c = pages.iter().find(|p| p.id == child).unwrap();
        assert_eq!(c.parent, Some(root));
    }

    #[test]
    fn rename_updates_in_place() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta { id, title: "Old".into(), parent: None, order: 0 },
        );
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta { id, title: "New".into(), parent: None, order: 0 },
        );
        let pages = NotesDoc::list_pages(&doc);
        assert_eq!(pages.len(), 1, "rename must not create a second page");
        assert_eq!(pages[0].title, "New");
    }

    #[test]
    fn two_docs_merge_pages() {
        // Models the real flow: peer A creates the workspace, peer B
        // joins by loading A's full state (the Arrived->SyncCRDT
        // handshake), THEN both edit and converge. Sharing a common base
        // is what makes the root `pages` Map the same object on both
        // sides — two independent `AutoCommit::new()` docs would each
        // create a competing `pages` Map and conflict, which is exactly
        // why late joiners always adopt remote state first.
        let mut a = AutoCommit::new();
        let ida = Uuid::new_v4();
        NotesDoc::upsert_page_meta(
            &mut a,
            &PageMeta { id: ida, title: "A".into(), parent: None, order: 0 },
        );

        // B joins from A's state.
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let idb = Uuid::new_v4();
        NotesDoc::upsert_page_meta(
            &mut b,
            &PageMeta { id: idb, title: "B".into(), parent: None, order: 1 },
        );

        // A pulls B's new page back.
        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();

        let titles: Vec<_> =
            NotesDoc::list_pages(&a).into_iter().map(|p| p.title).collect();
        assert!(titles.contains(&"A".to_string()), "converged doc has A: {titles:?}");
        assert!(titles.contains(&"B".to_string()), "converged doc has B: {titles:?}");
    }

    #[test]
    fn blocks_list_and_text_round_trip() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta { id, title: "P".into(), parent: None, order: 0 },
        );
        let blocks = NotesDoc::page_blocks_list(&mut doc, id).unwrap();
        let _t = NotesDoc::push_block(&mut doc, &blocks, "paragraph", "hi");
        assert_eq!(doc.length(&blocks), 1);
        assert_eq!(NotesDoc::block_kind(&doc, &blocks, 0), "paragraph");
        let tid = NotesDoc::block_text_obj(&doc, &blocks, 0).unwrap();
        assert_eq!(doc.text(&tid).unwrap(), "hi");
    }
}
