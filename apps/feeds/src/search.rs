//! What search finds in the library: its folders, its feeds and their
//! articles, each kind under its own heading with the mark the sidebar gives
//! it, and a preview worth reading before opening one.

use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::{SearchEntry, SearchPreview, SearchWeight};

use crate::model::{Article, BlockKind, Feed, FeedKind, Folder, Library, Row};

/// The heading folders are listed under.
pub const FOLDERS: &str = "Folders";
/// The heading feeds are listed under.
pub const FEEDS: &str = "Feeds";
/// The heading articles are listed under.
pub const ARTICLES: &str = "Articles";

/// How many of a feed's newest articles its preview names.
const LATEST: usize = 3;
/// How much of an article's text its preview starts with, in characters.
const EXCERPT_CHARS: usize = 280;

/// Every folder, feed and article, as search lists them.
pub fn entries(library: &Library) -> Vec<SearchEntry> {
    let folders = library
        .folders_sorted()
        .into_iter()
        .map(|folder| folder_entry(library, &folder));
    let feeds = library.feed_rows().map(|feed| feed_entry(library, &feed));
    let articles = library
        .article_rows()
        .map(|article| article_entry(library, &article));
    folders.chain(feeds).chain(articles).collect()
}

/// The mark a feed wears in the sidebar.
pub fn feed_icon(kind: FeedKind) -> ui::icons::Icon {
    match kind {
        // A Gemini capsule is a page of gemtext.
        FeedKind::Gemini => ui::icons::Icon::FileText,
        _ => ui::icons::Icon::Rss,
    }
}

fn folder_entry(library: &Library, folder: &Row<'_, Folder>) -> SearchEntry {
    let feeds = library.feeds_in_folder(Some(folder.id));
    let mut preview = SearchPreview::titled(&folder.name).fact(count(feeds.len(), "feed", "feeds"));
    let unread = library.unread_for_folder(folder.id);
    if unread > 0 {
        preview = preview.fact(format!("{unread} unread"));
    }
    for feed in &feeds {
        preview = preview.line(feed.shown_title());
    }
    SearchEntry::new(&folder.name, String::new())
        .anchored(item_resource_id::<Library>("folders", folder.id))
        .grouped(FOLDERS)
        .with_icon(ui::icons::Icon::Folder.name())
        .previewed(preview)
}

fn feed_entry(library: &Library, feed: &Row<'_, Feed>) -> SearchEntry {
    let title = feed.shown_title();
    let mut preview = SearchPreview::titled(&title);
    if let Some(site) = host(&feed.site_url) {
        preview = preview.fact(site);
    }
    let unread = library.unread_for_feed(feed.id);
    preview = preview.fact(if unread > 0 {
        format!("{unread} unread")
    } else {
        "All read".to_owned()
    });
    for article in library
        .articles_for(&crate::model::Filter::Feed(feed.id.clone()))
        .into_iter()
        .take(LATEST)
    {
        preview = preview.line(article.title.clone());
    }
    let mut entry = SearchEntry::new(&title, String::new())
        .anchored(item_resource_id::<Library>("feeds", feed.id))
        .grouped(FEEDS)
        .with_icon(feed_icon(feed.kind).name())
        .previewed(preview);
    entry
        .fields
        .push((feed.url.clone(), SearchWeight::Identity));
    entry
        .fields
        .push((feed.site_url.clone(), SearchWeight::Identity));
    entry
}

fn article_entry(library: &Library, article: &Row<'_, Article>) -> SearchEntry {
    let source = library
        .feed(&article.feed_id)
        .map(|feed| feed.shown_title());
    let mut preview = SearchPreview::titled(&article.title);
    if let Some(source) = &source {
        preview = preview.fact(source.clone());
    }
    if let Some(date) = published(article.published) {
        preview = preview.fact(date);
    }
    if let Some(excerpt) = excerpt(article) {
        preview = preview.body(excerpt);
    }
    let mut entry = SearchEntry::new(&article.title, String::new())
        .anchored(item_resource_id::<Library>("articles", article.id))
        .grouped(ARTICLES)
        .with_icon(ui::icons::Icon::FileText.name())
        .previewed(preview);
    if let Some(source) = source {
        entry = entry.subtitled(source);
    }
    if !article.author.is_empty() {
        entry
            .fields
            .push((article.author.clone(), SearchWeight::Subtitle));
    }
    for tag in article.tag_list() {
        entry.fields.push((tag, SearchWeight::Metadata));
    }
    if !article.summary.is_empty() {
        entry
            .fields
            .push((article.summary.clone(), SearchWeight::Body));
    }
    entry
}

/// `n` of something, in the singular for one.
fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The site an address is on, without its scheme and `www.`.
fn host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.strip_prefix("www.").unwrap_or(host);
    (!host.is_empty()).then(|| host.to_owned())
}

/// The day an article came out, when the feed said.
fn published(at: i64) -> Option<String> {
    use ui::format::Moment as _;
    if at <= 0 {
        return None;
    }
    ui::format::UnixSecs(at)
        .local()
        .map(|moment| ui::format::month_day_year(moment.date()))
}

/// The start of what an article says: its summary, else its first
/// paragraphs.
fn excerpt(article: &Article) -> Option<String> {
    let text = if article.summary.trim().is_empty() {
        article
            .blocks
            .iter()
            .filter(|block| matches!(block.kind, BlockKind::Paragraph | BlockKind::Quote))
            .map(|block| block.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        article.summary.trim().to_owned()
    };
    if text.is_empty() {
        return None;
    }
    let mut chars = text.chars();
    let start: String = chars.by_ref().take(EXCERPT_CHARS).collect();
    Some(if chars.next().is_some() {
        format!("{}…", start.trim_end())
    } else {
        start
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Library {
        crate::sample::store()
    }

    fn of_kind<'a>(entries: &'a [SearchEntry], group: &str) -> Vec<&'a SearchEntry> {
        entries
            .iter()
            .filter(|entry| entry.group.as_deref() == Some(group))
            .collect()
    }

    /// Folders, feeds and articles are listed apart, each with the sidebar's
    /// mark, and every entry the library holds is one of them.
    #[test]
    fn each_kind_is_its_own_group_with_its_own_mark() {
        let library = sample();
        let entries = entries(&library);
        let (folders, feeds, articles) = (
            of_kind(&entries, FOLDERS),
            of_kind(&entries, FEEDS),
            of_kind(&entries, ARTICLES),
        );
        assert_eq!(folders.len(), library.folders_sorted().len());
        assert_eq!(feeds.len(), library.feed_rows().count());
        assert_eq!(articles.len(), library.article_rows().count());
        assert_eq!(entries.len(), folders.len() + feeds.len() + articles.len());
        assert!(
            folders
                .iter()
                .all(|entry| entry.icon.as_deref() == Some("folder"))
        );
        assert!(
            feeds
                .iter()
                .all(|entry| { matches!(entry.icon.as_deref(), Some("rss" | "file-text")) })
        );
        assert!(
            articles
                .iter()
                .all(|entry| entry.icon.as_deref() == Some("file-text"))
        );
    }

    /// An article reads as the list shows it, with its feed under its title;
    /// its preview has its source, its date and the start of its text, never
    /// its address or the raw fields it is found by.
    #[test]
    fn an_article_previews_its_source_date_and_first_lines() {
        let library = sample();
        let entries = entries(&library);
        let article = library
            .article_rows()
            .find(|article| article.published > 0 && !article.summary.is_empty())
            .expect("the sample has a dated article with a summary");
        let feed = library.feed(&article.feed_id).expect("its feed");
        let entry = entries
            .iter()
            .find(|entry| entry.group.as_deref() == Some(ARTICLES) && entry.title == article.title)
            .expect("the article is found");
        assert_eq!(entry.subtitle.as_deref(), Some(feed.shown_title().as_str()));
        assert!(entry.text.is_empty(), "nothing raw reads as its preview");
        let preview = entry.preview.as_ref().expect("an article previews");
        assert_eq!(preview.title.as_deref(), Some(article.title.as_str()));
        assert_eq!(preview.facts.first(), Some(&feed.shown_title()));
        assert_eq!(preview.facts.len(), 2, "its source and its date");
        assert!(
            preview
                .body
                .as_deref()
                .is_some_and(|body| !body.contains("://"))
        );
    }

    /// A feed previews its site, how much is unread and its newest articles;
    /// a folder, the feeds it holds.
    #[test]
    fn a_feed_and_a_folder_preview_what_they_hold() {
        let library = sample();
        let entries = entries(&library);
        let feed = library.feed_rows().next().expect("a feed");
        let feed_entry = entries
            .iter()
            .find(|entry| {
                entry.group.as_deref() == Some(FEEDS) && entry.title == feed.shown_title()
            })
            .expect("the feed is found");
        let preview = feed_entry.preview.as_ref().expect("a feed previews");
        assert_eq!(preview.facts.first(), host(&feed.site_url).as_ref());
        let newest: Vec<String> = library
            .articles_for(&crate::model::Filter::Feed(feed.id.clone()))
            .into_iter()
            .take(LATEST)
            .map(|article| article.title.clone())
            .collect();
        assert_eq!(preview.lines, newest);
        assert!(
            feed_entry.fields.iter().any(|(text, _)| *text == feed.url),
            "a feed is still found by its address"
        );

        let folder = library
            .folders_sorted()
            .into_iter()
            .next()
            .expect("a folder");
        let folder_entry = entries
            .iter()
            .find(|entry| entry.group.as_deref() == Some(FOLDERS) && entry.title == folder.name)
            .expect("the folder is found");
        let held: Vec<String> = library
            .feeds_in_folder(Some(folder.id))
            .iter()
            .map(|feed| feed.shown_title())
            .collect();
        let preview = folder_entry.preview.as_ref().expect("a folder previews");
        assert_eq!(preview.lines, held);
        assert_eq!(
            preview.facts.first().map(String::as_str),
            Some(count(held.len(), "feed", "feeds").as_str())
        );
    }
}
