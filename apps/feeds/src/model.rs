//! Pure data model for the Feeds reader.
//!
//! The model is the in-memory working copy that drives rendering. It is
//! mirrored into / out of the Automerge document by [`crate::doc`] so the
//! collaborative state (subscriptions, read + star flags, cached articles)
//! converges across peers. Selection / scroll / filter are *local* per-peer
//! state and deliberately stay out of the CRDT.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type Id = Uuid;

/// What kind of source a feed was parsed from. Drives the little badge in
/// the sidebar and the reader's "via" line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedKind {
    Rss,
    Atom,
    Json,
    Gemini,
}

impl FeedKind {
    pub fn label(self) -> &'static str {
        match self {
            FeedKind::Rss => "RSS",
            FeedKind::Atom => "Atom",
            FeedKind::Json => "JSON",
            FeedKind::Gemini => "Gemini",
        }
    }

    /// The wire form persisted in the CRDT (see [`crate::doc`]). Matches the
    /// `kind` scalar written by `FeedsDoc::upsert_feed`, so it is the form an
    /// [`crate::actions::FeedsAction`] carries in its payload.
    pub fn as_str(self) -> &'static str {
        match self {
            FeedKind::Rss => "rss",
            FeedKind::Atom => "atom",
            FeedKind::Json => "json",
            FeedKind::Gemini => "gemini",
        }
    }

    pub fn from_key(s: &str) -> Self {
        match s {
            "atom" => FeedKind::Atom,
            "json" => FeedKind::Json,
            "gemini" => FeedKind::Gemini,
            _ => FeedKind::Rss,
        }
    }
}

/// One rendered content block in an article body. Articles store a small
/// block list rather than raw HTML so the reader pane can lay them out
/// natively (and so gemtext + HTML converge on one representation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKind {
    Heading1,
    Heading2,
    Heading3,
    Paragraph,
    Bullet,
    Quote,
    Code,
    Link,
    Divider,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBlock {
    pub kind: BlockKind,
    pub text: String,
    /// Target URL for `Link` blocks (gemtext `=>` lines, etc.).
    #[serde(default)]
    pub href: Option<String>,
}

impl ContentBlock {
    pub fn new(kind: BlockKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            href: None,
        }
    }
    pub fn link(text: impl Into<String>, href: impl Into<String>) -> Self {
        Self {
            kind: BlockKind::Link,
            text: text.into(),
            href: Some(href.into()),
        }
    }
}

/// A folder/category grouping feeds in the sidebar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: Id,
    pub name: String,
    pub order: u32,
    #[serde(default)]
    pub collapsed: bool,
}

/// A single subscription.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feed {
    pub id: Id,
    pub title: String,
    /// The feed document URL (the thing we re-fetch).
    pub url: String,
    /// The human homepage (the thing "Open site" goes to).
    #[serde(default)]
    pub site_url: String,
    pub folder: Option<Id>,
    pub order: u32,
    pub kind: FeedKind,
}

/// One article / entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Article {
    pub id: Id,
    pub feed_id: Id,
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub url: String,
    /// Publication time, epoch seconds. 0 == unknown.
    pub published: i64,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub blocks: Vec<ContentBlock>,
    pub read: bool,
    pub starred: bool,
    /// User-authored labels. Stored on the article itself so a concurrent tag
    /// edit is one mergeable field and survives feed refreshes.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Stable local key for a normalized tag. Resource addresses carry the full
/// UTF-8 tag losslessly; this compact key only keeps [`Filter`] copyable.
pub type TagKey = u64;

pub fn tag_key(tag: &str) -> TagKey {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, tag.trim().to_lowercase().as_bytes());
    std::hash::Hasher::finish(&hasher)
}

/// Which subset of articles the list pane is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Unread,
    Starred,
    Folder(Id),
    Feed(Id),
    Tag(TagKey),
}

// ---------------------------------------------------------------------
// Reading settings (article reader pane)
//
// Mirrors the ebook reader's reading-mode model so the two apps stay
// consistent. The *page tint* (light/sepia/dark) is deliberately distinct
// from the OS theme: people read on warm "paper" even in a dark desktop. All
// chrome reads from the OS `Theme`; only the reader pane uses these.
// ---------------------------------------------------------------------

/// Page tint for the article reader. Distinct from the OS theme on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadTheme {
    Light,
    Sepia,
    Dark,
}

impl ReadTheme {
    pub const ALL: [ReadTheme; 3] = [ReadTheme::Light, ReadTheme::Sepia, ReadTheme::Dark];
    pub fn label(self) -> &'static str {
        match self {
            ReadTheme::Light => "Light",
            ReadTheme::Sepia => "Sepia",
            ReadTheme::Dark => "Dark",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ReadTheme::Light => "light",
            ReadTheme::Sepia => "sepia",
            ReadTheme::Dark => "dark",
        }
    }
    pub fn from_key(s: &str) -> Self {
        match s {
            "sepia" => ReadTheme::Sepia,
            "dark" => ReadTheme::Dark,
            _ => ReadTheme::Light,
        }
    }
}

/// Reading face. Unlike the ebook reader (which only offered Sans/Mono because
/// it draws through the proportional/monospace `draw_style` path), the
/// feeds reader renders the body through `ui::richtext::RichTextLayout`, which
/// exposes a real `FontFamily::Serif`. So we offer a genuine serif option for
/// long-form reading, plus the proportional sans and a fixed-width mono.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadFont {
    Sans,
    Serif,
    Mono,
}

impl ReadFont {
    pub const ALL: [ReadFont; 3] = [ReadFont::Sans, ReadFont::Serif, ReadFont::Mono];
    pub fn label(self) -> &'static str {
        match self {
            ReadFont::Sans => "Sans",
            ReadFont::Serif => "Serif",
            ReadFont::Mono => "Mono",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ReadFont::Sans => "sans",
            ReadFont::Serif => "serif",
            ReadFont::Mono => "mono",
        }
    }
    pub fn from_key(s: &str) -> Self {
        match s {
            "serif" => ReadFont::Serif,
            "mono" => ReadFont::Mono,
            _ => ReadFont::Sans,
        }
    }
    /// Cycle Sans → Serif → Mono → Sans (for the cycle button).
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            ReadFont::Sans => ReadFont::Serif,
            ReadFont::Serif => ReadFont::Mono,
            ReadFont::Mono => ReadFont::Sans,
        }
    }
}

/// Article-reader preferences. Persisted in the CRDT so collaborators reading
/// together (and the same user across sessions) share a consistent layout.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadSettings {
    /// Body font-size multiplier; 1.0 == `BASE_SIZE` px. Clamped to [0.8, 1.8].
    pub font_scale: f32,
    pub font: ReadFont,
    pub theme: ReadTheme,
}

impl Default for ReadSettings {
    fn default() -> Self {
        Self {
            font_scale: 1.0,
            font: ReadFont::Sans,
            theme: ReadTheme::Dark,
        }
    }
}

impl ReadSettings {
    pub const FONT_MIN: f32 = 0.8;
    pub const FONT_MAX: f32 = 1.8;
    /// Base body size in logical px before the scale multiplier.
    pub const BASE_SIZE: f32 = 16.0;

    pub fn body_size(&self) -> f32 {
        Self::BASE_SIZE * self.font_scale
    }
    /// Heading sizes scale with the body so a larger body keeps proportion.
    pub fn heading_size(&self, level: u8) -> f32 {
        let mult = match level {
            1 => 1.7,
            2 => 1.35,
            _ => 1.12,
        };
        (Self::BASE_SIZE * mult * self.font_scale).round()
    }
    #[must_use]
    pub fn clamped(mut self) -> Self {
        self.font_scale = self.font_scale.clamp(Self::FONT_MIN, Self::FONT_MAX);
        self
    }
}

/// The working copy of all feed state.
#[derive(Clone, Debug, Default)]
pub struct Store {
    pub folders: Vec<Folder>,
    pub feeds: Vec<Feed>,
    pub articles: Vec<Article>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.feeds.is_empty() && self.articles.is_empty()
    }

    // --- lookups ---------------------------------------------------------

    pub fn feed(&self, id: Id) -> Option<&Feed> {
        self.feeds.iter().find(|f| f.id == id)
    }

    pub fn folder(&self, id: Id) -> Option<&Folder> {
        self.folders.iter().find(|f| f.id == id)
    }

    pub fn article(&self, id: Id) -> Option<&Article> {
        self.articles.iter().find(|a| a.id == id)
    }

    pub fn article_mut(&mut self, id: Id) -> Option<&mut Article> {
        self.articles.iter_mut().find(|a| a.id == id)
    }

    /// Folders sorted by order.
    pub fn folders_sorted(&self) -> Vec<&Folder> {
        let mut v: Vec<&Folder> = self.folders.iter().collect();
        v.sort_by_key(|f| (f.order, f.name.clone()));
        v
    }

    /// Feeds in a folder (None = uncategorised), sorted by order/title.
    pub fn feeds_in_folder(&self, folder: Option<Id>) -> Vec<&Feed> {
        let mut v: Vec<&Feed> = self.feeds.iter().filter(|f| f.folder == folder).collect();
        v.sort_by_key(|f| (f.order, f.title.clone()));
        v
    }

    // --- counts ----------------------------------------------------------

    pub fn unread_for_feed(&self, feed: Id) -> usize {
        self.articles
            .iter()
            .filter(|a| a.feed_id == feed && !a.read)
            .count()
    }

    pub fn unread_for_folder(&self, folder: Id) -> usize {
        let feed_ids: Vec<Id> = self
            .feeds
            .iter()
            .filter(|f| f.folder == Some(folder))
            .map(|f| f.id)
            .collect();
        self.articles
            .iter()
            .filter(|a| feed_ids.contains(&a.feed_id) && !a.read)
            .count()
    }

    pub fn total_unread(&self) -> usize {
        self.articles.iter().filter(|a| !a.read).count()
    }

    pub fn total_starred(&self) -> usize {
        self.articles.iter().filter(|a| a.starred).count()
    }

    pub fn total_articles(&self) -> usize {
        self.articles.len()
    }

    // --- filtering -------------------------------------------------------

    /// Does an article match the active filter?
    fn matches(&self, a: &Article, filter: Filter) -> bool {
        match filter {
            Filter::All => true,
            Filter::Unread => !a.read,
            Filter::Starred => a.starred,
            Filter::Feed(id) => a.feed_id == id,
            Filter::Folder(fid) => self.feed(a.feed_id).is_some_and(|f| f.folder == Some(fid)),
            Filter::Tag(key) => a.tags.iter().any(|tag| tag_key(tag) == key),
        }
    }

    /// Unique normalized article tags, alphabetized for navigation and
    /// canonical resource publication.
    pub fn tags(&self) -> Vec<String> {
        self.tags_limited(usize::MAX)
    }

    /// Alphabetized unique tags with bounded retained allocation. The scan is
    /// linear in cached articles, but sidebar/resource materialization never
    /// grows with an archival tag vocabulary.
    pub fn tags_limited(&self, limit: usize) -> Vec<String> {
        let mut tags = self
            .articles
            .iter()
            .flat_map(|article| &article.tags)
            .map(|tag| tag.trim().to_lowercase())
            .filter(|tag| !tag.is_empty())
            .fold(Vec::with_capacity(limit.min(32)), |mut tags, tag| {
                match tags.binary_search(&tag) {
                    Err(index) if index < limit => {
                        tags.insert(index, tag);
                        if tags.len() > limit {
                            tags.pop();
                        }
                    }
                    Ok(_) | Err(_) => {}
                }
                tags
            });
        tags.truncate(limit);
        tags
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.articles.iter().any(|article| {
            article
                .tags
                .iter()
                .any(|candidate| candidate.trim().to_lowercase() == tag)
        })
    }

    pub fn set_tags(&mut self, id: Id, tags: Vec<String>) -> bool {
        let Some(article) = self.article_mut(id) else {
            return false;
        };
        if article.tags == tags {
            return false;
        }
        article.tags = tags;
        true
    }

    /// Articles matching `filter`, newest first.
    pub fn articles_for(&self, filter: Filter) -> Vec<&Article> {
        let mut v: Vec<&Article> = self
            .articles
            .iter()
            .filter(|a| self.matches(a, filter))
            .collect();
        // Newest first; stable on title for equal timestamps.
        v.sort_by(|a, b| {
            b.published
                .cmp(&a.published)
                .then_with(|| a.title.cmp(&b.title))
        });
        v
    }

    pub fn count_for(&self, filter: Filter) -> usize {
        self.articles
            .iter()
            .filter(|a| self.matches(a, filter))
            .count()
    }

    pub fn unread_count_for(&self, filter: Filter) -> usize {
        self.articles
            .iter()
            .filter(|a| self.matches(a, filter) && !a.read)
            .count()
    }

    // --- mutations -------------------------------------------------------

    pub fn mark_read(&mut self, id: Id, read: bool) -> bool {
        if let Some(a) = self.article_mut(id)
            && a.read != read
        {
            a.read = read;
            return true;
        }
        false
    }

    pub fn toggle_star(&mut self, id: Id) -> bool {
        if let Some(a) = self.article_mut(id) {
            a.starred = !a.starred;
            return true;
        }
        false
    }

    /// The unread article ids matching `filter`, in document order. Shared by
    /// [`Self::mark_all_read`] and the action model's "Mark all as read" so the
    /// affordance and the palette resolve the byte-identical id batch.
    pub fn unread_ids_for(&self, filter: Filter) -> Vec<Id> {
        self.articles
            .iter()
            .filter(|a| self.matches(a, filter) && !a.read)
            .map(|a| a.id)
            .collect()
    }

    /// Mark every article matching `filter` as read. Returns affected ids.
    pub fn mark_all_read(&mut self, filter: Filter) -> Vec<Id> {
        let ids = self.unread_ids_for(filter);
        for id in &ids {
            if let Some(a) = self.article_mut(*id) {
                a.read = true;
            }
        }
        ids
    }

    /// Insert (or replace) a feed.
    pub fn upsert_feed(&mut self, feed: Feed) {
        if let Some(existing) = self.feeds.iter_mut().find(|f| f.id == feed.id) {
            *existing = feed;
        } else {
            self.feeds.push(feed);
        }
    }

    pub fn upsert_folder(&mut self, folder: Folder) {
        if let Some(existing) = self.folders.iter_mut().find(|f| f.id == folder.id) {
            *existing = folder;
        } else {
            self.folders.push(folder);
        }
    }

    /// Insert an article if its `url` (or id) is not already present for
    /// that feed. Returns true if it was newly inserted.
    pub fn insert_article_dedup(&mut self, article: Article) -> bool {
        let dup = self.articles.iter().any(|a| {
            a.id == article.id
                || (a.feed_id == article.feed_id && !article.url.is_empty() && a.url == article.url)
        });
        if dup {
            return false;
        }
        self.articles.push(article);
        true
    }

    /// Unsubscribe: remove a feed and every article belonging to it. Returns
    /// the ids of the articles that were removed (so callers can clear a stale
    /// selection). A no-op returning an empty vec if the feed is unknown.
    pub fn remove_feed(&mut self, id: Id) -> Vec<Id> {
        let removed: Vec<Id> = self
            .articles
            .iter()
            .filter(|a| a.feed_id == id)
            .map(|a| a.id)
            .collect();
        self.articles.retain(|a| a.feed_id != id);
        self.feeds.retain(|f| f.id != id);
        removed
    }

    pub fn next_feed_order(&self, folder: Option<Id>) -> u32 {
        self.feeds
            .iter()
            .filter(|f| f.folder == folder)
            .map(|f| f.order)
            .max()
            .map_or(0, |m| m + 1)
    }
}

/// Format an epoch-seconds timestamp as a compact relative string
/// ("3h", "2d", "Mar 14"). `now` is passed so it's testable.
pub fn relative_time(published: i64, now: i64) -> String {
    const WEEK: i64 = 7 * 86_400;
    if published <= 0 {
        return "—".to_string();
    }
    let delta = (now - published).max(0);
    if delta < WEEK {
        ui::format::relative_compact(delta)
    } else {
        // Fall back to an absolute month/day.
        use chrono::{TimeZone, Utc};
        match Utc.timestamp_opt(published, 0).single() {
            Some(dt) => dt.format("%b %-d").to_string(),
            None => "—".to_string(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    fn feed(id: Id, folder: Option<Id>) -> Feed {
        Feed {
            id,
            title: "F".into(),
            url: "http://x".into(),
            site_url: String::new(),
            folder,
            order: 0,
            kind: FeedKind::Rss,
        }
    }
    fn art(id: Id, feed_id: Id, read: bool, starred: bool, when: i64) -> Article {
        Article {
            id,
            feed_id,
            title: format!("a{when}"),
            author: String::new(),
            url: format!("http://x/{id}"),
            published: when,
            summary: String::new(),
            blocks: vec![],
            read,
            starred,
            tags: Vec::new(),
        }
    }

    /// The store sorts articles, tracks unread counts, filters, marks read, dedups by URL and removes only a feed's own articles.
    #[test]
    fn store_orders_filters_counts_and_dedups_articles() {
        // --- phase: articles_for_sorts_newest_first ---
        {
            let mut s = Store::new();
            let f = Uuid::new_v4();
            s.upsert_feed(feed(f, None));
            s.insert_article_dedup(art(Uuid::new_v4(), f, false, false, 100));
            s.insert_article_dedup(art(Uuid::new_v4(), f, false, false, 300));
            s.insert_article_dedup(art(Uuid::new_v4(), f, false, false, 200));
            let got: Vec<i64> = s
                .articles_for(Filter::All)
                .iter()
                .map(|a| a.published)
                .collect();
            assert_eq!(got, vec![300, 200, 100]);
        }

        // --- phase: unread_counts_track_feed_and_folder ---
        {
            let mut s = Store::new();
            let folder = Uuid::new_v4();
            s.upsert_folder(Folder {
                id: folder,
                name: "N".into(),
                order: 0,
                collapsed: false,
            });
            let f = Uuid::new_v4();
            s.upsert_feed(feed(f, Some(folder)));
            s.insert_article_dedup(art(Uuid::new_v4(), f, false, false, 1));
            s.insert_article_dedup(art(Uuid::new_v4(), f, true, false, 2));
            assert_eq!(s.unread_for_feed(f), 1);
            assert_eq!(s.unread_for_folder(folder), 1);
            assert_eq!(s.total_unread(), 1);
        }

        // --- phase: filter_unread_and_starred ---
        {
            let mut s = Store::new();
            let f = Uuid::new_v4();
            s.upsert_feed(feed(f, None));
            let a1 = Uuid::new_v4();
            let a2 = Uuid::new_v4();
            s.insert_article_dedup(art(a1, f, false, true, 1));
            s.insert_article_dedup(art(a2, f, true, false, 2));
            assert_eq!(s.articles_for(Filter::Unread).len(), 1);
            assert_eq!(s.articles_for(Filter::Starred).len(), 1);
            assert_eq!(s.articles_for(Filter::Starred)[0].id, a1);
        }

        // --- phase: mark_all_read_clears_filter ---
        {
            let mut s = Store::new();
            let f = Uuid::new_v4();
            s.upsert_feed(feed(f, None));
            s.insert_article_dedup(art(Uuid::new_v4(), f, false, false, 1));
            s.insert_article_dedup(art(Uuid::new_v4(), f, false, false, 2));
            let affected = s.mark_all_read(Filter::All);
            assert_eq!(affected.len(), 2);
            assert_eq!(s.total_unread(), 0);
        }

        // --- phase: dedup_blocks_same_url ---
        {
            let mut s = Store::new();
            let f = Uuid::new_v4();
            s.upsert_feed(feed(f, None));
            let a = art(Uuid::new_v4(), f, false, false, 1);
            assert!(s.insert_article_dedup(a.clone()));
            let mut a2 = a;
            a2.id = Uuid::new_v4();
            assert!(!s.insert_article_dedup(a2), "same url is a dup");
        }

        // --- phase: remove_feed_drops_feed_and_its_articles_only ---
        {
            let mut s = Store::new();
            let keep = Uuid::new_v4();
            let drop = Uuid::new_v4();
            s.upsert_feed(feed(keep, None));
            s.upsert_feed(feed(drop, None));
            let survivor = Uuid::new_v4();
            s.insert_article_dedup(art(survivor, keep, false, false, 1));
            let doomed = Uuid::new_v4();
            s.insert_article_dedup(art(doomed, drop, false, false, 2));

            let removed = s.remove_feed(drop);
            assert_eq!(removed, vec![doomed], "returns the removed article ids");
            assert!(s.feed(drop).is_none(), "the feed is gone");
            assert!(s.feed(keep).is_some(), "the other feed survives");
            assert_eq!(
                s.articles.len(),
                1,
                "only the doomed feed's article is dropped"
            );
            assert_eq!(s.articles[0].id, survivor);
            // Removing an unknown feed is a harmless no-op.
            assert!(s.remove_feed(Uuid::new_v4()).is_empty());
        }
    }

    /// Reading settings round-trip their strings and clamp; relative times fall in the right buckets.
    #[test]
    fn reading_settings_and_relative_time_format_stably() {
        // --- phase: read_settings_round_trip_strings_and_clamp ---
        {
            for t in ReadTheme::ALL {
                assert_eq!(ReadTheme::from_key(t.as_str()), t);
            }
            for f in ReadFont::ALL {
                assert_eq!(ReadFont::from_key(f.as_str()), f);
            }
            // Serif is a first-class option (the differentiator vs the ebook reader).
            assert_eq!(ReadFont::from_key("serif"), ReadFont::Serif);
            // Cycle visits all three faces.
            assert_eq!(ReadFont::Sans.next(), ReadFont::Serif);
            assert_eq!(ReadFont::Serif.next(), ReadFont::Mono);
            assert_eq!(ReadFont::Mono.next(), ReadFont::Sans);
            // Clamp keeps font_scale inside bounds.
            let s = ReadSettings {
                font_scale: 5.0,
                ..Default::default()
            }
            .clamped();
            assert_eq!(s.font_scale, ReadSettings::FONT_MAX);
            let s = ReadSettings {
                font_scale: 0.1,
                ..Default::default()
            }
            .clamped();
            assert_eq!(s.font_scale, ReadSettings::FONT_MIN);
            // Body size scales with the multiplier.
            let s = ReadSettings {
                font_scale: 1.5,
                ..Default::default()
            };
            assert!((s.body_size() - ReadSettings::BASE_SIZE * 1.5).abs() < 0.01);
        }

        // --- phase: relative_time_buckets ---
        {
            let now = 2_000_000_000i64;
            assert_eq!(relative_time(0, now), "—");
            assert_eq!(relative_time(now - 30, now), "now");
            assert_eq!(relative_time(now - 120, now), "2m");
            assert_eq!(relative_time(now - 3 * 3600, now), "3h");
            assert_eq!(relative_time(now - 2 * 86400, now), "2d");
        }
    }
}
