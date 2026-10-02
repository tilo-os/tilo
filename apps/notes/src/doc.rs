//! Automerge document model for the notes workspace: a **live CRDT
//! structure** two peers edit concurrently and converge on.
//!
//! ```text
//! root
//! └── "pages" (Map, keyed by PageId string)
//!     └── <page-id> (Map)
//!         ├── "title"  (Text)   # collaborative title
//!         ├── "parent" (Str)    # PageId string, or "" for a root page
//!         ├── "order"  (Int)
//!         └── "body"   (Map)    # qdoc rich text, edited in the SDK editor
//! ```
//!
//! Everything here operates on a borrowed `&mut AutoCommit` — the app's
//! shared `State::doc`. "Which page I'm viewing" is deliberately NOT in
//! this document: it is per-peer presence (see `NotesApp`), so two peers
//! can read different pages while sharing one workspace.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use tilo_document::{RichText, RichTextAction};
use ui::doc::{put_if_changed, put_text_diffed};
use ui::widget::BlockContent;
use ui::widget::marks::Mark;

use crate::actions::{InlineMark, InlineMarkSpan};
use crate::model::{PageId, PageMeta, TagId, TagMeta};
use crate::num;

/// Namespace for the workspace document helpers. Stateless — every
/// function takes the doc explicitly.
pub struct NotesDoc;

impl NotesDoc {
    /// Copy a canonical subtree after validating the complete authored mapping.
    ///
    /// # Errors
    /// Refuses stale mappings or reused/malformed identities before mutation.
    pub fn duplicate_subtree(
        doc: &mut AutoCommit,
        source: PageId,
        pages: &[crate::actions::DuplicatePage],
    ) -> Result<(), String> {
        let (workspace, copies) = prepare_duplicate(doc, source, pages)?;
        let current = Self::list_pages(doc);
        let mut metadata = workspace.data.pages;
        metadata.sort_by_key(|page| page.id);
        for page in metadata {
            if !current.iter().any(|old| old == &page) {
                Self::upsert_page_meta(doc, &page);
            }
        }
        for copy in copies {
            Self::fill_page_body(doc, copy.page, &copy.blocks)?;
            for tag in copy.tags {
                Self::set_tag_assignment(doc, copy.page, tag, true);
            }
        }
        Ok(())
    }

    /// Tags are an independent keyed map. A page/tag association is another
    /// keyed map entry (`page:tag`), so peers changing unrelated memberships
    /// never contend on one aggregate array.
    ///
    /// # Panics
    ///
    /// Never in normal use: `ROOT` is permanent. A panic would indicate a
    /// corrupt Automerge document that cannot create a root-owned map.
    #[allow(
        clippy::expect_used,
        reason = "ROOT is permanent, so a fresh tags map cannot fail absent document corruption"
    )]
    pub fn tags_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "tags") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "tags", ObjType::Map)
                .expect("put_object tags on root never fails"),
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "ROOT is permanent, so a fresh tag-assignment map cannot fail absent document corruption"
    )]
    /// # Panics
    ///
    /// Never in normal use: `ROOT` is permanent. A panic would indicate a
    /// corrupt Automerge document that cannot create a root-owned map.
    pub fn assignments_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "tag_assignments") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "tag_assignments", ObjType::Map)
                .expect("put_object tag assignments on root never fails"),
        }
    }

    pub fn assignment_key(page: PageId, tag: TagId) -> String {
        format!("{page}:{tag}")
    }

    #[allow(
        clippy::expect_used,
        reason = "tags is a live map fetched or created immediately above"
    )]
    /// # Panics
    ///
    /// Never in normal use: this method fetches or creates the live `tags`
    /// map before creating a single child record.
    pub fn upsert_tag(doc: &mut AutoCommit, tag: &TagMeta) {
        let tags = Self::tags_map(doc);
        let key = tag.id.to_string();
        let entry = match doc.get(&tags, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&tags, key.as_str(), ObjType::Map)
                .expect("put_object tag map never fails"),
        };
        put_text_diffed(doc, &entry, "name", &tag.name);
    }

    /// Delete the tag and all known assignment edges in stable key order.
    /// Automerge retains these deletes as tombstones; readers additionally
    /// ignore any edge whose tag/page endpoint no longer exists.
    pub fn delete_tag(doc: &mut AutoCommit, id: TagId) {
        let tags = Self::tags_map(doc);
        let _ = doc.delete(&tags, id.to_string().as_str());
        let assignments = Self::assignments_map(doc);
        let suffix = format!(":{id}");
        let mut keys = doc
            .keys(&assignments)
            .filter(|key| key.ends_with(&suffix))
            .collect::<Vec<_>>();
        keys.sort();
        for key in keys {
            let _ = doc.delete(&assignments, key.as_str());
        }
    }

    pub fn set_tag_assignment(doc: &mut AutoCommit, page: PageId, tag: TagId, assigned: bool) {
        let assignments = Self::assignments_map(doc);
        let key = Self::assignment_key(page, tag);
        if assigned {
            put_if_changed(doc, &assignments, key.as_str(), true);
        } else {
            let _ = doc.delete(&assignments, key.as_str());
        }
    }

    pub fn list_tags(doc: &AutoCommit) -> Vec<TagMeta> {
        let Ok(Some((Value::Object(ObjType::Map), tags))) = doc.get(&ROOT, "tags") else {
            return Vec::new();
        };
        let mut out = doc
            .keys(&tags)
            .filter_map(|key| {
                let id = key.parse::<TagId>().ok()?;
                let (Value::Object(ObjType::Map), entry) = doc.get(&tags, key.as_str()).ok()??
                else {
                    return None;
                };
                let (Value::Object(ObjType::Text), name) = doc.get(&entry, "name").ok()?? else {
                    return None;
                };
                Some(TagMeta {
                    id,
                    name: doc.text(&name).unwrap_or_default(),
                })
            })
            .filter(|tag| !tag.name.trim().is_empty())
            .collect::<Vec<_>>();
        out.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        out
    }

    pub fn list_tag_assignments(doc: &AutoCommit) -> Vec<(PageId, TagId)> {
        let Ok(Some((Value::Object(ObjType::Map), assignments))) =
            doc.get(&ROOT, "tag_assignments")
        else {
            return Vec::new();
        };
        let mut out = doc
            .keys(&assignments)
            .filter_map(|key| {
                let (page, tag) = key.split_once(':')?;
                let page = page.parse().ok()?;
                let tag = tag.parse().ok()?;
                let present = matches!(doc.get(&assignments, key.as_str()).ok()?, Some((Value::Scalar(value), _)) if matches!(value.as_ref(), automerge::ScalarValue::Boolean(true)));
                present.then_some((page, tag))
            })
            .collect::<Vec<_>>();
        out.sort();
        out.dedup();
        out
    }
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

        // A page owns its body from its very first action, so a later editor
        // bind never creates collaborative structure outside the canonical
        // action path.
        if Self::page_body(doc, meta.id).is_none()
            && let Ok(body) = doc.put_object(&page, BODY, ObjType::Map)
        {
            let _ = RichText::initialize_under(doc, &body);
        }
    }

    /// Remove a page's entry from the shared doc. Called when a page is
    /// deleted locally so `sync`'s pull-loop (which treats any doc page
    /// missing locally as a remote create) doesn't re-add it next frame.
    pub fn remove_page(doc: &mut AutoCommit, id: PageId) {
        let pages = Self::pages_map(doc);
        let _ = doc.delete(&pages, id.to_string().as_str());
        // The note endpoint owns its membership edges too. Delete in a stable
        // lexical order so a recursive DeletePage action produces the same
        // operation sequence on every replica, while Automerge retains the
        // removals as tombstones for stale peers.
        let assignments = Self::assignments_map(doc);
        let prefix = format!("{id}:");
        let mut keys = doc
            .keys(&assignments)
            .filter(|key| key.starts_with(&prefix))
            .collect::<Vec<_>>();
        keys.sort();
        for key in keys {
            let _ = doc.delete(&assignments, key.as_str());
        }
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
                Ok(Some((Value::Scalar(v), _))) => num::u32_of_i64(v.as_i64().unwrap_or(0)),
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

    /// The page's body: the map its rich text lives under. Never writes.
    pub fn page_body(doc: &AutoCommit, id: PageId) -> Option<ObjId> {
        let page = page_map(doc, id)?;
        match doc.get(&page, BODY) {
            Ok(Some((Value::Object(ObjType::Map), body))) => Some(body),
            _ => None,
        }
    }

    /// The page's body as the editor shows it, a block per paragraph. Empty
    /// for a page without one.
    pub fn page_blocks(doc: &AutoCommit, id: PageId) -> Vec<BlockContent> {
        Self::page_body(doc, id)
            .and_then(|body| tilo_rich_text::blocks_under(doc, &body).ok())
            .unwrap_or_default()
    }

    /// Stable block identities and plain text for one page, in order.
    pub fn page_block_snapshots(doc: &AutoCommit, id: PageId) -> Vec<(String, String)> {
        Self::page_blocks(doc, id)
            .into_iter()
            .map(|block| (block.id, block.text))
            .collect()
    }

    /// The Notes formatting and page links of one page, per block, as UTF-8
    /// byte ranges.
    pub fn page_block_marks(doc: &AutoCommit, page: PageId) -> Vec<(String, Vec<InlineMarkSpan>)> {
        Self::page_blocks(doc, page)
            .into_iter()
            .map(|block| {
                let mut marks = block
                    .marks
                    .iter()
                    .map(InlineMarkSpan::from_ui)
                    .collect::<Vec<_>>();
                marks.sort_by(|a, b| {
                    (a.start, a.end, inline_mark_rank(&a.mark)).cmp(&(
                        b.start,
                        b.end,
                        inline_mark_rank(&b.mark),
                    ))
                });
                (block.id, marks)
            })
            .collect()
    }

    /// Apply one rich-text edit to a page's body.
    ///
    /// # Errors
    /// Returns the refusal when the page has no body or the edit is stale.
    pub fn edit_page_body(
        doc: &mut AutoCommit,
        page: PageId,
        edit: &RichTextAction,
    ) -> Result<(), String> {
        let body = Self::page_body(doc, page).ok_or("page no longer exists")?;
        edit.apply_under(doc, &body)
            .map_err(|error| error.to_string())
    }

    /// Write `blocks` into a page's still-empty body: a new page's first
    /// content (an import, a copy).
    ///
    /// # Errors
    /// Refuses a page without a body or one that already holds text.
    pub fn fill_page_body(
        doc: &mut AutoCommit,
        page: PageId,
        blocks: &[BlockContent],
    ) -> Result<(), String> {
        let body = Self::page_body(doc, page).ok_or("page no longer exists")?;
        tilo_rich_text::fill_under(doc, &body, blocks).map_err(|error| error.to_string())
    }
}

/// The page map's key holding its rich-text body.
const BODY: &str = "body";

fn page_map(doc: &AutoCommit, id: PageId) -> Option<ObjId> {
    let Ok(Some((Value::Object(ObjType::Map), pages))) = doc.get(&ROOT, "pages") else {
        return None;
    };
    match doc.get(&pages, id.to_string().as_str()) {
        Ok(Some((Value::Object(ObjType::Map), page))) => Some(page),
        _ => None,
    }
}

fn inline_mark_rank(mark: &InlineMark) -> u8 {
    match mark {
        InlineMark::Bold => 0,
        InlineMark::Italic => 1,
        InlineMark::Underline => 2,
        InlineMark::Strikethrough => 3,
        InlineMark::Code => 4,
        InlineMark::Link { .. } => 5,
        InlineMark::Transclusion { .. } => 6,
        InlineMark::ExternalLink { .. } => 7,
        InlineMark::Mention { .. } => 8,
    }
}

struct PreparedDuplicate {
    page: PageId,
    blocks: Vec<BlockContent>,
    tags: Vec<TagId>,
}

/// Preflight the whole source/mapping and snapshot every copied body before
/// writes. Links and embeds into the copied subtree follow the copy.
fn prepare_duplicate(
    doc: &AutoCommit,
    source: PageId,
    pages: &[crate::actions::DuplicatePage],
) -> Result<(crate::Workspace, Vec<PreparedDuplicate>), String> {
    use std::collections::{BTreeMap, HashSet};
    let mut workspace = crate::notebook::project(doc, None)?;
    let original = workspace
        .get(source)
        .ok_or("source page no longer exists")?
        .clone();
    let mut subtree = HashSet::new();
    let mut pending = vec![source];
    while let Some(id) = pending.pop() {
        if subtree.insert(id) {
            pending.extend(
                workspace
                    .pages()
                    .iter()
                    .filter(|p| p.parent == Some(id))
                    .map(|p| p.id),
            );
        }
    }
    let mut used = workspace
        .pages()
        .iter()
        .map(|p| p.id)
        .collect::<HashSet<_>>();
    let mut mapping = BTreeMap::new();
    for page in pages {
        let src = page
            .source
            .parse::<PageId>()
            .map_err(|_| "invalid source page id")?;
        let dst = page
            .destination
            .parse::<PageId>()
            .map_err(|_| "invalid destination page id")?;
        if !subtree.contains(&src)
            || dst.is_nil()
            || !used.insert(dst)
            || mapping.insert(src, dst).is_some()
        {
            return Err("duplicate page mapping is stale or reuses an identity".into());
        }
    }
    if mapping.len() != subtree.len() {
        return Err("duplicate page mapping is incomplete".into());
    }
    let new_root = *mapping.get(&source).ok_or("missing source mapping")?;
    let next_sibling = workspace
        .children_of(original.parent)
        .iter()
        .map(|p| p.id)
        .skip_while(|id| *id != source)
        .nth(1);
    let follow = |page: PageId| mapping.get(&page).copied().unwrap_or(page);
    let mut prepared = Vec::new();
    let mut metadata = Vec::new();
    for (&src, &dst) in &mapping {
        let mut blocks = NotesDoc::page_blocks(doc, src);
        for block in &mut blocks {
            // A copied fence is a fence of its own: its group id follows the
            // copy, the same on every replica.
            if let Some(group) = &block.code_group {
                let digest = blake3::hash(format!("{dst}/{group}").as_bytes());
                let mut bytes = [0_u8; 16];
                bytes.copy_from_slice(digest.as_bytes().get(..16).unwrap_or(&[0; 16]));
                block.code_group = Some(uuid::Uuid::from_bytes(bytes).to_string());
            }
            for span in &mut block.marks {
                span.mark = match &span.mark {
                    Mark::Link(page) => Mark::Link(follow(*page)),
                    Mark::Transclusion(page) => Mark::Transclusion(follow(*page)),
                    other => other.clone(),
                };
            }
        }
        let mut meta = workspace.get(src).ok_or("source page is missing")?.clone();
        meta.id = dst;
        if src == source {
            meta.title.push_str(" copy");
        } else {
            meta.parent = meta.parent.and_then(|parent| mapping.get(&parent).copied());
        }
        metadata.push(meta);
        prepared.push(PreparedDuplicate {
            page: dst,
            blocks,
            tags: workspace
                .tags_for_page(src)
                .into_iter()
                .map(|tag| tag.id)
                .collect(),
        });
    }
    workspace.data.pages.extend(metadata);
    workspace.move_to(new_root, original.parent, next_sibling);
    workspace.check_invariants()?;
    Ok((workspace, prepared))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PageMeta;
    use tilo_automerge::ApplyIncoming;
    use uuid::Uuid;

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn pages_round_trip_merge_and_rename_with_bounded_growth() {
        eprintln!("phase pages_round_trip");
        {
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

        eprintln!("phase parent_and_order_round_trip");
        {
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

        eprintln!("phase rename_updates_in_place");
        {
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

        eprintln!("phase two_docs_merge_pages");
        {
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
            a.merge_incoming(&mut b_loaded).unwrap();

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

        eprintln!("phase renaming_by_one_char_is_a_bounded_splice");
        {
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

        eprintln!("phase idempotent_upsert_does_not_grow_the_doc");
        {
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
    }

    fn page(doc: &mut AutoCommit, title: &str) -> PageId {
        let id = Uuid::new_v4();
        let order = u32::try_from(NotesDoc::list_pages(doc).len()).unwrap();
        NotesDoc::upsert_page_meta(
            doc,
            &PageMeta {
                id,
                title: title.into(),
                parent: None,
                order,
            },
        );
        id
    }

    #[test]
    fn a_page_body_is_rich_text_edited_by_rich_text_actions() {
        use tilo_document::action::RegionKind;
        let mut doc = AutoCommit::new();
        let id = page(&mut doc, "P");
        assert!(
            NotesDoc::page_body(&doc, id).is_some(),
            "a page starts its body"
        );
        assert_eq!(NotesDoc::page_block_snapshots(&doc, id).len(), 1);
        let mut bold = BlockContent::paragraph("", "café bold");
        bold.marks = vec![ui::widget::MarkSpan::new(6..10, Mark::Bold)];
        NotesDoc::fill_page_body(&mut doc, id, &[bold]).unwrap();
        let (block, text) = NotesDoc::page_block_snapshots(&doc, id).remove(0);
        assert_eq!(text, "café bold");
        assert_eq!(
            NotesDoc::page_block_marks(&doc, id),
            [(
                block.clone(),
                vec![InlineMarkSpan {
                    start: 6,
                    end: 10,
                    mark: InlineMark::Bold
                }]
            )]
        );
        NotesDoc::edit_page_body(
            &mut doc,
            id,
            &RichTextAction::SpliceText {
                region: RegionKind::Body,
                block,
                at: 0,
                del: 4,
                ins: "cafe".into(),
            },
        )
        .unwrap();
        assert_eq!(NotesDoc::page_block_snapshots(&doc, id)[0].1, "cafe bold");
        assert!(
            NotesDoc::edit_page_body(
                &mut doc,
                Uuid::new_v4(),
                &RichTextAction::AppendParagraph { text: "x".into() }
            )
            .is_err(),
            "a missing page refuses"
        );
        // An upsert of an existing page never restarts its body.
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id,
                title: "Renamed".into(),
                parent: None,
                order: 0,
            },
        );
        assert_eq!(NotesDoc::page_block_snapshots(&doc, id)[0].1, "cafe bold");
    }

    #[test]
    fn a_duplicate_copies_bodies_and_follows_links_into_the_copy() {
        let mut doc = AutoCommit::new();
        crate::notebook::initialize_empty(&mut doc).unwrap();
        let source = page(&mut doc, "Source");
        let outside = page(&mut doc, "Outside");
        let mut linked = BlockContent::paragraph("", "self and out");
        linked.marks = vec![
            ui::widget::MarkSpan::new(0..4, Mark::Link(source)),
            ui::widget::MarkSpan::new(9..12, Mark::Link(outside)),
        ];
        NotesDoc::fill_page_body(&mut doc, source, &[linked]).unwrap();
        let copy = Uuid::new_v4();
        NotesDoc::duplicate_subtree(
            &mut doc,
            source,
            &[crate::actions::DuplicatePage {
                source: source.to_string(),
                destination: copy.to_string(),
            }],
        )
        .unwrap();
        let blocks = NotesDoc::page_blocks(&doc, copy);
        assert_eq!(blocks[0].text, "self and out");
        assert_eq!(
            blocks[0].marks,
            [
                ui::widget::MarkSpan::new(0..4, Mark::Link(copy)),
                ui::widget::MarkSpan::new(9..12, Mark::Link(outside)),
            ]
        );
        assert_eq!(
            NotesDoc::page_blocks(&doc, source)[0].marks[0],
            ui::widget::MarkSpan::new(0..4, Mark::Link(source)),
            "the source is untouched"
        );
    }
}
