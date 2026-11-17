//! The library's articles, kept in numbered parts beside the library.
//!
//! One resource cannot hold every article a reader ever fetched at a cost that
//! stays flat: every reader of a resource loads, verifies and diffs all of it,
//! so one article marked read would cost every article fetched before it. The
//! articles live in a chain of [`ArticlePart`] resources instead (`tilo_app`'s
//! `Chained`), each holding up to [`PART_ARTICLES`] articles or
//! [`PART_TEXT_BYTES`] of their text; a change touches the one part holding its
//! article. What is above the articles — the folders and the subscriptions —
//! stays in the library's own document ([`crate::model::Library`]).
//!
//! - **Where a fetched article goes.** To the newest part, unless it is full
//!   ([`new_part`]): then to the next. Two writers starting the same part
//!   start the same resource, so their articles meet there.
//! - **Where a change goes.** To the part holding the article it names.
//! - **Reading in one go.** Marking a subscription (or everything) read is one
//!   step over articles of many parts, which no one part can take. It is
//!   written as a mark in the part it is invoked on — how many of a
//!   subscription's articles there were read ([`ArticlePart::mark_read`]) —
//!   and the parts before that one count as read with it ([`Articles`]). One
//!   small write, whatever the library holds.
//! - **What reads across parts.** An article whose subscription is gone is
//!   not shown and does not count: read so, never rewritten in other parts.

use std::collections::BTreeMap;
use std::rc::Rc;

use ui::framework::Chained;
use ui::tilo_resource::{self, ActionCx, Collection, Derived, Id, Model, Refusal, resource};

use crate::model::{Article, Fetched, Filter, Library, Row, tag_key};

/// Articles a part holds before the next one starts.
pub const PART_ARTICLES: usize = 300;

/// Text, in bytes, a part's articles hold (titles, summaries and bodies)
/// before the next one starts: what keeps one part, and so one change, cheap
/// whatever its articles say.
pub const PART_TEXT_BYTES: usize = 1024 * 1024;

/// One part of the library's articles: every article a refresh brought in
/// while it was the newest, and everything done to them since.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ArticlePart {
    /// Articles
    ///
    /// Each article is a storage unit of its own.
    #[model(unit)]
    pub articles: Collection<Article>,
    /// Read through
    ///
    /// By subscription: how many of its articles in this part were marked
    /// read in one go. A subscription's articles in the parts before this one
    /// were marked with them.
    #[model(readonly)]
    pub read_through: BTreeMap<String, i64>,
}

impl ArticlePart {
    /// The text its articles hold, in bytes: titles, summaries and bodies.
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        self.articles
            .values()
            .map(|article| {
                article
                    .title
                    .len()
                    .saturating_add(article.summary.len())
                    .saturating_add(
                        article
                            .blocks
                            .iter()
                            .map(|block| {
                                block
                                    .text
                                    .len()
                                    .saturating_add(block.href.as_deref().unwrap_or_default().len())
                            })
                            .sum::<usize>(),
                    )
            })
            .sum()
    }

    /// How many of subscription `feed`'s articles this part holds.
    #[must_use]
    pub fn of_feed(&self, feed: &Id) -> usize {
        self.articles
            .values()
            .filter(|article| &article.feed_id == feed)
            .count()
    }

    fn article_mut(&mut self, article: &Id) -> Result<&mut Article, Refusal> {
        self.articles
            .get_mut(article)
            .ok_or_else(|| Refusal::new("This article no longer exists"))
    }

    /// The places derived from this part's articles: each tag reveals the
    /// articles it labels. The chain publishes them under its own address,
    /// so the places of every part meet there.
    #[must_use]
    pub fn places(&self) -> Vec<Derived<Self>> {
        let mut tags: Vec<&str> = self
            .articles
            .values()
            .flat_map(|article| article.tags.iter().map(String::as_str))
            .collect::<Vec<_>>();
        tags.sort_unstable();
        tags.dedup();
        tags.into_iter()
            .take(crate::resource::MAX_TAG_PLACES)
            .map(Self::tag_place)
            .collect()
    }

    /// The tag `tag` as a place.
    pub fn tag_place(tag: &str) -> Derived<Self> {
        Derived::new(format!("#{tag}"))
            .of_kind("tag")
            .identified(format!("tag:{tag}"))
            .with_effect(&crate::resource::FeedsEffect::RevealTag {
                tag: tag.to_owned(),
            })
    }

    /// What system search finds of the part: each article, by its title, its
    /// author and what it says.
    #[must_use]
    pub fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        crate::search::part_entries(self)
    }

    /// What a part's articles hold, as [`crate::model::Library::read_document`]
    /// answers.
    ///
    /// # Errors
    /// Refuses an item the part does not hold.
    pub fn read_document(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

/// The part a fetched article goes to, given the newest part held (its number
/// and state): that part, or the next once it is full.
#[must_use]
pub fn new_part(head: Option<(u64, &ArticlePart)>) -> u64 {
    match head {
        None => 0,
        Some((index, part))
            if part.articles.len() >= PART_ARTICLES || part.text_bytes() >= PART_TEXT_BYTES =>
        {
            index.saturating_add(1)
        }
        Some((index, _)) => index,
    }
}

#[resource(
    kind = "feeds.articles",
    params = article_part,
    children = ArticlePart::places,
    search = ArticlePart::search,
    effects = crate::resource::FeedsEffect,
    read_content = ArticlePart::read_document
)]
impl ArticlePart {
    /// Land what a refresh fetched for one subscription.
    ///
    /// The articles the library does not hold yet; a refresh of the same feed
    /// brings nothing twice. Content from the feed, not anyone's edit.
    #[action(external, audience = "agents", writes = articles)]
    pub fn receive(
        &mut self,
        cx: &mut ActionCx,
        #[param("The subscription")] feed: Id,
        #[param("The articles it lists")] articles: Vec<Fetched>,
    ) -> Result<(), Refusal> {
        for article in articles {
            let id = cx.id();
            self.articles
                .push(id, Article::fetched(feed.clone(), article));
        }
        Ok(())
    }

    /// Mark a subscription's articles read, or every article, as far as this
    /// part holds them. The parts before it are read with it.
    #[action(writes = read_through)]
    pub fn mark_read(&mut self, #[param("The subscription; every one without")] feed: Option<Id>) {
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        for article in self.articles.values() {
            let count = counts
                .entry(article.feed_id.as_str().to_owned())
                .or_insert_with(|| {
                    self.read_through
                        .get(article.feed_id.as_str())
                        .copied()
                        .unwrap_or_default()
                });
            *count = count.saturating_add(1);
        }
        match feed {
            Some(feed) => {
                let key = feed.as_str().to_owned();
                let count = counts.remove(&key).unwrap_or_default();
                self.read_through.insert(key, count);
            }
            None => self.read_through = counts,
        }
    }

    /// Mark one article read or unread, as its row's and its page's control
    /// does.
    #[action(item = articles, field = "read", writes = articles)]
    pub fn set_read(&mut self, article: &Id, read: bool) -> Result<(), Refusal> {
        self.article_mut(article)?.read = read;
        Ok(())
    }

    /// Star an article, or take the star back.
    #[action(item = articles, field = "starred", writes = articles)]
    pub fn set_starred(&mut self, article: &Id, starred: bool) -> Result<(), Refusal> {
        self.article_mut(article)?.starred = starred;
        Ok(())
    }

    /// Give an article the tags it should have, replacing the ones it had.
    #[action(item = articles, step = "edit", writes = articles)]
    pub fn set_tags(
        &mut self,
        article: &Id,
        #[param("The tags it carries, up to 24")] tags: Vec<String>,
    ) -> Result<(), Refusal> {
        let target = self.article_mut(article)?;
        let mut kept = tilo_resource::Set::new();
        for tag in crate::model::normalize_tags(tags.iter().map(String::as_str)) {
            kept.insert(tag);
        }
        if kept.len() > crate::model::MAX_TAGS {
            return Err(Refusal::new("an article carries at most 24 tags"));
        }
        target.tags = kept;
        Ok(())
    }

    /// Drop the articles of a subscription this part holds, as unsubscribing
    /// does. Articles of a subscription the library no longer has are not
    /// shown anyway; this is how a reader takes the room back.
    #[action(destructive, writes = articles)]
    pub fn forget_feed(&mut self, #[param("The subscription")] feed: Id) -> Result<(), Refusal> {
        let going: Vec<Id> = self
            .articles
            .iter()
            .filter(|(_, article)| article.feed_id == feed)
            .map(|(id, _)| id.clone())
            .collect();
        for id in going {
            self.articles.remove(&id);
        }
        self.read_through.remove(feed.as_str());
        Ok(())
    }
}

/// Every article the held parts keep, read as one library: newest first for a
/// list, in the order they arrived for unread runs, each found by its id, with
/// what reads across parts (an article of a subscription that is gone, how far
/// a subscription was marked read) resolved the same way everywhere.
///
/// Built from the parts once per change of any of them, and read by the list,
/// the reader and the checks an action makes where it is invoked.
pub struct Articles<'a> {
    /// Every article, in the order the parts hold them: oldest part first,
    /// each part in the order its articles arrived.
    ordered: Vec<(&'a Id, &'a Article, u64, usize)>,
    /// Where each article is in `ordered`.
    at: std::collections::HashMap<&'a Id, usize>,
    /// How far each subscription was marked read: the part, and how many of
    /// its articles in that part. The parts before it are read with it.
    through: BTreeMap<&'a str, (u64, i64)>,
    /// The subscriptions the library still has, by id, and the folder each is
    /// listed in: an article of one it does not have is not shown.
    feeds: BTreeMap<&'a Id, Option<&'a Id>>,
}

impl<'a> Articles<'a> {
    /// The articles `parts` keep, with the subscriptions `library` has.
    #[must_use]
    pub fn of(parts: &'a [(u64, Rc<ArticlePart>)], library: &'a Library) -> Self {
        let mut ordered = Vec::new();
        let mut through: BTreeMap<&'a str, (u64, i64)> = BTreeMap::new();
        for (index, part) in parts {
            let mut seen: BTreeMap<&'a str, usize> = BTreeMap::new();
            for (id, article) in part.articles.iter() {
                let position = *seen.entry(article.feed_id.as_str()).or_default();
                seen.insert(article.feed_id.as_str(), position.saturating_add(1));
                ordered.push((id, article, *index, position));
            }
            for (feed, count) in &part.read_through {
                let mark = (*index, *count);
                through
                    .entry(feed.as_str())
                    .and_modify(|held| {
                        if mark.0 > held.0 {
                            *held = mark;
                        }
                    })
                    .or_insert(mark);
            }
        }
        let at = ordered
            .iter()
            .enumerate()
            .map(|(position, (id, ..))| (*id, position))
            .collect();
        let feeds = library
            .feeds
            .iter()
            .map(|(id, feed)| (id, feed.folder.as_ref()))
            .collect();
        Self {
            ordered,
            at,
            through,
            feeds,
        }
    }

    /// How many articles there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shown().count()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every article the library shows, in the order the parts keep them.
    pub fn iter(&self) -> impl Iterator<Item = (&'a Id, &'a Article)> + '_ {
        self.shown().map(|(id, article, ..)| (id, article))
    }

    /// Every article with the part and the place in that part it is kept at.
    fn held(&self) -> impl Iterator<Item = (&'a Id, &'a Article, u64, usize)> + '_ {
        self.ordered.iter().copied()
    }

    /// The articles the library still shows: those of a subscription it has.
    fn shown(&self) -> impl Iterator<Item = (&'a Id, &'a Article, u64, usize)> + '_ {
        self.held()
            .filter(|(_, article, ..)| self.feeds.contains_key(&article.feed_id))
    }

    /// The article `id`, if one is shown.
    #[must_use]
    pub fn article(&self, id: &Id) -> Option<Row<'a, Article>> {
        let position = *self.at.get(id)?;
        let (id, article, ..) = *self.ordered.get(position)?;
        self.shows(article).then(|| Row::new(id, article))
    }

    /// Whether the library still shows `article`.
    fn shows(&self, article: &Article) -> bool {
        self.feeds.contains_key(&article.feed_id)
    }

    /// Whether `article`, kept in part `part` at `position`, was read: on its
    /// own, or with the run a subscription was marked read to.
    fn read(&self, article: &Article, part: u64, position: usize) -> bool {
        article.read
            || self
                .through
                .get(article.feed_id.as_str())
                .is_some_and(|(through_part, seen)| {
                    part < *through_part
                        || (part == *through_part
                            && i64::try_from(position).unwrap_or(i64::MAX) < *seen)
                })
    }

    /// Whether a shown article matches `filter`.
    fn matches(&self, article: &Article, part: u64, position: usize, filter: &Filter) -> bool {
        match filter {
            Filter::All => true,
            Filter::Unread => !self.read(article, part, position),
            Filter::Starred => article.starred,
            Filter::Feed(id) => &article.feed_id == id,
            Filter::Folder(folder) => self.feed_folder(&article.feed_id) == Some(folder),
            Filter::Tag(key) => article.tags.iter().any(|tag| tag_key(tag) == *key),
        }
    }

    fn feed_folder(&self, feed: &Id) -> Option<&'a Id> {
        self.feeds.get(feed).copied().flatten()
    }

    /// The articles showing `filter`, newest first.
    #[must_use]
    pub fn articles_for(&self, filter: &Filter) -> Vec<Row<'a, Article>> {
        let mut rows: Vec<Row<'a, Article>> = self
            .shown()
            .filter(|(_, article, part, position)| self.matches(article, *part, *position, filter))
            .map(|(id, article, ..)| Row::new(id, article))
            .collect();
        rows.sort_by(|a, b| {
            b.published
                .cmp(&a.published)
                .then_with(|| a.title.cmp(&b.title))
        });
        rows
    }

    /// How many articles match `filter`.
    #[must_use]
    pub fn count_for(&self, filter: &Filter) -> usize {
        self.shown()
            .filter(|(_, article, part, position)| self.matches(article, *part, *position, filter))
            .count()
    }

    /// How many articles match `filter` and were not read.
    #[must_use]
    pub fn unread_count_for(&self, filter: &Filter) -> usize {
        self.shown()
            .filter(|(_, article, part, position)| {
                self.matches(article, *part, *position, filter)
                    && !self.read(article, *part, *position)
            })
            .count()
    }

    /// The unread articles matching `filter`, in the order they arrived.
    #[must_use]
    pub fn unread_ids_for(&self, filter: &Filter) -> Vec<Id> {
        self.shown()
            .filter(|(_, article, part, position)| {
                self.matches(article, *part, *position, filter)
                    && !self.read(article, *part, *position)
            })
            .map(|(id, ..)| id.clone())
            .collect()
    }

    /// Whether the subscription `feed` holds an article at `url` (or one
    /// titled `title`, for a feed whose articles have no links).
    #[must_use]
    pub fn has_article(&self, feed: &Id, url: &str, title: &str) -> bool {
        self.held().any(|(_, article, ..)| {
            &article.feed_id == feed
                && if url.is_empty() {
                    article.url.is_empty() && article.title == title
                } else {
                    article.url == url
                }
        })
    }

    /// Alphabetized unique tags, at most `limit` of them.
    #[must_use]
    pub fn tags_limited(&self, limit: usize) -> Vec<String> {
        let mut tags: Vec<String> = self
            .shown()
            .flat_map(|(_, article, ..)| article.tags.iter().cloned())
            .collect();
        tags.sort_unstable();
        tags.dedup();
        tags.truncate(limit);
        tags
    }

    /// Every unique tag, alphabetized.
    #[must_use]
    pub fn tags(&self) -> Vec<String> {
        self.tags_limited(usize::MAX)
    }

    /// Whether any article carries `tag`.
    #[must_use]
    pub fn has_tag(&self, tag: &str) -> bool {
        self.shown()
            .any(|(_, article, ..)| article.tags.iter().any(|held| held == tag))
    }

    /// Every shown article, in the order they arrived.
    pub fn article_rows(&self) -> impl Iterator<Item = Row<'a, Article>> + '_ {
        self.shown().map(|(id, article, ..)| Row::new(id, article))
    }

    /// How many of one subscription's articles are unread.
    #[must_use]
    pub fn unread_for_feed(&self, feed: &Id) -> usize {
        self.unread_count_for(&Filter::Feed(feed.clone()))
    }

    /// How many of a folder's articles are unread.
    #[must_use]
    pub fn unread_for_folder(&self, folder: &Id) -> usize {
        self.unread_count_for(&Filter::Folder(folder.clone()))
    }

    /// How many articles are unread.
    #[must_use]
    pub fn total_unread(&self) -> usize {
        self.unread_count_for(&Filter::All)
    }

    /// How many articles are starred.
    #[must_use]
    pub fn total_starred(&self) -> usize {
        self.count_for(&Filter::Starred)
    }

    /// How many subscriptions the library has.
    #[must_use]
    pub fn feed_count(&self) -> usize {
        self.feeds.len()
    }
}

/// The part `action` changes, given the parts held, oldest first: the one
/// holding the article it names, or for what a refresh brought the newest part
/// (or the next, once that is full).
///
/// # Errors
/// Refuses an action naming an article no part held here keeps.
pub fn part_for(
    action: &<ArticlePart as tilo_resource::Resource>::Action,
    parts: &[(u64, Rc<ArticlePart>)],
) -> Result<u64, String> {
    let newest = || new_part(parts.last().map(|(index, part)| (*index, &**part)));
    let holding = |article: &Id| {
        parts
            .iter()
            .rev()
            .find(|(_, part)| part.articles.contains(article))
            .map(|(index, _)| *index)
            .ok_or_else(|| "This article no longer exists".to_owned())
    };
    match &action.operation {
        ArticlePartAction::Receive(_) => Ok(newest()),
        // A mark reads everything before the part it is written in: it goes to
        // the newest one.
        ArticlePartAction::MarkRead(_) | ArticlePartAction::ForgetFeed(_) => {
            Ok(parts.last().map_or(0, |(index, _)| *index))
        }
        other => match item_of(other) {
            Some(article) => holding(article),
            None => Ok(parts.last().map_or(0, |(index, _)| *index)),
        },
    }
}

/// The article a verb on one article names.
#[must_use]
pub fn item_of(action: &ArticlePartAction) -> Option<&Id> {
    use crate::part::article_part as verb;
    match action {
        ArticlePartAction::SetRead(verb::SetRead { article, .. })
        | ArticlePartAction::SetStarred(verb::SetStarred { article, .. })
        | ArticlePartAction::SetTags(verb::SetTags { article, .. }) => Some(article),
        // A field edit of one article (its tags, typed into its row) names it
        // in its path.
        ArticlePartAction::FieldEdit(edit) => match edit.path() {
            [
                tilo_resource::Segment::Field(field),
                tilo_resource::Segment::Item(article),
                ..,
            ] if field == "articles" => Some(article),
            _ => None,
        },
        _ => None,
    }
}

/// Fill in, where `action` is invoked, what it reads of the library's
/// subscriptions and of every article held there, and refuse what they do not
/// allow: a refresh for a subscription the library does not have, articles it
/// already holds.
///
/// # Errors
/// Refuses as the action's verb would, had it every part.
pub fn prepare(
    action: &mut <ArticlePart as tilo_resource::Resource>::Action,
    library: &Library,
    parts: &[(u64, Rc<ArticlePart>)],
) -> Result<(), String> {
    use crate::part::article_part::{ForgetFeed, Receive};
    let held = Articles::of(parts, library);
    match &mut action.operation {
        ArticlePartAction::Receive(Receive { feed, articles }) => {
            if library.feeds.get(feed).is_none() {
                return Err("no such subscription".into());
            }
            articles.retain(|article| !held.has_article(feed, &article.url, &article.title));
        }
        ArticlePartAction::ForgetFeed(ForgetFeed { feed }) if library.feeds.get(feed).is_none() => {
            return Err("no such subscription".into());
        }
        _ => {}
    }
    Ok(())
}

/// The chain the parts belong to, beside the library's own document.
#[must_use]
pub fn chain() -> ui::resource::DocumentId {
    ui::resource::DocumentId::derived("articles", &ui::resource::DocumentId::workspace_default())
}

impl Chained for ArticlePart {
    type Base = Library;

    fn chain() -> ui::resource::DocumentId {
        chain()
    }

    fn part_for(action: &Self::Action, parts: &[(u64, Rc<Self>)]) -> Result<u64, String> {
        part_for(action, parts)
    }

    fn prepare(
        action: &mut Self::Action,
        library: &Library,
        parts: &[(u64, Rc<Self>)],
    ) -> Result<(), String> {
        prepare(action, library, parts)
    }

    /// The list shows every article, so a window holds every part; a part
    /// bounds what one change costs, not what is held.
    const HELD_WHOLE: bool = true;
}

#[cfg(test)]
mod tests;
