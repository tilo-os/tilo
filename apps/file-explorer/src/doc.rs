//! Shared Files view preferences and bookmark paths.
//!
//! The live folder listing, selection, clipboard, and navigation history stay
//! on the widget. What syncs is the collaborative view: list or grid, sort,
//! zoom, hidden files, and the ordered favourite paths (places / bookmarks).

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

use crate::model::{SortBy, SortOrder, ViewMode};

/// Stateless helpers over the shared view-state document.
pub struct ViewDoc;

impl ViewDoc {
    fn single<'a>(
        doc: &'a AutoCommit,
        obj: &ObjId,
        key: &str,
    ) -> Result<Option<(Value<'a>, ObjId)>, StoredValueError> {
        let Ok(mut values) = doc.get_all(obj, key) else {
            return Err(StoredValueError::WrongType);
        };
        if values.len() > 1 {
            return Err(StoredValueError::Conflict);
        }
        Ok(values.pop())
    }

    fn existing_view_map(doc: &AutoCommit) -> Result<Option<ObjId>, StoredValueError> {
        match Self::single(doc, &ROOT, "view")? {
            None => Ok(None),
            Some((Value::Object(ObjType::Map), id)) => Ok(Some(id)),
            Some(_) => Err(StoredValueError::WrongType),
        }
    }

    fn view_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "view") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc.put_object(&ROOT, "view", ObjType::Map).unwrap_or(ROOT),
        }
    }

    fn bookmarks_list(doc: &mut AutoCommit) -> ObjId {
        let view = Self::view_map(doc);
        match doc.get(&view, "bookmarks") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => doc
                .put_object(&view, "bookmarks", ObjType::List)
                .unwrap_or(view),
        }
    }

    fn get_str(doc: &AutoCommit, map: &ObjId, key: &str) -> Option<String> {
        match Self::single(doc, map, key) {
            Ok(Some((Value::Scalar(value), _))) => value.as_str().map(ToOwned::to_owned),
            Ok(Some((Value::Object(ObjType::Text), id))) => doc.text(&id).ok(),
            Ok(None | Some(_)) | Err(_) => None,
        }
    }

    fn put_str(doc: &mut AutoCommit, map: &ObjId, key: &str, value: &str) {
        let _ = doc.put(map, key, value);
    }

    /// Whether a shared view map has been written. An empty document is not
    /// shared state and must not reset a local sidebar.
    pub fn is_shared(doc: &AutoCommit) -> bool {
        Self::existing_view_map(doc).ok().flatten().is_some()
    }

    pub fn view_mode(doc: &AutoCommit) -> String {
        Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, "mode"))
            .unwrap_or_else(|| view_mode_token(ViewMode::List).to_owned())
    }

    pub fn set_view_mode(doc: &mut AutoCommit, mode: &str) {
        let map = Self::view_map(doc);
        Self::put_str(doc, &map, "mode", mode);
    }

    pub fn sort_key(doc: &AutoCommit) -> String {
        Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, "sort"))
            .unwrap_or_else(|| sort_key_token(SortBy::Name).to_owned())
    }

    pub fn sort_order(doc: &AutoCommit) -> String {
        Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, "order"))
            .unwrap_or_else(|| sort_order_token(SortOrder::Ascending).to_owned())
    }

    pub fn set_sort(doc: &mut AutoCommit, key: &str, order: &str) {
        let map = Self::view_map(doc);
        Self::put_str(doc, &map, "sort", key);
        Self::put_str(doc, &map, "order", order);
    }

    pub fn zoom(doc: &AutoCommit) -> i32 {
        let Some(map) = Self::existing_view_map(doc).ok().flatten() else {
            return 0;
        };
        match Self::single(doc, &map, "zoom") {
            Ok(Some((Value::Scalar(value), _))) => value
                .as_i64()
                .and_then(|level| i32::try_from(level).ok())
                .unwrap_or(0),
            _ => 0,
        }
    }

    pub fn set_zoom(doc: &mut AutoCommit, level: i32) {
        let map = Self::view_map(doc);
        let _ = doc.put(&map, "zoom", i64::from(level));
    }

    pub fn show_hidden(doc: &AutoCommit) -> bool {
        Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, "hidden"))
            .as_deref()
            == Some("true")
    }

    pub fn set_show_hidden(doc: &mut AutoCommit, shown: bool) {
        let map = Self::view_map(doc);
        Self::put_str(doc, &map, "hidden", if shown { "true" } else { "false" });
    }

    pub fn bookmarks(doc: &AutoCommit) -> Vec<String> {
        let Some(view) = Self::existing_view_map(doc).ok().flatten() else {
            return Vec::new();
        };
        let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&view, "bookmarks") else {
            return Vec::new();
        };
        (0..doc.length(&list))
            .filter_map(|index| match doc.get(&list, index) {
                Ok(Some((Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
                _ => None,
            })
            .collect()
    }

    /// Replace the shared favourite order. Pins are rare, so the list is
    /// rewritten rather than diffed character-by-character.
    pub fn set_bookmarks(doc: &mut AutoCommit, paths: &[String]) {
        let list = Self::bookmarks_list(doc);
        let len = doc.length(&list);
        for index in (0..len).rev() {
            let _ = doc.delete(&list, index);
        }
        for (index, path) in paths.iter().enumerate() {
            let _ = doc.insert(&list, index, path.as_str());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoredValueError {
    WrongType,
    Conflict,
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
