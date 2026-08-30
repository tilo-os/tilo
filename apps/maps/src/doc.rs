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
            _ => match doc.put_object(&ROOT, "maps", ObjType::Text) {
                Ok(id) => id,
                Err(_) => return,
            },
        };
        let Ok(json) = serde_json::to_string(model) else {
            return;
        };
        ui::doc::splice_text_diffed(doc, &id, &json);
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
        assert!(loaded.route.is_some());
        assert_eq!(loaded.transit.len(), model.transit.len());
    }

    #[test]
    fn load_empty_doc_is_none() {
        let doc = AutoCommit::new();
        assert!(MapsDoc::load(&doc).is_none());
    }

    #[test]
    fn small_model_edit_is_a_bounded_splice() {
        // A one-char change to the serialized model must splice only the diff,
        // not rewrite the whole JSON blob.
        let mut doc = AutoCommit::new();
        let mut model = sample::default_model();
        MapsDoc::save(&mut doc, &model);
        let before = doc.save().len();
        model.recents[0].push('!');
        MapsDoc::save(&mut doc, &model);
        let grew = doc.save().len() - before;
        assert_eq!(
            MapsDoc::load(&doc).unwrap().recents[0],
            model.recents[0],
            "value round-trips"
        );
        assert!(
            grew < 40,
            "one-char model edit grew doc by {grew} bytes — diff regressed"
        );
    }

    #[test]
    fn repeated_save_keeps_doc_bounded() {
        let mut doc = AutoCommit::new();
        let model = sample::default_model();
        MapsDoc::save(&mut doc, &model);
        let a = doc.save().len();
        for _ in 0..50 {
            MapsDoc::save(&mut doc, &model);
        }
        let b = doc.save().len();
        assert_eq!(
            a, b,
            "re-saving the same model grew the document {a} -> {b}"
        );
    }
}
