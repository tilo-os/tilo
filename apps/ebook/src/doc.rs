//! Automerge document layer for Books.
//!
//! Everything the user can change lives in the shared CRDT so undo/redo and
//! live collaboration work for free:
//!
//! ```text
//! root
//! ├── "settings"    (Map)  font_scale(f64) line_spacing(f64) font(str) theme(str)
//! ├── "progress"    (Map keyed by book-id)  { chapter, page, fraction }
//! └── "annotations" (Map keyed by uuid)     { doc_id, kind, chapter, block,
//!                                             start, end, excerpt, note,
//!                                             color, created }
//! ```
//!
//! The reader's *transient* view (which book is open, the live page index, the
//! selected block) is per-peer and stays out of the document — two readers can
//! browse the same library and annotations while sitting on different pages.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value, ROOT};
use uuid::Uuid;

use crate::model::{AnnoKind, Annotation, HiColor, Progress, ReadFont, ReadTheme, Settings};

/// Stateless namespace of document helpers.
pub struct BooksDoc;

impl BooksDoc {
    // --- generic helpers ---------------------------------------------------

    fn get_or_make_map(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
        match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(parent, key, ObjType::Map)
                .expect("put_object map never fails"),
        }
    }

    fn root_map(doc: &mut AutoCommit, key: &str) -> ObjId {
        let root = ROOT;
        Self::get_or_make_map(doc, &root, key)
    }

    fn scalar_f64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<f64> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::F64(f) => Some(*f),
                ScalarValue::Int(i) => Some(*i as f64),
                ScalarValue::Uint(u) => Some(*u as f64),
                _ => None,
            },
            _ => None,
        }
    }

    fn scalar_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Int(i) => Some(*i),
                ScalarValue::Uint(u) => Some(*u as i64),
                ScalarValue::F64(f) => Some(*f as i64),
                _ => None,
            },
            _ => None,
        }
    }

    fn scalar_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    // --- settings ----------------------------------------------------------

    pub fn has_settings(doc: &AutoCommit) -> bool {
        matches!(doc.get(&ROOT, "settings"), Ok(Some((Value::Object(ObjType::Map), _))))
    }

    pub fn write_settings(doc: &mut AutoCommit, s: &Settings) {
        let m = Self::root_map(doc, "settings");
        let _ = doc.put(&m, "font_scale", s.font_scale as f64);
        let _ = doc.put(&m, "line_spacing", s.line_spacing as f64);
        let _ = doc.put(&m, "font", s.font.as_str());
        let _ = doc.put(&m, "theme", s.theme.as_str());
    }

    pub fn read_settings(doc: &AutoCommit) -> Settings {
        let mut s = Settings::default();
        let m = match doc.get(&ROOT, "settings") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return s,
        };
        if let Some(v) = Self::scalar_f64(doc, &m, "font_scale") {
            s.font_scale = (v as f32).clamp(Settings::FONT_MIN, Settings::FONT_MAX);
        }
        if let Some(v) = Self::scalar_f64(doc, &m, "line_spacing") {
            s.line_spacing = (v as f32).clamp(Settings::SPACING_MIN, Settings::SPACING_MAX);
        }
        if let Some(v) = Self::scalar_str(doc, &m, "font") {
            s.font = ReadFont::from_str(&v);
        }
        if let Some(v) = Self::scalar_str(doc, &m, "theme") {
            s.theme = ReadTheme::from_str(&v);
        }
        s
    }

    // --- progress ----------------------------------------------------------

    pub fn write_progress(doc: &mut AutoCommit, book_id: &str, p: &Progress) {
        let progress = Self::root_map(doc, "progress");
        let entry = Self::get_or_make_map(doc, &progress, book_id);
        let _ = doc.put(&entry, "chapter", p.chapter as i64);
        let _ = doc.put(&entry, "page", p.page as i64);
        let _ = doc.put(&entry, "fraction", p.fraction as f64);
    }

    pub fn read_progress(doc: &AutoCommit, book_id: &str) -> Progress {
        let progress = match doc.get(&ROOT, "progress") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Progress::default(),
        };
        let entry = match doc.get(&progress, book_id) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Progress::default(),
        };
        Progress {
            chapter: Self::scalar_i64(doc, &entry, "chapter").unwrap_or(0).max(0) as usize,
            page: Self::scalar_i64(doc, &entry, "page").unwrap_or(0).max(0) as usize,
            fraction: Self::scalar_f64(doc, &entry, "fraction").unwrap_or(0.0) as f32,
        }
    }

    // --- annotations -------------------------------------------------------

    fn annotations_map(doc: &mut AutoCommit) -> ObjId {
        Self::root_map(doc, "annotations")
    }

    pub fn upsert_annotation(doc: &mut AutoCommit, a: &Annotation) {
        let map = Self::annotations_map(doc);
        let key = a.id.to_string();
        let entry = Self::get_or_make_map(doc, &map, &key);
        let _ = doc.put(&entry, "doc_id", a.doc_id.as_str());
        let _ = doc.put(&entry, "kind", a.kind.as_str());
        let _ = doc.put(&entry, "chapter", a.chapter as i64);
        let _ = doc.put(&entry, "block", a.block as i64);
        let _ = doc.put(&entry, "start", a.start as i64);
        let _ = doc.put(&entry, "end", a.end as i64);
        let _ = doc.put(&entry, "excerpt", a.excerpt.as_str());
        let _ = doc.put(&entry, "note", a.note.as_str());
        let _ = doc.put(&entry, "color", a.color.as_str());
        let _ = doc.put(&entry, "created", a.created as i64);
    }

    pub fn remove_annotation(doc: &mut AutoCommit, id: Uuid) {
        let map = Self::annotations_map(doc);
        let _ = doc.delete(&map, id.to_string().as_str());
    }

    pub fn list_annotations(doc: &AutoCommit) -> Vec<Annotation> {
        let map = match doc.get(&ROOT, "annotations") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        for key in doc.keys(&map) {
            let Ok(id) = key.parse::<Uuid>() else { continue };
            let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&map, key.as_str())
            else {
                continue;
            };
            let a = Annotation {
                id,
                doc_id: Self::scalar_str(doc, &e, "doc_id").unwrap_or_default(),
                kind: AnnoKind::from_str(
                    &Self::scalar_str(doc, &e, "kind").unwrap_or_default(),
                ),
                chapter: Self::scalar_i64(doc, &e, "chapter").unwrap_or(0).max(0) as usize,
                block: Self::scalar_i64(doc, &e, "block").unwrap_or(0).max(0) as usize,
                start: Self::scalar_i64(doc, &e, "start").unwrap_or(0).max(0) as usize,
                end: Self::scalar_i64(doc, &e, "end").unwrap_or(0).max(0) as usize,
                excerpt: Self::scalar_str(doc, &e, "excerpt").unwrap_or_default(),
                note: Self::scalar_str(doc, &e, "note").unwrap_or_default(),
                color: HiColor::from_str(
                    &Self::scalar_str(doc, &e, "color").unwrap_or_default(),
                ),
                created: Self::scalar_i64(doc, &e, "created").unwrap_or(0).max(0) as u64,
            };
            out.push(a);
        }
        out
    }

    /// Annotations for one book, sorted by location then creation time.
    pub fn annotations_for(doc: &AutoCommit, book_id: &str) -> Vec<Annotation> {
        let mut v: Vec<Annotation> = Self::list_annotations(doc)
            .into_iter()
            .filter(|a| a.doc_id == book_id)
            .collect();
        v.sort_by(|a, b| {
            (a.chapter, a.block, a.start, a.created)
                .cmp(&(b.chapter, b.block, b.start, b.created))
        });
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AnnoKind, HiColor};

    #[test]
    fn settings_round_trip() {
        let mut doc = AutoCommit::new();
        let mut s = Settings::default();
        s.font_scale = 1.3;
        s.line_spacing = 1.45;
        s.font = ReadFont::Sans;
        s.theme = ReadTheme::Dark;
        BooksDoc::write_settings(&mut doc, &s);
        let back = BooksDoc::read_settings(&doc);
        assert!((back.font_scale - 1.3).abs() < 0.001);
        assert!((back.line_spacing - 1.45).abs() < 0.001);
        assert_eq!(back.font, ReadFont::Sans);
        assert_eq!(back.theme, ReadTheme::Dark);
    }

    #[test]
    fn progress_round_trip() {
        let mut doc = AutoCommit::new();
        let p = Progress { chapter: 2, page: 5, fraction: 0.42 };
        BooksDoc::write_progress(&mut doc, "book:x", &p);
        let back = BooksDoc::read_progress(&doc, "book:x");
        assert_eq!(back.chapter, 2);
        assert_eq!(back.page, 5);
        assert!((back.fraction - 0.42).abs() < 0.001);
        // Unknown book -> default.
        assert_eq!(BooksDoc::read_progress(&doc, "nope"), Progress::default());
    }

    #[test]
    fn annotations_upsert_list_remove() {
        let mut doc = AutoCommit::new();
        let a = Annotation::new("book:x", AnnoKind::Highlight, 1, 3)
            .with_excerpt("a passage")
            .with_color(HiColor::Green)
            .with_note("nice");
        let id = a.id;
        BooksDoc::upsert_annotation(&mut doc, &a);
        let list = BooksDoc::annotations_for(&doc, "book:x");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, AnnoKind::Highlight);
        assert_eq!(list[0].color, HiColor::Green);
        assert_eq!(list[0].note, "nice");
        // Update in place (no duplicate).
        let mut a2 = a.clone();
        a2.note = "edited".into();
        BooksDoc::upsert_annotation(&mut doc, &a2);
        assert_eq!(BooksDoc::annotations_for(&doc, "book:x").len(), 1);
        assert_eq!(BooksDoc::annotations_for(&doc, "book:x")[0].note, "edited");
        // Remove.
        BooksDoc::remove_annotation(&mut doc, id);
        assert!(BooksDoc::annotations_for(&doc, "book:x").is_empty());
    }

    #[test]
    fn annotations_filtered_by_book() {
        let mut doc = AutoCommit::new();
        BooksDoc::upsert_annotation(
            &mut doc,
            &Annotation::new("book:a", AnnoKind::Bookmark, 0, 0),
        );
        BooksDoc::upsert_annotation(
            &mut doc,
            &Annotation::new("book:b", AnnoKind::Note, 0, 0),
        );
        assert_eq!(BooksDoc::annotations_for(&doc, "book:a").len(), 1);
        assert_eq!(BooksDoc::annotations_for(&doc, "book:b").len(), 1);
    }
}
