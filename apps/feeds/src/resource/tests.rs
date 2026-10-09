//! The library through the door the host uses: verbs, field edits, the
//! invariants, merging, the published tree, history, and the reader as the
//! framework mounts it.

use super::*;
use crate::model::{BlockKind, ContentBlock};
use ui::framework::testing::TestApp;
use ui::tilo_actions::{ActionSpec, OperationKind, Resource as Published};
use ui::tilo_resource::items::item_resource_id;
use ui::tilo_resource::{Edit, Replica, Resource};

fn edit(library: &mut Replica<Library>, edit: &Edit<Library>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    library
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn subscribe(library: &mut Replica<Library>, url: &str, folder: Option<&Id>) -> Id {
    library
        .apply(Library::ADD_SUBSCRIPTION.with(library::AddSubscription {
            url: url.to_owned(),
            folder: folder.cloned(),
        }))
        .unwrap();
    library
        .state()
        .unwrap()
        .feed_at(url)
        .map(|feed| feed.id.clone())
        .unwrap()
}

fn folder(library: &mut Replica<Library>, name: &str) -> Id {
    library.apply(Library::NEW_FOLDER.with(name)).unwrap();
    library
        .state()
        .unwrap()
        .folders
        .iter()
        .find(|(_, folder)| folder.name == name.trim())
        .map(|(id, _)| id.clone())
        .unwrap()
}

fn fetched(title: &str) -> Fetched {
    Fetched {
        title: title.to_owned(),
        url: format!("https://example.test/{}", title.replace(' ', "-")),
        published: 1_700_000_000,
        blocks: vec![ContentBlock::new(BlockKind::Paragraph, title)],
        ..Fetched::default()
    }
}

fn refreshed(feed: &Id, titles: &[&str]) -> Invoke<Library> {
    Library::REFRESHED.with(library::Refreshed {
        articles: titles.iter().map(|title| fetched(title)).collect(),
        feed: feed.clone(),
        title: None,
        certificate_pin: None,
    })
}

fn article_titled(library: &Library, title: &str) -> Id {
    library
        .article_rows()
        .find(|article| article.title == title)
        .map_or_else(
            || panic!("an article titled {title}"),
            |article| article.id.clone(),
        )
}

fn child<'a>(tree: &'a Published, id: &str) -> &'a Published {
    tree.children
        .iter()
        .find(|child| child.id.as_str() == id)
        .unwrap_or_else(|| panic!("{id} is published"))
}

fn action<'a>(resource: &'a Published, id: &str) -> &'a ActionSpec {
    resource
        .actions
        .iter()
        .find(|spec| spec.id == id)
        .unwrap_or_else(|| panic!("{} offers {id}", resource.id.as_str()))
}

/// Subscribing, folders, what a refresh brings in, and the field edits of
/// reading, starring, tagging, renaming and moving, each one change.
#[allow(
    clippy::too_many_lines,
    reason = "one library walked through every verb and field edit in order"
)]
#[test]
fn verbs_and_field_edits_change_the_library() {
    let mut library = Replica::<Library>::new();
    let reading = folder(&mut library, " Reading ");
    let feed = subscribe(&mut library, " gemini://example.test/feed ", Some(&reading));
    let state = library.state().unwrap();
    let subscription = state.feed(&feed).unwrap();
    assert_eq!(subscription.url, "gemini://example.test/feed");
    assert_eq!(
        subscription.title, subscription.url,
        "titled by its address"
    );
    assert_eq!(subscription.site_url, "gemini://example.test");
    assert_eq!(subscription.kind, crate::model::FeedKind::Gemini);
    assert_eq!(subscription.folder.as_ref(), Some(&reading));
    assert_eq!(state.folder(&reading).unwrap().name, "Reading");

    assert!(
        library
            .apply(Library::ADD_SUBSCRIPTION.with(library::AddSubscription {
                url: "   ".into(),
                folder: None,
            }))
            .is_err(),
        "a blank address subscribes to nothing"
    );
    assert!(
        library
            .apply(Library::ADD_SUBSCRIPTION.with(library::AddSubscription {
                url: "gemini://example.test/feed".into(),
                folder: None,
            }))
            .is_err(),
        "an address is subscribed once"
    );

    library
        .apply(Library::REFRESHED.with(library::Refreshed {
            articles: vec![fetched("First"), fetched("Second")],
            feed: feed.clone(),
            title: Some("Example capsule".into()),
            certificate_pin: Some("pin-1".into()),
        }))
        .unwrap();
    library
        .apply(Library::REFRESHED.with(library::Refreshed {
            articles: vec![fetched("Second"), fetched("Third")],
            feed: feed.clone(),
            title: None,
            certificate_pin: Some("impostor".into()),
        }))
        .unwrap();
    let state = library.state().unwrap();
    assert_eq!(state.articles.len(), 3, "an article is brought in once");
    let subscription = state.feed(&feed).unwrap();
    assert_eq!(subscription.title, "Example capsule");
    assert_eq!(
        subscription.certificate_pin.as_deref(),
        Some("pin-1"),
        "a feed pins the certificate of its first fetch, and only that"
    );

    let first = article_titled(&state, "First");
    let paths = Library::paths();
    let article = paths.articles.item(&first);
    edit(&mut library, &article.read.set(true)).unwrap();
    edit(&mut library, &article.starred.set(true)).unwrap();
    edit(&mut library, &article.tags.insert(&"design".to_owned())).unwrap();
    edit(
        &mut library,
        &paths.feeds.item(&feed).title.replace("My capsule"),
    )
    .unwrap();
    edit(&mut library, &paths.feeds.item(&feed).folder.set(None)).unwrap();
    edit(
        &mut library,
        &paths.folders.item(&reading).collapsed.set(true),
    )
    .unwrap();
    let state = library.state().unwrap();
    let changed = state.article(&first).unwrap();
    assert!(changed.read && changed.starred);
    assert_eq!(changed.tag_list(), ["design"]);
    assert_eq!(state.feed(&feed).unwrap().title, "My capsule");
    assert_eq!(state.feed(&feed).unwrap().folder, None);
    assert!(state.folder(&reading).unwrap().collapsed);

    assert!(
        edit(&mut library, &article.title.replace("Edited")).is_err(),
        "what the feed sent is nobody's edit"
    );
    assert!(
        edit(
            &mut library,
            &paths
                .feeds
                .item(&feed)
                .certificate_pin
                .set(Some("mine".into()))
        )
        .is_err(),
        "only a refresh pins a certificate"
    );

    library
        .apply(Library::MARK_FEED_READ.with(feed.clone()))
        .unwrap();
    assert_eq!(library.state().unwrap().unread_for_feed(&feed), 0);
    edit(&mut library, &article.read.set(false)).unwrap();
    library
        .apply(Library::MARK_EVERYTHING_READ.invoke())
        .unwrap();
    assert_eq!(library.state().unwrap().total_unread(), 0);
}

/// Tags are normalized and folders are named: the model refuses others
/// from any path.
#[test]
fn the_invariants_refuse_unnormalized_tags_and_blank_folders() {
    let mut library = Replica::<Library>::new();
    let feed = subscribe(&mut library, "https://example.test/feed.xml", None);
    library.apply(refreshed(&feed, &["Only"])).unwrap();
    let only = article_titled(&library.state().unwrap(), "Only");
    let tags = Library::paths().articles.item(&only).tags;
    for refused in ["Design", "#design", "  ", " design"] {
        assert!(
            edit(&mut library, &tags.insert(&refused.to_owned())).is_err(),
            "{refused:?} is refused"
        );
    }
    edit(&mut library, &tags.insert(&"design".to_owned())).unwrap();
    let reading = folder(&mut library, "Reading");
    assert!(
        edit(
            &mut library,
            &Library::paths().folders.item(&reading).name.replace("  ")
        )
        .is_err()
    );
    assert!(library.apply(Library::NEW_FOLDER.with(" ")).is_err());
}

/// Unsubscribing removes the subscription with its articles; deleting a
/// folder keeps its subscriptions, at the top of the sidebar.
#[test]
fn unsubscribing_and_deleting_a_folder() {
    let mut library = Replica::<Library>::new();
    let reading = folder(&mut library, "Reading");
    let kept = subscribe(&mut library, "https://a.test/rss", Some(&reading));
    let gone = subscribe(&mut library, "https://b.test/rss", None);
    library.apply(refreshed(&kept, &["Kept"])).unwrap();
    library.apply(refreshed(&gone, &["Gone"])).unwrap();

    assert!(
        library
            .apply(Library::UNSUBSCRIBE.with(gone.clone()))
            .is_err(),
        "unsubscribing asks first"
    );
    library
        .apply_confirmed(Library::UNSUBSCRIBE.with(gone.clone()))
        .unwrap();
    let state = library.state().unwrap();
    assert!(state.feed(&gone).is_none());
    assert_eq!(state.articles.len(), 1, "its articles went with it");

    library
        .apply_confirmed(Library::DELETE_FOLDER.with(reading.clone()))
        .unwrap();
    let state = library.state().unwrap();
    assert!(state.folders.is_empty());
    assert_eq!(
        state.feed(&kept).unwrap().folder,
        None,
        "a subscription is never lost with its folder"
    );
}

/// Importing subscribes to what the file lists that the library lacks, in
/// the folders it names; importing it again adds nothing.
#[test]
fn importing_subscribes_once_in_named_folders() {
    let mut library = Replica::<Library>::new();
    subscribe(&mut library, "https://jvns.ca/atom.xml", None);
    let outlines = crate::opml::outlines(&crate::opml::parse(crate::opml::sample_opml()));
    library
        .apply(Library::IMPORT_SUBSCRIPTIONS.with(outlines.clone()))
        .unwrap();
    let state = library.state().unwrap();
    assert_eq!(state.feeds.len(), 3, "two new, one already subscribed");
    let reading = state
        .folders
        .iter()
        .find(|(_, folder)| folder.name == "Reading")
        .map(|(id, _)| id.clone())
        .expect("the file's folder");
    assert_eq!(state.feeds_in_folder(Some(&reading)).len(), 2);
    assert_eq!(
        state
            .feed_at("https://lwn.net/headlines/rss")
            .unwrap()
            .title,
        "LWN"
    );
    library
        .apply(Library::IMPORT_SUBSCRIPTIONS.with(outlines))
        .unwrap();
    assert_eq!(library.state().unwrap().feeds.len(), 3);
    assert_eq!(library.state().unwrap().folders.len(), 1);
}

/// Two people reading one library at once converge: read and star flags
/// per article, tags per tag, articles per article.
#[test]
fn concurrent_edits_converge() {
    let mut ours = Replica::<Library>::new();
    let feed = subscribe(&mut ours, "https://example.test/feed.xml", None);
    ours.apply(refreshed(&feed, &["Shared"])).unwrap();
    let shared = article_titled(&ours.state().unwrap(), "Shared");
    let mut theirs = ours.fork();
    let article = Library::paths().articles.item(&shared);

    edit(&mut ours, &article.tags.insert(&"design".to_owned())).unwrap();
    edit(&mut ours, &article.read.set(true)).unwrap();
    edit(&mut theirs, &article.tags.insert(&"later".to_owned())).unwrap();
    edit(&mut theirs, &article.starred.set(true)).unwrap();
    theirs.apply(refreshed(&feed, &["Theirs"])).unwrap();

    ours.sync_with(&mut theirs).unwrap();
    let merged = ours.state().unwrap();
    assert_eq!(merged, theirs.state().unwrap());
    let article = merged.article(&shared).unwrap();
    assert_eq!(article.tag_list(), ["design", "later"]);
    assert!(article.read && article.starred);
    assert_eq!(merged.articles.len(), 2);
}

/// Each folder, subscription and article is published as its own item with
/// the verbs that act on it; each tag is a place that reveals its articles.
#[test]
fn the_published_library_offers_every_item_its_actions() {
    let mut library = Replica::<Library>::new();
    let reading = folder(&mut library, "Reading");
    let feed = subscribe(
        &mut library,
        "https://example.test/feed.xml",
        Some(&reading),
    );
    library.apply(refreshed(&feed, &["Tagged"])).unwrap();
    let tagged = article_titled(&library.state().unwrap(), "Tagged");
    edit(
        &mut library,
        &Library::paths()
            .articles
            .item(&tagged)
            .tags
            .insert(&"design".to_owned()),
    )
    .unwrap();
    let tree = library.published().unwrap();
    for id in [
        "add-subscription",
        "new-folder",
        "mark-everything-read",
        "import-subscriptions",
        "field-edit",
    ] {
        assert_eq!(action(&tree, id).kind, OperationKind::Document, "{id}");
    }

    let folder_item = child(&tree, &item_resource_id::<Library>("folders", &reading));
    assert_eq!(folder_item.label, "Reading");
    assert_eq!(
        action(folder_item, "delete-folder").contract.risk,
        ui::tilo_actions::RiskLevel::Destructive
    );
    let feed_item = child(&tree, &item_resource_id::<Library>("feeds", &feed));
    assert_eq!(
        action(feed_item, "unsubscribe").contract.risk,
        ui::tilo_actions::RiskLevel::Destructive
    );
    action(feed_item, "mark-feed-read");

    let tag = child(&tree, "feeds.library/derived/tag:design");
    assert_eq!(tag.kind, "feeds.library.tag");
    assert_eq!(action(tag, "reveal-tag").kind, OperationKind::View);

    // An item beyond the published page resolves by its id; a stale one
    // does not.
    let state = library.state().unwrap();
    assert!(Library::child(&state, &item_resource_id::<Library>("feeds", &feed)).is_some());
    assert!(
        Library::child(
            &state,
            &item_resource_id::<Library>("feeds", &Id::from_raw("gone"))
        )
        .is_none()
    );
}

/// Every subscription and article is found by what it holds, anchored at
/// its item.
#[test]
fn search_finds_subscriptions_and_articles() {
    let mut library = Replica::<Library>::new();
    let feed = subscribe(&mut library, "https://example.test/feed.xml", None);
    library
        .apply(Library::REFRESHED.with(library::Refreshed {
            articles: vec![Fetched {
                author: "Ada Lovelace".into(),
                ..fetched("Engines")
            }],
            feed: feed.clone(),
            title: Some("Analytical".into()),
            certificate_pin: None,
        }))
        .unwrap();
    let state = library.state().unwrap();
    let entries = Library::search(&state);
    let engines = article_titled(&state, "Engines");
    let hit = |title: &str| {
        entries
            .iter()
            .find(|entry| entry.title == title)
            .unwrap_or_else(|| panic!("{title} is found"))
    };
    assert_eq!(
        hit("Engines").anchor.as_deref(),
        Some(item_resource_id::<Library>("articles", &engines).as_str())
    );
    assert!(hit("Engines").text.contains("Ada Lovelace"));
    assert_eq!(
        hit("Analytical").anchor.as_deref(),
        Some(item_resource_id::<Library>("feeds", &feed).as_str())
    );
}

// ---- the reader, as the framework mounts it ------------------------------

/// The reader on the sample library, laid out as the screenshots are.
fn sample_app() -> TestApp<FeedsShell> {
    let mut app = TestApp::<FeedsShell>::new().unwrap();
    for invoke in FeedsShell::scenario(None) {
        app.invoke(invoke).unwrap();
    }
    app.settle();
    app
}

/// Unsubscribing from a feed's menu is confirmed by the host, once: the
/// window asks nothing of its own, and the feed and its articles go only
/// when the person agreed.
#[test]
fn unsubscribing_from_the_menu_is_confirmed_by_the_host() {
    let mut app = sample_app();
    let feed = app.state().feeds.ids().next().unwrap().clone();
    // What the window's own Edits are lent for: an unsubscribe applies
    // nothing now, it goes to the host to be confirmed.
    let mut unused = automerge::AutoCommit::new();
    app.surface_mut::<FeedsApp>().core_mut().apply_menu_action(
        crate::app::MenuAction::Unsubscribe(feed.clone()),
        &mut ui::widget::Edits::lend(&mut unused),
    );
    assert!(app.surface_mut::<FeedsApp>().modal.is_none());
    assert!(app.state().feeds.contains(&feed), "not until confirmed");
    assert!(app.confirm_asked().is_some(), "the host asks first");
    let library = app.state();
    assert!(!library.feeds.contains(&feed));
    assert!(!library.articles.values().any(|x| x.feed_id == feed));
    assert!(app.confirm_asked().is_none(), "asked once");
}

/// Opening the app writes nothing: there is no library to show.
#[test]
fn opening_writes_nothing() {
    let mut app = TestApp::<FeedsShell>::new().unwrap();
    app.settle();
    assert!(app.state().is_empty());
    assert!(!app.undo(), "there is nothing to undo");
}

/// How articles read is the person's setting: the palette's effects and
/// another window's write reach the reader, change no document and are
/// nobody's undo step.
#[test]
fn reading_preferences_are_personal_settings() {
    let mut app = sample_app();
    let before = app.peer_document().get_heads();
    app.effect(&FeedsEffect::ReaderFont {
        font: "serif".into(),
    })
    .unwrap();
    app.effect(&FeedsEffect::ReaderPaper {
        paper: "sepia".into(),
    })
    .unwrap();
    app.effect(&FeedsEffect::ReaderTextLarger).unwrap();
    app.settle();
    let stored = app.settings::<ReadSettings>().get();
    assert_eq!(stored.font, ReadFont::Serif);
    assert_eq!(stored.theme, ReadTheme::Sepia);
    assert!(stored.font_scale > ReadSettings::default().font_scale);
    assert!(
        app.effect(&FeedsEffect::ReaderFont {
            font: "comic".into()
        })
        .is_err(),
        "an unknown font is refused"
    );
    assert_eq!(
        app.peer_document().get_heads(),
        before,
        "no document change"
    );

    // A preference written by another window or device shows here.
    app.settings::<ReadSettings>()
        .set(&ReadSettings {
            font: ReadFont::Mono,
            ..stored
        })
        .unwrap();
    app.settle();
    let shown = app
        .with_surface(|feeds: &mut FeedsApp| feeds.read_settings_for_test())
        .unwrap();
    assert_eq!(shown.font, ReadFont::Mono);
}

/// The reader's keys, the palette and an agent all edit the library
/// through the same door, one undo step each.
#[test]
fn the_reader_edits_through_the_framework_door() {
    let mut app = sample_app();
    let first = {
        let open = app.surface_mut::<FeedsApp>().selected_article().unwrap();
        let starred = app.state().article(&open).unwrap().starred;
        (open, starred)
    };
    // `s` stars the open article.
    app.harness().key("s");
    app.settle();
    let starred = |app: &TestApp<FeedsShell>| app.state().article(&first.0).unwrap().starred;
    assert_eq!(starred(&app), !first.1);
    assert!(app.undo(), "starring is one undo step");
    assert_eq!(starred(&app), first.1);

    // `a` marks everything read.
    app.harness().key("a");
    app.settle();
    assert_eq!(app.state().total_unread(), 0);

    // The palette's prompt subscribes; a blank one adds nothing.
    let before = app.state().feeds.len();
    app.call(
        "add-subscription",
        serde_json::json!({ "url": "https://example.test/updates.xml" }),
    )
    .unwrap();
    assert!(
        app.state()
            .feed_at("https://example.test/updates.xml")
            .is_some()
    );
    assert!(
        app.call("add-subscription", serde_json::json!({ "url": "   " }))
            .is_err()
    );
    assert_eq!(app.state().feeds.len(), before + 1);
    app.call(
        "add-subscription",
        serde_json::json!({ "url": "https://example.test/later.xml" }),
    )
    .unwrap();
    assert_eq!(
        app.state()
            .feeds
            .values()
            .last()
            .map(|feed| feed.url.as_str()),
        Some("https://example.test/later.xml"),
        "a palette subscription goes after the last one"
    );
    let folders = app.state().folders.len();
    app.call("new-folder", serde_json::json!({ "name": "Later" }))
        .unwrap();
    let state = app.state();
    assert_eq!(state.folders.len(), folders + 1);
    assert_eq!(
        state
            .folders
            .values()
            .last()
            .map(|folder| folder.name.as_str()),
        Some("Later"),
        "a palette folder goes after the last one"
    );
}

/// An agent reaches every subscription, article and tag through the one
/// published tree, each change one call, and opening an item shows it.
#[test]
fn an_agent_edits_and_opens_items_one_call_each() {
    let mut app = sample_app();
    let state = app.state();
    let article = state
        .articles_for(&Filter::All)
        .into_iter()
        .find(|article| !article.starred)
        .map(|article| article.id.clone())
        .unwrap();
    let feed = state.feeds.ids().next().unwrap().clone();
    let paths = Library::paths().articles.item(&article);
    for edit in [
        paths.starred.set(true),
        paths.tags.insert(&"later".to_owned()),
    ] {
        app.call("field-edit", serde_json::json!({ "edit": edit }))
            .unwrap();
    }
    let shown = app.state();
    let changed = shown.article(&article).unwrap();
    assert!(changed.starred);
    assert!(changed.tags.contains(&"later".to_owned()));

    let feed_item = item_resource_id::<Library>("feeds", &feed);
    app.call_on(&feed_item, "mark-feed-read", serde_json::json!({}))
        .unwrap();
    assert_eq!(app.state().unread_for_feed(&feed), 0);

    let article_item = item_resource_id::<Library>("articles", &article);
    app.call("open", serde_json::json!({ "anchor": article_item }))
        .unwrap();
    assert!(
        app.call(
            "open",
            serde_json::json!({ "anchor": item_resource_id::<Library>("articles", &Id::from_raw("gone")) })
        )
        .is_err(),
        "an article that does not exist is not opened"
    );
    app.settle();
    assert_eq!(
        app.surface_mut::<FeedsApp>().selected_article(),
        Some(article.clone()),
        "opening shows the article"
    );
    app.call_on(
        "feeds.library/derived/tag:later",
        "reveal-tag",
        serde_json::json!({}),
    )
    .unwrap();
    app.settle();
    assert_eq!(
        app.surface_mut::<FeedsApp>().active_filter(),
        tag_filter("later"),
        "a tag reveals its articles"
    );

    app.call_on(&feed_item, "unsubscribe", serde_json::json!({}))
        .expect_err("unsubscribing asks for confirmation");
    app.call_confirmed_on(&feed_item, "unsubscribe", serde_json::json!({}))
        .unwrap();
    assert!(app.state().feed(&feed).is_none());
}

/// Unsubscribing the last feed — here, through the palette, or by a
/// collaborator — leaves the reader showing no rows of it.
#[test]
fn unsubscribing_the_last_feed_empties_the_reader() {
    let mut app = TestApp::<FeedsShell>::new().unwrap();
    app.call(
        "add-subscription",
        serde_json::json!({ "url": "https://example.test/only.xml" }),
    )
    .unwrap();
    let feed = app.state().feeds.ids().next().unwrap().clone();
    app.invoke(
        Invoke::edit(
            &Library::paths()
                .feeds
                .item(&feed)
                .title
                .replace("The Only Feed"),
        )
        .unwrap(),
    )
    .unwrap();
    app.invoke(refreshed(&feed, &["The Only Article"])).unwrap();
    app.settle();
    let shown = app.harness().inspection_json().unwrap();
    assert!(shown.contains("The Only Feed"), "{shown}");
    assert!(shown.contains("The Only Article"), "{shown}");

    app.invoke_confirmed(Library::UNSUBSCRIBE.with(feed))
        .unwrap();
    app.settle();
    assert!(app.state().is_empty());
    let shown = app.harness().inspection_json().unwrap();
    assert!(!shown.contains("The Only Feed"), "{shown}");
    assert!(!shown.contains("The Only Article"), "{shown}");
}

/// Refreshing and OPML import/export are the reader's effects: every
/// caller sees them, and refreshing says why it cannot run without
/// subscriptions.
#[test]
fn refreshing_and_opml_are_effects_of_the_reader() {
    let mut app = TestApp::<FeedsShell>::new().unwrap();
    let surface = app.agent_surface();
    let action = |id: &str| {
        surface
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["resource"] == <Library as Resource>::KIND && row["action"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} is published: {surface:#}"))
    };
    for id in ["refresh-all", "import-opml", "export-opml"] {
        assert_eq!(action(id)["operation"], "Effect", "{id}");
    }
    assert!(
        app.effect(&FeedsEffect::RefreshAll).is_err(),
        "there is nothing to refresh"
    );
}

/// What a refresh fetched reaches every collaborator but is nobody's
/// undo step, while an OPML import is one step.
#[test]
fn fetched_rows_are_no_undo_step_and_an_import_is_one() {
    let mut app = TestApp::<FeedsShell>::new().unwrap();
    app.call(
        "add-subscription",
        serde_json::json!({ "url": "https://example.test/feed.xml" }),
    )
    .unwrap();
    let feed = app.state().feeds.ids().next().unwrap().clone();
    app.invoke(Invoke::edit(&Library::paths().feeds.item(&feed).title.replace("Mine")).unwrap())
        .unwrap();
    app.invoke(refreshed(&feed, &["Fetched"])).unwrap();
    assert_eq!(app.state().articles.len(), 1);

    // Undo takes back the rename, past the fetched article, which stays.
    assert!(app.undo());
    let state = app.state();
    assert_ne!(state.feed(&feed).unwrap().title, "Mine");
    assert_eq!(state.articles.len(), 1);

    // The subscriptions an import added go in one step.
    app.invoke(Library::IMPORT_SUBSCRIPTIONS.with(vec![
        Outline {
            title: "A".into(),
            url: "https://a.test/rss".into(),
            folder: Some("Reading".into()),
            ..Outline::default()
        },
        Outline {
            title: "B".into(),
            url: "https://b.test/rss".into(),
            folder: Some("Reading".into()),
            ..Outline::default()
        },
    ]))
    .unwrap();
    assert_eq!(app.state().feeds.len(), 3);
    assert_eq!(app.state().folders.len(), 1);
    assert!(app.undo());
    assert_eq!(app.state().feeds.len(), 1);
    assert!(app.state().folders.is_empty());
}

/// What an agent does in the library reads as the reader's sentences, in
/// the person's language.
#[allow(
    clippy::too_many_lines,
    reason = "one entry per field and verb, read back in order"
)]
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = TestApp::<FeedsShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = <Library as Resource>::KIND;
    app.call_by(
        agent.clone(),
        kind,
        "new-folder",
        serde_json::json!({"name": "Reading"}),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        kind,
        "add-subscription",
        serde_json::json!({"url": "https://example.test/feed.xml"}),
    )
    .unwrap();
    let feed = app.state().feeds.ids().next().unwrap().clone();
    let reading = app.state().folders.ids().next().unwrap().clone();
    app.invoke(refreshed(&feed, &["Engines"])).unwrap();
    let engines = article_titled(&app.state(), "Engines");
    let article = Library::paths().articles.item(&engines);
    let subscription = Library::paths().feeds.item(&feed);
    for edit in [
        article.read.set(true),
        article.read.set(false),
        article.starred.set(true),
        article.starred.set(false),
        article.tags.insert(&"design".to_owned()),
        subscription.title.replace("Analytical"),
        subscription.folder.set(Some(reading.clone())),
        Library::paths().folders.item(&reading).collapsed.set(true),
        Library::paths()
            .folders
            .item(&reading)
            .name
            .replace("Later"),
    ] {
        app.call_by(
            agent.clone(),
            kind,
            "field-edit",
            serde_json::json!({ "edit": edit }),
        )
        .unwrap();
    }
    app.call_by(
        agent.clone(),
        kind,
        "mark-everything-read",
        serde_json::json!({}),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        kind,
        "field-edit",
        serde_json::json!({ "edit": article.read.set(false) }),
    )
    .unwrap();
    let feed_item = item_resource_id::<Library>("feeds", &feed);
    app.call_by(
        agent.clone(),
        &feed_item,
        "mark-feed-read",
        serde_json::json!({}),
    )
    .unwrap();
    app.call_by(
        agent,
        kind,
        "import-subscriptions",
        serde_json::json!({"outlines": [{"title": "LWN", "url": "https://lwn.net/headlines/rss"}]}),
    )
    .unwrap();

    let sentences = |locale: &str| -> Vec<String> {
        let source = LOCALES
            .iter()
            .find(|(candidate, _)| *candidate == locale)
            .map(|(_, source)| *source)
            .unwrap();
        let catalogue = tilo_history::Catalogue::new(locale, &[source]);
        let reader = tilo_history::Reader::new(locale).with_app(&catalogue);
        app.versions()
            .into_iter()
            .filter_map(|version| version.context)
            .filter(|entry| {
                entry
                    .invoker
                    .as_ref()
                    .is_some_and(|invoker| invoker.actor == "agent:claude")
            })
            .map(|entry| reader.sentence(&entry))
            .collect()
    };
    assert_eq!(
        sentences("en-US"),
        [
            "Created the folder Reading",
            "Subscribed to https://example.test/feed.xml",
            "Marked Engines as read",
            "Marked Engines as unread",
            "Starred Engines",
            "Unstarred Engines",
            "Changed the tags of Engines",
            "Renamed the subscription https://example.test/feed.xml",
            "Moved Analytical to another folder",
            "Folded the folder Reading",
            "Renamed the folder Reading",
            "Marked all articles as read",
            "Marked Engines as unread",
            "Marked everything in Analytical as read",
            "Subscribed to what an OPML file lists",
        ]
    );
    assert_eq!(sentences("de-DE")[0], "Ordner Reading angelegt");
}

/// Every screenshot is the framework's capture: the library comes from
/// the scenario's actions, the open popovers from the reader's staging,
/// and the narrow scenarios get a phone-sized window.
#[test]
fn captures_stage_through_actions_and_the_reader() {
    let mut detail = TestApp::<FeedsShell>::capture(Some("detail")).unwrap();
    assert_eq!(detail.harness().inspection().viewport, [560, 800]);
    assert!(!detail.state().feeds.is_empty());
    assert!(
        detail
            .harness()
            .inspection_json()
            .unwrap()
            .contains("nav-shell-back")
    );

    let settings = TestApp::<FeedsShell>::capture(Some("reader-settings")).unwrap();
    let staged = settings.settings::<ReadSettings>().get();
    assert_eq!(staged.font, ReadFont::Serif);
    assert_eq!(staged.theme, ReadTheme::Sepia);

    let long = TestApp::<FeedsShell>::capture(Some("long-titles")).unwrap();
    assert!(
        long.state()
            .feeds
            .values()
            .any(|feed| feed.title.starts_with("International Space Station"))
    );

    let imported = TestApp::<FeedsShell>::capture(Some("post-import")).unwrap();
    assert!(
        imported
            .state()
            .feed_at("https://jvns.ca/atom.xml")
            .is_some()
    );

    let mut tags = TestApp::<FeedsShell>::capture(Some("tags")).unwrap();
    assert_eq!(tags.harness().inspection().viewport, [1280, 800]);
    let nodes = tags.harness().a11y_nodes();
    assert!(
        nodes.iter().any(|(_, node)| {
            node.role() == ui::accesskit::Role::Dialog && node.label() == Some("Edit tags")
        }),
        "the capture stages the actual article-tag dialog"
    );
    assert!(
        nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Save tags")),
        "the staged dialog retains its real submission control"
    );

    let empty = TestApp::<FeedsShell>::capture(Some("empty")).unwrap();
    assert!(empty.state().is_empty());
}

/// Every host that mounts the reader loads its messages.
#[test]
fn the_catalog_resolves_the_link_messages() {
    FeedsShell::catalog().unwrap().ensure();
    for (key, value) in [
        (
            "feeds-open-link-invalid",
            ui::i18n!("feeds-open-link-invalid"),
        ),
        (
            "feeds-open-link-started",
            ui::i18n!("feeds-open-link-started"),
        ),
        (
            "feeds-open-link-failed",
            ui::i18n!("feeds-open-link-failed"),
        ),
    ] {
        assert_ne!(key, value);
        assert!(!value.is_empty());
    }
}
