//! Automerge document layer.
//!
//! What the user decided lives in the CRDT: which file is open, and the edits
//! applied to it. That makes every edit undoable through `State::commit_action`
//! and replicable to a collaborator for free.
//!
//! ```text
//! root
//! ├── "image"  (Str)   # serde_json of Option<OpenImage>  — path + name + intrinsic size
//! └── "edits"  (Str)   # serde_json of Edits              — crop rect + quarter turns
//! ```
//!
//! # What is deliberately not here
//!
//! The pixels. The document holds a path and two small edit fields and never
//! the image itself, which is the same invariant
//! `crates/doc/qdoc/src/blobstore.rs` documents for a document's blobs: an
//! automerge change carrying a multi-megabyte payload exceeds the 64 KiB
//! gossipsub RPC cap, is dropped by the transport without an error, and every
//! subsequent change names an ancestor the far replica will never receive — so
//! the peer freezes permanently while both ends still report themselves
//! connected. A path plus a crop rect is a change of a few dozen bytes.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, Value};

use crate::model::{Edits, OpenImage};

pub struct PhotosDoc;

impl PhotosDoc {
    /// True when the document has never been written to.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        !matches!(doc.get(&ROOT, "edits"), Ok(Some((Value::Scalar(_), _))))
    }

    /// Seed default state: **no image open** and no edits.
    ///
    /// There is no sample photograph here and no fabricated recent list. An
    /// app with nothing open shows an empty state that says so; content only
    /// ever arrives from a file the user actually opened.
    /// Idempotent — only writes fields that are missing.
    pub fn seed(doc: &mut AutoCommit) {
        if Self::get_str(doc, "image").is_none() {
            Self::write_json(doc, "image", &Option::<OpenImage>::None);
        }
        if Self::get_str(doc, "edits").is_none() {
            Self::write_json(doc, "edits", &Edits::default());
        }
    }

    /// The photograph currently open, if any.
    pub fn image(doc: &AutoCommit) -> Option<OpenImage> {
        Self::read_json::<Option<OpenImage>>(doc, "image").flatten()
    }

    /// Open `image`, resetting the edits — a newly opened file starts
    /// untouched, never inheriting the previous one's crop.
    pub fn set_image(doc: &mut AutoCommit, image: Option<&OpenImage>) {
        Self::write_json(doc, "image", &image.cloned());
        Self::write_json(doc, "edits", &Edits::default());
    }

    /// The edits in force.
    pub fn edits(doc: &AutoCommit) -> Edits {
        Self::read_json::<Edits>(doc, "edits").unwrap_or_default()
    }

    /// Replace the edits in force.
    pub fn set_edits(doc: &mut AutoCommit, edits: &Edits) {
        Self::write_json(doc, "edits", edits);
    }

    // -- scalar helpers ----------------------------------------------------

    fn get_str(doc: &AutoCommit, key: &str) -> Option<String> {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Scalar(s), _))) => s.to_str().map(|s| s.to_string()),
            _ => None,
        }
    }

    fn write_json<T: serde::Serialize>(doc: &mut AutoCommit, key: &str, value: &T) {
        if let Ok(s) = serde_json::to_string(value) {
            let _ = doc.put(&ROOT, key, s);
        }
    }

    fn read_json<T: serde::de::DeserializeOwned>(doc: &AutoCommit, key: &str) -> Option<T> {
        let raw = Self::get_str(doc, key)?;
        serde_json::from_str(&raw).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rect01;

    #[test]
    fn a_seeded_document_has_no_image_open() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        assert!(
            PhotosDoc::image(&doc).is_none(),
            "a fresh document must not fabricate a photograph"
        );
        assert!(PhotosDoc::edits(&doc).is_identity());
    }

    #[test]
    fn seeding_twice_does_not_discard_state() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/x/harbour.png", 800, 600);
        PhotosDoc::set_image(&mut doc, Some(&img));
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());

        PhotosDoc::seed(&mut doc);
        assert_eq!(PhotosDoc::image(&doc).unwrap().name, "harbour.png");
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 1);
    }

    #[test]
    fn edits_round_trip_through_the_crdt() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let e = Edits {
            crop: Rect01 {
                x: 0.25,
                y: 0.5,
                w: 0.5,
                h: 0.25,
            },
            rotate_quarters: 3,
        };
        PhotosDoc::set_edits(&mut doc, &e);
        assert_eq!(PhotosDoc::edits(&doc), e);
    }

    #[test]
    fn opening_a_new_image_clears_the_previous_crop() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        let img = OpenImage::from_path("/x/other.png", 10, 10);
        PhotosDoc::set_image(&mut doc, Some(&img));
        assert!(
            PhotosDoc::edits(&doc).is_identity(),
            "a newly opened photograph must start untouched"
        );
    }

    /// The invariant the blob store exists to protect, asserted here rather
    /// than assumed: a document that has an image open is still tiny, because
    /// the pixels are not in it.
    #[test]
    fn the_document_never_carries_the_pixels() {
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let img = OpenImage::from_path("/var/tmp/qos/home/quinten/harbour.png", 4000, 3000);
        PhotosDoc::set_image(&mut doc, Some(&img));
        PhotosDoc::set_edits(&mut doc, &Edits::default().rotated_right());
        let saved = doc.save().len();
        assert!(
            saved < 4096,
            "the document grew to {saved} bytes; an image must never travel inside a change"
        );
    }
}
