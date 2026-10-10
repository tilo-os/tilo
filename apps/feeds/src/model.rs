//! The library's model: what is stored, and the pure queries the reader
//! asks of it.
//!
//! [`Library`] is the record: folders, subscriptions and the articles a
//! refresh brought in, each an item of a [`Collection`] whose fields merge
//! the way their types say (read and star flags last writer wins, tags per
//! tag). `#[derive(Model)]` is the whole storage contract; nothing here
//! touches the document. The record's verbs are in [`crate::resource`].
//! Selection, scroll and the chosen place are this window's, never stored.

use std::ops::Deref;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Collection, Json, JsonSchema, Model, Refusal, Set};

/// The id of a folder, a subscription or an article: its item in the
/// library's collections.
pub type Id = tilo_resource::Id;

/// One item of the library with its id, as the queries hand it out.
#[derive(Debug)]
pub struct Row<'a, T> {
    pub id: &'a Id,
    item: &'a T,
}

impl<T> Clone for Row<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Row<'_, T> {}

impl<'a, T> Row<'a, T> {
    pub fn new(id: &'a Id, item: &'a T) -> Self {
        Self { id, item }
    }

    /// The item itself, for as long as the library lives.
    pub fn item(&self) -> &'a T {
        self.item
    }
}

impl<T> Deref for Row<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.item
    }
}

/// What kind of source a feed was parsed from. Drives the little badge in
/// the sidebar and the reader's "via" line.
#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedKind {
    #[default]
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

    /// The form OPML and the reader's "via" line name it by.
    pub fn as_str(self) -> &'static str {
        match self {
            FeedKind::Rss => "rss",
            FeedKind::Atom => "atom",
            FeedKind::Json => "json",
            FeedKind::Gemini => "gemini",
        }
    }

    /// Guess a feed's kind from its URL scheme, as the fetch worker reads
    /// it, so a new subscription matches what a refresh would produce.
    pub fn guess(url: &str) -> Self {
        if url.trim().starts_with("gemini://") {
            FeedKind::Gemini
        } else {
            FeedKind::Rss
        }
    }
}

/// One rendered content block in an article body. Articles store a small
/// block list rather than raw HTML so the reader pane can lay them out
/// natively (and so gemtext + HTML converge on one representation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

/// A folder grouping subscriptions in the sidebar.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = Folder::valid)]
pub struct Folder {
    /// Name
    #[model(label)]
    pub name: String,
    /// Folded in the sidebar
    pub collapsed: bool,
}

impl Folder {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            collapsed: false,
        }
    }

    /// The sidebar lists a folder by its name: never blank.
    fn valid(&self) -> Result<(), Refusal> {
        if self.name.trim().is_empty() {
            return Err(Refusal::new("a folder needs a name"));
        }
        Ok(())
    }
}

/// A single subscription.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Feed {
    /// Name
    #[model(label)]
    pub title: String,
    /// Address
    ///
    /// The feed document's URL, which a refresh fetches again.
    #[model(readonly)]
    pub url: String,
    /// Site
    ///
    /// The homepage "Open site" goes to.
    #[model(readonly)]
    pub site_url: String,
    /// Folder
    ///
    /// The folder it is listed in; none for the top of the sidebar.
    pub folder: Option<Id>,
    /// Kind
    #[model(readonly)]
    pub kind: FeedKind,
    /// Server certificate
    ///
    /// A `gemini://` feed's server certificate (BLAKE3 of its DER, hex),
    /// pinned on first contact. Gemini servers sign their own certificates,
    /// so this pin is all that stops anyone on the path from serving the
    /// feed. Only a refresh sets it.
    #[model(readonly)]
    pub certificate_pin: Option<String>,
}

impl Feed {
    /// A subscription to `url`, titled with its address until a refresh
    /// brings its real title.
    pub fn subscribing(url: &str, folder: Option<Id>) -> Self {
        let url = url.trim();
        Self {
            title: url.to_owned(),
            url: url.to_owned(),
            site_url: crate::fetch::derive_site_url(url),
            folder,
            kind: FeedKind::guess(url),
            certificate_pin: None,
        }
    }
}

/// One article a refresh brought in. What it says is the feed's; whether
/// it was read, its star and its tags are the readers'.
#[derive(Model, Clone, Debug, PartialEq)]
#[model(check = Article::valid)]
pub struct Article {
    /// Subscription
    #[model(readonly)]
    pub feed_id: Id,
    /// Title
    #[model(label, readonly)]
    pub title: String,
    /// Author
    #[model(readonly)]
    pub author: String,
    /// Link
    #[model(readonly)]
    pub url: String,
    /// Published
    ///
    /// Epoch seconds; 0 when the feed did not say.
    #[model(readonly)]
    pub published: i64,
    /// Summary
    #[model(readonly)]
    pub summary: String,
    /// Body
    ///
    /// What the feed sent, as blocks the reader lays out. Written whole by
    /// a refresh; nobody edits it.
    #[model(readonly)]
    pub blocks: Json<Vec<ContentBlock>>,
    /// Read
    pub read: bool,
    /// Starred
    pub starred: bool,
    /// Tags
    ///
    /// The readers' own labels: lower case, without `#`, at most
    /// [`MAX_TAG_CHARS`] characters and [`MAX_TAGS`] of them.
    pub tags: Set<String>,
}

impl Default for Article {
    fn default() -> Self {
        Self {
            feed_id: Id::from_raw(""),
            title: String::new(),
            author: String::new(),
            url: String::new(),
            published: 0,
            summary: String::new(),
            blocks: Json::default(),
            read: false,
            starred: false,
            tags: Set::new(),
        }
    }
}

/// How long a tag may be, in characters.
pub const MAX_TAG_CHARS: usize = 48;
/// How many tags an article may carry.
pub const MAX_TAGS: usize = 24;

impl Article {
    /// The article `fetched` from the subscription `feed_id`, unread.
    pub fn fetched(feed_id: Id, fetched: Fetched) -> Self {
        Self {
            feed_id,
            title: fetched.title,
            author: fetched.author,
            url: fetched.url,
            published: fetched.published,
            summary: fetched.summary,
            blocks: Json(fetched.blocks),
            ..Self::default()
        }
    }

    /// Its tags, sorted.
    pub fn tag_list(&self) -> Vec<String> {
        let mut tags: Vec<String> = self.tags.iter().cloned().collect();
        tags.sort();
        tags
    }

    /// Tags are what people type to find articles again: normalized, so
    /// "Design" and "#design" are one tag.
    fn valid(&self) -> Result<(), Refusal> {
        if self.tags.len() > MAX_TAGS {
            return Err(Refusal::new("an article carries at most 24 tags"));
        }
        if self
            .tags
            .iter()
            .any(|tag| normalize_tags([tag.as_str()]).first() != Some(tag))
        {
            return Err(Refusal::new(
                "a tag is lower case, not blank, without `#` and at most 48 characters",
            ));
        }
        Ok(())
    }
}

/// Normalize typed tags: trimmed, lower case, without a leading `#`, at
/// most [`MAX_TAG_CHARS`] characters, sorted, each once, at most
/// [`MAX_TAGS`].
pub fn normalize_tags<'a>(tags: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut tags = tags
        .into_iter()
        .map(|tag| tag.trim().trim_start_matches('#').trim().to_lowercase())
        .filter(|tag| !tag.is_empty())
        .map(|tag| tag.chars().take(MAX_TAG_CHARS).collect::<String>())
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    tags.truncate(MAX_TAGS);
    tags
}

/// What a refresh fetched for one article, before the library holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Fetched {
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub url: String,
    /// Epoch seconds; 0 when unknown.
    #[serde(default)]
    pub published: i64,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub blocks: Vec<ContentBlock>,
}

/// One subscription an OPML file lists.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Outline {
    pub title: String,
    /// The feed's address.
    pub url: String,
    /// Its homepage.
    #[serde(default)]
    pub site_url: String,
    /// The folder the file files it under, by name.
    #[serde(default)]
    pub folder: Option<String>,
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
// The reader's paper follows the system theme unless the person picks a
// fixed paper (light, sepia or dark) for long-form reading. All chrome reads
// from the system `Theme`; only the article page uses these.
// ---------------------------------------------------------------------

/// Paper of the article reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadTheme {
    /// The page is the window ground: it follows the system theme.
    System,
    Light,
    Sepia,
    Dark,
}

impl ReadTheme {
    pub const ALL: [ReadTheme; 4] = [
        ReadTheme::System,
        ReadTheme::Light,
        ReadTheme::Sepia,
        ReadTheme::Dark,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ReadTheme::System => "System",
            ReadTheme::Light => "Light",
            ReadTheme::Sepia => "Sepia",
            ReadTheme::Dark => "Dark",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ReadTheme::System => "system",
            ReadTheme::Light => "light",
            ReadTheme::Sepia => "sepia",
            ReadTheme::Dark => "dark",
        }
    }
    pub fn from_key(s: &str) -> Self {
        match s {
            "light" => ReadTheme::Light,
            "sepia" => ReadTheme::Sepia,
            "dark" => ReadTheme::Dark,
            _ => ReadTheme::System,
        }
    }
}

/// Face of the article body: the serif reading face, the UI sans or the
/// monospace face.
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

/// Article-reader preferences: the person's own, following them to every
/// device as a personal settings section, never the shared library.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReadSettings {
    /// How much larger than its text role the article is set (1.0 is the
    /// role's own size), clamped to the reader’s percentage range.
    pub font_scale: f32,
    pub font: ReadFont,
    pub theme: ReadTheme,
}

impl Default for ReadSettings {
    fn default() -> Self {
        Self {
            font_scale: 1.0,
            font: ReadFont::Serif,
            theme: ReadTheme::System,
        }
    }
}

impl ui::storage::SettingsSchema for ReadSettings {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Personal;
}

impl ReadSettings {
    /// Accessible reader control bounds, expressed in whole percentages.
    pub const TEXT_PERCENT_RANGE: std::ops::RangeInclusive<u16> = 80..=180;
    /// One increment of the percentage control, also used by its actions.
    pub const TEXT_PERCENT_STEP: u16 = 10;

    pub fn adjust_text_percent(&mut self, direction: i16) {
        let next = self.percent() + f64::from(direction) * f64::from(Self::TEXT_PERCENT_STEP);
        self.font_scale = (next / 100.0) as f32;
        *self = (*self).clamped();
    }
    /// The reading scale as a whole percentage (`110` for 1.1).
    pub fn percent(&self) -> f64 {
        (f64::from(self.font_scale) * 100.0).round()
    }

    #[must_use]
    pub fn clamped(mut self) -> Self {
        self.font_scale = self.font_scale.clamp(
            f32::from(*Self::TEXT_PERCENT_RANGE.start()) / 100.0,
            f32::from(*Self::TEXT_PERCENT_RANGE.end()) / 100.0,
        );
        self
    }
}

/// The library: folders, subscriptions and their articles, in the order
/// they were made.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Library {
    /// Folders
    pub folders: Collection<Folder>,
    /// Subscriptions
    pub feeds: Collection<Feed>,
    /// Articles
    pub articles: Collection<Article>,
}

impl Library {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.feeds.is_empty() && self.articles.is_empty()
    }

    // --- lookups ---------------------------------------------------------

    pub fn feed(&self, id: &Id) -> Option<Row<'_, Feed>> {
        let at = self.feeds.position(id)?;
        self.feeds.at(at).map(|(id, feed)| Row::new(id, feed))
    }

    pub fn folder(&self, id: &Id) -> Option<Row<'_, Folder>> {
        let at = self.folders.position(id)?;
        self.folders.at(at).map(|(id, folder)| Row::new(id, folder))
    }

    pub fn article(&self, id: &Id) -> Option<Row<'_, Article>> {
        let at = self.articles.position(id)?;
        self.articles
            .at(at)
            .map(|(id, article)| Row::new(id, article))
    }

    pub fn article_mut(&mut self, id: &Id) -> Option<&mut Article> {
        self.articles.get_mut(id)
    }

    /// Every subscription, in the order they were added.
    pub fn feed_rows(&self) -> impl Iterator<Item = Row<'_, Feed>> {
        self.feeds.iter().map(|(id, feed)| Row::new(id, feed))
    }

    /// Every article, in the order they arrived.
    pub fn article_rows(&self) -> impl Iterator<Item = Row<'_, Article>> {
        self.articles
            .iter()
            .map(|(id, article)| Row::new(id, article))
    }

    /// Folders in the order they were made.
    pub fn folders_sorted(&self) -> Vec<Row<'_, Folder>> {
        self.folders
            .iter()
            .map(|(id, folder)| Row::new(id, folder))
            .collect()
    }

    /// Subscriptions in a folder (`None`: at the top of the sidebar), in
    /// the order they were added.
    pub fn feeds_in_folder(&self, folder: Option<&Id>) -> Vec<Row<'_, Feed>> {
        self.feed_rows()
            .filter(|feed| feed.folder.as_ref() == folder)
            .collect()
    }

    /// The subscription to `url`, if the library has one.
    pub fn feed_at(&self, url: &str) -> Option<Row<'_, Feed>> {
        let url = url.trim();
        self.feed_rows().find(|feed| feed.url == url)
    }

    // --- counts ----------------------------------------------------------

    pub fn unread_for_feed(&self, feed: &Id) -> usize {
        self.articles
            .values()
            .filter(|a| &a.feed_id == feed && !a.read)
            .count()
    }

    pub fn unread_for_folder(&self, folder: &Id) -> usize {
        self.unread_count_for(&Filter::Folder(folder.clone()))
    }

    pub fn total_unread(&self) -> usize {
        self.articles.values().filter(|a| !a.read).count()
    }

    pub fn total_starred(&self) -> usize {
        self.articles.values().filter(|a| a.starred).count()
    }

    // --- filtering -------------------------------------------------------

    /// Does an article match the active filter?
    fn matches(&self, a: &Article, filter: &Filter) -> bool {
        match filter {
            Filter::All => true,
            Filter::Unread => !a.read,
            Filter::Starred => a.starred,
            Filter::Feed(id) => &a.feed_id == id,
            Filter::Folder(fid) => self
                .feeds
                .get(&a.feed_id)
                .is_some_and(|f| f.folder.as_ref() == Some(fid)),
            Filter::Tag(key) => a.tags.iter().any(|tag| tag_key(tag) == *key),
        }
    }

    /// Unique article tags, alphabetized for navigation and the places
    /// the library publishes.
    pub fn tags(&self) -> Vec<String> {
        self.tags_limited(usize::MAX)
    }

    /// Alphabetized unique tags with bounded retained allocation. The scan is
    /// linear in cached articles, but sidebar/resource materialization never
    /// grows with an archival tag vocabulary.
    pub fn tags_limited(&self, limit: usize) -> Vec<String> {
        let mut tags = self
            .articles
            .values()
            .flat_map(|article| article.tags.iter())
            .fold(Vec::with_capacity(limit.min(32)), |mut tags, tag| {
                match tags.binary_search(tag) {
                    Err(index) if index < limit => {
                        tags.insert(index, tag.clone());
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
        let tag = tag.to_owned();
        self.articles
            .values()
            .any(|article| article.tags.contains(&tag))
    }

    /// Articles matching `filter`, newest first.
    pub fn articles_for(&self, filter: &Filter) -> Vec<Row<'_, Article>> {
        let mut v: Vec<Row<'_, Article>> = self
            .article_rows()
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

    pub fn count_for(&self, filter: &Filter) -> usize {
        self.articles
            .values()
            .filter(|a| self.matches(a, filter))
            .count()
    }

    pub fn unread_count_for(&self, filter: &Filter) -> usize {
        self.articles
            .values()
            .filter(|a| self.matches(a, filter) && !a.read)
            .count()
    }

    /// The unread articles matching `filter`, in the order they arrived.
    pub fn unread_ids_for(&self, filter: &Filter) -> Vec<Id> {
        self.article_rows()
            .filter(|a| self.matches(a, filter) && !a.read)
            .map(|a| a.id.clone())
            .collect()
    }

    // --- what this window changes before the document follows -------------

    pub fn mark_read(&mut self, id: &Id, read: bool) -> bool {
        if let Some(a) = self.article_mut(id)
            && a.read != read
        {
            a.read = read;
            return true;
        }
        false
    }

    pub fn toggle_star(&mut self, id: &Id) -> bool {
        if let Some(a) = self.article_mut(id) {
            a.starred = !a.starred;
            return true;
        }
        false
    }

    /// Mark every article matching `filter` as read. Returns affected ids.
    pub fn mark_all_read(&mut self, filter: &Filter) -> Vec<Id> {
        let ids = self.unread_ids_for(filter);
        for id in &ids {
            if let Some(a) = self.article_mut(id) {
                a.read = true;
            }
        }
        ids
    }

    /// Whether the subscription `feed` already holds an article at `url`
    /// (or one titled `title`, for a feed whose articles have no links).
    pub fn has_article(&self, feed: &Id, url: &str, title: &str) -> bool {
        self.articles.values().any(|a| {
            &a.feed_id == feed
                && if url.is_empty() {
                    a.url.is_empty() && a.title == title
                } else {
                    a.url == url
                }
        })
    }

    /// Unsubscribe: remove a feed and every article belonging to it.
    /// Returns the ids of the articles that were removed. A no-op returning
    /// an empty vec if the feed is unknown.
    pub fn remove_feed(&mut self, id: &Id) -> Vec<Id> {
        let removed: Vec<Id> = self
            .article_rows()
            .filter(|a| &a.feed_id == id)
            .map(|a| a.id.clone())
            .collect();
        self.articles.retain(|_, a| &a.feed_id != id);
        self.feeds.remove(id);
        removed
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

    fn feed(s: &mut Library, id: &str, folder: Option<&Id>) -> Id {
        let id = Id::from_raw(id);
        s.feeds.push(
            id.clone(),
            Feed {
                title: "F".into(),
                url: format!("http://x/{id}"),
                folder: folder.cloned(),
                ..Feed::default()
            },
        );
        id
    }

    fn art(s: &mut Library, id: &str, feed_id: &Id, read: bool, starred: bool, when: i64) -> Id {
        let id = Id::from_raw(id);
        s.articles.push(
            id.clone(),
            Article {
                feed_id: feed_id.clone(),
                title: format!("a{when}"),
                url: format!("http://x/{id}"),
                published: when,
                read,
                starred,
                ..Article::default()
            },
        );
        id
    }

    /// The library sorts articles, tracks unread counts, filters, marks
    /// read, finds articles it already holds and removes only a feed's own
    /// articles.
    #[test]
    fn library_orders_filters_counts_and_finds_articles() {
        // --- phase: articles_for_sorts_newest_first ---
        {
            let mut s = Library::new();
            let f = feed(&mut s, "f", None);
            art(&mut s, "a1", &f, false, false, 100);
            art(&mut s, "a2", &f, false, false, 300);
            art(&mut s, "a3", &f, false, false, 200);
            let got: Vec<i64> = s
                .articles_for(&Filter::All)
                .iter()
                .map(|a| a.published)
                .collect();
            assert_eq!(got, vec![300, 200, 100]);
        }

        // --- phase: unread_counts_track_feed_and_folder ---
        {
            let mut s = Library::new();
            let folder = Id::from_raw("n");
            s.folders.push(folder.clone(), Folder::named("N"));
            let f = feed(&mut s, "f", Some(&folder));
            art(&mut s, "a1", &f, false, false, 1);
            art(&mut s, "a2", &f, true, false, 2);
            assert_eq!(s.unread_for_feed(&f), 1);
            assert_eq!(s.unread_for_folder(&folder), 1);
            assert_eq!(s.total_unread(), 1);
        }

        // --- phase: filter_unread_and_starred ---
        {
            let mut s = Library::new();
            let f = feed(&mut s, "f", None);
            let a1 = art(&mut s, "a1", &f, false, true, 1);
            art(&mut s, "a2", &f, true, false, 2);
            assert_eq!(s.articles_for(&Filter::Unread).len(), 1);
            assert_eq!(s.articles_for(&Filter::Starred).len(), 1);
            assert_eq!(s.articles_for(&Filter::Starred)[0].id, &a1);
        }

        // --- phase: mark_all_read_clears_filter ---
        {
            let mut s = Library::new();
            let f = feed(&mut s, "f", None);
            art(&mut s, "a1", &f, false, false, 1);
            art(&mut s, "a2", &f, false, false, 2);
            let affected = s.mark_all_read(&Filter::All);
            assert_eq!(affected.len(), 2);
            assert_eq!(s.total_unread(), 0);
        }

        // --- phase: an article at the same address is already held ---
        {
            let mut s = Library::new();
            let f = feed(&mut s, "f", None);
            let other = feed(&mut s, "g", None);
            let a = art(&mut s, "a1", &f, false, false, 1);
            let url = s.article(&a).unwrap().url.clone();
            assert!(s.has_article(&f, &url, "whatever"), "same url is held");
            assert!(!s.has_article(&other, &url, "whatever"), "per feed");
        }

        // --- phase: remove_feed_drops_feed_and_its_articles_only ---
        {
            let mut s = Library::new();
            let keep = feed(&mut s, "keep", None);
            let drop = feed(&mut s, "drop", None);
            let survivor = art(&mut s, "survivor", &keep, false, false, 1);
            let doomed = art(&mut s, "doomed", &drop, false, false, 2);

            let removed = s.remove_feed(&drop);
            assert_eq!(removed, vec![doomed], "returns the removed article ids");
            assert!(s.feed(&drop).is_none(), "the feed is gone");
            assert!(s.feed(&keep).is_some(), "the other feed survives");
            assert_eq!(
                s.articles.len(),
                1,
                "only the doomed feed's article is dropped"
            );
            assert!(s.articles.contains(&survivor));
            // Removing an unknown feed is a harmless no-op.
            assert!(s.remove_feed(&Id::from_raw("gone")).is_empty());
        }
    }

    /// Typed tags normalize; the model refuses tags that are not.
    #[test]
    fn tags_normalize_and_the_model_refuses_others() {
        assert_eq!(
            normalize_tags(["  #Design", "design", "", "Later "]),
            ["design", "later"]
        );
        let mut article = Article::default();
        article.tags.insert("design".into());
        assert!(article.valid().is_ok());
        article.tags.insert("Design".into());
        assert!(article.valid().is_err());
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
            assert_eq!(ReadFont::from_key("serif"), ReadFont::Serif);
            // An unknown or missing paper follows the system theme.
            assert_eq!(ReadTheme::from_key(""), ReadTheme::System);
            assert_eq!(ReadSettings::default().theme, ReadTheme::System);
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
            assert_eq!(
                s.font_scale,
                f32::from(*ReadSettings::TEXT_PERCENT_RANGE.end()) / 100.0
            );
            let s = ReadSettings {
                font_scale: 0.1,
                ..Default::default()
            }
            .clamped();
            assert_eq!(
                s.font_scale,
                f32::from(*ReadSettings::TEXT_PERCENT_RANGE.start()) / 100.0
            );
            let s = ReadSettings {
                font_scale: 1.1,
                ..Default::default()
            };
            assert_eq!(s.percent(), 110.0);
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
