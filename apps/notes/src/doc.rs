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
use ui::doc::{put_if_changed, put_text_diffed};

use crate::model::{PageId, PageMeta};
use crate::num;

/// Namespace for the workspace document helpers. Stateless — every
/// function takes the doc explicitly.
pub struct NotesDoc;

impl NotesDoc {
    /// Get-or-create the top-level `pages` Map.
    ///
    /// # Panics
    /// Never in practice: `put_object` on `ROOT` only fails for an invalid or
    /// deleted parent object, and `ROOT` is permanent for the life of the doc.
    #[allow(
        clippy::expect_used,
        reason = "ROOT is Automerge's permanent, undeletable root Map, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
    pub fn pages_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "pages") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "pages", ObjType::Map)
                .expect("put_object pages on root never fails"),
        }
    }

    /// Get-or-create the per-page Map for `id`. Returns the page Map's
    /// `ObjId`. Does not touch the page's fields.
    ///
    /// # Panics
    /// Never in practice: `pages` was just fetched-or-created above, so
    /// `put_object` on it only fails for an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "pages is a Map this call itself just fetched-or-created via pages_map, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
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

        // title (Text): diff-splice only the changed span so a rename doesn't
        // rewrite (and tombstone) the whole title every upsert.
        put_text_diffed(doc, &page, "title", &meta.title);

        // parent (string; "" == root) — guarded so an unchanged upsert is a no-op.
        let parent_str = meta.parent.map(|p| p.to_string()).unwrap_or_default();
        put_if_changed(doc, &page, "parent", parent_str);

        // order (int)
        put_if_changed(doc, &page, "order", i64::from(meta.order));
    }

    /// Remove a page's entry from the shared doc. Called when a page is
    /// deleted locally so `sync`'s pull-loop (which treats any doc page
    /// missing locally as a remote create) doesn't re-add it next frame.
    pub fn remove_page(doc: &mut AutoCommit, id: PageId) {
        let pages = Self::pages_map(doc);
        let _ = doc.delete(&pages, id.to_string().as_str());
    }

    /// Read every page's metadata out of the document, sorted by
    /// `order`. Pages with an unparseable id key are skipped.
    pub fn list_pages(doc: &AutoCommit) -> Vec<PageMeta> {
        let Ok(Some((Value::Object(ObjType::Map), pages))) = doc.get(&ROOT, "pages") else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for key in doc.keys(&pages) {
            let Ok(id) = key.parse::<PageId>() else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), page))) = doc.get(&pages, key.as_str())
            else {
                continue;
            };

            let title = match doc.get(&page, "title") {
                Ok(Some((Value::Object(ObjType::Text), tid))) => doc.text(&tid).unwrap_or_default(),
                _ => String::new(),
            };

            let parent = match doc.get(&page, "parent") {
                Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                    automerge::ScalarValue::Str(s) if !s.is_empty() => s.parse::<PageId>().ok(),
                    _ => None,
                },
                _ => None,
            };

            let order = match doc.get(&page, "order") {
                Ok(Some((Value::Scalar(v), _))) => num::u32_of_i64(v.to_i64().unwrap_or(0)),
                _ => 0,
            };

            out.push(PageMeta {
                id,
                title,
                parent,
                order,
            });
        }
        out.sort_by_key(|p| p.order);
        out
    }

    /// Get-or-create the `blocks` List for an existing page. Returns
    /// `None` if the page Map doesn't exist yet (caller must
    /// `upsert_page_meta` first).
    ///
    /// # Panics
    /// Never in practice: `page` was just fetched above (and matched as a
    /// Map), so `put_object` on it only fails for an already-corrupted
    /// document.
    #[allow(
        clippy::expect_used,
        reason = "page was just fetched-and-matched as a Map above, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
    pub fn page_blocks_list(doc: &mut AutoCommit, id: PageId) -> Option<ObjId> {
        let pages = Self::pages_map(doc);
        let key = id.to_string();
        let Ok(Some((Value::Object(ObjType::Map), page))) = doc.get(&pages, key.as_str()) else {
            return None;
        };
        Some(match doc.get(&page, "blocks") {
            Ok(Some((Value::Object(ObjType::List), lid))) => lid,
            _ => doc
                .put_object(&page, "blocks", ObjType::List)
                .expect("put_object blocks never fails"),
        })
    }

    /// The `Text` `ObjId` for block `index` in a `blocks` List, if that
    /// block exists and has a Text body.
    pub fn block_text_obj(doc: &AutoCommit, blocks: &ObjId, index: usize) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(blocks, index) else {
            return None;
        };
        match doc.get(&block, "text") {
            Ok(Some((Value::Object(ObjType::Text), t))) => Some(t),
            _ => None,
        }
    }

    /// The `kind` string for block `index`, defaulting to `"paragraph"`.
    pub fn block_kind(doc: &AutoCommit, blocks: &ObjId, index: usize) -> String {
        let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(blocks, index) else {
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
    /// the new block's `text` `ObjId` (already seeded with `text`).
    ///
    /// # Panics
    /// Never in practice: `blocks` and the freshly-inserted block map are
    /// live objects this call itself just created, so these only fail for
    /// an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "blocks is caller-supplied but always a live List, and the block map was just inserted by this call, so these cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
    pub fn push_block(doc: &mut AutoCommit, blocks: &ObjId, kind: &str, text: &str) -> ObjId {
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
            &PageMeta {
                id,
                title: "Hello".into(),
                parent: None,
                order: 0,
            },
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
            &PageMeta {
                id: root,
                title: "Root".into(),
                parent: None,
                order: 0,
            },
        );
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id: child,
                title: "Child".into(),
                parent: Some(root),
                order: 0,
            },
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
            &PageMeta {
                id,
                title: "Old".into(),
                parent: None,
                order: 0,
            },
        );
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id,
                title: "New".into(),
                parent: None,
                order: 0,
            },
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
            &PageMeta {
                id: ida,
                title: "A".into(),
                parent: None,
                order: 0,
            },
        );

        // B joins from A's state.
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let idb = Uuid::new_v4();
        NotesDoc::upsert_page_meta(
            &mut b,
            &PageMeta {
                id: idb,
                title: "B".into(),
                parent: None,
                order: 1,
            },
        );

        // A pulls B's new page back.
        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();

        let titles: Vec<_> = NotesDoc::list_pages(&a)
            .into_iter()
            .map(|p| p.title)
            .collect();
        assert!(
            titles.contains(&"A".to_string()),
            "converged doc has A: {titles:?}"
        );
        assert!(
            titles.contains(&"B".to_string()),
            "converged doc has B: {titles:?}"
        );
    }

    #[test]
    fn renaming_by_one_char_is_a_bounded_splice() {
        // A long title renamed by one char must diff-splice (grow the doc a
        // little), not delete+reinsert the whole title (O(len) tombstones per
        // edit — the churn that freezes the app).
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        let long = "A very long note title that would be expensive to rewrite each keystroke";
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id,
                title: long.into(),
                parent: None,
                order: 0,
            },
        );
        let before = doc.save().len();
        let mut longer = long.to_string();
        longer.push('!');
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id,
                title: longer.clone(),
                parent: None,
                order: 0,
            },
        );
        let grew = doc.save().len() - before;
        assert_eq!(NotesDoc::list_pages(&doc)[0].title, longer);
        assert!(
            grew < 40,
            "appending one char to a title grew the doc by {grew} bytes (expected a single-char splice)"
        );
    }

    #[test]
    fn idempotent_upsert_does_not_grow_the_doc() {
        // Re-storing an unchanged page (title + parent + order) must be a no-op:
        // change-guarded scalars + diff-spliced title mean zero new ops.
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        let meta = PageMeta {
            id,
            title: "Stable".into(),
            parent: None,
            order: 3,
        };
        NotesDoc::upsert_page_meta(&mut doc, &meta);
        let before = doc.save().len();
        for _ in 0..25 {
            NotesDoc::upsert_page_meta(&mut doc, &meta);
        }
        assert_eq!(
            doc.save().len(),
            before,
            "idempotent upsert must not grow the document"
        );
    }

    #[test]
    fn blocks_list_and_text_round_trip() {
        let mut doc = AutoCommit::new();
        let id = Uuid::new_v4();
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id,
                title: "P".into(),
                parent: None,
                order: 0,
            },
        );
        let blocks = NotesDoc::page_blocks_list(&mut doc, id).unwrap();
        let _t = NotesDoc::push_block(&mut doc, &blocks, "paragraph", "hi");
        assert_eq!(doc.length(&blocks), 1);
        assert_eq!(NotesDoc::block_kind(&doc, &blocks, 0), "paragraph");
        let tid = NotesDoc::block_text_obj(&doc, &blocks, 0).unwrap();
        assert_eq!(doc.text(&tid).unwrap(), "hi");
    }
}
