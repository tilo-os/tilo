//! Automerge document model for the hex editor.
//!
//! The byte buffer is the shared, collaborative state, so it lives in the
//! CRDT. Automerge has a native `Bytes` scalar that stores an arbitrary
//! blob, which is exactly right for "the file we're editing":
//!
//! ```text
//! root
//! ├── "bytes"  (Bytes)  # the editable buffer
//! └── "meta"   (Map)
//!     ├── "cursor"        (Int)
//!     ├── "bytes_per_row" (Int)
//!     ├── "mode"          (Str)  # "ovr" | "ins"
//!     └── "endian"        (Str)  # "le"  | "be"
//! ```
//!
//! Storing the whole buffer as one `Bytes` scalar means an edit replaces
//! the blob rather than splicing — fine for the modest buffers a hex
//! editor opens, and it keeps the snapshot-based undo (which the toolkit
//! drives by cloning the whole document) trivially correct. Cursor +
//! view config round-trip so a reopened / re-synced document restores
//! exactly where the user was.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{EditMode, Endian, HexModel};

/// Stateless namespace for the document helpers.
pub struct HexDoc;

impl HexDoc {
    /// True if the document already carries a `bytes` blob — i.e. some
    /// instance has seeded it. A fresh `AutoCommit` returns false.
    pub fn is_seeded(doc: &AutoCommit) -> bool {
        matches!(doc.get(&ROOT, "bytes"), Ok(Some((Value::Scalar(_), _))))
    }

    /// Get-or-create the `meta` map. `None` only if `put_object` on the root
    /// itself fails, which does not happen in practice; callers treat that
    /// the same as any other best-effort meta write.
    fn meta_map(doc: &mut AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, "meta") {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => doc.put_object(&ROOT, "meta", ObjType::Map).ok(),
        }
    }

    /// Write the whole model (bytes + view state) into the document. Used to
    /// seed / replace the shared buffer wholesale (open a file, adopt a peer),
    /// not for incremental edits — those go through the action model, which
    /// writes only the [`write_bytes`](Self::write_bytes) blob.
    pub fn write_model(doc: &mut AutoCommit, model: &HexModel) {
        Self::write_bytes(doc, model.bytes());
        Self::write_meta(doc, model);
    }

    /// Write only the editable byte buffer. This is the sole mutation the
    /// [`crate::actions`] byte-edit actions perform, so an insert / delete /
    /// overwrite replays byte-identically and never disturbs the presentation
    /// meta (cursor, mode, endianness).
    pub fn write_bytes(doc: &mut AutoCommit, bytes: &[u8]) {
        let _ = doc.put(&ROOT, "bytes", ScalarValue::Bytes(bytes.to_vec()));
    }

    /// Write only the presentation meta (cursor, width, edit mode, endianness).
    /// This is view state, not a collaborative edit, so it never crosses the
    /// action model — it is a plain register write that round-trips a reopened
    /// document to where the user was.
    pub fn write_meta(doc: &mut AutoCommit, model: &HexModel) {
        let Some(meta) = Self::meta_map(doc) else {
            return;
        };
        let _ = doc.put(&meta, "cursor", model.cursor() as i64);
        let _ = doc.put(&meta, "bytes_per_row", model.bytes_per_row() as i64);
        let _ = doc.put(
            &meta,
            "mode",
            match model.mode() {
                EditMode::Overwrite => "ovr",
                EditMode::Insert => "ins",
            },
        );
        let _ = doc.put(
            &meta,
            "endian",
            match model.endian() {
                Endian::Little => "le",
                Endian::Big => "be",
            },
        );
    }

    /// Read the byte buffer out of the document, if present.
    pub fn read_bytes(doc: &AutoCommit) -> Option<Vec<u8>> {
        match doc.get(&ROOT, "bytes") {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Bytes(b) => Some(b.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn read_meta_int(doc: &AutoCommit, key: &str) -> Option<i64> {
        let Ok(Some((Value::Object(ObjType::Map), meta))) = doc.get(&ROOT, "meta") else {
            return None;
        };
        match doc.get(&meta, key) {
            Ok(Some((Value::Scalar(v), _))) => v.to_i64(),
            _ => None,
        }
    }

    fn read_meta_str(doc: &AutoCommit, key: &str) -> Option<String> {
        let Ok(Some((Value::Object(ObjType::Map), meta))) = doc.get(&ROOT, "meta") else {
            return None;
        };
        match doc.get(&meta, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Rebuild a [`HexModel`] from the document. Returns None when the
    /// document hasn't been seeded with a `bytes` blob yet.
    pub fn read_model(doc: &AutoCommit) -> Option<HexModel> {
        let bytes = Self::read_bytes(doc)?;
        let mut model = HexModel::new(bytes);
        if let Some(bpr) = Self::read_meta_int(doc, "bytes_per_row") {
            model.set_bytes_per_row(bpr.max(1) as usize);
        }
        if let Some(c) = Self::read_meta_int(doc, "cursor") {
            model.move_to(c.max(0) as usize, false);
        }
        if let Some(m) = Self::read_meta_str(doc, "mode")
            && (m == "ins") != (model.mode() == EditMode::Insert)
        {
            model.toggle_mode();
        }
        if let Some(e) = Self::read_meta_str(doc, "endian")
            && (e == "be") != (model.endian() == Endian::Big)
        {
            model.toggle_endian();
        }
        Some(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Column;

    #[test]
    fn round_trips_bytes_and_cursor() {
        let mut doc = AutoCommit::new();
        let mut m = HexModel::new(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        m.move_to(2, false);
        HexDoc::write_model(&mut doc, &m);

        let back = HexDoc::read_model(&doc).unwrap();
        assert_eq!(back.bytes(), &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(back.cursor(), 2);
    }

    #[test]
    fn round_trips_view_config() {
        let mut doc = AutoCommit::new();
        let mut m = HexModel::new(vec![0; 32]);
        m.set_bytes_per_row(8);
        m.toggle_mode(); // Insert
        m.toggle_endian(); // Big
        let _ = Column::Hex; // (column is per-peer presence, not persisted)
        HexDoc::write_model(&mut doc, &m);

        let back = HexDoc::read_model(&doc).unwrap();
        assert_eq!(back.bytes_per_row(), 8);
        assert_eq!(back.mode(), EditMode::Insert);
        assert_eq!(back.endian(), Endian::Big);
    }

    #[test]
    fn unseeded_doc_reads_none() {
        let doc = AutoCommit::new();
        assert!(!HexDoc::is_seeded(&doc));
        assert!(HexDoc::read_model(&doc).is_none());
    }

    #[test]
    fn edit_replaces_blob() {
        let mut doc = AutoCommit::new();
        let mut m = HexModel::new(vec![0x01, 0x02]);
        HexDoc::write_model(&mut doc, &m);
        m.set_byte(0, 0xFF);
        HexDoc::write_model(&mut doc, &m);
        assert_eq!(HexDoc::read_bytes(&doc).unwrap(), vec![0xFF, 0x02]);
    }

    #[test]
    fn two_peers_converge_on_latest_write() {
        // A seeds, B loads, B edits, A merges — last-writer per the blob.
        let mut a = AutoCommit::new();
        HexDoc::write_model(&mut a, &HexModel::new(vec![0x00, 0x00]));
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let mut bm = HexDoc::read_model(&b).unwrap();
        bm.set_byte(1, 0x42);
        HexDoc::write_model(&mut b, &bm);
        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();
        assert_eq!(HexDoc::read_bytes(&a).unwrap()[1], 0x42);
    }
}
