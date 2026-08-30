//! Automerge document layer for Books: everything the user can change lives
//! in the shared CRDT so undo/redo and live collaboration work for free.
//!
//! ```text
//! root
//! ├── "settings"        (Map)  font_scale(f64) line_spacing(f64) font(str) theme(str)
//! ├── "library"         (Map keyed by book-id)  { path, title, author, subject, added }
//! ├── "watched_folders" (List of str)  folders auto-imported into the library
//! ├── "progress"        (Map keyed by book-id)  { chapter, page, fraction }
//! └── "annotations"     (Map keyed by uuid)     { doc_id, kind, chapter, block,
//!                                                 start, end, excerpt, note,
//!                                                 color, created }
//! ```
//!
//! Library, progress and annotations are all keyed by the content-hash book
//! id, so a book's shelf entry, reading position, and every annotation
//! travel together across devices and to anyone it's shared with. The
//! reader's transient view (open book, page, selection) stays per-peer,
//! out of the document.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use uuid::Uuid;

use std::path::PathBuf;

use crate::model::{
    AnnoKind, Annotation, ColorScheme, FontEntry, FontKind, HiColor, LibraryEntry, LibraryPrefs,
    LibrarySort, Progress, ReadMode, ReadTheme, Settings, default_fonts, default_schemes, now_ms,
};

/// Pack an sRGB triple into a single int for the CRDT.
fn pack_rgb(c: [u8; 3]) -> i64 {
    (i64::from(c[0]) << 16) | (i64::from(c[1]) << 8) | i64::from(c[2])
}
fn unpack_rgb(v: i64) -> [u8; 3] {
    [
        ((v >> 16) & 0xFF) as u8,
        ((v >> 8) & 0xFF) as u8,
        (v & 0xFF) as u8,
    ]
}

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

    fn scalar_bool(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<bool> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Boolean(b) => Some(*b),
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
        matches!(
            doc.get(&ROOT, "settings"),
            Ok(Some((Value::Object(ObjType::Map), _)))
        )
    }

    pub fn write_settings(doc: &mut AutoCommit, s: &Settings) {
        let m = Self::root_map(doc, "settings");
        let _ = doc.put(&m, "font_scale", f64::from(s.font_scale));
        let _ = doc.put(&m, "line_spacing", f64::from(s.line_spacing));
        let _ = doc.put(&m, "font_id", s.font_id.as_str());
        let _ = doc.put(&m, "theme", s.theme.as_str());
        let _ = doc.put(&m, "scheme_id", s.scheme_id.as_str());
        let _ = doc.put(&m, "mode", s.mode.as_str());
        let _ = doc.put(&m, "composition", s.composition.as_str());
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
        if let Some(v) = Self::scalar_str(doc, &m, "font_id") {
            s.font_id = v;
        }
        if let Some(v) = Self::scalar_str(doc, &m, "theme") {
            s.theme = ReadTheme::from_str(&v);
        }
        if let Some(v) = Self::scalar_str(doc, &m, "scheme_id") {
            s.scheme_id = v;
        }
        if let Some(v) = Self::scalar_str(doc, &m, "mode") {
            s.mode = ReadMode::from_str(&v);
        }
        if let Some(v) = Self::scalar_str(doc, &m, "composition") {
            s.composition = crate::model::CompositionStyle::from_str(&v);
        }
        s
    }

    // --- font registry -----------------------------------------------------

    /// Read the registered fonts. When none are stored (a fresh library) the
    /// built-in [`default_fonts`] set is returned without writing, so the
    /// document stays clean until the user actually edits the registry.
    pub fn read_fonts(doc: &AutoCommit) -> Vec<FontEntry> {
        let list = match doc.get(&ROOT, "fonts") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => return default_fonts(),
        };
        let mut out = Vec::new();
        for i in 0..doc.length(&list) {
            let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&list, i) else {
                continue;
            };
            let id = Self::scalar_str(doc, &e, "id").unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            out.push(FontEntry {
                kind: FontKind::from_code(
                    &Self::scalar_str(doc, &e, "kind").unwrap_or_else(|| "default".into()),
                ),
                name: Self::scalar_str(doc, &e, "name").unwrap_or_else(|| id.clone()),
                size_mult: Self::scalar_f64(doc, &e, "size_mult").map_or(1.0, |v| {
                    (v as f32).clamp(FontEntry::MULT_MIN, FontEntry::MULT_MAX)
                }),
                shown: Self::scalar_bool(doc, &e, "shown").unwrap_or(true),
                id,
            });
        }
        if out.is_empty() { default_fonts() } else { out }
    }

    /// Replace the whole font registry (the manager edits the list wholesale).
    pub fn write_fonts(doc: &mut AutoCommit, fonts: &[FontEntry]) {
        let list = doc
            .put_object(&ROOT, "fonts", ObjType::List)
            .expect("put_object list never fails");
        for (i, f) in fonts.iter().enumerate() {
            let e = doc
                .insert_object(&list, i, ObjType::Map)
                .expect("insert_object map never fails");
            let _ = doc.put(&e, "id", f.id.as_str());
            let _ = doc.put(&e, "kind", f.kind.as_code());
            let _ = doc.put(&e, "name", f.name.as_str());
            let _ = doc.put(&e, "size_mult", f64::from(f.size_mult));
            let _ = doc.put(&e, "shown", f.shown);
        }
    }

    /// The registry-wide default font id (used when a book has no explicit
    /// per-reader selection). Empty string means "the first registered font".
    pub fn read_default_font(doc: &AutoCommit) -> String {
        match doc.get(&ROOT, "settings") {
            Ok(Some((Value::Object(ObjType::Map), m))) => {
                Self::scalar_str(doc, &m, "default_font").unwrap_or_default()
            }
            _ => String::new(),
        }
    }

    pub fn write_default_font(doc: &mut AutoCommit, id: &str) {
        let m = Self::root_map(doc, "settings");
        let _ = doc.put(&m, "default_font", id);
    }

    // --- library shelf preferences ----------------------------------------

    /// Read the shelf's sort order + hide-read flag (defaults when unset).
    pub fn read_library_prefs(doc: &AutoCommit) -> LibraryPrefs {
        let mut p = LibraryPrefs::default();
        let m = match doc.get(&ROOT, "settings") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return p,
        };
        if let Some(v) = Self::scalar_str(doc, &m, "library_sort") {
            p.sort = LibrarySort::from_str(&v);
        }
        if let Some(v) = Self::scalar_bool(doc, &m, "library_hide_read") {
            p.hide_read = v;
        }
        p
    }

    pub fn write_library_prefs(doc: &mut AutoCommit, p: &LibraryPrefs) {
        let m = Self::root_map(doc, "settings");
        let _ = doc.put(&m, "library_sort", p.sort.as_str());
        let _ = doc.put(&m, "library_hide_read", p.hide_read);
    }

    // --- the library: books the reader has added, keyed by content-hash id -

    /// Add or update a book in the library. Re-importing the same file (same
    /// content id) refreshes its recorded path + metadata while preserving the
    /// original "added" time, so the shelf's add order stays stable.
    pub fn upsert_library_entry(doc: &mut AutoCommit, e: &LibraryEntry) {
        let lib = Self::root_map(doc, "library");
        let existing_added = match doc.get(&lib, e.id.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Self::scalar_i64(doc, &id, "added")
                .filter(|&v| v > 0)
                .map(|v| v as u64),
            _ => None,
        };
        let entry = Self::get_or_make_map(doc, &lib, &e.id);
        let _ = doc.put(&entry, "path", e.path.to_string_lossy().as_ref());
        let _ = doc.put(&entry, "title", e.title.as_str());
        let _ = doc.put(&entry, "author", e.author.as_str());
        let _ = doc.put(&entry, "subject", e.subject.as_str());
        let added = existing_added.unwrap_or_else(|| if e.added > 0 { e.added } else { now_ms() });
        let _ = doc.put(&entry, "added", added as i64);
    }

    /// Drop a book from the library. Its annotations + progress are left in place
    /// (keyed by content id) so re-adding the same file restores them.
    pub fn remove_library_entry(doc: &mut AutoCommit, id: &str) {
        let lib = Self::root_map(doc, "library");
        let _ = doc.delete(&lib, id);
    }

    /// Every book in the library, ordered by when it was added (oldest first) so
    /// the shelf has a stable base order the sort control then re-orders.
    pub fn read_library_entries(doc: &AutoCommit) -> Vec<LibraryEntry> {
        let lib = match doc.get(&ROOT, "library") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        for key in doc.keys(&lib) {
            let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&lib, key.as_str()) else {
                continue;
            };
            let Some(path) = Self::scalar_str(doc, &e, "path") else {
                continue;
            };
            if path.is_empty() {
                continue;
            }
            out.push(LibraryEntry {
                id: key,
                path: PathBuf::from(path),
                title: Self::scalar_str(doc, &e, "title").unwrap_or_default(),
                author: Self::scalar_str(doc, &e, "author").unwrap_or_default(),
                subject: Self::scalar_str(doc, &e, "subject").unwrap_or_default(),
                added: Self::scalar_i64(doc, &e, "added").unwrap_or(0).max(0) as u64,
            });
        }
        out.sort_by(|a, b| a.added.cmp(&b.added).then_with(|| a.id.cmp(&b.id)));
        out
    }

    // --- watched folders: explicit, user-managed auto-import roots ---------

    /// The folders the reader has asked to watch: any new `.epub` found in one is
    /// pulled into the library automatically. Empty by default — the app never
    /// watches a hidden magic folder the reader didn't choose.
    pub fn read_watched_folders(doc: &AutoCommit) -> Vec<PathBuf> {
        let list = match doc.get(&ROOT, "watched_folders") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        for i in 0..doc.length(&list) {
            if let Ok(Some((Value::Scalar(v), _))) = doc.get(&list, i)
                && let ScalarValue::Str(s) = v.as_ref()
                && !s.is_empty()
            {
                out.push(PathBuf::from(s.to_string()));
            }
        }
        out
    }

    /// Replace the watched-folder set wholesale (the Library settings pane edits
    /// it as a list).
    pub fn write_watched_folders(doc: &mut AutoCommit, folders: &[PathBuf]) {
        let list = doc
            .put_object(&ROOT, "watched_folders", ObjType::List)
            .expect("put_object list never fails");
        for (i, f) in folders.iter().enumerate() {
            let _ = doc.insert(&list, i, f.to_string_lossy().as_ref());
        }
    }

    // --- colour schemes ----------------------------------------------------

    /// Read the registered colour schemes, falling back to the built-ins when
    /// none are stored (kept out of the document until the user edits them).
    pub fn read_schemes(doc: &AutoCommit) -> Vec<ColorScheme> {
        let list = match doc.get(&ROOT, "schemes") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => return default_schemes(),
        };
        let mut out = Vec::new();
        for i in 0..doc.length(&list) {
            let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&list, i) else {
                continue;
            };
            let id = Self::scalar_str(doc, &e, "id").unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            let page = unpack_rgb(Self::scalar_i64(doc, &e, "page").unwrap_or(0));
            out.push(ColorScheme {
                name: Self::scalar_str(doc, &e, "name").unwrap_or_else(|| id.clone()),
                page,
                ink: unpack_rgb(Self::scalar_i64(doc, &e, "ink").unwrap_or(0)),
                accent: unpack_rgb(Self::scalar_i64(doc, &e, "accent").unwrap_or(0)),
                dark: Self::scalar_bool(doc, &e, "dark")
                    .unwrap_or_else(|| crate::model::luminance(page) < 0.5),
                enabled: Self::scalar_bool(doc, &e, "enabled").unwrap_or(true),
                builtin: Self::scalar_bool(doc, &e, "builtin").unwrap_or(false),
                id,
            });
        }
        if out.is_empty() {
            default_schemes()
        } else {
            out
        }
    }

    /// Replace the whole scheme registry (the manager edits it wholesale).
    pub fn write_schemes(doc: &mut AutoCommit, schemes: &[ColorScheme]) {
        let list = doc
            .put_object(&ROOT, "schemes", ObjType::List)
            .expect("put_object list never fails");
        for (i, s) in schemes.iter().enumerate() {
            let e = doc
                .insert_object(&list, i, ObjType::Map)
                .expect("insert_object map never fails");
            let _ = doc.put(&e, "id", s.id.as_str());
            let _ = doc.put(&e, "name", s.name.as_str());
            let _ = doc.put(&e, "page", pack_rgb(s.page));
            let _ = doc.put(&e, "ink", pack_rgb(s.ink));
            let _ = doc.put(&e, "accent", pack_rgb(s.accent));
            let _ = doc.put(&e, "dark", s.dark);
            let _ = doc.put(&e, "enabled", s.enabled);
            let _ = doc.put(&e, "builtin", s.builtin);
        }
    }

    // --- progress ----------------------------------------------------------

    pub fn write_progress(doc: &mut AutoCommit, book_id: &str, p: &Progress) {
        let progress = Self::root_map(doc, "progress");
        let entry = Self::get_or_make_map(doc, &progress, book_id);
        let _ = doc.put(&entry, "chapter", p.chapter as i64);
        let _ = doc.put(&entry, "page", p.page as i64);
        let _ = doc.put(&entry, "fraction", f64::from(p.fraction));
        // Preserve a previously recorded open time; only advance it (0 = never).
        if p.opened > 0 {
            let _ = doc.put(&entry, "opened", p.opened as i64);
        }
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
            opened: Self::scalar_i64(doc, &entry, "opened").unwrap_or(0).max(0) as u64,
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
        let _ = doc.put(&entry, "group", a.group.to_string().as_str());
    }

    pub fn remove_annotation(doc: &mut AutoCommit, id: Uuid) {
        let map = Self::annotations_map(doc);
        let _ = doc.delete(&map, id.to_string().as_str());
    }

    /// Remove every annotation row belonging to one logical group — a highlight
    /// (its per-block pieces) together with all of its comments.
    pub fn remove_group(doc: &mut AutoCommit, group: Uuid) {
        let ids: Vec<Uuid> = Self::list_annotations(doc)
            .into_iter()
            .filter(|a| a.group == group)
            .map(|a| a.id)
            .collect();
        for id in ids {
            Self::remove_annotation(doc, id);
        }
    }

    pub fn list_annotations(doc: &AutoCommit) -> Vec<Annotation> {
        let map = match doc.get(&ROOT, "annotations") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        for key in doc.keys(&map) {
            let Ok(id) = key.parse::<Uuid>() else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&map, key.as_str()) else {
                continue;
            };
            let a = Annotation {
                id,
                doc_id: Self::scalar_str(doc, &e, "doc_id").unwrap_or_default(),
                kind: AnnoKind::from_str(&Self::scalar_str(doc, &e, "kind").unwrap_or_default()),
                chapter: Self::scalar_i64(doc, &e, "chapter").unwrap_or(0).max(0) as usize,
                block: Self::scalar_i64(doc, &e, "block").unwrap_or(0).max(0) as usize,
                start: Self::scalar_i64(doc, &e, "start").unwrap_or(0).max(0) as usize,
                end: Self::scalar_i64(doc, &e, "end").unwrap_or(0).max(0) as usize,
                excerpt: Self::scalar_str(doc, &e, "excerpt").unwrap_or_default(),
                note: Self::scalar_str(doc, &e, "note").unwrap_or_default(),
                color: HiColor::from_str(&Self::scalar_str(doc, &e, "color").unwrap_or_default()),
                created: Self::scalar_i64(doc, &e, "created").unwrap_or(0).max(0) as u64,
                // Older rows predate grouping; fall back to a self-group (the
                // row's own id) so each behaves as a standalone mark.
                group: Self::scalar_str(doc, &e, "group")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(id),
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
            (a.chapter, a.block, a.start, a.created).cmp(&(b.chapter, b.block, b.start, b.created))
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
        let s = Settings {
            font_scale: 1.3,
            line_spacing: 1.45,
            font_id: "serif".into(),
            theme: ReadTheme::Dark,
            scheme_id: "midnight".into(),
            mode: crate::model::ReadMode::TwoUp,
            composition: crate::model::CompositionStyle::Ragged,
        };
        BooksDoc::write_settings(&mut doc, &s);
        let back = BooksDoc::read_settings(&doc);
        assert!((back.font_scale - 1.3).abs() < 0.001);
        assert!((back.line_spacing - 1.45).abs() < 0.001);
        assert_eq!(back.font_id, "serif");
        assert_eq!(back.theme, ReadTheme::Dark);
        assert_eq!(back.scheme_id, "midnight");
        assert_eq!(
            back.mode,
            crate::model::ReadMode::TwoUp,
            "reading mode persists"
        );
        assert_eq!(
            back.composition,
            crate::model::CompositionStyle::Ragged,
            "composition style persists"
        );
        // A fresh document defaults to the paged reading mode.
        assert_eq!(
            BooksDoc::read_settings(&AutoCommit::new()).mode,
            crate::model::ReadMode::Paged
        );
        assert_eq!(
            BooksDoc::read_settings(&AutoCommit::new()).composition,
            crate::model::CompositionStyle::Book
        );
    }

    #[test]
    fn fonts_registry_defaults_and_round_trips() {
        let mut doc = AutoCommit::new();
        // No stored registry -> the built-in defaults, without writing.
        let defaults = BooksDoc::read_fonts(&doc);
        assert!(defaults.iter().any(|f| f.kind == FontKind::Serif));

        // A custom registry (rename + hide + a named face) round-trips exactly.
        let mut custom = default_fonts();
        custom[0].name = "Book".into();
        custom[1].shown = false;
        custom.push(FontEntry::new(
            "u1",
            FontKind::Named("Literata".into()),
            "Literata",
            1.1,
            true,
        ));
        BooksDoc::write_fonts(&mut doc, &custom);
        let back = BooksDoc::read_fonts(&doc);
        assert_eq!(back.len(), custom.len());
        assert_eq!(back[0].name, "Book");
        assert!(!back[1].shown);
        assert_eq!(
            back.last().unwrap().kind,
            FontKind::Named("Literata".into())
        );
        assert!((back.last().unwrap().size_mult - 1.1).abs() < 0.001);
    }

    #[test]
    fn progress_round_trip() {
        let mut doc = AutoCommit::new();
        let p = Progress {
            chapter: 2,
            page: 5,
            fraction: 0.42,
            opened: 1720000000000,
        };
        BooksDoc::write_progress(&mut doc, "book:x", &p);
        let back = BooksDoc::read_progress(&doc, "book:x");
        assert_eq!(back.chapter, 2);
        assert_eq!(back.page, 5);
        assert!((back.fraction - 0.42).abs() < 0.001);
        assert_eq!(
            back.opened, 1720000000000,
            "open time persists for recent-sort"
        );
        // Unknown book -> default.
        assert_eq!(BooksDoc::read_progress(&doc, "nope"), Progress::default());
    }

    #[test]
    fn library_prefs_round_trip() {
        let mut doc = AutoCommit::new();
        assert_eq!(BooksDoc::read_library_prefs(&doc), LibraryPrefs::default());
        let p = LibraryPrefs {
            sort: LibrarySort::Recent,
            hide_read: true,
        };
        BooksDoc::write_library_prefs(&mut doc, &p);
        assert_eq!(BooksDoc::read_library_prefs(&doc), p);
    }

    #[test]
    fn schemes_registry_round_trips_with_a_custom_scheme() {
        let mut doc = AutoCommit::new();
        assert_eq!(BooksDoc::read_schemes(&doc).len(), default_schemes().len());
        let mut schemes = default_schemes();
        schemes[0].enabled = false; // disable a built-in
        schemes.push(ColorScheme::from_bg("u1", "Ocean", [0x0A, 0x22, 0x33]));
        BooksDoc::write_schemes(&mut doc, &schemes);
        let back = BooksDoc::read_schemes(&doc);
        assert_eq!(back.len(), schemes.len());
        assert!(!back[0].enabled, "disabled state round-trips");
        let custom = back.last().unwrap();
        assert_eq!(custom.name, "Ocean");
        assert_eq!(custom.page, [0x0A, 0x22, 0x33]);
        assert!(custom.dark && !custom.builtin);
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
    fn library_entries_round_trip_and_preserve_added_time() {
        use crate::model::LibraryEntry;
        let mut doc = AutoCommit::new();
        assert!(
            BooksDoc::read_library_entries(&doc).is_empty(),
            "fresh library is empty"
        );

        let e = LibraryEntry {
            id: "book:abc".into(),
            path: "/home/reader/Novel.epub".into(),
            title: "Novel".into(),
            author: "A. Writer".into(),
            subject: "Fiction".into(),
            added: 1_000,
        };
        BooksDoc::upsert_library_entry(&mut doc, &e);
        let back = BooksDoc::read_library_entries(&doc);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0], e);

        // Re-importing the same book from a new path (e.g. after sharing it into
        // the workspace) updates the path but keeps the original added time, so
        // the shelf order doesn't jump.
        let moved = LibraryEntry {
            path: "/var/workspace/Books/Novel.epub".into(),
            added: 9_999,
            ..e.clone()
        };
        BooksDoc::upsert_library_entry(&mut doc, &moved);
        let back = BooksDoc::read_library_entries(&doc);
        assert_eq!(
            back.len(),
            1,
            "same content id updates in place, not duplicated"
        );
        assert_eq!(
            back[0].path,
            std::path::PathBuf::from("/var/workspace/Books/Novel.epub")
        );
        assert_eq!(
            back[0].added, 1_000,
            "added time is preserved across a re-import"
        );

        BooksDoc::remove_library_entry(&mut doc, "book:abc");
        assert!(
            BooksDoc::read_library_entries(&doc).is_empty(),
            "removed from the shelf"
        );
    }

    #[test]
    fn watched_folders_round_trip() {
        let mut doc = AutoCommit::new();
        assert!(
            BooksDoc::read_watched_folders(&doc).is_empty(),
            "nothing watched by default"
        );
        let folders = vec![
            std::path::PathBuf::from("/home/reader/Books"),
            std::path::PathBuf::from("/var/workspace/Books"),
        ];
        BooksDoc::write_watched_folders(&mut doc, &folders);
        assert_eq!(BooksDoc::read_watched_folders(&doc), folders);
    }

    #[test]
    fn annotations_filtered_by_book() {
        let mut doc = AutoCommit::new();
        BooksDoc::upsert_annotation(
            &mut doc,
            &Annotation::new("book:a", AnnoKind::Bookmark, 0, 0),
        );
        BooksDoc::upsert_annotation(&mut doc, &Annotation::new("book:b", AnnoKind::Note, 0, 0));
        assert_eq!(BooksDoc::annotations_for(&doc, "book:a").len(), 1);
        assert_eq!(BooksDoc::annotations_for(&doc, "book:b").len(), 1);
    }
}
