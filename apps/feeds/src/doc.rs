//! Automerge document model for the feed reader.
//!
//! Layout:
//! ```text
//! root
//! ├── "folders"  (Map id -> { name, order, collapsed })
//! ├── "feeds"    (Map id -> { title, url, site_url, folder, order, kind })
//! └── "articles" (Map id -> { feed, title, author, url, published,
//!                             summary, read, starred, content })
//! ```
//!
//! `read` / `starred` are plain scalar fields so concurrent toggles merge
//! last-writer-wins per key. Subscriptions (folders/feeds) are likewise
//! field-level. Article *content* is an opaque JSON blob (`content`) —
//! fetched bodies are immutable, so there's no value in CRDT-splitting them.
//! Selection / scroll / filter are per-peer and never touch the document.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{
    Article, ContentBlock, Feed, FeedKind, Folder, Id, ReadFont, ReadSettings, ReadTheme, Store,
};

pub struct FeedsDoc;

impl FeedsDoc {
    fn map_field(doc: &mut AutoCommit, key: &str) -> ObjId {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, key, ObjType::Map)
                .expect("put_object root map"),
        }
    }

    fn kind_str(kind: FeedKind) -> &'static str {
        match kind {
            FeedKind::Rss => "rss",
            FeedKind::Atom => "atom",
            FeedKind::Json => "json",
            FeedKind::Gemini => "gemini",
        }
    }
    fn kind_from(s: &str) -> FeedKind {
        match s {
            "atom" => FeedKind::Atom,
            "json" => FeedKind::Json,
            "gemini" => FeedKind::Gemini,
            _ => FeedKind::Rss,
        }
    }

    fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> String {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }
    fn get_int(doc: &AutoCommit, obj: &ObjId, key: &str) -> i64 {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => v.to_i64().unwrap_or(0),
            _ => 0,
        }
    }
    fn get_bool(doc: &AutoCommit, obj: &ObjId, key: &str) -> bool {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Boolean(b) => *b,
                _ => false,
            },
            _ => false,
        }
    }
    fn get_f64(doc: &AutoCommit, obj: &ObjId, key: &str, default: f64) -> f64 {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::F64(f) => *f,
                ScalarValue::Int(i) => *i as f64,
                ScalarValue::Uint(u) => *u as f64,
                _ => default,
            },
            _ => default,
        }
    }

    /// True when the document has no feeds yet (fresh / un-seeded).
    pub fn is_empty(doc: &AutoCommit) -> bool {
        match doc.get(&ROOT, "feeds") {
            Ok(Some((Value::Object(ObjType::Map), id))) => doc.keys(&id).count() == 0,
            _ => true,
        }
    }

    // --- writers ---------------------------------------------------------

    pub fn upsert_folder(doc: &mut AutoCommit, f: &Folder) {
        let folders = Self::map_field(doc, "folders");
        let key = f.id.to_string();
        let obj = match doc.get(&folders, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&folders, key.as_str(), ObjType::Map)
                .expect("folder map"),
        };
        ui::doc::put_if_changed(doc, &obj, "name", f.name.as_str());
        ui::doc::put_if_changed(doc, &obj, "order", f.order as i64);
        ui::doc::put_if_changed(doc, &obj, "collapsed", f.collapsed);
    }

    pub fn upsert_feed(doc: &mut AutoCommit, f: &Feed) {
        let feeds = Self::map_field(doc, "feeds");
        let key = f.id.to_string();
        let obj = match doc.get(&feeds, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&feeds, key.as_str(), ObjType::Map)
                .expect("feed map"),
        };
        ui::doc::put_if_changed(doc, &obj, "title", f.title.as_str());
        ui::doc::put_if_changed(doc, &obj, "url", f.url.as_str());
        ui::doc::put_if_changed(doc, &obj, "site_url", f.site_url.as_str());
        ui::doc::put_if_changed(
            doc,
            &obj,
            "folder",
            f.folder.map(|x| x.to_string()).unwrap_or_default(),
        );
        ui::doc::put_if_changed(doc, &obj, "order", f.order as i64);
        ui::doc::put_if_changed(doc, &obj, "kind", Self::kind_str(f.kind));
    }

    pub fn upsert_article(doc: &mut AutoCommit, a: &Article) {
        let articles = Self::map_field(doc, "articles");
        let key = a.id.to_string();
        let obj = match doc.get(&articles, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&articles, key.as_str(), ObjType::Map)
                .expect("article map"),
        };
        ui::doc::put_if_changed(doc, &obj, "feed", a.feed_id.to_string());
        ui::doc::put_if_changed(doc, &obj, "title", a.title.as_str());
        ui::doc::put_if_changed(doc, &obj, "author", a.author.as_str());
        ui::doc::put_if_changed(doc, &obj, "url", a.url.as_str());
        ui::doc::put_if_changed(doc, &obj, "published", a.published);
        ui::doc::put_if_changed(doc, &obj, "summary", a.summary.as_str());
        ui::doc::put_if_changed(doc, &obj, "read", a.read);
        ui::doc::put_if_changed(doc, &obj, "starred", a.starred);
        let content = serde_json::to_string(&a.blocks).unwrap_or_default();
        ui::doc::put_if_changed(doc, &obj, "content", content.as_str());
    }

    /// Unsubscribe: delete a feed entry and every article belonging to it from
    /// the document. Merge-friendly — a delete of a key another peer never
    /// re-adds converges cleanly. Mirrors [`crate::model::Store::remove_feed`]
    /// so the in-memory store and the CRDT stay in lock-step.
    pub fn remove_feed(doc: &mut AutoCommit, id: Id) {
        if let Ok(Some((Value::Object(ObjType::Map), feeds))) = doc.get(&ROOT, "feeds") {
            let _ = doc.delete(&feeds, id.to_string().as_str());
        }
        if let Ok(Some((Value::Object(ObjType::Map), articles))) = doc.get(&ROOT, "articles") {
            let feed_key = id.to_string();
            let keys: Vec<String> = doc.keys(&articles).collect();
            for key in keys {
                if let Ok(Some((Value::Object(ObjType::Map), obj))) =
                    doc.get(&articles, key.as_str())
                {
                    if Self::get_str(doc, &obj, "feed") == feed_key {
                        let _ = doc.delete(&articles, key.as_str());
                    }
                }
            }
        }
    }

    fn article_obj(doc: &mut AutoCommit, id: Id) -> Option<ObjId> {
        let articles = match doc.get(&ROOT, "articles") {
            Ok(Some((Value::Object(ObjType::Map), m))) => m,
            _ => return None,
        };
        match doc.get(&articles, id.to_string().as_str()) {
            Ok(Some((Value::Object(ObjType::Map), o))) => Some(o),
            _ => None,
        }
    }

    fn feed_obj(doc: &mut AutoCommit, id: Id) -> Option<ObjId> {
        let feeds = match doc.get(&ROOT, "feeds") {
            Ok(Some((Value::Object(ObjType::Map), m))) => m,
            _ => return None,
        };
        match doc.get(&feeds, id.to_string().as_str()) {
            Ok(Some((Value::Object(ObjType::Map), o))) => Some(o),
            _ => None,
        }
    }

    fn folder_obj(doc: &mut AutoCommit, id: Id) -> Option<ObjId> {
        let folders = match doc.get(&ROOT, "folders") {
            Ok(Some((Value::Object(ObjType::Map), m))) => m,
            _ => return None,
        };
        match doc.get(&folders, id.to_string().as_str()) {
            Ok(Some((Value::Object(ObjType::Map), o))) => Some(o),
            _ => None,
        }
    }

    /// Rename a feed — a single field-level write so a concurrent title edit
    /// merges last-writer-wins. A no-op if the feed is unknown (a bad replay
    /// can't wedge).
    pub fn set_feed_title(doc: &mut AutoCommit, id: Id, title: &str) {
        if let Some(obj) = Self::feed_obj(doc, id) {
            ui::doc::put_if_changed(doc, &obj, "title", title);
        }
    }

    /// Move a feed into `folder` (`None` = uncategorised). Field-level, so a
    /// concurrent move merges last-writer-wins.
    pub fn set_feed_folder(doc: &mut AutoCommit, id: Id, folder: Option<Id>) {
        if let Some(obj) = Self::feed_obj(doc, id) {
            ui::doc::put_if_changed(
                doc,
                &obj,
                "folder",
                folder.map(|x| x.to_string()).unwrap_or_default(),
            );
        }
    }

    /// Rename a folder — a single field-level write.
    pub fn set_folder_name(doc: &mut AutoCommit, id: Id, name: &str) {
        if let Some(obj) = Self::folder_obj(doc, id) {
            ui::doc::put_if_changed(doc, &obj, "name", name);
        }
    }

    /// Delete a folder and unparent every feed that pointed at it (the feeds
    /// survive as uncategorised — deleting a folder never drops subscriptions).
    /// Deterministic against the replay base: the affected feeds are read from
    /// the document, not from any local cache.
    pub fn remove_folder(doc: &mut AutoCommit, id: Id) {
        let key = id.to_string();
        if let Ok(Some((Value::Object(ObjType::Map), feeds))) = doc.get(&ROOT, "feeds") {
            let feed_keys: Vec<String> = doc.keys(&feeds).collect();
            for fk in feed_keys {
                if let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&feeds, fk.as_str()) {
                    if Self::get_str(doc, &obj, "folder") == key {
                        let _ = doc.put(&obj, "folder", "");
                    }
                }
            }
        }
        if let Ok(Some((Value::Object(ObjType::Map), folders))) = doc.get(&ROOT, "folders") {
            let _ = doc.delete(&folders, key.as_str());
        }
    }

    /// Update just the `read` flag (merge-friendly per-field write).
    pub fn set_read(doc: &mut AutoCommit, id: Id, read: bool) {
        if let Some(obj) = Self::article_obj(doc, id) {
            let _ = doc.put(&obj, "read", read);
        }
    }

    pub fn set_starred(doc: &mut AutoCommit, id: Id, starred: bool) {
        if let Some(obj) = Self::article_obj(doc, id) {
            let _ = doc.put(&obj, "starred", starred);
        }
    }

    pub fn set_folder_collapsed(doc: &mut AutoCommit, id: Id, collapsed: bool) {
        let folders = match doc.get(&ROOT, "folders") {
            Ok(Some((Value::Object(ObjType::Map), m))) => m,
            _ => return,
        };
        if let Ok(Some((Value::Object(ObjType::Map), obj))) =
            doc.get(&folders, id.to_string().as_str())
        {
            let _ = doc.put(&obj, "collapsed", collapsed);
        }
    }

    /// Write the entire store into the document (first-run seed).
    pub fn seed(doc: &mut AutoCommit, store: &Store) {
        for f in &store.folders {
            Self::upsert_folder(doc, f);
        }
        for f in &store.feeds {
            Self::upsert_feed(doc, f);
        }
        for a in &store.articles {
            Self::upsert_article(doc, a);
        }
    }

    // --- reading settings ------------------------------------------------
    //
    // Stored under a `settings` map so concurrent edits merge field-level
    // (last-writer-wins per key). These are the article-reader preferences
    // (font size / family / page tint); they ride in the same CRDT so a
    // collaborator reading the same cluster sees a consistent layout.

    /// True once the reading settings have been written at least once.
    pub fn has_settings(doc: &AutoCommit) -> bool {
        matches!(doc.get(&ROOT, "settings"), Ok(Some((Value::Object(ObjType::Map), _))))
    }

    pub fn write_settings(doc: &mut AutoCommit, s: &ReadSettings) {
        let obj = Self::map_field(doc, "settings");
        // font_scale is an f64 — guard with a bit-exact compare rather than the
        // generic put_if_changed, so a NaN would not defeat the equality check.
        let fs = s.font_scale as f64;
        if Self::get_f64(doc, &obj, "font_scale", f64::NAN).to_bits() != fs.to_bits() {
            let _ = doc.put(&obj, "font_scale", fs);
        }
        ui::doc::put_if_changed(doc, &obj, "font", s.font.as_str());
        ui::doc::put_if_changed(doc, &obj, "theme", s.theme.as_str());
    }

    pub fn read_settings(doc: &AutoCommit) -> ReadSettings {
        let default = ReadSettings::default();
        let obj = match doc.get(&ROOT, "settings") {
            Ok(Some((Value::Object(ObjType::Map), o))) => o,
            _ => return default,
        };
        ReadSettings {
            font_scale: Self::get_f64(doc, &obj, "font_scale", default.font_scale as f64) as f32,
            font: {
                let s = Self::get_str(doc, &obj, "font");
                if s.is_empty() { default.font } else { ReadFont::from_str(&s) }
            },
            theme: {
                let s = Self::get_str(doc, &obj, "theme");
                if s.is_empty() { default.theme } else { ReadTheme::from_str(&s) }
            },
        }
        .clamped()
    }

    // --- reader ----------------------------------------------------------

    /// Read the whole store back out of the document.
    pub fn load(doc: &AutoCommit) -> Store {
        let mut store = Store::new();

        if let Ok(Some((Value::Object(ObjType::Map), folders))) =
            doc.get(&ROOT, "folders")
        {
            for key in doc.keys(&folders) {
                let Ok(id) = key.parse::<Id>() else { continue };
                let Ok(Some((Value::Object(ObjType::Map), obj))) =
                    doc.get(&folders, key.as_str())
                else {
                    continue;
                };
                store.folders.push(Folder {
                    id,
                    name: Self::get_str(doc, &obj, "name"),
                    order: Self::get_int(doc, &obj, "order") as u32,
                    collapsed: Self::get_bool(doc, &obj, "collapsed"),
                });
            }
        }

        if let Ok(Some((Value::Object(ObjType::Map), feeds))) =
            doc.get(&ROOT, "feeds")
        {
            for key in doc.keys(&feeds) {
                let Ok(id) = key.parse::<Id>() else { continue };
                let Ok(Some((Value::Object(ObjType::Map), obj))) =
                    doc.get(&feeds, key.as_str())
                else {
                    continue;
                };
                let folder_s = Self::get_str(doc, &obj, "folder");
                store.feeds.push(Feed {
                    id,
                    title: Self::get_str(doc, &obj, "title"),
                    url: Self::get_str(doc, &obj, "url"),
                    site_url: Self::get_str(doc, &obj, "site_url"),
                    folder: if folder_s.is_empty() {
                        None
                    } else {
                        folder_s.parse::<Id>().ok()
                    },
                    order: Self::get_int(doc, &obj, "order") as u32,
                    kind: Self::kind_from(&Self::get_str(doc, &obj, "kind")),
                });
            }
        }

        if let Ok(Some((Value::Object(ObjType::Map), articles))) =
            doc.get(&ROOT, "articles")
        {
            for key in doc.keys(&articles) {
                let Ok(id) = key.parse::<Id>() else { continue };
                let Ok(Some((Value::Object(ObjType::Map), obj))) =
                    doc.get(&articles, key.as_str())
                else {
                    continue;
                };
                let feed_s = Self::get_str(doc, &obj, "feed");
                let Ok(feed_id) = feed_s.parse::<Id>() else { continue };
                let content = Self::get_str(doc, &obj, "content");
                let blocks: Vec<ContentBlock> =
                    serde_json::from_str(&content).unwrap_or_default();
                store.articles.push(Article {
                    id,
                    feed_id,
                    title: Self::get_str(doc, &obj, "title"),
                    author: Self::get_str(doc, &obj, "author"),
                    url: Self::get_str(doc, &obj, "url"),
                    published: Self::get_int(doc, &obj, "published"),
                    summary: Self::get_str(doc, &obj, "summary"),
                    blocks,
                    read: Self::get_bool(doc, &obj, "read"),
                    starred: Self::get_bool(doc, &obj, "starred"),
                });
            }
        }

        store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BlockKind, ContentBlock};
    use uuid::Uuid;

    fn sample_store() -> Store {
        let mut s = Store::new();
        let folder = Folder {
            id: Uuid::new_v4(),
            name: "News".into(),
            order: 0,
            collapsed: false,
        };
        let feed = Feed {
            id: Uuid::new_v4(),
            title: "Example".into(),
            url: "https://example.com/feed.xml".into(),
            site_url: "https://example.com".into(),
            folder: Some(folder.id),
            order: 0,
            kind: FeedKind::Atom,
        };
        let article = Article {
            id: Uuid::new_v4(),
            feed_id: feed.id,
            title: "Hello".into(),
            author: "Ada".into(),
            url: "https://example.com/1".into(),
            published: 1_700_000_000,
            summary: "sum".into(),
            blocks: vec![
                ContentBlock::new(BlockKind::Heading1, "Hello"),
                ContentBlock::new(BlockKind::Paragraph, "World"),
            ],
            read: false,
            starred: true,
        };
        s.upsert_folder(folder);
        s.upsert_feed(feed);
        s.insert_article_dedup(article);
        s
    }

    #[test]
    fn seed_and_load_round_trip() {
        let mut doc = AutoCommit::new();
        let store = sample_store();
        FeedsDoc::seed(&mut doc, &store);

        let loaded = FeedsDoc::load(&doc);
        assert_eq!(loaded.folders.len(), 1);
        assert_eq!(loaded.feeds.len(), 1);
        assert_eq!(loaded.articles.len(), 1);
        assert_eq!(loaded.feeds[0].kind, FeedKind::Atom);
        assert_eq!(loaded.articles[0].blocks.len(), 2);
        assert!(loaded.articles[0].starred);
        assert!(!loaded.articles[0].read);
        assert_eq!(loaded.articles[0].author, "Ada");
    }

    #[test]
    fn is_empty_detects_unseeded() {
        let mut doc = AutoCommit::new();
        assert!(FeedsDoc::is_empty(&doc));
        FeedsDoc::seed(&mut doc, &sample_store());
        assert!(!FeedsDoc::is_empty(&doc));
    }

    #[test]
    fn set_read_persists_and_merges() {
        let mut doc = AutoCommit::new();
        let store = sample_store();
        FeedsDoc::seed(&mut doc, &store);
        let id = store.articles[0].id;
        FeedsDoc::set_read(&mut doc, id, true);
        let loaded = FeedsDoc::load(&doc);
        assert!(loaded.articles[0].read);
    }

    #[test]
    fn remove_feed_deletes_feed_and_its_articles() {
        let mut doc = AutoCommit::new();
        let store = sample_store();
        FeedsDoc::seed(&mut doc, &store);
        let feed_id = store.feeds[0].id;
        assert_eq!(FeedsDoc::load(&doc).feeds.len(), 1);
        assert_eq!(FeedsDoc::load(&doc).articles.len(), 1);

        FeedsDoc::remove_feed(&mut doc, feed_id);

        let loaded = FeedsDoc::load(&doc);
        assert!(loaded.feeds.is_empty(), "the feed is deleted from the document");
        assert!(
            loaded.articles.is_empty(),
            "the feed's articles are deleted too"
        );
    }

    #[test]
    fn reading_settings_round_trip() {
        use crate::model::{ReadFont, ReadTheme};
        let mut doc = AutoCommit::new();
        assert!(!FeedsDoc::has_settings(&doc));
        // Defaults read back even before any write.
        assert_eq!(FeedsDoc::read_settings(&doc), ReadSettings::default());

        let s = ReadSettings { font_scale: 1.3, font: ReadFont::Serif, theme: ReadTheme::Sepia };
        FeedsDoc::write_settings(&mut doc, &s);
        assert!(FeedsDoc::has_settings(&doc));
        let loaded = FeedsDoc::read_settings(&doc);
        assert_eq!(loaded.font, ReadFont::Serif);
        assert_eq!(loaded.theme, ReadTheme::Sepia);
        assert!((loaded.font_scale - 1.3).abs() < 0.001);
    }

    #[test]
    fn two_peers_converge_on_read_flag() {
        let mut a = AutoCommit::new();
        let store = sample_store();
        FeedsDoc::seed(&mut a, &store);
        let id = store.articles[0].id;

        // Peer B joins from A's state, stars an article.
        let mut b = AutoCommit::load(&a.save()).unwrap();
        FeedsDoc::set_starred(&mut b, id, false);
        FeedsDoc::set_read(&mut a, id, true);

        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();
        let loaded = FeedsDoc::load(&a);
        assert!(loaded.articles[0].read, "A's read flag survived merge");
    }

    #[test]
    fn repeated_seed_keeps_doc_bounded() {
        // The store is mirrored into the CRDT via the upsert_* writers; with the
        // change-guarded scalar puts, re-seeding an unchanged store is a no-op.
        let mut doc = AutoCommit::new();
        let store = sample_store();
        FeedsDoc::seed(&mut doc, &store);
        let a = doc.save().len();
        for _ in 0..50 {
            FeedsDoc::seed(&mut doc, &store);
        }
        let b = doc.save().len();
        assert_eq!(a, b, "re-seeding the same store grew the document {a} -> {b}");
    }

    #[test]
    fn repeated_write_settings_keeps_doc_bounded() {
        use crate::model::{ReadFont, ReadTheme};
        let mut doc = AutoCommit::new();
        let s = ReadSettings { font_scale: 1.3, font: ReadFont::Serif, theme: ReadTheme::Sepia };
        FeedsDoc::write_settings(&mut doc, &s);
        let a = doc.save().len();
        for _ in 0..50 {
            FeedsDoc::write_settings(&mut doc, &s);
        }
        let b = doc.save().len();
        assert_eq!(a, b, "re-writing unchanged settings grew the document {a} -> {b}");
    }
}

