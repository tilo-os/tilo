//! Automerge document layer for Books: everything the user can change lives
//! in the shared CRDT so undo/redo and live collaboration work for free.
//!
//! ```text
//! root
//! ├── "settings"        (Map)  font_scale(f64) line_spacing(f64) font(str) theme(str)
//! ├── "library"         (Map keyed by book-id)  { title, author, subject, shared, added,
//! │                                               content? }
//! ├── "library_tombstones" (Map keyed by book-id) true
//! ├── "progress"        (Map keyed by book-id)  { chapter, page, fraction }
//! ├── "annotations"     (Map keyed by uuid)     { doc_id, kind, chapter, block,
//! │                                               start, end, excerpt, note,
//! │                                               color, created }
//! ├── "annotation_tombstones" (Map keyed by uuid) true
//! └── "highlight_tombstones"  (Map keyed by group uuid) true
//! ```
//!
//! Library, progress and annotations are all keyed by the content-hash book
//! id, so a book's shelf entry, reading position, and every annotation
//! travel together across devices and to anyone it's shared with. Annotation
//! Library, annotation and highlight tombstones are monotonic: a delete wins over an offline
//! edit/add that arrives later, rather than allowing a removed row to be
//! resurrected by a concurrent map write. Raw source
//! locations and watched folders are local capability state, never document
//! data. The reader's transient view (open book, page, selection) stays
//! per-peer, out of the document.

#[cfg(test)]
use automerge::ActorId;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use std::collections::BTreeMap;
use tilo_clock::unix_now_ms;
use ui::doc::{read_bool, read_number, read_scalar, read_str};
use uuid::Uuid;

/// Where an annotation keeps its note: rich text.
const TEXT: &str = "text";

use crate::model::{
    AnnoKind, Annotation, AnnotationAnchor, ColorScheme, FontEntry, FontKind, HiColor,
    LibraryEntry, LibraryPrefs, LibrarySort, Progress, ReadMode, ReadTheme, Settings,
    default_fonts, default_schemes,
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

    // `put_object` here only ever creates a fresh map at a key we either just
    // resolved as ROOT or as a map this same function created, so there is no
    // real fallback `ObjId` to substitute on error — a failure here means the
    // document itself is corrupt.
    #[allow(
        clippy::expect_used,
        reason = "put_object on ROOT or a map this fn created cannot fail short of document corruption, which has no meaningful ObjId fallback"
    )]
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

    /// Every concurrent map object stored at one root key. Fresh documents
    /// normally have one; lazily-created roots may conflict when two
    /// offline peers create the first child simultaneously, and reads must
    /// materialise all branches instead of accepting Automerge's one winner.
    fn root_maps(doc: &AutoCommit, key: &str) -> Vec<ObjId> {
        doc.get_all(&ROOT, key)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(value, id)| matches!(value, Value::Object(ObjType::Map)).then_some(id))
            .collect()
    }

    /// All map-valued entries below every concurrently-created root map,
    /// grouped by their logical key.  A lazy root can be created independently
    /// by two offline peers; accepting only `ReadDoc::get` would then hide one
    /// complete branch after merge.
    fn keyed_map_entries(doc: &AutoCommit, root_key: &str) -> BTreeMap<String, Vec<ObjId>> {
        let mut grouped = BTreeMap::<String, Vec<ObjId>>::new();
        for root in Self::root_maps(doc, root_key) {
            for key in doc.keys(&root) {
                for (value, id) in doc.get_all(&root, key.as_str()).unwrap_or_default() {
                    if matches!(value, Value::Object(ObjType::Map)) {
                        let entries = grouped.entry(key.clone()).or_default();
                        if !entries.contains(&id) {
                            entries.push(id);
                        }
                    }
                }
            }
        }
        grouped
    }

    /// Resolve every object for `key`, creating the missing keyed object in
    /// every live root branch. Updating all branches heals old first-use root
    /// conflicts instead of leaving a stale twin that can win a later read.
    fn get_or_make_keyed_maps(doc: &mut AutoCommit, root_key: &str, key: &str) -> Vec<ObjId> {
        let mut roots = Self::root_maps(doc, root_key);
        if roots.is_empty() {
            roots.push(Self::root_map(doc, root_key));
        }
        let mut out = Vec::new();
        for root in roots {
            let mut found = doc
                .get_all(&root, key)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(value, id)| {
                    matches!(value, Value::Object(ObjType::Map)).then_some(id)
                })
                .collect::<Vec<_>>();
            if found.is_empty()
                && let Ok(id) = doc.put_object(&root, key, ObjType::Map)
            {
                found.push(id);
            }
            for id in found {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        out
    }

    fn merged_str(doc: &AutoCommit, entries: &[ObjId], key: &str) -> Option<String> {
        entries
            .iter()
            .filter_map(|entry| read_str(doc, entry, key))
            .max()
    }

    /// An `Int`, a `Uint` reinterpreted as `i64`, or an `F64` truncated.
    fn scalar_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
        match read_scalar(doc, obj, key)? {
            ScalarValue::Int(i) => Some(i),
            ScalarValue::Uint(u) => Some(u as i64),
            ScalarValue::F64(f) => Some(f as i64),
            _ => None,
        }
    }

    // --- settings ----------------------------------------------------------

    pub fn has_settings(doc: &AutoCommit) -> bool {
        !Self::root_maps(doc, "settings").is_empty()
    }

    #[cfg(test)]
    pub(crate) fn write_settings(doc: &mut AutoCommit, s: &Settings) {
        let m = Self::root_map(doc, "settings");
        let _ = doc.put(&m, "font_scale", f64::from(s.font_scale));
        let _ = doc.put(&m, "line_spacing", f64::from(s.line_spacing));
        let _ = doc.put(&m, "font_id", s.font_id.as_str());
        let _ = doc.put(&m, "theme", s.theme.as_str());
        let _ = doc.put(&m, "scheme_id", s.scheme_id.as_str());
        let _ = doc.put(&m, "mode", s.mode.as_str());
        let _ = doc.put(&m, "composition", s.composition.as_str());
    }

    /// Write only fields changed by one authored settings gesture. This keeps
    /// concurrent changes to unrelated preferences (for example theme and
    /// line spacing) as independent Automerge register updates.
    pub(crate) fn write_settings_delta(doc: &mut AutoCommit, before: &Settings, after: &Settings) {
        let m = Self::root_map(doc, "settings");
        if before.font_scale.to_bits() != after.font_scale.to_bits() {
            let _ = doc.put(&m, "font_scale", f64::from(after.font_scale));
        }
        if before.line_spacing.to_bits() != after.line_spacing.to_bits() {
            let _ = doc.put(&m, "line_spacing", f64::from(after.line_spacing));
        }
        if before.font_id != after.font_id {
            let _ = doc.put(&m, "font_id", after.font_id.as_str());
        }
        if before.theme != after.theme {
            let _ = doc.put(&m, "theme", after.theme.as_str());
        }
        if before.scheme_id != after.scheme_id {
            let _ = doc.put(&m, "scheme_id", after.scheme_id.as_str());
        }
        if before.mode != after.mode {
            let _ = doc.put(&m, "mode", after.mode.as_str());
        }
        if before.composition != after.composition {
            let _ = doc.put(&m, "composition", after.composition.as_str());
        }
    }

    pub fn read_settings(doc: &AutoCommit) -> Settings {
        let mut s = Settings::default();
        let maps = Self::root_maps(doc, "settings");
        if maps.is_empty() {
            return s;
        }
        let f64_value = |key| maps.iter().find_map(|map| read_number(doc, map, key));
        let str_value = |key| maps.iter().find_map(|map| read_str(doc, map, key));
        if let Some(v) = f64_value("font_scale") {
            s.font_scale =
                (v as f32).clamp(Settings::READING_SCALE_MIN, Settings::READING_SCALE_MAX);
        }
        if let Some(v) = f64_value("line_spacing") {
            s.line_spacing = (v as f32).clamp(Settings::SPACING_MIN, Settings::SPACING_MAX);
        }
        if let Some(v) = str_value("font_id") {
            s.font_id = v;
        }
        if let Some(v) = str_value("theme") {
            s.theme = ReadTheme::from_str(&v);
        }
        if let Some(v) = str_value("scheme_id") {
            s.scheme_id = v;
        }
        if let Some(v) = str_value("mode") {
            s.mode = ReadMode::from_str(&v);
        }
        if let Some(v) = str_value("composition") {
            s.composition = crate::model::CompositionStyle::from_str(&v);
        }
        s
    }

    // --- font registry -----------------------------------------------------

    /// Read the registered fonts. When none are stored (a fresh library) the
    /// built-in [`default_fonts`] set is returned without writing, so the
    /// document stays clean until the user actually edits the registry.
    pub fn read_fonts(doc: &AutoCommit) -> Vec<FontEntry> {
        Self::read_registry(
            doc,
            default_fonts,
            ["fonts", "font_entries", "font_tombstones"],
            Self::read_font_entry,
            |font| &font.id,
        )
    }

    fn read_font_entry(
        doc: &AutoCommit,
        e: &ObjId,
        fallback_id: Option<&str>,
    ) -> Option<FontEntry> {
        let id = read_str(doc, e, "id").unwrap_or_default();
        let id = if id.is_empty() {
            fallback_id?.to_string()
        } else {
            id
        };
        Some(FontEntry {
            kind: FontKind::from_code(
                &read_str(doc, e, "kind").unwrap_or_else(|| "default".into()),
            ),
            name: read_str(doc, e, "name").unwrap_or_else(|| id.clone()),
            size_mult: read_number(doc, e, "size_mult").map_or(1.0, |v| {
                (v as f32).clamp(FontEntry::MULT_MIN, FontEntry::MULT_MAX)
            }),
            shown: read_bool(doc, e, "shown").unwrap_or(true),
            id,
        })
    }

    /// Upsert one stable font entity. Tombstoned ids are never resurrected by
    /// a delayed peer edit; a newly-added face always receives a fresh id.
    pub(crate) fn upsert_font(doc: &mut AutoCommit, font: &FontEntry) {
        if Self::has_tombstone(doc, "font_tombstones", &font.id) {
            return;
        }
        let entries = Self::root_map(doc, "font_entries");
        let entry = Self::get_or_make_map(doc, &entries, &font.id);
        let _ = doc.put(&entry, "id", font.id.as_str());
        let _ = doc.put(&entry, "kind", font.kind.as_code());
        let _ = doc.put(&entry, "name", font.name.as_str());
        let _ = doc.put(&entry, "size_mult", f64::from(font.size_mult));
        let _ = doc.put(&entry, "shown", font.shown);
    }

    pub(crate) fn remove_font(doc: &mut AutoCommit, id: &str) {
        let tombstones = Self::root_map(doc, "font_tombstones");
        let _ = doc.put(&tombstones, id, true);
        let entries = Self::root_map(doc, "font_entries");
        let _ = doc.delete(&entries, id);
    }

    /// Replace the whole font registry (the manager edits the list wholesale).
    ///
    /// # Panics
    ///
    /// Panics if creating the fresh `"fonts"` list (or a fresh entry map
    /// inside it) fails, which only happens if the document itself is
    /// corrupt — there is no meaningful `ObjId` to fall back to.
    #[allow(
        clippy::expect_used,
        reason = "put_object/insert_object on a freshly-created list cannot fail short of document corruption, which has no meaningful ObjId fallback"
    )]
    #[cfg(test)]
    pub(crate) fn write_fonts(doc: &mut AutoCommit, fonts: &[FontEntry]) {
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
        Self::root_maps(doc, "settings")
            .iter()
            .find_map(|map| read_str(doc, map, "default_font"))
            .unwrap_or_default()
    }

    pub(crate) fn write_default_font(doc: &mut AutoCommit, id: &str) {
        let m = Self::root_map(doc, "settings");
        let _ = doc.put(&m, "default_font", id);
    }

    // --- library shelf preferences ----------------------------------------

    /// Read the shelf's sort order + hide-read flag (defaults when unset).
    pub fn read_library_prefs(doc: &AutoCommit) -> LibraryPrefs {
        let mut p = LibraryPrefs::default();
        let maps = Self::root_maps(doc, "settings");
        if let Some(v) = maps
            .iter()
            .find_map(|map| read_str(doc, map, "library_sort"))
        {
            p.sort = LibrarySort::from_str(&v);
        }
        if let Some(v) = maps
            .iter()
            .find_map(|map| read_bool(doc, map, "library_hide_read"))
        {
            p.hide_read = v;
        }
        p
    }

    #[cfg(test)]
    pub(crate) fn write_library_prefs(doc: &mut AutoCommit, p: LibraryPrefs) {
        let m = Self::root_map(doc, "settings");
        let _ = doc.put(&m, "library_sort", p.sort.as_str());
        let _ = doc.put(&m, "library_hide_read", p.hide_read);
    }

    pub(crate) fn write_library_prefs_delta(
        doc: &mut AutoCommit,
        before: LibraryPrefs,
        after: LibraryPrefs,
    ) {
        let m = Self::root_map(doc, "settings");
        if before.sort != after.sort {
            let _ = doc.put(&m, "library_sort", after.sort.as_str());
        }
        if before.hide_read != after.hide_read {
            let _ = doc.put(&m, "library_hide_read", after.hide_read);
        }
    }

    // --- the library: books the reader has added, keyed by content-hash id -

    /// Add or update a book in the library. Re-importing the same file (same
    /// content id) refreshes its public metadata while preserving the original
    /// "added" time. Source paths are device-private capability grants and
    /// must never be written to the shared document.
    pub(crate) fn upsert_library_entry(doc: &mut AutoCommit, e: &LibraryEntry) {
        // Only an explicit, user-consented import may restore a removed shelf
        // row. Ordinary delayed metadata upserts must not resurrect it.
        if Self::library_is_removed(doc, &e.id) {
            return;
        }
        let entries = Self::get_or_make_keyed_maps(doc, "library", &e.id);
        let existing_added = entries
            .iter()
            .filter_map(|entry| Self::scalar_i64(doc, entry, "added"))
            .filter(|&value| value > 0)
            .min()
            .map(|value| value as u64);
        let was_shared = entries
            .iter()
            .any(|entry| read_bool(doc, entry, "shared").unwrap_or(false));
        let added =
            existing_added.unwrap_or_else(|| if e.added > 0 { e.added } else { unix_now_ms() });
        for entry in entries {
            let _ = doc.put(&entry, "title", e.title.as_str());
            let _ = doc.put(&entry, "author", e.author.as_str());
            let _ = doc.put(&entry, "subject", e.subject.as_str());
            let _ = doc.put(&entry, "shared", was_shared || e.shared);
            let _ = doc.put(&entry, "added", added as i64);
            if let Some(content) = &e.content {
                let _ = doc.put(&entry, "content", content.as_str());
            }
        }
    }

    /// Drop a book from the library. Its annotations + progress are left in place
    /// (keyed by content id) so re-adding the same file restores them.
    pub(crate) fn remove_library_entry(doc: &mut AutoCommit, id: &str) {
        let tombstones = Self::root_map(doc, "library_tombstones");
        let _ = doc.put(&tombstones, id, true);
        for lib in Self::root_maps(doc, "library") {
            let _ = doc.delete(&lib, id);
        }
    }

    /// A fresh, user-consented import is the sole restore path for a shelf
    /// tombstone. This keeps delayed metadata writes from reviving a removed
    /// book while retaining the intentional "remove then re-add" workflow.
    pub(crate) fn restore_library_entry(doc: &mut AutoCommit, e: &LibraryEntry) {
        for tombstones in Self::root_maps(doc, "library_tombstones") {
            let _ = doc.delete(&tombstones, &e.id);
        }
        Self::upsert_library_entry(doc, e);
    }

    pub fn library_is_removed(doc: &AutoCommit, id: &str) -> bool {
        Self::has_tombstone(doc, "library_tombstones", id)
    }

    /// Every book in the library, ordered by when it was added (oldest first) so
    /// the shelf has a stable base order the sort control then re-orders.
    pub fn read_library_entries(doc: &AutoCommit) -> Vec<LibraryEntry> {
        let mut out = Vec::new();
        for (key, entries) in Self::keyed_map_entries(doc, "library") {
            if Self::library_is_removed(doc, &key) {
                continue;
            }
            out.push(LibraryEntry {
                id: key,
                shared: entries
                    .iter()
                    .any(|entry| read_bool(doc, entry, "shared").unwrap_or(false)),
                title: Self::merged_str(doc, &entries, "title").unwrap_or_default(),
                author: Self::merged_str(doc, &entries, "author").unwrap_or_default(),
                subject: Self::merged_str(doc, &entries, "subject").unwrap_or_default(),
                added: entries
                    .iter()
                    .filter_map(|entry| Self::scalar_i64(doc, entry, "added"))
                    .filter(|&value| value > 0)
                    .min()
                    .unwrap_or(0) as u64,
                content: Self::merged_str(doc, &entries, "content"),
            });
        }
        out.sort_by(|a, b| a.added.cmp(&b.added).then_with(|| a.id.cmp(&b.id)));
        out
    }

    // --- colour schemes ----------------------------------------------------

    /// Read the registered colour schemes, falling back to the built-ins when
    /// none are stored (kept out of the document until the user edits them).
    pub fn read_schemes(doc: &AutoCommit) -> Vec<ColorScheme> {
        Self::read_registry(
            doc,
            default_schemes,
            ["schemes", "scheme_entries", "scheme_tombstones"],
            Self::read_scheme_entry,
            |scheme| &scheme.id,
        )
    }

    fn read_scheme_entry(
        doc: &AutoCommit,
        e: &ObjId,
        fallback_id: Option<&str>,
    ) -> Option<ColorScheme> {
        let id = read_str(doc, e, "id").unwrap_or_default();
        let id = if id.is_empty() {
            fallback_id?.to_string()
        } else {
            id
        };
        let page = unpack_rgb(Self::scalar_i64(doc, e, "page").unwrap_or(0));
        Some(ColorScheme {
            name: read_str(doc, e, "name").unwrap_or_else(|| id.clone()),
            page,
            ink: unpack_rgb(Self::scalar_i64(doc, e, "ink").unwrap_or(0)),
            accent: unpack_rgb(Self::scalar_i64(doc, e, "accent").unwrap_or(0)),
            dark: read_bool(doc, e, "dark").unwrap_or_else(|| crate::model::luminance(page) < 0.5),
            enabled: read_bool(doc, e, "enabled").unwrap_or(true),
            builtin: read_bool(doc, e, "builtin").unwrap_or(false),
            id,
        })
    }

    pub(crate) fn upsert_scheme(doc: &mut AutoCommit, scheme: &ColorScheme) {
        if Self::has_tombstone(doc, "scheme_tombstones", &scheme.id) {
            return;
        }
        let entries = Self::root_map(doc, "scheme_entries");
        let entry = Self::get_or_make_map(doc, &entries, &scheme.id);
        let _ = doc.put(&entry, "id", scheme.id.as_str());
        let _ = doc.put(&entry, "name", scheme.name.as_str());
        let _ = doc.put(&entry, "page", pack_rgb(scheme.page));
        let _ = doc.put(&entry, "ink", pack_rgb(scheme.ink));
        let _ = doc.put(&entry, "accent", pack_rgb(scheme.accent));
        let _ = doc.put(&entry, "dark", scheme.dark);
        let _ = doc.put(&entry, "enabled", scheme.enabled);
        let _ = doc.put(&entry, "builtin", scheme.builtin);
    }

    pub(crate) fn remove_scheme(doc: &mut AutoCommit, id: &str) {
        let tombstones = Self::root_map(doc, "scheme_tombstones");
        let _ = doc.put(&tombstones, id, true);
        let entries = Self::root_map(doc, "scheme_entries");
        let _ = doc.delete(&entries, id);
    }

    /// Replace the whole scheme registry (the manager edits it wholesale).
    ///
    /// # Panics
    ///
    /// Panics if creating the fresh `"schemes"` list (or a fresh entry map
    /// inside it) fails, which only happens if the document itself is
    /// corrupt — there is no meaningful `ObjId` to fall back to.
    #[allow(
        clippy::expect_used,
        reason = "put_object/insert_object on a freshly-created list cannot fail short of document corruption, which has no meaningful ObjId fallback"
    )]
    #[cfg(test)]
    pub(crate) fn write_schemes(doc: &mut AutoCommit, schemes: &[ColorScheme]) {
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

    pub(crate) fn write_progress(doc: &mut AutoCommit, book_id: &str, p: &Progress) {
        for entry in Self::get_or_make_keyed_maps(doc, "progress", book_id) {
            let _ = doc.put(&entry, "chapter", p.chapter as i64);
            let _ = doc.put(&entry, "block", p.block as i64);
            let _ = doc.put(&entry, "fraction", f64::from(p.fraction));
            // Preserve a previously recorded open time; only advance it (0 = never).
            if p.opened > 0 {
                let _ = doc.put(&entry, "opened", p.opened as i64);
            }
        }
    }

    pub fn read_progress(doc: &AutoCommit, book_id: &str) -> Progress {
        Self::keyed_map_entries(doc, "progress")
            .remove(book_id)
            .unwrap_or_default()
            .into_iter()
            .map(|entry| Progress {
                chapter: Self::scalar_i64(doc, &entry, "chapter").unwrap_or(0).max(0) as usize,
                block: Self::scalar_i64(doc, &entry, "block").unwrap_or(0).max(0) as usize,
                fraction: (read_number(doc, &entry, "fraction").unwrap_or(0.0) as f32)
                    .clamp(0.0, 1.0),
                opened: Self::scalar_i64(doc, &entry, "opened").unwrap_or(0).max(0) as u64,
            })
            // Keep one authored tuple coherent across conflicting entry
            // objects. Latest explicit open wins; ordinary position updates
            // then prefer furthest progress with stable location tie-breaks.
            .max_by(|left, right| {
                left.opened
                    .cmp(&right.opened)
                    .then_with(|| left.fraction.total_cmp(&right.fraction))
                    .then_with(|| left.chapter.cmp(&right.chapter))
                    .then_with(|| left.block.cmp(&right.block))
            })
            .unwrap_or_default()
    }

    // --- annotations -------------------------------------------------------

    /// A deletion marker is deliberately a separate, keyed CRDT map rather
    /// than a `delete` of the annotation entry. Automerge merges a map delete
    /// with a concurrent field edit, which can make a supposedly deleted
    /// annotation visible again. A monotonic marker gives deletion a stable
    /// winner without turning the annotation collection into a JSON blob.
    fn tombstone_map(doc: &mut AutoCommit, key: &str) -> ObjId {
        Self::root_map(doc, key)
    }

    fn has_tombstone(doc: &AutoCommit, key: &str, id: &str) -> bool {
        Self::root_maps(doc, key)
            .iter()
            .any(|tombstones| read_bool(doc, tombstones, id).unwrap_or(false))
    }

    /// Read a user-editable registry (fonts, colour schemes) stored under the
    /// root `[list, entries, tombstones]` keys: a legacy list replaces the
    /// `defaults`, keyed entries override or extend it by id, tombstoned ids
    /// drop out, and an empty result falls back to the `defaults` again.
    fn read_registry<T>(
        doc: &AutoCommit,
        defaults: fn() -> Vec<T>,
        [list_key, entries_key, tombstones_key]: [&str; 3],
        read_entry: fn(&AutoCommit, &ObjId, Option<&str>) -> Option<T>,
        id_of: fn(&T) -> &str,
    ) -> Vec<T> {
        let mut out = defaults();
        if let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&ROOT, list_key) {
            out.clear();
            for i in 0..doc.length(&list) {
                let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&list, i) else {
                    continue;
                };
                if let Some(item) = read_entry(doc, &e, None) {
                    out.push(item);
                }
            }
        }
        for entries in Self::root_maps(doc, entries_key) {
            let keys: Vec<String> = doc.keys(&entries).collect();
            for id in keys {
                if Self::has_tombstone(doc, tombstones_key, &id) {
                    continue;
                }
                let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&entries, &id) else {
                    continue;
                };
                let Some(item) = read_entry(doc, &entry, Some(id.as_str())) else {
                    continue;
                };
                if let Some(existing) = out
                    .iter_mut()
                    .find(|existing| id_of(existing) == id_of(&item))
                {
                    *existing = item;
                } else {
                    out.push(item);
                }
            }
        }
        out.retain(|item| !Self::has_tombstone(doc, tombstones_key, id_of(item)));
        if out.is_empty() { defaults() } else { out }
    }

    pub fn annotation_is_deleted(doc: &AutoCommit, id: Uuid) -> bool {
        Self::has_tombstone(doc, "annotation_tombstones", &id.to_string())
    }

    pub fn group_is_deleted(doc: &AutoCommit, group: Uuid) -> bool {
        Self::has_tombstone(doc, "highlight_tombstones", &group.to_string())
    }

    /// Whether a row may still be changed or shown.  A row tombstone is
    /// sufficient to hide a standalone annotation; rows belonging to a
    /// deleted highlight group are hidden as well.  Keeping this check here
    /// prevents a late edit action from mutating an otherwise invisible row
    /// after a collaborator deleted its parent highlight.
    pub fn annotation_is_live(doc: &AutoCommit, id: Uuid) -> bool {
        if Self::annotation_is_deleted(doc, id) {
            return false;
        }
        let Some(entries) = Self::keyed_map_entries(doc, "annotations").remove(&id.to_string())
        else {
            return false;
        };
        let group = Self::merged_str(doc, &entries, "group")
            .and_then(|value| value.parse().ok())
            .unwrap_or(id);
        !Self::group_is_deleted(doc, group)
    }

    fn mark_tombstone(doc: &mut AutoCommit, map_key: &str, id: Uuid) {
        let mut tombstones = Self::root_maps(doc, map_key);
        if tombstones.is_empty() {
            tombstones.push(Self::tombstone_map(doc, map_key));
        }
        for tombstones in tombstones {
            let _ = doc.put(&tombstones, id.to_string().as_str(), true);
        }
    }

    pub(crate) fn upsert_annotation(doc: &mut AutoCommit, a: &Annotation) {
        // IDs and logical highlight groups are minted exactly once. Never let
        // a delayed duplicate/add resurrect something a collaborator deleted.
        if Self::annotation_is_deleted(doc, a.id) || Self::group_is_deleted(doc, a.group) {
            return;
        }
        // An annotation id never changes logical highlight group. Apart from
        // making accidental UUID reuse harmless, this prevents a stale row
        // hidden by a group tombstone from being reattached to a new group.
        let existing = Self::keyed_map_entries(doc, "annotations")
            .remove(&a.id.to_string())
            .unwrap_or_default();
        for entry in &existing {
            let existing_group = read_str(doc, entry, "group")
                .and_then(|value| value.parse().ok())
                .unwrap_or(a.id);
            if existing_group != a.group || Self::group_is_deleted(doc, existing_group) {
                return;
            }
        }
        let key = a.id.to_string();
        for entry in Self::get_or_make_keyed_maps(doc, "annotations", &key) {
            let _ = doc.put(&entry, "doc_id", a.doc_id.as_str());
            let _ = doc.put(&entry, "kind", a.kind.as_str());
            let _ = doc.put(&entry, "chapter", a.chapter as i64);
            let _ = doc.put(&entry, "block", a.block as i64);
            let _ = doc.put(&entry, "start", a.start as i64);
            let _ = doc.put(&entry, "end", a.end as i64);
            let _ = doc.put(&entry, "excerpt", a.excerpt.as_str());
            Self::write_text(doc, &entry, &tilo_rich_text::plain_clips(&a.note));
            let _ = doc.put(&entry, "color", a.color.as_str());
            let _ = doc.put(&entry, "created", a.created as i64);
            let _ = doc.put(&entry, "group", a.group.to_string().as_str());
        }
    }

    /// Replace only the note's text, preserving concurrent anchor/colour
    /// edits made by another reader.
    pub(crate) fn write_annotation_note(doc: &mut AutoCommit, id: Uuid, note: &str) -> bool {
        let entries = Self::keyed_map_entries(doc, "annotations")
            .remove(&id.to_string())
            .unwrap_or_default();
        for entry in &entries {
            Self::write_text(doc, entry, &tilo_rich_text::plain_clips(note));
        }
        !entries.is_empty()
    }

    /// Write an annotation's text as rich text under [`TEXT`], replacing
    /// what it held.
    fn write_text(doc: &mut AutoCommit, entry: &ObjId, clips: &[tilo_document::ClipBlock]) {
        if let Err(error) = tilo_rich_text::put_rich_text(doc, entry, TEXT, clips) {
            tracing::warn!(%error, "annotation text not written");
        }
    }

    /// The rich text an annotation's note is edited in: the first of its
    /// rows that holds some.
    pub fn annotation_text(doc: &AutoCommit, id: Uuid) -> Option<ObjId> {
        Self::keyed_map_entries(doc, "annotations")
            .remove(&id.to_string())?
            .iter()
            .find_map(|entry| tilo_rich_text::rich_text_at(doc, entry, TEXT))
    }

    /// An annotation's text, plain and as written, from its rows.
    fn read_text(doc: &AutoCommit, entries: &[ObjId]) -> (String, Vec<ui::widget::BlockContent>) {
        if let Some(root) = entries
            .iter()
            .find_map(|entry| tilo_rich_text::rich_text_at(doc, entry, TEXT))
        {
            return (
                tilo_document::RichText::plain_text_under(doc, &root).unwrap_or_default(),
                tilo_rich_text::blocks_under(doc, &root).unwrap_or_default(),
            );
        }
        (String::new(), Vec::new())
    }

    pub(crate) fn write_annotation_created(doc: &mut AutoCommit, id: Uuid, created: u64) -> bool {
        let entries = Self::keyed_map_entries(doc, "annotations")
            .remove(&id.to_string())
            .unwrap_or_default();
        let created = i64::try_from(created).unwrap_or(i64::MAX);
        for entry in &entries {
            let _ = doc.put(entry, "created", created);
        }
        !entries.is_empty()
    }

    pub fn move_annotation(doc: &mut AutoCommit, id: Uuid, anchor: &AnnotationAnchor) -> bool {
        let entries = Self::keyed_map_entries(doc, "annotations")
            .remove(&id.to_string())
            .unwrap_or_default();
        for entry in &entries {
            let _ = doc.put(entry, "chapter", anchor.chapter as i64);
            let _ = doc.put(entry, "block", anchor.block as i64);
            let _ = doc.put(entry, "start", anchor.start as i64);
            let _ = doc.put(entry, "end", anchor.end as i64);
            let _ = doc.put(entry, "excerpt", &anchor.excerpt);
        }
        !entries.is_empty()
    }

    pub(crate) fn write_annotation_color(doc: &mut AutoCommit, id: Uuid, color: HiColor) -> bool {
        let entries = Self::keyed_map_entries(doc, "annotations")
            .remove(&id.to_string())
            .unwrap_or_default();
        for entry in &entries {
            let _ = doc.put(entry, "color", color.as_str());
        }
        !entries.is_empty()
    }

    pub(crate) fn remove_annotation(doc: &mut AutoCommit, id: Uuid) {
        Self::mark_tombstone(doc, "annotation_tombstones", id);
    }

    /// Remove every annotation row belonging to one logical group — a highlight
    /// (its per-block pieces) together with all of its comments.
    pub(crate) fn remove_group(doc: &mut AutoCommit, group: Uuid) {
        // Capture current children while they are still visible, then write
        // both the durable group marker and child markers. The operation is
        // atomic on this replica; the group marker still hides any offline
        // comment which arrives later through a merge.
        let ids: Vec<Uuid> = Self::list_annotations(doc)
            .into_iter()
            .filter(|a| a.group == group)
            .map(|a| a.id)
            .collect();
        Self::mark_tombstone(doc, "highlight_tombstones", group);
        for id in ids {
            Self::remove_annotation(doc, id);
        }
    }

    pub fn list_annotations(doc: &AutoCommit) -> Vec<Annotation> {
        let mut out = Vec::new();
        for (key, entries) in Self::keyed_map_entries(doc, "annotations") {
            let Ok(id) = key.parse::<Uuid>() else {
                continue;
            };
            if Self::annotation_is_deleted(doc, id) {
                continue;
            }
            let (note, blocks) = Self::read_text(doc, &entries);
            let a = Annotation {
                id,
                doc_id: Self::merged_str(doc, &entries, "doc_id").unwrap_or_default(),
                kind: AnnoKind::from_str(
                    &Self::merged_str(doc, &entries, "kind").unwrap_or_default(),
                ),
                chapter: entries
                    .iter()
                    .filter_map(|entry| Self::scalar_i64(doc, entry, "chapter"))
                    .max()
                    .unwrap_or(0)
                    .max(0) as usize,
                block: entries
                    .iter()
                    .filter_map(|entry| Self::scalar_i64(doc, entry, "block"))
                    .max()
                    .unwrap_or(0)
                    .max(0) as usize,
                start: entries
                    .iter()
                    .filter_map(|entry| Self::scalar_i64(doc, entry, "start"))
                    .max()
                    .unwrap_or(0)
                    .max(0) as usize,
                end: entries
                    .iter()
                    .filter_map(|entry| Self::scalar_i64(doc, entry, "end"))
                    .max()
                    .unwrap_or(0)
                    .max(0) as usize,
                excerpt: Self::merged_str(doc, &entries, "excerpt").unwrap_or_default(),
                note,
                blocks,
                color: HiColor::from_str(
                    &Self::merged_str(doc, &entries, "color").unwrap_or_default(),
                ),
                created: entries
                    .iter()
                    .filter_map(|entry| Self::scalar_i64(doc, entry, "created"))
                    .max()
                    .unwrap_or(0)
                    .max(0) as u64,
                // Older rows predate grouping; fall back to a self-group (the
                // row's own id) so each behaves as a standalone mark.
                group: Self::merged_str(doc, &entries, "group")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(id),
            };
            if !Self::group_is_deleted(doc, a.group) {
                out.push(a);
            }
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
    use tilo_automerge::ApplyIncoming;

    /// Settings, fonts, progress, library prefs, schemes and library entries round-trip through the document.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn settings_and_library_registries_round_trip() {
        // --- phase: settings_round_trip ---
        {
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

        // --- phase: fonts_registry_defaults_and_round_trips ---
        {
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

        // --- phase: progress_round_trip ---
        {
            let mut doc = AutoCommit::new();
            let p = Progress {
                chapter: 2,
                block: 5,
                fraction: 0.42,
                opened: 1_720_000_000_000,
            };
            BooksDoc::write_progress(&mut doc, "book:x", &p);
            let back = BooksDoc::read_progress(&doc, "book:x");
            assert_eq!(back.chapter, 2);
            assert_eq!(back.block, 5);
            assert!((back.fraction - 0.42).abs() < 0.001);
            assert_eq!(
                back.opened, 1_720_000_000_000,
                "open time persists for recent-sort"
            );
            // Unknown book -> default.
            assert_eq!(BooksDoc::read_progress(&doc, "nope"), Progress::default());
        }

        // --- phase: library_prefs_round_trip ---
        {
            let mut doc = AutoCommit::new();
            assert_eq!(BooksDoc::read_library_prefs(&doc), LibraryPrefs::default());
            let p = LibraryPrefs {
                sort: LibrarySort::Recent,
                hide_read: true,
            };
            BooksDoc::write_library_prefs(&mut doc, p);
            assert_eq!(BooksDoc::read_library_prefs(&doc), p);
        }

        // --- phase: schemes_registry_round_trips_with_a_custom_scheme ---
        {
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

        // --- phase: library_entries_round_trip_and_preserve_added_time ---
        {
            use crate::model::LibraryEntry;
            let mut doc = AutoCommit::new();
            assert!(
                BooksDoc::read_library_entries(&doc).is_empty(),
                "fresh library is empty"
            );

            let e = LibraryEntry {
                id: "book:abc".into(),
                shared: false,
                title: "Novel".into(),
                author: "A. Writer".into(),
                subject: "Fiction".into(),
                added: 1_000,
                content: None,
            };
            BooksDoc::upsert_library_entry(&mut doc, &e);
            let back = BooksDoc::read_library_entries(&doc);
            assert_eq!(back.len(), 1);
            assert_eq!(back[0], e);

            // Re-importing the same content after it is shared advertises the
            // canonical workspace availability, but still keeps its shelf order.
            let moved = LibraryEntry {
                shared: true,
                added: 9_999,
                content: None,
                ..e.clone()
            };
            BooksDoc::upsert_library_entry(&mut doc, &moved);
            let back = BooksDoc::read_library_entries(&doc);
            assert_eq!(
                back.len(),
                1,
                "same content id updates in place, not duplicated"
            );
            assert!(back[0].shared, "shared availability is replicated");
            assert_eq!(
                back[0].added, 1_000,
                "added time is preserved across a re-import"
            );
            let bytes = doc.save();
            let stored = String::from_utf8_lossy(&bytes);
            assert!(
                !stored.contains("/home/reader/Novel.epub"),
                "a source path must not enter the replicated document"
            );

            BooksDoc::remove_library_entry(&mut doc, "book:abc");
            assert!(
                BooksDoc::read_library_entries(&doc).is_empty(),
                "removed from the shelf"
            );
        }
    }

    /// Annotations upsert, list, filter and remove; group tombstones hide late comments and win across replicas.
    #[test]
    fn annotations_round_trip_and_tombstones_win() {
        // --- phase: annotations_upsert_list_remove ---
        {
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
            assert!(BooksDoc::annotation_is_deleted(&doc, id));

            // A stale peer can still carry the old row. It must not revive a
            // deletion just because its map write arrives after the tombstone.
            BooksDoc::upsert_annotation(&mut doc, &a2);
            assert!(BooksDoc::annotations_for(&doc, "book:x").is_empty());
        }

        // --- phase: annotations_filtered_by_book ---
        {
            let mut doc = AutoCommit::new();
            BooksDoc::upsert_annotation(
                &mut doc,
                &Annotation::new("book:a", AnnoKind::Bookmark, 0, 0),
            );
            BooksDoc::upsert_annotation(&mut doc, &Annotation::new("book:b", AnnoKind::Note, 0, 0));
            assert_eq!(BooksDoc::annotations_for(&doc, "book:a").len(), 1);
            assert_eq!(BooksDoc::annotations_for(&doc, "book:b").len(), 1);
        }

        // --- phase: group_tombstone_hides_a_late_comment_without_deleting_unrelated_rows ---
        {
            let mut doc = AutoCommit::new();
            let group = Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888);
            let highlight = Annotation::new("book:x", AnnoKind::Highlight, 0, 1).with_group(group);
            let comment = Annotation::new("book:x", AnnoKind::Note, 0, 1)
                .with_group(group)
                .with_note("late peer comment");
            let unrelated = Annotation::new("book:x", AnnoKind::Note, 0, 2).with_note("keep me");
            BooksDoc::upsert_annotation(&mut doc, &highlight);
            BooksDoc::remove_group(&mut doc, group);
            // Simulate an offline peer's comment arriving after the group was
            // deleted. The row is never reintroduced to the visible collection.
            BooksDoc::upsert_annotation(&mut doc, &comment);
            BooksDoc::upsert_annotation(&mut doc, &unrelated);

            assert!(BooksDoc::group_is_deleted(&doc, group));
            assert!(BooksDoc::annotation_is_deleted(&doc, highlight.id));
            let rows = BooksDoc::annotations_for(&doc, "book:x");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id, unrelated.id);
        }

        // --- phase: tombstones_win_when_independent_replicas_merge ---
        {
            let mut base = AutoCommit::new();
            let group = Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888);
            let highlight = Annotation::new("book:x", AnnoKind::Highlight, 0, 1).with_group(group);
            let comment = Annotation::new("book:x", AnnoKind::Note, 0, 1)
                .with_group(group)
                .with_note("offline comment");
            BooksDoc::upsert_annotation(&mut base, &highlight);

            // Different actors diverge from the same ancestor: one reader deletes
            // the highlight while another reader adds a comment to it offline.
            let mut deleting_peer = base.fork().with_actor(ActorId::random());
            let mut editing_peer = base.fork().with_actor(ActorId::random());
            BooksDoc::remove_group(&mut deleting_peer, group);
            BooksDoc::upsert_annotation(&mut editing_peer, &comment);

            deleting_peer
                .merge_incoming(&mut editing_peer)
                .expect("independent replicas merge");
            editing_peer
                .merge_incoming(&mut deleting_peer)
                .expect("second replica converges");

            for peer in [&deleting_peer, &editing_peer] {
                assert!(BooksDoc::group_is_deleted(peer, group));
                assert!(BooksDoc::annotations_for(peer, "book:x").is_empty());
                assert!(!BooksDoc::annotation_is_live(peer, highlight.id));
                assert!(!BooksDoc::annotation_is_live(peer, comment.id));
            }
        }
    }

    /// Independently created keyed roots materialise every branch.
    #[test]
    fn document_merges_keyed_roots() {
        // --- phase: independently_created_keyed_roots_materialise_every_branch ---
        {
            let mut left = AutoCommit::new().with_actor(ActorId::random());
            let mut right = AutoCommit::new().with_actor(ActorId::random());
            let left_book = LibraryEntry {
                id: "book:left".into(),
                shared: true,
                title: "Left".into(),
                author: "A".into(),
                subject: String::new(),
                added: 1,
                content: None,
            };
            let right_book = LibraryEntry {
                id: "book:right".into(),
                shared: true,
                title: "Right".into(),
                author: "B".into(),
                subject: String::new(),
                added: 2,
                content: None,
            };
            let left_annotation = Annotation::new(&left_book.id, AnnoKind::Note, 0, 1);
            let right_annotation = Annotation::new(&right_book.id, AnnoKind::Bookmark, 0, 2);
            BooksDoc::upsert_library_entry(&mut left, &left_book);
            BooksDoc::upsert_annotation(&mut left, &left_annotation);
            BooksDoc::write_progress(
                &mut left,
                "book:shared-progress",
                &Progress {
                    chapter: 1,
                    block: 3,
                    fraction: 0.25,
                    opened: 10,
                },
            );
            BooksDoc::upsert_library_entry(&mut right, &right_book);
            BooksDoc::upsert_annotation(&mut right, &right_annotation);
            BooksDoc::write_progress(
                &mut right,
                "book:shared-progress",
                &Progress {
                    chapter: 4,
                    block: 9,
                    fraction: 0.75,
                    opened: 20,
                },
            );

            left.merge_incoming(&mut right).unwrap();
            right.merge_incoming(&mut left).unwrap();
            for peer in [&left, &right] {
                assert_eq!(
                    BooksDoc::read_library_entries(peer)
                        .into_iter()
                        .map(|entry| entry.id)
                        .collect::<Vec<_>>(),
                    vec!["book:left".to_string(), "book:right".to_string()]
                );
                let annotation_ids = BooksDoc::list_annotations(peer)
                    .into_iter()
                    .map(|annotation| annotation.id)
                    .collect::<std::collections::HashSet<_>>();
                assert_eq!(
                    annotation_ids,
                    [left_annotation.id, right_annotation.id]
                        .into_iter()
                        .collect()
                );
                assert_eq!(
                    BooksDoc::read_progress(peer, "book:shared-progress"),
                    Progress {
                        chapter: 4,
                        block: 9,
                        fraction: 0.75,
                        opened: 20,
                    }
                );
            }
        }
    }
}
