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
    Article, ContentBlock, Feed, FeedKind, Folder, Id, Store,
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
        let _ = doc.put(&obj, "name", f.name.as_str());
        let _ = doc.put(&obj, "order", f.order as i64);
        let _ = doc.put(&obj, "collapsed", f.collapsed);
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
        let _ = doc.put(&obj, "title", f.title.as_str());
        let _ = doc.put(&obj, "url", f.url.as_str());
        let _ = doc.put(&obj, "site_url", f.site_url.as_str());
        let _ = doc.put(
            &obj,
            "folder",
            f.folder.map(|x| x.to_string()).unwrap_or_default(),
        );
        let _ = doc.put(&obj, "order", f.order as i64);
        let _ = doc.put(&obj, "kind", Self::kind_str(f.kind));
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
        let _ = doc.put(&obj, "feed", a.feed_id.to_string());
        let _ = doc.put(&obj, "title", a.title.as_str());
        let _ = doc.put(&obj, "author", a.author.as_str());
        let _ = doc.put(&obj, "url", a.url.as_str());
        let _ = doc.put(&obj, "published", a.published);
        let _ = doc.put(&obj, "summary", a.summary.as_str());
        let _ = doc.put(&obj, "read", a.read);
        let _ = doc.put(&obj, "starred", a.starred);
        let content = serde_json::to_string(&a.blocks).unwrap_or_default();
        let _ = doc.put(&obj, "content", content.as_str());
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
}
