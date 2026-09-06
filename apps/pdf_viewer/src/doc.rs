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
//! ├── "meta" (Map) { title, author, seeded:bool }
//! ├── "page-plan" (List of Map) { id, source-page }
//! ├── "annotations" (List of Map)
//! │     { id, page, kind, color, author, x, y, w, h, points }
//! ├── "comments" (List of Map)
//! │     { id, anno, page, author, body, created, resolved, replies:List }
//! └── "signatures" (List of Map)
//!       { id, page, x, y, w, h, signer, date, ink, strokes }
//! ```
//!
//! View state (current page, zoom, active tool) is deliberately NOT here —
//! it is per-peer, so two reviewers can look at different pages of the
//! same shared markup.

use std::collections::HashSet;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{AnnoKind, Annotation, Comment, PtRect, Reply, Signature, Swatch};

/// A durable page-plan entry. `source_page` names the immutable original page
/// in the opened PDF; `id` names this occurrence (duplicates therefore get a
/// new id while retaining the same source page).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PagePlanEntry {
    pub id: String,
    pub source_page: usize,
    /// `None` denotes an immutable page of the file which was opened.  A
    /// populated value names bytes in qdoc's content-addressed store; bytes
    /// never enter Automerge.
    pub imported: Option<ImportedPage>,
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

    fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_f64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<f64> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::F64(f) => Some(*f),
                ScalarValue::Int(i) => Some(*i as f64),
                ScalarValue::Uint(u) => Some(*u as f64),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Int(i) => Some(*i),
                ScalarValue::Uint(u) => Some(*u as i64),
                _ => None,
            },
            _ => None,
        }
    }

    fn get_bool(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<bool> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Boolean(b) => Some(*b),
                _ => None,
            },
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

    fn list_ro(doc: &AutoCommit, key: &str) -> Option<ObjId> {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
            _ => None,
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
        Self::get_bool(doc, &m, "seeded").unwrap_or(false)
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
        if Self::get_bool(doc, &meta, "page-plan-seeded").unwrap_or(false) {
            return;
        }

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
        let Some(list) = Self::list_ro(doc, "page-plan") else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        (0..doc.length(&list))
            .filter_map(|index| {
                let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&list, index) else {
                    return None;
                };
                let id = Self::get_str(doc, &entry, "id")?;
                let source_page = Self::get_i64(doc, &entry, "source-page")? as usize;
                let imported = match Self::get_str(doc, &entry, "content-hash") {
                    Some(hash) => Some(ImportedPage {
                        hash,
                        kind: Self::get_str(doc, &entry, "content-kind")?,
                        page: Self::get_i64(doc, &entry, "content-page")? as usize,
                        width_milli: Self::get_i64(doc, &entry, "page-width-milli")? as u32,
                        height_milli: Self::get_i64(doc, &entry, "page-height-milli")? as u32,
                    }),
                    None => None,
                };
                (source_page > 0
                    && imported.as_ref().is_none_or(ImportedPage::valid)
                    && seen.insert(id.clone()))
                .then_some(PagePlanEntry {
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

    fn page_number_for_id(doc: &AutoCommit, id: &str) -> Option<usize> {
        Self::list_page_plan(doc)
            .iter()
            .position(|entry| entry.id == id)
            .map(|index| index + 1)
    }

    /// Upgrade ordinal-only anchors from records written before durable page
    /// occurrences existed. This is schema hydration, not a user edit: it runs
    /// once after the deterministic source plan is available and preserves the
    /// page each legacy anchor originally denoted.
    pub fn migrate_page_anchors(doc: &mut AutoCommit) {
        for key in ["annotations", "comments", "signatures"] {
            let Some(list) = Self::list(doc, key) else {
                continue;
            };
            for index in 0..doc.length(&list) {
                let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&list, index) else {
                    continue;
                };
                if Self::get_str(doc, &entry, "page-id").is_some() {
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
        let Some(list) = Self::list(doc, "page-plan") else {
            return;
        };
        if Self::index_of(doc, &list, new_id).is_some() {
            return;
        }
        let Some(index) = Self::index_of(doc, &list, id) else {
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
        let Some(list) = Self::list(doc, "page-plan") else {
            return;
        };
        if pages
            .iter()
            .any(|entry| Self::index_of(doc, &list, &entry.id).is_some())
        {
            return;
        }
        let mut seen = HashSet::new();
        if pages.iter().any(|entry| !seen.insert(&entry.id)) {
            return;
        }
        let at = after_id
            .and_then(|id| Self::index_of(doc, &list, id).map(|i| i + 1))
            .unwrap_or_else(|| doc.length(&list));
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
        let Some(list) = Self::list(doc, "page-plan") else {
            return;
        };
        if Self::list_page_plan(doc).len() <= 1 {
            return;
        }
        for index in (0..doc.length(&list)).rev() {
            let is_target = doc
                .get(&list, index)
                .ok()
                .flatten()
                .and_then(|(value, entry)| {
                    matches!(value, Value::Object(ObjType::Map))
                        .then(|| Self::get_str(doc, &entry, "id"))
                        .flatten()
                })
                .is_some_and(|candidate| candidate == id);
            if is_target {
                let _ = doc.delete(&list, index);
            }
        }
        Self::delete_page_anchors(doc, id);
    }

    /// Deleting an occurrence removes all markup attached specifically to it.
    /// A duplicate deliberately duplicates only page content, never review
    /// markup; users can add independent annotations to the new occurrence.
    fn delete_page_anchors(doc: &mut AutoCommit, page_id: &str) {
        let annotation_ids: Vec<String> = Self::list_annotations(doc)
            .into_iter()
            .filter(|annotation| {
                Self::annotation_page_id(doc, &annotation.id).as_deref() == Some(page_id)
            })
            .map(|annotation| annotation.id)
            .collect();
        for id in annotation_ids {
            Self::delete_annotation(doc, &id);
        }
        for key in ["comments", "signatures"] {
            let Some(list) = Self::list(doc, key) else {
                continue;
            };
            for index in (0..doc.length(&list)).rev() {
                let anchored = doc
                    .get(&list, index)
                    .ok()
                    .flatten()
                    .and_then(|(value, entry)| {
                        matches!(value, Value::Object(ObjType::Map))
                            .then(|| Self::get_str(doc, &entry, "page-id"))
                            .flatten()
                    })
                    .as_deref()
                    == Some(page_id);
                if anchored {
                    let _ = doc.delete(&list, index);
                }
            }
        }
    }

    pub fn move_page_plan(doc: &mut AutoCommit, id: &str, after_id: Option<&str>) {
        let Some(list) = Self::list(doc, "page-plan") else {
            return;
        };
        let Some(from) = Self::index_of(doc, &list, id) else {
            return;
        };
        let Ok(Some((Value::Object(ObjType::Map), _))) = doc.get(&list, from) else {
            return;
        };
        let Some(moved_entry) = Self::list_page_plan(doc)
            .into_iter()
            .find(|candidate| candidate.id == id)
        else {
            return;
        };
        // Remove every physical representation of this durable occurrence.
        // Concurrent moves can each have inserted one, and leaving an older
        // one behind would let a subsequent delete reveal a ghost page.
        for index in (0..doc.length(&list)).rev() {
            let is_target = doc
                .get(&list, index)
                .ok()
                .flatten()
                .and_then(|(value, entry)| {
                    matches!(value, Value::Object(ObjType::Map))
                        .then(|| Self::get_str(doc, &entry, "id"))
                        .flatten()
                })
                .is_some_and(|candidate| candidate == id);
            if is_target {
                let _ = doc.delete(&list, index);
            }
        }
        let to = after_id
            .and_then(|after| Self::index_of(doc, &list, after).map(|i| i + 1))
            .unwrap_or(0);
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

    pub fn add_annotation(doc: &mut AutoCommit, a: &Annotation) {
        let Some(list) = Self::list(doc, "annotations") else {
            return;
        };
        let idx = doc.length(&list);
        let Ok(m) = doc.insert_object(&list, idx, ObjType::Map) else {
            return;
        };
        let _ = doc.put(&m, "id", a.id.as_str());
        let _ = doc.put(&m, "page", a.page as i64);
        if let Some(page_id) = Self::page_id_for_number(doc, a.page) {
            let _ = doc.put(&m, "page-id", page_id);
        }
        let _ = doc.put(&m, "kind", a.kind.type_key());
        let _ = doc.put(&m, "color", a.color.key());
        let _ = doc.put(&m, "author", a.author.as_str());
        match &a.kind {
            AnnoKind::Highlight { rect } => {
                let _ = doc.put(&m, "x", rect.x);
                let _ = doc.put(&m, "y", rect.y);
                let _ = doc.put(&m, "w", rect.w);
                let _ = doc.put(&m, "h", rect.h);
            }
            AnnoKind::Note { x, y } => {
                let _ = doc.put(&m, "x", *x);
                let _ = doc.put(&m, "y", *y);
            }
            AnnoKind::Ink { points } => {
                let _ = doc.put(&m, "points", Self::points_to_str(points));
            }
        }
    }

    pub fn list_annotations(doc: &AutoCommit) -> Vec<Annotation> {
        let Some(list) = Self::list_ro(doc, "annotations") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..doc.length(&list) {
            let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) else {
                continue;
            };
            let id = Self::get_str(doc, &m, "id").unwrap_or_default();
            let legacy_page = Self::get_i64(doc, &m, "page").unwrap_or(1) as usize;
            let page = Self::get_str(doc, &m, "page-id")
                .and_then(|page_id| Self::page_number_for_id(doc, &page_id))
                .unwrap_or(legacy_page);
            let color = Swatch::parse(&Self::get_str(doc, &m, "color").unwrap_or_default());
            let author = Self::get_str(doc, &m, "author").unwrap_or_default();
            let kind = match Self::get_str(doc, &m, "kind").as_deref() {
                Some("highlight") => AnnoKind::Highlight {
                    rect: PtRect::new(
                        Self::get_f64(doc, &m, "x").unwrap_or(0.0),
                        Self::get_f64(doc, &m, "y").unwrap_or(0.0),
                        Self::get_f64(doc, &m, "w").unwrap_or(0.0),
                        Self::get_f64(doc, &m, "h").unwrap_or(0.0),
                    ),
                },
                Some("ink") => AnnoKind::Ink {
                    points: Self::points_from_str(
                        &Self::get_str(doc, &m, "points").unwrap_or_default(),
                    ),
                },
                _ => AnnoKind::Note {
                    x: Self::get_f64(doc, &m, "x").unwrap_or(0.0),
                    y: Self::get_f64(doc, &m, "y").unwrap_or(0.0),
                },
            };
            out.push(Annotation {
                id,
                page,
                kind,
                color,
                author,
            });
        }
        out
    }

    fn annotation_page_id(doc: &AutoCommit, id: &str) -> Option<String> {
        let list = Self::list_ro(doc, "annotations")?;
        let index = Self::index_of(doc, &list, id)?;
        let (_, entry) = doc.get(&list, index).ok()??;
        Self::get_str(doc, &entry, "page-id")
    }

    fn index_of(doc: &AutoCommit, list: &ObjId, id: &str) -> Option<usize> {
        for i in 0..doc.length(list) {
            if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(list, i)
                && Self::get_str(doc, &m, "id").as_deref() == Some(id)
            {
                return Some(i);
            }
        }
        None
    }

    pub fn set_annotation_color(doc: &mut AutoCommit, id: &str, color: Swatch) {
        let Some(list) = Self::list(doc, "annotations") else {
            return;
        };
        if let Some(i) = Self::index_of(doc, &list, id)
            && let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
        {
            let _ = doc.put(&m, "color", color.key());
        }
    }

    /// Move an annotation's anchor to `(x, y)` in page-point space. Used by the
    /// draggable annotation markers — repositioning a sticky note. Highlights
    /// keep their width/height and shift; notes carry only a point. A no-op for
    /// ink (its geometry is the polyline, not a single anchor).
    pub fn set_annotation_position(doc: &mut AutoCommit, id: &str, x: f64, y: f64) {
        let Some(list) = Self::list(doc, "annotations") else {
            return;
        };
        if let Some(i) = Self::index_of(doc, &list, id)
            && let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
            && Self::get_str(doc, &m, "kind").as_deref() != Some("ink")
        {
            let _ = doc.put(&m, "x", x);
            let _ = doc.put(&m, "y", y);
        }
    }

    pub fn delete_annotation(doc: &mut AutoCommit, id: &str) {
        if let Some(list) = Self::list(doc, "annotations")
            && let Some(i) = Self::index_of(doc, &list, id)
        {
            let _ = doc.delete(&list, i);
        }
        // Any comments anchored to this annotation are removed with it so a
        // deleted note never leaves a dangling thread behind in the rail.
        let Some(comments) = Self::list(doc, "comments") else {
            return;
        };
        let mut i = 0;
        while i < doc.length(&comments) {
            let anchored = matches!(
                doc.get(&comments, i),
                Ok(Some((Value::Object(ObjType::Map), m)))
                    if Self::get_str(doc, &m, "anno").as_deref() == Some(id)
            );
            if anchored {
                let _ = doc.delete(&comments, i);
            } else {
                i += 1;
            }
        }
    }

    // ---- comments ---------------------------------------------------

    pub fn add_comment(doc: &mut AutoCommit, c: &Comment) {
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
        if let Some(page_id) = Self::page_id_for_number(doc, c.page) {
            let _ = doc.put(&m, "page-id", page_id);
        }
        let _ = doc.put(&m, "author", c.author.as_str());
        let _ = doc.put(&m, "body", c.body.as_str());
        let _ = doc.put(&m, "created", c.created);
        let _ = doc.put(&m, "resolved", c.resolved);
        let Ok(replies) = doc.put_object(&m, "replies", ObjType::List) else {
            return;
        };
        for (ri, r) in c.replies.iter().enumerate() {
            let Ok(rm) = doc.insert_object(&replies, ri, ObjType::Map) else {
                continue;
            };
            let _ = doc.put(&rm, "author", r.author.as_str());
            let _ = doc.put(&rm, "body", r.body.as_str());
        }
    }

    pub fn list_comments(doc: &AutoCommit) -> Vec<Comment> {
        let Some(list) = Self::list_ro(doc, "comments") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..doc.length(&list) {
            let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) else {
                continue;
            };
            let id = Self::get_str(doc, &m, "id").unwrap_or_default();
            let anno_id = Self::get_str(doc, &m, "anno");
            let legacy_page = Self::get_i64(doc, &m, "page").unwrap_or(1) as usize;
            let page = Self::get_str(doc, &m, "page-id")
                .and_then(|page_id| Self::page_number_for_id(doc, &page_id))
                .unwrap_or(legacy_page);
            let author = Self::get_str(doc, &m, "author").unwrap_or_default();
            let body = Self::get_str(doc, &m, "body").unwrap_or_default();
            let created = Self::get_i64(doc, &m, "created").unwrap_or(0);
            let resolved = Self::get_bool(doc, &m, "resolved").unwrap_or(false);
            let mut replies = Vec::new();
            if let Ok(Some((Value::Object(ObjType::List), rl))) = doc.get(&m, "replies") {
                for j in 0..doc.length(&rl) {
                    if let Ok(Some((Value::Object(ObjType::Map), rm))) = doc.get(&rl, j) {
                        replies.push(Reply {
                            author: Self::get_str(doc, &rm, "author").unwrap_or_default(),
                            body: Self::get_str(doc, &rm, "body").unwrap_or_default(),
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
            });
        }
        out.sort_by_key(|c| c.created);
        out
    }

    pub fn set_comment_resolved(doc: &mut AutoCommit, id: &str, resolved: bool) {
        let Some(list) = Self::list(doc, "comments") else {
            return;
        };
        if let Some(i) = Self::index_of(doc, &list, id)
            && let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
        {
            let _ = doc.put(&m, "resolved", resolved);
        }
    }

    /// Replace a comment's body text. Used by the inline editor when the user
    /// types into a note or comment thread.
    pub fn set_comment_body(doc: &mut AutoCommit, id: &str, body: &str) {
        let Some(list) = Self::list(doc, "comments") else {
            return;
        };
        if let Some(i) = Self::index_of(doc, &list, id)
            && let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
        {
            let _ = doc.put(&m, "body", body);
        }
    }

    pub fn delete_comment(doc: &mut AutoCommit, id: &str) {
        if let Some(list) = Self::list(doc, "comments")
            && let Some(i) = Self::index_of(doc, &list, id)
        {
            let _ = doc.delete(&list, i);
        }
    }

    pub fn add_reply(doc: &mut AutoCommit, comment_id: &str, reply: &Reply) {
        let Some(list) = Self::list(doc, "comments") else {
            return;
        };
        if let Some(i) = Self::index_of(doc, &list, comment_id)
            && let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
        {
            let replies = match doc.get(&m, "replies") {
                Ok(Some((Value::Object(ObjType::List), rl))) => Some(rl),
                _ => doc.put_object(&m, "replies", ObjType::List).ok(),
            };
            let Some(replies) = replies else {
                return;
            };
            let idx = doc.length(&replies);
            let Ok(rm) = doc.insert_object(&replies, idx, ObjType::Map) else {
                return;
            };
            let _ = doc.put(&rm, "author", reply.author.as_str());
            let _ = doc.put(&rm, "body", reply.body.as_str());
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

    pub fn add_signature(doc: &mut AutoCommit, s: &Signature) {
        let Some(list) = Self::list(doc, "signatures") else {
            return;
        };
        let idx = doc.length(&list);
        let Ok(m) = doc.insert_object(&list, idx, ObjType::Map) else {
            return;
        };
        let _ = doc.put(&m, "id", s.id.as_str());
        let _ = doc.put(&m, "page", s.page as i64);
        if let Some(page_id) = Self::page_id_for_number(doc, s.page) {
            let _ = doc.put(&m, "page-id", page_id);
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
        let Some(list) = Self::list_ro(doc, "signatures") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..doc.length(&list) {
            let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) else {
                continue;
            };
            // Signatures written before real ink existed carry no "ink" field;
            // they render as a typed-name signature (no fabricated squiggle).
            let ink = match Self::get_str(doc, &m, "ink").as_deref() {
                Some("drawn") => crate::model::SignatureInk::Drawn {
                    strokes: Self::strokes_from_str(
                        &Self::get_str(doc, &m, "strokes").unwrap_or_default(),
                    ),
                },
                _ => crate::model::SignatureInk::Typed,
            };
            out.push(Signature {
                id: Self::get_str(doc, &m, "id").unwrap_or_default(),
                page: Self::get_str(doc, &m, "page-id")
                    .and_then(|page_id| Self::page_number_for_id(doc, &page_id))
                    .unwrap_or_else(|| Self::get_i64(doc, &m, "page").unwrap_or(1) as usize),
                rect: PtRect::new(
                    Self::get_f64(doc, &m, "x").unwrap_or(0.0),
                    Self::get_f64(doc, &m, "y").unwrap_or(0.0),
                    Self::get_f64(doc, &m, "w").unwrap_or(180.0),
                    Self::get_f64(doc, &m, "h").unwrap_or(56.0),
                ),
                signer: Self::get_str(doc, &m, "signer").unwrap_or_default(),
                date: Self::get_str(doc, &m, "date").unwrap_or_default(),
                ink,
            });
        }
        out
    }

    pub fn delete_signature(doc: &mut AutoCommit, id: &str) {
        if let Some(list) = Self::list(doc, "signatures")
            && let Some(i) = Self::index_of(doc, &list, id)
        {
            let _ = doc.delete(&list, i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AnnoKind, Annotation, Comment, PtRect, Reply, Signature, Swatch};

    fn hl(page: usize) -> Annotation {
        Annotation {
            id: Annotation::new_id(),
            page,
            kind: AnnoKind::Highlight {
                rect: PtRect::new(10.0, 20.0, 100.0, 14.0),
            },
            color: Swatch::Yellow,
            author: "Alice".into(),
        }
    }

    #[test]
    fn annotation_round_trips() {
        let mut doc = AutoCommit::new();
        let a = hl(2);
        PdfDoc::add_annotation(&mut doc, &a);
        let list = PdfDoc::list_annotations(&doc);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].page, 2);
        assert_eq!(list[0].color, Swatch::Yellow);
        match &list[0].kind {
            AnnoKind::Highlight { rect } => {
                assert!((rect.w - 100.0).abs() < 1e-6);
            }
            _ => panic!("expected highlight"),
        }
    }

    #[test]
    fn ink_points_round_trip() {
        let mut doc = AutoCommit::new();
        let a = Annotation {
            id: Annotation::new_id(),
            page: 1,
            kind: AnnoKind::Ink {
                points: vec![(1.0, 2.0), (3.5, 4.25), (10.0, 0.0)],
            },
            color: Swatch::Blue,
            author: "Bob".into(),
        };
        PdfDoc::add_annotation(&mut doc, &a);
        let got = PdfDoc::list_annotations(&doc);
        match &got[0].kind {
            AnnoKind::Ink { points } => {
                assert_eq!(points.len(), 3);
                assert!((points[1].0 - 3.5).abs() < 1e-6);
                assert!((points[1].1 - 4.25).abs() < 1e-6);
            }
            _ => panic!("expected ink"),
        }
    }

    #[test]
    fn set_color_and_delete() {
        let mut doc = AutoCommit::new();
        let a = hl(1);
        let id = a.id.clone();
        PdfDoc::add_annotation(&mut doc, &a);
        PdfDoc::set_annotation_color(&mut doc, &id, Swatch::Green);
        assert_eq!(PdfDoc::list_annotations(&doc)[0].color, Swatch::Green);
        PdfDoc::delete_annotation(&mut doc, &id);
        assert!(PdfDoc::list_annotations(&doc).is_empty());
    }

    #[test]
    fn set_position_moves_a_note_marker() {
        let mut doc = AutoCommit::new();
        let a = Annotation {
            id: Annotation::new_id(),
            page: 1,
            kind: AnnoKind::Note { x: 30.0, y: 40.0 },
            color: Swatch::Blue,
            author: "Alice".into(),
        };
        let id = a.id.clone();
        PdfDoc::add_annotation(&mut doc, &a);
        PdfDoc::set_annotation_position(&mut doc, &id, 120.5, 210.25);
        match &PdfDoc::list_annotations(&doc)[0].kind {
            AnnoKind::Note { x, y } => {
                assert!((x - 120.5).abs() < 1e-6, "note x moved");
                assert!((y - 210.25).abs() < 1e-6, "note y moved");
            }
            _ => panic!("expected note"),
        }
    }

    #[test]
    fn set_position_leaves_ink_geometry_untouched() {
        // Ink is a polyline, not a single anchor — repositioning must be a no-op.
        let mut doc = AutoCommit::new();
        let a = Annotation {
            id: Annotation::new_id(),
            page: 1,
            kind: AnnoKind::Ink {
                points: vec![(1.0, 2.0), (3.0, 4.0)],
            },
            color: Swatch::Blue,
            author: "Bob".into(),
        };
        let id = a.id.clone();
        PdfDoc::add_annotation(&mut doc, &a);
        PdfDoc::set_annotation_position(&mut doc, &id, 99.0, 99.0);
        match &PdfDoc::list_annotations(&doc)[0].kind {
            AnnoKind::Ink { points } => assert_eq!(points.len(), 2, "ink polyline preserved"),
            _ => panic!("expected ink"),
        }
    }

    #[test]
    fn comment_thread_round_trips() {
        let mut doc = AutoCommit::new();
        let c = Comment {
            id: Comment::new_id(),
            anno_id: Some("anno-1".into()),
            page: 1,
            author: "Alice".into(),
            body: "Is this still accurate?".into(),
            created: 5,
            resolved: false,
            replies: vec![Reply {
                author: "Bob".into(),
                body: "Yes, verified.".into(),
            }],
        };
        let cid = c.id.clone();
        PdfDoc::add_comment(&mut doc, &c);
        let got = PdfDoc::list_comments(&doc);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].anno_id.as_deref(), Some("anno-1"));
        assert_eq!(got[0].replies.len(), 1);
        assert_eq!(got[0].replies[0].author, "Bob");

        PdfDoc::add_reply(
            &mut doc,
            &cid,
            &Reply {
                author: "Carol".into(),
                body: "Agreed.".into(),
            },
        );
        PdfDoc::set_comment_resolved(&mut doc, &cid, true);
        PdfDoc::set_comment_body(&mut doc, &cid, "Edited: confirmed accurate.");
        let got = PdfDoc::list_comments(&doc);
        assert_eq!(got[0].replies.len(), 2);
        assert!(got[0].resolved);
        assert_eq!(got[0].body, "Edited: confirmed accurate.");
    }

    #[test]
    fn comments_sort_by_created() {
        let mut doc = AutoCommit::new();
        for (i, created) in [("a", 30), ("b", 10), ("c", 20)] {
            PdfDoc::add_comment(
                &mut doc,
                &Comment {
                    id: i.into(),
                    anno_id: None,
                    page: 1,
                    author: "x".into(),
                    body: "y".into(),
                    created,
                    resolved: false,
                    replies: vec![],
                },
            );
        }
        let got = PdfDoc::list_comments(&doc);
        let order: Vec<i64> = got.iter().map(|c| c.created).collect();
        assert_eq!(order, vec![10, 20, 30]);
    }

    #[test]
    fn signature_round_trips() {
        let mut doc = AutoCommit::new();
        let s = Signature {
            id: Signature::new_id(),
            page: 5,
            rect: PtRect::new(360.0, 640.0, 180.0, 56.0),
            signer: "Alice Example".into(),
            date: "2026-06-30".into(),
            ink: crate::model::SignatureInk::Typed,
        };
        PdfDoc::add_signature(&mut doc, &s);
        let got = PdfDoc::list_signatures(&doc);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].signer, "Alice Example");
        assert_eq!(got[0].page, 5);
        assert_eq!(got[0].ink, crate::model::SignatureInk::Typed);
    }

    #[test]
    fn drawn_signature_strokes_round_trip() {
        let mut doc = AutoCommit::new();
        let strokes = vec![
            vec![(0.1, 0.5), (0.3, 0.2), (0.5, 0.6)],
            vec![(0.6, 0.4), (0.9, 0.5)],
        ];
        let s = Signature {
            id: Signature::new_id(),
            page: 2,
            rect: PtRect::new(100.0, 100.0, 200.0, 70.0),
            signer: "Grace".into(),
            date: "2026-08-17".into(),
            ink: crate::model::SignatureInk::Drawn { strokes },
        };
        PdfDoc::add_signature(&mut doc, &s);
        let got = PdfDoc::list_signatures(&doc);
        assert_eq!(got.len(), 1);
        match &got[0].ink {
            crate::model::SignatureInk::Drawn { strokes: back } => {
                assert_eq!(back.len(), 2, "both strokes survive");
                assert_eq!(back[0].len(), 3);
                assert!((back[0][1].0 - 0.3).abs() < 1e-2 && (back[0][1].1 - 0.2).abs() < 1e-2);
                assert_eq!(back[1].len(), 2);
            }
            crate::model::SignatureInk::Typed => panic!("expected drawn ink, got Typed"),
        }
    }

    #[test]
    fn deleting_annotation_removes_its_anchored_comments() {
        let mut doc = AutoCommit::new();
        let a = hl(1);
        let aid = a.id.clone();
        PdfDoc::add_annotation(&mut doc, &a);
        PdfDoc::add_comment(
            &mut doc,
            &Comment {
                id: Comment::new_id(),
                anno_id: Some(aid.clone()),
                page: 1,
                author: "Alice".into(),
                body: "pinned".into(),
                created: 1,
                resolved: false,
                replies: vec![],
            },
        );
        // An unrelated free comment must survive the delete.
        PdfDoc::add_comment(
            &mut doc,
            &Comment {
                id: Comment::new_id(),
                anno_id: None,
                page: 1,
                author: "Bob".into(),
                body: "free".into(),
                created: 2,
                resolved: false,
                replies: vec![],
            },
        );
        assert_eq!(PdfDoc::list_comments(&doc).len(), 2);
        PdfDoc::delete_annotation(&mut doc, &aid);
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
    fn delete_comment_and_signature() {
        let mut doc = AutoCommit::new();
        let c = Comment {
            id: Comment::new_id(),
            anno_id: None,
            page: 1,
            author: "Alice".into(),
            body: "x".into(),
            created: 1,
            resolved: false,
            replies: vec![],
        };
        let cid = c.id.clone();
        PdfDoc::add_comment(&mut doc, &c);
        PdfDoc::delete_comment(&mut doc, &cid);
        assert!(PdfDoc::list_comments(&doc).is_empty());

        let s = Signature {
            id: Signature::new_id(),
            page: 1,
            rect: PtRect::new(10.0, 10.0, 100.0, 40.0),
            signer: "Alice".into(),
            date: "2026-06-30".into(),
            ink: crate::model::SignatureInk::Typed,
        };
        let sid = s.id.clone();
        PdfDoc::add_signature(&mut doc, &s);
        PdfDoc::delete_signature(&mut doc, &sid);
        assert!(PdfDoc::list_signatures(&doc).is_empty());
    }

    #[test]
    fn seeded_flag_flips() {
        let mut doc = AutoCommit::new();
        assert!(!PdfDoc::is_seeded(&mut doc));
        PdfDoc::set_meta(&mut doc, "T", "A");
        assert!(PdfDoc::is_seeded(&mut doc));
    }

    #[test]
    fn concurrent_initialisation_and_same_page_moves_converge_without_duplicate_ids() {
        let base = AutoCommit::new().save();
        let mut left = AutoCommit::load(&base).unwrap();
        let mut right = AutoCommit::load(&base).unwrap();

        // Both clients open the same uninitialised record before either sees
        // the other. Their physical list insertions may both survive merge,
        // but the materialised plan has one deterministic occurrence per id.
        PdfDoc::ensure_page_plan(&mut left, 3);
        PdfDoc::ensure_page_plan(&mut right, 3);
        left.merge(&mut right).unwrap();
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
        a.merge(&mut b).unwrap();
        let plan = PdfDoc::list_page_plan(&a);
        assert!(!plan.is_empty());
        let ids: HashSet<&str> = plan.iter().map(|entry| entry.id.as_str()).collect();
        assert_eq!(ids.len(), plan.len(), "each durable id materialises once");
        assert_eq!(ids.len(), 3);
        b.merge(&mut a).unwrap();
        assert_eq!(PdfDoc::list_page_plan(&a), PdfDoc::list_page_plan(&b));
    }

    #[test]
    fn markup_anchors_follow_a_page_occurrence_through_move_and_delete() {
        let mut doc = AutoCommit::new();
        PdfDoc::ensure_page_plan(&mut doc, 3);
        let annotation = hl(1);
        let annotation_id = annotation.id.clone();
        PdfDoc::add_annotation(&mut doc, &annotation);
        PdfDoc::add_comment(
            &mut doc,
            &Comment {
                id: "comment-on-source-1".into(),
                anno_id: Some(annotation_id),
                page: 1,
                author: "Alice".into(),
                body: "attached".into(),
                created: 1,
                resolved: false,
                replies: vec![],
            },
        );
        PdfDoc::add_signature(
            &mut doc,
            &Signature {
                id: "signature-on-source-1".into(),
                page: 1,
                rect: PtRect::new(10.0, 10.0, 100.0, 40.0),
                signer: "Alice".into(),
                date: "2026-01-01".into(),
                ink: crate::model::SignatureInk::Typed,
            },
        );

        PdfDoc::move_page_plan(&mut doc, "source-1", Some("source-3"));
        assert_eq!(PdfDoc::list_annotations(&doc)[0].page, 3);
        assert_eq!(PdfDoc::list_comments(&doc)[0].page, 3);
        assert_eq!(PdfDoc::list_signatures(&doc)[0].page, 3);

        PdfDoc::delete_page_plan(&mut doc, "source-1");
        assert!(PdfDoc::list_annotations(&doc).is_empty());
        assert!(PdfDoc::list_comments(&doc).is_empty());
        assert!(PdfDoc::list_signatures(&doc).is_empty());
    }

    #[test]
    fn imported_pages_are_atomic_and_keep_their_durable_identity() {
        let mut doc = AutoCommit::new();
        PdfDoc::ensure_page_plan(&mut doc, 2);
        let imported = ImportedPage {
            hash: "a".repeat(64),
            kind: "image".into(),
            page: 1,
            width_milli: 612_000,
            height_milli: 408_000,
        };
        let page = PagePlanEntry {
            id: "imported-image".into(),
            source_page: 1,
            imported: Some(imported),
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
