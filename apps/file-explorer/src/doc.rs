//! The Files document: the person's bookmarked places, in order.
//!
//! It also queues the filesystem changes made from Files
//! ([`crate::file_ops`]): each is a document action, carried out by the
//! device it names and settled with what it changed.
//!
//! What a window shows — the folder, its listing, the selection, the
//! clipboard, navigation history — is the window's own, and how it shows it
//! (list or grid, sort, item size, hidden files) is the person's preference
//! ([`crate::resource::FilesPreferences`]). The places are stored as
//! [`crate::places`] names them, so they resolve on every device.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

use crate::model::{SortBy, SortOrder, ViewMode};

const PLACES: &str = "places";
const OPS: &str = "ops";
/// How many carried-out filesystem operations the document keeps, so recent
/// ones can still be undone.
const KEPT_OPS: usize = 64;

/// Stateless helpers over the Files document.
pub struct PlacesDoc;

impl PlacesDoc {
    fn existing_list(doc: &AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, PLACES) {
            Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
            _ => None,
        }
    }

    fn list(doc: &mut AutoCommit) -> Result<ObjId, automerge::AutomergeError> {
        match Self::existing_list(doc) {
            Some(list) => Ok(list),
            None => doc.put_object(&ROOT, PLACES, ObjType::List),
        }
    }

    /// A new document's places: [`crate::places::DEFAULT_PLACES`].
    ///
    /// # Errors
    /// The document's write error.
    pub fn initialize(doc: &mut AutoCommit) -> Result<(), automerge::AutomergeError> {
        let list = doc.put_object(&ROOT, PLACES, ObjType::List)?;
        for (index, place) in crate::places::DEFAULT_PLACES.iter().enumerate() {
            doc.insert(&list, index, *place)?;
        }
        Ok(())
    }

    /// The bookmarked places, in order.
    pub fn places(doc: &AutoCommit) -> Vec<String> {
        let Some(list) = Self::existing_list(doc) else {
            return Vec::new();
        };
        (0..doc.length(&list))
            .filter_map(|index| match doc.get(&list, index) {
                Ok(Some((Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
                _ => None,
            })
            .collect()
    }

    fn position(doc: &AutoCommit, list: &ObjId, place: &str) -> Option<usize> {
        (0..doc.length(list)).find(|index| {
            matches!(doc.get(list, *index), Ok(Some((Value::Scalar(value), _)))
                if value.as_str() == Some(place))
        })
    }

    /// Bookmark `place` at `at` (the end when `None` or past it), unless it is
    /// bookmarked already.
    ///
    /// # Errors
    /// The document's write error.
    pub fn add(
        doc: &mut AutoCommit,
        place: &str,
        at: Option<usize>,
    ) -> Result<(), automerge::AutomergeError> {
        let list = Self::list(doc)?;
        if Self::position(doc, &list, place).is_some() {
            return Ok(());
        }
        let len = doc.length(&list);
        doc.insert(&list, at.unwrap_or(len).min(len), place)
    }

    /// Forget `place`.
    ///
    /// # Errors
    /// The document's write error.
    pub fn remove(doc: &mut AutoCommit, place: &str) -> Result<(), automerge::AutomergeError> {
        let Some(list) = Self::existing_list(doc) else {
            return Ok(());
        };
        while let Some(index) = Self::position(doc, &list, place) {
            doc.delete(&list, index)?;
        }
        Ok(())
    }

    /// Move `place` to position `to` among the places.
    ///
    /// # Errors
    /// The document's write error.
    pub fn move_to(
        doc: &mut AutoCommit,
        place: &str,
        to: usize,
    ) -> Result<(), automerge::AutomergeError> {
        let Some(list) = Self::existing_list(doc) else {
            return Ok(());
        };
        let Some(from) = Self::position(doc, &list, place) else {
            return Ok(());
        };
        doc.delete(&list, from)?;
        let len = doc.length(&list);
        doc.insert(&list, to.min(len), place)
    }
}

/// Stateless helpers over the filesystem operation queue.
pub struct OpsDoc;

impl OpsDoc {
    fn existing(doc: &AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, OPS) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => None,
        }
    }

    fn map(doc: &mut AutoCommit) -> Result<ObjId, automerge::AutomergeError> {
        match Self::existing(doc) {
            Some(map) => Ok(map),
            None => doc.put_object(&ROOT, OPS, ObjType::Map),
        }
    }

    fn text(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
            _ => None,
        }
    }

    fn order(doc: &AutoCommit, obj: &ObjId) -> i64 {
        match doc.get(obj, "order") {
            Ok(Some((Value::Scalar(value), _))) => value.as_i64().unwrap_or_default(),
            _ => 0,
        }
    }

    /// The id an operation queued as `id` gets: `id` itself, or the first
    /// `id.N` not yet taken when the same action is queued again (a redo).
    pub fn fresh_id(doc: &AutoCommit, id: &str) -> String {
        let Some(map) = Self::existing(doc) else {
            return id.to_owned();
        };
        let taken = |candidate: &str| matches!(doc.get(&map, candidate), Ok(Some(_)));
        if !taken(id) {
            return id.to_owned();
        }
        // One more than the operations queued: at least one id is free.
        let bound = doc.length(&map) + 1;
        (1..=bound)
            .map(|n| format!("{id}.{n}"))
            .find(|candidate| !taken(candidate))
            .unwrap_or_else(|| format!("{id}.{bound}"))
    }

    /// Queue `op` for `device` under [`Self::fresh_id`] of `id`.
    ///
    /// # Errors
    /// The document's write error.
    pub fn queue(
        doc: &mut AutoCommit,
        id: &str,
        device: &str,
        op: &crate::file_ops::FileOp,
    ) -> Result<(), automerge::AutomergeError> {
        let id = Self::fresh_id(doc, id);
        let next = Self::ops(doc).iter().map(|op| op.order).max().unwrap_or(0) + 1;
        let map = Self::map(doc)?;
        let entry = doc.put_object(&map, &id, ObjType::Map)?;
        doc.put(&entry, "device", device)?;
        doc.put(
            &entry,
            "op",
            serde_json::to_string(op).unwrap_or_default().as_str(),
        )?;
        doc.put(&entry, "order", next)?;
        Ok(())
    }

    /// Record `outcome` for the operation `id`, keeping only the most recent
    /// carried-out operations.
    ///
    /// # Errors
    /// The document's write error.
    pub fn settle(
        doc: &mut AutoCommit,
        id: &str,
        outcome: &crate::file_ops::Outcome,
    ) -> Result<(), automerge::AutomergeError> {
        let Some(map) = Self::existing(doc) else {
            return Ok(());
        };
        let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&map, id) else {
            return Ok(());
        };
        doc.put(
            &entry,
            "outcome",
            serde_json::to_string(outcome).unwrap_or_default().as_str(),
        )?;
        let mut settled: Vec<(i64, String)> = Self::ops(doc)
            .into_iter()
            .filter(|op| op.outcome.is_some())
            .map(|op| (op.order, op.id))
            .collect();
        settled.sort();
        let excess = settled.len().saturating_sub(KEPT_OPS);
        for (_, id) in settled.into_iter().take(excess) {
            doc.delete(&map, id.as_str())?;
        }
        Ok(())
    }

    /// The queued operations, oldest first.
    pub fn ops(doc: &AutoCommit) -> Vec<crate::file_ops::QueuedOp> {
        let Some(map) = Self::existing(doc) else {
            return Vec::new();
        };
        let mut ops: Vec<crate::file_ops::QueuedOp> = doc
            .keys(&map)
            .filter_map(|id| {
                let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&map, id.as_str())
                else {
                    return None;
                };
                Some(crate::file_ops::QueuedOp {
                    device: Self::text(doc, &entry, "device")?,
                    op: serde_json::from_str(&Self::text(doc, &entry, "op")?).ok()?,
                    outcome: Self::text(doc, &entry, "outcome")
                        .and_then(|outcome| serde_json::from_str(&outcome).ok()),
                    order: Self::order(doc, &entry),
                    id,
                })
            })
            .collect();
        ops.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
        ops
    }
}

pub fn view_mode_token(mode: ViewMode) -> &'static str {
    match mode {
        ViewMode::List => "list",
        ViewMode::Grid => "grid",
    }
}

pub fn parse_view_mode(token: &str) -> ViewMode {
    match token {
        "grid" => ViewMode::Grid,
        _ => ViewMode::List,
    }
}

pub fn sort_key_token(sort: SortBy) -> &'static str {
    match sort {
        SortBy::Name => "name",
        SortBy::Size => "size",
        SortBy::Modified => "modified",
        SortBy::Type => "type",
    }
}

pub fn parse_sort_key(token: &str) -> SortBy {
    match token {
        "size" => SortBy::Size,
        "modified" => SortBy::Modified,
        "type" => SortBy::Type,
        _ => SortBy::Name,
    }
}

pub fn sort_order_token(order: SortOrder) -> &'static str {
    match order {
        SortOrder::Ascending => "ascending",
        SortOrder::Descending => "descending",
    }
}

pub fn parse_sort_order(token: &str) -> SortOrder {
    match token {
        "descending" => SortOrder::Descending,
        _ => SortOrder::Ascending,
    }
}
