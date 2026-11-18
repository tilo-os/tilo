//! What search finds in the library: its folders and subscriptions, and, in
//! the parts the articles are kept in, the articles themselves. Each kind
//! under its own heading with the mark the sidebar gives it, and a preview
//! worth reading before opening one.
//!
//! A part is searched on its own ([`crate::article_list::ArticleList`]): it does not
//! read the library, so an article names where it came from by its address.

use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::{SearchEntry, SearchPreview, SearchWeight};

use crate::article_list::ArticleList;
use crate::model::{Article, BlockKind, Feed, FeedKind, Folder, Library, Row};

pub const FOLDERS: &str = "Folders";
pub const FEEDS: &str = "Feeds";
pub const ARTICLES: &str = "Articles";

const EXCERPT_CHARS: usize = 280;

/// What search finds in the library's own record: its folders and its
/// subscriptions.
pub fn entries(library: &Library) -> Vec<SearchEntry> {
    let folders = library
        .folders_sorted()
        .into_iter()
        .map(|folder| folder_entry(library, &folder));
    let feeds = library.feed_rows().map(|feed| feed_entry(&feed));
    folders.chain(feeds).collect()
}

/// What search finds in one part of the library: its articles.
pub fn part_entries(part: &ArticleList) -> Vec<SearchEntry> {
    part.articles
        .iter()
        .map(|(id, article)| article_entry(id, article))
        .collect()
}

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
    for feed in &feeds {
        preview = preview.line(feed.shown_title());
    }
    SearchEntry::new(&folder.name, String::new())
        .anchored(item_resource_id::<Library>("folders", folder.id))
        .grouped(FOLDERS)
        .with_icon(ui::icons::Icon::Folder.name())
        .previewed(preview)
}

fn feed_entry(feed: &Row<'_, Feed>) -> SearchEntry {
    let title = feed.shown_title();
    let mut preview = SearchPreview::titled(&title);
    if let Some(site) = host(&feed.site_url) {
        preview = preview.fact(site);
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

fn article_entry(id: &ui::tilo_resource::Id, article: &Article) -> SearchEntry {
    let source = host(&article.url);
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
        .anchored(item_resource_id::<ArticleList>("articles", id))
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

fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.strip_prefix("www.").unwrap_or(host);
    (!host.is_empty()).then(|| host.to_owned())
}

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

    fn sample() -> (Library, ArticleList) {
        crate::sample::store()
    }

    fn of_kind<'a>(entries: &'a [SearchEntry], group: &str) -> Vec<&'a SearchEntry> {
        entries
            .iter()
            .filter(|entry| entry.group.as_deref() == Some(group))
            .collect()
    }

    #[test]
    fn each_kind_is_its_own_group_with_its_own_mark() {
        let (library, part) = sample();
        let library_entries = entries(&library);
        let article_entries = part_entries(&part);
        assert_eq!(
            of_kind(&library_entries, FOLDERS).len(),
            library.folders_sorted().len()
        );
        assert_eq!(
            of_kind(&library_entries, FEEDS).len(),
            library.feed_rows().count()
        );
        assert_eq!(
            of_kind(&article_entries, ARTICLES).len(),
            part.articles.len(),
            "every article the part keeps is found"
        );
        assert!(
            of_kind(&library_entries, FOLDERS)
                .iter()
                .all(|entry| entry.icon.as_deref() == Some("folder"))
        );
        assert!(
            of_kind(&library_entries, FEEDS)
                .iter()
                .all(|entry| matches!(entry.icon.as_deref(), Some("rss" | "file-text")))
        );
        assert!(
            article_entries
                .iter()
                .all(|entry| entry.icon.as_deref() == Some("file-text"))
        );
    }

    #[test]
    fn an_article_previews_its_source_date_and_first_lines() {
        let (_, part) = sample();
        let article = part
            .articles
            .iter()
            .find(|(_, article)| article.published > 0 && !article.summary.is_empty())
            .expect("the sample has a dated article with a summary");
        let source = host(&article.1.url).expect("its address names its site");
        let entries = part_entries(&part);
        let entry = entries
            .iter()
            .find(|entry| {
                entry.group.as_deref() == Some(ARTICLES) && entry.title == article.1.title
            })
            .expect("the article is found");
        assert_eq!(entry.subtitle.as_deref(), Some(source.as_str()));
        assert!(entry.text.is_empty(), "nothing raw reads as its preview");
        let preview = entry.preview.as_ref().expect("an article previews");
        assert_eq!(preview.title.as_deref(), Some(article.1.title.as_str()));
        assert_eq!(preview.facts.first(), Some(&source));
        assert_eq!(preview.facts.len(), 2, "its source and its date");
        assert!(
            preview
                .body
                .as_deref()
                .is_some_and(|body| !body.contains("://"))
        );
    }

    #[test]
    fn a_feed_and_a_folder_preview_what_they_hold() {
        let (library, _) = sample();
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
