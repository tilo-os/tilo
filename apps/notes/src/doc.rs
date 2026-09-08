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
//!                 └── "text" (Text)  # body + author/Notes inline marks
//! ```
//!
//! Everything here operates on a borrowed `&mut AutoCommit` — the app's
//! shared `State::doc`. "Which page I'm viewing" is deliberately NOT in
//! this document: it is per-peer presence (see `NotesApp`), so two peers
//! can read different pages while sharing one workspace.

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::doc::{put_if_changed, put_text_diffed};
use ui::widget::BlockEditOp;
use uuid::Uuid;

use crate::actions::{InlineMark, InlineMarkSpan};
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

        // A page owns a durable block list from its very first action. This
        // avoids a later editor bind creating collaborative structure outside
        // the canonical action path.
        if !matches!(
            doc.get(&page, "blocks"),
            Ok(Some((Value::Object(ObjType::List), _)))
        ) {
            let _ = doc.put_object(&page, "blocks", ObjType::List);
        }
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

    /// Return an existing page's `blocks` List. Page creation ensures the
    /// list exists as part of `UpsertPage`; this read helper intentionally
    /// never creates document state from an editor/widget path.
    ///
    pub fn page_blocks_list(doc: &AutoCommit, id: PageId) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), pages))) = doc.get(&ROOT, "pages") else {
            return None;
        };
        let key = id.to_string();
        let Ok(Some((Value::Object(ObjType::Map), page))) = doc.get(&pages, key.as_str()) else {
            return None;
        };
        match doc.get(&page, "blocks") {
            Ok(Some((Value::Object(ObjType::List), lid))) => Some(lid),
            _ => None,
        }
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

    /// Stable block identities and current plain text for one page. The Text
    /// object's Automerge id survives list insertion, deletion and reordering.
    pub fn page_block_snapshots(doc: &AutoCommit, id: PageId) -> Vec<(String, String)> {
        let Ok(Some((Value::Object(ObjType::Map), pages))) = doc.get(&ROOT, "pages") else {
            return Vec::new();
        };
        let key = id.to_string();
        let Ok(Some((Value::Object(ObjType::Map), page))) = doc.get(&pages, key.as_str()) else {
            return Vec::new();
        };
        let Ok(Some((Value::Object(ObjType::List), blocks))) = doc.get(&page, "blocks") else {
            return Vec::new();
        };
        (0..doc.length(&blocks))
            .filter_map(|index| {
                let text = Self::block_text_obj(doc, &blocks, index)?;
                let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(&blocks, index) else {
                    return None;
                };
                // Pre-action Notes documents used the Text object id as the
                // public identity. Keep that as a read-only compatibility
                // fallback, but all action-created blocks persist an explicit
                // UUID so identities remain stable across move/reinsert.
                let body = doc.text(&text).unwrap_or_default();
                let id = block_id(doc, &block).unwrap_or_else(|| {
                    ui::widget::legacy_block_id(
                        &blocks,
                        index,
                        &Self::block_kind(doc, &blocks, index),
                        &body,
                    )
                });
                Some((id, body))
            })
            .collect()
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
        let _ = doc.put(&block, "id", Uuid::new_v4().to_string());
        let text_id = doc
            .put_object(&block, "text", ObjType::Text)
            .expect("put_object block text never fails");
        if !text.is_empty() {
            let _ = doc.splice_text(&text_id, 0, 0, text);
        }
        text_id
    }

    /// Apply semantic editor operations through a page's durable block list.
    /// Every existing target is UUID-addressed, stale predecessor/target ids
    /// refuse, and malformed UTF-8 byte offsets refuse before a write. This
    /// keeps agents and peers from accidentally applying an edit to a nearby
    /// block after concurrent structure changes.
    ///
    /// # Errors
    ///
    /// Returns an error when the page or addressed block no longer exists, an
    /// operation is malformed or stale, or Automerge refuses the requested
    /// structural/text mutation.
    pub fn apply_block_ops(
        doc: &mut AutoCommit,
        page: PageId,
        ops: &[BlockEditOp],
    ) -> Result<(), String> {
        let Some(list) = Self::page_blocks_list(doc, page) else {
            return Err("page no longer exists".into());
        };
        // Pre-action documents did not persist a block UUID. Materialise the
        // shared widget's deterministic legacy identity *inside this action*,
        // never during widget binding, so a first edit remains replayable.
        ensure_block_ids(doc, &list)?;
        for op in ops {
            apply_block_op(doc, &list, op)?;
        }
        Ok(())
    }

    /// Replace Notes' own formatting/link marks on one durable block.  The
    /// marks live on Automerge's Text object rather than in a sidecar JSON
    /// blob, so a concurrent text splice carries the marked range with it.
    /// Other applications' marks (notably `qos.author`) are deliberately left
    /// untouched.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale page/block, invalid link target or UTF-8
    /// range, or an Automerge mutation failure.
    pub fn set_block_marks(
        doc: &mut AutoCommit,
        page: PageId,
        id: &str,
        marks: &[InlineMarkSpan],
    ) -> Result<(), String> {
        let Some(list) = Self::page_blocks_list(doc, page) else {
            return Err("page no longer exists".into());
        };
        ensure_block_ids(doc, &list)?;
        let (_, block) =
            block_map(doc, &list, id).ok_or_else(|| "block no longer exists".to_string())?;
        let Ok(Some((Value::Object(ObjType::Text), text_obj))) = doc.get(&block, "text") else {
            return Err("block text no longer exists".into());
        };
        let text = doc
            .text(&text_obj)
            .map_err(|_| "block text no longer exists")?;
        for mark in marks {
            validate_inline_mark(&text, mark)?;
        }

        replace_native_marks(doc, &block, &text_obj, &text, marks)
    }

    /// Whether at least one block in `page` has been through the native-mark
    /// action path. This lets binding migrate legacy Markdown marks once while
    /// still allowing a collaborator to intentionally clear every mark.
    pub fn page_marks_are_canonical(doc: &AutoCommit, page: PageId) -> bool {
        let Some(list) = Self::page_blocks_list(doc, page) else {
            return false;
        };
        (0..doc.length(&list)).any(|index| {
            let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(&list, index) else {
                return false;
            };
            matches!(
                doc.get(&block, "notes_marks_version"),
                Ok(Some((Value::Scalar(value), _))) if value.to_i64() == Some(1)
            )
        })
    }

    /// Read the Notes-owned native marks for one page as stable block ids and
    /// UTF-8 byte ranges. Invalid legacy marks are ignored rather than making
    /// an otherwise readable note impossible to open.
    pub fn page_block_marks(doc: &AutoCommit, page: PageId) -> Vec<(String, Vec<InlineMarkSpan>)> {
        let Some(list) = Self::page_blocks_list(doc, page) else {
            return Vec::new();
        };
        (0..doc.length(&list))
            .filter_map(|index| {
                let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(&list, index) else {
                    return None;
                };
                let text_obj = Self::block_text_obj(doc, &list, index)?;
                let text = doc.text(&text_obj).ok()?;
                let id = block_id(doc, &block).unwrap_or_else(|| {
                    let kind = block_kind_value(doc, &block).unwrap_or_default();
                    ui::widget::legacy_block_id(&list, index, &kind, &text)
                });
                let marks = doc
                    .marks(&text_obj)
                    .ok()?
                    .iter()
                    .filter_map(|mark| inline_mark_span(&text, mark))
                    .collect();
                Some((id, marks))
            })
            .collect()
    }
}

const NOTE_MARK_PREFIX: &str = "notes.inline.";

fn note_mark_name(name: &str) -> bool {
    name.starts_with(NOTE_MARK_PREFIX)
}

fn durable_mark_value(mark: &InlineMark) -> Result<(String, automerge::ScalarValue), String> {
    let name = match mark {
        InlineMark::Bold => "bold".to_string(),
        InlineMark::Italic => "italic".to_string(),
        InlineMark::Code => "code".to_string(),
        InlineMark::Underline => "underline".to_string(),
        InlineMark::Strikethrough => "strikethrough".to_string(),
        InlineMark::Link { page } => {
            page.parse::<PageId>()
                .map_err(|_| "link target is invalid".to_string())?;
            format!("link:{page}")
        }
    };
    let value = match mark {
        InlineMark::Link { page } => automerge::ScalarValue::Str(page.clone().into()),
        _ => automerge::ScalarValue::Boolean(true),
    };
    Ok((format!("{NOTE_MARK_PREFIX}{name}"), value))
}

fn validate_inline_mark(text: &str, mark: &InlineMarkSpan) -> Result<(), String> {
    if mark.start >= mark.end || mark.end > text.len() {
        return Err("mark range is out of bounds".into());
    }
    if !text.is_char_boundary(mark.start) || !text.is_char_boundary(mark.end) {
        return Err("mark range is not on a character boundary".into());
    }
    let _ = durable_mark_value(&mark.mark)?;
    Ok(())
}

fn char_to_byte_index(text: &str, character: usize) -> Option<usize> {
    if character == text.chars().count() {
        return Some(text.len());
    }
    text.char_indices().nth(character).map(|(byte, _)| byte)
}

fn inline_mark_span(text: &str, mark: &Mark) -> Option<InlineMarkSpan> {
    let name = mark.name.strip_prefix(NOTE_MARK_PREFIX)?;
    let inline = match name {
        "bold" => InlineMark::Bold,
        "italic" => InlineMark::Italic,
        "code" => InlineMark::Code,
        "underline" => InlineMark::Underline,
        "strikethrough" => InlineMark::Strikethrough,
        link if link.starts_with("link:") => {
            let page = match &mark.value {
                automerge::ScalarValue::Str(value) => value.to_string(),
                _ => link.strip_prefix("link:")?.to_string(),
            };
            page.parse::<PageId>().ok()?;
            InlineMark::Link { page }
        }
        _ => return None,
    };
    Some(InlineMarkSpan {
        start: char_to_byte_index(text, mark.start)?,
        end: char_to_byte_index(text, mark.end)?,
        mark: inline,
    })
}

fn replace_native_marks(
    doc: &mut AutoCommit,
    block: &ObjId,
    text_obj: &ObjId,
    text: &str,
    marks: &[InlineMarkSpan],
) -> Result<(), String> {
    // Native marks use character offsets. Clear every Notes-owned mark from
    // its exact current range before writing this authoritative action
    // payload. Native marks originating from other concerns stay intact.
    let existing = doc
        .marks(text_obj)
        .map_err(|_| "block marks could not be read")?;
    for mark in existing
        .iter()
        .filter(|mark| note_mark_name(mark.name.as_str()))
    {
        doc.unmark(
            text_obj,
            mark.name.as_str(),
            mark.start,
            mark.end,
            ExpandMark::After,
        )
        .map_err(|_| "block marks could not be cleared")?;
    }
    for span in marks {
        let (name, value) = durable_mark_value(&span.mark)?;
        let start = byte_to_char_index(text, span.start)
            .ok_or_else(|| "mark start is not on a character boundary".to_string())?;
        let end = byte_to_char_index(text, span.end)
            .ok_or_else(|| "mark end is not on a character boundary".to_string())?;
        doc.mark(
            text_obj,
            Mark::new(name, value, start, end),
            ExpandMark::After,
        )
        .map_err(|_| "block marks could not be written")?;
    }
    // A version marker distinguishes an intentional empty formatting snapshot
    // from a pre-native-marks document that still needs Markdown migration.
    doc.put(block, "notes_marks_version", 1_i64)
        .map_err(|_| "block marks version could not be written")?;
    Ok(())
}

fn block_id(doc: &AutoCommit, block: &ObjId) -> Option<String> {
    match doc.get(block, "id") {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            automerge::ScalarValue::Str(id) if Uuid::parse_str(id).is_ok() => Some(id.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn block_index(doc: &AutoCommit, list: &ObjId, id: &str) -> Option<usize> {
    (0..doc.length(list)).find(|index| {
        let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(list, *index) else {
            return false;
        };
        if block_id(doc, &block).as_deref() == Some(id) {
            return true;
        }
        let kind = block_kind_value(doc, &block).unwrap_or_default();
        let text = match doc.get(&block, "text") {
            Ok(Some((Value::Object(ObjType::Text), text))) => doc.text(&text).unwrap_or_default(),
            _ => String::new(),
        };
        ui::widget::legacy_block_id(list, *index, &kind, &text) == id
    })
}

fn ensure_block_ids(doc: &mut AutoCommit, list: &ObjId) -> Result<(), String> {
    for index in 0..doc.length(list) {
        let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(list, index) else {
            return Err("page block no longer exists".into());
        };
        if block_id(doc, &block).is_some() {
            continue;
        }
        let kind = block_kind_value(doc, &block)
            .ok_or_else(|| "block kind no longer exists".to_string())?;
        let text = match doc.get(&block, "text") {
            Ok(Some((Value::Object(ObjType::Text), text))) => {
                doc.text(&text).map_err(|_| "block text no longer exists")?
            }
            _ => return Err("block text no longer exists".into()),
        };
        let id = ui::widget::legacy_block_id(list, index, &kind, &text);
        doc.put(&block, "id", id)
            .map_err(|_| "page block list no longer exists")?;
    }
    Ok(())
}

fn block_map(doc: &AutoCommit, list: &ObjId, id: &str) -> Option<(usize, ObjId)> {
    let index = block_index(doc, list, id)?;
    match doc.get(list, index) {
        Ok(Some((Value::Object(ObjType::Map), block))) => Some((index, block)),
        _ => None,
    }
}

fn valid_kind(kind: &str) -> bool {
    matches!(
        kind,
        "paragraph" | "divider" | "heading1" | "heading2" | "heading3"
    ) || kind
        .strip_prefix("bullet:")
        .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| n <= 5))
        || kind
            .strip_prefix("numbered:")
            .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| n <= 5))
        || kind.strip_prefix("checklist:").is_some_and(|rest| {
            let mut parts = rest.split(':');
            parts
                .next()
                .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| n <= 5))
                && matches!(parts.next(), Some("0" | "1"))
                && parts.next().is_none()
        })
        || kind.starts_with("code:")
}

fn insert_block(
    doc: &mut AutoCommit,
    list: &ObjId,
    index: usize,
    id: &str,
    kind: &str,
    text: &str,
) -> Result<(), String> {
    if Uuid::parse_str(id).is_err() || !valid_kind(kind) {
        return Err("block operation is invalid".into());
    }
    let block = doc
        .insert_object(list, index.min(doc.length(list)), ObjType::Map)
        .map_err(|_| "page block list no longer exists")?;
    doc.put(&block, "id", id)
        .map_err(|_| "page block list no longer exists")?;
    doc.put(&block, "kind", kind)
        .map_err(|_| "page block list no longer exists")?;
    let text_obj = doc
        .put_object(&block, "text", ObjType::Text)
        .map_err(|_| "page block list no longer exists")?;
    if !text.is_empty() {
        doc.splice_text(&text_obj, 0, 0, text)
            .map_err(|_| "block text could not be written")?;
    }
    Ok(())
}

fn byte_to_char_index(text: &str, byte: usize) -> Option<usize> {
    text.get(..byte).map(|prefix| prefix.chars().count())
}

#[allow(
    clippy::too_many_lines,
    reason = "keeping the exhaustive BlockEditOp interpreter in one match makes its validation and mutation rules auditable together"
)]
fn apply_block_op(doc: &mut AutoCommit, list: &ObjId, op: &BlockEditOp) -> Result<(), String> {
    match op {
        BlockEditOp::Insert {
            block_id: id,
            after,
            kind,
            text,
        } => {
            if block_index(doc, list, id).is_some() {
                return Err("block already exists".into());
            }
            let index = match after {
                Some(after) => block_index(doc, list, after)
                    .map(|index| index.saturating_add(1))
                    .ok_or_else(|| "block predecessor no longer exists".to_string())?,
                None => 0,
            };
            insert_block(doc, list, index, id, kind, text)
        }
        BlockEditOp::Delete { block_id: id } => {
            let (index, _) =
                block_map(doc, list, id).ok_or_else(|| "block no longer exists".to_string())?;
            doc.delete(list, index)
                .map_err(|_| "page block list no longer exists".into())
        }
        BlockEditOp::SetKind { block_id: id, kind } => {
            if !valid_kind(kind) {
                return Err("block kind is invalid".into());
            }
            let (_, block) =
                block_map(doc, list, id).ok_or_else(|| "block no longer exists".to_string())?;
            doc.put(&block, "kind", kind)
                .map_err(|_| "page block list no longer exists".into())
        }
        BlockEditOp::SetChecklist { block_id: id, done } => {
            let (_, block) =
                block_map(doc, list, id).ok_or_else(|| "block no longer exists".to_string())?;
            let Some(kind) = block_kind_value(doc, &block) else {
                return Err("block kind no longer exists".into());
            };
            let Some((prefix, _)) = kind.rsplit_once(':') else {
                return Err("block is not a checklist".into());
            };
            if !prefix.starts_with("checklist:") {
                return Err("block is not a checklist".into());
            }
            doc.put(&block, "kind", format!("{prefix}:{}", i32::from(*done)))
                .map_err(|_| "page block list no longer exists".into())
        }
        BlockEditOp::TextSplice {
            block_id: id,
            start,
            end,
            text,
        } => {
            let (_, block) =
                block_map(doc, list, id).ok_or_else(|| "block no longer exists".to_string())?;
            let Ok(Some((Value::Object(ObjType::Text), text_obj))) = doc.get(&block, "text") else {
                return Err("block text no longer exists".into());
            };
            let current = doc
                .text(&text_obj)
                .map_err(|_| "block text no longer exists")?;
            if start > end || *end > current.len() {
                return Err("block splice is out of bounds".into());
            }
            let (Some(start), Some(end)) = (
                byte_to_char_index(&current, *start),
                byte_to_char_index(&current, *end),
            ) else {
                return Err("block splice is not on a character boundary".into());
            };
            let deleted = isize::try_from(end.saturating_sub(start))
                .map_err(|_| "block splice is too large")?;
            doc.splice_text(&text_obj, start, deleted, text)
                .map_err(|_| "block text could not be written".into())
        }
        BlockEditOp::Move {
            block_id: id,
            after,
        } => {
            if after.as_deref() == Some(id) {
                return Err("block cannot follow itself".into());
            }
            let (old_index, block) =
                block_map(doc, list, id).ok_or_else(|| "block no longer exists".to_string())?;
            let kind = block_kind_value(doc, &block)
                .ok_or_else(|| "block kind no longer exists".to_string())?;
            let text = match doc.get(&block, "text") {
                Ok(Some((Value::Object(ObjType::Text), text))) => {
                    doc.text(&text).map_err(|_| "block text no longer exists")?
                }
                _ => return Err("block text no longer exists".into()),
            };
            // Automerge has no list move primitive: this is a delete + insert.
            // Preserve the Notes-owned native marks (and their migration
            // marker) explicitly so a reorder cannot silently strip formatting
            // or page links from the moved block.
            let Ok(Some((Value::Object(ObjType::Text), text_obj))) = doc.get(&block, "text") else {
                return Err("block text no longer exists".into());
            };
            let marks = doc
                .marks(&text_obj)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|mark| inline_mark_span(&text, &mark))
                .collect::<Vec<_>>();
            let had_marks_version = matches!(
                doc.get(&block, "notes_marks_version"),
                Ok(Some((Value::Scalar(value), _))) if value.to_i64() == Some(1)
            );
            doc.delete(list, old_index)
                .map_err(|_| "page block list no longer exists")?;
            let index = match after {
                Some(after) => block_index(doc, list, after)
                    .map(|index| index.saturating_add(1))
                    .ok_or_else(|| "block predecessor no longer exists".to_string())?,
                None => 0,
            };
            insert_block(doc, list, index, id, &kind, &text)?;
            if had_marks_version {
                let (_, inserted) = block_map(doc, list, id)
                    .ok_or_else(|| "moved block no longer exists".to_string())?;
                let Ok(Some((Value::Object(ObjType::Text), inserted_text))) =
                    doc.get(&inserted, "text")
                else {
                    return Err("moved block text no longer exists".into());
                };
                replace_native_marks(doc, &inserted, &inserted_text, &text, &marks)?;
            }
            Ok(())
        }
    }
}

fn block_kind_value(doc: &AutoCommit, block: &ObjId) -> Option<String> {
    match doc.get(block, "kind") {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            automerge::ScalarValue::Str(kind) => Some(kind.to_string()),
            _ => None,
        },
        _ => None,
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
    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end native-mark regression keeps reorder, Unicode, and merge evidence together"
    )]
    fn native_inline_marks_survive_reorder_and_unicode_text_edits() {
        let mut doc = AutoCommit::new();
        let page = Uuid::new_v4();
        let block = Uuid::new_v4().to_string();
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id: page,
                title: "Formatting".into(),
                parent: None,
                order: 0,
            },
        );
        NotesDoc::apply_block_ops(
            &mut doc,
            page,
            &[BlockEditOp::Insert {
                block_id: block.clone(),
                after: None,
                kind: "paragraph".into(),
                text: "café link".into(),
            }],
        )
        .unwrap();
        let target = Uuid::new_v4();
        let expected = vec![
            InlineMarkSpan {
                start: 0,
                end: 5,
                mark: InlineMark::Bold,
            },
            InlineMarkSpan {
                start: 6,
                end: 10,
                mark: InlineMark::Link {
                    page: target.to_string(),
                },
            },
        ];
        NotesDoc::set_block_marks(&mut doc, page, &block, &expected).unwrap();
        assert!(NotesDoc::page_marks_are_canonical(&doc, page));
        assert_eq!(NotesDoc::page_block_marks(&doc, page)[0].1, expected);

        // A Notes block move is represented as delete+insert, so this pins
        // the explicit native-mark preservation in that action interpreter.
        NotesDoc::apply_block_ops(
            &mut doc,
            page,
            &[BlockEditOp::Move {
                block_id: block.clone(),
                after: None,
            }],
        )
        .unwrap();
        assert_eq!(NotesDoc::page_block_marks(&doc, page)[0].1, expected);

        // The mark positions are native character anchors, not a brittle
        // JSON byte offset: an adjacent Unicode edit keeps the spans readable.
        NotesDoc::apply_block_ops(
            &mut doc,
            page,
            &[BlockEditOp::TextSplice {
                block_id: block,
                start: 0,
                end: 0,
                text: "✓ ".into(),
            }],
        )
        .unwrap();
        let after = NotesDoc::page_block_marks(&doc, page);
        assert_eq!(after.len(), 1);
        assert!(
            after[0]
                .1
                .iter()
                .any(|span| matches!(span.mark, InlineMark::Bold))
        );
        assert!(
            after[0]
                .1
                .iter()
                .any(|span| matches!(span.mark, InlineMark::Link { .. }))
        );

        // A late-joining peer's text edit merges back without turning the
        // native formatting/link anchors into stale plain-text state.
        let mut peer = AutoCommit::load(&doc.save()).unwrap();
        let moved = after[0].0.clone();
        let end = peer
            .text(
                NotesDoc::block_text_obj(
                    &peer,
                    &NotesDoc::page_blocks_list(&peer, page).unwrap(),
                    0,
                )
                .unwrap(),
            )
            .unwrap()
            .len();
        NotesDoc::apply_block_ops(
            &mut peer,
            page,
            &[BlockEditOp::TextSplice {
                block_id: moved,
                start: end,
                end,
                text: "!".into(),
            }],
        )
        .unwrap();
        let mut merged_peer = AutoCommit::load(&peer.save()).unwrap();
        doc.merge(&mut merged_peer).unwrap();
        assert!(
            NotesDoc::page_block_marks(&doc, page)[0]
                .1
                .iter()
                .any(|span| matches!(span.mark, InlineMark::Link { .. }))
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
        let blocks = NotesDoc::page_blocks_list(&doc, id).unwrap();
        let _t = NotesDoc::push_block(&mut doc, &blocks, "paragraph", "hi");
        assert_eq!(doc.length(&blocks), 1);
        assert_eq!(NotesDoc::block_kind(&doc, &blocks, 0), "paragraph");
        let tid = NotesDoc::block_text_obj(&doc, &blocks, 0).unwrap();
        assert_eq!(doc.text(&tid).unwrap(), "hi");
    }

    #[test]
    fn legacy_block_identity_is_read_only_until_an_action_persists_it() {
        let mut doc = AutoCommit::new();
        let page = Uuid::new_v4();
        NotesDoc::upsert_page_meta(
            &mut doc,
            &PageMeta {
                id: page,
                title: "Legacy".into(),
                parent: None,
                order: 0,
            },
        );
        let list = NotesDoc::page_blocks_list(&doc, page).unwrap();
        NotesDoc::push_block(&mut doc, &list, "paragraph", "old");
        let (_, map) = doc.get(&list, 0).unwrap().unwrap();
        doc.delete(&map, "id").unwrap();
        let legacy = ui::widget::legacy_block_id(&list, 0, "paragraph", "old");
        assert_eq!(NotesDoc::page_block_snapshots(&doc, page)[0].0, legacy);

        NotesDoc::apply_block_ops(
            &mut doc,
            page,
            &[BlockEditOp::TextSplice {
                block_id: legacy,
                start: 0,
                end: 3,
                text: "new".into(),
            }],
        )
        .unwrap();
        assert_eq!(NotesDoc::page_block_snapshots(&doc, page)[0].1, "new");
        assert!(
            block_id(&doc, &map).is_some(),
            "action persisted the legacy id"
        );
    }
}
