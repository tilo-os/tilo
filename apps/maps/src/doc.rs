//! CRDT persistence for the Maps app.
//!
//! The whole [`MapModel`] is serialised to JSON and stored in a single
//! Automerge `Text` object at `root["maps"]`. This mirrors the notes app's
//! Phase-1 storage shim: it gives us free undo/redo (full-doc snapshots)
//! and live replication across collaborating peers, while keeping the data
//! model a plain serde type. A later phase could promote individual fields
//! to native Automerge structures for finer-grained merges.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};

use crate::model::MapModel;

pub struct MapsDoc;

impl MapsDoc {
    /// Read the model from the CRDT, if it has been seeded.
    pub fn load(doc: &AutoCommit) -> Option<MapModel> {
        if let Ok(Some((Value::Object(ObjType::Text), id))) = doc.get(&ROOT, "maps") {
            let s = doc.text(&id).ok()?;
            serde_json::from_str(&s).ok()
        } else {
            None
        }
    }

    /// Write the model into the CRDT, splicing the `Text` blob only when the
    /// serialisation actually changed (avoids op churn on idle frames).
    pub fn save(doc: &mut AutoCommit, model: &MapModel) {
        let id = match doc.get(&ROOT, "maps") {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => doc
                .put_object(&ROOT, "maps", ObjType::Text)
                .expect("put_object maps text"),
        };
        let json = match serde_json::to_string(model) {
            Ok(j) => j,
            Err(_) => return,
        };
        let cur = doc.text(&id).unwrap_or_default();
        if cur != json {
            let n = cur.chars().count() as isize;
            let _ = doc.splice_text(&id, 0, n, &json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample;

    #[test]
    fn roundtrip_through_crdt() {
        let mut doc = AutoCommit::new();
        let model = sample::default_model();
        MapsDoc::save(&mut doc, &model);
        let loaded = MapsDoc::load(&doc).expect("loads back");
        assert_eq!(loaded.places.len(), model.places.len());
        assert_eq!(loaded.selected, model.selected);
        assert_eq!(loaded.route.is_some(), true);
        assert_eq!(loaded.transit.len(), model.transit.len());
    }

    #[test]
    fn load_empty_doc_is_none() {
        let doc = AutoCommit::new();
        assert!(MapsDoc::load(&doc).is_none());
    }
}
