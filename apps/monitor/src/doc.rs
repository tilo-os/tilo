//! CRDT-backed table layout for System Monitor.
//!
//! The process table and resource gauges are live readings of this machine —
//! they never enter Automerge. What *does* sync is how the table is laid
//! out: its sort and whether it shows the process tree. Which tab is showing,
//! the filter and the selected process belong to the window that shows them;
//! the person's preferences (cadence, kernel threads, confirm-kill) are
//! [`crate::prefs::MonitorPreferences`], a personal setting.
//!
//! ```text
//! root
//! └── "view" (Map)
//!     ├── "sort"      (Str)   # "pid" | "name" | "user" | "cpu" | "mem" | "threads"
//!     ├── "sort_desc" (Str)   # "true" | "false"
//!     └── "tree"      (Str)   # "true" | "false"
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

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

    fn flag(doc: &AutoCommit, key: &str, default: bool) -> bool {
        match Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, key))
            .as_deref()
        {
            Some("true") => true,
            Some("false") => false,
            _ => default,
        }
    }

    fn set_flag(doc: &mut AutoCommit, key: &str, value: bool) {
        let m = Self::view_map(doc);
        Self::put_str(doc, &m, key, if value { "true" } else { "false" });
    }

    pub fn sort_key(doc: &AutoCommit) -> String {
        Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, "sort"))
            .unwrap_or_else(|| "cpu".into())
    }

    pub fn set_sort_key(doc: &mut AutoCommit, key: &str) {
        let m = Self::view_map(doc);
        Self::put_str(doc, &m, "sort", key);
    }

    pub fn sort_desc(doc: &AutoCommit) -> bool {
        Self::flag(doc, "sort_desc", true)
    }

    pub fn set_sort_desc(doc: &mut AutoCommit, desc: bool) {
        Self::set_flag(doc, "sort_desc", desc);
    }

    pub fn tree_mode(doc: &AutoCommit) -> bool {
        Self::flag(doc, "tree", true)
    }

    pub fn set_tree_mode(doc: &mut AutoCommit, tree: bool) {
        Self::set_flag(doc, "tree", tree);
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
    fn view_round_trips_the_shared_table_layout() {
        let mut doc = AutoCommit::new();
        ViewDoc::set_sort_key(&mut doc, "name");
        ViewDoc::set_sort_desc(&mut doc, false);
        ViewDoc::set_tree_mode(&mut doc, false);
        doc.commit();

        assert_eq!(ViewDoc::sort_key(&doc), "name");
        assert!(!ViewDoc::sort_desc(&doc));
        assert!(!ViewDoc::tree_mode(&doc));
    }
}
