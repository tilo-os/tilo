//! CRDT-backed view preferences for Passwords.
//!
//! The encrypted vault is device-local — secrets never enter Automerge. What
//! *does* sync is the collaborative vault view: the sidebar filter and which
//! opaque item id is selected. Reveal, clipboard clear, and unlock
//! stay process-local Effects / session state.
//!
//! ```text
//! root
//! └── "view" (Map)
//!     ├── "filter"   (Str)   # all | favorites | login | api-key | note | wifi | other
//!     └── "selected" (Str)   # opaque secret id, or "" for none
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

    pub fn filter(doc: &AutoCommit) -> String {
        Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, "filter"))
            .unwrap_or_else(|| "all".into())
    }

    pub fn set_filter(doc: &mut AutoCommit, filter: &str) {
        let m = Self::view_map(doc);
        Self::put_str(doc, &m, "filter", filter);
    }

    pub fn selected_id(doc: &AutoCommit) -> Option<String> {
        Self::existing_view_map(doc)
            .ok()
            .flatten()
            .and_then(|map| Self::get_str(doc, &map, "selected"))
            .filter(|s| !s.is_empty())
    }

    pub fn set_selected_id(doc: &mut AutoCommit, id: Option<&str>) {
        let m = Self::view_map(doc);
        Self::put_str(doc, &m, "selected", id.unwrap_or(""));
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
        ViewDoc::set_filter(&mut doc, "favorites");
        ViewDoc::set_selected_id(&mut doc, Some("opaque-id"));
        doc.commit();

        assert_eq!(ViewDoc::filter(&doc), "favorites");
        assert_eq!(ViewDoc::selected_id(&doc).as_deref(), Some("opaque-id"));

        ViewDoc::set_filter(&mut doc, "all");
        ViewDoc::set_selected_id(&mut doc, None);
        doc.commit();
        assert_eq!(ViewDoc::filter(&doc), "all");
        assert!(ViewDoc::selected_id(&doc).is_none());
    }
}
