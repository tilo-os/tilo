//! Automerge document model for the feed reader.
//!
//! Layout:
//! ```text
//! root
//! ├── "folders"  (Map id -> { name, order, collapsed })
//! ├── "feeds"    (Map id -> { title, url, site_url, folder, order, kind })
//! └── "articles" (Map id -> { feed, title, author, url, published,
//!                             summary, read, starred, tags{}, content })
//! ```
//!
//! `read` / `starred` are plain scalar fields so concurrent toggles merge
//! last-writer-wins per key. Subscriptions (folders/feeds) are likewise
//! field-level. Tags are a CRDT map keyed by lossless hex names, so edits to
//! different labels merge independently. Article *content* is an opaque JSON blob (`content`) —
//! fetched bodies are immutable, so there's no value in CRDT-splitting them.
//! Selection / scroll / filter are per-peer and never touch the document.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::doc::{read_bool, read_i64_coerced, read_number, read_str};

use crate::model::{
    Article, ContentBlock, Feed, FeedKind, Folder, Id, ReadFont, ReadSettings, ReadTheme, Store,
};

pub struct FeedsDoc;

impl FeedsDoc {
    #[allow(
        clippy::expect_used,
        reason = "creating a root-level Automerge map object cannot fail under \
                  normal operation, and every caller here returns a value type with \
                  no fallible path to propagate to"
    )]
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

    /// True when the document has no feeds yet (fresh / un-seeded).
    pub fn is_empty(doc: &AutoCommit) -> bool {
        match doc.get(&ROOT, "feeds") {
            Ok(Some((Value::Object(ObjType::Map), id))) => doc.keys(&id).count() == 0,
            _ => true,
        }
    }

    // --- writers ---------------------------------------------------------

    /// # Panics
    /// Panics if creating the folder's Automerge map object fails — not
    /// expected under normal operation (see the `expect_used` allow below).
    #[allow(
        clippy::expect_used,
        reason = "creating a root-level Automerge map object cannot fail under \
                  normal operation, and every caller here returns a value type with \
                  no fallible path to propagate to"
    )]
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
        ui::doc::put_if_changed(doc, &obj, "order", i64::from(f.order));
        ui::doc::put_if_changed(doc, &obj, "collapsed", f.collapsed);
    }

    /// # Panics
    /// Panics if creating the feed's Automerge map object fails — not
    /// expected under normal operation (see the `expect_used` allow below).
    #[allow(
        clippy::expect_used,
        reason = "creating a root-level Automerge map object cannot fail under \
                  normal operation, and every caller here returns a value type with \
                  no fallible path to propagate to"
    )]
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
        ui::doc::put_if_changed(doc, &obj, "order", i64::from(f.order));
        ui::doc::put_if_changed(doc, &obj, "kind", Self::kind_str(f.kind));
    }

    /// # Panics
    /// Panics if creating the article's Automerge map object fails — not
    /// expected under normal operation (see the `expect_used` allow below).
    #[allow(
        clippy::expect_used,
        reason = "creating a root-level Automerge map object cannot fail under \
                  normal operation, and every caller here returns a value type with \
                  no fallible path to propagate to"
    )]
    pub fn upsert_article(doc: &mut AutoCommit, a: &Article) {
        let articles = Self::map_field(doc, "articles");
        let key = a.id.to_string();
        let (obj, created) = match doc.get(&articles, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => (id, false),
            _ => (
                doc.put_object(&articles, key.as_str(), ObjType::Map)
                    .expect("article map"),
                true,
            ),
        };
        ui::doc::put_if_changed(doc, &obj, "feed", a.feed_id.to_string());
        ui::doc::put_if_changed(doc, &obj, "title", a.title.as_str());
        ui::doc::put_if_changed(doc, &obj, "author", a.author.as_str());
        ui::doc::put_if_changed(doc, &obj, "url", a.url.as_str());
        ui::doc::put_if_changed(doc, &obj, "published", a.published);
        ui::doc::put_if_changed(doc, &obj, "summary", a.summary.as_str());
        ui::doc::put_if_changed(doc, &obj, "read", a.read);
        ui::doc::put_if_changed(doc, &obj, "starred", a.starred);
        // Feed refreshes update cached content but must not erase labels a
        // collaborator attached to an existing article.
        if created {
            Self::replace_tags(doc, &obj, &a.tags);
        }
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
                    && read_str(doc, &obj, "feed").unwrap_or_default() == feed_key
                {
                    let _ = doc.delete(&articles, key.as_str());
                }
            }
        }
    }

    fn article_obj(doc: &AutoCommit, id: Id) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), articles))) = doc.get(&ROOT, "articles") else {
            return None;
        };
        match doc.get(&articles, id.to_string().as_str()) {
            Ok(Some((Value::Object(ObjType::Map), o))) => Some(o),
            _ => None,
        }
    }

    fn feed_obj(doc: &AutoCommit, id: Id) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), feeds))) = doc.get(&ROOT, "feeds") else {
            return None;
        };
        match doc.get(&feeds, id.to_string().as_str()) {
            Ok(Some((Value::Object(ObjType::Map), o))) => Some(o),
            _ => None,
        }
    }

    fn folder_obj(doc: &AutoCommit, id: Id) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), folders))) = doc.get(&ROOT, "folders") else {
            return None;
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
                if let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&feeds, fk.as_str())
                    && read_str(doc, &obj, "folder").unwrap_or_default() == key
                {
                    let _ = doc.put(&obj, "folder", "");
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

    pub fn set_tags(doc: &mut AutoCommit, id: Id, tags: &[String]) {
        if let Some(obj) = Self::article_obj(doc, id) {
            Self::replace_tags(doc, &obj, tags);
        }
    }

    fn replace_tags(doc: &mut AutoCommit, article: &ObjId, tags: &[String]) {
        let tags_obj = match doc.get(article, "tags") {
            Ok(Some((Value::Object(ObjType::Map), obj))) => obj,
            _ => match doc.put_object(article, "tags", ObjType::Map) {
                Ok(obj) => obj,
                Err(_) => return,
            },
        };
        let desired = tags
            .iter()
            .map(|tag| (Self::tag_key(tag), tag.as_str()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let existing = doc.keys(&tags_obj).collect::<Vec<_>>();
        for key in existing {
            if !desired.contains_key(&key) {
                let _ = doc.delete(&tags_obj, key);
            }
        }
        for (key, tag) in desired {
            // Rewriting retained tags records this replacement's intent and
            // gives Automerge an operation to reconcile with a concurrent edit.
            let _ = doc.put(&tags_obj, key, tag);
        }
    }

    fn tag_key(tag: &str) -> String {
        hex::encode(tag)
    }

    pub fn set_folder_collapsed(doc: &mut AutoCommit, id: Id, collapsed: bool) {
        let Ok(Some((Value::Object(ObjType::Map), folders))) = doc.get(&ROOT, "folders") else {
            return;
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
        matches!(
            doc.get(&ROOT, "settings"),
            Ok(Some((Value::Object(ObjType::Map), _)))
        )
    }

    pub fn write_settings(doc: &mut AutoCommit, s: &ReadSettings) {
        let obj = Self::map_field(doc, "settings");
        // font_scale is an f64 — guard with a bit-exact compare rather than the
        // generic put_if_changed, so a NaN would not defeat the equality check.
        let fs = f64::from(s.font_scale);
        if read_number(doc, &obj, "font_scale")
            .unwrap_or(f64::NAN)
            .to_bits()
            != fs.to_bits()
        {
            let _ = doc.put(&obj, "font_scale", fs);
        }
        ui::doc::put_if_changed(doc, &obj, "font", s.font.as_str());
        ui::doc::put_if_changed(doc, &obj, "theme", s.theme.as_str());
    }

    pub fn read_settings(doc: &AutoCommit) -> ReadSettings {
        let default = ReadSettings::default();
        let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&ROOT, "settings") else {
            return default;
        };
        ReadSettings {
            font_scale: read_number(doc, &obj, "font_scale")
                .unwrap_or_else(|| f64::from(default.font_scale)) as f32,
            font: {
                let s = read_str(doc, &obj, "font").unwrap_or_default();
                if s.is_empty() {
                    default.font
                } else {
                    ReadFont::from_key(&s)
                }
            },
            theme: {
                let s = read_str(doc, &obj, "theme").unwrap_or_default();
                if s.is_empty() {
                    default.theme
                } else {
                    ReadTheme::from_key(&s)
                }
            },
        }
        .clamped()
    }

    // --- reader ----------------------------------------------------------

    /// Read the whole store back out of the document.
    pub fn load(doc: &AutoCommit) -> Store {
        let mut store = Store::new();

        if let Ok(Some((Value::Object(ObjType::Map), folders))) = doc.get(&ROOT, "folders") {
            for key in doc.keys(&folders) {
                let Ok(id) = key.parse::<Id>() else { continue };
                let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&folders, key.as_str())
                else {
                    continue;
                };
                store.folders.push(Folder {
                    id,
                    name: read_str(doc, &obj, "name").unwrap_or_default(),
                    order: read_i64_coerced(doc, &obj, "order").unwrap_or(0) as u32,
                    collapsed: read_bool(doc, &obj, "collapsed").unwrap_or(false),
                });
            }
        }

        if let Ok(Some((Value::Object(ObjType::Map), feeds))) = doc.get(&ROOT, "feeds") {
            for key in doc.keys(&feeds) {
                let Ok(id) = key.parse::<Id>() else { continue };
                let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&feeds, key.as_str())
                else {
                    continue;
                };
                let folder_s = read_str(doc, &obj, "folder").unwrap_or_default();
                store.feeds.push(Feed {
                    id,
                    title: read_str(doc, &obj, "title").unwrap_or_default(),
                    url: read_str(doc, &obj, "url").unwrap_or_default(),
                    site_url: read_str(doc, &obj, "site_url").unwrap_or_default(),
                    folder: if folder_s.is_empty() {
                        None
                    } else {
                        folder_s.parse::<Id>().ok()
                    },
                    order: read_i64_coerced(doc, &obj, "order").unwrap_or(0) as u32,
                    kind: Self::kind_from(&read_str(doc, &obj, "kind").unwrap_or_default()),
                });
            }
        }

        if let Ok(Some((Value::Object(ObjType::Map), articles))) = doc.get(&ROOT, "articles") {
            for key in doc.keys(&articles) {
                let Ok(id) = key.parse::<Id>() else { continue };
                let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&articles, key.as_str())
                else {
                    continue;
                };
                let feed_s = read_str(doc, &obj, "feed").unwrap_or_default();
                let Ok(feed_id) = feed_s.parse::<Id>() else {
                    continue;
                };
                let content = read_str(doc, &obj, "content").unwrap_or_default();
                let blocks: Vec<ContentBlock> = serde_json::from_str(&content).unwrap_or_default();
                let tags = match doc.get(&obj, "tags") {
                    Ok(Some((Value::Object(ObjType::Map), tags_obj))) => {
                        let mut tags = doc
                            .keys(&tags_obj)
                            .filter_map(|key| match doc.get(&tags_obj, key) {
                                Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                                    ScalarValue::Str(tag) => Some(tag.to_string()),
                                    _ => None,
                                },
                                _ => None,
                            })
                            .collect::<Vec<_>>();
                        tags.sort();
                        tags.dedup();
                        tags
                    }
                    // Read legacy JSON-scalar documents; the next tag edit
                    // migrates this field to the collaborative map form.
                    _ => serde_json::from_str(&read_str(doc, &obj, "tags").unwrap_or_default())
                        .unwrap_or_default(),
                };
                store.articles.push(Article {
                    id,
                    feed_id,
                    title: read_str(doc, &obj, "title").unwrap_or_default(),
                    author: read_str(doc, &obj, "author").unwrap_or_default(),
                    url: read_str(doc, &obj, "url").unwrap_or_default(),
                    published: read_i64_coerced(doc, &obj, "published").unwrap_or(0),
                    summary: read_str(doc, &obj, "summary").unwrap_or_default(),
                    blocks,
                    read: read_bool(doc, &obj, "read").unwrap_or(false),
                    starred: read_bool(doc, &obj, "starred").unwrap_or(false),
                    tags,
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
    use tilo_automerge::ApplyIncoming;
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
            tags: vec!["example".into()],
        };
        s.upsert_folder(folder);
        s.upsert_feed(feed);
        s.insert_article_dedup(article);
        s
    }

    /// The feeds document seeds, detects emptiness, round-trips settings and removes a feed with its articles.
    #[test]
    fn feeds_doc_round_trips_library_and_settings() {
        // --- phase: seed_and_load_round_trip ---
        {
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

        // --- phase: is_empty_detects_unseeded ---
        {
            let mut doc = AutoCommit::new();
            assert!(FeedsDoc::is_empty(&doc));
            FeedsDoc::seed(&mut doc, &sample_store());
            assert!(!FeedsDoc::is_empty(&doc));
        }

        // --- phase: reading_settings_round_trip ---
        {
            use crate::model::{ReadFont, ReadTheme};
            let mut doc = AutoCommit::new();
            assert!(!FeedsDoc::has_settings(&doc));
            // Defaults read back even before any write.
            assert_eq!(FeedsDoc::read_settings(&doc), ReadSettings::default());

            let s = ReadSettings {
                font_scale: 1.3,
                font: ReadFont::Serif,
                theme: ReadTheme::Sepia,
            };
            FeedsDoc::write_settings(&mut doc, &s);
            assert!(FeedsDoc::has_settings(&doc));
            let loaded = FeedsDoc::read_settings(&doc);
            assert_eq!(loaded.font, ReadFont::Serif);
            assert_eq!(loaded.theme, ReadTheme::Sepia);
            assert!((loaded.font_scale - 1.3).abs() < 0.001);
        }

        // --- phase: remove_feed_deletes_feed_and_its_articles ---
        {
            let mut doc = AutoCommit::new();
            let store = sample_store();
            FeedsDoc::seed(&mut doc, &store);
            let feed_id = store.feeds[0].id;
            assert_eq!(FeedsDoc::load(&doc).feeds.len(), 1);
            assert_eq!(FeedsDoc::load(&doc).articles.len(), 1);

            FeedsDoc::remove_feed(&mut doc, feed_id);

            let loaded = FeedsDoc::load(&doc);
            assert!(
                loaded.feeds.is_empty(),
                "the feed is deleted from the document"
            );
            assert!(
                loaded.articles.is_empty(),
                "the feed's articles are deleted too"
            );
        }
    }

    /// Read flags and tag edits persist, survive refreshes and converge across peers.
    #[test]
    fn peers_converge_on_read_flags_and_tags() {
        // --- phase: set_read_persists_and_merges ---
        {
            let mut doc = AutoCommit::new();
            let store = sample_store();
            FeedsDoc::seed(&mut doc, &store);
            let id = store.articles[0].id;
            FeedsDoc::set_read(&mut doc, id, true);
            let loaded = FeedsDoc::load(&doc);
            assert!(loaded.articles[0].read);
        }

        // --- phase: tag_edits_round_trip_and_refreshes_preserve_them ---
        {
            let mut doc = AutoCommit::new();
            let store = sample_store();
            FeedsDoc::seed(&mut doc, &store);
            let id = store.articles[0].id;
            FeedsDoc::set_tags(&mut doc, id, &["design".into(), "later".into()]);

            let mut refreshed = store.articles[0].clone();
            refreshed.title = "Updated upstream title".into();
            refreshed.tags.clear();
            FeedsDoc::upsert_article(&mut doc, &refreshed);

            let loaded = FeedsDoc::load(&doc);
            let article = loaded.article(id).expect("article survives refresh");
            assert_eq!(article.title, "Updated upstream title");
            assert_eq!(article.tags, ["design", "later"]);
        }

        // --- phase: concurrent_distinct_tag_additions_merge ---
        {
            let mut alice = AutoCommit::new();
            let store = sample_store();
            FeedsDoc::seed(&mut alice, &store);
            alice.commit();
            alice.set_actor(automerge::ActorId::from("alice".as_bytes()));
            let mut bob = alice.fork();
            bob.set_actor(automerge::ActorId::from("bob".as_bytes()));
            let id = store.articles[0].id;

            FeedsDoc::set_tags(&mut alice, id, &["example".into(), "design".into()]);
            FeedsDoc::set_tags(&mut bob, id, &["example".into(), "later".into()]);
            alice.merge_incoming(&mut bob).expect("replicas merge");

            assert_eq!(
                FeedsDoc::load(&alice).article(id).expect("article").tags,
                ["design", "example", "later"],
                "independent tag additions survive concurrent editing"
            );
        }

        // --- phase: two_peers_converge_on_read_flag ---
        {
            let mut a = AutoCommit::new();
            let store = sample_store();
            FeedsDoc::seed(&mut a, &store);
            let id = store.articles[0].id;

            // Peer B joins from A's state, stars an article.
            let mut b = AutoCommit::load(&a.save()).unwrap();
            FeedsDoc::set_starred(&mut b, id, false);
            FeedsDoc::set_read(&mut a, id, true);

            let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_loaded).unwrap();
            let loaded = FeedsDoc::load(&a);
            assert!(loaded.articles[0].read, "A's read flag survived merge");
        }
    }

    /// Repeated seeding and settings writes keep the document bounded.
    #[test]
    fn repeated_writes_keep_the_doc_bounded() {
        // --- phase: repeated_seed_keeps_doc_bounded ---
        {
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
            assert_eq!(
                a, b,
                "re-seeding the same store grew the document {a} -> {b}"
            );
        }

        // --- phase: repeated_write_settings_keeps_doc_bounded ---
        {
            use crate::model::{ReadFont, ReadTheme};
            let mut doc = AutoCommit::new();
            let s = ReadSettings {
                font_scale: 1.3,
                font: ReadFont::Serif,
                theme: ReadTheme::Sepia,
            };
            FeedsDoc::write_settings(&mut doc, &s);
            let a = doc.save().len();
            for _ in 0..50 {
                FeedsDoc::write_settings(&mut doc, &s);
            }
            let b = doc.save().len();
            assert_eq!(
                a, b,
                "re-writing unchanged settings grew the document {a} -> {b}"
            );
        }
    }
}
