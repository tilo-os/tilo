//! CRDT-backed view preferences for Logs.
//!
//! The journal catalogue is a host projection — it never enters Automerge.
//! What *does* sync is the collaborative investigation view: severity
//! threshold, unit and boot filters, free-text search, and which entry is
//! selected. Live tail and refresh stay Effects on this process.
//!
//! ```text
//! root
//! └── "view" (Map)
//!     ├── "priority"  (Str)   # min severity 0..=7
//!     ├── "unit"      (Str)   # exact unit, or "" for all
//!     ├── "boot"      (Str)   # exact boot id, or "" for all
//!     ├── "search"    (Text)  # collaborative search string
//!     └── "selected"  (Str)   # entry resource id, or "" for none
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

use crate::model::Filter;

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

    fn get_str(doc: &AutoCommit, map: &ObjId, key: &str) -> Option<String> {
        match Self::single(doc, map, key) {
            Ok(Some((Value::Scalar(s), _))) => s.as_str().map(ToOwned::to_owned),
            Ok(Some((Value::Object(ObjType::Text), tid))) => doc.text(&tid).ok(),
            Ok(None | Some(_)) | Err(_) => None,
        }
    }

    fn put_str(doc: &mut AutoCommit, map: &ObjId, key: &str, value: &str) {
        let _ = doc.put(map, key, value);
    }

    fn text_obj(doc: &mut AutoCommit, map: &ObjId, key: &str) -> ObjId {
        match doc.get(map, key) {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => doc
                .put_object(map, key, ObjType::Text)
                .unwrap_or_else(|_| map.clone()),
        }
    }

    pub fn filter(doc: &AutoCommit) -> Filter {
        let Some(map) = Self::existing_view_map(doc).ok().flatten() else {
            return Filter::default();
        };
        Filter {
            min_priority: Self::get_str(doc, &map, "priority")
                .and_then(|s| s.parse().ok())
                .map_or(7, |v: i64| v.clamp(0, 7)),
            unit: Self::get_str(doc, &map, "unit").unwrap_or_default(),
            boot: Self::get_str(doc, &map, "boot").unwrap_or_default(),
            search: Self::get_str(doc, &map, "search").unwrap_or_default(),
        }
    }

    pub fn set_priority(doc: &mut AutoCommit, value: i64) {
        let m = Self::view_map(doc);
        Self::put_str(doc, &m, "priority", &value.clamp(0, 7).to_string());
    }

    pub fn set_unit(doc: &mut AutoCommit, value: &str) {
        let m = Self::view_map(doc);
        Self::put_str(doc, &m, "unit", value);
    }

    pub fn set_boot(doc: &mut AutoCommit, value: &str) {
        let m = Self::view_map(doc);
        Self::put_str(doc, &m, "boot", value);
    }

    pub fn set_search(doc: &mut AutoCommit, value: &str) {
        let m = Self::view_map(doc);
        let t = Self::text_obj(doc, &m, "search");
        ui::doc::splice_text_diffed(doc, &t, value);
    }

    pub fn clear_filters(doc: &mut AutoCommit) {
        Self::set_priority(doc, 7);
        Self::set_unit(doc, "");
        Self::set_boot(doc, "");
        Self::set_search(doc, "");
    }

    pub fn selected_entry(doc: &AutoCommit) -> Option<String> {
        Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, "selected"))
            .filter(|s| !s.is_empty())
    }

    pub fn set_selected_entry(doc: &mut AutoCommit, entry_id: Option<&str>) {
        let m = Self::view_map(doc);
        Self::put_str(doc, &m, "selected", entry_id.unwrap_or(""));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StoredValueError {
    WrongType,
    Conflict,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_round_trips_shared_preferences() {
        let mut doc = AutoCommit::new();
        ViewDoc::set_priority(&mut doc, 3);
        ViewDoc::set_unit(&mut doc, "shell.service");
        ViewDoc::set_boot(&mut doc, "boot-a");
        ViewDoc::set_search(&mut doc, "iroh");
        ViewDoc::set_selected_entry(&mut doc, Some("logs.entry.abc"));
        doc.commit();

        let filter = ViewDoc::filter(&doc);
        assert_eq!(filter.min_priority, 3);
        assert_eq!(filter.unit, "shell.service");
        assert_eq!(filter.boot, "boot-a");
        assert_eq!(filter.search, "iroh");
        assert_eq!(
            ViewDoc::selected_entry(&doc).as_deref(),
            Some("logs.entry.abc")
        );

        ViewDoc::clear_filters(&mut doc);
        doc.commit();
        assert!(ViewDoc::filter(&doc).is_clear());
    }
}
