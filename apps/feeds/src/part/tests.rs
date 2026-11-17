#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion"
)]

//! The articles in parts beside the library: where a write goes, what a part
//! holds, reading in one go across parts, and what one change costs however
//! much the library holds.

use std::rc::Rc;

use ui::tilo_resource::{Id, Invoke, Replica};

use crate::model::{Fetched, Filter, Library};
use crate::part::{ArticlePart, Articles, PART_ARTICLES, article_part};
use crate::resource::library;

/// What a refresh fetched for one article: the same title and address every
/// time, so the dedupe in `prepare` keeps them one.
fn fetched(title: &str) -> Fetched {
    Fetched {
        title: title.to_owned(),
        url: format!("https://example.test/{}", title.replace(' ', "-")),
        published: 1_700_000_000,
        summary: format!("{title}, in a paragraph or two."),
        ..Fetched::default()
    }
}

/// A library and its parts as one device holds them: every action goes to the
/// part the chain sends it to, read against the library and the parts held,
/// as a window carries it out.
struct Shelf {
    library: Replica<Library>,
    /// The library as this device last read it.
    state: Library,
    parts: Vec<(u64, Replica<ArticlePart>)>,
    /// Each part's state as last written, as a window follows them.
    states: Vec<(u64, Rc<ArticlePart>)>,
    author: String,
}

impl Shelf {
    fn new(author: &str) -> Self {
        let mut library = Replica::<Library>::new().as_author(author);
        library.set_now_ms(1_000);
        Self {
            library,
            state: Library::new(),
            parts: Vec::new(),
            states: Vec::new(),
            author: author.to_owned(),
        }
    }

    fn states(&self) -> Vec<(u64, Rc<ArticlePart>)> {
        self.states.clone()
    }

    /// Every article the held parts keep, with the subscriptions the library
    /// has.
    fn articles(&self) -> Articles<'_> {
        Articles::of(&self.states, &self.state)
    }

    /// A subscription of the library, as subscribing makes one.
    fn subscribe(&mut self, url: &str) -> Id {
        self.library
            .apply(Library::ADD_SUBSCRIPTION.with(library::AddSubscription {
                url: url.parse().unwrap(),
                folder: None,
            }))
            .unwrap();
        self.state = self.library.state().unwrap();
        self.state.feed_at(url).map(|feed| feed.id.clone()).unwrap()
    }

    fn part(&mut self, index: u64) -> &mut Replica<ArticlePart> {
        if !self.parts.iter().any(|(held, _)| *held == index) {
            let mut part = Replica::<ArticlePart>::new().as_author(&self.author);
            part.set_now_ms(1_000);
            self.parts.push((index, part));
        }
        self.parts
            .iter_mut()
            .find(|(held, _)| *held == index)
            .map(|(_, part)| part)
            .unwrap()
    }

    /// Follow part `index` after a write into it.
    fn follow(&mut self, index: u64) {
        let state = Rc::new(self.part(index).state().unwrap());
        match self.states.iter_mut().find(|(held, _)| *held == index) {
            Some((_, held)) => *held = state,
            None => self.states.push((index, state)),
        }
    }

    /// Carry `invoke` out as the chain does: to the part `part_for` names,
    /// read against the library and the parts held, and refused as `prepare`
    /// refuses it. The part written.
    fn apply(&mut self, invoke: &Invoke<ArticlePart>) -> Result<u64, String> {
        let mut action = invoke.build(1_000)?;
        let held = self.states();
        crate::part::prepare(&mut action, &self.state, &held)?;
        let index = crate::part::part_for(&action, &held)?;
        self.part(index)
            .apply_action(&action)
            .map_err(|error| error.to_string())?;
        self.follow(index);
        Ok(index)
    }

    /// What a refresh brought for `feed`.
    fn receive(&mut self, feed: &Id, titles: &[&str]) -> Result<u64, String> {
        self.apply(&ArticlePart::RECEIVE.with(article_part::Receive {
            feed: feed.clone(),
            articles: titles.iter().map(|title| fetched(title)).collect(),
        }))
    }

    fn article_titled(&self, title: &str) -> Id {
        let articles = self.articles();
        let article = articles
            .iter()
            .find(|(_, article)| article.title == title)
            .map(|(id, _)| id.clone());
        article.unwrap_or_else(|| panic!("an article titled {title}"))
    }
}

/// `count` articles of one subscription, titled in order.
fn titles(count: usize) -> Vec<String> {
    (0..count).map(|at| format!("Article {at}")).collect()
}

fn as_refs(titles: &[String]) -> Vec<&str> {
    titles.iter().map(String::as_str).collect()
}

#[test]
fn a_refresh_lands_in_the_newest_part_and_brings_nothing_twice() {
    let mut shelf = Shelf::new("ada");
    let feed = shelf.subscribe("https://example.test/feed.xml");
    assert_eq!(shelf.receive(&feed, &["First", "Second"]).unwrap(), 0);
    // The same article again, plus one that is new: only the new one lands.
    assert_eq!(shelf.receive(&feed, &["Second", "Third"]).unwrap(), 0);
    let articles = shelf.articles();
    assert_eq!(articles.len(), 3, "an article is brought in once");
    assert_eq!(shelf.states().len(), 1, "one part holds them all");
}

#[test]
fn a_refresh_for_no_subscription_is_refused() {
    let mut shelf = Shelf::new("ada");
    let refused = shelf
        .receive(&Id::from_raw("gone"), &["Nowhere"])
        .unwrap_err();
    assert!(refused.contains("no such subscription"), "{refused}");
    assert!(shelf.states().is_empty(), "nothing was written");
}

#[test]
fn a_full_part_is_left_and_the_next_one_takes_the_articles() {
    let mut shelf = Shelf::new("ada");
    let feed = shelf.subscribe("https://example.test/feed.xml");
    let held = titles(PART_ARTICLES);
    assert_eq!(shelf.receive(&feed, &as_refs(&held)).unwrap(), 0);
    assert_eq!(shelf.receive(&feed, &["One more"]).unwrap(), 1);
    let states = shelf.states();
    assert_eq!(states.len(), 2, "the head was full: a second part started");
    assert_eq!(states[0].1.articles.len(), PART_ARTICLES);
    assert_eq!(states[1].1.articles.len(), 1);
}

#[test]
fn marking_a_subscription_read_covers_every_part_before_the_mark() {
    let mut shelf = Shelf::new("ada");
    let feed = shelf.subscribe("https://example.test/feed.xml");
    let held = titles(PART_ARTICLES);
    shelf.receive(&feed, &as_refs(&held)).unwrap();
    shelf.receive(&feed, &["Later"]).unwrap();
    assert_eq!(shelf.states().len(), 2);
    assert_eq!(shelf.articles().unread_for_feed(&feed), PART_ARTICLES + 1);

    // One step, one write into the newest part.
    let written = shelf
        .apply(&ArticlePart::MARK_READ.with(Some(feed.clone())))
        .unwrap();
    assert_eq!(written, 1);
    assert_eq!(
        shelf.articles().unread_for_feed(&feed),
        0,
        "the parts before the mark are read with it"
    );

    // What a refresh brings after the mark is unread again.
    shelf.receive(&feed, &["After the mark"]).unwrap();
    let articles = shelf.articles();
    let after = shelf.article_titled("After the mark");
    assert_eq!(articles.unread_for_feed(&feed), 1);
    assert_eq!(articles.unread_ids_for(&Filter::Unread), vec![after]);
}

#[test]
fn marking_everything_read_covers_every_subscription() {
    let mut shelf = Shelf::new("ada");
    let first = shelf.subscribe("https://a.test/rss");
    let second = shelf.subscribe("https://b.test/rss");
    shelf.receive(&first, &["One", "Two"]).unwrap();
    shelf.receive(&second, &["Three"]).unwrap();
    assert_eq!(shelf.articles().total_unread(), 3);
    shelf.apply(&ArticlePart::MARK_READ.with(None)).unwrap();
    assert_eq!(shelf.articles().total_unread(), 0);
}

#[test]
fn reading_starring_and_tagging_one_article_go_to_the_part_holding_it() {
    let mut shelf = Shelf::new("ada");
    let feed = shelf.subscribe("https://example.test/feed.xml");
    let held = titles(PART_ARTICLES);
    shelf.receive(&feed, &as_refs(&held)).unwrap();
    shelf.receive(&feed, &["In the second part"]).unwrap();
    let article = shelf.article_titled("Article 7");

    assert_eq!(
        shelf
            .apply(&ArticlePart::SET_STARRED.with(article_part::SetStarred {
                article: article.clone(),
                starred: true,
            }))
            .unwrap(),
        0,
        "an article of the first part is changed there"
    );
    assert_eq!(
        shelf
            .apply(&ArticlePart::SET_READ.with(article_part::SetRead {
                article: article.clone(),
                read: true,
            }))
            .unwrap(),
        0
    );
    assert_eq!(
        shelf
            .apply(&ArticlePart::SET_TAGS.with(article_part::SetTags {
                article: article.clone(),
                tags: vec![" Design ".into(), "#design".into(), "later".into()],
            }))
            .unwrap(),
        0
    );
    let articles = shelf.articles();
    let changed = articles.article(&article).unwrap();
    assert!(changed.read && changed.starred);
    assert_eq!(changed.tag_list(), ["design", "later"]);
    assert_eq!(articles.total_starred(), 1);
    assert_eq!(articles.tags(), ["design", "later"]);
    assert_eq!(
        articles
            .articles_for(&Filter::Tag(crate::model::tag_key("later")))
            .len(),
        1
    );
}

#[test]
fn an_article_no_part_holds_is_refused() {
    let mut shelf = Shelf::new("ada");
    let feed = shelf.subscribe("https://example.test/feed.xml");
    shelf.receive(&feed, &["Only"]).unwrap();
    let refused = shelf
        .apply(&ArticlePart::SET_READ.with(article_part::SetRead {
            article: Id::from_raw("gone"),
            read: true,
        }))
        .unwrap_err();
    assert!(refused.contains("no longer exists"), "{refused}");
}

#[test]
fn forgetting_a_subscription_takes_its_articles_out_of_the_part() {
    let mut shelf = Shelf::new("ada");
    let kept = shelf.subscribe("https://a.test/rss");
    let gone = shelf.subscribe("https://b.test/rss");
    shelf.receive(&kept, &["Kept"]).unwrap();
    shelf.receive(&gone, &["Gone"]).unwrap();
    shelf
        .apply(&ArticlePart::FORGET_FEED.with(article_part::ForgetFeed { feed: gone }))
        .unwrap();
    assert_eq!(shelf.articles().len(), 1, "only the kept one's articles");
    assert_eq!(shelf.states()[0].1.articles.len(), 1);
}

#[test]
fn an_article_of_a_subscription_that_is_gone_is_not_shown() {
    // Unsubscribing removes the subscription; what it brought in stays in the
    // parts, not shown and never rewritten.
    let mut shelf = Shelf::new("ada");
    let feed = shelf.subscribe("https://example.test/feed.xml");
    shelf.receive(&feed, &["Orphan"]).unwrap();
    assert_eq!(shelf.articles().len(), 1);
    shelf
        .library
        .apply(Library::UNSUBSCRIBE.with(feed.clone()))
        .unwrap();
    shelf.state = shelf.library.state().unwrap();
    assert_eq!(shelf.articles().len(), 0, "not shown");
    assert_eq!(shelf.states()[0].1.articles.len(), 1, "not rewritten");
}

#[test]
fn one_change_costs_one_part_whatever_the_library_holds() {
    // Ten parts' worth of articles: one more change goes into the newest
    // part, and writes what a part holds, not what the library holds.
    let mut shelf = Shelf::new("ada");
    let feed = shelf.subscribe("https://example.test/feed.xml");
    for at in 0..(PART_ARTICLES * 10) {
        let title = format!("Article {at}");
        shelf.receive(&feed, &[title.as_str()]).unwrap();
    }
    assert_eq!(shelf.articles().len(), PART_ARTICLES * 10);
    assert_eq!(shelf.states().len(), 10, "ten parts hold the library");

    // A star in the first part costs that part, and the part is bounded
    // whatever the ten hold.
    let first = shelf.article_titled("Article 0");
    let written = shelf
        .apply(&ArticlePart::SET_STARRED.with(article_part::SetStarred {
            article: first,
            starred: true,
        }))
        .unwrap();
    assert_eq!(written, 0);
    let states = shelf.states();
    for (_, part) in &states {
        assert!(
            part.text_bytes() < crate::part::PART_TEXT_BYTES,
            "no part grows past its bound"
        );
    }
    // One more article starts an eleventh part, which holds one article: a
    // change there is as cheap as on a library that fits in one.
    let written = shelf.receive(&feed, &["One more"]).unwrap();
    assert_eq!(written, 10, "a new part takes it");
    assert_eq!(shelf.states().len(), 11);
    assert_eq!(shelf.states()[10].1.articles.len(), 1);
}

#[test]
fn a_field_edit_of_one_article_writes_into_the_part_holding_it() {
    let mut shelf = Shelf::new("ada");
    let feed = shelf.subscribe("https://example.test/feed.xml");
    shelf.receive(&feed, &["Only"]).unwrap();
    let article = shelf.article_titled("Only");
    let edit = ArticlePart::paths()
        .articles
        .item(&article)
        .starred
        .set(true);
    let invoke = ui::tilo_resource::Invoke::edit(&edit).unwrap();
    shelf.part(0).apply(invoke).unwrap();
    shelf.follow(0);
    let articles = shelf.articles();
    assert_eq!(
        articles.article(&article).map(|a| a.starred),
        Some(true),
        "a field edit reaches the item"
    );
}

#[test]
fn a_chain_writes_a_verb_into_the_part_holding_its_article() {
    let (library, part) = crate::sample::store();
    let library = std::rc::Rc::new(library);
    let base = {
        let library = std::rc::Rc::clone(&library);
        move || std::rc::Rc::clone(&library)
    };
    let chain = ui::framework::Chain::detached(
        "so.tilo.feeds",
        &ui::tilo_resource::MemberId::person("ada"),
        base,
    )
    .expect("a chain of parts");
    chain.write_start(0, &part).unwrap();
    let held: Vec<(u64, Rc<ArticlePart>)> = chain.parts();
    let article = held
        .iter()
        .flat_map(|(_, part)| {
            part.articles
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        })
        .next()
        .unwrap();
    chain
        .invoke(&ArticlePart::SET_STARRED.with(article_part::SetStarred {
            article: article.clone(),
            starred: true,
        }))
        .unwrap();
    let after: Vec<(u64, Rc<ArticlePart>)> = chain.parts();
    let starred = after
        .iter()
        .flat_map(|(_, part)| {
            part.articles
                .iter()
                .filter(|(id, _)| **id == article)
                .map(|(_, held)| held.starred)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(starred, [true], "the chain writes the verb into the part");
}
