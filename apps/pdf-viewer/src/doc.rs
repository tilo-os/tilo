//! Automerge document model for the PDF viewer's collaborative state.
//!
//! The rendered document (page text) is static and lives in
//! [`crate::model::Document`]. What is *collaborative* — and therefore lives
//! in the CRDT — is everything the user creates on top of it: a durable page
//! plan, annotations, threaded comments and signatures. Two peers reviewing
//! the same PDF converge on the same intent and markup; a peer without the
//! source bytes can still inspect the page plan but cannot rasterise it.
//!
//! **One document's markup, not the app's.** This is the schema of a
//! *record* — the annotation-set record derived from the file being read
//! (`PdfApp::markup_record_for`) — so page numbers, which only mean anything
//! within one file, are only ever read against the file they came from.
//! Opening a second document checks out that document's own markup record;
//! nothing is cleared, because nothing is shared.
//!
//! ```text
//! root
//! ├── "meta" (Map) { title, author, seeded:bool,
//! │                  source-hash, source-len, source-mime, imported-source }
//! ├── "page-plan" (List of Map) { id, source-page, [import descriptor] }
//! ├── "page-plan-tombstones" (Map) { <page id>: true }
//! ├── "page-rotation" (Map) { <page id>: degrees }
//! ├── "annotations" (List of Map)
//! │     { id, page-id, kind, color, author, created-ms, geom-space,
//! │       quads, chars | points | strokes | x, y | x, y, w, h, text, font-size }
//! ├── "comments" (List of Map)
//! │     { id, anno, page-id, author, body, created, resolved, replies:List }
//! ├── "signatures" (List of Map)
//! │     { id, page-id, geom-space, x, y, w, h, signer, date, ink, strokes }
//! ├── "form-values" (Map) { <field name>: Map { kind, value, export } }
//! └── "adopted" (Map) { <annotation or reply id>:
//!                        Map { page, index, name, kind, print, parent } }
//! ```
//!
//! **Markup already in the PDF is taken over.** The first time a record
//! meets its source bytes, the file's highlights, notes and their reply
//! threads, free text, ink and form values become records like any other
//! (`meta.imported-source` names the bytes imported; ids derive from them so
//! concurrent imports merge). `adopted` remembers where each came from, so
//! an export keeps, replaces or removes the originals instead of adding
//! copies — see [`crate::adopt`].
//!
//! **The PDF itself is an immutable blob.** `meta.source-*` pins the
//! content hash of the bytes the markup was made against; the same bytes are
//! kept in the content-addressed blob store and listed as a blob reference in
//! the record's metadata. Every annotation below is relative to those bytes.
//!
//! **Anchors are stable.** Markup names its page by durable occurrence id
//! (`page-id`), never by position, so reordering pages, deleting them and
//! annotating concurrently compose. Geometry is stored in the page's PDF user
//! space (`geom-space: "user"`), which does not change however the page is
//! displayed or rotated; text markup also keeps the engine character range it
//! covers (`chars`). Records written before these fields carry page-point
//! geometry and a page ordinal, and are still read.
//!
//! **Concurrency.** Lists merge by Automerge's list CRDT, so concurrent
//! annotations on one page all survive. Deleting a page writes a delete-wins
//! tombstone; markup anchored to it stays in the record (hidden) and comes
//! back if the page is restored by undo or history. Page rotation and form
//! values are last-writer-wins registers (one map key per page / field):
//! concurrent writes converge on the value of the operation Automerge orders
//! last (highest Lamport counter, ties broken by actor id); the other value
//! remains in the key's conflict set.
//!
//! View state (current page, zoom, active tool) is deliberately NOT here —
//! it is per-peer, so two reviewers can look at different pages of the
//! same shared markup.

use std::collections::HashSet;

/// Where a comment or reply keeps its text: rich text.
const TEXT: &str = "text";
use ui::doc::{read_bool, read_number, read_scalar, read_str};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{
    AnnoKind, Annotation, Comment, FormEntry, GeomSpace, MarkupStyle, PtRect, Reply, Signature,
    Swatch,
};

/// A durable page-plan entry. `source_page` names the immutable original page
/// in the opened PDF; `id` names this occurrence (duplicates therefore get a
/// new id while retaining the same source page).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PagePlanEntry {
    pub id: String,
    pub source_page: usize,
    /// `None` denotes an immutable page of the file which was opened.  A
    /// populated value names bytes in the app's content store by hash; bytes
    /// never enter Automerge.
    pub imported: Option<ImportedPage>,
    /// Clockwise rotation (0, 90, 180 or 270) added to the source page's own.
    /// Held in the record's `page-rotation` map, keyed by occurrence id, so a
    /// concurrent move of the occurrence cannot lose it.
    #[serde(default)]
    pub rotation: i32,
}

/// A page occurrence from an imported payload. Dimensions are captured at
/// authoring time so a peer can reserve an honest page-shaped placeholder
/// before it has downloaded the blob.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImportedPage {
    pub hash: String,
    pub kind: String,
    pub page: usize,
    pub width_milli: u32,
    pub height_milli: u32,
}

impl ImportedPage {
    pub const MAX_HASH_LEN: usize = 128;
    pub fn valid(&self) -> bool {
        self.hash.len() == 64
            && self.hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            && matches!(self.kind.as_str(), "pdf" | "image")
            && self.page > 0
            && self.width_milli > 0
            && self.height_milli > 0
            && self.width_milli <= 100_000_000
            && self.height_milli <= 100_000_000
    }
}

pub struct PdfDoc;

impl PdfDoc {
    // ---- scalar read helpers ----------------------------------------

    /// An `Int`, or a `Uint` reinterpreted as `i64`.
    fn get_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
        match read_scalar(doc, obj, key)? {
            ScalarValue::Int(i) => Some(i),
            ScalarValue::Uint(u) => Some(u as i64),
            _ => None,
        }
    }

    /// The `List` object at root `key`, creating it if missing. `None` only
    /// if `key` already names a non-list value on this document — every
    /// write below then no-ops rather than corrupting that value.
    fn list(doc: &mut AutoCommit, key: &str) -> Option<ObjId> {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
            _ => doc.put_object(&ROOT, key, ObjType::List).ok(),
        }
    }

    /// Every object of type `ty` at root `key`: the one Automerge shows
    /// first, then any other replica created concurrently under the same key.
    /// Two peers that each create a container before syncing produce two
    /// objects; reading all of them keeps both peers' entries.
    fn roots(doc: &AutoCommit, key: &str, ty: ObjType) -> Vec<ObjId> {
        let mut out: Vec<ObjId> = Vec::new();
        if let Ok(Some((Value::Object(t), id))) = doc.get(&ROOT, key)
            && t == ty
        {
            out.push(id);
        }
        for (value, id) in doc.get_all(&ROOT, key).unwrap_or_default() {
            if matches!(value, Value::Object(t) if t == ty) && !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    /// Every entry map of the lists at root `key`, in list order, the
    /// visible list first.
    fn entries(doc: &AutoCommit, key: &str) -> Vec<ObjId> {
        let mut out = Vec::new();
        for list in Self::roots(doc, key, ObjType::List) {
            for i in 0..doc.length(&list) {
                if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) {
                    out.push(m);
                }
            }
        }
        out
    }

    /// The list and position of entry `id` among the lists at root `key`.
    fn locate(doc: &AutoCommit, key: &str, id: &str) -> Option<(ObjId, usize)> {
        Self::roots(doc, key, ObjType::List)
            .into_iter()
            .find_map(|list| Self::index_of(doc, &list, id).map(|i| (list, i)))
    }

    /// Create every container the schema uses, once, when the record is
    /// first seeded, so replicas that sync the seed share them.
    fn ensure_containers(doc: &mut AutoCommit) {
        for key in ["annotations", "comments", "signatures"] {
            let _ = Self::list(doc, key);
        }
        for key in ["page-plan-tombstones", "page-rotation", "form-values"] {
            if Self::roots(doc, key, ObjType::Map).is_empty() {
                let _ = doc.put_object(&ROOT, key, ObjType::Map);
            }
        }
    }

    /// Delete-wins markers for durable page occurrences. A stale peer may
    /// still hold the old list object after a deletion; the keyed marker keeps
    /// that occurrence hidden when the replicas merge instead of reviving it.
    fn page_tombstoned(doc: &AutoCommit, id: &str) -> bool {
        Self::roots(doc, "page-plan-tombstones", ObjType::Map)
            .iter()
            .any(|map| doc.get(map, id).ok().flatten().is_some())
    }

    fn tombstone_page(doc: &mut AutoCommit, id: &str) {
        let map = match doc.get(&ROOT, "page-plan-tombstones") {
            Ok(Some((Value::Object(ObjType::Map), map))) => Some(map),
            _ => doc
                .put_object(&ROOT, "page-plan-tombstones", ObjType::Map)
                .ok(),
        };
        if let Some(map) = map {
            let _ = doc.put(&map, id, true);
        }
    }

    // ---- meta -------------------------------------------------------

    /// The document's `meta` map, creating it if missing. `None` only if
    /// `"meta"` already names a non-map value on this document.
    pub fn meta(doc: &mut AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, "meta") {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => doc.put_object(&ROOT, "meta", ObjType::Map).ok(),
        }
    }

    pub fn is_seeded(doc: &mut AutoCommit) -> bool {
        let Some(m) = Self::meta(doc) else {
            return false;
        };
        read_bool(doc, &m, "seeded").unwrap_or(false)
    }

    pub fn set_meta(doc: &mut AutoCommit, title: &str, author: &str) {
        let Some(m) = Self::meta(doc) else {
            return;
        };
        let _ = doc.put(&m, "title", title);
        let _ = doc.put(&m, "author", author);
        let _ = doc.put(&m, "seeded", true);
    }

    // ---- shared page plan ------------------------------------------

    /// Initialise the page plan exactly once from the immutable source PDF.
    /// Existing shared plans are never replaced on reopen. Concurrent peers can
    /// both insert the deterministic `source-N` entries; `list_page_plan`
    /// canonicalises those physical CRDT duplicates by durable occurrence id.
    pub fn ensure_page_plan(doc: &mut AutoCommit, source_page_count: usize) {
        let Some(list) = Self::list(doc, "page-plan") else {
            return;
        };
        let Some(meta) = Self::meta(doc) else {
            return;
        };
        if read_bool(doc, &meta, "page-plan-seeded").unwrap_or(false) {
            return;
        }
        Self::ensure_containers(doc);

        let canonical = Self::list_page_plan(doc);
        // Documents written before this marker use a non-empty source plan as
        // their durable initialization signal.  Do not resurrect a source
        // occurrence which was intentionally deleted in such a document.
        // A plan containing imports *only*, however, can be produced when an
        // ImportPages action arrives before this peer has seeded its immutable
        // base source.  In that case add the deterministic base occurrences
        // beside the imported ones before committing the marker.
        if canonical.iter().any(|entry| entry.imported.is_none()) {
            let _ = doc.put(&meta, "page-plan-seeded", true);
            return;
        }
        let present: HashSet<String> = canonical.into_iter().map(|entry| entry.id).collect();
        for source_page in 1..=source_page_count.max(1) {
            let id = format!("source-{source_page}");
            if present.contains(&id) {
                continue;
            }
            let Ok(entry) = doc.insert_object(&list, doc.length(&list), ObjType::Map) else {
                continue;
            };
            let _ = doc.put(&entry, "id", id);
            let _ = doc.put(&entry, "source-page", source_page as i64);
        }
        let _ = doc.put(&meta, "page-plan-seeded", true);
    }

    /// Return the canonical materialised plan. The CRDT list may contain
    /// duplicate physical maps after concurrent initialise/move operations;
    /// each durable occurrence id appears only once, chosen by Automerge's
    /// deterministic list order. Invalid maps do not reserve an id, so a later
    /// valid representation can still repair a malformed concurrent entry.
    pub fn list_page_plan(doc: &AutoCommit) -> Vec<PagePlanEntry> {
        let mut seen = HashSet::new();
        Self::entries(doc, "page-plan")
            .into_iter()
            .filter_map(|entry| {
                let id = read_str(doc, &entry, "id")?;
                let source_page = Self::get_i64(doc, &entry, "source-page")? as usize;
                let imported = match read_str(doc, &entry, "content-hash") {
                    Some(hash) => Some(ImportedPage {
                        hash,
                        kind: read_str(doc, &entry, "content-kind")?,
                        page: Self::get_i64(doc, &entry, "content-page")? as usize,
                        width_milli: Self::get_i64(doc, &entry, "page-width-milli")? as u32,
                        height_milli: Self::get_i64(doc, &entry, "page-height-milli")? as u32,
                    }),
                    None => None,
                };
                (source_page > 0
                    && !Self::page_tombstoned(doc, &id)
                    && imported.as_ref().is_none_or(ImportedPage::valid)
                    && seen.insert(id.clone()))
                .then(|| PagePlanEntry {
                    rotation: Self::page_rotation(doc, &id),
                    id,
                    source_page,
                    imported,
                })
            })
            .collect()
    }

    fn page_id_for_number(doc: &AutoCommit, page: usize) -> Option<String> {
        Self::list_page_plan(doc)
            .get(page.checked_sub(1)?)
            .map(|entry| entry.id.clone())
    }

    /// Upgrade ordinal-only anchors from records written before durable page
    /// occurrences existed. This is schema hydration, not a user edit: it runs
    /// once after the deterministic source plan is available and preserves the
    /// page each legacy anchor originally denoted.
    pub fn migrate_page_anchors(doc: &mut AutoCommit) {
        for key in ["annotations", "comments", "signatures"] {
            for entry in Self::entries(doc, key) {
                if read_str(doc, &entry, "page-id").is_some() {
                    continue;
                }
                let page = Self::get_i64(doc, &entry, "page").unwrap_or(1) as usize;
                if let Some(page_id) = Self::page_id_for_number(doc, page) {
                    let _ = doc.put(&entry, "page-id", page_id);
                }
            }
        }
    }

    pub fn duplicate_page_plan(
        doc: &mut AutoCommit,
        id: &str,
        new_id: &str,
        expected: &PagePlanEntry,
    ) {
        if Self::page_tombstoned(doc, new_id) || Self::locate(doc, "page-plan", new_id).is_some() {
            return;
        }
        let Some((list, index)) = Self::locate(doc, "page-plan", id) else {
            return;
        };
        let Some(stored) = Self::list_page_plan(doc)
            .into_iter()
            .find(|entry| entry.id == id)
        else {
            return;
        };
        if stored.source_page != expected.source_page || stored.imported != expected.imported {
            return;
        }
        let Ok(copy) = doc.insert_object(&list, index + 1, ObjType::Map) else {
            return;
        };
        Self::put_page_entry(
            doc,
            &copy,
            &PagePlanEntry {
                id: new_id.to_string(),
                ..expected.clone()
            },
        );
    }

    /// Insert an atomic, prevalidated batch of imported occurrences.  Invalid
    /// action payloads are a no-op: no partially imported page plan is ever
    /// visible to another replica.
    pub fn import_pages(doc: &mut AutoCommit, after_id: Option<&str>, pages: &[PagePlanEntry]) {
        const MAX_IMPORTED_PAGES: usize = 512;
        if pages.is_empty()
            || pages.len() > MAX_IMPORTED_PAGES
            || pages.iter().any(|entry| {
                entry.id.is_empty()
                    || entry.id.len() > 160
                    || entry.imported.as_ref().is_none_or(|p| !p.valid())
            })
        {
            return;
        }
        if pages.iter().any(|entry| {
            Self::page_tombstoned(doc, &entry.id)
                || Self::locate(doc, "page-plan", &entry.id).is_some()
        }) {
            return;
        }
        let mut seen = HashSet::new();
        if pages.iter().any(|entry| !seen.insert(&entry.id)) {
            return;
        }
        let located = after_id.and_then(|id| Self::locate(doc, "page-plan", id));
        let (list, at) = if let Some((list, i)) = located {
            (list, i + 1)
        } else {
            let Some(list) = Self::list(doc, "page-plan") else {
                return;
            };
            let end = doc.length(&list);
            (list, end)
        };
        for (at, page) in (at..).zip(pages) {
            let Ok(map) = doc.insert_object(&list, at, ObjType::Map) else {
                return;
            };
            Self::put_page_entry(doc, &map, page);
        }
    }

    fn put_page_entry(doc: &mut AutoCommit, map: &ObjId, entry: &PagePlanEntry) {
        let _ = doc.put(map, "id", entry.id.as_str());
        let _ = doc.put(map, "source-page", entry.source_page as i64);
        if let Some(imported) = &entry.imported {
            let _ = doc.put(map, "content-hash", imported.hash.as_str());
            let _ = doc.put(map, "content-kind", imported.kind.as_str());
            let _ = doc.put(map, "content-page", imported.page as i64);
            let _ = doc.put(map, "page-width-milli", i64::from(imported.width_milli));
            let _ = doc.put(map, "page-height-milli", i64::from(imported.height_milli));
        }
    }

    pub fn delete_page_plan(doc: &mut AutoCommit, id: &str) {
        if Self::list_page_plan(doc).len() <= 1 {
            return;
        }
        Self::tombstone_page(doc, id);
        Self::remove_plan_entries(doc, id);
        // Markup anchored to the occurrence is kept: it is hidden while the
        // page is deleted and reappears if the deletion is undone or an
        // earlier version is restored. A duplicate deliberately duplicates
        // only page content, never review markup.
    }

    /// Remove every physical representation of occurrence `id` from every
    /// page-plan list.
    fn remove_plan_entries(doc: &mut AutoCommit, id: &str) {
        for list in Self::roots(doc, "page-plan", ObjType::List) {
            Self::remove_from_list(doc, &list, id);
        }
    }

    fn remove_from_list(doc: &mut AutoCommit, list: &ObjId, id: &str) {
        let list = list.clone();
        for index in (0..doc.length(&list)).rev() {
            let is_target = doc
                .get(&list, index)
                .ok()
                .flatten()
                .and_then(|(value, entry)| {
                    matches!(value, Value::Object(ObjType::Map))
                        .then(|| read_str(doc, &entry, "id"))
                        .flatten()
                })
                .is_some_and(|candidate| candidate == id);
            if is_target {
                let _ = doc.delete(&list, index);
            }
        }
    }

    // ---- page rotation ----------------------------------------------

    /// The rotation (0, 90, 180, 270) recorded for page occurrence `id`.
    pub fn page_rotation(doc: &AutoCommit, id: &str) -> i32 {
        Self::roots(doc, "page-rotation", ObjType::Map)
            .iter()
            .find_map(|map| Self::get_i64(doc, map, id))
            .and_then(|d| i32::try_from(d).ok())
            .filter(|d| d % 90 == 0)
            .map_or(0, |d| d.rem_euclid(360))
    }

    /// Set page occurrence `id`'s rotation (any multiple of 90).
    pub fn set_page_rotation(doc: &mut AutoCommit, id: &str, degrees: i32) {
        if degrees % 90 != 0 || id.is_empty() {
            return;
        }
        let map = match doc.get(&ROOT, "page-rotation") {
            Ok(Some((Value::Object(ObjType::Map), map))) => Some(map),
            _ => doc.put_object(&ROOT, "page-rotation", ObjType::Map).ok(),
        };
        if let Some(map) = map {
            let _ = doc.put(&map, id, i64::from(degrees.rem_euclid(360)));
        }
    }

    // ---- the pinned source blob -------------------------------------

    /// The content hash, length and type of the PDF bytes this record's
    /// markup was made against.
    pub fn source(doc: &AutoCommit) -> Option<(String, u64, String)> {
        let Ok(Some((Value::Object(ObjType::Map), meta))) = doc.get(&ROOT, "meta") else {
            return None;
        };
        let hash = read_str(doc, &meta, "source-hash")?;
        let len = Self::get_i64(doc, &meta, "source-len").and_then(|l| u64::try_from(l).ok())?;
        let mime = read_str(doc, &meta, "source-mime").unwrap_or_else(|| "application/pdf".into());
        Some((hash, len, mime))
    }

    /// Pin the source bytes by content hash. The first pin wins: a record
    /// never silently re-targets its markup at different bytes.
    pub fn pin_source(doc: &mut AutoCommit, hash: &str, len: u64, mime: &str) {
        let valid = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        if !valid || Self::source(doc).is_some() {
            return;
        }
        let Some(meta) = Self::meta(doc) else {
            return;
        };
        let _ = doc.put(&meta, "source-hash", hash.to_ascii_lowercase());
        let _ = doc.put(&meta, "source-len", i64::try_from(len).unwrap_or(i64::MAX));
        let _ = doc.put(&meta, "source-mime", mime);
        let _ = ui::record::add_blob_reference(
            doc,
            &ui::record::BlobRef {
                hash: hash.to_ascii_lowercase(),
                len,
                mime: mime.to_string(),
                relation: "source".to_string(),
            },
        );
    }

    // ---- markup taken over from the PDF -----------------------------

    /// The source hash whose existing markup this record has taken over.
    pub fn imported_source(doc: &AutoCommit) -> Option<String> {
        let Ok(Some((Value::Object(ObjType::Map), meta))) = doc.get(&ROOT, "meta") else {
            return None;
        };
        read_str(doc, &meta, "imported-source")
    }

    /// Take over a document's existing markup: add the planned records and
    /// remember where each came from. Idempotent per source: a record that
    /// already imported `source_hash` is left alone, and every record id is
    /// derived from the source, so concurrent imports merge.
    pub fn import_existing(
        doc: &mut AutoCommit,
        source_hash: &str,
        plan: &crate::adopt::ImportPlan,
    ) {
        if Self::imported_source(doc).as_deref() == Some(source_hash) {
            return;
        }
        Self::ensure_containers(doc);
        for (annotation, page_id) in &plan.annotations {
            Self::add_annotation(doc, annotation, Some(page_id));
        }
        for comment in &plan.comments {
            Self::add_comment(doc, comment);
        }
        let recorded = Self::form_values(doc);
        for (name, value) in &plan.form_values {
            if !recorded.contains_key(name) {
                Self::set_form_value(doc, name, value);
            }
        }
        let map = match doc.get(&ROOT, "adopted") {
            Ok(Some((Value::Object(ObjType::Map), map))) => Some(map),
            _ => doc.put_object(&ROOT, "adopted", ObjType::Map).ok(),
        };
        if let Some(map) = map {
            for entry in &plan.adopted {
                let Ok(e) = doc.put_object(&map, entry.id.as_str(), ObjType::Map) else {
                    continue;
                };
                let _ = doc.put(&e, "page", entry.page as i64);
                let _ = doc.put(&e, "index", entry.index as i64);
                let _ = doc.put(&e, "name", entry.name.as_str());
                let _ = doc.put(&e, "kind", entry.kind.as_str());
                let _ = doc.put(&e, "print", entry.print.as_str());
                if let Some(parent) = &entry.parent {
                    let _ = doc.put(&e, "parent", parent.as_str());
                }
            }
        }
        if let Some(meta) = Self::meta(doc) {
            let _ = doc.put(&meta, "imported-source", source_hash);
        }
    }

    /// Every taken-over annotation's origin, by record id.
    pub fn adopted(doc: &AutoCommit) -> Vec<crate::adopt::AdoptedEntry> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for map in Self::roots(doc, "adopted", ObjType::Map) {
            for id in doc.keys(&map) {
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&map, id.as_str()) else {
                    continue;
                };
                let (Some(page), Some(index)) = (
                    Self::get_i64(doc, &e, "page").and_then(|p| usize::try_from(p).ok()),
                    Self::get_i64(doc, &e, "index").and_then(|i| usize::try_from(i).ok()),
                ) else {
                    continue;
                };
                out.push(crate::adopt::AdoptedEntry {
                    id,
                    page,
                    index,
                    name: read_str(doc, &e, "name").unwrap_or_default(),
                    kind: read_str(doc, &e, "kind").unwrap_or_default(),
                    print: read_str(doc, &e, "print").unwrap_or_default(),
                    parent: read_str(doc, &e, "parent"),
                });
            }
        }
        out
    }

    // ---- form values ------------------------------------------------

    /// Every form value held in the record, by field name.
    pub fn form_values(doc: &AutoCommit) -> std::collections::BTreeMap<String, FormEntry> {
        let mut out = std::collections::BTreeMap::new();
        for map in Self::roots(doc, "form-values", ObjType::Map) {
            Self::read_form_values(doc, &map, &mut out);
        }
        out
    }

    /// Add `map`'s entries to `out`, keeping values already there (a map
    /// Automerge shows first takes precedence over a concurrent duplicate).
    fn read_form_values(
        doc: &AutoCommit,
        map: &ObjId,
        out: &mut std::collections::BTreeMap<String, FormEntry>,
    ) {
        let map = map.clone();
        for name in doc.keys(&map) {
            if out.contains_key(&name) {
                continue;
            }
            let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&map, name.as_str())
            else {
                continue;
            };
            let value = read_str(doc, &entry, "value").unwrap_or_default();
            let parsed = match read_str(doc, &entry, "kind").as_deref() {
                Some("text") => Some(FormEntry::Text(value)),
                Some("choice") => Some(FormEntry::Choice(value)),
                Some("checked") => Some(FormEntry::Checked {
                    on: value == "on",
                    export: read_str(doc, &entry, "export"),
                }),
                _ => None,
            };
            if let Some(parsed) = parsed {
                out.insert(name, parsed);
            }
        }
    }

    /// Record a value for field `name`. Each assignment replaces the whole
    /// entry, so concurrent assignments resolve as one register (see the
    /// module documentation).
    pub fn set_form_value(doc: &mut AutoCommit, name: &str, value: &FormEntry) {
        if name.is_empty() || name.len() > 1024 {
            return;
        }
        let map = match doc.get(&ROOT, "form-values") {
            Ok(Some((Value::Object(ObjType::Map), map))) => Some(map),
            _ => doc.put_object(&ROOT, "form-values", ObjType::Map).ok(),
        };
        let Some(map) = map else {
            return;
        };
        let Ok(entry) = doc.put_object(&map, name, ObjType::Map) else {
            return;
        };
        let (kind, text, export) = match value {
            FormEntry::Text(t) => ("text", t.clone(), None),
            FormEntry::Choice(c) => ("choice", c.clone(), None),
            FormEntry::Checked { on, export } => (
                "checked",
                if *on { "on" } else { "off" }.to_string(),
                export.clone(),
            ),
        };
        let _ = doc.put(&entry, "kind", kind);
        let _ = doc.put(&entry, "value", text);
        if let Some(export) = export {
            let _ = doc.put(&entry, "export", export);
        }
    }

    /// Forget the recorded value for field `name` (the PDF's own value shows
    /// again).
    pub fn clear_form_value(doc: &mut AutoCommit, name: &str) {
        for map in Self::roots(doc, "form-values", ObjType::Map) {
            let _ = doc.delete(&map, name);
        }
    }

    pub fn move_page_plan(doc: &mut AutoCommit, id: &str, after_id: Option<&str>) {
        if Self::locate(doc, "page-plan", id).is_none() {
            return;
        }
        let Some(moved_entry) = Self::list_page_plan(doc)
            .into_iter()
            .find(|candidate| candidate.id == id)
        else {
            return;
        };
        // Remove every physical representation of this durable occurrence.
        // Concurrent moves can each have inserted one, and leaving an older
        // one behind would let a subsequent delete reveal a ghost page.
        Self::remove_plan_entries(doc, id);
        let located = after_id.and_then(|after| Self::locate(doc, "page-plan", after));
        let (list, to) = if let Some((list, i)) = located {
            (list, i + 1)
        } else {
            let Some(list) = Self::list(doc, "page-plan") else {
                return;
            };
            (list, 0)
        };
        let Ok(moved) = doc.insert_object(&list, to, ObjType::Map) else {
            return;
        };
        Self::put_page_entry(doc, &moved, &moved_entry);
    }

    // ---- annotations ------------------------------------------------

    fn points_to_str(points: &[(f64, f64)]) -> String {
        points
            .iter()
            .map(|(x, y)| format!("{x:.2},{y:.2}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn points_from_str(s: &str) -> Vec<(f64, f64)> {
        s.split_whitespace()
            .filter_map(|pair| {
                let mut it = pair.split(',');
                let x = it.next()?.parse::<f64>().ok()?;
                let y = it.next()?.parse::<f64>().ok()?;
                Some((x, y))
            })
            .collect()
    }

    fn rects_to_str(rects: &[PtRect]) -> String {
        rects
            .iter()
            .map(|r| format!("{:.2},{:.2},{:.2},{:.2}", r.x, r.y, r.w, r.h))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn rects_from_str(s: &str) -> Vec<PtRect> {
        s.split_whitespace()
            .filter_map(|quad| {
                let v: Vec<f64> = quad.split(',').filter_map(|n| n.parse().ok()).collect();
                match v.as_slice() {
                    [left, top, width, height]
                        if [left, top, width, height].iter().all(|n| n.is_finite()) =>
                    {
                        Some(PtRect::new(*left, *top, *width, *height))
                    }
                    _ => None,
                }
            })
            .collect()
    }

    /// Map of page occurrence id → 1-based displayed page number.
    fn page_numbers(doc: &AutoCommit) -> std::collections::HashMap<String, usize> {
        Self::list_page_plan(doc)
            .into_iter()
            .enumerate()
            .map(|(i, entry)| (entry.id, i + 1))
            .collect()
    }

    /// The displayed page an anchored entry is on: its occurrence's position,
    /// or `None` while that occurrence is deleted. Entries written before
    /// occurrence ids fall back to their stored ordinal.
    fn resolve_page(
        doc: &AutoCommit,
        entry: &ObjId,
        numbers: &std::collections::HashMap<String, usize>,
    ) -> Option<usize> {
        match read_str(doc, entry, "page-id") {
            Some(id) => numbers.get(&id).copied(),
            None => Some(Self::get_i64(doc, entry, "page").map_or(1, |p| p.max(1) as usize)),
        }
    }

    fn space_of(doc: &AutoCommit, entry: &ObjId) -> GeomSpace {
        if read_str(doc, entry, "geom-space").as_deref() == Some("user") {
            GeomSpace::User
        } else {
            GeomSpace::Page
        }
    }

    /// Add an annotation anchored to page occurrence `page_id`, or — for a
    /// caller that only knows a page number — to the occurrence currently at
    /// `a.page`.
    pub fn add_annotation(doc: &mut AutoCommit, a: &Annotation, page_id: Option<&str>) {
        if Self::locate(doc, "annotations", &a.id).is_some() {
            // A redelivered action envelope: the id is already present.
            return;
        }
        let page_id = page_id
            .map(str::to_string)
            .or_else(|| Self::page_id_for_number(doc, a.page));
        let Some(list) = Self::list(doc, "annotations") else {
            return;
        };
        let idx = doc.length(&list);
        let Ok(m) = doc.insert_object(&list, idx, ObjType::Map) else {
            return;
        };
        let _ = doc.put(&m, "id", a.id.as_str());
        let _ = doc.put(&m, "page", a.page as i64);
        if let Some(page_id) = page_id {
            let _ = doc.put(&m, "page-id", page_id);
        }
        let _ = doc.put(&m, "kind", a.kind.type_key());
        let _ = doc.put(&m, "color", a.color.key());
        let _ = doc.put(&m, "author", a.author.as_str());
        if a.created_ms != 0 {
            let _ = doc.put(&m, "created-ms", a.created_ms);
        }
        if a.space == GeomSpace::User {
            let _ = doc.put(&m, "geom-space", "user");
        }
        match &a.kind {
            AnnoKind::Markup { quads, .. } => {
                let _ = doc.put(&m, "quads", Self::rects_to_str(quads));
                if let Some((start, end)) = a.chars {
                    let _ = doc.put(&m, "chars", format!("{start},{end}"));
                }
            }
            AnnoKind::Note { x, y } => {
                let _ = doc.put(&m, "x", *x);
                let _ = doc.put(&m, "y", *y);
            }
            AnnoKind::Ink { strokes } => match strokes.as_slice() {
                [points] => {
                    let _ = doc.put(&m, "points", Self::points_to_str(points));
                }
                _ => {
                    let _ = doc.put(&m, "strokes", Self::strokes_to_str(strokes));
                }
            },
            AnnoKind::FreeText { rect, text, size } => {
                Self::put_rect(doc, &m, rect);
                let _ = doc.put(&m, "text", text.as_str());
                let _ = doc.put(&m, "font-size", *size);
            }
        }
    }

    fn put_rect(doc: &mut AutoCommit, m: &ObjId, rect: &PtRect) {
        let _ = doc.put(m, "x", rect.x);
        let _ = doc.put(m, "y", rect.y);
        let _ = doc.put(m, "w", rect.w);
        let _ = doc.put(m, "h", rect.h);
    }

    fn annotation_entry(doc: &AutoCommit, id: &str) -> Option<ObjId> {
        let (list, i) = Self::locate(doc, "annotations", id)?;
        match doc.get(&list, i) {
            Ok(Some((Value::Object(ObjType::Map), m))) => Some(m),
            _ => None,
        }
    }

    /// Replace a free text annotation's text.
    pub fn set_free_text(doc: &mut AutoCommit, id: &str, text: &str) {
        if let Some(m) = Self::annotation_entry(doc, id)
            && read_str(doc, &m, "kind").as_deref() == Some("freetext")
        {
            let _ = doc.put(&m, "text", text);
        }
    }

    /// Move or resize a free text box, and set its text size.
    pub fn set_free_text_box(
        doc: &mut AutoCommit,
        id: &str,
        rect: &PtRect,
        size: f64,
        space: GeomSpace,
    ) {
        if !size.is_finite()
            || size <= 0.0
            || [rect.x, rect.y, rect.w, rect.h]
                .iter()
                .any(|n| !n.is_finite())
        {
            return;
        }
        if let Some(m) = Self::annotation_entry(doc, id)
            && read_str(doc, &m, "kind").as_deref() == Some("freetext")
        {
            Self::put_rect(doc, &m, rect);
            let _ = doc.put(&m, "font-size", size);
            if space == GeomSpace::User {
                let _ = doc.put(&m, "geom-space", "user");
            } else {
                let _ = doc.delete(&m, "geom-space");
            }
        }
    }

    fn read_annotation(
        doc: &AutoCommit,
        m: &ObjId,
        numbers: &std::collections::HashMap<String, usize>,
    ) -> (Option<usize>, Annotation) {
        let id = read_str(doc, m, "id").unwrap_or_default();
        let page = Self::resolve_page(doc, m, numbers);
        let color = Swatch::parse(&read_str(doc, m, "color").unwrap_or_default());
        let author = read_str(doc, m, "author").unwrap_or_default();
        let kind_key = read_str(doc, m, "kind").unwrap_or_default();
        let kind = if let Some(style) = MarkupStyle::parse(&kind_key) {
            let quads = read_str(doc, m, "quads").map_or_else(
                || {
                    // Records from before multi-band markup: one rectangle.
                    vec![PtRect::new(
                        read_number(doc, m, "x").unwrap_or(0.0),
                        read_number(doc, m, "y").unwrap_or(0.0),
                        read_number(doc, m, "w").unwrap_or(0.0),
                        read_number(doc, m, "h").unwrap_or(0.0),
                    )]
                },
                |s| Self::rects_from_str(&s),
            );
            AnnoKind::Markup { style, quads }
        } else if kind_key == "ink" {
            let strokes = match read_str(doc, m, "strokes") {
                Some(s) => Self::strokes_from_str(&s),
                None => vec![Self::points_from_str(
                    &read_str(doc, m, "points").unwrap_or_default(),
                )],
            };
            AnnoKind::Ink { strokes }
        } else if kind_key == "freetext" {
            AnnoKind::FreeText {
                rect: PtRect::new(
                    read_number(doc, m, "x").unwrap_or(0.0),
                    read_number(doc, m, "y").unwrap_or(0.0),
                    read_number(doc, m, "w").unwrap_or(0.0),
                    read_number(doc, m, "h").unwrap_or(0.0),
                ),
                text: read_str(doc, m, "text").unwrap_or_default(),
                size: read_number(doc, m, "font-size")
                    .filter(|s| s.is_finite() && *s > 0.0)
                    .unwrap_or(crate::model::FREE_TEXT_SIZE),
            }
        } else {
            AnnoKind::Note {
                x: read_number(doc, m, "x").unwrap_or(0.0),
                y: read_number(doc, m, "y").unwrap_or(0.0),
            }
        };
        let chars = read_str(doc, m, "chars").and_then(|s| {
            let (a, b) = s.split_once(',')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        });
        (
            page,
            Annotation {
                id,
                page: page.unwrap_or(0),
                kind,
                color,
                author,
                chars,
                created_ms: Self::get_i64(doc, m, "created-ms").unwrap_or(0),
                space: Self::space_of(doc, m),
            },
        )
    }

    /// Annotations on pages that are currently part of the document.
    /// Annotations anchored to a deleted page stay in the record but are not
    /// listed until the page comes back.
    pub fn list_annotations(doc: &AutoCommit) -> Vec<Annotation> {
        Self::list_annotations_with_hidden(doc)
            .into_iter()
            .filter(|a| a.page > 0)
            .collect()
    }

    /// Every annotation in the record, those anchored to a deleted page with
    /// `page == 0`.
    pub fn list_annotations_with_hidden(doc: &AutoCommit) -> Vec<Annotation> {
        let numbers = Self::page_numbers(doc);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for m in Self::entries(doc, "annotations") {
            let (_, annotation) = Self::read_annotation(doc, &m, &numbers);
            if seen.insert(annotation.id.clone()) {
                out.push(annotation);
            }
        }
        out
    }

    /// The page occurrence an annotation is anchored to.
    pub fn annotation_page_id(doc: &AutoCommit, id: &str) -> Option<String> {
        let (list, index) = Self::locate(doc, "annotations", id)?;
        let (_, entry) = doc.get(&list, index).ok()??;
        read_str(doc, &entry, "page-id")
    }

    fn index_of(doc: &AutoCommit, list: &ObjId, id: &str) -> Option<usize> {
        for i in 0..doc.length(list) {
            if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(list, i)
                && read_str(doc, &m, "id").as_deref() == Some(id)
            {
                return Some(i);
            }
        }
        None
    }

    pub fn set_annotation_color(doc: &mut AutoCommit, id: &str, color: Swatch) {
        if let Some((list, i)) = Self::locate(doc, "annotations", id)
            && let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
        {
            let _ = doc.put(&m, "color", color.key());
        }
    }

    /// Move an annotation's anchor to `(x, y)` in page-point space. Used by the
    /// draggable annotation markers — repositioning a sticky note. Highlights
    /// keep their width/height and shift; notes carry only a point. A no-op for
    /// ink (its geometry is the polyline, not a single anchor).
    pub fn set_annotation_position(
        doc: &mut AutoCommit,
        id: &str,
        x: f64,
        y: f64,
        space: GeomSpace,
    ) {
        if let Some((list, i)) = Self::locate(doc, "annotations", id)
            && let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
            && read_str(doc, &m, "kind").as_deref() == Some("note")
        {
            let _ = doc.put(&m, "x", x);
            let _ = doc.put(&m, "y", y);
            if space == GeomSpace::User {
                let _ = doc.put(&m, "geom-space", "user");
            } else {
                let _ = doc.delete(&m, "geom-space");
            }
        }
    }

    pub fn delete_annotation(doc: &mut AutoCommit, id: &str) {
        // Every copy: replicas that took over the same markup concurrently
        // each inserted the record under its source-derived id.
        for list in Self::roots(doc, "annotations", ObjType::List) {
            Self::remove_from_list(doc, &list, id);
        }
        // Any comments anchored to this annotation are removed with it so a
        // deleted note never leaves a dangling thread behind in the rail.
        for comments in Self::roots(doc, "comments", ObjType::List) {
            let mut i = 0;
            while i < doc.length(&comments) {
                let anchored = matches!(
                    doc.get(&comments, i),
                    Ok(Some((Value::Object(ObjType::Map), m)))
                        if read_str(doc, &m, "anno").as_deref() == Some(id)
                );
                if anchored {
                    let _ = doc.delete(&comments, i);
                } else {
                    i += 1;
                }
            }
        }
    }

    // ---- comments ---------------------------------------------------

    pub fn add_comment(doc: &mut AutoCommit, c: &Comment) {
        if Self::locate(doc, "comments", &c.id).is_some() {
            return;
        }
        let Some(list) = Self::list(doc, "comments") else {
            return;
        };
        let idx = doc.length(&list);
        let Ok(m) = doc.insert_object(&list, idx, ObjType::Map) else {
            return;
        };
        let _ = doc.put(&m, "id", c.id.as_str());
        if let Some(a) = &c.anno_id {
            let _ = doc.put(&m, "anno", a.as_str());
        }
        let _ = doc.put(&m, "page", c.page as i64);
        // A comment on an annotation follows the annotation's page.
        let page_id = c
            .anno_id
            .as_deref()
            .and_then(|a| Self::annotation_page_id(doc, a))
            .or_else(|| Self::page_id_for_number(doc, c.page));
        if let Some(page_id) = page_id {
            let _ = doc.put(&m, "page-id", page_id);
        }
        let _ = doc.put(&m, "author", c.author.as_str());
        Self::write_text(doc, &m, &tilo_rich_text::plain_clips(&c.body));
        let _ = doc.put(&m, "created", c.created);
        let _ = doc.put(&m, "resolved", c.resolved);
        let Ok(replies) = doc.put_object(&m, "replies", ObjType::List) else {
            return;
        };
        for (ri, r) in c.replies.iter().enumerate() {
            let Ok(rm) = doc.insert_object(&replies, ri, ObjType::Map) else {
                continue;
            };
            let _ = doc.put(&rm, "id", r.id.as_str());
            let _ = doc.put(&rm, "author", r.author.as_str());
            Self::write_text(doc, &rm, &tilo_rich_text::plain_clips(&r.body));
        }
    }

    /// Write a comment or reply's text as rich text under its [`TEXT`] key,
    /// replacing what it held.
    fn write_text(doc: &mut AutoCommit, entry: &ObjId, clips: &[tilo_document::ClipBlock]) {
        if let Err(error) = tilo_rich_text::put_rich_text(doc, entry, TEXT, clips) {
            tracing::warn!(%error, "comment text not written");
        }
    }

    /// A comment or reply's text: its plain text and its paragraphs.
    fn read_text(doc: &AutoCommit, entry: &ObjId) -> (String, Vec<ui::widget::BlockContent>) {
        if let Some(root) = tilo_rich_text::rich_text_at(doc, entry, TEXT) {
            return (
                tilo_document::RichText::plain_text_under(doc, &root).unwrap_or_default(),
                tilo_rich_text::blocks_under(doc, &root).unwrap_or_default(),
            );
        }
        (String::new(), Vec::new())
    }

    pub fn list_comments(doc: &AutoCommit) -> Vec<Comment> {
        let numbers = Self::page_numbers(doc);
        let mut seen_ids = HashSet::new();
        let mut out = Vec::new();
        for m in Self::entries(doc, "comments") {
            if !read_str(doc, &m, "id").is_some_and(|id| seen_ids.insert(id)) {
                continue;
            }
            let id = read_str(doc, &m, "id").unwrap_or_default();
            let anno_id = read_str(doc, &m, "anno");
            let Some(page) = Self::resolve_page(doc, &m, &numbers) else {
                // On a deleted page: kept, but hidden until it returns.
                continue;
            };
            let author = read_str(doc, &m, "author").unwrap_or_default();
            let (body, blocks) = Self::read_text(doc, &m);
            let created = Self::get_i64(doc, &m, "created").unwrap_or(0);
            let resolved = read_bool(doc, &m, "resolved").unwrap_or(false);
            let mut replies = Vec::new();
            if let Ok(Some((Value::Object(ObjType::List), rl))) = doc.get(&m, "replies") {
                let mut seen_reply_ids = HashSet::new();
                for j in 0..doc.length(&rl) {
                    if let Ok(Some((Value::Object(ObjType::Map), rm))) = doc.get(&rl, j) {
                        let Some(id) = read_str(doc, &rm, "id") else {
                            continue;
                        };
                        if !seen_reply_ids.insert(id.clone()) {
                            continue;
                        }
                        let (body, blocks) = Self::read_text(doc, &rm);
                        replies.push(Reply {
                            id,
                            author: read_str(doc, &rm, "author").unwrap_or_default(),
                            body,
                            blocks,
                        });
                    }
                }
            }
            out.push(Comment {
                id,
                anno_id,
                page,
                author,
                body,
                created,
                resolved,
                replies,
                blocks,
            });
        }
        out.sort_by_key(|c| c.created);
        out
    }

    pub fn set_comment_resolved(doc: &mut AutoCommit, id: &str, resolved: bool) {
        if let Some((list, i)) = Self::locate(doc, "comments", id)
            && let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
        {
            let _ = doc.put(&m, "resolved", resolved);
        }
    }

    /// Replace a comment's text with `clips` (its paragraphs as written).
    pub fn set_comment_text(doc: &mut AutoCommit, id: &str, clips: &[tilo_document::ClipBlock]) {
        if let Some((list, i)) = Self::locate(doc, "comments", id)
            && let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
        {
            Self::write_text(doc, &m, clips);
        }
    }

    pub fn delete_comment(doc: &mut AutoCommit, id: &str) {
        if let Some((list, i)) = Self::locate(doc, "comments", id) {
            let _ = doc.delete(&list, i);
        }
    }

    /// Append `reply`, its text written as `clips` (its paragraphs as
    /// written; none writes its plain `body`), to the thread of `comment_id`.
    pub fn add_reply(
        doc: &mut AutoCommit,
        comment_id: &str,
        reply: &Reply,
        clips: &[tilo_document::ClipBlock],
    ) {
        if reply.id.trim().is_empty() {
            return;
        }
        if let Some((list, i)) = Self::locate(doc, "comments", comment_id)
            && let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
        {
            let replies = match doc.get(&m, "replies") {
                Ok(Some((Value::Object(ObjType::List), rl))) => Some(rl),
                _ => doc.put_object(&m, "replies", ObjType::List).ok(),
            };
            let Some(replies) = replies else {
                return;
            };
            // Action envelopes can be delivered more than once after a retry
            // or peer merge.  The durable reply id makes this idempotent.
            for index in 0..doc.length(&replies) {
                if let Ok(Some((Value::Object(ObjType::Map), existing))) = doc.get(&replies, index)
                    && read_str(doc, &existing, "id").as_deref() == Some(reply.id.as_str())
                {
                    return;
                }
            }
            let idx = doc.length(&replies);
            let Ok(rm) = doc.insert_object(&replies, idx, ObjType::Map) else {
                return;
            };
            let _ = doc.put(&rm, "id", reply.id.as_str());
            let _ = doc.put(&rm, "author", reply.author.as_str());
            if clips.is_empty() {
                Self::write_text(doc, &rm, &tilo_rich_text::plain_clips(&reply.body));
            } else {
                Self::write_text(doc, &rm, clips);
            }
        }
    }

    // ---- signatures -------------------------------------------------

    /// Serialise freehand signature strokes to a string: points within a stroke
    /// are `x,y` pairs joined by spaces (reusing [`Self::points_to_str`]),
    /// strokes joined by `;`.
    fn strokes_to_str(strokes: &[Vec<(f64, f64)>]) -> String {
        strokes
            .iter()
            .map(|s| Self::points_to_str(s))
            .collect::<Vec<_>>()
            .join(";")
    }

    fn strokes_from_str(s: &str) -> Vec<Vec<(f64, f64)>> {
        s.split(';')
            .map(Self::points_from_str)
            .filter(|s| !s.is_empty())
            .collect()
    }

    pub fn add_signature(doc: &mut AutoCommit, s: &Signature, page_id: Option<&str>) {
        let page_id = page_id
            .map(str::to_string)
            .or_else(|| Self::page_id_for_number(doc, s.page));
        if Self::locate(doc, "signatures", &s.id).is_some() {
            return;
        }
        let Some(list) = Self::list(doc, "signatures") else {
            return;
        };
        let idx = doc.length(&list);
        let Ok(m) = doc.insert_object(&list, idx, ObjType::Map) else {
            return;
        };
        let _ = doc.put(&m, "id", s.id.as_str());
        let _ = doc.put(&m, "page", s.page as i64);
        if let Some(page_id) = page_id {
            let _ = doc.put(&m, "page-id", page_id);
        }
        if s.space == GeomSpace::User {
            let _ = doc.put(&m, "geom-space", "user");
        }
        let _ = doc.put(&m, "x", s.rect.x);
        let _ = doc.put(&m, "y", s.rect.y);
        let _ = doc.put(&m, "w", s.rect.w);
        let _ = doc.put(&m, "h", s.rect.h);
        let _ = doc.put(&m, "signer", s.signer.as_str());
        let _ = doc.put(&m, "date", s.date.as_str());
        match &s.ink {
            crate::model::SignatureInk::Typed => {
                let _ = doc.put(&m, "ink", "typed");
            }
            crate::model::SignatureInk::Drawn { strokes } => {
                let _ = doc.put(&m, "ink", "drawn");
                let _ = doc.put(&m, "strokes", Self::strokes_to_str(strokes));
            }
        }
    }

    pub fn list_signatures(doc: &AutoCommit) -> Vec<Signature> {
        let numbers = Self::page_numbers(doc);
        let mut seen_ids = HashSet::new();
        let mut out = Vec::new();
        for m in Self::entries(doc, "signatures") {
            if !read_str(doc, &m, "id").is_some_and(|id| seen_ids.insert(id)) {
                continue;
            }
            // Signatures written before real ink existed carry no "ink" field;
            // they render as a typed-name signature (no fabricated squiggle).
            let ink = match read_str(doc, &m, "ink").as_deref() {
                Some("drawn") => crate::model::SignatureInk::Drawn {
                    strokes: Self::strokes_from_str(
                        &read_str(doc, &m, "strokes").unwrap_or_default(),
                    ),
                },
                _ => crate::model::SignatureInk::Typed,
            };
            let Some(page) = Self::resolve_page(doc, &m, &numbers) else {
                continue;
            };
            out.push(Signature {
                id: read_str(doc, &m, "id").unwrap_or_default(),
                page,
                space: Self::space_of(doc, &m),
                rect: PtRect::new(
                    read_number(doc, &m, "x").unwrap_or(0.0),
                    read_number(doc, &m, "y").unwrap_or(0.0),
                    read_number(doc, &m, "w").unwrap_or(180.0),
                    read_number(doc, &m, "h").unwrap_or(56.0),
                ),
                signer: read_str(doc, &m, "signer").unwrap_or_default(),
                date: read_str(doc, &m, "date").unwrap_or_default(),
                ink,
            });
        }
        out
    }

    pub fn delete_signature(doc: &mut AutoCommit, id: &str) {
        if let Some((list, i)) = Self::locate(doc, "signatures", id) {
            let _ = doc.delete(&list, i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AnnoKind, Annotation, Comment, GeomSpace, PtRect, Reply, Signature, Swatch,
    };
    use tilo_automerge::ApplyIncoming;

    fn anno(page: usize, kind: AnnoKind, color: Swatch, author: &str) -> Annotation {
        Annotation {
            id: Annotation::new_id(),
            page,
            kind,
            color,
            author: author.into(),
            chars: None,
            created_ms: 0,
            space: crate::model::GeomSpace::Page,
        }
    }

    fn hl(page: usize) -> Annotation {
        anno(
            page,
            AnnoKind::highlight(PtRect::new(10.0, 20.0, 100.0, 14.0)),
            Swatch::Yellow,
            "Alice",
        )
    }

    fn comment(id: &str, anno_id: Option<&str>, author: &str, body: &str, created: i64) -> Comment {
        Comment {
            id: id.into(),
            anno_id: anno_id.map(Into::into),
            page: 1,
            author: author.into(),
            body: body.into(),
            created,
            resolved: false,
            replies: Vec::new(),
            blocks: Vec::new(),
        }
    }

    fn reply(id: &str, author: &str, body: &str) -> Reply {
        Reply {
            id: id.into(),
            author: author.into(),
            body: body.into(),
            blocks: Vec::new(),
        }
    }

    fn signature(
        id: &str,
        page: usize,
        rect: PtRect,
        signer: &str,
        date: &str,
        ink: crate::model::SignatureInk,
    ) -> Signature {
        Signature {
            id: id.into(),
            page,
            rect,
            signer: signer.into(),
            date: date.into(),
            ink,
            space: crate::model::GeomSpace::Page,
        }
    }

    #[test]
    fn a_comment_written_rich_keeps_its_marks_and_plain_text() {
        let mut doc = AutoCommit::new();
        PdfDoc::add_comment(&mut doc, &comment("c", None, "Ada", "draft", 1));
        let mut bold = ui::widget::BlockContent::paragraph("", "plain bold");
        bold.marks = vec![ui::widget::MarkSpan::new(
            6..10,
            ui::widget::marks::Mark::Bold,
        )];
        let clips = tilo_rich_text::Draft::with_blocks(&[bold]).clip_blocks();
        PdfDoc::set_comment_text(&mut doc, "c", &clips);
        let got = &PdfDoc::list_comments(&doc)[0];
        assert_eq!(got.body, "plain bold");
        assert_eq!(got.blocks.len(), 1);
        assert_eq!(got.blocks[0].marks.len(), 1);
    }

    #[test]
    fn annotations_round_trip_recolor_reposition_and_delete() {
        let mut doc = AutoCommit::new();
        let a = hl(2);
        PdfDoc::add_annotation(&mut doc, &a, None);
        let list = PdfDoc::list_annotations(&doc);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].page, 2);
        assert_eq!(list[0].color, Swatch::Yellow);
        match &list[0].kind {
            AnnoKind::Markup { quads, .. } => assert!((quads[0].w - 100.0).abs() < 1e-6),
            _ => panic!("expected highlight"),
        }
        PdfDoc::set_annotation_color(&mut doc, &a.id, Swatch::Green);
        assert_eq!(PdfDoc::list_annotations(&doc)[0].color, Swatch::Green);
        PdfDoc::delete_annotation(&mut doc, &a.id);
        assert!(PdfDoc::list_annotations(&doc).is_empty());

        // Ink points round-trip exactly.
        let mut doc = AutoCommit::new();
        let ink = anno(
            1,
            AnnoKind::ink(vec![(1.0, 2.0), (3.5, 4.25), (10.0, 0.0)]),
            Swatch::Blue,
            "Bob",
        );
        PdfDoc::add_annotation(&mut doc, &ink, None);
        match &PdfDoc::list_annotations(&doc)[0].kind {
            AnnoKind::Ink { strokes } => {
                let points = &strokes[0];
                assert_eq!(points.len(), 3);
                assert!((points[1].0 - 3.5).abs() < 1e-6);
                assert!((points[1].1 - 4.25).abs() < 1e-6);
            }
            _ => panic!("expected ink"),
        }

        // Repositioning moves a note marker ...
        let mut doc = AutoCommit::new();
        let note = anno(
            1,
            AnnoKind::Note { x: 30.0, y: 40.0 },
            Swatch::Blue,
            "Alice",
        );
        PdfDoc::add_annotation(&mut doc, &note, None);
        PdfDoc::set_annotation_position(&mut doc, &note.id, 120.5, 210.25, GeomSpace::Page);
        match &PdfDoc::list_annotations(&doc)[0].kind {
            AnnoKind::Note { x, y } => {
                assert!((x - 120.5).abs() < 1e-6, "note x moved");
                assert!((y - 210.25).abs() < 1e-6, "note y moved");
            }
            _ => panic!("expected note"),
        }

        // ... but ink is a polyline, not a single anchor — repositioning must be a no-op.
        let mut doc = AutoCommit::new();
        let ink = anno(
            1,
            AnnoKind::ink(vec![(1.0, 2.0), (3.0, 4.0)]),
            Swatch::Blue,
            "Bob",
        );
        PdfDoc::add_annotation(&mut doc, &ink, None);
        PdfDoc::set_annotation_position(&mut doc, &ink.id, 99.0, 99.0, GeomSpace::Page);
        match &PdfDoc::list_annotations(&doc)[0].kind {
            AnnoKind::Ink { strokes } => assert_eq!(strokes[0].len(), 2, "ink polyline preserved"),
            _ => panic!("expected ink"),
        }
    }

    #[test]
    fn comment_threads_round_trip_sort_and_keep_reply_ids_through_merges() {
        let mut doc = AutoCommit::new();
        let mut c = comment(
            &Comment::new_id(),
            Some("anno-1"),
            "Alice",
            "Is this still accurate?",
            5,
        );
        c.replies = vec![reply(&Reply::new_id(), "Bob", "Yes, verified.")];
        PdfDoc::add_comment(&mut doc, &c);
        let got = PdfDoc::list_comments(&doc);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].anno_id.as_deref(), Some("anno-1"));
        assert_eq!(got[0].replies.len(), 1);
        assert_eq!(got[0].replies[0].author, "Bob");
        assert_eq!(got[0].replies[0].id, c.replies[0].id);
        PdfDoc::add_reply(
            &mut doc,
            &c.id,
            &reply(&Reply::new_id(), "Carol", "Agreed."),
            &tilo_rich_text::plain_clips("Agreed."),
        );
        PdfDoc::set_comment_resolved(&mut doc, &c.id, true);
        PdfDoc::set_comment_text(
            &mut doc,
            &c.id,
            &tilo_rich_text::plain_clips("Edited: confirmed accurate."),
        );
        let got = PdfDoc::list_comments(&doc);
        assert_eq!(got[0].replies.len(), 2);
        assert!(got[0].resolved);
        assert_eq!(got[0].body, "Edited: confirmed accurate.");

        // Comments list in creation order.
        let mut doc = AutoCommit::new();
        for (id, created) in [("a", 30), ("b", 10), ("c", 20)] {
            PdfDoc::add_comment(&mut doc, &comment(id, None, "x", "y", created));
        }
        let order: Vec<i64> = PdfDoc::list_comments(&doc)
            .iter()
            .map(|c| c.created)
            .collect();
        assert_eq!(order, vec![10, 20, 30]);

        // Reply ids survive reload, retries and concurrent merges.
        let mut base = AutoCommit::new();
        let thread = comment("thread-1", None, "Ada", "Please review this.", 1);
        PdfDoc::add_comment(&mut base, &thread);
        let bytes = base.save();
        let mut left = AutoCommit::load(&bytes).unwrap();
        let mut right = AutoCommit::load(&bytes).unwrap();
        let left_reply = reply("reply-left", "Grace", "Looks good.");
        PdfDoc::add_reply(&mut left, &thread.id, &left_reply, &[]);
        // A retried delivery has the same id and must remain exactly one reply.
        PdfDoc::add_reply(&mut left, &thread.id, &left_reply, &[]);
        PdfDoc::add_reply(
            &mut right,
            &thread.id,
            &reply("reply-right", "Lin", "One small change."),
            &tilo_rich_text::plain_clips("One small change."),
        );
        left.merge_incoming(&mut right).unwrap();
        let replies = &PdfDoc::list_comments(&left)[0].replies;
        assert_eq!(replies.len(), 2);
        assert!(replies.iter().any(|reply| reply.id == "reply-left"));
        assert!(replies.iter().any(|reply| reply.id == "reply-right"));
        let reloaded = AutoCommit::load(&left.save()).unwrap();
        let reloaded_comments = PdfDoc::list_comments(&reloaded);
        let ids: Vec<&str> = reloaded_comments[0]
            .replies
            .iter()
            .map(|reply| reply.id.as_str())
            .collect();
        assert!(ids.contains(&"reply-left") && ids.contains(&"reply-right"));
    }

    #[test]
    fn signatures_round_trip_and_deletes_cascade_only_to_anchored_comments() {
        use crate::model::SignatureInk;
        let mut doc = AutoCommit::new();
        assert!(!PdfDoc::is_seeded(&mut doc));
        PdfDoc::set_meta(&mut doc, "T", "A");
        assert!(PdfDoc::is_seeded(&mut doc));

        let typed = signature(
            &Signature::new_id(),
            5,
            PtRect::new(360.0, 640.0, 180.0, 56.0),
            "Alice Daskju",
            "2026-06-30",
            SignatureInk::Typed,
        );
        PdfDoc::add_signature(&mut doc, &typed, None);
        let got = PdfDoc::list_signatures(&doc);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].signer, "Alice Daskju");
        assert_eq!(got[0].page, 5);
        assert_eq!(got[0].ink, SignatureInk::Typed);
        PdfDoc::delete_signature(&mut doc, &typed.id);
        assert!(PdfDoc::list_signatures(&doc).is_empty());

        let strokes = vec![
            vec![(0.1, 0.5), (0.3, 0.2), (0.5, 0.6)],
            vec![(0.6, 0.4), (0.9, 0.5)],
        ];
        let drawn = signature(
            &Signature::new_id(),
            2,
            PtRect::new(100.0, 100.0, 200.0, 70.0),
            "Grace",
            "2026-08-17",
            SignatureInk::Drawn { strokes },
        );
        PdfDoc::add_signature(&mut doc, &drawn, None);
        let got = PdfDoc::list_signatures(&doc);
        assert_eq!(got.len(), 1);
        match &got[0].ink {
            SignatureInk::Drawn { strokes: back } => {
                assert_eq!(back.len(), 2, "both strokes survive");
                assert_eq!(back[0].len(), 3);
                assert!((back[0][1].0 - 0.3).abs() < 1e-2 && (back[0][1].1 - 0.2).abs() < 1e-2);
                assert_eq!(back[1].len(), 2);
            }
            SignatureInk::Typed => panic!("expected drawn ink, got Typed"),
        }

        // Deleting a free comment removes it.
        let mut doc = AutoCommit::new();
        let free = comment(&Comment::new_id(), None, "Alice", "x", 1);
        PdfDoc::add_comment(&mut doc, &free);
        PdfDoc::delete_comment(&mut doc, &free.id);
        assert!(PdfDoc::list_comments(&doc).is_empty());

        // Deleting an annotation removes its anchored comments; an unrelated
        // free comment must survive the delete.
        let mut doc = AutoCommit::new();
        let a = hl(1);
        PdfDoc::add_annotation(&mut doc, &a, None);
        PdfDoc::add_comment(
            &mut doc,
            &comment(
                &Comment::new_id(),
                Some(a.id.as_str()),
                "Alice",
                "pinned",
                1,
            ),
        );
        PdfDoc::add_comment(
            &mut doc,
            &comment(&Comment::new_id(), None, "Bob", "free", 2),
        );
        assert_eq!(PdfDoc::list_comments(&doc).len(), 2);
        PdfDoc::delete_annotation(&mut doc, &a.id);
        assert!(PdfDoc::list_annotations(&doc).is_empty());
        let left = PdfDoc::list_comments(&doc);
        assert_eq!(
            left.len(),
            1,
            "anchored comment should be deleted with its annotation"
        );
        assert_eq!(left[0].body, "free");
    }

    #[test]
    fn page_plan_converges_imports_atomically_and_carries_markup() {
        let base = AutoCommit::new().save();
        let mut left = AutoCommit::load(&base).unwrap();
        let mut right = AutoCommit::load(&base).unwrap();

        // Both clients open the same uninitialised record before either sees
        // the other. Their physical list insertions may both survive merge,
        // but the materialised plan has one deterministic occurrence per id.
        PdfDoc::ensure_page_plan(&mut left, 3);
        PdfDoc::ensure_page_plan(&mut right, 3);
        left.merge_incoming(&mut right).unwrap();
        let initial = PdfDoc::list_page_plan(&left);
        assert_eq!(initial.len(), 3);
        assert_eq!(
            initial
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec!["source-1", "source-2", "source-3"]
        );

        let checkpoint = left.save();
        let mut a = AutoCommit::load(&checkpoint).unwrap();
        let mut b = AutoCommit::load(&checkpoint).unwrap();
        PdfDoc::move_page_plan(&mut a, "source-2", None);
        PdfDoc::move_page_plan(&mut b, "source-2", Some("source-3"));
        a.merge_incoming(&mut b).unwrap();
        let plan = PdfDoc::list_page_plan(&a);
        assert!(!plan.is_empty());
        let ids: HashSet<&str> = plan.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids.len(), plan.len(), "each durable id materialises once");
        assert_eq!(ids.len(), 3);
        b.merge_incoming(&mut a).unwrap();
        assert_eq!(PdfDoc::list_page_plan(&a), PdfDoc::list_page_plan(&b));

        // Markup anchors follow a page occurrence through move and delete.
        let mut doc = AutoCommit::new();
        PdfDoc::ensure_page_plan(&mut doc, 3);
        let annotation = hl(1);
        PdfDoc::add_annotation(&mut doc, &annotation, None);
        PdfDoc::add_comment(
            &mut doc,
            &comment(
                "comment-on-source-1",
                Some(annotation.id.as_str()),
                "Alice",
                "attached",
                1,
            ),
        );
        PdfDoc::add_signature(
            &mut doc,
            &signature(
                "signature-on-source-1",
                1,
                PtRect::new(10.0, 10.0, 100.0, 40.0),
                "Alice",
                "2026-01-01",
                crate::model::SignatureInk::Typed,
            ),
            None,
        );
        PdfDoc::move_page_plan(&mut doc, "source-1", Some("source-3"));
        assert_eq!(PdfDoc::list_annotations(&doc)[0].page, 3);
        assert_eq!(PdfDoc::list_comments(&doc)[0].page, 3);
        assert_eq!(PdfDoc::list_signatures(&doc)[0].page, 3);
        PdfDoc::delete_page_plan(&mut doc, "source-1");
        assert!(PdfDoc::list_annotations(&doc).is_empty());
        assert!(PdfDoc::list_comments(&doc).is_empty());
        assert!(PdfDoc::list_signatures(&doc).is_empty());
        // The markup stays in the record, attached to the deleted page.
        let hidden = PdfDoc::list_annotations_with_hidden(&doc);
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].page, 0);
        assert_eq!(
            PdfDoc::annotation_page_id(&doc, &hidden[0].id).as_deref(),
            Some("source-1")
        );

        // Imported pages are atomic and keep their durable identity.
        let mut doc = AutoCommit::new();
        PdfDoc::ensure_page_plan(&mut doc, 2);
        let page = PagePlanEntry {
            id: "imported-image".into(),
            source_page: 1,
            imported: Some(ImportedPage {
                hash: "a".repeat(64),
                kind: "image".into(),
                page: 1,
                width_milli: 612_000,
                height_milli: 408_000,
            }),
            rotation: 0,
        };
        PdfDoc::import_pages(&mut doc, Some("source-1"), std::slice::from_ref(&page));
        assert_eq!(PdfDoc::list_page_plan(&doc)[1], page);
        // A malformed batch refuses as a whole, including the otherwise valid
        // first entry. It cannot leave a partial shared page plan behind.
        let mut bad = page.clone();
        bad.id = "bad".into();
        bad.imported.as_mut().unwrap().hash = "tampered".into();
        PdfDoc::import_pages(&mut doc, None, &[page, bad]);
        assert_eq!(PdfDoc::list_page_plan(&doc).len(), 3);
    }
}
