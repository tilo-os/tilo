//! The article list's rows: what each says and how tall it is.

use super::*;
use crate::model::{Fetched, Library};

/// A library of one feed and the articles titled and summarised so.
fn core_with(articles: &[(&str, &str)]) -> Core {
    let mut core = Core::new(false);
    let mut library = Library::new();
    let feed = Id::from_raw("feed");
    library.feeds.push(
        feed.clone(),
        crate::model::Feed::subscribing("https://example.org/feed.xml", None),
    );
    for (n, (title, summary)) in articles.iter().enumerate() {
        library.articles.push(
            Id::from_raw(format!("article-{n}")),
            Article::fetched(
                feed.clone(),
                Fetched {
                    title: (*title).to_owned(),
                    summary: (*summary).to_owned(),
                    url: format!("https://example.org/{n}"),
                    ..Fetched::default()
                },
            ),
        );
    }
    core.store = std::rc::Rc::new(library);
    core
}

/// A row's excerpt never opens with the article's title, whatever the feed sent; a row whose
/// article has no excerpt names its feed.
#[test]
fn a_rows_excerpt_never_repeats_its_title() {
    let core = core_with(&[
        (
            "The AI Pascal's Wager",
            "The AI Pascal's Wager Should you bet on artificial intelligence?",
        ),
        ("Plain", "An excerpt that does not repeat anything."),
        ("Bare", ""),
    ]);
    let rows: Vec<String> = core
        .store
        .article_rows()
        .map(|article| core.row_secondary(article.item()).into_owned())
        .collect();
    assert_eq!(rows[0], "Should you bet on artificial intelligence?");
    assert_eq!(rows[1], "An excerpt that does not repeat anything.");
    assert_eq!(
        rows[2], "example.org",
        "an article without one names its feed"
    );
}

/// Every article row is laid out in the one face, whatever its title and excerpt, so rows share
/// one height.
#[test]
fn every_article_row_has_one_height() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let site = AvatarFace::new("example.org", "https://example.org/feed.xml").square();
    let heights: Vec<f64> = [
        ("Short", ""),
        (
            "A much longer title that runs well past one line of a list",
            "An excerpt.",
        ),
        (
            "Short",
            "A long excerpt that would happily take several lines if it could.",
        ),
    ]
    .iter()
    .map(|(title, excerpt)| {
        article_face(title, excerpt, site)
            .meta("09:41")
            .height(&mut text, &theme)
    })
    .collect();
    assert!(
        heights
            .windows(2)
            .all(|pair| (pair[0] - pair[1]).abs() < 0.5),
        "{heights:?}"
    );
}
